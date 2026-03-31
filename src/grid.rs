use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use crate::config::{Config, GridConfig};
use crate::dry_run::DryRunEngine;
use crate::order_manager::{Action, BatchOp, OrderManager, OrderState, Side};
use crate::trade_log::TradeLogger;
use crate::types::{AccountState, MarketConfig};
use crate::util;
use crate::vol_obi::VolObiCalculator;

const MAX_CLIENT_ORDER_INDEX: i64 = (1i64 << 48) - 1;

// ---------------------------------------------------------------------------
// Grid parameters
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct GridParams {
    pub vol_to_half_spread: f64,
    pub min_half_spread_bps: f64,
    pub skew: f64,
    pub spread_factor_level1: f64,
    pub capital_usage_percent: f64,
    pub num_levels: usize,
    pub c1_ticks: f64,
    pub label: String,
}

impl GridParams {
    pub fn param_key(&self) -> String {
        util::param_key(
            self.vol_to_half_spread,
            self.min_half_spread_bps,
            self.skew,
            self.spread_factor_level1,
            self.capital_usage_percent,
            self.num_levels,
            self.c1_ticks,
        )
    }
}

// ---------------------------------------------------------------------------
// GridSlot — one parameter combination
// ---------------------------------------------------------------------------

pub struct GridSlot {
    pub index: usize,
    pub label: String,
    pub param_key: String,
    pub params: GridParams,
    // Per-slot state
    pub account: AccountState,
    pub orders: OrderState,
    pub order_manager: OrderManager,
    pub calculator: VolObiCalculator,
    pub dry_engine: DryRunEngine,
    pub trade_logger: TradeLogger,
    pub client_to_exchange_id: HashMap<i64, i64>,
    pub spread_factors: Vec<f64>,
    pub last_cid: i64,
}

impl GridSlot {
    pub fn next_client_order_index(&mut self) -> i64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;
        let mut new_id = now % (MAX_CLIENT_ORDER_INDEX + 1);
        if new_id <= self.last_cid {
            new_id = (self.last_cid + 1) % (MAX_CLIENT_ORDER_INDEX + 1);
        }
        self.last_cid = new_id;
        new_id
    }
}

// ---------------------------------------------------------------------------
// GridRunner
// ---------------------------------------------------------------------------

pub struct GridRunner {
    symbol: String,
    capital: f64,
    leverage: u32,
    warmup_seconds: f64,
    summary_interval: f64,
    sim_latency: f64,
    maker_fee_rate: f64,
    min_order_value_usd: f64,
    // Vol OBI config
    vol_obi_window_steps: usize,
    vol_obi_step_ns: u64,
    vol_obi_looking_depth: f64,
    vol_obi_min_warmup_samples: u64,
    // Param combos
    param_combos: Vec<GridParams>,
    // Runtime
    pub slots: Vec<GridSlot>,
    grid_dir: PathBuf,
    start_time: Instant,
    last_summary: Instant,
}

