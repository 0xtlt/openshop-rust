//! What a request for a page of the admin goes through before the page is
//! rendered: the launch Shopify signs, the installation of the shop, the app
//! whose key the page carries, the paths that are not pages, development
//! reloads; and the lifecycle of the server.

use std::io::ErrorKind;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::common::{TestApp, create_config, install, noop_flow, two_shopify_apps};
use axum::Router;
use openshop::server::ServerOptions;
use openshop::server::{listen, serve};
use openshop::testing::{TestContext, TestOptions, session_token, sign_query, to_query_string};
use openshop::{OpenShopConfig, UiSource, Value, json};
use reqwest::header::{CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, HOST, HeaderMap, LOCATION};
use reqwest::{Method, RequestBuilder, StatusCode};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

const SHOP: &str = "static-test.myshopify.com";
const SECRET: &str = "test-secret";
const ROBOTS: &str = "noindex, nofollow, noarchive, nosnippet";
const UNAUTHORIZED_UI: &str = "Open this app from Shopify admin";
/// What a rendered page has: the content the script replaces.
const PAGE: &str = r#"<main id="openshop-page">"#;

// ─── Helpers ─────────────────────────────────────────────────────────

fn static_config() -> OpenShopConfig {
    create_config([noop_flow("static-flow")])
}

fn multi_app_config() -> OpenShopConfig {
    OpenShopConfig::builder()
        .shopify(two_shopify_apps())
        .flow(noop_flow("static-flow"))
        .build()
        .unwrap()
}

fn server(dev: bool) -> ServerOptions {
    ServerOptions {
        ui: UiSource::Server,
        dev,
    }
}

/// A server with the given options whose test shop is installed.
async fn context_with(config: OpenShopConfig, server: ServerOptions) -> TestContext {
    TestContext::with_options(
        config,
        TestOptions {
            shop: SHOP.to_string(),
            access_token: Some("offline-token".to_string()),
            server,
            ..Default::default()
        },
    )
    .await
    .expect("test context")
}

async fn admin_context() -> TestContext {
    context_with(static_config(), server(false)).await
}

/// The path Shopify admin opens: launch parameters, and `extra`, signed with
/// the app secret, with the `timestamp` of now.
fn signed_launch(path: &str, shop: &str, secret: &str, extra: &[(&str, &str)]) -> String {
    let mut params = vec![("host", "admin.shopify.com/store/static-test"), ("shop", shop)];
    params.extend_from_slice(extra);
    format!("{path}?{}", to_query_string(&sign_query(&params, secret)))
}

/// The launch of a page with the session token Shopify opens it with.
fn launch(ctx: &TestContext, path: &str) -> String {
    signed_launch(path, SHOP, SECRET, &[("id_token", &ctx.session_token())])
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Reply {
    fn header(&self, name: impl reqwest::header::AsHeaderName) -> Option<&str> {
        self.headers.get(name).map(|value| value.to_str().unwrap())
    }

    fn content_type(&self) -> &str {
        self.header(CONTENT_TYPE).unwrap_or_default()
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|_| panic!("not JSON: {}", self.body))
    }
}

async fn fetch(request: RequestBuilder) -> Reply {
    let response = request.send().await.expect("response");
    Reply {
        status: response.status(),
        headers: response.headers().clone(),
        body: response.text().await.expect("response body"),
    }
}

fn request(ctx: &TestContext, method: Method, path: &str) -> RequestBuilder {
    ctx.http.request(method, format!("{}{path}", ctx.url))
}

async fn get(ctx: &TestContext, path: &str) -> Reply {
    fetch(request(ctx, Method::GET, path)).await
}

