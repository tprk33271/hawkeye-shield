#![allow(dead_code)]
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};

// ============================================================
// Birdeye API Client — Core Data Source for HawkEye Shield
// Replaces: DexScreener + GMGN from the original meme-sniper
// ============================================================

#[derive(Clone)]
pub struct BirdeyeClient {
    client: reqwest::Client,
    base_url: String,
}

impl BirdeyeClient {
    pub fn new(api_key: &str) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert("X-API-KEY", HeaderValue::from_str(api_key).unwrap_or(HeaderValue::from_static("")));
        headers.insert("x-chain", HeaderValue::from_static("solana"));
        headers.insert("accept", HeaderValue::from_static("application/json"));

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("Failed to build HTTP client");

        Self {
            client,
            base_url: "https://public-api.birdeye.so".to_string(),
        }
    }

    async fn get_with_retry(&self, url: &str) -> Result<serde_json::Value, String> {
        let mut attempts = 0;
        let max_attempts = 3;
        let mut delay = std::time::Duration::from_millis(500);

        loop {
            let resp = self.client.get(url).send().await.map_err(|e| e.to_string())?;
            let status = resp.status();
            
            if status.is_success() {
                let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
                return Ok(body);
            }

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS && attempts < max_attempts {
                attempts += 1;
                tokio::time::sleep(delay).await;
                delay *= 2; // Exponential backoff
                continue;
            }

            let body_text = resp.text().await.unwrap_or_else(|_| "Unknown error".to_string());
            
            // Handle Birdeye-specific Compute Unit limits returned as 400 Bad Request
            if status == reqwest::StatusCode::BAD_REQUEST && body_text.contains("Compute units usage limit exceeded") && attempts < max_attempts {
                attempts += 1;
                tokio::time::sleep(delay * 2).await; // Longer delay for CU limits
                delay *= 2;
                continue;
            }

            return Err(format!("API Error {}: {}", status, body_text));
        }
    }

    // ─── Trending Tokens (replaces DexScreener trending) ───
    pub async fn get_trending(&self) -> Result<Vec<TrendingToken>, String> {
        let url = format!("{}/defi/token_trending?sort_by=rank&sort_type=asc&offset=0&limit=10", self.base_url);
        let body = self.get_with_retry(&url).await?;

        let tokens = body["data"]["tokens"].as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|t| {
                Some(TrendingToken {
                    address: t["address"].as_str()?.to_string(),
                    symbol: t["symbol"].as_str().unwrap_or("???").to_string(),
                    name: t["name"].as_str().unwrap_or("Unknown").to_string(),
                    price: t["price"].as_f64(),
                    volume_24h: t["volume24hUSD"].as_f64(),
                    price_change_24h: t["priceChange24hPercent"].as_f64(),
                    liquidity: t["liquidity"].as_f64(),
                    rank: t["rank"].as_u64().unwrap_or(999),
                })
            })
            .collect();

        Ok(tokens)
    }

    // ─── New Listings (replaces DexScreener profiles) ───
    pub async fn get_new_listings(&self) -> Result<Vec<NewListing>, String> {
        // Added meme_platform_enabled=true for better Pump.fun coverage
        let url = format!("{}/defi/v2/tokens/new_listing?limit=15&meme_platform_enabled=true", self.base_url);
        let body = self.get_with_retry(&url).await?;

        let items = body["data"]["items"].as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|t| {
                Some(NewListing {
                    address: t["address"].as_str()?.to_string(),
                    symbol: t["symbol"].as_str().unwrap_or("???").to_string(),
                    name: t["name"].as_str().unwrap_or("Unknown").to_string(),
                    price: None, 
                    liquidity: t["liquidity"].as_f64(),
                    listing_time: None,
                })
            })
            .collect();

        Ok(items)
    }

    // ─── Token Price (replaces Jupiter Price API fallback) ───
    pub async fn get_price(&self, address: &str) -> Result<f64, Box<dyn std::error::Error>> {
        let url = format!("{}/defi/price?address={}", self.base_url, address);
        let resp = self.client
            .get(&url)
            .send()
            .await?;

        let status = resp.status();
        let body_text = resp.text().await?;

        if !status.is_success() {
            return Err(format!("API Error {}: {}", status, body_text).into());
        }

        let json: serde_json::Value = serde_json::from_str(&body_text)?;
        if !json["success"].as_bool().unwrap_or(false) {
            return Err(format!("API Error: {}", body_text).into());
        }

        let price = json["data"]["value"].as_f64().unwrap_or(0.0);
        Ok(price)
    }

    pub async fn get_token_security(&self, address: &str) -> Result<TokenSecurity, String> {
        let url = format!("{}/defi/token_security?address={}", self.base_url, address);
        let resp = self.client
            .get(&url)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(format!("API Error {}: {}", status, body_text));
        }

        let json: serde_json::Value = serde_json::from_str(&body_text).map_err(|e| e.to_string())?;
        if !json["success"].as_bool().unwrap_or(false) {
            return Err(format!("API Error: {}", body_text));
        }

        let d = &json["data"];
        
        Ok(TokenSecurity {
            is_mintable: d["mintable"].as_bool().or_else(|| d["isMintable"].as_bool()).or_else(|| d["mintAuthority"].is_string().then_some(true)),
            is_freezable: d["freezable"].as_bool().or_else(|| d["isFreezable"].as_bool()).or_else(|| d["freezeAuthority"].is_string().then_some(true)),
            top10_holder_percent: d["top10UserPercent"].as_f64().or_else(|| d["top10HolderPercent"].as_f64()),
            creator_percentage: d["creatorPercentage"].as_f64().or_else(|| d["creatorBalance"].as_f64()),
            owner_percentage: d["ownerPercentage"].as_f64(),
            is_true_token: d["isTrueToken"].as_bool(),
            is_honeypot: d["isHoneypot"].as_bool(),
        })
    }

    // ─── Token Overview (replaces DexScreener token data) ───
    pub async fn get_token_overview(&self, address: &str) -> Result<TokenOverview, String> {
        let url = format!("{}/defi/token_overview?address={}", self.base_url, address);
        let body = self.get_with_retry(&url).await?;

        if body["success"].as_bool() == Some(false) {
            return Err(format!("API Success=false: {}", body["message"].as_str().unwrap_or("Unknown")));
        }

        let d = &body["data"];

        Ok(TokenOverview {
            address: d["address"].as_str().unwrap_or("").to_string(),
            symbol: d["symbol"].as_str().unwrap_or("???").to_string(),
            name: d["name"].as_str().unwrap_or("Unknown").to_string(),
            price: d["price"].as_f64().unwrap_or(0.0),
            liquidity: d["liquidity"].as_f64().unwrap_or(0.0),
            volume_24h: d["v24hUSD"].as_f64().unwrap_or(0.0),
            price_change_5m: d["priceChange5mPercent"].as_f64().unwrap_or(0.0),
            price_change_1h: d["priceChange1hPercent"].as_f64().unwrap_or(0.0),
            price_change_24h: d["priceChange24hPercent"].as_f64().unwrap_or(0.0),
            buy_5m: d["buy5m"].as_u64().unwrap_or(0),
            sell_5m: d["sell5m"].as_u64().unwrap_or(0),
            volume_5m: d["v5mUSD"].as_f64().unwrap_or(0.0),
            volume_1h: d["v1hUSD"].as_f64().unwrap_or(0.0),
            market_cap: d["mc"].as_f64().unwrap_or(0.0),
            created_at: d["createdAt"].as_i64(),
        })
    }

    pub async fn get_meme_detail(&self, address: &str) -> Result<MemeDetail, String> {
        let url = format!("{}/defi/v3/token/meme-detail/single?address={}", self.base_url, address);
        let body = self.get_with_retry(&url).await?;
        let d = &body["data"];

        Ok(MemeDetail {
            address: d["address"].as_str().unwrap_or(address).to_string(),
            graduated: d["meme_info"]["graduated"].as_bool().unwrap_or(false),
            progress_percent: d["meme_info"]["progress_percent"].as_f64().unwrap_or(0.0),
            real_sol_reserves: d["meme_info"]["pool"]["real_sol_reserves"].as_f64().unwrap_or(0.0),
            creator: d["meme_info"]["creator"].as_str().unwrap_or("").to_string(),
        })
    }

    pub async fn get_meme_list(&self, min_liquidity: f64) -> Result<Vec<NewListing>, String> {
        // Using explicit sorting and source to ensure results (Birdeye V3)
        let url = format!("{}/defi/v3/token/meme/list?sort_by=creation_time&sort_type=desc&min_liquidity={}&limit=10&source=all", self.base_url, min_liquidity);
        let body = self.get_with_retry(&url).await?;
        
        let mut tokens = Vec::new();
        let data = &body["data"];
        if let Some(items) = data["items"].as_array() {
            for item in items {
                tokens.push(NewListing {
                    address: item["address"].as_str().unwrap_or("").to_string(),
                    symbol: item["symbol"].as_str().unwrap_or("").to_string(),
                    name: item["name"].as_str().unwrap_or("").to_string(),
                    price: item["price"].as_f64(),
                    liquidity: item["liquidity"].as_f64(),
                    listing_time: item["creation_time"].as_i64(),
                });
            }
        }
        Ok(tokens)
    }
}

