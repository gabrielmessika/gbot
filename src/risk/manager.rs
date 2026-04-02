use std::path::{Path, PathBuf};

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::config::settings::RiskSettings;
use crate::execution::position_manager::PositionManager;
use crate::features::engine::CoinFeatures;
use crate::strategy::signal::Intent;

/// Persisted subset of risk state — survives restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RiskSnapshot {
    peak_equity: Decimal,
    daily_start_balance: Decimal,
    daily_reset_ts: i64,
    kill_switch_active: bool,
    saved_at: i64,
}

/// Risk manager — has absolute veto power over the strategy.
pub struct RiskManager {
    settings: RiskSettings,
    peak_equity: Decimal,
    daily_start_balance: Decimal,
    daily_reset_ts: i64,
    kill_switch_active: bool,
    error_count_5m: u32,
    last_error_ts: i64,
    state_path: Option<PathBuf>,
}

impl RiskManager {
    pub fn new(settings: RiskSettings, initial_equity: Decimal) -> Self {
        Self {
            settings,
            peak_equity: initial_equity,
            daily_start_balance: initial_equity,
            daily_reset_ts: chrono::Utc::now().timestamp_millis(),
            kill_switch_active: false,
            error_count_5m: 0,
            last_error_ts: 0,
            state_path: None,
        }
    }

    /// Create a RiskManager and attempt to restore persisted state from `data_dir/risk_state.json`.
    ///
    /// **Priority**: `initial_equity` (from the exchange in live, or simulated) is the source of
    /// truth. Persisted state only fills in fields that the exchange cannot provide
    /// (`daily_start_balance`, `daily_reset_ts`, `kill_switch`).
    /// `peak_equity` = max(exchange equity, persisted peak) to avoid artificially lowering it.
    ///
    /// Falls back to fresh state if the file is missing, corrupt, or stale (> 24h old).
    pub fn new_with_persistence(settings: RiskSettings, initial_equity: Decimal, data_dir: &str) -> Self {
        let state_path = Path::new(data_dir).join("risk_state.json");
        let mut mgr = Self::new(settings, initial_equity);
        mgr.state_path = Some(state_path.clone());

        if let Ok(contents) = std::fs::read_to_string(&state_path) {
            match serde_json::from_str::<RiskSnapshot>(&contents) {
                Ok(snap) => {
                    let now = chrono::Utc::now().timestamp_millis();
                    let age_h = (now - snap.saved_at) as f64 / 3_600_000.0;

                    if age_h > 24.0 {
                        info!("[RISK] Persisted state too old ({:.1}h) — starting fresh", age_h);
                    } else {
                        // peak_equity: never lower it below the real exchange equity
                        mgr.peak_equity = snap.peak_equity.max(initial_equity);
                        // daily tracking: only from persistence (exchange doesn't provide this)
                        mgr.daily_start_balance = snap.daily_start_balance;
                        mgr.daily_reset_ts = snap.daily_reset_ts;
                        mgr.kill_switch_active = snap.kill_switch_active;

                        info!(
                            "[RISK] Restored risk state (age={:.1}h): peak={} (exchange={}) daily_start={} kill_switch={}",
                            age_h, mgr.peak_equity, initial_equity, snap.daily_start_balance, snap.kill_switch_active
                        );
                    }
                }
                Err(e) => {
                    warn!("[RISK] Failed to parse risk_state.json: {} — starting fresh", e);
                }
            }
        } else {
            info!("[RISK] No persisted risk state found — starting fresh");
        }

        // Run daily reset check immediately (in case bot restarted after midnight)
        mgr.check_daily_reset(initial_equity);

        mgr
    }

    /// Persist current risk state to disk. Called periodically and on shutdown.
    pub fn save_state(&self) {
        let Some(ref path) = self.state_path else { return };

        let snap = RiskSnapshot {
            peak_equity: self.peak_equity,
            daily_start_balance: self.daily_start_balance,
            daily_reset_ts: self.daily_reset_ts,
            kill_switch_active: self.kill_switch_active,
            saved_at: chrono::Utc::now().timestamp_millis(),
        };

        match serde_json::to_string_pretty(&snap) {
            Ok(json) => {
                if let Err(e) = std::fs::write(path, json) {
                    warn!("[RISK] Failed to save risk state: {}", e);
                }
            }
            Err(e) => {
                warn!("[RISK] Failed to serialize risk state: {}", e);
            }
        }
    }

