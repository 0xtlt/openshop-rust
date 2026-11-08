//! Shopify webhooks: `POST /webhooks`, dispatched by the `X-Shopify-Topic` header.
//!
//! `app/uninstalled` is recorded by OpenShop before its handler, if the
//! application has one, runs: the installation is marked uninstalled and its
//! access token deleted, as `POST /auth/webhooks/app-uninstalled` does.
//!
//! Shopify delivers a webhook at least once: a delivery it believes failed is
//! sent again, with the same `X-Shopify-Webhook-Id`. A delivery this process
//! has already run is acknowledged and not run again, within [`REMEMBER`].

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::header::RETRY_AFTER;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::post;
use serde_json::{Value, json};
use tokio::time::Instant;

use super::auth::{is_app_uninstalled, record_uninstall};
use super::http::{catch_panic, header, json_error, json_response, report_failure};
use crate::app::OpenShop;
use crate::runtime::context_logger::ContextLogger;
use crate::runtime::logger;
use crate::runtime::report;
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

/// What the process knows of a delivery.
#[derive(Clone, Copy)]
enum Known {
    /// A handler is running for it.
    Running(Instant),
    /// A handler completed.
    Done(Instant),
}

impl Known {
    fn at(self) -> Instant {
        match self {
            Self::Running(at) | Self::Done(at) => at,
        }
    }
}

/// The deliveries this process has run, or is running. It is in memory:
/// another replica, or a restart, does not know them, and a handler must still
/// be safe to run twice for a delivery (see the
/// [webhooks reference](../../../docs/src/reference/webhooks.md)).
#[derive(Default)]
pub(crate) struct WebhookDeliveries {
    seen: Mutex<HashMap<String, Known>>,
}

/// What [`WebhookDeliveries::claim`] found.
pub(crate) enum Claim<'a> {
    /// Nobody has run this delivery: the caller does, and says how it went.
    First(Delivery<'a>),
    /// A handler is running for it right now.
    Running,
    /// A handler completed for it.
    Done,
}

/// The claim on a delivery whose handler runs. Unless [`Delivery::complete`]
/// is called, the claim is given up when this is dropped: the handler failed,
/// or its request was cancelled (Shopify hung up), and the delivery that
/// Shopify sends again must run.
pub(crate) struct Delivery<'a> {
    deliveries: &'a WebhookDeliveries,
    key: String,
    started: Instant,
    completed: bool,
}

impl Delivery<'_> {
    /// The handler completed: the repeats of this delivery are not run.
    pub(crate) fn complete(mut self) {
        self.completed = true;
        self.deliveries.lock().insert(self.key.clone(), Known::Done(self.started));
    }
}

impl Drop for Delivery<'_> {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        // Only the claim that is still the running one is given up: one that
        // took too long was replaced by another.
        let mut seen = self.deliveries.lock();
        if matches!(seen.get(&self.key), Some(Known::Running(at)) if *at == self.started) {
            seen.remove(&self.key);
        }
    }
}

impl WebhookDeliveries {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Known>> {
        self.seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Takes the delivery to run it, unless it was run or is being run.
    ///
    /// The key names the app and the topic as well as the id, and carries the
    /// signature: the headers are not signed, so a delivery with an id that was
    /// not sent by Shopify with that body is another delivery.
    pub(crate) fn claim(&self, shopify_app: &str, topic: &str, id: &str, signature: &str) -> Claim<'_> {
        self.claim_at(Instant::now(), shopify_app, topic, id, signature)
    }

    fn claim_at(&self, now: Instant, shopify_app: &str, topic: &str, id: &str, signature: &str) -> Claim<'_> {
        let key = format!("{shopify_app}\n{topic}\n{id}\n{signature}");
        let mut seen = self.lock();
        match seen.get(&key) {
            Some(Known::Done(at)) if now.duration_since(*at) < REMEMBER => return Claim::Done,
            Some(Known::Running(at)) if now.duration_since(*at) < REMEMBER => return Claim::Running,
            _ => {}
        }
        if seen.len() >= MAX_REMEMBERED {
            seen.retain(|_, known| now.duration_since(known.at()) < REMEMBER);
            if seen.len() >= MAX_REMEMBERED {
                let oldest = seen.values().map(|known| known.at()).min();
                seen.retain(|_, known| Some(known.at()) != oldest);
            }
        }
        seen.insert(key.clone(), Known::Running(now));
        Claim::First(Delivery {
            deliveries: self,
            key,
            started: now,
            completed: false,
        })
    }
}

