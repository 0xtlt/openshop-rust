//! The page `flows` of the framework, rendered by the server: the flows,
//! their runs filtered and paged by the URL, the form that starts a flow and
//! the deletion of the runs selected.

use chrono::{DateTime, Duration, Utc};
use openshop::builder::{Flow, InputSchema};
use openshop::server::ServerOptions;
use openshop::testing::{TestContext, TestOptions};
use openshop::{Concurrency, Cron, FlowContext, OpenShopConfig, UiSource, Value, json};
use reqwest::header::AUTHORIZATION;
use reqwest::{Method, StatusCode};
use uuid::Uuid;

use super::{fetch, form, request, send, urlencoding};

const SHOP: &str = "flows-page.myshopify.com";

/// `syncOrders` takes `{ "limit": <number> }` and runs once at a time, on a
/// cron; `manual` takes anything, as often as asked.
fn config() -> OpenShopConfig {
    let sync_orders =
        Flow::new("syncOrders", |_ctx: FlowContext| async move { Ok(()) }).input_schema(InputSchema::custom(
            json!({
                "type": "object",
                "properties": { "limit": { "type": "integer", "minimum": 1 } },
                "required": ["limit"],
            }),
            |value| match value.get("limit") {
                Some(Value::Number(_)) => Ok(value),
                _ => Err("limit must be a number".to_string()),
            },
        ));
    let manual = Flow::new("manual", |_ctx: FlowContext| async move { Ok(()) }).concurrency(Concurrency::Allow);
    OpenShopConfig::builder()
        .flow(sync_orders)
        .flow(manual)
        .cron(Cron::new("*/5 * * * *", "syncOrders").input(json!({ "limit": 10 })))
        .build()
        .expect("valid config")
}

async fn context() -> TestContext {
    TestContext::with_options(
        config(),
        TestOptions {
            shop: SHOP.to_string(),
            access_token: Some("offline-token".to_string()),
            server: ServerOptions {
                ui: UiSource::Server,
                dev: false,
            },
            ..Default::default()
        },
    )
    .await
    .expect("test context")
}

/// A run of the shop, created `minutes_ago`; one that is over took 1.25 s.
async fn seed_run(ctx: &TestContext, flow: &str, status: &str, minutes_ago: i64) -> Uuid {
    let created: DateTime<Utc> = Utc::now() - Duration::minutes(minutes_ago);
    let started = (status != "pending").then_some(created);
    let completed =
        matches!(status, "completed" | "failed" | "canceled").then(|| created + Duration::milliseconds(1_250));
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO flow_runs (app_handle, shop, flow_name, status, input, available_at, created_at, started_at, completed_at) \
         VALUES ('default', $1, $2, $3, '{}'::json, $4, $4, $5, $6) RETURNING id",
    )
    .bind(&ctx.shop)
    .bind(flow)
    .bind(status)
    .bind(created)
    .bind(started)
    .bind(completed)
    .fetch_one(&ctx.db)
    .await
    .unwrap();
    id
}

async fn run_exists(ctx: &TestContext, id: Uuid) -> bool {
    let (exists,): (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM flow_runs WHERE id = $1)")
        .bind(id)
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    exists
}

