//! Tokens obtained with Shopify's client credentials grant: for any shop
//! Shopify vouched for (a recent signed admin launch, a verified session
//! token), for the shop of the `client_credentials_shop` setting in background
//! work, and to renew the token a shop's row holds.

mod common;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::noop_flow;
use futures::StreamExt;
use openshop::builder::{AdminPage, Flow, Route, Webhook};
use openshop::engine::Scheduler;
use openshop::runtime::{RuntimeLogger, logger};
use openshop::server::http::parse_query;
use openshop::server::{ServerOptions, UiSource};
use openshop::testing::{
    AdminTestOptions, MockTransport, TestContext, TestOptions, admin_context, sign_query, sign_webhook_body,
    to_query_string,
};
use openshop::testing::{FORM_CONTENT_TYPE, OutboundRequest, OutboundResponse};
use openshop::{
    AdminContext, Cron, FlowContext, OpenShopConfig, ShopifyAppConfig, ShopifyConfig, Value, WebhookContext, json,
    response,
};
use openshop_shared::build_admin_function_rpc_url;
use reqwest::StatusCode;
use reqwest::header::{CONTENT_SECURITY_POLICY, LOCATION};

const SHOP: &str = "dev-store-1.myshopify.com";
const OTHER_SHOP: &str = "dev-store-2.myshopify.com";
const API_KEY: &str = "dev-app-key";
const SECRET: &str = "dev-app-secret";
const ENCRYPTION_KEY: &str = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
const QUERY: &str = "query { shop { name } }";
const EVERY_SECOND: &str = "* * * * * *";

/// Shopify as the tests script it: a token for each client credentials
/// request, one for each token exchange, and an Admin API that refuses the
/// tokens it was told to.
struct Shopify {
    issued: Mutex<u32>,
    expires_in: Mutex<Option<i64>>,
    revoked: Mutex<HashSet<String>>,
    refusal: Mutex<Option<(u16, Value)>>,
    exchange_refused: Mutex<bool>,
    /// Runs once, while Shopify answers the next client credentials request.
    during_grant: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Default for Shopify {
    fn default() -> Self {
        Self {
            issued: Mutex::new(0),
            expires_in: Mutex::new(Some(86_399)),
            revoked: Mutex::new(HashSet::new()),
            refusal: Mutex::new(None),
            exchange_refused: Mutex::new(false),
            during_grant: Mutex::new(None),
        }
    }
}

impl Shopify {
    fn install(ctx: &TestContext) -> Arc<Self> {
        let shopify = Arc::new(Self::default());
        let answering = shopify.clone();
        ctx.transport.respond_with(move |request| Ok(answering.answer(request)));
        shopify
    }

