//! OAuth (`/auth`, `/auth/callback`), the uninstall webhook and the session-token
//! authentication of the Admin API.

mod common;

use std::time::Duration;

use chrono::Utc;
use common::noop_flow;
use openshop::db::schema::InstallationRow;
use openshop::testing::{
    MockTransport, TestContext, TestOptions, session_token, sign_query, sign_webhook_body, to_query_string,
};
use openshop::{IndexMap, OpenShopConfig, ShopifyAppConfig, ShopifyConfig, Value, anyhow, json};
use reqwest::Response;

const ENCRYPTION_KEY: &str = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
const ENCRYPTED_PREFIX: &str = "enc:aes-256-gcm:";
const SHOP: &str = "test.myshopify.com";
const SECRET: &str = "test-secret";

fn config() -> OpenShopConfig {
    OpenShopConfig::builder().flow(noop_flow("auth-flow")).build().unwrap()
}

fn multi_config() -> OpenShopConfig {
    let apps = [("clientA", "client-a"), ("clientB", "client-b")]
        .into_iter()
        .map(|(handle, slug)| {
            let app = ShopifyAppConfig::Keys {
                api_key: format!("{slug}-key"),
                api_secret: format!("{slug}-secret"),
                app_url: Some(format!("https://{slug}.example.test")),
            };
            (handle.to_string(), app)
        })
        .collect();
    OpenShopConfig::builder()
        .flow(noop_flow("auth-flow"))
        .shopify(ShopifyConfig {
            scopes: Some("read_products".to_string()),
            apps: Some(apps),
        })
        .build()
        .unwrap()
}

fn with_encryption() -> TestOptions {
    TestOptions {
        encryption_key: Some(ENCRYPTION_KEY.to_string()),
        ..Default::default()
    }
}

async fn single_app() -> TestContext {
    TestContext::with_options(config(), with_encryption()).await.unwrap()
}

async fn multi_app() -> TestContext {
    TestContext::with_options(multi_config(), with_encryption())
        .await
        .unwrap()
}

async fn seed_installation(
    ctx: &TestContext,
    app_handle: &str,
    shop: &str,
    nonce: Option<&str>,
    access_token: Option<&str>,
) {
    sqlx::query("INSERT INTO installations (app_handle, shop, nonce, access_token) VALUES ($1, $2, $3, $4)")
        .bind(app_handle)
        .bind(shop)
        .bind(nonce)
        .bind(access_token)
        .execute(&ctx.db)
        .await
        .unwrap();
}

async fn find_installation(ctx: &TestContext, app_handle: &str, shop: &str) -> Option<InstallationRow> {
    sqlx::query_as("SELECT * FROM installations WHERE app_handle = $1 AND shop = $2")
        .bind(app_handle)
        .bind(shop)
        .fetch_optional(&ctx.db)
        .await
        .unwrap()
}

async fn installation(ctx: &TestContext, app_handle: &str, shop: &str) -> InstallationRow {
    find_installation(ctx, app_handle, shop)
        .await
        .expect("installation row")
}

async fn installation_count(ctx: &TestContext) -> i64 {
    let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM installations")
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    count
}

fn decrypted_token(ctx: &TestContext, row: &InstallationRow) -> Option<String> {
    ctx.app.crypto().decrypt_string(row.access_token.as_deref()).unwrap()
}

/// An unauthenticated GET.
async fn get(ctx: &TestContext, path: &str) -> Response {
    ctx.http.get(format!("{}{path}", ctx.url)).send().await.unwrap()
}

async fn api_get(ctx: &TestContext, path: &str, session_token: &str) -> Response {
    ctx.http
        .get(format!("{}{path}", ctx.url))
        .bearer_auth(session_token)
        .send()
        .await
        .unwrap()
}

fn location(response: &Response) -> String {
    response
        .headers()
        .get("location")
        .expect("location header")
        .to_str()
        .unwrap()
        .to_string()
}

async fn assert_error(response: Response, status: u16, error: &str) {
    assert_eq!(response.status().as_u16(), status);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body, json!({ "error": error }));
}

fn signed_callback(shop: &str, code: &str, state: &str, secret: &str) -> IndexMap<String, String> {
    let timestamp = Utc::now().timestamp().to_string();
    sign_query(
        &[
            ("shop", shop),
            ("code", code),
            ("state", state),
            ("timestamp", &timestamp),
        ],
        secret,
    )
}

fn callback_path(shop: &str, code: &str, state: &str, secret: &str) -> String {
    format!(
        "/auth/callback?{}",
        to_query_string(&signed_callback(shop, code, state, secret))
    )
}

/// An authenticated-looking GET carrying `cookie` (`name=value`), as a browser would.
async fn get_with(ctx: &TestContext, path: &str, cookie: Option<&str>) -> Response {
    let mut request = ctx.http.get(format!("{}{path}", ctx.url));
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    request.send().await.unwrap()
}

/// What `/auth` hands a browser: the `state` of the authorization and the
/// cookie (`name=value`) it sets.
struct Started {
    state: String,
    cookie: String,
}

/// Starts an installation at `path` (`/auth?shop=...`) as a browser that sends `cookie`, if it has one.
async fn begin_with(ctx: &TestContext, path: &str, cookie: Option<&str>) -> Started {
    let response = get_with(ctx, path, cookie).await;
    assert_eq!(response.status(), 302);
    let state = location(&response)
        .rsplit_once("&state=")
        .expect("state parameter")
        .1
        .to_string();
    let cookie = response
        .headers()
        .get("set-cookie")
        .expect("set-cookie header")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    Started { state, cookie }
}

/// A browser that has no cookie yet starts an installation of `shop`.
async fn begin(ctx: &TestContext, shop: &str) -> Started {
    begin_with(ctx, &format!("/auth?shop={shop}"), None).await
}

/// The callback of Shopify, signed with `secret`, in the browser of `started`.
async fn finish(ctx: &TestContext, shop: &str, started: &Started, secret: &str) -> Response {
    get_with(
        ctx,
        &callback_path(shop, "code", &started.state, secret),
        Some(&started.cookie),
    )
    .await
}

/// A state as `/auth` signs it, with any expiry.
fn signed_state(secret: &str, app_handle: &str, shop: &str, expiry: i64, nonce: &str, browser: &str) -> String {
    use hmac::{Hmac, Mac};
    let message = format!("openshop-oauth-state\0{app_handle}\0{shop}\0{expiry}\0{nonce}\0{browser}");
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(message.as_bytes());
    format!("{expiry}.{nonce}.{}", hex::encode(mac.finalize().into_bytes()))
}

fn grant_access_token(ctx: &TestContext, access_token: &str) {
    ctx.transport
        .respond_json(json!({ "access_token": access_token, "scope": "read_products" }));
}

/// Runs a GraphQL query as the shop and returns the access token sent to Shopify.
async fn shopify_access_token_header(ctx: &TestContext, shop: &str) -> String {
    ctx.transport.clear();
    ctx.transport
        .respond_json(json!({ "data": { "shop": { "name": "Test" } } }));
    let client = ctx.app.shopify_client(shop, "default");
    client.graphql("{ shop { name } }", None).await.unwrap();

    let request = ctx.transport.last_request().expect("GraphQL request");
    assert!(
        request.url.starts_with(&format!("https://{shop}/admin/api/")),
        "{}",
        request.url
    );
    request
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("x-shopify-access-token"))
        .map(|(_, value)| value.clone())
        .expect("access token header")
}

async fn post_uninstall(ctx: &TestContext, body: &str, hmac: &str, shop_header: Option<&str>) -> Response {
    let mut request = ctx
        .http
        .post(format!("{}/auth/webhooks/app-uninstalled", ctx.url))
        .header("content-type", "application/json")
        .header("x-shopify-hmac-sha256", hmac)
        .header("x-shopify-topic", "app/uninstalled")
        .body(body.to_string());
    if let Some(shop) = shop_header {
        request = request.header("x-shopify-shop-domain", shop);
    }
    request.send().await.unwrap()
}