    /// Validate an intent before it reaches the exchange.
    /// Returns Ok(()) if allowed, Err(reasons) if rejected.
    pub fn validate_intent(
        &self,
        intent: &Intent,
        equity: Decimal,
        features: &CoinFeatures,
        position_mgr: &PositionManager,
    ) -> Result<(), Vec<String>> {
        let mut rejections = Vec::new();

        // Kill-switch check
        if self.kill_switch_active {
            rejections.push("Kill-switch active".to_string());
            return Err(rejections);
        }

        match intent {
            Intent::PlacePassiveEntry {
                coin, direction, ..
            } => {
                // Max open positions
                if position_mgr.count() >= self.settings.max_open_positions {
                    rejections.push(format!(
                        "Max open positions ({}) reached",
                        self.settings.max_open_positions
                    ));
                }

                // Max directional bias
                let dir_count = position_mgr.count_direction(*direction);
                if dir_count >= self.settings.max_directional_bias {
                    rejections.push(format!(
                        "Max directional bias ({}) reached for {:?}",
                        self.settings.max_directional_bias, direction
                    ));
                }

                // Duplicate coin
                if position_mgr.get(coin).is_some() {
                    rejections.push(format!("Already have position on {}", coin));
                }

                // Cooldown
                let now = chrono::Utc::now().timestamp_millis();
                if position_mgr.in_cooldown(coin, now) {
                    rejections.push(format!("{} is in cooldown", coin));
                }

                // Drawdown checks (on EQUITY, not available balance — t-bot bug #12)
                let drawdown_pct = self.current_drawdown_pct(equity);

                if drawdown_pct >= self.settings.drawdown_circuit_breaker_pct {
                    rejections.push(format!(
                        "Drawdown {:.1}% >= circuit breaker {:.1}%",
                        drawdown_pct, self.settings.drawdown_circuit_breaker_pct
                    ));
                }

                // Drawdown throttle — reduce max positions
                let effective_max_positions = self.effective_max_positions(drawdown_pct);
                if position_mgr.count() >= effective_max_positions {
                    rejections.push(format!(
                        "Drawdown throttle: max {} positions (drawdown={:.1}%)",
                        effective_max_positions, drawdown_pct
                    ));
                }

                // Daily loss limit
                let daily_pnl = equity - self.daily_start_balance;
                let daily_loss_pct = if self.daily_start_balance > Decimal::ZERO {
                    (daily_pnl / self.daily_start_balance * Decimal::new(100, 0))
                        .to_string()
                        .parse::<f64>()
                        .unwrap_or(0.0)
                } else {
                    0.0
                };
                if daily_loss_pct < -self.settings.max_daily_loss_pct {
                    rejections.push(format!(
                        "Daily loss {:.1}% exceeds max {:.1}%",
                        daily_loss_pct, self.settings.max_daily_loss_pct
                    ));
                }

                // Feature-based checks
                if features.book.spread_bps > self.settings.max_spread_bps {
                    rejections.push(format!(
                        "Spread {:.1} bps > max {:.1} bps",
                        features.book.spread_bps, self.settings.max_spread_bps
                    ));
                }
                if features.book.bid_depth_10bps < self.settings.min_depth_usd
                    || features.book.ask_depth_10bps < self.settings.min_depth_usd
                {
                    rejections.push(format!(
                        "Insufficient depth: bid=${:.0} ask=${:.0} (min=${:.0})",
                        features.book.bid_depth_10bps,
                        features.book.ask_depth_10bps,
                        self.settings.min_depth_usd
                    ));
                }
                if features.flow.toxicity_proxy_instant > self.settings.max_toxicity {
                    rejections.push(format!(
                        "Toxicity {:.2} > max {:.2}",
                        features.flow.toxicity_proxy_instant, self.settings.max_toxicity
                    ));
                }
                if features.flow.vol_ratio > self.settings.max_vol_ratio {
                    rejections.push(format!(
                        "Vol ratio {:.2} > max {:.2}",
                        features.flow.vol_ratio, self.settings.max_vol_ratio
                    ));
                }
            }
            // Exit intents always allowed (defensive exits must not be blocked)
            _ => {}
        }

        if rejections.is_empty() {
            Ok(())
        } else {
            Err(rejections)
        }
    }

