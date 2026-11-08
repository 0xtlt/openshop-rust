//! The Admin API of a test, answered by the name of each GraphQL operation.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::{Value, json};

use super::transport::{OutboundRequest, OutboundResponse};
use crate::billing::{ACTIVE_SUBSCRIPTIONS_OPERATION, AppSubscription, SubscriptionStatus};
use crate::locked;

/// A GraphQL request the application sent to the Admin API.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphqlCall {
    /// The name of the operation: its `operationName`, else the name its
    /// text gives it (`query CurrentFunctions { … }`). `None` for an
    /// anonymous operation, `{ shop { name } }`.
    pub operation: Option<String>,
    /// The text of the operation.
    pub query: String,
    /// Its variables: an object, empty when it has none.
    pub variables: Value,
    /// The address it was sent to, `…/admin/api/{version}/graphql.json`.
    pub url: String,
}

impl GraphqlCall {
    /// The GraphQL request of an outbound request: a `POST` to a
    /// `graphql.json` path whose body has a `query`.
    pub(crate) fn of(request: &OutboundRequest) -> Option<Self> {
        let path = request.url.split(['?', '#']).next().unwrap_or_default();
        if request.method != "POST" || !path.ends_with("/graphql.json") {
            return None;
        }
        let query = request.body.get("query")?.as_str()?.to_string();
        let operation = request
            .body
            .get("operationName")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| operation_name(&query));
        let variables = match request.body.get("variables") {
            Some(Value::Object(variables)) => Value::Object(variables.clone()),
            _ => json!({}),
        };
        Some(Self {
            operation,
            query,
            variables,
            url: request.url.clone(),
        })
    }

    /// The name of the operation, `(anonymous)` without one.
    fn name(&self) -> &str {
        self.operation.as_deref().unwrap_or(ANONYMOUS)
    }
}

/// How an operation without a name is listed.
const ANONYMOUS: &str = "(anonymous)";

type Respond = Arc<dyn Fn(&GraphqlCall) -> Result<OutboundResponse> + Send + Sync>;

/// What answers an operation, when its variables contain `variables`.
struct Answer {
    operation: String,
    variables: Option<Value>,
    respond: Respond,
}

#[derive(Default)]
struct State {
    /// Oldest first; the newest that fits answers.
    answers: Vec<Answer>,
    calls: Vec<GraphqlCall>,
    /// The operations no answer and no responder took, once each.
    unanswered: Vec<String>,
}

/// The Admin API of a test: answers each GraphQL operation the application
/// sends by its name, and records them. `ctx.shopify` of a
/// [`TestContext`](super::TestContext), or [`MockTransport::shopify`](super::MockTransport::shopify).
///
/// ```
/// # use openshop::json;
/// # use openshop::testing::{TestContext, subscription};
/// # fn answer(ctx: &TestContext) {
/// ctx.shopify
///     .active_subscriptions([subscription("Pro")])
///     .on("CurrentFunctions", json!({ "paymentCustomizations": { "nodes": [] } }))
///     .on_with("CreateCustomization", |call| {
///         assert_eq!(call.variables["input"]["enabled"], true);
///         Ok(json!({ "paymentCustomizationCreate": { "userErrors": [] } }))
///     });
/// # }
/// ```
///
/// An operation it has no answer for goes to the responder of the transport
/// ([`respond_with`](super::MockTransport::respond_with)) when the test set
/// one. Without one, Shopify answers with a GraphQL error that names the
/// operation and the ones answered, which the call fails with; the operation
/// is then listed by [`unanswered`](Self::unanswered). Clones share their
/// answers and calls.
#[derive(Clone, Default)]
pub struct FakeShopify {
    state: Arc<Mutex<State>>,
}

impl FakeShopify {
    fn add(&self, operation: &str, variables: Option<Value>, respond: Respond) -> &Self {
        locked(&self.state).answers.push(Answer {
            operation: operation.to_string(),
            variables,
            respond,
        });
        self
    }

    /// Answers `operation` with `data`: `{ "data": data }`, status `200`. A
    /// later answer of the same operation replaces it.
    pub fn on(&self, operation: &str, data: Value) -> &Self {
        self.add(operation, None, Arc::new(move |_| Ok(data_response(data.clone()))))
    }

