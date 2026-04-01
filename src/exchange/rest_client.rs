use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use reqwest::Client;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use tracing::error;

use crate::config::settings::ExchangeSettings;
use crate::exchange::rate_limiter::RateLimiter;
use crate::exchange::signer::HyperliquidSigner;

/// Time-in-force for orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tif {
    /// Add Liquidity Only — rejected if it would cross the book.
    Alo,
    /// Immediate Or Cancel — fills what it can, cancels the rest.
    Ioc,
    /// Good Till Cancel.
    Gtc,
}

/// Order type for exchange requests.
#[derive(Debug, Clone)]
pub struct OrderRequest {
    pub coin: String,
    /// Numeric asset index from the exchange universe (CoinMeta.asset_index).
    pub asset_index: u32,
    pub is_buy: bool,
    pub price: Decimal,
    pub size: Decimal,
    pub tif: Tif,
    pub reduce_only: bool,
    pub client_oid: Option<String>,
}

/// Result from placing an order.
#[derive(Debug, Clone)]
pub struct OrderResult {
    pub oid: Option<String>,
    pub status: String,
    pub error: Option<String>,
}

/// Position info from the exchange.
#[derive(Debug, Clone)]
pub struct ExchangePosition {
    pub coin: String,
    pub size: Decimal,
    pub entry_price: Decimal,
    pub unrealized_pnl: Decimal,
    pub leverage: u32,
    pub liquidation_price: Option<Decimal>,
}

/// REST client for Hyperliquid /info and /exchange endpoints.
pub struct RestClient {
    client: Client,
    settings: ExchangeSettings,
    signer: Arc<HyperliquidSigner>,
    rate_limiter: Arc<RateLimiter>,
}

