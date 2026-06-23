use anyhow::Context;
use sqlx::Row;

use crate::core::repository::ArticleSummary;
use crate::core::{
    Core, embeddings, fetchers,
    types::{DerivativesSnapshot, RawItem},
};

/// Max characters of an article fed to the embedder. BGE-small truncates around
/// ~512 tokens; this keeps us comfortably under that while capturing title +
/// summary/lead.
const EMBED_TEXT_MAX_CHARS: usize = 1500;

/// Fetch all enabled sources and persist them into the db. Returns the number of
/// newly-stored items.
pub async fn run(app_state: &Core) -> usize {
    let cfg = &app_state.config.toml_config;

    // Accumulate every article newly stored across all sources this tick. We gather
    // them here (rather than publishing per source) so subscribers receive one batch
    // per ingest run instead of a burst of small ones.
    let mut new_articles: Vec<ArticleSummary> = Vec::new();

    // --- RSS / Atom feeds ---
    for source in &cfg.sources.rss {
        match fetchers::rss::fetch_rss_source(&app_state.http_client, source).await {
            Ok(raw_items) => new_articles.extend(store_items(app_state, &raw_items).await),
            Err(e) => tracing::error!(
                "Failed to fetch rss source {} at {}: {}",
                source.name,
                source.url,
                e
            ),
        }
    }

    // --- CoinGecko market overview (keyless public API) ---
    if cfg.sources.coingecko.enabled {
        match fetchers::coingecko::fetch_market_overview(
            &app_state.http_client,
            cfg.sources.coingecko.top_n,
        )
        .await
        {
            Ok(items) => new_articles.extend(store_items(app_state, &items).await),
            Err(e) => tracing::error!("Failed to fetch coingecko market overview: {}", e),
        }
    }

    // --- DeFiLlama TVL overview (keyless) ---
    if cfg.sources.defillama.enabled {
        match fetchers::defillama::fetch_tvl_overview(&app_state.http_client).await {
            Ok(items) => new_articles.extend(store_items(app_state, &items).await),
            Err(e) => tracing::error!("Failed to fetch defillama tvl overview: {}", e),
        }
    }

    // --- Fear & Greed Index (keyless) ---
    if cfg.sources.fear_greed.enabled {
        match fetchers::feargreed::fetch_fear_greed(&app_state.http_client).await {
            Ok(items) => new_articles.extend(store_items(app_state, &items).await),
            Err(e) => tracing::error!("Failed to fetch fear & greed index: {}", e),
        }
    }

    // --- Binance futures derivatives (keyless) ---
    // Structured numeric data, not articles: it goes straight to the `derivatives`
    // table and is not embedded or broadcast as an article alert.
    if cfg.sources.derivatives.enabled {
        let snapshots = fetchers::binance_futures::fetch_derivatives(
            &app_state.http_client,
            &cfg.sources.derivatives.symbols,
            &cfg.sources.derivatives.long_short_period,
        )
        .await;
        let stored = store_derivatives(app_state, &snapshots).await;
        tracing::debug!("stored {stored} derivatives readings");
    }

    let new_stored = new_articles.len();

    // Notify subscribers (e.g. the Telegram connector) of what just landed. Skip the
    // call entirely on an empty tick — there's nothing to announce, and it avoids
    // waking subscribers for no reason. `publish_ingest` is itself a no-op when no
    // one is subscribed.
    if !new_articles.is_empty() {
        app_state.publish_ingest(new_articles);
    }

    new_stored
}

