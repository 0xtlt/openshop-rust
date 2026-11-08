//! Typed loaders and actions for custom admin pages.
//!
//! A custom page talks to its server-side functions through [`use_loader`] and [`use_action`].
//! Both post to an internal RPC endpoint scoped to the current page and authenticated with a
//! fresh App Bridge session token.

use crate::{
    dom,
    fetch::session_headers,
    http::{self, Method},
    shopify,
};
use leptos::{prelude::*, task::spawn_local};
use leptos_router::hooks::use_location;
pub use openshop_shared::AdminFunctionKind;
use openshop_shared::build_admin_function_rpc_url;
use send_wrapper::SendWrapper;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashMap},
    fmt,
    marker::PhantomData,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use web_sys::{AbortController, AbortSignal};

/// Client-side handle to a loader or action registered on the server for a custom admin page.
///
/// `I` is the input sent as JSON, `O` the JSON output. Use `()` for a function without input.
///
/// ```
/// use openshop_ui::admin::AdminFunctionRef;
///
/// #[derive(serde::Serialize)]
/// struct SaveReview { title: String }
///
/// const LIST_REVIEWS: AdminFunctionRef<(), Vec<String>> = AdminFunctionRef::loader("listReviews");
/// const SAVE_REVIEW: AdminFunctionRef<SaveReview, bool> = AdminFunctionRef::action("saveReview");
/// assert_eq!(LIST_REVIEWS.name(), "listReviews");
/// assert_eq!(SAVE_REVIEW.kind().as_str(), "action");
/// ```
pub struct AdminFunctionRef<I, O> {
    kind: AdminFunctionKind,
    name: &'static str,
    types: PhantomData<fn(I) -> O>,
}

impl<I, O> AdminFunctionRef<I, O> {
    pub const fn loader(name: &'static str) -> Self {
        Self {
            kind: AdminFunctionKind::Loader,
            name,
            types: PhantomData,
        }
    }

    pub const fn action(name: &'static str) -> Self {
        Self {
            kind: AdminFunctionKind::Action,
            name,
            types: PhantomData,
        }
    }

    pub const fn kind(&self) -> AdminFunctionKind {
        self.kind
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }
}

impl<I, O> Clone for AdminFunctionRef<I, O> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<I, O> Copy for AdminFunctionRef<I, O> {}

impl<I, O> fmt::Debug for AdminFunctionRef<I, O> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdminFunctionRef")
            .field("kind", &self.kind)
            .field("name", &self.name)
            .finish()
    }
}

/// Failure of a loader or action call.
///
/// `code`, `request_id`, `field_errors` and `details` are set when the server answered with a
/// structured error; transport and decoding failures only carry a `message`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdminRpcError {
    pub message: String,
    pub code: Option<String>,
    pub request_id: Option<String>,
    pub field_errors: Option<BTreeMap<String, String>>,
    /// What else the server sent with the error, by name: `pricingUrl` and `pricingLink` for a
    /// `PLAN_REQUIRED` error, the page where the merchant chooses a plan.
    pub details: Option<BTreeMap<String, String>>,
}

impl AdminRpcError {
    fn from_message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Self::default()
        }
    }

    fn from_payload(payload: &Value) -> Self {
        let text = |key: &str| payload.get(key).and_then(Value::as_str).map(str::to_string);
        let texts = |key: &str| {
            payload.get(key).and_then(Value::as_object).map(|values| {
                values
                    .iter()
                    .filter_map(|(name, value)| Some((name.clone(), value.as_str()?.to_string())))
                    .collect()
            })
        };
        Self {
            message: text("error").unwrap_or_default(),
            code: text("code"),
            request_id: text("requestId"),
            field_errors: texts("fieldErrors"),
            details: texts("details"),
        }
    }

    /// A value the server sent with the error, by name: `error.detail("pricingUrl")`.
    pub fn detail(&self, name: &str) -> Option<&str> {
        self.details.as_ref()?.get(name).map(String::as_str)
    }
}

impl fmt::Display for AdminRpcError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AdminRpcError {}

fn rpc_body(input_json: &str) -> String {
    format!(r#"{{"input":{input_json}}}"#)
}

fn serialize_input<I: Serialize>(input: &I) -> Result<String, AdminRpcError> {
    serde_json::to_string(input).map_err(|error| AdminRpcError::from_message(error.to_string()))
}

fn parse_rpc_response<O: DeserializeOwned>(ok: bool, body: &str) -> Result<O, AdminRpcError> {
    let decode_error = |error: serde_json::Error| AdminRpcError::from_message(error.to_string());
    let payload: Value = serde_json::from_str(body).map_err(decode_error)?;
    if !ok {
        return Err(AdminRpcError::from_payload(&payload));
    }
    serde_json::from_value(payload).map_err(decode_error)
}

struct Call<'a> {
    kind: AdminFunctionKind,
    name: &'a str,
    pathname: &'a str,
    input_json: &'a str,
    signal: Option<&'a AbortSignal>,
    idempotency_key: Option<&'a str>,
}

