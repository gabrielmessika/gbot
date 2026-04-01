use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Metadata for a single Hyperliquid perpetual contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoinMeta {
    pub coin: String,
    pub asset_index: u32,
    pub tick_size: Decimal,
    pub lot_size: Decimal,
    pub max_leverage: u32,
    pub is_dex: bool,
}

/// In-memory store for all coin metadata, loaded from the exchange at startup.
#[derive(Debug, Clone, Default)]
pub struct CoinMetaStore {
    coins: HashMap<String, CoinMeta>,
}

impl CoinMetaStore {
    pub fn new() -> Self {
        Self {
            coins: HashMap::new(),
        }
    }

    pub fn insert(&mut self, meta: CoinMeta) {
        self.coins.insert(meta.coin.clone(), meta);
    }

    pub fn get(&self, coin: &str) -> Option<&CoinMeta> {
        self.coins.get(coin)
    }

    pub fn contains(&self, coin: &str) -> bool {
        self.coins.contains_key(coin)
    }

    pub fn all(&self) -> &HashMap<String, CoinMeta> {
        &self.coins
    }

    /// Build from the Hyperliquid /info meta response.
    pub fn from_exchange_meta(universe: &[serde_json::Value]) -> Self {
        let mut store = Self::new();
        for (idx, item) in universe.iter().enumerate() {
            if let Some(obj) = item.as_object() {
                let coin = obj
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let tick_size = obj
                    .get("szDecimals")
                    .and_then(|v| v.as_u64())
                    .map(|d| {
                        Decimal::new(1, d as u32)
                    })
                    .unwrap_or(Decimal::new(1, 4));
                let max_leverage = obj
                    .get("maxLeverage")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10) as u32;

                store.insert(CoinMeta {
                    coin,
                    asset_index: idx as u32,
                    tick_size,
                    lot_size: tick_size, // sz_decimals applies to both
                    max_leverage,
                    is_dex: false,
                });
            }
        }
        store
    }
}

/// Round a price to 5 significant figures (Hyperliquid price convention).
/// The `tick` parameter is kept for API compatibility but is not used:
/// Hyperliquid does not expose a price tick in its meta response — it enforces
/// 5 significant figures on prices. Using szDecimals (lot size) as a price tick
/// caused DOGE/XRP prices to round to 0 or 1 (szDecimals=0 → tick=1.0).
pub fn round_price_to_tick(price: Decimal, _tick: Decimal) -> Decimal {
    if price <= Decimal::ZERO {
        return price;
    }
    let mag = floor_log10(price);
    let dp = (4 - mag).max(0) as u32;
    price.round_dp(dp)
}

/// Returns floor(log10(price)) for positive prices.
/// Used to compute the number of decimal places for 5 significant figures.
fn floor_log10(price: Decimal) -> i32 {
    if price >= Decimal::ONE {
        let mut mag = 0i32;
        let mut p = price;
        while p >= Decimal::TEN {
            p /= Decimal::TEN;
            mag += 1;
        }
        mag
    } else {
        let mut mag = 0i32;
        let mut p = price;
        while p < Decimal::ONE {
            p *= Decimal::TEN;
            mag -= 1;
        }
        mag
    }
}

/// Round a size to the nearest valid lot (round down).
pub fn round_size_to_lot(size: Decimal, lot: Decimal) -> Decimal {
    (size / lot).floor() * lot
}

/// Validate an order's price/size before sending.
pub fn validate_order(coin: &str, price: Decimal, size: Decimal, meta: &CoinMeta) -> anyhow::Result<()> {
    if price <= Decimal::ZERO {
        anyhow::bail!("[{}] Invalid price: {}", coin, price);
    }
    if size <= Decimal::ZERO {
        anyhow::bail!("[{}] Invalid size: {}", coin, size);
    }
    let notional = price * size;
    if notional < Decimal::new(11, 0) {
        anyhow::bail!(
            "[{}] Notional ${} below Hyperliquid minimum $11",
            coin,
            notional
        );
    }
    // Verify tick alignment
    let rounded_price = round_price_to_tick(price, meta.tick_size);
    if rounded_price != price {
        anyhow::bail!(
            "[{}] Price {} not aligned to tick {}",
            coin,
            price,
            meta.tick_size
        );
    }
    let rounded_size = round_size_to_lot(size, meta.lot_size);
    if rounded_size != size {
        anyhow::bail!(
            "[{}] Size {} not aligned to lot {}",
            coin,
            size,
            meta.lot_size
        );
    }
    Ok(())
}
