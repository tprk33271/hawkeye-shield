use solana_sdk::signature::{Keypair, Signer};
use std::env;
use std::sync::Arc;

#[derive(Clone)]
pub struct Config {
    // Birdeye
    pub birdeye_api_key: String,
    // Solana
    pub solana_rpc_url: String,
    pub solana_write_rpc_url: String,
    pub wallet: Option<Arc<Keypair>>,
    // Trading
    pub trade_size_sol: f64,
    pub use_dynamic_sizing: bool,
    pub kelly_fraction: f64,
    pub take_profit_pct: f64,
    pub stop_loss_pct: f64,
    pub slippage_bps: u64,
    pub max_active_trades: usize,
    // Jupiter
    pub jupiter_api_key: String,
    pub use_ultra: bool,
    pub priority_fee_micro_lamports: u64,
    // Mode
    pub paper_trade: bool,
}

impl Config {
    pub fn from_env() -> Self {
        dotenv::dotenv().ok();

        let private_key_str = env::var("SOLANA_PRIVATE_KEY").unwrap_or_default();
        let wallet = if !private_key_str.is_empty() && private_key_str != "your_base58_private_key_here" {
            match bs58::decode(&private_key_str).into_vec() {
                Ok(bytes) => {
                    match Keypair::try_from(bytes.as_slice()) {
                        Ok(kp) => Some(Arc::new(kp)),
                        Err(_) => {
                            tracing::error!("❌ Invalid SOLANA_PRIVATE_KEY format");
                            None
                        }
                    }
                }
                Err(_) => {
                    tracing::error!("❌ SOLANA_PRIVATE_KEY is not valid Base58");
                    None
                }
            }
        } else {
            None
        };

        let rpc_url = env::var("SOLANA_RPC_URL")
            .unwrap_or_else(|_| "https://api.mainnet-beta.solana.com".to_string());

        Self {
            birdeye_api_key: env::var("BIRDEYE_API_KEY").unwrap_or_default(),
            solana_rpc_url: rpc_url.clone(),
            solana_write_rpc_url: env::var("SOLANA_WRITE_RPC_URL").unwrap_or(rpc_url),
            wallet,
            trade_size_sol: env::var("TRADE_SIZE_SOL")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(0.05),
            use_dynamic_sizing: env::var("USE_DYNAMIC_SIZING")
                .map(|v| v == "true").unwrap_or(true),
            kelly_fraction: env::var("KELLY_FRACTION")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(0.15),
            take_profit_pct: env::var("TAKE_PROFIT_PERCENT")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(40.0),
            stop_loss_pct: env::var("STOP_LOSS_PERCENT")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(12.0),
            slippage_bps: env::var("SLIPPAGE_BPS")
                .ok().and_then(|v| v.trim().split_whitespace().next()
                    .and_then(|s| s.parse().ok())).unwrap_or(250),
            max_active_trades: env::var("MAX_ACTIVE_TRADES")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(2),
            jupiter_api_key: env::var("JUPITER_API_KEY").unwrap_or_default(),
            use_ultra: env::var("USE_JUPITER_ULTRA")
                .map(|v| v == "true").unwrap_or(true),
            priority_fee_micro_lamports: env::var("PRIORITY_FEE_MICRO_LAMPORTS")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(1_000_000),
            paper_trade: env::var("PAPER_TRADE")
                .map(|v| v == "true").unwrap_or(true),
        }
    }

    #[allow(dead_code)]
    pub fn wallet_pubkey_str(&self) -> String {
        self.wallet.as_ref()
            .map(|k| k.pubkey().to_string())
            .unwrap_or_else(|| "NO_WALLET".to_string())
    }
}
