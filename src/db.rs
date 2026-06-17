//! SQLite storage for the minimal slice.
//!
//! A single connection guarded by a `Mutex` (the app is low-throughput and
//! single-process). DB calls are short and synchronous; the spec's
//! `spawn_blocking` pattern is deferred until profiling ever shows it matters.
//! Schema is a trimmed subset of §4 — one `news_items` table plus an FTS5 index.

use std::sync::Mutex;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};

use crate::domain::{Category, NewsItem, Severity};
use crate::error::DbError;

pub struct Db {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;

CREATE TABLE IF NOT EXISTS news_items (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  url TEXT NOT NULL,
  canonical_url TEXT NOT NULL UNIQUE,
  source TEXT NOT NULL,
  category TEXT NOT NULL,
  published_at TEXT NOT NULL,
  fetched_at TEXT NOT NULL,
  severity TEXT NOT NULL DEFAULT 'info',
  score REAL NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_items_published ON news_items (published_at DESC);

CREATE VIRTUAL TABLE IF NOT EXISTS news_fts USING fts5(
  title,
  content='news_items',
  content_rowid='rowid'
);

CREATE TRIGGER IF NOT EXISTS news_items_ai AFTER INSERT ON news_items BEGIN
  INSERT INTO news_fts(rowid, title) VALUES (new.rowid, new.title);
END;
CREATE TRIGGER IF NOT EXISTS news_items_ad AFTER DELETE ON news_items BEGIN
  INSERT INTO news_fts(news_fts, rowid, title) VALUES ('delete', old.rowid, old.title);
END;
";

impl Db {
    /// Open (creating if needed) the SQLite file and apply the schema.
    pub fn open(path: &str) -> Result<Self, DbError> {
        let conn = Connection::open(path).map_err(DbError::Open)?;
        conn.execute_batch(SCHEMA).map_err(DbError::Migrate)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// In-memory DB for tests.
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, DbError> {
        let conn = Connection::open_in_memory().map_err(DbError::Open)?;
        conn.execute_batch(SCHEMA).map_err(DbError::Migrate)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        // Poisoning only happens if another thread panicked mid-write; recover
        // the guard rather than propagating, the data is still consistent.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Insert an item, or ignore it if its canonical URL is already stored.
    /// Returns true when a new row was written.
    pub fn upsert(&self, item: &NewsItem, canonical_url: &str) -> Result<bool, DbError> {
        let conn = self.conn();
        let changed = conn
            .execute(
                "INSERT INTO news_items
                   (id, title, url, canonical_url, source, category, published_at, fetched_at, severity, score)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(canonical_url) DO NOTHING",
                params![
                    item.id,
                    item.title,
                    item.url,
                    canonical_url,
                    item.source,
                    item.category.as_str(),
                    item.published_at.to_rfc3339(),
                    item.fetched_at.to_rfc3339(),
                    item.severity.as_str(),
                    item.score,
                ],
            )
            .map_err(DbError::Write)?;
        Ok(changed > 0)
    }

    /// Most recent items published at or after `since`, optionally filtered by
    /// category, ordered by score then recency.
    pub fn recent(
        &self,
        since: DateTime<Utc>,
        category: Option<Category>,
        limit: u32,
    ) -> Result<Vec<NewsItem>, DbError> {
        let conn = self.conn();
        let cat = category.map(|c| c.as_str());
        let mut stmt = conn
            .prepare(
                "SELECT id, title, url, source, category, published_at, fetched_at, severity, score
                 FROM news_items
                 WHERE published_at >= ?1 AND (?2 IS NULL OR category = ?2)
                 ORDER BY score DESC, published_at DESC
                 LIMIT ?3",
            )
            .map_err(DbError::Read)?;
        let rows = stmt
            .query_map(params![since.to_rfc3339(), cat, limit], row_to_item)
            .map_err(DbError::Read)?;
        collect(rows)
    }

    /// Full-text search over titles, ranked by FTS relevance.
    pub fn search(&self, query: &str, limit: u32) -> Result<Vec<NewsItem>, DbError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT i.id, i.title, i.url, i.source, i.category, i.published_at,
                        i.fetched_at, i.severity, i.score
                 FROM news_fts f
                 JOIN news_items i ON i.rowid = f.rowid
                 WHERE news_fts MATCH ?1
                 ORDER BY bm25(news_fts), i.published_at DESC
                 LIMIT ?2",
            )
            .map_err(DbError::Read)?;
        let rows = stmt
            .query_map(params![query, limit], row_to_item)
            .map_err(DbError::Read)?;
        collect(rows)
    }

    /// Count of stored items, used to decide whether a first fetch is needed.
    pub fn count(&self) -> Result<i64, DbError> {
        let conn = self.conn();
        conn.query_row("SELECT COUNT(*) FROM news_items", [], |r| r.get(0))
            .map_err(DbError::Read)
    }

    /// Timestamp of the most recently fetched item, if any.
    pub fn last_fetched_at(&self) -> Result<Option<DateTime<Utc>>, DbError> {
        let conn = self.conn();
        let s: Option<String> = conn
            .query_row("SELECT MAX(fetched_at) FROM news_items", [], |r| r.get(0))
            .optional()
            .map_err(DbError::Read)?
            .flatten();
        Ok(s.and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|d| d.with_timezone(&Utc)))
    }
}

fn row_to_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<NewsItem> {
    let category: String = row.get(4)?;
    let severity: String = row.get(7)?;
    let published_at: String = row.get(5)?;
    let fetched_at: String = row.get(6)?;
    Ok(NewsItem {
        id: row.get(0)?,
        title: row.get(1)?,
        url: row.get(2)?,
        source: row.get(3)?,
        category: Category::parse(&category).unwrap_or(Category::Crypto),
        published_at: parse_ts(&published_at),
        fetched_at: parse_ts(&fetched_at),
        severity: Severity::parse(&severity).unwrap_or(Severity::Info),
        score: row.get(8)?,
    })
}

fn parse_ts(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

fn collect(
    rows: impl Iterator<Item = rusqlite::Result<NewsItem>>,
) -> Result<Vec<NewsItem>, DbError> {
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(DbError::Read)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::RawItem;
    use crate::pipeline;

    fn item(title: &str, url: &str) -> (NewsItem, String) {
        let raw = RawItem {
            title: title.into(),
            url: url.into(),
            source: "test".into(),
            category: Category::Ai,
            published_at: Utc::now(),
        };
        pipeline::build(&raw, &[])
    }

    #[test]
    fn upsert_is_idempotent_on_canonical_url() {
        let db = Db::open_in_memory().unwrap();
        let (it, canon) = item("Hello world", "https://x.com/a?utm_source=rss");
        assert!(db.upsert(&it, &canon).unwrap());
        // Same URL without the tracking param canonicalizes identically.
        let (it2, canon2) = item("Hello world", "https://x.com/a");
        assert!(!db.upsert(&it2, &canon2).unwrap());
        assert_eq!(db.count().unwrap(), 1);
    }

    #[test]
    fn search_finds_by_title_token() {
        let db = Db::open_in_memory().unwrap();
        let (it, canon) = item("Anthropic releases new model", "https://x.com/b");
        db.upsert(&it, &canon).unwrap();
        let hits = db.search("model", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(db.search("ethereum", 10).unwrap().is_empty());
    }
}
