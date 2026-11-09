//! Shopify's Partner API: where an app on Shopify App Pricing reads the plan
//! of a shop. One client per Partner organization, not per shop.
//!
//! - Endpoint: `POST https://partners.shopify.com/{partner_id}/api/2026-10/graphql.json`,
//!   the origin being the `partner_origin` of the
//!   [Shopify endpoints](crate::ShopifyEndpoints::partner_origin).
//! - Authentication: `X-Shopify-Access-Token` with the token of a Partner API
//!   client of the organization.
//! - Rate limit: four requests a second per Partner API client, for every
//!   shop and every process together.
//!
//! See <https://shopify.dev/docs/api/partner>.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde::Serialize;
use serde_json::{Value, json};

use crate::runtime::Transport;
use crate::runtime::transport::{HttpRequest, Method};

/// The variable that sets [`PartnerApi::partner_id`] when the code does not.
pub const PARTNER_ID_VAR: &str = "SHOPIFY_PARTNER_ID";
/// The variable that sets [`PartnerApi::token`] when the code does not.
pub const PARTNER_API_TOKEN_VAR: &str = "SHOPIFY_PARTNER_API_TOKEN";
/// The version of the Partner API OpenShop sends its requests to.
pub const PARTNER_API_VERSION: &str = "2026-10";
/// How long the plan of a shop read from the Partner API is kept, unless
/// [`PartnerApi::plan_ttl`] says otherwise.
pub const DEFAULT_PLAN_TTL: Duration = Duration::from_secs(5 * 60);

/// The Partner API of the organization that owns the app: what billing reads
/// the plans from, in place of the Admin API, for an app on Shopify App
/// Pricing. Set both the Partner ID and the token, or neither.
///
/// Each field comes from a call at startup (on the builder of
/// [`App::main_with`](crate::App::main_with)), else from the `partner_api` of
/// [`app!`](crate::app!), else from its variable, `SHOPIFY_PARTNER_ID` or
/// `SHOPIFY_PARTNER_API_TOKEN`. The token is a secret: `Debug` hides it, and
/// no error or log line shows it.
///
/// ```
/// use std::time::Duration;
/// use openshop::PartnerApi;
///
/// // The Partner ID in the code, the token from `SHOPIFY_PARTNER_API_TOKEN`.
/// let partner = PartnerApi::new().partner_id("1234567").plan_ttl(Duration::from_secs(60));
/// assert!(!format!("{:?}", PartnerApi::new().token("prtapi_secret")).contains("prtapi"));
/// # let _ = partner;
/// ```
#[derive(Clone, Default, PartialEq, Eq)]
pub struct PartnerApi {
    /// The number in the URL of the Partner Dashboard
    /// (`partners.shopify.com/1234567/…`), not the organization ID of the Dev
    /// Dashboard.
    pub partner_id: Option<String>,
    /// The access token of a Partner API client of the organization, with
    /// the "Manage apps" permission.
    pub token: Option<String>,
    /// How long the plan of a shop read from the Partner API is kept:
    /// [`DEFAULT_PLAN_TTL`] when `None`. `Duration::ZERO` keeps nothing.
    pub plan_ttl: Option<Duration>,
}

impl std::fmt::Debug for PartnerApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PartnerApi")
            .field("partner_id", &self.partner_id)
            .field("token", &crate::runtime::redact::redacted_option(&self.token))
            .field("plan_ttl", &self.plan_ttl)
            .finish()
    }
}

impl PartnerApi {
    /// No field set.
    pub fn new() -> Self {
        Self::default()
    }

    /// The number in the URL of the Partner Dashboard, `1234567`.
    pub fn partner_id(mut self, partner_id: impl Into<String>) -> Self {
        self.partner_id = Some(partner_id.into());
        self
    }

    /// The access token of a Partner API client of the organization.
    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// How long the plan of a shop read from the Partner API is kept. See
    /// [`DEFAULT_PLAN_TTL`].
    pub fn plan_ttl(mut self, ttl: Duration) -> Self {
        self.plan_ttl = Some(ttl);
        self
    }

