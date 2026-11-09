//! Public server routes under `/routes`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::common::{install, json_body, two_shopify_apps, vault_provider};
use openshop::builder::{OpenShopBuilder, Route};
use openshop::response::{self, IntoResponse, Response, StatusCode};
use openshop::server::http::parse_query;
use openshop::testing::{TestContext, TestOptions};
use openshop::{IndexMap, OpenShopConfig, Result, RouteAuthOutcome, RouteContext, RouteShopInput, Value, anyhow, json};
use reqwest::{Method, RequestBuilder};

const ENCRYPTION_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

async fn open(ctx: RouteContext) -> Result<Response> {
    Ok((
        StatusCode::CREATED,
        [("x-server-route", "open")],
        response::json(json!({ "route": "open", "params": ctx.params })),
    )
        .into_response())
}

fn open_route() -> Route {
    Route::public().get(open)
}

fn authenticated_route() -> Route {
    Route::authenticated(|ctx| async move {
        let body: Value = ctx.request.json()?;
        Ok(match body["token"].as_str() {
            Some("valid") => RouteAuthOutcome::Authenticated(json!({ "subject": "provider-webhook" })),
            _ => RouteAuthOutcome::Unauthorized,
        })
    })
    .post(|ctx| async move {
        let body: Value = ctx.request.json()?;
        Ok(response::json(json!({ "auth": ctx.auth, "body": body })))
    })
}

/// `?shop=…&app=…` of a request as the input of `for_shop`.
fn shop_input(query: &IndexMap<String, String>) -> RouteShopInput {
    let input = RouteShopInput::new(query.get("shop").cloned().unwrap_or_default());
    match query.get("app") {
        Some(handle) => input.shopify_app(handle),
        None => input,
    }
}

/// Authenticates a shop, then reads a secret through its provider connector.
fn provider_route() -> Route {
    Route::authenticated(|ctx| async move {
        let query = parse_query(ctx.request.uri.query());
        let Ok(shop_context) = ctx.for_shop(shop_input(&query)).await else {
            return Ok(RouteAuthOutcome::Unauthorized);
        };
        if query.get("token").map(String::as_str) != Some("valid") {
            return Ok(RouteAuthOutcome::Unauthorized);
        }
        let value: String = shop_context.connectors.call_as("vault", "reveal", "value").await?;
        Ok(RouteAuthOutcome::Authenticated(json!({
            "shop": shop_context.shop,
            "shopifyApp": shop_context.shopify_app,
            "value": value,
        })))
    })
    .post(|ctx| async move { Ok(response::json(ctx.auth)) })
}

/// Reports what `for_shop` resolves, or why it refuses.
fn shop_context_route() -> Route {
    Route::public().get(|ctx| async move {
        let query = parse_query(ctx.request.uri.query());
        Ok(match ctx.for_shop(shop_input(&query)).await {
            Ok(shop_context) => response::json(json!({
                "shop": shop_context.shop,
                "shopifyApp": shop_context.shopify_app,
                "providers": shop_context.connectors.names().collect::<Vec<_>>(),
            })),
            Err(error) => response::json_with_status(StatusCode::BAD_REQUEST, json!({ "error": error.to_string() })),
        })
    })
}

/// Reports the links to the admin a route gives for the shop it names,
/// installed or not, or why it refuses.
fn admin_links_route() -> Route {
    Route::public().get(|ctx| async move {
        let input = shop_input(&parse_query(ctx.request.uri.query()));
        let links = (|| -> Result<Value> {
            let admin_url = ctx.admin_url(input.clone(), "settings/payments")?;
            // A link that needs the handle of the app, or the error that says to add it.
            let or_error = |link: Result<String>| link.unwrap_or_else(|error| error.to_string());
            Ok(json!({
                "appHandle": or_error(ctx.app_handle(input.clone()).map(str::to_string)),
                "adminUrl": admin_url,
                "pricingUrl": or_error(ctx.pricing_url(input.clone())),
            }))
        })();
        Ok(match links {
            Ok(links) => response::json(links),
            Err(error) => response::json_with_status(StatusCode::BAD_REQUEST, json!({ "error": error.to_string() })),
        })
    })
}

