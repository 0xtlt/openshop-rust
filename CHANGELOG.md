# Changelog

## Unreleased

- A custom admin page is written in one place. The view of a page is the
  function marked `#[view]` in its `#[admin_page]` block, next to the loaders
  and the actions it calls, and no longer lives in a separate `ui/` crate. It
  calls them through a constant the block gains per function,
  `use_loader(Self::LIST_REVIEWS, || ())`, which carries the type the
  function takes and the type it returns: the names and the types are no
  longer written a second time. Views need the `ui` feature of `openshop`,
  which re-exports `openshop-ui` as `openshop::ui`.
- The admin UI bundle of an application is built by `openshop` from its
  pages: `openshop dev` and `openshop build` compile to WebAssembly, on their
  own, the modules that declare a page with a view, and write the bundle into
  `dist/`. The application has no file of the bundle to keep, and the rest of
  it is written as before. `openshop build-ui` builds the bundle alone.
- `openshop init-ui` writes an example page, `src/admin/hello.rs`, and adds
  the `ui` feature and `leptos` to the manifest. It no longer takes a
  directory.
- Every context of a shop gives the same three things: `ctx.db`, `ctx.shopify`
  and `ctx.connectors`, in a flow, a webhook, a proxy, an admin function, an
  MCP tool, and on the shop a route resolves with `ctx.for_shop(…)`. Nothing
  is read when a context is built: the access token is read at the first
  Shopify request, the provider configuration at the first provider call.
  `OpenShop::shopify_client` and `OpenShop::connectors` are no longer `async`.
- `#[flow]` adds `dispatch` to the flow: `sync_orders::dispatch(&ctx,
  SyncOrdersInput { limit: 10 })` queues a run for the shop and the Shopify
  app of a context, with a typed input. `app.dispatch_flow(DispatchOptions::new(
  name, shop)…)` remains for a flow known by name, a delay or a retry policy.
- `#[input]` makes a type the input of a flow, of an MCP tool or of an admin
  function: it derives `Deserialize`, `Serialize` and `JsonSchema` without
  `serde` or `schemars` among the dependencies of the application, and names
  fields in camel case outside Rust.
- The logger of a flow takes the message first: `ctx.logger.info(message)`,
  and `info_with(message, json!({ … }))` for data, likewise for `warn` and
  `error`.
- Liquid templates live in files next to their handler: `liquid!(
  "reviews.liquid")` embeds one when the crate compiles, and
  `.assign("name", value)` gives it what only the server knows, written as a
  Liquid literal the value cannot close.
- `#[route(GET, "/path", json)]`: the function returns anything that
  serializes, like a proxy.
- What the authentication function of a route returns for an authenticated
  request keeps its type: `RouteAuthOutcome<Partner>` on one side,
  `ctx: RouteContext<Partner>` and `ctx.auth.key` on the other, and a route
  whose handler and authentication disagree does not compile. Without a type,
  both carry a `Value`, as before.
- An admin page is served as soon as it is listed in `admin_pages`, and
  `#[admin_page("/reviews", nav = "Reviews")]` gives it its entry in the menu
  of the admin. The `admin_menu` setting sets the menu by hand, with
  `AdminMenuItem::new(label, path)`: it replaces `custom_pages`,
  `CustomAdminNavigationItem`, `ExperimentalConfig` and `.experimental(…)`,
  and its errors name `adminMenu[0].path`.
- An MCP permission is a constant, `McpPermission::new("reviews:read", "Read
  reviews")`, and the tools and resources that list it register it.
  `McpConfig::disabled()` turns MCP off, and the MCP page of the admin with
  it unless the application sets a mode for that page.
- The input of a cron is checked against the input of its flow when the
  application is built. A cron runs for no shop, `.all_shops()`, `.shop(…)` or
  `.shops([…])`; `"all"` and `"global"` are no longer read from a string.
- `#[openshop::test(APP)]` makes an `async fn(ctx: &TestContext)` a test of
  the application: started on an isolated database with its shop installed,
  and shut down when the function returns. The argument can also be an
  expression that resolves to a test context, `#[openshop::test(context())]`.
  `TestContext::for_app`, `APP.test()` and `TestOptions::installed()` do the
  same by hand. A test context stands for its shop: `Review::of(ctx)` and
  `sync_orders::dispatch(ctx, input)` take it.
- `use openshop::prelude::*;` imports what the code of an application uses.
- The root of the crate holds what an application names, about half of what
  it exported: the attributes, `app!`, the contexts, the settings and the
  values handlers exchange. The definitions a block is built from by hand
  moved to `openshop::builder` with `OpenShopBuilder` (`Flow`, `Provider`,
  `Field`, `Proxy`, `Route`, `Webhook`, `Function`, `McpTool`, `McpResource`,
  `AdminPage`, `Table`, `InputSchema`…). `Worker`, `Scheduler`, `run_flow`,
  `cancel_run`, `StepExecutor` and `SleepSignal` are in `openshop::engine`,
  `ServerOptions` in `openshop::server`, `RuntimeEnv` in `openshop::runtime`,
  `IntoVariable` in `openshop::shopify` and `Declared` in `openshop::declare`.
