# GraphQL

Use this reference for Shopify Admin GraphQL operations in OpenShop app and framework code.

Write an operation known in advance with `graphql!`, the macro of
`shopify_api` that `openshop` re-exports: it is checked against
Shopify's Admin schema when the crate compiles, and its variables and its
result are typed. The crate that writes one depends on `openshop` alone (the
generated code names `openshop::shopify_api`) and pins its version in `[package.metadata.shopify_api] api-version`; the first build
downloads `graphql/admin.schema.json`, which is committed. `graphql()` takes any string and checks
nothing at compile time; keep them for operations built at run time, and
follow the rules of the later sections for those.

## Typed Operations

```rust
use openshop::ShopifyClient;
use openshop::prelude::*;

pub async fn product_title(shopify: &ShopifyClient, id: &str) -> Result<String> {
    let data = shopify
        .execute(graphql!(
            "#graphql
              query GetProduct($id: ID!) {
                product(id: $id) {
                  id
                  title
                  totalInventory
                }
              }
            ",
            id = id
        ))
        .await?;
    let product = data.product.ok_or_else(|| anyhow!("Product {id} not found"))?;
    Ok(product.title)
}
```

- Variables follow the operation as `name = value`. Required ones are mandatory; nullable ones and those with a default can be left out.
- The result mirrors the selection: snake-case fields, `Option` for nullable fields and for fields under a fragment on another type, `Vec` for lists, `String` for IDs, enum values and Shopify's string-encoded scalars, `i64` for `Int`.
- The result types derive `Serialize` and `Deserialize`: return them from `ctx.step.run()` directly.
- The types of an inline operation have no name. To return them from a function or share an operation, use the module form: `graphql! { pub mod get_product = "…"; }`, then `get_product::Data`, and `get_product::Variables { … }` or `get_product::operation()`, which `execute` takes.
- Write input objects as literals checked field by field (`product = { id: id, title: title, vendor?: maybe_vendor }`), or build them ahead with `openshop::shopify::input!(ProductUpdateInput { … })`. Reference: https://docs.rs/shopify_api/latest/shopify_api/macro.graphql.html.
- Do not work around a compile error by switching to `graphql()`: fix the operation. The error names the field, argument or variable and the API version it was checked against.
- Operations go to the version of the schema they were checked against. To move to another version, change `api-version` and commit the schema the next build downloads. `ShopifyClient::with_api_version(ApiVersion)` pins a client to a version.
- For pagination, bulk operations or webhook subscriptions, use `ctx.shopify.api().await?`, the `shopify_api::Shopify` client of the shop.
- A test still scripts the answer with `MockTransport`. An answer that leaves out a non-null field fails with a `shopify_api::Error::Decode` that names the path of the mismatch.

Reference: `docs/src/reference/graphql.md`.

## Untyped Operations

The rest of this page applies to `graphql()`.

## Expected Pattern

Keep each operation in a `const` and name the operation. Call it through the
shop's `ShopifyClient`:

```rust
use openshop::ShopifyClient;
use openshop::prelude::*;

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
    let data: Value = shopify.graphql(GET_PRODUCT, json!({ "id": id })).await?;
    let title = data
        .pointer("/product/title")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Product {id} not found"))?;
    Ok(title.to_string())
}
```

- `graphql(query, variables)` returns the `data` object of the response as a `serde_json::Value`. Pass `()` when the operation has no variables.
- Start a query kept in a `const` with a `#graphql` comment line. Keep it for consistency.
- A named operation (`query GetProduct`) lets a test recognize the request by its name.
- The client is `ctx.shopify` in every context that belongs to a shop: a flow, a webhook, a proxy handler, an admin loader or action, an MCP tool or resource. In a server route, it is the `shopify` of what `ctx.for_shop("…").await?` returns. Creating a client reads nothing: the access token is read at the first request.
- Keep `app.shopify_client(shop, shopify_app)` for code without a context of that shop. It is not `async` and returns no `Result`.

