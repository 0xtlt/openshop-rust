# Configuration

An OpenShop app declares itself once with `app!`: the building blocks it is
made of, each declared where it is written, and its settings. Nothing is
discovered from files: a flow, a provider, a route, or a page takes effect
when the application lists it.

```rust,ignore
use std::sync::Arc;
use std::time::Duration;

use openshop::prelude::*;

use flows::sync_orders::sync_orders;
use providers::warehouse::Warehouse;

async fn report_failure(error: Arc<openshop::Error>, context: ErrorContext) {
    eprintln!("[openshop:error] {context:?}: {error:#}");
}

pub static APP: App = app! {
    providers: [Warehouse],
    flows: [sync_orders],
    crons: [
        Cron::new("*/5 * * * *", sync_orders::NAME)
            .name("Quick sync")
            .input(json!({ "limit": 10 }))
            .all_shops(),
    ],
    worker: WorkerConfig::new().concurrency(5),
    retry_policy: RetryPolicy::new()
        .max_attempts(3)
        .initial_interval(Duration::from_secs(1))
        .backoff_coefficient(2.0)
        .max_interval(Duration::from_secs(30)),
    on_error: report_failure,
};

fn main() -> Result<()> {
    APP.main()
}
```

`app!` is an expression of type `App`, usually kept in a `static`. An `App` is
a value: the binary runs it, a test reads its configuration.

