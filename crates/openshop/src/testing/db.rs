//! An isolated database per test.

use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{Connection, PgConnection, PgPool};

use crate::db::migrate::push_schema;
use crate::db::{Table, ping_after_idle};
use crate::runtime::env::DEFAULT_DATABASE_URL;

/// How long a test waits for PostgreSQL before it fails instead of hanging.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The database the tests run against.
///
/// `TEST_DATABASE_URL` is used as it is: setting it is the explicit choice of a
/// database for the tests. Without it, `DATABASE_URL` is used only when it
/// points at this machine (`localhost`, a loopback address, a socket), so that
/// the shell of a deployment, with the production `DATABASE_URL` in it, never
/// runs a test. Without both, it is the local development database.
pub fn test_database_url() -> Result<String> {
    database_url_for_tests(
        std::env::var("TEST_DATABASE_URL").ok().as_deref(),
        std::env::var("DATABASE_URL").ok().as_deref(),
    )
}

fn database_url_for_tests(test_url: Option<&str>, database_url: Option<&str>) -> Result<String> {
    if let Some(url) = test_url.filter(|url| !url.is_empty()) {
        return Ok(url.to_string());
    }
    let Some(url) = database_url.filter(|url| !url.is_empty()) else {
        return Ok(DEFAULT_DATABASE_URL.to_string());
    };
    let options = PgConnectOptions::from_str(url).context("[openshop] Invalid DATABASE_URL")?;
    let host = options.get_host();
    let local = host.starts_with('/')
        || host.eq_ignore_ascii_case("localhost")
        || host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if !local {
        anyhow::bail!(
            "[openshop] DATABASE_URL points at `{host}`, not at this machine: the tests create and drop schemas, \
             so they do not run on it. Set TEST_DATABASE_URL to the database they may use."
        );
    }
    Ok(url.to_string())
}

/// Connects, or fails after [`CONNECT_TIMEOUT`] saying what is missing.
pub(crate) async fn connect(options: &PgConnectOptions) -> Result<PgConnection> {
    let unreachable = || {
        format!(
            "[openshop] Failed to connect to the test database at {}:{} (is PostgreSQL running? \
             `mise run db:up`, or set TEST_DATABASE_URL)",
            options.get_host(),
            options.get_port()
        )
    };
    tokio::time::timeout(CONNECT_TIMEOUT, PgConnection::connect_with(options))
        .await
        .map_err(|_| anyhow::anyhow!(unreachable()))?
        .with_context(unreachable)
}

/// A PostgreSQL schema created for one test, with the framework tables pushed.
///
/// Uses `TEST_DATABASE_URL`, then `DATABASE_URL` when it points at this
/// machine, then the local development database (see [`test_database_url`]).
/// Tests can run in parallel: each one only sees its own schema.
///
/// [`TestDb::cleanup`] drops the schema. A test that panics before reaching it
/// still gets a best-effort drop when the value goes out of scope.
pub struct TestDb {
    pub pool: PgPool,
    pub schema: String,
    options: PgConnectOptions,
    dropped: bool,
}

impl TestDb {
    pub async fn create() -> Result<Self> {
        Self::create_with_models(&[]).await
    }

    pub async fn create_with_models(models: &[Table]) -> Result<Self> {
        let url = test_database_url()?;
        let options = PgConnectOptions::from_str(&url).context("[openshop] Invalid test database URL")?;
        let schema = format!("test_{}", uuid::Uuid::new_v4().simple());

        let mut connection = connect(&options).await?;
        sqlx::raw_sql(&format!("CREATE SCHEMA \"{schema}\""))
            .execute(&mut connection)
            .await?;
        connection.close().await?;

        let pool = ping_after_idle(PgPoolOptions::new().max_connections(8))
            .connect_with(options.clone().options([("search_path", schema.as_str())]))
            .await?;
        push_schema(&pool, models, false).await?;

        Ok(Self {
            pool,
            schema,
            options,
            dropped: false,
        })
    }

    /// Drops the schema and everything in it.
    pub async fn cleanup(mut self) -> Result<()> {
        self.pool.close().await;
        drop_schema(&self.options, &self.schema).await?;
        self.dropped = true;
        Ok(())
    }
}

async fn drop_schema(options: &PgConnectOptions, schema: &str) -> Result<()> {
    let mut connection = PgConnection::connect_with(options).await?;
    sqlx::raw_sql(&format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE"))
        .execute(&mut connection)
        .await?;
    connection.close().await?;
    Ok(())
}

impl Drop for TestDb {
    fn drop(&mut self) {
        if self.dropped {
            return;
        }
        // Reached when a test fails before `cleanup`. The pool may still hold
        // locks, so the drop runs on its own thread, bounded in time.
        let options = self.options.clone().options([("lock_timeout", "2s")]);
        let schema = std::mem::take(&mut self.schema);
        let cleanup = std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
                return;
            };
            runtime.block_on(async {
                let _ = tokio::time::timeout(Duration::from_secs(5), drop_schema(&options, &schema)).await;
            });
        });
        let _ = cleanup.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(test_url: Option<&str>, database_url: Option<&str>) -> Result<String> {
        database_url_for_tests(test_url, database_url)
    }

    #[test]
    fn the_local_development_database_is_the_default() {
        assert_eq!(url(None, None).unwrap(), DEFAULT_DATABASE_URL);
        assert_eq!(url(Some(""), Some("")).unwrap(), DEFAULT_DATABASE_URL);
    }

    #[test]
    fn a_database_url_of_this_machine_is_used() {
        for local in [
            "postgresql://u:p@localhost:5432/db",
            "postgresql://u:p@LOCALHOST/db",
            "postgresql://u:p@127.0.0.1:55440/db?sslmode=disable",
            "postgresql://u:p@[::1]:5432/db",
            "postgresql:///db?host=/var/run/postgresql",
        ] {
            assert_eq!(url(None, Some(local)).unwrap(), local);
        }
    }

    #[test]
    fn a_database_url_of_another_machine_is_refused() {
        for remote in [
            "postgresql://u:p@db.internal:5432/app",
            "postgresql://u:p@10.0.0.4/app",
            "postgresql://u:p@localhost.evil.example/app",
            "postgresql://u:p@ep-cool-123.eu-west-2.aws.neon.tech/app",
        ] {
            let error = url(None, Some(remote)).unwrap_err().to_string();
            assert!(error.contains("TEST_DATABASE_URL"), "{remote}: {error}");
            // The password is not repeated.
            assert!(!error.contains(":p@"), "{remote}: {error}");
        }
    }

    #[test]
    fn the_test_database_url_is_explicit_and_wins() {
        let remote = "postgresql://u:p@db.internal:5432/tests";
        assert_eq!(url(Some(remote), None).unwrap(), remote);
        assert_eq!(url(Some(remote), Some("postgresql://u:p@prod.example/app")).unwrap(), remote);
    }

    #[tokio::test]
    async fn an_unreachable_database_fails_with_what_to_do() {
        let options = PgConnectOptions::from_str("postgresql://u:p@127.0.0.1:1/db").unwrap();
        let error = connect(&options).await.unwrap_err().to_string();
        assert!(error.contains("127.0.0.1:1") && error.contains("mise run db:up"), "{error}");
    }
}
