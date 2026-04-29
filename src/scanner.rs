use crate::birdeye::{BirdeyeClient, TokenOverview};
use crate::config::Config;
use std::collections::HashMap;
use std::time::Instant;

// ============================================================
// Scanner — Token Discovery + Filtering Engine
// Port of scanner.js with Birdeye as sole data source
// ============================================================

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TokenCandidate {
    pub address: String,
    pub symbol: String,
    pub name: String,
    pub price: f64,
    pub liquidity: f64,
    pub volume_5m: f64,
    pub price_change_m5: f64,
    pub strategy: String,
}

pub struct Scanner {
    birdeye: BirdeyeClient,
    config: Config,
    scanned_cache: HashMap<String, Instant>,     // cooldown 2 min
    safety_blacklist: HashMap<String, Instant>,   // blacklist 1 hour
    trade_blacklist: HashMap<String, (u32, Instant)>, // (loss_count, release_at)
    tui_state: std::sync::Arc<crate::tui::TuiState>,
}

impl Scanner {
    pub fn new(birdeye: BirdeyeClient, config: Config, tui_state: std::sync::Arc<crate::tui::TuiState>) -> Self {
        Self {
            birdeye,
            config,
            scanned_cache: HashMap::new(),
            safety_blacklist: HashMap::new(),
            trade_blacklist: HashMap::new(),
            tui_state,
        }
    }

    /// Record trade result — blacklist tokens with repeated losses
    #[allow(dead_code)]
    pub fn handle_trade_result(&mut self, address: &str, pnl_pct: f64) {
        if pnl_pct < 0.0 {
            let entry = self.trade_blacklist.entry(address.to_string())
                .or_insert((0, Instant::now()));
            entry.0 += 1;
            if entry.0 >= 2 {
                entry.1 = Instant::now() + std::time::Duration::from_secs(3600);
                self.tui_state.log_scanner(&format!("🚫 [BLACKLIST] ${} banned 1h (Loss count: {})", address, entry.0));
            }
        }
    }

