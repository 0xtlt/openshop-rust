//! The builders, defaults, parsing and validation of the definitions of `types`.
//!
//! Nothing here needs a database: the contexts that carry a pool are not built.

use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderValue, Method};
use serde_json::json;

use super::*;

fn map(value: Value) -> JsonMap {
    value.as_object().cloned().unwrap()
}

#[test]
fn a_missing_value_is_absent_null_or_empty() {
    assert!(is_missing(None));
    assert!(is_missing(Some(&Value::Null)));
    assert!(is_missing(Some(&json!(""))));
    for present in [json!(" "), json!(0), json!(false), json!([]), json!({})] {
        assert!(!is_missing(Some(&present)), "{present}");
    }
}

// ─── Provider ───────────────────────────────────────────────────────

#[test]
fn field_types_are_named_as_they_serialize() {
    for field_type in [
        FieldType::Text,
        FieldType::Password,
        FieldType::Number,
        FieldType::Select,
        FieldType::Checkbox,
    ] {
        assert_eq!(serde_json::to_value(field_type).unwrap(), json!(field_type.as_str()));
        assert_eq!(
            serde_json::from_value::<FieldType>(json!(field_type.as_str())).unwrap(),
            field_type
        );
    }
}

#[test]
fn a_field_is_required_unless_it_says_otherwise() {
    assert!(Field::text("Name").is_required());
    assert!(Field::text("Name").required(true).is_required());
    assert!(!Field::text("Name").required(false).is_required());
    assert!(!Field::text("Name").optional().is_required());
    assert_eq!(Field::text("Name").required, None);
    assert_eq!(Field::text("Name").optional().required, Some(false));
}

#[test]
fn the_metadata_of_a_field_leaves_out_what_is_not_set_and_the_validator() {
    let plain = Field::password("Token").metadata();
    assert_eq!(Value::Object(plain), json!({ "type": "password", "label": "Token" }));

    let select = Field::select("Mode", [FieldOption::new("Live", "live"), FieldOption::new("Test", "test")])
        .placeholder("Pick one")
        .description("Live charges real cards")
        .optional()
        .validate(|value| Ok(value.clone()));
    assert_eq!(
        Value::Object(select.metadata()),
        json!({
            "type": "select",
            "label": "Mode",
            "placeholder": "Pick one",
            "description": "Live charges real cards",
            "options": [{ "label": "Live", "value": "live" }, { "label": "Test", "value": "test" }],
            "required": false,
        })
    );
    // The closure cannot be printed: the debug output only says there is one.
    assert!(format!("{select:?}").contains("validate: Some(\"…\")"), "{select:?}");
    assert!(format!("{:?}", Field::text("Name")).contains("validate: None"));
}

#[test]
fn validators_chain_on_the_value_the_previous_one_returned() {
    let field = Field::text("Url")
        .validate(|value| Ok(json!(value.as_str().unwrap_or_default().trim())))
        .validate(|value| match value.as_str() {
            Some("") => Err("must not be blank".to_string()),
            Some(text) => Ok(json!(text.to_lowercase())),
            None => Err("must be text".to_string()),
        });
    let validate = field.validate.as_ref().unwrap();
    assert_eq!(validate(&json!("  Hello ")).unwrap(), json!("hello"));
    // The second validator sees the trimmed value.
    assert_eq!(validate(&json!("   ")).unwrap_err(), "must not be blank");
}

#[test]
fn rust_types_become_the_fields_of_a_provider_form() {
    let text = String::field("Name");
    assert_eq!(
        (text.field_type, text.is_required(), text.validate.is_none()),
        (FieldType::Text, true, true)
    );
    assert_eq!(bool::field("On").field_type, FieldType::Checkbox);
    assert_eq!(f64::field("Ratio").field_type, FieldType::Number);
    assert!(f64::field("Ratio").validate.is_none());

    let optional = Option::<String>::field("Note");
    assert_eq!((optional.field_type, optional.is_required()), (FieldType::Text, false));
    const { assert!(Option::<String>::OPTIONAL && !String::OPTIONAL) };

    // The form refuses what the type cannot hold.
    let signed = i32::field("Count");
    let validate = signed.validate.as_ref().unwrap();
    assert_eq!(validate(&json!(-3)).unwrap(), json!(-3));
    assert!(validate(&json!(1.5)).unwrap_err().contains("must be an integer"));
    assert!(validate(&json!("3")).unwrap_err().contains("must be a number"));

    let unsigned = u8::field("Retries");
    let validate = unsigned.validate.as_ref().unwrap();
    assert_eq!(validate(&json!(0)).unwrap(), json!(0));
    assert!(validate(&json!(-1)).unwrap_err().contains("must be at least 0"));
}

