use std::collections::HashMap;

use anyhow::Result;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

use crate::config::coins::{round_price_to_tick, round_size_to_lot, CoinMetaStore};
use crate::config::settings::{BotMode, ExecutionSettings};
use crate::exchange::rest_client::{OrderRequest, OrderResult, RestClient, Tif};
use crate::strategy::signal::{Direction, Intent};

/// State of a pending order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingOrder {
    pub oid: String,
    pub client_oid: String,
    pub coin: String,
    pub direction: Direction,
    pub price: Decimal,
    pub size: Decimal,
    /// Stop-loss price — stored so on_fill() can produce FilledEntry without extra lookup.
    pub stop_loss: Decimal,
    /// Take-profit price — same rationale.
    pub take_profit: Decimal,
    pub leverage: u32,
    pub filled_qty: Decimal,
    pub placed_at: i64,
    pub max_wait_s: u64,
    pub status: PendingOrderStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PendingOrderStatus {
    Working,
    PartialFill,
    Filled,
    Cancelled,
    Rejected,
}

/// Tracks a reduce-only close order (ForceExitIoc / PlacePassiveExit).
#[derive(Debug, Clone)]
struct CloseOrder {
    coin: String,
    reason: String,
}

/// Emitted by on_fill() when an entry order is fully filled.
#[derive(Debug, Clone)]
pub struct FilledEntry {
    pub coin: String,
    pub direction: Direction,
    /// Actual fill price from the exchange (used for SL/TP adjustment).
    pub fill_price: Decimal,
    pub size: Decimal,
    /// Stop-loss — adjusted proportionally if fill_price deviated > 0.5% (t-bot bug #2).
    pub stop_loss: Decimal,
    /// Take-profit — same adjustment.
    pub take_profit: Decimal,
    pub leverage: u32,
    pub client_oid: String,
}

/// Emitted by on_fill() when a close/exit order is fully filled.
#[derive(Debug, Clone)]
pub struct ClosedEntry {
    pub coin: String,
    pub fill_price: Decimal,
    pub size: Decimal,
    pub reason: String,
}

/// A fill event returned by on_fill().
#[derive(Debug, Clone)]
pub enum FillEvent {
    EntryFilled(FilledEntry),
    ExitFilled(ClosedEntry),
}

/// Trade state machine per coin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TradeState {
    Flat,
    SetupDetected { signal_ts: i64 },
    WaitingPullback { signal_ts: i64, expires_at: i64 },
    EntryWorking { oid: String, order: PendingOrder },
    EntryPartial { oid: String, filled_qty: Decimal, total_qty: Decimal },
    InPosition,
    ExitWorking { oid: String },
    ExitPartial { oid: String, remaining_qty: Decimal },
    ForceExit { reason: String },
    ErrorRecovery { since: i64, last_error: String },
    SafeMode,
}

/// Manages order placement, amendment, cancellation and state transitions.
pub struct OrderManager {
    states: HashMap<String, TradeState>,
    pending_orders: HashMap<String, PendingOrder>,
    /// OID → CloseOrder for reduce-only exit orders.
    close_orders: HashMap<String, CloseOrder>,
    sequence: u64,
    session_date: String,
    mode: BotMode,
    settings: ExecutionSettings,
}

impl OrderManager {
    pub fn new(mode: BotMode, settings: ExecutionSettings) -> Self {
        let session_date = chrono::Utc::now().format("%Y%m%d").to_string();
        Self {
            states: HashMap::new(),
            pending_orders: HashMap::new(),
            close_orders: HashMap::new(),
            sequence: 0,
            session_date,
            mode,
            settings,
        }
    }

    /// Get the trade state for a coin.
    pub fn state(&self, coin: &str) -> &TradeState {
        self.states.get(coin).unwrap_or(&TradeState::Flat)
    }

