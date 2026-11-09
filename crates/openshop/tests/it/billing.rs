//! `openshop::billing`: the plans of a shop, from the Admin API or the
//! Partner API, the guard of an admin page that needs a plan, and the pricing
//! page of the app.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::common::{LogCapture, LoggedLine, TEST_SECRET, TEST_SHOP, json_body};
use openshop::billing::{
    self, ACTIVE_SUBSCRIPTIONS_OPERATION, BillingSource, PARTNER_IDS_OPERATION, PARTNER_SUBSCRIPTION_OPERATION,
    SubscriptionStatus,
};
use openshop::builder::{AdminFunction, AdminPage, InputSchema, Route};
use openshop::response::{self, IntoResponse, StatusCode, header};
use openshop::server::{ServerOptions, UiSource};
use openshop::testing::{
    MockTransport, TEST_APP_GID, TEST_SHOP_GID, TestContext, TestOptions, app_pricing_subscription, sign_query,
    subscription, to_query_string,
};
use openshop::{AdminContext, OpenShopConfig, PartnerApi, ShopScope, ShopifyEndpoints, Value, json};
use openshop_shared::build_admin_function_rpc_url;
use reqwest::{Method, Response};

/// The Partner ID and the token of the tests of the Partner API, in lsf's shape.
const PARTNER_ID: &str = "7438846";
const PARTNER_TOKEN: &str = "prtapi_lsf_d532acaed0aa7474ed8e231cde1d1c84";

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

/// `/routes/welcome`: the welcome link of a plan, as the billing reference
/// writes it. It reads the plan again for a launch Shopify signed only.
fn welcome_route() -> Route {
    Route::public().get(|ctx| async move {
        let Some(launch) = ctx.shopify_launch() else {
            // Not signed by Shopify: the app's page in the admin, and no Partner API call.
            let shop = ctx.request.query().get("shop").cloned().unwrap_or_default();
            let location = ctx.admin_url(&shop, &format!("apps/{}", ctx.app_handle(&shop)?))?;
            return Ok((StatusCode::FOUND, [(header::LOCATION, location)]).into_response());
        };
        if launch.is_plan_welcome() {
            let refreshed = async { ctx.for_shop(&launch).await?.shopify.refresh_plans().await };
            if let Err(error) = refreshed.await {
                ctx.logger
                    .warn(format!("The plan of {} was not read again: {error:#}", launch.shop));
            }
        }
        // Still in the frame of the admin: the app's home, with the signed query.
        let location = format!("/?{}", launch.query);
        Ok((StatusCode::FOUND, [(header::LOCATION, location)]).into_response())
    })
}

