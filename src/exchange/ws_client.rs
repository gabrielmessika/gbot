use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::time::{Instant, interval, sleep};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

use crate::config::settings::ExchangeSettings;

/// Events emitted by the WebSocket client.
#[derive(Debug, Clone)]
pub enum WsEvent {
    /// L2 book update (snapshot or delta).
    BookUpdate {
        coin: String,
        levels: Value,
        timestamp: i64,
    },
    /// Trade prints from the tape.
    TradePrint {
        coin: String,
        trades: Vec<TradePrintData>,
    },
    /// Mid price update for all coins.
    MidUpdate {
        mids: Value,
    },
    /// User order update (fill, cancel, reject).
    UserOrderUpdate {
        data: Value,
    },
    /// WebSocket reconnected — consumers should pause trading.
    Reconnected,
    /// Snapshot loaded after reconnect — consumers can resume.
    SnapshotLoaded {
        coin: String,
    },
}

#[derive(Debug, Clone)]
pub struct TradePrintData {
    pub coin: String,
    pub price: f64,
    pub size: f64,
    pub side: String, // "B" or "A"
    pub timestamp: i64,
}

/// Persistent WebSocket client with reconnect, heartbeat, and message routing.
pub struct WsClient {
    settings: ExchangeSettings,
    coins: Vec<String>,
    event_tx: mpsc::Sender<WsEvent>,
}

impl WsClient {
    pub fn new(
        settings: ExchangeSettings,
        coins: Vec<String>,
        event_tx: mpsc::Sender<WsEvent>,
    ) -> Self {
        Self {
            settings,
            coins,
            event_tx,
        }
    }

    /// Run the WebSocket client loop. Reconnects automatically on failure.
    pub async fn run(self: Arc<Self>) {
        let mut delay_ms = self.settings.reconnect.initial_delay_ms;

        loop {
            info!("[WS] Connecting to {}", self.settings.ws_url);
            let connect_start = std::time::Instant::now();

            match self.connect_and_listen().await {
                Ok(_) => {
                    info!("[WS] Connection closed gracefully");
                }
                Err(e) => {
                    error!("[WS] Connection error: {}", e);
                }
            }

            // Only reset backoff if the connection lived long enough (>30s).
            // A short-lived connection (e.g. immediate "Connection reset") is
            // effectively an error — keep increasing the backoff.
            let lived_s = connect_start.elapsed().as_secs();
            if lived_s > 30 {
                delay_ms = self.settings.reconnect.initial_delay_ms;
            }

            // Notify reconnect
            let _ = self.event_tx.send(WsEvent::Reconnected).await;

            warn!("[WS] Reconnecting in {}ms (connection lived {}s)", delay_ms, lived_s);
            sleep(Duration::from_millis(delay_ms)).await;

            // Exponential backoff
            delay_ms = ((delay_ms as f64 * self.settings.reconnect.backoff_factor) as u64)
                .min(self.settings.reconnect.max_delay_ms);
        }
    }

