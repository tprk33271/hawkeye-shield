use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use serde::{Deserialize, Serialize};

// ============================================================
// WebSocket — Birdeye Native Real-time Meme Data & Prices
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

pub async fn start_websocket(api_key: &str, tx: mpsc::Sender<WsEvent>, mut cmd_rx: mpsc::Receiver<WsCommand>) {
    let ws_url = format!("wss://public-api.birdeye.so/socket/solana?x-api-key={}", api_key);

    loop {
        tracing::info!("📡 [WebSocket] Connecting to Birdeye WS...");

        match connect_async(&ws_url).await {
            Ok((ws_stream, _)) => {
                tracing::info!("✅ [WebSocket] Connected to Birdeye successfully!");
                let (mut write, mut read) = ws_stream.split();
                use futures_util::SinkExt;

                // Subscribe to Pump.fun tokens
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

                if let Err(e) = write.send(Message::Text(subscribe_msg.to_string().into())).await {
                    tracing::error!("❌ [WebSocket] Subscription failed: {}", e);
                    continue;
                }

                loop {
                    tokio::select! {
                        msg = read.next() => {
                            let msg = match msg {
                                Some(m) => m,
                                None => {
                                    tracing::warn!("⚠️ [WebSocket] Stream closed by server.");
                                    break;
                                }
                            };
                            match msg {
                                Ok(Message::Text(text)) => {
                                    if let Ok(event) = serde_json::from_str::<BirdeyeEvent>(&text) {
                                        match event {
                                            BirdeyeEvent::MemeData { data } => {
                                                tracing::info!("🔔 [WS] Enriched Token Data for ${} ({})", data.symbol, data.address);
                                                let _ = tx.send(WsEvent::EnrichedMeme(data)).await;
                                            }
                                            BirdeyeEvent::PriceData { data } => {
                                                // Handle price update
                                                let _ = tx.send(WsEvent::PriceUpdate(data.address, data.c)).await;
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                                Ok(Message::Close(_)) | Err(_) => {
                                    tracing::warn!("⚠️ [WebSocket] Connection closed or error occurred.");
                                    break;
                                }
                                _ => {}
                            }
                        }
                        cmd = cmd_rx.recv() => {
                            if let Some(cmd) = cmd {
                                match cmd {
                                    WsCommand::SubscribePrice(address) => {
                                        let sub_msg = serde_json::json!({
                                            "type": "SUBSCRIBE_PRICE",
                                            "data": {
                                                "chartType": "1m",
                                                "currency": "usd",
                                                "address": address
                                            }
                                        });
                                        if let Err(e) = write.send(Message::Text(sub_msg.to_string().into())).await {
                                            tracing::error!("❌ [WebSocket] Failed to subscribe to price: {}", e);
                                        } else {
                                            tracing::info!("📡 [WebSocket] Sent SUBSCRIBE_PRICE for {}", address);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                tracing::error!("❌ [WebSocket] Birdeye WS connection failed: {}", e);
            }
        }

        tracing::info!("🔄 [WebSocket] Reconnecting in 5s...");
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

