//! What the pages of the functions show, from plain data: the functions of
//! the app, the instances Shopify listed, and the form of an instance with
//! its deletion.

use maud::{Markup, html};
use serde_json::Value;

use super::{
    BLOCK_ON_FAILURE, CONFIG, CREATE, DELETE, DELETE_MODAL, MODE, UPDATE, instance_path, instances_path, new_path,
};
use crate::admin::FormState;
use crate::server::functions::AppFunction;
use crate::server::pages::ui;
use crate::types::{DiscountMode, FieldType, Fields, FunctionType, JsonMap, is_missing};
use openshop_shared::json::js_string;

fn type_label(kind: FunctionType) -> &'static str {
    match kind {
        FunctionType::Discount => "Discount",
        FunctionType::CartTransform => "Cart Transform",
        FunctionType::DeliveryCustomization => "Delivery Customization",
        FunctionType::PaymentCustomization => "Payment Customization",
        FunctionType::CheckoutValidation => "Checkout Validation",
        FunctionType::FulfillmentConstraints => "Fulfillment Constraints",
    }
}

fn mode_label(mode: DiscountMode) -> &'static str {
    match mode {
        DiscountMode::Automatic => "Automatic discount",
        DiscountMode::Code => "Discount code",
    }
}

/// The ways a discount may be given, when there is a choice.
fn mode_choice(function: &AppFunction<'_>) -> Option<Vec<DiscountMode>> {
    function.modes().filter(|modes| modes.len() > 1)
}

/// An instance of a function, as Shopify lists it.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Instance {
    pub(super) id: String,
    /// What the app calls a Cart Transform, which Shopify does not title.
    label: Option<String>,
    /// `active` or `inactive`, of a Cart Transform.
    state: Option<String>,
    title: Option<String>,
    /// `ACTIVE`, `EXPIRED`, `SCHEDULED`, of a discount.
    status: Option<String>,
    /// Of a customization or a validation.
    enabled: Option<bool>,
    block_on_failure: Option<bool>,
    pub(super) config: JsonMap,
}

impl Instance {
    /// An item of [`function_instances`](crate::server::functions::function_instances).
    pub(super) fn of(item: &Value) -> Self {
        let text = |key: &str| item.get(key).and_then(Value::as_str).map(str::to_string);
        Self {
            id: text("id").unwrap_or_default(),
            label: text("label"),
            state: text("state"),
            title: text("title"),
            status: text("status"),
            enabled: item.get("enabled").and_then(Value::as_bool),
            block_on_failure: item.get("blockOnFailure").and_then(Value::as_bool),
            config: item
                .get("config")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
        }
    }

    fn name(&self) -> &str {
        self.label
            .as_deref()
            .or(self.title.as_deref())
            .unwrap_or("(untitled)")
    }

    /// `Active` for a Cart Transform, Shopify's status of a discount, else
    /// whether it is enabled.
    fn state(&self) -> String {
        if let Some(state) = self.state.as_deref().filter(|state| !state.is_empty()) {
            let mut letters = state.chars();
            return letters
                .next()
                .map(|first| first.to_uppercase().chain(letters).collect())
                .unwrap_or_default();
        }
        self.status.clone().unwrap_or_else(|| {
            if self.enabled == Some(true) { "Active" } else { "Inactive" }.to_string()
        })
    }

    fn is_active(&self) -> bool {
        self.state.as_deref() == Some("active") || self.status.as_deref() == Some("ACTIVE") || self.enabled == Some(true)
    }

