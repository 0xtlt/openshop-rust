use openshop::{ServerRoute, json, response};

pub fn ping() -> ServerRoute {
    ServerRoute::public().get(|_ctx| async move { Ok(response::json(json!({ "ok": true, "source": "server-route" }))) })
}
