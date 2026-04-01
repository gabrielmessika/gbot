use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use tracing::{info, warn};

/// Record of a signal emitted by the strategy, with full feature context.
#[derive(Debug, Clone, Serialize)]
pub struct SignalRecord {
    pub ts: i64,
    pub coin: String,
    pub direction: String,
    pub dir_score: f64,
    pub queue_score: f64,
    pub entry_price: String,
    pub stop_loss: String,
    pub take_profit: String,
    // Book features
    pub spread_bps: f64,
    pub imbalance_top5: f64,
    pub depth_ratio: f64,
    pub micro_price_vs_mid_bps: f64,
    pub vamp_signal_bps: f64,
    pub bid_depth_10bps: f64,
    pub ask_depth_10bps: f64,
    // Flow features
    pub ofi_10s: f64,
    pub toxicity: f64,
    pub vol_ratio: f64,
    pub aggression: f64,
    pub trade_intensity: f64,
    // Outcome
    pub action: String, // "placed", "risk_rejected", "cooldown_blocked"
    pub rejection_reason: Option<String>,
}

/// Writes signal records to `{data_dir}/signals/{date}.jsonl`.
pub struct SignalRecorder {
    data_dir: PathBuf,
    lock: Mutex<()>,
}

impl SignalRecorder {
    pub fn new(data_dir: &str) -> Result<Self> {
        let dir = Path::new(data_dir).join("signals");
        fs::create_dir_all(&dir)?;
        info!("[SIGNALS] Recording signals to {}", dir.display());
        Ok(Self {
            data_dir: dir,
            lock: Mutex::new(()),
        })
    }

    pub fn record(&self, record: &SignalRecord) {
        if let Err(e) = self.write_record(record) {
            warn!("[SIGNALS] Failed to write signal: {}", e);
        }
    }

    fn write_record(&self, record: &SignalRecord) -> Result<()> {
        let _guard = self.lock.lock().map_err(|e| anyhow::anyhow!("lock: {}", e))?;
        let date = Utc::now().format("%Y-%m-%d").to_string();
        let path = self.data_dir.join(format!("{}.jsonl", date));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, record)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        Ok(())
    }
}
