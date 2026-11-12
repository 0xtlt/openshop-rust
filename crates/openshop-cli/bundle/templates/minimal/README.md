# OpenShop minimal app

A Shopify integration built with [OpenShop](https://github.com/0xtlt/openshop-rust).

## Develop

```bash
docker compose up -d --wait   # PostgreSQL
cp .env.example .env          # then fill in the Shopify credentials
openshop dev                  # or: cargo run -- dev
```

`dev` pushes the database schema, then serves the API, the embedded admin UI,
a worker and the cron scheduler from one process on `http://localhost:3000`.
Run it through `shopify app dev` to open the app inside a development store.

## Test

```bash
cargo test                    # unit tests
cargo test -- --ignored       # tests that need PostgreSQL
```

## Deploy

```bash
cargo run --release -- migrate    # apply ./migrations
cargo run --release -- start      # web process: API, admin UI, scheduler
cargo run --release -- worker     # one or more workers
```

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
