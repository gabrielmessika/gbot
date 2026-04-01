use serde::{Deserialize, Serialize};

use crate::backtest::sim_book::SimBook;
use crate::features::flow_features::FlowFeatures;

/// Simulated fill result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimFill {
    pub filled: bool,
    pub fill_price: f64,
    pub slippage_bps: f64,
}

/// Determine if a passive order would have been filled in simulation.
pub fn should_fill_passive(
    is_buy: bool,
    order_price: f64,
    sim_book: &SimBook,
    flow: &FlowFeatures,
) -> SimFill {
    let book = sim_book.inner();

    // 1. The price must have been reached
    let price_reached = if is_buy {
        book.best_ask().map(|ask| ask <= order_price).unwrap_or(false)
    } else {
        book.best_bid().map(|bid| bid >= order_price).unwrap_or(false)
    };

    if !price_reached {
        return SimFill {
            filled: false,
            fill_price: order_price,
            slippage_bps: 0.0,
        };
    }

    // 2. Queue position (simplified: assume we're in the middle)
    let vol_traded = sim_book.volume_traded_at(order_price);
    let vol_queued = sim_book.depth_at_price(order_price);
    let fill_prob = if vol_queued > 0.0 {
        (vol_traded / (vol_queued * 0.5)).min(1.0)
    } else {
        // Price level empty = likely filled
        1.0
    };

    // 3. Winner's curse adjustment
    let adverse_adjust = 1.0 - flow.fill_toxicity_5s * 0.3;
    let final_prob = fill_prob * adverse_adjust;

    let filled = rand::random::<f64>() < final_prob;

    SimFill {
        filled,
        fill_price: order_price,
        slippage_bps: 0.0, // Maker = no slippage
    }
}

/// Simulate a taker fill with slippage.
pub fn simulate_taker_fill(
    is_buy: bool,
    size: f64,
    sim_book: &SimBook,
) -> SimFill {
    let book = sim_book.inner();
    let mid = book.mid().unwrap_or(0.0);

    // Walk the book to calculate average fill price
    let levels = if is_buy {
        book.top_asks(20)
    } else {
        book.top_bids(20)
    };

    let mut remaining = size;
    let mut total_cost = 0.0;

    for level in &levels {
        let fill_at_level = remaining.min(level.size);
        total_cost += fill_at_level * level.price;
        remaining -= fill_at_level;
        if remaining <= 0.0 {
            break;
        }
    }

    let filled_size = size - remaining;
    let avg_price = if filled_size > 0.0 {
        total_cost / filled_size
    } else {
        mid
    };

    let slippage_bps = if mid > 0.0 {
        ((avg_price - mid).abs() / mid) * 10_000.0
    } else {
        0.0
    };

    SimFill {
        filled: filled_size > 0.0,
        fill_price: avg_price,
        slippage_bps,
    }
}

/// Fee calculation for backtesting.
pub struct FeeModel {
    pub maker_rate: f64,  // 0.00015 = 1.5 bps
    pub taker_rate: f64,  // 0.00045 = 4.5 bps
}

impl FeeModel {
    pub fn hyperliquid() -> Self {
        Self {
            maker_rate: 0.00015,
            taker_rate: 0.00045,
        }
    }

    pub fn maker_fee(&self, notional: f64) -> f64 {
        notional * self.maker_rate
    }

    pub fn taker_fee(&self, notional: f64) -> f64 {
        notional * self.taker_rate
    }

    /// Round-trip cost in bps for a given entry/exit mode.
    pub fn round_trip_bps(&self, entry_maker: bool, exit_maker: bool) -> f64 {
        let entry_fee = if entry_maker {
            self.maker_rate
        } else {
            self.taker_rate
        };
        let exit_fee = if exit_maker {
            self.maker_rate
        } else {
            self.taker_rate
        };
        (entry_fee + exit_fee) * 10_000.0
    }
}
