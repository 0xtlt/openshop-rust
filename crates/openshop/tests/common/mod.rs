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
