# OpenShop (Rust)

Build Shopify integrations in Rust with background flows, configurable
service providers, and an embedded admin UI.

This is the Rust implementation of [OpenShop](https://github.com/0xtlt/OpenShop).
It keeps the same model, HTTP API, database schema and admin screens as the
TypeScript framework, compiled into a single binary: the server, the worker,
the scheduler and the WebAssembly admin UI.

This implementation is a v0 in development and has no users yet. APIs,
generated files, and workflows change without a transition period before `1.0`.

## Start here

You need Rust 1.94+, PostgreSQL 17, the Shopify CLI, and access to a
development store.

```bash
cargo install --locked --git https://github.com/0xtlt/openshop-rust openshop-cli
openshop init my-app       # asks for a template: minimal, custom or public
cd my-app
docker compose up -d --wait
cp .env.example .env
openshop dev
```

The project depends on `openshop` at the commit the CLI was built from, which
`cargo install --git` takes from the repository. To follow another commit, tag
or branch, or a local checkout, pass `--openshop-rev=<commit>`,
`--openshop-tag=<tag>`, `--openshop-branch=<name>` or `--openshop-path=<dir>`
to `openshop init`.

`openshop dev` pushes the database schema, then serves the API, the admin UI,
a worker and the cron scheduler on `http://localhost:3000`. It restarts when
sources change, and the admin UI reloads by itself. Run it through
`shopify app dev` to open the app in a store.

To work without a development store, run the app against a simulator of
Shopify such as [local-storefront](https://github.com/0xtlt/local-storefront):
its `ShopifyEndpoints` send the Admin API and OAuth there, and make the admin
UI load the simulator's App Bridge and Polaris. A proxy or an API of your own
works the same way. See
[Custom Shopify endpoints](docs/src/guides/custom-shopify-endpoints.md).

## What an application looks like

```rust
use openshop::prelude::*;
use serde::{Deserialize, Serialize};

/// The input of a run: it is validated, and the admin builds its form from it.
#[input]
struct SyncInput {
    #[schemars(range(min = 1))]
    limit: u32,
}

#[derive(Serialize, Deserialize)]
struct Order {
    id: String,
    name: String,
}

/// A provider is a struct: its fields are the form a merchant fills per shop…
#[derive(ProviderConfig)]
struct Warehouse {
    #[field(label = "API URL", url)]
    api_url: String,
    #[field(label = "API Key", password, non_empty)]
    api_key: String,
}

/// …and its public methods are what flows can call, with that configuration as `self`.
/// The provider is named `warehouse`, after the struct.
#[provider]
impl Warehouse {
    pub async fn push(&self, orders: Vec<Order>) -> Result<usize> {
        // call the service at `self.api_url` with `self.api_key`
        Ok(orders.len())
    }
}

/// A flow is an `async` function; its second argument is the validated input of a run.
#[flow("syncOrders")]
async fn sync_orders(ctx: FlowContext, input: SyncInput) -> Result<()> {
    // Each step is checkpointed: a retried run reuses its stored output.
    let orders: Vec<Order> = ctx
        .step
        .run("fetch-orders", async || {
            // Checked against Shopify's schema when the crate compiles; `data` is typed.
            let data = ctx
                .shopify
                .execute(graphql!(
                    "query($first: Int!) { orders(first: $first) { nodes { id name } } }",
                    first = input.limit
                ))
                .await?;
            let orders = data.orders.nodes.into_iter();
            Ok(orders.map(|order| Order { id: order.id, name: order.name }).collect())
        })
        .await?;

    ctx.step
        .run("send-to-warehouse", async || {
            // Checked by the compiler: a `Vec<Order>` in, a `usize` out.
            Warehouse::on(&ctx.connectors).push(&orders).await?;
            Ok(())
        })
        .await
}

/// The application is a value that lists its building blocks.
static APP: App = app! {
    providers: [Warehouse],
    flows: [sync_orders],
};

fn main() -> Result<()> {
    APP.main()
}
```

The binary understands `dev`, `start`, `worker [--concurrency=N]` and
`migrate [generate|check|status]`.

## How it fits together

- **Providers** define external API methods and credential fields. OpenShop builds
  a configuration form for each shop and supplies configured connectors at runtime.
- **Flows** define background work in named steps. OpenShop queues runs, stores
  checkpoints, applies retry policies, and exposes execution logs.
- **App configuration** lists the flows, providers, webhooks, app proxies,
  public routes, Shopify Functions and MCP capabilities of the application,
  each declared where it is written, and sets its schedules. Shopify OAuth and
  the embedded admin are handled by the framework.
- **Shopify** is reached through [`shopify_api`](https://docs.rs/shopify_api):
  `ctx.shopify` runs its operations, checked against Shopify's schema at
  compile time, with the token of the installation, and its verification of
  session tokens, launches, app proxies and webhooks guards the routes.
- **The admin** shows a shop its flows, runs, logs and providers, or only the
  pages you wrote for it. An optional dashboard under `/ops`, behind a
  password, shows the operator every shop.
- **Sentry** receives the runs that fail, the handlers that fail and the
  panics, each with its flow and its shop, once the application gives a DSN.

In production, a web process (`start`) accepts requests and queues work; separate
workers (`worker`) execute it. PostgreSQL stores installations, credentials, runs,
and checkpoints. See [Architecture](docs/src/concepts/architecture.md) and
[Deploy to production](docs/src/guides/deploy-production.md).

## Coming from the TypeScript version

The runtime behavior is the same; the way an application is assembled follows
Rust conventions.

| TypeScript | Rust |
| --- | --- |
| `defineOpenShop({ providers }).defineConfig({ … })` | `app! { providers: […], flows: […], … }`, a value of type `App` |
| `defineFlow({ name, input: type({…}), run })` | `#[flow("name")]` on an `async fn(ctx, input)`, the input being a type marked `#[input]` |
| `await step('name', fn)` / `step.sleep` | `ctx.step.run("name", async \|\| { … }).await?` / `ctx.step.sleep` |
| `defineProvider({ fields, methods })` | a struct with `#[derive(ProviderConfig)]` and `#[provider]` on its `impl` block |
| `connectors.warehouse.push(data)` | `Warehouse::on(&ctx.connectors).push(&data)`, or by name with `ctx.connectors.call("warehouse", "push", data)` |
| `proxy/*.ts`, `routes/*.ts`, `admin/pages/**` discovered from files | `#[proxy(GET, "/path")]`, `#[route(GET, "/path")]` and `#[admin_page("/path")]` on the code, listed in `app!` |
| A Liquid proxy that returns its template as a string | A `.liquid` file next to the handler, with the values of the server assigned to it: `liquid!("reviews.liquid").assign("name", value)` |
| `validate: type('string.url')` | `url`, `non_empty`, `range(…)` or `length(…)` in the `#[field(…)]` of a field, and `validate = <validator>` for anything else |
| `openshop.config.ts` loaded by the `openshop` CLI | your own binary running the application it declares: `APP.main()` |
| `defineModel('reviews', { … })` and Drizzle queries | `#[derive(Model)]` on a struct, with typed rows: `Review::of(&ctx).filter(Review::RATING.gte(4))` |
| `drizzle-kit` migrations in `./drizzle` | SQL migrations in `./migrations`, generated from what changed in the models (`migrate generate` / `migrate`) |
| Preact admin UI built with Vite | Leptos admin UI compiled to WebAssembly and embedded in the binary |
| `createTestContext()` from `openshop/test` | `#[openshop::test(APP)]` on an `async fn(ctx: &TestContext)` (feature `testing`) |
| `shopify.graphql(\`#graphql …\`)` typed by codegen | `ctx.shopify.execute(graphql!("…", var = value))`, checked and typed at compile time |
| ESLint preset | not needed |

The database tables, the encryption format of stored credentials and tokens,
the MCP token format and every HTTP route are identical, so both runtimes can
serve the same database. A few behaviors are intentionally stricter or more
robust; they are listed in [Differences](docs/src/reference/differences.md).

## Repository layout

| Path | Contents |
| --- | --- |
| `crates/openshop` | The framework: engine, server, MCP, testing harness, embedded UI assets |
| `crates/openshop-shared` | Types and rules shared by the server and the WebAssembly UI |
| `crates/openshop-macros` | The macros: `app!`, and the attributes and derives that declare building blocks |
| `crates/openshop-ui` | The admin UI (Leptos, built with trunk into `crates/openshop/ui-dist`) |
| `crates/openshop-cli` | The `openshop` command: `init`, `init-ui`, `dev`, `build` and `build-ui` |
| `templates/minimal` | The template of `openshop init --template=minimal`: one provider and one flow |
| `templates/custom` | The template of a custom app: every building block, with the built-in admin open to the merchant and a custom admin page |
| `templates/public` | The template of a public app: an admin made of its own pages for the merchants, the operator dashboard for whoever runs it |
| `templates/shared` | The files every project gets: Dockerfile, Procfile, Docker Compose, `.env.example` |
| `apps/preview` | What serves the custom or public template in a browser without a Shopify store |
| `docs` | Documentation (mdBook) |

## Contribute

See [CONTRIBUTING.md](CONTRIBUTING.md) for development and checks.

## License

OpenShop is licensed under the MIT License.

Read the LICENSE file for the full terms.
