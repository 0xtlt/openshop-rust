# Shopify Functions

A struct with `#[derive(FunctionConfig)]` describes a Shopify Function to
OpenShop: `#[function(…)]` names the function, and the fields of the struct are
the configuration form of an instance. It configures OpenShop's admin UI and
the GraphQL mutations used to manage Shopify Function owners. It does not
build, deploy, or execute the WASM extension.

```rust,ignore
use openshop::prelude::*;

#[derive(FunctionConfig)]
#[function(
    "volumeDiscount",
    discount = "volume-discount",
    label = "Volume discount",
    modes(automatic, code),
    title = Self::title,
    starts_at,
    ends_at,
    combines_with(product_discounts)
)]
pub struct VolumeDiscount {
    #[field(label = "Minimum quantity", validate = validators::integer_min(1))]
    pub threshold: u32,
    #[field(label = "Percentage off", range(min = 0, max = 100))]
    pub percentage: f64,
}

impl VolumeDiscount {
    fn title(&self) -> String {
        format!("Volume discount {}", self.threshold)
    }
}
```

List the struct in the `functions` of `app!`: `functions: [VolumeDiscount]`.

| `#[function(…)]` option | |
| --- | --- |
| `"volumeDiscount"` | The key the function is registered under, also the constant `VolumeDiscount::NAME`. Left out, it is the name of the struct with a lowercase initial. |
| `discount = "volume-discount"` | The type of the function and the handle of its extension. Required. The types are listed under [Supported lifecycle](#supported-lifecycle). |
| `label = "Volume discount"` | The merchant-facing label. |
| `modes(automatic, code)` | The modes of a discount. |
| `title`, `starts_at`, `ends_at`, `usage_limit`, `applies_on_each_item`, `enabled`, `combines_with(…)` | The [owner options](#owner-options). |

The handle must match the Shopify Function extension handle deployed for the
app. OpenShop requires handles to be non-empty and unique in its config.
`label` is optional, stays local to OpenShop, and falls back to the
humanized key (`volumeDiscount` becomes `Volume Discount`).

## Supported lifecycle

| Type option | `FunctionType` | Serialized as | List | Create | Update | Delete |
| --- | --- | --- | --- | --- | --- | --- |
| `discount` | `Discount` | `discount` | `discountNodes` | Automatic or code | Yes | Yes |
| `cart_transform` | `CartTransform` | `cart-transform` | `cartTransforms` | Yes | No | Yes |
| `delivery_customization` | `DeliveryCustomization` | `delivery-customization` | `deliveryCustomizations` | Yes | Yes | Yes |
| `payment_customization` | `PaymentCustomization` | `payment-customization` | `paymentCustomizations` | Yes | Yes | Yes |
| `checkout_validation` | `CheckoutValidation` | `checkout-validation` | `validations` | Yes | Yes | Yes |
| `fulfillment_constraints` | `FulfillmentConstraints` | `fulfillment-constraints` | `fulfillmentConstraintRules` | Yes | No | Yes |

For types without update, the API returns a 400 instructing the caller to delete
and recreate. These mappings reflect the GraphQL operations currently used by
OpenShop; Shopify availability and required scopes depend on the Admin API
version and app configuration.

`modes(…)` is only accepted on discount functions; setting it on another type
fails configuration validation.

Cart Transform is a singleton owner. Shopify does not provide it with a title
or an editable enabled status, so OpenShop displays the label of the function and
derives `active` from the owner's existence. Its creation form exposes
`blockOnFailure`; once an owner exists, the Create action is hidden and the API
answers a second creation with `409`.

## Configuration fields

Each field of the struct is a configuration field, saved under its name in
camel case (a `min_quantity` field is `minQuantity`). Function config uses the field
types and the `#[field(…)]` options of a
[provider struct](providers.md#the-configuration): `String` for text, a
password, or a select, a number type for a number, and `bool` for a checkbox.
Fields are required unless their type is an `Option`. Numbers and checkboxes
are coerced before validation.

As on a provider struct, `label` is optional: a `min_quantity` field without
one is labeled `Min quantity`. The constraints `url`, `non_empty`,
`range(min = …, max = …)` and `length(min = …, max = …)` are options of
`#[field(…)]`, and `validate = <validator>` remains for anything else, such as
`validators::integer_min(1)` on an integer field.

The derive implements the `FunctionConfig` trait: `VolumeDiscount::NAME` is
the key of the function, `VolumeDiscount::definition()` is the `Function` the
struct describes, and `VolumeDiscount::from_config(map)` reads the config of
an instance into the struct.

Only declared fields are retained. Missing required fields and validator
failures return HTTP 400 with `Field "<name>" is required` or
`Field "<name>": <summary>`. OpenShop stores the validated config as JSON in a
metafield:

```text
namespace: $app:openshop
key: <function handle>
type: json
```

The WASM implementation must read the same metafield if it needs this config.

## Owner options

`title = "…"` gives the Shopify owner of an instance a fixed title, and
`title = <function>` computes it from the validated config, read into the
struct: the function is a `fn(&Self) -> String`, such as `Self::title`. When
the config cannot be read into the struct, the title is the label of the
function, or its handle. A function without `title` has no owner options and
uses `Untitled`.
`combines_with(product_discounts, order_discounts, shipping_discounts)` is
copied into discount create and update inputs; create inputs default to `{}`.
Each name states `true`, or `false` when written `order_discounts = false`; a
name left out is not sent.

The other owner options are the flags `starts_at`, `ends_at`, `usage_limit`,
`applies_on_each_item`, and `enabled`, written alone for `true` or with
`= false`. Like `combines_with(…)`, they need `title`. The current
server mutations do not branch on those flags. Instead, they consume the
corresponding request-body values described below. Treat the flags as
definition metadata, not server-side validation.

The current create mutations always enable delivery, payment, and validation
owners. Cart transforms and validations accept `blockOnFailure` (default
`false`); fulfillment constraints accept `deliveryMethodTypes` (default
`["SHIPPING"]`).

Discount mode comes from the request body's `mode`, then defaults to the first
`modes` value, then `automatic`. Automatic and code creates default `startsAt`
to the current time and `endsAt` to `null`. Code discounts accept `code` and
default `usageLimit` to `null`.

Updates always send the title and the config metafield. They forward
`startsAt` and `endsAt` (discounts), `usageLimit` (code discounts), `enabled`
(delivery, payment, and validation owners), and `blockOnFailure` (validations)
only when the request body contains them.

## Instance API behavior

The embedded UI calls:

```text
GET    /api/functions
GET    /api/functions/:handle/instances
POST   /api/functions/:handle/instances
PUT    /api/functions/:handle/instances/:id
DELETE /api/functions/:handle/instances/:id
```

Lists query Shopify live and return at most 50 nodes. Create returns HTTP 201;
update and delete return `{ "ok": true }`. An unknown handle returns `404` with
`Function not found`.

Shopify mutation `userErrors` become HTTP 400 with both a joined `error` string
and the `userErrors` array. Transport or top-level GraphQL errors raised by the
Shopify client produce a `500`. Deleting a discount with multiple configured
modes requires `?mode=automatic` or `?mode=code`.

See [Admin API](admin-api.md#shopify-functions) for the payloads.

## Extension lifecycle

Build and deploy the Shopify Function extension with Shopify's tooling before
creating an owner in OpenShop. Deploying OpenShop config alone does not publish
WASM or register a function handle. Likewise, deleting an owner instance does
not remove the deployed extension.

## Without the attribute

The derive is built on `Function`, which can be used directly when
the fields of a function are only known at run time:

```rust,ignore
use openshop::builder::{CombinesWith, DiscountMode, Field, Function, FunctionOwner, FunctionType};

pub fn volume_discount() -> Function {
    Function::new(FunctionType::Discount, "volume-discount")
        .label("Volume discount")
        .modes([DiscountMode::Automatic, DiscountMode::Code])
        .owner(
            FunctionOwner::dynamic_title(|config| format!("Volume discount {}", config["threshold"]))
                .combines_with(CombinesWith {
                    product_discounts: Some(true),
                    ..Default::default()
                })
                .starts_at(true)
                .ends_at(true),
        )
        .field("threshold", Field::number("Minimum quantity"))
        .field("percentage", Field::number("Percentage off"))
}
```

Register it under a key with `.function("volumeDiscount", volume_discount())`
on the builder: `APP.builder()?` is the builder of the declaration, to add to.

`.field(name, definition)` adds a configuration field from a provider-style
[field definition](providers.md#field-definitions); fields are required unless
`.optional()` is called. `FunctionOwner::new(title)` takes a fixed title and
`FunctionOwner::dynamic_title(|config| …)` computes it from the validated
config, a JSON object. `FunctionOwner` has one builder method per owner
option: `.combines_with(…)`, `.starts_at(…)`, `.ends_at(…)`,
`.usage_limit(…)`, `.applies_on_each_item(…)`, and `.enabled(…)`.