    fn answer(&self, request: &OutboundRequest) -> OutboundResponse {
        if request.url.ends_with("/admin/oauth/access_token") {
            if request.body["grant_type"] != "client_credentials" {
                if *self.exchange_refused.lock().unwrap() {
                    return MockTransport::json(400, json!({ "error": "invalid_subject_token" }));
                }
                return MockTransport::json(
                    200,
                    json!({ "access_token": "exchange-token", "scope": "read_products" }),
                );
            }
            if let Some(during_grant) = self.during_grant.lock().unwrap().take() {
                during_grant();
            }
            if let Some((status, body)) = self.refusal.lock().unwrap().clone() {
                return MockTransport::json(status, body);
            }
            let mut issued = self.issued.lock().unwrap();
            *issued += 1;
            let mut body =
                json!({ "access_token": format!("cc-token-{issued}"), "scope": "read_products,write_products" });
            if let Some(expires_in) = *self.expires_in.lock().unwrap() {
                body["expires_in"] = json!(expires_in);
            }
            return MockTransport::json(200, body);
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

    /// Shopify refuses the token exchange, as it may for a store the app is
    /// not installed on.
    fn refuse_exchange(&self) {
        *self.exchange_refused.lock().unwrap() = true;
    }

    fn expire_in(&self, expires_in: Option<i64>) {
        *self.expires_in.lock().unwrap() = expires_in;
    }

    /// Runs `sql` on the database of the test while Shopify answers the next
    /// client credentials request, as another request or a webhook would,
    /// on a connection of its own.
    fn write_during_grant(&self, ctx: &TestContext, sql: String) {
        let options = ctx.db.connect_options();
        *self.during_grant.lock().unwrap() = Some(Box::new(move || {
            std::thread::spawn(move || {
                tokio::runtime::Runtime::new().unwrap().block_on(async move {
                    let pool = sqlx::postgres::PgPoolOptions::new()
                        .max_connections(1)
                        .connect_with((*options).clone())
                        .await
                        .unwrap();
                    sqlx::query(&sql).execute(&pool).await.unwrap();
                    pool.close().await;
                });
            })
            .join()
            .unwrap();
        }));
    }
}

fn header<'a>(request: &'a OutboundRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// Calls the Admin API of `SHOP` in a handler; reports the outcome.
fn shop_route() -> Route {
    Route::public().get(|ctx| async move {
        let query = parse_query(ctx.request.uri.query());
        let shop = query.get("shop").cloned().unwrap_or_default();
        let outcome = match ctx.for_shop(shop.as_str()).await {
            Ok(shop_context) => match shop_context.shopify.graphql(QUERY, ()).await {
                Ok(data) => json!({ "data": data }),
                Err(error) => json!({ "error": error.to_string() }),
            },
            Err(error) => json!({ "error": error.to_string() }),
        };
        Ok(response::json(outcome))
    })
}

/// Reads the shop's name through the Admin API, from an admin page.
async fn admin_shop_name(ctx: AdminContext, _input: Value) -> openshop::Result<Value> {
    ctx.shopify.graphql(QUERY, ()).await
}

fn config(crons: &[Cron]) -> OpenShopConfig {
    let builder = OpenShopConfig::builder()
        .flow(noop_flow("dev-store-flow"))
        .flow(Flow::new("shop-name-flow", |ctx: FlowContext| async move {
            ctx.shopify.graphql(QUERY, ()).await?;
            Ok(())
        }))
        .admin_page(AdminPage::new("/tools").loader("shop", admin_shop_name))
        .route("/shop", shop_route())
        .webhook(
            "ORDERS_CREATE",
            Webhook::new(|ctx: WebhookContext| async move {
                ctx.shopify.graphql(QUERY, ()).await?;
                Ok(())
            }),
        );
    crons
        .iter()
        .fold(builder, |builder, cron| builder.cron(cron.clone()))
        .build()
        .expect("valid config")
}

fn options(client_credentials_shop: Option<&str>) -> TestOptions {
    TestOptions {
        shop: SHOP.to_string(),
        api_key: API_KEY.to_string(),
        secret: SECRET.to_string(),
        encryption_key: Some(ENCRYPTION_KEY.to_string()),
        client_credentials_shop: client_credentials_shop.map(str::to_string),
        server: ServerOptions {
            ui: UiSource::Embedded,
            dev: false,
        },
        ..Default::default()
    }
}

/// The `client_credentials_shop` setting names `SHOP`, which has no installation yet.
async fn configured() -> TestContext {
    TestContext::with_options(config(&[]), options(Some(SHOP)))
        .await
        .unwrap()
}

/// No `client_credentials_shop` setting.
async fn unconfigured() -> TestContext {
    TestContext::with_options(config(&[]), options(None)).await.unwrap()
}

async fn shop_name(ctx: &TestContext) -> openshop::Result<Value> {
    ctx.app.shopify_client(SHOP, "default").graphql(QUERY, ()).await
}

/// The client credentials requests, without the token exchanges.
fn grant_requests(ctx: &TestContext) -> Vec<OutboundRequest> {
    ctx.transport
        .requests()
        .into_iter()
        .filter(|request| request.body["grant_type"] == "client_credentials")
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

/// Every request went to `SHOP`, and nowhere else.
fn assert_only_shop_was_called(ctx: &TestContext) {
    for request in ctx.transport.requests() {
        assert!(
            request.url.starts_with(&format!("https://{SHOP}/admin/")),
            "{}",
            request.url
        );
    }
}

type Stored = (Option<String>, Option<String>, bool);

/// The stored token, the scopes and whether the shop is uninstalled.
async fn stored(ctx: &TestContext, shop: &str) -> Option<Stored> {
    sqlx::query_as(
        "SELECT access_token, scopes, uninstalled_at IS NOT NULL \
         FROM installations WHERE app_handle = 'default' AND shop = $1",
    )
    .bind(shop)
    .fetch_optional(&ctx.db)
    .await
    .unwrap()
}

fn decrypted(ctx: &TestContext, stored: &Stored) -> Option<String> {
    ctx.app.crypto().decrypt_string(stored.0.as_deref()).unwrap()
}

async fn install(ctx: &TestContext, shop: &str, token: Option<&str>, uninstalled: bool) {
    let token = token.map(|token| ctx.app.crypto().encrypt_string(token).unwrap());
    sqlx::query(
        "INSERT INTO installations (shop, access_token, scopes, uninstalled_at) \
         VALUES ($1, $2, 'read_orders', CASE WHEN $3 THEN now() END)",
    )
    .bind(shop)
    .bind(token)
    .bind(uninstalled)
    .execute(&ctx.db)
    .await
    .unwrap();
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The path Shopify admin opens: launch parameters signed with the app
/// secret at `timestamp`.
fn signed_launch(shop: &str, timestamp: i64) -> String {
    let query = sign_query(
        &[
            ("host", "YWRtaW4uc2hvcGlmeS5jb20vc3RvcmUvZGV2"),
            ("shop", shop),
            ("timestamp", &timestamp.to_string()),
        ],
        SECRET,
    );
    format!("/?{}", to_query_string(&query))
}

/// A launch from Shopify admin, signed now.
async fn launch(ctx: &TestContext, shop: &str) -> reqwest::Response {
    launch_at(ctx, shop, now()).await
}

async fn launch_at(ctx: &TestContext, shop: &str, timestamp: i64) -> reqwest::Response {
    ctx.http
        .get(ctx.url_for(&signed_launch(shop, timestamp)))
        .send()
        .await
        .unwrap()
}

/// Lets `duration` pass for the process, as a timer sees it.
async fn wait(duration: Duration) {
    tokio::time::pause();
    tokio::time::advance(duration).await;
    tokio::time::resume();
}

async fn webhook(ctx: &TestContext, shop: &str) -> reqwest::Response {
    let body = json!({ "id": 1 }).to_string();
    ctx.http
        .post(ctx.url_for("/webhooks"))
        .header("content-type", "application/json")
        .header("x-shopify-hmac-sha256", sign_webhook_body(&body, SECRET))
        .header("x-shopify-topic", "orders/create")
        .header("x-shopify-shop-domain", shop)
        .body(body)
        .send()
        .await
        .unwrap()
}

async fn route(ctx: &TestContext, shop: &str) -> Value {
    ctx.http
        .get(ctx.url_for("/routes/shop"))
        .query(&[("shop", shop)])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

// ─── Background work: the configured shop ────────────────────────────

#[openshop::test(configured())]
async fn background_work_gets_a_token_for_the_configured_shop(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);

    let data = shop_name(ctx).await.unwrap();
    assert_eq!(data["shop"]["name"], "Dev store 1");

    let requests = grant_requests(ctx);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, format!("https://{SHOP}/admin/oauth/access_token"));
    assert_eq!(header(&requests[0], "Content-Type"), Some(FORM_CONTENT_TYPE));
    assert_eq!(
        requests[0].body,
        json!({ "grant_type": "client_credentials", "client_id": API_KEY, "client_secret": SECRET })
    );
    assert_eq!(graphql_tokens(ctx), ["cc-token-1"]);

    let row = stored(ctx, SHOP).await.expect("installation");
    assert!(
        row.0.as_deref().unwrap().starts_with("enc:aes-256-gcm:"),
        "the token is stored encrypted"
    );
    assert_eq!(decrypted(ctx, &row).as_deref(), Some("cc-token-1"));
    assert_eq!(row.1.as_deref(), Some("read_products,write_products"));
    assert!(!row.2);

    // Each run, request or handler has a client of its own: they share the stored token.
    for _ in 0..3 {
        shop_name(ctx).await.unwrap();
    }
    assert_eq!(grant_requests(ctx).len(), 1);
}

#[openshop::test(configured())]
async fn a_token_this_process_obtained_is_renewed_five_minutes_before_it_expires(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);

    // Shopify gives a token for four minutes: within the margin, the next client renews it.
    shopify.expire_in(Some(240));
    shop_name(ctx).await.unwrap();
    shop_name(ctx).await.unwrap();
    assert_eq!(grant_requests(ctx).len(), 2);
    assert_eq!(graphql_tokens(ctx), ["cc-token-1", "cc-token-2"]);

    // A day: kept.
    shopify.expire_in(Some(86_399));
    shop_name(ctx).await.unwrap();
    assert_eq!(grant_requests(ctx).len(), 3);
    shop_name(ctx).await.unwrap();
    shop_name(ctx).await.unwrap();
    assert_eq!(grant_requests(ctx).len(), 3);
    assert_eq!(graphql_tokens(ctx)[2..], ["cc-token-3", "cc-token-3", "cc-token-3"]);
}

#[openshop::test(configured())]
async fn without_expires_in_a_token_is_only_renewed_when_the_admin_api_refuses_it(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);
    shopify.expire_in(None);

    for _ in 0..3 {
        shop_name(ctx).await.unwrap();
    }
    assert_eq!(grant_requests(ctx).len(), 1);

    shopify.revoke("cc-token-1");
    shop_name(ctx).await.unwrap();
    assert_eq!(grant_requests(ctx).len(), 2);
    assert_eq!(
        graphql_tokens(ctx),
        ["cc-token-1", "cc-token-1", "cc-token-1", "cc-token-1", "cc-token-2"]
    );
}

#[openshop::test(configured())]
async fn concurrent_requests_ask_shopify_for_one_token(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);

    let results = futures::future::join_all((0..8).map(|_| shop_name(ctx))).await;
    assert!(results.iter().all(Result::is_ok));
    assert_eq!(grant_requests(ctx).len(), 1);
    assert_eq!(graphql_tokens(ctx), ["cc-token-1"; 8]);
}

#[openshop::test(configured())]
async fn a_refused_token_is_renewed_once_and_the_request_sent_again_once(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);
    let client = ctx.app.shopify_client(SHOP, "default");
    client.graphql(QUERY, ()).await.unwrap();

    // Shopify no longer takes the token, long before it expires.
    shopify.revoke("cc-token-1");
    let data = client.graphql(QUERY, ()).await.unwrap();
    assert_eq!(data["shop"]["name"], "Dev store 1");
    assert_eq!(grant_requests(ctx).len(), 2);
    assert_eq!(graphql_tokens(ctx), ["cc-token-1", "cc-token-1", "cc-token-2"]);

    // A new token refused as well is not renewed again: the 401 is the answer.
    shopify.revoke("cc-token-2");
    shopify.revoke("cc-token-3");
    ctx.transport.clear();
    let error = client.graphql(QUERY, ()).await.unwrap_err().to_string();
    assert!(error.contains("answered 401"), "{error}");
    assert_eq!(grant_requests(ctx).len(), 1);
    assert_eq!(graphql_tokens(ctx), ["cc-token-2", "cc-token-3"]);
}

#[openshop::test(configured())]
async fn when_the_grant_after_a_401_is_refused_the_401_is_the_answer(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);
    shop_name(ctx).await.unwrap();
    shopify.revoke("cc-token-1");
    shopify.refuse(400, json!({ "error": "shop_not_permitted" }));
    ctx.transport.clear();

    let error = shop_name(ctx).await.unwrap_err().to_string();
    assert!(error.contains("answered 401"), "{error}");
    assert_eq!(grant_requests(ctx).len(), 1);
    // Sent once more as without the grant, with the stored token.
    assert_eq!(graphql_tokens(ctx), ["cc-token-1", "cc-token-1"]);
    // The refused token stays stored: nothing replaced it.
    let row = stored(ctx, SHOP).await.unwrap();
    assert_eq!(decrypted(ctx, &row).as_deref(), Some("cc-token-1"));
}

