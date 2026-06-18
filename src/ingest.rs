use crate::{fetchers, state::AppState, types::RawItem};

/// Fetch all enabled sources and persist them into the db. Returns the number of
/// newly-stored items.
pub async fn run(app_state: &AppState) -> usize {
    let cfg = &app_state.config.toml_config;
    let mut new_stored: usize = 0;

    // --- RSS / Atom feeds ---
    for source in &cfg.sources.rss {
        match fetchers::rss::fetch_rss_source(&app_state.http_client, source).await {
            Ok(raw_items) => new_stored += store_items(app_state, &raw_items).await,
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
            Ok(items) => new_stored += store_items(app_state, &items).await,
            Err(e) => tracing::error!("Failed to fetch coingecko market overview: {}", e),
        }
    }

    // --- DeFiLlama TVL overview (keyless) ---
    if cfg.sources.defillama.enabled {
        match fetchers::defillama::fetch_tvl_overview(&app_state.http_client).await {
            Ok(items) => new_stored += store_items(app_state, &items).await,
            Err(e) => tracing::error!("Failed to fetch defillama tvl overview: {}", e),
        }
    }

    // --- Fear & Greed Index (keyless) ---
    if cfg.sources.fear_greed.enabled {
        match fetchers::feargreed::fetch_fear_greed(&app_state.http_client).await {
            Ok(items) => new_stored += store_items(app_state, &items).await,
            Err(e) => tracing::error!("Failed to fetch fear & greed index: {}", e),
        }
    }

    new_stored
}

/// Persist a batch of raw items, ignoring duplicates (by URL). Returns the count
/// of rows actually inserted.
async fn store_items(app_state: &AppState, items: &[RawItem]) -> usize {
    let mut stored = 0;
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
            Ok(r) if r.rows_affected() > 0 => stored += 1,
            Ok(_) => {}
            Err(e) => tracing::error!("Failed to insert article {}: {}", item.url, e),
        }
    }
    stored
}
