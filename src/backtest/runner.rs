use std::collections::VecDeque;
use std::path::Path;

use anyhow::Result;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::backtest::replay_engine::{BacktestSummary, BacktestTrade, ReplayEngine};
use crate::backtest::sim_execution::FeeModel;
use crate::config::settings::Settings;
use crate::features::book_features::{self, SpreadAverage};
use crate::features::engine::CoinFeatures;
use crate::features::flow_features;
use crate::market_data::book::OrderBook;
use crate::market_data::book_manager::TapeEntry;
use crate::market_data::recorder::{BookRecord, TradeRecord};
use crate::regime::engine as regime_engine;
use crate::risk::manager::RiskManager;
use crate::strategy::mfdp::MfdpStrategy;
use crate::strategy::signal::{Direction, Intent};

/// Phase 7.6 — SL mode for comparison runs.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum SlMode {
    /// Dynamic SL based on realized_vol_30s (current strategy).
    Dynamic,
    /// Fixed SL in bps, ignoring volatility.
    Fixed(f64),
}

/// Phase 7.6 — Result of a comparison backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonResult {
    pub dynamic_sl: BacktestSummary,
    pub fixed_sl: BacktestSummary,
    /// Improvement of dynamic over fixed (positive = dynamic is better).
    pub pnl_delta: f64,
    pub hit_rate_delta: f64,
    pub avg_mae_delta: f64,
}

/// Replays recorded JSONL sessions through the full MFDP pipeline.
///
/// Position sizing mirrors the live risk manager:
///   size_usd = (equity × max_loss_per_trade_pct) / sl_distance_pct
///   capped by max_leverage and max_margin_usage_pct
///
/// Equity is updated after each closed trade so later trades reflect actual P&L.
pub struct BacktestRunner {
    strategy: MfdpStrategy,
    replay: ReplayEngine,
    initial_equity: f64,
    settings_risk: crate::config::settings::RiskSettings,
}

impl BacktestRunner {
    pub fn new(strategy: MfdpStrategy, initial_equity: f64, settings: &Settings) -> Self {
        Self {
            strategy,
            replay: ReplayEngine::new(initial_equity),
            initial_equity,
            settings_risk: settings.risk.clone(),
        }
    }

    /// Run a backtest from JSONL files recorded by the Recorder.
    ///
    /// `dates`: one or more `YYYY-MM-DD` strings. Dates are replayed in order;
    /// equity is carried over between dates (continuous session).
    /// Pass an empty slice to auto-discover all available dates in `data_dir/l2/`.
    ///
    /// Directory layout:
    /// ```text
    /// data/l2/{COIN}/{YYYY-MM-DD}.jsonl
    /// data/trades/{COIN}/{YYYY-MM-DD}.jsonl
    /// ```
    pub fn run_from_files(
        &mut self,
        data_dir: &str,
        coins: &[String],
        dates: &[String],
        settings: &Settings,
    ) -> Result<BacktestSummary> {
        self.run_with_sl_mode(data_dir, coins, dates, settings, SlMode::Dynamic)
    }

    /// Run with a specific SL mode (Dynamic or Fixed).
    ///
    /// When `dates` is empty, all available dates are auto-discovered from
    /// `data_dir/l2/<first_coin>/` and replayed in chronological order.
    /// Equity is carried continuously across dates.
    pub fn run_with_sl_mode(
        &mut self,
        data_dir: &str,
        coins: &[String],
        dates: &[String],
        settings: &Settings,
        sl_mode: SlMode,
    ) -> Result<BacktestSummary> {
        // Reset engine for a fresh run
        self.replay = ReplayEngine::new(self.initial_equity);

        let resolved_dates = if dates.is_empty() {
            Self::discover_dates(data_dir, coins)
        } else {
            dates.to_vec()
        };

        if resolved_dates.is_empty() {
            info!("[BACKTEST] No dates found in {}/l2/ — nothing to replay", data_dir);
            return Ok(self.replay.summary());
        }

        info!(
            "[BACKTEST] Replaying {} date(s): {} … {} (SL={:?})",
            resolved_dates.len(),
            resolved_dates.first().map(String::as_str).unwrap_or(""),
            resolved_dates.last().map(String::as_str).unwrap_or(""),
            sl_mode,
        );

        // Replay dates in order — equity carries across dates
        for date in &resolved_dates {
            for coin in coins {
                let book_path = Path::new(data_dir)
                    .join("l2")
                    .join(coin)
                    .join(format!("{}.jsonl", date));
                let trade_path = Path::new(data_dir)
                    .join("trades")
                    .join(coin)
                    .join(format!("{}.jsonl", date));

                if !book_path.exists() {
                    continue;
                }

                let book_records = Self::load_book_records(&book_path)?;
                let trade_records = if trade_path.exists() {
                    Self::load_trade_records(&trade_path)?
                } else {
                    Vec::new()
                };

                info!(
                    "[BACKTEST] {} on {}: {} L2 snapshots, {} trades",
                    coin, date, book_records.len(), trade_records.len(),
                );

                self.replay_coin(coin, &book_records, &trade_records, settings, sl_mode)?;
            }
        }

        Ok(self.replay.summary())
    }

