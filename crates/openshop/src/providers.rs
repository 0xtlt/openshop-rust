//! The configuration a shop saved for a provider, read and written by the
//! code of the application: what the built-in providers page does, from a
//! page of the application, a flow or any other context of a shop.
//!
//! [`ProviderStore`] is what `Warehouse::config_of(&ctx)` returns, for a
//! provider declared with [`#[provider]`](crate::provider). It validates with
//! the rules of the `#[field]`s and encrypts as the built-in page does, which
//! the application can then disable (`builtin_pages`): the providers stay, and
//! only the application's own pages manage them.

use std::marker::PhantomData;

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::app::OpenShop;
use crate::server::api::providers::{
    BlankPassword, CHECK_TIMEOUT, CheckOutcome, ProviderOwner, SaveOutcome, check_provider_connection,
    remove_provider_config, save_provider_config, stored_config,
};
use crate::server::connectors::Connectors;
use crate::server::provider_config::{ConfigError, provider_config_is_complete, public_provider_config};
use crate::types::{FieldType, JsonMap, Provider, ProviderConfig, ShopScope};

/// The configuration of the provider `P` for one shop: `Warehouse::config_of(&ctx)`.
///
/// [`status`](Self::status) names fields as in the struct (`api_url`), whatever
/// key they are saved under. A form names them by that key (`apiUrl`), as an
/// `#[input]` does: [`save_form`](Self::save_form) takes either, and
/// [`Saved::Refused`] names them by their key.
pub struct ProviderStore<P> {
    app: OpenShop,
    shop: String,
    shopify_app: String,
    name: &'static str,
    /// Forget what they loaded when the configuration changes.
    connectors: Option<Connectors>,
    provider: PhantomData<fn() -> P>,
}

/// What became of a configuration given to [`ProviderStore::save`] or
/// [`ProviderStore::save_form`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub enum Saved {
    Saved,
    /// Nothing is saved. `fields` holds each field refused, by the key it is
    /// saved under (`apiKey`, the name an `#[input]` gives it), with what a
    /// form shows under it: what
    /// [`Outcome::invalid`](crate::admin::Outcome::invalid) takes.
    Refused {
        /// The first refusal, as a sentence, which names the field as `fields`
        /// does.
        message: String,
        fields: Vec<(String, String)>,
    },
}

/// What the `#[checker]` of a provider answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// It answered `true`.
    Ok,
    /// It answered `false`: the service refused the configuration.
    Refused,
    /// It did not answer: it failed, panicked, or took longer than the 10
    /// seconds a check may take. What a merchant may read of it. The
    /// result of the last check is kept.
    Failed(String),
}

/// What a shop saved for a provider, without its passwords.
#[derive(Debug, Clone, Default, PartialEq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStatus {
    /// A configuration is saved.
    pub saved: bool,
    /// Every required field has a value.
    pub configured: bool,
    /// The values saved, by field name; a password never is.
    pub values: JsonMap,
    /// The password fields that have a value saved.
    pub saved_secrets: Vec<String>,
    pub last_checked_at: Option<DateTime<Utc>>,
    /// What the last check answered.
    pub last_check_ok: Option<bool>,
}

impl ProviderStatus {
    /// The value saved for a field that is not a password, as text: a number
    /// or a boolean is written out.
    pub fn value(&self, field: &str) -> Option<String> {
        match self.values.get(field)? {
            Value::Null => None,
            Value::String(text) => Some(text.clone()),
            other => Some(other.to_string()),
        }
    }

    /// Whether the password field `field` has a value saved.
    pub fn has_secret(&self, field: &str) -> bool {
        self.saved_secrets.iter().any(|saved| saved == field)
    }
}

impl<P: ProviderConfig> ProviderStore<P> {
    /// What `config_of` of a `#[provider]` block returns.
    #[doc(hidden)]
    pub fn new(scope: &(impl ShopScope + ?Sized), name: &'static str) -> Self {
        Self {
            app: scope.app().clone(),
            shop: scope.shop().to_string(),
            shopify_app: scope.shopify_app().to_string(),
            name,
            connectors: scope.scope_connectors().cloned(),
            provider: PhantomData,
        }
    }

