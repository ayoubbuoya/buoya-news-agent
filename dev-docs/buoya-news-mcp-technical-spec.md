# Technical Specification: buoya-news-mcp (Rust)

**Version:** 2.0
**Based on:** PRD v0.2 (approved, stack revised to Rust)
**Date:** June 2026
**Deployment decision:** Option A, single local process (one binary). Fetch scheduler runs in-process as tokio tasks, default interval 12 hours, fully configurable per source group.

---

## 1. Scope

This spec covers v1: toolchain and dependency versions, project structure, database schema (DDL), Rust domain types, the fetcher trait and all v1 fetchers, dedup and scoring algorithms, MCP tool contracts, configuration schema, error handling, testing strategy, and the sprint plan with task breakdown.

### Design Consequence of the 12-Hour Interval (Unchanged From v1 Spec)

A 12-hour local cron alone cannot satisfy "critical events visible within ≤ 30 min". Mitigations, both configurable:

1. **Lazy staleness refresh:** every tool call checks data age. If older than `staleness.<group>` (defaults: news 6h, security 6h, market 1h), an ingest runs before answering, bounded by `staleness.max_wait_ms` (default 8000). Past that bound the tool answers from cache and sets `"data_freshness": "stale"` in the response.
2. **Per-group intervals:** `news`, `market`, `security` each have their own duration in config (`"12h"`, `"30m"`, etc.). Changing them is a config edit, no code.

The real fix for push-latency on exploits remains the Telegram backlog task (BNM-28).

---

## 2. Toolchain and Dependencies

### 2.1 Versions: What Is Verified vs. Pinned at Init

Verified as of June 10, 2026:

- **Rust stable: 1.96.0** (released May 28, 2026; Rust releases every 6 weeks, so re-check with `rustup update` at project start). **Edition 2024.** Pin `rust-version = "1.96"` in `Cargo.toml`.
- **`rmcp` (official MCP Rust SDK): 1.x line, latest published 1.7.0.** Note the SDK crossed 1.0 with breaking changes from 0.x; any 0.x tutorial or blog post you find is outdated. Use the official repo's migration guide and current examples, not third-party 0.x articles.

For all other crates I am deliberately **not hardcoding patch versions in this document**, because I cannot verify each crate's exact latest release and stale pins would defeat your "latest, not old" requirement. The rule: add every dependency with `cargo add <crate>`, which resolves to the latest compatible release at init time, then commit `Cargo.lock`. Major lines and required features:

| Crate | Features | Purpose |
|---|---|---|
| `rmcp` (1.x) | `server`, `transport-io` (stdio); name per current docs | MCP server, tool macros, stdio transport |
| `tokio` (1.x) | `macros`, `rt-multi-thread`, `time`, `sync` | Async runtime, scheduler timers, channels |
| `reqwest` | `json`, `rustls-tls` (disable default openssl), `gzip` | HTTP client, one shared instance with timeouts |
| `rusqlite` | `bundled` (compiles SQLite in, includes FTS5) | SQLite driver |
| `serde`, `serde_json` | `derive` | All (de)serialization |
| `toml` | — | Config parsing (see §9 for why TOML, not YAML) |
| `schemars` | `derive` | JSON Schema for MCP tool inputs (used by rmcp macros) |
| `feed-rs` | — | RSS + Atom parsing, one parser for all feeds |
| `chrono` | `serde` | Timestamps; matches feed-rs date types |
| `thiserror` | — | Typed errors in library modules |
| `anyhow` | — | Error context at binary boundaries |
| `tracing`, `tracing-subscriber` | `env-filter`, `json` | Structured logs to stderr |
| `sha2`, `hex` | — | Deterministic item ids |
| `humantime` | — | Parse `"12h"` / `"30m"` duration strings in config |
| `quick-xml` | `serialize` | arXiv Atom is handled by feed-rs; quick-xml only if a source needs raw XML. Add only when needed |

Dev-dependencies: `pretty_assertions`, plus fixtures as plain files under `tests/fixtures/` read with `include_str!`. No mocking framework; the architecture makes fetch parsing and pipeline logic pure functions.