- Durations are `Duration`s: `RetryPolicy::initial_interval` and
  `max_interval`, `WorkerConfig::poll_interval`, `poll_max_interval` and
  `lease_duration`, `DispatchOptions::delay`. In `#[flow]`, `timeout` and
  `step_timeout` are written as text checked when the crate compiles:
  `timeout = "10m"`, `"30s"`, `"1h30m"`.
- The setting of the built-in pages of the admin is `builtin_pages`, with
  `.builtin_pages(…)` and `.builtin_page(id, mode)` on the builder, next to
  `admin_pages`, the pages of the application.
- Names. A definition is named after what it defines: `Field`, `Fields`,
  `Function`, `FunctionType`, `Proxy`, `Route`, `McpTool`, `McpResource`,
  `McpPermission`, `Cron`. A context is named after its attribute:
  `RouteContext`, `RouteAuthContext`, `AdminContext`, `McpContext`, whose
  `app_handle` is `shopify_app` like everywhere else. `RetryPolicy` and
  `WorkerConfig` are what an application sets. `RunOutcome` and `QueuedRun`
  are what running and dispatching a flow return, and `DispatchOptions` what
  `dispatch_flow` takes. Constructors are associated functions:
  `AdminFunction::loader`, `action`, `typed_loader` and `typed_action`,
  `Table::conventional`, `Factory::new`, `FakeProviders::new`,
  `ShopifyClient::new`; `AdminPage::access` and `AdminFunction::authorize`
  take the function itself. The process logger is `openshop::logger()` and
  `set_logger`, the server is `openshop::server::router` and `listen`, and the
  builders of MCP capabilities say `permissions` and `risk` like the
  attributes.
- Typed provider calls: a `ProviderCall<A, R>` constant names a method of a
  provider (`Provider::handle`), with the saved configuration deserialized
  into a struct. `Provider::typed_checker` does the same for the connection
  check.
- Providers as structs: `#[derive(ProviderConfig)]` makes the fields of a
  struct the configuration form, and `#[provider]` on its `impl` block makes
  its public `async` methods the methods of the provider, with the saved
  configuration as `self`. The provider is named after the struct
  (`Warehouse` is `warehouse`), or by `#[provider("name")]`, and the block
  carries `Warehouse::NAME`. Flows call the methods with
  `Warehouse::on(&ctx.connectors).push(&orders)`.
- Fields of a provider or of a Shopify Function state what a value must
  satisfy in `#[field(…)]`: `url`, `non_empty`, `range(min = …, max = …)` and
  `length(min = …, max = …)`, before any `validate = …`. A field without a
  `label` is labeled after its name: `batch_size` is "Batch size".
  `Field::validate` adds a validator after the ones already set.
- In a test, `ctx.fakes.for_call(Warehouse::PUSH)` is a typed fake: `returns`
  takes what the method returns, and its calls give the arguments as the
  method receives them.
- `OpenShopBuilder::providers`, `flows` and `with`.
- Typed GraphQL: `graphql!("…", name = value)` checks a Shopify Admin
  operation against the schema at compile time and types its variables and its
  result; `ShopifyClient::execute` runs it. The module form,
  `graphql! { mod name = "…"; }`, names the generated types and input objects.
  The Admin schema of the default API version is bundled; `openshop codegen`
  downloads another one.
- `#[derive(Model)]` with `#[model("reviews")]`: a struct as the single
  description of an application table, with typed rows for insertion, lookup,
  filters, ordering, update and deletion, limited to a shop:
  `Review::of(&ctx)` for the shop a handler runs for, `Review::for_shop(&db,
  &shop)` without a context. The derive implements the trait `Model`;
  `Review::table()` is the `Table` the struct describes.
- `migrate generate` compares the models with `migrations/schema.snapshot.json`
  and writes what changed: created and dropped tables, added, dropped, renamed
  and altered columns, indexes. Statements that can lose data or fail come with
  a warning. `migrate check` fails while a model change has no migration.
- `#[model(references(Other), on_delete = "cascade")]` declares a foreign key.
  Tables are created before their constraints, in any registration order, and
  generated migrations add, drop and rename the constraints.
- `dev` adds the columns, indexes and foreign keys a model gained to an
  existing table.
- Blocks declared with attributes: a block is the function or the struct
  itself, and no function builds it. `#[flow]`, `#[webhook]`, `#[proxy]`,
  `#[route]`, `#[mcp_tool]` and `#[mcp_resource]` go on `async` functions,
  which stay ordinary functions; the input of a flow, the payload of a
  webhook, the arguments of an MCP tool, and the query or the JSON body of a
  proxy or route request are an optional, typed second argument. A proxy or a
  route answers `422` to a request that does not fit its type.
  `#[admin_page("/path")]` goes on an `impl` block whose functions are marked
  `#[access]`, `#[loader]` or `#[action]`. `#[derive(FunctionConfig)]` with
  `#[function(…)]` makes a struct the configuration form of a Shopify
  Function. Each block carries its constants: `sync_orders::NAME`,
  `orders_create::TOPICS`, `reviews::PATH`, `ReviewsPage::PATH`,
  `VolumeDiscount::NAME`.
