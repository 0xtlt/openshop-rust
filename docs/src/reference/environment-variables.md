# Environment variables

`dev` loads `.env` from the project root. An environment variable already
present in the process wins over the value in `.env`. Production commands such
as `start` and `worker` expect the process manager or deployment platform to
inject their environment.

Shopify credentials, the app URL, the encryption key, and the production flag
are captured once in a `RuntimeEnv` when a command starts. Changing a variable
afterwards has no effect on a running process.

## Complete reference

| Variable | Required | Default | Used by |
| --- | --- | --- | --- |
| `DATABASE_URL` | Yes | `postgresql://openshop:openshop@localhost:5432/openshop` for CLI commands | API, worker, migration commands, tests |
| `SHOPIFY_API_KEY` | Single-app mode | Empty | Shopify session-token audience and embedded App Bridge key |
| `SHOPIFY_API_SECRET` | Single-app mode | Empty | OAuth, session-token, webhook, and proxy signature verification |
| `HOST` | Production | Falls back to `SHOPIFY_APP_URL` | Public application URL and allowed CORS origin |
| `SHOPIFY_APP_URL` | When `HOST` is absent | Empty | Legacy public application URL and allowed CORS origin |
| `ENCRYPTION_KEY` | Production | Plaintext storage in development | Provider configuration and Shopify access-token encryption |
| `OPENSHOP_ENV` | Production | Development mode | `production` enables production safety checks and encryption enforcement |
| `NODE_ENV` | No | — | `production` is honored like `OPENSHOP_ENV=production` |
| `PORT` | No | `3000` | HTTP port of `dev` and `start` |
| `OPENSHOP_UI_DIR` | No | The bundle set with `.admin_ui(…)`, then the embedded admin UI | Directory of a custom admin UI bundle served by `dev` and `start` |
| `PGPOOL_MAX` | No | `10` | Maximum PostgreSQL pool connections per process |
| `PGPOOL_IDLE_TIMEOUT_MS` | No | `30000` | Idle PostgreSQL connection timeout |
| `PGPOOL_CONNECTION_TIMEOUT_MS` | No | `5000` | PostgreSQL connection acquisition timeout |
| `RUST_LOG` | No | `info,sqlx=warn` | Log filter of the process logger |
| `TEST_DATABASE_URL` | No | Falls back to `DATABASE_URL`, then the local development database | Database of the test harness |

Notes:

- An empty value counts as unset for `SHOPIFY_API_KEY`, `SHOPIFY_API_SECRET`,
  `HOST`, `SHOPIFY_APP_URL`, `ENCRYPTION_KEY`, and `OPENSHOP_UI_DIR`.
- A `PORT` that is not a number between 1 and 65535 falls back to `3000`.
- A `PGPOOL_*` value that is not a non-negative integer falls back to its
  default.
- The `DATABASE_URL` fallback applies to `dev`, `start`, `worker`, `migrate`,
  and `migrate status`, when the variable is unset or empty.
  `OpenShop::connect` and `openshop::db::connect` have no fallback and fail
  with `[openshop] DATABASE_URL not set`.
- `RUST_LOG` is read when the command starts. `dev` loads `.env` first, so the
  variable can be set there for development. See [Logging](logging.md).

## Minimal single-app `.env`

```dotenv
DATABASE_URL=postgresql://openshop:openshop@localhost:5432/openshop
SHOPIFY_API_KEY=your-client-id
SHOPIFY_API_SECRET=your-client-secret
HOST=https://your-app.example.com
ENCRYPTION_KEY=replace-with-64-hex-characters
```

Generate the encryption key once:

```bash
openssl rand -hex 32
```

The key must remain stable. Changing or losing it makes encrypted provider
credentials and Shopify access tokens unreadable.

The `.env` loader reads one `NAME=value` assignment per line. Lines that are
empty or start with `#` are skipped, a leading `export` is accepted, and a
value wrapped in matching single or double quotes is unquoted. Comments after a
value are not stripped.

