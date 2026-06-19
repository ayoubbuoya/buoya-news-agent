use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::core::{
    error::FetchError,
    types::{Category, RawItem},
};

/// Alternative.me Crypto Fear & Greed Index. Free, no API key required.
const FNG_URL: &str = "https://api.alternative.me/fng/?limit=1";

#[derive(Debug, Deserialize)]
struct FngResponse {
    data: Vec<FngEntry>,
}

#[derive(Debug, Deserialize)]
struct FngEntry {
    /// Index value 0..=100 as a string, e.g. "40".
    value: String,
    /// Human label, e.g. "Fear", "Extreme Greed".
    value_classification: String,
    /// Unix seconds, as a string.
    timestamp: String,
}

/// Fetch the latest Fear & Greed reading and synthesize a single daily snapshot
/// item. The URL is day-keyed so repeated ingests within a day are deduped by the
/// `articles.url` UNIQUE constraint (the index only updates once per day anyway).
pub async fn fetch_fear_greed(http_client: &reqwest::Client) -> Result<Vec<RawItem>, FetchError> {
    tracing::debug!("Fetching Source : {}", FNG_URL);

    let bytes = http_client
        .get(FNG_URL)
        .send()
        .await
        .map_err(FetchError::Http)?
        .error_for_status()
        .map_err(FetchError::Http)?
        .bytes()
        .await
        .map_err(FetchError::Http)?;

    let resp: FngResponse =
        serde_json::from_slice(bytes.as_ref()).map_err(|e| FetchError::Parse(e.to_string()))?;

    let Some(entry) = resp.data.into_iter().next() else {
        return Ok(Vec::new());
    };

    let published_at = entry
        .timestamp
        .parse::<i64>()
        .ok()
        .and_then(|secs| DateTime::from_timestamp(secs, 0))
        .unwrap_or_else(Utc::now);

    let day = published_at.format("%Y-%m-%d");
    let title = format!(
        "Crypto Fear & Greed Index: {} ({})",
        entry.value_classification, entry.value
    );
    let body = format!(
        "Market sentiment is currently '{}' with a score of {}/100 \
         (0 = Extreme Fear, 100 = Extreme Greed). Source: alternative.me.",
        entry.value_classification, entry.value
    );

    Ok(vec![RawItem {
        title,
        url: format!("https://alternative.me/crypto/fear-and-greed-index/#{day}"),
        source: "fear-greed".to_string(),
        category: Category::Market,
        summary: Some(body.clone()),
        content: Some(body),
        published_at,
    }])
}
