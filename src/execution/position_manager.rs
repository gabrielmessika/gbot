use std::collections::HashMap;

use anyhow::Result;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::config::settings::{BotMode, ExecutionSettings};
use crate::exchange::rest_client::{ExchangePosition, RestClient};
use crate::strategy::signal::Direction;

/// An open position tracked internally.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenPosition {
    pub coin: String,
    pub direction: Direction,
    pub entry_price: Decimal,
    pub size: Decimal,
    pub stop_loss: Decimal,
    pub take_profit: Decimal,
    pub original_stop_loss: Decimal,
    pub leverage: u32,
    pub opened_at: i64,
    pub break_even_applied: bool,
    pub trailing_tier: u8,
    /// OID of the TP limit order (ALO maker) — NOT a trigger order.
    pub tp_order_oid: Option<String>,
    pub sl_trigger_oid: Option<String>,
    pub client_oid: String,
}

/// Manages the lifecycle of open positions.
pub struct PositionManager {
    positions: HashMap<String, OpenPosition>,
    /// coin → epoch-ms when cooldown expires
    cooldowns: HashMap<String, i64>,
    settings: ExecutionSettings,
}

impl PositionManager {
    pub fn new(settings: ExecutionSettings) -> Self {
        Self {
            positions: HashMap::new(),
            cooldowns: HashMap::new(),
            settings,
        }
    }

    /// Open a new position (without placing trigger orders).
    pub fn open_position(&mut self, position: OpenPosition) {
        info!(
            "[POSITION] Opened: {} {:?} entry={} sl={} tp={} size={} lev={}x",
            position.coin,
            position.direction,
            position.entry_price,
            position.stop_loss,
            position.take_profit,
            position.size,
            position.leverage,
        );
        self.positions.insert(position.coin.clone(), position);
    }

    /// Open a position AND place SL/TP trigger orders on the exchange.
    /// Stores the trigger OIDs in the position.
    ///
    /// In dry-run/observation mode, only logs — does not call exchange.
    pub async fn open_position_with_triggers(
        &mut self,
        mut position: OpenPosition,
        rest: &RestClient,
        mode: &BotMode,
        asset_index: u32,
    ) -> Result<()> {
        if *mode == BotMode::Live {
            // SL trigger: exit is OPPOSITE of entry direction
            let sl_is_buy = position.direction == Direction::Short;
            match rest
                .place_trigger_order(
                    &position.coin,
                    sl_is_buy,
                    position.stop_loss,
                    position.size,
                    false, // is_tp = false
                    asset_index,
                )
                .await
            {
                Ok(r) if r.error.is_none() => {
                    position.sl_trigger_oid = r.oid.clone();
                    info!("[POSITION] SL trigger placed for {}: {:?}", position.coin, r.oid);
                }
                Ok(r) => warn!("[POSITION] SL trigger error for {}: {:?}", position.coin, r.error),
                Err(e) => warn!("[POSITION] SL trigger call failed for {}: {}", position.coin, e),
            }

            // TP as ALO limit order (maker, 1.5 bps vs 4.5 bps taker trigger)
            // reduce_only=true ensures it closes the position, not opens a new one.
            let tp_is_buy = !sl_is_buy;
            let tp_req = crate::exchange::rest_client::OrderRequest {
                coin: position.coin.clone(),
                asset_index,
                is_buy: tp_is_buy,
                price: position.take_profit,
                size: position.size,
                tif: crate::exchange::rest_client::Tif::Alo,
                reduce_only: true,
                client_oid: Some(format!("tp_{}", position.client_oid)),
            };
            match rest.place_order(&tp_req).await {
                Ok(r) if r.error.is_none() => {
                    position.tp_order_oid = r.oid.clone();
                    info!("[POSITION] TP ALO placed for {}: {:?} @ {}", position.coin, r.oid, position.take_profit);
                }
                Ok(r) => warn!("[POSITION] TP ALO error for {}: {:?}", position.coin, r.error),
                Err(e) => warn!("[POSITION] TP ALO call failed for {}: {}", position.coin, e),
            }
        }

        self.open_position(position);
        Ok(())
    }

