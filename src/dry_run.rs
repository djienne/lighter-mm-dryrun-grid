use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use ordered_float::OrderedFloat;
use serde::{Deserialize, Serialize};

use crate::order_manager::{Action, BatchOp, OrderManager, OrderState, Side};
use crate::orderbook::BookSide;
use crate::trade_log::TradeLogger;

// ---------------------------------------------------------------------------
// SimulatedOrder
// ---------------------------------------------------------------------------

pub struct SimulatedOrder {
    pub client_order_id: i64,
    pub side: Side,
    pub price: f64,
    pub size: f64,
    pub original_size: f64,
    pub level: usize,
    pub created_at: Instant,
    pub synthetic_exchange_id: i64,
    pub eligible_at: Instant,
    pub pending_cancel_at: Option<Instant>,
    pub prev_by_price: HashMap<OrderedFloat<f64>, f64>,
    pub arrival_checked: bool,
    pub queue_ts: Instant,
    // Pending modify
    pub pending_price: Option<f64>,
    pub pending_size: Option<f64>,
    pub pending_prev_by_price: Option<HashMap<OrderedFloat<f64>, f64>>,
}

// ---------------------------------------------------------------------------
// Saved state (JSON-compatible with Python)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
pub struct SavedState {
    pub available_capital: f64,
    pub portfolio_value: f64,
    pub position: f64,
    pub entry_vwap: f64,
    pub realized_pnl: f64,
    pub fill_count: u64,
    pub total_volume: f64,
    pub initial_capital: f64,
    pub initial_portfolio_value: f64,
    pub updated_at: String,
}

// ---------------------------------------------------------------------------
// DryRunEngine
// ---------------------------------------------------------------------------

pub struct DryRunEngine {
    // Simulated order book
    live_orders: HashMap<i64, SimulatedOrder>,
    next_exchange_id: i64,

    // PnL tracking (average-cost basis)
    pub position: f64,
    pub entry_vwap: f64,
    pub realized_pnl: f64,
    pub total_volume: f64,
    pub fill_count: u64,
    pub initial_capital: f64,
    pub initial_portfolio_value: f64,
    entry_price_before: f64,

    // Config
    sim_latency: std::time::Duration,
    leverage: u32,
    maker_fee_rate: f64,
    log_interval: std::time::Duration,
    last_summary: Instant,
    state_path: Option<PathBuf>,

    pub initialized: bool,
}

impl DryRunEngine {
    pub fn new(
        leverage: u32,
        sim_latency_s: f64,
        maker_fee_rate: f64,
        state_path: Option<PathBuf>,
    ) -> Self {
        Self {
            live_orders: HashMap::new(),
            next_exchange_id: 900_000_000,
            position: 0.0,
            entry_vwap: 0.0,
            realized_pnl: 0.0,
            total_volume: 0.0,
            fill_count: 0,
            initial_capital: 0.0,
            initial_portfolio_value: 0.0,
            entry_price_before: 0.0,
            sim_latency: std::time::Duration::from_secs_f64(sim_latency_s),
            leverage: leverage.max(1),
            maker_fee_rate,
            log_interval: std::time::Duration::from_secs(60),
            last_summary: Instant::now(),
            state_path,
            initialized: false,
        }
    }

    /// Snapshot the account capital and position before trading starts.
    pub fn capture_initial_state(
        &mut self,
        capital: f64,
        portfolio_value: f64,
        position: f64,
        mid_price: Option<f64>,
    ) {
        self.initial_capital = capital;
        self.initial_portfolio_value = if portfolio_value > 0.0 {
            portfolio_value
        } else {
            capital
        };
        self.position = position;
        if position.abs() > 1e-12 {
            self.entry_vwap = mid_price.unwrap_or(0.0);
        } else {
            self.entry_vwap = 0.0;
        }
        self.initialized = true;
        tracing::info!(
            "DRY-RUN: captured initial capital ${:.2} | position {:.6} | leverage {}x | sim_latency={}ms",
            self.initial_capital,
            self.position,
            self.leverage,
            self.sim_latency.as_millis(),
        );
    }

