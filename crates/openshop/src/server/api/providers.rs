use std::panic::AssertUnwindSafe;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use futures::FutureExt;
use serde_json::{Value, json};

use crate::app::OpenShop;
use crate::db::schema::ProviderConfigRow;
use crate::runtime::time::to_iso;
use crate::server::http::{
    ApiResult, ClientError, client_error, json_error, json_object_body, json_response, public_error,
};
use crate::server::provider_config::{
    ConfigError, parse_provider_config, provider_config_is_complete, provider_fields_for_response,
    public_provider_config,
};
use crate::server::shop::ShopSession;
use crate::types::{JsonMap, Provider};

/// The shop a provider configuration belongs to, and the Shopify app that
/// serves it: a session of the admin, or any context of a shop.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProviderOwner<'a> {
    pub(crate) shop: &'a str,
    pub(crate) shopify_app: &'a str,
}

impl<'a> From<&'a ShopSession> for ProviderOwner<'a> {
    fn from(session: &'a ShopSession) -> Self {
        Self {
            shop: &session.shop,
            shopify_app: &session.shopify_app,
        }
    }
}

pub(crate) async fn stored_config(
    app: &OpenShop,
    owner: ProviderOwner<'_>,
    provider_name: &str,
) -> Result<Option<ProviderConfigRow>, sqlx::Error> {
    sqlx::query_as("SELECT * FROM provider_configs WHERE app_handle = $1 AND shop = $2 AND provider_name = $3 LIMIT 1")
        .bind(owner.shopify_app)
        .bind(owner.shop)
        .bind(provider_name)
        .fetch_optional(app.db())
        .await
}

/// Removes what the shop saved for the provider `name`: whether there was a
/// configuration.
pub(crate) async fn remove_provider_config(
    app: &OpenShop,
    owner: ProviderOwner<'_>,
    name: &str,
) -> anyhow::Result<bool> {
    let removed =
        sqlx::query("DELETE FROM provider_configs WHERE app_handle = $1 AND shop = $2 AND provider_name = $3")
            .bind(owner.shopify_app)
            .bind(owner.shop)
            .bind(name)
            .execute(app.db())
            .await?;
    Ok(removed.rows_affected() > 0)
}

/// A provider of the application and what the session's shop saved for it,
/// as the admin shows it: never the value of a password.
pub(crate) struct ProviderItem<'a> {
    pub(crate) name: &'a str,
    pub(crate) provider: &'a Provider,
    /// The metadata of each field; a password says only whether a value is
    /// saved (`hasValue`).
    pub(crate) fields: JsonMap,
    /// The values saved, passwords left out.
    pub(crate) config: JsonMap,
    /// Every required field has a value saved.
    pub(crate) configured: bool,
    pub(crate) last_checked_at: Option<String>,
    pub(crate) last_check_ok: Option<bool>,
}

impl ProviderItem<'_> {
    /// Whether the field `name`, a password, has a value saved, which is never shown.
    pub(crate) fn has_saved_value(&self, name: &str) -> bool {
        self.fields
            .get(name)
            .and_then(|field| field.get("hasValue"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// What `GET /api/providers` answers for it.
    fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "fields": self.fields,
            "config": self.config,
            "configured": self.configured,
            "lastCheckedAt": self.last_checked_at,
            "lastCheckOk": self.last_check_ok,
        })
    }
}

