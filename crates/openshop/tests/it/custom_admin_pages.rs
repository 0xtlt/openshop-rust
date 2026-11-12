//! Custom admin pages: discovery, access checks, the pages the server renders
//! and their forms, and their functions as a test calls them.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::common::{
    LogCapture, TEST_SHOP, decode_uri_component, install, json_body, two_shopify_apps, vault_provider,
};
use futures::FutureExt;
use maud::html;
use openshop::admin::{AdminFunctionKind, PageRequest, output};
use openshop::billing;
use openshop::builder::{AdminFunction, AdminPage, InputSchema, OpenShopBuilder};
use openshop::testing::{
    AdminTestOptions, MockTransport, TestContext, TestOptions, admin_context, invoke_admin_function, session_token,
    sign_session_token, subscription,
};
use openshop::{
    AdminActor, AdminContext, AdminMenuItem, AdminPublicError, OpenShopConfig, Outcome, Result, Value, anyhow, json,
};
use reqwest::header::{CONTENT_TYPE, HeaderMap};
use reqwest::{Method, RequestBuilder, Response};
use schemars::JsonSchema;
use serde::Deserialize;

/// The staff member the `/reviews` pages are restricted to.
const REVIEWER: &str = "123";
const OTHER_STAFF: &str = "456";

/// The header of the requests of the admin's script: a page answers them
/// with headers, not redirects.
const FETCH: &str = "x-openshop-fetch";

fn menu_item(label: &str, path: &str) -> AdminMenuItem {
    AdminMenuItem {
        label: label.to_string(),
        path: path.to_string(),
    }
}

async fn details(ctx: AdminContext, _input: Value) -> Result<Value> {
    Ok(json!({
        "id": ctx.params.get("id"),
        "actorId": ctx.actor.id,
        "shop": ctx.shop,
        "shopifyApp": ctx.shopify_app,
    }))
}

/// `page`, with a view that runs one of its loaders as `#[view]` does (its
/// `authorize` rule, then the loader) and writes what it returns in the
/// page, as JSON in `<pre id="output">`: what a test reads of the page. The
/// loader is the one `?loader=` names, the first one of the page otherwise.
fn viewed(page: AdminPage) -> AdminPage {
    let functions = page.functions.clone();
    page.view(move |request: PageRequest| {
        let functions = functions.clone();
        async move {
            let named = url::form_urlencoded::parse(request.query.as_bytes())
                .find(|(name, _)| name == "loader")
                .map(|(_, value)| value.into_owned());
            let (_, loader) = functions
                .iter()
                .filter(|(_, function)| function.kind == AdminFunctionKind::Loader)
                .find(|(name, _)| named.as_deref().is_none_or(|named| named == name.as_str()))
                .ok_or_else(|| anyhow!("the page has no loader {named:?}"))?;
            if let Some(rule) = loader.authorize.clone() {
                request.authorize(move |ctx| rule(ctx)).await?;
            }
            let output = (loader.handler)(request.ctx.clone(), Value::Null).await?;
            Ok(html! { s-page { pre id="output" { (output) } } })
        }
    })
}

/// `/reviews`, only for [`REVIEWER`].
fn reviews_page() -> AdminPage {
    AdminPage::new("/reviews").access(|ctx| async move { Ok(ctx.actor.id == REVIEWER) })
}

/// `/reviews/:id`, whose view shows its loader `details`.
fn review_details_page() -> AdminPage {
    viewed(AdminPage::new("/reviews/:id").loader("details", details))
}

/// The two review pages, with `/reviews` in the sidebar.
fn reviews_app() -> OpenShopBuilder {
    OpenShopConfig::builder()
        .admin_page(reviews_page())
        .admin_page(review_details_page())
        .admin_menu([menu_item("Reviews", "/reviews")])
}

/// One unrestricted page at `/tools`.
fn tools_app(page: AdminPage) -> OpenShopConfig {
    OpenShopConfig::builder()
        .admin_page(page)
        .admin_menu([menu_item("Tools", "/tools")])
        .build()
        .expect("valid config")
}

async fn context(config: OpenShopConfig) -> TestContext {
    let options = TestOptions {
        shop: TEST_SHOP.to_string(),
        access_token: Some("test-access-token".to_string()),
        ..Default::default()
    };
    TestContext::with_options(config, options).await.expect("test context")
}

/// The page at `path`, as staff member `sub` of the shop of the test opens
/// it from the admin: the request of the admin's script, with the session
/// token App Bridge adds.
fn page_as(ctx: &TestContext, sub: &str, path: &str) -> RequestBuilder {
    ctx.request_as(Method::GET, path, &ctx.shop, sub).header(FETCH, "1")
}

/// A form of the page at `path`, as staff member `sub` sends it from the
/// admin: `fields`, the name of its action in `_action`.
fn submit_as(ctx: &TestContext, sub: &str, path: &str, fields: &[(&str, &str)]) -> RequestBuilder {
    form(ctx.request_as(Method::POST, path, &ctx.shop, sub), fields)
}

/// `request`, a form of the admin's script.
fn form(request: RequestBuilder, fields: &[(&str, &str)]) -> RequestBuilder {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields)
        .finish();
    request
        .header(FETCH, "1")
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(body)
}

/// What a page or a form answered.
struct Answer {
    status: u16,
    headers: HeaderMap,
    body: String,
}

impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(|value| value.to_str().unwrap())
    }

    /// What the view of the page wrote in `<pre id="output">`, the output of
    /// its loader, as JSON: see [`viewed`].
    fn output(&self) -> Option<Value> {
        let start = self.body.find(r#"<pre id="output">"#)? + r#"<pre id="output">"#.len();
        let end = start + self.body[start..].find("</pre>")?;
        let text = self.body[start..end]
            .replace("&quot;", "\"")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&");
        Some(serde_json::from_str(&text).expect("JSON"))
    }

    /// The toast the action of a form asked for.
    fn toast(&self) -> Option<String> {
        self.header("x-openshop-toast").map(decode_uri_component)
    }

    /// The error toast of a form that failed.
    fn error_toast(&self) -> Option<String> {
        self.header("x-openshop-toast-error").map(decode_uri_component)
    }
}

async fn answer(request: RequestBuilder) -> Answer {
    let response = request.send().await.unwrap();
    Answer {
        status: response.status().as_u16(),
        headers: response.headers().clone(),
        body: response.text().await.unwrap(),
    }
}

async fn access(ctx: &TestContext, sub: &str, query: &str) -> Response {
    ctx.request_as(Method::GET, &format!("/api/pages/custom/access{query}"), &ctx.shop, sub)
        .send()
        .await
        .unwrap()
}

#[openshop::test(context(reviews_app().build().unwrap()))]
async fn passes_trusted_identity_and_dynamic_params_to_a_loader(ctx: &TestContext) {
    let first = answer(page_as(ctx, REVIEWER, "/reviews/41")).await;
    assert_eq!(first.status, 200, "{}", first.body);
    assert_eq!(first.output().unwrap()["id"], "41");

    let page = answer(page_as(ctx, REVIEWER, "/reviews/42")).await;
    assert_eq!(page.status, 200);
    assert_eq!(page.header("content-type"), Some("text/html; charset=utf-8"));
    let trusted = json!({ "id": "42", "actorId": "123", "shop": TEST_SHOP, "shopifyApp": "default" });
    assert_eq!(page.output(), Some(trusted.clone()));

    // Identity never comes from the query string of the page, nor from the input.
    let page = answer(page_as(
        ctx,
        REVIEWER,
        "/reviews/42?shop=attacker.myshopify.com&actor=1&id=7",
    ))
    .await;
    assert_eq!(page.output(), Some(trusted.clone()));
    let called: Value = ctx
        .call_loader_as(
            "/reviews/42",
            "details",
            json!({ "shop": "attacker.myshopify.com", "actor": { "id": "1" } }),
            TEST_SHOP,
            REVIEWER,
        )
        .await
        .unwrap();
    assert_eq!(called, trusted);
}