impl GridRunner {
    pub fn new(
        grid_config: &GridConfig,
        app_config: &Config,
        symbol: &str,
    ) -> anyhow::Result<Self> {
        let vol_obi = &app_config.trading.vol_obi;

        // Build Cartesian product
        let mut axis_names: Vec<String> = grid_config.parameters.keys().cloned().collect();
        axis_names.sort();
        let axis_values: Vec<&Vec<f64>> =
            axis_names.iter().map(|k| &grid_config.parameters[k]).collect();

        if axis_values.is_empty() {
            anyhow::bail!("Grid config 'parameters' must contain at least one axis");
        }

        let mut param_combos = Vec::new();
        let mut indices = vec![0usize; axis_names.len()];
        let fixed = &grid_config.fixed;

        loop {
            let mut kw = HashMap::new();
            // Apply fixed values
            for (k, v) in fixed {
                kw.insert(k.clone(), v.clone());
            }
            // Apply current combo
            for (i, name) in axis_names.iter().enumerate() {
                kw.insert(
                    name.clone(),
                    serde_json::Value::from(axis_values[i][indices[i]]),
                );
            }

            let label = format!("s{:03}", param_combos.len());
            let params = GridParams {
                vol_to_half_spread: get_f64(&kw, "vol_to_half_spread", 48.0),
                min_half_spread_bps: get_f64(&kw, "min_half_spread_bps", 8.0),
                skew: get_f64(&kw, "skew", 3.0),
                spread_factor_level1: get_f64(&kw, "spread_factor_level1", 2.0),
                capital_usage_percent: get_f64(&kw, "capital_usage_percent", 0.12),
                num_levels: get_f64(&kw, "num_levels", 2.0) as usize,
                c1_ticks: get_f64(&kw, "c1_ticks", 20.0),
                label,
            };
            param_combos.push(params);

            // Advance indices (Cartesian product iteration)
            let mut carry = true;
            for i in (0..indices.len()).rev() {
                if carry {
                    indices[i] += 1;
                    if indices[i] >= axis_values[i].len() {
                        indices[i] = 0;
                    } else {
                        carry = false;
                    }
                }
            }
            if carry {
                break;
            }
        }

        if param_combos.len() > 2000 {
            anyhow::bail!(
                "Grid too large: {} combos (max 2000)",
                param_combos.len()
            );
        }

        tracing::info!(
            "Grid config: {} parameter combos, capital=${:.0}, leverage={}",
            param_combos.len(),
            grid_config.capital,
            grid_config.leverage,
        );

        let now = Instant::now();
        let log_dir = std::env::var("LOG_DIR").unwrap_or_else(|_| "logs".to_string());
        let grid_dir = PathBuf::from(&log_dir).join("grid");

        Ok(Self {
            symbol: symbol.to_uppercase(),
            capital: grid_config.capital,
            leverage: grid_config.leverage,
            warmup_seconds: grid_config.warmup_seconds,
            summary_interval: grid_config.summary_interval_seconds,
            sim_latency: grid_config.sim_latency_s,
            maker_fee_rate: grid_config.maker_fee_rate,
            min_order_value_usd: app_config.trading.min_order_value_usd,
            vol_obi_window_steps: vol_obi.window_steps,
            vol_obi_step_ns: vol_obi.step_ns,
            vol_obi_looking_depth: vol_obi.looking_depth,
            vol_obi_min_warmup_samples: vol_obi.min_warmup_samples,
            param_combos,
            slots: Vec::new(),
            grid_dir,
            start_time: now,
            last_summary: now,
        })
    }

