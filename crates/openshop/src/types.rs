//! Definitions that make up an OpenShop application: providers, flows, crons,
//! webhooks, Shopify Functions, app proxies, server routes and MCP capabilities.

use std::fmt;
use std::future::Future;
use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use futures::future::BoxFuture;
use indexmap::IndexMap;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use crate::app::OpenShop;
use crate::engine::step::StepExecutor;
use crate::runtime::flow_logger::FlowLogger;
use crate::server::connectors::Connectors;
use crate::shopify::client::ShopifyClient;

pub use openshop_shared::{
    AdminMenuItem, AdminPageId, AdminPageMode, AdminPagesConfig, CustomAdminPageAccess, CustomAdminPagesResponse,
    ResolvedAdminPages,
};

/// A JSON object, the Rust counterpart of `Record<string, unknown>`.
pub type JsonMap = serde_json::Map<String, Value>;

pub(crate) fn is_missing(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::String(text)) => text.is_empty(),
        Some(_) => false,
    }
}

// ─── Provider ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    Text,
    Password,
    Number,
    Select,
    Checkbox,
}

impl FieldType {
    pub const ALL: [FieldType; 5] = [
        FieldType::Text,
        FieldType::Password,
        FieldType::Number,
        FieldType::Select,
        FieldType::Checkbox,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Password => "password",
            Self::Number => "number",
            Self::Select => "select",
            Self::Checkbox => "checkbox",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldOption {
    pub label: String,
    pub value: String,
}

impl FieldOption {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

/// Validates (and may normalize) a field value. The error is a short summary
/// such as `must be a URL`.
pub type FieldValidator = Arc<dyn Fn(&Value) -> Result<Value, String> + Send + Sync>;

/// A configuration field rendered by the admin UI for providers and Shopify Functions.
#[derive(Clone)]
pub struct Field {
    pub field_type: FieldType,
    pub label: String,
    pub placeholder: Option<String>,
    /// Choices for `select` fields.
    pub options: Option<Vec<FieldOption>>,
    /// Fields are required unless this is `Some(false)`.
    pub required: Option<bool>,
    pub validate: Option<FieldValidator>,
}

impl Field {
    pub fn new(field_type: FieldType, label: impl Into<String>) -> Self {
        Self {
            field_type,
            label: label.into(),
            placeholder: None,
            options: None,
            required: None,
            validate: None,
        }
    }

    pub fn text(label: impl Into<String>) -> Self {
        Self::new(FieldType::Text, label)
    }

    pub fn password(label: impl Into<String>) -> Self {
        Self::new(FieldType::Password, label)
    }

    pub fn number(label: impl Into<String>) -> Self {
        Self::new(FieldType::Number, label)
    }

    pub fn checkbox(label: impl Into<String>) -> Self {
        Self::new(FieldType::Checkbox, label)
    }

    pub fn select(label: impl Into<String>, options: impl IntoIterator<Item = FieldOption>) -> Self {
        let mut field = Self::new(FieldType::Select, label);
        field.options = Some(options.into_iter().collect());
        field
    }

    pub fn placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = Some(placeholder.into());
        self
    }

    pub fn required(mut self, required: bool) -> Self {
        self.required = Some(required);
        self
    }

    pub fn optional(self) -> Self {
        self.required(false)
    }

    /// Adds a validator. It runs after the ones already added, on the value
    /// they return.
    pub fn validate(mut self, validate: impl Fn(&Value) -> Result<Value, String> + Send + Sync + 'static) -> Self {
        self.validate = Some(match self.validate.take() {
            Some(first) => Arc::new(move |value| validate(&first(value)?)),
            None => Arc::new(validate),
        });
        self
    }

    pub fn is_required(&self) -> bool {
        self.required != Some(false)
    }

    /// Public metadata: everything except the validator.
    pub fn metadata(&self) -> JsonMap {
        let mut metadata = JsonMap::new();
        metadata.insert("type".into(), Value::from(self.field_type.as_str()));
        metadata.insert("label".into(), Value::from(self.label.clone()));
        if let Some(placeholder) = &self.placeholder {
            metadata.insert("placeholder".into(), Value::from(placeholder.clone()));
        }
        if let Some(options) = &self.options {
            metadata.insert("options".into(), serde_json::to_value(options).unwrap_or(Value::Null));
        }
        if let Some(required) = self.required {
            metadata.insert("required".into(), Value::from(required));
        }
        metadata
    }
}

impl std::fmt::Debug for Field {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Field")
            .field("field_type", &self.field_type)
            .field("label", &self.label)
            .field("placeholder", &self.placeholder)
            .field("options", &self.options)
            .field("required", &self.required)
            .field("validate", &self.validate.as_ref().map(|_| "…"))
            .finish()
    }
}

pub type Fields = IndexMap<String, Field>;

/// A provider method receives the shop's stored configuration and the call arguments.
pub type ProviderMethod = Arc<dyn Fn(JsonMap, Value) -> BoxFuture<'static, Result<Value>> + Send + Sync>;
pub type ProviderChecker = Arc<dyn Fn(JsonMap) -> BoxFuture<'static, Result<bool>> + Send + Sync>;
/// Reshapes submitted provider data before validation. Must return a JSON object.
pub type ProviderTransformer = Arc<dyn Fn(JsonMap) -> Value + Send + Sync>;

/// A typed reference to a provider method: the provider it belongs to, its
/// name, and the types of its arguments (`A`) and of its result (`R`).
///
/// A `#[provider]` block declares one per method, `Warehouse::PUSH`: it is
/// what [`Provider::handle`] implements, and what names the fake of the
/// method in a test, `ctx.fakes.for_call(Warehouse::PUSH)`. The names are
/// then written in a single place, and the compiler checks the arguments and
/// the result of every call.
///
/// ```
/// use openshop::builder::ProviderCall;
///
/// const PUSH: ProviderCall<Vec<String>, usize> = ProviderCall::new("warehouse", "push");
/// assert_eq!(PUSH.provider(), "warehouse");
/// assert_eq!(PUSH.method(), "push");
/// ```
///
/// `provider` is the key the provider is registered under: its own name with
/// `.provider(..)`, or the key given to `.provider_as(..)`. Building the
/// configuration fails when they differ.
pub struct ProviderCall<A, R> {
    provider: &'static str,
    method: &'static str,
    types: PhantomData<fn(A) -> R>,
}

impl<A, R> ProviderCall<A, R> {
    pub const fn new(provider: &'static str, method: &'static str) -> Self {
        Self {
            provider,
            method,
            types: PhantomData,
        }
    }

    pub const fn provider(&self) -> &'static str {
        self.provider
    }

    pub const fn method(&self) -> &'static str {
        self.method
    }
}

impl<A, R> Clone for ProviderCall<A, R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<A, R> Copy for ProviderCall<A, R> {}

impl<A, R> fmt::Debug for ProviderCall<A, R> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderCall")
            .field("provider", &self.provider)
            .field("method", &self.method)
            .finish()
    }
}