#[tokio::test]
async fn inherits_parent_page_policy_for_child_pages_and_functions() {
    let calls = Arc::new(AtomicUsize::new(0));
    let loader_calls = calls.clone();
    let config = OpenShopConfig::builder()
        .admin_page(reviews_page())
        .admin_page(viewed(AdminPage::new("/reviews/:id").loader(
            "details",
            move |ctx, input| {
                loader_calls.fetch_add(1, Ordering::SeqCst);
                details(ctx, input)
            },
        )))
        .admin_page(viewed(
            AdminPage::new("/reviews/:id/comments").loader("details", details),
        ))
        .admin_menu([menu_item("Reviews", "/reviews")])
        .build()
        .unwrap();
    let ctx = context(config).await;

    let denied = access(&ctx, OTHER_STAFF, "?path=%2Freviews%2F42").await;
    assert_eq!(denied.status(), 200);
    assert_eq!(json_body(denied).await, json!({ "allowed": false }));

    // The page shows nothing of itself, and its function refuses them.
    let page = answer(page_as(&ctx, OTHER_STAFF, "/reviews/42")).await;
    assert_eq!(page.status, 404);
    assert!(page.body.contains("This page is disabled."), "{}", page.body);
    assert_eq!(page.output(), None);
    let error = ctx
        .call_loader_as::<Value>("/reviews/42", "details", (), TEST_SHOP, OTHER_STAFF)
        .await
        .unwrap_err();
    assert_eq!((error.code.as_str(), error.status), ("FORBIDDEN", 403));
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    // The rule applies to every level below the page that declares it.
    let nested = access(&ctx, OTHER_STAFF, "?path=/reviews/42/comments").await;
    assert_eq!(json_body(nested).await, json!({ "allowed": false }));
    let page = answer(page_as(&ctx, OTHER_STAFF, "/reviews/42/comments")).await;
    assert_eq!((page.status, page.output()), (404, None));
    let error = ctx
        .call_loader_as::<Value>("/reviews/42/comments", "details", (), TEST_SHOP, OTHER_STAFF)
        .await
        .unwrap_err();
    assert_eq!((error.code.as_str(), error.status), ("FORBIDDEN", 403));

    let allowed = access(&ctx, REVIEWER, "?path=%2Freviews%2F42").await;
    assert_eq!(allowed.status(), 200);
    assert_eq!(json_body(allowed).await, json!({ "allowed": true }));
    let page = answer(page_as(&ctx, REVIEWER, "/reviews/42")).await;
    assert_eq!(page.status, 200);
    assert_eq!(page.output().unwrap()["id"], "42");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_static_page_takes_precedence_over_a_dynamic_sibling() {
    // Routes of the same shape cannot be registered, but a static page can
    // still overlap a dynamic sibling: the most specific one handles the path.
    let config = OpenShopConfig::builder()
        .admin_page(viewed(AdminPage::new("/reviews/:id").loader("details", details)))
        .admin_page(viewed(AdminPage::new("/reviews/new").loader("draft", details)))
        .admin_menu([])
        .build()
        .unwrap();
    let ctx = context(config).await;

    let allowed = access(&ctx, REVIEWER, "?path=%2Freviews%2Fnew").await;
    assert_eq!(json_body(allowed).await, json!({ "allowed": true }));
    let draft = answer(page_as(&ctx, REVIEWER, "/reviews/new")).await;
    assert_eq!(draft.status, 200);
    assert_eq!(draft.output().unwrap()["id"], Value::Null);
    // A form there runs the functions of that page only.
    let posted = answer(submit_as(&ctx, REVIEWER, "/reviews/new", &[("_action", "details")])).await;
    assert_eq!(posted.status, 404);

    let details = answer(page_as(&ctx, REVIEWER, "/reviews/42")).await;
    assert_eq!(details.output().unwrap()["id"], "42");
    let posted = answer(submit_as(&ctx, REVIEWER, "/reviews/42", &[("_action", "draft")])).await;
    assert_eq!(posted.status, 404);

    // The collection endpoints of the Admin API answer GET only.
    for path in ["/api/pages/custom", "/api/pages/custom/access"] {
        let response = ctx
            .request_as(Method::POST, path, &ctx.shop, REVIEWER)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 404, "{path}");
    }
    ctx.shutdown().await.unwrap();
}

#[test]
fn ambiguous_and_reserved_page_routes_are_rejected_when_the_config_is_built() {
    let error_of = |builder: OpenShopBuilder| builder.build().err().expect("invalid config").to_string();

    let error = error_of(
        OpenShopConfig::builder()
            .admin_page(AdminPage::new("/reviews/[id]"))
            .admin_page(AdminPage::new("/reviews/[slug]")),
    );
    assert!(error.contains("have the same route shape"), "{error}");

    let error = error_of(OpenShopConfig::builder().admin_page(AdminPage::new("/flows/custom")));
    assert!(error.contains("is reserved or invalid"), "{error}");

    let error = error_of(OpenShopConfig::builder().admin_page(AdminPage::new("/ops")));
    assert!(error.contains("is reserved or invalid"), "{error}");

    // The home of the admin is a path a page can take, once.
    assert!(
        OpenShopConfig::builder()
            .admin_page(AdminPage::new("/"))
            .build()
            .is_ok()
    );
    let error = error_of(
        OpenShopConfig::builder()
            .admin_page(AdminPage::new("/"))
            .admin_page(AdminPage::new("/")),
    );
    assert!(error.contains("have the same route shape"), "{error}");
}

#[tokio::test]
async fn an_invalid_admin_menu_is_rejected_when_the_config_is_built() {
    let error_of = |items: Vec<AdminMenuItem>| {
        OpenShopConfig::builder()
            .admin_page(reviews_page())
            .admin_page(review_details_page())
            .admin_menu(items)
            .build()
            .err()
            .expect("invalid menu")
            .to_string()
    };

    let error = error_of(vec![menu_item("Missing", "/missing")]);
    assert!(
        error.contains("adminMenu[0].path \"/missing\" does not match a page of the application"),
        "{error}"
    );
    let error = error_of(vec![menu_item("Review", "/reviews/:id")]);
    assert!(error.contains("must be a static custom page path"), "{error}");
    let error = error_of(vec![menu_item(" ", "/reviews")]);
    assert!(error.contains("label must be a non-empty string"), "{error}");
    let error = error_of(vec![menu_item("Reviews", "/reviews"), menu_item("Again", "/reviews")]);
    assert!(error.contains("is listed twice"), "{error}");

    // A dynamic page can be linked through one of its concrete paths.
    let config = reviews_app()
        .admin_menu([menu_item("Featured", "/reviews/featured")])
        .build()
        .unwrap();
    let ctx = context(config).await;
    let body = json_body(
        ctx.request_as(Method::GET, "/api/pages/custom", &ctx.shop, REVIEWER)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        body["navigation"],
        json!([{ "label": "Featured", "path": "/reviews/featured" }])
    );
    ctx.shutdown().await.unwrap();
}

