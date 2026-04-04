use rust_decimal::Decimal;
use tracing::{debug, info};

use crate::config::settings::StrategySettings;
use crate::features::engine::CoinFeatures;
use crate::market_data::book::OrderBook;
use crate::regime::engine::Regime;
use crate::strategy::signal::{Direction, Intent, Signal};

/// MFDP V1 — Microstructure First Directional Pullback.
///
/// Detects a short-term directional bias via microstructure features,
/// waits for a micro-pullback, then enters passively via ALO.
pub struct MfdpStrategy {
    settings: StrategySettings,
}

impl MfdpStrategy {
    pub fn new(settings: StrategySettings) -> Self {
        Self { settings }
    }

    /// Evaluate features and regime to produce an Intent.
    /// The returned `PlacePassiveEntry` carries `stop_loss` and `take_profit` so
    /// main.rs can compute position size via RiskManager before execution.
    ///
    /// Also returns `(direction_score, queue_score)` for logging/recording.
    pub fn evaluate(
        &self,
        coin: &str,
        features: &CoinFeatures,
        regime: Regime,
        book: &OrderBook,
    ) -> (Intent, f64, f64) {
        // Regime gate
        match regime {
            Regime::DoNotTrade
            | Regime::ActiveToxic
            | Regime::NewslikeChaos
            | Regime::WideSpread
            | Regime::LowSignal
            | Regime::RangingMarket => return (Intent::NoTrade, 0.0, 0.0),
            // EVO-1: Mean-reversion path for flat markets
            Regime::RangingMeanRevert => {
                return self.evaluate_mean_reversion(coin, features, book);
            }
            _ => {}
        }

        // Feature maturity gate: skip if insufficient data
        if !features.flow.is_mature() {
            debug!("[MFDP] {} features not mature (trades_10s={}, vol_30s={:.6}) — skip",
                   coin, features.flow.trade_count_10s, features.flow.realized_vol_30s);
            return (Intent::NoTrade, 0.0, 0.0);
        }

        // Book health gate: skip if spread is zero or negative (crossed book)
        if features.book.spread_bps <= 0.0 {
            debug!("[MFDP] {} spread_bps={:.1} (crossed or empty book) — skip",
                   coin, features.book.spread_bps);
            return (Intent::NoTrade, 0.0, 0.0);
        }

        // Queue desirability score
        let queue_score = self.compute_queue_score(features);
        if queue_score < self.settings.queue_score_threshold {
            debug!("[MFDP] {} queue_score {:.3} < threshold — skip", coin, queue_score);
            return (Intent::NoTrade, 0.0, queue_score);
        }

        // Direction score
        let direction_score = self.compute_direction_score(features);

        let direction = if direction_score > self.settings.direction_threshold_long {
            Some(Direction::Long)
        } else if direction_score < self.settings.direction_threshold_short {
            Some(Direction::Short)
        } else {
            None
        };

        let direction = match direction {
            Some(d) => d,
            None => return (Intent::NoTrade, direction_score, queue_score),
        };

        // Determine entry / SL / TP price levels (dynamic based on volatility)
        let (entry_price, stop_loss, take_profit) =
            match self.compute_levels(direction, book, features.flow.realized_vol_30s) {
                Some(levels) => levels,
                None => return (Intent::NoTrade, direction_score, queue_score),
            };

        info!(
            "[MFDP] Signal: {} {} | dir_score={:.3} | queue={:.3} | entry={} | sl={} | tp={}",
            coin,
            match direction {
                Direction::Long => "LONG",
                Direction::Short => "SHORT",
            },
            direction_score,
            queue_score,
            entry_price,
            stop_loss,
            take_profit
        );

        debug!(
            "[MFDP] {} features: pr5s={:.2}bps pr10s={:.2}bps micro={:.2} vamp={:.2} depth_imb={:.2} tox={:.2} spread={:.1}bps vol_r={:.2} intensity={:.1}",
            coin,
            features.flow.price_return_5s,
            features.flow.price_return_10s,
            features.book.micro_price_vs_mid_bps,
            features.book.vamp_signal_bps,
            features.book.imbalance_weighted,
            features.flow.toxicity_proxy_instant,
            features.book.spread_bps,
            features.flow.vol_ratio,
            features.flow.trade_intensity,
        );

        // size = ZERO here; computed by main.rs via risk_mgr.compute_position_size()
        (Intent::PlacePassiveEntry {
            coin: coin.to_string(),
            direction,
            price: entry_price,
            stop_loss,
            take_profit,
            size: Decimal::ZERO,
            max_wait_s: self.settings.pullback_wait_retrace_s,
        }, direction_score, queue_score)
    }