    /// Each field of `self`, or of `fallback` where `self` sets none.
    pub fn or(self, fallback: Self) -> Self {
        Self {
            partner_id: self.partner_id.or(fallback.partner_id),
            token: self.token.or(fallback.token),
            plan_ttl: self.plan_ttl.or(fallback.plan_ttl),
        }
    }

    /// The fields the environment sets: `SHOPIFY_PARTNER_ID` and
    /// `SHOPIFY_PARTNER_API_TOKEN`. An empty value is not set.
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok().filter(|value| !value.trim().is_empty());
        Self {
            partner_id: var(PARTNER_ID_VAR),
            token: var(PARTNER_API_TOKEN_VAR),
            plan_ttl: None,
        }
    }

    /// Checks the fields the code sets, as `build()` does. Whether both are
    /// set is checked at startup, once the environment is read.
    pub(crate) fn check(&self) -> Result<()> {
        if let Some(partner_id) = &self.partner_id {
            check_partner_id(partner_id).map_err(|reason| Source::Code("partner_id").invalid(reason))?;
        }
        if let Some(token) = &self.token {
            check_token(token).map_err(|reason| Source::Code("token").invalid(reason))?;
        }
        Ok(())
    }
}

/// Where a field comes from: the code, or a variable of the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Code(&'static str),
    Env(&'static str),
}

impl Source {
    fn name(self) -> String {
        match self {
            Self::Code(field) => format!("partner_api.{field}"),
            Self::Env(var) => var.to_string(),
        }
    }

    fn pick<'a>(
        code: &'a Option<String>,
        env: &'a Option<String>,
        field: &'static str,
        var: &'static str,
    ) -> Option<(&'a str, Self)> {
        match (code, env) {
            (Some(value), _) => Some((value, Self::Code(field))),
            (None, Some(value)) => Some((value, Self::Env(var))),
            (None, None) => None,
        }
    }

    /// The error of a value that is not valid. It never shows the value: a
    /// token set in the place of the Partner ID would be printed.
    fn invalid(self, reason: &str) -> anyhow::Error {
        match self {
            Self::Code(_) => anyhow!("[openshop] Invalid config: {} is not valid: {reason}", self.name()),
            Self::Env(_) => anyhow!("[openshop] {} is not valid: {reason}", self.name()),
        }
    }
}

fn check_partner_id(partner_id: &str) -> Result<(), &'static str> {
    let partner_id = partner_id.trim();
    if partner_id.is_empty() || !partner_id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("expected the number in the URL of the Partner Dashboard, such as 1234567");
    }
    Ok(())
}

fn check_token(token: &str) -> Result<(), &'static str> {
    let token = token.trim();
    if token.is_empty() {
        return Err("it is empty");
    }
    if !token.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err("expected the access token of a Partner API client, without spaces");
    }
    Ok(())
}

/// The Partner API of a running application, checked: a Partner ID and a
/// token, each from the code or the environment, and the origin of the
/// Shopify endpoints. Cheap to clone.
///
/// [`OpenShop::partner_api`](crate::OpenShop::partner_api) gives it. Billing
/// reads the plans with it; [`graphql`](Self::graphql) sends any other query
/// of the Partner API, such as `events`. `Debug` does not show the token.
#[derive(Clone)]
pub struct PartnerApiClient {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    partner_id: String,
    token: String,
    url: String,
    plan_ttl: Duration,
    transport: Arc<dyn Transport>,
    retry: shopify_api::RetryPolicy,
    /// What the startup line says of where the Partner ID and the token come from.
    sources: (Source, Source),
}

impl std::fmt::Debug for PartnerApiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PartnerApiClient")
            .field("partner_id", &self.inner.partner_id)
            .field("token", &crate::runtime::redact::redacted(&self.inner.token))
            .field("url", &self.inner.url)
            .field("plan_ttl", &self.inner.plan_ttl)
            .finish()
    }
}

