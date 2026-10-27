//! Test harness for OpenShop applications (enable the `testing` feature).
//!
//! [`TestContext`] starts the application on a random port against an isolated
//! PostgreSQL schema, replaces provider methods with recording fakes and signs
//! requests the way Shopify does.

mod admin;
mod context;
mod db;
mod factory;
mod fake;
mod transport;

pub use admin::{AdminTestOptions, admin_context};
pub use context::{
    IntoTestContext, ProxyClient, ProxyRequestBuilder, ProxyResponse, StartedTestContext, TestContext, TestOptions,
};
pub use db::{TestDb, test_database_url};
#[cfg(test)]
pub(crate) use db::connect;
pub use factory::{Factory, FactoryScope};
pub use fake::{FakeCall, FakeMethod, FakeProviders, TypedCall, TypedFake};
pub use transport::{FORM_CONTENT_TYPE, MockTransport, OutboundRequest, OutboundResponse};

pub use crate::admin::invoke_admin_function;

/// The runtime `#[openshop::test]` runs a test on.
#[doc(hidden)]
pub use tokio;

use indexmap::IndexMap;
use serde_json::json;
use shopify_api::oauth::ShopifyApp;

/// What a `#[openshop::test]` function can return: nothing, or a `Result`
/// whose error fails the test.
pub trait TestOutcome {
    /// Panics when the outcome is a failure.
    fn check(self);
}

impl TestOutcome for () {
    fn check(self) {}
}

impl<E: std::fmt::Debug> TestOutcome for Result<(), E> {
    fn check(self) {
        if let Err(error) = self {
            panic!("the test returned an error: {error:?}");
        }
    }
}

/// Signs a session token the way Shopify App Bridge does, with the claims you choose.
pub fn sign_session_token(claims: &serde_json::Value, secret: &str) -> String {
    ShopifyApp::new("", secret).sign_session_token(claims)
}

/// A session token as issued by Shopify App Bridge, valid for one minute.
pub fn session_token(shop: &str, secret: &str, api_key: &str, sub: &str) -> String {
    let now = chrono::Utc::now().timestamp();
    sign_session_token(
        &json!({
            "iss": format!("https://{shop}/admin"),
            "dest": format!("https://{shop}"),
            "aud": api_key,
            "sub": sub,
            "exp": now + 60,
            "nbf": now - 10,
            "iat": now,
            "jti": "test-jti",
            "sid": "test-sid",
        }),
        secret,
    )
}

/// The `X-Shopify-Hmac-Sha256` header value for a webhook body.
pub fn sign_webhook_body(body: impl AsRef<[u8]>, secret: &str) -> String {
    shopify_api::webhooks::sign(secret, body.as_ref())
}

/// Adds the `hmac` parameter Shopify appends to OAuth and app-launch URLs,
/// and the `timestamp` of now when `params` has none: a signed query is
/// taken for 90 seconds.
pub fn sign_query(params: &[(&str, &str)], secret: &str) -> IndexMap<String, String> {
    let mut query: IndexMap<String, String> = params
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    query
        .entry("timestamp".to_string())
        .or_insert_with(|| chrono::Utc::now().timestamp().to_string());
    let hmac = ShopifyApp::new("", secret)
        .sign_query(&to_query_string(&query))
        .expect("a query string");
    query.insert("hmac".to_string(), hmac);
    query
}

/// The `signature` Shopify gives an app proxy request with the parameters of
/// `query`.
pub fn proxy_signature(query: &IndexMap<String, String>, secret: &str) -> String {
    ShopifyApp::new("", secret)
        .sign_app_proxy(&to_query_string(query))
        .expect("a query string")
}

/// Serializes a query map for a URL.
pub fn to_query_string(query: &IndexMap<String, String>) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(query)
        .finish()
}