    /// Process an intent from the strategy, converting it into exchange actions.
    pub async fn process_intent(
        &mut self,
        intent: Intent,
        rest: &RestClient,
        meta_store: &CoinMetaStore,
    ) -> Result<()> {
        match intent {
            Intent::NoTrade => {}

            Intent::PlacePassiveEntry {
                coin,
                direction,
                price,
                stop_loss,
                take_profit,
                size,
                max_wait_s,
            } => {
                // Check: no duplicate position on same coin
                if !matches!(self.state(&coin), TradeState::Flat) {
                    debug!("[ORDER] Skipping {}: not flat (state={:?})", coin, self.state(&coin));
                    return Ok(());
                }

                if size <= Decimal::ZERO {
                    warn!("[ORDER] Size is 0 for {} — skipping (risk manager did not compute size?)", coin);
                    return Ok(());
                }

                let meta = meta_store
                    .get(&coin)
                    .ok_or_else(|| anyhow::anyhow!("No metadata for {}", coin))?;

                let price = round_price_to_tick(price, meta.tick_size);
                let size = round_size_to_lot(size, meta.lot_size);

                if size <= Decimal::ZERO {
                    warn!("[ORDER] Size is 0 for {} after lot rounding — skipping", coin);
                    return Ok(());
                }

                let client_oid = self.next_client_oid(&coin, "entry");

                if self.mode == BotMode::Live {
                    let req = OrderRequest {
                        coin: coin.clone(),
                        asset_index: meta.asset_index,
                        is_buy: direction.is_buy(),
                        price,
                        size,
                        tif: Tif::Alo,
                        reduce_only: false,
                        client_oid: Some(client_oid.clone()),
                    };
                    let result = rest.place_order(&req).await?;
                    self.handle_order_result(
                        &coin, &client_oid, direction,
                        price, stop_loss, take_profit, size, max_wait_s, 0, result,
                    )?;
                } else {
                    info!(
                        "[ORDER] DRY-RUN: {} {} {} @ {} size={} sl={} tp={}",
                        if direction.is_buy() { "BUY" } else { "SELL" },
                        coin, client_oid, price, size, stop_loss, take_profit
                    );
                    let now = chrono::Utc::now().timestamp_millis();
                    let pending = PendingOrder {
                        oid: client_oid.clone(),
                        client_oid: client_oid.clone(),
                        coin: coin.clone(),
                        direction,
                        price,
                        size,
                        stop_loss,
                        take_profit,
                        leverage: 1, // dry-run default
                        filled_qty: Decimal::ZERO,
                        placed_at: now,
                        max_wait_s,
                        status: PendingOrderStatus::Working,
                    };
                    self.pending_orders.insert(client_oid.clone(), pending.clone());
                    self.states.insert(
                        coin,
                        TradeState::EntryWorking {
                            oid: client_oid,
                            order: pending,
                        },
                    );
                }
            }

            Intent::AmendPassiveEntry { oid, new_price } => {
                if self.mode == BotMode::Live {
                    if let Some(order) = self.pending_orders.get(&oid) {
                        let result = rest.amend_order(&oid, new_price, order.size).await?;
                        if let Some(err) = &result.error {
                            warn!("[ORDER] Amend failed for {}: {}", oid, err);
                        } else {
                            if let Some(order) = self.pending_orders.get_mut(&oid) {
                                order.price = new_price;
                            }
                            info!("[ORDER] Amended {} to price {}", oid, new_price);
                        }
                    }
                }
            }

            Intent::CancelEntry { oid, reason } => {
                info!("[ORDER] Cancelling {} — reason: {}", oid, reason);
                if self.mode == BotMode::Live {
                    if let Some(order) = self.pending_orders.get(&oid) {
                        // asset_index needed for cancel; fall back to 0 if meta unavailable
                        // (caller should ensure meta is loaded before trading)
                        let _ = rest.cancel_order(&order.coin, &oid, 0).await;
                    }
                }
                if let Some(order) = self.pending_orders.get(&oid) {
                    self.states.insert(order.coin.clone(), TradeState::Flat);
                }
                self.pending_orders.remove(&oid);
            }

            Intent::ForceExitIoc {
                coin,
                direction,
                mid_price,
                size,
                reason,
            } => {
                warn!("[ORDER] Force exit {} — direction={:?} reason: {}", coin, direction, reason);
                if self.mode == BotMode::Live {
                    let meta = meta_store
                        .get(&coin)
                        .ok_or_else(|| anyhow::anyhow!("No metadata for {}", coin))?;
                    let size = round_size_to_lot(size, meta.lot_size);
                    if size <= Decimal::ZERO {
                        warn!("[ORDER] Force exit size=0 for {} after rounding", coin);
                    } else {
                        // Exit is OPPOSITE of entry direction
                        let is_buy = !direction.is_buy();
                        // IOC limit: mid ± 0.5% slippage to guarantee fill
                        let slippage = Decimal::try_from(0.005).unwrap_or_default();
                        let limit_price = if is_buy {
                            round_price_to_tick(mid_price * (Decimal::ONE + slippage), meta.tick_size)
                        } else {
                            round_price_to_tick(mid_price * (Decimal::ONE - slippage), meta.tick_size)
                        };
                        let client_oid = self.next_client_oid(&coin, "exit");
                        let req = OrderRequest {
                            coin: coin.clone(),
                            asset_index: meta.asset_index,
                            is_buy,
                            price: limit_price,
                            size,
                            tif: Tif::Ioc,
                            reduce_only: true,
                            client_oid: Some(client_oid.clone()),
                        };
                        match rest.place_order(&req).await {
                            Ok(result) => {
                                if let Some(oid) = result.oid {
                                    self.close_orders.insert(oid, CloseOrder {
                                        coin: coin.clone(),
                                        reason: reason.clone(),
                                    });
                                }
                                if let Some(err) = result.error {
                                    error!("[ORDER] Force exit error for {}: {}", coin, err);
                                }
                            }
                            Err(e) => error!("[ORDER] Force exit failed for {}: {}", coin, e),
                        }
                    }
                }
                self.states.insert(coin, TradeState::ForceExit { reason });
            }

            Intent::PlacePassiveExit {
                coin,
                direction,
                price,
                size,
            } => {
                info!("[ORDER] Passive exit {} @ {} direction={:?}", coin, price, direction);
                if self.mode == BotMode::Live {
                    let meta = meta_store
                        .get(&coin)
                        .ok_or_else(|| anyhow::anyhow!("No metadata for {}", coin))?;
                    let client_oid = self.next_client_oid(&coin, "tp");
                    // Exit is OPPOSITE of entry direction
                    let is_buy = !direction.is_buy();
                    let req = OrderRequest {
                        coin: coin.clone(),
                        asset_index: meta.asset_index,
                        is_buy,
                        price: round_price_to_tick(price, meta.tick_size),
                        size: round_size_to_lot(size, meta.lot_size),
                        tif: Tif::Gtc,
                        reduce_only: true,
                        client_oid: Some(client_oid.clone()),
                    };
                    match rest.place_order(&req).await {
                        Ok(result) => {
                            if let Some(oid) = result.oid {
                                self.close_orders.insert(oid, CloseOrder {
                                    coin: coin.clone(),
                                    reason: "passive_exit".to_string(),
                                });
                            }
                        }
                        Err(e) => error!("[ORDER] Passive exit failed for {}: {}", coin, e),
                    }
                }
            }

            Intent::ReducePosition { coin, size } => {
                info!("[ORDER] Reduce position {} by {}", coin, size);
            }

            Intent::Cooldown { coin, duration_s } => {
                info!("[ORDER] Cooldown {} for {}s", coin, duration_s);
            }
        }

        Ok(())
    }

