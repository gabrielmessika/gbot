use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use rust_decimal::Decimal;
use tokio::sync::{mpsc, RwLock};
use tracing::{error, info, warn};

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
    self, BookView, DashboardSnapshot, DashboardState, EventFeed, MetricsView,
    PendingOrderView, PositionView,
};
use gbot::observability::metrics::Metrics;
use gbot::persistence::journal::Journal;
use gbot::portfolio::state::PortfolioState;
use gbot::regime::engine as regime_engine;
use gbot::risk::manager::RiskManager;
use gbot::strategy::mfdp::MfdpStrategy;
use gbot::strategy::signal::Intent;

#[tokio::main]
async fn main() -> Result<()> {
    // ── Load config ──
    let settings = Settings::load()?;

    // ── Init tracing ──
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&settings.general.log_level));
    tracing_subscriber::fmt().with_env_filter(env_filter).json().init();

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

    // ── Load coin metadata ──
    let meta_store = match rest.fetch_meta().await {
        Ok(meta) => {
            let universe = meta
                .get("universe")
                .and_then(|u| u.as_array())
                .map(|arr| arr.to_vec())
                .unwrap_or_default();
            let store = CoinMetaStore::from_exchange_meta(&universe);
            info!("[MAIN] Loaded metadata for {} coins", store.all().len());
            store
        }
        Err(e) => {
            warn!("[MAIN] Failed to load metadata: {} — using empty store", e);
            CoinMetaStore::new()
        }
    };

    // ── Get initial equity ──
    let initial_equity = match rest.get_equity().await {
        Ok(eq) => {
            info!("[MAIN] Initial equity: ${}", eq);
            eq
        }
        Err(e) => {
            warn!("[MAIN] Failed to get equity: {} — using $0", e);
            Decimal::ZERO
        }
    };

    // ── Init components ──
    let book_mgr = BookManager::new(&coins, settings.features.trade_tape_size);
    let feature_engine = FeatureEngine::new(&coins);
    let recorder = Recorder::new(
        &settings.general.data_dir,
        &coins,
        settings.recording.enabled,
    );
    let _journal = Journal::new(&settings.general.data_dir)?;
    let metrics = Arc::new(Metrics::new());
    let strategy = MfdpStrategy::new(settings.strategy.clone());
    let mut risk_mgr = RiskManager::new(settings.risk.clone(), initial_equity);
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

    // ── Dashboard state ──
    let mut regimes: HashMap<String, regime_engine::Regime> = HashMap::new();
    let mut event_feed = EventFeed::new(30);
    let mut dashboard_tick = tokio::time::interval(Duration::from_millis(500));

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
                if now_ms - last_equity_fetch_ms > equity_fetch_interval_ms {
                    match rest.get_equity().await {
                        Ok(eq) => {
                            current_equity = eq;
                            risk_mgr.update_equity(current_equity);
                            risk_mgr.check_daily_reset(current_equity);
                            metrics.equity.set(
                                current_equity.to_string().parse::<f64>().unwrap_or(0.0),
                            );
                            last_equity_fetch_ms = now_ms;
                        }
                        Err(e) => warn!("[MAIN] Equity fetch failed: {}", e),
                    }
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
                                if elapsed_s > settings.execution.max_hold_s as i64 {
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
                                        }
                                    }
                                }

                                // Regime-forced exit
                                if regime.requires_exit() {
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
                            if is_flat && position_mgr.get(coin).is_none() && regime.allows_entry() {
                                if let Some(book) = book_mgr.books.get(coin) {
                                    let raw_intent = match regime {
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
                                        match risk_mgr.validate_intent(
                                            &sized_intent,
                                            current_equity,
                                            &features,
                                            &position_mgr,
                                        ) {
                                            Ok(()) => {
                                                event_feed.push("order", format!(
                                                    "{} entry placed", coin
                                                ));
                                                if let Err(e) = order_mgr
                                                    .process_intent(sized_intent, &rest, &meta_store)
                                                    .await
                                                {
                                                    error!("[MAIN] Order error for {}: {}", coin, e);
                                                    risk_mgr.record_error();
                                                    event_feed.push("risk", format!(
                                                        "{} order error: {}", coin, e
                                                    ));
                                                }
                                            }
                                            Err(reasons) => {
                                                for r in &reasons {
                                                    info!("[RISK] Rejected {}: {}", coin, r);
                                                }
                                                event_feed.push("risk", format!(
                                                    "{} rejected: {}", coin, reasons.join(", ")
                                                ));
                                            }
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
                                                    tp_trigger_oid: None,
                                                    sl_trigger_oid: None,
                                                    client_oid: filled.client_oid.clone(),
                                                };
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
                                                // Close position in tracker
                                                position_mgr.close_position(
                                                    &closed.coin,
                                                    &closed.reason,
                                                    closed.fill_price,
                                                    cooldown_s,
                                                );

                                                // Record PnL in portfolio
                                                // (simplified: we record the closing fee; PnL computed from exchange)
                                                let notional = closed.fill_price * closed.size;
                                                let fee = notional
                                                    * Decimal::try_from(0.00015_f64).unwrap_or_default();
                                                portfolio.record_fee(fee);
                                                metrics.open_positions.set(position_mgr.count() as i64);

                                                event_feed.push("fill", format!(
                                                    "{} closed @ {} — {}",
                                                    closed.coin, closed.fill_price, closed.reason
                                                ));
                                            }

                                            None => {} // Partial fill — state already updated
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
                    if let Err(e) = order_mgr.process_intent(intent, &rest, &meta_store).await {
                        error!("[MAIN] Timeout cancel error: {}", e);
                    }
                }

                // ── Circuit breaker ──
                risk_mgr.check_circuit_breaker(current_equity);

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
                };

                *dashboard_snapshot.write().await = snap;
            }

            else => {
                error!("[MAIN] Event channel closed — shutting down");
                break;
            }
        }
    }

    Ok(())
}
