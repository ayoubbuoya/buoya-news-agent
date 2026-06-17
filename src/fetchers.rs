//! Source fetchers. The minimal slice ships only the generic RSS/Atom fetcher;
//! HN, Reddit, CoinGecko, etc. (§5 of the spec) are added later.
//!
//! Each fetcher splits into a thin network step (`fetch_bytes`) and a pure
//! `parse` step so parsing can be unit-tested against fixtures with no network.

use chrono::Utc;
use feed_rs::parser;

use crate::config::RssSource;
use crate::domain::{Category, RawItem};
use crate::error::FetchError;

/// Fetch and parse a single RSS/Atom feed into raw items.
pub async fn fetch_rss(
    client: &reqwest::Client,
    source: &RssSource,
) -> Result<Vec<RawItem>, FetchError> {
    let bytes = client
        .get(&source.url)
        .send()
        .await
        .map_err(FetchError::Http)?
        .error_for_status()
        .map_err(FetchError::Http)?
        .bytes()
        .await
        .map_err(FetchError::Http)?;
    parse_rss(&bytes, &source.name, source.category)
}

/// Pure: parse feed bytes into `RawItem`s. Entries missing a title or link are
/// skipped rather than failing the whole feed.
pub fn parse_rss(
    bytes: &[u8],
    source_name: &str,
    category: Category,
) -> Result<Vec<RawItem>, FetchError> {
    let feed = parser::parse(bytes).map_err(|e| FetchError::Parse(e.to_string()))?;

    let items = feed
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
            Some(RawItem {
                title,
                url,
                source: source_name.to_string(),
                category,
                published_at,
            })
        })
        .collect();
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ATOM: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Test Feed</title>
  <entry>
    <title>First post</title>
    <link href="https://example.com/1"/>
    <updated>2026-06-10T12:00:00Z</updated>
  </entry>
  <entry>
    <title>Second post</title>
    <link href="https://example.com/2"/>
    <updated>2026-06-11T12:00:00Z</updated>
  </entry>
</feed>"#;

    #[test]
    fn parses_atom_entries() {
        let items = parse_rss(ATOM.as_bytes(), "test", Category::Ai).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "First post");
        assert_eq!(items[0].source, "test");
        assert_eq!(items[1].url, "https://example.com/2");
    }
}