/// The saved configuration of a provider as a struct: its fields are the
/// fields of the configuration form. Implemented by `#[derive(ProviderConfig)]`.
pub trait ProviderConfig: Sized {
    /// The fields of the form, in the order of the struct.
    fn fields() -> Fields;

    /// Reads a saved configuration. The error names the field that does not fit.
    fn from_config(config: JsonMap) -> std::result::Result<Self, String>;
}

/// A Rust type a field of a provider configuration can have.
///
/// | Rust | Field |
/// | --- | --- |
/// | `String` | `text`; `password` or `select` when the field says so |
/// | `bool` | `checkbox` |
/// | Integers and floats | `number` |
/// | `Option<T>` | The field of `T`, optional |
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a type a provider field can have",
    note = "use `String`, `bool`, a number type, or an `Option` of them"
)]
pub trait ProviderField: DeserializeOwned {
    /// Whether the form accepts the field left blank.
    const OPTIONAL: bool = false;

    /// The field of the form for a value of this type.
    fn field(label: &str) -> Field;
}

impl ProviderField for String {
    fn field(label: &str) -> Field {
        Field::text(label)
    }
}

impl ProviderField for bool {
    fn field(label: &str) -> Field {
        Field::checkbox(label)
    }
}

macro_rules! number_field {
    ($($number:ty),+ => $field:expr) => {$(
        impl ProviderField for $number {
            fn field(label: &str) -> Field {
                let field: fn(&str) -> Field = $field;
                field(label)
            }
        }
    )+};
}

// The form refuses what the type cannot hold when the value is saved: a
// fraction for an integer, a negative number for an unsigned one.
number_field!(i8, i16, i32, i64, isize => |label| Field::number(label).validate(crate::validators::integer()));
number_field!(u8, u16, u32, u64, usize => |label| Field::number(label).validate(crate::validators::integer_min(0)));
number_field!(f32, f64 => |label| Field::number(label));

impl<T: ProviderField> ProviderField for Option<T> {
    const OPTIONAL: bool = true;

    fn field(label: &str) -> Field {
        T::field(label).optional()
    }
}

/// Takes the value of a field out of a saved configuration. Used by the code
/// `#[derive(ProviderConfig)]` generates.
#[doc(hidden)]
pub fn provider_field_value<T: DeserializeOwned>(config: &mut JsonMap, key: &str) -> std::result::Result<T, String> {
    let value = config.remove(key).unwrap_or(Value::Null);
    let missing = value.is_null();
    serde_json::from_value(value).map_err(|error| {
        if missing {
            format!("missing field `{key}`")
        } else {
            format!("field `{key}`: {error}")
        }
    })
}

/// What a provider method returns: a `Result` whose error converts to
/// [`anyhow::Error`]. Lets generated code name the success type.
#[diagnostic::on_unimplemented(
    message = "a provider method returns a `Result`, not `{Self}`",
    note = "return `openshop::Result<T>`, with `T` the value the caller receives"
)]
pub trait MethodResult {
    type Ok;

    fn into_result(self) -> Result<Self::Ok>;
}

impl<T, E: Into<anyhow::Error>> MethodResult for std::result::Result<T, E> {
    type Ok = T;

    fn into_result(self) -> Result<T> {
        self.map_err(Into::into)
    }
}

/// The stored configuration of a provider as the type its methods expect.
fn typed_provider_config<C: DeserializeOwned>(provider: &str, config: JsonMap) -> Result<C> {
    serde_json::from_value(Value::Object(config))
        .map_err(|error| anyhow!("Invalid configuration for provider \"{provider}\": {error}"))
}

/// The arguments of a call as the type its method expects.
fn typed_arguments<A: DeserializeOwned>(label: &str, args: Value) -> Result<A> {
    serde_json::from_value(args).map_err(|error| anyhow!("Invalid arguments for \"{label}\": {error}"))
}

/// An external service with per-shop credentials and callable methods.
#[derive(Clone)]
pub struct Provider {
    pub name: String,
    pub fields: Fields,
    pub transformer: Option<ProviderTransformer>,
    pub checker: Option<ProviderChecker>,
    pub methods: IndexMap<String, ProviderMethod>,
    /// Provider and method named by each call registered with [`Provider::handle`].
    pub(crate) typed_calls: Vec<(&'static str, &'static str)>,
}

impl Provider {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            fields: IndexMap::new(),
            transformer: None,
            checker: None,
            methods: IndexMap::new(),
            typed_calls: Vec::new(),
        }
    }

    pub fn field(mut self, name: impl Into<String>, field: Field) -> Self {
        self.fields.insert(name.into(), field);
        self
    }

    /// Adds several fields, such as those of a [`ProviderConfig`] struct.
    pub fn fields(mut self, fields: Fields) -> Self {
        self.fields.extend(fields);
        self
    }

    pub fn transformer(mut self, transformer: impl Fn(JsonMap) -> Value + Send + Sync + 'static) -> Self {
        self.transformer = Some(Arc::new(transformer));
        self
    }

    pub fn checker<F, Fut>(mut self, checker: F) -> Self
    where
        F: Fn(JsonMap) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<bool>> + Send + 'static,
    {
        self.checker = Some(Arc::new(move |config| Box::pin(checker(config))));
        self
    }

    /// Like [`Provider::checker`], with the configuration deserialized into `C`.
    pub fn typed_checker<C, F, Fut>(self, checker: F) -> Self
    where
        C: DeserializeOwned + Send + 'static,
        F: Fn(C) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<bool>> + Send + 'static,
    {
        let provider = self.name.clone();
        self.checker(move |config| {
            let check = typed_provider_config(&provider, config).map(&checker);
            async move { check?.await }
        })
    }

    /// Registers a method taking the shop configuration and raw JSON arguments.
    pub fn method<F, Fut>(mut self, name: impl Into<String>, method: F) -> Self
    where
        F: Fn(JsonMap, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value>> + Send + 'static,
    {
        self.methods.insert(
            name.into(),
            Arc::new(move |config, args| Box::pin(method(config, args))),
        );
        self
    }

    /// Registers a method with serde-typed arguments and return value.
    pub fn typed_method<A, R, F, Fut>(self, name: impl Into<String>, method: F) -> Self
    where
        A: DeserializeOwned + Send + 'static,
        R: Serialize + 'static,
        F: Fn(JsonMap, A) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<R>> + Send + 'static,
    {
        let name = name.into();
        let label = name.clone();
        self.method(name, move |config, args| {
            let call = typed_arguments(&label, args).map(|args| method(config, args));
            async move { Ok(serde_json::to_value(call?.await?)?) }
        })
    }

    /// Implements a typed [`ProviderCall`]: the method receives the shop
    /// configuration deserialized into `C` and the arguments of the call.
    ///
    /// `C` is any `serde` type matching the fields of the provider. Name its
    /// fields after them, usually with `#[serde(rename_all = "camelCase")]`, and
    /// use `Option` for optional fields.
    pub fn handle<C, A, R, F, Fut>(mut self, call: ProviderCall<A, R>, method: F) -> Self
    where
        C: DeserializeOwned + Send + 'static,
        A: DeserializeOwned + Send + 'static,
        R: Serialize + 'static,
        F: Fn(C, A) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<R>> + Send + 'static,
    {
        let (provider, name) = (call.provider(), call.method());
        self.typed_calls.push((provider, name));
        let label = format!("{provider}.{name}");
        self.method(name, move |config, args| {
            let call = typed_provider_config(provider, config)
                .and_then(|config| Ok(method(config, typed_arguments(&label, args)?)));
            async move { Ok(serde_json::to_value(call?.await?)?) }
        })
    }
}

