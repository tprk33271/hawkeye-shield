use crate::birdeye::{BirdeyeClient, TokenOverview, TokenSecurity};
use crate::config::Config;
use std::collections::HashMap;
use std::time::Instant;

// ============================================================
// Scanner — Token Discovery + Filtering Engine
// Port of scanner.js with Birdeye as sole data source
// ============================================================

#[derive(Debug, Clone)]
pub struct TokenCandidate {
    pub address: String,
    pub symbol: String,
    pub name: String,
    pub price: f64,
    pub liquidity: f64,
    pub volume_5m: f64,
    pub strategy: String,
}

pub struct Scanner {
    birdeye: BirdeyeClient,
    config: Config,
    scanned_cache: HashMap<String, Instant>,     // cooldown 2 min
    safety_blacklist: HashMap<String, Instant>,   // blacklist 1 hour
    trade_blacklist: HashMap<String, (u32, Instant)>, // (loss_count, release_at)
}

impl Scanner {
    pub fn new(birdeye: BirdeyeClient, config: Config) -> Self {
        Self {
            birdeye,
            config,
            scanned_cache: HashMap::new(),
            safety_blacklist: HashMap::new(),
            trade_blacklist: HashMap::new(),
        }
    }

    /// Record trade result — blacklist tokens with repeated losses
    pub fn handle_trade_result(&mut self, address: &str, pnl_pct: f64) {
        if pnl_pct < 0.0 {
            let entry = self.trade_blacklist.entry(address.to_string())
                .or_insert((0, Instant::now()));
            entry.0 += 1;
            if entry.0 >= 2 {
                entry.1 = Instant::now() + std::time::Duration::from_secs(3600);
                tracing::warn!("🚫 [BLACKLIST] ${} แบน 1 ชั่วโมง (ขาดทุนซ้ำ {} ครั้ง)", address, entry.0);
            }
        }
    }

    /// Main scan loop — find token opportunities
    pub async fn scan_for_opportunities(&mut self, exclude_addresses: &[String]) -> Option<TokenCandidate> {
        let now = Instant::now();
        tracing::info!("🔍 [Scanner] กำลังสแกนหาเหรียญ Trending + New Listings จาก Birdeye...");

        // Cleanup expired entries
        self.scanned_cache.retain(|_, t| now.duration_since(*t).as_secs() < 120);
        self.safety_blacklist.retain(|_, t| now.duration_since(*t).as_secs() < 3600);
        self.trade_blacklist.retain(|_, (_, t)| *t > now);

        // Fetch trending + new listings from Birdeye
        let trending = self.birdeye.get_trending().await.unwrap_or_default();
        let new_listings = self.birdeye.get_new_listings().await.unwrap_or_default();

        // Merge unique addresses
        let mut addresses: Vec<String> = Vec::new();
        for t in &trending {
            if !addresses.contains(&t.address) {
                addresses.push(t.address.clone());
            }
        }
        for n in &new_listings {
            if !addresses.contains(&n.address) {
                addresses.push(n.address.clone());
            }
        }

        tracing::info!("  📊 พบ {} เหรียญจาก Birdeye (Trending: {}, New: {})",
            addresses.len(), trending.len(), new_listings.len());

        for address in addresses.iter().take(30) {
            // Skip if already holding
            if exclude_addresses.contains(address) { continue; }
            // Skip if trade-blacklisted
            if self.trade_blacklist.contains_key(address) { continue; }
            // Skip if safety-blacklisted
            if self.safety_blacklist.contains_key(address) { continue; }
            // Skip if recently scanned (2 min cooldown)
            if let Some(t) = self.scanned_cache.get(address) {
                if now.duration_since(*t).as_secs() < 120 { continue; }
            }

            self.scanned_cache.insert(address.clone(), now);

            // Fetch detailed data from Birdeye
            let overview = match self.birdeye.get_token_overview(address).await {
                Ok(o) => o,
                Err(_) => continue,
            };

            // ─── FILTERING LOGIC (ported from scanner.js) ───

            let age_minutes = overview.created_at
                .map(|ts| (chrono::Utc::now().timestamp() - ts) as f64 / 60.0)
                .unwrap_or(0.0);

            let m5_buys = overview.buy_5m;
            let m5_sells = overview.sell_5m;
            let m5_volume = overview.volume_5m;
            let liquidity = overview.liquidity;
            let price_change_m5 = overview.price_change_5m;

            // Anti-Wash Trade
            let avg_buy_size = if m5_buys > 0 { m5_volume / m5_buys as f64 } else { 0.0 };
            if m5_buys < 30 && avg_buy_size > 500.0 {
                tracing::debug!("  ⚠️ ${} ข้าม: อาจเป็นการปั่น Volume", overview.symbol);
                continue;
            }

            let is_healthy_momentum = m5_buys > (m5_sells as f64 * 1.5) as u64 && m5_volume > 2000.0;

            // Strategy matching
            let strategy = self.match_strategy(
                age_minutes, is_healthy_momentum, liquidity,
                price_change_m5, m5_volume, &overview
            );

            let strategy = match strategy {
                Some(s) => s,
                None => {
                    tracing::debug!("  - ${} ไม่เข้าสูตร (M5 Vol: ${:.0}, Age: {:.1}m)",
                        overview.symbol, m5_volume, age_minutes);
                    continue;
                }
            };

            // ─── SAFETY CHECK (Birdeye Token Security) ───
            match self.check_safety(address, age_minutes).await {
                Ok(true) => {},
                Ok(false) => continue,
                Err(_) => {
                    if liquidity < 30000.0 {
                        tracing::debug!("  ❌ ${} Security check ล้มเหลว + สภาพคล่องต่ำ", overview.symbol);
                        continue;
                    }
                }
            }

            tracing::info!("🚀 [เป้าหมายล็อกแล้ว!] กลยุทธ: {} | ${} | ราคา: ${:.8}",
                strategy, overview.symbol, overview.price);

            return Some(TokenCandidate {
                address: address.clone(),
                symbol: overview.symbol,
                name: overview.name,
                price: overview.price,
                liquidity,
                volume_5m: m5_volume,
                strategy,
            });
        }

        tracing::info!("  - จบรอบการสแกน (ไม่พบเหรียญที่ผ่านเกณฑ์)");
        None
    }

