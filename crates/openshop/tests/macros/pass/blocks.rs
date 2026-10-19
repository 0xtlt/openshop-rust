use openshop::prelude::*;
use std::time::Duration;

#[derive(serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
struct Greeting {
    name: String,
}

#[flow("greet", timeout = "1h30m", step_timeout = Duration::from_secs(5), concurrency = allow)]
async fn greet(_ctx: FlowContext, _input: Greeting) -> Result<()> {
    Ok(())
}

/// Named after the function.
#[flow]
async fn nightly_cleanup(_ctx: FlowContext) -> Result<()> {
    Ok(())
}

#[webhook("orders/create", "orders/updated")]
async fn order_changed(_ctx: WebhookContext) -> Result<()> {
    Ok(())
}

#[proxy(GET, "/widgets", json)]
async fn widgets(_ctx: ProxyContext) -> Result<Value> {
    Ok(json!([]))
}

#[route(GET, "/health")]
async fn health(_ctx: RouteContext) -> Result<String> {
    Ok("ok".into())
}

/// Lists the reviews.
#[mcp_tool("reviews.list", risk = low)]
async fn list_reviews(_ctx: McpContext) -> Result<Value> {
    Ok(json!([]))
}

#[mcp_resource("app://guide", name = "Guide", description = "How to use the app")]
async fn guide(_ctx: McpContext) -> Result<String> {
    Ok("read me".into())
}

static APP: App = app! {
    flows: [greet, nightly_cleanup],
    webhooks: [order_changed],
    proxies: [widgets],
    routes: [health],
    mcp_tools: [list_reviews],
    mcp_resources: [guide],
};

fn main() {
    assert_eq!(greet::NAME, "greet");
    assert_eq!(nightly_cleanup::NAME, "nightlyCleanup");
    assert_eq!(order_changed::TOPICS, ["orders/create", "orders/updated"]);
    assert_eq!(widgets::METHOD, "GET");
    assert_eq!(health::PATH, "/health");
    assert_eq!(list_reviews::NAME, "reviews.list");
    assert_eq!(guide::URI, "app://guide");
    let _ = &APP;
}
