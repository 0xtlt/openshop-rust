//! The Admin API: flows, runs, logs, crons and providers, scoped to the session's shop and app.

mod common;

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, SubsecRound, Utc};
use common::{create_config, noop_flow};
use openshop::builder::{Field, Flow, Function, FunctionType, Provider};
use openshop::db::schema::FlowRun;
use openshop::engine::{RunFlowOptions, run_flow};
use openshop::runtime::time::to_iso;
use openshop::server::ServerOptions;
use openshop::testing::{TestContext, TestOptions, session_token, sign_session_token};
use openshop::{
    Concurrency, Cron, DispatchOptions, FlowCanceledError, FlowContext, IndexMap, OpenShopConfig, PgPool, RetryPolicy,
    RunOutcome, ShopifyAppConfig, ShopifyConfig, Value, anyhow, json, validators,
};
use reqwest::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    ACCESS_CONTROL_REQUEST_HEADERS, ACCESS_CONTROL_REQUEST_METHOD, CONTENT_DISPOSITION, CONTENT_TYPE, ORIGIN,
};
use reqwest::{Method, RequestBuilder, StatusCode};
use uuid::Uuid;

const OTHER_SHOP: &str = "other-shop.myshopify.com";
const ENCRYPTION_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const NIL_RUN: &str = "00000000-0000-0000-0000-000000000000";

// ─── Helpers ─────────────────────────────────────────────────────────

fn warehouse() -> Provider {
    Provider::new("warehouse")
        .field("endpoint", Field::text("Endpoint"))
        .field("apiKey", Field::password("API key"))
        .field("optionalNote", Field::text("Optional note").optional())
        .checker(|config| async move {
            Ok(
                config.get("endpoint").and_then(Value::as_str) == Some("https://warehouse.test")
                    && config.get("apiKey").and_then(Value::as_str) == Some("secret-1"),
            )
        })
}

fn api_config_with(flow: Flow) -> OpenShopConfig {
    OpenShopConfig::builder()
        .flow(flow)
        .provider(warehouse())
        .cron(Cron::new("*/5 * * * *", "test-flow").name("Test cron"))
        .build()
        .expect("valid config")
}

fn api_config() -> OpenShopConfig {
    api_config_with(noop_flow("test-flow"))
}

async fn context(config: OpenShopConfig) -> TestContext {
    TestContext::new(config).await.expect("test context")
}

async fn api_context() -> TestContext {
    context(api_config()).await
}

/// Status and body of a response; non-JSON bodies are returned as a string.
async fn send(request: RequestBuilder) -> (StatusCode, Value) {
    let response = request.send().await.expect("response");
    let status = response.status();
    let text = response.text().await.expect("response body");
    let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
    (status, body)
}

/// A request authenticated with an arbitrary session token.
fn request_as(ctx: &TestContext, token: &str, method: Method, path: &str) -> RequestBuilder {
    ctx.http
        .request(method, format!("{}{path}", ctx.url))
        .bearer_auth(token)
}

fn anonymous(ctx: &TestContext, method: Method, path: &str) -> RequestBuilder {
    ctx.http.request(method, format!("{}{path}", ctx.url))
}

fn error(message: &str) -> Value {
    json!({ "error": message })
}

/// A whole-second instant in the recent past, so that ISO strings round-trip exactly.
fn base_time() -> DateTime<Utc> {
    (Utc::now() - chrono::Duration::hours(1)).trunc_subsecs(0)
}

fn assert_iso_millis(value: &Value) {
    let text = value
        .as_str()
        .unwrap_or_else(|| panic!("expected a timestamp, got {value}"));
    let parsed = DateTime::parse_from_rfc3339(text).unwrap_or_else(|_| panic!("not ISO-8601: {text}"));
    assert_eq!(
        to_iso(&parsed.with_timezone(&Utc)),
        text,
        "expected UTC with millisecond precision"
    );
}

async fn seed_run_for(
    db: &PgPool,
    app_handle: &str,
    shop: &str,
    flow_name: &str,
    status: &str,
    created_at: DateTime<Utc>,
) -> Uuid {
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO flow_runs (app_handle, shop, flow_name, status, input, available_at, created_at) \
         VALUES ($1, $2, $3, $4, '{}'::json, $5, $5) RETURNING id",
    )
    .bind(app_handle)
    .bind(shop)
    .bind(flow_name)
    .bind(status)
    .bind(created_at)
    .fetch_one(db)
    .await
    .unwrap();
    id
}

async fn seed_run(ctx: &TestContext, flow_name: &str, status: &str, created_at: DateTime<Utc>) -> Uuid {
    seed_run_for(&ctx.db, "default", &ctx.shop, flow_name, status, created_at).await
}

async fn seed_step(db: &PgPool, run_id: Uuid, name: &str, status: &str, created_at: DateTime<Utc>) {
    sqlx::query(
        "INSERT INTO step_results (flow_run_id, step_name, status, output, created_at) \
         VALUES ($1, $2, $3, 'null'::json, $4)",
    )
    .bind(run_id.to_string())
    .bind(name)
    .bind(status)
    .bind(created_at)
    .execute(db)
    .await
    .unwrap();
}

async fn seed_log(
    db: &PgPool,
    run_id: Uuid,
    level: &str,
    message: &str,
    payload: Value,
    created_at: DateTime<Utc>,
) -> Uuid {
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO logs (flow_run_id, level, message, payload, created_at) \
         VALUES ($1, $2, $3, $4::json, $5) RETURNING id",
    )
    .bind(run_id.to_string())
    .bind(level)
    .bind(message)
    .bind(payload.to_string())
    .bind(created_at)
    .fetch_one(db)
    .await
    .unwrap();
    id
}

async fn find_run(db: &PgPool, run_id: Uuid) -> Option<FlowRun> {
    sqlx::query_as("SELECT * FROM flow_runs WHERE id = $1")
        .bind(run_id)
        .fetch_optional(db)
        .await
        .unwrap()
}

async fn run_status(db: &PgPool, run_id: Uuid) -> String {
    find_run(db, run_id).await.expect("run").status
}

async fn step_names(db: &PgPool, run_id: Uuid) -> Vec<String> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT step_name FROM step_results WHERE flow_run_id = $1 ORDER BY created_at")
            .bind(run_id.to_string())
            .fetch_all(db)
            .await
            .unwrap();
    rows.into_iter().map(|(name,)| name).collect()
}

async fn log_messages(db: &PgPool, run_id: Uuid) -> Vec<String> {
    let rows: Vec<(Option<String>,)> =
        sqlx::query_as("SELECT message FROM logs WHERE flow_run_id = $1 ORDER BY created_at")
            .bind(run_id.to_string())
            .fetch_all(db)
            .await
            .unwrap();
    rows.into_iter().filter_map(|(message,)| message).collect()
}

/// Dispatches a run that fails for good on its first failure.
async fn dispatch_without_retries(ctx: &TestContext, flow_name: &str) -> Uuid {
    ctx.app
        .dispatch_flow(
            DispatchOptions::new(flow_name, ctx.shop.clone()).retry_policy(RetryPolicy::new().max_attempts(0)),
        )
        .await
        .unwrap()
        .run_id
}

async fn execute(ctx: &TestContext, run_id: Uuid, flow_name: &str) -> RunOutcome {
    run_flow(&ctx.app, RunFlowOptions::new(run_id, flow_name, ctx.shop.clone()))
        .await
        .unwrap()
}

fn strings(values: &Value, key: &str) -> Vec<String> {
    values
        .as_array()
        .expect("array")
        .iter()
        .map(|value| value[key].as_str().unwrap_or_default().to_string())
        .collect()
}

// ─── Flows ───────────────────────────────────────────────────────────

#[openshop::test(api_context())]
async fn get_flows_returns_flow_list(ctx: &TestContext) {
    let (status, data) = send(ctx.get("/api/flows")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        data,
        json!([{
            "name": "test-flow",
            "crons": [{ "name": "Test cron", "schedule": "*/5 * * * *", "flow": "test-flow" }],
            "inputSchema": null,
        }])
    );
}

#[cfg(feature = "schema")]
#[derive(serde::Deserialize, schemars::JsonSchema)]
struct SyncInput {
    since: String,
    limit: Option<u32>,
}

#[cfg(feature = "schema")]
#[tokio::test]
async fn get_flows_exposes_the_generated_input_schema() {
    let typed = Flow::new("typed-flow", |ctx: FlowContext| async move {
        let input: SyncInput = ctx.input()?;
        ctx.logger
            .info_with("Syncing", json!({ "since": input.since, "limit": input.limit }));
        Ok(())
    })
    .input::<SyncInput>();
    let config = OpenShopConfig::builder()
        .flow(typed)
        .flow(noop_flow("plain-flow"))
        .cron(
            Cron::new("0 * * * *", "plain-flow")
                .input(json!({ "full": true }))
                .all_shops(),
        )
        .build()
        .unwrap();
    let ctx = context(config).await;

    let (status, data) = send(ctx.get("/api/flows")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(strings(&data, "name"), ["typed-flow", "plain-flow"]);

    let schema = &data[0]["inputSchema"];
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["properties"]["since"]["type"], "string");
    assert_eq!(schema["required"], json!(["since"]));
    assert_eq!(data[0]["crons"], json!([]));

    assert_eq!(data[1]["inputSchema"], Value::Null);
    assert_eq!(
        data[1]["crons"],
        json!([{ "schedule": "0 * * * *", "flow": "plain-flow", "input": { "full": true }, "shops": "all" }])
    );
    ctx.shutdown().await.unwrap();
}

// ─── Crons ───────────────────────────────────────────────────────────

#[openshop::test(api_context())]
async fn get_crons_returns_cron_list(ctx: &TestContext) {
    let (status, data) = send(ctx.get("/api/crons")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        data,
        json!([{
            "index": 0,
            "key": "test-flow:*/5 * * * *",
            "name": "Test cron",
            "flow": "test-flow",
            "schedule": "*/5 * * * *",
            "input": null,
            "shops": "global",
            "enabled": true,
        }])
    );
}

