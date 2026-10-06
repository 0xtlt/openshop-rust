//! Calling custom admin page loaders and actions without HTTP.

use anyhow::Result;
use indexmap::IndexMap;

use crate::admin::{AdminActor, AdminServerContext};
use crate::app::OpenShop;

#[derive(Debug, Clone)]
pub struct AdminFunctionTestOptions {
    pub shop: String,
    pub shopify_app: String,
    pub actor_id: String,
    pub params: IndexMap<String, String>,
    pub request_id: String,
}

impl Default for AdminFunctionTestOptions {
    fn default() -> Self {
        Self {
            shop: "test.myshopify.com".to_string(),
            shopify_app: "default".to_string(),
            actor_id: "test-actor".to_string(),
            params: IndexMap::new(),
            request_id: "test-request".to_string(),
        }
    }
}

pub async fn create_admin_function_test_context(
    app: &OpenShop,
    options: AdminFunctionTestOptions,
) -> Result<AdminServerContext> {
    Ok(AdminServerContext {
        actor: AdminActor {
            id: options.actor_id,
            session_id: "test-session".to_string(),
        },
        params: options.params,
        db: app.db().clone(),
        shopify: app.shopify_client(&options.shop, &options.shopify_app).await?,
        connectors: app.connectors(&options.shop, &options.shopify_app).await?,
        request_id: options.request_id,
        idempotency_key: None,
        shop: options.shop,
        shopify_app: options.shopify_app,
        app: app.clone(),
    })
}
