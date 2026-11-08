//! `LSF_URL`: with local-storefront, a local fake of Shopify, the Admin API,
//! OAuth and the token exchange go to it, and the admin UI loads its App
//! Bridge and Polaris from it. Unset, nothing changes: the other modules check
//! the exact addresses and pages of Shopify.

use crate::common::{noop_flow, test_env};
use chrono::Utc;
use openshop::runtime::RuntimeEnv;
use openshop::server::ServerOptions;
use openshop::testing::{
    MockTransport, OutboundRequest, TestContext, TestDb, TestOptions, sign_query, to_query_string,
};
use openshop::{OpenShop, OpenShopConfig, UiSource, json};
use reqwest::StatusCode;
use reqwest::header::{CONTENT_SECURITY_POLICY, LOCATION};
use uuid::Uuid;

/// lsf's address as a developer writes it: the trailing `/` goes.
const LSF_URL: &str = "http://localhost:9292/";
const LSF: &str = "http://localhost:9292";
const SECRET: &str = "test-secret";
const LSF_APP_BRIDGE: &str = r#"<script src="http://localhost:9292/__lsf/admin/app-bridge.js"></script>"#;
const LSF_POLARIS: &str = r#"<script src="http://localhost:9292/__lsf/cdn/shopifycloud/polaris.js"></script>"#;
const SHELL: &str = concat!(
    r#"<html><head><meta name="shopify-api-key" content="" />"#,
    r#"<script src="https://cdn.shopify.com/shopifycloud/app-bridge.js"></script>"#,
    r#"<script src="https://cdn.shopify.com/shopifycloud/polaris.js"></script>"#,
    "</head><body>spa-shell</body></html>",
);

fn config() -> OpenShopConfig {
    OpenShopConfig::builder().flow(noop_flow("lsf-flow")).build().unwrap()
}

async fn with_lsf(options: TestOptions) -> TestContext {
    TestContext::with_options(
        config(),
        TestOptions {
            lsf_url: Some(LSF_URL.to_string()),
            ..options
        },
    )
    .await
    .expect("test context")
}

/// Answers like lsf: a token at the token endpoint, a shop at the Admin API.
fn lsf_like(transport: &MockTransport) {
    transport.respond_with(|request: &OutboundRequest| {
        Ok(if request.url.ends_with("/admin/oauth/access_token") {
            MockTransport::json(200, json!({ "access_token": "shpat_lsf", "scope": "read_products" }))
        } else {
            MockTransport::json(200, json!({ "data": { "shop": { "name": "Local Supply Co" } } }))
        })
    });
}

