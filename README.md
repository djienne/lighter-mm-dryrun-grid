# lighter-mm-dryrun-grid

⚡ Support this project — Trade on Lighter – Spot & Perpetuals, 100% decentralized, no KYC, and ZERO fees – https://app.lighter.xyz/?referral=FREQTRADE (100% kickback with this link)

A Rust market-making dry-run engine that runs hundreds of parameter combinations simultaneously against live Lighter DEX and Binance order book feeds, simulating fills in real time without placing real orders — to find optimal spread, skew, and inventory decay settings under actual market conditions.

## Quick Start

```bash
cargo build --release

# Grid mode (default) — sweep parameter combinations
./target/release/lighter-mm-dryrun --symbol BTC

# Single dry-run — one slot with config.json params
./target/release/lighter-mm-dryrun --symbol BTC --dry-run --capital 1000
```

## CLI Flags

| Flag | Default | Description |
|------|---------|-------------|
| `--symbol` | `BTC` | Trading symbol (BTC, ETH, SOL, etc.) |
| `--dry-run` | off | Single-slot mode using `config.json` parameters |
| `--grid <path>` | `grid_config.json` | Path to grid config for parameter sweep |
| `--capital <f64>` | `1000` | Starting capital (single dry-run mode) |
| `--test <secs>` | none | Run for N seconds then exit |
| `--config <path>` | `config.json` | Path to main config file |

## Strategy

Quotes are computed using a volatility + order book imbalance (OBI) model:

1. **Volatility** is estimated as the rolling standard deviation of mid-price changes (Welford's algorithm), scaled to a per-second rate.
2. **OBI alpha** is the z-score of `Σ(bid sizes) − Σ(ask sizes)` within a configurable depth window. When available and fresh, Binance depth-feed OBI overrides the local Lighter OBI.
3. **Fair price** is shifted from mid by the alpha signal: `fair = mid + c1_ticks × tick_size × alpha`.
4. **Half-spread** (in ticks) is `volatility × vol_to_half_spread / tick_size`, floored by `min_half_spread_bps`.
5. **Inventory skew** widens the spread on the side of existing exposure and tightens the opposite side: `bid_depth = half_spread × (1 + skew × norm_pos)`, `ask_depth = half_spread × (1 − skew × norm_pos)`, where `norm_pos` is the position normalized to max dollar size, clamped to [-1, 1].
6. Final bid/ask are snapped to the tick grid.

### Key Parameters

| Parameter | Role |
|-----------|------|
| `vol_to_half_spread` | Multiplier from volatility to half-spread width |
| `skew` | Inventory lean intensity — how aggressively quotes shift to flatten position |
| `c1_ticks` | Alpha sensitivity — how many ticks the fair price moves per unit of OBI z-score |
| `min_half_spread_bps` | Minimum half-spread floor in basis points |
| `spread_factor_level1` | Width multiplier for the second quoting level |
| `capital_usage_percent` | Fraction of capital used per level |

## Configuration

### `grid_config.json` (grid mode)

Defines the parameter sweep. All combinations in `parameters` are crossed (cartesian product); `fixed` values apply to every slot.

```json
{
  "capital": 1000,
  "warmup_seconds": 600,
  "sim_latency_s": 0.050,
  "parameters": {
    "vol_to_half_spread": [6, 10, 15, 21, 30, 42, 60, 80],
    "skew": [0.1, 0.5, 1.5, 3.0, 5.0],
    "c1_ticks": [5, 10, 20, 40, 80, 120, 160, 250, 350, 500]
  },
  "fixed": {
    "min_half_spread_bps": 4,
    "spread_factor_level1": 2.0,
    "capital_usage_percent": 0.12,
    "num_levels": 2
  }
}
```

### `config.json` (single dry-run mode)

Controls trading strategy, alpha source, WebSocket settings, and performance tuning. See `src/config.rs` for all fields and defaults.

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `LOG_DIR` | `logs` | Base directory for output; grid results go to `$LOG_DIR/grid/` |
| `RUST_LOG` | `info` | Log level (`debug`, `info`, `warn`, `error`) |

## Output

Grid results are written to `$LOG_DIR/grid/`:

- `state_<SYMBOL>_<param_key>.json` — final state per slot (PnL, fill count, volume)
- `trades_<SYMBOL>_<param_key>.csv` — trade log per slot

## Analysis

```bash
python check_grid_results.py                    # scan logs/grid/
python check_grid_results.py /path/to/grid/     # custom directory
python check_grid_results.py --top 20           # show top 20 slots
python check_grid_results.py --sort fills       # sort by fill count
python check_grid_results.py --fee 0.00005      # custom maker fee rate
```

Outputs: overall summary, top/bottom performers, per-parameter average PnL, and v2hs x skew heatmaps.
