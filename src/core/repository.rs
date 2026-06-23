//! Typed, transport-agnostic data access over the article store.
//!
//! These methods are the news-domain API: they take and return Rust types, not
//! JSON, and know nothing about LLMs or HTTP. Every adapter — the agent's tools,
//! the HTTP server, the MCP server — reads through this layer, so the query logic
//! lives in exactly one place.

use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool};
use utoipa::ToSchema;

use crate::core::embeddings::{self, Embedder};

/// The market-snapshot sources, one row written per day by each fetcher, so the
/// newest row is today's reading.
const SNAPSHOT_SOURCES: [&str; 3] = ["fear-greed", "coingecko", "defillama"];

/// Compact article projection used by search/list results: enough to cite or to
/// decide whether to fetch the full article, without the heavy `content` column.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ArticleSummary {
    pub id: i64,
    pub title: String,
    pub url: String,
    pub source: String,
    pub category: String,
    pub summary: Option<String>,
    pub published_at: String,
    /// Vector distance to the query; present only for semantic search results.
    /// Lower means more relevant. Omitted from output when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance: Option<f64>,
}

/// A full stored article, including the body `content`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Article {
    pub id: i64,
    pub title: String,
    pub url: String,
    pub source: String,
    pub category: String,
    pub summary: Option<String>,
    pub content: Option<String>,
    pub published_at: String,
}

/// One daily market-snapshot record (Fear & Greed, market overview, or DeFi TVL).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SnapshotItem {
    pub id: i64,
    pub title: String,
    pub source: String,
    pub category: String,
    pub summary: Option<String>,
    pub content: Option<String>,
    pub published_at: String,
}

/// One structured crypto-futures derivatives reading for a symbol (open interest,
/// funding rate, long/short ratio, …) as stored in the `derivatives` table.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DerivativesRow {
    pub id: i64,
    pub symbol: String,
    pub open_interest: Option<f64>,
    pub open_interest_usd: Option<f64>,
    pub funding_rate: Option<f64>,
    pub mark_price: Option<f64>,
    pub long_short_ratio: Option<f64>,
    pub long_account: Option<f64>,
    pub short_account: Option<f64>,
    pub taker_buy_sell_ratio: Option<f64>,
    pub taker_buy_vol: Option<f64>,
    pub taker_sell_vol: Option<f64>,
    pub top_trader_long_short_ratio: Option<f64>,
    pub top_trader_long_account: Option<f64>,
    pub top_trader_short_account: Option<f64>,
    pub next_funding_time: Option<String>,
    pub fetched_at: String,
    /// Percent change in USD open interest vs ~24h ago. Computed (not stored) and
    /// only populated for the latest-per-symbol view when a ~24h-old reading
    /// exists; omitted otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_interest_usd_change_24h_pct: Option<f64>,
    /// Percent change in mark price vs ~24h ago. Same provenance/conditions as
    /// [`Self::open_interest_usd_change_24h_pct`]; pairs with it for the
    /// open-interest-vs-price read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mark_price_change_24h_pct: Option<f64>,
}

/// Read access to the article store. Bundles the connection pool and embedder a
/// query needs; cheap to construct (both fields are reference-counted handles).
/// Build one via [`crate::core::Core::repository`].
#[derive(Clone)]
pub struct Repository {
    pool: SqlitePool,
    embedder: Arc<Embedder>,
}

impl Repository {
    pub fn new(pool: SqlitePool, embedder: Arc<Embedder>) -> Self {
        Self { pool, embedder }
    }

    /// Semantic (meaning-based) search via vector similarity. Embeds `query`, then
    /// runs a KNN lookup against `vec_articles`, returning the `limit` closest
    /// articles ordered by ascending distance (most relevant first).
    pub async fn search_semantic(&self, query: &str, limit: i64) -> Result<Vec<ArticleSummary>> {
        // Embed the query off the async runtime (CPU-bound).
        let embedder = self.embedder.clone();
        let q = query.to_string();
        let mut vectors = tokio::task::spawn_blocking(move || embedder.embed(vec![q]))
            .await
            .context("query embedding task panicked")??;
        let query_vec = vectors
            .pop()
            .context("embedder returned no vector for the query")?;
        let query_bytes = embeddings::vec_to_bytes(&query_vec);

        // KNN in a CTE, then join back to articles. sqlite-vec wants the MATCH and
        // `k` constraints on the bare virtual table, so the KNN is isolated from
        // the join.
        let rows = sqlx::query(
            "WITH knn AS (
                 SELECT rowid, distance
                 FROM vec_articles
                 WHERE embedding MATCH ? AND k = ?
                 ORDER BY distance
             )
             SELECT a.id, a.title, a.url, a.source, a.category, a.summary, a.published_at, knn.distance
             FROM knn
             JOIN articles a ON a.id = knn.rowid
             ORDER BY knn.distance",
        )
        .bind(query_bytes)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("semantic search query failed")?;

