//! Tokens obtained with Shopify's client credentials grant: for any shop
//! Shopify vouched for (a signed admin launch, a verified session token), and
//! for the shop of `SHOPIFY_CLIENT_CREDENTIALS_SHOP` in background work.

mod common;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::noop_flow;
use openshop::builder::{AdminPage, Route, Webhook};
use openshop::engine::Scheduler;
use openshop::runtime::{FORM_CONTENT_TYPE, OutboundRequest, OutboundResponse};
use openshop::server::ServerOptions;
use openshop::server::http::parse_query;
use openshop::testing::{MockTransport, TestContext, TestOptions, sign_query, sign_webhook_body, to_query_string};
use openshop::{
    AdminContext, Cron, OpenShopConfig, ShopifyAppConfig, ShopifyConfig, Value, WebhookContext, json, response,
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
}

impl Default for Shopify {
    fn default() -> Self {
        Self {
            issued: Mutex::new(0),
            expires_in: Mutex::new(Some(86_399)),
            revoked: Mutex::new(HashSet::new()),
            refusal: Mutex::new(None),
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
                return MockTransport::json(
                    200,
                    json!({ "access_token": "exchange-token", "scope": "read_products" }),
                );
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

    fn expire_in(&self, expires_in: Option<i64>) {
        *self.expires_in.lock().unwrap() = expires_in;
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
            Ok(shop_context) => match shop_context.shopify.graphql(QUERY, None).await {
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
    ctx.shopify.graphql(QUERY, None).await
}

fn config(crons: &[Cron]) -> OpenShopConfig {
    let builder = OpenShopConfig::builder()
        .flow(noop_flow("dev-store-flow"))
        .admin_page(AdminPage::new("/tools").loader("shop", admin_shop_name))
        .route("/shop", shop_route())
        .webhook(
            "ORDERS_CREATE",
            Webhook::new(|ctx: WebhookContext| async move {
                ctx.shopify.graphql(QUERY, None).await?;
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
        server: ServerOptions::production(),
        ..Default::default()
    }
}

/// `SHOPIFY_CLIENT_CREDENTIALS_SHOP` names `SHOP`, which has no installation yet.
async fn configured() -> TestContext {
    TestContext::with_options(config(&[]), options(Some(SHOP)))
        .await
        .unwrap()
}

/// No `SHOPIFY_CLIENT_CREDENTIALS_SHOP`.
async fn unconfigured() -> TestContext {
    TestContext::with_options(config(&[]), options(None)).await.unwrap()
}

async fn shop_name(ctx: &TestContext) -> openshop::Result<Value> {
    ctx.app.shopify_client(SHOP, "default").graphql(QUERY, None).await
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

/// The path Shopify admin opens: launch parameters signed with the app secret.
fn signed_launch(shop: &str) -> String {
    let query = sign_query(
        &[
            ("host", "YWRtaW4uc2hvcGlmeS5jb20vc3RvcmUvZGV2"),
            ("shop", shop),
            ("timestamp", "1770000000"),
        ],
        SECRET,
    );
    format!("/?{}", to_query_string(&query))
}

async fn launch(ctx: &TestContext, shop: &str) -> reqwest::Response {
    ctx.http.get(ctx.url_for(&signed_launch(shop))).send().await.unwrap()
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
    client.graphql(QUERY, None).await.unwrap();

    // Shopify no longer takes the token, long before it expires.
    shopify.revoke("cc-token-1");
    let data = client.graphql(QUERY, None).await.unwrap();
    assert_eq!(data["shop"]["name"], "Dev store 1");
    assert_eq!(grant_requests(ctx).len(), 2);
    assert_eq!(graphql_tokens(ctx), ["cc-token-1", "cc-token-1", "cc-token-2"]);

    // A new token refused as well is not renewed again: the 401 is the answer.
    shopify.revoke("cc-token-2");
    shopify.revoke("cc-token-3");
    ctx.transport.clear();
    let error = client.graphql(QUERY, None).await.unwrap_err().to_string();
    assert!(error.contains("Shopify GraphQL error (401)"), "{error}");
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
    assert!(error.contains("Shopify GraphQL error (401)"), "{error}");
    assert_eq!(grant_requests(ctx).len(), 1);
    assert_eq!(graphql_tokens(ctx), ["cc-token-1"]);
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
async fn a_failed_request_counts_as_a_refusal(ctx: &TestContext) {
    ctx.transport
        .respond_with(|_| Err(openshop::anyhow!("connection reset")));

    for _ in 0..3 {
        let error = shop_name(ctx).await.unwrap_err().to_string();
        assert!(error.contains("No access token found"), "{error}");
    }
    assert_eq!(grant_requests(ctx).len(), 1);
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
        .graphql(QUERY, None)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("No access token found"), "{error}");
    assert!(ctx.transport.requests().is_empty());
}

#[openshop::test(unconfigured())]
async fn without_the_variable_background_work_never_asks(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);

    let error = shop_name(ctx).await.unwrap_err().to_string();
    assert!(error.contains("No access token found"), "{error}");
    assert!(ctx.transport.requests().is_empty());

    // A stored token the Admin API refuses is not renewed.
    install(ctx, SHOP, Some("offline-token"), false).await;
    shopify.revoke("offline-token");
    let error = shop_name(ctx).await.unwrap_err().to_string();
    assert!(error.contains("Shopify GraphQL error (401)"), "{error}");
    assert!(grant_requests(ctx).is_empty());
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
            "[openshop] SHOPIFY_CLIENT_CREDENTIALS_SHOP must be a *.myshopify.com domain"
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

#[openshop::test(unconfigured())]
async fn a_session_token_gets_a_token_before_the_token_exchange(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);

    let reply = ctx
        .request_as(reqwest::Method::GET, "/api/flows", SHOP, "1")
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::OK);
    let requests = ctx.transport.requests();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0].body["grant_type"], "client_credentials");
    let row = stored(ctx, SHOP).await.unwrap();
    assert_eq!(decrypted(ctx, &row).as_deref(), Some("cc-token-1"));
}

#[openshop::test(unconfigured())]
async fn a_session_token_falls_back_to_the_token_exchange(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);
    shopify.refuse(400, json!({ "error": "shop_not_permitted" }));

    let reply = ctx
        .request_as(reqwest::Method::GET, "/api/flows", SHOP, "1")
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::OK);
    let grants: Vec<Value> = ctx
        .transport
        .requests()
        .iter()
        .map(|request| request.body["grant_type"].clone())
        .collect();
    assert_eq!(
        grants,
        [
            json!("client_credentials"),
            json!("urn:ietf:params:oauth:grant-type:token-exchange")
        ]
    );
    let row = stored(ctx, SHOP).await.unwrap();
    assert_eq!(decrypted(ctx, &row).as_deref(), Some("exchange-token"));
}

#[openshop::test(unconfigured())]
async fn an_admin_handler_renews_a_token_the_admin_api_refuses(ctx: &TestContext) {
    let shopify = Shopify::install(ctx);
    install(ctx, SHOP, Some("offline-token"), false).await;
    shopify.revoke("offline-token");

    // Background work has no shop of a session: the 401 is the answer.
    let error = shop_name(ctx).await.unwrap_err().to_string();
    assert!(error.contains("Shopify GraphQL error (401)"), "{error}");
    assert!(grant_requests(ctx).is_empty());

    // The client of an admin handler serves the shop of the session token.
    let url = build_admin_function_rpc_url("loader", "shop", "/tools");
    let reply = ctx
        .request_as(reqwest::Method::POST, &url, SHOP, "1")
        .json(&json!({ "input": null }))
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::OK);
    assert_eq!(reply.json::<Value>().await.unwrap()["shop"]["name"], "Dev store 1");
    assert_eq!(grant_requests(ctx).len(), 1);
    assert_eq!(graphql_tokens(ctx), ["offline-token", "offline-token", "cc-token-1"]);
    let row = stored(ctx, SHOP).await.unwrap();
    assert_eq!(decrypted(ctx, &row).as_deref(), Some("cc-token-1"));
}

