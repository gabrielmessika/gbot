use prometheus::{
    self, Gauge, IntCounter, IntCounterVec, IntGauge, Opts, Registry,
};

/// Prometheus metrics for observability.
pub struct Metrics {
    pub registry: Registry,

    // Connection
    pub ws_reconnect_total: IntCounter,

    // Orders
    pub order_reject_total: IntCounterVec,
    pub passive_fill_count: IntCounter,
    pub passive_cancel_count: IntCounter,

    // Trading
    pub maker_share: Gauge,
    pub adverse_selection_rate: Gauge,
    pub spread_capture_bps: Gauge,

    // System
    pub queue_lag_ms: Gauge,
    pub kill_switch_total: IntCounter,

    // Portfolio
    pub equity: Gauge,
    pub drawdown_pct: Gauge,
    pub open_positions: IntGauge,
}

impl Metrics {
    pub fn new() -> Self {
        let registry = Registry::new();

        let ws_reconnect_total = IntCounter::new("ws_reconnect_total", "WebSocket reconnections")
            .expect("metric creation");
        let order_reject_total = IntCounterVec::new(
            Opts::new("order_reject_total", "Orders rejected by exchange"),
            &["coin", "reason"],
        )
        .expect("metric creation");
        let passive_fill_count =
            IntCounter::new("passive_fill_count", "ALO orders filled").expect("metric creation");
        let passive_cancel_count =
            IntCounter::new("passive_cancel_count", "ALO orders cancelled").expect("metric creation");
        let maker_share =
            Gauge::new("maker_share", "Percentage of trades as maker").expect("metric creation");
        let adverse_selection_rate =
            Gauge::new("adverse_selection_rate", "Proxy adverse selection rate")
                .expect("metric creation");
        let spread_capture_bps =
            Gauge::new("spread_capture_bps", "Bps captured net of fees").expect("metric creation");
        let queue_lag_ms =
            Gauge::new("queue_lag_ms", "Message queue lag in ms").expect("metric creation");
        let kill_switch_total =
            IntCounter::new("kill_switch_total", "Kill-switch activations").expect("metric creation");
        let equity = Gauge::new("equity", "Current equity").expect("metric creation");
        let drawdown_pct =
            Gauge::new("drawdown_pct", "Current drawdown percentage").expect("metric creation");
        let open_positions =
            IntGauge::new("open_positions", "Number of open positions").expect("metric creation");

        registry.register(Box::new(ws_reconnect_total.clone())).ok();
        registry.register(Box::new(order_reject_total.clone())).ok();
        registry.register(Box::new(passive_fill_count.clone())).ok();
        registry.register(Box::new(passive_cancel_count.clone())).ok();
        registry.register(Box::new(maker_share.clone())).ok();
        registry.register(Box::new(adverse_selection_rate.clone())).ok();
        registry.register(Box::new(spread_capture_bps.clone())).ok();
        registry.register(Box::new(queue_lag_ms.clone())).ok();
        registry.register(Box::new(kill_switch_total.clone())).ok();
        registry.register(Box::new(equity.clone())).ok();
        registry.register(Box::new(drawdown_pct.clone())).ok();
        registry.register(Box::new(open_positions.clone())).ok();

        Self {
            registry,
            ws_reconnect_total,
            order_reject_total,
            passive_fill_count,
            passive_cancel_count,
            maker_share,
            adverse_selection_rate,
            spread_capture_bps,
            queue_lag_ms,
            kill_switch_total,
            equity,
            drawdown_pct,
            open_positions,
        }
    }
}
