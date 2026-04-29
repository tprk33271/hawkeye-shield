use crate::birdeye::BirdeyeClient;
use crate::config::Config;
use crate::scanner::TokenCandidate;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use solana_client::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::signature::Signer;
use solana_sdk::transaction::VersionedTransaction;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex;

// ============================================================
// Trade Executor — Buy/Sell via Jupiter + Position Management
// ============================================================

const SOL_MINT: &str = "So11111111111111111111111111111111111111112";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveTrade {
    pub symbol: String,
    pub entry_price: f64,
    pub spent_sol: f64,
    pub current_price: f64,
    pub pnl_pct: f64,
    pub tp1_hit: bool,
    pub break_even_hit: bool,
    pub highest_price: f64,
    pub stop_price: f64,
    pub start_time: u64,
    pub status: String,
}

pub struct TradeExecutor {
    config: Config,
    birdeye: BirdeyeClient,
    read_rpc: RpcClient,
    write_rpc: RpcClient,
    pub active_trades: Arc<Mutex<HashMap<String, ActiveTrade>>>,
    jup_base: String,
    pub tui_state: std::sync::Arc<crate::tui::TuiState>,
}

pub enum NewPairType {
    Raydium,
    PumpFun,
}

impl TradeExecutor {
    pub fn new(config: Config, birdeye: BirdeyeClient, active_trades: Arc<Mutex<HashMap<String, ActiveTrade>>>, tui_state: std::sync::Arc<crate::tui::TuiState>) -> Self {
        let read_rpc = RpcClient::new_with_commitment(
            config.solana_rpc_url.clone(),
            CommitmentConfig::confirmed(),
        );
        let write_rpc = RpcClient::new_with_commitment(
            config.solana_write_rpc_url.clone(),
            CommitmentConfig::confirmed(),
        );

        let jup_base = if config.use_ultra {
            "https://api.jup.ag/ultra/v1".to_string()
        } else {
            "https://api.jup.ag/v6".to_string()
        };

        Self {
            config,
            birdeye,
            read_rpc,
            write_rpc,
            active_trades,
            jup_base,
            tui_state,
        }
    }

    pub fn get_sol_balance(&self) -> f64 {
        match &self.config.wallet {
            Some(kp) => {
                self.read_rpc.get_balance(&kp.pubkey())
                    .map(|b| b as f64 / 1e9)
                    .unwrap_or(0.0)
            }
            None => 0.0,
        }
    }

    /// Fetch Mint address from a transaction signature (Real-time Sniper logic)
    pub async fn fetch_mint_from_sig(&self, signature: &str, pair_type: NewPairType) -> Option<String> {
        use solana_client::rpc_config::RpcTransactionConfig;
        use solana_transaction_status::UiTransactionEncoding;
        use solana_sdk::signature::Signature;

        let sig = match Signature::from_str(signature) {
            Ok(s) => s,
            Err(_) => return None,
        };

        let config = RpcTransactionConfig {
            encoding: Some(UiTransactionEncoding::JsonParsed),
            commitment: Some(CommitmentConfig::confirmed()),
            max_supported_transaction_version: Some(0),
        };

        match self.read_rpc.get_transaction_with_config(&sig, config) {
            Ok(tx) => {
                let meta = tx.transaction.meta?;
                
                // Better approach: Look at postTokenBalances to find the new token
                if let solana_transaction_status::option_serializer::OptionSerializer::Some(balances) = meta.post_token_balances {
                    for balance in balances {
                        let mint = balance.mint;
                        // Skip WSOL
                        if mint != SOL_MINT {
                            return Some(mint);
                        }
                    }
                }

                // Fallback to account indices if balances are empty (unlikely for successful pool creation)
                let msg = tx.transaction.transaction.decode()?.message;
                let accounts = msg.static_account_keys();
                
                match pair_type {
                    NewPairType::PumpFun => {
                        if accounts.len() > 1 {
                            return Some(accounts[1].to_string());
                        }
                    }
                    NewPairType::Raydium => {
                        // Raydium Initialize2: accounts 8 and 9 are the mints
                        if accounts.len() > 9 {
                            let m1 = accounts[8].to_string();
                            let m2 = accounts[9].to_string();
                            return if m1 == SOL_MINT { Some(m2) } else { Some(m1) };
                        }
                    }
                }
                None
            }
            Err(_e) => {
                None
            }
        }
    }

