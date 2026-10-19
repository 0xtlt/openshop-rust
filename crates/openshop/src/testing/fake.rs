//! Recording fakes for provider methods.

use std::marker::PhantomData;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use indexmap::IndexMap;
use serde::Serialize;
use serde::de::DeserializeOwned;
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
    /// Counts the resets, so that a call still running after one does not write into the calls after it.
    generation: u64,
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
        let mut state = self.state();
        *state = FakeState {
            generation: state.generation + 1,
            ..FakeState::default()
        };
    }

    pub async fn call(&self, args: Value) -> Result<Value> {
        // The call takes its place in the record, and so its index, in the same
        // lock as the rules are read: calls that overlap each get their own.
        let (index, generation, override_result, implementation, default_error, default_return) = {
            let mut state = self.state();
            let index = state.calls.len();
            state.calls.push(FakeCall {
                args: args.clone(),
                returned_value: None,
                thrown_error: None,
                timestamp: Utc::now(),
            });
            (
                index,
                state.generation,
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
            (None, Some(implementation), _) => implementation(args).await.map_err(|error| format!("{error:#}")),
            (None, None, Some(error)) => Err(error),
            (None, None, None) => Ok(default_return),
        };

        {
            let mut state = self.state();
            // A reset in the meantime forgot this call.
            if state.generation == generation
                && let Some(call) = state.calls.get_mut(index)
            {
                match &outcome {
                    Ok(value) => call.returned_value = Some(value.clone()),
                    Err(error) => call.thrown_error = Some(error.clone()),
                }
            }
        }
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

impl<'a, A, R> TypedFake<'a, A, R> {
    /// The fake without the types, to answer with something the method could
    /// not return or to replace its body.
    pub fn untyped(&self) -> &'a FakeMethod {
        self.fake
    }

    pub fn called(&self) -> bool {
        self.fake.called()
    }

    pub fn call_count(&self) -> usize {
        self.fake.call_count()
    }

    /// Makes every call without a more specific rule fail.
    pub fn rejects(&self, error: impl Into<String>) {
        self.fake.rejects(error);
    }

    /// Makes the `n`-th call (zero-based) fail.
    pub fn on_call_rejects(&self, n: usize, error: impl Into<String>) {
        self.fake.on_call_rejects(n, error);
    }

    /// Forgets recorded calls and every configured behavior.
    pub fn reset(&self) {
        self.fake.reset();
    }
}

impl<A, R: Serialize> TypedFake<'_, A, R> {
    /// Value returned by every call without a more specific rule.
    pub fn returns(&self, value: R) {
        self.fake.returns(value);
    }

    /// Value returned by the `n`-th call (zero-based).
    pub fn on_call_returns(&self, n: usize, value: R) {
        self.fake.on_call_returns(n, value);
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

    pub fn get(&self, provider: &str, method: &str) -> Option<&FakeMethod> {
        self.providers.get(provider)?.get(method)
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
    async fn concurrent_calls_each_take_their_own_index() {
        let fake = FakeMethod::default();
        fake.on_call_returns(1, "second");
        fake.implementation(|args| async move {
            // The first call is still running when the second one starts.
            tokio::time::sleep(std::time::Duration::from_millis(if args == json!("a") { 50 } else { 0 })).await;
            Ok(json!("implementation"))
        });

        let (first, second) = tokio::join!(fake.call(json!("a")), fake.call(json!("b")));
        assert_eq!(first.unwrap(), json!("implementation"));
        assert_eq!(second.unwrap(), json!("second"));

        // The calls are in the order they started in, each with its own result.
        let calls = fake.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!((&calls[0].args, &calls[0].returned_value), (&json!("a"), &Some(json!("implementation"))));
        assert_eq!((&calls[1].args, &calls[1].returned_value), (&json!("b"), &Some(json!("second"))));
    }

    #[tokio::test]
    async fn a_reset_during_a_call_does_not_touch_the_calls_after_it() {
        let fake = FakeMethod::default();
        fake.implementation(|_| async {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            Ok(json!("late"))
        });
        let running = fake.call(json!("old"));
        let reset = async {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            fake.reset();
            fake.returns("new");
            fake.call(json!("fresh")).await.unwrap();
        };
        let (late, ()) = tokio::join!(running, reset);
        assert_eq!(late.unwrap(), json!("late"));

        let calls = fake.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!((&calls[0].args, &calls[0].returned_value), (&json!("fresh"), &Some(json!("new"))));
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
        assert!(fakes.get("warehouse", "missing").is_none());
        assert!(connectors.call("warehouse", "missing", Value::Null).await.is_err());

        fakes.reset();
        assert!(!fakes.method("warehouse", "push").called());
    }
}