// ─── Shop scope ──────────────────────────────────────────────────────

/// What a handler runs for: a shop, the Shopify app that serves it, and the
/// application. Implemented by the contexts that belong to a shop, so that
/// what needs one takes the context itself: `sync_orders::dispatch(&ctx, …)`,
/// `Review::of(&ctx)`.
pub trait ShopScope {
    /// The domain of the shop.
    fn shop(&self) -> &str;

    /// The handle of the Shopify app the shop is served by.
    fn shopify_app(&self) -> &str;

    fn app(&self) -> &OpenShop;
}

macro_rules! shop_scope {
    ($($context:ty),+ $(,)?) => {$(
        impl ShopScope for $context {
            fn shop(&self) -> &str {
                &self.shop
            }

            fn shopify_app(&self) -> &str {
                &self.shopify_app
            }

            fn app(&self) -> &OpenShop {
                &self.app
            }
        }
    )+};
}

shop_scope!(FlowContext, WebhookContext, ProxyContext, RouteShopContext, McpContext);

// ─── Flow ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StepOptions {
    pub timeout: Option<Duration>,
}

impl StepOptions {
    pub fn timeout(timeout: Duration) -> Self {
        Self { timeout: Some(timeout) }
    }
}

/// Everything a flow run can use.
#[derive(Clone)]
pub struct FlowContext {
    /// Validated run input.
    pub input: Value,
    pub connectors: Connectors,
    pub shopify: ShopifyClient,
    pub shop: String,
    pub shopify_app: String,
    pub step: StepExecutor,
    pub logger: FlowLogger,
    /// Cancelled when the run is canceled. Pass it to cancellable work in long steps.
    pub signal: CancellationToken,
    pub db: PgPool,
    pub app: OpenShop,
}

impl FlowContext {
    /// Deserializes the validated input into a typed value.
    pub fn input<T: DeserializeOwned>(&self) -> Result<T> {
        Ok(serde_json::from_value(self.input.clone())?)
    }
}

/// Validates run input and exposes its JSON Schema to the admin UI and MCP.
#[derive(Clone)]
pub struct InputSchema {
    pub json_schema: Value,
    parse: Arc<dyn Fn(Value) -> Result<Value, String> + Send + Sync>,
}

impl InputSchema {
    pub fn custom(json_schema: Value, parse: impl Fn(Value) -> Result<Value, String> + Send + Sync + 'static) -> Self {
        Self {
            json_schema,
            parse: Arc::new(parse),
        }
    }

    /// Accepts any JSON value.
    pub fn any() -> Self {
        Self::custom(Value::Object(JsonMap::new()), Ok)
    }

    /// Derives the schema from a Rust type: the value must satisfy the generated
    /// JSON Schema and deserialize into `T`.
    #[cfg(feature = "schema")]
    pub fn of<T: DeserializeOwned + schemars::JsonSchema>() -> Self {
        let json_schema = serde_json::to_value(schemars::schema_for!(T)).unwrap_or(Value::Null);
        let schema = json_schema.clone();
        Self::custom(json_schema, move |value| {
            crate::server::mcp::schema::validate_json_schema(&value, &schema, "input")?;
            serde_json::from_value::<T>(value.clone()).map_err(|error| error.to_string())?;
            Ok(value)
        })
    }

    /// Returns the validated value or a short error summary.
    pub fn parse(&self, value: Value) -> Result<Value, String> {
        (self.parse)(value)
    }
}

impl std::fmt::Debug for InputSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputSchema")
            .field("json_schema", &self.json_schema)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Concurrency {
    /// Refuse to queue a run while another one is active for the same shop.
    #[default]
    Reject,
    Allow,
}

pub type FlowRunFn = Arc<dyn Fn(FlowContext) -> BoxFuture<'static, Result<()>> + Send + Sync>;

/// Background work made of named, checkpointed steps.
#[derive(Clone)]
pub struct Flow {
    pub name: String,
    pub input: Option<InputSchema>,
    pub timeout: Option<Duration>,
    pub step_timeout: Option<Duration>,
    pub concurrency: Option<Concurrency>,
    pub retry_policy: RetryPolicy,
    pub run: FlowRunFn,
}

impl Flow {
    pub fn new<F, Fut>(name: impl Into<String>, run: F) -> Self
    where
        F: Fn(FlowContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        Self {
            name: name.into(),
            input: None,
            timeout: None,
            step_timeout: None,
            concurrency: None,
            retry_policy: RetryPolicy::default(),
            run: Arc::new(move |ctx| Box::pin(run(ctx))),
        }
    }

    #[cfg(feature = "schema")]
    pub fn input<T: DeserializeOwned + schemars::JsonSchema>(self) -> Self {
        self.input_schema(InputSchema::of::<T>())
    }

    pub fn input_schema(mut self, schema: InputSchema) -> Self {
        self.input = Some(schema);
        self
    }

    /// Maximum wall-clock duration of a run, including retries and sleeps.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Default timeout applied to every step.
    pub fn step_timeout(mut self, timeout: Duration) -> Self {
        self.step_timeout = Some(timeout);
        self
    }

    pub fn concurrency(mut self, concurrency: Concurrency) -> Self {
        self.concurrency = Some(concurrency);
        self
    }

    pub fn retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = policy;
        self
    }
}

// ─── Config pieces ───────────────────────────────────────────────────

/// Which shops a cron runs for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum CronShops {
    /// Runs once without shop context.
    #[default]
    Global,
    /// Runs once per installed shop.
    All,
    /// Runs only for this shop.
    Shop(String),
    Shops(Vec<String>),
}

impl CronShops {
    pub fn to_json(&self) -> Value {
        match self {
            Self::Global => Value::from("global"),
            Self::All => Value::from("all"),
            Self::Shop(shop) => Value::from(shop.clone()),
            Self::Shops(shops) => Value::from(shops.clone()),
        }
    }
}

impl From<Vec<String>> for CronShops {
    fn from(value: Vec<String>) -> Self {
        Self::Shops(value)
    }
}

/// A schedule on which a flow runs. Without [`Cron::all_shops`],
/// [`Cron::shop`] or [`Cron::shops`], the flow runs once, for no shop.
#[derive(Debug, Clone, PartialEq)]
pub struct Cron {
    pub name: Option<String>,
    pub schedule: String,
    pub flow: String,
    pub input: Option<Value>,
    pub shops: Option<CronShops>,
}

