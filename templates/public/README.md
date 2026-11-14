# OpenShop public app

A Shopify app built with [OpenShop](https://github.com/0xtlt/openshop-rust),
installed by many shops: a stock watch that tells each merchant which variants
run low.

| Block | Where |
| --- | --- |
| The hourly stock check of every shop | `src/flows` |
| The settings of each shop, in their table | `src/models` |
| The admin of a merchant: a home and a settings page | `src/admin` |
| Their texts, in the language of the merchant's admin: English, French | `locales` |
| The cleanup when a shop uninstalls the app | `src/webhooks` |

A merchant sees the pages of the application and nothing of the flows, the
runs or the logs: those are for whoever runs the application, on the operator
dashboard at `/ops` (set `OPS_PASSWORD`).

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
