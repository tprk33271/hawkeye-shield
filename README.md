# 🦅 HawkEye Shield — Autonomous Solana Trading Agent

> **A high-performance, autonomous meme token sniper built in Rust, powered by [Birdeye Data API](https://birdeye.so/) for real-time market intelligence and risk assessment on Solana.**

Built for the **Birdeye Data Services BIP Competition (Sprint 2)**.

---

## 🎯 What It Does

HawkEye Shield is an **execution-layer trading bot** that autonomously discovers, evaluates, buys, and sells meme tokens on Solana — all powered by Birdeye's data infrastructure.

**It is NOT a dashboard or read-only tool.** It makes real trades with real SOL.

### Core Loop (every 8 seconds)

```
Birdeye Trending API → Filter (Liquidity + Volume + Safety) → Jupiter Buy → Monitor → Auto-Sell
```

### Key Features

| Feature | Description |
|---|---|
| 🔍 **Birdeye-Powered Scanner** | Scans trending tokens + new listings via Birdeye API every 8s |
| 🛡️ **Birdeye Security Check** | Anti-rugpull: checks Top 10 holders, mintable, freezable via Birdeye Token Security |
| ⚡ **WebSocket Discovery** | Real-time Raydium + PumpFun new pair detection via Solana RPC WebSocket |
| 💰 **Jupiter Execution** | Buys/sells via Jupiter V6 or Ultra API with priority fees |
| 📊 **Smart Risk Management** | Stop-Loss, Break-Even Lock, TP1 (sell 50% to pull capital), Trailing Stop |
| 📝 **Paper Trade Mode** | Safe testing mode — simulates trades without spending SOL |
| 🎯 **Accurate Entry Price** | Calculates from on-chain token balance diff + Birdeye SOL price (not API estimates) |

---

## 🏗️ Architecture

```
┌─────────────────────────────────────────────────────┐
│                   HawkEye Shield                     │
│                  (Rust / Tokio)                       │
├─────────────────────┬───────────────────────────────┤
│                     │                               │
│  ┌─────────────┐    │    ┌──────────────────┐       │
│  │  WebSocket   │────┼───▶│    Scanner       │       │
│  │  (Raydium +  │    │    │  (Filter Logic)  │       │
│  │   PumpFun)   │    │    └────────┬─────────┘       │
│  └─────────────┘    │             │                  │
│                     │             ▼                  │
│  ┌─────────────┐    │    ┌──────────────────┐       │
│  │  Birdeye    │────┼───▶│   Executor       │       │
│  │  API Client │    │    │  (Jupiter Swap)   │       │
│  │  - Trending │    │    │  - Buy / Sell     │       │
│  │  - Price    │    │    │  - TP / SL        │       │
│  │  - Security │    │    │  - Trailing Stop  │       │
│  │  - Overview │    │    └──────────────────┘       │
│  └─────────────┘    │                               │
└─────────────────────┴───────────────────────────────┘
```

---

## 🔌 Birdeye API Usage

HawkEye Shield uses **6 Birdeye endpoints** as its primary data source:

| Endpoint | Purpose | Frequency |
|---|---|---|
| `GET /defi/token_trending` | Discover trending tokens | Every 8s |
| `GET /defi/v3/token/new_listing` | Discover new listings | Every 8s |
| `GET /defi/token_overview` | Liquidity, Volume, Buy/Sell ratio | Per candidate |
| `GET /defi/token_security` | Anti-rugpull safety check | Per candidate |
| `GET /defi/price` | Real-time price for monitoring | Every 2s per active trade |
| `GET /defi/price` (SOL) | SOL/USD for entry price calculation | Per buy |

**Estimated API calls per hour:** 450+ calls/hour at steady state (scanning + monitoring).

---

## 🚀 Quick Start

### Prerequisites

- Rust 1.75+ (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)
- Birdeye API Key ([get one here](https://birdeye.so/))
- Solana wallet with SOL
- Helius or QuickNode RPC endpoint

### Setup

```bash
git clone https://github.com/YOUR_USERNAME/hawkeye-shield.git
cd hawkeye-shield

# Configure
cp .env.example .env
# Edit .env with your keys

# Build
cargo build --release

# Run (Paper Trade mode by default)
./target/release/hawkeye-shield
```

### Configuration (.env)

```env
# Birdeye API
BIRDEYE_API_KEY=your_key_here

# Solana
SOLANA_PRIVATE_KEY=your_base58_key
SOLANA_RPC_URL=https://mainnet.helius-rpc.com/?api-key=xxx
SOLANA_WRITE_RPC_URL=https://mainnet.helius-rpc.com/?api-key=xxx

# Trading
TRADE_SIZE_SOL=0.05
TAKE_PROFIT_PERCENT=40
STOP_LOSS_PERCENT=12
SLIPPAGE_BPS=250
MAX_ACTIVE_TRADES=2

# Jupiter
JUPITER_API_KEY=your_key
USE_JUPITER_ULTRA=true

# Mode (IMPORTANT: set to false for live trading)
PAPER_TRADE=true
```

---

## 🛡️ Risk Management

HawkEye Shield implements a multi-layer risk management system:

### Entry Filters
- **Minimum Liquidity:** $10,000+ (new tokens) / $12,000+ (legacy)
- **Buy/Sell Ratio:** Buys must be 1.5x Sells in last 5 minutes
- **Anti-Wash Trade:** Detects fake volume (low buys + high avg size)
- **Birdeye Security:** Top 10 holders < 30-40%, not mintable, not freezable

### Position Management
1. **Stop-Loss:** -12% (configurable)
2. **Break-Even Lock:** At +20%, move stop to +5%
3. **TP1 (Pull Capital):** At +40%, sell 50% to secure capital
4. **Trailing Stop:** After TP1, trail at 75% of highest price
5. **Hard Stop:** -50% emergency exit
6. **Timeout:** 15-minute max hold

---

## 📁 Project Structure

```
hawkeye-shield/
├── Cargo.toml          # Dependencies
├── .env.example        # Configuration template
├── src/
│   ├── main.rs         # Entry point + event loop
│   ├── config.rs       # Environment config loader
│   ├── birdeye.rs      # Birdeye API client (6 endpoints)
│   ├── scanner.rs      # Token discovery + filtering
│   ├── executor.rs     # Jupiter buy/sell + position monitor
│   ├── websocket.rs    # Raydium/PumpFun new pair detection
│   └── logger.rs       # Trade result logging
└── README.md
```

---

## 🏆 Why This Project

Most BIP submissions are **read-only dashboards** or **MCP wrappers**. HawkEye Shield is different:

- **Execution Layer:** It makes real trades, not just displays data
- **Birdeye as the Brain:** Every decision (discover → filter → price → safety → monitor) flows through Birdeye API
- **Rust Performance:** Sub-millisecond event processing, compiled binary, no runtime overhead
- **Battle-Tested Logic:** Risk management system ported from a proven Node.js trading bot

---

## ⚠️ Disclaimer

This software is for **educational and research purposes**. Trading meme tokens is extremely risky. Never trade with money you can't afford to lose. The authors are not responsible for any financial losses.

---

## 📜 License

MIT

---

Built with 🦀 Rust + 🦅 Birdeye Data API
