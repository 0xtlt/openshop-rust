//! `/functions`, `/functions/:handle` and `/functions/:handle/:action`: the
//! Shopify Functions of the app, the instances of one in the shop, which
//! Shopify's Admin API lists (discounts, cart transforms, customizations, …),
//! and the form of an instance: `new` creates one, its id edits it, deletes it
//! after a confirmation, or says that its type cannot be changed.
//!
//! This module reads the URL, the data and the forms; `views` renders them.

mod views;

use anyhow::Result;
use maud::Markup;
use serde_json::Value;

use super::{BuiltinPage, field, ui};
use crate::admin::{AdminPublicError, Outcome};
use crate::admin_paths::rpc::{decode_uri_component, encode_uri_component as encode};
use crate::server::functions::{
    AppFunction, InstanceRefusal, app_functions, create_function_instance, delete_function_instance, find_function,
    function_instances, update_function_instance,
};
use crate::server::http::report_failure;
use crate::types::{FieldType, FunctionType, JsonMap, is_missing};
use views::{Instance, form_view, instances_view, list_view, unloaded_view};

/// The action of the form of `/functions/:handle/new`.
const CREATE: &str = "create";
/// The actions of the page of an instance: its form, and its deletion.
const UPDATE: &str = "update";
const DELETE: &str = "delete";
/// The prefix of the fields of the configuration: `config.percent`.
const CONFIG: &str = "config.";
const MODE: &str = "mode";
const BLOCK_ON_FAILURE: &str = "blockOnFailure";
const DELETE_MODAL: &str = "delete-function-instance-modal";

/// What the path of the page shows.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    /// `/functions`: the functions of the app.
    List,
    /// `/functions/:handle`: the instances of a function.
    Instances(String),
    /// `/functions/:handle/new`: the form of a new instance.
    New(String),
    /// `/functions/:handle/:id`: the form of an instance, its id a segment.
    Edit(String, String),
}

fn route(pathname: &str) -> Option<Route> {
    let rest = pathname.strip_prefix("/functions")?;
    if !rest.is_empty() && !rest.starts_with('/') {
        return None;
    }
    let segments = rest
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(decode_uri_component)
        .collect::<Option<Vec<_>>>()?;
    match segments.as_slice() {
        [] => Some(Route::List),
        [handle] => Some(Route::Instances(handle.clone())),
        [handle, action] if action == "new" => Some(Route::New(handle.clone())),
        [handle, id] => Some(Route::Edit(handle.clone(), id.clone())),
        _ => None,
    }
}

fn instances_path(handle: &str) -> String {
    format!("/functions/{}", encode(handle))
}

fn new_path(handle: &str) -> String {
    format!("/functions/{}/new", encode(handle))
}

fn instance_path(handle: &str, id: &str) -> String {
    format!("/functions/{}/{}", encode(handle), encode(id))
}

fn not_found(message: impl Into<String>) -> anyhow::Error {
    AdminPublicError::new("NOT_FOUND", message).status(404).into()
}

/// The function of the app the URL names, or a 404 that says it has none.
fn found<'a>(page: &BuiltinPage<'a>, handle: &str) -> Result<AppFunction<'a>> {
    find_function(page.app.config(), handle)
        .ok_or_else(|| not_found(format!("The app declares no function \"{handle}\".")))
}

pub(super) async fn render(page: &BuiltinPage<'_>) -> Result<Markup> {
    match route(page.pathname).ok_or_else(|| not_found("Not found"))? {
        Route::List => Ok(list_view(&app_functions(page.app.config()))),
        Route::Instances(handle) => {
            let function = found(page, &handle)?;
            let instances = listed(page, &function).await;
            Ok(instances_view(&function, instances.as_deref()))
        }
        Route::New(handle) => {
            let function = found(page, &handle)?;
            Ok(form_view(&function, None, page.form))
        }
        Route::Edit(handle, id) => {
            let function = found(page, &handle)?;
            let Some(instances) = listed(page, &function).await else {
                return Ok(unloaded_view(&function));
            };
            let instance = instances
                .into_iter()
                .find(|instance| instance.id == id)
                .ok_or_else(|| not_found("This instance no longer exists."))?;
            Ok(form_view(&function, Some(&instance), page.form))
        }
    }
}

