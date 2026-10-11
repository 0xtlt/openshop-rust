//! MCP capabilities of the demo: a tool behind a permission, and a resource.

use openshop::prelude::*;
use serde::Serialize;

/// The permission a token needs to call [`status`]. The tool lists it, which
/// registers it with the application.
pub const READ_STATUS: McpPermission = McpPermission::new("demo:read_status", "Read demo status")
    .description("Allows MCP tokens to read a small demo status payload.")
    .group("Demo")
    .risk(McpRiskLevel::Low);

/// What [`status`] answers.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub ok: bool,
    pub shop: String,
    pub shopify_app: String,
}

/// Read a small OpenShop demo status payload.
#[mcp_tool("demo.status", permissions(READ_STATUS), risk = low)]
pub async fn status(ctx: McpContext) -> Result<Status> {
    Ok(Status {
        ok: true,
        shop: ctx.shop,
        shopify_app: ctx.shopify_app,
    })
}

/// Example resource exposed by the demo app.
#[mcp_resource("openshop://demo/mcp", name = "Demo MCP guide", mime_type = "text/markdown")]
pub async fn guide(_ctx: McpContext) -> Result<String> {
    Ok([
        "# Demo MCP",
        "",
        "Create an MCP token from the OpenShop admin, grant `demo:read_status`, then call `demo.status`.",
        "Core tools such as `openshop.logs.search` are available through OpenShop itself.",
    ]
    .join("\n"))
}
