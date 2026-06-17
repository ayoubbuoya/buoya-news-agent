//! Turn a `RawItem` into a stored `NewsItem`: canonicalize the URL, derive a
//! deterministic id, and apply a simple recency + keyword score.
//!
//! This is the minimal-slice pipeline. Cross-source dedup grouping and the full
//! weighted scoring of §6 are deferred; what's here is a pure function over the
//! item so it stays easy to test and to extend later.

use chrono::Utc;
use sha2::{Digest, Sha256};
use url::Url;

use crate::domain::{NewsItem, RawItem, Severity};

/// Keyword that bumps an item's score and can raise it to `notable`.
pub struct Keyword<'a> {
    pub term: &'a str,
    pub weight: f64,
}

/// Build a scored `NewsItem` plus its canonical URL (used as the dedup key).
pub fn build(raw: &RawItem, keywords: &[Keyword<'_>]) -> (NewsItem, String) {
    let canonical = canonical_url(&raw.url);
    let id = item_id(&canonical);

    let now = Utc::now();
    let age_hours = (now - raw.published_at).num_minutes() as f64 / 60.0;
    // Half-life decay: a day-old item scores ~0.71, a week-old ~0.13.
    let recency = 0.5_f64.powf(age_hours.max(0.0) / 48.0);

    let title_lc = raw.title.to_lowercase();
    let keyword_score: f64 = keywords
        .iter()
        .filter(|k| title_lc.contains(&k.term.to_lowercase()))
        .map(|k| k.weight)
        .sum();

    let score = recency + keyword_score;
    let severity = if keyword_score >= 1.0 {
        Severity::Notable
    } else {
        Severity::Info
    };

    let item = NewsItem {
        id,
        title: raw.title.clone(),
        url: raw.url.clone(),
        source: raw.source.clone(),
        category: raw.category,
        published_at: raw.published_at,
        fetched_at: now,
        severity,
        score,
    };
    (item, canonical)
}

/// Lowercase host, drop tracking params, strip fragment and trailing slash.
/// Falls back to the trimmed raw string for URLs the `url` crate can't parse.
fn canonical_url(raw: &str) -> String {
    let Ok(mut url) = Url::parse(raw) else {
        return raw.trim().trim_end_matches('/').to_string();
    };
    url.set_fragment(None);

    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| !is_tracking_param(k))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if kept.is_empty() {
        url.set_query(None);
    } else {
        let mut qs = url.query_pairs_mut();
        qs.clear();
        for (k, v) in &kept {
            qs.append_pair(k, v);
        }
        drop(qs);
    }

    let mut s = url.to_string();
    if s.ends_with('/') {
        s.pop();
    }
    s
}

fn is_tracking_param(key: &str) -> bool {
    key.starts_with("utm_") || matches!(key, "ref" | "fbclid" | "gclid")
}

/// First 32 hex chars of sha256(canonical_url) — stable across re-fetches.
fn item_id(canonical: &str) -> String {
    let digest = Sha256::digest(canonical.as_bytes());
    hex::encode(&digest[..16])
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn canonical_strips_tracking_and_trailing_slash() {
        assert_eq!(
            canonical_url("https://x.com/post/?utm_source=rss&id=7#frag"),
            "https://x.com/post/?id=7"
        );
        assert_eq!(canonical_url("https://x.com/a/"), "https://x.com/a");
    }

    #[test]
    fn same_url_yields_same_id() {
        let a = item_id(&canonical_url("https://x.com/a?utm_medium=x"));
        let b = item_id(&canonical_url("https://x.com/a"));
        assert_eq!(a, b);
    }

    #[test]
    fn keyword_match_raises_severity_and_score() {
        let raw = RawItem {
            title: "Major exploit drains protocol".into(),
            url: "https://x.com/hack".into(),
            source: "rekt".into(),
            category: crate::domain::Category::Security,
            published_at: Utc::now(),
        };
        let kws = [Keyword { term: "exploit", weight: 1.0 }];
        let (item, _) = build(&raw, &kws);
        assert_eq!(item.severity, Severity::Notable);
        assert!(item.score > 1.0);
    }

    #[test]
    fn older_items_score_lower() {
        let base = RawItem {
            title: "neutral headline".into(),
            url: "https://x.com/n".into(),
            source: "s".into(),
            category: crate::domain::Category::Ai,
            published_at: Utc::now(),
        };
        let (fresh, _) = build(&base, &[]);
        let mut old = base.clone();
        old.published_at = Utc::now() - Duration::hours(96);
        let (stale, _) = build(&old, &[]);
        assert!(fresh.score > stale.score);
    }
}
