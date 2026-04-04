use std::collections::VecDeque;
use std::sync::Arc;

use dashmap::DashMap;

use crate::features::book_features::{self, BookFeatures, SpreadAverage};
use crate::features::flow_features::{self, FlowFeatures};
use crate::market_data::book_manager::BookManager;

/// Computed features per coin, kept up to date by the engine.
#[derive(Debug, Clone, Default)]
pub struct CoinFeatures {
    pub book: BookFeatures,
    pub flow: FlowFeatures,
    pub timestamp: i64,
}

/// Number of vol_compression samples to track for squeeze detection (EVO-2).
const VOL_COMPRESSION_LOOKBACK: usize = 5;

/// Orchestrates feature computation for all coins.
pub struct FeatureEngine {
    pub features: DashMap<String, CoinFeatures>,
    spread_averages: DashMap<String, SpreadAverage>,
    /// Ring buffer of recent vol_compression values per coin (EVO-2: squeeze detection).
    vol_compression_history: DashMap<String, VecDeque<f64>>,
}

impl FeatureEngine {
    pub fn new(coins: &[String]) -> Arc<Self> {
        let engine = Arc::new(Self {
            features: DashMap::new(),
            spread_averages: DashMap::new(),
            vol_compression_history: DashMap::new(),
        });

        for coin in coins {
            engine.features.insert(coin.clone(), CoinFeatures::default());
            engine
                .spread_averages
                .insert(coin.clone(), SpreadAverage::new(1000));
            engine
                .vol_compression_history
                .insert(coin.clone(), VecDeque::with_capacity(VOL_COMPRESSION_LOOKBACK + 1));
        }

        engine
    }

    /// Recompute all features for a given coin from the current book/tape state.
    pub fn update(&self, coin: &str, book_mgr: &BookManager, now_ms: i64) {
        // Book features
        let book_feats = if let Some(book) = book_mgr.books.get(coin) {
            if let Some(mut spread_avg) = self.spread_averages.get_mut(coin) {
                book_features::compute_book_features(&book, &mut spread_avg)
            } else {
                BookFeatures::default()
            }
        } else {
            BookFeatures::default()
        };

        // Flow features — pass cancel_add_ratio from BookManager delta stats
        let cancel_add_ratio = book_mgr.get_cancel_add_ratio(coin);
        let mut flow_feats = if let Some(tape) = book_mgr.tapes.get(coin) {
            flow_features::compute_flow_features(&tape, now_ms, cancel_add_ratio)
        } else {
            FlowFeatures::default()
        };

        // EVO-2: Track vol_compression history and compute vol_expanding
        if let Some(mut history) = self.vol_compression_history.get_mut(coin) {
            history.push_back(flow_feats.vol_compression);
            while history.len() > VOL_COMPRESSION_LOOKBACK {
                history.pop_front();
            }
            // vol_expanding = true if the last N samples show a monotonically increasing trend
            if history.len() >= 3 {
                let expanding = history.iter()
                    .zip(history.iter().skip(1))
                    .all(|(prev, curr)| curr > prev);
                flow_feats.vol_expanding = expanding;
            }
        }

        self.features.insert(
            coin.to_string(),
            CoinFeatures {
                book: book_feats,
                flow: flow_feats,
                timestamp: now_ms,
            },
        );
    }

    /// Get current features for a coin.
    pub fn get(&self, coin: &str) -> Option<CoinFeatures> {
        self.features.get(coin).map(|f| f.clone())
    }
}
