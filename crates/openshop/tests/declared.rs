//! Building blocks declared with an attribute, and the application `app!`
//! makes of them: nothing is built by a function, each block is the function
//! or the struct itself.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use openshop::builder::{DiscountMode, Flow, FunctionTitle, FunctionType, Proxy, ProxyResponseType, Route};
use openshop::response::{Response, StatusCode};
use openshop::testing::{AdminTestOptions, TestContext, admin_context, invoke_admin_function};
use openshop::{
    AdminContext, AdminMenuItem, App, CancellationToken, Concurrency, Cron, ErrorContext, FlowContext, FunctionConfig,
    JsonMap, McpContext, McpPermission, McpResourceContent, McpRiskLevel, Model, OpenShopConfig, ProviderConfig,
    ProxyContext, Result, RetryPolicy, RouteAuthContext, RouteAuthOutcome, RouteContext, RunOutcome, Uuid, Value,
    WebhookContext, WorkerConfig, admin_page, app, flow, json, mcp_resource, mcp_tool, provider, proxy, response,
    route, validators, webhook,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What the blocks of this file did, by the test that made them do it.
static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn saw(event: impl Into<String>) {
    SEEN.lock().unwrap().push(event.into());
}

fn seen(prefix: &str) -> Vec<String> {
    SEEN.lock()
        .unwrap()
        .iter()
        .filter(|event| event.starts_with(prefix))
        .cloned()
        .collect()
}

// ─── The blocks ──────────────────────────────────────────────────────

#[derive(Debug, ProviderConfig)]
struct Warehouse {
    #[field(label = "API URL")]
    api_url: String,
}

#[provider("warehouse")]
impl Warehouse {
    pub async fn ping(&self) -> Result<String> {
        Ok(format!("pong from {}", self.api_url))
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Model)]
#[model("widgets")]
struct Widget {
    id: Uuid,
    shop: String,
    name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GreetInput {
    #[schemars(length(min = 1))]
    name: String,
}

#[flow(
    "greet",
    timeout = Duration::from_secs(30),
    step_timeout = Duration::from_secs(5),
    concurrency = Concurrency::Allow,
    retry_policy = RetryPolicy::new().max_attempts(2)
)]
async fn greet(ctx: FlowContext, input: GreetInput) -> Result<()> {
    let greeting: String = ctx
        .step
        .run("greet", || async { Ok(format!("hello {}", input.name)) })
        .await?;
    saw(format!("greet:{greeting}"));
    Ok(())
}

/// Named after the function, without an input.
#[flow]
async fn nightly_cleanup(ctx: FlowContext) -> Result<()> {
    saw(format!("cleanup:{}", ctx.shop));
    Ok(())
}

/// Fails with an error of its own type.
#[flow("parse")]
async fn parse(ctx: FlowContext) -> std::result::Result<(), std::num::ParseIntError> {
    let text = ctx.input.as_str().unwrap_or_default().to_string();
    text.parse::<i64>().map(|_| ())
}

#[derive(Debug, Deserialize)]
struct ChangedOrder {
    name: String,
}

#[webhook("orders/create", "orders/updated")]
async fn order_changed(ctx: WebhookContext, order: ChangedOrder) -> Result<()> {
    saw(format!("webhook:{}:{}", ctx.topic, order.name));
    Ok(())
}

#[webhook("app/uninstalled")]
async fn uninstalled(ctx: WebhookContext) -> Result<()> {
    saw(format!("webhook:{}:{}", ctx.topic, ctx.shop));
    Ok(())
}

#[derive(Debug, Serialize)]
struct Listing {
    shop: String,
    page: u32,
}

#[proxy(GET, "/widgets")]
async fn list_widgets(ctx: ProxyContext) -> Result<Listing> {
    Ok(Listing {
        shop: ctx.shop,
        page: ctx.query.get("page").and_then(|page| page.parse().ok()).unwrap_or(1),
    })
}

#[proxy(POST, "/widgets", json)]
async fn create_widget(ctx: ProxyContext) -> Result<Value> {
    Ok(json!({ "created": ctx.body }))
}

