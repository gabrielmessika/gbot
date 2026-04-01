use std::collections::VecDeque;
use std::path::Path;

use anyhow::Result;
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
    /// ```
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
                "[BACKTEST] {} on {}: {} book snapshots, {} trades",
                coin, date, book_records.len(), trade_records.len()
            );

            self.replay_coin(coin, &book_records, &trade_records, settings)?;
        }

        Ok(self.replay.summary())
    }

    fn replay_coin(
        &mut self,
        coin: &str,
        book_records: &[BookRecord],
        trade_records: &[TradeRecord],
        settings: &Settings,
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
                // Inject synthetic top-of-book snapshot
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

            // ── Check exit conditions for active trade ──
            if let Some(ref trade) = active {
                let mid = book.mid().unwrap_or(trade.entry_price);
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
                let intent = self.strategy.evaluate(coin, &features, regime, &book);

                if let Intent::PlacePassiveEntry {
                    direction,
                    price: entry_price,
                    stop_loss,
                    take_profit,
                    ..
                } = intent
                {
                    let entry = entry_price.to_string().parse::<f64>().unwrap_or(0.0);
                    let sl = stop_loss.to_string().parse::<f64>().unwrap_or(0.0);
                    let tp = take_profit.to_string().parse::<f64>().unwrap_or(0.0);
                    if entry > 0.0 {
                        self.replay.record_maker_attempt(true);
                        active = Some(ActiveTrade {
                            direction,
                            entry_price: entry,
                            stop_loss: sl,
                            take_profit: tp,
                            // Simplified: fixed 0.001 coin — real sizing needs equity context
                            size: 0.001,
                            entry_ts: now_ms,
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
}
