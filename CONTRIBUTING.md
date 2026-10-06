# Contributing

## Setup

- Rust 1.88 or newer (`rustup`), plus the `wasm32-unknown-unknown` target and
  [trunk](https://trunkrs.dev) if you work on the admin UI.
- PostgreSQL 17. `docker compose up -d` starts one on `localhost:5432` with the
  `openshop` role and database.

```bash
export DATABASE_URL=postgresql://openshop:openshop@localhost:5432/openshop
cargo test --workspace
```

Integration tests create one PostgreSQL schema per test and drop it afterwards,
so they run in parallel against a single database. They never touch the
`public` schema.

## Checks

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The admin UI is its own workspace:

```bash
cd crates/openshop-ui
cargo fmt --check
cargo clippy --target wasm32-unknown-unknown -- -D warnings
cargo test
trunk build --release      # writes crates/openshop/ui-dist
```

`crates/openshop/ui-dist` is committed so that applications depending on the
framework get the admin UI without installing trunk. Rebuild and commit it when
you change `crates/openshop-ui`.

## Running the demo

```bash
cp .env.example apps/demo/.env     # add SHOPIFY_API_KEY and SHOPIFY_API_SECRET
cargo run -p openshop-demo -- dev
```

The admin UI is an embedded Shopify app: it only starts inside the Shopify
admin iframe. To exercise the backend without a store, call the API with an
HS256 session token signed with `SHOPIFY_API_SECRET` (`aud` = `SHOPIFY_API_KEY`,
`iss` / `dest` = `https://<shop>.myshopify.com`), as the test harness does.

## Conventions

- The TypeScript implementation is the reference for behavior. When the Rust
  version deliberately differs, document it in `docs/src/reference/differences.md`.
- Database rows and encrypted values must stay readable by both runtimes.
- Keep `openshop-shared` free of server-only dependencies: it compiles to WebAssembly.
