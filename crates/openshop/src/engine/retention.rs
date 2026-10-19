//! Removes the log lines older than the `log_retention` setting.
//!
//! The lines go in small batches, so that a purge of a large table never holds
//! a long statement or a long lock, and `SKIP LOCKED` lets several workers
//! purge at once without waiting for each other.

use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

/// Lines removed by one statement.
pub const PURGE_BATCH: i64 = 5_000;
/// Statements of one sweep. A larger backlog is left to the next one.
const MAX_BATCHES_PER_SWEEP: usize = 200;
/// Time between two sweeps.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// Time left to the database between two batches.
const PAUSE: Duration = Duration::from_millis(50);

/// Removes up to `batch` lines created before `cutoff`, and returns how many.
pub async fn purge_logs_batch(db: &PgPool, cutoff: DateTime<Utc>, batch: i64) -> Result<u64> {
    let result = sqlx::query(
        "DELETE FROM logs WHERE ctid IN ( \
           SELECT ctid FROM logs WHERE created_at < $1 LIMIT $2 FOR UPDATE SKIP LOCKED \
         )",
    )
    .bind(cutoff)
    .bind(batch)
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}

/// Removes the lines created before `cutoff`, a batch at a time, and returns
/// how many. Stops after a bounded number of batches: what is left goes in the
/// next sweep.
pub async fn purge_logs(db: &PgPool, cutoff: DateTime<Utc>) -> Result<u64> {
    let mut removed = 0;
    for _ in 0..MAX_BATCHES_PER_SWEEP {
        let batch = purge_logs_batch(db, cutoff, PURGE_BATCH).await?;
        removed += batch;
        if batch < PURGE_BATCH as u64 {
            break;
        }
        tokio::time::sleep(PAUSE).await;
    }
    Ok(removed)
}
