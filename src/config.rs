use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

// ---------------------------------------------------------------------------
// config.json
// ---------------------------------------------------------------------------

#[derive(Deserialize, Clone)]
pub struct Config {
    pub trading: TradingConfig,
    #[serde(default)]
    pub performance: PerformanceConfig,
    #[serde(default)]
    pub websocket: WebsocketConfig,
    #[serde(default)]
    pub output: OutputConfig,
}

#[derive(Deserialize, Clone)]
pub struct TradingConfig {
    #[serde(default = "default_leverage")]
    pub leverage: u32,
    #[serde(default = "default_levels")]
    pub levels_per_side: usize,
    #[serde(default)]
    pub capital_usage_percent: f64,
    #[serde(default)]
    pub spread_factor_level1: f64,
    #[serde(default)]
    pub min_order_value_usd: f64,
    #[serde(default)]
    pub vol_obi: VolObiConfig,
    #[serde(default)]
    pub alpha: AlphaConfig,
}

#[derive(Deserialize, Clone, Default)]
pub struct VolObiConfig {
    #[serde(default = "default_window_steps")]
    pub window_steps: usize,
    #[serde(default = "default_step_ns")]
    pub step_ns: u64,
    #[serde(default)]
    pub vol_to_half_spread: f64,
    #[serde(default)]
    pub min_half_spread_bps: f64,
    #[serde(default)]
    pub c1_ticks: f64,
    #[serde(default)]
    pub skew: f64,
    #[serde(default = "default_looking_depth")]
    pub looking_depth: f64,
    #[serde(default = "default_min_warmup")]
    pub min_warmup_samples: u64,
    #[serde(default = "default_warmup_seconds")]
    pub warmup_seconds: f64,
}

#[derive(Deserialize, Clone, Default)]
pub struct AlphaConfig {
    #[serde(default = "default_alpha_source")]
    pub source: String,
    #[serde(default = "default_stale_seconds")]
    pub stale_seconds: f64,
    #[serde(default = "default_window_steps")]
    pub window_size: usize,
    #[serde(default = "default_alpha_min_samples")]
    pub min_samples: u64,
    #[serde(default = "default_looking_depth")]
    pub looking_depth: f64,
    #[serde(default = "default_bbo_min_samples")]
    pub bbo_min_samples: u64,
    #[serde(default = "default_stale_seconds")]
    pub bbo_stale_seconds: f64,
    #[serde(default = "default_depth_snapshot_limit")]
    pub depth_snapshot_limit: usize,
}

#[derive(Deserialize, Clone, Default)]
pub struct PerformanceConfig {
    #[serde(default = "default_min_loop_interval")]
    pub min_loop_interval: f64,
}

#[derive(Deserialize, Clone)]
pub struct OutputConfig {
    #[serde(default = "default_trade_log_enabled")]
    pub trade_log_enabled: bool,
    #[serde(default = "default_trade_log_rotate_bytes")]
    pub trade_log_rotate_bytes: u64,
    #[serde(default = "default_trade_log_compress_rotated")]
    pub trade_log_compress_rotated: bool,
    #[serde(default = "default_generated_output_max_bytes")]
    pub generated_output_max_bytes: u64,
    #[serde(default = "default_summary_log_rotate_bytes")]
    pub summary_log_rotate_bytes: u64,
    #[serde(default = "default_summary_log_max_files")]
    pub summary_log_max_files: usize,
    #[serde(default = "default_results_max_files")]
    pub results_max_files: usize,
    #[serde(default = "default_flush_interval_seconds")]
    pub flush_interval_seconds: f64,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            trade_log_enabled: default_trade_log_enabled(),
            trade_log_rotate_bytes: default_trade_log_rotate_bytes(),
            trade_log_compress_rotated: default_trade_log_compress_rotated(),
            generated_output_max_bytes: default_generated_output_max_bytes(),
            summary_log_rotate_bytes: default_summary_log_rotate_bytes(),
            summary_log_max_files: default_summary_log_max_files(),
            results_max_files: default_results_max_files(),
            flush_interval_seconds: default_flush_interval_seconds(),
        }
    }
}

