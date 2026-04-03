use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use rust_decimal::Decimal;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

use gbot::config::coins::CoinMetaStore;
use gbot::config::settings::{BotMode, Settings};
use gbot::exchange::rate_limiter::RateLimiter;
use gbot::exchange::rest_client::RestClient;
use gbot::exchange::signer::HyperliquidSigner;
use gbot::exchange::ws_client::{WsClient, WsEvent};
use gbot::execution::order_manager::{FillEvent, OrderManager};
use gbot::execution::position_manager::{OpenPosition, PositionManager};
use gbot::features::engine::FeatureEngine;
use gbot::market_data::book_manager::BookManager;
use gbot::market_data::recorder::Recorder;
use gbot::observability::dashboard::{
    self, BookView, BotStatusView, ClosedTradeView, DashboardSnapshot, DashboardState, EventFeed,
    MetricsView, PendingOrderView, PositionView,
};
use gbot::observability::metrics::Metrics;
use gbot::persistence::journal::{Journal, JournalEvent};
use gbot::persistence::signal_recorder::{SignalRecord, SignalRecorder};
use gbot::portfolio::state::PortfolioState;
use gbot::regime::engine as regime_engine;
use gbot::risk::manager::RiskManager;
use gbot::strategy::mfdp::MfdpStrategy;
use gbot::strategy::pullback::{PullbackSettings, PullbackTracker, UpdateResult};
use gbot::strategy::signal::{Direction, Intent};

