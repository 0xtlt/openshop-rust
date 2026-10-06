//! Browser glue shared by the pages: modals, clipboard, downloads, history and locale formatting.

use gloo_timers::callback::Interval;
use js_sys::{Array, Date, Function, Object, Promise, Reflect, Uint8Array};
use leptos::prelude::{document, on_cleanup, window};
use send_wrapper::SendWrapper;
use std::cmp::Ordering;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Blob, BlobPropertyBag, Element, Event, EventTarget, HtmlAnchorElement, Url};

/// `target[name]` unless it is `null` or `undefined`.
pub fn member(target: &JsValue, name: &str) -> Option<JsValue> {
    Reflect::get(target, &JsValue::from_str(name))
        .ok()
        .filter(|value| !value.is_null() && !value.is_undefined())
}

/// `target[method](...arguments)`.
pub fn call_method(target: &JsValue, method: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let function: Function = Reflect::get(target, &JsValue::from_str(method))?.dyn_into()?;
    function.apply(target, &arguments.iter().collect::<Array>())
}

/// `error instanceof Error ? error.message : String(error)`.
pub fn js_error_message(error: &JsValue) -> String {
    if let Some(error) = error.dyn_ref::<js_sys::Error>() {
        return String::from(error.message());
    }
    error.as_string().unwrap_or_else(|| format!("{error:?}"))
}

pub fn element_by_id(id: &str) -> Option<Element> {
    document().get_element_by_id(id)
}

fn call_element_method(id: &str, method: &str) {
    if let Some(element) = element_by_id(id) {
        let _ = call_method(&element, method, &[]);
    }
}

/// Opens an App Bridge `ui-modal`.
pub fn show_modal(id: &str) {
    call_element_method(id, "show");
}

/// Closes an App Bridge `ui-modal`.
pub fn hide_modal(id: &str) {
    call_element_method(id, "hide");
}

/// Closes a Polaris `s-modal`.
pub fn hide_overlay(id: &str) {
    call_element_method(id, "hideOverlay");
}

pub fn location_pathname() -> String {
    window().location().pathname().unwrap_or_default()
}

pub fn location_search() -> String {
    window().location().search().unwrap_or_default()
}

pub fn location_origin() -> String {
    window().location().origin().unwrap_or_default()
}

/// `history.replaceState(null, '', url)`; App Bridge mirrors it to the admin address bar.
pub fn replace_url(url: &str) {
    if let Ok(history) = window().history() {
        let _ = history.replace_state_with_url(&JsValue::NULL, "", Some(url));
    }
}

pub fn now_ms() -> i64 {
    Date::now() as i64
}

pub async fn copy_text(value: &str) -> Result<(), JsValue> {
    let clipboard = Reflect::get(&window().navigator(), &JsValue::from_str("clipboard"))?;
    let promise: Promise = call_method(&clipboard, "writeText", &[JsValue::from_str(value)])?.dyn_into()?;
    JsFuture::from(promise).await.map(drop)
}

fn save_blob(blob: Result<Blob, JsValue>, filename: &str) {
    let Ok(url) = blob.and_then(|blob| Url::create_object_url_with_blob(&blob)) else {
        return;
    };
    if let Ok(anchor) = document().create_element("a") {
        let anchor: HtmlAnchorElement = anchor.unchecked_into();
        anchor.set_href(&url);
        anchor.set_download(filename);
        anchor.click();
    }
    let _ = Url::revoke_object_url(&url);
}

fn blob_options(content_type: &str) -> BlobPropertyBag {
    let options = BlobPropertyBag::new();
    options.set_type(content_type);
    options
}

pub fn download_text(text: &str, content_type: &str, filename: &str) {
    let parts = Array::of1(&JsValue::from_str(text));
    save_blob(
        Blob::new_with_str_sequence_and_options(&parts, &blob_options(content_type)),
        filename,
    );
}

pub fn download_bytes(bytes: &[u8], content_type: &str, filename: &str) {
    let parts = Array::of1(&Uint8Array::from(bytes));
    save_blob(
        Blob::new_with_u8_array_sequence_and_options(&parts, &blob_options(content_type)),
        filename,
    );
}

/// A `Date` for an ISO timestamp; a missing one is the epoch, like `new Date(value ?? 0)`.
pub fn date(iso: Option<&str>) -> Date {
    match iso {
        Some(iso) => Date::new(&JsValue::from_str(iso)),
        None => Date::new(&JsValue::from_f64(0.0)),
    }
}

/// Calls `toLocaleString` / `toLocaleDateString` / `toLocaleTimeString` in the browser's
/// locale with `Intl.DateTimeFormat` options.
pub fn localized(date: &Date, method: &str, options: &[(&str, &str)]) -> String {
    let arguments = if options.is_empty() {
        Vec::new()
    } else {
        let object = Object::new();
        for (key, value) in options {
            let _ = Reflect::set(&object, &JsValue::from_str(key), &JsValue::from_str(value));
        }
        vec![JsValue::UNDEFINED, object.unchecked_into()]
    };
    call_method(date, method, &arguments)
        .ok()
        .and_then(|formatted| formatted.as_string())
        .unwrap_or_default()
}

/// `new Date(iso).toLocaleString()`.
pub fn locale_date_time(iso: &str) -> String {
    localized(&date(Some(iso)), "toLocaleString", &[])
}

/// `left.localeCompare(right)`.
pub fn locale_compare(left: &str, right: &str) -> Ordering {
    call_method(&JsValue::from_str(left), "localeCompare", &[JsValue::from_str(right)])
        .ok()
        .and_then(|order| order.as_f64())
        .map_or_else(|| left.cmp(right), |order| order.total_cmp(&0.0))
}

/// Keeps a browser handle (timer, listener…) alive until the current reactive owner is
/// cleaned up, then drops it.
pub fn hold_until_cleanup<T: 'static>(handle: T) {
    let handle = SendWrapper::new(handle);
    on_cleanup(move || drop(handle));
}

/// `setInterval` tied to the current reactive owner.
pub fn every(millis: u32, tick: impl FnMut() + 'static) {
    hold_until_cleanup(Interval::new(millis, tick));
}

/// A DOM event listener removed on drop.
pub struct Listener {
    target: EventTarget,
    event: &'static str,
    callback: Closure<dyn FnMut(Event)>,
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self
            .target
            .remove_event_listener_with_callback(self.event, self.callback.as_ref().unchecked_ref());
    }
}

pub fn listen(target: &EventTarget, event: &'static str, handler: impl FnMut(Event) + 'static) -> Listener {
    let callback = Closure::<dyn FnMut(Event)>::new(handler);
    let _ = target.add_event_listener_with_callback(event, callback.as_ref().unchecked_ref());
    Listener {
        target: target.clone(),
        event,
        callback,
    }
}
