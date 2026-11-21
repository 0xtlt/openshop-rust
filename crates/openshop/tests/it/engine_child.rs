//! A flow that knows its run, and the runs it starts (#100).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::common::{TEST_SHOP, TestApp, create_config, find_run, find_steps, noop_flow};
use openshop::builder::Flow;
use openshop::engine::Worker;
use openshop::{Concurrency, DispatchOptions, FlowContext, OpenShop, WorkerConfig, bail, json};
use uuid::Uuid;

const TIMEOUT: Duration = Duration::from_secs(15);

fn worker(app: &OpenShop) -> Worker {
    Worker::new(
        app.clone(),
        WorkerConfig::new()
            .concurrency(4)
            .poll_interval(Duration::from_millis(20))
            .poll_max_interval(Duration::from_millis(40)),
    )
}

async fn wait_for(test: &TestApp, run_id: Uuid, status: &str) -> openshop::db::schema::FlowRun {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let run = find_run(&test.db, run_id).await;
        if run.status == status {
            return run;
        }
        assert!(
            Instant::now() < deadline,
            "run is still \"{}\" (error: {:?}), expected \"{status}\"",
            run.status,
            run.error
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_flow_reads_its_run_and_is_the_parent_of_the_runs_it_dispatches() {
    let seen = Arc::new(Mutex::new(None));
    let parent = {
        let seen = seen.clone();
        Flow::new("parent", move |ctx: FlowContext| {
            let seen = seen.clone();
            async move {
                *seen.lock().unwrap() = Some(ctx.run_id);
                let child = ctx.app.dispatch_flow(DispatchOptions::for_scope("child", &ctx)).await?;
                ctx.step.run("remember", || async move { Ok(child.run_id) }).await?;
                Ok(())
            }
        })
    };
    let test = TestApp::new(create_config([parent, noop_flow("child")])).await;
    let run_id = test
        .app
        .dispatch_flow(DispatchOptions::new("parent", TEST_SHOP))
        .await
        .unwrap()
        .run_id;
    // Dispatched from outside a flow: no parent.
    assert_eq!(find_run(&test.db, run_id).await.parent_run_id, None);

    let worker = worker(&test.app);
    worker.start().await;
    wait_for(&test, run_id, "completed").await;
    worker.stop().await;

    assert_eq!(*seen.lock().unwrap(), Some(run_id));
    let child: Uuid = serde_json::from_value(find_steps(&test.db, run_id).await[0].output.clone().unwrap()).unwrap();
    assert_eq!(find_run(&test.db, child).await.parent_run_id, Some(run_id.to_string()));
    test.cleanup().await;
}

#[tokio::test]
async fn run_flow_waits_for_the_child_and_leaves_no_step_sleeping() {
    let parent = Flow::new("chain", |ctx: FlowContext| async move {
        let first = ctx.run_flow("first", "slow", json!({ "ms": 1_500 })).await?;
        let second = ctx.run_flow("second", "broken", json!({})).await?;
        ctx.step
            .run("report", || async move {
                Ok(json!({ "first": first.status, "second": second.status, "error": second.error }))
            })
            .await?;
        Ok(())
    })
    .concurrency(Concurrency::Allow);
    let slow = Flow::new("slow", |ctx: FlowContext| async move {
        let ms = ctx.input["ms"].as_u64().unwrap_or(0);
        tokio::time::sleep(Duration::from_millis(ms)).await;
        Ok(())
    });
    let broken = Flow::new("broken", |_ctx: FlowContext| async move { bail!("no luck") })
        .retry_policy(openshop::RetryPolicy::new().max_attempts(1));
    let test = TestApp::new(create_config([parent, slow, broken])).await;
    let run_id = test
        .app
        .dispatch_flow(DispatchOptions::new("chain", TEST_SHOP))
        .await
        .unwrap()
        .run_id;

    let worker = worker(&test.app);
    worker.start().await;
    wait_for(&test, run_id, "completed").await;
    worker.stop().await;

    let steps = find_steps(&test.db, run_id).await;
    let report = steps.iter().find(|step| step.step_name == "report").unwrap();
    assert_eq!(
        report.output,
        Some(json!({ "first": "completed", "second": "failed", "error": "no luck" }))
    );
    // The child was dispatched once, whatever the replays.
    let children: Vec<(String, String)> = sqlx::query_as(
        "SELECT flow_name, parent_run_id FROM flow_runs WHERE parent_run_id IS NOT NULL ORDER BY created_at",
    )
    .fetch_all(&test.db)
    .await
    .unwrap();
    assert_eq!(
        children,
        [
            ("slow".to_string(), run_id.to_string()),
            ("broken".to_string(), run_id.to_string())
        ]
    );
    // The run waited for `slow`, and the sleep that woke it last is closed.
    assert!(steps.iter().any(|step| step.step_name == "first: wait #1"), "{steps:?}");
    assert!(
        steps.iter().all(|step| step.status == "completed"),
        "{:?}",
        steps
            .iter()
            .map(|step| (&step.step_name, &step.status))
            .collect::<Vec<_>>()
    );
    test.cleanup().await;
}