    /// Answers `operation` with `data` when its variables contain
    /// `variables`: each key given, with the same value (an object compared
    /// the same way, key by key). An answer with variables that fit is
    /// chosen before one without.
    pub fn on_variables(&self, operation: &str, variables: Value, data: Value) -> &Self {
        self.add(
            operation,
            Some(variables),
            Arc::new(move |_| Ok(data_response(data.clone()))),
        )
    }

    /// Answers `operation` with what `answer` makes of each call: `Ok(data)`
    /// is `{ "data": data }`, an `Err` a GraphQL error with its message. For
    /// a shop whose state the mutations change, or to check the variables.
    pub fn on_with(
        &self,
        operation: &str,
        answer: impl Fn(&GraphqlCall) -> Result<Value> + Send + Sync + 'static,
    ) -> &Self {
        self.add(
            operation,
            None,
            Arc::new(move |call| {
                Ok(match answer(call) {
                    Ok(data) => data_response(data),
                    Err(error) => errors_response(&format!("{error:#}")),
                })
            }),
        )
    }

    /// Answers `operation` with `response` as it is: a status and a body, such
    /// as `MockTransport::json(503, json!({}))` for a Shopify that fails, or
    /// `json!({ "errors": … })`.
    pub fn respond(&self, operation: &str, response: OutboundResponse) -> &Self {
        self.add(operation, None, Arc::new(move |_| Ok(response.clone())))
    }

    /// Answers `OpenShopActiveSubscriptions`, what
    /// [`ShopifyClient::active_subscriptions`](crate::ShopifyClient::active_subscriptions),
    /// [`billing::require_plan`](crate::billing::require_plan) and
    /// [`billing::any_plan`](crate::billing::any_plan) ask, with these
    /// subscriptions; none is a shop without a plan. See [`subscription`].
    pub fn active_subscriptions(&self, subscriptions: impl IntoIterator<Item = AppSubscription>) -> &Self {
        let subscriptions: Vec<AppSubscription> = subscriptions.into_iter().collect();
        self.on(
            ACTIVE_SUBSCRIPTIONS_OPERATION,
            crate::billing::active_subscriptions_data(&subscriptions),
        )
    }

    /// Every GraphQL request sent so far, answered or not, oldest first.
    pub fn calls(&self) -> Vec<GraphqlCall> {
        locked(&self.state).calls.clone()
    }

    /// The calls of one operation, oldest first.
    pub fn calls_to(&self, operation: &str) -> Vec<GraphqlCall> {
        self.calls()
            .into_iter()
            .filter(|call| call.operation.as_deref() == Some(operation))
            .collect()
    }

    /// The names of the operations sent, in order: `(anonymous)` for one
    /// without a name.
    pub fn operations(&self) -> Vec<String> {
        locked(&self.state).calls.iter().map(|call| call.name().to_string()).collect()
    }

    /// The operations Shopify was sent that nothing answered, once each, in
    /// the order they were first sent.
    pub fn unanswered(&self) -> Vec<String> {
        locked(&self.state).unanswered.clone()
    }

    /// Panics when an operation was sent that nothing answered, naming them:
    /// for code that would not fail on the error it got.
    pub fn assert_answered(&self) {
        let unanswered = self.unanswered();
        assert!(
            unanswered.is_empty(),
            "[openshop] The Admin API was sent operations the test does not answer: {}. Answer them with `ctx.shopify.on(\"{}\", data)`.",
            unanswered.join(", "),
            unanswered[0],
        );
    }

    /// Forgets the answers and the calls.
    pub fn reset(&self) {
        *locked(&self.state) = State::default();
    }

    /// Records `call`, and answers it when an answer fits. `None` leaves it
    /// to the responder of the transport.
    pub(crate) fn answer(&self, call: &GraphqlCall) -> Option<Result<OutboundResponse>> {
        let respond = {
            let mut state = locked(&self.state);
            state.calls.push(call.clone());
            let name = call.name();
            let fits = |answer: &&Answer, with_variables: bool| {
                answer.operation == name
                    && match &answer.variables {
                        Some(variables) => with_variables && contains(&call.variables, variables),
                        None => !with_variables,
                    }
            };
            state
                .answers
                .iter()
                .rev()
                .find(|answer| fits(answer, true))
                .or_else(|| state.answers.iter().rev().find(|answer| fits(answer, false)))
                .map(|answer| answer.respond.clone())
        };
        // Outside the lock: an answer may read the calls.
        respond.map(|respond| respond(call))
    }