/// The instances of the function in the shop, or `None` when Shopify did not
/// list them: the failure is reported, the page says so.
async fn listed(page: &BuiltinPage<'_>, function: &AppFunction<'_>) -> Option<Vec<Instance>> {
    match function_instances(page.app, page.session, function).await {
        Ok(items) => Some(items.iter().map(Instance::of).collect()),
        Err(error) => {
            report_failure(
                "[openshop] The instances of a Shopify Function could not be listed",
                &error,
            );
            None
        }
    }
}

/// The form of an instance: created, updated or deleted, where the Leptos
/// admin went after it, with a toast; refused, under its fields when it can be.
pub(super) async fn submit(page: &BuiltinPage<'_>, action: &str, fields: &[(String, String)]) -> Result<Outcome> {
    match (route(page.pathname), action) {
        (Some(Route::New(handle)), CREATE) => create(page, &handle, fields).await,
        (Some(Route::Edit(handle, id)), UPDATE) => update(page, &handle, &id, fields).await,
        (Some(Route::Edit(handle, id)), DELETE) => delete(page, &handle, &id, fields).await,
        _ => Err(super::no_action()),
    }
}

/// What the form of an instance sends, as the API reads it: its
/// configuration, the mode of a discount, and whether a Cart Transform blocks
/// the checkout when it fails.
fn body_of(function: &AppFunction<'_>, fields: &[(String, String)]) -> JsonMap {
    let mut body = JsonMap::new();
    body.insert(
        "config".into(),
        ui::config_from_form(&function.definition.config, fields, CONFIG),
    );
    if let Some(mode) = field(fields, MODE) {
        body.insert("mode".into(), Value::from(mode));
    }
    if function.definition.function_type == FunctionType::CartTransform {
        body.insert(
            "blockOnFailure".into(),
            Value::Bool(field(fields, BLOCK_ON_FAILURE) == Some("true")),
        );
    }
    body
}

async fn create(page: &BuiltinPage<'_>, handle: &str, fields: &[(String, String)]) -> Result<Outcome> {
    let function = found(page, handle)?;
    let body = body_of(&function, fields);
    match create_function_instance(page.app, page.session, &function, &body).await? {
        Ok(_) => Ok(Outcome::redirect(instances_path(handle)).with_toast("Instance created")),
        Err(refusal) => Err(refused(&function, refusal)),
    }
}

async fn update(page: &BuiltinPage<'_>, handle: &str, id: &str, fields: &[(String, String)]) -> Result<Outcome> {
    let function = found(page, handle)?;
    let mut body = body_of(&function, fields);
    if function.supports_update() {
        keep_saved_passwords(page, &function, id, &mut body).await?;
    }
    match update_function_instance(page.app, page.session, &function, id, &body).await? {
        Ok(()) => Ok(Outcome::toast("Instance updated")),
        Err(refusal) => Err(refused(&function, refusal)),
    }
}

/// A password left blank keeps the one the instance has: the page never
/// shows it, and its field says "Saved; leave blank to keep".
async fn keep_saved_passwords(
    page: &BuiltinPage<'_>,
    function: &AppFunction<'_>,
    id: &str,
    body: &mut JsonMap,
) -> Result<()> {
    let Some(config) = body.get_mut("config").and_then(Value::as_object_mut) else {
        return Ok(());
    };
    let blank: Vec<&String> = function
        .definition
        .config
        .iter()
        .filter(|(name, field)| field.field_type == FieldType::Password && is_missing(config.get(*name)))
        .map(|(name, _)| name)
        .collect();
    if blank.is_empty() {
        return Ok(());
    }
    let instances = function_instances(page.app, page.session, function).await?;
    let Some(instance) = instances.iter().map(Instance::of).find(|instance| instance.id == id) else {
        return Ok(());
    };
    for name in blank {
        if let Some(saved) = instance.config.get(name).filter(|saved| !is_missing(Some(saved))) {
            config.insert(name.clone(), saved.clone());
        }
    }
    Ok(())
}

async fn delete(page: &BuiltinPage<'_>, handle: &str, id: &str, fields: &[(String, String)]) -> Result<Outcome> {
    let function = found(page, handle)?;
    match delete_function_instance(page.app, page.session, &function, id, field(fields, MODE)).await? {
        Ok(()) => Ok(Outcome::redirect(instances_path(handle)).with_toast("Instance deleted")),
        Err(refusal) => Err(refused(&function, refusal)),
    }
}