#[openshop::test(context(reviews_app().build().unwrap()))]
async fn a_page_and_its_forms_need_a_session_token(ctx: &TestContext) {
    let page = || ctx.http.get(ctx.url_for("/reviews/42")).header(FETCH, "1");
    let posted = || form(ctx.http.post(ctx.url_for("/reviews/42")), &[("_action", "details")]);

    // The requests of the admin's script: App Bridge asks again with a fresh token.
    for request in [page(), posted()] {
        let refused = answer(request).await;
        assert_eq!(refused.status, 401);
        assert_eq!(refused.header("x-shopify-retry-invalid-session-request"), Some("1"));
        assert_eq!(refused.output(), None);
    }
    // Outside Shopify admin: where the app opens from, and nothing of the page.
    let outside = answer(ctx.http.get(ctx.url_for("/reviews/42"))).await;
    assert_eq!(outside.status, 401);
    assert!(
        outside.body.contains("Open this app from Shopify admin"),
        "{}",
        outside.body
    );
    assert_eq!(outside.output(), None);

    let response = ctx.http.get(ctx.url_for("/api/pages/custom")).send().await.unwrap();
    assert_eq!(response.status(), 401);
    assert_eq!(
        json_body(response).await,
        json!({ "error": "Unauthorized: missing session token" })
    );
    let response = ctx
        .http
        .get(ctx.url_for("/api/pages/custom/access?path=/reviews"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);

    // A token signed with another secret is no session.
    let forged = session_token(TEST_SHOP, "not-the-secret", &ctx.api_key, REVIEWER);
    for request in [page(), posted()] {
        let refused = answer(request.bearer_auth(&forged)).await;
        assert_eq!(refused.status, 401);
        assert_eq!(refused.output(), None);
    }
    let response = ctx
        .http
        .get(ctx.url_for("/api/pages/custom"))
        .bearer_auth(&forged)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert_eq!(
        json_body(response).await,
        json!({ "error": "Unauthorized: invalid session token" })
    );
}

#[tokio::test]
async fn lists_navigation_and_page_access_for_the_actor() {
    let config = OpenShopConfig::builder()
        .admin_page(AdminPage::new("/settings"))
        .admin_page(review_details_page())
        .admin_page(reviews_page())
        .admin_menu([menu_item("Reviews", "/reviews"), menu_item("Settings", "/settings")])
        .build()
        .unwrap();
    let ctx = context(config).await;

    let response = ctx
        .request_as(Method::GET, "/api/pages/custom", &ctx.shop, REVIEWER)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        json_body(response).await,
        json!({
            "navigation": [
                { "label": "Reviews", "path": "/reviews" },
                { "label": "Settings", "path": "/settings" },
            ],
            // Dynamic pages are checked on access, with their real parameters.
            "pages": [
                { "id": "reviews", "path": "/reviews", "allowed": true },
                { "id": "reviews/[id]", "path": "/reviews/:id", "allowed": false },
                { "id": "settings", "path": "/settings", "allowed": true },
            ],
        })
    );

    let response = ctx
        .request_as(Method::GET, "/api/pages/custom", &ctx.shop, OTHER_STAFF)
        .send()
        .await
        .unwrap();
    assert_eq!(
        json_body(response).await,
        json!({
            "navigation": [{ "label": "Settings", "path": "/settings" }],
            "pages": [
                { "id": "reviews", "path": "/reviews", "allowed": false },
                { "id": "reviews/[id]", "path": "/reviews/:id", "allowed": false },
                { "id": "settings", "path": "/settings", "allowed": true },
            ],
        })
    );
    ctx.shutdown().await.unwrap();
}

#[openshop::test(context(OpenShopConfig::builder().build().unwrap()))]
async fn an_application_without_pages_has_no_custom_pages(ctx: &TestContext) {
    let custom_pages = ctx
        .request_as(Method::GET, "/api/pages/custom", &ctx.shop, REVIEWER)
        .send()
        .await
        .unwrap();
    assert_eq!(custom_pages.status(), 200);
    assert_eq!(json_body(custom_pages).await, json!({ "navigation": [], "pages": [] }));

    let denied = access(ctx, REVIEWER, "?path=/reviews").await;
    assert_eq!(denied.status(), 404);
    assert_eq!(json_body(denied).await, json!({ "allowed": false }));

    // No page at its path, and no form.
    let page = answer(page_as(ctx, REVIEWER, "/reviews/42")).await;
    assert_eq!(page.status, 404);
    let posted = answer(submit_as(ctx, REVIEWER, "/reviews/42", &[("_action", "details")])).await;
    assert_eq!(posted.status, 404);
    assert_eq!(posted.error_toast().as_deref(), Some("Not found"));

    let built_in_pages = ctx
        .request_as(Method::GET, "/api/pages", &ctx.shop, REVIEWER)
        .send()
        .await
        .unwrap();
    assert_eq!(built_in_pages.status(), 200);
}

#[tokio::test]
async fn listing_a_page_serves_it_and_its_nav_label_is_its_menu_entry() {
    // No `admin_menu`: the pages are served because they are listed, and
    // the menu is made of the ones that declare a label.
    let config = OpenShopConfig::builder()
        .admin_page(reviews_page().nav("Reviews"))
        .admin_page(review_details_page())
        .build()
        .unwrap();
    assert_eq!(
        config.resolved_admin_menu(),
        [AdminMenuItem::new("Reviews", "/reviews")]
    );
    let ctx = context(config).await;

    let custom_pages = ctx
        .request_as(Method::GET, "/api/pages/custom", &ctx.shop, REVIEWER)
        .send()
        .await
        .unwrap();
    let body = json_body(custom_pages).await;
    assert_eq!(body["navigation"], json!([{ "label": "Reviews", "path": "/reviews" }]));
    assert_eq!(body["pages"].as_array().unwrap().len(), 2);
    ctx.shutdown().await.unwrap();

    // A menu set by hand replaces the entries the pages declare.
    let by_hand = OpenShopConfig::builder()
        .admin_page(reviews_page().nav("Reviews"))
        .admin_menu([AdminMenuItem::new("Customer reviews", "/reviews")])
        .build()
        .unwrap();
    assert_eq!(
        by_hand.resolved_admin_menu(),
        [AdminMenuItem::new("Customer reviews", "/reviews")]
    );

    // A page with a dynamic segment cannot be a menu entry.
    let error = OpenShopConfig::builder()
        .admin_page(review_details_page().nav("Details"))
        .build()
        .err()
        .unwrap()
        .to_string();
    assert_eq!(
        error,
        "[openshop] Invalid config: adminPages./reviews/:id.nav: a page with a dynamic segment cannot have a menu entry"
    );
}

/// The home of the admin, only for [`REVIEWER`], next to an unrestricted page.
fn home_app() -> OpenShopBuilder {
    OpenShopConfig::builder()
        .admin_page(viewed(
            AdminPage::new("/")
                .access(|ctx| async move { Ok(ctx.actor.id == REVIEWER) })
                .loader("summary", |ctx, _input| async move { Ok(json!({ "shop": ctx.shop })) }),
        ))
        .admin_page(AdminPage::new("/tools"))
}

#[openshop::test(context(home_app().build().unwrap()))]
async fn a_page_at_the_root_is_the_home_of_the_admin(ctx: &TestContext) {
    // Its view is the page at `/`, in place of the dashboard of the framework.
    let home = answer(page_as(ctx, REVIEWER, "/")).await;
    assert_eq!(home.status, 200, "{}", home.body);
    assert_eq!(home.output(), Some(json!({ "shop": TEST_SHOP })));

    let listed = ctx
        .request_as(Method::GET, "/api/pages/custom", &ctx.shop, REVIEWER)
        .send()
        .await
        .unwrap();
    assert_eq!(
        json_body(listed).await["pages"],
        json!([
            { "id": "", "path": "/", "allowed": true },
            { "id": "tools", "path": "/tools", "allowed": true },
        ])
    );
    assert_eq!(
        json_body(access(ctx, REVIEWER, "?path=%2F").await).await,
        json!({ "allowed": true })
    );

    // Its access rule is its own: the home is not a parent of the other pages.
    let refused = answer(page_as(ctx, OTHER_STAFF, "/")).await;
    assert_eq!((refused.status, refused.output()), (404, None));
    let error = ctx
        .call_loader_as::<Value>("/", "summary", (), TEST_SHOP, OTHER_STAFF)
        .await
        .unwrap_err();
    assert_eq!((error.code.as_str(), error.status), ("FORBIDDEN", 403));
    assert_eq!(
        json_body(access(ctx, OTHER_STAFF, "?path=%2F").await).await,
        json!({ "allowed": false })
    );
    assert_eq!(
        json_body(access(ctx, OTHER_STAFF, "?path=%2Ftools").await).await,
        json!({ "allowed": true })
    );
    assert_eq!(answer(page_as(ctx, OTHER_STAFF, "/tools")).await.status, 200);
}

