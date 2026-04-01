use serde::Deserialize;

/// Top-level configuration parsed from config/default.toml + env overrides.
#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    pub general: GeneralSettings,
    pub exchange: ExchangeSettings,
    pub coins: CoinListSettings,
    pub features: FeaturesSettings,
    pub regime: RegimeSettings,
    pub strategy: StrategySettings,
    pub risk: RiskSettings,
    pub execution: ExecutionSettings,
    pub recording: RecordingSettings,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GeneralSettings {
    pub mode: BotMode,
    pub log_level: String,
    pub data_dir: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BotMode {
    Observation,
    DryRun,
    Live,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExchangeSettings {
    pub ws_url: String,
    pub rest_url: String,
    pub wallet_address: String,
    pub agent_private_key: String,
    pub subaccount: Option<String>,
    pub rate_limit: RateLimitSettings,
    pub timeouts: TimeoutSettings,
    pub reconnect: ReconnectSettings,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitSettings {
    pub max_weight_per_minute: u32,
    pub candle_base_weight: u32,
    pub info_heavy_weight: u32,
    pub info_light_weight: u32,
    pub order_weight: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TimeoutSettings {
    pub connect_ms: u64,
    pub read_ms: u64,
    pub ws_heartbeat_s: u64,
    pub ws_stale_s: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReconnectSettings {
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
    pub backoff_factor: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CoinListSettings {
    pub active: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeaturesSettings {
    pub trade_tape_size: usize,
    pub ofi_windows: Vec<f64>,
    pub vol_windows: Vec<f64>,
    pub toxicity_lookahead_s: f64,
    pub toxicity_sample_size: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegimeSettings {
    pub quiet_tight_max_spread_bps: f64,
    pub quiet_tight_max_toxicity: f64,
    pub quiet_tight_max_vol_ratio: f64,
    pub quiet_tight_min_depth_usd: f64,

    pub active_healthy_max_spread_bps: f64,
    pub active_healthy_max_toxicity: f64,
    pub active_healthy_max_vol_ratio: f64,
    pub active_healthy_min_depth_usd: f64,

    pub active_toxic_min_toxicity: f64,

    pub dnt_max_spread_bps: f64,
    pub dnt_max_vol_ratio: f64,
    pub dnt_min_depth_usd: f64,

    pub funding_boundary_no_entry_s: u64,
    pub funding_boundary_force_exit_s: u64,
    pub max_cancel_add_ratio: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StrategySettings {
    pub w_ofi: f64,
    pub w_micro_price: f64,
    pub w_vamp: f64,
    pub w_aggression: f64,
    pub w_depth_ratio: f64,
    pub w_toxicity: f64,

    pub direction_threshold_long: f64,
    pub direction_threshold_short: f64,

    pub pullback_retrace_pct: f64,
    pub max_wait_pullback_s: u64,

    pub queue_w_spread: f64,
    pub queue_w_imbalance: f64,
    pub queue_w_toxicity: f64,
    pub queue_w_depth: f64,
    pub queue_w_vol: f64,
    pub queue_score_threshold: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RiskSettings {
    pub max_loss_per_trade_pct: f64,
    pub max_open_positions: usize,
    pub max_directional_bias: usize,
    pub max_margin_usage_pct: f64,
    pub max_daily_loss_pct: f64,
    pub drawdown_throttle_start_pct: f64,
    pub drawdown_throttle_severe_pct: f64,
    pub drawdown_circuit_breaker_pct: f64,
    pub cooldown_after_close_s: u64,
    pub max_slippage_pct: f64,
    pub min_spread_bps: f64,
    pub max_spread_bps: f64,
    pub min_depth_usd: f64,
    pub max_toxicity: f64,
    pub max_vol_ratio: f64,
    pub equity_spike_guard_pct: f64,
    pub leverage: LeverageSettings,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeverageSettings {
    pub min_leverage: u32,
    pub max_leverage: u32,
    pub default_leverage: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecutionSettings {
    pub max_hold_s: u64,
    pub max_mae_bps: f64,
    pub order_timeout_s: u64,
    pub fill_poll_interval_s: u64,
    pub sync_interval_s: u64,
    pub breakeven: BreakevenSettings,
    pub trailing: TrailingSettings,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BreakevenSettings {
    pub trigger_pct: f64,
    pub detection_tolerance_pct: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TrailingSettings {
    pub tier1_progress_pct: f64,
    pub tier1_lock_pct: f64,
    pub tier2_progress_pct: f64,
    pub tier2_lock_pct: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecordingSettings {
    pub enabled: bool,
    pub flush_interval_s: u64,
}

impl Settings {
    /// Load settings from config/default.toml, then overlay with env vars (GBOT_ prefix).
    pub fn load() -> anyhow::Result<Self> {
        let builder = config::Config::builder()
            .add_source(config::File::with_name("config/default"))
            .add_source(
                config::Environment::with_prefix("GBOT")
                    .separator("__")
                    .try_parsing(true),
            )
            .build()?;

        let settings: Settings = builder.try_deserialize()?;
        Ok(settings)
    }
}
