//! A provider written as a struct: `#[derive(ProviderConfig)]` for the form,
//! `#[provider]` on its `impl` block for the methods.

use crate::common::shop_connectors as connectors;
use openshop::builder::{Field, FieldOption, Flow, Provider};
use openshop::testing::TestContext;
use openshop::{OpenShopConfig, ProviderConfig, Result, RunOutcome, Value, json, provider, validators};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Order {
    id: String,
    total: u32,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Receipt {
    endpoint: String,
    authorized: bool,
    region: Option<String>,
    batches: usize,
    total: u32,
}

#[derive(Debug, ProviderConfig)]
struct Warehouse {
    #[field(label = "API URL", placeholder = "https://warehouse.example.com", validate = validators::url())]
    api_url: String,
    #[field(label = "API key", password, validate = validators::non_empty())]
    api_key: String,
    #[field(label = "Region", select("Europe" = "eu", "United States" = "us"))]
    region: Option<String>,
    #[field(label = "Batch size", validate = validators::integer_min(1))]
    batch_size: Option<u32>,
    #[field(label = "Sandbox", name = "useSandbox")]
    sandbox: bool,
}

#[provider("warehouse")]
impl Warehouse {
    #[checker]
    async fn check(&self) -> Result<bool> {
        Ok(self.api_key == "secret-1")
    }

    /// Sends orders to the warehouse, in batches.
    pub async fn push(&self, orders: Vec<Order>) -> Result<Receipt> {
        let batches = orders.chunks(self.batch_size.unwrap_or(50) as usize).count();
        Ok(Receipt {
            endpoint: self.endpoint(),
            authorized: self.api_key == "secret-1",
            region: self.region.clone(),
            batches,
            total: orders.iter().map(|order| order.total).sum(),
        })
    }

    /// Several arguments travel together.
    pub async fn move_stock(&self, sku: String, quantity: i64) -> Result<String> {
        Ok(format!("{quantity} x {sku} at {}", self.endpoint()))
    }

    pub async fn ping(&self) -> Result<String> {
        Ok(format!("pong from {}", self.endpoint()))
    }

    /// An error of another type than `anyhow::Error`.
    pub async fn parse(&self, text: String) -> std::result::Result<i64, std::num::ParseIntError> {
        text.parse()
    }

    /// Not public: a helper, not a method of the provider.
    fn endpoint(&self) -> String {
        let host = self.api_url.trim_end_matches('/');
        if self.sandbox {
            format!("{host}/sandbox")
        } else {
            host.to_string()
        }
    }
}

/// A provider without configuration.
#[derive(ProviderConfig)]
struct Clock;

#[provider("clock")]
impl Clock {
    pub async fn year(&self) -> Result<i32> {
        Ok(2026)
    }
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
        .provider(Warehouse::provider())
        .provider(Clock::provider())
        .flow(crate::common::noop_flow("sync"))
        .build()
        .expect("valid config");
    TestContext::new(config).await.expect("test context")
}

async fn save_config(ctx: &TestContext, config: Value) {
    crate::common::save_provider_config(ctx, "warehouse", config).await;
}

/// The same form, written with the builder.
fn built_by_hand() -> Provider {
    Provider::new("warehouse")
        .field(
            "apiUrl",
            Field::text("API URL")
                .placeholder("https://warehouse.example.com")
                .validate(validators::url()),
        )
        .field("apiKey", Field::password("API key").validate(validators::non_empty()))
        .field(
            "region",
            Field::select(
                "Region",
                [
                    FieldOption::new("Europe", "eu"),
                    FieldOption::new("United States", "us"),
                ],
            )
            .optional(),
        )
        .field(
            "batchSize",
            Field::number("Batch size")
                .optional()
                .validate(validators::integer_min(1)),
        )
        .field("useSandbox", Field::checkbox("Sandbox"))
}

#[test]
fn the_struct_is_the_form() {
    let provider = Warehouse::provider();
    assert_eq!(provider.name, "warehouse");

    let metadata = |provider: &Provider| -> Vec<(String, Value)> {
        provider
            .fields
            .iter()
            .map(|(key, field)| (key.clone(), Value::Object(field.metadata())))
            .collect()
    };
    assert_eq!(metadata(&provider), metadata(&built_by_hand()));
    assert_eq!(
        metadata(&provider)[2],
        (
            "region".to_string(),
            json!({
                "type": "select",
                "label": "Region",
                "options": [{ "label": "Europe", "value": "eu" }, { "label": "United States", "value": "us" }],
                "required": false,
            })
        )
    );
    // The validators are those of the fields.
    let validate = provider.fields["apiUrl"].validate.as_ref().expect("a validator");
    assert!(validate(&json!("not a url")).is_err());

    // Public async methods, and only them.
    let methods: Vec<&str> = provider.methods.keys().map(String::as_str).collect();
    assert_eq!(methods, ["push", "move_stock", "ping", "parse"]);
    assert!(provider.checker.is_some());

    assert!(Clock::provider().fields.is_empty());
    assert!(Clock::provider().checker.is_none());
    OpenShopConfig::builder()
        .provider(Warehouse::provider())
        .build()
        .unwrap();
}

#[test]
fn the_struct_reads_a_saved_configuration() {
    let config = |value: Value| value.as_object().cloned().unwrap();

    let warehouse = Warehouse::from_config(config(json!({
        "apiUrl": "https://warehouse.test",
        "apiKey": "secret-1",
        "region": "eu",
        "batchSize": 10,
        "useSandbox": true,
        "leftover": "ignored",
    })))
    .unwrap();
    assert_eq!(warehouse.api_url, "https://warehouse.test");
    assert_eq!(warehouse.region.as_deref(), Some("eu"));
    assert_eq!(warehouse.batch_size, Some(10));
    assert!(warehouse.sandbox);

    // Optional fields may be absent or null.
    let minimal = Warehouse::from_config(config(
        json!({ "apiUrl": "u", "apiKey": "k", "region": null, "useSandbox": false }),
    ))
    .unwrap();
    assert_eq!((minimal.region, minimal.batch_size), (None, None));

    assert_eq!(
        Warehouse::from_config(config(json!({ "apiUrl": "u" }))).unwrap_err(),
        "missing field `apiKey`"
    );
    assert_eq!(
        Warehouse::from_config(config(
            json!({ "apiUrl": "u", "apiKey": "k", "batchSize": "many", "useSandbox": false })
        ))
        .unwrap_err(),
        "field `batchSize`: invalid type: string \"many\", expected u32"
    );
    // The struct deserializes the same way.
    let error = serde_json::from_value::<Warehouse>(json!({ "apiUrl": "u" })).unwrap_err();
    assert_eq!(error.to_string(), "missing field `apiKey`");
}

#[tokio::test]
async fn the_methods_can_be_called_on_a_value() {
    // No framework involved: a struct and its methods.
    let warehouse = Warehouse {
        api_url: "https://warehouse.test/".to_string(),
        api_key: "secret-1".to_string(),
        region: None,
        batch_size: Some(1),
        sandbox: true,
    };
    let receipt = warehouse.push(orders()).await.unwrap();
    assert_eq!(receipt.endpoint, "https://warehouse.test/sandbox");
    assert_eq!((receipt.batches, receipt.total), (2, 42));
    assert!(warehouse.check().await.unwrap());
}

#[openshop::test(context())]
async fn the_connectors_run_the_methods_with_the_saved_configuration(ctx: &TestContext) {
    save_config(
        ctx,
        json!({
            "apiUrl": "https://warehouse.test",
            "apiKey": "secret-1",
            "region": "eu",
            "batchSize": "1",
            "useSandbox": "false",
        }),
    )
    .await;
    let connectors = connectors(ctx).await;
    let warehouse = Warehouse::on(&connectors);
    let orders = orders();

    // An argument is taken by reference or by value.
    let receipt: Receipt = warehouse.push(&orders).await.unwrap();
    assert_eq!(
        receipt,
        Receipt {
            endpoint: "https://warehouse.test".to_string(),
            authorized: true,
            region: Some("eu".to_string()),
            batches: 2,
            total: 42,
        }
    );
    assert_eq!(warehouse.push(orders.clone()).await.unwrap().total, 42);

    let moved: String = warehouse.move_stock("SKU-1".to_string(), 3).await.unwrap();
    assert_eq!(moved, "3 x SKU-1 at https://warehouse.test");
    let sku = "SKU-2".to_string();
    assert_eq!(
        warehouse.move_stock(&sku, &-1).await.unwrap(),
        "-1 x SKU-2 at https://warehouse.test"
    );
    assert_eq!(warehouse.ping().await.unwrap(), "pong from https://warehouse.test");
    assert_eq!(Clock::on(&connectors).year().await.unwrap(), 2026);

    // The error of a method reaches the caller.
    assert_eq!(warehouse.parse("12".to_string()).await.unwrap(), 12);
    let error = warehouse.parse("twelve".to_string()).await.unwrap_err();
    assert_eq!(error.to_string(), "invalid digit found in string");

    // The same methods through their constants and by name.
    assert_eq!(connectors.invoke(Warehouse::PUSH, &orders).await.unwrap().batches, 2);
    assert_eq!(
        connectors
            .invoke(Warehouse::MOVE_STOCK, &("SKU-3".to_string(), 1))
            .await
            .unwrap(),
        "1 x SKU-3 at https://warehouse.test"
    );
    let by_name = connectors
        .call("warehouse", "move_stock", json!(["SKU-4", 2]))
        .await
        .unwrap();
    assert_eq!(by_name, "2 x SKU-4 at https://warehouse.test");
}

#[openshop::test(context())]
async fn a_shop_without_configuration_fails_before_the_method_runs(ctx: &TestContext) {
    let connectors = connectors(ctx).await;

    let error = Warehouse::on(&connectors).ping().await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "Invalid configuration for provider \"warehouse\": missing field `apiUrl`"
    );
    // A provider without fields needs none.
    assert_eq!(Clock::on(&connectors).year().await.unwrap(), 2026);
}