impl Cron {
    pub fn new(schedule: impl Into<String>, flow: impl Into<String>) -> Self {
        Self {
            name: None,
            schedule: schedule.into(),
            flow: flow.into(),
            input: None,
            shops: None,
        }
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn input(mut self, input: Value) -> Self {
        self.input = Some(input);
        self
    }

    /// Runs once for each shop the application is installed on.
    pub fn all_shops(mut self) -> Self {
        self.shops = Some(CronShops::All);
        self
    }

    /// Runs for this shop only.
    pub fn shop(mut self, shop: impl Into<String>) -> Self {
        self.shops = Some(CronShops::Shop(shop.into()));
        self
    }

    /// Runs for each of these shops.
    pub fn shops<I, S>(mut self, shops: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.shops = Some(CronShops::Shops(shops.into_iter().map(Into::into).collect()));
        self
    }

    /// Stable identifier used for per-shop overrides: `flow:schedule`.
    pub fn key(&self) -> String {
        format!("{}:{}", self.flow, self.schedule)
    }

    pub fn shops_json(&self) -> Value {
        self.shops.clone().unwrap_or_default().to_json()
    }
}

/// Everything the handler of a webhook can use.
#[derive(Clone)]
pub struct WebhookContext {
    pub topic: String,
    pub shop: String,
    pub shopify_app: String,
    pub payload: Value,
    pub api_version: String,
    pub db: PgPool,
    /// The Admin API of the shop. Nothing is read before the first request.
    pub shopify: ShopifyClient,
    /// The providers of the shop. Nothing is read before the first call.
    pub connectors: Connectors,
    pub app: OpenShop,
}

pub type WebhookFn = Arc<dyn Fn(WebhookContext) -> BoxFuture<'static, Result<()>> + Send + Sync>;

#[derive(Clone)]
pub struct Webhook {
    pub run: WebhookFn,
}

impl Webhook {
    pub fn new<F, Fut>(run: F) -> Self
    where
        F: Fn(WebhookContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        Self {
            run: Arc::new(move |ctx| Box::pin(run(ctx))),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShopifyConfig {
    /// Global OAuth scopes. If omitted, OpenShop reads scopes from Shopify TOML files when available.
    pub scopes: Option<String>,
    /// Multiple Shopify apps served by the same OpenShop instance. Omit for env-based single-app mode.
    pub apps: Option<IndexMap<String, ShopifyAppConfig>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShopifyAppConfig {
    Toml {
        /// Shopify app TOML path, relative to the project root. OpenShop reads client_id and app URL from it.
        toml: String,
        /// Shopify API secret. Keep it in env-backed config, not in TOML.
        api_secret: String,
        /// Optional app URL override. Defaults to TOML application_url, HOST, or SHOPIFY_APP_URL.
        app_url: Option<String>,
    },
    Keys {
        /// Shopify API key/client ID for non-TOML setups.
        api_key: String,
        api_secret: String,
        /// Public app URL used for OAuth redirects.
        app_url: Option<String>,
    },
}

impl ShopifyAppConfig {
    pub fn api_secret(&self) -> &str {
        match self {
            Self::Toml { api_secret, .. } | Self::Keys { api_secret, .. } => api_secret,
        }
    }

    pub fn app_url(&self) -> Option<&str> {
        match self {
            Self::Toml { app_url, .. } | Self::Keys { app_url, .. } => app_url.as_deref(),
        }
    }
}

/// Called when a flow run fails, with the flow name.
pub type OnErrorFn = Arc<dyn Fn(Arc<anyhow::Error>, ErrorContext) -> BoxFuture<'static, ()> + Send + Sync>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ErrorContext {
    pub flow: Option<String>,
    pub step: Option<String>,
}

// ─── Shopify Function ────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FunctionType {
    Discount,
    CartTransform,
    DeliveryCustomization,
    PaymentCustomization,
    CheckoutValidation,
    FulfillmentConstraints,
}

impl FunctionType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Discount => "discount",
            Self::CartTransform => "cart-transform",
            Self::DeliveryCustomization => "delivery-customization",
            Self::PaymentCustomization => "payment-customization",
            Self::CheckoutValidation => "checkout-validation",
            Self::FulfillmentConstraints => "fulfillment-constraints",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiscountMode {
    Automatic,
    Code,
}

impl DiscountMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Code => "code",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "automatic" => Some(Self::Automatic),
            "code" => Some(Self::Code),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CombinesWith {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_discounts: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_discounts: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shipping_discounts: Option<bool>,
}

#[derive(Clone)]
pub enum FunctionTitle {
    Static(String),
    /// Computed from the validated instance configuration.
    Dynamic(Arc<dyn Fn(&JsonMap) -> String + Send + Sync>),
}

impl From<&str> for FunctionTitle {
    fn from(value: &str) -> Self {
        Self::Static(value.to_string())
    }
}

impl From<String> for FunctionTitle {
    fn from(value: String) -> Self {
        Self::Static(value)
    }
}

#[derive(Clone)]
pub struct FunctionOwner {
    pub title: FunctionTitle,
    // Discount-specific
    pub starts_at: Option<bool>,
    pub ends_at: Option<bool>,
    pub usage_limit: Option<bool>,
    pub combines_with: Option<CombinesWith>,
    pub applies_on_each_item: Option<bool>,
    // Non-discount
    pub enabled: Option<bool>,
}

impl FunctionOwner {
    pub fn new(title: impl Into<FunctionTitle>) -> Self {
        Self {
            title: title.into(),
            starts_at: None,
            ends_at: None,
            usage_limit: None,
            combines_with: None,
            applies_on_each_item: None,
            enabled: None,
        }
    }

    pub fn dynamic_title(title: impl Fn(&JsonMap) -> String + Send + Sync + 'static) -> Self {
        Self::new(FunctionTitle::Dynamic(Arc::new(title)))
    }

    pub fn starts_at(mut self, value: bool) -> Self {
        self.starts_at = Some(value);
        self
    }

    pub fn ends_at(mut self, value: bool) -> Self {
        self.ends_at = Some(value);
        self
    }

    pub fn usage_limit(mut self, value: bool) -> Self {
        self.usage_limit = Some(value);
        self
    }

    pub fn combines_with(mut self, value: CombinesWith) -> Self {
        self.combines_with = Some(value);
        self
    }

    pub fn applies_on_each_item(mut self, value: bool) -> Self {
        self.applies_on_each_item = Some(value);
        self
    }

    pub fn enabled(mut self, value: bool) -> Self {
        self.enabled = Some(value);
        self
    }
}

/// A Shopify Function whose instances are managed from the admin UI.
#[derive(Clone)]
pub struct Function {
    pub function_type: FunctionType,
    pub handle: String,
    /// Merchant-facing label used by OpenShop. Shopify owners may not have a title.
    pub label: Option<String>,
    pub modes: Option<Vec<DiscountMode>>,
    pub owner: Option<FunctionOwner>,
    pub config: Fields,
}

impl Function {
    pub fn new(function_type: FunctionType, handle: impl Into<String>) -> Self {
        Self {
            function_type,
            handle: handle.into(),
            label: None,
            modes: None,
            owner: None,
            config: IndexMap::new(),
        }
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn modes(mut self, modes: impl IntoIterator<Item = DiscountMode>) -> Self {
        self.modes = Some(modes.into_iter().collect());
        self
    }

    pub fn owner(mut self, owner: FunctionOwner) -> Self {
        self.owner = Some(owner);
        self
    }

    pub fn field(mut self, name: impl Into<String>, field: Field) -> Self {
        self.config.insert(name.into(), field);
        self
    }

    pub(crate) fn first_mode(&self) -> DiscountMode {
        self.modes
            .as_ref()
            .and_then(|modes| modes.first().copied())
            .unwrap_or(DiscountMode::Automatic)
    }
}

/// A struct that is the configuration of a Shopify Function: its fields are
/// the form of an instance. Implemented by `#[derive(FunctionConfig)]`.
pub trait FunctionConfig: Sized {
    /// The name the function is registered under.
    const NAME: &'static str;

    /// The function, with the fields of the struct as its form.
    fn definition() -> Function;

    /// Reads the configuration of an instance.
    fn from_config(config: JsonMap) -> Result<Self, String>;
}

// ─── App Proxy ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProxyResponseType {
    Liquid,
    #[default]
    Json,
    Html,
}

impl ProxyResponseType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Liquid => "liquid",
            Self::Json => "json",
            Self::Html => "html",
        }
    }
}

