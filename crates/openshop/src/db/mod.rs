//! PostgreSQL access: connection pool, framework tables, models and migrations.

pub mod diff;
pub mod migrate;
pub mod model;
pub mod record;
pub mod schema;

use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, PgPool};

pub use model::{ColumnDef, ForeignKeyDef, IndexDef, Model, define_model};
pub use record::{Bindable, Column, ColumnType, Condition, Order, Query, Record, Rows, SqlBuilder};

fn env_number(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// An idle connection is pinged before use only when it waited longer than this.
const PING_AFTER_IDLE: Duration = Duration::from_secs(1);

/// Pool sizing comes from `PGPOOL_MAX`, `PGPOOL_IDLE_TIMEOUT_MS` and
/// `PGPOOL_CONNECTION_TIMEOUT_MS`. A connection is pinged before use only
/// after it waited more than a second in the pool.
pub fn pool_options() -> PgPoolOptions {
    ping_after_idle(
        PgPoolOptions::new()
            .max_connections(env_number("PGPOOL_MAX", 10) as u32)
            .idle_timeout(Duration::from_millis(env_number("PGPOOL_IDLE_TIMEOUT_MS", 30_000)))
            .acquire_timeout(Duration::from_millis(env_number("PGPOOL_CONNECTION_TIMEOUT_MS", 5_000))),
    )
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

/// Connects using `DATABASE_URL`.
pub async fn connect() -> Result<PgPool> {
    let url = std::env::var("DATABASE_URL").context("[openshop] DATABASE_URL not set")?;
    connect_to(&url).await
}

pub async fn connect_to(url: &str) -> Result<PgPool> {
    pool_options()
        .connect(url)
        .await
        .context("[openshop] Failed to connect to PostgreSQL")
}