    /// `threshold: 3, percent: 10`, or `—`. A password of `fields` is not
    /// written out, as its field does not show it.
    fn summary(&self, fields: &Fields) -> String {
        let is_password = |key: &str| fields.get(key).is_some_and(|field| field.field_type == FieldType::Password);
        let summary = self
            .config
            .iter()
            .map(|(key, value)| {
                if is_password(key) {
                    format!("{key}: ••••••••")
                } else {
                    format!("{key}: {}", js_string(value))
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        if summary.is_empty() { "—".to_string() } else { summary }
    }
}

/// Whether the page offers a new instance: always, but for a Cart Transform,
/// of which a shop has one at most, only once Shopify said it has none.
fn can_create(function: &AppFunction<'_>, instances: Option<&[Instance]>) -> bool {
    !function.is_singleton() || instances.is_some_and(<[Instance]>::is_empty)
}

pub(super) fn list_view(functions: &[AppFunction<'_>]) -> Markup {
    html! {
        s-page heading="Functions" {
            @if functions.is_empty() {
                s-box padding="large-500" {
                    s-stack gap="base" {
                        s-heading { "No functions configured" }
                        s-paragraph {
                            "Declare Shopify Functions with #[function] and list them in functions: of app! to manage discount, cart transform, and other function instances from here."
                        }
                    }
                }
            } @else {
                s-stack gap="large-100" {
                    @for function in functions { (card(function)) }
                }
            }
        }
    }
}

fn card(function: &AppFunction<'_>) -> Markup {
    let modes = mode_choice(function).map(|modes| {
        modes
            .iter()
            .map(|mode| mode.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    });
    html! {
        s-box padding="large-100" background="base" border="base" borderRadius="large" {
            s-stack direction="inline" justifyContent="space-between" alignItems="center" {
                s-stack gap="small" {
                    s-heading { (function.label) }
                    s-stack direction="inline" gap="base" {
                        s-badge { (type_label(function.definition.function_type)) }
                        @if let Some(modes) = modes { s-badge tone="info" { (modes) } }
                    }
                    s-text color="subdued" { (function.definition.config.len()) " config fields" }
                }
                s-button href=(instances_path(function.handle())) { "Manage" }
            }
        }
    }
}

/// The instances of a function; `None` when Shopify did not list them.
pub(super) fn instances_view(function: &AppFunction<'_>, instances: Option<&[Instance]>) -> Markup {
    let handle = function.handle();
    let kind = type_label(function.definition.function_type);
    let creatable = can_create(function, instances);
    html! {
        s-page heading=(function.label) {
            s-link slot="breadcrumb-actions" href="/functions" { "Functions" }
            @if creatable {
                s-button slot="primary-action" variant="primary" href=(new_path(handle)) { "Create instance" }
            }
            @match instances {
                None => {
                    s-banner tone="critical" { "The instances could not be loaded from Shopify." }
                    s-box padding="large-500" {
                        s-text color="subdued" { "The instances could not be loaded." }
                    }
                }
                Some([]) => {
                    s-box padding="large-500" background="subdued" borderRadius="large" {
                        s-stack gap="base" alignItems="center" {
                            s-heading { "No instances yet" }
                            s-paragraph {
                                "Create your first " (kind) " instance to start using this function. Each instance has its own configuration stored as metafields on Shopify."
                            }
                            @if creatable {
                                s-button variant="primary" href=(new_path(handle)) { "Create first instance" }
                            }
                        }
                    }
                }
                Some(instances) => {
                    s-section {
                        s-text color="subdued" {
                            (instances.len()) " instance" (if instances.len() == 1 { "" } else { "s" }) " — " (kind)
                        }
                        s-table {
                            s-table-header-row {
                                s-table-header listSlot="primary" {
                                    (if function.is_singleton() { "Label" } else { "Title" })
                                }
                                s-table-header listSlot="inline" { "Status" }
                                s-table-header listSlot="secondary" { "Configuration" }
                                s-table-header {}
                            }
                            s-table-body {
                                @for instance in instances { (row(function, instance)) }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn row(function: &AppFunction<'_>, instance: &Instance) -> Markup {
    html! {
        s-table-row {
            s-table-cell { (instance.name()) }
            s-table-cell {
                s-badge tone=(if instance.is_active() { "success" } else { "warning" }) { (instance.state()) }
            }
            s-table-cell { s-text color="subdued" { (instance.summary(&function.definition.config)) } }
            s-table-cell {
                s-button variant="secondary" href=(instance_path(function.handle(), &instance.id)) { "See" }
            }
        }
    }
}

/// The page of an instance Shopify did not list.
pub(super) fn unloaded_view(function: &AppFunction<'_>) -> Markup {
    html! {
        s-page heading="Edit instance" {
            s-link slot="breadcrumb-actions" href=(instances_path(function.handle())) { (function.label) }
            s-banner tone="critical" { "The instance could not be loaded." }
        }
    }
}

fn mode_select(modes: &[DiscountMode], value: &str, error: Option<&str>) -> Markup {
    html! {
        s-select name=(MODE) label="Discount mode" value=(value) error=[error] {
            @for mode in modes {
                s-option value=(mode.as_str()) selected[mode.as_str() == value] { (mode_label(*mode)) }
            }
        }
    }
}

/// The form of a new instance (`instance` is `None`) or of one Shopify
/// listed, with its deletion: shown again with what was sent and the errors
/// of its fields when it was refused.
pub(super) fn form_view(function: &AppFunction<'_>, instance: Option<&Instance>, form: &FormState) -> Markup {
    let definition = function.definition;
    let kind = definition.function_type;
    let is_edit = instance.is_some();
    let read_only = is_edit && !function.supports_update();
    let action = if is_edit { UPDATE } else { CREATE };
    let saving = form.of(action);
    let deleting = form.of(DELETE);
    let heading = if is_edit {
        "Edit instance".to_string()
    } else {
        format!("New {}", type_label(kind))
    };
    let modes = mode_choice(function);
    let first_mode = definition.first_mode().as_str();
    let saved = |name: &str| instance.and_then(|instance| instance.config.get(name));
    let blocked = instance.and_then(|instance| instance.block_on_failure) == Some(true);
    html! {
        form method="post" {
            input type="hidden" name="_action" value=(action);
            s-page heading=(heading) {
                s-link slot="breadcrumb-actions" href=(instances_path(function.handle())) { (function.label) }
                s-button slot="primary-action" variant="primary" type="submit" disabled[read_only] {
                    (if is_edit { "Save" } else { "Create" })
                }
                s-stack gap="large-100" {
                    @if let Some(message) = &saving.message {
                        s-banner tone="critical" { (message) }
                    }
                    @if read_only {
                        s-banner tone="warning" {
                            "This function type does not support updates. Delete and recreate to change configuration."
                        }
                    }
                    @if kind == FunctionType::CartTransform {
                        s-box padding="large-100" background="base" border="base" borderRadius="large" {
                            s-checkbox
                                name=(BLOCK_ON_FAILURE)
                                value="true"
                                label="Block checkout when the Cart Transform fails"
                                checked[saving.value_or(BLOCK_ON_FAILURE, if blocked { "true" } else { "false" }) == "true"]
                                disabled[is_edit]
                                error=[saving.error(BLOCK_ON_FAILURE)] {}
                        }
                    }
                    @if let Some(modes) = &modes {
                        s-box padding="large-100" background="base" border="base" borderRadius="large" {
                            (mode_select(modes, saving.value_or(MODE, first_mode), saving.error(MODE)))
                        }
                    }
                    @if !definition.config.is_empty() {
                        s-box padding="large-100" background="base" border="base" borderRadius="large" {
                            s-stack gap="base" {
                                s-heading { "Configuration" }
                                @for (name, field) in &definition.config {
                                    @let input = format!("{CONFIG}{name}");
                                    @let value = ui::config_value(saved(name));
                                    @let has_password = field.field_type == FieldType::Password && !is_missing(saved(name));
                                    (ui::config_field(&input, field, saving.value_or(&input, &value), has_password, saving.error(&input)))
                                }
                            }
                        }
                    }
                    @if is_edit {
                        s-box padding="large-100" background="base" border="base" borderRadius="large" {
                            s-stack gap="base" {
                                s-heading { "Danger zone" }
                                s-paragraph { "This will permanently remove this function instance from Shopify." }
                                s-button variant="secondary" tone="critical" commandFor=(DELETE_MODAL) command="--show" {
                                    "Delete instance"
                                }
                            }
                        }
                    }
                }
            }
        }
        @if is_edit {
            // The modal is the form's: its buttons, slotted, are the modal's
            // own children, and Polaris submits the form around them.
            form method="post" {
                input type="hidden" name="_action" value=(DELETE);
                s-modal
                    id=(DELETE_MODAL)
                    heading="Delete instance"
                    accessibilityLabel="Confirm function instance deletion"
                    data-openshop-open[deleting.is_refused()]
                {
                    s-stack gap="base" {
                        s-text { "This will permanently remove this function instance from Shopify." }
                        @if let Some(modes) = &modes {
                            (mode_select(modes, deleting.value_or(MODE, first_mode), deleting.error(MODE)))
                        }
                    }
                    s-button slot="primary-action" type="submit" variant="primary" tone="critical" { "Delete" }
                    s-button slot="secondary-actions" variant="secondary" commandFor=(DELETE_MODAL) command="--hide" {
                        "Cancel"
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Field, Function, FunctionOwner};
    use indexmap::IndexMap;
    use serde_json::json;

    fn discount() -> Function {
        Function::new(FunctionType::Discount, "volume-discount")
            .modes([DiscountMode::Automatic, DiscountMode::Code])
            .owner(FunctionOwner::new("Volume"))
            .field("percent", Field::number("Percent"))
    }

    fn cart_transform() -> Function {
        Function::new(FunctionType::CartTransform, "cart-transform")
            .label("Bundle <cart>")
            .field("message", Field::text("Message"))
    }

    fn instance(item: Value) -> Instance {
        Instance::of(&item)
    }

    fn refused(action: &str, values: &[(&str, &str)], errors: &[(&str, &str)], message: Option<&str>) -> FormState {
        FormState {
            action: Some(action.to_string()),
            values: values
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            errors: errors
                .iter()
                .map(|(name, error)| (name.to_string(), error.to_string()))
                .collect::<IndexMap<_, _>>(),
            message: message.map(str::to_string),
        }
    }


    #[test]
    fn lists_the_functions_with_their_type_and_modes() {
        let (discount, cart) = (discount(), cart_transform());
        let functions = [
            AppFunction::of("discount", &discount),
            AppFunction::of("cartTransform", &cart),
        ];
        let html = list_view(&functions).into_string();
        assert!(html.contains("<s-heading>Discount</s-heading>"), "{html}");
        assert!(html.contains("<s-badge>Discount</s-badge>"), "{html}");
        assert!(html.contains(r#"<s-badge tone="info">automatic, code</s-badge>"#), "{html}");
        assert!(html.contains("<s-heading>Bundle &lt;cart&gt;</s-heading>"), "{html}");
        assert!(html.contains("<s-badge>Cart Transform</s-badge>"), "{html}");
        assert!(html.contains("1 config fields"), "{html}");
        assert!(html.contains(r#"<s-button href="/functions/volume-discount">Manage</s-button>"#), "{html}");
        assert!(list_view(&[]).into_string().contains("No functions configured"));
    }

    #[test]
    fn shows_the_instances_shopify_listed() {
        let definition = discount();
        let function = AppFunction::of("discount", &definition);
        let instances = [
            instance(json!({ "id": "gid://shopify/DiscountNode/1", "title": "Volume <10>", "status": "ACTIVE",
                             "config": { "percent": 10, "stack": false } })),
            instance(json!({ "id": "gid://shopify/DiscountNode/2", "title": "Legacy", "status": "EXPIRED", "config": {} })),
        ];
        let html = instances_view(&function, Some(&instances)).into_string();
        assert!(html.contains("2 instances — Discount"), "{html}");
        assert!(html.contains(r#"<s-table-header listSlot="primary">Title</s-table-header>"#), "{html}");
        assert!(html.contains("<s-table-cell>Volume &lt;10&gt;</s-table-cell>"), "{html}");
        assert!(html.contains(r#"<s-badge tone="success">ACTIVE</s-badge>"#), "{html}");
        assert!(html.contains(r#"<s-badge tone="warning">EXPIRED</s-badge>"#), "{html}");
        assert!(html.contains("percent: 10, stack: false"), "{html}");
        assert!(html.contains(r#"<s-text color="subdued">—</s-text>"#), "{html}");
        assert!(
            html.contains(r#"href="/functions/volume-discount/gid%3A%2F%2Fshopify%2FDiscountNode%2F1">See</s-button>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<s-button slot="primary-action" variant="primary" href="/functions/volume-discount/new">Create instance</s-button>"#),
            "{html}"
        );
        assert!(html.contains(r#"<s-link slot="breadcrumb-actions" href="/functions">Functions</s-link>"#));

        // None yet: the first one.
        let empty = instances_view(&function, Some(&[])).into_string();
        assert!(empty.contains("No instances yet"), "{empty}");
        assert!(empty.contains("Create your first Discount instance"), "{empty}");
        assert!(empty.contains("Create first instance"), "{empty}");

        // Shopify did not answer: said, and a discount may still be created.
        let failed = instances_view(&function, None).into_string();
        assert!(failed.contains(r#"<s-banner tone="critical">"#), "{failed}");
        assert!(failed.contains("The instances could not be loaded."), "{failed}");
        assert!(failed.contains("Create instance"), "{failed}");
    }

    #[test]
    fn offers_a_cart_transform_only_to_a_shop_known_to_have_none() {
        let definition = cart_transform();
        let function = AppFunction::of("cartTransform", &definition);
        let one = [instance(json!({ "id": "gid://shopify/CartTransform/1", "label": "Bundle <cart>",
                                    "state": "active", "blockOnFailure": true, "config": {} }))];
        let html = instances_view(&function, Some(&one)).into_string();
        assert!(html.contains(r#"<s-table-header listSlot="primary">Label</s-table-header>"#), "{html}");
        assert!(html.contains(r#"<s-badge tone="success">Active</s-badge>"#), "{html}");
        assert!(html.contains("1 instance — Cart Transform"), "{html}");
        assert!(!html.contains("Create instance"), "{html}");
        assert!(instances_view(&function, Some(&[])).into_string().contains("Create first instance"));
        assert!(!instances_view(&function, None).into_string().contains("Create"));
    }

    #[test]
    fn names_and_states_an_instance_as_shopify_describes_it() {
        let bare = instance(json!({ "id": "1", "enabled": true }));
        assert_eq!((bare.name(), bare.state().as_str(), bare.is_active()), ("(untitled)", "Active", true));
        let off = instance(json!({ "id": "1", "title": "Rules", "enabled": false }));
        assert_eq!((off.name(), off.state().as_str(), off.is_active()), ("Rules", "Inactive", false));
        assert_eq!(off.summary(&Fields::new()), "—");
    }

    #[test]
    fn renders_the_form_of_a_new_instance() {
        let definition = discount();
        let function = AppFunction::of("discount", &definition);
        let html = form_view(&function, None, &FormState::default()).into_string();
        assert!(html.contains(r#"<form method="post"><input type="hidden" name="_action" value="create"><s-page heading="New Discount">"#), "{html}");
        assert!(
            html.contains(r#"<s-link slot="breadcrumb-actions" href="/functions/volume-discount">Discount</s-link>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<s-button slot="primary-action" variant="primary" type="submit">Create</s-button>"#),
            "{html}"
        );
        assert!(html.contains(r#"<s-select name="mode" label="Discount mode" value="automatic">"#), "{html}");
        assert!(html.contains(r#"<s-option value="code">Discount code</s-option>"#), "{html}");
        assert!(
            html.contains(r#"<s-number-field name="config.percent" label="Percent" value=""></s-number-field>"#),
            "{html}"
        );
        // Nothing to delete yet.
        assert!(!html.contains("s-modal"), "{html}");
        assert!(!html.contains("Danger zone"), "{html}");
    }

    #[test]
    fn renders_a_refused_form_with_what_was_sent_and_its_errors() {
        let definition = discount();
        let function = AppFunction::of("discount", &definition);
        let form = refused(
            CREATE,
            &[("_action", CREATE), ("mode", "code"), ("config.percent", "-1")],
            &[("config.percent", "Must be at least 0 (was -1)")],
            Some("Field \"percent\": must be at least 0 (was -1)"),
        );
        let html = form_view(&function, None, &form).into_string();
        assert!(
            html.contains(r#"<s-number-field name="config.percent" label="Percent" value="-1" error="Must be at least 0 (was -1)">"#),
            "{html}"
        );
        assert!(html.contains(r#"<s-option value="code" selected>"#), "{html}");
        assert!(
            html.contains(r#"<s-banner tone="critical">Field &quot;percent&quot;: must be at least 0 (was -1)</s-banner>"#),
            "{html}"
        );
    }

    #[test]
    fn renders_the_form_of_an_instance_with_its_deletion() {
        let definition = discount();
        let function = AppFunction::of("discount", &definition);
        let saved = instance(json!({ "id": "gid://shopify/DiscountNode/1", "title": "Volume", "config": { "percent": 10 } }));
        let html = form_view(&function, Some(&saved), &FormState::default()).into_string();
        assert!(html.contains(r#"<input type="hidden" name="_action" value="update"><s-page heading="Edit instance">"#), "{html}");
        assert!(html.contains(r#"name="config.percent" label="Percent" value="10""#), "{html}");
        assert!(html.contains(">Save</s-button>"), "{html}");
        assert!(
            html.contains(r#"<s-button variant="secondary" tone="critical" commandFor="delete-function-instance-modal" command="--show">Delete instance</s-button>"#),
            "{html}"
        );
        // The deletion is its own form, around its modal, with the mode of the discount.
        assert!(
            html.contains(r#"<form method="post"><input type="hidden" name="_action" value="delete"><s-modal id="delete-function-instance-modal" heading="Delete instance" accessibilityLabel="Confirm function instance deletion">"#),
            "{html}"
        );
        assert_eq!(html.matches(r#"<s-select name="mode""#).count(), 2, "{html}");
        assert!(
            html.contains(r#"<s-button slot="primary-action" type="submit" variant="primary" tone="critical">Delete</s-button>"#),
            "{html}"
        );
        // A refused deletion opens its modal again.
        let form = refused(DELETE, &[("_action", DELETE), ("mode", "")], &[("mode", "Choose")], None);
        let html = form_view(&function, Some(&saved), &form).into_string();
        assert!(html.contains(r#"accessibilityLabel="Confirm function instance deletion" data-openshop-open>"#), "{html}");
        assert!(html.contains(r#"value="" error="Choose""#), "{html}");
        // The other form is shown as it was.
        assert!(html.contains(r#"value="10""#), "{html}");
    }

    #[test]
    fn a_type_without_update_is_read_only() {
        let definition = cart_transform();
        let function = AppFunction::of("cartTransform", &definition);
        let saved = instance(json!({ "id": "gid://shopify/CartTransform/1", "blockOnFailure": true, "config": { "message": "hi" } }));
        let html = form_view(&function, Some(&saved), &FormState::default()).into_string();
        assert!(html.contains(r#"type="submit" disabled>Save</s-button>"#), "{html}");
        assert!(html.contains("This function type does not support updates."), "{html}");
        assert!(
            html.contains(r#"<s-checkbox name="blockOnFailure" value="true" label="Block checkout when the Cart Transform fails" checked disabled>"#),
            "{html}"
        );
        // No discount, no mode.
        assert!(!html.contains(r#"name="mode""#), "{html}");
        let new = form_view(&function, None, &FormState::default()).into_string();
        assert!(new.contains(r#"label="Block checkout when the Cart Transform fails">"#), "{new}");
        assert!(new.contains("New Cart Transform"), "{new}");
    }

    #[test]
    fn a_password_saved_is_never_shown() {
        let definition = Function::new(FunctionType::DeliveryCustomization, "rules").field("token", Field::password("Token"));
        let function = AppFunction::of("rules", &definition);
        let saved = instance(json!({ "id": "1", "config": { "token": "s3cret" } }));
        let html = form_view(&function, Some(&saved), &FormState::default()).into_string();
        assert!(!html.contains("s3cret"), "{html}");
        assert!(html.contains(r#"placeholder="Saved; leave blank to keep""#), "{html}");
        let listed = instances_view(&function, Some(&[saved])).into_string();
        assert!(!listed.contains("s3cret"), "{listed}");
        assert!(listed.contains("token: ••••••••"), "{listed}");
    }

}
