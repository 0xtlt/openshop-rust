# Admin API

The Admin API is mounted at `/api` and is intended for OpenShop's embedded admin UI.

> **Stability.** These HTTP routes are **internal and unversioned**. They are
> documented to help with debugging and controlled integrations, but they are
> not currently a stable public API. Pin the OpenShop version and test response
> contracts before depending on them. The Rust API documented in
> [Public SDK](sdk.md) is the supported application API.

## Authentication

Every `/api/*` request requires a Shopify session token:

```http
GET /api/runs?limit=20
Authorization: Bearer <shopify-session-token>
```

In an embedded app, App Bridge issues the token (`shopify.idToken()`); obtain a
fresh one for every request. The admin UI and the `openshop-ui` hooks do this
for you. In tests, `TestContext::get("/api/runs?limit=20")` signs the request.

OpenShop validates the HS256 signature, `aud`, `exp`, `nbf`, and matching `iss` /
`dest` shop domains. The audience selects the Shopify app in multi-app deployments.
The authenticated app handle and shop scope every database query.

A missing or invalid token returns `401` with `{ "error": "Unauthorized: …" }`.
The token must belong to a staff user: a token whose `sub` is empty or a
customer (`gid://shopify/Customer/…`) is rejected with
`Unauthorized: session token was not issued to a staff user`. Do not use a
Shopify Admin API access token or an OpenShop MCP token here.

Authentication runs first: a disabled page or an unknown path under `/api`
still answers `401` without a valid token.

## Conventions

- Bodies are JSON. A missing or malformed JSON body is treated as `{}`.
- JSON keys are camelCase. Timestamps are UTC ISO 8601 strings with
  millisecond precision, such as `2026-04-01T12:30:05.123Z`.
- Errors are `{ "error": "<message>" }` unless stated otherwise. An unexpected
  failure answers `500` with the plain text `Internal Server Error`; the cause
  is written to the runtime log.
- An identifier that is not a UUID is answered like an unknown one (`404`).
- An unknown path, or an unsupported method on a known path, answers a plain
  `404 Not Found`.
