//! The page `mcp` of the framework, rendered by the server: the URL of the
//! MCP server, the tokens of the shop, and the forms that create, change,
//! rotate and revoke them. The secret of a token is in the page answered to
//! the form that made it, and never again.

use openshop::server::ServerOptions;
use openshop::testing::{TestContext, TestOptions};
use openshop::{McpConfig, McpPermission, OpenShopConfig, UiSource, Value, json};
use reqwest::header::AUTHORIZATION;
use reqwest::{Method, StatusCode};

use super::{Reply, fetch, form, request, send, urlencoding};

const SHOP: &str = "server-pages-mcp.myshopify.com";
const APP_URL: &str = "https://mcp-admin.example.com";
/// Where the forms of the page post: `POST /mcp` is the MCP endpoint.
const FORMS: &str = "/mcp/tokens";

async fn context() -> TestContext {
    let mcp = McpConfig::new()
        .permission(McpPermission::new("demo:read_status", "Read <demo> status").description("What the demo says."));
    let config = OpenShopConfig::builder().mcp(mcp).build().unwrap();
    TestContext::with_options(
        config,
        TestOptions {
            shop: SHOP.to_string(),
            access_token: Some("offline-token".to_string()),
            host: Some(APP_URL.to_string()),
            server: ServerOptions {
                ui: UiSource::Server,
                dev: false,
            },
            ..Default::default()
        },
    )
    .await
    .expect("test context")
}

/// A form of the page, sent by the script.
async fn submit(ctx: &TestContext, body: &str) -> Reply {
    send(form(fetch(ctx, Method::POST, FORMS), body.to_string())).await
}

/// The secret a page shows, if it shows one.
fn secret_in(body: &str) -> Option<&str> {
    let start = body.find("oshp_mcp_")?;
    let end = body[start..].find(['<', '"'])?;
    Some(&body[start..start + end])
}

/// What `initialize` answers to an MCP client with `secret`.
async fn initialize(ctx: &TestContext, secret: &str) -> StatusCode {
    ctx.http
        .post(ctx.url_for("/mcp"))
        .bearer_auth(secret)
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }))
        .send()
        .await
        .expect("response")
        .status()
}

/// The tokens of the shop, as the JSON API lists them.
async fn tokens(ctx: &TestContext) -> Vec<Value> {
    let listed = send(ctx.get("/api/mcp/tokens")).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    serde_json::from_str(&listed.body).unwrap()
}

async fn token_named(ctx: &TestContext, name: &str) -> Value {
    tokens(ctx)
        .await
        .into_iter()
        .find(|token| token["name"] == name)
        .unwrap_or_else(|| panic!("token {name}"))
}

