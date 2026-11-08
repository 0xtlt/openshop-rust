//! The logger of a handler that is not a flow run: a webhook, a loader or an
//! action of an admin page. It has the methods of [`FlowLogger`](super::FlowLogger),
//! and its lines go to the process logger with what they were logged for.

use std::sync::Arc;

use serde_json::{Map, Value};

use super::logger::logger;
use super::report::Trail;

/// `ctx.logger` of a [`WebhookContext`](crate::WebhookContext) and of an
/// [`AdminContext`](crate::AdminContext): the methods of
/// [`FlowLogger`](super::FlowLogger), `info`, `warn`, `error` and their
/// `_with` forms, which take a JSON payload.
///
/// A line goes to the [runtime logger](super::RuntimeLogger) at its level, as
/// the flow logger's do, with its payload and what the context runs for: the shop,
/// the Shopify app, and the topic of a webhook or the request of an admin page.
/// A key of the payload wins over those. A handler has no run: its lines are
/// not stored in the database, unlike those of a flow. When the application
/// reports to Sentry, they are the breadcrumbs of a failure of the handler,
/// without their payload.
///
/// ```
/// use openshop::{Result, WebhookContext, json, webhook};
///
/// #[webhook("orders/create")]
/// pub async fn orders_create(ctx: WebhookContext) -> Result<()> {
///     ctx.logger.info("Order received");
///     ctx.logger.warn_with("No customer", json!({ "order": ctx.payload["id"] }));
///     Ok(())
/// }
/// ```
#[derive(Clone)]
pub struct ContextLogger {
    fields: Arc<Map<String, Value>>,
    trail: Trail,
}

impl Default for ContextLogger {
    /// A logger whose lines carry their payload only.
    fn default() -> Self {
        Self::new(Value::Null)
    }
}

impl std::fmt::Debug for ContextLogger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextLogger").field("fields", &self.fields).finish()
    }
}

impl ContextLogger {
    /// A logger whose lines carry `fields`, an object, next to their payload:
    /// `json!({ "shop": "demo.myshopify.com" })`. Anything else carries
    /// nothing. For a context built by hand, in a test.
    pub fn new(fields: Value) -> Self {
        Self::with_category(fields, "app")
    }

    /// The same, whose lines are the breadcrumbs of `category` in a report.
    pub(crate) fn with_category(fields: Value, category: &'static str) -> Self {
        let fields = match fields {
            Value::Object(fields) => fields,
            _ => Map::new(),
        };
        Self {
            fields: Arc::new(fields),
            trail: Trail::current_as(category),
        }
    }

    /// What a line is logged with: see [`with_fields`].
    fn context(&self, payload: Value) -> Value {
        with_fields(payload, &self.fields)
    }

    /// The level is the method of the runtime logger, which writes it: the
    /// message is not prefixed with it.
    fn log(&self, level: &'static str, message: &str, payload: Value) {
        let context = self.context(payload);
        let logger = logger();
        match level {
            "warn" => logger.warn(message, Some(&context)),
            "error" => logger.error(message, Some(&context)),
            _ => logger.info(message, Some(&context)),
        }
        self.trail.leave(level, message);
    }

    /// Logs a line.
    pub fn info(&self, message: impl AsRef<str>) {
        self.log("info", message.as_ref(), Value::Null);
    }

    /// Logs a line with data next to it: `json!({ "count": 3 })`.
    pub fn info_with(&self, message: impl AsRef<str>, payload: Value) {
        self.log("info", message.as_ref(), payload);
    }

    pub fn warn(&self, message: impl AsRef<str>) {
        self.log("warn", message.as_ref(), Value::Null);
    }

    pub fn warn_with(&self, message: impl AsRef<str>, payload: Value) {
        self.log("warn", message.as_ref(), payload);
    }

    pub fn error(&self, message: impl AsRef<str>) {
        self.log("error", message.as_ref(), Value::Null);
    }

    pub fn error_with(&self, message: impl AsRef<str>, payload: Value) {
        self.log("error", message.as_ref(), payload);
    }
}

/// What a line goes to the runtime logger with: its payload, and the
/// `fields` of its logger it does not name. A payload that is not an object
/// is under `payload`.
pub(super) fn with_fields(payload: Value, fields: &Map<String, Value>) -> Value {
    let mut context = match payload {
        Value::Object(payload) => payload,
        Value::Null => Map::new(),
        other => Map::from_iter([("payload".to_string(), other)]),
    };
    for (key, value) in fields {
        if !context.contains_key(key) {
            context.insert(key.clone(), value.clone());
        }
    }
    Value::Object(context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_line_carries_its_payload_and_what_the_context_runs_for() {
        let logger = ContextLogger::new(json!({ "shop": "demo.myshopify.com", "topic": "orders/create" }));
        assert_eq!(
            logger.context(Value::Null),
            json!({ "shop": "demo.myshopify.com", "topic": "orders/create" })
        );
        // The payload wins over a field of the same name.
        assert_eq!(
            logger.context(json!({ "count": 3, "topic": "mine" })),
            json!({ "count": 3, "topic": "mine", "shop": "demo.myshopify.com" })
        );
        assert_eq!(
            logger.context(json!([1, 2])),
            json!({ "payload": [1, 2], "shop": "demo.myshopify.com", "topic": "orders/create" })
        );
        assert_eq!(ContextLogger::default().context(json!({})), json!({}));
        assert_eq!(ContextLogger::new(json!("not an object")).context(Value::Null), json!({}));
    }
}