    /// Main scan loop — find token opportunities
    pub async fn scan_for_opportunities(&mut self, exclude_addresses: &[String]) -> Option<TokenCandidate> {
        let now = Instant::now();
        self.tui_state.log_scanner(&format!("🔍 [Scanner] Scanning Trending + New Listings from Birdeye..."));

        // Cleanup expired entries
        self.scanned_cache.retain(|_, t| now.duration_since(*t).as_secs() < 120);
        self.safety_blacklist.retain(|_, t| now.duration_since(*t).as_secs() < 3600);
        self.trade_blacklist.retain(|_, (_, t)| *t > now);

        // Fetch Trending + New Listings from Birdeye
        let trending = match self.birdeye.get_trending().await {
            Ok(data) => data,
            Err(e) => {
                eprintln!("API_ERROR_TRENDING: {}", e);
                self.tui_state.log_scanner(&format!("❌ [Scanner] Error fetching trending: {}", e));
                vec![]
            }
        };
        
        let new_listings = match self.birdeye.get_new_listings().await {
            Ok(data) => data,
            Err(e) => {
                eprintln!("API_ERROR_NEW: {}", e);
                self.tui_state.log_scanner(&format!("❌ [Scanner] Error fetching new listings: {}", e));
                vec![]
            }
        };

        let meme_list = match self.birdeye.get_meme_list(500.0).await {
            Ok(data) => data,
            Err(_) => {
                // Meme list V3 might fail on some keys, skip silently if so
                vec![]
            }
        };

        // Merge unique addresses (Trending + New + Meme V3)
        let mut addresses: Vec<String> = Vec::new();
        
        // Take top from trending
        for t in trending.iter().take(5) {
            if !addresses.contains(&t.address) { addresses.push(t.address.clone()); }
        }
        
        // Take top from new listings
        for n in new_listings.iter().take(5) {
            if !addresses.contains(&n.address) { addresses.push(n.address.clone()); }
        }

        // Take top from Meme List V3
        for m in meme_list.iter().take(5) {
            if !addresses.contains(&m.address) { addresses.push(m.address.clone()); }
        }

        self.tui_state.log_scanner(&format!("  📊 Sources: Trending ({}), New ({}), Meme-V3 ({})", 
            trending.len(), new_listings.len(), meme_list.len()));

        let mut addresses_to_scan = Vec::new();
        for address in addresses.iter() {
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
            addresses_to_scan.push(address.clone());
        }

        use futures_util::stream::{self, StreamExt};

        let mut stream = stream::iter(addresses_to_scan).map(|address| {
            let birdeye = self.birdeye.clone();
            let trending = trending.clone();
            let paper_trade = self.config.paper_trade;
            let tui_state = self.tui_state.clone();
            
            async move {
                // Add a small delay to avoid hitting rate limit burst (max 5 requests per second)
                // Using 600ms because we run with buffer_unordered(2)
                tokio::time::sleep(std::time::Duration::from_millis(600)).await;

                // Fetch detailed data from Birdeye
                let mut overview = match birdeye.get_token_overview(&address).await {
                    Ok(o) => o,
                    Err(e) => {
                        tui_state.log_scanner(&format!("  ❌ {} skip: Overview Error: {}", address, e));
                        return (None, None);
                    }
                };

                // Enrich with Trending API data
                if let Some(t) = trending.iter().find(|t| &t.address == &address) {
                    if overview.market_cap <= 0.0 {
                        overview.market_cap = t.volume_24h.unwrap_or(0.0); // fallback estimate
                    }
                    if overview.liquidity <= 0.0 {
                        overview.liquidity = t.liquidity.unwrap_or(0.0);
                    }
                    if overview.volume_24h <= 0.0 {
                        overview.volume_24h = t.volume_24h.unwrap_or(0.0);
                    }
                    if overview.price <= 0.0 {
                        overview.price = t.price.unwrap_or(0.0);
                    }
                }

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
                    return (None, None);
                }

                // ═══ HARD FILTERS (don't rely on Security API) ═══
                if liquidity < 4000.0 {
                    tui_state.log_scanner(&format!("  🛑 ${} skip: Low Liquidity (${:.0})", overview.symbol, liquidity));
                    return (None, None);
                }

                if m5_buys < 5 {
                    tui_state.log_scanner(&format!("  🛑 ${} skip: No buyers ({})", overview.symbol, m5_buys));
                    return (None, None);
                }

                if overview.volume_24h < 3000.0 {
                    tui_state.log_scanner(&format!("  🛑 ${} skip: Dead volume (24h: ${:.0})", overview.symbol, overview.volume_24h));
                    return (None, None);
                }

                // ═══ MOMENTUM CHECK (same for Paper + Live) ═══
                let is_healthy_momentum = m5_buys > (m5_sells as f64 * 1.2) as u64 && m5_volume > 500.0;

                // Strategy matching
                let strategy = Scanner::match_strategy_static(
                    paper_trade, age_minutes, is_healthy_momentum, liquidity,
                    price_change_m5, m5_volume, &overview
                );

                let strategy = match strategy {
                    Some(s) => s,
                    None => {
                        if paper_trade {
                            let reason = if !is_healthy_momentum { "Low Momentum" } else { "No Strategy Fit" };
                            tui_state.log_scanner(&format!("  ℹ️ Skipping ${}: {} (M5 Chg: {:.1}%, Vol: ${:.0})", 
                                overview.symbol, reason, price_change_m5, m5_volume));
                        }
                        return (None, None);
                    }
                };

                // ─── MEME ALPHA ENRICHMENT (V3) ───
                let mut meme_conviction = "N/A".to_string();
                if let Ok(meme) = birdeye.get_meme_detail(&address).await {
                    if meme.progress_percent > 80.0 && !meme.graduated {
                        meme_conviction = format!("🔥 Bonding: {:.0}%", meme.progress_percent);
                    } else if meme.graduated {
                        meme_conviction = "🎓 Graduated".to_string();
                    } else {
                        meme_conviction = format!("Meme ({:.0}%)", meme.progress_percent);
                    }
                }

                // ─── SAFETY CHECK (Birdeye Token Security) ───
                // Always run safety check (even in paper trade for demo credibility)
                match Scanner::check_safety_static(&birdeye, &tui_state, &address, age_minutes).await {
                    Ok(true) => {
                        tui_state.log_scanner(&format!("  ✅ ${} passed safety check", overview.symbol));
                    },
                    Ok(false) => {
                        tui_state.log_scanner(&format!("  🛑 Safety Fail: ${} ({})", overview.symbol, meme_conviction));
                        return (None, Some(address.clone()));
                    }
                    Err(_) => {
                        // Security data unavailable = too risky, skip
                        tui_state.log_scanner(&format!("  ❌ ${} Security check unavailable — skipping", overview.symbol));
                        return (None, None);
                    }
                }

                tui_state.log_scanner(&format!("🚀 [TARGET LOCKED!] {} | ${} | Price: ${:.8} | Stats: {}",
                    strategy, overview.symbol, overview.price, meme_conviction));

                let candidate = TokenCandidate {
                    address: address.clone(),
                    symbol: overview.symbol,
                    name: overview.name,
                    price: overview.price,
                    liquidity,
                    volume_5m: m5_volume,
                    price_change_m5,
                    strategy,
                };
            }
        }).buffer_unordered(1); // SEQUENTIAL ONLY to prevent Compute Unit limit explosion

