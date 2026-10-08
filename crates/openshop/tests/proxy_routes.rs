//! App proxy routes under `/proxy` and their extension-direct twins under `/ext`.

use openshop::builder::Proxy;
use openshop::server::proxy::create_proxy_routes;
use openshop::testing::{
    TestContext, TestOptions, proxy_signature, session_token as signed_session_token, sign_session_token,
    to_query_string,
};
use openshop::{
    IndexMap, OpenShopConfig, ProxyAuthKind, ProxyContext, Result, ShopifyAppConfig, ShopifyConfig, Value, anyhow, json,
};
use reqwest::{Method, RequestBuilder, Response};

const CUSTOMER_GID: &str = "gid://shopify/Customer/123";
const EXTENSION_ORIGIN: &str = "https://extensions.shopifycdn.com";

async fn ping(ctx: ProxyContext) -> Result<Value> {
    Ok(json!({
        "pong": true,
        "from": "proxy-spec",
        "shop": ctx.shop,
        "customerId": ctx.customer_id,
        "authKind": ctx.auth,
        "shopifyApp": ctx.shopify_app,
        "queryShop": ctx.query.get("shop"),
        "queryCustomerId": ctx.query.get("logged_in_customer_id"),
    }))
}

async fn echo(ctx: ProxyContext) -> Result<Value> {
    Ok(json!({
        "shop": ctx.shop,
        "customerId": ctx.customer_id,
        "query": ctx.query,
        "params": ctx.params,
        "path": ctx.path,
        "method": ctx.method,
        "body": ctx.body,
        "testHeader": ctx.headers.get("x-test"),
    }))
}

fn ping_config() -> OpenShopConfig {
    OpenShopConfig::builder()
        .proxy("/ping", Proxy::json().get(ping))
        .build()
        .expect("valid config")
}

fn echo_config() -> OpenShopConfig {
    OpenShopConfig::builder()
        .proxy(
            "/echo",
            Proxy::json().get(echo).post(echo).put(echo).patch(echo).delete(echo),
        )
        .build()
        .expect("valid config")
}

fn app_keys(api_key: &str, api_secret: &str, app_url: &str) -> ShopifyAppConfig {
    ShopifyAppConfig::Keys {
        api_key: api_key.to_string(),
        api_secret: api_secret.to_string(),
        app_url: Some(app_url.to_string()),
    }
}

fn multi_app_config(apps: &[(&str, ShopifyAppConfig)]) -> OpenShopConfig {
    OpenShopConfig::builder()
        .shopify(ShopifyConfig {
            scopes: Some("read_products".to_string()),
            apps: Some(
                apps.iter()
                    .map(|(handle, app)| (handle.to_string(), app.clone()))
                    .collect(),
            ),
        })
        .proxy("/ping", Proxy::json().get(ping))
        .build()
        .expect("valid config")
}

fn client_apps() -> OpenShopConfig {
    multi_app_config(&[
        (
            "clientA",
            app_keys("client-a-key", "client-a-secret", "https://client-a.example.test"),
        ),
        (
            "clientB",
            app_keys("client-b-key", "client-b-secret", "https://client-b.example.test"),
        ),
    ])
}

/// A query string signed the way Shopify's app proxy signs it.
fn signed_query(params: &[(&str, &str)], secret: &str) -> String {
    let mut query: IndexMap<String, String> = params
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    query.insert("signature".to_string(), proxy_signature(&query, secret));
    to_query_string(&query)
}

/// A session token with full control over its claims.
fn session_token(shop: &str, sub: &str, audience: &str, expires_in_seconds: i64, secret: &str) -> String {
    let now = chrono::Utc::now().timestamp();
    sign_session_token(
        &json!({
            "iss": format!("https://{shop}/admin"),
            "dest": format!("https://{shop}"),
            "aud": audience,
            "sub": sub,
            "exp": now + expires_in_seconds,
            "nbf": now - 10,
            "iat": now,
            "jti": "proxy-jti",
            "sid": "proxy-sid",
        }),
        secret,
    )
}

