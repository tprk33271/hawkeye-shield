use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use serde::{Deserialize, Serialize};

// ============================================================
// WebSocket — Birdeye Native Real-time Meme Data
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
#[serde(tag = "type")]
pub enum BirdeyeEvent {
    #[serde(rename = "MEME_DATA")]
    MemeData { data: MemeData },
    #[serde(other)]
    Unknown,
}

pub enum WsEvent {
    EnrichedMeme(MemeData),
}

pub async fn start_websocket(api_key: &str, tx: mpsc::Sender<WsEvent>) {
    let ws_url = format!("wss://public-api.birdeye.so/socket/solana?x-api-key={}", api_key);

    loop {
        tracing::info!("📡 [WebSocket] Connecting to Birdeye WS...");

        match connect_async(&ws_url).await {
            Ok((ws_stream, _)) => {
                tracing::info!("✅ [WebSocket] Connected to Birdeye successfully!");
                let (mut write, mut read) = ws_stream.split();

                use futures_util::SinkExt;

                // Subscribe to Pump.fun tokens with > 50% progress
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

                while let Some(msg) = read.next().await {
                    match msg {
                        Ok(Message::Text(text)) => {
                            if let Ok(event) = serde_json::from_str::<BirdeyeEvent>(&text) {
                                match event {
                                    BirdeyeEvent::MemeData { data } => {
                                        tracing::info!("🔔 [WS] Enriched Token Data for ${} ({})", data.symbol, data.address);
                                        let _ = tx.send(WsEvent::EnrichedMeme(data)).await;
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
            }
            Err(e) => {
                tracing::error!("❌ [WebSocket] Birdeye WS connection failed: {}", e);
            }
        }

        tracing::info!("🔄 [WebSocket] Reconnecting in 5s...");
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