- `app!` declares an application as a value, an `App`: it lists the blocks by
  name (`flows: [sync_orders]`, `models: [Review]`…) and takes the settings of
  the builder (`crons: […]`, `on_error: report`…), which can fail with `?`
  (`shopify: shopify_apps()?`). `APP.main()` runs the command given on the
  command line from a plain `fn main`, so an application does not depend on
  `tokio`; `APP.run()` does it on a runtime of your own. `APP.config()`
  returns the validated configuration and `APP.builder()` the builder to add
  to.
- `OpenShopBuilder::add::<Block>()` adds a declared block of any kind.
  `model_of` is removed: use `.add::<Review>()`, or `models: [Review]` in
  `app!`.
- `build()` refuses a key registered twice, with
  `[openshop] Invalid config: flows.syncOrders is registered twice`: providers,
  flows, functions, webhooks, models, and MCP tools, resources and permissions.
- The methods of a proxy or route path can be registered separately:
  `.proxy(path, …)` and `.route(path, …)` called again for a path add methods
  to it. A method registered twice is refused, as are two response types on a
  proxy path. Each method of a route keeps its own authentication.
- `.mcp(…)` adds to the MCP permissions, tools and resources already
  registered instead of replacing them.
- A project created by `openshop init` is written with the declared forms
  (`app!`, `#[flow]`, `#[input]`, `#[provider]`, `graphql!`) and depends on
  `openshop` and `serde` only. It starts with MCP off: `POST /mcp` is refused
  and the admin has no MCP page, until `mcp: McpConfig::disabled()` is removed
  from `src/main.rs`.
- `OpenShopBuilder::crons`, `mcp_tool` and `mcp_resource`.
- Responses under 1 KB are no longer compressed: on small JSON responses the
  compression took more CPU than the rest of the request and did not make them
  smaller.
- The pool no longer pings a connection before every statement, which cost
  each one a second round trip to PostgreSQL. Only a connection that waited
  more than a second in the pool is pinged, and replaced when the ping fails.
  If PostgreSQL closes a connection used less than a second ago, the next
  statement on it fails once and the pool drops the connection. The test
  harness uses the same check.
- `0` in a `PGPOOL_*` variable means what it means to node-postgres:
  `PGPOOL_IDLE_TIMEOUT_MS=0` and `PGPOOL_CONNECTION_TIMEOUT_MS=0` disable the
  timeout, and `PGPOOL_MAX=0` is the default size. Before, the first closed
  every connection as soon as it was released, the second kept the pool from
  connecting, and the third stopped the process with a panic.
- A worker with runs waiting is no longer held back by its poll interval or by
  the length of the queue. A run that finishes wakes the worker, which used to
  wait a full poll interval while its slots were taken: 5 runs per second
  with the default settings, whatever the flow did. It claims as many runs as
  it has free slots in one statement, through a partial index, so a claim
  costs the same with a few runs waiting or with 80,000.
- Runs past their deadline are failed once per poll interval, also while
  every slot of the worker is taken, instead of before each claim. A claim
  skips them, so none starts late.
- The framework schema has a second migration, `openshop_queue_indexes`: the
  two partial indexes on `flow_runs` that workers read the queue through.
  `migrate generate` adds it to an existing project, and `migrate check` fails
  until it is there. `dev` creates the indexes on start.
- A run writes its log lines in fewer statements. A line waits up to 50 ms for
  the next ones and they are inserted together, each with the time it was
  logged; `flush()` and the end of the flow do not wait. The lines a flow
  logged are now stored before the outcome of its run. A flow of three steps
  goes from 21 statements to 15: a busy worker completes about a third more
  runs per second and uses a fifth less CPU per run.
- An open connection no longer holds its own copy of the route table, which
  axum made for every connection it accepted. With the routes of the demo, a
  connection goes from 62 KB to 19 KB: 43 MB less with 1,000 connections open.

## 0.0.12-beta.0

Initial Rust implementation, at feature parity with OpenShop (TypeScript)
`0.0.12-beta.0`.

- Flow engine: dispatch with per-shop concurrency control, checkpointed steps,
  durable sleeps, retries with exponential backoff, deadlines, leased workers
  and a cron scheduler.
- Server: Shopify OAuth and session tokens, multi-app support, the Admin API,
  webhooks, app proxy and extension routes, public server routes, Shopify
  Function instances, custom admin page loaders and actions, and the MCP endpoint
  with scoped tokens and audit logs.
- Admin UI compiled to WebAssembly and embedded in the binary.
- SQL migrations owned by the application, with the framework schema generated
  into `./migrations`.
- `openshop` CLI (`init`, `init-ui`, `dev`) and a testing harness with isolated
  databases, fake providers and signed requests.