#[proxy(GET, "/banner", liquid)]
async fn banner(ctx: ProxyContext) -> Result<String> {
    Ok(format!("<p>{{{{ shop.name }}}} via {}</p>", ctx.shop))
}

#[proxy(GET, "/page", html)]
async fn page(_ctx: ProxyContext) -> Result<&'static str> {
    Ok("<h1>Page</h1>")
}

/// The same path and method as [`list_widgets`].
#[proxy(GET, "/widgets")]
async fn list_widgets_again(_ctx: ProxyContext) -> Result<Value> {
    Ok(Value::Null)
}

/// Another response type on the path of [`banner`].
#[proxy(POST, "/banner")]
async fn banner_as_json(_ctx: ProxyContext) -> Result<Value> {
    Ok(Value::Null)
}

async fn bearer(ctx: RouteAuthContext) -> Result<RouteAuthOutcome> {
    Ok(match ctx.request.header("authorization") {
        Some("Bearer let-me-in") => RouteAuthOutcome::Authenticated(json!({ "caller": "partner" })),
        _ => RouteAuthOutcome::Unauthorized,
    })
}

#[route(GET, "/status")]
async fn status(_ctx: RouteContext) -> Result<Response> {
    Ok(response::json(json!({ "status": "up" })))
}

/// The same path as [`status`], behind an authentication of its own.
#[route(POST, "/status", auth = bearer)]
async fn set_status(ctx: RouteContext) -> Result<(StatusCode, String)> {
    Ok((
        StatusCode::CREATED,
        format!(
            "{} set {}",
            ctx.auth["caller"].as_str().unwrap_or_default(),
            ctx.request.text()
        ),
    ))
}

#[route(GET, "/status")]
async fn status_again(_ctx: RouteContext) -> Result<Response> {
    Ok(response::text("again"))
}

/// Declared once: the tool and the resource that list it register it.
const READ_WIDGETS: McpPermission = McpPermission::new("widgets:read", "Read widgets").risk(McpRiskLevel::Low);

#[derive(Debug, Deserialize, JsonSchema)]
struct CountInput {
    color: String,
}

/// Counts the widgets of a color.
///
/// Costs nothing.
#[mcp_tool("widgets.count", permissions(READ_WIDGETS), risk = medium, confirmation_hint = "Counting is safe")]
async fn count_widgets(ctx: McpContext, input: CountInput) -> Result<Value> {
    Ok(json!({ "shop": ctx.shop, "color": input.color, "count": 3 }))
}

#[mcp_tool("widgets.ping", description = "Says pong.")]
async fn ping_widgets(_ctx: McpContext) -> Result<&'static str> {
    Ok("pong")
}

/// How widgets work.
#[mcp_resource("app://widgets/guide", name = "Widget guide", mime_type = "text/markdown", permissions(READ_WIDGETS), risk = low)]
async fn widget_guide(ctx: McpContext) -> Result<String> {
    Ok(format!("# Widgets of {}", ctx.shop))
}

#[mcp_resource("app://widgets/raw", name = "Raw", description = "Stated in the attribute.")]
async fn raw_widgets(_ctx: McpContext) -> Result<McpResourceContent> {
    Ok(McpResourceContent {
        text: "raw".to_string(),
        mime_type: Some("text/plain".to_string()),
    })
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RenameInput {
    #[schemars(length(min = 1))]
    name: String,
}

struct WidgetsPage;

#[admin_page("/widgets")]
impl WidgetsPage {
    #[access]
    async fn staff(ctx: AdminContext) -> Result<bool> {
        Ok(ctx.actor.id != "stranger")
    }

    async fn manager(ctx: AdminContext) -> Result<bool> {
        Ok(ctx.actor.id == "manager")
    }

    #[loader]
    async fn list_widgets(ctx: AdminContext) -> Result<Value> {
        Ok(json!({ "widgets": [Self::label(&ctx.shop)] }))
    }

    #[loader("find")]
    async fn find_widget(_ctx: AdminContext, input: RenameInput) -> Result<Value> {
        Ok(json!({ "found": input.name }))
    }

    #[action(authorize = Self::manager)]
    async fn rename_widget(ctx: AdminContext, input: RenameInput) -> Result<Value> {
        Ok(json!({ "renamed": input.name, "by": ctx.actor.id }))
    }

    #[action]
    async fn refresh(_ctx: AdminContext) -> Result<bool> {
        Ok(true)
    }

    /// Not marked: a helper of the page.
    fn label(shop: &str) -> String {
        format!("widget of {shop}")
    }
}

#[derive(Debug, FunctionConfig)]
#[function(
    "volumeDiscount",
    discount = "volume-discount",
    label = "Volume discount",
    modes(automatic, code),
    title = Self::title,
    starts_at,
    ends_at = false,
    usage_limit,
    applies_on_each_item,
    combines_with(product_discounts, order_discounts = false)
)]
struct VolumeDiscount {
    #[field(label = "Minimum quantity", validate = validators::integer_min(1))]
    min_quantity: u32,
    #[field(label = "Percentage")]
    percentage: f64,
    #[field(label = "Excluded tags", placeholder = "gift, sample")]
    excluded_tags: Option<String>,
}

