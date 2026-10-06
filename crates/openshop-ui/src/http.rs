//! Minimal `fetch` wrapper; URLs are sent exactly as given.

use crate::dom::js_error_message;
use js_sys::{ArrayBuffer, Uint8Array};
use leptos::prelude::window;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{AbortSignal, Headers, RequestInit};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }
}

pub struct Request<'a> {
    pub method: Method,
    pub url: &'a str,
    pub headers: &'a [(String, String)],
    pub body: Option<&'a str>,
    pub signal: Option<&'a AbortSignal>,
}

pub struct Response(web_sys::Response);

fn js_error(error: JsValue) -> String {
    js_error_message(&error)
}

impl Response {
    pub fn status(&self) -> u16 {
        self.0.status()
    }

    pub fn ok(&self) -> bool {
        self.0.ok()
    }

    pub fn header(&self, name: &str) -> Option<String> {
        self.0.headers().get(name).ok().flatten()
    }

    pub async fn text(&self) -> Result<String, String> {
        let text = JsFuture::from(self.0.text().map_err(js_error)?)
            .await
            .map_err(js_error)?;
        Ok(text.as_string().unwrap_or_default())
    }

    pub async fn bytes(&self) -> Result<Vec<u8>, String> {
        let buffer = JsFuture::from(self.0.array_buffer().map_err(js_error)?)
            .await
            .map_err(js_error)?;
        Ok(Uint8Array::new(&buffer.unchecked_into::<ArrayBuffer>()).to_vec())
    }
}

/// Resolves once the response headers arrive; fails on network errors and aborts.
pub async fn send(request: Request<'_>) -> Result<Response, String> {
    let headers = Headers::new().map_err(js_error)?;
    for (name, value) in request.headers {
        headers.set(name, value).map_err(js_error)?;
    }
    let init = RequestInit::new();
    init.set_method(request.method.as_str());
    init.set_headers(&headers);
    if let Some(body) = request.body {
        init.set_body(&JsValue::from_str(body));
    }
    init.set_signal(request.signal);
    let response = JsFuture::from(window().fetch_with_str_and_init(request.url, &init))
        .await
        .map_err(js_error)?;
    response.dyn_into().map(Response).map_err(js_error)
}