    /// What Shopify answers `call` when nothing does: a GraphQL error that
    /// names it and the operations that are answered.
    pub(crate) fn unanswered_response(&self, call: &GraphqlCall) -> OutboundResponse {
        let mut state = locked(&self.state);
        let name = call.name().to_string();
        if !state.unanswered.contains(&name) {
            state.unanswered.push(name.clone());
        }
        let mut answered: Vec<&str> = state.answers.iter().map(|answer| answer.operation.as_str()).collect();
        answered.sort_unstable();
        answered.dedup();
        let answered = if answered.is_empty() {
            "none".to_string()
        } else {
            answered.join(", ")
        };
        errors_response(&format!(
            "[openshop] The test does not answer the Admin API operation `{name}`: answer it with \
             `ctx.shopify.on(\"{name}\", data)`. Answered: {answered}."
        ))
    }
}

impl std::fmt::Debug for FakeShopify {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = locked(&self.state);
        let answered: Vec<&str> = state.answers.iter().map(|answer| answer.operation.as_str()).collect();
        f.debug_struct("FakeShopify")
            .field("answered", &answered)
            .field("calls", &state.calls.len())
            .field("unanswered", &state.unanswered)
            .finish()
    }
}

fn data_response(data: Value) -> OutboundResponse {
    OutboundResponse {
        status: 200,
        body: json!({ "data": data }).to_string(),
    }
}

fn errors_response(message: &str) -> OutboundResponse {
    OutboundResponse {
        status: 200,
        body: json!({ "errors": [{ "message": message }] }).to_string(),
    }
}

/// Whether `value` holds `pattern`: an object each key of the pattern with a
/// value that holds the pattern's, anything else the same value.
fn contains(value: &Value, pattern: &Value) -> bool {
    match (value, pattern) {
        (Value::Object(value), Value::Object(pattern)) => pattern
            .iter()
            .all(|(key, expected)| value.get(key).is_some_and(|given| contains(given, expected))),
        _ => value == pattern,
    }
}

/// An active test subscription to the plan `name`, as
/// `activeSubscriptions` lists one: `gid://shopify/AppSubscription/1`, no
/// trial, created now, without lines. Its fields are public: change what the
/// test needs, `AppSubscription { status: SubscriptionStatus::Frozen, ..subscription("Pro") }`,
/// or give it a line with the handle of its plan.
pub fn subscription(name: &str) -> AppSubscription {
    AppSubscription {
        id: "gid://shopify/AppSubscription/1".to_string(),
        name: name.to_string(),
        status: SubscriptionStatus::Active,
        test: true,
        trial_days: 0,
        created_at: chrono::Utc::now(),
        current_period_end: None,
        line_items: Vec::new(),
    }
}

/// The name of the first operation of a GraphQL document: `Name` of
/// `query Name(…) { … }`, of a `mutation` or a `subscription`. `None` when
/// it has none, `{ shop { name } }` or `query($id: ID!) { … }`. Comments,
/// strings and the fragments before it are skipped.
pub(crate) fn operation_name(document: &str) -> Option<String> {
    let mut tokens = Tokens { rest: document };
    loop {
        match tokens.next()? {
            Token::Name("query" | "mutation" | "subscription") => {
                return match tokens.next()? {
                    Token::Name(name) => Some(name.to_string()),
                    _ => None,
                };
            }
            // A selection set at the top: an anonymous query.
            Token::Punctuator('{') => return None,
            // `fragment Name on Type { … }`: skipped up to its selection set.
            Token::Name("fragment") => loop {
                match tokens.next()? {
                    Token::Punctuator('{') => {
                        tokens.skip_selection_set()?;
                        break;
                    }
                    _ => continue,
                }
            },
            _ => return None,
        }
    }
}

enum Token<'a> {
    Name(&'a str),
    Punctuator(char),
    /// A string, a number or anything else that is neither.
    Other,
}