#[derive(Debug, Clone)]
pub struct MemeTokenInfo {
    pub address: String,
    pub symbol: String,
    pub name: String,
    pub price: f64,
    pub liquidity: f64,
    pub mc: f64,
    pub last_trade_unix_time: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeData {
    pub price: f64,
    pub volume: f64,
    pub trade_time: i64,
    pub type_: String,
}


#[derive(Debug, Clone)]
pub struct MemeDetail {
    pub address: String,
    pub graduated: bool,
    pub progress_percent: f64,
    pub real_sol_reserves: f64,
    pub creator: String,
}

#[derive(Debug, Clone)]
pub struct TrendingToken {
    pub address: String,
    pub symbol: String,
    pub name: String,
    pub price: Option<f64>,
    pub volume_24h: Option<f64>,
    pub price_change_24h: Option<f64>,
    pub liquidity: Option<f64>,
    pub rank: u64,
}

#[derive(Debug, Clone)]
pub struct NewListing {
    pub address: String,
    pub symbol: String,
    pub name: String,
    pub price: Option<f64>,
    pub liquidity: Option<f64>,
    pub listing_time: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct TokenOverview {
    pub address: String,
    pub symbol: String,
    pub name: String,
    pub price: f64,
    pub liquidity: f64,
    pub volume_24h: f64,
    pub price_change_5m: f64,
    pub price_change_1h: f64,
    pub price_change_24h: f64,
    pub buy_5m: u64,
    pub sell_5m: u64,
    pub volume_5m: f64,
    pub volume_1h: f64,
    pub market_cap: f64,
    pub created_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct TokenSecurity {
    pub is_mintable: Option<bool>,
    pub is_freezable: Option<bool>,
    pub top10_holder_percent: Option<f64>,
    pub creator_percentage: Option<f64>,
    pub owner_percentage: Option<f64>,
    pub is_true_token: Option<bool>,
    pub is_honeypot: Option<bool>, // Added to match
}
