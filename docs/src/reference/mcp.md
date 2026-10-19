# MCP

OpenShop exposes a streamable HTTP-style JSON-RPC endpoint at `POST /mcp`.
MCP access is separate from Shopify OAuth: OAuth scopes limit Shopify access,
while MCP grants limit capabilities inside OpenShop.

## Connect a client

Configure an MCP client with:

```text
URL: https://your-app.example.com/mcp
Authorization: Bearer oshp_mcp_...
```

The endpoint implements protocol version `2025-11-25` and the methods
`initialize`, `tools/list`, `tools/call`, `resources/list`, and
`resources/read`. It does not advertise resource subscriptions or list-change
notifications.

Example request:

```http
POST /mcp
Authorization: Bearer oshp_mcp_...
Content-Type: application/json

{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}
```

Every response carries `MCP-Protocol-Version: 2025-11-25`. Notification methods
whose name starts with `notifications/` and have no `id` receive HTTP 202 with
an empty body.

`GET /mcp` is not part of the protocol: it is the token management page of the
embedded admin UI.

## Tokens

Create and manage tokens from the embedded `/mcp` page.

`mcp: McpConfig::disabled()` in `app!` turns MCP off: `POST /mcp` is refused,
and the `Mcp` page of the admin is disabled with it, which blocks the page and
`/api/mcp/*`. `McpConfig::new().enabled(false)` is the same setting. A project
created by `openshop init` sets it: MCP is off until the line is removed from
`src/main.rs`.