#[openshop::test(configured())]
async fn a_refusal_falls_back_and_is_remembered(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);
    shopify.refuse(
        400,
        json!({
            "error": "shop_not_permitted",
            "error_description": format!("{API_KEY} cannot use {SECRET} here"),
        }),
    );

    // As without the grant: no token.
    let error = format!("{:#}", shop_name(ctx).await.unwrap_err());
    assert!(error.contains("No access token found"), "{error}");
    assert!(!error.contains(SECRET) && !error.contains(API_KEY), "{error}");
    assert_eq!(grant_requests(ctx).len(), 1);
    assert!(graphql_tokens(ctx).is_empty());
    assert!(stored(ctx, SHOP).await.is_none());

    // Within the hour, nothing asks Shopify again for this app and shop.
    for _ in 0..3 {
        assert!(shop_name(ctx).await.is_err());
    }
    let reply = route(ctx, SHOP).await;
    assert!(
        reply["error"].as_str().unwrap().contains("not actively installed"),
        "{reply}"
    );
    assert_eq!(grant_requests(ctx).len(), 1);
}

#[openshop::test(configured())]
async fn a_failed_request_holds_back_for_thirty_seconds(ctx: &TestContext) {
    ctx.transport
        .respond_with(|_| Err(openshop::anyhow!("connection reset")));

    for _ in 0..3 {
        let error = shop_name(ctx).await.unwrap_err().to_string();
        assert!(error.contains("No access token found"), "{error}");
    }
    assert_eq!(grant_requests(ctx).len(), 1);

    wait(Duration::from_secs(31)).await;
    let _shopify = Shopify::install(ctx);
    shop_name(ctx).await.unwrap();
    assert_eq!(grant_requests(ctx).len(), 2);
}

#[openshop::test(configured())]
async fn an_outage_holds_back_for_thirty_seconds_then_shopify_is_asked_again(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);
    shopify.refuse(503, json!({ "errors": "Service Unavailable" }));

    assert!(shop_name(ctx).await.is_err());
    wait(Duration::from_secs(20)).await;
    assert!(shop_name(ctx).await.is_err());
    assert_eq!(grant_requests(ctx).len(), 1);

    // Shopify is back: the next request after the backoff gets a token.
    *shopify.refusal.lock().unwrap() = None;
    wait(Duration::from_secs(11)).await;
    let data = shop_name(ctx).await.unwrap();
    assert_eq!(data["shop"]["name"], "Dev store 1");
    assert_eq!(grant_requests(ctx).len(), 2);
}

#[openshop::test(configured())]
async fn a_definite_refusal_is_remembered_for_an_hour(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);
    shopify.refuse(400, json!({ "error": "shop_not_permitted" }));

    assert!(shop_name(ctx).await.is_err());
    wait(Duration::from_secs(31)).await;
    assert!(shop_name(ctx).await.is_err());
    wait(Duration::from_secs(58 * 60)).await;
    assert!(shop_name(ctx).await.is_err());
    assert_eq!(grant_requests(ctx).len(), 1);

    wait(Duration::from_secs(2 * 60)).await;
    assert!(shop_name(ctx).await.is_err());
    assert_eq!(grant_requests(ctx).len(), 2);
}

#[openshop::test(configured())]
async fn a_refusal_in_background_work_does_not_hold_back_a_signed_launch(ctx: &TestContext) {
    // A cron asks before the app is installed on the store: refused.
    let shopify = Shopify::install(ctx);
    shopify.refuse(400, json!({ "error": "shop_not_permitted" }));
    assert!(shop_name(ctx).await.is_err());
    assert_eq!(grant_requests(ctx).len(), 1);

    // The developer installs the app, then opens it within the hour.
    *shopify.refusal.lock().unwrap() = None;
    wait(Duration::from_secs(10 * 60)).await;
    // Background work is still held back...
    assert!(shop_name(ctx).await.is_err());
    assert_eq!(grant_requests(ctx).len(), 1);
    // ...but the launch asks, gets a token and is served, not sent to `/auth`.
    let reply = launch(ctx, SHOP).await;
    assert_eq!(reply.status(), StatusCode::OK);
    assert_eq!(grant_requests(ctx).len(), 2);
    let row = stored(ctx, SHOP).await.unwrap();
    assert_eq!(decrypted(ctx, &row).as_deref(), Some("cc-token-1"));

    // The success clears the refusal: background work uses that token.
    shop_name(ctx).await.unwrap();
    assert_eq!(grant_requests(ctx).len(), 2);
    assert_eq!(graphql_tokens(ctx), ["cc-token-1"]);
}

#[openshop::test(configured())]
async fn a_refused_signed_launch_holds_back_launches_and_background_work_for_an_hour(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);
    shopify.refuse(400, json!({ "error": "shop_not_permitted" }));

    assert_eq!(launch(ctx, SHOP).await.status(), StatusCode::FOUND);
    assert_eq!(grant_requests(ctx).len(), 1);
    wait(Duration::from_secs(30 * 60)).await;
    assert_eq!(launch(ctx, SHOP).await.status(), StatusCode::FOUND);
    assert!(shop_name(ctx).await.is_err());
    assert_eq!(grant_requests(ctx).len(), 1);

    wait(Duration::from_secs(31 * 60)).await;
    assert_eq!(launch(ctx, SHOP).await.status(), StatusCode::FOUND);
    assert_eq!(grant_requests(ctx).len(), 2);
}

#[openshop::test(configured())]
async fn an_outage_in_background_work_does_not_send_a_later_launch_to_oauth(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);
    shopify.refuse(503, json!({}));
    assert!(shop_name(ctx).await.is_err());
    assert_eq!(grant_requests(ctx).len(), 1);

    // A launch is not held back by it, even within the thirty seconds.
    *shopify.refusal.lock().unwrap() = None;
    let reply = launch(ctx, SHOP).await;
    assert_eq!(reply.status(), StatusCode::OK);
    assert_eq!(grant_requests(ctx).len(), 2);
}

#[tokio::test]
async fn an_unusable_encryption_key_fails_clearly_and_is_not_remembered() {
    // A missing key stops the application from starting; a malformed one is
    // only found when it is used.
    let options = TestOptions {
        encryption_key: Some("too-short".to_string()),
        ..options(Some(SHOP))
    };
    let ctx = TestContext::with_options(config(&[]), options).await.unwrap();
    let _shopify = Shopify::install(&ctx);

    for _ in 0..2 {
        let error = format!("{:#}", shop_name(&ctx).await.unwrap_err());
        assert!(error.contains("ENCRYPTION_KEY must be 64 hex characters"), "{error}");
    }
    // Nothing was asked, nothing was stored in plaintext.
    assert!(ctx.transport.requests().is_empty());
    assert!(stored(&ctx, SHOP).await.is_none());
    ctx.shutdown().await.unwrap();
}

#[openshop::test(configured())]
async fn a_stored_token_is_used_as_it_is_and_never_replaced(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);
    install(ctx, SHOP, Some("offline-token"), false).await;

    for _ in 0..2 {
        shop_name(ctx).await.unwrap();
    }
    assert!(grant_requests(ctx).is_empty());
    assert_eq!(graphql_tokens(ctx), ["offline-token", "offline-token"]);
    let row = stored(ctx, SHOP).await.unwrap();
    assert_eq!(decrypted(ctx, &row).as_deref(), Some("offline-token"));
    assert_eq!(row.1.as_deref(), Some("read_orders"));
}

/// A test installed with `TestOptions::access_token` and the variable naming its shop.
async fn installed_and_configured() -> TestContext {
    let options = TestOptions {
        access_token: Some("test-access-token".to_string()),
        ..options(Some(SHOP))
    };
    TestContext::with_options(config(&[]), options).await.unwrap()
}

#[openshop::test(installed_and_configured())]
async fn the_token_of_test_options_is_used_as_an_installed_one(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);

    shop_name(ctx).await.unwrap();
    assert!(grant_requests(ctx).is_empty());
    assert_eq!(graphql_tokens(ctx), ["test-access-token"]);
    let row = stored(ctx, SHOP).await.unwrap();
    assert!(!row.2);
}

#[openshop::test(configured())]
async fn a_shop_marked_uninstalled_is_never_brought_back_by_background_work(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);
    install(ctx, SHOP, None, true).await;

    let error = shop_name(ctx).await.unwrap_err().to_string();
    assert!(error.contains("No access token found"), "{error}");
    let reply = route(ctx, SHOP).await;
    assert!(
        reply["error"].as_str().unwrap().contains("not actively installed"),
        "{reply}"
    );
    assert!(ctx.transport.requests().is_empty());
    assert!(stored(ctx, SHOP).await.unwrap().2);
}

