//! One-time import of a Python install's Postgres data into SQLite:
//! `hookforms import-postgres --from postgres://user:pass@host/db [--to /data/hookforms.db] [--force]`.
//!
//! Reads every table the Python Alembic migrations 0001 and 0002 create
//! (`webhook_inboxes`, `webhook_events`, `api_keys`, `notification_channels`,
//! `email_providers`) inside one read-only snapshot, and writes them in one
//! SQLite transaction, converted to the stored formats of `migrations/`:
//! - UUIDs as lowercase hyphenated text,
//! - timestamps as `2026-10-27T15:28:02.123456Z` (UTC, six digits, `Z`), the
//!   format `db::format_ts` writes and the cleanup compares against,
//! - JSON and JSONB as compact JSON text, the `scopes` array as a JSON array,
//! - booleans as 0/1, where a NULL becomes 0 (every Python filter used
//!   `is_active IS TRUE`, so a NULL row already behaved as inactive).
//!
//! API key hashes are copied as they are: passlib's `$pbkdf2-sha256$` format is
//! what `auth::verify_key` checks, so keys made in Python keep working.
//! Only table names and row counts are ever printed, never row contents.

use std::fmt;

use sqlx::postgres::{PgConnection, PgRow};
use sqlx::sqlite::SqliteConnection;
use sqlx::{Connection, Row, SqlitePool};

use crate::db;

pub const DEFAULT_TARGET: &str = "/data/hookforms.db";
const EVENT_BATCH: i64 = 1000;

/// Tables in insert order (parents first).
pub const TABLES: [&str; 5] = [
    "webhook_inboxes",
    "api_keys",
    "email_providers",
    "notification_channels",
    "webhook_events",
];

pub const USAGE: &str = "usage: hookforms import-postgres --from postgres://user:pass@host:5432/db \
[--to /data/hookforms.db] [--force]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub from: String,
    pub to: String,
    pub force: bool,
}

#[derive(Debug)]
pub enum ImportError {
    Usage(String),
    /// The target already holds data and `--force` was not given.
    TargetNotEmpty(Vec<(&'static str, i64)>),
    Source(sqlx::Error),
    Target(sqlx::Error),
    Migrate(String),
    /// A stored value could not be converted (table, column, row id).
    Convert {
        table: &'static str,
        column: &'static str,
        id: String,
    },
    Verify {
        table: &'static str,
        source: i64,
        target: i64,
    },
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::Usage(m) => write!(f, "{m}\n{USAGE}"),
            ImportError::TargetNotEmpty(counts) => {
                write!(f, "the target database already has data (")?;
                let parts: Vec<String> = counts
                    .iter()
                    .filter(|(_, n)| *n > 0)
                    .map(|(t, n)| format!("{t}: {n}"))
                    .collect();
                write!(
                    f,
                    "{}); refusing to import. Run with --force to replace it.",
                    parts.join(", ")
                )
            }
            ImportError::Source(e) => write!(f, "Postgres error: {e}"),
            ImportError::Target(e) => write!(f, "SQLite error: {e}"),
            ImportError::Migrate(e) => write!(f, "SQLite setup failed: {e}"),
            ImportError::Convert { table, column, id } => {
                write!(f, "cannot convert {table}.{column} of row {id}")
            }
            ImportError::Verify {
                table,
                source,
                target,
            } => write!(
                f,
                "count check failed for {table}: {source} rows in Postgres, {target} in SQLite"
            ),
        }
    }
}

impl std::error::Error for ImportError {}

/// Parse the arguments after `import-postgres`.
pub fn parse_args<I, S>(args: I) -> Result<Options, ImportError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut from = None;
    let mut to = None;
    let mut force = false;
    let mut it = args.into_iter().map(|a| a.as_ref().to_string());
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        match flag.as_str() {
            "--from" | "--to" => {
                let value = inline
                    .or_else(|| it.next())
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| ImportError::Usage(format!("{flag} needs a value")))?;
                if flag == "--from" {
                    from = Some(value);
                } else {
                    to = Some(value);
                }
            }
            "--force" if inline.is_none() => force = true,
            other => {
                return Err(ImportError::Usage(format!("unknown argument {other:?}")));
            }
        }
    }
    let from = from.ok_or_else(|| ImportError::Usage("--from is required".into()))?;
    Ok(Options {
        from: postgres_url(&from)?,
        to: to.unwrap_or_else(|| DEFAULT_TARGET.to_string()),
        force,
    })
}