// ─── GET /auth ───────────────────────────────────────────────────────

#[openshop::test(single_app())]
async fn get_auth_without_shop_returns_400(ctx: &TestContext) {
    assert_error(get(ctx, "/auth").await, 400, "Missing shop parameter").await;
    assert_error(get(ctx, "/auth?shop=").await, 400, "Missing shop parameter").await;
    assert_eq!(installation_count(ctx).await, 0);
}

#[tokio::test]
async fn get_auth_with_shop_builds_shopify_oauth_redirect() {
    let config = OpenShopConfig::builder()
        .flow(noop_flow("auth-flow"))
        .shopify(ShopifyConfig {
            scopes: Some("read_products,write_orders".to_string()),
            apps: None,
        })
        .build()
        .unwrap();
    let options = TestOptions {
        api_key: "test-client-id".to_string(),
        host: Some("https://app.example.test".to_string()),
        ..with_encryption()
    };
    let ctx = TestContext::with_options(config, options).await.unwrap();

    let response = get(&ctx, "/auth?shop=redirect-shop.myshopify.com").await;
    assert_eq!(response.status(), 302);
    let location = location(&response);
    assert!(
        location.starts_with("https://redirect-shop.myshopify.com/admin/oauth/authorize?client_id=test-client-id&"),
        "{location}"
    );
    assert!(location.contains("&scope=read_products,write_orders&"), "{location}");
    assert!(
        location.contains("&redirect_uri=https%3A%2F%2Fapp.example.test%2Fauth%2Fcallback&"),
        "{location}"
    );

    // `<expiry>.<nonce>.<signature>`, in the URL and in no row.
    let state = location.rsplit_once("&state=").expect("state").1;
    let parts: Vec<&str> = state.split('.').collect();
    assert_eq!(parts.len(), 3, "{state}");
    let expiry: i64 = parts[0].parse().unwrap();
    let ttl = expiry - Utc::now().timestamp();
    assert!((595..=600).contains(&ttl), "{ttl}");
    for part in &parts[1..] {
        assert!(part.bytes().all(|byte| byte.is_ascii_hexdigit()), "{state}");
    }
    assert_eq!(parts[1].len(), 32);
    assert_eq!(installation_count(&ctx).await, 0);

    // The state is bound to the browser by a cookie that script cannot read
    // and a third party cannot send.
    let cookie = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(cookie.starts_with("openshop_oauth="), "{cookie}");
    for attribute in ["Path=/auth", "Max-Age=600", "HttpOnly", "SameSite=Lax", "Secure"] {
        assert!(cookie.contains(&format!("; {attribute}")), "{attribute} in {cookie}");
    }
    assert_eq!(cookie.split(';').next().unwrap().len(), "openshop_oauth=".len() + 32);
    ctx.shutdown().await.unwrap();
}

#[openshop::test(multi_app())]
async fn get_auth_with_multiple_apps_requires_an_explicit_app_handle(ctx: &TestContext) {
    assert_error(
        get(ctx, "/auth?shop=multi-auth.myshopify.com").await,
        400,
        "[openshop] Missing Shopify app handle",
    )
    .await;
    assert_eq!(installation_count(ctx).await, 0);
}

#[openshop::test(multi_app())]
async fn get_auth_with_unknown_app_handle_returns_400(ctx: &TestContext) {
    assert_error(
        get(ctx, "/auth?shop=multi-auth.myshopify.com&app=clientC").await,
        400,
        "[openshop] Unknown Shopify app \"clientC\"",
    )
    .await;
    assert_error(
        get(ctx, "/auth?shop=multi-auth.myshopify.com&app=").await,
        400,
        "[openshop] Missing Shopify app handle",
    )
    .await;
    assert_eq!(installation_count(ctx).await, 0);
}

#[openshop::test(multi_app())]
async fn get_auth_with_app_handle_builds_the_matching_app_oauth_redirect(ctx: &TestContext) {
    let response = get(ctx, "/auth?shop=multi-auth.myshopify.com&app=clientA").await;
    assert_eq!(response.status(), 302);
    let location = location(&response);
    assert!(location.contains("client_id=client-a-key"), "{location}");
    assert!(location.contains("scope=read_products"), "{location}");
    assert!(
        location.contains("https%3A%2F%2Fclient-a.example.test%2Fauth%2Fcallback"),
        "{location}"
    );

    // Nothing is written until Shopify calls back.
    assert_eq!(installation_count(ctx).await, 0);
}

#[openshop::test(single_app())]
async fn get_auth_rejects_invalid_shop_domain(ctx: &TestContext) {
    assert_error(
        get(ctx, "/auth?shop=evil.example.com").await,
        400,
        "Invalid shop parameter",
    )
    .await;
}

#[openshop::test(single_app())]
async fn get_auth_rejects_shop_domain_normalization_attacks(ctx: &TestContext) {
    for shop in [
        "evil.com/x.myshopify.com",
        "https://evil.com/x.myshopify.com",
        "x.myshopify.com@evil.com",
        "https://x.myshopify.com@evil.com",
        "x.myshopify.com:pass@evil.com",
        "evil.com#x.myshopify.com",
        "evil.com?x.myshopify.com",
        "evil.com\\x.myshopify.com",
        "x.myshopify.com.evil.com",
        "x.myshopify.com.",
        "xmyshopify.com",
        "myshopify.com",
        ".myshopify.com",
        "-x.myshopify.com",
        "a.b.myshopify.com",
        "x_y.myshopify.com",
        "x.myshopify.io",
        "x.myshopify.com evil.com",
        "x.myshopify.com\nevil.com",
        "x.myshopify.com%00.evil.com",
        "x.myshopify.com?next=https://evil.com",
        "x.myshopify.com://evil.com",
        "//evil.com",
    ] {
        let response = ctx
            .http
            .get(format!("{}/auth", ctx.url))
            .query(&[("shop", shop)])
            .send()
            .await
            .unwrap();
        assert!(response.headers().get("location").is_none(), "{shop:?}");
        assert_eq!(response.status(), 400, "{shop:?}");
        let body: Value = response.json().await.unwrap();
        assert_eq!(body, json!({ "error": "Invalid shop parameter" }), "{shop:?}");
    }
    assert_eq!(installation_count(ctx).await, 0);
}

#[openshop::test(single_app())]
async fn get_auth_always_redirects_to_the_normalized_shop_host(ctx: &TestContext) {
    for shop in [
        "Normalized.MyShopify.com",
        "  normalized.myshopify.com  ",
        "https://normalized.myshopify.com/admin",
        "normalized.myshopify.com/../../evil.com",
        "normalized.myshopify.com:443",
        "normalized.myshopify.com#@evil.com",
        "normalized.myshopify.com?next=evil.com",
        // Userinfo is dropped: only the real host survives.
        "evil.com@normalized.myshopify.com",
    ] {
        let response = ctx
            .http
            .get(format!("{}/auth", ctx.url))
            .query(&[("shop", shop)])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 302, "{shop:?}");
        let location = location(&response);
        assert!(
            location.starts_with("https://normalized.myshopify.com/admin/oauth/authorize?client_id=test-app&"),
            "{shop:?} -> {location}"
        );
        assert!(!location.contains("evil"), "{shop:?} -> {location}");
    }
    assert_eq!(installation_count(ctx).await, 0);
}

#[openshop::test(single_app())]
async fn get_auth_writes_nothing_and_leaves_the_installation_alone(ctx: &TestContext) {
    seed_installation(ctx, "default", "rotate.myshopify.com", None, Some("existing-token")).await;
    let before = installation(ctx, "default", "rotate.myshopify.com").await;

    begin(ctx, "rotate.myshopify.com").await;
    begin(ctx, "rotate.myshopify.com").await;
    begin(ctx, "unknown.myshopify.com").await;

    let row = installation(ctx, "default", "rotate.myshopify.com").await;
    assert_eq!(row.access_token.as_deref(), Some("existing-token"));
    assert_eq!(row.nonce, before.nonce);
    assert_eq!(installation_count(ctx).await, 1);
}