    /// Compute position size based on risk parameters.
    pub fn compute_position_size(
        &self,
        equity: Decimal,
        entry_price: Decimal,
        stop_loss: Decimal,
        coin_max_leverage: u32,
    ) -> (Decimal, u32) {
        let sl_distance_pct = ((entry_price - stop_loss).abs() / entry_price)
            .to_string()
            .parse::<f64>()
            .unwrap_or(0.01);

        if sl_distance_pct == 0.0 {
            return (Decimal::ZERO, 1);
        }

        // Target: max_loss_per_trade_pct of equity
        let max_loss = equity
            * Decimal::try_from(self.settings.max_loss_per_trade_pct / 100.0)
                .unwrap_or(Decimal::new(15, 3));

        // Position size in USD = max_loss / sl_distance_pct
        let position_size_usd = max_loss / Decimal::try_from(sl_distance_pct).unwrap_or(Decimal::ONE);

        // Effective leverage = position_size / equity (capped)
        let effective_max_lev = self
            .settings
            .leverage
            .max_leverage
            .min(coin_max_leverage);

        let max_position = equity * Decimal::from(effective_max_lev);

        // Also cap by margin usage
        let max_margin_position = equity
            * Decimal::try_from(self.settings.max_margin_usage_pct / 100.0)
                .unwrap_or(Decimal::new(6, 1));

        let capped_size = position_size_usd.min(max_position).min(max_margin_position);

        // Size in coins
        let size_coins = if entry_price > Decimal::ZERO {
            capped_size / entry_price
        } else {
            Decimal::ZERO
        };

        // Actual leverage used
        let leverage = if equity > Decimal::ZERO {
            let lev = (capped_size / equity)
                .to_string()
                .parse::<f64>()
                .unwrap_or(1.0)
                .ceil() as u32;
            lev.clamp(self.settings.leverage.min_leverage, effective_max_lev)
        } else {
            self.settings.leverage.min_leverage
        };

        (size_coins, leverage)
    }

    /// Update equity tracking. Guard against spikes (tbot-scalp race condition fix).
    pub fn update_equity(&mut self, equity: Decimal) {
        if equity > self.peak_equity {
            let jump_pct = if self.peak_equity > Decimal::ZERO {
                ((equity - self.peak_equity) / self.peak_equity * Decimal::new(100, 0))
                    .to_string()
                    .parse::<f64>()
                    .unwrap_or(0.0)
            } else {
                0.0
            };

            if jump_pct <= self.settings.equity_spike_guard_pct {
                self.peak_equity = equity;
            } else {
                warn!(
                    "[RISK] Equity spike ignored: {} → {} (+{:.1}%) > {:.0}% guard",
                    self.peak_equity, equity, jump_pct, self.settings.equity_spike_guard_pct
                );
            }
        }
    }

    /// Reset daily counters at midnight UTC.
    pub fn check_daily_reset(&mut self, equity: Decimal) {
        let now = chrono::Utc::now().timestamp_millis();
        let midnight = chrono::Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();

        if self.daily_reset_ts < midnight {
            info!(
                "[RISK] Daily reset — new start balance: {}",
                equity
            );
            self.daily_start_balance = equity;
            self.daily_reset_ts = now;
            self.peak_equity = equity; // Reset peak daily
        }
    }

    /// Current drawdown percentage.
    fn current_drawdown_pct(&self, equity: Decimal) -> f64 {
        if self.peak_equity <= Decimal::ZERO {
            return 0.0;
        }
        let dd = (self.peak_equity - equity) / self.peak_equity * Decimal::new(100, 0);
        dd.to_string().parse::<f64>().unwrap_or(0.0).max(0.0)
    }

    /// Effective max positions given drawdown throttle.
    fn effective_max_positions(&self, drawdown_pct: f64) -> usize {
        if drawdown_pct >= self.settings.drawdown_throttle_severe_pct {
            1
        } else if drawdown_pct >= self.settings.drawdown_throttle_start_pct {
            self.settings.max_open_positions / 2
        } else {
            self.settings.max_open_positions
        }
    }

    /// Activate the kill-switch.
    pub fn activate_kill_switch(&mut self, reason: &str) {
        if !self.kill_switch_active {
            warn!("[RISK] KILL-SWITCH ACTIVATED: {}", reason);
            self.kill_switch_active = true;
        }
    }

    /// Deactivate the kill-switch (manual).
    pub fn deactivate_kill_switch(&mut self) {
        info!("[RISK] Kill-switch deactivated");
        self.kill_switch_active = false;
    }

    /// Check circuit breaker conditions.
    pub fn check_circuit_breaker(&mut self, equity: Decimal) {
        let dd = self.current_drawdown_pct(equity);
        if dd >= self.settings.drawdown_circuit_breaker_pct {
            self.activate_kill_switch(&format!("Drawdown {:.1}% >= circuit breaker", dd));
        }
    }

    /// Record an exchange error (for rate-based kill-switch).
    pub fn record_error(&mut self) {
        let now = chrono::Utc::now().timestamp_millis();
        // Reset counter if last error was > 5 minutes ago
        if now - self.last_error_ts > 300_000 {
            self.error_count_5m = 0;
        }
        self.error_count_5m += 1;
        self.last_error_ts = now;

        if self.error_count_5m > 10 {
            self.activate_kill_switch(&format!(
                "{} exchange errors in 5 minutes",
                self.error_count_5m
            ));
        }
    }

    pub fn is_kill_switch_active(&self) -> bool {
        self.kill_switch_active
    }

    pub fn peak_equity(&self) -> Decimal {
        self.peak_equity
    }

    pub fn daily_start_balance(&self) -> Decimal {
        self.daily_start_balance
    }
}
