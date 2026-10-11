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

The demo has a custom admin page with a view, so it has an admin UI bundle of
its own, compiled from its `admin` module. Build it into `apps/demo/dist`,
which is not committed:

```bash
mise run demo:ui           # `openshop build-ui` in apps/demo
```

## Running the demo

```bash
cp .env.example apps/demo/.env     # add SHOPIFY_API_KEY and SHOPIFY_API_SECRET
cargo run -p openshop-demo -- dev
```

The admin UI is an embedded Shopify app: it only starts inside the Shopify
admin iframe. To exercise the backend without a store, call the API with an
HS256 session token signed with `SHOPIFY_API_SECRET` (`aud` = `SHOPIFY_API_KEY`,
`iss` / `dest` = `https://<shop>.myshopify.com`), as the test harness does.

To look at the application without a store, run the preview. It serves the
demo on `http://127.0.0.1:3999`, in a throwaway schema of the database above,
and only accepts loopback connections. It answers as in production: without a
shop, the admin is the page that says where the app opens from, and the
[operator dashboard](docs/src/reference/operator-dashboard.md) is at `/ops`
(password `preview`):

```bash
cargo run -p openshop-demo --example preview
```

To look at the admin UI itself, set `PREVIEW_AS_SHOP=1`: the preview then puts
a stub in place of Shopify App Bridge, with a session for a fixed test shop:

```bash
PREVIEW_AS_SHOP=1 cargo run -p openshop-demo --example preview
```

Set `PREVIEW_UI_DIR=apps/demo/dist` to preview the demo's own bundle after
building it with `mise run demo:ui`. The stub does not
reproduce what Shopify admin renders itself (navigation menu, title bar,
modals), so check those in a development store.

## Conventions

- The TypeScript implementation is the reference for behavior. When the Rust
  version deliberately differs, document it in `docs/src/reference/differences.md`.
- Database rows and encrypted values must stay readable by both runtimes.
- Keep `openshop-shared` free of server-only dependencies: it compiles to WebAssembly.
