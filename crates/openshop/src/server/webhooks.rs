//! Shopify webhooks: `POST /webhooks`, dispatched by the `X-Shopify-Topic` header.
//!
//! Shopify delivers a webhook at least once: a delivery it believes failed is
//! sent again, with the same `X-Shopify-Webhook-Id`. A delivery this process
//! has already run is acknowledged and not run again, within [`REMEMBER`].

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::Mutex;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::post;
use futures::FutureExt;
use serde_json::{Value, json};
use tokio::time::Instant;

use super::http::{header, json_error, json_response};
use crate::app::OpenShop;
use crate::engine::step::error_message;
use crate::runtime::report::{self, Panicked};
use crate::runtime::logger;
use crate::server::connectors::Connectors;
use crate::server::shop_domain::strict_shop_domain;
use crate::shopify::client::ShopifyClient;
use crate::types::WebhookContext;

/// How long a delivery is remembered. Shopify retries over two days, but a
/// retry follows a failure by minutes: an hour covers the ones that matter,
/// and a longer memory would only grow.
pub(crate) const REMEMBER: Duration = Duration::from_secs(60 * 60);
/// How many deliveries are remembered at once: when the memory is full, the
/// oldest are forgotten first.
const MAX_REMEMBERED: usize = 100_000;

/// The deliveries this process has run. It is in memory: another replica, or
/// a restart, does not know them, and a handler must still be safe to run
/// twice for a delivery (see the [webhooks reference](../../../docs/src/reference/webhooks.md)).
#[derive(Default)]
pub(crate) struct WebhookDeliveries {
    seen: Mutex<HashMap<String, Instant>>,
}

impl WebhookDeliveries {
    /// Takes note of a delivery. `false` when it was already there: the
    /// delivery was run, or is being run.
    ///
    /// The key names the app and the topic as well as the id, and carries the
    /// signature: the headers are not signed, so a delivery with an id that was
    /// not sent by Shopify with that body is another delivery.
    pub(crate) fn first_time(&self, shopify_app: &str, topic: &str, id: &str, signature: &str) -> bool {
        self.first_time_at(Instant::now(), shopify_app, topic, id, signature)
    }

    fn first_time_at(&self, now: Instant, shopify_app: &str, topic: &str, id: &str, signature: &str) -> bool {
        let key = format!("{shopify_app}\n{topic}\n{id}\n{signature}");
        let mut seen = self.seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if seen.get(&key).is_some_and(|at| now.duration_since(*at) < REMEMBER) {
            return false;
        }
        if seen.len() >= MAX_REMEMBERED {
            seen.retain(|_, at| now.duration_since(*at) < REMEMBER);
            if seen.len() >= MAX_REMEMBERED {
                let oldest = seen.values().min().copied();
                seen.retain(|_, at| Some(*at) != oldest);
            }
        }
        seen.insert(key, now);
        true
    }
}

/// Shopify sends `orders/create`; handlers may also be registered as `ORDERS_CREATE`.
pub fn normalize_topic(topic: &str) -> String {
    topic.replace('/', "_").to_uppercase()
}

async fn handle(State(app): State<OpenShop>, headers: HeaderMap, body: Bytes) -> Response {
    let config = app.config();
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

    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        return json_error(StatusCode::BAD_REQUEST, "Invalid JSON body");
    };
    // The signature covers the body, not this header: it is only taken as a
    // shop domain written in full, never a bare name.
    let Some(shop) = strict_shop_domain(&shop).filter(|domain| *domain == shop.trim().to_ascii_lowercase()) else {
        return json_error(StatusCode::BAD_REQUEST, "Invalid shop domain");
    };
    report::describe(|delivery| {
        delivery.unit("webhook", &topic).shop(&shop, &shopify_app.handle);
    });

    // A delivery Shopify sends again is acknowledged, not run again.
    let delivery_id = header(&headers, "x-shopify-webhook-id")
        .or_else(|| header(&headers, "x-shopify-event-id"))
        .filter(|id| !id.is_empty());
    if let Some(id) = delivery_id
        && !app.webhook_deliveries().first_time(&shopify_app.handle, &topic, id, &hmac)
    {
        logger::info(format!(
            "[openshop] Webhook \"{topic}\" delivery {id} was already received: not run again"
        ));
        return ok();
    }

    let context = WebhookContext {
        topic: topic.clone(),
        shop: shop.clone(),
        shopify_app: shopify_app.handle.clone(),
        payload,
        api_version,
        db: app.db().clone(),
        // A delivery can be replayed and its shop header is not signed: the
        // client is the one of background work, which renews the token a
        // shop's row holds and asks for no other shop than the configured one.
        shopify: ShopifyClient::new(&app, &shop, &shopify_app.handle),
        connectors: Connectors::for_shop(&app, &shop, &shopify_app.handle),
        app: app.clone(),
    };
    // Still return 200 on handler errors — don't let Shopify retry them.
    let failure = match AssertUnwindSafe((handler.run)(context)).catch_unwind().await {
        Ok(outcome) => outcome.err(),
        Err(_) => Some(Panicked::error("handler panicked")),
    };
    if let Some(error) = failure {
        report::error(&error);
        logger::error_with(
            format!("[openshop] Webhook handler error for \"{topic}\""),
            json!({ "error": error_message(&error) }),
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
    fn a_delivery_is_run_once_within_the_time_it_is_remembered() {
        let deliveries = WebhookDeliveries::default();
        let start = Instant::now();
        assert!(deliveries.first_time_at(start, "app", "ORDERS_CREATE", "id-1", "sig"));
        assert!(!deliveries.first_time_at(start + Duration::from_secs(5), "app", "ORDERS_CREATE", "id-1", "sig"));
        // Another id, topic, app or body is another delivery.
        assert!(deliveries.first_time_at(start, "app", "ORDERS_CREATE", "id-2", "sig"));
        assert!(deliveries.first_time_at(start, "app", "ORDERS_PAID", "id-1", "sig"));
        assert!(deliveries.first_time_at(start, "other", "ORDERS_CREATE", "id-1", "sig"));
        assert!(deliveries.first_time_at(start, "app", "ORDERS_CREATE", "id-1", "another-sig"));
        // Forgotten after the hour, and it is a first time again.
        let later = start + REMEMBER;
        assert!(deliveries.first_time_at(later, "app", "ORDERS_CREATE", "id-1", "sig"));
        assert!(!deliveries.first_time_at(later, "app", "ORDERS_CREATE", "id-1", "sig"));
    }

    #[test]
    fn the_memory_of_deliveries_is_bounded() {
        let deliveries = WebhookDeliveries::default();
        let start = Instant::now();
        for index in 0..MAX_REMEMBERED + 10 {
            let now = start + Duration::from_millis(index as u64);
            assert!(deliveries.first_time_at(now, "app", "T", &index.to_string(), "sig"));
        }
        let size = deliveries.seen.lock().unwrap().len();
        assert!(size <= MAX_REMEMBERED, "{size}");
        // The oldest were forgotten, the latest are not.
        let now = start + Duration::from_millis(MAX_REMEMBERED as u64 + 20);
        assert!(!deliveries.first_time_at(now, "app", "T", &(MAX_REMEMBERED + 9).to_string(), "sig"));
        assert!(deliveries.first_time_at(now, "app", "T", "0", "sig"));
    }

    #[test]
    fn normalizes_topics() {
        assert_eq!(normalize_topic("orders/create"), "ORDERS_CREATE");
        assert_eq!(normalize_topic("app/uninstalled"), "APP_UNINSTALLED");
        assert_eq!(normalize_topic("ORDERS_CREATE"), "ORDERS_CREATE");
    }
}
