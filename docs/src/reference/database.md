# Database and migrations

To change a schema, see [Manage migrations](../guides/manage-migrations.md). For tenant boundaries, read [Shop and app isolation](../concepts/shop-isolation.md).

OpenShop uses PostgreSQL through [`sqlx`](https://docs.rs/sqlx).

## Connection

The CLI commands connect to `DATABASE_URL` and fall back to
`postgresql://openshop:openshop@localhost:5432/openshop` when it is unset.
`OpenShop::connect` and `openshop::db::connect` require the variable and fail
with `[openshop] DATABASE_URL not set` when it is absent.

| Environment variable | Default |
| --- | --- |
| `PGPOOL_MAX` | `10` |
| `PGPOOL_IDLE_TIMEOUT_MS` | `30000` |
| `PGPOOL_CONNECTION_TIMEOUT_MS` | `5000` |

Values that are not non-negative integers fall back to the default. Each
process creates one pool and shares it through the `OpenShop` handle.

| Function | Purpose |
| --- | --- |
| `openshop::db::connect()` | Pool for `DATABASE_URL`. |
| `openshop::db::connect_to(url)` | Pool for an explicit URL. |
| `openshop::db::pool_options()` | `PgPoolOptions` with the `PGPOOL_*` settings applied. |

## App models

```rust,ignore
use openshop::{Model, define_model};

pub fn reviews() -> Model {
    define_model("reviews")
        .column("title", "text NOT NULL")
        .column("rating", "integer NOT NULL")
        .index("reviews_shop_rating_idx", &["shop", "rating"])
        .unique_index("reviews_shop_title_unique", &["shop", "title"])
}
```

`define_model(table)` always adds a UUID `id` primary key with the default
`gen_random_uuid()`.

| Generated column | Omit with | Definition |
| --- | --- | --- |
| `shop` | `.without_shop()` | `text NOT NULL` |
| `created_at` | `.without_created_at()` | `timestamp with time zone DEFAULT now() NOT NULL` |
| `updated_at` | `.without_updated_at()` | `timestamp with time zone DEFAULT now() NOT NULL` |

| Method | Purpose |
| --- | --- |
| `.column(name, definition)` | Adds a column with its SQL definition, such as `"text NOT NULL"`. |
| `.index(name, &[columns])` | Adds `CREATE INDEX IF NOT EXISTS`. |
| `.unique_index(name, &[columns])` | Adds `CREATE UNIQUE INDEX IF NOT EXISTS`. |
| `.create_sql()` | Returns the idempotent `CREATE TABLE IF NOT EXISTS` and index statements. |

`updated_at` has a creation default; OpenShop does not automatically change it
on updates to app-owned rows.

A model registered with `.model(…)` on the configuration builder is created by
`dev` and by the test harness. They only create what is missing: a changed
column definition is not applied to an existing table. Production databases
get the table from a migration: `migrate generate` writes the `create_sql()`
of every registered model whose table no migration creates yet. Later changes
to the table are migrations you write by hand.

## Queries and transactions

Flows, admin functions, and MCP tools receive `ctx.db`; other handlers use
`ctx.app.db()`. Both are the shared `sqlx::PgPool`.

```rust,ignore
use openshop::{PgPool, Result, sqlx};

async fn list_and_add(db: &PgPool, shop: &str) -> Result<Vec<(String, i32)>> {
    let rows: Vec<(String, i32)> = sqlx::query_as("SELECT title, rating FROM reviews WHERE shop = $1")
        .bind(shop)
        .fetch_all(db)
        .await?;

    let mut tx = db.begin().await?;
    sqlx::query("INSERT INTO reviews (shop, title, rating) VALUES ($1, $2, $3)")
        .bind(shop)
        .bind("Fast")
        .bind(5)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    Ok(rows)
}
```

`openshop::sqlx` re-exports the `sqlx` crate with its PostgreSQL, `chrono`,
`uuid`, and JSON support. To use `#[derive(sqlx::FromRow)]`, add `sqlx` to your
own dependencies.

Transactions are normal `sqlx` transactions. Do not perform slow network calls
inside them: they retain a PostgreSQL connection and may keep locks open.

## Framework tables

The framework schema consists of nine tables. Every table has a UUID `id`
primary key. Rows are scoped by `app_handle` (default `'default'`) and `shop`
unless noted.

| Table | Purpose | Other columns | Unique indexes |
| --- | --- | --- | --- |
| `installations` | Installed shops and their access tokens | `access_token`, `scopes`, `nonce`, `installed_at`, `uninstalled_at` | `(app_handle, shop)` |
| `flow_runs` | Queue and history of flow runs | `flow_name`, `status`, `input`, `error`, `deadline_at`, `parent_run_id`, `attempts`, `available_at`, `worker_id`, `retry_policy`, `started_at`, `completed_at`, `created_at` | — |
| `step_results` | Checkpoints; scoped by `flow_run_id` | `step_name`, `attempt`, `status`, `output`, `error`, `duration_ms`, `created_at` | `(flow_run_id, step_name, attempt)` |
| `provider_configs` | Saved provider configuration | `provider_name`, `config`, `last_checked_at`, `last_check_ok`, `updated_at` | `(app_handle, shop, provider_name)` |
| `cron_overrides` | Per-shop cron enabled state | `cron_key`, `enabled`, `updated_at` | `(app_handle, shop, cron_key)` |
| `logs` | Flow log lines; scoped by `flow_run_id` | `level`, `message`, `payload`, `created_at` | — |
| `mcp_tokens` | MCP token hashes and metadata | `name`, `token_id`, `token_hash`, `token_fingerprint`, `status`, `expires_at`, `revoked_at`, `last_used_at`, `created_at`, `updated_at` | `(token_id)` |
| `mcp_permission_grants` | Permissions granted to MCP tokens | `token_id`, `permission_key`, `granted`, `created_at`, `updated_at` | `(app_handle, shop, token_id, permission_key)` |
| `mcp_audit_logs` | MCP tool and resource calls | `token_id`, `capability_type`, `capability_name`, `permission_keys`, `target_shop`, `status`, `error`, `duration_ms`, `request_id`, `created_at` | — |

JSON columns (`input`, `retry_policy`, `output`, `config`, `payload`,
`permission_keys`) have the type `json`. Timestamps are
`timestamp with time zone`. `step_results.flow_run_id` and `logs.flow_run_id`
hold the run's UUID as `text`; there are no foreign keys.

The row types are exported from `openshop::db::schema` (`Installation`,
`FlowRun`, `StepResult`, `ProviderConfigRow`, `CronOverride`, `LogRow`,
`McpToken`, `McpAuditLog`) and implement `sqlx::FromRow` and `Serialize`.

Migration history lives in a tenth table, `__openshop_migrations`, with the
columns `tag` (primary key), `checksum`, and `applied_at`.

## Client-owned migrations

Projects own their migrations as SQL files in `./migrations`. The migration
generated by `openshop init` contains the framework tables for installations,
flow runs, steps, logs, providers, crons, and MCP.

```bash
cargo run -- migrate generate
cargo run -- migrate check
cargo run -- migrate
```

- `migrate generate` writes the framework schema your project does not cover
  yet, and the tables of registered models that no migration creates, in
  development or CI. With `--name=<name>` and nothing else to write, it adds an
  empty migration for your own changes.
- `migrate check` fails when the committed migrations do not cover the
  framework schema, or when a registered model has no migration creating its
  table.
- `migrate` applies committed SQL only.

`migrate` does not run generation. `start` and `worker` never generate or
apply migrations. Commit generated SQL and apply it before starting production
processes.

How migrations are applied:

- Files ending in `.sql` are applied in file-name order. Hidden files are
  ignored. Generated files are named `<YYYYMMDDHHMMSS>_<name>.sql`.
- Each file runs in its own transaction and is recorded in
  `__openshop_migrations` under its file name without `.sql`, together with the
  SHA-256 checksum of its content.
- Concurrent `migrate` processes are serialized with a PostgreSQL advisory
  lock, so several replicas can run it during a deploy.
- An applied file whose content changed is rejected:
  `[openshop] Migration "<tag>" was modified after it was applied. Restore it and add a new migration instead.`
- Without any SQL file, `migrate` fails with
  `[openshop] No client migrations found in ./migrations. …`.

A generated framework migration starts with the marker line
`-- openshop:framework <id>`. `migrate generate` and `migrate check` use that
line to know which parts of the framework schema your project covers: keep it
when you edit the file.

A model counts as covered when any migration file contains a `CREATE TABLE`
statement for its table name, generated or hand-written. Uncovered models are
written together into one file, named after `--name` or
`<YYYYMMDDHHMMSS>_models.sql`. `migrate check` reports them with
``[openshop] No migration creates the table of these models: <tables>. …``.
Column changes of a model whose table already has a migration are not
detected.

`migrate generate` refuses to run in production mode unless
`--allow-generate-in-production` is given.

`dev` and the test harness do not use migrations: they execute the framework
schema and the registered models directly, with `IF NOT EXISTS` statements.
That push is refused in production mode with
``[openshop] Refusing to push the schema in production. Run `openshop migrate` before starting OpenShop.``

The same operations are available as functions in `openshop::db::migrate`:
`migrate_schema`, `get_client_migration_status`, `generate_client_migrations`,
`check_client_migrations`, and `push_schema`.