#[tokio::main]
async fn main() -> Result<()> {
    // ── Load config ──
    let settings = Settings::load()?;

    // ── Init tracing (stdout JSON + daily rotating log file) ──
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&settings.general.log_level));

    let log_dir = format!("{}/logs", settings.general.data_dir);
    std::fs::create_dir_all(&log_dir).ok();
    let file_appender = tracing_appender::rolling::daily(&log_dir, "gbot.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(std::io::stdout),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(non_blocking),
        )
        .init();

    info!("╔══════════════════════════════════════╗");
    info!("║         gbot — MFDP V1               ║");
    info!("║  Microstructure First Directional     ║");
    info!("║        Pullback Bot                   ║");
    info!("╚══════════════════════════════════════╝");
    info!("Mode: {:?}", settings.general.mode);
    info!("Active coins: {:?}", settings.coins.active);

    let coins = settings.coins.active.clone();

    // ── Exchange setup ──
    let rate_limiter = RateLimiter::new(settings.exchange.rate_limit.clone());

    let signer = Arc::new(
        HyperliquidSigner::new(
            &settings.exchange.agent_private_key,
            settings.exchange.wallet_address.clone(),
        )
        .unwrap_or_else(|e| {
            warn!(
                "[MAIN] Signer init failed (no private key?): {}. Running in observation mode.",
                e
            );
            // All-zero key is invalid for secp256k1; use scalar=1 as dummy
            HyperliquidSigner::new(&format!("{:0>63}1", ""), String::new()).expect("dummy signer")
        }),
    );

    let rest = Arc::new(RestClient::new(
        settings.exchange.clone(),
        signer.clone(),
        rate_limiter.clone(),
    )?);

    // ── Load coin metadata (standard perps + xyz dex) ──
    let meta_store = match rest.fetch_meta().await {
        Ok(meta) => {
            let universe = meta
                .get("universe")
                .and_then(|u| u.as_array())
                .map(|arr| arr.to_vec())
                .unwrap_or_default();
            let mut store = CoinMetaStore::from_exchange_meta(&universe);
            info!("[MAIN] Loaded {} standard perps", store.all().len());

            // Load xyz dex assets (HIP-3: stocks, forex, commodities)
            match rest.fetch_xyz_meta().await {
                Ok(xyz_meta) => {
                    let xyz_universe = xyz_meta
                        .get("universe")
                        .and_then(|u| u.as_array())
                        .map(|arr| arr.to_vec())
                        .unwrap_or_default();
                    let before = store.all().len();
                    store.add_xyz_meta(&xyz_universe);
                    let xyz_count = store.all().len() - before;
                    info!("[MAIN] Loaded {} xyz dex assets (total: {})", xyz_count, store.all().len());
                }
                Err(e) => {
                    warn!("[MAIN] Failed to load xyz metadata: {} — xyz coins unavailable", e);
                }
            }
            store
        }
        Err(e) => {
            warn!("[MAIN] Failed to load metadata: {} — using empty store", e);
            CoinMetaStore::new()
        }
    };

    // ── Get initial equity ──
    let initial_equity = if settings.general.mode == BotMode::Live {
        match rest.get_equity().await {
            Ok(eq) => {
                info!("[MAIN] Initial equity: ${}", eq);
                eq
            }
            Err(e) => {
                warn!("[MAIN] Failed to get equity: {} — using $0", e);
                Decimal::ZERO
            }
        }
    } else {
        let sim = Decimal::try_from(settings.general.simulated_equity).unwrap_or(Decimal::new(10_000, 0));
        info!("[MAIN] Dry-run simulated equity: ${}", sim);
        sim
    };

    // ── Init components ──
    let book_mgr = BookManager::new(&coins, settings.features.trade_tape_size);
    let feature_engine = FeatureEngine::new(&coins);
    let recorder = Recorder::new(
        &settings.general.data_dir,
        &coins,
        settings.recording.enabled,
    );
    let journal = Journal::new(&settings.general.data_dir)?;
    let signal_recorder = SignalRecorder::new(&settings.general.data_dir)?;
    let metrics = Arc::new(Metrics::new());
    let strategy = MfdpStrategy::new(settings.strategy.clone());
    let mut risk_mgr = RiskManager::new_with_persistence(
        settings.risk.clone(),
        initial_equity,
        &settings.general.data_dir,
    );
    let mut order_mgr = OrderManager::new(settings.general.mode.clone(), settings.execution.clone());
    let mut position_mgr = PositionManager::new(settings.execution.clone());
    let mut portfolio = PortfolioState::new(initial_equity);

    // ── Recover positions at startup (if live) ──
    if settings.general.mode == BotMode::Live {
        info!("[MAIN] Recovering positions from exchange...");
        if let Err(e) = position_mgr.recover_positions(&rest).await {
            error!("[MAIN] Position recovery failed: {}", e);
        }
        if let Err(e) = position_mgr.cleanup_orphan_triggers(&rest).await {
            warn!("[MAIN] Orphan trigger cleanup failed: {}", e);
        }
    }

    // ── WebSocket event channel ──
    let (event_tx, mut event_rx) = mpsc::channel::<WsEvent>(50_000);

    // ── Start WebSocket client ──
    let ws_client = Arc::new(WsClient::new(
        settings.exchange.clone(),
        coins.clone(),
        event_tx,
    ));
    let _ws_handle = {
        let ws = ws_client.clone();
        tokio::spawn(async move { ws.run().await })
    };

    // ── Start dashboard ──
    let dashboard_snapshot = Arc::new(RwLock::new(DashboardSnapshot::default()));
    let dashboard_state = Arc::new(DashboardState {
        metrics: metrics.clone(),
        snapshot: dashboard_snapshot.clone(),
    });
    tokio::spawn(async move {
        dashboard::start_dashboard(dashboard_state, 3000).await;
    });

    // ── Periodic recorder flush ──
    let recorder_flush = recorder.clone();
    let flush_interval = settings.recording.flush_interval_s;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(flush_interval));
        loop {
            interval.tick().await;
            if let Err(e) = recorder_flush.flush().await {
                error!("[RECORDER] Flush error: {}", e);
            }
        }
    });

    // ── Main event loop ──
    info!("[MAIN] Entering main event loop...");

    // Equity is fetched every 30s and fed to RiskManager
    let mut current_equity = initial_equity;
    let mut last_equity_fetch_ms: i64 = 0;
    let equity_fetch_interval_ms: i64 = 30_000;

    let mut reconnect_recent = false;
    let mut reconnect_clear_at: i64 = 0;
    let cooldown_s = settings.risk.cooldown_after_close_s;

    // ── Per-coin signal cooldown (prevents re-emitting same signal every tick) ──
    let mut signal_cooldowns: HashMap<String, i64> = HashMap::new();
    let signal_cooldown_ms: i64 = 60_000; // 60s cooldown per coin after signal

    // ── Direction confirmation tracking ──
    // Require min_direction_confirmations consecutive evaluations with score above threshold
    // before emitting a signal. (last_sign: +1=Long, -1=Short, 0=neutral; consecutive_count)
    let mut direction_confirms: HashMap<String, (i8, u32)> = HashMap::new();
    let min_confirmations = settings.strategy.min_direction_confirmations;
    let min_trades_for_signal = settings.strategy.min_trades_for_signal;

    // ── Per-coin signal quota (rolling 10min window) ──
    // Prevents high-frequency coins (ETH, BTC) from monopolizing all signals.
    // Each entry is a VecDeque of signal timestamps (ms) within the last 10 minutes.
    let mut coin_signal_timestamps: HashMap<String, std::collections::VecDeque<i64>> = HashMap::new();
    let max_signals_per_coin_10min = settings.risk.max_signals_per_coin_10min;
    let signal_quota_window_ms: i64 = 600_000; // 10 minutes

    // ── Phase 7.5: Pullback tracker ──────────────────────────────────────────
    // After direction confirmation, instead of placing the order immediately,
    // we arm the pullback tracker. It waits for a micro-move then a retrace,
    // and only then emits the entry signal.
    let pullback_settings = PullbackSettings {
        min_move_bps: settings.strategy.pullback_min_move_bps,
        retrace_pct: settings.strategy.pullback_retrace_pct,
        wait_move_ms: settings.strategy.pullback_wait_move_s as i64 * 1000,
        wait_retrace_ms: settings.strategy.pullback_wait_retrace_s as i64 * 1000,
        ofi_confirm_threshold: settings.strategy.pullback_ofi_confirm,
    };
    let mut pullback_tracker = PullbackTracker::new();

    // ── Periodic summary tracking ──
    let summary_interval_ms: i64 = 300_000; // every 5 minutes
    let mut last_summary_ms: i64 = 0;
    let mut signals_since_summary: u32 = 0;
    let mut orders_since_summary: u32 = 0;
    let mut rejections_since_summary: u32 = 0;
    let mut fills_since_summary: u32 = 0;

    // ── Dry-run fill simulation tracking ──
    // In dry-run: when mid crosses the entry price, simulate a fill
    // We track pending dry-run orders to check mid crossing
    let is_dry_run = settings.general.mode == BotMode::DryRun;

    // ── Dashboard state ──
    let mut regimes: HashMap<String, regime_engine::Regime> = HashMap::new();
    let mut event_feed = EventFeed::new(30);
    let mut dashboard_tick = tokio::time::interval(Duration::from_millis(500));

    // ── Graceful shutdown signal ──
    let shutdown_signal = tokio::signal::ctrl_c();
    tokio::pin!(shutdown_signal);

    // ── Trade history + bot status tracking ──
    let started_at_ms = chrono::Utc::now().timestamp_millis();
    let mut closed_trades: Vec<ClosedTradeView> = Vec::new();
    let mut error_count: u64 = 0;
    let mut warn_count: u64 = 0;
    let mut last_error = String::new();
    let mut last_error_ts: i64 = 0;

    loop {
        tokio::select! {
            Some(event) = event_rx.recv() => {
                let now_ms = chrono::Utc::now().timestamp_millis();

                // Clear reconnect flag after 10s stabilisation
                if reconnect_recent && now_ms > reconnect_clear_at {
                    reconnect_recent = false;
                    info!("[MAIN] Reconnect stabilisation complete — resuming trading");
                }

                // ── Periodic equity refresh (fixes gap #4) ──
                if settings.general.mode == BotMode::Live
                    && now_ms - last_equity_fetch_ms > equity_fetch_interval_ms
                {
                    match rest.get_equity().await {
                        Ok(eq) => {
                            current_equity = eq;
                            risk_mgr.update_equity(current_equity);
                            risk_mgr.check_daily_reset(current_equity);
                            metrics.equity.set(
                                current_equity.to_string().parse::<f64>().unwrap_or(0.0),
                            );
                        }
                        Err(e) => {
                            warn!("[MAIN] Equity fetch failed: {}", e);
                            warn_count += 1;
                        }
                    }
                    last_equity_fetch_ms = now_ms;
                }

                // Update book manager first (all events)
                book_mgr.handle_event(&event);

                match &event {
                    WsEvent::BookUpdate { coin, .. } => {
                        // Recompute features for this coin
                        feature_engine.update(coin, &book_mgr, now_ms);

                        // Record L2 snapshot
                        if let Some(book) = book_mgr.books.get(coin) {
                            recorder.record_book(&book).await;
                        }

                        // Skip strategy in pure observation mode
                        if settings.general.mode == BotMode::Observation {
                            continue;
                        }

                        if let Some(features) = feature_engine.get(coin) {
                            let is_stale = book_mgr.is_stale(coin);
                            let regime = regime_engine::classify(
                                &features,
                                is_stale,
                                reconnect_recent,
                                &settings.regime,
                                None, // funding seconds — not yet fetched; None = no restriction
                            );

                            // Track regime for dashboard + push event on change
                            let prev_regime = regimes.get(coin).copied();
                            if prev_regime != Some(regime) {
                                if prev_regime.is_some() {
                                    event_feed.push("regime", format!("{} regime → {:?}", coin, regime));
                                }
                                regimes.insert(coin.clone(), regime);
                            }

                            let current_mid = book_mgr.mids.get(coin).map(|m| *m).unwrap_or(0.0);

                            // ── Position lifecycle for this coin ──
                            if position_mgr.get(coin).is_some() {
                                // Break-even check
                                if let Some(new_sl) = position_mgr.check_break_even(coin, current_mid) {
                                    let asset_idx = meta_store.get(coin).map(|m| m.asset_index).unwrap_or(0);
                                    if let Err(e) = position_mgr
                                        .update_sl_trigger(coin, new_sl, &rest, &settings.general.mode, asset_idx)
                                        .await
                                    {
                                        warn!("[MAIN] BE SL update failed for {}: {}", coin, e);
                                    }
                                }

                                // Trailing stop check
                                if let Some(new_sl) = position_mgr.check_trailing(coin, current_mid) {
                                    let asset_idx = meta_store.get(coin).map(|m| m.asset_index).unwrap_or(0);
                                    if let Err(e) = position_mgr
                                        .update_sl_trigger(coin, new_sl, &rest, &settings.general.mode, asset_idx)
                                        .await
                                    {
                                        warn!("[MAIN] Trailing SL update failed for {}: {}", coin, e);
                                    }
                                }

                                // Max hold timeout
                                let elapsed_s = position_mgr
                                    .get(coin)
                                    .map(|p| (now_ms - p.opened_at) / 1000)
                                    .unwrap_or(0);
                                if elapsed_s > settings.execution.max_hold_s as i64
                                    && !matches!(order_mgr.state(coin), gbot::execution::order_manager::TradeState::ForceExit { .. })
                                {
                                    if let Some(pos) = position_mgr.get(coin) {
                                        let intent = Intent::ForceExitIoc {
                                            coin: coin.clone(),
                                            direction: pos.direction,
                                            mid_price: Decimal::try_from(current_mid)
                                                .unwrap_or_default(),
                                            size: pos.size,
                                            reason: format!("max_hold_{}s", elapsed_s),
                                        };
                                        if let Err(e) = order_mgr
                                            .process_intent(intent, &rest, &meta_store)
                                            .await
                                        {
                                            error!("[MAIN] Timeout exit error for {}: {}", coin, e);
                                            error_count += 1;
                                            last_error = format!("Timeout exit {}: {}", coin, e);
                                            last_error_ts = now_ms;
                                        }
                                    }
                                }

                                // Regime-forced exit
                                if regime.requires_exit()
                                    && !matches!(order_mgr.state(coin), gbot::execution::order_manager::TradeState::ForceExit { .. })
                                {
                                    if let Some(pos) = position_mgr.get(coin) {
                                        warn!(
                                            "[MAIN] Regime {:?} requires exit for {}",
                                            regime, coin
                                        );
                                        let intent = Intent::ForceExitIoc {
                                            coin: coin.clone(),
                                            direction: pos.direction,
                                            mid_price: Decimal::try_from(current_mid)
                                                .unwrap_or_default(),
                                            size: pos.size,
                                            reason: format!("regime:{:?}", regime),
                                        };
                                        if let Err(e) = order_mgr
                                            .process_intent(intent, &rest, &meta_store)
                                            .await
                                        {
                                            error!("[MAIN] Regime exit error for {}: {}", coin, e);
                                            error_count += 1;
                                            last_error = format!("Regime exit {}: {}", coin, e);
                                            last_error_ts = now_ms;
                                        }
                                    }
                                }

                                // ── 5.3 Signal inverse exit ──────────────────────────────────
                                // Evaluate direction score for this coin. If it crosses the
                                // OPPOSITE threshold, force-exit immediately instead of waiting
                                // for SL/TP/max_hold. This cuts losses early on reversals.
                                if !matches!(order_mgr.state(coin), gbot::execution::order_manager::TradeState::ForceExit { .. })
                                    && features.flow.is_mature()
                                    && features.book.spread_bps > 0.0
                                {
                                    if let Some(pos) = position_mgr.get(coin) {
                                        let pr5s_norm = {
                                            let v = features.flow.price_return_5s;
                                            v.signum() * (v.abs() / 5.0).min(1.0)
                                        };
                                        // Quick directional check: is momentum strongly opposite?
                                        let opposite = match pos.direction {
                                            Direction::Long => pr5s_norm < -0.5,   // strong bearish momentum
                                            Direction::Short => pr5s_norm > 0.5,   // strong bullish momentum
                                        };
                                        if opposite {
                                            info!(
                                                "[MAIN] Signal inverse for {} (pr5s={:.2}bps, pos={:?}) — force exit",
                                                coin, features.flow.price_return_5s, pos.direction
                                            );
                                            let intent = Intent::ForceExitIoc {
                                                coin: coin.clone(),
                                                direction: pos.direction,
                                                mid_price: Decimal::try_from(current_mid)
                                                    .unwrap_or_default(),
                                                size: pos.size,
                                                reason: "signal_inverse".to_string(),
                                            };
                                            if let Err(e) = order_mgr
                                                .process_intent(intent, &rest, &meta_store)
                                                .await
                                            {
                                                error!("[MAIN] Signal inverse exit error for {}: {}", coin, e);
                                            }
                                        }
                                    }
                                }

                                // ── 5.2 Stale quote: cancel TP ALO if conditions deteriorate ─
                                // If toxicity spikes or regime becomes hostile while we have a
                                // resting TP ALO, cancel it (position stays, SL still active).
                                // The max_hold will eventually close the position.
                                if let Some(pos) = position_mgr.get(coin) {
                                    if pos.tp_order_oid.is_some() {
                                        let should_cancel_tp =
                                            features.flow.toxicity_proxy_instant > settings.risk.max_toxicity
                                            || regime == regime_engine::Regime::ActiveToxic
                                            || regime == regime_engine::Regime::NewslikeChaos;
                                        if should_cancel_tp {
                                            let tp_oid = pos.tp_order_oid.clone().unwrap();
                                            let asset_idx = meta_store.get(coin).map(|m| m.asset_index).unwrap_or(0);
                                            info!("[MAIN] Stale quote: cancelling TP ALO {} for {} (tox={:.2}, regime={:?})",
                                                tp_oid, coin, features.flow.toxicity_proxy_instant, regime);
                                            if let Err(e) = rest.cancel_order(coin, &tp_oid, asset_idx).await {
                                                warn!("[MAIN] Failed to cancel stale TP ALO for {}: {}", coin, e);
                                            }
                                            if let Some(pos_mut) = position_mgr.get_mut(coin) {
                                                pos_mut.tp_order_oid = None;
                                            }
                                        }
                                    }
                                }

                                // ── 5.5 Smart max hold: early exit if losing at 70% of max_hold ─
                                // Instead of a blind timeout, if the trade is underwater at 70%
                                // of max_hold, exit early to limit damage. Profitable trades
                                // continue to max_hold (or hit TP/trailing).
                                if !matches!(order_mgr.state(coin), gbot::execution::order_manager::TradeState::ForceExit { .. }) {
                                    if let Some(pos) = position_mgr.get(coin) {
                                        let elapsed_s = (now_ms - pos.opened_at) / 1000;
                                        let early_exit_threshold_s = (settings.execution.max_hold_s as f64 * 0.7) as i64;
                                        if elapsed_s >= early_exit_threshold_s
                                            && elapsed_s < settings.execution.max_hold_s as i64
                                        {
                                            let entry_f = pos.entry_price.to_string().parse::<f64>().unwrap_or(0.0);
                                            let unrealized = match pos.direction {
                                                Direction::Long => current_mid - entry_f,
                                                Direction::Short => entry_f - current_mid,
                                            };
                                            // Exit early only if losing (unrealized < 0)
                                            if unrealized < 0.0 {
                                                info!(
                                                    "[MAIN] Smart max_hold: {} losing ${:.2} at {}s/{}s — early exit",
                                                    coin,
                                                    unrealized * pos.size.to_string().parse::<f64>().unwrap_or(0.0),
                                                    elapsed_s,
                                                    settings.execution.max_hold_s
                                                );
                                                let intent = Intent::ForceExitIoc {
                                                    coin: coin.clone(),
                                                    direction: pos.direction,
                                                    mid_price: Decimal::try_from(current_mid)
                                                        .unwrap_or_default(),
                                                    size: pos.size,
                                                    reason: format!("smart_exit_{}s", elapsed_s),
                                                };
                                                if let Err(e) = order_mgr
                                                    .process_intent(intent, &rest, &meta_store)
                                                    .await
                                                {
                                                    error!("[MAIN] Smart exit error for {}: {}", coin, e);
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            // ── Strategy evaluation for new entries ──
                            // Only enter when coin is flat (not already in position or entry working)
                            let is_flat = matches!(
                                order_mgr.state(coin),
                                gbot::execution::order_manager::TradeState::Flat
                            );

                            // Signal cooldown: skip if this coin emitted a signal recently
                            let in_signal_cooldown = signal_cooldowns
                                .get(coin)
                                .map(|&cd_until| now_ms < cd_until)
                                .unwrap_or(false);

                            // ── Pullback tracker update (Phase 7.5) ───────────────────────────
                            // Runs while the coin is flat, even during the signal cooldown window
                            // (the pullback was armed before cooldown was set). When the pullback
                            // fires we place the entry; on abandon we set a short cooldown.
                            if is_flat && position_mgr.get(coin).is_none()
                                && regime.allows_entry()
                                && pullback_tracker.is_pending(coin)
                            {
                                let ofi = features.flow.ofi_10s;
                                match pullback_tracker.update(
                                    coin,
                                    current_mid,
                                    ofi,
                                    now_ms,
                                    false, // opposite signal detection via reversal (>100% retrace)
                                    &pullback_settings,
                                ) {
                                    UpdateResult::Ready(entry) => {
                                        // Rebuild intent with updated entry price at pullback point
                                        let ep = Decimal::try_from(entry.entry_mid).unwrap_or_default();
                                        let sl_dist = Decimal::try_from(entry.entry_mid * entry.sl_pct).unwrap_or_default();
                                        let tp_dist = Decimal::try_from(entry.entry_mid * entry.tp_pct).unwrap_or_default();
                                        let (sl_price, tp_price) = match entry.direction {
                                            Direction::Long => (ep - sl_dist, ep + tp_dist),
                                            Direction::Short => (ep + sl_dist, ep - tp_dist),
                                        };
                                        let pullback_intent = Intent::PlacePassiveEntry {
                                            coin: coin.clone(),
                                            direction: entry.direction,
                                            price: ep,
                                            stop_loss: sl_price,
                                            take_profit: tp_price,
                                            size: entry.size,
                                            max_wait_s: entry.max_wait_s,
                                        };

                                        // Set signal cooldown only when we actually place
                                        signal_cooldowns.insert(coin.clone(), now_ms + signal_cooldown_ms);
                                        signals_since_summary += 1;

                                        match risk_mgr.validate_intent(
                                            &pullback_intent,
                                            current_equity,
                                            &features,
                                            &position_mgr,
                                        ) {
                                            Ok(()) => {
                                                orders_since_summary += 1;
                                                info!(
                                                    "[PULLBACK] {} {:?} entry placed at {:.4} (sl={:.4} tp={:.4})",
                                                    coin, entry.direction, entry.entry_mid, sl_price, tp_price
                                                );
                                                event_feed.push("order", format!("{} pullback entry", coin));
                                                journal.log_event(&JournalEvent::OrderPlaced {
                                                    ts_local: now_ms,
                                                    coin: coin.clone(),
                                                    direction: format!("{:?}", entry.direction),
                                                    price: ep.to_string(),
                                                    size: entry.size.to_string(),
                                                    tif: "ALO".to_string(),
                                                    client_oid: String::new(),
                                                });
                                                if let Err(e) = order_mgr
                                                    .process_intent(pullback_intent, &rest, &meta_store)
                                                    .await
                                                {
                                                    error!("[PULLBACK] Order error for {}: {}", coin, e);
                                                    risk_mgr.record_error();
                                                    error_count += 1;
                                                    last_error = format!("Pullback order error {}: {}", coin, e);
                                                    last_error_ts = now_ms;
                                                }
                                            }
                                            Err(reasons) => {
                                                rejections_since_summary += 1;
                                                for r in &reasons {
                                                    info!("[PULLBACK] Rejected {}: {}", coin, r);
                                                }
                                                journal.log_event(&JournalEvent::RiskRejection {
                                                    ts_local: now_ms,
                                                    coin: coin.clone(),
                                                    reasons,
                                                });
                                            }
                                        }
                                    }
                                    UpdateResult::Abandoned(reason) => {
                                        info!("[PULLBACK] {} setup abandoned: {:?}", coin, reason);
                                        // Short cooldown to avoid immediately re-arming the same signal
                                        signal_cooldowns.insert(coin.clone(), now_ms + 5_000);
                                    }
                                    _ => {} // Waiting or Idle — nothing to do
                                }
                                continue;
                            }

                            if is_flat && position_mgr.get(coin).is_none() && regime.allows_entry() && !in_signal_cooldown {
                                // Feature maturity guard: skip if insufficient data
                                if !features.flow.is_mature() || features.flow.trade_count_10s < min_trades_for_signal {
                                    continue;
                                }

                                // Book health guard: skip if spread is zero or negative
                                if features.book.spread_bps <= 0.0 {
                                    continue;
                                }

                                if let Some(book) = book_mgr.books.get(coin) {
                                    let (raw_intent, dir_score, queue_score) = match regime {
                                        regime_engine::Regime::QuietThin => {
                                            strategy.evaluate_with_reduced_size(
                                                coin, &features, regime, &book,
                                            )
                                        }
                                        _ => strategy.evaluate(coin, &features, regime, &book),
                                    };

                                    // ── Compute position size (fixes gap #1) ──
                                    let sized_intent = if let Intent::PlacePassiveEntry {
                                        ref coin,
                                        direction,
                                        price,
                                        stop_loss,
                                        take_profit,
                                        max_wait_s,
                                        ..
                                    } = raw_intent
                                    {
                                        let coin_max_lev =
                                            meta_store.get(coin).map(|m| m.max_leverage).unwrap_or(1);
                                        let (size, _leverage) = risk_mgr.compute_position_size(
                                            current_equity,
                                            price,
                                            stop_loss,
                                            coin_max_lev,
                                        );
                                        Intent::PlacePassiveEntry {
                                            coin: coin.clone(),
                                            direction,
                                            price,
                                            stop_loss,
                                            take_profit,
                                            size,
                                            max_wait_s,
                                        }
                                    } else {
                                        raw_intent
                                    };

                                    if !matches!(sized_intent, Intent::NoTrade) {
                                        // Direction confirmation: require consecutive evaluations
                                        let dir_sign: i8 = if dir_score > 0.0 { 1 } else { -1 };
                                        let (prev_sign, count) = direction_confirms.entry(coin.clone()).or_insert((0, 0));
                                        if dir_sign == *prev_sign {
                                            *count += 1;
                                        } else {
                                            *prev_sign = dir_sign;
                                            *count = 1;
                                        }
                                        if *count < min_confirmations {
                                            debug!(
                                                "[MAIN] {} direction confirmation {}/{} — waiting",
                                                coin, count, min_confirmations
                                            );
                                            continue;
                                        }
                                        // Reset counter after direction confirmed
                                        *count = 0;

                                        // ── Per-coin signal quota check ───────────────────────
                                        // Prune timestamps older than 10min, then check quota.
                                        let ts_queue = coin_signal_timestamps
                                            .entry(coin.clone())
                                            .or_insert_with(std::collections::VecDeque::new);
                                        while ts_queue.front().map_or(false, |&t| now_ms - t > signal_quota_window_ms) {
                                            ts_queue.pop_front();
                                        }
                                        if ts_queue.len() as u32 >= max_signals_per_coin_10min {
                                            debug!(
                                                "[MAIN] {} signal quota reached ({}/{} in 10min) — skip",
                                                coin, ts_queue.len(), max_signals_per_coin_10min
                                            );
                                            continue;
                                        }
                                        ts_queue.push_back(now_ms);

                                        // Record signal for observability (before pullback arm)
                                        let (sig_coin, sig_dir, sig_price, sig_sl, sig_tp) = if let Intent::PlacePassiveEntry {
                                            ref coin, direction, price, stop_loss, take_profit, ..
                                        } = sized_intent {
                                            (coin.clone(), format!("{:?}", direction), price.to_string(), stop_loss.to_string(), take_profit.to_string())
                                        } else {
                                            (coin.clone(), String::new(), String::new(), String::new(), String::new())
                                        };
                                        signal_recorder.record(&SignalRecord {
                                            ts: now_ms,
                                            coin: sig_coin,
                                            direction: sig_dir,
                                            dir_score,
                                            queue_score,
                                            entry_price: sig_price,
                                            stop_loss: sig_sl,
                                            take_profit: sig_tp,
                                            spread_bps: features.book.spread_bps,
                                            imbalance_top5: features.book.imbalance_top5,
                                            depth_ratio: features.book.depth_ratio,
                                            micro_price_vs_mid_bps: features.book.micro_price_vs_mid_bps,
                                            vamp_signal_bps: features.book.vamp_signal_bps,
                                            bid_depth_10bps: features.book.bid_depth_10bps,
                                            ask_depth_10bps: features.book.ask_depth_10bps,
                                            ofi_10s: features.flow.ofi_10s,
                                            toxicity: features.flow.toxicity_proxy_instant,
                                            vol_ratio: features.flow.vol_ratio,
                                            aggression: features.flow.aggression_persistence,
                                            trade_intensity: features.flow.trade_intensity,
                                            action: "pullback_armed".to_string(),
                                            rejection_reason: None,
                                        });
                                        signals_since_summary += 1;

                                        // ── Phase 7.5: Arm pullback tracker ─────────────────
                                        // Extract SL/TP as relative percentages, then hand off
                                        // to the pullback tracker. Entry will be placed only
                                        // after pullback + OFI confirmation.
                                        if let Intent::PlacePassiveEntry {
                                            direction, price, stop_loss, take_profit, size, max_wait_s, ..
                                        } = sized_intent {
                                            let p = f64::try_from(price).unwrap_or(current_mid);
                                            let sl = f64::try_from(stop_loss).unwrap_or(p);
                                            let tp = f64::try_from(take_profit).unwrap_or(p);
                                            let sl_pct = if p > 0.0 { (p - sl).abs() / p } else { 0.0 };
                                            let tp_pct = if p > 0.0 { (tp - p).abs() / p } else { 0.0 };
                                            pullback_tracker.start(
                                                coin,
                                                direction,
                                                current_mid,
                                                sl_pct,
                                                tp_pct,
                                                size,
                                                max_wait_s,
                                                dir_score,
                                                now_ms,
                                                &pullback_settings,
                                            );
                                            debug!(
                                                "[MAIN] {} pullback armed: mid={:.4} sl={:.4}% tp={:.4}%",
                                                coin, current_mid, sl_pct * 100.0, tp_pct * 100.0
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }

                    WsEvent::TradePrint { coin, trades } => {
                        let tape_entries: Vec<_> = trades
                            .iter()
                            .map(|t| gbot::market_data::book_manager::TapeEntry {
                                price: t.price,
                                size: t.size,
                                is_buy: t.side == "B",
                                timestamp: t.timestamp,
                            })
                            .collect();
                        recorder.record_trades(coin, &tape_entries).await;
                    }

                    WsEvent::UserOrderUpdate { data } => {
                        // Primary fill path via WebSocket (sub-second, preferred over polling)
                        if let Some(arr) = data.as_array() {
                            for update in arr {
                                let oid = update
                                    .get("oid")
                                    .and_then(|o| o.as_str())
                                    .unwrap_or("");
                                let status = update
                                    .get("status")
                                    .and_then(|s| s.as_str())
                                    .unwrap_or("");

                                match status {
                                    "filled" => {
                                        let avg_px: Decimal = update
                                            .get("avgPx")
                                            .and_then(|p| p.as_str())
                                            .unwrap_or("0")
                                            .parse()
                                            .unwrap_or_default();
                                        let filled_qty: Decimal = update
                                            .get("sz")
                                            .and_then(|s| s.as_str())
                                            .unwrap_or("0")
                                            .parse()
                                            .unwrap_or_default();

                                        // on_fill returns Some(FillEvent) on full fill
                                        match order_mgr.on_fill(oid, avg_px, filled_qty) {
                                            Some(FillEvent::EntryFilled(filled)) => {
                                                fills_since_summary += 1;
                                                // ── Open position + place SL/TP triggers (fixes gap #2) ──
                                                let asset_idx = meta_store
                                                    .get(&filled.coin)
                                                    .map(|m| m.asset_index)
                                                    .unwrap_or(0);
                                                let pos = OpenPosition {
                                                    coin: filled.coin.clone(),
                                                    direction: filled.direction,
                                                    entry_price: filled.fill_price,
                                                    size: filled.size,
                                                    stop_loss: filled.stop_loss,
                                                    take_profit: filled.take_profit,
                                                    original_stop_loss: filled.stop_loss,
                                                    leverage: filled.leverage,
                                                    opened_at: now_ms,
                                                    break_even_applied: false,
                                                    trailing_tier: 0,
                                                    tp_order_oid: None,
                                                    sl_trigger_oid: None,
                                                    client_oid: filled.client_oid.clone(),
                                                };

                                                // Journal: entry fill + position opened
                                                journal.log_event(&JournalEvent::OrderFilled {
                                                    ts_local: now_ms,
                                                    ts_exchange: None,
                                                    coin: filled.coin.clone(),
                                                    oid: filled.client_oid.clone(),
                                                    fill_price: filled.fill_price.to_string(),
                                                    fill_size: filled.size.to_string(),
                                                    latency_ms: None,
                                                });
                                                journal.log_event(&JournalEvent::PositionOpened {
                                                    ts_local: now_ms,
                                                    coin: filled.coin.clone(),
                                                    direction: format!("{:?}", filled.direction),
                                                    entry_price: filled.fill_price.to_string(),
                                                    stop_loss: filled.stop_loss.to_string(),
                                                    take_profit: filled.take_profit.to_string(),
                                                    size: filled.size.to_string(),
                                                    leverage: filled.leverage,
                                                });

                                                if let Err(e) = position_mgr
                                                    .open_position_with_triggers(
                                                        pos,
                                                        &rest,
                                                        &settings.general.mode,
                                                        asset_idx,
                                                    )
                                                    .await
                                                {
                                                    error!(
                                                        "[MAIN] open_position_with_triggers failed for {}: {}",
                                                        filled.coin, e
                                                    );
                                                    error_count += 1;
                                                    last_error = format!("Trigger placement {}: {}", filled.coin, e);
                                                    last_error_ts = now_ms;
                                                }

                                                // Update portfolio state (fixes gap #5)
                                                let notional = filled.fill_price * filled.size;
                                                let fee = notional
                                                    * Decimal::try_from(0.00015_f64).unwrap_or_default();
                                                portfolio.record_fee(fee);
                                                metrics.passive_fill_count.inc();
                                                metrics.open_positions.set(position_mgr.count() as i64);

                                                event_feed.push("fill", format!(
                                                    "{} {:?} filled @ {} (size={})",
                                                    filled.coin, filled.direction, filled.fill_price, filled.size
                                                ));
                                            }

                                            Some(FillEvent::ExitFilled(closed)) => {
                                                fills_since_summary += 1;
                                                // Capture position data before closing
                                                let trade_view = if let Some(pos) = position_mgr.get(&closed.coin) {
                                                    let entry_f = pos.entry_price.to_string().parse::<f64>().unwrap_or(0.0);
                                                    let exit_f = closed.fill_price.to_string().parse::<f64>().unwrap_or(0.0);
                                                    let size_f = pos.size.to_string().parse::<f64>().unwrap_or(0.0);
                                                    let (pnl_pct, pnl_usd) = if entry_f > 0.0 {
                                                        let diff = match pos.direction {
                                                            gbot::strategy::signal::Direction::Long => exit_f - entry_f,
                                                            gbot::strategy::signal::Direction::Short => entry_f - exit_f,
                                                        };
                                                        (diff / entry_f * 100.0, diff * size_f)
                                                    } else {
                                                        (0.0, 0.0)
                                                    };

                                                    // Journal: position closed
                                                    journal.log_event(&JournalEvent::PositionClosed {
                                                        ts_local: now_ms,
                                                        coin: pos.coin.clone(),
                                                        direction: format!("{:?}", pos.direction),
                                                        entry_price: pos.entry_price.to_string(),
                                                        exit_price: closed.fill_price.to_string(),
                                                        pnl: format!("{:.2}", pnl_usd),
                                                        reason: closed.reason.clone(),
                                                    });

                                                    Some(ClosedTradeView {
                                                        coin: pos.coin.clone(),
                                                        direction: format!("{:?}", pos.direction),
                                                        entry_price: entry_f,
                                                        exit_price: exit_f,
                                                        pnl_usd,
                                                        pnl_pct,
                                                        close_reason: closed.reason.clone(),
                                                        opened_at: pos.opened_at,
                                                        closed_at: now_ms,
                                                        hold_s: (now_ms - pos.opened_at) / 1000,
                                                        break_even_applied: pos.break_even_applied,
                                                    })
                                                } else {
                                                    None
                                                };

                                                // Close position in tracker — cancel resting TP ALO if any
                                                let tp_oid_to_cancel = position_mgr.close_position(
                                                    &closed.coin,
                                                    &closed.reason,
                                                    closed.fill_price,
                                                    cooldown_s,
                                                );
                                                if let Some(tp_oid) = tp_oid_to_cancel {
                                                    let asset_idx = meta_store
                                                        .get(&closed.coin)
                                                        .map(|m| m.asset_index)
                                                        .unwrap_or(0);
                                                    if let Err(e) = rest.cancel_order(&closed.coin, &tp_oid, asset_idx).await {
                                                        warn!("[MAIN] Failed to cancel TP ALO {} for {}: {}", tp_oid, closed.coin, e);
                                                    } else {
                                                        info!("[MAIN] Cancelled TP ALO {} for {} (position closed: {})", tp_oid, closed.coin, closed.reason);
                                                    }
                                                }

                                                // Store the closed trade
                                                if let Some(tv) = trade_view {
                                                    portfolio.record_pnl(
                                                        Decimal::try_from(tv.pnl_usd).unwrap_or_default()
                                                    );
                                                    closed_trades.push(tv);
                                                }

                                                // Record closing fee: SL = taker (0.045%), TP/other = maker (0.015%)
                                                let notional = closed.fill_price * closed.size;
                                                let exit_fee_rate = if closed.reason.contains("SL") {
                                                    0.00045_f64 // taker — SL triggers are market orders
                                                } else {
                                                    0.00015_f64 // maker
                                                };
                                                let fee = notional
                                                    * Decimal::try_from(exit_fee_rate).unwrap_or_default();
                                                portfolio.record_fee(fee);
                                                metrics.open_positions.set(position_mgr.count() as i64);

                                                event_feed.push("fill", format!(
                                                    "{} closed @ {} — {}",
                                                    closed.coin, closed.fill_price, closed.reason
                                                ));
                                            }

                                            None => {
                                                // Check if this is a TP ALO fill
                                                let tp_coin = position_mgr.find_coin_by_tp_oid(oid);
                                                if let Some(coin) = tp_coin {
                                                    fills_since_summary += 1;
                                                    let reason = "TP_HIT".to_string();
                                                    if let Some(pos) = position_mgr.get(&coin) {
                                                        let entry_f = pos.entry_price.to_string().parse::<f64>().unwrap_or(0.0);
                                                        let exit_f = avg_px.to_string().parse::<f64>().unwrap_or(0.0);
                                                        let size_f = pos.size.to_string().parse::<f64>().unwrap_or(0.0);
                                                        let pnl_usd = match pos.direction {
                                                            gbot::strategy::signal::Direction::Long => (exit_f - entry_f) * size_f,
                                                            gbot::strategy::signal::Direction::Short => (entry_f - exit_f) * size_f,
                                                        };
                                                        let pnl_pct = if entry_f > 0.0 { pnl_usd / (entry_f * size_f) * 100.0 } else { 0.0 };

                                                        journal.log_event(&JournalEvent::PositionClosed {
                                                            ts_local: now_ms,
                                                            coin: coin.clone(),
                                                            direction: format!("{:?}", pos.direction),
                                                            entry_price: pos.entry_price.to_string(),
                                                            exit_price: avg_px.to_string(),
                                                            pnl: format!("{:.2}", pnl_usd),
                                                            reason: reason.clone(),
                                                        });

                                                        let trade_view = ClosedTradeView {
                                                            coin: pos.coin.clone(),
                                                            direction: format!("{:?}", pos.direction),
                                                            entry_price: entry_f,
                                                            exit_price: exit_f,
                                                            pnl_usd,
                                                            pnl_pct,
                                                            close_reason: reason.clone(),
                                                            opened_at: pos.opened_at,
                                                            closed_at: now_ms,
                                                            hold_s: (now_ms - pos.opened_at) / 1000,
                                                            break_even_applied: pos.break_even_applied,
                                                        };

                                                        // TP exit = maker fee (already ALO)
                                                        let notional = avg_px * filled_qty;
                                                        let fee = notional * Decimal::try_from(0.00015_f64).unwrap_or_default();
                                                        portfolio.record_fee(fee);
                                                        portfolio.record_pnl(Decimal::try_from(pnl_usd).unwrap_or_default());
                                                        closed_trades.push(trade_view);

                                                        info!("[MAIN] TP ALO filled: {} @ {} P&L=${:.2} (maker fee)", coin, avg_px, pnl_usd);
                                                    }

                                                    // Close position — no TP to cancel (it just filled)
                                                    // But cancel the SL trigger
                                                    if let Some(pos) = position_mgr.get(&coin) {
                                                        if let Some(sl_oid) = &pos.sl_trigger_oid {
                                                            let asset_idx = meta_store.get(&coin).map(|m| m.asset_index).unwrap_or(0);
                                                            if let Err(e) = rest.cancel_order(&coin, sl_oid, asset_idx).await {
                                                                warn!("[MAIN] Failed to cancel SL trigger for {}: {}", coin, e);
                                                            }
                                                        }
                                                    }
                                                    // Set tp_order_oid to None before close so it doesn't try to cancel it
                                                    if let Some(pos) = position_mgr.get_mut(&coin) {
                                                        pos.tp_order_oid = None;
                                                    }
                                                    position_mgr.close_position(&coin, &reason, avg_px, cooldown_s);
                                                    order_mgr.set_flat(&coin);
                                                    metrics.open_positions.set(position_mgr.count() as i64);

                                                    event_feed.push("fill", format!(
                                                        "{} TP ALO filled @ {} (maker)",
                                                        coin, avg_px
                                                    ));
                                                }
                                            }
                                        }
                                    }
                                    "canceled" | "cancelled" => {
                                        order_mgr.on_cancel(oid);
                                    }
                                    "rejected" => {
                                        let err = update
                                            .get("error")
                                            .and_then(|e| e.as_str())
                                            .map(String::from);
                                        order_mgr.on_reject(oid, err);
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }

                    WsEvent::Reconnected => {
                        warn!("[MAIN] WebSocket reconnected — pausing trading for 10s");
                        reconnect_recent = true;
                        reconnect_clear_at = now_ms + 10_000;
                        metrics.ws_reconnect_total.inc();
                        event_feed.push("system", "WebSocket reconnected — pausing 10s".into());
                    }

                    WsEvent::SnapshotLoaded { coin } => {
                        info!("[MAIN] Snapshot loaded for {}", coin);
                    }

                    WsEvent::MidUpdate { .. } => {
                        // Already handled by book_manager
                    }
                }

                // ── Order timeout sweep ──
                let timeouts = order_mgr.check_timeouts(now_ms);
                for intent in timeouts {
                    // Journal: order cancelled (timeout)
                    if let Intent::CancelEntry { ref oid, ref reason } = intent {
                        if let Some(order) = order_mgr.pending_orders().get(oid) {
                            journal.log_event(&JournalEvent::OrderCancelled {
                                ts_local: now_ms,
                                coin: order.coin.clone(),
                                oid: oid.clone(),
                                reason: reason.clone(),
                            });
                        }
                    }
                    if let Err(e) = order_mgr.process_intent(intent, &rest, &meta_store).await {
                        error!("[MAIN] Timeout cancel error: {}", e);
                    }
                }

                // ── Dry-run fill simulation ──
                // When mid price crosses a pending entry order price, simulate the fill
                if is_dry_run {
                    let mut simulated_fills: Vec<(String, Decimal, Decimal)> = Vec::new();
                    for (oid, order) in order_mgr.pending_orders() {
                        if order.status != gbot::execution::order_manager::PendingOrderStatus::Working {
                            continue;
                        }
                        if let Some(mid_ref) = book_mgr.mids.get(&order.coin) {
                            let mid_dec = Decimal::try_from(*mid_ref).unwrap_or_default();
                            // Long: fill when mid <= entry price (passive bid)
                            // Short: fill when mid >= entry price (passive ask)
                            let should_fill = match order.direction {
                                gbot::strategy::signal::Direction::Long => mid_dec <= order.price,
                                gbot::strategy::signal::Direction::Short => mid_dec >= order.price,
                            };
                            if should_fill {
                                simulated_fills.push((oid.clone(), order.price, order.size));
                            }
                        }
                    }
                    for (oid, fill_price, fill_qty) in simulated_fills {
                        info!("[DRYFILL] Simulated fill: {} @ {}", oid, fill_price);
                        match order_mgr.on_fill(&oid, fill_price, fill_qty) {
                            Some(FillEvent::EntryFilled(filled)) => {
                                fills_since_summary += 1;
                                let pos = OpenPosition {
                                    coin: filled.coin.clone(),
                                    direction: filled.direction,
                                    entry_price: filled.fill_price,
                                    size: filled.size,
                                    stop_loss: filled.stop_loss,
                                    take_profit: filled.take_profit,
                                    original_stop_loss: filled.stop_loss,
                                    leverage: filled.leverage,
                                    opened_at: now_ms,
                                    break_even_applied: false,
                                    trailing_tier: 0,
                                    tp_order_oid: None,
                                    sl_trigger_oid: None,
                                    client_oid: filled.client_oid.clone(),
                                };

                                // Journal: position opened
                                journal.log_event(&JournalEvent::PositionOpened {
                                    ts_local: now_ms,
                                    coin: filled.coin.clone(),
                                    direction: format!("{:?}", filled.direction),
                                    entry_price: filled.fill_price.to_string(),
                                    stop_loss: filled.stop_loss.to_string(),
                                    take_profit: filled.take_profit.to_string(),
                                    size: filled.size.to_string(),
                                    leverage: filled.leverage,
                                });

                                // In dry-run, skip trigger placement — just open position directly
                                position_mgr.open_position(pos);

                                let notional = filled.fill_price * filled.size;
                                let fee = notional * Decimal::try_from(0.00015_f64).unwrap_or_default();
                                portfolio.record_fee(fee);
                                metrics.passive_fill_count.inc();
                                metrics.open_positions.set(position_mgr.count() as i64);
                                event_feed.push("fill", format!(
                                    "[DRY] {} {:?} filled @ {} (size={})",
                                    filled.coin, filled.direction, filled.fill_price, filled.size
                                ));
                            }
                            _ => {}
                        }
                    }

                    // ── Dry-run position exit simulation ──
                    // Check if mid hit SL or TP for any open position,
                    // or if a ForceExit was requested (max_hold / regime)
                    let mut dry_exits: Vec<(String, Decimal, String)> = Vec::new();
                    for pos in position_mgr.all().values() {
                        // ForceExit: close at current mid immediately
                        if matches!(order_mgr.state(&pos.coin), gbot::execution::order_manager::TradeState::ForceExit { .. }) {
                            if let Some(mid_ref) = book_mgr.mids.get(&pos.coin) {
                                let mid_dec = Decimal::try_from(*mid_ref).unwrap_or_default();
                                let reason = if let gbot::execution::order_manager::TradeState::ForceExit { reason } = order_mgr.state(&pos.coin) {
                                    reason.clone()
                                } else {
                                    "force_exit".to_string()
                                };
                                dry_exits.push((pos.coin.clone(), mid_dec, reason));
                            }
                            continue;
                        }
                        if let Some(mid_ref) = book_mgr.mids.get(&pos.coin) {
                            let mid_dec = Decimal::try_from(*mid_ref).unwrap_or_default();
                            match pos.direction {
                                gbot::strategy::signal::Direction::Long => {
                                    if mid_dec >= pos.take_profit {
                                        dry_exits.push((pos.coin.clone(), pos.take_profit, "TP_HIT".to_string()));
                                    } else if mid_dec <= pos.stop_loss {
                                        dry_exits.push((pos.coin.clone(), pos.stop_loss, "SL_HIT".to_string()));
                                    }
                                }
                                gbot::strategy::signal::Direction::Short => {
                                    if mid_dec <= pos.take_profit {
                                        dry_exits.push((pos.coin.clone(), pos.take_profit, "TP_HIT".to_string()));
                                    } else if mid_dec >= pos.stop_loss {
                                        dry_exits.push((pos.coin.clone(), pos.stop_loss, "SL_HIT".to_string()));
                                    }
                                }
                            }
                        }
                    }
                    for (coin, exit_price, reason) in dry_exits {
                        if let Some(pos) = position_mgr.get(&coin) {
                            let entry_f = pos.entry_price.to_string().parse::<f64>().unwrap_or(0.0);
                            let exit_f = exit_price.to_string().parse::<f64>().unwrap_or(0.0);
                            let size_f = pos.size.to_string().parse::<f64>().unwrap_or(0.0);
                            let notional_f = entry_f * size_f;

                            // Exit fee: SL = taker (0.045%), TP/other = maker (0.015%)
                            // Entry fee (maker 0.015%) already recorded at fill time
                            let exit_fee_rate = if reason.contains("SL") { 0.00045 } else { 0.00015 };
                            let exit_fee = notional_f * exit_fee_rate;
                            let entry_fee = notional_f * 0.00015; // for display only

                            // Gross P&L (same as live path — portfolio handles fees separately)
                            let (_pnl_pct, pnl_usd) = if entry_f > 0.0 {
                                let diff = match pos.direction {
                                    gbot::strategy::signal::Direction::Long => exit_f - entry_f,
                                    gbot::strategy::signal::Direction::Short => entry_f - exit_f,
                                };
                                (diff / entry_f * 100.0, diff * size_f)
                            } else {
                                (0.0, 0.0)
                            };
                            let net_pnl = pnl_usd - entry_fee - exit_fee;

                            info!(
                                "[DRYFILL] {} closed: {} @ {} | P&L: ${:.2} net=${:.2} fees=${:.2}",
                                coin, reason, exit_price, pnl_usd, net_pnl, entry_fee + exit_fee
                            );

                            // Journal: position closed
                            journal.log_event(&JournalEvent::PositionClosed {
                                ts_local: now_ms,
                                coin: coin.clone(),
                                direction: format!("{:?}", pos.direction),
                                entry_price: pos.entry_price.to_string(),
                                exit_price: exit_price.to_string(),
                                pnl: format!("{:.2}", net_pnl),
                                reason: reason.clone(),
                            });

                            let trade_view = ClosedTradeView {
                                coin: pos.coin.clone(),
                                direction: format!("{:?}", pos.direction),
                                entry_price: entry_f,
                                exit_price: exit_f,
                                pnl_usd: net_pnl,
                                pnl_pct: if notional_f > 0.0 { net_pnl / notional_f * 100.0 } else { 0.0 },
                                close_reason: reason.clone(),
                                opened_at: pos.opened_at,
                                closed_at: now_ms,
                                hold_s: (now_ms - pos.opened_at) / 1000,
                                break_even_applied: pos.break_even_applied,
                            };

                            // Portfolio: gross P&L + exit fee only (entry fee already recorded)
                            portfolio.record_pnl(Decimal::try_from(pnl_usd).unwrap_or_default());
                            portfolio.record_fee(
                                Decimal::try_from(exit_fee).unwrap_or_default()
                            );
                            let _tp_oid = position_mgr.close_position(&coin, &reason, exit_price, cooldown_s);
                            // In dry-run, no exchange order to cancel
                            order_mgr.set_flat(&coin);
                            closed_trades.push(trade_view);
                            metrics.open_positions.set(position_mgr.count() as i64);

                            // Update simulated equity with net P&L (gross - all fees)
                            current_equity += Decimal::try_from(net_pnl).unwrap_or_default();
                            risk_mgr.update_equity(current_equity);

                            event_feed.push("fill", format!(
                                "[DRY] {} closed {} @ {} P&L ${:.2}",
                                coin, reason, exit_price, pnl_usd
                            ));
                        }
                    }
                }

                // ── Circuit breaker ──
                risk_mgr.check_circuit_breaker(current_equity);

                // ── Periodic summary (every 5 minutes) ──
                if now_ms - last_summary_ms >= summary_interval_ms {
                    let uptime_min = (now_ms - started_at_ms) / 60_000;
                    let total_closed = closed_trades.len();
                    let total_pnl: f64 = closed_trades.iter().map(|t| t.pnl_usd).sum();
                    let total_wins = closed_trades.iter().filter(|t| t.pnl_usd > 0.0).count();
                    let wr = if total_closed > 0 { total_wins as f64 / total_closed as f64 * 100.0 } else { 0.0 };
                    info!(
                        "[SUMMARY] Uptime={}min | Equity=${} | Positions={} | Closed={} (WR={:.0}%) | P&L=${:.2} | Since last: signals={} orders={} rejected={} fills={}",
                        uptime_min,
                        current_equity,
                        position_mgr.count(),
                        total_closed,
                        wr,
                        total_pnl,
                        signals_since_summary,
                        orders_since_summary,
                        rejections_since_summary,
                        fills_since_summary,
                    );
                    signals_since_summary = 0;
                    orders_since_summary = 0;
                    rejections_since_summary = 0;
                    fills_since_summary = 0;
                    last_summary_ms = now_ms;

                    // Persist risk state every summary cycle
                    risk_mgr.save_state();
                }

                // ── Metrics snapshot ──
                metrics.open_positions.set(position_mgr.count() as i64);
                let dd_pct = if initial_equity > Decimal::ZERO {
                    ((initial_equity - current_equity) / initial_equity * Decimal::new(100, 0))
                        .to_string()
                        .parse::<f64>()
                        .unwrap_or(0.0)
                        .max(0.0)
                } else {
                    0.0
                };
                metrics.drawdown_pct.set(dd_pct);
            }

            // ── Dashboard snapshot (every 500ms) ──
            _ = dashboard_tick.tick() => {
                let now_ms = chrono::Utc::now().timestamp_millis();
                let eq_f64 = current_equity.to_string().parse::<f64>().unwrap_or(0.0);
                let init_f64 = initial_equity.to_string().parse::<f64>().unwrap_or(0.0);
                let dd = if init_f64 > 0.0 { ((init_f64 - eq_f64) / init_f64 * 100.0).max(0.0) } else { 0.0 };
                let daily_start_f64 = portfolio.daily_start_balance.to_string().parse::<f64>().unwrap_or(0.0);
                let daily_pnl = eq_f64 - daily_start_f64;

                // Build position views
                let mut pos_views = Vec::new();
                for pos in position_mgr.all().values() {
                    let mid = book_mgr.mids.get(&pos.coin).map(|m| *m).unwrap_or(0.0);
                    let entry_f = pos.entry_price.to_string().parse::<f64>().unwrap_or(0.0);
                    let size_f = pos.size.to_string().parse::<f64>().unwrap_or(0.0);
                    let (pnl_pct, pnl_usd) = if entry_f > 0.0 {
                        let diff = match pos.direction {
                            gbot::strategy::signal::Direction::Long => mid - entry_f,
                            gbot::strategy::signal::Direction::Short => entry_f - mid,
                        };
                        (diff / entry_f * 100.0, diff * size_f)
                    } else {
                        (0.0, 0.0)
                    };
                    pos_views.push(PositionView {
                        coin: pos.coin.clone(),
                        direction: format!("{:?}", pos.direction),
                        state: "InPosition".into(),
                        entry_price: entry_f,
                        current_price: mid,
                        pnl_pct,
                        pnl_usd,
                        elapsed_s: (now_ms - pos.opened_at) / 1000,
                        break_even_applied: pos.break_even_applied,
                        sl: pos.stop_loss.to_string().parse::<f64>().unwrap_or(0.0),
                        tp: pos.take_profit.to_string().parse::<f64>().unwrap_or(0.0),
                    });
                }

                // Build pending order views
                let mut pending_views = Vec::new();
                for order in order_mgr.pending_orders().values() {
                    if order.status == gbot::execution::order_manager::PendingOrderStatus::Working {
                        pending_views.push(PendingOrderView {
                            coin: order.coin.clone(),
                            direction: format!("{:?}", order.direction),
                            state: "EntryWorking".into(),
                            price: order.price.to_string().parse::<f64>().unwrap_or(0.0),
                            placed_s_ago: (now_ms - order.placed_at) / 1000,
                            max_wait_s: order.max_wait_s,
                        });
                    }
                }

                // Build book views
                let mut book_views = HashMap::new();
                for coin in &coins {
                    if let Some(feats) = feature_engine.get(coin) {
                        let regime = regimes.get(coin).copied().unwrap_or(regime_engine::Regime::LowSignal);
                        book_views.insert(coin.clone(), BookView {
                            spread_bps: feats.book.spread_bps,
                            imbalance_top5: feats.book.imbalance_top5,
                            micro_price_vs_mid_bps: feats.book.micro_price_vs_mid_bps,
                            toxicity: feats.flow.toxicity_proxy_instant,
                            regime: format!("{:?}", regime),
                        });
                    }
                }

                // Build metrics view
                let metrics_view = MetricsView {
                    maker_fill_rate_1h: metrics.maker_share.get(),
                    adverse_selection_rate_1h: metrics.adverse_selection_rate.get(),
                    spread_capture_bps_session: metrics.spread_capture_bps.get(),
                    ws_reconnects_today: metrics.ws_reconnect_total.get(),
                    queue_lag_ms_p95: metrics.queue_lag_ms.get(),
                    kill_switch_count: metrics.kill_switch_total.get(),
                };

                // Build bot status with period breakdowns
                let uptime_s = (now_ms - started_at_ms) / 1000;
                let total_trades = closed_trades.len() as u32;
                let total_wins = closed_trades.iter().filter(|t| t.pnl_usd > 0.0).count() as u32;
                let total_losses = total_trades - total_wins;
                let total_pnl: f64 = closed_trades.iter().map(|t| t.pnl_usd).sum();
                let win_rate = if total_trades > 0 { total_wins as f64 / total_trades as f64 * 100.0 } else { 0.0 };

                let cutoff_1h = now_ms - 3_600_000;
                let cutoff_24h = now_ms - 86_400_000;
                let cutoff_7d = now_ms - 604_800_000;

                let trades_1h: Vec<&ClosedTradeView> = closed_trades.iter().filter(|t| t.closed_at >= cutoff_1h).collect();
                let trades_24h: Vec<&ClosedTradeView> = closed_trades.iter().filter(|t| t.closed_at >= cutoff_24h).collect();
                let trades_7d: Vec<&ClosedTradeView> = closed_trades.iter().filter(|t| t.closed_at >= cutoff_7d).collect();

                let pnl_1h: f64 = trades_1h.iter().map(|t| t.pnl_usd).sum();
                let pnl_24h: f64 = trades_24h.iter().map(|t| t.pnl_usd).sum();
                let pnl_7d: f64 = trades_7d.iter().map(|t| t.pnl_usd).sum();

                let wr = |trades: &[&ClosedTradeView]| -> f64 {
                    if trades.is_empty() { 0.0 }
                    else { trades.iter().filter(|t| t.pnl_usd > 0.0).count() as f64 / trades.len() as f64 * 100.0 }
                };

                let bot_status = BotStatusView {
                    mode: format!("{:?}", settings.general.mode),
                    started_at: started_at_ms,
                    uptime_s,
                    active_coins: coins.clone(),
                    error_count,
                    warn_count,
                    last_error: last_error.clone(),
                    last_error_ts,
                    total_trades,
                    total_wins,
                    total_losses,
                    total_pnl_usd: total_pnl,
                    win_rate_pct: win_rate,
                    pnl_1h,
                    pnl_24h,
                    pnl_7d,
                    trades_1h: trades_1h.len() as u32,
                    trades_24h: trades_24h.len() as u32,
                    trades_7d: trades_7d.len() as u32,
                    win_rate_1h: wr(&trades_1h),
                    win_rate_24h: wr(&trades_24h),
                    win_rate_7d: wr(&trades_7d),
                };

                let snap = DashboardSnapshot {
                    ts: now_ms,
                    equity: eq_f64,
                    drawdown_pct: dd,
                    daily_pnl,
                    positions: pos_views,
                    pending_orders: pending_views,
                    books: book_views,
                    metrics: metrics_view,
                    events: event_feed.snapshot(),
                    closed_trades: closed_trades.clone(),
                    bot_status,
                };

                *dashboard_snapshot.write().await = snap;
            }

            _ = &mut shutdown_signal => {
                info!("[MAIN] Shutdown signal received — saving risk state...");
                risk_mgr.save_state();
                info!("[MAIN] Risk state saved. Exiting.");
                break;
            }

            else => {
                error!("[MAIN] Event channel closed — shutting down");
                risk_mgr.save_state();
                break;
            }
        }
    }

    Ok(())
}
