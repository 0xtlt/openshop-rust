use openshop::prelude::*;

/// Lists the reviews.
#[mcp_tool("reviews.list", risk = extreme)]
async fn list_reviews(_ctx: McpContext) -> Result<Value> {
    Ok(json!([]))
}

fn main() {}
