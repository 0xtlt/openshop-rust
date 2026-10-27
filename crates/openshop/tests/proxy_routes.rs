//! App proxy routes under `/proxy` and their extension-direct twins under `/ext`.

mod common;

use common::{assert_error, json_body, now, two_shopify_apps, two_shopify_apps_with_one_secret};
use openshop::builder::{Proxy, ProxyResponseType};
use openshop::server::proxy::create_proxy_routes;
use openshop::testing::{
    TestContext, TestOptions, proxy_signature, session_token as signed_session_token, sign_session_token,
    to_query_string,
};
use openshop::{IndexMap, OpenShopConfig, ProxyAuthKind, ProxyContext, Result, ShopifyConfig, Value, anyhow, json};
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
        .proxy("/ping", Proxy::default().get(ping))
        .build()
        .expect("valid config")
}

fn echo_config() -> OpenShopConfig {
    OpenShopConfig::builder()
        .proxy(
            "/echo",
            Proxy::default().get(echo).post(echo).put(echo).patch(echo).delete(echo),
        )
        .build()
        .expect("valid config")
}

fn multi_app_config(shopify: ShopifyConfig) -> OpenShopConfig {
    OpenShopConfig::builder()
        .shopify(shopify)
        .proxy("/ping", Proxy::default().get(ping))
        .build()
        .expect("valid config")
}

fn client_apps() -> OpenShopConfig {
    multi_app_config(two_shopify_apps())
}

/// A query string signed the way Shopify's app proxy signs it, with the
/// parameters as given and nothing added.
fn signed_as_given(params: &[(&str, &str)], secret: &str) -> String {
    let mut query: IndexMap<String, String> = params
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    query.insert("signature".to_string(), proxy_signature(&query, secret));
    to_query_string(&query)
}

/// A query string signed at `timestamp` the way Shopify's app proxy signs it.
fn signed_query_at(params: &[(&str, &str)], secret: &str, timestamp: i64) -> String {
    let timestamp = timestamp.to_string();
    let mut params = params.to_vec();
    params.push(("timestamp", &timestamp));
    signed_as_given(&params, secret)
}

/// A query string signed just now the way Shopify's app proxy signs it.
fn signed_query(params: &[(&str, &str)], secret: &str) -> String {
    signed_query_at(params, secret, now())
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

fn header<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response.headers().get(name).and_then(|value| value.to_str().ok())
}

#[openshop::test(TestContext::new(ping_config()))]
async fn get_without_valid_proxy_signature_returns_401(ctx: &TestContext) {
    let response = get(ctx, "/proxy/ping?shop=test.myshopify.com").send().await.unwrap();
    assert_error(response, 401, "Invalid proxy signature").await;

    let response = get(ctx, "/proxy/ping").send().await.unwrap();
    assert_error(response, 401, "Invalid proxy signature").await;
}

#[openshop::test(TestContext::new(ping_config()))]
async fn get_with_invalid_proxy_signature_returns_401(ctx: &TestContext) {
    let params = [("shop", "test.myshopify.com"), ("logged_in_customer_id", "42")];

    let not_hex = "/proxy/ping?shop=test.myshopify.com&logged_in_customer_id=42&signature=bad-signature";
    assert_error(get(ctx, not_hex).send().await.unwrap(), 401, "Invalid proxy signature").await;

    let other_secret = signed_query(&params, "another-secret");
    let response = get(ctx, &format!("/proxy/ping?{other_secret}")).send().await.unwrap();
    assert_error(response, 401, "Invalid proxy signature").await;

    // The signature covers every parameter: the customer cannot be swapped afterwards.
    let tampered = signed_query(&params, &ctx.secret).replace("logged_in_customer_id=42", "logged_in_customer_id=43");
    let response = get(ctx, &format!("/proxy/ping?{tampered}")).send().await.unwrap();
    assert_error(response, 401, "Invalid proxy signature").await;
}