fn config() -> OpenShopConfig {
    OpenShopConfig::builder()
        .shopify(two_shopify_apps())
        .provider_as("vault", vault_provider())
        .route("/", open_route())
        .route("/authenticated", authenticated_route())
        .route("/provider", provider_route())
        .route("/shop-context", shop_context_route())
        .route("/admin-links", admin_links_route())
        .route("/orders/[id]", open_route())
        .route(
            "/orders/new",
            Route::public().get(|_ctx| async move { Ok(response::json(json!({ "route": "new" }))) }),
        )
        .route(
            "/throws",
            Route::public()
                .get(|_ctx| async move { Err(anyhow!("route failure")) })
                .post(|ctx| async move {
                    if ctx.request.body.is_empty() {
                        panic!("route handler panic");
                    }
                    Ok(response::text("recovered"))
                }),
        )
        .route(
            "/auth-throws",
            // Never authenticates: the type of the value has to be stated.
            Route::<Value>::authenticated(|ctx| async move {
                if ctx.request.header("x-panic").is_some() {
                    panic!("route authentication panic");
                }
                Err(anyhow!("authentication failure"))
            })
            .post(|_ctx| async move { Ok(response::text("unreachable")) }),
        )
        .build()
        .expect("valid config")
}

fn request(ctx: &TestContext, method: Method, path: &str) -> RequestBuilder {
    ctx.http.request(method, format!("{}/routes{path}", ctx.url))
}

fn get(ctx: &TestContext, path: &str) -> RequestBuilder {
    request(ctx, Method::GET, path)
}

fn post(ctx: &TestContext, path: &str) -> RequestBuilder {
    request(ctx, Method::POST, path)
}

fn header<'a>(response: &'a reqwest::Response, name: &str) -> Option<&'a str> {
    response.headers().get(name).and_then(|value| value.to_str().ok())
}

async fn store_vault_secret(ctx: &TestContext, app_handle: &str, shop: &str, secret: &str) {
    let config = json!({ "secret": secret });
    let encrypted = ctx.app.crypto().encrypt_config(config.as_object().unwrap()).unwrap();
    assert!(
        encrypted.get("secret").is_none(),
        "provider configs are stored encrypted"
    );
    sqlx::query(
        "INSERT INTO provider_configs (app_handle, shop, provider_name, config) VALUES ($1, $2, 'vault', $3::json)",
    )
    .bind(app_handle)
    .bind(shop)
    .bind(encrypted.to_string())
    .execute(&ctx.db)
    .await
    .unwrap();
}

#[openshop::test(TestContext::new(config()))]
async fn mounts_index_and_dynamic_routes_under_routes(ctx: &TestContext) {
    for path in ["", "/"] {
        let root = get(ctx, path).send().await.unwrap();
        assert_eq!(root.status(), 201);
        assert_eq!(header(&root, "x-server-route"), Some("open"));
        assert_eq!(json_body(root).await, json!({ "route": "open", "params": {} }));
    }

    let dynamic = get(ctx, "/orders/123").send().await.unwrap();
    assert_eq!(dynamic.status(), 201);
    assert_eq!(json_body(dynamic).await["params"], json!({ "id": "123" }));

    let encoded = get(ctx, "/orders/order%20%2342").send().await.unwrap();
    assert_eq!(json_body(encoded).await["params"], json!({ "id": "order #42" }));

    // The static route wins over the dynamic one registered before it.
    let static_route = get(ctx, "/orders/new").send().await.unwrap();
    assert_eq!(static_route.status(), 200);
    assert_eq!(json_body(static_route).await, json!({ "route": "new" }));

    assert_eq!(get(ctx, "/missing").send().await.unwrap().status(), 404);
    assert_eq!(get(ctx, "/orders/123/items").send().await.unwrap().status(), 404);
}

