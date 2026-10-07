# Architecture

Use this reference before moving behavior across OpenShop boundaries.

## Repo Map

- `crates/openshop/src/engine`: flow execution, steps, scheduling, retries, aborts, and worker behavior.
- `crates/openshop/src/server`: axum server, Shopify auth, webhooks, app proxy, Admin API routes (`server/api`), functions, MCP (`server/mcp`), crypto, logs, and shop isolation.
- `crates/openshop/src/cli`: commands of an application binary: `dev`, `start`, `worker`, and `migrate`.
- `crates/openshop/src/db`: connection pool, framework row types, models (`#[derive(Model)]`, `define_model`), typed rows (`record.rs`), the schema snapshot and its comparison (`diff.rs`), and migrations.
- `crates/openshop/src/admin`: server side of custom admin pages (access rules, loaders, actions).
- `crates/openshop/src/config/validate.rs`: validation of an application definition.
- `crates/openshop/src/runtime`: environment (`RuntimeEnv`), logging, and the outbound HTTP transport.
- `crates/openshop/src/shopify`: Shopify Admin GraphQL client.
- `crates/openshop/src/testing`: test harness, compiled with the `testing` feature.
- `crates/openshop/src/types.rs`: the definitions an application registers (providers, flows, crons, webhooks, proxies, routes, functions, MCP).
- `crates/openshop/src/declare.rs`: `App`, the value `app!` declares, and `Declared`, the trait every attribute implements for its block.
- `crates/openshop/src/app.rs`: `OpenShopConfig`, `OpenShopBuilder` (the lower level `app!` expands to), and the `OpenShop` handle.
- `crates/openshop-macros`: `app!`, the attributes and derives of the building blocks (`#[flow]`, `#[webhook]`, `#[proxy]`, `#[route]`, `#[admin_page]`, `#[mcp_tool]`, `#[mcp_resource]`, `#[provider]`, `ProviderConfig`, `FunctionConfig`, `Model`), and `graphql!`. Their documentation is at the bottom of `crates/openshop/src/lib.rs`.
- `crates/openshop/migrations`: framework schema SQL, embedded in the crate.
- `crates/openshop/ui-dist`: prebuilt admin UI bundle, committed and embedded in the crate.
- `crates/openshop-shared`: types and rules shared by the server and the WebAssembly UI.
- `crates/openshop-ui`: embedded admin UI written with Leptos and Shopify's Polaris web components, compiled to WebAssembly; its own workspace. Its library lets an application build a bundle with custom admin pages.
- `crates/openshop-cli`: the `openshop` command (`init`, `dev`); `templates/minimal` is the generated app template.
- `apps/demo`: runnable demo app and integration surface; update it when examples or framework usage change. `apps/demo/ui` is its admin UI bundle with a custom page, also its own workspace.
- `docs/src`: user-facing documentation (mdBook).

## Boundary Rules

- Keep reusable framework behavior in `crates/openshop/src`; keep sample usage in `apps/demo` and the template.
- Do not duplicate framework behavior in the template or the demo app when it can live in OpenShop.
- If a public API changes, update docs, the template, and demo usage in the same change.
- Prefer existing config validation in `crates/openshop/src/config/validate.rs` for definition checks. It runs in `OpenShopBuilder::build()` and again when an `OpenShop` is created. The builder itself refuses a key registered twice (`flows.syncOrders is registered twice`), a method handled twice on a proxy or route path, and two response types on a proxy path.
- Preserve shop/app isolation by `(app_handle, shop)` in server and database behavior.
- Keep secrets in environment-backed config; never put API secrets in TOML examples.
- In framework code, read environment-backed settings from `app.env()` (`RuntimeEnv`). It is captured once at startup so that requests never read process-global state.
- Template files are embedded with `include_str!` in `crates/openshop-cli/src/init.rs`. Add a new template file to its `TEMPLATE` list.
- Framework schema changes are SQL files in `crates/openshop/migrations`, listed in `FRAMEWORK_MIGRATIONS` (`crates/openshop/src/db/migrate.rs`). Keep them idempotent: `dev` and the test harness execute them on every start.

## Shared App Code

