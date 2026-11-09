//! `/mcp`: the MCP server of the app and the tokens of its clients. The URL
//! of the server, to copy; the tokens of the shop, newest first, each with
//! its status, its expiration and its last use, a form to disable or enable
//! it, to rotate it and to revoke it (confirmed in a modal), and a modal that
//! edits its permissions and shows its recent audit; a modal that creates a
//! token. The secret of a token created or rotated is shown once, in a
//! banner, with a button that copies it.
//!
//! The forms post to `/mcp`, as forms: a `POST` of `/mcp` with another body
//! is the MCP endpoint.

mod view;

use anyhow::Result;
use maud::Markup;
use serde_json::{Value, json};

use super::{BuiltinPage, field};
use crate::admin::{AdminPublicError, Outcome};
use crate::server::api::mcp::{
    TokenRefusal, create_token_for, revoke_token_for, rotate_token_for, set_permissions_for, token_items,
    update_token_for,
};
use crate::types::JsonMap;

/// The form of the create modal.
const CREATE: &str = "createToken";
/// The form of a row that disables or enables its token.
const UPDATE: &str = "updateToken";
/// The form of the edit modal of a token.
const SET_PERMISSIONS: &str = "setPermissions";
/// The form of a row that gives its token a new secret.
const ROTATE: &str = "rotateToken";
/// The form of the modal that confirms a revocation.
const REVOKE: &str = "revokeToken";

/// The audits the edit modal of a token shows.
const RECENT_AUDITS: i64 = 10;

pub(super) async fn render(page: &BuiltinPage<'_>) -> Result<Markup> {
    let app = page.app;
    let tokens = token_items(app, page.session, RECENT_AUDITS).await?;
    let app_url = app
        .apps()
        .by_handle(Some(&page.session.shopify_app))
        .map(|shopify_app| shopify_app.app_url.as_str())
        .unwrap_or_default();
    let revealed = page.revealed();
    let secret = revealed.as_ref().and_then(|revealed| {
        Some(Secret {
            token: revealed.get("token")?.as_str()?,
            message: revealed.get("message")?.as_str()?,
        })
    });
    Ok(view::view(
        &mcp_url(app_url),
        &app.mcp_registry().permissions,
        &tokens,
        secret,
        page.form,
    ))
}

/// The secret of a token, made by the action that ran: shown once.
#[derive(Clone, Copy)]
struct Secret<'a> {
    token: &'a str,
    /// What happened: `Token created. …`.
    message: &'a str,
}

/// Where MCP clients reach the server: `/mcp` at the origin of the app,
/// where the admin is loaded from.
fn mcp_url(app_url: &str) -> String {
    match url::Url::parse(app_url.trim()) {
        Ok(url) if url.origin().is_tuple() => format!("{}/mcp", url.origin().ascii_serialization()),
        _ => "/mcp".to_string(),
    }
}

/// The expiration a form chose, as the API takes it: `none` is no
/// expiration, a number a number of days; anything else is refused there.
fn expiration_value(choice: &str) -> Value {
    match choice.trim() {
        "none" => Value::Null,
        days => days.parse::<i64>().map_or_else(|_| Value::from(days), Value::from),
    }
}

/// The permissions a form checked.
fn checked_permissions(fields: &[(String, String)]) -> Value {
    fields
        .iter()
        .filter(|(name, _)| name == "permissions")
        .map(|(_, key)| Value::from(key.as_str()))
        .collect()
}

/// The request of the create form, as `POST /api/mcp/tokens` takes it.
fn create_body(fields: &[(String, String)]) -> JsonMap {
    let mut body = JsonMap::new();
    body.insert("name".into(), Value::from(field(fields, "name").unwrap_or_default()));
    body.insert("permissions".into(), checked_permissions(fields));
    if let Some(choice) = field(fields, "expiresInDays") {
        body.insert("expiresInDays".into(), expiration_value(choice));
    }
    body
}

/// What the page answers to a refusal of the API: under the field it is
/// about, else in an error toast.
fn refusal(refused: TokenRefusal) -> anyhow::Error {
    let message = refused.message();
    let status = refused.status().as_u16();
    match refused {
        TokenRefusal::NotFound => AdminPublicError::new("NOT_FOUND", message).status(status).into(),
        TokenRefusal::Revoked(_) => AdminPublicError::new("CONFLICT", message).status(status).into(),
        TokenRefusal::Invalid { field, .. } => {
            let shown = match field {
                "name" => "Token name is required.".to_string(),
                "expiresInDays" | "expiresAt" => "Must be a whole number of days, or no expiration".to_string(),
                _ => message.clone(),
            };
            AdminPublicError::new("INVALID_INPUT", message)
                .status(422)
                .field_errors([(field, shown)])
                .into()
        }
    }
}