impl PartnerApiClient {
    /// The Partner API of `code`, else of `env`, field by field, at `origin`.
    /// `None` when neither sets a Partner ID or a token: billing reads the
    /// Admin API. Fails when one is set and not the other, or when one is
    /// not valid, naming the setting or the variable, never the value.
    pub(crate) fn resolve(
        code: &PartnerApi,
        env: &PartnerApi,
        origin: &str,
        transport: Arc<dyn Transport>,
        retry: shopify_api::RetryPolicy,
    ) -> Result<Option<Self>> {
        let partner_id = Source::pick(&code.partner_id, &env.partner_id, "partner_id", PARTNER_ID_VAR);
        let token = Source::pick(&code.token, &env.token, "token", PARTNER_API_TOKEN_VAR);
        let ((partner_id, id_source), (token, token_source)) = match (partner_id, token) {
            (None, None) => return Ok(None),
            (Some(partner_id), Some(token)) => (partner_id, token),
            (Some((_, source)), None) => bail!(
                "[openshop] Invalid config: the Partner API has a Partner ID ({}) but no token: set \
                 {PARTNER_API_TOKEN_VAR} or partner_api.token, or remove the Partner ID to read the plans \
                 from the Admin API",
                source.name()
            ),
            (None, Some((_, source))) => bail!(
                "[openshop] Invalid config: the Partner API has a token ({}) but no Partner ID: set \
                 {PARTNER_ID_VAR} or partner_api.partner_id, or remove the token to read the plans from \
                 the Admin API",
                source.name()
            ),
        };
        check_partner_id(partner_id).map_err(|reason| id_source.invalid(reason))?;
        check_token(token).map_err(|reason| token_source.invalid(reason))?;
        let partner_id = partner_id.trim().to_string();
        Ok(Some(Self {
            inner: Arc::new(ClientInner {
                url: format!(
                    "{}/{partner_id}/api/{PARTNER_API_VERSION}/graphql.json",
                    origin.trim_end_matches('/')
                ),
                partner_id,
                token: token.trim().to_string(),
                plan_ttl: code.plan_ttl.or(env.plan_ttl).unwrap_or(DEFAULT_PLAN_TTL),
                transport,
                retry,
                sources: (id_source, token_source),
            }),
        }))
    }

    /// The number in the URL of the Partner Dashboard.
    pub fn partner_id(&self) -> &str {
        &self.inner.partner_id
    }

    /// Where the requests go: `{origin}/{partner_id}/api/2026-10/graphql.json`.
    pub fn url(&self) -> &str {
        &self.inner.url
    }

    /// How long the plan of a shop is kept.
    pub fn plan_ttl(&self) -> Duration {
        self.inner.plan_ttl
    }

    /// The line the application logs at startup: where the plans are read,
    /// and where the Partner ID and the token come from. Not the token.
    pub(crate) fn startup_line(&self) -> String {
        let (id_source, token_source) = self.inner.sources;
        format!(
            "[openshop] Billing reads the plans from the Partner API: {} ({}, token from {}), each plan kept {}s per shop",
            self.inner.url,
            id_source.name(),
            token_source.name(),
            self.inner.plan_ttl.as_secs()
        )
    }

    /// Runs a query of the Partner API and returns its `data`.
    ///
    /// A `429` (four requests a second per Partner API client), a `5xx` or a
    /// network failure is sent again as the `shopify_retry` of the
    /// application says: at most its number of retries, at most its longest
    /// wait apart, after the `Retry-After` Shopify gives or a backoff from
    /// one second. A mutation is sent again only after a `429`, which Shopify
    /// did not run. Fails when Shopify answers with `errors`, without `data`,
    /// or with another status: the error gives Shopify's messages, never the
    /// token.
    pub async fn graphql(&self, query: &str, variables: impl Serialize) -> Result<Value> {
        let body = serde_json::to_vec(&json!({ "query": query, "variables": serde_json::to_value(variables)? }))?;
        let mutation = is_mutation(query);
        let retry = &self.inner.retry;
        let mut attempt = 0u32;
        loop {
            let request = HttpRequest::new(Method::Post, &self.inner.url)
                .with_header("Content-Type", "application/json")
                .with_header("Accept", "application/json")
                .with_header("X-Shopify-Access-Token", &self.inner.token)
                .with_body(body.clone());
            let (retriable, failure, retry_after) = match self.inner.transport.send(request).await {
                Ok(response) if response.is_success() => return read_data(&response.text()),
                Ok(response) => {
                    let retry_after = response
                        .header("retry-after")
                        .and_then(|value| value.trim().parse::<u64>().ok())
                        .map(Duration::from_secs);
                    let retriable = response.status == 429 || (response.status >= 500 && !mutation);
                    (retriable, status_error(response.status, &response.text()), retry_after)
                }
                Err(error) => (
                    !mutation,
                    anyhow!("[openshop] The Partner API could not be reached: {error}"),
                    None,
                ),
            };
            if !retriable || attempt >= retry.max_retries() {
                return Err(failure);
            }
            let backoff = Duration::from_secs(1).saturating_mul(1u32 << attempt.min(16));
            tokio::time::sleep(retry_after.unwrap_or(backoff).min(retry.max_delay())).await;
            attempt += 1;
        }
    }
}