#[tokio::test]
async fn the_flows_page_lists_the_flows_and_their_runs() {
    let ctx = context().await;

    // Without runs.
    let empty = send(fetch(&ctx, Method::GET, "/flows")).await;
    assert_eq!(empty.status, StatusCode::OK, "{}", empty.body);
    let body = &empty.body;
    assert!(body.contains(r#"<s-page heading="Flows">"#), "{body}");
    assert!(
        body.contains(r#"<s-link href="/flows/syncOrders">syncOrders</s-link>"#),
        "{body}"
    );
    assert!(body.contains(">*/5 * * * *</code>"), "{body}");
    assert!(body.contains(r#"<s-badge tone="info">Scheduled</s-badge>"#), "{body}");
    assert!(body.contains(r#"<s-badge tone="neutral">Manual</s-badge>"#), "{body}");
    // The form of a flow starts with an example of its input schema.
    assert!(
        body.contains(r#"<s-modal id="run-flow-0" heading="Run syncOrders">"#),
        "{body}"
    );
    assert!(body.contains("value=\"{\n  &quot;limit&quot;: 1\n}\""), "{body}");
    assert!(body.contains("No runs yet"), "{body}");
    assert!(!body.contains("data-openshop-refresh"), "{body}");

    let completed = seed_run(&ctx, "syncOrders", "completed", 30).await;
    let failed = seed_run(&ctx, "manual", "failed", 20).await;
    let running = seed_run(&ctx, "manual", "running", 10).await;

    // Every run, newest first, with its flow; one is active, so the page is fetched again.
    let page = send(fetch(&ctx, Method::GET, "/flows")).await;
    let body = &page.body;
    let position = |id: Uuid| {
        body.find(&format!(r#"href="/runs/{id}""#))
            .unwrap_or_else(|| panic!("{id}: {body}"))
    };
    assert!(position(running) < position(failed) && position(failed) < position(completed));
    assert!(
        body.contains(r#"<s-section heading="Runs" data-openshop-refresh="5000">"#),
        "{body}"
    );
    assert!(body.contains(r#"<s-badge tone="critical">failed</s-badge>"#), "{body}");
    assert!(body.contains("<s-table-cell>1.3s</s-table-cell>"), "{body}");
    assert!(body.contains("<s-table-cell>…</s-table-cell>"), "{body}");
    assert!(body.contains("<s-text>30m ago</s-text>"), "{body}");
    assert!(body.contains(&format!(r#"name="ids" value="{failed}""#)), "{body}");

    // The runs of one flow.
    let one = send(fetch(&ctx, Method::GET, "/flows/syncOrders")).await;
    assert_eq!(one.status, StatusCode::OK);
    assert!(
        one.body.contains(r#"<s-section heading="Runs — syncOrders">"#),
        "{}",
        one.body
    );
    assert!(one.body.contains(&format!("/runs/{completed}")), "{}", one.body);
    assert!(!one.body.contains(&format!("/runs/{failed}")), "{}", one.body);

    // Searched, and filtered by status, as the URL says.
    let short = &running.to_string()[..8];
    let searched = send(fetch(&ctx, Method::GET, &format!("/flows?q={short}"))).await;
    assert!(searched.body.contains(&format!("/runs/{running}")), "{}", searched.body);
    assert!(!searched.body.contains(&format!("/runs/{failed}")), "{}", searched.body);
    assert!(
        searched.body.contains(&format!(r#"value="{short}""#)),
        "{}",
        searched.body
    );
    let failures = send(fetch(&ctx, Method::GET, "/flows?status=failed")).await;
    assert!(failures.body.contains(&format!("/runs/{failed}")), "{}", failures.body);
    assert!(
        !failures.body.contains(&format!("/runs/{running}")),
        "{}",
        failures.body
    );
    assert!(!failures.body.contains("data-openshop-refresh"), "{}", failures.body);
    let tomorrow = (Utc::now() + Duration::days(1)).format("%Y-%m-%d");
    let later = send(fetch(&ctx, Method::GET, &format!("/flows?from={tomorrow}"))).await;
    assert!(later.body.contains("No runs match these filters"), "{}", later.body);
    let today = Utc::now().format("%Y-%m-%d");
    let until_today = send(fetch(&ctx, Method::GET, &format!("/flows?to={today}"))).await;
    assert!(
        until_today.body.contains(&format!("/runs/{running}")),
        "{}",
        until_today.body
    );

    // A page at a time: 25 newer runs push the first three to the second page.
    for _ in 0..25 {
        seed_run(&ctx, "manual", "completed", 0).await;
    }
    let first = send(fetch(&ctx, Method::GET, "/flows")).await;
    assert!(
        first
            .body
            .contains(r#"accessibilityLabel="Next page" href="/flows?page=2">"#),
        "{}",
        first.body
    );
    assert!(!first.body.contains(&format!("/runs/{completed}")), "{}", first.body);
    let second = send(fetch(&ctx, Method::GET, "/flows?page=2")).await;
    assert!(
        second
            .body
            .contains(r#"accessibilityLabel="Previous page" href="/flows">"#),
        "{}",
        second.body
    );
    assert!(
        second.body.contains(r#"accessibilityLabel="Next page" disabled>"#),
        "{}",
        second.body
    );
    assert!(second.body.contains(&format!("/runs/{completed}")), "{}", second.body);
    // A page too deep is said above the table.
    let deep = send(fetch(&ctx, Method::GET, "/flows?page=1000")).await;
    assert_eq!(deep.status, StatusCode::OK);
    assert!(
        deep.body.contains(
            r#"<s-banner tone="critical" heading="Runs could not be loaded">offset must be at most 10000</s-banner>"#
        ),
        "{}",
        deep.body
    );

    // Every run of the page selected.
    let all = send(fetch(&ctx, Method::GET, "/flows?select=all")).await;
    assert_eq!(
        all.body
            .matches(r#"labelAccessibilityVisibility="exclusive" checked>"#)
            .count(),
        25,
        "{}",
        all.body
    );
    assert!(
        all.body.contains(r#"href="/flows">Clear selection</s-button>"#),
        "{}",
        all.body
    );

    // A flow the code no longer declares still lists its runs: the page of a run links there.
    let gone = seed_run(&ctx, "gone/flow", "failed", 50).await;
    let removed = send(fetch(&ctx, Method::GET, "/flows/gone%2Fflow")).await;
    assert_eq!(removed.status, StatusCode::OK);
    assert!(
        removed.body.contains(r#"<s-section heading="Runs — gone/flow">"#),
        "{}",
        removed.body
    );
    assert!(removed.body.contains(&format!("/runs/{gone}")), "{}", removed.body);

    // A path that is no page of flows.
    let missing = send(fetch(&ctx, Method::GET, "/flows/a/b")).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_flow_is_started_from_its_form() {
    let ctx = context().await;

    // Not JSON: the form again, the error under its field and its modal open.
    let invalid = send(form(
        fetch(&ctx, Method::POST, "/flows"),
        format!("_action=run&flow=syncOrders&input={}", urlencoding("{ limit: ")),
    ))
    .await;
    assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", invalid.body);
    assert!(
        invalid
            .body
            .contains(r#"<s-modal id="run-flow-0" heading="Run syncOrders" data-openshop-open>"#),
        "{}",
        invalid.body
    );
    assert!(
        invalid.body.contains(r#"value="{ limit: " error="Invalid JSON""#),
        "{}",
        invalid.body
    );
    // An input the flow refuses, said the same way.
    let refused = send(form(
        fetch(&ctx, Method::POST, "/flows"),
        format!(
            "_action=run&flow=syncOrders&input={}",
            urlencoding(r#"{"limit":"ten"}"#)
        ),
    ))
    .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        refused
            .body
            .contains(r#"error="Invalid input for flow &quot;syncOrders&quot;: limit must be a number""#),
        "{}",
        refused.body
    );
    let (runs,): (i64,) = sqlx::query_as("SELECT count(*) FROM flow_runs")
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(runs, 0);

    // Started: its run, with a toast.
    let started = send(form(
        fetch(&ctx, Method::POST, "/flows/syncOrders?q=x"),
        format!(
            "_action=run&flow=syncOrders&input={}",
            urlencoding("{\n  \"limit\": 3\n}")
        ),
    ))
    .await;
    assert_eq!(started.status, StatusCode::NO_CONTENT);
    assert_eq!(
        started.header("x-openshop-toast"),
        Some("Flow%20%22syncOrders%22%20started")
    );
    let location = started.header("x-openshop-location").unwrap();
    let run_id = Uuid::parse_str(location.strip_prefix("/runs/").unwrap()).unwrap();
    let (flow, input): (String, Value) = sqlx::query_as("SELECT flow_name, input FROM flow_runs WHERE id = $1")
        .bind(run_id)
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!((flow.as_str(), input), ("syncOrders", json!({ "limit": 3 })));

    // Once at a time: the page stays, with an error toast.
    let again = send(form(
        fetch(&ctx, Method::POST, "/flows"),
        format!("_action=run&flow=syncOrders&input={}", urlencoding(r#"{"limit":1}"#)),
    ))
    .await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    let conflict = format!("Flow \"syncOrders\" is already running for shop \"{SHOP}\" (run #{run_id})");
    assert_eq!(
        again.header("x-openshop-toast-error"),
        Some(openshop_shared::rpc::encode_uri_component(&conflict).as_str())
    );
    assert!(again.body.is_empty());
    let unknown = send(form(
        fetch(&ctx, Method::POST, "/flows"),
        "_action=run&flow=nope&input=%7B%7D",
    ))
    .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(
        unknown.header("x-openshop-toast-error"),
        Some("Flow%20%22nope%22%20not%20found")
    );

    // `null` is no input, as for the API; without the script, a redirect to the run.
    let plain = send(form(
        request(&ctx, Method::POST, "/flows").header(AUTHORIZATION, ctx.authorization_header()),
        "_action=run&flow=manual&input=null",
    ))
    .await;
    assert_eq!(plain.status, StatusCode::SEE_OTHER);
    let location = plain.header("location").unwrap();
    let run_id = Uuid::parse_str(location.strip_prefix("/runs/").unwrap()).unwrap();
    let (input,): (Value,) = sqlx::query_as("SELECT input FROM flow_runs WHERE id = $1")
        .bind(run_id)
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(input, json!({}));

    let unknown_action = send(form(fetch(&ctx, Method::POST, "/flows"), "_action=dropTables")).await;
    assert_eq!(unknown_action.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_runs_selected_are_deleted() {
    let ctx = context().await;
    let completed = seed_run(&ctx, "manual", "completed", 3).await;
    let failed = seed_run(&ctx, "manual", "failed", 2).await;
    let pending = seed_run(&ctx, "manual", "pending", 1).await;

    // Nothing selected.
    let none = send(form(fetch(&ctx, Method::POST, "/flows"), "_action=deleteRuns")).await;
    assert_eq!(none.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        none.header("x-openshop-toast-error"),
        Some("Select%20the%20runs%20to%20delete")
    );

    // The runs that are over are deleted; an active one is skipped, as is what names no run.
    let deleted = send(form(
        fetch(&ctx, Method::POST, "/flows?status=&q="),
        format!("_action=deleteRuns&ids={completed}&ids={failed}&ids={pending}&ids=nope"),
    ))
    .await;
    assert_eq!(deleted.status, StatusCode::OK, "{}", deleted.body);
    assert_eq!(
        deleted.header("x-openshop-toast"),
        Some("Deleted%202%20run(s)%2C%202%20skipped%20(active)")
    );
    assert!(
        !deleted.body.contains(&format!("/runs/{completed}")),
        "{}",
        deleted.body
    );
    assert!(deleted.body.contains(&format!("/runs/{pending}")), "{}", deleted.body);
    assert!(!run_exists(&ctx, completed).await && !run_exists(&ctx, failed).await);
    assert!(run_exists(&ctx, pending).await);

    // From a page where every run was selected: the page again, nothing selected.
    let over = seed_run(&ctx, "manual", "canceled", 4).await;
    let cleared = send(form(
        fetch(&ctx, Method::POST, "/flows/manual?select=all&q=man"),
        format!("_action=deleteRuns&ids={over}"),
    ))
    .await;
    assert_eq!(cleared.status, StatusCode::NO_CONTENT);
    assert_eq!(cleared.header("x-openshop-location"), Some("/flows/manual?q=man"));
    assert_eq!(cleared.header("x-openshop-toast"), Some("Deleted%201%20run(s)"));
    assert!(!run_exists(&ctx, over).await);
}