async fn call_admin_function<O: DeserializeOwned>(call: Call<'_>) -> Result<O, AdminRpcError> {
    let url = build_admin_function_rpc_url(call.kind.as_str(), call.name, call.pathname);
    // Unlike the requests of the built-in screens, a call needs its session.
    let token = shopify::id_token().await.map_err(AdminRpcError::from_message)?;
    let mut headers = session_headers(token, true);
    if let Some(key) = call.idempotency_key {
        headers.push(("idempotency-key".to_string(), key.to_string()));
    }
    // A binding of its own: Rust 1.88 does not extend a temporary borrowed
    // inside `Some(..)` of a struct literal.
    let body = rpc_body(call.input_json);
    let request = http::Request {
        method: Method::Post,
        url: &url,
        headers: &headers,
        body: Some(&body),
        signal: call.signal,
    };
    let response = http::send(request).await.map_err(AdminRpcError::from_message)?;
    let body = response.text().await.map_err(AdminRpcError::from_message)?;
    parse_rpc_response(response.ok(), &body)
}

/// Revalidation counters of the mounted `use_loader` hooks, by loader name.
type LoaderListeners = HashMap<&'static str, Vec<(u64, RwSignal<u64>)>>;

thread_local! {
    static LOADER_LISTENERS: RefCell<LoaderListeners> = RefCell::default();
    static NEXT_LISTENER_ID: Cell<u64> = const { Cell::new(0) };
}

fn revalidate(loader: &'static str) {
    let listeners = LOADER_LISTENERS.with(|listeners| listeners.borrow().get(loader).cloned());
    for (_, generation) in listeners.unwrap_or_default() {
        generation.update(|generation| *generation += 1);
    }
}

fn listen_for_revalidation(loader: &'static str, generation: RwSignal<u64>) {
    let id = NEXT_LISTENER_ID.with(|next| next.replace(next.get() + 1));
    LOADER_LISTENERS.with(|listeners| {
        listeners.borrow_mut().entry(loader).or_default().push((id, generation));
    });
    on_cleanup(move || {
        LOADER_LISTENERS.with(|listeners| {
            let mut listeners = listeners.borrow_mut();
            if let Some(entries) = listeners.get_mut(loader) {
                entries.retain(|(entry, _)| *entry != id);
                if entries.is_empty() {
                    listeners.remove(loader);
                }
            }
        });
    });
}

/// Reactive state of a [`use_loader`] call.
pub struct AdminLoaderState<O: 'static> {
    /// Last successful output; kept while a reload is in flight, cleared when a load fails.
    pub data: ReadSignal<Option<O>>,
    pub error: ReadSignal<Option<AdminRpcError>>,
    pub loading: ReadSignal<bool>,
    generation: RwSignal<u64>,
}

impl<O> AdminLoaderState<O> {
    /// Calls the loader again.
    pub fn revalidate(&self) {
        self.generation.update(|generation| *generation += 1);
    }
}

impl<O> Clone for AdminLoaderState<O> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<O> Copy for AdminLoaderState<O> {}

/// Calls a loader when the page mounts and again whenever `input` (tracked reactively,
/// compared as JSON) changes or the loader is revalidated. Responses of superseded calls are
/// discarded.
///
/// Must be called while rendering a custom admin page.
pub fn use_loader<I, O>(
    reference: AdminFunctionRef<I, O>,
    input: impl Fn() -> I + Send + Sync + 'static,
) -> AdminLoaderState<O>
where
    I: Serialize + 'static,
    O: DeserializeOwned + Send + Sync + 'static,
{
    let data = RwSignal::new(None::<O>);
    let error = RwSignal::new(None::<AdminRpcError>);
    let loading = RwSignal::new(true);
    let generation = RwSignal::new(0_u64);
    let serialized = Memo::new(move |_| serialize_input(&input()));
    let pathname = use_location().pathname;
    listen_for_revalidation(reference.name, generation);

    Effect::new(move |_| {
        let input_json = serialized.get();
        let pathname = pathname.get();
        generation.track();
        loading.set(true);
        error.set(None);

        let controller = AbortController::new().ok();
        let signal = controller.as_ref().map(AbortController::signal);
        let superseded = Arc::new(AtomicBool::new(false));
        spawn_local({
            let superseded = Arc::clone(&superseded);
            async move {
                let result = match input_json {
                    Ok(input_json) => {
                        call_admin_function::<O>(Call {
                            kind: reference.kind,
                            name: reference.name,
                            pathname: &pathname,
                            input_json: &input_json,
                            signal: signal.as_ref(),
                            idempotency_key: None,
                        })
                        .await
                    }
                    Err(error) => Err(error),
                };
                if superseded.load(Ordering::Relaxed) {
                    return;
                }
                match result {
                    Ok(output) => data.set(Some(output)),
                    Err(failure) => {
                        data.set(None);
                        error.set(Some(failure));
                    }
                }
                loading.set(false);
            }
        });

        let controller = SendWrapper::new(controller);
        on_cleanup(move || {
            superseded.store(true, Ordering::Relaxed);
            if let Some(controller) = controller.take() {
                controller.abort();
            }
        });
    });

    AdminLoaderState {
        data: data.read_only(),
        error: error.read_only(),
        loading: loading.read_only(),
        generation,
    }
}