/// Whether the GraphQL document is a mutation: its first word, past white
/// space and comments.
fn is_mutation(query: &str) -> bool {
    let mut rest = query;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == ',' || c == '\u{feff}');
        match rest.strip_prefix('#') {
            Some(comment) => rest = comment.find('\n').map_or("", |end| &comment[end..]),
            None => return rest.starts_with("mutation"),
        }
    }
}

/// The messages of the `errors` of a GraphQL answer, joined.
fn error_messages(answer: &Value) -> Option<String> {
    let messages: Vec<&str> = answer
        .get("errors")?
        .as_array()?
        .iter()
        .filter_map(|error| error.get("message").and_then(Value::as_str))
        .collect();
    (!messages.is_empty()).then(|| messages.join("; "))
}

/// The `data` of a `2xx` answer, or the error of its `errors`.
fn read_data(body: &str) -> Result<Value> {
    let answer: Value = serde_json::from_str(body)
        .map_err(|_| anyhow!("[openshop] The Partner API answered with a body that is not JSON"))?;
    if let Some(messages) = error_messages(&answer) {
        bail!("[openshop] The Partner API answered with errors: {messages}");
    }
    match answer.get("data") {
        Some(data) if !data.is_null() => Ok(data.clone()),
        _ => bail!("[openshop] The Partner API answered without data"),
    }
}

