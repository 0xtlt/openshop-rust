use openshop::testing::{TestContext, sign_webhook_body};

use openshop_template_public::APP;
use openshop_template_public::models::stock_settings::{NewStockSettings, StockSettings};

#[openshop::test(APP)]
async fn a_shop_that_uninstalls_takes_its_settings_with_it(ctx: &TestContext) {
    let elsewhere = StockSettings::for_shop(&ctx.db, "another-shop.myshopify.com");
    for settings in [StockSettings::of(ctx), elsewhere] {
        settings.insert(NewStockSettings { threshold: 12 }).await.unwrap();
    }

    // Shopify signs what it sends with the secret of the app. The payload of
    // `app/uninstalled` is the shop, which OpenShop marks uninstalled.
    let body = openshop::json!({ "myshopify_domain": ctx.shop }).to_string();
    let delivered = ctx
        .http
        .post(ctx.url_for("/webhooks"))
        .header("content-type", "application/json")
        .header("x-shopify-hmac-sha256", sign_webhook_body(&body, &ctx.secret))
        .header("x-shopify-topic", "app/uninstalled")
        .header("x-shopify-shop-domain", &ctx.shop)
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(delivered.status(), 200);

    assert_eq!(StockSettings::of(ctx).count().await.unwrap(), 0);
    // Those of another shop stay.
    assert_eq!(StockSettings::all_shops(&ctx.db).count().await.unwrap(), 1);
}
