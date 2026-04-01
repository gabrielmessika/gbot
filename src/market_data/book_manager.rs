use std::collections::VecDeque;
use std::sync::Arc;

use dashmap::DashMap;
use serde_json::Value;
use tracing::{debug, warn};

use crate::exchange::ws_client::{TradePrintData, WsEvent};
use crate::market_data::book::{BookLevel, OrderBook};

/// Trade stored in the tape ring buffer.
#[derive(Debug, Clone)]
pub struct TapeEntry {
    pub price: f64,
    pub size: f64,
    pub is_buy: bool,
    pub timestamp: i64,
}

/// Rolling counts of book level additions vs. cancellations.
/// Used to compute `cancel_add_ratio` (spoofing proxy) in flow features.
#[derive(Debug, Clone, Default)]
pub struct BookDeltaStats {
    pub add_count: u64,
    pub cancel_count: u64,
    /// Start of the current counting window (reset every 60 s).
    pub window_start_ms: i64,
}

/// Manages order books and trade tapes for all subscribed coins.
pub struct BookManager {
    /// Local order books per coin.
    pub books: DashMap<String, OrderBook>,
    /// Trade tape per coin (ring buffer).
    pub tapes: DashMap<String, VecDeque<TapeEntry>>,
    /// Whether a coin's book is stale (queue saturated or reconnecting).
    pub book_stale: DashMap<String, bool>,
    /// Mid prices per coin.
    pub mids: DashMap<String, f64>,
    /// Book delta add/cancel counts per coin (cancel_add_ratio computation).
    pub delta_stats: DashMap<String, BookDeltaStats>,
    /// Max tape size per coin.
    tape_max_size: usize,
}

impl BookManager {
    pub fn new(coins: &[String], tape_max_size: usize) -> Arc<Self> {
        let mgr = Arc::new(Self {
            books: DashMap::new(),
            tapes: DashMap::new(),
            book_stale: DashMap::new(),
            mids: DashMap::new(),
            delta_stats: DashMap::new(),
            tape_max_size,
        });

        for coin in coins {
            mgr.books.insert(coin.clone(), OrderBook::new(coin.clone()));
            mgr.tapes.insert(coin.clone(), VecDeque::with_capacity(tape_max_size));
            mgr.book_stale.insert(coin.clone(), true); // stale until snapshot loaded
            mgr.delta_stats.insert(coin.clone(), BookDeltaStats::default());
        }

        mgr
    }

    /// Process a WsEvent and update internal state.
    pub fn handle_event(&self, event: &WsEvent) {
        match event {
            WsEvent::BookUpdate { coin, levels, timestamp } => {
                self.handle_book_update(coin, levels, *timestamp);
            }
            WsEvent::TradePrint { coin, trades } => {
                self.handle_trades(coin, trades);
            }
            WsEvent::MidUpdate { mids } => {
                self.handle_mid_update(mids);
            }
            WsEvent::Reconnected => {
                for mut entry in self.book_stale.iter_mut() {
                    *entry.value_mut() = true;
                }
            }
            WsEvent::SnapshotLoaded { coin } => {
                self.book_stale.insert(coin.clone(), false);
            }
            _ => {}
        }
    }

    fn handle_book_update(&self, coin: &str, levels: &Value, timestamp: i64) {
        let (bid_levels, ask_levels) = Self::parse_levels(levels);

        if let Some(mut book) = self.books.get_mut(coin) {
            if !book.snapshot_loaded {
                // First message = snapshot — no delta stats for snapshots
                book.apply_snapshot(&bid_levels, &ask_levels, timestamp);
                self.book_stale.insert(coin.to_string(), false);
                debug!(
                    "[BOOK] Snapshot loaded for {} ({} bids, {} asks)",
                    coin,
                    book.bids.len(),
                    book.asks.len()
                );
            } else {
                book.apply_delta(&bid_levels, &ask_levels, timestamp);

                // Update cancel/add delta stats for this coin (fixes gap #7)
                // "add" = level with size > 0 (new or updated)
                // "cancel" = level with size == 0 (removed)
                let adds = (bid_levels.iter().filter(|l| l.size > 0.0).count()
                    + ask_levels.iter().filter(|l| l.size > 0.0).count())
                    as u64;
                let cancels = (bid_levels.iter().filter(|l| l.size == 0.0).count()
                    + ask_levels.iter().filter(|l| l.size == 0.0).count())
                    as u64;

                if let Some(mut stats) = self.delta_stats.get_mut(coin) {
                    // Reset window every 60 s
                    if timestamp - stats.window_start_ms > 60_000 {
                        stats.add_count = 0;
                        stats.cancel_count = 0;
                        stats.window_start_ms = timestamp;
                    }
                    stats.add_count += adds;
                    stats.cancel_count += cancels;
                }
            }
        }
    }

    fn handle_trades(&self, coin: &str, trades: &[TradePrintData]) {
        if let Some(mut tape) = self.tapes.get_mut(coin) {
            for t in trades {
                let entry = TapeEntry {
                    price: t.price,
                    size: t.size,
                    is_buy: t.side == "B",
                    timestamp: t.timestamp,
                };
                if tape.len() >= self.tape_max_size {
                    tape.pop_front();
                }
                tape.push_back(entry);
            }
        }
    }

    fn handle_mid_update(&self, mids: &Value) {
        if let Some(obj) = mids.get("mids").and_then(|m| m.as_object()) {
            for (coin, mid_val) in obj {
                if let Some(mid_str) = mid_val.as_str() {
                    if let Ok(mid) = mid_str.parse::<f64>() {
                        self.mids.insert(coin.clone(), mid);
                    }
                }
            }
        }
    }

    fn parse_levels(levels: &Value) -> (Vec<BookLevel>, Vec<BookLevel>) {
        let mut bids = Vec::new();
        let mut asks = Vec::new();

        if let Some(arr) = levels.as_array() {
            if arr.len() >= 2 {
                if let Some(bid_arr) = arr[0].as_array() {
                    for level in bid_arr {
                        if let Some(l) = Self::parse_single_level(level) {
                            bids.push(l);
                        }
                    }
                }
                if let Some(ask_arr) = arr[1].as_array() {
                    for level in ask_arr {
                        if let Some(l) = Self::parse_single_level(level) {
                            asks.push(l);
                        }
                    }
                }
            }
        }

        (bids, asks)
    }

    fn parse_single_level(level: &Value) -> Option<BookLevel> {
        let price = level
            .get("px")
            .and_then(|p| p.as_str())
            .and_then(|s| s.parse::<f64>().ok())?;
        let size = level
            .get("sz")
            .and_then(|s| s.as_str())
            .and_then(|s| s.parse::<f64>().ok())?;
        Some(BookLevel { price, size })
    }

    /// Check if a coin's book is stale.
    pub fn is_stale(&self, coin: &str) -> bool {
        self.book_stale.get(coin).map(|v| *v).unwrap_or(true)
    }

    /// Get the cancel/add ratio for a coin over the last 60-second window.
    /// Returns 0.0 when no deltas have been seen yet.
    pub fn get_cancel_add_ratio(&self, coin: &str) -> f64 {
        self.delta_stats
            .get(coin)
            .map(|s| {
                if s.add_count > 0 {
                    s.cancel_count as f64 / s.add_count as f64
                } else {
                    0.0
                }
            })
            .unwrap_or(0.0)
    }
}
