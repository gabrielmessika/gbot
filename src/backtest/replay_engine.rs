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
    pub adverse_selection_rate: f64,
    pub fee_drag_pct: f64,
    pub trades: Vec<BacktestTrade>,
}

/// Replay engine for tick-by-tick backtesting.
/// Phase 1: placeholder structure. Full implementation requires recorded data.
pub struct ReplayEngine {
    #[allow(dead_code)]
    fee_model: FeeModel,
    trades: Vec<BacktestTrade>,
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
            equity: initial_equity,
            peak_equity: initial_equity,
            max_drawdown: 0.0,
            maker_fills: 0,
            maker_attempts: 0,
        }
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
            adverse_selection_rate: 0.0, // Needs post-analysis
            fee_drag_pct: fee_drag,
            trades: self.trades.clone(),
        }
    }
}