#[openshop::test(single_app())]
async fn a_second_get_auth_for_a_shop_does_not_invalidate_the_first_pending_installation(ctx: &TestContext) {
    let shop = "pending.myshopify.com";
    // The merchant's browser starts, then somebody else's browser does.
    let merchant = begin(ctx, shop).await;
    let other = begin(ctx, shop).await;
    assert_ne!(merchant.state, other.state);
    assert_ne!(merchant.cookie, other.cookie);
    grant_access_token(ctx, "merchant-token");

    let response = finish(ctx, shop, &merchant, SECRET).await;
    assert_eq!(response.status(), 302);
    let row = installation(ctx, "default", shop).await;
    assert_eq!(decrypted_token(ctx, &row).as_deref(), Some("merchant-token"));

    // The other one is still good for its own browser.
    grant_access_token(ctx, "other-token");
    assert_eq!(finish(ctx, shop, &other, SECRET).await.status(), 302);
}

#[openshop::test(single_app())]
async fn several_pending_installations_of_one_browser_stay_valid(ctx: &TestContext) {
    let first = begin(ctx, "first.myshopify.com").await;
    // The browser sends its cookie: it keeps it.
    let second = begin_with(ctx, "/auth?shop=second.myshopify.com", Some(&first.cookie)).await;
    let again = begin_with(ctx, "/auth?shop=first.myshopify.com", Some(&first.cookie)).await;
    assert_eq!(first.cookie, second.cookie);
    assert_eq!(first.cookie, again.cookie);
    grant_access_token(ctx, "token");

    assert_eq!(finish(ctx, "second.myshopify.com", &second, SECRET).await.status(), 302);
    assert_eq!(finish(ctx, "first.myshopify.com", &again, SECRET).await.status(), 302);
    // The first state was never used, and is as good as the others.
    assert_eq!(finish(ctx, "first.myshopify.com", &first, SECRET).await.status(), 302);
}

#[openshop::test(single_app())]
async fn get_auth_callback_refuses_a_state_from_another_browser(ctx: &TestContext) {
    let shop = "browser.myshopify.com";
    let victim = begin(ctx, shop).await;
    let attacker = begin(ctx, shop).await;
    grant_access_token(ctx, "must-not-be-stored");

    // No cookie, the cookie of another browser, a made-up cookie.
    let path = callback_path(shop, "code", &victim.state, SECRET);
    assert_error(get_with(ctx, &path, None).await, 401, "Invalid state/nonce").await;
    assert_error(
        get_with(ctx, &path, Some(&attacker.cookie)).await,
        401,
        "Invalid state/nonce",
    )
    .await;
    let made_up = format!("openshop_oauth={}", "0".repeat(32));
    assert_error(get_with(ctx, &path, Some(&made_up)).await, 401, "Invalid state/nonce").await;

    assert!(ctx.transport.requests().is_empty());
    assert_eq!(installation_count(ctx).await, 0);
    // The browser that started it still can.
    assert_eq!(finish(ctx, shop, &victim, SECRET).await.status(), 302);
}

#[openshop::test(single_app())]
async fn get_auth_callback_ignores_a_nonce_stored_in_the_installation(ctx: &TestContext) {
    // What an earlier version, or the TypeScript runtime, left in the row.
    let shop = "legacy.myshopify.com";
    seed_installation(ctx, "default", shop, Some("legacy-nonce"), None).await;
    grant_access_token(ctx, "must-not-be-stored");

    let path = callback_path(shop, "code", "legacy-nonce", SECRET);
    assert_error(get(ctx, &path).await, 401, "Invalid state/nonce").await;
    assert!(ctx.transport.requests().is_empty());

    // An installation completed here clears it.
    let started = begin(ctx, shop).await;
    assert_eq!(finish(ctx, shop, &started, SECRET).await.status(), 302);
    let row = installation(ctx, "default", shop).await;
    assert!(row.nonce.is_none());
    assert!(row.access_token.is_some());
}

#[openshop::test(single_app())]
async fn get_auth_callback_refuses_an_expired_state(ctx: &TestContext) {
    let shop = "expired.myshopify.com";
    let browser = "a".repeat(32);
    let cookie = format!("openshop_oauth={browser}");
    grant_access_token(ctx, "must-not-be-stored");

    let now = Utc::now().timestamp();
    let expired = signed_state(SECRET, "default", shop, now - 1, "b", &browser);
    let path = callback_path(shop, "code", &expired, SECRET);
    assert_error(get_with(ctx, &path, Some(&cookie)).await, 401, "Invalid state/nonce").await;
    assert!(ctx.transport.requests().is_empty());

    // The same state, still alive, is good: only the expiry was wrong.
    let alive = signed_state(SECRET, "default", shop, now + 60, "b", &browser);
    let path = callback_path(shop, "code", &alive, SECRET);
    assert_eq!(get_with(ctx, &path, Some(&cookie)).await.status(), 302);
}

#[openshop::test(single_app())]
async fn get_auth_callback_refuses_a_state_that_was_changed(ctx: &TestContext) {
    let shop = "changed.myshopify.com";
    let started = begin(ctx, shop).await;
    grant_access_token(ctx, "must-not-be-stored");
    let (expiry, rest) = started.state.split_once('.').unwrap();
    let later = format!("{}.{rest}", expiry.parse::<i64>().unwrap() + 3600);
    let mut flipped = started.state.clone();
    let last = flipped.pop().unwrap();
    flipped.push(if last == '0' { '1' } else { '0' });

    for state in [
        later,
        flipped,
        format!("{}.", started.state),
        started.state.replace('.', ":"),
        "nonce".to_string(),
        String::new(),
    ] {
        let path = callback_path(shop, "code", &state, SECRET);
        let response = get_with(ctx, &path, Some(&started.cookie)).await;
        // An empty state is a missing parameter.
        assert!(matches!(response.status().as_u16(), 400 | 401), "{state}");
    }
    assert!(ctx.transport.requests().is_empty());
    assert_eq!(installation_count(ctx).await, 0);
}

// ─── GET /auth/callback ──────────────────────────────────────────────

#[openshop::test(single_app())]
async fn get_auth_callback_without_required_parameters_returns_400(ctx: &TestContext) {
    for query in [
        vec![("code", "c"), ("state", "s")],
        vec![("shop", "x.myshopify.com"), ("state", "s")],
        vec![("shop", "x.myshopify.com"), ("code", "c")],
        vec![("shop", "x.myshopify.com"), ("code", ""), ("state", "s")],
    ] {
        let path = format!("/auth/callback?{}", to_query_string(&sign_query(&query, SECRET)));
        assert_error(get(ctx, &path).await, 400, "Missing required parameters").await;
    }
    assert!(ctx.transport.requests().is_empty());
}

#[openshop::test(single_app())]
async fn get_auth_callback_rejects_invalid_hmac(ctx: &TestContext) {
    let response = get(ctx, "/auth/callback?shop=x.myshopify.com&code=c&state=s&hmac=deadbeef").await;
    assert_error(response, 401, "Invalid HMAC").await;
    let response = get(ctx, "/auth/callback?shop=x.myshopify.com&code=c&state=s").await;
    assert_error(response, 401, "Invalid HMAC").await;

    assert!(ctx.transport.requests().is_empty());
    assert_eq!(installation_count(ctx).await, 0);
}

#[openshop::test(single_app())]
async fn get_auth_callback_rejects_a_query_signed_with_another_secret(ctx: &TestContext) {
    seed_installation(ctx, "default", "x.myshopify.com", None, None).await;
    grant_access_token(ctx, "must-not-be-stored");

    let path = callback_path("x.myshopify.com", "c", "s", "another-secret");
    assert_error(get(ctx, &path).await, 401, "Invalid HMAC").await;

    assert!(ctx.transport.requests().is_empty());
    assert!(
        installation(ctx, "default", "x.myshopify.com")
            .await
            .access_token
            .is_none()
    );
}

