use serde::{Deserialize, Serialize};

use crate::config::settings::RegimeSettings;
use crate::features::engine::CoinFeatures;

/// Market regime classification.
/// Determines whether and how trading is allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Regime {
    /// Spread serré, book profond, vol basse — idéal pour entrées maker.
    QuietTight,
    /// Spread serré mais book peu profond — risque d'impact, prudence.
    QuietThin,
    /// Spread acceptable, vol normale, flow propre — tradable avec filtres.
    ActiveHealthy,
    /// Flow informatif, adverse selection élevée — ne pas poster.
    ActiveToxic,
    /// Spread > seuil — edge maker insuffisant.
    WideSpread,
    /// Vol explosive, updates très fréquentes, book instable — stop.
    NewslikeChaos,
    /// Marché trop calme, pas assez d'edge — pas de trade.
    LowSignal,
    /// Marché sans tendance (|price_return_30s| < trending_min_bps) — momentum nul.
    RangingMarket,
    /// Flat market + mean-reversion conditions met → entry autorisée en sens inverse.
    RangingMeanRevert,
    /// Kill-switch, circuit breaker, reconnect, book stale.
    DoNotTrade,
}

impl Regime {
    /// Whether this regime allows new entries.
    pub fn allows_entry(&self) -> bool {
        matches!(self, Regime::QuietTight | Regime::QuietThin | Regime::ActiveHealthy | Regime::RangingMeanRevert)
    }

    /// Whether positions should be force-exited.
    pub fn requires_exit(&self) -> bool {
        matches!(self, Regime::DoNotTrade | Regime::NewslikeChaos)
    }
}

/// Classify the current market regime for a coin based on its features.
///
/// `seconds_to_funding` is the time remaining until the next funding payment for this coin.
/// - `None` means unknown (no restriction applied).
/// - `Some(s) <= funding_boundary_force_exit_s` → DoNotTrade (close positions).
/// - `Some(s) <= funding_boundary_no_entry_s` → ActiveToxic (no new entries).
pub fn classify(
    features: &CoinFeatures,
    book_stale: bool,
    reconnect_recent: bool,
    settings: &RegimeSettings,
    seconds_to_funding: Option<u64>,
) -> Regime {
    // ── Funding boundary checks ──
    if let Some(secs) = seconds_to_funding {
        if secs <= settings.funding_boundary_force_exit_s {
            return Regime::DoNotTrade;
        }
        if secs <= settings.funding_boundary_no_entry_s {
            // Block new entries; existing positions may stay open
            return Regime::ActiveToxic;
        }
    }

    // ── DoNotTrade checks (any one = immediate DNT) ──
    if book_stale || reconnect_recent {
        return Regime::DoNotTrade;
    }
    if features.book.spread_bps > settings.dnt_max_spread_bps {
        return Regime::DoNotTrade;
    }
    if features.flow.vol_ratio > settings.dnt_max_vol_ratio {
        return Regime::DoNotTrade;
    }
    if features.book.bid_depth_10bps < settings.dnt_min_depth_usd
        || features.book.ask_depth_10bps < settings.dnt_min_depth_usd
    {
        return Regime::DoNotTrade;
    }

    // ── NewslikeChaos ──
    if features.flow.vol_ratio > settings.active_healthy_max_vol_ratio * 1.5
        && features.flow.trade_intensity > 50.0
    {
        return Regime::NewslikeChaos;
    }

    // ── ActiveToxic ──
    if features.flow.toxicity_proxy_instant > settings.active_toxic_min_toxicity
        || features.flow.cancel_add_ratio > settings.max_cancel_add_ratio
    {
        return Regime::ActiveToxic;
    }

    // ── WideSpread ──
    if features.book.spread_bps > settings.active_healthy_max_spread_bps {
        return Regime::WideSpread;
    }

    // ── RangingMarket: price flat over 30s → no directional momentum ──
    // MUST be checked BEFORE tradable regimes (QuietTight, ActiveHealthy).
    // Empirically: directional_acc=0% in flat market (|pr30s| < trending_min_bps).
    // Bug fix: was placed after QuietTight → never triggered (QuietTight matched first).
    if features.flow.price_return_30s.abs() < settings.trending_min_bps {
        // EVO-1 / EVO-2: Check mean-reversion conditions instead of blocking.
        // EVO-2 squeeze guard: if vol is expanding rapidly, a breakout is imminent → no MR.
        let vol_expanding_breakout = features.flow.vol_expanding
            && features.flow.vol_compression > settings.squeeze_vol_compression_max;

        if !vol_expanding_breakout
            && settings.mr_enabled
            && features.book.spread_bps < settings.mr_max_spread_bps
            && features.flow.toxicity_proxy_instant < settings.mr_max_toxicity
            && (features.book.micro_price_vs_mid_bps.abs() > settings.mr_min_micro_dev_bps
                || features.book.imbalance_weighted.abs() > settings.mr_min_imbalance
                || (features.flow.vol_ratio > settings.mr_vol_spike_threshold
                    && features.flow.price_return_5s.abs() > settings.mr_vol_spike_pr5s_min_bps))
        {
            return Regime::RangingMeanRevert;
        }

        return Regime::RangingMarket;
    }

    // ── QuietTight ──
    if features.book.spread_bps <= settings.quiet_tight_max_spread_bps
        && features.flow.toxicity_proxy_instant <= settings.quiet_tight_max_toxicity
        && features.flow.vol_ratio <= settings.quiet_tight_max_vol_ratio
        && features.book.bid_depth_10bps >= settings.quiet_tight_min_depth_usd
        && features.book.ask_depth_10bps >= settings.quiet_tight_min_depth_usd
    {
        return Regime::QuietTight;
    }

    // ── QuietThin ──
    if features.book.spread_bps <= settings.quiet_tight_max_spread_bps
        && (features.book.bid_depth_10bps < settings.quiet_tight_min_depth_usd
            || features.book.ask_depth_10bps < settings.quiet_tight_min_depth_usd)
    {
        return Regime::QuietThin;
    }

    // ── ActiveHealthy ──
    if features.book.spread_bps <= settings.active_healthy_max_spread_bps
        && features.flow.toxicity_proxy_instant <= settings.active_healthy_max_toxicity
        && features.flow.vol_ratio <= settings.active_healthy_max_vol_ratio
        && features.book.bid_depth_10bps >= settings.active_healthy_min_depth_usd
    {
        return Regime::ActiveHealthy;
    }

    // ── LowSignal (catch-all for quiet markets with insufficient conditions) ──
    if features.flow.trade_intensity < 1.0 {
        return Regime::LowSignal;
    }

    // Default: ActiveHealthy if nothing else matched
    Regime::ActiveHealthy
}