    /// Handle an order result from the exchange after placing an entry order.
    fn handle_order_result(
        &mut self,
        coin: &str,
        client_oid: &str,
        direction: Direction,
        price: Decimal,
        stop_loss: Decimal,
        take_profit: Decimal,
        size: Decimal,
        max_wait_s: u64,
        leverage: u32,
        result: OrderResult,
    ) -> Result<()> {
        if let Some(err) = &result.error {
            warn!("[ORDER] Order rejected for {}: {}", coin, err);
            self.states.insert(coin.to_string(), TradeState::Flat);
            return Ok(());
        }

        let oid = result.oid.unwrap_or_else(|| client_oid.to_string());
        let now = chrono::Utc::now().timestamp_millis();

        let pending = PendingOrder {
            oid: oid.clone(),
            client_oid: client_oid.to_string(),
            coin: coin.to_string(),
            direction,
            price,
            size,
            stop_loss,
            take_profit,
            leverage,
            filled_qty: Decimal::ZERO,
            placed_at: now,
            max_wait_s,
            status: if result.status == "filled" {
                PendingOrderStatus::Filled
            } else {
                PendingOrderStatus::Working
            },
        };

        self.pending_orders.insert(oid.clone(), pending.clone());

        let new_state = if result.status == "filled" {
            info!("[ORDER] Immediate fill for {} oid={}", coin, oid);
            TradeState::InPosition
        } else {
            info!("[ORDER] Order resting for {} oid={}", coin, oid);
            TradeState::EntryWorking { oid: oid.clone(), order: pending }
        };

        self.states.insert(coin.to_string(), new_state);
        Ok(())
    }