#[openshop::test(single_app())]
async fn get_auth_callback_rejects_tampered_parameters(ctx: &TestContext) {
    seed_installation(ctx, "default", "x.myshopify.com", None, None).await;
    seed_installation(ctx, "default", "y.myshopify.com", None, None).await;
    grant_access_token(ctx, "must-not-be-stored");

    let tampered: [(&str, &str); 4] = [
        ("code", "another-code"),
        ("shop", "y.myshopify.com"),
        ("timestamp", "1"),
        ("extra", "1"),
    ];
    for (key, value) in tampered {
        let mut query = signed_callback("x.myshopify.com", "c", "s", SECRET);
        query.insert(key.to_string(), value.to_string());
        let path = format!("/auth/callback?{}", to_query_string(&query));
        assert_error(get(ctx, &path).await, 401, "Invalid HMAC").await;
    }

    // A valid signature followed by garbage is not a valid signature.
    let mut query = signed_callback("x.myshopify.com", "c", "s", SECRET);
    query["hmac"].push_str("00");
    let path = format!("/auth/callback?{}", to_query_string(&query));
    assert_error(get(ctx, &path).await, 401, "Invalid HMAC").await;

    assert!(ctx.transport.requests().is_empty());
}

#[openshop::test(single_app())]
async fn get_auth_callback_rejects_valid_hmac_with_invalid_shop_domain(ctx: &TestContext) {
    let path = callback_path("evil.example.com", "c", "s", SECRET);
    assert_error(get(ctx, &path).await, 400, "Invalid shop parameter").await;
    assert!(ctx.transport.requests().is_empty());
}

#[openshop::test(single_app())]
async fn get_auth_callback_rejects_shop_domain_normalization_attacks(ctx: &TestContext) {
    seed_installation(ctx, "default", "x.myshopify.com", None, None).await;
    grant_access_token(ctx, "must-not-be-stored");

    for shop in [
        "evil.com/x.myshopify.com",
        "https://evil.com/x.myshopify.com",
        "x.myshopify.com@evil.com",
        "evil.com#x.myshopify.com",
        "evil.com\\x.myshopify.com",
        "x.myshopify.com.evil.com",
    ] {
        let path = callback_path(shop, "c", "s", SECRET);
        assert_error(get(ctx, &path).await, 400, "Invalid shop parameter").await;
    }

    assert!(ctx.transport.requests().is_empty());
    assert!(
        installation(ctx, "default", "x.myshopify.com")
            .await
            .access_token
            .is_none()
    );
}

#[tokio::test]
async fn get_auth_callback_with_valid_hmac_and_mocked_token_exchange_updates_installation() {
    let options = TestOptions {
        api_key: "app-key-for-callback".to_string(),
        ..with_encryption()
    };
    let ctx = TestContext::with_options(config(), options).await.unwrap();
    let shop = "oauth-ok.myshopify.com";
    let started = begin(&ctx, shop).await;
    grant_access_token(&ctx, "stored-access-token");

    let response = finish(&ctx, shop, &started, SECRET).await;
    assert_eq!(response.status(), 302);
    assert_eq!(
        location(&response),
        "https://oauth-ok.myshopify.com/admin/apps/app-key-for-callback"
    );

    let requests = ctx.transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].url,
        "https://oauth-ok.myshopify.com/admin/oauth/access_token"
    );
    assert_eq!(
        requests[0].body,
        json!({ "client_id": "app-key-for-callback", "client_secret": SECRET, "code": "code" })
    );

    let row = installation(&ctx, "default", shop).await;
    let stored = row.access_token.clone().expect("stored token");
    assert_ne!(stored, "stored-access-token");
    assert!(stored.starts_with(ENCRYPTED_PREFIX), "{stored}");
    assert!(!stored.contains("stored-access-token"));
    assert_eq!(decrypted_token(&ctx, &row).as_deref(), Some("stored-access-token"));
    assert_eq!(row.scopes.as_deref(), Some("read_products"));
    assert!(row.nonce.is_none());
    assert!(row.uninstalled_at.is_none());
    ctx.shutdown().await.unwrap();
}

#[openshop::test(multi_app())]
async fn get_auth_callback_resolves_the_app_from_hmac_and_isolates_installations_by_app(ctx: &TestContext) {
    let shop = "same-shop.myshopify.com";
    let started_a = begin_with(ctx, &format!("/auth?shop={shop}&app=clientA"), None).await;
    let started_b = begin_with(ctx, &format!("/auth?shop={shop}&app=clientB"), None).await;
    grant_access_token(ctx, "stored-token-client-b");

    let response = finish(ctx, shop, &started_b, "client-b-secret").await;
    assert_eq!(response.status(), 302);
    assert_eq!(
        location(&response),
        "https://same-shop.myshopify.com/admin/apps/client-b-key"
    );

    let requests = ctx.transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].body,
        json!({
            "client_id": "client-b-key",
            "client_secret": "client-b-secret",
            "code": "code",
        })
    );

    assert!(find_installation(ctx, "clientA", shop).await.is_none());
    let row_b = installation(ctx, "clientB", shop).await;
    assert_eq!(decrypted_token(ctx, &row_b).as_deref(), Some("stored-token-client-b"));
    assert!(row_b.nonce.is_none());

    // The installation of the other app is still pending.
    grant_access_token(ctx, "stored-token-client-a");
    assert_eq!(finish(ctx, shop, &started_a, "client-a-secret").await.status(), 302);
}

#[openshop::test(single_app())]
async fn get_auth_callback_completes_the_flow_started_by_get_auth(ctx: &TestContext) {
    let shop = "full-flow.myshopify.com";
    let started = begin(ctx, shop).await;
    grant_access_token(ctx, "full-flow-token");

    let response = finish(ctx, shop, &started, SECRET).await;
    assert_eq!(response.status(), 302);
    assert_eq!(
        location(&response),
        "https://full-flow.myshopify.com/admin/apps/test-app"
    );

    let row = installation(ctx, "default", shop).await;
    assert_eq!(decrypted_token(ctx, &row).as_deref(), Some("full-flow-token"));
    assert!(row.nonce.is_none());

    // The stored ciphertext is what authenticates later Admin API calls.
    assert_eq!(shopify_access_token_header(ctx, shop).await, "full-flow-token");
}

#[openshop::test(single_app())]
async fn get_auth_callback_rejects_unknown_or_mismatched_state(ctx: &TestContext) {
    grant_access_token(ctx, "must-not-be-stored");
    let started = begin(ctx, "started.myshopify.com").await;

    // No authorization was ever started for this shop: the state is for another.
    let path = callback_path("never-started.myshopify.com", "c", &started.state, SECRET);
    assert_error(
        get_with(ctx, &path, Some(&started.cookie)).await,
        401,
        "Invalid state/nonce",
    )
    .await;

    for state in [
        "another-nonce",
        "null",
        "undefined",
        "s",
        "1.2.3",
        "9999999999.nonce.signature",
    ] {
        let path = callback_path("started.myshopify.com", "c", state, SECRET);
        assert_error(
            get_with(ctx, &path, Some(&started.cookie)).await,
            401,
            "Invalid state/nonce",
        )
        .await;
    }

    // An installed shop without a pending authorization has nothing to match.
    seed_installation(ctx, "default", "installed.myshopify.com", None, Some("existing-token")).await;
    for state in ["null", "undefined", "s"] {
        let path = callback_path("installed.myshopify.com", "c", state, SECRET);
        assert_error(
            get_with(ctx, &path, Some(&started.cookie)).await,
            401,
            "Invalid state/nonce",
        )
        .await;
    }

    assert!(ctx.transport.requests().is_empty());
    assert_eq!(installation_count(ctx).await, 1);
    let installed = installation(ctx, "default", "installed.myshopify.com").await;
    assert_eq!(installed.access_token.as_deref(), Some("existing-token"));
}

