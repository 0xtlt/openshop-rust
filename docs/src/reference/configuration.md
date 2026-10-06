# Configuration

An OpenShop app describes itself once with `OpenShopConfig::builder()` and
hands the function that builds the configuration to `openshop::cli::run`.
Nothing is discovered from files: every flow, provider, route, and page is
registered on the builder.

```rust,ignore
use openshop::{CronEntry, OpenShopConfig, PartialRetryPolicy, PartialWorkerConfig, Result, json};

fn config() -> Result<OpenShopConfig> {
    OpenShopConfig::builder()
        .provider(providers::warehouse::warehouse())
        .flow(flows::sync_orders::sync_orders())
        .cron(
            CronEntry::new("*/5 * * * *", "syncOrders")
                .name("Quick sync")
                .input(json!({ "limit": 10 }))
                .shops("all"),
        )
        .worker(PartialWorkerConfig::new().concurrency(5))
        .retry_policy(
            PartialRetryPolicy::new()
                .max_attempts(3)
                .initial_interval_ms(1_000)
                .backoff_coefficient(2.0)
                .max_interval_ms(30_000),
        )
        .on_error(|error, context| async move {
            eprintln!("[openshop:error] {context:?}: {error:#}");
        })
        .build()
}

#[tokio::main]
async fn main() -> Result<()> {
    openshop::cli::run(config).await
}
```

`openshop::cli::run` accepts either a function returning
`Result<OpenShopConfig>` or an already built `OpenShopConfig`. Prefer the
function: `dev` then loads `.env` before the configuration is built, so the
configuration can read variables defined there.

