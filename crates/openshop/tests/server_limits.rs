mod common;

use std::borrow::Cow;

use common::noop_flow;
use openshop::builder::Route;
use openshop::server::ServerOptions;
use openshop::testing::{TestContext, TestOptions, sign_query, to_query_string};
use openshop::{OpenShopConfig, UiSource, Value, json, response};
use reqwest::Method;

fn echo_size() -> Route {
    Route::public().post(|ctx| async move { Ok(response::json(json!({ "bytes": ctx.request.body.len() }))) })
}

#[tokio::test]
async fn unsupported_methods_on_known_paths_are_plain_404s() {
    let ctx = TestContext::new(OpenShopConfig::builder().flow(noop_flow("sync")).build().unwrap())
        .await
        .unwrap();

    for (method, path) in [
        (Method::POST, "/health"),
        (Method::POST, "/robots.txt"),
        (Method::GET, "/webhooks"),
        (Method::DELETE, "/auth"),
        (Method::GET, "/auth/webhooks/app-uninstalled"),
    ] {
        let response = ctx
            .http
            .request(method.clone(), ctx.url_for(path))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 404, "{method} {path}");
        assert_eq!(response.text().await.unwrap(), "404 Not Found", "{method} {path}");
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn request_bodies_are_limited_to_two_megabytes_by_default() {
    let config = OpenShopConfig::builder().route("/echo", echo_size()).build().unwrap();
    let ctx = TestContext::new(config).await.unwrap();

    let accepted = ctx
        .http
        .post(ctx.url_for("/routes/echo"))
        .body(vec![b'a'; 1024 * 1024])
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.json::<Value>().await.unwrap(), json!({ "bytes": 1024 * 1024 }));

    let rejected = ctx
        .http
        .post(ctx.url_for("/routes/echo"))
        .body(vec![b'a'; 3 * 1024 * 1024])
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), 413);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_request_body_limit_is_configurable() {
    let config = OpenShopConfig::builder()
        .route("/echo", echo_size())
        .max_request_body_bytes(4 * 1024 * 1024)
        .build()
        .unwrap();
    let ctx = TestContext::new(config).await.unwrap();

    let accepted = ctx
        .http
        .post(ctx.url_for("/routes/echo"))
        .body(vec![b'a'; 3 * 1024 * 1024])
        .send()
        .await
        .unwrap();
    assert_eq!(
        accepted.json::<Value>().await.unwrap(),
        json!({ "bytes": 3 * 1024 * 1024 })
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn responses_are_compressed_when_the_client_accepts_it() {
    let flows = (0..40).map(|index| noop_flow(&format!("flow-number-{index}")));
    let config = flows
        .fold(OpenShopConfig::builder(), |builder, flow| builder.flow(flow))
        .build()
        .unwrap();
    let ctx = TestContext::new(config).await.unwrap();

    let compressed = ctx
        .get("/api/flows")
        .header("accept-encoding", "gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(compressed.status(), 200);
    assert_eq!(compressed.headers()["content-encoding"], "gzip");

    let brotli = ctx
        .get("/api/flows")
        .header("accept-encoding", "br")
        .send()
        .await
        .unwrap();
    assert_eq!(brotli.headers()["content-encoding"], "br");

    let plain = ctx.get("/api/flows").send().await.unwrap();
    assert!(plain.headers().get("content-encoding").is_none());
    assert_eq!(plain.json::<Vec<Value>>().await.unwrap().len(), 40);

    // A body that fits in one packet is sent as it is.
    let small = ctx
        .http
        .get(ctx.url_for("/health"))
        .header("accept-encoding", "gzip, br")
        .send()
        .await
        .unwrap();
    assert!(small.headers().get("content-encoding").is_none());
    assert_eq!(small.json::<Value>().await.unwrap()["status"], "ok");
    ctx.shutdown().await.unwrap();
}

fn bundled_asset(path: &str) -> Option<Cow<'static, [u8]>> {
    match path {
        "index.html" => Some(Cow::Borrowed(
            br#"<html><head><meta name="shopify-api-key" content="" /></head><body>custom bundle</body></html>"#,
        )),
        "app-0123456789abcdef.js" => Some(Cow::Borrowed(b"console.log('custom')")),
        _ => None,
    }
}

#[tokio::test]
async fn serves_a_ui_bundle_compiled_into_the_application() {
    let ctx = TestContext::with_options(
        OpenShopConfig::builder().build().unwrap(),
        TestOptions {
            access_token: Some("shpat_test".to_string()),
            server: ServerOptions {
                ui: UiSource::assets(bundled_asset),
                dev: false,
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let query = to_query_string(&sign_query(
        &[("shop", ctx.shop.as_str()), ("timestamp", "1780000000")],
        &ctx.secret,
    ));
    let shell = ctx.http.get(ctx.url_for(&format!("/?{query}"))).send().await.unwrap();
    assert_eq!(shell.status(), 200);
    let html = shell.text().await.unwrap();
    assert!(html.contains("custom bundle"));
    assert!(html.contains(r#"<meta name="shopify-api-key" content="test-app" />"#));

    let asset = ctx
        .http
        .get(ctx.url_for("/app-0123456789abcdef.js"))
        .send()
        .await
        .unwrap();
    assert_eq!(asset.status(), 200);
    assert_eq!(asset.headers()["cache-control"], "public, max-age=31536000, immutable");
    assert_eq!(asset.text().await.unwrap(), "console.log('custom')");

    let missing = ctx.http.get(ctx.url_for("/missing.js")).send().await.unwrap();
    assert_eq!(missing.status(), 404);
    ctx.shutdown().await.unwrap();
}
