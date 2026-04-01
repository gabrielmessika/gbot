use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A single price level in the order book.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookLevel {
    pub price: f64,
    pub size: f64,
}

/// Local order book for a single coin.
/// Maintained from WS l2Book snapshots/deltas.
#[derive(Debug, Clone)]
pub struct OrderBook {
    pub coin: String,
    /// Bids: price → size (descending key order via BTreeMap reverse iteration).
    pub bids: BTreeMap<OrderedFloat, f64>,
    /// Asks: price → size (ascending key order).
    pub asks: BTreeMap<OrderedFloat, f64>,
    pub last_update_ts: i64,
    pub snapshot_loaded: bool,
}

/// Wrapper for f64 that implements Ord for use in BTreeMap.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OrderedFloat(pub f64);

impl Eq for OrderedFloat {}

impl PartialOrd for OrderedFloat {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedFloat {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.partial_cmp(&other.0).unwrap_or(std::cmp::Ordering::Equal)
    }
}

impl OrderBook {
    pub fn new(coin: String) -> Self {
        Self {
            coin,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            last_update_ts: 0,
            snapshot_loaded: false,
        }
    }

    /// Apply a full snapshot (replaces all levels).
    pub fn apply_snapshot(&mut self, bid_levels: &[BookLevel], ask_levels: &[BookLevel], ts: i64) {
        self.bids.clear();
        self.asks.clear();

        for level in bid_levels {
            if level.size > 0.0 {
                self.bids.insert(OrderedFloat(level.price), level.size);
            }
        }
        for level in ask_levels {
            if level.size > 0.0 {
                self.asks.insert(OrderedFloat(level.price), level.size);
            }
        }

        self.last_update_ts = ts;
        self.snapshot_loaded = true;
    }

    /// Apply a delta update (insert/update/remove levels).
    pub fn apply_delta(&mut self, bid_levels: &[BookLevel], ask_levels: &[BookLevel], ts: i64) {
        for level in bid_levels {
            let key = OrderedFloat(level.price);
            if level.size <= 0.0 {
                self.bids.remove(&key);
            } else {
                self.bids.insert(key, level.size);
            }
        }
        for level in ask_levels {
            let key = OrderedFloat(level.price);
            if level.size <= 0.0 {
                self.asks.remove(&key);
            } else {
                self.asks.insert(key, level.size);
            }
        }

        self.last_update_ts = ts;
    }

    /// Best bid price (highest bid).
    pub fn best_bid(&self) -> Option<f64> {
        self.bids.keys().next_back().map(|k| k.0)
    }

    /// Best ask price (lowest ask).
    pub fn best_ask(&self) -> Option<f64> {
        self.asks.keys().next().map(|k| k.0)
    }

    /// Best bid size.
    pub fn best_bid_size(&self) -> Option<f64> {
        self.bids.iter().next_back().map(|(_, &s)| s)
    }

    /// Best ask size.
    pub fn best_ask_size(&self) -> Option<f64> {
        self.asks.iter().next().map(|(_, &s)| s)
    }

    /// Mid price: (best_bid + best_ask) / 2.
    pub fn mid(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => Some((bid + ask) / 2.0),
            _ => None,
        }
    }

    /// Spread in basis points.
    pub fn spread_bps(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) if bid > 0.0 => {
                let mid = (bid + ask) / 2.0;
                Some((ask - bid) / mid * 10_000.0)
            }
            _ => None,
        }
    }

    /// Top N bid levels (descending by price).
    pub fn top_bids(&self, n: usize) -> Vec<BookLevel> {
        self.bids
            .iter()
            .rev()
            .take(n)
            .map(|(k, &s)| BookLevel {
                price: k.0,
                size: s,
            })
            .collect()
    }

    /// Top N ask levels (ascending by price).
    pub fn top_asks(&self, n: usize) -> Vec<BookLevel> {
        self.asks
            .iter()
            .take(n)
            .map(|(k, &s)| BookLevel {
                price: k.0,
                size: s,
            })
            .collect()
    }

    /// Cumulative bid depth within `bps` basis points of mid.
    pub fn bid_depth_within_bps(&self, bps: f64) -> f64 {
        let mid = match self.mid() {
            Some(m) => m,
            None => return 0.0,
        };
        let threshold = mid * (1.0 - bps / 10_000.0);

        self.bids
            .iter()
            .rev()
            .take_while(|(k, _)| k.0 >= threshold)
            .map(|(k, &s)| k.0 * s) // notional
            .sum()
    }

    /// Cumulative ask depth within `bps` basis points of mid.
    pub fn ask_depth_within_bps(&self, bps: f64) -> f64 {
        let mid = match self.mid() {
            Some(m) => m,
            None => return 0.0,
        };
        let threshold = mid * (1.0 + bps / 10_000.0);

        self.asks
            .iter()
            .take_while(|(k, _)| k.0 <= threshold)
            .map(|(k, &s)| k.0 * s) // notional
            .sum()
    }
}
