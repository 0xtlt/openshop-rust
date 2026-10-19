use openshop::prelude::*;

#[proxy(FETCH, "/widgets")]
async fn widgets(_ctx: ProxyContext) -> Result<Value> {
    Ok(json!({}))
}

fn main() {}
