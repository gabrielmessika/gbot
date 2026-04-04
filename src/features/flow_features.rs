use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::market_data::book_manager::TapeEntry;

/// Temporal features computed over rolling windows from the trade tape.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FlowFeatures {
    // OFI — kept for compatibility / observability but no longer used in dir_score
    pub ofi_1s: f64,
    pub ofi_3s: f64,
    pub ofi_10s: f64,
    pub ofi_30s: f64,

    // Price momentum — primary directional signal (empirically: corr ~0.35 vs 0.06 for OFI)
    pub price_return_5s: f64,   // (price_now - price_5s_ago) / price_5s_ago × 10_000 (bps)
    pub price_return_10s: f64,  // same over 10s
    pub price_return_30s: f64,  // used for trending regime filter

    // Trade aggression
    pub trade_intensity: f64,       // trades/sec over 10s window
    pub avg_trade_size: f64,
    pub large_trade_ratio: f64,     // % trades > 2× avg size
    pub aggression_persistence: f64, // signed: buy dominance (+) vs sell dominance (-)

    // Realized volatility
    pub realized_vol_3s: f64,
    pub realized_vol_10s: f64,
    pub realized_vol_30s: f64,
    pub vol_ratio: f64,             // realized_vol_3s / realized_vol_30s

    // Toxicity proxy
    pub fill_toxicity_5s: f64,
    pub toxicity_proxy_instant: f64,

    // Refill speed
    pub refill_speed: f64,

    // Cancel/add ratio (spoofing proxy)
    pub cancel_add_ratio: f64,

    // EVO-2: Volatility compression (squeeze detection)
    /// realized_vol_3s / realized_vol_30s — same as vol_ratio, aliased for clarity.
    pub vol_compression: f64,
    /// True if vol_compression has been increasing over the last N ticks (breakout signal).
    pub vol_expanding: bool,

    // Maturity indicators
    pub trade_count_10s: usize,
}

impl FlowFeatures {
    /// Whether the flow features have enough data to be meaningful for trading decisions.
    /// Returns false if there are too few trades or no volatility data.
    pub fn is_mature(&self) -> bool {
        self.trade_count_10s >= 5 && self.realized_vol_30s > 0.0
    }
}

/// Compute flow features from the trade tape.
///
/// `cancel_add_ratio` is supplied by the caller from `BookManager::get_cancel_add_ratio()`.
/// `refill_speed` requires per-level order tracking not yet available — left at 0.0.
pub fn compute_flow_features(tape: &VecDeque<TapeEntry>, now_ms: i64, cancel_add_ratio: f64) -> FlowFeatures {
    let mut features = FlowFeatures::default();

    if tape.is_empty() {
        return features;
    }

    // ── OFI (Order Flow Imbalance) ──
    features.ofi_1s = compute_ofi(tape, now_ms, 1_000);
    features.ofi_3s = compute_ofi(tape, now_ms, 3_000);
    features.ofi_10s = compute_ofi(tape, now_ms, 10_000);
    features.ofi_30s = compute_ofi(tape, now_ms, 30_000);

    // ── Trade aggression ──
    let window_10s: Vec<&TapeEntry> = tape
        .iter()
        .filter(|t| now_ms - t.timestamp <= 10_000)
        .collect();

    if !window_10s.is_empty() {
        let elapsed_s = (now_ms - window_10s.first().unwrap().timestamp).max(1) as f64 / 1000.0;
        features.trade_intensity = window_10s.len() as f64 / elapsed_s;

        let total_size: f64 = window_10s.iter().map(|t| t.size).sum();
        features.avg_trade_size = total_size / window_10s.len() as f64;

        let large_threshold = features.avg_trade_size * 2.0;
        let large_count = window_10s.iter().filter(|t| t.size > large_threshold).count();
        features.large_trade_ratio = large_count as f64 / window_10s.len() as f64;
    }

    features.trade_count_10s = window_10s.len();

    // Aggression persistence: signed ratio of buy vs sell dominance in last 10 trades.
    // Range: [-1, +1]. Positive = buy dominance, negative = sell dominance.
    let last_n: Vec<&TapeEntry> = tape.iter().rev().take(10).collect();
    if !last_n.is_empty() {
        let buy_count = last_n.iter().filter(|t| t.is_buy).count() as f64;
        let total = last_n.len() as f64;
        features.aggression_persistence = (2.0 * buy_count - total) / total;
    }

    // ── Price momentum — primary directional signal ──
    // Computed from last traded price vs price N seconds ago in the tape.
    // Empirically: corr(pr5s, ret30s) = +0.354 vs corr(ofi_10s, ret30s) = +0.058.
    features.price_return_5s = compute_price_return(tape, now_ms, 5_000);
    features.price_return_10s = compute_price_return(tape, now_ms, 10_000);
    features.price_return_30s = compute_price_return(tape, now_ms, 30_000);

    // ── Realized volatility ──
    features.realized_vol_3s = compute_realized_vol(tape, now_ms, 3_000);
    features.realized_vol_10s = compute_realized_vol(tape, now_ms, 10_000);
    features.realized_vol_30s = compute_realized_vol(tape, now_ms, 30_000);
    features.vol_ratio = if features.realized_vol_30s > 0.0 {
        features.realized_vol_3s / features.realized_vol_30s
    } else {
        1.0
    };

    // ── Toxicity proxy (instant — no lookahead) ──
    // Proportion of trades that moved the price significantly (> 0.01%)
    if tape.len() >= 2 {
        let recent: Vec<&TapeEntry> = tape.iter().rev().take(100).collect();
        let mut toxic_count = 0;
        for pair in recent.windows(2) {
            let prev = pair[1]; // older
            let curr = pair[0]; // newer
            if prev.price > 0.0 {
                let move_pct = ((curr.price - prev.price) / prev.price).abs();
                if move_pct > 0.0001 {
                    toxic_count += 1;
                }
            }
        }
        let total_pairs = recent.len().saturating_sub(1).max(1);
        features.toxicity_proxy_instant = toxic_count as f64 / total_pairs as f64;
    }

    // ── Cancel/add ratio (spoofing proxy) — supplied from BookManager delta stats ──
    features.cancel_add_ratio = cancel_add_ratio;

    // ── Vol compression (EVO-2) — same as vol_ratio, set here for clarity ──
    // vol_expanding is stateful and must be computed in FeatureEngine.
    features.vol_compression = features.vol_ratio;
    features.vol_expanding = false; // default; overridden by FeatureEngine

    // ── Refill speed — requires per-level order tracking, not yet available ──
    // features.refill_speed = ...; // TODO: implement with L2 order-level tracking

    features
}