#[test]
fn a_field_value_is_taken_out_of_the_configuration() {
    let mut config = map(json!({ "name": "shop", "count": "many", "empty": null }));
    assert_eq!(provider_field_value::<String>(&mut config, "name").unwrap(), "shop");
    assert!(!config.contains_key("name"));

    assert_eq!(
        provider_field_value::<String>(&mut config, "absent").unwrap_err(),
        "missing field `absent`"
    );
    // A null is as missing as an absent key.
    assert_eq!(
        provider_field_value::<String>(&mut config, "empty").unwrap_err(),
        "missing field `empty`"
    );
    let error = provider_field_value::<u32>(&mut config, "count").unwrap_err();
    assert!(error.starts_with("field `count`: "), "{error}");
    // An optional field reads a missing value as `None`.
    assert_eq!(provider_field_value::<Option<u32>>(&mut config, "absent").unwrap(), None);
}

#[derive(Deserialize)]
struct WarehouseConfig {
    token: String,
}

const PUSH: ProviderCall<Vec<String>, usize> = ProviderCall::new("warehouse", "push");

#[tokio::test]
async fn a_typed_method_reads_its_arguments_and_returns_json() {
    let provider =
        Provider::new("warehouse").typed_method("push", |_config, args: Vec<String>| async move { Ok(args.len()) });
    let push = &provider.methods["push"];
    assert_eq!(push(JsonMap::new(), json!(["a", "b"])).await.unwrap(), json!(2));

    let error = push(JsonMap::new(), json!("not a list")).await.unwrap_err().to_string();
    assert!(error.starts_with("Invalid arguments for \"push\": "), "{error}");
}

#[tokio::test]
async fn a_handled_call_reads_the_configuration_and_the_arguments() {
    let provider = Provider::new("warehouse").handle(PUSH, |config: WarehouseConfig, args| async move {
        Ok(config.token.len() + args.len())
    });
    assert_eq!(provider.typed_calls, vec![("warehouse", "push")]);

    let push = &provider.methods["push"];
    assert_eq!(
        push(map(json!({ "token": "abc" })), json!(["x"])).await.unwrap(),
        json!(4)
    );

    let error = push(JsonMap::new(), json!(["x"])).await.unwrap_err().to_string();
    assert!(
        error.starts_with("Invalid configuration for provider \"warehouse\": "),
        "{error}"
    );
    let error = push(map(json!({ "token": "abc" })), json!(1))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.starts_with("Invalid arguments for \"warehouse.push\": "), "{error}");
}

#[tokio::test]
async fn a_typed_checker_fails_on_a_configuration_that_does_not_fit() {
    let provider =
        Provider::new("warehouse").typed_checker(|config: WarehouseConfig| async move { Ok(config.token == "good") });
    let checker = provider.checker.as_ref().unwrap();
    assert!(checker(map(json!({ "token": "good" }))).await.unwrap());
    assert!(!checker(map(json!({ "token": "bad" }))).await.unwrap());
    let error = checker(JsonMap::new()).await.unwrap_err().to_string();
    assert!(
        error.starts_with("Invalid configuration for provider \"warehouse\": "),
        "{error}"
    );
}

#[test]
fn a_provider_gathers_its_fields_in_order() {
    let provider = Provider::new("warehouse")
        .field("url", Field::text("Url"))
        .fields(IndexMap::from([("token".to_string(), Field::password("Token"))]))
        .field("url", Field::text("Endpoint"));
    // A field declared again replaces the first, keeping its place.
    assert_eq!(provider.fields.keys().collect::<Vec<_>>(), ["url", "token"]);
    assert_eq!(provider.fields["url"].label, "Endpoint");
}

#[test]
fn a_provider_call_is_a_copyable_name() {
    let copy = PUSH;
    assert_eq!((copy.provider(), copy.method()), ("warehouse", "push"));
    assert_eq!(
        format!("{PUSH:?}"),
        "ProviderCall { provider: \"warehouse\", method: \"push\" }"
    );
}