| Method | Returns |
| --- | --- |
| `APP.main()` | `Result<()>`. Starts the runtime and runs the command given on the command line: `dev`, `start`, `worker`, or `migrate`. See [CLI commands](cli.md#command-behavior). |
| `APP.run().await` | `Result<()>`. Runs the command in a binary that has its own runtime. |
| `APP.config()` | `Result<OpenShopConfig>`: the configuration, after [validation](#runtime-validation). |
| `APP.builder()` | `Result<OpenShopBuilder>`: the builder holding the blocks and settings of the declaration, to add to them. It fails when a setting does. See [Builder methods](#builder-methods). |

Nothing is evaluated before one of these is called, and each call starts over.
`APP.main()` and `APP.run()` read the declaration after `dev` loaded `.env`,
so a setting can read variables defined there.

At runtime the configuration lives inside an `OpenShop` handle. Every handler
context carries one as `ctx.app`. To enqueue durable work from a webhook, a
proxy, or an admin function, call the `dispatch` of the flow with the context:
`sync_orders::dispatch(&ctx, input)`. A server route passes what
`ctx.for_shop(…)` returns. See
[`sync_orders::dispatch`](sdk.md#sync_ordersdispatchctx-input) and
[`OpenShop`](sdk.md#openshop).

## Lists

Each list takes the blocks of one kind, by name.

| List | Takes | Declared with |
| --- | --- | --- |
| `providers` | Provider structs | `#[derive(ProviderConfig)]` on the struct and `#[provider]` on its `impl` block. See [Providers](providers.md). |
| `flows` | Flow functions | `#[flow]`. See [Flows](flows.md). |
| `functions` | Shopify Function configuration structs | `#[derive(FunctionConfig)]`. See [Shopify Functions](shopify-functions.md). |
| `models` | Model structs | `#[derive(Model)]` and `#[model("table")]`. See [App models](database.md#app-models). |
| `admin_pages` | Custom admin page types | `#[admin_page("/path")]` on the `impl` block, with `nav = "…"` for an entry in the admin menu. Listing a page is enough to serve it. See [Custom admin pages](custom-admin-pages.md). |
| `webhooks` | Webhook functions | `#[webhook("topic")]`. See [Webhooks](webhooks.md). |
| `proxies` | App proxy functions | `#[proxy(GET, "/path")]`, with `liquid` or `html` for another response type than JSON. See [Proxy routes](proxy-routes.md). |
| `routes` | Server route functions | `#[route(GET, "/path")]`, with `json` for a function that returns a value to serialize. See [Public server routes](server-routes.md). |
| `mcp_tools` | MCP tool functions | `#[mcp_tool("name")]`. See [MCP](mcp.md). |
| `mcp_resources` | MCP resource functions | `#[mcp_resource("uri", name = "…")]`. See [MCP](mcp.md). |

A block is named as it is in scope: imported (`flows: [sync_orders]`) or by
path (`flows: [flows::sync_orders::sync_orders]`). Listing a block under
another kind, or a function without its attribute, does not compile. Each key
of the declaration, list or setting, is written once.

An attribute on a function leaves it an ordinary function and adds a type of
the same name, with the constants of the block (`sync_orders::NAME`,
`orders_create::TOPICS`, `ping::PATH`) and, for a flow,
`sync_orders::dispatch`. That type is what a list takes. Because
of it, do not re-export a declared function next to a module of the same name:
`pub use sync_orders::sync_orders;` beside `mod sync_orders;` does not compile.
Import the function where the application is declared.

## Settings

Every other entry is a setting, written `name: value`. The value is an
expression, passed to the [builder method](#builder-methods) of that name when
the configuration is built. The application reads a secret from the
environment itself: `ops_password: std::env::var("OPS_PASSWORD").ok()`. A value
can fail with `?`: the error is the one `APP.config()` and `APP.run()` return.

| Setting | Value | Default when absent |
| --- | --- | --- |
| `crons` | [`Cron`](#crons) values, as an array or any other iterator | No crons |
| `on_error` | `async fn(Arc<openshop::Error>, ErrorContext)`, called after each failed attempt of a flow run | No hook |
| `mcp` | `McpConfig`: `McpConfig::disabled()` to turn MCP off, or `McpConfig::new()` with a permission no tool or resource lists (`.permission(…)`) and tools or resources built without an attribute | MCP enabled, with the core capabilities and the permissions the tools and resources of the application list |
| `admin_menu` | The entries of the admin menu for the pages of the application (`AdminMenuItem::new(label, path)`), as an array or any other iterator, to set another order or other labels than the pages declare | One entry per page that declares `nav = "…"`, in the order of `admin_pages`; see [Custom admin pages](custom-admin-pages.md) |
| `shopify` | [`ShopifyConfig`](#shopify) | Single app resolved from env and Shopify TOML |
| `worker` | `WorkerConfig` | See [Worker defaults](#worker-defaults) |
| `retry_policy` | `RetryPolicy` | See [Retry defaults](#retry-defaults) |
| `builtin_pages` | [`AdminPagesConfig`](#admin-pages), or `AdminPagesConfig::none()` for an admin made of the pages of the application | Every built-in page of the admin `visible` |
| `admin_ui` | `UiSource` | The admin UI bundle built into the framework; see [Custom admin pages](custom-admin-pages.md#serve-the-bundle) |
| `max_request_body_bytes` | `usize` | `2 * 1024 * 1024` (2 MB); larger request bodies are answered with `413` |
| `request_timeout` | `Duration` | `60 s`; a request whose response has not begun by then is answered with `503` (`Request timed out`). The time to read the body counts; a response already streaming does not |
| `max_concurrent_requests` | `usize` | `1024`; a request that comes while that many are being served is answered with `503` and `Retry-After: 1`, and `/health` is always answered |
| `ops_password` | `Option<String>`, or a `String`: the password of the [operator dashboard](operator-dashboard.md) | No dashboard: `/ops` answers `404` |
| `log_retention` | `Option<Duration>`, or a `Duration`: the age after which workers remove a log line, in batches; see [Retention and export](logging.md#retention-and-export). Zero is refused with `logRetention must be a positive number` | `None`: log lines are kept forever |
| `sentry` | `Option<String>`, or a `String`: the DSN of the project failures are reported to, or a `SentryConfig` that also names the environment and the release; see [Sentry](sentry.md) | Nothing is reported |
| `client_credentials_shop` | `Option<String>`, or a `String`: the one `*.myshopify.com` development store that work no Shopify request vouches for (flows, crons, routes, webhook handlers, proxies, the MCP tool) may get an access token for with the [client credentials grant](authentication.md#client-credentials-grant) without an installation row. A value that is not a shop domain stops the application with `[openshop] client_credentials_shop must be a *.myshopify.com domain` | Only a signed launch, a session token whose exchange Shopify refused, or the renewal of a stored token asks |

## Builder methods

`app!` expands to calls on an `OpenShopBuilder`: a list adds each of its
blocks, a setting calls the method of its name. The builder is the lower
level. Use it in a test that adds to the application, and for a block built at
run time.

```rust,ignore
use openshop::prelude::*;
use openshop::builder::Flow;
use openshop::OpenShopConfig;

// The declaration, with one more flow.
fn extended() -> Result<OpenShopConfig> {
    APP.builder()?
        .flow(Flow::new("extra", |_ctx| async { Ok(()) }))
        .build()
}

// From an empty builder: a declared block is added by its name, whatever its kind.
fn minimal() -> Result<OpenShopConfig> {
    OpenShopConfig::builder()
        .add::<Warehouse>()
        .add::<sync_orders>()
        .build()
}
```

A binary runs such a configuration with `openshop::cli::run(extended)`, which
takes a function returning `Result<OpenShopConfig>`: `dev` loads `.env` before
it builds the configuration.

| Method | Argument | Default when not called |
| --- | --- | --- |
| `.add::<Block>()` | A declared block, as a type: `.add::<Warehouse>()`, `.add::<sync_orders>()` | — |
| `.provider(provider)` | `Provider`, registered under `provider.name` | No providers |
| `.provider_as(key, provider)` | Key and `Provider` | — |
| `.flow(flow)` | `Flow`, registered under `flow.name` | No flows |
| `.shopify(config)` | `ShopifyConfig` | Single app resolved from env and Shopify TOML |
| `.function(key, definition)` | Key and `Function` | No functions |
| `.mcp(config)` | `McpConfig` | MCP enabled, with the core capabilities; no custom capabilities |
| `.mcp_tool(name, definition)` | Tool name and `McpTool` | No custom tools |
| `.mcp_resource(uri, definition)` | Resource URI and `McpResource` | No custom resources |
| `.webhook(topic, webhook)` | Topic and `Webhook` | No handlers |
| `.cron(entry)` | `Cron` | No crons |
| `.crons(entries)` | Iterator of `Cron` | — |
| `.proxy(path, definition)` | Route path and `Proxy` | No proxy routes; `/proxy` and `/ext` are not mounted |
| `.route(path, route)` | Route path and `Route` | No server routes; `/routes` is not mounted |
| `.admin_page(page)` | `AdminPage` | No custom admin pages |
| `.admin_menu(menu)` | The entries of the admin menu for the pages of the application (`AdminMenuItem`), in place of the ones the pages declare | One entry per page that declares a `nav` label; see [Custom admin pages](custom-admin-pages.md) |
| `.builtin_pages(config)` | `AdminPagesConfig` | Every built-in page of the admin `visible` |
| `.builtin_page(id, mode)` | `AdminPageId` and `AdminPageMode` | — |
| `.model(table)` | `Table` | No app tables pushed in development |
| `.admin_ui(ui)` | `UiSource` | The admin UI bundle built into the framework; see [Custom admin pages](custom-admin-pages.md#serve-the-bundle) |
| `.max_request_body_bytes(bytes)` | `usize` | `2 * 1024 * 1024` (2 MB); larger request bodies are answered with `413` |
| `.request_timeout(timeout)` | `Duration` | `60 s`; see the setting of the same name |
| `.max_concurrent_requests(requests)` | `usize` | `1024`; see the setting of the same name |
| `.ops_password(password)` | `Option<String>` or `String` | No [operator dashboard](operator-dashboard.md) |
| `.log_retention(retention)` | `Option<Duration>` or `Duration` | Log lines are kept forever |
| `.sentry(sentry)` | `Option<String>`, `String`, `&str` or `SentryConfig`; needs the `sentry` feature, on by default | Nothing is reported to [Sentry](sentry.md) |
| `.client_credentials_shop(shop)` | `Option<String>` or `String` | Background work asks no token for a shop without a row |
| `.worker(config)` | `WorkerConfig` | See [Worker defaults](#worker-defaults) |
| `.retry_policy(policy)` | `RetryPolicy` | See [Retry defaults](#retry-defaults) |
| `.on_error(hook)` | `Fn(Arc<anyhow::Error>, ErrorContext) -> impl Future<Output = ()>` | No hook |
| `.build()` | — | Returns `Result<OpenShopConfig>` after [validation](#runtime-validation) |

A key is registered once. `build()` refuses a second provider, flow, function,
webhook topic, model table, MCP tool, MCP resource, or MCP permission under a
key that is taken, for example with
`[openshop] Invalid config: flows.syncOrders is registered twice`. This holds
for a declared block added or listed twice as well as for the methods above.

Calling the other methods again:

- `.proxy(path, …)` and `.route(path, …)` add methods to a path that is
  already registered. A method registered twice is refused
  (`proxies./widgets handles GET twice`, `routes./status handles GET twice`).
  The methods of a proxy path share one response type
  (`proxies./banner answers with both liquid and json; a path has one response type`);
  each method of a route keeps its own authentication.
- `.mcp(config)` adds its permissions, tools, and resources to the ones
  registered instead of replacing them. Its `enabled`, when set, replaces the
  earlier value.
- `.cron(…)`, `.crons(…)`, and `.admin_page(…)` append.
- `.builtin_pages(…)` and `.builtin_page(…)` merge with earlier calls; later values win.
- The remaining settings (`.shopify(…)`, `.worker(…)`, `.retry_policy(…)`,
  `.on_error(…)`, `.admin_menu(…)`, `.admin_ui(…)`,
  `.max_request_body_bytes(…)`, `.request_timeout(…)`,
  `.max_concurrent_requests(…)`, `.ops_password(…)`, `.log_retention(…)`,
  `.sentry(…)`) replace the
  earlier value.

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
use openshop::prelude::*;
use openshop::{IndexMap, ShopifyAppConfig, ShopifyConfig};

pub static APP: App = app! {
    shopify: ShopifyConfig {
        scopes: Some("read_products,read_orders".to_string()),
        apps: Some(IndexMap::from([
            (
                "retail".to_string(),
                ShopifyAppConfig::Toml {
                    toml: "shopify.app.retail.toml".to_string(),
                    api_secret: std::env::var("SHOPIFY_RETAIL_API_SECRET")
                        .context("SHOPIFY_RETAIL_API_SECRET is not set")?,
                    app_url: None,
                },
            ),
            (
                "wholesale".to_string(),
                ShopifyAppConfig::Keys {
                    api_key: std::env::var("SHOPIFY_WHOLESALE_API_KEY")
                        .context("SHOPIFY_WHOLESALE_API_KEY is not set")?,
                    api_secret: std::env::var("SHOPIFY_WHOLESALE_API_SECRET")
                        .context("SHOPIFY_WHOLESALE_API_SECRET is not set")?,
                    app_url: std::env::var("SHOPIFY_WHOLESALE_APP_URL").ok(),
                },
            ),
        ])),
    },
};
```

A `?` makes the declaration fail when a variable is not set: `APP.config()`
and every command of `APP.run()` return the error, here
`SHOPIFY_RETAIL_API_SECRET is not set`.

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

A declaration has nothing to split: each block is declared in the module that
writes it, and `app!` names it, imported or by path. Registration order is
kept, and a key registered twice is refused.

## Flows

A flow listed in `flows` is registered under its name: the one given to
`#[flow]`, or the name of the function in camel case, also available as the
constant `sync_orders::NAME`. That key is the name used by dispatch, crons,
runs, and logs. `sync_orders::dispatch(&ctx, input)` queues a run of it for
the shop of a context. On the builder, `.flow(flow)` uses the flow's own `name` as the
key. The
[flow reference](flows.md) owns the definition options, context, checkpoint
contract, and timeout behavior.

## Crons

The `crons` setting takes the entries of the application.
`Cron::new(schedule, flow)` creates an entry; the other fields are set
with builder methods.

| Field | Method | Type | Default |
| --- | --- | --- | --- |
| `name` | `.name(…)` | `String` | Omitted; display logs fall back to the flow name |
| `schedule` | `Cron::new` | cron expression string | Required |
| `flow` | `Cron::new` | registered flow key, such as `sync_orders::NAME` | Required |
| `input` | `.input(…)` | JSON object (`serde_json::Value`) | `{}` |
| `shops` | `.all_shops()`, `.shop(…)`, or `.shops([…])` | `CronShops`; see [Cron shop targeting](#cron-shop-targeting) | `CronShops::Global`: the cron runs once, for no shop |
| `timezone` | `.timezone(…)` | IANA name, such as `"Europe/Paris"` | UTC |

```rust,ignore
Cron::new("0 2 * * *", sync_orders::NAME)
    .name("Nightly full sync")
    .input(json!({ "limit": 1000 }))
    .all_shops()
```

The schedule is a five-field cron expression, a six-field expression with
leading seconds, or one of the nicknames `@yearly`, `@annually`, `@monthly`,
`@weekly`, `@daily`, and `@hourly`. It is evaluated in UTC, whatever the time
zone of the host, or in the `timezone` of the cron, daylight saving time
included: `Cron::new("0 9 * * *", sync_orders::NAME).timezone("Europe/Paris")`
fires at 09:00 in Paris. An invalid expression or time zone is rejected when
the configuration is built.

A process that was stopped or suspended does not catch up on what it missed.
When it wakes after several occurrences went by, it fires the latest one, once.
A scheduler that stops, because its schedule has no occurrence left or because
it panicked, logs an error, reports it to Sentry, and starts again after a
wait that grows from one second to five minutes.

The `input` of a cron is checked against the [input](flows.md#input) of its
flow when the configuration is built. An input the flow would refuse makes
`APP.config()` and startup fail, for example with
`[openshop] Invalid config: crons[0].input is not an input of flow "syncOrders": input.limit must be >= 1`.
A cron without `input` is checked as `{}`: give an input to the cron of a flow
that requires one.

`Cron::key()` returns `<flow>:<schedule>`, the identifier used by per-shop
overrides. Changing the schedule of a cron changes its key: the overrides of the
old key are never read again, and `/api/crons/toggle` and the MCP tool answer
`Cron not found` to a key the application does not declare. An `input` that is
not a JSON object is ignored.

## Admin pages

Page modes control which embedded admin screens appear in the Shopify sidebar
and whether their admin API stays reachable. Home is always there: the
built-in dashboard, or
[the page the application declares at `/`](custom-admin-pages.md#the-home-of-the-admin). Flows,
Providers, Crons, and Functions are automatically hidden from the sidebar when
their corresponding collection is empty; their direct URLs and admin APIs
remain available. MCP stays visible by default because OpenShop provides core
MCP capabilities without custom configuration. With MCP turned off
(`mcp: McpConfig::disabled()`), its page is `Disabled` unless the application
sets a mode for it.

Runtime behavior is unchanged: crons still fire, webhooks still run, Shopify
Functions still execute, and `POST /mcp` still follows `McpConfig::enabled`.

```rust,ignore
use openshop::prelude::*;
use openshop::{AdminPageMode, AdminPagesConfig};

pub static APP: App = app! {
    flows: [flows::sync_orders::sync_orders],
    builtin_pages: AdminPagesConfig {
        functions: Some(AdminPageMode::Hidden),
        mcp: Some(AdminPageMode::Disabled),
        ..Default::default()
    },
};
```

| Mode | Sidebar | Direct URL | Admin API `/api/*` |
| --- | --- | --- | --- |
| `AdminPageMode::Visible` (default) | Shown when the corresponding collection has entries | Works | 200 |
| `AdminPageMode::Hidden` | Hidden | Works | 200 |
| `AdminPageMode::Disabled` | Hidden | Shows “This page is disabled” | 404 |

The fields of `AdminPagesConfig` are `flows`, `providers`, `crons`,
`functions`, and `mcp`, each an `Option<AdminPageMode>`. On the builder,
`.builtin_pages(AdminPagesConfig { … })` sets several pages at once and
`.builtin_page(id, mode)` sets one, named by an `AdminPageId`: `Flows`, `Providers`,
`Crons`, `Functions`, or `Mcp`.

`Hidden` is navigation-only and must not be used as an access-control boundary.
Use `Disabled` when shop staff must not be able to open a page or call its
admin API.

### An admin made of the pages of the application

`AdminPagesConfig::none()` disables every built-in page: the merchant
reaches the pages the application declares, and nothing of the flows, the
runs, the logs, the providers, the crons, the functions, or MCP.

```rust,ignore
use openshop::AdminPagesConfig;
use openshop::prelude::*;

pub static APP: App = app! {
    flows: [flows::sync_orders::sync_orders],
    admin_pages: [HomePage, SettingsPage],
    builtin_pages: AdminPagesConfig::none(),
};
```

The built-in Home is a summary of the built-in pages. With all of them
disabled, the admin opens on a page of the application: one of
`admin_pages` is declared at `/`, or the configuration is rejected:

```text
[openshop] Invalid config: builtinPages disables every built-in page of the admin: declare its home, a page at `/` (`#[admin_page("/")]`)
```

An application served without an admin UI (`admin_ui: UiSource::None`)
needs no home.

A page is opened again by naming it, for example the form where a merchant
fills the credentials of a provider:

```rust,ignore
builtin_pages: AdminPagesConfig {
    providers: Some(AdminPageMode::Visible),
    ..AdminPagesConfig::none()
},
```

What the merchant no longer sees stays available to the operator of the
application: the [operator dashboard](operator-dashboard.md) shows the runs
and the logs of every shop, and
[the records of the framework](database.md#read-the-framework-tables) give
them to the code of the application, for a page of its own.

`McpConfig::disabled()` turns MCP off: `POST /mcp` is refused, and the MCP page
of the admin is disabled with it, along with `/api/mcp/*`.

```rust,ignore
use openshop::prelude::*;

pub static APP: App = app! {
    flows: [flows::sync_orders::sync_orders],
    mcp: McpConfig::disabled(),
};
```

The dashboard and the protocol can also be set apart:

- Set only the page mode to `Disabled` to block MCP administration while
  existing MCP tokens can continue using `POST /mcp`.
- Set `McpConfig::disabled()` and a page mode, such as
  `mcp: Some(AdminPageMode::Visible)` in `builtin_pages`, to stop the protocol while
  leaving the dashboard available for token administration.

## Runtime validation

`APP.config()` returns an error instead of a configuration when a definition
is invalid, and so does every command of `APP.run()`; on the builder, that is
`.build()`. Messages have the form
`[openshop] Invalid config: <path> <problem>`, for example
`[openshop] Invalid config: crons[0].flow references unknown flow "syncOrder"`.
The checks are:

- a key must be registered once, and a method once for a proxy or server route
  path, as described under [Builder methods](#builder-methods);
- cron entries must reference registered flows, carry a valid schedule, and
  give an `input` that the input of their flow accepts;
- provider and flow names must be non-empty;
- provider and function fields must have a non-empty label, and `select`
  fields non-empty, uniquely valued options;
- Shopify Function handles must be non-empty and unique, and `modes` is only
  accepted on discount functions;
- worker and retry numbers and durations must be positive;
- flow timeouts and step timeouts must be positive when set;
- Shopify app handles, secrets, keys, TOML paths, and URL overrides must be
  non-empty and well formed;
- proxy and server route paths must be unambiguous, their parameter names may
  only contain letters, numbers, and `_`, and every server route must declare
  at least one HTTP method;
- custom admin page paths must be valid, unreserved, and unambiguous, and the
  entries of the admin menu, declared with `nav` or set with `admin_menu`,
  must point to registered static pages other than the home;
- an application that disables every built-in admin page must declare a
  page at `/`;
- MCP permissions, tools, and resources must be well formed. See
  [MCP](mcp.md).

## Cron shop targeting

Cron entries support these shop modes:

| `CronShops` | Set with | Behavior |
| --- | --- | --- |
| `Global` | None of the methods below | Run once without a shop context. The run uses the shop `__global__` and the app handle `default`. |
| `All` | `.all_shops()` | Run once per installation that is not uninstalled, for every `(app handle, shop)` pair. The shop of the `client_credentials_shop` setting is also targeted for each app that has no row for it, once Shopify gives that app a token for it with the [client credentials grant](authentication.md#client-credentials-grant); a row marked uninstalled keeps it out. |
| `Shop(shop)` | `.shop("shop.myshopify.com")` | Run only for one shop. |
| `Shops(shops)` | `.shops(["a.myshopify.com", "b.myshopify.com"])` | Run for selected shops. |

A listed shop runs once per app it is installed for. A listed shop without an
installation still runs, under the app handle `default`. A cron disabled for a
shop through the admin UI or MCP is skipped for that shop.

## Worker defaults

The `worker` setting takes a `WorkerConfig`. Missing fields use these
runtime defaults:

| Field | Type | Default | Purpose |
| --- | --- | --- | --- |
| `concurrency` | `usize`, positive | `5` | Maximum active runs in one worker process. |
| `poll_interval` | `Duration`, positive | 1 second | Initial polling interval of an empty queue, and interval between two sweeps of the runs past their deadline. |
| `poll_max_interval` | `Duration`, positive | 5 seconds | Maximum empty-queue backoff. |
| `poll_backoff_coefficient` | `f64`, positive | `1.5` | Multiplier applied after consecutive empty polls. |
| `lease_duration` | `Duration`, positive | 30 seconds | Claim lease and graceful-stop deadline. Active runs refresh their lease every 15 seconds, or twice per lease period when that is shorter. The lease is dated by the clock of the database. A run still executing when the stop deadline passes is handed back to the queue. |

Each field has a builder method of the same name on `WorkerConfig`:
`WorkerConfig::new().concurrency(10).lease_duration(Duration::from_secs(60))`.
The `worker` command overrides `concurrency` only when `--concurrency=N` is
given; other worker settings come from the configuration. `dev` uses the
configuration as is.

A worker only polls while the queue is empty. With runs waiting, it claims as
many as it has free slots in one statement, and claims again as soon as a run
finishes: the poll interval does not limit throughput. Run multiple worker
processes to scale horizontally; PostgreSQL claims work with
`FOR UPDATE SKIP LOCKED`.

## Retry defaults

The app-level `retry_policy` setting supplies defaults for the flows of the
application. Flow and dispatch settings override it field by field. See the
[retry defaults and precedence](flows.md#retry-precedence-and-defaults)
for the authoritative option table and deadline behavior.

## Environment variables

See [Environment variables](environment-variables.md) for required
values, defaults, `.env` loading, and resolution order. `dev` loads the
project-root file; migration and production commands use the process
environment.