/// OFI = (buy_vol - sell_vol) / (buy_vol + sell_vol) over a time window,
/// scaled by trade count confidence to prevent saturation with few trades.
/// When there are fewer than MIN_OFI_TRADES trades, the OFI magnitude is reduced
/// proportionally, preventing a single large trade from saturating OFI to ±1.
const MIN_OFI_TRADES: usize = 5;

fn compute_ofi(tape: &VecDeque<TapeEntry>, now_ms: i64, window_ms: i64) -> f64 {
    let mut buy_vol = 0.0;
    let mut sell_vol = 0.0;
    let mut trade_count: usize = 0;

    for entry in tape.iter().rev() {
        if now_ms - entry.timestamp > window_ms {
            break;
        }
        if entry.is_buy {
            buy_vol += entry.size;
        } else {
            sell_vol += entry.size;
        }
        trade_count += 1;
    }

    let total = buy_vol + sell_vol;
    if total == 0.0 {
        return 0.0;
    }
    let raw_ofi = (buy_vol - sell_vol) / total;

    // Confidence scaling: reduce magnitude when few trades to prevent saturation
    let confidence = (trade_count as f64 / MIN_OFI_TRADES as f64).min(1.0);
    raw_ofi * confidence
}

/// Realized volatility: std dev of log returns over a time window.
fn compute_realized_vol(tape: &VecDeque<TapeEntry>, now_ms: i64, window_ms: i64) -> f64 {
    let prices: Vec<f64> = tape
        .iter()
        .rev()
        .filter(|t| now_ms - t.timestamp <= window_ms)
        .map(|t| t.price)
        .collect();

    if prices.len() < 2 {
        return 0.0;
    }

    let returns: Vec<f64> = prices
        .windows(2)
        .filter(|w| w[1] > 0.0)
        .map(|w| (w[0] / w[1]).ln())
        .collect();

    if returns.is_empty() {
        return 0.0;
    }

    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64;
    variance.sqrt()
}

/// Price return in bps over a time window: (last_price - first_price) / first_price × 10_000.
/// Returns 0.0 if fewer than 2 trades are in the window.
fn compute_price_return(tape: &VecDeque<TapeEntry>, now_ms: i64, window_ms: i64) -> f64 {
    let window: Vec<&TapeEntry> = tape
        .iter()
        .rev()
        .filter(|t| now_ms - t.timestamp <= window_ms)
        .collect();
    if window.len() < 2 {
        return 0.0;
    }
    let last_price = window[0].price;                    // most recent
    let first_price = window[window.len() - 1].price;   // oldest in window
    if first_price <= 0.0 {
        return 0.0;
    }
    (last_price - first_price) / first_price * 10_000.0
}
