//! MCP: the admin token API (`/api/mcp/*`) and the JSON-RPC endpoint (`POST /mcp`).

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use openshop::db::schema::McpAuditLog;
use openshop::runtime::OutboundRequest;
use openshop::server::mcp::core::latest_stable_shopify_admin_api_version;
use openshop::testing::{MockTransport, TestContext, TestOptions, session_token};
use openshop::{
    AdminPageId, AdminPageMode, CancellationToken, Cron, DispatchOptions, Flow, FlowContext, IndexMap, InputSchema,
    McpConfig, McpContext, McpPermission, McpResource, McpResourceContent, McpTool, OpenShopConfig, RunFlowOptions,
    ShopifyAppConfig, ShopifyConfig, Value, anyhow, json, run_flow,
};
use reqwest::{Method, RequestBuilder};
use uuid::Uuid;

const SHOP_B: &str = "mcp-other.myshopify.com";
const PROTOCOL_VERSION: &str = "2025-11-25";
const MAX_PAYLOAD_BYTES: usize = 1_000_000;
const ENCRYPTION_KEY: &str = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
const CORE_PERMISSIONS: [&str; 10] = [
    "read_logs",
    "read_flows",
    "run_flows",
    "read_crons",
    "manage_crons",
    "read_providers",
    "write_providers",
    "read_functions",
    "manage_mcp_tokens",
    "shopify_admin_graphql",
];
const CORE_TOOLS: [&str; 7] = [
    "openshop.shops.list",
    "openshop.admin.graphql",
    "openshop.logs.search",
    "openshop.flows.list",
    "openshop.flows.run",
    "openshop.crons.list",
    "openshop.crons.set_enabled",
];
const CORE_RESOURCES: [&str; 4] = [
    "openshop://docs/log-search",
    "openshop://permissions",
    "openshop://tools/openshop.logs.search",
    "openshop://runs/recent",
];

// ─── Application under test ──────────────────────────────────────────

fn log_flow() -> Flow {
    Flow::new("log-flow", |ctx: FlowContext| async move {
        ctx.logger.info_with("mcp info", json!({ "area": "mcp" }));
        ctx.logger.warn_with("mcp warn", json!({ "area": "mcp" }));
        Ok(())
    })
}

fn demo_mcp() -> McpConfig {
    McpConfig::new()
        .permission(McpPermission::new("demo:read_status", "Read demo status"))
        .tool(
            "demo.status",
            McpTool::new("Read status", |_ctx, _input| async { Ok(json!({ "ok": true })) })
                .permissions(["demo:read_status"]),
        )
        .resource(
            "openshop://demo/status",
            McpResource::new("Demo status docs", |_ctx| async {
                Ok(McpResourceContent::from("demo"))
            }),
        )
}

fn config_with(mcp: McpConfig) -> OpenShopConfig {
    OpenShopConfig::builder().flow(log_flow()).mcp(mcp).build().unwrap()
}

async fn context() -> TestContext {
    TestContext::new(config_with(demo_mcp())).await.unwrap()
}

/// Two Shopify apps served by one instance; the context's own session belongs to `client-a`.
async fn multi_app_context() -> TestContext {
    let apps: IndexMap<String, ShopifyAppConfig> = ["a", "b"]
        .into_iter()
        .map(|suffix| {
            let app = ShopifyAppConfig::Keys {
                api_key: format!("key-{suffix}"),
                api_secret: format!("secret-{suffix}"),
                app_url: None,
            };
            (format!("client-{suffix}"), app)
        })
        .collect();
    let config = OpenShopConfig::builder()
        .shopify(ShopifyConfig {
            scopes: Some("read_products".to_string()),
            apps: Some(apps),
        })
        .flow(log_flow())
        .cron(Cron::new("0 * * * *", "log-flow"))
        .build()
        .unwrap();
    let options = TestOptions {
        api_key: "key-a".to_string(),
        secret: "secret-a".to_string(),
        ..Default::default()
    };
    TestContext::with_options(config, options).await.unwrap()
}

// ─── HTTP helpers ────────────────────────────────────────────────────

async fn send(request: RequestBuilder) -> (u16, Value) {
    let response = request.send().await.expect("response");
    let status = response.status().as_u16();
    let text = response.text().await.expect("response body");
    let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
    (status, body)
}

/// An Admin API request carrying the given Shopify session token.
fn admin_with(ctx: &TestContext, method: Method, path: &str, session_token: &str) -> RequestBuilder {
    ctx.http
        .request(method, format!("{}{path}", ctx.url))
        .header("Authorization", format!("Bearer {session_token}"))
}

/// An Admin API request authenticated as another shop of the context's app.
fn admin_as(ctx: &TestContext, method: Method, path: &str, shop: &str) -> RequestBuilder {
    admin_with(ctx, method, path, &ctx.session_token_for(shop, "1"))
}

struct IssuedToken {
    /// Row id, used by the admin API.
    id: String,
    token_id: String,
    /// The secret bearer token.
    token: String,
    item: Value,
}

impl IssuedToken {
    fn secret(&self) -> &str {
        self.token.split_once('.').expect("token secret").1
    }

    fn path(&self, suffix: &str) -> String {
        format!("/api/mcp/tokens/{}{suffix}", self.id)
    }
}

async fn create_token_with(ctx: &TestContext, session_token: &str, permissions: &[&str], extra: Value) -> IssuedToken {
    let mut body = json!({ "name": "test token", "permissions": permissions });
    if let (Some(body), Value::Object(extra)) = (body.as_object_mut(), extra) {
        body.extend(extra);
    }
    let (status, data) = send(admin_with(ctx, Method::POST, "/api/mcp/tokens", session_token).json(&body)).await;
    assert_eq!(status, 201, "{data}");
    let text = |value: &Value| value.as_str().expect("string").to_string();
    IssuedToken {
        id: text(&data["item"]["id"]),
        token_id: text(&data["item"]["tokenId"]),
        token: text(&data["token"]),
        item: data["item"].clone(),
    }
}

async fn create_token_as(ctx: &TestContext, shop: &str, permissions: &[&str], extra: Value) -> IssuedToken {
    create_token_with(ctx, &ctx.session_token_for(shop, "1"), permissions, extra).await
}

async fn create_token(ctx: &TestContext, permissions: &[&str]) -> IssuedToken {
    create_token_as(ctx, &ctx.shop, permissions, json!({})).await
}