    /// Update the SL trigger on the exchange (cancel old, place new).
    /// Used for break-even and trailing stop updates.
    pub async fn update_sl_trigger(
        &mut self,
        coin: &str,
        new_sl: Decimal,
        rest: &RestClient,
        mode: &BotMode,
        asset_index: u32,
    ) -> Result<()> {
        let (old_oid, direction, size) = {
            let pos = match self.positions.get(coin) {
                Some(p) => p,
                None => return Ok(()),
            };
            (pos.sl_trigger_oid.clone(), pos.direction, pos.size)
        };

        if *mode == BotMode::Live {
            // Cancel old SL trigger
            if let Some(oid) = old_oid {
                if let Err(e) = rest.cancel_order(coin, &oid, asset_index).await {
                    warn!("[POSITION] Failed to cancel old SL trigger for {}: {}", coin, e);
                }
            }

            // Place new SL trigger at updated price
            let sl_is_buy = direction == Direction::Short;
            match rest
                .place_trigger_order(coin, sl_is_buy, new_sl, size, false, asset_index)
                .await
            {
                Ok(r) if r.error.is_none() => {
                    if let Some(pos) = self.positions.get_mut(coin) {
                        pos.sl_trigger_oid = r.oid.clone();
                    }
                    info!("[POSITION] SL trigger updated for {} to {} — new oid={:?}", coin, new_sl, r.oid);
                }
                Ok(r) => warn!("[POSITION] New SL trigger error for {}: {:?}", coin, r.error),
                Err(e) => warn!("[POSITION] New SL trigger call failed for {}: {}", coin, e),
            }
        } else if let Some(pos) = self.positions.get_mut(coin) {
            pos.stop_loss = new_sl;
        }

        Ok(())
    }

    /// Close a position and start a cooldown.
    ///
    /// `cooldown_s` is taken from `risk.cooldown_after_close_s` (not execution.breakeven).
    /// Close a position. Returns the TP ALO order OID if one exists (caller must cancel it).
    pub fn close_position(
        &mut self,
        coin: &str,
        reason: &str,
        exit_price: Decimal,
        cooldown_s: u64,
    ) -> Option<String> {
        if let Some(pos) = self.positions.remove(coin) {
            let pnl = match pos.direction {
                Direction::Long => (exit_price - pos.entry_price) * pos.size,
                Direction::Short => (pos.entry_price - exit_price) * pos.size,
            };
            info!(
                "[POSITION] Closed: {} {:?} | entry={} exit={} | pnl={} | reason={}",
                coin, pos.direction, pos.entry_price, exit_price, pnl, reason
            );
            let now = chrono::Utc::now().timestamp_millis();
            self.cooldowns.insert(coin.to_string(), now + (cooldown_s as i64 * 1000));
            // Return the TP limit order OID so the caller can cancel it on the exchange
            pos.tp_order_oid
        } else {
            None
        }
    }

    /// Find which coin has a TP ALO order with this OID.
    pub fn find_coin_by_tp_oid(&self, oid: &str) -> Option<String> {
        for (coin, pos) in &self.positions {
            if pos.tp_order_oid.as_deref() == Some(oid) {
                return Some(coin.clone());
            }
        }
        None
    }

    /// Get an open position.
    pub fn get(&self, coin: &str) -> Option<&OpenPosition> {
        self.positions.get(coin)
    }

    /// Get a mutable reference to a position.
    pub fn get_mut(&mut self, coin: &str) -> Option<&mut OpenPosition> {
        self.positions.get_mut(coin)
    }

    /// Check if a coin is in cooldown.
    pub fn in_cooldown(&self, coin: &str, now_ms: i64) -> bool {
        self.cooldowns
            .get(coin)
            .map(|&expires| now_ms < expires)
            .unwrap_or(false)
    }

