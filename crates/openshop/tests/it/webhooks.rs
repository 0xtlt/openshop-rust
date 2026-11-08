//! `POST /webhooks`: HMAC verification, topic dispatch and the handler context.

use std::sync::{Arc, Mutex};

use crate::common::{LogCapture, assert_error, noop_flow, two_shopify_apps, two_shopify_apps_with_one_secret};
use openshop::builder::{OpenShopBuilder, Webhook};
use openshop::db::schema::FlowRun;
use openshop::testing::{TestContext, TestOptions, sign_webhook_body};
use openshop::{DispatchOptions, OpenShopConfig, Value, WebhookContext, bail, json};
use reqwest::Response;

const SECRET: &str = "test-secret";

/// The context a handler was called with, without its `app`.
#[derive(Debug, Clone, PartialEq)]
struct Delivery {
    topic: String,
    shop: String,
    shopify_app: String,
    api_version: String,
    payload: Value,
}

/// Records the deliveries a webhook handler receives.
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<Delivery>>>);

impl Recorder {
    fn webhook(&self) -> Webhook {
        let recorder = self.clone();
        Webhook::new(move |ctx: WebhookContext| {
            let recorder = recorder.clone();
            async move {
                recorder.0.lock().unwrap().push(Delivery {
                    topic: ctx.topic,
                    shop: ctx.shop,
                    shopify_app: ctx.shopify_app,
                    api_version: ctx.api_version,
                    payload: ctx.payload,
                });
                Ok(())
            }
        })
    }

    fn deliveries(&self) -> Vec<Delivery> {
        self.0.lock().unwrap().clone()
    }

