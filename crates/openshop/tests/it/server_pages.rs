//! The admin rendered by the server (`UiSource::Server`): a `#[view]` that
//! returns `Markup`, its forms, the bounce page, and a page of the framework.

use std::sync::atomic::{AtomicBool, Ordering};

use maud::html;
use openshop::prelude::*;
use openshop::server::ServerOptions;
use openshop::testing::{TestContext, TestOptions};
use openshop::{AdminPublicError, Cron, UiSource};
use reqwest::header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, LOCATION};
use reqwest::{Method, RequestBuilder, StatusCode};

const SHOP: &str = "server-pages.myshopify.com";

/// The state the home page shows: one per test binary, which one test changes.
static HIDDEN: AtomicBool = AtomicBool::new(false);

#[input]
struct SetHidden {
    hidden: bool,
}

#[input]
struct Tab {
    tab: Option<String>,
}

/// What the home page shows, as shop-test's does. `Serialize`: the loader
/// also answers the RPC transport of the Leptos views.
#[derive(serde::Serialize)]
enum Home {
    Plan { hidden: bool },
}

struct HomePage;

#[admin_page("/")]
impl HomePage {
    #[loader]
    async fn home(_ctx: AdminContext) -> Result<Home> {
        Ok(Home::Plan {
            hidden: HIDDEN.load(Ordering::SeqCst),
        })
    }

    #[action(authorize = Self::staff)]
    async fn set_hidden(_ctx: AdminContext, input: SetHidden) -> Result<Outcome> {
        if input.hidden && HIDDEN.load(Ordering::SeqCst) {
            bail!(AdminPublicError::new("ALREADY_HIDDEN", "Already hidden").status(409));
        }
        HIDDEN.store(input.hidden, Ordering::SeqCst);
        Ok(Outcome::toast(if input.hidden {
            "PayPal Express hidden"
        } else {
            "PayPal Express shown"
        }))
    }

    #[action]
    async fn go_to_reviews(_ctx: AdminContext) -> Result<Outcome> {
        Ok(Outcome::redirect("/reviews/42").with_toast("Moved"))
    }

    async fn staff(ctx: AdminContext) -> Result<bool> {
        Ok(!ctx.actor.id.is_empty())
    }

    #[view]
    fn view(home: Home) -> Markup {
        let Home::Plan { hidden } = home;
        html! {
            s-page {
                s-banner tone=(if hidden { "success" } else { "warning" }) heading=(if hidden { "Hidden" } else { "Shown" }) {
                    form method="post" {
                        input type="hidden" name="_action" value=(Self::SET_HIDDEN);
                        input type="hidden" name="hidden" value=(!hidden);
                        s-button type="submit" variant="primary" { (if hidden { "Show" } else { "Hide" }) }
                    }
                }
            }
        }
    }
}

struct ReviewPage;

#[admin_page("/reviews/:id")]
impl ReviewPage {
    #[loader]
    async fn review(ctx: AdminContext, input: Tab) -> Result<String> {
        let id = ctx.params.get("id").cloned().unwrap_or_default();
        Ok(format!(
            "Review {id}, tab {}",
            input.tab.unwrap_or_else(|| "none".to_string())
        ))
    }

    #[view]
    fn view(review: String) -> Markup {
        html! { s-page heading=(review) {} }
    }
}

struct PrivatePage;

#[admin_page("/private", nav = "Private")]
impl PrivatePage {
    #[access]
    async fn nobody(_ctx: AdminContext) -> Result<bool> {
        Ok(false)
    }

    #[loader]
    async fn secret(_ctx: AdminContext) -> Result<String> {
        Ok("top secret".to_string())
    }

    #[view]
    fn view(secret: String) -> Markup {
        html! { s-page { (secret) } }
    }
}

struct SettingsPage;

#[admin_page("/settings", nav = "Settings")]
impl SettingsPage {
    #[loader]
    async fn settings(_ctx: AdminContext) -> Result<String> {
        Ok("settings".to_string())
    }

