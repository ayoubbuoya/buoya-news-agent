# buoya-news-agent

A free, single-binary crypto & AI news agent. It ingests news from free sources into a local SQLite database and gives you a terminal chat UI to talk to an LLM about it — backed by any OpenAI-compatible API (defaults to OpenRouter).

> **Status:** v0.1 — early development. The terminal chat UI, multi-session persistence, and background RSS ingestion work today. The LLM-driven tool-use loop and most data sources are still on the roadmap (see [Roadmap](#roadmap)).

---

## Why

High-signal information about crypto and AI is scattered across news sites, security feeds, research announcements, Reddit, Hacker News, and company blogs. Checking them all daily is slow and inconsistent, and important events (a major exploit, a coin crash, a significant model release) can be missed for hours.

The goal of `buoya-news-agent` is a single, queryable, prioritized feed of *what actually matters*, driven by an LLM with dedicated news-fetching tools.

**Design constraints:**

- **Zero recurring cost** — only free sources (RSS, public/free-tier APIs). No paid API keys required beyond an LLM key (an OpenRouter free-tier model works).
- **Information, not advice** — no trading signals or financial advice.
- **Simple deployment** — one compiled binary, no Node runtime, near-zero idle memory.

## What works today

When you launch the binary:

1. A **background task** fetches every configured RSS feed and stores new articles in a local SQLite database (`INSERT OR IGNORE` on the article URL deduplicates).
2. A **terminal chat UI** (built with [ratatui](https://ratatui.rs/)) opens immediately. You can:
   - hold multiple chat **sessions** in a sidebar, each persisted to SQLite,
   - send a message and watch the assistant reply **stream in**, rendered as Markdown,
   - navigate with the keyboard (see [Keys](#keys)).

> ⚠️ The chat currently talks to the LLM as a **plain chatbot** — it does *not* yet feed the ingested news into the conversation or let the model call fetching tools. Wiring the news database into the agent loop is the next major step.

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

Currently only **RSS feeds** are fetched. The default config ships with CoinDesk, Cointelegraph, and rekt.news. Add or remove feeds by editing `[[sources.rss]]` entries in the config.

Other sources (DeFiLlama, CoinGecko, CryptoPanic, Reddit, arXiv, Hugging Face) exist as configuration structs but their fetchers are **not yet implemented** — toggling them on currently has no effect.

> Free tiers and rate limits change over time. Every source is treated as optional and re-verified at implementation time.

## Architecture

- **Language:** Rust (stable, edition 2024, `rust-version = 1.96`). `unsafe` is forbidden; `unwrap`/`expect` are denied in non-test code.
- **LLM backend:** any OpenAI-compatible API via [`async-openai`](https://github.com/64bit/async-openai); defaults to OpenRouter.
- **UI:** terminal app via [ratatui](https://ratatui.rs/) + [crossterm](https://github.com/crossterm-rs/crossterm). Logs go to `data/agent.log` so they don't corrupt the rendered screen.
- **Storage:** a single SQLite file (via [`sqlx`](https://github.com/launchbadge/sqlx)) holding `articles`, `chat_sessions`, and `chat_messages`.
- **Ingestion:** `fetchers` parse feed bytes into a normalized `RawItem`, which `ingest` stores into the `articles` table.

## Build

Requires the Rust toolchain. Install via [rustup](https://rustup.rs/).

```sh
git clone <repo-url>
cd buoya-news-agent
cargo build --release
```

The binary is produced at `target/release/buoya-news-agent`.

## Configuration

### Environment variables

Set via the environment or a `.env` file (see `.env.example`):

| Variable | Required | Default | Purpose |
|---|---|---|---|
| `AI_API_KEY` | **yes** | — | API key for the OpenAI-compatible LLM backend |
| `DATABASE_URL` | **yes** | — | SQLite connection string, e.g. `sqlite://data/buoya.db` |
| `AI_BASE_URL` | no | `https://openrouter.ai/api/v1` | LLM API base URL |
| `AI_MODEL` | no | `openai/gpt-oss-20b:free` | Model name to request |

```sh
cp .env.example .env
# then edit .env and set AI_API_KEY
```

### Sources & general settings

Source feeds, watchlist coins, retention, and HTTP settings live in `config.default.toml`. Every field has a serde default, and the parser rejects unknown keys.

> Note: the binary currently loads `config.default.toml` directly. The `config.toml`-overrides-defaults merge described in earlier drafts is not implemented yet.

## Run

```sh
cargo run --release
# or run the built binary
./target/release/buoya-news-agent
```

## Testing

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## Roadmap

- [ ] Feed the ingested news into the chat (retrieval/context injection).
- [ ] LLM tool-use loop so the model can call fetching/search tools on demand.
- [ ] Implement the remaining source fetchers (DeFiLlama, CoinGecko, CryptoPanic, Reddit, arXiv, Hugging Face).
- [ ] Full-text search over articles (e.g. SQLite FTS5).
- [ ] Importance scoring and cross-source deduplication.
- [ ] Retention enforcement (`general.retention_days`).
- [ ] `config.toml` overrides merged on top of `config.default.toml`.

## License

TBD.
