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
| A webhook handler returns an error | `error` | `webhook <topic>` | `webhook`, `shop`, `shopify_app` |
| A proxy handler returns an error | `error` | `proxy <METHOD> <path>` | `proxy`, `shop`, `shopify_app` |
| The handler of a server route, or its authentication, returns an error | `error` | `route <METHOD> <path>` | `route` |
| A loader or an action of a custom admin page fails with an error that is not an `AdminPublicError` | `error` | `admin <page> <function>` | `admin`, `shop`, `shopify_app`, `request_id` |
| The Admin API answers `500 Internal Server Error` | `error` | `<METHOD> <path>` | `shop`, `shopify_app` |
| The MCP endpoint cannot check a token and answers `500` | `error` | `POST /mcp` | — |
| A panic, anywhere in the process | `fatal` | The unit of work it happened in | The tags of that unit of work |
| `dev`, `start`, or `worker` stops with an error | `error` | — | — |

Every event also carries the tag `process`, the command that reports it
(`dev`, `start`, or `worker`), and the environment and the release of the
[settings](#settings).

A flow run is reported once per failed attempt, like the
[`on_error` hook](errors.md#application-error-hook) is called. The paths of
proxies, server routes, and admin pages are the ones they are declared with,
such as `/orders/:id`: the path of the request, which can carry a token, is
not sent. `request_id` is the `requestId` of the error a custom admin page
shows: it finds the event of a failure a merchant describes.

A panic is reported where it happens, with its stack, by the panic hook of the
Sentry SDK. OpenShop catches the panics of flows and handlers and records them
as failures; that failure is not reported a second time. The hook waits for
the event of a panic to be sent, for up to two seconds, before the thread goes
on: a process that panics and exits still reports why.

## What is not reported

- A run that is canceled, or that sleeps.
- What a handler answers on purpose: `InvalidInput` (`422`), an
  `AdminPublicError`, an authentication that refuses a request.
- An error returned by an MCP tool or resource. It is answered to the client
  and kept in the MCP audit log.
- A provider check that fails in the admin.
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
  The payload of a line is not sent.
- **Nothing of the request.** No header, no query string, no body, and no
  input of a flow.
- **The process.** The host name, the operating system, and the Rust version,
  which the SDK adds to every event.

The message of an error is sent as it is. Write errors the way you write log
lines: without an access token, a password, or personal data.

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
| `.environment(name)` | The environment of the events | `production` with `OPENSHOP_ENV=production`, `development` otherwise |
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

Dropping the guard sends what is queued, then reports nothing more. Start
Sentry before the server and the workers: a thread that used the SDK before
it was started stays without a client.

## Transport and Cargo feature

Events are sent with the HTTP client of the framework, from a thread of its
own: a report is never awaited by what fails. When Sentry answers with a rate
limit, events are dropped until the delay it gave has passed. When more than
30 events wait to be sent, the next ones are dropped.

The integration is the `sentry` feature of `openshop`, on by default. An
application that sets `default-features = false` compiles without the Sentry
SDK, and without the `sentry` setting.
