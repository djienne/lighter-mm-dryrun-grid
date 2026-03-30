#[allow(dead_code)]
mod config;
#[allow(dead_code)]
mod dry_run;
#[allow(dead_code)]
mod grid;
mod market_info;
#[allow(dead_code)]
mod order_manager;
mod orderbook;
#[allow(dead_code)]
mod rolling_stats;
#[allow(dead_code)]
mod trade_log;
#[allow(dead_code)]
mod types;
mod util;
#[allow(dead_code)]
mod vol_obi;
mod ws_binance;
mod ws_lighter;

use std::path::PathBuf;
use std::time::Instant;

use clap::Parser;
use tokio::sync::mpsc;

use config::{Config, GridConfig};
use grid::GridRunner;
use types::{MarketConfig, MarketState, SharedAlpha, SharedBBO};
use ws_binance::BinanceMsg;
use ws_lighter::LighterWsMsg;

#[derive(Parser)]
#[command(name = "lighter-mm-dryrun")]
#[command(about = "Lighter Market Maker Dry-Run (Rust)")]
struct Cli {
    /// Trading symbol (e.g., BTC, ETH)
    #[arg(long, default_value = "BTC")]
    symbol: String,

    /// Run in single dry-run mode (one slot with config.json params) instead of grid
    #[arg(long, name = "dry-run")]
    dry_run: bool,

    /// Path to grid config JSON for grid dry-run mode
    #[arg(long)]
    grid: Option<PathBuf>,

    /// Starting capital for single dry-run (default: 1000)
    #[arg(long, default_value = "1000")]
    capital: f64,

    /// Smoke test: run for N seconds then exit
    #[arg(long)]
    test: Option<u64>,

    /// Path to config.json (default: config.json)
    #[arg(long, default_value = "config.json")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let symbol = cli.symbol.to_uppercase();

    // Load config
    let app_config = Config::load(&cli.config)?;

    tracing::info!("=== LIGHTER MM DRY-RUN (Rust) starting: {} ===", symbol);

    // Fetch market details
    tracing::info!("Fetching market details for {}...", symbol);
    let market_details = market_info::get_market_details(&symbol).await?;
    let market_id = market_details.market_id;

    tracing::info!(
        "Market {}: id={}, tick(price)={}, tick(amount)={}",
        symbol, market_id, market_details.price_tick, market_details.amount_tick,
    );

    let market_config = MarketConfig {
        market_id: Some(market_id),
        price_tick_float: market_details.price_tick,
        amount_tick_float: market_details.amount_tick,
        min_base_amount: market_details.min_base_amount,
        min_quote_amount: market_details.min_quote_amount,
    };

    // Determine mode: --dry-run for single slot, otherwise grid (default: grid_config.json)
    if cli.dry_run {
        run_single_dry_run(&cli, &app_config, &market_config, market_id, &symbol).await
    } else {
        let grid_path = cli.grid.clone().unwrap_or_else(|| PathBuf::from("grid_config.json"));
        run_grid(&cli, &app_config, &market_config, market_id, &symbol, &grid_path).await
    }
}

async fn run_grid(
    cli: &Cli,
    app_config: &Config,
    market_config: &MarketConfig,
    market_id: i64,
    symbol: &str,
    grid_path: &PathBuf,
) -> anyhow::Result<()> {
    let grid_config = GridConfig::load(grid_path)?;
    run_with_grid(cli, app_config, market_config, market_id, symbol, &grid_config).await
}

