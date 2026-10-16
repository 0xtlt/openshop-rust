# OpenShop public app

A Shopify app built with [OpenShop](https://github.com/0xtlt/openshop-rust),
installed by many shops: a stock watch that tells each merchant which variants
run low.

| Block | Where |
| --- | --- |
| The hourly stock check of every shop | `src/flows` |
| The settings of each shop, in their table | `src/models` |
| The admin of a merchant: a home and a settings page | `src/admin` |
| The cleanup when a shop uninstalls the app | `src/webhooks` |

A merchant sees the pages of the application and nothing of the flows, the
runs or the logs: those are for whoever runs the application, on the operator
dashboard at `/ops` (set `OPS_PASSWORD`).

## Develop

```bash
docker compose up -d          # PostgreSQL
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
docker compose up -d          # most tests need PostgreSQL
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

Set `OPENSHOP_ENV=production` and `ENCRYPTION_KEY` in production. Schema
changes live in `./migrations`: run `cargo run -- migrate generate` after
upgrading OpenShop, or `cargo run -- migrate generate --name=<name>` to add
your own tables.
