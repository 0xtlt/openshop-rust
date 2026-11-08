# Connect an external service

Use this guide to add a warehouse connector whose credentials are editable in
Shopify admin. Start with an existing OpenShop app and a warehouse API that
accepts authenticated `GET /health` and `POST /orders` requests. Adapt those two
paths and the authorization header to your service's API.

The generated tutorial provider is a logging stub. Replacing it with this code
makes real HTTP requests. Use the service's development credentials first.

## 1. Create the provider

The provider calls the warehouse over HTTP. Add an HTTP client to the
`[dependencies]` of `Cargo.toml`, next to `serde`, which the generated project
already lists. This is the version and feature set OpenShop itself uses, so
nothing is compiled twice:

```toml
reqwest = { version = "0.13", default-features = false, features = ["json", "rustls"] }
```

A provider is a struct. Its fields are the configuration a merchant saves, and
its methods are what flows do with it. Replace `src/providers/warehouse.rs`:

```rust,ignore
use openshop::prelude::*;
use serde::{Deserialize, Serialize};

/// One order as the warehouse receives it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Order {
    pub id: String,
    pub name: String,
}

/// What a merchant saves for the warehouse.
#[derive(ProviderConfig)]
pub struct Warehouse {
    #[field(label = "API URL", url)]
    api_url: String,
    #[field(label = "API key", password, non_empty)]
    api_key: String,
    #[field(range(min = 1, max = 1000))]
    batch_size: Option<u32>,
}

/// Named `warehouse`, after the struct.
#[provider]
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
page: a text input, a secret and an optional number. `batch_size` has no
`label`, so the form labels it after its name, "Batch size". The constraints
are options of `#[field(…)]`: `url` and `non_empty` reject invalid URLs and
empty API keys, and `range(min = 1, max = 1000)` a batch size out of bounds,
before a configuration is saved. `length(min = …, max = …)` bounds the length
of a text, and `validate = <validator>` takes one of `openshop::validators` or
a validator of your own for anything else.

`#[provider]` makes the public `async` methods of the block the methods of the
provider; inside them, `self` is the configuration of the shop the method runs
for. The provider is named after the struct, `warehouse`;
`#[provider("wms")]` states another name. For every option, see
[Providers](../reference/providers.md).

## 2. Register it

In `src/main.rs`, list the provider in the `providers` of `app!`, keeping its
other entries. The generated project already does:

```rust,ignore
use openshop::prelude::*;

use providers::warehouse::Warehouse;

static APP: App = app! {
    providers: [Warehouse],
    // ... flows, crons and other settings ...
};
```

The provider is registered under its name, `warehouse`, which is also the
constant `Warehouse::NAME`.

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

let orders = vec![Order { id: "gid://shopify/Order/1".to_string(), name: "#1001".to_string() }];
let sent = ctx
    .step
    .run("push-orders", async || { Warehouse::on(&ctx.connectors).push(&orders).await })
    .await?;
```

`Warehouse::on(&ctx.connectors)` has the public methods of the provider. They
run with the configuration the shop saved, so the call passes the arguments
only, and the compiler checks them: passing anything other than a
`Vec<Order>` does not compile, and `sent` is a `usize`. The flow of the
generated project calls `push` this way, with the orders it read from Shopify.

`ctx.connectors` is there in every context that belongs to a shop: a flow, a
webhook, a proxy handler, a loader or an action of an admin page, an MCP tool,
and what `ctx.for_shop(..)` returns in a server route. The configuration of the
shop is read at the first call.

Whether the shop saved its credentials is a run-time question: without them
the call fails with
`Invalid configuration for provider "warehouse": missing field `apiUrl``. If
it fails with `Unknown provider "warehouse"`, check that the application lists
the provider.

A method can also be called by name, with
`ctx.connectors.call("warehouse", "push", &orders)`, or
`call_as::<usize>(..)` to read its result. That reaches the same method
without the checks of the compiler: keep it for a provider or a method only
known at run time.

## 5. Verify the integration

Run `cargo check`, then use [Sync orders with a flow](define-flow.md) to
call the connector. Confirm the provider check succeeds, the flow completes, and
the warehouse receives the expected records. A non-2xx warehouse response fails
the step and can trigger a flow retry, so make writes idempotent.

Before saving production credentials, configure the persistent encryption key
as described in [Deploy to production](deploy-production.md). See
[provider secret behavior](../reference/providers.md#secret-behavior) for storage and
password-update rules.
