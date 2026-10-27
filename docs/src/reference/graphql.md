# Typed GraphQL

`graphql!` checks a Shopify Admin GraphQL operation against Shopify's schema
while the crate compiles, and gives its variables and its result Rust types. It
is the macro of [`shopify_api`](https://docs.rs/shopify_api), the Shopify
client OpenShop runs on, re-exported as `openshop::graphql!` and in the
prelude. It replaces the GraphQL code generation of the TypeScript version:
there is no generation step and no generated file.

## Setup

The crate that writes an operation depends on `openshop` alone: the code
`graphql!` generates names `shopify_api` through `openshop::shopify_api`. It
names its Admin API version in `Cargo.toml`, as the templates of
`openshop init` do:

```toml
[package.metadata.shopify_api]
api-version = "2026-10"

[dependencies]
openshop = "…"
```

The dependency is named `openshop`: under another name, the generated code
does not find it.

`api-version` is the Admin API version the operations are checked against and
sent to. Its schema is `graphql/admin.schema.json`, next to `Cargo.toml`,
which `openshop init` writes: commit it. To move to another version, change
`api-version` and run `shopify-api schema download` in that directory
(`cargo install shopify_api --features cli`). The build does not download it:
until the file matches `api-version`, it stops and says so.

## Inline operations

```rust,ignore
use openshop::prelude::*;

async fn recent_order_names(ctx: &FlowContext, limit: u32) -> Result<Vec<String>> {
    let data = ctx
        .shopify
        .execute(graphql!(
            "query($first: Int!) { orders(first: $first) { nodes { id name } } }",
            first = limit
        ))
        .await?;

    Ok(data.orders.nodes.into_iter().map(|order| order.name).collect())
}
```

`execute` resolves to the contents of Shopify's `data` field, not to the
complete GraphQL response: read `data.orders`, not `data.data`.

A field, argument, enum value or variable that does not exist is a compile
error, with a suggestion when a name is close to an existing one; a deprecated
one is a compiler warning with Shopify's reason. Reading a field that the
operation does not select is a compile error too.

Each selection becomes a struct with one field per selected key, in snake
case. A nullable field is an `Option`, a list a `Vec`, an enum value a
`String`. The types implement `Serialize` and `Deserialize`, so a result can
be returned from `ctx.step.run`: it is stored under the names of the GraphQL
fields.

Values follow the operation as `name = value`, converted to the type of the
variable: any integer for an `Int`, `&str` or `String` for an `ID`, a bare
value or an `Option` for a nullable variable, a slice or an array for a list.
A value of the wrong type does not compile.

## Input objects

An input object is written as an object literal, checked against the schema
field by field:

```rust,ignore
ctx.shopify
    .execute(graphql!(
        "mutation($product: ProductUpdateInput!) { productUpdate(product: $product) { userErrors { field message } } }",
        product = {
            id: product_id,
            title: format!("{name} (promo)"),
            status: ACTIVE,          // an enum value, checked
            vendor?: new_vendor,     // an Option: None leaves the field out
        },
    ))
    .await?;
```

`openshop::shopify::input!` builds one ahead of the call, in a loop for
instance:

```rust,ignore
use openshop::shopify::input;

let metafields: Vec<_> = rows
    .iter()
    .map(|row| input!(MetafieldsSetInput {
        ownerId: &row.product_id, namespace: "custom", key: "color", value: &row.color,
        type: "single_line_text_field",
    }))
    .collect();
```

## Named operations

Give an operation a module to name its types: to share it, to write a function
that returns its data, or to build its variables elsewhere.

```rust,ignore
use openshop::ShopifyClient;
use openshop::prelude::*;

graphql! {
    pub mod shop_name = "query ShopName { shop { name currencyCode } }";
    pub mod order = file("graphql/order.graphql");
}

async fn shop_name(shopify: &ShopifyClient) -> Result<String> {
    let data: shop_name::Data = shopify.execute(shop_name::operation()).await?;
    Ok(data.shop.name)
}
```

A module whose operation has variables has a `Variables` struct, which
`execute` takes as it is.

The reference of `graphql!`, with the mapping of every GraphQL type, the forms
of a literal and the bulk operations, is
[on docs.rs](https://docs.rs/shopify_api/latest/shopify_api/macro.graphql.html).

## Runtime behavior and errors

`ctx.shopify` sends the operation with the token of the installation, to the
version of the schema it was checked against. A throttled request waits for
what its cost needs, then goes again; a query is retried after a server error,
a mutation never runs twice. A token the Admin API refuses with a `401` is
renewed once where the client credentials grant may renew it (see
[authentication](authentication.md)).

`execute` fails when the shop has no access token, when Shopify answers a
status other than 2xx, with a non-empty `errors` list, or with data that does
not fit the operation. The error is a `shopify_api::Error`, which
`error.downcast_ref::<openshop::shopify_api::Error>()` reads: `Http` with its status,
`Graphql` with Shopify's errors, `Decode` with the path of the first mismatch.

Mutation-specific `userErrors` live inside `data` and are not raised
automatically. Inspect them in application code.

`ctx.shopify.api().await?` is the `shopify_api::Shopify` client of the shop,
for what `ShopifyClient` does not wrap: `paginate`, the bulk operations, the
webhook subscriptions, `execute_response` and the cost of an operation.

## Another API version

`ShopifyClient::with_api_version(version)` sends every request of the client
to `version`, a `shopify_api::ApiVersion`. The operations stay checked against
the schema of the crate.

## Untyped calls

`graphql(query, variables)` remains for operations built at run time. Nothing checks them when the crate compiles.
`variables` is anything that serializes to an object, or `()` for none.

## Limits

- Subscriptions are refused.
- One operation per `graphql!` or per module.
- A literal and an `input!` are checked against the schema of the crate where
  they are written.