async fn token_detail(ctx: &TestContext, issued: &IssuedToken) -> Value {
    let (status, detail) = send(ctx.get(&issued.path(""))).await;
    assert_eq!(status, 200, "{detail}");
    detail
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

/// `POST /mcp`. Every response must announce the protocol version.
async fn mcp_send(ctx: &TestContext, authorization: Option<&str>, body: impl Into<reqwest::Body>) -> (u16, Value) {
    let mut request = ctx
        .http
        .post(format!("{}/mcp", ctx.url))
        .header("Content-Type", "application/json")
        .body(body);
    if let Some(authorization) = authorization {
        request = request.header("Authorization", authorization);
    }
    let response = request.send().await.expect("response");
    assert_eq!(
        response
            .headers()
            .get("mcp-protocol-version")
            .and_then(|value| value.to_str().ok()),
        Some(PROTOCOL_VERSION),
        "status {}",
        response.status()
    );
    let status = response.status().as_u16();
    let text = response.text().await.expect("response body");
    (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

async fn rpc(ctx: &TestContext, token: &str, message: Value) -> (u16, Value) {
    mcp_send(ctx, Some(&bearer(token)), message.to_string()).await
}

/// A JSON-RPC call with id `1`; JSON-RPC level errors still answer with HTTP 200.
async fn call(ctx: &TestContext, token: &str, method: &str, params: Value) -> Value {
    let (status, body) = rpc(
        ctx,
        token,
        json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["jsonrpc"], "2.0");
    assert_eq!(body["id"], 1);
    body
}

async fn call_tool(ctx: &TestContext, token: &str, name: &str, arguments: Value) -> Value {
    call(
        ctx,
        token,
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    )
    .await
}

async fn read_resource(ctx: &TestContext, token: &str, uri: &str) -> Value {
    call(ctx, token, "resources/read", json!({ "uri": uri })).await
}

async fn initialize_status(ctx: &TestContext, token: &str) -> (u16, Value) {
    rpc(ctx, token, json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" })).await
}

fn strings(items: &Value, key: &str) -> Vec<String> {
    items
        .as_array()
        .expect("array")
        .iter()
        .map(|item| item[key].as_str().expect("string").to_string())
        .collect()
}

fn sorted(values: &Value) -> Vec<String> {
    let mut values: Vec<String> = values
        .as_array()
        .expect("array")
        .iter()
        .map(|value| value.as_str().expect("string").to_string())
        .collect();
    values.sort();
    values
}

/// Timestamps are ISO-8601 UTC with millisecond precision.
fn timestamp(value: &Value) -> DateTime<Utc> {
    let text = value
        .as_str()
        .unwrap_or_else(|| panic!("expected a timestamp, got {value}"));
    assert!(text.len() == 24 && text.ends_with('Z'), "{text}");
    DateTime::parse_from_rfc3339(text)
        .expect("ISO timestamp")
        .with_timezone(&Utc)
}

fn days_from_now(value: &Value) -> f64 {
    (timestamp(value) - Utc::now()).num_seconds() as f64 / 86_400.0
}

// ─── Database helpers ────────────────────────────────────────────────

async fn audits(ctx: &TestContext, token_id: &str) -> Vec<McpAuditLog> {
    sqlx::query_as("SELECT * FROM mcp_audit_logs WHERE token_id = $1 ORDER BY created_at, id")
        .bind(token_id)
        .fetch_all(&ctx.db)
        .await
        .unwrap()
}

async fn install(ctx: &TestContext, app_handle: &str, shop: &str, access_token: Option<&str>) {
    let stored = access_token.map(|token| ctx.app.crypto().encrypt_string(token).unwrap());
    sqlx::query(
        "INSERT INTO installations (app_handle, shop, access_token, scopes) VALUES ($1, $2, $3, 'read_products')",
    )
    .bind(app_handle)
    .bind(shop)
    .bind(stored)
    .execute(&ctx.db)
    .await
    .unwrap();
}

async fn uninstall(ctx: &TestContext, shop: &str) {
    sqlx::query("UPDATE installations SET uninstalled_at = now() WHERE shop = $1")
        .bind(shop)
        .execute(&ctx.db)
        .await
        .unwrap();
}

/// Dispatches and runs `log-flow`, which leaves four log lines.
async fn logged_run(ctx: &TestContext, app_handle: &str, shop: &str) -> Uuid {
    let dispatched = ctx
        .app
        .dispatch_flow(DispatchOptions::new("log-flow", shop).shopify_app(app_handle))
        .await
        .unwrap();
    let result = run_flow(
        &ctx.app,
        RunFlowOptions::new(dispatched.run_id, "log-flow", shop).shopify_app(app_handle),
    )
    .await
    .unwrap();
    assert_eq!(result.status(), "completed");
    dispatched.run_id
}

fn graphql_requests(ctx: &TestContext) -> Vec<OutboundRequest> {
    ctx.transport
        .requests()
        .into_iter()
        .filter(|request| request.url.ends_with("/graphql.json"))
        .collect()
}

fn header<'a>(request: &'a OutboundRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// Answers Admin GraphQL calls with `body`; anything else (token exchange) is not mocked.
fn respond_to_graphql(ctx: &TestContext, status: u16, body: Value) {
    ctx.transport.respond_with(move |request| {
        if request.url.ends_with("/graphql.json") {
            Ok(MockTransport::json(status, body.clone()))
        } else {
            Ok(MockTransport::json(404, json!({})))
        }
    });
}

// ─── Admin API: capabilities ─────────────────────────────────────────

#[tokio::test]
async fn capabilities_list_core_and_custom_permissions_tools_and_resources() {
    let ctx = context().await;

    let (status, body) = send(ctx.get("/api/mcp/capabilities")).await;
    assert_eq!(status, 200);
    assert_eq!(body["enabled"], true);
    assert_eq!(body["defaultExpirationDays"], 90);
    assert_eq!(body["expirationOptions"], json!([30, 90, 365, null]));

    let keys = |section: &str| -> Vec<String> { body[section].as_object().unwrap().keys().cloned().collect() };
    assert_eq!(
        keys("permissions"),
        [&CORE_PERMISSIONS[..], &["demo:read_status"]].concat()
    );
    assert_eq!(keys("tools"), [&CORE_TOOLS[..], &["demo.status"]].concat());
    assert_eq!(
        keys("resources"),
        [&CORE_RESOURCES[..], &["openshop://demo/status"]].concat()
    );

    assert_eq!(
        body["permissions"]["read_logs"],
        json!({
            "label": "Read logs",
            "description": "Read OpenShop flow run logs.",
            "group": "Observability",
            "riskLevel": "low",
            "key": "read_logs",
            "source": "core",
        })
    );
    // Custom permissions default to the namespace as group and a medium risk level.
    assert_eq!(
        body["permissions"]["demo:read_status"],
        json!({
            "label": "Read demo status",
            "group": "demo",
            "riskLevel": "medium",
            "key": "demo:read_status",
            "source": "custom",
        })
    );

    let graphql = &body["tools"]["openshop.admin.graphql"];
    assert_eq!(graphql["name"], "openshop.admin.graphql");
    assert_eq!(graphql["requiredPermissions"], json!(["shopify_admin_graphql"]));
    assert_eq!(graphql["riskLevel"], "high");
    assert_eq!(graphql["source"], "core");
    assert_eq!(graphql["inputSchema"]["required"], json!(["shop", "query"]));
    assert!(
        graphql["confirmationHint"]
            .as_str()
            .unwrap()
            .contains("Shopify Admin data")
    );
    assert_eq!(
        body["tools"]["demo.status"],
        json!({
            "name": "demo.status",
            "description": "Read status",
            "inputSchema": null,
            "requiredPermissions": ["demo:read_status"],
            "riskLevel": "low",
            "confirmationHint": null,
            "source": "custom",
        })
    );

    assert_eq!(
        body["resources"]["openshop://docs/log-search"],
        json!({
            "uri": "openshop://docs/log-search",
            "name": "Log search guide",
            "description": "How to search OpenShop logs through MCP.",
            "mimeType": "text/markdown",
            "requiredPermissions": ["read_logs"],
            "riskLevel": "low",
            "source": "core",
        })
    );
    assert_eq!(
        body["resources"]["openshop://demo/status"],
        json!({
            "uri": "openshop://demo/status",
            "name": "Demo status docs",
            "description": null,
            "mimeType": "text/plain",
            "requiredPermissions": [],
            "riskLevel": "low",
            "source": "custom",
        })
    );

    let (status, _) = send(ctx.http.get(format!("{}/api/mcp/capabilities", ctx.url))).await;
    assert_eq!(status, 401);
    ctx.shutdown().await.unwrap();
}

// ─── Admin API: tokens ───────────────────────────────────────────────

#[tokio::test]
async fn admin_can_create_a_token_and_only_receives_the_secret_once() {
    let ctx = context().await;

    let issued = create_token(&ctx, &["read_logs"]).await;
    assert!(issued.token_id.starts_with("mtk_"));
    assert!(issued.token.starts_with(&format!("oshp_mcp_{}.", issued.token_id)));
    assert!(issued.secret().len() >= 32);
    assert_eq!(issued.item["permissions"], json!(["read_logs"]));
    assert_eq!(issued.item["name"], "test token");
    assert_eq!(issued.item["status"], "active");
    assert_eq!(issued.item["revokedAt"], Value::Null);
    assert_eq!(issued.item["lastUsedAt"], Value::Null);
    assert_eq!(issued.item["stalePermissions"], json!([]));
    assert_eq!(issued.item["recentAudits"], json!([]));
    assert_eq!(issued.item["tokenFingerprint"].as_str().unwrap().len(), 16);
    assert!(Uuid::parse_str(&issued.id).is_ok());
    timestamp(&issued.item["createdAt"]);
    timestamp(&issued.item["updatedAt"]);

    let (status, list) = send(ctx.get("/api/mcp/tokens")).await;
    assert_eq!(status, 200);
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].get("token").is_none());
    assert!(list[0].get("tokenHash").is_none());
    assert_eq!(list[0]["tokenId"], issued.token_id.as_str());
    assert_eq!(list[0]["permissions"], json!(["read_logs"]));
    // Tokens expire after 90 days unless told otherwise.
    let days = days_from_now(&list[0]["expiresAt"]);
    assert!((89.9..=90.0).contains(&days), "{days}");
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn tokens_are_hashed_at_rest_and_the_secret_is_never_returned_again() {
    let ctx = context().await;
    let issued = create_token(&ctx, &["read_logs"]).await;

    let stored = |ctx: &TestContext| {
        let db = ctx.db.clone();
        let token_id = issued.token_id.clone();
        async move {
            let row: (String, String, String) = sqlx::query_as(
                "SELECT token_hash, token_fingerprint, row_to_json(t)::text FROM mcp_tokens t WHERE token_id = $1",
            )
            .bind(token_id)
            .fetch_one(&db)
            .await
            .unwrap();
            row
        }
    };

    let (hash, fingerprint, row) = stored(&ctx).await;
    assert!(hash.starts_with("scrypt:"), "{hash}");
    assert_eq!(hash.split(':').count(), 3);
    assert!(!row.contains(issued.secret()));
    assert!(!row.contains(&issued.token));
    assert_eq!(issued.item["tokenFingerprint"], fingerprint.as_str());

    let detail = token_detail(&ctx, &issued).await;
    let (_, list) = send(ctx.get("/api/mcp/tokens")).await;
    for body in [&issued.item, &detail, &list] {
        let text = body.to_string();
        assert!(!text.contains(issued.secret()));
        assert!(!text.contains("scrypt:"));
        assert!(!text.contains(&hash));
    }

    // Rotation replaces the hash; the new secret is again only part of the response.
    let (status, rotated) = send(ctx.post(&issued.path("/rotate"))).await;
    assert_eq!(status, 200);
    let rotated_secret = rotated["token"].as_str().unwrap().split_once('.').unwrap().1;
    let (rotated_hash, rotated_fingerprint, rotated_row) = stored(&ctx).await;
    assert!(rotated_hash.starts_with("scrypt:"));
    assert_ne!(rotated_hash, hash);
    assert_ne!(rotated_fingerprint, fingerprint);
    assert!(!rotated_row.contains(rotated_secret));
    assert!(!rotated_row.contains(issued.secret()));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn token_creation_validates_the_name_and_the_permissions() {
    let ctx = context().await;
    let create = |body: Value| send(ctx.post("/api/mcp/tokens").json(&body));

    for body in [
        json!({}),
        json!({ "name": "   " }),
        json!({ "name": 5 }),
        json!(["name"]),
    ] {
        let (status, error) = create(body).await;
        assert_eq!(status, 400);
        assert_eq!(error, json!({ "error": "name is required" }));
    }
    let (status, error) = send(ctx.post("/api/mcp/tokens").body("not json")).await;
    assert_eq!(status, 400);
    assert_eq!(error, json!({ "error": "name is required" }));

    let (status, error) = create(json!({ "name": "Agent", "permissions": ["read_logs", "nope"] })).await;
    assert_eq!(status, 400);
    assert_eq!(error, json!({ "error": "Unknown permission \"nope\"" }));
    let (_, list) = send(ctx.get("/api/mcp/tokens")).await;
    assert_eq!(list, json!([]));

    // The name is trimmed; permissions are deduplicated and non-strings ignored.
    let (status, created) = create(json!({
        "name": "  Agent  ",
        "permissions": ["read_logs", "read_logs", 7, "demo:read_status"],
    }))
    .await;
    assert_eq!(status, 201, "{created}");
    assert_eq!(created["item"]["name"], "Agent");
    assert_eq!(created["item"]["permissions"], json!(["read_logs", "demo:read_status"]));
    assert_eq!(created["item"]["stalePermissions"], json!([]));

    let (_, list) = send(ctx.get("/api/mcp/tokens")).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(sorted(&list[0]["permissions"]), ["demo:read_status", "read_logs"]);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn admin_token_api_rejects_invalid_explicit_expiration_values() {
    let ctx = context().await;

    for expires_at in [json!("not-a-date"), json!(5), json!(""), json!("   "), json!(true)] {
        let (status, body) = send(
            ctx.post("/api/mcp/tokens")
                .json(&json!({ "name": "bad expiry", "expiresAt": expires_at })),
        )
        .await;
        assert_eq!(status, 400, "{expires_at}");
        assert_eq!(body["error"], "expiresAt must be a valid date string or null");
    }
    for expires_in_days in [json!(0), json!(-1), json!(1.5), json!("30"), json!(false)] {
        let (status, body) = send(
            ctx.post("/api/mcp/tokens")
                .json(&json!({ "name": "bad expiry", "expiresInDays": expires_in_days })),
        )
        .await;
        assert_eq!(status, 400, "{expires_in_days}");
        assert_eq!(body["error"], "expiresInDays must be a positive integer or null");
    }
    let (_, list) = send(ctx.get("/api/mcp/tokens")).await;
    assert_eq!(list, json!([]));

    let issued = create_token(&ctx, &[]).await;
    let (status, body) = send(ctx.patch(&issued.path("")).json(&json!({ "expiresInDays": 0 }))).await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], "expiresInDays must be a positive integer or null");
    let (status, body) = send(
        ctx.patch(&issued.path(""))
            .json(&json!({ "name": "renamed", "expiresAt": "tomorrow" })),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], "expiresAt must be a valid date string or null");

    // A rejected patch changes nothing.
    let detail = token_detail(&ctx, &issued).await;
    assert_eq!(detail["name"], "test token");
    assert_eq!(detail["expiresAt"], issued.item["expiresAt"]);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn token_expiration_follows_expires_at_and_expires_in_days() {
    let ctx = context().await;
    let shop = ctx.shop.clone();

    let in_thirty_days = create_token_as(&ctx, &shop, &[], json!({ "expiresInDays": 30 })).await;
    let days = days_from_now(&in_thirty_days.item["expiresAt"]);
    assert!((29.9..=30.0).contains(&days), "{days}");

    let never = create_token_as(&ctx, &shop, &[], json!({ "expiresInDays": null })).await;
    assert_eq!(never.item["expiresAt"], Value::Null);

    let explicit = create_token_as(&ctx, &shop, &[], json!({ "expiresAt": "2030-01-01T00:00:00.000Z" })).await;
    assert_eq!(explicit.item["expiresAt"], "2030-01-01T00:00:00.000Z");

    // `expiresAt` wins over `expiresInDays`.
    let both = create_token_as(&ctx, &shop, &[], json!({ "expiresAt": null, "expiresInDays": 30 })).await;
    assert_eq!(both.item["expiresAt"], Value::Null);

    let (status, body) = send(ctx.patch(&never.path("")).json(&json!({ "expiresInDays": 7 }))).await;
    assert_eq!((status, body), (200, json!({ "ok": true })));
    let patched = token_detail(&ctx, &never).await;
    let days = days_from_now(&patched["expiresAt"]);
    assert!((6.9..=7.0).contains(&days), "{days}");

    // Other updates keep the expiration.
    send(ctx.patch(&never.path("")).json(&json!({ "name": "renamed" }))).await;
    let renamed = token_detail(&ctx, &never).await;
    assert_eq!(renamed["name"], "renamed");
    assert_eq!(renamed["expiresAt"], patched["expiresAt"]);

    send(ctx.patch(&explicit.path("")).json(&json!({ "expiresAt": null }))).await;
    assert_eq!(token_detail(&ctx, &explicit).await["expiresAt"], Value::Null);
    send(
        ctx.patch(&explicit.path(""))
            .json(&json!({ "expiresAt": "2031-06-01T12:00:00+02:00" })),
    )
    .await;
    assert_eq!(
        token_detail(&ctx, &explicit).await["expiresAt"],
        "2031-06-01T10:00:00.000Z"
    );

    // Moving the expiration into the past locks the token out until it is extended.
    send(
        ctx.patch(&explicit.path(""))
            .json(&json!({ "expiresAt": "2020-01-01T00:00:00.000Z" })),
    )
    .await;
    let (status, body) = initialize_status(&ctx, &explicit.token).await;
    assert_eq!(status, 403);
    assert_eq!(body, json!({ "error": "Forbidden: MCP token has expired" }));
    send(ctx.patch(&explicit.path("")).json(&json!({ "expiresInDays": 1 }))).await;
    let (status, _) = initialize_status(&ctx, &explicit.token).await;
    assert_eq!(status, 200);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn rotating_a_token_keeps_its_token_id_and_invalidates_the_old_secret() {
    let ctx = context().await;
    let issued = create_token(&ctx, &["read_logs"]).await;

    let (status, rotated) = send(ctx.post(&issued.path("/rotate"))).await;
    assert_eq!(status, 200);
    assert_eq!(rotated["ok"], true);
    let rotated_token = rotated["token"].as_str().unwrap();
    assert!(rotated_token.starts_with(&format!("oshp_mcp_{}.", issued.token_id)));
    assert_ne!(rotated_token, issued.token);

    let (status, body) = initialize_status(&ctx, &issued.token).await;
    assert_eq!(status, 401);
    assert_eq!(body, json!({ "error": "Unauthorized: invalid MCP token" }));

    let (status, _) = initialize_status(&ctx, rotated_token).await;
    assert_eq!(status, 200);

    // The token keeps its identity and grants.
    let detail = token_detail(&ctx, &issued).await;
    assert_eq!(detail["id"], issued.id.as_str());
    assert_eq!(detail["tokenId"], issued.token_id.as_str());
    assert_eq!(detail["permissions"], json!(["read_logs"]));
    assert_ne!(detail["tokenFingerprint"], issued.item["tokenFingerprint"]);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn revoked_tokens_cannot_be_rotated() {
    let ctx = context().await;
    let issued = create_token(&ctx, &["read_logs"]).await;
    send(ctx.post(&issued.path("/revoke"))).await;

    let (status, body) = send(ctx.post(&issued.path("/rotate"))).await;
    assert_eq!(status, 409);
    assert_eq!(body["error"], "Cannot rotate a revoked token");
    assert!(body.get("token").is_none());
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn revoked_tokens_cannot_be_reactivated_through_patch() {
    let ctx = context().await;
    let issued = create_token(&ctx, &["read_logs"]).await;
    send(ctx.post(&issued.path("/revoke"))).await;

    let (status, body) = send(
        ctx.patch(&issued.path(""))
            .json(&json!({ "status": "active", "name": "reactivated token" })),
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(body["error"], "Cannot update a revoked token");

    let detail = token_detail(&ctx, &issued).await;
    assert_eq!(detail["status"], "revoked");
    assert_eq!(detail["name"], "test token");
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn revoked_tokens_cannot_have_permissions_changed() {
    let ctx = context().await;
    let issued = create_token(&ctx, &["read_logs"]).await;
    send(ctx.post(&issued.path("/revoke"))).await;

    let (status, body) = send(
        ctx.put(&issued.path("/permissions"))
            .json(&json!({ "permissions": ["read_flows"] })),
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(body["error"], "Cannot update a revoked token");

    let detail = token_detail(&ctx, &issued).await;
    assert_eq!(detail["permissions"], json!(["read_logs"]));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn revoking_a_token_records_the_revocation_and_blocks_it() {
    let ctx = context().await;
    let issued = create_token(&ctx, &[]).await;

    let (status, body) = send(ctx.post(&issued.path("/revoke"))).await;
    assert_eq!((status, body), (200, json!({ "ok": true })));

    let detail = token_detail(&ctx, &issued).await;
    assert_eq!(detail["status"], "revoked");
    let revoked_at = timestamp(&detail["revokedAt"]);
    assert!((Utc::now() - revoked_at).num_seconds().abs() < 60);

    let (status, body) = initialize_status(&ctx, &issued.token).await;
    assert_eq!(status, 403);
    assert_eq!(body, json!({ "error": "Forbidden: MCP token is not active" }));

    // Revoking again is harmless.
    let (status, _) = send(ctx.post(&issued.path("/revoke"))).await;
    assert_eq!(status, 200);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn patch_renames_disables_and_reactivates_tokens() {
    let ctx = context().await;
    let issued = create_token(&ctx, &[]).await;

    let (status, body) = send(
        ctx.patch(&issued.path(""))
            .json(&json!({ "name": "  renamed  ", "status": "disabled" })),
    )
    .await;
    assert_eq!((status, body), (200, json!({ "ok": true })));
    let detail = token_detail(&ctx, &issued).await;
    assert_eq!(detail["name"], "renamed");
    assert_eq!(detail["status"], "disabled");
    assert_eq!(detail["revokedAt"], Value::Null);
    assert!(timestamp(&detail["updatedAt"]) >= timestamp(&issued.item["updatedAt"]));

    let (status, body) = initialize_status(&ctx, &issued.token).await;
    assert_eq!(status, 403);
    assert_eq!(body, json!({ "error": "Forbidden: MCP token is not active" }));

    // Only `active` and `disabled` can be set; revocation has its own endpoint.
    let (status, _) = send(
        ctx.patch(&issued.path(""))
            .json(&json!({ "status": "revoked", "name": "" })),
    )
    .await;
    assert_eq!(status, 200);
    let detail = token_detail(&ctx, &issued).await;
    assert_eq!(detail["status"], "disabled");
    assert_eq!(detail["name"], "renamed");

    send(ctx.patch(&issued.path("")).json(&json!({ "status": "active" }))).await;
    let (status, _) = initialize_status(&ctx, &issued.token).await;
    assert_eq!(status, 200);

    // Rotating a disabled token hands out a working secret again.
    send(ctx.patch(&issued.path("")).json(&json!({ "status": "disabled" }))).await;
    let (_, rotated) = send(ctx.post(&issued.path("/rotate"))).await;
    assert_eq!(token_detail(&ctx, &issued).await["status"], "active");
    let (status, _) = initialize_status(&ctx, rotated["token"].as_str().unwrap()).await;
    assert_eq!(status, 200);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn permissions_can_be_replaced_and_apply_to_the_next_request() {
    let ctx = context().await;
    let issued = create_token(&ctx, &["read_logs"]).await;

    let tools = call(&ctx, &issued.token, "tools/list", json!({})).await;
    assert_eq!(
        strings(&tools["result"]["tools"], "name"),
        ["openshop.shops.list", "openshop.logs.search"]
    );

    let (status, body) = send(
        ctx.put(&issued.path("/permissions"))
            .json(&json!({ "permissions": ["read_flows", "demo:read_status", "read_flows", 3] })),
    )
    .await;
    assert_eq!((status, body), (200, json!({ "ok": true })));
    let detail = token_detail(&ctx, &issued).await;
    assert_eq!(sorted(&detail["permissions"]), ["demo:read_status", "read_flows"]);

    let tools = call(&ctx, &issued.token, "tools/list", json!({})).await;
    assert_eq!(
        strings(&tools["result"]["tools"], "name"),
        ["openshop.shops.list", "openshop.flows.list", "demo.status"]
    );

    let (status, body) = send(
        ctx.put(&issued.path("/permissions"))
            .json(&json!({ "permissions": ["read_logs", "nope"] })),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body, json!({ "error": "Unknown permission \"nope\"" }));
    assert_eq!(
        sorted(&token_detail(&ctx, &issued).await["permissions"]),
        ["demo:read_status", "read_flows"]
    );

    // Without a list every grant is removed.
    let (status, _) = send(ctx.put(&issued.path("/permissions")).json(&json!({}))).await;
    assert_eq!(status, 200);
    assert_eq!(token_detail(&ctx, &issued).await["permissions"], json!([]));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn token_details_report_stale_permissions() {
    let ctx = context().await;
    let issued = create_token(&ctx, &["read_logs"]).await;

    // A grant for a permission the application no longer defines.
    sqlx::query(
        "INSERT INTO mcp_permission_grants (shop, token_id, permission_key) VALUES ($1, $2, 'legacy:read_things')",
    )
    .bind(&ctx.shop)
    .bind(&issued.token_id)
    .execute(&ctx.db)
    .await
    .unwrap();
    // Withdrawn grants are not permissions at all.
    sqlx::query(
        "INSERT INTO mcp_permission_grants (shop, token_id, permission_key, granted) VALUES ($1, $2, 'read_flows', false)",
    )
    .bind(&ctx.shop)
    .bind(&issued.token_id)
    .execute(&ctx.db)
    .await
    .unwrap();

    let detail = token_detail(&ctx, &issued).await;
    assert_eq!(sorted(&detail["permissions"]), ["legacy:read_things", "read_logs"]);
    assert_eq!(detail["stalePermissions"], json!(["legacy:read_things"]));

    let (_, list) = send(ctx.get("/api/mcp/tokens")).await;
    assert_eq!(list[0]["stalePermissions"], json!(["legacy:read_things"]));

    let tools = call(&ctx, &issued.token, "tools/list", json!({})).await;
    assert_eq!(
        strings(&tools["result"]["tools"], "name"),
        ["openshop.shops.list", "openshop.logs.search"]
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn token_details_include_the_ten_most_recent_audits() {
    let ctx = context().await;
    let issued = create_token(&ctx, &[]).await;

    let denied = call_tool(&ctx, &issued.token, "openshop.flows.list", json!({})).await;
    assert_eq!(denied["error"]["code"], -32003);
    let listed = call_tool(&ctx, &issued.token, "openshop.shops.list", json!({})).await;
    assert!(listed.get("error").is_none());

    let detail = token_detail(&ctx, &issued).await;
    let recent = detail["recentAudits"].as_array().unwrap();
    assert_eq!(recent.len(), 2);
    let newest = &recent[0];
    assert_eq!(newest["capabilityType"], "tool");
    assert_eq!(newest["capabilityName"], "openshop.shops.list");
    assert_eq!(newest["permissionKeys"], json!([]));
    assert_eq!(newest["status"], "success");
    assert_eq!(newest["error"], Value::Null);
    assert_eq!(newest["targetShop"], Value::Null);
    assert_eq!(newest["requestId"], "1");
    assert_eq!(newest["tokenId"], issued.token_id.as_str());
    assert_eq!(newest["shop"], ctx.shop.as_str());
    assert_eq!(newest["appHandle"], "default");
    assert!(newest["durationMs"].as_i64().unwrap() >= 0);
    assert!(Uuid::parse_str(newest["id"].as_str().unwrap()).is_ok());
    timestamp(&newest["createdAt"]);
    assert_eq!(recent[1]["capabilityName"], "openshop.flows.list");
    assert_eq!(recent[1]["permissionKeys"], json!(["read_flows"]));
    assert_eq!(recent[1]["status"], "denied");
    assert_eq!(recent[1]["error"], "Permission denied");

    // The list view never embeds audits.
    let (_, list) = send(ctx.get("/api/mcp/tokens")).await;
    assert_eq!(list[0]["recentAudits"], json!([]));

    let insert_audit = |token_id: &str, shop: &str, name: String, seconds: f64| {
        sqlx::query(
            "INSERT INTO mcp_audit_logs (shop, token_id, capability_type, capability_name, permission_keys, status, created_at) \
             VALUES ($1, $2, 'tool', $3, '[]'::json, 'success', now() + make_interval(secs => $4))",
        )
        .bind(shop.to_string())
        .bind(token_id.to_string())
        .bind(name)
        .bind(seconds)
        .execute(&ctx.db)
    };
    for index in 0..12 {
        insert_audit(
            &issued.token_id,
            &ctx.shop,
            format!("bulk.{index}"),
            f64::from(index) + 1.0,
        )
        .await
        .unwrap();
    }
    // Audits of other tokens and other shops stay out.
    insert_audit("mtk_someone_else", &ctx.shop, "foreign.token".to_string(), 100.0)
        .await
        .unwrap();
    insert_audit(&issued.token_id, SHOP_B, "foreign.shop".to_string(), 100.0)
        .await
        .unwrap();

    let detail = token_detail(&ctx, &issued).await;
    let expected: Vec<String> = (2..12).rev().map(|index| format!("bulk.{index}")).collect();
    assert_eq!(strings(&detail["recentAudits"], "capabilityName"), expected);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn admin_token_list_is_isolated_by_shop() {
    let ctx = context().await;
    let shop_b = create_token_as(&ctx, SHOP_B, &["read_logs"], json!({})).await;
    let contains = |list: &Value| {
        list.as_array()
            .unwrap()
            .iter()
            .any(|token| token["tokenId"] == shop_b.token_id.as_str())
    };

    let (_, list_a) = send(ctx.get("/api/mcp/tokens")).await;
    assert!(!contains(&list_a));
    let (_, list_b) = send(admin_as(&ctx, Method::GET, "/api/mcp/tokens", SHOP_B)).await;
    assert!(contains(&list_b));

    // Another shop can neither read nor change the token.
    let attempts = [
        (Method::GET, shop_b.path(""), json!({})),
        (
            Method::PATCH,
            shop_b.path(""),
            json!({ "name": "hijacked", "status": "disabled" }),
        ),
        (Method::PUT, shop_b.path("/permissions"), json!({ "permissions": [] })),
        (Method::POST, shop_b.path("/rotate"), json!({})),
        (Method::POST, shop_b.path("/revoke"), json!({})),
    ];
    for (method, path, body) in attempts {
        let (status, error) = send(ctx.request(method.clone(), &path).json(&body)).await;
        assert_eq!(status, 404, "{method} {path}");
        assert_eq!(error, json!({ "error": "Token not found" }));
    }

    let (status, detail) = send(admin_as(&ctx, Method::GET, &shop_b.path(""), SHOP_B)).await;
    assert_eq!(status, 200);
    assert_eq!(detail["name"], "test token");
    assert_eq!(detail["status"], "active");
    assert_eq!(detail["permissions"], json!(["read_logs"]));
    assert_eq!(detail["tokenFingerprint"], shop_b.item["tokenFingerprint"]);
    let (status, _) = initialize_status(&ctx, &shop_b.token).await;
    assert_eq!(status, 200);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn unknown_and_malformed_token_ids_are_not_found() {
    let ctx = context().await;

    for id in [Uuid::new_v4().to_string(), "not-a-uuid".to_string()] {
        let attempts = [
            (Method::GET, format!("/api/mcp/tokens/{id}")),
            (Method::PATCH, format!("/api/mcp/tokens/{id}")),
            (Method::PUT, format!("/api/mcp/tokens/{id}/permissions")),
            (Method::POST, format!("/api/mcp/tokens/{id}/rotate")),
            (Method::POST, format!("/api/mcp/tokens/{id}/revoke")),
        ];
        for (method, path) in attempts {
            let (status, error) = send(ctx.request(method.clone(), &path).json(&json!({}))).await;
            assert_eq!(status, 404, "{method} {path}");
            assert_eq!(error, json!({ "error": "Token not found" }));
        }
    }
    ctx.shutdown().await.unwrap();
}

// ─── MCP endpoint: authentication and transport ──────────────────────

#[tokio::test]
async fn mcp_rejects_missing_and_invalid_bearer_tokens() {
    let ctx = context().await;
    let initialize = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }).to_string();
    let issued = create_token(&ctx, &[]).await;

    let missing = json!({ "error": "Unauthorized: missing MCP bearer token" });
    let (status, body) = mcp_send(&ctx, None, initialize.clone()).await;
    assert_eq!((status, body), (401, missing.clone()));
    for authorization in ["Basic abc", "Bearer", &format!("bearer {}", issued.token)] {
        let (status, body) = mcp_send(&ctx, Some(authorization), initialize.clone()).await;
        assert_eq!((status, body), (401, missing.clone()), "{authorization}");
    }

    // A Shopify session token is not an MCP token.
    let malformed = json!({ "error": "Unauthorized: invalid MCP token format" });
    for token in ["not-a-token", "oshp_mcp_tok_abc.secret", &ctx.session_token()] {
        let (status, body) = mcp_send(&ctx, Some(&bearer(token)), initialize.clone()).await;
        assert_eq!((status, body), (401, malformed.clone()), "{token}");
    }

    let (status, body) = mcp_send(&ctx, Some("Bearer oshp_mcp_mtk_unknown.secret"), initialize.clone()).await;
    assert_eq!(status, 401);
    assert_eq!(body, json!({ "error": "Unauthorized: unknown MCP token" }));

    let (status, body) = mcp_send(&ctx, Some(&bearer(&format!("{}x", issued.token))), initialize.clone()).await;
    assert_eq!(status, 401);
    assert_eq!(body, json!({ "error": "Unauthorized: invalid MCP token" }));

    // Rejected attempts do not count as usage.
    assert_eq!(token_detail(&ctx, &issued).await["lastUsedAt"], Value::Null);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn mcp_rejects_expired_and_revoked_tokens() {
    let ctx = context().await;
    let shop = ctx.shop.clone();

    let expires_at = (Utc::now() - chrono::Duration::seconds(60)).to_rfc3339();
    let expired = create_token_as(&ctx, &shop, &[], json!({ "expiresAt": expires_at })).await;
    let (status, body) = initialize_status(&ctx, &expired.token).await;
    assert_eq!(status, 403);
    assert_eq!(body, json!({ "error": "Forbidden: MCP token has expired" }));

    let active = create_token(&ctx, &[]).await;
    send(ctx.post(&active.path("/revoke"))).await;
    let (status, body) = initialize_status(&ctx, &active.token).await;
    assert_eq!(status, 403);
    assert_eq!(body, json!({ "error": "Forbidden: MCP token is not active" }));

    // The state of a token is only disclosed to whoever holds its secret.
    let (status, body) = initialize_status(&ctx, &format!("{}x", active.token)).await;
    assert_eq!(status, 401);
    assert_eq!(body, json!({ "error": "Unauthorized: invalid MCP token" }));

    for issued in [&expired, &active] {
        assert_eq!(token_detail(&ctx, issued).await["lastUsedAt"], Value::Null);
        assert!(audits(&ctx, &issued.token_id).await.is_empty());
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn mcp_calls_update_last_used_at() {
    let ctx = context().await;
    let issued = create_token(&ctx, &[]).await;
    assert_eq!(issued.item["lastUsedAt"], Value::Null);

    let before = Utc::now() - chrono::Duration::seconds(1);
    let (status, _) = initialize_status(&ctx, &issued.token).await;
    assert_eq!(status, 200);

    let detail = token_detail(&ctx, &issued).await;
    let last_used_at = timestamp(&detail["lastUsedAt"]);
    assert!(last_used_at >= before, "{last_used_at} < {before}");
    assert!(timestamp(&detail["updatedAt"]) >= last_used_at - chrono::Duration::milliseconds(1));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn mcp_is_rejected_when_disabled_in_the_config() {
    // Turning MCP off turns its admin page off too: this application keeps
    // the page, to manage tokens while the protocol is refused.
    let config = OpenShopConfig::builder()
        .flow(log_flow())
        .mcp(McpConfig::disabled())
        .page(AdminPageId::Mcp, AdminPageMode::Visible)
        .build()
        .unwrap();
    let ctx = TestContext::new(config).await.unwrap();

    let (_, capabilities) = send(ctx.get("/api/mcp/capabilities")).await;
    assert_eq!(capabilities["enabled"], false);

    // Tokens can still be managed.
    let issued = create_token(&ctx, &["read_logs"]).await;
    let (status, body) = initialize_status(&ctx, &issued.token).await;
    assert_eq!(status, 403);
    assert_eq!(body, json!({ "error": "MCP is disabled" }));

    // Authentication comes first.
    let (status, body) = mcp_send(&ctx, None, "{}").await;
    assert_eq!(status, 401);
    assert_eq!(body, json!({ "error": "Unauthorized: missing MCP bearer token" }));
    ctx.shutdown().await.unwrap();
}

fn padded_request(size: usize) -> String {
    let prefix = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","padding":""#;
    let suffix = r#""}"#;
    format!("{prefix}{}{suffix}", "x".repeat(size - prefix.len() - suffix.len()))
}

/// Sends the body with chunked transfer encoding, i.e. without a `Content-Length`.
async fn post_mcp_chunked(ctx: &TestContext, token: &str, body: String) -> String {
    let port = ctx.port;
    let authorization = bearer(token);
    tokio::task::spawn_blocking(move || {
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        let head = format!(
            "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: {authorization}\r\n\
             Content-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(head.as_bytes())?;
        let (first, last) = body.split_at(body.len() - 1);
        stream.write_all(format!("{:x}\r\n", first.len()).as_bytes())?;
        stream.write_all(first.as_bytes())?;
        // The final byte and the terminating chunk travel together.
        stream.write_all(format!("\r\n1\r\n{last}\r\n0\r\n\r\n").as_bytes())?;
        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        Ok::<_, std::io::Error>(response)
    })
    .await
    .unwrap()
    .unwrap()
}

#[tokio::test]
async fn mcp_rejects_oversized_request_bodies_without_relying_on_content_length() {
    let ctx = context().await;
    let issued = create_token(&ctx, &["read_logs"]).await;
    let authorization = bearer(&issued.token);

    let (status, body) = mcp_send(&ctx, Some(&authorization), padded_request(MAX_PAYLOAD_BYTES + 1)).await;
    assert_eq!(status, 413);
    assert_eq!(body, json!({ "error": "MCP payload too large" }));

    // The limit itself is still accepted.
    let (status, body) = mcp_send(&ctx, Some(&authorization), padded_request(MAX_PAYLOAD_BYTES)).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"]["protocolVersion"], PROTOCOL_VERSION);

    let response = post_mcp_chunked(&ctx, &issued.token, padded_request(MAX_PAYLOAD_BYTES + 1)).await;
    let (head, body) = response.split_once("\r\n\r\n").expect("HTTP response");
    assert!(head.starts_with("HTTP/1.1 413 "), "{head}");
    assert!(
        head.to_lowercase().contains("mcp-protocol-version: 2025-11-25"),
        "{head}"
    );
    assert!(body.contains("MCP payload too large"), "{body}");

    let response = post_mcp_chunked(&ctx, &issued.token, padded_request(4_096)).await;
    assert!(response.starts_with("HTTP/1.1 200 "), "{response}");
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn mcp_rejects_invalid_json_rpc_requests() {
    let ctx = context().await;
    let issued = create_token(&ctx, &[]).await;
    let authorization = bearer(&issued.token);

    let invalid = json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": { "code": -32600, "message": "Invalid JSON-RPC request" },
    });
    for body in [
        "",
        "not json",
        "[]",
        r#"{"jsonrpc":"1.0","id":1,"method":"initialize"}"#,
        r#"{"jsonrpc":"2.0","id":1,"method":7}"#,
    ] {
        let (status, response) = mcp_send(&ctx, Some(&authorization), body).await;
        assert_eq!((status, response), (400, invalid.clone()), "{body}");
    }
    assert!(audits(&ctx, &issued.token_id).await.is_empty());
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn mcp_accepts_notifications_and_reports_unknown_methods() {
    let ctx = context().await;
    let issued = create_token(&ctx, &[]).await;

    let (status, body) = rpc(
        &ctx,
        &issued.token,
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    )
    .await;
    assert_eq!(status, 202);
    assert_eq!(body, "");

    // With an id it is an ordinary request for a method that does not exist.
    let (status, body) = rpc(
        &ctx,
        &issued.token,
        json!({ "jsonrpc": "2.0", "id": 7, "method": "notifications/initialized" }),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        body,
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "error": { "code": -32601, "message": "Method not found: notifications/initialized" },
        })
    );

    let (status, body) = rpc(
        &ctx,
        &issued.token,
        json!({ "jsonrpc": "2.0", "id": "abc", "method": "prompts/list" }),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["id"], "abc");
    assert_eq!(
        body["error"],
        json!({ "code": -32601, "message": "Method not found: prompts/list" })
    );

    // Requests without an id are answered with a null id.
    let (status, body) = rpc(&ctx, &issued.token, json!({ "jsonrpc": "2.0", "method": "tools/list" })).await;
    assert_eq!(status, 200);
    assert_eq!(body["id"], Value::Null);
    assert_eq!(strings(&body["result"]["tools"], "name"), ["openshop.shops.list"]);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn initialize_describes_the_server() {
    let ctx = context().await;
    let issued = create_token(&ctx, &[]).await;

    let (status, body) = rpc(
        &ctx,
        &issued.token,
        json!({ "jsonrpc": "2.0", "id": "init-1", "method": "initialize", "params": { "protocolVersion": "2025-06-18" } }),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        body,
        json!({
            "jsonrpc": "2.0",
            "id": "init-1",
            "result": {
                "protocolVersion": "2025-11-25",
                "capabilities": {
                    "tools": { "listChanged": false },
                    "resources": { "subscribe": false, "listChanged": false },
                },
                "serverInfo": { "name": "openshop", "version": "0.1.1" },
            },
        })
    );

    // The endpoint also answers with a trailing slash.
    let (status, body) = send(
        ctx.http
            .post(format!("{}/mcp/", ctx.url))
            .bearer_auth(&issued.token)
            .json(&json!({ "jsonrpc": "2.0", "id": 2, "method": "initialize" })),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["result"]["serverInfo"]["name"], "openshop");
    assert!(audits(&ctx, &issued.token_id).await.is_empty());
    ctx.shutdown().await.unwrap();
}

// ─── MCP endpoint: listings ──────────────────────────────────────────

#[tokio::test]
async fn tools_list_only_returns_tools_allowed_by_granted_permissions() {
    let ctx = context().await;
    let issued = create_token(&ctx, &["read_logs"]).await;

    let body = call(&ctx, &issued.token, "tools/list", Value::Null).await;
    let names = strings(&body["result"]["tools"], "name");
    assert!(names.contains(&"openshop.shops.list".to_string()));
    assert!(names.contains(&"openshop.logs.search".to_string()));
    assert!(!names.contains(&"openshop.admin.graphql".to_string()));
    assert!(!names.contains(&"demo.status".to_string()));
    assert_eq!(names.len(), 2);
    assert_eq!(
        body["result"]["tools"][0],
        json!({
            "name": "openshop.shops.list",
            "description": "List installed shops for this OpenShop app.",
            "inputSchema": { "type": "object", "additionalProperties": false, "properties": {} },
        })
    );
    assert_eq!(body["result"]["tools"][1]["inputSchema"]["required"], json!(["runId"]));

    // Core tools come first, then the application's tools.
    let everything = [&CORE_PERMISSIONS[..], &["demo:read_status"]].concat();
    let all = create_token(&ctx, &everything).await;
    let body = call(&ctx, &all.token, "tools/list", json!({})).await;
    let tools = &body["result"]["tools"];
    assert_eq!(strings(tools, "name"), [&CORE_TOOLS[..], &["demo.status"]].concat());
    // A tool without a schema accepts any object.
    assert_eq!(
        tools[7],
        json!({
            "name": "demo.status",
            "description": "Read status",
            "inputSchema": { "type": "object", "properties": {} },
        })
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn resources_list_only_returns_resources_allowed_by_granted_permissions() {
    let ctx = context().await;

    let none = create_token(&ctx, &[]).await;
    let body = call(&ctx, &none.token, "resources/list", json!({})).await;
    assert_eq!(
        body["result"]["resources"],
        json!([
            {
                "uri": "openshop://permissions",
                "name": "MCP permission catalog",
                "description": "Core and custom MCP permission definitions visible to this OpenShop app.",
                "mimeType": "application/json",
            },
            { "uri": "openshop://demo/status", "name": "Demo status docs", "mimeType": "text/plain" },
        ])
    );

    let logs = create_token(&ctx, &["read_logs"]).await;
    let body = call(&ctx, &logs.token, "resources/list", json!({})).await;
    assert_eq!(
        strings(&body["result"]["resources"], "uri"),
        [
            "openshop://docs/log-search",
            "openshop://permissions",
            "openshop://tools/openshop.logs.search",
            "openshop://demo/status",
        ]
    );

    let flows = create_token(&ctx, &["read_logs", "read_flows"]).await;
    let body = call(&ctx, &flows.token, "resources/list", json!({})).await;
    assert_eq!(
        strings(&body["result"]["resources"], "uri"),
        [&CORE_RESOURCES[..], &["openshop://demo/status"]].concat()
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn resources_list_and_resources_read_expose_documentation_resources() {
    let ctx = context().await;
    let issued = create_token(&ctx, &["read_logs"]).await;

    let list = call(&ctx, &issued.token, "resources/list", json!({})).await;
    let uris = strings(&list["result"]["resources"], "uri");
    assert!(uris.contains(&"openshop://docs/log-search".to_string()));

    let read = read_resource(&ctx, &issued.token, "openshop://docs/log-search").await;
    let contents = read["result"]["contents"].as_array().unwrap();
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0]["uri"], "openshop://docs/log-search");
    assert_eq!(contents[0]["mimeType"], "text/markdown");
    let text = contents[0]["text"].as_str().unwrap();
    assert!(text.starts_with("# OpenShop log search\n"));
    assert!(text.contains("openshop.logs.search"));

    let recorded = audits(&ctx, &issued.token_id).await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].capability_type.as_deref(), Some("resource"));
    assert_eq!(
        recorded[0].capability_name.as_deref(),
        Some("openshop://docs/log-search")
    );
    assert_eq!(recorded[0].permission_keys, Some(json!(["read_logs"])));
    assert_eq!(recorded[0].status, "success");
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn resources_read_enforces_permissions_and_reports_unknown_resources() {
    let ctx = context().await;
    let issued = create_token(&ctx, &[]).await;

    let denied = read_resource(&ctx, &issued.token, "openshop://docs/log-search").await;
    assert_eq!(
        denied["error"],
        json!({ "code": -32003, "message": "Permission denied for resource \"openshop://docs/log-search\"" })
    );
    assert!(denied.get("result").is_none());

    let unknown = read_resource(&ctx, &issued.token, "openshop://nope").await;
    assert_eq!(
        unknown["error"],
        json!({ "code": -32602, "message": "Unknown resource \"openshop://nope\"" })
    );
    let missing = call(&ctx, &issued.token, "resources/read", json!({})).await;
    assert_eq!(
        missing["error"],
        json!({ "code": -32602, "message": "Unknown resource \"\"" })
    );

    // Only the denied read of an existing resource is audited.
    let recorded = audits(&ctx, &issued.token_id).await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].capability_type.as_deref(), Some("resource"));
    assert_eq!(
        recorded[0].capability_name.as_deref(),
        Some("openshop://docs/log-search")
    );
    assert_eq!(recorded[0].permission_keys, Some(json!(["read_logs"])));
    assert_eq!(recorded[0].status, "denied");
    assert_eq!(recorded[0].error.as_deref(), Some("Permission denied"));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn core_resources_describe_permissions_tools_and_recent_runs() {
    let ctx = context().await;
    let shop = ctx.shop.clone();
    let own_run = logged_run(&ctx, "default", &shop).await;
    logged_run(&ctx, "default", SHOP_B).await;
    let issued = create_token(&ctx, &["read_logs", "read_flows"]).await;

    let content = |body: &Value| -> (String, Value) {
        let content = &body["result"]["contents"][0];
        let parsed = serde_json::from_str(content["text"].as_str().unwrap()).unwrap();
        (content["mimeType"].as_str().unwrap().to_string(), parsed)
    };

    let (mime_type, permissions) = content(&read_resource(&ctx, &issued.token, "openshop://permissions").await);
    assert_eq!(mime_type, "application/json");
    let core: Vec<&String> = permissions["core"].as_object().unwrap().keys().collect();
    assert_eq!(core, CORE_PERMISSIONS);
    assert_eq!(
        permissions["core"]["run_flows"],
        json!({ "label": "Run flows", "description": "Trigger OpenShop flow runs.", "group": "Flows", "riskLevel": "high" })
    );
    assert_eq!(
        permissions["custom"],
        json!({ "demo:read_status": { "label": "Read demo status" } })
    );

    let (mime_type, reference) =
        content(&read_resource(&ctx, &issued.token, "openshop://tools/openshop.logs.search").await);
    assert_eq!(mime_type, "application/json");
    assert_eq!(reference["name"], "openshop.logs.search");
    assert_eq!(reference["description"], "Search logs for a visible OpenShop flow run.");
    assert_eq!(reference["requiredPermissions"], json!(["read_logs"]));
    assert_eq!(reference["inputSchema"]["properties"]["limit"]["maximum"], 200);

    // Only the runs of the token's shop.
    let (mime_type, recent) = content(&read_resource(&ctx, &issued.token, "openshop://runs/recent").await);
    assert_eq!(mime_type, "application/json");
    let runs = recent["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["id"], own_run.to_string());
    assert_eq!(runs[0]["flowName"], "log-flow");
    assert_eq!(runs[0]["status"], "completed");
    assert!(timestamp(&runs[0]["completedAt"]) >= timestamp(&runs[0]["createdAt"]));
    ctx.shutdown().await.unwrap();
}

// ─── MCP endpoint: tools/call ────────────────────────────────────────

#[tokio::test]
async fn tools_call_denies_missing_permissions_and_writes_audit() {
    let ctx = context().await;
    let shop = ctx.shop.clone();
    let run_id = logged_run(&ctx, "default", &shop).await;
    let issued = create_token(&ctx, &[]).await;

    let (status, body) = rpc(
        &ctx,
        &issued.token,
        json!({
            "jsonrpc": "2.0",
            "id": "req-42",
            "method": "tools/call",
            "params": { "name": "openshop.logs.search", "arguments": { "runId": run_id } },
        }),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        body,
        json!({
            "jsonrpc": "2.0",
            "id": "req-42",
            "error": { "code": -32003, "message": "Permission denied for tool \"openshop.logs.search\"" },
        })
    );

    let recorded = audits(&ctx, &issued.token_id).await;
    assert_eq!(recorded.len(), 1);
    let audit = &recorded[0];
    assert_eq!(audit.status, "denied");
    assert_eq!(audit.error.as_deref(), Some("Permission denied"));
    assert_eq!(audit.capability_type.as_deref(), Some("tool"));
    assert_eq!(audit.capability_name.as_deref(), Some("openshop.logs.search"));
    assert_eq!(audit.permission_keys, Some(json!(["read_logs"])));
    assert_eq!(audit.app_handle, "default");
    assert_eq!(audit.shop, shop);
    assert_eq!(audit.target_shop, None);
    assert_eq!(audit.request_id.as_deref(), Some("req-42"));
    assert!(audit.duration_ms.is_some_and(|duration| duration >= 0));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn tools_call_validates_arguments_before_running_a_tool() {
    let runs = Arc::new(AtomicUsize::new(0));
    let counter = runs.clone();
    let mcp = McpConfig::new().tool(
        "demo.count",
        McpTool::new("Counts its runs", move |_ctx, _input| {
            let counter = counter.clone();
            async move { Ok(json!(counter.fetch_add(1, Ordering::SeqCst) + 1)) }
        })
        .input_schema(json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["mode"],
            "properties": {
                "mode": { "type": "string", "enum": ["fast", "slow"] },
                "tags": { "type": "array", "items": { "type": "string" } },
            },
        })),
    );
    let ctx = TestContext::new(config_with(mcp)).await.unwrap();
    let issued = create_token(&ctx, &["read_logs"]).await;

    let body = call_tool(&ctx, &issued.token, "openshop.logs.search", json!({ "runId": 123 })).await;
    assert_eq!(
        body["error"],
        json!({ "code": -32602, "message": "arguments.runId must be string" })
    );

    let invalid = [
        (
            "openshop.logs.search",
            json!({ "limit": 5 }),
            "arguments.runId is required",
        ),
        (
            "openshop.logs.search",
            json!({ "runId": "x", "limit": 201 }),
            "arguments.limit must be <= 200",
        ),
        ("demo.count", json!([]), "arguments must be an object"),
        (
            "demo.count",
            json!({ "mode": "fast", "extra": true }),
            "arguments.extra is not allowed",
        ),
        (
            "demo.count",
            json!({ "mode": "warp" }),
            "arguments.mode must be one of \"fast\", \"slow\"",
        ),
        (
            "demo.count",
            json!({ "mode": "fast", "tags": ["a", 2] }),
            "arguments.tags[1] must be string",
        ),
    ];
    for (tool, arguments, message) in invalid {
        let body = call_tool(&ctx, &issued.token, tool, arguments).await;
        assert_eq!(body["error"], json!({ "code": -32602, "message": message }), "{tool}");
    }

    let unknown = call_tool(&ctx, &issued.token, "demo.missing", json!({})).await;
    assert_eq!(
        unknown["error"],
        json!({ "code": -32602, "message": "Unknown tool \"demo.missing\"" })
    );
    let unnamed = call(&ctx, &issued.token, "tools/call", json!({ "name": 5 })).await;
    assert_eq!(
        unnamed["error"],
        json!({ "code": -32602, "message": "Unknown tool \"\"" })
    );

    // Nothing ran and nothing was audited.
    assert_eq!(runs.load(Ordering::SeqCst), 0);
    assert!(audits(&ctx, &issued.token_id).await.is_empty());

    let body = call_tool(
        &ctx,
        &issued.token,
        "demo.count",
        json!({ "mode": "fast", "tags": ["a"] }),
    )
    .await;
    assert_eq!(body["result"], json!({ "content": [{ "type": "text", "text": "1" }] }));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn custom_tools_and_resources_are_served_and_audited() {
    let ctx = context().await;
    let issued = create_token(&ctx, &["demo:read_status"]).await;

    let body = call_tool(&ctx, &issued.token, "demo.status", json!({})).await;
    assert_eq!(
        body["result"],
        json!({
            "content": [{ "type": "text", "text": "{\n  \"ok\": true\n}" }],
            "structuredContent": { "ok": true },
        })
    );
    // Arguments may be omitted altogether.
    let body = call(&ctx, &issued.token, "tools/call", json!({ "name": "demo.status" })).await;
    assert_eq!(body["result"]["structuredContent"], json!({ "ok": true }));

    let body = read_resource(&ctx, &issued.token, "openshop://demo/status").await;
    assert_eq!(
        body["result"],
        json!({ "contents": [{ "uri": "openshop://demo/status", "mimeType": "text/plain", "text": "demo" }] })
    );

    let recorded = audits(&ctx, &issued.token_id).await;
    let summary: Vec<Value> = recorded
        .iter()
        .map(|audit| {
            json!([
                audit.capability_type,
                audit.capability_name,
                audit.permission_keys,
                audit.status
            ])
        })
        .collect();
    assert_eq!(
        summary,
        [
            json!(["tool", "demo.status", ["demo:read_status"], "success"]),
            json!(["tool", "demo.status", ["demo:read_status"], "success"]),
            json!(["resource", "openshop://demo/status", [], "success"]),
        ]
    );
    assert!(recorded.iter().all(|audit| audit.error.is_none()));

    // Without the custom permission the tool is neither listed nor callable.
    let other = create_token(&ctx, &["read_logs"]).await;
    let body = call_tool(&ctx, &other.token, "demo.status", json!({})).await;
    assert_eq!(body["error"]["code"], -32003);
    ctx.shutdown().await.unwrap();
}

fn showcase_mcp() -> McpConfig {
    McpConfig::new()
        .permission(McpPermission::new("demo:use_tools", "Use demo tools"))
        .tool(
            "demo.context",
            McpTool::new("Echo the execution context", |ctx: McpContext, input| async move {
                let (tokens,): (i64,) = sqlx::query_as("SELECT count(*) FROM mcp_tokens")
                    .fetch_one(&ctx.db)
                    .await?;
                Ok(json!({
                    "shop": ctx.shop,
                    "appHandle": ctx.shopify_app,
                    "tokenId": ctx.token_id,
                    "permissions": ctx.permissions,
                    "cancelled": ctx.signal.is_cancelled(),
                    "flows": ctx.app.config().flows.keys().collect::<Vec<_>>(),
                    "tokens": tokens,
                    "input": input,
                }))
            })
            .permissions(["demo:use_tools", "demo:use_tools"]),
        )
        .tool(
            "demo.text",
            McpTool::new("Plain text", |_ctx, _input| async { Ok(json!("plain text")) }),
        )
        .tool(
            "demo.content",
            McpTool::new("Explicit content", |_ctx, _input| async {
                Ok(json!({ "content": [{ "type": "text", "text": "custom" }], "isError": false }))
            }),
        )
        .tool(
            "demo.list",
            McpTool::new("A JSON array", |_ctx, _input| async { Ok(json!([1, "two"])) }),
        )
        .tool(
            "demo.nothing",
            McpTool::new("No result", |_ctx, _input| async { Ok(Value::Null) }),
        )
        .tool(
            "demo.fail",
            McpTool::new("Always fails", |_ctx, _input| async { Err(anyhow!("boom")) }),
        )
        .tool(
            "demo.panic",
            McpTool::new("Always panics", |_ctx, input| async move {
                let missing: Option<&Value> = input.get("missing");
                Ok(missing.expect("tool exploded").clone())
            }),
        )
        .resource(
            "demo://report",
            McpResource::new("Report", |ctx: McpContext| async move {
                Ok(McpResourceContent {
                    text: json!({ "shop": ctx.shop }).to_string(),
                    mime_type: Some("application/json".to_string()),
                })
            })
            .mime_type("text/plain")
            .permissions(["demo:use_tools"]),
        )
        .resource(
            "demo://notes",
            McpResource::new("Notes", |_ctx| async { Ok("# Notes".into()) })
                .description("Markdown notes")
                .mime_type("text/markdown"),
        )
        .resource(
            "demo://broken",
            McpResource::new("Broken", |_ctx| async { Err(anyhow!("unreadable")) }),
        )
}

#[tokio::test]
async fn custom_tools_receive_the_execution_context_and_have_their_results_normalized() {
    let ctx = TestContext::new(config_with(showcase_mcp())).await.unwrap();
    let issued = create_token(&ctx, &["demo:use_tools", "read_logs"]).await;

    let body = call_tool(&ctx, &issued.token, "demo.context", json!({ "anything": [1, 2] })).await;
    let context = &body["result"]["structuredContent"];
    assert_eq!(context["shop"], ctx.shop.as_str());
    assert_eq!(context["appHandle"], "default");
    assert_eq!(context["tokenId"], issued.token_id.as_str());
    assert_eq!(sorted(&context["permissions"]), ["demo:use_tools", "read_logs"]);
    assert_eq!(context["cancelled"], false);
    assert_eq!(context["flows"], json!(["log-flow"]));
    assert_eq!(context["tokens"], 1);
    assert_eq!(context["input"], json!({ "anything": [1, 2] }));
    assert_eq!(
        serde_json::from_str::<Value>(body["result"]["content"][0]["text"].as_str().unwrap()).unwrap(),
        *context
    );

    let body = call_tool(&ctx, &issued.token, "demo.text", json!({})).await;
    assert_eq!(
        body["result"],
        json!({ "content": [{ "type": "text", "text": "plain text" }] })
    );

    let body = call_tool(&ctx, &issued.token, "demo.content", json!({})).await;
    assert_eq!(
        body["result"],
        json!({ "content": [{ "type": "text", "text": "custom" }], "isError": false })
    );

    let body = call_tool(&ctx, &issued.token, "demo.list", json!({})).await;
    assert_eq!(body["result"]["structuredContent"], json!([1, "two"]));
    assert_eq!(body["result"]["content"][0]["text"], "[\n  1,\n  \"two\"\n]");

    let body = call_tool(&ctx, &issued.token, "demo.nothing", json!({})).await;
    assert_eq!(body["result"], json!({ "content": [{ "type": "text", "text": "" }] }));

    // Repeated required permissions are recorded once.
    let recorded = audits(&ctx, &issued.token_id).await;
    assert_eq!(recorded.len(), 5);
    assert_eq!(recorded[0].permission_keys, Some(json!(["demo:use_tools"])));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn custom_resources_choose_their_mime_type() {
    let ctx = TestContext::new(config_with(showcase_mcp())).await.unwrap();
    let issued = create_token(&ctx, &["demo:use_tools"]).await;

    let list = call(&ctx, &issued.token, "resources/list", json!({})).await;
    let listed = list["result"]["resources"].as_array().unwrap();
    assert_eq!(
        listed[1..],
        [
            json!({ "uri": "demo://report", "name": "Report", "mimeType": "text/plain" }),
            json!({ "uri": "demo://notes", "name": "Notes", "description": "Markdown notes", "mimeType": "text/markdown" }),
            json!({ "uri": "demo://broken", "name": "Broken", "mimeType": "text/plain" }),
        ]
    );

    // The content's own type wins over the definition's.
    let body = read_resource(&ctx, &issued.token, "demo://report").await;
    assert_eq!(
        body["result"]["contents"],
        json!([{
            "uri": "demo://report",
            "mimeType": "application/json",
            "text": json!({ "shop": ctx.shop }).to_string(),
        }])
    );
    let body = read_resource(&ctx, &issued.token, "demo://notes").await;
    assert_eq!(
        body["result"]["contents"],
        json!([{ "uri": "demo://notes", "mimeType": "text/markdown", "text": "# Notes" }])
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn tool_and_resource_errors_are_reported_and_audited() {
    let ctx = TestContext::new(config_with(showcase_mcp())).await.unwrap();
    let issued = create_token(&ctx, &[]).await;

    let (status, body) = rpc(
        &ctx,
        &issued.token,
        json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": { "name": "demo.fail" } }),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        body,
        json!({ "jsonrpc": "2.0", "id": 9, "error": { "code": -32000, "message": "boom" } })
    );

    let body = read_resource(&ctx, &issued.token, "demo://broken").await;
    assert_eq!(body["error"], json!({ "code": -32000, "message": "unreadable" }));

    let recorded = audits(&ctx, &issued.token_id).await;
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].capability_type.as_deref(), Some("tool"));
    assert_eq!(recorded[0].capability_name.as_deref(), Some("demo.fail"));
    assert_eq!(recorded[0].status, "error");
    assert_eq!(recorded[0].error.as_deref(), Some("boom"));
    assert_eq!(recorded[0].request_id.as_deref(), Some("9"));
    assert_eq!(recorded[1].capability_type.as_deref(), Some("resource"));
    assert_eq!(recorded[1].capability_name.as_deref(), Some("demo://broken"));
    assert_eq!(recorded[1].status, "error");
    assert_eq!(recorded[1].error.as_deref(), Some("unreadable"));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_panicking_tool_is_reported_as_an_error_and_audited() {
    let ctx = TestContext::new(config_with(showcase_mcp())).await.unwrap();
    let issued = create_token(&ctx, &[]).await;

    let body = call_tool(&ctx, &issued.token, "demo.panic", json!({})).await;
    assert_eq!(body["error"]["code"], -32000);
    assert_eq!(body["error"]["message"], "Tool \"demo.panic\" panicked");

    let recorded = audits(&ctx, &issued.token_id).await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].capability_name.as_deref(), Some("demo.panic"));
    assert_eq!(recorded[0].status, "error");
    assert_eq!(recorded[0].error.as_deref(), Some("Tool \"demo.panic\" panicked"));

    // The server keeps answering.
    let body = call_tool(&ctx, &issued.token, "demo.text", json!({})).await;
    assert_eq!(body["result"]["content"][0]["text"], "plain text");
    ctx.shutdown().await.unwrap();
}

#[derive(Default)]
struct SlowToolProbe {
    started: tokio::sync::Notify,
    signal: Mutex<Option<CancellationToken>>,
    finished: AtomicBool,
    dropped: AtomicBool,
}

struct MarkDropped(Arc<SlowToolProbe>);

impl Drop for MarkDropped {
    fn drop(&mut self) {
        self.0.dropped.store(true, Ordering::SeqCst);
    }
}

/// An application whose `demo.slow` tool reports that it started and then waits forever.
fn slow_tool_config(probe: &Arc<SlowToolProbe>) -> OpenShopConfig {
    let shared = probe.clone();
    config_with(McpConfig::new().tool(
        "demo.slow",
        McpTool::new("Never finishes", move |ctx: McpContext, _input| {
            let probe = shared.clone();
            async move {
                let _marker = MarkDropped(probe.clone());
                *probe.signal.lock().unwrap() = Some(ctx.signal.clone());
                probe.started.notify_one();
                std::future::pending::<()>().await;
                probe.finished.store(true, Ordering::SeqCst);
                Ok(Value::Null)
            }
        }),
    ))
}

#[tokio::test]
async fn a_tool_exceeding_the_execution_limit_is_cut_off() {
    let probe = Arc::new(SlowToolProbe::default());
    let ctx = TestContext::new(slow_tool_config(&probe)).await.unwrap();
    let issued = create_token(&ctx, &[]).await;

    let pending = tokio::spawn(
        ctx.http
            .post(format!("{}/mcp", ctx.url))
            .bearer_auth(&issued.token)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": "demo.slow", "arguments": {} },
            }))
            .send(),
    );

    // The server runs on this runtime: once the tool is waiting, skip past the 30 second limit.
    probe.started.notified().await;
    assert!(!probe.dropped.load(Ordering::SeqCst));
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(31)).await;
    tokio::time::resume();

    let response = pending.await.unwrap().unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["mcp-protocol-version"].to_str().unwrap(),
        PROTOCOL_VERSION
    );
    let body: Value = response.json().await.unwrap();
    let message = "Tool \"demo.slow\" timed out after 30000ms";
    assert_eq!(body["error"], json!({ "code": -32000, "message": message }));

    assert!(probe.dropped.load(Ordering::SeqCst));
    assert!(!probe.finished.load(Ordering::SeqCst));
    let signal = probe.signal.lock().unwrap().clone().expect("tool started");
    assert!(signal.is_cancelled());

    let recorded = audits(&ctx, &issued.token_id).await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].capability_name.as_deref(), Some("demo.slow"));
    assert_eq!(recorded[0].status, "error");
    assert_eq!(recorded[0].error.as_deref(), Some(message));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_client_disconnect_drops_the_running_tool_and_cancels_its_signal() {
    let probe = Arc::new(SlowToolProbe::default());
    let ctx = TestContext::new(slow_tool_config(&probe)).await.unwrap();
    let issued = create_token(&ctx, &[]).await;

    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "demo.slow", "arguments": {} },
    })
    .to_string();
    let mut client = std::net::TcpStream::connect(("127.0.0.1", ctx.port)).unwrap();
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: {}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\n\r\n{body}",
        ctx.port,
        bearer(&issued.token),
        body.len()
    );
    client.write_all(request.as_bytes()).unwrap();

    probe.started.notified().await;
    assert!(!probe.dropped.load(Ordering::SeqCst));
    drop(client);

    tokio::time::timeout(Duration::from_secs(20), async {
        while !probe.dropped.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the tool is dropped once its client is gone");
    assert!(!probe.finished.load(Ordering::SeqCst));
    let signal = probe.signal.lock().unwrap().clone().expect("tool started");
    assert!(signal.is_cancelled());

    // Nobody is left to answer: the aborted call is not audited.
    assert!(audits(&ctx, &issued.token_id).await.is_empty());
    ctx.shutdown().await.unwrap();
}

// ─── Core tools ──────────────────────────────────────────────────────

#[tokio::test]
async fn openshop_shops_list_is_available_without_grants_and_lists_active_installations_for_the_token_app() {
    let ctx = context().await;
    let shop = ctx.shop.clone();
    install(&ctx, "default", &shop, Some("token-a")).await;
    install(&ctx, "default", SHOP_B, Some("token-b")).await;
    install(&ctx, "default", "uninstalled.myshopify.com", Some("token-c")).await;
    uninstall(&ctx, "uninstalled.myshopify.com").await;
    install(&ctx, "default", "pending.myshopify.com", None).await;
    install(&ctx, "other", "hidden.myshopify.com", Some("token-d")).await;
    let issued = create_token(&ctx, &[]).await;

    let body = call_tool(&ctx, &issued.token, "openshop.shops.list", json!({})).await;
    let listed = &body["result"]["structuredContent"]["shops"];
    assert_eq!(strings(listed, "shop"), [SHOP_B, shop.as_str()]);
    assert_eq!(listed[0]["scopes"], "read_products");
    timestamp(&listed[0]["installedAt"]);
    // Access tokens never leave the server.
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(!text.contains("token-"));
    assert_eq!(
        serde_json::from_str::<Value>(text).unwrap(),
        body["result"]["structuredContent"]
    );

    let recorded = audits(&ctx, &issued.token_id).await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].status, "success");
    assert_eq!(recorded[0].permission_keys, Some(json!([])));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn openshop_admin_graphql_requires_its_explicit_permission() {
    let ctx = context().await;
    let shop = ctx.shop.clone();
    install(&ctx, "default", &shop, Some("offline-admin-token")).await;
    respond_to_graphql(&ctx, 200, json!({ "data": { "shop": { "name": "Forbidden" } } }));
    let issued = create_token(&ctx, &CORE_PERMISSIONS[..9]).await;

    let body = call_tool(
        &ctx,
        &issued.token,
        "openshop.admin.graphql",
        json!({ "shop": shop, "apiVersion": "2026-04", "query": "query { shop { name } }" }),
    )
    .await;
    assert_eq!(
        body["error"],
        json!({ "code": -32003, "message": "Permission denied for tool \"openshop.admin.graphql\"" })
    );
    assert!(graphql_requests(&ctx).is_empty());

    let recorded = audits(&ctx, &issued.token_id).await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].status, "denied");
    assert_eq!(recorded[0].permission_keys, Some(json!(["shopify_admin_graphql"])));
    assert_eq!(recorded[0].target_shop, None);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn openshop_admin_graphql_defaults_omitted_api_version_to_the_latest_stable_version() {
    let ctx = context().await;
    let shop = ctx.shop.clone();
    install(&ctx, "default", &shop, Some("offline-admin-token")).await;
    respond_to_graphql(&ctx, 200, json!({ "data": { "shop": { "name": "Latest Shop" } } }));
    let issued = create_token(&ctx, &["shopify_admin_graphql"]).await;
    let latest = latest_stable_shopify_admin_api_version(Utc::now());

    for arguments in [
        json!({ "shop": shop, "query": "query { shop { name } }" }),
        json!({ "shop": shop, "query": "query { shop { name } }", "apiVersion": "latest" }),
    ] {
        ctx.transport.clear();
        let body = call_tool(&ctx, &issued.token, "openshop.admin.graphql", arguments).await;
        let result = &body["result"]["structuredContent"];
        let version = result["apiVersion"].as_str().unwrap();
        assert_eq!(version, latest);
        assert_eq!(version.len(), 7);
        assert!(version.chars().enumerate().all(|(index, character)| match index {
            4 => character == '-',
            _ => character.is_ascii_digit(),
        }));
        assert_eq!(result["data"]["shop"]["name"], "Latest Shop");

        let requests = graphql_requests(&ctx);
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url,
            format!("https://{shop}/admin/api/{version}/graphql.json")
        );
        assert_eq!(
            header(&requests[0], "X-Shopify-Access-Token"),
            Some("offline-admin-token")
        );
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn openshop_admin_graphql_runs_against_an_installed_shop_returns_raw_shopify_json_and_audits_the_target_shop() {
    let ctx = context().await;
    install(&ctx, "default", SHOP_B, Some("offline-admin-token")).await;
    respond_to_graphql(
        &ctx,
        200,
        json!({
            "data": { "shop": { "name": "Integration Shop" } },
            "extensions": { "cost": { "requestedQueryCost": 1 } },
        }),
    );
    let issued = create_token(&ctx, &["shopify_admin_graphql"]).await;

    let query = "query Shop($first: Int) { shop { name } }";
    let body = call_tool(
        &ctx,
        &issued.token,
        "openshop.admin.graphql",
        json!({ "shop": SHOP_B, "apiVersion": "2026-04", "query": query, "variables": { "first": 1 } }),
    )
    .await;
    let expected = json!({
        "shop": SHOP_B,
        "apiVersion": "2026-04",
        "status": 200,
        "ok": true,
        "data": { "shop": { "name": "Integration Shop" } },
        "extensions": { "cost": { "requestedQueryCost": 1 } },
    });
    assert_eq!(body["result"]["structuredContent"], expected);
    assert_eq!(
        serde_json::from_str::<Value>(body["result"]["content"][0]["text"].as_str().unwrap()).unwrap(),
        expected
    );

    let requests = graphql_requests(&ctx);
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].url,
        format!("https://{SHOP_B}/admin/api/2026-04/graphql.json")
    );
    assert_eq!(
        header(&requests[0], "X-Shopify-Access-Token"),
        Some("offline-admin-token")
    );
    assert_eq!(header(&requests[0], "Content-Type"), Some("application/json"));
    assert_eq!(requests[0].body, json!({ "query": query, "variables": { "first": 1 } }));

    // The shop may be given as a URL; variables default to an empty object.
    let body = call_tool(
        &ctx,
        &issued.token,
        "openshop.admin.graphql",
        json!({ "shop": "https://MCP-Other.myshopify.com/admin", "apiVersion": "2025-10", "query": "{ shop { id } }" }),
    )
    .await;
    assert_eq!(body["result"]["structuredContent"]["shop"], SHOP_B);
    let requests = graphql_requests(&ctx);
    assert_eq!(
        requests[1].url,
        format!("https://{SHOP_B}/admin/api/2025-10/graphql.json")
    );
    assert_eq!(requests[1].body, json!({ "query": "{ shop { id } }", "variables": {} }));

    let recorded = audits(&ctx, &issued.token_id).await;
    assert_eq!(recorded.len(), 2);
    for audit in &recorded {
        assert_eq!(audit.shop, ctx.shop);
        assert_eq!(audit.target_shop.as_deref(), Some(SHOP_B));
        assert_eq!(audit.status, "success");
        assert_eq!(audit.capability_name.as_deref(), Some("openshop.admin.graphql"));
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn openshop_admin_graphql_refuses_shops_that_are_not_installed_for_the_token_app() {
    let ctx = context().await;
    install(&ctx, "default", "uninstalled.myshopify.com", Some("token-c")).await;
    uninstall(&ctx, "uninstalled.myshopify.com").await;
    install(&ctx, "default", "pending.myshopify.com", None).await;
    install(&ctx, "other", "hidden.myshopify.com", Some("token-d")).await;
    install(&ctx, "default", SHOP_B, Some("token-b")).await;
    respond_to_graphql(&ctx, 200, json!({ "data": {} }));
    let issued = create_token(&ctx, &["shopify_admin_graphql"]).await;

    for shop in [
        "missing.myshopify.com",
        "uninstalled.myshopify.com",
        "pending.myshopify.com",
        "hidden.myshopify.com",
    ] {
        let body = call_tool(
            &ctx,
            &issued.token,
            "openshop.admin.graphql",
            json!({ "shop": shop, "query": "{ shop { name } }" }),
        )
        .await;
        let message = format!("Shop \"{shop}\" is not installed for app \"default\"");
        assert_eq!(body["error"], json!({ "code": -32000, "message": message }), "{shop}");
    }

    let invalid = [
        (
            json!({ "shop": "example.com", "query": "{ shop { name } }" }),
            "shop must be a myshopify.com domain",
        ),
        (json!({ "shop": SHOP_B, "query": "   " }), "query is required"),
        (
            json!({ "shop": SHOP_B, "query": "{ shop { name } }", "apiVersion": "unstable" }),
            "apiVersion must use YYYY-MM format or \"latest\"",
        ),
    ];
    for (arguments, message) in invalid {
        let body = call_tool(&ctx, &issued.token, "openshop.admin.graphql", arguments).await;
        assert_eq!(body["error"], json!({ "code": -32000, "message": message }));
    }
    assert!(graphql_requests(&ctx).is_empty());

    // Refused calls are audited with the shop they aimed at.
    let recorded = audits(&ctx, &issued.token_id).await;
    assert_eq!(recorded.len(), 7);
    assert!(recorded.iter().all(|audit| audit.status == "error"));
    assert_eq!(recorded[0].target_shop.as_deref(), Some("missing.myshopify.com"));
    assert_eq!(
        recorded[0].error.as_deref(),
        Some("Shop \"missing.myshopify.com\" is not installed for app \"default\"")
    );
    assert_eq!(recorded[3].target_shop.as_deref(), Some("hidden.myshopify.com"));
    assert_eq!(recorded[4].target_shop, None);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn openshop_admin_graphql_reports_shopify_failures_without_hiding_them() {
    let ctx = context().await;
    install(&ctx, "default", SHOP_B, Some("token-b")).await;
    let issued = create_token(&ctx, &["shopify_admin_graphql"]).await;
    let arguments = json!({ "shop": SHOP_B, "apiVersion": "2026-04", "query": "{ shop { name } }" });

    ctx.transport.respond_with(|_| {
        Ok(openshop::runtime::OutboundResponse {
            status: 502,
            body: "Bad Gateway".to_string(),
        })
    });
    let body = call_tool(&ctx, &issued.token, "openshop.admin.graphql", arguments.clone()).await;
    assert_eq!(
        body["result"]["structuredContent"],
        json!({ "shop": SHOP_B, "apiVersion": "2026-04", "status": 502, "ok": false, "responseText": "Bad Gateway" })
    );

    // GraphQL errors are part of the raw response.
    respond_to_graphql(&ctx, 200, json!({ "errors": [{ "message": "Throttled" }] }));
    let body = call_tool(&ctx, &issued.token, "openshop.admin.graphql", arguments.clone()).await;
    let result = &body["result"]["structuredContent"];
    assert_eq!(result["ok"], true);
    assert_eq!(result["errors"], json!([{ "message": "Throttled" }]));
    assert!(result.get("responseText").is_none());

    respond_to_graphql(&ctx, 200, json!([1, 2]));
    let body = call_tool(&ctx, &issued.token, "openshop.admin.graphql", arguments.clone()).await;
    assert_eq!(body["result"]["structuredContent"]["responseText"], "[1,2]");

    ctx.transport.respond_with(|_| Err(anyhow!("network down")));
    let body = call_tool(&ctx, &issued.token, "openshop.admin.graphql", arguments).await;
    assert_eq!(body["error"], json!({ "code": -32000, "message": "network down" }));

    let statuses: Vec<String> = audits(&ctx, &issued.token_id)
        .await
        .into_iter()
        .map(|audit| audit.status)
        .collect();
    assert_eq!(statuses, ["success", "success", "success", "error"]);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn openshop_admin_graphql_decrypts_the_stored_access_token() {
    let options = TestOptions {
        encryption_key: Some(ENCRYPTION_KEY.to_string()),
        ..Default::default()
    };
    let ctx = TestContext::with_options(config_with(demo_mcp()), options)
        .await
        .unwrap();
    install(&ctx, "default", SHOP_B, Some("offline-admin-token")).await;
    let (stored,): (String,) = sqlx::query_as("SELECT access_token FROM installations WHERE shop = $1")
        .bind(SHOP_B)
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert!(stored.starts_with("enc:aes-256-gcm:"));
    respond_to_graphql(&ctx, 200, json!({ "data": { "shop": { "name": "Encrypted" } } }));
    let issued = create_token(&ctx, &["shopify_admin_graphql"]).await;

    let body = call_tool(
        &ctx,
        &issued.token,
        "openshop.admin.graphql",
        json!({ "shop": SHOP_B, "query": "{ shop { name } }" }),
    )
    .await;
    assert_eq!(body["result"]["structuredContent"]["data"]["shop"]["name"], "Encrypted");
    let requests = graphql_requests(&ctx);
    assert_eq!(
        header(&requests[0], "X-Shopify-Access-Token"),
        Some("offline-admin-token")
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn openshop_logs_search_returns_logs_for_the_token_shop_and_writes_audit() {
    let ctx = context().await;
    let shop = ctx.shop.clone();
    let run_id = logged_run(&ctx, "default", &shop).await;
    let issued = create_token(&ctx, &["read_logs"]).await;

    let body = call_tool(
        &ctx,
        &issued.token,
        "openshop.logs.search",
        json!({ "runId": run_id, "q": "warn", "limit": 10 }),
    )
    .await;
    assert!(body["result"].get("content").is_some());
    assert!(
        body["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("mcp warn")
    );
    let found = &body["result"]["structuredContent"];
    assert_eq!(found["total"], 4);
    assert_eq!(found["returned"], 1);
    let log = &found["logs"][0];
    assert_eq!(log["message"], "mcp warn");
    assert_eq!(log["level"], "warn");
    assert_eq!(log["payload"], json!({ "area": "mcp" }));
    assert_eq!(log["flowRunId"], run_id.to_string());
    assert_eq!(log["_matched"], true);
    timestamp(&log["createdAt"]);

    let recorded = audits(&ctx, &issued.token_id).await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].status, "success");
    assert_eq!(recorded[0].capability_name.as_deref(), Some("openshop.logs.search"));

    // Without a query every line of the default levels is returned.
    let body = call_tool(&ctx, &issued.token, "openshop.logs.search", json!({ "runId": run_id })).await;
    let all = &body["result"]["structuredContent"];
    assert_eq!(all["returned"], 4);
    let mut messages = strings(&all["logs"], "message");
    messages.sort();
    assert_eq!(
        messages,
        [
            "Flow \"log-flow\" completed",
            "Flow \"log-flow\" started",
            "mcp info",
            "mcp warn",
        ]
    );
    assert!(all["logs"][0].get("_matched").is_none());

    let body = call_tool(
        &ctx,
        &issued.token,
        "openshop.logs.search",
        json!({ "runId": run_id, "levels": "info", "limit": 2 }),
    )
    .await;
    let limited = &body["result"]["structuredContent"];
    assert_eq!(limited["total"], 4);
    assert_eq!(limited["returned"], 2);
    assert_eq!(strings(&limited["logs"], "level"), ["info", "info"]);

    let body = call_tool(
        &ctx,
        &issued.token,
        "openshop.logs.search",
        json!({ "runId": run_id, "q": "|= \"mcp\" != \"warn\"", "includeContext": false }),
    )
    .await;
    assert_eq!(
        strings(&body["result"]["structuredContent"]["logs"], "message"),
        ["mcp info"]
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn mcp_token_cannot_read_another_shop_run_logs() {
    let ctx = context().await;
    let other_run_id = logged_run(&ctx, "default", SHOP_B).await;
    let issued = create_token(&ctx, &["read_logs"]).await;

    let body = call_tool(
        &ctx,
        &issued.token,
        "openshop.logs.search",
        json!({ "runId": other_run_id }),
    )
    .await;
    assert_eq!(body["error"]["code"], -32000);
    assert!(body["error"]["message"].as_str().unwrap().contains("Run not found"));
    assert!(body.get("result").is_none());

    for run_id in [Uuid::new_v4().to_string(), "not-a-uuid".to_string()] {
        let body = call_tool(&ctx, &issued.token, "openshop.logs.search", json!({ "runId": run_id })).await;
        assert_eq!(body["error"], json!({ "code": -32000, "message": "Run not found" }));
    }

    let recorded = audits(&ctx, &issued.token_id).await;
    assert_eq!(recorded.len(), 3);
    assert!(recorded.iter().all(|audit| audit.status == "error"));
    assert_eq!(recorded[0].error.as_deref(), Some("Run not found"));

    // The other shop's own token does see the run.
    let owner = create_token_as(&ctx, SHOP_B, &["read_logs"], json!({})).await;
    let body = call_tool(
        &ctx,
        &owner.token,
        "openshop.logs.search",
        json!({ "runId": other_run_id }),
    )
    .await;
    assert_eq!(body["result"]["structuredContent"]["returned"], 4);
    ctx.shutdown().await.unwrap();
}

fn automation_config() -> OpenShopConfig {
    let schema = json!({
        "type": "object",
        "required": ["sku"],
        "properties": { "sku": { "type": "string" } },
    });
    OpenShopConfig::builder()
        .flow(log_flow())
        .flow(
            Flow::new("typed-flow", |_ctx: FlowContext| async move { Ok(()) })
                .input_schema(InputSchema::custom(schema, Ok)),
        )
        .cron(Cron::new("0 * * * *", "log-flow").name("hourly"))
        .cron(
            Cron::new("*/5 * * * *", "log-flow")
                .all_shops()
                .input(json!({ "source": "cron" })),
        )
        .build()
        .unwrap()
}

#[tokio::test]
async fn openshop_flows_list_describes_flows_and_their_crons() {
    let ctx = TestContext::new(automation_config()).await.unwrap();
    let issued = create_token(&ctx, &["read_flows"]).await;

    let body = call_tool(&ctx, &issued.token, "openshop.flows.list", json!({})).await;
    assert_eq!(
        body["result"]["structuredContent"],
        json!({
            "flows": [
                {
                    "name": "log-flow",
                    "inputSchema": null,
                    "crons": [
                        { "name": "hourly", "schedule": "0 * * * *", "shops": "global" },
                        { "name": null, "schedule": "*/5 * * * *", "shops": "all" },
                    ],
                },
                {
                    "name": "typed-flow",
                    "inputSchema": {
                        "type": "object",
                        "required": ["sku"],
                        "properties": { "sku": { "type": "string" } },
                    },
                    "crons": [],
                },
            ],
        })
    );

    let body = call_tool(&ctx, &issued.token, "openshop.flows.list", json!({ "extra": 1 })).await;
    assert_eq!(
        body["error"],
        json!({ "code": -32602, "message": "arguments.extra is not allowed" })
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn openshop_flows_run_dispatches_a_run_for_the_token_shop() {
    let ctx = TestContext::new(automation_config()).await.unwrap();
    let issued = create_token(&ctx, &["run_flows"]).await;

    let body = call_tool(
        &ctx,
        &issued.token,
        "openshop.flows.run",
        json!({ "flow": "log-flow", "input": { "source": "mcp" } }),
    )
    .await;
    let result = &body["result"]["structuredContent"];
    assert_eq!(result["status"], "pending");
    let run_id = Uuid::parse_str(result["runId"].as_str().unwrap()).unwrap();

    let run: (String, String, String, String, Option<Value>) =
        sqlx::query_as("SELECT app_handle, shop, flow_name, status, input FROM flow_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(&ctx.db)
            .await
            .unwrap();
    assert_eq!(
        run,
        (
            "default".to_string(),
            ctx.shop.clone(),
            "log-flow".to_string(),
            "pending".to_string(),
            Some(json!({ "source": "mcp" })),
        )
    );

    // The flow rejects concurrent runs for the same shop.
    let body = call_tool(&ctx, &issued.token, "openshop.flows.run", json!({ "flow": "log-flow" })).await;
    let message = format!(
        "Flow \"log-flow\" is already running for shop \"{}\" (run #{run_id})",
        ctx.shop
    );
    assert_eq!(body["error"], json!({ "code": -32000, "message": message }));

    let body = call_tool(&ctx, &issued.token, "openshop.flows.run", json!({ "flow": "missing" })).await;
    assert_eq!(
        body["error"],
        json!({ "code": -32000, "message": "Unknown flow \"missing\"" })
    );

    for (arguments, message) in [
        (json!({}), "arguments.flow is required"),
        (
            json!({ "flow": "typed-flow", "input": "sku-1" }),
            "arguments.input must be object",
        ),
    ] {
        let body = call_tool(&ctx, &issued.token, "openshop.flows.run", arguments).await;
        assert_eq!(body["error"], json!({ "code": -32602, "message": message }));
    }

    let (runs,): (i64,) = sqlx::query_as("SELECT count(*) FROM flow_runs")
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(runs, 1);
    let statuses: Vec<String> = audits(&ctx, &issued.token_id)
        .await
        .into_iter()
        .map(|audit| audit.status)
        .collect();
    assert_eq!(statuses, ["success", "error", "error"]);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn openshop_crons_list_and_set_enabled_are_scoped_to_the_token_shop() {
    let ctx = TestContext::new(automation_config()).await.unwrap();
    let manager = create_token(&ctx, &["read_crons", "manage_crons"]).await;
    let reader = create_token_as(&ctx, SHOP_B, &["read_crons"], json!({})).await;
    let hourly = "log-flow:0 * * * *";

    let body = call_tool(&ctx, &manager.token, "openshop.crons.list", json!({})).await;
    assert_eq!(
        body["result"]["structuredContent"],
        json!({
            "crons": [
                {
                    "index": 0,
                    "key": hourly,
                    "name": "hourly",
                    "flow": "log-flow",
                    "schedule": "0 * * * *",
                    "shops": "global",
                    "input": null,
                    "enabled": true,
                },
                {
                    "index": 1,
                    "key": "log-flow:*/5 * * * *",
                    "name": null,
                    "flow": "log-flow",
                    "schedule": "*/5 * * * *",
                    "shops": "all",
                    "input": { "source": "cron" },
                    "enabled": true,
                },
            ],
        })
    );

    let body = call_tool(
        &ctx,
        &manager.token,
        "openshop.crons.set_enabled",
        json!({ "key": hourly, "enabled": false }),
    )
    .await;
    assert_eq!(
        body["result"]["structuredContent"],
        json!({ "ok": true, "key": hourly, "enabled": false })
    );

    let enabled = |body: &Value| -> Vec<bool> {
        body["result"]["structuredContent"]["crons"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cron| cron["enabled"].as_bool().unwrap())
            .collect()
    };
    let body = call_tool(&ctx, &manager.token, "openshop.crons.list", json!({})).await;
    assert_eq!(enabled(&body), [false, true]);
    // The other shop keeps its own state and cannot change anything.
    let body = call_tool(&ctx, &reader.token, "openshop.crons.list", json!({})).await;
    assert_eq!(enabled(&body), [true, true]);
    let body = call_tool(
        &ctx,
        &reader.token,
        "openshop.crons.set_enabled",
        json!({ "key": hourly, "enabled": false }),
    )
    .await;
    assert_eq!(body["error"]["code"], -32003);

    let overrides: Vec<(String, String, String, bool)> =
        sqlx::query_as("SELECT app_handle, shop, cron_key, enabled FROM cron_overrides")
            .fetch_all(&ctx.db)
            .await
            .unwrap();
    assert_eq!(
        overrides,
        [("default".to_string(), ctx.shop.clone(), hourly.to_string(), false)]
    );

    let body = call_tool(
        &ctx,
        &manager.token,
        "openshop.crons.set_enabled",
        json!({ "key": hourly }),
    )
    .await;
    assert_eq!(
        body["error"],
        json!({ "code": -32602, "message": "arguments.enabled is required" })
    );

    // Enabling again updates the same override.
    call_tool(
        &ctx,
        &manager.token,
        "openshop.crons.set_enabled",
        json!({ "key": hourly, "enabled": true }),
    )
    .await;
    let overrides: Vec<(bool,)> = sqlx::query_as("SELECT enabled FROM cron_overrides")
        .fetch_all(&ctx.db)
        .await
        .unwrap();
    assert_eq!(overrides, [(true,)]);
    ctx.shutdown().await.unwrap();
}

// ─── Isolation between Shopify apps ──────────────────────────────────

#[tokio::test]
async fn mcp_tokens_only_reach_the_data_of_their_own_shopify_app() {
    let ctx = multi_app_context().await;
    let shop = ctx.shop.clone();
    let session_b = session_token(&shop, "secret-b", "key-b", "1");
    install(&ctx, "client-a", &shop, Some("token-a")).await;
    install(&ctx, "client-a", SHOP_B, Some("token-a2")).await;
    install(&ctx, "client-b", &shop, Some("token-b")).await;
    install(&ctx, "client-b", "only-b.myshopify.com", Some("token-b2")).await;
    respond_to_graphql(&ctx, 200, json!({ "data": { "shop": { "name": "Leaked" } } }));
    let run_a = logged_run(&ctx, "client-a", &shop).await;
    let run_b = logged_run(&ctx, "client-b", &shop).await;

    let permissions = [
        "read_logs",
        "read_flows",
        "run_flows",
        "manage_crons",
        "shopify_admin_graphql",
    ];
    let token_a = create_token(&ctx, &permissions).await;
    let token_b = create_token_with(&ctx, &session_b, &["read_crons"], json!({})).await;

    // The admin API of one app does not show the other app's tokens.
    let (_, list_a) = send(ctx.get("/api/mcp/tokens")).await;
    assert_eq!(strings(&list_a, "tokenId"), [token_a.token_id.as_str()]);
    let (_, list_b) = send(admin_with(&ctx, Method::GET, "/api/mcp/tokens", &session_b)).await;
    assert_eq!(strings(&list_b, "tokenId"), [token_b.token_id.as_str()]);
    let (status, _) = send(admin_with(&ctx, Method::GET, &token_a.path(""), &session_b)).await;
    assert_eq!(status, 404);
    let (status, _) = send(admin_with(&ctx, Method::POST, &token_a.path("/revoke"), &session_b)).await;
    assert_eq!(status, 404);

    let body = call_tool(&ctx, &token_a.token, "openshop.shops.list", json!({})).await;
    assert_eq!(
        strings(&body["result"]["structuredContent"]["shops"], "shop"),
        [SHOP_B, shop.as_str()]
    );

    // Runs of the same shop under another app are invisible.
    let body = call_tool(&ctx, &token_a.token, "openshop.logs.search", json!({ "runId": run_b })).await;
    assert_eq!(body["error"], json!({ "code": -32000, "message": "Run not found" }));
    let body = read_resource(&ctx, &token_a.token, "openshop://runs/recent").await;
    let recent: Value = serde_json::from_str(body["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(strings(&recent["runs"], "id"), [run_a.to_string()]);

    let body = call_tool(
        &ctx,
        &token_a.token,
        "openshop.flows.run",
        json!({ "flow": "log-flow" }),
    )
    .await;
    let dispatched = Uuid::parse_str(body["result"]["structuredContent"]["runId"].as_str().unwrap()).unwrap();
    let (app_handle, run_shop): (String, String) =
        sqlx::query_as("SELECT app_handle, shop FROM flow_runs WHERE id = $1")
            .bind(dispatched)
            .fetch_one(&ctx.db)
            .await
            .unwrap();
    assert_eq!((app_handle.as_str(), run_shop.as_str()), ("client-a", shop.as_str()));

    // Cron overrides belong to the app that set them.
    let hourly = "log-flow:0 * * * *";
    call_tool(
        &ctx,
        &token_a.token,
        "openshop.crons.set_enabled",
        json!({ "key": hourly, "enabled": false }),
    )
    .await;
    let body = call_tool(&ctx, &token_b.token, "openshop.crons.list", json!({})).await;
    assert_eq!(body["result"]["structuredContent"]["crons"][0]["enabled"], true);
    let overrides: Vec<(String, String)> = sqlx::query_as("SELECT app_handle, shop FROM cron_overrides")
        .fetch_all(&ctx.db)
        .await
        .unwrap();
    assert_eq!(overrides, [("client-a".to_string(), shop.clone())]);

    // A shop installed for another app only is out of reach.
    let body = call_tool(
        &ctx,
        &token_a.token,
        "openshop.admin.graphql",
        json!({ "shop": "only-b.myshopify.com", "query": "{ shop { name } }" }),
    )
    .await;
    assert_eq!(
        body["error"]["message"],
        "Shop \"only-b.myshopify.com\" is not installed for app \"client-a\""
    );
    assert!(graphql_requests(&ctx).is_empty());

    // For a shop both apps share, the token's own installation is used.
    let body = call_tool(
        &ctx,
        &token_a.token,
        "openshop.admin.graphql",
        json!({ "shop": shop, "query": "{ shop { name } }" }),
    )
    .await;
    assert_eq!(body["result"]["structuredContent"]["ok"], true);
    let requests = graphql_requests(&ctx);
    assert_eq!(requests.len(), 1);
    assert_eq!(header(&requests[0], "X-Shopify-Access-Token"), Some("token-a"));

    let recorded = audits(&ctx, &token_a.token_id).await;
    assert!(!recorded.is_empty());
    assert!(
        recorded
            .iter()
            .all(|audit| audit.app_handle == "client-a" && audit.shop == shop)
    );
    ctx.shutdown().await.unwrap();
}

// ─── Registry validation ─────────────────────────────────────────────

fn noop_tool(description: &str) -> McpTool {
    McpTool::new(description, |_ctx, _input| async { Ok(json!("ok")) })
}

fn noop_resource(name: &str) -> McpResource {
    McpResource::new(name, |_ctx| async { Ok("ok".into()) })
}

fn build_error(mcp: McpConfig) -> String {
    OpenShopConfig::builder()
        .mcp(mcp)
        .build()
        .err()
        .expect("the MCP config must be rejected")
        .to_string()
}

#[test]
fn mcp_config_is_validated_when_the_config_is_built() {
    let permission = |key: &'static str| McpPermission::new(key, "Read inventory");
    let valid = McpConfig::new()
        .permission(permission("warehouse:read_inventory"))
        .tool(
            "warehouse.inventory.list",
            noop_tool("List inventory").permissions(["warehouse:read_inventory"]),
        )
        .resource(
            "openshop://warehouse/inventory",
            noop_resource("Inventory help").permissions(["warehouse:read_inventory"]),
        );
    assert!(OpenShopConfig::builder().mcp(valid).build().is_ok());

    let cases = [
        (
            McpConfig::new().permission(permission("admin")),
            "permission \"admin\" is not allowed",
        ),
        (
            McpConfig::new().permission(permission("*")),
            "permission \"*\" is not allowed",
        ),
        (
            McpConfig::new().permission(permission("read_logs")),
            "custom permission \"read_logs\" must use namespace:action_resource",
        ),
        (
            McpConfig::new().permission(permission("Warehouse:Read")),
            "custom permission \"Warehouse:Read\" must use namespace:action_resource",
        ),
        (
            McpConfig::new().permission(McpPermission::new("warehouse:read_inventory", "  ")),
            "permissions.custom.warehouse:read_inventory.label must be a non-empty string",
        ),
        (
            McpConfig::new().tool("openshop.logs.search", noop_tool("Shadow log search")),
            "mcp.tools.openshop.logs.search conflicts with a core tool",
        ),
        (
            McpConfig::new().tool("Bad Name", noop_tool("Invalid")),
            "mcp.tools.Bad Name has an invalid tool name",
        ),
        (
            McpConfig::new().tool("warehouse.list", noop_tool(" ")),
            "mcp.tools.warehouse.list.description must be a non-empty string",
        ),
        (
            McpConfig::new().tool(
                "warehouse.list",
                noop_tool("List inventory").permissions(["warehouse:read_inventory"]),
            ),
            "mcp.tools.warehouse.list references unknown permission \"warehouse:read_inventory\"",
        ),
        (
            McpConfig::new().resource("openshop://permissions", noop_resource("Shadow permissions")),
            "mcp.resources.openshop://permissions conflicts with a core resource",
        ),
        (
            McpConfig::new().resource("not a uri", noop_resource("Invalid")),
            "mcp.resources.not a uri must be a URI",
        ),
        (
            McpConfig::new().resource("warehouse://help", noop_resource("")),
            "mcp.resources.warehouse://help.name must be a non-empty string",
        ),
        (
            McpConfig::new().resource("warehouse://help", noop_resource("Help").permissions(["nope"])),
            "mcp.resources.warehouse://help references unknown permission \"nope\"",
        ),
    ];
    for (mcp, message) in cases {
        assert_eq!(build_error(mcp), format!("[openshop] Invalid MCP config: {message}"));
    }
}
