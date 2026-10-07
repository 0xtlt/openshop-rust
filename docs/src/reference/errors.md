# Errors

Fallible OpenShop functions return `openshop::Result<T>`, an alias of
`anyhow::Result<T>`. The engine's typed errors travel inside `anyhow::Error`
and are recovered with `downcast_ref`.

## Flow errors

| Error | Fields | Message | Meaning | Recovery |
| --- | --- | --- | --- | --- |
| `FlowConcurrencyError` | `flow_name`, `shop`, `existing_run_id` | `Flow "<flow>" is already running for shop "<shop>" (run #<id>)` | A `pending`, `running`, or `sleeping` run already exists for the same app, shop, and flow while concurrency is `Reject`. | Reuse or wait for the existing run, or opt into `Concurrency::Allow` when parallel execution is safe. |
| `StepTimeoutError` | `step_name`, `timeout_ms` | `Step "<step>" timed out after <n>ms` | A step exceeded its effective timeout. | Shorten the operation or increase the step timeout. |
| `FlowTimeoutError` | `flow_name`, `timeout_ms` | `Flow "<flow>" timed out after <n>ms` | An attempt exceeded the flow's `timeout`. | Increase `timeout` or split the job. |
| `FlowCanceledError` | — | `Flow was canceled` | The run was canceled and its signal fired. | Stop external work and propagate the error. |
| `SleepSignal` | `resume_at` | `Flow sleeping until <timestamp>` | Internal continuation signal created by `step.sleep()`. | Propagate it; do not treat it as an application failure. |

All five are exported from the crate root.

```rust,ignore
use openshop::{DispatchFlowParams, FlowConcurrencyError, OpenShop, Result};

/// Queues a sync, or returns the ID of the run that is already active.
async fn queue_sync(app: &OpenShop, shop: &str) -> Result<String> {
    match app.dispatch_flow(DispatchFlowParams::new("syncOrders", shop)).await {
        Ok(queued) => Ok(queued.run_id.to_string()),
        Err(error) => match error.downcast_ref::<FlowConcurrencyError>() {
            Some(conflict) => Ok(conflict.existing_run_id.clone()),
            None => Err(error),
        },
    }
}
```

Normal run states are `pending`, `running`, `sleeping`, `completed`, `failed`,
and `canceled`. A worker lease loss (`RunFlowResult::LeaseLost`) is an
execution result used to prevent an outdated worker from committing over a
newer lease; it is not stored as a run state.

The `error` column of a failed run or step holds the error's full context
chain, outermost first, such as `while syncing: connection refused`. Other
messages the engine stores on runs:

| Run error | Cause |
| --- | --- |
| `Invalid input for flow "<flow>": <summary>` | The dispatched input failed validation. |
| `Flow panicked: <message>` | The flow function panicked. |
| `Flow timed out — exceeded maximum allowed duration` | The run passed its deadline before a worker could finish it. |
| `Flow "<flow>" not registered` | A worker claimed a run of a flow that its binary does not register. |
| `Worker error: <message>` | The run could not be started, for example because provider configuration could not be decrypted. |

## Admin function errors

`AdminPublicError` is the error a custom admin page loader or action returns
for a failure that is safe to show to the merchant.

| Field | Type | Default |
| --- | --- | --- |
| `code` | `String` | Set by `AdminPublicError::new(code, message)` |
| `message` | `String` | Set by `AdminPublicError::new(code, message)` |
| `status` | `u16` | `400`; change with `.status(…)` |
| `field_errors` | `Option<IndexMap<String, String>>` | `None`; set with `.field_errors(…)` |

See [Custom admin pages](custom-admin-pages.md#authorization-and-errors).

## Configuration errors

`APP.config()`, which `APP.run()` calls, and `OpenShop::new` return an error
whose message starts with `[openshop]`, such as
`[openshop] Invalid config: flows.syncOrders.timeout must be a positive number`.
With the standard `main`, the process prints the error and exits with a
non-zero status before serving anything. See
[Runtime validation](configuration.md#runtime-validation).

## HTTP status conventions

| Status | Meaning |
| --- | --- |
| `400` | Invalid query, body, configuration, or operation state. |
| `401` | Missing or invalid Shopify, proxy, webhook, or MCP authentication. |
| `403` | Authenticated MCP token lacks permission, is disabled, revoked, or expired; custom admin function rejected by an access rule. |
| `404` | Flow, run, provider, Function instance, resource, or static route not found; unsupported method on a known path. |
| `405` | Server route called with a method it does not declare. |
| `409` | State conflict such as a revoked MCP token mutation. |
| `413` | Request body exceeds the accepted limit. |
| `422` | Custom admin function input failed validation. |
| `500` | Handler, provider, Shopify, database, or unexpected runtime failure. |

Error bodies generally contain an `error` message. MCP protocol failures use a
JSON-RPC error object instead, and unexpected Admin API failures answer with
the plain text `Internal Server Error`. Treat message text as diagnostic, not
as a stable machine-readable code unless the endpoint documents one.

## Application error hook

Set the `on_error` setting of `app!` for centralized reporting:

```rust,ignore
use std::sync::Arc;

use openshop::{App, ErrorContext, app};

use flows::sync_orders::sync_orders;

async fn report_failure(error: Arc<openshop::Error>, context: ErrorContext) {
    // Forward to your error reporter here.
    eprintln!(
        "flow {:?} step {:?} failed: {error:#}",
        context.flow, context.step
    );
}

pub static APP: App = app! {
    flows: [sync_orders],
    on_error: report_failure,
};
```

The hook is an async function. It receives the error as an
`Arc<anyhow::Error>` (`openshop::Error` is `anyhow::Error`) and an
`ErrorContext` whose `flow` is the flow key. `step` is currently always
`None`. It runs after every failed attempt of a flow run, whether or not the
run will be retried, and not for cancellations. A panic inside the hook is
caught and ignored.

The hook supplements stored logs. It does not replace returning errors from
failed provider or Shopify operations.
