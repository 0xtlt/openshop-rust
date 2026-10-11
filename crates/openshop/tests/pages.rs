//! Admin page visibility: `GET /api/pages` and the gate that blocks the Admin API of disabled pages.

mod common;

use common::{create_config, noop_flow};
use openshop::builder::{AdminPage, Field, Function, FunctionType, OpenShopBuilder, Provider};
use openshop::server::mcp::tokens::create_mcp_token;
use openshop::testing::TestContext;
use openshop::{AdminPageId, AdminPageMode, AdminPagesConfig, Cron, McpConfig, OpenShopConfig, UiSource, Value, json};
use reqwest::{Method, RequestBuilder, StatusCode};

const CRON_KEY: &str = "test-flow:*/5 * * * *";

// ─── Helpers ─────────────────────────────────────────────────────────

fn builder() -> OpenShopBuilder {
    OpenShopConfig::builder().flow(noop_flow("test-flow"))
}

/// A configuration with one resource behind every built-in page.
fn full_builder() -> OpenShopBuilder {
    builder()
        .provider(Provider::new("warehouse").field("token", Field::password("Token")))
        .cron(Cron::new("*/5 * * * *", "test-flow"))
        .function("testFn", Function::new(FunctionType::CartTransform, "test-fn"))
}

async fn context(builder: OpenShopBuilder) -> TestContext {
    TestContext::new(builder.build().expect("valid config"))
        .await
        .expect("test context")
}

async fn send(request: RequestBuilder) -> (StatusCode, Value) {
    let response = request.send().await.expect("response");
    let status = response.status();
    let text = response.text().await.expect("response body");
    let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
    (status, body)
}

async fn status_of(request: RequestBuilder) -> StatusCode {
    send(request).await.0
}

fn not_found() -> Value {
    json!({ "error": "Not found" })
}

/// Stores an active MCP token for the test shop and returns the bearer secret.
async fn insert_mcp_token(ctx: &TestContext, name: &str) -> String {
    let generated = create_mcp_token();
    sqlx::query(
        "INSERT INTO mcp_tokens (app_handle, shop, name, token_id, token_hash, token_fingerprint) \
         VALUES ('default', $1, $2, $3, $4, $5)",
    )
    .bind(&ctx.shop)
    .bind(name)
    .bind(&generated.token_id)
    .bind(&generated.token_hash)
    .bind(&generated.token_fingerprint)
    .execute(&ctx.db)
    .await
    .unwrap();
    generated.token
}

fn mcp_initialize(ctx: &TestContext, token: &str) -> RequestBuilder {
    ctx.http
        .post(format!("{}/mcp", ctx.url))
        .bearer_auth(token)
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }))
}

