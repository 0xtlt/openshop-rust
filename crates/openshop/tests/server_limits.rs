mod common;

use std::borrow::Cow;
use std::time::Duration;

use common::noop_flow;
use openshop::builder::Route;
use openshop::server::{DEFAULT_MAX_REQUEST_BODY_BYTES, ServerOptions};
use openshop::testing::{TestContext, TestOptions, sign_query, to_query_string};
use openshop::{OpenShopConfig, UiSource, Value, json, response};
use reqwest::Method;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn echo_size() -> Route {
    Route::public().post(|ctx| async move { Ok(response::json(json!({ "bytes": ctx.request.body.len() }))) })
}

fn slow(delay: Duration) -> Route {
    Route::public().get(move |_ctx| async move {
        tokio::time::sleep(delay).await;
        Ok(response::json(json!({ "done": true })))
    })
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

// A body over the limit is refused as soon as the limit is crossed, before the
// rest of it has arrived. Closing the connection there resets it under a client
// that is still sending: its write fails and it never reads the answer.
#[tokio::test]
async fn a_client_still_sending_when_it_is_refused_reads_the_answer() {
    let config = OpenShopConfig::builder().route("/echo", echo_size()).build().unwrap();
    let ctx = TestContext::new(config).await.unwrap();
    let length = 3 * 1024 * 1024;
    let crossing = DEFAULT_MAX_REQUEST_BODY_BYTES + 1;

    let mut client = TcpStream::connect(("127.0.0.1", ctx.port)).await.unwrap();
    let head = format!("POST /routes/echo HTTP/1.1\r\nhost: localhost\r\ncontent-length: {length}\r\n\r\n");
    client.write_all(head.as_bytes()).await.unwrap();
    client.write_all(&vec![b'a'; crossing]).await.unwrap();
    let mut status = [0; 12];
    client.read_exact(&mut status).await.unwrap();
    assert_eq!(&status, b"HTTP/1.1 413");

    // The megabyte still to come is taken, and the connection ends without a reset.
    client.write_all(&vec![b'a'; length - crossing]).await.unwrap();
    client.read_to_end(&mut Vec::new()).await.unwrap();
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

    let query = to_query_string(&sign_query(&[("shop", ctx.shop.as_str())], &ctx.secret));
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

#[tokio::test]
async fn a_request_that_takes_too_long_is_answered_with_503() {
    let config = OpenShopConfig::builder()
        .route("/slow", slow(Duration::from_secs(30)))
        .route("/quick", slow(Duration::ZERO))
        .request_timeout(Duration::from_millis(300))
        .build()
        .unwrap();
    let ctx = TestContext::new(config).await.unwrap();

    let started = std::time::Instant::now();
    let timed_out = ctx.http.get(ctx.url_for("/routes/slow")).send().await.unwrap();
    assert_eq!(timed_out.status(), 503);
    assert_eq!(
        timed_out.json::<Value>().await.unwrap(),
        json!({ "error": "Request timed out" })
    );
    assert!(started.elapsed() < Duration::from_secs(10));

    let quick = ctx.http.get(ctx.url_for("/routes/quick")).send().await.unwrap();
    assert_eq!(quick.status(), 200);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_body_that_never_finishes_arriving_runs_into_the_timeout() {
    let config = OpenShopConfig::builder()
        .route("/echo", echo_size())
        .request_timeout(Duration::from_millis(300))
        .build()
        .unwrap();
    let ctx = TestContext::new(config).await.unwrap();

    let mut client = TcpStream::connect(("127.0.0.1", ctx.port)).await.unwrap();
    let head = "POST /routes/echo HTTP/1.1\r\nhost: localhost\r\ncontent-length: 1000\r\n\r\nabc";
    client.write_all(head.as_bytes()).await.unwrap();
    let mut status = [0; 12];
    tokio::time::timeout(Duration::from_secs(10), client.read_exact(&mut status))
        .await
        .expect("an answer before the client gives up")
        .unwrap();
    assert_eq!(&status, b"HTTP/1.1 503");
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn requests_above_the_concurrency_limit_are_shed_with_503() {
    let config = OpenShopConfig::builder()
        .route("/slow", slow(Duration::from_millis(800)))
        .max_concurrent_requests(2)
        .build()
        .unwrap();
    let ctx = TestContext::new(config).await.unwrap();
    let slow_url = ctx.url_for("/routes/slow");

    let busy: Vec<_> = (0..2)
        .map(|_| {
            let (http, url) = (ctx.http.clone(), slow_url.clone());
            tokio::spawn(async move { http.get(url).send().await.unwrap().status() })
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let shed = ctx.http.get(&slow_url).send().await.unwrap();
    assert_eq!(shed.status(), 503);
    assert_eq!(shed.headers()["retry-after"], "1");
    assert_eq!(
        shed.json::<Value>().await.unwrap(),
        json!({ "error": "Server is busy, try again" })
    );

    // The health probe is answered by a server that is full.
    let health = ctx.http.get(ctx.url_for("/health")).send().await.unwrap();
    assert_eq!(health.status(), 200);

    for request in busy {
        assert_eq!(request.await.unwrap(), 200);
    }
    // The slots are free again.
    let after = ctx.http.get(&slow_url).send().await.unwrap();
    assert_eq!(after.status(), 200);
    ctx.shutdown().await.unwrap();
}

#[test]
fn the_limits_must_be_positive() {
    let Err(error) = OpenShopConfig::builder().request_timeout(Duration::ZERO).build() else {
        panic!("a zero timeout is refused");
    };
    assert!(
        error.to_string().contains("request_timeout must be a positive number"),
        "{error}"
    );
    let Err(error) = OpenShopConfig::builder().max_concurrent_requests(0).build() else {
        panic!("no slot is refused");
    };
    assert!(
        error
            .to_string()
            .contains("max_concurrent_requests must be a positive integer"),
        "{error}"
    );
}
