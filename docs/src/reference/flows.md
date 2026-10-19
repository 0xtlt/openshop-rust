# Flows

For a working example, see [Sync orders with a flow](../guides/define-flow.md). For the replay model, read [Checkpoints and retries](../concepts/checkpoints-and-retries.md).

Flows are background jobs dispatched to an OpenShop worker. A flow is an
`async` function marked `#[flow]`.

```rust,ignore
use std::time::Duration;

use openshop::prelude::*;
use serde::{Deserialize, Serialize};

#[input]
#[derive(Debug)]
pub struct SyncOrdersInput {
    #[schemars(range(min = 1))]
    pub limit: u32,
}

#[derive(Serialize, Deserialize)]
struct Batch {
    requested: u32,
    prepared_at: String,
}

#[flow(
    "syncOrders",
    timeout = "60s",
    step_timeout = "15s",
    concurrency = reject,
    retry_policy = RetryPolicy::new().max_attempts(3)
)]
pub async fn sync_orders(ctx: FlowContext, input: SyncOrdersInput) -> Result<()> {
    let batch: Batch = ctx
        .step
        .run("prepare-batch", async || {
            Ok(Batch {
                requested: input.limit,
                prepared_at: openshop::chrono::Utc::now().to_rfc3339(),
            })
        })
        .await?;

    ctx.logger.info_with(
        "Order batch prepared",
        json!({ "requested": batch.requested, "preparedAt": batch.prepared_at }),
    );
    Ok(())
}
```

List it in the `flows` of [`app!`](configuration.md): `flows: [sync_orders]`.

## Definition options

The function is `async fn(ctx: FlowContext) -> Result<()>`, or
`async fn(ctx: FlowContext, input: Input) -> Result<()>` for a flow with a
typed input. Its error type is anything that converts to `openshop::Error`. It
cannot be generic.

