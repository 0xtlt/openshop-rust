//! What a session of the Admin API cannot reach or override: the data of
//! another shop, injection attempts and session-token edge cases. The routes
//! scoped to the session's shop and app are tried one by one in `api.rs`.

mod common;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use common::{find_run, json_body, run_exists};
use hmac::{Hmac, Mac};
use openshop::builder::{Field, Flow, Provider};
use openshop::testing::{
    TestContext, TestOptions, proxy_signature, sign_query, sign_session_token, sign_webhook_body, to_query_string,
};
use openshop::{Concurrency, Cron, DispatchOptions, FlowContext, OpenShopConfig, Value, json};
use reqwest::{Method, Response};
use sha2::Sha256;
use uuid::Uuid;

const SECRET: &str = "test-secret";
const API_KEY: &str = "test-app";
const SHOP_A: &str = "shop-a.myshopify.com";
const SHOP_B: &str = "shop-b.myshopify.com";
const ENCRYPTION_KEY: &str = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";

fn simple_flow() -> Flow {
    Flow::new("test-flow", |ctx: FlowContext| async move {
        ctx.logger.info("executed");
        Ok(())
    })
    .concurrency(Concurrency::Allow)
}

/// One flow, a cron for it and a provider with a secret field.
fn config() -> OpenShopConfig {
    let warehouse = Provider::new("warehouse")
        .field("endpoint", Field::text("Endpoint"))
        .field("apiKey", Field::password("API key"));
    OpenShopConfig::builder()
        .flow(simple_flow())
        .cron(Cron::new("*/5 * * * *", "test-flow"))
        .provider(warehouse)
        .build()
        .unwrap()
}

async fn context() -> TestContext {
    TestContext::new(config()).await.unwrap()
}

async fn get_as(ctx: &TestContext, shop: &str, path: &str) -> Response {
    ctx.request_as(Method::GET, path, shop, "123").send().await.unwrap()
}

async fn post_as(ctx: &TestContext, shop: &str, path: &str) -> Response {
    ctx.request_as(Method::POST, path, shop, "123").send().await.unwrap()
}

async fn assert_run_not_found(response: Response) {
    assert_eq!(response.status(), 404);
    assert_eq!(json_body(response).await, json!({ "error": "Run not found" }));
}

fn ids(runs: &Value) -> Vec<String> {
    runs.as_array()
        .expect("array of runs")
        .iter()
        .map(|run| run["id"].as_str().unwrap().to_string())
        .collect()
}

async fn dispatch(ctx: &TestContext, shop: &str) -> Uuid {
    ctx.app
        .dispatch_flow(DispatchOptions::new("test-flow", shop))
        .await
        .unwrap()
        .run_id
}

// ─── Cross-shop isolation ────────────────────────────────────────────

