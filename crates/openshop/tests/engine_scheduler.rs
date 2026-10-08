mod common;

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use common::{TestApp, noop_flow, test_env};
use openshop::builder::Flow;
use openshop::db::schema::FlowRun;
use openshop::engine::Scheduler;
use openshop::testing::{MockTransport, TestDb};
use openshop::{Concurrency, Cron, IndexMap, OpenShop, OpenShopConfig, json};

const EVERY_SECOND: &str = "* * * * * *";
/// Long enough for an every-second cron to fire at least once.
const ONE_TICK: Duration = Duration::from_millis(1_200);

fn config(flows: impl IntoIterator<Item = Flow>, crons: impl IntoIterator<Item = Cron>) -> OpenShopConfig {
    let builder = flows
        .into_iter()
        .fold(OpenShopConfig::builder(), |builder, flow| builder.flow(flow));
    crons
        .into_iter()
        .fold(builder, |builder, cron| builder.cron(cron))
        .build()
        .expect("valid config")
}

fn concurrent_flow(name: &str) -> Flow {
    noop_flow(name).concurrency(Concurrency::Allow)
}

async fn runs_of(test: &TestApp, flow: &str) -> Vec<FlowRun> {
    sqlx::query_as("SELECT * FROM flow_runs WHERE flow_name = $1 ORDER BY created_at")
        .bind(flow)
        .fetch_all(&test.db)
        .await
        .unwrap()
}