- Follow the layout of `apps/demo/src`: one module per block under `flows/`, `providers/`, `webhooks/`, `proxy/`, `routes/`, `admin/`, `functions/`, and `models/`, each declaring its block with an attribute on the function or the struct itself, all listed in the single `app!` declaration of `src/lib.rs` (`pub static APP: App = app! { … };`). `src/main.rs` only runs it: `APP.run().await`.
- No function builds a block or the configuration. Write `#[flow("syncOrders")] pub async fn sync_orders(ctx: FlowContext, input: SyncOrdersInput) -> Result<()>`, not `fn sync_orders() -> Flow`; write `app! { flows: [sync_orders] }`, not `fn config()` with `OpenShopConfig::builder()` and `openshop::cli::run(config)`.
- Nothing is discovered from the file system, and an attribute alone registers nothing. A block exists only once it is listed in `app!`, under the list of its kind: `providers`, `flows`, `functions`, `models`, `admin_pages`, `webhooks`, `proxies`, `routes`, `mcp_tools`, `mcp_resources`. A block listed under another kind, or a function without its attribute, does not compile.
- Every other entry of `app!` is a setting, `name: value`, passed to the `OpenShopBuilder` method of that name: `crons`, `on_error`, `mcp`, `custom_pages`, `shopify`, `worker`, `retry_policy`, `pages`, `experimental`, `admin_ui`, `max_request_body_bytes`.
- A function attribute leaves the function an ordinary function and adds a type of the same name, which is what the lists take and what carries the constants (`sync_orders::NAME`, `orders_create::TOPICS`, `reviews::PATH` and `reviews::METHOD`, `status::NAME`, `guide::URI`). Do not re-export a declared function next to a module of the same name (`pub use sync_orders::sync_orders;` beside `mod sync_orders;` does not compile); import it where the application is declared. Refer to a block by its constant instead of repeating its name: `CronEntry::new("0 2 * * *", sync_orders::NAME)`, `CustomAdminNavigationItem::new("Reviews", ReviewsPage::PATH)`.
- Declare each kind of block in its form:
  - Flow: `#[flow("name", timeout = …, step_timeout = …, concurrency = …, retry_policy = …)]` on `async fn(ctx: FlowContext) -> Result<()>`, with the input as an optional second argument (a type that implements `Deserialize` and `JsonSchema`). Without a name, the flow is named after the function in camel case.
  - Webhook: `#[webhook("orders/create", …)]` on `async fn(ctx: WebhookContext) -> Result<()>`, with the payload as an optional second argument (a `Deserialize` type).
  - App proxy: `#[proxy(GET, "/reviews", liquid)]` on `async fn(ctx: ProxyContext) -> Result<T>` where `T` serializes (a `String` is the body of a `liquid` or `html` response), one function per method of a path; the response type is `json` (the default), `liquid` or `html`, the same for every method of the path.
  - Server route: `#[route(POST, "/orders/:id", auth = partner)]` on `async fn(ctx: ServerRouteContext) -> Result<T>` where `T` is a response (`openshop::response::Response`, `(StatusCode, String)`…), one function per method of a path, each with its own authentication; public without `auth`.
  - Admin page: `#[admin_page("/reviews")]` on the `impl` block of a type, with `#[access]`, `#[loader]` and `#[action]` on its functions (`authorize = Self::rule` for a rule of their own, an optional typed input as second argument). Unmarked functions are helpers.
  - MCP: `#[mcp_tool("name", permissions(…), risk = low)]` and `#[mcp_resource("scheme://…", name = "…")]` on `async fn(ctx: McpExecutionContext) -> Result<T>`; the description is the documentation comment. Custom permissions stay data: `mcp: McpConfig::new().permission(key, McpPermissionDefinition::new(…))`.
  - Shopify Function: `#[derive(FunctionConfig)]` with `#[function("key", discount = "handle", …)]` on the struct whose fields, with `#[field(label = "…")]`, are the configuration form of an instance.
