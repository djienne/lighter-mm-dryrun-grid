use std::time::Instant;

use crate::orderbook::Orderbook;

// ---------------------------------------------------------------------------
// Market state (shared across grid slots)
// ---------------------------------------------------------------------------

pub struct MarketState {
    pub mid_price: Option<f64>,
    pub last_order_book_update: Instant,
    pub ws_connection_healthy: bool,
    pub orderbook: Orderbook,
    pub ticker_best_bid: Option<f64>,
    pub ticker_best_ask: Option<f64>,
    pub ticker_updated_at: Instant,
}

impl MarketState {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            mid_price: None,
            last_order_book_update: now,
            ws_connection_healthy: false,
            orderbook: Orderbook::new(),
            ticker_best_bid: None,
            ticker_best_ask: None,
            ticker_updated_at: now,
        }
    }
}

// ---------------------------------------------------------------------------
// Market config (shared across grid slots)
// ---------------------------------------------------------------------------

pub struct MarketConfig {
    pub market_id: Option<i64>,
    pub price_tick_float: f64,
    pub amount_tick_float: f64,
    pub min_base_amount: f64,
    pub min_quote_amount: f64,
}

// ---------------------------------------------------------------------------
// Account state (per-slot)
// ---------------------------------------------------------------------------

pub struct AccountState {
    pub available_capital: Option<f64>,
    pub portfolio_value: Option<f64>,
    pub position_size: f64,
}

impl AccountState {
    pub fn new(capital: f64) -> Self {
        Self {
            available_capital: Some(capital),
            portfolio_value: Some(capital),
            position_size: 0.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Shared Binance state
// ---------------------------------------------------------------------------

pub struct SharedBBO {
    pub best_bid: f64,
    pub best_ask: f64,
    pub bid_qty: f64,
    pub ask_qty: f64,
    pub mid: f64,
    pub update_id: i64,
    pub last_update_time: Instant,
    pub sample_count: u64,
    warmed_up: bool,
    min_samples: u64,
}

impl SharedBBO {
    pub fn new(min_samples: u64) -> Self {
        Self {
            best_bid: 0.0,
            best_ask: 0.0,
            bid_qty: 0.0,
            ask_qty: 0.0,
            mid: 0.0,
            update_id: 0,
            last_update_time: Instant::now(),
            sample_count: 0,
            warmed_up: false,
            min_samples,
        }
    }

    pub fn update(
        &mut self,
        best_bid: f64,
        best_ask: f64,
        bid_qty: f64,
        ask_qty: f64,
        update_id: i64,
    ) {
        self.best_bid = best_bid;
        self.best_ask = best_ask;
        self.bid_qty = bid_qty;
        self.ask_qty = ask_qty;
        self.mid = (best_bid + best_ask) * 0.5;
        self.update_id = update_id;
        self.last_update_time = Instant::now();
        self.sample_count += 1;
        if !self.warmed_up && self.sample_count >= self.min_samples {
            self.warmed_up = true;
        }
    }

    pub fn is_stale(&self, threshold_seconds: f64) -> bool {
        self.last_update_time.elapsed().as_secs_f64() > threshold_seconds
    }

    pub fn warmed_up(&self) -> bool {
        self.warmed_up
    }

    pub fn reset(&mut self) {
        self.best_bid = 0.0;
        self.best_ask = 0.0;
        self.bid_qty = 0.0;
        self.ask_qty = 0.0;
        self.mid = 0.0;
        self.update_id = 0;
        self.sample_count = 0;
        self.warmed_up = false;
    }
}

pub struct SharedAlpha {
    pub alpha: f64,
    pub last_update_time: Instant,
    pub sample_count: u64,
    warmed_up: bool,
    min_samples: u64,
}

impl SharedAlpha {
    pub fn new(min_samples: u64) -> Self {
        Self {
            alpha: 0.0,
            last_update_time: Instant::now(),
            sample_count: 0,
            warmed_up: false,
            min_samples,
        }
    }

    pub fn update(&mut self, alpha: f64) {
        self.alpha = alpha;
        self.last_update_time = Instant::now();
        self.sample_count += 1;
        if !self.warmed_up && self.sample_count >= self.min_samples {
            self.warmed_up = true;
        }
    }

    pub fn is_stale(&self, threshold_seconds: f64) -> bool {
        self.last_update_time.elapsed().as_secs_f64() > threshold_seconds
    }

    pub fn warmed_up(&self) -> bool {
        self.warmed_up
    }

    pub fn reset(&mut self) {
        self.alpha = 0.0;
        self.sample_count = 0;
        self.warmed_up = false;
    }
}
