//! Shared helpers for the integration tests. Every test gets its own database schema.
#![allow(dead_code)]

use std::sync::Arc;

use openshop::runtime::RuntimeEnv;
use openshop::testing::{MockTransport, TestDb};
use openshop::{Flow, FlowContext, OpenShop, OpenShopConfig, PgPool, Result};

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
    ctx.app.connectors(&ctx.shop, "default").await.unwrap()
}
