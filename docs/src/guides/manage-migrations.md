# Manage migrations

Use this guide when adding app tables or upgrading OpenShop's framework schema.
Start with a generated app, its `migrations/` directory, and an explicitly
configured `DATABASE_URL` for each environment.

## 1. Include the schema

Migrations are SQL files owned by your project. `openshop init` writes the
framework schema as the first one:

```txt
migrations/
  20261006143115_openshop_init.sql
```

The file starts with a marker line:

```sql
-- openshop:framework 0001_openshop_init
```

Keep that line. `migrate generate` and `migrate check` read it to know which
parts of the framework schema your project already covers. Files are applied in
file-name order and recorded in the `__openshop_migrations` table.

Your own tables go in additional files in the same directory. If you register
application models with `.model(define_model(..))`, `dev` and the test harness
create those tables directly, but the migration that creates them elsewhere is
yours to write. See the [model reference](../reference/database.md#app-models)
for model definitions.

## 2. Generate and review locally

After upgrading OpenShop, run outside production mode:

```bash
cargo run -- migrate generate
cargo run -- migrate check
```

`migrate generate` writes one file for each framework schema change that
`migrations/` does not cover yet and prints its path. When nothing is missing it
prints `[openshop] No schema changes, nothing to generate`. `migrate check`
fails while the framework schema is not covered.

To change your own tables, create an empty migration and write its SQL:

```bash
cargo run -- migrate generate --name=add_reviews
```

This creates `migrations/<timestamp>_add_reviews.sql`. For a model, the SQL to
put there is what `Model::create_sql()` returns. For this model:

```rust,ignore
use openshop::{Model, define_model};

pub fn reviews() -> Model {
    define_model("reviews")
        .column("product_id", "text NOT NULL")
        .column("rating", "integer NOT NULL")
        .index("reviews_shop_product_idx", &["shop", "product_id"])
}
```

the migration is:

```sql
-- add_reviews
-- Write your schema changes below.

CREATE TABLE IF NOT EXISTS "reviews" (
  "id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  "shop" text NOT NULL,
  "created_at" timestamp with time zone DEFAULT now() NOT NULL,
  "updated_at" timestamp with time zone DEFAULT now() NOT NULL,
  "product_id" text NOT NULL,
  "rating" integer NOT NULL
);
CREATE INDEX IF NOT EXISTS "reviews_shop_product_idx" ON "reviews" ("shop", "product_id");
```

Review the new SQL under `migrations/`, especially destructive changes
and required columns on existing tables. Commit those files with the code that
needs them. `migrate check` only checks that the framework schema is covered; it
does not compare your models with your SQL and does not decide whether a schema
change is safe for your data.

Do not edit a migration after it has been applied: `migrate` compares checksums
and refuses a modified file. Add a new migration instead.

`dev` pushes the schema for local iteration. It creates missing tables and
indexes and does not alter existing columns, so a changed column needs an
`ALTER TABLE` migration applied with `cargo run -- migrate` even locally. A
successful dev session is not proof that committed migrations can upgrade an
existing database. Test the migration sequence on a separate database with the
previous schema.

## 3. Apply committed SQL during deployment

Inject the target environment's `DATABASE_URL` through your deployment platform,
then run the finite migration job once before starting the new app processes.
With the binary built by the generated `Dockerfile`:

```bash
app migrate
app migrate status
```

From a source checkout, the same commands are `cargo run -- migrate` and
`cargo run -- migrate status`.

Expect `pending: 0`. Migration commands do not load the development `.env` file;
export the URL explicitly when running them manually. Without `DATABASE_URL`
they connect to the local development database
(`postgresql://openshop:openshop@localhost:5432/openshop`).

The apply command reads committed SQL from `./migrations` in the working
directory; it does not generate new files. Each pending file runs in its own
transaction. Include the migration directory in the deployment artifact, as the
generated `Dockerfile` does. Web and worker startup never apply migrations.

## 4. Verify and recover

Start the web and worker services and run a harmless smoke flow. If schema access
fails, check the migration job's database URL and status before restarting again.

Take a database backup before a production schema change. If SQL has already
been applied, an application rollback is safe only when the old code remains
compatible with that schema; use a reviewed forward migration or your database
restore procedure otherwise.

Continue with [Deploy to production](deploy-production.md) or
[Upgrade OpenShop](upgrade.md).