    /// Handle a fill event from WS orderUpdates.
    ///
    /// Returns:
    /// - `Some(FillEvent::EntryFilled(_))` when an entry order is fully filled
    /// - `Some(FillEvent::ExitFilled(_))` when a close order is fully filled
    /// - `None` for partial fills or unknown OIDs
    pub fn on_fill(&mut self, oid: &str, fill_price: Decimal, filled_qty: Decimal) -> Option<FillEvent> {
        // Check if it's a close order
        if let Some(close) = self.close_orders.remove(oid) {
            info!(
                "[ORDER] Exit fill: {} oid={} qty={} @ {} reason={}",
                close.coin, oid, filled_qty, fill_price, close.reason
            );
            self.states.insert(close.coin.clone(), TradeState::Flat);
            return Some(FillEvent::ExitFilled(ClosedEntry {
                coin: close.coin,
                fill_price,
                size: filled_qty,
                reason: close.reason,
            }));
        }

        // Check if it's an entry order
        if let Some(order) = self.pending_orders.get_mut(oid) {
            order.filled_qty += filled_qty;
            info!(
                "[ORDER] Entry fill: {} {} qty={} @ {} (total={}/{})",
                order.coin, oid, filled_qty, fill_price, order.filled_qty, order.size
            );

            if order.filled_qty >= order.size {
                order.status = PendingOrderStatus::Filled;
                self.states.insert(order.coin.clone(), TradeState::InPosition);

                // Build FilledEntry, adjusting SL/TP if fill price deviated (t-bot bug #2)
                let order = self.pending_orders.remove(oid).unwrap();
                let (adj_sl, adj_tp) = adjust_levels_for_fill(
                    order.direction, order.price, fill_price,
                    order.stop_loss, order.take_profit,
                );

                Some(FillEvent::EntryFilled(FilledEntry {
                    coin: order.coin,
                    direction: order.direction,
                    fill_price,
                    size: order.size,
                    stop_loss: adj_sl,
                    take_profit: adj_tp,
                    leverage: order.leverage,
                    client_oid: order.client_oid,
                }))
            } else {
                order.status = PendingOrderStatus::PartialFill;
                let coin = order.coin.clone();
                let fq = order.filled_qty;
                let tq = order.size;
                self.states.insert(
                    coin,
                    TradeState::EntryPartial { oid: oid.to_string(), filled_qty: fq, total_qty: tq },
                );
                None
            }
        } else {
            None
        }
    }

    /// Handle a cancel event from WS.
    pub fn on_cancel(&mut self, oid: &str) {
        self.close_orders.remove(oid);
        if let Some(order) = self.pending_orders.remove(oid) {
            info!("[ORDER] Cancelled: {} {}", order.coin, oid);
            self.states.insert(order.coin, TradeState::Flat);
        }
    }

    /// Handle a reject event from WS.
    pub fn on_reject(&mut self, oid: &str, error: Option<String>) {
        self.close_orders.remove(oid);
        if let Some(order) = self.pending_orders.remove(oid) {
            warn!("[ORDER] Rejected: {} {} — {:?}", order.coin, oid, error);
            self.states.insert(order.coin, TradeState::Flat);
        }
    }

    /// Generate a client OID: `mfdp-{coin}-{date}-{seq:06}-{intent}`.
    fn next_client_oid(&mut self, coin: &str, intent_type: &str) -> String {
        self.sequence += 1;
        format!(
            "mfdp-{}-{}-{:06}-{}",
            coin.to_lowercase(),
            self.session_date,
            self.sequence,
            intent_type
        )
    }

    /// Get all pending orders.
    pub fn pending_orders(&self) -> &HashMap<String, PendingOrder> {
        &self.pending_orders
    }

    /// Get all trade states per coin.
    pub fn all_states(&self) -> &HashMap<String, TradeState> {
        &self.states
    }

    /// Check for timed-out resting orders and return cancel intents.
    pub fn check_timeouts(&mut self, now_ms: i64) -> Vec<Intent> {
        let mut cancels = Vec::new();
        for (oid, order) in &self.pending_orders {
            if order.status == PendingOrderStatus::Working {
                let elapsed_s = (now_ms - order.placed_at) / 1000;
                if elapsed_s > order.max_wait_s as i64 {
                    cancels.push(Intent::CancelEntry {
                        oid: oid.clone(),
                        reason: format!("timeout after {}s", elapsed_s),
                    });
                }
            }
        }
        cancels
    }
}

/// Adjust SL/TP proportionally if actual fill deviated > 0.5% from intended price (t-bot bug #2).
fn adjust_levels_for_fill(
    direction: Direction,
    intended_price: Decimal,
    fill_price: Decimal,
    stop_loss: Decimal,
    take_profit: Decimal,
) -> (Decimal, Decimal) {
    if intended_price <= Decimal::ZERO {
        return (stop_loss, take_profit);
    }
    let drift = ((fill_price - intended_price).abs() / intended_price)
        .to_string()
        .parse::<f64>()
        .unwrap_or(0.0);

    if drift <= 0.005 {
        // Within 0.5% — no adjustment needed
        return (stop_loss, take_profit);
    }

    // Compute ratio and scale
    let ratio = fill_price / intended_price;
    let adj_sl = stop_loss * ratio;
    let adj_tp = take_profit * ratio;

    tracing::info!(
        "[ORDER] SL/TP adjusted for fill drift {:.2}%: sl {} → {} | tp {} → {}",
        drift * 100.0, stop_loss, adj_sl, take_profit, adj_tp
    );

    (adj_sl, adj_tp)
}