#[openshop::test(configured())]
async fn background_work_serves_the_configured_shop_only(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);

    let error = ctx
        .app
        .shopify_client(OTHER_SHOP, "default")
        .graphql(QUERY, ())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("No access token found"), "{error}");
    assert!(ctx.transport.requests().is_empty());
}

#[openshop::test(unconfigured())]
async fn without_the_variable_background_work_only_renews_a_stored_token(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);

    let error = shop_name(ctx).await.unwrap_err().to_string();
    assert!(error.contains("No access token found"), "{error}");
    assert!(ctx.transport.requests().is_empty());

    // A stored token the Admin API refuses is renewed once: its row proves
    // an earlier installation.
    install(ctx, SHOP, Some("offline-token"), false).await;
    shopify.revoke("offline-token");
    shop_name(ctx).await.unwrap();
    assert_eq!(grant_requests(ctx).len(), 1);
    assert_eq!(graphql_tokens(ctx), ["offline-token", "cc-token-1"]);
    let row = stored(ctx, SHOP).await.unwrap();
    assert_eq!(decrypted(ctx, &row).as_deref(), Some("cc-token-1"));
    assert_eq!(row.1.as_deref(), Some("read_products,write_products"));
}

#[openshop::test(unconfigured())]
async fn a_row_marked_uninstalled_or_without_a_token_is_never_renewed_by_background_work(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);
    // A row without a token, as `/auth` leaves before its callback.
    install(ctx, SHOP, None, false).await;
    // A row marked uninstalled whose token was kept.
    install(ctx, OTHER_SHOP, Some("old-token"), true).await;
    shopify.revoke("old-token");

    assert!(shop_name(ctx).await.is_err());
    let error = ctx
        .app
        .shopify_client(OTHER_SHOP, "default")
        .graphql(QUERY, ())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("answered 401"), "{error}");
    assert!(grant_requests(ctx).is_empty());
    assert!(stored(ctx, OTHER_SHOP).await.unwrap().2);
}

#[openshop::test(unconfigured())]
async fn a_shop_granted_by_a_launch_keeps_working_in_background_once_its_token_expires(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);

    // A launch vouched for the shop: it got a token and a row.
    assert_eq!(launch(ctx, OTHER_SHOP).await.status(), StatusCode::OK);
    assert_eq!(grant_requests(ctx).len(), 1);

    // A day later, in another process, a flow of a cron uses the stored token.
    shopify.revoke("cc-token-1");
    ctx.run_flow_for("shop-name-flow", json!({}), OTHER_SHOP).await.unwrap();
    let (status, error): (String, Option<String>) =
        sqlx::query_as("SELECT status, error FROM flow_runs WHERE flow_name = 'shop-name-flow'")
            .fetch_one(&ctx.db)
            .await
            .unwrap();
    assert_eq!(status, "completed", "{error:?}");
    assert_eq!(grant_requests(ctx).len(), 2);
    assert_eq!(graphql_tokens(ctx), ["cc-token-1", "cc-token-2"]);
    let row = stored(ctx, OTHER_SHOP).await.unwrap();
    assert_eq!(decrypted(ctx, &row).as_deref(), Some("cc-token-2"));
}

#[tokio::test]
async fn a_value_that_is_not_a_shop_domain_stops_the_application() {
    for value in [
        "dev-store-1.example.com",
        "https://dev-store-1.myshopify.com",
        "dev-store-1.myshopify.com:8443",
    ] {
        let error = TestContext::with_options(config(&[]), options(Some(value)))
            .await
            .err()
            .unwrap()
            .to_string();
        assert_eq!(
            error,
            "[openshop] client_credentials_shop must be a *.myshopify.com domain"
        );
    }
}

// ─── The scheduler ───────────────────────────────────────────────────

async fn scheduled(configured_shop: Option<&str>) -> TestContext {
    let crons = [Cron::new(EVERY_SECOND, "dev-store-flow").all_shops()];
    TestContext::with_options(config(&crons), options(configured_shop))
        .await
        .unwrap()
}

async fn cron_runs(ctx: &TestContext) -> Vec<(String, String)> {
    sqlx::query_as("SELECT app_handle, shop FROM flow_runs WHERE flow_name = 'dev-store-flow'")
        .fetch_all(&ctx.db)
        .await
        .unwrap()
}

#[openshop::test(scheduled(Some(SHOP)))]
async fn a_cron_of_every_shop_targets_the_configured_shop_on_an_empty_database(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);
    let mut scheduler = Scheduler::start(&ctx.app).unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    while cron_runs(ctx).await.is_empty() {
        assert!(std::time::Instant::now() < deadline, "the cron queued no run");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    scheduler.stop();

    let runs = cron_runs(ctx).await;
    assert!(
        runs.iter().all(|run| *run == ("default".to_string(), SHOP.to_string())),
        "{runs:?}"
    );
    // The token was stored: later ticks find the shop installed.
    assert_eq!(grant_requests(ctx).len(), 1);
    assert!(stored(ctx, SHOP).await.unwrap().0.is_some());
}

#[openshop::test(scheduled(Some(SHOP)))]
async fn a_cron_of_every_shop_leaves_a_configured_shop_marked_uninstalled(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);
    install(ctx, SHOP, None, true).await;
    let mut scheduler = Scheduler::start(&ctx.app).unwrap();
    tokio::time::sleep(Duration::from_millis(2_200)).await;
    scheduler.stop();

    assert!(cron_runs(ctx).await.is_empty());
    assert!(ctx.transport.requests().is_empty());
    assert!(stored(ctx, SHOP).await.unwrap().2);
}

#[openshop::test(scheduled(Some(SHOP)))]
async fn a_cron_of_every_shop_skips_the_configured_shop_shopify_refuses(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);
    shopify.refuse(400, json!({ "error": "shop_not_permitted" }));
    let mut scheduler = Scheduler::start(&ctx.app).unwrap();
    tokio::time::sleep(Duration::from_millis(2_200)).await;
    scheduler.stop();

    assert!(cron_runs(ctx).await.is_empty());
    // Refused once, then held back for the hour.
    assert_eq!(grant_requests(ctx).len(), 1);
}

// ─── Signed admin launch ─────────────────────────────────────────────

#[openshop::test(unconfigured())]
async fn a_signed_launch_gets_a_token_instead_of_redirecting_to_oauth(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);

    // No variable: Shopify vouched for the shop with the signed query.
    let reply = launch(ctx, SHOP).await;
    assert_eq!(reply.status(), StatusCode::OK);
    assert_eq!(
        reply.headers()[CONTENT_SECURITY_POLICY].to_str().unwrap(),
        "frame-ancestors https://dev-store-1.myshopify.com https://admin.shopify.com;"
    );
    assert_eq!(grant_requests(ctx).len(), 1);
    let row = stored(ctx, SHOP).await.unwrap();
    assert_eq!(decrypted(ctx, &row).as_deref(), Some("cc-token-1"));

    // The next launch finds the shop installed, and background work its token.
    assert_eq!(launch(ctx, SHOP).await.status(), StatusCode::OK);
    shop_name(ctx).await.unwrap();
    assert_eq!(grant_requests(ctx).len(), 1);
    assert_eq!(graphql_tokens(ctx), ["cc-token-1"]);
}

#[openshop::test(unconfigured())]
async fn a_signed_launch_shopify_refuses_falls_back_to_oauth(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);
    shopify.refuse(400, json!({ "error": "shop_not_permitted" }));

    let replies = futures::future::join_all((0..20).map(|_| launch(ctx, SHOP))).await;
    for reply in replies {
        assert_eq!(reply.status(), StatusCode::FOUND);
        assert_eq!(
            reply.headers()[LOCATION].to_str().unwrap(),
            "/auth?shop=dev-store-1.myshopify.com"
        );
    }
    assert_eq!(grant_requests(ctx).len(), 1);
    assert!(stored(ctx, SHOP).await.is_none());
}

#[openshop::test(unconfigured())]
async fn concurrent_signed_launches_ask_shopify_for_one_token(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);

    let replies = futures::future::join_all((0..20).map(|_| launch(ctx, SHOP))).await;
    assert!(replies.iter().all(|reply| reply.status() == StatusCode::OK));
    assert_eq!(grant_requests(ctx).len(), 1);
}

