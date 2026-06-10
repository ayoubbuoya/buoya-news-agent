# PRD: buoya-news-mcp

**Version:** 0.1
**Author:** Ayoub
**Date:** June 2026
**Status:** Proposal

---

## 1. Problem Statement

As a developer working across backend, blockchain, and AI projects, staying current requires checking many sources daily: crypto news sites, security feeds, AI research announcements, Reddit, Hacker News, and Twitter/X. This is time-expensive and inconsistent. Important events (a major exploit, a coin crash, a significant AI model release) can be missed for hours or days, while low-value noise consumes the limited reading time available.

The core problem: **high-signal information is scattered across many free sources, and there is no single, queryable, prioritized feed of "what actually matters" in crypto and AI.**

## 2. Goal

Build an MCP (Model Context Protocol) server, `buoya-news-mcp`, that aggregates news from multiple free sources about crypto and AI, scores items by importance, deduplicates them, and exposes them as MCP tools. The user connects the MCP to Claude (or any MCP client) and asks natural-language questions like "what happened in crypto today?" or "any exploits this week?" and gets a ranked, deduplicated answer in seconds.

### Non-Goals (v1)

- No paid APIs, no API keys with paid tiers as a requirement. Free tiers with generous limits are acceptable.
- No trading signals or financial advice. This is an information tool, not an alpha tool.
- No web UI. The MCP client (Claude Desktop, Claude Code, etc.) is the interface.
- No real-time streaming (websockets). Polling on a schedule is sufficient for the use case.

## 3. Target User

Primary user: a busy developer (initially just the author) who wants a 5-minute daily briefing instead of 45 minutes of feed-scrolling, plus the ability to ask ad-hoc questions about recent events. Secondary users later: other developers who install the MCP from npm or GitHub.

## 4. Data Sources (All Free)

The selection principle: prefer structured APIs and RSS over scraping, because scraping breaks and may violate ToS. Every source below is free with no payment required. Note: free tiers and rate limits change; each must be re-verified at implementation time.

| Category | Source | Access Method | What It Provides |
|---|---|---|---|
| Crypto news | CoinDesk, Cointelegraph, The Block, Decrypt | RSS feeds | General crypto news headlines |
| Crypto news aggregation | CryptoPanic | Free API (key required, free tier) | Aggregated + community-voted news, has "important" filter |
| Exploits / hacks | rekt.news | RSS | Post-mortems of DeFi exploits |
| Exploits / hacks | DeFiLlama Hacks | Public API (free, no key) | Structured hack data: protocol, amount lost, technique |
| Market crashes / moves | CoinGecko | Free API (no key for basic endpoints) | Prices, 24h change, market cap. Detect crashes via threshold |
| Market sentiment | alternative.me Fear & Greed Index | Free API | Single sentiment number, useful context for "crash" detection |
| AI news | Hacker News | Algolia HN Search API (free, no key) | Top stories filtered by AI keywords, points as quality signal |
| AI research | arXiv | Public API / RSS (cs.AI, cs.LG, cs.CL) | New papers. Filter by category, not full firehose |
| AI releases | Hugging Face | RSS / free Hub API | Trending models, new releases |
| AI / general tech | Company blogs (OpenAI, Anthropic, Google DeepMind, Meta AI, Mistral) | RSS | First-party release announcements |
| Community signal | Reddit (r/CryptoCurrency, r/MachineLearning, r/LocalLLaMA, r/ethereum) | Public JSON endpoints (`/.json`, free, rate-limited) | Community-surfaced events, upvotes as signal |
| Dev signal | GitHub Trending | Unofficial endpoints / scraping fallback | New AI tools and libraries gaining traction |

I cannot confirm the exact current rate limits of each free tier; these change. The implementation must treat every source as optional and degrade gracefully when one is down or rate-limited.

## 5. Functional Requirements

### 5.1 MCP Tools Exposed

The server exposes these tools to the MCP client:

**`get_briefing(period, topics?)`** — The flagship tool. Returns a ranked digest of the most important items in the last `period` (e.g., `24h`, `7d`), optionally filtered by topic (`crypto`, `ai`, `security`). This is the "what did I miss?" tool and should be the best-polished one.

**`get_alerts(severity?)`** — Returns only high-severity events: exploits, crashes beyond a threshold, major protocol failures, significant model releases. Designed to answer "did anything bad/big happen?" with a near-empty response on quiet days.