#[openshop::test(context())]
async fn the_form_validates_what_the_struct_expects(ctx: &TestContext) {
    let save = |config: Value| {
        ctx.put("/api/providers/warehouse")
            .json(&json!({ "config": config }))
            .send()
    };

    let refused = save(json!({ "apiUrl": "nope", "apiKey": "k", "useSandbox": false }))
        .await
        .unwrap();
    assert_eq!(refused.status(), 400);
    let body: Value = refused.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .starts_with("Field \"apiUrl\": must be a URL"),
        "{body}"
    );

    let refused = save(json!({ "apiUrl": "https://warehouse.test", "useSandbox": false }))
        .await
        .unwrap();
    let body: Value = refused.json().await.unwrap();
    assert_eq!(body["error"], "Field \"apiKey\" is required");

    // The connection check is the method marked `#[checker]`.
    for (api_key, expected) in [("secret-1", true), ("wrong", false)] {
        save_config(
            ctx,
            json!({ "apiUrl": "https://warehouse.test", "apiKey": api_key, "useSandbox": false }),
        )
        .await;
        let checked: Value = crate::common::check_provider(ctx, "warehouse").await;
        assert_eq!(checked, json!({ "ok": expected }), "{api_key}");
    }

    let listed: Value = ctx.get("/api/providers").send().await.unwrap().json().await.unwrap();
    let warehouse = &listed[0];
    assert_eq!(warehouse["configured"], true);
    assert_eq!(
        warehouse["config"],
        json!({ "apiUrl": "https://warehouse.test", "useSandbox": false })
    );
    assert_eq!(warehouse["fields"]["apiKey"]["hasValue"], true);
}