#[openshop::test(unconfigured())]
async fn a_signed_launch_reinstalls_a_shop_marked_uninstalled(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);
    install(ctx, SHOP, None, true).await;

    assert_eq!(launch(ctx, SHOP).await.status(), StatusCode::OK);
    let row = stored(ctx, SHOP).await.unwrap();
    assert_eq!(decrypted(ctx, &row).as_deref(), Some("cc-token-1"));
    assert!(!row.2, "a signed launch brings the shop back");
    // Shopify gave scopes: they replace the stored ones.
    assert_eq!(row.1.as_deref(), Some("read_products,write_products"));
    // Installed again now, as OAuth does.
    let (recent,): (bool,) =
        sqlx::query_as("SELECT installed_at > now() - interval '1 minute' FROM installations WHERE shop = $1")
            .bind(SHOP)
            .fetch_one(&ctx.db)
            .await
            .unwrap();
    assert!(recent);
}

#[openshop::test(unconfigured())]
async fn a_signed_launch_older_than_ninety_seconds_never_asks(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);
    install(ctx, OTHER_SHOP, None, true).await;

    // Margins the clock cannot flip while the launch is sent: stale by 120 s
    // or more, fresh within 30 s.
    // `shopify_api` verifies no launch older than 90 seconds: it is replayed.
    for timestamp in [now() - 120, now() + 120, 1_770_000_000] {
        let reply = launch_at(ctx, SHOP, timestamp).await;
        assert_eq!(reply.status(), StatusCode::UNAUTHORIZED, "{timestamp}");
        // Nor brings back a shop marked uninstalled.
        assert_eq!(
            launch_at(ctx, OTHER_SHOP, timestamp).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
    assert!(ctx.transport.requests().is_empty());
    assert!(stored(ctx, SHOP).await.is_none());
    assert!(stored(ctx, OTHER_SHOP).await.unwrap().2);

    // Nor of an installed shop.
    install(ctx, SHOP, Some("offline-token"), false).await;
    assert_eq!(
        launch_at(ctx, SHOP, now() - 600).await.status(),
        StatusCode::UNAUTHORIZED
    );

    // A fresh one asks.
    assert_eq!(launch_at(ctx, OTHER_SHOP, now() - 30).await.status(), StatusCode::OK);
    assert_eq!(grant_requests(ctx).len(), 1);
}

#[openshop::test(unconfigured())]
async fn a_launch_without_a_valid_signature_never_asks(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);

    let unsigned = ctx
        .http
        .get(ctx.url_for(&format!("/?shop={SHOP}&hmac=00")))
        .send()
        .await
        .unwrap();
    assert_eq!(unsigned.status(), StatusCode::UNAUTHORIZED);
    assert!(ctx.transport.requests().is_empty());
}

// ─── Verified session token ──────────────────────────────────────────

/// The kinds of the token requests sent, in order.
fn token_requests(ctx: &TestContext) -> Vec<Value> {
    ctx.transport
        .requests()
        .iter()
        .filter(|request| request.url.ends_with("/admin/oauth/access_token"))
        .map(|request| request.body["grant_type"].clone())
        .collect()
}

const TOKEN_EXCHANGE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";

async fn api_request(ctx: &TestContext) {
    let reply = ctx
        .request_as(reqwest::Method::GET, "/api/flows", SHOP, "1")
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::OK);
}

#[openshop::test(unconfigured())]
async fn a_session_token_gets_the_token_of_the_exchange_without_a_grant(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);

    api_request(ctx).await;
    // The offline token of the exchange, which does not expire: no grant.
    assert_eq!(token_requests(ctx), [json!(TOKEN_EXCHANGE)]);
    let row = stored(ctx, SHOP).await.unwrap();
    assert_eq!(decrypted(ctx, &row).as_deref(), Some("exchange-token"));

    // Later requests use it.
    api_request(ctx).await;
    assert_eq!(token_requests(ctx).len(), 1);
}

#[openshop::test(unconfigured())]
async fn a_session_token_falls_back_to_the_grant_when_the_exchange_is_refused(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);
    shopify.refuse_exchange();

    api_request(ctx).await;
    assert_eq!(
        token_requests(ctx),
        [json!(TOKEN_EXCHANGE), json!("client_credentials")]
    );
    let row = stored(ctx, SHOP).await.unwrap();
    assert_eq!(decrypted(ctx, &row).as_deref(), Some("cc-token-1"));

    // The token stored, later requests ask nothing.
    api_request(ctx).await;
    assert_eq!(token_requests(ctx).len(), 2);
}

#[openshop::test(unconfigured())]
async fn a_session_token_replaces_an_expiring_grant_token_by_the_exchange(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);
    shopify.refuse_exchange();
    // Within the five minutes before it expires.
    shopify.expire_in(Some(240));
    api_request(ctx).await;
    assert_eq!(
        decrypted(ctx, &stored(ctx, SHOP).await.unwrap()).as_deref(),
        Some("cc-token-1")
    );

    // A refused exchange is not tried again for thirty seconds.
    *shopify.exchange_refused.lock().unwrap() = false;
    wait(Duration::from_secs(31)).await;
    api_request(ctx).await;
    assert_eq!(
        token_requests(ctx),
        [
            json!(TOKEN_EXCHANGE),
            json!("client_credentials"),
            json!(TOKEN_EXCHANGE)
        ]
    );
    assert_eq!(
        decrypted(ctx, &stored(ctx, SHOP).await.unwrap()).as_deref(),
        Some("exchange-token")
    );
}

#[openshop::test(unconfigured())]
async fn an_admin_handler_renews_a_token_the_admin_api_refuses(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);
    install(ctx, SHOP, Some("offline-token"), false).await;

    // The session's token was used, then refused.
    let url = build_admin_function_rpc_url("loader", "shop", "/tools");
    let call = || async {
        ctx.request_as(reqwest::Method::POST, &url, SHOP, "1")
            .json(&json!({ "input": null }))
            .send()
            .await
            .unwrap()
    };
    assert_eq!(call().await.status(), StatusCode::OK);
    shopify.revoke("offline-token");
    let reply = call().await;
    assert_eq!(reply.status(), StatusCode::OK);
    assert_eq!(reply.json::<Value>().await.unwrap()["shop"]["name"], "Dev store 1");
    assert_eq!(grant_requests(ctx).len(), 1);
    assert_eq!(graphql_tokens(ctx), ["offline-token", "offline-token", "cc-token-1"]);
    let row = stored(ctx, SHOP).await.unwrap();
    assert_eq!(decrypted(ctx, &row).as_deref(), Some("cc-token-1"));
}