**`search_news(query, since?)`** — Full-text search over the cached corpus. "What happened with Hedera this month?"

**`get_exploits(since?, min_amount_usd?)`** — Structured exploit data from DeFiLlama + rekt, with protocol name, loss amount, and technique when available.

**`get_market_movers(threshold_pct?)`** — Coins (from a configurable watchlist plus top-N by market cap) that moved more than `threshold_pct` in 24h, in either direction.

**`get_ai_releases(since?)`** — New models, papers, and tools, ranked by signal (HN points, HF trending position, source authority).

**`get_source_status()`** — Health of each upstream source (last successful fetch, error count). Important for trusting the data: silence from the tool should be distinguishable from a broken fetcher.

### 5.2 Ingestion Pipeline

A background fetcher runs on a schedule (configurable, default every 30 minutes; market data every 10 minutes). For each source it fetches new items, normalizes them into a common `NewsItem` schema, deduplicates, scores, and stores. Failures on one source must never block others.

Normalized schema (core fields): `id`, `title`, `url`, `source`, `category` (crypto | ai | security | market), `published_at`, `fetched_at`, `score`, `severity` (info | notable | critical), `raw_signals` (points, upvotes, votes, loss amount), `dedup_group_id`.

### 5.3 Deduplication

The same story appears on five sites within hours. Without dedup the briefing is unusable. v1 approach (no paid embeddings): normalize titles (lowercase, strip punctuation and stopwords), then compare with token-set similarity (Jaccard) plus URL canonicalization within a 48-hour window. Items above a similarity threshold join a dedup group; the group is represented by the highest-authority source, and the duplicate count itself becomes a signal (a story covered by 5 sources is probably important).

### 5.4 Importance Scoring

A deterministic heuristic, not an LLM, so it is free and fast. Components, roughly weighted:

1. **Cross-source coverage:** number of sources in the dedup group. Strongest signal.
2. **Community signals:** HN points, Reddit upvotes, CryptoPanic votes, normalized per source.
3. **Keyword severity:** terms like "exploit", "hack", "drained", "halted", "SEC", "outage", "state of the art", "open-source release" bump score; keep the keyword list in config, not code.
4. **Quantified impact:** exploit loss amount in USD (from DeFiLlama), price move percentage. These map directly to severity tiers (e.g., loss > $10M or top-50 coin moving > 15% in 24h → `critical`).
5. **Recency decay:** exponential decay so the briefing favors fresh items.

Calibration of weights is expected to be iterative. The scoring function must be a pure function over the item + group so it can be unit-tested and re-run when weights change.

### 5.5 Storage

SQLite, single file. Rationale: zero infrastructure cost, trivially portable, full-text search via FTS5 covers `search_news` without adding a search engine. PostgreSQL would be familiar but is overkill for a single-user local tool and violates the "free + simple" constraint when hosted. Retention: 90 days of items, then pruned.

### 5.6 Configuration

A single `config.yaml` (or `.json`): enabled sources, fetch intervals, watchlist coins, score weights, severity thresholds, keyword lists. No code changes needed to tune behavior.

## 6. Architecture

**Language/runtime:** Rust (latest stable toolchain, edition 2024) with the official `rmcp` SDK (modelcontextprotocol/rust-sdk). Decision rationale: single static binary with no Node runtime dependency, near-zero idle memory for an always-running local daemon, and it builds on existing Rust experience. Tradeoffs accepted: development will be slower than the TypeScript path estimated in v0.1 of this PRD, and the `rmcp` ecosystem is younger than the TS SDK. Both are acceptable for a personal tool.

**Process model, two options:**

**Option A (recommended for v1): single process.** The MCP server runs locally (stdio transport for Claude Desktop / Claude Code) and runs the fetch scheduler in-process as tokio background tasks. Fetches happen lazily too: if a tool is called and data is stale beyond a threshold, fetch before answering. Simplest possible deployment: one compiled binary, `buoya-news-mcp`, pointed to from the MCP client config.

**Option B: split fetcher.** A GitHub Actions workflow on a cron schedule (free for public repos) runs the fetcher and commits the SQLite DB (or a JSON snapshot) to the repo or pushes to a free object store; the local MCP server only reads. This gives "always fresh even when my machine was off" at zero cost, at the price of more moving parts. Defer to v1.1; design the fetcher module so it can run standalone from day one.