impl VolumeDiscount {
    fn title(&self) -> String {
        format!("{}% off {}+ items", self.percentage, self.min_quantity)
    }
}

/// Keyed after the struct, with a title that does not depend on the instance.
#[derive(Debug, FunctionConfig)]
#[function(delivery_customization = "hide-express", title = "Hide express", enabled)]
struct HideExpress;

/// Without a Shopify owner.
#[derive(Debug, FunctionConfig)]
#[function("cartRules", cart_transform = "cart-rules")]
struct CartRules {
    #[field(label = "Bundle tag")]
    bundle_tag: String,
}

async fn report(error: Arc<openshop::Error>, context: ErrorContext) {
    saw(format!("failed:{}:{error}", context.flow.unwrap_or_default()));
}

static APP: App = app! {
    providers: [Warehouse],
    flows: [greet, nightly_cleanup, parse],
    functions: [VolumeDiscount, HideExpress, CartRules],
    models: [Widget],
    admin_pages: [WidgetsPage],
    admin_menu: [AdminMenuItem::new("Widgets", WidgetsPage::PATH)],
    webhooks: [order_changed, uninstalled],
    proxies: [list_widgets, create_widget, banner, page],
    routes: [status, set_status],
    mcp_tools: [count_widgets, ping_widgets],
    mcp_resources: [widget_guide, raw_widgets],
    crons: [Cron::new("0 2 * * *", nightly_cleanup::NAME).all_shops()],
    worker: WorkerConfig::new().concurrency(2),
    on_error: report,
};

/// A setting read from the environment, which can be missing.
fn concurrency(variable: &str) -> Result<WorkerConfig> {
    let value = std::env::var(variable).map_err(|_| openshop::anyhow!("{variable} is not set"))?;
    Ok(WorkerConfig::new().concurrency(value.parse()?))
}

static UNSET: App = app! {
    flows: [greet],
    worker: concurrency("OPENSHOP_TEST_UNSET_CONCURRENCY")?,
};

fn refusal(config: Result<OpenShopConfig>) -> String {
    match config {
        Ok(_) => panic!("accepted"),
        Err(error) => error.to_string(),
    }
}

// ─── The application ─────────────────────────────────────────────────

