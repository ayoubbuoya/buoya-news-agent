use chrono::Utc;
use serde::Deserialize;

use crate::{
    error::FetchError,
    types::{Category, RawItem},
};

/// DeFiLlama TVL by chain. Free, no API key required.
const CHAINS_URL: &str = "https://api.llama.fi/v2/chains";

/// How many of the top chains to list in the snapshot body.
const TOP_CHAINS: usize = 15;

#[derive(Debug, Deserialize)]
struct Chain {
    name: String,
    tvl: Option<f64>,
}

fn fmt_usd(v: f64) -> String {
    if v >= 1e9 {
        format!("${:.2}B", v / 1e9)
    } else if v >= 1e6 {
        format!("${:.2}M", v / 1e6)
    } else {
        format!("${v:.0}")
    }
}

/// Fetch per-chain TVL and synthesize a single daily DeFi overview snapshot.
/// Day-keyed URL dedupes repeated ingests within a day via the `articles.url`
/// UNIQUE constraint.
pub async fn fetch_tvl_overview(http_client: &reqwest::Client) -> Result<Vec<RawItem>, FetchError> {
    tracing::debug!("Fetching Source : {}", CHAINS_URL);

    let bytes = http_client
        .get(CHAINS_URL)
        .send()
        .await
        .map_err(FetchError::Http)?
        .error_for_status()
        .map_err(FetchError::Http)?
        .bytes()
        .await
        .map_err(FetchError::Http)?;

    let mut chains: Vec<Chain> =
        serde_json::from_slice(bytes.as_ref()).map_err(|e| FetchError::Parse(e.to_string()))?;

    if chains.is_empty() {
        return Ok(Vec::new());
    }

    let total: f64 = chains.iter().filter_map(|c| c.tvl).sum();
    chains.sort_by(|a, b| {
        b.tvl
            .unwrap_or(0.0)
            .partial_cmp(&a.tvl.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let lines: Vec<String> = chains
        .iter()
        .filter(|c| c.tvl.is_some())
        .take(TOP_CHAINS)
        .map(|c| format!("{}: {} TVL", c.name, fmt_usd(c.tvl.unwrap_or(0.0))))
        .collect();
    let content = lines.join("\n");

    let top_chain = chains
        .first()
        .map(|c| format!("{} ({})", c.name, fmt_usd(c.tvl.unwrap_or(0.0))))
        .unwrap_or_else(|| "n/a".into());

    let day = Utc::now().format("%Y-%m-%d");
    let summary = format!(
        "Total DeFi TVL across all chains: {}. Largest chain by TVL: {top_chain}.",
        fmt_usd(total)
    );

    Ok(vec![RawItem {
        title: format!("DeFi TVL Overview by Chain ({day})"),
        url: format!("https://defillama.com/chains#{day}"),
        source: "defillama".to_string(),
        category: Category::Defi,
        summary: Some(summary),
        content: Some(content),
        published_at: Utc::now(),
    }])
}