    fn match_strategy(
        &self, age_minutes: f64, is_healthy_momentum: bool,
        liquidity: f64, price_change_m5: f64, m5_volume: f64,
        overview: &TokenOverview,
    ) -> Option<String> {
        // 1. [NEW TOKEN] 5-60 minutes old
        if age_minutes >= 5.0 && age_minutes <= 60.0 {
            if is_healthy_momentum && liquidity > 10000.0 && price_change_m5 > 2.0 {
                return Some("Solid Trend (เหรียญเริ่มติดลมบน)".to_string());
            }
        }
        // 2. [WHALE LEGACY] > 60 minutes old
        else if age_minutes > 60.0 {
            let h1_price_change = overview.price_change_1h;
            let h1_volume = overview.volume_1h;
            let avg_m5_from_h1 = h1_volume / 12.0;
            let is_volume_spiking = m5_volume > (avg_m5_from_h1 * 1.5);
            let min_legacy_liq = 12000.0;

            if h1_price_change > 2.0 && is_volume_spiking && liquidity > min_legacy_liq && is_healthy_momentum {
                return Some("Whale Legacy (วาฬเริ่มกวาดเหรียญเก่า)".to_string());
            }
            // 3. [DIP SNIPER]
            if h1_price_change <= 2.0 && is_volume_spiking && liquidity > min_legacy_liq
                && is_healthy_momentum && price_change_m5 > 3.0 {
                return Some("Dip Sniper (ดีดกลับจากจุดย่อ)".to_string());
            }
        }
        None
    }

    async fn check_safety(&mut self, address: &str, age_minutes: f64) -> Result<bool, String> {
        let security = self.birdeye.get_token_security(address).await?;

        // Top 10 Holders check (ผ่อนปรนตามอายุ)
        let max_top10 = if age_minutes > 1440.0 { 0.40 } else { 0.30 };
        if let Some(top10) = security.top10_holder_percent {
            if top10 > max_top10 {
                tracing::debug!("  ❌ ${} Top 10 Holders > {:.0}% ({:.1}%)", address, max_top10 * 100.0, top10 * 100.0);
                self.safety_blacklist.insert(address.to_string(), Instant::now());
                return Ok(false);
            }
        }

        // Creator holding check
        if let Some(creator_pct) = security.creator_percentage {
            if creator_pct > 5.0 {
                tracing::debug!("  ❌ ${} Dev holds {:.1}%", address, creator_pct);
                self.safety_blacklist.insert(address.to_string(), Instant::now());
                return Ok(false);
            }
        }

        // Mintable check
        if security.is_mintable == Some(true) {
            tracing::debug!("  ❌ ${} Mintable is ON", address);
            self.safety_blacklist.insert(address.to_string(), Instant::now());
            return Ok(false);
        }

        // Freezable check (bonus — JS version didn't have this)
        if security.is_freezable == Some(true) {
            tracing::debug!("  ❌ ${} Freezable is ON", address);
            self.safety_blacklist.insert(address.to_string(), Instant::now());
            return Ok(false);
        }

        tracing::info!("  ✅ ${} ผ่านเกณฑ์ความปลอดภัย (Birdeye Security)", address);
        Ok(true)
    }
}
