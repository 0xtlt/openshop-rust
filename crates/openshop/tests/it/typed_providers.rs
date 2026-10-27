//! Typed provider calls: a `ProviderCall` constant shared by the provider that
//! implements it and the code that invokes it.

use crate::common::{noop_flow, shop_connectors as connectors};
use openshop::builder::{Field, Provider, ProviderCall};
use openshop::testing::TestContext;
use openshop::{OpenShopConfig, Value, json, validators};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WarehouseConfig {
    api_url: String,
    api_key: String,
    batch_size: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Order {
    id: String,
    total: u32,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct PushReceipt {
    endpoint: String,
    authorized: bool,
    batch_size: u32,
    accepted: usize,
    total: u32,
}

const PUSH: ProviderCall<Vec<Order>, PushReceipt> = ProviderCall::new("warehouse", "push");
const PING: ProviderCall<(), String> = ProviderCall::new("warehouse", "ping");

fn warehouse_named(name: &str) -> Provider {
    Provider::new(name)
        .field("apiUrl", Field::text("API URL").validate(validators::url()))
        .field("apiKey", Field::password("API key"))
        .field("batchSize", Field::number("Batch size").optional())
        .typed_checker(|config: WarehouseConfig| async move { Ok(config.api_key == "secret-1") })
        .handle(PUSH, |config: WarehouseConfig, orders| async move {
            Ok(PushReceipt {
                endpoint: config.api_url,
                authorized: config.api_key == "secret-1",
                batch_size: config.batch_size.unwrap_or(50),
                accepted: orders.len(),
                total: orders.iter().map(|order| order.total).sum(),
            })
        })
        .handle(PING, |config: WarehouseConfig, ()| async move {
            Ok(format!("pong from {}", config.api_url))
        })
}

fn warehouse() -> Provider {
    warehouse_named("warehouse")
}

fn orders() -> Vec<Order> {
    vec![
        Order {
            id: "#1001".to_string(),
            total: 40,
        },
        Order {
            id: "#1002".to_string(),
            total: 2,
        },
    ]
}

async fn context() -> TestContext {
    let config = OpenShopConfig::builder()
        .provider(warehouse())
        .flow(noop_flow("sync"))
        .build()
        .expect("valid config");
    TestContext::new(config).await.expect("test context")
}

async fn save_config(ctx: &TestContext, config: Value) {
    crate::common::save_provider_config(ctx, "warehouse", config).await;
}

#[openshop::test(context())]
async fn a_typed_call_receives_the_stored_configuration_and_returns_a_typed_result(ctx: &TestContext) {
    save_config(
        ctx,
        json!({ "apiUrl": "https://warehouse.test", "apiKey": "secret-1", "batchSize": "25" }),
    )
    .await;
    let connectors = connectors(ctx).await;

    let receipt = connectors.invoke(PUSH, &orders()).await.unwrap();
    assert_eq!(
        receipt,
        PushReceipt {
            endpoint: "https://warehouse.test".to_string(),
            authorized: true,
            batch_size: 25,
            accepted: 2,
            total: 42,
        }
    );
    assert_eq!(
        connectors.invoke(PING, &()).await.unwrap(),
        "pong from https://warehouse.test"
    );
}

#[tokio::test]
async fn arguments_can_be_passed_in_their_borrowed_form() {
    const ECHO: ProviderCall<String, String> = ProviderCall::new("echo", "say");
    let provider = Provider::new("echo").handle(ECHO, |_config: Value, text| async move { Ok(format!("{text}!")) });
    let ctx = TestContext::new(OpenShopConfig::builder().provider(provider).build().unwrap())
        .await
        .unwrap();
    let connectors = ctx.app.connectors(&ctx.shop, "default");

    // `&str` for a `String`, and no saved configuration needed for a `Value`.
    assert_eq!(connectors.invoke(ECHO, "hello").await.unwrap(), "hello!");
    assert_eq!(connectors.invoke(ECHO, &"hello".to_string()).await.unwrap(), "hello!");
    ctx.shutdown().await.unwrap();

    let ctx = context().await;
    save_config(
        &ctx,
        json!({ "apiUrl": "https://warehouse.test", "apiKey": "secret-1" }),
    )
    .await;
    let orders = orders();
    let receipt = self::connectors(&ctx).await.invoke(PUSH, &orders[..1]).await.unwrap();
    assert_eq!((receipt.accepted, receipt.total), (1, 40));
    ctx.shutdown().await.unwrap();
}

#[openshop::test(context())]
async fn optional_fields_left_blank_are_absent_from_the_typed_configuration(ctx: &TestContext) {
    save_config(ctx, json!({ "apiUrl": "https://warehouse.test", "apiKey": "other" })).await;

    let receipt = connectors(ctx).await.invoke(PUSH, &orders()).await.unwrap();
    assert_eq!(receipt.batch_size, 50);
    assert!(!receipt.authorized);
}

#[openshop::test(context())]
async fn a_typed_call_is_also_reachable_by_name(ctx: &TestContext) {
    save_config(ctx, json!({ "apiUrl": "https://warehouse.test", "apiKey": "secret-1" })).await;
    let connectors = connectors(ctx).await;

    let receipt = connectors
        .call("warehouse", "push", json!([{ "id": "#1", "total": 7 }]))
        .await
        .unwrap();
    assert_eq!(receipt["accepted"], 1);
    assert_eq!(receipt["total"], 7);

    let error = connectors
        .call("warehouse", "push", json!([{ "id": "#1" }]))
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Invalid arguments for \"warehouse.push\": missing field `total`"
    );
}

#[openshop::test(context())]
async fn a_provider_that_is_not_configured_fails_with_the_missing_field(ctx: &TestContext) {
    let error = connectors(ctx).await.invoke(PUSH, &orders()).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "Invalid configuration for provider \"warehouse\": missing field `apiUrl`"
    );
}

