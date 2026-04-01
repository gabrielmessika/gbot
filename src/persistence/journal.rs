use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use tracing::{info, warn};

/// JSONL journal for orders and events (debug/audit fallback).
/// Thread-safe: wraps file access in a Mutex so it can be shared across tasks.
pub struct Journal {
    path: PathBuf,
    lock: Mutex<()>,
}

impl Journal {
    pub fn new(data_dir: &str) -> Result<Self> {
        let dir = Path::new(data_dir).join("journal");
        fs::create_dir_all(&dir)?;

        let ts = Utc::now().format("%Y-%m-%d_%H-%M-%S");
        let path = dir.join(format!("journal_{}.jsonl", ts));

        info!("[JOURNAL] Writing to {}", path.display());
        Ok(Self {
            path,
            lock: Mutex::new(()),
        })
    }

    /// Append a serializable event to the journal.
    pub fn write<T: Serialize>(&self, event: &T) -> Result<()> {
        let _guard = self.lock.lock().map_err(|e| anyhow::anyhow!("journal lock: {}", e))?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, event)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        Ok(())
    }

    /// Best-effort write — logs a warning on failure instead of propagating.
    pub fn log_event<T: Serialize>(&self, event: &T) {
        if let Err(e) = self.write(event) {
            warn!("[JOURNAL] Failed to write event: {}", e);
        }
    }
}

/// Structured journal event types.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event_type")]
pub enum JournalEvent {
    OrderPlaced {
        ts_local: i64,
        coin: String,
        direction: String,
        price: String,
        size: String,
        tif: String,
        client_oid: String,
    },
    OrderFilled {
        ts_local: i64,
        ts_exchange: Option<i64>,
        coin: String,
        oid: String,
        fill_price: String,
        fill_size: String,
        latency_ms: Option<i64>,
    },
    OrderCancelled {
        ts_local: i64,
        coin: String,
        oid: String,
        reason: String,
    },
    OrderRejected {
        ts_local: i64,
        coin: String,
        oid: String,
        error: String,
    },
    PositionOpened {
        ts_local: i64,
        coin: String,
        direction: String,
        entry_price: String,
        stop_loss: String,
        take_profit: String,
        size: String,
        leverage: u32,
    },
    PositionClosed {
        ts_local: i64,
        coin: String,
        direction: String,
        entry_price: String,
        exit_price: String,
        pnl: String,
        reason: String,
    },
    BreakEvenApplied {
        ts_local: i64,
        coin: String,
        new_sl: String,
    },
    TrailingUpdate {
        ts_local: i64,
        coin: String,
        tier: u8,
        new_sl: String,
    },
    KillSwitchActivated {
        ts_local: i64,
        reason: String,
    },
    RiskRejection {
        ts_local: i64,
        coin: String,
        reasons: Vec<String>,
    },
}
