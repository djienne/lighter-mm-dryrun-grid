use std::collections::BTreeMap;

use futures_util::StreamExt;
use ordered_float::OrderedFloat;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;

use crate::rolling_stats::RollingStats;

const BINANCE_FUTURES_WS: &str = "wss://fstream.binance.com/ws";
const BINANCE_FUTURES_REST: &str = "https://fapi.binance.com";

// ---------------------------------------------------------------------------
// Symbol mapping
// ---------------------------------------------------------------------------

pub fn lighter_to_binance_symbol(symbol: &str) -> Option<String> {
    match symbol.to_uppercase().as_str() {
        "BTC" => Some("btcusdt".to_string()),
        "ETH" => Some("ethusdt".to_string()),
        "SOL" => Some("solusdt".to_string()),
        "PAXG" => Some("paxgusdt".to_string()),
        "CRV" => Some("crvusdt".to_string()),
        "ASTER" => None,
        other => Some(format!("{}usdt", other.to_lowercase())),
    }
}

// ---------------------------------------------------------------------------
// Messages sent to the main loop
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum BinanceMsg {
    BboUpdate {
        best_bid: f64,
        best_ask: f64,
        bid_qty: f64,
        ask_qty: f64,
        update_id: i64,
    },
    AlphaUpdate {
        alpha: f64,
    },
    Disconnected {
        feed: &'static str,
    },
}

// ---------------------------------------------------------------------------
// BinanceBookTickerClient
// ---------------------------------------------------------------------------

pub async fn run_binance_bbo(binance_symbol: &str, tx: mpsc::Sender<BinanceMsg>) {
    let url = format!("{}/{}@bookTicker", BINANCE_FUTURES_WS, binance_symbol);
    let mut backoff = 2u64;

    loop {
        match connect_async(&url).await {
            Ok((ws_stream, _)) => {
                tracing::info!("Binance BBO connected: {}", url);
                let (_write, mut read) = ws_stream.split();
                backoff = 2;

                loop {
                    let msg =
                        tokio::time::timeout(tokio::time::Duration::from_secs(30), read.next())
                            .await;
                    match msg {
                        Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text)))) => {
                            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&text) {
                                let bb = data
                                    .get("b")
                                    .and_then(|v| v.as_str())
                                    .and_then(|s| s.parse::<f64>().ok());
                                let ba = data
                                    .get("a")
                                    .and_then(|v| v.as_str())
                                    .and_then(|s| s.parse::<f64>().ok());
                                let bq = data
                                    .get("B")
                                    .and_then(|v| v.as_str())
                                    .and_then(|s| s.parse::<f64>().ok());
                                let aq = data
                                    .get("A")
                                    .and_then(|v| v.as_str())
                                    .and_then(|s| s.parse::<f64>().ok());
                                let uid = data.get("u").and_then(|v| v.as_i64());

                                if let (Some(bb), Some(ba), Some(bq), Some(aq), Some(uid)) =
                                    (bb, ba, bq, aq, uid)
                                {
                                    if bb > 0.0 && ba > 0.0 && ba > bb {
                                        let _ = tx
                                            .send(BinanceMsg::BboUpdate {
                                                best_bid: bb,
                                                best_ask: ba,
                                                bid_qty: bq,
                                                ask_qty: aq,
                                                update_id: uid,
                                            })
                                            .await;
                                    }
                                }
                            }
                        }
                        Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_)))) | Ok(None) => break,
                        Ok(Some(Err(e))) => {
                            tracing::warn!("Binance BBO error: {}", e);
                            break;
                        }
                        Err(_) => {
                            tracing::warn!("Binance BBO: no data for 30s");
                            break;
                        }
                        _ => {}
                    }
                }

                let _ = tx
                    .send(BinanceMsg::Disconnected { feed: "bbo" })
                    .await;
            }
            Err(e) => {
                tracing::warn!("Binance BBO connect failed: {}", e);
            }
        }

        tokio::time::sleep(tokio::time::Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(60);
    }
}

