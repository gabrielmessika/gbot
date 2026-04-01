use std::collections::HashMap;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::execution::position_manager::OpenPosition;

/// Internal truth of the portfolio state. Reconciled periodically with the exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioState {
    pub positions: HashMap<String, OpenPosition>,
    pub realized_pnl: Decimal,
    pub funding_cumulated: Decimal,
    pub fees_cumulated: Decimal,
    pub margin_used: Decimal,
    pub peak_equity: Decimal,
    pub daily_start_balance: Decimal,
    pub daily_reset_ts: i64,
}

impl PortfolioState {
    pub fn new(initial_balance: Decimal) -> Self {
        Self {
            positions: HashMap::new(),
            realized_pnl: Decimal::ZERO,
            funding_cumulated: Decimal::ZERO,
            fees_cumulated: Decimal::ZERO,
            margin_used: Decimal::ZERO,
            peak_equity: initial_balance,
            daily_start_balance: initial_balance,
            daily_reset_ts: chrono::Utc::now().timestamp_millis(),
        }
    }

    /// Record a realized trade PnL.
    pub fn record_pnl(&mut self, pnl: Decimal) {
        self.realized_pnl += pnl;
    }

    /// Record fees.
    pub fn record_fee(&mut self, fee: Decimal) {
        self.fees_cumulated += fee;
    }

    /// Record funding payment.
    pub fn record_funding(&mut self, amount: Decimal) {
        self.funding_cumulated += amount;
    }

    /// Net PnL = realized - fees + funding.
    pub fn net_pnl(&self) -> Decimal {
        self.realized_pnl - self.fees_cumulated + self.funding_cumulated
    }

    /// Current equity = daily_start_balance + net_pnl + unrealized_pnl.
    pub fn equity(&self, unrealized_pnl: Decimal) -> Decimal {
        self.daily_start_balance + self.net_pnl() + unrealized_pnl
    }
}
