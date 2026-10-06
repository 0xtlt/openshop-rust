# Shop and app isolation

A Shopify **app** has a client ID and secret. A **shop** is a store such as
`example.myshopify.com`. An **installation** connects one app to one shop.
One OpenShop deployment can serve several apps, and each app can be installed
on several shops.

## The tenant is an app/shop pair

Framework state uses the pair `(app_handle, shop)`. Single-app mode uses the
handle `default`; explicit multi-app mode uses the keys of `ShopifyConfig::apps`.
The same shop installing two different apps therefore has two installations
and two provider configurations.

This scope applies to framework installations, provider configurations, flow
runs, cron overrides, Function configuration, and MCP tokens. Runtime contexts
expose the handle as `shopify_app` (`app_handle` in MCP tools and resources).

Shopify credentials establish the identity at the HTTP boundary. For example,
a session token's audience selects the app, while its verified destination
identifies the shop. Client-supplied fields do not establish that identity.
See [Authentication](../reference/authentication.md) for each surface's contract.

## App-owned tables require app-owned filters

`define_model()` adds a `shop` column by default. It does **not** automatically add
an app handle or restrict queries: `ctx.db` is a plain `sqlx` connection pool.
Application code must filter by the trusted identity from the runtime context.

For data that differs between apps installed on the same shop, add an app-handle
column and include both values in reads, writes, and uniqueness constraints.
A `shop` filter alone cannot distinguish those installations.

```rust
use openshop::{Model, define_model};

pub fn reviews() -> Model {
    define_model("reviews")
        .column("app_handle", "text NOT NULL")
        .column("product_id", "text NOT NULL")
        .unique_index("reviews_app_shop_product_unique", &["app_handle", "shop", "product_id"])
}
```

A query on that table then binds both values from the context, here inside a
flow:

```rust
let (count,): (i64,) = openshop::sqlx::query_as(
    "SELECT count(*) FROM reviews WHERE app_handle = $1 AND shop = $2",
)
.bind(&ctx.shopify_app)
.bind(&ctx.shop)
.fetch_one(&ctx.db)
.await?;
```

The same rule applies to custom admin loaders and actions: their `ctx.db` is
the same unrestricted pool.

## Shared hosting does not mean independent configuration

All Shopify apps in one OpenShop instance share the flow/provider definitions,
database schema, and OAuth scopes. Credentials and shop-scoped state are separate.
If two apps need incompatible scopes or release schedules, separate deployments
may be easier to operate.

Keep handles stable: changing a handle changes the identity used to find stored
state. Changing an encryption key also needs a data migration; it is not an
ordinary configuration rename.

For setup steps, use [Configure Shopify apps](../guides/configure-shopify-apps.md).
For application models, see [Database and migrations](../reference/database.md).