/// The error of an answer whose status is not `2xx`.
fn status_error(status: u16, body: &str) -> anyhow::Error {
    let messages = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|answer| error_messages(&answer))
        .map(|messages| format!(": {messages}"))
        .unwrap_or_default();
    match status {
        401 | 403 => anyhow!(
            "[openshop] The Partner API refused the request ({status}{messages}): check the Partner ID and \
             that the token is the one of a Partner API client of that organization, with the \"Manage apps\" \
             permission"
        ),
        429 => anyhow!(
            "[openshop] The Partner API throttled the request (429{messages}): it takes four requests a \
             second per Partner API client"
        ),
        _ => anyhow!("[openshop] The Partner API answered with status {status}{messages}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(code: &PartnerApi, env: &PartnerApi) -> Result<Option<PartnerApiClient>> {
        PartnerApiClient::resolve(
            code,
            env,
            "https://partners.shopify.com",
            Arc::new(crate::runtime::ReqwestTransport::new().unwrap()),
            shopify_api::RetryPolicy::default(),
        )
    }

    #[test]
    fn nothing_set_reads_the_admin_api() {
        assert!(resolve(&PartnerApi::new(), &PartnerApi::new()).unwrap().is_none());
        // A cache duration alone sets nothing.
        assert!(
            resolve(&PartnerApi::new().plan_ttl(Duration::from_secs(1)), &PartnerApi::new())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn each_field_comes_from_the_code_else_from_the_environment() {
        let env = PartnerApi::new().partner_id("1111111").token("prtapi_from_env");
        let client = resolve(&PartnerApi::new().partner_id(" 7438846 "), &env)
            .unwrap()
            .unwrap();
        assert_eq!(client.partner_id(), "7438846");
        assert_eq!(
            client.url(),
            "https://partners.shopify.com/7438846/api/2026-10/graphql.json"
        );
        assert_eq!(client.plan_ttl(), DEFAULT_PLAN_TTL);
        assert_eq!(client.inner.token, "prtapi_from_env");
        let line = client.startup_line();
        assert_eq!(
            line,
            "[openshop] Billing reads the plans from the Partner API: \
             https://partners.shopify.com/7438846/api/2026-10/graphql.json (partner_api.partner_id, token from \
             SHOPIFY_PARTNER_API_TOKEN), each plan kept 300s per shop"
        );
        assert!(!format!("{client:?}").contains("prtapi_from_env"));
        assert!(!format!("{env:?}").contains("prtapi_from_env"));

        let ttl = resolve(&PartnerApi::new().plan_ttl(Duration::ZERO), &env)
            .unwrap()
            .unwrap();
        assert_eq!(ttl.plan_ttl(), Duration::ZERO);
        assert_eq!(
            PartnerApi::new().token("a").or(env.clone()),
            PartnerApi::new().partner_id("1111111").token("a")
        );
    }

    #[test]
    fn both_are_needed_and_no_error_shows_the_token() {
        let error = |code: PartnerApi, env: PartnerApi| resolve(&code, &env).unwrap_err().to_string();
        assert_eq!(
            error(PartnerApi::new().partner_id("1234567"), PartnerApi::new()),
            "[openshop] Invalid config: the Partner API has a Partner ID (partner_api.partner_id) but no token: \
             set SHOPIFY_PARTNER_API_TOKEN or partner_api.token, or remove the Partner ID to read the plans \
             from the Admin API"
        );
        let missing_id = error(PartnerApi::new(), PartnerApi::new().token("prtapi_hunter2"));
        assert!(
            missing_id.contains("has a token (SHOPIFY_PARTNER_API_TOKEN) but no Partner ID"),
            "{missing_id}"
        );
        assert!(!missing_id.contains("hunter2"));

        // The token in the place of the Partner ID is not printed.
        let swapped = error(
            PartnerApi::new(),
            PartnerApi::new().partner_id("prtapi_hunter2").token("prtapi_x"),
        );
        assert_eq!(
            swapped,
            "[openshop] SHOPIFY_PARTNER_ID is not valid: expected the number in the URL of the Partner \
             Dashboard, such as 1234567"
        );
        let spaced = error(
            PartnerApi::new().partner_id("1").token("prtapi hunter2"),
            PartnerApi::new(),
        );
        assert_eq!(
            spaced,
            "[openshop] Invalid config: partner_api.token is not valid: expected the access token of a Partner \
             API client, without spaces"
        );
        assert!(PartnerApi::new().partner_id("12a").check().is_err());
        assert!(PartnerApi::new().partner_id("12").token("prtapi_x").check().is_ok());
    }

    #[test]
    fn a_mutation_is_named_by_its_first_word() {
        assert!(is_mutation("mutation Credit { appCreditCreate { id } }"));
        assert!(is_mutation("  # a comment\n mutation { a }"));
        assert!(!is_mutation("query OpenShopPartnerSubscription { a }"));
        assert!(!is_mutation("{ a } # mutation"));
    }

    #[test]
    fn errors_give_shopify_s_messages() {
        assert_eq!(read_data(r#"{"data":{"a":1}}"#).unwrap(), json!({ "a": 1 }));
        assert_eq!(
            read_data(r#"{"data":null,"errors":[{"message":"Shop not found"}]}"#)
                .unwrap_err()
                .to_string(),
            "[openshop] The Partner API answered with errors: Shop not found"
        );
        assert!(read_data("<html>").is_err());
        let refused = status_error(
            401,
            r#"{"errors":[{"message":"Invalid access token","extensions":{"code":"401"}}]}"#,
        )
        .to_string();
        assert!(refused.starts_with("[openshop] The Partner API refused the request (401: Invalid access token)"));
        assert_eq!(
            status_error(502, "Bad gateway").to_string(),
            "[openshop] The Partner API answered with status 502"
        );
    }
}
