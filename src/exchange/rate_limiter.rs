use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::Mutex;
use tokio::time::Instant;
use tracing::warn;

use crate::config::settings::RateLimitSettings;

/// Token-bucket rate limiter with real Hyperliquid weights.
/// Shared across all modules — a single instance globally.
pub struct RateLimiter {
    inner: Mutex<RateLimiterInner>,
    settings: RateLimitSettings,
}

struct RateLimiterInner {
    tokens: f64,
    last_refill: Instant,
}

impl RateLimiter {
    pub fn new(settings: RateLimitSettings) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(RateLimiterInner {
                tokens: settings.max_weight_per_minute as f64,
                last_refill: Instant::now(),
            }),
            settings,
        })
    }

    /// Refill tokens based on elapsed time (linearly).
    fn refill(inner: &mut RateLimiterInner, max_tokens: f64) {
        let now = Instant::now();
        let elapsed = now.duration_since(inner.last_refill).as_secs_f64();
        let refill = elapsed * (max_tokens / 60.0); // tokens per second
        inner.tokens = (inner.tokens + refill).min(max_tokens);
        inner.last_refill = now;
    }

    /// Acquire tokens for a given weight. Blocks until tokens are available.
    pub async fn acquire(&self, weight: u32) -> Result<()> {
        let w = weight as f64;
        let max_tokens = self.settings.max_weight_per_minute as f64;

        loop {
            {
                let mut inner = self.inner.lock().await;
                Self::refill(&mut inner, max_tokens);

                if inner.tokens >= w {
                    inner.tokens -= w;
                    return Ok(());
                }

                // Calculate how long to wait
                let deficit = w - inner.tokens;
                let wait_secs = deficit / (max_tokens / 60.0);
                drop(inner);

                if wait_secs > 5.0 {
                    warn!(
                        "[RATE_LIMITER] Waiting {:.1}s for {} weight (tokens depleted)",
                        wait_secs, weight
                    );
                }
                tokio::time::sleep(Duration::from_secs_f64(wait_secs.min(1.0))).await;
            }
        }
    }

    /// Acquire for an /info light call (allMids, clearinghouseState) — weight 2.
    pub async fn acquire_info_light(&self) -> Result<()> {
        self.acquire(self.settings.info_light_weight).await
    }

    /// Acquire for an /info heavy call (meta, openOrders) — weight 20.
    pub async fn acquire_info_heavy(&self) -> Result<()> {
        self.acquire(self.settings.info_heavy_weight).await
    }

    /// Acquire for a candle snapshot — weight 20 + candles/60.
    pub async fn acquire_candle(&self, estimated_candle_count: u32) -> Result<()> {
        let weight = self.settings.candle_base_weight + estimated_candle_count / 60;
        self.acquire(weight).await
    }

    /// Acquire for an /exchange order call — weight 1.
    pub async fn acquire_order(&self) -> Result<()> {
        self.acquire(self.settings.order_weight).await
    }
}
