# Sentry

OpenShop reports what fails to [Sentry](https://sentry.io): the runs of a
flow, the handlers of the server, and the panics. Each event carries the flow
or the route it comes from, and the shop it happened for.

It is off by default. Without a DSN, nothing is reported and nothing is sent.

## Turn it on

The `sentry` setting of `app!` takes the DSN of the Sentry project, as an
`Option<String>`. The application reads it from wherever it keeps its
settings: OpenShop does not read a variable of its own.

```rust,ignore
use openshop::prelude::*;

static APP: App = app! {
    flows: [sync_orders],
    sentry: std::env::var("SENTRY_DSN").ok(),
};
```

`SENTRY_DSN` is the name the `openshop init` template uses; it is a variable
of the application, read by its own code.

| `sentry` | Effect |
| --- | --- |
| Absent, `None`, empty, or only whitespace | Nothing is reported. |
| A DSN | `dev`, `start`, and `worker` report to that project for as long as they run. |
| Text that is not a DSN | The application does not start: `[openshop] Invalid config: sentry.dsn is not a valid DSN: …`. |

`migrate` reports nothing. When a process stops, it sends what is still
queued, waiting up to two seconds.

## What is reported

| Failure | Level | Named | Tags |
| --- | --- | --- | --- |
| An attempt of a flow run fails and will be retried | `warning` | `flow <name>` | `flow`, `shop`, `shopify_app`, `run_id`, `attempt`, `will_retry` = `true` |
| The last attempt of a flow run fails | `error` | `flow <name>` | The same, with `will_retry` = `false` |
| A run fails without an attempt: it passed its deadline, or its flow is not registered in the worker that claimed it | `error` | `flow <name>` | `flow`, `shop`, `shopify_app`, `run_id` |
| An attempt cannot be made, or its outcome cannot be recorded, for example when the database fails | `error` | `flow <name>` | `flow`, `shop`, `shopify_app`, `run_id` |
| A cron cannot queue its runs, or cannot list the shops it runs for | `error`, once each time the cron fires | `cron <flow>` | `cron`, `shops_missed`, and `shop`, `shopify_app` when one shop got no run |
| A webhook handler returns an error | `error` | `webhook <topic>` | `webhook`, `shop`, `shopify_app` |
| A proxy handler returns an error | `error` | `proxy <METHOD> <path>` | `proxy`, `shop`, `shopify_app` |
| The handler of a server route, or its authentication, returns an error | `error` | `route <METHOD> <path>` | `route` |
| A loader or an action of a custom admin page fails with an error that is not an `AdminPublicError` | `error` | `admin <page> <function>` | `admin`, `shop`, `shopify_app`, `request_id` |
| The Admin API or the operator dashboard answers `500 Internal Server Error` | `error` | `<METHOD> <route>`, such as `GET /api/runs/{id}` | `shop`, `shopify_app` for the Admin API |
| The MCP endpoint cannot check a token and answers `500` | `error` | `POST /mcp` | — |
| A panic, anywhere in the process | `fatal`, or `warning` for an attempt of a flow run that will be retried | The unit of work it happened in | The tags of that unit of work |
| `dev`, `start`, or `worker` stops with an error | `error` | — | — |

Every event also carries the tag `process`, the command that reports it
(`dev`, `start`, or `worker`), and the environment and the release of the
[settings](#settings).

A flow run is reported once per failed attempt, like the
[`on_error` hook](errors.md#application-error-hook) is called. An event is
named after a route as it is declared, such as `/orders/:id` for a server
route or `/api/runs/{id}` for the Admin API: the path of a request, which can
carry an identifier or a token, is never sent. `request_id` is the
`requestId` of the error a custom admin page shows: it finds the event of a
failure a merchant describes.

A panic is reported where it happens, with its stack, by a panic hook of the
framework. OpenShop catches the panics of flows and handlers and records them
as failures; that failure is not reported a second time. The panic of an
attempt of a flow run is its failure: it carries `attempt` and `will_retry`,
and is a `warning` while the run will be retried, like an error is. The hook
does not wait for the event to be sent, so a handler that panics does not
hold a thread of the server, except in a binary built with
`panic = "abort"`, where the process stops with the panic. An application
that adds the `PanicIntegration` of the SDK with `configure` gets the hook of
the SDK instead, which waits, and reports every panic as `fatal`.

A cron that fires for many shops and can queue none of their runs is one
event, not one per shop: `shops_missed` says how many, and the error is the
one of the first.

## What is not reported

- A run that is canceled, or that sleeps.
- What a handler answers on purpose: `InvalidInput` (`422`), an
  `AdminPublicError`, an authentication that refuses a request.
- An error returned by an MCP tool or resource. It is answered to the client
  and kept in the MCP audit log.
- A provider check that fails in the admin.
- A cron whose flow refuses a second run while one is active
  (`concurrency = reject`): the flow does what it declares.
- The warnings of the [runtime logger](logging.md#runtime-logger), such as a
  worker that cannot reach the database to claim runs.
- A line logged with `ctx.logger.error(…)`. It is part of the trail of its
  run, described below.

## What an event carries

- **The shop, as its user.** An issue in Sentry counts the users it reaches:
  for an OpenShop application, the shops.
- **The log lines of the run.** The lines a run logs with `ctx.logger`, and
  the ones the engine adds for its steps, are the breadcrumbs of what the run
  reports: the level and the message of the last 100 lines of the attempt.
  The payload of a line is not sent. The lines a webhook handler or an admin
  page logs with its `ctx.logger` are the breadcrumbs of what it reports, the
  same way (categories `webhook` and `admin`, `flow` for a run).
- **Nothing of the request.** No header, no query string, no body, and no
  input of a flow.
- **The process.** The host name, the operating system, and the Rust version,
  which the SDK adds to every event.

The message of an error is sent as it is, with one exception: the URLs in
it. The error of an HTTP client or of a database names the URL it failed on,
in a message nobody chose to write. In a message, in the message of a cause,
and in a log line, a URL is sent without:

| Part of a URL | Sent as |
| --- | --- |
| Its credentials, up to the last `@` | `postgresql://[redacted]@host/db` |
| Its query string, up to the next space | `https://api.example.com/orders?[redacted]` |
| A fragment that holds values, such as `#access_token=…` | `https://app.example.com/callback#[redacted]` |
| A part of its path of 20 characters or more that mixes letters and digits, which is what the token of a webhook URL looks like; an identifier written as a UUID is kept | `https://hooks.slack.com/services/T024BE7LD/B08LX2K4P/[redacted]` |

When in doubt, more is removed than less: what follows a `?` goes with it up
to the next space, a closing parenthesis or quote included. This also applies
to what the application captures itself, wherever an event carries text: the
tags, the extra data, every kind of context (OpenTelemetry included), the data of the breadcrumbs, and the
parameters of a log entry are scrubbed like a message. A request an application
attaches keeps its method and its address scrubbed as above; its query string,
cookies and body are replaced by `[redacted]`, and so is the value of a header
or an environment entry whose name says that it is a secret (it contains
`authorization`, `cookie`, `token`, `secret`, `password`, `api-key`, `hmac` or
`signature`).

Nothing else is removed. A shorter secret in a path, a token quoted outside a
URL, or a password in a message is sent as written: write errors the way you
write log lines, without an access token, a password, or personal data, and
drop or rewrite an event with `before_send` (see [Settings](#settings)) when a
library does it for you.

## Settings

`SentryConfig` names more than the DSN:

```rust,ignore
use openshop::prelude::*;

static APP: App = app! {
    flows: [sync_orders],
    sentry: SentryConfig::new(std::env::var("SENTRY_DSN").ok())
        .environment("staging")
        .release(env!("CARGO_PKG_VERSION")),
};
```

| Method | Value | Default when not called |
| --- | --- | --- |
| `SentryConfig::new(dsn)` | `Option<String>` or `String` | — |
| `.environment(name)` | The environment of the events | `production` with `OPENSHOP_ENV=production` or `NODE_ENV=production`, `development` otherwise |
| `.release(name)` | The release of the events: a version, the hash of a commit | No release |
| `.configure(\|options\| …)` | A function from the `ClientOptions` of the Sentry SDK to the options to use | The defaults of the SDK |

`configure` reaches every other option of the SDK, with the methods of its
`ClientOptions`. This application reports only the last attempt of a run, and
half of its events:

```rust,ignore
sentry: SentryConfig::new(std::env::var("SENTRY_DSN").ok()).configure(|options| {
    options.sample_rate(0.5).before_send(|event| {
        let retried = event.tags.get("will_retry").is_some_and(|retried| retried == "true");
        (!retried).then_some(event)
    })
}),
```

OpenShop reads no variable for Sentry: `SENTRY_ENVIRONMENT` and
`SENTRY_RELEASE`, which the SDK reads when it is started with `sentry::init`,
have no effect unless the application passes them to `environment` and
`release`.

## Report from your own code

`openshop::sentry` is the Sentry SDK that OpenShop reports with. Inside a
flow or a handler, what the application captures joins the scope of the unit
of work: the same name, the same tags, the same breadcrumbs.

```rust,ignore
use openshop::prelude::*;
use openshop::sentry;

#[flow("syncOrders")]
pub async fn sync_orders(ctx: FlowContext) -> Result<()> {
    sentry::configure_scope(|scope| scope.set_tag("warehouse", "paris"));
    sentry::capture_message("An order has no shipping line", sentry::Level::Warning);
    Ok(())
}
```

Use this re-export, and do not add the `sentry` crate to `Cargo.toml`:
another version of it would have a client of its own, which nothing starts.

## Stack traces

A panic carries the stack it happened on. An error carries one only when the
process runs with `RUST_BACKTRACE=1` (or `RUST_LIB_BACKTRACE=1`), which makes
`anyhow` capture a backtrace each time an error is created. Without it, an
error is reported with its message and the chain of its causes.

The `release` profile of the `openshop init` template strips debug
information: frames then name functions, without files or lines.

## A process that starts the server itself

`APP.main()` and `APP.run()` start Sentry. A process that calls
`openshop::server::listen` or builds a `Worker` itself starts it first, and
keeps the guard for as long as it runs:

```rust,ignore
let config = APP.config()?;
let production = openshop::runtime::RuntimeEnv::from_env().production;
let _sentry = config.sentry.as_ref().map(|sentry| sentry.init(production));
```

Dropping the guard sends what is queued, then reports nothing more. What
the framework reports always goes to the client the process has now, whatever
thread it runs on, and whether Sentry was started before or after that
thread. What the application captures itself with `openshop::sentry` outside
a flow or a handler follows the SDK: a thread keeps the client it first saw.
Start Sentry before the server and the workers.

`sentry.client_options(production)` returns the options `init` starts the
client with, for a process that binds the client itself. A test passes them
to `openshop::sentry::test::with_captured_events_options`, which keeps the
events instead of sending them.

## Transport and Cargo feature

Events are sent with the HTTP client of the framework, from a thread of its
own: a report is never awaited by what fails. When Sentry answers with a rate
limit, what it limits is dropped until the delay it gave has passed: events,
or the transactions of an application that turned tracing on, each on its
own. When more than 30 envelopes wait to be sent, the next ones are dropped.
A process that stops waits for what is queued for two seconds at most,
however full the queue is.

The integration is the `sentry` feature of `openshop`, on by default. An
application that sets `default-features = false` compiles without the Sentry
SDK, and without the `sentry` setting.

With the feature, the server notes the route each request matched, to name
its events, and looks for a client to report to: an extension of the request
and a lookup, whether or not the application gives a DSN. With a DSN, each
request also gets a scope of its own.
