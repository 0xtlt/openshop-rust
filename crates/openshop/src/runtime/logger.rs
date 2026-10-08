//! Pluggable process logger. The default forwards to `tracing`.

use std::sync::{Arc, RwLock};

use serde_json::Value;

pub trait RuntimeLogger: Send + Sync {
    fn info(&self, message: &str, context: Option<&Value>);
    fn warn(&self, message: &str, context: Option<&Value>);
    fn error(&self, message: &str, context: Option<&Value>);
}

struct TracingLogger;

impl RuntimeLogger for TracingLogger {
    fn info(&self, message: &str, context: Option<&Value>) {
        match context {
            Some(context) => tracing::info!(%context, "{message}"),
            None => tracing::info!("{message}"),
        }
    }

    fn warn(&self, message: &str, context: Option<&Value>) {
        match context {
            Some(context) => tracing::warn!(%context, "{message}"),
            None => tracing::warn!("{message}"),
        }
    }

    fn error(&self, message: &str, context: Option<&Value>) {
        match context {
            Some(context) => tracing::error!(%context, "{message}"),
            None => tracing::error!("{message}"),
        }
    }
}

static LOGGER: RwLock<Option<Arc<dyn RuntimeLogger>>> = RwLock::new(None);

fn default_logger() -> Arc<dyn RuntimeLogger> {
    Arc::new(TracingLogger)
}

/// Replaces the process logger and returns the previous one.
pub fn set_logger(next: Arc<dyn RuntimeLogger>) -> Arc<dyn RuntimeLogger> {
    let mut slot = LOGGER.write().unwrap_or_else(|poisoned| poisoned.into_inner());
    slot.replace(next).unwrap_or_else(default_logger)
}

pub fn logger() -> Arc<dyn RuntimeLogger> {
    LOGGER
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .unwrap_or_else(default_logger)
}

pub(crate) fn info(message: impl AsRef<str>) {
    logger().info(message.as_ref(), None);
}

pub(crate) fn warn(message: impl AsRef<str>) {
    logger().warn(message.as_ref(), None);
}

pub(crate) fn warn_with(message: impl AsRef<str>, context: Value) {
    logger().warn(message.as_ref(), Some(&context));
}

pub(crate) fn info_with(message: impl AsRef<str>, context: Value) {
    logger().info(message.as_ref(), Some(&context));
}

pub(crate) fn error_with(message: impl AsRef<str>, context: Value) {
    logger().error(message.as_ref(), Some(&context));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Capture(Mutex<Vec<String>>);

    impl RuntimeLogger for Capture {
        fn info(&self, message: &str, _context: Option<&Value>) {
            self.0.lock().unwrap().push(format!("info:{message}"));
        }
        fn warn(&self, message: &str, _context: Option<&Value>) {
            self.0.lock().unwrap().push(format!("warn:{message}"));
        }
        fn error(&self, message: &str, context: Option<&Value>) {
            self.0
                .lock()
                .unwrap()
                .push(format!("error:{message}:{}", context.is_some()));
        }
    }

    #[test]
    fn swaps_the_runtime_logger_and_restores_the_previous_one() {
        let capture = Arc::new(Capture::default());
        let previous = set_logger(capture.clone());

        logger().info("hello", None);
        logger().warn("careful", None);
        logger().error("boom", Some(&serde_json::json!({ "code": 1 })));

        let restored = set_logger(previous);
        restored.info("after", None);

        let lines = capture.0.lock().unwrap().clone();
        assert!(lines.contains(&"info:hello".to_string()));
        assert!(lines.contains(&"warn:careful".to_string()));
        assert!(lines.contains(&"error:boom:true".to_string()));
        assert!(lines.contains(&"info:after".to_string()));
    }
}