#[openshop::test(TestContext::new(ping_config()))]
async fn a_signed_query_is_only_good_for_a_few_minutes(ctx: &TestContext) {
    let params = [("shop", "test.myshopify.com"), ("logged_in_customer_id", "42")];

    // A little skew on either side is tolerated: 90 seconds.
    for timestamp in [now() - 60, now() + 60] {
        let query = signed_query_at(&params, &ctx.secret, timestamp);
        let response = get(ctx, &format!("/proxy/ping?{query}")).send().await.unwrap();
        assert_eq!(response.status(), 200, "{timestamp}");
    }

    // A signed URL that was kept, or one that is not due yet, is no credential.
    for timestamp in [now() - 3600, now() - 120, now() + 120, 1_700_000_000] {
        let query = signed_query_at(&params, &ctx.secret, timestamp);
        let response = get(ctx, &format!("/proxy/ping?{query}")).send().await.unwrap();
        assert_error(response, 401, "Invalid proxy signature").await;
    }

    // Shopify always sends one: without it, or with one that is no time, there is nothing to check.
    let query = signed_as_given(&params, &ctx.secret);
    let response = get(ctx, &format!("/proxy/ping?{query}")).send().await.unwrap();
    assert_error(response, 401, "Invalid proxy signature").await;
    for timestamp in ["", "soon", "1.5e9", "-1"] {
        let query = signed_as_given(&[params[0], params[1], ("timestamp", timestamp)], &ctx.secret);
        let response = get(ctx, &format!("/proxy/ping?{query}")).send().await.unwrap();
        assert_error(response, 401, "Invalid proxy signature").await;
    }
}

#[openshop::test(TestContext::new(ping_config()))]
async fn a_parameter_sent_several_times_is_signed_with_its_values_joined(ctx: &TestContext) {
    // Shopify signs `extra=1,2` for `extra=1&extra=2`.
    let signed = signed_query(&[("shop", "test.myshopify.com"), ("extra", "1,2")], &ctx.secret);
    let repeated = signed.replace("extra=1%2C2", "extra=1&extra=2");
    assert_ne!(repeated, signed);
    let response = get(ctx, &format!("/proxy/ping?{repeated}")).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(json_body(response).await["shop"], "test.myshopify.com");

    // But not a parameter of Shopify's own: a second `shop` or `timestamp`, signed or not.
    let timestamp = now().to_string();
    let two_shops = signed_query(&[("shop", "test.myshopify.com,other.myshopify.com")], &ctx.secret)
        .replace("test.myshopify.com%2Cother", "test.myshopify.com&shop=other");
    let two_timestamps = signed_as_given(
        &[
            ("shop", "test.myshopify.com"),
            ("timestamp", &format!("{timestamp},{timestamp}")),
        ],
        &ctx.secret,
    )
    .replace("%2C", "&timestamp=");
    for query in [
        format!(
            "{}&shop=other.myshopify.com",
            signed_query(&[("shop", "test.myshopify.com")], &ctx.secret)
        ),
        two_shops,
        two_timestamps,
    ] {
        let response = get(ctx, &format!("/proxy/ping?{query}")).send().await.unwrap();
        assert_error(response, 401, "Invalid proxy signature").await;
    }
}

/// The pairs are signed glued together, so the text of one request can be cut
/// in other pairs: the signature of the request of a shop the sender owns then
/// stands for another shop.
#[openshop::test(TestContext::new(ping_config()))]
async fn a_signed_query_cannot_be_split_again_to_change_its_shop(ctx: &TestContext) {
    let timestamp = now().to_string();
    // What the sender has Shopify sign for their own shop: the parameter `m`
    // holds the text of a request for the victim's.
    let note = format!("1path_prefix=/apps/xshop=victim.myshopify.comtimestamp={timestamp}");
    let own = [
        ("m", note.as_str()),
        ("path_prefix", "/apps/x"),
        ("shop", "attacker.myshopify.com"),
        ("timestamp", timestamp.as_str()),
    ];
    // A value with a `=` is refused, even for the shop that signed it.
    let own_query = signed_as_given(&own, &ctx.secret);
    let response = get(ctx, &format!("/proxy/ping?{own_query}")).send().await.unwrap();
    assert_error(response, 401, "Invalid proxy signature").await;

    // The same signature, on the same text cut so that the shop is the victim's.
    let signature = own_query.rsplit("signature=").next().unwrap();
    let tail = format!("{timestamp}path_prefix=/apps/xshop=attacker.myshopify.comtimestamp={timestamp}");
    let forged = to_query_string(&IndexMap::from([
        ("m".to_string(), "1".to_string()),
        ("path_prefix".to_string(), "/apps/x".to_string()),
        ("shop".to_string(), "victim.myshopify.com".to_string()),
        ("timestamp".to_string(), tail),
        ("signature".to_string(), signature.to_string()),
    ]));
    let response = get(ctx, &format!("/proxy/ping?{forged}")).send().await.unwrap();
    assert_error(response, 401, "Invalid proxy signature").await;
}