#[openshop::test(unconfigured())]
async fn the_admin_context_of_tests_renews_a_refused_token_as_in_production(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);
    install(ctx, SHOP, Some("offline-token"), false).await;
    shopify.revoke("offline-token");

    let admin = admin_context(
        &ctx.app,
        AdminTestOptions {
            shop: SHOP.to_string(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let data = admin_shop_name(admin, Value::Null).await.unwrap();
    assert_eq!(data["shop"]["name"], "Dev store 1");
    assert_eq!(graphql_tokens(ctx), ["offline-token", "cc-token-1"]);
}

#[openshop::test(unconfigured())]
async fn a_vouched_client_pointed_at_another_shop_or_app_asks_nothing(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);
    let admin = admin_context(
        &ctx.app,
        AdminTestOptions {
            shop: SHOP.to_string(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // Shopify vouched for `SHOP`: another shop gets no token from it.
    let mut other_shop = admin.shopify.clone();
    other_shop.shop = OTHER_SHOP.to_string();
    let error = other_shop.graphql(QUERY, ()).await.unwrap_err().to_string();
    assert!(error.contains("No access token found"), "{error}");
    let mut other_app = admin.shopify.clone();
    other_app.shopify_app = "other".to_string();
    assert!(other_app.graphql(QUERY, ()).await.is_err());
    assert!(ctx.transport.requests().is_empty());
    assert!(stored(ctx, OTHER_SHOP).await.is_none());

    // The client itself still does.
    admin.shopify.graphql(QUERY, ()).await.unwrap();
    assert_eq!(grant_requests(ctx).len(), 1);
    assert_only_shop_was_called(ctx);
}

#[openshop::test(unconfigured())]
async fn a_clone_pointed_at_another_shop_sends_that_shop_its_own_token(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);
    install(ctx, SHOP, Some("first-shop-token"), false).await;
    install(ctx, OTHER_SHOP, Some("other-shop-token"), false).await;

    let client = ctx.app.shopify_client(SHOP, "default");
    client.graphql(QUERY, ()).await.unwrap();
    let mut other = client.clone();
    other.shop = OTHER_SHOP.to_string();
    other.graphql(QUERY, ()).await.unwrap();
    client.graphql(QUERY, ()).await.unwrap();

    let sent: Vec<(String, String)> = ctx
        .transport
        .requests()
        .iter()
        .map(|request| {
            (
                request.url.clone(),
                header(request, "X-Shopify-Access-Token")
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect();
    let graphql = |shop: &str| format!("https://{shop}/admin/api/2026-10/graphql.json");
    assert_eq!(
        sent,
        [
            (graphql(SHOP), "first-shop-token".to_string()),
            (graphql(OTHER_SHOP), "other-shop-token".to_string()),
            (graphql(SHOP), "first-shop-token".to_string()),
        ]
    );

    // A shop without a token gets nothing sent at all.
    let mut third = client.clone();
    third.shop = "dev-store-3.myshopify.com".to_string();
    assert!(third.graphql(QUERY, ()).await.is_err());
    assert_eq!(ctx.transport.requests().len(), 3);
}

// ─── Paths nothing vouches for ───────────────────────────────────────

#[openshop::test(unconfigured())]
async fn unsigned_paths_never_ask(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);
    // A shop installed without a token: a vouched grant would ask for it.
    install(ctx, SHOP, None, false).await;
    // A shop whose token the Admin API refuses: a background client would
    // renew it.
    install(ctx, OTHER_SHOP, Some("revoked-token"), false).await;
    shopify.revoke("revoked-token");

    // OAuth start: redirected to Shopify, as before.
    let reply = ctx
        .http
        .get(ctx.url_for(&format!("/auth?shop={SHOP}")))
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::FOUND);
    assert!(
        reply.headers()[LOCATION]
            .to_str()
            .unwrap()
            .starts_with(&format!("https://{SHOP}/admin/oauth/authorize?"))
    );

    // A route resolving the shop of its query.
    let reply = route(ctx, SHOP).await;
    assert!(
        reply["error"].as_str().unwrap().contains("not actively installed"),
        "{reply}"
    );

    // A flow that calls the Admin API of the shop, and the client of
    // background work: no token, and none asked.
    let outcome = ctx.run_flow_for("shop-name-flow", json!({}), SHOP).await.unwrap();
    assert!(
        matches!(&outcome, openshop::RunOutcome::Failed { error, .. } if error.contains("No access token")),
        "{outcome:?}"
    );
    assert!(shop_name(ctx).await.is_err());

    // A signed webhook whose handler calls the Admin API: its shop header is
    // not signed, so the shop without a token gets none.
    assert_eq!(webhook(ctx, SHOP).await.status(), StatusCode::OK);

    assert!(grant_requests(ctx).is_empty());
    assert!(graphql_tokens(ctx).is_empty());
    assert!(stored(ctx, SHOP).await.unwrap().0.is_none());
}

#[openshop::test(configured())]
async fn unknown_shops_through_a_route_leave_nothing_behind(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);

    // Many shops, sixteen at a time.
    let replies: Vec<Value> = futures::stream::iter(0..200)
        .map(|index| async move { route(ctx, &format!("unknown-{index}-{}.myshopify.com", "x".repeat(40))).await })
        .buffer_unordered(16)
        .collect()
        .await;
    for reply in replies {
        assert!(
            reply["error"].as_str().unwrap().contains("not actively installed"),
            "{reply}"
        );
    }
    // The process only remembers a shop it asked Shopify for (checked by
    // the unit tests of the grant): none was asked.
    assert!(ctx.transport.requests().is_empty());

    // A name longer than a DNS label is no shop.
    let reply = route(ctx, &format!("{}.myshopify.com", "a".repeat(64))).await;
    assert!(
        reply["error"].as_str().unwrap().contains("Invalid shop domain"),
        "{reply}"
    );
    assert!(ctx.transport.requests().is_empty());

    // The configured shop is asked for.
    route(ctx, SHOP).await;
    assert_eq!(grant_requests(ctx).len(), 1);
}

#[openshop::test(configured())]
async fn a_route_resolves_the_configured_shop_and_no_other(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);

    let reply = route(ctx, OTHER_SHOP).await;
    assert!(
        reply["error"].as_str().unwrap().contains("not actively installed"),
        "{reply}"
    );
    assert!(ctx.transport.requests().is_empty());

    let reply = route(ctx, SHOP).await;
    assert_eq!(reply["data"]["shop"]["name"], "Dev store 1", "{reply}");
    assert_eq!(grant_requests(ctx).len(), 1);
}

#[openshop::test(unconfigured())]
async fn a_webhook_never_asks_for_a_shop_without_a_row(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);

    // A shop without a row, from the header.
    assert_eq!(webhook(ctx, OTHER_SHOP).await.status(), StatusCode::OK);
    assert!(ctx.transport.requests().is_empty());
    assert!(stored(ctx, OTHER_SHOP).await.is_none());
}

#[openshop::test(configured())]
async fn a_webhook_asks_for_the_configured_shop_as_background_work_does(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);

    assert_eq!(webhook(ctx, SHOP).await.status(), StatusCode::OK);
    assert_eq!(grant_requests(ctx).len(), 1);
    assert_eq!(graphql_tokens(ctx), ["cc-token-1"]);
    // Any other shop without a row: nothing.
    assert_eq!(webhook(ctx, OTHER_SHOP).await.status(), StatusCode::OK);
    assert_eq!(grant_requests(ctx).len(), 1);
    assert!(stored(ctx, OTHER_SHOP).await.is_none());
}

#[openshop::test(unconfigured())]
async fn a_webhook_handler_renews_the_token_a_row_holds_once(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);
    // A shop a launch installed with the grant, in an earlier process: its
    // token expired since.
    install(ctx, OTHER_SHOP, Some("expired-token"), false).await;
    shopify.revoke("expired-token");

    // The handler's Admin API call is renewed once and sent again once,
    // which Shopify answers.
    assert_eq!(webhook(ctx, OTHER_SHOP).await.status(), StatusCode::OK);
    assert_eq!(grant_requests(ctx).len(), 1);
    assert_eq!(graphql_tokens(ctx), ["expired-token", "cc-token-1"]);
    let row = stored(ctx, OTHER_SHOP).await.unwrap();
    assert_eq!(decrypted(ctx, &row).as_deref(), Some("cc-token-1"));

    // A token Shopify refuses again: one grant, one retry, then the `401`.
    shopify.revoke("cc-token-1");
    shopify.revoke("cc-token-2");
    assert_eq!(webhook(ctx, OTHER_SHOP).await.status(), StatusCode::OK);
    assert_eq!(grant_requests(ctx).len(), 2);
    assert_eq!(graphql_tokens(ctx)[2..], ["cc-token-1", "cc-token-2"]);
}

#[openshop::test(configured())]
async fn a_webhook_header_that_is_not_a_full_shop_domain_never_names_the_configured_shop(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);

    // Refused by the verification of `shopify_api` (401), or by the check of
    // a full shop domain (400).
    for shop in ["__global__", "dev-store-1", " dev-store-1 ", ""] {
        let status = webhook(ctx, shop).await.status();
        assert!(matches!(status.as_u16(), 400 | 401), "{shop:?}: {status}");
    }
    assert!(ctx.transport.requests().is_empty());
}

// ─── Where a token goes ──────────────────────────────────────────────

const FOREIGN_SHOPS: [&str; 6] = [
    "evil.example://dev-store-1.myshopify.com",
    "https://evil.example/dev-store-1.myshopify.com",
    "dev-store-1.myshopify.com:8443",
    "evil.example@dev-store-1.myshopify.com",
    "dev-store-1.myshopify.com@evil.example",
    "dev-store-1.myshopify.com.",
];

