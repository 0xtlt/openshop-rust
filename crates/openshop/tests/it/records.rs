//! The rows of the framework tables, read across shops: installations, runs,
//! their steps and their logs.

use crate::common::{TestApp, create_config, install, noop_flow};
use chrono::{Duration, Utc};
use openshop::testing::{AdminTestOptions, admin_context};
use openshop::{FlowRun, Installation, LogRow, PgPool, Record, StepResult, Uuid, json};

const SHOP: &str = "one.myshopify.com";
const OTHER_SHOP: &str = "two.myshopify.com";

/// Inserts a run created `minutes_ago` and returns its id.
async fn run(db: &PgPool, app_handle: &str, shop: &str, status: &str, minutes_ago: i64) -> Uuid {
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO flow_runs (app_handle, shop, flow_name, status, input, error, created_at) \
         VALUES ($1, $2, 'sync', $3, '{\"limit\":10}', CASE WHEN $3 = 'failed' THEN 'boom' END, $4) RETURNING id",
    )
    .bind(app_handle)
    .bind(shop)
    .bind(status)
    .bind(Utc::now() - Duration::minutes(minutes_ago))
    .fetch_one(db)
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn installations_are_read_across_shops_without_their_access_token() {
    let test = TestApp::new(create_config([noop_flow("sync")])).await;
    install(&test.db, "default", SHOP, Some("shpat_secret"), false).await;
    install(&test.db, "default", OTHER_SHOP, Some("shpat_secret"), true).await;
    install(&test.db, "second", SHOP, Some("shpat_secret"), false).await;

    // A shop that started the installation without finishing it.
    sqlx::query("INSERT INTO installations (shop, nonce) VALUES ('pending.myshopify.com', 'nonce')")
        .execute(&test.db)
        .await
        .unwrap();

    assert_eq!(Installation::all_shops(&test.db).count().await.unwrap(), 4);
    assert_eq!(Installation::installed(&test.db).count().await.unwrap(), 2);
    let installed = Installation::installed(&test.db)
        .filter(Installation::APP_HANDLE.eq("default"))
        .all()
        .await
        .unwrap();
    assert_eq!(installed.len(), 1);
    assert_eq!(installed[0].shop, SHOP);

    let active = Installation::installed(&test.db)
        .order_by(Installation::APP_HANDLE.asc())
        .all()
        .await
        .unwrap();
    let summary: Vec<(&str, &str)> = active
        .iter()
        .map(|installation| (installation.app_handle.as_str(), installation.shop.as_str()))
        .collect();
    assert_eq!(summary, [("default", SHOP), ("second", SHOP)]);
    assert_eq!(active[0].scopes.as_deref(), Some("read_orders"));

    // The token and the OAuth nonce are not columns of the record.
    assert!(!Installation::COLUMNS.contains(&"access_token"));
    assert!(!Installation::COLUMNS.contains(&"nonce"));
    let serialized = json!(active[0]).to_string();
    assert!(!serialized.contains("shpat_secret"), "{serialized}");
    assert!(serialized.contains("\"appHandle\":\"default\""), "{serialized}");

    test.cleanup().await;
}

#[tokio::test]
async fn runs_are_filtered_sorted_and_counted_across_shops() {
    let test = TestApp::new(create_config([noop_flow("sync")])).await;
    let old_failure = run(&test.db, "default", SHOP, "failed", 120).await;
    let failure = run(&test.db, "default", OTHER_SHOP, "failed", 5).await;
    let completed = run(&test.db, "default", SHOP, "completed", 1).await;

    let failed = FlowRun::all_shops(&test.db)
        .filter(FlowRun::STATUS.eq("failed"))
        .order_by(FlowRun::CREATED_AT.desc())
        .all()
        .await
        .unwrap();
    assert_eq!(
        failed.iter().map(|run| run.id).collect::<Vec<_>>(),
        [failure, old_failure]
    );
    assert_eq!(failed[0].shop, OTHER_SHOP);
    assert_eq!(failed[0].error.as_deref(), Some("boom"));
    assert_eq!(failed[0].input, Some(json!({ "limit": 10 })));

    let recent = FlowRun::all_shops(&test.db)
        .filter(FlowRun::CREATED_AT.gte(Utc::now() - Duration::minutes(30)))
        .count()
        .await
        .unwrap();
    assert_eq!(recent, 2);

    let latest = FlowRun::all_shops(&test.db)
        .order_by(FlowRun::CREATED_AT.desc())
        .limit(1)
        .all()
        .await
        .unwrap();
    assert_eq!(latest[0].id, completed);
    assert_eq!(
        FlowRun::all_shops(&test.db)
            .filter(FlowRun::STATUS.is_in(["pending", "running"]))
            .count()
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        FlowRun::all_shops(&test.db).find(failure).await.unwrap().unwrap().shop,
        OTHER_SHOP
    );

    test.cleanup().await;
}

