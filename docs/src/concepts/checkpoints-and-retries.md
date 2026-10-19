# Checkpoints and retries

A checkpoint lets a run reuse completed work after a failure or a pause. It does
not make an external API call and a PostgreSQL write one atomic operation.
That distinction matters for order creation, payments, emails, and other side effects.

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
the service again. Use the service's idempotency mechanism with a stable business
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

Retries use the configured attempt limit and backoff. Waking up from a sleep is
not a retry and does not use an attempt. A flow deadline can stop further retries
even when attempts remain. The
[flow reference](../reference/flows.md) gives the exact defaults and precedence.

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