    /// Create all grid slots. Call after market config is known.
    pub fn create_slots(&mut self, market_config: &MarketConfig) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.grid_dir)?;
        let tick = market_config.price_tick_float;
        let _amount_tick = market_config.amount_tick_float;

        for (i, params) in self.param_combos.iter().enumerate() {
            let pk = params.param_key();
            let n_levels = params.num_levels;

            let mut account = AccountState::new(self.capital);

            let calc = VolObiCalculator::new(
                tick,
                self.vol_obi_window_steps,
                self.vol_obi_step_ns,
                params.vol_to_half_spread,
                params.min_half_spread_bps,
                params.c1_ticks,
                0.0,
                params.skew,
                self.vol_obi_looking_depth,
                self.vol_obi_min_warmup_samples,
                500.0,
            );

            let state_path = self.grid_dir.join(format!("state_{}_{}.json", self.symbol, pk));

            let mut engine = DryRunEngine::new(
                self.leverage,
                self.sim_latency,
                self.maker_fee_rate,
                Some(state_path.clone()),
            );

            // Try to restore from previous run
            if let Some(saved) = DryRunEngine::load_state(&state_path) {
                account.available_capital = Some(saved.available_capital);
                account.portfolio_value = Some(saved.portfolio_value);
                account.position_size = saved.position;
                engine.restore_from(&saved);
                tracing::info!(
                    "Grid slot {} ({}): restored | capital=${:.2} pos={:.6} pnl=${:.4} fills={}",
                    params.label, pk, saved.available_capital, saved.position,
                    saved.realized_pnl, saved.fill_count,
                );
            } else {
                engine.capture_initial_state(self.capital, self.capital, 0.0, None);
                tracing::info!("Grid slot {} ({}): fresh | capital=${:.0}", params.label, pk, self.capital);
            }

            let trade_logger =
                TradeLogger::new(&self.grid_dir, &format!("{}_{}", self.symbol, pk))?;

            let spread_factors: Vec<f64> = (0..n_levels)
                .map(|lvl| params.spread_factor_level1.powi(lvl as i32))
                .collect();

            self.slots.push(GridSlot {
                index: i,
                label: params.label.clone(),
                param_key: pk,
                params: params.clone(),
                account,
                orders: OrderState::new(n_levels),
                order_manager: OrderManager::new(n_levels),
                calculator: calc,
                dry_engine: engine,
                trade_logger,
                client_to_exchange_id: HashMap::new(),
                spread_factors,
                last_cid: 0,
            });
        }

        tracing::info!("Created {} grid slots", self.slots.len());
        Ok(())
    }

    /// Feed orderbook update to all slots (calculator + fill check).
    pub fn on_book_update(
        &mut self,
        _mid: f64,
        alpha_override: Option<f64>,
        _market_config: &MarketConfig,
    ) {
        // We can't iterate mutably over slots while also passing shared book references.
        // Since each slot has its own calculator and engine, we iterate by index.
        for i in 0..self.slots.len() {
            let slot = &mut self.slots[i];
            slot.calculator.set_alpha_override(alpha_override);
            // Note: We need the shared orderbook from MarketState, which the caller manages.
            // The caller should feed the bids/asks directly.
        }
    }

    /// Feed calculator update for a specific slot.
    pub fn feed_slot_calculator(
        &mut self,
        slot_idx: usize,
        mid: f64,
        bids: &crate::orderbook::BookSide,
        asks: &crate::orderbook::BookSide,
        alpha_override: Option<f64>,
    ) {
        let slot = &mut self.slots[slot_idx];
        slot.calculator.set_alpha_override(alpha_override);
        slot.calculator.on_book_update(mid, bids, asks);
    }

    /// Check fills for a specific slot.
    pub fn check_slot_fills(
        &mut self,
        slot_idx: usize,
        bids: &crate::orderbook::BookSide,
        asks: &crate::orderbook::BookSide,
        mid_price: Option<f64>,
        market_id: Option<i64>,
    ) {
        let slot = &mut self.slots[slot_idx];
        let cap = slot.account.available_capital.unwrap_or(0.0);
        let pv = slot.account.portfolio_value.unwrap_or(0.0);
        let pos = slot.account.position_size;
        let mut cap_mut = cap;
        let mut pv_mut = pv;
        let mut pos_mut = pos;

        slot.dry_engine.check_fills(
            bids,
            asks,
            &mut slot.orders,
            &mut slot.order_manager,
            &mut cap_mut,
            &mut pv_mut,
            &mut pos_mut,
            mid_price,
            market_id,
            Some(&mut slot.trade_logger),
        );

        slot.account.available_capital = Some(cap_mut);
        slot.account.portfolio_value = Some(pv_mut);
        slot.account.position_size = pos_mut;
    }

    /// Tick one slot: compute quotes, collect ops, process batch.
    pub fn tick_slot(
        &mut self,
        slot_idx: usize,
        mid: f64,
        market_config: &MarketConfig,
        bids: &crate::orderbook::BookSide,
        asks: &crate::orderbook::BookSide,
    ) {
        let slot = &mut self.slots[slot_idx];
        let capital = match slot.account.available_capital {
            Some(c) if c > 0.0 => c,
            _ => return,
        };

        let base_amount = compute_base_amount(
            mid, capital, slot.params.capital_usage_percent,
            self.leverage, market_config, self.min_order_value_usd,
        );
        let base_amount = match base_amount {
            Some(a) if a > 0.0 => a,
            _ => return,
        };

        let max_pos = compute_max_pos(mid, capital, base_amount, slot.params.num_levels, self.leverage);

        if !slot.calculator.warmed_up() {
            return;
        }
        if max_pos > 0.0 {
            slot.calculator.set_max_position_dollar(max_pos);
        }

        let quote = match slot.calculator.quote(mid, slot.account.position_size) {
            Some(q) => q,
            None => return,
        };

        let (mut buy_0, mut sell_0) = (Some(quote.0), Some(quote.1));

        // Position limit suppression
        if max_pos > 0.0 {
            let pos_val = slot.account.position_size.abs() * mid;
            if pos_val >= max_pos {
                if slot.account.position_size > 0.0 {
                    buy_0 = None;
                } else if slot.account.position_size < 0.0 {
                    sell_0 = None;
                }
                if buy_0.is_none() && sell_0.is_none() {
                    return;
                }
            }
        }

        // Build level prices
        let tick = market_config.price_tick_float;
        let mut levels = Vec::with_capacity(slot.params.num_levels);
        levels.push((buy_0, sell_0));

        let bid_depth = buy_0.map(|b| mid - b);
        let ask_depth = sell_0.map(|a| a - mid);

        for lvl in 1..slot.params.num_levels {
            let factor = slot.spread_factors[lvl];
            let raw_bid = bid_depth.map(|d| {
                let raw = mid - d * factor;
                if tick > 0.0 { (raw / tick).floor() * tick } else { raw }
            });
            let raw_ask = ask_depth.map(|d| {
                let raw = mid + d * factor;
                if tick > 0.0 { (raw / tick).ceil() * tick } else { raw }
            });
            levels.push((raw_bid, raw_ask));
        }

        // Collect ops
        let ops = collect_slot_ops(slot, &levels, base_amount, market_config);

        if !ops.is_empty() {
            let best_bid = bids.keys().next_back().map(|k| k.into_inner());
            let best_ask = asks.keys().next().map(|k| k.into_inner());
            slot.dry_engine.process_batch(
                &ops,
                &mut slot.orders,
                &mut slot.order_manager,
                &mut slot.client_to_exchange_id,
                best_bid,
                best_ask,
                bids,
                asks,
            );
        }
    }

    /// Check if grid summary should be logged.
    pub fn should_log_summary(&self) -> bool {
        self.last_summary.elapsed().as_secs_f64() >= self.summary_interval
    }

    /// Log grid summary to console and compact line to file.
    pub fn log_summary(&mut self, mid: f64) {
        self.last_summary = Instant::now();
        let elapsed = self.start_time.elapsed().as_secs_f64();
        let elapsed_str = format!("{:.1}h", elapsed / 3600.0);

        let mut best_pnl = f64::NEG_INFINITY;
        let mut best_label = String::new();
        let mut total_fills: u64 = 0;
        let mut total_volume: f64 = 0.0;
        let mut slots_with_fills: usize = 0;

        // Update portfolio values and find best slot
        for slot in &mut self.slots {
            let unrealized = slot.dry_engine.compute_unrealized_pnl(Some(mid));
            let total = slot.dry_engine.realized_pnl + unrealized;
            slot.account.portfolio_value =
                Some(slot.dry_engine.initial_portfolio_value + slot.dry_engine.realized_pnl + unrealized);
            total_fills += slot.dry_engine.fill_count;
            total_volume += slot.dry_engine.total_volume;
            if slot.dry_engine.fill_count > 0 {
                slots_with_fills += 1;
            }
            if total > best_pnl {
                best_pnl = total;
                best_label = format!(
                    "{} (v2hs={}, skew={}, c1={})",
                    slot.params.label,
                    slot.params.vol_to_half_spread,
                    slot.params.skew,
                    slot.params.c1_ticks,
                );
            }
        }

        // Compact console summary: just top 10 + stats
        let mut sorted: Vec<(usize, f64)> = self.slots.iter().enumerate().map(|(i, s)| {
            let u = s.dry_engine.compute_unrealized_pnl(Some(mid));
            (i, s.dry_engine.realized_pnl + u)
        }).collect();
        sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut lines = Vec::new();
        lines.push(format!(
            "GRID ({} slots, {} elapsed, mid=${:.2}) fills={} active={} vol=${:.0}",
            self.slots.len(), elapsed_str, mid, total_fills, slots_with_fills, total_volume,
        ));
        lines.push(format!(
            "{:<5} | {:>5} | {:>5} | {:>5} | {:>5} | {:>9} | {:>9}",
            "Slot", "v2hs", "skew", "c1", "Fills", "Total", "Volume"
        ));
        // Show top 10 only
        for &(i, pnl) in sorted.iter().take(10) {
            let s = &self.slots[i];
            lines.push(format!(
                "{:<5} | {:>5.1} | {:>5.1} | {:>5.0} | {:>5} | ${:>8.4} | ${:>8.2}",
                s.params.label, s.params.vol_to_half_spread, s.params.skew,
                s.params.c1_ticks,
                s.dry_engine.fill_count, pnl, s.dry_engine.total_volume,
            ));
        }
        if !best_label.is_empty() {
            lines.push(format!("Best: {} ${:.4}", best_label, best_pnl));
        }
        tracing::info!("\n{}", lines.join("\n"));

        // Compact one-line append to summary.log (no full table)
        let summary_path = self.grid_dir.join("summary.log");
        let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");

        // Rotate if > 10MB
        if let Ok(meta) = std::fs::metadata(&summary_path) {
            if meta.len() > 10 * 1024 * 1024 {
                let rotated = self.grid_dir.join("summary.log.1");
                let _ = std::fs::rename(&summary_path, &rotated);
            }
        }

        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&summary_path)
            .and_then(|mut f| {
                use std::io::Write;
                writeln!(
                    f, "{} mid={:.2} fills={} active={} vol={:.0} best={} ${:.4}",
                    ts, mid, total_fills, slots_with_fills, total_volume, best_label, best_pnl,
                )
            });
    }

    /// Flush all slot states and trade logs to disk.
    pub fn flush_all(&mut self) {
        for slot in &mut self.slots {
            let cap = slot.account.available_capital.unwrap_or(0.0);
            let pv = slot.account.portfolio_value.unwrap_or(0.0);
            slot.dry_engine.save_state(cap, pv);
            let _ = slot.trade_logger.flush();
        }
    }

    /// Write final CSV results.
    pub fn write_final_results(&self) {
        let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let csv_path = self.grid_dir.join(format!("results_{}_{}.csv", self.symbol, ts));

        let file = match std::fs::File::create(&csv_path) {
            Ok(f) => f,
            Err(e) => {
                tracing::error!("Failed to create results CSV: {}", e);
                return;
            }
        };
        let mut wtr = csv::Writer::from_writer(file);

        let _ = wtr.write_record([
            "slot", "param_key",
            "vol_to_half_spread", "min_half_spread_bps", "skew",
            "spread_factor_level1", "capital_usage_percent", "num_levels", "c1_ticks",
            "fills", "realized_pnl", "unrealized_pnl", "total_pnl",
            "total_volume", "portfolio_value",
        ]);

        for slot in &self.slots {
            let e = &slot.dry_engine;
            let p = &slot.params;
            let unrealized = e.compute_unrealized_pnl(None); // mid not available at shutdown
            let total = e.realized_pnl + unrealized;
            let pv = slot.account.portfolio_value.unwrap_or(0.0);

            let _ = wtr.write_record(&[
                &p.label,
                &slot.param_key,
                &p.vol_to_half_spread.to_string(),
                &p.min_half_spread_bps.to_string(),
                &p.skew.to_string(),
                &p.spread_factor_level1.to_string(),
                &p.capital_usage_percent.to_string(),
                &p.num_levels.to_string(),
                &p.c1_ticks.to_string(),
                &e.fill_count.to_string(),
                &format!("{:.6}", e.realized_pnl),
                &format!("{:.6}", unrealized),
                &format!("{:.6}", total),
                &format!("{:.2}", e.total_volume),
                &format!("{:.2}", pv),
            ]);
        }

        let _ = wtr.flush();
        tracing::info!("Final results written to {}", csv_path.display());
    }

    pub fn warmup_seconds(&self) -> f64 {
        self.warmup_seconds
    }

    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