async fn handle(State(app): State<OpenShop>, headers: HeaderMap, body: Bytes) -> Response {
    let config = app.config();
    if !app.apps().has_configured_secret() {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "SHOPIFY_API_SECRET is not configured",
        );
    }
    let Ok((shopify_app, webhook)) = app.apps().by_webhook(&headers, &body) else {
        return json_error(StatusCode::UNAUTHORIZED, "Invalid webhook HMAC");
    };
    let read = |name: &str| header(&headers, name).unwrap_or_default().to_string();
    let hmac = read("x-shopify-hmac-sha256");
    let shop = read("x-shopify-shop-domain");
    let topic = webhook.topic().to_string();
    let api_version = webhook.api_version().unwrap_or_default().to_string();

    let ok = || json_response(StatusCode::OK, json!({ "ok": true }));
    // Shopify sends `orders/create`; handlers may also be registered as `ORDERS_CREATE`.
    let handler = config
        .webhooks
        .get(&topic)
        .or_else(|| config.webhooks.get(&webhook.topic_enum()));
    // `app/uninstalled` is OpenShop's to record, with or without a handler.
    let uninstalled = is_app_uninstalled(&topic);
    if handler.is_none() && !uninstalled {
        logger::warn(format!("[openshop] No webhook handler for topic \"{topic}\""));
        // Always return 200 to Shopify
        return ok();
    }

    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        return json_error(StatusCode::BAD_REQUEST, "Invalid JSON body");
    };
    // The signature covers the body, not this header: it is only taken as a
    // shop domain written in full, never a bare name.
    let Some(shop) = strict_shop_domain(&shop).filter(|domain| *domain == shop.trim().to_ascii_lowercase()) else {
        return json_error(StatusCode::BAD_REQUEST, "Invalid shop domain");
    };
    // An uninstallation is of the shop its signed payload names: one whose
    // header names another shop runs nothing, the handler included.
    if uninstalled && webhook.verify_payload_shop().is_err() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "The payload does not name the shop of X-Shopify-Shop-Domain",
        );
    }
    report::describe(|delivery| {
        delivery.unit("webhook", &topic).shop(&shop, &shopify_app.handle);
    });

    // A delivery Shopify sends again is acknowledged, not run again.
    let delivery_id = webhook
        .webhook_id()
        .or_else(|| webhook.event_id())
        .filter(|id| !id.is_empty());
    let claim = match delivery_id.map(|id| (id, app.webhook_deliveries().claim(&shopify_app.handle, &topic, id, &hmac))) {
        Some((id, Claim::Done)) => {
            logger::info(format!(
                "[openshop] Webhook \"{topic}\" delivery {id} was already handled: not run again"
            ));
            return ok();
        }
        // Not acknowledged: the handler may yet fail or be cancelled, and then
        // the delivery has not been handled. Shopify sends it again.
        Some((_, Claim::Running)) => {
            let mut response = json_error(StatusCode::SERVICE_UNAVAILABLE, "Delivery in progress");
            response.headers_mut().insert(RETRY_AFTER, HeaderValue::from_static("5"));
            return response;
        }
        Some((_, Claim::First(delivery))) => Some(delivery),
        None => None,
    };

    // The installation is marked uninstalled and its token deleted before the
    // handler runs. A failure is the server's: Shopify sends the delivery
    // again, and the claim is given up for it.
    if uninstalled {
        if let Err(error) = record_uninstall(&app, &shopify_app.handle, &webhook).await {
            report_failure(format!("[openshop] Uninstallation of {shop} could not be recorded"), &error);
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error");
        }
        logger::info(format!(
            "[openshop] {shop} uninstalled app \"{}\": its access token is deleted",
            shopify_app.handle
        ));
    }
    let Some(handler) = handler else {
        if let Some(delivery) = claim {
            delivery.complete();
        }
        return ok();
    };

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
        logger: ContextLogger::with_category(
            json!({ "shop": shop, "shopifyApp": shopify_app.handle, "topic": topic }),
            "webhook",
        ),
        app: app.clone(),
    };
    // Still return 200 on handler errors — don't let Shopify retry them.
    match catch_panic("handler", (handler.run)(context)).await {
        Err(error) => {
            report_failure(format!("[openshop] Webhook handler error for \"{topic}\""), &error);
            // A delivery whose handler failed is not handled: dropping the
            // claim lets a repeat of it run.
            drop(claim);
        }
        Ok(_) => {
            if let Some(delivery) = claim {
                delivery.complete();
            }
        }
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

    fn first<'a>(claim: Claim<'a>) -> Delivery<'a> {
        match claim {
            Claim::First(delivery) => delivery,
            Claim::Running => panic!("running"),
            Claim::Done => panic!("done"),
        }
    }

    #[test]
    fn a_delivery_is_run_once_within_the_time_it_is_remembered() {
        let deliveries = WebhookDeliveries::default();
        let start = Instant::now();
        first(deliveries.claim_at(start, "app", "ORDERS_CREATE", "id-1", "sig")).complete();
        assert!(matches!(
            deliveries.claim_at(start + Duration::from_secs(5), "app", "ORDERS_CREATE", "id-1", "sig"),
            Claim::Done
        ));
        // Another id, topic, app or body is another delivery.
        for (app, topic, id, signature) in [
            ("app", "ORDERS_CREATE", "id-2", "sig"),
            ("app", "ORDERS_PAID", "id-1", "sig"),
            ("other", "ORDERS_CREATE", "id-1", "sig"),
            ("app", "ORDERS_CREATE", "id-1", "another-sig"),
        ] {
            first(deliveries.claim_at(start, app, topic, id, signature)).complete();
        }
        // Forgotten after the hour, and it is a first time again.
        let later = start + REMEMBER;
        first(deliveries.claim_at(later, "app", "ORDERS_CREATE", "id-1", "sig")).complete();
        assert!(matches!(
            deliveries.claim_at(later, "app", "ORDERS_CREATE", "id-1", "sig"),
            Claim::Done
        ));
    }

    #[test]
    fn a_delivery_that_is_not_completed_can_be_run_again() {
        let deliveries = WebhookDeliveries::default();
        let start = Instant::now();
        let delivery = first(deliveries.claim_at(start, "app", "T", "id", "sig"));
        // While the handler runs, a repeat is neither run nor taken for handled.
        assert!(matches!(deliveries.claim_at(start, "app", "T", "id", "sig"), Claim::Running));
        // The handler failed, or its request was cancelled: the claim is dropped.
        drop(delivery);
        let delivery = first(deliveries.claim_at(start, "app", "T", "id", "sig"));
        delivery.complete();
        assert!(matches!(deliveries.claim_at(start, "app", "T", "id", "sig"), Claim::Done));

        // A handler that never ends does not hold the delivery for ever.
        let stuck = first(deliveries.claim_at(start, "app", "T", "stuck", "sig"));
        assert!(matches!(deliveries.claim_at(start, "app", "T", "stuck", "sig"), Claim::Running));
        first(deliveries.claim_at(start + REMEMBER, "app", "T", "stuck", "sig")).complete();
        drop(stuck);
    }

    #[test]
    fn the_memory_of_deliveries_is_bounded() {
        let deliveries = WebhookDeliveries::default();
        let start = Instant::now();
        for index in 0..MAX_REMEMBERED + 10 {
            let now = start + Duration::from_millis(index as u64);
            first(deliveries.claim_at(now, "app", "T", &index.to_string(), "sig")).complete();
        }
        let size = deliveries.lock().len();
        assert!(size <= MAX_REMEMBERED, "{size}");
        // The oldest were forgotten, the latest are not.
        let now = start + Duration::from_millis(MAX_REMEMBERED as u64 + 20);
        assert!(matches!(
            deliveries.claim_at(now, "app", "T", &(MAX_REMEMBERED + 9).to_string(), "sig"),
            Claim::Done
        ));
        first(deliveries.claim_at(now, "app", "T", "0", "sig")).complete();
    }
}