| Option | Type | Default | Behavior |
| --- | --- | --- | --- |
| `"syncOrders"`, before the other options | String literal | The name of the function in camel case: `sync_orders` is `syncOrders` | Name of the flow: the key used by dispatch, crons, runs, and logs. |
| Second argument of the function | A type marked [`#[input]`](#input) | None | The [input](#input) of a run, validated in the worker before the function is called. Invalid input fails the run. |
| `timeout = …` | A positive duration as text, `"60s"`, `"10m"`, `"1h30m"`, checked when the crate compiles. An expression of type `Duration` works as well. | None | Per-attempt execution timeout; also anchors the retry deadline at dispatch time. |
| `step_timeout = …` | Same as `timeout` | None | Default timeout for every step. |
| `concurrency = …` | `reject` or `allow`, as a word. An expression such as `Concurrency::Allow` works as well. | `reject` | Controls overlapping active runs for the same app, shop, and flow. |
| `retry_policy = …` | `RetryPolicy` | App policy | Overrides the app-level policy. |

The function stays an ordinary function. The attribute adds a type of the same
name, which is what `flows` lists, with the name of the flow as the constant
`sync_orders::NAME` and with [`sync_orders::dispatch`](#dispatch), which
queues a run. Because of that type, do not re-export the function next
to a module of the same name (`pub use sync_orders::sync_orders;` beside
`mod sync_orders;` does not compile): import it where the application is
declared.

## Input

The second argument of the function is the input of a run. Its type is marked
`#[input]`:

```rust,ignore
use openshop::prelude::*;

#[input]
#[derive(Debug)]
pub struct CountVariantsInput {
    #[schemars(length(min = 1))]
    pub product_id: String,
}

#[flow]
pub async fn count_variants(ctx: FlowContext, input: CountVariantsInput) -> Result<()> {
    ctx.logger.info(format!("Counting the variants of {}", input.product_id));
    Ok(())
}
```

`#[input]` derives `Deserialize`, `Serialize` and `JsonSchema` for the type,
without `serde` or `schemars` among the dependencies of the application.

- Fields are named in camel case outside Rust: `product_id` is `productId` in
  the dispatched JSON and in the schema. A `#[serde(rename_all = "…")]` on the
  type states another rule.
- `#[serde(…)]` and `#[schemars(…)]` attributes work on the type and on its
  fields.
- `#[input]` goes above the other attributes of the type, `#[derive(…)]`
  included.
- The same attribute marks the input of an [MCP tool](mcp.md) and of a loader
  or an action of a [custom admin page](custom-admin-pages.md).
- The payload of a [webhook](webhooks.md) is not an input: Shopify names its
  fields in snake case, and a plain `#[derive(Deserialize)]` reads it.

`#[input]` is the short form. The function accepts any type that implements
`Deserialize` and `JsonSchema`; `dispatch` needs `Serialize` as well.

The type gives the flow a JSON Schema. In the worker, the dispatched value
must satisfy that schema and deserialize into the type; otherwise the run
fails with `Invalid input for flow "<key>": <summary>`, for example
`Invalid input for flow "syncOrders": input.limit must be >= 1`. The schema is
also what `GET /api/flows` and the MCP tool `openshop.flows.list` return as
`inputSchema`. A function without a second argument declares a flow without a
schema.

The schema check understands the keywords listed under
[Input schema validation](mcp.md#input-schema-validation). Constraints
expressed with other keywords are enforced only if deserialization into the
type enforces them.

Input is validated when a worker starts the run, not at dispatch. A dispatched
input that is not a JSON object is replaced with `{}` before validation. The
`input` of a [cron](configuration.md#crons) is also checked when the
configuration is built.

## Dispatch

`sync_orders::dispatch(&ctx, input)` queues a run for the shop and the Shopify
app of a context:

```rust,ignore
use openshop::prelude::*;

use crate::flows::sync_orders::{SyncOrdersInput, sync_orders};

#[webhook("orders/create")]
pub async fn orders_create(ctx: WebhookContext) -> Result<()> {
    sync_orders::dispatch(&ctx, SyncOrdersInput { limit: 10 }).await?;
    Ok(())
}
```

`ctx` is any context that belongs to a shop: the context of a flow, of a
webhook, of an app proxy, of an admin function, of an MCP tool or resource, or
what `ctx.for_shop(…)` returns in a server route. A flow without input is
dispatched with the context alone: `nightly_cleanup::dispatch(&ctx)`. Both
return `QueuedRun { run_id, status: "pending" }`.

`app.dispatch_flow(DispatchOptions::new(name, shop)…)` queues a flow only
known by its name, or a run with settings of its own: `.delay(…)` postpones the
run, and `.retry_policy(…)` overrides the flow and app values for that run.
See [`app.dispatch_flow`](sdk.md#appdispatch_flowoptions).

## Runtime context

| Field | Contract |
| --- | --- |
| `input` | The validated input as a `serde_json::Value`, or the dispatched object when no schema exists. A typed input is the same value, as the second argument of the function. |
| `input::<T>()` | Method that deserializes `input` into `T`. |
| `connectors` | `Connectors`: provider methods bound to the shop's stored configuration, read at the first call. |
| `shopify` | `ShopifyClient` for the run's app and shop. The access token is read at the first request. |
| `shop` | Shop domain associated with the run. |
| `shopify_app` | Internal app handle (`default` in single-app mode). |
| `step` | `StepExecutor`, the checkpointed step helper. |
| `logger` | `FlowLogger`, the structured log of the run. See below. |
| `signal` | `CancellationToken` cancelled when the run is canceled. |
| `db` | `PgPool`, the shared `sqlx` pool. |
| `app` | `OpenShop` handle. |

`FlowContext` is `Clone`. It implements `ShopScope`, like every context that
belongs to a shop: `other_flow::dispatch(&ctx, …)` queues another flow for the
same shop, and `Review::of(&ctx)` gives the rows of a
[model](database.md#app-models) for it.

The logger takes the message first, and a JSON payload with the `_with`
methods:

| Method | |
| --- | --- |
| `ctx.logger.info(message)` | A message. |
| `ctx.logger.info_with(message, json!({ … }))` | A message and its payload. |
| `warn(message)`, `warn_with(message, payload)` | The same, at the `warn` level. |
| `error(message)`, `error_with(message, payload)` | The same, at the `error` level. |

`message` is anything that reads as a string: a literal, or a `format!(…)`.
See [Logging](logging.md).

## Retry precedence and defaults

Retry settings merge from least to most specific:

1. OpenShop defaults
2. the `retry_policy` setting of the application
3. `retry_policy` on the flow
4. the `.retry_policy(…)` of the `DispatchOptions` passed to `dispatch_flow`

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `max_attempts` | `u64` | `3` | Total attempts, including the first. |
| `initial_interval` | `Duration` | 1 second | Delay before the first retry. |
| `backoff_coefficient` | `f64` | `2.0` | Exponential multiplier. |
| `max_interval` | `Duration` | 30 seconds | Delay cap. |

Each field has a builder method of the same name on `RetryPolicy`:
`RetryPolicy::new().max_attempts(3).initial_interval(Duration::from_secs(1))`.
A field left unset keeps the value of the level before it.

The merged policy is stored on the run at dispatch. The delay before retry
`n` is `initial_interval × backoff_coefficient^(n − 1)`, capped at
`max_interval`.

A retry is not scheduled when `max_attempts` is reached or when its next start
would be at or after the deadline stored at dispatch. Each attempt still uses
the full `timeout` in its execution race. The `on_error` hook runs after each
failed attempt, including attempts that will be retried; a failure or panic
inside the hook is ignored.

Every time a worker takes a run, it counts as an attempt, except when the run
wakes up from `ctx.step.sleep`: a sleep is not a failure, so a flow can sleep as
often as it needs to, even with `max_attempts(1)`. If a worker dies without
recording the outcome of an attempt, the run is taken again once its lease has
expired. When that attempt was the last one, the run is marked `failed` with the
error `Run abandoned: …` instead, so a flow that takes its worker down is not
taken forever.

A run requeued with the Admin API retry endpoint carries no retry policy: if it
fails again, or if its worker dies, it is marked `failed` without further
attempts.

Only the worker that took a run last can record its outcome. If the lease of a
slow execution expired and the run was taken again, even by the same worker, the
old execution is told to stop at its next heartbeat, and what it returns is
ignored.

## Concurrency

With the default `Concurrency::Reject` policy, dispatch obtains a PostgreSQL
advisory lock and rejects a second `pending`, `running`, or `sleeping` run for
the same `(shopify_app, shop, flow)`. It fails with a `FlowConcurrencyError`,
whose `existing_run_id` identifies the active run. `Concurrency::Allow`,
written `concurrency = allow` in `#[flow(…)]`, skips that check.

## Checkpointed steps

```rust,ignore
use std::time::Duration;

use openshop::prelude::*;
use openshop::StepOptions;

const ORDERS: &str = "query { orders(first: 10) { nodes { id name } } }";

async fn fetch_orders(ctx: &FlowContext) -> Result<Value> {
    ctx.step
        .run_with("fetch-orders", StepOptions::timeout(Duration::from_secs(10)), async || {
            ctx.shopify.graphql(ORDERS, None).await
        })
        .await
}
```

`ctx.step.run(name, f)` and `ctx.step.run_with(name, options, f)`:

- return a previously completed step's stored output without calling `f`;
- use `options.timeout` before the flow's `step_timeout`;
- require an output type that is `Serialize + DeserializeOwned`, because the
  output is stored as JSON and read back on replay. `()` is stored as `null`;
- store failures per run attempt and return the error. Propagate it with `?`
  to fail the attempt and let the flow retry;
- return an error on replay when the stored output no longer deserializes into
  the requested type.

`f` is an async closure, `async || { … }`, whose output is `Result<T>`. It may
borrow from the flow, including `ctx`.

Step names must be stable and unique within the logical run. A renamed step is
a new checkpoint.

A step result has one of the statuses `running`, `completed`, `failed`,
`canceled`, or `sleeping`.

## Sleeping without a worker slot

```rust,ignore
use std::time::Duration;

use openshop::prelude::*;

async fn wait_for_rate_limit(ctx: &FlowContext) -> Result<()> {
    ctx.step.sleep("rate-limit-window", Duration::from_secs(30)).await
}
```

The first call records a resume timestamp and returns an error carrying a
`SleepSignal`. Propagate it with `?`: the run moves to `sleeping` and the
worker slot is released. When a worker picks the run up after that timestamp,
the flow function runs again from the top, completed steps return their stored
output, the sleep checkpoint completes, and execution continues. Retries reuse
the stored timestamp rather than adding another full delay.

Do not handle the error returned by `sleep` as an application failure.

## Cancellation and timeouts

Canceling a run cancels `ctx.signal`. A step that is executing is interrupted
at its next `.await` and recorded as `canceled`; `run`, `run_with`, and `sleep`
fail with `FlowCanceledError` when they are called after cancellation. The run
is marked `canceled`. Cancellation also reaches runs executing in another
process: the worker notices on its next heartbeat, which runs every 15
seconds with the default lease.

Flow and step timeouts fail with `FlowTimeoutError` or `StepTimeoutError` and
drop the future that was executing, which stops at its current `.await`. A
step that was executing when its flow timed out is recorded as `failed` with
the error `Step was interrupted`.
Dropping a future cannot interrupt synchronous code between two awaits, a
blocking call, or a task started with `tokio::spawn`. For such work, pass
`ctx.signal` along and stop when `signal.cancelled().await` resolves or
`signal.is_cancelled()` returns `true`.

A flow with a `timeout` also gets a deadline at dispatch: the dispatch time
plus the timeout. Workers never start a run past its deadline, and mark any
`pending`, `running`, or `sleeping` run past it as `failed` with
`Flow timed out — exceeded maximum allowed duration`. Each worker looks for
such runs once per `poll_interval`, one second by default.

A panic inside the flow function is caught and handled like a returned error.
The run fails with `Flow panicked: <message>` and is retried according to its
policy.

## Without the attribute

`#[flow]` is built on `Flow`, which can be used directly when the name or the
body of a flow is only known at run time, and in tests:

```rust,ignore
use std::time::Duration;

use openshop::prelude::*;
use openshop::builder::Flow;

pub fn export(resource: &'static str) -> Flow {
    Flow::new(format!("export-{resource}"), move |ctx| async move {
        ctx.logger.info_with("Exporting", json!({ "resource": resource }));
        Ok(())
    })
    .timeout(Duration::from_secs(60))
}
```

Register it on the [builder](configuration.md#builder-methods), for example
with `APP.builder()?.flow(export("orders"))`. `.flow(flow)` registers the flow
under `flow.name`; `.flow_as(key, flow)` sets another key, which is then the
runtime identifier used by dispatch, crons, runs, and logs.

| Option | Type | Default | Behavior |
| --- | --- | --- | --- |
| `Flow::new(name, …)` | `impl Into<String>` | Required | Definition name. `.flow(flow)` registers the flow under it. |
| `Flow::new(…, run)` | `Fn(FlowContext) -> impl Future<Output = Result<()>>` | Required | Flow implementation. The closure and its future must be `Send + 'static`. It reads the input with `ctx.input()`. |
| `.input::<T>()` | `T: DeserializeOwned + JsonSchema` | None | Validated in the worker before `run`. Invalid input fails the run. |
| `.input_schema(schema)` | `InputSchema` | None | Same, with a custom JSON Schema and validation function. |
| `.timeout(duration)` | Positive `Duration` | None | Same as `timeout` of the attribute. |
| `.step_timeout(duration)` | Positive `Duration` | None | Same as `step_timeout` of the attribute. |
| `.concurrency(mode)` | `Concurrency::Reject` or `Concurrency::Allow` | `Reject` | Same as `concurrency` of the attribute. |
| `.retry_policy(policy)` | `RetryPolicy` | App policy | Same as `retry_policy` of the attribute. |

`InputSchema::custom(json_schema, parse)` replaces both parts of the
[input](#input) validation: `parse` receives the dispatched `Value` and returns
the value passed to the flow, or a short error summary.