    /// Try to restore from saved JSON state file.
    pub fn load_state(path: &Path) -> Option<SavedState> {
        if !path.exists() {
            return None;
        }
        match std::fs::read_to_string(path) {
            Ok(data) => match serde_json::from_str(&data) {
                Ok(state) => Some(state),
                Err(e) => {
                    tracing::warn!("DRY-RUN: could not parse state from {}: {}", path.display(), e);
                    None
                }
            },
            Err(e) => {
                tracing::warn!("DRY-RUN: could not read state from {}: {}", path.display(), e);
                None
            }
        }
    }

    /// Restore engine fields from saved state.
    pub fn restore_from(&mut self, saved: &SavedState) {
        self.initial_capital = saved.initial_capital;
        self.initial_portfolio_value = saved.initial_portfolio_value;
        self.position = saved.position;
        self.entry_vwap = saved.entry_vwap;
        self.realized_pnl = saved.realized_pnl;
        self.fill_count = saved.fill_count;
        self.total_volume = saved.total_volume;
        self.initialized = true;
        tracing::info!(
            "DRY-RUN: restored state | capital=${:.2} | pos={:.6} | realized=${:.4} | fills={}",
            saved.available_capital,
            self.position,
            self.realized_pnl,
            self.fill_count,
        );
    }

