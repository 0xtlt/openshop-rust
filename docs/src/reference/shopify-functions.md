# Shopify Functions

A `FunctionDefinition` configures OpenShop's admin UI and the GraphQL mutations
used to manage Shopify Function owners. It does not build, deploy, or execute the
WASM extension.

```rust,ignore
use openshop::{
    CombinesWith, DiscountMode, FieldDef, FunctionDefinition, FunctionOwner, ShopifyFunctionType,
};

pub fn volume_discount() -> FunctionDefinition {
    FunctionDefinition::new(ShopifyFunctionType::Discount, "volume-discount")
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
        .field("threshold", FieldDef::number("Minimum quantity"))
        .field("percentage", FieldDef::number("Percentage off"))
}
```

Register it under a key with `.function("volumeDiscount", volume_discount())`
on the configuration builder.

The handle must match the Shopify Function extension handle deployed for the
app. OpenShop requires handles to be non-empty and unique in its config.
`.label(…)` is optional, stays local to OpenShop, and falls back to the
humanized key used at registration (`volumeDiscount` becomes
`Volume Discount`).

## Supported lifecycle

| `ShopifyFunctionType` | Serialized as | List | Create | Update | Delete |
| --- | --- | --- | --- | --- | --- |
| `Discount` | `discount` | `discountNodes` | Automatic or code | Yes | Yes |
| `CartTransform` | `cart-transform` | `cartTransforms` | Yes | No | Yes |
| `DeliveryCustomization` | `delivery-customization` | `deliveryCustomizations` | Yes | Yes | Yes |
| `PaymentCustomization` | `payment-customization` | `paymentCustomizations` | Yes | Yes | Yes |
| `CheckoutValidation` | `checkout-validation` | `validations` | Yes | Yes | Yes |
| `FulfillmentConstraints` | `fulfillment-constraints` | `fulfillmentConstraintRules` | Yes | No | Yes |

For types without update, the API returns a 400 instructing the caller to delete
and recreate. These mappings reflect the GraphQL operations currently used by
OpenShop; Shopify availability and required scopes depend on the Admin API
version and app configuration.

`.modes(…)` is only accepted on discount functions; setting it on another type
fails configuration validation.

Cart Transform is a singleton owner. Shopify does not provide it with a title
or an editable enabled status, so OpenShop displays the definition label and
derives `active` from the owner's existence. Its creation form exposes
`blockOnFailure`; once an owner exists, the Create action is hidden and the API
answers a second creation with `409`.

## Configuration fields

`.field(name, definition)` adds a configuration field. Function config uses
provider-style `text`, `password`, `number`, `select`, and `checkbox`
[field definitions](providers.md#field-definitions). Fields are required unless
`.optional()` is called. Numbers and checkboxes are coerced before validation.

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

`FunctionOwner::new(title)` takes a fixed title and
`FunctionOwner::dynamic_title(|config| …)` computes it from the validated
config; a definition without an owner uses `Untitled`. `.combines_with(…)` is
copied into discount create and update inputs; create inputs default to `{}`.

`FunctionOwner` also has the builder methods `.starts_at(…)`, `.ends_at(…)`,
`.usage_limit(…)`, `.applies_on_each_item(…)`, and `.enabled(…)`. The current
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
