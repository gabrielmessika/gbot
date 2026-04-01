use std::collections::VecDeque;
use std::path::Path;

use anyhow::Result;
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

/// Replays recorded JSONL sessions through the full MFDP pipeline and
/// produces a BacktestSummary.
pub struct BacktestRunner {
    strategy: MfdpStrategy,
    replay: ReplayEngine,
}

impl BacktestRunner {
    pub fn new(strategy: MfdpStrategy, initial_equity: f64) -> Self {
        Self {
            strategy,
            replay: ReplayEngine::new(initial_equity),
        }
    }

    /// Run a backtest from JSONL files recorded by the Recorder.
    ///
    /// Directory layout:
    /// ```text
    /// data/l2/{coin}/{YYYY-MM-DD}.jsonl
    /// data/trades/{coin}/{YYYY-MM-DD}.jsonl
    /// ```
    pub fn run_from_files(
        &mut self,
        data_dir: &str,
        coins: &[String],
        date: &str,
        settings: &Settings,
    ) -> Result<BacktestSummary> {
        self.run_with_sl_mode(data_dir, coins, date, settings, SlMode::Dynamic)
    }

    /// Phase 7.6 — Run with a specific SL mode (Dynamic or Fixed).
    pub fn run_with_sl_mode(
        &mut self,
        data_dir: &str,
        coins: &[String],
        date: &str,
        settings: &Settings,
        sl_mode: SlMode,
    ) -> Result<BacktestSummary> {
        self.replay = ReplayEngine::new(self.replay.initial_equity());

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
                info!("[BACKTEST] No book data for {} on {} — skipping", coin, date);
                continue;
            }

            let book_records = Self::load_book_records(&book_path)?;
            let trade_records = if trade_path.exists() {
                Self::load_trade_records(&trade_path)?
            } else {
                Vec::new()
            };

            info!(
                "[BACKTEST] {} on {}: {} book snapshots, {} trades (SL={:?})",
                coin, date, book_records.len(), trade_records.len(), sl_mode
            );

