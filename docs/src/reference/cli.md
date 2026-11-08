# CLI commands

An OpenShop app has two command lines:

- the **application binary**, your own crate, whose `main` runs the
  application it declares with `app!`: `APP.main()`. It implements every
  runtime command;
- the **`openshop` CLI**, an optional tool that scaffolds projects, restarts
  the app when sources change, and forwards the other commands to `cargo`.

| `openshop` command | What it runs | Purpose |
| --- | --- | --- |
| `openshop init [<dir>]` | — | Scaffold a new project from a template. |
| `openshop init-ui` | — | Scaffold a custom admin page with its view. |
| `openshop dev [cargo args]` | `cargo run [cargo args] -- dev`, restarted on changes | Start the development server. The admin UI reloads by itself. |
| `openshop build [args]` | `openshop build-ui`, then `cargo build --release [args]` | Build the app for production. |
| `openshop build-ui` | `trunk build --release` on the crate of the bundle | Build the admin UI bundle of an app whose admin pages have a view, into `dist/`. |
| `openshop start [args]` | `cargo run --release -- start [args]` | Start the production web process. |
| `openshop worker [args]` | `cargo run --release -- worker [args]` | Start the worker process. |
| `openshop migrate [args]` | `cargo run -- migrate [args]` | Apply, generate, check, or inspect migrations. |
| `openshop test [args]` | `cargo test [args]` | Run the app's tests. |

An unknown command fails with status `2` and the usage text (see the end of
this page). When `cargo` fails, `openshop` prints the error and exits with
status `1`.

Install the CLI from the repository. `--locked` builds it with the versions of
the dependencies the repository tested:

```bash
cargo install --locked --git https://github.com/0xtlt/openshop-rust openshop-cli
```

The CLI is a convenience. Every command except `init`, `init-ui`,
`build-ui`, and the file watcher of `dev` can be run with `cargo run -- <command>` or, in
production, by executing the compiled binary directly (`./my-app start`).

## Scaffold a project

```bash
openshop init                              # asks for the directory, then the template
openshop init my-app --template=public     # asks for nothing
```

`init` makes a project from one of the templates embedded in the CLI:

| Template | What the project starts with |
| --- | --- |
| `minimal` | One provider and one flow, with the built-in admin. |
| `custom` | A custom app, built for the merchant who runs it: every building block (a provider, flows, a Shopify Function, webhooks, crons, app proxy routes, a public route, MCP capabilities, a model) and a custom admin page next to the built-in pages. |
| `public` | A public app, installed by many shops: an admin made of its own pages, a home and settings, and the operator dashboard for whoever runs it. |

In a terminal, `init` asks for the directory and the template it was not
given: type the name, choose the template with the arrow keys. Elsewhere, in
a script or CI, it needs both, and fails with the list of templates.

The target must be an empty or nonexistent directory. `init` derives the Cargo
package name from the directory name, pins the `openshop` dependency to the
Git commit the CLI was built from (`rev = "<commit>"`; a CLI built without git
pins the tag `v<version>` of its version instead, and says that the tag has to
exist), and writes the
migrations of the template in `migrations/`: the framework schema, and the
tables of its models. It does not update an existing app. See the
[first-app tutorial](../tutorials/first-app.md) for the full setup and
[Project structure](project-structure.md) for the generated files.

| Argument | Behavior |
| --- | --- |
| `<dir>` | Target directory, relative to the current directory. Asked for in a terminal; elsewhere the command fails with `Usage: openshop init [<dir>] [--template=<name>]`. |
| `--template=<name>`, `--template <name>`, `-t <name>` | `minimal`, `custom` or `public`. Asked for in a terminal; elsewhere the command fails with ``[openshop] Name a template: openshop init <dir> --template=<name>`` and the list. An unknown name fails with `[openshop] Unknown template: <name>`. |
| `--openshop-rev=<commit>` | Make the generated `Cargo.toml` depend on this commit of the Git repository (`rev = "…"`) instead of the commit the CLI was built from. |
| `--openshop-tag=<tag>` | Depend on this tag of the Git repository (`tag = "…"`). |
| `--openshop-branch=<name>` | Depend on this branch of the Git repository (`branch = "…"`). |
| `--openshop-path=<dir>` | Depend on a local checkout of the `openshop` crate (`path = "…"`). The directory must exist. |

