//! A flow that runs another flow and waits for it: `ctx.run_flow(…)`.

use std::time::Duration;

use anyhow::{Result, anyhow};
use serde_json::Value;
use uuid::Uuid;

use super::dispatch::DispatchOptions;
use crate::db::schema::FlowRun;
use crate::types::FlowContext;

/// The statuses a run ends with.
const FINISHED: [&str; 3] = ["completed", "failed", "canceled"];

/// The longest wait between two checks of a child run.
const MAX_WAIT: Duration = Duration::from_secs(60);

/// How long the run sleeps before its `round`th check of the child, from 1:
/// 1s, 2s, 4s… up to [`MAX_WAIT`]. A child that takes seconds is seen soon,
/// one that takes hours costs a check a minute.
fn wait_before(round: u32) -> Duration {
    Duration::from_secs(1u64 << round.saturating_sub(1).min(6)).min(MAX_WAIT)
}

impl FlowContext {
    /// Runs the flow `flow` for the shop of this run, as its child, and waits
    /// for it to end: resolves to the child run once it is `completed`,
    /// `failed` or `canceled`, whatever it ended with. Between two checks this
    /// run sleeps ([`StepExecutor::sleep`](super::StepExecutor::sleep)) and holds
    /// no worker.
    ///
    /// `name` names the steps of the wait, `{name}: dispatch` and
    /// `{name}: wait #1`, `#2`…: like the name of a step, it must be unique
    /// within the flow. The child is dispatched once, whatever the replays of
    /// this run. A dispatch that is refused (a child that rejects concurrent
    /// runs and has one) fails the step.
    ///
    /// ```ignore
    /// let export = ctx.run_flow("export", export_orders::NAME, json!({ "since": since })).await?;
    /// if export.status != "completed" {
    ///     bail!("The export failed: {}", export.error.unwrap_or_default());
    /// }
    /// ```
    pub async fn run_flow(&self, name: &str, flow: &str, input: Value) -> Result<FlowRun> {
        self.run_flow_with(name, DispatchOptions::for_scope(flow, self).input(input))
            .await
    }

    /// [`run_flow`](Self::run_flow) with the options of the dispatch: a delay,
    /// a retry policy, a concurrency key… Its shop and Shopify app are those
    /// of this run, which is its parent.
    pub async fn run_flow_with(&self, name: &str, mut options: DispatchOptions) -> Result<FlowRun> {
        options.shop = self.shop.clone();
        options.shopify_app = Some(self.shopify_app.clone());
        options.parent_run_id = Some(self.run_id.to_string());
        let flow = options.flow_name.clone();
        let app = self.app.clone();
        let child: Uuid = self
            .step
            .run(&format!("{name}: dispatch"), || async move {
                Ok(app.dispatch_flow(options).await?.run_id)
            })
            .await?;

        let mut round = 0;
        loop {
            let run = FlowRun::of(self).find(child).await?.ok_or_else(|| {
                anyhow!("The run {child} of flow \"{flow}\", started by step \"{name}\", was deleted")
            })?;
            if FINISHED.contains(&run.status.as_str()) {
                return Ok(run);
            }
            round += 1;
            // On a replay, the waits already slept return at once.
            self.step
                .sleep(&format!("{name}: wait #{round}"), wait_before(round))
                .await?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_longer_each_round_up_to_a_minute() {
        let waits: Vec<u64> = (1..=9).map(|round| wait_before(round).as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
    }
}
