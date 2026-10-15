//! The client credentials mode: the shop of `SHOPIFY_CLIENT_CREDENTIALS_SHOP`
//! gets its access token from Shopify with no OAuth, renews it before it
//! expires and when the Admin API refuses it.

mod common;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use chrono::{TimeDelta, Utc};
use common::{create_config, noop_flow};
use openshop::runtime::{FORM_CONTENT_TYPE, OutboundRequest};
use openshop::server::ServerOptions;
use openshop::testing::{MockTransport, TestContext, TestOptions, sign_query, to_query_string};
use openshop::{Value, json};
use reqwest::StatusCode;
use reqwest::header::{CONTENT_SECURITY_POLICY, LOCATION};

const SHOP: &str = "dev-store-1.myshopify.com";
const API_KEY: &str = "dev-app-key";
const SECRET: &str = "dev-app-secret";
const ENCRYPTION_KEY: &str = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
const QUERY: &str = "query { shop { name } }";

/// Shopify as the tests script it: a token for each client credentials
/// request, and an Admin API that refuses the tokens it was told to.
#[derive(Default)]
struct Shopify {
    issued: Mutex<u32>,
    revoked: Mutex<HashSet<String>>,
    refusal: Mutex<Option<(u16, Value)>>,
}

impl Shopify {
    fn install(self: &Arc<Self>, ctx: &TestContext) {
        let shopify = self.clone();
        ctx.transport.respond_with(move |request| Ok(shopify.answer(request)));
    }

    fn answer(&self, request: &OutboundRequest) -> openshop::runtime::OutboundResponse {
        if request.url.ends_with("/admin/oauth/access_token") {
            if let Some((status, body)) = self.refusal.lock().unwrap().clone() {
                return MockTransport::json(status, body);
            }
            let mut issued = self.issued.lock().unwrap();
            *issued += 1;
            return MockTransport::json(
                200,
                json!({
                    "access_token": format!("cc-token-{issued}"),
                    "scope": "read_products,write_products",
                    "expires_in": 86399,
                }),
            );
        }
        let token = header(request, "X-Shopify-Access-Token").unwrap_or_default();
        if self.revoked.lock().unwrap().contains(token) {
            return MockTransport::json(401, json!({ "errors": "[API] Invalid API key or access token" }));
        }
        MockTransport::json(200, json!({ "data": { "shop": { "name": "Dev store 1" } } }))
    }

    fn revoke(&self, token: &str) {
        self.revoked.lock().unwrap().insert(token.to_string());
    }

    fn refuse(&self, status: u16, body: Value) {
        *self.refusal.lock().unwrap() = Some((status, body));
    }
}

fn header<'a>(request: &'a OutboundRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn options(client_credentials_shop: Option<&str>) -> TestOptions {
    TestOptions {
        shop: SHOP.to_string(),
        api_key: API_KEY.to_string(),
        secret: SECRET.to_string(),
        encryption_key: Some(ENCRYPTION_KEY.to_string()),
        client_credentials_shop: client_credentials_shop.map(str::to_string),
        server: ServerOptions::production(),
        ..Default::default()
    }
}

/// A development store: the mode is on for its shop, which has no installation yet.
async fn dev_store() -> TestContext {
    TestContext::with_options(create_config([noop_flow("dev-store-flow")]), options(Some(SHOP)))
        .await
        .unwrap()
}

/// The same application without the mode.
async fn without_mode() -> TestContext {
    TestContext::with_options(create_config([noop_flow("dev-store-flow")]), options(None))
        .await
        .unwrap()
}

async fn shop_name(ctx: &TestContext) -> openshop::Result<Value> {
    ctx.app.shopify_client(SHOP, "default").graphql(QUERY, None).await
}

fn token_requests(ctx: &TestContext) -> Vec<OutboundRequest> {
    ctx.transport
        .requests()
        .into_iter()
        .filter(|request| request.url.ends_with("/admin/oauth/access_token"))
        .collect()
}