#[tokio::test]
async fn the_token_exchange_and_the_admin_api_go_to_local_storefront() {
    let ctx = with_lsf(TestOptions::default()).await;
    assert_eq!(ctx.app.lsf_origin().map(|origin| origin.as_str()), Some(LSF));
    lsf_like(&ctx.transport);

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
    assert_eq!(requests[0].url, format!("{LSF}/admin/oauth/access_token"));
    assert_eq!(
        requests[0].body["grant_type"],
        "urn:ietf:params:oauth:grant-type:token-exchange"
    );
    assert!(
        requests[1].url.starts_with(&format!("{LSF}/admin/api/")) && requests[1].url.ends_with("/graphql.json"),
        "{}",
        requests[1].url
    );
    assert_eq!(requests[1].header("x-shopify-access-token"), Some("shpat_lsf"));
    // The shop is still the shop: lsf serves every shop on one origin.
    for request in &requests {
        assert_eq!(request.header("x-forwarded-host"), Some(ctx.shop.as_str()));
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn oauth_goes_through_local_storefront_and_back_to_its_admin() {
    let ctx = with_lsf(TestOptions::default()).await;
    lsf_like(&ctx.transport);
    let shop = "lsf-oauth.myshopify.com";

    let started = ctx
        .http
        .get(format!("{}/auth?shop={shop}", ctx.url))
        .send()
        .await
        .unwrap();
    assert_eq!(started.status(), StatusCode::FOUND);
    let authorize = started.headers()[LOCATION].to_str().unwrap().to_string();
    assert!(
        authorize.starts_with(&format!("{LSF}/admin/oauth/authorize?client_id=test-app&")),
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

    // lsf approves, and its admin signs the callback as Shopify's does.
    let timestamp = Utc::now().timestamp().to_string();
    let callback = sign_query(
        &[
            ("shop", shop),
            ("code", "lsf-code"),
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
        format!("{LSF}/admin/apps/test-app?shop={shop}")
    );
    let exchange = ctx.transport.last_request().unwrap();
    assert_eq!(exchange.url, format!("{LSF}/admin/oauth/access_token"));
    assert_eq!(exchange.body["code"], "lsf-code");
    assert_eq!(exchange.header("x-forwarded-host"), Some(shop));
    ctx.shutdown().await.unwrap();
}

/// A bundle whose shell loads App Bridge and Polaris from Shopify's CDN, as
/// the one the framework embeds does. Removed on drop.
struct Bundle(std::path::PathBuf);

impl Bundle {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("openshop-lsf-ui-{}", Uuid::new_v4()));
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

fn assert_lsf_shell(body: &str) {
    assert!(
        body.contains(&format!(
            r#"{LSF_APP_BRIDGE}<script data-api-key="test-app" src="https://cdn.shopify.com/shopifycloud/app-bridge.js"></script>"#
        )),
        "{body}"
    );
    assert!(body.contains(LSF_POLARIS), "{body}");
    assert!(
        !body.contains("https://cdn.shopify.com/shopifycloud/polaris.js"),
        "{body}"
    );
    assert!(body.contains("spa-shell"));
}

#[tokio::test]
async fn a_launch_from_the_admin_of_local_storefront_loads_its_app_bridge_and_may_be_framed_by_it() {
    let bundle = Bundle::new();
    let ctx = with_lsf(TestOptions {
        access_token: Some("shpat_lsf".to_string()),
        server: bundle.server(false),
        ..Default::default()
    })
    .await;

    let launch = sign_query(&[("host", "localhost:9292/admin"), ("shop", ctx.shop.as_str())], SECRET);
    let (status, policy, body) = page(&ctx, &format!("/?{}", to_query_string(&launch))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_lsf_shell(&body);
    assert_eq!(
        policy.as_deref(),
        Some("frame-ancestors https://test.myshopify.com https://admin.shopify.com http://localhost:9292;")
    );

    // The page of a visitor who did not come from an admin: Polaris through
    // lsf, and lsf allowed where Polaris loads from.
    let (status, policy, body) = page(&ctx, "/").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains(LSF_POLARIS), "{body}");
    assert_eq!(body.matches("<script").count(), 1, "{body}");
    assert_eq!(
        policy.as_deref(),
        Some(
            "default-src 'none'; script-src http://localhost:9292 https://cdn.shopify.com/shopifycloud/polaris.js; \
             style-src 'unsafe-inline' http://localhost:9292 https://cdn.shopify.com/static/fonts/; \
             font-src http://localhost:9292 https://cdn.shopify.com/static/fonts/; img-src http://localhost:9292; \
             connect-src http://localhost:9292 https://cdn.shopify.com/shopifycloud/; form-action 'none'; base-uri 'none'"
        )
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn in_development_the_shell_loads_the_app_bridge_of_local_storefront() {
    let bundle = Bundle::new();
    let ctx = with_lsf(TestOptions {
        server: bundle.server(true),
        ..Default::default()
    })
    .await;

    let (status, policy, body) = page(&ctx, "/?shop=test.myshopify.com").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_lsf_shell(&body);
    assert_eq!(
        policy.as_deref(),
        Some(
            "frame-ancestors https://*.myshopify.com https://admin.shopify.com http://localhost:* http://localhost:9292;"
        )
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_lsf_url_that_is_not_an_origin_of_this_machine_or_https_stops_the_application() {
    let db = TestDb::create().await.unwrap();
    for lsf_url in [
        "http://lsf.example.com",
        "http://localhost:9292/admin",
        "localhost:9292",
    ] {
        let env = RuntimeEnv {
            lsf_url: Some(lsf_url.to_string()),
            ..test_env()
        };
        let error = OpenShop::with_transport(config(), db.pool.clone(), env, MockTransport::new())
            .err()
            .unwrap_or_else(|| panic!("{lsf_url} was taken"));
        assert!(
            error
                .to_string()
                .starts_with("[openshop] LSF_URL must be the origin of local-storefront"),
            "{lsf_url}: {error}"
        );
    }
    db.cleanup().await.unwrap();
}
