# Providers

To add a service, see [Connect an external service](../guides/define-provider.md).

Providers describe external services used by flows. A provider is a struct:
its fields are the configuration a merchant saves for a shop, and its methods
are what flows do with that configuration.

```rust,ignore
use openshop::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct Order {
    pub id: String,
}

#[derive(ProviderConfig)]
pub struct Warehouse {
    #[field(label = "API URL", placeholder = "https://warehouse.example.com/api", url)]
    api_url: String,
    #[field(label = "API key", password, non_empty)]
    api_key: String,
    #[field(range(max = 1000), validate = validators::integer_min(1))]
    batch_size: Option<u32>,
}

#[provider]
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

List it in the `providers` of [`app!`](configuration.md):
`providers: [Warehouse]`. It is registered under the name of the provider,
here `warehouse`, after the struct.

## Provider structs

### The configuration

`#[derive(ProviderConfig)]` makes each field of the struct a field of the form
shown in the admin. The value is saved under the name of the field in camel
case: `api_url` is `apiUrl`.

| Rust type | Input | |
| --- | --- | --- |
| `String` | Text | With `password`, a secret; with `select(…)`, a list of choices. |
| `bool` | Checkbox | |
| Integer and float types | Number | Add `validate = validators::integer_min(…)` to an integer field, so that `2.5` is refused when the configuration is saved. |
| `Option<T>` | The input of `T` | The field may be left blank. Every other field is required. |

Options go in `#[field(…)]`. All of them are optional, and so is the
attribute:

| Option | |
| --- | --- |
| `label = "API URL"` | The label of the input. Left out, the field is labeled after its name: `batch_size` is `Batch size`. |
| `placeholder = "https://…"` | |
| `password` | A secret: never returned by the Admin API, and kept when the form is saved without it. |
| `select("Europe" = "eu", "United States" = "us")` | A list of choices, each a label and the value saved. |
| `url` | The value must be an absolute `http` or `https` URL. |
| `non_empty` | The value must be a string with at least one character. |
| `range(min = 1, max = 1000)` | The value must be a number within the bounds. Either bound may be left out. |
| `length(min = 1, max = 8)` | The value must be a string whose number of characters is within the bounds. Either bound may be left out. |
| `validate = validators::integer_min(1)` | A [validator](#validators), for anything else. The value it returns is the one saved. |
| `name = "endpoint"` | The key the value is saved under, instead of the field name in camel case. |

`url`, `non_empty`, `range(…)` and `length(…)` are the constraints of a field:
each applies the [validator](#validators) of the same name. They are checked
in the order they are written, then `validate` runs. A field is `password` or
`select(…)`, not both.

A struct without fields, `struct Clock;`, is a provider without configuration.
The derive also implements `Deserialize` from the saved configuration: do not
derive it yourself. The same fields are available as
`Warehouse::fields()`, and `Warehouse::from_config(map)` reads a saved
configuration, through the `ProviderConfig` trait.

### The methods

`#[provider]` goes on the `impl` block of the struct. The provider is named
after the struct, with a lowercase initial: `Warehouse` is `warehouse`.
`#[provider("wms")]` states another name. `&self` is the configuration of the
shop the method runs for.

| In the block | |
| --- | --- |
| `pub async fn name(&self, …) -> Result<T>` | A method of the provider. Its arguments and `T` travel as JSON: they implement `Serialize` and `Deserialize`. Arguments are taken by value. The error type is anything that converts to `anyhow::Error`. |
| `#[checker] async fn check(&self) -> Result<bool>` | The [connection check](#checker) of the admin. At most one. |
| A private method, a function without `&self` | An ordinary helper, not part of the provider. |

The block keeps its methods: on a value, `warehouse.push(orders).await` runs
the code directly, which is how a unit test exercises it. The macro adds:

| Added | |
| --- | --- |
| `Warehouse::NAME` | The name of the provider, `"warehouse"`. |
| `Warehouse::provider()` | The [`Provider`](#providers-without-a-struct) the struct describes. The struct itself is what `providers` lists; take the `Provider` to extend it, for example with `.transformer(…)`, and register the result with `.provider(…)` on the [builder](configuration.md#builder-methods) instead. |
| `Warehouse::on(&connectors)` | The same public methods, run with the configuration the shop saved: `Warehouse::on(&ctx.connectors).push(&orders)`. An argument is passed by value or by reference. |
| `Warehouse::PUSH`, one constant per method | A [`ProviderCall`](#typed-calls), for the [fake](testing.md#configure-provider-fakes) of a test: `ctx.fakes.for_call(Warehouse::PUSH)`. |

Calls go through the connectors of the shop, so a test that fakes the provider
replaces every one of them, and `connectors.call("warehouse", "push", …)` still
reaches the method by name. A method with several arguments receives them as a
JSON array, one without argument receives `null`.

What is checked when:

- The compiler checks the arguments and the result of
  `Warehouse::on(..).push(..)`, and that the method exists.
- A provider listed in `app!` is registered under its own name. On the
  builder, `build()` checks it: with
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
use openshop::prelude::*;
use openshop::builder::{Field, Provider};
use openshop::JsonMap;

pub fn warehouse() -> Provider {
    Provider::new("warehouse")
        .field("apiUrl", Field::text("API URL"))
        .field("apiKey", Field::password("API key"))
        .checker(|config| async move {
            Ok(config.get("apiUrl").and_then(Value::as_str).is_some())
        })
        .typed_method("push", |config, rows: Vec<JsonMap>| async move {
            let api_url = config.get("apiUrl").and_then(Value::as_str).unwrap_or_default();
            Ok(json!({ "accepted": rows.len(), "endpoint": api_url }))
        })
}
```

Register it on the [builder](configuration.md#builder-methods), for example
with `APP.builder()?.provider(warehouse())`. It is stored under
`provider.name`; `.provider_as(key, provider)` uses another key. The sections
below describe this form: [field definitions](#field-definitions),
[methods](#methods-and-connectors) and [typed calls](#typed-calls).

## Field definitions

`.field(name, definition)` adds a field. All fields require a type and a
non-empty label.

| Constructor | Type | Saved value | Notes |
| --- | --- | --- | --- |
| `Field::text(label)` | `text` | String-like input | Supports `.placeholder(…)`. |
| `Field::password(label)` | `password` | Secret input | Omitted from read responses; an empty update preserves the stored value. |
| `Field::number(label)` | `number` | Number | A non-empty string is converted to a number. A string that is not a number counts as missing. |
| `Field::select(label, options)` | `select` | Selected string | Requires non-empty, uniquely valued `FieldOption::new(label, value)` options. |
| `Field::checkbox(label)` | `checkbox` | Boolean | The string `"true"` is true; other strings are false. |

Fields are required by default. Call `.optional()` or `.required(false)` to
make a field optional. `.validate(…)` accepts a validator; the value it returns
becomes the saved value. Called again, it adds a validator that runs after the
ones already set, on the value they return.

### What is checked when a configuration is saved

Before a validator runs, every submitted value is checked against the type of
its field. A value of another type is refused with `400` and
`{ "error": "Field \"retries\": must be a number (was \"abc\")" }`:

| Field | Accepts |
| --- | --- |
| `text`, `password` | A string. |
| `number` | A number, or a string that is one (`"3"`, `" 2.5 "`), saved as a number. |
| `checkbox` | `true`, `false`, `"true"` or `"false"`, saved as a boolean. Anything else, `"yes"` or `1` included, is refused. |
| `select` | A string equal to the `value` of one of the options. |

A null or an empty string is no value: a required field then fails with
`Field "…" is required`, and an optional one is not saved. A blank password
keeps the one already saved, as it is: it is not transformed or validated
again. A validator that refuses a password gives `Field "…" is invalid`, since
its summary could repeat the secret. Keys that are not fields of the provider are
ignored and not saved. A transformer runs on the checked values, before the
validators.

### Validators

A validator is any `Fn(&Value) -> Result<Value, String>`: it returns the value
to save or a short summary such as `must be a URL`. `openshop::validators`
provides the common ones:

| Validator | Accepts | Summary on failure |
| --- | --- | --- |
| `validators::string()` | Any string | `must be a string (was …)` |
| `validators::non_empty()` | A string with at least one character | `must be non-empty`, or `must be a string (was …)` |
| `validators::url()` | An absolute `http` or `https` URL with a host | `must be a URL string (was …)`, or `must be an http or https URL (was …)` |
| `validators::public_url()` | Like `url()`, and the host is not on the server's own network | `must be a public http or https URL (was …)`, plus those of `url()` |
| `validators::number()` | Any finite number | `must be a number (was …)` |
| `validators::number_range(min, max)` | A number within `min..=max` | `must be at least <min> (was …)`, `must be at most <max> (was …)`, or `must be a number (was …)` |
| `validators::range(min, max)` | A number within the bounds that are given, each an `Option<f64>` | `must be at least <min> (was …)`, `must be at most <max> (was …)`, or `must be a number (was …)` |
| `validators::length(min, max)` | A string whose number of characters is within the bounds that are given, each an `Option<usize>` | `must be at least <min> characters (was …)`, `must be at most <max> characters (was …)`, or `must be a string (was …)` |
| `validators::integer_min(min)` | A whole number greater than or equal to `min` | `must be an integer (was …)`, `must be at least <min> (was …)`, or `must be a number (was …)` |
| `validators::one_of([..])` | One of the listed strings | `must be one of "a", "b" (was …)` |

`url()` refuses `file:`, `gopher:`, `ftp:` and every other scheme, since the
server fetches the URLs a merchant saves. It accepts any host, `localhost` and
private addresses included, which a provider that talks to a service on the
same network needs. When the URL must point at the internet, as with an
endpoint a merchant types in, use
`validate = validators::public_url()`: it also refuses `localhost`,
`*.localhost`, `metadata.google.internal` and hosts written as an IP address in
a loopback, private (`10/8`, `172.16/12`, `192.168/16`, `fc00::/7`),
link-local (`169.254/16`, `fe80::/10`), site-local (`fec0::/10`), shared
(`100.64/10`), multicast or unspecified range, and the IPv6 ranges that carry
an IPv4 address (IPv4-mapped and -compatible, NAT64, 6to4, Teredo). It only reads the text of the URL: a public name that
resolves to a private address (DNS rebinding) passes. If that matters, also
check the address the connection is made to when the request is sent.

The ready-made validators return the value unchanged. On a
[provider struct](#the-configuration), the `url`, `non_empty`, `range(…)` and
`length(…)` options of `#[field(…)]` apply `validators::url()`,
`validators::non_empty()`, `validators::range(…)` and
`validators::length(…)`.

```rust,ignore
use openshop::prelude::*;
use openshop::builder::{Field, FieldOption};

fn fields() -> Vec<(&'static str, Field)> {
    vec![
        ("apiUrl", Field::text("API URL").validate(validators::url())),
        ("batchSize", Field::number("Batch size").validate(validators::number_range(1.0, 1000.0))),
        (
            "region",
            Field::select("Region", [FieldOption::new("Europe", "eu"), FieldOption::new("United States", "us")]),
        ),
        (
            "warehouseCode",
            Field::text("Warehouse code").optional().validate(|value: &Value| match value.as_str() {
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
use openshop::prelude::*;
use openshop::builder::{Field, Provider};

pub fn warehouse() -> Provider {
    Provider::new("warehouse")
        .field("apiUrl", Field::text("API URL"))
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

Flows call the method by name through `ctx.connectors`, without the config
argument:

```rust,ignore
use openshop::prelude::*;
use openshop::JsonMap;
use serde::Deserialize;

#[derive(Deserialize)]
struct Pushed {
    accepted: u64,
}

async fn push(ctx: &FlowContext, rows: &[JsonMap]) -> Result<u64> {
    // Raw result.
    let result: Value = ctx.connectors.call("warehouse", "push", rows).await?;

    // Typed result.
    let pushed: Pushed = ctx.connectors.call_as("warehouse", "push", rows).await?;

    Ok(pushed.accepted.max(result["accepted"].as_u64().unwrap_or(0)))
}
```

| Call | Returns |
| --- | --- |
| `connectors.call(provider, method, args).await` | `Result<Value>` |
| `connectors.call_as::<T>(provider, method, args).await` | `Result<T>`. A result that is not a `T` fails with `Invalid result from "<provider>.<method>": …`. |
| `connectors.get(provider).await` | `Result<&Connector>`, with `call(method, args)` and `call_as::<T>(method, args)` plus `has(method)` and `method_names()` |
| `connectors.names()` | Iterator over the provider keys |

These are the calls by name. For a [provider struct](#the-methods), prefer
`Warehouse::on(&ctx.connectors).push(&orders)`, which the compiler checks.

`args` is one serializable value. Pass a tuple or a struct when a method needs
several arguments. An unregistered provider fails with
`Unknown provider "<name>"` and an unregistered method with
`Unknown provider method "<name>"`.

The connectors of a context read nothing when the context is built: the
configuration the shop saved for the app is loaded at the first call, once,
and kept. A shop without a saved configuration gets an empty object. The
checker is not run before a method call.

Every context that belongs to a shop has `ctx.connectors`: flows, webhooks,
app proxies, admin functions, and MCP tools and resources. In a server route,
`ctx.for_shop(…)` returns a context that has it. For code without a context,
`app.connectors(shop, shopify_app)` returns the same `Connectors`.

## Typed calls

A `ProviderCall` constant names a method once: the provider key, the method
name, and the types of the arguments and of the result. `#[provider]` declares
one per method, such as `Warehouse::PUSH`. With the builder, you declare them.
`.handle(call, f)` then implements the method with the saved config as a typed
value: the compiler checks every field `f` reads, where `.method` and
`.typed_method` hand it a JSON object whose keys are only looked up when the
call happens. A test takes the [fake](testing.md#configure-provider-fakes) of
the method from the same constant.

```rust,ignore
use openshop::prelude::*;
use openshop::builder::{Field, Provider, ProviderCall};
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
        .field("apiUrl", Field::text("API URL").validate(validators::url()))
        .field("apiKey", Field::password("API key"))
        .field(
            "batchSize",
            Field::number("Batch size").optional().validate(validators::integer_min(1)),
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
    ctx.connectors.call_as(PUSH.provider(), PUSH.method(), orders).await
}
```

| API | Signature | Notes |
| --- | --- | --- |
| `ProviderCall::<A, R>::new(provider, method)` | `const fn` | `provider` is the key the provider is registered under. `.provider()` and `.method()` return the two names. |
| `.handle(call, f)` | `f: Fn(C, A) -> impl Future<Output = Result<R>>` | Registers `call.method()`. `C: DeserializeOwned`. |
| `.typed_checker(f)` | `f: Fn(C) -> impl Future<Output = Result<bool>>` | The [checker](#checker) with a typed config. |
| `ctx.fakes.for_call(call)` | `TypedFake<A, R>` | The [fake](testing.md#configure-provider-fakes) of the method in a test: it returns an `R` and records its arguments as an `A`. |

`A` and `R` travel as JSON between the caller and the method, so they
implement both `Serialize` and `Deserialize`. Use `()` for a method without
arguments, and call it with `()`: `connectors.call("warehouse", "ping", ())`.

The config type `C` is any `serde` type whose fields are the fields of the
provider:

| Field | Rust type |
| --- | --- |
| `text`, `password`, `select` | `String` |
| `number` | An integer or float type. A whole number is saved as an integer; add `validators::integer_min(…)` when the type is an integer, so that `2.5` is refused when the config is saved. |
| `checkbox` | `bool` |
| Optional field | `Option<T>`: a blank optional value is not saved. |

What is and is not checked:

- The compiler checks the function given to `.handle` against the constant:
  it takes an `A` and returns an `R`. It also checks what a test gives to and
  reads from `ctx.fakes.for_call(…)`.
- A call by name, `connectors.call(…)` or `connectors.call_as(…)`, is only
  checked when it happens. The calls of a provider struct,
  `Warehouse::on(..).push(..)`, are checked by the compiler.
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
[fake](testing.md#configure-provider-fakes). With `connectors.call_as(…)` and
with `Warehouse::on(..)`, a result that is not an `R`, for example the `null`
of a fake without a rule, fails with
`Invalid result from "warehouse.push": …`.

## Checker

`.checker(…)` takes a `Fn(JsonMap) -> impl Future<Output = Result<bool>>`;
`.typed_checker(…)` takes the config as a [typed value](#typed-calls) instead,
and answers with an error when the saved config does not fit the type. The
admin check endpoint saves `lastCheckedAt` and `lastCheckOk` only when a config
row already exists. A checker error produces `{ ok: false, error }` with HTTP
500. Only an error written for the client is shown: return
`PublicError::new("API key rejected")` (`Err(PublicError::new(…).into())`, or
`bail!(PublicError::new(…))`). Any other error, such as a `reqwest` error whose
message holds the URL, or a decoding error that quotes a stored value, is
logged under an id, and the response has `error: "Error: Internal error (error
id <id>)"` and `errorId` instead of its text. A checker that takes longer than 10 seconds produces
`{ ok: false, error }` with HTTP 504. Returning `false` produces `{ ok: false }`
with HTTP 200. A provider
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
plaintext; anywhere else, the application does not start without a key. The key must contain exactly 64 hex
characters (32 bytes). Preserve it across deployments. See
[Security](security.md#encryption-at-rest).
