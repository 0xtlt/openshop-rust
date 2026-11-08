//! The Shopify endpoints: with an admin origin, the Admin API, OAuth and the
//! token exchange go there, and the admin UI loads the App Bridge and the
//! Polaris it is given. Set in `app!`, at startup on the builder, in a test's
//! options or by environment variables, field by field. Unset, nothing
//! changes: the other modules check the exact addresses and pages of Shopify.

use crate::common::{noop_flow, test_env};
use chrono::Utc;
use openshop::runtime::RuntimeEnv;
use openshop::server::ServerOptions;
use openshop::testing::{
    MockTransport, OutboundRequest, TestContext, TestDb, TestOptions, sign_query, to_query_string,
};
use openshop::{App, OpenShop, OpenShopConfig, ShopifyEndpoints, UiSource, app, json};
use reqwest::StatusCode;
use reqwest::header::{CONTENT_SECURITY_POLICY, LOCATION};
use uuid::Uuid;

/// The origin as a developer writes it: the trailing `/` goes.
const ORIGIN_AS_WRITTEN: &str = "http://localhost:9393/";
const ORIGIN: &str = "http://localhost:9393";
const APP_BRIDGE: &str = "http://localhost:9393/admin/app-bridge.js";
const POLARIS: &str = "http://localhost:9393/cdn/shopifycloud/polaris.js";
const SECRET: &str = "test-secret";
const SHELL: &str = concat!(
    r#"<html><head><meta name="shopify-api-key" content="" />"#,
    r#"<script src="https://cdn.shopify.com/shopifycloud/app-bridge.js"></script>"#,
    r#"<script src="https://cdn.shopify.com/shopifycloud/polaris.js"></script>"#,
    "</head><body>spa-shell</body></html>",
);

fn config() -> OpenShopConfig {
    OpenShopConfig::builder()
        .flow(noop_flow("endpoints-flow"))
        .build()
        .unwrap()
}

fn simulator() -> ShopifyEndpoints {
    ShopifyEndpoints::new()
        .admin_origin(ORIGIN_AS_WRITTEN)
        .app_bridge(APP_BRIDGE)
        .polaris(POLARIS)
}

async fn with_simulator(options: TestOptions) -> TestContext {
    TestContext::with_options(
        config(),
        TestOptions {
            shopify_endpoints: simulator(),
            ..options
        },
    )
    .await
    .expect("test context")
}

/// Answers like a simulator of Shopify: a token at the token endpoint, a
/// shop at the Admin API.
fn simulator_like(transport: &MockTransport) {
    transport.respond_with(|request: &OutboundRequest| {
        Ok(if request.url.ends_with("/admin/oauth/access_token") {
            MockTransport::json(200, json!({ "access_token": "shpat_sim", "scope": "read_products" }))
        } else {
            MockTransport::json(200, json!({ "data": { "shop": { "name": "Local Supply Co" } } }))
        })
    });
}

