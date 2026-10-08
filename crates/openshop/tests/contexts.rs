//! What the context of a shop gives and what takes one (`dispatch`, the rows
//! of a model), the typed inputs of proxies and routes, Liquid templates, and
//! the input of a cron checked when the application is built.

use openshop::prelude::*;
use openshop::testing::{TestContext, sign_webhook_body};
use openshop::{OpenShopConfig, sqlx};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Model)]
#[model("notes")]
struct Note {
    id: Uuid,
    shop: String,
    text: String,
}

#[input]
#[derive(Debug)]
struct NoteInput {
    note_text: String,
}

#[flow("writeNote")]
async fn write_note(ctx: FlowContext, input: NoteInput) -> Result<()> {
    // The rows of the shop the run belongs to.
    Note::of(&ctx).insert(NewNote { text: input.note_text }).await?;
    Ok(())
}

#[flow]
async fn refresh(_ctx: FlowContext) -> Result<()> {
    Ok(())
}

#[derive(Debug, Deserialize)]
struct Order {
    name: String,
}

#[webhook("orders/create")]
async fn orders_create(ctx: WebhookContext, order: Order) -> Result<()> {
    // Queued for the shop and the Shopify app the event came from.
    write_note::dispatch(&ctx, NoteInput { note_text: order.name }).await?;
    refresh::dispatch(&ctx).await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct Search {
    page: Option<u32>,
    #[serde(default)]
    exact: bool,
}

#[proxy(GET, "/notes")]
async fn list_notes(ctx: ProxyContext, search: Search) -> Result<Value> {
    Ok(json!({
        "notes": Note::of(&ctx).count().await?,
        "page": search.page.unwrap_or(1),
        "exact": search.exact,
    }))
}

#[derive(Debug, Serialize, Deserialize)]
struct NoteBody {
    text: String,
}

#[proxy(POST, "/notes")]
async fn add_note(ctx: ProxyContext, body: NoteBody) -> Result<Value> {
    let note = Note::of(&ctx).insert(NewNote { text: body.text }).await?;
    Ok(json!({ "shop": note.shop, "text": note.text }))
}

#[proxy(GET, "/greeting", liquid)]
async fn greeting(ctx: ProxyContext) -> Result<Liquid> {
    Ok(Liquid::new("<p>{{ greeting }}</p>").assign("greeting", format!("Hello {}", ctx.shop)))
}

#[derive(Debug, Deserialize)]
struct ShopQuery {
    shop: String,
}

#[route(GET, "/count", json)]
async fn count(ctx: RouteContext, query: ShopQuery) -> Result<Value> {
    // A route belongs to no shop: it resolves one, and gets what a context gives.
    let shop = ctx.for_shop(&query.shop).await?;
    Ok(json!({ "shop": shop.shop, "notes": Note::of(&shop).count().await? }))
}

#[route(POST, "/echo", json)]
async fn echo(_ctx: RouteContext, body: NoteBody) -> Result<NoteBody> {
    Ok(body)
}

/// Refuses unknown parameters: Shopify's signing parameters are not part of
/// the typed input, a body is optional, and the handler refuses a rating itself.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Strict {
    rating: Option<u8>,
}

#[proxy(GET, "/strict")]
async fn strict(_ctx: ProxyContext, query: Strict) -> Result<Value> {
    Ok(json!({ "rating": query.rating }))
}

#[route(POST, "/rate", json)]
async fn rate(_ctx: RouteContext, body: Option<Strict>) -> Result<Value> {
    let rating = body.and_then(|body| body.rating).unwrap_or(5);
    if rating > 5 {
        bail!(openshop::InvalidInput("rating is 1 to 5".to_string()));
    }
    Ok(json!({ "rating": rating }))
}

static APP: App = app! {
    flows: [write_note, refresh],
    models: [Note],
    webhooks: [orders_create],
    proxies: [list_notes, add_note, greeting, strict],
    routes: [count, echo, rate],
};

