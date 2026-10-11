---
name: openshop-best-practices
description: Use when working on OpenShop (Rust) framework or app code, including Rust APIs, Shopify GraphQL, flows, webhooks, providers, proxy routes, server routes, CLI commands, database code, tests, templates, documentation, or the demo app. Apply OpenShop repo conventions, keep Shopify GraphQL responses behind serde types, and choose the right verification commands.
---

# OpenShop Best Practices

Use this skill to keep OpenShop changes aligned with the framework's local patterns.

## Start Here

Before editing:
- Inspect nearby source, tests, docs, and templates before choosing an implementation.
- Prefer existing helpers and boundaries in `crates/openshop/src` over new abstractions.
- Keep public APIs stable unless the user explicitly asks for a breaking change.
- Check whether a framework change also requires updates to `crates/openshop-cli/templates/minimal`, the demos (`apps/demo-custom`, `apps/demo-public`), or `docs/src`.

## Reference Routing

- Read `references/graphql.md` before writing or changing Shopify GraphQL operations, `graphql!`, `ctx.shopify.graphql()` or `graphql_as()` calls, response types, or tests that mock Shopify.
- Read `references/architecture.md` before changing engine, server, CLI, database, config, templates, providers, flows, webhooks, proxy routes, or UI boundaries.
- Read `references/testing.md` before adding tests, changing test setup, or choosing validation commands.

## Core Rules

- Use `cargo` for workspace commands. `crates/openshop-ui` is a separate workspace built with `trunk`.
- Declare building blocks where they are written, with an attribute on the function or the struct itself, and list them in the `app!` declaration of the application: `#[flow]`, `#[webhook]`, `#[proxy]`, `#[route]`, `#[mcp_tool]` and `#[mcp_resource]` on `async` functions, `#[admin_page(..)]` on an `impl` block, `#[derive(ProviderConfig)]` with `#[provider]`, `#[derive(FunctionConfig)]` with `#[function(..)]`, and `#[derive(Model)]` with `#[model("table")]` on structs. Do not write functions that build a block or the configuration. Keep the builder (`OpenShopConfig::builder()`, `.add::<Block>()`, `Flow::new(..)`, `AdminPage::new(..)`…) for tests and for blocks only known at run time. See `references/architecture.md`.
- Import application code with `use openshop::prelude::*;` and list explicitly only what the prelude lacks (`openshop::testing::TestContext`, `serde::Deserialize`, `std::time::Duration`). Run the application with `fn main() -> Result<()> { APP.main() }`. Do not add `schemars` or `tokio` to an application.
- Mark the input of a flow, of an MCP tool and of an admin loader or action with `#[input]`, above its `#[derive(..)]`. Its fields are camel case outside Rust. The payload of a webhook is not an input: derive `Deserialize` on it.
- Write a step with an async closure, `ctx.step.run("name", async || { … })`, never `|| async { … }`. Log with the message first: `ctx.logger.info("…")`, `ctx.logger.info_with("…", json!({ … }))`, and the same for `warn` and `error`.
- Use `ctx.shopify`, `ctx.connectors` and `ctx.db` in every context that belongs to a shop: a flow, a webhook, a proxy, an admin loader or action, an MCP tool or resource, and what `ctx.for_shop("…")` returns in a route. Pass the context itself to what needs a shop: `sync_orders::dispatch(&ctx, SyncOrdersInput { .. })`, `Review::of(&ctx)`.
- Treat hand-indexed `serde_json::Value` as a last resort where a shape is known; prefer `graphql!` for Shopify operations, serde types, `graphql_as::<T>()`, `Struct::on(&ctx.connectors).method(&args)` for a provider, and a typed second argument for the input of a flow, of an admin loader or action and of an MCP tool, for the payload of a webhook, and for the query or the body of a proxy or a route.
- Keep Liquid in a `.liquid` file next to its handler and return `liquid!("file.liquid").assign("name", value)`. Never write Liquid in a Rust string, and never paste a value into a template with `format!`.
- Test an application with `#[openshop::test(APP)]` on `async fn(ctx: &TestContext)`, and fake a provider method with `ctx.fakes.for_call(Warehouse::PUSH)`. See `references/testing.md`.
- Return errors with `?` and `openshop::Result`; do not `unwrap()` or `expect()` values that come from Shopify, providers, requests, or the database. Engine errors are typed and recovered with `downcast_ref`.
- Do not add `unsafe`: the framework crate denies `unsafe_code` and the CLI forbids it.
- The TypeScript implementation is the reference for behavior. When the Rust version deliberately differs, document it in `docs/src/reference/differences.md`.
- Database rows and encrypted values must stay readable by both runtimes.
- Keep `openshop-shared` free of server-only dependencies: it compiles to WebAssembly.
- Keep tests close to the behavior: a `#[cfg(test)] mod tests` next to pure helpers and integration tests under `crates/openshop/tests` for runtime contracts.

## Verification

For Rust code changes, run:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

`cargo test --workspace` needs PostgreSQL. For narrow changes, also run the most relevant test target described in `references/testing.md`.