/// A request without any credentials.
fn request(ctx: &TestContext, method: Method, path: &str) -> RequestBuilder {
    ctx.http.request(method, format!("{}{path}", ctx.url))
}

fn get(ctx: &TestContext, path: &str) -> RequestBuilder {
    request(ctx, Method::GET, path)
}

async fn json_body(response: Response) -> Value {
    response.json().await.expect("JSON body")
}

async fn assert_error(response: Response, status: u16, message: &str) {
    assert_eq!(response.status(), status);
    assert_eq!(json_body(response).await, json!({ "error": message }));
}

fn header<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response.headers().get(name).and_then(|value| value.to_str().ok())
}

#[tokio::test]
async fn get_without_valid_proxy_signature_returns_401() {
    let ctx = TestContext::new(ping_config()).await.unwrap();

    let response = get(&ctx, "/proxy/ping?shop=test.myshopify.com").send().await.unwrap();
    assert_error(response, 401, "Invalid proxy signature").await;

    let response = get(&ctx, "/proxy/ping").send().await.unwrap();
    assert_error(response, 401, "Invalid proxy signature").await;
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_with_invalid_proxy_signature_returns_401() {
    let ctx = TestContext::new(ping_config()).await.unwrap();
    let params = [("shop", "test.myshopify.com"), ("logged_in_customer_id", "42")];

    let not_hex = "/proxy/ping?shop=test.myshopify.com&logged_in_customer_id=42&signature=bad-signature";
    assert_error(get(&ctx, not_hex).send().await.unwrap(), 401, "Invalid proxy signature").await;

    let other_secret = signed_query(&params, "another-secret");
    let response = get(&ctx, &format!("/proxy/ping?{other_secret}")).send().await.unwrap();
    assert_error(response, 401, "Invalid proxy signature").await;

    // The signature covers every parameter: the customer cannot be swapped afterwards.
    let tampered = signed_query(&params, &ctx.secret).replace("logged_in_customer_id=42", "logged_in_customer_id=43");
    let response = get(&ctx, &format!("/proxy/ping?{tampered}")).send().await.unwrap();
    assert_error(response, 401, "Invalid proxy signature").await;
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_with_valid_hmac_returns_handler_json() {
    let ctx = TestContext::new(ping_config()).await.unwrap();
    let query = signed_query(
        &[("shop", "test.myshopify.com"), ("logged_in_customer_id", "42")],
        &ctx.secret,
    );

    let response = get(&ctx, &format!("/proxy/ping?{query}")).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(header(&response, "content-type"), Some("application/json"));
    assert_eq!(
        json_body(response).await,
        json!({
            "pong": true,
            "from": "proxy-spec",
            "shop": "test.myshopify.com",
            "customerId": "42",
            "authKind": "appProxyHmac",
            "shopifyApp": "default",
            "queryShop": "test.myshopify.com",
            "queryCustomerId": "42",
        })
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn customer_id_comes_only_from_a_numeric_signed_parameter() {
    let ctx = TestContext::new(ping_config()).await.unwrap();

    // Shopify sends an empty `logged_in_customer_id` for anonymous visitors.
    let anonymous = ctx.proxy().get("/ping").send().await.unwrap();
    assert_eq!(anonymous.status, 200);
    assert_eq!(anonymous.body["customerId"], Value::Null);
    assert_eq!(anonymous.body["queryCustomerId"], Value::Null);

    let malformed = ctx.proxy().get("/ping").as_customer("42abc").send().await.unwrap();
    assert_eq!(malformed.status, 200);
    assert_eq!(malformed.body["customerId"], Value::Null);
    assert_eq!(malformed.body["queryCustomerId"], Value::Null);

    let customer = ctx.proxy().get("/ping").as_customer("789456").send().await.unwrap();
    assert_eq!(customer.body["customerId"], "789456");
    assert_eq!(customer.body["queryCustomerId"], "789456");
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn signed_request_with_an_invalid_shop_returns_401() {
    let ctx = TestContext::new(ping_config()).await.unwrap();

    for params in [
        vec![("shop", "evil.example.com"), ("logged_in_customer_id", "42")],
        vec![("shop", "test.myshopify.com.evil.com")],
        vec![("logged_in_customer_id", "42")],
    ] {
        let query = signed_query(&params, &ctx.secret);
        let response = get(&ctx, &format!("/proxy/ping?{query}")).send().await.unwrap();
        assert_error(response, 401, "Invalid shop parameter").await;
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn handlers_receive_the_sanitized_query_and_request_details() {
    let ctx = TestContext::new(echo_config()).await.unwrap();

    let response = ctx
        .proxy()
        .get("/echo")
        .as_customer("42")
        .qs(&[("page", "2"), ("q", "blue shoes & socks")])
        .header("X-Test", "from-storefront")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body["shop"], ctx.shop);
    assert_eq!(response.body["customerId"], "42");
    assert_eq!(response.body["path"], "/proxy/echo");
    assert_eq!(response.body["method"], "GET");
    assert_eq!(response.body["params"], json!({}));
    assert_eq!(response.body["body"], Value::Null);
    assert_eq!(response.body["testHeader"], "from-storefront");

    let query = &response.body["query"];
    assert_eq!(query["shop"], ctx.shop);
    assert_eq!(query["logged_in_customer_id"], "42");
    assert_eq!(query["page"], "2");
    assert_eq!(query["q"], "blue shoes & socks");
    assert_eq!(query["path_prefix"], "/apps/openshop");
    assert!(query["timestamp"].is_string());
    assert!(query["signature"].is_string());

    // The shop is normalized, in the context and in the query alike.
    let query = signed_query(&[("shop", "Test.MyShopify.com")], &ctx.secret);
    let response = get(&ctx, &format!("/proxy/echo?{query}")).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let body = json_body(response).await;
    assert_eq!(body["shop"], "test.myshopify.com");
    assert_eq!(body["query"]["shop"], "test.myshopify.com");
    assert_eq!(body["query"].get("logged_in_customer_id"), None);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn liquid_html_and_json_routes_set_their_content_type() {
    let config = OpenShopConfig::builder()
        .proxy(
            "/reviews",
            Proxy::liquid().get(|ctx| async move {
                Ok(Value::from(format!(
                    "<h2>Reviews for {{{{ shop.name }}}}</h2><p>{}</p>",
                    ctx.shop
                )))
            }),
        )
        .proxy(
            "/widget",
            Proxy::html().get(|_ctx| async move { Ok(json!("<p>Widget</p>")) }),
        )
        .proxy(
            "/liquid-data",
            Proxy::liquid().get(|_ctx| async move { Ok(json!({ "liquid": false })) }),
        )
        .proxy("/text", Proxy::json().get(|_ctx| async move { Ok(json!("plain")) }))
        .proxy(
            "/default",
            Proxy::default().get(|_ctx| async move { Ok(json!([1, 2, 3])) }),
        )
        .build()
        .unwrap();
    let ctx = TestContext::new(config).await.unwrap();

    let liquid = ctx.proxy().get("/reviews").send().await.unwrap();
    assert_eq!(liquid.status, 200);
    assert_eq!(liquid.content_type, "application/liquid");
    assert_eq!(
        liquid.text,
        format!("<h2>Reviews for {{{{ shop.name }}}}</h2><p>{}</p>", ctx.shop)
    );

    let html = ctx.proxy().get("/widget").send().await.unwrap();
    assert_eq!(html.status, 200);
    assert_eq!(html.content_type, "text/html");
    assert_eq!(html.text, "<p>Widget</p>");

    // Only strings are sent as Liquid or HTML.
    let data = ctx.proxy().get("/liquid-data").send().await.unwrap();
    assert_eq!(data.content_type, "application/json");
    assert_eq!(data.body, json!({ "liquid": false }));

    let text = ctx.proxy().get("/text").send().await.unwrap();
    assert_eq!(text.content_type, "application/json");
    assert_eq!(text.text, "\"plain\"");

    let default = ctx.proxy().get("/default").send().await.unwrap();
    assert_eq!(default.content_type, "application/json");
    assert_eq!(default.body, json!([1, 2, 3]));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn dynamic_segments_are_exposed_as_params() {
    let config = OpenShopConfig::builder()
        .proxy("/api/reviews/:id", Proxy::json().get(echo))
        .proxy("/orders/[order]/items/[item]", Proxy::json().get(echo))
        .proxy(
            "/api/reviews/latest",
            Proxy::json().get(|_ctx| async move { Ok(json!({ "route": "latest" })) }),
        )
        .build()
        .unwrap();
    let ctx = TestContext::new(config).await.unwrap();

    let review = ctx.proxy().get("/api/reviews/123").send().await.unwrap();
    assert_eq!(review.status, 200);
    assert_eq!(review.body["params"], json!({ "id": "123" }));
    assert_eq!(review.body["path"], "/proxy/api/reviews/123");

    let encoded = ctx.proxy().get("/api/reviews/blue%20shoes").send().await.unwrap();
    assert_eq!(encoded.body["params"], json!({ "id": "blue shoes" }));

    let item = ctx.proxy().get("/orders/1001/items/sku-7").send().await.unwrap();
    assert_eq!(item.body["params"], json!({ "order": "1001", "item": "sku-7" }));

    // Static routes win over dynamic ones.
    let latest = ctx.proxy().get("/api/reviews/latest").send().await.unwrap();
    assert_eq!(latest.body, json!({ "route": "latest" }));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn index_route_is_served_at_the_prefix_root() {
    let config = OpenShopConfig::builder()
        .proxy("/", Proxy::json().get(echo))
        .build()
        .unwrap();
    let ctx = TestContext::new(config).await.unwrap();

    let response = ctx.proxy().get("").send().await.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body["path"], "/proxy");

    let token = ctx.session_token_for(&ctx.shop, CUSTOMER_GID);
    let response = get(&ctx, "/ext").bearer_auth(token).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(json_body(response).await["path"], "/ext");
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn non_get_methods_receive_the_parsed_json_body() {
    let ctx = TestContext::new(echo_config()).await.unwrap();
    let proxy = ctx.proxy();
    let review = json!({ "text": "Great product!", "rating": 5 });

    for (method, builder) in [
        ("POST", proxy.post("/echo")),
        ("PUT", proxy.put("/echo")),
        ("PATCH", proxy.patch("/echo")),
        ("DELETE", proxy.delete("/echo")),
    ] {
        let response = builder.as_customer("789456").json(review.clone()).send().await.unwrap();
        assert_eq!(response.status, 200, "{method}");
        assert_eq!(response.body["method"], method);
        assert_eq!(response.body["body"], review, "{method}");
        assert_eq!(response.body["customerId"], "789456", "{method}");
    }

    // Arrays and scalars are valid JSON bodies too.
    let response = proxy.post("/echo").json(json!([1, "two"])).send().await.unwrap();
    assert_eq!(response.body["body"], json!([1, "two"]));

    // A missing or malformed body is not an error: the handler sees no body.
    let empty = proxy.post("/echo").send().await.unwrap();
    assert_eq!(empty.status, 200);
    assert_eq!(empty.body["body"], Value::Null);

    let query = signed_query(&[("shop", ctx.shop.as_str())], &ctx.secret);
    let malformed = request(&ctx, Method::POST, &format!("/proxy/echo?{query}"))
        .header("content-type", "application/json")
        .body("{ not json")
        .send()
        .await
        .unwrap();
    assert_eq!(malformed.status(), 200);
    assert_eq!(json_body(malformed).await["body"], Value::Null);

    // GET handlers never get a body.
    let response = proxy.get("/echo").json(review).send().await.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body["body"], Value::Null);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn handler_errors_return_500_without_details() {
    let config = OpenShopConfig::builder()
        .proxy(
            "/fails",
            Proxy::json()
                .get(|_ctx| async move { Err(anyhow!("database password is hunter2")) })
                .post(|ctx| async move {
                    if ctx.body.is_none() {
                        panic!("proxy handler panic");
                    }
                    Ok(json!({ "ok": true }))
                }),
        )
        .build()
        .unwrap();
    let ctx = TestContext::new(config).await.unwrap();

    let failed = ctx.proxy().get("/fails").send().await.unwrap();
    assert_eq!(failed.status, 500);
    assert_eq!(failed.body, json!({ "error": "Internal proxy error" }));

    let panicked = ctx.proxy().post("/fails").send().await.unwrap();
    assert_eq!(panicked.status, 500);
    assert_eq!(panicked.body, json!({ "error": "Internal proxy error" }));

    // The server keeps serving after a failing handler.
    let recovered = ctx.proxy().post("/fails").json(json!({})).send().await.unwrap();
    assert_eq!(recovered.status, 200);
    assert_eq!(recovered.body, json!({ "ok": true }));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn unknown_paths_and_unregistered_methods_return_404() {
    let ctx = TestContext::new(ping_config()).await.unwrap();

    assert_eq!(ctx.proxy().get("/missing").send().await.unwrap().status, 404);
    assert_eq!(ctx.proxy().get("/ping/extra").send().await.unwrap().status, 404);
    // `/ping` only declares GET.
    assert_eq!(ctx.proxy().post("/ping").send().await.unwrap().status, 404);
    assert_eq!(ctx.proxy().delete("/ping").send().await.unwrap().status, 404);

    let token = ctx.session_token_for(&ctx.shop, CUSTOMER_GID);
    let response = get(&ctx, "/ext/missing").bearer_auth(&token).send().await.unwrap();
    assert_eq!(response.status(), 404);
    let response = request(&ctx, Method::POST, "/ext/ping")
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_with_app_proxy_signature_resolves_the_matching_shopify_app() {
    let ctx = TestContext::new(client_apps()).await.unwrap();
    let params = [("shop", "client-b-shop.myshopify.com"), ("logged_in_customer_id", "42")];

    let query = signed_query(&params, "client-b-secret");
    let response = get(&ctx, &format!("/proxy/ping?{query}")).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let body = json_body(response).await;
    assert_eq!(body["shop"], "client-b-shop.myshopify.com");
    assert_eq!(body["authKind"], "appProxyHmac");
    assert_eq!(body["shopifyApp"], "clientB");

    let query = signed_query(&params, "client-a-secret");
    let response = get(&ctx, &format!("/proxy/ping?{query}")).send().await.unwrap();
    assert_eq!(json_body(response).await["shopifyApp"], "clientA");

    // The env-based secret is not an app of a multi-app setup.
    let query = signed_query(&params, &ctx.secret);
    let response = get(&ctx, &format!("/proxy/ping?{query}")).send().await.unwrap();
    assert_error(response, 401, "Invalid proxy signature").await;
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn proxy_signature_matching_several_shopify_apps_is_rejected() {
    let config = multi_app_config(&[
        (
            "clientA",
            app_keys("client-a-key", "shared-secret", "https://client-a.example.test"),
        ),
        (
            "clientB",
            app_keys("client-b-key", "shared-secret", "https://client-b.example.test"),
        ),
    ]);
    let ctx = TestContext::new(config).await.unwrap();

    let query = signed_query(&[("shop", "test.myshopify.com")], "shared-secret");
    let response = get(&ctx, &format!("/proxy/ping?{query}")).send().await.unwrap();
    assert_error(response, 401, "Invalid proxy signature").await;
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn extension_mode_rejects_unsigned_forged_query_identity() {
    let ctx = TestContext::new(ping_config()).await.unwrap();

    let response = get(&ctx, "/ext/ping?shop=victim.myshopify.com&logged_in_customer_id=999")
        .send()
        .await
        .unwrap();
    assert_error(response, 401, "Unauthorized: missing session token").await;
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn extension_mode_rejects_app_proxy_signatures() {
    let ctx = TestContext::new(ping_config()).await.unwrap();
    let query = signed_query(
        &[("shop", "test.myshopify.com"), ("logged_in_customer_id", "42")],
        &ctx.secret,
    );

    let response = get(&ctx, &format!("/ext/ping?{query}")).send().await.unwrap();
    assert_error(response, 401, "Unauthorized: missing session token").await;

    // Only the Bearer scheme carries a session token.
    let token = ctx.session_token_for("jwt-shop.myshopify.com", CUSTOMER_GID);
    let response = get(&ctx, "/ext/ping")
        .header("Authorization", format!("Basic {token}"))
        .send()
        .await
        .unwrap();
    assert_error(response, 401, "Unauthorized: missing session token").await;
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn extension_mode_uses_jwt_identity_and_ignores_query_identity() {
    let ctx = TestContext::new(ping_config()).await.unwrap();
    let token = ctx.session_token_for("jwt-shop.myshopify.com", CUSTOMER_GID);

    let response = get(&ctx, "/ext/ping?shop=attacker.myshopify.com&logged_in_customer_id=999")
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        json_body(response).await,
        json!({
            "pong": true,
            "from": "proxy-spec",
            "shop": "jwt-shop.myshopify.com",
            "customerId": "123",
            "authKind": "customerAccountJwt",
            "shopifyApp": "default",
            "queryShop": "jwt-shop.myshopify.com",
            "queryCustomerId": "123",
        })
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn extension_mode_resolves_shopify_app_from_jwt_audience() {
    let ctx = TestContext::new(client_apps()).await.unwrap();
    let token = signed_session_token(
        "jwt-client-b.myshopify.com",
        "client-b-secret",
        "client-b-key",
        CUSTOMER_GID,
    );

    let response = get(&ctx, "/ext/ping?shop=attacker.myshopify.com&logged_in_customer_id=999")
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = json_body(response).await;
    assert_eq!(body["shop"], "jwt-client-b.myshopify.com");
    assert_eq!(body["authKind"], "customerAccountJwt");
    assert_eq!(body["shopifyApp"], "clientB");
    assert_eq!(body["queryShop"], "jwt-client-b.myshopify.com");

    // A token for one app signed with another app's secret is not accepted.
    let mismatched = signed_session_token(
        "jwt-client-b.myshopify.com",
        "client-a-secret",
        "client-b-key",
        CUSTOMER_GID,
    );
    let response = get(&ctx, "/ext/ping").bearer_auth(mismatched).send().await.unwrap();
    assert_error(response, 401, "Unauthorized: invalid or expired session token").await;
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn extension_mode_rejects_jwt_with_wrong_audience() {
    let ctx = TestContext::new(ping_config()).await.unwrap();
    let token = signed_session_token("jwt-shop.myshopify.com", &ctx.secret, "wrong-app", CUSTOMER_GID);

    let response = get(&ctx, "/ext/ping").bearer_auth(token).send().await.unwrap();
    assert_error(response, 401, "Unauthorized: invalid or expired session token").await;
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn extension_mode_rejects_invalid_bearer_token() {
    let ctx = TestContext::new(ping_config()).await.unwrap();

    let response = get(&ctx, "/ext/ping")
        .bearer_auth("not-a-valid-token")
        .send()
        .await
        .unwrap();
    assert_error(response, 401, "Unauthorized: missing token audience").await;

    let forged = signed_session_token("jwt-shop.myshopify.com", "not-the-secret", &ctx.api_key, CUSTOMER_GID);
    let response = get(&ctx, "/ext/ping").bearer_auth(forged).send().await.unwrap();
    assert_error(response, 401, "Unauthorized: invalid or expired session token").await;
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn extension_mode_rejects_expired_bearer_token() {
    let ctx = TestContext::new(ping_config()).await.unwrap();
    let token = session_token("jwt-shop.myshopify.com", CUSTOMER_GID, &ctx.api_key, -3600, &ctx.secret);

    let response = get(&ctx, "/ext/ping").bearer_auth(token).send().await.unwrap();
    assert_error(response, 401, "Unauthorized: invalid or expired session token").await;
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn numeric_admin_sub_is_not_treated_as_a_customer_id() {
    let ctx = TestContext::new(ping_config()).await.unwrap();
    let token = ctx.session_token_for("jwt-shop.myshopify.com", "123");

    let response = get(&ctx, "/ext/ping?logged_in_customer_id=123")
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = json_body(response).await;
    assert_eq!(body["shop"], "jwt-shop.myshopify.com");
    assert_eq!(body["customerId"], Value::Null);
    assert_eq!(body["queryShop"], "jwt-shop.myshopify.com");
    assert_eq!(body["queryCustomerId"], Value::Null);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn proxy_mode_accepts_session_tokens_and_prefers_them_over_the_signature() {
    let ctx = TestContext::new(ping_config()).await.unwrap();
    let query = signed_query(
        &[("shop", "test.myshopify.com"), ("logged_in_customer_id", "42")],
        &ctx.secret,
    );

    let token = ctx.session_token_for("jwt-shop.myshopify.com", CUSTOMER_GID);
    let response = get(&ctx, &format!("/proxy/ping?{query}"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = json_body(response).await;
    assert_eq!(body["authKind"], "customerAccountJwt");
    assert_eq!(body["shop"], "jwt-shop.myshopify.com");
    assert_eq!(body["customerId"], "123");
    assert_eq!(body["queryCustomerId"], "123");

    // A bad token is never rescued by a valid signature.
    let expired = session_token("jwt-shop.myshopify.com", CUSTOMER_GID, &ctx.api_key, -3600, &ctx.secret);
    let response = get(&ctx, &format!("/proxy/ping?{query}"))
        .bearer_auth(expired)
        .send()
        .await
        .unwrap();
    assert_error(response, 401, "Unauthorized: invalid or expired session token").await;
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn session_tokens_are_rejected_on_routes_limited_to_proxy_signatures() {
    let ctx = TestContext::new(ping_config()).await.unwrap();
    let router = create_proxy_routes(&ctx.app, "/signed", &[ProxyAuthKind::AppProxyHmac], false)
        .unwrap()
        .with_state(ctx.app.clone());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let url = format!("http://{}/signed/ping", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await });

    let token = ctx.session_token_for("jwt-shop.myshopify.com", CUSTOMER_GID);
    let response = ctx.http.get(&url).bearer_auth(token).send().await.unwrap();
    assert_error(response, 401, "Unauthorized: session token not accepted on this route").await;

    let query = signed_query(&[("shop", "test.myshopify.com")], &ctx.secret);
    let response = ctx.http.get(format!("{url}?{query}")).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(json_body(response).await["authKind"], "appProxyHmac");

    server.abort();
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn server_proxy_routes_allow_shopify_ui_extension_cors() {
    let ctx = TestContext::new(ping_config()).await.unwrap();
    let token = ctx.session_token_for("jwt-shop.myshopify.com", CUSTOMER_GID);

    for prefix in ["/proxy", "/ext"] {
        let path = format!("{prefix}/ping");

        let preflight = request(&ctx, Method::OPTIONS, &path)
            .header("Origin", EXTENSION_ORIGIN)
            .header("Access-Control-Request-Method", "GET")
            .header("Access-Control-Request-Headers", "authorization")
            .send()
            .await
            .unwrap();
        assert_eq!(preflight.status(), 204, "{prefix}");
        assert_eq!(
            header(&preflight, "access-control-allow-origin"),
            Some(EXTENSION_ORIGIN)
        );
        assert_eq!(
            header(&preflight, "access-control-allow-headers"),
            Some("authorization")
        );
        let methods = header(&preflight, "access-control-allow-methods").unwrap_or_default();
        assert!(methods.contains("GET") && methods.contains("POST"), "{methods}");

        let response = get(&ctx, &path)
            .header("Origin", EXTENSION_ORIGIN)
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{prefix}");
        assert_eq!(header(&response, "access-control-allow-origin"), Some(EXTENSION_ORIGIN));

        // Rejections are readable by the extension too.
        let rejected = get(&ctx, &path)
            .header("Origin", EXTENSION_ORIGIN)
            .bearer_auth("not-a-valid-token")
            .send()
            .await
            .unwrap();
        assert_eq!(rejected.status(), 401, "{prefix}");
        assert_eq!(header(&rejected, "access-control-allow-origin"), Some(EXTENSION_ORIGIN));
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn cors_headers_are_limited_to_allowed_origins() {
    let options = TestOptions {
        host: Some("https://app.example.com/".to_string()),
        ..Default::default()
    };
    let ctx = TestContext::with_options(ping_config(), options).await.unwrap();
    let token = ctx.session_token_for("jwt-shop.myshopify.com", CUSTOMER_GID);

    for origin in [
        "https://jwt-shop.myshopify.com",
        "https://shopify.com",
        "http://localhost:3000",
        "https://app.example.com",
    ] {
        let response = get(&ctx, "/ext/ping")
            .header("Origin", origin)
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{origin}");
        assert_eq!(header(&response, "access-control-allow-origin"), Some(origin));
    }

    for origin in ["https://evil.example.com", "https://extensions.shopifycdn.com.evil.com"] {
        let response = get(&ctx, "/ext/ping")
            .header("Origin", origin)
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{origin}");
        assert_eq!(header(&response, "access-control-allow-origin"), None, "{origin}");

        let preflight = request(&ctx, Method::OPTIONS, "/proxy/ping")
            .header("Origin", origin)
            .header("Access-Control-Request-Method", "GET")
            .send()
            .await
            .unwrap();
        assert_eq!(header(&preflight, "access-control-allow-origin"), None, "{origin}");
    }
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn missing_shopify_api_secret_fails_closed() {
    let options = TestOptions {
        secret: String::new(),
        ..Default::default()
    };
    let ctx = TestContext::with_options(ping_config(), options).await.unwrap();

    let response = get(&ctx, "/proxy/ping?shop=test.myshopify.com").send().await.unwrap();
    assert_error(response, 500, "SHOPIFY_API_SECRET is not configured").await;

    // Not even a request signed with the empty secret gets through.
    let query = signed_query(&[("shop", "test.myshopify.com")], "");
    let response = get(&ctx, &format!("/proxy/ping?{query}")).send().await.unwrap();
    assert_error(response, 500, "SHOPIFY_API_SECRET is not configured").await;

    let token = signed_session_token("test.myshopify.com", "", &ctx.api_key, CUSTOMER_GID);
    let response = get(&ctx, "/ext/ping").bearer_auth(token).send().await.unwrap();
    assert_error(response, 500, "SHOPIFY_API_SECRET is not configured").await;
    ctx.shutdown().await.unwrap();
}

#[test]
fn ambiguous_proxy_routes_are_rejected_when_the_config_is_built() {
    let error = OpenShopConfig::builder()
        .proxy("/reviews/:id", Proxy::json().get(ping))
        .proxy("/reviews/[slug]", Proxy::json().get(ping))
        .build()
        .err()
        .expect("ambiguous routes")
        .to_string();
    assert!(error.contains("have the same route shape"), "{error}");
}