fn get_f64(map: &HashMap<String, serde_json::Value>, key: &str, default: f64) -> f64 {
    map.get(key)
        .and_then(|v| v.as_f64())
        .unwrap_or(default)
}

fn compute_base_amount(
    mid: f64,
    capital: f64,
    cap_pct: f64,
    leverage: u32,
    config: &MarketConfig,
    min_order_value_usd: f64,
) -> Option<f64> {
    if mid <= 0.0 || capital <= 0.0 {
        return None;
    }
    let usd = capital * cap_pct * leverage as f64;
    let mut size = usd / mid;
    let tick = config.amount_tick_float;
    if tick > 0.0 {
        size = (size / tick).round() * tick;
    }
    if config.min_base_amount > 0.0 && size < config.min_base_amount {
        size = config.min_base_amount;
    }
    if config.min_quote_amount > 0.0 && size * mid < config.min_quote_amount {
        size = config.min_quote_amount / mid;
        if tick > 0.0 {
            size = (size / tick).ceil() * tick;
        }
    }
    if min_order_value_usd > 0.0 && size * mid < min_order_value_usd {
        size = min_order_value_usd / mid;
        if tick > 0.0 {
            size = (size / tick).ceil() * tick;
        }
    }
    Some(size)
}

fn compute_max_pos(
    mid: f64,
    capital: f64,
    base_amount: f64,
    num_levels: usize,
    leverage: u32,
) -> f64 {
    if capital <= 0.0 || mid <= 0.0 {
        return 0.0;
    }
    let mut raw = capital * leverage as f64;
    if base_amount > 0.0 {
        raw -= 2.0 * num_levels as f64 * base_amount * mid;
    }
    (raw * 0.9).max(0.0)
}

