# Database and migrations

To change a schema, see [Manage migrations](../guides/manage-migrations.md). For tenant boundaries, read [Shop and app isolation](../concepts/shop-isolation.md).

OpenShop uses PostgreSQL through [`sqlx`](https://docs.rs/sqlx).

## Connection

The CLI commands connect to `DATABASE_URL` and fall back to
`postgresql://openshop:openshop@localhost:5432/openshop` when it is unset.
`OpenShop::connect` and `openshop::db::connect` require the variable and fail
with `[openshop] DATABASE_URL not set` when it is absent.

| Environment variable | Default | `0` |
| --- | --- | --- |
| `PGPOOL_MAX` | `10` | The default size |
| `PGPOOL_IDLE_TIMEOUT_MS` | `30000` | No idle timeout |
| `PGPOOL_CONNECTION_TIMEOUT_MS` | `5000` | No connection timeout |

Values that are not non-negative integers fall back to the default, and `0`
means what it means to node-postgres. Each process creates one pool and shares
it through the `OpenShop` handle.

`PGPOOL_CONNECTION_TIMEOUT_MS` covers the wait for a free connection and the
opening of a new one. While PostgreSQL refuses connections, the pool retries at
growing intervals until the timeout, then fails with `pool timed out while
waiting for an open connection`. Without a timeout it keeps retrying, at
intervals that double each time: after an outage, a waiting statement can take
as long as the outage lasted to notice that PostgreSQL is back.

| Function | Purpose |
| --- | --- |
| `openshop::db::connect()` | Pool for `DATABASE_URL`. |
| `openshop::db::connect_to(url)` | Pool for an explicit URL. |
| `openshop::db::pool_options()` | `PgPoolOptions` with the `PGPOOL_*` settings and the [idle connection check](#idle-connection-check) applied. |

### Idle connection check

PostgreSQL can close a connection while it waits in the pool: a restart, a
failover, the idle timeout of a proxy. The pool finds out when it uses the
connection again.

- A connection that waited **more than one second** is pinged first, and
  replaced when the ping fails. The statement runs on another connection and
  no error reaches the application.
- A connection reused **within the second** is handed out as is. If the server
  closed it in the meantime, the statement run on it fails, and the pool drops
  the connection when it comes back: one failed statement per closed
  connection, `PGPOOL_MAX` at most per process.

`sqlx` pings every connection it hands out by default, a second network round
trip for each statement run on the pool. OpenShop replaces that with the check
above, which costs nothing while connections are busy; the delay is not
configurable. A pool built from `pool_options()` can set its own check with
`test_before_acquire` or `before_acquire`.

A connection that stays unused is closed after `PGPOOL_IDLE_TIMEOUT_MS`, unless
it is `0`. The pool looks for such connections once per timeout, so one can
wait up to twice that long.

## App models

A struct with `#[derive(Model)]` is the single description of an application
table. Its fields are the columns: they define the table that `dev` creates
and that `migrate generate` writes migrations for, and they type the rows you
read and write.

```rust,ignore
use openshop::chrono::{DateTime, Utc};
use openshop::prelude::*;

#[derive(Debug, Clone, Model)]
#[model("reviews", index(shop, product_id))]
pub struct Review {
    pub id: Uuid,
    pub shop: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub product_id: String,
    pub rating: i32,
    pub note: Option<String>,
    #[model(default = "false")]
    pub approved: bool,
}
```

List it in the `models` of `app!`: `models: [Review]`.

The derive implements the trait `Model`: `use openshop::Model;` imports the
derive and the trait, and so does the prelude. `Review::table()` returns the
`Table` the struct describes, and `Review::TABLE` is its name.

Four fields are conventional. They are recognized by name and their
definition is fixed:

| Field | Column | Required |
| --- | --- | --- |
| `id: Uuid` | `uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL` | Yes |
| `shop: String` | `text NOT NULL` | No. Without it the table is not scoped by shop. |
| `created_at: DateTime<Utc>` | `timestamp with time zone DEFAULT now() NOT NULL` | No |
| `updated_at: DateTime<Utc>` | `timestamp with time zone DEFAULT now() NOT NULL` | No |

Every other field is a column named after it, `NOT NULL` unless its type is an
`Option`:

| Rust | SQL |
| --- | --- |
| `String` | `text` |
| `bool` | `boolean` |
| `i16`, `i32`, `i64` | `smallint`, `integer`, `bigint` |
| `f32`, `f64` | `real`, `double precision` |
| `Uuid` | `uuid` |
| `DateTime<Utc>` | `timestamp with time zone` |
| `serde_json::Value` | `jsonb` |
| `Vec<String>`, `Vec<i32>`, `Vec<i64>`, `Vec<Uuid>` | `text[]`, `integer[]`, `bigint[]`, `uuid[]` |
| `Option<T>` | The type of `T`, nullable |

Another type is a compile error, unless you implement
`openshop::db::ColumnType` for it; `sqlx` must be able to encode and decode it.

Options are written in `#[model(…)]`. On the struct, the table comes first:
`#[model("reviews", index(shop, product_id))]`.

| On the struct | |
| --- | --- |
| `"reviews"` | The table. Required, and the first argument. |
| `index(shop, product_id)` | An index on those fields, named `<table>_<columns>_idx`. |
| `unique(shop, title)` | A unique index, named `<table>_<columns>_unique`. |
| `index(…, name = "…")`, `unique(…, name = "…")` | The same with a name of your choice. Required when the generated name exceeds 63 characters. |
| `renamed_from = "ratings"` | The previous name of the table. See [Renames](#renames). |

| On a field | |
| --- | --- |
| `default = "false"` | The default of the column, as a SQL expression. |
| `sql_type = "varchar(64)"` | The SQL type, instead of the one of the Rust type. |
| `column = "val"` | The name of the column, instead of the name of the field. |
| `index`, `unique` | An index on that column alone. |
| `references(Review)`, `on_delete = "cascade"` | A foreign key. See [References](#references). |
| `renamed_from = "stars"` | The previous name of the column. See [Renames](#renames). |

An index names fields, so a misspelled one is a compile error.

### Typed rows

The derive gives the struct an entry point that returns its rows:

| Struct | Entry point | Rows |
| --- | --- | --- |
| With a `shop` field | `Review::of(&ctx)` | Those of the shop of a context. |
| | `Review::for_shop(&db, &shop)` | Those of one shop, with the database and the shop given. |
| | `Review::all_shops(&db)` | Those of every shop, for maintenance work. |
| Without | `Setting::rows(&db)` | All of them. |

`of` takes any context that belongs to a shop: `FlowContext`,
`WebhookContext`, `ProxyContext`, `AdminContext`, `McpContext`, and the
`RouteShopContext` that `ctx.for_shop(…)` returns in a server route. These
implement the trait `ShopScope`. `Review::of(&ctx)` is
`Review::for_shop(&ctx.db, &ctx.shop)`.

```rust,ignore
use openshop::prelude::*;

async fn moderate(ctx: &FlowContext) -> Result<()> {
    let reviews = Review::of(ctx);

    let review = reviews
        .insert(NewReview {
            product_id: "gid://shopify/Product/1".to_string(),
            rating: 5,
            note: None,
            approved: None,
        })
        .await?;

    let best = reviews
        .filter(Review::RATING.gte(4))
        .filter(Review::PRODUCT_ID.eq("gid://shopify/Product/1"))
        .order_by(Review::CREATED_AT.desc())
        .limit(10)
        .all()
        .await?;

    let mut review = reviews.find(review.id).await?.expect("just inserted");
    review.approved = true;
    reviews.save(&review).await?;

    reviews.filter(Review::RATING.lt(2)).delete().await?;
    println!("{} good reviews", best.len());
    Ok(())
}
```

| Operation | Returns | |
| --- | --- | --- |
| `insert(new)` | The stored row | Takes `New<Struct>`: the fields without `id`, `created_at`, `updated_at` and `shop`. A field with a `default` is an `Option`, and `None` leaves the default. |
| `find(id)` | `Option<Row>` | |
| `all()`, `first()`, `count()` | `Vec<Row>`, `Option<Row>`, `i64` | |
| `save(&row)` | `Option<Row>` | Writes every field except `id`, `shop` and `created_at`, and sets `updated_at` to now. `None` when the row does not exist. |
| `delete(id)` | `bool` | Whether the row existed. |
| `filter(condition)`, `order_by(order)`, `limit(n)`, `offset(n)` | A query | Ends with `all()`, `first()`, `count()` or `delete()`. |

The rows of a shop are limited to that shop in every operation: `find`, `save`
and `delete` do not reach a row of another shop even by its id, and `insert`
sets the shop. `all_shops` reads and deletes across shops and refuses to
insert. A model without `shop` has no such limit: prefer keeping the field.

Each field has a column constant, `Review::RATING`, typed like the field.
Conditions and orders are built from it:

| | |
| --- | --- |
| `eq`, `ne`, `gt`, `gte`, `lt`, `lte` | Compare with a value of the column's type. A nullable column compares with the value it holds. |
| `is_in([…])` | One of the values. No row matches an empty list. |
| `is_null()`, `is_not_null()` | |
| `asc()`, `desc()` | An order for `order_by`. |

Conditions are combined with `AND`. A comparison with a value of the wrong
type, or with a column of another model, does not compile.

For what this does not cover, such as joins, `OR`, aggregates and
transactions, use `sqlx` on the same pool, as described under
[Queries and transactions](#queries-and-transactions). `Review` implements
`sqlx::FromRow`, so `sqlx::query_as::<_, Review>("SELECT …")` returns the same
struct.

### References

A field can hold the key of a row of another model:

```rust,ignore
use openshop::prelude::*;

#[derive(Debug, Clone, Model)]
#[model("review_votes", index(review_id))]
pub struct ReviewVote {
    pub id: Uuid,
    pub shop: String,
    #[model(references(Review), on_delete = "cascade")]
    pub review_id: Uuid,
    pub value: i32,
}
```

`references(Review)` adds a foreign key from `review_id` to the `id` of
`reviews`. PostgreSQL then refuses a vote whose review does not exist, and
applies `on_delete` to the votes of a review that is deleted.

| Option | |
| --- | --- |
| `references(Review)` | The `id` of another model, or of the model itself. List that model too. |
| `references(Review, column = "code")` | Another column of that model. It must be unique. |
| `references("legacy_orders")` | A table by its name, when no struct describes it. |
| `on_delete = "…"` | `cascade`, `restrict`, `set null`, `set default` or `no action`, which is what PostgreSQL does when the option is absent. `set null` needs an `Option` field. |

The constraint is named `<table>_<column>_fkey`. Models can be listed in
any order: `dev` and the generated migrations create every table first, then
the constraints.

PostgreSQL does not index the referencing column. Add `index(review_id)` when
you filter on it or when deletions cascade through it.

A reference is a fact of the schema, enforced by the database. The typed rows
do not follow it: read the other side with a filter,

```rust,ignore
let votes = ReviewVote::of(&ctx)
    .filter(ReviewVote::REVIEW_ID.eq(review.id))
    .all()
    .await?;
```

and write joins in SQL, [checked at compile time](#checked-sql) if you want.

### Tables without a struct

A `Table` describes a table without typed rows. It is what
`#[derive(Model)]` builds from a struct, and can be built directly for a table
that is only known at run time:

```rust,ignore
use openshop::Table;

pub fn reviews() -> Table {
    Table::conventional("reviews")
        .column("title", "text NOT NULL")
        .column("rating", "integer NOT NULL")
        .index("reviews_shop_rating_idx", &["shop", "rating"])
        .unique_index("reviews_shop_title_unique", &["shop", "title"])
}
```

`Table::conventional(table)` starts with the conventional columns: `id`,
`shop`, `created_at` and `updated_at`. `.without_shop()`,
`.without_created_at()` and `.without_updated_at()` omit them.
`Table::new(table)` starts without any column. `.column(name, definition)`
takes the SQL definition of the column, and `.create_sql()` returns the
idempotent `CREATE TABLE IF NOT EXISTS` and index statements. Register it with
`.model(…)` on the builder, for example `APP.builder()?.model(reviews())`; a
table registered twice is refused when the configuration is built.
Migrations are generated for it like for a struct: the type,
nullability and default of each definition are read; a change to another
constraint written in a definition, such as `CHECK`, is left for you to
write. Declare a foreign key with
`.with_foreign_key(ForeignKeyDef::new("review_id", "reviews").on_delete("cascade"))`
rather than in the definition.

`updated_at` has a creation default. `save` updates it; an `UPDATE` written
with `sqlx` does not, unless it sets the column.

### In development

`dev` and the test harness create the tables of the registered models
directly. On a table that already exists, they add the columns, indexes and
foreign keys the model gained since. They do not rename, retype or drop
anything: a change they cannot apply is logged, and needs a migration or a
fresh development database.

## Queries and transactions

Every context has `ctx.db`, the shared `sqlx::PgPool`: the contexts of flows,
webhooks, proxy routes, server routes, admin functions, and MCP tools. Code
without a context reads the same pool with `app.db()`.

```rust,ignore
use openshop::prelude::*;
use openshop::{PgPool, sqlx};

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
`uuid`, and JSON support. A `#[derive(Model)]` struct already implements
`sqlx::FromRow`; to derive it on another type, add `sqlx` to your own
dependencies.

These queries are strings: nothing checks them against the tables when the
project compiles. The [typed rows](#typed-rows) of a model are checked, and so
is the SQL below.

### Checked SQL

`sqlx::query!` and `sqlx::query_as!` check a SQL string against the database
while the project compiles, and type its parameters and its result. They cover
what the typed rows do not, such as a join between two models:

```rust,ignore
let rows = sqlx::query!(
    "SELECT r.product_id, r.rating, COALESCE(SUM(v.value), 0) AS score
     FROM reviews r
     LEFT JOIN review_votes v ON v.review_id = r.id
     WHERE r.shop = $1
     GROUP BY r.id",
    ctx.shop
)
.fetch_all(&ctx.db)
.await?;

for row in rows {
    // `product_id: String`, `rating: i32`, `score: Option<i64>`
    println!("{} {} {:?}", row.product_id, row.rating, row.score);
}

// The struct of a model, from free SQL.
let best: Vec<Review> = sqlx::query_as!(Review, "SELECT * FROM reviews WHERE shop = $1 AND rating >= $2", ctx.shop, 4)
    .fetch_all(&ctx.db)
    .await?;
```

A column that does not exist, a parameter of the wrong type and a result read
as the wrong type are compile errors.

These are macros of `sqlx` itself, with its requirements:

- **`sqlx` as your own dependency**, in the version OpenShop uses, with the
  `macros` feature. The macros do not work through the `openshop::sqlx`
  re-export.

  ```toml
  sqlx = { version = "0.8", default-features = false, features = ["macros", "postgres", "runtime-tokio", "uuid", "chrono", "json"] }
  ```

- **A database while compiling.** The macros connect to `DATABASE_URL`, read
  from the environment or from `.env`, and the tables must exist there. A
  query on a table that is not created yet does not compile: add the model,
  start `dev` once or apply its migration, then write the query.
- **Or prepared queries**, for builds without a database such as CI and
  Docker: `cargo sqlx prepare`, from `sqlx-cli`, records the checks in a
  `.sqlx` directory to commit. See the
  [`sqlx` documentation](https://docs.rs/sqlx/latest/sqlx/macro.query.html#offline-mode).
  OpenShop does not manage that directory.

The shop filter of these queries is yours to write.

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

Workers read `flow_runs` through two partial indexes limited to `pending`,
`sleeping` and `running` rows: `flow_runs_claimable_idx` on
`(available_at, created_at)` to claim runs, and `flow_runs_deadline_idx` on
`deadline_at` to fail the runs past their deadline. A claim costs the same
whatever the number of runs waiting.

The row types are exported from `openshop::db::schema` (`Installation`,
`FlowRun`, `StepResult`, `ProviderConfigRow`, `CronOverride`, `LogRow`,
`McpToken`, `McpAuditLog`) and implement `sqlx::FromRow` and `Serialize`.

Migration history lives in a tenth table, `__openshop_migrations`, with the
columns `tag` (primary key), `checksum`, and `applied_at`.

## Client-owned migrations

Projects own their migrations as SQL files in `./migrations`. The migrations
generated by `openshop init` contain the framework tables for installations,
flow runs, steps, logs, providers, crons, and MCP, and the indexes workers
read the run queue through.

```bash
cargo run -- migrate generate
cargo run -- migrate check
cargo run -- migrate
```

- `migrate generate` writes the framework schema your project does not cover
  yet, and the statements that bring your tables in line with the registered
  models, in development or CI. With `--name=<name>` and nothing else to
  write, it adds an empty migration for your own changes.
- `migrate check` fails when the committed migrations do not cover the
  framework schema, or when the models changed since the last generated
  migration.
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

### Generated model migrations

`migrate generate` compares the registered models with
`migrations/schema.snapshot.json`, which records them as of the last
generation, writes the difference as one migration, named after `--name` or
`<YYYYMMDDHHMMSS>_models.sql`, and updates the snapshot. It does not connect to
the database. Commit the snapshot with the migrations.

| Change to a model | Generated |
| --- | --- |
| New model | `CREATE TABLE IF NOT EXISTS` and its indexes |
| Model no longer registered | `DROP TABLE IF EXISTS`, with a warning |
| New field | `ALTER TABLE … ADD COLUMN IF NOT EXISTS`, with a warning when it is `NOT NULL` without default |
| Removed field | `ALTER TABLE … DROP COLUMN IF EXISTS`, with a warning |
| Other type | `ALTER COLUMN … TYPE … USING …::…`, with a warning |
| `Option` added or removed | `DROP NOT NULL`, or `SET NOT NULL` with a warning |
| Other default | `SET DEFAULT` or `DROP DEFAULT` |
| New, removed or changed index | `CREATE INDEX IF NOT EXISTS`, `DROP INDEX IF EXISTS`; a unique index comes with a warning |
| New, removed or changed `references` | `ADD CONSTRAINT … FOREIGN KEY`, with a warning on a table that already exists; `DROP CONSTRAINT IF EXISTS` |
| `renamed_from` | `RENAME TO`, `RENAME COLUMN … TO …`, and `RENAME CONSTRAINT` for the foreign keys it concerns |

A statement that can lose data or fail on existing rows is preceded by a
`-- WARNING:` comment in the file, and `migrate generate` prints it. The file
is yours to edit before it is applied: a backfill, a different cast, a
statement in another order. Data is never moved for you.

`migrate check` fails while a change has no migration:

```text
[openshop] The models changed since the last generated migration:
  - ALTER TABLE "reviews" ADD COLUMN IF NOT EXISTS "note" text
Run `openshop migrate generate`, review and commit the generated SQL, then run `openshop migrate`.
```

### Renames

A comparison cannot tell a renamed column from a column dropped and another
added. Say it with `renamed_from`, on the field or on the struct:

```rust,ignore
#[derive(Model)]
#[model("product_reviews", renamed_from = "reviews")]
pub struct Review {
    pub id: Uuid,
    #[model(renamed_from = "stars")]
    pub rating: i32,
}
```

The next generation writes `ALTER TABLE "reviews" RENAME TO "product_reviews"`
and `RENAME COLUMN "stars" TO "rating"`. Once the snapshot has the new names
the attribute does nothing, and can be removed.

### Projects without a snapshot

A project whose migrations predate the snapshot has none. Its first
`migrate generate` takes the tables that a migration already creates as the
models describe them today, records them, and writes nothing for them. Run it
before changing a model. A table created by a hand-written migration is taken
the same way, whatever the casing or quoting of its `CREATE TABLE`.

Once the snapshot exists, it is the only reference: a model it does not record
gets a `CREATE TABLE IF NOT EXISTS`, whatever the migration files contain.

`migrate generate` refuses to run in production mode unless
`--allow-generate-in-production` is given.

`dev` and the test harness do not use migrations: they execute the framework
schema and the registered models directly, with `IF NOT EXISTS` statements, as
described under [In development](#in-development).
That push is refused in production mode with
``[openshop] Refusing to push the schema in production. Run `openshop migrate` before starting OpenShop.``

The same operations are available as functions in `openshop::db::migrate`:
`migrate_schema`, `get_client_migration_status`, `generate_migrations`,
`check_migrations`, and `push_schema`. The comparison itself is
`openshop::db::diff`.
