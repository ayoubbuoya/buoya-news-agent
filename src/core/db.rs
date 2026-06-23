use std::str::FromStr;

use anyhow::{Context, Result};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::core::types::{ChatMessage, ChatSession, Role};

pub async fn init_db() -> Result<SqlitePool> {
    let database_url =
        std::env::var("DATABASE_URL").context("DATABASE_URL must be set in environment")?;

    let opts = SqliteConnectOptions::from_str(&database_url)
        .context("invalid DATABASE_URL")?
        .create_if_missing(true);

    if let Some(parent) = opts.get_filename().parent() {
        std::fs::create_dir_all(parent)?;
    }

    let pool = SqlitePool::connect_with(opts).await?;

    // Confirm the sqlite-vec extension is loaded on connections from this pool.
    // If registration (main::register_sqlite_vec) didn't take, this fails loudly
    // here rather than later at the first vector query.
    let vec_version: String = sqlx::query_scalar("SELECT vec_version()")
        .fetch_one(&pool)
        .await
        .context("sqlite-vec extension not available (vec_version() failed)")?;
    tracing::info!("sqlite-vec loaded: {vec_version}");

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS articles (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            title       TEXT NOT NULL,
            url         TEXT NOT NULL UNIQUE,
            source      TEXT NOT NULL,
            category    TEXT NOT NULL,
            summary     TEXT,
            content     TEXT,
            published_at TEXT NOT NULL,
            created_at  TEXT NOT NULL DEFAULT (datetime('now'))
         );",
    )
    .execute(&pool)
    .await?;

    // Vector index for semantic search, one row per article keyed by
    // `rowid = articles.id`. vec0 virtual tables don't reliably support
    // `IF NOT EXISTS`, so create-and-ignore the "already exists" error, mirroring
    // the tool_calls migration below.
    let create_vec = format!(
        "CREATE VIRTUAL TABLE vec_articles USING vec0(embedding float[{}])",
        crate::core::embeddings::EMBED_DIM
    );
    // AssertSqlSafe: `create_vec` is built only from the EMBED_DIM compile-time
    // constant, never from user input.
    if let Err(e) = sqlx::query(sqlx::AssertSqlSafe(create_vec))
        .execute(&pool)
        .await
    {
        tracing::debug!("vec_articles creation skipped (likely exists): {e}");
    }

    // Structured crypto-futures derivatives readings, one row per (symbol, tick).
    // Numeric and time-stamped so trends are queryable — distinct from the text
    // `articles` store. Written by the Binance-futures fetcher each ingest tick.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS derivatives (
            id                INTEGER PRIMARY KEY AUTOINCREMENT,
            symbol            TEXT NOT NULL,
            open_interest     REAL,
            open_interest_usd REAL,
            funding_rate      REAL,
            mark_price        REAL,
            long_short_ratio  REAL,
            long_account      REAL,
            short_account     REAL,
            next_funding_time TEXT,
            fetched_at        TEXT NOT NULL DEFAULT (datetime('now'))
         );",
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_derivatives_symbol
         ON derivatives(symbol, fetched_at DESC);",
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS chat_sessions (
            id          TEXT PRIMARY KEY,
            title       TEXT NOT NULL,
            created_at  TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
         );",
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS chat_messages (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id  TEXT NOT NULL REFERENCES chat_sessions(id),
            role        TEXT NOT NULL,
            content     TEXT NOT NULL,
            tool_calls  TEXT,
            created_at  TEXT NOT NULL DEFAULT (datetime('now'))
         );",
    )
    .execute(&pool)
    .await?;

    // Backfill the column on databases created before `tool_calls` existed. The
    // ALTER errors with "duplicate column name" once the column is present, which
    // is the expected steady state, so the error is ignored.
    if let Err(e) = sqlx::query("ALTER TABLE chat_messages ADD COLUMN tool_calls TEXT")
        .execute(&pool)
        .await
    {
        tracing::debug!("tool_calls column migration skipped: {e}");
    }

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_chat_messages_session
         ON chat_messages(session_id, id);",
    )
    .execute(&pool)
    .await?;

    Ok(pool)
}