The `Mcp` page has its own mode. Set it to `Hidden` to remove only its sidebar
link, or to `Disabled` to block the page and `/api/mcp/*` while the JSON-RPC
endpoint stays on. A mode set for the page is kept when MCP is turned off:
with `Visible`, the page stays available next to a refused endpoint. See
[Admin pages](configuration.md#admin-pages).

`Hidden` is navigation-only: the direct page URL and admin API remain available.
It must not be used as an access-control boundary.

- The plaintext token is returned only on create or rotate.
- OpenShop stores a hash, stable token ID, and display fingerprint.
- Tokens are scoped to the current `(app handle, shop)`.
- New tokens expire after 90 days unless `expiresAt`, `expiresInDays`, or no
  expiry is selected.
- Status is `active`, `disabled`, or `revoked`; revocation cannot be undone.
- Rotation preserves the token ID and grants but invalidates the old secret.

A token has the form `oshp_mcp_<token id>.<secret>`, where the token ID starts
with `mtk_`. The stored hash is `scrypt:<salt>:<hash>` (scrypt with N = 16384,
r = 8, p = 1 and a 32-byte key), and the fingerprint is the first 16 characters
of the base64url SHA-256 digest of the token.

Checking a secret against its scrypt hash is slow on purpose, so it is
bounded: at most as many hashes as CPUs (4 at most) run at once, and a request
that gets no turn within five seconds is answered with HTTP 503. A secret that
was right is remembered for five minutes, under a keyed hash that lives in the
memory of the process only; only the scrypt step is remembered. Whether a token
is active, revoked, or expired, and its expiry, are read from the database at
every request, and rotating a token changes its hash, so a token is refused at
once when it is revoked, disabled, expired, or rotated.

Missing, malformed, unknown, or cryptographically invalid tokens return HTTP
401. Disabled, revoked, and expired tokens return HTTP 403. Successful
authentication updates `lastUsedAt`.

| Status | `error` |
| --- | --- |
| `401` | `Unauthorized: missing MCP bearer token` |
| `401` | `Unauthorized: invalid MCP token format` |
| `401` | `Unauthorized: unknown MCP token` |
| `401` | `Unauthorized: invalid MCP token` |
| `403` | `Forbidden: MCP token is not active` |
| `403` | `Forbidden: MCP token has expired` |
| `403` | `MCP is disabled` (valid token, endpoint disabled) |
| `503` | `Service Unavailable: too many MCP tokens are being checked, try again` |

## Permissions

Core permissions are:

| Permission | Risk | Purpose |
| --- | --- | --- |
| `read_logs` | low | Search visible flow-run logs. |
| `read_flows` | low | List flows and recent runs. |
| `run_flows` | high | Dispatch flows. |
| `read_crons` | low | List crons. |
| `manage_crons` | medium | Change cron enabled state. |
| `read_providers` | low | Provider access reserved by the capability registry. |
| `write_providers` | high | Provider access reserved by the capability registry. |
| `read_functions` | low | Function access reserved by the capability registry. |
| `manage_mcp_tokens` | high | Token management reserved by the capability registry. |
| `shopify_admin_graphql` | high | Run arbitrary Admin GraphQL documents for installed shops. |

Custom keys must match `namespace:action_resource`: a lowercase namespace
(letters, digits, `-`), a colon, and lowercase words separated by `_`.
Wildcard-style names such as `*`, `all`, `admin`, `root`, `full_access`, and
`full-access` are forbidden.

A custom permission is declared once, as a constant, next to what requires
it:

```rust,ignore
use openshop::prelude::*;

pub const READ_INVENTORY: McpPermission =
    McpPermission::new("warehouse:read_inventory", "Read warehouse inventory")
        .description("Allows MCP tokens to read the inventory of the warehouse.")
        .group("Warehouse")
        .risk(McpRiskLevel::Low);
```

`McpPermission::new(key, label)` takes the key and the label the admin shows.
`.description(…)`, `.group(…)`, and `.risk(…)` are optional: a custom
permission defaults to medium risk and to the namespace of its key as its
group.

A tool or a resource that lists the constant in its `permissions(…)` registers
it with the application: the permission needs no entry of its own in `app!`.
A permission that no tool and no resource lists, here a constant
`EXPORT_INVENTORY`, is given to the `mcp` setting:

```rust,ignore
static APP: App = app! {
    mcp: McpConfig::new().permission(EXPORT_INVENTORY),
};
```

`permissions(…)` also accepts a key as a string, `"warehouse:read_inventory"`,
which must then be a core permission or one registered elsewhere.

Unknown permissions, conflicts with core names, and malformed definitions fail
configuration validation with an error that starts with
`[openshop] Invalid MCP config:`. So does a key declared with two different
definitions
(`[openshop] Invalid MCP config: permission "warehouse:read_inventory" is declared with two definitions`).

## Custom tools

`#[mcp_tool(name)]` makes an `async` function a tool:

```rust,ignore
use openshop::prelude::*;
use serde::Serialize;

#[input]
pub struct ListInventory {
    pub location: Option<String>,
}

/// What `list_inventory` answers.
#[derive(Serialize)]
pub struct Inventory {
    pub shop: String,
    pub location: Option<String>,
    pub items: Vec<String>,
}

/// List warehouse inventory.
#[mcp_tool("warehouse.inventory.list", permissions(READ_INVENTORY), risk = low)]
pub async fn list_inventory(ctx: McpContext, input: ListInventory) -> Result<Inventory> {
    Ok(Inventory {
        shop: ctx.shop,
        location: input.location,
        items: Vec::new(),
    })
}
```

List the function in the `mcp_tools` of `app!`: `mcp_tools: [list_inventory]`.
The name of the tool is also the constant `list_inventory::NAME`.

The function stays an ordinary function. The attribute adds a type of the same
name, which is what `mcp_tools` lists: do not re-export the function next to a
module of the same name (`pub use list_inventory::list_inventory;` beside
`mod list_inventory;` does not compile), import it where the application is
declared.

Tool names use lower-case alphanumeric segments separated by `.`, `_`, or `-`
and must not reuse a core tool name. The description is the documentation
comment of the function, or `description = "…"`; a tool without either does
not compile.

| Option | Default | Purpose |
| --- | --- | --- |
| `permissions(READ_INVENTORY, "read_logs")` | None: every valid token can call the tool | Permissions a token must hold, all of them: `McpPermission` constants, which the tool registers, or keys as strings, each a core or registered permission. |
| `risk = low` | Reported as `low` | `low`, `medium`, or `high`, reported as `riskLevel` by `GET /api/mcp/capabilities`. |
| `confirmation_hint = "…"` | None | Free text reported as `confirmationHint` by `GET /api/mcp/capabilities`. |
| `description = "…"` | The documentation comment | The description of the tool. |

The function receives a `McpContext` and, optionally, the `arguments`
of the call as a second argument: a type marked `#[input]`, whose JSON Schema
is the input schema of the tool. `#[input]` derives `Deserialize`,
`Serialize`, and `JsonSchema` for the type and names its fields in camel case
outside Rust (`product_id` is `productId`). A function that takes the context
alone is a tool without arguments: its input schema is an object that accepts
no property. The result is any `Serialize` value: a struct, a string, a
`serde_json::Value`.

| Context field | Meaning |
| --- | --- |
| `shopify_app`, `shop`, `token_id` | Immutable token scope: the handle of the Shopify app, the shop, and the token. |
| `permissions` | Current grants. |
| `signal` | `CancellationToken` cancelled when the client disconnects or after 30 seconds. |
| `db` | `PgPool`, the shared `sqlx` pool. |
| `shopify` | `ShopifyClient`, the Admin API of the shop. |
| `connectors` | `Connectors`, the providers of the shop. |
| `app` | `OpenShop` handle. |

Nothing is read when the context is built: the access token of the shop is
read at the first Shopify request, and the provider configuration at the first
provider call. `McpContext` belongs to a shop, so what needs one takes the
context itself: `Review::of(&ctx)` and `sync_orders::dispatch(&ctx, …)`.

A string result becomes text content. A JSON object or array becomes formatted
text plus `structuredContent`. Objects already containing a `content` array or
a `structuredContent` key pass through. Numbers and booleans become text
content, and `null` an empty text.

### Input schema validation

Missing `arguments` are treated as `{}`, and `arguments` that are not an object
are rejected. The built-in JSON Schema validator supports `type` (object,
array, string, number, integer, boolean, null), `required`, `properties`,
`additionalProperties: false`, `items`, `minimum`, `maximum`,
`exclusiveMinimum`, `exclusiveMaximum`, `minLength`, `maxLength`, `enum`,
`const`, `anyOf`, `oneOf`, and local `$ref`. Other keywords are ignored. Numeric,
string, object, and array constraints apply only when the schema declares the
matching `type`. It is intentionally not a complete JSON Schema implementation.

The same validator checks the typed input of a flow.

## Custom resources

`#[mcp_resource(uri, name = "…")]` makes an `async` function a resource:

```rust,ignore
use openshop::prelude::*;

/// How inventory sync works.
#[mcp_resource(
    "openshop://warehouse/guide",
    name = "Warehouse guide",
    mime_type = "text/markdown",
    permissions(READ_INVENTORY)
)]
pub async fn warehouse_guide(_ctx: McpContext) -> Result<String> {
    Ok("# Warehouse sync".to_string())
}
```

List the function in the `mcp_resources` of `app!`:
`mcp_resources: [warehouse_guide]`. The URI of the resource is also the
constant `warehouse_guide::URI`.

Resource URIs must be URIs (`scheme://…`), and `name = "…"` is required and
non-empty. The description is the documentation comment of the function, or
`description = "…"`; it is optional. The function receives the
`McpContext` and returns the text, as a `String` or a `&str`, or a
`McpResourceContent { text, mime_type }` to choose the MIME type of that
answer. The MIME type defaults to the `mime_type = "…"` of the attribute and
then to `text/plain`. `permissions(…)` and `risk = …` behave as for tools.

Built-in resources are:

| URI | Permission |
| --- | --- |
| `openshop://docs/log-search` | `read_logs` |
| `openshop://permissions` | None |
| `openshop://tools/openshop.logs.search` | `read_logs` |
| `openshop://runs/recent` | `read_flows` |

`openshop://runs/recent` lists the 20 most recent runs of the token's shop.

## Core tools

| Tool | Permission | Important inputs/defaults |
| --- | --- | --- |
| `openshop.shops.list` | None | Lists installed shops for the token app. |
| `openshop.admin.graphql` | `shopify_admin_graphql` | `shop` and `query` required; `variables` defaults `{}`; `apiVersion` is `YYYY-MM` or `latest` and defaults to the latest stable version. |
| `openshop.logs.search` | `read_logs` | `runId` required; `q` uses the [log query syntax](logging.md#log-query-syntax); `levels` defaults to `info,warn,error`; `includeContext` defaults to true; `limit` 50, max 200. |
| `openshop.flows.list` | `read_flows` | Includes input schemas and configured crons. |
| `openshop.flows.run` | `run_flows` | `flow` required; `input` defaults `{}`; runs only for token shop. |
| `openshop.crons.list` | `read_crons` | Includes per-token-shop override state. |
| `openshop.crons.set_enabled` | `manage_crons` | Requires `key` and boolean `enabled`. |

The GraphQL tool accepts only shops installed for the token's app handle. Its
result includes `shop`, `apiVersion`, the HTTP `status`, `ok`, and Shopify's
parsed response; unlike the regular Shopify client, it returns GraphQL error
payloads to the MCP caller.

## Visibility and audit

`tools/list` and `resources/list` omit capabilities whose permissions are not
fully granted. Calling a known but unauthorized capability still fails closed.
Tool and resource calls are audited with token, capability, permissions, status
(`success`, `denied`, or `error`), duration, request ID, and target shop where
applicable. Calls rejected for an unknown name or invalid arguments are not
audited.

## Limits and JSON-RPC errors

Requests are limited to 1,000,000 bytes, independently of the server's request
body limit. Tool and resource executions time out after 30,000 ms and their
`signal` is cancelled.

| Error | Meaning |
| --- | --- |
| HTTP `400`, JSON-RPC `-32600` | Invalid JSON-RPC request. |
| HTTP `413` | Payload exceeds the limit. |
| `-32601` | Unsupported method. |
| `-32602` | Unknown capability or invalid arguments. |
| `-32003` | Permission denied. |
| `-32000` | Tool/resource error, panic, or timeout. |

Application-level JSON-RPC errors normally use HTTP 200. Authentication and
payload errors use their explicit HTTP status and a body of the form
`{ "error": "<message>" }`.

## Without the attribute

`#[mcp_tool]` and `#[mcp_resource]` are built on `McpTool` and
`McpResource`, which can be used directly when a tool or a resource
is only known at run time:

```rust,ignore
use openshop::prelude::*;
use openshop::builder::{McpResource, McpTool};
use openshop::McpResourceContent;

pub fn inventory_tool() -> McpTool {
    McpTool::new("List warehouse inventory.", |ctx, _input| async move {
        Ok(json!({ "shop": ctx.shop, "items": [] }))
    })
    .permissions([READ_INVENTORY])
    .risk(McpRiskLevel::Low)
    .input_schema(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {},
    }))
}

pub fn warehouse_guide() -> McpResource {
    McpResource::new("Warehouse guide", |_ctx| async move {
        Ok(McpResourceContent::from("# Warehouse sync"))
    })
    .description("How inventory sync works.")
    .mime_type("text/markdown")
    .permissions([READ_INVENTORY])
}
```

Register them on the builder with
`.mcp_tool("warehouse.inventory.list", inventory_tool())` and
`.mcp_resource("openshop://warehouse/guide", warehouse_guide())`, or as part of
the `McpConfig` of the `mcp` setting:
`McpConfig::new().tool(name, tool).resource(uri, resource)`. On the builder,
`.mcp(…)` adds to the permissions, tools, and resources already registered; one
registered twice is refused when the configuration is built, for example with
`[openshop] Invalid config: mcp.tools.warehouse.inventory.list is registered twice`.

The tool function receives the `McpContext` and the arguments as a
`JsonMap`; the resource function returns a `McpResourceContent`.

| Builder method | Default | Purpose |
| --- | --- | --- |
| `.permissions([...])` | None: every valid token can call the tool or read the resource | Permissions a token must hold, all of them: `McpPermission` constants, which are registered with the tool or the resource, or keys as strings. |
| `.input_schema(value)` (tools) | Any JSON object | JSON Schema of the `arguments`. |
| `.risk(level)` | Reported as `low` | `McpRiskLevel::Low`, `Medium`, or `High`. |
| `.confirmation_hint(text)` (tools) | None | Free text reported as `confirmationHint`. |
| `.description(text)`, `.mime_type(text)` (resources) | None | The description and the default MIME type of the resource. |
