# Project structure

`openshop init` creates a Cargo package with the app definition, a sample
provider and flow, Shopify configuration, deployment files, and an initial
migration. Add optional modules as your app needs them.

```text
my-app/
├── src/
│   ├── main.rs
│   ├── flows/
│   │   ├── mod.rs
│   │   └── sync_orders.rs
│   └── providers/
│       ├── mod.rs
│       └── warehouse.rs
├── migrations/
│   └── <timestamp>_openshop_init.sql
├── Cargo.toml
├── shopify.app.toml
├── shopify.web.toml
├── .env.example
├── .gitignore
├── .dockerignore
├── Dockerfile
├── docker-compose.yml
├── Procfile
└── README.md
```

OpenShop does not discover anything from the file system at runtime. The
module layout is a convention; a definition takes effect when it is registered
on `OpenShopConfig::builder()`.

| Optional module | Add it when you need | Register with |
| --- | --- | --- |
| `src/proxy/` | Storefront or Customer Account endpoints | `.proxy(path, …)` |
| `src/routes/` | HTTP endpoints with explicit authentication | `.route(path, …)` |
| `src/webhooks/` | Shopify event handlers | `.webhook(topic, …)` |
| `src/functions/` | Shopify Function definitions | `.function(key, …)` |
| `src/models/` | Application-owned database tables | `.model(…)` |
| `src/admin/` | Server side of experimental custom admin screens | `.admin_page(…)` and `.custom_pages(…)` |
| `tests/` | Integration tests using `openshop::testing` | — |
| `ui/` | WebAssembly admin UI bundle with the views of custom admin screens, created by `openshop init-ui` | `.admin_ui(…)` or `OPENSHOP_UI_DIR` |

## `Cargo.toml`

Declares the package and its dependencies:

- `openshop`, from the Git repository, pinned to the tag of the release that
  created the project (`tag = "v<version>"`), or from a local path when the
  project was created with `--openshop-path`;
- `serde` and `schemars`, used to derive typed flow inputs;
- `tokio`, for the `#[tokio::main]` entry point;
- `openshop` again under `[dev-dependencies]` with the `testing` feature.

The `[profile.release]` section enables thin LTO and strips debug information.
The `scrypt` and `salsa20` packages are optimized in the development profile
too, because MCP token hashing is very slow without optimizations.

## `src/main.rs`

Defines `config()`, which builds the configuration with
`OpenShopConfig::builder()`, and passes that function to `openshop::cli::run`,
which implements the `dev`, `start`, `worker`, and `migrate` commands. The generated file registers the sample provider and flow,
a "Quick sync" cron that runs the flow every five minutes, and an `on_error`
hook that prints failed flows. Register further providers, flows, crons,
webhooks, routes, Shopify Function definitions, worker settings, retry policy,
MCP configuration, and admin page visibility here. The file also contains the
generated tests. See [Configuration](configuration.md).

## `src/providers/`

Contains provider definitions. Providers define admin config fields and methods
for external systems. See [Providers](providers.md).

## `src/flows/`

Contains background job definitions. Flows run in workers and can use
checkpointed steps, the Shopify client, provider connectors, logs, and the
database. See [Flows](flows.md).

## `src/proxy/`

Contains app proxy and Customer Account extension handlers. Each function
returns a `ProxyDefinition` that `main.rs` registers under a path. See
[Proxy routes](proxy-routes.md).

## `src/routes/`

Contains optional public server routes mounted under `/routes/*`. Every route
is created with `ServerRoute::public()` or `ServerRoute::authenticated(…)`. See
[Public server routes](server-routes.md).

## `src/admin/`

Contains the server side of experimental custom admin pages: `AdminPage`
definitions with their loaders, actions, and access rules. The page views are
Rust components compiled into a separate WebAssembly UI bundle. See
[Custom admin pages](custom-admin-pages.md).

## `migrations/`

Contains committed SQL migrations owned by the app. The first file holds the
framework tables; later files hold framework schema updates and your own
tables. See [Database and migrations](database.md).

## Shopify files

- `shopify.app.toml` is the Shopify app configuration template. OpenShop reads
  scopes from it, and `client_id` and `application_url` when the file is
  referenced by a `ShopifyAppConfig::Toml` entry.
- `shopify.web.toml` tells Shopify CLI how to start the app in development:
  `openshop dev`.

The generated `.gitignore` excludes `target/`, `.env`, and `.shopify/`.

## Deployment files

- `Dockerfile` builds the release binary and copies it, `migrations/`, and
  `shopify.app*.toml` into a slim image. It sets `OPENSHOP_ENV=production` and
  `PORT=3000` and starts the web process with `app start`.
- `Procfile` declares three processes: `release: app migrate`,
  `web: app start`, and `worker: app worker --concurrency=5`.
- `docker-compose.yml` starts a local PostgreSQL 17 with the `openshop` role
  and database expected by the default `DATABASE_URL`.
- `.env.example` lists the variables to copy into `.env`. `ENCRYPTION_KEY` is
  commented out: development stores credentials in plaintext until you set
  it. See [Environment variables](environment-variables.md).

## Generated files

Nothing is generated at build time: `graphql!` types Shopify operations while
the crate compiles, without writing files. The generated files are the SQL
migrations written by `openshop init` and `migrate generate`, to review and
commit with `migrations/schema.snapshot.json`, and `graphql/admin.schema.json` when `openshop codegen` downloaded the
schema of [another API version](graphql.md#schema-and-api-version).

## Modules

`main.rs` declares the app's modules, and definitions are referenced by path:

```rust,ignore
mod flows;
mod providers;

use openshop::{OpenShopConfig, Result};

fn config() -> Result<OpenShopConfig> {
    OpenShopConfig::builder()
        .provider(providers::warehouse::warehouse())
        .flow(flows::sync_orders::sync_orders())
        .build()
}
```

Each directory has a `mod.rs` that declares its files (`pub mod sync_orders;`).
When you also need a library target, for example to share the configuration
with integration tests in `tests/`, move the modules and `config()` to
`src/lib.rs` and keep `main.rs` as a thin entry point.
