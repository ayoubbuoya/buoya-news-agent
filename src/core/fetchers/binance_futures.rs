//! Crypto-futures derivatives metrics from Binance's keyless USDⓈ-M public API.
//!
//! For each configured symbol we read three endpoints — open interest, the
//! premium index (funding rate + mark price), and the global long/short account
//! ratio — and fold them into one [`DerivativesSnapshot`]. These are the numbers
//! market makers watch; they're stored structured in the `derivatives` table, not
//! as text articles.
//!
//! Every metric is best-effort: a single endpoint failing for one symbol logs a
//! warning and leaves that field `None` rather than dropping the whole reading.
//! Parsing is split out from the HTTP calls (the `parse_*` fns) so it can be tested
//! against fixtures without network access.

use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;

use crate::core::{error::FetchError, types::DerivativesSnapshot};

const FAPI_BASE: &str = "https://fapi.binance.com";

/// Binance `/fapi/v1/openInterest`: numbers come back as strings.
#[derive(Debug, Deserialize)]
struct OpenInterestResp {
    #[serde(rename = "openInterest")]
    open_interest: String,
}

/// Binance `/fapi/v1/premiumIndex`.
#[derive(Debug, Deserialize)]
struct PremiumIndexResp {
    #[serde(rename = "markPrice")]
    mark_price: String,
    #[serde(rename = "lastFundingRate")]
    last_funding_rate: String,
    #[serde(rename = "nextFundingTime")]
    next_funding_time: i64,
}

/// One element of Binance `/futures/data/globalLongShortAccountRatio`.
#[derive(Debug, Deserialize)]
struct LongShortResp {
    #[serde(rename = "longShortRatio")]
    long_short_ratio: String,
    #[serde(rename = "longAccount")]
    long_account: String,
    #[serde(rename = "shortAccount")]
    short_account: String,
}

fn parse_open_interest(bytes: &[u8]) -> Result<f64, FetchError> {
    let resp: OpenInterestResp =
        serde_json::from_slice(bytes).map_err(|e| FetchError::Parse(e.to_string()))?;
    resp.open_interest
        .parse()
        .map_err(|e| FetchError::Parse(format!("openInterest not a number: {e}")))
}

/// Returns `(mark_price, funding_rate, next_funding_time)`.
fn parse_premium_index(bytes: &[u8]) -> Result<(f64, f64, Option<DateTime<Utc>>), FetchError> {
    let resp: PremiumIndexResp =
        serde_json::from_slice(bytes).map_err(|e| FetchError::Parse(e.to_string()))?;
    let mark_price = resp
        .mark_price
        .parse()
        .map_err(|e| FetchError::Parse(format!("markPrice not a number: {e}")))?;
    let funding_rate = resp
        .last_funding_rate
        .parse()
        .map_err(|e| FetchError::Parse(format!("lastFundingRate not a number: {e}")))?;
    // 0 means "no next funding scheduled"; treat as absent rather than the epoch.
    let next = if resp.next_funding_time > 0 {
        Utc.timestamp_millis_opt(resp.next_funding_time).single()
    } else {
        None
    };
    Ok((mark_price, funding_rate, next))
}

/// Returns `(long_short_ratio, long_account, short_account)` from the most recent
/// (last) element of the series. An empty array yields `None`.
#[allow(clippy::type_complexity)]
fn parse_long_short(bytes: &[u8]) -> Result<Option<(f64, f64, f64)>, FetchError> {
    let series: Vec<LongShortResp> =
        serde_json::from_slice(bytes).map_err(|e| FetchError::Parse(e.to_string()))?;
    let Some(latest) = series.last() else {
        return Ok(None);
    };
    let ratio = latest
        .long_short_ratio
        .parse()
        .map_err(|e| FetchError::Parse(format!("longShortRatio not a number: {e}")))?;
    let long = latest
        .long_account
        .parse()
        .map_err(|e| FetchError::Parse(format!("longAccount not a number: {e}")))?;
    let short = latest
        .short_account
        .parse()
        .map_err(|e| FetchError::Parse(format!("shortAccount not a number: {e}")))?;
    Ok(Some((ratio, long, short)))
}

/// GET a URL and return the body bytes, mapping transport/status errors.
async fn get_bytes(http_client: &reqwest::Client, url: &str) -> Result<Vec<u8>, FetchError> {
    let bytes = http_client
        .get(url)
        .send()
        .await
        .map_err(FetchError::Http)?
        .error_for_status()
        .map_err(FetchError::Http)?
        .bytes()
        .await
        .map_err(FetchError::Http)?;
    Ok(bytes.to_vec())
}

