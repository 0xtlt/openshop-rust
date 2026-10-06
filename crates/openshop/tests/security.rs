//! Isolation between shops and between Shopify apps, injection attempts and
//! session-token edge cases of the Admin API.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use hmac::{Hmac, Mac};
use openshop::db::schema::FlowRun;
use openshop::testing::{
    TestContext, TestOptions, create_session_token, proxy_signature, sign_query, sign_session_token, sign_webhook_body,
    to_query_string,
};
use openshop::{
    Concurrency, CronEntry, DispatchFlowParams, FieldDef, Flow, FlowContext, JsonMap, OpenShopBuilder, OpenShopConfig,
    Provider, RunFlowOptions, ShopifyAppConfig, ShopifyConfig, Value, json, run_flow,
};
use reqwest::{Method, RequestBuilder, Response};
use sha2::Sha256;
use uuid::Uuid;

const SECRET: &str = "test-secret";
const API_KEY: &str = "test-app";
const SHOP_A: &str = "shop-a.myshopify.com";
const SHOP_B: &str = "shop-b.myshopify.com";
const MULTI_APP_SHOP: &str = "same-shop.myshopify.com";
const CRON_KEY: &str = "test-flow:*/5 * * * *";
const ENCRYPTION_KEY: &str = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";

fn simple_flow() -> Flow {
    Flow::new("test-flow", |ctx: FlowContext| async move {
        ctx.logger.info(json!({}), "executed");
        Ok(())
    })
    .concurrency(Concurrency::Allow)
}

fn warehouse() -> Provider {
    Provider::new("warehouse")
        .field("endpoint", FieldDef::text("Endpoint"))
        .field("apiKey", FieldDef::password("API key"))
}

fn builder() -> OpenShopBuilder {
    OpenShopConfig::builder().flow(simple_flow())
}

/// One flow, a cron for it and a provider whose check only passes for shop A's credentials.
fn config() -> OpenShopConfig {
    let provider = warehouse().checker(|config: JsonMap| async move {
        Ok(config.get("endpoint") == Some(&json!("https://shop-a.test"))
            && config.get("apiKey") == Some(&json!("secret-a")))
    });
    builder()
        .cron(CronEntry::new("*/5 * * * *", "test-flow"))
        .provider(provider)
        .build()
        .unwrap()
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
    builder()
        .shopify(ShopifyConfig {
            scopes: Some("read_products".to_string()),
            apps: Some(apps),
        })
        .provider(warehouse())
        .build()
        .unwrap()
}

async fn context() -> TestContext {
    TestContext::new(config()).await.unwrap()
}

/// A request authenticated as a staff member of `shop`.
fn request_as(ctx: &TestContext, shop: &str, method: Method, path: &str) -> RequestBuilder {
    ctx.http
        .request(method, format!("{}{path}", ctx.url))
        .bearer_auth(ctx.session_token_for(shop, "123"))
}

async fn get_as(ctx: &TestContext, shop: &str, path: &str) -> Response {
    request_as(ctx, shop, Method::GET, path).send().await.unwrap()
}

async fn post_as(ctx: &TestContext, shop: &str, path: &str) -> Response {
    request_as(ctx, shop, Method::POST, path).send().await.unwrap()
}

/// A request authenticated for one of the apps of [`multi_config`], `client-a` or `client-b`.
fn request_as_app(ctx: &TestContext, slug: &str, method: Method, path: &str) -> RequestBuilder {
    let token = create_session_token(MULTI_APP_SHOP, &format!("{slug}-secret"), &format!("{slug}-key"), "123");
    ctx.http
        .request(method, format!("{}{path}", ctx.url))
        .bearer_auth(token)
}

async fn json_body(response: Response) -> Value {
    response.json().await.unwrap()
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
        .dispatch_flow(DispatchFlowParams::new("test-flow", shop))
        .await
        .unwrap()
        .run_id
}