#[openshop::test(context(reviews_app().build().unwrap()))]
async fn an_application_without_a_home_page_has_no_function_at_the_root(ctx: &TestContext) {
    // The home is the dashboard of the framework, which has no such action.
    let home = answer(page_as(ctx, REVIEWER, "/")).await;
    assert_eq!((home.status, home.output()), (200, None));
    let posted = answer(submit_as(ctx, REVIEWER, "/", &[("_action", "summary")])).await;
    assert_eq!(posted.status, 404);
    assert_eq!(access(ctx, REVIEWER, "?path=%2F").await.status(), 404);
}

#[test]
fn the_home_of_the_admin_has_no_menu_entry() {
    let error = OpenShopConfig::builder()
        .admin_page(AdminPage::new("/").nav("Home"))
        .build()
        .err()
        .unwrap()
        .to_string();
    assert_eq!(
        error,
        "[openshop] Invalid config: adminPages./.nav: the page at `/` is the home of the admin and has no menu entry"
    );

    let error = OpenShopConfig::builder()
        .admin_page(AdminPage::new("/"))
        .admin_menu([AdminMenuItem::new("Home", "/")])
        .build()
        .err()
        .unwrap()
        .to_string();
    assert_eq!(
        error,
        "[openshop] Invalid config: adminMenu[0].path must be a static custom page path"
    );
}

