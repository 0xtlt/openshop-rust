//! Throughput of a worker on a flow of three steps that do nothing.
//!
//! ```text
//! cargo run --release -p openshop --example worker_bench -- --backlog=80000 --seconds=20
//! ```
//!
//! Seeds `--backlog=N` pending runs (2,000 by default), starts a worker with the
//! default settings and counts the runs completed in `--seconds=N` (20), or
//! until the queue is empty.
//!
//! - `--concurrency=N` and `--poll-interval-ms=N` override the worker settings;
//!   `PGPOOL_MAX` sizes the connection pool.
//! - `--workers=N` starts several workers on the same pool.
//! - `--deadlines` gives every run a deadline one hour away.
//! - `--progress` prints the runs completed and the slots busy, second by second.
//! - `--keep` leaves the schema in place and prints its name, for `EXPLAIN`.
//!
//! Runs in its own schema of `DATABASE_URL`, by default the development
//! database. Each run commits about ten times, so the figures follow the commit
//! latency of PostgreSQL: compare medians of several runs.

use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use openshop::builder::Flow;
use openshop::db::migrate::push_schema;
use openshop::engine::Worker;
use openshop::runtime::RuntimeEnv;
use openshop::sqlx::postgres::PgConnectOptions;
use openshop::{Concurrency, FlowContext, OpenShop, OpenShopConfig, Result, WorkerConfig};

const FLOW: &str = "bench-flow";

fn option<T: FromStr>(name: &str) -> Option<T> {
    let prefix = format!("--{name}=");
    std::env::args().find_map(|arg| arg.strip_prefix(&prefix).and_then(|value| value.parse().ok()))
}

fn flag(name: &str) -> bool {
    std::env::args().any(|arg| arg == format!("--{name}"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let backlog: i64 = option("backlog").unwrap_or(2_000);
    let seconds: u64 = option("seconds").unwrap_or(20);
    let workers: usize = option("workers").unwrap_or(1);
    let mut settings = WorkerConfig::new();
    settings.concurrency = option("concurrency");
    settings.poll_interval = option("poll-interval-ms").map(Duration::from_millis);

    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgresql://openshop:openshop@localhost:5432/openshop".to_string());
    let schema = format!("bench_{}", openshop::Uuid::new_v4().simple());
    let options = PgConnectOptions::from_str(&url)?.options([("search_path", schema.as_str())]);
    let pool = openshop::db::pool_options().connect_with(options).await?;
    execute(&pool, &format!("CREATE SCHEMA \"{schema}\"")).await?;
    push_schema(&pool, &[], false).await?;

    let completed = Arc::new(AtomicU64::new(0));
    let flow = Flow::new(FLOW, {
        let completed = completed.clone();
        move |ctx: FlowContext| {
            let completed = completed.clone();
            async move {
                for step in ["one", "two", "three"] {
                    let _: u32 = ctx.step.run(step, || async { Ok(1) }).await?;
                }
                completed.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        }
    })
    .concurrency(Concurrency::Allow);
    let app = OpenShop::new(
        OpenShopConfig::builder().flow(flow).build()?,
        pool.clone(),
        RuntimeEnv {
            development: true,
            ..Default::default()
        },
    )?;

    // The rows `dispatch_flow` writes, one millisecond apart.
    let deadline = if flag("deadlines") {
        "now() + interval '1 hour'"
    } else {
        "NULL"
    };
    execute(
        &pool,
        &format!(
            "INSERT INTO flow_runs (shop, flow_name, status, input, retry_policy, deadline_at, available_at, created_at) \
             SELECT 'bench.myshopify.com', '{FLOW}', 'pending', '{{}}'::json, \
               '{{\"maxAttempts\":3,\"initialIntervalMs\":1000,\"backoffCoefficient\":2.0,\"maxIntervalMs\":30000}}'::json, \
               {deadline}, \
               now() - interval '1 day' + n * interval '1 millisecond', \
               now() - interval '1 day' + n * interval '1 millisecond' \
             FROM generate_series(1, {backlog}) AS n"
        ),
    )
    .await?;
    execute(&pool, "ANALYZE flow_runs").await?;

    let fleet: Vec<Worker> = (0..workers).map(|_| Worker::new(app.clone(), settings)).collect();
    let started = Instant::now();
    for worker in &fleet {
        worker.start().await;
    }
    let window = Duration::from_secs(seconds);
    // `--progress` prints, for each second, the runs completed and the slots that were busy on average.
    let (mut next_report, mut reported) = (Duration::from_secs(1), 0);
    let (mut busy, mut samples) = (0, 0);
    while started.elapsed() < window && completed.load(Ordering::Relaxed) < backlog as u64 {
        tokio::time::sleep(Duration::from_millis(2)).await;
        busy += fleet.iter().map(Worker::active_count).sum::<usize>();
        samples += 1;
        if flag("progress") && started.elapsed() >= next_report {
            let done = completed.load(Ordering::Relaxed);
            println!(
                "  {:>3}s: {} runs/s, {:.1} slots busy",
                next_report.as_secs(),
                done - reported,
                busy as f64 / samples as f64
            );
            (next_report, reported) = (next_report + Duration::from_secs(1), done);
            (busy, samples) = (0, 0);
        }
    }
    let elapsed = started.elapsed();
    let done = completed.load(Ordering::Relaxed);
    for worker in &fleet {
        worker.stop().await;
    }

    // Every run counted once: completed in the database, and never claimed twice.
    let (recorded, failed, max_attempts): (i64, i64, Option<i32>) = openshop::sqlx::query_as(
        "SELECT count(*) FILTER (WHERE status = 'completed'), count(*) FILTER (WHERE status = 'failed'), \
           max(attempts) FROM flow_runs",
    )
    .fetch_one(&pool)
    .await?;
    println!(
        "backlog={backlog} workers={workers} concurrency={} poll_interval_ms={} deadlines={}: \
         {done} runs in {:.2}s = {:.1} runs/s (database: {recorded} completed, {failed} failed, max attempts {})",
        settings
            .concurrency
            .map_or("default".to_string(), |value| value.to_string()),
        settings
            .poll_interval
            .map_or("default".to_string(), |value| value.as_millis().to_string()),
        flag("deadlines"),
        elapsed.as_secs_f64(),
        done as f64 / elapsed.as_secs_f64(),
        max_attempts.unwrap_or(0),
    );

    if flag("keep") {
        println!("schema kept: {schema}");
    } else {
        execute(&pool, &format!("DROP SCHEMA \"{schema}\" CASCADE")).await?;
    }
    Ok(())
}

async fn execute(pool: &openshop::PgPool, sql: &str) -> Result<()> {
    openshop::sqlx::raw_sql(openshop::sqlx::AssertSqlSafe(sql))
        .execute(pool)
        .await?;
    Ok(())
}
