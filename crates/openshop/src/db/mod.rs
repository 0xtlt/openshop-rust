//! PostgreSQL access: connection pool, framework tables, models and migrations.

pub mod migrate;
pub mod model;
pub mod schema;

use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

pub use model::{Model, define_model};

fn env_number(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// Pool sizing comes from `PGPOOL_MAX`, `PGPOOL_IDLE_TIMEOUT_MS` and
/// `PGPOOL_CONNECTION_TIMEOUT_MS`.
pub fn pool_options() -> PgPoolOptions {
    PgPoolOptions::new()
        .max_connections(env_number("PGPOOL_MAX", 10) as u32)
        .idle_timeout(Duration::from_millis(env_number("PGPOOL_IDLE_TIMEOUT_MS", 30_000)))
        .acquire_timeout(Duration::from_millis(env_number("PGPOOL_CONNECTION_TIMEOUT_MS", 5_000)))
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
