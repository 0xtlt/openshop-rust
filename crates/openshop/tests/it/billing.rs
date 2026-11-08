//! `openshop::billing`: the active subscriptions of a shop, the guard of an
//! admin page that needs a plan, and the pricing page of the app.

use std::path::PathBuf;

use crate::common::{TEST_SHOP, json_body};
use openshop::billing::{self, SubscriptionStatus};
use openshop::builder::{AdminFunction, AdminPage, InputSchema, Route};
use openshop::response;
use openshop::testing::{MockTransport, TestContext, TestOptions};
use openshop::{AdminContext, OpenShopConfig, ShopScope, ShopifyEndpoints, Value, json};
use openshop_shared::build_admin_function_rpc_url;
use reqwest::{Method, Response};

/// What Shopify answers to `activeSubscriptions` for a shop with `plans`,
/// each `(name, status)`.
fn subscriptions(plans: &[(&str, &str)]) -> Value {
    let subscriptions: Vec<Value> = plans
        .iter()
        .map(|(name, status)| {
            json!({
                "id": format!("gid://shopify/AppSubscription/{name}"),
                "name": name,
                "status": status,
                "test": false,
                "trialDays": 0,
                "createdAt": "2026-11-01T10:00:00Z",
                "currentPeriodEnd": "2026-12-01T10:00:00Z",
                "lineItems": [{
                    "id": "gid://shopify/AppSubscriptionLineItem/1",
                    "plan": { "pricingDetails": {
                        "__typename": "AppRecurringPricing",
                        "interval": "EVERY_30_DAYS",
                        "planHandle": name.to_lowercase(),
                        "price": { "amount": "9.0", "currencyCode": "USD" },
                        "discount": null
                    } }
                }]
            })
        })
        .collect();
    json!({ "data": { "currentAppInstallation": { "activeSubscriptions": subscriptions } } })
}

/// `/reports`: a loader for the Pro and Plus plans, an action for any plan.
/// `/routes/plans`: the links a route gives for the shop of `?shop=`.
fn config() -> OpenShopConfig {
    let page = AdminPage::new("/reports")
        .loader("report", |ctx: AdminContext, _input: Value| async move {
            let subscription = billing::require_plan(&ctx, &["Pro", "Plus"]).await?;
            Ok(json!({ "plan": subscription.name, "handle": subscription.plan_handle() }))
        })
        .function(
            "export",
            AdminFunction::action(InputSchema::any(), |ctx: AdminContext, _input: Value| async move {
                Ok(json!({ "shop": ctx.shop }))
            })
            .authorize(billing::any_plan),
        );
    let plans = Route::public().get(|ctx| async move {
        let shop = ctx.request.query().get("shop").cloned().unwrap_or_default();
        Ok(response::json(json!({
            "appHandle": ctx.app_handle(&shop)?,
            "pricingUrl": ctx.pricing_url(&shop)?,
            "welcome": ctx.admin_url(&shop, &format!("apps/{}", ctx.app_handle(&shop)?))?,
        })))
    });
    OpenShopConfig::builder()
        .admin_page(page)
        .route("/plans", plans)
        .build()
        .unwrap()
}

async fn context(
    transport: std::sync::Arc<MockTransport>,
    endpoints: ShopifyEndpoints,
    cwd: Option<PathBuf>,
) -> TestContext {
    let options = TestOptions {
        shop: TEST_SHOP.to_string(),
        access_token: Some("test-access-token".to_string()),
        transport: Some(transport),
        shopify_endpoints: endpoints,
        cwd,
        ..Default::default()
    };
    TestContext::with_options(config(), options).await.unwrap()
}