fn graphql_tokens(ctx: &TestContext) -> Vec<String> {
    ctx.transport
        .requests()
        .iter()
        .filter(|request| request.url.ends_with("/graphql.json"))
        .map(|request| {
            header(request, "X-Shopify-Access-Token")
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

type Stored = (Option<String>, Option<chrono::DateTime<Utc>>, Option<String>, bool);

/// The stored token, its expiry, the scopes and whether the shop is uninstalled.
async fn stored(ctx: &TestContext) -> Option<Stored> {
    sqlx::query_as(
        "SELECT access_token, access_token_expires_at, scopes, uninstalled_at IS NOT NULL \
         FROM installations WHERE app_handle = 'default' AND shop = $1",
    )
    .bind(SHOP)
    .fetch_optional(&ctx.db)
    .await
    .unwrap()
}

async fn expire_in(ctx: &TestContext, delta: TimeDelta) {
    sqlx::query("UPDATE installations SET access_token_expires_at = $1 WHERE shop = $2")
        .bind(Utc::now() + delta)
        .bind(SHOP)
        .execute(&ctx.db)
        .await
        .unwrap();
}

/// The path Shopify admin opens: launch parameters signed with the app secret.
fn signed_launch(shop: &str) -> String {
    let query = sign_query(
        &[
            ("host", "admin.shopify.com/store/dev-store-1"),
            ("shop", shop),
            ("timestamp", "1770000000"),
        ],
        SECRET,
    );
    format!("/?{}", to_query_string(&query))
}

// ─── Tokens ──────────────────────────────────────────────────────────

#[openshop::test(dev_store())]
async fn a_shop_without_a_token_gets_one_with_client_credentials(ctx: &TestContext) {
    let shopify = Arc::new(Shopify::default());
    shopify.install(ctx);

    let data = shop_name(ctx).await.unwrap();
    assert_eq!(data["shop"]["name"], "Dev store 1");

    let requests = token_requests(ctx);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, format!("https://{SHOP}/admin/oauth/access_token"));
    assert_eq!(header(&requests[0], "Content-Type"), Some(FORM_CONTENT_TYPE));
    assert_eq!(
        requests[0].body,
        json!({ "grant_type": "client_credentials", "client_id": API_KEY, "client_secret": SECRET })
    );
    assert_eq!(graphql_tokens(ctx), ["cc-token-1"]);

    let (token, expires_at, scopes, uninstalled) = stored(ctx).await.expect("installation");
    let token = token.unwrap();
    assert!(token.starts_with("enc:aes-256-gcm:"), "the token is stored encrypted");
    assert_eq!(
        ctx.app.crypto().decrypt_string(Some(&token)).unwrap().as_deref(),
        Some("cc-token-1")
    );
    let lifetime = expires_at.unwrap() - Utc::now();
    assert!(lifetime > TimeDelta::seconds(86_300) && lifetime <= TimeDelta::seconds(86_399));
    assert_eq!(scopes.as_deref(), Some("read_products,write_products"));
    assert!(!uninstalled);
}

#[openshop::test(dev_store())]
async fn the_token_is_reused_until_shortly_before_it_expires(ctx: &TestContext) {
    let shopify = Arc::new(Shopify::default());
    shopify.install(ctx);

    // Each run, webhook or request has a client of its own: they share the stored token.
    for _ in 0..3 {
        shop_name(ctx).await.unwrap();
    }
    assert_eq!(token_requests(ctx).len(), 1);
    assert_eq!(graphql_tokens(ctx), ["cc-token-1"; 3]);

    // Six minutes left: still used.
    expire_in(ctx, TimeDelta::minutes(6)).await;
    shop_name(ctx).await.unwrap();
    assert_eq!(token_requests(ctx).len(), 1);

    // Four minutes left: renewed before Shopify refuses it.
    expire_in(ctx, TimeDelta::minutes(4)).await;
    shop_name(ctx).await.unwrap();
    assert_eq!(token_requests(ctx).len(), 2);
    assert_eq!(graphql_tokens(ctx).last().map(String::as_str), Some("cc-token-2"));

    // Expired: renewed too.
    expire_in(ctx, TimeDelta::minutes(-1)).await;
    shop_name(ctx).await.unwrap();
    assert_eq!(token_requests(ctx).len(), 3);
    assert_eq!(graphql_tokens(ctx).last().map(String::as_str), Some("cc-token-3"));
}

#[openshop::test(dev_store())]
async fn concurrent_requests_ask_shopify_for_one_token(ctx: &TestContext) {
    let shopify = Arc::new(Shopify::default());
    shopify.install(ctx);

    let results = futures::future::join_all((0..8).map(|_| shop_name(ctx))).await;
    assert!(results.iter().all(Result::is_ok));
    assert_eq!(token_requests(ctx).len(), 1);
    assert_eq!(graphql_tokens(ctx), ["cc-token-1"; 8]);
}

#[openshop::test(dev_store())]
async fn a_token_the_admin_api_refuses_is_renewed_once_and_the_request_sent_again(ctx: &TestContext) {
    let shopify = Arc::new(Shopify::default());
    shopify.install(ctx);
    let client = ctx.app.shopify_client(SHOP, "default");
    client.graphql(QUERY, None).await.unwrap();

    // Shopify no longer takes the token, long before it expires.
    shopify.revoke("cc-token-1");
    let data = client.graphql(QUERY, None).await.unwrap();
    assert_eq!(data["shop"]["name"], "Dev store 1");
    assert_eq!(token_requests(ctx).len(), 2);
    assert_eq!(graphql_tokens(ctx), ["cc-token-1", "cc-token-1", "cc-token-2"]);

    // Another client, which never saw the refused token, reads the new one.
    shop_name(ctx).await.unwrap();
    assert_eq!(token_requests(ctx).len(), 2);

    // A new token refused as well is not renewed again: the 401 is the answer.
    shopify.revoke("cc-token-2");
    shopify.revoke("cc-token-3");
    ctx.transport.clear();
    let error = client.graphql(QUERY, None).await.unwrap_err().to_string();
    assert!(error.contains("Shopify GraphQL error (401)"), "{error}");
    assert_eq!(token_requests(ctx).len(), 1);
    assert_eq!(graphql_tokens(ctx), ["cc-token-2", "cc-token-3"]);
}

#[openshop::test(dev_store())]
async fn a_refusal_names_the_status_and_the_code_and_never_the_credentials(ctx: &TestContext) {
    let shopify = Arc::new(Shopify::default());
    shopify.install(ctx);
    shopify.refuse(
        400,
        json!({
            "error": "shop_not_permitted",
            "error_description": format!("{API_KEY} cannot use {SECRET} here"),
        }),
    );

    let error = format!("{:#}", shop_name(ctx).await.unwrap_err());
    assert_eq!(
        error,
        "[openshop] Shopify refused the client credentials of app \"default\" for dev-store-1.myshopify.com \
         (400: shop_not_permitted)"
    );
    assert!(!error.contains(SECRET) && !error.contains(API_KEY));
    assert!(graphql_tokens(ctx).is_empty());
    assert!(stored(ctx).await.is_none());

    // An answer without a code keeps the status alone.
    shopify.refuse(500, json!({ "errors": "Something went wrong, please retry." }));
    let error = shop_name(ctx).await.unwrap_err().to_string();
    assert!(error.ends_with("for dev-store-1.myshopify.com (500)"), "{error}");
}

#[openshop::test(dev_store())]
async fn an_installed_token_that_does_not_expire_is_used_as_it_is(ctx: &TestContext) {
    let shopify = Arc::new(Shopify::default());
    shopify.install(ctx);
    let token = ctx.app.crypto().encrypt_string("offline-token").unwrap();
    sqlx::query("INSERT INTO installations (shop, access_token) VALUES ($1, $2)")
        .bind(SHOP)
        .bind(token)
        .execute(&ctx.db)
        .await
        .unwrap();

    shop_name(ctx).await.unwrap();
    assert!(token_requests(ctx).is_empty());
    assert_eq!(graphql_tokens(ctx), ["offline-token"]);
}

// ─── Without the mode ────────────────────────────────────────────────

#[openshop::test(without_mode())]
async fn without_the_mode_a_shop_without_a_token_stays_without_one(ctx: &TestContext) {
    let shopify = Arc::new(Shopify::default());
    shopify.install(ctx);

    let error = shop_name(ctx).await.unwrap_err().to_string();
    assert!(error.contains("No access token found"), "{error}");
    assert!(ctx.transport.requests().is_empty());

    let reply = ctx
        .http
        .get(format!("{}{}", ctx.url, signed_launch(SHOP)))
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::FOUND);
    assert_eq!(
        reply.headers()[LOCATION].to_str().unwrap(),
        "/auth?shop=dev-store-1.myshopify.com"
    );
    assert!(ctx.transport.requests().is_empty());
}

#[openshop::test(without_mode())]
async fn without_the_mode_a_refused_token_is_not_renewed(ctx: &TestContext) {
    let shopify = Arc::new(Shopify::default());
    shopify.install(ctx);
    let token = ctx.app.crypto().encrypt_string("offline-token").unwrap();
    sqlx::query("INSERT INTO installations (shop, access_token) VALUES ($1, $2)")
        .bind(SHOP)
        .bind(token)
        .execute(&ctx.db)
        .await
        .unwrap();
    shopify.revoke("offline-token");

    let error = shop_name(ctx).await.unwrap_err().to_string();
    assert!(error.contains("Shopify GraphQL error (401)"), "{error}");
    assert!(token_requests(ctx).is_empty());
}

#[openshop::test(dev_store())]
async fn the_mode_serves_its_shop_only(ctx: &TestContext) {
    let shopify = Arc::new(Shopify::default());
    shopify.install(ctx);

    let error = ctx
        .app
        .shopify_client("dev-store-2.myshopify.com", "default")
        .graphql(QUERY, None)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("No access token found"), "{error}");
    assert!(ctx.transport.requests().is_empty());
}

