# Deploy to production

Use this guide for an app that already works on a development store. You need a
production PostgreSQL database, an HTTPS app origin, a process supervisor or
hosting platform, and the credentials for the Shopify app being deployed.

A deployment has three stages: build, apply migrations once, then start the web
and worker services. Run app commands from the project root.

## 1. Prepare the release

In development or CI:

```bash
cargo check --locked
cargo run -- migrate generate
cargo run -- migrate check
```

Run your app's tests if you have [set up a test suite](test-app.md).
Review and commit any generated files in `migrations/`, and commit `Cargo.lock`:
the generated `Dockerfile` builds with `--locked`. Test the migrations against
a separate database before applying them to production; see
[Manage migrations](manage-migrations.md).

## 2. Configure the environment

Inject these into the production services using your platform's secret settings:

```dotenv
OPENSHOP_ENV=production
DATABASE_URL=postgresql://user:password@database:5432/openshop
SHOPIFY_API_KEY=your-client-id
SHOPIFY_API_SECRET=your-client-secret
HOST=https://your-app.example.com
ENCRYPTION_KEY=replace-with-64-hex-characters
```

`OPENSHOP_ENV=production` turns on production mode: `ENCRYPTION_KEY` becomes
mandatory for storing credentials, and migration generation is refused.
Generate `ENCRYPTION_KEY` once with `openssl rand -hex 32`, store it securely, and
reuse it across deployments. Both services need the same key, database, and
Shopify configuration. Production commands do not load `.env` automatically.
The web process listens on `PORT`, `3000` by default.
For multiple apps, use [Configure Shopify apps](configure-shopify-apps.md).
Every variable is described in
[Environment variables](../reference/environment-variables.md).

## 3. Build the artifact

```bash
cargo build --release --locked
```

The output is a single binary, `target/release/<package-name>`. It contains the
web server, the worker, the scheduler, the migration commands, and the admin UI.
No Shopify credential is needed at build time. Deploy the binary together with
`migrations/` and any referenced Shopify TOML files, and run it from the
directory that contains them. An app with custom admin pages also ships its own
UI bundle; see [Ship the bundle](add-admin-page.md#ship-the-bundle).

The generated `Dockerfile` is a starting point: it builds the binary, installs
it as `app`, copies `migrations/` and `shopify.app*.toml`, sets
`OPENSHOP_ENV=production` and `PORT=3000`, and starts the web process. With
the `custom` and `public` templates, it builds the admin UI bundle first and
serves it from `/app/dist`.

```bash
docker build -t my-app .
```

One container runs one process. Start workers and the migration job from the
same image with a different command, as the generated `Procfile` does:

```txt
release: app migrate
web: app start
worker: app worker --concurrency=5
```

## 4. Apply migrations

Run a single release job with the target production `DATABASE_URL`:

```bash
app migrate
app migrate status
```

Confirm `pending: 0` before starting the new processes. This command applies
committed SQL only. Neither the web process nor the worker migrates on startup,
and both refuse to start while a migration of `migrations/` is not applied:
`Not starting: 1 migration is not applied (…). Run `migrate` first.`

## 5. Start both services

Configure two independently supervised services from the same build:

| Service | Command | Initial process count |
| --- | --- | --- |
| Web | `app start` | 1 |
| Worker | `app worker --concurrency=5` | 1 |

Both commands keep running. Put them in separate service definitions, containers,
or process-manager entries; two sequential lines in a shell script would never
start the worker. The web process serves HTTP and dispatches scheduled runs.
The worker executes the queued work.

Several web replicas are fine with crons: every replica starts a scheduler, and
the first to reach an occurrence of a cron for a shop queues its run, so it
runs once. Scale workers after measuring database and provider capacity. See
[Operate an app](operate-app.md) for that procedure.

Both processes stop cleanly on `SIGTERM`. Give the worker up to its lease
duration, 30 seconds by default, to finish active runs before the platform kills
it.

## 6. Release the Shopify configuration

Set the production application and callback URLs in the linked Shopify app TOML,
then release its configuration:

```bash
shopify app deploy --config shopify.app.toml
```

Repeat for every configured app. This updates Shopify-side configuration; it does
not deploy the OpenShop server. Complete the app's installation or renewed
scope authorization as needed.

## 7. Verify the whole path

1. Request `https://your-app.example.com/health` and expect HTTP 200.
2. Open the installed app inside Shopify admin.
3. Check a configured provider using its production credentials.
4. Run a harmless flow and verify it reaches **completed** with the expected steps.
5. Confirm both service logs show no database, encryption, or configuration errors.

`/health` only proves the HTTP process responds. A completed smoke flow also
exercises the queue and worker. If runs stay pending, use
[Troubleshooting](troubleshooting.md#runs-stay-pending).

## Roll back a failed release

Restore the previous app build only if it remains compatible with the current
database schema. Applied SQL is not undone by rolling back a process. For an
incompatible schema change, use a reviewed forward fix or your database restore
procedure. Keep the original encryption key available with database backups.
