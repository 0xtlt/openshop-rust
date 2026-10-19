use openshop::prelude::*;

#[mcp_tool("reviews.list")]
async fn list_reviews(_ctx: McpContext) -> Result<Value> {
    Ok(json!([]))
}

fn main() {}