            self.replay_coin(coin, &book_records, &trade_records, settings, sl_mode)?;
        }

        Ok(self.replay.summary())
    }

    /// Phase 7.6 — Run dynamic vs fixed SL comparison and return both summaries.
    ///
    /// `fixed_sl_bps`: fixed SL distance in bps for the comparison run.
    pub fn run_comparison(
        &mut self,
        data_dir: &str,
        coins: &[String],
        date: &str,
        settings: &Settings,
        fixed_sl_bps: f64,
    ) -> Result<ComparisonResult> {
        let dynamic = self.run_with_sl_mode(data_dir, coins, date, settings, SlMode::Dynamic)?;
        let fixed = self.run_with_sl_mode(data_dir, coins, date, settings, SlMode::Fixed(fixed_sl_bps))?;

        info!(
            "[BACKTEST] Comparison — Dynamic: P&L={:.2} WR={:.1}% MAE={:.2}bps | Fixed {}bps: P&L={:.2} WR={:.1}% MAE={:.2}bps",
            dynamic.total_pnl_net, dynamic.hit_rate, dynamic.avg_mae_bps,
            fixed_sl_bps,
            fixed.total_pnl_net, fixed.hit_rate, fixed.avg_mae_bps,
        );

        Ok(ComparisonResult {
            pnl_delta: dynamic.total_pnl_net - fixed.total_pnl_net,
            hit_rate_delta: dynamic.hit_rate - fixed.hit_rate,
            avg_mae_delta: dynamic.avg_mae_bps - fixed.avg_mae_bps,
            dynamic_sl: dynamic,
            fixed_sl: fixed,
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

        // Interleave book snapshots and trade records by timestamp
        while book_idx < book_records.len() || trade_idx < trade_records.len() {
            let next_book_ts = book_records.get(book_idx).map(|r| r.timestamp).unwrap_or(i64::MAX);
            let next_trade_ts = trade_records.get(trade_idx).map(|r| r.timestamp).unwrap_or(i64::MAX);

            let now_ms = if next_book_ts <= next_trade_ts {
                let rec = &book_records[book_idx];
                book_idx += 1;
                if rec.best_bid > 0.0 && rec.best_ask > 0.0 {
                    book.apply_snapshot(
                        &[BookLevel { price: rec.best_bid, size: 1.0 }],
                        &[BookLevel { price: rec.best_ask, size: 1.0 }],
                        rec.timestamp,
                    );
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

            // ── Check exit conditions + update MAE/MFE for active trade ──
            if let Some(ref mut trade) = active {
                let mid = book.mid().unwrap_or(trade.entry_price);

                // Update MAE/MFE (Phase 7.6)
                if trade.entry_price > 0.0 {
                    let signed_move_bps = match trade.direction {
                        Direction::Long => (mid - trade.entry_price) / trade.entry_price * 10_000.0,
                        Direction::Short => (trade.entry_price - mid) / trade.entry_price * 10_000.0,
                    };
                    if signed_move_bps < 0.0 {
                        trade.mae_bps = trade.mae_bps.max(-signed_move_bps);
                    } else {
                        trade.mfe_bps = trade.mfe_bps.max(signed_move_bps);
                    }

                    // Adverse selection: record if mid moved against direction within 5s
                    let elapsed_s = (now_ms - trade.entry_ts) as f64 / 1000.0;
                    if !trade.adverse_5s_recorded && elapsed_s >= 5.0 {
                        trade.adverse_5s = signed_move_bps < 0.0;
                        trade.adverse_5s_recorded = true;
                    }
                }

                let exit = match trade.direction {
                    Direction::Long => {
                        if mid <= trade.stop_loss {
                            Some(("SL_HIT", trade.stop_loss))
                        } else if mid >= trade.take_profit {
                            Some(("TP_HIT", trade.take_profit))
                        } else if (now_ms - trade.entry_ts) / 1000 > settings.execution.max_hold_s as i64 {
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
                        } else if (now_ms - trade.entry_ts) / 1000 > settings.execution.max_hold_s as i64 {
                            Some(("TIMEOUT", mid))
                        } else {
                            None
                        }
                    }
                };

                if let Some((reason, exit_price)) = exit {
                    let pnl_gross = match trade.direction {
                        Direction::Long => (exit_price - trade.entry_price) * trade.size,
                        Direction::Short => (trade.entry_price - exit_price) * trade.size,
                    };
                    let notional = trade.entry_price * trade.size;
                    let fee_entry = fee_model.maker_fee(notional);
                    let fee_exit = if reason == "SL_HIT" {
                        fee_model.taker_fee(notional)
                    } else {
                        fee_model.maker_fee(notional)
                    };
                    let mae = trade.mae_bps;
                    let mfe = trade.mfe_bps;
                    let adv = trade.adverse_5s;
                    self.replay.record_trade(BacktestTrade {
                        coin: coin.to_string(),
                        direction: format!("{:?}", trade.direction),
                        entry_price: trade.entry_price,
                        exit_price,
                        size: trade.size,
                        entry_maker: true,
                        exit_maker: reason != "SL_HIT",
                        pnl_gross,
                        fee_entry,
                        fee_exit,
                        pnl_net: pnl_gross - fee_entry - fee_exit,
                        hold_duration_s: (now_ms - trade.entry_ts) as f64 / 1000.0,
                        exit_reason: reason.to_string(),
                        entry_ts: trade.entry_ts,
                        exit_ts: now_ms,
                        mae_bps: mae,
                        mfe_bps: mfe,
                        adverse_5s: adv,
                    });
                    active = None;
                }
            }

            // ── Strategy evaluation — only when no active trade and book ready ──
            if active.is_none() && book.snapshot_loaded && tape.len() >= 10 {
                let book_feats = book_features::compute_book_features(&book, &mut spread_avg);
                let flow_feats = flow_features::compute_flow_features(&tape, now_ms, 0.0);
                let features = CoinFeatures { book: book_feats, flow: flow_feats, timestamp: now_ms };

                let regime = regime_engine::classify(&features, false, false, &settings.regime, None);
                let (intent, _dir_score, _queue_score) = self.strategy.evaluate(coin, &features, regime, &book);

                if let Intent::PlacePassiveEntry {
                    direction,
                    price: entry_price,
                    stop_loss,
                    take_profit,
                    ..
                } = intent
                {
                    let entry = entry_price.to_string().parse::<f64>().unwrap_or(0.0);
                    let mut sl = stop_loss.to_string().parse::<f64>().unwrap_or(0.0);
                    let mut tp = take_profit.to_string().parse::<f64>().unwrap_or(0.0);

                    // Phase 7.6: override SL/TP for fixed-SL comparison mode
                    if let SlMode::Fixed(fixed_bps) = sl_mode {
                        if entry > 0.0 {
                            let sl_pct = fixed_bps / 10_000.0;
                            let tp_pct = sl_pct * settings.strategy.target_rr;
                            (sl, tp) = match direction {
                                Direction::Long => (entry * (1.0 - sl_pct), entry * (1.0 + tp_pct)),
                                Direction::Short => (entry * (1.0 + sl_pct), entry * (1.0 - tp_pct)),
                            };
                        }
                    }

                    if entry > 0.0 && sl > 0.0 && tp > 0.0 {
                        self.replay.record_maker_attempt(true);
                        active = Some(ActiveTrade {
                            direction,
                            entry_price: entry,
                            stop_loss: sl,
                            take_profit: tp,
                            size: 0.001, // Simplified: fixed size — real sizing needs equity context
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
    entry_ts: i64,
    // Phase 7.6: MAE/MFE tracking
    mae_bps: f64,
    mfe_bps: f64,
    adverse_5s: bool,
    adverse_5s_recorded: bool,
}