/// Accept the Python `DATABASE_URL` as it is (`postgresql+asyncpg://...`).
pub fn postgres_url(url: &str) -> Result<String, ImportError> {
    let url = url.trim();
    let rest = ["postgresql+asyncpg://", "postgresql://", "postgres://"]
        .iter()
        .find_map(|p| url.strip_prefix(p))
        .ok_or_else(|| ImportError::Usage("--from must be a postgres:// URL".into()))?;
    Ok(format!("postgres://{rest}"))
}

/// A plain path becomes a `sqlite://` URL, a `sqlite:` URL is kept.
pub fn sqlite_url(to: &str) -> String {
    if to.starts_with("sqlite:") {
        to.to_string()
    } else {
        format!("sqlite://{to}")
    }
}

/// What the import did, for printing.
#[derive(Debug, Default)]
pub struct Report {
    /// Rows per table, in `TABLES` order, re-counted in SQLite after commit.
    pub counts: Vec<(&'static str, i64)>,
    /// Tables of the HookForms schema missing in the source (counted as 0).
    pub missing: Vec<&'static str>,
    /// Other tables found in the source and not imported.
    pub skipped: Vec<String>,
    /// True when `--force` replaced existing data.
    pub replaced: bool,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.replaced {
            writeln!(f, "Existing data in the target was replaced (--force).")?;
        }
        for t in &self.missing {
            writeln!(
                f,
                "Table {t} not found in Postgres, nothing to import there."
            )?;
        }
        for t in &self.skipped {
            writeln!(f, "Table {t} is not part of the HookForms schema, skipped.")?;
        }
        writeln!(f, "Imported rows:")?;
        for (t, n) in &self.counts {
            writeln!(f, "  {t:<24}{n:>10}")?;
        }
        write!(f, "Verified: every count matches the source.")
    }
}

/// Run the import.
pub async fn run(opts: &Options) -> Result<Report, ImportError> {
    let pool = db::init(&sqlite_url(&opts.to))
        .await
        .map_err(|e| ImportError::Migrate(e.to_string()))?;
    let result = import_into(&pool, &opts.from, opts.force).await;
    pool.close().await;
    result
}

