# GraphQL

Use this reference for Shopify Admin GraphQL operations in OpenShop app and framework code.

There is no GraphQL code generation in the Rust version. A query is a string,
the response is JSON, and nothing checks one against the other at compile time.
The rules below keep that boundary small and tested.

## Expected Pattern

Keep each operation in a `const` and name the operation. Call it through the
shop's `ShopifyClient`:

```rust
use openshop::{Result, ShopifyClient, Value, json};

const GET_PRODUCT: &str = "#graphql
  query GetProduct($id: ID!) {
    product(id: $id) {
      id
      title
      totalInventory
    }
  }
";

pub async fn product_title(shopify: &ShopifyClient, id: &str) -> Result<String> {
    let data: Value = shopify.graphql(GET_PRODUCT, Some(json!({ "id": id }))).await?;
    let title = data
        .pointer("/product/title")
        .and_then(Value::as_str)
        .ok_or_else(|| openshop::anyhow!("Product {id} not found"))?;
    Ok(title.to_string())
}
```

- `graphql(query, variables)` returns the `data` object of the response as a `serde_json::Value`. Pass `None` when the operation has no variables.
- The demo app and the template start every query with a `#graphql` comment line. Keep it for consistency.
- A named operation (`query GetProduct`) lets a test recognize the request by its name.
- In a flow, the client is `ctx.shopify`. In custom admin loaders and actions it is also `ctx.shopify`. In webhooks, proxy handlers, and server routes, create it with `ctx.app.shopify_client(&ctx.shop, &ctx.shopify_app).await?`.

## Prefer Typed Responses

When more than one or two fields are read, describe the response with serde
types and call `graphql_as::<T>()`. It deserializes the `data` object:

```rust
use openshop::{Result, ShopifyClient, json};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
struct GetProductData {
    product: Option<Product>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Product {
    pub id: String,
    pub title: String,
    pub total_inventory: Option<i64>,
}

pub async fn get_product(shopify: &ShopifyClient, id: &str) -> Result<Product> {
    let data: GetProductData = shopify
        .graphql_as(GET_PRODUCT, Some(json!({ "id": id })))
        .await?;
    data.product
        .ok_or_else(|| openshop::anyhow!("Product {id} not found"))
}
```

- Mirror the selection set: one struct per selected object, `#[serde(rename_all = "camelCase")]` for field names.
- Use `Option<T>` for every field the Admin API schema declares nullable, and for objects looked up by ID. A missing `Option` turns a `null` into a deserialization error at run time.
- Keep the query `const` and its response types in the same module so they change together.
- A type returned from `ctx.step.run()` must also derive `Serialize`, because step results are stored as JSON.

## Do Not Index Blindly

Do not write this pattern:

```rust
let title = data["product"]["title"].as_str().unwrap().to_string();
```

Indexing a `Value` with a missing key yields `null` instead of failing, and the
`unwrap()` then panics far from the cause. It hides renamed fields, wrong
variables, and nullable results.

If a response does not have the expected shape:
- Compare the selection set in the `const` with the response type or the pointers used.
- Check nullability in the Admin API schema for the API version in use.
- Read the error: `graphql()` fails with `[openshop] GraphQL errors: …` when Shopify returns `errors`, with `[openshop] Shopify GraphQL error (<status>): …` for a non-2xx response, and with `[openshop] No access token found for app "…" and shop "…". Is the app installed?` when the shop has no stored token.
- Fix the query or the type instead of adding a fallback value that masks the mismatch.

## Acceptable Narrowing

Handle absence explicitly after the response is typed:

```rust
let product = data.product.ok_or_else(|| openshop::anyhow!("Product not found"))?;
```

For a small read through `Value`, use `pointer()` with an explicit default or
error, as the template does, rather than chained indexing.

Use explicit domain types for transformed outputs, not for forcing raw GraphQL data:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WarehouseOrder {
    id: String,
    name: String,
    items: usize,
}
```

## Test Every Operation

Because the compiler does not check queries, each operation needs a test that
runs it against a scripted response. `TestContext` routes Shopify calls to a
`MockTransport`:

```rust
use openshop::testing::{MockTransport, TestContext, TestOptions};
use openshop::{OpenShopConfig, json};

#[tokio::test]
async fn reads_a_product() {
    let ctx = TestContext::with_options(
        OpenShopConfig::builder().build().unwrap(),
        TestOptions {
            access_token: Some("test-access-token".to_string()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    ctx.transport.respond_with(|request| {
        assert!(request.body["query"].as_str().unwrap_or_default().contains("GetProduct"));
        assert_eq!(request.body["variables"]["id"], "gid://shopify/Product/1");
        Ok(MockTransport::json(
            200,
            json!({ "data": { "product": {
                "id": "gid://shopify/Product/1",
                "title": "Test product",
                "totalInventory": 12,
            } } }),
        ))
    });

    let shopify = ctx.app.shopify_client(&ctx.shop, "default").await.unwrap();
    let product = get_product(&shopify, "gid://shopify/Product/1").await.unwrap();

    assert_eq!(product.title, "Test product");
    assert_eq!(product.total_inventory, Some(12));
    assert_eq!(ctx.transport.requests().len(), 1);
    ctx.shutdown().await.unwrap();
}
```

- Set `access_token` in `TestOptions`; without it the shop is not installed and `graphql()` fails before sending anything.
- The responder receives the outbound request. `request.body["query"]` and `request.body["variables"]` are what the client sent.
- The response body must have the Shopify envelope: `{ "data": … }` or `{ "errors": [{ "message": … }] }`.
- A transport without a responder answers `404 not mocked`. `respond_json(body)` answers every request with the same body.
- `ctx.transport.requests()` and `last_request()` return the recorded requests; `clear()` forgets them.
- For a flow, script the transport and call `ctx.run_flow("name", json!({ … }))`. The flow's `ctx.shopify` uses the same transport. See `apps/demo/tests/common/mod.rs` for fixtures selected by operation name.
- Write fixtures with the same field casing and nullability Shopify returns, including a `null` case for nullable fields.

## Framework Code

When changing GraphQL support in `crates/openshop`:
- The client lives in `crates/openshop/src/shopify/client.rs`. `SHOPIFY_API_VERSION` is the default Admin API version; `create_shopify_client(app, shop, shopify_app, Some(version))` selects another one.
- Send every Shopify request through the `HttpTransport` trait (`crates/openshop/src/runtime/transport.rs`) so tests can replace it with `MockTransport`.
- The error messages of `graphql()` are the same as in the TypeScript implementation. Keep them, or document the change in `docs/src/reference/differences.md`.
- Update `docs/src`, `apps/demo`, and `crates/openshop-cli/templates/minimal` when user-facing usage changes.