// ---------------------------------------------------------------------------
// BinanceDiffDepthClient
// ---------------------------------------------------------------------------

type BinanceBook = BTreeMap<OrderedFloat<f64>, f64>;

pub async fn run_binance_depth(
    binance_symbol: &str,
    tx: mpsc::Sender<BinanceMsg>,
    window_size: usize,
    looking_depth: f64,
    snapshot_limit: usize,
) {
    let symbol = binance_symbol.to_lowercase();
    let url = format!("{}/{}@depth@100ms", BINANCE_FUTURES_WS, symbol);
    let mut backoff = 2u64;

    loop {
        let mut bids: BinanceBook = BTreeMap::new();
        let mut asks: BinanceBook = BTreeMap::new();
        let mut imb_stats = RollingStats::new(window_size);
        let mut last_update_id: i64 = 0;
        let mut prev_u: i64 = 0;

        match connect_async(&url).await {
            Ok((ws_stream, _)) => {
                tracing::info!("Binance depth connected: {}", url);
                let (_write, mut read) = ws_stream.split();
                backoff = 2;

                // Phase 1: Fetch REST snapshot, then drain WS buffer
                let snap_url = format!(
                    "{}/fapi/v1/depth?symbol={}&limit={}",
                    BINANCE_FUTURES_REST,
                    symbol.to_uppercase(),
                    snapshot_limit,
                );
                let snapshot = match reqwest::get(&snap_url).await {
                    Ok(resp) => match resp.json::<serde_json::Value>().await {
                        Ok(data) => Some(data),
                        Err(e) => {
                            tracing::warn!("Binance depth snapshot parse failed: {}", e);
                            None
                        }
                    },
                    Err(e) => {
                        tracing::warn!("Binance depth snapshot fetch failed: {}", e);
                        None
                    }
                };

                // Drain any buffered WS events that arrived during snapshot fetch
                let mut buffer: Vec<serde_json::Value> = Vec::new();
                let drain_deadline =
                    tokio::time::Instant::now() + tokio::time::Duration::from_millis(500);
                loop {
                    let remaining = drain_deadline.saturating_duration_since(tokio::time::Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    match tokio::time::timeout(remaining, read.next()).await {
                        Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text)))) => {
                            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&text) {
                                if data.get("U").is_some() && data.get("u").is_some() {
                                    buffer.push(data);
                                }
                            }
                        }
                        _ => break,
                    }
                }

                let Some(snapshot) = snapshot else {
                    tokio::time::sleep(tokio::time::Duration::from_secs(backoff)).await;
                    backoff = (backoff * 2).min(60);
                    continue;
                };

                // Phase 2: Apply snapshot
                apply_binance_snapshot(&snapshot, &mut bids, &mut asks, &mut last_update_id);
                tracing::info!(
                    "Binance depth snapshot: lastUpdateId={}, bids={}, asks={}",
                    last_update_id,
                    bids.len(),
                    asks.len(),
                );

                // Phase 3: Drain buffer with sequence alignment
                let mut first_valid = false;
                for event in &buffer {
                    let u = event.get("u").and_then(|v| v.as_i64()).unwrap_or(0);
                    let big_u = event.get("U").and_then(|v| v.as_i64()).unwrap_or(0);
                    if u <= last_update_id {
                        continue;
                    }
                    if !first_valid {
                        if big_u <= last_update_id + 1 && u >= last_update_id + 1 {
                            first_valid = true;
                        } else {
                            continue;
                        }
                    }
                    apply_binance_diff(event, &mut bids, &mut asks);
                    prev_u = u;
                }

                let synced = first_valid || buffer.is_empty();
                if !synced {
                    tracing::warn!("Binance depth: no valid event in buffer, retrying...");
                    tokio::time::sleep(tokio::time::Duration::from_secs(backoff)).await;
                    backoff = (backoff * 2).min(60);
                    continue;
                }

                tracing::info!("Binance depth synced, entering live stream");

                // Phase 4: Normal recv loop
                loop {
                    let msg = tokio::time::timeout(
                        tokio::time::Duration::from_secs(30),
                        read.next(),
                    )
                    .await;

                    match msg {
                        Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text)))) => {
                            let event: serde_json::Value = match serde_json::from_str(&text) {
                                Ok(d) => d,
                                Err(_) => continue,
                            };

                            if event.get("U").is_none() || event.get("u").is_none() {
                                continue;
                            }

                            // Sequence check
                            let u = event.get("u").and_then(|v| v.as_i64()).unwrap_or(0);
                            if prev_u == 0 {
                                // Nothing was applied from the drain buffer:
                                // align the first live event against the REST
                                // snapshot, same as the buffered path.
                                if u <= last_update_id {
                                    continue;
                                }
                                let big_u =
                                    event.get("U").and_then(|v| v.as_i64()).unwrap_or(0);
                                if big_u > last_update_id + 1 {
                                    tracing::warn!(
                                        "Binance depth: first event U={} leaves gap after snapshot lastUpdateId={}",
                                        big_u,
                                        last_update_id,
                                    );
                                    break; // reconnect
                                }
                            } else {
                                let pu = event.get("pu").and_then(|v| v.as_i64()).unwrap_or(0);
                                if pu != prev_u {
                                    tracing::warn!(
                                        "Binance depth sequence gap: pu={} expected={}",
                                        pu,
                                        prev_u,
                                    );
                                    break; // reconnect
                                }
                            }

                            apply_binance_diff(&event, &mut bids, &mut asks);
                            prev_u = u;

                            // Compute alpha
                            if let (Some(bb), Some(ba)) = (
                                bids.keys().next_back().map(|k| k.into_inner()),
                                asks.keys().next().map(|k| k.into_inner()),
                            ) {
                                let mid = (bb + ba) * 0.5;
                                if mid > 0.0 {
                                    let imbalance =
                                        compute_imbalance(&bids, &asks, mid, looking_depth);
                                    imb_stats.push(imbalance);
                                    let alpha = imb_stats.zscore(imbalance);
                                    let _ = tx.send(BinanceMsg::AlphaUpdate { alpha }).await;
                                }
                            }
                        }
                        Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_)))) | Ok(None) => {
                            break;
                        }
                        Ok(Some(Err(e))) => {
                            tracing::warn!("Binance depth error: {}", e);
                            break;
                        }
                        Err(_) => {
                            tracing::warn!("Binance depth: no data for 30s");
                            break;
                        }
                        _ => {}
                    }
                }

                let _ = tx
                    .send(BinanceMsg::Disconnected { feed: "depth" })
                    .await;
            }
            Err(e) => {
                tracing::warn!("Binance depth connect failed: {}", e);
            }
        }

        tokio::time::sleep(tokio::time::Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(60);
    }
}