        Ok(rows
            .iter()
            .map(|row| {
                let mut summary = article_summary(row);
                summary.distance = Some(row.get("distance"));
                summary
            })
            .collect())
    }

    /// Exact keyword/substring search over titles, summaries, and body content,
    /// newest first.
    pub async fn search_keyword(&self, query: &str, limit: i64) -> Result<Vec<ArticleSummary>> {
        let pattern = format!("%{query}%");
        let rows = sqlx::query(
            "SELECT id, title, url, source, category, summary, published_at
             FROM articles
             WHERE title LIKE ? OR summary LIKE ? OR content LIKE ?
             ORDER BY published_at DESC
             LIMIT ?",
        )
        .bind(&pattern)
        .bind(&pattern)
        .bind(&pattern)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("failed to search articles")?;

        Ok(rows.iter().map(article_summary).collect())
    }

    /// The most recently published articles, optionally restricted to a single
    /// `category`.
    pub async fn list_recent(
        &self,
        category: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ArticleSummary>> {
        let rows = match category {
            Some(category) => {
                sqlx::query(
                    "SELECT id, title, url, source, category, summary, published_at
                     FROM articles
                     WHERE category = ?
                     ORDER BY published_at DESC
                     LIMIT ?",
                )
                .bind(category)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
            }
            None => {
                sqlx::query(
                    "SELECT id, title, url, source, category, summary, published_at
                     FROM articles
                     ORDER BY published_at DESC
                     LIMIT ?",
                )
                .bind(limit)
                .fetch_all(&self.pool)
                .await
            }
        }
        .context("failed to list recent articles")?;

        Ok(rows.iter().map(article_summary).collect())
    }

    /// The full stored record for a single article, or `None` if no row matches.
    pub async fn get_article(&self, id: i64) -> Result<Option<Article>> {
        let row = sqlx::query(
            "SELECT id, title, url, source, category, summary, content, published_at
             FROM articles
             WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch article")?;

        Ok(row.map(|row| Article {
            id: row.get("id"),
            title: row.get("title"),
            url: row.get("url"),
            source: row.get("source"),
            category: row.get("category"),
            summary: row.get("summary"),
            content: row.get("content"),
            published_at: row.get("published_at"),
        }))
    }

    /// The latest daily snapshot for each market source (Fear & Greed, market
    /// overview, DeFi TVL). Sources with no rows yet are simply omitted.
    pub async fn market_snapshot(&self) -> Result<Vec<SnapshotItem>> {
        let mut snapshots: Vec<SnapshotItem> = Vec::with_capacity(SNAPSHOT_SOURCES.len());

        for source in SNAPSHOT_SOURCES {
            let row = sqlx::query(
                "SELECT id, title, url, source, category, summary, content, published_at
                 FROM articles
                 WHERE source = ?
                 ORDER BY published_at DESC
                 LIMIT 1",
            )
            .bind(source)
            .fetch_optional(&self.pool)
            .await
            .with_context(|| format!("failed to fetch latest {source} snapshot"))?;

            if let Some(row) = row {
                snapshots.push(SnapshotItem {
                    id: row.get("id"),
                    title: row.get("title"),
                    source: row.get("source"),
                    category: row.get("category"),
                    summary: row.get("summary"),
                    content: row.get("content"),
                    published_at: row.get("published_at"),
                });
            }
        }

        Ok(snapshots)
    }

    /// The latest derivatives reading for each tracked symbol — the current
    /// open-interest / funding / positioning picture across the watchlist. Ordered
    /// by USD open interest (largest first); symbols with no rows yet are absent.
    /// Each row's `open_interest_usd_change_24h_pct` is filled in from the reading
    /// closest to 24h prior, when one exists.
    pub async fn latest_derivatives(&self) -> Result<Vec<DerivativesRow>> {
        let rows = sqlx::query(
            "SELECT d.* FROM derivatives d
             JOIN (SELECT symbol, MAX(id) AS max_id FROM derivatives GROUP BY symbol) latest
               ON d.id = latest.max_id
             ORDER BY d.open_interest_usd DESC",
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch latest derivatives")?;

        let mut out: Vec<DerivativesRow> = rows.iter().map(derivatives_row).collect();

        // The most recent reading per symbol at least ~24h old, used as the OI and
        // price change baseline. Absent for symbols without that much history yet.
        let reference = sqlx::query(
            "SELECT d.symbol, d.open_interest_usd, d.mark_price
             FROM derivatives d
             JOIN (SELECT symbol, MAX(id) AS rid
                   FROM derivatives
                   WHERE fetched_at <= datetime('now', '-24 hours')
                   GROUP BY symbol) r
               ON d.id = r.rid",
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch derivatives 24h baseline")?;

        for row in out.iter_mut() {
            let Some(base) = reference.iter().find(|r| {
                let sym: String = r.get("symbol");
                sym == row.symbol
            }) else {
                continue;
            };
            row.open_interest_usd_change_24h_pct =
                pct_change(row.open_interest_usd, base.get("open_interest_usd"));
            row.mark_price_change_24h_pct = pct_change(row.mark_price, base.get("mark_price"));
        }

        Ok(out)
    }

    /// The recent history of derivatives readings for a single symbol, newest
    /// first — for trend questions ("is funding rising on HBAR?"). Matches the
    /// symbol case-insensitively so `hbarusdt` and `HBARUSDT` both work.
    pub async fn derivatives_history(
        &self,
        symbol: &str,
        limit: i64,
    ) -> Result<Vec<DerivativesRow>> {
        let rows = sqlx::query(
            "SELECT * FROM derivatives
             WHERE symbol = ? COLLATE NOCASE
             ORDER BY id DESC
             LIMIT ?",
        )
        .bind(symbol)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch derivatives history")?;

        Ok(rows.iter().map(derivatives_row).collect())
    }
}

/// Project a `derivatives` row into [`DerivativesRow`].
fn derivatives_row(row: &SqliteRow) -> DerivativesRow {
    DerivativesRow {
        id: row.get("id"),
        symbol: row.get("symbol"),
        open_interest: row.get("open_interest"),
        open_interest_usd: row.get("open_interest_usd"),
        funding_rate: row.get("funding_rate"),
        mark_price: row.get("mark_price"),
        long_short_ratio: row.get("long_short_ratio"),
        long_account: row.get("long_account"),
        short_account: row.get("short_account"),
        taker_buy_sell_ratio: row.get("taker_buy_sell_ratio"),
        taker_buy_vol: row.get("taker_buy_vol"),
        taker_sell_vol: row.get("taker_sell_vol"),
        top_trader_long_short_ratio: row.get("top_trader_long_short_ratio"),
        top_trader_long_account: row.get("top_trader_long_account"),
        top_trader_short_account: row.get("top_trader_short_account"),
        next_funding_time: row.get("next_funding_time"),
        fetched_at: row.get("fetched_at"),
        // Derived (not columns); filled in by latest_derivatives, left None here.
        open_interest_usd_change_24h_pct: None,
        mark_price_change_24h_pct: None,
    }
}

/// Percent change from `then` to `now`, or `None` when either is absent or the
/// baseline is zero.
fn pct_change(now: Option<f64>, then: Option<f64>) -> Option<f64> {
    match (now, then) {
        (Some(now), Some(then)) if then != 0.0 => Some((now - then) / then * 100.0),
        _ => None,
    }
}

/// Project a row into the compact [`ArticleSummary`] shape, leaving `distance`
/// unset (callers doing similarity search fill it in).
fn article_summary(row: &SqliteRow) -> ArticleSummary {
    ArticleSummary {
        id: row.get("id"),
        title: row.get("title"),
        url: row.get("url"),
        source: row.get("source"),
        category: row.get("category"),
        summary: row.get("summary"),
        published_at: row.get("published_at"),
        distance: None,
    }
}
