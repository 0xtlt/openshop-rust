//! The page of a run, rendered by the server: its steps, the output of each,
//! its logs and their filters, read from the URL; retried and deleted by its
//! forms; fetched again while it is active.

use chrono::{DateTime, Duration, SubsecRound, Utc};
use openshop::builder::Flow;
use openshop::prelude::*;
use openshop::server::ServerOptions;
use openshop::testing::{TestContext, TestOptions};
use openshop::{OpenShopConfig, PgPool, UiSource};
use reqwest::header::{AUTHORIZATION, CONTENT_DISPOSITION, LOCATION};
use reqwest::{Method, StatusCode};

use super::{Reply, fetch, form, request, send};
use crate::common::{find_run, find_steps, run_exists};

const SHOP: &str = "flow-run-page.myshopify.com";
const OTHER_SHOP: &str = "other-shop.myshopify.com";
const FLOW: &str = "syncOrders";

async fn context() -> TestContext {
    let flow = Flow::new(FLOW, |_ctx: FlowContext| async move { Ok(()) });
    let config = OpenShopConfig::builder().flow(flow).build().expect("valid config");
    TestContext::with_options(
        config,
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

/// A whole second of the recent past: dates that the page writes as they are.
fn base_time() -> DateTime<Utc> {
    (Utc::now() - Duration::hours(1)).trunc_subsecs(0)
}

async fn seed_run(db: &PgPool, shop: &str, status: &str, created_at: DateTime<Utc>) -> Uuid {
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO flow_runs (app_handle, shop, flow_name, status, input, error, available_at, created_at, \
           started_at, completed_at) \
         VALUES ('default', $1, $2, $3, '{\"limit\": 5}'::json, $4, $5, $5, $5, $6) RETURNING id",
    )
    .bind(shop)
    .bind(FLOW)
    .bind(status)
    .bind((status == "failed").then_some("Shopify said <no>"))
    .bind(created_at)
    .bind((!["pending", "running", "sleeping"].contains(&status)).then_some(created_at + Duration::milliseconds(1_250)))
    .fetch_one(db)
    .await
    .unwrap();
    id
}

async fn seed_step(db: &PgPool, run_id: Uuid, name: &str, status: &str, output: Value, created_at: DateTime<Utc>) {
    sqlx::query(
        "INSERT INTO step_results (flow_run_id, step_name, status, output, error, duration_ms, created_at) \
         VALUES ($1, $2, $3, $4::json, $5, 42, $6)",
    )
    .bind(run_id.to_string())
    .bind(name)
    .bind(status)
    .bind(output.to_string())
    .bind((status == "failed").then_some("Push refused"))
    .bind(created_at)
    .execute(db)
    .await
    .unwrap();
}

async fn seed_log(db: &PgPool, run_id: Uuid, level: &str, message: &str, payload: Value, created_at: DateTime<Utc>) {
    sqlx::query(
        "INSERT INTO logs (flow_run_id, level, message, payload, created_at) VALUES ($1, $2, $3, $4::json, $5)",
    )
    .bind(run_id.to_string())
    .bind(level)
    .bind(message)
    .bind(payload.to_string())
    .bind(created_at)
    .execute(db)
    .await
    .unwrap();
}

async fn set_status(db: &PgPool, run_id: Uuid, status: &str) {
    sqlx::query("UPDATE flow_runs SET status = $2 WHERE id = $1")
        .bind(run_id)
        .bind(status)
        .execute(db)
        .await
        .unwrap();
}

/// A failed run of two steps, the first one done, and three lines of logs.
async fn seed_failed_run(ctx: &TestContext) -> Uuid {
    let base = base_time();
    let run = seed_run(&ctx.db, SHOP, "failed", base).await;
    seed_step(
        &ctx.db,
        run,
        "fetch",
        "completed",
        json!({ "orders": ["<#1001>"] }),
        base,
    )
    .await;
    seed_step(&ctx.db, run, "push", "failed", Value::Null, base + Duration::seconds(1)).await;
    seed_log(&ctx.db, run, "info", "Fetched <1> order", json!({ "count": 1 }), base).await;
    seed_log(
        &ctx.db,
        run,
        "warn",
        "Shopify is slow",
        json!(null),
        base + Duration::seconds(1),
    )
    .await;
    seed_log(
        &ctx.db,
        run,
        "error",
        "Push failed",
        json!({}),
        base + Duration::seconds(2),
    )
    .await;
    run
}

async fn page(ctx: &TestContext, path: &str) -> Reply {
    send(fetch(ctx, Method::GET, path)).await
}

/// A header of the answer, as the script decodes it.
fn decoded(reply: &Reply, name: &str) -> Option<String> {
    let value = reply.header(name)?;
    url::form_urlencoded::parse(format!("v={value}").as_bytes())
        .next()
        .map(|(_, value)| value.into_owned())
}

/// The value of the first attribute `name` after `marker` in the page, unescaped.
/// The text of the element that starts with `start`, unescaped.
fn html_text(body: &str, start: &str) -> String {
    let after = body.find(start).unwrap_or_else(|| panic!("no {start}: {body}"));
    let body = &body[after..];
    let open = body.find('>').unwrap() + 1;
    let close = open + body[open..].find("</").unwrap();
    body[open..close]
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn attribute(body: &str, marker: &str, name: &str) -> String {
    let after = body.find(marker).unwrap_or_else(|| panic!("no {marker}: {body}"));
    let body = &body[after..];
    let start = body
        .find(&format!("{name}=\""))
        .unwrap_or_else(|| panic!("no {name}: {body}"))
        + name.len()
        + 2;
    let end = start + body[start..].find('"').unwrap();
    body[start..end]
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[tokio::test]
async fn the_page_of_a_run_shows_its_steps_their_output_and_its_logs() {
    let ctx = context().await;
    let run = seed_failed_run(&ctx).await;

    let reply = page(&ctx, &format!("/runs/{run}")).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let body = &reply.body;
    assert!(body.contains(&format!(r#"<s-page heading="Run #{run}">"#)), "{body}");
    assert!(
        body.contains(r#"<s-link slot="breadcrumb-actions" href="/flows/syncOrders">syncOrders</s-link>"#),
        "{body}"
    );
    assert!(body.contains(r#"<s-badge tone="critical">failed</s-badge>"#), "{body}");
    assert!(body.contains(r#"<s-text type="strong">1.3s</s-text>"#), "{body}");
    assert!(
        body.contains(r#"<s-banner tone="critical" heading="Error">Shopify said &lt;no&gt;</s-banner>"#),
        "{body}"
    );
    // A run that is over is not fetched again.
    assert!(!body.contains("data-openshop-refresh"), "{body}");
    // The input, in a modal.
    assert!(
        body.contains(r#"<s-modal id="run-input" heading="Input" size="large">"#),
        "{body}"
    );
    assert!(body.contains("&quot;limit&quot;: 5"), "{body}");

    // The steps, and the output of the one that has one, which the modal copies and downloads.
    assert!(body.contains("<strong>fetch</strong>"), "{body}");
    assert!(
        body.contains(r#"<s-badge tone="success">completed</s-badge>"#),
        "{body}"
    );
    assert!(body.contains("<s-table-cell>42ms</s-table-cell>"), "{body}");
    assert!(
        body.contains(r#"<span style="color:#d72c0d;font-size:13px">Push refused</span>"#),
        "{body}"
    );
    assert_eq!(body.matches(">Output</s-button>").count(), 1, "{body}");
    assert!(body.contains(r#"heading="Output — fetch" size="large">"#), "{body}");
    let output = serde_json::to_string_pretty(&json!({ "orders": ["<#1001>"] })).unwrap();
    let modal = "heading=\"Output — fetch\"";
    // The buttons copy and download the `pre` of the modal, which holds it.
    let source = attribute(body, modal, "data-openshop-copy-from");
    assert_eq!(attribute(body, modal, "data-openshop-download-from"), source);
    let shown = html_text(body, &format!(r#"<pre id="{source}""#));
    assert_eq!(shown, output);
    assert!(body.contains(r#"data-openshop-filename="fetch.json""#), "{body}");

    // The logs, newest first, escaped, with the count of each level and the exports.
    let pushed = body.find("Push failed").unwrap();
    assert!(pushed < body.find("Shopify is slow").unwrap(), "{body}");
    assert!(body.contains("Fetched &lt;1&gt; order"), "{body}");
    assert!(
        body.contains(r#"<span style="color:#8c9196"> count=1</span>"#),
        "{body}"
    );
    assert!(body.contains(">INFO (1)</s-button>"), "{body}");
    assert!(body.contains("3/3 logs"), "{body}");
    assert!(
        body.contains(&format!(
            r#"data-openshop-download="/api/runs/{run}/logs/export?levels=info%2Cwarn%2Cerror&amp;format=csv" data-openshop-filename="run-{run}-logs.csv""#
        )),
        "{body}"
    );
    // The forms of the run that is over.
    assert!(
        body.contains(r#"<input type="hidden" name="mode" value="resume">"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<input type="hidden" name="_action" value="delete">"#),
        "{body}"
    );

    // The filters are the URL: a search, and the levels shown.
    let filtered = page(&ctx, &format!("/runs/{run}?q=shopify&levels=warn%2Cerror")).await;
    assert_eq!(filtered.status, StatusCode::OK, "{}", filtered.body);
    let body = &filtered.body;
    assert!(body.contains("Shopify is slow"), "{body}");
    assert!(!body.contains("Push failed"), "{body}");
    assert!(!body.contains("Fetched"), "{body}");
    assert!(body.contains("1/3 logs"), "{body}");
    assert!(body.contains(r#"value="shopify"></s-search-field>"#), "{body}");
    assert!(
        body.contains(r#"<input type="hidden" name="levels" value="warn,error">"#),
        "{body}"
    );
    assert!(
        body.contains(&format!(
            r#"<s-button href="/runs/{run}?q=shopify&amp;levels=warn%2Cerror%2Cinfo" variant="secondary">INFO (1)</s-button>"#
        )),
        "{body}"
    );
    // The export is the file of what the page shows, which the script asks for with the session token.
    let export = attribute(body, ">JSON<", "data-openshop-download");
    assert_eq!(
        export,
        format!("/api/runs/{run}/logs/export?q=shopify&levels=warn%2Cerror&format=csv")
    );
    let file = send(fetch(&ctx, Method::GET, &export)).await;
    assert_eq!(file.status, StatusCode::OK);
    assert_eq!(
        file.header(CONTENT_DISPOSITION.as_str()),
        Some(format!("attachment; filename=\"run-{run}-logs.csv\"").as_str())
    );
    assert!(
        file.body.contains("Shopify is slow") && !file.body.contains("Push failed"),
        "{}",
        file.body
    );

    let lately = page(&ctx, &format!("/runs/{run}?q=last%3A5m")).await;
    assert!(lately.body.contains("No matching logs."), "{}", lately.body);
    let none = page(&ctx, &format!("/runs/{run}?levels=")).await;
    assert!(none.body.contains("0/3 logs"), "{}", none.body);
    assert!(none.body.contains(">INFO (1)</s-button>"), "{}", none.body);

    // A search the engine refuses says why, and keeps the counts of the run.
    let refused = page(&ctx, &format!("/runs/{run}?q=%7C~+%22(%22")).await;
    assert_eq!(refused.status, StatusCode::OK, "{}", refused.body);
    assert!(
        refused
            .body
            .contains(r#"<s-banner tone="critical" heading="The search was refused">Invalid regex &quot;(&quot;: "#),
        "{}",
        refused.body
    );
    assert!(refused.body.contains("0/3 logs"), "{}", refused.body);
    assert!(refused.body.contains(">ERROR (1)</s-button>"), "{}", refused.body);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_active_run_is_fetched_again_and_a_missing_one_is_not_found() {
    let ctx = context().await;
    let base = base_time();
    let running = seed_run(&ctx.db, SHOP, "running", base).await;
    seed_log(&ctx.db, running, "info", "Just now", json!(null), Utc::now()).await;

    let reply = page(&ctx, &format!("/runs/{running}")).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(reply.body.contains(r#"data-openshop-refresh="3000""#), "{}", reply.body);
    assert!(
        reply.body.contains(r#"<s-text type="strong">Running…</s-text>"#),
        "{}",
        reply.body
    );
    // Nothing to retry or delete while it runs; the line it just wrote is highlighted.
    assert!(!reply.body.contains(r#"name="_action""#), "{}", reply.body);
    assert!(reply.body.contains("background:#fff3cd"), "{}", reply.body);
    assert!(reply.body.contains("Just now"), "{}", reply.body);

    // Not there, not a run, or a run of another shop.
    let elsewhere = seed_run(&ctx.db, OTHER_SHOP, "failed", base).await;
    for id in [Uuid::nil().to_string(), "not-a-uuid".to_string(), elsewhere.to_string()] {
        let missing = page(&ctx, &format!("/runs/{id}")).await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND, "{id}");
        assert!(
            missing
                .body
                .contains(r#"<s-banner tone="critical" heading="Run not found">"#),
            "{id}: {}",
            missing.body
        );
        let action = send(form(
            fetch(&ctx, Method::POST, &format!("/runs/{id}")),
            "_action=retry&mode=resume",
        ))
        .await;
        assert_eq!(action.status, StatusCode::NOT_FOUND, "{id}");
        assert_eq!(
            decoded(&action, "x-openshop-toast-error").as_deref(),
            Some("Run not found")
        );
        let action = send(form(
            fetch(&ctx, Method::POST, &format!("/runs/{id}")),
            "_action=delete",
        ))
        .await;
        assert_eq!(action.status, StatusCode::NOT_FOUND, "{id}");
    }
    assert!(run_exists(&ctx.db, elsewhere).await);
    assert_eq!(find_run(&ctx.db, elsewhere).await.status, "failed");
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_run_is_retried_by_the_form_of_its_mode() {
    let ctx = context().await;
    let run = seed_failed_run(&ctx).await;
    let path = format!("/runs/{run}?q=shopify");
    let steps = |ctx: &TestContext| {
        let db = ctx.db.clone();
        async move {
            find_steps(&db, run)
                .await
                .into_iter()
                .map(|step| step.step_name)
                .collect::<Vec<_>>()
        }
    };

    // Resumed: the steps that completed are kept, and the page, with its filters, follows the run.
    let resumed = send(form(fetch(&ctx, Method::POST, &path), "_action=retry&mode=resume")).await;
    assert_eq!(resumed.status, StatusCode::OK, "{}", resumed.body);
    assert_eq!(decoded(&resumed, "x-openshop-toast").as_deref(), Some("Run resumed"));
    assert!(
        resumed.body.contains(r#"<s-badge tone="info">pending</s-badge>"#),
        "{}",
        resumed.body
    );
    assert!(
        resumed.body.contains(r#"data-openshop-refresh="3000""#),
        "{}",
        resumed.body
    );
    assert!(
        resumed.body.contains(r#"value="shopify"></s-search-field>"#),
        "{}",
        resumed.body
    );
    assert_eq!(find_run(&ctx.db, run).await.status, "pending");
    assert_eq!(steps(&ctx).await, ["fetch"]);

    // A run that is not over is refused, and stays as it is.
    let again = send(form(fetch(&ctx, Method::POST, &path), "_action=retry&mode=reset")).await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(
        decoded(&again, "x-openshop-toast-error").as_deref(),
        Some(r#"Cannot retry a run with status "pending""#)
    );
    assert!(again.body.is_empty());
    assert_eq!(steps(&ctx).await, ["fetch"]);

    // Restarted: every step is discarded.
    set_status(&ctx.db, run, "failed").await;
    let restarted = send(form(fetch(&ctx, Method::POST, &path), "_action=retry&mode=reset")).await;
    assert_eq!(restarted.status, StatusCode::OK, "{}", restarted.body);
    assert_eq!(
        decoded(&restarted, "x-openshop-toast").as_deref(),
        Some("Run restarted")
    );
    assert!(steps(&ctx).await.is_empty());
    // The line of the retry, in the logs of the run.
    let logs = page(&ctx, &format!("/runs/{run}?levels=info")).await;
    assert!(
        logs.body.contains("Run retried (restart — all steps discarded)"),
        "{}",
        logs.body
    );

    // The flow rejects a second active run, as a dispatch does.
    set_status(&ctx.db, run, "completed").await;
    let active = seed_run(&ctx.db, SHOP, "sleeping", base_time()).await;
    let concurrent = send(form(fetch(&ctx, Method::POST, &path), "_action=retry&mode=resume")).await;
    assert_eq!(concurrent.status, StatusCode::CONFLICT);
    assert_eq!(
        decoded(&concurrent, "x-openshop-toast-error"),
        Some(format!(
            "Flow \"{FLOW}\" is already running for shop \"{SHOP}\" (run #{active})"
        ))
    );
    assert_eq!(find_run(&ctx.db, run).await.status, "completed");

    // A mode the page does not have, and an action it does not have.
    let sideways = send(form(fetch(&ctx, Method::POST, &path), "_action=retry&mode=sideways")).await;
    assert_eq!(sideways.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        decoded(&sideways, "x-openshop-toast-error").as_deref(),
        Some("mode must be resume or reset")
    );
    let unknown = send(form(fetch(&ctx, Method::POST, &path), "_action=cancel")).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(find_run(&ctx.db, run).await.status, "completed");
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_run_is_deleted_by_its_form_and_the_page_of_its_flow_is_shown() {
    let ctx = context().await;
    let run = seed_failed_run(&ctx).await;
    let path = format!("/runs/{run}");

    // An active run is not deleted.
    set_status(&ctx.db, run, "running").await;
    let refused = send(form(fetch(&ctx, Method::POST, &path), "_action=delete")).await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(
        decoded(&refused, "x-openshop-toast-error").as_deref(),
        Some("Cannot delete an active run — cancel it first")
    );
    assert!(run_exists(&ctx.db, run).await);

    // Over: deleted with its steps and its logs; the script goes to the page of its flow.
    set_status(&ctx.db, run, "failed").await;
    let deleted = send(form(fetch(&ctx, Method::POST, &path), "_action=delete")).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);
    assert_eq!(deleted.header("x-openshop-location"), Some("/flows/syncOrders"));
    assert_eq!(decoded(&deleted, "x-openshop-toast").as_deref(), Some("Run deleted"));
    assert!(!run_exists(&ctx.db, run).await);
    assert!(find_steps(&ctx.db, run).await.is_empty());
    let (logs,): (i64,) = sqlx::query_as("SELECT count(*) FROM logs WHERE flow_run_id = $1")
        .bind(run.to_string())
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(logs, 0);

    let gone = send(form(fetch(&ctx, Method::POST, &path), "_action=delete")).await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
    assert_eq!(page(&ctx, &path).await.status, StatusCode::NOT_FOUND);

    // Without the script: a redirect to the page of the flow.
    let other = seed_failed_run(&ctx).await;
    let plain = send(form(
        request(&ctx, Method::POST, &format!("/runs/{other}")).header(AUTHORIZATION, ctx.authorization_header()),
        "_action=delete",
    ))
    .await;
    assert_eq!(plain.status, StatusCode::SEE_OTHER);
    assert_eq!(plain.headers.get(LOCATION).unwrap(), "/flows/syncOrders");
    assert!(!run_exists(&ctx.db, other).await);
    ctx.shutdown().await.unwrap();
}