Lint policy: `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check` in CI. `unsafe_code = "forbid"` in `Cargo.toml` lints table. No `unwrap()`/`expect()` outside tests and `main()` startup (enforce with `clippy::unwrap_used` at `deny` for non-test code).

### 2.2 SQLite Concurrency Model

`rusqlite` is synchronous. The app is low-throughput, so the model is: a single `Connection` owned by a `Db` struct behind `Arc<Mutex<...>>` (std mutex, not tokio's; calls are short), and every DB call from async context goes through `tokio::task::spawn_blocking`. No connection pool. WAL mode on. This is the simplest correct design for one process; revisit only if profiling ever shows contention (it will not at this scale).

---

## 3. Project Structure

```
buoya-news-mcp/
├── Cargo.toml
├── Cargo.lock                    # committed
├── config.default.toml           # committed defaults
├── config.toml                   # user overrides, gitignored if it holds tokens
├── data/buoya.db                 # gitignored
├── migrations/
│   ├── 001_init.sql
│   └── ...
├── src/
│   ├── main.rs                   # parse args, load config, init tracing(stderr), migrate, spawn scheduler, serve MCP on stdio
│   ├── config.rs                 # serde structs + #[serde(default)] + validate(); humantime durations
│   ├── domain.rs                 # NewsItem, RawItem, Category, Severity, ScoreBreakdown, Signals
│   ├── error.rs                  # AppError (thiserror)
│   ├── db/
│   │   ├── mod.rs                # Db handle, spawn_blocking wrapper fn
│   │   ├── migrate.rs            # versioned, idempotent
│   │   └── repo.rs               # all SQL behind typed fns: upsert_item, find_candidates, top_by_score, fts_search, source_status, app_state, snapshots
│   ├── fetchers/
│   │   ├── mod.rs                # Fetcher trait, registry built from config
│   │   ├── rss.rs                # generic, instantiated per [sources.rss] entry
│   │   ├── hn.rs                 # Algolia API
│   │   ├── defillama.rs
│   │   ├── coingecko.rs
│   │   ├── cryptopanic.rs
│   │   ├── reddit.rs
│   │   ├── arxiv.rs
│   │   └── huggingface.rs
│   ├── pipeline/
│   │   ├── mod.rs                # ingest orchestration per group
│   │   ├── normalize.rs          # canonical url, id, RawItem -> NewsItem shell
│   │   ├── dedup.rs              # pure: assign_group()
│   │   └── score.rs              # pure: score_item() + severity rules
│   ├── scheduler.rs              # per-group loops, circuit breaker, ensure_fresh()
│   └── server/
│       ├── mod.rs                # rmcp ServerHandler impl, tool router
│       └── tools.rs              # 7 tool fns + shared Envelope type
└── tests/
    ├── fixtures/                 # saved real responses per source
    ├── pipeline_dedup.rs
    ├── pipeline_score.rs
    ├── fetcher_parse.rs
    └── repo.rs                   # :memory: db
```

Architecture rule, unchanged: **fetchers never touch the DB; tools never touch the network.** Fetchers parse bytes into `Vec<RawItem>`; the pipeline persists; tools read via `repo` and may request a refresh through the scheduler's `ensure_fresh`.

---

## 4. Database Schema (DDL)

Identical to spec v1 (the schema is stack-independent). `migrations/001_init.sql`:

```sql
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE schema_version (
  version INTEGER PRIMARY KEY,
  applied_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE dedup_groups (
  id TEXT PRIMARY KEY,
  representative_item_id TEXT,
  item_count INTEGER NOT NULL DEFAULT 1,
  first_seen_at TEXT NOT NULL,
  last_seen_at TEXT NOT NULL
);

CREATE TABLE news_items (
  id TEXT PRIMARY KEY,                  -- first 32 hex chars of sha256(canonical_url)
  title TEXT NOT NULL,
  url TEXT NOT NULL,
  canonical_url TEXT NOT NULL,
  source TEXT NOT NULL,
  category TEXT NOT NULL CHECK (category IN ('crypto','ai','security','market')),
  published_at TEXT NOT NULL,           -- ISO 8601 UTC
  fetched_at TEXT NOT NULL,
  severity TEXT NOT NULL DEFAULT 'info' CHECK (severity IN ('info','notable','critical')),
  score REAL NOT NULL DEFAULT 0,
  score_breakdown TEXT NOT NULL DEFAULT '{}',
  raw_signals TEXT NOT NULL DEFAULT '{}',
  dedup_group_id TEXT NOT NULL REFERENCES dedup_groups(id),
  UNIQUE (canonical_url)
);

CREATE INDEX idx_items_published ON news_items (published_at DESC);
CREATE INDEX idx_items_category_published ON news_items (category, published_at DESC);
CREATE INDEX idx_items_severity ON news_items (severity, published_at DESC);
CREATE INDEX idx_items_group ON news_items (dedup_group_id);

CREATE VIRTUAL TABLE news_fts USING fts5(
  title,
  content='news_items',
  content_rowid='rowid'
);
CREATE TRIGGER news_items_ai AFTER INSERT ON news_items BEGIN
  INSERT INTO news_fts(rowid, title) VALUES (new.rowid, new.title);
END;
CREATE TRIGGER news_items_ad AFTER DELETE ON news_items BEGIN
  INSERT INTO news_fts(news_fts, rowid, title) VALUES ('delete', old.rowid, old.title);
END;

CREATE TABLE source_status (
  source TEXT PRIMARY KEY,
  last_success_at TEXT,
  last_error_at TEXT,
  last_error TEXT,
  consecutive_failures INTEGER NOT NULL DEFAULT 0,
  backoff_until TEXT,
  items_last_run INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE app_state (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE market_snapshots (
  coin_id TEXT NOT NULL,
  snapshot_at TEXT NOT NULL,
  price_usd REAL NOT NULL,
  pct_change_24h REAL NOT NULL,
  market_cap_rank INTEGER,
  PRIMARY KEY (coin_id, snapshot_at)
);
```

The `bundled` feature of rusqlite compiles SQLite with FTS5 enabled; verify with a startup assertion that `news_fts` is creatable, failing fast with a clear message otherwise.

Retention after each ingest: items > 90 days deleted, snapshots > 30 days deleted, orphaned groups pruned.

---

## 5. Domain Types (Rust)

```rust
// src/domain.rs
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category { Crypto, Ai, Security, Market }

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity { Info, Notable, Critical }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceGroup { News, Market, Security }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Signals {
    pub hn_points: Option<u32>,
    pub reddit_upvotes: Option<u32>,
    pub cryptopanic_votes: Option<u32>,
    pub loss_usd: Option<f64>,
    pub pct_change_24h: Option<f64>,
    pub market_cap_rank: Option<u32>,
    pub hf_trending_rank: Option<u32>,
}

/// What a fetcher returns. Minimal, source-shaped.
#[derive(Debug, Clone)]
pub struct RawItem {
    pub title: String,
    pub url: String,
    pub source: String,
    pub category: Category,
    pub published_at: DateTime<Utc>,
    pub signals: Signals,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScoreBreakdown {
    pub coverage: f64,
    pub community: f64,
    pub keywords: f64,
    pub impact: f64,
    pub recency: f64,          // decay multiplier applied, 0..=1
    pub watchlist: f64,        // multiplier applied, 1.0 if none
    pub total: f64,
    pub matched_keywords: Vec<String>,
}

/// Persisted, scored item.
#[derive(Debug, Clone)]
pub struct NewsItem {
    pub id: String,
    pub canonical_url: String,
    pub fetched_at: DateTime<Utc>,
    pub severity: Severity,
    pub score: f64,
    pub breakdown: ScoreBreakdown,
    pub dedup_group_id: String,
    pub raw: RawItem,
}
```

The fetcher contract uses `async_trait`-free async traits (native async-in-trait is stable on Rust 1.96; object safety handled via the `dyn`-compatible pattern below):

```rust
// src/fetchers/mod.rs
pub trait Fetcher: Send + Sync {
    fn name(&self) -> &'static str;            // unique; source_status key
    fn group(&self) -> SourceGroup;
    fn fetch<'a>(&'a self, ctx: &'a FetchContext)
        -> Pin<Box<dyn Future<Output = Result<Vec<RawItem>, FetchError>> + Send + 'a>>;
}

pub struct FetchContext {
    pub http: reqwest::Client,     // shared, timeout configured globally
    pub config: Arc<AppConfig>,
}
```

(Boxed futures because the registry holds `Vec<Box<dyn Fetcher>>`; native AFIT is not dyn-compatible. This is the standard pattern; do not fight it.)

Each fetcher is split into `async fn fetch_bytes(...)` (network, thin) and `fn parse(bytes_or_str) -> Result<Vec<RawItem>>` (pure). Tests target `parse` with fixtures.

---

## 6. Pipeline Algorithms

### 6.1 Normalization (`normalize.rs`)

- `canonical_url`: lowercase host, strip `utm_*`, `ref`, `fbclid` query params (use the `url` crate, pulled transitively by reqwest, as a direct dep), strip trailing slash and fragment.
- `id = hex(sha256(canonical_url))[..32]`. Deterministic → re-fetch is an upsert.
- Upsert: on `canonical_url` conflict, update `raw_signals` (merge: take max of each numeric signal) and re-score. `published_at` immutable.

### 6.2 Deduplication (`dedup.rs`)

Pure function: `fn assign_group(item: &NewsItem, candidates: &[NewsItem], cfg: &DedupCfg) -> Option<GroupId>`.

1. Candidates: same `category`, `published_at` within ±48h (indexed repo query feeds this in).
2. Title normalization: lowercase → strip punctuation → split whitespace → drop stopwords (fixed small English list in `domain.rs`) → `HashSet<&str>`.
3. Jaccard = |A∩B| / |A∪B|; threshold 0.6 from config.
4. Best match above threshold → join its group; else new group (UUID v4 via `uuid` crate).
5. Representative = member with highest `source_authority` from config; tie → earliest `published_at`.
6. Group membership change ⇒ re-score all members (coverage changed).

Accepted v1 limitation: paraphrased titles with low token overlap won't merge; coverage scoring degrades gracefully. Backlog BNM-32 covers shingles/embeddings.

### 6.3 Scoring (`score.rs`)

Pure: `fn score_item(item, group, now, cfg) -> (f64, Severity, ScoreBreakdown)`. Weights from `[scoring]` config.

```
base =
    w.coverage  * log2(group.item_count + 1)
  + w.community * norm_community(signals)        // each signal mapped to 0..1 by per-source cap; max taken
  + w.keywords  * Σ matched keyword weights
  + w.impact    * impact(signals)                // loss: min(log10(loss)/9, 1); pct: min(|pct|/50, 1)

score = base * recency_decay * watchlist_multiplier
recency_decay = exp(-ln(2) * age_hours / cfg.half_life_hours)     // default 48h
watchlist_multiplier = cfg.watchlist_multiplier (default 1.5) if watchlist term in title else 1.0
```

**Severity is rule-based; keywords alone never produce `critical`:**

- `critical`: `loss_usd ≥ thresholds.critical_loss_usd` (10_000_000), OR `|pct_change_24h| ≥ thresholds.critical_pct` (15) with `market_cap_rank ≤ 50`, OR `item_count ≥ thresholds.critical_coverage` (4) AND ≥1 severity keyword matched.
- `notable`: one quantified signal at notable tier, or coverage ≥ 2 with keyword match.
- `info`: otherwise.

Default weights: `coverage 3.0, community 2.0, keywords 1.5, impact 4.0`.

### 6.4 Ingest Orchestration (`pipeline/mod.rs`)

Per-group run: for each enabled fetcher → `tokio::time::timeout(cfg.http.timeout, f.fetch(ctx))` → parse → normalize → upsert → dedup → score → persist → update `source_status`. Fetcher failure = log + status row, never aborts the run (`futures::join_all` over per-fetcher `Result`s, or sequential; sequential is fine and gentler on rate limits — choose sequential). After the run: retention prune, set `app_state.last_fetch_<group>`.

---

## 7. Scheduler (`scheduler.rs`)

One tokio task per group:

```rust
loop {
    run_ingest(group).await;                       // logs errors internally
    tokio::time::sleep(cfg.intervals[group]).await; // parsed via humantime, default "12h"
}
```

Plus a command channel for on-demand refresh:

```rust
pub enum Cmd { EnsureFresh { group: SourceGroup, reply: oneshot::Sender<Freshness> } }
pub enum Freshness { Fresh, Refreshed, Stale }
```

`ensure_fresh(group, max_wait)`: if `now - last_fetch(group) <= staleness[group]` → `Fresh`. Else trigger an ingest and `tokio::time::timeout(max_wait, done_rx)`; on timeout return `Stale` (ingest continues in background and completes anyway). A per-group `tokio::sync::Mutex` guard prevents concurrent ingests of the same group (scheduled run + on-demand collision).

**Circuit breaker per source:** after 3 consecutive failures, `backoff_until = now + min(2^failures * 5min, 12h)`; scheduler skips sources still in backoff. State lives in `source_status` so it survives restarts.

Defaults: `staleness.news = "6h"`, `staleness.security = "6h"`, `staleness.market = "1h"`, `staleness.max_wait_ms = 8000`. Every tool response carries `data_freshness` and `data_as_of`.

---

## 8. MCP Tool Specifications

Server: `rmcp` 1.x `ServerHandler` with the tool-router macros; inputs are `serde::Deserialize + schemars::JsonSchema` structs (rmcp derives the JSON Schema shown to the client). Transport: stdio. **Nothing but MCP protocol goes to stdout; all logs to stderr** (tracing-subscriber writer = stderr).

Common envelope serialized as the tool's JSON text content:

```rust
#[derive(Serialize)]
struct Envelope<T: Serialize> {
    data_freshness: Freshness,        // "fresh" | "refreshed" | "stale"
    data_as_of: DateTime<Utc>,
    items: Vec<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    meta: Option<serde_json::Value>,
}
```

Tools (contracts unchanged from spec v1):

**`get_briefing`** — `{ period?: "24h"|"7d"|"since_last" (default "since_last", first-ever call falls back to "24h"), topics?: Category[], limit?: u32 (default 15, max 50) }`. Group representatives only, score desc, each with severity, breakdown, `sources: Vec<String>` of all group members, url. Sets `app_state.last_briefing_at`. `critical` items always included regardless of limit.

**`get_alerts`** — `{ severity?: "critical"|"notable" (default "critical"), since?: RFC3339 (default 7d) }`. Possibly empty; empty sets `meta.message = "no alerts in period"` so quiet ≠ broken (combined with freshness field).

**`search_news`** — `{ query: String, since?: RFC3339, limit?: u32 (default 20) }`. FTS5 `MATCH` with bm25, secondary sort score. No staleness refresh; reports `data_as_of`.

**`get_exploits`** — `{ since?: RFC3339 (default 30d), min_amount_usd?: f64 }`. Sources `defillama`/`rekt` + category `security`; structured `loss_usd` when known.

**`get_market_movers`** — `{ threshold_pct?: f64 (default thresholds.notable_pct = 8) }`. Latest snapshots; watchlist coins always included and flagged `watchlist: true`.

**`get_ai_releases`** — `{ since?: RFC3339 (default 7d), limit?: u32 }`. Category `ai`, release-type sources (HF, blogs, arXiv), score-ranked.

**`get_source_status`** — no input. Dumps `source_status` + per-group last_fetch + current intervals. Must work with everything else broken: direct repo read, no refresh.

---

## 9. Configuration (`config.toml`)

**Format change from spec v1: TOML, not YAML.** Reason: the Rust YAML story is poor (`serde_yaml` is archived/unmaintained; successors are fragmented), while TOML is the ecosystem-native, first-class format. Loading: parse `config.default.toml`, then `config.toml` if present, with overrides applied via serde defaults on every struct field (`#[serde(default = "...")]`), then `validate()` (e.g., thresholds positive, at least one source enabled). Invalid config = exit non-zero with a precise message.

```toml
[intervals]            # humantime strings
news = "12h"
market = "12h"
security = "12h"

[staleness]
news = "6h"
market = "1h"
security = "6h"
max_wait_ms = 8000

[[sources.rss]]
name = "coindesk"
url = "..."
category = "crypto"
authority = 0.8

[[sources.rss]]
name = "rekt"
url = "..."
category = "security"
authority = 0.9
# ... one table per feed; remove to disable

[sources.hn]
enabled = true
min_points = 80
keywords = ["ai", "llm", "gpt", "claude", "model"]

[sources.defillama]
enabled = true

[sources.coingecko]
enabled = true
top_n = 100

[sources.cryptopanic]
enabled = false
api_key = ""

[sources.reddit]
enabled = true
subreddits = ["CryptoCurrency", "MachineLearning", "LocalLLaMA", "ethereum"]
min_upvotes = 200

[sources.arxiv]
enabled = true
categories = ["cs.AI", "cs.LG", "cs.CL"]
max_per_run = 25

[sources.huggingface]
enabled = true

[scoring]
half_life_hours = 48
watchlist_multiplier = 1.5

[scoring.weights]
coverage = 3.0
community = 2.0
keywords = 1.5
impact = 4.0

[[scoring.keywords]]
term = "exploit"
weight = 1.0
# ... drained 1.0, hack 1.0, halted 0.8, outage 0.7, "state of the art" 0.6, sec 0.5, "open source" 0.4

[thresholds]
critical_loss_usd = 10000000
notable_loss_usd = 1000000
critical_pct = 15.0
notable_pct = 8.0
critical_coverage = 4

[dedup]
jaccard_threshold = 0.6
window_hours = 48

watchlist = ["hedera", "hbar", "stellar", "xlm", "ethereum"]
retention_days = 90

[http]
timeout_ms = 15000
user_agent = "buoya-news-mcp/1.0 (personal aggregator)"
```

---

## 10. Error Handling and Logging

- `error.rs`: `FetchError`, `DbError`, `ConfigError` via `thiserror`; `main` and tool boundaries use `anyhow::Context` for readable chains.
- Tool boundary: errors map to MCP error results with a safe message; full chain to stderr at `error` level.
- Panics: `panic = "abort"` is NOT set; a panic in a fetcher task is caught by the scheduler (`JoinHandle` result) and recorded as a source failure. Panics in pure pipeline code are bugs; tests guard them.
- tracing-subscriber: stderr writer, `RUST_LOG` env filter, JSON format optional via env.

## 11. Testing Strategy

- **Pure-function tests (highest value):** `pipeline_score.rs`, `pipeline_dedup.rs`. Required cases: 3-source same story merges; unrelated titles don't; keyword-only never yields critical; $24M loss fixture yields critical end-to-end through severity rules; decay halves at half-life; signal merge takes max.
- **Fetcher parse tests:** `parse()` per fetcher against committed fixtures (`include_str!`). No network in `cargo test`.
- **Repo tests:** `Connection::open_in_memory()`, run migrations, assert upsert semantics and FTS search; assert FTS triggers keep index consistent after delete.
- **Smoke binary:** `cargo run --bin smoke` (or `--features smoke`) does one live ingest of enabled sources and prints a briefing. Manual only.

CI (GitHub Actions): `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`. Cache with `Swatinem/rust-cache`. No deploy stage.

---

## 12. Sprint Plan

Three one-week sprints. Estimates assume occasional (not daily) Rust fluency: borrow-checker friction, rmcp API learning, and async trait patterns are priced in. The same scope was ~46h in TypeScript; in Rust I estimate **≈ 60h total (+30%)**. This is inference from typical TS→Rust ratios for I/O-heavy glue code, not a verified fact; treat Sprint 1 as calibration and re-estimate after it.

Task IDs unchanged (`BNM-x`) for continuity. Dependencies top-to-bottom within a sprint unless noted.

### Sprint 1 — Skeleton to First Briefing (≈ 25h)

Goal: MCP server runs in Claude Desktop via the compiled binary, ingests 4 RSS feeds + HN, answers `get_briefing` and `search_news` with deduped, scored items.

| ID | Task | Description | Acceptance Criteria | Est |
|---|---|---|---|---|
| BNM-1 | Project scaffold | `cargo init`, edition 2024, `rust-version = "1.96"`, deps via `cargo add` (§2.1), clippy/fmt config, lints table (`unsafe_code = "forbid"`, `unwrap_used = "deny"`), CI workflow stub | `cargo clippy -- -D warnings` and `cargo test` green on empty project; Cargo.lock committed | 2h |
| BNM-2 | Config module | §9: serde structs with full defaults, TOML load + override, humantime durations, `validate()` | Missing file uses defaults; bad value exits non-zero with precise message; unit tests for defaults and override precedence | 3h |
| BNM-3 | DB layer | Migration runner (versioned, idempotent), `Db` handle with spawn_blocking pattern (§2.2), repo fns: `upsert_item`, `find_candidates`, `top_by_score`, `fts_search`, `get/set_state`, `source_status` | Repo tests on `:memory:` pass; FTS5 availability asserted at startup; double migration is a no-op | 4h |
| BNM-4 | Normalization | canonical URL rules (`url` crate), sha256 id, upsert signal-merge (max), `published_at` immutable | Unit tests: utm stripped, same URL twice = 1 row with merged signals | 2h |
| BNM-5 | Dedup engine | §6.2 pure fn + repo glue | §11 dedup cases pass; threshold from config | 3h |
| BNM-6 | Scoring engine | §6.3 pure fn, breakdown struct, severity rules | §11 score cases pass; breakdown components sum to total before multipliers, multipliers recorded | 3h |
| BNM-7 | Generic RSS fetcher | feed-rs, one instance per config entry, UTC date handling, 4 feeds configured (coindesk, cointelegraph, rekt, anthropic blog); fetch_bytes/parse split | Fixture parse test per feed; malformed entry skipped with warn, not fatal | 3h |
| BNM-8 | HN fetcher | Algolia search API, keyword + min_points from config, hn_points signal | Fixture test; min_points respected | 1.5h |
| BNM-9 | MCP server + 2 tools | rmcp 1.x ServerHandler, tool router, stdio transport, `get_briefing` (incl. since_last via app_state) and `search_news`; tracing to stderr only | Claude Desktop connects via binary path; briefing shows breakdown + sources; stdout carries only protocol | 3.5h |

Sprint 1 total: 25h, the heaviest and the calibration sprint. If it overruns, BNM-8 slips to Sprint 2 without breaking the goal. Expect BNM-9 to be where rmcp learning cost lands; budget reading the official examples repo before coding it.

### Sprint 2 — Crypto Depth, Scheduler, Alerts (≈ 19h)

Goal: severity tiers end-to-end; exploits and market movers queryable; background scheduler with circuit breaker and staleness refresh.

| ID | Task | Description | Acceptance Criteria | Est |
|---|---|---|---|---|
| BNM-10 | Scheduler | §7: per-group tasks, command channel, `ensure_fresh` with timeout, per-group ingest mutex, last_fetch state | Test config with 5s intervals runs two cycles; `ensure_fresh` triggers when stale and returns `Stale` on timeout while ingest completes in background; no concurrent same-group ingest | 4h |
| BNM-11 | Circuit breaker + source_status | Failure counting, exp backoff capped 12h, skip-on-backoff, `items_last_run`; state persisted | Simulated failing fetcher backs off after 3 failures, recovers on success, survives restart | 2.5h |
| BNM-12 | DeFiLlama hacks fetcher | Free hacks endpoint, `loss_usd` signal, category security | Fixture test; $24M item is `critical` end-to-end | 2h |
| BNM-13 | CoinGecko fetcher | Top-N + watchlist, `market_snapshots` writes, synthesize market items only for moves ≥ notable_pct | Fixture test; 16% top-50 move yields `critical`; snapshots pruned at 30d | 3h |
| BNM-14 | Reddit fetcher | Public JSON endpoints, explicit User-Agent, min_upvotes, per-subreddit config | Fixture test; test asserts UA header set | 2h |
| BNM-15 | `get_alerts` tool | §8 incl. empty-state meta + envelope | Returns only ≥ requested severity; quiet period returns explicit message | 1.5h |
| BNM-16 | `get_exploits` + `get_market_movers` tools | §8; movers read snapshots, watchlist always included | Both callable from Claude Desktop, correct shapes | 2.5h |
| BNM-17 | Retention job | 90d items, 30d snapshots, orphan groups, post-ingest | Test: out-of-window rows gone, FTS index consistent | 1h |

### Sprint 3 — AI Depth, Explainability, Polish (≈ 16h)

Goal: AI sources complete, all 7 tools live, read-state UX, CI green, release build + install docs.

| ID | Task | Description | Acceptance Criteria | Est |
|---|---|---|---|---|
| BNM-18 | arXiv fetcher | Atom via feed-rs against API query per category, max_per_run cap, title only stored | Fixture test; cap respected | 2h |
| BNM-19 | Hugging Face fetcher | Trending models via free Hub API, `hf_trending_rank` signal | Fixture test | 1.5h |
| BNM-20 | Remaining blog feeds | OpenAI/DeepMind/Meta/Mistral RSS in default config with authority weights; verify each URL live once | Each feed parses in smoke run or documented dead | 1h |
| BNM-21 | `get_ai_releases` tool | §8 | Callable, ranked, release-source filtered | 1h |
| BNM-22 | `get_source_status` tool | §8; independent of scheduler/pipeline health | Works with all fetchers disabled | 1h |
| BNM-23 | Watchlist multiplier + breakdown surfacing | Wire watchlist into score; briefing renders matched_keywords + per-component contributions | Watchlist item outranks identical non-watchlist item by exactly the multiplier | 1.5h |
| BNM-24 | since_last read-state | Default period since_last, first call falls back 24h, state written only on success | Two consecutive calls: second returns only newer items | 1h |
| BNM-25 | CryptoPanic fetcher (key-gated) | Behind enabled flag, free-tier key, votes signal | Fixture test; disabled by default; enabled-without-key = clear startup error | 1.5h |
| BNM-26 | CI + smoke + release + README | GH Actions (fmt/clippy/test, rust-cache), smoke binary, `cargo build --release` documented, README: build, Claude Desktop config snippet (binary path), config reference | Fresh clone: `cargo build --release` + README steps produce a working MCP in Claude Desktop | 3h |
| BNM-27 | Tuning pass | 2-day live run, review briefings, adjust default weights/thresholds, CHANGELOG entry | Documented before/after of ≥1 weight change with rationale | 1.5h |

### Backlog (post-v1, unscheduled)

- **BNM-28** Telegram push for `critical` (free Bot API; the real fix for 12h alert latency).
- **BNM-29** Remote fetcher via GitHub Actions + DB snapshot sync (PRD Option B).
- **BNM-30** Config hot-reload (notify crate file watcher).
- **BNM-31** Prebuilt release binaries via GH Actions (cargo-dist) for zero-toolchain install.
- **BNM-32** Dedup v2: bigram shingles or local embeddings for paraphrase merging.

---

## 13. Definition of Done (Project-Level)

v1 is done when: all 7 tools respond correctly from Claude Desktop through the release binary; ≥ 8 sources ingest with independent failure isolation visible in `get_source_status`; dedup/score covered by unit tests; clippy clean with warnings denied; a 2-day live run produces briefings where the top 5 items are subjectively the right top 5 (BNM-27); total recurring cost remains $0.
