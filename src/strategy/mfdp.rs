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
    pub fn evaluate(
        &self,
        coin: &str,
        features: &CoinFeatures,
        regime: Regime,
        book: &OrderBook,
    ) -> Intent {
        // Regime gate
        match regime {
            Regime::DoNotTrade
            | Regime::ActiveToxic
            | Regime::NewslikeChaos
            | Regime::WideSpread
            | Regime::LowSignal => return Intent::NoTrade,
            _ => {}
        }

        // Queue desirability score
        let queue_score = self.compute_queue_score(features);
        if queue_score < self.settings.queue_score_threshold {
            debug!("[MFDP] {} queue_score {:.3} < threshold — skip", coin, queue_score);
            return Intent::NoTrade;
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
            None => return Intent::NoTrade,
        };

        // Determine entry / SL / TP price levels
        let (entry_price, stop_loss, take_profit) =
            match self.compute_levels(direction, book) {
                Some(levels) => levels,
                None => return Intent::NoTrade,
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

        // size = ZERO here; computed by main.rs via risk_mgr.compute_position_size()
        Intent::PlacePassiveEntry {
            coin: coin.to_string(),
            direction,
            price: entry_price,
            stop_loss,
            take_profit,
            size: Decimal::ZERO,
            max_wait_s: self.settings.max_wait_pullback_s,
        }
    }

    /// Evaluate with reduced size hint (for QuietThin regime).
    /// Risk manager will apply reduced sizing based on the regime passed by main.rs.
    pub fn evaluate_with_reduced_size(
        &self,
        coin: &str,
        features: &CoinFeatures,
        regime: Regime,
        book: &OrderBook,
    ) -> Intent {
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
    fn compute_direction_score(&self, features: &CoinFeatures) -> f64 {
        let s = &self.settings;

        let ofi_norm = features.flow.ofi_10s.clamp(-1.0, 1.0);
        let micro_norm = (features.book.micro_price_vs_mid_bps / 5.0).clamp(-1.0, 1.0);
        let vamp_norm = (features.book.vamp_signal_bps / 5.0).clamp(-1.0, 1.0);

        // Aggression persistence signed by OFI direction
        let agg_signed = if features.flow.ofi_10s >= 0.0 {
            features.flow.aggression_persistence
        } else {
            -features.flow.aggression_persistence
        };

        // Depth ratio: > 1 = more bid depth = bullish
        let depth_signed = (features.book.depth_ratio - 1.0).clamp(-1.0, 1.0);

        // Toxicity penalty (always negative contribution)
        let toxicity_penalty = features.flow.toxicity_proxy_instant;

        s.w_ofi * ofi_norm
            + s.w_micro_price * micro_norm
            + s.w_vamp * vamp_norm
            + s.w_aggression * agg_signed
            + s.w_depth_ratio * depth_signed
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
    fn compute_levels(
        &self,
        direction: Direction,
        book: &OrderBook,
    ) -> Option<(Decimal, Decimal, Decimal)> {
        let best_bid = Decimal::try_from(book.best_bid()?).ok()?;
        let best_ask = Decimal::try_from(book.best_ask()?).ok()?;

        let (entry, sl, tp) = match direction {
            Direction::Long => {
                let entry = best_bid;
                let sl_distance = entry
                    * Decimal::try_from(self.settings.pullback_retrace_pct)
                        .unwrap_or(Decimal::new(3, 3));
                let sl = entry - sl_distance;
                let tp = entry + sl_distance * Decimal::new(2, 0);
                (entry, sl, tp)
            }
            Direction::Short => {
                let entry = best_ask;
                let sl_distance = entry
                    * Decimal::try_from(self.settings.pullback_retrace_pct)
                        .unwrap_or(Decimal::new(3, 3));
                let sl = entry + sl_distance;
                let tp = entry - sl_distance * Decimal::new(2, 0);
                (entry, sl, tp)
            }
        };

        Some((entry, sl, tp))
    }
}
