use std::sync::Arc;

use axum::{
    extract::State,
    response::Json,
    routing::get,
    Router,
};
use prometheus::{Encoder, TextEncoder};
use serde_json::{json, Value};
use tracing::info;

use crate::observability::metrics::Metrics;

/// Shared state for the dashboard HTTP server.
pub struct DashboardState {
    pub metrics: Arc<Metrics>,
}

/// Build the Axum router for the dashboard.
pub fn build_router(state: Arc<DashboardState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(prometheus_metrics))
        .route("/api/status", get(bot_status))
        .with_state(state)
}

async fn health() -> &'static str {
    "ok"
}

async fn prometheus_metrics(State(state): State<Arc<DashboardState>>) -> String {
    let encoder = TextEncoder::new();
    let metric_families = state.metrics.registry.gather();
    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    String::from_utf8(buffer).unwrap_or_default()
}

async fn bot_status(State(state): State<Arc<DashboardState>>) -> Json<Value> {
    Json(json!({
        "status": "running",
        "equity": state.metrics.equity.get(),
        "drawdown_pct": state.metrics.drawdown_pct.get(),
        "open_positions": state.metrics.open_positions.get(),
        "ws_reconnects": state.metrics.ws_reconnect_total.get(),
    }))
}

/// Start the dashboard HTTP server.
pub async fn start_dashboard(state: Arc<DashboardState>, port: u16) {
    let app = build_router(state);
    let addr = format!("0.0.0.0:{}", port);
    info!("[DASHBOARD] Starting on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