#[openshop::test(TestContext::new(config()))]
async fn returns_405_and_allow_for_a_method_without_a_handler(ctx: &TestContext) {
    let response = post(ctx, "").send().await.unwrap();
    assert_eq!(response.status(), 405);
    assert_eq!(header(&response, "allow"), Some("GET"));
    assert_eq!(json_body(response).await, json!({ "error": "Method not allowed" }));

    // HEAD is not implied by GET.
    let head = request(ctx, Method::HEAD, "").send().await.unwrap();
    assert_eq!(head.status(), 405);
    assert_eq!(header(&head, "allow"), Some("GET"));

    let options = request(ctx, Method::OPTIONS, "/orders/123").send().await.unwrap();
    assert_eq!(options.status(), 405);

    // Declared methods are listed in a fixed order, and the check comes before authentication.
    let response = request(ctx, Method::DELETE, "/throws").send().await.unwrap();
    assert_eq!(response.status(), 405);
    assert_eq!(header(&response, "allow"), Some("GET, POST"));

    let response = get(ctx, "/auth-throws").send().await.unwrap();
    assert_eq!(response.status(), 405);
    assert_eq!(header(&response, "allow"), Some("POST"));
}

#[tokio::test]
async fn every_http_method_can_be_handled() {
    let describe = |ctx: RouteContext| async move {
        let method = ctx.request.method.to_string();
        Ok((
            [("x-method", method.clone())],
            response::json(json!({ "method": method })),
        )
            .into_response())
    };
    let config = OpenShopConfig::builder()
        .route(
            "/any",
            Route::public()
                .options(describe)
                .delete(describe)
                .patch(describe)
                .put(describe)
                .post(describe)
                .head(describe)
                .get(describe),
        )
        .build()
        .unwrap();
    let ctx = TestContext::new(config).await.unwrap();

    for method in [
        Method::GET,
        Method::POST,
        Method::PUT,
        Method::PATCH,
        Method::DELETE,
        Method::OPTIONS,
    ] {
        let response = request(&ctx, method.clone(), "/any").send().await.unwrap();
        assert_eq!(response.status(), 200, "{method}");
        assert_eq!(json_body(response).await, json!({ "method": method.as_str() }));
    }

    let head = request(&ctx, Method::HEAD, "/any").send().await.unwrap();
    assert_eq!(head.status(), 200);
    assert_eq!(header(&head, "x-method"), Some("HEAD"));

    let other = request(&ctx, Method::from_bytes(b"PROPFIND").unwrap(), "/any")
        .send()
        .await
        .unwrap();
    assert_eq!(other.status(), 405);
    assert_eq!(
        header(&other, "allow"),
        Some("GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS")
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn handlers_receive_the_full_request() {
    let config = OpenShopConfig::builder()
        .route(
            "/inspect/:section",
            Route::public().put(|ctx| async move {
                let query = parse_query(ctx.request.uri.query());
                Ok(response::json(json!({
                    "method": ctx.request.method.as_str(),
                    "path": ctx.request.uri.path(),
                    "query": query,
                    "params": ctx.params,
                    "signature": ctx.request.header("X-Provider-Signature"),
                    "missing": ctx.request.header("x-missing"),
                    "text": ctx.request.text(),
                    "auth": ctx.auth,
                })))
            }),
        )
        .build()
        .unwrap();
    let ctx = TestContext::new(config).await.unwrap();

    let response = request(&ctx, Method::PUT, "/inspect/stock?sku=A-1&warehouse=paris%20nord")
        .header("x-provider-signature", "sha256=abc")
        .body("raw payload")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        json_body(response).await,
        json!({
            "method": "PUT",
            "path": "/routes/inspect/stock",
            "query": { "sku": "A-1", "warehouse": "paris nord" },
            "params": { "section": "stock" },
            "signature": "sha256=abc",
            "missing": null,
            "text": "raw payload",
            "auth": null,
        })
    );
    ctx.shutdown().await.unwrap();
}

#[openshop::test(TestContext::new(config()))]
async fn auth_can_read_the_body_and_passes_its_result_to_the_handler(ctx: &TestContext) {
    let response = post(ctx, "/authenticated")
        .json(&json!({ "token": "valid", "event": "created" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        json_body(response).await,
        json!({
            "auth": { "subject": "provider-webhook" },
            "body": { "token": "valid", "event": "created" },
        })
    );
}

#[tokio::test]
async fn auth_unauthorized_returns_401_and_an_auth_response_short_circuits() {
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_calls = calls.clone();
    let config = OpenShopConfig::builder()
        .route("/authenticated", authenticated_route())
        .route(
            "/forbidden",
            Route::authenticated(|ctx| async move {
                Ok(match ctx.request.header("x-api-key") {
                    Some("letmein") => RouteAuthOutcome::Authenticated(Value::Null),
                    Some(_) => RouteAuthOutcome::Response((StatusCode::FORBIDDEN, "Forbidden").into_response()),
                    None => RouteAuthOutcome::Unauthorized,
                })
            })
            .post(move |_ctx| {
                let calls = handler_calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(response::text("reached"))
                }
            }),
        )
        .build()
        .unwrap();
    let ctx = TestContext::new(config).await.unwrap();

    let unauthorized = post(&ctx, "/authenticated")
        .json(&json!({ "token": "invalid" }))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), 401);
    assert_eq!(json_body(unauthorized).await, json!({ "error": "Unauthorized" }));

    let unauthorized = post(&ctx, "/forbidden").send().await.unwrap();
    assert_eq!(unauthorized.status(), 401);

    let forbidden = post(&ctx, "/forbidden")
        .header("x-api-key", "wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden.status(), 403);
    assert_eq!(forbidden.text().await.unwrap(), "Forbidden");
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let allowed = post(&ctx, "/forbidden")
        .header("x-api-key", "letmein")
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), 200);
    assert_eq!(allowed.text().await.unwrap(), "reached");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    ctx.shutdown().await.unwrap();
}

