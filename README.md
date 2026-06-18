# buoya-news-agent

A free, single-binary AI agent that aggregates crypto and AI news from many free sources, deduplicates and scores it by importance, and lets you query it in natural language via an OpenAI-compatible API (e.g. OpenRouter).

Ask `"what happened in crypto today?"` or `"any exploits this week?"` and get a ranked, deduplicated answer in seconds — a 5-minute daily briefing instead of 45 minutes of feed-scrolling.

> **Status:** v0.1 — in development.

---

## Why

High-signal information about crypto and AI is scattered across crypto news sites, security feeds, AI research announcements, Reddit, Hacker News, and company blogs. Checking them all daily is slow and inconsistent, and important events (a major exploit, a coin crash, a significant model release) can be missed for hours.

`buoya-news-agent` is a single, queryable, prioritized feed of *what actually matters*, driven by an LLM agent with dedicated news-fetching tools.

**Design constraints:**

- **Zero recurring cost** — only free sources (RSS, public/free-tier APIs). No paid API keys required beyond an OpenRouter key.
- **Information, not advice** — no trading signals or financial advice.
- **Simple deployment** — one compiled binary, no Node runtime, near-zero idle memory.

## How it works

The agent runs a tool-use loop against any OpenAI-compatible API. When you ask a question, the LLM decides which fetching tools to call, the agent executes them, and the results are returned as a structured answer.

Sources are configured via `config.toml` — toggle them on/off, set watchlist coins, adjust intervals — no code changes needed.

## Data Sources (all free)

| Category | Sources |
|---|---|
| Crypto news | CoinDesk, Cointelegraph, The Block, Decrypt (RSS), CryptoPanic (free API) |
| Exploits / hacks | rekt.news (RSS), DeFiLlama Hacks (public API) |
| Market | CoinGecko (prices, 24h change), alternative.me Fear & Greed Index |
| AI news & research | Hacker News (Algolia API), arXiv (cs.AI/cs.LG/cs.CL), Hugging Face |
| AI releases | Company blogs (OpenAI, Anthropic, Google DeepMind, Meta AI, Mistral) |
| Community signal | Reddit (r/CryptoCurrency, r/MachineLearning, r/LocalLLaMA, r/ethereum) |

> Free tiers and rate limits change over time. Every source is treated as optional and re-verified at implementation time.

## Architecture

- **Language:** Rust (stable 1.96+, edition 2024).
- **LLM backend:** any OpenAI-compatible API via [`async-openai`](https://github.com/64bit/async-openai); defaults to OpenRouter.
- **Agent loop:** the binary runs a tool-use loop — LLM emits tool calls, agent executes fetchers, results are fed back until the LLM produces a final answer.
- **Storage:** a single SQLite file with FTS5 for full-text search. 90-day retention. Zero infrastructure.
- **Pipeline:** fetchers parse bytes into a normalized `NewsItem` schema → deduplicate → score → store.

## Build

Requires the Rust toolchain (1.96+). Install via [rustup](https://rustup.rs/).

```sh
git clone <repo-url>
cd buoya-news-agent
cargo build --release
```

The binary is produced at `target/release/buoya-news-agent`.

## Configuration

Configuration lives in `config.toml` (overriding committed defaults in `config.default.toml`). Every field has a default, so a missing file still runs.

Set your OpenRouter API key via environment variable:

```sh
export OPENROUTER_API_KEY=sk-or-...
```

## Testing

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## License

TBD.
