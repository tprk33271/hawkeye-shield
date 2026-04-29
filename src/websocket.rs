use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ============================================================
// WebSocket — Birdeye Native Real-time Meme Data & Prices
// Supports BDS (Business+) with graceful fallback for Free tier
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemeData {
    pub address: String,
    pub name: String,
    pub symbol: String,
    pub price: f64,
    pub liquidity: f64,
    pub market_cap: f64,
    pub fdv: Option<f64>,
    pub meme_info: MemeInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemeInfo {
    pub source: String,
    pub progress_percent: f64,
    pub graduated: bool,
    pub creation_time: u64,
    pub creator: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceData {
    pub c: f64, // Close price
    pub address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BirdeyeEvent {
    #[serde(rename = "MEME_DATA")]
    MemeData { data: MemeData },
    #[serde(rename = "PRICE_DATA")]
    PriceData { data: PriceData },
    #[serde(rename = "WELCOME")]
    Welcome,
    #[serde(other)]
    Unknown,
}

pub enum WsEvent {
    EnrichedMeme(MemeData),
    PriceUpdate(String, f64),
}

pub enum WsCommand {
    SubscribePrice(String),
}

/// Track whether WS is active for the rest of the system
#[allow(dead_code)]
pub struct WsStatus {
    pub connected: bool,
    pub received_data: bool,
}

pub async fn start_websocket(
    api_key: &str,
    tx: mpsc::Sender<WsEvent>,
    mut cmd_rx: mpsc::Receiver<WsCommand>,
    tui_state: Arc<crate::tui::TuiState>,
) {
    let ws_url = format!(
        "wss://public-api.birdeye.so/socket/solana?x-api-key={}",
        api_key
    );

    let max_retries = 3;
    let mut attempt = 0;

    loop {
        attempt += 1;
        tui_state.log_scanner(&format!(
            "📡 [WebSocket] Connecting to Birdeye BDS... (attempt {}/{})",
            attempt, max_retries
        ));

        // Birdeye BDS requires these specific headers for handshake
        let request = match http::Request::builder()
            .uri(&ws_url)
            .header("Origin", "ws://public-api.birdeye.so")
            .header("Sec-WebSocket-Origin", "ws://public-api.birdeye.so")
            .header("Sec-WebSocket-Protocol", "echo-protocol")
            .body(())
        {
            Ok(r) => r,
            Err(e) => {
                tui_state.log_scanner(&format!("❌ [WebSocket] Failed to build request: {}", e));
                break;
            }
        };

        match connect_async(request).await {
            Ok((ws_stream, _)) => {
                tui_state.log_scanner("✅ [WebSocket] TCP handshake successful!");
                let (mut write, mut read) = ws_stream.split();
                use futures_util::SinkExt;

                // Reset retry counter on successful connect
                attempt = 0;

                // Subscribe to Pump.fun enriched meme tokens
                let subscribe_msg = serde_json::json!({
                    "type": "SUBSCRIBE_MEME",
                    "data": {
                        "source": "pump_dot_fun",
                        "progress_percent": {
                            "min": 50,
                            "max": 100
                        }
                    }
                });

                if let Err(e) = write
                    .send(Message::Text(subscribe_msg.to_string().into()))
                    .await
                {
                    tui_state.log_scanner(&format!(
                        "❌ [WebSocket] SUBSCRIBE_MEME send failed: {}",
                        e
                    ));
                    continue;
                }
                tui_state.log_scanner("📨 [WebSocket] Sent SUBSCRIBE_MEME (Pump.fun 50-100%)");

                // Heartbeat interval (30s ping-pong)
                let mut heartbeat =
                    tokio::time::interval(std::time::Duration::from_secs(30));

                // Data timeout: if we don't receive any data within 60s after connecting,
                // log a warning (likely Free tier) but keep the connection open
                let mut received_any_data = false;
                let connect_time = std::time::Instant::now();
                let mut warned_no_data = false;

                loop {
                    tokio::select! {
                        // Heartbeat ping
                        _ = heartbeat.tick() => {
                            if let Err(e) = write.send(Message::Ping(vec![].into())).await {
                                tui_state.log_scanner(&format!("❌ [WebSocket] Ping failed: {}", e));
                                break;
                            }

                            // Check if we've received data
                            if !received_any_data && !warned_no_data && connect_time.elapsed().as_secs() > 60 {
                                warned_no_data = true;
                                tui_state.log_scanner(
                                    "⚠️ [WebSocket] Connected but no data received (Business Package may be required). REST polling active as primary."
                                );
                            }
                        }

                        // Incoming messages
                        msg = read.next() => {
                            let msg = match msg {
                                Some(m) => m,
                                None => {
                                    tui_state.log_scanner("⚠️ [WebSocket] Stream closed by server.");
                                    break;
                                }
                            };
                            match msg {
                                Ok(Message::Text(text)) => {
                                    received_any_data = true;
                                    match serde_json::from_str::<BirdeyeEvent>(&text) {
                                        Ok(event) => match event {
                                            BirdeyeEvent::MemeData { data } => {
                                                tui_state.log_scanner(&format!(
                                                    "🔔 [WS] Enriched Meme: ${} | Liq: ${:.0} | Progress: {:.0}%",
                                                    data.symbol, data.liquidity, data.meme_info.progress_percent
                                                ));
                                                let _ = tx.send(WsEvent::EnrichedMeme(data)).await;
                                            }
                                            BirdeyeEvent::PriceData { data } => {
                                                let _ = tx.send(WsEvent::PriceUpdate(data.address, data.c)).await;
                                            }
                                            BirdeyeEvent::Welcome => {
                                                tui_state.log_scanner("🤝 [WebSocket] Received WELCOME from Birdeye BDS");
                                            }
                                            _ => {}
                                        },
                                        Err(_) => {
                                            // Log unknown messages for debugging (truncated)
                                            let preview: String = text.chars().take(120).collect();
                                            tui_state.log_scanner(&format!(
                                                "📩 [WS] Raw msg: {}...", preview
                                            ));
                                        }
                                    }
                                }
                                Ok(Message::Pong(_)) => {
                                    // Connection alive, heartbeat OK
                                }
                                Ok(Message::Ping(payload)) => {
                                    // Server-initiated ping, respond with pong
                                    let _ = write.send(Message::Pong(payload)).await;
                                }
                                Ok(Message::Close(_)) | Err(_) => {
                                    tui_state.log_scanner("⚠️ [WebSocket] Connection closed or error.");
                                    break;
                                }
                                _ => {}
                            }
                        }

                        // Commands from executor (SUBSCRIBE_PRICE for active trades)
                        cmd = cmd_rx.recv() => {
                            if let Some(cmd) = cmd {
                                match cmd {
                                    WsCommand::SubscribePrice(address) => {
                                        let sub_msg = serde_json::json!({
                                            "type": "SUBSCRIBE_PRICE",
                                            "data": {
                                                "queryType": "simple",
                                                "chartType": "1m",
                                                "currency": "usd",
                                                "address": address
                                            }
                                        });
                                        if let Err(e) = write.send(Message::Text(sub_msg.to_string().into())).await {
                                            tui_state.log_scanner(&format!(
                                                "❌ [WS] SUBSCRIBE_PRICE failed: {}", e
                                            ));
                                        } else {
                                            tui_state.log_scanner(&format!(
                                                "📡 [WS] Subscribed PRICE for {}", &address[..8.min(address.len())]
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                tui_state.log_scanner(&format!(
                    "❌ [WebSocket] Connection failed: {}", e
                ));
            }
        }

        // Retry logic with limit
        if attempt >= max_retries {
            tui_state.log_scanner(&format!(
                "⚠️ [WebSocket] {} attempts failed. WebSocket paused — REST polling is primary data source. Will retry in 5 min.",
                max_retries
            ));
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
            attempt = 0; // Reset for another round after 5 min
        } else {
            let delay = 5 * attempt as u64;
            tui_state.log_scanner(&format!(
                "🔄 [WebSocket] Reconnecting in {}s...", delay
            ));
            tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
        }
    }
}