#[derive(Deserialize, Clone)]
pub struct WebsocketConfig {
    #[serde(default = "default_ping_interval")]
    pub ping_interval: u64,
    #[serde(default = "default_recv_timeout")]
    pub recv_timeout: f64,
    #[serde(default = "default_reconnect_base")]
    pub reconnect_base_delay: u64,
    #[serde(default = "default_reconnect_max")]
    pub reconnect_max_delay: u64,
}

impl Default for WebsocketConfig {
    fn default() -> Self {
        Self {
            ping_interval: 20,
            recv_timeout: 30.0,
            reconnect_base_delay: 5,
            reconnect_max_delay: 60,
        }
    }
}

// Defaults
fn default_leverage() -> u32 {
    1
}
fn default_levels() -> usize {
    2
}
fn default_window_steps() -> usize {
    6000
}
fn default_step_ns() -> u64 {
    100_000_000
}
fn default_looking_depth() -> f64 {
    0.025
}
fn default_min_warmup() -> u64 {
    100
}
fn default_warmup_seconds() -> f64 {
    600.0
}
fn default_alpha_source() -> String {
    "binance".to_string()
}
fn default_stale_seconds() -> f64 {
    5.0
}
fn default_alpha_min_samples() -> u64 {
    150
}
fn default_bbo_min_samples() -> u64 {
    10
}
fn default_depth_snapshot_limit() -> usize {
    1000
}
fn default_min_loop_interval() -> f64 {
    0.1
}
fn default_ping_interval() -> u64 {
    20
}
fn default_recv_timeout() -> f64 {
    30.0
}
fn default_reconnect_base() -> u64 {
    5
}
fn default_reconnect_max() -> u64 {
    60
}
fn default_trade_log_enabled() -> bool {
    true
}
fn default_trade_log_rotate_bytes() -> u64 {
    128 * 1024
}
fn default_trade_log_compress_rotated() -> bool {
    true
}
fn default_generated_output_max_bytes() -> u64 {
    500 * 1024 * 1024
}
fn default_summary_log_rotate_bytes() -> u64 {
    10 * 1024 * 1024
}
fn default_summary_log_max_files() -> usize {
    3
}
fn default_results_max_files() -> usize {
    50
}
fn default_flush_interval_seconds() -> f64 {
    60.0
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let data = std::fs::read_to_string(path)?;
        // Strip comments (JSON doesn't support them, but config.json has _comment fields)
        let config: Config = serde_json::from_str(&data)?;
        Ok(config)
    }
}

// ---------------------------------------------------------------------------
// grid_config.json
// ---------------------------------------------------------------------------

#[derive(Deserialize, Clone)]
pub struct GridConfig {
    #[serde(default = "default_grid_capital")]
    pub capital: f64,
    #[serde(default = "default_leverage")]
    pub leverage: u32,
    #[serde(default = "default_warmup_seconds")]
    pub warmup_seconds: f64,
    #[serde(default = "default_summary_interval")]
    pub summary_interval_seconds: f64,
    #[serde(default = "default_sim_latency")]
    pub sim_latency_s: f64,
    #[serde(default = "default_maker_fee_rate")]
    pub maker_fee_rate: f64,
    pub parameters: HashMap<String, Vec<f64>>,
    #[serde(default)]
    pub fixed: HashMap<String, serde_json::Value>,
}

fn default_grid_capital() -> f64 {
    1000.0
}
fn default_summary_interval() -> f64 {
    60.0
}
fn default_sim_latency() -> f64 {
    0.050
}
fn default_maker_fee_rate() -> f64 {
    0.000_04
}

impl GridConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let data = std::fs::read_to_string(path)?;
        let config: GridConfig = serde_json::from_str(&data)?;
        Ok(config)
    }
}
