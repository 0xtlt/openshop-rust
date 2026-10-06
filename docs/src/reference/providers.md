# Providers

To add a service, see [Connect an external service](../guides/define-provider.md).

Providers describe external services used by flows. A provider is a struct:
its fields are the configuration a merchant saves for a shop, and its methods
are what flows do with that configuration.

```rust,ignore
use openshop::{FlowContext, ProviderConfig, Result, provider, validators};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Order {
    pub id: String,
}

#[derive(ProviderConfig)]
pub struct Warehouse {
    #[field(label = "API URL", validate = validators::url())]
    api_url: String,
    #[field(label = "API key", password)]
    api_key: String,
    #[field(label = "Batch size", validate = validators::integer_min(1))]
    batch_size: Option<u32>,
}

#[provider("warehouse")]
impl Warehouse {
    #[checker]
    async fn check(&self) -> Result<bool> {
        // Call `{api_url}/health` with your HTTP client and report the outcome.
        Ok(!self.api_key.is_empty())
    }

    /// Sends orders and returns how many were accepted.
    pub async fn push(&self, orders: Vec<Order>) -> Result<usize> {
        // POST `orders` to `{api_url}/orders` in batches of `batch_size`.
        let batches = orders.chunks(self.batch_size.unwrap_or(100) as usize).count();
        println!("{} orders to {} in {batches} batches", orders.len(), self.api_url);
        Ok(orders.len())
    }
}

async fn push(ctx: &FlowContext, orders: &Vec<Order>) -> Result<usize> {
    Warehouse::on(&ctx.connectors).push(orders).await
}
```

Register it with `.provider(Warehouse::provider())` on the configuration
builder, under the name given to `#[provider]`.

## Provider structs

### The configuration

`#[derive(ProviderConfig)]` makes each field of the struct a field of the form
shown in the admin. The value is saved under the name of the field in camel
case: `api_url` is `apiUrl`.

| Rust type | Input | |
| --- | --- | --- |
| `String` | Text | With `password`, a secret; with `select(…)`, a list of choices. |
| `bool` | Checkbox | |
| Integer and float types | Number | Add `validators::integer_min(…)` to an integer field, so that `2.5` is refused when the configuration is saved. |
| `Option<T>` | The input of `T` | The field may be left blank. Every other field is required. |

Options go in `#[field(…)]`:

| Option | |
| --- | --- |
| `label = "API URL"` | The label of the input. Required. |
| `placeholder = "https://…"` | |
| `password` | A secret: never returned by the Admin API, and kept when the form is saved without it. |
| `select("Europe" = "eu", "United States" = "us")` | A list of choices, each a label and the value saved. |
| `validate = validators::url()` | A [validator](#validators). The value it returns is the one saved. |
| `name = "endpoint"` | The key the value is saved under, instead of the field name in camel case. |

A struct without fields, `struct Clock;`, is a provider without configuration.
The derive also implements `Deserialize` from the saved configuration: do not
derive it yourself. The same fields are available as
`Warehouse::fields()`, and `Warehouse::from_config(map)` reads a saved
configuration, through the `ProviderConfig` trait.

### The methods

`#[provider("warehouse")]` goes on the `impl` block of the struct. `&self` is
the configuration of the shop the method runs for.

| In the block | |
| --- | --- |
| `pub async fn name(&self, …) -> Result<T>` | A method of the provider. Its arguments and `T` travel as JSON: they implement `Serialize` and `Deserialize`. Arguments are taken by value. The error type is anything that converts to `anyhow::Error`. |
| `#[checker] async fn check(&self) -> Result<bool>` | The [connection check](#checker) of the admin. At most one. |
| A private method, a function without `&self` | An ordinary helper, not part of the provider. |

The block keeps its methods: on a value, `warehouse.push(orders).await` runs
the code directly, which is how a unit test exercises it. The macro adds:

| Added | |
| --- | --- |
| `Warehouse::provider()` | The [`Provider`](#providers-without-a-struct) to register. It can still be extended, for example with `.transformer(…)`. |
| `Warehouse::on(&connectors)` | The same public methods, run with the configuration the shop saved: `Warehouse::on(&ctx.connectors).push(&orders)`. An argument is passed by value or by reference. |
| `Warehouse::PUSH`, one constant per method | A [`ProviderCall`](#typed-calls), for `connectors.invoke(…)` and for the [fake](testing.md#configure-provider-fakes) of a test: `ctx.fakes.for_call(Warehouse::PUSH)`. |

Calls go through the connectors of the shop, so a test that fakes the provider
replaces every one of them, and `connectors.call("warehouse", "push", …)` still
reaches the method by name. A method with several arguments receives them as a
JSON array, one without argument receives `null`.

What is checked when:

- The compiler checks the arguments and the result of
  `Warehouse::on(..).push(..)`, and that the method exists.
- `build()` checks that the provider is registered under its own name: with
  `.provider_as("main", Warehouse::provider())` it fails, as described under
  [Typed calls](#typed-calls).
- Whether the shop saved its configuration is only known when the call
  happens. Without it, the call fails with
  `Invalid configuration for provider "warehouse": missing field `apiUrl``
  before the method runs.

## Providers without a struct

The struct form is built on `Provider`, which can be used directly when the
fields or the methods are only known at run time:

```rust,ignore
use openshop::{FieldDef, JsonMap, Provider, Value, json};

pub fn warehouse() -> Provider {
    Provider::new("warehouse")
        .field("apiUrl", FieldDef::text("API URL"))
        .field("apiKey", FieldDef::password("API key"))
        .checker(|config| async move {
            Ok(config.get("apiUrl").and_then(Value::as_str).is_some())
        })
        .typed_method("push", |config, rows: Vec<JsonMap>| async move {
            let api_url = config.get("apiUrl").and_then(Value::as_str).unwrap_or_default();
            Ok(json!({ "accepted": rows.len(), "endpoint": api_url }))
        })
}
```

Register it with `.provider(warehouse())`. It is stored under `provider.name`;
`.provider_as(key, provider)` uses another key. The sections below describe
this form: [field definitions](#field-definitions),
[methods](#methods-and-connectors) and [typed calls](#typed-calls).

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
| `.handle(call, f)` | `Fn(C, A) -> impl Future<Output = Result<R>>` | Typed configuration, arguments and result, named by a `ProviderCall`. See [Typed calls](#typed-calls). |

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
| `connectors.invoke(call, &args)` | `Result<R>` for a `ProviderCall<A, R>`. See [Typed calls](#typed-calls). |
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

## Typed calls

`.method` and `.typed_method` name a method with a string and receive the saved
config as a JSON object, so a misspelled name or key only fails when the call
happens. A `ProviderCall` constant moves those checks to compile time. It
carries the provider key, the method name, and the types of the arguments and
of the result, and it is shared by the provider that implements the method and
by the code that calls it. `#[provider]` declares one per method; with the
builder, you declare them:

```rust,ignore
use openshop::{FieldDef, FlowContext, Provider, ProviderCall, Result, validators};
use serde::{Deserialize, Serialize};

/// The saved config, named after the fields of the provider.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WarehouseConfig {
    pub api_url: String,
    pub api_key: String,
    pub batch_size: Option<u32>,
}

#[derive(Serialize, Deserialize)]
pub struct Order {
    pub id: String,
}

pub const PUSH: ProviderCall<Vec<Order>, usize> = ProviderCall::new("warehouse", "push");

pub fn warehouse() -> Provider {
    Provider::new("warehouse")
        .field("apiUrl", FieldDef::text("API URL").validate(validators::url()))
        .field("apiKey", FieldDef::password("API key"))
        .field(
            "batchSize",
            FieldDef::number("Batch size").optional().validate(validators::integer_min(1)),
        )
        .typed_checker(|config: WarehouseConfig| async move {
            // Call `{api_url}/health` with your HTTP client and report the outcome.
            Ok(!config.api_key.is_empty())
        })
        .handle(PUSH, |config: WarehouseConfig, orders| async move {
            // POST `orders` to `{api_url}/orders` in batches of `batch_size`.
            let batches = orders.chunks(config.batch_size.unwrap_or(100) as usize).count();
            println!("{} orders to {} in {batches} batches", orders.len(), config.api_url);
            Ok(orders.len())
        })
}

async fn push(ctx: &FlowContext, orders: &[Order]) -> Result<usize> {
    ctx.connectors.invoke(PUSH, orders).await
}
```

| API | Signature | Notes |
| --- | --- | --- |
| `ProviderCall::<A, R>::new(provider, method)` | `const fn` | `provider` is the key the provider is registered under. `.provider()` and `.method()` return the two names. |
| `.handle(call, f)` | `f: Fn(C, A) -> impl Future<Output = Result<R>>` | Registers `call.method()`. `C: DeserializeOwned`. |
| `.typed_checker(f)` | `f: Fn(C) -> impl Future<Output = Result<bool>>` | The [checker](#checker) with a typed config. |
| `connectors.invoke(call, &args)` | `Result<R>` | `args` is a reference to `A`, or to what `A` borrows as: `&[T]` for a `Vec<T>`, `&str` for a `String`. |

`A` and `R` are used on both sides of the call, so they implement both
`Serialize` and `Deserialize`. Use `()` for a method without arguments:
`connectors.invoke(PING, &())`.

The config type `C` is any `serde` type whose fields are the fields of the
provider:

| Field | Rust type |
| --- | --- |
| `text`, `password`, `select` | `String` |
| `number` | An integer or float type. A whole number is saved as an integer; add `validators::integer_min(…)` when the type is an integer, so that `2.5` is refused when the config is saved. |
| `checkbox` | `bool` |
| Optional field | `Option<T>`: a blank optional value is not saved. |

What is and is not checked:

- The compiler checks the name of the method and the types of the arguments
  and of the result at every `invoke`.
- `build()` checks that each call names the key its provider is registered
  under. With `.provider_as("mainWarehouse", warehouse())` it fails with
  `[openshop] Invalid config: providers.mainWarehouse.methods.push implements a call declared for provider "warehouse"; declare it as ProviderCall::new("mainWarehouse", "push")`.
- Whether the provider is registered at all, and whether the shop saved its
  config, is only known when the call happens. A config that does not fit `C`,
  such as the empty object of a shop that saved nothing, fails with
  `Invalid configuration for provider "warehouse": missing field `apiUrl``
  before the method runs.

A typed call is still an ordinary method. `connectors.call("warehouse", "push", …)`
reaches it, with arguments that do not deserialize refused as
`Invalid arguments for "warehouse.push": …`, and tests replace it with a
[fake](testing.md#configure-provider-fakes). A result that is not an `R`, for
example the `null` of a fake without a rule, fails with
`Invalid result from "warehouse.push": …`.

## Checker

`.checker(…)` takes a `Fn(JsonMap) -> impl Future<Output = Result<bool>>`;
`.typed_checker(…)` takes the config as a [typed value](#typed-calls) instead,
and answers with an error when the saved config does not fit the type. The
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
