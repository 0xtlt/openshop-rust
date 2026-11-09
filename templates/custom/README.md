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

`dev` pushes the database schema, then serves the API, the admin, a worker
and the cron scheduler from one process on `http://localhost:3000`. The pages
of `src/admin` are rendered by the server: `openshop dev` restarts the app
when one changes, and the page reloads. Run it through `shopify app dev` to
open the app inside a development store.

## Test

```bash
docker compose up -d --wait   # most tests need PostgreSQL
cargo test
```

## Deploy

```bash
cargo build --release
cargo run --release -- migrate    # apply ./migrations
cargo run --release -- start      # web process: API, admin, scheduler
cargo run --release -- worker     # one or more workers
```

The admin is part of the binary: the `Dockerfile` builds the app, and there is
nothing else to ship.

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
