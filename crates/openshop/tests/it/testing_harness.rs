use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

use crate::common::{LogCapture, create_config, noop_flow};
use openshop::billing::{self, ACTIVE_SUBSCRIPTIONS_OPERATION, AppSubscription, SubscriptionStatus};
use openshop::builder::Flow;
use openshop::shopify::RetryPolicy;
use openshop::testing::{
    AdminTestOptions, MockTransport, TEST_API_KEY, TestContext, TestDb, TestOptions, admin_context, in_crate,
    subscription,
};
use openshop::{FlowContext, OpenShopConfig, RunOutcome, ShopScope, json};
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection};

const OTHER_SHOP: &str = "other-shop.myshopify.com";

async fn schema_exists(schema: &str) -> bool {
    let url = openshop::testing::test_database_url().unwrap();
    let mut connection = PgConnection::connect_with(&PgConnectOptions::from_str(&url).unwrap())
        .await
        .unwrap();
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = $1)")
        .bind(schema)
        .fetch_one(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    exists
}

#[tokio::test]
async fn cleanup_drops_the_schema() {
    let db = TestDb::create().await.unwrap();
    let schema = db.schema.clone();
    assert!(schema_exists(&schema).await);

    db.cleanup().await.unwrap();
    assert!(!schema_exists(&schema).await);
}

#[tokio::test]
async fn a_database_that_was_not_cleaned_up_is_dropped_with_its_value() {
    let db = TestDb::create().await.unwrap();
    let schema = db.schema.clone();

    // What happens when an assertion fails before `cleanup`.
    drop(db);
    assert!(!schema_exists(&schema).await);
}

#[tokio::test]
async fn a_context_that_was_not_shut_down_drops_its_schema() {
    let ctx = TestContext::new(OpenShopConfig::builder().flow(noop_flow("sync")).build().unwrap())
        .await
        .unwrap();
    let schema: String = sqlx::query_scalar("SELECT current_schema()::text")
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert!(schema.starts_with("test_"));

    drop(ctx);
    assert!(!schema_exists(&schema).await);
}

