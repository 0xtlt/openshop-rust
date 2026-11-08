# Logging

OpenShop has three logging surfaces:

- flow logs written through `ctx.logger` and stored in PostgreSQL;
- the lines of a webhook handler or of an admin page, written through their
  own `ctx.logger` and sent to the runtime logger with the shop they run for;
- runtime logs written by the web process, worker, scheduler, and server, and
  by the application through `tracing`, which `openshop` re-exports.

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

At most 10,000 lines wait to be written. A flow that logs faster than the
database takes them loses the newest lines instead of slowing down, and the run
gets one `warn` line saying how many (`1500 log lines were dropped`). A write
that fails is told to the runtime logger with the run and the number of lines
lost. A statement holds at most 1,000 lines or 1 MiB of text, however fast the
flow logs. A NUL byte, which PostgreSQL cannot store, is replaced by U+FFFD in a
message and in the strings and keys of a payload; keys that become equal get
`#2`, `#3`… after them.

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

## Handler logger

A webhook handler and the loaders, actions and access rules of an admin page
have a `ctx.logger` too, a `ContextLogger`, with the methods of the flow
logger:

```rust,ignore
use openshop::prelude::*;

#[webhook("orders/create")]
pub async fn orders_create(ctx: WebhookContext) -> Result<()> {
    ctx.logger.info_with("Order received", json!({ "id": ctx.payload["id"] }));
    Ok(())
}
```

A handler has no run: its lines are not stored in the database and the admin
does not show them. Each one goes to the runtime logger as `[LEVEL] message`,
with its payload and what the handler runs for:

| Context | Fields added to the payload |
| --- | --- |
| `WebhookContext` | `shop`, `shopifyApp`, `topic` |
| `AdminContext` | `shop`, `shopifyApp`, `requestId` (the one of the error response and of the line OpenShop logs for the call) |

A key of the payload wins over a field of the same name, and a payload that is
not an object is logged under `payload`. When the application reports to
[Sentry](sentry.md), the lines are the breadcrumbs of a failure of the handler,
without their payload, as the lines of a run are.

A context built by hand, in a test, takes `ContextLogger::new(json!({ … }))`,
whose fields are added to every line, or `ContextLogger::default()`.

## `tracing`

`openshop::tracing` is the [`tracing`](https://docs.rs/tracing) crate OpenShop
logs with, and the prelude names it: an application logs to the runtime logger
without a dependency of its own.

```rust,ignore
use openshop::prelude::*;

pub fn report(shop: &str, synced: usize) {
    tracing::info!(shop, synced, "Orders synced");
}
```

Its lines go through the subscriber `APP.main()` installs (see
[Runtime logger](#runtime-logger)), with the filter of `RUST_LOG`. Prefer
`ctx.logger` in a flow, whose lines are kept with its run, and in a handler,
whose lines say what they ran for.

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

All terms must hold. A context count above 100 is read as 100. Matching is case-insensitive and runs against the message
followed by the top-level `key=value` pairs of an object payload, for example
`Fetching orders limit=10`. Values of nested objects are not searchable.

Regular expressions are limited to 120 characters and must not contain
backreferences, nested quantifiers such as `(a+)+`, or repeated `.*`. A pattern
that is rejected, invalid, or compiles to more than 1 MiB fails the whole
query: the Admin API answers `400` with the reason, and the MCP tool answers
an error. A query is at most 2,000 characters and holds at most 20 filters. A
`last:` that reaches back further than the calendar fails the query the same
way.

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