/// Parse a `[price, qty]` level. Returns None for malformed entries so they
/// are skipped instead of landing in the book at price 0.0 (which would
/// become best ask and corrupt the mid).
fn parse_level(level: &serde_json::Value) -> Option<(f64, f64)> {
    let arr = level.as_array()?;
    if arr.len() < 2 {
        return None;
    }
    let price: f64 = arr[0].as_str()?.parse().ok()?;
    let qty: f64 = arr[1].as_str()?.parse().ok()?;
    if !price.is_finite() || price <= 0.0 || !qty.is_finite() || qty < 0.0 {
        return None;
    }
    Some((price, qty))
}

fn apply_binance_snapshot(
    data: &serde_json::Value,
    bids: &mut BinanceBook,
    asks: &mut BinanceBook,
    last_update_id: &mut i64,
) {
    bids.clear();
    asks.clear();
    if let Some(bids_arr) = data.get("bids").and_then(|v| v.as_array()) {
        for level in bids_arr {
            let Some((price, qty)) = parse_level(level) else { continue };
            if qty > 0.0 {
                bids.insert(OrderedFloat(price), qty);
            }
        }
    }
    if let Some(asks_arr) = data.get("asks").and_then(|v| v.as_array()) {
        for level in asks_arr {
            let Some((price, qty)) = parse_level(level) else { continue };
            if qty > 0.0 {
                asks.insert(OrderedFloat(price), qty);
            }
        }
    }
    *last_update_id = data.get("lastUpdateId").and_then(|v| v.as_i64()).unwrap_or(0);
}