    #[view]
    fn view(settings: String) -> Markup {
        html! { s-page heading="Settings" { (settings) } }
    }
}

static APP: App = app! {
    admin_pages: [HomePage, ReviewPage, PrivatePage, SettingsPage],
};

async fn context() -> TestContext {
    let mut config = APP.config().unwrap();
    config.crons.push(
        Cron::new("*/5 * * * *", "syncOrders")
            .name("Quick sync")
            .input(json!({ "limit": 10 })),
    );
    let flow = openshop::builder::Flow::new("syncOrders", |_ctx: FlowContext| async move { Ok(()) });
    config.flows.insert(flow.name.clone(), flow);
    TestContext::with_options(
        config,
        TestOptions {
            shop: SHOP.to_string(),
            access_token: Some("offline-token".to_string()),
            server: ServerOptions {
                ui: UiSource::Server,
                dev: false,
            },
            ..Default::default()
        },
    )
    .await
    .expect("test context")
}

struct Reply {
    status: StatusCode,
    headers: reqwest::header::HeaderMap,
    body: String,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(|value| value.to_str().unwrap())
    }
}

async fn send(request: RequestBuilder) -> Reply {
    let response = request.send().await.expect("response");
    Reply {
        status: response.status(),
        headers: response.headers().clone(),
        body: response.text().await.expect("body"),
    }
}

fn request(ctx: &TestContext, method: Method, path: &str) -> RequestBuilder {
    ctx.http.request(method, ctx.url_for(path))
}

/// A request of `openshop.js`: App Bridge's `fetch` adds the session token.
fn fetch(ctx: &TestContext, method: Method, path: &str) -> RequestBuilder {
    request(ctx, method, path)
        .header(AUTHORIZATION, ctx.authorization_header())
        .header("x-openshop-fetch", "1")
}

fn form(request: RequestBuilder, body: impl Into<String>) -> RequestBuilder {
    request
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(body.into())
}

