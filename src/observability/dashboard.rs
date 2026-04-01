use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::State,
    response::{
        sse::{Event, KeepAlive, Sse},
        Json,
    },
    routing::get,
    Router,
};
use futures_util::stream::Stream;
use prometheus::{Encoder, TextEncoder};
use serde::Serialize;
use tokio::sync::RwLock;
use tower_http::services::ServeDir;
use tracing::info;

use crate::observability::metrics::Metrics;

// ── Dashboard snapshot types ──

/// Full snapshot pushed via SSE every 500ms.
#[derive(Debug, Clone, Serialize, Default)]
pub struct DashboardSnapshot {
    pub ts: i64,
    pub equity: f64,
    pub drawdown_pct: f64,
    pub daily_pnl: f64,
    pub positions: Vec<PositionView>,
    pub pending_orders: Vec<PendingOrderView>,
    pub books: HashMap<String, BookView>,
    pub metrics: MetricsView,
    pub events: Vec<EventEntry>,
    pub closed_trades: Vec<ClosedTradeView>,
    pub bot_status: BotStatusView,
}

/// Serializable position for the UI.
#[derive(Debug, Clone, Serialize)]
pub struct PositionView {
    pub coin: String,
    pub direction: String,
    pub state: String,
    pub entry_price: f64,
    pub current_price: f64,
    pub pnl_pct: f64,
    pub pnl_usd: f64,
    pub elapsed_s: i64,
    pub break_even_applied: bool,
    pub sl: f64,
    pub tp: f64,
}

/// Serializable pending order for the UI.
#[derive(Debug, Clone, Serialize)]
pub struct PendingOrderView {
    pub coin: String,
    pub direction: String,
    pub state: String,
    pub price: f64,
    pub placed_s_ago: i64,
    pub max_wait_s: u64,
}

/// Serializable book view for the UI.
#[derive(Debug, Clone, Serialize)]
pub struct BookView {
    pub spread_bps: f64,
    pub imbalance_top5: f64,
    pub micro_price_vs_mid_bps: f64,
    pub toxicity: f64,
    pub regime: String,
}

/// Aggregated metrics for the UI.
#[derive(Debug, Clone, Serialize, Default)]
pub struct MetricsView {
    pub maker_fill_rate_1h: f64,
    pub adverse_selection_rate_1h: f64,
    pub spread_capture_bps_session: f64,
    pub ws_reconnects_today: u64,
    pub queue_lag_ms_p95: f64,
    pub kill_switch_count: u64,
}

/// A closed trade for the history view.
#[derive(Debug, Clone, Serialize)]
pub struct ClosedTradeView {
    pub coin: String,
    pub direction: String,
    pub entry_price: f64,
    pub exit_price: f64,
    pub pnl_usd: f64,
    pub pnl_pct: f64,
    pub close_reason: String,
    pub opened_at: i64,
    pub closed_at: i64,
    pub hold_s: i64,
    pub break_even_applied: bool,
}

/// Bot health and session status.
#[derive(Debug, Clone, Serialize, Default)]
pub struct BotStatusView {
    pub mode: String,
    pub started_at: i64,
    pub uptime_s: i64,
    pub active_coins: Vec<String>,
    /// Number of errors since startup (equity fetch fails, order errors, etc.)
    pub error_count: u64,
    /// Number of warnings since startup
    pub warn_count: u64,
    /// Last error message if any
    pub last_error: String,
    /// Last error timestamp
    pub last_error_ts: i64,
    /// Totals
    pub total_trades: u32,
    pub total_wins: u32,
    pub total_losses: u32,
    pub total_pnl_usd: f64,
    pub win_rate_pct: f64,
    // Period breakdowns: 1h, 24h, 7d
    pub pnl_1h: f64,
    pub pnl_24h: f64,
    pub pnl_7d: f64,
    pub trades_1h: u32,
    pub trades_24h: u32,
    pub trades_7d: u32,
    pub win_rate_1h: f64,
    pub win_rate_24h: f64,
    pub win_rate_7d: f64,
}

/// A single event in the rolling feed.
#[derive(Debug, Clone, Serialize)]
pub struct EventEntry {
    pub ts: i64,
    /// One of: "fill", "regime", "risk", "order", "system"
    pub event_type: String,
    pub message: String,
}

// ── Event feed ──

/// Rolling event feed (last N events).
pub struct EventFeed {
    events: VecDeque<EventEntry>,
    max_size: usize,
}

impl EventFeed {
    pub fn new(max_size: usize) -> Self {
        Self {
            events: VecDeque::with_capacity(max_size),
            max_size,
        }
    }

    pub fn push(&mut self, event_type: &str, message: String) {
        let entry = EventEntry {
            ts: chrono::Utc::now().timestamp_millis(),
            event_type: event_type.to_string(),
            message,
        };
        if self.events.len() >= self.max_size {
            self.events.pop_front();
        }
        self.events.push_back(entry);
    }

    pub fn snapshot(&self) -> Vec<EventEntry> {
        self.events.iter().cloned().collect()
    }
}

// ── Shared state for the dashboard HTTP server ──

pub struct DashboardState {
    pub metrics: Arc<Metrics>,
    pub snapshot: Arc<RwLock<DashboardSnapshot>>,
}

/// Build the Axum router for the dashboard.
pub fn build_router(state: Arc<DashboardState>) -> Router {
    // Determine static dir: try ./static/ then ./src/observability/static/
    let static_dir = if PathBuf::from("static").is_dir() {
        PathBuf::from("static")
    } else {
        PathBuf::from("src/observability/static")
    };

    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(prometheus_metrics))
        .route("/api/state", get(api_state))
        .route("/api/stream", get(sse_stream))
        .nest_service("/static", ServeDir::new(static_dir.clone()))
        .fallback_service(ServeDir::new(static_dir))
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

/// Full snapshot as JSON.
async fn api_state(State(state): State<Arc<DashboardState>>) -> Json<DashboardSnapshot> {
    let snap = state.snapshot.read().await;
    Json(snap.clone())
}

/// SSE stream — pushes full snapshot every 500ms.
async fn sse_stream(
    State(state): State<Arc<DashboardState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = futures_util::stream::unfold(state, |state| async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let data = {
            let snap = state.snapshot.read().await;
            serde_json::to_string(&*snap).unwrap_or_default()
        }; // RwLockReadGuard dropped here
        let event = Event::default().data(data);
        Some((Ok::<_, Infallible>(event), state))
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Start the dashboard HTTP server.
pub async fn start_dashboard(state: Arc<DashboardState>, port: u16) {
    let app = build_router(state);
    let addr = format!("0.0.0.0:{}", port);
    info!("[DASHBOARD] Starting on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
