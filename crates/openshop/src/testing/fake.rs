//! Recording fakes for provider methods.

use std::marker::PhantomData;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::locked;
use crate::server::connectors::{Connector, ConnectorMethod, Connectors};
use crate::types::{Provider, ProviderCall};

#[derive(Debug, Clone, PartialEq)]
pub struct FakeCall {
    pub args: Value,
    pub returned_value: Option<Value>,
    pub thrown_error: Option<String>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Default)]
struct FakeState {
    default_return: Value,
    default_error: Option<String>,
    calls: Vec<FakeCall>,
}

/// A spy standing in for one provider method. Returns `null` unless told otherwise.
#[derive(Clone, Default)]
pub struct FakeMethod {
    state: Arc<Mutex<FakeState>>,
}

impl FakeMethod {
    fn state(&self) -> MutexGuard<'_, FakeState> {
        locked(&self.state)
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

    /// Value returned by every call.
    pub fn returns(&self, value: impl Serialize) {
        self.state().default_return = serde_json::to_value(value).unwrap_or(Value::Null);
    }

    /// Makes every call fail.
    pub fn rejects(&self, error: impl Into<String>) {
        self.state().default_error = Some(error.into());
    }

    /// Forgets recorded calls and every configured behavior.
    pub fn reset(&self) {
        *self.state() = FakeState::default();
    }

    /// Records a call, and answers it as configured.
    pub fn call(&self, args: Value) -> Result<Value> {
        let mut state = self.state();
        let outcome = match &state.default_error {
            Some(error) => Err(error.clone()),
            None => Ok(state.default_return.clone()),
        };
        state.calls.push(FakeCall {
            args,
            returned_value: outcome.clone().ok(),
            thrown_error: outcome.clone().err(),
            timestamp: Utc::now(),
        });
        outcome.map_err(|error| anyhow!(error))
    }
}

/// A call a [`TypedFake`] recorded, with the arguments as the method receives them.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedCall<A> {
    pub args: A,
    pub returned_value: Option<Value>,
    pub thrown_error: Option<String>,
    pub timestamp: DateTime<Utc>,
}

/// The fake of a typed call, such as `Warehouse::PUSH`: what it returns has
/// the type the method returns, and its calls give the arguments the method
/// would have received.
pub struct TypedFake<'a, A, R> {
    fake: &'a FakeMethod,
    types: PhantomData<fn(A) -> R>,
}

impl<A, R> Clone for TypedFake<'_, A, R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<A, R> Copy for TypedFake<'_, A, R> {}

impl<A, R> TypedFake<'_, A, R> {
    pub fn called(&self) -> bool {
        self.fake.called()
    }

    pub fn call_count(&self) -> usize {
        self.fake.call_count()
    }

    /// Makes every call fail.
    pub fn rejects(&self, error: impl Into<String>) {
        self.fake.rejects(error);
    }

    /// Forgets recorded calls and every configured behavior.
    pub fn reset(&self) {
        self.fake.reset();
    }
}

impl<A, R: Serialize> TypedFake<'_, A, R> {
    /// Value returned by every call.
    pub fn returns(&self, value: R) {
        self.fake.returns(value);
    }
}

impl<A: DeserializeOwned, R> TypedFake<'_, A, R> {
    fn typed(call: FakeCall) -> TypedCall<A> {
        let args = serde_json::from_value(call.args)
            .unwrap_or_else(|error| panic!("the recorded arguments are not those of the method: {error}"));
        TypedCall {
            args,
            returned_value: call.returned_value,
            thrown_error: call.thrown_error,
            timestamp: call.timestamp,
        }
    }

    /// The recorded calls, oldest first. Panics when a call was made with
    /// arguments the method does not take, which only an untyped call can do.
    pub fn calls(&self) -> Vec<TypedCall<A>> {
        self.fake.calls().into_iter().map(Self::typed).collect()
    }

    pub fn last_call(&self) -> Option<TypedCall<A>> {
        self.fake.last_call().map(Self::typed)
    }
}

/// One [`FakeMethod`] per method of every configured provider.
#[derive(Clone, Default)]
pub struct FakeProviders {
    providers: IndexMap<String, IndexMap<String, FakeMethod>>,
}

impl FakeProviders {
    /// One fake per method of each provider.
    pub fn new(providers: &IndexMap<String, Provider>) -> Self {
        Self {
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

    /// The fake of `provider.method`. Panics when the provider or method is not configured.
    pub fn method(&self, provider: &str, method: &str) -> &FakeMethod {
        self.providers
            .get(provider)
            .and_then(|methods| methods.get(method))
            .unwrap_or_else(|| panic!("no fake for provider method \"{provider}.{method}\""))
    }

    /// The fake of a typed call, `ctx.fakes.for_call(Warehouse::PUSH)`.
    /// Panics when the provider or method is not configured.
    pub fn for_call<A, R>(&self, call: ProviderCall<A, R>) -> TypedFake<'_, A, R> {
        TypedFake {
            fake: self.method(call.provider(), call.method()),
            types: PhantomData,
        }
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
                                Box::pin(async move { fake.call(args) })
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn records_calls_and_returns_null_by_default() {
        let fake = FakeMethod::default();
        assert!(!fake.called());
        assert_eq!(fake.call(json!([1, 2])).unwrap(), Value::Null);
        assert!(fake.called());
        assert_eq!(fake.call_count(), 1);
        let call = fake.last_call().unwrap();
        assert_eq!(call.args, json!([1, 2]));
        assert_eq!(call.returned_value, Some(Value::Null));
        assert_eq!(call.thrown_error, None);
    }

    #[test]
    fn returns_or_rejects_until_reset() {
        let fake = FakeMethod::default();
        fake.returns("default");
        assert_eq!(fake.call(Value::Null).unwrap(), json!("default"));

        fake.rejects("always");
        assert_eq!(fake.call(Value::Null).unwrap_err().to_string(), "always");
        assert_eq!(fake.calls()[1].thrown_error.as_deref(), Some("always"));

        fake.reset();
        assert_eq!(fake.call(Value::Null).unwrap(), Value::Null);
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
        let fakes = FakeProviders::new(&providers);
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
        assert!(connectors.call("warehouse", "missing", Value::Null).await.is_err());

        fakes.reset();
        assert!(!fakes.method("warehouse", "push").called());
    }
}
