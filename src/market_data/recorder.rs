use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::info;

use crate::market_data::book::OrderBook;
use crate::market_data::book_manager::TapeEntry;

/// Row for L2 book recording.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookRecord {
    pub timestamp: i64,
    pub coin: String,
    pub best_bid: f64,
    pub best_ask: f64,
    pub bid_depth_10bps: f64,
    pub ask_depth_10bps: f64,
    pub spread_bps: f64,
    pub mid: f64,
}

/// Row for trades recording.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeRecord {
    pub timestamp: i64,
    pub coin: String,
    pub price: f64,
    pub size: f64,
    pub is_buy: bool,
}

/// Async data recorder that buffers data and flushes to disk periodically.
/// Phase 1: writes JSONL files. Phase 2+: upgrade to Parquet.
pub struct Recorder {
    data_dir: PathBuf,
    book_buffers: Mutex<HashMap<String, Vec<BookRecord>>>,
    trade_buffers: Mutex<HashMap<String, Vec<TradeRecord>>>,
    enabled: bool,
}

impl Recorder {
    pub fn new(data_dir: &str, coins: &[String], enabled: bool) -> Arc<Self> {
        let rec = Arc::new(Self {
            data_dir: PathBuf::from(data_dir),
            book_buffers: Mutex::new(HashMap::new()),
            trade_buffers: Mutex::new(HashMap::new()),
            enabled,
        });

        if enabled {
            // Initialize buffers
            let mut book_bufs = HashMap::new();
            let mut trade_bufs = HashMap::new();
            for coin in coins {
                book_bufs.insert(coin.clone(), Vec::with_capacity(10_000));
                trade_bufs.insert(coin.clone(), Vec::with_capacity(10_000));
            }
            // We'll set them in an async context
            let rec_clone = rec.clone();
            tokio::spawn(async move {
                *rec_clone.book_buffers.lock().await = book_bufs;
                *rec_clone.trade_buffers.lock().await = trade_bufs;
            });
        }

        rec
    }

    /// Record a book snapshot.
    pub async fn record_book(&self, book: &OrderBook) {
        if !self.enabled {
            return;
        }
        let record = BookRecord {
            timestamp: book.last_update_ts,
            coin: book.coin.clone(),
            best_bid: book.best_bid().unwrap_or(0.0),
            best_ask: book.best_ask().unwrap_or(0.0),
            bid_depth_10bps: book.bid_depth_within_bps(10.0),
            ask_depth_10bps: book.ask_depth_within_bps(10.0),
            spread_bps: book.spread_bps().unwrap_or(0.0),
            mid: book.mid().unwrap_or(0.0),
        };

        let mut buffers = self.book_buffers.lock().await;
        if let Some(buf) = buffers.get_mut(&record.coin) {
            buf.push(record);
        }
    }

    /// Record trade entries.
    pub async fn record_trades(&self, coin: &str, entries: &[TapeEntry]) {
        if !self.enabled {
            return;
        }
        let mut buffers = self.trade_buffers.lock().await;
        if let Some(buf) = buffers.get_mut(coin) {
            for entry in entries {
                buf.push(TradeRecord {
                    timestamp: entry.timestamp,
                    coin: coin.to_string(),
                    price: entry.price,
                    size: entry.size,
                    is_buy: entry.is_buy,
                });
            }
        }
    }

    /// Flush all buffers to disk (JSONL files).
    pub async fn flush(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        let date_str = Utc::now().format("%Y-%m-%d").to_string();

        // Flush book records
        {
            let mut buffers = self.book_buffers.lock().await;
            for (coin, records) in buffers.iter_mut() {
                if records.is_empty() {
                    continue;
                }
                let dir = self.data_dir.join("l2").join(coin);
                std::fs::create_dir_all(&dir)?;
                let path = dir.join(format!("{}.jsonl", date_str));
                let file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)?;
                let mut writer = std::io::BufWriter::new(file);
                for record in records.drain(..) {
                    serde_json::to_writer(&mut writer, &record)?;
                    std::io::Write::write_all(&mut writer, b"\n")?;
                }
                info!("[RECORDER] Flushed L2 data for {} to {}", coin, path.display());
            }
        }

        // Flush trade records
        {
            let mut buffers = self.trade_buffers.lock().await;
            for (coin, records) in buffers.iter_mut() {
                if records.is_empty() {
                    continue;
                }
                let dir = self.data_dir.join("trades").join(coin);
                std::fs::create_dir_all(&dir)?;
                let path = dir.join(format!("{}.jsonl", date_str));
                let file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)?;
                let mut writer = std::io::BufWriter::new(file);
                for record in records.drain(..) {
                    serde_json::to_writer(&mut writer, &record)?;
                    std::io::Write::write_all(&mut writer, b"\n")?;
                }
                info!("[RECORDER] Flushed trade data for {} to {}", coin, path.display());
            }
        }

        Ok(())
    }
}