    /// Number of open positions.
    pub fn count(&self) -> usize {
        self.positions.len()
    }

    /// Count positions in a given direction.
    pub fn count_direction(&self, direction: Direction) -> usize {
        self.positions.values().filter(|p| p.direction == direction).count()
    }

    /// All open positions.
    pub fn all(&self) -> &HashMap<String, OpenPosition> {
        &self.positions
    }

    /// Check break-even condition.
    /// Returns `Some(new_sl_price)` if break-even triggers (call `update_sl_trigger` after).
    pub fn check_break_even(&mut self, coin: &str, current_price: f64) -> Option<Decimal> {
        let trigger_pct = self.settings.breakeven.trigger_pct / 100.0;

        let pos = self.positions.get_mut(coin)?;
        if pos.break_even_applied {
            return None;
        }

        let entry = pos.entry_price;
        let tp = pos.take_profit;
        let current = Decimal::try_from(current_price).ok()?;

        let tp_distance = match pos.direction {
            Direction::Long => tp - entry,
            Direction::Short => entry - tp,
        };
        if tp_distance <= Decimal::ZERO {
            return None;
        }

        let current_progress = match pos.direction {
            Direction::Long => current - entry,
            Direction::Short => entry - current,
        };

        let progress_pct = current_progress / tp_distance;
        if progress_pct
            >= Decimal::try_from(trigger_pct).unwrap_or(Decimal::new(5, 1))
        {
            pos.break_even_applied = true;
            pos.stop_loss = entry;
            info!("[POSITION] Break-even triggered for {} — SL → entry {}", coin, entry);
            Some(entry)
        } else {
            None
        }
    }

    /// Check trailing stop tiers.
    /// Returns `Some(new_sl_price)` if a tier fires (call `update_sl_trigger` after).
    pub fn check_trailing(&mut self, coin: &str, current_price: f64) -> Option<Decimal> {
        let settings = &self.settings;
        let pos = self.positions.get_mut(coin)?;

        if !pos.break_even_applied {
            return None;
        }

        let entry = pos.entry_price;
        let tp = pos.take_profit;
        let current = Decimal::try_from(current_price).ok()?;

        let tp_distance = match pos.direction {
            Direction::Long => tp - entry,
            Direction::Short => entry - tp,
        };
        if tp_distance <= Decimal::ZERO {
            return None;
        }

        let current_progress = match pos.direction {
            Direction::Long => current - entry,
            Direction::Short => entry - current,
        };

        let progress_pct = (current_progress / tp_distance * Decimal::new(100, 0))
            .to_string()
            .parse::<f64>()
            .unwrap_or(0.0);

        let (new_tier, lock_pct) =
            if progress_pct >= settings.trailing.tier2_progress_pct && pos.trailing_tier < 2 {
                (2u8, settings.trailing.tier2_lock_pct / 100.0)
            } else if progress_pct >= settings.trailing.tier1_progress_pct && pos.trailing_tier < 1 {
                (1u8, settings.trailing.tier1_lock_pct / 100.0)
            } else {
                return None;
            };

        let lock = Decimal::try_from(lock_pct).unwrap_or(Decimal::new(25, 2));
        let new_sl = match pos.direction {
            Direction::Long => entry + tp_distance * lock,
            Direction::Short => entry - tp_distance * lock,
        };

        pos.trailing_tier = new_tier;
        pos.stop_loss = new_sl;
        info!("[POSITION] Trailing tier {} for {} — SL → {}", new_tier, coin, new_sl);
        Some(new_sl)
    }