**Failure handling:** per-source circuit breaker (after N consecutive failures, back off exponentially), per-source timeout, and the `get_source_status` tool for visibility. Respect upstream rate limits with a per-source token bucket and proper `User-Agent` headers (Reddit in particular requires this).

## 7. Suggested Improvements Beyond the Original Idea

These were not in the original request but materially increase value at zero cost:

**7.1 Severity-tiered alerts instead of one flat feed.** Separating `critical` (exploit, crash, major outage) from `notable` from `info` is what makes a 5-minute briefing possible. Most aggregators fail because everything has equal weight.

**7.2 Telegram push for critical events (optional module).** Telegram bots are completely free. When an item crosses the `critical` threshold, the fetcher pushes a message. This converts the tool from pull-only ("ask Claude") to push for the rare events where hours matter (an exploit on a chain you build on). Off by default, enabled via config with a bot token.

**7.3 Watchlist-aware scoring.** Items mentioning watchlist entries (e.g., Hedera, Stellar, specific protocols) get a score multiplier. Personalization with zero ML.

**7.4 "Why is this ranked here" explainability.** Each item in a briefing carries its score breakdown (sources: 4, HN points: 230, keyword: exploit). This builds trust in the ranking and makes tuning weights much easier.

**7.5 Read-state tracking.** The server remembers the timestamp of the last briefing served, so `get_briefing` can default to "since you last asked". This directly serves the "I don't have time" requirement: no re-reading.

**7.6 Weekly digest tool.** `get_briefing(period: "7d")` with stricter score cutoff, for catching up after being heads-down on a sprint.

**7.7 Source authority weights in config.** First-party blogs (Anthropic, OpenAI) and structured data (DeFiLlama) outrank aggregators for the same story. Prevents low-quality sources from dominating via volume.

## 8. Explicitly Rejected Approaches

- **Twitter/X as a source:** the free API tier is effectively unusable for read access, and scraping violates ToS. Reddit + HN + CryptoPanic capture most of the same signal with lag measured in minutes.
- **LLM-based summarization inside the fetcher:** costs money or local compute, and the MCP client (Claude) already summarizes at query time for free from the structured data. The server's job is collection, dedup, and ranking, not prose.
- **Full web scraping framework:** brittle and a maintenance tax. RSS + JSON APIs only; if a source has neither, drop it.

## 9. Milestones

**M1 — Core pipeline (week 1):** NewsItem schema, SQLite + FTS5, RSS fetchers (3–4 sources), HN Algolia fetcher, basic dedup, `get_briefing` and `search_news` tools, stdio MCP server runnable in Claude Desktop.

**M2 — Crypto depth (week 2):** DeFiLlama hacks, CoinGecko market movers, CryptoPanic, severity tiers, `get_alerts`, `get_exploits`, `get_market_movers`.

**M3 — AI depth + polish (week 3):** arXiv, Hugging Face, company blog RSS, `get_ai_releases`, scoring explainability, `get_source_status`, config file, read-state tracking.

**M4 — Optional (later):** Telegram push, GitHub Actions remote fetcher, npm publication, watchlist multipliers.

## 10. Success Metrics

- Daily catch-up time drops from ~30–45 minutes of manual checking to under 5 minutes via one `get_briefing` call.
- Critical events (exploit > $10M, top-50 coin > 15% move, major model release) appear in the feed within one fetch interval (≤ 30 min) of being published by any source.
- Duplicate rate in a briefing: < 5% of items are the same story twice.
- Zero recurring cost.

## 11. Risks

- **Free tier changes:** any source can change its rate limits or shut off free access. Mitigation: every source is optional and config-toggled; minimum 3 sources per category.
- **Scoring miscalibration:** the briefing surfaces noise or buries signal early on. Mitigation: explainable scores (7.4) and config-tunable weights make iteration cheap.
- **Reddit/unofficial endpoint fragility:** these are the most likely to break. Mitigation: treat as supplementary signal, never as the only source for a category.
- **Keyword false positives:** "crash" appears in unrelated contexts. Mitigation: keywords adjust score, they never alone produce `critical`; critical requires a quantified signal or multi-source coverage.
