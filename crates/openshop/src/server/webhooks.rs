//! Shopify webhooks: `POST /webhooks`, dispatched by the `X-Shopify-Topic` header.

use std::panic::AssertUnwindSafe;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::post;
use futures::FutureExt;
use serde_json::{Value, json};

use super::http::{header, json_error, json_response};
use crate::app::OpenShop;
use crate::engine::step::error_message;
use crate::runtime::{logger, report};
use crate::server::connectors::Connectors;
use crate::shopify::client::ShopifyClient;
use crate::types::WebhookContext;

/// Shopify sends `orders/create`; handlers may also be registered as `ORDERS_CREATE`.
pub fn normalize_topic(topic: &str) -> String {
    topic.replace('/', "_").to_uppercase()
}

async fn handle(State(app): State<OpenShop>, headers: HeaderMap, body: Bytes) -> Response {
    let config = app.config();
    let body = String::from_utf8_lossy(&body).into_owned();
    let read = |name: &str| header(&headers, name).unwrap_or_default().to_string();
    let hmac = read("x-shopify-hmac-sha256");
    let topic = read("x-shopify-topic");
    let shop = read("x-shopify-shop-domain");
    let api_version = read("x-shopify-api-version");

    if !app.apps().has_configured_secret() {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "SHOPIFY_API_SECRET is not configured",
        );
    }
    let Ok(shopify_app) = app.apps().by_webhook_hmac(&body, &hmac) else {
        return json_error(StatusCode::UNAUTHORIZED, "Invalid webhook HMAC");
    };

    let ok = || json_response(StatusCode::OK, json!({ "ok": true }));
    let Some(handler) = config
        .webhooks
        .get(&topic)
        .or_else(|| config.webhooks.get(&normalize_topic(&topic)))
    else {
        logger::warn(format!("[openshop] No webhook handler for topic \"{topic}\""));
        // Always return 200 to Shopify
        return ok();
    };

    let Ok(payload) = serde_json::from_str::<Value>(&body) else {
        return json_error(StatusCode::BAD_REQUEST, "Invalid JSON body");
    };
    report::unit("webhook", &topic);
    report::shop(&shop, &shopify_app.handle);

    let context = WebhookContext {
        topic: topic.clone(),
        shop: shop.clone(),
        shopify_app: shopify_app.handle.clone(),
        payload,
        api_version,
        db: app.db().clone(),
        shopify: ShopifyClient::new(&app, &shop, &shopify_app.handle),
        connectors: Connectors::for_shop(&app, &shop, &shopify_app.handle),
        app: app.clone(),
    };
    // Still return 200 on handler errors — don't let Shopify retry them.
    let failure = match AssertUnwindSafe((handler.run)(context)).catch_unwind().await {
        Ok(Ok(())) => None,
        Ok(Err(error)) => {
            report::error(&error);
            Some(error_message(&error))
        }
        Err(_) => Some("handler panicked".to_string()),
    };
    if let Some(error) = failure {
        logger::error_with(
            format!("[openshop] Webhook handler error for \"{topic}\""),
            json!({ "error": error }),
        );
    }

    ok()
}

pub fn create_webhook_routes() -> Router<OpenShop> {
    Router::new()
        .route("/webhooks", post(handle))
        .route("/webhooks/", post(handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_topics() {
        assert_eq!(normalize_topic("orders/create"), "ORDERS_CREATE");
        assert_eq!(normalize_topic("app/uninstalled"), "APP_UNINSTALLED");
        assert_eq!(normalize_topic("ORDERS_CREATE"), "ORDERS_CREATE");
    }
}
