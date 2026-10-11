mod common;

use openshop::testing::TestContext;
use openshop::{CronShops, RunOutcome, json};
use openshop_demo_public::APP;
use openshop_demo_public::flows::check_stock::check_stock;
use openshop_demo_public::models::stock_settings::{NewStockSettings, StockSettings};

/// What the last check asked Shopify for.
fn searched(ctx: &TestContext) -> String {
    let request = ctx.transport.last_request().unwrap();
    request.body["variables"]["query"].as_str().unwrap().to_string()
}

#[openshop::test(APP)]
async fn looks_for_the_variants_under_the_threshold_of_the_shop(ctx: &TestContext) {
    common::answer_shopify(ctx);
    StockSettings::of(ctx)
        .insert(NewStockSettings { threshold: 12 })
        .await
        .unwrap();

    let outcome = ctx.run_flow(check_stock::NAME, json!({})).await.unwrap();

    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(searched(ctx), "inventory_quantity:<12");
}

#[openshop::test(APP)]
async fn a_shop_that_saved_nothing_is_checked_with_the_default_threshold(ctx: &TestContext) {
    common::answer_shopify(ctx);
    // What another shop chose is not read.
    StockSettings::for_shop(&ctx.db, "another-shop.myshopify.com")
        .insert(NewStockSettings { threshold: 12 })
        .await
        .unwrap();

    let outcome = ctx.run_flow(check_stock::NAME, json!({})).await.unwrap();

    assert_eq!(outcome, RunOutcome::Completed);
    assert_eq!(
        searched(ctx),
        format!("inventory_quantity:<{}", StockSettings::DEFAULT_THRESHOLD)
    );
}

#[test]
fn every_shop_is_checked_on_the_hour() {
    let config = APP.config().unwrap();

    assert_eq!(config.crons.len(), 1);
    assert_eq!(config.crons[0].flow, check_stock::NAME);
    assert_eq!(config.crons[0].schedule, "0 * * * *");
    assert_eq!(config.crons[0].shops, Some(CronShops::All));
}