/// How the shop and customer identity of a proxy request were established.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ProxyAuthKind {
    #[serde(rename = "appProxyHmac")]
    AppProxyHmac,
    #[serde(rename = "customerAccountJwt")]
    CustomerAccountJwt,
}

#[derive(Clone)]
pub struct ProxyContext {
    /// Shop domain (from the signed query or the JWT `dest` claim).
    pub shop: String,
    /// Internal OpenShop Shopify app handle that authenticated this request.
    pub shopify_app: String,
    /// Customer ID as a numeric string (signed `logged_in_customer_id` or JWT `sub`). Always trusted.
    pub customer_id: Option<String>,
    pub auth: ProxyAuthKind,
    pub query: IndexMap<String, String>,
    pub params: IndexMap<String, String>,
    /// Header names are lowercase.
    pub headers: IndexMap<String, String>,
    pub path: String,
    pub method: String,
    pub body: Option<Value>,
    pub db: PgPool,
    /// The Admin API of the shop. Nothing is read before the first request.
    pub shopify: ShopifyClient,
    /// The providers of the shop. Nothing is read before the first call.
    pub connectors: Connectors,
    pub app: OpenShop,
}

pub type ProxyHandler = Arc<dyn Fn(ProxyContext) -> BoxFuture<'static, Result<Value>> + Send + Sync>;

/// Handlers for a storefront app proxy path. A string result is sent as Liquid
/// or HTML for those response types; everything else is sent as JSON.
#[derive(Clone, Default)]
pub struct Proxy {
    pub response_type: Option<ProxyResponseType>,
    pub get: Option<ProxyHandler>,
    pub post: Option<ProxyHandler>,
    pub put: Option<ProxyHandler>,
    pub delete: Option<ProxyHandler>,
    pub patch: Option<ProxyHandler>,
}