/// Create a new chat session with the given title and return it.
pub async fn create_session(pool: &SqlitePool, title: &str) -> Result<ChatSession> {
    let id = Uuid::new_v4().to_string();

    sqlx::query("INSERT INTO chat_sessions (id, title) VALUES (?, ?)")
        .bind(&id)
        .bind(title)
        .execute(pool)
        .await
        .context("failed to insert chat session")?;

    let row =
        sqlx::query("SELECT id, title, created_at, updated_at FROM chat_sessions WHERE id = ?")
            .bind(&id)
            .fetch_one(pool)
            .await
            .context("failed to read back created session")?;

    Ok(ChatSession {
        id: row.get("id"),
        title: row.get("title"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}

/// List all sessions, most recently updated first.
pub async fn list_sessions(pool: &SqlitePool) -> Result<Vec<ChatSession>> {
    let rows = sqlx::query(
        "SELECT id, title, created_at, updated_at
         FROM chat_sessions
         ORDER BY updated_at DESC, created_at DESC",
    )
    .fetch_all(pool)
    .await
    .context("failed to list chat sessions")?;

    Ok(rows
        .into_iter()
        .map(|row| ChatSession {
            id: row.get("id"),
            title: row.get("title"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
        .collect())
}

/// Fetch a single session by id, or `None` if it doesn't exist.
pub async fn get_session(pool: &SqlitePool, session_id: &str) -> Result<Option<ChatSession>> {
    let row =
        sqlx::query("SELECT id, title, created_at, updated_at FROM chat_sessions WHERE id = ?")
            .bind(session_id)
            .fetch_optional(pool)
            .await
            .context("failed to fetch chat session")?;

    Ok(row.map(|row| ChatSession {
        id: row.get("id"),
        title: row.get("title"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }))
}

/// Delete a session and all of its messages. Messages are removed first since
/// foreign-key enforcement isn't guaranteed to be on for this connection.
pub async fn delete_session(pool: &SqlitePool, session_id: &str) -> Result<()> {
    let mut tx = pool.begin().await.context("failed to begin delete tx")?;
    sqlx::query("DELETE FROM chat_messages WHERE session_id = ?")
        .bind(session_id)
        .execute(&mut *tx)
        .await
        .context("failed to delete session messages")?;
    sqlx::query("DELETE FROM chat_sessions WHERE id = ?")
        .bind(session_id)
        .execute(&mut *tx)
        .await
        .context("failed to delete session")?;
    tx.commit()
        .await
        .context("failed to commit session delete")?;
    Ok(())
}

/// Load every message of a session, oldest first.
pub async fn load_messages(pool: &SqlitePool, session_id: &str) -> Result<Vec<ChatMessage>> {
    let rows = sqlx::query(
        "SELECT id, session_id, role, content, tool_calls, created_at
         FROM chat_messages
         WHERE session_id = ?
         ORDER BY id ASC",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
    .context("failed to load chat messages")?;

    let mut messages = Vec::with_capacity(rows.len());
    for row in rows {
        let role_str: String = row.get("role");
        let role = Role::from_str(&role_str).map_err(|e| anyhow::anyhow!(e))?;
        let tools_used = row
            .get::<Option<String>, _>("tool_calls")
            .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
            .unwrap_or_default();
        messages.push(ChatMessage {
            id: row.get("id"),
            session_id: row.get("session_id"),
            role,
            content: row.get("content"),
            created_at: row.get("created_at"),
            tools_used,
        });
    }

    Ok(messages)
}

/// Insert a message into a session and return the stored row. `tools_used` are
/// the display labels for any tools the assistant invoked while producing this
/// message (empty for user messages); they are stored as a JSON array. Also
/// bumps the session's `updated_at` so recently-used sessions sort to the top.
pub async fn insert_message(
    pool: &SqlitePool,
    session_id: &str,
    role: Role,
    content: &str,
    tools_used: &[String],
) -> Result<ChatMessage> {
    let tool_calls_json = if tools_used.is_empty() {
        None
    } else {
        Some(serde_json::to_string(tools_used).context("failed to serialize tool labels")?)
    };

    let result = sqlx::query(
        "INSERT INTO chat_messages (session_id, role, content, tool_calls) VALUES (?, ?, ?, ?)",
    )
    .bind(session_id)
    .bind(role.as_str())
    .bind(content)
    .bind(&tool_calls_json)
    .execute(pool)
    .await
    .context("failed to insert chat message")?;

    let id = result.last_insert_rowid();

    touch_session(pool, session_id).await?;

    let row = sqlx::query("SELECT created_at FROM chat_messages WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .context("failed to read back inserted message")?;

    Ok(ChatMessage {
        id,
        session_id: session_id.to_string(),
        role,
        content: content.to_string(),
        created_at: row.get("created_at"),
        tools_used: tools_used.to_vec(),
    })
}

/// Bump a session's `updated_at` timestamp to now.
pub async fn touch_session(pool: &SqlitePool, session_id: &str) -> Result<()> {
    sqlx::query("UPDATE chat_sessions SET updated_at = datetime('now') WHERE id = ?")
        .bind(session_id)
        .execute(pool)
        .await
        .context("failed to touch session")?;
    Ok(())
}

/// Rename a session (used to auto-title a session from its first user message).
pub async fn rename_session(pool: &SqlitePool, session_id: &str, title: &str) -> Result<()> {
    sqlx::query("UPDATE chat_sessions SET title = ? WHERE id = ?")
        .bind(title)
        .bind(session_id)
        .execute(pool)
        .await
        .context("failed to rename session")?;
    Ok(())
}

/// Title given to a freshly created session until its first user message renames it.
pub const DEFAULT_SESSION_TITLE: &str = "New chat";

/// Build a short session title from the first line of a user message. Truncates to
/// 40 characters and falls back to [`DEFAULT_SESSION_TITLE`] when the text is empty.
/// Shared by every surface (TUI, HTTP) so a session is titled the same way regardless
/// of where the first message came from.
pub fn title_from(text: &str) -> String {
    let first_line = text.lines().next().unwrap_or(text).trim();
    let truncated: String = first_line.chars().take(40).collect();
    if truncated.is_empty() {
        DEFAULT_SESSION_TITLE.to_string()
    } else if first_line.chars().count() > 40 {
        format!("{truncated}…")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same registration as `main::register_sqlite_vec`, duplicated here so the
    /// test is self-contained. `sqlite3_auto_extension` is process-global and
    /// idempotent, so registering again is harmless.
    fn register_vec() {
        #[allow(unsafe_code)]
        unsafe {
            libsqlite3_sys::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut libsqlite3_sys::sqlite3,
                    *mut *mut std::os::raw::c_char,
                    *const libsqlite3_sys::sqlite3_api_routines,
                ) -> std::os::raw::c_int,
            >(sqlite_vec::sqlite3_vec_init as *const ())));
        }
    }

    fn vec_to_bytes(v: &[f32]) -> Vec<u8> {
        let mut b = Vec::with_capacity(v.len() * 4);
        for f in v {
            b.extend_from_slice(&f.to_le_bytes());
        }
        b
    }

    #[tokio::test]
    async fn sqlite_vec_registers_and_knn_returns_nearest() {
        register_vec();
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();

        let version: String = sqlx::query_scalar("SELECT vec_version()")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(!version.is_empty(), "vec_version() should return a version");

        sqlx::query("CREATE VIRTUAL TABLE vec_articles USING vec0(embedding float[3])")
            .execute(&pool)
            .await
            .unwrap();

        // Two rows; row 2 is nearest to the query [1,0,0].
        for (rowid, v) in [(1_i64, [0.0_f32, 1.0, 0.0]), (2, [0.9, 0.1, 0.0])] {
            sqlx::query("INSERT INTO vec_articles(rowid, embedding) VALUES (?, ?)")
                .bind(rowid)
                .bind(vec_to_bytes(&v))
                .execute(&pool)
                .await
                .unwrap();
        }

        let nearest: i64 = sqlx::query_scalar(
            "SELECT rowid FROM vec_articles WHERE embedding MATCH ? AND k = 1 ORDER BY distance",
        )
        .bind(vec_to_bytes(&[1.0_f32, 0.0, 0.0]))
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(nearest, 2, "row 2 should be the nearest neighbour");
    }
}
