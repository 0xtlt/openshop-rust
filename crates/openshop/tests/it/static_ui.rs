//! Serving the admin UI (shell, assets, launch gate) and the server lifecycle.

use std::borrow::Cow;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::common::{TestApp, create_config, install, noop_flow, two_shopify_apps};
use axum::Router;
use openshop::server::ServerOptions;
use openshop::server::{listen, serve};
use openshop::testing::{TestContext, TestOptions, sign_query, to_query_string};
use openshop::{OpenShopConfig, UiSource, Value, json};
use reqwest::header::{CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, HOST, HeaderMap, LOCATION, REFERER};
use reqwest::{Method, RequestBuilder, StatusCode};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use uuid::Uuid;

const SHOP: &str = "static-test.myshopify.com";
const SECRET: &str = "test-secret";
const ROBOTS: &str = "noindex, nofollow, noarchive, nosnippet";
const SHELL: &str = concat!(
    r#"<html><head><meta name="shopify-api-key" content="" />"#,
    r#"<script src="https://cdn.shopify.com/shopifycloud/app-bridge.js"></script>"#,
    "</head><body>spa-shell</body></html>",
);
/// The shell as the TypeScript build wrote it, with an empty `data-api-key` placeholder.
const SHELL_WITH_PLACEHOLDER: &str = concat!(
    r#"<html><head><meta name="shopify-api-key" content="" />"#,
    r#"<script data-api-key="" src="https://cdn.shopify.com/shopifycloud/app-bridge.js"></script>"#,
    "</head><body>spa-shell</body></html>",
);
const UNAUTHORIZED_UI: &str = "Open this app from Shopify admin";
const TOP_SECRET: &str = "top secret outside the bundle";

// ─── Helpers ─────────────────────────────────────────────────────────

/// A UI bundle in `<root>/ui` next to a file that must never be served. Removed on drop.
struct UiDir {
    root: PathBuf,
}

impl UiDir {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("openshop-static-test-{}", Uuid::new_v4()));
        let dir = Self { root };
        std::fs::create_dir_all(dir.ui().join("assets")).unwrap();
        std::fs::write(dir.secret(), TOP_SECRET).unwrap();
        dir.write("index.html", SHELL);
        dir.write("asset.txt", "plain asset");
        dir.write("assets/app-4f2a.js", "console.log('app')");
        dir.write("assets/app-4f2a.css", "body{}");
        dir.write("openshop-ui_bg.wasm", "\0asm");
        dir
    }

    fn ui(&self) -> PathBuf {
        self.root.join("ui")
    }

    fn secret(&self) -> PathBuf {
        self.root.join("secret.txt")
    }

    fn write(&self, relative: &str, contents: &str) {
        std::fs::write(self.ui().join(relative), contents).unwrap();
    }

    fn server(&self, dev: bool) -> ServerOptions {
        ServerOptions {
            ui: UiSource::Dir(self.ui()),
            dev,
        }
    }
}

impl Drop for UiDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

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

/// The UI behind the signed-launch gate.
async fn ui_context(ui: &UiDir) -> TestContext {
    context_with(static_config(), ui.server(false)).await
}

