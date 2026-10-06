//! Recording fakes for provider methods.

use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use indexmap::IndexMap;
use serde::Serialize;
use serde_json::Value;

use crate::server::connectors::{Connector, ConnectorMethod, Connectors};
use crate::types::{Provider, ProviderCall};

#[derive(Debug, Clone, PartialEq)]
pub struct FakeCall {
    pub args: Value,
    pub returned_value: Option<Value>,
    pub thrown_error: Option<String>,
    pub timestamp: DateTime<Utc>,
}

type Implementation = Arc<dyn Fn(Value) -> BoxFuture<'static, Result<Value>> + Send + Sync>;

#[derive(Default)]
struct FakeState {
    default_return: Value,
    default_error: Option<String>,
    implementation: Option<Implementation>,
    per_call: Vec<(usize, Result<Value, String>)>,
    calls: Vec<FakeCall>,
}

/// A spy standing in for one provider method. Returns `null` unless told otherwise.
#[derive(Clone, Default)]
pub struct FakeMethod {
    state: Arc<Mutex<FakeState>>,
}

impl FakeMethod {
    fn state(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn calls(&self) -> Vec<FakeCall> {
        self.state().calls.clone()
    }

    pub fn called(&self) -> bool {
        !self.state().calls.is_empty()
    }

    pub fn call_count(&self) -> usize {
        self.state().calls.len()
    }

    pub fn last_call(&self) -> Option<FakeCall> {
        self.state().calls.last().cloned()
    }

    /// Value returned by every call without a more specific rule.
    pub fn returns(&self, value: impl Serialize) {
        self.state().default_return = serde_json::to_value(value).unwrap_or(Value::Null);
    }

    /// Makes every call without a more specific rule fail.
    pub fn rejects(&self, error: impl Into<String>) {
        self.state().default_error = Some(error.into());
    }

    /// Value returned by the `n`-th call (zero-based).
    pub fn on_call_returns(&self, n: usize, value: impl Serialize) {
        let value = serde_json::to_value(value).unwrap_or(Value::Null);
        self.state().per_call.push((n, Ok(value)));
    }

    /// Makes the `n`-th call (zero-based) fail.
    pub fn on_call_rejects(&self, n: usize, error: impl Into<String>) {
        self.state().per_call.push((n, Err(error.into())));
    }

    /// Replaces the method body.
    pub fn implementation<F, Fut>(&self, implementation: F)
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value>> + Send + 'static,
    {
        self.state().implementation = Some(Arc::new(move |args| Box::pin(implementation(args))));
    }

    /// Forgets recorded calls and every configured behavior.
    pub fn reset(&self) {
        *self.state() = FakeState::default();
    }

    pub async fn call(&self, args: Value) -> Result<Value> {
        let (override_result, implementation, default_error, default_return) = {
            let state = self.state();
            let index = state.calls.len();
            (
                state
                    .per_call
                    .iter()
                    .rev()
                    .find(|(n, _)| *n == index)
                    .map(|(_, result)| result.clone()),
                state.implementation.clone(),
                state.default_error.clone(),
                state.default_return.clone(),
            )
        };

        let outcome = match (override_result, implementation, default_error) {
            (Some(result), _, _) => result,
            (None, Some(implementation), _) => implementation(args.clone()).await.map_err(|error| format!("{error:#}")),
            (None, None, Some(error)) => Err(error),
            (None, None, None) => Ok(default_return),
        };

        let (returned_value, thrown_error) = match &outcome {
            Ok(value) => (Some(value.clone()), None),
            Err(error) => (None, Some(error.clone())),
        };
        self.state().calls.push(FakeCall {
            args,
            returned_value,
            thrown_error,
            timestamp: Utc::now(),
        });
        outcome.map_err(|error| anyhow!(error))
    }
}

/// One [`FakeMethod`] per method of every configured provider.
#[derive(Clone, Default)]
pub struct FakeProviders {
    providers: IndexMap<String, IndexMap<String, FakeMethod>>,
}

impl FakeProviders {
    /// The fake of `provider.method`. Panics when the provider or method is not configured.
    pub fn method(&self, provider: &str, method: &str) -> &FakeMethod {
        self.providers
            .get(provider)
            .and_then(|methods| methods.get(method))
            .unwrap_or_else(|| panic!("no fake for provider method \"{provider}.{method}\""))
    }

