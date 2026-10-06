//! Form values of event targets; Polaris fields expose `value` / `checked` like native inputs.

use js_sys::Reflect;
use wasm_bindgen::JsValue;
use web_sys::Event;

fn target_property(event: &Event, name: &str) -> JsValue {
    event
        .target()
        .and_then(|target| Reflect::get(&target, &JsValue::from_str(name)).ok())
        .unwrap_or(JsValue::UNDEFINED)
}

pub fn event_value(event: &Event) -> String {
    let value = target_property(event, "value");
    value
        .as_string()
        .or_else(|| value.as_f64().map(|number| number.to_string()))
        .unwrap_or_default()
}

pub fn event_checked(event: &Event) -> bool {
    target_property(event, "checked").is_truthy()
}