/// Without any `=` in a key or a value, the characters that stand between two
/// `=` still go to the value before them or to the key after them. Here the
/// request of an anonymous visitor is read as the one of customer 42, with the
/// signature of the first.
#[openshop::test(TestContext::new(ping_config()))]
async fn a_signed_query_cannot_be_cut_again_to_log_a_customer_in(ctx: &TestContext) {
    let timestamp = now().to_string();
    let shared = [
        ("path_prefix", "/apps/openshop"),
        ("shop", "test.myshopify.com"),
        ("timestamp", timestamp.as_str()),
    ];
    let original = [
        &[
            ("1", "foo"),
            ("2logged_in_customer_id", "4"),
            ("2m", "o"),
            ("logged_in_customer_id", ""),
        ][..],
        &shared,
    ]
    .concat();
    let signed = signed_as_given(&original, &ctx.secret);
    let signature = signed.rsplit("signature=").next().unwrap();

    let forged = [
        &[
            ("1", "foo2"),
            ("logged_in_customer_id", "42"),
            ("m", ""),
            ("ologged_in_customer_id", ""),
        ][..],
        &shared,
        &[("signature", signature)],
    ]
    .concat();
    let forged = forged
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<Vec<_>>();
    // The two read the same to the signature.
    let query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(&forged)
        .finish();
    let response = get(ctx, &format!("/proxy/ping?{query}")).send().await.unwrap();
    assert_error(response, 401, "Invalid proxy signature").await;

    // What the visitor had Shopify sign is not a request of a customer either.
    let response = get(ctx, &format!("/proxy/ping?{signed}")).send().await.unwrap();
    assert_error(response, 401, "Invalid proxy signature").await;
}

/// `a.myshopify.comt@b.myshopify.com` is a URL whose host is `b.myshopify.com`:
/// a shop that was cut out of the beginning of a parameter of the visitor.
#[openshop::test(TestContext::new(ping_config()))]
async fn a_signed_query_cannot_be_cut_again_to_take_the_shop_of_a_url(ctx: &TestContext) {
    let timestamp = now().to_string();
    let signed = signed_as_given(
        &[
            ("logged_in_customer_id", ""),
            ("path_prefix", "/apps/openshop"),
            ("shop", "test.myshopify.com"),
            ("t@victim.myshopify.comta", "1"),
            ("timestamp", &timestamp),
        ],
        &ctx.secret,
    );
    let response = get(ctx, &format!("/proxy/ping?{signed}")).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(json_body(response).await["shop"], "test.myshopify.com");

    let signature = signed.rsplit("signature=").next().unwrap();
    let forged = to_query_string(&IndexMap::from([
        ("logged_in_customer_id".to_string(), String::new()),
        ("path_prefix".to_string(), "/apps/openshop".to_string()),
        (
            "shop".to_string(),
            "test.myshopify.comt@victim.myshopify.com".to_string(),
        ),
        ("ta".to_string(), "1".to_string()),
        ("timestamp".to_string(), timestamp),
        ("signature".to_string(), signature.to_string()),
    ]));
    let response = get(ctx, &format!("/proxy/ping?{forged}")).send().await.unwrap();
    assert_error(response, 401, "Invalid shop parameter").await;
}

#[openshop::test(TestContext::new(ping_config()))]
async fn get_with_valid_hmac_returns_handler_json(ctx: &TestContext) {
    let query = signed_query(
        &[("shop", "test.myshopify.com"), ("logged_in_customer_id", "42")],
        &ctx.secret,
    );

    let response = get(ctx, &format!("/proxy/ping?{query}")).send().await.unwrap();
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
}

#[openshop::test(TestContext::new(ping_config()))]
async fn customer_id_comes_only_from_a_numeric_signed_parameter(ctx: &TestContext) {
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
}