async fn run_single_dry_run(
    cli: &Cli,
    app_config: &Config,
    market_config: &MarketConfig,
    market_id: i64,
    symbol: &str,
) -> anyhow::Result<()> {
    // Single dry-run = grid with 1 slot using config.json params
    let vol_obi = &app_config.trading.vol_obi;
    let grid_config = GridConfig {
        capital: cli.capital,
        leverage: app_config.trading.leverage,
        warmup_seconds: vol_obi.warmup_seconds,
        summary_interval_seconds: 60.0,
        sim_latency_s: 0.050,
        maker_fee_rate: 0.000_04,
        parameters: {
            let mut m = std::collections::HashMap::new();
            m.insert("vol_to_half_spread".to_string(), vec![vol_obi.vol_to_half_spread]);
            m
        },
        fixed: {
            let mut m = std::collections::HashMap::new();
            m.insert("min_half_spread_bps".into(), serde_json::json!(vol_obi.min_half_spread_bps));
            m.insert("skew".into(), serde_json::json!(vol_obi.skew));
            m.insert("spread_factor_level1".into(), serde_json::json!(app_config.trading.spread_factor_level1));
            m.insert("capital_usage_percent".into(), serde_json::json!(app_config.trading.capital_usage_percent));
            m.insert("num_levels".into(), serde_json::json!(app_config.trading.levels_per_side));
            m.insert("c1_ticks".into(), serde_json::json!(vol_obi.c1_ticks));
            m
        },
    };

    tracing::info!("Single dry-run mode | capital=${:.0}", cli.capital);
    run_with_grid(cli, app_config, market_config, market_id, symbol, &grid_config).await
}