/// Import into an open, migrated SQLite pool.
pub async fn import_into(
    pool: &SqlitePool,
    from: &str,
    force: bool,
) -> Result<Report, ImportError> {
    let existing = count_target(pool).await?;
    let has_data = existing.iter().any(|(_, n)| *n > 0);
    if has_data && !force {
        return Err(ImportError::TargetNotEmpty(existing));
    }

    let mut src = PgConnection::connect(from)
        .await
        .map_err(ImportError::Source)?;
    // One consistent snapshot of every table, and UTC for every timestamp text.
    sqlx::query("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut src)
        .await
        .map_err(ImportError::Source)?;
    sqlx::query("SET LOCAL TIME ZONE 'UTC'")
        .execute(&mut src)
        .await
        .map_err(ImportError::Source)?;

    let present: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables \
         WHERE table_schema = current_schema() AND table_type = 'BASE TABLE' ORDER BY 1",
    )
    .fetch_all(&mut src)
    .await
    .map_err(ImportError::Source)?;
    let has = |t: &str| present.iter().any(|p| p == t);
    let mut report = Report {
        replaced: has_data,
        missing: TABLES.iter().copied().filter(|t| !has(t)).collect(),
        skipped: present
            .iter()
            .filter(|p| !TABLES.contains(&p.as_str()) && p.as_str() != "alembic_version")
            .cloned()
            .collect(),
        ..Report::default()
    };

    let mut source_counts = Vec::new();
    for t in TABLES {
        let n: i64 = if has(t) {
            sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {t}"))
                .fetch_one(&mut src)
                .await
                .map_err(ImportError::Source)?
        } else {
            0
        };
        source_counts.push((t, n));
    }

    let mut conn = pool.acquire().await.map_err(ImportError::Target)?;
    let mut tx = conn.begin().await.map_err(ImportError::Target)?;
    if has_data {
        for t in TABLES.iter().rev() {
            sqlx::query(&format!("DELETE FROM {t}"))
                .execute(&mut *tx)
                .await
                .map_err(ImportError::Target)?;
        }
    }
    if has("webhook_inboxes") {
        copy_inboxes(&mut src, &mut tx).await?;
    }
    if has("api_keys") {
        copy_keys(&mut src, &mut tx).await?;
    }
    if has("email_providers") {
        copy_providers(&mut src, &mut tx).await?;
    }
    if has("notification_channels") {
        copy_channels(&mut src, &mut tx).await?;
    }
    if has("webhook_events") {
        copy_events(&mut src, &mut tx).await?;
    }
    tx.commit().await.map_err(ImportError::Target)?;
    drop(conn);
    let _ = sqlx::query("ROLLBACK").execute(&mut src).await;
    let _ = src.close().await;

    let target_counts = count_target(pool).await?;
    for ((t, source), (_, target)) in source_counts.iter().zip(&target_counts) {
        if source != target {
            return Err(ImportError::Verify {
                table: t,
                source: *source,
                target: *target,
            });
        }
    }
    report.counts = target_counts;
    Ok(report)
}

async fn count_target(pool: &SqlitePool) -> Result<Vec<(&'static str, i64)>, ImportError> {
    let mut out = Vec::new();
    for t in TABLES {
        let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {t}"))
            .fetch_one(pool)
            .await
            .map_err(ImportError::Target)?;
        out.push((t, n));
    }
    Ok(out)
}

/// Postgres expression giving a timestamp column in the stored text format.
/// The session time zone is UTC, so `timestamptz` prints as UTC.
fn ts(col: &str) -> String {
    format!("to_char({col}, 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS {col}")
}

/// JSON text from Postgres (json, jsonb or an array via `to_jsonb`) as the
/// compact text the Rust app writes. NULL stays NULL.
fn json_text(
    row: &PgRow,
    table: &'static str,
    column: &'static str,
    id: &str,
) -> Result<Option<String>, ImportError> {
    let raw: Option<String> = row.try_get(column).map_err(ImportError::Source)?;
    raw.map(|t| {
        serde_json::from_str::<serde_json::Value>(&t)
            .map(|v| v.to_string())
            .map_err(|_| ImportError::Convert {
                table,
                column,
                id: id.to_string(),
            })
    })
    .transpose()
}

fn get<'r, T: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>>(
    row: &'r PgRow,
    col: &str,
) -> Result<T, ImportError> {
    row.try_get(col).map_err(ImportError::Source)
}

fn active(row: &PgRow) -> Result<bool, ImportError> {
    Ok(get::<Option<bool>>(row, "is_active")?.unwrap_or(false))
}