/// The providers of the application, in the order of the configuration, with
/// what the session's shop saved for each.
pub(crate) async fn provider_items<'a>(
    app: &'a OpenShop,
    session: &ShopSession,
) -> anyhow::Result<Vec<ProviderItem<'a>>> {
    let stored_rows: Vec<ProviderConfigRow> =
        sqlx::query_as("SELECT * FROM provider_configs WHERE app_handle = $1 AND shop = $2")
            .bind(&session.shopify_app)
            .bind(&session.shop)
            .fetch_all(app.db())
            .await?;

    let mut providers = Vec::new();
    for (name, provider) in &app.config().providers {
        let stored = stored_rows.iter().find(|row| row.provider_name == *name);
        let config = app
            .crypto()
            .decrypt_config(stored.and_then(|row| row.config.as_ref()))?;

        providers.push(ProviderItem {
            name,
            provider,
            fields: provider_fields_for_response(provider, &config),
            config: public_provider_config(provider, &config),
            configured: provider_config_is_complete(provider, &config),
            last_checked_at: stored.and_then(|row| row.last_checked_at.as_ref().map(to_iso)),
            last_check_ok: stored.and_then(|row| row.last_check_ok),
        });
    }
    Ok(providers)
}

async fn list_providers(State(app): State<OpenShop>, Extension(session): Extension<ShopSession>) -> ApiResult {
    let providers = provider_items(&app, &session).await?;
    Ok(json_response(
        StatusCode::OK,
        Value::Array(providers.iter().map(ProviderItem::to_json).collect()),
    ))
}

/// What became of a configuration sent for a provider.
pub(crate) enum SaveOutcome {
    Saved,
    /// No provider of that name.
    NotFound,
    /// The configuration was refused: nothing is saved.
    Refused(ConfigError),
}

/// What a blank password of a configuration sent for a provider means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlankPassword {
    /// The saved one is kept, as the form of a page sends it.
    KeepsSaved,
    /// No password: the configuration replaces the saved one as a whole.
    IsBlank,
}

/// Saves the configuration `raw_config` of the provider `name` for the shop
/// of `owner`, validated against the one saved. Encrypted at rest when the
/// application has a key.
pub(crate) async fn save_provider_config(
    app: &OpenShop,
    owner: ProviderOwner<'_>,
    name: &str,
    raw_config: Option<&Value>,
    blank_password: BlankPassword,
) -> anyhow::Result<SaveOutcome> {
    let Some(provider) = app.config().providers.get(name) else {
        return Ok(SaveOutcome::NotFound);
    };

    let existing_config = match blank_password {
        BlankPassword::KeepsSaved => {
            let existing = stored_config(app, owner, name).await?;
            app.crypto()
                .decrypt_config(existing.as_ref().and_then(|row| row.config.as_ref()))?
        }
        BlankPassword::IsBlank => JsonMap::new(),
    };
    let parsed = match parse_provider_config(
        &provider.fields,
        provider.transformer.as_ref(),
        raw_config,
        &existing_config,
    ) {
        Ok(parsed) => parsed,
        Err(error) => return Ok(SaveOutcome::Refused(error)),
    };

    let encrypted = app.crypto().encrypt_config(&parsed)?;
    sqlx::query(
        "INSERT INTO provider_configs (app_handle, shop, provider_name, config) VALUES ($1, $2, $3, $4::json) \
         ON CONFLICT (app_handle, shop, provider_name) DO UPDATE SET config = EXCLUDED.config, updated_at = now()",
    )
    .bind(owner.shopify_app)
    .bind(owner.shop)
    .bind(name)
    .bind(encrypted.to_string())
    .execute(app.db())
    .await?;
    Ok(SaveOutcome::Saved)
}

async fn save_provider(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path(name): Path<String>,
    body: Bytes,
) -> ApiResult {
    let body = json_object_body(&body);
    Ok(
        match save_provider_config(
            &app,
            (&session).into(),
            &name,
            body.get("config"),
            BlankPassword::KeepsSaved,
        )
        .await?
        {
            SaveOutcome::Saved => json_response(StatusCode::OK, json!({ "ok": true })),
            SaveOutcome::NotFound => json_error(StatusCode::NOT_FOUND, "Provider not found"),
            SaveOutcome::Refused(error) => json_error(StatusCode::BAD_REQUEST, error),
        },
    )
}