/// Fetch a derivatives snapshot for one symbol. The three endpoints are read
/// independently and each failure is tolerated (logged, field left `None`), so a
/// partial outage still yields a useful row. Returns `None` only when *every*
/// metric failed — nothing worth storing.
async fn fetch_one(
    http_client: &reqwest::Client,
    symbol: &str,
    period: &str,
) -> Option<DerivativesSnapshot> {
    let oi_url = format!("{FAPI_BASE}/fapi/v1/openInterest?symbol={symbol}");
    let premium_url = format!("{FAPI_BASE}/fapi/v1/premiumIndex?symbol={symbol}");
    let ls_url = format!(
        "{FAPI_BASE}/futures/data/globalLongShortAccountRatio?symbol={symbol}&period={period}&limit=1"
    );

    let open_interest = match get_bytes(http_client, &oi_url).await {
        Ok(b) => parse_open_interest(&b)
            .map_err(|e| tracing::warn!("derivatives {symbol}: open interest: {e}"))
            .ok(),
        Err(e) => {
            tracing::warn!("derivatives {symbol}: open interest request: {e}");
            None
        }
    };

    let (mark_price, funding_rate, next_funding_time) =
        match get_bytes(http_client, &premium_url).await {
            Ok(b) => match parse_premium_index(&b) {
                Ok((m, f, n)) => (Some(m), Some(f), n),
                Err(e) => {
                    tracing::warn!("derivatives {symbol}: premium index: {e}");
                    (None, None, None)
                }
            },
            Err(e) => {
                tracing::warn!("derivatives {symbol}: premium index request: {e}");
                (None, None, None)
            }
        };

    let (long_short_ratio, long_account, short_account) =
        match get_bytes(http_client, &ls_url).await {
            Ok(b) => match parse_long_short(&b) {
                Ok(Some((r, l, s))) => (Some(r), Some(l), Some(s)),
                Ok(None) => (None, None, None),
                Err(e) => {
                    tracing::warn!("derivatives {symbol}: long/short ratio: {e}");
                    (None, None, None)
                }
            },
            Err(e) => {
                tracing::warn!("derivatives {symbol}: long/short ratio request: {e}");
                (None, None, None)
            }
        };

    // Nothing came back at all — skip the symbol this tick.
    if open_interest.is_none()
        && mark_price.is_none()
        && funding_rate.is_none()
        && long_short_ratio.is_none()
    {
        return None;
    }

    let open_interest_usd = match (open_interest, mark_price) {
        (Some(oi), Some(mp)) => Some(oi * mp),
        _ => None,
    };

    Some(DerivativesSnapshot {
        symbol: symbol.to_string(),
        open_interest,
        open_interest_usd,
        funding_rate,
        mark_price,
        long_short_ratio,
        long_account,
        short_account,
        next_funding_time,
    })
}

/// Fetch a derivatives snapshot for each configured symbol. Symbols are fetched
/// sequentially to stay polite to Binance's keyless rate limits; symbols that
/// return nothing are simply omitted from the result.
pub async fn fetch_derivatives(
    http_client: &reqwest::Client,
    symbols: &[String],
    period: &str,
) -> Vec<DerivativesSnapshot> {
    let mut out = Vec::with_capacity(symbols.len());
    for symbol in symbols {
        if let Some(snapshot) = fetch_one(http_client, symbol, period).await {
            out.push(snapshot);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_open_interest() {
        let body = br#"{"symbol":"HBARUSDT","openInterest":"1234567.8","time":1700000000000}"#;
        assert_eq!(parse_open_interest(body).unwrap(), 1234567.8);
    }

    #[test]
    fn parses_premium_index() {
        let body = br#"{"symbol":"BTCUSDT","markPrice":"64250.50","indexPrice":"64200.0",
            "lastFundingRate":"0.0001","nextFundingTime":1700000000000,"time":1699999000000}"#;
        let (mark, funding, next) = parse_premium_index(body).unwrap();
        assert_eq!(mark, 64250.50);
        assert_eq!(funding, 0.0001);
        assert!(next.is_some());
    }

    #[test]
    fn premium_index_zero_next_funding_is_none() {
        let body = br#"{"markPrice":"1.0","lastFundingRate":"0.0","nextFundingTime":0}"#;
        let (_, _, next) = parse_premium_index(body).unwrap();
        assert!(next.is_none());
    }

    #[test]
    fn parses_long_short_taking_latest() {
        let body = br#"[
            {"symbol":"ETHUSDT","longShortRatio":"1.20","longAccount":"0.55","shortAccount":"0.45","timestamp":1},
            {"symbol":"ETHUSDT","longShortRatio":"1.50","longAccount":"0.60","shortAccount":"0.40","timestamp":2}
        ]"#;
        let (ratio, long, short) = parse_long_short(body).unwrap().unwrap();
        assert_eq!(ratio, 1.50);
        assert_eq!(long, 0.60);
        assert_eq!(short, 0.40);
    }

    #[test]
    fn empty_long_short_series_is_none() {
        assert!(parse_long_short(b"[]").unwrap().is_none());
    }

    #[test]
    fn malformed_open_interest_errors() {
        assert!(parse_open_interest(b"not json").is_err());
        let bad_number = br#"{"openInterest":"abc"}"#;
        assert!(parse_open_interest(bad_number).is_err());
    }
}