async fn count(ctx: &TestContext, table: &str) -> i64 {
    let (rows,): (i64,) = sqlx::query_as(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    rows
}

// ─── GET /api/pages ──────────────────────────────────────────────────

#[openshop::test(context(builder()))]
async fn get_pages_hides_config_backed_pages_without_resources(ctx: &TestContext) {
    let (status, pages) = send(ctx.get("/api/pages")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        pages,
        json!({
            "flows": "visible",
            "providers": "hidden",
            "crons": "hidden",
            "functions": "hidden",
            "mcp": "visible",
        })
    );
}

#[tokio::test]
async fn get_pages_shows_pages_backed_by_resources() {
    let ctx = context(full_builder()).await;
    let (status, pages) = send(ctx.get("/api/pages")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        pages,
        json!({
            "flows": "visible",
            "providers": "visible",
            "crons": "visible",
            "functions": "visible",
            "mcp": "visible",
        })
    );
    ctx.shutdown().await.unwrap();

    // Explicit modes win over the resource-based default, and a flow-less app hides Flows.
    let config = OpenShopConfig::builder()
        .provider(Provider::new("warehouse").field("token", Field::password("Token")))
        .builtin_page(AdminPageId::Providers, AdminPageMode::Hidden)
        .builtin_page(AdminPageId::Crons, AdminPageMode::Visible)
        .builtin_page(AdminPageId::Functions, AdminPageMode::Disabled);
    let ctx = context(config).await;
    let (_, pages) = send(ctx.get("/api/pages")).await;
    assert_eq!(
        pages,
        json!({
            "flows": "hidden",
            "providers": "hidden",
            "crons": "hidden",
            "functions": "disabled",
            "mcp": "visible",
        })
    );
    ctx.shutdown().await.unwrap();
}

#[openshop::test(TestContext::new(create_config([noop_flow("test-flow")])))]
async fn get_pages_requires_an_authenticated_shopify_session(ctx: &TestContext) {
    let (status, body) = send(ctx.http.get(format!("{}/api/pages", ctx.url))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, json!({ "error": "Unauthorized: missing session token" }));
}

// ─── Page gate ───────────────────────────────────────────────────────

#[tokio::test]
async fn hidden_pages_remain_reachable_on_the_admin_api() {
    let ctx = context(
        builder()
            .builtin_page(AdminPageId::Functions, AdminPageMode::Hidden)
            .builtin_page(AdminPageId::Mcp, AdminPageMode::Hidden),
    )
    .await;

    let (status, pages) = send(ctx.get("/api/pages")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        pages,
        json!({
            "flows": "visible",
            "providers": "hidden",
            "crons": "hidden",
            "functions": "hidden",
            "mcp": "hidden",
        })
    );

    for path in [
        "/api/functions",
        "/api/mcp/capabilities",
        "/api/flows",
        // Hidden only because the app defines no provider or cron.
        "/api/providers",
        "/api/crons",
    ] {
        assert_eq!(status_of(ctx.get(path)).await, StatusCode::OK, "{path}");
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn disabled_pages_return_404_on_their_admin_api_group() {
    let ctx = context(
        builder()
            .builtin_page(AdminPageId::Flows, AdminPageMode::Disabled)
            .builtin_page(AdminPageId::Functions, AdminPageMode::Disabled)
            .builtin_page(AdminPageId::Mcp, AdminPageMode::Disabled),
    )
    .await;

    let (status, pages) = send(ctx.get("/api/pages")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(pages["flows"], "disabled");
    assert_eq!(pages["functions"], "disabled");
    assert_eq!(pages["mcp"], "disabled");

    for path in [
        "/api/flows",
        "/api/flows/test-flow/runs",
        "/api/runs?limit=10",
        "/api/runs/00000000-0000-0000-0000-000000000000",
        "/api/functions",
        "/api/mcp/capabilities",
        "/api/mcp/tokens",
        // Paths without a route are blocked the same way.
        "/api/flows/",
        "/api/runs/a/b/c",
        "/api/mcp",
    ] {
        let (status, body) = send(ctx.get(path)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(body, not_found(), "{path}");
    }

    // Percent-encoded section names cannot be used to slip past the gate.
    for path in [
        "/api/%66lows",
        "/api/%72uns?limit=10",
        "/api/%66unctions",
        "/api/%6dcp/capabilities",
        "/api/%2e/flows",
        "/api//flows",
    ] {
        assert_eq!(status_of(ctx.get(path)).await, StatusCode::NOT_FOUND, "{path}");
    }

    assert_eq!(status_of(ctx.get("/api/providers")).await, StatusCode::OK);
    assert_eq!(status_of(ctx.get("/api/crons")).await, StatusCode::OK);
    ctx.shutdown().await.unwrap();
}

#[openshop::test(context(builder().builtin_page(AdminPageId::Flows, AdminPageMode::Disabled)))]
async fn disabled_flows_page_blocks_run_mutations(ctx: &TestContext) {
    let run_id = ctx.dispatch_flow("test-flow", json!({})).await.unwrap();

    let requests = [
        ctx.post("/api/flows/test-flow/run").json(&json!({ "input": {} })),
        ctx.post("/api/flows/other-flow/run"),
        ctx.post(&format!("/api/runs/{run_id}/cancel")),
        ctx.post(&format!("/api/runs/{run_id}/retry")),
        ctx.delete(&format!("/api/runs/{run_id}")),
        ctx.post("/api/runs/bulk-delete").json(&json!({ "ids": [run_id] })),
        ctx.get(&format!("/api/runs/{run_id}/logs")),
        ctx.get(&format!("/api/runs/{run_id}/logs/export?format=csv")),
        ctx.request(Method::PUT, "/api/flows"),
    ];
    for request in requests {
        let (status, body) = send(request).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, not_found());
    }

    let (status,): (String,) = sqlx::query_as("SELECT status FROM flow_runs WHERE id = $1")
        .bind(run_id)
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(status, "pending");
    assert_eq!(count(ctx, "flow_runs").await, 1);
}

#[tokio::test]
async fn disabled_providers_and_crons_pages_block_reads_and_writes() {
    let ctx = context(
        full_builder()
            .builtin_page(AdminPageId::Providers, AdminPageMode::Disabled)
            .builtin_page(AdminPageId::Crons, AdminPageMode::Disabled),
    )
    .await;

    let (_, pages) = send(ctx.get("/api/pages")).await;
    assert_eq!(
        pages,
        json!({
            "flows": "visible",
            "providers": "disabled",
            "crons": "disabled",
            "functions": "visible",
            "mcp": "visible",
        })
    );

    let requests = [
        ctx.get("/api/providers"),
        ctx.put("/api/providers/warehouse")
            .json(&json!({ "config": { "token": "secret" } })),
        ctx.post("/api/providers/warehouse/check"),
        ctx.get("/api/crons"),
        ctx.post("/api/crons/toggle")
            .json(&json!({ "key": CRON_KEY, "enabled": false })),
    ];
    for request in requests {
        let (status, body) = send(request).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, not_found());
    }
    assert_eq!(count(&ctx, "provider_configs").await, 0);
    assert_eq!(count(&ctx, "cron_overrides").await, 0);

    for path in ["/api/flows", "/api/runs", "/api/functions", "/api/mcp/capabilities"] {
        assert_eq!(status_of(ctx.get(path)).await, StatusCode::OK, "{path}");
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn disabled_pages_do_not_reveal_themselves_without_a_session() {
    let ctx = context(
        full_builder()
            .builtin_page(AdminPageId::Flows, AdminPageMode::Disabled)
            .builtin_page(AdminPageId::Providers, AdminPageMode::Disabled)
            .builtin_page(AdminPageId::Crons, AdminPageMode::Disabled)
            .builtin_page(AdminPageId::Functions, AdminPageMode::Disabled)
            .builtin_page(AdminPageId::Mcp, AdminPageMode::Disabled)
            // Without a built-in page, the admin opens on a page of the application.
            .admin_page(AdminPage::new("/")),
    )
    .await;

    for path in [
        "/api/flows",
        "/api/runs",
        "/api/providers",
        "/api/crons",
        "/api/functions",
        "/api/mcp/tokens",
    ] {
        let anonymous = ctx.http.get(format!("{}{path}", ctx.url));
        assert_eq!(status_of(anonymous).await, StatusCode::UNAUTHORIZED, "{path}");

        let (status, body) = send(ctx.get(path)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(body, not_found(), "{path}");
    }

    // Page resolution itself is never gated.
    let (status, pages) = send(ctx.get("/api/pages")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(pages.as_object().unwrap().values().all(|mode| mode == "disabled"));
    let (status, custom) = send(ctx.get("/api/pages/custom")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(custom.is_object());
    ctx.shutdown().await.unwrap();
}

#[test]
fn an_admin_without_built_in_pages_opens_on_a_page_of_the_application() {
    let closed = || full_builder().builtin_pages(AdminPagesConfig::none());
    let error = closed().build().err().unwrap().to_string();
    assert_eq!(
        error,
        "[openshop] Invalid config: builtinPages disables every built-in page of the admin: \
         declare its home, a page at `/` (`#[admin_page(\"/\")]`)"
    );

    assert!(closed().admin_page(AdminPage::new("/")).build().is_ok());
    // One page opened again: the built-in Home has something to show.
    assert!(
        closed()
            .builtin_page(AdminPageId::Providers, AdminPageMode::Visible)
            .build()
            .is_ok()
    );
    // Without an admin UI there is no home to open.
    assert!(closed().admin_ui(UiSource::None).build().is_ok());
}

// ─── MCP protocol ────────────────────────────────────────────────────

#[openshop::test(context(builder().builtin_page(AdminPageId::Mcp, AdminPageMode::Disabled)))]
async fn disabling_the_mcp_admin_page_does_not_disable_the_mcp_protocol(ctx: &TestContext) {
    let token = insert_mcp_token(ctx, "protocol-only token").await;

    let (status, body) = send(ctx.get("/api/mcp/capabilities")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, not_found());

    let (status, body) = send(mcp_initialize(ctx, &token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["jsonrpc"], "2.0");
    assert_eq!(body["id"], 1);
    assert!(body["result"].is_object(), "{body}");
}

#[openshop::test(context(builder().mcp(McpConfig::disabled())))]
async fn mcp_enabled_false_rejects_mcp_protocol_requests(ctx: &TestContext) {
    let token = insert_mcp_token(ctx, "disabled protocol token").await;

    let (status, body) = send(mcp_initialize(ctx, &token)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body, json!({ "error": "MCP is disabled" }));

    // The admin page follows: there is nothing to manage.
    let (_, pages) = send(ctx.get("/api/pages")).await;
    assert_eq!(pages["mcp"], "disabled");
    assert_eq!(status_of(ctx.get("/api/mcp/capabilities")).await, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_mcp_page_stays_when_the_application_asks_for_it() {
    let ctx = context(
        builder()
            .mcp(McpConfig::disabled())
            .builtin_page(AdminPageId::Mcp, AdminPageMode::Visible),
    )
    .await;

    let (_, pages) = send(ctx.get("/api/pages")).await;
    assert_eq!(pages["mcp"], "visible");
    assert_eq!(status_of(ctx.get("/api/mcp/capabilities")).await, StatusCode::OK);
    ctx.shutdown().await.unwrap();
}