struct Tokens<'a> {
    rest: &'a str,
}

impl<'a> Tokens<'a> {
    /// The next token, past the white space, the commas and the comments.
    fn next(&mut self) -> Option<Token<'a>> {
        loop {
            let trimmed = self.rest.trim_start_matches(|c: char| c.is_whitespace() || c == ',' || c == '\u{feff}');
            if let Some(comment) = trimmed.strip_prefix('#') {
                self.rest = comment.find('\n').map_or("", |end| &comment[end..]);
                continue;
            }
            self.rest = trimmed;
            break;
        }
        let first = self.rest.chars().next()?;
        if first == '_' || first.is_ascii_alphabetic() {
            let end = self
                .rest
                .find(|c: char| !(c == '_' || c.is_ascii_alphanumeric()))
                .unwrap_or(self.rest.len());
            let (name, rest) = self.rest.split_at(end);
            self.rest = rest;
            return Some(Token::Name(name));
        }
        if first == '"' {
            self.skip_string();
            return Some(Token::Other);
        }
        self.rest = &self.rest[first.len_utf8()..];
        Some(if "{}()[]:!$@=|&".contains(first) {
            Token::Punctuator(first)
        } else {
            Token::Other
        })
    }

    /// Past a string: `"…"` with its escapes, or a block string `"""…"""`.
    fn skip_string(&mut self) {
        if let Some(block) = self.rest.strip_prefix("\"\"\"") {
            let mut index = 0;
            while let Some(found) = block[index..].find("\"\"\"") {
                let at = index + found;
                if block[..at].ends_with('\\') {
                    index = at + 3;
                    continue;
                }
                self.rest = &block[at + 3..];
                return;
            }
            self.rest = "";
            return;
        }
        let mut escaped = false;
        for (index, c) in self.rest.char_indices().skip(1) {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => {
                    self.rest = &self.rest[index + 1..];
                    return;
                }
                _ => {}
            }
        }
        self.rest = "";
    }

    /// Past the selection set whose `{` was just read.
    fn skip_selection_set(&mut self) -> Option<()> {
        let mut depth = 1usize;
        while depth > 0 {
            match self.next()? {
                Token::Punctuator('{') => depth += 1,
                Token::Punctuator('}') => depth -= 1,
                _ => {}
            }
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_operation_of_a_document() {
        for (document, name) in [
            ("query Shop { shop { name } }", Some("Shop")),
            ("#graphql\n    query ListShopifyFunctions($after: String) {\n x }", Some("ListShopifyFunctions")),
            ("mutation CreateThing($input: X!) { a }", Some("CreateThing")),
            ("subscription Changes { a }", Some("Changes")),
            ("  # a comment { query Not }\n query Real { a }", Some("Real")),
            ("fragment F on Shop { name { x } } query AfterFragment { shop { ...F } }", Some("AfterFragment")),
            ("\u{feff}query WithBom { a }", Some("WithBom")),
            ("query($id: ID!) { node(id: $id) { id } }", None),
            ("query { shop { name } }", None),
            ("{ shop { name } }", None),
            ("", None),
            ("not graphql", None),
            (
                "fragment F on Shop { description(arg: \"}\") a: b(x: \"\"\" } \\\"\"\" ok \"\"\") } mutation M { a }",
                Some("M"),
            ),
        ] {
            assert_eq!(operation_name(document).as_deref(), name, "{document}");
        }
    }

    #[test]
    fn variables_are_contained_key_by_key() {
        let given = json!({ "id": "gid://1", "input": { "enabled": true, "title": "T" }, "first": 10 });
        assert!(contains(&given, &json!({})));
        assert!(contains(&given, &json!({ "id": "gid://1" })));
        assert!(contains(&given, &json!({ "input": { "enabled": true } })));
        assert!(!contains(&given, &json!({ "input": { "enabled": false } })));
        assert!(!contains(&given, &json!({ "missing": null })));
        assert!(!contains(&given, &json!({ "first": "10" })));
        assert!(contains(&json!([1, 2]), &json!([1, 2])));
        assert!(!contains(&json!([1, 2]), &json!([1])));
    }

    fn request(body: Value) -> OutboundRequest {
        OutboundRequest {
            method: "POST",
            url: "https://test.myshopify.com/admin/api/2026-10/graphql.json".to_string(),
            headers: Vec::new(),
            body,
        }
    }

    fn data(response: Option<Result<OutboundResponse>>) -> Value {
        let response = response.expect("an answer").expect("a response");
        serde_json::from_str::<Value>(&response.body).unwrap()["data"].clone()
    }

    #[test]
    fn answers_by_operation_then_by_variables() {
        let shopify = FakeShopify::default();
        shopify
            .on("Product", json!({ "product": "any" }))
            .on_variables("Product", json!({ "id": "gid://2" }), json!({ "product": "second" }))
            .on_with("Count", |call| match call.variables["fail"].as_bool() {
                Some(true) => anyhow::bail!("refused"),
                _ => Ok(json!({ "count": 3 })),
            });

        let call = |body| GraphqlCall::of(&request(body)).unwrap();
        let product = |id: &str| call(json!({ "query": "query Product($id: ID!) { product(id: $id) { id } }", "variables": { "id": id } }));
        assert_eq!(data(shopify.answer(&product("gid://1"))), json!({ "product": "any" }));
        assert_eq!(data(shopify.answer(&product("gid://2"))), json!({ "product": "second" }));
        // The operation name of the body wins over the text.
        let named = call(json!({ "query": "query Other { x }", "operationName": "Count" }));
        assert_eq!(data(shopify.answer(&named)), json!({ "count": 3 }));
        let refused = shopify
            .answer(&call(json!({ "query": "query Count { x }", "variables": { "fail": true } })))
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&refused.body).unwrap(),
            json!({ "errors": [{ "message": "refused" }] })
        );
        // A later answer replaces an earlier one.
        shopify.on("Product", json!({ "product": "newer" }));
        assert_eq!(data(shopify.answer(&product("gid://1"))), json!({ "product": "newer" }));

        let anonymous = call(json!({ "query": "{ shop { name } }" }));
        assert!(shopify.answer(&anonymous).is_none());
        let missing = shopify.unanswered_response(&anonymous);
        let message = serde_json::from_str::<Value>(&missing.body).unwrap()["errors"][0]["message"].clone();
        assert_eq!(
            message,
            "[openshop] The test does not answer the Admin API operation `(anonymous)`: answer it with \
             `ctx.shopify.on(\"(anonymous)\", data)`. Answered: Count, Product."
        );
        assert_eq!(shopify.unanswered(), ["(anonymous)"]);
        assert_eq!(
            shopify.operations(),
            ["Product", "Product", "Count", "Count", "Product", "(anonymous)"]
        );
        assert_eq!(shopify.calls_to("Count").len(), 2);
        assert_eq!(shopify.calls_to("Count")[1].variables, json!({ "fail": true }));

        shopify.reset();
        assert!(shopify.calls().is_empty() && shopify.unanswered().is_empty());
        assert!(shopify.answer(&product("gid://1")).is_none());
    }

    #[test]
    #[should_panic(expected = "operations the test does not answer: Missing")]
    fn assert_answered_names_what_was_not() {
        let shopify = FakeShopify::default();
        let call = GraphqlCall::of(&request(json!({ "query": "query Missing { a }" }))).unwrap();
        assert!(shopify.answer(&call).is_none());
        shopify.unanswered_response(&call);
        shopify.assert_answered();
    }

    #[test]
    fn only_a_graphql_request_is_a_call() {
        assert!(GraphqlCall::of(&request(json!({ "query": "query A { a }" }))).is_some());
        let mut token = request(json!({ "grant_type": "client_credentials" }));
        token.url = "https://test.myshopify.com/admin/oauth/access_token".to_string();
        assert!(GraphqlCall::of(&token).is_none());
        let mut get = request(json!({ "query": "query A { a }" }));
        get.method = "GET";
        assert!(GraphqlCall::of(&get).is_none());
        assert!(GraphqlCall::of(&request(json!({ "query": 1 }))).is_none());
        let call = GraphqlCall::of(&request(json!({ "query": "query A { a }", "variables": null }))).unwrap();
        assert_eq!(call.variables, json!({}));
    }
}
