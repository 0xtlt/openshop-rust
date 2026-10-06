# CLI commands

An OpenShop app has two command lines:

- the **application binary**, your own crate, whose `main` calls
  `openshop::cli::run(config)` with the function that builds the
  configuration. It implements every runtime command;
- the **`openshop` CLI**, an optional tool that scaffolds projects, restarts
  the app when sources change, and forwards the other commands to `cargo`.

| `openshop` command | What it runs | Purpose |
| --- | --- | --- |
| `openshop init <dir>` | — | Scaffold a new project. |
| `openshop init-ui [dir]` | — | Scaffold the admin UI bundle for custom admin pages. |
| `openshop dev [cargo args]` | `cargo run [cargo args] -- dev`, restarted on changes | Start the development server. |
| `openshop codegen [--api-version=YYYY-MM] [--out=<file>]` | — | Download the Shopify Admin GraphQL schema of another API version for [`graphql!`](graphql.md#schema-and-api-version). |
| `openshop build [args]` | `cargo build --release [args]` | Build the app for production. |
| `openshop start [args]` | `cargo run --release -- start [args]` | Start the production web process. |
| `openshop worker [args]` | `cargo run --release -- worker [args]` | Start the worker process. |
| `openshop migrate [args]` | `cargo run -- migrate [args]` | Apply, generate, check, or inspect migrations. |
| `openshop test [args]` | `cargo test [args]` | Run the app's tests. |

Any other command prints the usage text. When `cargo` fails, `openshop` prints
the error and exits with status `1`.

Install the CLI from the repository:

```bash
cargo install --git https://github.com/0xtlt/openshop-rust openshop-cli
```

The CLI is a convenience. Every command except `init`, `init-ui`, `codegen`,
and the file watcher of `dev` can be run with `cargo run -- <command>` or, in
production, by executing the compiled binary directly (`./my-app start`).

## Scaffold a project

```bash
openshop init my-app
```

`init <dir>` requires an empty or nonexistent target directory. It writes the
embedded minimal template, derives the Cargo package name from the directory
name, pins the `openshop` dependency to the Git tag of the CLI's own version
(`tag = "v<version>"`), and generates the initial migration in `migrations/`.
It does not update an existing app. See the
[first-app tutorial](../tutorials/first-app.md) for the full setup and
[Project structure](project-structure.md) for the generated files.

| Argument | Behavior |
| --- | --- |
| `<dir>` | Target directory, relative to the current directory. Required: without it the command fails with `Usage: openshop init <dir>`. |
| `--openshop-path=<dir>` | Make the generated `Cargo.toml` depend on a local checkout of the `openshop` crate (`path = "…"`) instead of the tagged Git repository. The directory must exist. |

A non-empty target fails with
`[openshop] Target directory is not empty: <path>`.

### Scaffold a custom admin UI

```bash
openshop init-ui
```

`init-ui [dir]` adds the WebAssembly UI crate that an app with
[custom admin pages](custom-admin-pages.md) needs. Run it from the project
root: without a `Cargo.toml` in the current directory it fails with
``[openshop] No Cargo.toml here. Run `openshop init-ui` from the root of an OpenShop project.``

| Argument | Behavior |
| --- | --- |
| `[dir]` | Target directory, `ui` by default. It must be empty or nonexistent. |
| `--openshop-path=<dir>` | Depend on the `openshop-ui` crate next to a local checkout of the `openshop` crate instead of the tagged Git repository. |

The command writes `Cargo.toml` (package `<app>-ui`, its own cargo workspace,
`openshop-ui` pinned to the CLI's tag), `Trunk.toml`, `index.html`,
`src/main.rs` with an example `/hello` page, and `.gitignore`, then prints the
builder calls to add to your configuration.

## Command behavior

These are the commands of the application binary. Run them from the app root:
the binary resolves `./migrations`, `.env`, and Shopify TOML files relative to
the current directory.

| Command | Input or flags | Behavior |
| --- | --- | --- |
| `dev` | Process environment and `.env` | Loads `.env`, pushes the framework schema and registered models, then runs the API, the admin UI, a worker, and the cron scheduler in one process. Refused in production mode. |
| `start` | Process environment | Serves the API and the admin UI and starts cron dispatch. No worker, no schema changes. |
| `worker` | `--concurrency=N` | Executes queued runs. No HTTP server, no cron dispatch, no schema changes. |
| `migrate` | Committed `migrations/` directory | Applies pending SQL files in file-name order. |
| `migrate generate` | `--name=<name>`, `--allow-generate-in-production` | Writes the framework schema your project does not cover yet, and the statements that bring your tables in line with the registered models since the last generation; with `--name` and nothing else to write, writes an empty migration for your own changes. Updates `migrations/schema.snapshot.json`. Does not connect to the database. Blocked in production by default. See [Generated model migrations](database.md#generated-model-migrations). |
| `migrate check` | `migrations/` directory | Fails when the committed migrations do not cover the framework schema, or when the models changed since the last generated migration. Does not connect to the database. |
| `migrate status` | Database and `migrations/` directory | Prints the migrations folder and the applied and pending migrations. |

Any other first argument, or none, prints the usage text and exits with status
`0`. An unknown `migrate` subcommand fails with
`[openshop] Unknown migrate command: <name>`. A failing command prints its
error and exits with a non-zero status.

Details:

- Every command builds the configuration before it runs, including `migrate`.
  Variables the configuration requires must be set for all of them.
- `dev` and `start` listen on `0.0.0.0` at `PORT` (default `3000`).
- `--concurrency=N` must be written with `=`. When the flag is absent, the
  worker uses `worker.concurrency` from the configuration, then the default of
  `5`. A value that is not a positive integer stops the command with
  `[openshop] --concurrency must be a positive integer, got "<value>"`. See
  [Worker defaults](configuration.md#worker-defaults).
- `--name=<name>` is lowercased and every character that is not an ASCII letter
  or digit becomes `_`. Generated files are named
  `<YYYYMMDDHHMMSS>_<name>.sql` using the current UTC time. Without `--name`,
  a migration for new models is named `<YYYYMMDDHHMMSS>_models.sql`.
- `start` and `worker` never generate or apply migrations. See
  [Database and migrations](database.md#client-owned-migrations) for the
  migration contract and [Deploy to production](../guides/deploy-production.md)
  for the sequence.
- `dev`, `start`, `worker`, `migrate`, and `migrate status` connect to
  `DATABASE_URL` and fall back to the local development database when it is
  unset. See [Environment variables](environment-variables.md).

`dev`, `start`, and `worker` stop on `SIGINT` or `SIGTERM`. The HTTP server
stops accepting connections and waits up to five seconds for requests in
flight. A worker stops claiming runs and waits for active runs for up to one
lease duration (`30` seconds by default).

`start` runs the cron scheduler and there is no leader election: run a single
`start` process unless duplicate cron dispatches are acceptable.

## Development process

`shopify app dev` executes the `dev` command declared in `shopify.web.toml`
(`openshop dev` in the template, which requires the CLI to be installed). Do
not start a second `dev` process alongside it.

`openshop dev` wraps `cargo run -- dev` with a file watcher:

- it watches `src/` and `migrations/` recursively, and `Cargo.toml`,
  `Cargo.lock`, `.env`, and `shopify.app*.toml` in the project root;
- hidden files and directories and names ending in `~` are ignored;
- changes are debounced for 500 ms, then the process receives `SIGTERM`, is
  killed if it has not exited after 35 seconds, and is started again;
- changes that arrive during a restart are coalesced into one more restart;
- arguments after `dev` are passed to `cargo run`, for example
  `openshop dev -p my-app` in a workspace;
- when the project has a `ui/Trunk.toml`, it also runs `trunk watch` in `ui/`
  to rebuild the custom admin UI bundle and, unless `OPENSHOP_UI_DIR` is
  already set, starts the app with `OPENSHOP_UI_DIR` pointing at `ui/dist`. If
  `trunk` cannot be started, a warning is printed and the bundle is not
  rebuilt.

Only `dev` loads the project-root `.env` file, before it builds the
configuration. Set variables explicitly for build, migration, test, and
production commands. Existing process variables take precedence over `.env`
in development. See [Environment variables](environment-variables.md).

## Tests

`openshop test [args]` is `cargo test [args]`. The generated project contains a
configuration test and one ignored test that needs PostgreSQL; run it with
`cargo test -- --ignored`. The test harness lives behind the `testing` cargo
feature of the `openshop` crate. Follow [Test an app](../guides/test-app.md) to
add a suite and see [Testing](testing.md) for the API.
