# OpenShop (Rust)

Build Shopify integrations in Rust with background flows, configurable
service providers, and an embedded admin UI.

This is the Rust implementation of [OpenShop](https://github.com/0xtlt/OpenShop).
It keeps the same model, HTTP API, database schema and admin screens as the
TypeScript framework, compiled into a single binary: the server, the worker,
the scheduler and the WebAssembly admin UI.

OpenShop is in beta. APIs, generated files, and workflows may change before `1.0`.

## Start here

You need Rust 1.88+, PostgreSQL 17, the Shopify CLI, and access to a
development store.

```bash
cargo install --git https://github.com/0xtlt/openshop-rust openshop-cli
openshop init my-app
cd my-app
docker compose up -d
cp .env.example .env
openshop dev
```

`openshop dev` pushes the database schema, then serves the API, the admin UI,
a worker and the cron scheduler on `http://localhost:3000`, restarting when
sources change. Run it through `shopify app dev` to open the app in a store.

## What an application looks like

```rust
use openshop::{FieldDef, Flow, JsonMap, OpenShopConfig, Provider, Result, json, serde_json, validators};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
struct SyncInput {
    #[schemars(range(min = 1))]
    limit: u32,
}

fn warehouse() -> Provider {
    Provider::new("warehouse")
        .field("apiUrl", FieldDef::text("API URL").validate(validators::url()))
        .field("apiKey", FieldDef::password("API Key").validate(validators::non_empty()))
        .typed_method("push", |config, orders: Vec<JsonMap>| async move {
            // call the service with `config["apiUrl"]` and `config["apiKey"]`
            Ok(orders.len())
        })
}

fn sync_orders() -> Flow {
    Flow::new("syncOrders", |ctx| async move {
        let input: SyncInput = ctx.input()?;

        // Each step is checkpointed: a retried run reuses its stored output.
        let orders: Vec<JsonMap> = ctx
            .step
            .run("fetch-orders", || async {
                let data = ctx
                    .shopify
                    .graphql(
                        "query($first: Int!) { orders(first: $first) { nodes { id name } } }",
                        Some(json!({ "first": input.limit })),
                    )
                    .await?;
                Ok(serde_json::from_value(data["orders"]["nodes"].clone())?)
            })
            .await?;

        ctx.step
            .run("send-to-warehouse", || async {
                ctx.connectors.call("warehouse", "push", &orders).await?;
                Ok(())
            })
            .await
    })
    .input::<SyncInput>()
}

fn config() -> Result<OpenShopConfig> {
    OpenShopConfig::builder()
        .provider(warehouse())
        .flow(sync_orders())
        .build()
}

#[tokio::main]
async fn main() -> Result<()> {
    openshop::cli::run(config).await
}
```

The binary understands `dev`, `start`, `worker [--concurrency=N]` and
`migrate [generate|check|status]`.

## How it fits together

- **Providers** define external API methods and credential fields. OpenShop builds
  a configuration form for each shop and supplies configured connectors at runtime.
- **Flows** define background work in named steps. OpenShop queues runs, stores
  checkpoints, applies retry policies, and exposes execution logs.
- **App configuration** registers flows, providers, schedules, webhooks, app
  proxies, public routes, Shopify Functions and MCP capabilities. Shopify OAuth
  and the embedded admin are handled by the framework.

In production, a web process (`start`) accepts requests and queues work; separate
workers (`worker`) execute it. PostgreSQL stores installations, credentials, runs,
and checkpoints. See [Architecture](docs/src/concepts/architecture.md) and
[Deploy to production](docs/src/guides/deploy-production.md).

## Coming from the TypeScript version

The runtime behavior is the same; the way an application is assembled follows
Rust conventions.

| TypeScript | Rust |
| --- | --- |
| `defineOpenShop({ providers }).defineConfig({ … })` | `OpenShopConfig::builder()…build()` |
| `defineFlow({ name, input: type({…}), run })` | `Flow::new(name, run).input::<T>()` with `serde` + `schemars` |
| `await step('name', fn)` / `step.sleep` | `ctx.step.run("name", \|\| async { … }).await?` / `ctx.step.sleep` |
| `connectors.warehouse.push(data)` | `ctx.connectors.call("warehouse", "push", data)` |
| `proxy/*.ts`, `routes/*.ts`, `admin/pages/**` discovered from files | `.proxy(path, …)`, `.route(path, …)`, `.admin_page(…)` on the builder |
| `validate: type('string.url')` | `.validate(validators::url())` or any closure |
| `openshop.config.ts` loaded by the `openshop` CLI | your own binary calling `openshop::cli::run(config)` |
| `drizzle-kit` migrations in `./drizzle` | SQL migrations in `./migrations` (`migrate generate` / `migrate`) |
| Preact admin UI built with Vite | Leptos admin UI compiled to WebAssembly and embedded in the binary |
| `createTestContext()` from `openshop/test` | `openshop::testing::TestContext` (feature `testing`) |
| GraphQL codegen, ESLint preset | not needed: responses are `serde` types or `serde_json::Value` |

The database tables, the encryption format of stored credentials and tokens,
the MCP token format and every HTTP route are identical, so both runtimes can
serve the same database. A few behaviors are intentionally stricter or more
robust; they are listed in [Differences](docs/src/reference/differences.md).

## Repository layout

| Path | Contents |
| --- | --- |
| `crates/openshop` | The framework: engine, server, MCP, testing harness, embedded UI assets |
| `crates/openshop-shared` | Types and rules shared by the server and the WebAssembly UI |
| `crates/openshop-ui` | The admin UI (Leptos, built with trunk into `crates/openshop/ui-dist`) |
| `crates/openshop-cli` | The `openshop` command: `init`, `init-ui` and `dev` |
| `apps/demo` | A demo app exercising every building block; `apps/demo/ui` is its admin UI bundle with a custom page |
| `docs` | Documentation (mdBook) |

## Contribute

See [CONTRIBUTING.md](CONTRIBUTING.md) for development and checks.

## License

OpenShop is licensed under the MIT License.

Read the LICENSE file for the full terms.