#[tokio::test]
async fn page_access_reports_whether_the_actor_may_open_a_path() {
    let config = OpenShopConfig::builder()
        .admin_page(reviews_page())
        .admin_page(
            AdminPage::new("/reviews/:id")
                .access(|ctx| async move { Ok(ctx.params.get("id").is_some_and(|id| id != "archived")) }),
        )
        .admin_page(AdminPage::new("/broken").access(|ctx| async move {
            if ctx.actor.id != REVIEWER {
                panic!("permission service panicked");
            }
            Err(anyhow!("permission service unavailable"))
        }))
        .admin_menu([menu_item("Reviews", "/reviews"), menu_item("Broken", "/broken")])
        .build()
        .unwrap();
    let ctx = context(config).await;

    for (sub, query, allowed) in [
        (REVIEWER, "?path=/reviews", true),
        (REVIEWER, "?path=%2Freviews%2F", true),
        (OTHER_STAFF, "?path=/reviews", false),
        // The page's own rule sees the route parameters.
        (REVIEWER, "?path=/reviews/42", true),
        (REVIEWER, "?path=/reviews/archived", false),
        (OTHER_STAFF, "?path=/reviews/42", false),
        // A failing rule denies access, whether it returns an error or panics.
        (REVIEWER, "?path=/broken", false),
        (OTHER_STAFF, "?path=/broken", false),
    ] {
        let response = access(&ctx, sub, query).await;
        assert_eq!(response.status(), 200, "{sub} {query}");
        assert_eq!(
            json_body(response).await,
            json!({ "allowed": allowed }),
            "{sub} {query}"
        );
    }

    for query in [
        "?path=/missing",
        "?path=/reviews/42/edit",
        "?path=",
        "",
        "?other=/reviews",
    ] {
        let response = access(&ctx, REVIEWER, query).await;
        assert_eq!(response.status(), 404, "{query}");
        assert_eq!(json_body(response).await, json!({ "allowed": false }), "{query}");
    }

    // A page whose rule fails is left out of the sidebar, not the whole listing.
    let body = json_body(
        ctx.request_as(Method::GET, "/api/pages/custom", &ctx.shop, REVIEWER)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(body["navigation"], json!([{ "label": "Reviews", "path": "/reviews" }]));
    assert_eq!(
        body["pages"][0],
        json!({ "id": "broken", "path": "/broken", "allowed": false })
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_form_runs_an_action_of_its_page_and_nothing_else() {
    let page = viewed(
        AdminPage::new("/tools")
            .loader("list", |_ctx, _input| async move { Ok(json!({ "items": [] })) })
            .function(
                "save",
                AdminFunction::action(
                    InputSchema::custom(json!({}), Ok),
                    |_ctx, input| async move { Ok(input) },
                ),
            ),
    );
    let ctx = context(tools_app(page)).await;

    assert_eq!(answer(page_as(&ctx, REVIEWER, "/tools")).await.status, 200);
    // The action runs, and the page is rendered again.
    let saved = answer(submit_as(&ctx, REVIEWER, "/tools", &[("_action", "save")])).await;
    assert_eq!(saved.status, 200);
    assert_eq!(saved.output(), Some(json!({ "items": [] })));

    for (path, fields) in [
        ("/unknown", &[("_action", "list")][..]),
        ("/tools/extra", &[("_action", "save")]),
        ("/tools", &[("_action", "missing")]),
        // A loader is not an action.
        ("/tools", &[("_action", "list")]),
        // A form that names no action.
        ("/tools", &[]),
        ("/tools", &[("_action", "")]),
    ] {
        let refused = answer(submit_as(&ctx, REVIEWER, path, fields)).await;
        assert_eq!(refused.status, 404, "{path} {fields:?}");
        assert_eq!(refused.error_toast().as_deref(), Some("Not found"), "{path} {fields:?}");
        assert!(refused.body.is_empty(), "{path} {fields:?}");
    }

    // A page is read with GET, and its forms are sent with POST: no other method.
    assert_eq!(answer(page_as(&ctx, REVIEWER, "/unknown")).await.status, 404);
    for method in [Method::PUT, Method::PATCH, Method::DELETE] {
        let response = ctx
            .request_as(method.clone(), "/tools", &ctx.shop, REVIEWER)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 404, "{method}");
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn function_authorize_rejects_with_403() {
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_calls = calls.clone();
    let publish = AdminFunction::action(
        InputSchema::custom(
            json!({ "type": "object", "properties": { "published": { "type": "boolean" } } }),
            |value| {
                value["published"]
                    .is_boolean()
                    .then_some(value)
                    .ok_or_else(|| "published must be a boolean".to_string())
            },
        ),
        move |_ctx, input| {
            handler_calls.fetch_add(1, Ordering::SeqCst);
            async move { Ok(json!({ "published": input["published"] })) }
        },
    )
    .authorize(|ctx| async move { Ok(ctx.actor.id == REVIEWER) });
    let page = viewed(
        AdminPage::new("/tools")
            .loader("list", |_ctx, _input| async move { Ok(json!([])) })
            .function("publish", publish),
    );
    let ctx = context(tools_app(page)).await;
    let publish_as = |sub: &str, published: &str| {
        answer(submit_as(
            &ctx,
            sub,
            "/tools",
            &[("_action", "publish"), ("published", published)],
        ))
    };

    // The page itself is open to everyone.
    let listed = answer(page_as(&ctx, OTHER_STAFF, "/tools")).await;
    assert_eq!((listed.status, listed.output()), (200, Some(json!([]))));

    // Its action is not: its form, and its function as a test calls it.
    let refused = publish_as(OTHER_STAFF, "true").await;
    assert_eq!(refused.status, 403);
    assert_eq!(refused.error_toast().as_deref(), Some("Forbidden"));
    let error = ctx
        .call_action_as::<Value>(
            "/tools",
            "publish",
            json!({ "published": true }),
            TEST_SHOP,
            OTHER_STAFF,
        )
        .await
        .unwrap_err();
    assert_eq!((error.code.as_str(), error.status), ("FORBIDDEN", 403));

    // Authorization comes before input validation.
    assert_eq!(publish_as(OTHER_STAFF, "yes").await.status, 403);
    let error = ctx
        .call_action_as::<Value>(
            "/tools",
            "publish",
            json!({ "published": "yes" }),
            TEST_SHOP,
            OTHER_STAFF,
        )
        .await
        .unwrap_err();
    assert_eq!(error.status, 403);
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let published = publish_as(REVIEWER, "true").await;
    assert_eq!(published.status, 200);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let refused = publish_as(REVIEWER, "yes").await;
    assert_eq!(refused.status, 422);
    assert_eq!(refused.error_toast().as_deref(), Some("published must be a boolean"));
    let output: Value = ctx
        .call_action_as("/tools", "publish", json!({ "published": true }), TEST_SHOP, REVIEWER)
        .await
        .unwrap();
    assert_eq!(output, json!({ "published": true }));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    ctx.shutdown().await.unwrap();
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SaveReview {
    #[schemars(length(min = 1))]
    title: String,
    rating: u8,
}

#[tokio::test]
async fn validates_input_and_rejects_with_422() {
    let search_input = InputSchema::custom(json!({ "type": "string" }), |value| {
        match value.as_str().map(str::trim) {
            Some(query) if !query.is_empty() => Ok(json!(query)),
            _ => Err("query must be a non-empty string".to_string()),
        }
    });
    let page = AdminPage::new("/tools")
        .function(
            "save",
            AdminFunction::typed_action(|ctx, input: SaveReview| async move {
                Ok(json!({ "title": input.title, "rating": input.rating, "savedBy": ctx.actor.id }))
            }),
        )
        .function(
            "search",
            AdminFunction::loader(|_ctx, input| async move { Ok(json!({ "query": input })) }).input(search_input),
        )
        .loader("echo", |_ctx, input| async move { Ok(json!({ "input": input })) });
    let ctx = context(tools_app(page)).await;
    let save = |input: Value| ctx.call_action_as::<Value>("/tools", "save", input, TEST_SHOP, REVIEWER);

    let saved = save(json!({ "title": "Great", "rating": 5 })).await.unwrap();
    assert_eq!(saved, json!({ "title": "Great", "rating": 5, "savedBy": "123" }));
    // Its form, whose fields are read as the schema types them.
    let posted = answer(submit_as(
        &ctx,
        REVIEWER,
        "/tools",
        &[("_action", "save"), ("title", "Great"), ("rating", "5")],
    ))
    .await;
    assert_eq!(posted.status, 200);

    // What the schema refuses, with the field it refuses when it can tell.
    for (input, field) in [
        (json!({ "title": "", "rating": 5 }), Some("title")),
        (json!({ "title": "Great" }), Some("rating")),
        (json!({ "title": "Great", "rating": "five" }), Some("rating")),
        (json!({ "title": "Great", "rating": 500 }), Some("rating")),
        (json!("Great"), None),
        (Value::Null, None),
    ] {
        let error = save(input.clone()).await.unwrap_err();
        assert_eq!((error.code.as_str(), error.status), ("INVALID_INPUT", 422), "{input}");
        assert!(!error.message.is_empty(), "{error:?}");
        if let Some(field) = field {
            let fields = error.field_errors.unwrap_or_default();
            assert!(fields.contains_key(field), "{input}: {fields:?}");
        }
    }
    // A form the schema refuses: `422`, and the page keeps the form as it
    // was, with an error toast, as its view does not show the form.
    for fields in [
        &[("_action", "save"), ("title", ""), ("rating", "5")][..],
        &[("_action", "save"), ("title", "Great"), ("rating", "five")],
        // A form without the fields of the input is a missing input.
        &[("_action", "save")],
    ] {
        let refused = answer(submit_as(&ctx, REVIEWER, "/tools", fields)).await;
        assert_eq!(refused.status, 422, "{fields:?}");
        assert!(
            refused.error_toast().is_some_and(|toast| !toast.is_empty()),
            "{fields:?}"
        );
        assert!(refused.body.is_empty(), "{fields:?}");
    }

    // The handler receives what the schema returns.
    let found: Value = ctx.call_loader("/tools", "search", "  rust  ").await.unwrap();
    assert_eq!(found, json!({ "query": "rust" }));
    let error = ctx.call_loader::<Value>("/tools", "search", "   ").await.unwrap_err();
    assert_eq!(
        (error.code.as_str(), error.status, error.message.as_str()),
        ("INVALID_INPUT", 422, "query must be a non-empty string")
    );

    // Loaders without a schema get the input untouched.
    let echoed: Value = ctx
        .call_loader("/tools", "echo", json!({ "page": 2, "tags": ["a", "b"] }))
        .await
        .unwrap();
    assert_eq!(echoed, json!({ "input": { "page": 2, "tags": ["a", "b"] } }));
    let echoed: Value = ctx.call_loader("/tools", "echo", ()).await.unwrap();
    assert_eq!(echoed, json!({ "input": null }));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn actions_without_an_input_schema_are_refused() {
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_calls = calls.clone();
    let unchecked = AdminFunction {
        input: None,
        ..AdminFunction::action(InputSchema::custom(json!({}), Ok), move |_ctx, input| {
            handler_calls.fetch_add(1, Ordering::SeqCst);
            async move { Ok(input) }
        })
    };
    let ctx = context(tools_app(AdminPage::new("/tools").function("unchecked", unchecked))).await;

    let error = ctx
        .call_action::<Value>("/tools", "unchecked", json!({ "anything": true }))
        .await
        .unwrap_err();
    assert_eq!(
        (error.code.as_str(), error.status, error.message.as_str()),
        ("INVALID_ACTION", 500, "Action input schema is required")
    );
    let refused = answer(submit_as(
        &ctx,
        REVIEWER,
        "/tools",
        &[("_action", "unchecked"), ("anything", "true")],
    ))
    .await;
    assert_eq!(refused.status, 500);
    assert_eq!(
        refused.error_toast().as_deref(),
        Some("Action input schema is required")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    ctx.shutdown().await.unwrap();
}

fn title_taken() -> AdminPublicError {
    AdminPublicError::new("TITLE_TAKEN", "This title is already used")
        .status(409)
        .field_errors([("title", "Already used"), ("slug", "Already used")])
}

#[tokio::test]
async fn public_errors_expose_their_status_code_and_field_errors() {
    let page = viewed(
        AdminPage::new("/tools")
            .loader("taken", |_ctx, _input| async move { Err(title_taken().into()) })
            .loader("plain", |ctx, _input| async move {
                Err(AdminPublicError::new("REQUEST", ctx.request_id).into())
            })
            .function(
                "premium",
                AdminFunction::loader(|_ctx, _input| async move { Ok(json!("unreachable")) }).authorize(
                    |_ctx| async move {
                        Err(AdminPublicError::new("PLAN_REQUIRED", "Upgrade to use this feature")
                            .status(402)
                            .into())
                    },
                ),
            )
            .action("take", InputSchema::any(), |_ctx, _input| async move {
                Err(title_taken().into())
            }),
    );
    let ctx = context(tools_app(page)).await;

    // The error a function returns, as the page receives it.
    let error = ctx.call_loader::<Value>("/tools", "taken", ()).await.unwrap_err();
    assert_eq!(error, title_taken());

    // 400 by default, without field errors; the message is the request id
    // the function saw, its own at each call.
    let plain = ctx.call_loader::<Value>("/tools", "plain", ()).await.unwrap_err();
    assert_eq!((plain.code.as_str(), plain.status), ("REQUEST", 400));
    assert_eq!(plain.field_errors, None);
    assert!(uuid::Uuid::parse_str(&plain.message).is_ok(), "{plain:?}");
    let again = ctx.call_loader::<Value>("/tools", "plain", ()).await.unwrap_err();
    assert_ne!(again.message, plain.message);

    let error = ctx.call_loader::<Value>("/tools", "premium", ()).await.unwrap_err();
    assert_eq!(
        (error.code.as_str(), error.status, error.message.as_str()),
        ("PLAN_REQUIRED", 402, "Upgrade to use this feature")
    );

    // The page whose view loads one: its status, its message in the page.
    for (loader, status, message) in [
        ("taken", 409, "This title is already used"),
        ("premium", 402, "Upgrade to use this feature"),
    ] {
        let page = answer(page_as(&ctx, REVIEWER, &format!("/tools?loader={loader}"))).await;
        assert_eq!(page.status, status, "{loader}");
        assert!(
            page.body
                .contains(&format!(r#"<s-banner tone="critical" heading="{message}">"#)),
            "{}",
            page.body
        );
    }
    assert_eq!(answer(page_as(&ctx, REVIEWER, "/tools?loader=plain")).await.status, 400);

    // A form whose action returns one: its status, its message in an error
    // toast, the page as it was (its view does not show the fields).
    let refused = answer(submit_as(&ctx, REVIEWER, "/tools", &[("_action", "take")])).await;
    assert_eq!(refused.status, 409);
    assert_eq!(refused.error_toast().as_deref(), Some("This title is already used"));
    assert!(refused.body.is_empty());
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn unexpected_errors_return_500_without_leaking_details() {
    let page = viewed(
        AdminPage::new("/tools")
            .loader("works", |_ctx, _input| async move { Ok(json!({ "ok": true })) })
            .loader("fails", |_ctx, _input| async move {
                Err(anyhow!("database password is hunter2"))
            })
            .loader("panics", |ctx, _input| async move {
                // No such parameter on a static page.
                Ok(json!({ "id": ctx.params["id"] }))
            })
            .function(
                "guarded",
                AdminFunction::loader(|_ctx, _input| async move { Ok(json!("unreachable")) })
                    .authorize(|_ctx| async move { Err(anyhow!("permission lookup failed: hunter2")) }),
            )
            .action("explode", InputSchema::any(), |_ctx, _input| async move {
                Err(anyhow!("database password is hunter2"))
            }),
    );
    let ctx = context(tools_app(page)).await;

    for name in ["fails", "panics", "guarded"] {
        let page = answer(page_as(&ctx, REVIEWER, &format!("/tools?loader={name}"))).await;
        assert_eq!(page.status, 500, "{name}");
        assert!(!page.body.contains("hunter2"), "{}", page.body);
        assert!(page.body.contains("This page could not be displayed."), "{}", page.body);
    }
    let works = answer(page_as(&ctx, REVIEWER, "/tools?loader=works")).await;
    assert_eq!((works.status, works.output()), (200, Some(json!({ "ok": true }))));

    // A form whose action fails.
    let failed = answer(submit_as(&ctx, REVIEWER, "/tools", &[("_action", "explode")])).await;
    assert_eq!(failed.status, 500);
    assert_eq!(failed.error_toast().as_deref(), Some("Internal server error"));
    assert!(!format!("{:?} {}", failed.headers, failed.body).contains("hunter2"));

    // A test is told what the page is not.
    let error = ctx.call_loader::<Value>("/tools", "fails", ()).await.unwrap_err();
    assert_eq!((error.code.as_str(), error.status), ("INTERNAL_ERROR", 500));
    assert!(error.message.contains("hunter2"), "{error:?}");
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn decodes_dynamic_route_params_and_function_names() {
    let params = |ctx: AdminContext, _input: Value| async move { Ok(json!(ctx.params)) };
    let config = OpenShopConfig::builder()
        .admin_page(viewed(
            AdminPage::new("/reviews/[id]")
                .loader("params", params)
                .loader("review details", |_ctx, _input| async move { Ok(json!("by name")) })
                .action("save review", InputSchema::any(), |ctx, _input| async move {
                    output(Outcome::toast(format!("Saved {}", ctx.params["id"])))
                }),
        ))
        .admin_page(viewed(
            AdminPage::new("/reviews/:id/comments/:comment_id").loader("params", params),
        ))
        .admin_menu([])
        .build()
        .unwrap();
    let ctx = context(config).await;
    let shown = async |path: &str| answer(page_as(&ctx, REVIEWER, path)).await.output();

    assert_eq!(shown("/reviews/42").await, Some(json!({ "id": "42" })));
    assert_eq!(
        shown("/reviews/caf%C3%A9%20noir").await,
        Some(json!({ "id": "café noir" }))
    );
    assert_eq!(
        shown("/reviews/42/comments/7").await,
        Some(json!({ "id": "42", "comment_id": "7" }))
    );
    let called: Value = ctx
        .call_loader("/reviews/caf%C3%A9%20noir", "params", ())
        .await
        .unwrap();
    assert_eq!(called, json!({ "id": "café noir" }));

    // A function is found by its name as it is written, spaces included.
    let named: Value = ctx.call_loader("/reviews/1", "review details", ()).await.unwrap();
    assert_eq!(named, json!("by name"));
    let saved = answer(submit_as(
        &ctx,
        REVIEWER,
        "/reviews/caf%C3%A9%20noir",
        &[("_action", "save review")],
    ))
    .await;
    assert_eq!(saved.status, 200);
    assert_eq!(saved.toast().as_deref(), Some("Saved café noir"));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_form_has_a_request_id_of_its_own_and_no_idempotency_key() {
    let page = AdminPage::new("/tools").action("charge", InputSchema::any(), |ctx, _input| async move {
        let seen = json!({ "idempotencyKey": ctx.idempotency_key, "requestId": ctx.request_id });
        output(Outcome::toast(seen.to_string()))
    });
    let ctx = context(tools_app(page)).await;
    let logs = LogCapture::start().await;
    let charge = async |request: RequestBuilder| {
        let charged = answer(request).await;
        assert_eq!(charged.status, 200);
        serde_json::from_str::<Value>(&charged.toast().unwrap()).unwrap()
    };

    // A form sends no idempotency key: a header that would be one is not read.
    let first = charge(
        submit_as(&ctx, REVIEWER, "/tools", &[("_action", "charge")]).header("Idempotency-Key", "charge-2026-0001"),
    )
    .await;
    assert_eq!(first["idempotencyKey"], Value::Null);
    let second = charge(submit_as(&ctx, REVIEWER, "/tools", &[("_action", "charge")])).await;
    assert_eq!(second["idempotencyKey"], Value::Null);
    // Nor does a test's call.
    let called: Outcome = ctx.call_action("/tools", "charge", ()).await.unwrap();
    let called: Value = serde_json::from_str(&called.toast.unwrap()).unwrap();
    assert_eq!(called["idempotencyKey"], Value::Null);

    // Every form, and every call, gets its own request id.
    for seen in [&first, &second, &called] {
        let request_id = seen["requestId"].as_str().expect("requestId");
        assert!(uuid::Uuid::parse_str(request_id).is_ok(), "{request_id}");
    }
    assert_ne!(first["requestId"], second["requestId"]);
    assert_ne!(second["requestId"], called["requestId"]);

    // A form is logged with its request id, its shop and who sent it.
    let line = logs
        .lines()
        .into_iter()
        .find(|line| {
            line.message == "[openshop] Custom admin function completed"
                && line
                    .context
                    .as_ref()
                    .is_some_and(|context| context["requestId"] == first["requestId"])
        })
        .expect("the line of the form");
    drop(logs);
    assert_eq!(line.level, "info");
    let context = line.context.unwrap();
    for (key, value) in [
        ("shop", json!(TEST_SHOP)),
        ("shopifyApp", json!("default")),
        ("actorId", json!(REVIEWER)),
        ("pageId", json!("tools")),
        ("functionName", json!("charge")),
        ("kind", json!("action")),
        ("form", json!(true)),
    ] {
        assert_eq!(context[key], value, "{key}: {context}");
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn takes_the_actor_from_the_session_token() {
    let page = viewed(AdminPage::new("/tools").loader("whoami", |ctx, _input| async move { Ok(json!(ctx.actor)) }));
    let ctx = context(tools_app(page)).await;

    for sub in [REVIEWER, OTHER_STAFF] {
        let page = answer(page_as(&ctx, sub, "/tools")).await;
        assert_eq!(page.status, 200);
        assert_eq!(page.output(), Some(json!({ "id": sub, "sessionId": "test-sid" })));
    }

    let now = chrono::Utc::now().timestamp();
    let token = sign_session_token(
        &json!({
            "iss": format!("https://{TEST_SHOP}/admin"),
            "dest": format!("https://{TEST_SHOP}"),
            "aud": ctx.api_key,
            "sub": "789",
            "exp": now + 60,
            "nbf": now - 10,
            "iat": now,
            "jti": "custom-page-test",
            "sid": "custom-page-session",
        }),
        &ctx.secret,
    );
    let page = answer(
        ctx.http
            .get(ctx.url_for("/tools"))
            .bearer_auth(token)
            .header(FETCH, "1"),
    )
    .await;
    assert_eq!(
        page.output(),
        Some(json!({ "id": "789", "sessionId": "custom-page-session" }))
    );
    ctx.shutdown().await.unwrap();
}

/// Reads everything a function can reach for its tenant.
async fn tenant(ctx: AdminContext, _input: Value) -> Result<Value> {
    let secret: String = ctx.connectors.call_as("vault", "reveal", "vault").await?;
    let shopify = ctx.shopify.graphql("{ shop { name } }", ()).await?;
    let (installations,): (i64,) = sqlx::query_as("SELECT count(*) FROM installations")
        .fetch_one(&ctx.db)
        .await?;
    Ok(json!({
        "shop": ctx.shop,
        "shopifyApp": ctx.shopify_app,
        "secret": secret,
        "shopName": shopify["shop"]["name"],
        "clientShop": ctx.shopify.shop,
        "installations": installations,
    }))
}

async fn store_vault_secret(ctx: &TestContext, app_handle: &str, shop: &str, secret: &str) {
    sqlx::query(
        "INSERT INTO provider_configs (app_handle, shop, provider_name, config) VALUES ($1, $2, 'vault', $3::json)",
    )
    .bind(app_handle)
    .bind(shop)
    .bind(json!({ "secret": secret }).to_string())
    .execute(&ctx.db)
    .await
    .unwrap();
}

/// Answers GraphQL calls with the name of the shop they were sent to.
fn answer_with_shop_name(transport: &MockTransport) {
    transport.respond_with(|request| {
        let shop = request
            .url
            .trim_start_matches("https://")
            .split('/')
            .next()
            .unwrap_or_default();
        Ok(MockTransport::json(
            200,
            json!({ "data": { "shop": { "name": format!("Shop {shop}") } } }),
        ))
    });
}

fn access_token_sent_to(ctx: &TestContext, shop: &str) -> Vec<String> {
    ctx.transport
        .requests()
        .into_iter()
        .filter(|request| request.url.starts_with(&format!("https://{shop}/admin/api/")))
        .flat_map(|request| request.headers)
        .filter(|(name, _)| name == "X-Shopify-Access-Token")
        .map(|(_, value)| value)
        .collect()
}

#[tokio::test]
async fn scopes_connectors_and_shopify_client_to_the_session_shop() {
    const OTHER_SHOP: &str = "other-tenant.myshopify.com";
    let config = OpenShopConfig::builder()
        .provider_as("vault", vault_provider())
        .admin_page(viewed(AdminPage::new("/tools").loader("tenant", tenant)))
        .admin_menu([])
        .build()
        .unwrap();
    let ctx = context(config).await;
    answer_with_shop_name(&ctx.transport);
    install(&ctx.db, "default", OTHER_SHOP, Some("other-access-token"), false).await;
    store_vault_secret(&ctx, "default", TEST_SHOP, "first-secret").await;
    store_vault_secret(&ctx, "default", OTHER_SHOP, "other-secret").await;

    let page = answer(page_as(&ctx, REVIEWER, "/tools")).await;
    assert_eq!(page.status, 200, "{}", page.body);
    assert_eq!(
        page.output(),
        Some(json!({
            "shop": TEST_SHOP,
            "shopifyApp": "default",
            "secret": "vault:first-secret",
            "shopName": format!("Shop {TEST_SHOP}"),
            "clientShop": TEST_SHOP,
            "installations": 2,
        }))
    );

    // A staff member of another shop sees that shop's, whatever the URL says.
    let page = answer(
        ctx.request_as(Method::GET, &format!("/tools?shop={TEST_SHOP}"), OTHER_SHOP, REVIEWER)
            .header(FETCH, "1"),
    )
    .await;
    assert_eq!(page.status, 200, "{}", page.body);
    let shown = page.output().unwrap();
    assert_eq!(shown["shop"], OTHER_SHOP);
    assert_eq!(shown["secret"], "vault:other-secret");
    assert_eq!(shown["shopName"], format!("Shop {OTHER_SHOP}"));
    assert_eq!(shown["clientShop"], OTHER_SHOP);

    assert_eq!(access_token_sent_to(&ctx, TEST_SHOP), ["test-access-token"]);
    assert_eq!(access_token_sent_to(&ctx, OTHER_SHOP), ["other-access-token"]);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn resolves_the_shopify_app_from_the_token_audience() {
    let config = OpenShopConfig::builder()
        .shopify(two_shopify_apps())
        .provider_as("vault", vault_provider())
        .admin_page(viewed(AdminPage::new("/tools").loader("tenant", tenant)))
        .admin_menu([])
        .build()
        .unwrap();
    let ctx = TestContext::new(config).await.unwrap();
    answer_with_shop_name(&ctx.transport);
    // The same shop installed both apps, each with its own token and configuration.
    install(&ctx.db, "clientA", TEST_SHOP, Some("token-a"), false).await;
    install(&ctx.db, "clientB", TEST_SHOP, Some("token-b"), false).await;
    store_vault_secret(&ctx, "clientA", TEST_SHOP, "secret-a").await;
    store_vault_secret(&ctx, "clientB", TEST_SHOP, "secret-b").await;
    let page = |token: String| {
        ctx.http
            .get(ctx.url_for("/tools"))
            .bearer_auth(token)
            .header(FETCH, "1")
    };

    for (handle, api_key, api_secret, secret) in [
        ("clientA", "client-a-key", "client-a-secret", "vault:secret-a"),
        ("clientB", "client-b-key", "client-b-secret", "vault:secret-b"),
    ] {
        let token = session_token(TEST_SHOP, api_secret, api_key, REVIEWER);
        let shown = answer(page(token)).await;
        assert_eq!(shown.status, 200, "{handle}: {}", shown.body);
        let shown = shown.output().unwrap();
        assert_eq!(shown["shopifyApp"], handle);
        assert_eq!(shown["secret"], secret);
    }
    assert_eq!(access_token_sent_to(&ctx, TEST_SHOP), ["token-a", "token-b"]);

    // A token signed with another app's secret is not a session.
    let token = session_token(TEST_SHOP, "client-a-secret", "client-b-key", REVIEWER);
    let refused = answer(page(token)).await;
    assert_eq!((refused.status, refused.output()), (401, None));
    ctx.shutdown().await.unwrap();
}

#[derive(Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedReview {
    title: String,
    saved_by: String,
}

#[openshop::test(context(reviews_app().build().unwrap()))]
async fn functions_can_be_invoked_without_http(ctx: &TestContext) {
    let save = AdminFunction::typed_action(|ctx, input: SaveReview| async move {
        Ok(json!({ "title": input.title, "rating": input.rating, "savedBy": ctx.actor.id }))
    })
    .authorize(|ctx| async move { Ok(ctx.actor.id == REVIEWER) });

    let defaults = admin_context(&ctx.app, AdminTestOptions::default()).await.unwrap();
    assert_eq!(defaults.shop, "test.myshopify.com");
    assert_eq!(defaults.shopify_app, "default");
    assert_eq!(
        defaults.actor,
        AdminActor {
            id: "test-actor".to_string(),
            session_id: "test-session".to_string(),
        }
    );
    assert_eq!(defaults.request_id, "test-request");
    assert_eq!(defaults.idempotency_key, None);
    assert!(defaults.params.is_empty());

    let options = AdminTestOptions {
        shop: TEST_SHOP.to_string(),
        actor_id: REVIEWER.to_string(),
        params: [("id".to_string(), "42".to_string())].into(),
        ..Default::default()
    };
    let reviewer = admin_context(&ctx.app, options).await.unwrap();

    let output: Value = invoke_admin_function(&AdminFunction::loader(details), reviewer.clone(), Value::Null)
        .await
        .unwrap();
    assert_eq!(
        output,
        json!({ "id": "42", "actorId": "123", "shop": TEST_SHOP, "shopifyApp": "default" })
    );

    let saved: SavedReview = invoke_admin_function(&save, reviewer.clone(), json!({ "title": "Great", "rating": 5 }))
        .await
        .unwrap();
    assert_eq!(
        saved,
        SavedReview {
            title: "Great".to_string(),
            saved_by: "123".to_string(),
        }
    );

    // Input validation and authorization apply as they do over HTTP.
    let error = invoke_admin_function::<Value>(&save, reviewer, json!({ "title": "", "rating": 5 }))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("title"), "{error}");

    let error = invoke_admin_function::<Value>(&save, defaults, json!({ "title": "Great", "rating": 5 }))
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "Admin function authorization rejected");
}

/// The review pages, with an action only [`REVIEWER`] may run and that
/// needs a plan, a home with a loader that fails, and the shop installed.
fn reviews_app_with_functions() -> OpenShopConfig {
    let save = AdminFunction::typed_action(|ctx, input: SaveReview| async move {
        Ok(json!({ "title": input.title, "rating": input.rating, "savedBy": ctx.actor.id }))
    })
    .authorize(|ctx| async move { Ok(ctx.actor.id == REVIEWER && billing::any_plan(ctx).await?) });
    OpenShopConfig::builder()
        .admin_page(reviews_page())
        .admin_page(review_details_page().function("save", save))
        .admin_page(AdminPage::new("/").loader("home", |_ctx, _input| async { anyhow::bail!("the shop is closed") }))
        .build()
        .unwrap()
}

/// [`context`], in a project whose `shopify.app.toml` gives the app its
/// handle, which the pricing page of a `402` names it by.
async fn context_with_handle(config: OpenShopConfig) -> TestContext {
    let cwd = std::env::temp_dir().join("openshop-custom-pages-handle");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(
        cwd.join("shopify.app.toml"),
        "client_id = \"test-app\"\nhandle = \"reviews\"\n",
    )
    .unwrap();
    let options = TestOptions {
        shop: TEST_SHOP.to_string(),
        access_token: Some("test-access-token".to_string()),
        cwd: Some(cwd),
        ..Default::default()
    };
    TestContext::with_options(config, options).await.expect("test context")
}

#[openshop::test(context_with_handle(reviews_app_with_functions()))]
async fn a_test_calls_the_functions_of_a_page_by_its_path_as_the_view_does(ctx: &TestContext) {
    // The loader of `/reviews/:id` at `/reviews/42`, as the reviewer: the
    // rule of `/reviews`, which the page is nested under, lets them in.
    let details: Value = ctx
        .call_loader_as("/reviews/42", "details", (), TEST_SHOP, REVIEWER)
        .await
        .unwrap();
    assert_eq!(
        details,
        json!({ "id": "42", "actorId": REVIEWER, "shop": TEST_SHOP, "shopifyApp": "default" })
    );
    // Staff user `1` of the shop of the test, which the rule refuses.
    let error = ctx
        .call_loader::<Value>("/reviews/42", "details", ())
        .await
        .unwrap_err();
    assert_eq!((error.code.as_str(), error.status), ("FORBIDDEN", 403));

    // An action, typed, with a plan.
    ctx.shopify.active_subscriptions([subscription("Pro")]);
    let saved: SavedReview = ctx
        .call_action_as(
            "/reviews/42",
            "save",
            json!({ "title": "Great", "rating": 5 }),
            TEST_SHOP,
            REVIEWER,
        )
        .await
        .unwrap();
    assert_eq!(
        saved,
        SavedReview {
            title: "Great".into(),
            saved_by: REVIEWER.into()
        }
    );
    // What the page is told: the input does not fit, the shop has no plan.
    let save = |input| ctx.call_action_as::<SavedReview>("/reviews/42", "save", input, TEST_SHOP, REVIEWER);
    let error = save(json!({ "title": "", "rating": 5 })).await.unwrap_err();
    assert_eq!((error.code.as_str(), error.status), ("INVALID_INPUT", 422));
    ctx.shopify.active_subscriptions([]);
    let error = save(json!({ "title": "Great", "rating": 5 })).await.unwrap_err();
    assert_eq!((error.code.as_str(), error.status), (billing::PLAN_REQUIRED, 402));
    assert_eq!(
        error.details.unwrap()["pricingLink"],
        "shopify:admin/charges/reviews/pricing_plans"
    );

    // Any other error is the `500` of the page, with its message for the test.
    let error = ctx.call_loader::<Value>("/", "home", ()).await.unwrap_err();
    assert_eq!(
        (error.code.as_str(), error.status, error.message.as_str()),
        ("INTERNAL_ERROR", 500, "the shop is closed")
    );
}

#[openshop::test(context(reviews_app_with_functions()))]
async fn a_test_that_names_no_function_of_a_page_is_told_what_the_page_has(ctx: &TestContext) {
    for (call, expected) in [
        (
            ctx.call_action::<Value>("/reviews/42", "details", ()).boxed(),
            "[openshop] The admin page `/reviews/:id` has no action `details`. Its loaders: details; its actions: save",
        ),
        (
            ctx.call_loader::<Value>("/orders", "list", ()).boxed(),
            "[openshop] No admin page of the application is at `/orders`. Its pages: /, /reviews, /reviews/:id",
        ),
    ] {
        let panic = std::panic::AssertUnwindSafe(call).catch_unwind().await.unwrap_err();
        assert_eq!(panic.downcast_ref::<String>().map(String::as_str), Some(expected));
    }
}

#[tokio::test]
async fn a_loader_logs_for_its_shop_and_request() {
    let page = viewed(
        AdminPage::new("/tools").loader("check", |ctx: AdminContext, _input: Value| async move {
            ctx.logger.error_with("Admin logger check", json!({ "step": "load" }));
            Ok(json!({ "requestId": ctx.request_id }))
        }),
    );
    let ctx = context(tools_app(page)).await;
    let logs = LogCapture::start().await;

    // The page, whose view runs the loader.
    let page = answer(page_as(&ctx, REVIEWER, "/tools")).await;
    assert_eq!(page.status, 200);
    let request_id = page.output().unwrap()["requestId"].clone();
    assert!(
        uuid::Uuid::parse_str(request_id.as_str().unwrap()).is_ok(),
        "{request_id}"
    );

    let line = logs
        .lines()
        .into_iter()
        .find(|line| line.message == "Admin logger check")
        .expect("the line of the loader");
    assert_eq!(line.level, "error");
    assert_eq!(
        line.context,
        Some(json!({ "step": "load", "shop": TEST_SHOP, "shopifyApp": "default", "requestId": request_id }))
    );
}
