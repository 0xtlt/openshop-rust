//! The Admin API: flows, runs, logs, crons and providers, scoped to the session's shop and app.

mod common;

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, SubsecRound, Utc};
use common::{LogCapture, create_config, find_run, noop_flow, run_exists, send, two_shopify_apps};
use openshop::builder::{Field, FieldOption, Flow, Function, FunctionType, Provider};
use openshop::engine::abort::register_abort;
use openshop::engine::{RunFlowOptions, run_flow};
use openshop::runtime::time::to_iso;
use openshop::server::{ServerOptions, UiSource};
use openshop::testing::{TestContext, TestOptions, session_token};
use openshop::{
    Concurrency, Cron, DispatchOptions, FlowCanceledError, FlowContext, OpenShopConfig, PgPool, RetryPolicy,
    RunOutcome, Value, anyhow, json, validators,
};
use reqwest::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    ACCESS_CONTROL_REQUEST_HEADERS, ACCESS_CONTROL_REQUEST_METHOD, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE,
    ORIGIN,
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

async fn run_status(db: &PgPool, run_id: Uuid) -> String {
    find_run(db, run_id).await.status
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

/// A transaction that has changed a run and not committed yet: the requests
/// that write the run wait for it. Committing it plays a concurrent request
/// that won the race.
struct RaceWinner {
    tx: sqlx::Transaction<'static, sqlx::Postgres>,
    pid: i32,
}

/// Starts the transaction with a statement over the run (`$1`).
async fn race_winner(ctx: &TestContext, run_id: Uuid, statement: &str) -> RaceWinner {
    let mut tx = ctx.db.begin().await.unwrap();
    let (pid,): (i32,) = sqlx::query_as("SELECT pg_backend_pid()")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    sqlx::query(statement).bind(run_id).execute(&mut *tx).await.unwrap();
    RaceWinner { tx, pid }
}

/// Waits until a statement is stuck behind the backend `pid`.
async fn wait_until_blocked_by(db: &PgPool, pid: i32) {
    for _ in 0..500 {
        let (waiting,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid))")
                .bind(pid)
                .fetch_one(db)
                .await
                .unwrap();
        if waiting > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("nothing waits for backend {pid}");
}

impl RaceWinner {
    /// Waits until a request is stuck behind this transaction, then commits it.
    async fn win(self, db: &PgPool) {
        wait_until_blocked_by(db, self.pid).await;
        self.tx.commit().await.unwrap();
    }
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

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct SyncInput {
    since: String,
    limit: Option<u32>,
}

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

#[openshop::test(api_context())]
async fn post_crons_toggle_ignores_what_is_not_a_cron_of_the_app(ctx: &TestContext) {
    // A key of a schedule that was changed, or invented: it would be stored and never read.
    let (status, response) = send(
        ctx.post("/api/crons/toggle")
            .json(&json!({ "key": "test-flow:0 0 * * *", "enabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(response, error("Cron not found"));

    let (overrides,): (i64,) = sqlx::query_as("SELECT count(*) FROM cron_overrides")
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(overrides, 0);
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
async fn run_search_is_text_not_a_pattern(ctx: &TestContext) {
    let base = base_time();
    let percent = seed_run(ctx, "ship-100%-done", "completed", base).await;
    let underscore = seed_run(ctx, "ship_it", "completed", base).await;
    let backslash = seed_run(ctx, "ship\\it", "completed", base).await;
    let plain = seed_run(ctx, "shipXit", "completed", base).await;
    let _ = plain;

    for (search, expected) in [
        ("%", percent),
        ("100%", percent),
        ("p_i", underscore),
        ("\\", backslash),
        ("p\\i", backslash),
    ] {
        let (status, data) = send(ctx.get("/api/runs").query(&[("search", search)])).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(strings(&data, "id"), [expected.to_string()], "search={search}");
    }
}

#[openshop::test(api_context())]
async fn run_list_refuses_an_offset_past_the_cap(ctx: &TestContext) {
    for path in ["/api/runs", "/api/flows/test-flow/runs"] {
        let (status, data) = send(ctx.get(&format!("{path}?offset=10000"))).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(data, json!([]));
        let (status, data) = send(ctx.get(&format!("{path}?offset=10001"))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(data, error("offset must be at most 10000"));
    }
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

    let run = find_run(&ctx.db, run_id).await;
    assert_eq!(run.status, "pending");
    assert_eq!(run.flow_name, "test-flow");
    assert_eq!(run.shop, ctx.shop);
    assert_eq!(run.app_handle, "default");
    assert_eq!(run.input, Some(json!({ "source": "admin" })));
}

#[openshop::test(api_context())]
async fn post_flow_run_rejects_unknown_flow(ctx: &TestContext) {
    let (status, body) = send(ctx.post("/api/flows/nope/run").json(&json!({ "input": {} }))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, error("Flow \"nope\" not found"));

    let (runs,): (i64,) = sqlx::query_as("SELECT count(*) FROM flow_runs")
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(runs, 0);
}

/// A flow whose input is `{ "orderId": <number> }`.
fn order_flow() -> Flow {
    noop_flow("order-flow")
        .concurrency(Concurrency::Allow)
        .input_schema(openshop::builder::InputSchema::custom(
            json!({ "type": "object", "required": ["orderId"] }),
            |value| match value.get("orderId") {
                Some(Value::Number(_)) => Ok(value),
                _ => Err("orderId must be a number".to_string()),
            },
        ))
}

#[openshop::test(context(create_config([order_flow()])))]
async fn post_flow_run_rejects_input_the_flow_refuses(ctx: &TestContext) {
    for input in [json!({}), json!({ "orderId": "12" }), Value::Null] {
        let (status, body) = send(ctx.post("/api/flows/order-flow/run").json(&json!({ "input": input }))).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{input}");
        assert_eq!(
            body,
            error("Invalid input for flow \"order-flow\": orderId must be a number")
        );
    }
    // No input at all is `{}`, which the flow refuses too.
    let (status, _) = send(ctx.post("/api/flows/order-flow/run")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (runs,): (i64,) = sqlx::query_as("SELECT count(*) FROM flow_runs")
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(runs, 0);

    let (status, data) = send(
        ctx.post("/api/flows/order-flow/run")
            .json(&json!({ "input": { "orderId": 12 } })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let run_id = Uuid::parse_str(data["runId"].as_str().expect("runId")).unwrap();
    assert_eq!(find_run(&ctx.db, run_id).await.input, Some(json!({ "orderId": 12 })));
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
        assert_eq!(find_run(&ctx.db, run_id).await.input, Some(json!({})));
    }
}

#[tokio::test]
async fn flow_names_are_decoded_from_the_path() {
    let name = "orders/sync now";
    let ctx = context(create_config([noop_flow(name)])).await;

    let (status, data) = send(ctx.post("/api/flows/orders%2Fsync%20now/run")).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let run_id = Uuid::parse_str(data["runId"].as_str().expect("runId")).unwrap();
    assert_eq!(find_run(&ctx.db, run_id).await.flow_name, name);

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

    let run = find_run(&ctx.db, run_id).await;
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

    let run = find_run(&ctx.db, run_id).await;
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
    assert!(matches!(result, RunOutcome::Failed { .. }), "{result:?}");
    assert_eq!(run_status(&ctx.db, run_id).await, "failed");
    assert_eq!(step_names(&ctx.db, run_id).await, ["fetch", "push"]);

    let before = Utc::now();
    let (status, body) = send(ctx.post(&format!("/api/runs/{run_id}/retry"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["mode"], "resume");
    assert_eq!(step_names(&ctx.db, run_id).await, ["fetch"]);

    let run = find_run(&ctx.db, run_id).await;
    assert_eq!(run.status, "pending");
    assert_eq!(run.error, None);
    let deadline_in = (run.deadline_at.expect("deadline from the flow timeout") - before).num_seconds();
    assert!((55..=65).contains(&deadline_in), "{deadline_in}");

    let result = execute(&ctx, run_id, "test-flow").await;
    assert!(matches!(result, RunOutcome::Failed { .. }), "{result:?}");
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

    // Another flow, so that the active runs do not make `test-flow` refuse the
    // retries below.
    for status in ["pending", "running", "sleeping"] {
        let run_id = seed_run(ctx, "other-flow", status, base).await;
        let (code, body) = send(ctx.post(&format!("/api/runs/{run_id}/retry"))).await;
        assert_eq!(code, StatusCode::CONFLICT);
        assert_eq!(body, error(&format!("Cannot retry a run with status \"{status}\"")));
        assert_eq!(run_status(&ctx.db, run_id).await, status);
    }
    // One flow each: a retried run is active, and `test-flow` rejects a second one.
    for (flow_name, status) in [("test-flow", "completed"), ("third-flow", "canceled")] {
        let run_id = seed_run(ctx, flow_name, status, base).await;
        let (code, _) = send(ctx.post(&format!("/api/runs/{run_id}/retry"))).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(run_status(&ctx.db, run_id).await, "pending");
    }
}

// A request reads the run, then writes it. These tests let another transaction
// change the run in between, and check that the request notices.

#[openshop::test(api_context())]
async fn post_run_cancel_does_not_overwrite_a_run_that_completed_meanwhile(ctx: &TestContext) {
    let run_id = seed_run(ctx, "test-flow", "running", base_time()).await;
    let abort = register_abort(&run_id.to_string(), None);
    let winner = race_winner(
        ctx,
        run_id,
        "UPDATE flow_runs SET status = 'completed', completed_at = now() WHERE id = $1",
    )
    .await;

    let request = tokio::spawn(send(ctx.post(&format!("/api/runs/{run_id}/cancel"))));
    winner.win(&ctx.db).await;
    let (code, body) = request.await.unwrap();

    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(body, error("Cannot cancel a run with status \"completed\""));
    assert_eq!(run_status(&ctx.db, run_id).await, "completed");
    assert!(
        !abort.token.is_cancelled(),
        "the worker of a run that completed is left alone"
    );
    abort.cleanup();
}

/// A cancel tells the worker while it still holds the row of the run, before
/// the cancellation is visible: nobody can retry the run, and see a worker claim
/// it again, in between, so the signal cannot reach a new execution.
/// The commit of the cancellation is held back by a deferred trigger that waits
/// for an advisory lock of the test.
#[openshop::test(api_context())]
async fn post_run_cancel_signals_the_worker_before_the_cancellation_is_visible(ctx: &TestContext) {
    let run_id = seed_run(ctx, "test-flow", "running", base_time()).await;
    let abort = register_abort(&run_id.to_string(), None);
    let key = i64::from(Uuid::new_v4().as_u128() as u32);
    sqlx::raw_sql(&format!(
        "CREATE FUNCTION hold_cancellation() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN PERFORM pg_advisory_lock({key}); PERFORM pg_advisory_unlock({key}); RETURN NULL; END $$; \
         CREATE CONSTRAINT TRIGGER hold_cancellation AFTER UPDATE ON flow_runs \
           DEFERRABLE INITIALLY DEFERRED FOR EACH ROW WHEN (NEW.status = 'canceled') \
           EXECUTE FUNCTION hold_cancellation();"
    ))
    .execute(&ctx.db)
    .await
    .unwrap();
    let mut holder = ctx.db.acquire().await.unwrap();
    let (pid,): (i32,) = sqlx::query_as("SELECT pg_backend_pid()")
        .fetch_one(&mut *holder)
        .await
        .unwrap();
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(key)
        .execute(&mut *holder)
        .await
        .unwrap();

    let canceling = tokio::spawn(send(ctx.post(&format!("/api/runs/{run_id}/cancel"))));
    wait_until_blocked_by(&ctx.db, pid).await;
    assert!(abort.token.is_cancelled(), "the worker is told before the commit");
    assert_eq!(run_status(&ctx.db, run_id).await, "running", "not visible yet");

    // A retry cannot slip in before the cancellation: the run is still running.
    let (code, body) = send(ctx.post(&format!("/api/runs/{run_id}/retry"))).await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(body, error("Cannot retry a run with status \"running\""));

    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(key)
        .execute(&mut *holder)
        .await
        .unwrap();
    drop(holder);
    let (code, body) = canceling.await.unwrap();
    assert_eq!((code, body), (200, json!({ "ok": true })));
    assert_eq!(run_status(&ctx.db, run_id).await, "canceled");
}

#[openshop::test(api_context())]
async fn post_run_retry_of_a_run_deleted_meanwhile_is_not_found(ctx: &TestContext) {
    let run_id = seed_run(ctx, "test-flow", "completed", base_time()).await;
    let winner = race_winner(ctx, run_id, "DELETE FROM flow_runs WHERE id = $1").await;

    let request = tokio::spawn(send(ctx.post(&format!("/api/runs/{run_id}/retry"))));
    winner.win(&ctx.db).await;
    let (code, body) = request.await.unwrap();

    assert_eq!(code, StatusCode::NOT_FOUND);
    assert_eq!(body, error("Run not found"));
    assert!(!run_exists(&ctx.db, run_id).await);
}

#[openshop::test(api_context())]
async fn post_run_retry_does_not_reset_a_run_that_started_again_meanwhile(ctx: &TestContext) {
    let base = base_time();
    let run_id = seed_run(ctx, "test-flow", "failed", base).await;
    seed_step(&ctx.db, run_id, "push", "failed", base).await;
    let winner = race_winner(
        ctx,
        run_id,
        "UPDATE flow_runs SET status = 'running', worker_id = 'w1', attempts = 3 WHERE id = $1",
    )
    .await;

    let request = tokio::spawn(send(ctx.post(&format!("/api/runs/{run_id}/retry"))));
    winner.win(&ctx.db).await;
    let (code, body) = request.await.unwrap();

    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(body, error("Cannot retry a run with status \"running\""));
    let run = find_run(&ctx.db, run_id).await;
    assert_eq!((run.status.as_str(), run.attempts), ("running", 3));
    assert_eq!(run.worker_id.as_deref(), Some("w1"));
    assert_eq!(step_names(&ctx.db, run_id).await, ["push"], "the steps are kept");
    assert!(log_messages(&ctx.db, run_id).await.is_empty());
}

#[openshop::test(api_context())]
async fn delete_run_does_not_remove_a_run_that_started_meanwhile(ctx: &TestContext) {
    let base = base_time();
    let run_id = seed_run(ctx, "test-flow", "completed", base).await;
    seed_step(&ctx.db, run_id, "step", "completed", base).await;
    seed_log(&ctx.db, run_id, "info", "line", json!({}), base).await;
    let winner = race_winner(
        ctx,
        run_id,
        "UPDATE flow_runs SET status = 'running', worker_id = 'w1' WHERE id = $1",
    )
    .await;

    let request = tokio::spawn(send(ctx.delete(&format!("/api/runs/{run_id}"))));
    winner.win(&ctx.db).await;
    let (code, body) = request.await.unwrap();

    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(body, error("Cannot delete an active run — cancel it first"));
    assert_eq!(run_status(&ctx.db, run_id).await, "running");
    assert_eq!(step_names(&ctx.db, run_id).await, ["step"]);
    assert_eq!(log_messages(&ctx.db, run_id).await, ["line"]);
}

#[openshop::test(api_context())]
async fn post_runs_bulk_delete_skips_a_run_that_started_meanwhile(ctx: &TestContext) {
    let base = base_time();
    let started = seed_run(ctx, "test-flow", "completed", base).await;
    let finished = seed_run(ctx, "test-flow", "failed", base).await;
    for id in [started, finished] {
        seed_step(&ctx.db, id, "step", "completed", base).await;
    }
    let winner = race_winner(
        ctx,
        started,
        "UPDATE flow_runs SET status = 'running', worker_id = 'w1' WHERE id = $1",
    )
    .await;

    let request = tokio::spawn(send(
        ctx.post("/api/runs/bulk-delete")
            .json(&json!({ "ids": [started, finished] })),
    ));
    winner.win(&ctx.db).await;
    let (code, body) = request.await.unwrap();

    assert_eq!(code, StatusCode::OK);
    assert_eq!(body, json!({ "deleted": 1, "skipped": 1 }));
    assert_eq!(run_status(&ctx.db, started).await, "running");
    assert_eq!(step_names(&ctx.db, started).await, ["step"]);
    assert!(!run_exists(&ctx.db, finished).await);
    assert!(step_names(&ctx.db, finished).await.is_empty());
}

#[openshop::test(api_context())]
async fn post_run_retry_refuses_what_the_concurrency_of_the_flow_refuses(ctx: &TestContext) {
    let base = base_time();
    let failed = seed_run(ctx, "test-flow", "failed", base).await;
    seed_step(&ctx.db, failed, "push", "failed", base).await;
    let active = seed_run(ctx, "test-flow", "sleeping", base).await;
    let other_shop = seed_run_for(&ctx.db, "default", OTHER_SHOP, "test-flow", "running", base).await;

    // `test-flow` rejects concurrent runs by default, like a dispatch does.
    let (code, body) = send(ctx.post(&format!("/api/runs/{failed}/retry"))).await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(
        body,
        json!({
            "error": format!("Flow \"test-flow\" is already running for shop \"{}\" (run #{active})", ctx.shop),
            "existingRunId": active,
        })
    );
    assert_eq!(run_status(&ctx.db, failed).await, "failed");
    assert_eq!(step_names(&ctx.db, failed).await, ["push"], "nothing was changed");
    assert!(log_messages(&ctx.db, failed).await.is_empty());

    // The run of the shop is the only one in the way: not the run of another shop.
    sqlx::query("UPDATE flow_runs SET status = 'completed' WHERE id = $1")
        .bind(active)
        .execute(&ctx.db)
        .await
        .unwrap();
    let (code, _) = send(ctx.post(&format!("/api/runs/{failed}/retry"))).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(run_status(&ctx.db, failed).await, "pending");
    assert_eq!(run_status(&ctx.db, other_shop).await, "running");

    // Now `failed` is the active run: another finished run cannot be retried.
    let (code, body) = send(ctx.post(&format!("/api/runs/{active}/retry"))).await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(body["existingRunId"], json!(failed));
    assert_eq!(run_status(&ctx.db, active).await, "completed");
}

#[tokio::test]
async fn post_run_retry_of_a_flow_that_allows_concurrency_runs_beside_an_active_run() {
    let ctx = context(api_config_with(noop_flow("test-flow").concurrency(Concurrency::Allow))).await;
    let base = base_time();
    let failed = seed_run(&ctx, "test-flow", "failed", base).await;
    let active = seed_run(&ctx, "test-flow", "running", base).await;

    let (code, _) = send(ctx.post(&format!("/api/runs/{failed}/retry"))).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(run_status(&ctx.db, failed).await, "pending");
    assert_eq!(run_status(&ctx.db, active).await, "running");
    ctx.shutdown().await.unwrap();
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

    assert!(!run_exists(&ctx.db, run_id).await);
    assert!(step_names(&ctx.db, run_id).await.is_empty());
    assert!(log_messages(&ctx.db, run_id).await.is_empty());

    assert!(run_exists(&ctx.db, kept).await);
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
        assert!(run_exists(&ctx.db, run_id).await);
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
        assert!(!run_exists(&ctx.db, id).await);
        assert!(step_names(&ctx.db, id).await.is_empty());
        assert!(log_messages(&ctx.db, id).await.is_empty());
    }
    for id in [pending, untouched] {
        assert!(run_exists(&ctx.db, id).await);
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

    assert!(run_exists(&ctx.db, run_id).await);

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
        (Method::GET, "/api/installations/active"),
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

    let cases: [(&str, &[&str]); 8] = [
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
async fn get_run_logs_refuses_a_query_it_cannot_run(ctx: &TestContext) {
    let (run_id, _) = seed_order_logs(ctx).await;
    let too_long = format!("|= \"{}\"", "a".repeat(2_000));
    let too_many = r#"|= "a" "#.repeat(21);
    let cases = [
        (r#"|~ "(""#.to_string(), "Invalid regex \"(\": "),
        (r#"|~ "[invalid""#.to_string(), "Invalid regex \"[invalid\": "),
        (
            r#"|~ "(a+)+""#.to_string(),
            "Invalid regex \"(a+)+\": it repeats a repeated group",
        ),
        (r#"|~ "(o)\1""#.to_string(), "it uses a backreference"),
        (
            format!("|~ \"{}\"", "o".repeat(121)),
            "it is longer than 120 characters",
        ),
        (too_long, "Log query is longer than 2000 characters"),
        (too_many, "Too many filters: 20 at most, got 21"),
    ];
    for (query, expected) in cases {
        for path in [
            format!("/api/runs/{run_id}/logs"),
            format!("/api/runs/{run_id}/logs/export"),
        ] {
            let (status, body) = send(ctx.get(&path).query(&[("q", query.as_str())])).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path} q={query}");
            let message = body["error"].as_str().unwrap_or_else(|| panic!("{body}"));
            assert!(message.contains(expected), "{message} should contain {expected}");
            assert!(!message.contains('\n'), "{message}");
        }
    }
}

#[openshop::test(api_context())]
async fn get_run_logs_refuses_a_last_beyond_the_calendar(ctx: &TestContext) {
    let (run_id, _) = seed_order_logs(ctx).await;
    for query in ["last:9223372036854775807s", "last:99999999999999999999999999d"] {
        let (status, body) = send(ctx.get(&format!("/api/runs/{run_id}/logs")).query(&[("q", query)])).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
        assert!(
            body["error"].as_str().unwrap().contains("further than the calendar"),
            "{body}"
        );
    }
    // The server is still up.
    let (status, _) = send(ctx.get(&format!("/api/runs/{run_id}/logs"))).await;
    assert_eq!(status, StatusCode::OK);
}

#[openshop::test(api_context())]
async fn get_run_logs_caps_the_context_around_matches(ctx: &TestContext) {
    let base = base_time();
    let run_id = seed_run(ctx, "test-flow", "completed", base).await;
    for index in 0..300 {
        let at = base + chrono::Duration::seconds(index);
        seed_log(&ctx.db, run_id, "info", &format!("line {index:03}"), json!({}), at).await;
    }
    // Newest first: the match is the 150th newest line, line 150.
    for query in ["line 150 C:100000", "line 150 C:99999999999999999999999999"] {
        let (status, data) = send(ctx.get(&format!("/api/runs/{run_id}/logs")).query(&[("q", query)])).await;
        assert_eq!(status, StatusCode::OK, "{query}");
        assert_eq!(data["logs"].as_array().unwrap().len(), 201, "{query}");
    }
    let (_, data) = send(
        ctx.get(&format!("/api/runs/{run_id}/logs"))
            .query(&[("q", "line 150 B:2 A:1")]),
    )
    .await;
    assert_eq!(data["logs"].as_array().unwrap().len(), 4);
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
        ("xml", "json"),
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

/// Follows `next` from the first page until it is null, with the query given.
async fn read_log_pages(ctx: &TestContext, run_id: Uuid, params: &[(&str, &str)]) -> Vec<Value> {
    let mut pages = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut query: Vec<(&str, &str)> = params.to_vec();
        if let Some(cursor) = &cursor {
            query.push(("cursor", cursor));
        }
        let (status, page) = send(ctx.get(&format!("/api/runs/{run_id}/logs")).query(&query)).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        cursor = page["next"].as_str().map(str::to_string);
        pages.push(page);
        assert!(pages.len() < 100, "next never ends");
        if cursor.is_none() {
            return pages;
        }
    }
}

fn log_ids(pages: &[Value]) -> Vec<String> {
    pages
        .iter()
        .flat_map(|page| page["logs"].as_array().unwrap())
        .map(|log| log["id"].as_str().unwrap().to_string())
        .collect()
}

#[openshop::test(api_context())]
async fn get_run_logs_pages_with_a_cursor(ctx: &TestContext) {
    let base = base_time();
    let run_id = seed_run(ctx, "test-flow", "completed", base).await;
    // Lines that share a timestamp, to check that the order is total.
    for index in 0..25 {
        let at = base + chrono::Duration::seconds(index / 5);
        seed_log(&ctx.db, run_id, "info", &format!("line {index}"), json!({}), at).await;
    }

    let (_, everything) = send(ctx.get(&format!("/api/runs/{run_id}/logs?limit=100"))).await;
    assert_eq!(everything["logs"].as_array().unwrap().len(), 25);
    assert!(everything["next"].is_null());
    assert_eq!(everything["total"], 25);

    let pages = read_log_pages(ctx, run_id, &[("limit", "10")]).await;
    let sizes: Vec<usize> = pages
        .iter()
        .map(|page| page["logs"].as_array().unwrap().len())
        .collect();
    assert_eq!(sizes, [10, 10, 5]);
    assert!(pages.iter().all(|page| page["total"] == 25));
    assert_eq!(log_ids(&pages), log_ids(std::slice::from_ref(&everything)));

    // An exact multiple of the limit ends without an empty page.
    let pages = read_log_pages(ctx, run_id, &[("limit", "5")]).await;
    assert_eq!(pages.len(), 5);

    // The default and the cap of the limit: nothing to refuse, nothing unbounded.
    let (status, defaults) = send(ctx.get(&format!("/api/runs/{run_id}/logs?limit=abc"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(defaults["logs"].as_array().unwrap().len(), 25);
    let (status, _) = send(ctx.get(&format!("/api/runs/{run_id}/logs?limit=999999999"))).await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send(ctx.get(&format!("/api/runs/{run_id}/logs?cursor=nope"))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, error("Invalid cursor"));
}

#[openshop::test(api_context())]
async fn get_run_logs_counts_the_lines_of_each_level(ctx: &TestContext) {
    let (run_id, _) = seed_order_logs(ctx).await;
    let (_, data) = send(ctx.get(&format!("/api/runs/{run_id}/logs?levels=error&limit=1"))).await;
    assert_eq!(data["levelCounts"], json!({ "info": 4, "warn": 1, "error": 1 }));
    assert_eq!(data["total"], 6);
}

/// A page is bounded by its limit, whatever the matches and the context asked
/// for, and the pages put end to end are the list of one unbounded request.
#[openshop::test(api_context())]
async fn get_run_logs_pages_are_bounded_with_consecutive_matches_and_wide_context(ctx: &TestContext) {
    let base = base_time();
    let run_id = seed_run(ctx, "test-flow", "completed", base).await;
    // Newest first, the lines are `line 60` down to `line 1`; every 7th is marked.
    for index in 1..=60 {
        let message = if index % 7 == 0 {
            format!("line {index} mark")
        } else {
            format!("line {index}")
        };
        let at = base + chrono::Duration::seconds(index);
        seed_log(&ctx.db, run_id, "info", &message, json!({}), at).await;
    }

    // What the query shows, from the definition: a match, and `before` lines
    // newer / `after` lines older around it (the list runs newest first).
    let expected = |marked_only: bool, before: usize, after: usize| -> Vec<String> {
        let lines: Vec<(String, bool)> = (1..=60)
            .rev()
            .map(|index| (format!("line {index}"), !marked_only || index % 7 == 0))
            .collect();
        let mut visible = vec![false; lines.len()];
        for (position, (_, matched)) in lines.iter().enumerate() {
            if *matched {
                let start = position.saturating_sub(before);
                let end = (position + after).min(lines.len() - 1);
                visible[start..=end].iter_mut().for_each(|flag| *flag = true);
            }
        }
        lines
            .into_iter()
            .zip(visible)
            .filter(|(_, visible)| *visible)
            .map(|(line, _)| line.0)
            .collect()
    };
    let names = |pages: &[Value]| -> Vec<String> {
        pages
            .iter()
            .flat_map(|page| strings(&page["logs"], "message"))
            .map(|message| message.trim_end_matches(" mark").to_string())
            .collect()
    };

    let cases = [
        // (query, marked only, before, after)
        (r#"|= "line" A:1"#, false, 0, 1),
        (r#"|= "line" C:3"#, false, 3, 3),
        (r#"|= "line" A:100000"#, false, 0, 100_000),
        (r#"|= "line" B:100000 A:100000"#, false, 1_000, 100_000),
        (r#"|= "mark" A:1"#, true, 0, 1),
        (r#"|= "mark" B:2 A:4"#, true, 2, 4),
        (r#"|= "mark" C:100000"#, true, 1_000, 100_000),
        (r#"|= "mark""#, true, 0, 0),
    ];
    for (query, marked_only, before, after) in cases {
        let want = expected(marked_only, before, after);
        for limit in [1_usize, 2, 3, 7, 25] {
            let pages = read_log_pages(ctx, run_id, &[("q", query), ("limit", &limit.to_string())]).await;
            // A match is never split from the lines before it, which can be more than the limit.
            let most = limit.max(before + 1);
            for page in &pages {
                let size = page["logs"].as_array().unwrap().len();
                assert!(size <= most, "q={query} limit={limit}: {size} lines");
            }
            assert_eq!(names(&pages), want, "q={query} limit={limit}");
        }
        let (_, all) = send(
            ctx.get(&format!("/api/runs/{run_id}/logs"))
                .query(&[("q", query), ("limit", "5000")]),
        )
        .await;
        assert_eq!(names(&[all]), want, "q={query} unbounded");
    }
}

#[openshop::test(api_context())]
async fn get_run_logs_pages_through_a_query_and_its_context(ctx: &TestContext) {
    let (run_id, _) = seed_order_logs(ctx).await;
    let messages = |pages: &[Value]| -> Vec<String> {
        pages
            .iter()
            .flat_map(|page| strings(&page["logs"], "message"))
            .collect()
    };

    let pages = read_log_pages(ctx, run_id, &[("q", "synced"), ("limit", "1")]).await;
    assert_eq!(messages(&pages), ["Order 1003 synced", "Order 1001 synced"]);
    assert_eq!(pages.len(), 2);

    // Whatever the limit, the pages put end to end are the list of one request,
    // context lines included.
    let query = "synced C:1";
    let (_, all) = send(
        ctx.get(&format!("/api/runs/{run_id}/logs"))
            .query(&[("q", query), ("limit", "100")]),
    )
    .await;
    assert_eq!(all["logs"].as_array().unwrap().len(), 6);
    for limit in ["1", "2", "3", "4"] {
        let pages = read_log_pages(ctx, run_id, &[("q", query), ("limit", limit)]).await;
        assert_eq!(log_ids(&pages), log_ids(std::slice::from_ref(&all)), "limit={limit}");
        let flags: Vec<&Value> = pages
            .iter()
            .flat_map(|page| page["logs"].as_array().unwrap())
            .map(|log| &log["_matched"])
            .collect();
        let expected: Vec<&Value> = all["logs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|log| &log["_matched"])
            .collect();
        assert_eq!(flags, expected, "limit={limit}");
    }
}

/// Inserts `count` lines for a run in one statement, newest last.
async fn seed_many_logs(ctx: &TestContext, run_id: Uuid, count: i32) {
    sqlx::query(
        "INSERT INTO logs (flow_run_id, level, message, payload, created_at) \
         SELECT $1, CASE WHEN n % 10 = 0 THEN 'error' ELSE 'info' END, 'line ' || n, \
                json_build_object('n', n), $2::timestamptz + n * interval '1 millisecond' \
         FROM generate_series(1, $3) AS n",
    )
    .bind(run_id.to_string())
    .bind(base_time())
    .bind(count)
    .execute(&ctx.db)
    .await
    .unwrap();
}

#[openshop::test(api_context())]
async fn get_run_logs_export_streams_a_large_run(ctx: &TestContext) {
    let run_id = seed_run(ctx, "test-flow", "completed", base_time()).await;
    seed_many_logs(ctx, run_id, 5_000).await;

    let response = ctx
        .get(&format!("/api/runs/{run_id}/logs/export?format=csv"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get(CONTENT_LENGTH).is_none(), "streamed");
    let text = response.text().await.unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 5_001);
    assert_eq!(lines[0], "id,level,message,payload,createdAt");
    assert!(lines[1].contains("\"line 5000\""), "{}", lines[1]);
    assert!(lines[5_000].contains("\"line 1\""), "{}", lines[5_000]);

    let response = ctx
        .get(&format!("/api/runs/{run_id}/logs/export?levels=error"))
        .send()
        .await
        .unwrap();
    let logs: Value = response.json().await.unwrap();
    let logs = logs.as_array().unwrap();
    assert_eq!(logs.len(), 500);
    assert_eq!(logs[0]["message"], "line 5000");
    assert_eq!(logs[499]["message"], "line 10");

    // A query filters the stream too, in the pages of the scan.
    let response = ctx
        .get(&format!("/api/runs/{run_id}/logs/export"))
        .query(&[("q", "line 4999"), ("format", "csv")])
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap().lines().count(), 2);

    // Nothing to export is still a valid file.
    let empty = seed_run(ctx, "test-flow", "completed", base_time()).await;
    let response = ctx.get(&format!("/api/runs/{empty}/logs/export")).send().await.unwrap();
    assert_eq!(response.text().await.unwrap(), "[]");
    let response = ctx
        .get(&format!("/api/runs/{empty}/logs/export?format=csv"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "id,level,message,payload,createdAt");
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
async fn a_malformed_encryption_key_refuses_to_store_secrets() {
    let ctx = TestContext::with_options(
        api_config(),
        TestOptions {
            encryption_key: Some("too-short".to_string()),
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
            Field::number("Retries").validate(validators::range(Some(0.0), Some(10.0))),
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
async fn put_providers_checks_each_value_against_its_field_type() {
    let erp = Provider::new("erp")
        .field("endpoint", Field::text("Endpoint").validate(validators::url()))
        .field(
            "callback",
            Field::text("Callback").optional().validate(validators::public_url()),
        )
        .field(
            "mode",
            Field::select(
                "Mode",
                [FieldOption::new("Live", "live"), FieldOption::new("Test", "test")],
            ),
        )
        .field("retries", Field::number("Retries").optional())
        .field("sandbox", Field::checkbox("Sandbox").optional());
    let config = OpenShopConfig::builder()
        .flow(noop_flow("test-flow"))
        .provider(erp)
        .build()
        .unwrap();
    let ctx = context(config).await;
    let valid = json!({ "endpoint": "https://erp.test", "mode": "live" });
    let save = |changes: Value| {
        let mut config = valid.clone();
        config
            .as_object_mut()
            .unwrap()
            .extend(changes.as_object().unwrap().clone());
        send(ctx.put("/api/providers/erp").json(&json!({ "config": config })))
    };

    for (changes, message) in [
        (
            json!({ "mode": "prod" }),
            "Field \"mode\": must be one of \"live\", \"test\" (was \"prod\")",
        ),
        (
            json!({ "retries": "abc" }),
            "Field \"retries\": must be a number (was \"abc\")",
        ),
        (
            json!({ "sandbox": "yes" }),
            "Field \"sandbox\": must be a boolean (was \"yes\")",
        ),
        (
            json!({ "endpoint": "file:///etc/passwd" }),
            "Field \"endpoint\": must be an http or https URL (was \"file:///etc/passwd\")",
        ),
        (
            json!({ "endpoint": "gopher://erp.test:70/_x" }),
            "Field \"endpoint\": must be an http or https URL (was \"gopher://erp.test:70/_x\")",
        ),
        (
            json!({ "callback": "http://169.254.169.254/latest/meta-data/" }),
            "Field \"callback\": must be a public http or https URL (was \"http://169.254.169.254/latest/meta-data/\")",
        ),
    ] {
        let (status, body) = save(changes.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{changes}");
        assert_eq!(body, error(message));
    }
    let (rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM provider_configs")
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(rows, 0);

    let (status, _) = save(json!({
        "retries": "2",
        "sandbox": "false",
        "callback": "https://hooks.example.com/erp",
        "unknown": "ignored",
    }))
    .await;
    assert_eq!(status, StatusCode::OK);
    let (stored,): (Value,) = sqlx::query_as("SELECT config FROM provider_configs WHERE provider_name = 'erp'")
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(
        stored,
        json!({
            "endpoint": "https://erp.test",
            "callback": "https://hooks.example.com/erp",
            "mode": "live",
            "retries": 2,
            "sandbox": false,
        })
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn put_providers_never_repeats_a_secret_in_an_error() {
    // The transformer stores the token as an object, as a provider that keeps
    // an OAuth token might do.
    let plain = Provider::new("plain")
        .field("endpoint", Field::text("Endpoint"))
        .field("token", Field::password("Token"))
        .transformer(|mut data| {
            if let Some(Value::String(token)) = data.get("token").cloned() {
                data.insert("token".into(), json!({ "accessToken": token }));
            }
            Value::Object(data)
        });
    let config = OpenShopConfig::builder()
        .flow(noop_flow("test-flow"))
        .provider(plain)
        .build()
        .unwrap();
    let ctx = TestContext::with_options(
        config,
        TestOptions {
            encryption_key: Some(ENCRYPTION_KEY.to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // Not a string: the error does not say what it was.
    let (status, body) = send(
        ctx.put("/api/providers/plain")
            .json(&json!({ "config": { "endpoint": "https://erp.test", "token": { "accessToken": "secret-token" } } })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, error("Field \"token\": must be a string"));

    // A blank password keeps the stored one, whatever shape the transformer gave it.
    let (status, _) = send(
        ctx.put("/api/providers/plain")
            .json(&json!({ "config": { "endpoint": "https://erp.test", "token": "secret-token" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = send(
        ctx.put("/api/providers/plain")
            .json(&json!({ "config": { "endpoint": "https://erp.test/v2", "token": "" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

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
                Some("public") => Err(openshop::PublicError::new("connection refused").into()),
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
    // A plain error is the checker's own business: the client reads an id.
    assert_eq!(body["ok"], false);
    assert!(body["errorId"].is_string(), "{body}");
    assert!(!body.to_string().contains("connection refused"), "{body}");

    let (status, _) = send(
        ctx.put("/api/providers/flaky")
            .json(&json!({ "config": { "token": "public" } })),
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

#[tokio::test]
async fn a_checker_failing_inside_the_server_is_logged_and_never_shown() {
    let leaky = Provider::new("leaky")
        .field("token", Field::password("Token"))
        .checker(|config| async move {
            match config.get("token").and_then(Value::as_str) {
                Some("wrapped") => Err(anyhow::Error::new(sqlx::Error::Protocol(
                    "relation \"secret_table\" at postgres://user:hunter2@db.internal/shop".to_string(),
                ))
                .context("loading the key")),
                _ => Err(sqlx::Error::Protocol("password=hunter2".to_string()).into()),
            }
        });
    let config = OpenShopConfig::builder()
        .flow(noop_flow("test-flow"))
        .provider(leaky)
        .build()
        .unwrap();
    let ctx = context(config).await;
    let logs = LogCapture::start().await;

    let mut ids = Vec::new();
    for token in ["plain", "wrapped"] {
        let (status, _) = send(
            ctx.put("/api/providers/leaky")
                .json(&json!({ "config": { "token": token } })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, body) = send(ctx.post("/api/providers/leaky/check")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        let text = body.to_string();
        for secret in ["hunter2", "secret_table", "postgres://", "loading the key", "Protocol"] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
        assert_eq!(body["ok"], false);
        let id = body["errorId"].as_str().expect("an error id").to_string();
        assert_eq!(id.len(), 12);
        assert_eq!(body["error"], format!("Error: Internal error (error id {id})"));
        ids.push(id);
    }
    assert_ne!(ids[0], ids[1]);

    // The cause is in the log, under the id.
    let logged = logs.text();
    for id in &ids {
        let line = logged.lines().find(|line| line.contains(id.as_str())).expect("a line");
        assert!(line.contains("hunter2"), "{line}");
    }
    ctx.shutdown().await.unwrap();
}

#[derive(serde::Deserialize)]
struct NumericCredentials {
    #[serde(rename = "apiKey")]
    _api_key: u64,
}

#[tokio::test]
async fn a_typed_checker_does_not_echo_a_saved_credential_it_cannot_decode() {
    let config = OpenShopConfig::builder()
        .provider(
            Provider::new("warehouse")
                .field("apiKey", Field::password("API key"))
                .typed_checker(|_: NumericCredentials| async { Ok(true) }),
        )
        .build()
        .unwrap();
    let ctx = TestContext::with_options(
        config,
        TestOptions {
            encryption_key: Some(ENCRYPTION_KEY.to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let (status, _) = send(
        ctx.put("/api/providers/warehouse")
            .json(&json!({ "config": { "apiKey": "secret-checker-credential" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send(ctx.post("/api/providers/warehouse/check")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!body.to_string().contains("secret-checker-credential"), "{body}");
    assert!(body["errorId"].is_string(), "{body}");
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_checker_that_never_answers_times_out() {
    let hung = Provider::new("hung")
        .field("token", Field::password("Token"))
        .checker(|_config| async move {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok(true)
        });
    let config = OpenShopConfig::builder()
        .flow(noop_flow("test-flow"))
        .provider(hung)
        .build()
        .unwrap();
    let ctx = context(config).await;
    let (status, _) = send(
        ctx.put("/api/providers/hung")
            .json(&json!({ "config": { "token": "t" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let started = std::time::Instant::now();
    let (status, body) = send(ctx.post("/api/providers/hung/check")).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(
        body,
        json!({ "ok": false, "error": "Error: the check timed out after 10 seconds" })
    );
    assert!(started.elapsed() < Duration::from_secs(30));

    // The server keeps serving, and the stored check is untouched.
    let (_, list) = send(ctx.get("/api/providers")).await;
    assert_eq!(list[0]["lastCheckOk"], Value::Null);
    ctx.shutdown().await.unwrap();
}

#[openshop::test(api_context())]
async fn an_unexpected_failure_has_an_id_in_a_header_and_in_the_log(ctx: &TestContext) {
    let logs = LogCapture::start().await;
    sqlx::query("DROP TABLE flow_runs CASCADE")
        .execute(&ctx.db)
        .await
        .unwrap();

    let response = ctx.get("/api/runs").send().await.expect("response");
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let id = response.headers()["x-error-id"].to_str().unwrap().to_string();
    assert_eq!(response.text().await.unwrap(), "Internal Server Error");

    let logged = logs.text();
    let line = logged.lines().find(|line| line.contains(&id)).expect("a line");
    assert!(line.contains("flow_runs"), "{line}");
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
        (Method::GET, format!("/api/runs/{run_id}/logs/export?format=json")),
        (Method::POST, format!("/api/runs/{run_id}/retry")),
        (Method::POST, format!("/api/runs/{run_id}/retry?mode=reset")),
        (Method::DELETE, format!("/api/runs/{run_id}")),
        (Method::POST, format!("/api/runs/{active}/cancel")),
    ] {
        let (status, body) = send(as_intruder(method.clone(), path.clone())).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
        assert_eq!(body, error("Run not found"), "{method} {path}");
    }

    // In a batch, only the runs of the session's shop are deleted.
    let finished = seed_run_for(&ctx.db, "default", OTHER_SHOP, "test-flow", "completed", base).await;
    let batch = json!({ "ids": [run_id, active, finished] });
    let (status, body) = send(as_intruder(Method::POST, "/api/runs/bulk-delete".to_string()).json(&batch)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "deleted": 1, "skipped": 2 }));
    assert!(!run_exists(&ctx.db, finished).await);

    assert_eq!(run_status(&ctx.db, run_id).await, "completed");
    assert_eq!(run_status(&ctx.db, active).await, "pending");
    assert_eq!(step_names(&ctx.db, run_id).await, ["step"]);
    assert_eq!(log_messages(&ctx.db, run_id).await, ["private line"]);

    // A run triggered by the other shop belongs to it, and does not conflict with ours.
    let (status, triggered) = send(as_intruder(Method::POST, "/api/flows/test-flow/run".to_string())).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let foreign = Uuid::parse_str(triggered["runId"].as_str().unwrap()).unwrap();
    assert_eq!(find_run(&ctx.db, foreign).await.shop, OTHER_SHOP);

    let (_, own) = send(ctx.get("/api/runs")).await;
    let mut own_ids = strings(&own, "id");
    own_ids.sort();
    let mut expected = vec![run_id.to_string(), active.to_string()];
    expected.sort();
    assert_eq!(own_ids, expected);
    let (status, _) = send(ctx.get(&format!("/api/runs/{foreign}"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Crons aimed at `OTHER_SHOP`, at the context's shop and `OTHER_SHOP`, and at every shop.
fn named_shops_config() -> OpenShopConfig {
    OpenShopConfig::builder()
        .flow(noop_flow("test-flow"))
        .cron(
            Cron::new("0 * * * *", "test-flow")
                .shop(OTHER_SHOP)
                .input(json!({ "customer": "secret-of-other" })),
        )
        .cron(Cron::new("1 * * * *", "test-flow").shops(["test.myshopify.com", OTHER_SHOP]))
        .cron(Cron::new("2 * * * *", "test-flow").all_shops())
        .build()
        .unwrap()
}

#[openshop::test(context(named_shops_config()))]
async fn crons_and_flows_of_the_admin_only_show_what_applies_to_the_session_shop(ctx: &TestContext) {
    let (status, crons) = send(ctx.get("/api/crons")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(strings(&crons, "key"), ["test-flow:1 * * * *", "test-flow:2 * * * *"]);
    assert_eq!(crons[0]["shops"], json!([ctx.shop]));
    let (_, flows) = send(ctx.get("/api/flows")).await;
    assert_eq!(strings(&flows[0]["crons"], "schedule"), ["1 * * * *", "2 * * * *"]);
    assert_eq!(flows[0]["crons"][0]["shops"], json!([ctx.shop]));
    for body in [crons.to_string(), flows.to_string()] {
        assert!(
            !body.contains(OTHER_SHOP) && !body.contains("secret-of-other"),
            "{body}"
        );
    }

    // Only a cron the shop sees can be switched.
    let (status, body) = send(
        ctx.post("/api/crons/toggle")
            .json(&json!({ "key": "test-flow:0 * * * *", "enabled": false })),
    )
    .await;
    assert_eq!((status, body), (404, error("Cron not found")));
    let (status, _) = send(
        ctx.post("/api/crons/toggle")
            .json(&json!({ "key": "test-flow:1 * * * *", "enabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let intruder = ctx.session_token_for(OTHER_SHOP, "2");
    let (_, crons) = send(request_as(ctx, &intruder, Method::GET, "/api/crons")).await;
    assert_eq!(
        strings(&crons, "key"),
        ["test-flow:0 * * * *", "test-flow:1 * * * *", "test-flow:2 * * * *"]
    );
    assert_eq!(crons[1]["enabled"], true);
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
    let config = OpenShopConfig::builder()
        .shopify(two_shopify_apps())
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
    assert_eq!(body, error("Unauthorized: invalid session token"));

    let (status, triggered) = send(as_a(Method::POST, "/api/flows/test-flow/run")).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let run_id = Uuid::parse_str(triggered["runId"].as_str().unwrap()).unwrap();
    assert_eq!(find_run(&ctx.db, run_id).await.app_handle, "clientA");
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
    let own_config = json!({ "config": { "endpoint": "https://client-b.test", "apiKey": "secret-b" } });
    let (status, _) = send(as_b(Method::PUT, "/api/providers/warehouse").json(&own_config)).await;
    assert_eq!(status, StatusCode::OK);

    // The same flow may run concurrently for the other app.
    let (status, other) = send(as_b(Method::POST, "/api/flows/test-flow/run")).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let other_run = Uuid::parse_str(other["runId"].as_str().unwrap()).unwrap();
    assert_eq!(find_run(&ctx.db, other_run).await.app_handle, "clientB");

    // App A still sees exactly its own data.
    let (_, runs) = send(as_a(Method::GET, "/api/runs")).await;
    assert_eq!(strings(&runs, "id"), [run_id.to_string()]);
    assert_eq!(run_status(&ctx.db, run_id).await, "pending");
    let (_, logs) = send(as_a(Method::GET, &format!("/api/runs/{run_id}/logs"))).await;
    assert_eq!(strings(&logs["logs"], "message"), ["private line"]);
    let (_, providers) = send(as_a(Method::GET, "/api/providers")).await;
    assert_eq!(providers[0]["configured"], true);
    assert_eq!(providers[0]["config"], json!({ "endpoint": "https://warehouse.test" }));
    let (_, crons) = send(as_a(Method::GET, "/api/crons")).await;
    assert_eq!(crons[0]["enabled"], false);
    let handles: Vec<(String,)> = sqlx::query_as("SELECT app_handle FROM provider_configs ORDER BY app_handle")
        .fetch_all(&ctx.db)
        .await
        .unwrap();
    assert_eq!(handles, [("clientA".to_string(),), ("clientB".to_string(),)]);
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
        "https://admin.shopify.com.evil.com",
        "https://some-shop.myshopify.com.evil.com",
        "http://some-shop.myshopify.com",
        "https://app.example.com",
        "null",
    ] {
        let response = ctx.get("/api/flows").header(ORIGIN, origin).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{origin}");
        assert_eq!(allowed_origin(&response), None, "{origin}");
        let preflight = anonymous(ctx, Method::OPTIONS, "/api/flows")
            .header(ORIGIN, origin)
            .header(ACCESS_CONTROL_REQUEST_METHOD, "GET")
            .header(ACCESS_CONTROL_REQUEST_HEADERS, "authorization")
            .send()
            .await
            .unwrap();
        assert_eq!(allowed_origin(&preflight), None, "{origin}");
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
        (
            ServerOptions {
                ui: UiSource::Embedded,
                dev: false,
            },
            None,
        ),
        (
            ServerOptions {
                ui: UiSource::Embedded,
                dev: true,
            },
            Some(origin),
        ),
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