    /// Discover all available dates (chronological) from `data_dir/l2/{coin}/`.
    pub fn discover_dates(data_dir: &str, coins: &[String]) -> Vec<String> {
        use std::collections::BTreeSet;
        let mut dates: BTreeSet<String> = BTreeSet::new();
        // Union of dates across all coins
        let search_coins: Vec<_> = if coins.is_empty() {
            // Discover all coin dirs
            let l2_dir = Path::new(data_dir).join("l2");
            std::fs::read_dir(&l2_dir)
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                      .filter_map(|e| e.file_name().into_string().ok())
                      .collect()
                })
                .unwrap_or_default()
        } else {
            coins.iter().map(|s| s.clone()).collect()
        };

        for coin in &search_coins {
            let coin_dir = Path::new(data_dir).join("l2").join(coin);
            if let Ok(rd) = std::fs::read_dir(&coin_dir) {
                for entry in rd.filter_map(|e| e.ok()) {
                    let name = entry.file_name().into_string().unwrap_or_default();
                    if name.ends_with(".jsonl") && name.len() == 16 {
                        // "YYYY-MM-DD.jsonl" → strip suffix
                        dates.insert(name[..10].to_string());
                    }
                }
            }
        }
        dates.into_iter().collect()
    }

    /// Run dynamic vs fixed SL comparison and return both summaries + deltas.
    pub fn run_comparison(
        &mut self,
        data_dir: &str,
        coins: &[String],
        dates: &[String],
        settings: &Settings,
        fixed_sl_bps: f64,
    ) -> Result<ComparisonResult> {
        let dynamic = self.run_with_sl_mode(data_dir, coins, dates, settings, SlMode::Dynamic)?;
        let fixed   = self.run_with_sl_mode(data_dir, coins, dates, settings, SlMode::Fixed(fixed_sl_bps))?;

        info!(
            "[BACKTEST] Comparison — Dynamic: P&L={:.2}$ WR={:.1}% | Fixed {}bps: P&L={:.2}$ WR={:.1}%",
            dynamic.total_pnl_net, dynamic.hit_rate, fixed_sl_bps, fixed.total_pnl_net, fixed.hit_rate,
        );

        Ok(ComparisonResult {
            pnl_delta:       dynamic.total_pnl_net - fixed.total_pnl_net,
            hit_rate_delta:  dynamic.hit_rate       - fixed.hit_rate,
            avg_mae_delta:   dynamic.avg_mae_bps    - fixed.avg_mae_bps,
            dynamic_sl: dynamic,
            fixed_sl:   fixed,
        })
    }

    fn replay_coin(
        &mut self,
        coin: &str,
        book_records: &[BookRecord],
        trade_records: &[TradeRecord],
        settings: &Settings,
        sl_mode: SlMode,
    ) -> Result<()> {
        use crate::market_data::book::BookLevel;

        let fee_model = FeeModel::hyperliquid();
        let mut book = OrderBook::new(coin.to_string());
        let mut tape: VecDeque<TapeEntry> = VecDeque::with_capacity(1000);
        let mut spread_avg = SpreadAverage::new(1000);

        let mut book_idx = 0usize;
        let mut trade_idx = 0usize;
        let mut active: Option<ActiveTrade> = None;

        // Running equity: starts at initial_equity, updated after each closed trade.
        // This means later trades in the session are sized based on actual P&L.
        let mut equity = self.replay.initial_equity();

        // RiskManager for position sizing (no position tracking needed in backtest —
        // we only use compute_position_size, not validate_intent)
        let risk_mgr = RiskManager::new(
            self.settings_risk.clone(),
            Decimal::try_from(equity).unwrap_or(Decimal::new(10_000, 0)),
        );

        // Interleave book snapshots and trade records by timestamp
        while book_idx < book_records.len() || trade_idx < trade_records.len() {
            let next_book_ts = book_records.get(book_idx).map(|r| r.timestamp).unwrap_or(i64::MAX);
            let next_trade_ts = trade_records.get(trade_idx).map(|r| r.timestamp).unwrap_or(i64::MAX);

            let now_ms = if next_book_ts <= next_trade_ts {
                let rec = &book_records[book_idx];
                book_idx += 1;
                if rec.best_bid > 0.0 && rec.best_ask > 0.0 {
                    // Prefer multi-level book when available (recorded since bid_levels/ask_levels addition).
                    if !rec.bid_levels.is_empty() && !rec.ask_levels.is_empty() {
                        let bids: Vec<BookLevel> = rec.bid_levels.iter()
                            .map(|l| BookLevel { price: l[0], size: l[1] })
                            .collect();
                        let asks: Vec<BookLevel> = rec.ask_levels.iter()
                            .map(|l| BookLevel { price: l[0], size: l[1] })
                            .collect();
                        book.apply_snapshot(&bids, &asks, rec.timestamp);
                    } else {
                        // Fallback: single-level from depth fields (older recordings).
                        let mid = (rec.best_bid + rec.best_ask) / 2.0;
                        let bid_size = if mid > 0.0 { rec.bid_depth_10bps / mid } else { 1.0 };
                        let ask_size = if mid > 0.0 { rec.ask_depth_10bps / mid } else { 1.0 };
                        book.apply_snapshot(
                            &[BookLevel { price: rec.best_bid, size: bid_size.max(0.001) }],
                            &[BookLevel { price: rec.best_ask, size: ask_size.max(0.001) }],
                            rec.timestamp,
                        );
                    }
                }
                rec.timestamp
            } else {
                let rec = &trade_records[trade_idx];
                trade_idx += 1;
                tape.push_back(TapeEntry {
                    price: rec.price,
                    size: rec.size,
                    is_buy: rec.is_buy,
                    timestamp: rec.timestamp,
                });
                if tape.len() > 1000 {
                    tape.pop_front();
                }
                rec.timestamp
            };

            // ── Update MAE/MFE + check exit conditions for active trade ──────
            if let Some(ref mut trade) = active {
                let mid = book.mid().unwrap_or(trade.entry_price);

                // Track MAE/MFE (signed bps from entry)
                if trade.entry_price > 0.0 {
                    let signed_bps = match trade.direction {
                        Direction::Long  => (mid - trade.entry_price) / trade.entry_price * 10_000.0,
                        Direction::Short => (trade.entry_price - mid) / trade.entry_price * 10_000.0,
                    };
                    if signed_bps < 0.0 {
                        trade.mae_bps = trade.mae_bps.max(-signed_bps);
                    } else {
                        trade.mfe_bps = trade.mfe_bps.max(signed_bps);
                    }
                    // Adverse selection: was mid against direction at +5s?
                    let elapsed_s = (now_ms - trade.entry_ts) as f64 / 1000.0;
                    if !trade.adverse_5s_recorded && elapsed_s >= 5.0 {
                        trade.adverse_5s = signed_bps < 0.0;
                        trade.adverse_5s_recorded = true;
                    }
                }

                let hold_limit_s = settings.execution.max_hold_s as i64;

                let exit = match trade.direction {
                    Direction::Long => {
                        if mid <= trade.stop_loss {
                            Some(("SL_HIT", trade.stop_loss))
                        } else if mid >= trade.take_profit {
                            Some(("TP_HIT", trade.take_profit))
                        } else if (now_ms - trade.entry_ts) / 1000 > hold_limit_s {
                            Some(("TIMEOUT", mid))
                        } else {
                            None
                        }
                    }
                    Direction::Short => {
                        if mid >= trade.stop_loss {
                            Some(("SL_HIT", trade.stop_loss))
                        } else if mid <= trade.take_profit {
                            Some(("TP_HIT", trade.take_profit))
                        } else if (now_ms - trade.entry_ts) / 1000 > hold_limit_s {
                            Some(("TIMEOUT", mid))
                        } else {
                            None
                        }
                    }
                };

                if let Some((reason, exit_price)) = exit {
                    let pnl_gross = match trade.direction {
                        Direction::Long  => (exit_price - trade.entry_price) * trade.size,
                        Direction::Short => (trade.entry_price - exit_price) * trade.size,
                    };
                    let notional = trade.entry_price * trade.size;
                    let fee_entry = fee_model.maker_fee(notional);
                    let fee_exit = if reason == "SL_HIT" {
                        fee_model.taker_fee(notional)
                    } else {
                        fee_model.maker_fee(notional)
                    };
                    let pnl_net = pnl_gross - fee_entry - fee_exit;

                    // Update running equity so subsequent trades are sized correctly
                    equity += pnl_net;

                    self.replay.record_trade(BacktestTrade {
                        coin: coin.to_string(),
                        direction: format!("{:?}", trade.direction),
                        entry_price: trade.entry_price,
                        exit_price,
                        size: trade.size,
                        size_usd: trade.size_usd,
                        leverage: trade.leverage,
                        entry_maker: true,
                        exit_maker: reason != "SL_HIT",
                        pnl_gross,
                        fee_entry,
                        fee_exit,
                        pnl_net,
                        hold_duration_s: (now_ms - trade.entry_ts) as f64 / 1000.0,
                        exit_reason: reason.to_string(),
                        entry_ts: trade.entry_ts,
                        exit_ts: now_ms,
                        mae_bps: trade.mae_bps,
                        mfe_bps: trade.mfe_bps,
                        adverse_5s: trade.adverse_5s,
                    });
                    active = None;
                }
            }

            // ── Strategy evaluation — only when flat and book ready ──────────
            if active.is_none() && book.snapshot_loaded && tape.len() >= 10 {
                let book_feats = book_features::compute_book_features(&book, &mut spread_avg);
                let flow_feats = flow_features::compute_flow_features(&tape, now_ms, 0.0);

                let features = CoinFeatures { book: book_feats, flow: flow_feats, timestamp: now_ms };

                let regime = regime_engine::classify(&features, false, false, &settings.regime, None);

                // ── Directional MFDP path ─────────────────────
                let (intent, _dir_score, _queue_score) = self.strategy.evaluate(coin, &features, regime, &book);

                if let Intent::PlacePassiveEntry {
                    direction,
                    price: entry_price,
                    stop_loss,
                    take_profit,
                    ..
                } = intent
                {
                    let entry = f64::try_from(entry_price).unwrap_or(0.0);
                    let mut sl = f64::try_from(stop_loss).unwrap_or(0.0);
                    let mut tp = f64::try_from(take_profit).unwrap_or(0.0);

                    // Fixed-SL override for comparison mode
                    if let SlMode::Fixed(fixed_bps) = sl_mode {
                        if entry > 0.0 {
                            let sl_pct = fixed_bps / 10_000.0;
                            let tp_pct = sl_pct * settings.strategy.target_rr;
                            (sl, tp) = match direction {
                                Direction::Long  => (entry * (1.0 - sl_pct), entry * (1.0 + tp_pct)),
                                Direction::Short => (entry * (1.0 + sl_pct), entry * (1.0 - tp_pct)),
                            };
                        }
                    }

                    if entry > 0.0 && sl > 0.0 && tp > 0.0 {
                        // ── Real position sizing via RiskManager ─────────────
                        // Uses the same formula as live:
                        //   size_usd = equity × max_loss_pct / sl_distance_pct
                        //   capped by max_leverage and max_margin_usage_pct
                        let equity_dec = Decimal::try_from(equity).unwrap_or(Decimal::ZERO);
                        let entry_dec = Decimal::try_from(entry).unwrap_or(Decimal::ZERO);
                        let sl_dec = Decimal::try_from(sl).unwrap_or(Decimal::ZERO);
                        // Use config max_leverage as the coin cap (no exchange meta in backtest)
                        let coin_max_lev = settings.risk.leverage.max_leverage;
                        let (size_coins, leverage) =
                            risk_mgr.compute_position_size(equity_dec, entry_dec, sl_dec, coin_max_lev);

                        let size = f64::try_from(size_coins).unwrap_or(0.0);
                        let size_usd = size * entry;

                        // Skip if size is too small (e.g. equity is near zero)
                        if size <= 0.0 || size_usd < 11.0 {
                            continue;
                        }

                        self.replay.record_maker_attempt(true);
                        active = Some(ActiveTrade {
                            direction,
                            entry_price: entry,
                            stop_loss: sl,
                            take_profit: tp,
                            size,
                            size_usd,
                            leverage,
                            entry_ts: now_ms,
                            mae_bps: 0.0,
                            mfe_bps: 0.0,
                            adverse_5s: false,
                            adverse_5s_recorded: false,
                        });
                    }
                }
            }
        }

        Ok(())
    }

    fn load_book_records(path: &Path) -> Result<Vec<BookRecord>> {
        Ok(std::fs::read_to_string(path)?
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect())
    }

    fn load_trade_records(path: &Path) -> Result<Vec<TradeRecord>> {
        Ok(std::fs::read_to_string(path)?
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect())
    }
}

struct ActiveTrade {
    direction: Direction,
    entry_price: f64,
    stop_loss: f64,
    take_profit: f64,
    size: f64,
    size_usd: f64,
    leverage: u32,
    entry_ts: i64,
    mae_bps: f64,
    mfe_bps: f64,
    adverse_5s: bool,
    adverse_5s_recorded: bool,
}
