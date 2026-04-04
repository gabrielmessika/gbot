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

        // Momentum concordance filter: pr5s and pr10s must agree on direction.
        // Apr 2-3 data: winners and losers had identical dir_score (0.674 vs 0.672).
        // Contradictory momentum (pr5s up + pr10s down) produces scores that pass
        // the threshold but have zero predictive power. Require alignment.
        let pr5s = features.flow.price_return_5s;
        let pr10s = features.flow.price_return_10s;
        if pr5s.abs() > 0.5 && pr10s.abs() > 0.5 {
            // Both have meaningful magnitude — check they agree with trade direction
            let momentum_agrees = match direction {
                Direction::Long => pr5s > 0.0 && pr10s > 0.0,
                Direction::Short => pr5s < 0.0 && pr10s < 0.0,
            };
            if !momentum_agrees {
                debug!(
                    "[MFDP] {} momentum discord: pr5s={:.2} pr10s={:.2} vs {:?} — skip",
                    coin, pr5s, pr10s, direction
                );
                return (Intent::NoTrade, direction_score, queue_score);
            }
        }

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
}
