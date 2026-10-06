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
- Check whether a framework change also requires updates to `crates/openshop-cli/templates/minimal`, `apps/demo`, or `docs/src`.

## Reference Routing

- Read `references/graphql.md` before writing or changing Shopify GraphQL operations, `ctx.shopify.graphql()` or `graphql_as()` calls, response types, or tests that mock Shopify.
- Read `references/architecture.md` before changing engine, server, CLI, database, config, templates, providers, flows, webhooks, proxy routes, or UI boundaries.
- Read `references/testing.md` before adding tests, changing test setup, or choosing validation commands.

## Core Rules

- Use `cargo` for workspace commands. `crates/openshop-ui` is a separate workspace built with `trunk`.
- Follow the existing builder style at public boundaries: `OpenShopConfig::builder()`, `Flow::new(..).input::<T>()`, `Provider::new(..).typed_method(..)`.
- Treat hand-indexed `serde_json::Value` as a last resort where a shape is known; prefer serde types, `graphql_as::<T>()`, `call_as::<T>()`, and `ctx.input::<T>()`.
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