/// The page a visitor without a shop receives: where the app opens from, and
/// nothing of the application.
fn assert_unauthorized_ui(reply: &Reply) {
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{}", reply.body);
    assert_eq!(reply.header("x-robots-tag"), Some(ROBOTS));
    assert_eq!(reply.content_type(), "text/html; charset=utf-8");
    assert_eq!(reply.header(CACHE_CONTROL), Some("no-store"));
    assert_eq!(
        reply.header(CONTENT_SECURITY_POLICY),
        Some(
            "default-src 'none'; script-src https://cdn.shopify.com/shopifycloud/polaris.js; \
             style-src 'unsafe-inline' https://cdn.shopify.com/static/fonts/; \
             font-src https://cdn.shopify.com/static/fonts/; connect-src https://cdn.shopify.com/shopifycloud/; \
             form-action 'none'; base-uri 'none'"
        )
    );
    assert!(
        reply
            .body
            .contains(&format!("<s-heading>{UNAUTHORIZED_UI}</s-heading>")),
        "{}",
        reply.body
    );
    // Polaris is its only script, and it has nothing of the admin.
    assert_eq!(reply.body.matches("<script").count(), 1, "{}", reply.body);
    assert!(!reply.body.contains(PAGE));
    assert!(!reply.body.contains("shopify-api-key"));
}