// ─── Flow ───────────────────────────────────────────────────────────

#[test]
fn the_open_schema_accepts_any_input() {
    let schema = InputSchema::any();
    assert_eq!(schema.json_schema, json!({}));
    for value in [json!(null), json!(1), json!({ "a": [1] })] {
        assert_eq!(schema.parse(value.clone()).unwrap(), value);
    }
    // The parser cannot be printed.
    assert_eq!(format!("{schema:?}"), "InputSchema { json_schema: Object {}, .. }");
}

#[test]
fn a_custom_schema_returns_what_its_parser_returns() {
    let schema = InputSchema::custom(json!({ "type": "integer" }), |value| match value.as_i64() {
        Some(number) => Ok(json!(number * 2)),
        None => Err("must be an integer".to_string()),
    });
    assert_eq!(schema.parse(json!(21)).unwrap(), json!(42));
    assert_eq!(schema.parse(json!("x")).unwrap_err(), "must be an integer");
}

#[test]
fn the_schema_of_a_type_checks_the_input_and_deserializes_it() {
    #[derive(Deserialize, schemars::JsonSchema)]
    #[allow(dead_code)]
    struct Input {
        count: u32,
        note: Option<String>,
    }
    let schema = InputSchema::of::<Input>();
    assert_eq!(schema.json_schema["type"], json!("object"));
    assert_eq!(schema.json_schema["required"], json!(["count"]));

    let good = json!({ "count": 3 });
    assert_eq!(schema.parse(good.clone()).unwrap(), good);
    assert!(schema.parse(json!({})).is_err());
    assert!(schema.parse(json!({ "count": "three" })).is_err());
    assert!(schema.parse(json!({ "count": -1 })).is_err());
}

#[test]
fn a_flow_starts_without_options_and_a_concurrency_that_rejects() {
    let flow = Flow::new("sync", |_ctx| async { Ok(()) });
    assert_eq!(flow.name, "sync");
    assert!(flow.input.is_none() && flow.timeout.is_none() && flow.step_timeout.is_none());
    assert_eq!(flow.concurrency, None);
    assert!(flow.retry_policy.is_empty());
    assert_eq!(Concurrency::default(), Concurrency::Reject);

    let flow = flow
        .timeout(Duration::from_secs(60))
        .step_timeout(Duration::from_secs(5))
        .concurrency(Concurrency::Allow)
        .retry_policy(RetryPolicy::new().max_attempts(1));
    assert_eq!(
        (flow.timeout, flow.step_timeout, flow.concurrency),
        (
            Some(Duration::from_secs(60)),
            Some(Duration::from_secs(5)),
            Some(Concurrency::Allow)
        )
    );
    assert_eq!(flow.retry_policy.max_attempts, Some(1));
    assert_eq!(
        StepOptions::timeout(Duration::from_secs(2)).timeout,
        Some(Duration::from_secs(2))
    );
    assert_eq!(StepOptions::default().timeout, None);
}

// ─── Cron ───────────────────────────────────────────────────────────

#[test]
fn a_cron_runs_once_for_no_shop_unless_told_otherwise() {
    let cron = Cron::new("0 * * * *", "sync");
    assert_eq!(cron.key(), "sync:0 * * * *");
    assert_eq!(cron.shops, CronShops::Global);
    assert_eq!(cron.shops_json(), json!("global"));
    assert_eq!(CronShops::default(), CronShops::Global);
}

#[test]
fn a_cron_names_the_shops_it_runs_for() {
    let cron = Cron::new("* * * * *", "sync");
    assert_eq!(cron.clone().all_shops().shops_json(), json!("all"));
    assert_eq!(
        cron.clone().shop("a.myshopify.com").shops_json(),
        json!("a.myshopify.com")
    );
    assert_eq!(
        cron.clone().shops(["a.myshopify.com", "b.myshopify.com"]).shops_json(),
        json!(["a.myshopify.com", "b.myshopify.com"])
    );
    // The last choice wins.
    assert_eq!(
        cron.clone().all_shops().shop("a.myshopify.com").shops,
        CronShops::Shop("a.myshopify.com".into())
    );

    let cron = cron.name("hourly").input(json!({ "full": true }));
    assert_eq!(
        (cron.name.as_deref(), cron.input),
        (Some("hourly"), Some(json!({ "full": true })))
    );
}