/// The token a form is about.
fn token_id(fields: &[(String, String)]) -> Result<&str> {
    field(fields, "id")
        .filter(|id| !id.is_empty())
        .ok_or_else(|| AdminPublicError::new("INVALID_INPUT", "id is required").into())
}

/// The forms of the page. A token created or rotated gives the page its
/// secret, shown once.
pub(super) async fn submit(page: &BuiltinPage<'_>, action: &str, fields: &[(String, String)]) -> Result<Outcome> {
    let (app, session) = (page.app, page.session);
    match action {
        CREATE => {
            let (token, _) = create_token_for(app, session, &create_body(fields))
                .await?
                .map_err(refusal)?;
            let message = "Token created. Copy it now; it will not be shown again.";
            page.reveal(json!({ "token": token, "message": message }));
            Ok(Outcome::toast("Token created"))
        }
        UPDATE => {
            let id = token_id(fields)?;
            let (status, toast) = match field(fields, "status") {
                Some("active") => ("active", "Token enabled"),
                Some("disabled") => ("disabled", "Token disabled"),
                _ => return Err(AdminPublicError::new("INVALID_INPUT", "status must be active or disabled").into()),
            };
            let body = JsonMap::from_iter([("status".to_string(), Value::from(status))]);
            update_token_for(app, session, id, &body).await?.map_err(refusal)?;
            Ok(Outcome::toast(toast))
        }
        SET_PERMISSIONS => {
            let id = token_id(fields)?;
            let body = JsonMap::from_iter([("permissions".to_string(), checked_permissions(fields))]);
            set_permissions_for(app, session, id, &body).await?.map_err(refusal)?;
            Ok(Outcome::toast("Permissions saved"))
        }
        ROTATE => {
            let token = rotate_token_for(app, session, token_id(fields)?)
                .await?
                .map_err(refusal)?;
            let message = "Token rotated. Copy the new value now.";
            page.reveal(json!({ "token": token, "message": message }));
            Ok(Outcome::toast("Token rotated"))
        }
        REVOKE => {
            revoke_token_for(app, session, token_id(fields)?)
                .await?
                .map_err(refusal)?;
            Ok(Outcome::toast("Token revoked"))
        }
        _ => Err(super::no_action()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::parse_fields;

    #[test]
    fn the_url_of_the_server_is_at_the_origin_of_the_app() {
        assert_eq!(mcp_url("https://app.example.com/admin/"), "https://app.example.com/mcp");
        assert_eq!(mcp_url("http://localhost:3000"), "http://localhost:3000/mcp");
        assert_eq!(mcp_url(""), "/mcp");
    }

    #[test]
    fn reads_the_create_form_as_the_api_takes_it() {
        let fields =
            parse_fields(b"_action=createToken&name=Claude&expiresInDays=30&permissions=read_logs&permissions=run_flows");
        assert_eq!(
            Value::Object(create_body(&fields)),
            json!({ "name": "Claude", "expiresInDays": 30, "permissions": ["read_logs", "run_flows"] })
        );
        let none = create_body(&parse_fields(b"name=CI&expiresInDays=none"));
        assert_eq!(
            Value::Object(none),
            json!({ "name": "CI", "expiresInDays": null, "permissions": [] })
        );
        // What is not a number of days is the API's to refuse.
        assert_eq!(expiration_value("soon"), json!("soon"));
        assert_eq!(expiration_value("-3"), json!(-3));
        // Without a choice, the API's default.
        assert!(!create_body(&[]).contains_key("expiresInDays"));
    }

    #[test]
    fn says_a_refusal_under_its_field_or_in_a_toast() {
        let invalid = refusal(TokenRefusal::Invalid {
            field: "name",
            message: "name is required".to_string(),
        });
        let public = invalid.downcast_ref::<AdminPublicError>().unwrap();
        assert_eq!(public.status, 422);
        assert_eq!(public.field_errors.as_ref().unwrap()["name"], "Token name is required.");
        let revoked = refusal(TokenRefusal::Revoked("rotate"));
        let public = revoked.downcast_ref::<AdminPublicError>().unwrap();
        assert_eq!((public.status, public.message.as_str()), (409, "Cannot rotate a revoked token"));
        assert!(public.field_errors.is_none());
        let missing = refusal(TokenRefusal::NotFound);
        assert_eq!(missing.downcast_ref::<AdminPublicError>().unwrap().status, 404);
    }
}
