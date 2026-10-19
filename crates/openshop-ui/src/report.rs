//! Failures no page has a place for: logged to the console, and shown to the merchant in a toast
//! when it asked for something.

use crate::shopify;
use std::fmt::Display;

/// The line logged for a failure.
pub fn failure_line(context: &str, error: &dyn Display) -> String {
    format!("[openshop] {context}: {error}")
}

#[cfg(target_arch = "wasm32")]
fn console_error(line: &str) {
    web_sys::console::error_1(&line.into());
}

// Native builds only exist for `cargo test`.
#[cfg(not(target_arch = "wasm32"))]
fn console_error(line: &str) {
    eprintln!("{line}");
}

/// Logs a failure to the browser console.
pub fn log_error(context: &str, error: &dyn Display) {
    console_error(&failure_line(context, error));
}

/// Logs a failure, and tells the merchant with an error toast carrying `message`.
pub fn report_error(context: &str, error: &dyn Display, message: &str) {
    log_error(context, error);
    shopify::toast_error(message);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_what_failed_and_why() {
        assert_eq!(
            failure_line("Request to /api/flows failed", &"Failed to fetch"),
            "[openshop] Request to /api/flows failed: Failed to fetch"
        );
    }
}