/// Polls until the cron has queued runs satisfying `done`.
async fn wait_for_runs(test: &TestApp, flow: &str, done: impl Fn(&[FlowRun]) -> bool) -> Vec<FlowRun> {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let runs = runs_of(test, flow).await;
        if done(&runs) {
            return runs;
        }
        assert!(
            Instant::now() < deadline,
            "cron for \"{flow}\" did not queue the expected runs, got {} run(s)",
            runs.len()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn targets(runs: &[FlowRun]) -> BTreeSet<(&str, &str)> {
    runs.iter()
        .map(|run| (run.app_handle.as_str(), run.shop.as_str()))
        .collect()
}

fn has_shops(shops: &'static [&'static str]) -> impl Fn(&[FlowRun]) -> bool {
    move |runs| shops.iter().all(|shop| runs.iter().any(|run| run.shop == *shop))
}

async fn install(test: &TestApp, app_handle: &str, shop: &str, uninstalled: bool) {
    sqlx::query(
        "INSERT INTO installations (app_handle, shop, access_token, scopes, uninstalled_at) \
         VALUES ($1, $2, 'token', 'read_products', CASE WHEN $3 THEN now() END)",
    )
    .bind(app_handle)
    .bind(shop)
    .bind(uninstalled)
    .execute(&test.db)
    .await
    .unwrap();
}

async fn override_cron(test: &TestApp, shop: &str, cron_key: &str, enabled: bool) {
    sqlx::query("INSERT INTO cron_overrides (shop, cron_key, enabled) VALUES ($1, $2, $3)")
        .bind(shop)
        .bind(cron_key)
        .bind(enabled)
        .execute(&test.db)
        .await
        .unwrap();
}

#[tokio::test]
async fn registers_cron_and_triggers_flow_dispatch() {
    let test = TestApp::new(config(
        [noop_flow("quick-flow")],
        [Cron::new(EVERY_SECOND, "quick-flow")],
    ))
    .await;

    let mut scheduler = Scheduler::start(&test.app).unwrap();
    assert_eq!(scheduler.job_count(), 1);
    let runs = wait_for_runs(&test, "quick-flow", |runs| !runs.is_empty()).await;
    scheduler.stop();

    assert_eq!(runs[0].shop, "__global__");
    assert_eq!(runs[0].app_handle, "default");
    assert_eq!(runs[0].status, "pending");
    assert_eq!(runs[0].input, Some(json!({})));
    test.cleanup().await;
}

#[tokio::test]
async fn keeps_ticking_after_a_rejected_dispatch() {
    let test = TestApp::new(config(
        [noop_flow("quick-flow")],
        [Cron::new(EVERY_SECOND, "quick-flow")],
    ))
    .await;
    let mut scheduler = Scheduler::start(&test.app).unwrap();

    // While the first run is still queued, later ticks are rejected by the flow's concurrency.
    wait_for_runs(&test, "quick-flow", |runs| !runs.is_empty()).await;
    tokio::time::sleep(ONE_TICK).await;
    assert_eq!(runs_of(&test, "quick-flow").await.len(), 1);

    sqlx::query("UPDATE flow_runs SET status = 'completed'")
        .execute(&test.db)
        .await
        .unwrap();
    let runs = wait_for_runs(&test, "quick-flow", |runs| runs.len() >= 2).await;
    scheduler.stop();

    assert_eq!(runs[1].status, "pending");
    test.cleanup().await;
}

// TS "skips unknown flow without crashing": such a configuration cannot reach the
// scheduler here, it is rejected when the configuration or the application is built.
#[tokio::test]
async fn cron_referencing_an_unknown_flow_is_rejected_before_the_scheduler_starts() {
    let cron = Cron::new("* * * * *", "nonexistent");
    let expected = "[openshop] Invalid config: crons[0].flow references unknown flow \"nonexistent\"";

    let error = OpenShopConfig::builder()
        .flow(noop_flow("quick-flow"))
        .cron(cron.clone())
        .build()
        .err()
        .expect("rejected by the builder");
    assert_eq!(error.to_string(), expected);

    let literal = OpenShopConfig {
        flows: IndexMap::from([("quick-flow".to_string(), noop_flow("quick-flow"))]),
        crons: vec![cron],
        ..Default::default()
    };
    let db = TestDb::create().await.unwrap();
    let error = OpenShop::with_transport(literal, db.pool.clone(), test_env(), MockTransport::new())
        .err()
        .expect("rejected by the application");
    assert_eq!(error.to_string(), expected);
    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn invalid_schedules_are_rejected_before_the_scheduler_starts() {
    // The last two have the right shape and are refused by the cron parser itself.
    for (schedule, reason) in [
        ("* * *", "expected 5 or 6 fields, got 3"),
        ("61 * * * *", ""),
        ("@sometimes", ""),
    ] {
        let error = OpenShopConfig::builder()
            .flow(noop_flow("quick-flow"))
            .cron(Cron::new(schedule, "quick-flow"))
            .build()
            .err()
            .unwrap_or_else(|| panic!("\"{schedule}\" must be rejected"))
            .to_string();
        let prefix = format!(
            "[openshop] Invalid config: crons[0].schedule is invalid: \
             Invalid cron schedule \"{schedule}\" for flow \"quick-flow\": "
        );
        assert!(error.starts_with(&prefix) && error.ends_with(reason), "{error}");
    }
}

#[tokio::test]
async fn resolves_shops_all_from_installations() {
    let test = TestApp::new(config(
        [concurrent_flow("quick-flow")],
        [Cron::new(EVERY_SECOND, "quick-flow").all_shops()],
    ))
    .await;
    install(&test, "default", "shop-a.myshopify.com", false).await;
    install(&test, "default", "shop-b.myshopify.com", false).await;
    install(&test, "client-b", "shop-c.myshopify.com", false).await;
    install(&test, "default", "gone.myshopify.com", true).await;

    let mut scheduler = Scheduler::start(&test.app).unwrap();
    wait_for_runs(
        &test,
        "quick-flow",
        has_shops(&["shop-a.myshopify.com", "shop-b.myshopify.com", "shop-c.myshopify.com"]),
    )
    .await;
    // Let the tick finish before looking for runs that must not exist.
    tokio::time::sleep(Duration::from_millis(150)).await;
    scheduler.stop();

    let runs = runs_of(&test, "quick-flow").await;
    assert_eq!(
        targets(&runs),
        BTreeSet::from([
            ("client-b", "shop-c.myshopify.com"),
            ("default", "shop-a.myshopify.com"),
            ("default", "shop-b.myshopify.com"),
        ])
    );
    test.cleanup().await;
}

#[tokio::test]
async fn shops_all_is_resolved_again_at_every_tick() {
    let test = TestApp::new(config(
        [noop_flow("quick-flow")],
        [Cron::new(EVERY_SECOND, "quick-flow").all_shops()],
    ))
    .await;
    let mut scheduler = Scheduler::start(&test.app).unwrap();

    // Nothing is installed yet: ticks have nothing to queue, and the cron stays registered.
    tokio::time::sleep(ONE_TICK).await;
    assert!(runs_of(&test, "quick-flow").await.is_empty());

    install(&test, "default", "late.myshopify.com", false).await;
    let runs = wait_for_runs(&test, "quick-flow", |runs| !runs.is_empty()).await;
    scheduler.stop();

    assert_eq!(targets(&runs), BTreeSet::from([("default", "late.myshopify.com")]));
    test.cleanup().await;
}

#[tokio::test]
async fn respects_cron_override_disabled() {
    let test = TestApp::new(config(
        [noop_flow("quick-flow"), noop_flow("witness-flow")],
        [
            Cron::new(EVERY_SECOND, "quick-flow"),
            Cron::new(EVERY_SECOND, "witness-flow"),
        ],
    ))
    .await;
    override_cron(&test, "__global__", "quick-flow:* * * * * *", false).await;
    // An override for another cron of the same shop changes nothing.
    override_cron(&test, "__global__", "witness-flow:0 3 * * *", false).await;

    let mut scheduler = Scheduler::start(&test.app).unwrap();
    assert_eq!(scheduler.job_count(), 2);
    // Both crons fire on the same second: once the witness ran, the disabled one was skipped.
    wait_for_runs(&test, "witness-flow", |runs| !runs.is_empty()).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    scheduler.stop();

    assert!(runs_of(&test, "quick-flow").await.is_empty());
    test.cleanup().await;
}

#[tokio::test]
async fn cron_override_only_disables_its_own_shop() {
    let shops = vec![
        "shop-off.myshopify.com".to_string(),
        "shop-on.myshopify.com".to_string(),
    ];
    let cron = Cron::new(EVERY_SECOND, "quick-flow").shops(shops);
    let key = cron.key();
    assert_eq!(key, "quick-flow:* * * * * *");
    let test = TestApp::new(config([concurrent_flow("quick-flow")], [cron])).await;
    override_cron(&test, "shop-off.myshopify.com", &key, false).await;
    override_cron(&test, "shop-on.myshopify.com", &key, true).await;

    let mut scheduler = Scheduler::start(&test.app).unwrap();
    // Shops are handled in order, so the disabled one was decided before this run appeared.
    let runs = wait_for_runs(&test, "quick-flow", has_shops(&["shop-on.myshopify.com"])).await;
    scheduler.stop();

    assert!(runs.iter().all(|run| run.shop == "shop-on.myshopify.com"));
    test.cleanup().await;
}

#[tokio::test]
async fn stop_clears_all_crons() {
    let test = TestApp::new(config(
        [noop_flow("quick-flow")],
        [Cron::new(EVERY_SECOND, "quick-flow")],
    ))
    .await;

    let mut scheduler = Scheduler::start(&test.app).unwrap();
    scheduler.stop();
    assert_eq!(scheduler.job_count(), 0);
    // Dropping a scheduler stops it as well.
    drop(Scheduler::start(&test.app).unwrap());

    tokio::time::sleep(ONE_TICK).await;
    assert!(runs_of(&test, "quick-flow").await.is_empty());
    test.cleanup().await;
}

#[tokio::test]
async fn resolves_shops_as_string_single_shop() {
    let test = TestApp::new(config(
        [noop_flow("quick-flow")],
        [Cron::new(EVERY_SECOND, "quick-flow").shop("specific.myshopify.com")],
    ))
    .await;

    let mut scheduler = Scheduler::start(&test.app).unwrap();
    let runs = wait_for_runs(&test, "quick-flow", |runs| !runs.is_empty()).await;
    scheduler.stop();

    // Without an installation the shop still runs, under the default app.
    assert_eq!(targets(&runs), BTreeSet::from([("default", "specific.myshopify.com")]));
    test.cleanup().await;
}

#[tokio::test]
async fn single_shop_runs_once_per_app_it_is_installed_on() {
    let test = TestApp::new(config(
        [noop_flow("quick-flow")],
        [Cron::new(EVERY_SECOND, "quick-flow").shop("specific.myshopify.com")],
    ))
    .await;
    install(&test, "client-a", "specific.myshopify.com", false).await;
    install(&test, "client-b", "specific.myshopify.com", false).await;
    install(&test, "client-c", "specific.myshopify.com", true).await;
    install(&test, "client-a", "other.myshopify.com", false).await;

    let mut scheduler = Scheduler::start(&test.app).unwrap();
    wait_for_runs(&test, "quick-flow", |runs| runs.len() >= 2).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    scheduler.stop();

    let runs = runs_of(&test, "quick-flow").await;
    assert_eq!(
        targets(&runs),
        BTreeSet::from([
            ("client-a", "specific.myshopify.com"),
            ("client-b", "specific.myshopify.com"),
        ])
    );
    test.cleanup().await;
}

#[tokio::test]
async fn resolves_shops_as_array() {
    let shops = vec!["shop-x.myshopify.com".to_string(), "shop-y.myshopify.com".to_string()];
    let test = TestApp::new(config(
        [concurrent_flow("quick-flow")],
        [Cron::new(EVERY_SECOND, "quick-flow").shops(shops)],
    ))
    .await;
    install(&test, "client-a", "shop-y.myshopify.com", false).await;

    let mut scheduler = Scheduler::start(&test.app).unwrap();
    let runs = wait_for_runs(
        &test,
        "quick-flow",
        has_shops(&["shop-x.myshopify.com", "shop-y.myshopify.com"]),
    )
    .await;
    scheduler.stop();

    assert_eq!(
        targets(&runs),
        BTreeSet::from([
            ("client-a", "shop-y.myshopify.com"),
            ("default", "shop-x.myshopify.com"),
        ])
    );
    test.cleanup().await;
}

#[tokio::test]
async fn passes_the_cron_input_to_the_run() {
    let test = TestApp::new(config(
        [noop_flow("full-sync"), noop_flow("plain-sync")],
        [
            Cron::new(EVERY_SECOND, "full-sync")
                .name("Nightly sync")
                .input(json!({ "mode": "full", "limit": 50 })),
            // Only objects are valid run input; anything else falls back to `{}`.
            Cron::new(EVERY_SECOND, "plain-sync").input(json!("not-an-object")),
        ],
    ))
    .await;

    let mut scheduler = Scheduler::start(&test.app).unwrap();
    let full = wait_for_runs(&test, "full-sync", |runs| !runs.is_empty()).await;
    let plain = wait_for_runs(&test, "plain-sync", |runs| !runs.is_empty()).await;
    scheduler.stop();

    assert_eq!(full[0].input, Some(json!({ "mode": "full", "limit": 50 })));
    assert_eq!(plain[0].input, Some(json!({})));
    test.cleanup().await;
}

#[tokio::test]
async fn registers_one_job_per_cron_including_nicknames() {
    let none = TestApp::new(config([noop_flow("quick-flow")], [])).await;
    assert_eq!(Scheduler::start(&none.app).unwrap().job_count(), 0);
    none.cleanup().await;

    let test = TestApp::new(config(
        [noop_flow("quick-flow"), noop_flow("other-flow")],
        [
            Cron::new("@yearly", "quick-flow"),
            Cron::new("0 0 1 1 *", "quick-flow").all_shops(),
            Cron::new("  0 0 0 1 1 *  ", "other-flow"),
        ],
    ))
    .await;
    let mut scheduler = Scheduler::start(&test.app).unwrap();
    assert_eq!(scheduler.job_count(), 3);
    scheduler.stop();

    let (queued,): (i64,) = sqlx::query_as("SELECT count(*) FROM flow_runs")
        .fetch_one(&test.db)
        .await
        .unwrap();
    assert_eq!(queued, 0);
    test.cleanup().await;
}
