//! Shared helpers for the integration tests. Every test gets its own database schema.
#![allow(dead_code)]

use std::sync::Arc;

use openshop::builder::Flow;
use openshop::runtime::RuntimeEnv;
use openshop::testing::{MockTransport, TestDb};
use openshop::{FlowContext, OpenShop, OpenShopConfig, PgPool, Result};

pub const TEST_SHOP: &str = "test-integration.myshopify.com";
pub const TEST_SECRET: &str = "test-secret";
pub const TEST_API_KEY: &str = "test-app";

/// An application bound to an isolated database, without an HTTP server.
pub struct TestApp {
    pub app: OpenShop,
    pub db: PgPool,
    pub transport: Arc<MockTransport>,
    test_db: TestDb,
}

pub fn test_env() -> RuntimeEnv {
    RuntimeEnv {
        shopify_api_key: Some(TEST_API_KEY.to_string()),
        shopify_api_secret: Some(TEST_SECRET.to_string()),
        cwd: std::env::temp_dir().join("openshop-test-no-toml"),
        ..Default::default()
    }
}

impl TestApp {
    pub async fn new(config: OpenShopConfig) -> Self {
        let test_db = TestDb::create().await.expect("test database");
        let transport = MockTransport::new();
        let app = OpenShop::with_transport(config, test_db.pool.clone(), test_env(), transport.clone())
            .expect("valid application");
        Self {
            db: test_db.pool.clone(),
            app,
            transport,
            test_db,
        }
    }

    /// Another application definition sharing this database.
    pub fn sibling(&self, config: OpenShopConfig) -> OpenShop {
        OpenShop::with_transport(config, self.db.clone(), test_env(), self.transport.clone())
            .expect("valid application")
    }

    pub async fn cleanup(self) {
        self.test_db.cleanup().await.expect("drop test schema");
    }
}

/// A configuration containing only the given flows.
pub fn create_config(flows: impl IntoIterator<Item = Flow>) -> OpenShopConfig {
    flows
        .into_iter()
        .fold(OpenShopConfig::builder(), |builder, flow| builder.flow(flow))
        .build()
        .expect("valid config")
}

/// A flow that does nothing.
pub fn noop_flow(name: &str) -> Flow {
    Flow::new(name, |_ctx: FlowContext| async move { Result::Ok(()) })
}

/// The directory of a project, with its `migrations` folder, removed when the test ends.
pub struct TempProject {
    dir: std::path::PathBuf,
}

impl TempProject {
    pub fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("openshop-project-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.dir
    }

    pub fn folder(&self) -> std::path::PathBuf {
        self.dir.join(openshop::db::migrate::MIGRATIONS_DIR)
    }

    /// Writes the migration `<tag>.sql`.
    pub fn write(&self, tag: &str, sql: &str) {
        self.write_file(&format!("{tag}.sql"), sql);
    }

    pub fn write_file(&self, name: &str, content: &str) {
        let path = self.folder().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A database without any table, as before the first deployment.
pub async fn empty_database() -> TestDb {
    let db = TestDb::create().await.unwrap();
    sqlx::raw_sql(&format!(
        "DROP SCHEMA \"{0}\" CASCADE; CREATE SCHEMA \"{0}\"",
        db.schema
    ))
    .execute(&db.pool)
    .await
    .unwrap();
    db
}

/// Saves the configuration of a provider for the shop of the context, through the Admin API.
pub async fn save_provider_config(ctx: &openshop::testing::TestContext, provider: &str, config: openshop::Value) {
    let response = ctx
        .put(&format!("/api/providers/{provider}"))
        .json(&openshop::json!({ "config": config }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(status, 200, "{}", response.text().await.unwrap());
}

/// Runs the connection check of a provider and returns the answer of the Admin API.
pub async fn check_provider(ctx: &openshop::testing::TestContext, provider: &str) -> openshop::Value {
    let response = ctx
        .post(&format!("/api/providers/{provider}/check"))
        .send()
        .await
        .unwrap();
    response.json().await.unwrap()
}

/// The connectors of the shop of the context, with its saved configurations.
pub async fn shop_connectors(ctx: &openshop::testing::TestContext) -> openshop::Connectors {
    ctx.app.connectors(&ctx.shop, "default")
}

/// Makes the updates of `table` that match `when` fail the way a lost
/// connection does (SQLSTATE 08006): the first `failures` of them, or all of
/// them until [`heal_write_fault`]. No code of the framework is involved.
pub async fn inject_write_fault(db: &PgPool, table: &str, when: &str, failures: i64) {
    sqlx::raw_sql(&format!(
        "CREATE SEQUENCE fault_{table}_hits; \
         CREATE FUNCTION fault_{table}() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN \
           IF nextval('fault_{table}_hits') <= {failures} THEN \
             RAISE EXCEPTION 'injected fault' USING ERRCODE = '08006'; \
           END IF; \
           RETURN NEW; \
         END $$; \
         CREATE TRIGGER fault BEFORE UPDATE ON {table} FOR EACH ROW WHEN ({when}) \
           EXECUTE FUNCTION fault_{table}()"
    ))
    .execute(db)
    .await
    .expect("fault trigger");
}

/// Removes the fault of [`inject_write_fault`].
pub async fn heal_write_fault(db: &PgPool, table: &str) {
    sqlx::raw_sql(&format!("DROP TRIGGER fault ON {table}"))
        .execute(db)
        .await
        .expect("drop fault trigger");
}

/// How many updates the fault of [`inject_write_fault`] has seen.
pub async fn write_fault_hits(db: &PgPool, table: &str) -> i64 {
    let (hits,): (i64,) = sqlx::query_as(&format!("SELECT last_value FROM fault_{table}_hits"))
        .fetch_one(db)
        .await
        .unwrap();
    hits
}