// ─── Paths nothing vouches for ───────────────────────────────────────

#[openshop::test(unconfigured())]
async fn unsigned_paths_never_ask(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);

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

    // A flow queued for the shop, and the client of background work.
    ctx.dispatch_flow("dev-store-flow", json!({})).await.unwrap();
    assert!(shop_name(ctx).await.is_err());

    // A signed webhook: its shop header is not signed.
    assert_eq!(webhook(ctx, SHOP).await.status(), StatusCode::OK);

    assert!(ctx.transport.requests().is_empty());
    assert!(stored(ctx, SHOP).await.is_none_or(|row| row.0.is_none()));
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

#[openshop::test(configured())]
async fn a_webhook_never_asks_even_for_the_configured_shop(ctx: &TestContext) {
    let _shopify = Shopify::install(ctx);

    assert_eq!(webhook(ctx, SHOP).await.status(), StatusCode::OK);
    assert!(ctx.transport.requests().is_empty());
    assert!(stored(ctx, SHOP).await.is_none());
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
            .graphql(QUERY, None)
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
            .graphql(QUERY, None)
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
        let reply = webhook(ctx, shop).await;
        assert_eq!(reply.status(), StatusCode::BAD_REQUEST, "{shop}");
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
    let query = sign_query(&[("shop", SHOP), ("timestamp", "1770000000")], "second-secret");
    let reply = ctx
        .http
        .get(ctx.url_for(&format!("/?{}", to_query_string(&query))))
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), StatusCode::OK);

    // Background work for the first app asks with the first app's credentials.
    ctx.app
        .shopify_client(SHOP, "first")
        .graphql(QUERY, None)
        .await
        .unwrap();

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
        assert!(ctx.app.shopify_client(SHOP, handle).graphql(QUERY, None).await.is_err());
    }
    assert_eq!(grant_requests(ctx).len(), 2);
}