fn apply_binance_diff(
    event: &serde_json::Value,
    bids: &mut BinanceBook,
    asks: &mut BinanceBook,
) {
    if let Some(bids_arr) = event.get("b").and_then(|v| v.as_array()) {
        for level in bids_arr {
            let Some((price, qty)) = parse_level(level) else { continue };
            let key = OrderedFloat(price);
            if qty == 0.0 {
                bids.remove(&key);
            } else {
                bids.insert(key, qty);
            }
        }
    }
    if let Some(asks_arr) = event.get("a").and_then(|v| v.as_array()) {
        for level in asks_arr {
            let Some((price, qty)) = parse_level(level) else { continue };
            let key = OrderedFloat(price);
            if qty == 0.0 {
                asks.remove(&key);
            } else {
                asks.insert(key, qty);
            }
        }
    }
}

fn compute_imbalance(
    bids: &BinanceBook,
    asks: &BinanceBook,
    mid: f64,
    looking_depth: f64,
) -> f64 {
    let lower = mid * (1.0 - looking_depth);
    let upper = mid * (1.0 + looking_depth);

    let mut sum_bid = 0.0;
    for (&price, &size) in bids.iter().rev() {
        if price.into_inner() < lower {
            break;
        }
        sum_bid += size;
    }

    let mut sum_ask = 0.0;
    for (&price, &size) in asks.iter() {
        if price.into_inner() > upper {
            break;
        }
        sum_ask += size;
    }

    sum_bid - sum_ask
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_level_rejects_malformed() {
        assert_eq!(parse_level(&serde_json::json!(["oops", "1.5"])), None);
        assert_eq!(parse_level(&serde_json::json!(["0", "1.5"])), None);
        assert_eq!(parse_level(&serde_json::json!(["-1.0", "1.5"])), None);
        assert_eq!(parse_level(&serde_json::json!(["100.0", "-2.0"])), None);
        assert_eq!(parse_level(&serde_json::json!(["100.0"])), None);
        assert_eq!(parse_level(&serde_json::json!([100.0, 1.5])), None); // numbers, not strings
    }

    #[test]
    fn parse_level_accepts_valid() {
        assert_eq!(
            parse_level(&serde_json::json!(["100.5", "0.25"])),
            Some((100.5, 0.25))
        );
        // qty 0 is a valid delete marker in diffs
        assert_eq!(
            parse_level(&serde_json::json!(["100.5", "0"])),
            Some((100.5, 0.0))
        );
    }

    #[test]
    fn diff_skips_malformed_levels() {
        let mut bids = BinanceBook::new();
        let mut asks = BinanceBook::new();
        bids.insert(OrderedFloat(100.0), 1.0);
        let event = serde_json::json!({
            "b": [["oops", "2.0"], ["101.0", "3.0"]],
            "a": [["bad", "5.0"]],
        });
        apply_binance_diff(&event, &mut bids, &mut asks);
        assert_eq!(bids.len(), 2);
        assert_eq!(bids.get(&OrderedFloat(101.0)), Some(&3.0));
        assert!(!bids.contains_key(&OrderedFloat(0.0)));
        assert!(asks.is_empty());
    }
}
