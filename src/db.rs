//! SQLite pool, migrations and the shared conventions for stored values.
//!
//! Conventions every route module follows (see `migrations/`):
//! - ids are UUID v4 as lowercase hyphenated TEXT (`new_id()`),
//! - timestamps are UTC TEXT from `now()`, the same format the schema defaults use,
//! - booleans are INTEGER 0/1 (`bool` in sqlx), JSON columns are TEXT
//!   (`sqlx::types::Json<T>` or serde_json by hand).

use std::str::FromStr;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Open (and create if needed) the database with WAL, foreign keys and a busy timeout.
pub async fn connect(url: &str) -> Result<SqlitePool, sqlx::Error> {
    let in_memory = url.contains(":memory:") || url.contains("mode=memory");
    let mut opts = SqliteConnectOptions::from_str(url)?
        .create_if_missing(true)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));
    if !in_memory {
        opts = opts
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal);
    }
    let mut pool = SqlitePoolOptions::new();
    if in_memory {
        // Each connection to :memory: is its own database, so keep exactly one.
        pool = pool
            .max_connections(1)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None);
    } else {
        pool = pool.max_connections(8);
    }
    pool.connect_with(opts).await
}

/// Connect and apply all pending migrations.
pub async fn init(url: &str) -> Result<SqlitePool, Box<dyn std::error::Error + Send + Sync>> {
    let pool = connect(url).await?;
    MIGRATOR.run(&pool).await?;
    Ok(pool)
}

/// Current UTC time in the stored format, e.g. `2026-10-27T15:28:02.123456Z`.
pub fn now() -> String {
    format_ts(chrono::Utc::now())
}

pub fn format_ts(ts: chrono::DateTime<chrono::Utc>) -> String {
    ts.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrations_apply_on_memory_db() {
        let pool = init("sqlite::memory:").await.unwrap();
        let tables: Vec<(String,)> = sqlx::query_as(
            "SELECT name FROM sqlite_master WHERE type = 'table' \
             AND name NOT LIKE '\\_%' ESCAPE '\\' AND name NOT LIKE 'sqlite%' ORDER BY name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let names: Vec<&str> = tables.iter().map(|t| t.0.as_str()).collect();
        assert_eq!(
            names,
            [
                "api_keys",
                "email_providers",
                "notification_channels",
                "webhook_events",
                "webhook_inboxes"
            ]
        );
    }

    #[tokio::test]
    async fn defaults_and_cascade_work() {
        let pool = init("sqlite::memory:").await.unwrap();
        let inbox = new_id();
        sqlx::query("INSERT INTO webhook_inboxes (id, slug) VALUES (?, 'contact')")
            .bind(&inbox)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO webhook_events (id, inbox_id, method) VALUES (?, ?, 'POST')")
            .bind(new_id())
            .bind(&inbox)
            .execute(&pool)
            .await
            .unwrap();
        let (active, created): (bool, String) =
            sqlx::query_as("SELECT is_active, created_at FROM webhook_inboxes WHERE id = ?")
                .bind(&inbox)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(active);
        // Schema default and now() produce the same shape.
        assert_eq!(created.len(), now().len());
        assert!(created.ends_with('Z'));
        let (headers,): (String,) = sqlx::query_as("SELECT headers FROM webhook_events")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(headers, "{}");

        sqlx::query("DELETE FROM webhook_inboxes WHERE id = ?")
            .bind(&inbox)
            .execute(&pool)
            .await
            .unwrap();
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM webhook_events")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 0, "events cascade with their inbox");
    }
}
