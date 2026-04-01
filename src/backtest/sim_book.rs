use serde::Serialize;

use crate::market_data::book::{BookLevel, OrderBook};

/// Simulated order book for backtesting.
/// Replays book states from recorded data.
pub struct SimBook {
    book: OrderBook,
    volume_at_price: std::collections::HashMap<u64, f64>, // price_key → cumulative volume
}

impl SimBook {
    pub fn new(coin: String) -> Self {
        Self {
            book: OrderBook::new(coin),
            volume_at_price: std::collections::HashMap::new(),
        }
    }

    /// Apply a recorded book state.
    pub fn apply_snapshot(&mut self, bids: &[BookLevel], asks: &[BookLevel], ts: i64) {
        self.book.apply_snapshot(bids, asks, ts);
    }

    /// Record volume traded at a price level (for fill simulation).
    pub fn record_volume_at_price(&mut self, price: f64, volume: f64) {
        let key = (price * 100.0) as u64;
        *self.volume_at_price.entry(key).or_insert(0.0) += volume;
    }

    /// Get cumulative volume at a price since a given timestamp.
    pub fn volume_traded_at(&self, price: f64) -> f64 {
        let key = (price * 100.0) as u64;
        *self.volume_at_price.get(&key).unwrap_or(&0.0)
    }

    /// Get depth at a price level.
    pub fn depth_at_price(&self, price: f64) -> f64 {
        let key = crate::market_data::book::OrderedFloat(price);
        self.book
            .bids
            .get(&key)
            .or_else(|| self.book.asks.get(&key))
            .copied()
            .unwrap_or(0.0)
    }

    pub fn inner(&self) -> &OrderBook {
        &self.book
    }

    pub fn reset_volume(&mut self) {
        self.volume_at_price.clear();
    }
}
