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
expose the handle as `shopify_app`. Every context that belongs to a shop
carries `ctx.shopify` and `ctx.connectors` for that pair: the access token and
the provider configuration they read are those of the app and the shop the
handler runs for.

Shopify credentials establish the identity at the HTTP boundary. For example,
a session token's audience selects the app, while its verified destination
identifies the shop. Client-supplied fields do not establish that identity.
See [Authentication](../reference/authentication.md) for each surface's contract.

## App-owned tables require app-owned filters

A model with a `shop` field gets rows that are limited to one shop:
`Review::of(&ctx)` only finds, updates and deletes the rows of the shop of the
context, and inserts into it. `Review::for_shop(&ctx.db, &ctx.shop)` is the
same with the database and the shop given: pass the trusted identity from the
runtime context, never a value sent by the client.

That limit is the only one. `ctx.db` itself is a plain `sqlx` connection pool:
a query written with `sqlx`, and `Review::all_shops(..)`, see every shop, and
application code must filter them.

The rows of the framework are read the same way. `FlowRun::of(&ctx)` gives the
runs of the shop and the app of the context; `FlowRun::all_shops(&db)` and
`Installation::installed(&db)` see every shop, for the operator of the
application. The [operator dashboard](../reference/operator-dashboard.md) is
that second view: it belongs to no shop, and no merchant reaches it.

For data that differs between apps installed on the same shop, add an
app-handle column and include it in writes, reads, and uniqueness constraints.
A `shop` filter alone cannot distinguish those installations.

```rust
use openshop::prelude::*;

#[derive(Debug, Clone, Model)]
#[model("reviews", unique([app_handle, shop, product_id]))]
pub struct Review {
    pub id: Uuid,
    pub shop: String,
    pub app_handle: String,
    pub product_id: String,
}
```

A query on that table then filters on both values from the context, here
inside a flow:

```rust
let count = Review::of(&ctx)
    .filter(Review::APP_HANDLE.eq(&ctx.shopify_app))
    .count()
    .await?;
```

The same rule applies in every handler: `ctx.db` is the same unrestricted pool
in a webhook, a proxy handler, a server route, a loader or an action of an
admin page, and an MCP tool.

The core MCP tools follow it: an MCP token belongs to one app and one shop, and
every core tool acts on that pair. `openshop.admin.graphql` takes its shop
from the token, not from its arguments: a `shop` naming another shop is
refused. A
staff member can grant any permission to the tokens of their own shop, so a
permission cannot separate shops; reaching several shops through MCP is not
something a token can be granted.

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