fn collect_slot_ops(
    slot: &mut GridSlot,
    level_prices: &[(Option<f64>, Option<f64>)],
    base_amount: f64,
    market_config: &MarketConfig,
) -> Vec<BatchOp> {
    let mut ops = Vec::new();
    let threshold = 10.0; // fixed threshold in dry-run
    let amount_tick = market_config.amount_tick_float;

    for (level, &(buy_price, sell_price)) in level_prices.iter().enumerate() {
        for (is_buy, new_price) in [(true, buy_price), (false, sell_price)] {
            let side = if is_buy { Side::Buy } else { Side::Sell };

            if new_price.is_none() {
                // Cancel existing if any
                let existing_id = if is_buy {
                    slot.orders.bid_order_ids[level]
                } else {
                    slot.orders.ask_order_ids[level]
                };
                if let Some(eid_key) = existing_id {
                    if let Some(&exchange_id) = slot.client_to_exchange_id.get(&eid_key) {
                        ops.push(BatchOp {
                            side,
                            level,
                            action: Action::Cancel,
                            price: 0.0,
                            size: 0.0,
                            order_id: eid_key,
                            exchange_id,
                        });
                    }
                }
                continue;
            }
            let new_price = new_price.unwrap();
            if new_price <= 0.0 {
                continue;
            }

            let (existing_id, existing_price, existing_size) = if is_buy {
                (
                    slot.orders.bid_order_ids[level],
                    slot.orders.bid_prices[level],
                    slot.orders.bid_sizes[level],
                )
            } else {
                (
                    slot.orders.ask_order_ids[level],
                    slot.orders.ask_prices[level],
                    slot.orders.ask_sizes[level],
                )
            };

            if let Some(eid_key) = existing_id {
                let exchange_id = match slot.client_to_exchange_id.get(&eid_key) {
                    Some(&eid) => eid,
                    None => continue,
                };
                let change_bps = match existing_price {
                    Some(ep) => util::price_change_bps(ep, new_price),
                    None => f64::INFINITY,
                };
                let size_changed = match existing_size {
                    Some(es) => {
                        let min_diff = if amount_tick > 0.0 { amount_tick } else { util::EPSILON };
                        (es - base_amount).abs() >= min_diff.max(util::EPSILON)
                    }
                    None => true,
                };
                if existing_price.is_some() && change_bps <= threshold && !size_changed {
                    continue;
                }
                ops.push(BatchOp {
                    side,
                    level,
                    action: Action::Modify,
                    price: new_price,
                    size: base_amount,
                    order_id: eid_key,
                    exchange_id,
                });
            } else {
                let new_order_id = slot.next_client_order_index();
                ops.push(BatchOp {
                    side,
                    level,
                    action: Action::Create,
                    price: new_price,
                    size: base_amount,
                    order_id: new_order_id,
                    exchange_id: 0,
                });
            }
        }
    }
    ops
}