/// What [`AdminActionState::invoke`] does while a previous call is still in flight.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ActionConcurrency {
    /// Skip the new call; `invoke` resolves to `Ok(None)`.
    #[default]
    Ignore,
    /// Run the calls concurrently; the latest invocation owns `data` and `error`.
    Allow,
}

/// Per-call options of [`AdminActionState::invoke`].
#[derive(Debug, Clone, Default)]
pub struct AdminActionOptions {
    pub concurrency: ActionConcurrency,
    /// Sent as the `idempotency-key` header.
    pub idempotency_key: Option<String>,
    revalidate: Vec<&'static str>,
}

impl AdminActionOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn concurrency(mut self, concurrency: ActionConcurrency) -> Self {
        self.concurrency = concurrency;
        self
    }

    pub fn idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = Some(key.into());
        self
    }

    /// Reloads every mounted [`use_loader`] of `loader` once the action succeeds.
    pub fn revalidate<I, O>(mut self, loader: AdminFunctionRef<I, O>) -> Self {
        self.revalidate.push(loader.name);
        self
    }
}

/// Counts a call of [`AdminActionState::invoke`] in flight until it is
/// dropped. Its future may be dropped mid-call (the page is left, or the
/// caller stops waiting): the count still goes down, so `pending` does not
/// stay set and later [`ActionConcurrency::Ignore`] calls are not skipped
/// for good.
struct InFlightCall {
    in_flight: StoredValue<u32>,
    set_pending: WriteSignal<bool>,
}

impl Drop for InFlightCall {
    fn drop(&mut self) {
        let still_pending = self
            .in_flight
            .try_update_value(|count| {
                *count = count.saturating_sub(1);
                *count > 0
            })
            .unwrap_or(false);
        self.set_pending.try_set(still_pending);
    }
}

/// Reactive state of a [`use_action`] call.
pub struct AdminActionState<I: 'static, O: 'static> {
    /// Output of the latest successful invocation.
    pub data: ReadSignal<Option<O>>,
    pub error: ReadSignal<Option<AdminRpcError>>,
    pub pending: ReadSignal<bool>,
    reference: AdminFunctionRef<I, O>,
    pathname: Memo<String>,
    set_data: WriteSignal<Option<O>>,
    set_error: WriteSignal<Option<AdminRpcError>>,
    set_pending: WriteSignal<bool>,
    in_flight: StoredValue<u32>,
    latest_invocation: StoredValue<u32>,
}

impl<I, O> Clone for AdminActionState<I, O> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<I, O> Copy for AdminActionState<I, O> {}

impl<I, O> AdminActionState<I, O>
where
    I: Serialize + 'static,
    O: DeserializeOwned + Clone + Send + Sync + 'static,
{
    /// Calls the action. Never retries on its own.
    ///
    /// Resolves to `Ok(None)` when the call was skipped because another one is in flight and
    /// `options.concurrency` is [`ActionConcurrency::Ignore`].
    pub async fn invoke(&self, input: I, options: AdminActionOptions) -> Result<Option<O>, AdminRpcError> {
        let in_flight = self.in_flight.try_get_value().unwrap_or(0);
        if in_flight > 0 && options.concurrency != ActionConcurrency::Allow {
            return Ok(None);
        }
        self.in_flight.try_update_value(|count| *count += 1);
        let call = InFlightCall {
            in_flight: self.in_flight,
            set_pending: self.set_pending,
        };
        let invocation = self
            .latest_invocation
            .try_update_value(|latest| {
                *latest += 1;
                *latest
            })
            .unwrap_or(0);
        self.set_pending.set(true);
        self.set_error.set(None);

        let pathname = self.pathname.try_get_untracked().unwrap_or_else(dom::location_pathname);
        let result = match serialize_input(&input) {
            Ok(input_json) => {
                call_admin_function::<O>(Call {
                    kind: self.reference.kind,
                    name: self.reference.name,
                    pathname: &pathname,
                    input_json: &input_json,
                    signal: None,
                    idempotency_key: options.idempotency_key.as_deref(),
                })
                .await
            }
            Err(error) => Err(error),
        };

        let is_latest = self.latest_invocation.try_get_value() == Some(invocation);
        match result {
            Ok(output) => {
                if is_latest {
                    self.set_data.set(Some(output.clone()));
                    self.set_error.set(None);
                }
                drop(call);
                for loader in options.revalidate {
                    revalidate(loader);
                }
                Ok(Some(output))
            }
            Err(failure) => {
                if is_latest {
                    self.set_data.set(None);
                    self.set_error.set(Some(failure.clone()));
                }
                drop(call);
                Err(failure)
            }
        }
    }

    /// Clears `data` and `error`.
    pub fn reset(&self) {
        self.set_data.set(None);
        self.set_error.set(None);
        self.set_pending.set(self.in_flight.try_get_value().unwrap_or(0) > 0);
    }
}