async fn call(ctx: &TestContext, kind: &str, name: &str) -> Response {
    let url = build_admin_function_rpc_url(kind, name, "/reports");
    ctx.request_as(Method::POST, &url, &ctx.shop, "42")
        .json(&json!({ "input": null }))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn a_page_that_needs_a_plan_answers_402_with_the_pricing_page() {
    let transport = MockTransport::new();
    transport.respond_json(subscriptions(&[("Basic", "ACTIVE"), ("Pro", "CANCELLED")]));
    let ctx = context(transport.clone(), ShopifyEndpoints::new(), None).await;

    let response = call(&ctx, "loader", "report").await;
    assert_eq!(response.status(), 402);
    let body = json_body(response).await;
    assert_eq!(body["code"], "PLAN_REQUIRED");
    assert_eq!(body["error"], "This feature needs the Pro or Plus plan");
    // Without a `shopify.app.toml`, the app is named by its API key.
    assert_eq!(
        body["details"],
        json!({
            "pricingUrl": "https://admin.shopify.com/store/test-integration/charges/test-app/pricing_plans",
            "pricingLink": "shopify:admin/charges/test-app/pricing_plans",
        })
    );
    // One request to the Admin API of the shop, with its token.
    let request = transport.last_request().unwrap();
    assert!(
        request
            .url
            .starts_with("https://test-integration.myshopify.com/admin/api/"),
        "{}",
        request.url
    );
    assert!(request.body["query"].as_str().unwrap().contains("activeSubscriptions"));

    // Any plan is enough for the action.
    let response = call(&ctx, "action", "export").await;
    assert_eq!(response.status(), 200);
    assert_eq!(json_body(response).await, json!({ "shop": TEST_SHOP }));

    transport.respond_json(subscriptions(&[]));
    let response = call(&ctx, "action", "export").await;
    assert_eq!(response.status(), 402);
    let body = json_body(response).await;
    assert_eq!(body["error"], "Choose a plan to use this feature");

    transport.respond_json(subscriptions(&[("Plus", "ACTIVE")]));
    let response = call(&ctx, "loader", "report").await;
    assert_eq!(response.status(), 200);
    assert_eq!(json_body(response).await, json!({ "plan": "Plus", "handle": "plus" }));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_failure_of_shopify_is_not_a_missing_plan() {
    let transport = MockTransport::new();
    transport.respond_with(|_| {
        Ok(MockTransport::json(
            200,
            json!({ "errors": [{ "message": "Throttled" }] }),
        ))
    });
    let ctx = context(transport, ShopifyEndpoints::new(), None).await;

    let response = call(&ctx, "loader", "report").await;
    assert_eq!(response.status(), 500);
    assert_eq!(json_body(response).await["code"], "INTERNAL_ERROR");
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_pricing_page_is_at_the_admin_origin_and_names_the_app_by_its_handle() {
    let cwd = std::env::temp_dir().join(format!("openshop-billing-{}", openshop::Uuid::new_v4()));
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(
        cwd.join("shopify.app.toml"),
        "client_id = \"test-app\"\nname = \"Hide PayPal\"\nhandle = \"hide-paypal\"\n",
    )
    .unwrap();
    let transport = MockTransport::new();
    transport.respond_json(subscriptions(&[]));
    let endpoints = ShopifyEndpoints::new().admin_origin("http://localhost:9393");
    let ctx = context(transport.clone(), endpoints, Some(cwd.clone())).await;

    let response = call(&ctx, "loader", "report").await;
    assert_eq!(response.status(), 402);
    assert_eq!(
        json_body(response).await["details"],
        json!({
            "pricingUrl": "http://localhost:9393/admin/charges/hide-paypal/pricing_plans?shop=test-integration.myshopify.com",
            "pricingLink": "shopify:admin/charges/hide-paypal/pricing_plans",
        })
    );
    // The Admin API is at the admin origin too.
    assert!(
        transport
            .last_request()
            .unwrap()
            .url
            .starts_with("http://localhost:9393/admin/api/")
    );

    // What a context gives for its shop, `shopify_app` unchanged.
    assert_eq!(ctx.shopify_app(), "default");
    assert_eq!(ctx.app_handle(), "hide-paypal");
    assert_eq!(
        billing::pricing_url(&ctx),
        "http://localhost:9393/admin/charges/hide-paypal/pricing_plans?shop=test-integration.myshopify.com"
    );
    assert_eq!(
        ctx.admin_url("settings/payments"),
        "http://localhost:9393/admin/settings/payments?shop=test-integration.myshopify.com"
    );

    // A route gives the same for a shop it names, installed or not.
    let links = ctx
        .http
        .get(format!("{}/routes/plans?shop=Not-Installed.myshopify.com", ctx.url))
        .send()
        .await
        .unwrap();
    assert_eq!(
        json_body(links).await,
        json!({
            "appHandle": "hide-paypal",
            "pricingUrl": "http://localhost:9393/admin/charges/hide-paypal/pricing_plans?shop=not-installed.myshopify.com",
            "welcome": "http://localhost:9393/admin/apps/hide-paypal?shop=not-installed.myshopify.com",
        })
    );

    // Read from a flow, a webhook or a route the same way.
    transport.respond_json(subscriptions(&[("Pro", "ACTIVE")]));
    let subscriptions = ctx
        .app
        .shopify_client(TEST_SHOP, "default")
        .active_subscriptions()
        .await
        .unwrap();
    assert!(subscriptions.has_plan(&["pro"]));
    assert_eq!(subscriptions[0].status, SubscriptionStatus::Active);
    ctx.shutdown().await.unwrap();
    std::fs::remove_dir_all(cwd).ok();
}