#[openshop::test(api_context())]
async fn post_crons_toggle_disables_a_cron(ctx: &TestContext) {
    let key = "test-flow:*/5 * * * *";

    let (status, body) = send(
        ctx.post("/api/crons/toggle")
            .json(&json!({ "key": key, "enabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "ok": true, "key": key, "enabled": false }));

    let (_, data) = send(ctx.get("/api/crons")).await;
    assert_eq!(data[0]["enabled"], false);

    let (status, _) = send(
        ctx.post("/api/crons/toggle")
            .json(&json!({ "key": key, "enabled": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, data) = send(ctx.get("/api/crons")).await;
    assert_eq!(data[0]["enabled"], true);

    let (overrides,): (i64,) = sqlx::query_as("SELECT count(*) FROM cron_overrides")
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(overrides, 1);
}

#[openshop::test(api_context())]
async fn post_crons_toggle_validates_its_body(ctx: &TestContext) {
    for body in [
        json!({}),
        json!({ "key": "test-flow:*/5 * * * *" }),
        json!({ "enabled": false }),
        json!({ "key": "", "enabled": false }),
        json!({ "key": "test-flow:*/5 * * * *", "enabled": "no" }),
        json!(["test-flow:*/5 * * * *", false]),
    ] {
        let (status, response) = send(ctx.post("/api/crons/toggle").json(&body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(response, error("key and enabled are required"));
    }

    let (status, response) = send(ctx.post("/api/crons/toggle").body("not json")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(response, error("key and enabled are required"));

    let (_, data) = send(ctx.get("/api/crons")).await;
    assert_eq!(data[0]["enabled"], true);
}

// ─── Runs ────────────────────────────────────────────────────────────

#[openshop::test(api_context())]
async fn get_runs_returns_all_runs(ctx: &TestContext) {
    let run_id = ctx
        .dispatch_flow("test-flow", json!({ "source": "test" }))
        .await
        .unwrap();

    let (status, data) = send(ctx.get("/api/runs?limit=10")).await;
    assert_eq!(status, StatusCode::OK);
    let runs = data.as_array().expect("array");
    assert_eq!(runs.len(), 1);

    let run = &runs[0];
    assert_eq!(run["id"], run_id.to_string());
    assert_eq!(run["appHandle"], "default");
    assert_eq!(run["shop"], ctx.shop);
    assert_eq!(run["flowName"], "test-flow");
    assert_eq!(run["status"], "pending");
    assert_eq!(run["input"], json!({ "source": "test" }));
    assert_eq!(run["attempts"], 0);
    for key in [
        "error",
        "deadlineAt",
        "parentRunId",
        "workerId",
        "startedAt",
        "completedAt",
    ] {
        assert_eq!(run[key], Value::Null, "{key}");
    }
    assert_iso_millis(&run["createdAt"]);
    assert_iso_millis(&run["availableAt"]);
    assert!(run["retryPolicy"].is_object());
    assert!(run.get("flow_name").is_none());
}

#[openshop::test(api_context())]
async fn get_runs_supports_search(ctx: &TestContext) {
    let run_id = ctx.dispatch_flow("test-flow", json!({})).await.unwrap();
    let id_fragment = &run_id.to_string()[..8];

    for search in ["test-flow", "TEST-FL", "pend", id_fragment] {
        let (status, data) = send(ctx.get("/api/runs").query(&[("search", search)])).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(strings(&data, "id"), [run_id.to_string()], "search={search}");
    }

    let (status, data) = send(ctx.get("/api/runs?search=nonexistent")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data, json!([]));
}

#[openshop::test(api_context())]
async fn get_flow_runs_returns_filtered_runs(ctx: &TestContext) {
    let base = base_time();
    let own = seed_run(ctx, "test-flow", "completed", base).await;
    let other = seed_run(ctx, "other-flow", "completed", base).await;

    let (status, data) = send(ctx.get("/api/flows/test-flow/runs?limit=10")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(strings(&data, "id"), [own.to_string()]);
    assert_eq!(data[0]["flowName"], "test-flow");

    let (_, data) = send(ctx.get("/api/flows/other-flow/runs")).await;
    assert_eq!(strings(&data, "id"), [other.to_string()]);

    let (status, data) = send(ctx.get("/api/flows/missing/runs")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data, json!([]));
}

#[openshop::test(api_context())]
async fn get_runs_filters_by_status_and_date_range(ctx: &TestContext) {
    let base = base_time();
    let hours_ago = |hours: i64| base - chrono::Duration::hours(hours);
    let completed = seed_run(ctx, "test-flow", "completed", hours_ago(3)).await.to_string();
    let failed = seed_run(ctx, "test-flow", "failed", hours_ago(2)).await.to_string();
    let canceled = seed_run(ctx, "other-flow", "canceled", hours_ago(1)).await.to_string();
    let pending = seed_run(ctx, "test-flow", "pending", base).await.to_string();
    let (completed, failed, canceled, pending) =
        (completed.as_str(), failed.as_str(), canceled.as_str(), pending.as_str());
    let everything = [pending, canceled, failed, completed];

    let ids = |query: Vec<(&'static str, String)>| {
        let request = ctx.get("/api/runs").query(&query);
        async move {
            let (status, data) = send(request).await;
            assert_eq!(status, StatusCode::OK);
            strings(&data, "id")
        }
    };

    assert_eq!(ids(vec![("status", "failed".into())]).await, [failed]);
    assert_eq!(ids(vec![("status", "bogus".into())]).await, everything);
    assert_eq!(
        ids(vec![("from", to_iso(&hours_ago(2)))]).await,
        [pending, canceled, failed]
    );
    assert_eq!(ids(vec![("to", to_iso(&hours_ago(2)))]).await, [failed, completed]);
    assert_eq!(
        ids(vec![("from", to_iso(&hours_ago(2))), ("to", to_iso(&hours_ago(1)))]).await,
        [canceled, failed]
    );
    assert_eq!(
        ids(vec![("from", "nope".into()), ("to", "nope".into())]).await,
        everything
    );
    assert_eq!(
        ids(vec![("search", "test-flow".into()), ("status", "pending".into())]).await,
        [pending]
    );

    let (_, data) = send(ctx.get("/api/flows/test-flow/runs?status=completed")).await;
    assert_eq!(strings(&data, "id"), [completed]);
}

#[openshop::test(api_context())]
async fn get_runs_paginates_newest_first_with_clamped_limits(ctx: &TestContext) {
    let base = base_time();
    sqlx::query(
        "INSERT INTO flow_runs (shop, flow_name, status, input, available_at, created_at) \
         SELECT $1, 'test-flow', 'completed', '{}'::json, $2, $2 - make_interval(mins => n) \
         FROM generate_series(0, 204) AS n",
    )
    .bind(&ctx.shop)
    .bind(base)
    .execute(&ctx.db)
    .await
    .unwrap();
    let minutes_ago = |minutes: i64| to_iso(&(base - chrono::Duration::minutes(minutes)));

    let created_at = |path: String| {
        let request = ctx.get(&path);
        async move {
            let (status, data) = send(request).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            strings(&data, "createdAt")
        }
    };
    let created = |query: &str| created_at(format!("/api/runs{query}"));

    let default_page = created("").await;
    assert_eq!(default_page.len(), 50);
    assert_eq!(default_page[0], minutes_ago(0));
    assert_eq!(default_page[49], minutes_ago(49));
    assert!(default_page.windows(2).all(|pair| pair[0] > pair[1]));

    assert_eq!(
        created("?limit=3").await,
        [minutes_ago(0), minutes_ago(1), minutes_ago(2)]
    );
    assert_eq!(
        created("?limit=3&offset=3").await,
        [minutes_ago(3), minutes_ago(4), minutes_ago(5)]
    );
    assert_eq!(created("?limit=1000").await.len(), 200);
    assert_eq!(created("?limit=0").await.len(), 50);
    assert_eq!(created("?limit=abc").await.len(), 50);
    assert_eq!(created("?limit=-5").await, [minutes_ago(0)]);
    assert_eq!(created("?limit=1&offset=-5").await, [minutes_ago(0)]);
    assert_eq!(created("?limit=10&offset=204").await, [minutes_ago(204)]);
    assert!(created("?offset=500").await.is_empty());

    assert_eq!(
        created_at("/api/flows/test-flow/runs?limit=2&offset=1".to_string()).await,
        [minutes_ago(1), minutes_ago(2)]
    );
}

// ─── Run detail ──────────────────────────────────────────────────────

#[tokio::test]
async fn get_run_returns_run_with_steps() {
    let flow = Flow::new("test-flow", |ctx: FlowContext| async move {
        let first: i64 = ctx.step.run("first", || async { Ok(1) }).await?;
        let _second: String = ctx
            .step
            .run("second", || async move { Ok(format!("value-{first}")) })
            .await?;
        Ok(())
    });
    let ctx = context(api_config_with(flow)).await;
    let run_id = ctx.dispatch_flow("test-flow", json!({ "order": 7 })).await.unwrap();
    assert_eq!(execute(&ctx, run_id, "test-flow").await, RunOutcome::Completed);

    let (status, data) = send(ctx.get(&format!("/api/runs/{run_id}"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data["id"], run_id.to_string());
    assert_eq!(data["status"], "completed");
    assert_eq!(data["flowName"], "test-flow");
    assert_eq!(data["input"], json!({ "order": 7 }));
    assert_iso_millis(&data["createdAt"]);
    assert_iso_millis(&data["completedAt"]);

    let steps = data["steps"].as_array().expect("steps");
    assert_eq!(strings(&data["steps"], "stepName"), ["first", "second"]);
    assert_eq!(steps[0]["output"], 1);
    assert_eq!(steps[1]["output"], "value-1");
    for step in steps {
        assert_eq!(step["status"], "completed");
        assert_eq!(step["flowRunId"], run_id.to_string());
        assert_eq!(step["error"], Value::Null);
        assert!(step["attempt"].is_i64());
        assert!(step["durationMs"].is_i64());
        assert_iso_millis(&step["createdAt"]);
    }
    ctx.shutdown().await.unwrap();
}

#[openshop::test(api_context())]
async fn get_run_orders_steps_by_creation_time(ctx: &TestContext) {
    let base = base_time();
    let seconds = chrono::Duration::seconds;
    let run_id = seed_run(ctx, "test-flow", "completed", base).await;
    let unrelated = seed_run(ctx, "test-flow", "completed", base).await;
    seed_step(&ctx.db, run_id, "third", "completed", base + seconds(3)).await;
    seed_step(&ctx.db, run_id, "first", "completed", base + seconds(1)).await;
    seed_step(&ctx.db, run_id, "second", "failed", base + seconds(2)).await;
    seed_step(&ctx.db, unrelated, "elsewhere", "completed", base).await;

    let (status, data) = send(ctx.get(&format!("/api/runs/{run_id}"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(strings(&data["steps"], "stepName"), ["first", "second", "third"]);
    assert_eq!(data["steps"][0]["createdAt"], to_iso(&(base + seconds(1))));

    let (_, data) = send(ctx.get(&format!("/api/runs/{unrelated}"))).await;
    assert_eq!(strings(&data["steps"], "stepName"), ["elsewhere"]);
}

#[openshop::test(api_context())]
async fn get_run_returns_404_for_unknown(ctx: &TestContext) {
    for id in [NIL_RUN, "not-a-uuid"] {
        for (method, path) in [
            (Method::GET, format!("/api/runs/{id}")),
            (Method::GET, format!("/api/runs/{id}/logs")),
            (Method::GET, format!("/api/runs/{id}/logs/export")),
            (Method::POST, format!("/api/runs/{id}/retry")),
            (Method::POST, format!("/api/runs/{id}/cancel")),
            (Method::DELETE, format!("/api/runs/{id}")),
        ] {
            let (status, body) = send(ctx.request(method.clone(), &path)).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
            assert_eq!(body, error("Run not found"), "{method} {path}");
        }
    }
}

// ─── Trigger flow ────────────────────────────────────────────────────

#[openshop::test(api_context())]
async fn post_flow_run_triggers_a_flow(ctx: &TestContext) {
    let (status, data) = send(
        ctx.post("/api/flows/test-flow/run")
            .json(&json!({ "input": { "source": "admin" } })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(data["status"], "pending");
    let run_id = Uuid::parse_str(data["runId"].as_str().expect("runId")).unwrap();

    let run = find_run(&ctx.db, run_id).await.expect("run");
    assert_eq!(run.status, "pending");
    assert_eq!(run.flow_name, "test-flow");
    assert_eq!(run.shop, ctx.shop);
    assert_eq!(run.app_handle, "default");
    assert_eq!(run.input, Some(json!({ "source": "admin" })));
}

#[openshop::test(api_context())]
async fn post_flow_run_rejects_unknown_flow(ctx: &TestContext) {
    let (status, body) = send(ctx.post("/api/flows/nope/run").json(&json!({ "input": {} }))).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body, "Internal Server Error");

    let (runs,): (i64,) = sqlx::query_as("SELECT count(*) FROM flow_runs")
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(runs, 0);
}

#[openshop::test(context(api_config_with(noop_flow("test-flow").concurrency(Concurrency::Allow))))]
async fn post_flow_run_defaults_input_when_the_body_is_missing_or_invalid(ctx: &TestContext) {
    let requests = [
        ctx.post("/api/flows/test-flow/run"),
        ctx.post("/api/flows/test-flow/run").body("not json"),
        ctx.post("/api/flows/test-flow/run").json(&json!({ "input": null })),
        ctx.post("/api/flows/test-flow/run").json(&json!({ "other": 1 })),
    ];
    for request in requests {
        let (status, data) = send(request).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let run_id = Uuid::parse_str(data["runId"].as_str().expect("runId")).unwrap();
        assert_eq!(find_run(&ctx.db, run_id).await.expect("run").input, Some(json!({})));
    }
}

#[tokio::test]
async fn flow_names_are_decoded_from_the_path() {
    let name = "orders/sync now";
    let ctx = context(create_config([noop_flow(name)])).await;

    let (status, data) = send(ctx.post("/api/flows/orders%2Fsync%20now/run")).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let run_id = Uuid::parse_str(data["runId"].as_str().expect("runId")).unwrap();
    assert_eq!(find_run(&ctx.db, run_id).await.expect("run").flow_name, name);

    let (status, runs) = send(ctx.get("/api/flows/orders%2Fsync%20now/runs")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(strings(&runs, "id"), [run_id.to_string()]);
    ctx.shutdown().await.unwrap();
}

#[openshop::test(api_context())]
async fn post_flow_run_conflicts_while_a_run_is_active(ctx: &TestContext) {
    let (status, first) = send(ctx.post("/api/flows/test-flow/run").json(&json!({ "input": {} }))).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let first_run = first["runId"].as_str().expect("runId");

    let (status, conflict) = send(ctx.post("/api/flows/test-flow/run").json(&json!({ "input": {} }))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        conflict,
        json!({
            "error": format!("Flow \"test-flow\" is already running for shop \"{}\" (run #{first_run})", ctx.shop),
            "existingRunId": first_run,
        })
    );
}

// ─── Cancel, retry, delete ───────────────────────────────────────────

#[openshop::test(api_context())]
async fn post_run_cancel_cancels_a_pending_run(ctx: &TestContext) {
    let run_id = ctx.dispatch_flow("test-flow", json!({})).await.unwrap();

    let (status, body) = send(ctx.post(&format!("/api/runs/{run_id}/cancel"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "ok": true }));

    let run = find_run(&ctx.db, run_id).await.expect("run");
    assert_eq!(run.status, "canceled");
    assert!(run.completed_at.is_some());
}

#[tokio::test]
async fn post_run_cancel_aborts_a_run_in_progress() {
    let started = Arc::new(tokio::sync::Notify::new());
    let flow_started = started.clone();
    let flow = Flow::new("test-flow", move |ctx: FlowContext| {
        let started = flow_started.clone();
        async move {
            started.notify_one();
            ctx.signal.cancelled().await;
            Err(anyhow!(FlowCanceledError))
        }
    });
    let ctx = context(api_config_with(flow)).await;
    let run_id = ctx.dispatch_flow("test-flow", json!({})).await.unwrap();

    let app = ctx.app.clone();
    let shop = ctx.shop.clone();
    let running = tokio::spawn(async move { run_flow(&app, RunFlowOptions::new(run_id, "test-flow", shop)).await });
    started.notified().await;

    let (status, _) = send(ctx.post(&format!("/api/runs/{run_id}/cancel"))).await;
    assert_eq!(status, StatusCode::OK);

    let result = tokio::time::timeout(Duration::from_secs(10), running)
        .await
        .expect("the flow observes the cancellation")
        .unwrap()
        .unwrap();
    assert_eq!(result, RunOutcome::Canceled);
    assert_eq!(run_status(&ctx.db, run_id).await, "canceled");
    ctx.shutdown().await.unwrap();
}

#[openshop::test(api_context())]
async fn post_run_cancel_rejects_finished_runs(ctx: &TestContext) {
    let base = base_time();

    for status in ["completed", "failed", "canceled"] {
        let run_id = seed_run(ctx, "test-flow", status, base).await;
        let (code, body) = send(ctx.post(&format!("/api/runs/{run_id}/cancel"))).await;
        assert_eq!(code, StatusCode::CONFLICT);
        assert_eq!(body, error(&format!("Cannot cancel a run with status \"{status}\"")));
        assert_eq!(run_status(&ctx.db, run_id).await, status);
    }
    for status in ["running", "sleeping"] {
        let run_id = seed_run(ctx, "test-flow", status, base).await;
        let (code, _) = send(ctx.post(&format!("/api/runs/{run_id}/cancel"))).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(run_status(&ctx.db, run_id).await, "canceled");
    }
}

#[openshop::test(api_context())]
async fn post_run_retry_retries_a_failed_run(ctx: &TestContext) {
    let run_id = dispatch_without_retries(ctx, "test-flow").await;
    execute(ctx, run_id, "test-flow").await;

    // Manually set to failed so we can retry.
    sqlx::query("UPDATE flow_runs SET status = 'failed', error = 'boom', attempts = 2, worker_id = 'w1' WHERE id = $1")
        .bind(run_id)
        .execute(&ctx.db)
        .await
        .unwrap();

    let (status, body) = send(ctx.post(&format!("/api/runs/{run_id}/retry"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "ok": true, "runId": run_id.to_string(), "mode": "resume" })
    );

    let run = find_run(&ctx.db, run_id).await.expect("run");
    assert_eq!(run.status, "pending");
    assert_eq!(run.error, None);
    assert_eq!(run.attempts, 0);
    assert_eq!(run.worker_id, None);
    assert_eq!(run.retry_policy, None);
    assert_eq!(run.started_at, None);
    assert_eq!(run.completed_at, None);
    assert_eq!(run.deadline_at, None);
    let available_in = (run.available_at.expect("availableAt") - Utc::now())
        .num_seconds()
        .abs();
    assert!(available_in < 60, "{available_in}");

    let (payload,): (Value,) = sqlx::query_as(
        "SELECT payload FROM logs WHERE flow_run_id = $1 AND message = 'Run retried (resume — completed steps kept)'",
    )
    .bind(run_id.to_string())
    .fetch_one(&ctx.db)
    .await
    .unwrap();
    assert_eq!(payload, json!({ "mode": "resume", "flowName": "test-flow" }));
}

#[tokio::test]
async fn post_run_retry_resume_keeps_completed_steps_and_reset_discards_them() {
    let flow = Flow::new("test-flow", |ctx: FlowContext| async move {
        let _fetched: i64 = ctx.step.run("fetch", || async { Ok(1) }).await?;
        let _pushed: i64 = ctx
            .step
            .run("push", || async { Err::<i64, _>(anyhow!("push failed")) })
            .await?;
        Ok(())
    })
    .timeout(Duration::from_secs(60));
    let ctx = context(api_config_with(flow)).await;
    let run_id = dispatch_without_retries(&ctx, "test-flow").await;

    let result = execute(&ctx, run_id, "test-flow").await;
    assert_eq!(result.status(), "failed");
    assert_eq!(run_status(&ctx.db, run_id).await, "failed");
    assert_eq!(step_names(&ctx.db, run_id).await, ["fetch", "push"]);

    let before = Utc::now();
    let (status, body) = send(ctx.post(&format!("/api/runs/{run_id}/retry"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["mode"], "resume");
    assert_eq!(step_names(&ctx.db, run_id).await, ["fetch"]);

    let run = find_run(&ctx.db, run_id).await.expect("run");
    assert_eq!(run.status, "pending");
    assert_eq!(run.error, None);
    let deadline_in = (run.deadline_at.expect("deadline from the flow timeout") - before).num_seconds();
    assert!((55..=65).contains(&deadline_in), "{deadline_in}");

    assert_eq!(execute(&ctx, run_id, "test-flow").await.status(), "failed");
    assert_eq!(step_names(&ctx.db, run_id).await, ["fetch", "push"]);

    let (status, body) = send(ctx.post(&format!("/api/runs/{run_id}/retry?mode=reset"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "ok": true, "runId": run_id.to_string(), "mode": "reset" })
    );
    assert!(step_names(&ctx.db, run_id).await.is_empty());
    assert_eq!(run_status(&ctx.db, run_id).await, "pending");
    assert!(
        log_messages(&ctx.db, run_id)
            .await
            .contains(&"Run retried (restart — all steps discarded)".to_string())
    );
    ctx.shutdown().await.unwrap();
}

#[openshop::test(api_context())]
async fn post_run_retry_rejects_active_runs(ctx: &TestContext) {
    let base = base_time();

    for status in ["pending", "running", "sleeping"] {
        let run_id = seed_run(ctx, "test-flow", status, base).await;
        let (code, body) = send(ctx.post(&format!("/api/runs/{run_id}/retry"))).await;
        assert_eq!(code, StatusCode::CONFLICT);
        assert_eq!(body, error(&format!("Cannot retry a run with status \"{status}\"")));
        assert_eq!(run_status(&ctx.db, run_id).await, status);
    }
    for status in ["completed", "canceled"] {
        let run_id = seed_run(ctx, "test-flow", status, base).await;
        let (code, _) = send(ctx.post(&format!("/api/runs/{run_id}/retry"))).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(run_status(&ctx.db, run_id).await, "pending");
    }
}

#[openshop::test(api_context())]
async fn delete_run_removes_the_run_with_its_steps_and_logs(ctx: &TestContext) {
    let base = base_time();
    let run_id = seed_run(ctx, "test-flow", "completed", base).await;
    let kept = seed_run(ctx, "test-flow", "failed", base).await;
    for id in [run_id, kept] {
        seed_step(&ctx.db, id, "step", "completed", base).await;
        seed_log(&ctx.db, id, "info", "line", json!({}), base).await;
    }

    let (status, body) = send(ctx.delete(&format!("/api/runs/{run_id}"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "ok": true }));

    assert!(find_run(&ctx.db, run_id).await.is_none());
    assert!(step_names(&ctx.db, run_id).await.is_empty());
    assert!(log_messages(&ctx.db, run_id).await.is_empty());

    assert!(find_run(&ctx.db, kept).await.is_some());
    assert_eq!(step_names(&ctx.db, kept).await, ["step"]);
    assert_eq!(log_messages(&ctx.db, kept).await, ["line"]);

    let (status, _) = send(ctx.delete(&format!("/api/runs/{run_id}"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[openshop::test(api_context())]
async fn delete_run_rejects_active_runs(ctx: &TestContext) {
    let base = base_time();

    for status in ["pending", "running", "sleeping"] {
        let run_id = seed_run(ctx, "test-flow", status, base).await;
        let (code, body) = send(ctx.delete(&format!("/api/runs/{run_id}"))).await;
        assert_eq!(code, StatusCode::CONFLICT);
        assert_eq!(body, error("Cannot delete an active run — cancel it first"));
        assert!(find_run(&ctx.db, run_id).await.is_some());
    }
}

#[openshop::test(api_context())]
async fn post_runs_bulk_delete_deletes_finished_runs_and_skips_the_rest(ctx: &TestContext) {
    let base = base_time();
    let completed = seed_run(ctx, "test-flow", "completed", base).await;
    let failed = seed_run(ctx, "test-flow", "failed", base).await;
    let pending = seed_run(ctx, "test-flow", "pending", base).await;
    let untouched = seed_run(ctx, "test-flow", "completed", base).await;
    for id in [completed, failed, pending, untouched] {
        seed_step(&ctx.db, id, "step", "completed", base).await;
        seed_log(&ctx.db, id, "info", "line", json!({}), base).await;
    }

    let (status, body) = send(ctx.post("/api/runs/bulk-delete").json(&json!({
        "ids": [completed, failed, pending, NIL_RUN, "not-a-uuid", 42],
    })))
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "deleted": 2, "skipped": 4 }));

    for id in [completed, failed] {
        assert!(find_run(&ctx.db, id).await.is_none());
        assert!(step_names(&ctx.db, id).await.is_empty());
        assert!(log_messages(&ctx.db, id).await.is_empty());
    }
    for id in [pending, untouched] {
        assert!(find_run(&ctx.db, id).await.is_some());
        assert_eq!(step_names(&ctx.db, id).await, ["step"]);
        assert_eq!(log_messages(&ctx.db, id).await, ["line"]);
    }

    let (status, body) = send(ctx.post("/api/runs/bulk-delete").json(&json!({ "ids": [pending] }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "deleted": 0, "skipped": 1 }));
}

#[openshop::test(api_context())]
async fn post_runs_bulk_delete_validates_its_body(ctx: &TestContext) {
    let run_id = seed_run(ctx, "test-flow", "completed", base_time()).await;

    let invalid = [
        ctx.post("/api/runs/bulk-delete"),
        ctx.post("/api/runs/bulk-delete").body("not json"),
        ctx.post("/api/runs/bulk-delete").json(&json!({})),
        ctx.post("/api/runs/bulk-delete").json(&json!({ "ids": [] })),
        ctx.post("/api/runs/bulk-delete").json(&json!({ "ids": run_id })),
        ctx.post("/api/runs/bulk-delete").json(&json!([run_id])),
    ];
    for request in invalid {
        let (status, body) = send(request).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body, error("No IDs provided"));
    }

    let mut too_many = vec![json!(run_id)];
    too_many.extend((0..100).map(|_| json!(Uuid::new_v4())));
    let (status, body) = send(ctx.post("/api/runs/bulk-delete").json(&json!({ "ids": too_many }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, error("Max 100 runs per batch"));

    assert!(find_run(&ctx.db, run_id).await.is_some());

    let (status, body) = send(
        ctx.post("/api/runs/bulk-delete")
            .json(&json!({ "ids": too_many[..100] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "deleted": 1, "skipped": 99 }));
}

// ─── Auth ────────────────────────────────────────────────────────────

#[openshop::test(api_context())]
async fn request_without_token_returns_401(ctx: &TestContext) {
    let (status, body) = send(anonymous(ctx, Method::GET, "/api/flows")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, error("Unauthorized: missing session token"));

    let (status, body) = send(anonymous(ctx, Method::GET, "/api/flows").header("Authorization", "Basic abc")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, error("Unauthorized: missing session token"));

    // The session token is only read from the Authorization header.
    let path = format!("/api/flows?shop={}&token={}", ctx.shop, ctx.session_token());
    let (status, _) = send(anonymous(ctx, Method::GET, &path)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[openshop::test(api_context())]
async fn request_with_invalid_token_returns_401(ctx: &TestContext) {
    let (status, body) = send(request_as(ctx, "invalid.token.here", Method::GET, "/api/flows")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, error("Unauthorized: missing token audience"));

    let (status, _) = send(request_as(ctx, "garbage", Method::GET, "/api/flows")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[openshop::test(api_context())]
async fn request_with_forged_or_expired_token_returns_401(ctx: &TestContext) {
    let now = Utc::now().timestamp();
    let claims = |shop: &str, exp: i64| {
        json!({
            "iss": format!("https://{shop}/admin"),
            "dest": format!("https://{shop}"),
            "aud": ctx.api_key,
            "sub": "1",
            "exp": exp,
            "nbf": now - 10,
            "iat": now,
            "jti": "jti",
            "sid": "sid",
        })
    };
    let mut mismatched = claims(&ctx.shop, now + 60);
    mismatched["iss"] = json!(format!("https://{OTHER_SHOP}/admin"));

    let cases = [
        (
            session_token(&ctx.shop, "wrong-secret", &ctx.api_key, "1"),
            "Unauthorized: Invalid JWT signature",
        ),
        (
            session_token(&ctx.shop, &ctx.secret, "another-app", "1"),
            "Unauthorized: [openshop] No Shopify app matches the session token audience",
        ),
        (
            sign_session_token(&claims(&ctx.shop, now - 5), &ctx.secret),
            "Unauthorized: JWT expired",
        ),
        (
            sign_session_token(&claims("evil.example.com", now + 60), &ctx.secret),
            "Unauthorized: JWT missing or invalid dest claim",
        ),
        (
            sign_session_token(&mismatched, &ctx.secret),
            "Unauthorized: JWT dest/iss mismatch",
        ),
    ];
    for (token, message) in cases {
        let (status, body) = send(request_as(ctx, &token, Method::GET, "/api/flows")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{message}");
        assert_eq!(body, error(message));
    }
}

#[openshop::test(api_context())]
async fn unknown_api_paths_require_a_session_then_return_404(ctx: &TestContext) {
    let cases = [
        (Method::GET, "/api"),
        (Method::GET, "/api/"),
        (Method::GET, "/api/unknown"),
        (Method::POST, "/api/unknown/deep/path"),
        (Method::DELETE, "/api/flows"),
        (Method::POST, "/api/runs"),
        (Method::PUT, "/api/crons/toggle"),
        (Method::PATCH, "/api/providers/warehouse"),
    ];
    for (method, path) in cases {
        let (status, body) = send(anonymous(ctx, method.clone(), path)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
        assert_eq!(body, error("Unauthorized: missing session token"));

        let (status, body) = send(ctx.request(method.clone(), path)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
        assert_eq!(body, "404 Not Found", "{method} {path}");
    }
}

#[tokio::test]
async fn session_token_exchange_installs_the_shop_once() {
    let ctx = TestContext::with_options(
        api_config(),
        TestOptions {
            encryption_key: Some(ENCRYPTION_KEY.to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // A failed exchange does not block the request. The client credentials
    // grant is asked after it, and fails too.
    let (status, _) = send(ctx.get("/api/flows")).await;
    assert_eq!(status, StatusCode::OK);
    let grant_types: Vec<Value> = ctx
        .transport
        .requests()
        .iter()
        .map(|request| request.body["grant_type"].clone())
        .collect();
    assert_eq!(
        grant_types,
        [
            json!("urn:ietf:params:oauth:grant-type:token-exchange"),
            json!("client_credentials")
        ]
    );
    ctx.transport.clear();

    ctx.transport
        .respond_json(json!({ "access_token": "shpat_offline", "scope": "read_products" }));
    let token = ctx.session_token();
    let (status, _) = send(request_as(&ctx, &token, Method::GET, "/api/flows")).await;
    assert_eq!(status, StatusCode::OK);

    let requests = ctx.transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].url,
        format!("https://{}/admin/oauth/access_token", ctx.shop)
    );
    assert_eq!(
        requests[0].body,
        json!({
            "client_id": ctx.api_key,
            "client_secret": ctx.secret,
            "grant_type": "urn:ietf:params:oauth:grant-type:token-exchange",
            "subject_token": token,
            "subject_token_type": "urn:ietf:params:oauth:token-type:id_token",
            "requested_token_type": "urn:shopify:params:oauth:token-type:offline-access-token",
        })
    );

    let (app_handle, stored, scopes): (String, String, String) =
        sqlx::query_as("SELECT app_handle, access_token, scopes FROM installations WHERE shop = $1")
            .bind(&ctx.shop)
            .fetch_one(&ctx.db)
            .await
            .unwrap();
    assert_eq!(app_handle, "default");
    assert_eq!(scopes, "read_products");
    assert!(stored.starts_with("enc:aes-256-gcm:"), "{stored}");
    assert_eq!(
        ctx.app.crypto().decrypt_string(Some(&stored)).unwrap().as_deref(),
        Some("shpat_offline")
    );

    let (status, _) = send(ctx.get("/api/flows")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ctx.transport.requests().len(), 1);
    ctx.shutdown().await.unwrap();
}

// ─── Logs ────────────────────────────────────────────────────────────

fn logging_flow() -> Flow {
    Flow::new("test-flow", |ctx: FlowContext| async move {
        ctx.logger.info_with("info message", json!({ "key": "val" }));
        ctx.logger.warn("warn message");
        ctx.logger.error("error message");
        Ok(())
    })
}

#[openshop::test(context(api_config_with(logging_flow())))]
async fn get_run_logs_returns_logs_with_filters(ctx: &TestContext) {
    let run_id = ctx.dispatch_flow("test-flow", json!({})).await.unwrap();
    execute(ctx, run_id, "test-flow").await;

    // All logs: the three lines of the flow plus the ones of the engine.
    let (status, data) = send(ctx.get(&format!("/api/runs/{run_id}/logs?levels=info,warn,error"))).await;
    assert_eq!(status, StatusCode::OK);
    let logs = data["logs"].as_array().expect("logs");
    let total = logs.len();
    assert!(total >= 3, "{data}");
    assert_eq!(data["total"], total);
    for log in logs {
        assert_eq!(log["flowRunId"], run_id.to_string());
        assert!(Uuid::parse_str(log["id"].as_str().unwrap()).is_ok());
        assert_iso_millis(&log["createdAt"]);
        assert!(log.get("_matched").is_none());
    }
    let info = logs
        .iter()
        .find(|log| log["message"] == "info message")
        .expect("info line");
    assert_eq!(info["level"], "info");
    assert_eq!(info["payload"], json!({ "key": "val" }));

    // Levels default to info, warn and error.
    let (_, defaults) = send(ctx.get(&format!("/api/runs/{run_id}/logs"))).await;
    assert_eq!(defaults, data);

    let (_, errors) = send(ctx.get(&format!("/api/runs/{run_id}/logs?levels=error"))).await;
    assert_eq!(strings(&errors["logs"], "message"), ["error message"]);
    assert_eq!(errors["total"], total);

    // Filter by query.
    let (status, filtered) = send(ctx.get(&format!("/api/runs/{run_id}/logs?levels=info,warn,error&q=warn"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(strings(&filtered["logs"], "message"), ["warn message"]);
    assert_eq!(filtered["logs"][0]["_matched"], true);
    assert_eq!(filtered["total"], total);
}

/// Six log lines one second apart; the API returns them newest first.
async fn seed_order_logs(ctx: &TestContext) -> (Uuid, DateTime<Utc>) {
    let base = base_time();
    let run_id = seed_run(ctx, "test-flow", "completed", base).await;
    let lines = [
        ("info", "Fetching orders", json!({ "count": 3 })),
        ("info", "Order 1001 synced", json!({ "orderId": 1001 })),
        ("warn", "Rate limit hit", json!({ "retryAfter": 2 })),
        (
            "error",
            "Order 1002 failed",
            json!({ "orderId": 1002, "reason": "Timeout" }),
        ),
        ("info", "Order 1003 synced", json!({ "orderId": 1003 })),
        ("info", "Done", json!({})),
    ];
    for (index, (level, message, payload)) in lines.into_iter().enumerate() {
        let at = base + chrono::Duration::seconds(index as i64);
        seed_log(&ctx.db, run_id, level, message, payload, at).await;
    }
    (run_id, base)
}

async fn query_logs(ctx: &TestContext, run_id: Uuid, params: &[(&str, &str)]) -> Value {
    let (status, data) = send(ctx.get(&format!("/api/runs/{run_id}/logs")).query(params)).await;
    assert_eq!(status, StatusCode::OK, "{params:?}");
    assert_eq!(data["total"], 6, "{params:?}");
    data["logs"].clone()
}

#[openshop::test(api_context())]
async fn get_run_logs_supports_the_query_syntax(ctx: &TestContext) {
    let (run_id, _) = seed_order_logs(ctx).await;

    let all = query_logs(ctx, run_id, &[]).await;
    assert_eq!(
        strings(&all, "message"),
        [
            "Done",
            "Order 1003 synced",
            "Order 1002 failed",
            "Rate limit hit",
            "Order 1001 synced",
            "Fetching orders"
        ]
    );

    let cases: [(&str, &[&str]); 10] = [
        (
            "ORDER",
            &[
                "Order 1003 synced",
                "Order 1002 failed",
                "Order 1001 synced",
                "Fetching orders",
            ],
        ),
        (r#"|= "synced""#, &["Order 1003 synced", "Order 1001 synced"]),
        (
            r#"|= "order" != "failed""#,
            &["Order 1003 synced", "Order 1001 synced", "Fetching orders"],
        ),
        (
            r#"|~ "order \d+ (synced|failed)""#,
            &["Order 1003 synced", "Order 1002 failed", "Order 1001 synced"],
        ),
        (r#"|~ "^done""#, &["Done"]),
        ("orderid=1002", &["Order 1002 failed"]),
        ("reason=timeout", &["Order 1002 failed"]),
        (r#"!= "order""#, &["Done", "Rate limit hit"]),
        (r#"|~ "(a+)+""#, &[]),
        (r#"|~ "(""#, &[]),
    ];
    for (query, expected) in cases {
        let logs = query_logs(ctx, run_id, &[("q", query)]).await;
        assert_eq!(strings(&logs, "message"), expected, "q={query}");
        assert!(
            logs.as_array().unwrap().iter().all(|log| log["_matched"] == true),
            "q={query}"
        );
    }

    let warnings = query_logs(ctx, run_id, &[("levels", "warn,error"), ("q", "order")]).await;
    assert_eq!(strings(&warnings, "message"), ["Order 1002 failed"]);

    let none = query_logs(ctx, run_id, &[("levels", "debug")]).await;
    assert_eq!(none, json!([]));
}

#[openshop::test(api_context())]
async fn get_run_logs_expands_context_around_matches(ctx: &TestContext) {
    let (run_id, _) = seed_order_logs(ctx).await;
    let flagged = |logs: &Value| -> Vec<(String, bool)> {
        logs.as_array()
            .unwrap()
            .iter()
            .map(|log| (log["message"].as_str().unwrap().to_string(), log["_matched"] == true))
            .collect()
    };
    let line = |message: &str, matched: bool| (message.to_string(), matched);

    let around = query_logs(ctx, run_id, &[("q", "failed C:1")]).await;
    assert_eq!(
        flagged(&around),
        [
            line("Order 1003 synced", false),
            line("Order 1002 failed", true),
            line("Rate limit hit", false),
        ]
    );

    let before = query_logs(ctx, run_id, &[("q", "B:1 failed")]).await;
    assert_eq!(
        flagged(&before),
        [line("Order 1003 synced", false), line("Order 1002 failed", true)]
    );

    let after = query_logs(ctx, run_id, &[("q", "failed A:2")]).await;
    assert_eq!(
        flagged(&after),
        [
            line("Order 1002 failed", true),
            line("Rate limit hit", false),
            line("Order 1001 synced", false),
        ]
    );

    // Context lines are taken from the level-filtered list.
    let filtered = query_logs(ctx, run_id, &[("levels", "info,error"), ("q", "failed C:1")]).await;
    assert_eq!(
        flagged(&filtered),
        [
            line("Order 1003 synced", false),
            line("Order 1002 failed", true),
            line("Order 1001 synced", false),
        ]
    );

    // Context alone is not a filter.
    let unfiltered = query_logs(ctx, run_id, &[("q", "C:2")]).await;
    assert_eq!(unfiltered.as_array().unwrap().len(), 6);
    assert!(unfiltered[0].get("_matched").is_none());
}

#[openshop::test(api_context())]
async fn get_run_logs_restricts_by_time_range(ctx: &TestContext) {
    let (run_id, base) = seed_order_logs(ctx).await;
    let at = |seconds: i64| to_iso(&(base + chrono::Duration::seconds(seconds)));

    let from = query_logs(ctx, run_id, &[("q", &format!("from:{}", at(3)))]).await;
    assert_eq!(
        strings(&from, "message"),
        ["Done", "Order 1003 synced", "Order 1002 failed"]
    );
    assert!(from[0].get("_matched").is_none());

    let to = query_logs(ctx, run_id, &[("q", &format!("to:{}", at(1)))]).await;
    assert_eq!(strings(&to, "message"), ["Order 1001 synced", "Fetching orders"]);

    let between = query_logs(ctx, run_id, &[("q", &format!("between:{},{}", at(1), at(3)))]).await;
    assert_eq!(
        strings(&between, "message"),
        ["Order 1002 failed", "Rate limit hit", "Order 1001 synced"]
    );

    let combined = query_logs(ctx, run_id, &[("q", &format!("from:{} synced", at(3)))]).await;
    assert_eq!(strings(&combined, "message"), ["Order 1003 synced"]);

    // The lines were written an hour ago.
    assert_eq!(query_logs(ctx, run_id, &[("q", "last:5m")]).await, json!([]));
    assert_eq!(
        query_logs(ctx, run_id, &[("q", "last:2h")])
            .await
            .as_array()
            .unwrap()
            .len(),
        6
    );

    let invalid = query_logs(ctx, run_id, &[("q", "from:yesterday")]).await;
    assert_eq!(invalid.as_array().unwrap().len(), 6);
}

#[openshop::test(context(api_config_with(logging_flow())))]
async fn get_run_logs_export_returns_json(ctx: &TestContext) {
    let run_id = ctx.dispatch_flow("test-flow", json!({})).await.unwrap();
    execute(ctx, run_id, "test-flow").await;

    let response = ctx
        .get(&format!(
            "/api/runs/{run_id}/logs/export?format=json&levels=info,warn,error"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
    assert_eq!(
        response.headers()[CONTENT_DISPOSITION],
        format!("attachment; filename=\"run-{run_id}-logs.json\"")
    );
    let text = response.text().await.unwrap();
    assert!(text.starts_with("[\n  {"), "pretty-printed: {text}");
    let logs: Value = serde_json::from_str(&text).unwrap();
    let messages = strings(&logs, "message");
    for message in ["info message", "warn message", "error message"] {
        assert!(
            messages.iter().any(|logged| logged == message),
            "{message}: {messages:?}"
        );
    }
    assert!(logs.as_array().unwrap().iter().all(|log| log.get("_matched").is_none()));

    // JSON is the default format; a query exports the matching lines only, without context.
    let response = ctx
        .get(&format!("/api/runs/{run_id}/logs/export"))
        .query(&[("q", "message C:5"), ("levels", "warn,error")])
        .send()
        .await
        .unwrap();
    assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
    let logs: Value = response.json().await.unwrap();
    let mut messages = strings(&logs, "message");
    messages.sort();
    assert_eq!(messages, ["error message", "warn message"]);
    assert!(logs[0].get("_matched").is_none());
}

#[openshop::test(api_context())]
async fn get_run_logs_export_returns_csv(ctx: &TestContext) {
    let (run_id, base) = seed_order_logs(ctx).await;
    let (first_id,): (Uuid,) =
        sqlx::query_as("SELECT id FROM logs WHERE flow_run_id = $1 AND message = 'Order 1001 synced'")
            .bind(run_id.to_string())
            .fetch_one(&ctx.db)
            .await
            .unwrap();

    let response = ctx
        .get(&format!(
            "/api/runs/{run_id}/logs/export?format=csv&levels=info,warn,error"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[CONTENT_TYPE], "text/csv");
    assert_eq!(
        response.headers()[CONTENT_DISPOSITION],
        format!("attachment; filename=\"run-{run_id}-logs.csv\"")
    );
    let text = response.text().await.unwrap();
    assert!(text.contains("level,message"));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 7);
    assert_eq!(lines[0], "id,level,message,payload,createdAt");
    assert_eq!(
        lines[5],
        format!(
            "{first_id},info,\"Order 1001 synced\",\"{{\"\"orderId\"\":1001}}\",{}",
            to_iso(&(base + chrono::Duration::seconds(1)))
        )
    );

    let response = ctx
        .get(&format!("/api/runs/{run_id}/logs/export"))
        .query(&[("format", "csv"), ("q", r#"|= "synced""#)])
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap().lines().count(), 3);
}

#[openshop::test(api_context())]
async fn get_run_logs_export_sanitizes_unknown_formats(ctx: &TestContext) {
    let (run_id, _) = seed_order_logs(ctx).await;

    for (format, extension) in [
        ("xml", "xml"),
        ("../../etc/passwd", "json"),
        ("a\"b", "json"),
        ("", "json"),
    ] {
        let response = ctx
            .get(&format!("/api/runs/{run_id}/logs/export"))
            .query(&[("format", format)])
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "format={format}");
        assert_eq!(response.headers()[CONTENT_TYPE], "application/json", "format={format}");
        assert_eq!(
            response.headers()[CONTENT_DISPOSITION],
            format!("attachment; filename=\"run-{run_id}-logs.{extension}\""),
            "format={format}"
        );
        let logs: Value = response.json().await.unwrap();
        assert_eq!(logs.as_array().unwrap().len(), 6);
    }
}

// ─── Providers ───────────────────────────────────────────────────────

fn warehouse_credentials() -> Value {
    json!({ "config": { "endpoint": "https://warehouse.test", "apiKey": "secret-1" } })
}

async fn stored_provider_config(db: &PgPool, shop: &str) -> Value {
    let (config,): (Value,) =
        sqlx::query_as("SELECT config FROM provider_configs WHERE shop = $1 AND provider_name = 'warehouse'")
            .bind(shop)
            .fetch_one(db)
            .await
            .unwrap();
    config
}

#[openshop::test(api_context())]
async fn get_providers_returns_provider_list(ctx: &TestContext) {
    let (status, data) = send(ctx.get("/api/providers")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        data,
        json!([{
            "name": "warehouse",
            "fields": {
                "endpoint": { "type": "text", "label": "Endpoint" },
                "apiKey": { "type": "password", "label": "API key", "hasValue": false },
                "optionalNote": { "type": "text", "label": "Optional note", "required": false },
            },
            "config": {},
            "configured": false,
            "lastCheckedAt": null,
            "lastCheckOk": null,
        }])
    );
    assert!(data[0]["fields"]["apiKey"].get("validate").is_none());
}

#[openshop::test(api_context())]
async fn put_providers_masks_and_preserves_password_fields(ctx: &TestContext) {
    let (status, body) = send(ctx.put("/api/providers/warehouse").json(&warehouse_credentials())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "ok": true }));

    let (_, list) = send(ctx.get("/api/providers")).await;
    let provider = &list[0];
    assert_eq!(provider["config"], json!({ "endpoint": "https://warehouse.test" }));
    assert_eq!(provider["fields"]["apiKey"]["hasValue"], true);
    assert_eq!(provider["configured"], true);
    assert_eq!(provider["lastCheckOk"], Value::Null);
    assert!(!list.to_string().contains("secret-1"));

    // A blank password keeps the stored one.
    let (status, _) = send(
        ctx.put("/api/providers/warehouse")
            .json(&json!({ "config": { "endpoint": "https://warehouse.test", "apiKey": "" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        stored_provider_config(&ctx.db, &ctx.shop).await,
        json!({ "endpoint": "https://warehouse.test", "apiKey": "secret-1" })
    );

    let (status, _) = send(ctx.put("/api/providers/warehouse").json(&json!({
        "config": { "endpoint": "https://warehouse-2.test", "apiKey": "secret-2", "optionalNote": "EU" },
    })))
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        stored_provider_config(&ctx.db, &ctx.shop).await,
        json!({ "endpoint": "https://warehouse-2.test", "apiKey": "secret-2", "optionalNote": "EU" })
    );
    let (_, list) = send(ctx.get("/api/providers")).await;
    assert_eq!(
        list[0]["config"],
        json!({ "endpoint": "https://warehouse-2.test", "optionalNote": "EU" })
    );

    let (rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM provider_configs")
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(rows, 1);
}

#[tokio::test]
async fn put_providers_encrypts_config_at_rest() {
    let ctx = TestContext::with_options(
        api_config(),
        TestOptions {
            encryption_key: Some(ENCRYPTION_KEY.to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let (status, _) = send(ctx.put("/api/providers/warehouse").json(&warehouse_credentials())).await;
    assert_eq!(status, StatusCode::OK);

    let stored = stored_provider_config(&ctx.db, &ctx.shop).await;
    assert_eq!(stored["__encrypted"], "aes-256-gcm");
    assert!(stored["payload"].is_string());
    assert_eq!(stored.as_object().unwrap().len(), 2);
    let raw = stored.to_string();
    assert!(!raw.contains("secret-1") && !raw.contains("warehouse.test"), "{raw}");
    assert_eq!(
        Value::Object(ctx.app.crypto().decrypt_config(Some(&stored)).unwrap()),
        warehouse_credentials()["config"]
    );

    let (_, list) = send(ctx.get("/api/providers")).await;
    assert_eq!(list[0]["config"], json!({ "endpoint": "https://warehouse.test" }));
    assert_eq!(list[0]["fields"]["apiKey"]["hasValue"], true);
    assert_eq!(list[0]["configured"], true);

    // The stored password survives an update that leaves it blank, and the checker sees plaintext.
    let (status, _) = send(
        ctx.put("/api/providers/warehouse")
            .json(&json!({ "config": { "endpoint": "https://warehouse.test", "apiKey": "" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let updated = stored_provider_config(&ctx.db, &ctx.shop).await;
    assert_ne!(updated["payload"], stored["payload"]);
    assert_eq!(
        Value::Object(ctx.app.crypto().decrypt_config(Some(&updated)).unwrap()),
        warehouse_credentials()["config"]
    );

    let (status, check) = send(ctx.post("/api/providers/warehouse/check")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(check, json!({ "ok": true }));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn legacy_plaintext_provider_config_is_read_and_encrypted_on_save() {
    let ctx = TestContext::with_options(
        api_config(),
        TestOptions {
            encryption_key: Some(ENCRYPTION_KEY.to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    sqlx::query("INSERT INTO provider_configs (shop, provider_name, config) VALUES ($1, 'warehouse', $2::json)")
        .bind(&ctx.shop)
        .bind(warehouse_credentials()["config"].to_string())
        .execute(&ctx.db)
        .await
        .unwrap();

    let (status, list) = send(ctx.get("/api/providers")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list[0]["config"], json!({ "endpoint": "https://warehouse.test" }));
    assert_eq!(list[0]["configured"], true);
    let (_, check) = send(ctx.post("/api/providers/warehouse/check")).await;
    assert_eq!(check, json!({ "ok": true }));

    let (status, _) = send(
        ctx.put("/api/providers/warehouse")
            .json(&json!({ "config": { "endpoint": "https://warehouse.test", "apiKey": "" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let stored = stored_provider_config(&ctx.db, &ctx.shop).await;
    assert_eq!(stored["__encrypted"], "aes-256-gcm");
    assert_eq!(
        Value::Object(ctx.app.crypto().decrypt_config(Some(&stored)).unwrap()),
        warehouse_credentials()["config"]
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn production_refuses_to_store_secrets_without_an_encryption_key() {
    let ctx = TestContext::with_options(
        api_config(),
        TestOptions {
            production: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    ctx.transport
        .respond_json(json!({ "access_token": "shpat_offline", "scope": "read_products" }));

    let (status, list) = send(ctx.get("/api/providers")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list[0]["configured"], false);

    let (status, body) = send(ctx.put("/api/providers/warehouse").json(&warehouse_credentials())).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body, "Internal Server Error");

    // Neither the provider credentials nor the exchanged access token reach the database.
    for table in ["provider_configs", "installations"] {
        let (rows,): (i64,) = sqlx::query_as(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&ctx.db)
            .await
            .unwrap();
        assert_eq!(rows, 0, "{table}");
    }
    assert!(!ctx.transport.requests().is_empty());
    ctx.shutdown().await.unwrap();
}

#[openshop::test(api_context())]
async fn saved_provider_stays_configured_before_and_after_a_health_check(ctx: &TestContext) {
    // Nothing is stored yet: the check runs against an empty configuration and records nothing.
    let (status, check) = send(ctx.post("/api/providers/warehouse/check")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(check, json!({ "ok": false }));
    let (_, list) = send(ctx.get("/api/providers")).await;
    assert_eq!(list[0]["lastCheckOk"], Value::Null);

    let (status, _) = send(ctx.put("/api/providers/warehouse").json(&warehouse_credentials())).await;
    assert_eq!(status, StatusCode::OK);

    let (_, before) = send(ctx.get("/api/providers")).await;
    assert_eq!(before[0]["configured"], true);
    assert_eq!(before[0]["lastCheckOk"], Value::Null);
    assert_eq!(before[0]["lastCheckedAt"], Value::Null);

    let (status, check) = send(ctx.post("/api/providers/warehouse/check")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(check, json!({ "ok": true }));

    let (_, after) = send(ctx.get("/api/providers")).await;
    assert_eq!(after[0]["configured"], true);
    assert_eq!(after[0]["lastCheckOk"], true);
    assert_iso_millis(&after[0]["lastCheckedAt"]);

    // Saving again keeps the last check; a failing check overwrites it.
    let (status, _) = send(ctx.put("/api/providers/warehouse").json(&json!({
        "config": { "endpoint": "https://warehouse.test", "apiKey": "wrong" },
    })))
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, saved) = send(ctx.get("/api/providers")).await;
    assert_eq!(saved[0]["lastCheckOk"], true);

    let (status, check) = send(ctx.post("/api/providers/warehouse/check")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(check, json!({ "ok": false }));
    let (_, failed) = send(ctx.get("/api/providers")).await;
    assert_eq!(failed[0]["configured"], true);
    assert_eq!(failed[0]["lastCheckOk"], false);
}

#[openshop::test(api_context())]
async fn put_providers_rejects_missing_required_fields(ctx: &TestContext) {
    let (status, body) = send(
        ctx.put("/api/providers/warehouse")
            .json(&json!({ "config": { "endpoint": "https://warehouse.test" } })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, error("Field \"apiKey\" is required"));

    for body in [json!({}), json!({ "config": null }), json!({ "config": "endpoint" })] {
        let (status, response) = send(ctx.put("/api/providers/warehouse").json(&body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(response, error("Field \"endpoint\" is required"));
    }

    let (rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM provider_configs")
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(rows, 0);
}

#[tokio::test]
async fn put_providers_validates_coerces_and_transforms_fields() {
    let erp = Provider::new("erp")
        .field("endpoint", Field::text("Endpoint").validate(validators::url()))
        .field(
            "retries",
            Field::number("Retries").validate(validators::number_range(0.0, 10.0)),
        )
        .field("sandbox", Field::checkbox("Sandbox"))
        .transformer(|mut data| {
            if let Some(Value::String(endpoint)) = data.get("endpoint").cloned() {
                data.insert("endpoint".into(), json!(endpoint.trim_end_matches('/')));
            }
            Value::Object(data)
        });
    let config = OpenShopConfig::builder()
        .flow(noop_flow("test-flow"))
        .provider(erp)
        .build()
        .unwrap();
    let ctx = context(config).await;
    let save = |config: Value| send(ctx.put("/api/providers/erp").json(&json!({ "config": config })));

    let (status, body) = save(json!({ "endpoint": "nope", "retries": "3", "sandbox": "true" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, error("Field \"endpoint\": must be a URL string (was \"nope\")"));

    let (status, body) = save(json!({ "endpoint": "https://erp.test", "retries": "99", "sandbox": false })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let message = body["error"].as_str().unwrap();
    assert!(message.starts_with("Field \"retries\": "), "{message}");

    let (status, _) = save(json!({ "endpoint": "https://erp.test/", "retries": "3", "sandbox": "true" })).await;
    assert_eq!(status, StatusCode::OK);
    let (stored,): (Value,) = sqlx::query_as("SELECT config FROM provider_configs WHERE provider_name = 'erp'")
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(
        stored,
        json!({ "endpoint": "https://erp.test", "retries": 3, "sandbox": true })
    );

    let (_, list) = send(ctx.get("/api/providers")).await;
    assert_eq!(list[0]["config"], stored);
    assert_eq!(list[0]["configured"], true);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn provider_routes_reject_unknown_providers_and_missing_checkers() {
    let config = OpenShopConfig::builder()
        .flow(noop_flow("test-flow"))
        .provider(Provider::new("plain").field("token", Field::password("Token")))
        .build()
        .unwrap();
    let ctx = context(config).await;

    let (status, body) = send(
        ctx.put("/api/providers/nope")
            .json(&json!({ "config": { "token": "t" } })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, error("Provider not found"));

    let (status, body) = send(ctx.post("/api/providers/nope/check")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, error("Provider not found"));

    let (status, body) = send(ctx.post("/api/providers/plain/check")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, error("No checker defined"));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn post_providers_check_reports_checker_failures() {
    let flaky = Provider::new("flaky")
        .field("token", Field::password("Token"))
        .checker(|config| async move {
            match config.get("token").and_then(Value::as_str) {
                Some("panic") => panic!("checker exploded"),
                _ => Err(anyhow!("connection refused")),
            }
        });
    let config = OpenShopConfig::builder()
        .flow(noop_flow("test-flow"))
        .provider(flaky)
        .build()
        .unwrap();
    let ctx = context(config).await;
    let (status, _) = send(
        ctx.put("/api/providers/flaky")
            .json(&json!({ "config": { "token": "t" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send(ctx.post("/api/providers/flaky/check")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body, json!({ "ok": false, "error": "Error: connection refused" }));

    let (_, list) = send(ctx.get("/api/providers")).await;
    assert_eq!(list[0]["lastCheckOk"], Value::Null);
    assert_eq!(list[0]["lastCheckedAt"], Value::Null);

    let (status, _) = send(
        ctx.put("/api/providers/flaky")
            .json(&json!({ "config": { "token": "panic" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = send(ctx.post("/api/providers/flaky/check")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["ok"], false);

    // The server keeps serving after a panicking checker.
    let (status, _) = send(ctx.get("/api/providers")).await;
    assert_eq!(status, StatusCode::OK);
    ctx.shutdown().await.unwrap();
}

// ─── Health ──────────────────────────────────────────────────────────

#[openshop::test(api_context())]
async fn get_health_returns_ok(ctx: &TestContext) {
    let (status, data) = send(anonymous(ctx, Method::GET, "/health")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data["status"], "ok");
    assert_iso_millis(&data["timestamp"]);
}

// ─── Shopify functions list ──────────────────────────────────────────

#[tokio::test]
async fn get_functions_returns_definitions_without_calling_shopify() {
    let config = OpenShopConfig::builder()
        .flow(noop_flow("test-flow"))
        .function(
            "testFn",
            Function::new(FunctionType::CartTransform, "test-fn").field("note", Field::text("Note")),
        )
        .build()
        .unwrap();
    let ctx = TestContext::with_options(
        config,
        TestOptions {
            access_token: Some("offline-token".to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let (status, data) = send(ctx.get("/api/functions")).await;
    assert_eq!(status, StatusCode::OK);
    let functions = data.as_array().expect("array");
    assert_eq!(functions.len(), 1);
    assert_eq!(functions[0]["handle"], "test-fn");
    assert_eq!(functions[0]["type"], "cart-transform");
    assert_eq!(functions[0]["supportsUpdate"], false);
    assert_eq!(
        functions[0]["fields"],
        json!({ "note": { "type": "text", "label": "Note" } })
    );
    assert!(ctx.transport.requests().is_empty());
    ctx.shutdown().await.unwrap();
}

// ─── Tenant isolation ────────────────────────────────────────────────

#[openshop::test(api_context())]
async fn sessions_cannot_reach_runs_and_logs_of_another_shop(ctx: &TestContext) {
    let base = base_time();
    let run_id = seed_run(ctx, "test-flow", "completed", base).await;
    let active = seed_run(ctx, "test-flow", "pending", base).await;
    seed_step(&ctx.db, run_id, "step", "completed", base).await;
    seed_log(&ctx.db, run_id, "info", "private line", json!({}), base).await;
    let intruder = ctx.session_token_for(OTHER_SHOP, "2");
    let as_intruder = |method: Method, path: String| request_as(ctx, &intruder, method, &path);

    for path in ["/api/runs", "/api/runs?search=test-flow", "/api/flows/test-flow/runs"] {
        let (status, data) = send(as_intruder(Method::GET, path.to_string())).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(data, json!([]), "{path}");
    }

    for (method, path) in [
        (Method::GET, format!("/api/runs/{run_id}")),
        (Method::GET, format!("/api/runs/{run_id}/logs")),
        (Method::GET, format!("/api/runs/{run_id}/logs/export?format=csv")),
        (Method::POST, format!("/api/runs/{run_id}/retry")),
        (Method::POST, format!("/api/runs/{run_id}/retry?mode=reset")),
        (Method::DELETE, format!("/api/runs/{run_id}")),
        (Method::POST, format!("/api/runs/{active}/cancel")),
    ] {
        let (status, body) = send(as_intruder(method.clone(), path.clone())).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
        assert_eq!(body, error("Run not found"), "{method} {path}");
    }

    let (status, body) =
        send(as_intruder(Method::POST, "/api/runs/bulk-delete".to_string()).json(&json!({ "ids": [run_id, active] })))
            .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "deleted": 0, "skipped": 2 }));

    assert_eq!(run_status(&ctx.db, run_id).await, "completed");
    assert_eq!(run_status(&ctx.db, active).await, "pending");
    assert_eq!(step_names(&ctx.db, run_id).await, ["step"]);
    assert_eq!(log_messages(&ctx.db, run_id).await, ["private line"]);

    // A run triggered by the other shop belongs to it, and does not conflict with ours.
    let (status, triggered) = send(as_intruder(Method::POST, "/api/flows/test-flow/run".to_string())).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let foreign = Uuid::parse_str(triggered["runId"].as_str().unwrap()).unwrap();
    assert_eq!(find_run(&ctx.db, foreign).await.expect("run").shop, OTHER_SHOP);

    let (_, own) = send(ctx.get("/api/runs")).await;
    let mut own_ids = strings(&own, "id");
    own_ids.sort();
    let mut expected = vec![run_id.to_string(), active.to_string()];
    expected.sort();
    assert_eq!(own_ids, expected);
    let (status, _) = send(ctx.get(&format!("/api/runs/{foreign}"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[openshop::test(api_context())]
async fn sessions_cannot_reach_providers_and_crons_of_another_shop(ctx: &TestContext) {
    let key = "test-flow:*/5 * * * *";
    let intruder = ctx.session_token_for(OTHER_SHOP, "2");
    let as_intruder = |method: Method, path: &str| request_as(ctx, &intruder, method, path);

    let (status, _) = send(ctx.put("/api/providers/warehouse").json(&warehouse_credentials())).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(
        ctx.post("/api/crons/toggle")
            .json(&json!({ "key": key, "enabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The other shop sees an unconfigured provider and the cron default.
    let (status, list) = send(as_intruder(Method::GET, "/api/providers")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list[0]["config"], json!({}));
    assert_eq!(list[0]["configured"], false);
    assert_eq!(list[0]["fields"]["apiKey"]["hasValue"], false);
    let (_, crons) = send(as_intruder(Method::GET, "/api/crons")).await;
    assert_eq!(crons[0]["enabled"], true);

    // A blank password must not pick up another shop's stored secret.
    let (status, body) = send(
        as_intruder(Method::PUT, "/api/providers/warehouse")
            .json(&json!({ "config": { "endpoint": "https://intruder.test", "apiKey": "" } })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, error("Field \"apiKey\" is required"));

    // Its check runs against its own (empty) configuration and leaves ours unchecked.
    let (status, check) = send(as_intruder(Method::POST, "/api/providers/warehouse/check")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(check, json!({ "ok": false }));

    let (status, _) = send(
        as_intruder(Method::PUT, "/api/providers/warehouse")
            .json(&json!({ "config": { "endpoint": "https://intruder.test", "apiKey": "intruder" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) =
        send(as_intruder(Method::POST, "/api/crons/toggle").json(&json!({ "key": key, "enabled": true }))).await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(
        stored_provider_config(&ctx.db, &ctx.shop).await,
        warehouse_credentials()["config"]
    );
    assert_eq!(
        stored_provider_config(&ctx.db, OTHER_SHOP).await,
        json!({ "endpoint": "https://intruder.test", "apiKey": "intruder" })
    );
    let (_, own) = send(ctx.get("/api/providers")).await;
    assert_eq!(own[0]["config"], json!({ "endpoint": "https://warehouse.test" }));
    assert_eq!(own[0]["lastCheckOk"], Value::Null);
    let (_, crons) = send(ctx.get("/api/crons")).await;
    assert_eq!(crons[0]["enabled"], false);
}

#[tokio::test]
async fn sessions_cannot_reach_data_of_another_shopify_app() {
    let app = |api_key: &str, api_secret: &str| ShopifyAppConfig::Keys {
        api_key: api_key.to_string(),
        api_secret: api_secret.to_string(),
        app_url: Some(format!("https://{api_key}.example.test")),
    };
    let config = OpenShopConfig::builder()
        .shopify(ShopifyConfig {
            scopes: Some("read_products".to_string()),
            apps: Some(IndexMap::from([
                ("clientA".to_string(), app("client-a-key", "client-a-secret")),
                ("clientB".to_string(), app("client-b-key", "client-b-secret")),
            ])),
        })
        .flow(noop_flow("test-flow"))
        .provider(warehouse())
        .cron(Cron::new("*/5 * * * *", "test-flow").name("Test cron"))
        .build()
        .unwrap();
    let ctx = context(config).await;
    let key = "test-flow:*/5 * * * *";
    let token_a = session_token(&ctx.shop, "client-a-secret", "client-a-key", "1");
    let token_b = session_token(&ctx.shop, "client-b-secret", "client-b-key", "1");
    let as_a = |method: Method, path: &str| request_as(&ctx, &token_a, method, path);
    let as_b = |method: Method, path: &str| request_as(&ctx, &token_b, method, path);

    // The single-app credentials of the environment are not accepted, nor is a mixed-up secret.
    let (status, _) = send(ctx.get("/api/flows")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let forged = session_token(&ctx.shop, "client-b-secret", "client-a-key", "1");
    let (status, body) = send(request_as(&ctx, &forged, Method::GET, "/api/flows")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, error("Unauthorized: Invalid JWT signature"));

    let (status, triggered) = send(as_a(Method::POST, "/api/flows/test-flow/run")).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let run_id = Uuid::parse_str(triggered["runId"].as_str().unwrap()).unwrap();
    assert_eq!(find_run(&ctx.db, run_id).await.expect("run").app_handle, "clientA");
    seed_log(&ctx.db, run_id, "info", "private line", json!({}), base_time()).await;
    let (status, _) = send(as_a(Method::PUT, "/api/providers/warehouse").json(&warehouse_credentials())).await;
    assert_eq!(status, StatusCode::OK);
    let toggle = as_a(Method::POST, "/api/crons/toggle").json(&json!({ "key": key, "enabled": false }));
    let (status, _) = send(toggle).await;
    assert_eq!(status, StatusCode::OK);

    // Same shop, other app: nothing is visible or mutable.
    let (status, runs) = send(as_b(Method::GET, "/api/runs")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(runs, json!([]));
    for (method, path) in [
        (Method::GET, format!("/api/runs/{run_id}")),
        (Method::GET, format!("/api/runs/{run_id}/logs")),
        (Method::GET, format!("/api/runs/{run_id}/logs/export")),
        (Method::POST, format!("/api/runs/{run_id}/cancel")),
        (Method::POST, format!("/api/runs/{run_id}/retry")),
        (Method::DELETE, format!("/api/runs/{run_id}")),
    ] {
        let (status, body) = send(as_b(method.clone(), &path)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
        assert_eq!(body, error("Run not found"));
    }
    let (_, body) = send(as_b(Method::POST, "/api/runs/bulk-delete").json(&json!({ "ids": [run_id] }))).await;
    assert_eq!(body, json!({ "deleted": 0, "skipped": 1 }));

    let (_, providers) = send(as_b(Method::GET, "/api/providers")).await;
    assert_eq!(providers[0]["configured"], false);
    assert_eq!(providers[0]["config"], json!({}));
    let (_, crons) = send(as_b(Method::GET, "/api/crons")).await;
    assert_eq!(crons[0]["enabled"], true);

    // The same flow may run concurrently for the other app.
    let (status, other) = send(as_b(Method::POST, "/api/flows/test-flow/run")).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let other_run = Uuid::parse_str(other["runId"].as_str().unwrap()).unwrap();
    assert_eq!(find_run(&ctx.db, other_run).await.expect("run").app_handle, "clientB");

    // App A still sees exactly its own data.
    let (_, runs) = send(as_a(Method::GET, "/api/runs")).await;
    assert_eq!(strings(&runs, "id"), [run_id.to_string()]);
    assert_eq!(run_status(&ctx.db, run_id).await, "pending");
    let (_, logs) = send(as_a(Method::GET, &format!("/api/runs/{run_id}/logs"))).await;
    assert_eq!(strings(&logs["logs"], "message"), ["private line"]);
    let (_, providers) = send(as_a(Method::GET, "/api/providers")).await;
    assert_eq!(providers[0]["configured"], true);
    let (_, crons) = send(as_a(Method::GET, "/api/crons")).await;
    assert_eq!(crons[0]["enabled"], false);
    ctx.shutdown().await.unwrap();
}

// ─── CORS ────────────────────────────────────────────────────────────

fn allowed_origin(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get(ACCESS_CONTROL_ALLOW_ORIGIN)
        .map(|value| value.to_str().unwrap().to_string())
}

#[tokio::test]
async fn cors_allows_known_origins_on_the_api() {
    let ctx = TestContext::with_options(
        api_config(),
        TestOptions {
            host: Some("https://app.example.com/".to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    for origin in [
        "https://admin.shopify.com",
        "https://some-shop.myshopify.com",
        "https://extensions.shopifycdn.com",
        "http://localhost:3000",
        "https://app.example.com",
    ] {
        let response = ctx.get("/api/flows").header(ORIGIN, origin).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{origin}");
        assert_eq!(allowed_origin(&response).as_deref(), Some(origin));

        // Errors raised before the handler carry the header too, so that the browser can read them.
        let response = anonymous(&ctx, Method::GET, "/api/flows")
            .header(ORIGIN, origin)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{origin}");
        assert_eq!(allowed_origin(&response).as_deref(), Some(origin));
    }

    let response = ctx
        .get("/api/unknown")
        .header(ORIGIN, "https://admin.shopify.com")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(allowed_origin(&response).as_deref(), Some("https://admin.shopify.com"));
    ctx.shutdown().await.unwrap();
}

#[openshop::test(api_context())]
async fn cors_omits_headers_for_unknown_origins(ctx: &TestContext) {
    for origin in [
        "https://evil.example.com",
        "https://admin.shopify.com.evil.example.com",
        "http://some-shop.myshopify.com",
        "https://app.example.com",
        "null",
    ] {
        let response = ctx.get("/api/flows").header(ORIGIN, origin).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{origin}");
        assert_eq!(allowed_origin(&response), None, "{origin}");
    }

    // CORS is scoped to the API.
    let response = anonymous(ctx, Method::GET, "/health")
        .header(ORIGIN, "https://admin.shopify.com")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(allowed_origin(&response), None);
}

#[openshop::test(api_context())]
async fn cors_answers_preflight_requests_without_a_session(ctx: &TestContext) {
    let preflight = |origin: &str| {
        anonymous(ctx, Method::OPTIONS, "/api/providers/warehouse")
            .header(ORIGIN, origin)
            .header(ACCESS_CONTROL_REQUEST_METHOD, "PUT")
            .header(ACCESS_CONTROL_REQUEST_HEADERS, "authorization,content-type")
            .send()
    };

    let response = preflight("https://admin.shopify.com").await.unwrap();
    assert!(response.status().is_success(), "{}", response.status());
    assert_eq!(allowed_origin(&response).as_deref(), Some("https://admin.shopify.com"));
    let methods = response.headers()[ACCESS_CONTROL_ALLOW_METHODS]
        .to_str()
        .unwrap()
        .to_string();
    for method in ["GET", "HEAD", "PUT", "POST", "DELETE", "PATCH"] {
        assert!(methods.contains(method), "{methods}");
    }
    assert_eq!(
        response.headers()[ACCESS_CONTROL_ALLOW_HEADERS],
        "authorization,content-type"
    );
    assert!(response.text().await.unwrap().is_empty());

    let response = preflight("https://evil.example.com").await.unwrap();
    assert_eq!(allowed_origin(&response), None);
}

#[tokio::test]
async fn cors_is_only_enabled_without_ui_or_in_dev_mode() {
    let origin = "https://admin.shopify.com";
    for (server, expected) in [
        (ServerOptions::default(), Some(origin)),
        (ServerOptions::production(), None),
        (ServerOptions::development(), Some(origin)),
    ] {
        let ctx = TestContext::with_options(
            api_config(),
            TestOptions {
                server: server.clone(),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let response = ctx.get("/api/flows").header(ORIGIN, origin).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{server:?}");
        assert_eq!(allowed_origin(&response).as_deref(), expected, "{server:?}");

        let response = anonymous(&ctx, Method::GET, "/api/flows")
            .header(ORIGIN, origin)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{server:?}");
        assert_eq!(allowed_origin(&response).as_deref(), expected, "{server:?}");
        ctx.shutdown().await.unwrap();
    }
}