#[tokio::test]
async fn the_token_exchange_and_the_admin_api_go_to_the_admin_origin() {
    let ctx = with_simulator(TestOptions::default()).await;
    assert_eq!(
        ctx.app.shopify_endpoints(),
        &ShopifyEndpoints::new()
            .admin_origin(ORIGIN)
            .app_bridge(APP_BRIDGE)
            .polaris(POLARIS)
    );
    simulator_like(&ctx.transport);

    // A request of the admin UI, with a session token of a shop without a token.
    assert_eq!(ctx.get("/api/flows").send().await.unwrap().status(), StatusCode::OK);
    let data = ctx
        .app
        .shopify_client(&ctx.shop, "default")
        .graphql("{ shop { name } }", ())
        .await
        .unwrap();
    assert_eq!(data["shop"]["name"], "Local Supply Co");

    let requests = ctx.transport.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_eq!(requests[0].url, format!("{ORIGIN}/admin/oauth/access_token"));
    assert_eq!(
        requests[0].body["grant_type"],
        "urn:ietf:params:oauth:grant-type:token-exchange"
    );
    assert!(
        requests[1].url.starts_with(&format!("{ORIGIN}/admin/api/")) && requests[1].url.ends_with("/graphql.json"),
        "{}",
        requests[1].url
    );
    assert_eq!(requests[1].header("x-shopify-access-token"), Some("shpat_sim"));
    // The shop is still the shop: one origin may serve every shop.
    for request in &requests {
        assert_eq!(request.header("x-forwarded-host"), Some(ctx.shop.as_str()));
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn oauth_goes_through_the_admin_origin_and_back_to_its_admin() {
    let ctx = with_simulator(TestOptions::default()).await;
    simulator_like(&ctx.transport);
    let shop = "endpoints-oauth.myshopify.com";

    let started = ctx
        .http
        .get(format!("{}/auth?shop={shop}", ctx.url))
        .send()
        .await
        .unwrap();
    assert_eq!(started.status(), StatusCode::FOUND);
    let authorize = started.headers()[LOCATION].to_str().unwrap().to_string();
    assert!(
        authorize.starts_with(&format!("{ORIGIN}/admin/oauth/authorize?client_id=test-app&")),
        "{authorize}"
    );
    assert!(authorize.ends_with(&format!("&shop={shop}")), "{authorize}");
    let state = authorize.split("&state=").nth(1).unwrap().split('&').next().unwrap();
    let cookie = started.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();

    // The admin approves, and signs the callback as Shopify's does.
    let timestamp = Utc::now().timestamp().to_string();
    let callback = sign_query(
        &[
            ("shop", shop),
            ("code", "sim-code"),
            ("state", state),
            ("timestamp", &timestamp),
        ],
        SECRET,
    );
    let finished = ctx
        .http
        .get(format!("{}/auth/callback?{}", ctx.url, to_query_string(&callback)))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(finished.status(), StatusCode::FOUND);
    assert_eq!(
        finished.headers()[LOCATION].to_str().unwrap(),
        format!("{ORIGIN}/admin/apps/test-app?shop={shop}")
    );
    let exchange = ctx.transport.last_request().unwrap();
    assert_eq!(exchange.url, format!("{ORIGIN}/admin/oauth/access_token"));
    assert_eq!(exchange.body["code"], "sim-code");
    assert_eq!(exchange.header("x-forwarded-host"), Some(shop));
    ctx.shutdown().await.unwrap();
}

/// A bundle whose shell loads App Bridge and Polaris from Shopify's CDN, as
/// the one the framework embeds does. Removed on drop.
struct Bundle(std::path::PathBuf);

impl Bundle {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("openshop-endpoints-ui-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), SHELL).unwrap();
        Self(dir)
    }

    fn server(&self, dev: bool) -> ServerOptions {
        ServerOptions {
            ui: UiSource::Dir(self.0.clone()),
            dev,
        }
    }
}

impl Drop for Bundle {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn page(ctx: &TestContext, path: &str) -> (StatusCode, Option<String>, String) {
    let response = ctx.http.get(format!("{}{path}", ctx.url)).send().await.unwrap();
    let policy = response
        .headers()
        .get(CONTENT_SECURITY_POLICY)
        .map(|policy| policy.to_str().unwrap().to_string());
    (response.status(), policy, response.text().await.unwrap())
}

fn assert_simulator_shell(body: &str) {
    assert!(
        body.contains(&format!(
            r#"<script data-api-key="test-app" src="{APP_BRIDGE}"></script><script src="{POLARIS}"></script>"#
        )),
        "{body}"
    );
    assert!(
        body.contains(r#"<meta name="shopify-api-key" content="test-app" />"#),
        "{body}"
    );
    assert!(!body.contains("cdn.shopify.com"), "{body}");
    assert!(body.contains("spa-shell"));
}

#[tokio::test]
async fn a_launch_loads_the_scripts_of_the_endpoints_and_may_be_framed_by_the_admin_origin() {
    let bundle = Bundle::new();
    let ctx = with_simulator(TestOptions {
        access_token: Some("shpat_sim".to_string()),
        server: bundle.server(false),
        ..Default::default()
    })
    .await;

    let launch = sign_query(&[("host", "localhost:9393/admin"), ("shop", ctx.shop.as_str())], SECRET);
    let (status, policy, body) = page(&ctx, &format!("/?{}", to_query_string(&launch))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_simulator_shell(&body);
    assert_eq!(
        policy.as_deref(),
        Some("frame-ancestors https://test.myshopify.com https://admin.shopify.com http://localhost:9393;")
    );

    // The page of a visitor who did not come from an admin: its Polaris, and
    // the origin of its Polaris allowed where Polaris loads from.
    let (status, policy, body) = page(&ctx, "/").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        body.contains(&format!(r#"<script src="{POLARIS}"></script>"#)),
        "{body}"
    );
    assert_eq!(body.matches("<script").count(), 1, "{body}");
    assert_eq!(
        policy.as_deref(),
        Some(
            "default-src 'none'; script-src https://cdn.shopify.com/shopifycloud/polaris.js http://localhost:9393; \
             style-src 'unsafe-inline' https://cdn.shopify.com/static/fonts/ http://localhost:9393; \
             font-src https://cdn.shopify.com/static/fonts/ http://localhost:9393; \
             connect-src https://cdn.shopify.com/shopifycloud/ http://localhost:9393; form-action 'none'; base-uri 'none'"
        )
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn in_development_the_shell_loads_the_scripts_of_the_endpoints() {
    let bundle = Bundle::new();
    let ctx = with_simulator(TestOptions {
        server: bundle.server(true),
        ..Default::default()
    })
    .await;

    let (status, policy, body) = page(&ctx, "/?shop=test.myshopify.com").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_simulator_shell(&body);
    assert_eq!(
        policy.as_deref(),
        Some(
            "frame-ancestors https://*.myshopify.com https://admin.shopify.com http://localhost:* http://localhost:9393;"
        )
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_scripts_alone_leave_the_admin_api_and_the_frame_to_shopify() {
    let bundle = Bundle::new();
    let ctx = TestContext::with_options(
        config(),
        TestOptions {
            shopify_endpoints: ShopifyEndpoints::new().app_bridge(APP_BRIDGE),
            server: bundle.server(true),
            ..TestOptions::installed()
        },
    )
    .await
    .unwrap();
    let (status, policy, body) = page(&ctx, "/?shop=test.myshopify.com").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains(&format!(
            r#"<script data-api-key="test-app" src="{APP_BRIDGE}"></script>"#
        )),
        "{body}"
    );
    assert!(
        body.contains(r#"<script src="https://cdn.shopify.com/shopifycloud/polaris.js"></script>"#),
        "{body}"
    );
    assert_eq!(
        policy.as_deref(),
        Some("frame-ancestors https://*.myshopify.com https://admin.shopify.com http://localhost:*;")
    );

    simulator_like(&ctx.transport);
    ctx.app
        .shopify_client(&ctx.shop, "default")
        .graphql("{ shop { name } }", ())
        .await
        .unwrap();
    let request = ctx.transport.last_request().unwrap();
    assert!(
        request.url.starts_with("https://test.myshopify.com/admin/api/"),
        "{}",
        request.url
    );
    assert_eq!(request.header("x-forwarded-host"), None);
    ctx.shutdown().await.unwrap();
}

// ─── Where the endpoints come from ───────────────────────────────────

static DECLARED: App = app! {
    shopify_endpoints: ShopifyEndpoints::new()
        .admin_origin("https://shopify-proxy.example.com")
        .polaris("https://assets.example.com/polaris.js"),
};

#[test]
fn app_declares_them_without_making_a_multi_app_setup() {
    let config = DECLARED.config().unwrap();
    assert_eq!(
        config.shopify_endpoints,
        ShopifyEndpoints::new()
            .admin_origin("https://shopify-proxy.example.com")
            .polaris("https://assets.example.com/polaris.js")
    );
    // The Shopify app still comes from the environment.
    assert!(config.shopify.is_none());
}

#[test]
fn a_call_at_startup_wins_over_the_declaration_field_by_field() {
    let config = DECLARED
        .builder()
        .unwrap()
        .shopify_endpoints(ShopifyEndpoints::new().admin_origin(ORIGIN).app_bridge(APP_BRIDGE))
        .build()
        .unwrap();
    assert_eq!(
        config.shopify_endpoints,
        ShopifyEndpoints::new()
            .admin_origin(ORIGIN)
            .app_bridge(APP_BRIDGE)
            .polaris("https://assets.example.com/polaris.js")
    );
}

#[tokio::test]
async fn the_environment_sets_only_what_the_code_does_not() {
    let db = TestDb::create().await.unwrap();
    let config = DECLARED.config().unwrap();
    let env = RuntimeEnv {
        shopify_endpoints: ShopifyEndpoints::new()
            .admin_origin("http://localhost:9999")
            .app_bridge(APP_BRIDGE),
        ..test_env()
    };
    let app = OpenShop::with_transport(config, db.pool.clone(), env, MockTransport::new()).unwrap();
    assert_eq!(
        app.shopify_endpoints(),
        &ShopifyEndpoints::new()
            .admin_origin("https://shopify-proxy.example.com")
            .app_bridge(APP_BRIDGE)
            .polaris("https://assets.example.com/polaris.js")
    );

    // A test's options win over the declaration, field by field, and the
    // environment is not read.
    let ctx = TestContext::with_options(
        DECLARED.config().unwrap(),
        TestOptions {
            shopify_endpoints: ShopifyEndpoints::new().polaris(POLARIS),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        ctx.app.shopify_endpoints(),
        &ShopifyEndpoints::new()
            .admin_origin("https://shopify-proxy.example.com")
            .polaris(POLARIS)
    );
    ctx.shutdown().await.unwrap();
    db.cleanup().await.unwrap();
}

#[tokio::test]
async fn an_endpoint_that_is_not_https_or_on_this_machine_stops_the_application() {
    // In the code: the configuration does not build.
    for (endpoints, setting) in [
        (
            ShopifyEndpoints::new().admin_origin("http://simulator.example.com"),
            "admin_origin",
        ),
        (
            ShopifyEndpoints::new().admin_origin("http://localhost:9393/admin"),
            "admin_origin",
        ),
        (
            ShopifyEndpoints::new().app_bridge("http://simulator.example.com/app-bridge.js"),
            "app_bridge",
        ),
        (ShopifyEndpoints::new().polaris("/polaris.js"), "polaris"),
    ] {
        let error = OpenShopConfig::builder()
            .shopify_endpoints(endpoints)
            .build()
            .err()
            .unwrap()
            .to_string();
        assert!(
            error.starts_with(&format!(
                "[openshop] Invalid config: shopify_endpoints.{setting} is not valid: "
            )),
            "{error}"
        );
    }

    // In the environment: the application does not start.
    let db = TestDb::create().await.unwrap();
    for (endpoints, variable) in [
        (
            ShopifyEndpoints::new().admin_origin("localhost:9393"),
            "SHOPIFY_ADMIN_ORIGIN",
        ),
        (
            ShopifyEndpoints::new().app_bridge("http://example.com/a.js"),
            "SHOPIFY_APP_BRIDGE_URL",
        ),
        (
            ShopifyEndpoints::new().polaris("https://user:pass@example.com/p.js"),
            "SHOPIFY_POLARIS_URL",
        ),
    ] {
        let env = RuntimeEnv {
            shopify_endpoints: endpoints,
            ..test_env()
        };
        let error = OpenShop::with_transport(config(), db.pool.clone(), env, MockTransport::new())
            .err()
            .unwrap_or_else(|| panic!("{variable} was taken"))
            .to_string();
        assert!(
            error.starts_with(&format!("[openshop] {variable} is not valid: ")),
            "{error}"
        );
        assert!(!error.contains("pass@"), "{error}");
    }
    db.cleanup().await.unwrap();
}
