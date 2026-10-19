# Logging

OpenShop has two logging surfaces:

- flow logs written through `ctx.logger` and stored in PostgreSQL;
- runtime logs written by the web process, worker, scheduler, and server.

## Flow logger

```rust,ignore
use openshop::prelude::*;

#[flow("syncOrders")]
pub async fn sync_orders(ctx: FlowContext) -> Result<()> {
    ctx.logger.info("Starting order sync");

    ctx.step
        .run("push", async || {
            if let Err(error) = ctx.connectors.call("warehouse", "push", json!([])).await {
                ctx.logger.error_with("Provider push failed", json!({ "error": format!("{error:#}") }));
                return Err(error);
            }
            ctx.logger.info_with("Orders pushed", json!({ "source": "shopify" }));
            Ok(())
        })
        .await
}
```

`ctx.logger` is a `FlowLogger`. The message comes first:

| Method | Logs |
| --- | --- |
| `info(message)`, `warn(message)`, `error(message)` | A line. |
| `info_with(message, payload)`, `warn_with(message, payload)`, `error_with(message, payload)` | A line with data the admin shows next to it. |

The message is a `&str` or a `String`, such as the result of `format!`. The
payload is a JSON value (`serde_json::Value`), usually
`json!({ "count": 3 })`; a line logged without one is stored with an empty
object.

Each line is sent to the runtime logger as `[LEVEL] message` and stored in the
`logs` table with the time it was emitted. Lines are written in emission order
by a background task:
logging never blocks or fails a flow. A line waits up to 50 ms for the next
ones, which are written with it in one statement, and the lines a flow logged
are stored before the outcome of its run is. `flush().await` writes the lines
logged so far without waiting.

Logs are associated with the current run and appear in the embedded admin. The
Admin API supports level/text filters, context expansion, and JSON or CSV
export. The run-log endpoint returns the complete filtered set and does not
provide pagination.

The engine adds its own lines to each run: flow start, completion, sleep,
cancellation, and failure, and step start, completion, failure, and cache hits.

When the application reports to [Sentry](sentry.md), the lines of a run are
also the breadcrumbs of the failure it reports: their level and their
message, without their payload.

Do not log access tokens, provider passwords, session tokens, OAuth state, or
personal data that is not required for diagnosis.

## Log query syntax

The `q` parameter of the run-log endpoints, the search box of the admin UI, and
the MCP tool `openshop.logs.search` share one query syntax. A query combines
any of these terms:

- free text — keeps lines that contain the text;
- `|= "text"` — keeps lines that contain the text;
- `!= "text"` — drops lines that contain the text;
- `|~ "pattern"` — keeps lines that match the regular expression;
- `C:n` — also returns `n` lines before and after each match;
- `B:n`, `A:n` — also returns the `n` lines listed before, or after, each match;
- `last:15m` — keeps lines newer than a duration, with the units `s`, `m`, `h`, and `d`;
- `from:<date>`, `to:<date>` — keeps lines after, or before, a date;
- `between:<from>,<to>` — both bounds at once.

All terms must hold. Matching is case-insensitive and runs against the message
followed by the top-level `key=value` pairs of an object payload, for example
`Fetching orders limit=10`. Values of nested objects are not searchable.

Regular expressions are limited to 120 characters and must not contain
backreferences, nested quantifiers such as `(a+)+`, or repeated `.*`. A pattern
that is rejected or invalid matches nothing.

Dates are RFC 3339 timestamps, `YYYY-MM-DDTHH:MM[:SS]` (read as UTC), or
`YYYY-MM-DD`. Lines are listed newest first, and context lines are taken from
that list after the level and time filters.

## Runtime logger

Applications that embed OpenShop logging into another logger can install the
public runtime adapter:

```rust,ignore
use std::sync::Arc;

use openshop::prelude::*;
use openshop::{RuntimeLogger, set_logger};

struct StderrLogger;

impl RuntimeLogger for StderrLogger {
    fn info(&self, message: &str, context: Option<&Value>) {
        eprintln!("info {message} {context:?}");
    }

    fn warn(&self, message: &str, context: Option<&Value>) {
        eprintln!("warn {message} {context:?}");
    }

    fn error(&self, message: &str, context: Option<&Value>) {
        eprintln!("error {message} {context:?}");
    }
}

fn install_logger() {
    set_logger(Arc::new(StderrLogger));
}
```

Call `openshop::set_logger(logger)` before starting OpenShop processes, for
example at the top of `main`. It returns the previous logger.
`openshop::logger()` returns the active adapter.

The default adapter forwards to the [`tracing`](https://docs.rs/tracing) crate
at the matching level, with the context as a `context` field.
`APP.main()` and `APP.run()` call `openshop::cli::init_logging()`, which installs a
`tracing` formatter that writes to standard output unless the process already
has a global subscriber. Its filter is the `RUST_LOG` environment variable,
or `info,sqlx=warn` when the variable is unset: `info` and above, with `sqlx`
statement logging limited to warnings.

```bash
RUST_LOG=debug cargo run -- dev
```

To send runtime logs elsewhere, either install your own `tracing` subscriber
before calling `APP.main()`, or replace the adapter with
`openshop::set_logger`. Startup banners of the CLI commands are printed
directly to standard output.

## Retention and export

OpenShop keeps flow logs forever unless the application sets
[`log_retention`](configuration.md#settings), the age after which the workers
remove a log line:

```rust
openshop::app! {
    // …
    log_retention: std::time::Duration::from_secs(30 * 24 * 3600),
}
```

A worker (`dev`, `start`, and `worker`) removes the lines older than that once
an hour, 5,000 at a time with a short pause between two statements, and at
most 1,000,000 per sweep: a larger backlog goes on at the next sweep. Several
workers can purge at the same time. Only the `logs` table is purged; runs and
step results stay. The default is to keep everything, since removing data is
not something an upgrade should start doing: set a retention that fits your
needs, and plan backups and deletion with the rest of your policy. Deleting a
run through the Admin API deletes its logs.

The logs of a run are read and exported without holding the run in memory. The
logs endpoint returns the newest 1,000 lines by default, 5,000 at most, with a
`next` cursor for the following page; the export streams the whole run. See
[Run logs](admin-api.md#run-logs). Export is intended for diagnosis, not as a
durable analytics pipeline.