#[test]
fn an_application_lists_what_it_is_made_of() {
    let config = APP.config().unwrap();

    assert_eq!(config.providers.keys().collect::<Vec<_>>(), ["warehouse"]);
    assert_eq!(
        config.flows.keys().collect::<Vec<_>>(),
        ["greet", "nightlyCleanup", "parse"]
    );
    assert_eq!(
        config.functions.keys().collect::<Vec<_>>(),
        ["volumeDiscount", "hideExpress", "cartRules"]
    );
    assert_eq!(config.models.len(), 1);
    assert_eq!(config.models[0].table, "widgets");
    assert_eq!(config.admin_pages.len(), 1);
    assert_eq!(
        config.resolved_admin_menu(),
        [AdminMenuItem::new("Widgets", "/widgets")]
    );
    assert_eq!(
        config.webhooks.keys().collect::<Vec<_>>(),
        ["orders/create", "orders/updated", "app/uninstalled"]
    );
    assert_eq!(
        config.proxies.keys().collect::<Vec<_>>(),
        ["/widgets", "/banner", "/page"]
    );
    assert_eq!(config.routes.keys().collect::<Vec<_>>(), ["/status"]);
    let mcp = config.mcp.as_ref().unwrap();
    assert_eq!(mcp.tools.keys().collect::<Vec<_>>(), ["widgets.count", "widgets.ping"]);
    assert_eq!(
        mcp.resources.keys().collect::<Vec<_>>(),
        ["app://widgets/guide", "app://widgets/raw"]
    );
    // The permission came with the capabilities that require it.
    assert!(mcp.custom_permissions.is_empty());
    assert_eq!(mcp.tools["widgets.count"].declared_permissions, [READ_WIDGETS]);
    let registry = openshop::server::mcp::registry::build_mcp_registry(
        Some(mcp),
        &openshop::server::mcp::core::create_core_mcp_capabilities(),
    )
    .unwrap();
    assert_eq!(registry.permissions[READ_WIDGETS.key].label, "Read widgets");
    assert_eq!(config.crons, [Cron::new("0 2 * * *", "nightlyCleanup").all_shops()]);
    assert_eq!(config.worker.concurrency, Some(2));
    assert!(config.on_error.is_some());

    // Declaring an application is not building it once: each reading starts over.
    assert_eq!(APP.config().unwrap().flows.len(), 3);
}

#[test]
fn a_declaration_can_be_added_to() {
    let config = APP
        .builder()
        .unwrap()
        .flow(Flow::new("extra", |_ctx| async { Ok(()) }))
        .build()
        .unwrap();
    assert_eq!(
        config.flows.keys().collect::<Vec<_>>(),
        ["greet", "nightlyCleanup", "parse", "extra"]
    );

    // A setting that fails makes the declaration fail, when it is read.
    assert_eq!(refusal(UNSET.config()), "OPENSHOP_TEST_UNSET_CONCURRENCY is not set");
    assert!(UNSET.builder().is_err());

    // A block is added by its name, whatever its kind.
    let config = OpenShopConfig::builder()
        .add::<Warehouse>()
        .add::<greet>()
        .add::<Widget>()
        .add::<status>()
        .build()
        .unwrap();
    assert!(config.providers.contains_key("warehouse"));
    assert!(config.flows.contains_key("greet"));
    assert_eq!(config.models[0].table, "widgets");
    assert!(config.routes.contains_key("/status"));
}

#[test]
fn a_block_is_registered_once() {
    let builder = OpenShopConfig::builder;
    for (config, problem) in [
        (
            builder().add::<greet>().add::<greet>(),
            "flows.greet is registered twice",
        ),
        (
            builder().add::<Warehouse>().add::<Warehouse>(),
            "providers.warehouse is registered twice",
        ),
        (
            builder().add::<Widget>().add::<Widget>(),
            "models.widgets is registered twice",
        ),
        (
            builder().add::<order_changed>().add::<order_changed>(),
            "webhooks.orders/create is registered twice",
        ),
        (
            builder().add::<CartRules>().add::<CartRules>(),
            "functions.cartRules is registered twice",
        ),
        (
            builder().add::<ping_widgets>().add::<ping_widgets>(),
            "mcp.tools.widgets.ping is registered twice",
        ),
        (
            builder().add::<raw_widgets>().add::<raw_widgets>(),
            "mcp.resources.app://widgets/raw is registered twice",
        ),
        (
            builder().add::<list_widgets>().add::<list_widgets_again>(),
            "proxies./widgets handles GET twice",
        ),
        (
            builder().add::<banner>().add::<banner_as_json>(),
            "proxies./banner answers with both liquid and json; a path has one response type",
        ),
        (
            builder().add::<status>().add::<status_again>(),
            "routes./status handles GET twice",
        ),
    ] {
        assert_eq!(refusal(config.build()), format!("[openshop] Invalid config: {problem}"));
    }

    // The same holds for what is registered by hand.
    let twice = builder()
        .flow(Flow::new("extra", |_ctx| async { Ok(()) }))
        .flow(Flow::new("extra", |_ctx| async { Ok(()) }));
    assert_eq!(
        refusal(twice.build()),
        "[openshop] Invalid config: flows.extra is registered twice"
    );
    // While the methods of a path can come one at a time.
    let config = builder()
        .proxy("/widgets", Proxy::default().get(|_ctx| async { Ok(Value::Null) }))
        .proxy("/widgets", Proxy::default().post(|_ctx| async { Ok(Value::Null) }))
        .route(
            "/status",
            Route::public().get(|_ctx| async { Ok(response::text("up")) }),
        )
        .route(
            "/status",
            Route::authenticated(bearer).post(|_ctx| async { Ok(response::text("set")) }),
        )
        .build()
        .unwrap();
    assert_eq!(
        config.proxies["/widgets"].handlers.keys().copied().collect::<Vec<_>>(),
        ["GET", "POST"]
    );
    assert_eq!(
        config.routes["/status"].handlers.keys().copied().collect::<Vec<_>>(),
        ["GET", "POST"]
    );
}

