use crate::birdeye::{BirdeyeClient, TokenOverview};
use crate::config::Config;
use std::collections::HashMap;
use std::time::Instant;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

fn debug_log(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/hawkeye_debug.log") {
        let _ = writeln!(f, "{}", msg);
    }
}

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
    tui_state: Arc<crate::tui::TuiState>,
    pub ws_buffer: Arc<TokioMutex<Vec<crate::websocket::MemeData>>>,
}

impl Scanner {
    pub fn new(birdeye: BirdeyeClient, config: Config, tui_state: Arc<crate::tui::TuiState>) -> Self {
        Self {
            birdeye,
            config,
            scanned_cache: HashMap::new(),
            safety_blacklist: HashMap::new(),
            trade_blacklist: HashMap::new(),
            tui_state,
            ws_buffer: Arc::new(TokioMutex::new(Vec::new())),
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
                debug_log(&format!("API_ERROR_TRENDING: {}", e));
                self.tui_state.log_scanner(&format!("❌ [Scanner] Error fetching trending: {}", e));
                vec![]
            }
        };
        
        let new_listings = match self.birdeye.get_new_listings().await {
            Ok(data) => data,
            Err(e) => {
                debug_log(&format!("API_ERROR_NEW: {}", e));
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

        let mut addresses: Vec<String> = Vec::new();
        
        let mut ws_candidates: Vec<crate::websocket::MemeData> = Vec::new();
        {
            let mut buffer = self.ws_buffer.lock().await;
            ws_candidates.extend(buffer.clone());
            buffer.clear();
        }

        // Take top from trending - FILTER EARLY to save CU
        for t in trending.iter().take(4) {
            if t.liquidity.unwrap_or(0.0) < 3000.0 { continue; } // Skip dead ones immediately
            if !addresses.contains(&t.address) { addresses.push(t.address.clone()); }
        }
        
        // Take top from new listings - FILTER EARLY
        for n in new_listings.iter().take(4) {
            if n.liquidity.unwrap_or(0.0) < 2000.0 { continue; }
            if !addresses.contains(&n.address) { addresses.push(n.address.clone()); }
        }

        // Take top from Meme List V3
        for m in meme_list.iter().take(3) {
            if !addresses.contains(&m.address) { addresses.push(m.address.clone()); }
        }

        self.tui_state.log_scanner(&format!("  📊 Sources: Trending ({}), New ({}), Meme-V3 ({})", 
            trending.len(), new_listings.len(), meme_list.len()));

        let mut addresses_to_scan = Vec::new();
        // Tokens from WebSocket already have some data, we prioritize them
        let mut enriched_ws_to_scan = Vec::new();

        for data in ws_candidates {
            let address = data.address.clone();
            if exclude_addresses.contains(&address) || self.trade_blacklist.contains_key(&address) || 
               self.safety_blacklist.contains_key(&address) { continue; }
            
            if let Some(t) = self.scanned_cache.get(&address) {
                if now.duration_since(*t).as_secs() < 120 { continue; }
            }
            
            self.scanned_cache.insert(address.clone(), now);
            enriched_ws_to_scan.push(data);
            if (enriched_ws_to_scan.len() + addresses_to_scan.len()) >= 8 { break; }
        }

        if (enriched_ws_to_scan.len() + addresses_to_scan.len()) < 8 {
            for address in addresses.iter() {
                if exclude_addresses.contains(address) || self.trade_blacklist.contains_key(address) || 
                   self.safety_blacklist.contains_key(address) { continue; }
                
                if let Some(t) = self.scanned_cache.get(address) {
                    if now.duration_since(*t).as_secs() < 120 { continue; }
                }
                
                // Avoid duplicates already in enriched_ws_to_scan
                if enriched_ws_to_scan.iter().any(|d| &d.address == address) { continue; }

                self.scanned_cache.insert(address.clone(), now);
                addresses_to_scan.push(address.clone());
                
                if (enriched_ws_to_scan.len() + addresses_to_scan.len()) >= 8 { break; }
            }
        }

        use futures_util::stream::{self, StreamExt};

        enum ScanJob {
            Enriched(crate::websocket::MemeData),
            Address(String),
        }

        let mut jobs = Vec::new();
        for d in enriched_ws_to_scan { jobs.push(ScanJob::Enriched(d)); }
        for a in addresses_to_scan { jobs.push(ScanJob::Address(a)); }

        let mut combined_stream = stream::iter(jobs).map(|job| {
            let birdeye = self.birdeye.clone();
            let paper_trade = self.config.paper_trade;
            let tui_state = self.tui_state.clone();
            let trending = trending.clone();

            async move {
                match job {
                    ScanJob::Enriched(data) => {
                        let address = data.address.clone();
                        // Tier 2: Pre-filter based on WS data (ZERO CU)
                        if data.liquidity < 3500.0 {
                            debug_log(&format!("REJECT_WS {} Low liq: ${:.0}", address, data.liquidity));
                            tui_state.log_scanner(&format!("  ❌ {} Low liq (WS): ${:.0}", address, data.liquidity));
                            return (None, None);
                        }

                        // Tier 3: Security Check (Required for both paths)
                        let age_minutes = (chrono::Utc::now().timestamp() as u64 - data.meme_info.creation_time) as f64 / 60.0;
                        
                        match Scanner::check_safety_static(&birdeye, &tui_state, &address, age_minutes).await {
                            Ok(true) => {},
                            _ => return (None, Some(address.clone())),
                        }

                        let strategy = format!("🚀 WS-Fast (Bonding: {:.1}%)", data.meme_info.progress_percent);
                        
                        (Some(TokenCandidate {
                            address: address.clone(),
                            symbol: data.symbol,
                            name: data.name,
                            price: data.price,
                            liquidity: data.liquidity,
                            volume_5m: 0.0,
                            price_change_m5: 0.0,
                            strategy,
                        }), None)
                    }
                    ScanJob::Address(address) => {
                        // Add a longer delay for REST path to prevent CU exhaustion
                        tokio::time::sleep(std::time::Duration::from_millis(2000)).await;

                        // Fetch detailed data from Birdeye
                        let mut overview = match birdeye.get_token_overview(&address).await {
                            Ok(o) => o,
                            Err(e) => {
                                debug_log(&format!("REJECT_API {} Overview Error: {}", address, e));
                                tui_state.log_scanner(&format!("  ❌ {} skip: Overview Error: {}", address, e));
                                return (None, None);
                            }
                        };
                        
                        // Heuristic Honeypot Check (since Free Tier might not have security API)
                        if overview.sell_5m == 0 && overview.buy_5m > 0 {
                            debug_log(&format!("REJECT_HONEYPOT {} Zero sells despite buys", address));
                            tui_state.log_scanner(&format!("  ❌ {} suspected honeypot (0 sells)", address));
                            return (None, Some(address.clone()));
                        }

                        let now_utc = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
                        let age_minutes = (now_utc - overview.created_at.unwrap_or(now_utc)) as f64 / 60.0;

                        // Check static safety (Top 10 holders, Dev percentage, etc.)
                        let is_safe = match Self::check_safety_static(&birdeye, &tui_state, &address, age_minutes).await {
                            Ok(safe) => safe,
                            Err(e) => {
                                debug_log(&format!("SAFETY_ERROR {} - {}", address, e));
                                return (None, Some(address.clone()));
                            }
                        };

                        if !is_safe {
                            return (None, Some(address.clone()));
                        }

                        // Enrich with Trending API data
                        if let Some(t) = trending.iter().find(|t| &t.address == &address) {
                            if overview.market_cap <= 0.0 {
                                overview.market_cap = t.volume_24h.unwrap_or(0.0);
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

                        // ─── FILTERING LOGIC ───
                        let age_minutes = overview.created_at
                            .map(|ts| (chrono::Utc::now().timestamp() - ts) as f64 / 60.0)
                            .unwrap_or(0.0);

                        let m5_buys = overview.buy_5m;
                        let m5_volume = overview.volume_5m;
                        let liquidity = overview.liquidity;
                        let price_change_m5 = overview.price_change_5m;

                        // Anti-Wash Trade (relaxed: only flag if very few buys with huge avg size)
                        let avg_buy_size = if m5_buys > 0 { m5_volume / m5_buys as f64 } else { 0.0 };
                        if m5_buys < 10 && avg_buy_size > 2000.0 {
                            debug_log(&format!("REJECT {} Wash trade (buys:{}, avg:${:.0})", address, m5_buys, avg_buy_size));
                            tui_state.log_scanner(&format!("  ❌ {} Wash trade (buys:{}, avg:${:.0})", address, m5_buys, avg_buy_size));
                            return (None, None);
                        }

                        // HARD FILTERS (relaxed for meme market reality)
                        if liquidity < 2000.0 {
                            debug_log(&format!("REJECT {} Low liq: ${:.0}", address, liquidity));
                            tui_state.log_scanner(&format!("  ❌ {} Low liq: ${:.0}", address, liquidity));
                            return (None, None);
                        }
                        if m5_buys < 2 {
                            debug_log(&format!("REJECT {} Low buys: {} (sell:{}, vol5m:${:.0}, liq:${:.0})", address, m5_buys, overview.sell_5m, m5_volume, liquidity));
                            tui_state.log_scanner(&format!("  ❌ {} Low buys: {}", address, m5_buys));
                            return (None, None);
                        }
                        if overview.volume_24h < 500.0 && age_minutes > 60.0 {
                            debug_log(&format!("REJECT {} Low vol24h: ${:.0} (age:{:.0}m)", address, overview.volume_24h, age_minutes));
                            tui_state.log_scanner(&format!("  ❌ {} Low vol24h: ${:.0}", address, overview.volume_24h));
                            return (None, None);
                        }

                        let is_healthy_momentum = m5_buys >= overview.sell_5m && m5_volume > 100.0;
                        let strategy = Scanner::match_strategy_static(
                            paper_trade, age_minutes, is_healthy_momentum, liquidity,
                            price_change_m5, m5_volume, &overview
                        );

                        let strategy = match strategy {
                            Some(s) => s,
                            None => {
                                debug_log(&format!("REJECT {} No strategy (age:{:.0}m, m5:{:.1}%, liq:${:.0}K, buys:{}, sells:{}, vol5m:${:.0}, healthy:{})", 
                                    address, age_minutes, price_change_m5, liquidity/1000.0, m5_buys, overview.sell_5m, m5_volume, is_healthy_momentum));
                                tui_state.log_scanner(&format!("  ❌ {} No strategy matched (m5:{:.1}%, liq:${:.0}K, buys:{})", address, price_change_m5, liquidity/1000.0, m5_buys));
                                return (None, None);
                            }
                        };

                        // Safety Check
                        match Scanner::check_safety_static(&birdeye, &tui_state, &address, age_minutes).await {
                            Ok(true) => {},
                            Ok(false) => {
                                debug_log(&format!("SAFETY_REJECT {} (age:{:.0}m) - see safety log above", address, age_minutes));
                                return (None, Some(address.clone()));
                            },
                            Err(e) => {
                                debug_log(&format!("SAFETY_ERROR {} - {}", address, e));
                                return (None, Some(address.clone()));
                            }
                        }

                        (Some(TokenCandidate {
                            address: address.clone(),
                            symbol: overview.symbol,
                            name: overview.name,
                            price: overview.price,
                            liquidity,
                            volume_5m: m5_volume,
                            price_change_m5,
                            strategy,
                        }), None)
                    }
                }
            }
        // Process sequentially (1) or at most 2 to avoid Birdeye CU limits
        }).buffer_unordered(1);

        let mut candidates = Vec::new();
        while let Some((candidate_opt, blacklist_opt)) = combined_stream.next().await {
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
        // Thresholds (Balanced: catch opportunities while filtering junk)
        // ┌─────────────────────────────────────────────────────────────┐
        // │ PRODUCTION VALUES (for live trading with real capital):    │
        // │   min_liq_new = 4000, min_liq_old = 10000                 │
        // │   min_m5_pct = 3.0,   min_h1_pct = 10.0                  │
        // │ CURRENT VALUES (tuned for demo & paper trade):            │
        // └─────────────────────────────────────────────────────────────┘
        let min_liq_new = 2000.0;
        let min_liq_old = 5000.0;
        let min_m5_pct = 1.0;
        let min_h1_pct = 5.0;

        // ═══ STRATEGY 1: Early Momentum (New Token < 2h) ═══
        if age_minutes >= 2.0 && age_minutes <= 120.0 {
            if is_healthy_momentum && liquidity > min_liq_new && price_change_m5 > min_m5_pct {
                return Some(format!("⚡ Early Momentum (5m: +{:.1}%, Liq: ${:.0}K)", price_change_m5, liquidity / 1000.0));
            }
        }

        // ═══ STRATEGY 2: Whale Accumulation (1h pumping + volume spike) ═══
        if age_minutes > 30.0 && h1_change > min_h1_pct && is_volume_spiking 
            && liquidity > min_liq_old && is_healthy_momentum {
            return Some(format!("🐋 Whale Accumulation (1h: +{:.1}%, Vol Spike: {:.0}x)", h1_change, 
                if avg_m5_from_h1 > 0.0 { m5_volume / avg_m5_from_h1 } else { 0.0 }));
        }

        // ═══ STRATEGY 3: Dip Sniper (24h/1h ลบ แต่ 5m กลับตัวแรง) ═══
        if age_minutes > 30.0 && (h1_change < 0.0 || h24_change < -5.0) 
            && price_change_m5 > 1.5 && is_volume_spiking 
            && liquidity > min_liq_old && is_healthy_momentum {
            return Some(format!("🎯 Dip Sniper (24h: {:.1}%, 1h: {:.1}%, 5m reversal: +{:.1}%)", h24_change, h1_change, price_change_m5));
        }

        // ═══ STRATEGY 4: Volume Breakout (5m volume spike ผิดปกติ) ═══
        if age_minutes > 15.0 && avg_m5_from_h1 > 0.0 && m5_volume > (avg_m5_from_h1 * 3.0)
            && liquidity > min_liq_new && price_change_m5 > 2.0 {
            return Some(format!("📊 Volume Breakout (Vol: {:.0}x avg, 5m: +{:.1}%)", m5_volume / avg_m5_from_h1, price_change_m5));
        }

        // ═══ STRATEGY 5: Fresh Pump (Brand new token with aggressive positive momentum) ═══
        if age_minutes < 30.0 && liquidity > min_liq_new && price_change_m5 > 2.0 && m5_volume > 300.0 {
            return Some(format!("🔥 Fresh Pump (Age: {:.0}m, 5m: +{:.1}%)", age_minutes, price_change_m5));
        }

        // ═══ STRATEGY 6: Active Trading (Any token with positive movement and decent activity) ═══
        if liquidity > min_liq_new && price_change_m5 > 0.5 && m5_volume > 100.0 && is_healthy_momentum {
            return Some(format!("📈 Active Trading (5m: +{:.1}%, Vol: ${:.0})", price_change_m5, m5_volume));
        }

        None
    }

    async fn check_safety_static(birdeye: &BirdeyeClient, tui_state: &crate::tui::TuiState, address: &str, age_minutes: f64) -> Result<bool, String> {
        let security = match birdeye.get_token_security(address).await {
            Ok(s) => s,
            Err(e) => {
                if e.contains("401") || e.contains("403") || e.contains("Compute units") {
                    debug_log(&format!("SAFETY_WARNING {} - API Limit ({}). Bypassing security.", address, e));
                    return Ok(true);
                } else {
                    return Err(e);
                }
            }
        };

        debug_log(&format!("SECURITY_DATA {} top10:{:?} creator:{:?} mintable:{:?} freezable:{:?}", 
            address, security.top10_holder_percent, security.creator_percentage, security.is_mintable, security.is_freezable));

        // Top 10 Holders check (relaxed based on age and Pump.fun curve)
        if let Some(top10) = security.top10_holder_percent {
            // Normalize: if value > 1.0, Birdeye sent it as percentage (85.0 = 85%)
            let top10_normalized = if top10 > 1.0 { top10 / 100.0 } else { top10 };
            // Note: Pump.fun bonding curves hold 80% of the supply initially.
            // Meme tokens almost always have high top10 concentration, even after hours.
            let max_top10 = if age_minutes < 60.0 { 0.98 } else if age_minutes < 360.0 { 0.90 } else if age_minutes > 1440.0 { 0.50 } else { 0.80 };
            if top10_normalized > max_top10 {
                debug_log(&format!("REJECT_TOP10 {} raw:{} norm:{:.3} max:{:.2} age:{:.0}m", address, top10, top10_normalized, max_top10, age_minutes));
                tui_state.log_scanner(&format!("  ❌ {} Top10: {:.1}% > {:.0}%", address, top10_normalized * 100.0, max_top10 * 100.0));
                return Ok(false);
            }
        }

        // Creator percentage check
        if let Some(creator_pct) = security.creator_percentage {
            // Normalize: if value > 1.0, it's a percentage
            let creator_normalized = if creator_pct > 1.0 { creator_pct / 100.0 } else { creator_pct };
            if creator_normalized > 0.15 {
                debug_log(&format!("REJECT_CREATOR {} raw:{} norm:{:.3}", address, creator_pct, creator_normalized));
                tui_state.log_scanner(&format!("  ❌ {} Dev holds {:.1}%", address, creator_normalized * 100.0));
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

        if security.is_honeypot == Some(true) {
            tui_state.log_scanner(&format!("  ❌ ${} Honeypot detected", address));
            return Ok(false);
        }

        tui_state.log_scanner(&format!("  ✅ ${} Passed safety checks (Birdeye Security)", address));
        Ok(true)
    }
}