    /// Get token balance for wallet (amount + decimals)
    fn get_token_balance(&self, mint: &str) -> (u64, u8) {
        let wallet = match &self.config.wallet {
            Some(kp) => kp.pubkey(),
            None => return (0, 6),
        };
        let mint_pk = match solana_sdk::pubkey::Pubkey::from_str(mint) {
            Ok(pk) => pk,
            Err(_) => return (0, 6),
        };
        match self.read_rpc.get_token_accounts_by_owner(
            &wallet,
            solana_client::rpc_request::TokenAccountsFilter::Mint(mint_pk),
        ) {
            Ok(accounts) if !accounts.is_empty() => {
                if let Some(data) = accounts[0].account.data.decode() {
                    // SPL Token account: amount at offset 64, 8 bytes LE
                    if data.len() >= 72 {
                        let amount = u64::from_le_bytes(data[64..72].try_into().unwrap_or([0u8; 8]));
                        // decimals need mint info, default 6 for most meme tokens
                        return (amount, 6);
                    }
                }
                (0, 6)
            }
            _ => (0, 6),
        }
    }

    /// Calculate actual entry price from on-chain data (Birdeye-powered)
    #[allow(dead_code)]
    async fn calc_actual_entry_price(&self, token_mint: &str, sol_spent: f64) -> f64 {
        // 1. Get SOL price from Birdeye (reliable)
        let sol_price_usd = self.birdeye.get_price(SOL_MINT).await.unwrap_or(0.0);

        // 2. Try to get token balance (retry up to 5 times)
        let before_balance = self.get_token_balance(token_mint);
        tokio::time::sleep(std::time::Duration::from_millis(2000)).await;

        let mut tokens_received: u64 = 0;
        let mut decimals: u8 = before_balance.1;
        for i in 0..5 {
            let after = self.get_token_balance(token_mint);
            if after.0 > before_balance.0 {
                tokens_received = after.0 - before_balance.0;
                decimals = after.1;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if i == 4 {
                self.tui_state.log_trade(&format!("⚠️ Token balance unchanged — Fallback to Birdeye price"));
            }
        }

        // 3. Calculate actual price
        let ui_amount = tokens_received as f64 / 10f64.powi(decimals as i32);
        if ui_amount > 0.0 && sol_price_usd > 0.0 {
            let actual = (sol_spent * sol_price_usd) / ui_amount;
            self.tui_state.log_trade(&format!("✅ [On-Chain] Received {:.2} tokens (Entry: ${:.8})", ui_amount, actual));
            return actual;
        }

        // 4. Fallback: Birdeye token price (much better than DexScreener)
        if let Ok(price) = self.birdeye.get_price(token_mint).await {
            if price > 0.0 {
                self.tui_state.log_trade(&format!("📊 [Birdeye Fallback] Entry: ${:.8}", price));
                return price;
            }
        }

        // 5. Last resort: Jupiter price
        if let Some(price) = self.refresh_price(token_mint).await {
            self.tui_state.log_trade(&format!("⚠️ [Jupiter Fallback] Entry: ${:.8}", price));
            return price;
        }

        0.0
    }

    /// Refresh token price via Birdeye (primary) + Jupiter (fallback)
    async fn refresh_price(&self, address: &str) -> Option<f64> {
        // Primary: Birdeye
        if let Ok(price) = self.birdeye.get_price(address).await {
            if price > 0.0 { return Some(price); }
        }

        // Fallback: Jupiter Price API
        let client = reqwest::Client::new();
        if let Ok(resp) = client.get(format!("https://api.jup.ag/price/v2?ids={}", address))
            .send().await
        {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                if let Some(price) = body["data"][address]["price"].as_str()
                    .and_then(|p| p.parse::<f64>().ok())
                {
                    if price > 0.0 { return Some(price); }
                }
            }
        }

        None
    }

