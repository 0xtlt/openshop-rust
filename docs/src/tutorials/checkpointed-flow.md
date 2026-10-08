# Build a checkpointed flow

In this lesson, we will add a small report flow to the app from
[Build your first app](first-app.md). It will save a timestamp, sleep for
10 seconds, and log a report using that saved value. It makes no external API
calls and does not change store data.

Keep `shopify app dev` running and open the installed app in Shopify admin.
Work from the `my-app` directory.

## 1. Write the flow

Create `src/flows/prepare_report.rs`:

```rust
use std::time::Duration;

use openshop::chrono::Utc;
use openshop::prelude::*;
use serde::{Deserialize, Serialize};

#[input]
#[derive(Debug)]
pub struct PrepareReportInput {
    #[schemars(length(min = 1))]
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    label: String,
    prepared_at: String,
}

#[flow("prepareReport")]
pub async fn prepare_report(ctx: FlowContext, input: PrepareReportInput) -> Result<()> {
    let report: Report = ctx
        .step
        .run("prepare-report", async || {
            let report = Report {
                label: input.label.clone(),
                prepared_at: Utc::now().to_rfc3339(),
            };
            ctx.logger.info_with("Prepared report", json!(report));
            Ok(report)
        })
        .await?;

    ctx.step
        .sleep("wait-before-publishing", Duration::from_secs(10))
        .await?;

    ctx.step
        .run("publish-report", async || {
            ctx.logger.info_with("Published report", json!(report));
            Ok(())
        })
        .await
}
```

`#[flow("prepareReport")]` makes the function the flow of that name. Its second
argument is the input of a run: `#[input]` makes `PrepareReportInput` a type a
run can receive, and a run with an empty `label` is refused.

`Report` derives both `Serialize` and `Deserialize`. `ctx.step.run()` saves the
returned value as JSON so the resumed flow can read the same timestamp back.

## 2. Register it

Declare the module in `src/flows/mod.rs`:

```rust
pub mod prepare_report;
pub mod sync_orders;
```

In `src/main.rs`, import the flow next to `sync_orders`:

```rust
use flows::prepare_report::prepare_report;
use flows::sync_orders::sync_orders;
```

Then list it in the `flows` of `app!`:

```rust
static APP: App = app! {
    providers: [Warehouse],
    flows: [sync_orders, prepare_report],
    // MCP is off: `POST /mcp` is refused and the admin has no MCP page.
    // Remove this line to turn it on.
    mcp: McpConfig::disabled(),
    on_error: report_failure,
};
```

Leave the rest of the file unchanged.

Wait for the development server to reload. In the terminal running
`shopify app dev`, `openshop dev` reports the changed file, recompiles the
project, and restarts the app. Each saved file triggers a rebuild; wait for the
`[openshop] Ready.` that follows your last edit, then refresh **Flows**. You
should see `prepareReport` alongside `syncOrders`.

## 3. Observe the pause

Click **Run** next to `prepareReport` and run it with:

```json
{ "label": "My first report" }
```

The run page opens. During the 10-second pause, the run's status is `sleeping`.
After the worker picks it up again, it becomes `completed`. If you miss the
sleeping state, the step history still shows the pause.

Inspect the logs:

- **Prepared report** appears once.
- **Published report** contains the same `preparedAt` timestamp.
- **Step "prepare-report" skipped (cached)** appears after the pause.
- The run has checkpoints for `prepare-report`, `wait-before-publishing`, and `publish-report`.

The worker re-entered the flow after the sleep. The completed `prepare-report`
checkpoint supplied its stored result, so the closure did not run again.

## 4. Start a separate run

Wait for completion, then run `prepareReport` again from **Flows**.
The new run prepares a new timestamp. Checkpoints belong to a particular run;
they are not shared between independent runs.

Run `cargo test` from a second terminal in the project directory. The generated
`the_application_is_valid` test now also validates the new flow.

## What you built

You wrote and listed a flow, validated its input, and observed a saved
checkpoint across a pause. Read [Checkpoints and retries](../concepts/checkpoints-and-retries.md)
for the failure cases and why external writes still need idempotency.
When you are ready to call a real service, use
[Connect an external service](../guides/define-provider.md) and
[Sync orders with a flow](../guides/define-flow.md).