At runtime the configuration lives inside an `OpenShop` handle. Every handler
context carries one as `ctx.app`: use `ctx.app.dispatch_flow(…)` to enqueue
durable work from a webhook, a proxy, a server route, or an admin function. See
[`OpenShop`](sdk.md#openshop).

## Builder methods

| Method | Argument | Default when not called |
| --- | --- | --- |
| `.provider(provider)` | `Provider`, registered under `provider.name` | No providers |
| `.provider_as(key, provider)` | Key and `Provider` | — |
| `.providers(providers)` | Iterator of `Provider`, each registered under its own name | — |
| `.flow(flow)` | `Flow`, registered under `flow.name` | No flows |
| `.flow_as(key, flow)` | Key and `Flow` | — |
| `.flows(flows)` | Iterator of `Flow`, each registered under its own name | — |
| `.with(register)` | `FnOnce(OpenShopBuilder) -> OpenShopBuilder` | — |
| `.shopify(config)` | `ShopifyConfig` | Single app resolved from env and Shopify TOML |
| `.function(key, definition)` | Key and `FunctionDefinition` | No functions |
| `.mcp(config)` | `McpConfig` | Core MCP capabilities enabled; no custom capabilities |
| `.webhook(topic, webhook)` | Topic and `Webhook` | No handlers |
| `.cron(entry)` | `CronEntry` | No crons |
| `.proxy(path, definition)` | Route path and `ProxyDefinition` | No proxy routes; `/proxy` and `/ext` are not mounted |
| `.route(path, route)` | Route path and `ServerRoute` | No server routes; `/routes` is not mounted |
| `.admin_page(page)` | `AdminPage` | No custom admin pages |
| `.custom_pages(navigation)` | Sidebar entries (`CustomAdminNavigationItem`) | Custom pages disabled; see [Custom admin pages](custom-admin-pages.md) |
| `.experimental(config)` | `ExperimentalConfig` | Same as above; replaces `.custom_pages(…)` |
| `.pages(config)` | `AdminPagesConfig` | Every admin page `visible` |
| `.page(id, mode)` | `AdminPageId` and `AdminPageMode` | — |
| `.model(model)` | `Model` | No app tables pushed in development |
| `.admin_ui(ui)` | `UiSource` | The admin UI bundle built into the framework; see [Custom admin pages](custom-admin-pages.md#serve-the-bundle) |
| `.max_request_body_bytes(bytes)` | `usize` | `2 * 1024 * 1024` (2 MB); larger request bodies are answered with `413` |
| `.worker(config)` | `PartialWorkerConfig` | See [Worker defaults](#worker-defaults) |
| `.retry_policy(policy)` | `PartialRetryPolicy` | See [Retry defaults](#retry-defaults) |
| `.on_error(hook)` | `Fn(Arc<anyhow::Error>, ErrorContext) -> impl Future<Output = ()>` | No hook |
| `.build()` | — | Returns `Result<OpenShopConfig>` after [validation](#runtime-validation) |

Registering a second definition under an existing key replaces the first one.
`.pages(…)` and `.page(…)` merge with earlier calls; later values win.

The fields of `OpenShopConfig` are public. `OpenShop::new` validates the
configuration again, so a configuration modified after `build()` is still
checked before it is served.

## Shopify

Omitting `ShopifyConfig::apps` enables single-app mode:

- `SHOPIFY_API_KEY` and `SHOPIFY_API_SECRET` provide the credentials;
- `HOST`, then `SHOPIFY_APP_URL`, provides the public app URL;
- `ShopifyConfig::scopes` provides the scopes. Without it they are read from
  the first `shopify.app.<name>.toml` file (in name order) that declares
  scopes, then from `shopify.app.toml`.

The app handle is `default` in single-app mode.

For multiple apps, define one entry per stable app handle:

```rust,ignore
use openshop::{Context, IndexMap, OpenShopConfig, Result, ShopifyAppConfig, ShopifyConfig};

fn env(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("{name} is not set"))
}

fn config() -> Result<OpenShopConfig> {
    let mut apps = IndexMap::new();
    apps.insert(
        "retail".to_string(),
        ShopifyAppConfig::Toml {
            toml: "shopify.app.retail.toml".to_string(),
            api_secret: env("SHOPIFY_RETAIL_API_SECRET")?,
            app_url: None,
        },
    );
    apps.insert(
        "wholesale".to_string(),
        ShopifyAppConfig::Keys {
            api_key: env("SHOPIFY_WHOLESALE_API_KEY")?,
            api_secret: env("SHOPIFY_WHOLESALE_API_SECRET")?,
            app_url: std::env::var("SHOPIFY_WHOLESALE_APP_URL").ok(),
        },
    );

    OpenShopConfig::builder()
        .shopify(ShopifyConfig {
            scopes: Some("read_products,read_orders".to_string()),
            apps: Some(apps),
        })
        .build()
}
```

| Field | Type | Behavior |
| --- | --- | --- |
| `ShopifyConfig::scopes` | `Option<String>` | Global non-empty scope string. All configured apps share it. |
| `ShopifyConfig::apps` | `Option<IndexMap<String, ShopifyAppConfig>>` | Enables multi-app mode. Handles may contain letters, numbers, `_`, and `-`. Must contain at least one app. |
| `ShopifyAppConfig::Toml { toml, .. }` | `String` | Reads `client_id`, `application_url`, and `scopes` from this project-relative TOML path. |
| `ShopifyAppConfig::Keys { api_key, .. }` | `String` | Client ID for an app without a TOML file. |
| `api_secret` (both variants) | `String` | Required non-empty secret. Keep it in an environment variable. |
| `app_url` (both variants) | `Option<String>` | Optional URL override. `Toml` entries otherwise use `application_url`, `HOST`, then `SHOPIFY_APP_URL`; `Keys` entries use `HOST`, then `SHOPIFY_APP_URL`. |

Per-app scopes are not supported. If `ShopifyConfig::scopes` is omitted, all
configured apps must resolve to the same scope string; otherwise startup fails
with `[openshop] Shopify app scopes must be identical across all configured apps`.

Shopify apps are resolved once, when the `OpenShop` handle is created. A
missing TOML file (`[openshop] Shopify app "<handle>" TOML not found: <path>`)
or a TOML file without `client_id` stops the process at startup.

See [Multi-app configuration](environment-variables.md#multi-app-configuration)
for how variables you read yourself interact with `.env`.

## Split the registration

`.providers([…])` and `.flows([…])` register several definitions at once, each
under its own name. `.with(register)` hands the builder to a function, so that
a module registers everything it defines and `config()` stays short:

```rust,ignore
use openshop::{OpenShopBuilder, OpenShopConfig, Result};

// src/reviews/mod.rs
pub fn register(builder: OpenShopBuilder) -> OpenShopBuilder {
    builder
        .flow(moderate_reviews())
        .proxy("/reviews", list_reviews())
        .admin_page(reviews_page())
}

// src/main.rs
fn config() -> Result<OpenShopConfig> {
    OpenShopConfig::builder()
        .providers([warehouse(), erp()])
        .flows([sync_orders(), sync_products()])
        .with(reviews::register)
        .build()
}
```

Registration order is kept, and a later registration under the same key
replaces the earlier one, as with the singular methods.

## Flows

Register flow definitions under stable keys. The key is the name used by
dispatch, crons, runs, and logs. `.flow(flow)` uses the flow's own `name` as the
key; `.flow_as(key, flow)` registers it under another one. The
[flow reference](flows.md) owns the definition options, context, checkpoint
contract, and timeout behavior.

## Crons

`CronEntry::new(schedule, flow)` creates an entry; the other fields are set
with builder methods.

| Field | Method | Type | Default |
| --- | --- | --- | --- |
| `name` | `.name(…)` | `String` | Omitted; display logs fall back to the flow name |
| `schedule` | `CronEntry::new` | cron expression string | Required |
| `flow` | `CronEntry::new` | registered flow key | Required |
| `input` | `.input(…)` | JSON object (`serde_json::Value`) | `{}` |
| `shops` | `.shops(…)` | `CronShops`, or anything that converts into it | `CronShops::Global` |

The schedule is a five-field cron expression, a six-field expression with
leading seconds, or one of the nicknames `@yearly`, `@annually`, `@monthly`,
`@weekly`, `@daily`, and `@hourly`. It is evaluated in the local time zone of
the process that runs the scheduler. An invalid expression is rejected when the
configuration is built.

`CronEntry::key()` returns `<flow>:<schedule>`, the identifier used by per-shop
overrides. An `input` that is not a JSON object is ignored.

## Admin pages

Page modes control which embedded admin screens appear in the Shopify sidebar
and whether their admin API stays reachable. Home is always visible. Flows,
Providers, Crons, and Functions are automatically hidden from the sidebar when
their corresponding collection is empty; their direct URLs and admin APIs
remain available. MCP stays visible by default because OpenShop provides core
MCP capabilities without custom configuration.

Runtime behavior is unchanged: crons still fire, webhooks still run, Shopify
Functions still execute, and `POST /mcp` still follows `McpConfig::enabled`.

```rust,ignore
use openshop::{AdminPageId, AdminPageMode, OpenShopConfig, Result};

fn config() -> Result<OpenShopConfig> {
    OpenShopConfig::builder()
        .flow(flows::sync_orders::sync_orders())
        .page(AdminPageId::Functions, AdminPageMode::Hidden)
        .page(AdminPageId::Mcp, AdminPageMode::Disabled)
        .build()
}
```

| Mode | Sidebar | Direct URL | Admin API `/api/*` |
| --- | --- | --- | --- |
| `AdminPageMode::Visible` (default) | Shown when the corresponding collection has entries | Works | 200 |
| `AdminPageMode::Hidden` | Hidden | Works | 200 |
| `AdminPageMode::Disabled` | Hidden | Shows “This page is disabled” | 404 |

`AdminPageId` has the variants `Flows`, `Providers`, `Crons`, `Functions`, and
`Mcp`. `.pages(AdminPagesConfig { … })` sets several pages at once; its fields
are `flows`, `providers`, `crons`, `functions`, and `mcp`, each an
`Option<AdminPageMode>`.

`Hidden` is navigation-only and must not be used as an access-control boundary.
Use `Disabled` when shop staff must not be able to open a page or call its
admin API.

The MCP dashboard and protocol are independent:

```rust,ignore
use openshop::{AdminPageId, AdminPageMode, McpConfig, OpenShopConfig, Result};

fn config() -> Result<OpenShopConfig> {
    OpenShopConfig::builder()
        .flow(flows::sync_orders::sync_orders())
        .page(AdminPageId::Mcp, AdminPageMode::Disabled) // Block the dashboard and /api/mcp/*.
        .mcp(McpConfig::new().enabled(false)) // Block POST /mcp.
        .build()
}
```

- Set only the page mode to `Disabled` to block MCP administration while
  existing MCP tokens can continue using `POST /mcp`.
- Set only `McpConfig::enabled(false)` to stop the protocol while leaving the
  dashboard available for token administration.
- Set both values to disable MCP completely.

## Runtime validation

`.build()` returns an error instead of a configuration when a definition is
invalid. Messages have the form `[openshop] Invalid config: <path> <problem>`,
for example
`[openshop] Invalid config: crons[0].flow references unknown flow "syncOrder"`.
The checks are:

- cron entries must reference registered flows and carry a valid schedule;
- provider and flow names must be non-empty;
- provider and function fields must have a non-empty label, and `select`
  fields non-empty, uniquely valued options;
- Shopify Function handles must be non-empty and unique, and `modes` is only
  accepted on discount functions;
- worker and retry numbers must be positive;
- flow timeouts and step timeouts must be positive when set;
- Shopify app handles, secrets, keys, TOML paths, and URL overrides must be
  non-empty and well formed;
- proxy and server route paths must be unambiguous, their parameter names may
  only contain letters, numbers, and `_`, and every server route must declare
  at least one HTTP method;
- custom admin page paths must be valid, unreserved, and unambiguous, and
  custom navigation must point to registered static pages;
- MCP permissions, tools, and resources must be well formed. See
  [MCP](mcp.md).

## Cron shop targeting

Cron entries support these shop modes:

| `CronShops` | `.shops(…)` shorthand | Behavior |
| --- | --- | --- |
| `Global` | `"global"` | Run once without a shop context. The run uses the shop `__global__` and the app handle `default`. |
| `All` | `"all"` | Run once per installation that is not uninstalled, for every `(app handle, shop)` pair. |
| `Shop(shop)` | `"shop.myshopify.com"` | Run only for one shop. |
| `Shops(shops)` | `vec!["a.myshopify.com".to_string(), "b.myshopify.com".to_string()]` | Run for selected shops. |

A listed shop runs once per app it is installed for. A listed shop without an
installation still runs, under the app handle `default`. A cron disabled for a
shop through the admin UI or MCP is skipped for that shop.

## Worker defaults

`.worker(…)` accepts a `PartialWorkerConfig`. Missing fields use these runtime
defaults:

| Field | Type | Default | Purpose |
| --- | --- | --- | --- |
| `concurrency` | `usize`, positive | `5` | Maximum active runs in one worker process. |
| `poll_interval_ms` | `u64`, positive | `1000` | Initial queue polling interval and delay while all slots are occupied. |
| `poll_max_interval_ms` | `u64`, positive | `5000` | Maximum empty-queue backoff. |
| `poll_backoff_coefficient` | `f64`, positive | `1.5` | Multiplier applied after consecutive empty polls. |
| `lease_duration_ms` | `u64`, positive | `30000` | Claim lease and graceful-stop deadline. Active runs refresh their lease every 15 seconds, or twice per lease period when that is shorter. |

Each field has a builder method of the same name on `PartialWorkerConfig`.
The `worker` command overrides `concurrency` only when `--concurrency=N` is
given; other worker settings come from the configuration. `dev` uses the
configuration as is. Run multiple worker processes to scale horizontally;
PostgreSQL claims work with `FOR UPDATE SKIP LOCKED`.

## Retry defaults

The app-level `.retry_policy(…)` supplies defaults for registered flows. Flow
and dispatch settings override it field by field. See the
[retry defaults and precedence](flows.md#retry-precedence-and-defaults)
for the authoritative option table and deadline behavior.

## Environment variables

See [Environment variables](environment-variables.md) for required
values, defaults, `.env` loading, and resolution order. `dev` loads the
project-root file; migration and production commands use the process
environment.
