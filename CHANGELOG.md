# Changelog

## Unreleased

- Typed provider calls: a `ProviderCall<A, R>` constant shared by the provider
  (`Provider::handle`) and its callers (`Connectors::invoke`), with the saved
  configuration deserialized into a struct. `Provider::typed_checker` does the
  same for the connection check, and `FakeProviders::for_call` returns the fake
  of a call.
- Providers as structs: `#[derive(ProviderConfig)]` makes the fields of a
  struct the configuration form, and `#[provider("name")]` on its `impl` block
  makes its public `async` methods the methods of the provider, with the
  saved configuration as `self`. Flows call them with
  `Warehouse::on(&ctx.connectors).push(&orders)`.
- `OpenShopBuilder::providers`, `flows` and `with`.
- Typed GraphQL: `graphql!("…", name = value)` checks a Shopify Admin
  operation against the schema at compile time and types its variables and its
  result; `ShopifyClient::execute` runs it. The module form,
  `graphql! { mod name = "…"; }`, names the generated types and input objects.
  The Admin schema of the default API version is bundled; `openshop codegen`
  downloads another one.
- `#[derive(Model)]`: a struct as the single description of an application
  table, with typed rows (`Review::for_shop(&db, &shop)`) for insertion,
  lookup, filters, ordering, update and deletion, limited to a shop.
- `migrate generate` compares the models with `migrations/schema.snapshot.json`
  and writes what changed: created and dropped tables, added, dropped, renamed
  and altered columns, indexes. Statements that can lose data or fail come with
  a warning. `migrate check` fails while a model change has no migration.
- `#[model(references(Other), on_delete = "cascade")]` declares a foreign key.
  Tables are created before their constraints, in any registration order, and
  generated migrations add, drop and rename the constraints.
- `dev` adds the columns, indexes and foreign keys a model gained to an
  existing table.
- Responses under 1 KB are no longer compressed: on small JSON responses the
  compression took more CPU than the rest of the request and did not make them
  smaller.
- A worker with runs waiting is no longer held back by its poll interval or by
  the length of the queue. A run that finishes wakes the worker, which used to
  wait a full `poll_interval_ms` while its slots were taken: 5 runs per second
  with the default settings, whatever the flow did. It claims as many runs as
  it has free slots in one statement, through a partial index, so a claim
  costs the same with a few runs waiting or with 80,000.
- Runs past their deadline are failed once per `poll_interval_ms`, also while
  every slot of the worker is taken, instead of before each claim. A claim
  skips them, so none starts late.
- The framework schema has a second migration, `openshop_queue_indexes`: the
  two partial indexes on `flow_runs` that workers read the queue through.
  `migrate generate` adds it to an existing project, and `migrate check` fails
  until it is there. `dev` creates the indexes on start.

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