#[tokio::test]
async fn a_value_that_is_not_a_shop_domain_stops_the_application() {
    let error = TestContext::with_options(create_config([]), options(Some("dev-store-1.example.com")))
        .await
        .err()
        .unwrap()
        .to_string();
    assert_eq!(
        error,
        "[openshop] SHOPIFY_CLIENT_CREDENTIALS_SHOP must be a *.myshopify.com domain"
    );
}

// ─── Admin launch ────────────────────────────────────────────────────

#[openshop::test(dev_store())]
async fn an_admin_launch_gets_the_token_instead_of_redirecting_to_oauth(ctx: &TestContext) {
    let shopify = Arc::new(Shopify::default());
    shopify.install(ctx);

    let reply = ctx
        .http
        .get(format!("{}{}", ctx.url, signed_launch(SHOP)))
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::OK);
    assert_eq!(
        reply.headers()[CONTENT_SECURITY_POLICY].to_str().unwrap(),
        "frame-ancestors https://dev-store-1.myshopify.com https://admin.shopify.com;"
    );
    assert_eq!(token_requests(ctx).len(), 1);
    assert!(stored(ctx).await.unwrap().0.is_some());

    // The next launch finds the shop installed.
    let reply = ctx
        .http
        .get(format!("{}{}", ctx.url, signed_launch(SHOP)))
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::OK);
    assert_eq!(token_requests(ctx).len(), 1);
}

#[openshop::test(dev_store())]
async fn an_admin_launch_shopify_refuses_falls_back_to_oauth(ctx: &TestContext) {
    let shopify = Arc::new(Shopify::default());
    shopify.install(ctx);
    shopify.refuse(400, json!({ "error": "shop_not_permitted" }));

    let reply = ctx
        .http
        .get(format!("{}{}", ctx.url, signed_launch(SHOP)))
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::FOUND);
    assert_eq!(
        reply.headers()[LOCATION].to_str().unwrap(),
        "/auth?shop=dev-store-1.myshopify.com"
    );
    assert!(stored(ctx).await.is_none());
}
