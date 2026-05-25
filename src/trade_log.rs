use chrono::Utc;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const HEADER: &str = "timestamp,symbol,side,price,size,level,position_after,realized_pnl,available_capital,portfolio_value,simulated\n";

#[derive(Clone, Copy)]
pub struct TradeLogOptions {
    pub enabled: bool,
    pub rotate_bytes: u64,
    pub compress_rotated: bool,
}

/// Buffered CSV trade logger with bounded active-file size.
///
/// `log_fill()` only appends to an in-memory buffer. `flush()` appends buffered
/// rows to the active CSV, then rotates the active file when it exceeds the
/// configured size. Rotated files are gzip-compressed by default.
pub struct TradeLogger {
    path: PathBuf,
    symbol: String,
    buffer: Vec<u8>, // pre-formatted bytes, avoids per-row String alloc
    enabled: bool,
    rotate_bytes: u64,
    compress_rotated: bool,
}

impl TradeLogger {
    pub fn new(log_dir: &Path, symbol: &str, options: TradeLogOptions) -> io::Result<Self> {
        fs::create_dir_all(log_dir)?;
        let path = log_dir.join(format!("trades_{}.csv", symbol));

        let mut logger = Self {
            path,
            symbol: symbol.to_string(),
            buffer: Vec::with_capacity(4096),
            enabled: options.enabled,
            rotate_bytes: options.rotate_bytes,
            compress_rotated: options.compress_rotated,
        };

        if logger.enabled {
            logger.ensure_header()?;
            logger.rotate_if_needed()?;
        }

        Ok(logger)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Buffer one fill row (no disk I/O). Hot path.
    pub fn log_fill(
        &mut self,
        side: &str,
        price: f64,
        size: f64,
        level: usize,
        position_after: f64,
        realized_pnl: f64,
        available_capital: f64,
        portfolio_value: f64,
        simulated: bool,
    ) {
        if !self.enabled {
            return;
        }
        use std::io::Write as _;
        let ts = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ");
        let _ = write!(
            self.buffer,
            "{},{},{},{:.10},{:.6},{},{:.6},{:.4},{:.2},{:.2},{}\n",
            ts,
            self.symbol,
            side,
            price,
            size,
            level,
            position_after,
            realized_pnl,
            available_capital,
            portfolio_value,
            simulated,
        );
    }

    /// Append buffered rows and rotate the active CSV when needed.
    pub fn flush(&mut self) -> io::Result<bool> {
        if !self.enabled {
            self.buffer.clear();
            return Ok(false);
        }

        self.ensure_header()?;
        if !self.buffer.is_empty() {
            let mut main_file = fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&self.path)?;
            main_file.write_all(&self.buffer)?;
            main_file.flush()?;
            self.buffer.clear();
        }

        self.rotate_if_needed()
    }

    /// Delete the trade log and reset.
    pub fn clear(&mut self) -> io::Result<()> {
        self.buffer.clear();
        if !self.enabled {
            return Ok(());
        }
        if self.path.exists() {
            fs::remove_file(&self.path)?;
        }
        fs::write(&self.path, HEADER)?;
        Ok(())
    }

    pub fn rotate_if_needed(&mut self) -> io::Result<bool> {
        if !self.enabled || self.rotate_bytes == 0 || !self.path.exists() {
            return Ok(false);
        }
        let len = fs::metadata(&self.path)?.len();
        if len >= self.rotate_bytes && len > HEADER.len() as u64 {
            self.rotate_current()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn ensure_header(&self) -> io::Result<()> {
        if !self.path.exists() || fs::metadata(&self.path).map_or(true, |m| m.len() == 0) {
            fs::write(&self.path, HEADER)?;
        }
        Ok(())
    }

    fn rotate_current(&mut self) -> io::Result<()> {
        let rotated = self.next_rotated_path();
        if self.compress_rotated {
            self.compress_to(&rotated)?;
            fs::remove_file(&self.path)?;
        } else {
            fs::rename(&self.path, &rotated)?;
        }
        fs::write(&self.path, HEADER)?;
        Ok(())
    }

    fn compress_to(&self, rotated: &Path) -> io::Result<()> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        let tmp_path = dir.join(format!(
            ".tmp_trades_{}_{}.gz",
            self.sanitized_symbol(),
            std::process::id(),
        ));

        let result = (|| -> io::Result<()> {
            let mut input = File::open(&self.path)?;
            let output = File::create(&tmp_path)?;
            let mut encoder = GzEncoder::new(output, Compression::best());
            io::copy(&mut input, &mut encoder)?;
            encoder.finish()?;
            fs::rename(&tmp_path, rotated)?;
            Ok(())
        })();

        if result.is_err() {
            let _ = fs::remove_file(&tmp_path);
        }
        result
    }

    fn next_rotated_path(&self) -> PathBuf {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        let ts = Utc::now().format("%Y%m%dT%H%M%S%3fZ");
        for attempt in 0..1000 {
            let suffix = if attempt == 0 {
                String::new()
            } else {
                format!("_{}", attempt)
            };
            let ext = if self.compress_rotated {
                "csv.gz"
            } else {
                "csv"
            };
            let path = dir.join(format!(
                "trades_{}__rotated_{}{}.{}",
                self.symbol, ts, suffix, ext,
            ));
            if !path.exists() {
                return path;
            }
        }
        dir.join(format!(
            "trades_{}__rotated_{}_{}.{}",
            self.symbol,
            ts,
            std::process::id(),
            if self.compress_rotated {
                "csv.gz"
            } else {
                "csv"
            },
        ))
    }

    fn sanitized_symbol(&self) -> String {
        self.symbol
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::GzDecoder;
    use std::io::Read;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lighter_mm_{}_{}_{}",
            name,
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn rotates_and_compresses_trade_log() {
        let dir = temp_dir("trade_rotate");
        let options = TradeLogOptions {
            enabled: true,
            rotate_bytes: HEADER.len() as u64 + 20,
            compress_rotated: true,
        };
        let mut logger = TradeLogger::new(&dir, "BTC_test", options).unwrap();
        logger.log_fill("buy", 100.0, 0.1, 0, 0.1, 0.0, 990.0, 1000.0, true);
        assert!(logger.flush().unwrap());

        let active = fs::read_to_string(dir.join("trades_BTC_test.csv")).unwrap();
        assert_eq!(active, HEADER);

        let gz_path = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| p.extension().and_then(|e| e.to_str()) == Some("gz"))
            .unwrap();
        let mut decoded = String::new();
        GzDecoder::new(File::open(gz_path).unwrap())
            .read_to_string(&mut decoded)
            .unwrap();
        assert!(decoded.starts_with(HEADER));
        assert!(decoded.contains("BTC_test"));

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn disabled_trade_log_does_not_create_file() {
        let dir = temp_dir("trade_disabled");
        let options = TradeLogOptions {
            enabled: false,
            rotate_bytes: 1,
            compress_rotated: true,
        };
        let mut logger = TradeLogger::new(&dir, "BTC_disabled", options).unwrap();
        logger.log_fill("buy", 100.0, 0.1, 0, 0.1, 0.0, 990.0, 1000.0, true);
        assert!(!logger.flush().unwrap());
        assert!(!dir.join("trades_BTC_disabled.csv").exists());

        fs::remove_dir_all(dir).unwrap();
    }
}
