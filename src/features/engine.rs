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

/// Orchestrates feature computation for all coins.
pub struct FeatureEngine {
    pub features: DashMap<String, CoinFeatures>,
    spread_averages: DashMap<String, SpreadAverage>,
}

impl FeatureEngine {
    pub fn new(coins: &[String]) -> Arc<Self> {
        let engine = Arc::new(Self {
            features: DashMap::new(),
            spread_averages: DashMap::new(),
        });

        for coin in coins {
            engine.features.insert(coin.clone(), CoinFeatures::default());
            engine
                .spread_averages
                .insert(coin.clone(), SpreadAverage::new(1000));
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
        let flow_feats = if let Some(tape) = book_mgr.tapes.get(coin) {
            flow_features::compute_flow_features(&tape, now_ms, cancel_add_ratio)
        } else {
            FlowFeatures::default()
        };

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