#[openshop::test(single_app())]
async fn get_auth_callback_rejects_a_replayed_state(ctx: &TestContext) {
    let shop = "replay.myshopify.com";
    let started = begin(ctx, shop).await;
    grant_access_token(ctx, "first-token");

    assert_eq!(finish(ctx, shop, &started, SECRET).await.status(), 302);

    grant_access_token(ctx, "second-token");
    assert_error(finish(ctx, shop, &started, SECRET).await, 401, "Invalid state/nonce").await;
    let path = callback_path(shop, "code-2", &started.state, SECRET);
    assert_error(
        get_with(ctx, &path, Some(&started.cookie)).await,
        401,
        "Invalid state/nonce",
    )
    .await;

    assert_eq!(ctx.transport.requests().len(), 1);
    let row = installation(ctx, "default", shop).await;
    assert_eq!(decrypted_token(ctx, &row).as_deref(), Some("first-token"));
}

#[openshop::test(single_app())]
async fn get_auth_callback_rejects_a_state_issued_for_another_shop(ctx: &TestContext) {
    let started_a = begin(ctx, "shop-a.myshopify.com").await;
    let started_b = begin_with(ctx, "/auth?shop=shop-b.myshopify.com", Some(&started_a.cookie)).await;
    grant_access_token(ctx, "must-not-be-stored");

    let path = callback_path("shop-b.myshopify.com", "c", &started_a.state, SECRET);
    assert_error(
        get_with(ctx, &path, Some(&started_a.cookie)).await,
        401,
        "Invalid state/nonce",
    )
    .await;

    assert!(ctx.transport.requests().is_empty());
    assert_eq!(installation_count(ctx).await, 0);
    // Refused states are not spent.
    assert_eq!(
        finish(ctx, "shop-b.myshopify.com", &started_b, SECRET).await.status(),
        302
    );
}

#[openshop::test(multi_app())]
async fn get_auth_callback_rejects_a_state_issued_for_another_app(ctx: &TestContext) {
    let shop = "same-shop.myshopify.com";
    let started_a = begin_with(ctx, &format!("/auth?shop={shop}&app=clientA"), None).await;
    grant_access_token(ctx, "must-not-be-stored");

    let path = callback_path(shop, "c", &started_a.state, "client-b-secret");
    assert_error(
        get_with(ctx, &path, Some(&started_a.cookie)).await,
        401,
        "Invalid state/nonce",
    )
    .await;
    // The single-app environment secret is not an app of a multi-app instance.
    let path = callback_path(shop, "c", &started_a.state, SECRET);
    assert_error(get_with(ctx, &path, Some(&started_a.cookie)).await, 401, "Invalid HMAC").await;

    assert!(ctx.transport.requests().is_empty());
    assert_eq!(installation_count(ctx).await, 0);
}

#[openshop::test(single_app())]
async fn get_auth_callback_reports_a_failed_token_exchange(ctx: &TestContext) {
    let shop = "exchange-fails.myshopify.com";
    let started = begin(ctx, shop).await;
    ctx.transport
        .respond_with(|_| Ok(MockTransport::json(400, json!({ "error": "invalid_request" }))));

    let response = finish(ctx, shop, &started, SECRET).await;
    assert_error(response, 500, "Token exchange failed: {\"error\":\"invalid_request\"}").await;

    // The code Shopify gave is spent with it: the merchant starts again.
    let started = begin(ctx, shop).await;
    ctx.transport.respond_with(|_| Err(anyhow!("connection reset")));
    let response = finish(ctx, shop, &started, SECRET).await;
    assert_eq!(response.status(), 500);
    assert_eq!(response.text().await.unwrap(), "Internal Server Error");

    assert_eq!(installation_count(ctx).await, 0);
}

#[openshop::test(single_app())]
async fn get_auth_callback_reinstalls_an_uninstalled_shop(ctx: &TestContext) {
    let shop = "reinstall.myshopify.com";
    sqlx::query("INSERT INTO installations (shop, uninstalled_at) VALUES ($1, now())")
        .bind(shop)
        .execute(&ctx.db)
        .await
        .unwrap();
    let started = begin(ctx, shop).await;
    grant_access_token(ctx, "fresh-token");

    let response = finish(ctx, shop, &started, SECRET).await;
    assert_eq!(response.status(), 302);

    let row = installation(ctx, "default", shop).await;
    assert!(row.uninstalled_at.is_none());
    assert_eq!(decrypted_token(ctx, &row).as_deref(), Some("fresh-token"));
    assert_eq!(installation_count(ctx).await, 1);
}

#[openshop::test(TestContext::new(config()))]
async fn get_auth_callback_stores_the_token_in_plaintext_without_an_encryption_key(ctx: &TestContext) {
    let shop = "plaintext.myshopify.com";
    let started = begin(ctx, shop).await;
    grant_access_token(ctx, "plaintext-token");

    let response = finish(ctx, shop, &started, SECRET).await;
    assert_eq!(response.status(), 302);
    assert_eq!(
        installation(ctx, "default", shop).await.access_token.as_deref(),
        Some("plaintext-token")
    );
}