fn proxy_handler<F, Fut>(handler: F) -> ProxyHandler
where
    F: Fn(ProxyContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Value>> + Send + 'static,
{
    Arc::new(move |ctx| Box::pin(handler(ctx)))
}

macro_rules! proxy_methods {
    ($($name:ident),*) => {
        $(
            pub fn $name<F, Fut>(mut self, handler: F) -> Self
            where
                F: Fn(ProxyContext) -> Fut + Send + Sync + 'static,
                Fut: Future<Output = Result<Value>> + Send + 'static,
            {
                self.$name = Some(proxy_handler(handler));
                self
            }
        )*
    };
}

impl Proxy {
    pub fn new(response_type: ProxyResponseType) -> Self {
        Self {
            response_type: Some(response_type),
            ..Self::default()
        }
    }

    pub fn json() -> Self {
        Self::new(ProxyResponseType::Json)
    }

    pub fn liquid() -> Self {
        Self::new(ProxyResponseType::Liquid)
    }

    pub fn html() -> Self {
        Self::new(ProxyResponseType::Html)
    }

    proxy_methods!(get, post, put, delete, patch);

    pub(crate) fn handlers(&self) -> [(&'static str, Option<&ProxyHandler>); 5] {
        [
            ("GET", self.get.as_ref()),
            ("POST", self.post.as_ref()),
            ("PUT", self.put.as_ref()),
            ("DELETE", self.delete.as_ref()),
            ("PATCH", self.patch.as_ref()),
        ]
    }

    /// Adds the handlers of another definition of the same path. Fails when
    /// both handle a method, or answer with different response types.
    pub(crate) fn merge(&mut self, other: Self) -> Result<(), String> {
        let (ours, theirs) = (
            self.response_type.unwrap_or_default(),
            other.response_type.unwrap_or_default(),
        );
        if ours != theirs {
            return Err(format!(
                "answers with both {} and {}; a path has one response type",
                ours.as_str(),
                theirs.as_str()
            ));
        }
        for (method, slot, handler) in [
            ("GET", &mut self.get, other.get),
            ("POST", &mut self.post, other.post),
            ("PUT", &mut self.put, other.put),
            ("DELETE", &mut self.delete, other.delete),
            ("PATCH", &mut self.patch, other.patch),
        ] {
            if handler.is_none() {
                continue;
            }
            if slot.is_some() {
                return Err(format!("handles {method} twice"));
            }
            *slot = handler;
        }
        Ok(())
    }
}

// ─── Public server routes ──────────────────────────────────────────

/// An actively installed shop, as a public route resolves it: its identity
/// and what the other contexts give for a shop.
#[derive(Clone)]
pub struct RouteShopContext {
    pub shop: String,
    pub shopify_app: String,
    pub db: PgPool,
    pub shopify: ShopifyClient,
    pub connectors: Connectors,
    pub app: OpenShop,
}

/// The shop a public route asks for: its domain and, when the application
/// serves several Shopify apps, the handle of the app. A domain alone names
/// the shop for the single-app setup: `ctx.for_shop("demo.myshopify.com")`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteShopInput {
    pub shop: String,
    pub shopify_app: Option<String>,
}

impl RouteShopInput {
    pub fn new(shop: impl Into<String>) -> Self {
        Self {
            shop: shop.into(),
            shopify_app: None,
        }
    }

    pub fn shopify_app(mut self, handle: impl Into<String>) -> Self {
        self.shopify_app = Some(handle.into());
        self
    }
}

impl From<&str> for RouteShopInput {
    fn from(shop: &str) -> Self {
        Self::new(shop)
    }
}

impl From<String> for RouteShopInput {
    fn from(shop: String) -> Self {
        Self::new(shop)
    }
}

impl From<&String> for RouteShopInput {
    fn from(shop: &String) -> Self {
        Self::new(shop.as_str())
    }
}

/// The incoming request with its body fully read.
#[derive(Debug, Clone)]
pub struct RouteRequest {
    pub method: axum::http::Method,
    pub uri: axum::http::Uri,
    pub headers: axum::http::HeaderMap,
    pub body: axum::body::Bytes,
}

impl RouteRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    /// Query parameters; the first value wins for repeated keys.
    pub fn query(&self) -> IndexMap<String, String> {
        crate::server::http::parse_query(self.uri.query())
    }

    pub fn json<T: DeserializeOwned>(&self) -> Result<T> {
        Ok(serde_json::from_slice(&self.body)?)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// What the authentication function of a route receives.
#[derive(Clone)]
pub struct RouteAuthContext {
    pub request: RouteRequest,
    pub params: IndexMap<String, String>,
    pub db: PgPool,
    pub app: OpenShop,
}

impl RouteAuthContext {
    /// Resolves an installed shop, and gives what a context gives for it:
    /// its database, its Shopify client and its providers.
    pub async fn for_shop(&self, shop: impl Into<RouteShopInput>) -> Result<RouteShopContext> {
        crate::server::routes::resolve_shop_context(&self.app, shop.into()).await
    }
}

/// What the handler of a route receives. `A` is what the authentication
/// function of the route returns for an authenticated request: a `Value` by
/// default, `Null` on a public route.
#[derive(Clone)]
pub struct RouteContext<A = Value> {
    pub request: RouteRequest,
    pub params: IndexMap<String, String>,
    /// What the authentication function of the route returned.
    pub auth: A,
    pub db: PgPool,
    pub app: OpenShop,
}

impl<A> RouteContext<A> {
    /// Resolves an installed shop, and gives what a context gives for it:
    /// its database, its Shopify client and its providers.
    pub async fn for_shop(&self, shop: impl Into<RouteShopInput>) -> Result<RouteShopContext> {
        crate::server::routes::resolve_shop_context(&self.app, shop.into()).await
    }
}

/// Outcome of a route authentication function. `A` is what an authenticated
/// request is known by, which the handler finds in `ctx.auth`: any type, a
/// `Value` unless stated.
///
/// ```
/// use openshop::prelude::*;
///
/// pub struct Partner {
///     pub key: String,
/// }
///
/// async fn partner(ctx: RouteAuthContext) -> Result<RouteAuthOutcome<Partner>> {
///     Ok(match ctx.request.header("x-partner-key") {
///         Some(key) => RouteAuthOutcome::Authenticated(Partner { key: key.to_string() }),
///         None => RouteAuthOutcome::Unauthorized,
///     })
/// }
///
/// #[route(GET, "/whoami", json, auth = partner)]
/// async fn whoami(ctx: RouteContext<Partner>) -> Result<Value> {
///     Ok(json!({ "key": ctx.auth.key }))
/// }
/// ```
pub enum RouteAuthOutcome<A = Value> {
    /// Request is authenticated; the value is exposed as `ctx.auth`.
    Authenticated(A),
    /// Rejects with `401 Unauthorized`.
    Unauthorized,
    /// Short-circuits with a custom response.
    Response(axum::response::Response),
}

/// The value of an authenticated request as the server carries it from the
/// authentication function of a route to its handler, whatever its type.
pub type RouteAuthValue = Box<dyn std::any::Any + Send>;

pub type RouteAuthFn =
    Arc<dyn Fn(RouteAuthContext) -> BoxFuture<'static, Result<RouteAuthOutcome<RouteAuthValue>>> + Send + Sync>;
pub type RouteHandler =
    Arc<dyn Fn(RouteContext<RouteAuthValue>) -> BoxFuture<'static, Result<axum::response::Response>> + Send + Sync>;

#[derive(Clone)]
pub enum RouteAuth {
    /// Public route: no authentication.
    None,
    Custom(RouteAuthFn),
}

pub const ROUTE_METHODS: [&str; 7] = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

/// The handler of one HTTP method, with the authentication it was declared under.
#[derive(Clone)]
pub struct RouteMethod {
    pub auth: RouteAuth,
    pub handler: RouteHandler,
}

/// A public HTTP route mounted under `/routes` with an explicit authentication
/// contract. `A` is what its authentication function returns for an
/// authenticated request, and what its handlers find in `ctx.auth`.
pub struct Route<A = Value> {
    /// Authentication of the handlers added to this definition.
    pub auth: RouteAuth,
    pub handlers: IndexMap<&'static str, RouteMethod>,
    value: PhantomData<fn() -> A>,
}

impl<A> Clone for Route<A> {
    fn clone(&self) -> Self {
        Self {
            auth: self.auth.clone(),
            handlers: self.handlers.clone(),
            value: PhantomData,
        }
    }
}

macro_rules! route_methods {
    ($($name:ident => $method:literal),*) => {
        $(
            pub fn $name<F, Fut>(self, handler: F) -> Self
            where
                F: Fn(RouteContext<A>) -> Fut + Send + Sync + 'static,
                Fut: Future<Output = Result<axum::response::Response>> + Send + 'static,
            {
                self.handler($method, handler)
            }
        )*
    };
}

impl Route {
    /// A route without authentication (`auth: 'none'`). Its handlers find
    /// `Null` in `ctx.auth`.
    pub fn public() -> Self {
        Self {
            auth: RouteAuth::None,
            handlers: IndexMap::new(),
            value: PhantomData,
        }
    }
}

impl<A: Send + 'static> Route<A> {
    /// A route whose requests `auth` decides on before a handler runs. What
    /// it returns for an authenticated request is the `ctx.auth` of the
    /// handlers, with its type.
    pub fn authenticated<F, Fut>(auth: F) -> Self
    where
        F: Fn(RouteAuthContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<RouteAuthOutcome<A>>> + Send + 'static,
    {
        Self {
            auth: RouteAuth::Custom(Arc::new(move |ctx| {
                let outcome = auth(ctx);
                Box::pin(async move {
                    Ok(match outcome.await? {
                        RouteAuthOutcome::Authenticated(value) => {
                            RouteAuthOutcome::Authenticated(Box::new(value) as RouteAuthValue)
                        }
                        RouteAuthOutcome::Unauthorized => RouteAuthOutcome::Unauthorized,
                        RouteAuthOutcome::Response(response) => RouteAuthOutcome::Response(response),
                    })
                })
            })),
            handlers: IndexMap::new(),
            value: PhantomData,
        }
    }

    fn handler<F, Fut>(mut self, method: &'static str, handler: F) -> Self
    where
        F: Fn(RouteContext<A>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<axum::response::Response>> + Send + 'static,
    {
        self.handlers.insert(
            method,
            RouteMethod {
                auth: self.auth.clone(),
                // The value comes from the authentication of this definition: it is an `A`.
                handler: Arc::new(move |ctx| -> BoxFuture<'static, Result<axum::response::Response>> {
                    let RouteContext {
                        request,
                        params,
                        auth,
                        db,
                        app,
                    } = ctx;
                    match auth.downcast::<A>() {
                        Ok(auth) => Box::pin(handler(RouteContext {
                            request,
                            params,
                            auth: *auth,
                            db,
                            app,
                        })),
                        Err(_) => Box::pin(async {
                            Err(anyhow!(
                                "[openshop] A route handler received the authentication value of another route"
                            ))
                        }),
                    }
                }),
            },
        );
        self
    }

    route_methods!(get => "GET", head => "HEAD", post => "POST", put => "PUT", patch => "PATCH", delete => "DELETE", options => "OPTIONS");

    /// The definition without the type of its authentication value, as an
    /// application stores it: its handlers already know that type.
    pub(crate) fn erased(self) -> Route {
        Route {
            auth: self.auth,
            handlers: self.handlers,
            value: PhantomData,
        }
    }
}

impl<A> Route<A> {
    /// Adds the handlers of another definition of the same path, each keeping
    /// its own authentication. Fails when both handle a method.
    pub(crate) fn merge(&mut self, other: Route) -> Result<(), String> {
        for (method, handler) in other.handlers {
            if self.handlers.contains_key(method) {
                return Err(format!("handles {method} twice"));
            }
            self.handlers.insert(method, handler);
        }
        Ok(())
    }

    /// Declared methods in canonical order.
    pub(crate) fn allowed_methods(&self) -> Vec<&'static str> {
        ROUTE_METHODS
            .into_iter()
            .filter(|method| self.handlers.contains_key(method))
            .collect()
    }
}

// ─── MCP ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpCapabilityType {
    Tool,
    Resource,
}

impl McpCapabilityType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Resource => "resource",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpRiskLevel {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpAuditStatus {
    Success,
    Denied,
    Error,
}

impl McpAuditStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Denied => "denied",
            Self::Error => "error",
        }
    }
}

