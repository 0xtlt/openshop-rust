//! MCP capabilities of the demo: a tool behind a permission, and a resource.

use openshop::{McpExecutionContext, Result, Value, json, mcp_resource, mcp_tool};

/// The permission a token needs to call [`status`].
pub const READ_STATUS: &str = "demo:read_status";

/// Read a small OpenShop demo status payload.
#[mcp_tool("demo.status", permissions(READ_STATUS), risk = low)]
pub async fn status(ctx: McpExecutionContext) -> Result<Value> {
    Ok(json!({
        "content": [{
            "type": "text",
            "text": format!("Demo app is reachable for {} ({}).", ctx.shop, ctx.app_handle),
        }],
        "structuredContent": { "ok": true, "shop": ctx.shop, "appHandle": ctx.app_handle },
    }))
}

/// Example resource exposed by the demo app.
#[mcp_resource("openshop://demo/mcp", name = "Demo MCP guide", mime_type = "text/markdown")]
pub async fn guide(_ctx: McpExecutionContext) -> Result<String> {
    Ok([
        "# Demo MCP",
        "",
        "Create an MCP token from the OpenShop admin, grant `demo:read_status`, then call `demo.status`.",
        "Core tools such as `openshop.logs.search` are available through OpenShop itself.",
    ]
    .join("\n"))
}