    /// Evaluate with reduced size hint (for QuietThin regime).
    /// Risk manager will apply reduced sizing based on the regime passed by main.rs.
    pub fn evaluate_with_reduced_size(
        &self,
        coin: &str,
        features: &CoinFeatures,
        regime: Regime,
        book: &OrderBook,
    ) -> (Intent, f64, f64) {
        self.evaluate(coin, features, regime, book)
    }

    /// Check if an existing signal is still valid (for stale-quote detection).
    pub fn signal_still_valid(&self, signal: &Signal, features: &CoinFeatures) -> bool {
        let current_score = self.compute_direction_score(features);
        match signal.direction {
            Direction::Long => current_score > self.settings.direction_threshold_long * 0.5,
            Direction::Short => current_score < self.settings.direction_threshold_short * 0.5,
        }
    }

    /// Compute the directional score from features (range roughly −1 to +1).
    ///
    /// Primary signal: price momentum (pr5s/pr10s), corr=+0.354 with ret30s.
    /// Secondary: book microstructure (micro_price, vamp, depth_imb).
    /// Penalty: toxicity.
    fn compute_direction_score(&self, features: &CoinFeatures) -> f64 {
        let s = &self.settings;

        // Price momentum: normalize so 5bps = full signal for pr5s, 10bps for pr10s
        let pr5s_norm = {
            let v = features.flow.price_return_5s;
            v.signum() * (v.abs() / 5.0).min(1.0)
        };
        let pr10s_norm = {
            let v = features.flow.price_return_10s;
            v.signum() * (v.abs() / 10.0).min(1.0)
        };

        // Book microstructure
        let micro_norm = (features.book.micro_price_vs_mid_bps / 5.0).clamp(-1.0, 1.0);
        let vamp_norm = (features.book.vamp_signal_bps / 5.0).clamp(-1.0, 1.0);

        // Depth imbalance: imbalance_weighted is already signed [-1, +1]
        let depth_imb = features.book.imbalance_weighted.clamp(-1.0, 1.0);

        // Toxicity penalty (always negative contribution)
        let toxicity_penalty = features.flow.toxicity_proxy_instant;

        s.w_pr5s * pr5s_norm
            + s.w_pr10s * pr10s_norm
            + s.w_micro_price * micro_norm
            + s.w_vamp * vamp_norm
            + s.w_depth_imb * depth_imb
            - s.w_toxicity * toxicity_penalty
    }

    /// Compute queue desirability score (entry quality).
    fn compute_queue_score(&self, features: &CoinFeatures) -> f64 {
        let s = &self.settings;

        let spread_norm = 1.0 - (features.book.spread_bps / 10.0).min(1.0);
        let imb = features.book.imbalance_weighted.abs();
        let tox = 1.0 - features.flow.toxicity_proxy_instant;
        let depth_fav = (features.book.depth_ratio - 0.5).clamp(0.0, 1.0);
        let vol = 1.0 - (features.flow.vol_ratio / 3.0).min(1.0);

        s.queue_w_spread * spread_norm
            + s.queue_w_imbalance * imb
            + s.queue_w_toxicity * tox
            + s.queue_w_depth * depth_fav
            + s.queue_w_vol * vol
    }