async fn run_with_grid(
    cli: &Cli,
    app_config: &Config,
    market_config: &MarketConfig,
    market_id: i64,
    symbol: &str,
    grid_config: &GridConfig,
) -> anyhow::Result<()> {
    let mut runner = GridRunner::new(grid_config, app_config, symbol)?;
    runner.create_slots(market_config)?;

    // Channels
    let (lighter_tx, mut lighter_rx) = mpsc::channel::<LighterWsMsg>(256);
    let (binance_tx, mut binance_rx) = mpsc::channel::<BinanceMsg>(256);

    let mut market_state = MarketState::new();
    let mut shared_alpha = SharedAlpha::new(app_config.trading.alpha.min_samples);
    let mut shared_bbo = SharedBBO::new(app_config.trading.alpha.bbo_min_samples);
    let alpha_stale_seconds = app_config.trading.alpha.stale_seconds;

    // Spawn Lighter WS task
    let ws_cfg = app_config.websocket.clone();
    tokio::spawn(async move {
        ws_lighter::run_lighter_ws(
            market_id,
            lighter_tx,
            ws_cfg.ping_interval,
            ws_cfg.recv_timeout,
            ws_cfg.reconnect_base_delay,
            ws_cfg.reconnect_max_delay,
        )
        .await;
    });

    // Spawn Binance feeds
    if app_config.trading.alpha.source == "binance" {
        if let Some(binance_sym) = ws_binance::lighter_to_binance_symbol(symbol) {
            let tx1 = binance_tx.clone();
            let sym1 = binance_sym.clone();
            tokio::spawn(async move {
                ws_binance::run_binance_bbo(&sym1, tx1).await;
            });

            let tx2 = binance_tx.clone();
            let sym2 = binance_sym.clone();
            let window = app_config.trading.alpha.window_size;
            let depth = app_config.trading.alpha.looking_depth;
            let snap = app_config.trading.alpha.depth_snapshot_limit;
            tokio::spawn(async move {
                ws_binance::run_binance_depth(&sym2, tx2, window, depth, snap).await;
            });

            tracing::info!("Binance feeds: {}@bookTicker + {}@depth@100ms", binance_sym, binance_sym);
        }
    }

    // Main loop
    let warmup_seconds = runner.warmup_seconds();
    let start_time = Instant::now();
    let test_duration = cli.test.map(|s| std::time::Duration::from_secs(s));
    let mut warmup_complete = false;
    let mut last_warmup_log = Instant::now();

    // Handle both SIGINT (Ctrl+C) and SIGTERM (kill, Docker stop)
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("failed to register SIGTERM handler");

    tracing::info!("Entering main loop ({} slots), warmup period started ({}s)...", runner.slot_count(), warmup_seconds);

    loop {
        if let Some(dur) = test_duration {
            if start_time.elapsed() >= dur {
                tracing::info!("Test duration reached, shutting down...");
                break;
            }
        }

        tokio::select! {
            msg = lighter_rx.recv() => {
                let Some(msg) = msg else { break };
                match msg {
                    LighterWsMsg::OrderbookUpdate { bids, asks } => {
                        market_state.orderbook.apply_update(&bids, &asks, 100);
                        market_state.ws_connection_healthy = true;
                        market_state.last_order_book_update = Instant::now();

                        if let Some(mid) = market_state.orderbook.mid() {
                            market_state.mid_price = Some(mid);

                            let alpha_override = if shared_alpha.warmed_up()
                                && !shared_alpha.is_stale(alpha_stale_seconds)
                            {
                                Some(shared_alpha.alpha)
                            } else {
                                None
                            };

                            // Fan-out to all slots: feed calculator + check fills
                            let ob_bids = &market_state.orderbook.bids;
                            let ob_asks = &market_state.orderbook.asks;
                            for i in 0..runner.slot_count() {
                                runner.feed_slot_calculator(i, mid, ob_bids, ob_asks, alpha_override);
                                runner.check_slot_fills(i, ob_bids, ob_asks, Some(mid), Some(market_id));
                            }

                            // Tick slots if warmup complete
                            if warmup_complete {
                                let any_ready = runner.slots.iter().any(|s| s.calculator.warmed_up());
                                if any_ready {
                                    for i in 0..runner.slot_count() {
                                        runner.tick_slot(i, mid, market_config, ob_bids, ob_asks);
                                    }
                                }
                                if runner.should_log_summary() {
                                    runner.log_summary(mid);
                                    runner.flush_all();
                                }
                            } else {
                                let elapsed = start_time.elapsed().as_secs_f64();
                                if elapsed >= warmup_seconds {
                                    warmup_complete = true;
                                    tracing::info!("Warmup complete, starting trading");
                                } else if last_warmup_log.elapsed().as_secs() >= 60 {
                                    tracing::info!("Warmup: {:.0}/{:.0} seconds", elapsed, warmup_seconds);
                                    last_warmup_log = Instant::now();
                                }
                            }
                        } else {
                            market_state.mid_price = None;
                        }
                    }
                    LighterWsMsg::TickerUpdate { best_bid, best_ask } => {
                        if best_bid > 0.0 { market_state.ticker_best_bid = Some(best_bid); }
                        if best_ask > 0.0 { market_state.ticker_best_ask = Some(best_ask); }
                        market_state.ticker_updated_at = Instant::now();
                    }
                    LighterWsMsg::Disconnected => {
                        tracing::warn!("Lighter WS disconnected, clearing orderbook");
                        market_state.ws_connection_healthy = false;
                        market_state.mid_price = None;
                        market_state.orderbook.clear();
                        for slot in &mut runner.slots {
                            slot.calculator.reset();
                        }
                    }
                }
            }
            msg = binance_rx.recv() => {
                let Some(msg) = msg else { continue };
                match msg {
                    BinanceMsg::BboUpdate { best_bid, best_ask, bid_qty, ask_qty, update_id } => {
                        shared_bbo.update(best_bid, best_ask, bid_qty, ask_qty, update_id);
                    }
                    BinanceMsg::AlphaUpdate { alpha } => {
                        shared_alpha.update(alpha);
                    }
                    BinanceMsg::Disconnected { feed } => {
                        tracing::warn!("Binance {} disconnected", feed);
                        if feed == "bbo" { shared_bbo.reset(); } else { shared_alpha.reset(); }
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Received SIGINT, shutting down gracefully...");
                break;
            }
            _ = sigterm.recv() => {
                tracing::info!("Received SIGTERM, shutting down gracefully...");
                break;
            }
        }
    }

    // Shutdown
    tracing::info!("Saving all slot states...");
    runner.flush_all();
    runner.write_final_results();
    if let Some(mid) = market_state.mid_price {
        runner.log_summary(mid);
    }
    tracing::info!("=== DRY-RUN finished ===");

    Ok(())
}
