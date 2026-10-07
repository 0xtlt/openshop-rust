use openshop::response::Response;
use openshop::{Result, ServerRouteContext, json, response, route};

#[route(GET, "/ping")]
pub async fn ping(_ctx: ServerRouteContext) -> Result<Response> {
    Ok(response::json(json!({ "ok": true, "source": "server-route" })))
}
