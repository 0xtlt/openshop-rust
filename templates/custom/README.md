# OpenShop custom app

A Shopify app built with [OpenShop](https://github.com/0xtlt/openshop-rust)
for the merchant who runs it. It uses every building block:

| Block | Where |
| --- | --- |
| A provider, the warehouse the orders are pushed to | `src/providers` |
| Two flows, run by hand and by crons | `src/flows` |
| A Shopify Function | `src/functions` |
| Webhooks | `src/webhooks` |
| App proxy routes, one rendered with Liquid | `src/proxy` |
| A public route | `src/routes` |
| MCP capabilities | `src/mcp.rs` |
| A model and its table | `src/models` |
| A custom admin page, next to the built-in pages | `src/admin` |

`src/lib.rs` lists them in `app!`. Remove what the app does not need from
there and from `src/`.

## Develop

```bash
docker compose up -d --wait   # PostgreSQL
cp .env.example .env          # then fill in the Shopify credentials
openshop dev                  # or: cargo run -- dev
```

`dev` pushes the database schema, builds the admin UI bundle from the pages
in `src/admin`, then serves the API, the admin UI, a worker and the cron
scheduler from one process on `http://localhost:3000`. It rebuilds the bundle
when a page changes. Run it through `shopify app dev` to open the app inside a
development store.

The views of the pages are compiled to WebAssembly with trunk. Install the
toolchain once:

```bash
rustup target add wasm32-unknown-unknown
cargo install trunk
```

## Test

```bash
docker compose up -d --wait   # most tests need PostgreSQL
cargo test
```

## Deploy

```bash
openshop build                    # the admin UI bundle into dist/, then the app
cargo run --release -- migrate    # apply ./migrations
OPENSHOP_UI_DIR=dist cargo run --release -- start    # web process: API, admin UI, scheduler
cargo run --release -- worker     # one or more workers
```

The server serves the bundle of the directory named by `OPENSHOP_UI_DIR`:
deploy `dist` next to the binary. The `Dockerfile` does it: it builds the
bundle with the `openshop` CLI, copies it into the image and sets
`OPENSHOP_UI_DIR=/app/dist`.

Set `OPS_PASSWORD` to serve the operator dashboard at `/ops`: the runs and
the installations of every shop, read-only, outside Shopify admin.

Set `SENTRY_DSN` to report failures to Sentry: the runs that fail, the
handlers that fail and the panics, each with its flow and its shop.

Set `OPENSHOP_ENV=production` and `ENCRYPTION_KEY` in production: without a
key, `start` and `worker` do not start (`dev` stores secrets in plaintext).
Generate one with `openssl rand -hex 32`. Schema
changes live in `./migrations`: run `cargo run -- migrate generate` after
upgrading OpenShop, or `cargo run -- migrate generate --name=<name>` to add
your own tables.
