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

Each demo has admin pages with a view, so it has an admin UI bundle of its
own, compiled from its `admin` module. Build them into `apps/demo-custom/dist`
and `apps/demo-public/dist`, which are not committed:

```bash
mise run demo:ui           # `openshop build-ui` in each demo
```

## The demos

Two applications show the two ways an app is distributed. A change to the
framework is tried on both:

| | `apps/demo-custom` | `apps/demo-public` |
| --- | --- | --- |
| The app | Built for the merchant who runs it | Installed by many shops |
| What a merchant sees | The built-in admin (flows, runs, logs, providers, crons, functions, MCP) and a page of the application | The pages of the application and nothing else: its home and its settings |
| Who reads the runs and the logs | The merchant, in Shopify admin | Whoever runs the app, on the operator dashboard at `/ops` |
| Made of | Every building block | A flow run for every shop each hour, a model, two admin pages, a webhook |
| Preview | `http://localhost:3999` | `http://localhost:3998` |

`apps/preview` is what serves either of them without a store.

## Running a demo

To work on the framework and look at the result in a browser, without a store:

```bash
mise run dev:custom        # or dev:public
```

It needs the PostgreSQL of the tests: `mise run db:up`, or `DATABASE_URL`.
It serves the admin UI of the demo on `http://localhost:3999/`
(`http://localhost:3998/` for the public one), as the staff of a fake shop,
and the operator dashboard at `/ops` (password `preview`). It is the preview
described below, as a shop and with the demo's own bundle, run by
`openshop dev`: the demo is started again when a crate of the workspace
changes, trunk builds its admin UI bundle again, and the page reloads by
itself. The pages of `/ops` run no script: reload them by hand.

To run a demo in a development store, with the same loop:

```bash
cp .env.example apps/demo-custom/.env     # add SHOPIFY_API_KEY and SHOPIFY_API_SECRET
mise run demo:custom                      # `openshop dev` in apps/demo-custom
```

`apps/demo-public` and `mise run demo:public` do the same for the public one.

The admin UI is an embedded Shopify app: it only starts inside the Shopify
admin iframe. To exercise the backend without a store, call the API with an
HS256 session token signed with `SHOPIFY_API_SECRET` (`aud` = `SHOPIFY_API_KEY`,
`iss` / `dest` = `https://<shop>.myshopify.com`), as the test harness does.

To look at an application without a store, run its preview. It serves the
demo on `http://127.0.0.1:3999` (`3998` for the public one), in a throwaway
schema of the database above, and only accepts loopback connections. It
answers as in production: without a shop, the admin is the page that says
where the app opens from, and the
[operator dashboard](docs/src/reference/operator-dashboard.md) is at `/ops`
(password `preview`):

```bash
cargo run -p openshop-demo-custom --example preview-custom    # or openshop-demo-public, preview-public
```

To look at the admin UI itself, set `PREVIEW_AS_SHOP=1`: the preview then puts
a stub in place of Shopify App Bridge, with a session for a fixed test shop:

```bash
PREVIEW_AS_SHOP=1 cargo run -p openshop-demo-custom --example preview-custom
```

Set `PREVIEW_UI_DIR=apps/demo-custom/dist` to preview the demo's own bundle
after building it with `mise run demo:ui`. The public demo needs its own, as
its admin is made of its pages. The stub does not
reproduce what Shopify admin renders itself (navigation menu, title bar,
modals), so check those in a development store.

## Conventions

- The TypeScript implementation is the reference for behavior. When the Rust
  version deliberately differs, document it in `docs/src/reference/differences.md`.
- Database rows and encrypted values must stay readable by both runtimes.
- Keep `openshop-shared` free of server-only dependencies: it compiles to WebAssembly.
