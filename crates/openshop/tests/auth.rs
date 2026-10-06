//! OAuth (`/auth`, `/auth/callback`), the uninstall webhook and the session-token
//! authentication of the Admin API.

mod common;

use chrono::Utc;
use common::noop_flow;
use openshop::db::schema::Installation;
use openshop::testing::{
    MockTransport, TestContext, TestOptions, create_session_token, sign_query, sign_webhook_body, to_query_string,
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

async fn find_installation(ctx: &TestContext, app_handle: &str, shop: &str) -> Option<Installation> {
    sqlx::query_as("SELECT * FROM installations WHERE app_handle = $1 AND shop = $2")
        .bind(app_handle)
        .bind(shop)
        .fetch_optional(&ctx.db)
        .await
        .unwrap()
}

async fn installation(ctx: &TestContext, app_handle: &str, shop: &str) -> Installation {
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

fn decrypted_token(ctx: &TestContext, row: &Installation) -> Option<String> {
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

fn grant_access_token(ctx: &TestContext, access_token: &str) {
    ctx.transport
        .respond_json(json!({ "access_token": access_token, "scope": "read_products" }));
}

/// Runs a GraphQL query as the shop and returns the access token sent to Shopify.
async fn shopify_access_token_header(ctx: &TestContext, shop: &str) -> String {
    ctx.transport.clear();
    ctx.transport
        .respond_json(json!({ "data": { "shop": { "name": "Test" } } }));
    let client = ctx.app.shopify_client(shop, "default").await.unwrap();
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

#[tokio::test]
async fn get_auth_without_shop_returns_400() {
    let ctx = single_app().await;
    assert_error(get(&ctx, "/auth").await, 400, "Missing shop parameter").await;
    assert_error(get(&ctx, "/auth?shop=").await, 400, "Missing shop parameter").await;
    assert_eq!(installation_count(&ctx).await, 0);
    ctx.shutdown().await.unwrap();
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

    let nonce = installation(&ctx, "default", "redirect-shop.myshopify.com")
        .await
        .nonce
        .expect("nonce");
    assert_eq!(nonce.len(), 32);
    assert!(nonce.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(location.ends_with(&format!("&state={nonce}")), "{location}");
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_auth_with_multiple_apps_requires_an_explicit_app_handle() {
    let ctx = multi_app().await;
    assert_error(
        get(&ctx, "/auth?shop=multi-auth.myshopify.com").await,
        400,
        "[openshop] Missing Shopify app handle",
    )
    .await;
    assert_eq!(installation_count(&ctx).await, 0);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_auth_with_unknown_app_handle_returns_400() {
    let ctx = multi_app().await;
    assert_error(
        get(&ctx, "/auth?shop=multi-auth.myshopify.com&app=clientC").await,
        400,
        "[openshop] Unknown Shopify app \"clientC\"",
    )
    .await;
    assert_error(
        get(&ctx, "/auth?shop=multi-auth.myshopify.com&app=").await,
        400,
        "[openshop] Missing Shopify app handle",
    )
    .await;
    assert_eq!(installation_count(&ctx).await, 0);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_auth_with_app_handle_builds_the_matching_app_oauth_redirect() {
    let ctx = multi_app().await;
    let response = get(&ctx, "/auth?shop=multi-auth.myshopify.com&app=clientA").await;
    assert_eq!(response.status(), 302);
    let location = location(&response);
    assert!(location.contains("client_id=client-a-key"), "{location}");
    assert!(location.contains("scope=read_products"), "{location}");
    assert!(
        location.contains("https%3A%2F%2Fclient-a.example.test%2Fauth%2Fcallback"),
        "{location}"
    );

    let row = installation(&ctx, "clientA", "multi-auth.myshopify.com").await;
    assert_eq!(row.shop, "multi-auth.myshopify.com");
    assert!(row.nonce.is_some());
    assert!(
        find_installation(&ctx, "clientB", "multi-auth.myshopify.com")
            .await
            .is_none()
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_auth_rejects_invalid_shop_domain() {
    let ctx = single_app().await;
    assert_error(
        get(&ctx, "/auth?shop=evil.example.com").await,
        400,
        "Invalid shop parameter",
    )
    .await;
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_auth_rejects_shop_domain_normalization_attacks() {
    let ctx = single_app().await;
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
    assert_eq!(installation_count(&ctx).await, 0);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_auth_always_redirects_to_the_normalized_shop_host() {
    let ctx = single_app().await;
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
    assert_eq!(installation_count(&ctx).await, 1);
    assert!(
        installation(&ctx, "default", "normalized.myshopify.com")
            .await
            .nonce
            .is_some()
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_auth_rotates_the_nonce_and_keeps_the_stored_token() {
    let ctx = single_app().await;
    seed_installation(&ctx, "default", "rotate.myshopify.com", None, Some("existing-token")).await;

    let first = location(&get(&ctx, "/auth?shop=rotate.myshopify.com").await);
    let first_nonce = installation(&ctx, "default", "rotate.myshopify.com")
        .await
        .nonce
        .unwrap();
    let second = location(&get(&ctx, "/auth?shop=rotate.myshopify.com").await);
    let row = installation(&ctx, "default", "rotate.myshopify.com").await;
    let second_nonce = row.nonce.clone().unwrap();

    assert_ne!(first_nonce, second_nonce);
    assert!(first.ends_with(&format!("&state={first_nonce}")));
    assert!(second.ends_with(&format!("&state={second_nonce}")));
    assert_eq!(row.access_token.as_deref(), Some("existing-token"));
    assert_eq!(installation_count(&ctx).await, 1);
    ctx.shutdown().await.unwrap();
}

// ─── GET /auth/callback ──────────────────────────────────────────────

#[tokio::test]
async fn get_auth_callback_without_required_parameters_returns_400() {
    let ctx = single_app().await;
    for query in [
        vec![("code", "c"), ("state", "s")],
        vec![("shop", "x.myshopify.com"), ("state", "s")],
        vec![("shop", "x.myshopify.com"), ("code", "c")],
        vec![("shop", "x.myshopify.com"), ("code", ""), ("state", "s")],
    ] {
        let path = format!("/auth/callback?{}", to_query_string(&sign_query(&query, SECRET)));
        assert_error(get(&ctx, &path).await, 400, "Missing required parameters").await;
    }
    assert!(ctx.transport.requests().is_empty());
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_auth_callback_rejects_invalid_hmac() {
    let ctx = single_app().await;
    seed_installation(&ctx, "default", "x.myshopify.com", Some("s"), None).await;

    let response = get(&ctx, "/auth/callback?shop=x.myshopify.com&code=c&state=s&hmac=deadbeef").await;
    assert_error(response, 401, "Invalid HMAC").await;
    let response = get(&ctx, "/auth/callback?shop=x.myshopify.com&code=c&state=s").await;
    assert_error(response, 401, "Invalid HMAC").await;

    assert!(ctx.transport.requests().is_empty());
    assert_eq!(
        installation(&ctx, "default", "x.myshopify.com").await.nonce.as_deref(),
        Some("s")
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_auth_callback_rejects_a_query_signed_with_another_secret() {
    let ctx = single_app().await;
    seed_installation(&ctx, "default", "x.myshopify.com", Some("s"), None).await;
    grant_access_token(&ctx, "must-not-be-stored");

    let path = callback_path("x.myshopify.com", "c", "s", "another-secret");
    assert_error(get(&ctx, &path).await, 401, "Invalid HMAC").await;

    assert!(ctx.transport.requests().is_empty());
    assert!(
        installation(&ctx, "default", "x.myshopify.com")
            .await
            .access_token
            .is_none()
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_auth_callback_rejects_tampered_parameters() {
    let ctx = single_app().await;
    seed_installation(&ctx, "default", "x.myshopify.com", Some("s"), None).await;
    seed_installation(&ctx, "default", "y.myshopify.com", Some("s"), None).await;
    grant_access_token(&ctx, "must-not-be-stored");

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
        assert_error(get(&ctx, &path).await, 401, "Invalid HMAC").await;
    }

    // A valid signature followed by garbage is not a valid signature.
    let mut query = signed_callback("x.myshopify.com", "c", "s", SECRET);
    query["hmac"].push_str("00");
    let path = format!("/auth/callback?{}", to_query_string(&query));
    assert_error(get(&ctx, &path).await, 401, "Invalid HMAC").await;

    assert!(ctx.transport.requests().is_empty());
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_auth_callback_rejects_valid_hmac_with_invalid_shop_domain() {
    let ctx = single_app().await;
    let path = callback_path("evil.example.com", "c", "s", SECRET);
    assert_error(get(&ctx, &path).await, 400, "Invalid shop parameter").await;
    assert!(ctx.transport.requests().is_empty());
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_auth_callback_rejects_shop_domain_normalization_attacks() {
    let ctx = single_app().await;
    seed_installation(&ctx, "default", "x.myshopify.com", Some("s"), None).await;
    grant_access_token(&ctx, "must-not-be-stored");

    for shop in [
        "evil.com/x.myshopify.com",
        "https://evil.com/x.myshopify.com",
        "x.myshopify.com@evil.com",
        "evil.com#x.myshopify.com",
        "evil.com\\x.myshopify.com",
        "x.myshopify.com.evil.com",
    ] {
        let path = callback_path(shop, "c", "s", SECRET);
        assert_error(get(&ctx, &path).await, 400, "Invalid shop parameter").await;
    }

    assert!(ctx.transport.requests().is_empty());
    assert!(
        installation(&ctx, "default", "x.myshopify.com")
            .await
            .access_token
            .is_none()
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_auth_callback_with_valid_hmac_and_mocked_token_exchange_updates_installation() {
    let options = TestOptions {
        api_key: "app-key-for-callback".to_string(),
        ..with_encryption()
    };
    let ctx = TestContext::with_options(config(), options).await.unwrap();
    let shop = "oauth-ok.myshopify.com";
    let state = "expected-nonce-123";
    seed_installation(&ctx, "default", shop, Some(state), None).await;
    grant_access_token(&ctx, "stored-access-token");

    let response = get(&ctx, &callback_path(shop, "temporary-code", state, SECRET)).await;
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
        json!({ "client_id": "app-key-for-callback", "client_secret": SECRET, "code": "temporary-code" })
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

#[tokio::test]
async fn get_auth_callback_resolves_the_app_from_hmac_and_isolates_installations_by_app() {
    let ctx = multi_app().await;
    let shop = "same-shop.myshopify.com";
    seed_installation(&ctx, "clientA", shop, Some("nonce-client-a"), None).await;
    seed_installation(&ctx, "clientB", shop, Some("nonce-client-b"), None).await;
    grant_access_token(&ctx, "stored-token-client-b");

    let path = callback_path(shop, "temporary-code-client-b", "nonce-client-b", "client-b-secret");
    let response = get(&ctx, &path).await;
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
            "code": "temporary-code-client-b",
        })
    );

    let row_a = installation(&ctx, "clientA", shop).await;
    let row_b = installation(&ctx, "clientB", shop).await;
    assert!(row_a.access_token.is_none());
    assert_eq!(row_a.nonce.as_deref(), Some("nonce-client-a"));
    assert_eq!(decrypted_token(&ctx, &row_b).as_deref(), Some("stored-token-client-b"));
    assert!(row_b.nonce.is_none());
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_auth_callback_completes_the_flow_started_by_get_auth() {
    let ctx = single_app().await;
    let shop = "full-flow.myshopify.com";
    let authorize = location(&get(&ctx, &format!("/auth?shop={shop}")).await);
    let state = authorize.rsplit_once("&state=").expect("state parameter").1;
    grant_access_token(&ctx, "full-flow-token");

    let response = get(&ctx, &callback_path(shop, "code-1", state, SECRET)).await;
    assert_eq!(response.status(), 302);
    assert_eq!(
        location(&response),
        "https://full-flow.myshopify.com/admin/apps/test-app"
    );

    let row = installation(&ctx, "default", shop).await;
    assert_eq!(decrypted_token(&ctx, &row).as_deref(), Some("full-flow-token"));
    assert!(row.nonce.is_none());

    // The stored ciphertext is what authenticates later Admin API calls.
    assert_eq!(shopify_access_token_header(&ctx, shop).await, "full-flow-token");
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_auth_callback_rejects_unknown_or_mismatched_state() {
    let ctx = single_app().await;
    grant_access_token(&ctx, "must-not-be-stored");

    // No authorization was ever started for this shop.
    let path = callback_path("never-started.myshopify.com", "c", "s", SECRET);
    assert_error(get(&ctx, &path).await, 401, "Invalid state/nonce").await;
    assert_eq!(installation_count(&ctx).await, 0);

    seed_installation(&ctx, "default", "started.myshopify.com", Some("expected-nonce"), None).await;
    for state in ["another-nonce", "expected-nonc", "expected-nonce-", "EXPECTED-NONCE"] {
        let path = callback_path("started.myshopify.com", "c", state, SECRET);
        assert_error(get(&ctx, &path).await, 401, "Invalid state/nonce").await;
    }

    // An installed shop without a pending authorization has no nonce to match.
    seed_installation(&ctx, "default", "installed.myshopify.com", None, Some("existing-token")).await;
    for state in ["null", "undefined", "s"] {
        let path = callback_path("installed.myshopify.com", "c", state, SECRET);
        assert_error(get(&ctx, &path).await, 401, "Invalid state/nonce").await;
    }

    assert!(ctx.transport.requests().is_empty());
    let started = installation(&ctx, "default", "started.myshopify.com").await;
    assert_eq!(started.nonce.as_deref(), Some("expected-nonce"));
    assert!(started.access_token.is_none());
    let installed = installation(&ctx, "default", "installed.myshopify.com").await;
    assert_eq!(installed.access_token.as_deref(), Some("existing-token"));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_auth_callback_rejects_a_replayed_state() {
    let ctx = single_app().await;
    let shop = "replay.myshopify.com";
    seed_installation(&ctx, "default", shop, Some("one-time-nonce"), None).await;
    grant_access_token(&ctx, "first-token");

    let path = callback_path(shop, "code-1", "one-time-nonce", SECRET);
    assert_eq!(get(&ctx, &path).await.status(), 302);

    grant_access_token(&ctx, "second-token");
    assert_error(get(&ctx, &path).await, 401, "Invalid state/nonce").await;
    let path = callback_path(shop, "code-2", "one-time-nonce", SECRET);
    assert_error(get(&ctx, &path).await, 401, "Invalid state/nonce").await;

    assert_eq!(ctx.transport.requests().len(), 1);
    let row = installation(&ctx, "default", shop).await;
    assert_eq!(decrypted_token(&ctx, &row).as_deref(), Some("first-token"));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_auth_callback_rejects_a_state_issued_for_another_shop() {
    let ctx = single_app().await;
    seed_installation(&ctx, "default", "shop-a.myshopify.com", Some("nonce-a"), None).await;
    seed_installation(&ctx, "default", "shop-b.myshopify.com", Some("nonce-b"), None).await;
    grant_access_token(&ctx, "must-not-be-stored");

    let path = callback_path("shop-b.myshopify.com", "c", "nonce-a", SECRET);
    assert_error(get(&ctx, &path).await, 401, "Invalid state/nonce").await;

    assert!(ctx.transport.requests().is_empty());
    for (shop, nonce) in [("shop-a.myshopify.com", "nonce-a"), ("shop-b.myshopify.com", "nonce-b")] {
        let row = installation(&ctx, "default", shop).await;
        assert_eq!(row.nonce.as_deref(), Some(nonce));
        assert!(row.access_token.is_none());
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_auth_callback_rejects_a_state_issued_for_another_app() {
    let ctx = multi_app().await;
    let shop = "same-shop.myshopify.com";
    seed_installation(&ctx, "clientA", shop, Some("nonce-client-a"), None).await;
    seed_installation(&ctx, "clientB", shop, Some("nonce-client-b"), None).await;
    grant_access_token(&ctx, "must-not-be-stored");

    let path = callback_path(shop, "c", "nonce-client-a", "client-b-secret");
    assert_error(get(&ctx, &path).await, 401, "Invalid state/nonce").await;
    // The single-app environment secret is not an app of a multi-app instance.
    let path = callback_path(shop, "c", "nonce-client-a", SECRET);
    assert_error(get(&ctx, &path).await, 401, "Invalid HMAC").await;

    assert!(ctx.transport.requests().is_empty());
    for handle in ["clientA", "clientB"] {
        assert!(installation(&ctx, handle, shop).await.access_token.is_none());
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_auth_callback_reports_a_failed_token_exchange() {
    let ctx = single_app().await;
    let shop = "exchange-fails.myshopify.com";
    seed_installation(&ctx, "default", shop, Some("nonce-1"), None).await;
    ctx.transport
        .respond_with(|_| Ok(MockTransport::json(400, json!({ "error": "invalid_request" }))));

    let response = get(&ctx, &callback_path(shop, "expired-code", "nonce-1", SECRET)).await;
    assert_error(response, 500, "Token exchange failed: {\"error\":\"invalid_request\"}").await;

    ctx.transport.respond_with(|_| Err(anyhow!("connection reset")));
    let response = get(&ctx, &callback_path(shop, "expired-code", "nonce-1", SECRET)).await;
    assert_eq!(response.status(), 500);
    assert_eq!(response.text().await.unwrap(), "Internal Server Error");

    let row = installation(&ctx, "default", shop).await;
    assert!(row.access_token.is_none());
    assert_eq!(row.nonce.as_deref(), Some("nonce-1"));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_auth_callback_reinstalls_an_uninstalled_shop() {
    let ctx = single_app().await;
    let shop = "reinstall.myshopify.com";
    sqlx::query("INSERT INTO installations (shop, nonce, uninstalled_at) VALUES ($1, 'nonce-1', now())")
        .bind(shop)
        .execute(&ctx.db)
        .await
        .unwrap();
    grant_access_token(&ctx, "fresh-token");

    let response = get(&ctx, &callback_path(shop, "c", "nonce-1", SECRET)).await;
    assert_eq!(response.status(), 302);

    let row = installation(&ctx, "default", shop).await;
    assert!(row.uninstalled_at.is_none());
    assert_eq!(decrypted_token(&ctx, &row).as_deref(), Some("fresh-token"));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_auth_callback_stores_the_token_in_plaintext_without_an_encryption_key() {
    let ctx = TestContext::new(config()).await.unwrap();
    let shop = "plaintext.myshopify.com";
    seed_installation(&ctx, "default", shop, Some("nonce-1"), None).await;
    grant_access_token(&ctx, "plaintext-token");

    let response = get(&ctx, &callback_path(shop, "c", "nonce-1", SECRET)).await;
    assert_eq!(response.status(), 302);
    assert_eq!(
        installation(&ctx, "default", shop).await.access_token.as_deref(),
        Some("plaintext-token")
    );
    ctx.shutdown().await.unwrap();
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
    seed_installation(&ctx, "default", shop, Some("nonce-1"), None).await;
    grant_access_token(&ctx, "production-token");

    let response = get(&ctx, &callback_path(shop, "c", "nonce-1", SECRET)).await;
    assert_eq!(response.status(), 500);
    let body = response.text().await.unwrap();
    assert_eq!(body, "Internal Server Error");

    let row = installation(&ctx, "default", shop).await;
    assert!(row.access_token.is_none());
    assert_eq!(row.nonce.as_deref(), Some("nonce-1"));
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
        seed_installation(&ctx, handle, shop, Some("nonce-1"), Some("token")).await;
    }
    grant_access_token(&ctx, "must-not-be-stored");

    let token = create_session_token(shop, "shared-secret", "shared-key", "1");
    assert_error(
        api_get(&ctx, "/api/flows", &token).await,
        401,
        "Unauthorized: [openshop] Multiple Shopify apps use apiKey \"shared-key\"",
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
        assert_eq!(row.nonce.as_deref(), Some("nonce-1"));
    }
    ctx.shutdown().await.unwrap();
}

// ─── POST /auth/webhooks/app-uninstalled ─────────────────────────────

#[tokio::test]
async fn app_uninstalled_webhook_rejects_invalid_hmac() {
    let ctx = single_app().await;
    seed_installation(&ctx, "default", SHOP, None, Some("token")).await;
    let body = json!({ "myshopify_domain": SHOP }).to_string();

    for hmac in [
        "invalid".to_string(),
        String::new(),
        sign_webhook_body(&body, "another-secret"),
        sign_webhook_body("{}", SECRET),
    ] {
        let response = post_uninstall(&ctx, &body, &hmac, Some(SHOP)).await;
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

    let row = installation(&ctx, "default", SHOP).await;
    assert_eq!(row.access_token.as_deref(), Some("token"));
    assert!(row.uninstalled_at.is_none());
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn app_uninstalled_webhook_clears_the_token_of_the_shop() {
    let ctx = single_app().await;
    seed_installation(&ctx, "default", SHOP, None, Some("token")).await;
    seed_installation(&ctx, "default", "other.myshopify.com", None, Some("other-token")).await;
    let body = json!({ "id": 1, "myshopify_domain": "TEST.myshopify.com" }).to_string();

    // The signed payload wins over the unsigned header, and is normalized.
    let response = post_uninstall(
        &ctx,
        &body,
        &sign_webhook_body(&body, SECRET),
        Some("other.myshopify.com"),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.json::<Value>().await.unwrap(), json!({ "ok": true }));

    let row = installation(&ctx, "default", SHOP).await;
    assert!(row.access_token.is_none());
    assert!(row.uninstalled_at.is_some());
    let other = installation(&ctx, "default", "other.myshopify.com").await;
    assert_eq!(other.access_token.as_deref(), Some("other-token"));
    assert!(other.uninstalled_at.is_none());
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn app_uninstalled_webhook_falls_back_to_the_shop_header() {
    let ctx = single_app().await;
    seed_installation(&ctx, "default", SHOP, None, Some("token")).await;
    let body = json!({ "id": 1 }).to_string();

    let response = post_uninstall(&ctx, &body, &sign_webhook_body(&body, SECRET), Some(SHOP)).await;
    assert_eq!(response.status(), 200);

    let row = installation(&ctx, "default", SHOP).await;
    assert!(row.access_token.is_none());
    assert!(row.uninstalled_at.is_some());
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn app_uninstalled_webhook_ignores_unknown_or_invalid_shops() {
    let ctx = single_app().await;
    seed_installation(&ctx, "default", SHOP, None, Some("token")).await;

    // The shop of the signed payload decides, whatever the header claims.
    for shop in [
        "unknown.myshopify.com",
        "evil.example.com",
        "test.myshopify.com.evil.com",
    ] {
        let body = json!({ "myshopify_domain": shop }).to_string();
        let response = post_uninstall(&ctx, &body, &sign_webhook_body(&body, SECRET), Some(SHOP)).await;
        assert_eq!(response.status(), 200, "{shop}");
    }
    let empty = "{}";
    let response = post_uninstall(&ctx, empty, &sign_webhook_body(empty, SECRET), None).await;
    assert_eq!(response.status(), 200);
    // A signed body that is not JSON is an unhandled error, not an uninstall.
    let response = post_uninstall(&ctx, "not json", &sign_webhook_body("not json", SECRET), Some(SHOP)).await;
    assert_eq!(response.status(), 500);
    assert_eq!(response.text().await.unwrap(), "Internal Server Error");

    let row = installation(&ctx, "default", SHOP).await;
    assert_eq!(row.access_token.as_deref(), Some("token"));
    assert!(row.uninstalled_at.is_none());
    assert_eq!(installation_count(&ctx).await, 1);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn app_uninstalled_webhook_only_clears_the_matching_app_handle() {
    let ctx = multi_app().await;
    let shop = "same-shop.myshopify.com";
    seed_installation(&ctx, "clientA", shop, None, Some("token-a")).await;
    seed_installation(&ctx, "clientB", shop, None, Some("token-b")).await;
    let body = json!({ "myshopify_domain": shop }).to_string();

    // Neither the environment secret nor an unknown one selects an app.
    for secret in [SECRET, "client-c-secret"] {
        let response = post_uninstall(&ctx, &body, &sign_webhook_body(&body, secret), Some(shop)).await;
        assert_error(response, 401, "Invalid webhook HMAC").await;
    }
    assert!(installation(&ctx, "clientB", shop).await.access_token.is_some());

    let response = post_uninstall(&ctx, &body, &sign_webhook_body(&body, "client-b-secret"), Some(shop)).await;
    assert_eq!(response.status(), 200);

    let row_a = installation(&ctx, "clientA", shop).await;
    assert_eq!(row_a.access_token.as_deref(), Some("token-a"));
    assert!(row_a.uninstalled_at.is_none());
    let row_b = installation(&ctx, "clientB", shop).await;
    assert!(row_b.access_token.is_none());
    assert!(row_b.uninstalled_at.is_some());
    ctx.shutdown().await.unwrap();
}

// ─── Session-token middleware ────────────────────────────────────────

#[tokio::test]
async fn api_without_a_bearer_session_token_returns_401() {
    let ctx = single_app().await;
    let token = ctx.session_token();

    assert_error(
        get(&ctx, "/api/flows").await,
        401,
        "Unauthorized: missing session token",
    )
    .await;
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
    let response = get(&ctx, &format!("/api/flows?id_token={token}&session={token}")).await;
    assert_error(response, 401, "Unauthorized: missing session token").await;

    assert!(ctx.transport.requests().is_empty());
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn api_with_a_valid_session_token_is_scoped_to_its_shop_and_app() {
    let ctx = single_app().await;
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
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn unknown_api_paths_require_a_session_token_before_returning_404() {
    let ctx = single_app().await;
    for path in ["/api", "/api/", "/api/unknown", "/api/installations/active"] {
        assert_error(get(&ctx, path).await, 401, "Unauthorized: missing session token").await;
        let response = ctx.get(path).send().await.unwrap();
        assert_eq!(response.status(), 404, "{path}");
        assert_eq!(response.text().await.unwrap(), "404 Not Found", "{path}");
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn session_token_request_exchanges_the_token_and_stores_it_encrypted() {
    let ctx = single_app().await;
    ctx.transport
        .respond_json(json!({ "access_token": "offline-token", "scope": "read_products,write_orders" }));
    let token = ctx.session_token();

    assert_eq!(api_get(&ctx, "/api/flows", &token).await.status(), 200);

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

    let row = installation(&ctx, "default", SHOP).await;
    let stored = row.access_token.clone().expect("stored token");
    assert!(stored.starts_with(ENCRYPTED_PREFIX), "{stored}");
    assert!(!stored.contains("offline-token"));
    assert_eq!(decrypted_token(&ctx, &row).as_deref(), Some("offline-token"));
    assert_eq!(row.scopes.as_deref(), Some("read_products,write_orders"));
    assert!(row.uninstalled_at.is_none());

    // Once installed, later requests do not exchange again.
    assert_eq!(api_get(&ctx, "/api/flows", &token).await.status(), 200);
    assert_eq!(ctx.transport.requests().len(), 1);
    ctx.shutdown().await.unwrap();
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

#[tokio::test]
async fn session_token_request_survives_a_failed_token_exchange() {
    let ctx = single_app().await;

    // The default mock answers 404.
    assert_eq!(ctx.get("/api/flows").send().await.unwrap().status(), 200);
    ctx.transport.respond_with(|_| Err(anyhow!("connection reset")));
    assert_eq!(ctx.get("/api/flows").send().await.unwrap().status(), 200);
    ctx.transport.respond_json(json!({ "scope": "read_products" }));
    assert_eq!(ctx.get("/api/flows").send().await.unwrap().status(), 200);
    ctx.transport
        .respond_with(|_| Ok(MockTransport::json(200, json!("not an object"))));
    assert_eq!(ctx.get("/api/flows").send().await.unwrap().status(), 200);

    assert_eq!(ctx.transport.requests().len(), 4);
    assert_eq!(installation_count(&ctx).await, 0);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn rejected_session_tokens_never_trigger_a_token_exchange() {
    let ctx = single_app().await;
    grant_access_token(&ctx, "must-not-be-stored");

    let forged = create_session_token(SHOP, "wrong-secret", "test-app", "1");
    let response = api_get(&ctx, "/api/flows", &forged).await;
    assert_error(response, 401, "Unauthorized: Invalid JWT signature").await;

    assert!(ctx.transport.requests().is_empty());
    assert_eq!(installation_count(&ctx).await, 0);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn token_exchange_reinstalls_a_shop_after_the_uninstall_webhook() {
    let ctx = single_app().await;
    grant_access_token(&ctx, "first-token");
    assert_eq!(ctx.get("/api/flows").send().await.unwrap().status(), 200);
    let first = installation(&ctx, "default", SHOP).await;
    assert_eq!(decrypted_token(&ctx, &first).as_deref(), Some("first-token"));

    let body = json!({ "myshopify_domain": SHOP }).to_string();
    let response = post_uninstall(&ctx, &body, &sign_webhook_body(&body, SECRET), Some(SHOP)).await;
    assert_eq!(response.status(), 200);
    let uninstalled = installation(&ctx, "default", SHOP).await;
    assert!(uninstalled.access_token.is_none());
    assert!(uninstalled.uninstalled_at.is_some());

    grant_access_token(&ctx, "second-token");
    assert_eq!(ctx.get("/api/flows").send().await.unwrap().status(), 200);
    let reinstalled = installation(&ctx, "default", SHOP).await;
    assert_eq!(reinstalled.id, first.id);
    assert_eq!(decrypted_token(&ctx, &reinstalled).as_deref(), Some("second-token"));
    assert!(reinstalled.uninstalled_at.is_none());
    assert_eq!(ctx.transport.requests().len(), 2);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn token_exchange_stores_the_token_in_plaintext_without_an_encryption_key() {
    let ctx = TestContext::new(config()).await.unwrap();
    grant_access_token(&ctx, "plaintext-token");

    assert_eq!(ctx.get("/api/flows").send().await.unwrap().status(), 200);

    assert_eq!(
        installation(&ctx, "default", SHOP).await.access_token.as_deref(),
        Some("plaintext-token")
    );
    ctx.shutdown().await.unwrap();
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
        seed_installation(&ctx, "default", SHOP, Some("nonce-1"), Some("token")).await;
        grant_access_token(&ctx, "must-not-be-stored");

        // Each credential is signed with the blank secret itself.
        let response = api_get(&ctx, "/api/flows", &ctx.session_token()).await;
        assert_error(response, 401, "Unauthorized: SHOPIFY_API_SECRET is not configured").await;

        let response = get(&ctx, &callback_path(SHOP, "c", "nonce-1", secret)).await;
        assert_error(response, 401, "Invalid HMAC").await;

        let body = json!({ "myshopify_domain": SHOP }).to_string();
        let response = post_uninstall(&ctx, &body, &sign_webhook_body(&body, secret), Some(SHOP)).await;
        assert_error(response, 401, "Invalid webhook HMAC").await;

        assert!(ctx.transport.requests().is_empty());
        let row = installation(&ctx, "default", SHOP).await;
        assert_eq!(row.access_token.as_deref(), Some("token"));
        assert_eq!(row.nonce.as_deref(), Some("nonce-1"));
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
        seed_installation(&ctx, "default", shop, Some("nonce-1"), None).await;
        grant_access_token(&ctx, "bad-key-token");

        let response = get(&ctx, &callback_path(shop, "c", "nonce-1", SECRET)).await;
        assert_eq!(response.status(), 500);
        assert_eq!(response.text().await.unwrap(), "Internal Server Error");
        assert_eq!(ctx.get("/api/flows").send().await.unwrap().status(), 200);

        assert!(installation(&ctx, "default", shop).await.access_token.is_none());
        let stored = find_installation(&ctx, "default", SHOP).await;
        assert!(stored.is_none_or(|row| row.access_token.is_none()));
        ctx.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn session_token_is_resolved_by_audience_in_multi_app_mode() {
    let ctx = multi_app().await;
    let shop = "same-shop.myshopify.com";
    ctx.transport.respond_with(|request| {
        let client_id = request.body["client_id"].as_str().unwrap_or_default();
        Ok(MockTransport::json(
            200,
            json!({ "access_token": format!("token-for-{client_id}"), "scope": "read_products" }),
        ))
    });

    let token_b = create_session_token(shop, "client-b-secret", "client-b-key", "7");
    assert_eq!(api_get(&ctx, "/api/flows", &token_b).await.status(), 200);

    let requests = ctx.transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].url,
        "https://same-shop.myshopify.com/admin/oauth/access_token"
    );
    assert_eq!(requests[0].body["client_id"], "client-b-key");
    assert_eq!(requests[0].body["client_secret"], "client-b-secret");
    assert_eq!(requests[0].body["subject_token"], token_b.as_str());
    let row_b = installation(&ctx, "clientB", shop).await;
    assert_eq!(decrypted_token(&ctx, &row_b).as_deref(), Some("token-for-client-b-key"));
    assert!(find_installation(&ctx, "clientA", shop).await.is_none());
    assert!(find_installation(&ctx, "default", shop).await.is_none());

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

    let token_a = create_session_token(shop, "client-a-secret", "client-a-key", "7");
    assert_eq!(api_get(&ctx, "/api/flows", &token_a).await.status(), 200);
    let row_a = installation(&ctx, "clientA", shop).await;
    assert_eq!(decrypted_token(&ctx, &row_a).as_deref(), Some("token-for-client-a-key"));
    assert_eq!(installation_count(&ctx).await, 2);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn multi_app_session_tokens_cannot_be_used_across_apps() {
    let ctx = multi_app().await;
    let shop = "same-shop.myshopify.com";
    grant_access_token(&ctx, "must-not-be-stored");

    // Audience of app A, signed with the secret of app B.
    let crossed = create_session_token(shop, "client-b-secret", "client-a-key", "1");
    let response = api_get(&ctx, "/api/flows", &crossed).await;
    assert_error(response, 401, "Unauthorized: Invalid JWT signature").await;

    // The environment credentials of single-app mode are not an app here.
    let env_token = ctx.session_token_for(shop, "1");
    let response = api_get(&ctx, "/api/flows", &env_token).await;
    assert_error(
        response,
        401,
        "Unauthorized: [openshop] No Shopify app matches the session token audience",
    )
    .await;

    // Neither is an app handle an audience.
    let by_handle = create_session_token(shop, "client-a-secret", "clientA", "1");
    let response = api_get(&ctx, "/api/flows", &by_handle).await;
    assert_error(
        response,
        401,
        "Unauthorized: [openshop] No Shopify app matches the session token audience",
    )
    .await;

    assert!(ctx.transport.requests().is_empty());
    assert_eq!(installation_count(&ctx).await, 0);
    ctx.shutdown().await.unwrap();
}