#[openshop::test(TestContext::new(config()))]
async fn authentication_and_handler_errors_fail_with_500(ctx: &TestContext) {
    let internal_error = json!({ "error": "Internal server route error" });

    let auth_failed = post(ctx, "/auth-throws").send().await.unwrap();
    assert_eq!(auth_failed.status(), 500);
    assert_eq!(json_body(auth_failed).await, internal_error);

    let failed = get(ctx, "/throws").send().await.unwrap();
    assert_eq!(failed.status(), 500);
    assert_eq!(json_body(failed).await, internal_error);

    // An unreadable body in authentication is an error, not a rejection.
    let malformed = post(ctx, "/authenticated").body("{ not json").send().await.unwrap();
    assert_eq!(malformed.status(), 500);
    assert_eq!(json_body(malformed).await, internal_error);

    // Panics are contained the same way.
    let auth_panicked = post(ctx, "/auth-throws").header("x-panic", "1").send().await.unwrap();
    assert_eq!(auth_panicked.status(), 500);
    assert_eq!(json_body(auth_panicked).await, internal_error);

    let panicked = post(ctx, "/throws").send().await.unwrap();
    assert_eq!(panicked.status(), 500);
    assert_eq!(json_body(panicked).await, internal_error);

    let recovered = post(ctx, "/throws").body("payload").send().await.unwrap();
    assert_eq!(recovered.status(), 200);
    assert_eq!(recovered.text().await.unwrap(), "recovered");
}