#[openshop::test(APP)]
async fn a_flow_reads_and_writes_the_rows_of_its_shop(ctx: &TestContext) {
    let outcome = ctx
        .run_flow(write_note::NAME, json!({ "noteText": "From a flow" }))
        .await
        .unwrap();
    assert_eq!(outcome, RunOutcome::Completed);

    let notes = Note::for_shop(&ctx.db, &ctx.shop).all().await.unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(
        (notes[0].shop.as_str(), notes[0].text.as_str()),
        (ctx.shop.as_str(), "From a flow")
    );
    assert_eq!(
        Note::for_shop(&ctx.db, "elsewhere.myshopify.com")
            .count()
            .await
            .unwrap(),
        0
    );
}

#[openshop::test(APP)]
async fn a_webhook_dispatches_flows_for_the_shop_of_its_context(ctx: &TestContext) {
    let body = r##"{"name":"#1001"}"##;
    let response = ctx
        .http
        .post(ctx.url_for("/webhooks"))
        .header("content-type", "application/json")
        .header("x-shopify-hmac-sha256", sign_webhook_body(body, &ctx.secret))
        .header("x-shopify-topic", "orders/create")
        .header("x-shopify-shop-domain", &ctx.shop)
        .header("x-shopify-api-version", "2026-04")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    let runs: Vec<(String, String, String, Value)> =
        sqlx::query_as("SELECT flow_name, app_handle, shop, input FROM flow_runs ORDER BY flow_name")
            .fetch_all(&ctx.db)
            .await
            .unwrap();
    assert_eq!(
        runs,
        [
            // A flow without input is dispatched without one.
            (
                refresh::NAME.to_string(),
                "default".to_string(),
                ctx.shop.clone(),
                json!({})
            ),
            // The typed input is written as the flow reads it.
            (
                write_note::NAME.to_string(),
                "default".to_string(),
                ctx.shop.clone(),
                json!({ "noteText": "#1001" })
            ),
        ]
    );
}

#[openshop::test(APP)]
async fn a_proxy_reads_its_typed_query_and_body(ctx: &TestContext) {
    let created = ctx
        .proxy()
        .post("/notes")
        .json(json!({ "text": "From the storefront" }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.body, json!({ "shop": ctx.shop, "text": "From the storefront" }));

    // Numbers and booleans are read from the text of the query.
    let listed = ctx
        .proxy()
        .get("/notes")
        .qs(&[("page", "3"), ("exact", "true")])
        .send()
        .await
        .unwrap();
    assert_eq!(listed.body, json!({ "notes": 1, "page": 3, "exact": true }));
    let defaults = ctx.proxy().get("/notes").send().await.unwrap();
    assert_eq!(defaults.body, json!({ "notes": 1, "page": 1, "exact": false }));

    // What does not fit the type is refused before the handler runs.
    let wrong_query = ctx.proxy().get("/notes").qs(&[("page", "three")]).send().await.unwrap();
    assert_eq!(wrong_query.status, 422);
    let error = wrong_query.body["error"].as_str().unwrap();
    assert!(error.starts_with("Invalid input: "), "{error}");

    let wrong_body = ctx
        .proxy()
        .post("/notes")
        .json(json!({ "title": "No text" }))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_body.status, 422);
    assert_eq!(Note::for_shop(&ctx.db, &ctx.shop).count().await.unwrap(), 1);
}

#[openshop::test(APP)]
async fn a_liquid_proxy_answers_its_template_with_its_variables(ctx: &TestContext) {
    let response = ctx.proxy().get("/greeting").send().await.unwrap();

    assert!(response.content_type.contains("application/liquid"));
    assert_eq!(
        response.text,
        format!(
            "{{%- assign greeting = \"Hello {}\" -%}}\n<p>{{{{ greeting }}}}</p>",
            ctx.shop
        )
    );
}