/// A permission an MCP token can be granted, and that tools and resources
/// require.
///
/// Declare it once, as a constant, next to what requires it. A tool that
/// lists it registers it with the application:
///
/// ```
/// use openshop::{McpPermission, McpRiskLevel};
///
/// pub const READ_REVIEWS: McpPermission = McpPermission::new("reviews:read", "Read reviews")
///     .group("Reviews")
///     .risk(McpRiskLevel::Low);
/// assert_eq!(READ_REVIEWS.key, "reviews:read");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpPermission {
    /// What tokens are granted and capabilities require: `namespace:action`.
    #[serde(skip)]
    pub key: &'static str,
    pub label: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk_level: Option<McpRiskLevel>,
}

impl McpPermission {
    pub const fn new(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            description: None,
            group: None,
            risk_level: None,
        }
    }

    pub const fn description(mut self, description: &'static str) -> Self {
        self.description = Some(description);
        self
    }

    /// The heading the admin lists the permission under. Defaults to the
    /// namespace of the key.
    pub const fn group(mut self, group: &'static str) -> Self {
        self.group = Some(group);
        self
    }

    pub const fn risk(mut self, level: McpRiskLevel) -> Self {
        self.risk_level = Some(level);
        self
    }
}

/// A permission a tool or a resource requires: its key, or the
/// [`McpPermission`] itself, which is then registered with the capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequiredPermission {
    Key(String),
    Declared(McpPermission),
}

impl RequiredPermission {
    pub fn key(&self) -> &str {
        match self {
            Self::Key(key) => key,
            Self::Declared(permission) => permission.key,
        }
    }
}

impl From<&str> for RequiredPermission {
    fn from(key: &str) -> Self {
        Self::Key(key.to_string())
    }
}

impl From<String> for RequiredPermission {
    fn from(key: String) -> Self {
        Self::Key(key)
    }
}

impl From<McpPermission> for RequiredPermission {
    fn from(permission: McpPermission) -> Self {
        Self::Declared(permission)
    }
}

impl From<&McpPermission> for RequiredPermission {
    fn from(permission: &McpPermission) -> Self {
        Self::Declared(*permission)
    }
}

/// The keys of the permissions, and those that come with their definition.
fn required_permissions<I, P>(permissions: I) -> (Vec<String>, Vec<McpPermission>)
where
    I: IntoIterator<Item = P>,
    P: Into<RequiredPermission>,
{
    let mut keys = Vec::new();
    let mut declared = Vec::new();
    for permission in permissions {
        let permission = permission.into();
        keys.push(permission.key().to_string());
        if let RequiredPermission::Declared(permission) = permission {
            declared.push(permission);
        }
    }
    (keys, declared)
}

/// Everything an MCP tool or resource can use.
#[derive(Clone)]
pub struct McpContext {
    pub shop: String,
    /// The handle of the Shopify app the token belongs to.
    pub shopify_app: String,
    pub token_id: String,
    pub permissions: Vec<String>,
    /// Cancelled when the request is aborted or exceeds the execution limit.
    pub signal: CancellationToken,
    pub db: PgPool,
    /// The Admin API of the shop. Nothing is read before the first request.
    pub shopify: ShopifyClient,
    /// The providers of the shop. Nothing is read before the first call.
    pub connectors: Connectors,
    pub app: OpenShop,
}

/// A tool result may be a string, an object with `content` / `structuredContent`,
/// or any other JSON value (rendered as text and structured content).
pub type McpToolFn = Arc<dyn Fn(McpContext, JsonMap) -> BoxFuture<'static, Result<Value>> + Send + Sync>;

#[derive(Clone)]
pub struct McpTool {
    pub description: String,
    pub input_schema: Option<Value>,
    pub required_permissions: Vec<String>,
    /// The required permissions that came with their definition.
    pub declared_permissions: Vec<McpPermission>,
    pub risk_level: Option<McpRiskLevel>,
    pub confirmation_hint: Option<String>,
    pub run: McpToolFn,
}

impl McpTool {
    pub fn new<F, Fut>(description: impl Into<String>, run: F) -> Self
    where
        F: Fn(McpContext, JsonMap) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value>> + Send + 'static,
    {
        Self {
            description: description.into(),
            input_schema: None,
            required_permissions: Vec::new(),
            declared_permissions: Vec::new(),
            risk_level: None,
            confirmation_hint: None,
            run: Arc::new(move |ctx, input| Box::pin(run(ctx, input))),
        }
    }

    pub fn input_schema(mut self, schema: Value) -> Self {
        self.input_schema = Some(schema);
        self
    }