#[openshop::test(configured())]
async fn a_client_sends_nothing_for_a_shop_that_is_not_a_shop_domain(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);

    for shop in FOREIGN_SHOPS {
        let error = ctx
            .app
            .shopify_client(shop, "default")
            .graphql(QUERY, ())
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("Invalid shop domain"), "{shop}: {error}");
    }
    assert!(ctx.transport.requests().is_empty());

    // Case and blanks aside, the configured shop is the configured shop.
    for shop in [
        "DEV-STORE-1.MYSHOPIFY.COM",
        " dev-store-1.myshopify.com ",
        "dev-store-1",
    ] {
        ctx.app
            .shopify_client(shop, "default")
            .graphql(QUERY, ())
            .await
            .unwrap();
    }
    assert_eq!(grant_requests(ctx).len(), 1);
    assert_only_shop_was_called(ctx);
}

#[openshop::test(configured())]
async fn a_signed_webhook_or_a_route_sends_a_token_to_the_shop_only(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);
    install(ctx, SHOP, Some("offline-token"), false).await;

    for shop in FOREIGN_SHOPS {
        let status = webhook(ctx, shop).await.status();
        assert!(matches!(status.as_u16(), 400 | 401), "{shop}: {status}");
    }
    assert!(ctx.transport.requests().is_empty());

    assert_eq!(webhook(ctx, "DEV-STORE-1.MYSHOPIFY.COM").await.status(), StatusCode::OK);
    for shop in FOREIGN_SHOPS {
        // A route reads a URL as the shop it names, and calls that shop alone.
        route(ctx, shop).await;
    }
    assert!(!graphql_tokens(ctx).is_empty());
    assert_only_shop_was_called(ctx);
}

#[openshop::test(unconfigured())]
async fn a_signed_launch_asks_the_shop_it_names_only(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);

    for shop in FOREIGN_SHOPS {
        launch(ctx, shop).await;
    }
    assert_only_shop_was_called(ctx);
}

// ─── Several Shopify apps ────────────────────────────────────────────

fn multi_app_config() -> OpenShopConfig {
    let app = |api_key: &str, api_secret: &str| ShopifyAppConfig::Keys {
        api_key: api_key.to_string(),
        api_secret: api_secret.to_string(),
        app_url: Some("https://app.example.test".to_string()),
    };
    OpenShopConfig::builder()
        .flow(noop_flow("dev-store-flow"))
        .shopify(ShopifyConfig {
            scopes: Some("read_products".to_string()),
            apps: Some(
                [
                    ("first".to_string(), app("first-key", "first-secret")),
                    ("second".to_string(), app("second-key", "second-secret")),
                ]
                .into(),
            ),
        })
        .build()
        .expect("valid config")
}

async fn multi_app() -> TestContext {
    TestContext::with_options(multi_app_config(), options(Some(SHOP)))
        .await
        .unwrap()
}

#[openshop::test(multi_app())]
async fn each_app_asks_with_its_own_credentials(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);

    // A launch signed by the second app asks with the second app's credentials.
    let timestamp = now().to_string();
    let query = sign_query(&[("shop", SHOP), ("timestamp", &timestamp)], "second-secret");
    let reply = ctx
        .http
        .get(ctx.url_for(&format!("/?{}", to_query_string(&query))))
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::OK);

    // Background work for the first app asks with the first app's credentials.
    ctx.app.shopify_client(SHOP, "first").graphql(QUERY, ()).await.unwrap();

    let bodies: Vec<Value> = grant_requests(ctx).into_iter().map(|request| request.body).collect();
    assert_eq!(
        bodies,
        [
            json!({ "grant_type": "client_credentials", "client_id": "second-key", "client_secret": "second-secret" }),
            json!({ "grant_type": "client_credentials", "client_id": "first-key", "client_secret": "first-secret" }),
        ]
    );
    let handles: Vec<(String,)> =
        sqlx::query_as("SELECT app_handle FROM installations WHERE shop = $1 ORDER BY app_handle")
            .bind(SHOP)
            .fetch_all(&ctx.db)
            .await
            .unwrap();
    assert_eq!(handles, [("first".to_string(),), ("second".to_string(),)]);

    // A refusal for one app holds back that app only.
    ctx.transport.clear();
    let shopify = Shopify::install(ctx);
    shopify.refuse(400, json!({ "error": "shop_not_permitted" }));
    sqlx::query("DELETE FROM installations").execute(&ctx.db).await.unwrap();
    for handle in ["first", "first", "second", "second"] {
        assert!(ctx.app.shopify_client(SHOP, handle).graphql(QUERY, ()).await.is_err());
    }
    assert_eq!(grant_requests(ctx).len(), 2);
}

// ─── What an error shows ─────────────────────────────────────────────

/// The logger is the process's: the tests that read it take turns.
static LOGGING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Keeps every line logged while it is installed.
#[derive(Default)]
struct Lines(Mutex<Vec<String>>);

impl RuntimeLogger for Lines {
    fn info(&self, message: &str, context: Option<&Value>) {
        self.0.lock().unwrap().push(format!("{message} {context:?}"));
    }
    fn warn(&self, message: &str, context: Option<&Value>) {
        self.info(message, context);
    }
    fn error(&self, message: &str, context: Option<&Value>) {
        self.info(message, context);
    }
}

/// The database refuses every token from now on, quoting it, as a
/// constraint would.
async fn refuse_tokens(ctx: &TestContext) {
    sqlx::query(
        "CREATE FUNCTION refuse_tokens() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN RAISE EXCEPTION 'cannot store %', NEW.access_token USING DETAIL = NEW.access_token; END $$",
    )
    .execute(&ctx.db)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER refuse_tokens BEFORE INSERT OR UPDATE ON installations \
         FOR EACH ROW EXECUTE FUNCTION refuse_tokens()",
    )
    .execute(&ctx.db)
    .await
    .unwrap();
}

#[openshop::test(configured())]
async fn a_token_that_cannot_be_stored_holds_back_for_thirty_seconds(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);
    refuse_tokens(ctx).await;

    let error = format!("{:#}", shop_name(ctx).await.unwrap_err());
    assert!(
        error.contains("Could not store the client credentials token"),
        "{error}"
    );
    // Every other caller within the thirty seconds falls back without a
    // token of its own.
    for _ in 0..5 {
        let error = shop_name(ctx).await.unwrap_err().to_string();
        assert!(error.contains("No access token found"), "{error}");
    }
    let reply = route(ctx, SHOP).await;
    assert!(
        reply["error"].as_str().unwrap().contains("not actively installed"),
        "{reply}"
    );
    assert_eq!(grant_requests(ctx).len(), 1);

    wait(Duration::from_secs(31)).await;
    assert!(shop_name(ctx).await.is_err());
    assert_eq!(grant_requests(ctx).len(), 2);
}

#[openshop::test(configured())]
async fn a_token_that_cannot_be_stored_shows_neither_itself_nor_its_ciphertext(ctx: &TestContext) {
    let _logging = LOGGING.lock().await;
    let shopify = Shopify::install(ctx);
    install(ctx, OTHER_SHOP, Some("offline-token"), false).await;
    shopify.revoke("offline-token");
    refuse_tokens(ctx).await;
    let lines = Arc::new(Lines::default());
    let previous = logger::set_logger(lines.clone());

    let mut shown = Vec::new();
    // Background work for the configured shop.
    let error = shop_name(ctx).await.unwrap_err();
    shown.push(format!("{error:#} {error:?}"));
    // A flow, whose run keeps its error.
    let _ = ctx.run_flow_for("shop-name-flow", json!({}), SHOP).await;
    let errors: Vec<(Option<String>,)> = sqlx::query_as("SELECT error FROM flow_runs")
        .fetch_all(&ctx.db)
        .await
        .unwrap();
    shown.extend(errors.into_iter().filter_map(|(error,)| error));
    // The renewal of a stored token after a `401`.
    let error = ctx
        .app
        .shopify_client(OTHER_SHOP, "default")
        .graphql(QUERY, ())
        .await
        .unwrap_err();
    shown.push(format!("{error:#} {error:?}"));
    // A signed launch, sent to OAuth as without the grant.
    let reply = launch(ctx, "dev-store-3.myshopify.com").await;
    assert_eq!(reply.status(), StatusCode::FOUND);
    shown.push(format!("{:?}", reply.headers().get("location")));
    shown.push(reply.text().await.unwrap());
    let logs: Vec<(String, Option<String>)> = sqlx::query_as("SELECT coalesce(message, ''), payload::text FROM logs")
        .fetch_all(&ctx.db)
        .await
        .unwrap();
    shown.extend(
        logs.into_iter()
            .map(|(message, payload)| format!("{message} {payload:?}")),
    );

    logger::set_logger(previous);
    shown.extend(lines.0.lock().unwrap().iter().cloned());

    assert!(grant_requests(ctx).len() >= 3);
    assert!(
        shown
            .iter()
            .any(|shown| shown.contains("Could not store the client credentials token")),
        "{shown:#?}"
    );
    for shown in &shown {
        assert!(!shown.contains("cc-token-"), "{shown}");
        assert!(!shown.contains("enc:aes-256-gcm"), "{shown}");
        assert!(!shown.contains("cannot store"), "{shown}");
    }
}