/// A refusal as the form shows it: a configuration under its fields, a
/// missing mode under its select, Shopify's `userErrors` under the field they
/// name; anything else in an error toast (a banner without the script).
fn refused(function: &AppFunction<'_>, refusal: InstanceRefusal) -> anyhow::Error {
    let message = refusal.message();
    match refusal {
        InstanceRefusal::InvalidConfig(error) => ui::config_refusal(error, CONFIG),
        InstanceRefusal::AlreadyRegistered(_) => AdminPublicError::new("CONFLICT", message).status(409).into(),
        InstanceRefusal::NoUpdate(_) => AdminPublicError::new("NO_UPDATE", message).status(400).into(),
        InstanceRefusal::ModeRequired => AdminPublicError::new("INVALID_INPUT", message)
            .status(422)
            .field_errors([(MODE, "Choose the kind of discount this instance is")])
            .into(),
        InstanceRefusal::UserErrors(errors) => user_errors(function, &errors),
    }
}

/// The field of the form a `userError` of Shopify names (the last part of
/// its `field`), when the form has it: of what the form sends, Shopify reads
/// `blockOnFailure` itself; the configuration is one metafield to it.
fn field_of(function: &AppFunction<'_>, error: &Value) -> Option<&'static str> {
    let named = match error.get("field") {
        Some(Value::Array(path)) => path.last().and_then(Value::as_str),
        Some(Value::String(name)) => Some(name.as_str()),
        _ => None,
    }?;
    (named == BLOCK_ON_FAILURE && function.definition.function_type == FunctionType::CartTransform)
        .then_some(BLOCK_ON_FAILURE)
}

/// Shopify's `userErrors`: each one under the field of the form it names, the
/// others said above the form; when none names a field, an error toast.
fn user_errors(function: &AppFunction<'_>, errors: &[Value]) -> anyhow::Error {
    let message = |error: &Value| {
        error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let mut fields = Vec::new();
    let mut others = Vec::new();
    for error in errors {
        match field_of(function, error) {
            Some(name) => fields.push((name, message(error))),
            None => others.push(message(error)),
        }
    }
    let all = errors.iter().map(message).collect::<Vec<_>>().join(", ");
    if fields.is_empty() {
        return AdminPublicError::new("USER_ERRORS", all).status(400).into();
    }
    // Errors that are all under their fields need no message above them.
    let refusal = if others.is_empty() {
        AdminPublicError::new("INVALID_INPUT", all)
    } else {
        AdminPublicError::new("USER_ERRORS", others.join(", "))
    };
    refusal.status(422).field_errors(fields).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Field, Function};
    use serde_json::json;

    #[test]
    fn reads_the_route_from_the_path() {
        assert_eq!(route("/functions"), Some(Route::List));
        assert_eq!(route("/functions/"), Some(Route::List));
        assert_eq!(
            route("/functions/volume-discount"),
            Some(Route::Instances("volume-discount".into()))
        );
        assert_eq!(
            route("/functions/volume-discount/new"),
            Some(Route::New("volume-discount".into()))
        );
        assert_eq!(
            route("/functions/volume-discount/gid%3A%2F%2Fshopify%2FDiscountNode%2F1"),
            Some(Route::Edit(
                "volume-discount".into(),
                "gid://shopify/DiscountNode/1".into()
            ))
        );
        assert_eq!(route("/functions/a/b/c"), None);
        assert_eq!(route("/functionsx"), None);
        assert_eq!(
            instance_path("volume-discount", "gid://shopify/DiscountNode/1"),
            "/functions/volume-discount/gid%3A%2F%2Fshopify%2FDiscountNode%2F1"
        );
    }

    #[test]
    fn shows_shopify_user_errors_under_the_field_they_name() {
        let definition =
            Function::new(FunctionType::CartTransform, "cart-transform").field("message", Field::text("Message"));
        let function = AppFunction::of("cartTransform", &definition);
        let errors = [
            json!({ "field": ["blockOnFailure"], "message": "Not allowed" }),
            json!({ "field": null, "message": "Function is not deployed" }),
        ];
        let error = user_errors(&function, &errors);
        let public = error.downcast_ref::<AdminPublicError>().unwrap();
        assert_eq!(public.status, 422);
        assert_eq!(public.message, "Function is not deployed");
        assert_eq!(public.field_errors.as_ref().unwrap()["blockOnFailure"], "Not allowed");

        // None names a field of the form: one message, for a toast.
        let errors = [json!({ "field": ["input", "title"], "message": "Title is invalid" })];
        let error = user_errors(&function, &errors);
        let public = error.downcast_ref::<AdminPublicError>().unwrap();
        assert_eq!((public.status, public.message.as_str()), (400, "Title is invalid"));
        assert!(public.field_errors.is_none());
    }
}