/// What a connection check of a provider found.
pub(crate) enum CheckOutcome {
    /// No provider of that name.
    NotFound,
    /// The provider has no checker.
    NoChecker,
    /// What the checker answered, stored for the saved configuration.
    Answered(bool),
    /// The checker took longer than [`CHECK_TIMEOUT`]; the stored result is untouched.
    TimedOut,
    /// The checker failed: what the client may read of it. The stored result
    /// is untouched.
    Failed(ClientError),
}

impl CheckOutcome {
    /// What the merchant reads of a check that gave no answer, `Error: …`.
    pub(crate) fn error(&self) -> Option<String> {
        match self {
            Self::TimedOut => Some(format!(
                "Error: the check timed out after {} seconds",
                CHECK_TIMEOUT.as_secs()
            )),
            Self::Failed(failure) => Some(format!("Error: {}", failure.message)),
            Self::NotFound | Self::NoChecker | Self::Answered(_) => None,
        }
    }
}

/// Runs the checker of the provider `name` with the configuration the shop
/// of `owner` saved, for [`CHECK_TIMEOUT`] at most, and stores what it
/// answered when a configuration is saved. A failure of the checker is
/// logged under an id unless it was written for the client.
pub(crate) async fn check_provider_connection(
    app: &OpenShop,
    owner: ProviderOwner<'_>,
    name: &str,
) -> anyhow::Result<CheckOutcome> {
    let Some(provider) = app.config().providers.get(name) else {
        return Ok(CheckOutcome::NotFound);
    };
    let Some(checker) = &provider.checker else {
        return Ok(CheckOutcome::NoChecker);
    };
    let stored = stored_config(app, owner, name).await?;

    let check = async {
        let config = app
            .crypto()
            .decrypt_config(stored.as_ref().and_then(|row| row.config.as_ref()))?;
        let ok = AssertUnwindSafe(checker(config))
            .catch_unwind()
            .await
            .unwrap_or_else(|_| Err(public_error("checker panicked")))?;

        if let Some(stored) = &stored {
            sqlx::query("UPDATE provider_configs SET last_checked_at = now(), last_check_ok = $1 WHERE id = $2")
                .bind(ok)
                .bind(stored.id)
                .execute(app.db())
                .await?;
        }
        anyhow::Ok(ok)
    };

    Ok(match tokio::time::timeout(CHECK_TIMEOUT, check).await {
        Ok(Ok(ok)) => CheckOutcome::Answered(ok),
        Err(_) => CheckOutcome::TimedOut,
        Ok(Err(error)) => CheckOutcome::Failed(client_error(&format!("Check of provider \"{name}\" failed"), &error)),
    })
}

async fn check_provider(
    State(app): State<OpenShop>,
    Extension(session): Extension<ShopSession>,
    Path(name): Path<String>,
) -> ApiResult {
    let outcome = check_provider_connection(&app, (&session).into(), &name).await?;
    let error = outcome.error();
    Ok(match outcome {
        CheckOutcome::NotFound => json_error(StatusCode::NOT_FOUND, "Provider not found"),
        CheckOutcome::NoChecker => json_error(StatusCode::BAD_REQUEST, "No checker defined"),
        CheckOutcome::Answered(ok) => json_response(StatusCode::OK, json!({ "ok": ok })),
        CheckOutcome::TimedOut => json_response(StatusCode::GATEWAY_TIMEOUT, json!({ "ok": false, "error": error })),
        CheckOutcome::Failed(failure) => {
            let mut body = json!({ "ok": false, "error": error });
            if let Some(id) = failure.error_id {
                body["errorId"] = json!(id);
            }
            json_response(StatusCode::INTERNAL_SERVER_ERROR, body)
        }
    })
}

/// The longest a connection check may take: the admin waits for the answer.
pub(crate) const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

pub fn routes() -> Router<OpenShop> {
    Router::new()
        .route("/api/providers", get(list_providers))
        .route("/api/providers/{name}", put(save_provider))
        .route("/api/providers/{name}/check", post(check_provider))
}
