use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

// ============================================================
// WebSocket — Real-time New Pair Detection (Raydium + PumpFun)
// ============================================================

const RAYDIUM_PROGRAM: &str = "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8";
const PUMP_FUN_PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

pub enum WsEvent {
    NewPairRaydium(String),
    NewPairPumpFun(String),
}

pub async fn start_websocket(rpc_url: &str, tx: mpsc::Sender<WsEvent>) {
    let ws_url = rpc_url.replace("https", "wss");

    loop {
        tracing::info!("📡 [WebSocket] กำลังเชื่อมต่อ {}...", ws_url);

        match connect_async(&ws_url).await {
            Ok((ws_stream, _)) => {
                tracing::info!("✅ [WebSocket] เชื่อมต่อสำเร็จ!");
                let (mut write, mut read) = ws_stream.split();

                use futures_util::SinkExt;

                // Subscribe to Raydium logs
                let raydium_sub = serde_json::json!({
                    "jsonrpc": "2.0", "id": 1, "method": "logsSubscribe",
                    "params": [
                        { "mentions": [RAYDIUM_PROGRAM] },
                        { "commitment": "processed" }
                    ]
                });
                let _ = write.send(Message::Text(raydium_sub.to_string().into())).await;

                // Subscribe to PumpFun logs
                let pump_sub = serde_json::json!({
                    "jsonrpc": "2.0", "id": 2, "method": "logsSubscribe",
                    "params": [
                        { "mentions": [PUMP_FUN_PROGRAM] },
                        { "commitment": "processed" }
                    ]
                });
                let _ = write.send(Message::Text(pump_sub.to_string().into())).await;

                let tx_clone = tx.clone();

                while let Some(msg) = read.next().await {
                    match msg {
                        Ok(Message::Text(text)) => {
                            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                                if json["method"] == "logsNotification" {
                                    let logs = json["params"]["result"]["value"]["logs"]
                                        .as_array();
                                    let signature = json["params"]["result"]["value"]["signature"]
                                        .as_str().unwrap_or("").to_string();

                                    if let Some(logs) = logs {
                                        let log_strs: Vec<&str> = logs.iter()
                                            .filter_map(|l| l.as_str())
                                            .collect();

                                        let is_raydium_new = log_strs.iter().any(|l|
                                            l.contains("InitializeInstruction2") ||
                                            l.contains("InitializeInstruction"));
                                        if is_raydium_new && !signature.is_empty() {
                                            tracing::info!("🔔 [WS-Raydium] พบ Pool ใหม่: {}...",
                                                &signature[..signature.len().min(8)]);
                                            let _ = tx_clone.send(WsEvent::NewPairRaydium(signature.clone())).await;
                                        }

                                        let is_pump_new = log_strs.iter().any(|l|
                                            l.contains("Program log: Instruction: Create"));
                                        if is_pump_new && !signature.is_empty() {
                                            tracing::info!("💊 [WS-PumpFun] พบเหรียญใหม่: {}...",
                                                &signature[..signature.len().min(8)]);
                                            let _ = tx_clone.send(WsEvent::NewPairPumpFun(signature.clone())).await;
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("⚠️ [WebSocket] Error: {}", e);
                            break;
                        }
                        _ => {}
                    }
                }
            }
            Err(e) => {
                tracing::error!("❌ [WebSocket] Connection failed: {}", e);
            }
        }

        tracing::info!("🔄 [WebSocket] Reconnecting in 5s...");
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}