#[openshop::test(context())]
async fn session_shop_cannot_be_overridden_by_the_request(ctx: &TestContext) {
    let run_a = dispatch(ctx, SHOP_A).await;
    dispatch(ctx, SHOP_B).await;

    let response = ctx
        .request_as(Method::GET, "/api/runs", SHOP_A, "123")
        .query(&[("shop", SHOP_B), ("shopifyApp", "other"), ("appHandle", "other")])
        .header("x-shopify-shop-domain", SHOP_B)
        .send()
        .await
        .unwrap();
    assert_eq!(ids(&json_body(response).await), [run_a.to_string()]);

    // A run started by shop A is a run of shop A, whatever the body claims.
    let response = ctx
        .request_as(Method::POST, "/api/flows/test-flow/run", SHOP_A, "123")
        .json(&json!({ "input": {}, "shop": SHOP_B, "shopifyApp": "other", "appHandle": "other" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    let started: Uuid = json_body(response).await["runId"].as_str().unwrap().parse().unwrap();
    let run = find_run(&ctx.db, started).await;
    assert_eq!((run.shop.as_str(), run.app_handle.as_str()), (SHOP_A, "default"));
    assert_eq!(
        json_body(get_as(ctx, SHOP_B, "/api/runs").await)
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[openshop::test(context())]
async fn shop_a_cannot_list_all_active_installations_or_tokens(ctx: &TestContext) {
    sqlx::query(
        "INSERT INTO installations (shop, access_token, scopes) VALUES \
         ($1, 'token-a', 'read_products'), ($2, 'token-b', 'read_orders')",
    )
    .bind(SHOP_A)
    .bind(SHOP_B)
    .execute(&ctx.db)
    .await
    .unwrap();

    for path in ["/api/installations/active", "/api/installations", "/api/shops"] {
        let response = get_as(ctx, SHOP_A, path).await;
        assert_eq!(response.status(), 404, "{path}");
        let body = response.text().await.unwrap();
        assert!(!body.contains("token-"), "{path}: {body}");
    }
}

#[tokio::test]
async fn access_tokens_are_never_returned_by_api_responses() {
    let options = TestOptions {
        shop: SHOP_A.to_string(),
        encryption_key: Some(ENCRYPTION_KEY.to_string()),
        ..Default::default()
    };
    let ctx = TestContext::with_options(config(), options).await.unwrap();
    // Installed through the token exchange of the first authenticated request.
    ctx.transport
        .respond_json(json!({ "access_token": "shpat_super_secret_token", "scope": "read_products" }));
    let started = post_as(&ctx, SHOP_A, "/api/flows/test-flow/run").await;
    assert_eq!(started.status(), 202);
    let started = started.text().await.unwrap();
    let run_id = serde_json::from_str::<Value>(&started).unwrap()["runId"]
        .as_str()
        .unwrap()
        .to_string();

    let (stored,): (String,) = sqlx::query_as("SELECT access_token FROM installations WHERE shop = $1")
        .bind(SHOP_A)
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert!(stored.starts_with("enc:aes-256-gcm:"), "{stored}");

    let mut bodies = vec![("POST /api/flows/test-flow/run".to_string(), started)];
    for (path, expect_ok) in [
        ("/api/pages".to_string(), true),
        ("/api/flows".to_string(), true),
        ("/api/runs".to_string(), true),
        (format!("/api/runs/{run_id}"), true),
        ("/api/flows/test-flow/runs".to_string(), true),
        ("/api/crons".to_string(), true),
        ("/api/providers".to_string(), true),
        ("/api/functions".to_string(), false),
        ("/api/mcp/capabilities".to_string(), false),
        ("/api/mcp/tokens".to_string(), false),
        ("/api/pages/custom".to_string(), false),
        ("/api/installations".to_string(), false),
        ("/api/installations/active".to_string(), false),
        ("/health".to_string(), true),
    ] {
        let response = get_as(&ctx, SHOP_A, &path).await;
        if expect_ok {
            assert_eq!(response.status(), 200, "{path}");
        }
        bodies.push((path, response.text().await.unwrap()));
    }

    for (path, body) in bodies {
        for secret in [
            "shpat_super_secret_token",
            stored.as_str(),
            "enc:aes-256-gcm",
            "accessToken",
            "access_token",
            SECRET,
        ] {
            assert!(!body.contains(secret), "{path} leaks {secret:?}: {body}");
        }
    }
    ctx.shutdown().await.unwrap();
}

// ─── SQL injection ───────────────────────────────────────────────────

#[openshop::test(context())]
async fn search_param_with_sql_injection_returns_empty_not_error(ctx: &TestContext) {
    dispatch(ctx, SHOP_A).await;
    dispatch(ctx, SHOP_B).await;

    for payload in [
        "'; DROP TABLE flow_runs; --",
        "1' OR '1'='1",
        "' UNION SELECT * FROM installations --",
        "Robert'); DROP TABLE flow_runs;--",
    ] {
        let response = ctx
            .request_as(Method::GET, "/api/runs", SHOP_A, "123")
            .query(&[("search", payload)])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{payload}");
        assert_eq!(json_body(response).await, json!([]), "{payload}");
    }

    let (remaining,): (i64,) = sqlx::query_as("SELECT count(*) FROM flow_runs")
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(remaining, 2);
}

#[openshop::test(context())]
async fn status_param_with_injection_is_safe(ctx: &TestContext) {
    let injection = [("status", "' OR 1=1 --")];

    let response = ctx
        .request_as(Method::GET, "/api/runs", SHOP_A, "123")
        .query(&injection)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(json_body(response).await, json!([]));

    // An unknown status is ignored; it never widens the result beyond the session's shop.
    let run_a = dispatch(ctx, SHOP_A).await;
    dispatch(ctx, SHOP_B).await;
    let response = ctx
        .request_as(Method::GET, "/api/runs", SHOP_A, "123")
        .query(&injection)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(ids(&json_body(response).await), [run_a.to_string()]);
}

#[openshop::test(context())]
async fn from_and_to_date_params_with_injection_are_safe(ctx: &TestContext) {
    let run_a = dispatch(ctx, SHOP_A).await;
    dispatch(ctx, SHOP_B).await;

    // An invalid date is ignored: 200 with the normal results.
    for param in ["from", "to"] {
        let response = ctx
            .request_as(Method::GET, "/api/runs", SHOP_A, "123")
            .query(&[(param, "' OR 1=1 --")])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{param}");
        assert_eq!(ids(&json_body(response).await), [run_a.to_string()], "{param}");
    }
}

#[openshop::test(context())]
async fn run_id_and_flow_name_path_params_with_injection_are_safe(ctx: &TestContext) {
    let run_b = dispatch(ctx, SHOP_B).await;

    for id in [
        "' OR '1'='1".to_string(),
        "1; DROP TABLE flow_runs; --".to_string(),
        "not-a-uuid".to_string(),
        format!("{run_b}' OR '1'='1"),
        format!("{run_b} "),
    ] {
        let mut url = reqwest::Url::parse(&ctx.url).unwrap();
        url.path_segments_mut().unwrap().extend(["api", "runs", id.as_str()]);
        let response = ctx
            .http
            .get(url)
            .bearer_auth(ctx.session_token_for(SHOP_A, "123"))
            .send()
            .await
            .unwrap();
        assert_run_not_found(response).await;
    }

    let mut url = reqwest::Url::parse(&ctx.url).unwrap();
    url.path_segments_mut()
        .unwrap()
        .extend(["api", "flows", "test-flow' OR '1'='1", "runs"]);
    let response = ctx
        .http
        .get(url)
        .bearer_auth(ctx.session_token_for(SHOP_A, "123"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(json_body(response).await, json!([]));

    assert!(run_exists(&ctx.db, run_b).await);
}

// ─── Session-token edge cases ────────────────────────────────────────

fn claims(shop: &str) -> Value {
    let now = Utc::now().timestamp();
    json!({
        "iss": format!("https://{shop}/admin"),
        "dest": format!("https://{shop}"),
        "aud": API_KEY,
        "sub": "123",
        "exp": now + 3600,
        "nbf": now - 10,
        "iat": now,
        "jti": "jti-sec",
        "sid": "sid-sec",
    })
}

/// Valid claims for shop A with some replaced. A `null` override removes the claim.
fn claims_with(overrides: Value) -> Value {
    let mut claims = claims(SHOP_A);
    let object = claims.as_object_mut().unwrap();
    for (key, value) in overrides.as_object().unwrap() {
        if value.is_null() {
            object.remove(key);
        } else {
            object.insert(key.clone(), value.clone());
        }
    }
    claims
}

fn token_with(overrides: Value) -> String {
    sign_session_token(&claims_with(overrides), SECRET)
}

fn encode(value: &Value) -> String {
    URL_SAFE_NO_PAD.encode(value.to_string())
}

fn hs256(secret: &str, signing_input: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(signing_input.as_bytes());
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

/// A token with an arbitrary header, correctly signed with HS256 whatever the header says.
fn token_with_header(header: Value, claims: &Value) -> String {
    let signing_input = format!("{}.{}", encode(&header), encode(claims));
    let signature = hs256(SECRET, &signing_input);
    format!("{signing_input}.{signature}")
}

async fn with_token(ctx: &TestContext, token: &str) -> Response {
    ctx.http
        .get(format!("{}/api/runs", ctx.url))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
}

/// Why a token is refused is for the logs: the client is only told that it is,
/// unless nothing about the token is revealed by the reason.
async fn assert_unauthorized(ctx: &TestContext, token: &str, reason: &str) {
    let response = with_token(ctx, token).await;
    assert_eq!(response.status(), 401, "{reason}");
    let told = match reason {
        "missing token audience" | "session token was not issued to a staff user" => reason,
        _ => "invalid session token",
    };
    assert_eq!(
        json_body(response).await,
        json!({ "error": format!("Unauthorized: {told}") })
    );
}

#[openshop::test(context())]
async fn expired_jwt_is_rejected(ctx: &TestContext) {
    let now = Utc::now().timestamp();

    let token = token_with(json!({ "exp": now - 3600, "nbf": now - 7200, "iat": now - 7200 }));
    assert_unauthorized(ctx, &token, "JWT expired").await;
    // `shopify_api` tolerates ten seconds of clock skew.
    assert_unauthorized(ctx, &token_with(json!({ "exp": now - 30 })), "JWT expired").await;
    assert_unauthorized(ctx, &token_with(json!({ "exp": 0 })), "JWT expired").await;
}

#[openshop::test(context())]
async fn jwt_signed_with_wrong_secret_is_rejected(ctx: &TestContext) {
    let token = sign_session_token(&claims(SHOP_A), "wrong-secret");
    assert_unauthorized(ctx, &token, "Invalid JWT signature").await;
    // Neither the API key nor an empty key is the signing secret.
    for secret in [API_KEY, ""] {
        let token = sign_session_token(&claims(SHOP_A), secret);
        assert_unauthorized(ctx, &token, "Invalid JWT signature").await;
    }
}

#[openshop::test(context())]
async fn tampered_jwt_payload_is_rejected(ctx: &TestContext) {
    dispatch(ctx, SHOP_B).await;
    let valid = sign_session_token(&claims(SHOP_A), SECRET);
    assert_eq!(with_token(ctx, &valid).await.status(), 200);
    let parts: Vec<&str> = valid.split('.').collect();

    // Same header and signature, payload swapped for another shop.
    let tampered = format!("{}.{}.{}", parts[0], encode(&claims(SHOP_B)), parts[2]);
    assert_unauthorized(ctx, &tampered, "Invalid JWT signature").await;

    // The signature must match exactly: no truncation, no trailing bytes, no blank.
    let signature = parts[2];
    for forged in [
        String::new(),
        signature[..signature.len() - 1].to_string(),
        format!("{signature}AAAA"),
        hs256(SECRET, parts[1]),
    ] {
        let token = format!("{}.{}.{forged}", parts[0], parts[1]);
        assert_unauthorized(ctx, &token, "Invalid JWT signature").await;
    }
}

#[openshop::test(context())]
async fn not_yet_valid_jwt_is_rejected(ctx: &TestContext) {
    let now = Utc::now().timestamp();

    let token = token_with(json!({ "nbf": now + 3600 }));
    assert_unauthorized(ctx, &token, "JWT not yet valid").await;

    let token = token_with(json!({ "nbf": now + 30 }));
    assert_unauthorized(ctx, &token, "JWT not yet valid").await;

    // Ten seconds of clock skew are tolerated.
    let token = token_with(json!({ "nbf": now + 5 }));
    assert_eq!(with_token(ctx, &token).await.status(), 200);
}

#[openshop::test(context())]
async fn jwt_without_numeric_time_claims_is_rejected(ctx: &TestContext) {
    let now = Utc::now().timestamp();

    for exp in [Value::Null, json!((now + 3600).to_string()), json!(true)] {
        let token = token_with(json!({ "exp": exp }));
        assert_unauthorized(ctx, &token, "JWT missing exp claim").await;
    }
    // A `nbf` that is no date is refused; a token without one is valid until its `exp`.
    let token = token_with(json!({ "nbf": json!((now + 3600).to_string()) }));
    assert_eq!(with_token(ctx, &token).await.status(), 200);
}

#[openshop::test(context())]
async fn jwt_with_wrong_or_missing_audience_is_rejected(ctx: &TestContext) {
    let no_app = "[openshop] No Shopify app matches the session token audience";

    // Correctly signed with the app secret, but issued for another client id.
    for audience in ["another-app", "test-app ", "TEST-APP", "default", ""] {
        let token = token_with(json!({ "aud": audience }));
        assert_unauthorized(ctx, &token, no_app).await;
    }
    for audience in [Value::Null, json!(42), json!([API_KEY]), json!({ "aud": API_KEY })] {
        let token = token_with(json!({ "aud": audience }));
        assert_unauthorized(ctx, &token, "missing token audience").await;
    }
}

#[openshop::test(context())]
async fn jwt_with_alg_none_or_another_algorithm_is_rejected(ctx: &TestContext) {
    let claims = claims(SHOP_A);

    // Unsigned token.
    let unsigned = format!(
        "{}.{}.",
        encode(&json!({ "alg": "none", "typ": "JWT" })),
        encode(&claims)
    );
    assert_unauthorized(ctx, &unsigned, "Invalid JWT algorithm").await;

    // The algorithm is checked even when the HS256 signature would verify.
    for alg in [
        json!("none"),
        json!("None"),
        json!("HS512"),
        json!("RS256"),
        json!("hs256"),
        Value::Null,
    ] {
        let token = token_with_header(json!({ "alg": alg, "typ": "JWT" }), &claims);
        assert_unauthorized(ctx, &token, "Invalid JWT algorithm").await;
    }

    let control = token_with_header(json!({ "alg": "HS256", "typ": "JWT" }), &claims);
    assert_eq!(with_token(ctx, &control).await.status(), 200);
}

#[openshop::test(context())]
async fn malformed_jwt_is_rejected(ctx: &TestContext) {
    let valid = sign_session_token(&claims(SHOP_A), SECRET);
    let parts: Vec<&str> = valid.split('.').collect();

    // The audience is read first, to select the app.
    for token in ["garbage", "a.b.c", "..", &format!("{}..{}", parts[0], parts[2])] {
        assert_unauthorized(ctx, token, "missing token audience").await;
    }
    for token in [
        format!("{}.{}", parts[0], parts[1]),
        format!("{valid}.extra"),
        format!(".{valid}"),
    ] {
        let response = with_token(ctx, &token).await;
        assert_eq!(response.status(), 401, "{token}");
    }
    let token = format!("{valid}.{}", parts[2]);
    assert_unauthorized(ctx, &token, "Invalid JWT format").await;
    let token = format!("{}.{}", parts[0], parts[1]);
    assert_unauthorized(ctx, &token, "Invalid JWT format").await;

    // A header that is not JSON.
    let token = format!("{}.{}.{}", URL_SAFE_NO_PAD.encode("not json"), parts[1], parts[2]);
    assert_eq!(with_token(ctx, &token).await.status(), 401);
}

#[openshop::test(context())]
async fn jwt_with_mismatched_iss_and_dest_is_rejected(ctx: &TestContext) {
    dispatch(ctx, SHOP_B).await;

    // Signed by the app, but the issuer and the destination disagree about the shop.
    let token = token_with(json!({ "dest": format!("https://{SHOP_B}") }));
    assert_unauthorized(ctx, &token, "JWT dest/iss mismatch").await;
    let token = token_with(json!({ "iss": format!("https://{SHOP_B}/admin") }));
    assert_unauthorized(ctx, &token, "JWT dest/iss mismatch").await;
}

#[openshop::test(context())]
async fn jwt_with_shop_domain_attacks_is_rejected(ctx: &TestContext) {
    for shop in [
        "https://evil.com/shop-a.myshopify.com",
        "https://shop-a.myshopify.com@evil.com",
        "https://shop-a.myshopify.com.evil.com",
        "https://evil.com#shop-a.myshopify.com",
        "https://evil.com\\shop-a.myshopify.com",
        "https://evil.example.com",
        "shop-a",
        "",
    ] {
        let token = token_with(json!({ "dest": shop, "iss": shop }));
        assert_unauthorized(ctx, &token, "JWT missing or invalid dest claim").await;
        let token = token_with(json!({ "iss": shop }));
        assert_unauthorized(ctx, &token, "JWT missing or invalid iss claim").await;
    }
    for claim in ["dest", "iss"] {
        let reason = format!("JWT missing or invalid {claim} claim");
        assert_unauthorized(ctx, &token_with(json!({ claim: null })), &reason).await;
        assert_unauthorized(ctx, &token_with(json!({ claim: 42 })), &reason).await;
    }

    // The claims are exactly the origin of the shop and its admin, as Shopify writes them.
    for (dest, iss) in [
        (
            "https://evil.com@SHOP-A.myshopify.com/path?x=1",
            "https://shop-a.myshopify.com/admin",
        ),
        ("https://shop-a.myshopify.com", "shop-a.myshopify.com"),
        ("shop-a.myshopify.com", "https://shop-a.myshopify.com/admin"),
    ] {
        let token = token_with(json!({ "dest": dest, "iss": iss }));
        assert_unauthorized(ctx, &token, "JWT missing or invalid dest claim").await;
    }
}

#[openshop::test(context())]
async fn credentials_of_one_surface_are_not_accepted_on_another(ctx: &TestContext) {
    dispatch(ctx, SHOP_A).await;
    let timestamp = Utc::now().timestamp().to_string();
    let missing = json!({ "error": "Unauthorized: missing session token" });

    // A signed app-launch or OAuth query is not an Admin API session.
    let launch = sign_query(&[("shop", SHOP_A), ("timestamp", &timestamp)], SECRET);
    // Neither is an app proxy signature.
    let mut proxied = sign_query(&[("shop", SHOP_A), ("timestamp", &timestamp)], SECRET);
    proxied.shift_remove("hmac");
    proxied.insert("signature".to_string(), proxy_signature(&proxied, SECRET));
    for query in [launch, proxied] {
        let path = format!("/api/runs?{}", to_query_string(&query));
        let response = ctx.http.get(format!("{}{path}", ctx.url)).send().await.unwrap();
        assert_eq!(response.status(), 401, "{path}");
        assert_eq!(json_body(response).await, missing, "{path}");
    }

    // Nor a webhook signature.
    let body = json!({ "ids": [] }).to_string();
    let response = ctx
        .http
        .post(format!("{}/api/runs/bulk-delete", ctx.url))
        .header("x-shopify-hmac-sha256", sign_webhook_body(&body, SECRET))
        .header("x-shopify-shop-domain", SHOP_A)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert_eq!(json_body(response).await, missing);

    // A staff session token is not an MCP token.
    let response = ctx
        .request_as(Method::POST, "/mcp", SHOP_A, "123")
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}

#[openshop::test(context())]
async fn customer_and_anonymous_session_tokens_are_not_admin_sessions(ctx: &TestContext) {
    dispatch(ctx, SHOP_A).await;
    ctx.transport
        .respond_json(json!({ "access_token": "must-not-be-requested", "scope": "read_products" }));
    let not_staff = "session token was not issued to a staff user";

    // Customer account and checkout extensions get tokens signed with the same app secret.
    let customer = token_with(json!({ "sub": "gid://shopify/Customer/42" }));
    assert_unauthorized(ctx, &customer, not_staff).await;
    let anonymous = token_with(json!({ "sub": null }));
    assert_unauthorized(ctx, &anonymous, not_staff).await;
    assert_unauthorized(ctx, &token_with(json!({ "sub": "" })), not_staff).await;
    assert_unauthorized(ctx, &token_with(json!({ "sub": 42 })), not_staff).await;

    for (method, path) in [
        (Method::POST, "/api/flows/test-flow/run"),
        (Method::PUT, "/api/providers/warehouse"),
        (Method::POST, "/api/mcp/tokens"),
    ] {
        let response = ctx
            .http
            .request(method, format!("{}{path}", ctx.url))
            .bearer_auth(&customer)
            .json(&json!({ "config": { "endpoint": "https://evil.test", "apiKey": "x" }, "name": "x" }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401, "{path}");
    }

    assert!(ctx.transport.requests().is_empty());
    let (runs,): (i64,) = sqlx::query_as("SELECT count(*) FROM flow_runs")
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(runs, 1);
    let (configs,): (i64,) = sqlx::query_as("SELECT count(*) FROM provider_configs")
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(configs, 0);
}