/// A page of the admin, for the app whose key it carries.
fn assert_page(reply: &Reply, api_key: &str) {
    assert!(reply.body.contains(PAGE), "{}", reply.body);
    assert!(
        reply.content_type().to_lowercase().starts_with("text/html"),
        "{}",
        reply.content_type()
    );
    assert!(
        reply
            .body
            .contains(&format!(r#"<meta name="shopify-api-key" content="{api_key}">"#)),
        "{}",
        reply.body
    );
    assert!(
        reply.body.contains(&format!(
            r#"<script data-api-key="{api_key}" src="https://cdn.shopify.com/shopifycloud/app-bridge.js">"#
        )),
        "{}",
        reply.body
    );
}

/// A page loaded in the admin without a session token: sent to the bounce
/// page, where App Bridge gets one.
fn assert_bounced(reply: &Reply) {
    assert_eq!(reply.status, StatusCode::FOUND, "{}", reply.body);
    let location = reply.header(LOCATION).unwrap_or_default();
    assert!(location.starts_with("/auth/session-token?"), "{location}");
    assert!(!reply.body.contains(PAGE));
}

// ─── The launch ──────────────────────────────────────────────────────

#[tokio::test]
async fn get_robots_txt_disallows_all_crawlers() {
    let ctx = admin_context().await;

    let reply = get(&ctx, "/robots.txt").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.header("x-robots-tag"), Some(ROBOTS));
    assert!(
        reply.content_type().starts_with("text/plain"),
        "{}",
        reply.content_type()
    );
    assert_eq!(reply.body, "User-agent: *\nDisallow: /\n");
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn outside_shopify_admin_no_page_is_served() {
    let ctx = admin_context().await;

    for path in [
        "/",
        "/runs/550e8400-e29b-41d4-a716-446655440000",
        "/admin/deep/route",
        "/flows",
        "/flows/",
        "/index.html",
        "/apis",
        "/healthz",
    ] {
        assert_unauthorized_ui(&get(&ctx, path).await);
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_signed_launch_of_an_installed_shop_renders_its_page() {
    let ctx = admin_context().await;

    let reply = get(&ctx, &launch(&ctx, "/")).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_page(&reply, "test-app");
    assert_eq!(reply.header("x-robots-tag"), Some(ROBOTS));
    assert_eq!(reply.header(CACHE_CONTROL), Some("no-store"));
    assert_eq!(reply.header("x-content-type-options"), Some("nosniff"));
    // Framed by the admin of the shop of the session only.
    assert_eq!(
        reply.header(CONTENT_SECURITY_POLICY),
        Some("frame-ancestors https://static-test.myshopify.com https://admin.shopify.com;")
    );

    // Another page, and a path no page has: the document, and what the page says.
    let flows = get(&ctx, &launch(&ctx, "/flows/")).await;
    assert_eq!(flows.status, StatusCode::OK, "{}", flows.body);
    assert_page(&flows, "test-app");
    let nowhere = get(&ctx, &launch(&ctx, "/admin/deep/route")).await;
    assert_eq!(nowhere.status, StatusCode::NOT_FOUND);
    assert_page(&nowhere, "test-app");
    assert!(nowhere.body.contains("This page is disabled."), "{}", nowhere.body);

    // The shop is normalized before the installation lookup.
    let token = ctx.session_token();
    let reply = get(
        &ctx,
        &signed_launch("/", "Static-Test.MyShopify.com", SECRET, &[("id_token", &token)]),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_page(&reply, "test-app");
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_signed_launch_carries_the_key_of_the_app_that_signed_it() {
    let ctx = context_with(multi_app_config(), server(false)).await;
    let shop = "static-client-b.myshopify.com";
    install(&ctx.db, "clientB", shop, Some("offline-token"), false).await;
    let token = session_token(shop, "client-b-secret", "client-b-key", "1");

    let reply = get(
        &ctx,
        &signed_launch("/", shop, "client-b-secret", &[("id_token", &token)]),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_page(&reply, "client-b-key");
    assert!(!reply.body.contains("client-a-key"));
    assert_eq!(
        reply.header(CONTENT_SECURITY_POLICY),
        Some("frame-ancestors https://static-client-b.myshopify.com https://admin.shopify.com;")
    );

    // The shop installed app B only: a launch signed by app A starts its own OAuth flow.
    let reply = get(&ctx, &signed_launch("/", shop, "client-a-secret", &[])).await;
    assert_eq!(reply.status, StatusCode::FOUND);
    assert_eq!(
        reply.header(LOCATION),
        Some("/auth?shop=static-client-b.myshopify.com&app=clientA")
    );

    // The env-based secret of single-app mode signs nothing here: not a launch,
    // and without a token the page gets one from App Bridge first.
    assert_bounced(&get(&ctx, &signed_launch("/", shop, SECRET, &[])).await);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_signed_launch_of_an_uninstalled_shop_goes_to_auth() {
    let ctx = admin_context().await;

    let reply = get(&ctx, &signed_launch("/", "uninstalled.myshopify.com", SECRET, &[])).await;
    assert_eq!(reply.status, StatusCode::FOUND);
    assert_eq!(
        reply.header(LOCATION),
        Some("/auth?shop=uninstalled.myshopify.com&app=default")
    );
    assert_eq!(reply.header("x-robots-tag"), Some(ROBOTS));
    assert!(!reply.body.contains(PAGE));

    let reply = get(&ctx, &signed_launch("/flows", "uninstalled.myshopify.com", SECRET, &[])).await;
    assert_eq!(reply.status, StatusCode::FOUND);
    assert_eq!(
        reply.header(LOCATION),
        Some("/auth?shop=uninstalled.myshopify.com&app=default")
    );

    // An installation that was removed, or never received a token, does not count.
    sqlx::query("UPDATE installations SET uninstalled_at = now() WHERE shop = $1")
        .bind(SHOP)
        .execute(&ctx.db)
        .await
        .unwrap();
    let reply = get(&ctx, &signed_launch("/", SHOP, SECRET, &[])).await;
    assert_eq!(reply.status, StatusCode::FOUND);
    assert_eq!(
        reply.header(LOCATION),
        Some("/auth?shop=static-test.myshopify.com&app=default")
    );

    sqlx::query("INSERT INTO installations (shop, nonce) VALUES ('pending.myshopify.com', 'nonce')")
        .execute(&ctx.db)
        .await
        .unwrap();
    let reply = get(&ctx, &signed_launch("/", "pending.myshopify.com", SECRET, &[])).await;
    assert_eq!(reply.status, StatusCode::FOUND);
    assert_eq!(
        reply.header(LOCATION),
        Some("/auth?shop=pending.myshopify.com&app=default")
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_launch_whose_signature_does_not_verify_is_not_a_launch() {
    let ctx = admin_context().await;
    let valid = signed_launch("/", SHOP, SECRET, &[]);

    let signed_without_shop = to_query_string(&sign_query(&[], SECRET));
    let hmac = valid.rsplit("hmac=").next().unwrap();
    let timestamp = valid.split("timestamp=").nth(1).unwrap().split('&').next().unwrap();
    let later = (timestamp.parse::<i64>().unwrap() + 1).to_string();
    let stale = to_query_string(&sign_query(
        &[
            ("shop", "uninstalled.myshopify.com"),
            ("timestamp", &(timestamp.parse::<i64>().unwrap() - 120).to_string()),
        ],
        SECRET,
    ));
    let cases = [
        // Signed by someone else.
        signed_launch("/", "uninstalled.myshopify.com", "another-secret", &[]),
        // Parameters changed after signing.
        valid.replace(&format!("timestamp={timestamp}"), &format!("timestamp={later}")),
        valid.replace(SHOP, "uninstalled.myshopify.com"),
        format!("{valid}&extra=1"),
        // Missing or mangled signature.
        "/?shop=uninstalled.myshopify.com".to_string(),
        "/?shop=uninstalled.myshopify.com&hmac=".to_string(),
        format!("/?shop=uninstalled.myshopify.com&hmac={}", &hmac[..hmac.len() - 1]),
        // Signed, but more than 90 seconds ago: replayed.
        format!("/?{stale}"),
        // Correctly signed, but not for a shop.
        format!("/?{signed_without_shop}"),
        signed_launch("/", "evil.example.com", SECRET, &[]),
        signed_launch("/", "static-test.myshopify.com.evil.example.com", SECRET, &[]),
    ];
    for path in cases {
        // Not a launch: no installation is checked, and nothing renders
        // without a session token, which App Bridge gives in the admin.
        let reply = get(&ctx, &path).await;
        assert!(!reply.body.contains(PAGE), "{path}");
        let location = reply.header(LOCATION).unwrap_or_default();
        assert!(!location.starts_with("/auth?"), "{path}: {location}");
        match reply.status {
            StatusCode::FOUND => assert_bounced(&reply),
            _ => assert_unauthorized_ui(&reply),
        }
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_page_answers_get_head_and_its_forms() {
    let ctx = admin_context().await;
    let page = launch(&ctx, "/");

    let head = fetch(request(&ctx, Method::HEAD, &page)).await;
    assert_eq!(head.status, StatusCode::OK);
    assert!(head.body.is_empty());
    assert_eq!(head.header(CACHE_CONTROL), Some("no-store"));
    assert!(head.header(CONTENT_SECURITY_POLICY).is_some());

    // A form names an action of the page: the home has none.
    let form = fetch(
        ctx.post("/")
            .header("x-openshop-fetch", "1")
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body("_action=nothing"),
    )
    .await;
    assert_eq!(form.status, StatusCode::NOT_FOUND);
    assert_eq!(form.header("x-openshop-toast-error"), Some("Not%20found"));

    for method in [Method::PUT, Method::PATCH, Method::DELETE] {
        for target in [page.as_str(), "/"] {
            let reply = fetch(ctx.request(method.clone(), target)).await;
            assert_eq!(reply.status, StatusCode::NOT_FOUND, "{method} {target}");
            assert!(!reply.body.contains(PAGE));
        }
    }
    ctx.shutdown().await.unwrap();
}

/// The `<title>` of a page.
fn title(reply: &Reply) -> &str {
    let start = reply.body.find("<title>").expect("a title") + "<title>".len();
    let end = reply.body[start..].find("</title>").expect("the end of the title");
    &reply.body[start..start + end]
}

#[tokio::test]
async fn a_page_is_titled_with_the_name_of_the_app() {
    let cwd = std::env::temp_dir().join(format!("openshop-launch-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(
        cwd.join("shopify.app.toml"),
        "client_id = \"test-app\"\nname = \"Easy hide PayPal Express\"\nhandle = \"easy-hide-paypal-express\"\n",
    )
    .unwrap();
    let ctx = TestContext::with_options(
        static_config(),
        TestOptions {
            shop: SHOP.to_string(),
            access_token: Some("offline-token".to_string()),
            server: server(false),
            cwd: Some(cwd.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // The `name` of its `shopify.app.toml`: what App Bridge titles a page
    // without a heading with, in the title bar of the admin, and the tab shows.
    let reply = get(&ctx, &launch(&ctx, "/")).await;
    assert_page(&reply, "test-app");
    assert_eq!(title(&reply), "Easy hide PayPal Express");
    ctx.shutdown().await.unwrap();

    // Without a file, an app of `shopify.apps` is named by its key, in development too.
    let ctx = context_with(multi_app_config(), server(true)).await;
    let token = session_token(SHOP, "client-b-secret", "client-b-key", "1");
    let reply = get(&ctx, &format!("/?shop={SHOP}&id_token={token}")).await;
    assert_page(&reply, "client-b-key");
    assert_eq!(title(&reply), "clientB");
    ctx.shutdown().await.unwrap();

    // The app of the environment, without a file: nothing names it.
    let ctx = admin_context().await;
    assert_eq!(title(&get(&ctx, &launch(&ctx, "/")).await), "OpenShop");
    ctx.shutdown().await.unwrap();
    std::fs::remove_dir_all(cwd).ok();
}

// ─── Development mode ────────────────────────────────────────────────

/// The next version an event stream of `/__openshop/reload` names. `None` once it has ended.
async fn told(events: &mut reqwest::Response) -> Option<String> {
    loop {
        let chunk = tokio::time::timeout(Duration::from_secs(10), events.chunk())
            .await
            .expect("an event in time")
            .expect("event stream")?;
        let text = String::from_utf8_lossy(&chunk);
        if let Some(version) = text.lines().find_map(|line| line.strip_prefix("data: ")) {
            return Some(version.to_string());
        }
    }
}

#[tokio::test]
async fn dev_mode_reloads_the_page_when_the_server_starts_again_and_lets_the_server_close() {
    let ctx = context_with(static_config(), server(true)).await;
    let local = |path: &str| request(&ctx, Method::GET, path).header(HOST, format!("localhost:{}", ctx.port));
    let listens_for = |version: &str| format!(r#"if (event.data !== "{version}") location.reload();"#);

    let mut events = local("/__openshop/reload").send().await.expect("event stream");
    assert_eq!(events.status(), StatusCode::OK);
    let content_type = events.headers()[CONTENT_TYPE].to_str().unwrap().to_string();
    assert!(content_type.starts_with("text/event-stream"), "{content_type}");

    // The page is served with the version of the server, and listens for another one.
    let version = told(&mut events).await.expect("the version being served");
    let page = fetch(local(&format!("/?shop={SHOP}&id_token={}", ctx.session_token()))).await;
    assert_page(&page, "test-app");
    assert!(
        page.body.contains(r#"new EventSource("/__openshop/reload")"#),
        "{}",
        page.body
    );
    assert!(page.body.contains(&listens_for(&version)), "{}", page.body);
    // Framed by any shop, and by the pages of this machine.
    assert_eq!(
        page.header(CONTENT_SECURITY_POLICY),
        Some("frame-ancestors https://*.myshopify.com https://admin.shopify.com http://localhost:*;")
    );

    // A server that closes does not wait for the pages that listen to it.
    let closing = std::time::Instant::now();
    ctx.shutdown().await.unwrap();
    assert!(closing.elapsed() < Duration::from_secs(4), "{:?}", closing.elapsed());
    assert_eq!(told(&mut events).await, None);
}

#[tokio::test]
async fn only_dev_mode_reloads_pages() {
    let ctx = admin_context().await;

    let page = get(&ctx, &launch(&ctx, "/")).await;
    assert_page(&page, "test-app");
    assert!(!page.body.contains("EventSource"), "{}", page.body);
    // The event stream is a path like another: a page of the admin, behind its gate.
    assert_unauthorized_ui(&get(&ctx, "/__openshop/reload").await);
    ctx.shutdown().await.unwrap();
}

// ─── Routing around the pages ────────────────────────────────────────

#[tokio::test]
async fn get_mcp_is_a_page_while_post_mcp_is_the_protocol_endpoint() {
    let ctx = admin_context().await;

    for path in ["/mcp", "/mcp/"] {
        assert_unauthorized_ui(&get(&ctx, path).await);
        let page = get(&ctx, &launch(&ctx, path)).await;
        assert_eq!(page.status, StatusCode::OK, "{path}: {}", page.body);
        assert!(page.body.contains(r#"<s-page heading="MCP""#), "{}", page.body);

        let protocol = fetch(
            request(&ctx, Method::POST, path).json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" })),
        )
        .await;
        assert_eq!(protocol.status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(
            protocol.json(),
            json!({ "error": "Unauthorized: missing MCP bearer token" })
        );

        // A signed launch does not turn the protocol endpoint into a page.
        let signed = fetch(
            request(&ctx, Method::POST, &launch(&ctx, path))
                .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" })),
        )
        .await;
        assert_eq!(signed.status, StatusCode::UNAUTHORIZED, "{path}");
        assert!(!signed.body.contains(PAGE));
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn reserved_paths_are_never_pages() {
    let ctx = admin_context().await;

    // The Admin API keeps its own authentication, signed launch or not.
    for path in ["/api", "/api/flows", "/api/unknown"] {
        let reply = get(&ctx, &launch(&ctx, path)).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(reply.json(), json!({ "error": "Unauthorized: missing session token" }));
    }
    let api = fetch(ctx.get("/api/flows")).await;
    assert_eq!(api.status, StatusCode::OK);
    assert_eq!(api.json()[0]["name"], "static-flow");

    for path in [
        "/auth/unknown",
        "/webhooks/unknown",
        "/proxy/reviews",
        "/ext/reviews",
        "/routes/ping",
        "/health/deep",
        "/asset.js",
        "/favicon.ico",
    ] {
        for target in [path.to_string(), launch(&ctx, path)] {
            let reply = get(&ctx, &target).await;
            assert_eq!(reply.status, StatusCode::NOT_FOUND, "{target}");
            assert_eq!(reply.body, "404 Not Found", "{target}");
        }
    }

    let health = get(&ctx, &launch(&ctx, "/health")).await;
    assert_eq!(health.status, StatusCode::OK);
    assert_eq!(health.json()["status"], "ok");
    ctx.shutdown().await.unwrap();
}

#[openshop::test(context_with(static_config(), ServerOptions { ui: UiSource::None, dev: false }))]
async fn server_without_ui_returns_404_for_pages(ctx: &TestContext) {
    for path in [
        "/".to_string(),
        "/flows".to_string(),
        "/mcp".to_string(),
        "/index.html".to_string(),
        "/__openshop/openshop.js".to_string(),
        "/auth/session-token?shopify-reload=%2F".to_string(),
        signed_launch("/", SHOP, SECRET, &[]),
    ] {
        let reply = get(ctx, &path).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(reply.body, "404 Not Found", "{path}");
        assert_eq!(reply.header("x-robots-tag"), Some(ROBOTS), "{path}");
    }

    assert_eq!(get(ctx, "/health").await.status, StatusCode::OK);
    assert_eq!(get(ctx, "/robots.txt").await.status, StatusCode::OK);
    assert_eq!(fetch(ctx.get("/api/flows")).await.status, StatusCode::OK);
}

// ─── Health and crawler headers ──────────────────────────────────────

#[tokio::test]
async fn get_health_returns_ok_with_a_timestamp() {
    let api_only = ServerOptions {
        ui: UiSource::None,
        dev: false,
    };
    for server in [api_only, server(false), server(true)] {
        let ctx = context_with(static_config(), server).await;

        let reply = get(&ctx, "/health").await;
        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(reply.header("x-robots-tag"), Some(ROBOTS));
        assert!(
            reply.content_type().starts_with("application/json"),
            "{}",
            reply.content_type()
        );
        let health = reply.json();
        assert_eq!(health["status"], "ok");
        let timestamp = health["timestamp"].as_str().expect("timestamp");
        let parsed = chrono::DateTime::parse_from_rfc3339(timestamp).expect("ISO timestamp");
        assert!(
            timestamp.ends_with('Z') && timestamp.len() == "2026-01-01T00:00:00.000Z".len(),
            "{timestamp}"
        );
        assert!(
            (chrono::Utc::now() - parsed.with_timezone(&chrono::Utc))
                .num_seconds()
                .abs()
                < 60
        );
        ctx.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn x_robots_tag_is_set_on_every_response() {
    let ctx = admin_context().await;

    let requests = [
        (request(&ctx, Method::GET, "/health"), StatusCode::OK),
        (request(&ctx, Method::GET, "/robots.txt"), StatusCode::OK),
        (request(&ctx, Method::GET, "/__openshop/openshop.js"), StatusCode::OK),
        (request(&ctx, Method::GET, "/missing.png"), StatusCode::NOT_FOUND),
        (request(&ctx, Method::GET, "/"), StatusCode::UNAUTHORIZED),
        (request(&ctx, Method::GET, &launch(&ctx, "/")), StatusCode::OK),
        (
            request(
                &ctx,
                Method::GET,
                &signed_launch("/", "uninstalled.myshopify.com", SECRET, &[]),
            ),
            StatusCode::FOUND,
        ),
        (request(&ctx, Method::GET, "/api/flows"), StatusCode::UNAUTHORIZED),
        (ctx.get("/api/flows"), StatusCode::OK),
        (ctx.get("/api/unknown"), StatusCode::NOT_FOUND),
        (ctx.post("/api/flows/nope/run"), StatusCode::NOT_FOUND),
        (request(&ctx, Method::GET, "/auth"), StatusCode::BAD_REQUEST),
        (
            request(&ctx, Method::POST, "/webhooks").body("{}"),
            StatusCode::UNAUTHORIZED,
        ),
        (request(&ctx, Method::POST, "/mcp").body("{}"), StatusCode::UNAUTHORIZED),
        (request(&ctx, Method::GET, "/proxy/unknown"), StatusCode::NOT_FOUND),
    ];
    for (candidate, expected) in requests {
        let reply = fetch(candidate).await;
        assert_eq!(reply.status, expected);
        let values: Vec<_> = reply.headers.get_all("x-robots-tag").iter().collect();
        assert_eq!(values, [ROBOTS], "{expected}");
    }
    ctx.shutdown().await.unwrap();
}

// ─── Server startup ──────────────────────────────────────────────────

#[tokio::test]
async fn start_api_server_returns_a_closable_server_handle_that_can_rebind_the_same_port() {
    let test = TestApp::new(create_config([noop_flow("test-flow")])).await;

    // Another process may grab the freed port in between, so one clean cycle out of three is enough:
    // a server that kept its port would fail all of them.
    let mut rebound = false;
    for _ in 0..3 {
        let first = listen(&test.app, 0, ServerOptions::default()).await.unwrap();
        let port = first.addr.port();
        assert_ne!(port, 0);

        // The pooled keep-alive connection of this client must not hold the server open.
        let client = reqwest::Client::new();
        let health = client
            .get(format!("http://127.0.0.1:{port}/health"))
            .send()
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);
        assert_eq!(health.json::<Value>().await.unwrap()["status"], "ok");
        first.shutdown().await.unwrap();

        if let Ok(second) = listen(&test.app, port, ServerOptions::default()).await {
            assert_eq!(second.addr.port(), port);
            let health = reqwest::get(format!("http://127.0.0.1:{port}/health")).await.unwrap();
            assert_eq!(health.status(), StatusCode::OK);
            second.shutdown().await.unwrap();
            rebound = true;
            break;
        }
    }
    assert!(rebound, "the port is free again after shutdown");
    test.cleanup().await;
}

#[tokio::test]
async fn start_api_server_stops_accepting_connections_when_the_handle_is_dropped() {
    let test = TestApp::new(create_config([noop_flow("test-flow")])).await;

    // Another process may grab the freed port in between, so one refused connection out of three
    // cycles is enough: a server that kept listening would accept all of them.
    let mut refused = false;
    for _ in 0..3 {
        let server = listen(&test.app, 0, ServerOptions::default()).await.unwrap();
        let port = server.addr.port();
        let health = reqwest::get(format!("http://127.0.0.1:{port}/health")).await.unwrap();
        assert_eq!(health.status(), StatusCode::OK);
        drop(health);

        drop(server);

        // The server task closes the listener shortly after the drop, not within it.
        let deadline = Instant::now() + Duration::from_secs(2);
        while !refused && Instant::now() < deadline {
            match TcpStream::connect(("127.0.0.1", port)).await {
                Err(error) if error.kind() == ErrorKind::ConnectionRefused => refused = true,
                _ => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
        if refused {
            break;
        }
    }
    assert!(refused, "the port is closed after the handle is dropped");
    test.cleanup().await;
}

#[tokio::test]
async fn server_shutdown_stops_waiting_after_five_seconds_and_lets_the_request_in_flight_finish() {
    // Longer than the five seconds `shutdown()` waits.
    const HANDLER: Duration = Duration::from_millis(5500);
    let entered = Arc::new(Notify::new());
    let router = Router::new().route(
        "/slow",
        axum::routing::get({
            let entered = entered.clone();
            move || async move {
                entered.notify_one();
                tokio::time::sleep(HANDLER).await;
                "done"
            }
        }),
    );
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let server = serve(listener, router).await.unwrap();
    let slow = tokio::spawn(reqwest::get(format!("http://127.0.0.1:{}/slow", server.addr.port())));
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("the request reached its handler");

    let error = server.shutdown().await.expect_err("the request outlasts the wait");
    assert_eq!(error.to_string(), "Timed out closing HTTP server after 5000ms");

    // The request is not cut: it answers once its handler returns.
    let reply = slow.await.unwrap().unwrap();
    assert_eq!(reply.status(), StatusCode::OK);
    assert_eq!(reply.text().await.unwrap(), "done");
}

#[tokio::test]
async fn start_api_server_fails_when_the_port_is_taken() {
    let test = TestApp::new(create_config([noop_flow("test-flow")])).await;
    let running = listen(&test.app, 0, ServerOptions::default()).await.unwrap();
    let port = running.addr.port();

    let error = listen(&test.app, port, ServerOptions::default())
        .await
        .err()
        .expect("the port is already bound");
    assert_eq!(error.to_string(), format!("[openshop] Failed to listen on port {port}"));

    // The first server is unaffected.
    let health = reqwest::get(format!("http://127.0.0.1:{port}/health")).await.unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    running.shutdown().await.unwrap();
    test.cleanup().await;
}

#[tokio::test]
async fn listen_serves_the_admin_and_its_script() {
    let test = TestApp::new(create_config([noop_flow("test-flow")])).await;
    let server = listen(&test.app, 0, ServerOptions::default()).await.unwrap();
    let base = format!("http://127.0.0.1:{}", server.addr.port());

    let script = reqwest::get(format!("{base}/__openshop/openshop.js")).await.unwrap();
    assert_eq!(script.status(), StatusCode::OK);
    assert!(script.text().await.unwrap().contains("X-OpenShop-Fetch"));

    // A page loaded in the admin without a token: the bounce page gives it one.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let page = client.get(format!("{base}/?shop={SHOP}")).send().await.unwrap();
    assert_eq!(page.status(), StatusCode::FOUND);
    assert!(
        page.headers()[LOCATION]
            .to_str()
            .unwrap()
            .starts_with("/auth/session-token?"),
        "{:?}",
        page.headers()
    );

    server.shutdown().await.unwrap();
    test.cleanup().await;
}