At most one of the four `--openshop-*` arguments; a second one fails with
``[openshop] Name one source for `openshop` ``. The commit the CLI was built from
is the one `cargo install --git` took; for a CLI built from a local checkout,
make sure it is pushed, or pass `--openshop-path`. The `Dockerfile` of a project
with admin views installs the CLI of the `rev`, `tag` or `branch` of the
manifest, so it follows any of them.

A non-empty target fails with
`[openshop] Target directory is not empty: <path>`, before any question about
the template. The `custom` and `public` templates have admin pages with a
view, compiled to WebAssembly: install the toolchain once with
`rustup target add wasm32-unknown-unknown` and `cargo install trunk`.

If `init` fails after it started writing, it removes the files it wrote and the
directories it made, nothing else: a directory it shares with another `init`
stays if that one wrote in it. A
directory that already had something in it is refused before anything is
written.

### Scaffold a custom admin page

```bash
openshop init-ui
```

`init-ui` gives an app a first [custom admin page](custom-admin-pages.md) with
a view, and what views are compiled with. Run it from the project root:
without a `Cargo.toml` in the current directory it fails with
``[openshop] No Cargo.toml here. Run `openshop init-ui` from the root of an OpenShop project.``

The command writes `src/admin/hello.rs`, an example `/hello` page with a
loader and its view, declares it in `src/admin/mod.rs`, and adds `dist/`,
where the bundle is built, to `.gitignore`. When the page already exists it
fails with `[openshop] src/admin/hello.rs already exists.`

In `Cargo.toml`, it adds the `ui` feature to the `openshop` dependency and
adds `leptos`. When `openshop` is not declared on one line of
`[dependencies]`, the manifest is left as it is and the lines to add are
printed.

It then prints what to write in `src/main.rs`: `mod admin;`, the page in the
`admin_pages` of `app!`, and the `admin_ui` setting.

### Build the admin UI bundle

```bash
openshop build-ui
```