    /// Build save data dict.
    pub fn build_save_data(&self, available_capital: f64, portfolio_value: f64) -> SavedState {
        SavedState {
            available_capital,
            portfolio_value,
            position: self.position,
            entry_vwap: self.entry_vwap,
            realized_pnl: self.realized_pnl,
            fill_count: self.fill_count,
            total_volume: self.total_volume,
            initial_capital: self.initial_capital,
            initial_portfolio_value: self.initial_portfolio_value,
            updated_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Atomically write state to JSON (tmp + rename).
    pub fn save_state(&self, available_capital: f64, portfolio_value: f64) {
        let Some(ref path) = self.state_path else { return };
        let data = self.build_save_data(available_capital, portfolio_value);
        let dir = path.parent().unwrap_or(Path::new("."));
        match serde_json::to_string_pretty(&data) {
            Ok(json) => {
                let tmp = dir.join(format!(".tmp_{}", std::process::id()));
                if std::fs::write(&tmp, &json).is_ok() {
                    let _ = std::fs::rename(&tmp, path);
                }
            }
            Err(e) => tracing::warn!("DRY-RUN: failed to serialize state: {}", e),
        }
    }

    // ------------------------------------------------------------------
    // Batch processing (replaces sign_and_send_batch)
    // ------------------------------------------------------------------

    pub fn process_batch(
        &mut self,
        ops: &[BatchOp],
        orders: &mut OrderState,
        om: &mut OrderManager,
        id_map: &mut HashMap<i64, i64>,
        best_bid: Option<f64>,
        best_ask: Option<f64>,
        bids: &BookSide,
        asks: &BookSide,
    ) {
        for op in ops {
            match op.action {
                Action::Create => self.handle_create(op, orders, om, id_map, best_bid, best_ask, asks, bids),
                Action::Modify => self.handle_modify(op, orders, om, best_bid, best_ask, asks, bids),
                Action::Cancel => self.handle_cancel(op, orders, om),
            }
        }
    }

    fn handle_create(
        &mut self,
        op: &BatchOp,
        orders: &mut OrderState,
        om: &mut OrderManager,
        id_map: &mut HashMap<i64, i64>,
        best_bid: Option<f64>,
        best_ask: Option<f64>,
        asks: &BookSide,
        bids: &BookSide,
    ) {
        let eid = self.next_exchange_id;
        self.next_exchange_id += 1;
        let now = Instant::now();

        // POST_ONLY check: reject if immediately marketable
        match op.side {
            Side::Buy => {
                if let Some(ba) = best_ask {
                    if ba <= op.price {
                        tracing::debug!(
                            "DRY-RUN REJECT BUY L{}: {:.6} @ ${:.2} (POST_ONLY: best_ask ${:.2} <= price)",
                            op.level, op.size, op.price, ba,
                        );
                        return;
                    }
                }
            }
            Side::Sell => {
                if let Some(bb) = best_bid {
                    if bb >= op.price {
                        tracing::debug!(
                            "DRY-RUN REJECT SELL L{}: {:.6} @ ${:.2} (POST_ONLY: best_bid ${:.2} >= price)",
                            op.level, op.size, op.price, bb,
                        );
                        return;
                    }
                }
            }
        }

        // Snapshot current qualifying depth
        let prev_by_price = self.snapshot_qualifying_depth(op.side, op.price, asks, bids);

        let no_latency = self.sim_latency.is_zero();
        let sim = SimulatedOrder {
            client_order_id: op.order_id,
            side: op.side,
            price: op.price,
            size: op.size,
            original_size: op.size,
            level: op.level,
            created_at: now,
            synthetic_exchange_id: eid,
            eligible_at: now + self.sim_latency,
            pending_cancel_at: None,
            prev_by_price,
            arrival_checked: no_latency,
            queue_ts: now,
            pending_price: None,
            pending_size: None,
            pending_prev_by_price: None,
        };

        self.live_orders.insert(op.order_id, sim);
        id_map.insert(op.order_id, eid);
        om.bind_live(orders, op.side, op.order_id, op.price, op.size, op.level);

        tracing::debug!(
            "DRY-RUN CREATE {} L{}: {:.6} @ ${:.2} (cid={} eid={})",
            op.side, op.level, op.size, op.price, op.order_id, eid,
        );
    }

    fn handle_modify(
        &mut self,
        op: &BatchOp,
        orders: &mut OrderState,
        om: &mut OrderManager,
        best_bid: Option<f64>,
        best_ask: Option<f64>,
        asks: &BookSide,
        bids: &BookSide,
    ) {
        let sim = match self.live_orders.get_mut(&op.order_id) {
            Some(s) => s,
            None => return,
        };

        // POST_ONLY check for the new price
        match op.side {
            Side::Buy => {
                if let Some(ba) = best_ask {
                    if ba <= op.price {
                        tracing::debug!(
                            "DRY-RUN REJECT-MODIFY BUY L{}: @ ${:.2} -> ${:.2} (POST_ONLY: best_ask ${:.2})",
                            op.level, sim.price, op.price, ba,
                        );
                        return;
                    }
                }
            }
            Side::Sell => {
                if let Some(bb) = best_bid {
                    if bb >= op.price {
                        tracing::debug!(
                            "DRY-RUN REJECT-MODIFY SELL L{}: @ ${:.2} -> ${:.2} (POST_ONLY: best_bid ${:.2})",
                            op.level, sim.price, op.price, bb,
                        );
                        return;
                    }
                }
            }
        }

        let now = Instant::now();
        let new_snapshot = self.snapshot_qualifying_depth(op.side, op.price, asks, bids);

        // Need to re-borrow as mutable after snapshot_qualifying_depth
        let sim = self.live_orders.get_mut(&op.order_id).unwrap();

        if self.sim_latency.is_zero() {
            sim.price = op.price;
            sim.size = op.size;
            sim.prev_by_price = new_snapshot;
            sim.arrival_checked = true;
            sim.queue_ts = now;
            sim.pending_price = None;
            sim.pending_size = None;
            sim.pending_prev_by_price = None;
            om.bind_live(orders, op.side, op.order_id, op.price, op.size, op.level);
        } else {
            sim.pending_price = Some(op.price);
            sim.pending_size = Some(op.size);
            sim.pending_prev_by_price = Some(new_snapshot);
            sim.eligible_at = now + self.sim_latency;
        }

        tracing::debug!(
            "DRY-RUN MODIFY {} L{}: {:.6} @ ${:.2} (cid={})",
            op.side, op.level, op.size, op.price, op.order_id,
        );
    }

    fn handle_cancel(
        &mut self,
        op: &BatchOp,
        orders: &mut OrderState,
        om: &mut OrderManager,
    ) {
        match self.live_orders.get_mut(&op.order_id) {
            Some(sim) => {
                sim.pending_cancel_at = Some(Instant::now() + self.sim_latency);
            }
            None => {
                om.clear_live(orders, op.side, op.level);
            }
        }
    }

    fn snapshot_qualifying_depth(
        &self,
        side: Side,
        price: f64,
        asks: &BookSide,
        bids: &BookSide,
    ) -> HashMap<OrderedFloat<f64>, f64> {
        let mut snap = HashMap::new();
        match side {
            Side::Buy => {
                // For buy orders, snapshot asks at or below our price
                for (&ask_price, &ask_size) in asks.iter() {
                    if ask_price.into_inner() > price {
                        break;
                    }
                    snap.insert(ask_price, ask_size);
                }
            }
            Side::Sell => {
                // For sell orders, snapshot bids at or above our price
                for (&bid_price, &bid_size) in bids.range(OrderedFloat(price)..) {
                    snap.insert(bid_price, bid_size);
                }
            }
        }
        snap
    }

    // ------------------------------------------------------------------
    // Fill simulation
    // ------------------------------------------------------------------

    pub fn check_fills(
        &mut self,
        bids: &BookSide,
        asks: &BookSide,
        orders: &mut OrderState,
        om: &mut OrderManager,
        account_capital: &mut f64,
        account_portfolio: &mut f64,
        account_position: &mut f64,
        mid_price: Option<f64>,
        market_id: Option<i64>,
        mut trade_logger: Option<&mut TradeLogger>,
    ) {
        if self.live_orders.is_empty() {
            return;
        }

        let now = Instant::now();

        // PASS 1: Promote pending modifies whose latency expired
        let order_ids: Vec<i64> = self.live_orders.keys().copied().collect();
        for oid in &order_ids {
            let sim = match self.live_orders.get_mut(oid) {
                Some(s) => s,
                None => continue,
            };
            if sim.pending_price.is_none() || now < sim.eligible_at {
                continue;
            }

            let pending_price = sim.pending_price.unwrap();
            let mut rejected = false;

            if sim.side == Side::Buy {
                if let Some((&ba, _)) = asks.iter().next() {
                    if ba.into_inner() <= pending_price {
                        rejected = true;
                    }
                }
            } else if let Some((&bb, _)) = bids.iter().next_back() {
                if bb.into_inner() >= pending_price {
                    rejected = true;
                }
            }

            if rejected {
                let old_price = sim.price;
                let old_size = sim.size;
                let side = sim.side;
                let level = sim.level;
                let cid = sim.client_order_id;
                sim.pending_price = None;
                sim.pending_size = None;
                sim.pending_prev_by_price = None;
                om.bind_live(orders, side, cid, old_price, old_size, level);
            } else {
                let new_price = sim.pending_price.unwrap();
                let new_size = sim.pending_size.unwrap();
                let new_snap = sim.pending_prev_by_price.take().unwrap_or_default();
                let side = sim.side;
                let level = sim.level;
                let cid = sim.client_order_id;
                sim.price = new_price;
                sim.size = new_size;
                sim.prev_by_price = new_snap;
                sim.queue_ts = now;
                sim.pending_price = None;
                sim.pending_size = None;
                om.bind_live(orders, side, cid, new_price, new_size, level);
            }
        }

        // PASS 2: Sort by price priority + FIFO, then check fills
        let mut sorted_ids: Vec<(i64, Side, f64, Instant)> = self
            .live_orders
            .values()
            .map(|s| (s.client_order_id, s.side, s.price, s.queue_ts))
            .collect();
        sorted_ids.sort_by(|a, b| {
            let pa = if a.1 == Side::Buy { -a.2 } else { a.2 };
            let pb = if b.1 == Side::Buy { -b.2 } else { b.2 };
            pa.partial_cmp(&pb)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.3.cmp(&b.3))
        });

        let mut buy_consumed = 0.0;
        let mut sell_consumed = 0.0;

        // Collect fills before applying them (to avoid borrow issues)
        struct FillInfo {
            order_id: i64,
            fill_size: f64,
            fill_price: f64,
            side: Side,
            level: usize,
        }
        let mut fills: Vec<FillInfo> = Vec::new();
        let mut to_cancel: Vec<(i64, Side, usize)> = Vec::new();
        let mut to_reject: Vec<(i64, Side, usize)> = Vec::new();

        for (oid, _, _, _) in &sorted_ids {
            let sim = match self.live_orders.get_mut(oid) {
                Some(s) => s,
                None => continue,
            };

            // Skip orders in CREATE in-flight
            if now < sim.eligible_at && sim.pending_price.is_none() {
                if let Some(cancel_at) = sim.pending_cancel_at {
                    if now >= cancel_at {
                        to_cancel.push((sim.client_order_id, sim.side, sim.level));
                    }
                }
                continue;
            }

            // POST_ONLY recheck at simulated arrival time
            if !sim.arrival_checked {
                sim.arrival_checked = true;
                let mut rejected_at_arrival = false;
                if sim.side == Side::Buy {
                    if let Some((&ba, _)) = asks.iter().next() {
                        if ba.into_inner() <= sim.price {
                            rejected_at_arrival = true;
                        }
                    }
                } else if let Some((&bb, _)) = bids.iter().next_back() {
                    if bb.into_inner() >= sim.price {
                        rejected_at_arrival = true;
                    }
                }
                if rejected_at_arrival {
                    to_reject.push((sim.client_order_id, sim.side, sim.level));
                    continue;
                }
            }

            // Check fills
            let (fill, new_consumed) = if sim.side == Side::Buy {
                Self::check_buy_fill(sim, asks, buy_consumed)
            } else {
                Self::check_sell_fill(sim, bids, sell_consumed)
            };

            if sim.side == Side::Buy {
                buy_consumed = new_consumed;
            } else {
                sell_consumed = new_consumed;
            }

            if fill > 0.0 {
                fills.push(FillInfo {
                    order_id: sim.client_order_id,
                    fill_size: fill,
                    fill_price: sim.price,
                    side: sim.side,
                    level: sim.level,
                });
            }

            // Process matured cancel (after fill opportunity)
            if let Some(cancel_at) = sim.pending_cancel_at {
                if now >= cancel_at {
                    to_cancel.push((sim.client_order_id, sim.side, sim.level));
                }
            }
        }

        // Apply rejections
        for (oid, side, level) in to_reject {
            self.live_orders.remove(&oid);
            om.clear_live(orders, side, level);
        }

        // Apply fills
        for fill_info in fills {
            self.process_fill(
                fill_info.order_id,
                fill_info.fill_size,
                fill_info.fill_price,
                fill_info.side,
                fill_info.level,
                orders,
                om,
                account_capital,
                account_portfolio,
                account_position,
                mid_price,
                market_id,
                trade_logger.as_deref_mut(),
            );
        }

        // Apply cancels
        for (oid, side, level) in to_cancel {
            self.live_orders.remove(&oid);
            om.clear_live(orders, side, level);
        }
    }

    fn check_buy_fill(
        sim: &mut SimulatedOrder,
        asks: &BookSide,
        consumed: f64,
    ) -> (f64, f64) {
        if asks.is_empty() {
            sim.prev_by_price.clear();
            return (0.0, consumed);
        }
        let best_ask = asks.keys().next().unwrap().into_inner();
        if best_ask > sim.price {
            sim.prev_by_price.clear();
            return (0.0, consumed);
        }

        let mut current: HashMap<OrderedFloat<f64>, f64> = HashMap::new();
        for (&ask_price, &ask_size) in asks.iter() {
            if ask_price.into_inner() > sim.price {
                break;
            }
            current.insert(ask_price, ask_size);
        }

        let prev = &sim.prev_by_price;
        let mut new_liq = 0.0;
        for (&price, &size) in &current {
            let delta = size - prev.get(&price).copied().unwrap_or(0.0);
            if delta > 0.0 {
                new_liq += delta;
            }
        }
        new_liq -= consumed;
        sim.prev_by_price = current;
        if new_liq <= 0.0 {
            return (0.0, consumed);
        }
        let fill = sim.size.min(new_liq);
        (fill, consumed + fill)
    }

    fn check_sell_fill(
        sim: &mut SimulatedOrder,
        bids: &BookSide,
        consumed: f64,
    ) -> (f64, f64) {
        if bids.is_empty() {
            sim.prev_by_price.clear();
            return (0.0, consumed);
        }
        let best_bid = bids.keys().next_back().unwrap().into_inner();
        if best_bid < sim.price {
            sim.prev_by_price.clear();
            return (0.0, consumed);
        }

        let mut current: HashMap<OrderedFloat<f64>, f64> = HashMap::new();
        for (&bid_price, &bid_size) in bids.range(OrderedFloat(sim.price)..) {
            current.insert(bid_price, bid_size);
        }

        let prev = &sim.prev_by_price;
        let mut new_liq = 0.0;
        for (&price, &size) in &current {
            let delta = size - prev.get(&price).copied().unwrap_or(0.0);
            if delta > 0.0 {
                new_liq += delta;
            }
        }
        new_liq -= consumed;
        sim.prev_by_price = current;
        if new_liq <= 0.0 {
            return (0.0, consumed);
        }
        let fill = sim.size.min(new_liq);
        (fill, consumed + fill)
    }

    // ------------------------------------------------------------------
    // Fill processing & PnL
    // ------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn process_fill(
        &mut self,
        order_id: i64,
        fill_size: f64,
        fill_price: f64,
        side: Side,
        level: usize,
        orders: &mut OrderState,
        om: &mut OrderManager,
        account_capital: &mut f64,
        account_portfolio: &mut f64,
        account_position: &mut f64,
        mid_price: Option<f64>,
        _market_id: Option<i64>,
        mut trade_logger: Option<&mut TradeLogger>,
    ) {
        let sim = match self.live_orders.get_mut(&order_id) {
            Some(s) => s,
            None => return,
        };
        sim.size -= fill_size;
        let fully_filled = sim.size <= 1e-12;

        // Update simulated position
        let old_pos = self.position;
        match side {
            Side::Buy => self.position += fill_size,
            Side::Sell => self.position -= fill_size,
        }

        // PnL: average-cost basis
        let pnl_before = self.realized_pnl;
        self.entry_price_before = self.entry_vwap;
        self.update_pnl(side, fill_size, fill_price, old_pos);
        let mut realized_delta = self.realized_pnl - pnl_before;

        // Maker fee
        let fee = fill_size * fill_price * self.maker_fee_rate;
        self.realized_pnl -= fee;
        realized_delta -= fee;

        // Bookkeeping
        self.fill_count += 1;
        self.total_volume += fill_size * fill_price;

        // Capital impact: margin adjustment + realized PnL
        let leverage = self.leverage as f64;
        if Self::is_reducing(side, old_pos) {
            let reduce_qty = fill_size.min(old_pos.abs());
            let excess = fill_size - reduce_qty;
            let margin_release = reduce_qty * self.entry_price_before / leverage;
            *account_capital += margin_release + realized_delta;
            if excess > 1e-12 {
                *account_capital -= excess * fill_price / leverage;
            }
        } else {
            *account_capital -= fill_size * fill_price / leverage + fee;
        }

        // Portfolio value = initial + realized + unrealized
        let unrealized = self.compute_unrealized_pnl(mid_price);
        *account_portfolio = self.initial_portfolio_value + self.realized_pnl + unrealized;
        *account_position = self.position;

        // Update order state
        if fully_filled {
            self.live_orders.remove(&order_id);
            om.clear_live(orders, side, level);
        } else {
            let remaining = self.live_orders.get(&order_id).map(|s| s.size).unwrap_or(0.0);
            let price = self.live_orders.get(&order_id).map(|s| s.price).unwrap_or(fill_price);
            om.bind_live(orders, side, order_id, price, remaining, level);
        }

        tracing::debug!(
            "DRY-RUN {} {} L{}: {:.6} @ ${:.2} | pos={:.6} | realized=${:.4} | unrealized=${:.4}",
            if fully_filled { "FILLED" } else { "PARTIAL" },
            side, level, fill_size, fill_price,
            self.position, self.realized_pnl, unrealized,
        );

        // Trade log
        if let Some(ref mut logger) = trade_logger {
            logger.log_fill(
                side.as_str(),
                fill_price,
                fill_size,
                level,
                self.position,
                self.realized_pnl,
                *account_capital,
                *account_portfolio,
                true,
            );
        }
    }

