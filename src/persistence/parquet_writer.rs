use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use arrow::array::{BooleanArray, Float64Array, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::arrow_writer::ArrowWriter;
use parquet::file::properties::WriterProperties;
use tracing::info;

use crate::market_data::recorder::{BookRecord, TradeRecord};

/// Converts JSONL recordings to Parquet for efficient offline analysis.
///
/// Phase 1 data is always JSONL (via Recorder).
/// Call `convert_*` at end-of-day to produce columnar Parquet alongside the JSONL files.
pub struct ParquetWriter {
    data_dir: PathBuf,
}

impl ParquetWriter {
    pub fn new(data_dir: &str) -> Self {
        Self {
            data_dir: PathBuf::from(data_dir),
        }
    }

    /// Convert a day's L2 book JSONL to Parquet.
    pub fn convert_book_jsonl(&self, coin: &str, date: &str) -> Result<()> {
        let input = self.data_dir.join("l2").join(coin).join(format!("{}.jsonl", date));
        let output = self.data_dir.join("l2").join(coin).join(format!("{}.parquet", date));

        if !input.exists() {
            return Ok(());
        }

        let records: Vec<BookRecord> = std::fs::read_to_string(&input)?
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();

        if records.is_empty() {
            return Ok(());
        }

        let schema = Arc::new(Schema::new(vec![
            Field::new("timestamp", DataType::Int64, false),
            Field::new("coin", DataType::Utf8, false),
            Field::new("best_bid", DataType::Float64, true),
            Field::new("best_ask", DataType::Float64, true),
            Field::new("bid_depth_10bps", DataType::Float64, true),
            Field::new("ask_depth_10bps", DataType::Float64, true),
            Field::new("spread_bps", DataType::Float64, true),
            Field::new("mid", DataType::Float64, true),
        ]));

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int64Array::from_iter_values(records.iter().map(|r| r.timestamp))) as _,
                Arc::new(StringArray::from(
                    records.iter().map(|r| r.coin.as_str()).collect::<Vec<_>>(),
                )) as _,
                Arc::new(Float64Array::from_iter_values(records.iter().map(|r| r.best_bid))) as _,
                Arc::new(Float64Array::from_iter_values(records.iter().map(|r| r.best_ask))) as _,
                Arc::new(Float64Array::from_iter_values(records.iter().map(|r| r.bid_depth_10bps))) as _,
                Arc::new(Float64Array::from_iter_values(records.iter().map(|r| r.ask_depth_10bps))) as _,
                Arc::new(Float64Array::from_iter_values(records.iter().map(|r| r.spread_bps))) as _,
                Arc::new(Float64Array::from_iter_values(records.iter().map(|r| r.mid))) as _,
            ],
        )?;

        let file = File::create(&output)?;
        let props = WriterProperties::builder().build();
        let mut writer = ArrowWriter::try_new(file, schema, Some(props))?;
        writer.write(&batch)?;
        writer.close()?;

        info!("[PARQUET] Converted L2 {}/{} → {} rows", coin, date, records.len());
        Ok(())
    }

    /// Convert a day's trade JSONL to Parquet.
    pub fn convert_trade_jsonl(&self, coin: &str, date: &str) -> Result<()> {
        let input = self.data_dir.join("trades").join(coin).join(format!("{}.jsonl", date));
        let output = self.data_dir.join("trades").join(coin).join(format!("{}.parquet", date));

        if !input.exists() {
            return Ok(());
        }

        let records: Vec<TradeRecord> = std::fs::read_to_string(&input)?
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();

        if records.is_empty() {
            return Ok(());
        }

        let schema = Arc::new(Schema::new(vec![
            Field::new("timestamp", DataType::Int64, false),
            Field::new("coin", DataType::Utf8, false),
            Field::new("price", DataType::Float64, false),
            Field::new("size", DataType::Float64, false),
            Field::new("is_buy", DataType::Boolean, false),
        ]));

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int64Array::from_iter_values(records.iter().map(|r| r.timestamp))) as _,
                Arc::new(StringArray::from(
                    records.iter().map(|r| r.coin.as_str()).collect::<Vec<_>>(),
                )) as _,
                Arc::new(Float64Array::from_iter_values(records.iter().map(|r| r.price))) as _,
                Arc::new(Float64Array::from_iter_values(records.iter().map(|r| r.size))) as _,
                Arc::new(BooleanArray::from(
                    records.iter().map(|r| r.is_buy).collect::<Vec<_>>(),
                )) as _,
            ],
        )?;

        let file = File::create(&output)?;
        let props = WriterProperties::builder().build();
        let mut writer = ArrowWriter::try_new(file, schema, Some(props))?;
        writer.write(&batch)?;
        writer.close()?;

        info!("[PARQUET] Converted trades {}/{} → {} rows", coin, date, records.len());
        Ok(())
    }
}