- A request body larger than the
  [request body limit](configuration.md#settings) answers `413`.
- Responses of 1 KB or more are compressed with gzip or Brotli when the client
  accepts it.

## Admin pages

| Method and path | Request | Response |
| --- | --- | --- |
| `GET /api/pages` | — | `{ flows, providers, crons, functions, mcp }`, the resolved visibility of each built-in page. |
| `GET /api/pages/custom` | — | `{ navigation: [{ label, path }], pages: [{ id, path, allowed }] }` for experimental custom pages. |
| `GET /api/pages/custom/access?path=<path>` | — | `{ allowed }`; `404` with `{ "allowed": false }` when no custom page matches. |
| `POST /api/pages/custom/<page>/_rpc/<kind>/<name>` | `{ "input": ... }` | Internal loader or action result. |

Each value of `GET /api/pages` is `visible`, `hidden`, or `disabled`. A page
without a configured mode resolves to `visible`, or to `hidden` when its
collection (flows, providers, crons, functions) is empty. The `mcp` page
without a configured mode resolves to `disabled` when MCP is turned off with
`McpConfig::disabled()`. This endpoint stays available even when every page is
`disabled`.

When a page is `disabled`, its admin API group returns `404` with `{ "error": "Not found" }`:

- `flows` also covers `/api/runs`
- `providers` covers `/api/providers`
- `crons` covers `/api/crons`
- `functions` covers `/api/functions`
- `mcp` covers `/api/mcp`

`hidden` only removes the Shopify sidebar link; the URL and admin API still work. It is not an access-control boundary. Use `disabled` when shop staff must not be able to open a page or call its admin API. Runtime endpoints such as `POST /mcp`, webhooks, and the worker are not gated by page modes.

`GET /api/pages/custom` lists only navigation entries the staff member may
open. In `pages`, `allowed` is always `false` for pages with dynamic segments;
use the `access` endpoint for a concrete path. A page declared at `/`, the
home of the admin, has the `id` `""` and the `path` `/`, and its functions are
called at `/api/pages/custom/_rpc/<kind>/<name>`.

Custom-page RPC paths are implementation details. Applications should
use `use_loader` and `use_action`, not call them directly. Page and function
authorization rules run independently for every request. See
[Custom admin pages](custom-admin-pages.md#authorization-and-errors) for the
error contract.

## Flows and runs

| Method and path | Request | Response |
| --- | --- | --- |
| `GET /api/flows` | — | `[{ name, crons, inputSchema }]`: flow keys, their cron entries, and input JSON Schemas (`null` without a schema). |
| `GET /api/runs` | Query filters below | Run rows for the authenticated app and shop, newest first. |
| `GET /api/flows/:name/runs` | Same filters | Run rows restricted to one flow. |
| `GET /api/runs/:id` | — | Run row plus `steps`, in creation order. |
| `POST /api/flows/:name/run` | `{ "input": { ... } }` | `202` with `{ runId, status: "pending" }`. |
| `POST /api/runs/:id/retry?mode=resume` | — | Requeues a finished run: `{ ok: true, runId, mode }`. |
| `POST /api/runs/:id/retry?mode=reset` | — | Deletes every step result, then requeues. |
| `POST /api/runs/:id/cancel` | — | Cancels a pending, running, or sleeping run: `{ ok: true }`. |
| `DELETE /api/runs/:id` | — | Deletes an inactive run, its steps, and logs: `{ ok: true }`. |
| `POST /api/runs/bulk-delete` | `{ "ids": ["uuid"] }` | `{ deleted, skipped }`, maximum 100 IDs. |

A run row has the keys `id`, `appHandle`, `shop`, `flowName`, `status`,
`input`, `error`, `deadlineAt`, `parentRunId`, `attempts`, `availableAt`,
`workerId`, `retryPolicy`, `startedAt`, `completedAt`, and `createdAt`. A step
has `id`, `stepName`, `attempt`, `status`, `output`, `error`, `durationMs`,
`createdAt`, and `flowRunId`.

Run list query parameters:

- `limit`: `1`–`200`, default `50`.
- `offset`: non-negative, default `0`.
- `search`: case-insensitive flow name, status, or run-ID search.
- `status`: `pending`, `running`, `sleeping`, `completed`, `failed`, or `canceled`. Other values are ignored.
- `from`, `to`: dates applied to `createdAt`. Accepted forms are RFC 3339 timestamps, `YYYY-MM-DDTHH:MM[:SS]` (read as UTC), and `YYYY-MM-DD`. Unparsable values are ignored.

Retry `mode=resume` keeps completed step results and deletes the others. Any value
other than `reset` behaves as `resume`. Retry accepts only `failed`,
`completed`, or `canceled` runs; it resets the attempt count and the deadline.
Active runs cannot be deleted.

| Status | `error` | Condition |
| --- | --- | --- |
| `400` | `No IDs provided` | Bulk delete without a non-empty `ids` array. |
| `400` | `Max 100 runs per batch` | Bulk delete with more than 100 IDs. |
| `404` | `Run not found` | The run does not exist for the authenticated app and shop. |
| `409` | `Cannot retry a run with status "<status>"` | Retry of an active run. |
| `409` | `Cannot cancel a run with status "<status>"` | Cancel of a finished run. |
| `409` | `Cannot delete an active run — cancel it first` | Delete of an active run. |
| `409` | `Flow "<name>" is already running for shop "<shop>" (run #<id>)` | Flow concurrency conflict. The body also contains `existingRunId`. |

Dispatching an unregistered flow name is an unexpected failure (`500`). In a
bulk delete, IDs that are unknown, active, or not UUIDs count as skipped.

## Run logs

| Method and path | Request | Response |
| --- | --- | --- |
| `GET /api/runs/:id/logs` | `q`, `levels`, `limit`, `cursor` | `{ logs, total, levelCounts, next }`, newest first, with matching context markers. |
| `GET /api/runs/:id/logs/export` | `format`, `q`, `levels` | Downloadable JSON or CSV, streamed. |

A log has the keys `id`, `flowRunId`, `level`, `message`, `payload`, and
`createdAt`. `total` is the number of log lines of the run before filtering,
and `levelCounts` the number of each level (`{ "info": 4, "error": 1 }`) in
the whole run.

A page holds `limit` lines: 1,000 when it is absent or not a number, 5,000 at
most. `next` is a cursor, `null` on the last page; pass it as `cursor` with the
same `q` and `levels` to get the following lines. Lines are ordered by
`(createdAt, id)`, newest first, so pages neither repeat nor skip a line. A
cursor that is not one answers `400` with `Invalid cursor`. A page never holds
more than `limit` lines, however many lines match and whatever the context
(`A:`, `B:`, `C:`) asked for; the cursor carries the context still to come, so
the pages put end to end are the list one unbounded request would give. The one
exception is a match whose context before it is longer than `limit`: it is not
split, so a page that starts with it holds it, up to 1,001 lines. Levels and time bounds
are applied by the database; the text of the filters is checked on the lines as
they are read, so a rare match in a long run still reads the run, a chunk at a
time. `before` context is limited to 1,000 lines.

The export is written as the lines are read, 500 at a time, and is never held
whole by the server: it has no `Content-Length`.

`levels` is a comma-separated subset of `info,warn,error`; all three are enabled by
default. `q` uses the [log query syntax](logging.md#log-query-syntax), including
text filters, time bounds, and context expansion. When `q` contains a text
filter, each returned log carries `_matched`: `true` for a match, `false` for a
context line. Export uses only matching rows and omits context
expansion. `format=csv` selects CSV with the columns
`id,level,message,payload,createdAt`; every other value returns JSON. The
response is sent as an attachment named `run-<id>-logs.<format>`; a `format`
that contains anything but letters and digits is replaced with `json` in the
file name.

## Cron overrides

| Method and path | Request | Response |
| --- | --- | --- |
| `GET /api/crons` | — | `[{ index, key, name, flow, schedule, input, shops, enabled }]`: configured crons plus their per-shop enabled state. |
| `POST /api/crons/toggle` | `{ "key": "flow:schedule", "enabled": false }` | `{ ok: true, key, enabled }`. |

The key returned by the list endpoint is `<flow>:<schedule>`. A toggle stores a
per-app, per-shop override; it does not edit the configuration. `key` must be a
non-empty string and `enabled` a boolean, otherwise the endpoint answers `400`
with `key and enabled are required`.

## Providers

| Method and path | Request | Response |
| --- | --- | --- |
| `GET /api/providers` | — | `[{ name, fields, config, configured, lastCheckedAt, lastCheckOk }]`: definitions, public config, and last health check. |
| `PUT /api/providers/:name` | `{ "config": { ... } }` | Validates, keeps stored passwords left blank, encrypts, and stores config: `{ ok: true }`. |
| `POST /api/providers/:name/check` | — | `{ ok }`, or `{ ok: false, error }` with `500`. |

Each entry of `fields` has `type`, `label`, and, when set, `placeholder`,
`options`, and `required`; password fields also have `hasValue`. Password
values are not returned in plaintext: `config` omits them.

| Status | `error` | Condition |
| --- | --- | --- |
| `400` | `Field "<name>" is required`, `Field "<name>": <summary>`, or `Provider transformer must return an object` | Invalid provider config. |
| `400` | `No checker defined` | Check of a provider without a checker. |
| `404` | `Provider not found` | Unknown provider key. |

## Shopify Functions

These routes query or mutate Shopify live; they do not represent local database
instances.

| Method and path | Request | Response |
| --- | --- | --- |
| `GET /api/functions` | — | `[{ key, label, type, handle, modes, supportsUpdate, singleton, fields }]`: local function definitions and editable fields. `modes` is present for discount functions only. |
| `GET /api/functions/:handle/instances` | — | Up to 50 live Shopify instances. |
| `POST /api/functions/:handle/instances` | Function config and owner fields | `201` with `{ ok: true, data }`, where `data` is the mutation payload. |
| `PUT /api/functions/:handle/instances/:id` | Function config and owner fields | `{ ok: true }`. |
| `DELETE /api/functions/:handle/instances/:id` | Optional `mode` query | `{ ok: true }`. |

Instances are listed as `{ id, title, status, startsAt, endsAt, config }` for
discounts, `{ id, label, state, blockOnFailure, config }` for cart transforms,
and `{ id, title, enabled, config }` for the other types.

Create and update bodies always accept `config`. Additional owner fields depend on
the type: `mode`, `code`, `startsAt`, `endsAt`, and `usageLimit` for discounts,
`enabled` and `blockOnFailure` for the other types, and `deliveryMethodTypes`
for fulfillment constraints. See
[Owner options](shopify-functions.md#owner-options).

| Status | `error` | Condition |
| --- | --- | --- |
| `400` | `Field "<name>" is required` or `Field "<name>": <summary>` | Invalid function config. |
| `400` | Shopify's messages, joined | Shopify mutation `userErrors`. The body also contains `userErrors`. |
| `400` | `Function type "<type>" does not support update. Delete and recreate instead.` | Update of a cart transform or fulfillment constraint. |
| `400` | `mode query parameter is required for discount functions with multiple modes` | Delete of a discount configured with both modes, without `?mode=automatic` or `?mode=code`. |
| `404` | `Function not found` | Unknown function handle. |
| `409` | `This Cart Transform already has an instance` | Second cart transform creation. |

## MCP token administration

These `/api/mcp/*` routes use a **Shopify session token**. The separate `/mcp`
protocol endpoint uses the OpenShop-issued token created here.

| Method and path | Request | Response |
| --- | --- | --- |
| `GET /api/mcp/capabilities` | — | `{ enabled, permissions, tools, resources, defaultExpirationDays, expirationOptions }`. |
| `GET /api/mcp/tokens` | — | Public token metadata; never the secret token. |
| `POST /api/mcp/tokens` | `{ name, permissions?, expiresAt? }` or `expiresInDays` | `201` with `{ token, item }`; returns the secret token once. |
| `GET /api/mcp/tokens/:id` | — | Token metadata and 10 latest audits. |
| `PATCH /api/mcp/tokens/:id` | `name`, `status`, and/or expiry | `{ ok: true }`. |
| `PUT /api/mcp/tokens/:id/permissions` | `{ "permissions": ["key"] }` | Replaces the complete permission set: `{ ok: true }`. |
| `POST /api/mcp/tokens/:id/rotate` | — | `{ ok: true, token }`; returns a replacement secret token once. |
| `POST /api/mcp/tokens/:id/revoke` | — | Permanently revokes the token: `{ ok: true }`. |

Token metadata has the keys `id`, `tokenId`, `name`, `tokenFingerprint`,
`status`, `expiresAt`, `revokedAt`, `lastUsedAt`, `createdAt`, `updatedAt`,
`permissions`, `stalePermissions` (granted permissions that are no longer
registered), and `recentAudits`. The `:id` in these paths is `id`, not
`tokenId`.

`expiresAt` accepts an ISO-compatible date string or `null`. `expiresInDays` accepts
a positive integer or `null`; omitted expiry defaults to 90 days. `status`
accepts `active` or `disabled`. Disabled tokens can be re-enabled; revoked
tokens cannot be edited or rotated.

| Status | `error` | Condition |
| --- | --- | --- |
| `400` | `name is required` | Token creation without a name. |
| `400` | `Unknown permission "<key>"` | Permission that is not registered. |
| `400` | `expiresAt must be a valid date string or null` | Invalid `expiresAt`. |
| `400` | `expiresInDays must be a positive integer or null` | Invalid `expiresInDays`. |
| `404` | `Token not found` | The token does not exist for the authenticated app and shop. |
| `409` | `Cannot update a revoked token` | Edit or permission change of a revoked token. |
| `409` | `Cannot rotate a revoked token` | Rotation of a revoked token. |

Store the `token` returned by create or rotate immediately. Later reads expose only
its ID and fingerprint.

## Common status codes

| Status | Meaning |
| --- | --- |
| `200` | Read or mutation completed. |
| `201` | Function instance or MCP token created. |
| `202` | Flow accepted for asynchronous execution. |
| `400` | Invalid request, provider config, function config, or MCP permission. |
| `401` | Missing, expired, malformed, or invalid Shopify session token. |
| `404` | Resource not found within the authenticated app and shop, unknown path, or the admin page is `disabled`. |
| `409` | Lifecycle or concurrency conflict. |
| `413` | Request body larger than the request body limit. |
| `500` | Provider checker or unexpected server failure. |