    pub fn get(&self, provider: &str, method: &str) -> Option<&FakeMethod> {
        self.providers.get(provider)?.get(method)
    }

    /// The fake of a typed call. Panics when the provider or method is not configured.
    pub fn for_call<A, R>(&self, call: ProviderCall<A, R>) -> &FakeMethod {
        self.method(call.provider(), call.method())
    }

    /// Connectors whose methods are the fakes.
    pub fn connectors(&self) -> Connectors {
        Connectors::new(
            self.providers
                .iter()
                .map(|(name, methods)| {
                    let methods = methods
                        .iter()
                        .map(|(method_name, fake)| {
                            let fake = fake.clone();
                            let method: ConnectorMethod = Arc::new(move |args| {
                                let fake = fake.clone();
                                Box::pin(async move { fake.call(args).await })
                            });
                            (method_name.clone(), method)
                        })
                        .collect();
                    (name.clone(), Connector::new(methods))
                })
                .collect(),
        )
    }

    /// Reset all fakes: call history and configured behavior.
    pub fn reset(&self) {
        for fake in self.providers.values().flat_map(IndexMap::values) {
            fake.reset();
        }
    }
}

pub fn create_fake_providers(providers: &IndexMap<String, Provider>) -> FakeProviders {
    FakeProviders {
        providers: providers
            .iter()
            .map(|(name, provider)| {
                let methods = provider
                    .methods
                    .keys()
                    .map(|method| (method.clone(), FakeMethod::default()))
                    .collect();
                (name.clone(), methods)
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn records_calls_and_returns_null_by_default() {
        let fake = FakeMethod::default();
        assert!(!fake.called());
        assert_eq!(fake.call(json!([1, 2])).await.unwrap(), Value::Null);
        assert!(fake.called());
        assert_eq!(fake.call_count(), 1);
        let call = fake.last_call().unwrap();
        assert_eq!(call.args, json!([1, 2]));
        assert_eq!(call.returned_value, Some(Value::Null));
        assert_eq!(call.thrown_error, None);
    }

    #[tokio::test]
    async fn applies_rules_by_priority() {
        let fake = FakeMethod::default();
        fake.returns("default");
        fake.on_call_returns(1, "second");
        fake.on_call_rejects(2, "third failed");

        assert_eq!(fake.call(Value::Null).await.unwrap(), json!("default"));
        assert_eq!(fake.call(Value::Null).await.unwrap(), json!("second"));
        assert_eq!(fake.call(Value::Null).await.unwrap_err().to_string(), "third failed");
        assert_eq!(fake.calls()[2].thrown_error.as_deref(), Some("third failed"));

        fake.implementation(|args| async move { Ok(json!({ "echo": args })) });
        assert_eq!(fake.call(json!(7)).await.unwrap(), json!({ "echo": 7 }));

        fake.reset();
        fake.rejects("always");
        assert_eq!(fake.call(Value::Null).await.unwrap_err().to_string(), "always");
        assert_eq!(fake.call_count(), 1);
    }

    #[tokio::test]
    async fn builds_connectors_from_provider_definitions() {
        let providers = IndexMap::from([(
            "warehouse".to_string(),
            Provider::new("warehouse")
                .method("push", |_config, _args| async { Ok(json!(true)) })
                .method("delete", |_config, _args| async { Ok(Value::Null) }),
        )]);
        let fakes = create_fake_providers(&providers);
        fakes.method("warehouse", "push").returns(42);

        let connectors = fakes.connectors();
        let result: i64 = connectors
            .call_as("warehouse", "push", json!([{ "id": 1 }]))
            .await
            .unwrap();
        assert_eq!(result, 42);
        assert_eq!(
            fakes.method("warehouse", "push").last_call().unwrap().args,
            json!([{ "id": 1 }])
        );
        assert!(fakes.get("warehouse", "missing").is_none());
        assert!(connectors.call("warehouse", "missing", Value::Null).await.is_err());

        fakes.reset();
        assert!(!fakes.method("warehouse", "push").called());
    }
}