impl RestClient {
    pub fn new(
        settings: ExchangeSettings,
        signer: Arc<HyperliquidSigner>,
        rate_limiter: Arc<RateLimiter>,
    ) -> Result<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_millis(settings.timeouts.connect_ms))
            .timeout(Duration::from_millis(settings.timeouts.read_ms))
            .build()?;

        Ok(Self {
            client,
            settings,
            signer,
            rate_limiter,
        })
    }

    fn info_url(&self) -> String {
        format!("{}/info", self.settings.rest_url)
    }

    fn exchange_url(&self) -> String {
        format!("{}/exchange", self.settings.rest_url)
    }

    // ─── Info endpoints ───

    /// Fetch exchange metadata (universe of perpetuals).
    pub async fn fetch_meta(&self) -> Result<Value> {
        self.rate_limiter.acquire_info_heavy().await?;
        let body = json!({"type": "meta"});
        let resp = self.client.post(&self.info_url()).json(&body).send().await?;
        let data: Value = resp.json().await?;
        Ok(data)
    }

    /// Fetch all mid prices.
    pub async fn fetch_all_mids(&self) -> Result<Value> {
        self.rate_limiter.acquire_info_light().await?;
        let body = json!({"type": "allMids"});
        let resp = self.client.post(&self.info_url()).json(&body).send().await?;
        let data: Value = resp.json().await?;
        Ok(data)
    }

    /// Fetch clearinghouse state (account info, positions, margin).
    pub async fn fetch_clearinghouse_state(&self) -> Result<Value> {
        self.rate_limiter.acquire_info_light().await?;
        let body = json!({
            "type": "clearinghouseState",
            "user": self.settings.wallet_address
        });
        let resp = self.client.post(&self.info_url()).json(&body).send().await?;
        let data: Value = resp.json().await?;
        Ok(data)
    }

    /// Fetch open orders for our wallet.
    pub async fn fetch_open_orders(&self) -> Result<Value> {
        self.rate_limiter.acquire_info_heavy().await?;
        let body = json!({
            "type": "openOrders",
            "user": self.settings.wallet_address
        });
        let resp = self.client.post(&self.info_url()).json(&body).send().await?;
        let data: Value = resp.json().await?;
        Ok(data)
    }

    /// Fetch frontend open orders (includes trigger orders).
    pub async fn fetch_frontend_open_orders(&self) -> Result<Value> {
        self.rate_limiter.acquire_info_heavy().await?;
        let body = json!({
            "type": "frontendOpenOrders",
            "user": self.settings.wallet_address
        });
        let resp = self.client.post(&self.info_url()).json(&body).send().await?;
        let data: Value = resp.json().await?;
        Ok(data)
    }

    /// Fetch L2 book snapshot for a coin.
    pub async fn fetch_l2_snapshot(&self, coin: &str) -> Result<Value> {
        self.rate_limiter.acquire_info_heavy().await?;
        let body = json!({
            "type": "l2Book",
            "coin": coin
        });
        let resp = self.client.post(&self.info_url()).json(&body).send().await?;
        let data: Value = resp.json().await?;
        Ok(data)
    }

    /// Fetch recent fills for a user.
    pub async fn fetch_user_fills(&self, start_time_ms: u64) -> Result<Value> {
        self.rate_limiter.acquire_info_heavy().await?;
        let body = json!({
            "type": "userFillsByTime",
            "user": self.settings.wallet_address,
            "startTime": start_time_ms
        });
        let resp = self.client.post(&self.info_url()).json(&body).send().await?;
        let data: Value = resp.json().await?;
        Ok(data)
    }

    // ─── Exchange endpoints ───

    /// Place a single order on the exchange.
    pub async fn place_order(&self, req: &OrderRequest) -> Result<OrderResult> {
        self.rate_limiter.acquire_order().await?;

        let tif_json = match req.tif {
            Tif::Alo => json!({"Alo": true}),
            Tif::Ioc => json!({"Ioc": true}),
            Tif::Gtc => json!({"Gtc": true}),
        };

        let order_spec = json!({
            "a": req.asset_index,
            "b": req.is_buy,
            "p": req.price.to_string(),
            "s": req.size.to_string(),
            "r": req.reduce_only,
            "t": tif_json,
            "c": req.client_oid.as_deref().unwrap_or(""),
        });

        let action = json!({
            "type": "order",
            "orders": [order_spec],
            "grouping": "na"
        });

        let payload = self.signer.build_signed_request(action, None)?;
        let resp = self.client.post(&self.exchange_url()).json(&payload).send().await?;
        let data: Value = resp.json().await?;

        self.parse_order_response(&data)
    }

    /// Cancel an order by OID.
    pub async fn cancel_order(&self, _coin: &str, oid: &str, asset_index: u32) -> Result<OrderResult> {
        self.rate_limiter.acquire_order().await?;

        let action = json!({
            "type": "cancel",
            "cancels": [{
                "a": asset_index,
                "o": oid,
            }]
        });

        let payload = self.signer.build_signed_request(action, None)?;
        let resp = self.client.post(&self.exchange_url()).json(&payload).send().await?;
        let data: Value = resp.json().await?;

        self.parse_order_response(&data)
    }

    /// Amend an existing order (change price only, preserves queue position).
    pub async fn amend_order(&self, oid: &str, new_price: Decimal, new_size: Decimal) -> Result<OrderResult> {
        self.rate_limiter.acquire_order().await?;

        let action = json!({
            "type": "batchModify",
            "modifies": [{
                "oid": oid,
                "order": {
                    "p": new_price.to_string(),
                    "s": new_size.to_string(),
                }
            }]
        });

        let payload = self.signer.build_signed_request(action, None)?;
        let resp = self.client.post(&self.exchange_url()).json(&payload).send().await?;
        let data: Value = resp.json().await?;

        self.parse_order_response(&data)
    }

    /// Place a trigger order (TP or SL) as a safety net.
    pub async fn place_trigger_order(
        &self,
        coin: &str,
        is_buy: bool,
        trigger_price: Decimal,
        size: Decimal,
        is_tp: bool,
        asset_index: u32,
    ) -> Result<OrderResult> {
        self.rate_limiter.acquire_order().await?;

        let tpsl = if is_tp { "tp" } else { "sl" };
        let action = json!({
            "type": "order",
            "orders": [{
                "a": asset_index,
                "b": is_buy,
                "p": trigger_price.to_string(),
                "s": size.to_string(),
                "r": true,
                "t": {
                    "trigger": {
                        "triggerPx": trigger_price.to_string(),
                        "isMarket": true,
                        "tpsl": tpsl,
                    }
                },
                "c": "",
            }],
            "grouping": "na"
        });

        let payload = self.signer.build_signed_request(action, None)?;
        let resp = self.client.post(&self.exchange_url()).json(&payload).send().await?;
        let data: Value = resp.json().await?;

        let result = self.parse_order_response(&data)?;

        // Check for trigger order errors (t-bot bug #4)
        if let Some(err) = &result.error {
            error!("[EXCHANGE] Trigger order error for {}: {}", coin, err);
        }

        Ok(result)
    }

    /// Parse a standard Hyperliquid order response.
    fn parse_order_response(&self, data: &Value) -> Result<OrderResult> {
        if let Some(status) = data.get("status").and_then(|s| s.as_str()) {
            if status == "err" {
                let err_msg = data
                    .get("response")
                    .and_then(|r| r.as_str())
                    .unwrap_or("unknown error")
                    .to_string();
                return Ok(OrderResult {
                    oid: None,
                    status: "error".to_string(),
                    error: Some(err_msg),
                });
            }
        }

        // Extract statuses from response
        if let Some(response) = data.get("response") {
            if let Some(data_obj) = response.get("data") {
                if let Some(statuses) = data_obj.get("statuses") {
                    if let Some(arr) = statuses.as_array() {
                        if let Some(first) = arr.first() {
                            if let Some(err) = first.get("error").and_then(|e| e.as_str()) {
                                return Ok(OrderResult {
                                    oid: None,
                                    status: "error".to_string(),
                                    error: Some(err.to_string()),
                                });
                            }
                            let oid = first
                                .get("resting")
                                .and_then(|r| r.get("oid"))
                                .or_else(|| first.get("filled").and_then(|f| f.get("oid")))
                                .and_then(|o| o.as_str())
                                .map(|s| s.to_string());

                            let fill_status = if first.get("filled").is_some() {
                                "filled"
                            } else if first.get("resting").is_some() {
                                "resting"
                            } else {
                                "unknown"
                            };

                            return Ok(OrderResult {
                                oid,
                                status: fill_status.to_string(),
                                error: None,
                            });
                        }
                    }
                }
            }
        }

        Ok(OrderResult {
            oid: None,
            status: "unknown".to_string(),
            error: Some(format!("Unexpected response: {}", data)),
        })
    }

    // ─── Position helpers ───

    /// Get open positions from exchange. NEVER return empty on error (t-bot bug #13).
    pub async fn get_open_positions(&self) -> Result<Vec<ExchangePosition>> {
        let state = self.fetch_clearinghouse_state().await?;

        let positions = state
            .get("assetPositions")
            .and_then(|p| p.as_array())
            .ok_or_else(|| anyhow::anyhow!("Missing assetPositions in clearinghouse state"))?;

        let mut result = Vec::new();
        for pos in positions {
            let position_obj = pos.get("position").unwrap_or(pos);
            let coin = position_obj
                .get("coin")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            let size_str = position_obj
                .get("szi")
                .and_then(|s| s.as_str())
                .unwrap_or("0");
            let size: Decimal = size_str.parse().unwrap_or_default();

            if size == Decimal::ZERO {
                continue;
            }

            let entry_price: Decimal = position_obj
                .get("entryPx")
                .and_then(|s| s.as_str())
                .unwrap_or("0")
                .parse()
                .unwrap_or_default();
            let unrealized_pnl: Decimal = position_obj
                .get("unrealizedPnl")
                .and_then(|s| s.as_str())
                .unwrap_or("0")
                .parse()
                .unwrap_or_default();
            let leverage: u32 = position_obj
                .get("leverage")
                .and_then(|l| l.get("value"))
                .and_then(|v| v.as_u64())
                .unwrap_or(1) as u32;

            result.push(ExchangePosition {
                coin,
                size,
                entry_price,
                unrealized_pnl,
                leverage,
                liquidation_price: None,
            });
        }

        Ok(result)
    }

    /// Get available balance from exchange.
    pub async fn get_available_balance(&self) -> Result<Decimal> {
        let state = self.fetch_clearinghouse_state().await?;

        let balance: Decimal = state
            .get("marginSummary")
            .and_then(|m| m.get("accountValue"))
            .and_then(|v| v.as_str())
            .unwrap_or("0")
            .parse()
            .unwrap_or_default();

        Ok(balance)
    }

    /// Get full equity (not available balance — for drawdown calculation).
    /// Uses a single API call (t-bot bug #12 / tbot-scalp race condition fix).
    pub async fn get_equity(&self) -> Result<Decimal> {
        let state = self.fetch_clearinghouse_state().await?;

        // accountValue = equity (balance + unrealized PnL)
        let equity: Decimal = state
            .get("marginSummary")
            .and_then(|m| m.get("accountValue"))
            .and_then(|v| v.as_str())
            .unwrap_or("0")
            .parse()
            .unwrap_or_default();

        Ok(equity)
    }
}