    /// Execute buy order via Jupiter
    pub async fn execute_buy(&self, token: &TokenCandidate, amount_sol: f64) -> Result<BuyResult, String> {
        if self.config.paper_trade {
            return self.paper_buy(token, amount_sol).await;
        }

        let wallet = self.config.wallet.as_ref()
            .ok_or("No wallet configured")?;

        // Record token balance BEFORE buy
        let before_balance = self.get_token_balance(&token.address);
        // Get SOL price from Birdeye BEFORE buy (for accurate entry calc)
        let sol_price_usd = self.birdeye.get_price(SOL_MINT).await.unwrap_or(0.0);

        let lamports = (amount_sol * 1e9) as u64;
        let client = reqwest::Client::new();

        let quote_url = if self.config.use_ultra {
            format!("{}/order?inputMint={}&outputMint={}&amount={}&slippageBps={}&taker={}&computeUnitPriceMicroLamports={}",
                self.jup_base, SOL_MINT, token.address, lamports,
                self.config.slippage_bps, wallet.pubkey(),
                self.config.priority_fee_micro_lamports)
        } else {
            format!("{}/quote?inputMint={}&outputMint={}&amount={}&slippageBps={}",
                self.jup_base, SOL_MINT, token.address, lamports, self.config.slippage_bps)
        };

        let mut headers = reqwest::header::HeaderMap::new();
        if !self.config.jupiter_api_key.is_empty() {
            if let Ok(val) = self.config.jupiter_api_key.parse() {
                headers.insert("x-api-key", val);
            }
        }

        let quote: serde_json::Value = client.get(&quote_url)
            .headers(headers.clone())
            .send().await.map_err(|e| e.to_string())?
            .json().await.map_err(|e| e.to_string())?;

        if quote.get("error").is_some() || quote.get("errorCode").is_some() {
            return Err(format!("Jupiter quote error: {:?}", quote));
        }

        let swap_tx_base64 = if self.config.use_ultra {
            quote["transaction"].as_str().ok_or("No transaction in Ultra quote")?.to_string()
        } else {
            let swap_body = serde_json::json!({
                "quoteResponse": quote,
                "userPublicKey": wallet.pubkey().to_string(),
                "wrapAndUnwrapSol": true,
                "prioritizationFeeLamports": (self.config.priority_fee_micro_lamports / 1000)
            });
            let swap_url = format!("{}/swap", self.jup_base);
            let swap_resp: serde_json::Value = client.post(&swap_url)
                .headers(headers.clone())
                .json(&swap_body)
                .send().await.map_err(|e| e.to_string())?
                .json().await.map_err(|e| e.to_string())?;
            swap_resp["swapTransaction"].as_str()
                .ok_or("No swapTransaction in response")?.to_string()
        };

        let tx_bytes = base64::engine::general_purpose::STANDARD
            .decode(&swap_tx_base64).map_err(|e| e.to_string())?;
        let mut tx: VersionedTransaction = bincode::deserialize(&tx_bytes)
            .map_err(|e| e.to_string())?;
        let message_bytes = tx.message.serialize();
        let signature = wallet.sign_message(&message_bytes);
        tx.signatures[0] = signature;

        let txid = if self.config.use_ultra {
            let signed_b64 = base64::engine::general_purpose::STANDARD
                .encode(bincode::serialize(&tx).map_err(|e| e.to_string())?);
            let request_id = quote["requestId"].as_str().unwrap_or("");
            let exec_body = serde_json::json!({
                "requestId": request_id,
                "signedTransaction": signed_b64
            });
            let exec_url = format!("{}/execute", self.jup_base);
            let exec_resp: serde_json::Value = client.post(&exec_url)
                .headers(headers)
                .json(&exec_body)
                .send().await.map_err(|e| e.to_string())?
                .json().await.map_err(|e| e.to_string())?;
            exec_resp["signature"].as_str()
                .or(exec_resp["txid"].as_str())
                .ok_or("No txid from Jupiter Ultra")?
                .to_string()
        } else {
            let config = solana_client::rpc_config::RpcSendTransactionConfig {
                skip_preflight: true,
                max_retries: Some(2),
                ..Default::default()
            };
            let sig = self.write_rpc.send_transaction_with_config(&tx, config)
                .map_err(|e| e.to_string())?;
            sig.to_string()
        };

        // ─── ACTUAL ENTRY PRICE (fixed: on-chain balance diff) ───
        // Wait for TX to land, then check actual tokens received
        tokio::time::sleep(std::time::Duration::from_millis(2000)).await;

        let mut actual_price = 0.0;
        // Try on-chain balance diff (most accurate)
        for i in 0..8 {
            let after_balance = self.get_token_balance(&token.address);
            if after_balance.0 > before_balance.0 {
                let tokens_received = after_balance.0 - before_balance.0;
                let decimals = after_balance.1;
                let ui_amount = tokens_received as f64 / 10f64.powi(decimals as i32);
                if ui_amount > 0.0 && sol_price_usd > 0.0 {
                    actual_price = (amount_sol * sol_price_usd) / ui_amount;
                    self.tui_state.log_trade(&format!("✅ [On-Chain] Received {:.2} ${} (Entry: ${:.8})",
                        ui_amount, token.symbol, actual_price));
                    break;
                }
            }
            if i < 7 {
                tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            }
        }

        // Fallback: Birdeye price (much more reliable than old DexScreener)
        if actual_price <= 0.0 {
            if let Ok(price) = self.birdeye.get_price(&token.address).await {
                if price > 0.0 {
                    actual_price = price;
                    self.tui_state.log_trade(&format!("📊 [Birdeye] Entry fallback: ${:.8}", actual_price));
                }
            }
        }
        // Last resort: Jupiter price
        if actual_price <= 0.0 {
            actual_price = self.refresh_price(&token.address).await.unwrap_or(token.price);
            self.tui_state.log_trade(&format!("⚠️ [Jupiter] Entry last-resort: ${:.8}", actual_price));
        }

        self.tui_state.log_trade(&format!("✅ [BUY] ${} @ ${:.8} | TX: {}", token.symbol, actual_price, txid));

        Ok(BuyResult {
            success: true,
            txid: Some(txid),
            buy_price: actual_price,
            trade_size_sol: amount_sol,
        })
    }

