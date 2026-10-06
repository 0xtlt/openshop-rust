# Changelog

## Unreleased

- Typed provider calls: a `ProviderCall<A, R>` constant shared by the provider
  (`Provider::handle`) and its callers (`Connectors::invoke`), with the saved
  configuration deserialized into a struct. `Provider::typed_checker` does the
  same for the connection check, and `FakeProviders::for_call` returns the fake
  of a call.
- `OpenShopBuilder::providers`, `flows` and `with`.
- Typed GraphQL: `graphql!("…", name = value)` checks a Shopify Admin
  operation against the schema at compile time and types its variables and its
  result; `ShopifyClient::execute` runs it. The module form,
  `graphql! { mod name = "…"; }`, names the generated types and input objects.
  The Admin schema of the default API version is bundled; `openshop codegen`
  downloads another one.

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
