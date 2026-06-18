use std::cmp::Ordering;

use chrono::Utc;
use serde::Deserialize;

use crate::{
    error::FetchError,
    types::{Category, RawItem},
};

#[derive(Debug, Deserialize)]
struct Coin {
    name: String,
    symbol: String,
    current_price: Option<f64>,
    price_change_percentage_24h: Option<f64>,
}

fn fmt_price(p: f64) -> String {
    if p >= 1.0 {
        format!("${p:.2}")
    } else {
        format!("${p:.6}")
    }
}

/// Fetch the top-N coins by market cap from CoinGecko's keyless public API and
/// synthesize a single daily market-overview snapshot. Day-keyed URL dedupes
/// repeated ingests within a day via the `articles.url` UNIQUE constraint.
pub async fn fetch_market_overview(
    http_client: &reqwest::Client,
    top_n: u32,
) -> Result<Vec<RawItem>, FetchError> {
    let url = format!(
        "https://api.coingecko.com/api/v3/coins/markets\
         ?vs_currency=usd&order=market_cap_desc&per_page={}&page=1&price_change_percentage=24h",
        top_n.clamp(1, 250)
    );
    tracing::debug!("Fetching Source : {}", url);

    let bytes = http_client
        .get(&url)
        .send()
        .await
        .map_err(FetchError::Http)?
        .error_for_status()
        .map_err(FetchError::Http)?
        .bytes()
        .await
        .map_err(FetchError::Http)?;

    let coins: Vec<Coin> =
        serde_json::from_slice(bytes.as_ref()).map_err(|e| FetchError::Parse(e.to_string()))?;

    if coins.is_empty() {
        return Ok(Vec::new());
    }

    let lines: Vec<String> = coins
        .iter()
        .map(|c| {
            let price = c.current_price.map(fmt_price).unwrap_or_else(|| "n/a".into());
            let chg = c
                .price_change_percentage_24h
                .map(|p| format!("{p:+.2}%"))
                .unwrap_or_else(|| "n/a".into());
            format!("{} ({}): {price} (24h {chg})", c.name, c.symbol.to_uppercase())
        })
        .collect();
    let content = lines.join("\n");

    // Pick the biggest 24h gainer/loser among coins that report a change.
    let by_change = |a: &&Coin, b: &&Coin| {
        a.price_change_percentage_24h
            .unwrap_or(f64::MIN)
            .partial_cmp(&b.price_change_percentage_24h.unwrap_or(f64::MIN))
            .unwrap_or(Ordering::Equal)
    };
    let mover_phrase = |c: Option<&Coin>| {
        c.map(|c| {
            format!(
                "{} ({:+.2}%)",
                c.symbol.to_uppercase(),
                c.price_change_percentage_24h.unwrap_or(0.0)
            )
        })
        .unwrap_or_else(|| "n/a".into())
    };
    let has_change = |c: &&Coin| c.price_change_percentage_24h.is_some();
    let gainer = mover_phrase(coins.iter().filter(has_change).max_by(by_change));
    let loser = mover_phrase(coins.iter().filter(has_change).min_by(by_change));

    let day = Utc::now().format("%Y-%m-%d");
    let summary = format!(
        "Top {} coins by market cap. Biggest 24h gainer: {gainer}; biggest loser: {loser}.",
        coins.len()
    );

    Ok(vec![RawItem {
        title: format!(
            "Crypto Market Overview — Top {} by Market Cap ({day})",
            coins.len()
        ),
        url: format!("https://www.coingecko.com/#{day}"),
        source: "coingecko".to_string(),
        category: Category::Market,
        summary: Some(summary),
        content: Some(content),
        published_at: Utc::now(),
    }])
}