#[tokio::test]
async fn the_mcp_page_creates_a_token_and_shows_its_secret_once() {
    let ctx = context().await;

    let page = send(fetch(&ctx, Method::GET, "/mcp")).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;
    assert!(body.contains(r#"<s-page heading="MCP">"#), "{body}");
    // The URL of the server, to copy.
    assert!(
        body.contains(r#"data-openshop-copy="https://mcp-admin.example.com/mcp">Copy URL</s-button>"#),
        "{body}"
    );
    assert!(body.contains("No MCP tokens yet"), "{body}");
    // The permissions to grant, those of the application with those of the framework.
    assert!(
        body.contains(r#"<s-checkbox name="permissions" value="demo:read_status" label="Read &lt;demo&gt; status""#),
        "{body}"
    );
    assert!(body.contains("What the demo says."), "{body}");
    assert!(body.contains(r#"value="shopify_admin_graphql""#), "{body}");
    assert!(body.contains(r#"<form method="post" action="/mcp/tokens">"#), "{body}");
    assert!(secret_in(body).is_none());

    // Created: the page again, with the secret, and a toast.
    let created = submit(
        &ctx,
        "_action=createToken&name=Claude+Desktop&expiresInDays=30&permissions=read_logs&permissions=demo%3Aread_status",
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.body);
    assert_eq!(created.header("x-openshop-toast"), Some("Token%20created"));
    let body = &created.body;
    assert!(
        body.contains("Token created. Copy it now; it will not be shown again."),
        "{body}"
    );
    assert!(body.contains(r#"heading="Copy this token now""#), "{body}");
    let secret = secret_in(body).expect("the secret").to_string();
    assert!(
        body.contains(&format!(r#"data-openshop-copy="{secret}">Copy token</s-button>"#)),
        "{body}"
    );
    assert!(
        body.contains(r#"<s-text type="strong">Claude Desktop</s-text>"#),
        "{body}"
    );
    assert!(body.contains(r#"<s-badge tone="success">active</s-badge>"#), "{body}");
    // It is the token's secret: an MCP client gets in with it.
    assert_eq!(initialize(&ctx, &secret).await, StatusCode::OK);
    let token = token_named(&ctx, "Claude Desktop").await;
    assert_eq!(token["permissions"], json!(["read_logs", "demo:read_status"]));
    let expires = chrono::DateTime::parse_from_rfc3339(token["expiresAt"].as_str().unwrap()).unwrap();
    let days = (expires.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_hours();
    assert!((29 * 24..=30 * 24).contains(&days), "{days}");

    // Never again.
    let again = send(fetch(&ctx, Method::GET, "/mcp")).await;
    assert!(secret_in(&again.body).is_none(), "{}", again.body);
    assert!(!again.body.contains("Copy this token now"), "{}", again.body);
    // Its edit modal, its permissions checked.
    let id = token["id"].as_str().unwrap();
    assert!(
        again.body.contains(&format!(
            r#"<s-modal id="edit-mcp-token-{id}" heading="Edit Claude Desktop""#
        )),
        "{}",
        again.body
    );
    assert!(
        again
            .body
            .contains(r#"value="read_logs" label="Read logs" labelAccessibilityVisibility="exclusive" checked>"#),
        "{}",
        again.body
    );

    // Without the script: the page with the secret, not a redirect, which would lose it.
    let plain = send(form(
        request(&ctx, Method::POST, FORMS).header(AUTHORIZATION, ctx.authorization_header()),
        "_action=createToken&name=CI&expiresInDays=none",
    ))
    .await;
    assert_eq!(plain.status, StatusCode::OK, "{}", plain.body);
    assert!(plain.header("location").is_none());
    assert!(plain.body.starts_with("<!DOCTYPE html>"), "{}", plain.body);
    let plain_secret = secret_in(&plain.body).expect("the secret");
    assert_ne!(plain_secret, secret);
    assert_eq!(initialize(&ctx, plain_secret).await, StatusCode::OK);
    assert_eq!(token_named(&ctx, "CI").await["expiresAt"], Value::Null);

    // `POST /mcp` stays the endpoint of MCP clients.
    let protocol = send(form(fetch(&ctx, Method::POST, "/mcp"), "_action=createToken&name=Nope")).await;
    assert_eq!(protocol.status, StatusCode::UNAUTHORIZED, "{}", protocol.body);
    assert_eq!(tokens(&ctx).await.len(), 2);
}

#[tokio::test]
async fn a_refused_token_form_shows_its_errors_under_its_fields() {
    let ctx = context().await;

    let unnamed = submit(
        &ctx,
        "_action=createToken&name=+&expiresInDays=365&permissions=run_flows",
    )
    .await;
    assert_eq!(unnamed.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", unnamed.body);
    assert_eq!(unnamed.header("x-openshop-toast-error"), None);
    let body = &unnamed.body;
    // The modal opens again, with what was sent and the error under its field.
    assert!(
        body.contains(r#"<s-modal id="create-mcp-token-modal" heading="Create token" accessibilityLabel="Create MCP token" data-openshop-open>"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<s-text-field name="name" label="Name" placeholder="Claude Desktop" value=" " error="Token name is required."></s-text-field>"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<s-option value="365" selected>1 year</s-option>"#),
        "{body}"
    );
    assert!(
        body.contains(r#"value="run_flows" label="Run flows" labelAccessibilityVisibility="exclusive" checked>"#),
        "{body}"
    );
    assert!(secret_in(body).is_none());

    let expiry = submit(&ctx, "_action=createToken&name=CI&expiresInDays=0").await;
    assert_eq!(expiry.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        expiry.body.contains(
            r#"<s-select name="expiresInDays" label="Expiration" value="0" error="Must be a whole number of days, or no expiration">"#
        ),
        "{}",
        expiry.body
    );

    let unknown = submit(&ctx, "_action=createToken&name=CI&permissions=nope").await;
    assert_eq!(unknown.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        unknown
            .body
            .contains(r#"<s-banner tone="critical">Unknown permission &quot;nope&quot;</s-banner>"#),
        "{}",
        unknown.body
    );

    // Without the script: the same page.
    let plain = send(form(
        request(&ctx, Method::POST, FORMS).header(AUTHORIZATION, ctx.authorization_header()),
        "_action=createToken&name=",
    ))
    .await;
    assert_eq!(plain.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        plain.body.contains(r#"error="Token name is required.""#),
        "{}",
        plain.body
    );

    assert!(tokens(&ctx).await.is_empty());
    let missing = submit(&ctx, "_action=dropTokens").await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_forms_of_a_token_change_it() {
    let ctx = context().await;
    let created = submit(&ctx, "_action=createToken&name=Agent&permissions=read_logs").await;
    let secret = secret_in(&created.body).expect("the secret").to_string();
    let token = token_named(&ctx, "Agent").await;
    let id = token["id"].as_str().unwrap().to_string();
    let with_id = |rest: &str| format!("id={}&{rest}", urlencoding(&id));

    // Its permissions.
    let saved = submit(
        &ctx,
        &with_id("_action=setPermissions&permissions=read_crons&permissions=run_flows"),
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
    assert_eq!(saved.header("x-openshop-toast"), Some("Permissions%20saved"));
    assert!(secret_in(&saved.body).is_none());
    assert_eq!(
        token_named(&ctx, "Agent").await["permissions"],
        json!(["read_crons", "run_flows"])
    );
    assert!(saved.body.contains("2 of 7 selected"), "{}", saved.body);
    // None at all: no box checked sends no field.
    let cleared = submit(&ctx, &with_id("_action=setPermissions")).await;
    assert_eq!(cleared.status, StatusCode::OK);
    assert_eq!(token_named(&ctx, "Agent").await["permissions"], json!([]));
    // A permission that is not declared: the modal opens again and says so.
    let unknown = submit(&ctx, &with_id("_action=setPermissions&permissions=nope")).await;
    assert_eq!(unknown.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        unknown.body.contains(&format!(
            r#"<s-modal id="edit-mcp-token-{id}" heading="Edit Agent" accessibilityLabel="Edit MCP token" size="large" data-openshop-open>"#
        )),
        "{}",
        unknown.body
    );
    assert!(
        unknown.body.contains("Unknown permission &quot;nope&quot;"),
        "{}",
        unknown.body
    );

    // Disabled, then enabled.
    let disabled = submit(&ctx, &with_id("_action=updateToken&status=disabled")).await;
    assert_eq!(disabled.status, StatusCode::OK);
    assert_eq!(disabled.header("x-openshop-toast"), Some("Token%20disabled"));
    assert!(
        disabled.body.contains(r#"<s-badge tone="neutral">disabled</s-badge>"#),
        "{}",
        disabled.body
    );
    assert!(disabled.body.contains(">Enable</s-button>"), "{}", disabled.body);
    assert_eq!(token_named(&ctx, "Agent").await["status"], "disabled");
    let enabled = submit(&ctx, &with_id("_action=updateToken&status=active")).await;
    assert_eq!(enabled.header("x-openshop-toast"), Some("Token%20enabled"));
    assert_eq!(token_named(&ctx, "Agent").await["status"], "active");
    let odd = submit(&ctx, &with_id("_action=updateToken&status=revoked")).await;
    assert_eq!(odd.status, StatusCode::BAD_REQUEST);
    assert_eq!(token_named(&ctx, "Agent").await["status"], "active");

    // Rotated: a new secret, shown once; the old one no longer gets in.
    let rotated = submit(&ctx, &with_id("_action=rotateToken")).await;
    assert_eq!(rotated.status, StatusCode::OK, "{}", rotated.body);
    assert_eq!(rotated.header("x-openshop-toast"), Some("Token%20rotated"));
    assert!(
        rotated.body.contains("Token rotated. Copy the new value now."),
        "{}",
        rotated.body
    );
    let fresh = secret_in(&rotated.body).expect("the new secret").to_string();
    assert_ne!(fresh, secret);
    assert_eq!(initialize(&ctx, &fresh).await, StatusCode::OK);
    assert_ne!(initialize(&ctx, &secret).await, StatusCode::OK);
    let next = send(fetch(&ctx, Method::GET, "/mcp")).await;
    assert!(secret_in(&next.body).is_none(), "{}", next.body);

    // Revoked, confirmed in its modal.
    assert!(
        next.body.contains(&format!(
            r#"<s-modal id="revoke-mcp-token-{id}" heading="Revoke Agent?""#
        )),
        "{}",
        next.body
    );
    let revoked = submit(&ctx, &with_id("_action=revokeToken")).await;
    assert_eq!(revoked.status, StatusCode::OK);
    assert_eq!(revoked.header("x-openshop-toast"), Some("Token%20revoked"));
    assert!(
        revoked.body.contains(r#"<s-badge tone="critical">revoked</s-badge>"#),
        "{}",
        revoked.body
    );
    assert!(
        !revoked.body.contains(r#"<s-modal id="revoke-mcp-token-"#),
        "{}",
        revoked.body
    );
    assert!(
        revoked
            .body
            .contains(r#"variant="primary" disabled>Save permissions</s-button>"#),
        "{}",
        revoked.body
    );
    assert_eq!(token_named(&ctx, "Agent").await["status"], "revoked");
    assert_ne!(initialize(&ctx, &fresh).await, StatusCode::OK);

    // A revoked token changes no more: an error toast, the page stays.
    for (action, message) in [
        ("_action=rotateToken", "Cannot%20rotate%20a%20revoked%20token"),
        (
            "_action=setPermissions&permissions=read_logs",
            "Cannot%20update%20a%20revoked%20token",
        ),
        (
            "_action=updateToken&status=active",
            "Cannot%20update%20a%20revoked%20token",
        ),
    ] {
        let refused = submit(&ctx, &with_id(action)).await;
        assert_eq!(refused.status, StatusCode::CONFLICT, "{action}");
        assert_eq!(refused.header("x-openshop-toast-error"), Some(message), "{action}");
        assert!(refused.body.is_empty(), "{action}");
    }

    // A token that is not there.
    for action in ["_action=rotateToken", "_action=revokeToken", "_action=setPermissions"] {
        let missing = submit(&ctx, &format!("{action}&id=00000000-0000-0000-0000-000000000000")).await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND, "{action}");
        assert_eq!(missing.header("x-openshop-toast-error"), Some("Token%20not%20found"));
    }
    let unnamed = submit(&ctx, "_action=rotateToken").await;
    assert_eq!(unnamed.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_edit_modal_of_a_token_shows_its_recent_audit() {
    let ctx = context().await;
    submit(&ctx, "_action=createToken&name=First").await;
    submit(&ctx, "_action=createToken&name=Second").await;
    let first = token_named(&ctx, "First").await;
    let second = token_named(&ctx, "Second").await;
    for (token, count) in [(&first, 12), (&second, 1)] {
        for minutes in 0..count {
            sqlx::query(
                "INSERT INTO mcp_audit_logs (shop, token_id, capability_name, target_shop, status, created_at) \
                 VALUES ($1, $2, $3, $1, $4, now() - make_interval(mins => $5))",
            )
            .bind(SHOP)
            .bind(token["tokenId"].as_str())
            .bind(format!("tool.{minutes}"))
            .bind(if minutes == 0 { "denied" } else { "success" })
            .bind(minutes)
            .execute(&ctx.db)
            .await
            .unwrap();
        }
    }
    // The list of the JSON API says nothing of them.
    assert_eq!(first["recentAudits"], json!([]));

    let page = send(fetch(&ctx, Method::GET, "/mcp")).await;
    let modal = |token: &Value| {
        let start = format!(r#"<s-modal id="edit-mcp-token-{}""#, token["id"].as_str().unwrap());
        let from = page.body.find(&start).unwrap();
        let to = page.body[from..].find("</form>").unwrap();
        page.body[from..from + to].to_string()
    };
    // The ten latest of each token, newest first.
    let audits = modal(&first);
    assert!(audits.contains("Recent audit"), "{audits}");
    assert!(
        audits.contains(&format!(
            r#"<s-table-cell>tool.0<div style="color:#6d7175;font-size:12px">{SHOP}</div></s-table-cell><s-table-cell><s-badge tone="warning">denied</s-badge>"#
        )),
        "{audits}"
    );
    for minutes in 0..10 {
        assert!(audits.contains(&format!(">tool.{minutes}<")), "{minutes}: {audits}");
    }
    assert!(
        !audits.contains(">tool.10<") && !audits.contains(">tool.11<"),
        "{audits}"
    );
    assert!(audits.find(">tool.1<").unwrap() < audits.find(">tool.2<").unwrap());
    let other = modal(&second);
    assert_eq!(
        other.matches("<s-badge tone=\"warning\">denied</s-badge>").count(),
        1,
        "{other}"
    );
    assert!(!other.contains(">tool.1<"), "{other}");
}