    fn owner(&self) -> ProviderOwner<'_> {
        ProviderOwner {
            shop: &self.shop,
            shopify_app: &self.shopify_app,
        }
    }

    fn provider(&self) -> Result<&Provider> {
        self.app.config().providers.get(self.name).ok_or_else(|| {
            anyhow!(
                "[openshop] The provider \"{}\" is not in the `providers` of `app!`",
                self.name
            )
        })
    }

    /// What the shop saved, without its passwords.
    pub async fn status(&self) -> Result<ProviderStatus> {
        let provider = self.provider()?;
        let Some(stored) = stored_config(&self.app, self.owner(), self.name).await? else {
            return Ok(ProviderStatus::default());
        };
        let config = self.app.crypto().decrypt_config(stored.config.as_ref())?;
        let values = public_provider_config(provider, &config)
            .into_iter()
            .map(|(key, value)| (field_name::<P>(&key).to_string(), value))
            .collect();
        let saved_secrets = provider
            .fields
            .iter()
            .filter(|(key, field)| {
                field.field_type == FieldType::Password && config.get(*key).is_some_and(|value| !is_blank(value))
            })
            .map(|(key, _)| field_name::<P>(key).to_string())
            .collect();
        Ok(ProviderStatus {
            saved: true,
            configured: provider_config_is_complete(provider, &config),
            values,
            saved_secrets,
            last_checked_at: stored.last_checked_at,
            last_check_ok: stored.last_check_ok,
        })
    }

    /// Saves `config` as a whole, passwords included: an empty password is no
    /// password. Validated with the rules of the `#[field]`s, then encrypted.
    pub async fn save(&self, config: P) -> Result<Saved> {
        let config = Value::Object(config.to_config());
        self.save_raw(&config, BlankPassword::IsBlank).await
    }

    /// Saves the values of a form, as the built-in page does: a password left
    /// blank keeps the one saved. `values` is any struct or map that serializes
    /// to them, such as the `#[input]` of an action, each under the key of its
    /// field (`apiKey`) or its name in the struct (`api_key`).
    pub async fn save_form(&self, values: &impl Serialize) -> Result<Saved> {
        let Value::Object(values) = serde_json::to_value(values)? else {
            bail!("[openshop] The values of a provider form are a struct or a map");
        };
        let mut config = JsonMap::new();
        for (name, value) in values {
            let key = field_key::<P>(&name)
                .ok_or_else(|| anyhow!("[openshop] The provider \"{}\" has no field `{name}`", self.name))?;
            config.insert(key.to_string(), value);
        }
        self.save_raw(&Value::Object(config), BlankPassword::KeepsSaved).await
    }

    async fn save_raw(&self, config: &Value, blank_password: BlankPassword) -> Result<Saved> {
        self.provider()?;
        let saved = match save_provider_config(&self.app, self.owner(), self.name, Some(config), blank_password).await?
        {
            SaveOutcome::Saved => Saved::Saved,
            SaveOutcome::NotFound => unreachable!("the provider is registered"),
            // The pipeline names a field by its key, in the message too.
            SaveOutcome::Refused(ConfigError { message, fields }) => Saved::Refused { message, fields },
        };
        if saved == Saved::Saved {
            self.forget_loaded();
        }
        Ok(saved)
    }

    /// Runs the `#[checker]` of the provider with the configuration saved, and
    /// keeps what it answered. Fails when the provider has no checker.
    pub async fn check(&self) -> Result<Check> {
        self.provider()?;
        let outcome = check_provider_connection(&self.app, self.owner(), self.name).await?;
        Ok(match outcome {
            CheckOutcome::Answered(true) => Check::Ok,
            CheckOutcome::Answered(false) => Check::Refused,
            CheckOutcome::TimedOut => {
                Check::Failed(format!("The check timed out after {} seconds", CHECK_TIMEOUT.as_secs()))
            }
            CheckOutcome::Failed(failure) => Check::Failed(failure.message),
            CheckOutcome::NoChecker => bail!("[openshop] The provider \"{}\" has no `#[checker]`", self.name),
            CheckOutcome::NotFound => unreachable!("the provider is registered"),
        })
    }

    /// Removes what the shop saved: whether there was a configuration.
    pub async fn remove(&self) -> Result<bool> {
        self.provider()?;
        let removed = remove_provider_config(&self.app, self.owner(), self.name).await?;
        self.forget_loaded();
        Ok(removed)
    }

    fn forget_loaded(&self) {
        if let Some(connectors) = &self.connectors {
            connectors.forget_loaded();
        }
    }
}

fn is_blank(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.is_empty(),
        _ => false,
    }
}

/// The name in the struct of the field saved under `key`.
fn field_name<P: ProviderConfig>(key: &str) -> &str {
    P::FIELD_KEYS
        .iter()
        .find(|(_, saved_as)| *saved_as == key)
        .map_or(key, |(name, _)| name)
}

/// The key of the field named `name`, by that key or by its name in the
/// struct: the derive refuses a key that is the name of another field.
fn field_key<P: ProviderConfig>(name: &str) -> Option<&'static str> {
    P::FIELD_KEYS
        .iter()
        .find(|(field, key)| *key == name || *field == name)
        .map(|(_, key)| *key)
}
