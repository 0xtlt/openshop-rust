# Operate an OpenShop app

Use this guide after [deploying the app](deploy-production.md). You need
access to the embedded admin and to the web and worker service logs.
For the process model, read [Architecture](../concepts/architecture.md).

The commands below use `app`, the name the generated `Dockerfile` gives the
binary. From a source checkout, replace `app` with `cargo run --release --`.

## 1. Start separate processes

Web service:

```bash
app start
```

Worker service:

```bash
app worker --concurrency=5
```

Do not place the commands sequentially in one shell script: `app start` is
long-running, so the worker line would never execute. Configure two services,
containers, process-manager entries, or terminal sessions.

## 2. Check health and run a smoke flow

The unauthenticated health endpoint confirms only that the HTTP process responds:

```bash
curl --fail --silent https://your-app.example.com/health
```

Expected shape:

```json
{
  "status": "ok",
  "timestamp": "2026-07-29T12:00:00.000Z"
}
```

It does not prove PostgreSQL, Shopify, or workers are healthy. Complete a smoke
check by dispatching a harmless flow from the embedded admin UI and watching it
move from `pending` to `completed`.

## 3. Monitor runs

In the embedded admin UI:

1. Search runs by status: `pending`, `running`, `sleeping`, or `failed`.
2. Open a run to inspect step status and structured logs.
3. Select **Retry**, then **Resume** on a finished run to keep completed steps.
4. Use **Restart** (the API's `reset` mode) only when completed steps are safe to execute again.

Operational signals:

| Signal | Likely cause | Action |
| --- | --- | --- |
| Growing `pending` count | No worker, saturated concurrency, or DB failure | Check worker processes and database connectivity; then scale workers. |
| Old `running` runs | Worker crash or a long step | Check worker logs and step timeout; expired leases allow another worker to reclaim work. |
| Repeated `sleeping` runs | Explicit `ctx.step.sleep` | Inspect run logs and `availableAt`. |
| Runs returning to `pending` | Retrying failures | Inspect the run's `error`, `attempts`, and `availableAt`. |
| Many `failed` runs | Provider, Shopify, validation, or deployment regression | Filter logs by error and compare the first failure time with deployments. |
| Cron absent | Disabled per-shop override, no installed shop for a cron targeting `all`, an active run of the same flow, or web process down | Check the cron toggle, the cron's `shops` target, and the web/scheduler process logs. |

## 4. Scale workers

Start additional identical worker processes against the same database. Workers use
PostgreSQL row locking with `SKIP LOCKED`, so separate processes can safely claim
different runs.

Configure defaults on the builder in `src/main.rs`, keeping your other settings:

```rust,ignore
use openshop::PartialWorkerConfig;

fn config() -> Result<OpenShopConfig> {
    OpenShopConfig::builder()
        // ... providers, flows and other settings ...
        .worker(
            PartialWorkerConfig::new()
                .concurrency(5)
                .poll_interval_ms(1_000)
                .poll_max_interval_ms(5_000)
                .poll_backoff_coefficient(1.5)
                .lease_duration_ms(30_000),
        )
        .build()
}
```

A CLI `--concurrency` override wins for that worker process. The complete
[worker defaults](../reference/configuration.md#worker-defaults) are in the reference.

Scale gradually. Total simultaneous flow runs are approximately:

```text
worker process count × concurrency per worker
```

Also budget PostgreSQL connections per process using `PGPOOL_MAX`.

A worker mostly waits for PostgreSQL and for the services its steps call. The
tokio runtime starts one thread per core, and waking them costs more CPU than
they save on such work: on short flows, a worker with `TOKIO_WORKER_THREADS=1`
used a third less CPU per run than one with fourteen threads, at the same
throughput. Keep several threads when steps compute for long without awaiting:
on a single thread, such a step holds back every other run of the process.

## Recover failed runs

1. Open the failed run and find the first failed step and its error.
2. Correct the cause, such as invalid provider credentials or missing Shopify scopes.
3. Select **Retry**, then **Resume** to reuse completed checkpoints.
4. Verify the resumed run completes and check the downstream result.

Choose **Restart** only if every side effect can safely happen again. Read
[Checkpoints and retries](../concepts/checkpoints-and-retries.md) before resetting a
run that writes to another system.

## Cancel, retry, and delete safely

- A cancel request marks an active run `canceled` and interrupts the step being
  executed. A worker in another process notices at its next heartbeat, within
  about 15 seconds, or half the lease when `lease_duration_ms` is below 30
  seconds.
- Work that a step moved off its own future, such as a `tokio::spawn` task or
  `spawn_blocking` code, keeps running unless it watches `ctx.signal`.
- Resume retry keeps completed steps; reset retry discards all step results.
- Active runs must be canceled before deletion.
- Bulk delete accepts at most 100 IDs and skips active runs.

Pass the cancellation token to work the engine cannot interrupt:

```rust,ignore
use openshop::Flow;

pub fn refresh_catalog() -> Flow {
    Flow::new("refreshCatalog", |ctx| async move {
        let total: u64 = ctx
            .step
            .run("hash catalog", || async {
                let signal = ctx.signal.clone();
                let total = tokio::task::spawn_blocking(move || {
                    let mut total = 0u64;
                    for chunk in 0..1_000_000u64 {
                        if signal.is_cancelled() {
                            break;
                        }
                        total = total.wrapping_add(checksum(chunk));
                    }
                    total
                })
                .await?;
                Ok(total)
            })
            .await?;

        ctx.logger.info(openshop::json!({ "total": total }), "Catalog hashed");
        Ok(())
    })
}

fn checksum(chunk: u64) -> u64 {
    chunk.wrapping_mul(31)
}
```

## Graceful deployment

1. Stop routing new HTTP traffic to the old web process.
2. Send `SIGTERM` to old workers.
3. Allow at least `lease_duration_ms` for active work to finish.
4. Apply committed database migrations.
5. Start the new web process and workers from the same build.
6. Dispatch one harmless flow and confirm completion.

The worker stops claiming new runs on shutdown and waits up to its lease duration
for active runs. If a process dies, its leases eventually expire and another worker
can reclaim the runs.

## Backups and incident data

Back up PostgreSQL using your managed database policy. OpenShop state includes
installations, encrypted access tokens, provider configurations, flow runs, steps,
logs, cron overrides, and MCP tokens/audits.

During an incident, preserve:

- application version and deployment time;
- web and worker process logs;
- affected run IDs and exported run logs;
- database health and connection saturation;
- recent provider, Shopify scope, encryption-key, or environment changes.

Never publish exported logs without checking their structured payloads for customer
or order data.

For log queries and exports, see [Logging](../reference/logging.md). For API status
codes and retry endpoints, see [Admin API](../reference/admin-api.md).
