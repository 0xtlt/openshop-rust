//! What a test reads from the answer of a form of an admin page,
//! [`TestContext::submit`](super::TestContext::submit): its toasts, decoded.

use crate::admin_paths::rpc::decode_uri_component;
use reqwest::Response;

/// The header of the toast an action asked for, `Outcome::toast(…)`.
const TOAST_HEADER: &str = "x-openshop-toast";
/// The header of an error toast: `Outcome::error(…)`, an `AdminPublicError`,
/// a form refused that the view does not show.
const TOAST_ERROR_HEADER: &str = "x-openshop-toast-error";

/// The toast of the answer to a form, as the admin shows it:
/// `Outcome::toast("Saved")` is `Some("Saved")`, `None` without a toast.
///
/// The header, `x-openshop-toast`, is percent-encoded (`Saved%20Great`); this
/// decodes it, as `openshop.js` does. Read it before the body, which
/// `text()` consumes:
///
/// ```no_run
/// # async fn example(ctx: &openshop::testing::TestContext) -> openshop::Result<()> {
/// use openshop::testing::toast;
///
/// let saved = ctx.submit("/settings", &[("_action", "saveSettings"), ("threshold", "8")]).send().await?;
/// assert_eq!(toast(&saved).as_deref(), Some("Saved"));
/// let page = saved.text().await?;
/// # Ok(())
/// # }
/// ```
pub fn toast(response: &Response) -> Option<String> {
    decoded(response, TOAST_HEADER)
}

/// The error toast of the answer to a form, decoded as [`toast`] is: the
/// message of `Outcome::error(…)`, of an `AdminPublicError` the action
/// failed with, `Internal server error` for any other error, or the first
/// error of a form refused by a view without a `form` parameter. `None`
/// without one.
///
/// ```no_run
/// # async fn example(ctx: &openshop::testing::TestContext) -> openshop::Result<()> {
/// use openshop::testing::toast_error;
///
/// let refused = ctx.submit("/reviews", &[("_action", "publish")]).send().await?;
/// assert_eq!(refused.status(), 409);
/// assert_eq!(toast_error(&refused).as_deref(), Some("Already published"));
/// # Ok(())
/// # }
/// ```
pub fn toast_error(response: &Response) -> Option<String> {
    decoded(response, TOAST_ERROR_HEADER)
}

/// The header `name`, percent-decoded; as it is when it does not decode, as
/// the admin shows it then.
fn decoded(response: &Response, name: &str) -> Option<String> {
    let value = String::from_utf8_lossy(response.headers().get(name)?.as_bytes()).into_owned();
    Some(decode_uri_component(&value).unwrap_or(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(headers: &[(&str, &str)]) -> Response {
        let mut response = axum::http::Response::builder();
        for (name, value) in headers {
            response = response.header(*name, *value);
        }
        Response::from(response.body("").unwrap())
    }

    #[test]
    fn reads_the_toasts_decoded() {
        let saved = response(&[(TOAST_HEADER, "Saved%20%C2%AB%20caf%C3%A9%20%C2%BB%20100%25")]);
        assert_eq!(toast(&saved).as_deref(), Some("Saved « café » 100%"));
        assert_eq!(toast_error(&saved), None);

        let failed = response(&[(TOAST_ERROR_HEADER, "title%3A%20Required")]);
        assert_eq!(toast_error(&failed).as_deref(), Some("title: Required"));
        assert_eq!(toast(&failed), None);

        // What does not decode is shown as it is, as `openshop.js` shows it.
        assert_eq!(toast(&response(&[(TOAST_HEADER, "100%")])).as_deref(), Some("100%"));
        assert_eq!(toast(&response(&[])), None);
    }
}