/// `/reports`: a loader for the Pro and Plus plans, an action for any plan.
/// `/routes/plans`: the links a route gives for the shop of `?shop=`.
fn config() -> OpenShopConfig {
    let page = AdminPage::new("/reports")
        .loader("report", |ctx: AdminContext, _input: Value| async move {
            let plan = billing::require_plan(&ctx, &["Pro", "Plus", "pro", "plus"]).await?;
            Ok(json!({ "plan": plan.name, "handle": plan.handle }))
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
        .route("/welcome", welcome_route())
        .build()
        .unwrap()
}

/// A project whose `shopify.app.toml` is the one of the test app, with
/// `handle` when there is one.
fn project(handle: Option<&str>) -> PathBuf {
    let cwd = std::env::temp_dir().join(format!("openshop-billing-{}", openshop::Uuid::new_v4()));
    std::fs::create_dir_all(&cwd).unwrap();
    let handle = handle
        .map(|handle| format!("handle = \"{handle}\"\n"))
        .unwrap_or_default();
    std::fs::write(
        cwd.join("shopify.app.toml"),
        format!("client_id = \"test-app\"\nname = \"Hide PayPal\"\n{handle}"),
    )
    .unwrap();
    cwd
}

fn options(transport: Arc<MockTransport>, endpoints: ShopifyEndpoints, cwd: Option<PathBuf>) -> TestOptions {
    TestOptions {
        shop: TEST_SHOP.to_string(),
        secret: TEST_SECRET.to_string(),
        access_token: Some("test-access-token".to_string()),
        transport: Some(transport),
        shopify_endpoints: endpoints,
        cwd,
        ..Default::default()
    }
}

async fn context(transport: Arc<MockTransport>, endpoints: ShopifyEndpoints, cwd: Option<PathBuf>) -> TestContext {
    TestContext::with_options(config(), options(transport, endpoints, cwd))
        .await
        .unwrap()
}

/// A context whose plans are read from the Partner API, at lsf's origin,
/// with the admin UI served for the launches of the app.
async fn partner_context(transport: Arc<MockTransport>, cwd: PathBuf) -> TestContext {
    let options = TestOptions {
        partner_api: PartnerApi::new().partner_id(PARTNER_ID).token(PARTNER_TOKEN),
        server: ServerOptions {
            ui: UiSource::Embedded,
            dev: false,
        },
        ..options(
            transport,
            ShopifyEndpoints::new().partner_origin("http://localhost:9393"),
            Some(cwd),
        )
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

/// The status and the body of a call.
async fn answer(ctx: &TestContext, kind: &str, name: &str) -> (u16, Value) {
    let response = call(ctx, kind, name).await;
    (response.status().as_u16(), json_body(response).await)
}

/// The launch of the app from the admin of the shop, signed, with `extra`
/// in its query: the welcome link of a plan has `plan_handle`.
async fn launch(ctx: &TestContext, extra: &[(&str, &str)]) -> Response {
    let mut params = vec![("host", "YWRtaW4uc2hvcGlmeS5jb20vc3RvcmUvdGVzdA"), ("shop", TEST_SHOP)];
    params.extend_from_slice(extra);
    let query = sign_query(&params, TEST_SECRET);
    ctx.http
        .get(ctx.url_for(&format!("/welcome?{}", to_query_string(&query))))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn a_page_that_needs_a_plan_answers_402_with_the_pricing_page() {
    let cwd = project(Some("my-app"));
    let transport = MockTransport::new();
    transport.respond_json(subscriptions(&[("Basic", "ACTIVE"), ("Pro", "CANCELLED")]));
    let ctx = context(transport.clone(), ShopifyEndpoints::new(), Some(cwd.clone())).await;
    assert_eq!(ctx.app.billing_source(), BillingSource::AdminApi);

    let response = call(&ctx, "loader", "report").await;
    assert_eq!(response.status(), 402);
    let body = json_body(response).await;
    assert_eq!(body["code"], "PLAN_REQUIRED");
    assert_eq!(body["error"], "This feature needs the Pro, Plus, pro or plus plan");
    assert_eq!(
        body["details"],
        json!({
            "pricingUrl": "https://admin.shopify.com/store/test-integration/charges/my-app/pricing_plans",
            "pricingLink": "shopify:admin/charges/my-app/pricing_plans",
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

    // The Admin API is asked at each check: nothing is kept.
    transport.respond_json(subscriptions(&[("Plus", "ACTIVE")]));
    let response = call(&ctx, "loader", "report").await;
    assert_eq!(response.status(), 200);
    assert_eq!(json_body(response).await, json!({ "plan": "Plus", "handle": "plus" }));
    ctx.shutdown().await.unwrap();
    std::fs::remove_dir_all(cwd).ok();
}

#[tokio::test]
async fn without_a_handle_the_pricing_page_is_an_error_that_says_to_add_it() {
    let cwd = project(None);
    let transport = MockTransport::new();
    transport.respond_json(subscriptions(&[]));
    let ctx = context(transport.clone(), ShopifyEndpoints::new(), Some(cwd.clone())).await;

    // The client ID does not take the place of the handle.
    let error = billing::pricing_url(&ctx).unwrap_err().to_string();
    assert!(
        error.starts_with(
            "[openshop] The Shopify app \"default\" has no handle: add `handle = \"<your-app-handle>\"` at the top \
             of the shopify.app*.toml whose client_id is \"test-app\""
        ),
        "{error}"
    );
    assert!(billing::pricing_link(&ctx).is_err() && ctx.app_handle().is_err());
    assert!(billing::plan_required(&ctx, &[]).is_err());
    // A page without the plan is an error of the server, not a 402 without its pricing page.
    let (status, body) = answer(&ctx, "loader", "report").await;
    assert_eq!((status, body["code"].as_str()), (500, Some("INTERNAL_ERROR")));
    // A route too.
    let links = ctx
        .http
        .get(format!("{}/routes/plans?shop={TEST_SHOP}", ctx.url))
        .send()
        .await
        .unwrap();
    assert_eq!(links.status(), 500);
    // The other pages of the admin need no handle.
    assert_eq!(
        ctx.admin_url("settings/payments"),
        "https://admin.shopify.com/store/test-integration/settings/payments"
    );
    ctx.shutdown().await.unwrap();
    std::fs::remove_dir_all(cwd).ok();
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
    let cwd = project(Some("hide-paypal"));
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
    assert_eq!(ctx.app_handle().unwrap(), "hide-paypal");
    assert_eq!(
        billing::pricing_url(&ctx).unwrap(),
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
    let client = ctx.app.shopify_client(TEST_SHOP, "default");
    let subscriptions = client.active_subscriptions().await.unwrap();
    assert!(subscriptions.has_plan(&["pro"]));
    assert_eq!(subscriptions[0].status, SubscriptionStatus::Active);
    let plans = client.active_plans().await.unwrap();
    assert_eq!(
        (plans[0].name.as_str(), plans[0].source()),
        ("Pro", BillingSource::AdminApi)
    );
    ctx.shutdown().await.unwrap();
    std::fs::remove_dir_all(cwd).ok();
}

#[tokio::test]
async fn with_the_partner_api_the_plan_is_read_there_and_kept() {
    let cwd = project(Some("hide-paypal"));
    let transport = MockTransport::new();
    let ctx = partner_context(transport.clone(), cwd.clone()).await;
    assert_eq!(ctx.app.billing_source(), BillingSource::PartnerApi);
    let partner = ctx.app.partner_api().unwrap();
    assert_eq!(partner.url(), "http://localhost:9393/7438846/api/2026-10/graphql.json");
    assert!(!format!("{partner:?}").contains(PARTNER_TOKEN));

    ctx.shopify.active_subscription(app_pricing_subscription("pro"));
    assert_eq!(
        answer(&ctx, "loader", "report").await,
        (200, json!({ "plan": "pro", "handle": "pro" }))
    );
    // The GIDs from the Admin API of the shop, then the plan from the Partner
    // API, at its origin and with its token.
    assert_eq!(
        ctx.shopify.operations(),
        [PARTNER_IDS_OPERATION, PARTNER_SUBSCRIPTION_OPERATION]
    );
    let partner_call = &ctx.shopify.calls_to(PARTNER_SUBSCRIPTION_OPERATION)[0];
    assert_eq!(
        partner_call.url,
        "http://localhost:9393/7438846/api/2026-10/graphql.json"
    );
    assert_eq!(
        partner_call.variables,
        json!({ "appId": TEST_APP_GID, "shopId": TEST_SHOP_GID })
    );
    let request = transport.last_request().unwrap();
    assert_eq!(request.header("X-Shopify-Access-Token"), Some(PARTNER_TOKEN));
    // Nothing went to `currentAppInstallation.activeSubscriptions`.
    assert!(transport.requests().iter().all(|request| {
        !request.body["query"]
            .as_str()
            .unwrap_or_default()
            .contains("activeSubscriptions")
    }));

    // The answer is kept: the plan ends on Shopify's side, and the shop
    // keeps it until the answer is five minutes old.
    ctx.shopify.active_subscription(None);
    assert_eq!(answer(&ctx, "action", "export").await.0, 200);
    assert_eq!(answer(&ctx, "loader", "report").await.0, 200);
    assert_eq!(ctx.shopify.calls_to(PARTNER_SUBSCRIPTION_OPERATION).len(), 1);

    // The welcome link of a plan, a signed launch with `plan_handle`, makes
    // the next check ask again.
    let shell = launch(&ctx, &[("plan_handle", "free")]).await;
    assert_eq!(shell.status(), 200);
    let (status, body) = answer(&ctx, "loader", "report").await;
    assert_eq!((status, body["code"].as_str()), (402, Some("PLAN_REQUIRED")));
    assert_eq!(
        body["details"]["pricingUrl"],
        "https://admin.shopify.com/store/test-integration/charges/hide-paypal/pricing_plans"
    );
    assert_eq!(ctx.shopify.calls_to(PARTNER_SUBSCRIPTION_OPERATION).len(), 2);
    // A launch without it does not; nor does a check that fails within ten
    // seconds of the answer.
    ctx.shopify.active_subscription(app_pricing_subscription("plus"));
    assert_eq!(launch(&ctx, &[]).await.status(), 200);
    assert_eq!(answer(&ctx, "loader", "report").await.0, 402);
    assert_eq!(ctx.shopify.calls_to(PARTNER_SUBSCRIPTION_OPERATION).len(), 2);
    // The GIDs were asked once.
    assert_eq!(ctx.shopify.calls_to(PARTNER_IDS_OPERATION).len(), 1);

    // An uninstall forgets the plan too.
    let uninstalled = ctx
        .webhook("app/uninstalled", json!({ "myshopify_domain": TEST_SHOP }))
        .send()
        .await
        .unwrap();
    assert_eq!(uninstalled.status(), 200);
    let plans = ctx
        .app
        .shopify_client(TEST_SHOP, "default")
        .active_plans()
        .await
        .unwrap();
    assert_eq!(
        plans.plan(&["plus"]).map(|plan| plan.source()),
        Some(BillingSource::PartnerApi)
    );
    assert_eq!(ctx.shopify.calls_to(PARTNER_SUBSCRIPTION_OPERATION).len(), 3);
    ctx.shutdown().await.unwrap();
    std::fs::remove_dir_all(cwd).ok();
}

#[tokio::test]
async fn a_shop_the_partner_api_does_not_know_is_an_error_and_a_429_is_sent_again() {
    let cwd = project(Some("hide-paypal"));
    let transport = MockTransport::new();
    let ctx = partner_context(transport.clone(), cwd.clone()).await;
    ctx.shopify
        .on_with(PARTNER_SUBSCRIPTION_OPERATION, |_| anyhow::bail!("Shop not found"))
        .on(
            PARTNER_IDS_OPERATION,
            json!({ "shop": { "id": "gid://shopify/Shop/9" }, "currentAppInstallation": { "app": { "id": TEST_APP_GID } } }),
        );
    // Not a missing plan: an error of the server.
    let (status, body) = answer(&ctx, "loader", "report").await;
    assert_eq!((status, body["code"].as_str()), (500, Some("INTERNAL_ERROR")));
    let error = ctx
        .app
        .shopify_client(TEST_SHOP, "default")
        .active_plans()
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(error, "[openshop] The Partner API answered with errors: Shop not found");
    // The GIDs of the test's own answer.
    assert_eq!(
        ctx.shopify.calls_to(PARTNER_SUBSCRIPTION_OPERATION)[0].variables["shopId"],
        "gid://shopify/Shop/9"
    );

    // Four requests a second: a `429` is sent again, as `shopify_retry` says.
    ctx.shopify.reset();
    ctx.shopify.on(
        PARTNER_IDS_OPERATION,
        json!({ "shop": { "id": TEST_SHOP_GID }, "currentAppInstallation": { "app": { "id": TEST_APP_GID } } }),
    );
    let sent = Arc::new(AtomicUsize::new(0));
    let counter = sent.clone();
    transport.respond_with(move |_| {
        Ok(match counter.fetch_add(1, Ordering::SeqCst) {
            0 => MockTransport::json(429, json!({ "errors": [{ "message": "Too many requests" }] })),
            _ => MockTransport::json(200, json!({ "data": { "activeSubscription": null } })),
        })
    });
    let plans = ctx
        .app
        .shopify_client(TEST_SHOP, "default")
        .refresh_plans()
        .await
        .unwrap();
    assert!(plans.is_empty());
    assert_eq!(sent.load(Ordering::SeqCst), 2);
    ctx.shutdown().await.unwrap();
    std::fs::remove_dir_all(cwd).ok();
}

/// The warnings `active_subscriptions()` logged among `logs`.
fn admin_api_warnings(logs: &LogCapture) -> Vec<LoggedLine> {
    logs.at("warn")
        .into_iter()
        .filter(|line| line.message.contains("active_subscriptions() at "))
        .collect()
}

#[tokio::test]
async fn with_the_partner_api_active_subscriptions_warns_once_where_it_is_called() {
    let cwd = project(Some("hide-paypal"));
    let transport = MockTransport::new();
    let ctx = partner_context(transport, cwd.clone()).await;
    ctx.shopify
        .active_subscription(app_pricing_subscription("pro"))
        .active_subscriptions([subscription("Pro")]);
    let logs = LogCapture::start().await;

    // The plan checks read the Partner API, and do not warn.
    assert_eq!(answer(&ctx, "loader", "report").await.0, 200);
    let client = ctx.app.shopify_client(TEST_SHOP, "default");
    assert!(client.active_plans().await.unwrap().has_plan(&["pro"]));
    assert!(admin_api_warnings(&logs).is_empty(), "{}", logs.text());

    // `active_subscriptions()` still reads the Admin API, and says where it was called.
    let at = format!("{}:{}", file!(), line!() + 1);
    let subscriptions = client.active_subscriptions().await.unwrap();
    assert!(subscriptions.has_plan(&["Pro"]));
    assert_eq!(ctx.shopify.calls_to(ACTIVE_SUBSCRIPTIONS_OPERATION).len(), 1);
    // Once per process: not for a second call, nor for the client of another context.
    client.active_subscriptions().await.unwrap();
    ctx.app
        .shopify_client(TEST_SHOP, "default")
        .active_subscriptions()
        .await
        .unwrap();
    assert_eq!(ctx.shopify.calls_to(ACTIVE_SUBSCRIPTIONS_OPERATION).len(), 3);

    let warnings = admin_api_warnings(&logs);
    drop(logs);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(
        warnings[0].message,
        format!(
            "[openshop] active_subscriptions() at {at} reads the Admin API, but billing reads the plans from the \
             Partner API. A subscription of Shopify App Pricing may not be in the Admin API, so a paying merchant \
             can look like one without a plan. Call active_plans() instead, or keep active_subscriptions() only \
             to read the Admin API on purpose. This warning is logged once per process."
        )
    );
    assert_eq!(
        warnings[0].context,
        Some(json!({ "shop": TEST_SHOP, "shopifyApp": "default" }))
    );
    ctx.shutdown().await.unwrap();
    std::fs::remove_dir_all(cwd).ok();
}

#[tokio::test]
async fn without_the_partner_api_active_subscriptions_does_not_warn() {
    let transport = MockTransport::new();
    transport.respond_json(subscriptions(&[("Pro", "ACTIVE")]));
    let ctx = context(transport, ShopifyEndpoints::new(), None).await;
    let logs = LogCapture::start().await;

    let client = ctx.app.shopify_client(TEST_SHOP, "default");
    assert!(client.active_subscriptions().await.unwrap().has_plan(&["Pro"]));
    assert!(client.active_plans().await.unwrap().has_plan(&["pro"]));
    let warnings = admin_api_warnings(&logs);
    drop(logs);
    assert!(warnings.is_empty(), "{warnings:?}");
    ctx.shutdown().await.unwrap();
}

/// A `GET` of the welcome route with `query`: its status and where it sends.
async fn welcome(ctx: &TestContext, query: &str) -> (u16, String) {
    let response = ctx
        .http
        .get(format!("{}/routes/welcome?{query}", ctx.url))
        .send()
        .await
        .unwrap();
    let location = response
        .headers()
        .get("location")
        .map(|location| location.to_str().unwrap().to_string())
        .unwrap_or_default();
    (response.status().as_u16(), location)
}

#[tokio::test]
async fn a_welcome_route_reads_the_plan_again_only_for_a_launch_shopify_signed() {
    let cwd = project(Some("hide-paypal"));
    let ctx = partner_context(MockTransport::new(), cwd.clone()).await;
    let partner_calls = || ctx.shopify.calls_to(PARTNER_SUBSCRIPTION_OPERATION).len();
    // The check that sent the merchant to the plans: no plan, kept.
    ctx.shopify.active_subscription(None);
    assert_eq!(answer(&ctx, "loader", "report").await.0, 402);
    assert_eq!(partner_calls(), 1);
    ctx.shopify.active_subscription(app_pricing_subscription("pro"));

    // Anyone can call the route: without Shopify's signature, it sends to the
    // app in the admin and asks the Partner API nothing.
    let admin = "https://admin.shopify.com/store/test-integration/apps/hide-paypal".to_string();
    let host = "YWRtaW4uc2hvcGlmeS5jb20vc3RvcmUvdGVzdA";
    let forged = to_query_string(&sign_query(
        &[("host", host), ("plan_handle", "pro"), ("shop", TEST_SHOP)],
        "not-the-secret",
    ));
    for query in [format!("shop={TEST_SHOP}&plan_handle=pro"), forged] {
        assert_eq!(welcome(&ctx, &query).await, (302, admin.clone()), "{query}");
    }
    assert_eq!(partner_calls(), 1);
    // Signed, but not the welcome link of a plan: no Partner API call either.
    let home = to_query_string(&sign_query(&[("host", host), ("shop", TEST_SHOP)], TEST_SECRET));
    assert_eq!(welcome(&ctx, &home).await, (302, format!("/?{home}")));
    assert_eq!(partner_calls(), 1);

    // The welcome link Shopify signed: one Partner API call, and the app's
    // home in the frame, with the signed query.
    let signed = to_query_string(&sign_query(
        &[("host", host), ("plan_handle", "pro"), ("shop", TEST_SHOP)],
        TEST_SECRET,
    ));
    assert_eq!(welcome(&ctx, &signed).await, (302, format!("/?{signed}")));
    assert_eq!(partner_calls(), 2);
    // The plan read there is kept: the next check passes without asking.
    assert_eq!(
        answer(&ctx, "loader", "report").await,
        (200, json!({ "plan": "pro", "handle": "pro" }))
    );
    assert_eq!(partner_calls(), 2);

    // A Partner API that fails does not stop the merchant: the failure is
    // logged, with the route and the shop.
    ctx.shopify
        .on_with(PARTNER_SUBSCRIPTION_OPERATION, |_| anyhow::bail!("Throttled"));
    let logs = LogCapture::start().await;
    assert_eq!(welcome(&ctx, &signed).await, (302, format!("/?{signed}")));
    let warning = logs
        .at("warn")
        .into_iter()
        .find(|line| line.message.starts_with("The plan of "))
        .expect("the warning of the route");
    drop(logs);
    assert_eq!(
        warning.message,
        format!(
            "The plan of {TEST_SHOP} was not read again: [openshop] The Partner API answered with errors: Throttled"
        )
    );
    assert_eq!(warning.context, Some(json!({ "route": "GET /welcome" })));
    ctx.shutdown().await.unwrap();
    std::fs::remove_dir_all(cwd).ok();
}
