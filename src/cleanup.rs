//! Nightly retention cleanup (Python `app/worker.py`, the arq cron at 03:00 UTC):
//! delete `webhook_events` whose `received_at` is older than
//! `Config::event_retention_days`. Started from `main.rs` next to the server.
//!
//! `received_at` is TEXT in the fixed-width format of `db::format_ts`
//! (`2026-10-27T15:28:02.123456Z`), so the cutoff is formatted the same way and
//! compared as text, which then orders exactly like time. Comparing against any
//! other shape (a `+00:00` suffix, a space instead of `T`, fewer digits) would
//! silently keep or drop the wrong rows.

use chrono::{DateTime, Duration, NaiveTime, TimeDelta, Utc};
use sqlx::SqlitePool;
use tokio::task::JoinHandle;

use crate::db;

/// Hour (UTC) of the daily run, as in the Python cron (`hour={3}, minute={0}`).
pub const RUN_HOUR: u32 = 3;

/// Source of the current time, so tests can pin it.
pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> DateTime<Utc>;
}

/// The real clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// The stored-format cutoff: events received strictly before it are deleted
/// (Python `received_at < now - timedelta(days=...)`). `None` when the
/// retention is so large the date would leave chrono's range: nothing is old enough.
pub fn cutoff(now: DateTime<Utc>, retention_days: i64) -> Option<String> {
    let delta = TimeDelta::try_days(retention_days)?;
    now.checked_sub_signed(delta).map(db::format_ts)
}