/// Persist a batch of raw items, ignoring duplicates (by URL). Newly-inserted
/// rows are then embedded and indexed for semantic search. Returns an
/// [`ArticleSummary`] for each row actually inserted, so the caller can announce
/// them to ingest subscribers.
async fn store_items(app_state: &Core, items: &[RawItem]) -> Vec<ArticleSummary> {
    // One summary per row we actually insert (duplicates are skipped). This both
    // counts the inserts (via `.len()`) and carries them to the broadcast channel.
    let mut stored: Vec<ArticleSummary> = Vec::new();
    // (article id, text-to-embed) for rows actually inserted, embedded together
    // as one batch after the loop.
    let mut to_embed: Vec<(i64, String)> = Vec::new();

    for item in items {
        let category = format!("{:?}", item.category).to_lowercase();
        let published_at = item.published_at.to_rfc3339();

        let result = sqlx::query(
            "INSERT OR IGNORE INTO articles (title, url, source, category, summary, content, published_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&item.title)
        .bind(&item.url)
        .bind(&item.source)
        .bind(&category)
        .bind(&item.summary)
        .bind(&item.content)
        .bind(&published_at)
        .execute(&app_state.db_pool)
        .await;

        match result {
            // `rows_affected() > 0` means the row was new (INSERT OR IGNORE skips
            // duplicate URLs with 0 rows affected). Build its summary from the data
            // we just inserted plus the new rowid — no extra SELECT needed. The
            // `category`/`published_at` strings match exactly what's stored, so the
            // summary is identical to what a later DB read would produce.
            Ok(r) if r.rows_affected() > 0 => {
                let id = r.last_insert_rowid();
                stored.push(ArticleSummary {
                    id,
                    title: item.title.clone(),
                    url: item.url.clone(),
                    source: item.source.clone(),
                    category,
                    summary: item.summary.clone(),
                    published_at,
                    // `distance` is only meaningful for semantic-search results;
                    // these are freshly-ingested, not a query match.
                    distance: None,
                });
                to_embed.push((id, embed_text(item)));
            }
            Ok(_) => {}
            Err(e) => tracing::error!("Failed to insert article {}: {}", item.url, e),
        }
    }

    // Embedding failure is non-fatal: the article rows are stored, they just won't
    // be semantically searchable until a later backfill picks them up.
    if !to_embed.is_empty()
        && let Err(e) = store_embeddings(app_state, to_embed).await
    {
        tracing::error!("failed to store embeddings: {e:#}");
    }

    stored
}

/// Persist a batch of derivatives snapshots, one row each. Unlike articles these
/// are append-only time-series points (no dedup), so a fresh row is written every
/// tick. Returns the number of rows stored.
async fn store_derivatives(app_state: &Core, snapshots: &[DerivativesSnapshot]) -> usize {
    let mut stored = 0usize;
    for s in snapshots {
        let next_funding = s.next_funding_time.map(|t| t.to_rfc3339());
        let result = sqlx::query(
            "INSERT INTO derivatives
                (symbol, open_interest, open_interest_usd, funding_rate, mark_price,
                 long_short_ratio, long_account, short_account, next_funding_time)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&s.symbol)
        .bind(s.open_interest)
        .bind(s.open_interest_usd)
        .bind(s.funding_rate)
        .bind(s.mark_price)
        .bind(s.long_short_ratio)
        .bind(s.long_account)
        .bind(s.short_account)
        .bind(&next_funding)
        .execute(&app_state.db_pool)
        .await;

        match result {
            Ok(_) => stored += 1,
            Err(e) => tracing::error!("Failed to insert derivatives for {}: {}", s.symbol, e),
        }
    }
    stored
}

/// Embed a batch of `(article_id, text)` pairs and insert the vectors into
/// `vec_articles`. Inference runs on a blocking thread off the async runtime.
async fn store_embeddings(app_state: &Core, items: Vec<(i64, String)>) -> anyhow::Result<()> {
    let embedder = app_state.embedder.clone();
    let texts: Vec<String> = items.iter().map(|(_, t)| t.clone()).collect();

    let vectors = tokio::task::spawn_blocking(move || embedder.embed(texts))
        .await
        .context("embedding task panicked")??;

    for ((article_id, _), vector) in items.iter().zip(vectors) {
        let bytes = embeddings::vec_to_bytes(&vector);
        sqlx::query("INSERT OR REPLACE INTO vec_articles(rowid, embedding) VALUES (?, ?)")
            .bind(article_id)
            .bind(bytes)
            .execute(&app_state.db_pool)
            .await
            .with_context(|| format!("failed to index vector for article {article_id}"))?;
    }
    Ok(())
}

/// Build the text fed to the embedder for an article: title plus its summary (or
/// body when there's no summary), capped at [`EMBED_TEXT_MAX_CHARS`] on a char
/// boundary.
fn embed_text(item: &RawItem) -> String {
    let body = item
        .summary
        .as_deref()
        .or(item.content.as_deref())
        .unwrap_or("");
    truncate_embed_text(&item.title, body)
}

fn truncate_embed_text(title: &str, body: &str) -> String {
    format!("{title}\n{body}")
        .chars()
        .take(EMBED_TEXT_MAX_CHARS)
        .collect()
}

/// Number of articles embedded per backfill batch (one model call each).
const BACKFILL_BATCH: usize = 64;

/// Embed and index any articles that lack a vector — rows stored before semantic
/// search existed, or where ingest-time embedding failed. Idempotent and safe to
/// run on every startup: it's a no-op once everything is indexed. Each batch it
/// embeds shrinks the unindexed set, so the loop terminates.
pub async fn backfill_embeddings(app_state: &Core) {
    let mut total = 0usize;

    loop {
        let rows = match sqlx::query(
            "SELECT id, title, summary, content
             FROM articles
             WHERE id NOT IN (SELECT rowid FROM vec_articles)
             ORDER BY id
             LIMIT ?",
        )
        .bind(BACKFILL_BATCH as i64)
        .fetch_all(&app_state.db_pool)
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::error!("backfill query failed: {e:#}");
                return;
            }
        };

        if rows.is_empty() {
            break;
        }

        let items: Vec<(i64, String)> = rows
            .iter()
            .map(|row| {
                let id: i64 = row.get("id");
                let title: String = row.get("title");
                let summary: Option<String> = row.get("summary");
                let content: Option<String> = row.get("content");
                let body = summary.as_deref().or(content.as_deref()).unwrap_or("");
                (id, truncate_embed_text(&title, body))
            })
            .collect();

        let n = items.len();
        if let Err(e) = store_embeddings(app_state, items).await {
            tracing::error!("backfill embedding failed: {e:#}");
            return;
        }
        total += n;
        tracing::info!("backfilled embeddings: {n} articles ({total} total)");
    }

    if total > 0 {
        tracing::info!("embedding backfill complete: {total} articles");
    }
}
