# Flows

For a working example, see [Sync orders with a flow](../guides/define-flow.md). For the replay model, read [Checkpoints and retries](../concepts/checkpoints-and-retries.md).

Flows are background jobs dispatched to an OpenShop worker.

```rust,ignore
use std::time::Duration;

use openshop::{Concurrency, Flow, PartialRetryPolicy, json};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, JsonSchema)]
pub struct SyncOrdersInput {
    #[schemars(range(min = 1))]
    pub limit: u32,
}

#[derive(Serialize, Deserialize)]
struct Batch {
    requested: u32,
    prepared_at: String,
}

pub fn sync_orders() -> Flow {
    Flow::new("syncOrders", |ctx| async move {
        let input: SyncOrdersInput = ctx.input()?;

        let batch: Batch = ctx
            .step
            .run("prepare-batch", || async {
                Ok(Batch {
                    requested: input.limit,
                    prepared_at: openshop::chrono::Utc::now().to_rfc3339(),
                })
            })
            .await?;

        ctx.logger.info(
            json!({ "requested": batch.requested, "preparedAt": batch.prepared_at }),
            "Order batch prepared",
        );
        Ok(())
    })
    .input::<SyncOrdersInput>()
    .timeout(Duration::from_secs(60))
    .step_timeout(Duration::from_secs(15))
    .concurrency(Concurrency::Reject)
    .retry_policy(PartialRetryPolicy::new().max_attempts(3))
}
```

## Definition options

| Option | Type | Default | Behavior |
| --- | --- | --- | --- |
| `Flow::new(name, …)` | `impl Into<String>` | Required | Definition name. `.flow(flow)` registers the flow under it. |
| `Flow::new(…, run)` | `Fn(FlowContext) -> impl Future<Output = Result<()>>` | Required | Flow implementation. The closure and its future must be `Send + 'static`. |
| `.input::<T>()` | `T: DeserializeOwned + JsonSchema` | None | Validated in the worker before `run`. Invalid input fails the run. |
| `.input_schema(schema)` | `InputSchema` | None | Same, with a custom JSON Schema and validation function. |
| `.timeout(duration)` | Positive `Duration` | None | Per-attempt execution timeout; also anchors the retry deadline at dispatch time. |
| `.step_timeout(duration)` | Positive `Duration` | None | Default timeout for every step. |
| `.concurrency(mode)` | `Concurrency::Reject` or `Concurrency::Allow` | `Reject` | Controls overlapping active runs for the same app, shop, and flow. |
| `.retry_policy(policy)` | `PartialRetryPolicy` | App policy | Overrides the app-level policy. |

`app.dispatch_flow(…)` returns `DispatchFlowResult { run_id, status: "pending" }`.
Its `delay_ms` and `retry_policy` options override the flow and app values for
that run. See [`app.dispatch_flow`](sdk.md#appdispatch_flowparams).

The key a flow is registered under — not necessarily `flow.name` — is the
runtime identifier used by dispatch, crons, runs, and logs. `.flow(flow)` uses
the name as the key; `.flow_as(key, flow)` sets another one.

## Input

`.input::<T>()` derives a JSON Schema from `T` with `schemars`. In the worker,
the dispatched value must satisfy that schema and deserialize into `T`;
otherwise the run fails with
`Invalid input for flow "<key>": <summary>`, for example
`Invalid input for flow "syncOrders": input.limit must be >= 1`. The schema is
also what `GET /api/flows` and the MCP tool `openshop.flows.list` return as
`inputSchema`.

The schema check understands the keywords listed under
[Input schema validation](mcp.md#input-schema-validation). Constraints
expressed with other keywords are enforced only if deserialization into `T`
enforces them.

`InputSchema::custom(json_schema, parse)` replaces both parts: `parse` receives
the dispatched `Value` and returns the value passed to the flow, or a short
error summary.

Input is validated when a worker starts the run, not at dispatch. A dispatched
input that is not a JSON object is replaced with `{}` before validation.

## Runtime context

| Field | Contract |
| --- | --- |
| `input` | The validated input as a `serde_json::Value`, or the dispatched object when no schema exists. |
| `input::<T>()` | Method that deserializes `input` into `T`. |
| `connectors` | `Connectors`: provider methods bound to the shop's stored configuration. |
| `shopify` | `ShopifyClient` for the run's app and shop. |
| `shop` | Shop domain associated with the run. |
| `shopify_app` | Internal app handle (`default` in single-app mode). |
| `step` | `StepExecutor`, the checkpointed step helper. |
| `logger` | `FlowLogger` with `info`, `warn`, and `error` structured logging methods. |
| `signal` | `CancellationToken` cancelled when the run is canceled. |
| `db` | `PgPool`, the shared `sqlx` pool. |
| `app` | `OpenShop` handle, for example to dispatch another flow. |

`FlowContext` is `Clone`.

## Retry precedence and defaults

Retry settings merge from least to most specific:

1. OpenShop defaults
2. `.retry_policy(…)` on the configuration builder
3. `.retry_policy(…)` on the flow
4. `DispatchOptions::retry_policy` passed to `dispatch_flow`

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `max_attempts` | `u64` | `3` | Total attempts, including the first. |
| `initial_interval_ms` | `u64` | `1_000` | Delay before the first retry. |
| `backoff_coefficient` | `f64` | `2.0` | Exponential multiplier. |
| `max_interval_ms` | `u64` | `30_000` | Delay cap. |

The merged policy is stored on the run at dispatch. The delay before retry
`n` is `initial_interval_ms × backoff_coefficient^(n − 1)`, capped at
`max_interval_ms`.

A retry is not scheduled when `max_attempts` is reached or when its next start
would be at or after the deadline stored at dispatch. Each attempt still uses
the full `timeout` in its execution race. The `on_error` hook runs after each
failed attempt, including attempts that will be retried; a failure or panic
inside the hook is ignored.

A run requeued with the Admin API retry endpoint carries no retry policy: if it
fails again it is marked `failed` without further attempts.

## Concurrency

With the default `Concurrency::Reject` policy, dispatch obtains a PostgreSQL
advisory lock and rejects a second `pending`, `running`, or `sleeping` run for
the same `(shopify_app, shop, flow)`. It fails with a `FlowConcurrencyError`,
whose `existing_run_id` identifies the active run. `Concurrency::Allow` skips
that check.

## Checkpointed steps

```rust,ignore
use std::time::Duration;

use openshop::{FlowContext, Result, StepOptions, Value};

const ORDERS: &str = "query { orders(first: 10) { nodes { id name } } }";

async fn fetch_orders(ctx: &FlowContext) -> Result<Value> {
    ctx.step
        .run_with("fetch-orders", StepOptions::timeout(Duration::from_secs(10)), || async {
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

`f` is a closure returning a future (`|| async { … }`) whose output is
`Result<T>`. It may borrow from the flow, including `ctx`.

Step names must be stable and unique within the logical run. A renamed step is
a new checkpoint.

A step result has one of the statuses `running`, `completed`, `failed`,
`canceled`, or `sleeping`.

## Sleeping without a worker slot

```rust,ignore
use std::time::Duration;

use openshop::{FlowContext, Result};

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
such runs once per `poll_interval_ms`, one second by default.

A panic inside the flow function is caught and handled like a returned error.
The run fails with `Flow panicked: <message>` and is retried according to its
policy.