async fn dispatch_for_app(ctx: &TestContext, handle: &str) -> Uuid {
    ctx.app
        .dispatch_flow(DispatchFlowParams::new("test-flow", MULTI_APP_SHOP).shopify_app(handle))
        .await
        .unwrap()
        .run_id
}

/// Runs a dispatched flow to completion, like a worker would.
async fn complete(ctx: &TestContext, run_id: Uuid, shop: &str) {
    let result = run_flow(&ctx.app, RunFlowOptions::new(run_id, "test-flow", shop))
        .await
        .unwrap();
    assert_eq!(result.status(), "completed");
}

async fn find_run(ctx: &TestContext, run_id: Uuid) -> Option<FlowRun> {
    sqlx::query_as("SELECT * FROM flow_runs WHERE id = $1")
        .bind(run_id)
        .fetch_optional(&ctx.db)
        .await
        .unwrap()
}

async fn save_warehouse(request: RequestBuilder, endpoint: &str, api_key: &str) {
    let response = request
        .json(&json!({ "config": { "endpoint": endpoint, "apiKey": api_key } }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
}

// ─── Cross-shop isolation ────────────────────────────────────────────

#[tokio::test]
async fn shop_a_cannot_see_shop_b_runs_in_api_runs() {
    let ctx = context().await;
    let run_a = dispatch(&ctx, SHOP_A).await;
    let run_b = dispatch(&ctx, SHOP_B).await;

    let runs_a = json_body(get_as(&ctx, SHOP_A, "/api/runs").await).await;
    assert_eq!(ids(&runs_a), [run_a.to_string()]);
    assert_eq!(runs_a[0]["shop"], SHOP_A);

    let runs_b = json_body(get_as(&ctx, SHOP_B, "/api/runs").await).await;
    assert_eq!(ids(&runs_b), [run_b.to_string()]);
    assert_eq!(runs_b[0]["shop"], SHOP_B);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn shop_a_cannot_see_shop_b_runs_in_api_flow_runs() {
    let ctx = context().await;
    let run_a = dispatch(&ctx, SHOP_A).await;
    dispatch(&ctx, SHOP_B).await;

    let runs_a = json_body(get_as(&ctx, SHOP_A, "/api/flows/test-flow/runs").await).await;
    assert_eq!(ids(&runs_a), [run_a.to_string()]);
    assert_eq!(runs_a[0]["shop"], SHOP_A);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn session_shop_cannot_be_overridden_by_the_request() {
    let ctx = context().await;
    let run_a = dispatch(&ctx, SHOP_A).await;
    dispatch(&ctx, SHOP_B).await;

    let response = request_as(&ctx, SHOP_A, Method::GET, "/api/runs")
        .query(&[("shop", SHOP_B), ("shopifyApp", "other"), ("appHandle", "other")])
        .header("x-shopify-shop-domain", SHOP_B)
        .send()
        .await
        .unwrap();
    assert_eq!(ids(&json_body(response).await), [run_a.to_string()]);

    // A run started by shop A is a run of shop A, whatever the body claims.
    let response = request_as(&ctx, SHOP_A, Method::POST, "/api/flows/test-flow/run")
        .json(&json!({ "input": {}, "shop": SHOP_B, "shopifyApp": "other", "appHandle": "other" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);
    let started: Uuid = json_body(response).await["runId"].as_str().unwrap().parse().unwrap();
    let run = find_run(&ctx, started).await.unwrap();
    assert_eq!((run.shop.as_str(), run.app_handle.as_str()), (SHOP_A, "default"));
    assert_eq!(
        json_body(get_as(&ctx, SHOP_B, "/api/runs").await)
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn shop_a_cannot_access_shop_b_run_detail_idor() {
    let ctx = context().await;
    let run_b = dispatch(&ctx, SHOP_B).await;

    assert_run_not_found(get_as(&ctx, SHOP_A, &format!("/api/runs/{run_b}")).await).await;

    let own = get_as(&ctx, SHOP_B, &format!("/api/runs/{run_b}")).await;
    assert_eq!(own.status(), 200);
    assert_eq!(json_body(own).await["id"], run_b.to_string());
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn shop_a_cannot_access_shop_b_logs_idor() {
    let ctx = context().await;
    let run_b = dispatch(&ctx, SHOP_B).await;
    complete(&ctx, run_b, SHOP_B).await;
    let path = format!("/api/runs/{run_b}/logs?levels=info,warn,error");

    assert_run_not_found(get_as(&ctx, SHOP_A, &path).await).await;

    let own = get_as(&ctx, SHOP_B, &path).await;
    assert_eq!(own.status(), 200);
    let logs = json_body(own).await;
    assert!(
        logs["logs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|log| log["message"] == "executed")
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn shop_a_cannot_export_shop_b_logs_idor() {
    let ctx = context().await;
    let run_b = dispatch(&ctx, SHOP_B).await;
    complete(&ctx, run_b, SHOP_B).await;
    let path = format!("/api/runs/{run_b}/logs/export?format=json&levels=info");

    assert_run_not_found(get_as(&ctx, SHOP_A, &path).await).await;

    let own = get_as(&ctx, SHOP_B, &path).await;
    assert_eq!(own.status(), 200);
    assert!(own.text().await.unwrap().contains("executed"));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn shop_a_cannot_cancel_shop_b_run_idor() {
    let ctx = context().await;
    let run_b = dispatch(&ctx, SHOP_B).await;

    assert_run_not_found(post_as(&ctx, SHOP_A, &format!("/api/runs/{run_b}/cancel")).await).await;
    assert_eq!(find_run(&ctx, run_b).await.unwrap().status, "pending");

    let own = post_as(&ctx, SHOP_B, &format!("/api/runs/{run_b}/cancel")).await;
    assert_eq!(own.status(), 200);
    assert_eq!(find_run(&ctx, run_b).await.unwrap().status, "canceled");
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn shop_a_cannot_retry_shop_b_run_idor() {
    let ctx = context().await;
    let run_b = dispatch(&ctx, SHOP_B).await;
    complete(&ctx, run_b, SHOP_B).await;

    for mode in ["", "?mode=reset"] {
        let response = post_as(&ctx, SHOP_A, &format!("/api/runs/{run_b}/retry{mode}")).await;
        assert_run_not_found(response).await;
    }
    assert_eq!(find_run(&ctx, run_b).await.unwrap().status, "completed");

    let own = post_as(&ctx, SHOP_B, &format!("/api/runs/{run_b}/retry")).await;
    assert_eq!(own.status(), 200);
    assert_eq!(find_run(&ctx, run_b).await.unwrap().status, "pending");
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn shop_a_cannot_delete_shop_b_run_idor() {
    let ctx = context().await;
    let run_b = dispatch(&ctx, SHOP_B).await;
    complete(&ctx, run_b, SHOP_B).await;

    let response = request_as(&ctx, SHOP_A, Method::DELETE, &format!("/api/runs/{run_b}"))
        .send()
        .await
        .unwrap();
    assert_run_not_found(response).await;

    let run = find_run(&ctx, run_b).await.expect("run still exists");
    assert_eq!(run.id, run_b);
    assert_eq!(run.shop, SHOP_B);
    let (logs,): (i64,) = sqlx::query_as("SELECT count(*) FROM logs WHERE flow_run_id = $1")
        .bind(run_b.to_string())
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert!(logs > 0);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn shop_a_bulk_delete_skips_shop_b_run_ids_idor() {
    let ctx = context().await;
    let run_a = dispatch(&ctx, SHOP_A).await;
    let run_b = dispatch(&ctx, SHOP_B).await;
    complete(&ctx, run_a, SHOP_A).await;
    complete(&ctx, run_b, SHOP_B).await;

    let response = request_as(&ctx, SHOP_A, Method::POST, "/api/runs/bulk-delete")
        .json(&json!({ "ids": [run_a, run_b] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(json_body(response).await, json!({ "deleted": 1, "skipped": 1 }));

    assert!(find_run(&ctx, run_a).await.is_none());
    assert_eq!(find_run(&ctx, run_b).await.expect("run still exists").shop, SHOP_B);

    // Only foreign ids: nothing is deleted.
    let response = request_as(&ctx, SHOP_A, Method::POST, "/api/runs/bulk-delete")
        .json(&json!({ "ids": [run_b] }))
        .send()
        .await
        .unwrap();
    assert_eq!(json_body(response).await, json!({ "deleted": 0, "skipped": 1 }));
    assert!(find_run(&ctx, run_b).await.is_some());
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn shop_a_cron_overrides_do_not_affect_shop_b() {
    let ctx = context().await;
    let enabled_for = async |shop: &str| {
        let crons = json_body(get_as(&ctx, shop, "/api/crons").await).await;
        let cron = crons
            .as_array()
            .unwrap()
            .iter()
            .find(|cron| cron["key"] == CRON_KEY)
            .expect("configured cron")
            .clone();
        cron["enabled"].as_bool().unwrap()
    };

    let response = request_as(&ctx, SHOP_A, Method::POST, "/api/crons/toggle")
        .json(&json!({ "key": CRON_KEY, "enabled": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    assert!(!enabled_for(SHOP_A).await);
    assert!(enabled_for(SHOP_B).await);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn shop_a_cannot_list_all_active_installations_or_tokens() {
    let ctx = context().await;
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
        let response = get_as(&ctx, SHOP_A, path).await;
        assert_eq!(response.status(), 404, "{path}");
        let body = response.text().await.unwrap();
        assert!(!body.contains("token-"), "{path}: {body}");
    }
    ctx.shutdown().await.unwrap();
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

#[tokio::test]
async fn provider_configs_are_isolated_by_shop_and_do_not_leak_secrets() {
    let ctx = context().await;
    let put = |shop| request_as(&ctx, shop, Method::PUT, "/api/providers/warehouse");
    save_warehouse(put(SHOP_A), "https://shop-a.test", "secret-a").await;
    save_warehouse(put(SHOP_B), "https://shop-b.test", "secret-b").await;

    for (shop, endpoint) in [(SHOP_A, "https://shop-a.test"), (SHOP_B, "https://shop-b.test")] {
        let body = get_as(&ctx, shop, "/api/providers").await.text().await.unwrap();
        assert!(!body.contains("secret-"), "{body}");
        assert!(!body.contains("shop-a.test") || shop == SHOP_A, "{body}");
        assert!(!body.contains("shop-b.test") || shop == SHOP_B, "{body}");

        let providers: Value = serde_json::from_str(&body).unwrap();
        let provider = &providers[0];
        assert_eq!(provider["name"], "warehouse");
        assert_eq!(provider["config"], json!({ "endpoint": endpoint }));
        assert_eq!(provider["fields"]["apiKey"]["hasValue"], true);
        assert_eq!(provider["configured"], true);
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn provider_check_uses_only_the_current_shop_config() {
    let ctx = context().await;
    let put = |shop| request_as(&ctx, shop, Method::PUT, "/api/providers/warehouse");
    save_warehouse(put(SHOP_A), "https://shop-a.test", "secret-a").await;
    save_warehouse(put(SHOP_B), "https://shop-b.test", "secret-b").await;

    let check_a = post_as(&ctx, SHOP_A, "/api/providers/warehouse/check").await;
    let check_b = post_as(&ctx, SHOP_B, "/api/providers/warehouse/check").await;
    assert_eq!(check_a.status(), 200);
    assert_eq!(check_b.status(), 200);
    assert_eq!(json_body(check_a).await, json!({ "ok": true }));
    assert_eq!(json_body(check_b).await, json!({ "ok": false }));

    let rows: Vec<(String, Option<bool>)> =
        sqlx::query_as("SELECT shop, last_check_ok FROM provider_configs ORDER BY shop")
            .fetch_all(&ctx.db)
            .await
            .unwrap();
    assert_eq!(
        rows,
        [(SHOP_A.to_string(), Some(true)), (SHOP_B.to_string(), Some(false))]
    );
    ctx.shutdown().await.unwrap();
}

// ─── Cross-app isolation ─────────────────────────────────────────────

#[tokio::test]
async fn same_shop_runs_are_isolated_by_shopify_app() {
    let ctx = TestContext::new(multi_config()).await.unwrap();
    let run_a = dispatch_for_app(&ctx, "clientA").await;
    let run_b = dispatch_for_app(&ctx, "clientB").await;

    let list_a = request_as_app(&ctx, "client-a", Method::GET, "/api/runs")
        .send()
        .await
        .unwrap();
    assert_eq!(list_a.status(), 200);
    let runs_a = json_body(list_a).await;
    assert_eq!(ids(&runs_a), [run_a.to_string()]);
    assert_eq!(runs_a[0]["appHandle"], "clientA");

    let detail_b_from_a = request_as_app(&ctx, "client-a", Method::GET, &format!("/api/runs/{run_b}"))
        .send()
        .await
        .unwrap();
    assert_run_not_found(detail_b_from_a).await;
    for (method, path) in [
        (Method::POST, format!("/api/runs/{run_b}/cancel")),
        (Method::POST, format!("/api/runs/{run_b}/retry")),
        (Method::DELETE, format!("/api/runs/{run_b}")),
        (Method::GET, format!("/api/runs/{run_b}/logs")),
    ] {
        let response = request_as_app(&ctx, "client-a", method, &path).send().await.unwrap();
        assert_run_not_found(response).await;
    }
    assert_eq!(find_run(&ctx, run_b).await.unwrap().status, "pending");

    let list_b = request_as_app(&ctx, "client-b", Method::GET, "/api/runs")
        .send()
        .await
        .unwrap();
    let runs_b = json_body(list_b).await;
    assert_eq!(ids(&runs_b), [run_b.to_string()]);
    assert_eq!(runs_b[0]["appHandle"], "clientB");
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn same_shop_provider_configs_are_isolated_by_shopify_app() {
    let ctx = TestContext::new(multi_config()).await.unwrap();
    let put = |slug| request_as_app(&ctx, slug, Method::PUT, "/api/providers/warehouse");
    save_warehouse(put("client-a"), "https://client-a.test", "secret-a").await;
    save_warehouse(put("client-b"), "https://client-b.test", "secret-b").await;

    for (slug, endpoint) in [
        ("client-a", "https://client-a.test"),
        ("client-b", "https://client-b.test"),
    ] {
        let response = request_as_app(&ctx, slug, Method::GET, "/api/providers")
            .send()
            .await
            .unwrap();
        let providers = json_body(response).await;
        assert_eq!(providers[0]["config"], json!({ "endpoint": endpoint }));
    }

    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT app_handle, shop FROM provider_configs ORDER BY app_handle")
            .fetch_all(&ctx.db)
            .await
            .unwrap();
    assert_eq!(
        rows,
        [
            ("clientA".to_string(), MULTI_APP_SHOP.to_string()),
            ("clientB".to_string(), MULTI_APP_SHOP.to_string()),
        ]
    );
    ctx.shutdown().await.unwrap();
}

// ─── Cross-origin requests ───────────────────────────────────────────

#[tokio::test]
async fn cors_never_allows_foreign_origins_or_replaces_the_session_token() {
    let ctx = context().await;
    let allowed_origin = |response: &Response| {
        response
            .headers()
            .get("access-control-allow-origin")
            .map(|value| value.to_str().unwrap().to_string())
    };
    let preflight = async |origin: &str| {
        ctx.http
            .request(Method::OPTIONS, format!("{}/api/runs", ctx.url))
            .header("origin", origin)
            .header("access-control-request-method", "GET")
            .header("access-control-request-headers", "authorization")
            .send()
            .await
            .unwrap()
    };

    for origin in [
        "https://evil.example.com",
        "https://shop-a.myshopify.com.evil.com",
        "http://shop-a.myshopify.com",
        "https://admin.shopify.com.evil.com",
        "null",
    ] {
        assert_eq!(allowed_origin(&preflight(origin).await), None, "{origin}");
        let response = request_as(&ctx, SHOP_A, Method::GET, "/api/runs")
            .header("origin", origin)
            .send()
            .await
            .unwrap();
        assert_eq!(allowed_origin(&response), None, "{origin}");
    }

    for origin in ["https://admin.shopify.com", "https://shop-a.myshopify.com"] {
        assert_eq!(allowed_origin(&preflight(origin).await).as_deref(), Some(origin));
        // An allowed origin is not an authenticated one.
        let response = ctx
            .http
            .get(format!("{}/api/runs", ctx.url))
            .header("origin", origin)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401, "{origin}");
    }
    ctx.shutdown().await.unwrap();
}

// ─── SQL injection ───────────────────────────────────────────────────

#[tokio::test]
async fn search_param_with_sql_injection_returns_empty_not_error() {
    let ctx = context().await;
    dispatch(&ctx, SHOP_A).await;
    dispatch(&ctx, SHOP_B).await;

    for payload in [
        "'; DROP TABLE flow_runs; --",
        "1' OR '1'='1",
        "' UNION SELECT * FROM installations --",
        "Robert'); DROP TABLE flow_runs;--",
    ] {
        let response = request_as(&ctx, SHOP_A, Method::GET, "/api/runs")
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
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn status_param_with_injection_is_safe() {
    let ctx = context().await;
    let injection = [("status", "' OR 1=1 --")];

    let response = request_as(&ctx, SHOP_A, Method::GET, "/api/runs")
        .query(&injection)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(json_body(response).await, json!([]));

    // An unknown status is ignored; it never widens the result beyond the session's shop.
    let run_a = dispatch(&ctx, SHOP_A).await;
    dispatch(&ctx, SHOP_B).await;
    let response = request_as(&ctx, SHOP_A, Method::GET, "/api/runs")
        .query(&injection)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(ids(&json_body(response).await), [run_a.to_string()]);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn from_and_to_date_params_with_injection_are_safe() {
    let ctx = context().await;
    let run_a = dispatch(&ctx, SHOP_A).await;
    dispatch(&ctx, SHOP_B).await;

    // An invalid date is ignored: 200 with the normal results.
    for param in ["from", "to"] {
        let response = request_as(&ctx, SHOP_A, Method::GET, "/api/runs")
            .query(&[(param, "' OR 1=1 --")])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{param}");
        assert_eq!(ids(&json_body(response).await), [run_a.to_string()], "{param}");
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn run_id_and_flow_name_path_params_with_injection_are_safe() {
    let ctx = context().await;
    let run_b = dispatch(&ctx, SHOP_B).await;

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

    assert!(find_run(&ctx, run_b).await.is_some());
    ctx.shutdown().await.unwrap();
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

async fn assert_unauthorized(ctx: &TestContext, token: &str, reason: &str) {
    let response = with_token(ctx, token).await;
    assert_eq!(response.status(), 401, "{reason}");
    assert_eq!(
        json_body(response).await,
        json!({ "error": format!("Unauthorized: {reason}") })
    );
}

#[tokio::test]
async fn expired_jwt_is_rejected() {
    let ctx = context().await;
    let now = Utc::now().timestamp();

    let token = token_with(json!({ "exp": now - 3600, "nbf": now - 7200, "iat": now - 7200 }));
    assert_unauthorized(&ctx, &token, "JWT expired").await;
    assert_unauthorized(&ctx, &token_with(json!({ "exp": now - 5 })), "JWT expired").await;
    assert_unauthorized(&ctx, &token_with(json!({ "exp": 0 })), "JWT expired").await;
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn jwt_signed_with_wrong_secret_is_rejected() {
    let ctx = context().await;

    let token = sign_session_token(&claims(SHOP_A), "wrong-secret");
    assert_unauthorized(&ctx, &token, "Invalid JWT signature").await;
    // Neither the API key nor an empty key is the signing secret.
    for secret in [API_KEY, ""] {
        let token = sign_session_token(&claims(SHOP_A), secret);
        assert_unauthorized(&ctx, &token, "Invalid JWT signature").await;
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn tampered_jwt_payload_is_rejected() {
    let ctx = context().await;
    dispatch(&ctx, SHOP_B).await;
    let valid = sign_session_token(&claims(SHOP_A), SECRET);
    assert_eq!(with_token(&ctx, &valid).await.status(), 200);
    let parts: Vec<&str> = valid.split('.').collect();

    // Same header and signature, payload swapped for another shop.
    let tampered = format!("{}.{}.{}", parts[0], encode(&claims(SHOP_B)), parts[2]);
    assert_unauthorized(&ctx, &tampered, "Invalid JWT signature").await;

    // The signature must match exactly: no truncation, no trailing bytes, no blank.
    let signature = parts[2];
    for forged in [
        String::new(),
        signature[..signature.len() - 1].to_string(),
        format!("{signature}AAAA"),
        hs256(SECRET, parts[1]),
    ] {
        let token = format!("{}.{}.{forged}", parts[0], parts[1]);
        assert_unauthorized(&ctx, &token, "Invalid JWT signature").await;
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn not_yet_valid_jwt_is_rejected() {
    let ctx = context().await;
    let now = Utc::now().timestamp();

    let token = token_with(json!({ "nbf": now + 3600 }));
    assert_unauthorized(&ctx, &token, "JWT not yet valid").await;

    // A minute of clock skew is tolerated.
    let token = token_with(json!({ "nbf": now + 20 }));
    assert_eq!(with_token(&ctx, &token).await.status(), 200);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn jwt_without_numeric_time_claims_is_rejected() {
    let ctx = context().await;
    let now = Utc::now().timestamp();

    for exp in [Value::Null, json!((now + 3600).to_string()), json!(true)] {
        let token = token_with(json!({ "exp": exp }));
        assert_unauthorized(&ctx, &token, "JWT missing exp claim").await;
    }
    for nbf in [Value::Null, json!((now - 10).to_string())] {
        let token = token_with(json!({ "nbf": nbf }));
        assert_unauthorized(&ctx, &token, "JWT missing nbf claim").await;
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn jwt_with_wrong_or_missing_audience_is_rejected() {
    let ctx = context().await;
    let no_app = "[openshop] No Shopify app matches the session token audience";

    // Correctly signed with the app secret, but issued for another client id.
    for audience in ["another-app", "test-app ", "TEST-APP", "default", ""] {
        let token = token_with(json!({ "aud": audience }));
        assert_unauthorized(&ctx, &token, no_app).await;
    }
    for audience in [Value::Null, json!(42), json!([API_KEY]), json!({ "aud": API_KEY })] {
        let token = token_with(json!({ "aud": audience }));
        assert_unauthorized(&ctx, &token, "missing token audience").await;
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn jwt_with_alg_none_or_another_algorithm_is_rejected() {
    let ctx = context().await;
    let claims = claims(SHOP_A);

    // Unsigned token.
    let unsigned = format!(
        "{}.{}.",
        encode(&json!({ "alg": "none", "typ": "JWT" })),
        encode(&claims)
    );
    assert_unauthorized(&ctx, &unsigned, "Invalid JWT algorithm").await;

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
        assert_unauthorized(&ctx, &token, "Invalid JWT algorithm").await;
    }

    let control = token_with_header(json!({ "alg": "HS256", "typ": "JWT" }), &claims);
    assert_eq!(with_token(&ctx, &control).await.status(), 200);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn malformed_jwt_is_rejected() {
    let ctx = context().await;
    let valid = sign_session_token(&claims(SHOP_A), SECRET);
    let parts: Vec<&str> = valid.split('.').collect();

    // The audience is read first, to select the app.
    for token in ["garbage", "a.b.c", "..", &format!("{}..{}", parts[0], parts[2])] {
        assert_unauthorized(&ctx, token, "missing token audience").await;
    }
    for token in [
        format!("{}.{}", parts[0], parts[1]),
        format!("{valid}.extra"),
        format!(".{valid}"),
    ] {
        let response = with_token(&ctx, &token).await;
        assert_eq!(response.status(), 401, "{token}");
    }
    let token = format!("{valid}.{}", parts[2]);
    assert_unauthorized(&ctx, &token, "Invalid JWT format").await;
    let token = format!("{}.{}", parts[0], parts[1]);
    assert_unauthorized(&ctx, &token, "Invalid JWT format").await;

    // A header that is not JSON.
    let token = format!("{}.{}.{}", URL_SAFE_NO_PAD.encode("not json"), parts[1], parts[2]);
    assert_eq!(with_token(&ctx, &token).await.status(), 401);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn jwt_with_mismatched_iss_and_dest_is_rejected() {
    let ctx = context().await;
    dispatch(&ctx, SHOP_B).await;

    // Signed by the app, but the issuer and the destination disagree about the shop.
    let token = token_with(json!({ "dest": format!("https://{SHOP_B}") }));
    assert_unauthorized(&ctx, &token, "JWT dest/iss mismatch").await;
    let token = token_with(json!({ "iss": format!("https://{SHOP_B}/admin") }));
    assert_unauthorized(&ctx, &token, "JWT dest/iss mismatch").await;
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn jwt_with_shop_domain_attacks_is_rejected() {
    let ctx = context().await;

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
        assert_unauthorized(&ctx, &token, "JWT missing or invalid dest claim").await;
        let token = token_with(json!({ "iss": shop }));
        assert_unauthorized(&ctx, &token, "JWT missing or invalid iss claim").await;
    }
    for claim in ["dest", "iss"] {
        let reason = format!("JWT missing or invalid {claim} claim");
        assert_unauthorized(&ctx, &token_with(json!({ claim: null })), &reason).await;
        assert_unauthorized(&ctx, &token_with(json!({ claim: 42 })), &reason).await;
    }

    // Whatever form the claims take, the session is the normalized shop host.
    let run_a = dispatch(&ctx, SHOP_A).await;
    let token = token_with(json!({
        "dest": "https://evil.com@SHOP-A.myshopify.com/path?x=1",
        "iss": "shop-a.myshopify.com",
    }));
    let response = with_token(&ctx, &token).await;
    assert_eq!(response.status(), 200);
    assert_eq!(ids(&json_body(response).await), [run_a.to_string()]);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn credentials_of_one_surface_are_not_accepted_on_another() {
    let ctx = context().await;
    dispatch(&ctx, SHOP_A).await;
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
    let response = request_as(&ctx, SHOP_A, Method::POST, "/mcp")
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn customer_and_anonymous_session_tokens_are_not_admin_sessions() {
    let ctx = context().await;
    dispatch(&ctx, SHOP_A).await;
    ctx.transport
        .respond_json(json!({ "access_token": "must-not-be-requested", "scope": "read_products" }));
    let not_staff = "session token was not issued to a staff user";

    // Customer account and checkout extensions get tokens signed with the same app secret.
    let customer = token_with(json!({ "sub": "gid://shopify/Customer/42" }));
    assert_unauthorized(&ctx, &customer, not_staff).await;
    let anonymous = token_with(json!({ "sub": null }));
    assert_unauthorized(&ctx, &anonymous, not_staff).await;
    assert_unauthorized(&ctx, &token_with(json!({ "sub": "" })), not_staff).await;
    assert_unauthorized(&ctx, &token_with(json!({ "sub": 42 })), not_staff).await;

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
    ctx.shutdown().await.unwrap();
}
