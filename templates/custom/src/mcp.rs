//! MCP capabilities of the application: a tool behind a permission, and a resource.

use openshop::prelude::*;
use serde::Serialize;

/// The permission a token needs to call [`status`]. The tool lists it, which
/// registers it with the application.
pub const READ_STATUS: McpPermission = McpPermission::new("status:read", "Read the status")
    .description("Allows MCP tokens to read the status of the application.")
    .group("Status")
    .risk(McpRiskLevel::Low);

/// What [`status`] answers.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub ok: bool,
    pub shop: String,
    pub shopify_app: String,
}

/// Reads the status of the application for the shop of the token.
#[mcp_tool("app.status", permissions(READ_STATUS), risk = low)]
pub async fn status(ctx: McpContext) -> Result<Status> {
    Ok(Status {
        ok: true,
        shop: ctx.shop,
        shopify_app: ctx.shopify_app,
    })
}

/// A guide to the MCP capabilities of the application.
#[mcp_resource("openshop://app/mcp", name = "MCP guide", mime_type = "text/markdown")]
pub async fn guide(_ctx: McpContext) -> Result<String> {
    Ok([
        "# MCP",
        "",
        "Create an MCP token from the OpenShop admin, grant `status:read`, then call `app.status`.",
        "Core tools such as `openshop.logs.search` are available through OpenShop itself.",
    ]
    .join("\n"))
}