- The builder remains the lower level, for tests and for blocks built at run time: `OpenShopConfig::builder()` with `.add::<Block>()` for a declared block, `.flow(Flow::new(..))`, `.webhook(topic, Webhook::new(..))`, `.proxy(path, ProxyDefinition::json().get(..))`, `.route(path, ServerRoute::public().get(..))`, `.function(key, FunctionDefinition::new(..))`, `.admin_page(AdminPage::new(..))`, `.mcp_tool(name, ..)`, `.mcp_resource(uri, ..)`, `.model(..)`, then `.build()`. `APP.builder()?` is the builder of a declaration, to add to, and `APP.config()?` its validated configuration.
- `apps/demo` is the reference for the declared forms. The generated template (`crates/openshop-cli/templates/minimal`) is written with the builder.
- Keep proxy and server route handlers as thin adapters for auth checks, params, body parsing, and response shaping.
- Put reusable business logic and Admin GraphQL operations in ordinary modules whose functions take what they need (`&ShopifyClient`, `&PgPool`, `&Connectors`), so flows, webhooks, and routes can share them.
- Outside a flow, get the shop's client and connectors from the application handle: `ctx.app.shopify_client(&ctx.shop, &ctx.shopify_app)` and `ctx.app.connectors(&ctx.shop, &ctx.shopify_app)`.

## Flow And Provider Rules

- Step results must implement `Serialize` and `DeserializeOwned` and round-trip through JSON, because completed step output is persisted and reused on retry.
- Propagate the result of `ctx.step.run(..)` and `ctx.step.sleep(..)` with `?`. Sleeping and cancellation travel as errors (`SleepSignal`, `FlowCanceledError`) that the runner must receive.
- Give every step a name that is stable across deployments and unique within a run.
- A canceled or timed-out step is dropped at its next `.await`. Use `ctx.signal` (a `CancellationToken`) in work that does not yield or that you spawn.
- Keep provider and Function fields validated with `validate = <validator>` in `#[field(…)]`: one of `openshop::validators`, or a validator of your own.
- Describe an application table with a `#[derive(Model)]` struct listed in the `models` of `app!`, and read and write it through `T::for_shop(&ctx.db, &ctx.shop)`: every operation is then limited to the shop. Use `all_shops` only for maintenance work, and filter on the app handle yourself when several apps share a table. After changing a model, run `cargo run -- migrate generate`, review the SQL and its warnings, declare renames with `#[model(renamed_from = "…")]`, and commit the migration with `migrations/schema.snapshot.json`. Link two models with `#[model(references(Other), on_delete = "…")]` on the id field and index that field; read the other side with a filter, or with a join written in SQL (`sqlx::query!` checks it at compile time when the project depends on `sqlx` with the `macros` feature and a database is reachable while compiling).
- Write a provider as a struct: `#[derive(ProviderConfig)]` with a `#[field(label = "…")]` on each field for the configuration form, and `#[provider("name")]` on its `impl` block, where every `pub async fn(&self, …) -> Result<…>` is a method of the provider and `self` is the configuration of the shop. Mark the connection check with `#[checker]`, keep helpers private, list the struct in the `providers` of `app!`, and call methods with `Struct::on(&ctx.connectors).method(&args)`: the compiler checks the method, its arguments and its result. In tests, `ctx.fakes.for_call(Struct::METHOD)` is the fake of a method. `Provider::new(..)` with `handle`, `typed_method` and `ctx.connectors.call(..)` / `call_as::<T>(..)` remain for providers and calls only known at run time.

## UI Rules

- Match the existing compact admin UI instead of adding marketing-style layouts.
- Use Shopify's Polaris web components (`s-page`, `s-section`, `s-table`, …) where the surrounding UI already uses them.
- Keep Admin API calls routed through the fetch helpers in `crates/openshop-ui/src/fetch.rs`, which attach the session token.
- Put types and rules needed by both the server and the UI in `crates/openshop-shared`.
- After changing `crates/openshop-ui`, rebuild with `trunk build --release` and commit `crates/openshop/ui-dist`. The server embeds that directory, so rebuild the server afterwards.
- Keep the `shopify-api-key` meta tag and the App Bridge script tag of `crates/openshop-ui/index.html` as they are: the server rewrites them when it serves the shell.

## Customer Account Extensions

- Customer Account UI extensions should call OpenShop proxy handlers through `/ext/*` with `Authorization: Bearer <session token>`.
- Extension-backed proxy handlers should trust `ctx.shop`, `ctx.shopify_app`, and `ctx.customer_id`; do not trust customer IDs supplied by query strings or request bodies.
- Use `ctx.app.shopify_client(&ctx.shop, &ctx.shopify_app)` inside extension-backed proxy handlers so multi-app installs resolve the correct access token.
- For extension frontend code, keep backend origin handling explicit: Shopify CLI tunnel origin in dev, deployed app origin or extension settings in production.
