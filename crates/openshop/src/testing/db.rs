//! An isolated database per test.

use std::str::FromStr;

use anyhow::{Context, Result};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{Connection, PgConnection, PgPool};

use crate::db::Model;
use crate::db::migrate::push_schema;
use crate::runtime::env::DEFAULT_DATABASE_URL;

/// A PostgreSQL schema created for one test, with the framework tables pushed.
///
/// Uses `TEST_DATABASE_URL`, then `DATABASE_URL`, then the local development
/// database. Tests can run in parallel: each one only sees its own schema.
pub struct TestDb {
    pub pool: PgPool,
    pub schema: String,
    options: PgConnectOptions,
}

impl TestDb {
    pub async fn create() -> Result<Self> {
        Self::create_with_models(&[]).await
    }

    pub async fn create_with_models(models: &[Model]) -> Result<Self> {
        let url = std::env::var("TEST_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string());
        let options = PgConnectOptions::from_str(&url).context("[openshop] Invalid test database URL")?;
        let schema = format!("test_{}", uuid::Uuid::new_v4().simple());

        let mut connection = PgConnection::connect_with(&options)
            .await
            .context("[openshop] Failed to connect to the test database")?;
        sqlx::raw_sql(&format!("CREATE SCHEMA \"{schema}\""))
            .execute(&mut connection)
            .await?;
        connection.close().await?;

        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect_with(options.clone().options([("search_path", schema.as_str())]))
            .await?;
        push_schema(&pool, models, false).await?;

        Ok(Self { pool, schema, options })
    }

    /// Drops the schema and everything in it.
    pub async fn cleanup(self) -> Result<()> {
        self.pool.close().await;
        let mut connection = PgConnection::connect_with(&self.options).await?;
        sqlx::raw_sql(&format!("DROP SCHEMA IF EXISTS \"{}\" CASCADE", self.schema))
            .execute(&mut connection)
            .await?;
        connection.close().await?;
        Ok(())
    }
}