    /// Synchronize with exchange positions.
    /// NEVER interprets API error as "0 positions" (t-bot bug #13).
    pub async fn sync_with_exchange(&mut self, rest: &RestClient) -> Result<()> {
        let exchange_positions = match rest.get_open_positions().await {
            Ok(p) => p,
            Err(e) => {
                error!("[POSITION] Sync failed: {} — skipping (positions unchanged)", e);
                return Ok(());
            }
        };

        if exchange_positions.is_empty() && !self.positions.is_empty() {
            warn!(
                "[POSITION] Exchange reports 0 positions but tracking {} — likely API error, skipping sync",
                self.positions.len()
            );
            return Ok(());
        }

        // Orphan on exchange not tracked locally → recover
        for ep in &exchange_positions {
            if !self.positions.contains_key(&ep.coin) {
                warn!("[POSITION] Orphan on exchange: {} size={}", ep.coin, ep.size);
                self.recover_position(ep);
            }
        }

        // Locally tracked but no longer on exchange → closed
        let tracked_coins: Vec<String> = self.positions.keys().cloned().collect();
        for coin in tracked_coins {
            let on_exchange = exchange_positions.iter().any(|ep| ep.coin == coin);
            if !on_exchange {
                info!("[POSITION] {} no longer on exchange — marking closed", coin);
                self.positions.remove(&coin);
            }
        }

        Ok(())
    }

    /// Recover an orphan position from exchange data.
    fn recover_position(&mut self, ep: &ExchangePosition) {
        let direction = if ep.size > Decimal::ZERO {
            Direction::Long
        } else {
            Direction::Short
        };
        // Conservative SL/TP based on a fixed 0.3% distance
        let sl_distance = ep.entry_price * Decimal::new(3, 3);
        let (sl, tp) = match direction {
            Direction::Long => (
                ep.entry_price - sl_distance,
                ep.entry_price + sl_distance * Decimal::new(2, 0),
            ),
            Direction::Short => (
                ep.entry_price + sl_distance,
                ep.entry_price - sl_distance * Decimal::new(2, 0),
            ),
        };

        let pos = OpenPosition {
            coin: ep.coin.clone(),
            direction,
            entry_price: ep.entry_price,
            size: ep.size.abs(),
            stop_loss: sl,
            take_profit: tp,
            original_stop_loss: sl,
            leverage: ep.leverage,
            opened_at: chrono::Utc::now().timestamp_millis(),
            break_even_applied: false,
            trailing_tier: 0,
            tp_order_oid: None,
            sl_trigger_oid: None,
            client_oid: format!("recovered-{}", ep.coin),
        };
        info!(
            "[POSITION] Recovered: {} {:?} entry={} sl={} tp={}",
            ep.coin, direction, ep.entry_price, sl, tp
        );
        self.positions.insert(ep.coin.clone(), pos);
    }

    /// Recover positions at startup.
    pub async fn recover_positions(&mut self, rest: &RestClient) -> Result<()> {
        let exchange_positions = rest.get_open_positions().await?;
        if exchange_positions.is_empty() {
            info!("[POSITION] No positions to recover at startup");
            return Ok(());
        }
        for ep in &exchange_positions {
            self.recover_position(ep);
        }
        info!("[POSITION] Recovered {} positions at startup", exchange_positions.len());
        Ok(())
    }

    /// Cancel orphan trigger orders at startup that have no matching local position.
    pub async fn cleanup_orphan_triggers(&self, rest: &RestClient) -> Result<()> {
        let open_orders = rest.fetch_frontend_open_orders().await?;
        if let Some(orders) = open_orders.as_array() {
            for order in orders {
                let coin = order.get("coin").and_then(|c| c.as_str()).unwrap_or("");
                let oid = order.get("oid").and_then(|o| o.as_str()).unwrap_or("");
                if !self.positions.contains_key(coin) && !oid.is_empty() {
                    warn!("[POSITION] Cleaning orphan trigger: {} oid={}", coin, oid);
                    // asset_index 0 used here — orphan cleanup happens before meta is needed
                    let _ = rest.cancel_order(coin, oid, 0).await;
                }
            }
        }
        Ok(())
    }
}
