use openshop::prelude::*;

#[route(GET, "/ping", json)]
pub async fn ping(_ctx: RouteContext) -> Result<Value> {
    Ok(json!({ "ok": true, "source": "server-route" }))
}