#[tokio::test]
async fn flows_call_the_provider_and_tests_fake_it() {
    let flow = Flow::new("pushOrders", |ctx| async move {
        let receipt = Warehouse::on(&ctx.connectors).push(&orders()).await?;
        ctx.step
            .run("record", || async { Ok(json!({ "batches": receipt.batches })) })
            .await?;
        Ok(())
    });
    let config = OpenShopConfig::builder()
        .provider(Warehouse::provider())
        .flow(flow)
        .build()
        .unwrap();
    let ctx = TestContext::new(config).await.unwrap();

    // The constant of a method names its fake.
    let push = ctx.fakes.for_call(Warehouse::PUSH);
    push.returns(Receipt {
        endpoint: "fake".to_string(),
        authorized: true,
        region: None,
        batches: 7,
        total: 42,
    });
    assert_eq!(
        ctx.run_flow("pushOrders", json!({})).await.unwrap(),
        RunOutcome::Completed
    );
    assert_eq!(push.call_count(), 1);
    assert_eq!(push.last_call().unwrap().args, orders());
    let (output,): (Value,) = sqlx::query_as("SELECT output FROM step_results WHERE step_name = 'record'")
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(output, json!({ "batches": 7 }));

    // A fake without a configured result returns `null`, which is not a receipt.
    ctx.reset_fakes();
    let result = ctx.run_flow("pushOrders", json!({})).await.unwrap();
    assert!(
        matches!(
            result,
            RunOutcome::Failed { ref error, .. }
                if error.starts_with("Invalid result from \"warehouse.push\": invalid type: null")
        ),
        "{result:?}"
    );
    ctx.shutdown().await.unwrap();
}

#[test]
fn a_provider_is_registered_under_its_name() {
    let error = OpenShopConfig::builder()
        .provider_as("main", Warehouse::provider())
        .build()
        .err()
        .expect("another key");
    assert_eq!(
        error.to_string(),
        "[openshop] Invalid config: providers.main.methods.push implements a call declared for provider \
         \"warehouse\"; declare it as ProviderCall::new(\"main\", \"push\")"
    );
}
