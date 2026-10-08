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
│   ├── <timestamp>_openshop_init.sql
│   └── <timestamp>_openshop_queue_indexes.sql
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
module layout is a convention; a block takes effect when the application
lists it, in [`app!`](configuration.md).

| Optional module | Add it when you need | In `app!` |
| --- | --- | --- |
| `src/proxy/` | Storefront or Customer Account endpoints, each Liquid template next to its handler | `proxies` |
| `src/routes/` | HTTP endpoints with explicit authentication | `routes` |
| `src/webhooks/` | Shopify event handlers | `webhooks` |
| `src/functions/` | Shopify Function definitions | `functions` |
| `src/models/` | Application-owned database tables | `models` |
| `src/admin/` | Server side of experimental custom admin screens | `admin_pages` |
| `src/mcp.rs` | MCP tools, resources, and their permissions | `mcp_tools` and `mcp_resources` |
| `tests/` | Integration tests using `openshop::testing` | — |
| `ui/` | WebAssembly admin UI bundle with the views of custom admin screens, created by `openshop init-ui` | `admin_ui`, or `OPENSHOP_UI_DIR` |

An application that uses every block is laid out like `apps/demo` in the
OpenShop repository:

```text
src/
├── lib.rs
├── main.rs
├── mcp.rs
├── admin/
│   ├── mod.rs
│   └── reviews.rs
├── flows/
│   ├── mod.rs
│   ├── count_variants.rs
│   └── sync_orders.rs
├── functions/
│   ├── mod.rs
│   └── volume_discount.rs
├── models/
│   ├── mod.rs
│   └── review.rs
├── providers/
│   ├── mod.rs
│   └── warehouse.rs
├── proxy/
│   ├── mod.rs
│   ├── api_reviews.rs
│   ├── reviews.rs
│   └── reviews.liquid
├── routes/
│   ├── mod.rs
│   └── ping.rs
└── webhooks/
    ├── mod.rs
    ├── app_uninstalled.rs
    └── orders_create.rs
```

`lib.rs` declares the modules and the application, and `main.rs` runs it; see
[Modules](#modules).

## `Cargo.toml`

Declares the package and its dependencies:

- `openshop`, from the Git repository, pinned to the tag of the release that
  created the project (`tag = "v<version>"`), or from a local path when the
  project was created with `--openshop-path`;
- `serde`, used to derive `Serialize` and `Deserialize` on the types the
  application exchanges, such as the arguments of a provider method or the
  payload of a webhook;
- `openshop` again under `[dev-dependencies]` with the `testing` feature.

An application needs neither `schemars` nor `tokio`: `#[input]` derives what
the input of a flow needs, and `APP.main()` starts the async runtime.

The `[profile.release]` section enables thin LTO and strips debug information.
The `scrypt` and `salsa20` packages are optimized in the development profile
too, because MCP token hashing is very slow without optimizations.

## `src/main.rs`

Holds the application and the entry point of the `dev`, `start`, `worker`, and
`migrate` commands. An application is declared with `app!`, which lists its
providers, flows, webhooks, routes, Shopify Functions, and MCP capabilities,
and sets its crons, worker settings, retry policy, and admin page visibility;
`main` runs it, as shown under [Modules](#modules). See
[Configuration](configuration.md).

The generated file declares `APP` with `app!`: the sample provider
`Warehouse`, the sample flow `sync_orders`, a "Quick sync" cron that runs the
flow every five minutes, and an `on_error` hook that prints failed flows.
`main` runs it with `APP.main()`. The file also contains the generated tests:
one checks that the application is valid, the other runs the flow with
`#[openshop::test(APP)]`. It needs PostgreSQL and is ignored by default: run
it with `cargo test -- --ignored`.

MCP is off in the generated file: `mcp: McpConfig::disabled()` refuses
`POST /mcp` and removes the token page from the admin. Remove the line to use
[MCP](mcp.md).

## `src/providers/`

Contains provider definitions. Providers define admin config fields and methods
for external systems. See [Providers](providers.md).

## `src/flows/`

Contains background job definitions. Flows run in workers and can use
checkpointed steps, the Shopify client, provider connectors, logs, and the
database. See [Flows](flows.md).

## `src/proxy/`

Contains app proxy and Customer Account extension handlers. Each handler is a
function marked `#[proxy(GET, "/path")]`, one per method of a path. The Liquid
template of a handler is a file next to it: `src/proxy/reviews.liquid` beside
`src/proxy/reviews.rs`, which embeds it with `liquid!("reviews.liquid")`. See
[Proxy routes](proxy-routes.md).

## `src/routes/`

Contains optional public server routes mounted under `/routes/*`. Every route
is a function marked `#[route(GET, "/path")]`, public unless it names an
authentication function with `auth = …`. See
[Public server routes](server-routes.md).

## `src/admin/`

Contains the server side of experimental custom admin pages: `#[admin_page]`
blocks with their loaders, actions, and access rules. The page views are
Rust components compiled into a separate WebAssembly UI bundle. See
[Custom admin pages](custom-admin-pages.md).

## `migrations/`

Contains committed SQL migrations owned by the app. The first file holds the
framework tables and the second the indexes of the run queue; later files hold
framework schema updates and your own tables. See
[Database and migrations](database.md).

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

`main.rs` declares the app's modules, and the application lists the blocks
they declare:

```rust,ignore
mod flows;
mod providers;

use openshop::prelude::*;

use flows::sync_orders::sync_orders;
use providers::warehouse::Warehouse;

static APP: App = app! {
    providers: [Warehouse],
    flows: [sync_orders],
};

fn main() -> Result<()> {
    APP.main()
}
```

`use openshop::prelude::*;` imports what application code uses: `App`,
`Result`, the contexts of the handlers, the attributes and derives that
declare blocks, and macros such as `app!`, `json!`, `graphql!`, and `liquid!`.
A file lists explicitly only what the prelude lacks, for example
`use serde::Deserialize;` or `use openshop::testing::TestContext;`. The
prelude does not include `#[openshop::test]`, which would hide `#[test]`.

`APP.main()` starts the async runtime and runs the command given on the
command line. A binary that has its own runtime calls `APP.run().await`
instead.

Each directory has a `mod.rs` that declares its files (`pub mod sync_orders;`).
An attribute on a function, such as `#[flow]`, adds a type of the same name as
the function, so do not re-export a declared function next to its module:
`pub use sync_orders::sync_orders;` beside `pub mod sync_orders;` does not
compile. Import the function where the application is declared, as above.

When you also need a library target, for example to share the application
with integration tests in `tests/`, move the modules and `APP` to `src/lib.rs`
(`pub static APP`) and keep `main.rs` as a thin entry point:

```rust,ignore
// src/main.rs
fn main() -> openshop::Result<()> {
    my_app::APP.main()
}
```

A test reads the configuration with `APP.config()`.
