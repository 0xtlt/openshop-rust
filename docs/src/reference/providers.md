# Providers

To add a service, see [Connect an external service](../guides/define-provider.md).

Providers describe external services used by flows.

```rust,ignore
use openshop::{FieldDef, JsonMap, Provider, Value, json};

pub fn warehouse() -> Provider {
    Provider::new("warehouse")
        .field("apiUrl", FieldDef::text("API URL"))
        .field("apiKey", FieldDef::password("API key"))
        .checker(|config| async move {
            // Call `{apiUrl}/health` with your HTTP client and report the outcome.
            Ok(config.get("apiUrl").and_then(Value::as_str).is_some())
        })
        .typed_method("push", |config, rows: Vec<JsonMap>| async move {
            let api_url = config.get("apiUrl").and_then(Value::as_str).unwrap_or_default();
            // POST `rows` to `{api_url}/orders` with `apiKey` as bearer token.
            Ok(json!({ "accepted": rows.len(), "endpoint": api_url }))
        })
}
```

Register the provider with `.provider(warehouse())` on the configuration
builder. It is stored under `provider.name`; `.provider_as(key, provider)` uses
another key.

## Field definitions

`.field(name, definition)` adds a field. All fields require a type and a
non-empty label.

| Constructor | Type | Saved value | Notes |
| --- | --- | --- | --- |
| `FieldDef::text(label)` | `text` | String-like input | Supports `.placeholder(…)`. |
| `FieldDef::password(label)` | `password` | Secret input | Omitted from read responses; an empty update preserves the stored value. |
| `FieldDef::number(label)` | `number` | Number | A non-empty string is converted to a number. A string that is not a number counts as missing. |
| `FieldDef::select(label, options)` | `select` | Selected string | Requires non-empty, uniquely valued `FieldOption::new(label, value)` options. |
| `FieldDef::checkbox(label)` | `checkbox` | Boolean | The string `"true"` is true; other strings are false. |

Fields are required by default. Call `.optional()` or `.required(false)` to
make a field optional. `.validate(…)` accepts a validator; the value it returns
becomes the saved value.

### Validators

A validator is any `Fn(&Value) -> Result<Value, String>`: it returns the value
to save or a short summary such as `must be a URL`. `openshop::validators`
provides the common ones:

| Validator | Accepts | Summary on failure |
| --- | --- | --- |
| `validators::string()` | Any string | `must be a string (was …)` |
| `validators::non_empty()` | A string with at least one character | `must be non-empty`, or `must be a string (was …)` |
| `validators::url()` | An absolute URL | `must be a URL string (was …)` |
| `validators::number()` | Any finite number | `must be a number (was …)` |
| `validators::number_range(min, max)` | A number within `min..=max` | `must be at least <min> (was …)`, `must be at most <max> (was …)`, or `must be a number (was …)` |
| `validators::integer_min(min)` | A whole number greater than or equal to `min` | `must be an integer (was …)`, `must be at least <min> (was …)`, or `must be a number (was …)` |
| `validators::one_of([..])` | One of the listed strings | `must be one of "a", "b" (was …)` |

The ready-made validators return the value unchanged.

```rust,ignore
use openshop::{FieldDef, FieldOption, Value, validators};

fn fields() -> Vec<(&'static str, FieldDef)> {
    vec![
        ("apiUrl", FieldDef::text("API URL").validate(validators::url())),
        ("batchSize", FieldDef::number("Batch size").validate(validators::number_range(1.0, 1000.0))),
        (
            "region",
            FieldDef::select("Region", [FieldOption::new("Europe", "eu"), FieldOption::new("United States", "us")]),
        ),
        (
            "warehouseCode",
            FieldDef::text("Warehouse code").optional().validate(|value: &Value| match value.as_str() {
                Some(code) if code.len() == 4 => Ok(Value::from(code.to_uppercase())),
                _ => Err("must be a 4-character code".to_string()),
            }),
        ),
    ]
}
```

## Save pipeline

When a provider configuration is saved, OpenShop:

1. keeps an existing password when the submitted value is missing or empty;
2. coerces number and checkbox inputs;
3. checks required fields and drops everything that is not a declared field;
4. calls the transformer with the collected values, when one is defined;
5. requires the transformer result to be a JSON object;
6. checks required fields again and runs each validator;
7. encrypts and upserts the resulting object for the current app and shop.

`.transformer(…)` can normalize fields or add provider-specific values:

```rust,ignore
use openshop::{FieldDef, Provider, Value};

pub fn warehouse() -> Provider {
    Provider::new("warehouse")
        .field("apiUrl", FieldDef::text("API URL"))
        .transformer(|mut data| {
            if let Some(Value::String(api_url)) = data.get("apiUrl").cloned() {
                data.insert("apiUrl".into(), Value::from(api_url.trim_end_matches('/')));
            }
            Value::Object(data)
        })
}
```

Errors are explicit: missing values report `Field "name" is required`,
validation errors report `Field "name": <summary>`, and non-object transformer
results report `Provider transformer must return an object`. The Admin API
returns them with status `400`.

## Methods and connectors

The saved config is the first argument of each provider method.

| Registration | Method signature | Arguments and result |
| --- | --- | --- |
| `.method(name, f)` | `Fn(JsonMap, Value) -> impl Future<Output = Result<Value>>` | Raw JSON. |
| `.typed_method(name, f)` | `Fn(JsonMap, A) -> impl Future<Output = Result<R>>` | `A: DeserializeOwned`, `R: Serialize`. Arguments that do not deserialize fail with `Invalid arguments for "<name>": …`. |

Flows call the method through `ctx.connectors` without the config argument:

```rust,ignore
use openshop::{FlowContext, JsonMap, Result, Value};

async fn push(ctx: &FlowContext, rows: &[JsonMap]) -> Result<u64> {
    // Raw result.
    let result: Value = ctx.connectors.call("warehouse", "push", rows).await?;

    // Typed result.
    #[derive(serde::Deserialize)]
    struct Pushed {
        accepted: u64,
    }
    let pushed: Pushed = ctx.connectors.call_as("warehouse", "push", rows).await?;

    Ok(pushed.accepted.max(result["accepted"].as_u64().unwrap_or(0)))
}
```

| Call | Returns |
| --- | --- |
| `connectors.call(provider, method, args)` | `Result<Value>` |
| `connectors.call_as::<T>(provider, method, args)` | `Result<T>` |
| `connectors.get(provider)` | `Result<&Connector>`, with the same `call` / `call_as` plus `has(method)` and `method_names()` |
| `connectors.names()` | Iterator over the provider keys |

`args` is one serializable value. Pass a tuple or a struct when a method needs
several arguments. An unregistered provider fails with
`Unknown provider "<name>"` and an unregistered method with
`Unknown provider method "<name>"`.

OpenShop loads the app- and shop-specific config before building connectors. A
shop without a saved configuration gets an empty object. It does not
automatically run the checker before a method call.

Connectors are also available outside flows: `ctx.connectors` in admin
functions, `ctx.for_shop(…)` in server routes, and
`app.connectors(shop, shopify_app)` everywhere else.

## Checker

`.checker(…)` takes a `Fn(JsonMap) -> impl Future<Output = Result<bool>>`. The
admin check endpoint saves `lastCheckedAt` and `lastCheckOk` only when a config
row already exists. A checker error produces `{ ok: false, error }` with HTTP
500; returning `false` produces `{ ok: false }` with HTTP 200. A provider
without a checker answers `400` with `No checker defined`.

`GET /api/providers` also returns `configured`. It is true when every required
field has a saved value, including password fields that are omitted from the
public config. The dashboard uses that flag separately from the last check:
missing required values show **Not configured**, a complete config with no
check yet shows **Configured**, `lastCheckOk: true` shows **Connected**, and
`lastCheckOk: false` shows **Error**. Saving config does not run the checker.

## Secret behavior

Password values are excluded from provider read responses. Their field metadata
contains `hasValue` instead. The whole stored config is encrypted when
`ENCRYPTION_KEY` is set. Development without a key logs a warning and stores
plaintext; production without a key fails. The key must contain exactly 64 hex
characters (32 bytes). Preserve it across deployments. See
[Security](security.md#encryption-at-rest).