    /// Compute entry, SL and TP price levels.
    /// SL distance is dynamic, based on realized 30s volatility scaled by sl_vol_multiplier,
    /// clamped between sl_min_bps and sl_max_bps. TP = SL × target_rr.
    fn compute_levels(
        &self,
        direction: Direction,
        book: &OrderBook,
        realized_vol_30s: f64,
    ) -> Option<(Decimal, Decimal, Decimal)> {
        let best_bid = Decimal::try_from(book.best_bid()?).ok()?;
        let best_ask = Decimal::try_from(book.best_ask()?).ok()?;

        // Dynamic SL: N× realized volatility, clamped to [min_bps, max_bps]
        let sl_min_pct = self.settings.sl_min_bps / 10_000.0;
        let sl_max_pct = self.settings.sl_max_bps / 10_000.0;
        let vol_based_sl_pct = realized_vol_30s * self.settings.sl_vol_multiplier;
        let sl_pct = vol_based_sl_pct.max(sl_min_pct).min(sl_max_pct);
        let tp_pct = sl_pct * self.settings.target_rr;

        let sl_mult = Decimal::try_from(sl_pct).unwrap_or(Decimal::new(15, 4)); // 0.0015 fallback
        let tp_mult = Decimal::try_from(tp_pct).unwrap_or(Decimal::new(30, 4));

        let (entry, sl, tp) = match direction {
            Direction::Long => {
                let entry = best_bid;
                let sl = entry - entry * sl_mult;
                let tp = entry + entry * tp_mult;
                (entry, sl, tp)
            }
            Direction::Short => {
                let entry = best_ask;
                let sl = entry + entry * sl_mult;
                let tp = entry - entry * tp_mult;
                (entry, sl, tp)
            }
        };

        debug!(
            "[MFDP] SL/TP: vol_30s={:.6} → sl_pct={:.4}% tp_pct={:.4}% (min={:.4}% max={:.4}%)",
            realized_vol_30s,
            sl_pct * 100.0,
            tp_pct * 100.0,
            sl_min_pct * 100.0,
            sl_max_pct * 100.0,
        );

        Some((entry, sl, tp))
    }

    // ══════════════════════════════════════════════════════════════════════
    // EVO-1 / EVO-3: Mean-reversion in flat (RangingMeanRevert) regime
    // ══════════════════════════════════════════════════════════════════════

    /// Evaluate mean-reversion opportunity in a flat market.
    /// Inverts book microstructure signals: if the book pushes price up → short (fade).
    /// Returns (Intent, mr_score, queue_score).
    fn evaluate_mean_reversion(
        &self,
        coin: &str,
        features: &CoinFeatures,
        book: &OrderBook,
    ) -> (Intent, f64, f64) {
        // Feature maturity gate
        if !features.flow.is_mature() {
            return (Intent::NoTrade, 0.0, 0.0);
        }
        if features.book.spread_bps <= 0.0 {
            return (Intent::NoTrade, 0.0, 0.0);
        }

        // Queue score: reuse existing queue desirability
        let queue_score = self.compute_queue_score(features);
        if queue_score < self.settings.queue_score_threshold {
            return (Intent::NoTrade, 0.0, queue_score);
        }

        // Mean-reversion score (signed: + = fade short, - = fade long)
        let mr_score = self.compute_mean_reversion_score(features);

        if mr_score.abs() < self.settings.mr_threshold {
            return (Intent::NoTrade, mr_score, queue_score);
        }

        // Direction: INVERSE of the dislocation
        // Positive mr_score = book/price pushed UP → short (expect return down)
        // Negative mr_score = book/price pushed DOWN → long (expect return up)
        let direction = if mr_score > 0.0 {
            Direction::Short
        } else {
            Direction::Long
        };

        // Compute entry/SL/TP with fixed MR distances
        let (entry_price, stop_loss, take_profit) =
            match self.compute_mr_levels(direction, book) {
                Some(levels) => levels,
                None => return (Intent::NoTrade, mr_score, queue_score),
            };

        info!(
            "[MR] Signal: {} {} | mr_score={:.3} | queue={:.3} | entry={} | sl={} | tp={} | micro={:.2} imb={:.2} vol_r={:.2}",
            coin,
            match direction { Direction::Long => "LONG", Direction::Short => "SHORT" },
            mr_score, queue_score, entry_price, stop_loss, take_profit,
            features.book.micro_price_vs_mid_bps,
            features.book.imbalance_weighted,
            features.flow.vol_ratio,
        );

        // size = ZERO; computed by main.rs via risk_mgr
        // max_wait_s = mr_max_hold_s (no pullback for MR: direct entry)
        (Intent::PlacePassiveEntry {
            coin: coin.to_string(),
            direction,
            price: entry_price,
            stop_loss,
            take_profit,
            size: Decimal::ZERO,
            max_wait_s: self.settings.mr_max_hold_s,
        }, mr_score, queue_score)
    }

