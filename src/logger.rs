use chrono::Utc;
use std::fs::OpenOptions;
use std::io::Write;

// ============================================================
// Logger — Trade Result Logging
// ============================================================

pub fn log_trade_result(
    symbol: &str,
    entry_price: f64,
    exit_price: f64,
    pnl_pct: f64,
    reason: &str,
    extra: &str,
) {
    let timestamp = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let emoji = if pnl_pct >= 0.0 { "💚" } else { "🔴" };

    let line = format!(
        "{} {} | ${} | Entry: ${:.8} | Exit: ${:.8} | PNL: {:.2}% | Reason: {} | {}\n",
        emoji, timestamp, symbol, entry_price, exit_price, pnl_pct, reason, extra
    );

    // Log to console
    if pnl_pct >= 0.0 {
        tracing::info!("{}", line.trim());
    } else {
        tracing::warn!("{}", line.trim());
    }

    // Log to file
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open("trade-history.log")
    {
        let _ = file.write_all(line.as_bytes());
    }
}
