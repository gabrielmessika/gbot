use serde::{Deserialize, Serialize};

use crate::market_data::book::OrderBook;

/// Instantaneous features computed from a book snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BookFeatures {
    // Spread
    pub spread_bps: f64,
    pub spread_vs_avg: f64,

    // Imbalance (multiple granularities)
    pub imbalance_top1: f64,
    pub imbalance_top3: f64,
    pub imbalance_top5: f64,
    pub imbalance_weighted: f64,

    // Depth
    pub bid_depth_10bps: f64,
    pub ask_depth_10bps: f64,
    pub depth_ratio: f64,
    pub book_slope_bid: f64,
    pub book_slope_ask: f64,

    // Micro-price and VAMP
    pub micro_price: f64,
    pub micro_price_vs_mid_bps: f64,
    pub vamp: f64,
    pub vamp_signal_bps: f64,
}

/// Rolling average for spread normalization.
pub struct SpreadAverage {
    values: Vec<f64>,
    max_size: usize,
}

impl SpreadAverage {
    pub fn new(max_size: usize) -> Self {
        Self {
            values: Vec::with_capacity(max_size),
            max_size,
        }
    }

    pub fn push(&mut self, value: f64) {
        if self.values.len() >= self.max_size {
            self.values.remove(0);
        }
        self.values.push(value);
    }

    pub fn average(&self) -> f64 {
        if self.values.is_empty() {
            return 1.0;
        }
        self.values.iter().sum::<f64>() / self.values.len() as f64
    }
}

/// Compute instantaneous BookFeatures from a book snapshot.
pub fn compute_book_features(book: &OrderBook, spread_avg: &mut SpreadAverage) -> BookFeatures {
    let mut features = BookFeatures::default();

    let mid = match book.mid() {
        Some(m) => m,
        None => return features,
    };

    // ── Spread ──
    features.spread_bps = book.spread_bps().unwrap_or(0.0);
    spread_avg.push(features.spread_bps);
    let avg = spread_avg.average();
    features.spread_vs_avg = if avg > 0.0 {
        features.spread_bps / avg
    } else {
        1.0
    };

    // ── Imbalance ──
    let top_bids = book.top_bids(5);
    let top_asks = book.top_asks(5);

    features.imbalance_top1 = compute_imbalance(&top_bids, &top_asks, 1);
    features.imbalance_top3 = compute_imbalance(&top_bids, &top_asks, 3);
    features.imbalance_top5 = compute_imbalance(&top_bids, &top_asks, 5);
    features.imbalance_weighted = compute_weighted_imbalance(&top_bids, &top_asks, mid);

    // ── Depth ──
    features.bid_depth_10bps = book.bid_depth_within_bps(10.0);
    features.ask_depth_10bps = book.ask_depth_within_bps(10.0);
    features.depth_ratio = if features.ask_depth_10bps > 0.0 {
        features.bid_depth_10bps / features.ask_depth_10bps
    } else {
        1.0
    };
    features.book_slope_bid = compute_slope(&top_bids);
    features.book_slope_ask = compute_slope(&top_asks);

    // ── Micro-price ──
    // micro_price = ask × (Q_bid / (Q_bid + Q_ask)) + bid × (Q_ask / (Q_bid + Q_ask))
    if let (Some(bid), Some(ask), Some(bid_sz), Some(ask_sz)) = (
        book.best_bid(),
        book.best_ask(),
        book.best_bid_size(),
        book.best_ask_size(),
    ) {
        let total = bid_sz + ask_sz;
        if total > 0.0 {
            features.micro_price = ask * (bid_sz / total) + bid * (ask_sz / total);
            features.micro_price_vs_mid_bps = (features.micro_price - mid) / mid * 10_000.0;
        }
    }

    // ── VAMP (Volume-Adjusted Mid Price) ──
    features.vamp = compute_vamp(&top_bids, &top_asks);
    if mid > 0.0 {
        features.vamp_signal_bps = (features.vamp - mid) / mid * 10_000.0;
    }

    features
}

/// Simple imbalance for top N levels: (bid_qty - ask_qty) / (bid_qty + ask_qty).
fn compute_imbalance(
    bids: &[crate::market_data::book::BookLevel],
    asks: &[crate::market_data::book::BookLevel],
    n: usize,
) -> f64 {
    let bid_qty: f64 = bids.iter().take(n).map(|l| l.size).sum();
    let ask_qty: f64 = asks.iter().take(n).map(|l| l.size).sum();
    let total = bid_qty + ask_qty;
    if total == 0.0 {
        return 0.0;
    }
    (bid_qty - ask_qty) / total
}

/// Weighted imbalance: levels closer to mid get more weight.
fn compute_weighted_imbalance(
    bids: &[crate::market_data::book::BookLevel],
    asks: &[crate::market_data::book::BookLevel],
    mid: f64,
) -> f64 {
    if mid == 0.0 {
        return 0.0;
    }

    let mut weighted_bid = 0.0;
    let mut weighted_ask = 0.0;

    for level in bids {
        let distance = ((mid - level.price) / mid).abs();
        let weight = 1.0 / (1.0 + distance * 1000.0); // closer = more weight
        weighted_bid += level.size * weight;
    }
    for level in asks {
        let distance = ((level.price - mid) / mid).abs();
        let weight = 1.0 / (1.0 + distance * 1000.0);
        weighted_ask += level.size * weight;
    }

    let total = weighted_bid + weighted_ask;
    if total == 0.0 {
        return 0.0;
    }
    (weighted_bid - weighted_ask) / total
}

/// Book slope: how quickly liquidity diminishes away from top of book.
fn compute_slope(levels: &[crate::market_data::book::BookLevel]) -> f64 {
    if levels.len() < 2 {
        return 0.0;
    }
    let first_size = levels[0].size;
    if first_size == 0.0 {
        return 0.0;
    }
    // Average ratio of subsequent levels to first level
    let ratios: Vec<f64> = levels
        .iter()
        .skip(1)
        .map(|l| l.size / first_size)
        .collect();
    ratios.iter().sum::<f64>() / ratios.len() as f64
}

/// VAMP = Σ(P_bid_i × Q_ask_i + P_ask_i × Q_bid_i) / Σ(Q_ask_i + Q_bid_i)
fn compute_vamp(
    bids: &[crate::market_data::book::BookLevel],
    asks: &[crate::market_data::book::BookLevel],
) -> f64 {
    let n = bids.len().min(asks.len());
    if n == 0 {
        return 0.0;
    }

    let mut numerator = 0.0;
    let mut denominator = 0.0;

    for i in 0..n {
        numerator += bids[i].price * asks[i].size + asks[i].price * bids[i].size;
        denominator += asks[i].size + bids[i].size;
    }

    if denominator == 0.0 {
        return 0.0;
    }
    numerator / denominator
}
