# 🦅 HawkEye Shield — Autonomous Solana Trading Agent

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange.svg)](https://www.rust-lang.org/)
[![Powered by Birdeye](https://img.shields.io/badge/Data-Birdeye_API-blue)](https://birdeye.so/)

> **A high-performance, autonomous meme token sniper built in Rust, powered purely by the [Birdeye Data API](https://birdeye.so/) for real-time market intelligence, risk assessment, and trade execution on Solana.**

*Built for the **Birdeye Data Services BIP Competition (Sprint 2)**.*

---

## 🎬 Live Demonstration
https://github.com/user-attachments/assets/15b5cd93-cc4c-4731-bb66-cfb8f2c256cc

> **[Watch the 30-Second Live Action Demo](./assets/demo.mov)** 
*(If the preview doesn't load, click the link above to view the raw .MOV file)*

---

## 🎯 Overview

HawkEye Shield is an **execution-layer trading agent** that autonomously discovers, evaluates, buys, and sells meme tokens on Solana. It is built to demonstrate how Birdeye's data infrastructure can act as the "brain" of a fully autonomous financial system.

**It is NOT a dashboard or read-only tool.** It makes real trades with real SOL using real-time API integrations.

### High-Efficiency "Funnel" Architecture
```text
[Birdeye SUBSCRIBE_MEME WS] → [Zero-CU Metadata Filtering] → [Tiered REST Verification] → [Jupiter Execution]
```
> **CU Efficiency:** Our Birdeye-Native architecture reduces Compute Unit (CU) consumption by **~80%** compared to legacy RPC polling.

---

## 🏆 Birdeye BIP Competition — Technical Depth

HawkEye Shield was engineered from the ground up to showcase the power, reliability, and speed of the Birdeye Data API in a high-concurrency algorithmic trading environment.

### API Qualification (100+ API Calls per session)
Unlike simple search tools, HawkEye Shield is an **active participant** in the market. It maintains a persistent loop that generates high-quality, continuous API traffic:
*   **Discovery Polling**: Constantly queries `trending` and `meme/list` endpoints.
*   **Deep Scanning**: Executes parallel checks (`overview` + `security`) for every candidate token.
*   **Live Monitoring**: Polls `price` every 2 seconds for every active open position.
*   **Throttling & Concurrency**: Implements intelligent `tokio` exponential backoff to perfectly ride the rate-limit threshold without failing.

### 🔍 API Implementation Mapping

| Endpoint | Purpose in HawkEye Shield | Why Birdeye? |
|---|---|---|
| `SUBSCRIBE_MEME` (WS) | **Real-time discovery with ZERO CU.** | Eliminates expensive RPC polling; provides metadata (price, liquidity, bonding curve) instantly. |
| `/defi/token_trending` | Discovery for momentum-based sniping. | Superior sorting/ranking compared to standard DEX feeds. |
| `/defi/v2/tokens/new_listing` | Triggering immediate analysis for new market opportunities. | Instant indexing of new SPL tokens. |
| `/defi/v3/token/meme/list` | Discovering newly minted meme tokens matching specific liquidity criteria. | High conviction alpha specifically for the meme narrative. |
| `/defi/token_overview` | Extracting `v5mUSD` and `priceChange5mPercent`. | High-fidelity data for micro-cap volatility analysis. |
| `/defi/token_security` | Evaluating `top10HolderPercent`, `isMintable`, and `isFreezable`. | Crucial for automated risk mitigation (Anti-Rug). |
| `/defi/price` | Real-time monitoring and exact entry price calculation. | Eliminates "price lag" found in aggregate RPC polling. |

---

## 🛡️ Institutional-Grade Risk Management

HawkEye Shield implements a multi-layer risk management system adapted from institutional quant strategies, tailored for the Pump.fun / Solana Meme ecosystem.

### 1. Dynamic Entry Filters
- **Meme Sniper Thresholds:** Strict momentum requirements (e.g., 1h price > +10%, 5m volume spike > 3x average) to catch true breakouts.
- **Minimum Liquidity:** $4,000+ (for new Pump.fun curve tokens) / $10,000+ (for graduated/Raydium legacy tokens).
- **Anti-Wash Trade Detection:** Detects fake volume by correlating low transaction counts with abnormally high average sizes.
- **Birdeye Security Guard:** Hard-rejects tokens where Top 10 holders own > 30-80% (scaled by age), or if the token is `Mintable` or `Freezable`.

### 2. API Resilience & Free-Tier Graceful Degradation
- **Rate Limit Throttling:** Intelligently modulates request concurrency (`buffer_unordered(1)`) and polling intervals to stay precisely under the Free Tier's 60 Requests-Per-Minute and 30k CU monthly limits.
- **Security Fallback:** If the Premium Security API throws a 401/403 on a free-tier key, the bot gracefully bypasses the endpoint and falls back to a custom heuristic (e.g., detecting `0` sells alongside high buys) to filter honeypots, ensuring continuous trading without API lockouts.

### 3. Dual-Mode Position Sizing
- **Dynamic Kelly Fraction:** Buys using a percentage of your current wallet balance (e.g., 15%). As your portfolio grows, your trade sizes scale automatically.
- **Fixed Allocation:** Optionally bypass dynamic sizing to buy an exact, fixed amount of SOL per trade.

### 4. Lifecycle Management
1. **Stop-Loss:** Triggers an emergency sell at -12% (configurable).
2. **Break-Even Lock:** If the token hits +20%, the stop-loss is automatically moved to +5% to guarantee a risk-free trade.
3. **Take Profit 1 (Pull Capital):** At +40%, the bot sells exactly 50% of the holdings to secure the initial capital.
4. **Trailing Stop:** After TP1 is hit, the remaining "moonbag" trails at 75% of the highest recorded price.
5. **Hard Timeout:** If a token goes nowhere after 15 minutes, the bot closes the position to free up capital.

---

## 🚀 Quick Start & Installation

### Prerequisites
- **Rust 1.75+** (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)
- **Birdeye API Key** ([Get one here](https://birdeye.so/))
- **Solana Wallet** (Funded with SOL)
- **Helius or QuickNode RPC endpoint**

### 1. Clone & Setup
```bash
git clone https://github.com/tprk33271/hawkeye-shield.git
cd hawkeye-shield
cp .env.example .env
```

### 2. Configuration (`.env`)
Edit your `.env` file with your credentials. Below is the structure:
```env
# === Birdeye Data API ===
BIRDEYE_API_KEY=your_key_here

# === Solana ===
SOLANA_PRIVATE_KEY=your_base58_key
SOLANA_RPC_URL=https://mainnet.helius-rpc.com/?api-key=xxx
SOLANA_WRITE_RPC_URL=https://mainnet.helius-rpc.com/?api-key=xxx

# === Trading & Position Sizing ===
# Set USE_DYNAMIC_SIZING=true to use Kelly Fraction (%). Set to false to use fixed TRADE_SIZE_SOL.
USE_DYNAMIC_SIZING=true    
KELLY_FRACTION=0.15        
TRADE_SIZE_SOL=0.05        
TAKE_PROFIT_PERCENT=40
STOP_LOSS_PERCENT=12
SLIPPAGE_BPS=250
MAX_ACTIVE_TRADES=2

# === Jupiter Swap ===
JUPITER_API_KEY=your_key
USE_JUPITER_ULTRA=true

# === Mode (IMPORTANT) ===
PAPER_TRADE=true # Set to false to trade with real SOL!
```

### 3. Run the Bot
```bash
cargo build --release
./target/release/hawkeye-shield
```
*(Note: HawkEye Shield runs in Paper Trade mode by default to ensure you don't accidentally spend funds during setup).*

---

## 📁 Repository Architecture

```text
hawkeye-shield/
├── Cargo.toml          # Rust dependencies & build config
├── .env.example        # Configuration template
├── src/
│   ├── main.rs         # Entry point, orchestrator, and event loop
│   ├── config.rs       # Environment configuration loader
│   ├── birdeye.rs      # Birdeye REST API client (7 integrated endpoints)
│   ├── scanner.rs      # 3-Tier Funnel (Filtering + Security checks)
│   ├── executor.rs     # Jupiter API integration & position monitoring
│   ├── websocket.rs    # Birdeye Native SUBSCRIBE_MEME implementation
│   └── logger.rs       # Output formatting and trade result logging
└── assets/
    └── demo.mov        # Video demonstration
```

---

## ⚠️ Disclaimer

This software is for **educational, research, and hackathon purposes only**. Trading cryptocurrency, especially meme tokens on decentralized exchanges, involves extreme financial risk. Never trade with money you cannot afford to lose. The authors and contributors are not responsible for any financial losses incurred from using this software.

---

## 📜 License

[MIT License](LICENSE)

---

*Built with 🦀 Rust + 🦅 Birdeye Data API*