/// Delete every event older than the retention window and return how many went.
pub async fn run_once(
    pool: &SqlitePool,
    clock: &dyn Clock,
    retention_days: i64,
) -> Result<u64, sqlx::Error> {
    let Some(cutoff) = cutoff(clock.now(), retention_days) else {
        return Ok(0);
    };
    let result = sqlx::query("DELETE FROM webhook_events WHERE received_at < ?")
        .bind(&cutoff)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// The first 03:00 UTC strictly after `now`.
pub fn next_run_after(now: DateTime<Utc>) -> DateTime<Utc> {
    let at = NaiveTime::from_hms_opt(RUN_HOUR, 0, 0).expect("valid time");
    let today = now.date_naive().and_time(at).and_utc();
    if today > now {
        today
    } else {
        today + Duration::days(1)
    }
}

/// Run the cleanup every day at 03:00 UTC until the task is dropped.
pub fn spawn(pool: SqlitePool, retention_days: i64) -> JoinHandle<()> {
    spawn_with_clock(pool, retention_days, SystemClock)
}

pub fn spawn_with_clock<C: Clock>(
    pool: SqlitePool,
    retention_days: i64,
    clock: C,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let now = clock.now();
            let wait = (next_run_after(now) - now)
                .to_std()
                .unwrap_or(std::time::Duration::from_secs(1));
            tokio::time::sleep(wait).await;
            match run_once(&pool, &clock, retention_days).await {
                Ok(n) => tracing::info!(
                    deleted = n,
                    retention_days,
                    "event cleanup: deleted {n} events older than {retention_days} days"
                ),
                Err(e) => tracing::error!(error = %e, "event cleanup failed"),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    struct Fixed(DateTime<Utc>);
    impl Clock for Fixed {
        fn now(&self) -> DateTime<Utc> {
            self.0
        }
    }

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32, us: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap() + Duration::microseconds(us as i64)
    }

    async fn pool_with_events(times: &[(&str, String)]) -> SqlitePool {
        let pool = db::init("sqlite::memory:").await.unwrap();
        let inbox = db::new_id();
        sqlx::query("INSERT INTO webhook_inboxes (id, slug) VALUES (?, 'contact')")
            .bind(&inbox)
            .execute(&pool)
            .await
            .unwrap();
        for (name, ts) in times {
            sqlx::query(
                "INSERT INTO webhook_events (id, inbox_id, method, source_ip, received_at) \
                 VALUES (?, ?, 'POST', ?, ?)",
            )
            .bind(db::new_id())
            .bind(&inbox)
            .bind(name)
            .bind(ts)
            .execute(&pool)
            .await
            .unwrap();
        }
        pool
    }

    async fn remaining(pool: &SqlitePool) -> Vec<String> {
        sqlx::query_as::<_, (String,)>("SELECT source_ip FROM webhook_events ORDER BY received_at")
            .fetch_all(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.0)
            .collect()
    }

    #[test]
    fn cutoff_uses_the_stored_format() {
        let now = at(2026, 10, 27, 3, 0, 0, 0);
        assert_eq!(
            cutoff(now, 30).unwrap(),
            "2026-09-27T03:00:00.000000Z".to_string()
        );
        assert_eq!(
            cutoff(at(2026, 3, 1, 12, 5, 9, 7), 1).unwrap(),
            "2026-02-28T12:05:09.000007Z"
        );
        assert!(cutoff(now, i64::MAX).is_none());
    }

    #[tokio::test]
    async fn boundary_exactly_at_cutoff_and_one_second_either_side() {
        let now = at(2026, 10, 27, 3, 0, 0, 0);
        let edge = now - Duration::days(30);
        let pool = pool_with_events(&[
            ("before", db::format_ts(edge - Duration::seconds(1))),
            (
                "before_1us",
                db::format_ts(edge - Duration::microseconds(1)),
            ),
            ("exact", db::format_ts(edge)),
            ("after", db::format_ts(edge + Duration::seconds(1))),
            ("fresh", db::format_ts(now)),
        ])
        .await;
        let n = run_once(&pool, &Fixed(now), 30).await.unwrap();
        assert_eq!(n, 2);
        // Strictly older than the cutoff goes, exactly at the cutoff stays.
        assert_eq!(remaining(&pool).await, ["exact", "after", "fresh"]);
        // A second run finds nothing more.
        assert_eq!(run_once(&pool, &Fixed(now), 30).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn schema_default_timestamps_compare_correctly() {
        // Rows stamped by the column default (strftime + '000Z') share the format.
        let pool = db::init("sqlite::memory:").await.unwrap();
        let inbox = db::new_id();
        sqlx::query("INSERT INTO webhook_inboxes (id, slug) VALUES (?, 'x')")
            .bind(&inbox)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO webhook_events (id, inbox_id, method) VALUES (?, ?, 'POST')")
            .bind(db::new_id())
            .bind(&inbox)
            .execute(&pool)
            .await
            .unwrap();
        let clock = Fixed(Utc::now() + Duration::minutes(1));
        assert_eq!(run_once(&pool, &clock, 0).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn zero_and_huge_retention() {
        let now = at(2026, 10, 27, 3, 0, 0, 0);
        let pool = pool_with_events(&[
            ("old", "2001-01-01T00:00:00.000000Z".into()),
            ("fresh", db::format_ts(now - Duration::seconds(1))),
        ])
        .await;
        assert_eq!(run_once(&pool, &Fixed(now), 100_000).await.unwrap(), 0);
        assert_eq!(run_once(&pool, &Fixed(now), i64::MAX).await.unwrap(), 0);
        assert_eq!(run_once(&pool, &Fixed(now), 0).await.unwrap(), 2);
    }

    #[test]
    fn next_run_is_the_coming_three_am_utc() {
        assert_eq!(
            next_run_after(at(2026, 10, 27, 2, 59, 59, 0)),
            at(2026, 10, 27, 3, 0, 0, 0)
        );
        assert_eq!(
            next_run_after(at(2026, 10, 27, 3, 0, 0, 0)),
            at(2026, 10, 28, 3, 0, 0, 0)
        );
        assert_eq!(
            next_run_after(at(2026, 10, 27, 3, 0, 0, 1)),
            at(2026, 10, 28, 3, 0, 0, 0)
        );
        assert_eq!(
            next_run_after(at(2026, 12, 31, 23, 0, 0, 0)),
            at(2027, 1, 1, 3, 0, 0, 0)
        );
    }

    #[tokio::test]
    async fn spawned_task_waits_for_three_am_then_deletes() {
        // The clock is pinned 200 ms before 03:00, so the task's first sleep is
        // 200 ms of real time.
        let now = at(2026, 10, 27, 2, 59, 59, 800_000);
        let pool = pool_with_events(&[("old", "2001-01-01T00:00:00.000000Z".into())]).await;
        let handle = spawn_with_clock(pool.clone(), 30, Fixed(now));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(remaining(&pool).await, ["old"], "nothing before 03:00");
        let mut left = remaining(&pool).await;
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            left = remaining(&pool).await;
            if left.is_empty() {
                break;
            }
        }
        assert!(left.is_empty(), "deleted at 03:00");
        handle.abort();
    }
}