#[tokio::test]
async fn invoking_a_call_that_is_not_registered_fails_by_name() {
    const ARCHIVE: ProviderCall<String, ()> = ProviderCall::new("warehouse", "archive");
    const ERP_SYNC: ProviderCall<(), ()> = ProviderCall::new("erp", "sync");
    let ctx = context().await;
    let connectors = connectors(&ctx).await;

    let error = connectors.invoke(ARCHIVE, &"#1".to_string()).await.unwrap_err();
    assert_eq!(error.to_string(), "Unknown provider method \"archive\"");
    let error = connectors.invoke(ERP_SYNC, &()).await.unwrap_err();
    assert_eq!(error.to_string(), "Unknown provider \"erp\"");
    ctx.shutdown().await.unwrap();
}

#[openshop::test(context())]
async fn the_typed_checker_receives_the_stored_configuration(ctx: &TestContext) {
    let unconfigured: Value = crate::common::check_provider(ctx, "warehouse").await;
    assert_eq!(
        unconfigured,
        json!({
            "ok": false,
            "error": "Error: Invalid configuration for provider \"warehouse\": missing field `apiUrl`",
        })
    );

    for (api_key, expected) in [("secret-1", true), ("wrong", false)] {
        save_config(ctx, json!({ "apiUrl": "https://warehouse.test", "apiKey": api_key })).await;
        let checked: Value = crate::common::check_provider(ctx, "warehouse").await;
        assert_eq!(checked, json!({ "ok": expected }), "{api_key}");
    }
}

#[test]
fn a_call_must_name_the_key_its_provider_is_registered_under() {
    let error = OpenShopConfig::builder()
        .provider_as("mainWarehouse", warehouse())
        .build()
        .err()
        .expect("mismatched provider key");
    assert_eq!(
        error.to_string(),
        "[openshop] Invalid config: providers.mainWarehouse.methods.push implements a call declared for provider \
         \"warehouse\"; declare it as ProviderCall::new(\"mainWarehouse\", \"push\")"
    );

    // The name of the provider is free as long as the key matches the calls.
    let config = OpenShopConfig::builder()
        .provider_as("warehouse", warehouse_named("Main warehouse"))
        .build()
        .unwrap();
    assert_eq!(config.providers["warehouse"].name, "Main warehouse");
}

// ─── Builder ─────────────────────────────────────────────────────────

fn erp() -> Provider {
    Provider::new("erp").field("token", Field::password("Token"))
}

#[test]
fn providers_and_flows_keep_the_order_they_are_registered_in() {
    let config = OpenShopConfig::builder()
        .provider(warehouse())
        .provider(erp())
        .flow(noop_flow("syncOrders"))
        .flow(noop_flow("syncProducts"))
        .flow(noop_flow("cleanup"))
        .build()
        .unwrap();

    assert_eq!(config.providers.keys().collect::<Vec<_>>(), ["warehouse", "erp"]);
    assert_eq!(
        config.flows.keys().collect::<Vec<_>>(),
        ["syncOrders", "syncProducts", "cleanup"]
    );
}