// ─── Shopify config ─────────────────────────────────────────────────

#[test]
fn a_shopify_app_gives_its_secret_and_url_whatever_its_kind() {
    let toml = ShopifyAppConfig::Toml {
        toml: "shopify.app.toml".into(),
        api_secret: "toml-secret".into(),
        app_url: Some("https://toml.example".into()),
    };
    let keys = ShopifyAppConfig::Keys {
        api_key: "key".into(),
        api_secret: "keys-secret".into(),
        app_url: None,
    };
    assert_eq!(
        (toml.api_secret(), toml.app_url()),
        ("toml-secret", Some("https://toml.example"))
    );
    assert_eq!((keys.api_secret(), keys.app_url()), ("keys-secret", None));
}

// ─── Shopify Function ───────────────────────────────────────────────

#[test]
fn function_types_are_named_as_they_serialize() {
    for function_type in [
        FunctionType::Discount,
        FunctionType::CartTransform,
        FunctionType::DeliveryCustomization,
        FunctionType::PaymentCustomization,
        FunctionType::CheckoutValidation,
        FunctionType::FulfillmentConstraints,
    ] {
        assert_eq!(
            serde_json::to_value(function_type).unwrap(),
            json!(function_type.as_str())
        );
    }
    assert_eq!(FunctionType::CartTransform.as_str(), "cart-transform");
}

#[test]
fn discount_modes_parse_their_names_and_nothing_else() {
    for mode in [DiscountMode::Automatic, DiscountMode::Code] {
        assert_eq!(DiscountMode::parse(mode.as_str()), Some(mode));
        assert_eq!(serde_json::to_value(mode).unwrap(), json!(mode.as_str()));
    }
    for other in ["", "Automatic", "CODE", " code", "manual"] {
        assert_eq!(DiscountMode::parse(other), None, "{other:?}");
    }
}

#[test]
fn a_function_defaults_to_the_automatic_mode() {
    let function = Function::new(FunctionType::Discount, "volume");
    assert_eq!(function.first_mode(), DiscountMode::Automatic);
    assert_eq!(function.clone().modes([]).first_mode(), DiscountMode::Automatic);
    assert_eq!(
        function
            .clone()
            .modes([DiscountMode::Code, DiscountMode::Automatic])
            .first_mode(),
        DiscountMode::Code
    );

    let function = function.label("Volume").field("min", Field::number("Minimum"));
    assert_eq!((function.label.as_deref(), function.config.len()), (Some("Volume"), 1));
}

#[test]
fn combining_discounts_serialize_only_what_is_set() {
    assert_eq!(serde_json::to_value(CombinesWith::default()).unwrap(), json!({}));
    let combines = CombinesWith {
        product_discounts: Some(true),
        shipping_discounts: Some(false),
        ..CombinesWith::default()
    };
    assert_eq!(
        serde_json::to_value(combines).unwrap(),
        json!({ "productDiscounts": true, "shippingDiscounts": false })
    );
}

#[test]
fn the_title_of_a_function_owner_is_static_or_computed() {
    let owner = FunctionOwner::new("Volume discount").starts_at(true).enabled(false);
    assert!(matches!(&owner.title, FunctionTitle::Static(title) if title == "Volume discount"));
    assert_eq!(
        (owner.starts_at, owner.ends_at, owner.enabled),
        (Some(true), None, Some(false))
    );

    let owner = FunctionOwner::dynamic_title(|config| format!("{} off", config["percent"]));
    let FunctionTitle::Dynamic(title) = &owner.title else {
        panic!("expected a computed title");
    };
    assert_eq!(title(&map(json!({ "percent": 10 }))), "10 off");
}

// ─── App proxy ──────────────────────────────────────────────────────

#[test]
fn proxy_types_are_named_for_their_wire_form() {
    assert_eq!(ProxyResponseType::default(), ProxyResponseType::Json);
    assert_eq!(
        [ProxyResponseType::Liquid, ProxyResponseType::Json, ProxyResponseType::Html].map(ProxyResponseType::as_str),
        ["liquid", "json", "html"]
    );
    assert_eq!(
        serde_json::to_value(ProxyAuthKind::AppProxyHmac).unwrap(),
        json!("appProxyHmac")
    );
    assert_eq!(
        serde_json::to_value(ProxyAuthKind::CustomerAccountJwt).unwrap(),
        json!("customerAccountJwt")
    );
}