        let mut candidates = Vec::new();
        while let Some((candidate_opt, blacklist_opt)) = stream.next().await {
            if let Some(blacklist_addr) = blacklist_opt {
                self.safety_blacklist.insert(blacklist_addr, Instant::now());
            }
            if let Some(candidate) = candidate_opt {
                candidates.push(candidate);
            }
        }

        if candidates.is_empty() {
            self.tui_state.log_scanner(&format!("  - Scan cycle complete (No candidates found)"));
            return None;
        }

        // ─── SCORING SYSTEM ───
        // Score tokens based on High 5m volume + High 5m price momentum
        candidates.sort_by(|a, b| {
            let score_a = (a.volume_5m / 1000.0) + (a.price_change_m5 * 10.0);
            let score_b = (b.volume_5m / 1000.0) + (b.price_change_m5 * 10.0);
            score_b.partial_cmp(&score_a).unwrap_or(std::cmp::Ordering::Equal)
        });

        let best = candidates.remove(0);
        self.tui_state.log_scanner(&format!("🏆 Best Candidate Selected: ${} (Out of {})", best.symbol, candidates.len() + 1));
        
        Some(best)
    }

    fn match_strategy_static(
        _paper_trade: bool, age_minutes: f64, is_healthy_momentum: bool,
        liquidity: f64, price_change_m5: f64, m5_volume: f64,
        overview: &TokenOverview,
    ) -> Option<String> {
        let h1_change = overview.price_change_1h;
        let h24_change = overview.price_change_24h;
        let h1_volume = overview.volume_1h;
        let avg_m5_from_h1 = if h1_volume > 0.0 { h1_volume / 12.0 } else { 0.0 };
        let is_volume_spiking = avg_m5_from_h1 > 0.0 && m5_volume > (avg_m5_from_h1 * 1.5);

        // Thresholds (unified — Paper = Live)
        let min_liq_new = 4000.0;
        let min_liq_old = 10000.0;
        let min_m5_pct = 1.5;
        let min_h1_pct = 2.0;

        // ═══ STRATEGY 1: Early Momentum (New Token < 1h) ═══
        if age_minutes >= 5.0 && age_minutes <= 60.0 {
            if is_healthy_momentum && liquidity > min_liq_new && price_change_m5 > min_m5_pct {
                return Some(format!("⚡ Early Momentum (5m: +{:.1}%, Liq: ${:.0}K)", price_change_m5, liquidity / 1000.0));
            }
        }

        // ═══ STRATEGY 2: Whale Accumulation (1h pumping + volume spike) ═══
        if age_minutes > 60.0 && h1_change > min_h1_pct && is_volume_spiking 
            && liquidity > min_liq_old && is_healthy_momentum {
            return Some(format!("🐋 Whale Accumulation (1h: +{:.1}%, Vol Spike: {:.0}x)", h1_change, 
                if avg_m5_from_h1 > 0.0 { m5_volume / avg_m5_from_h1 } else { 0.0 }));
        }

        // ═══ STRATEGY 3: Dip Sniper (24h/1h ลบ แต่ 5m กลับตัวแรง) ═══
        if age_minutes > 60.0 && (h1_change < 0.0 || h24_change < -5.0) 
            && price_change_m5 > 3.0 && is_volume_spiking 
            && liquidity > min_liq_old && is_healthy_momentum {
            return Some(format!("🎯 Dip Sniper (24h: {:.1}%, 1h: {:.1}%, 5m reversal: +{:.1}%)", h24_change, h1_change, price_change_m5));
        }

        // ═══ STRATEGY 4: Volume Breakout (5m volume spike ผิดปกติ) ═══
        if age_minutes > 30.0 && avg_m5_from_h1 > 0.0 && m5_volume > (avg_m5_from_h1 * 3.0)
            && liquidity > min_liq_old && price_change_m5 > 0.5 {
            return Some(format!("📊 Volume Breakout (Vol: {:.0}x avg, 5m: +{:.1}%)", m5_volume / avg_m5_from_h1, price_change_m5));
        }

        None
    }

    async fn check_safety_static(birdeye: &BirdeyeClient, tui_state: &crate::tui::TuiState, address: &str, age_minutes: f64) -> Result<bool, String> {
        let security = birdeye.get_token_security(address).await?;

        // Top 10 Holders check (relaxed based on age and Pump.fun curve)
        if let Some(top10) = security.top10_holder_percent {
            // Note: Pump.fun bonding curves hold 80% of the supply initially.
            // If the token is very new (< 60 mins), the top 10 holders will almost always be > 80% due to the curve.
            let max_top10 = if age_minutes < 60.0 { 0.95 } else if age_minutes > 1440.0 { 0.40 } else { 0.30 };
            if top10 > max_top10 {
                tui_state.log_scanner(&format!("  ❌ ${} Top 10 Holders > {:.0}% ({:.1}%)", address, max_top10 * 100.0, top10 * 100.0));
                return Ok(false);
            }
        }

        // Creator percentage check
        if let Some(creator_pct) = security.creator_percentage {
            if creator_pct > 0.15 {
                tui_state.log_scanner(&format!("  ❌ ${} Dev holds {:.1}%", address, creator_pct));
                return Ok(false);
            }
        }

        // Rug check
        if security.is_mintable == Some(true) {
            tui_state.log_scanner(&format!("  ❌ ${} Mintable is ON", address));
            return Ok(false);
        }

        if security.is_freezable == Some(true) {
            tui_state.log_scanner(&format!("  ❌ ${} Freezable is ON", address));
            return Ok(false);
        }

        tui_state.log_scanner(&format!("  ✅ ${} Passed safety checks (Birdeye Security)", address));
        Ok(true)
    }

    /// Scan a single token (for real-time sniper)
    pub async fn scan_single(&mut self, address: &str) -> Option<TokenCandidate> {
        if self.scanned_cache.contains_key(address) {
            return None;
        }

        match self.birdeye.get_token_overview(address).await {
            Ok(overview) => {
                let age_minutes = overview.created_at
                    .map(|ts| (chrono::Utc::now().timestamp() - ts) as f64 / 60.0)
                    .unwrap_or(0.0);
                let liquidity = overview.liquidity;
                let m5_volume = overview.volume_5m;
                let m5_buys = overview.buy_5m;
                let m5_sells = overview.sell_5m;
                let price_change_m5 = overview.price_change_5m;

                // Anti-Wash Trade
                let avg_buy_size = if m5_buys > 0 { m5_volume / m5_buys as f64 } else { 0.0 };
                if m5_buys < 30 && avg_buy_size > 500.0 {
                    return None;
                }

                let is_healthy_momentum = m5_buys > (m5_sells as f64 * 1.2) as u64 && m5_volume > 500.0;

                if let Some(strategy) = Scanner::match_strategy_static(self.config.paper_trade, age_minutes, is_healthy_momentum, liquidity, price_change_m5, m5_volume, &overview) {
                    // Safety check
                    if !self.config.paper_trade {
                        match Scanner::check_safety_static(&self.birdeye, &self.tui_state, address, age_minutes).await {
                            Ok(true) => {},
                            _ => {
                                self.safety_blacklist.insert(address.to_string(), Instant::now());
                                return None;
                            }
                        }
                    }

                    let candidate = TokenCandidate {
                        address: address.to_string(),
                        symbol: overview.symbol.clone(),
                        name: overview.name.clone(),
                        price: overview.price,
                        liquidity,
                        volume_5m: m5_volume,
                        price_change_m5,
                        strategy,
                    };

                    self.scanned_cache.insert(address.to_string(), Instant::now());
                    Some(candidate)
                } else {
                    None
                }
            }
            Err(_) => None,
        }
    }
}