#[tokio::test]
async fn flows_can_be_dispatched_for_another_shop() {
    let ctx = TestContext::new(OpenShopConfig::builder().flow(noop_flow("sync")).build().unwrap())
        .await
        .unwrap();

    let own = ctx.dispatch_flow("sync", json!({})).await.unwrap();
    let other = ctx.dispatch_flow_for("sync", json!({}), OTHER_SHOP).await.unwrap();

    let shops: Vec<(uuid::Uuid, String)> = sqlx::query_as("SELECT id, shop FROM flow_runs ORDER BY created_at, shop")
        .fetch_all(&ctx.db)
        .await
        .unwrap();
    assert_eq!(shops.len(), 2);
    assert!(shops.contains(&(own, ctx.shop.clone())));
    assert!(shops.contains(&(other, OTHER_SHOP.to_string())));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn authorization_headers_can_target_another_shop_and_user() {
    let ctx = TestContext::new(OpenShopConfig::builder().flow(noop_flow("sync")).build().unwrap())
        .await
        .unwrap();
    ctx.dispatch_flow_for("sync", json!({}), OTHER_SHOP).await.unwrap();

    let own: openshop::Value = ctx.get("/api/runs").send().await.unwrap().json().await.unwrap();
    assert_eq!(own.as_array().unwrap().len(), 0);

    let other: openshop::Value = ctx
        .http
        .get(ctx.url_for("/api/runs"))
        .header("Authorization", ctx.authorization_header_for(OTHER_SHOP, "42"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(other.as_array().unwrap().len(), 1);
    ctx.shutdown().await.unwrap();
}

// ─── The Admin API of a test ─────────────────────────────────────────

/// A flow that asks Shopify for the subscriptions of the shop, then sends
/// `query`, and fails with what Shopify answered.
fn asking_flow(name: &str, query: &'static str) -> Flow {
    Flow::new(name, move |ctx: FlowContext| async move {
        let subscriptions = ctx.shopify.active_subscriptions().await?;
        let data = ctx
            .shopify
            .graphql(query, json!({ "id": "gid://shopify/Product/1" }))
            .await?;
        ctx.logger.info_with(
            "Asked",
            json!({ "plan": subscriptions.has_plan(&["Pro"]), "data": data }),
        );
        Ok(())
    })
}

async fn installed(config: OpenShopConfig) -> TestContext {
    TestContext::with_options(config, TestOptions::installed())
        .await
        .unwrap()
}

#[tokio::test]
async fn shopify_answers_each_operation_by_its_name() {
    let ctx = installed(create_config([asking_flow(
        "ask",
        "query ProductTitle($id: ID!) { product(id: $id) { title } }",
    )]))
    .await;
    ctx.shopify
        .active_subscriptions([subscription("Pro")])
        .on("ProductTitle", json!({ "product": { "title": "Any" } }))
        .on_variables(
            "ProductTitle",
            json!({ "id": "gid://shopify/Product/1" }),
            json!({ "product": { "title": "First" } }),
        );

    let outcome = ctx.run_flow("ask", json!({})).await.unwrap();

    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(
        ctx.shopify.operations(),
        [ACTIVE_SUBSCRIPTIONS_OPERATION, "ProductTitle"]
    );
    let call = &ctx.shopify.calls_to("ProductTitle")[0];
    assert_eq!(call.variables, json!({ "id": "gid://shopify/Product/1" }));
    assert!(
        call.url.starts_with("https://test.myshopify.com/admin/api/"),
        "{}",
        call.url
    );
    assert!(ctx.shopify.unanswered().is_empty());
    ctx.shopify.assert_answered();
    // The transport recorded them too, as before.
    assert_eq!(ctx.transport.requests().len(), 2);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_operation_nothing_answers_fails_with_its_name() {
    let ctx = installed(create_config([asking_flow("ask", "query Missing { shop { name } }")])).await;
    ctx.shopify.active_subscriptions([]);

    let outcome = ctx.run_flow("ask", json!({})).await.unwrap();

    let RunOutcome::Failed { error, .. } = outcome else {
        panic!("a failure: {outcome:?}");
    };
    assert!(
        error.contains(
            "The test does not answer the Admin API operation `Missing`: answer it with `ctx.shopify.on(\"Missing\", data)`. Answered: OpenShopActiveSubscriptions."
        ),
        "{error}"
    );
    assert_eq!(ctx.shopify.unanswered(), ["Missing"]);

    // What `shopify` does not answer goes to the responder of the transport.
    ctx.transport.respond_with(|_| {
        Ok(MockTransport::json(
            200,
            json!({ "data": { "shop": { "name": "Responder" } } }),
        ))
    });
    assert_eq!(ctx.run_flow("ask", json!({})).await.unwrap(), RunOutcome::Completed);
    assert_eq!(ctx.shopify.unanswered(), ["Missing"]);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_request_that_is_not_graphql_is_left_to_the_responder() {
    let ctx = installed(create_config([noop_flow("sync")])).await;
    ctx.shopify.on("Anything", json!({}));
    // A token request: not GraphQL, a `404` without a responder.
    assert_eq!(token_request(&ctx).await, 404);
    ctx.transport.respond_json(json!({ "access_token": "token" }));
    assert_eq!(token_request(&ctx).await, 200);
    assert!(ctx.shopify.calls().is_empty() && ctx.shopify.unanswered().is_empty());
    ctx.shutdown().await.unwrap();
}

/// The status Shopify answers a token request of the client credentials
/// grant with, through the transport of the test.
async fn token_request(ctx: &TestContext) -> u16 {
    use openshop::shopify_api::transport::{HttpRequest, Method, Transport};
    let request = HttpRequest::new(Method::Post, "https://test.myshopify.com/admin/oauth/access_token")
        .with_header("Content-Type", "application/x-www-form-urlencoded")
        .with_body("grant_type=client_credentials");
    ctx.transport.send(request).await.unwrap().status
}

#[tokio::test]
async fn a_plan_is_given_to_the_billing_of_a_test_by_name() {
    let ctx = installed(OpenShopConfig::builder().build().unwrap()).await;
    let page = || admin_context(&ctx.app, AdminTestOptions::default());

    ctx.shopify.active_subscriptions([subscription("Monthly")]);
    let plan = billing::require_plan(&page().await.unwrap(), &["Monthly"])
        .await
        .unwrap();
    assert_eq!((plan.name.as_str(), plan.is_active()), ("Monthly", true));

    ctx.shopify.active_subscriptions([AppSubscription {
        status: SubscriptionStatus::Frozen,
        ..subscription("Monthly")
    }]);
    let error = billing::require_plan(&page().await.unwrap(), &[]).await.unwrap_err();
    let error = error
        .downcast_ref::<openshop::AdminPublicError>()
        .expect("a public error");
    assert_eq!((error.code.as_str(), error.status), (billing::PLAN_REQUIRED, 402));
    ctx.shutdown().await.unwrap();
}

// ─── The retries of the Shopify client ───────────────────────────────

/// A context whose Shopify fails `Flaky` with a `503`, and how many times it
/// was sent and for how long a flow that sends it ran.
async fn flaky(config: OpenShopConfig, retry: Option<RetryPolicy>) -> (usize, Duration) {
    let ctx = TestContext::with_options(
        config,
        TestOptions {
            shopify_retry: retry,
            ..TestOptions::installed()
        },
    )
    .await
    .unwrap();
    ctx.shopify.respond("Flaky", MockTransport::json(503, json!({})));
    let started = Instant::now();
    let outcome = ctx.run_flow("flaky", json!({})).await.unwrap();
    let elapsed = started.elapsed();
    assert!(matches!(outcome, RunOutcome::Failed { .. }), "{outcome:?}");
    let sent = ctx.shopify.calls_to("Flaky").len();
    ctx.shutdown().await.unwrap();
    (sent, elapsed)
}

fn flaky_flow() -> Flow {
    Flow::new("flaky", |ctx: FlowContext| async move {
        ctx.shopify.graphql("query Flaky { shop { name } }", ()).await?;
        Ok(())
    })
}

#[tokio::test]
async fn a_test_retries_shopify_as_the_app_does_without_waiting() {
    // `shopify_api`'s four retries, which wait 1, 2, 4 and 8 seconds outside a
    // test. The bound leaves room for a slow machine, far from those 15 s.
    let (sent, elapsed) = flaky(create_config([flaky_flow()]), None).await;
    assert_eq!(sent, 5);
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");

    // The policy of the application, attempts included.
    let config = OpenShopConfig::builder()
        .flow(flaky_flow())
        .shopify_retry(RetryPolicy::new(1).with_base_delay(Duration::from_secs(30)))
        .build()
        .unwrap();
    let (sent, elapsed) = flaky(config, None).await;
    assert_eq!(sent, 2);
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");

    // A test that sets one gets it as it is, waits included.
    let (sent, _) = flaky(create_config([flaky_flow()]), Some(RetryPolicy::none())).await;
    assert_eq!(sent, 1);
    let waiting = RetryPolicy::new(1)
        .with_base_delay(Duration::from_millis(300))
        .with_max_delay(Duration::from_millis(300));
    let (sent, elapsed) = flaky(create_config([flaky_flow()]), Some(waiting)).await;
    assert_eq!(sent, 2);
    assert!(elapsed >= Duration::from_millis(300), "{elapsed:?}");
}

// ─── The directory of the crate of a test ────────────────────────────

/// A directory with a `shopify.app.toml` of `client_id`, removed at the end.
fn project(client_id: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("openshop-harness-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("shopify.app.toml"),
        format!(
            "client_id = \"{client_id}\"\nname = \"Hide PayPal\"\nhandle = \"hide-paypal\"\n\n[access_scopes]\nscopes = \"write_payment_customizations\"\n"
        ),
    )
    .unwrap();
    dir
}

#[openshop::test(TestContext::with_options(create_config([noop_flow("sync")]), TestOptions::installed()))]
async fn a_test_reads_the_shopify_files_of_its_crate(ctx: &TestContext) {
    assert_eq!(ctx.app.env().cwd, Path::new(env!("CARGO_MANIFEST_DIR")));
}

#[tokio::test]
async fn the_contexts_of_a_test_are_the_app_of_its_shopify_app_toml() {
    let dir = project("0123456789abcdef0123456789abcdef");
    let manifest_dir: &'static str = Box::leak(dir.to_string_lossy().into_owned().into_boxed_str());
    let config = || create_config([noop_flow("sync")]);

    in_crate(manifest_dir, async {
        let ctx = installed(config()).await;
        // The app the file describes: its client id, its handle, its name, its scopes.
        assert_eq!(ctx.api_key, "0123456789abcdef0123456789abcdef");
        assert_eq!(ctx.app_handle(), "hide-paypal");
        assert_eq!(
            billing::pricing_link(&ctx),
            "shopify:admin/charges/hide-paypal/pricing_plans"
        );
        let app = ctx.app.apps().by_handle(Some("default")).unwrap();
        assert_eq!(
            (app.name.as_str(), app.scopes.as_str()),
            ("Hide PayPal", "write_payment_customizations")
        );
        ctx.shutdown().await.unwrap();

        // An API key of the options is kept, and a `cwd` replaces the directory.
        let elsewhere = std::env::temp_dir().join("openshop-harness-no-toml");
        let ctx = TestContext::with_options(
            config(),
            TestOptions {
                api_key: "another-key".to_string(),
                cwd: Some(elsewhere.clone()),
                ..TestOptions::installed()
            },
        )
        .await
        .unwrap();
        assert_eq!((ctx.api_key.as_str(), ctx.app_handle()), ("another-key", "another-key"));
        assert_eq!(ctx.app.env().cwd, elsewhere);
        ctx.shutdown().await.unwrap();
    })
    .await;

    // A file of the placeholder key is the app's as it is.
    let local = project(TEST_API_KEY);
    let ctx = TestContext::with_options(
        config(),
        TestOptions {
            cwd: Some(local.clone()),
            ..TestOptions::installed()
        },
    )
    .await
    .unwrap();
    assert_eq!((ctx.api_key.as_str(), ctx.app_handle()), (TEST_API_KEY, "hide-paypal"));
    ctx.shutdown().await.unwrap();

    // Outside `#[openshop::test]` and without a `cwd`, no file is read.
    let ctx = installed(config()).await;
    assert_eq!((ctx.api_key.as_str(), ctx.app_handle()), (TEST_API_KEY, TEST_API_KEY));
    ctx.shutdown().await.unwrap();
    std::fs::remove_dir_all(dir).ok();
    std::fs::remove_dir_all(local).ok();
}

// ─── The log lines of a run ──────────────────────────────────────────

#[tokio::test]
async fn the_lines_of_a_run_say_its_shop_flow_and_run_in_the_console() {
    let ctx = installed(create_config([Flow::new("sync", |ctx: FlowContext| async move {
        ctx.logger.info_with("Harness line", json!({ "count": 1 }));
        Ok(())
    })]))
    .await;
    let logs = LogCapture::start().await;

    // The runner gives a run of a worker the same logger.
    assert_eq!(ctx.run_flow("sync", json!({})).await.unwrap(), RunOutcome::Completed);

    let line = logs
        .lines()
        .into_iter()
        .find(|line| line.message == "Harness line")
        .expect("the line of the flow");
    drop(logs);
    let (run_id,): (uuid::Uuid,) = sqlx::query_as("SELECT id FROM flow_runs")
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(
        line.context,
        Some(json!({
            "count": 1,
            "shop": "test.myshopify.com",
            "shopifyApp": "default",
            "flow": "sync",
            "runId": run_id.to_string(),
        }))
    );
    ctx.shutdown().await.unwrap();
}