    fn count(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

fn builder() -> OpenShopBuilder {
    OpenShopConfig::builder().flow(noop_flow("wh-flow"))
}

fn orders_create_config(recorder: &Recorder) -> OpenShopConfig {
    builder().webhook("ORDERS_CREATE", recorder.webhook()).build().unwrap()
}

fn multi_app_config(recorder: &Recorder) -> OpenShopConfig {
    builder()
        .shopify(two_shopify_apps())
        .webhook("ORDERS_CREATE", recorder.webhook())
        .build()
        .unwrap()
}

async fn post_webhook(ctx: &TestContext, topic: &str, shop: &str, body: &str, hmac: &str) -> Response {
    ctx.http
        .post(format!("{}/webhooks", ctx.url))
        .header("content-type", "application/json")
        .header("x-shopify-hmac-sha256", hmac)
        .header("x-shopify-topic", topic)
        .header("x-shopify-shop-domain", shop)
        .header("x-shopify-api-version", "2024-01")
        .body(body.to_string())
        .send()
        .await
        .unwrap()
}

/// A delivery signed with the single-app secret.
async fn deliver(ctx: &TestContext, topic: &str, shop: &str, body: &str) -> Response {
    post_webhook(ctx, topic, shop, body, &sign_webhook_body(body, SECRET)).await
}

async fn assert_ok(response: Response) {
    assert_eq!(response.status(), 200);
    assert_eq!(response.json::<Value>().await.unwrap(), json!({ "ok": true }));
}

async fn flow_runs(ctx: &TestContext) -> Vec<FlowRun> {
    sqlx::query_as("SELECT * FROM flow_runs ORDER BY created_at")
        .fetch_all(&ctx.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn post_webhooks_rejects_missing_forged_or_mismatched_signatures() {
    let recorder = Recorder::default();
    let ctx = TestContext::new(orders_create_config(&recorder)).await.unwrap();
    let body = r#"{"id":1,"total":"10.00"}"#;
    let valid = sign_webhook_body(body, SECRET);

    let response = ctx
        .http
        .post(format!("{}/webhooks", ctx.url))
        .header("x-shopify-topic", "orders/create")
        .header("x-shopify-shop-domain", "test.myshopify.com")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_error(response, 401, "Invalid webhook HMAC").await;

    for hmac in [
        String::new(),
        "invalid".to_string(),
        sign_webhook_body(body, "another-secret"),
        sign_webhook_body(body, ""),
        // Signature of another payload.
        sign_webhook_body(r#"{"id":2,"total":"10.00"}"#, SECRET),
        // A valid signature with trailing garbage, or cut short.
        format!("{valid}AA"),
        valid[..valid.len() - 2].to_string(),
        valid.to_lowercase(),
    ] {
        let response = post_webhook(&ctx, "orders/create", "test.myshopify.com", body, &hmac).await;
        assert_error(response, 401, "Invalid webhook HMAC").await;
    }

    // The signature covers the exact bytes of the body, not its JSON value.
    let reformatted = r#"{"id": 1, "total": "10.00"}"#;
    let response = post_webhook(&ctx, "orders/create", "test.myshopify.com", reformatted, &valid).await;
    assert_error(response, 401, "Invalid webhook HMAC").await;

    assert_eq!(recorder.count(), 0);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn post_webhooks_with_unknown_topic_returns_200_without_invoking_handler() {
    let recorder = Recorder::default();
    let ctx = TestContext::new(orders_create_config(&recorder)).await.unwrap();

    assert_ok(deliver(&ctx, "products/update", "test.myshopify.com", r#"{"id":1}"#).await).await;
    // The body is only parsed for a registered topic.
    assert_ok(deliver(&ctx, "products/update", "test.myshopify.com", "{not json").await).await;
    assert_ok(deliver(&ctx, "", "test.myshopify.com", r#"{"id":1}"#).await).await;

    assert_eq!(recorder.count(), 0);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn post_webhooks_invokes_handler_for_registered_topic() {
    let recorder = Recorder::default();
    let ctx = TestContext::new(orders_create_config(&recorder)).await.unwrap();
    let body = r#"{"id":999,"name":"order"}"#;

    assert_ok(deliver(&ctx, "orders/create", "shop-hooks.myshopify.com", body).await).await;

    assert_eq!(
        recorder.deliveries(),
        vec![Delivery {
            topic: "orders/create".to_string(),
            shop: "shop-hooks.myshopify.com".to_string(),
            shopify_app: "default".to_string(),
            api_version: "2024-01".to_string(),
            payload: json!({ "id": 999, "name": "order" }),
        }]
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn post_webhooks_normalizes_the_topic_to_find_the_handler() {
    let orders = Recorder::default();
    let uninstalled = Recorder::default();
    let config = builder()
        .webhook("ORDERS_CREATE", orders.webhook())
        .webhook("APP_UNINSTALLED", uninstalled.webhook())
        .build()
        .unwrap();
    let ctx = TestContext::new(config).await.unwrap();

    for topic in ["orders/create", "ORDERS_CREATE", "Orders/Create", "orders_create"] {
        assert_ok(deliver(&ctx, topic, "test.myshopify.com", "{}").await).await;
    }
    let uninstall = r#"{"myshopify_domain":"test.myshopify.com"}"#;
    assert_ok(deliver(&ctx, "app/uninstalled", "test.myshopify.com", uninstall).await).await;
    // Close, but another topic.
    assert_ok(deliver(&ctx, "orders/created", "test.myshopify.com", "{}").await).await;
    assert_ok(deliver(&ctx, "orders/create/extra", "test.myshopify.com", "{}").await).await;

    // Handlers see the topic exactly as Shopify sent it.
    let topics: Vec<String> = orders.deliveries().into_iter().map(|delivery| delivery.topic).collect();
    assert_eq!(
        topics,
        ["orders/create", "ORDERS_CREATE", "Orders/Create", "orders_create"]
    );
    assert_eq!(uninstalled.count(), 1);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn post_webhooks_prefers_a_handler_registered_under_the_raw_topic() {
    let raw = Recorder::default();
    let normalized = Recorder::default();
    let config = builder()
        .webhook("ORDERS_CREATE", normalized.webhook())
        .webhook("orders/create", raw.webhook())
        .webhook("products/update", raw.webhook())
        .build()
        .unwrap();
    let ctx = TestContext::new(config).await.unwrap();

    assert_ok(deliver(&ctx, "orders/create", "test.myshopify.com", "{}").await).await;
    assert_eq!((raw.count(), normalized.count()), (1, 0));

    assert_ok(deliver(&ctx, "ORDERS_CREATE", "test.myshopify.com", "{}").await).await;
    assert_eq!((raw.count(), normalized.count()), (1, 1));

    // Normalization only goes one way: `products/update` is not found from `PRODUCTS_UPDATE`.
    assert_ok(deliver(&ctx, "products/update", "test.myshopify.com", "{}").await).await;
    assert_ok(deliver(&ctx, "PRODUCTS_UPDATE", "test.myshopify.com", "{}").await).await;
    assert_eq!((raw.count(), normalized.count()), (2, 1));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn post_webhooks_with_invalid_json_returns_400() {
    let recorder = Recorder::default();
    let ctx = TestContext::new(orders_create_config(&recorder)).await.unwrap();

    for body in ["{\"id\":", "{'id':1}", "{not json"] {
        let response = deliver(&ctx, "orders/create", "test.myshopify.com", body).await;
        assert_error(response, 400, "Invalid JSON body").await;
    }
    // Only a JSON object verifies: the secret also signs launches, app proxy
    // requests and session tokens, none of which starts with `{`.
    for body in ["not json", "", "[1,2]", " {\"id\":1}"] {
        let response = deliver(&ctx, "orders/create", "test.myshopify.com", body).await;
        assert_error(response, 401, "Invalid webhook HMAC").await;
    }
    assert_eq!(recorder.count(), 0);

    assert_ok(deliver(&ctx, "orders/create", "test.myshopify.com", r#"{"id":1}"#).await).await;
    assert_eq!(recorder.deliveries()[0].payload, json!({ "id": 1 }));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn post_webhooks_still_returns_200_when_the_handler_fails() {
    let recorder = Recorder::default();
    let config = builder()
        .webhook(
            "ORDERS_CREATE",
            Webhook::new(|_ctx: WebhookContext| async move { bail!("handler exploded") }),
        )
        .webhook(
            "ORDERS_UPDATED",
            Webhook::new(|ctx: WebhookContext| async move {
                if ctx.payload["panic"] == true {
                    panic!("handler panicked");
                }
                Ok(())
            }),
        )
        .webhook("ORDERS_PAID", recorder.webhook())
        .build()
        .unwrap();
    let ctx = TestContext::new(config).await.unwrap();

    assert_ok(deliver(&ctx, "orders/create", "test.myshopify.com", "{}").await).await;
    assert_ok(deliver(&ctx, "orders/updated", "test.myshopify.com", r#"{"panic":true}"#).await).await;

    // The server keeps serving after a failed handler.
    assert_ok(deliver(&ctx, "orders/paid", "test.myshopify.com", "{}").await).await;
    assert_eq!(recorder.count(), 1);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn post_webhooks_does_not_require_or_accept_a_session_token() {
    let recorder = Recorder::default();
    let ctx = TestContext::new(orders_create_config(&recorder)).await.unwrap();
    let body = r#"{"id":1}"#;

    // A valid session token is no substitute for the signature.
    let response = ctx
        .post("/webhooks")
        .header("x-shopify-topic", "orders/create")
        .header("x-shopify-shop-domain", "test.myshopify.com")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_error(response, 401, "Invalid webhook HMAC").await;
    assert_eq!(recorder.count(), 0);

    // The trailing-slash alias is no way around the signature either.
    let response = ctx
        .post("/webhooks/")
        .header("x-shopify-hmac-sha256", "invalid")
        .header("x-shopify-topic", "orders/create")
        .body(body)
        .send()
        .await
        .unwrap();
    assert!(!response.status().is_success());
    assert_eq!(recorder.count(), 0);

    assert_ok(deliver(&ctx, "orders/create", "test.myshopify.com", body).await).await;
    assert_eq!(recorder.count(), 1);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn post_webhooks_handler_can_dispatch_a_flow_through_the_app() {
    let config = builder()
        .webhook(
            "ORDERS_CREATE",
            Webhook::new(|ctx: WebhookContext| async move {
                ctx.app
                    .dispatch_flow(
                        DispatchOptions::new("wh-flow", ctx.shop.clone())
                            .input(json!({ "orderId": ctx.payload["id"], "topic": ctx.topic }))
                            .shopify_app(ctx.shopify_app.clone()),
                    )
                    .await?;
                Ok(())
            }),
        )
        .build()
        .unwrap();
    let ctx = TestContext::new(config).await.unwrap();

    assert_ok(deliver(&ctx, "orders/create", "shop-hooks.myshopify.com", r#"{"id":4242}"#).await).await;

    let runs = flow_runs(&ctx).await;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].flow_name, "wh-flow");
    assert_eq!(runs[0].shop, "shop-hooks.myshopify.com");
    assert_eq!(runs[0].app_handle, "default");
    assert_eq!(runs[0].status, "pending");
    assert_eq!(
        runs[0].input,
        Some(json!({ "orderId": 4242, "topic": "orders/create" }))
    );

    // The flow rejects concurrent runs: the handler error is swallowed and Shopify still gets a 200.
    assert_ok(deliver(&ctx, "orders/create", "shop-hooks.myshopify.com", r#"{"id":4243}"#).await).await;
    assert_eq!(flow_runs(&ctx).await.len(), 1);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn post_webhooks_acknowledges_a_delivery_whose_dispatch_input_is_refused() {
    // A handler that queues a run with input the flow refuses fails like any
    // handler error: logged, no run, and Shopify gets its 200, so it does not
    // redeliver the same payload for ever.
    let strict = noop_flow("wh-flow").input_schema(openshop::builder::InputSchema::custom(
        json!({ "type": "object" }),
        |value| match value.get("orderId") {
            Some(Value::Number(_)) => Ok(value),
            _ => Err("orderId must be a number".to_string()),
        },
    ));
    let config = OpenShopConfig::builder()
        .flow(strict)
        .webhook(
            "ORDERS_CREATE",
            Webhook::new(|ctx: WebhookContext| async move {
                ctx.app
                    .dispatch_flow(
                        DispatchOptions::new("wh-flow", ctx.shop.clone())
                            .input(json!({ "orderId": ctx.payload["name"] })),
                    )
                    .await?;
                Ok(())
            }),
        )
        .build()
        .unwrap();
    let ctx = TestContext::new(config).await.unwrap();

    assert_ok(
        deliver(
            &ctx,
            "orders/create",
            "shop-hooks.myshopify.com",
            r#"{"name":"order one"}"#,
        )
        .await,
    )
    .await;
    assert!(flow_runs(&ctx).await.is_empty());
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn post_webhooks_resolves_the_matching_shopify_app_from_hmac() {
    let recorder = Recorder::default();
    let ctx = TestContext::new(multi_app_config(&recorder)).await.unwrap();
    let body = r#"{"id":1000,"name":"order"}"#;

    let hmac = sign_webhook_body(body, "client-b-secret");
    let response = post_webhook(&ctx, "orders/create", "client-b-shop.myshopify.com", body, &hmac).await;
    assert_ok(response).await;
    let hmac = sign_webhook_body(body, "client-a-secret");
    let response = post_webhook(&ctx, "orders/create", "client-a-shop.myshopify.com", body, &hmac).await;
    assert_ok(response).await;

    let received: Vec<(String, String)> = recorder
        .deliveries()
        .into_iter()
        .map(|delivery| (delivery.shopify_app, delivery.shop))
        .collect();
    assert_eq!(
        received,
        [
            ("clientB".to_string(), "client-b-shop.myshopify.com".to_string()),
            ("clientA".to_string(), "client-a-shop.myshopify.com".to_string()),
        ]
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn post_webhooks_in_multi_app_mode_rejects_signatures_matching_no_app() {
    let recorder = Recorder::default();
    let ctx = TestContext::new(multi_app_config(&recorder)).await.unwrap();
    let body = r#"{"id":1000}"#;

    // `test-secret` is the environment secret of single-app mode; it is not an app here.
    for secret in [SECRET, "client-c-secret", "client-a-secre", "client-a-key"] {
        let hmac = sign_webhook_body(body, secret);
        let response = post_webhook(&ctx, "orders/create", "client-a-shop.myshopify.com", body, &hmac).await;
        assert_error(response, 401, "Invalid webhook HMAC").await;
    }
    assert_eq!(recorder.count(), 0);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn post_webhooks_in_multi_app_mode_rejects_a_signature_matching_several_apps() {
    let recorder = Recorder::default();
    let config = builder()
        .shopify(two_shopify_apps_with_one_secret())
        .webhook("ORDERS_CREATE", recorder.webhook())
        .build()
        .unwrap();
    let ctx = TestContext::new(config).await.unwrap();
    let body = r#"{"id":1000}"#;

    let hmac = sign_webhook_body(body, "shared-secret");
    let response = post_webhook(&ctx, "orders/create", "shop.myshopify.com", body, &hmac).await;
    assert_error(response, 401, "Invalid webhook HMAC").await;
    assert_eq!(recorder.count(), 0);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn post_webhooks_handler_dispatches_flows_for_the_resolved_app() {
    let config = builder()
        .shopify(two_shopify_apps())
        .webhook(
            "ORDERS_CREATE",
            Webhook::new(|ctx: WebhookContext| async move {
                ctx.app
                    .dispatch_flow(DispatchOptions::new("wh-flow", ctx.shop).shopify_app(ctx.shopify_app))
                    .await?;
                Ok(())
            }),
        )
        .build()
        .unwrap();
    let ctx = TestContext::new(config).await.unwrap();
    let body = r#"{"id":1}"#;
    let shop = "same-shop.myshopify.com";

    for secret in ["client-b-secret", "client-a-secret"] {
        let response = post_webhook(&ctx, "orders/create", shop, body, &sign_webhook_body(body, secret)).await;
        assert_ok(response).await;
    }

    // Same shop, same flow: one run per app, neither blocked by the other.
    let mut runs: Vec<(String, String)> = flow_runs(&ctx)
        .await
        .into_iter()
        .map(|run| (run.app_handle, run.shop))
        .collect();
    runs.sort();
    assert_eq!(
        runs,
        [
            ("clientA".to_string(), shop.to_string()),
            ("clientB".to_string(), shop.to_string()),
        ]
    );
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn post_webhooks_without_shopify_api_secret_returns_500() {
    let recorder = Recorder::default();
    for secret in ["", "   "] {
        let options = TestOptions {
            secret: secret.to_string(),
            ..Default::default()
        };
        let ctx = TestContext::with_options(orders_create_config(&recorder), options)
            .await
            .unwrap();

        for hmac in ["anything".to_string(), sign_webhook_body("{}", secret)] {
            let response = post_webhook(&ctx, "orders/create", "test.myshopify.com", "{}", &hmac).await;
            assert_error(response, 500, "SHOPIFY_API_SECRET is not configured").await;
        }
        ctx.shutdown().await.unwrap();
    }
    assert_eq!(recorder.count(), 0);
}

#[tokio::test]
async fn post_webhooks_verifies_the_bytes_that_were_sent_not_a_lossy_text() {
    let recorder = Recorder::default();
    let ctx = TestContext::new(orders_create_config(&recorder)).await.unwrap();
    // Latin-1 `é`: not UTF-8, so not JSON either.
    let body: Vec<u8> = b"{\"note\":\"caf\xe9\"}".to_vec();
    let mangled = String::from_utf8_lossy(&body).into_owned();
    let send = async |hmac: String| {
        ctx.http
            .post(format!("{}/webhooks", ctx.url))
            .header("x-shopify-hmac-sha256", hmac)
            .header("x-shopify-topic", "products/update")
            .header("x-shopify-shop-domain", "test.myshopify.com")
            .body(body.clone())
            .send()
            .await
            .unwrap()
    };

    // The signature of the bytes is the one that counts...
    assert_ok(send(sign_webhook_body(&body, SECRET)).await).await;
    // ...and the signature of what a lossy conversion made of them is not one.
    let response = send(sign_webhook_body(&mangled, SECRET)).await;
    assert_error(response, 401, "Invalid webhook HMAC").await;
    assert_eq!(recorder.count(), 0);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn post_webhooks_runs_a_delivery_that_shopify_sends_again_once() {
    let recorder = Recorder::default();
    let ctx = TestContext::new(orders_create_config(&recorder)).await.unwrap();
    let body = r#"{"id":7}"#;
    let hmac = sign_webhook_body(body, SECRET);
    let send = async |header: &str, id: &str, hmac: &str| {
        ctx.http
            .post(format!("{}/webhooks", ctx.url))
            .header("x-shopify-hmac-sha256", hmac)
            .header("x-shopify-topic", "orders/create")
            .header("x-shopify-shop-domain", "test.myshopify.com")
            .header(header, id)
            .body(body)
            .send()
            .await
            .unwrap()
    };

    // The same delivery again, and again: acknowledged, and run once.
    for _ in 0..3 {
        assert_ok(send("x-shopify-webhook-id", "delivery-1", &hmac).await).await;
    }
    assert_eq!(recorder.count(), 1);

    // Another delivery of the same event is another delivery; the event id stands in for a missing webhook id.
    assert_ok(send("x-shopify-webhook-id", "delivery-2", &hmac).await).await;
    assert_ok(send("x-shopify-event-id", "event-1", &hmac).await).await;
    assert_ok(send("x-shopify-event-id", "event-1", &hmac).await).await;
    assert_eq!(recorder.count(), 3);

    // A forged request does not use up the id of a real one.
    let response = send("x-shopify-webhook-id", "delivery-3", "forged").await;
    assert_error(response, 401, "Invalid webhook HMAC").await;
    assert_ok(send("x-shopify-webhook-id", "delivery-3", &hmac).await).await;
    assert_eq!(recorder.count(), 4);

    // Without an id, nothing says it is a repeat.
    assert_ok(deliver(&ctx, "orders/create", "test.myshopify.com", body).await).await;
    assert_ok(deliver(&ctx, "orders/create", "test.myshopify.com", body).await).await;
    assert_eq!(recorder.count(), 6);
    ctx.shutdown().await.unwrap();
}

/// A handler that does what `behave` says for the number of its call, counting the calls.
fn scripted<F, Fut>(behave: F) -> (Webhook, Arc<std::sync::atomic::AtomicUsize>)
where
    F: Fn(usize) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = openshop::Result<()>> + Send + 'static,
{
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = calls.clone();
    let webhook =
        Webhook::new(move |_ctx: WebhookContext| behave(counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1));
    (webhook, calls)
}

fn calls_of(calls: &Arc<std::sync::atomic::AtomicUsize>) -> usize {
    calls.load(std::sync::atomic::Ordering::SeqCst)
}

fn delivery(ctx: &TestContext, client: &reqwest::Client, id: &str) -> reqwest::RequestBuilder {
    let body = r#"{"id":7}"#;
    client
        .post(format!("{}/webhooks", ctx.url))
        .header("x-shopify-hmac-sha256", sign_webhook_body(body, SECRET))
        .header("x-shopify-topic", "orders/create")
        .header("x-shopify-shop-domain", "test.myshopify.com")
        .header("x-shopify-webhook-id", id)
        .body(body)
}

#[tokio::test]
async fn post_webhooks_runs_again_a_delivery_whose_handler_failed() {
    let (webhook, calls) = scripted(|call| async move {
        if call == 1 {
            bail!("first attempt failed");
        }
        Ok(())
    });
    let config = builder().webhook("ORDERS_CREATE", webhook).build().unwrap();
    let ctx = TestContext::new(config).await.unwrap();

    // The failure is still answered 200, but the delivery was not handled.
    assert_ok(delivery(&ctx, &ctx.http, "delivery-1").send().await.unwrap()).await;
    assert_eq!(calls_of(&calls), 1);
    // The repeat runs, completes, and is the last to run.
    assert_ok(delivery(&ctx, &ctx.http, "delivery-1").send().await.unwrap()).await;
    assert_eq!(calls_of(&calls), 2);
    assert_ok(delivery(&ctx, &ctx.http, "delivery-1").send().await.unwrap()).await;
    assert_eq!(calls_of(&calls), 2);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn post_webhooks_runs_again_a_delivery_whose_request_was_cancelled() {
    let (webhook, calls) = scripted(|call| async move {
        if call == 1 {
            // Never ends: Shopify gives up first.
            std::future::pending::<()>().await;
        }
        Ok(())
    });
    let config = builder().webhook("ORDERS_CREATE", webhook).build().unwrap();
    let ctx = TestContext::new(config).await.unwrap();

    let impatient = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(1500))
        .build()
        .unwrap();
    let first = tokio::spawn(delivery(&ctx, &impatient, "delivery-1").send());
    while calls_of(&calls) == 0 {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // While the handler runs, a repeat is not taken for handled: Shopify is asked to come back.
    let meanwhile = delivery(&ctx, &ctx.http, "delivery-1").send().await.unwrap();
    assert_eq!(meanwhile.status(), 503);
    assert_eq!(meanwhile.headers()["retry-after"], "5");
    assert_eq!(calls_of(&calls), 1);

    // Shopify hangs up: the handler is cancelled.
    assert!(first.await.unwrap().is_err());
    let mut retry = delivery(&ctx, &ctx.http, "delivery-1").send().await.unwrap();
    for _ in 0..100 {
        if retry.status() != 503 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        retry = delivery(&ctx, &ctx.http, "delivery-1").send().await.unwrap();
    }
    assert_ok(retry).await;
    assert_eq!(calls_of(&calls), 2, "the retry ran the handler");

    // Completed: the next repeat is acknowledged and not run.
    assert_ok(delivery(&ctx, &ctx.http, "delivery-1").send().await.unwrap()).await;
    assert_eq!(calls_of(&calls), 2);
    ctx.shutdown().await.unwrap();
}

// ─── app/uninstalled ─────────────────────────────────────────────────

/// The access token of the shop and whether it is marked uninstalled.
async fn installation_state(ctx: &TestContext, shop: &str) -> (Option<String>, bool) {
    sqlx::query_as("SELECT access_token, uninstalled_at IS NOT NULL FROM installations WHERE shop = $1")
        .bind(shop)
        .fetch_one(&ctx.db)
        .await
        .unwrap()
}

async fn installed(config: OpenShopConfig) -> TestContext {
    TestContext::with_options(config, TestOptions::installed())
        .await
        .unwrap()
}

#[tokio::test]
async fn post_webhooks_records_app_uninstalled_without_a_handler() {
    let ctx = installed(builder().build().unwrap()).await;
    let body = r#"{"id":1,"myshopify_domain":"test.myshopify.com"}"#;
    assert!(!installation_state(&ctx, "test.myshopify.com").await.1);

    assert_ok(deliver(&ctx, "app/uninstalled", "test.myshopify.com", body).await).await;
    assert_eq!(installation_state(&ctx, "test.myshopify.com").await, (None, true));

    // As the Admin API writes the topic, and again: nothing more to do.
    assert_ok(deliver(&ctx, "APP_UNINSTALLED", "test.myshopify.com", body).await).await;
    assert_eq!(installation_state(&ctx, "test.myshopify.com").await, (None, true));
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn post_webhooks_records_app_uninstalled_before_its_handler_runs() {
    // What the handler finds of the installation when it runs.
    type Seen = Arc<Mutex<Vec<(Option<String>, bool)>>>;
    let seen: Seen = Arc::default();
    let recorded = seen.clone();
    let handler = Webhook::new(move |ctx: WebhookContext| {
        let recorded = recorded.clone();
        async move {
            let state =
                sqlx::query_as("SELECT access_token, uninstalled_at IS NOT NULL FROM installations WHERE shop = $1")
                    .bind(&ctx.shop)
                    .fetch_one(&ctx.db)
                    .await?;
            recorded.lock().unwrap().push(state);
            Ok(())
        }
    });
    let ctx = installed(builder().webhook("app/uninstalled", handler).build().unwrap()).await;

    let body = r#"{"myshopify_domain":"test.myshopify.com"}"#;
    assert_ok(deliver(&ctx, "app/uninstalled", "test.myshopify.com", body).await).await;
    assert_eq!(*seen.lock().unwrap(), vec![(None, true)]);
    ctx.shutdown().await.unwrap();
}

#[tokio::test]
async fn post_webhooks_uninstalls_only_the_shop_the_payload_names() {
    let recorder = Recorder::default();
    let ctx = installed(
        builder()
            .webhook("APP_UNINSTALLED", recorder.webhook())
            .build()
            .unwrap(),
    )
    .await;

    // The header is not signed: a signed body of another shop, or of none,
    // uninstalls nothing. The handler still runs, as for any topic.
    for body in [r#"{"myshopify_domain":"other.myshopify.com"}"#, r#"{"id":1}"#] {
        assert_ok(deliver(&ctx, "app/uninstalled", "test.myshopify.com", body).await).await;
    }
    assert_eq!(recorder.count(), 2);
    assert_eq!(
        installation_state(&ctx, "test.myshopify.com").await,
        (Some("test-access-token".to_string()), false)
    );

    // The old route still uninstalls, for an app whose subscription points there.
    let body = r#"{"myshopify_domain":"test.myshopify.com"}"#;
    let response = ctx
        .http
        .post(format!("{}/auth/webhooks/app-uninstalled", ctx.url))
        .header("x-shopify-hmac-sha256", sign_webhook_body(body, SECRET))
        .header("x-shopify-topic", "app/uninstalled")
        .header("x-shopify-shop-domain", "test.myshopify.com")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_ok(response).await;
    assert_eq!(installation_state(&ctx, "test.myshopify.com").await, (None, true));
    // It runs no handler.
    assert_eq!(recorder.count(), 2);
    ctx.shutdown().await.unwrap();
}

// ─── ctx.logger ──────────────────────────────────────────────────────

#[tokio::test]
async fn a_handler_logs_for_its_shop_and_topic() {
    let handler = Webhook::new(|ctx: WebhookContext| async move {
        ctx.logger.info_with("Webhook logger check", json!({ "id": 7 }));
        ctx.logger.warn("Webhook logger warning");
        Ok(())
    });
    let ctx = TestContext::new(builder().webhook("orders/create", handler).build().unwrap())
        .await
        .unwrap();
    let logs = LogCapture::start().await;

    assert_ok(deliver(&ctx, "orders/create", "shop-logs.myshopify.com", r#"{"id":7}"#).await).await;

    let lines = logs.lines();
    let info = lines
        .iter()
        .find(|line| line.message == "[INFO] Webhook logger check")
        .expect("the line of the handler");
    assert_eq!(info.level, "info");
    assert_eq!(
        info.context,
        Some(json!({ "id": 7, "shop": "shop-logs.myshopify.com", "shopifyApp": "default", "topic": "orders/create" }))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.level == "warn" && line.message == "[WARN] Webhook logger warning")
    );
    drop(logs);
    ctx.shutdown().await.unwrap();
}