// ─── Flows ───────────────────────────────────────────────────────────

#[test]
fn a_flow_keeps_its_name_and_its_options() {
    assert_eq!(greet::NAME, "greet");
    assert_eq!(nightly_cleanup::NAME, "nightlyCleanup");

    let config = APP.config().unwrap();
    let flow = &config.flows["greet"];
    assert_eq!(flow.name, "greet");
    assert_eq!(flow.timeout, Some(Duration::from_secs(30)));
    assert_eq!(flow.step_timeout, Some(Duration::from_secs(5)));
    assert_eq!(flow.concurrency, Some(Concurrency::Allow));
    assert_eq!(flow.retry_policy.max_attempts, Some(2));
    let schema = &flow.input.as_ref().unwrap().json_schema;
    assert_eq!(schema["properties"]["name"]["minLength"], 1);

    let plain = &config.flows["nightlyCleanup"];
    assert!(plain.input.is_none());
    assert_eq!((plain.timeout, plain.concurrency), (None, None));
}

#[openshop::test(TestContext::new(APP.config().unwrap()))]
async fn a_flow_runs_with_its_typed_input(ctx: &TestContext) {
    let result = ctx.run_flow(greet::NAME, json!({ "name": "Ada" })).await.unwrap();
    assert_eq!(result, RunOutcome::Completed);
    assert_eq!(seen("greet:"), ["greet:hello Ada"]);

    for input in [json!({}), json!({ "name": "" }), json!({ "name": 3 })] {
        let result = ctx.run_flow(greet::NAME, input).await.unwrap();
        assert!(matches!(
            result,
            RunOutcome::Failed { ref error, .. } if error.starts_with("Invalid input for flow \"greet\"")
        ));
    }
    assert_eq!(seen("greet:").len(), 1);

    let result = ctx.run_flow(nightly_cleanup::NAME, Value::Null).await.unwrap();
    assert_eq!(result, RunOutcome::Completed);
    assert_eq!(seen("cleanup:"), [format!("cleanup:{}", ctx.shop)]);

    // An error of the function's own type fails the run.
    assert_eq!(
        ctx.run_flow(parse::NAME, json!("12")).await.unwrap(),
        RunOutcome::Completed
    );
    let result = ctx.run_flow(parse::NAME, json!("twelve")).await.unwrap();
    assert!(matches!(
        result,
        RunOutcome::Failed { ref error, .. } if error.contains("invalid digit")
    ));
}

// ─── Webhooks ────────────────────────────────────────────────────────

#[tokio::test]
async fn a_webhook_handles_its_topics_with_a_typed_payload() {
    assert_eq!(order_changed::TOPICS, ["orders/create", "orders/updated"]);
    let ctx = TestContext::new(APP.config().unwrap()).await.unwrap();
    let webhooks = &ctx.app.config().webhooks;
    let event = |topic: &str, payload: Value| WebhookContext {
        topic: topic.to_string(),
        shop: "webhooks.myshopify.com".to_string(),
        shopify_app: "default".to_string(),
        payload,
        api_version: "2026-04".to_string(),
        db: ctx.db.clone(),
        shopify: ctx.app.shopify_client("webhooks.myshopify.com", "default"),
        connectors: ctx.app.connectors("webhooks.myshopify.com", "default"),
        app: ctx.app.clone(),
    };

    for topic in ["orders/create", "orders/updated"] {
        (webhooks[topic].run)(event(topic, json!({ "name": "#1001", "total_price": "10.00" })))
            .await
            .unwrap();
    }
    (webhooks["app/uninstalled"].run)(event("app/uninstalled", json!({})))
        .await
        .unwrap();
    assert_eq!(
        seen("webhook:"),
        [
            "webhook:orders/create:#1001",
            "webhook:orders/updated:#1001",
            "webhook:app/uninstalled:webhooks.myshopify.com",
        ]
    );

    // A payload that is not what the function reads fails the delivery.
    let error = (webhooks["orders/create"].run)(event("orders/create", json!({ "id": 1 })))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("missing field `name`"), "{error}");
    ctx.shutdown().await.unwrap();
}

