//! Custom admin pages: discovery, access checks and the loader/action RPC endpoint.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use openshop::builder::{AdminFunction, AdminPage, Field, InputSchema, OpenShopBuilder, Provider};
use openshop::testing::{
    AdminTestOptions, MockTransport, TestContext, TestOptions, admin_context, invoke_admin_function, session_token,
    sign_session_token,
};
use openshop::{
    AdminActor, AdminContext, AdminMenuItem, AdminPublicError, OpenShopConfig, Result, ShopifyAppConfig, ShopifyConfig,
    Value, anyhow, json,
};
use openshop_shared::build_admin_function_rpc_url;
use reqwest::{Method, RequestBuilder, Response};
use schemars::JsonSchema;
use serde::Deserialize;

const TEST_SHOP: &str = "test-integration.myshopify.com";
/// The staff member the `/reviews` pages are restricted to.
const REVIEWER: &str = "123";
const OTHER_STAFF: &str = "456";

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

/// `/reviews`, only for [`REVIEWER`].
fn reviews_page() -> AdminPage {
    AdminPage::new("/reviews").access(|ctx| async move { Ok(ctx.actor.id == REVIEWER) })
}

fn review_details_page() -> AdminPage {
    AdminPage::new("/reviews/:id").loader("details", details)
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

/// A request carrying the session token of the staff member `sub`.
fn request_as(ctx: &TestContext, sub: &str, method: Method, path: &str) -> RequestBuilder {
    ctx.http
        .request(method, format!("{}{path}", ctx.url))
        .bearer_auth(ctx.session_token_for(&ctx.shop, sub))
}

/// Calls the loader or action `name` of the page displayed at `pathname`.
fn rpc(ctx: &TestContext, sub: &str, pathname: &str, kind: &str, name: &str, input: Value) -> RequestBuilder {
    let url = build_admin_function_rpc_url(kind, name, pathname);
    request_as(ctx, sub, Method::POST, &url).json(&json!({ "input": input }))
}

async fn load(ctx: &TestContext, pathname: &str, name: &str, input: Value) -> Response {
    rpc(ctx, REVIEWER, pathname, "loader", name, input)
        .send()
        .await
        .unwrap()
}

async fn act(ctx: &TestContext, pathname: &str, name: &str, input: Value) -> Response {
    rpc(ctx, REVIEWER, pathname, "action", name, input)
        .send()
        .await
        .unwrap()
}

async fn access(ctx: &TestContext, sub: &str, query: &str) -> Response {
    request_as(ctx, sub, Method::GET, &format!("/api/pages/custom/access{query}"))
        .send()
        .await
        .unwrap()
}

async fn json_body(response: Response) -> Value {
    response.json().await.expect("JSON body")
}

/// Checks an RPC error payload and returns its request id.
async fn assert_rpc_error(response: Response, status: u16, code: &str, message: &str) -> String {
    assert_eq!(response.status(), status);
    let body = json_body(response).await;
    assert_eq!(body["code"], code, "{body}");
    assert_eq!(body["error"], message, "{body}");
    let request_id = body["requestId"].as_str().expect("requestId").to_string();
    assert!(uuid::Uuid::parse_str(&request_id).is_ok(), "{request_id}");
    request_id
}

#[openshop::test(context(reviews_app().build().unwrap()))]
async fn passes_trusted_identity_and_dynamic_params_to_a_loader(ctx: &TestContext) {
    let first = load(ctx, "/reviews/41", "details", Value::Null).await;
    assert_eq!(first.status(), 200);
    assert_eq!(json_body(first).await["id"], "41");

    let response = load(ctx, "/reviews/42", "details", Value::Null).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "application/json; charset=UTF-8");
    assert_eq!(
        json_body(response).await,
        json!({ "id": "42", "actorId": "123", "shop": TEST_SHOP, "shopifyApp": "default" })
    );

    // Identity never comes from the query string or the input.
    let url = "/api/pages/custom/reviews/42/_rpc/loader/details?shop=attacker.myshopify.com";
    let response = request_as(ctx, REVIEWER, Method::POST, url)
        .json(&json!({ "input": { "shop": "attacker.myshopify.com", "actor": { "id": "1" } } }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        json_body(response).await,
        json!({ "id": "42", "actorId": "123", "shop": TEST_SHOP, "shopifyApp": "default" })
    );
}

#[tokio::test]
async fn inherits_parent_page_policy_for_child_direct_access_and_rpc() {
    let calls = Arc::new(AtomicUsize::new(0));
    let loader_calls = calls.clone();
    let config = OpenShopConfig::builder()
        .admin_page(reviews_page())
        .admin_page(AdminPage::new("/reviews/:id").loader("details", move |ctx, input| {
            loader_calls.fetch_add(1, Ordering::SeqCst);
            details(ctx, input)
        }))
        .admin_page(AdminPage::new("/reviews/:id/comments").loader("details", details))
        .admin_menu([menu_item("Reviews", "/reviews")])
        .build()
        .unwrap();
    let ctx = context(config).await;

    let denied = access(&ctx, OTHER_STAFF, "?path=%2Freviews%2F42").await;
    assert_eq!(denied.status(), 200);
    assert_eq!(json_body(denied).await, json!({ "allowed": false }));

    let response = rpc(&ctx, OTHER_STAFF, "/reviews/42", "loader", "details", Value::Null)
        .send()
        .await
        .unwrap();
    assert_rpc_error(response, 403, "FORBIDDEN", "Forbidden").await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    // The rule applies to every level below the page that declares it.
    let nested = access(&ctx, OTHER_STAFF, "?path=/reviews/42/comments").await;
    assert_eq!(json_body(nested).await, json!({ "allowed": false }));
    let response = rpc(
        &ctx,
        OTHER_STAFF,
        "/reviews/42/comments",
        "loader",
        "details",
        Value::Null,
    )
    .send()
    .await
    .unwrap();
    assert_rpc_error(response, 403, "FORBIDDEN", "Forbidden").await;

    let allowed = access(&ctx, REVIEWER, "?path=%2Freviews%2F42").await;
    assert_eq!(allowed.status(), 200);
    assert_eq!(json_body(allowed).await, json!({ "allowed": true }));
    assert_eq!(load(&ctx, "/reviews/42", "details", Value::Null).await.status(), 200);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_static_page_takes_precedence_over_a_dynamic_sibling() {
    // Routes of the same shape cannot be registered, but a static page can
    // still overlap a dynamic sibling: the most specific one handles the path.
    let config = OpenShopConfig::builder()
        .admin_page(AdminPage::new("/reviews/:id").loader("details", details))
        .admin_page(AdminPage::new("/reviews/new").loader("draft", details))
        .admin_menu([])
        .build()
        .unwrap();
    let ctx = context(config).await;

    let allowed = access(&ctx, REVIEWER, "?path=%2Freviews%2Fnew").await;
    assert_eq!(json_body(allowed).await, json!({ "allowed": true }));
    let draft = load(&ctx, "/reviews/new", "draft", Value::Null).await;
    assert_eq!(draft.status(), 200);
    assert_eq!(json_body(draft).await["id"], Value::Null);
    let response = load(&ctx, "/reviews/new", "details", Value::Null).await;
    assert_rpc_error(response, 404, "NOT_FOUND", "Not found").await;

    let details = load(&ctx, "/reviews/42", "details", Value::Null).await;
    assert_eq!(json_body(details).await["id"], "42");
    let response = load(&ctx, "/reviews/42", "draft", Value::Null).await;
    assert_rpc_error(response, 404, "NOT_FOUND", "Not found").await;

    // Posting to the collection endpoints is an RPC without a function.
    for path in ["/api/pages/custom", "/api/pages/custom/access"] {
        let response = request_as(&ctx, REVIEWER, Method::POST, path).send().await.unwrap();
        assert_rpc_error(response, 404, "NOT_FOUND", "Not found").await;
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
        request_as(&ctx, REVIEWER, Method::GET, "/api/pages/custom")
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
async fn rejects_rpc_requests_without_a_session_token(ctx: &TestContext) {
    let rpc_url = format!("{}/api/pages/custom/reviews/42/_rpc/loader/details", ctx.url);

    let response = ctx
        .http
        .post(&rpc_url)
        .json(&json!({ "input": null }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert_eq!(
        json_body(response).await,
        json!({ "error": "Unauthorized: missing session token" })
    );

    for path in ["/api/pages/custom", "/api/pages/custom/access?path=/reviews"] {
        let response = ctx.http.get(format!("{}{path}", ctx.url)).send().await.unwrap();
        assert_eq!(response.status(), 401, "{path}");
    }

    let forged = session_token(TEST_SHOP, "not-the-secret", &ctx.api_key, REVIEWER);
    let response = ctx
        .http
        .post(&rpc_url)
        .bearer_auth(forged)
        .json(&json!({ "input": null }))
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

    let response = request_as(&ctx, REVIEWER, Method::GET, "/api/pages/custom")
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

    let response = request_as(&ctx, OTHER_STAFF, Method::GET, "/api/pages/custom")
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
    let custom_pages = request_as(ctx, REVIEWER, Method::GET, "/api/pages/custom")
        .send()
        .await
        .unwrap();
    assert_eq!(custom_pages.status(), 200);
    assert_eq!(json_body(custom_pages).await, json!({ "navigation": [], "pages": [] }));

    let denied = access(ctx, REVIEWER, "?path=/reviews").await;
    assert_eq!(denied.status(), 404);
    assert_eq!(json_body(denied).await, json!({ "allowed": false }));

    let response = load(ctx, "/reviews/42", "details", Value::Null).await;
    assert_rpc_error(response, 404, "NOT_FOUND", "Not found").await;

    let built_in_pages = request_as(ctx, REVIEWER, Method::GET, "/api/pages")
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

    let custom_pages = request_as(&ctx, REVIEWER, Method::GET, "/api/pages/custom")
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
        .admin_page(
            AdminPage::new("/")
                .access(|ctx| async move { Ok(ctx.actor.id == REVIEWER) })
                .loader("summary", |ctx, _input| async move { Ok(json!({ "shop": ctx.shop })) }),
        )
        .admin_page(AdminPage::new("/tools"))
}

#[openshop::test(context(home_app().build().unwrap()))]
async fn a_page_at_the_root_is_the_home_of_the_admin(ctx: &TestContext) {
    assert_eq!(
        build_admin_function_rpc_url("loader", "summary", "/"),
        "/api/pages/custom/_rpc/loader/summary"
    );
    // A client that keeps the `/` of the page in the address is served too.
    for url in [
        "/api/pages/custom/_rpc/loader/summary",
        "/api/pages/custom//_rpc/loader/summary",
    ] {
        let response = request_as(ctx, REVIEWER, Method::POST, url)
            .json(&json!({ "input": null }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{url}");
        assert_eq!(json_body(response).await, json!({ "shop": TEST_SHOP }), "{url}");
    }

    let listed = request_as(ctx, REVIEWER, Method::GET, "/api/pages/custom")
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
    let refused = rpc(ctx, OTHER_STAFF, "/", "loader", "summary", Value::Null)
        .send()
        .await
        .unwrap();
    assert_rpc_error(refused, 403, "FORBIDDEN", "Forbidden").await;
    assert_eq!(
        json_body(access(ctx, OTHER_STAFF, "?path=%2F").await).await,
        json!({ "allowed": false })
    );
    assert_eq!(
        json_body(access(ctx, OTHER_STAFF, "?path=%2Ftools").await).await,
        json!({ "allowed": true })
    );
}

#[openshop::test(context(reviews_app().build().unwrap()))]
async fn an_application_without_a_home_page_has_no_function_at_the_root(ctx: &TestContext) {
    let response = rpc(ctx, REVIEWER, "/", "loader", "summary", Value::Null)
        .send()
        .await
        .unwrap();
    assert_rpc_error(response, 404, "NOT_FOUND", "Not found").await;
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
        request_as(&ctx, REVIEWER, Method::GET, "/api/pages/custom")
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
async fn returns_404_for_unknown_pages_functions_and_kind_mismatches() {
    let page = AdminPage::new("/tools")
        .loader("list", |_ctx, _input| async move { Ok(json!({ "items": [] })) })
        .function(
            "save",
            AdminFunction::action(
                InputSchema::custom(json!({}), Ok),
                |_ctx, input| async move { Ok(input) },
            ),
        );
    let ctx = context(tools_app(page)).await;

    assert_eq!(load(&ctx, "/tools", "list", Value::Null).await.status(), 200);
    assert_eq!(act(&ctx, "/tools", "save", json!(1)).await.status(), 200);

    for path in [
        "/api/pages/custom/unknown/_rpc/loader/list",
        "/api/pages/custom/tools/extra/_rpc/loader/list",
        "/api/pages/custom/tools/_rpc/loader/missing",
        // A loader cannot be called as an action, nor an action as a loader.
        "/api/pages/custom/tools/_rpc/action/list",
        "/api/pages/custom/tools/_rpc/loader/save",
        "/api/pages/custom/tools/_rpc/query/list",
        "/api/pages/custom/tools/_rpc/loader/list/extra",
        "/api/pages/custom/tools/_rpc/loader/",
        "/api/pages/custom/tools/_rpc/loader",
        "/api/pages/custom/tools/_rpc",
        "/api/pages/custom/tools",
        "/api/pages/custom/_rpc/loader/list",
    ] {
        let response = request_as(&ctx, REVIEWER, Method::POST, path)
            .json(&json!({ "input": null }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 404, "{path}");
        let body = json_body(response).await;
        assert_eq!(body["code"], "NOT_FOUND", "{path}");
        assert_eq!(body["error"], "Not found", "{path}");
        assert!(body["requestId"].is_string(), "{path}");
    }

    // The endpoint only answers POST.
    let response = request_as(&ctx, REVIEWER, Method::GET, "/api/pages/custom/tools/_rpc/loader/list")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn function_authorize_rejects_with_403() {
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_calls = calls.clone();
    let publish = AdminFunction::action(
        InputSchema::custom(json!({ "type": "boolean" }), |value| {
            value
                .is_boolean()
                .then_some(value)
                .ok_or_else(|| "must be a boolean".to_string())
        }),
        move |_ctx, input| {
            handler_calls.fetch_add(1, Ordering::SeqCst);
            async move { Ok(json!({ "published": input })) }
        },
    )
    .authorize(|ctx| async move { Ok(ctx.actor.id == REVIEWER) });
    let page = AdminPage::new("/tools")
        .loader("list", |_ctx, _input| async move { Ok(json!([])) })
        .function("publish", publish);
    let ctx = context(tools_app(page)).await;

    // The page itself is open to everyone.
    let listed = rpc(&ctx, OTHER_STAFF, "/tools", "loader", "list", Value::Null)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), 200);

    let response = rpc(&ctx, OTHER_STAFF, "/tools", "action", "publish", json!(true))
        .send()
        .await
        .unwrap();
    assert_rpc_error(response, 403, "FORBIDDEN", "Forbidden").await;

    // Authorization comes before input validation.
    let response = rpc(&ctx, OTHER_STAFF, "/tools", "action", "publish", json!("yes"))
        .send()
        .await
        .unwrap();
    assert_rpc_error(response, 403, "FORBIDDEN", "Forbidden").await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let response = act(&ctx, "/tools", "publish", json!(true)).await;
    assert_eq!(response.status(), 200);
    assert_eq!(json_body(response).await, json!({ "published": true }));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
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

    let saved = act(&ctx, "/tools", "save", json!({ "title": "Great", "rating": 5 })).await;
    assert_eq!(saved.status(), 200);
    assert_eq!(
        json_body(saved).await,
        json!({ "title": "Great", "rating": 5, "savedBy": "123" })
    );

    for input in [
        json!({ "title": "", "rating": 5 }),
        json!({ "title": "Great" }),
        json!({ "title": "Great", "rating": "five" }),
        json!({ "title": "Great", "rating": 500 }),
        json!("Great"),
        Value::Null,
    ] {
        let response = act(&ctx, "/tools", "save", input.clone()).await;
        assert_eq!(response.status(), 422, "{input}");
        let body = json_body(response).await;
        assert_eq!(body["code"], "INVALID_INPUT", "{input}");
        assert!(
            body["error"].as_str().is_some_and(|summary| !summary.is_empty()),
            "{body}"
        );
        assert!(body["requestId"].is_string(), "{body}");
    }

    // A body without `input`, or no JSON body at all, is a missing input.
    let url = "/api/pages/custom/tools/_rpc/action/save";
    for request in [
        request_as(&ctx, REVIEWER, Method::POST, url).json(&json!({ "title": "Great", "rating": 5 })),
        request_as(&ctx, REVIEWER, Method::POST, url).body("{ not json"),
        request_as(&ctx, REVIEWER, Method::POST, url),
    ] {
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), 422);
        assert_eq!(json_body(response).await["code"], "INVALID_INPUT");
    }

    // The handler receives what the schema returns.
    let found = load(&ctx, "/tools", "search", json!("  rust  ")).await;
    assert_eq!(found.status(), 200);
    assert_eq!(json_body(found).await, json!({ "query": "rust" }));
    let response = load(&ctx, "/tools", "search", json!("   ")).await;
    assert_rpc_error(response, 422, "INVALID_INPUT", "query must be a non-empty string").await;

    // Loaders without a schema get the input untouched.
    let echoed = load(&ctx, "/tools", "echo", json!({ "page": 2, "tags": ["a", "b"] })).await;
    assert_eq!(
        json_body(echoed).await,
        json!({ "input": { "page": 2, "tags": ["a", "b"] } })
    );
    let response = request_as(&ctx, REVIEWER, Method::POST, "/api/pages/custom/tools/_rpc/loader/echo")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(json_body(response).await, json!({ "input": null }));
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

    let response = act(&ctx, "/tools", "unchecked", json!({ "anything": true })).await;
    assert_rpc_error(response, 500, "INVALID_ACTION", "Action input schema is required").await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn public_errors_expose_their_status_code_and_field_errors() {
    let page = AdminPage::new("/tools")
        .loader("taken", |_ctx, _input| async move {
            Err(AdminPublicError::new("TITLE_TAKEN", "This title is already used")
                .status(409)
                .field_errors([("title", "Already used"), ("slug", "Already used")])
                .into())
        })
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
        );
    let ctx = context(tools_app(page)).await;

    let response = load(&ctx, "/tools", "taken", Value::Null).await;
    assert_eq!(response.status(), 409);
    let body = json_body(response).await;
    let request_id = body["requestId"].as_str().expect("requestId");
    assert_eq!(
        body,
        json!({
            "error": "This title is already used",
            "code": "TITLE_TAKEN",
            "requestId": request_id,
            "fieldErrors": { "title": "Already used", "slug": "Already used" },
        })
    );

    // 400 by default, without `fieldErrors`; the id is the one the function saw.
    let response = load(&ctx, "/tools", "plain", Value::Null).await;
    assert_eq!(response.status(), 400);
    let body = json_body(response).await;
    assert_eq!(body["code"], "REQUEST");
    assert_eq!(body["error"], body["requestId"]);
    assert_eq!(body.get("fieldErrors"), None);
    assert_ne!(body["requestId"], request_id);

    let response = load(&ctx, "/tools", "premium", Value::Null).await;
    assert_rpc_error(response, 402, "PLAN_REQUIRED", "Upgrade to use this feature").await;
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn unexpected_errors_return_500_without_leaking_details() {
    let page = AdminPage::new("/tools")
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
        .loader("works", |_ctx, _input| async move { Ok(json!({ "ok": true })) });
    let ctx = context(tools_app(page)).await;

    for name in ["fails", "panics", "guarded"] {
        let response = load(&ctx, "/tools", name, Value::Null).await;
        assert_eq!(response.status(), 500, "{name}");
        let text = response.text().await.unwrap();
        assert!(!text.contains("hunter2"), "{text}");
        let body: Value = serde_json::from_str(&text).unwrap();
        let request_id = body["requestId"].as_str().expect("requestId");
        assert_eq!(
            body,
            json!({ "error": "Internal server error", "code": "INTERNAL_ERROR", "requestId": request_id }),
            "{name}"
        );
    }

    let response = load(&ctx, "/tools", "works", Value::Null).await;
    assert_eq!(response.status(), 200);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn output_validator_failure_returns_500() {
    let count = AdminFunction::loader(|_ctx, input| async move { Ok(json!({ "count": input })) }).output(|output| {
        output["count"]
            .is_u64()
            .then_some(())
            .ok_or_else(|| "count must be a positive integer (secret detail)".to_string())
    });
    let ctx = context(tools_app(AdminPage::new("/tools").function("count", count))).await;

    let response = load(&ctx, "/tools", "count", json!(3)).await;
    assert_eq!(response.status(), 200);
    assert_eq!(json_body(response).await, json!({ "count": 3 }));

    let response = load(&ctx, "/tools", "count", json!("three")).await;
    assert_eq!(response.status(), 500);
    let text = response.text().await.unwrap();
    assert!(!text.contains("secret detail"), "{text}");
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["code"], "INTERNAL_ERROR");
    assert_eq!(body["error"], "Internal server error");
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn decodes_dynamic_route_params_and_function_names() {
    let params = |ctx: AdminContext, _input: Value| async move { Ok(json!(ctx.params)) };
    let config = OpenShopConfig::builder()
        .admin_page(
            AdminPage::new("/reviews/[id]")
                .loader("params", params)
                .loader("review details", |_ctx, _input| async move { Ok(json!("by name")) }),
        )
        .admin_page(AdminPage::new("/reviews/:id/comments/:comment_id").loader("params", params))
        .admin_menu([])
        .build()
        .unwrap();
    let ctx = context(config).await;

    let response = load(&ctx, "/reviews/42", "params", Value::Null).await;
    assert_eq!(json_body(response).await, json!({ "id": "42" }));

    let response = load(&ctx, "/reviews/caf%C3%A9%20noir", "params", Value::Null).await;
    assert_eq!(json_body(response).await, json!({ "id": "café noir" }));

    let response = load(&ctx, "/reviews/42/comments/7", "params", Value::Null).await;
    assert_eq!(json_body(response).await, json!({ "id": "42", "comment_id": "7" }));

    let url = build_admin_function_rpc_url("loader", "review details", "/reviews/1");
    assert_eq!(url, "/api/pages/custom/reviews/1/_rpc/loader/review%20details");
    let response = load(&ctx, "/reviews/1", "review details", Value::Null).await;
    assert_eq!(response.status(), 200);
    assert_eq!(json_body(response).await, json!("by name"));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn exposes_the_idempotency_key_and_request_id() {
    let page = AdminPage::new("/tools").function(
        "charge",
        AdminFunction::action(InputSchema::custom(json!({}), Ok), |ctx, _input| async move {
            Ok(json!({ "idempotencyKey": ctx.idempotency_key, "requestId": ctx.request_id }))
        }),
    );
    let ctx = context(tools_app(page)).await;

    let response = rpc(&ctx, REVIEWER, "/tools", "action", "charge", Value::Null)
        .header("Idempotency-Key", "charge-2026-0001")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let first = json_body(response).await;
    assert_eq!(first["idempotencyKey"], "charge-2026-0001");

    let second = json_body(act(&ctx, "/tools", "charge", Value::Null).await).await;
    assert_eq!(second["idempotencyKey"], Value::Null);

    // Every call gets its own request id.
    for body in [&first, &second] {
        let request_id = body["requestId"].as_str().expect("requestId");
        assert!(uuid::Uuid::parse_str(request_id).is_ok(), "{request_id}");
    }
    assert_ne!(first["requestId"], second["requestId"]);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn takes_the_actor_from_the_session_token() {
    let page = AdminPage::new("/tools").loader("whoami", |ctx, _input| async move { Ok(json!(ctx.actor)) });
    let ctx = context(tools_app(page)).await;

    for sub in [REVIEWER, OTHER_STAFF] {
        let response = rpc(&ctx, sub, "/tools", "loader", "whoami", Value::Null)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(json_body(response).await, json!({ "id": sub, "sessionId": "test-sid" }));
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
    let response = ctx
        .http
        .post(format!("{}/api/pages/custom/tools/_rpc/loader/whoami", ctx.url))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        json_body(response).await,
        json!({ "id": "789", "sessionId": "custom-page-session" })
    );
    ctx.shutdown().await.unwrap();
}

fn vault_provider() -> Provider {
    Provider::new("Vault")
        .field("secret", Field::password("Secret"))
        .typed_method("reveal", |config, prefix: String| async move {
            let secret = config.get("secret").and_then(Value::as_str).unwrap_or_default();
            Ok(format!("{prefix}:{secret}"))
        })
}

/// Reads everything a function can reach for its tenant.
async fn tenant(ctx: AdminContext, _input: Value) -> Result<Value> {
    let secret: String = ctx.connectors.call_as("vault", "reveal", "vault").await?;
    let shopify = ctx.shopify.graphql("{ shop { name } }", None).await?;
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

async fn install(ctx: &TestContext, app_handle: &str, shop: &str, access_token: &str) {
    sqlx::query("INSERT INTO installations (app_handle, shop, access_token) VALUES ($1, $2, $3)")
        .bind(app_handle)
        .bind(shop)
        .bind(access_token)
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
        .admin_page(AdminPage::new("/tools").loader("tenant", tenant))
        .admin_menu([])
        .build()
        .unwrap();
    let ctx = context(config).await;
    answer_with_shop_name(&ctx.transport);
    install(&ctx, "default", OTHER_SHOP, "other-access-token").await;
    store_vault_secret(&ctx, "default", TEST_SHOP, "first-secret").await;
    store_vault_secret(&ctx, "default", OTHER_SHOP, "other-secret").await;

    let response = load(&ctx, "/tools", "tenant", Value::Null).await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        json_body(response).await,
        json!({
            "shop": TEST_SHOP,
            "shopifyApp": "default",
            "secret": "vault:first-secret",
            "shopName": format!("Shop {TEST_SHOP}"),
            "clientShop": TEST_SHOP,
            "installations": 2,
        })
    );

    let response = ctx
        .http
        .post(format!(
            "{}/api/pages/custom/tools/_rpc/loader/tenant?shop={TEST_SHOP}",
            ctx.url
        ))
        .bearer_auth(ctx.session_token_for(OTHER_SHOP, REVIEWER))
        .json(&json!({ "input": { "shop": TEST_SHOP } }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = json_body(response).await;
    assert_eq!(body["shop"], OTHER_SHOP);
    assert_eq!(body["secret"], "vault:other-secret");
    assert_eq!(body["shopName"], format!("Shop {OTHER_SHOP}"));
    assert_eq!(body["clientShop"], OTHER_SHOP);

    assert_eq!(access_token_sent_to(&ctx, TEST_SHOP), ["test-access-token"]);
    assert_eq!(access_token_sent_to(&ctx, OTHER_SHOP), ["other-access-token"]);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn resolves_the_shopify_app_from_the_token_audience() {
    let app = |api_key: &str, api_secret: &str| ShopifyAppConfig::Keys {
        api_key: api_key.to_string(),
        api_secret: api_secret.to_string(),
        app_url: None,
    };
    let config = OpenShopConfig::builder()
        .shopify(ShopifyConfig {
            scopes: Some("read_products".to_string()),
            apps: Some(
                [
                    ("clientA".to_string(), app("client-a-key", "client-a-secret")),
                    ("clientB".to_string(), app("client-b-key", "client-b-secret")),
                ]
                .into(),
            ),
        })
        .provider_as("vault", vault_provider())
        .admin_page(AdminPage::new("/tools").loader("tenant", tenant))
        .admin_menu([])
        .build()
        .unwrap();
    let ctx = TestContext::new(config).await.unwrap();
    answer_with_shop_name(&ctx.transport);
    // The same shop installed both apps, each with its own token and configuration.
    install(&ctx, "clientA", TEST_SHOP, "token-a").await;
    install(&ctx, "clientB", TEST_SHOP, "token-b").await;
    store_vault_secret(&ctx, "clientA", TEST_SHOP, "secret-a").await;
    store_vault_secret(&ctx, "clientB", TEST_SHOP, "secret-b").await;
    let url = format!("{}/api/pages/custom/tools/_rpc/loader/tenant", ctx.url);

    for (handle, api_key, api_secret, secret) in [
        ("clientA", "client-a-key", "client-a-secret", "vault:secret-a"),
        ("clientB", "client-b-key", "client-b-secret", "vault:secret-b"),
    ] {
        let token = session_token(TEST_SHOP, api_secret, api_key, REVIEWER);
        let response = ctx.http.post(&url).bearer_auth(token).send().await.unwrap();
        assert_eq!(response.status(), 200, "{handle}");
        let body = json_body(response).await;
        assert_eq!(body["shopifyApp"], handle);
        assert_eq!(body["secret"], secret);
    }
    assert_eq!(access_token_sent_to(&ctx, TEST_SHOP), ["token-a", "token-b"]);

    // A token signed with another app's secret is not a session.
    let token = session_token(TEST_SHOP, "client-a-secret", "client-b-key", REVIEWER);
    let response = ctx.http.post(&url).bearer_auth(token).send().await.unwrap();
    assert_eq!(response.status(), 401);
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
