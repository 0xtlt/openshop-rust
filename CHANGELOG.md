# Changelog

## Unreleased

- A development store of the organization that owns the app gets its Admin
  API token with Shopify's client credentials grant, with no OAuth. A launch
  from Shopify admin whose signed query names a shop, or an Admin API request
  whose verified session token does, asks Shopify for a token when the app has
  none for that shop, before redirecting to `/auth` or exchanging the session
  token as before; a development store with an empty database then opens in the
  admin, where Shopify refuses to frame `/auth`. `SHOPIFY_CLIENT_CREDENTIALS_SHOP`
  names the one shop that flows, crons, routes and the MCP
  `openshop.admin.graphql` tool may ask a token for, without ever bringing back
  a shop marked uninstalled; a cron of every shop targets it even without an
  installation row. Webhooks never ask. The token is stored encrypted in
  `installations` like the others, with no schema change. A token this process
  obtained is asked again five minutes before it expires; any token is asked
  again once after a `401`, where the client may ask for its shop, and the
  request sent again once. A refusal is remembered by the process for an hour
  per app and shop.
- A `ShopifyClient` whose `shop` is not `<name>.myshopify.com` or a bare
  `<name>` fails with `[openshop] Invalid shop domain` and sends nothing; it
  used to build the address of the Admin API from the value as it was given.
  A webhook whose `X-Shopify-Shop-Domain` is not a shop domain is answered
  `400` when a handler matches, and its `shop` is lowercased.
- `ReqwestTransport` sends `OutboundRequest.body` as a URL-encoded form when
  the media type of the request's `Content-Type` is
  `application/x-www-form-urlencoded` (`runtime::FORM_CONTENT_TYPE`), whatever
  its parameters. The `HttpTransport` trait is unchanged, but OpenShop now
  sends such a request for the client credentials grant: a custom
  `HttpTransport` must encode it as a form too.
- A caller that is still sending when its request is refused reads the answer.
  A body over the request body limit is answered `413` as soon as the limit is
  crossed, and the connection was closed there: cut off in the middle of its
  upload, the caller saw a connection reset instead of the `413`. The server
  now closes its side first and reads what the caller still sends until it
  closes in turn, for five seconds at most.
- The payload of a log line is stored as the TypeScript runtime stores it:
  compact, with its keys in the order they were given. It went through `jsonb`
  on its way to the `json` column, which spaced it and sorted its keys.
- Sentry is supported natively. With the `sentry` setting of `app!`, which
  takes the DSN the application reads
  (`sentry: std::env::var("SENTRY_DSN").ok()`), `dev`, `start` and `worker`
  report what fails: each failed attempt of a flow run, a warning while it
  will be retried and an error at the last one; the runs a worker fails
  without an attempt; a cron that cannot queue its runs; the webhook, proxy,
  route and admin handlers that return an error; the `500` of the Admin API;
  and the panics, once each, where they happen, without a thread of the
  server waiting on Sentry. An event is named after its flow or its route, as
  the route is declared and never by the path of the request, carries the
  shop as a tag and as its user, and has the log lines of the run as
  breadcrumbs, without their payload. No header, query string, body or flow
  input is sent, and a URL in a message loses its credentials, its query
  string and what looks like a token in its path. The panic of an attempt
  that will be retried is a warning, like its error would be, and a cron that
  fails for many shops is one event. `SentryConfig` names the environment (`production` or `development`
  by default, after `OPENSHOP_ENV`) and the release, and `.configure(…)`
  reaches the other options of the SDK. The SDK is re-exported as
  `openshop::sentry` for what the application captures itself, in the scope
  of the flow or the request. Events are sent with the HTTP client of the
  framework. The integration is the `sentry` Cargo feature, on by default;
  the framework reads no environment variable for it.
- In `dev`, the admin UI reloads by itself: when the app has been started
  again, and when the bundle it is served from disk has been built again. The
  shell is served with a script that listens to `GET /__openshop/reload`, an
  event stream that names the version being served. Neither exists in `start`.
  With `openshop dev`, a change to a page or to the server is on screen
  without a restart or a reload by hand.
- `openshop dev` also watches the crates on disk the project is compiled with,
  its path dependencies and theirs: the app is started again, and its admin UI
  bundle built again, when one of them changes. `openshop dev -p my-app` in a
  workspace watches the crate of `my-app`, which it did not.
- `openshop::cli::shutdown_signal()` is public: it resolves on `SIGINT` or
  `SIGTERM`, for a process that embeds the server and stops as `dev` does.
- An admin can be made of the pages of the application only.
  `builtin_pages: AdminPagesConfig::none()` disables every built-in page
  (flows, runs, logs, providers, crons, functions, MCP) for the merchant; a
  page is opened again by naming it. Such an application declares the home of
  its admin, or its configuration is rejected.
- A custom admin page can be declared at `/`: `#[admin_page("/")]` is the home
  of the admin, in place of the built-in dashboard. It has no menu entry, and
  its access rule is its own.
- The operator dashboard, under `/ops`: what happens in every shop over a
  period, read-only, outside Shopify admin. An overview with a chart of the
  runs and the failures, the runs and the log lines with their filters, the
  flows, the shops and each of them, the crons. It
  is served when the application gives it a password,
  `ops_password: std::env::var("OPS_PASSWORD").ok()`, and answers `404`
  otherwise. Its pages are HTML rendered by the server, without script,
  with charts drawn as SVG; the session is a signed cookie, with no table. Production refuses a password
  shorter than 16 characters. `ops` is a reserved first segment for custom
  admin pages.
- A visitor who opens the application without a shop is told where the app
  opens from, on a page written with Polaris web components: the `401` of
  `start`, the `403` of `dev`, which was plain text, and what the admin UI
  shows without a Shopify session. It shows nothing of the application.
- The framework tables are read with typed columns: `FlowRun::of(&ctx)` for
  the runs of a shop, `FlowRun::all_shops(&db)` and
  `Installation::installed(&db)` across shops, `StepResult::of_run` and
  `LogRow::of_run` for a run. These records are never written, and
  `Installation` does not read the access token. `Record` is the part of
  `Model` that reads: `TABLE` and `COLUMNS` moved to it, and
  `openshop::db::schema::Installation`, the whole row, is now
  `InstallationRow`.
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
