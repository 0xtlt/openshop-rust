# Switch from the TypeScript version

Use this guide to move an app from the TypeScript version of OpenShop to this
one, on the same database. The framework tables, their rows, the installed
shops, the encrypted tokens and provider configurations, the queued runs and
the MCP tokens carry over as they are. The only extra step is the
`migrate generate` you run for any project, and the first migration it writes
takes over the tables Drizzle created.

## 1. Keep the names the database holds

Rows refer to flows, providers and crons by name. Give the Rust app the same
names:

| In the database | Rust | Keep |
| --- | --- | --- |
| `flow_runs.flow_name` | `#[flow("syncOrders")]` | The name of the TypeScript flow. Without a name, the function name in camel case is used. |
| `provider_configs.config` | `#[field(name = "apiKey")]` | The key of each field. Without a name, the field name in camel case is used. |
| `cron_overrides.cron_key` | `Cron::new("*/5 * * * *", …)` | The schedule as it was written, character for character: a cron is known by `flow:schedule`. |

Keep the same `ENCRYPTION_KEY`. Every process needs it, unless
`OPENSHOP_ENV` is `development` or `test`.

## 2. Describe the app tables as models

Write one struct with `#[derive(Model)]` for each `defineModel` table, with the
same table and column names. See the
[model reference](../reference/database.md#app-models).

| Drizzle | Rust field |
| --- | --- |
| `id`, `shop`, `createdAt`, `updatedAt` of `defineModel` | `id: Uuid`, `shop: String`, `created_at: DateTime<Utc>`, `updated_at: DateTime<Utc>` |
| `text`, `integer`, `boolean`, `uuid` | `String`, `i32`, `bool`, `Uuid` |
| `timestamp(…, { withTimezone: true })` | `DateTime<Utc>` |
| `json` | `serde_json::Value`, which is `jsonb` |
| without `.notNull()` | `Option<…>` |
| `.default(false)` | `#[model(default = "false")]` |
| `.references(() => reviews.id, { onDelete: 'cascade' })` | `#[model(references(Review), on_delete = "cascade")]` |
| `index('…').on(…)`, `uniqueIndex('…').on(…)` | `index(…)`, `unique(…)` on the struct |

Index names do not have to match: the migration renames those of Drizzle.

## 3. Generate the migrations

In a project without `migrations/`, run:

```bash
cargo run -- migrate generate
```

It writes the framework schema, then a `<timestamp>_models.sql` file. Before the
`CREATE TABLE` of each model, a block looks at the database. When the table
does not exist, the block does nothing. When it does, the block:

- adds the columns the table lacks;
- converts a `json` column to `jsonb` when the model says `jsonb`, and the
  other way round. The table is rewritten and locked while it converts;
- gives each column the default and the nullability of the model. `SET NOT NULL`
  fails on a column that holds `NULL`;
- renames an index on the same columns, and a foreign key with the same target
  and the same `ON DELETE`, to the name of the model, instead of creating a
  second one;
- drops the other foreign keys of a referencing column, then adds the one of
  the model.

The block refuses to go on, and the migration is rolled back, when:

- a column has another type than in the model, for example `numeric` against
  `double precision`. Give the field the type of the database with
  `#[model(sql_type = "…")]`;
- the table has a `NOT NULL` column without default that the model lacks.
  Every insertion would fail. Add the field to the model.

Columns the model lacks are left alone otherwise. Nothing is dropped.

## 4. Try it on a copy of the database

Restore a recent backup into a separate database, then run:

```bash
export DATABASE_URL=postgresql://…/copy
cargo run -- migrate
cargo run -- migrate status
cargo run -- start
```

Expect `pending: 0`. Do not start a worker on the copy: it would run the queued
flows against the shops, with their tokens. `start` serves the app without
running anything, and at startup logs two warnings for what step 1 missed:

- `Runs are waiting for flows this application does not register`: queued
  runs whose flow has another name in Rust. A worker fails them when it claims
  them.
- `Some shops disabled crons that no longer exist`: a cron schedule that
  changed. The shops that disabled it would run it again.

## 5. Deploy

1. Stop every TypeScript process: web, worker and scheduler. Do not run both
   versions at once: the TypeScript scheduler does not share the Rust one's
   record of fired crons, so crons would be queued twice.
2. Run `app migrate`, the same migration job as for any deployment.
3. Start the Rust processes.

A run that a TypeScript worker held is claimed by a Rust worker once its lease
expires, like the run of any worker that stopped. If it has used all its
attempts, it fails as abandoned instead.
The `drizzle` schema and its `__drizzle_migrations` table are left in place:
the TypeScript version can still start on this database if you go back, since
it reads columns by name and reads `jsonb` like `json`.
