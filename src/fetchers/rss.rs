use anyhow::Result;
use chrono::Utc;
use feed_rs::model::Feed;

use crate::{config::RssSource, error::FetchError, types::RawItem};

/// Fetch and parse a single RSS/Atom feed into raw items.
pub async fn fetch_rss_source(
    http_client: &reqwest::Client,
    source: &RssSource,
) -> Result<Vec<RawItem>, FetchError> {
    tracing::debug!("Fetching Source : {}", source.url);

    let bytes_source = http_client
        .get(&source.url)
        .send()
        .await
        .map_err(FetchError::Http)?
        .error_for_status()
        .map_err(FetchError::Http)?
        .bytes()
        .await
        .map_err(FetchError::Http)?;

    // NOTE: we use as_ref here to convert Bytes to &[u8] for feed-rs
    let feed: Feed = feed_rs::parser::parse(bytes_source.as_ref())
        .map_err(|e| FetchError::Parse(e.to_string()))?;

    let rss_items: Vec<RawItem> = feed
        .entries
        .into_iter()
        .filter_map(|entry| {
            let title = entry.title.map(|t| t.content)?;
            let url = entry.links.into_iter().next().map(|l| l.href)?;
            let published_at = entry
                .published
                .or(entry.updated)
                .unwrap_or_else(Utc::now)
                .with_timezone(&Utc);
            // `<content:encoded>` (RSS) / `<content>` (Atom). Not all feeds
            // provide it, so this stays optional and may be None.
            let content = entry.content.and_then(|c| c.body);
           
            Some(RawItem {
                title,
                url,
                source: source.name.clone(),
                category: source.category,
                summary: entry.summary.map(|s| s.content),
                content,
                published_at,
            })
        })
        .collect();

    Ok(rss_items)
}