/// The path Shopify admin opens: launch parameters signed with the app
/// secret, with the `timestamp` of now.
fn signed_launch(path: &str, shop: &str, secret: &str) -> String {
    let query = sign_query(
        &[("host", "admin.shopify.com/store/static-test"), ("shop", shop)],
        secret,
    );
    format!("{path}?{}", to_query_string(&query))
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

/// Sends the request target verbatim: HTTP clients normalize `..` segments away.
async fn raw_get(port: u16, target: &str) -> (u16, String) {
    let request = format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    tokio::task::spawn_blocking(move || {
        use std::io::{Read, Write};
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let text = String::from_utf8_lossy(&response).into_owned();
        let status = text
            .split_whitespace()
            .nth(1)
            .and_then(|status| status.parse().ok())
            .unwrap_or_default();
        (status, text)
    })
    .await
    .unwrap()
}

/// The page a visitor without a shop receives: where the app opens from, and
/// nothing of the application.
fn assert_welcome_page(reply: &Reply, status: StatusCode) {
    assert_eq!(reply.status, status);
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
    // Polaris is its only script, and it has nothing of the admin UI.
    assert_eq!(reply.body.matches("<script").count(), 1, "{}", reply.body);
    assert!(
        reply
            .body
            .contains("<script src=\"https://cdn.shopify.com/shopifycloud/polaris.js\"></script>"),
        "{}",
        reply.body
    );
    assert!(!reply.body.contains("spa-shell"));
    assert!(!reply.body.contains("shopify-api-key"));
}

fn assert_unauthorized_ui(reply: &Reply) {
    assert_welcome_page(reply, StatusCode::UNAUTHORIZED);
}

fn assert_shell(reply: &Reply, api_key: &str) {
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(
        reply.content_type().to_lowercase().starts_with("text/html"),
        "{}",
        reply.content_type()
    );
    assert!(reply.body.contains("spa-shell"));
    assert!(
        reply
            .body
            .contains(&format!(r#"<meta name="shopify-api-key" content="{api_key}" />"#)),
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

// ─── Static files and SPA fallback ───────────────────────────────────

#[tokio::test]
async fn get_serves_file_from_the_ui_directory() {
    let ui = UiDir::new();
    let ctx = ui_context(&ui).await;

    let reply = get(&ctx, "/asset.txt").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.header("x-robots-tag"), Some(ROBOTS));
    assert!(
        reply.content_type().starts_with("text/plain"),
        "{}",
        reply.content_type()
    );
    assert!(reply.body.contains("plain asset"));

    let script = get(&ctx, "/assets/app-4f2a.js?v=2").await;
    assert_eq!(script.status, StatusCode::OK);
    assert!(
        script.content_type().contains("javascript"),
        "{}",
        script.content_type()
    );
    assert_eq!(script.body, "console.log('app')");

    let styles = get(&ctx, "/assets/app-4f2a.css").await;
    assert_eq!(styles.status, StatusCode::OK);
    assert!(
        styles.content_type().starts_with("text/css"),
        "{}",
        styles.content_type()
    );

    let wasm = get(&ctx, "/openshop-ui_bg.wasm").await;
    assert_eq!(wasm.status, StatusCode::OK);
    assert_eq!(wasm.content_type(), "application/wasm");
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn assets_carry_a_strong_etag_and_answer_304_to_a_client_that_has_them() {
    let ui = UiDir::new();
    let ctx = ui_context(&ui).await;

    let first = get(&ctx, "/asset.txt").await;
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(first.header("x-content-type-options"), Some("nosniff"));
    let etag = first.header("etag").expect("etag").to_string();
    assert!(
        etag.starts_with('"') && etag.ends_with('"') && !etag.starts_with("W/"),
        "{etag}"
    );
    assert_eq!(get(&ctx, "/asset.txt").await.header("etag"), Some(etag.as_str()));
    assert_ne!(
        get(&ctx, "/assets/app-4f2a.js").await.header("etag"),
        Some(etag.as_str())
    );

    let weak = format!("W/{etag}");
    for matching in [etag.clone(), weak, "*".to_string(), format!("\"other\", {etag}")] {
        let reply = fetch(request(&ctx, Method::GET, "/asset.txt").header("if-none-match", &matching)).await;
        assert_eq!(reply.status, StatusCode::NOT_MODIFIED, "{matching}");
        assert_eq!(reply.body, "", "{matching}");
        assert_eq!(reply.header("etag"), Some(etag.as_str()));
        assert_eq!(reply.header(CACHE_CONTROL), Some("no-cache"));
        assert_eq!(reply.header("x-content-type-options"), Some("nosniff"));
    }
    let other = fetch(request(&ctx, Method::GET, "/asset.txt").header("if-none-match", "\"other\"")).await;
    assert_eq!(other.status, StatusCode::OK);
    assert_eq!(other.body, "plain asset");

    // A file that changes on disk gets another ETag.
    ui.write("asset.txt", "changed asset");
    let changed = get(&ctx, "/asset.txt").await;
    assert_eq!(changed.body, "changed asset");
    assert_ne!(changed.header("etag"), Some(etag.as_str()));
    let revalidated = fetch(request(&ctx, Method::GET, "/asset.txt").header("if-none-match", &etag)).await;
    assert_eq!(revalidated.status, StatusCode::OK);

    // The shell and the answers that are not assets are not sniffed either.
    let shell = get(&ctx, &signed_launch("/", SHOP, SECRET)).await;
    assert_eq!(shell.status, StatusCode::OK);
    assert_eq!(shell.header("x-content-type-options"), Some("nosniff"));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_compressed_asset_has_a_weak_etag() {
    let ui = UiDir::new();
    ui.write("big.txt", &"openshop ".repeat(500));
    let ctx = ui_context(&ui).await;

    let plain = get(&ctx, "/big.txt").await;
    assert!(plain.header("content-encoding").is_none());
    let etag = plain.header("etag").expect("etag").to_string();

    let compressed = request(&ctx, Method::GET, "/big.txt")
        .header("accept-encoding", "gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(compressed.headers()["content-encoding"], "gzip");
    assert_eq!(compressed.headers()["etag"], format!("W/{etag}").as_str());
    assert_eq!(compressed.headers()["vary"], "accept-encoding");
    // The answer to a revalidation describes the same representation as the
    // answer it replaces, whichever ETag the client holds.
    assert_eq!(plain.header("vary"), Some("accept-encoding"));
    for held in [format!("W/{etag}"), etag.clone()] {
        let revalidated = request(&ctx, Method::GET, "/big.txt")
            .header("accept-encoding", "gzip")
            .header("if-none-match", &held)
            .send()
            .await
            .unwrap();
        assert_eq!(revalidated.status(), StatusCode::NOT_MODIFIED, "{held}");
        assert_eq!(revalidated.headers()["etag"], format!("W/{etag}").as_str(), "{held}");
        assert_eq!(revalidated.headers()["vary"], "accept-encoding", "{held}");
    }
    let plain_again = fetch(request(&ctx, Method::GET, "/big.txt").header("if-none-match", &etag)).await;
    assert_eq!(plain_again.status, StatusCode::NOT_MODIFIED);
    assert_eq!(plain_again.header("etag"), Some(etag.as_str()));
    assert_eq!(plain_again.header("vary"), Some("accept-encoding"));
    // A small asset is never compressed: nothing varies.
    assert!(get(&ctx, "/asset.txt").await.header("vary").is_none());
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_asset_compiled_into_the_application_is_served_with_an_etag() {
    let ui = UiSource::Assets(|path| match path {
        "index.html" => Some(Cow::Borrowed(SHELL.as_bytes())),
        "app-1a2b3c4d5e.js" => Some(Cow::Borrowed(b"console.log('compiled')".as_slice())),
        _ => None,
    });
    let ctx = context_with(static_config(), ServerOptions { ui, dev: false }).await;

    let reply = get(&ctx, "/app-1a2b3c4d5e.js").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body, "console.log('compiled')");
    assert_eq!(reply.header(CACHE_CONTROL), Some("public, max-age=31536000, immutable"));
    assert_eq!(reply.header("x-content-type-options"), Some("nosniff"));
    let etag = reply.header("etag").expect("etag").to_string();
    let again = fetch(request(&ctx, Method::GET, "/app-1a2b3c4d5e.js").header("if-none-match", &etag)).await;
    assert_eq!(again.status, StatusCode::NOT_MODIFIED);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_serves_files_whose_names_need_percent_encoding() {
    let ui = UiDir::new();
    ui.write("assets/My File.txt", "spaced asset");
    ui.write("assets/été.txt", "accented asset");
    let ctx = ui_context(&ui).await;

    let spaced = get(&ctx, "/assets/My%20File.txt").await;
    assert_eq!(spaced.status, StatusCode::OK);
    assert_eq!(spaced.body, "spaced asset");
    assert!(
        spaced.content_type().starts_with("text/plain"),
        "{}",
        spaced.content_type()
    );

    let accented = get(&ctx, "/assets/%C3%A9t%C3%A9.txt").await;
    assert_eq!(accented.status, StatusCode::OK);
    assert_eq!(accented.body, "accented asset");

    for missing in [
        "/assets/My%20Other.txt",
        "/assets/%ff%fe.txt",
        "/assets/broken%2.txt",
        "/assets/nul%00.txt",
    ] {
        assert_eq!(get(&ctx, missing).await.status, StatusCode::NOT_FOUND, "{missing}");
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_missing_static_asset_returns_404() {
    let ui = UiDir::new();
    let ctx = ui_context(&ui).await;

    for path in [
        "/favicon.ico",
        "/assets/missing.js",
        "/assets/app-4f2a.js/extra.js",
        "/assets.d",
    ] {
        let reply = get(&ctx, path).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(reply.body, "404 Not Found", "{path}");
        assert_eq!(reply.header("x-robots-tag"), Some(ROBOTS), "{path}");
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_robots_txt_disallows_all_crawlers() {
    let ui = UiDir::new();
    ui.write("robots.txt", "User-agent: *\nAllow: /\n");
    let ctx = ui_context(&ui).await;

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
async fn without_a_valid_shopify_launch_neither_the_app_shell_nor_the_spa_fallback_is_served() {
    let ui = UiDir::new();
    let ctx = ui_context(&ui).await;

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
async fn get_with_signed_shopify_launch_serves_index_html_for_installed_shops() {
    let ui = UiDir::new();
    let ctx = ui_context(&ui).await;

    let reply = get(&ctx, &signed_launch("/", SHOP, SECRET)).await;
    assert_shell(&reply, "test-app");
    assert_eq!(reply.header("x-robots-tag"), Some(ROBOTS));
    assert_eq!(reply.header(CACHE_CONTROL), Some("no-store"));
    assert_eq!(
        reply.header(CONTENT_SECURITY_POLICY),
        Some("frame-ancestors https://static-test.myshopify.com https://admin.shopify.com;")
    );

    for path in [
        "/runs/550e8400-e29b-41d4-a716-446655440000",
        "/admin/deep/route",
        "/flows/",
        "/index.html",
    ] {
        assert_shell(&get(&ctx, &signed_launch(path, SHOP, SECRET)).await, "test-app");
    }

    // The shop is normalized before the installation lookup and the frame policy.
    let reply = get(&ctx, &signed_launch("/", "Static-Test.MyShopify.com", SECRET)).await;
    assert_shell(&reply, "test-app");
    assert_eq!(
        reply.header(CONTENT_SECURITY_POLICY),
        Some("frame-ancestors https://static-test.myshopify.com https://admin.shopify.com;")
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_with_signed_shopify_launch_injects_the_matching_app_bridge_api_key() {
    let ui = UiDir::new();
    ui.write("index.html", SHELL_WITH_PLACEHOLDER);
    let ctx = TestContext::with_options(
        multi_app_config(),
        TestOptions {
            server: ui.server(false),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let shop = "static-client-b.myshopify.com";
    install(&ctx.db, "clientB", shop, Some("offline-token"), false).await;

    let reply = get(&ctx, &signed_launch("/", shop, "client-b-secret")).await;
    assert_shell(&reply, "client-b-key");
    assert!(!reply.body.contains("client-a-key"));
    assert_eq!(
        reply.header(CONTENT_SECURITY_POLICY),
        Some("frame-ancestors https://static-client-b.myshopify.com https://admin.shopify.com;")
    );

    // The shop installed app B only: a launch signed by app A starts its own OAuth flow.
    let reply = get(&ctx, &signed_launch("/", shop, "client-a-secret")).await;
    assert_eq!(reply.status, StatusCode::FOUND);
    assert_eq!(
        reply.header(LOCATION),
        Some("/auth?shop=static-client-b.myshopify.com&app=clientA")
    );

    // The env-based secret of single-app mode is not a valid signer here.
    assert_unauthorized_ui(&get(&ctx, &signed_launch("/", shop, SECRET)).await);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_with_signed_shopify_launch_redirects_uninstalled_shops_to_auth() {
    let ui = UiDir::new();
    let ctx = ui_context(&ui).await;

    let reply = get(&ctx, &signed_launch("/", "uninstalled.myshopify.com", SECRET)).await;
    assert_eq!(reply.status, StatusCode::FOUND);
    assert_eq!(reply.header(LOCATION), Some("/auth?shop=uninstalled.myshopify.com"));
    assert_eq!(reply.header("x-robots-tag"), Some(ROBOTS));
    assert!(!reply.body.contains("spa-shell"));

    let reply = get(&ctx, &signed_launch("/flows", "uninstalled.myshopify.com", SECRET)).await;
    assert_eq!(reply.status, StatusCode::FOUND);
    assert_eq!(reply.header(LOCATION), Some("/auth?shop=uninstalled.myshopify.com"));

    // An installation that was removed, or never received a token, does not count.
    sqlx::query("UPDATE installations SET uninstalled_at = now() WHERE shop = $1")
        .bind(SHOP)
        .execute(&ctx.db)
        .await
        .unwrap();
    let reply = get(&ctx, &signed_launch("/", SHOP, SECRET)).await;
    assert_eq!(reply.status, StatusCode::FOUND);
    assert_eq!(reply.header(LOCATION), Some("/auth?shop=static-test.myshopify.com"));

    sqlx::query("INSERT INTO installations (shop, nonce) VALUES ('pending.myshopify.com', 'nonce')")
        .execute(&ctx.db)
        .await
        .unwrap();
    let reply = get(&ctx, &signed_launch("/", "pending.myshopify.com", SECRET)).await;
    assert_eq!(reply.status, StatusCode::FOUND);
    assert_eq!(reply.header(LOCATION), Some("/auth?shop=pending.myshopify.com"));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn signed_launch_rejects_tampered_or_foreign_signatures() {
    let ui = UiDir::new();
    let ctx = ui_context(&ui).await;
    let valid = signed_launch("/", SHOP, SECRET);
    assert_shell(&get(&ctx, &valid).await, "test-app");

    let signed_without_shop = to_query_string(&sign_query(&[], SECRET));
    let hmac = valid.rsplit("hmac=").next().unwrap();
    let timestamp = valid.split("timestamp=").nth(1).unwrap().split('&').next().unwrap();
    let later = (timestamp.parse::<i64>().unwrap() + 1).to_string();
    let stale = to_query_string(&sign_query(
        &[
            ("shop", SHOP),
            ("timestamp", &(timestamp.parse::<i64>().unwrap() - 120).to_string()),
        ],
        SECRET,
    ));
    let cases = [
        // Signed by someone else.
        signed_launch("/", SHOP, "another-secret"),
        // Parameters changed after signing.
        valid.replace(&format!("timestamp={timestamp}"), &format!("timestamp={later}")),
        valid.replace(SHOP, "uninstalled.myshopify.com"),
        format!("{valid}&extra=1"),
        // Missing or mangled signature.
        format!("/?shop={SHOP}"),
        format!("/?shop={SHOP}&hmac="),
        format!("/?shop={SHOP}&hmac={}", &hmac[..hmac.len() - 1]),
        // Signed, but more than 90 seconds ago: replayed.
        format!("/?{stale}"),
        // Correctly signed, but not for a shop.
        format!("/?{signed_without_shop}"),
        signed_launch("/", "evil.example.com", SECRET),
        signed_launch("/", "static-test.myshopify.com.evil.example.com", SECRET),
    ];
    for path in cases {
        assert_unauthorized_ui(&get(&ctx, &path).await);
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn shell_and_assets_only_answer_get_and_head() {
    let ui = UiDir::new();
    let ctx = ui_context(&ui).await;
    let launch = signed_launch("/", SHOP, SECRET);

    let head = fetch(request(&ctx, Method::HEAD, &launch)).await;
    assert_eq!(head.status, StatusCode::OK);
    assert!(head.body.is_empty());
    assert_eq!(head.header(CACHE_CONTROL), Some("no-store"));
    assert!(head.header(CONTENT_SECURITY_POLICY).is_some());

    for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
        let reply = fetch(request(&ctx, method.clone(), &launch)).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{method}");
        assert!(!reply.body.contains("spa-shell"));

        // The launch gate comes first, whatever the method.
        assert_unauthorized_ui(&fetch(request(&ctx, method.clone(), "/")).await);

        let asset = fetch(request(&ctx, method.clone(), "/asset.txt")).await;
        assert_eq!(asset.status, StatusCode::NOT_FOUND, "{method}");
        assert!(!asset.body.contains("plain asset"));
    }

    let head = fetch(request(&ctx, Method::HEAD, "/asset.txt")).await;
    assert_eq!(head.status, StatusCode::OK);
    assert!(head.body.is_empty());
    assert_eq!(
        fetch(request(&ctx, Method::HEAD, "/missing.txt")).await.status,
        StatusCode::NOT_FOUND
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn static_assets_cannot_escape_the_ui_directory() {
    let ui = UiDir::new();
    let absolute = ui.secret().to_string_lossy().replace(' ', "%20");
    let targets = [
        "/../secret.txt".to_string(),
        "/assets/../../secret.txt".to_string(),
        "/assets/../asset.txt".to_string(),
        "/./../secret.txt".to_string(),
        "/%2e%2e/secret.txt".to_string(),
        "/.%2e/secret.txt".to_string(),
        "/%2E%2E/secret.txt".to_string(),
        "/..%2fsecret.txt".to_string(),
        "/..%2Fsecret.txt".to_string(),
        "/%2e%2e%2fsecret.txt".to_string(),
        "/assets/..%2f..%2fsecret.txt".to_string(),
        "/assets/%2e%2e/%2e%2e/secret.txt".to_string(),
        "/..%5csecret.txt".to_string(),
        "/assets/..%5c..%5csecret.txt".to_string(),
        "/%252e%252e/secret.txt".to_string(),
        absolute.clone(),
        format!("/{absolute}"),
        format!("/%2f{}", absolute.trim_start_matches('/')),
        format!("/assets/%2f{}", absolute.trim_start_matches('/')),
    ];

    for dev in [false, true] {
        let ctx = context_with(static_config(), ui.server(dev)).await;

        let (status, response) = raw_get(ctx.port, "/asset.txt").await;
        assert_eq!(status, 200);
        assert!(response.ends_with("plain asset"), "{response}");

        for target in &targets {
            let (status, response) = raw_get(ctx.port, target).await;
            assert_eq!(status, 404, "dev={dev} {target}: {response}");
            assert!(!response.contains(TOP_SECRET), "dev={dev} {target}");
            assert!(!response.contains("plain asset"), "dev={dev} {target}");
        }
        ctx.shutdown().await.unwrap();
    }
}

// ─── Development mode ────────────────────────────────────────────────

#[tokio::test]
async fn dev_mode_serves_the_shell_only_to_shopify_looking_requests() {
    let ui = UiDir::new();
    let ctx = context_with(static_config(), ui.server(true)).await;
    let dev_policy = "frame-ancestors https://*.myshopify.com https://admin.shopify.com http://localhost:*;";
    // The development server is usually reached through a public tunnel.
    let tunnel = |method: Method, path: &str| request(&ctx, method, path).header(HOST, "openshop-dev.example.dev");

    for path in ["/", "/flows", "/runs/550e8400-e29b-41d4-a716-446655440000"] {
        // A visitor of the tunnel is told where the app opens from, like one of production.
        assert_welcome_page(&fetch(tunnel(Method::GET, path)).await, StatusCode::FORBIDDEN);
    }

    // No signature and no installation are required: a shop parameter is enough.
    let unsigned = fetch(tunnel(Method::GET, "/flows?shop=not-installed.myshopify.com")).await;
    assert_shell(&unsigned, "test-app");
    assert_eq!(unsigned.header(CONTENT_SECURITY_POLICY), Some(dev_policy));
    assert_eq!(unsigned.header(CACHE_CONTROL), Some("no-store"));
    assert_eq!(unsigned.header("x-robots-tag"), Some(ROBOTS));

    let allowed = [
        tunnel(Method::GET, "/").header("sec-fetch-dest", "iframe"),
        tunnel(Method::GET, "/").header(REFERER, "https://admin.shopify.com/store/static-test"),
        tunnel(Method::GET, "/").header(REFERER, "https://static-test.myshopify.com/admin"),
        tunnel(Method::GET, &signed_launch("/", SHOP, SECRET)),
        // A local visitor is the developer.
        request(&ctx, Method::GET, "/").header(HOST, format!("localhost:{}", ctx.port)),
    ];
    for candidate in allowed {
        assert_shell(&fetch(candidate).await, "test-app");
    }

    let denied = [
        tunnel(Method::GET, "/").header("sec-fetch-dest", "document"),
        tunnel(Method::GET, "/").header(REFERER, "https://example.com/"),
        tunnel(Method::POST, "/"),
    ];
    for candidate in denied {
        assert_eq!(fetch(candidate).await.status, StatusCode::FORBIDDEN);
    }

    // Assets are public, and only GET and HEAD reach the shell.
    assert_eq!(fetch(tunnel(Method::GET, "/asset.txt")).await.body, "plain asset");
    let post = fetch(tunnel(Method::POST, "/?shop=static-test.myshopify.com")).await;
    assert_eq!(post.status, StatusCode::NOT_FOUND);
    assert!(!post.body.contains("spa-shell"));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn dev_mode_injects_the_api_key_of_the_signing_app() {
    let ui = UiDir::new();
    let ctx = TestContext::with_options(
        multi_app_config(),
        TestOptions {
            server: ui.server(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let signed = get(&ctx, &signed_launch("/", SHOP, "client-b-secret")).await;
    assert_shell(&signed, "client-b-key");

    // Without a valid signature the first configured app is assumed.
    let unsigned = get(&ctx, &format!("/?shop={SHOP}")).await;
    assert_shell(&unsigned, "client-a-key");
    ctx.shutdown().await.unwrap();
}

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
async fn dev_mode_reloads_the_page_when_the_bundle_is_rebuilt_and_lets_the_server_close() {
    let ui = UiDir::new();
    let ctx = context_with(static_config(), ui.server(true)).await;
    let local = |path: &str| request(&ctx, Method::GET, path).header(HOST, format!("localhost:{}", ctx.port));
    let listens_for = |version: &str| format!(r#"if (event.data !== "{version}") location.reload();"#);

    let mut events = local("/__openshop/reload").send().await.expect("event stream");
    assert_eq!(events.status(), StatusCode::OK);
    let content_type = events.headers()[CONTENT_TYPE].to_str().unwrap().to_string();
    assert!(content_type.starts_with("text/event-stream"), "{content_type}");

    // The shell is served with the version being served, and listens for another one.
    let version = told(&mut events).await.expect("the version being served");
    let shell = fetch(local("/")).await;
    assert_shell(&shell, "test-app");
    assert!(
        shell.body.contains(r#"new EventSource("/__openshop/reload")"#),
        "{}",
        shell.body
    );
    assert!(shell.body.contains(&listens_for(&version)), "{}", shell.body);

    // A build writes another bundle: the page is told, and the shell is another version.
    ui.write("index.html", &SHELL.replace("spa-shell", "spa-shell rebuilt"));
    let rebuilt = told(&mut events).await.expect("the version of the new build");
    assert_ne!(rebuilt, version);
    assert!(fetch(local("/")).await.body.contains(&listens_for(&rebuilt)));

    // A server that closes does not wait for the pages that listen to it.
    let closing = std::time::Instant::now();
    ctx.shutdown().await.unwrap();
    assert!(closing.elapsed() < Duration::from_secs(4), "{:?}", closing.elapsed());
    assert_eq!(told(&mut events).await, None);
}

#[tokio::test]
async fn only_dev_mode_reloads_pages() {
    let ui = UiDir::new();
    let ctx = ui_context(&ui).await;

    let shell = get(&ctx, &signed_launch("/", SHOP, SECRET)).await;
    assert_shell(&shell, "test-app");
    assert!(!shell.body.contains("EventSource"), "{}", shell.body);
    // The event stream is a path like another: a page of the admin UI, behind its gate.
    assert_unauthorized_ui(&get(&ctx, "/__openshop/reload").await);
    ctx.shutdown().await.unwrap();
}

// ─── Routing around the UI ───────────────────────────────────────────

#[tokio::test]
async fn get_mcp_is_a_ui_page_while_post_mcp_is_the_protocol_endpoint() {
    let ui = UiDir::new();
    let ctx = ui_context(&ui).await;

    for path in ["/mcp", "/mcp/"] {
        assert_unauthorized_ui(&get(&ctx, path).await);
        assert_shell(&get(&ctx, &signed_launch(path, SHOP, SECRET)).await, "test-app");

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
        let signed = fetch(request(&ctx, Method::POST, &signed_launch(path, SHOP, SECRET))).await;
        assert_eq!(signed.status, StatusCode::UNAUTHORIZED, "{path}");
        assert!(!signed.body.contains("spa-shell"));
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn reserved_paths_are_never_served_the_shell() {
    let ui = UiDir::new();
    let ctx = ui_context(&ui).await;

    // The Admin API keeps its own authentication, signed launch or not.
    for path in ["/api", "/api/flows", "/api/unknown"] {
        let reply = get(&ctx, &signed_launch(path, SHOP, SECRET)).await;
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
    ] {
        for target in [path.to_string(), signed_launch(path, SHOP, SECRET)] {
            let reply = get(&ctx, &target).await;
            assert_eq!(reply.status, StatusCode::NOT_FOUND, "{target}");
            assert_eq!(reply.body, "404 Not Found", "{target}");
        }
    }

    let health = get(&ctx, &signed_launch("/health", SHOP, SECRET)).await;
    assert_eq!(health.status, StatusCode::OK);
    assert_eq!(health.json()["status"], "ok");
    ctx.shutdown().await.unwrap();
}

#[openshop::test(context_with(static_config(), ServerOptions::default()))]
async fn server_without_ui_returns_404_for_pages_and_assets(ctx: &TestContext) {
    for path in [
        "/".to_string(),
        "/flows".to_string(),
        "/mcp".to_string(),
        "/index.html".to_string(),
        "/asset.txt".to_string(),
        signed_launch("/", SHOP, SECRET),
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

#[cfg(feature = "embedded-ui")]
#[openshop::test(context_with(static_config(), ServerOptions { ui: UiSource::Embedded, dev: false }))]
async fn embedded_ui_serves_the_bundled_shell_and_assets(ctx: &TestContext) {
    assert_unauthorized_ui(&get(ctx, "/").await);

    let shell = get(ctx, &signed_launch("/", SHOP, SECRET)).await;
    assert_eq!(shell.status, StatusCode::OK);
    assert!(shell.body.contains("<title>OpenShop</title>"), "{}", shell.body);
    assert!(
        shell
            .body
            .contains(r#"<meta name="shopify-api-key" content="test-app" />"#)
    );
    assert!(
        shell
            .body
            .contains(r#"data-api-key="test-app" src="https://cdn.shopify.com/shopifycloud/app-bridge.js""#)
    );
    assert_eq!(shell.header(CACHE_CONTROL), Some("no-store"));

    // The bundle referenced by the shell is served without a launch signature.
    let start = shell.body.find("/openshop-ui-").expect("bundle reference");
    let length = shell.body[start..].find(".js").expect("bundle extension") + ".js".len();
    let bundle = get(ctx, &shell.body[start..start + length]).await;
    assert_eq!(bundle.status, StatusCode::OK);
    assert!(
        bundle.content_type().contains("javascript"),
        "{}",
        bundle.content_type()
    );
    assert!(!bundle.body.is_empty());

    assert_eq!(get(ctx, "/missing.js").await.status, StatusCode::NOT_FOUND);
}

// ─── Health and crawler headers ──────────────────────────────────────

#[tokio::test]
async fn get_health_returns_ok_with_a_timestamp() {
    let ui = UiDir::new();
    for server in [ServerOptions::default(), ui.server(false), ui.server(true)] {
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
    let ui = UiDir::new();
    let ctx = ui_context(&ui).await;

    let requests = [
        (request(&ctx, Method::GET, "/health"), StatusCode::OK),
        (request(&ctx, Method::GET, "/robots.txt"), StatusCode::OK),
        (request(&ctx, Method::GET, "/asset.txt"), StatusCode::OK),
        (request(&ctx, Method::GET, "/missing.png"), StatusCode::NOT_FOUND),
        (request(&ctx, Method::GET, "/"), StatusCode::UNAUTHORIZED),
        (
            request(&ctx, Method::GET, &signed_launch("/", SHOP, SECRET)),
            StatusCode::OK,
        ),
        (
            request(
                &ctx,
                Method::GET,
                &signed_launch("/", "uninstalled.myshopify.com", SECRET),
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
async fn start_api_server_serves_the_configured_ui() {
    let ui = UiDir::new();
    let test = TestApp::new(create_config([noop_flow("test-flow")])).await;
    let server = listen(&test.app, 0, ui.server(true)).await.unwrap();
    let base = format!("http://127.0.0.1:{}", server.addr.port());

    let asset = reqwest::get(format!("{base}/asset.txt")).await.unwrap();
    assert_eq!(asset.status(), StatusCode::OK);
    assert_eq!(asset.text().await.unwrap(), "plain asset");

    let shell = reqwest::get(format!("{base}/?shop={SHOP}")).await.unwrap();
    assert_eq!(shell.status(), StatusCode::OK);
    assert!(shell.text().await.unwrap().contains("spa-shell"));

    server.shutdown().await.unwrap();
    test.cleanup().await;
}