fn proxy_methods(proxy: &Proxy) -> Vec<&'static str> {
    proxy.handlers.keys().copied().collect()
}

#[test]
fn proxy_definitions_of_one_path_merge_when_they_do_not_overlap() {
    let mut proxy = Proxy::default().get(|_ctx| async { Ok(Value::Null) });
    proxy
        .merge(Proxy::default().post(|_ctx| async { Ok(Value::Null) }))
        .unwrap();
    assert_eq!(proxy_methods(&proxy), ["GET", "POST"]);

    // Left unset, the response type is the default one.
    let mut unset = Proxy::default().get(|_ctx| async { Ok(Value::Null) });
    unset
        .merge(Proxy::default().put(|_ctx| async { Ok(Value::Null) }))
        .unwrap();
    assert_eq!(proxy_methods(&unset), ["GET", "PUT"]);
}

#[test]
fn proxy_definitions_that_clash_do_not_merge() {
    let mut proxy = Proxy::default().get(|_ctx| async { Ok(Value::Null) });
    let error = proxy
        .merge(Proxy::default().get(|_ctx| async { Ok(Value::Null) }))
        .unwrap_err();
    assert_eq!(error, "handles GET twice");

    let error = proxy
        .merge(Proxy::new(ProxyResponseType::Liquid).post(|_ctx| async { Ok(Value::Null) }))
        .unwrap_err();
    assert_eq!(error, "answers with both json and liquid; a path has one response type");
    // Nothing was added by the one that failed.
    assert_eq!(proxy_methods(&proxy), ["GET"]);
}

// ─── Public routes ──────────────────────────────────────────────────

#[test]
fn a_shop_is_named_by_its_domain_and_possibly_its_app() {
    let from_str = RouteShopInput::from("a.myshopify.com");
    assert_eq!(from_str, RouteShopInput::new("a.myshopify.com"));
    assert_eq!(from_str.shopify_app, None);
    let owned = "b.myshopify.com".to_string();
    assert_eq!(RouteShopInput::from(&owned), RouteShopInput::from(owned.clone()));
    assert_eq!(
        RouteShopInput::from(owned).shopify_app("main"),
        RouteShopInput {
            shop: "b.myshopify.com".into(),
            shopify_app: Some("main".into())
        }
    );
}

fn request(uri: &str, body: &'static str) -> RouteRequest {
    let mut headers = HeaderMap::new();
    headers.insert("x-key", HeaderValue::from_static("secret"));
    headers.insert("x-binary", HeaderValue::from_bytes(&[0xff]).unwrap());
    RouteRequest {
        method: Method::POST,
        uri: uri.parse().unwrap(),
        headers,
        body: Bytes::from_static(body.as_bytes()),
    }
}

