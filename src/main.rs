mod birdeye;
mod config;
mod executor;
mod logger;
mod scanner;
mod websocket;
mod tui;

use crate::birdeye::BirdeyeClient;
use crate::config::Config;
use crate::executor::TradeExecutor;
use crate::scanner::Scanner;
use crate::tui::{run_tui, TuiState};
use crate::websocket::{start_websocket, WsEvent, WsCommand};
use tokio::sync::mpsc;
use std::sync::Arc;
use std::collections::HashMap;

// ============================================================
// HawkEye Shield — Autonomous Solana Trading Agent
// Powered by Birdeye Data API
// ============================================================

#[tokio::main]
async fn main() {
    // We don't initialize tracing_subscriber to stdout because TUI will take over the screen.
    // Optionally, we could write tracing logs to a file. For now, we rely on TuiState.
    
    let config = Config::from_env();

    if config.birdeye_api_key.is_empty() || config.birdeye_api_key == "your_birdeye_api_key_here" {
        eprintln!("❌ BIRDEYE_API_KEY not set — Please add to .env");
        std::process::exit(1);
    }

    let birdeye = BirdeyeClient::new(&config.birdeye_api_key);
    
    // Shared state for all executors to see the same active trades
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex as TokioMutex;
    let shared_trades = Arc::new(TokioMutex::new(HashMap::new()));
    
    // Initialize TUI State
    let tui_state = Arc::new(TuiState::new(shared_trades.clone(), config.clone()));
    
    // Spawn TUI rendering loop in background
    let tui_state_clone = tui_state.clone();
    tokio::spawn(async move {
        if let Err(e) = run_tui(tui_state_clone).await {
            eprintln!("TUI Error: {}", e);
            std::process::exit(1);
        }
    });

    let (ws_cmd_tx, ws_cmd_rx) = mpsc::channel::<WsCommand>(100);

    let executor = TradeExecutor::new(config.clone(), birdeye.clone(), shared_trades.clone(), tui_state.clone(), ws_cmd_tx.clone());
    let mut scanner = Scanner::new(birdeye.clone(), config.clone(), tui_state.clone());

    let (ws_tx, mut ws_rx) = mpsc::channel::<WsEvent>(100);

    // Start WebSocket in background
    let api_key = config.birdeye_api_key.clone();
    tokio::spawn(async move {
        start_websocket(&api_key, ws_tx, ws_cmd_rx).await;
    });

    let max_trades = config.max_active_trades;
    let mut scan_interval = tokio::time::interval(std::time::Duration::from_secs(45));

    tui_state.log_scanner("🚀 HawkEye Shield Started! (WebSocket + Polling every 45s)");

    loop {
        tokio::select! {
            Some(event) = ws_rx.recv() => {
                let active_count = executor.active_trades.lock().await.len();
                if active_count >= max_trades { continue; }

                match event {
                    WsEvent::EnrichedMeme(data) => {
                        tui_state.log_scanner(&format!("🔔 [WS] Enriched Token: {} (${})", data.symbol, data.address));
                        let mut buffer = scanner.ws_buffer.lock().await;
                        // Avoid duplicates in buffer
                        if !buffer.iter().any(|d| d.address == data.address) {
                            buffer.push(data);
                        }
                    }
                    WsEvent::PriceUpdate(address, price) => {
                        let mut trades = shared_trades.lock().await;
                        if let Some(trade) = trades.get_mut(&address) {
                            trade.current_price = price;
                        }
                    }
                }
            }

            _ = scan_interval.tick() => {
                let active_count = executor.active_trades.lock().await.len();
                
                // Update balance in TUI state
                if !config.paper_trade {
                    let current_balance = executor.get_sol_balance();
                    if let Ok(mut bal) = tui_state.balance.lock() {
                        *bal = current_balance;
                    }
                }
                // For paper trade, balance is managed by paper_buy/paper_sell

                if active_count >= max_trades {
                    continue;
                }

                let active_addresses: Vec<String> = executor.active_trades.lock().await
                    .keys().cloned().collect();

                if let Some(token) = scanner.scan_for_opportunities(&active_addresses).await {
                    handle_buy(token, &executor, &config, &birdeye, &shared_trades, &tui_state, &ws_cmd_tx).await;
                }
            }
        }
    }
}

async fn handle_buy(
    token: crate::scanner::TokenCandidate,
    executor: &TradeExecutor,
    config: &Config,
    birdeye: &BirdeyeClient,
    shared_trades: &Arc<tokio::sync::Mutex<HashMap<String, crate::executor::ActiveTrade>>>,
    tui_state: &Arc<TuiState>,
    ws_cmd_tx: &mpsc::Sender<WsCommand>
) {
    let balance = tui_state.balance.lock().unwrap().clone();
    let trade_size = if config.use_dynamic_sizing && balance > 0.0 {
        (balance * config.kelly_fraction).max(0.01) // Ensure a minimum size
    } else {
        config.trade_size_sol
    };

    tui_state.log_trade(&format!("💰 [Execution] Buying ${} for {:.3} SOL", token.symbol, trade_size));

    match executor.execute_buy(&token, trade_size).await {
        Ok(result) if result.success => {
            tui_state.log_trade(&format!("✅ [BUY SUCCESS] ${} @ ${:.8}", token.symbol, result.buy_price));

            let token_clone = token.clone();
            let buy_price = result.buy_price;
            let spent = result.trade_size_sol;
            let birdeye_clone = birdeye.clone();
            let config_clone = config.clone();
            let shared_trades_clone = shared_trades.clone();
            let tui_state_monitor = tui_state.clone();
            let ws_cmd_tx_clone = ws_cmd_tx.clone();

            tokio::spawn(async move {
                let monitor_executor = TradeExecutor::new(config_clone, birdeye_clone, shared_trades_clone, tui_state_monitor.clone(), ws_cmd_tx_clone);
                let pnl = monitor_executor.monitor_and_sell(token_clone, buy_price, spent).await;
                tui_state_monitor.log_trade(&format!("📊 [Trade Complete] PNL: {:.2}%", pnl));
            });
        }
        Ok(_) => {
            tui_state.log_trade(&format!("❌ [BUY FAILED] ${}", token.symbol));
        }
        Err(e) => {
            tui_state.log_trade(&format!("❌ [BUY ERROR] ${}: {}", token.symbol, e));
        }
    }
}

