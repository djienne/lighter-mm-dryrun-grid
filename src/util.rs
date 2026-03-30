pub const EPSILON: f64 = 1e-9;

/// Compute basis-point change between two prices.
/// Returns infinity if either price is None/invalid.
pub fn price_change_bps(old_price: f64, new_price: f64) -> f64 {
    if old_price <= 0.0 {
        return f64::INFINITY;
    }
    (new_price - old_price).abs() / old_price * 10000.0
}

/// Generate deterministic, human-readable param key from grid parameters.
pub fn param_key(
    vol_to_half_spread: f64,
    min_half_spread_bps: f64,
    skew: f64,
    spread_factor_level1: f64,
    capital_usage_percent: f64,
    num_levels: usize,
    c1_ticks: f64,
) -> String {
    format!(
        "v{}_m{}_s{}_f{}_c{}_l{}_t{}",
        vol_to_half_spread, min_half_spread_bps, skew,
        spread_factor_level1, capital_usage_percent, num_levels, c1_ticks,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_price_change_bps() {
        assert!((price_change_bps(100.0, 101.0) - 100.0).abs() < 1e-10);
        assert!((price_change_bps(100.0, 100.0) - 0.0).abs() < 1e-10);
        assert!(price_change_bps(0.0, 100.0).is_infinite());
    }

    #[test]
    fn test_param_key() {
        let key = param_key(48.0, 8.0, 3.0, 2.0, 0.12, 2, 20.0);
        assert_eq!(key, "v48_m8_s3_f2_c0.12_l2_t20");
    }
}