// ─── App proxy and server routes ─────────────────────────────────────

#[tokio::test]
async fn proxy_functions_share_a_path_by_method() {
    assert_eq!((list_widgets::METHOD, list_widgets::PATH), ("GET", "/widgets"));
    assert_eq!((create_widget::METHOD, create_widget::PATH), ("POST", "/widgets"));
    let config = APP.config().unwrap();
    assert_eq!(config.proxies["/widgets"].response_type, ProxyResponseType::Json);
    assert_eq!(config.proxies["/banner"].response_type, ProxyResponseType::Liquid);
    assert_eq!(config.proxies["/page"].response_type, ProxyResponseType::Html);

    let ctx = TestContext::new(config).await.unwrap();
    let proxy = ctx.proxy();

    let listed = proxy.get("/widgets").qs(&[("page", "4")]).send().await.unwrap();
    assert_eq!(listed.status, 200);
    assert_eq!(listed.body, json!({ "shop": ctx.shop, "page": 4 }));

    let created = proxy
        .post("/widgets")
        .json(json!({ "name": "gear" }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.body, json!({ "created": { "name": "gear" } }));

    let banner = proxy.get("/banner").send().await.unwrap();
    assert_eq!(banner.content_type, "application/liquid");
    assert_eq!(banner.text, format!("<p>{{{{ shop.name }}}} via {}</p>", ctx.shop));

    let page = proxy.get("/page").send().await.unwrap();
    assert_eq!(page.content_type, "text/html");
    assert_eq!(page.text, "<h1>Page</h1>");

    assert_eq!(proxy.delete("/widgets").send().await.unwrap().status, 404);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn route_functions_share_a_path_with_their_own_authentication() {
    assert_eq!((set_status::METHOD, set_status::PATH), ("POST", "/status"));
    let ctx = TestContext::new(APP.config().unwrap()).await.unwrap();
    let url = ctx.url_for("/routes/status");

    let public = ctx.http.get(&url).send().await.unwrap();
    assert_eq!(public.status(), 200);
    assert_eq!(public.json::<Value>().await.unwrap(), json!({ "status": "up" }));

    let refused = ctx.http.post(&url).body("down").send().await.unwrap();
    assert_eq!(refused.status(), 401);

    let accepted = ctx
        .http
        .post(&url)
        .header("authorization", "Bearer let-me-in")
        .body("down")
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), 201);
    assert_eq!(accepted.text().await.unwrap(), "partner set down");

    let unknown = ctx.http.delete(&url).send().await.unwrap();
    assert_eq!(unknown.status(), 405);
    assert_eq!(unknown.headers()["allow"], "GET, POST");
    ctx.shutdown().await.unwrap();
}

