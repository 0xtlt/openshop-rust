//! PostgreSQL access: connection pool, framework tables, models and migrations.

pub mod diff;
pub mod migrate;
pub mod model;
pub mod record;
pub mod schema;

use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, PgPool};

pub use model::{ColumnDef, ForeignKeyDef, IndexDef, Table};
pub use record::{Bindable, Column, ColumnType, Condition, Model, Order, Query, Record, Rows, SqlBuilder};
pub use schema::{FlowRun, Installation, LogRow, StepResult};

const DEFAULT_MAX: u32 = 10;

/// What sqlx is given for a connection timeout of `0`: it has no way to wait
/// without a limit.
const NO_TIMEOUT: Duration = Duration::from_secs(365 * 24 * 60 * 60);

/// An idle connection is pinged before use only when it waited longer than this.
const PING_AFTER_IDLE: Duration = Duration::from_secs(1);

/// Pool sizing comes from `PGPOOL_MAX`, `PGPOOL_IDLE_TIMEOUT_MS` and
/// `PGPOOL_CONNECTION_TIMEOUT_MS`. A connection is pinged before use only
/// after it waited more than a second in the pool.
pub fn pool_options() -> PgPoolOptions {
    pool_options_from(|name| std::env::var(name).ok())
}

/// `0` means what it means to node-postgres: the default size, and a timeout
/// that is disabled.
fn pool_options_from(var: impl Fn(&str) -> Option<String>) -> PgPoolOptions {
    let max = match number(var("PGPOOL_MAX"), DEFAULT_MAX) {
        0 => DEFAULT_MAX,
        max => max,
    };
    ping_after_idle(
        PgPoolOptions::new()
            .max_connections(max)
            .idle_timeout(timeout(var("PGPOOL_IDLE_TIMEOUT_MS"), 30_000))
            .acquire_timeout(timeout(var("PGPOOL_CONNECTION_TIMEOUT_MS"), 5_000).unwrap_or(NO_TIMEOUT)),
    )
}

fn number<T: FromStr>(value: Option<String>, default: T) -> T {
    value.and_then(|value| value.parse().ok()).unwrap_or(default)
}

/// A timeout in milliseconds, `None` when `0` disables it.
fn timeout(value: Option<String>, default: u64) -> Option<Duration> {
    let millis = number(value, default);
    (millis > 0).then(|| Duration::from_millis(millis))
}

/// sqlx pings a connection before every statement run on the pool, a second
/// round trip. This pings only the connections that waited more than
/// [`PING_AFTER_IDLE`], and replaces the ones that fail.
///
/// A connection the server closed sooner after its last use is handed out
/// unchecked: the statement fails, and the pool drops the connection when it
/// comes back.
pub(crate) fn ping_after_idle(options: PgPoolOptions) -> PgPoolOptions {
    options.test_before_acquire(false).before_acquire(|connection, meta| {
        Box::pin(async move {
            // `false` has the pool close the connection and take another one.
            Ok(meta.idle_for <= PING_AFTER_IDLE || connection.ping().await.is_ok())
        })
    })
}

pub async fn connect_to(url: &str) -> Result<PgPool> {
    pool_options()
        .connect(url)
        .await
        .context("[openshop] Failed to connect to PostgreSQL")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(vars: &[(&str, &str)]) -> PgPoolOptions {
        pool_options_from(|name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        })
    }

    /// Size, idle timeout and connection timeout, the timeouts in milliseconds.
    fn settings(vars: &[(&str, &str)]) -> (u32, Option<u128>, u128) {
        let options = options(vars);
        (
            options.get_max_connections(),
            options.get_idle_timeout().map(|timeout| timeout.as_millis()),
            options.get_acquire_timeout().as_millis(),
        )
    }

    const ZEROS: [(&str, &str); 3] = [
        ("PGPOOL_MAX", "0"),
        ("PGPOOL_IDLE_TIMEOUT_MS", "0"),
        ("PGPOOL_CONNECTION_TIMEOUT_MS", "0"),
    ];

    #[test]
    fn reads_the_pool_variables() {
        assert_eq!(settings(&[]), (10, Some(30_000), 5_000));
        assert_eq!(
            settings(&[
                ("PGPOOL_MAX", "25"),
                ("PGPOOL_IDLE_TIMEOUT_MS", "1500"),
                ("PGPOOL_CONNECTION_TIMEOUT_MS", "250"),
            ]),
            (25, Some(1_500), 250)
        );
    }

    #[test]
    fn a_value_that_is_not_a_non_negative_integer_falls_back_to_the_default() {
        for value in ["", "abc", "-1", "1.5", "99999999999999999999"] {
            assert_eq!(
                settings(&[
                    ("PGPOOL_MAX", value),
                    ("PGPOOL_IDLE_TIMEOUT_MS", value),
                    ("PGPOOL_CONNECTION_TIMEOUT_MS", value),
                ]),
                (10, Some(30_000), 5_000),
                "{value:?}"
            );
        }
        // Too large for a pool size.
        assert_eq!(settings(&[("PGPOOL_MAX", "4294967296")]).0, 10);
    }

    #[test]
    fn zero_disables_a_timeout_and_keeps_the_default_size() {
        assert_eq!(settings(&ZEROS), (10, None, NO_TIMEOUT.as_millis()));
    }

    #[tokio::test]
    async fn a_pool_without_timeouts_connects_and_keeps_its_connection() {
        let url = crate::testing::test_database_url().unwrap();
        // A pool without timeouts retries a refused connection forever: check
        // that PostgreSQL answers, and fail with what to do if it does not.
        let connect_options = url.parse().unwrap();
        crate::testing::connect(&connect_options).await.unwrap().close().await.unwrap();
        let pool = options(&ZEROS).connect(&url).await.unwrap();

        sqlx::query("SELECT 1").execute(&pool).await.unwrap();
        // Time for the connection to come back, and to be closed if the pool
        // took `0` for an idle timeout.
        tokio::time::sleep(Duration::from_millis(200)).await;

        assert_eq!((pool.size(), pool.num_idle()), (1, 1));
        pool.close().await;
    }
}
