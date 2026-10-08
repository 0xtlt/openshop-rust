# Architecture

An OpenShop deployment has two long-running processes backed by one PostgreSQL
database:

```text
┌──────────────────────────────────────────────────┐
│  Shopify admin / webhooks / proxies              │
└────────────────────────┬─────────────────────────┘
                         │
                         v
┌──────────────────────────────────────────────────┐
│  web process (`start`)                           │
│  - OAuth and session authentication              │
│  - embedded admin UI and Admin API               │
│  - webhook, proxy, server, Function, MCP routes  │
│  - cron scheduler                                │
└────────────────────────┬─────────────────────────┘
                         │ inserts pending flow runs
                         v
┌──────────────────────────────────────────────────┐
│  PostgreSQL                                      │
│  - installations and encrypted tokens            │
│  - provider configuration                        │
│  - flow runs, checkpoints, and logs              │
│  - cron overrides and MCP audit data             │
└──────────────────────────────────────────────────┘
                         ^
                         │ leases and executes runs
                         │
┌────────────────────────┴─────────────────────────┐
│  worker process (`worker`)                       │
│  - provider connectors                           │
│  - Shopify Admin GraphQL                         │
│  - checkpointed steps and retries                │
└──────────────────────────────────────────────────┘
```

Both processes are the same binary: your application, started with the `start`
or the `worker` command. They share one `OpenShopConfig` and one `DATABASE_URL`.
By default the admin UI is embedded in that binary. The web process does not
execute flows. A deployment therefore needs at least one instance of each process.

In development, the `dev` command runs the web process, a worker, and the
scheduler together in a single process.

## Request lifecycle

1. Shopify launches the embedded app or calls an authenticated route.
2. OpenShop resolves the Shopify app and shop identity.
3. A cron, API request, webhook, or one of your own handlers dispatches a flow.
4. The run is stored as `pending`; a worker leases it when its `available_at`
   time is due.
5. Each `ctx.step.run()` writes a checkpoint. A retry reuses completed step output.
6. The run ends as `completed`, `failed`, or `canceled`; logs remain queryable.

## Identity and tenancy

Shop-scoped framework data is identified by the pair `(app_handle, shop)`. In
single-app mode, the app handle is `default`. Multi-app deployments use the keys
declared in `ShopifyConfig::apps`. This boundary applies to installations, flow
runs, provider configuration, cron overrides, Functions, and MCP tokens.

An application table described by a `#[derive(Model)]` struct with a `shop`
field is read and written through the rows of a shop, `Review::of(&ctx)` in a
handler or `Review::for_shop(db, shop)`, which limit every operation to that
shop. Queries written with `sqlx` are not limited: application code is
responsible for their shop filter.

## Scaling boundaries

- Run one scheduler unless duplicate cron attempts are acceptable. The scheduler
  has no leader election, and every web process starts one. Flow concurrency can
  reject an overlapping run, but it is not a scheduler de-duplication guarantee.
- Run multiple workers against the same database to increase throughput.
- Set worker concurrency below the combined PostgreSQL and provider capacity.
- Keep migrations separate from process startup so every replica sees the same
  schema before it begins serving traffic. `start` and `worker` never change
  the schema.

See [Deploy to production](../guides/deploy-production.md) and
[Operate an app](../guides/operate-app.md) for process and recovery procedures.
