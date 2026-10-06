//! App Bridge globals exposed on `window.shopify`.

use crate::dom::{call_method, js_error_message, member};
use js_sys::{Object, Promise, Reflect};
use leptos::prelude::window;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

fn shopify() -> Option<JsValue> {
    member(&window(), "shopify")
}

pub(crate) fn has_id_token() -> bool {
    shopify().and_then(|shopify| member(&shopify, "idToken")).is_some()
}

/// A fresh session token from `shopify.idToken()`, or `None` outside Shopify admin.
pub async fn id_token() -> Result<Option<String>, String> {
    let Some(shopify) = shopify().filter(|shopify| member(shopify, "idToken").is_some()) else {
        return Ok(None);
    };
    let promise = call_method(&shopify, "idToken", &[]).map_err(|error| js_error_message(&error))?;
    let token = JsFuture::from(Promise::resolve(&promise))
        .await
        .map_err(|error| js_error_message(&error))?;
    Ok(token.as_string())
}

/// Options of `shopify.toast.show`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToastOptions {
    /// Display time in milliseconds.
    pub duration: Option<u32>,
    pub is_error: bool,
}

/// Shows an App Bridge toast. Returns `false` when toasts are unavailable.
pub fn toast(message: &str) -> bool {
    toast_with(message, ToastOptions::default())
}

/// Shows an App Bridge error toast. Returns `false` when toasts are unavailable.
pub fn toast_error(message: &str) -> bool {
    toast_with(
        message,
        ToastOptions {
            is_error: true,
            ..ToastOptions::default()
        },
    )
}

/// Shows an App Bridge toast with options. Returns `false` when toasts are unavailable.
pub fn toast_with(message: &str, options: ToastOptions) -> bool {
    let Some(toast) = shopify().and_then(|shopify| member(&shopify, "toast")) else {
        return false;
    };
    let mut arguments = vec![JsValue::from_str(message)];
    if options != ToastOptions::default() {
        let object = Object::new();
        if let Some(duration) = options.duration {
            let _ = Reflect::set(&object, &"duration".into(), &duration.into());
        }
        if options.is_error {
            let _ = Reflect::set(&object, &"isError".into(), &true.into());
        }
        arguments.push(object.unchecked_into());
    }
    let _ = call_method(&toast, "show", &arguments);
    true
}