#[tokio::test]
async fn for_shop_loads_only_the_active_app_and_shop_connectors() {
    let options = TestOptions {
        encryption_key: Some(ENCRYPTION_KEY.to_string()),
        ..Default::default()
    };
    let ctx = TestContext::with_options(config(), options).await.unwrap();
    install(&ctx.db, "clientA", "a.myshopify.com", Some("token-a"), false).await;
    install(&ctx.db, "clientB", "b.myshopify.com", Some("token-b"), false).await;
    install(&ctx.db, "clientB", "inactive.myshopify.com", None, true).await;
    store_vault_secret(&ctx, "clientA", "a.myshopify.com", "secret-a").await;
    store_vault_secret(&ctx, "clientB", "b.myshopify.com", "secret-b").await;
    store_vault_secret(&ctx, "clientB", "inactive.myshopify.com", "stale").await;

    let active = post(&ctx, "/provider?shop=b.myshopify.com&app=clientB&token=valid")
        .send()
        .await
        .unwrap();
    assert_eq!(active.status(), 200);
    assert_eq!(
        json_body(active).await,
        json!({ "shop": "b.myshopify.com", "shopifyApp": "clientB", "value": "value:secret-b" })
    );

    let other_app = post(&ctx, "/provider?shop=a.myshopify.com&app=clientA&token=valid")
        .send()
        .await
        .unwrap();
    assert_eq!(json_body(other_app).await["value"], "value:secret-a");

    for rejected in [
        "/provider?shop=inactive.myshopify.com&app=clientB&token=valid",
        "/provider?shop=not-a-shop&app=clientB&token=valid",
        "/provider?shop=b.myshopify.com&app=unknown&token=valid",
        // Installed for clientB only.
        "/provider?shop=b.myshopify.com&app=clientA&token=valid",
        // Several apps are configured: the handle is required.
        "/provider?shop=b.myshopify.com&token=valid",
        "/provider?shop=b.myshopify.com&app=clientB&token=wrong",
    ] {
        let response = post(&ctx, rejected).send().await.unwrap();
        assert_eq!(response.status(), 401, "{rejected}");
        assert_eq!(json_body(response).await, json!({ "error": "Unauthorized" }));
    }
    ctx.shutdown().await.unwrap();
}