// ─── MCP ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn mcp_functions_carry_their_description_and_their_schema() {
    assert_eq!(count_widgets::NAME, "widgets.count");
    assert_eq!(widget_guide::URI, "app://widgets/guide");
    let ctx = TestContext::new(APP.config().unwrap()).await.unwrap();
    let mcp = ctx.app.config().mcp.clone().unwrap();
    let execution = || McpContext {
        shop: "mcp.myshopify.com".to_string(),
        shopify_app: "default".to_string(),
        token_id: "token".to_string(),
        permissions: vec![READ_WIDGETS.key.to_string()],
        signal: CancellationToken::new(),
        db: ctx.db.clone(),
        shopify: ctx.app.shopify_client("mcp.myshopify.com", "default"),
        connectors: ctx.app.connectors("mcp.myshopify.com", "default"),
        app: ctx.app.clone(),
    };
    let arguments = |value: Value| -> JsonMap { value.as_object().cloned().unwrap_or_default() };

    let count = &mcp.tools["widgets.count"];
    assert_eq!(count.description, "Counts the widgets of a color.\n\nCosts nothing.");
    assert_eq!(count.required_permissions, [READ_WIDGETS.key]);
    assert_eq!(count.risk_level, Some(McpRiskLevel::Medium));
    assert_eq!(count.confirmation_hint.as_deref(), Some("Counting is safe"));
    let schema = count.input_schema.as_ref().unwrap();
    assert_eq!(schema["required"], json!(["color"]));
    assert_eq!(
        (count.run)(execution(), arguments(json!({ "color": "red" })))
            .await
            .unwrap(),
        json!({ "shop": "mcp.myshopify.com", "color": "red", "count": 3 })
    );
    let error = (count.run)(execution(), arguments(json!({}))).await.unwrap_err();
    assert!(error.to_string().contains("missing field `color`"), "{error}");

    let ping = &mcp.tools["widgets.ping"];
    assert_eq!(ping.description, "Says pong.");
    assert!(ping.required_permissions.is_empty());
    assert_eq!(
        ping.input_schema,
        Some(json!({ "type": "object", "additionalProperties": false, "properties": {} }))
    );
    assert_eq!((ping.run)(execution(), JsonMap::new()).await.unwrap(), json!("pong"));

    let guide = &mcp.resources["app://widgets/guide"];
    assert_eq!(guide.name, "Widget guide");
    assert_eq!(guide.description.as_deref(), Some("How widgets work."));
    assert_eq!(guide.mime_type.as_deref(), Some("text/markdown"));
    assert_eq!(guide.required_permissions, [READ_WIDGETS.key]);
    assert_eq!(guide.risk_level, Some(McpRiskLevel::Low));
    assert_eq!(
        (guide.read)(execution()).await.unwrap(),
        McpResourceContent::from("# Widgets of mcp.myshopify.com")
    );

    let raw = &mcp.resources["app://widgets/raw"];
    assert_eq!(raw.description.as_deref(), Some("Stated in the attribute."));
    assert_eq!(
        (raw.read)(execution()).await.unwrap().mime_type.as_deref(),
        Some("text/plain")
    );
    ctx.shutdown().await.unwrap();
}

// ─── Admin pages ─────────────────────────────────────────────────────

