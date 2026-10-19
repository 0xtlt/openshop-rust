# Build your first OpenShop app

In this lesson, we will create an embedded Shopify app and run `syncOrders`.
The flow will read orders from a development store and pass them to a sample
warehouse provider. The sample prints a line in the terminal; it does not send
the records to a real warehouse.

## Before you start

Have these ready:

- Rust **1.88** or newer, with Cargo. Check with `rustc --version`.
- Docker with its daemon running. We will use a PostgreSQL **17** container.
- [Shopify CLI](https://shopify.dev/docs/api/shopify-cli) installed and available as `shopify`.
- A Shopify development store and an account allowed to create and install an app on it.
- Basic familiarity with Rust and running commands in a terminal.

Use a development store throughout this lesson. An empty order list is fine.
If you already run PostgreSQL on port 5432, skip the container in step 2, use a
dedicated database there, and substitute its connection URL in step 3.

## 1. Create the project

Install the `openshop` command once:

```bash
cargo install --locked --git https://github.com/0xtlt/openshop-rust openshop-cli
```

Then create the project:

```bash
openshop init my-app --template=minimal
cd my-app
```

This tutorial starts from the `minimal` template. Without `--template`,
`openshop init` asks which one to start from; the
[CLI reference](../reference/cli.md#scaffold-a-project) describes the others.

The CLI prints `[openshop] Created my-app in` followed by the project path. Keep
this terminal in `my-app` for the remaining commands. The generated project pins
the OpenShop version that created it: its `Cargo.toml` depends on `openshop` at
the git commit the CLI was built from, and on `serde`.

Open these three files:

| File | What it contains |
| --- | --- |
| `src/providers/warehouse.rs` | The `warehouse` provider: the `Warehouse` struct with two configuration fields, and a `#[provider]` block with a connection check and a `push` method |
| `src/flows/sync_orders.rs` | The sample flow, a function marked `#[flow("syncOrders")]` with `fetch-orders` and `send-to-warehouse` steps, and `SyncOrdersInput`, the input of a run |
| `src/main.rs` | The application: an `app!` declaration that lists the provider and the flow, with a sample cron schedule, an error hook, and MCP turned off. `main` runs it with `APP.main()` |

Each building block is declared where it is written, with an attribute on the
function or the struct. `src/main.rs` only lists them.

There is also a `migrations/` directory containing the initial database migration.
The [project structure reference](../reference/project-structure.md) describes the
other files when you need them.

## 2. Start the database

The project includes a `docker-compose.yml` for PostgreSQL 17:

```bash
docker compose up -d
```

Wait until this command reports that PostgreSQL is accepting connections:

```bash
docker compose exec postgres pg_isready -U openshop -d openshop
```

On later sessions, run `docker compose up -d` again; the data is kept in a
Docker volume. The database name, username, and password are all `openshop`.
They are local development values.

## 3. Configure local storage

Copy the example environment file:

```bash
cp .env.example .env
```

`.env` now contains the URL of the database we just started:

```dotenv
DATABASE_URL=postgresql://openshop:openshop@localhost:5432/openshop
```

Append a new encryption key once:

```bash
printf 'ENCRYPTION_KEY=%s\n' "$(openssl rand -hex 32)" >> .env
```

Keep this key for subsequent sessions. The generated `.gitignore` excludes
`.env`. `openshop dev` reads it automatically. Leave `SHOPIFY_API_KEY` and
`SHOPIFY_API_SECRET` empty; Shopify CLI provides them in step 5.

Migration commands use the process environment, so export the database URL in
this terminal before applying the initial migration:

```bash
export DATABASE_URL=postgresql://openshop:openshop@localhost:5432/openshop
cargo run -- migrate
cargo run -- migrate status
```

The first command compiles the project, which takes a few minutes the first
time. It then prints `[openshop] Client migrations applied`. The status output
should end with `pending: 0`. If the database connection fails, check that the
container is ready and that the exported URL matches it.

## 4. Keep the first run manual

The generated application schedules `syncOrders` every five minutes. In
`src/main.rs`, the declaration of the application and the `main` function read:

```rust
static APP: App = app! {
    providers: [Warehouse],
    flows: [sync_orders],
    crons: [
        Cron::new("*/5 * * * *", sync_orders::NAME)
            .name("Quick sync")
            .input(json!({ "limit": 10 })),
    ],
    // MCP is off: `POST /mcp` is refused and the admin has no MCP page.
    // Remove this line to turn it on.
    mcp: McpConfig::disabled(),
    on_error: report_failure,
};

fn main() -> Result<()> {
    APP.main()
}
```

Remove the `crons` entry, so that the declaration becomes:

```rust
static APP: App = app! {
    providers: [Warehouse],
    flows: [sync_orders],
    // MCP is off: `POST /mcp` is refused and the admin has no MCP page.
    // Remove this line to turn it on.
    mcp: McpConfig::disabled(),
    on_error: report_failure,
};
```

Leave the rest of the file unchanged. We will run the flow from the admin UI
after saving its provider credentials. This removes the generated schedule for
now and keeps the error hook, `report_failure`, and the line that turns MCP
off. The
[flow guide](../guides/define-flow.md#3-schedule-the-flow-for-installed-shops) adds a
schedule that explicitly targets installed shops.

## 5. Link and start the Shopify app

```bash
shopify auth login
shopify app config link
```

Follow the prompts to select your organization and create or select a development
app. Check that the linked Shopify app configuration requests `read_products`
and `read_orders`, as the template does. Then start development:

```bash
shopify app dev
```

Follow Shopify CLI's prompts to select your development store. This command
starts the tunnel and OpenShop together: it runs `openshop dev`, the `dev`
command declared in `shopify.web.toml`. OpenShop compiles the project,
synchronizes the local database schema, and starts the API, admin UI, scheduler,
and worker. It rebuilds and restarts the app when a source file changes, and
the admin UI then reloads by itself. Wait for `[openshop] Ready.` in the output
and leave it running.

Open the preview URL printed by Shopify CLI (or press `p`) and install the app.
You should see the OpenShop dashboard **inside Shopify admin**. Opening the
local address of the app directly in a browser does not provide the Shopify
session the embedded UI needs.

## 6. Configure the sample provider

In **Providers**, fill in the `warehouse` form and click **Save**:

| Field | Development value |
| --- | --- |
| API URL | `https://example.com` |
| API Key | `development-only` |

Click **Test connection**. It should succeed: the generated checker only verifies
that both values are present. The generated `push` method only prints a line in
the terminal; these placeholder credentials never contact `example.com`.

## 7. Run the integration

In **Flows**, click **Run** next to `syncOrders`, replace the input with:

```json
{ "limit": 10 }
```

Click **Run**. The run page opens. The run should reach `completed` and show
two completed steps: `fetch-orders` and `send-to-warehouse`. Its logs should
include **Orders synced**. The count may be zero on a new development store.

If it fails, look at the failed step and the logs. A Shopify authorization error
usually means the app installation or `read_orders` permission needs attention;
see [troubleshooting](../guides/troubleshooting.md). A run that stays `pending`
needs a running worker; in this lesson, `shopify app dev` starts it for you.

In a second terminal, from `my-app`, check the project:

```bash
cargo test
```

This compiles the project and runs the generated `the_application_is_valid`
test, which builds the configuration of the application and checks that it
lists the flow and the provider. A second generated test,
`sync_orders_pushes_orders_to_the_warehouse`, is marked `#[openshop::test(APP)]`:
it starts the application on an isolated PostgreSQL schema and runs
`syncOrders` with a scripted Shopify answer and a fake warehouse. The fake,
`ctx.fakes.for_call(Warehouse::PUSH)`, is typed like the method: it returns a
`usize` and gives back the `Vec<Order>` it received. The test is skipped
unless you run `cargo test -- --ignored`.

## What you built

You now have an installed app, saved provider configuration, and a completed
background job with inspectable steps. Continue with
[Build a checkpointed flow](checkpointed-flow.md) to write a small flow
of your own and watch it resume after a pause.

When you finish the session, stop Shopify CLI with `Ctrl+C`. You can stop the
local database with `docker compose stop` and start it next time with
`docker compose up -d` without losing its data.
