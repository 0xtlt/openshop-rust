# Connect an external service

Use this guide to add a warehouse connector whose credentials are editable in
Shopify admin. Start with an existing OpenShop app and a warehouse API that
accepts authenticated `GET /health` and `POST /orders` requests. Adapt those two
paths and the authorization header to your service's API.

The generated tutorial provider is a logging stub. Replacing it with this code
makes real HTTP requests. Use the service's development credentials first.

## 1. Create the provider

The provider calls the warehouse over HTTP. Add an HTTP client and `serde` to
the `[dependencies]` of `Cargo.toml`. This is the version and feature set
OpenShop itself uses, so nothing is compiled twice:

```toml
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
serde = { version = "1", features = ["derive"] }
```

A provider is a struct. Its fields are the configuration a merchant saves, and
its methods are what flows do with it. Replace `src/providers/warehouse.rs`:

```rust,ignore
use openshop::{ProviderConfig, Result, bail, provider, validators};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Order {
    pub id: String,
}

/// What a merchant saves for the warehouse.
#[derive(ProviderConfig)]
pub struct Warehouse {
    #[field(label = "API URL", validate = validators::url())]
    api_url: String,
    #[field(label = "API key", password, validate = validators::non_empty())]
    api_key: String,
    #[field(label = "Batch size", validate = validators::integer_min(1))]
    batch_size: Option<u32>,
}

#[provider("warehouse")]
impl Warehouse {
    /// The connection check of the provider page.
    #[checker]
    async fn check(&self) -> Result<bool> {
        let response = self.request(reqwest::Method::GET, "health").send().await?;
        Ok(response.status().is_success())
    }

    /// Sends orders to the warehouse and returns how many were sent.
    pub async fn push(&self, orders: Vec<Order>) -> Result<usize> {
        for batch in orders.chunks(self.batch_size.unwrap_or(100) as usize) {
            let response = self.request(reqwest::Method::POST, "orders").json(batch).send().await?;
            if !response.status().is_success() {
                bail!("Warehouse push failed: {}", response.status());
            }
        }
        Ok(orders.len())
    }

    /// A private method is a helper: flows cannot call it.
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}/{path}", self.api_url.trim_end_matches('/'));
        reqwest::Client::new().request(method, url).bearer_auth(&self.api_key)
    }
}
```

The generated project already declares the module in `src/providers/mod.rs`
(`pub mod warehouse;`).

`#[derive(ProviderConfig)]` turns the fields into the form of the provider
page: a text input, a secret and an optional number. The validators reject
invalid URLs and empty API keys before a configuration is saved.
`#[provider("warehouse")]` makes the public `async` methods of the block the
methods of the provider; inside them, `self` is the configuration of the shop
the method runs for. For every option, see [Providers](../reference/providers.md).

## 2. Register it

In `src/main.rs`, register the provider on the builder, keeping the other
calls of your configuration. The generated project registers
`providers::warehouse::warehouse()`: replace that line.

```rust,ignore
fn config() -> Result<OpenShopConfig> {
    OpenShopConfig::builder()
        .provider(providers::warehouse::Warehouse::provider())
        // ... flows, crons and other settings ...
        .build()
}
```

The provider is registered under the name given to `#[provider]`, `warehouse`.

## 3. Save and check credentials

Open the embedded provider page, save the config, then run its check. Password
fields are write-only: the API returns `hasValue`, and submitting an empty
password preserves the stored secret.

The checker is manual. OpenShop does not call it before every flow method.
Saving a complete config marks the provider as configured. A successful check
then shows it as connected, and a failed check shows an error. Returning
`Ok(false)` records an unsuccessful check; returning an error answers HTTP 500.

## 4. Call the connector

Call the method inside a flow, on the connectors of the shop:

```rust,ignore
use crate::providers::warehouse::{Order, Warehouse};

let orders = vec![Order { id: "order-1".to_string() }];
let sent = ctx
    .step
    .run("push-orders", || async { Warehouse::on(&ctx.connectors).push(&orders).await })
    .await?;
```

`Warehouse::on(&ctx.connectors)` has the public methods of the provider. They
run with the configuration the shop saved, so the call passes the arguments
only, and the compiler checks them: passing anything other than a
`Vec<Order>` does not compile, and `sent` is a `usize`.

Whether the shop saved its credentials is a run-time question: without them
the call fails with
`Invalid configuration for provider "warehouse": missing field `apiUrl``. If
it fails with `Unknown provider "warehouse"`, check the registration on the
builder.

The flow of the generated project calls
`ctx.connectors.call("warehouse", "push", &orders)`, by name. That still
reaches the method, without the checks: the orders it passes must have the
fields of `Order`.

## 5. Verify the integration

Run `cargo check`, then use [Sync orders with a flow](define-flow.md) to
call the connector. Confirm the provider check succeeds, the flow completes, and
the warehouse receives the expected records. A non-2xx warehouse response fails
the step and can trigger a flow retry, so make writes idempotent.

Before saving production credentials, configure the persistent encryption key
as described in [Deploy to production](deploy-production.md). See
[provider secret behavior](../reference/providers.md#secret-behavior) for storage and
password-update rules.