async fn copy_inboxes(
    src: &mut PgConnection,
    dst: &mut SqliteConnection,
) -> Result<(), ImportError> {
    let sql = format!(
        "SELECT id::text AS id, slug, description, forward_url, notify_email, \
         email_subject_prefix, sender_name, turnstile_secret, is_active, {}, {} \
         FROM webhook_inboxes ORDER BY created_at, id",
        ts("created_at"),
        ts("updated_at")
    );
    let rows = sqlx::query(&sql)
        .fetch_all(&mut *src)
        .await
        .map_err(ImportError::Source)?;
    for r in &rows {
        sqlx::query(
            "INSERT INTO webhook_inboxes (id, slug, description, forward_url, notify_email, \
             email_subject_prefix, sender_name, turnstile_secret, is_active, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(get::<String>(r, "id")?)
        .bind(get::<String>(r, "slug")?)
        .bind(get::<Option<String>>(r, "description")?)
        .bind(get::<Option<String>>(r, "forward_url")?)
        .bind(get::<Option<String>>(r, "notify_email")?)
        .bind(get::<Option<String>>(r, "email_subject_prefix")?)
        .bind(get::<Option<String>>(r, "sender_name")?)
        .bind(get::<Option<String>>(r, "turnstile_secret")?)
        .bind(active(r)?)
        .bind(get::<String>(r, "created_at")?)
        .bind(get::<String>(r, "updated_at")?)
        .execute(&mut *dst)
        .await
        .map_err(ImportError::Target)?;
    }
    Ok(())
}

async fn copy_keys(src: &mut PgConnection, dst: &mut SqliteConnection) -> Result<(), ImportError> {
    let sql = format!(
        "SELECT id::text AS id, name, key_hash, key_prefix, to_jsonb(scopes)::text AS scopes, \
         is_active, {}, {}, {} FROM api_keys ORDER BY created_at, id",
        ts("last_used_at"),
        ts("created_at"),
        ts("updated_at")
    );
    let rows = sqlx::query(&sql)
        .fetch_all(&mut *src)
        .await
        .map_err(ImportError::Source)?;
    for r in &rows {
        let id: String = get(r, "id")?;
        let scopes = json_text(r, "api_keys", "scopes", &id)?;
        sqlx::query(
            "INSERT INTO api_keys (id, name, key_hash, key_prefix, scopes, is_active, \
             last_used_at, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(get::<String>(r, "name")?)
        .bind(get::<String>(r, "key_hash")?)
        .bind(get::<Option<String>>(r, "key_prefix")?)
        .bind(scopes)
        .bind(active(r)?)
        .bind(get::<Option<String>>(r, "last_used_at")?)
        .bind(get::<String>(r, "created_at")?)
        .bind(get::<String>(r, "updated_at")?)
        .execute(&mut *dst)
        .await
        .map_err(ImportError::Target)?;
    }
    Ok(())
}

async fn copy_providers(
    src: &mut PgConnection,
    dst: &mut SqliteConnection,
) -> Result<(), ImportError> {
    let sql = format!(
        "SELECT id::text AS id, inbox_id::text AS inbox_id, type, config::text AS config, \
         is_active, {} FROM email_providers ORDER BY created_at, id",
        ts("created_at")
    );
    let rows = sqlx::query(&sql)
        .fetch_all(&mut *src)
        .await
        .map_err(ImportError::Source)?;
    for r in &rows {
        let id: String = get(r, "id")?;
        let config = json_text(r, "email_providers", "config", &id)?;
        sqlx::query(
            "INSERT INTO email_providers (id, inbox_id, type, config, is_active, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(get::<Option<String>>(r, "inbox_id")?)
        .bind(get::<String>(r, "type")?)
        .bind(config.unwrap_or_else(|| "{}".into()))
        .bind(active(r)?)
        .bind(get::<String>(r, "created_at")?)
        .execute(&mut *dst)
        .await
        .map_err(ImportError::Target)?;
    }
    Ok(())
}

async fn copy_channels(
    src: &mut PgConnection,
    dst: &mut SqliteConnection,
) -> Result<(), ImportError> {
    let sql = format!(
        "SELECT id::text AS id, inbox_id::text AS inbox_id, type, label, config::text AS config, \
         is_active, {} FROM notification_channels ORDER BY created_at, id",
        ts("created_at")
    );
    let rows = sqlx::query(&sql)
        .fetch_all(&mut *src)
        .await
        .map_err(ImportError::Source)?;
    for r in &rows {
        let id: String = get(r, "id")?;
        let config = json_text(r, "notification_channels", "config", &id)?;
        sqlx::query(
            "INSERT INTO notification_channels (id, inbox_id, type, label, config, is_active, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(get::<String>(r, "inbox_id")?)
        .bind(get::<String>(r, "type")?)
        .bind(get::<Option<String>>(r, "label")?)
        .bind(config.unwrap_or_else(|| "{}".into()))
        .bind(active(r)?)
        .bind(get::<String>(r, "created_at")?)
        .execute(&mut *dst)
        .await
        .map_err(ImportError::Target)?;
    }
    Ok(())
}

/// Events can be many: read them in id order, a batch at a time.
async fn copy_events(
    src: &mut PgConnection,
    dst: &mut SqliteConnection,
) -> Result<(), ImportError> {
    let sql = format!(
        "SELECT id::text AS id, inbox_id::text AS inbox_id, method, headers::text AS headers, \
         body::text AS body, query_params::text AS query_params, source_ip, {} \
         FROM webhook_events WHERE ($1::uuid IS NULL OR id > $1::uuid) ORDER BY id LIMIT $2",
        ts("received_at")
    );
    let mut after: Option<String> = None;
    loop {
        let rows = sqlx::query(&sql)
            .bind(after.as_deref())
            .bind(EVENT_BATCH)
            .fetch_all(&mut *src)
            .await
            .map_err(ImportError::Source)?;
        for r in &rows {
            let id: String = get(r, "id")?;
            let headers = json_text(r, "webhook_events", "headers", &id)?;
            let body = json_text(r, "webhook_events", "body", &id)?;
            let query = json_text(r, "webhook_events", "query_params", &id)?;
            sqlx::query(
                "INSERT INTO webhook_events (id, inbox_id, method, headers, body, query_params, \
                 source_ip, received_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(get::<String>(r, "inbox_id")?)
            .bind(get::<String>(r, "method")?)
            .bind(headers)
            .bind(body)
            .bind(query)
            .bind(get::<Option<String>>(r, "source_ip")?)
            .bind(get::<String>(r, "received_at")?)
            .execute(&mut *dst)
            .await
            .map_err(ImportError::Target)?;
            after = Some(id);
        }
        if (rows.len() as i64) < EVENT_BATCH {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args() {
        let o = parse_args(["--from", "postgresql+asyncpg://u:p@pg:5432/hookforms"]).unwrap();
        assert_eq!(
            o,
            Options {
                from: "postgres://u:p@pg:5432/hookforms".into(),
                to: DEFAULT_TARGET.into(),
                force: false
            }
        );
        let o = parse_args(["--to=./x.db", "--force", "--from=postgres://h/db"]).unwrap();
        assert_eq!(o.to, "./x.db");
        assert!(o.force);
        assert_eq!(o.from, "postgres://h/db");
        assert!(matches!(
            parse_args(["--to", "x"]),
            Err(ImportError::Usage(_))
        ));
        assert!(matches!(parse_args(["--from"]), Err(ImportError::Usage(_))));
        assert!(matches!(
            parse_args(["--from", "mysql://h/db"]),
            Err(ImportError::Usage(_))
        ));
        assert!(matches!(
            parse_args(["--from", "postgres://h/db", "--bogus"]),
            Err(ImportError::Usage(_))
        ));
    }

    #[test]
    fn sqlite_targets() {
        assert_eq!(
            sqlite_url("/data/hookforms.db"),
            "sqlite:///data/hookforms.db"
        );
        assert_eq!(sqlite_url("sqlite::memory:"), "sqlite::memory:");
    }

    #[test]
    fn errors_never_carry_the_url() {
        let e = parse_args(["--from", "mysql://user:hunter2@h/db"]).unwrap_err();
        assert!(!e.to_string().contains("hunter2"));
    }

    #[tokio::test]
    async fn refuses_a_target_with_data_before_touching_postgres() {
        let pool = db::init("sqlite::memory:").await.unwrap();
        sqlx::query("INSERT INTO webhook_inboxes (id, slug) VALUES (?, 'x')")
            .bind(db::new_id())
            .execute(&pool)
            .await
            .unwrap();
        // The URL points nowhere: the check must fail first.
        let err = import_into(&pool, "postgres://nobody@127.0.0.1:1/none", false)
            .await
            .unwrap_err();
        assert!(matches!(err, ImportError::TargetNotEmpty(_)));
        assert!(err.to_string().contains("webhook_inboxes: 1"));
        assert!(err.to_string().contains("--force"));
    }
}
