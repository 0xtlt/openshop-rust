//! A shop Shopify no longer serves (#96): the Admin API answers `401`, `402`,
//! `403`, `404` or `423` to every request of the installation. The run is not
//! a failure, a `401` to the offline token marks the installation
//! uninstalled, and `openshop::shop_unavailable` reads the status.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::common::{find_run, install};
use openshop::builder::Flow;
use openshop::engine::{RunFlowOptions, run_flow};
use openshop::testing::{MockTransport, TestContext, TestOptions};
use openshop::{
    DispatchOptions, FlowContext, OpenShopConfig, RetryPolicy, RunOutcome, ShopUnavailable, Uuid, graphql, json,
    shop_unavailable,
};

const OTHER_SHOP: &str = "other.myshopify.com";

/// The flow of the issue: one step that reads the shop. With `{ "raw": true }`
/// it reads it with the client of `ctx.shopify.api()`, with `{ "shop": … }`
/// it reads another shop's.
fn check_flow() -> Flow {
    Flow::new("check", |ctx: FlowContext| async move {
        ctx.step
            .run("read", || async {
                let mut shopify = ctx.shopify.clone();
                if let Some(shop) = ctx.input["shop"].as_str() {
                    shopify.shop = shop.to_string();
                }
                if ctx.input["raw"] == true {
                    shopify
                        .api()
                        .await?
                        .execute(graphql!("query Shop { shop { id } }"))
                        .await?;
                } else {
                    shopify.execute(graphql!("query Shop { shop { id } }")).await?;
                }
                Ok(())
            })
            .await
    })
    .retry_policy(
        RetryPolicy::new()
            .max_attempts(3)
            .initial_interval(Duration::from_millis(1)),
    )
}

async fn context_with(on_error: Arc<AtomicUsize>) -> TestContext {
    let config = OpenShopConfig::builder()
        .flow(check_flow())
        .on_error(move |_error, _context| {
            on_error.fetch_add(1, Ordering::SeqCst);
            async {}
        })
        .build()
        .unwrap();
    TestContext::with_options(config, TestOptions::installed())
        .await
        .expect("test context")
}

async fn context() -> TestContext {
    context_with(Arc::default()).await
}

/// Queues a run of `check` with its retry policy, and executes it.
async fn dispatched(ctx: &TestContext, input: openshop::Value) -> (Uuid, RunOutcome) {
    let run_id = ctx
        .app
        .dispatch_flow(DispatchOptions::new("check", &ctx.shop).input(input.clone()))
        .await
        .unwrap()
        .run_id;
    let outcome = run_flow(
        &ctx.app,
        RunFlowOptions::new(run_id, "check", &ctx.shop)
            .input(input)
            .connectors(ctx.fakes.connectors()),
    )
    .await
    .unwrap();
    (run_id, outcome)
}

/// The access token and `uninstalled_at` of the installation of `shop`.
async fn installation(ctx: &TestContext, shop: &str) -> (Option<String>, bool) {
    sqlx::query_as("SELECT access_token, uninstalled_at IS NOT NULL FROM installations WHERE shop = $1")
        .bind(shop)
        .fetch_one(&ctx.db)
        .await
        .unwrap()
}

fn answer(ctx: &TestContext, status: u16) {
    ctx.shopify
        .respond("Shop", MockTransport::json(status, json!({ "errors": "Not Found" })));
}

#[tokio::test]
async fn a_closed_shop_does_not_fail_the_run() {
    let on_error = Arc::new(AtomicUsize::new(0));
    let ctx = context_with(on_error.clone()).await;
    answer(&ctx, 404);

    // The reproduction of the issue.
    assert_eq!(
        ctx.run_flow("check", json!({})).await.unwrap(),
        RunOutcome::ShopUnavailable { status: 404 }
    );

    // A run with a retry policy is not tried again: it ends, with what Shopify said.
    let (run_id, outcome) = dispatched(&ctx, json!({})).await;
    assert_eq!(outcome, RunOutcome::ShopUnavailable { status: 404 });
    let run = find_run(&ctx.db, run_id).await;
    assert_eq!(run.status, "canceled");
    assert!(run.completed_at.is_some());
    let error = run.error.unwrap();
    assert!(
        error.starts_with(
            "Shop \"test.myshopify.com\" is unavailable to app \"default\": the Admin API answered 404 (the shop is closed): "
        ),
        "{error}"
    );
    assert_eq!(on_error.load(Ordering::SeqCst), 0);

    // The shop may come back: it stays installed.
    assert_eq!(
        installation(&ctx, &ctx.shop).await,
        (Some("test-access-token".to_string()), false)
    );
    ctx.shutdown().await.unwrap();
}

#[openshop::test(context())]
async fn a_frozen_blocked_or_locked_shop_is_unavailable_and_stays_installed(ctx: &TestContext) {
    for status in [402, 403, 423] {
        answer(ctx, status);
        assert_eq!(
            ctx.run_flow("check", json!({})).await.unwrap(),
            RunOutcome::ShopUnavailable { status },
        );
        // The same through the client of `api()`.
        assert_eq!(
            ctx.run_flow("check", json!({ "raw": true })).await.unwrap(),
            RunOutcome::ShopUnavailable { status },
        );
        assert!(!installation(ctx, &ctx.shop).await.1, "{status}");
    }
}

