use serde::{Deserialize, Serialize};

use crate::backtest::sim_execution::FeeModel;

/// A single backtest trade result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestTrade {
    pub coin: String,
    pub direction: String,
    pub entry_price: f64,
    pub exit_price: f64,
    pub size: f64,
    pub entry_maker: bool,
    pub exit_maker: bool,
    pub pnl_gross: f64,
    pub fee_entry: f64,
    pub fee_exit: f64,
    pub pnl_net: f64,
    pub hold_duration_s: f64,
    pub exit_reason: String,
    pub entry_ts: i64,
    pub exit_ts: i64,
    // ── Phase 7.6: MAE/MFE ──────────────────────────────────────────────────
    /// Max Adverse Excursion in bps — worst unrealised loss during the trade.
    pub mae_bps: f64,
    /// Max Favourable Excursion in bps — best unrealised gain during the trade.
    pub mfe_bps: f64,
    /// True if mid moved against direction within 5s of entry (adverse selection proxy).
    pub adverse_5s: bool,
}

/// Backtest summary metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestSummary {
    pub total_trades: usize,
    pub winners: usize,
    pub losers: usize,
    pub hit_rate: f64,
    pub total_pnl_net: f64,
    pub avg_pnl_net: f64,
    pub avg_winner: f64,
    pub avg_loser: f64,
    pub max_drawdown_pct: f64,
    pub maker_fill_rate: f64,
    /// % of fills where mid moved against direction within 5s (adverse selection proxy).
    pub adverse_selection_rate: f64,
    pub fee_drag_pct: f64,
    // ── Phase 7.6 additions ──
    /// Average MAE across all trades (bps).
    pub avg_mae_bps: f64,
    /// Average MFE across all trades (bps).
    pub avg_mfe_bps: f64,
    /// Ratio avg_mae / SL_distance — > 1.0 means SL is tighter than typical noise.
    pub mae_to_sl_ratio: f64,
    pub trades: Vec<BacktestTrade>,
}

/// Replay engine for tick-by-tick backtesting.
pub struct ReplayEngine {
    #[allow(dead_code)]
    fee_model: FeeModel,
    trades: Vec<BacktestTrade>,
    initial_equity: f64,
    equity: f64,
    peak_equity: f64,
    max_drawdown: f64,
    maker_fills: usize,
    maker_attempts: usize,
}

impl ReplayEngine {
    pub fn new(initial_equity: f64) -> Self {
        Self {
            fee_model: FeeModel::hyperliquid(),
            trades: Vec::new(),
            initial_equity,
            equity: initial_equity,
            peak_equity: initial_equity,
            max_drawdown: 0.0,
            maker_fills: 0,
            maker_attempts: 0,
        }
    }

    pub fn initial_equity(&self) -> f64 {
        self.initial_equity
    }

    /// Record a completed trade.
    pub fn record_trade(&mut self, trade: BacktestTrade) {
        self.equity += trade.pnl_net;
        if self.equity > self.peak_equity {
            self.peak_equity = self.equity;
        }
        let dd = (self.peak_equity - self.equity) / self.peak_equity * 100.0;
        if dd > self.max_drawdown {
            self.max_drawdown = dd;
        }
        self.trades.push(trade);
    }

    /// Record a maker fill attempt.
    pub fn record_maker_attempt(&mut self, filled: bool) {
        self.maker_attempts += 1;
        if filled {
            self.maker_fills += 1;
        }
    }

    /// Generate a summary of the backtest.
    pub fn summary(&self) -> BacktestSummary {
        let total = self.trades.len();
        let winners: Vec<_> = self.trades.iter().filter(|t| t.pnl_net > 0.0).collect();
        let losers: Vec<_> = self.trades.iter().filter(|t| t.pnl_net <= 0.0).collect();

        let total_pnl: f64 = self.trades.iter().map(|t| t.pnl_net).sum();
        let total_gross: f64 = self.trades.iter().map(|t| t.pnl_gross).sum();
        let total_fees: f64 = self.trades.iter().map(|t| t.fee_entry + t.fee_exit).sum();

        let avg_winner = if !winners.is_empty() {
            winners.iter().map(|t| t.pnl_net).sum::<f64>() / winners.len() as f64
        } else {
            0.0
        };
        let avg_loser = if !losers.is_empty() {
            losers.iter().map(|t| t.pnl_net).sum::<f64>() / losers.len() as f64
        } else {
            0.0
        };

        let maker_fill_rate = if self.maker_attempts > 0 {
            self.maker_fills as f64 / self.maker_attempts as f64
        } else {
            0.0
        };

        let fee_drag = if total_gross.abs() > 0.0 {
            total_fees / total_gross.abs() * 100.0
        } else {
            0.0
        };

        // Phase 7.6: MAE/MFE + adverse selection
        let avg_mae_bps = if total > 0 {
            self.trades.iter().map(|t| t.mae_bps).sum::<f64>() / total as f64
        } else {
            0.0
        };
        let avg_mfe_bps = if total > 0 {
            self.trades.iter().map(|t| t.mfe_bps).sum::<f64>() / total as f64
        } else {
            0.0
        };
        // MAE/SL ratio: compare average MAE to the average SL distance
        // SL distance in bps ≈ |entry - stop_loss| / entry * 10_000
        let avg_sl_bps = if total > 0 {
            self.trades.iter().map(|t| {
                let sl_dist = (t.exit_price - t.entry_price).abs();
                if t.entry_price > 0.0 { sl_dist / t.entry_price * 10_000.0 } else { 0.0 }
            }).sum::<f64>() / total as f64
        } else {
            0.0
        };
        let mae_to_sl_ratio = if avg_sl_bps > 0.0 { avg_mae_bps / avg_sl_bps } else { 0.0 };

        let adverse_5s_count = self.trades.iter().filter(|t| t.adverse_5s).count();
        let adverse_selection_rate = if total > 0 {
            adverse_5s_count as f64 / total as f64 * 100.0
        } else {
            0.0
        };

        BacktestSummary {
            total_trades: total,
            winners: winners.len(),
            losers: losers.len(),
            hit_rate: if total > 0 {
                winners.len() as f64 / total as f64 * 100.0
            } else {
                0.0
            },
            total_pnl_net: total_pnl,
            avg_pnl_net: if total > 0 {
                total_pnl / total as f64
            } else {
                0.0
            },
            avg_winner,
            avg_loser,
            max_drawdown_pct: self.max_drawdown,
            maker_fill_rate: maker_fill_rate * 100.0,
            adverse_selection_rate,
            fee_drag_pct: fee_drag,
            avg_mae_bps,
            avg_mfe_bps,
            mae_to_sl_ratio,
            trades: self.trades.clone(),
        }
    }
}
