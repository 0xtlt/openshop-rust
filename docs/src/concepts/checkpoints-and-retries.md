# Checkpoints and retries

A checkpoint lets a run reuse completed work after a failure or a pause. Steps
run **at least once**: a step whose result is stored is not run again for that
run, but a step whose result was not stored yet can be. A checkpoint does not
make an external API call and a PostgreSQL write one atomic operation. That
distinction matters for order creation, payments, emails, and other side effects.

## The function starts again; completed steps supply their results

Consider an order-sync flow with two steps:

```text
Attempt 1: fetch-orders → saved result → push-orders fails
Attempt 2: fetch-orders → cached result → push-orders runs again
```

OpenShop re-enters the flow function. When it reaches `fetch-orders`,
`ctx.step.run()` returns the stored result without calling that closure. Code
outside steps executes again, including logging, random values, and network calls.

Checkpoints are identified by run ID and step name. A new run has fresh
checkpoints. Two calls with the same name in one run share a checkpoint, so
give each iteration of a loop its own step name. Changing a step name in a
deployment creates a new checkpoint for runs that resume on the new code, so
step names are part of the persisted workflow contract.

## A successful side effect can still repeat

There is a window between an external service accepting a request and OpenShop
saving the completed step:

```text
Warehouse accepts order → worker exits → checkpoint was never saved
```

A later attempt cannot know the warehouse already accepted the order. It calls
the service again. The same happens when the lease of a run expires while its
step still runs, because the worker was stalled or lost the database: another
worker claims the run and runs the step, as long as the first one did not store
its result. Use the service's idempotency mechanism with a stable business
identifier, or design the write as an upsert. A named step reduces repeated
work; it does not guarantee exactly-once delivery.

The default concurrency policy rejects overlapping runs for the same app, shop,
and flow. It does not deduplicate two separate runs that happen one after the
other, or writes made by different flows.

## Stored values cross a JSON boundary

The first execution returns the closure's value; a replay deserializes its
JSON-stored representation into the same type. `ctx.step.run()` therefore only
accepts a result type that implements both `Serialize` and `DeserializeOwned`.
Return structs that derive both, `Vec`, `String`, numbers, `bool`, `Option`, or
`serde_json::Value`. A `chrono::DateTime<Utc>` is stored as an RFC 3339 string
and read back as the same instant.

The compiler checks that a type can be written and read, not that it reads back
what it wrote. A field marked `#[serde(skip)]` comes back as its default value
on replay. A `NaN` or infinite float is stored as `null` and cannot be read back
as a number. Values that cannot be serialized at all, such as an HTTP response,
an open connection, or a file handle, are rejected at compile time: return the
data you extracted from them.

`()` and `None` are both stored as `null`.

## Retry, resume, and reset have different effects

| Operation | Checkpoint behavior |
| --- | --- |
| Automatic retry after failure | Reuses completed step results within the existing run |
| Manual retry in resume mode | Keeps completed steps and retries unfinished work |
| Manual retry in reset mode | Discards step results and starts the work again |
| Dispatch a new run | Creates an independent set of checkpoints |

Retries use the configured attempt limit and backoff, and wait between half the
backoff and all of it, drawn at random, so that runs that failed together do not
all come back at the same moment. Waking up from a sleep is
not a retry and does not use an attempt. A flow deadline can stop further retries
even when attempts remain. The
[flow reference](../reference/flows.md) gives the exact defaults and precedence.

## A failed write is retried, not counted as a failure

The engine writes the state of a run and of its steps to the database. When such
a write fails because the connection dropped or the pool timed out, it is tried
again a few times, over about a second and a half, before the error stands. The
write carries the same ownership check each time, so a try that comes after the
run was taken over by another execution writes nothing.

If the outcome of a run still cannot be recorded, the run is not marked `failed`:
that write would go through the same database. The worker stops extending its
lease and drops the run, which another claim takes up once the lease expires,
and the completed steps are replayed from their checkpoints. The claim uses an
attempt, and a flow that is not idempotent between two checkpoints runs that
part again, as after any crash.

A step whose result cannot be stored after those tries fails with the database
error, like any step that fails. A step that finds its run taken over by another
execution stops the flow with `LeaseLostError` and stores nothing.

## A lease is kept, or given up

A worker holds the lease of a run for `lease_duration` and extends it while the
run executes. Every time of a lease is read from the clock of the database, never
from the one of a worker, so replicas whose clocks differ agree on when a lease
ends. The extension has a time limit of a quarter of the lease (at least 250 ms,
at most 10 s), which covers the wait for a free connection of the pool and the
statement itself. A failed extension is logged (`Heartbeat of run … failed`: the
first time and every tenth in a row) and tried again at the next heartbeat. The
worker counts the lease from the moment it sent the last request that extended
it, less a tenth of the lease. When that time has passed, the lease has
expired and another worker may own the run, whether or not a heartbeat is still
waiting for an answer: the flow is dropped, with whatever it was doing at its current
`.await`, no outcome is recorded, and the run is claimed again. Code that never
yields, or a task started with `tokio::spawn`, is not reached by a drop. An application that runs many flows at once should leave room in the pool
for the heartbeats: a pool kept busy by the flows delays them (see
[Database pool sizing](../reference/environment-variables.md#database-pool-sizing)).

On a graceful stop, a worker waits up to one lease for its runs. A run still
executing then goes back to `pending` at once, so that another worker takes it up
without waiting for the lease to expire. Its flow is dropped first and no outcome
is recorded; the run is only handed back once the flow is gone, so that nothing of
it runs beside the next owner (a flow that does not yield within a second is left
to its lease instead). The release waits at most two seconds for a connection and
for a lock: a run that cannot be released in time is logged and left to its
lease, and does not hold back the shutdown nor the other runs. The attempt that was interrupted stays counted in
`attempts` (it keys the rows of its steps), so the run has one retry fewer; it is
never failed as abandoned for that, and it runs again even with a single attempt.
If the run was claimed by another worker in the meantime, it is left alone.

## Sleep releases capacity; cancellation acts at await points

`ctx.step.sleep()` records when the run can resume and releases its worker slot.
It suspends the flow by returning an error that the engine recognizes, so
propagate its result with `?` as you do for any other step. The worker later
re-enters the flow and reuses completed checkpoints. A `tokio::time::sleep`
inside a step keeps the worker slot occupied instead.

Canceling a run cancels `ctx.signal`, a `CancellationToken`. The engine stops a
running step at its next `.await` and records it as `canceled`; code between
steps stops at the next `ctx.step` call. A step or flow timeout stops the code
the same way. When the cancellation comes from another process, such as the web
process handling an Admin API request, the worker applies it at its next
heartbeat.

Stopping the code cannot undo a request that an external service has already
received. It also does not reach work that never yields or that runs elsewhere:
a long computation without an `.await`, blocking code, or a task started with
`tokio::spawn`. Check `ctx.signal.is_cancelled()` in long loops and pass a clone
of the token to the tasks you spawn.

Try the [checkpoint tutorial](../tutorials/checkpointed-flow.md) to observe replay,
or use [Operate an app](../guides/operate-app.md) to recover a failed run.