#[tokio::test]
async fn a_page_is_rendered_by_the_server() {
    let ctx = context().await;

    // The launch: the session token is in the URL.
    let page = send(request(
        &ctx,
        Method::GET,
        &format!("/?shop={SHOP}&id_token={}", ctx.session_token()),
    ))
    .await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert_eq!(page.header("cache-control"), Some("no-store"));
    assert!(
        page.header("content-security-policy")
            .unwrap()
            .starts_with("frame-ancestors ")
    );
    let body = &page.body;
    assert!(body.starts_with("<!DOCTYPE html>"), "{body}");
    // App Bridge first, with the key of the app, then Polaris and the script of the pages.
    let app_bridge = body
        .find(r#"src="https://cdn.shopify.com/shopifycloud/app-bridge.js""#)
        .unwrap();
    assert!(app_bridge < body.find("polaris.js").unwrap());
    assert!(body.contains(r#"<script src="/__openshop/openshop.js?v="#), "{body}");
    assert!(
        body.contains(r#"<meta name="shopify-api-key" content="test-app">"#),
        "{body}"
    );
    // The menu: the built-in pages that have something to show, then the pages of the app that let the staff member in.
    assert!(
        body.contains(r#"<ui-nav-menu><a href="/" rel="home">Home</a><a href="/flows">Flows</a><a href="/mcp">MCP</a><a href="/crons">Crons</a><a href="/settings">Settings</a></ui-nav-menu>"#),
        "{body}"
    );
    // The page, its form named after its action.
    // (The other tests of this file hide and show it: its state is not asserted here.)
    assert!(
        body.contains(r#"<main id="openshop-page"><s-page><s-banner tone="#),
        "{body}"
    );
    assert!(
        body.contains(r#"<input type="hidden" name="_action" value="setHidden">"#),
        "{body}"
    );

    // A link the script follows: the token is the header App Bridge's fetch adds.
    let review = send(fetch(&ctx, Method::GET, "/reviews/42?tab=notes")).await;
    assert_eq!(review.status, StatusCode::OK);
    assert!(
        review.body.contains(r#"<s-page heading="Review 42, tab notes">"#),
        "{}",
        review.body
    );
    // The input of a loader is read from the query, and checked.
    let unknown = send(fetch(&ctx, Method::GET, "/reviews/42")).await;
    assert!(unknown.body.contains("Review 42, tab none"), "{}", unknown.body);

    // A page whose access rule says no shows nothing of itself.
    let private = send(fetch(&ctx, Method::GET, "/private")).await;
    assert_eq!(private.status, StatusCode::NOT_FOUND);
    assert!(private.body.contains("This page is disabled."), "{}", private.body);
    assert!(!private.body.contains("top secret"));
    assert!(!private.body.contains(r#"href="/private""#));

    // A page of the framework that is not rendered by the server yet says so.
    let flows = send(fetch(&ctx, Method::GET, "/flows")).await;
    assert!(flows.body.contains("Not rendered by the server yet"), "{}", flows.body);
}

#[tokio::test]
async fn a_page_loaded_without_a_token_bounces() {
    let ctx = context().await;

    // In the frame of the admin: to the bounce page, which loads the URL again with a fresh token.
    let framed =
        send(request(&ctx, Method::GET, "/reviews/42?tab=notes&id_token=expired").header("sec-fetch-dest", "iframe"))
            .await;
    assert_eq!(framed.status, StatusCode::FOUND);
    assert_eq!(
        framed.header("location"),
        Some("/auth/session-token?tab=notes&shopify-reload=%2Freviews%2F42%3Ftab%3Dnotes")
    );
    let bounce = send(request(&ctx, Method::GET, framed.header("location").unwrap())).await;
    assert_eq!(bounce.status, StatusCode::OK);
    assert!(
        bounce
            .body
            .contains(r#"<script data-api-key="test-app" src="https://cdn.shopify.com/shopifycloud/app-bridge.js">"#),
        "{}",
        bounce.body
    );
    // Only to a page of the app.
    for elsewhere in ["%2F%2Fevil.example", "%2F%5Cevil.example", "https%3A%2F%2Fevil.example"] {
        let path = format!("/auth/session-token?shopify-reload={elsewhere}");
        assert_eq!(
            send(request(&ctx, Method::GET, &path)).await.status,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }

    // Outside Shopify admin: where the app opens from.
    let outside = send(request(&ctx, Method::GET, "/")).await;
    assert_eq!(outside.status, StatusCode::UNAUTHORIZED);
    assert!(outside.body.contains("Open this app from Shopify admin"));

    // The script's request: App Bridge asks again with a fresh token.
    let refused = send(request(&ctx, Method::GET, "/").header("x-openshop-fetch", "1")).await;
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
    assert_eq!(refused.header("x-shopify-retry-invalid-session-request"), Some("1"));

    // The script, cached for good under the URL of its version.
    let script = send(request(&ctx, Method::GET, "/__openshop/openshop.js")).await;
    assert_eq!(
        script.header(CONTENT_TYPE.as_str()),
        Some("text/javascript; charset=utf-8")
    );
    assert_eq!(
        script.header(CACHE_CONTROL.as_str()),
        Some("public, max-age=31536000, immutable")
    );
    assert!(script.body.contains("X-OpenShop-Toast"));
}

#[tokio::test]
async fn a_form_runs_its_action_and_the_page_is_rendered_again() {
    let ctx = context().await;

    // The script's request: the page again, with the toast the action asked for.
    let hidden = send(form(fetch(&ctx, Method::POST, "/"), "_action=setHidden&hidden=true")).await;
    assert_eq!(hidden.status, StatusCode::OK, "{}", hidden.body);
    assert_eq!(hidden.header("x-openshop-toast"), Some("PayPal%20Express%20hidden"));
    assert!(hidden.body.contains(r#"heading="Hidden""#), "{}", hidden.body);

    // A failure the action reports: its status and an error toast, the page stays.
    let again = send(form(fetch(&ctx, Method::POST, "/"), "_action=setHidden&hidden=true")).await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(again.header("x-openshop-toast-error"), Some("Already%20hidden"));
    assert!(again.body.is_empty());
    // An input the schema refuses, and an action the page does not have.
    let invalid = send(form(fetch(&ctx, Method::POST, "/"), "_action=setHidden&hidden=maybe")).await;
    assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY);
    let unknown = send(form(fetch(&ctx, Method::POST, "/"), "_action=dropTables")).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);

    // An action that goes elsewhere: the script follows `X-OpenShop-Location`.
    let moved = send(form(fetch(&ctx, Method::POST, "/"), "_action=goToReviews")).await;
    assert_eq!(moved.status, StatusCode::NO_CONTENT);
    assert_eq!(moved.header("x-openshop-location"), Some("/reviews/42"));
    assert_eq!(moved.header("x-openshop-toast"), Some("Moved"));

    // Without the script: a redirect to the page, whose URL is the state.
    let plain = send(form(
        request(&ctx, Method::POST, "/?shop=x").header(AUTHORIZATION, ctx.authorization_header()),
        "_action=setHidden&hidden=false",
    ))
    .await;
    assert_eq!(plain.status, StatusCode::SEE_OTHER);
    assert_eq!(plain.headers.get(LOCATION).unwrap(), "/?shop=x");
    let page = send(fetch(&ctx, Method::GET, "/")).await;
    assert!(page.body.contains(r#"heading="Shown""#), "{}", page.body);
    // A failure without the script: the page, with what went wrong above it.
    let plain_again = send(form(
        request(&ctx, Method::POST, "/").header(AUTHORIZATION, ctx.authorization_header()),
        "_action=setHidden&hidden=true",
    ))
    .await;
    assert_eq!(plain_again.status, StatusCode::SEE_OTHER);
    let refused = send(form(
        request(&ctx, Method::POST, "/").header(AUTHORIZATION, ctx.authorization_header()),
        "_action=setHidden&hidden=true",
    ))
    .await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert!(
        refused
            .body
            .contains(r#"<s-banner tone="critical">Already hidden</s-banner>"#),
        "{}",
        refused.body
    );
}

#[tokio::test]
async fn the_crons_page_is_rendered_by_the_server() {
    let ctx = context().await;

    let page = send(fetch(&ctx, Method::GET, "/crons")).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains(r#"<s-page heading="Crons">"#), "{}", page.body);
    assert!(page.body.contains("<strong>Quick sync</strong>"), "{}", page.body);
    assert!(
        page.body.contains(r#"<s-badge tone="success">Enabled</s-badge>"#),
        "{}",
        page.body
    );

    let key = openshop::Cron::new("*/5 * * * *", "syncOrders")
        .name("Quick sync")
        .input(json!({ "limit": 10 }))
        .key();
    let body = format!("_action=toggle&key={}&enabled=false", urlencoding(&key));
    let disabled = send(form(fetch(&ctx, Method::POST, "/crons"), body)).await;
    assert_eq!(disabled.status, StatusCode::OK, "{}", disabled.body);
    assert_eq!(disabled.header("x-openshop-toast"), Some("Cron%20disabled"));
    assert!(
        disabled.body.contains(r#"<s-badge tone="neutral">Disabled</s-badge>"#),
        "{}",
        disabled.body
    );

    let missing = send(form(
        fetch(&ctx, Method::POST, "/crons"),
        "_action=toggle&key=nope&enabled=false",
    ))
    .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.header("x-openshop-toast-error"), Some("Cron%20not%20found"));
}

fn urlencoding(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}