#[tokio::test]
async fn get_auth_callback_in_production_without_an_encryption_key_refuses_to_store_the_token() {
    let options = TestOptions {
        production: true,
        encryption_key: None,
        ..Default::default()
    };
    let ctx = TestContext::with_options(config(), options).await.unwrap();
    let shop = "production.myshopify.com";
    let started = begin(&ctx, shop).await;
    grant_access_token(&ctx, "production-token");

    let response = finish(&ctx, shop, &started, SECRET).await;
    assert_eq!(response.status(), 500);
    let body = response.text().await.unwrap();
    assert_eq!(body, "Internal Server Error");

    assert_eq!(installation_count(&ctx).await, 0);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn access_token_encrypted_by_the_typescript_implementation_is_still_usable() {
    // `encryptString("shpat_from_typescript")` from node:crypto with ENCRYPTION_KEY and a fixed IV.
    let stored = "enc:aes-256-gcm:000102030405060708090a0b:8a9b7b52670a17e1bc5b07b96fc873f4:\
                  b8c70330bb13a5f0c9515d0b46f86f359866f860e0";
    let ctx = single_app().await;
    seed_installation(&ctx, "default", SHOP, None, Some(stored)).await;

    assert_eq!(shopify_access_token_header(&ctx, SHOP).await, "shpat_from_typescript");

    // The shop counts as installed: no token exchange, and the row is left as it was.
    ctx.transport.clear();
    assert_eq!(ctx.get("/api/flows").send().await.unwrap().status(), 200);
    assert!(ctx.transport.requests().is_empty());
    assert_eq!(
        installation(&ctx, "default", SHOP).await.access_token.as_deref(),
        Some(stored)
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn ambiguous_app_resolution_fails_closed() {
    let apps = ["clientA", "clientB"]
        .into_iter()
        .map(|handle| {
            let app = ShopifyAppConfig::Keys {
                api_key: "shared-key".to_string(),
                api_secret: "shared-secret".to_string(),
                app_url: None,
            };
            (handle.to_string(), app)
        })
        .collect();
    let config = OpenShopConfig::builder()
        .flow(noop_flow("auth-flow"))
        .shopify(ShopifyConfig {
            scopes: Some("read_products".to_string()),
            apps: Some(apps),
        })
        .build()
        .unwrap();
    let ctx = TestContext::with_options(config, with_encryption()).await.unwrap();
    let shop = "same-shop.myshopify.com";
    for handle in ["clientA", "clientB"] {
        seed_installation(&ctx, handle, shop, None, Some("token")).await;
    }
    grant_access_token(&ctx, "must-not-be-stored");

    let token = session_token(shop, "shared-secret", "shared-key", "1");
    assert_error(
        api_get(&ctx, "/api/flows", &token).await,
        401,
        "Unauthorized: invalid session token",
    )
    .await;

    let path = callback_path(shop, "c", "nonce-1", "shared-secret");
    assert_error(get(&ctx, &path).await, 401, "Invalid HMAC").await;

    let body = json!({ "myshopify_domain": shop }).to_string();
    let response = post_uninstall(&ctx, &body, &sign_webhook_body(&body, "shared-secret"), Some(shop)).await;
    assert_error(response, 401, "Invalid webhook HMAC").await;

    assert!(ctx.transport.requests().is_empty());
    for handle in ["clientA", "clientB"] {
        let row = installation(&ctx, handle, shop).await;
        assert_eq!(row.access_token.as_deref(), Some("token"));
    }
    ctx.shutdown().await.unwrap();
}

// ─── POST /auth/webhooks/app-uninstalled ─────────────────────────────

#[openshop::test(single_app())]
async fn app_uninstalled_webhook_rejects_invalid_hmac(ctx: &TestContext) {
    seed_installation(ctx, "default", SHOP, None, Some("token")).await;
    let body = json!({ "myshopify_domain": SHOP }).to_string();

    for hmac in [
        "invalid".to_string(),
        String::new(),
        sign_webhook_body(&body, "another-secret"),
        sign_webhook_body("{}", SECRET),
    ] {
        let response = post_uninstall(ctx, &body, &hmac, Some(SHOP)).await;
        assert_error(response, 401, "Invalid webhook HMAC").await;
    }
    let response = ctx
        .http
        .post(format!("{}/auth/webhooks/app-uninstalled", ctx.url))
        .header("x-shopify-shop-domain", SHOP)
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_error(response, 401, "Invalid webhook HMAC").await;

    let row = installation(ctx, "default", SHOP).await;
    assert_eq!(row.access_token.as_deref(), Some("token"));
    assert!(row.uninstalled_at.is_none());
}

#[openshop::test(single_app())]
async fn app_uninstalled_webhook_clears_the_token_of_the_shop(ctx: &TestContext) {
    seed_installation(ctx, "default", SHOP, None, Some("token")).await;
    seed_installation(ctx, "default", "other.myshopify.com", None, Some("other-token")).await;
    let body = json!({ "id": 1, "myshopify_domain": "TEST.myshopify.com" }).to_string();

    // The signed payload wins over the unsigned header, and is normalized.
    let response = post_uninstall(
        ctx,
        &body,
        &sign_webhook_body(&body, SECRET),
        Some("other.myshopify.com"),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.json::<Value>().await.unwrap(), json!({ "ok": true }));

    let row = installation(ctx, "default", SHOP).await;
    assert!(row.access_token.is_none());
    assert!(row.uninstalled_at.is_some());
    let other = installation(ctx, "default", "other.myshopify.com").await;
    assert_eq!(other.access_token.as_deref(), Some("other-token"));
    assert!(other.uninstalled_at.is_none());
}

#[openshop::test(single_app())]
async fn app_uninstalled_webhook_falls_back_to_the_shop_header(ctx: &TestContext) {
    seed_installation(ctx, "default", SHOP, None, Some("token")).await;
    let body = json!({ "id": 1 }).to_string();

    let response = post_uninstall(ctx, &body, &sign_webhook_body(&body, SECRET), Some(SHOP)).await;
    assert_eq!(response.status(), 200);

    let row = installation(ctx, "default", SHOP).await;
    assert!(row.access_token.is_none());
    assert!(row.uninstalled_at.is_some());
}

#[openshop::test(single_app())]
async fn app_uninstalled_webhook_ignores_unknown_or_invalid_shops(ctx: &TestContext) {
    seed_installation(ctx, "default", SHOP, None, Some("token")).await;

    // The shop of the signed payload decides, whatever the header claims.
    for shop in [
        "unknown.myshopify.com",
        "evil.example.com",
        "test.myshopify.com.evil.com",
    ] {
        let body = json!({ "myshopify_domain": shop }).to_string();
        let response = post_uninstall(ctx, &body, &sign_webhook_body(&body, SECRET), Some(SHOP)).await;
        assert_eq!(response.status(), 200, "{shop}");
    }
    let empty = "{}";
    let response = post_uninstall(ctx, empty, &sign_webhook_body(empty, SECRET), None).await;
    assert_eq!(response.status(), 200);
    // A signed body that is not JSON is an unhandled error, not an uninstall.
    let response = post_uninstall(ctx, "not json", &sign_webhook_body("not json", SECRET), Some(SHOP)).await;
    assert_eq!(response.status(), 500);
    assert_eq!(response.text().await.unwrap(), "Internal Server Error");

    let row = installation(ctx, "default", SHOP).await;
    assert_eq!(row.access_token.as_deref(), Some("token"));
    assert!(row.uninstalled_at.is_none());
    assert_eq!(installation_count(ctx).await, 1);
}

#[openshop::test(multi_app())]
async fn app_uninstalled_webhook_only_clears_the_matching_app_handle(ctx: &TestContext) {
    let shop = "same-shop.myshopify.com";
    seed_installation(ctx, "clientA", shop, None, Some("token-a")).await;
    seed_installation(ctx, "clientB", shop, None, Some("token-b")).await;
    let body = json!({ "myshopify_domain": shop }).to_string();

    // Neither the environment secret nor an unknown one selects an app.
    for secret in [SECRET, "client-c-secret"] {
        let response = post_uninstall(ctx, &body, &sign_webhook_body(&body, secret), Some(shop)).await;
        assert_error(response, 401, "Invalid webhook HMAC").await;
    }
    assert!(installation(ctx, "clientB", shop).await.access_token.is_some());

    let response = post_uninstall(ctx, &body, &sign_webhook_body(&body, "client-b-secret"), Some(shop)).await;
    assert_eq!(response.status(), 200);

    let row_a = installation(ctx, "clientA", shop).await;
    assert_eq!(row_a.access_token.as_deref(), Some("token-a"));
    assert!(row_a.uninstalled_at.is_none());
    let row_b = installation(ctx, "clientB", shop).await;
    assert!(row_b.access_token.is_none());
    assert!(row_b.uninstalled_at.is_some());
}

// ─── Session-token middleware ────────────────────────────────────────

#[openshop::test(single_app())]
async fn api_without_a_bearer_session_token_returns_401(ctx: &TestContext) {
    let token = ctx.session_token();

    assert_error(get(ctx, "/api/flows").await, 401, "Unauthorized: missing session token").await;
    for authorization in [
        "Bearer".to_string(),
        "Basic dGVzdDp0ZXN0".to_string(),
        token.clone(),
        format!("bearer {token}"),
        format!("Token {token}"),
    ] {
        let response = ctx
            .http
            .get(format!("{}/api/flows", ctx.url))
            .header("authorization", &authorization)
            .send()
            .await
            .unwrap();
        assert_error(response, 401, "Unauthorized: missing session token").await;
    }
    // Shopify also passes the token as `id_token` on page loads; the API only reads the header.
    let response = get(ctx, &format!("/api/flows?id_token={token}&session={token}")).await;
    assert_error(response, 401, "Unauthorized: missing session token").await;

    assert!(ctx.transport.requests().is_empty());
}

#[openshop::test(single_app())]
async fn api_with_a_valid_session_token_is_scoped_to_its_shop_and_app(ctx: &TestContext) {
    let response = ctx.get("/api/flows").send().await.unwrap();
    assert_eq!(response.status(), 200);
    let flows: Value = response.json().await.unwrap();
    assert_eq!(flows[0]["name"], "auth-flow");

    let response = ctx.post("/api/flows/auth-flow/run").send().await.unwrap();
    assert_eq!(response.status(), 202);
    let (app_handle, shop): (String, String) = sqlx::query_as("SELECT app_handle, shop FROM flow_runs")
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!((app_handle.as_str(), shop.as_str()), ("default", SHOP));
}

#[openshop::test(single_app())]
async fn unknown_api_paths_require_a_session_token_before_returning_404(ctx: &TestContext) {
    for path in ["/api", "/api/", "/api/unknown", "/api/installations/active"] {
        assert_error(get(ctx, path).await, 401, "Unauthorized: missing session token").await;
        let response = ctx.get(path).send().await.unwrap();
        assert_eq!(response.status(), 404, "{path}");
        assert_eq!(response.text().await.unwrap(), "404 Not Found", "{path}");
    }
}

#[openshop::test(single_app())]
async fn session_token_request_exchanges_the_token_and_stores_it_encrypted(ctx: &TestContext) {
    ctx.transport
        .respond_json(json!({ "access_token": "offline-token", "scope": "read_products,write_orders" }));
    let token = ctx.session_token();

    assert_eq!(api_get(ctx, "/api/flows", &token).await.status(), 200);

    let requests = ctx.transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, "https://test.myshopify.com/admin/oauth/access_token");
    assert_eq!(
        requests[0].body,
        json!({
            "client_id": "test-app",
            "client_secret": SECRET,
            "grant_type": "urn:ietf:params:oauth:grant-type:token-exchange",
            "subject_token": token,
            "subject_token_type": "urn:ietf:params:oauth:token-type:id_token",
            "requested_token_type": "urn:shopify:params:oauth:token-type:offline-access-token",
        })
    );

    let row = installation(ctx, "default", SHOP).await;
    let stored = row.access_token.clone().expect("stored token");
    assert!(stored.starts_with(ENCRYPTED_PREFIX), "{stored}");
    assert!(!stored.contains("offline-token"));
    assert_eq!(decrypted_token(ctx, &row).as_deref(), Some("offline-token"));
    assert_eq!(row.scopes.as_deref(), Some("read_products,write_orders"));
    assert!(row.uninstalled_at.is_none());

    // Once installed, later requests do not exchange again.
    assert_eq!(api_get(ctx, "/api/flows", &token).await.status(), 200);
    assert_eq!(ctx.transport.requests().len(), 1);
}

#[tokio::test]
async fn session_token_request_skips_the_token_exchange_for_installed_shops() {
    let options = TestOptions {
        access_token: Some("existing-token".to_string()),
        ..with_encryption()
    };
    let ctx = TestContext::with_options(config(), options).await.unwrap();
    ctx.transport
        .respond_json(json!({ "access_token": "replacement-token", "scope": "write_everything" }));

    assert_eq!(ctx.get("/api/flows").send().await.unwrap().status(), 200);

    assert!(ctx.transport.requests().is_empty());
    let row = installation(&ctx, "default", SHOP).await;
    assert_eq!(row.access_token.as_deref(), Some("existing-token"));
    assert_eq!(row.scopes.as_deref(), Some("read_products,read_orders"));
    ctx.shutdown().await.unwrap();
}

/// Lets `duration` pass for the process, as a timer sees it.
async fn wait(duration: Duration) {
    tokio::time::pause();
    tokio::time::advance(duration).await;
    tokio::time::resume();
}

fn exchange_requests(ctx: &TestContext) -> usize {
    ctx.transport
        .requests()
        .iter()
        .filter(|request| request.body["grant_type"] == TOKEN_EXCHANGE)
        .count()
}

const TOKEN_EXCHANGE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const HELD_BACK: Duration = Duration::from_secs(31);

#[openshop::test(single_app())]
async fn session_token_request_survives_a_failed_token_exchange(ctx: &TestContext) {
    // The default mock answers 404.
    assert_eq!(ctx.get("/api/flows").send().await.unwrap().status(), 200);
    wait(HELD_BACK).await;
    ctx.transport.respond_with(|_| Err(anyhow!("connection reset")));
    assert_eq!(ctx.get("/api/flows").send().await.unwrap().status(), 200);
    wait(HELD_BACK).await;
    ctx.transport.respond_json(json!({ "scope": "read_products" }));
    assert_eq!(ctx.get("/api/flows").send().await.unwrap().status(), 200);
    wait(HELD_BACK).await;
    ctx.transport
        .respond_with(|_| Ok(MockTransport::json(200, json!("not an object"))));
    assert_eq!(ctx.get("/api/flows").send().await.unwrap().status(), 200);

    // Every failure is served: four exchanges, each followed by the grant.
    let requests = ctx.transport.requests();
    assert_eq!(requests.len(), 8);
    for pair in requests.chunks(2) {
        assert_eq!(pair[0].body["grant_type"], TOKEN_EXCHANGE);
        assert_eq!(pair[1].body["grant_type"], "client_credentials");
    }
    assert_eq!(installation_count(ctx).await, 0);
}

#[openshop::test(single_app())]
async fn concurrent_requests_of_a_shop_without_a_token_share_one_token_exchange(ctx: &TestContext) {
    grant_access_token(ctx, "shared-token");
    // Slow enough for every request to arrive while the exchange is out.
    ctx.transport.delay_responses(Duration::from_millis(400));
    let token = ctx.session_token();

    let statuses = futures::future::join_all((0..8).map(|_| api_get(ctx, "/api/flows", &token))).await;
    assert!(statuses.iter().all(|response| response.status() == 200));

    assert_eq!(exchange_requests(ctx), 1);
    let row = installation(ctx, "default", SHOP).await;
    assert_eq!(decrypted_token(ctx, &row).as_deref(), Some("shared-token"));
}

#[openshop::test(single_app())]
async fn a_shop_does_not_wait_for_the_token_exchange_of_another(ctx: &TestContext) {
    grant_access_token(ctx, "token");
    ctx.transport.delay_responses(Duration::from_millis(400));
    let token = ctx.session_token();
    let other = ctx.session_token_for("other.myshopify.com", "1");

    let started = std::time::Instant::now();
    let (first, second) = futures::join!(api_get(ctx, "/api/flows", &token), api_get(ctx, "/api/flows", &other));
    assert_eq!((first.status().as_u16(), second.status().as_u16()), (200, 200));
    // Two exchanges, side by side: the second did not wait for the first.
    assert_eq!(exchange_requests(ctx), 2);
    assert!(
        started.elapsed() < Duration::from_millis(780),
        "{:?}",
        started.elapsed()
    );
}

#[openshop::test(single_app())]
async fn a_refused_token_exchange_is_not_tried_again_for_thirty_seconds(ctx: &TestContext) {
    ctx.transport
        .respond_with(|_| Ok(MockTransport::json(400, json!({ "error": "invalid_subject_token" }))));
    let token = ctx.session_token();

    for _ in 0..4 {
        assert_eq!(api_get(ctx, "/api/flows", &token).await.status(), 200);
    }
    assert_eq!(exchange_requests(ctx), 1);

    // Shopify is back: the next request after the window gets the token.
    grant_access_token(ctx, "late-token");
    wait(HELD_BACK).await;
    assert_eq!(api_get(ctx, "/api/flows", &token).await.status(), 200);
    assert_eq!(exchange_requests(ctx), 2);
    let row = installation(ctx, "default", SHOP).await;
    assert_eq!(decrypted_token(ctx, &row).as_deref(), Some("late-token"));
}

#[openshop::test(single_app())]
async fn a_token_exchange_slower_than_five_seconds_fails_fast(ctx: &TestContext) {
    grant_access_token(ctx, "too-late");
    ctx.transport.delay_responses(Duration::from_secs(3600));
    let token = ctx.session_token();

    let started = std::time::Instant::now();
    let response = tokio::time::timeout(Duration::from_secs(9), api_get(ctx, "/api/flows", &token))
        .await
        .expect("the request waited for Shopify");
    // The request is served, without a token.
    assert_eq!(response.status(), 200);
    assert!(started.elapsed() < Duration::from_secs(9), "{:?}", started.elapsed());
    assert!(
        find_installation(ctx, "default", SHOP)
            .await
            .is_none_or(|row| row.access_token.is_none())
    );

    // The failure is remembered like a refusal.
    assert_eq!(api_get(ctx, "/api/flows", &token).await.status(), 200);
    assert_eq!(exchange_requests(ctx), 1);
}

#[openshop::test(single_app())]
async fn rejected_session_tokens_never_trigger_a_token_exchange(ctx: &TestContext) {
    grant_access_token(ctx, "must-not-be-stored");

    let forged = session_token(SHOP, "wrong-secret", "test-app", "1");
    let response = api_get(ctx, "/api/flows", &forged).await;
    assert_error(response, 401, "Unauthorized: invalid session token").await;

    assert!(ctx.transport.requests().is_empty());
    assert_eq!(installation_count(ctx).await, 0);
}

#[openshop::test(single_app())]
async fn token_exchange_reinstalls_a_shop_after_the_uninstall_webhook(ctx: &TestContext) {
    grant_access_token(ctx, "first-token");
    assert_eq!(ctx.get("/api/flows").send().await.unwrap().status(), 200);
    let first = installation(ctx, "default", SHOP).await;
    assert_eq!(decrypted_token(ctx, &first).as_deref(), Some("first-token"));

    let body = json!({ "myshopify_domain": SHOP }).to_string();
    let response = post_uninstall(ctx, &body, &sign_webhook_body(&body, SECRET), Some(SHOP)).await;
    assert_eq!(response.status(), 200);
    let uninstalled = installation(ctx, "default", SHOP).await;
    assert!(uninstalled.access_token.is_none());
    assert!(uninstalled.uninstalled_at.is_some());

    grant_access_token(ctx, "second-token");
    assert_eq!(ctx.get("/api/flows").send().await.unwrap().status(), 200);
    let reinstalled = installation(ctx, "default", SHOP).await;
    assert_eq!(reinstalled.id, first.id);
    assert_eq!(decrypted_token(ctx, &reinstalled).as_deref(), Some("second-token"));
    assert!(reinstalled.uninstalled_at.is_none());
    assert_eq!(ctx.transport.requests().len(), 2);
}

#[openshop::test(TestContext::new(config()))]
async fn token_exchange_stores_the_token_in_plaintext_without_an_encryption_key(ctx: &TestContext) {
    grant_access_token(ctx, "plaintext-token");

    assert_eq!(ctx.get("/api/flows").send().await.unwrap().status(), 200);

    assert_eq!(
        installation(ctx, "default", SHOP).await.access_token.as_deref(),
        Some("plaintext-token")
    );
}

#[tokio::test]
async fn token_exchange_in_production_without_an_encryption_key_never_writes_plaintext() {
    let options = TestOptions {
        production: true,
        encryption_key: None,
        ..Default::default()
    };
    let ctx = TestContext::with_options(config(), options).await.unwrap();
    grant_access_token(&ctx, "production-token");

    // The request itself is served: installation is best-effort.
    assert_eq!(ctx.get("/api/flows").send().await.unwrap().status(), 200);

    assert_eq!(ctx.transport.requests().len(), 1);
    let stored = find_installation(&ctx, "default", SHOP).await;
    assert!(stored.is_none_or(|row| row.access_token.is_none()));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn nothing_authenticates_without_a_configured_secret() {
    for secret in ["", "   "] {
        let options = TestOptions {
            secret: secret.to_string(),
            ..with_encryption()
        };
        let ctx = TestContext::with_options(config(), options).await.unwrap();
        seed_installation(&ctx, "default", SHOP, None, Some("token")).await;
        grant_access_token(&ctx, "must-not-be-stored");

        // Each credential is signed with the blank secret itself.
        let response = api_get(&ctx, "/api/flows", &ctx.session_token()).await;
        assert_error(response, 401, "Unauthorized: invalid session token").await;

        let response = get(&ctx, &callback_path(SHOP, "c", "nonce-1", secret)).await;
        assert_error(response, 401, "Invalid HMAC").await;

        // A state signed with a blank secret would be one anybody can make.
        let response = get(&ctx, "/auth?shop=blank.myshopify.com").await;
        assert_error(response, 500, "[openshop] The Shopify app has no API secret").await;

        let body = json!({ "myshopify_domain": SHOP }).to_string();
        let response = post_uninstall(&ctx, &body, &sign_webhook_body(&body, secret), Some(SHOP)).await;
        assert_error(response, 401, "Invalid webhook HMAC").await;

        assert!(ctx.transport.requests().is_empty());
        let row = installation(&ctx, "default", SHOP).await;
        assert_eq!(row.access_token.as_deref(), Some("token"));
        assert!(row.uninstalled_at.is_none());
        ctx.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn a_malformed_encryption_key_never_falls_back_to_plaintext() {
    for encryption_key in ["too-short", &"z".repeat(64)] {
        let options = TestOptions {
            encryption_key: Some(encryption_key.to_string()),
            ..Default::default()
        };
        let ctx = TestContext::with_options(config(), options).await.unwrap();
        let shop = "bad-key.myshopify.com";
        let started = begin(&ctx, shop).await;
        grant_access_token(&ctx, "bad-key-token");

        let response = finish(&ctx, shop, &started, SECRET).await;
        assert_eq!(response.status(), 500);
        assert_eq!(response.text().await.unwrap(), "Internal Server Error");
        assert_eq!(ctx.get("/api/flows").send().await.unwrap().status(), 200);

        assert!(find_installation(&ctx, "default", shop).await.is_none());
        let stored = find_installation(&ctx, "default", SHOP).await;
        assert!(stored.is_none_or(|row| row.access_token.is_none()));
        ctx.shutdown().await.unwrap();
    }
}

#[openshop::test(multi_app())]
async fn session_token_is_resolved_by_audience_in_multi_app_mode(ctx: &TestContext) {
    let shop = "same-shop.myshopify.com";
    ctx.transport.respond_with(|request| {
        let client_id = request.body["client_id"].as_str().unwrap_or_default();
        Ok(MockTransport::json(
            200,
            json!({ "access_token": format!("token-for-{client_id}"), "scope": "read_products" }),
        ))
    });

    let token_b = session_token(shop, "client-b-secret", "client-b-key", "7");
    assert_eq!(api_get(ctx, "/api/flows", &token_b).await.status(), 200);

    let requests = ctx.transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].url,
        "https://same-shop.myshopify.com/admin/oauth/access_token"
    );
    assert_eq!(requests[0].body["client_id"], "client-b-key");
    assert_eq!(requests[0].body["client_secret"], "client-b-secret");
    assert_eq!(requests[0].body["subject_token"], token_b.as_str());
    let row_b = installation(ctx, "clientB", shop).await;
    assert_eq!(decrypted_token(ctx, &row_b).as_deref(), Some("token-for-client-b-key"));
    assert!(find_installation(ctx, "clientA", shop).await.is_none());
    assert!(find_installation(ctx, "default", shop).await.is_none());

    // Work started with a token of app B belongs to app B.
    let response = ctx
        .http
        .post(format!("{}/api/flows/auth-flow/run", ctx.url))
        .bearer_auth(&token_b)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    let (app_handle,): (String,) = sqlx::query_as("SELECT app_handle FROM flow_runs WHERE shop = $1")
        .bind(shop)
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(app_handle, "clientB");

    let token_a = session_token(shop, "client-a-secret", "client-a-key", "7");
    assert_eq!(api_get(ctx, "/api/flows", &token_a).await.status(), 200);
    let row_a = installation(ctx, "clientA", shop).await;
    assert_eq!(decrypted_token(ctx, &row_a).as_deref(), Some("token-for-client-a-key"));
    assert_eq!(installation_count(ctx).await, 2);
}

#[openshop::test(multi_app())]
async fn multi_app_session_tokens_cannot_be_used_across_apps(ctx: &TestContext) {
    let shop = "same-shop.myshopify.com";
    grant_access_token(ctx, "must-not-be-stored");

    // Audience of app A, signed with the secret of app B.
    let crossed = session_token(shop, "client-b-secret", "client-a-key", "1");
    let response = api_get(ctx, "/api/flows", &crossed).await;
    assert_error(response, 401, "Unauthorized: invalid session token").await;

    // The environment credentials of single-app mode are not an app here.
    let env_token = ctx.session_token_for(shop, "1");
    let response = api_get(ctx, "/api/flows", &env_token).await;
    assert_error(response, 401, "Unauthorized: invalid session token").await;

    // Neither is an app handle an audience.
    let by_handle = session_token(shop, "client-a-secret", "clientA", "1");
    let response = api_get(ctx, "/api/flows", &by_handle).await;
    assert_error(response, 401, "Unauthorized: invalid session token").await;

    assert!(ctx.transport.requests().is_empty());
    assert_eq!(installation_count(ctx).await, 0);
}