/// Prepares an action. Nothing is sent until [`AdminActionState::invoke`] is called.
///
/// Must be called while rendering a custom admin page.
pub fn use_action<I, O>(reference: AdminFunctionRef<I, O>) -> AdminActionState<I, O>
where
    I: Serialize + 'static,
    O: DeserializeOwned + Clone + Send + Sync + 'static,
{
    let (data, set_data) = signal(None::<O>);
    let (error, set_error) = signal(None::<AdminRpcError>);
    let (pending, set_pending) = signal(false);
    AdminActionState {
        data,
        error,
        pending,
        reference,
        pathname: use_location().pathname,
        set_data,
        set_error,
        set_pending,
        in_flight: StoredValue::new(0),
        latest_invocation: StoredValue::new(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct SaveReview {
        title: String,
        review_id: Option<u32>,
    }

    #[test]
    fn wraps_the_input_or_null_in_the_rpc_body() {
        assert_eq!(rpc_body(&serialize_input(&()).unwrap()), r#"{"input":null}"#);
        assert_eq!(rpc_body(&serialize_input(&None::<u8>).unwrap()), r#"{"input":null}"#);
        let input = SaveReview {
            title: "Great".to_string(),
            review_id: Some(7),
        };
        assert_eq!(
            rpc_body(&serialize_input(&input).unwrap()),
            r#"{"input":{"title":"Great","reviewId":7}}"#
        );
    }

    #[test]
    fn decodes_successful_outputs() {
        let output: Vec<String> = parse_rpc_response(true, r#"["a","b"]"#).unwrap();
        assert_eq!(output, ["a", "b"]);
        let unit: Option<bool> = parse_rpc_response(true, "null").unwrap();
        assert_eq!(unit, None);
    }

    #[test]
    fn maps_error_payloads_to_admin_rpc_errors() {
        let error = parse_rpc_response::<Value>(
            false,
            r#"{"error":"Title is required","code":"invalid_input","requestId":"req_1",
                "fieldErrors":{"title":"Required"}}"#,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "Title is required");
        assert_eq!(error.code.as_deref(), Some("invalid_input"));
        assert_eq!(error.request_id.as_deref(), Some("req_1"));
        assert_eq!(
            error.field_errors,
            Some(BTreeMap::from([("title".to_string(), "Required".to_string())]))
        );

        let bare = AdminRpcError::from_payload(&json!({ "error": "Forbidden" }));
        assert_eq!(bare, AdminRpcError::from_message("Forbidden"));
        assert_eq!(bare.detail("pricingUrl"), None);

        let plan = AdminRpcError::from_payload(&json!({
            "error": "Choose a plan to use this feature",
            "code": "PLAN_REQUIRED",
            "details": { "pricingLink": "shopify:admin/charges/app/pricing_plans", "count": 1 }
        }));
        assert_eq!(
            plan.detail("pricingLink"),
            Some("shopify:admin/charges/app/pricing_plans")
        );
        // Only texts are kept.
        assert_eq!(plan.detail("count"), None);
    }

    #[test]
    fn reports_undecodable_responses() {
        assert!(parse_rpc_response::<Value>(false, "<html>").is_err());
        assert!(parse_rpc_response::<u32>(true, r#""text""#).is_err());
    }

    #[test]
    fn builds_references_in_const_context() {
        const LIST: AdminFunctionRef<(), Vec<String>> = AdminFunctionRef::loader("listReviews");
        const SAVE: AdminFunctionRef<SaveReview, bool> = AdminFunctionRef::action("saveReview");
        assert_eq!((LIST.kind(), LIST.name()), (AdminFunctionKind::Loader, "listReviews"));
        assert_eq!((SAVE.kind(), SAVE.name()), (AdminFunctionKind::Action, "saveReview"));
        let options = AdminActionOptions::new()
            .concurrency(ActionConcurrency::Allow)
            .idempotency_key("key-1")
            .revalidate(LIST);
        assert_eq!(options.revalidate, ["listReviews"]);
        assert_eq!(options.idempotency_key.as_deref(), Some("key-1"));
    }
}