#[test]
fn a_route_request_reads_its_headers_query_and_body() {
    let hook = request("/hook?a=1&a=2&b=%20x&flag", r#"{"n":1}"#);
    assert_eq!(hook.header("x-key"), Some("secret"));
    assert_eq!(hook.header("X-Key"), Some("secret"));
    assert_eq!(hook.header("x-absent"), None);
    // A header that is not text is not there.
    assert_eq!(hook.header("x-binary"), None);

    let query = hook.query();
    assert_eq!(query["a"], "1", "the first value of a repeated key wins");
    assert_eq!((query["b"].as_str(), query["flag"].as_str()), (" x", ""));
    assert!(request("/hook", "").query().is_empty());

    assert_eq!(hook.json::<Value>().unwrap(), json!({ "n": 1 }));
    assert!(request("/hook", "{").json::<Value>().is_err());
    assert_eq!(hook.text(), r#"{"n":1}"#);
}

#[test]
fn a_body_that_is_not_utf8_is_read_lossily() {
    let mut hook = request("/hook", "");
    hook.body = Bytes::from_static(&[b'a', 0xff, b'b']);
    assert_eq!(hook.text(), "a\u{fffd}b");
}

fn ok_handler(_ctx: RouteContext) -> std::future::Ready<Result<axum::response::Response>> {
    std::future::ready(Ok(axum::response::Response::new(axum::body::Body::empty())))
}

#[test]
fn a_route_lists_its_methods_in_the_canonical_order() {
    let route = Route::public()
        .delete(ok_handler)
        .options(ok_handler)
        .post(ok_handler)
        .get(ok_handler);
    assert!(matches!(route.auth, RouteAuth::None));
    assert_eq!(route.allowed_methods(), ["GET", "POST", "DELETE", "OPTIONS"]);
    assert!(Route::public().allowed_methods().is_empty());
}

#[test]
fn routes_of_one_path_merge_when_they_do_not_overlap() {
    let mut route = Route::public().get(ok_handler);
    route.merge(Route::public().put(ok_handler)).unwrap();
    assert_eq!(route.allowed_methods(), ["GET", "PUT"]);

    let error = route.merge(Route::public().get(ok_handler)).unwrap_err();
    assert_eq!(error, "handles GET twice");
    assert_eq!(route.allowed_methods(), ["GET", "PUT"]);
}

#[test]
fn each_method_keeps_the_authentication_it_was_declared_under() {
    let authenticated: Route<String> = Route::authenticated(|_ctx| async { Ok(RouteAuthOutcome::Unauthorized) });
    let authenticated = authenticated
        .get(|_ctx: RouteContext<String>| async {
            Ok(axum::response::Response::new(axum::body::Body::empty()))
        })
        .erased();

    let mut route = Route::public().get(ok_handler);
    assert_eq!(route.merge(authenticated.clone()).unwrap_err(), "handles GET twice");

    let mut route = Route::public().post(ok_handler);
    route.merge(authenticated).unwrap();
    assert!(matches!(route.handlers["POST"].auth, RouteAuth::None));
    assert!(matches!(route.handlers["GET"].auth, RouteAuth::Custom(_)));
}

// ─── MCP ────────────────────────────────────────────────────────────

#[test]
fn mcp_risk_levels_serialize_in_lowercase() {
    assert_eq!(serde_json::to_value(McpRiskLevel::High).unwrap(), json!("high"));
}

#[test]
fn a_permission_serializes_for_the_admin_without_its_key() {
    let permission = McpPermission::new("reviews:read", "Read reviews");
    assert_eq!(
        serde_json::to_value(permission).unwrap(),
        json!({ "label": "Read reviews" })
    );

    let permission = permission
        .description("All of them")
        .group("Reviews")
        .risk(McpRiskLevel::Low);
    assert_eq!(
        serde_json::to_value(permission).unwrap(),
        json!({ "label": "Read reviews", "description": "All of them", "group": "Reviews", "riskLevel": "low" })
    );
}

const READ: McpPermission = McpPermission::new("reviews:read", "Read reviews");
const WRITE: McpPermission = McpPermission::new("reviews:write", "Write reviews").risk(McpRiskLevel::High);

#[test]
fn a_required_permission_is_a_key_or_a_declaration() {
    assert_eq!(RequiredPermission::from("a:b").key(), "a:b");
    assert_eq!(RequiredPermission::from("a:c".to_string()).key(), "a:c");
    assert_eq!(RequiredPermission::from(READ), RequiredPermission::Declared(READ));
    assert_eq!(RequiredPermission::from(&WRITE).key(), "reviews:write");

    let (keys, declared) = required_permissions([RequiredPermission::from("raw:key"), READ.into(), WRITE.into()]);
    assert_eq!(keys, ["raw:key", "reviews:read", "reviews:write"]);
    assert_eq!(declared, [READ, WRITE]);
}

#[test]
fn tools_and_resources_register_the_permissions_they_come_with() {
    let tool = McpTool::new("Lists reviews", |_ctx, _input| async { Ok(Value::Null) })
        .permissions(["raw:key"])
        .permissions([READ])
        .risk(McpRiskLevel::Medium)
        .confirmation_hint("Sure?")
        .input_schema(json!({ "type": "object" }));
    // The last call replaces the permissions of the first.
    assert_eq!(tool.required_permissions, ["reviews:read"]);
    assert_eq!(tool.declared_permissions, [READ]);
    assert_eq!(tool.risk_level, Some(McpRiskLevel::Medium));
    assert_eq!(tool.confirmation_hint.as_deref(), Some("Sure?"));

    let resource = McpResource::new("reviews", |_ctx| async { Ok("text".into()) })
        .description("All reviews")
        .mime_type("text/plain")
        .permissions([WRITE])
        .risk(McpRiskLevel::High);
    assert_eq!(resource.required_permissions, ["reviews:write"]);
    assert_eq!(resource.declared_permissions, [WRITE]);
    assert_eq!(
        McpResourceContent::from("text"),
        McpResourceContent {
            text: "text".into(),
            mime_type: None
        }
    );
}

#[test]
fn the_permissions_of_an_application_gather_those_of_its_capabilities() {
    let config = McpConfig::new()
        .permission(McpPermission::new("reviews:read", "First definition"))
        .permission(McpPermission::new("custom:x", "Custom"))
        .tool(
            "list",
            McpTool::new("Lists", |_ctx, _input| async { Ok(Value::Null) }).permissions([READ, WRITE]),
        )
        .resource(
            "reviews://all",
            McpResource::new("all", |_ctx| async { Ok("x".into()) })
                .permissions([McpPermission::new("docs:read", "Read docs")]),
        );
    let permissions = config.permissions();
    assert_eq!(
        permissions.keys().collect::<Vec<_>>(),
        ["reviews:read", "custom:x", "reviews:write", "docs:read"]
    );
    // The one registered by hand is kept over the one a tool came with.
    assert_eq!(permissions["reviews:read"].label, "First definition");
}

#[test]
fn mcp_is_enabled_unless_it_is_turned_off() {
    assert_eq!(McpConfig::new().enabled, None);
    assert_eq!(McpConfig::disabled().enabled, Some(false));
    assert_eq!(McpConfig::disabled().enabled(true).enabled, Some(true));
}

// ─── Retry & worker ─────────────────────────────────────────────────

#[test]
fn a_stored_retry_policy_is_read_leniently() {
    let policy: ResolvedRetryPolicy = serde_json::from_value(json!({
        "maxAttempts": 2.6,
        "initialIntervalMs": -5,
        "backoffCoefficient": 1.5,
        "maxIntervalMs": 30000,
    }))
    .unwrap();
    assert_eq!(
        policy,
        ResolvedRetryPolicy {
            max_attempts: 3,
            initial_interval_ms: 0,
            backoff_coefficient: 1.5,
            max_interval_ms: 30_000,
        }
    );
    // Text is not a number.
    assert!(
        serde_json::from_value::<ResolvedRetryPolicy>(json!({
            "maxAttempts": "3", "initialIntervalMs": 1, "backoffCoefficient": 2, "maxIntervalMs": 1
        }))
        .is_err()
    );
    let written = serde_json::to_value(policy).unwrap();
    assert_eq!(written["maxAttempts"], json!(3));
    assert_eq!(written["maxIntervalMs"], json!(30_000));
}

#[test]
fn a_retry_policy_is_empty_until_a_value_is_set() {
    assert!(RetryPolicy::new().is_empty());
    for policy in [
        RetryPolicy::new().max_attempts(0),
        RetryPolicy::new().initial_interval(Duration::ZERO),
        RetryPolicy::new().backoff_coefficient(1.0),
        RetryPolicy::new().max_interval(Duration::ZERO),
    ] {
        assert!(!policy.is_empty(), "{policy:?}");
    }
}

#[test]
fn a_worker_config_keeps_what_is_set() {
    assert_eq!(WorkerConfig::new(), WorkerConfig::default());
    let config = WorkerConfig::new()
        .concurrency(4)
        .poll_interval(Duration::from_millis(100))
        .poll_max_interval(Duration::from_secs(2))
        .poll_backoff_coefficient(1.5)
        .lease_duration(Duration::from_secs(30));
    assert_eq!(
        config,
        WorkerConfig {
            concurrency: Some(4),
            poll_interval: Some(Duration::from_millis(100)),
            poll_max_interval: Some(Duration::from_secs(2)),
            poll_backoff_coefficient: Some(1.5),
            lease_duration: Some(Duration::from_secs(30)),
        }
    );
}

#[test]
fn the_active_statuses_are_among_all_of_them() {
    for status in ACTIVE_RUN_STATUSES {
        assert!(ALL_RUN_STATUSES.contains(&status), "{status}");
    }
    for status in ["completed", "failed", "canceled"] {
        assert!(ALL_RUN_STATUSES.contains(&status) && !ACTIVE_RUN_STATUSES.contains(&status));
    }
}