#[tokio::test]
async fn an_admin_page_is_an_impl_block() {
    assert_eq!(WidgetsPage::PATH, "/widgets");
    let ctx = TestContext::new(APP.config().unwrap()).await.unwrap();
    let page = ctx.app.config().admin_pages[0].clone();
    assert_eq!(page.path, "/widgets");
    assert_eq!(
        page.functions.keys().collect::<Vec<_>>(),
        ["listWidgets", "find", "renameWidget", "refresh"]
    );
    let context = |actor: &str| {
        admin_context(
            &ctx.app,
            AdminTestOptions {
                shop: ctx.shop.clone(),
                actor_id: actor.to_string(),
                ..Default::default()
            },
        )
    };

    let access = page.page_access.as_ref().unwrap();
    assert!(access(context("staff").await.unwrap()).await.unwrap());
    assert!(!access(context("stranger").await.unwrap()).await.unwrap());

    let listed: Value = invoke_admin_function(
        &page.functions["listWidgets"],
        context("staff").await.unwrap(),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(listed, json!({ "widgets": [format!("widget of {}", ctx.shop)] }));

    let found: Value = invoke_admin_function(
        &page.functions["find"],
        context("staff").await.unwrap(),
        json!({ "name": "gear" }),
    )
    .await
    .unwrap();
    assert_eq!(found, json!({ "found": "gear" }));

    let rename = &page.functions["renameWidget"];
    let renamed: Value = invoke_admin_function(rename, context("manager").await.unwrap(), json!({ "name": "cog" }))
        .await
        .unwrap();
    assert_eq!(renamed, json!({ "renamed": "cog", "by": "manager" }));
    let refused =
        invoke_admin_function::<Value>(rename, context("staff").await.unwrap(), json!({ "name": "cog" })).await;
    assert_eq!(
        refused.unwrap_err().to_string(),
        "Admin function authorization rejected"
    );
    let invalid =
        invoke_admin_function::<Value>(rename, context("manager").await.unwrap(), json!({ "name": "" })).await;
    assert!(invalid.is_err());

    let refreshed: bool = invoke_admin_function(&page.functions["refresh"], context("staff").await.unwrap(), json!({}))
        .await
        .unwrap();
    assert!(refreshed);
    ctx.shutdown().await.unwrap();
}

// ─── Shopify Functions ───────────────────────────────────────────────

#[test]
fn a_function_is_its_configuration_struct() {
    assert_eq!(VolumeDiscount::NAME, "volumeDiscount");
    assert_eq!(HideExpress::NAME, "hideExpress");

    let function = VolumeDiscount::definition();
    assert_eq!(function.function_type, FunctionType::Discount);
    assert_eq!(function.handle, "volume-discount");
    assert_eq!(function.label.as_deref(), Some("Volume discount"));
    assert_eq!(function.modes, Some(vec![DiscountMode::Automatic, DiscountMode::Code]));
    assert_eq!(
        function.config.keys().collect::<Vec<_>>(),
        ["minQuantity", "percentage", "excludedTags"]
    );
    assert!(function.config["minQuantity"].is_required());
    assert!(!function.config["excludedTags"].is_required());
    assert_eq!(
        function.config["excludedTags"].placeholder.as_deref(),
        Some("gift, sample")
    );

    let owner = function.owner.as_ref().unwrap();
    assert_eq!(
        (
            owner.starts_at,
            owner.ends_at,
            owner.usage_limit,
            owner.applies_on_each_item,
            owner.enabled
        ),
        (Some(true), Some(false), Some(true), Some(true), None)
    );
    let combines = owner.combines_with.unwrap();
    assert_eq!(
        (
            combines.product_discounts,
            combines.order_discounts,
            combines.shipping_discounts
        ),
        (Some(true), Some(false), None)
    );
    let FunctionTitle::Dynamic(title) = &owner.title else {
        panic!("a title computed from the instance");
    };
    let instance = json!({ "minQuantity": 3, "percentage": 12.5 });
    assert_eq!(title(instance.as_object().unwrap()), "12.5% off 3+ items");
    // Without a readable configuration the instance is called after the function.
    assert_eq!(title(&JsonMap::new()), "Volume discount");

    let read = VolumeDiscount::from_config(instance.as_object().cloned().unwrap()).unwrap();
    assert_eq!(
        (read.min_quantity, read.percentage, read.excluded_tags),
        (3, 12.5, None)
    );

    let hidden = HideExpress::definition();
    assert_eq!(hidden.function_type, FunctionType::DeliveryCustomization);
    assert!(hidden.config.is_empty() && hidden.modes.is_none());
    let owner = hidden.owner.as_ref().unwrap();
    assert!(matches!(&owner.title, FunctionTitle::Static(title) if title == "Hide express"));
    assert_eq!(owner.enabled, Some(true));

    let rules = CartRules::definition();
    assert!(rules.owner.is_none());
    assert_eq!(rules.config.keys().collect::<Vec<_>>(), ["bundleTag"]);
    assert_eq!(
        CartRules::from_config(json!({ "bundleTag": "kit" }).as_object().cloned().unwrap())
            .unwrap()
            .bundle_tag,
        "kit"
    );
}

// ─── The function stays a function ───────────────────────────────────

#[test]
fn a_declared_function_is_still_a_function() {
    // The attribute adds a type of the same name; the function is untouched.
    fn takes_flow<F, Fut>(_flow: F)
    where
        F: Fn(FlowContext, GreetInput) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
    }
    fn takes_proxy<F, Fut>(_proxy: F)
    where
        F: Fn(ProxyContext) -> Fut,
        Fut: Future<Output = Result<String>>,
    {
    }
    takes_flow(greet);
    takes_proxy(banner);
    // What `refresh` reads and `report` wrote, for the compiler.
    let _ = (seen("failed:"), WidgetsPage::refresh, raw_widgets, uninstalled);
}
