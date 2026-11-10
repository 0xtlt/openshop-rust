//! Webhook deliveries, signed and sent as Shopify sends them.

use anyhow::Result;
use chrono::SecondsFormat;
use indexmap::IndexMap;
use serde::Serialize;
use uuid::Uuid;

use super::{TestContext, sign_webhook_body};
use crate::shopify::ApiVersion;

/// The header of the signature of the body.
const SIGNATURE: &str = "X-Shopify-Hmac-Sha256";

/// A webhook delivery to the application, `ctx.webhook(topic, payload)`: the
/// payload as its body, signed with the secret of the app, and the headers
/// Shopify sends with it. Finish it with [`send`](Self::send).
pub struct WebhookRequestBuilder {
    http: reqwest::Client,
    base_url: String,
    path: String,
    body: serde_json::Result<Vec<u8>>,
    secret: String,
    /// By lowercase name, in the order they are sent.
    headers: IndexMap<String, (String, String)>,
}

impl WebhookRequestBuilder {
    /// Sends the delivery to `path` instead of `/webhooks`: another route of
    /// the application that takes webhooks, `/auth/webhooks/app-uninstalled`.
    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }

    /// The shop of `X-Shopify-Shop-Domain`, instead of the one of the test.
    pub fn shop(self, shop: impl Into<String>) -> Self {
        self.header("X-Shopify-Shop-Domain", shop)
    }

    /// Signs the body with `secret` instead of the secret of the test: the
    /// secret of another app.
    pub fn secret(mut self, secret: impl Into<String>) -> Self {
        self.secret = secret.into();
        self
    }

    /// Sends `signature` as `X-Shopify-Hmac-Sha256`, as it is, instead of the
    /// signature of the body: `.signature("forged")` is a delivery the
    /// application refuses with `401`.
    pub fn signature(self, signature: impl Into<String>) -> Self {
        self.header(SIGNATURE, signature)
    }

    /// The `X-Shopify-Webhook-Id` of the delivery, new for each one by
    /// default: a delivery sent again with the same id and body is one that
    /// Shopify retries.
    pub fn webhook_id(self, id: impl Into<String>) -> Self {
        self.header("X-Shopify-Webhook-Id", id)
    }

    /// Sets a header, in place of the one of the same name the delivery has.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        let name = name.into();
        self.headers.insert(name.to_ascii_lowercase(), (name, value.into()));
        self
    }

    /// Sends the delivery, and returns the response of the application.
    pub async fn send(self) -> Result<reqwest::Response> {
        let Self {
            http,
            base_url,
            path,
            body,
            secret,
            mut headers,
        } = self;
        let body = body?;
        headers
            .entry(SIGNATURE.to_ascii_lowercase())
            .or_insert_with(|| (SIGNATURE.to_string(), sign_webhook_body(&body, &secret)));
        let mut request = http.post(format!("{base_url}{path}"));
        for (name, value) in headers.values() {
            request = request.header(name, value);
        }
        Ok(request.body(body).send().await?)
    }
}

impl TestContext {
    /// A delivery of the webhook `topic` (`orders/create`) to `/webhooks`,
    /// with `payload` as its JSON body, signed with the secret of the test,
    /// and the headers Shopify sends: `X-Shopify-Topic`,
    /// `X-Shopify-Shop-Domain` (the shop of the test),
    /// `X-Shopify-Hmac-Sha256`, `X-Shopify-Webhook-Id`, `X-Shopify-Event-Id`
    /// (new ones), `X-Shopify-API-Version` (the version of the Admin API the
    /// application calls) and `X-Shopify-Triggered-At` (now). No session
    /// token.
    ///
    /// ```rust,ignore
    /// let response = ctx.webhook("orders/create", json!({ "id": 1 })).send().await?;
    /// assert_eq!(response.status(), 200);
    /// ```
    pub fn webhook(&self, topic: &str, payload: impl Serialize) -> WebhookRequestBuilder {
        let builder = WebhookRequestBuilder {
            http: self.http.clone(),
            base_url: self.url.clone(),
            path: "/webhooks".to_string(),
            body: serde_json::to_vec(&payload),
            secret: self.secret.clone(),
            headers: IndexMap::new(),
        };
        builder
            .header("Content-Type", "application/json")
            .header("X-Shopify-Topic", topic)
            .header("X-Shopify-Shop-Domain", &self.shop)
            .header("X-Shopify-API-Version", ApiVersion::DEFAULT.to_string())
            .header("X-Shopify-Webhook-Id", Uuid::new_v4().to_string())
            .header("X-Shopify-Event-Id", Uuid::new_v4().to_string())
            .header(
                "X-Shopify-Triggered-At",
                chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true),
            )
    }
}