#[tokio::test]
async fn without_a_usable_encryption_key_a_cron_of_every_shop_says_so_once_per_app() {
    let _logging = LOGGING.lock().await;
    // A shop of its own, so that its lines are told from other tests'.
    let shop = "dev-store-8.myshopify.com";
    let crons = [Cron::new(EVERY_SECOND, "dev-store-flow").all_shops()];
    let options = TestOptions {
        encryption_key: Some("too-short".to_string()),
        ..options(Some(shop))
    };
    let ctx = TestContext::with_options(config(&crons), options).await.unwrap();
    let _shopify = Shopify::install(&ctx);
    let lines = Arc::new(Lines::default());
    let previous = logger::set_logger(lines.clone());

    let mut scheduler = Scheduler::start(&ctx.app).unwrap();
    // Three ticks at least.
    tokio::time::sleep(Duration::from_millis(3_300)).await;
    scheduler.stop();
    logger::set_logger(previous);

    let said: Vec<String> = lines
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|line| line.contains(shop) && line.contains("ENCRYPTION_KEY"))
        .cloned()
        .collect();
    assert_eq!(said.len(), 1, "{said:#?}");
    assert!(ctx.transport.requests().is_empty());
    assert!(cron_runs(&ctx).await.is_empty());
    ctx.shutdown().await.unwrap();
}

// ─── What a review of the grant found ────────────────────────────────

#[openshop::test(unconfigured())]
async fn the_first_session_after_a_launch_replaces_its_token_by_the_exchange(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);

    assert_eq!(launch(ctx, SHOP).await.status(), StatusCode::OK);
    let row = stored(ctx, SHOP).await.unwrap();
    assert_eq!(decrypted(ctx, &row).as_deref(), Some("cc-token-1"));

    // The offline token of the exchange, which does not expire, takes its place.
    api_request(ctx).await;
    assert_eq!(
        token_requests(ctx),
        [json!("client_credentials"), json!(TOKEN_EXCHANGE)]
    );
    let row = stored(ctx, SHOP).await.unwrap();
    assert_eq!(decrypted(ctx, &row).as_deref(), Some("exchange-token"));

    // Then nothing more is asked.
    api_request(ctx).await;
    assert_eq!(token_requests(ctx).len(), 2);
}

#[openshop::test(unconfigured())]
async fn a_launch_keeps_its_token_when_shopify_refuses_the_exchange(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);
    shopify.refuse_exchange();

    assert_eq!(launch(ctx, SHOP).await.status(), StatusCode::OK);
    api_request(ctx).await;
    api_request(ctx).await;
    // The exchange is tried once in place of the token, then not again.
    assert_eq!(
        token_requests(ctx),
        [json!("client_credentials"), json!(TOKEN_EXCHANGE)]
    );
    let row = stored(ctx, SHOP).await.unwrap();
    assert_eq!(decrypted(ctx, &row).as_deref(), Some("cc-token-1"));
}

#[tokio::test]
async fn a_launch_whose_token_cannot_be_stored_is_sent_to_oauth() {
    // Without a usable `ENCRYPTION_KEY`: a token is never stored in plaintext.
    let options = TestOptions {
        encryption_key: Some("too-short".to_string()),
        ..options(None)
    };
    let mut ctx = TestContext::with_options(config(&[]), options).await.unwrap();
    let _shopify = Shopify::install(&ctx);

    let reply = launch(&ctx, SHOP).await;
    assert_eq!(reply.status(), StatusCode::FOUND);
    assert_eq!(
        reply.headers()[LOCATION].to_str().unwrap(),
        "/auth?shop=dev-store-1.myshopify.com"
    );
    assert!(grant_requests(&ctx).is_empty());
    ctx.cleanup().await;
}

#[openshop::test(unconfigured())]
async fn a_shop_uninstalled_while_a_launch_asks_for_a_token_stays_uninstalled(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);
    install(ctx, SHOP, None, false).await;
    // The `app/uninstalled` webhook, while Shopify answers the launch's request.
    shopify.write_during_grant(
        ctx,
        format!("UPDATE installations SET uninstalled_at = now(), access_token = NULL WHERE shop = '{SHOP}'"),
    );

    let reply = launch(ctx, SHOP).await;
    assert_eq!(reply.status(), StatusCode::FOUND);
    assert_eq!(grant_requests(ctx).len(), 1);
    let row = stored(ctx, SHOP).await.unwrap();
    assert_eq!((row.0, row.2), (None, true));
}

#[openshop::test(configured())]
async fn a_renewal_keeps_a_token_stored_while_it_asked(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);
    install(ctx, OTHER_SHOP, Some("old-token"), false).await;
    shopify.revoke("old-token");
    // OAuth stores an offline token while Shopify answers the renewal.
    let offline = ctx.app.crypto().encrypt_string("oauth-token").unwrap();
    shopify.write_during_grant(
        ctx,
        format!("UPDATE installations SET access_token = '{offline}' WHERE shop = '{OTHER_SHOP}'"),
    );

    let data = ctx
        .app
        .shopify_client(OTHER_SHOP, "default")
        .graphql(QUERY, ())
        .await
        .unwrap();
    assert_eq!(data["shop"]["name"], "Dev store 1");
    // The request was sent again with the token OAuth stored, which is kept.
    assert_eq!(graphql_tokens(ctx), ["old-token", "oauth-token"]);
    let row = stored(ctx, OTHER_SHOP).await.unwrap();
    assert_eq!(decrypted(ctx, &row).as_deref(), Some("oauth-token"));
}

#[openshop::test(configured())]
async fn a_renewal_that_fails_leaves_the_answer_of_shopify(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);
    install(ctx, OTHER_SHOP, Some("old-token"), false).await;
    shopify.revoke("old-token");
    refuse_tokens(ctx).await;

    let error = ctx
        .app
        .shopify_client(OTHER_SHOP, "default")
        .graphql(QUERY, ())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("answered 401"), "{error}");
}

/// The log lines with their level.
#[derive(Default)]
struct Levels(Mutex<Vec<(&'static str, String)>>);

impl RuntimeLogger for Levels {
    fn info(&self, message: &str, _context: Option<&Value>) {
        self.0.lock().unwrap().push(("info", message.to_string()));
    }
    fn warn(&self, message: &str, _context: Option<&Value>) {
        self.0.lock().unwrap().push(("warn", message.to_string()));
    }
    fn error(&self, message: &str, _context: Option<&Value>) {
        self.0.lock().unwrap().push(("error", message.to_string()));
    }
}

#[openshop::test(unconfigured())]
async fn the_refusal_of_a_merchant_store_at_its_launch_is_no_warning(ctx: &TestContext) {
    let _logging = LOGGING.lock().await;
    let shopify = Shopify::install(ctx);
    shopify.refuse(400, json!({ "error": "shop_not_permitted" }));
    let levels = Arc::new(Levels::default());
    let previous = logger::set_logger(levels.clone());

    assert_eq!(launch(ctx, SHOP).await.status(), StatusCode::FOUND);
    logger::set_logger(previous);
    let lines = levels.0.lock().unwrap().clone();
    let refusal: Vec<_> = lines.iter().filter(|(_, line)| line.contains("refused")).collect();
    assert_eq!(refusal.len(), 1, "{lines:?}");
    assert_eq!(refusal[0].0, "info");
}