#[openshop::test(context())]
async fn other_failures_of_the_admin_api_still_fail_the_run(ctx: &TestContext) {
    answer(ctx, 500);
    let (_, outcome) = dispatched(ctx, json!({})).await;
    assert!(
        matches!(outcome, RunOutcome::Failed { will_retry: true, .. }),
        "{outcome:?}"
    );

    // Another shop than the run's that is unavailable is a failure of the run.
    install(&ctx.db, "default", OTHER_SHOP, Some("other-token"), false).await;
    answer(ctx, 404);
    let outcome = ctx.run_flow("check", json!({ "shop": OTHER_SHOP })).await.unwrap();
    assert!(matches!(outcome, RunOutcome::Failed { .. }), "{outcome:?}");
    assert!(!installation(ctx, OTHER_SHOP).await.1);
}

#[openshop::test(context())]
async fn a_refused_offline_token_marks_the_installation_uninstalled(ctx: &TestContext) {
    answer(ctx, 401);
    assert_eq!(
        ctx.run_flow("check", json!({})).await.unwrap(),
        RunOutcome::ShopUnavailable { status: 401 }
    );
    // As `app/uninstalled` would: a cron of `.all_shops()` skips the shop now.
    assert_eq!(installation(ctx, &ctx.shop).await, (None, true));
    let installed: Vec<(String,)> =
        sqlx::query_as("SELECT shop FROM installations WHERE access_token IS NOT NULL AND uninstalled_at IS NULL")
            .fetch_all(&ctx.db)
            .await
            .unwrap();
    assert!(installed.is_empty());

    // The token exchange of the next session installs the shop again.
    ctx.transport.respond_with(|request| {
        Ok(match request.body["grant_type"].as_str() {
            Some("urn:ietf:params:oauth:grant-type:token-exchange") => MockTransport::json(
                200,
                json!({ "access_token": "reinstalled-token", "scope": "read_products" }),
            ),
            _ => MockTransport::json(400, json!({ "error": "shop_not_permitted" })),
        })
    });
    assert_eq!(ctx.get("/api/flows").send().await.unwrap().status(), 200);
    assert_eq!(
        installation(ctx, &ctx.shop).await,
        (Some("reinstalled-token".to_string()), false)
    );
}

#[openshop::test(context())]
async fn a_401_leaves_a_token_stored_since_in_place(ctx: &TestContext) {
    answer(ctx, 401);
    let shopify = ctx.app.shopify_client(&ctx.shop, "default");
    assert!(shopify.execute(graphql!("query Shop { shop { id } }")).await.is_err());
    assert_eq!(installation(ctx, &ctx.shop).await, (None, true));

    // Installed again with another token, which a request reads.
    let reinstall = |token: &'static str| {
        sqlx::query("UPDATE installations SET access_token = $1, uninstalled_at = NULL WHERE shop = $2")
            .bind(token)
            .bind(&ctx.shop)
            .execute(&ctx.db)
    };
    reinstall("second-token").await.unwrap();
    ctx.transport.delay_responses(Duration::from_millis(300));
    let shopify = ctx.app.shopify_client(&ctx.shop, "default");
    let (refused, stored) = tokio::join!(shopify.execute(graphql!("query Shop { shop { id } }")), async {
        // While Shopify answers, the shop is installed once more.
        tokio::time::sleep(Duration::from_millis(100)).await;
        reinstall("third-token").await.unwrap()
    });
    assert_eq!(shop_unavailable(&refused.unwrap_err()), Some(401));
    assert_eq!(stored.rows_affected(), 1);
    // The 401 was the answer to the second token: the third one stays.
    assert_eq!(
        installation(ctx, &ctx.shop).await,
        (Some("third-token".to_string()), false)
    );
}

#[openshop::test(context())]
async fn shop_unavailable_reads_the_status_of_an_admin_api_error(ctx: &TestContext) {
    let shopify = ctx.app.shopify_client(&ctx.shop, "default");
    answer(ctx, 404);

    let error = shopify
        .execute(graphql!("query Shop { shop { id } }"))
        .await
        .unwrap_err();
    assert_eq!(shop_unavailable(&error), Some(404));
    assert_eq!(
        error.downcast_ref::<ShopUnavailable>(),
        Some(&ShopUnavailable {
            shop: ctx.shop.clone(),
            shopify_app: "default".to_string(),
            status: 404,
        })
    );
    // What Shopify answered is still there.
    let answered = error.downcast_ref::<openshop::shopify_api::Error>().unwrap();
    assert_eq!(answered.status(), Some(404));

    // The error of the client of `api()`, as an application gets it.
    let raw: openshop::Error = shopify
        .api()
        .await
        .unwrap()
        .execute(graphql!("query Shop { shop { id } }"))
        .await
        .unwrap_err()
        .into();
    assert_eq!(shop_unavailable(&raw.context("while reading the shop")), Some(404));

    // Anything else is not an unavailable shop.
    answer(ctx, 500);
    let error = shopify
        .execute(graphql!("query Shop { shop { id } }"))
        .await
        .unwrap_err();
    assert_eq!(shop_unavailable(&error), None);
    assert_eq!(shop_unavailable(&openshop::anyhow!("Not Found")), None);
}