    /// Compute the mean-reversion score from microstructure features.
    ///
    /// Returns a SIGNED score: positive = price pushed UP (short to fade),
    ///                          negative = price pushed DOWN (long to fade).
    ///
    /// Components (EVO-1 + EVO-3):
    ///   - micro_price_vs_mid_bps: primary dislocation signal
    ///   - imbalance_weighted: book pressure that should revert
    ///   - vamp_signal_bps: deeper book dislocation
    ///   - vol_spike fade (EVO-3): if vol_ratio > 2.0, fade the price_return_5s direction
    fn compute_mean_reversion_score(&self, features: &CoinFeatures) -> f64 {
        // Normalize each component to roughly [-1, +1]
        let micro_dev = (features.book.micro_price_vs_mid_bps / 3.0).clamp(-1.0, 1.0);
        let imb = features.book.imbalance_weighted.clamp(-1.0, 1.0);
        let vamp_dev = (features.book.vamp_signal_bps / 3.0).clamp(-1.0, 1.0);

        // EVO-3: Vol spike fade — if vol spiked and price moved, fade the move
        let vol_spike_signal = if features.flow.vol_ratio > 2.0
            && features.flow.price_return_5s.abs() > 2.0
        {
            // Strong signal: price moved on a spike → fade it
            let pr5s_norm = (features.flow.price_return_5s / 5.0).clamp(-1.0, 1.0);
            pr5s_norm  // positive pr5s → positive signal → will become short (fade)
        } else {
            // Mild vol contribution: higher vol in flat = more reversion opportunity
            let vol_excess = (features.flow.vol_ratio - 1.0).max(0.0) / 2.0;
            vol_excess * micro_dev.signum() // align with primary dislocation
        };

        // Weighted sum — all point in the "dislocation direction"
        // The caller will INVERT the direction (positive → short, negative → long)
        0.35 * micro_dev + 0.25 * imb + 0.25 * vamp_dev + 0.15 * vol_spike_signal
    }

    /// Compute entry, SL and TP for a mean-reversion trade.
    /// Uses fixed bps distances from config (not vol-based like directional trades).
    fn compute_mr_levels(
        &self,
        direction: Direction,
        book: &OrderBook,
    ) -> Option<(Decimal, Decimal, Decimal)> {
        let best_bid = Decimal::try_from(book.best_bid()?).ok()?;
        let best_ask = Decimal::try_from(book.best_ask()?).ok()?;

        let sl_pct = self.settings.mr_sl_bps / 10_000.0;
        let tp_pct = self.settings.mr_tp_bps / 10_000.0;

        let sl_mult = Decimal::try_from(sl_pct).unwrap_or(Decimal::new(8, 4));
        let tp_mult = Decimal::try_from(tp_pct).unwrap_or(Decimal::new(6, 4));

        let (entry, sl, tp) = match direction {
            Direction::Long => {
                let entry = best_bid;
                let sl = entry - entry * sl_mult;
                let tp = entry + entry * tp_mult;
                (entry, sl, tp)
            }
            Direction::Short => {
                let entry = best_ask;
                let sl = entry + entry * sl_mult;
                let tp = entry - entry * tp_mult;
                (entry, sl, tp)
            }
        };

        debug!(
            "[MR] Levels: sl={:.4}bps tp={:.4}bps entry={} sl={} tp={}",
            self.settings.mr_sl_bps, self.settings.mr_tp_bps, entry, sl, tp
        );

        Some((entry, sl, tp))
    }
}