`build-ui` reads `src/` for the admin pages that have a `#[view]`. Without
one, it prints
`[openshop] No admin page has a view: the admin UI is the one built into openshop.`
and builds nothing. Otherwise it writes the crate of the bundle under the
`target` directory of the project, in `openshop-ui/<package>/`, and builds it
with [trunk](https://trunkrs.dev) into `dist/`, optimized for size. The crate
is made of the modules that declare the pages; see
[What the bundle is made of](custom-admin-pages.md#what-the-bundle-is-made-of).

A page with a view declared in `src/main.rs` or `src/lib.rs` is refused: the
bundle is made of a module, such as `src/admin/`. `openshop build` runs the
same build before `cargo build --release`.

## Command behavior

These are the commands of the application binary. Run them from the app root:
the binary resolves `./migrations`, `.env`, and Shopify TOML files relative to
the current directory.

| Command | Input or flags | Behavior |
| --- | --- | --- |
| `dev` | Process environment and `.env` | Loads `.env`, pushes the framework schema and registered models, then runs the API, the admin UI, a worker, and the cron scheduler in one process. Refused in production mode. |
| `start` | Process environment | Serves the API and the admin UI and starts cron dispatch. No worker, no schema changes. Refuses to start while `migrations/` holds a migration the database has not applied, or does not cover the framework schema. |
| `worker` | `--concurrency=N` | Executes queued runs. No HTTP server, no cron dispatch, no schema changes. Refuses to start in the same cases as `start`. |
| `migrate` | Committed `migrations/` directory | Applies pending SQL files in file-name order. |
| `migrate generate` | `--name=<name>`, `--allow-destructive`, `--allow-generate-in-production` | Writes the framework schema your project does not cover yet, and the statements that bring your tables in line with the registered models since the last generation; with `--name` and nothing else to write, writes an empty migration for your own changes. Updates `migrations/schema.snapshot.json`. Does not connect to the database. Refuses a migration that drops a table or a column, or converts a column in a way that can lose values, unless `--allow-destructive` is given. Blocked in production by default. See [Generated model migrations](database.md#generated-model-migrations). |
| `migrate check` | `migrations/` directory | Fails when the committed migrations do not cover the framework schema, or when the models changed since the last generated migration. Does not connect to the database. |
| `migrate status` | Database and `migrations/` directory | Prints the migrations folder and the applied and pending migrations. |

No command, `help`, `--help` and `-h` print the usage text and exit with status
`0`. Any other first argument fails with `[openshop] Unknown command: <name>`
and the usage text on stderr, with status `2`. An unknown `migrate` subcommand
fails with `[openshop] Unknown migrate command: <name>`. A failing command
prints its error and exits with a non-zero status.

Details:

- Every command builds the configuration before it runs, including `migrate`.
  Variables the configuration requires must be set for all of them.
- `dev` and `start` listen on `0.0.0.0` at `PORT` (default `3000`).
- In `dev`, a page of the admin UI reloads by itself when the app has been
  started again, or when the bundle it is served from disk has been built
  again. The shell is served with a script that listens to
  `GET /__openshop/reload`, a `text/event-stream` that names the version being
  served, and reloads the page when it is no longer the one it was served.
  `start` serves neither the script nor the route.
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
- it watches `src/`, `Cargo.toml`, and `build.rs` of every crate on disk the
  project is compiled with: its path dependencies and theirs, members of its
  workspace or not. `cargo metadata` gives the list at startup and each time a
  manifest changes, and the startup output names the crates. With
  `-p <package>`, they are the crates of that package, itself included; in the
  root of a workspace that is not a package, the ones of its default members;
- hidden files and directories and names ending in `~` are ignored;
- changes are debounced for 500 ms, then the app receives `SIGTERM`, is
  killed if it has not exited after 35 seconds, and is started again once
  every process of it is gone;
- changes that arrive during a restart are coalesced into one more restart;
- arguments after `dev` are passed to `cargo run`, for example
  `openshop dev -p my-app` in a workspace;
- when an admin page has a view, it also writes the crate of the admin UI
  bundle, as `build-ui` does, runs `trunk watch` on it to rebuild the bundle
  when a page changes, or a crate on disk the project is compiled with, and,
  unless `OPENSHOP_UI_DIR` is already set, starts the app with
  `OPENSHOP_UI_DIR` pointing at `dist`. A view added or removed while `dev`
  runs is taken into account at the next change. If `trunk` cannot be started,
  a warning is printed and the bundle is not rebuilt.

The page open in the browser follows: it reloads once the app is back, and
again if the bundle is ready after it. A change that does not compile leaves
the page as it is, and the error in the terminal.

### Stopping the app

`cargo run` is not always the app: cargo replaces itself with the app on
Unix, but the `cargo` on the `PATH` may be a wrapper that runs the real one as
its child, such as the shim of a tool manager, and so may a `runner` of the
cargo configuration. On Unix, `openshop dev` therefore starts `cargo run` in a
process group of its own, which everything it starts joins, and stops the
group: `SIGTERM` to every process of it, `SIGKILL` to those left after 35
seconds, then it waits until none is left. The app does not read the
terminal's input, and Ctrl-C, `SIGTERM` or `SIGHUP` (the terminal closing) to
`openshop dev` stop it the same way. On Windows, cargo stops what it started.

Before it starts the app, `openshop dev` checks that its port is free: `PORT`,
else the `PORT` of `.env`, else 3000. A port still taken after 5 seconds is
said loudly, with the process that holds it when `lsof` can name it:

```text
[openshop] !!! Port 3000 is still taken, by another process, PID 31654 (my-app).
[openshop] !!! The app starting now cannot listen on it: what answers on http://localhost:3000 is NOT this code.
[openshop] !!! Stop that process (`lsof -nP -iTCP:3000 -sTCP:LISTEN` names it), then save a file to restart.
```

The app is started anyway, and fails with `Address already in use`. An app
that exits by itself, because it does not build, does not start or cannot
listen, is said too: nothing runs until the next change.

```text
[openshop] !!! The app exited (exit status: 1): nothing of the current code is serving. Fix what it printed, then save a file to restart it.
```

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
