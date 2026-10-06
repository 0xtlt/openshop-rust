# Typed GraphQL

`graphql!` checks a Shopify Admin GraphQL operation against Shopify's schema
while the crate compiles, and gives its variables and its result Rust types. It
replaces the GraphQL code generation of the TypeScript version: there is no
generation step and no generated file.

## Inline operations

```rust,ignore
use openshop::{FlowContext, Result, graphql};

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

The operation is a string literal, with or without a leading `#graphql` line.
It contains one query or mutation and the fragments it uses. Everything the
schema can tell is checked when the crate compiles:

- fields, arguments, fragments and enum values exist, with a suggestion when a
  name is close to an existing one;
- required arguments and required input fields are given;
- composite fields have a selection, scalar fields have none;
- variables are declared, used, and of a type their arguments accept;
- every required variable has a value after the operation, and no value names
  an unknown variable.

A mistake is a compile error on the operation:

```text
error: Type `Order` has no field `nam` (did you mean `name`?) (line 1, column 58)
       (checked against the Shopify Admin API 2026-04)
 --> src/flows/sync_orders.rs:12:13
```

Reading a field that the operation does not select is a compile error too:
`no field `name` on type `DataOrdersNodes``.

## Result types

Each selection becomes a struct with one field per selected key. Fields are
named in snake case: `lineItems` is `line_items`, an alias gives its own name,
`__typename` is `typename`, and a Rust keyword is written raw (`r#type`).

| GraphQL | Rust |
| --- | --- |
| `String`, `ID` | `String` |
| `Int` | `i64` |
| `Float` | `f64` |
| `Boolean` | `bool` |
| `JSON` | `serde_json::Value` |
| Other scalars: `DateTime`, `Date`, `Decimal`, `Money`, `URL`, `HTML`, `UnsignedInt64`, … | `String`, as Shopify sends them |
| Enum | `String` holding the value, such as `"PAID"` |
| Object, interface, union | A struct |
| `T` without `!` | `Option<T>` |
| `[T]` | `Vec<T>` |

Enum values are strings so that a value Shopify adds later does not break
deserialization. Enum values written in the operation itself, such as
`sortKey: CREATED_AT`, are checked.

A field selected in a fragment on another type, as in `... on Product { title }`
below, is an `Option` that is `None` for the other types. So is a field under
`@include` or `@skip`. A fragment on the type itself or on one of its
interfaces adds plain fields.

```rust,ignore
let data = shopify
    .execute(graphql!(
        "query($ids: [ID!]!) {
           nodes(ids: $ids) {
             __typename
             id
             ... on Product { title }
             ... on Order { name }
           }
         }",
        ids = &ids
    ))
    .await?;

// `nodes` is `[Node]!`: a list of nullable nodes.
for node in data.nodes.iter().flatten() {
    println!("{} {} {:?} {:?}", node.typename, node.id, node.title, node.name);
}
```

The types implement `Debug`, `Clone`, `PartialEq`, `Serialize` and
`Deserialize`, so a result can be returned from `ctx.step.run`. It is stored
under the names of the GraphQL fields.