#[tokio::test]
async fn the_runs_of_a_context_are_those_of_its_shop_and_its_app() {
    let test = TestApp::new(create_config([noop_flow("sync")])).await;
    let mine = run(&test.db, "default", SHOP, "completed", 3).await;
    let other_shop = run(&test.db, "default", OTHER_SHOP, "completed", 2).await;
    let other_app = run(&test.db, "second", SHOP, "completed", 1).await;

    let options = AdminTestOptions {
        shop: SHOP.to_string(),
        ..Default::default()
    };
    let ctx = admin_context(&test.app, options).await.unwrap();

    let runs = FlowRun::of(&ctx).all().await.unwrap();
    assert_eq!(runs.iter().map(|run| run.id).collect::<Vec<_>>(), [mine]);
    assert_eq!(FlowRun::of(&ctx).count().await.unwrap(), 1);

    // A run of another shop, or of another app on the same shop, is not found by its id.
    assert!(FlowRun::of(&ctx).find(mine).await.unwrap().is_some());
    assert!(FlowRun::of(&ctx).find(other_shop).await.unwrap().is_none());
    assert!(FlowRun::of(&ctx).find(other_app).await.unwrap().is_none());

    test.cleanup().await;
}

#[tokio::test]
async fn the_steps_and_the_logs_of_a_run_are_read_by_its_id() {
    let test = TestApp::new(create_config([noop_flow("sync")])).await;
    let failed = run(&test.db, "default", SHOP, "failed", 2).await;
    let other = run(&test.db, "default", SHOP, "completed", 1).await;

    for (run, step, status, output, error) in [
        (failed, "fetch-orders", "completed", Some("[1,2]"), None),
        (failed, "send-to-warehouse", "failed", None, Some("502 Bad Gateway")),
        (other, "fetch-orders", "completed", Some("[]"), None),
    ] {
        sqlx::query(
            "INSERT INTO step_results (flow_run_id, step_name, status, output, error, duration_ms) \
             VALUES ($1, $2, $3, $4::json, $5, 12)",
        )
        .bind(run.to_string())
        .bind(step)
        .bind(status)
        .bind(output)
        .bind(error)
        .execute(&test.db)
        .await
        .unwrap();
    }
    for (run, level, message) in [
        (failed, "info", "Fetched 2 orders"),
        (failed, "error", "Push failed"),
        (other, "info", "Nothing to do"),
    ] {
        sqlx::query("INSERT INTO logs (flow_run_id, level, message, payload) VALUES ($1, $2, $3, '{\"count\":2}')")
            .bind(run.to_string())
            .bind(level)
            .bind(message)
            .execute(&test.db)
            .await
            .unwrap();
    }

    let steps = StepResult::of_run(&test.db, failed)
        .order_by(StepResult::STEP_NAME.asc())
        .all()
        .await
        .unwrap();
    let summary: Vec<(&str, &str)> = steps
        .iter()
        .map(|step| (step.step_name.as_str(), step.status.as_str()))
        .collect();
    assert_eq!(
        summary,
        [("fetch-orders", "completed"), ("send-to-warehouse", "failed")]
    );
    assert_eq!(steps[0].output, Some(json!([1, 2])));
    assert_eq!(steps[1].error.as_deref(), Some("502 Bad Gateway"));

    let errors = LogRow::of_run(&test.db, failed)
        .filter(LogRow::LEVEL.eq("error"))
        .all()
        .await
        .unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].message.as_deref(), Some("Push failed"));
    assert_eq!(errors[0].payload, Some(json!({ "count": 2 })));
    assert_eq!(LogRow::of_run(&test.db, failed).count().await.unwrap(), 2);
    assert_eq!(LogRow::of_run(&test.db, other).count().await.unwrap(), 1);

    test.cleanup().await;
}