#[openshop::test(TestContext::new(ping_config()))]
async fn signed_request_with_an_invalid_shop_returns_401(ctx: &TestContext) {
    for params in [
        vec![("shop", "evil.example.com"), ("logged_in_customer_id", "42")],
        vec![("shop", "test.myshopify.com.evil.com")],
        vec![("logged_in_customer_id", "42")],
    ] {
        let query = signed_query(&params, &ctx.secret);
        let response = get(ctx, &format!("/proxy/ping?{query}")).send().await.unwrap();
        assert_error(response, 401, "Invalid shop parameter").await;
    }
}

#[openshop::test(TestContext::new(echo_config()))]
async fn handlers_receive_the_sanitized_query_and_request_details(ctx: &TestContext) {
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
    let response = get(ctx, &format!("/proxy/echo?{query}")).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let body = json_body(response).await;
    assert_eq!(body["shop"], "test.myshopify.com");
    assert_eq!(body["query"]["shop"], "test.myshopify.com");
    assert_eq!(body["query"].get("logged_in_customer_id"), None);
}

#[tokio::test]
async fn liquid_html_and_json_routes_set_their_content_type() {
    let config = OpenShopConfig::builder()
        .proxy(
            "/reviews",
            Proxy::new(ProxyResponseType::Liquid).get(|ctx| async move {
                Ok(Value::from(format!(
                    "<h2>Reviews for {{{{ shop.name }}}}</h2><p>{}</p>",
                    ctx.shop
                )))
            }),
        )
        .proxy(
            "/widget",
            Proxy::new(ProxyResponseType::Html).get(|_ctx| async move { Ok(json!("<p>Widget</p>")) }),
        )
        .proxy(
            "/liquid-data",
            Proxy::new(ProxyResponseType::Liquid).get(|_ctx| async move { Ok(json!({ "liquid": false })) }),
        )
        .proxy("/text", Proxy::default().get(|_ctx| async move { Ok(json!("plain")) }))
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
        .proxy("/api/reviews/:id", Proxy::default().get(echo))
        .proxy("/orders/[order]/items/[item]", Proxy::default().get(echo))
        .proxy(
            "/api/reviews/latest",
            Proxy::default().get(|_ctx| async move { Ok(json!({ "route": "latest" })) }),
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
        .proxy("/", Proxy::default().get(echo))
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

#[openshop::test(TestContext::new(echo_config()))]
async fn non_get_methods_receive_the_parsed_json_body(ctx: &TestContext) {
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
    let malformed = request(ctx, Method::POST, &format!("/proxy/echo?{query}"))
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
}

#[tokio::test]
async fn handler_errors_return_500_without_details() {
    let config = OpenShopConfig::builder()
        .proxy(
            "/fails",
            Proxy::default()
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

#[openshop::test(TestContext::new(ping_config()))]
async fn unknown_paths_and_unregistered_methods_return_404(ctx: &TestContext) {
    assert_eq!(ctx.proxy().get("/missing").send().await.unwrap().status, 404);
    assert_eq!(ctx.proxy().get("/ping/extra").send().await.unwrap().status, 404);
    // `/ping` only declares GET.
    assert_eq!(ctx.proxy().post("/ping").send().await.unwrap().status, 404);
    assert_eq!(ctx.proxy().delete("/ping").send().await.unwrap().status, 404);

    let token = ctx.session_token_for(&ctx.shop, CUSTOMER_GID);
    let response = get(ctx, "/ext/missing").bearer_auth(&token).send().await.unwrap();
    assert_eq!(response.status(), 404);
    let response = request(ctx, Method::POST, "/ext/ping")
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
}

#[openshop::test(TestContext::new(client_apps()))]
async fn get_with_app_proxy_signature_resolves_the_matching_shopify_app(ctx: &TestContext) {
    let params = [("shop", "client-b-shop.myshopify.com"), ("logged_in_customer_id", "42")];

    let query = signed_query(&params, "client-b-secret");
    let response = get(ctx, &format!("/proxy/ping?{query}")).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let body = json_body(response).await;
    assert_eq!(body["shop"], "client-b-shop.myshopify.com");
    assert_eq!(body["authKind"], "appProxyHmac");
    assert_eq!(body["shopifyApp"], "clientB");

    let query = signed_query(&params, "client-a-secret");
    let response = get(ctx, &format!("/proxy/ping?{query}")).send().await.unwrap();
    assert_eq!(json_body(response).await["shopifyApp"], "clientA");

    // The env-based secret is not an app of a multi-app setup.
    let query = signed_query(&params, &ctx.secret);
    let response = get(ctx, &format!("/proxy/ping?{query}")).send().await.unwrap();
    assert_error(response, 401, "Invalid proxy signature").await;
}

#[tokio::test]
async fn proxy_signature_matching_several_shopify_apps_is_rejected() {
    let config = multi_app_config(two_shopify_apps_with_one_secret());
    let ctx = TestContext::new(config).await.unwrap();

    let query = signed_query(&[("shop", "test.myshopify.com")], "shared-secret");
    let response = get(&ctx, &format!("/proxy/ping?{query}")).send().await.unwrap();
    assert_error(response, 401, "Invalid proxy signature").await;
    ctx.shutdown().await.unwrap();
}

#[openshop::test(TestContext::new(ping_config()))]
async fn extension_mode_rejects_unsigned_forged_query_identity(ctx: &TestContext) {
    let response = get(ctx, "/ext/ping?shop=victim.myshopify.com&logged_in_customer_id=999")
        .send()
        .await
        .unwrap();
    assert_error(response, 401, "Unauthorized: missing session token").await;
}

#[openshop::test(TestContext::new(ping_config()))]
async fn extension_mode_rejects_app_proxy_signatures(ctx: &TestContext) {
    let query = signed_query(
        &[("shop", "test.myshopify.com"), ("logged_in_customer_id", "42")],
        &ctx.secret,
    );

    let response = get(ctx, &format!("/ext/ping?{query}")).send().await.unwrap();
    assert_error(response, 401, "Unauthorized: missing session token").await;

    // Only the Bearer scheme carries a session token.
    let token = ctx.session_token_for("jwt-shop.myshopify.com", CUSTOMER_GID);
    let response = get(ctx, "/ext/ping")
        .header("Authorization", format!("Basic {token}"))
        .send()
        .await
        .unwrap();
    assert_error(response, 401, "Unauthorized: missing session token").await;
}

#[openshop::test(TestContext::new(ping_config()))]
async fn extension_mode_uses_jwt_identity_and_ignores_query_identity(ctx: &TestContext) {
    let token = ctx.session_token_for("jwt-shop.myshopify.com", CUSTOMER_GID);

    let response = get(ctx, "/ext/ping?shop=attacker.myshopify.com&logged_in_customer_id=999")
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
}

#[openshop::test(TestContext::new(client_apps()))]
async fn extension_mode_resolves_shopify_app_from_jwt_audience(ctx: &TestContext) {
    let token = signed_session_token(
        "jwt-client-b.myshopify.com",
        "client-b-secret",
        "client-b-key",
        CUSTOMER_GID,
    );

    let response = get(ctx, "/ext/ping?shop=attacker.myshopify.com&logged_in_customer_id=999")
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
    let response = get(ctx, "/ext/ping").bearer_auth(mismatched).send().await.unwrap();
    assert_error(response, 401, "Unauthorized: invalid or expired session token").await;
}

#[openshop::test(TestContext::new(ping_config()))]
async fn extension_mode_rejects_jwt_with_wrong_audience(ctx: &TestContext) {
    let token = signed_session_token("jwt-shop.myshopify.com", &ctx.secret, "wrong-app", CUSTOMER_GID);

    let response = get(ctx, "/ext/ping").bearer_auth(token).send().await.unwrap();
    assert_error(response, 401, "Unauthorized: invalid or expired session token").await;
}

#[openshop::test(TestContext::new(ping_config()))]
async fn extension_mode_rejects_invalid_bearer_token(ctx: &TestContext) {
    let response = get(ctx, "/ext/ping")
        .bearer_auth("not-a-valid-token")
        .send()
        .await
        .unwrap();
    assert_error(response, 401, "Unauthorized: missing token audience").await;

    let forged = signed_session_token("jwt-shop.myshopify.com", "not-the-secret", &ctx.api_key, CUSTOMER_GID);
    let response = get(ctx, "/ext/ping").bearer_auth(forged).send().await.unwrap();
    assert_error(response, 401, "Unauthorized: invalid or expired session token").await;
}

#[openshop::test(TestContext::new(ping_config()))]
async fn extension_mode_rejects_expired_bearer_token(ctx: &TestContext) {
    let token = session_token("jwt-shop.myshopify.com", CUSTOMER_GID, &ctx.api_key, -3600, &ctx.secret);

    let response = get(ctx, "/ext/ping").bearer_auth(token).send().await.unwrap();
    assert_error(response, 401, "Unauthorized: invalid or expired session token").await;
}

#[openshop::test(TestContext::new(ping_config()))]
async fn the_session_token_of_a_staff_user_is_refused_on_proxy_and_ext_routes(ctx: &TestContext) {
    let not_staff = "Unauthorized: session token was issued to a staff user";
    let query = signed_query(&[("shop", "test.myshopify.com")], &ctx.secret);
    // The subject of a staff user is a numeric id, never a customer GID.
    for sub in ["123", "gid://shopify/StaffMember/9", "gid://shopify/Customer/abc"] {
        let token = ctx.session_token_for("jwt-shop.myshopify.com", sub);
        for path in [
            "/ext/ping?logged_in_customer_id=123".to_string(),
            format!("/proxy/ping?{query}"),
        ] {
            let response = get(ctx, &path).bearer_auth(&token).send().await.unwrap();
            assert_error(response, 401, not_staff).await;
        }
    }

    // The tokens of customer account and checkout extensions are accepted, a buyer without an account included.
    for (sub, customer_id) in [(CUSTOMER_GID, json!("123")), ("", Value::Null)] {
        let token = ctx.session_token_for("jwt-shop.myshopify.com", sub);
        let response = get(ctx, "/ext/ping").bearer_auth(token).send().await.unwrap();
        assert_eq!(response.status(), 200, "{sub}");
        let body = json_body(response).await;
        assert_eq!(body["shop"], "jwt-shop.myshopify.com");
        assert_eq!(body["customerId"], customer_id);
    }
}

#[openshop::test(TestContext::new(ping_config()))]
async fn proxy_mode_accepts_session_tokens_and_prefers_them_over_the_signature(ctx: &TestContext) {
    let query = signed_query(
        &[("shop", "test.myshopify.com"), ("logged_in_customer_id", "42")],
        &ctx.secret,
    );

    let token = ctx.session_token_for("jwt-shop.myshopify.com", CUSTOMER_GID);
    let response = get(ctx, &format!("/proxy/ping?{query}"))
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
    let response = get(ctx, &format!("/proxy/ping?{query}"))
        .bearer_auth(expired)
        .send()
        .await
        .unwrap();
    assert_error(response, 401, "Unauthorized: invalid or expired session token").await;
}

#[openshop::test(TestContext::new(ping_config()))]
async fn session_tokens_are_rejected_on_routes_limited_to_proxy_signatures(ctx: &TestContext) {
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
}

#[openshop::test(TestContext::new(ping_config()))]
async fn server_proxy_routes_allow_shopify_ui_extension_cors(ctx: &TestContext) {
    let token = ctx.session_token_for("jwt-shop.myshopify.com", CUSTOMER_GID);

    for prefix in ["/proxy", "/ext"] {
        let path = format!("{prefix}/ping");

        let preflight = request(ctx, Method::OPTIONS, &path)
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

        let response = get(ctx, &path)
            .header("Origin", EXTENSION_ORIGIN)
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{prefix}");
        assert_eq!(header(&response, "access-control-allow-origin"), Some(EXTENSION_ORIGIN));

        // Rejections are readable by the extension too.
        let rejected = get(ctx, &path)
            .header("Origin", EXTENSION_ORIGIN)
            .bearer_auth("not-a-valid-token")
            .send()
            .await
            .unwrap();
        assert_eq!(rejected.status(), 401, "{prefix}");
        assert_eq!(header(&rejected, "access-control-allow-origin"), Some(EXTENSION_ORIGIN));
    }
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
        .proxy("/reviews/:id", Proxy::default().get(ping))
        .proxy("/reviews/[slug]", Proxy::default().get(ping))
        .build()
        .err()
        .expect("ambiguous routes")
        .to_string();
    assert!(error.contains("have the same route shape"), "{error}");
}