The types of an inline operation have no name you can write. Let inference
carry them, convert the result to your own type, or use a
[module](#named-operations).

## Variables

Values follow the operation as `name = value`. A variable that is non-null and
has no default needs a value. The others can be left out, and are then not
sent.

A value is converted to the Rust type of the variable, so the usual spellings
work:

| Variable | Accepted values |
| --- | --- |
| `String`, `ID`, enums, string-encoded scalars | `String`, `&str`, `&String`, `Cow<str>` |
| `Int` | Any integer type |
| `Float` | `f32`, `f64`, `i32`, `u32` |
| `Boolean` | `bool` |
| `JSON` | `Value`, `&Value` |
| Nullable | The bare value, or an `Option` of it |
| List | A `Vec`, a slice, an array or a `&Vec` of accepted values |
| Input object | Any serializable value, such as `json!({ … })`. Its fields are not checked. |

Another type is a compile error on the value: `` `&str` is not a value for a
GraphQL variable of Rust type `i64` ``. The conversions are the
implementations of `openshop::IntoVariable`.

## Named operations

Give an operation a module to name its types: to share it between several
places, to write a function that returns its data, or to build input objects
whose fields are checked.

```rust,ignore
use openshop::{Result, ShopifyClient, graphql};

graphql! {
    /// Writes metafields and reports what Shopify refused.
    pub mod metafields_set = "mutation($metafields: [MetafieldsSetInput!]!) {
        metafieldsSet(metafields: $metafields) {
            metafields { id key }
            userErrors { field message code }
        }
    }";
}

async fn set_color(shopify: &ShopifyClient, product_id: &str) -> Result<Vec<String>> {
    let variables = metafields_set::Variables {
        metafields: vec![metafields_set::MetafieldsSetInput {
            owner_id: product_id.to_string(),
            namespace: Some("custom".to_string()),
            key: "color".to_string(),
            value: "blue".to_string(),
            ..Default::default()
        }],
    };
    let data: metafields_set::Data = shopify.execute(metafields_set::operation(variables)).await?;

    let payload = data.metafields_set.ok_or_else(|| openshop::anyhow!("No payload"))?;
    Ok(payload.user_errors.into_iter().map(|error| error.message).collect())
}
```

The module contains:

| Item | Contents |
| --- | --- |
| `QUERY` | The text of the operation. |
| `Data` and nested structs | The result. A nested struct is named after its path: `DataMetafieldsSetUserErrors`. |
| `Variables` | One field per variable. Absent when the operation declares none. |
| Input objects | One struct per input object the variables use, named as in the schema. |
| `operation(variables)` | The operation for `execute`. Takes no argument when there are no variables. |

In `Variables` and in input objects, a field that may be left out is an
`Option` and is not sent when `None`; the structs implement `Default`. An
explicit `null` cannot be written this way: use an inline operation with a
`json!` value, or the untyped client, for the mutations that need one.

## Schema and API version

The schema bundled with the framework is the one of `SHOPIFY_API_VERSION`,
currently `2026-04`: the version `ctx.shopify` and `app.shopify_client(…)`
call. Nothing has to be downloaded to use `graphql!`.

A client created for another version with
`create_shopify_client(&app, shop, shopify_app, Some("2026-07"))` needs the
schema of that version:

```bash
openshop codegen --api-version=2026-07
```

This downloads the schema from Shopify's public endpoint,
`https://shopify.dev/admin-graphql-direct-proxy/<version>`, and writes
`graphql/admin.schema.json` next to `Cargo.toml`. `graphql!` then checks every
operation of the crate against it. Commit the file: builds need it.

| Source, in order | |
| --- | --- |
| `OPENSHOP_GRAPHQL_SCHEMA` | Path of a schema file, relative to `Cargo.toml` or absolute. Read when the crate compiles. |
| `graphql/admin.schema.json` | Written by `openshop codegen`. |
| Bundled schema | `SHOPIFY_API_VERSION`. |

A crate uses one schema. The error of a refused operation names the API
version it was checked against.

## Runtime behavior and errors

`execute(operation)` sends the operation like
[`graphql()`](sdk.md#shopifyclient) and fails in the same cases: no access
token, a non-2xx response, a non-empty `errors` array. It also fails when
`data` does not fit the operation, with the path of the first mismatch:

```text
[openshop] Shopify's answer does not match the operation at `orders.nodes[0]`: missing field `name`
```

That happens when the shop runs another API version than the schema, or when a
test scripts an answer that leaves out a non-null field.

Mutation-specific `userErrors` live inside `data` and are not raised
automatically. Inspect them in application code.

## Untyped calls

`graphql(query, variables)` and `graphql_as::<T>(query, variables)` remain for
operations built at run time. Nothing checks them when the crate compiles.

## Limits

- Subscriptions are refused.
- One `graphql!` takes one operation.
- The compile error points at the whole operation; the line and column in its
  message locate the mistake inside it.
- Input objects given to an inline operation are not checked. Use a module.