    fn is_reducing(side: Side, old_pos: f64) -> bool {
        matches!(
            (side, old_pos < 0.0, old_pos > 0.0),
            (Side::Buy, true, _) | (Side::Sell, _, true)
        )
    }

    fn update_pnl(&mut self, side: Side, fill_size: f64, fill_price: f64, old_pos: f64) {
        if Self::is_reducing(side, old_pos) {
            let reduce_qty = fill_size.min(old_pos.abs());
            let pnl = if old_pos > 0.0 {
                reduce_qty * (fill_price - self.entry_vwap)
            } else {
                reduce_qty * (self.entry_vwap - fill_price)
            };
            self.realized_pnl += pnl;

            let excess = fill_size - reduce_qty;
            if excess > 1e-12 {
                self.entry_vwap = fill_price;
            } else if self.position.abs() < 1e-12 {
                self.entry_vwap = 0.0;
            }
        } else {
            let abs_old = old_pos.abs();
            let abs_new = self.position.abs();
            if abs_new > 1e-12 {
                self.entry_vwap =
                    (self.entry_vwap * abs_old + fill_price * fill_size) / abs_new;
            }
        }
    }

    // ------------------------------------------------------------------
    // Reporting
    // ------------------------------------------------------------------

    pub fn compute_unrealized_pnl(&self, mid_price: Option<f64>) -> f64 {
        let mid = match mid_price {
            Some(m) if self.position.abs() >= 1e-12 => m,
            _ => return 0.0,
        };
        if self.position > 0.0 {
            self.position * (mid - self.entry_vwap)
        } else {
            self.position.abs() * (self.entry_vwap - mid)
        }
    }

    pub fn total_pnl(&self, mid_price: Option<f64>) -> f64 {
        self.realized_pnl + self.compute_unrealized_pnl(mid_price)
    }

    pub fn should_log_summary(&self) -> bool {
        self.last_summary.elapsed() >= self.log_interval
    }

    pub fn mark_summary_logged(&mut self) {
        self.last_summary = Instant::now();
    }

    pub fn live_order_count(&self) -> usize {
        self.live_orders.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrealized_pnl_uses_mid_price() {
        let mut e = DryRunEngine::new(1, 0.0, 0.0, None);
        e.position = 2.0;
        e.entry_vwap = 100.0;
        // Without a mid price, open positions cannot be valued.
        assert_eq!(e.compute_unrealized_pnl(None), 0.0);
        // Long 2 @ 100, mid 105 → +10
        assert!((e.compute_unrealized_pnl(Some(105.0)) - 10.0).abs() < 1e-9);
        // Short 2 @ 100, mid 95 → +10
        e.position = -2.0;
        assert!((e.compute_unrealized_pnl(Some(95.0)) - 10.0).abs() < 1e-9);
    }
}