## Multi-app configuration

In multi-app mode, API keys and secrets come from the configuration your binary
builds, usually with secrets read from your own environment variable names:

```rust,ignore
use openshop::{Context, IndexMap, OpenShopConfig, Result, ShopifyAppConfig, ShopifyConfig};

fn env(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("{name} is not set"))
}

fn config() -> Result<OpenShopConfig> {
    let mut apps = IndexMap::new();
    apps.insert(
        "retail".to_string(),
        ShopifyAppConfig::Toml {
            toml: "shopify.app.retail.toml".to_string(),
            api_secret: env("RETAIL_SHOPIFY_API_SECRET")?,
            app_url: None,
        },
    );
    apps.insert(
        "wholesale".to_string(),
        ShopifyAppConfig::Keys {
            api_key: env("WHOLESALE_SHOPIFY_API_KEY")?,
            api_secret: env("WHOLESALE_SHOPIFY_API_SECRET")?,
            app_url: Some(env("WHOLESALE_APP_URL")?),
        },
    );

    OpenShopConfig::builder()
        .shopify(ShopifyConfig {
            scopes: Some("read_orders,write_products".to_string()),
            apps: Some(apps),
        })
        .build()
}
```

OpenShop does not assign special meaning to `RETAIL_SHOPIFY_API_SECRET`,
`WHOLESALE_SHOPIFY_API_KEY`, or `WHOLESALE_APP_URL`; they are application-defined
variables read by this example.

These variables are read while the configuration is built. Pass the `config`
function itself to `openshop::cli::run`, as the generated `main` does, so that
`dev` loads `.env` first:

```rust,ignore
#[tokio::main]
async fn main() -> openshop::Result<()> {
    openshop::cli::run(config).await
}
```

With `openshop::cli::run(config()?)`, the configuration is built before the
command starts and does not see values that only exist in `.env`.

## Resolution priority

### Development `.env`

1. Existing process environment
2. Project-root `.env`
3. Development CLI fallback, where one exists

The `.env` loader does not overwrite an existing process variable and is used by
`dev`, not by `start`, `worker`, or `migrate`.

### Shopify app URL

For a configured app, resolution is:

1. `app_url` of the `ShopifyAppConfig` entry
2. `application_url` in that app's TOML file
3. `HOST`
4. `SHOPIFY_APP_URL`

For single-app mode, resolution is `HOST`, then `SHOPIFY_APP_URL`.

### Shopify scopes

1. `ShopifyConfig::scopes`
2. `scopes` in the selected Shopify TOML file
3. Empty string

The selected TOML file is the app's own file for `ShopifyAppConfig::Toml`
entries. Single-app mode and `ShopifyAppConfig::Keys` entries use the first
`shopify.app.<name>.toml` file in the project root (in name order) that
declares scopes, then `shopify.app.toml`.

When multiple apps are configured without global `ShopifyConfig::scopes`, their
scopes must be identical.

## Database pool sizing

Pool settings apply **per process**. A deployment with one web process and three
workers at the default `PGPOOL_MAX=10` can open up to 40 PostgreSQL connections.
Choose `PGPOOL_MAX` from the database connection limit divided by the maximum number
of simultaneously running web and worker processes, leaving capacity for migrations
and administration.

## Production rules

- Set `OPENSHOP_ENV=production` in production.
- Set a persistent, secret `ENCRYPTION_KEY`; OpenShop refuses encrypted writes
  without one in production.
- Use an externally reachable HTTPS `HOST`.
- Never expose `SHOPIFY_API_SECRET`, `ENCRYPTION_KEY`, or `DATABASE_URL` to browser
  code.
- Give the web and worker processes the same database, encryption key, and
  application binary.

In production mode, `dev` refuses to push the schema and `migrate generate`
refuses to write migrations unless `--allow-generate-in-production` is given.
