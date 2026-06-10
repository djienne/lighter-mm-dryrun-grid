# lighter-mm-dryrun-grid

A Rust market-making dry-run engine that runs hundreds of parameter combinations simultaneously against live Lighter DEX and Binance order book feeds, simulating fills in real time without placing real orders — to find optimal spread, skew, and alpha-sensitivity settings under actual market conditions.

⚡ Support this project — Trade on Lighter spot and perpetual markets through https://app.lighter.xyz/?referral=FREQTRADE (100% kickback with this link). Lighter Standard Accounts currently have 0 maker / 0 taker trading fees; Premium Accounts have separate fee tiers.

## Quick Start

```bash
cargo build --release

# Grid mode (default) — sweep parameter combinations
./target/release/lighter-mm-dryrun --symbol BTC

# Single dry-run — one slot with config.json params
./target/release/lighter-mm-dryrun --symbol BTC --dry-run --capital 1000
```

## Docker

Run the default BTC grid in the background:

```bash
docker compose build
docker compose up -d
docker compose ps
docker compose logs -f --tail=100
```

Stop gracefully:

```bash
docker compose stop
```

The Compose service uses `restart: unless-stopped`, handles Docker `SIGTERM` shutdown, persists generated files in `./logs`, and caps Docker stdout logs at `10m x 5`. Docker and native runs use the same host `./logs/grid/` output directory, so existing state and trade history are restored and appended to.

The runtime image is small, but Docker build cache can grow after rebuilds. To reclaim build cache without deleting the built image:

```bash
docker builder prune -f
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

### Parity with the live trader

The quoting logic mirrors the Python live trader (`lighter_MM/market_maker_v2.py`) formula-for-formula: same volatility/OBI calculator, sizing, max-position and level-placement math, the same 10 bps quote-update threshold (`trading.default_quote_update_threshold_bps`), and the same cancel-everything behavior whenever no usable quote exists (calculator not warmed up, crossed-quote guard, or zero position headroom). Remaining differences are intentional:

- The live trader recomputes order size and max position only on ~$10 mid moves or capital updates (a performance cache); the dry run recomputes every tick with the same formulas.
- The live trader paces order submission through a rate-limited mailbox and widens its update threshold under exchange quota pressure; the dry run has no quota, so ops apply directly with `sim_latency_s` simulated transit.
- The live trader re-quotes after 5 s even without a book update; in the dry run, mid, alpha, and position only change on book updates, so that fallback could never produce different quotes.
- Fills are simulated (POST_ONLY checks plus a new-liquidity delta model) — a real exchange's queue position cannot be replicated.

## Configuration

### `grid_config.json` (grid mode)

Defines the parameter sweep. All combinations in `parameters` are crossed (cartesian product); `fixed` values apply to every slot.

```json
{
  "capital": 1000,
  "leverage": 1,
  "warmup_seconds": 600,
  "summary_interval_seconds": 60,
  "sim_latency_s": 0.050,
  "parameters": {
    "vol_to_half_spread": [6, 10, 15, 21, 30, 42, 60, 80],
    "skew": [0.1, 0.5, 1.5, 3.0, 5.0],
    "c1_ticks": [5, 10, 20, 40, 80, 120, 160, 250, 350, 500, 600, 800, 1000]
  },
  "fixed": {
    "min_half_spread_bps": 4,
    "spread_factor_level1": 2.0,
    "capital_usage_percent": 0.12,
    "num_levels": 2
  }
}
```

An optional top-level `maker_fee_rate` (default `0.00004`, i.e. 0.004%) is deducted from PnL on every simulated fill. `sim_latency_s` models exchange round-trip time: orders only become fillable (and POST_ONLY is re-checked) after this delay.

### `config.json` (both modes)

Controls trading strategy defaults, vol/OBI windowing, alpha source, WebSocket settings, and output retention. Grid mode reads the `vol_obi` windowing, `alpha`, `min_order_value_usd`, `default_quote_update_threshold_bps`, and `output` settings from here; single dry-run mode additionally uses the `trading` strategy parameters directly. Notable: `trading.default_quote_update_threshold_bps` (default `10.0`) — resting orders are only modified when the new price moves more than this, matching the live trader at full quota. See `src/config.rs` for all fields and defaults.

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `LOG_DIR` | `logs` | Base directory for output; grid results go to `$LOG_DIR/grid/` |
| `RUST_LOG` | `info` | Log level (`debug`, `info`, `warn`, `error`) |

## Output

Grid results are written to `$LOG_DIR/grid/`:

- `state_<SYMBOL>_<param_key>.json` — checkpoint state per slot for restart continuity
- `trades_<SYMBOL>_<param_key>.csv` — active trade log per slot
- `trades_<SYMBOL>_<param_key>__rotated_<UTC>.csv.gz` — compressed rotated trade history
- `summary.log` — compact run summary
- `results_<SYMBOL>_<UTC>.csv` — final run snapshot

Output retention is configured in `config.json` under `output`. By default per-fill trade logs stay enabled, active CSV files rotate at 128 KiB, rotated chunks are gzip-compressed with maximum compression, and generated output is capped at 500 MiB. When the cap is reached, result files and summaries are pruned before compressed trade chunks so fill history lasts as long as possible. `state_*.json` files are not deleted by retention because they are required for restart continuity.

## Analysis

```bash
python3 check_grid_results.py                    # scan logs/grid/
python3 check_grid_results.py /path/to/grid/     # custom directory
python3 check_grid_results.py --top 20           # show top 20 slots
python3 check_grid_results.py --sort fills       # sort by fill count
python3 check_grid_results.py --fee 0.00005      # custom maker fee rate
```

The analyzer reads active `.csv` trade logs and rotated `.csv.gz` trade history. Outputs: overall summary, top/bottom performers, per-parameter average PnL, and v2hs x skew heatmaps.
