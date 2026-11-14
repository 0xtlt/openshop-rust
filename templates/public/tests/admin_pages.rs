mod common;

use openshop::testing::{TestContext, WithLocale, assert_translated, toast};
use openshop::{FlowRun, Outcome, RunOutcome, Value, json};

use openshop_template_public::APP;
use openshop_template_public::admin::home::{HomePage, Summary};
use openshop_template_public::admin::settings::{Settings, SettingsPage};
use openshop_template_public::flows::check_stock::check_stock;
use openshop_template_public::models::stock_settings::StockSettings;

#[openshop::test(APP)]
async fn a_merchant_reaches_the_pages_of_the_application_and_nothing_else(ctx: &TestContext) {
    let builtin: Value = ctx.get("/api/pages").send().await.unwrap().json().await.unwrap();
    assert!(builtin.as_object().unwrap().values().all(|mode| mode == "disabled"));
    // Nothing of the flows, the runs or the crons of the application.
    for path in ["/api/flows", "/api/runs", "/api/crons", "/flows", "/crons"] {
        assert_eq!(ctx.page(path).send().await.unwrap().status(), 404, "{path}");
    }

    // The home has no entry in the menu: the name of the app opens it.
    let home = ctx.page(HomePage::PATH).send().await.unwrap().text().await.unwrap();
    assert!(
        home.contains(r#"<ui-nav-menu><a href="/" rel="home">Home</a><a href="/settings">Settings</a></ui-nav-menu>"#),
        "{home}"
    );
    assert!(home.contains(r#"<s-page heading="Stock watch">"#), "{home}");
}

#[openshop::test(APP)]
async fn the_home_shows_what_the_last_check_found(ctx: &TestContext) {
    common::answer_shopify(ctx);

    let before: Summary = ctx.call_loader(HomePage::PATH, "summary", ()).await.unwrap();
    assert_eq!(before.threshold, 5);
    assert!(before.checked_at.is_none() && before.low_stock.is_empty() && !before.checking);

    let outcome = ctx.run_flow(check_stock::NAME, json!({})).await.unwrap();
    assert_eq!(outcome, RunOutcome::Completed);

    let after: Summary = ctx.call_loader(HomePage::PATH, "summary", ()).await.unwrap();
    assert!(after.checked_at.is_some());
    let found: Vec<(&str, Option<&str>, i64)> = after
        .low_stock
        .iter()
        .map(|variant| (variant.name.as_str(), variant.sku.as_deref(), variant.available))
        .collect();
    assert_eq!(
        found,
        [
            ("Canvas tote - Natural", None, 0),
            ("Linen shirt - M", Some("SHIRT-M"), 2)
        ]
    );

    // The page the server renders with it.
    let page = ctx.page(HomePage::PATH).send().await.unwrap().text().await.unwrap();
    assert!(page.contains("2 variants run low."), "{page}");
    assert!(page.contains(r#"<s-badge tone="critical">0 left</s-badge>"#), "{page}");
    assert!(page.contains(r#"<s-text color="subdued">SHIRT-M</s-text>"#), "{page}");
}

#[openshop::test(APP)]
async fn check_now_queues_a_check_for_the_shop_of_the_session(ctx: &TestContext) {
    let checked = ctx
        .submit(HomePage::PATH, &[("_action", HomePage::CHECK_NOW)])
        .send()
        .await
        .unwrap();
    assert_eq!(checked.status(), 200);
    assert_eq!(toast(&checked).as_deref(), Some("Stock check started"));
    // The page again: a check runs, and the page is fetched again until it ends.
    let page = checked.text().await.unwrap();
    assert!(
        page.contains(r#"<s-banner tone="info" data-openshop-refresh="5000">"#),
        "{page}"
    );

    let queued = FlowRun::of(ctx).all().await.unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].flow_name, check_stock::NAME);
    assert_eq!(queued[0].status, "pending");
}

#[openshop::test(APP)]
async fn a_merchant_saves_the_threshold_of_their_shop(ctx: &TestContext) {
    let defaults: Settings = ctx.call_loader(SettingsPage::PATH, "settings", ()).await.unwrap();
    assert_eq!(defaults.threshold, StockSettings::DEFAULT_THRESHOLD);

    for threshold in [12, 20] {
        let saved: Outcome = ctx
            .call_action(SettingsPage::PATH, "saveSettings", json!({ "threshold": threshold }))
            .await
            .unwrap();
        assert_eq!(
            saved,
            Outcome::toast(format!("A variant runs low under {threshold} units"))
        );
    }
    // One row per shop, however many times its merchant saves.
    let rows = StockSettings::of(ctx).all().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].threshold, 20);

    // The form of the page: what was saved, and a number out of range refused under its field.
    let page = ctx.page(SettingsPage::PATH).send().await.unwrap().text().await.unwrap();
    assert!(page.contains(r#"value="20""#), "{page}");
    let refused = ctx
        .submit(
            SettingsPage::PATH,
            &[("_action", SettingsPage::SAVE_SETTINGS), ("threshold", "0")],
        )
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 422);
    let page = refused.text().await.unwrap();
    assert!(page.contains(r#"value="0" error="Must be at least 1""#), "{page}");
}

#[openshop::test(APP)]
async fn a_merchant_whose_admin_is_in_french_reads_the_pages_in_french(ctx: &TestContext) {
    // The language of the merchant's admin, which `openshop.js` sends with each request.
    let page = ctx
        .page(SettingsPage::PATH)
        .locale("fr-FR")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(page.contains(r#"<html lang="fr""#), "{page}");
    assert!(page.contains(r#"<s-page heading="Réglages">"#), "{page}");

    // The server keeps it for the requests that follow: the errors of a refused form too.
    let refused = ctx
        .submit(
            SettingsPage::PATH,
            &[("_action", SettingsPage::SAVE_SETTINGS), ("threshold", "0")],
        )
        .send()
        .await
        .unwrap();
    let page = refused.text().await.unwrap();
    assert!(
        page.contains(r#"value="0" error="Doit être supérieur ou égal à 1""#),
        "{page}"
    );
}

/// Every text of `locales/en.default.json` is in the other files of `locales/`.
#[test]
fn every_text_is_translated() {
    assert_translated(&APP);
}