    /// Paper trade buy (no real TX)
    async fn paper_buy(&self, token: &TokenCandidate, amount_sol: f64) -> Result<BuyResult, String> {
        // Try multiple price sources
        let mut price = self.refresh_price(&token.address).await.unwrap_or(0.0);
        if price <= 0.0 {
            price = token.price; // Use scanner's cached price
        }
        if price <= 0.0 {
            // Try token overview as last resort
            if let Ok(overview) = self.birdeye.get_token_overview(&token.address).await {
                if overview.price > 0.0 {
                    price = overview.price;
                }
            }
        }
        if price <= 0.0 {
            return Err(format!("Cannot buy ${}: price is $0 (too new)", token.symbol));
        }

        self.tui_state.log_trade(&format!("📝 [PAPER BUY] ${} @ ${:.8} ({:.4} SOL)", token.symbol, price, amount_sol));

        if let Ok(mut bal) = self.tui_state.balance.lock() {
            *bal -= amount_sol;
        }

        Ok(BuyResult {
            success: true,
            txid: Some(format!("PAPER_{}", chrono::Utc::now().timestamp())),
            buy_price: price,
            trade_size_sol: amount_sol,
        })
    }

    /// Monitor position and execute sell when conditions met
    pub async fn monitor_and_sell(
        &self,
        token: TokenCandidate,
        buy_price: f64,
        spent_sol: f64,
    ) -> f64 {
        // Guard: if buy_price is 0 or invalid, abort immediately
        if buy_price <= 0.0 || buy_price.is_nan() {
            self.tui_state.log_trade(&format!("⚠️ [Monitor] ${} skipped: invalid entry price ${:.8}", token.symbol, buy_price));
            self.remove_trade(&token.address).await;
            return 0.0;
        }

        let address = token.address.clone();
        let symbol = token.symbol.clone();
        let start = std::time::Instant::now();
        let max_monitor_ms: u128 = 15 * 60 * 1000;

        let stop_loss_pct = self.config.stop_loss_pct;
        let take_profit_pct = self.config.take_profit_pct;

        let mut tp1_hit = false;
        let mut break_even_hit = false;
        let mut highest_price = buy_price;
        let mut stop_price = buy_price * (1.0 - stop_loss_pct / 100.0);
        let mut current_price = buy_price;

        // Register active trade
        {
            let mut trades = self.active_trades.lock().await;
            trades.insert(address.clone(), ActiveTrade {
                symbol: symbol.clone(),
                entry_price: buy_price,
                spent_sol,
                current_price: buy_price,
                pnl_pct: 0.0,
                tp1_hit: false,
                break_even_hit: false,
                highest_price: buy_price,
                stop_price,
                start_time: chrono::Utc::now().timestamp() as u64,
                status: "Monitoring...".to_string(),
            });
        }

        self.tui_state.log_trade(&format!("🔔 [Monitor] Monitoring ${} (Entry: ${:.8}, Stop: ${:.8})", symbol, buy_price, stop_price));

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            if let Some(price) = self.refresh_price(&address).await {
                current_price = price;
            }

            let pnl_pct = if buy_price > 0.0 {
                ((current_price - buy_price) / buy_price) * 100.0
            } else {
                0.0
            };
            let pnl_pct = if pnl_pct.is_nan() || pnl_pct.is_infinite() { 0.0 } else { pnl_pct };
            let elapsed_ms = start.elapsed().as_millis();

            // Update active trade state
            {
                let mut trades = self.active_trades.lock().await;
                if let Some(trade) = trades.get_mut(&address) {
                    trade.current_price = current_price;
                    trade.pnl_pct = pnl_pct;
                    trade.stop_price = stop_price;
                    trade.tp1_hit = tp1_hit;
                    trade.break_even_hit = break_even_hit;
                    trade.highest_price = highest_price;
                }
            }

            // Print beautiful PNL log every ~10 seconds
            // (Removed PNL Tracker logging here since the TUI active trades table handles it)

            // ─── EXIT CONDITIONS ───

            if pnl_pct <= -50.0 {
                self.tui_state.log_trade(&format!("🛑 [HARD-STOP] ${} PNL: {:.2}%", symbol, pnl_pct));
                let current_value_sol = spent_sol * (1.0 + (pnl_pct / 100.0));
                self.execute_sell_action(&token, 100, "HARD-STOP", current_value_sol).await;
                self.remove_trade(&address).await;
                return pnl_pct;
            }

            if elapsed_ms > max_monitor_ms {
                self.tui_state.log_trade(&format!("⏰ [TIMEOUT] ${} PNL: {:.2}%", symbol, pnl_pct));
                let current_value_sol = spent_sol * (1.0 + (pnl_pct / 100.0));
                self.execute_sell_action(&token, 100, "TIMEOUT", current_value_sol).await;
                self.remove_trade(&address).await;
                return pnl_pct;
            }

            if current_price <= stop_price {
                let reason = if tp1_hit { "TRAILING-STOP" }
                    else if break_even_hit { "BREAK-EVEN" }
                    else { "STOP-LOSS" };
                self.tui_state.log_trade(&format!("🛑 [{}] ${} PNL: {:.2}%", reason, symbol, pnl_pct));
                let current_value_sol = spent_sol * (1.0 + (pnl_pct / 100.0));
                self.execute_sell_action(&token, 100, reason, current_value_sol).await;
                self.remove_trade(&address).await;
                return pnl_pct;
            }

            if pnl_pct >= 20.0 && !break_even_hit {
                break_even_hit = true;
                let new_stop = buy_price * 1.05;
                if new_stop > stop_price { stop_price = new_stop; }
                self.tui_state.log_trade(&format!("🛡️ [Break-Even] ${} locked at ${:.8}", symbol, stop_price));
            }

            if pnl_pct >= take_profit_pct && !tp1_hit {
                self.tui_state.log_trade(&format!("🎉 [TP1] ${} selling 50% to recover initial (PNL: {:.2}%)", symbol, pnl_pct));
                let current_value_sol = spent_sol * (1.0 + (pnl_pct / 100.0));
                self.execute_sell_action(&token, 50, "TP1", current_value_sol).await;
                tp1_hit = true;
                stop_price = buy_price * 1.10;
                self.tui_state.log_trade(&format!("🛡️ [Safety] ${} profit locked at ${:.8}", symbol, stop_price));
            }

            if current_price > highest_price {
                highest_price = current_price;
                if tp1_hit {
                    let new_stop = highest_price * 0.75;
                    if new_stop > stop_price {
                        stop_price = new_stop;
                        self.tui_state.log_trade(&format!("📈 [Trailing] ${} Stop → ${:.8}", symbol, stop_price));
                    }
                } else if pnl_pct >= 20.0 {
                    let new_stop = highest_price * 0.85;
                    if new_stop > stop_price { stop_price = new_stop; }
                }
            }
        }
    }

    async fn execute_sell_action(&self, token: &TokenCandidate, pct: u8, reason: &str, current_value_sol: f64) {
        if self.config.paper_trade {
            let received = current_value_sol * (pct as f64 / 100.0);
            if let Ok(mut bal) = self.tui_state.balance.lock() {
                *bal += received;
            }
            self.tui_state.log_trade(&format!("📝 [PAPER SELL {}%] ${} received {:.4} SOL (Reason: {})", pct, token.symbol, received, reason));
            return;
        }

        let wallet = match &self.config.wallet {
            Some(kp) => kp,
            None => {
                self.tui_state.log_trade(&format!("❌ [SELL FAILED] No wallet configured"));
                return;
            }
        };

        // Get token balance
        let (amount, _decimals) = self.get_token_balance(&token.address);
        if amount == 0 {
            self.tui_state.log_trade(&format!("❌ [SELL FAILED] ${} balance is 0", token.symbol));
            return;
        }

        let sell_amount = (amount as f64 * (pct as f64 / 100.0)) as u64;
        if sell_amount == 0 { return; }

        self.tui_state.log_trade(&format!("💰 [LIVE SELL {}%] ${} amount={} (Reason: {})", pct, token.symbol, sell_amount, reason));

        let client = reqwest::Client::new();
        let quote_url = format!("{}/quote?inputMint={}&outputMint={}&amount={}&slippageBps={}",
            self.jup_base, token.address, SOL_MINT, sell_amount, self.config.slippage_bps);

        let mut headers = reqwest::header::HeaderMap::new();
        if !self.config.jupiter_api_key.is_empty() {
            if let Ok(val) = self.config.jupiter_api_key.parse() {
                headers.insert("x-api-key", val);
            }
        }

        match client.get(&quote_url).headers(headers.clone()).send().await {
            Ok(resp) => {
                if let Ok(quote) = resp.json::<serde_json::Value>().await {
                    let swap_body = serde_json::json!({
                        "quoteResponse": quote,
                        "userPublicKey": wallet.pubkey().to_string(),
                        "wrapAndUnwrapSol": true,
                        "prioritizationFeeLamports": (self.config.priority_fee_micro_lamports / 1000)
                    });
                    
                    let swap_url = format!("{}/swap", self.jup_base);
                    if let Ok(swap_resp) = client.post(&swap_url).headers(headers).json(&swap_body).send().await {
                        if let Ok(json) = swap_resp.json::<serde_json::Value>().await {
                            if let Some(swap_tx_base64) = json["swapTransaction"].as_str() {
                                if let Ok(tx_bytes) = base64::engine::general_purpose::STANDARD.decode(swap_tx_base64) {
                                    if let Ok(mut tx) = bincode::deserialize::<VersionedTransaction>(&tx_bytes) {
                                        let message_bytes = tx.message.serialize();
                                        let signature = wallet.sign_message(&message_bytes);
                                        tx.signatures[0] = signature;

                                        let send_config = solana_client::rpc_config::RpcSendTransactionConfig {
                                            skip_preflight: true,
                                            max_retries: Some(2),
                                            ..Default::default()
                                        };

                                        match self.write_rpc.send_transaction_with_config(&tx, send_config) {
                                            Ok(sig) => {
                                                self.tui_state.log_trade(&format!("✅ [SELL SUCCESS] ${} TX: {}", token.symbol, sig));
                                            }
                                            Err(e) => {
                                                self.tui_state.log_trade(&format!("❌ [SELL ERROR] Send Failed: {}", e));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                self.tui_state.log_trade(&format!("❌ [SELL ERROR] Quote Failed: {}", e));
            }
        }
    }

    pub async fn remove_trade(&self, address: &str) {
        let mut trades = self.active_trades.lock().await;
        trades.remove(address);
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct BuyResult {
    pub success: bool,
    pub txid: Option<String>,
    pub buy_price: f64,
    pub trade_size_sol: f64,
}