    /// The permissions a token needs: keys, or [`McpPermission`]s, which
    /// are registered with the application.
    pub fn permissions<I, P>(mut self, permissions: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<RequiredPermission>,
    {
        (self.required_permissions, self.declared_permissions) = required_permissions(permissions);
        self
    }

    pub fn risk(mut self, level: McpRiskLevel) -> Self {
        self.risk_level = Some(level);
        self
    }

    pub fn confirmation_hint(mut self, hint: impl Into<String>) -> Self {
        self.confirmation_hint = Some(hint.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpResourceContent {
    pub text: String,
    pub mime_type: Option<String>,
}

impl From<String> for McpResourceContent {
    fn from(text: String) -> Self {
        Self { text, mime_type: None }
    }
}

impl From<&str> for McpResourceContent {
    fn from(text: &str) -> Self {
        Self::from(text.to_string())
    }
}

pub type McpResourceFn = Arc<dyn Fn(McpContext) -> BoxFuture<'static, Result<McpResourceContent>> + Send + Sync>;

#[derive(Clone)]
pub struct McpResource {
    pub name: String,
    pub description: Option<String>,
    pub mime_type: Option<String>,
    pub required_permissions: Vec<String>,
    /// The required permissions that came with their definition.
    pub declared_permissions: Vec<McpPermission>,
    pub risk_level: Option<McpRiskLevel>,
    pub read: McpResourceFn,
}

impl McpResource {
    pub fn new<F, Fut>(name: impl Into<String>, read: F) -> Self
    where
        F: Fn(McpContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<McpResourceContent>> + Send + 'static,
    {
        Self {
            name: name.into(),
            description: None,
            mime_type: None,
            required_permissions: Vec::new(),
            declared_permissions: Vec::new(),
            risk_level: None,
            read: Arc::new(move |ctx| Box::pin(read(ctx))),
        }
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn mime_type(mut self, mime_type: impl Into<String>) -> Self {
        self.mime_type = Some(mime_type.into());
        self
    }

    /// The permissions a token needs: keys, or [`McpPermission`]s, which
    /// are registered with the application.
    pub fn permissions<I, P>(mut self, permissions: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<RequiredPermission>,
    {
        (self.required_permissions, self.declared_permissions) = required_permissions(permissions);
        self
    }

    pub fn risk(mut self, level: McpRiskLevel) -> Self {
        self.risk_level = Some(level);
        self
    }
}

#[derive(Clone, Default)]
pub struct McpConfig {
    /// MCP is enabled unless this is `Some(false)`.
    pub enabled: Option<bool>,
    pub custom_permissions: IndexMap<String, McpPermission>,
    pub tools: IndexMap<String, McpTool>,
    pub resources: IndexMap<String, McpResource>,
}

impl McpConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = Some(enabled);
        self
    }

    /// The permissions of the application: the ones registered here, and the
    /// ones its tools and resources came with. The first definition of a key
    /// is kept; building the application refuses two different ones.
    pub fn permissions(&self) -> IndexMap<String, McpPermission> {
        let mut permissions = self.custom_permissions.clone();
        let declared = self.tools.values().flat_map(|tool| &tool.declared_permissions).chain(
            self.resources
                .values()
                .flat_map(|resource| &resource.declared_permissions),
        );
        for permission in declared {
            permissions.entry(permission.key.to_string()).or_insert(*permission);
        }
        permissions
    }

    /// MCP turned off: `POST /mcp` is refused, and the admin has no MCP page.
    pub fn disabled() -> Self {
        Self::new().enabled(false)
    }

    /// Registers a permission no tool or resource of the application lists.
    /// The ones they list are registered with them.
    pub fn permission(mut self, permission: McpPermission) -> Self {
        self.custom_permissions.insert(permission.key.to_string(), permission);
        self
    }

    pub fn tool(mut self, name: impl Into<String>, tool: McpTool) -> Self {
        self.tools.insert(name.into(), tool);
        self
    }

    pub fn resource(mut self, uri: impl Into<String>, resource: McpResource) -> Self {
        self.resources.insert(uri.into(), resource);
        self
    }
}

// ─── Retry & Worker ─────────────────────────────────────────────────

fn lenient_u64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let number = f64::deserialize(deserializer)?;
    Ok(if number.is_finite() && number > 0.0 {
        number.round() as u64
    } else {
        0
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedRetryPolicy {
    #[serde(deserialize_with = "lenient_u64")]
    pub max_attempts: u64,
    #[serde(deserialize_with = "lenient_u64")]
    pub initial_interval_ms: u64,
    pub backoff_coefficient: f64,
    #[serde(deserialize_with = "lenient_u64")]
    pub max_interval_ms: u64,
}

/// The retry policy of a flow, of an application or of one dispatch. A value
/// left unset falls back to the next level, then to the defaults: 3 attempts,
/// 1 second doubling up to 30 seconds.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RetryPolicy {
    pub max_attempts: Option<u64>,
    pub initial_interval: Option<Duration>,
    pub backoff_coefficient: Option<f64>,
    pub max_interval: Option<Duration>,
}

impl RetryPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many times a run is attempted.
    pub fn max_attempts(mut self, value: u64) -> Self {
        self.max_attempts = Some(value);
        self
    }

    /// The delay before the second attempt.
    pub fn initial_interval(mut self, value: Duration) -> Self {
        self.initial_interval = Some(value);
        self
    }

    /// What the delay is multiplied by after each attempt.
    pub fn backoff_coefficient(mut self, value: f64) -> Self {
        self.backoff_coefficient = Some(value);
        self
    }

    /// The longest delay between two attempts.
    pub fn max_interval(mut self, value: Duration) -> Self {
        self.max_interval = Some(value);
        self
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The settings of a worker, with the defaults applied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedWorkerConfig {
    pub concurrency: usize,
    pub poll_interval_ms: u64,
    pub poll_max_interval_ms: u64,
    pub poll_backoff_coefficient: f64,
    pub lease_duration_ms: u64,
}

/// The settings of the workers of an application. A value left unset keeps
/// its default.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct WorkerConfig {
    pub concurrency: Option<usize>,
    pub poll_interval: Option<Duration>,
    pub poll_max_interval: Option<Duration>,
    pub poll_backoff_coefficient: Option<f64>,
    pub lease_duration: Option<Duration>,
}

impl WorkerConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many runs a worker executes at once.
    pub fn concurrency(mut self, value: usize) -> Self {
        self.concurrency = Some(value);
        self
    }

    /// How long an idle worker waits before looking for work again.
    pub fn poll_interval(mut self, value: Duration) -> Self {
        self.poll_interval = Some(value);
        self
    }

    /// The longest an idle worker waits between two looks.
    pub fn poll_max_interval(mut self, value: Duration) -> Self {
        self.poll_max_interval = Some(value);
        self
    }

    /// What the wait is multiplied by each time a worker finds nothing.
    pub fn poll_backoff_coefficient(mut self, value: f64) -> Self {
        self.poll_backoff_coefficient = Some(value);
        self
    }

    /// How long a worker owns a run without extending its lease.
    pub fn lease_duration(mut self, value: Duration) -> Self {
        self.lease_duration = Some(value);
        self
    }
}

// ─── Row status types ────────────────────────────────────────────────

pub const ACTIVE_RUN_STATUSES: [&str; 3] = ["pending", "running", "sleeping"];
pub const ALL_RUN_STATUSES: [&str; 6] = ["pending", "running", "sleeping", "completed", "failed", "canceled"];
