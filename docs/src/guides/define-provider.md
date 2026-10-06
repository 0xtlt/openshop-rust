# Connect an external service

Use this guide to add a warehouse connector whose credentials are editable in
Shopify admin. Start with an existing OpenShop app and a warehouse API that
accepts authenticated `GET /health` and `POST /orders` requests. Adapt those two
paths and the authorization header to your service's API.

The generated tutorial provider is a logging stub. Replacing it with this code
makes real HTTP requests. Use the service's development credentials first.

## 1. Create the provider

The provider calls the warehouse over HTTP. Add an HTTP client to the
`[dependencies]` of `Cargo.toml`. This is the version and feature set OpenShop
itself uses, so nothing is compiled twice:

```toml
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
```

Create or replace `src/providers/warehouse.rs`:

```rust,ignore
use openshop::{FieldDef, JsonMap, Provider, Value, bail, validators};

fn text<'a>(config: &'a JsonMap, key: &str) -> &'a str {
    config.get(key).and_then(Value::as_str).unwrap_or_default()
}

pub fn warehouse() -> Provider {
    Provider::new("warehouse")
        .field("apiUrl", FieldDef::text("API URL").validate(validators::url()))
        .field("apiKey", FieldDef::password("API key").validate(validators::non_empty()))
        .field(
            "batchSize",
            FieldDef::number("Batch size").optional().validate(validators::integer_min(1)),
        )
        .transformer(|mut data| {
            if let Some(Value::String(api_url)) = data.get("apiUrl").cloned() {
                data.insert("apiUrl".into(), Value::from(api_url.trim_end_matches('/')));
            }
            Value::Object(data)
        })
        .checker(|config| async move {
            let response = reqwest::Client::new()
                .get(format!("{}/health", text(&config, "apiUrl")))
                .bearer_auth(text(&config, "apiKey"))
                .send()
                .await?;
            Ok(response.status().is_success())
        })
        .typed_method("push", |config, rows: Vec<Value>| async move {
            let response = reqwest::Client::new()
                .post(format!("{}/orders", text(&config, "apiUrl")))
                .bearer_auth(text(&config, "apiKey"))
                .json(&rows)
                .send()
                .await?;
            if !response.status().is_success() {
                bail!("Warehouse push failed: {}", response.status());
            }
            Ok(())
        })
}
```

The generated project already declares the module in `src/providers/mod.rs`
(`pub mod warehouse;`).

The transformer removes trailing slashes from the URL. The field validators
reject invalid URLs and empty API keys before configuration is saved. For the
full save pipeline and optional fields, see [Providers](../reference/providers.md).

## 2. Register it

The generated `src/main.rs` already registers this provider. In another project,
add it to the builder, keeping the other calls of your configuration:

```rust,ignore
fn config() -> Result<OpenShopConfig> {
    OpenShopConfig::builder()
        .provider(providers::warehouse::warehouse())
        // ... flows, crons and other settings ...
        .build()
}
```

`.provider(..)` registers the provider under its own name, `warehouse`. Flows
look connectors up by that name at run time: method arguments travel as JSON,
and `typed_method` deserializes them into the type the method declares.

## 3. Save and check credentials

Open the embedded provider page, save the config, then run its check. Password
fields are write-only: the API returns `hasValue`, and submitting an empty
password preserves the stored secret.

The checker is manual. OpenShop does not call it before every flow method.
Saving a complete config marks the provider as configured. A successful check
then shows it as connected, and a failed check shows an error. Returning
`Ok(false)` records an unsuccessful check; returning an error answers HTTP 500.

## 4. Call the connector

Call the method inside a flow, through `ctx.connectors`:

```rust,ignore
ctx.step
    .run("push-orders", || async {
        ctx.connectors
            .call("warehouse", "push", json!([{ "id": "order-1" }]))
            .await?;
        Ok(())
    })
    .await?;
```

The provider method receives saved config as its first argument, but the
connector call omits it and passes a single JSON-serializable argument. Use
`call_as::<T>(..)` to deserialize the method's return value. If the call fails
with `Unknown provider "warehouse"`, check the registration on the builder.

## 5. Verify the integration

Run `cargo check`, then use [Sync orders with a flow](define-flow.md) to
call the connector. Confirm the provider check succeeds, the flow completes, and
the warehouse receives the expected records. A non-2xx warehouse response fails
the step and can trigger a flow retry, so make writes idempotent.

Before saving production credentials, configure the persistent encryption key
as described in [Deploy to production](deploy-production.md). See
[provider secret behavior](../reference/providers.md#secret-behavior) for storage and
password-update rules.
