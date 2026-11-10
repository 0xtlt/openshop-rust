use openshop::testing::TestContext;

use openshop_template_public::APP;
use openshop_template_public::models::stock_settings::{NewStockSettings, StockSettings};

#[openshop::test(APP)]
async fn a_shop_that_uninstalls_takes_its_settings_with_it(ctx: &TestContext) {
    let elsewhere = StockSettings::for_shop(&ctx.db, "another-shop.myshopify.com");
    for settings in [StockSettings::of(ctx), elsewhere] {
        settings.insert(NewStockSettings { threshold: 12 }).await.unwrap();
    }

    // Delivered as Shopify delivers it, signed with the secret of the app. The
    // payload of `app/uninstalled` is the shop, which OpenShop marks uninstalled.
    let payload = openshop::json!({ "myshopify_domain": ctx.shop });
    let delivered = ctx.webhook("app/uninstalled", payload).send().await.unwrap();
    assert_eq!(delivered.status(), 200);

    assert_eq!(StockSettings::of(ctx).count().await.unwrap(), 0);
    // Those of another shop stay.
    assert_eq!(StockSettings::all_shops(&ctx.db).count().await.unwrap(), 1);
}
