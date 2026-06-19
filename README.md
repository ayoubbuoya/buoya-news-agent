# buoya-news-agent

A free, single-binary crypto & AI news agent. It ingests news and market data from free sources into a local SQLite database and lets you talk to an LLM about it — either through a terminal chat UI or an HTTP backend (REST + SSE) for a frontend. Backed by any OpenAI-compatible API (defaults to OpenRouter), with **local** text embeddings for meaning-based search.

> **Status:** v0.1 — early development. Two surfaces work today: the terminal chat UI (`buoya`) and the HTTP backend (`buoya serve`). Background ingestion covers RSS plus three market sources; the LLM tool-use loop, semantic + keyword search over stored articles, and a push-only Telegram connector all work. Some data sources and the MCP endpoint are still on the roadmap (see [Roadmap](#roadmap)).

---

## Why

High-signal information about crypto and AI is scattered across news sites, security feeds, research announcements, Reddit, Hacker News, and company blogs. Checking them all daily is slow and inconsistent, and important events (a major exploit, a coin crash, a significant model release) can be missed for hours.

The goal of `buoya-news-agent` is a single, queryable, prioritized feed of *what actually matters*, driven by an LLM with dedicated news- and market-fetching tools.

**Design constraints:**

- **Zero recurring cost** — only free sources (RSS, public/free-tier APIs) and a **local** embedding model. No paid API keys required beyond an LLM key (an OpenRouter free-tier model works).
- **Information, not advice** — no trading signals or financial advice.
- **Simple deployment** — one compiled binary, no Node runtime, near-zero idle memory.

## What works today

A shared **core** (database, embedder, LLM/HTTP clients, and the background ingest + embedding-backfill tasks) is built once and handed to whichever surface you run. On startup it:

1. Loads a **local embedding model** (BGE-small-en-v1.5; on first run it downloads ~130 MB of weights and caches them on disk).
2. Spawns a **background ingest task** that fetches every enabled source and stores new articles and market snapshots in a local SQLite database (`INSERT OR IGNORE` on the article URL deduplicates). It runs once at startup, then re-ingests on the configured interval (`general.ingest_interval_secs`, default 15 minutes). A one-shot **embedding backfill** indexes any articles stored before semantic search existed.

You then pick a surface:

### `buoya` / `buoya tui` — terminal chat UI (default)

A terminal app built with [ratatui](https://ratatui.rs/). You can:

- hold multiple chat **sessions** in a sidebar, each persisted to SQLite,
- send a message and watch the assistant reply **stream in**, rendered as Markdown,
- watch the assistant **call tools** to look up stored articles and market data, shown inline as it works,
- navigate with the keyboard (see [Keys](#keys)).

The chat runs an **LLM tool-use loop**: the model is given tools to read the ingested data and is steered (by a system prompt) to ground answers in stored articles, citing titles and sources. Tool rounds are resolved transparently — up to 5 per turn — and only the final answer streams to the screen.

### `buoya serve` — HTTP backend (REST + SSE)

An [actix-web](https://actix.rs/) daemon that exposes the same core to a frontend. Plain JSON data routes (no LLM involved) plus a streaming `POST /chat` that drives the agent and relays events as Server-Sent Events. It also starts any enabled push connectors (e.g. Telegram).

| Method & path | What it returns |
|---|---|
| `GET /health` | `{"status":"ok"}` |
| `GET /articles?category=&limit=` | Most recent articles (optionally by category) |
| `GET /articles/search?q=&semantic=&limit=` | Keyword search, or meaning-based vector search when `semantic=true` |
| `GET /articles/{id}` | Full article record, or 404 |
| `GET /market/snapshot` | Latest daily snapshot per market source |
| `POST /chat` | Drives one agent turn; streams SSE events `token` / `tool` / `done` / `error` |
| `* /mcp` | **Stub** — returns 501 until the MCP adapter is mounted (see [Roadmap](#roadmap)) |

```sh
buoya serve --host 127.0.0.1 --port 8080
```

### Tools the model can call

| Tool | What it does |
|---|---|
| `semantic_search` | Meaning-based vector search over stored articles (local embeddings). Best for conceptual/topical questions. |
| `search_articles` | Exact keyword/substring search over article titles, summaries, and content. Best for tickers and proper names. |
| `list_recent_articles` | List the most recent articles, optionally filtered by category |
| `get_article` | Fetch a single article (including full body) by its numeric id |
| `get_market_snapshot` | Latest structured market data: crypto Fear & Greed index, top coins by market cap with 24h moves, and total DeFi TVL by chain |

The tool set is defined once in a registry ([src/core/llm/tools.rs](src/core/llm/tools.rs)); the OpenAI function-calling schema and a neutral, transport-agnostic view (the hook for a future MCP server) are both derived from it.

### Keys

| Key | Action |
|---|---|
| `Tab` | Switch focus between sidebar and input |
| `Enter` (input) | Send message |
| `Alt+Enter` | Insert a newline in the input |
| `Ctrl+N` | New chat session |
| `↑` / `↓` (sidebar) | Move session selection |
| `Enter` (sidebar) | Open selected session |
| `PageUp` / `PageDown` | Scroll chat history |
| `Ctrl+Q` | Quit |

## Data sources

Implemented and ingested today:

- **RSS feeds** — the default config ships with feeds across crypto, DeFi, AI, and security: CoinDesk, Cointelegraph, The Block, Decrypt, Bitcoin Magazine, Bankless, The Defiant, the Ethereum Foundation blog, Stellar, Hedera, the Hugging Face blog, rekt.news, BleepingComputer, and KrebsOnSecurity. Add or remove feeds by editing `[[sources.rss]]` entries in the config.
- **Market data** — [CoinGecko](https://www.coingecko.com/) (top coins by market cap), [DeFiLlama](https://defillama.com/) (TVL by chain), and the [Crypto Fear & Greed Index](https://alternative.me/crypto/fear-and-greed-index/). These produce one daily snapshot per source, surfaced via `get_market_snapshot` and `GET /market/snapshot`.

Configured but **not yet implemented** (toggling them on currently has no effect): Reddit, arXiv, Hugging Face, and CryptoPanic.

> Free tiers and rate limits change over time. Every source is treated as optional and re-verified at implementation time.

## Architecture

- **Language:** Rust (stable, edition 2024, `rust-version = 1.96`). `unsafe` is denied crate-wide (one justified exception: registering the sqlite-vec extension); `unwrap`/`expect` are denied in non-test code.
- **Core boundary:** `src/core` owns all domain logic and knows nothing about its callers. `Core::start(config)` wires everything and spawns the background tasks; `Core::repository()` hands out typed reads; `Core::chat_stream(history)` drives one streamed agent turn. Each surface (TUI, server, connectors) is a thin adapter over this handle. See [docs/architecture-evolution.md](docs/architecture-evolution.md).
- **LLM backend:** any OpenAI-compatible API via [`async-openai`](https://github.com/64bit/async-openai); defaults to OpenRouter.
- **Embeddings & semantic search:** local inference via [`fastembed`](https://github.com/Anush008/fastembed-rs) (BGE-small-en-v1.5, 384-dim), with vectors stored and queried through the [`sqlite-vec`](https://github.com/asg017/sqlite-vec) extension.
- **TUI:** [ratatui](https://ratatui.rs/) + [crossterm](https://github.com/crossterm-rs/crossterm). The TUI owns the terminal, so its logs go to `data/agent.log`; `serve` has no UI and logs to the terminal.
- **HTTP server:** [actix-web](https://actix.rs/) with [actix-web-lab](https://github.com/robjtede/actix-web-lab) for the SSE helper. The shared `Core` lives in `web::Data` so every worker shares one pool, embedder, and client.
- **Connectors:** pure background consumers of the core. They subscribe to an ingest broadcast channel and fan out alerts (lossy under pressure so ingestion never blocks). The first is a push-only [Telegram](docs/telegram-connector-plan.md) connector, started by `serve` when enabled.
- **Storage:** a single SQLite file (via [`sqlx`](https://github.com/launchbadge/sqlx)) holding articles (plus their vectors), market snapshots, and chat sessions/messages (which record the assistant's tool calls alongside message text).
- **Ingestion:** `fetchers` parse source bytes into normalized items, which `ingest` stores and broadcasts to subscribers.

## Build

Requires the Rust toolchain. Install via [rustup](https://rustup.rs/).

```sh
git clone <repo-url>
cd buoya-news-agent
cargo build --release
```

The binary is produced at `target/release/buoya-news-agent` (invoked as `buoya`).

## Configuration

### Environment variables

Set via the environment or a `.env` file (see `.env.example`):

| Variable | Required | Default | Purpose |
|---|---|---|---|
| `AI_API_KEY` | **yes** | — | API key for the OpenAI-compatible LLM backend |
| `DATABASE_URL` | **yes** | — | SQLite connection string, e.g. `sqlite://data/buoya.db` |
| `AI_BASE_URL` | no | `https://openrouter.ai/api/v1` | LLM API base URL |
| `AI_MODEL` | no | `openai/gpt-oss-20b:free` | Model name to request |
| `TELEGRAM_BOT_TOKEN` | no | — | Bot token (from @BotFather) for the Telegram connector. Leave unset to disable. |

```sh
cp .env.example .env
# then edit .env and set AI_API_KEY
```

### Sources, connectors & general settings

Source feeds, market toggles, the watchlist, retention, HTTP settings, and connectors live in `config.default.toml`. Every field has a serde default, and the parser rejects unknown keys.

The **Telegram connector** is opt-in. Enable it under `[connectors.telegram]` with a destination `chat_id`, set `TELEGRAM_BOT_TOKEN` in the environment, and run `buoya serve`. It pushes an alert for each newly-ingested article (optionally filtered by category).

> Note: the binary currently loads `config.default.toml` directly. A `config.toml`-overrides-defaults merge is not wired in yet.

## Run

```sh
# Terminal chat UI (default)
cargo run --release
# or the built binary
./target/release/buoya-news-agent

# HTTP backend (REST + SSE)
cargo run --release -- serve --host 127.0.0.1 --port 8080
```

## Testing

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## Roadmap

- [x] Feed the ingested news into the chat (LLM tool-use loop over the article + market database).
- [x] Semantic search over articles (local embeddings + sqlite-vec).
- [x] Market data fetchers (CoinGecko, DeFiLlama, Fear & Greed) surfaced as a structured snapshot.
- [x] HTTP backend: REST data routes + SSE chat.
- [x] Push notifications via connectors (Telegram).
- [ ] Implement the remaining source fetchers (Reddit, arXiv, Hugging Face, CryptoPanic).
- [ ] Mount the MCP adapter at `/mcp` so other models can call the same tools.
- [ ] Importance scoring and cross-source deduplication.
- [ ] Retention enforcement (`general.retention_days`).
- [ ] `config.toml` overrides merged on top of `config.default.toml`.

## License

TBD.