    async fn connect_and_listen(&self) -> Result<()> {
        let (ws_stream, _) = connect_async(&self.settings.ws_url).await?;
        let (mut write, mut read) = ws_stream.split();

        info!("[WS] Connected. Subscribing to channels...");

        // Subscribe to channels with pacing (100ms between each) to avoid
        // Hyperliquid killing the connection on burst subscription spam.
        // Bug: sending 30+ subscribe messages instantly caused "Connection reset".
        for coin in &self.coins {
            // L2 book
            let sub_book = json!({
                "method": "subscribe",
                "subscription": {"type": "l2Book", "coin": coin}
            });
            write.send(Message::Text(sub_book.to_string().into())).await?;

            // Trades
            let sub_trades = json!({
                "method": "subscribe",
                "subscription": {"type": "trades", "coin": coin}
            });
            write.send(Message::Text(sub_trades.to_string().into())).await?;

            // Pace: 250ms between each coin's subscriptions.
            // 100ms was not enough — 15 coins still caused "Connection reset".
            // 3 coins (no pacing) worked fine → threshold is between 6 and 30 messages.
            sleep(Duration::from_millis(250)).await;
        }

        // allMids
        let sub_mids = json!({
            "method": "subscribe",
            "subscription": {"type": "allMids"}
        });
        write.send(Message::Text(sub_mids.to_string().into())).await?;

        // orderUpdates (if wallet configured)
        if !self.settings.wallet_address.is_empty() {
            let sub_orders = json!({
                "method": "subscribe",
                "subscription": {"type": "orderUpdates", "user": self.settings.wallet_address}
            });
            write.send(Message::Text(sub_orders.to_string().into())).await?;
        }

        info!("[WS] Subscribed to {} coins + allMids + orderUpdates", self.coins.len());

        let heartbeat_interval = Duration::from_secs(self.settings.timeouts.ws_heartbeat_s);
        let stale_timeout = Duration::from_secs(self.settings.timeouts.ws_stale_s);
        let mut last_message = Instant::now();
        let mut heartbeat_tick = interval(heartbeat_interval);

        loop {
            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            last_message = Instant::now();
                            if let Err(e) = self.handle_message(&text).await {
                                warn!("[WS] Error handling message: {}", e);
                            }
                        }
                        Some(Ok(Message::Ping(data))) => {
                            let _ = write.send(Message::Pong(data)).await;
                            last_message = Instant::now();
                        }
                        Some(Ok(Message::Close(_))) => {
                            info!("[WS] Server closed connection");
                            break;
                        }
                        Some(Err(e)) => {
                            error!("[WS] Read error: {}", e);
                            break;
                        }
                        None => {
                            info!("[WS] Stream ended");
                            break;
                        }
                        _ => {}
                    }
                }
                _ = heartbeat_tick.tick() => {
                    // Check for stale connection
                    if last_message.elapsed() > stale_timeout {
                        warn!("[WS] No message in {}s — reconnecting", stale_timeout.as_secs());
                        break;
                    }
                    // Send ping
                    let _ = write.send(Message::Ping(vec![].into())).await;
                }
            }
        }

        Ok(())
    }

    async fn handle_message(&self, text: &str) -> Result<()> {
        let msg: Value = serde_json::from_str(text)?;

        let channel = msg.get("channel").and_then(|c| c.as_str()).unwrap_or("");

        match channel {
            "l2Book" => {
                if let Some(data) = msg.get("data") {
                    let coin = data
                        .get("coin")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string();
                    let time = data.get("time").and_then(|t| t.as_i64()).unwrap_or(0);
                    let levels = data.get("levels").cloned().unwrap_or(Value::Null);

                    self.send_event(WsEvent::BookUpdate {
                        coin,
                        levels,
                        timestamp: time,
                    })
                    .await;
                }
            }
            "trades" => {
                if let Some(data) = msg.get("data") {
                    if let Some(arr) = data.as_array() {
                        let mut trades = Vec::new();
                        for t in arr {
                            let coin = t
                                .get("coin")
                                .and_then(|c| c.as_str())
                                .unwrap_or("")
                                .to_string();
                            let price = t
                                .get("px")
                                .and_then(|p| p.as_str())
                                .unwrap_or("0")
                                .parse::<f64>()
                                .unwrap_or(0.0);
                            let size = t
                                .get("sz")
                                .and_then(|s| s.as_str())
                                .unwrap_or("0")
                                .parse::<f64>()
                                .unwrap_or(0.0);
                            let side = t
                                .get("side")
                                .and_then(|s| s.as_str())
                                .unwrap_or("B")
                                .to_string();
                            let timestamp =
                                t.get("time").and_then(|t| t.as_i64()).unwrap_or(0);

                            trades.push(TradePrintData {
                                coin,
                                price,
                                size,
                                side,
                                timestamp,
                            });
                        }
                        if let Some(first) = trades.first() {
                            self.send_event(WsEvent::TradePrint {
                                coin: first.coin.clone(),
                                trades,
                            })
                            .await;
                        }
                    }
                }
            }
            "allMids" => {
                if let Some(data) = msg.get("data") {
                    self.send_event(WsEvent::MidUpdate {
                        mids: data.clone(),
                    })
                    .await;
                }
            }
            "orderUpdates" => {
                if let Some(data) = msg.get("data") {
                    self.send_event(WsEvent::UserOrderUpdate { data: data.clone() })
                        .await;
                }
            }
            _ => {
                debug!("[WS] Unhandled channel: {}", channel);
            }
        }

        Ok(())
    }

    async fn send_event(&self, event: WsEvent) {
        if self.event_tx.send(event).await.is_err() {
            error!("[WS] Event channel closed — consumer dropped");
        }
    }
}