#[openshop::test(APP)]
async fn a_route_answers_json_and_reads_its_typed_input(ctx: &TestContext) {
    Note::for_shop(&ctx.db, &ctx.shop)
        .insert(NewNote {
            text: "Counted".to_string(),
        })
        .await
        .unwrap();

    let counted = ctx
        .http
        .get(format!("{}?shop={}", ctx.url_for("/routes/count"), ctx.shop))
        .send()
        .await
        .unwrap();
    assert_eq!(counted.status(), 200);
    assert_eq!(
        counted.json::<Value>().await.unwrap(),
        json!({ "shop": ctx.shop, "notes": 1 })
    );

    let echoed = ctx
        .http
        .post(ctx.url_for("/routes/echo"))
        .json(&json!({ "text": "Echo" }))
        .send()
        .await
        .unwrap();
    assert_eq!(echoed.status(), 200);
    assert_eq!(echoed.json::<Value>().await.unwrap(), json!({ "text": "Echo" }));

    for refused in [
        ctx.http.get(ctx.url_for("/routes/count")),
        ctx.http.post(ctx.url_for("/routes/echo")).json(&json!({ "text": 1 })),
    ] {
        let response = refused.send().await.unwrap();
        assert_eq!(response.status(), 422);
        let body: Value = response.json().await.unwrap();
        assert!(body["error"].as_str().unwrap().starts_with("Invalid input: "), "{body}");
    }
}

#[openshop::test(APP)]
async fn the_typed_input_is_the_request_without_what_shopify_adds(ctx: &TestContext) {
    // The signed query carries `shop`, `timestamp`, `signature`…: none reaches the type.
    let strict = ctx.proxy().get("/strict").qs(&[("rating", "4")]).send().await.unwrap();
    assert_eq!((strict.status, &strict.body), (200, &json!({ "rating": 4 })));
    let unknown = ctx.proxy().get("/strict").qs(&[("stars", "4")]).send().await.unwrap();
    assert_eq!(unknown.status, 422);

    // A route without a body gives `None` to an optional input.
    let rate = |body: Option<Value>| {
        let request = ctx.http.post(ctx.url_for("/routes/rate"));
        match body {
            Some(body) => request.json(&body),
            None => request,
        }
    };
    let default = rate(None).send().await.unwrap();
    assert_eq!(default.json::<Value>().await.unwrap(), json!({ "rating": 5 }));
    let given = rate(Some(json!({ "rating": 3 }))).send().await.unwrap();
    assert_eq!(given.json::<Value>().await.unwrap(), json!({ "rating": 3 }));

    // A handler refuses an input the same way the typed input does.
    let refused = rate(Some(json!({ "rating": 9 }))).send().await.unwrap();
    assert_eq!(refused.status(), 422);
    assert_eq!(
        refused.json::<Value>().await.unwrap(),
        json!({ "error": "Invalid input: rating is 1 to 5" })
    );
}

#[test]
fn the_input_of_a_cron_is_checked_when_the_application_is_built() {
    let with = |cron: Cron| {
        OpenShopConfig::builder()
            .add::<write_note>()
            .add::<refresh>()
            .cron(cron)
            .build()
    };

    assert!(with(Cron::new("0 2 * * *", write_note::NAME).input(json!({ "noteText": "Nightly" }))).is_ok());
    // A flow without input takes any cron.
    assert!(with(Cron::new("0 2 * * *", refresh::NAME).all_shops()).is_ok());

    for (cron, reason) in [
        // No input at all: the scheduler would dispatch `{}`.
        (Cron::new("0 2 * * *", write_note::NAME), "input.noteText is required"),
        (
            Cron::new("0 2 * * *", write_note::NAME).input(json!({ "note_text": "Wrong name" })),
            "input.noteText is required",
        ),
        (
            Cron::new("0 2 * * *", write_note::NAME).input(json!({ "noteText": 3 })),
            "input.noteText must be string",
        ),
    ] {
        assert_eq!(
            with(cron).err().unwrap().to_string(),
            format!("[openshop] Invalid config: crons[0].input is not an input of flow \"writeNote\": {reason}")
        );
    }
}
