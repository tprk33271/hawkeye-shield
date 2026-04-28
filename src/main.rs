mod birdeye;
mod config;
mod executor;
mod logger;
mod scanner;
mod websocket;

use crate::birdeye::BirdeyeClient;
use crate::config::Config;
use crate::executor::TradeExecutor;
use crate::scanner::Scanner;
use crate::websocket::{start_websocket, WsEvent};
use tokio::sync::mpsc;

// ============================================================
// HawkEye Shield — Autonomous Solana Trading Agent
// Powered by Birdeye Data API
// ============================================================

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("hawkeye_shield=info".parse().unwrap())
                .add_directive("reqwest=warn".parse().unwrap())
        )
        .with_target(false)
        .init();

    let config = Config::from_env();

    if config.birdeye_api_key.is_empty() || config.birdeye_api_key == "your_birdeye_api_key_here" {
        tracing::error!("❌ BIRDEYE_API_KEY ไม่ได้ตั้งค่า — กรุณาใส่ใน .env");
        std::process::exit(1);
    }

    let mode = if config.paper_trade { "📝 PAPER TRADE" } else { "💰 LIVE TRADE" };
    let wallet_str = config.wallet_pubkey_str();
    let wallet_short = if wallet_str.len() > 8 { &wallet_str[..8] } else { &wallet_str };

    println!();
    println!("  ╔══════════════════════════════════════════════════╗");
    println!("  ║      🦅 HawkEye Shield — Solana Sniper Agent    ║");
    println!("  ║      Powered by Birdeye Data API                 ║");
    println!("  ╠══════════════════════════════════════════════════╣");
    println!("  ║  Mode:    {:<40}║", mode);
    println!("  ║  Wallet:  {}...{:<31}║", wallet_short, "");
    println!("  ║  Size:    {} SOL per trade{:<24}║", config.trade_size_sol, "");
    println!("  ║  TP/SL:   +{}% / -{}%{:<28}║", config.take_profit_pct, config.stop_loss_pct, "");
    println!("  ╚══════════════════════════════════════════════════╝");
    println!();

    let birdeye = BirdeyeClient::new(&config.birdeye_api_key);
    let executor = TradeExecutor::new(config.clone(), birdeye.clone());
    let mut scanner = Scanner::new(birdeye.clone(), config.clone());

    let (ws_tx, mut ws_rx) = mpsc::channel::<WsEvent>(100);

    // Start WebSocket in background
    let ws_rpc = config.solana_rpc_url.clone();
    tokio::spawn(async move {
        start_websocket(&ws_rpc, ws_tx).await;
    });

    let max_trades = config.max_active_trades;
    let trade_size = config.trade_size_sol;
    let mut scan_interval = tokio::time::interval(std::time::Duration::from_secs(8));

    tracing::info!("🚀 HawkEye Shield เริ่มทำงาน! (WebSocket + Polling ทุก 8 วินาที)");

    loop {
        tokio::select! {
            Some(event) = ws_rx.recv() => {
                let active_count = executor.active_trades.lock().await.len();
                if active_count >= max_trades { continue; }

                match event {
                    WsEvent::NewPairRaydium(sig) => {
                        tracing::info!("⚡ [WS-Raydium] Processing new pair: {}...", &sig[..sig.len().min(8)]);
                    }
                    WsEvent::NewPairPumpFun(sig) => {
                        tracing::info!("💊 [WS-PumpFun] Processing new token: {}...", &sig[..sig.len().min(8)]);
                    }
                }
            }

            _ = scan_interval.tick() => {
                let active_count = executor.active_trades.lock().await.len();
                if active_count >= max_trades {
                    tracing::debug!("⏸️ Max trades ({}/{}), skip scan", active_count, max_trades);
                    continue;
                }

                let active_addresses: Vec<String> = executor.active_trades.lock().await
                    .keys().cloned().collect();

                if let Some(token) = scanner.scan_for_opportunities(&active_addresses).await {
                    tracing::info!("💰 [Execution] ซื้อ: ${} จำนวน {} SOL", token.symbol, trade_size);

                    match executor.execute_buy(&token, trade_size).await {
                        Ok(result) if result.success => {
                            tracing::info!("✅ [BUY SUCCESS] ${} @ ${:.8}", token.symbol, result.buy_price);

                            let token_clone = token.clone();
                            let buy_price = result.buy_price;
                            let spent = result.trade_size_sol;
                            let birdeye_clone = birdeye.clone();
                            let config_clone = config.clone();

                            tokio::spawn(async move {
                                let monitor_executor = TradeExecutor::new(config_clone, birdeye_clone);
                                let pnl = monitor_executor.monitor_and_sell(token_clone, buy_price, spent).await;
                                tracing::info!("📊 [Trade Complete] PNL: {:.2}%", pnl);
                            });
                        }
                        Ok(_) => {
                            tracing::error!("❌ [BUY FAILED] ${}", token.symbol);
                        }
                        Err(e) => {
                            tracing::error!("❌ [BUY ERROR] ${}: {}", token.symbol, e);
                        }
                    }
                }
            }
        }
    }
}