## Prefer Typed Responses

When more than one or two fields are read, describe the response with serde
types and deserialize the `data` object `graphql()` returns into them:

```rust
use openshop::ShopifyClient;
use openshop::prelude::*;
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
    let data = shopify.graphql(GET_PRODUCT, json!({ "id": id })).await?;
    let data: GetProductData = openshop::serde_json::from_value(data)?;
    data.product.ok_or_else(|| anyhow!("Product {id} not found"))
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
- Read the error: `graphql()` fails with `GraphQL errors: …` when Shopify returns `errors`, with `Shopify answered <status> to …` for a non-2xx response (both a `shopify_api::Error`, which `downcast_ref` reads), and with `[openshop] No access token found for app "…" and shop "…". Is the app installed?` when the shop has no stored token.
- Fix the query or the type instead of adding a fallback value that masks the mismatch.

## Acceptable Narrowing

Handle absence explicitly after the response is typed:

```rust
let product = data.product.ok_or_else(|| anyhow!("Product not found"))?;
```

For a small read through `Value`, use `pointer()` with an explicit default or
error rather than chained indexing.

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

The compiler does not check untyped queries, and does not check what a flow
does with the result of a typed one: each operation needs a test that runs it
against a scripted response. `TestContext` routes Shopify calls to a
`MockTransport`:

```rust
use openshop::json;
use openshop::testing::{MockTransport, TestContext};

use my_app::APP;

#[openshop::test(APP)]
async fn reads_a_product(ctx: &TestContext) {
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

    let shopify = ctx.app.shopify_client(&ctx.shop, "default");
    let product = get_product(&shopify, "gid://shopify/Product/1").await.unwrap();

    assert_eq!(product.title, "Test product");
    assert_eq!(product.total_inventory, Some(12));
    assert_eq!(ctx.transport.requests().len(), 1);
}
```

- `#[openshop::test(APP)]` installs the shop of the test. A context started by hand needs `TestOptions::installed()`: without an access token the shop is not installed and `graphql()` fails before sending anything.
- The responder receives the outbound request. `request.body["query"]` and `request.body["variables"]` are what the client sent.
- The response body must have the Shopify envelope: `{ "data": … }` or `{ "errors": [{ "message": … }] }`.
- A transport without a responder answers `404 not mocked`. `respond_json(body)` answers every request with the same body.
- `ctx.transport.requests()` and `last_request()` return the recorded requests; `clear()` forgets them.
- For a flow, script the transport and call `ctx.run_flow(sync_orders::NAME, json!({ … }))`. The flow's `ctx.shopify` uses the same transport. See `templates/custom/tests/common/mod.rs` for fixtures selected by operation name.
- Write fixtures with the same field casing and nullability Shopify returns, including a `null` case for nullable fields.

## Framework Code

When changing GraphQL support in `crates/openshop`:
- `graphql!` is `shopify_api`'s; OpenShop only re-exports it. `crates/openshop/tests/graphql_typed.rs` checks it end to end against `crates/openshop/graphql/admin.schema.json`, the schema of the version in `[package.metadata.shopify_api]` of `crates/openshop/Cargo.toml`.
- The client lives in `crates/openshop/src/shopify/client.rs`: a `ShopifyClient` resolves, at its first request, a `shopify_api::Shopify` client with the stored token, or the shared client of the client credentials grant (`crates/openshop/src/shopify/client_credentials.rs`), which stores what it obtains in `installations`.
- Do not write a request to Shopify by hand: use `shopify_api` (the client, `oauth::ShopifyApp`, `webhooks::Webhook`). Its requests go through the `Transport` of the application (`crates/openshop/src/runtime/transport.rs`), which tests replace with `MockTransport`.
- Update `docs/src` and the templates (`templates/minimal`, `templates/custom`, `templates/public`) when user-facing usage changes.