#[openshop::test(TestContext::new(config()))]
async fn for_shop_requires_a_valid_and_actively_installed_shop(ctx: &TestContext) {
    install(&ctx.db, "clientB", "b.myshopify.com", Some("token-b"), false).await;
    install(&ctx.db, "clientB", "uninstalled.myshopify.com", Some("token"), true).await;
    install(&ctx.db, "clientB", "pending.myshopify.com", None, false).await;

    let resolved = get(
        ctx,
        "/shop-context?shop=https%3A%2F%2FB.MyShopify.com%2Fadmin&app=clientB",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(resolved.status(), 200);
    assert_eq!(
        json_body(resolved).await,
        json!({ "shop": "b.myshopify.com", "shopifyApp": "clientB", "providers": ["vault"] })
    );

    for (query, error) in [
        ("shop=not-a-shop&app=clientB", "[openshop] Invalid shop domain"),
        (
            "shop=b.myshopify.com.evil.com&app=clientB",
            "[openshop] Invalid shop domain",
        ),
        ("app=clientB", "[openshop] Invalid shop domain"),
        (
            "shop=unknown.myshopify.com&app=clientB",
            "[openshop] Shop is not actively installed",
        ),
        (
            "shop=uninstalled.myshopify.com&app=clientB",
            "[openshop] Shop is not actively installed",
        ),
        (
            "shop=pending.myshopify.com&app=clientB",
            "[openshop] Shop is not actively installed",
        ),
        (
            "shop=b.myshopify.com&app=clientA",
            "[openshop] Shop is not actively installed",
        ),
        (
            "shop=b.myshopify.com&app=unknown",
            "[openshop] Unknown Shopify app \"unknown\"",
        ),
        ("shop=b.myshopify.com", "[openshop] Missing Shopify app handle"),
    ] {
        let response = get(ctx, &format!("/shop-context?{query}")).send().await.unwrap();
        assert_eq!(response.status(), 400, "{query}");
        assert_eq!(json_body(response).await, json!({ "error": error }), "{query}");
    }
}

#[tokio::test]
async fn for_shop_defaults_to_the_only_configured_app() {
    let config = OpenShopConfig::builder()
        .provider_as("vault", vault_provider())
        .route("/shop-context", shop_context_route())
        .route("/admin-links", admin_links_route())
        .route("/provider", provider_route())
        .build()
        .unwrap();
    let options = TestOptions {
        access_token: Some("test-access-token".to_string()),
        ..Default::default()
    };
    let ctx = TestContext::with_options(config, options).await.unwrap();

    let resolved = get(&ctx, &format!("/shop-context?shop={}", ctx.shop))
        .send()
        .await
        .unwrap();
    assert_eq!(resolved.status(), 200);
    assert_eq!(
        json_body(resolved).await,
        json!({ "shop": ctx.shop, "shopifyApp": "default", "providers": ["vault"] })
    );

    // Without a stored configuration the provider method sees an empty one.
    let response = post(&ctx, &format!("/provider?shop={}&token=valid", ctx.shop))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(json_body(response).await["value"], "value:");
    ctx.shutdown().await.unwrap();
}

#[test]
fn rejects_routes_without_handlers_and_ambiguous_routes_when_the_config_is_built() {
    let error_of = |builder: OpenShopBuilder| builder.build().err().expect("invalid config").to_string();

    let error = error_of(OpenShopConfig::builder().route("/missing-handler", Route::public()));
    assert_eq!(
        error,
        "[openshop] Invalid server route /missing-handler: define at least one HTTP method"
    );

    let error = error_of(OpenShopConfig::builder().route(
        "/missing-handler",
        Route::<Value>::authenticated(|_ctx| async move { Ok(RouteAuthOutcome::Unauthorized) }),
    ));
    assert!(error.contains("define at least one HTTP method"), "{error}");

    let error = error_of(
        OpenShopConfig::builder()
            .route("/orders/:id", open_route())
            .route("/orders/[slug]", open_route()),
    );
    assert!(error.contains("have the same route shape"), "{error}");

    let error = error_of(
        OpenShopConfig::builder()
            .route("/orders", open_route())
            .route("orders/", open_route()),
    );
    assert!(error.contains("have the same route shape"), "{error}");

    let error = error_of(OpenShopConfig::builder().route("/orders/:order-id", open_route()));
    assert!(error.contains("parameter names"), "{error}");
}

#[openshop::test(TestContext::new(config()))]
async fn links_to_the_admin_of_a_shop_need_no_installation(ctx: &TestContext) {
    let links = |query: &str| {
        let request = get(ctx, &format!("/admin-links?{query}"));
        async move {
            let response = request.send().await.unwrap();
            (response.status().as_u16(), json_body(response).await)
        }
    };
    let shop = "shop=Links-Demo.myshopify.com&app=clientB";

    // `for_shop` refuses a shop that has not installed the app; its links are there.
    let refused = json_body(get(ctx, &format!("/shop-context?{shop}")).send().await.unwrap()).await;
    assert_eq!(refused["error"], "[openshop] Shop is not actively installed");
    // No `shopify.app.toml`: no handle, and not the API key in its place.
    let no_handle = "[openshop] The Shopify app \"clientB\" has no handle: add `handle = \"<your-app-handle>\"` \
                     at the top of the shopify.app*.toml whose client_id is \"client-b-key\", next to client_id \
                     (`shopify app config link` writes it). The pages of the Shopify admin, such as the plan \
                     selection page, name the app by its handle, and Shopify does not take the client ID there.";
    assert_eq!(
        links(shop).await,
        (
            200,
            json!({
                "appHandle": no_handle,
                "adminUrl": "https://admin.shopify.com/store/links-demo/settings/payments",
                "pricingUrl": no_handle,
            })
        )
    );
    let client_a = links("shop=links-demo.myshopify.com&app=clientA").await.1;
    assert!(
        client_a["appHandle"]
            .as_str()
            .unwrap()
            .contains("\"clientA\" has no handle"),
        "{client_a}"
    );

    // The shop and the app are checked as `for_shop` checks them.
    for (query, error) in [
        ("shop=links-demo.myshopify.com", "[openshop] Missing Shopify app handle"),
        (
            "shop=links-demo.myshopify.com&app=nope",
            "[openshop] Unknown Shopify app \"nope\"",
        ),
        ("shop=links.example.com&app=clientA", "[openshop] Invalid shop domain"),
        ("app=clientA", "[openshop] Invalid shop domain"),
    ] {
        assert_eq!(links(query).await, (400, json!({ "error": error })), "{query}");
    }
}
