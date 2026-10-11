//! The operator dashboard under `/ops`: served only with a password, read-only,
//! and showing every shop.

mod common;

use common::noop_flow;
use openshop::builder::{Field, OpenShopBuilder, Provider};
use openshop::server::ServerOptions;
use openshop::testing::{TestContext, TestOptions};
use openshop::{Cron, OpenShopConfig, PgPool, Uuid};
use reqwest::{Method, Response, StatusCode};

const PASSWORD: &str = "a long enough password";
const SHOP: &str = "one.myshopify.com";
const OTHER_SHOP: &str = "two.myshopify.com";

fn builder() -> OpenShopBuilder {
    OpenShopConfig::builder()
        .flow(noop_flow("sync"))
        .flow(noop_flow("idle"))
        .provider(Provider::new("warehouse").field("token", Field::password("Token")))
        .cron(Cron::new("0 2 * * *", "sync").name("Nightly sync").all_shops())
}

fn with_dashboard() -> OpenShopConfig {
    builder().ops_password(PASSWORD.to_string()).build().unwrap()
}

async fn context(config: OpenShopConfig) -> TestContext {
    TestContext::new(config).await.expect("test context")
}

async fn get(ctx: &TestContext, path: &str, cookie: Option<&str>) -> Response {
    let request = ctx.http.get(format!("{}{path}", ctx.url));
    match cookie {
        Some(cookie) => request.header("cookie", cookie),
        None => request,
    }
    .send()
    .await
    .unwrap()
}

async fn post_password(ctx: &TestContext, password: &str) -> Response {
    ctx.http
        .post(format!("{}/ops/login", ctx.url))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!("password={}", password.replace(' ', "+")))
        .send()
        .await
        .unwrap()
}

fn location(response: &Response) -> &str {
    response.headers()["location"].to_str().unwrap()
}

/// Signs in and returns the `name=value` pair of the session cookie.
async fn sign_in(ctx: &TestContext) -> String {
    let response = post_password(ctx, PASSWORD).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let cookie = response.headers()["set-cookie"].to_str().unwrap();
    cookie.split(';').next().unwrap().to_string()
}

/// The body of a page the dashboard serves to a signed-in visitor.
async fn page(ctx: &TestContext, path: &str, cookie: &str) -> String {
    let response = get(ctx, path, Some(cookie)).await;
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    response.text().await.unwrap()
}

async fn install(db: &PgPool, shop: &str, access_token: Option<&str>, uninstalled: bool) {
    sqlx::query(
        "INSERT INTO installations (shop, access_token, scopes, uninstalled_at) \
         VALUES ($1, $2, 'read_orders', CASE WHEN $3 THEN now() END)",
    )
    .bind(shop)
    .bind(access_token)
    .bind(uninstalled)
    .execute(db)
    .await
    .unwrap();
}

async fn run(db: &PgPool, shop: &str, status: &str, error: Option<&str>) -> Uuid {
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO flow_runs (shop, flow_name, status, error, input, started_at, completed_at) \
         VALUES ($1, 'sync', $2, $3, '{\"limit\":10}', now() - interval '2 seconds', now()) RETURNING id",
    )
    .bind(shop)
    .bind(status)
    .bind(error)
    .fetch_one(db)
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn without_a_password_the_dashboard_does_not_exist() {
    // An empty password is no password.
    let config = builder().ops_password(Some("  ".to_string())).build().unwrap();
    assert_eq!(config.ops_password, None);
    assert_eq!(builder().build().unwrap().ops_password, None);

    // With or without an admin UI, `/ops` is not a page of it either.
    for server in [ServerOptions::default(), ServerOptions::production()] {
        let options = TestOptions {
            server,
            ..Default::default()
        };
        let ctx = TestContext::with_options(builder().build().unwrap(), options)
            .await
            .unwrap();
        for path in ["/ops", "/ops/", "/ops/login", "/ops/runs", "/ops/shops"] {
            assert_eq!(get(&ctx, path, None).await.status(), StatusCode::NOT_FOUND, "{path}");
        }
        assert_eq!(post_password(&ctx, PASSWORD).await.status(), StatusCode::NOT_FOUND);
        ctx.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn production_refuses_a_password_that_is_easy_to_guess() {
    let production = || TestOptions {
        production: true,
        encryption_key: Some("ab".repeat(32)),
        ..Default::default()
    };
    let short = builder().ops_password("fifteen-chars-x".to_string()).build().unwrap();
    let error = TestContext::with_options(short, production()).await.err().unwrap();
    assert_eq!(
        error.to_string(),
        "[openshop] Invalid config: opsPassword must be at least 16 characters in production"
    );

    let ctx = TestContext::with_options(with_dashboard(), production()).await.unwrap();
    assert_eq!(get(&ctx, "/ops/login", None).await.status(), StatusCode::OK);
    ctx.shutdown().await.unwrap();
}

#[openshop::test(context(with_dashboard()))]
async fn the_dashboard_opens_with_the_password_only(ctx: &TestContext) {
    // Without a session, every address leads to the sign-in form.
    for path in [
        "/ops",
        "/ops/",
        "/ops/runs",
        "/ops/logs",
        "/ops/shops",
        "/ops/shops/one.myshopify.com",
        "/ops/flows",
        "/ops/crons",
        "/ops/anything",
    ] {
        let response = get(ctx, path, None).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER, "{path}");
        assert_eq!(location(&response), "/ops/login", "{path}");
    }
    for forged in ["openshop_ops=v1.9999999999.00", "openshop_ops=", "other=1"] {
        assert_eq!(
            get(ctx, "/ops", Some(forged)).await.status(),
            StatusCode::SEE_OTHER,
            "{forged}"
        );
    }

    let form = get(ctx, "/ops/login", None).await;
    assert_eq!(form.status(), StatusCode::OK);
    let headers = form.headers();
    assert_eq!(headers["content-type"], "text/html; charset=utf-8");
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(headers["x-frame-options"], "DENY");
    assert_eq!(headers["referrer-policy"], "no-referrer");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(
        headers["content-security-policy"],
        "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'"
    );
    let html = form.text().await.unwrap();
    assert!(html.contains("<form method=\"post\" action=\"/ops/login\">"), "{html}");
    assert!(html.contains("<input type=\"password\" name=\"password\""), "{html}");
    assert!(!html.contains("<script"), "{html}");

    let wrong = post_password(ctx, "a wrong password").await;
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    assert!(wrong.headers().get("set-cookie").is_none());
    assert!(wrong.text().await.unwrap().contains("Wrong password."));

    // A form posted from another site is refused, whatever it carries.
    let cross_site = ctx
        .http
        .post(format!("{}/ops/login", ctx.url))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("sec-fetch-site", "cross-site")
        .body(format!("password={}", PASSWORD.replace(' ', "+")))
        .send()
        .await
        .unwrap();
    assert_eq!(cross_site.status(), StatusCode::FORBIDDEN);
    assert!(cross_site.headers().get("set-cookie").is_none());

    let granted = post_password(ctx, PASSWORD).await;
    assert_eq!(granted.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&granted), "/ops");
    let set_cookie = granted.headers()["set-cookie"].to_str().unwrap();
    assert!(set_cookie.starts_with("openshop_ops=v1."), "{set_cookie}");
    assert!(
        set_cookie.ends_with("; Path=/ops; Max-Age=43200; HttpOnly; SameSite=Strict; Secure"),
        "{set_cookie}"
    );
    let cookie = set_cookie.split(';').next().unwrap();

    let overview = get(ctx, "/ops", Some(cookie)).await;
    assert_eq!(overview.status(), StatusCode::OK);
    assert_eq!(overview.headers()["cache-control"], "no-store");
    let overview = overview.text().await.unwrap();
    assert!(
        overview.contains("<title>Overview · OpenShop ops</title>"),
        "{overview}"
    );

    // Signed in, the form is of no use.
    let form = get(ctx, "/ops/login", Some(cookie)).await;
    assert_eq!((form.status(), location(&form)), (StatusCode::SEE_OTHER, "/ops"));

    let signed_out = ctx
        .http
        .post(format!("{}/ops/logout", ctx.url))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(signed_out.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&signed_out), "/ops/login");
    assert_eq!(
        signed_out.headers()["set-cookie"],
        "openshop_ops=; Path=/ops; Max-Age=0; HttpOnly; SameSite=Strict; Secure"
    );
}

#[tokio::test]
async fn the_session_cookie_is_secure_unless_the_server_is_reached_over_plain_http() {
    // A browser keeps a `Secure` cookie only over HTTPS.
    for (host, server, secure) in [
        (Some("https://app.example.com"), ServerOptions::default(), true),
        (None, ServerOptions::default(), true),
        (Some("http://localhost:3000"), ServerOptions::default(), false),
        (Some("https://app.example.com"), ServerOptions::development(), false),
    ] {
        let options = TestOptions {
            host: host.map(str::to_string),
            server,
            ..Default::default()
        };
        let ctx = TestContext::with_options(with_dashboard(), options).await.unwrap();
        let granted = post_password(&ctx, PASSWORD).await;
        assert_eq!(granted.status(), StatusCode::SEE_OTHER);
        let cookie = granted.headers()["set-cookie"].to_str().unwrap();
        assert_eq!(cookie.ends_with("; Secure"), secure, "{host:?}: {cookie}");
        assert!(cookie.contains("; HttpOnly; SameSite=Strict"), "{cookie}");
        ctx.shutdown().await.unwrap();
    }
}

/// Asserts that a page has every one of `expected`.
fn assert_has(page: &str, expected: &[&str]) {
    for expected in expected {
        assert!(page.contains(expected), "missing {expected} in {page}");
    }
}

/// Two shops with a run each, one failed with markup in its error, and a
/// third that did not finish installing. Returns the ids of the completed
/// run and of the failed one.
async fn seed(ctx: &TestContext) -> (Uuid, Uuid) {
    install(&ctx.db, SHOP, Some("shpat_one"), false).await;
    install(&ctx.db, OTHER_SHOP, Some("shpat_two"), true).await;
    install(&ctx.db, "pending.myshopify.com", None, false).await;
    let completed = run(&ctx.db, SHOP, "completed", None).await;
    let failed = run(&ctx.db, OTHER_SHOP, "failed", Some("<script>alert(1)</script>")).await;
    sqlx::query(
        "INSERT INTO step_results (flow_run_id, step_name, status, error, duration_ms, output) VALUES \
         ($1, 'fetch-orders', 'completed', NULL, 12, '[\"<b>\"]'), ($1, 'send-to-warehouse', 'failed', '502 <Bad> Gateway', 40, NULL)",
    )
    .bind(failed.to_string())
    .execute(&ctx.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO logs (flow_run_id, level, message, payload) VALUES ($1, 'error', 'Push <failed>', '{\"count\":2}')",
    )
    .bind(failed.to_string())
    .execute(&ctx.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO provider_configs (shop, provider_name, config, last_checked_at, last_check_ok) \
         VALUES ($1, 'warehouse', '{\"token\":\"provider-secret\"}', now(), false)",
    )
    .bind(OTHER_SHOP)
    .execute(&ctx.db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO cron_overrides (shop, cron_key, enabled) VALUES ($1, 'sync:0 2 * * *', false)")
        .bind(OTHER_SHOP)
        .execute(&ctx.db)
        .await
        .unwrap();
    (completed, failed)
}

#[openshop::test(context(with_dashboard()))]
async fn the_overview_says_what_happened_and_what_calls_for_a_look(ctx: &TestContext) {
    let (_, failed) = seed(ctx).await;
    let cookie = sign_in(ctx).await;

    let overview = page(ctx, "/ops", &cookie).await;
    assert_has(
        &overview,
        &[
            "<title>Overview · OpenShop ops</title>",
            "<a href=\"/ops\" aria-current=\"page\">Overview</a>",
            "<div class=\"label\">Installed shops</div><div class=\"value\">1</div>",
            "<div class=\"label\">Runs</div><div class=\"value\">2</div><div class=\"hint\">the last 24 hours</div>",
            "<div class=\"kpi bad\"><div class=\"label\">Failed</div><div class=\"value\">1</div>\
             <div class=\"hint\">50.0% of the runs</div>",
            "<div class=\"label\">Waiting</div><div class=\"value\">0</div><div class=\"hint\">none is late</div>",
            "<div class=\"label\">Running</div><div class=\"value\">0</div>",
            // The chart, drawn for a wide page and for a narrow one.
            "<svg class=\"chart wide\"",
            "<svg class=\"chart narrow\"",
            "<meta http-equiv=\"refresh\" content=\"30\">",
            // What failed, by flow and by shop, each row leading somewhere.
            "<a class=\"mono\" href=\"/ops/runs?range=24h&amp;flow=sync\">sync</a>",
            "<a class=\"mono\" href=\"/ops/shops/two.myshopify.com\">two.myshopify.com</a>",
            &format!("<tr class=\"link\"><td class=\"go\"><a href=\"/ops/runs/{failed}\""),
            // What a shop wrote is text, never markup.
            "<span class=\"bad mono\">&lt;script&gt;alert(1)&lt;/script&gt;</span>",
            // The periods of the page.
            "<a href=\"/ops?range=7d\">7d</a>",
            "<a href=\"/ops?range=24h\" aria-current=\"true\">24h</a>",
        ],
    );
    // No script, and nothing of the access tokens of the shops.
    assert!(!overview.contains("<script"), "{overview}");
    assert!(!overview.contains("shpat_"), "{overview}");

    // A pending run that could have started is late.
    sqlx::query(
        "INSERT INTO flow_runs (shop, flow_name, status, created_at) \
         VALUES ($1, 'sync', 'pending', now() - interval '90 seconds')",
    )
    .bind(SHOP)
    .execute(&ctx.db)
    .await
    .unwrap();
    let late = page(ctx, "/ops?range=1h", &cookie).await;
    assert_has(
        &late,
        &[
            "<div class=\"label\">Waiting</div><div class=\"value\">1</div><div class=\"hint\">the oldest for 1 min 3",
            "<div class=\"hint\">the last hour</div>",
            "<a href=\"/ops?range=1h\" aria-current=\"true\">1h</a>",
        ],
    );
}

#[openshop::test(context(with_dashboard()))]
async fn runs_and_log_lines_are_filtered_over_a_period(ctx: &TestContext) {
    let (completed, failed) = seed(ctx).await;
    let cookie = sign_in(ctx).await;
    let (completed, failed) = (completed.to_string(), failed.to_string());

    let all = page(ctx, "/ops/runs", &cookie).await;
    assert_has(
        &all,
        &[
            &completed,
            &failed,
            "<svg class=\"chart wide\"",
            "<span><i class=\"bad\"></i>failed</span>",
        ],
    );

    let failures = page(ctx, "/ops/runs?status=failed", &cookie).await;
    assert!(
        failures.contains(&failed) && !failures.contains(&completed),
        "{failures}"
    );
    assert_has(
        &failures,
        &[
            "<option value=\"failed\" selected>failed</option>",
            // The filter in force is a link that removes it.
            "<a class=\"chip\" href=\"/ops/runs\" title=\"Remove this filter\">Status is <b>failed</b> ×</a>",
            // The whole row leads to the run.
            &format!("<tr class=\"link\"><td class=\"go\"><a href=\"/ops/runs/{failed}\""),
            &format!("<a class=\"mono\" href=\"/ops/runs/{failed}\">sync</a>"),
        ],
    );
    // Already filtered on what failed, the chart has one line.
    assert!(!failures.contains("<i class=\"bad\">"), "{failures}");

    let of_shop = page(ctx, "/ops/runs?shop=ONE.my&flow=sync&range=1h", &cookie).await;
    assert!(of_shop.contains(&completed) && !of_shop.contains(&failed), "{of_shop}");
    assert_has(
        &of_shop,
        &[
            "value=\"ONE.my\"",
            "<option value=\"sync\" selected>sync</option>",
            "<input type=\"hidden\" name=\"range\" value=\"1h\">",
            "<a class=\"chip\" href=\"/ops/runs?range=1h&amp;flow=sync\" title=\"Remove this filter\">Shop has <b>ONE.my</b> ×</a>",
        ],
    );

    let by_error = page(ctx, "/ops/runs?q=ALERT", &cookie).await;
    assert!(
        by_error.contains(&failed) && !by_error.contains(&completed),
        "{by_error}"
    );
    // A search is text: neither markup nor a pattern.
    let markup = page(ctx, "/ops/runs?q=%22%3E%3Cb%3E", &cookie).await;
    assert_has(&markup, &["No run matches.", "value=\"&quot;&gt;&lt;b&gt;\""]);
    for none in ["/ops/runs?q=%25", "/ops/runs?before=2000-01-01T00:00:00Z"] {
        assert!(page(ctx, none, &cookie).await.contains("No run matches."), "{none}");
    }
    // An unknown status or period is none.
    let unknown = page(ctx, "/ops/runs?status=nope&range=nope", &cookie).await;
    assert_has(&unknown, &[&completed, &failed, "aria-current=\"true\">24h</a>"]);

    let logs = page(ctx, "/ops/logs", &cookie).await;
    assert_has(
        &logs,
        &[
            "<title>Logs · OpenShop ops</title>",
            "Push &lt;failed&gt;",
            &format!("<a class=\"mono\" href=\"/ops/runs/{failed}\">sync</a>"),
            &format!("<tr class=\"link\"><td class=\"go\"><a href=\"/ops/runs/{failed}\""),
            "<a href=\"/ops/shops/two.myshopify.com\">two.myshopify.com</a>",
            "<span><i class=\"bad\"></i>errors</span>",
        ],
    );
    let found = page(ctx, "/ops/logs?q=push&level=error&shop=two", &cookie).await;
    assert!(found.contains("Push &lt;failed&gt;"), "{found}");
    for none in [
        "/ops/logs?level=info",
        "/ops/logs?shop=one",
        "/ops/logs?flow=idle",
        "/ops/logs?q=%25",
    ] {
        assert!(
            page(ctx, none, &cookie).await.contains("No log line matches."),
            "{none}"
        );
    }
}

#[openshop::test(context(with_dashboard()))]
async fn a_run_shows_its_steps_and_its_log_lines(ctx: &TestContext) {
    let (_, failed) = seed(ctx).await;
    let cookie = sign_in(ctx).await;

    let detail = page(ctx, &format!("/ops/runs/{failed}"), &cookie).await;
    assert_has(
        &detail,
        &[
            "<title>sync · OpenShop ops</title>",
            "<a href=\"/ops/runs\">Runs</a><span>/</span><strong>sync</strong>",
            "<dt>Duration</dt><dd>2.0 s</dd>",
            "<dt>Shop</dt><dd><a href=\"/ops/shops/two.myshopify.com\">two.myshopify.com</a></dd>",
            "<span class=\"bad mono\">&lt;script&gt;alert(1)&lt;/script&gt;</span>",
            "<span class=\"mono\">fetch-orders</span>",
            "<span class=\"mono\">send-to-warehouse</span>",
            "<span class=\"bad mono\">502 &lt;Bad&gt; Gateway</span>",
            "<details><summary>Output</summary><pre>[\n  &quot;&lt;b&gt;&quot;\n]</pre></details>",
            "<span class=\"mono\">Push &lt;failed&gt;</span>",
            "&quot;count&quot;: 2",
            "&quot;limit&quot;: 10",
        ],
    );
    assert!(!detail.contains("<script"), "{detail}");
    for missing in [
        format!("/ops/runs/{}", Uuid::new_v4()),
        "/ops/runs/not-a-run".to_string(),
        "/ops/shops/nobody.myshopify.com".to_string(),
        "/ops/nope".to_string(),
    ] {
        assert_eq!(
            get(ctx, &missing, Some(&cookie)).await.status(),
            StatusCode::NOT_FOUND,
            "{missing}"
        );
    }
}

#[openshop::test(context(with_dashboard()))]
async fn shops_flows_and_crons_have_their_page(ctx: &TestContext) {
    let (_, failed) = seed(ctx).await;
    let cookie = sign_in(ctx).await;

    let shops = page(ctx, "/ops/shops", &cookie).await;
    assert_has(
        &shops,
        &[
            "<span>installed</span>",
            "<span>uninstalled</span>",
            "<span>pending</span>",
            "<a href=\"/ops/shops/pending.myshopify.com\">pending.myshopify.com</a>",
            "<tr class=\"link\"><td data-label=\"Shop\"><a href=\"/ops/shops/two.myshopify.com\">two.myshopify.com</a></td>",
            "<span class=\"bad\">1</span>",
            "<span class=\"mono\">read_orders</span>",
        ],
    );
    assert!(!shops.contains("shpat_"), "{shops}");
    // The shop with a failure comes first.
    assert!(
        shops.find("two.myshopify.com").unwrap() < shops.find("one.myshopify.com").unwrap(),
        "{shops}"
    );
    let pending = page(ctx, "/ops/shops?state=pending", &cookie).await;
    assert!(
        pending.contains("pending.myshopify.com") && !pending.contains("one.myshopify.com"),
        "{pending}"
    );
    let found = page(ctx, "/ops/shops?q=TWO", &cookie).await;
    assert!(
        found.contains("two.myshopify.com") && !found.contains("one.myshopify.com"),
        "{found}"
    );
    assert!(
        page(ctx, "/ops/shops?q=%25", &cookie)
            .await
            .contains("No installation matches.")
    );

    let shop = page(ctx, "/ops/shops/two.myshopify.com", &cookie).await;
    assert_has(
        &shop,
        &[
            "<title>two.myshopify.com · OpenShop ops</title>",
            "<a href=\"/ops/shops\">Shops</a><span>/</span><strong>two.myshopify.com</strong>",
            "<div class=\"kpi bad\"><div class=\"label\">Failed</div><div class=\"value\">1</div>\
             <div class=\"hint\">100.0% of the runs</div>",
            "<span>uninstalled</span>",
            // Whether the shop saved a provider and how its check went: never what it saved.
            "<span class=\"mono\">warehouse</span>",
            "<span>check failed</span>",
            "<span class=\"mono\">sync:0 2 * * *</span>",
            &format!("<a class=\"mono\" href=\"/ops/runs/{failed}\">sync</a>"),
            "<a href=\"/ops/runs?range=24h&amp;shop=two.myshopify.com\">Every run of this shop</a>",
            "<a href=\"/ops/logs?range=24h&amp;shop=two.myshopify.com\">Its log lines</a>",
            "<svg class=\"chart wide\"",
        ],
    );
    assert!(!shop.contains("provider-secret") && !shop.contains("shpat_"), "{shop}");
    let unconfigured = page(ctx, "/ops/shops/one.myshopify.com", &cookie).await;
    assert_has(
        &unconfigured,
        &["<span>not configured</span>", "The shop turned no cron off."],
    );

    let flows = page(ctx, "/ops/flows", &cookie).await;
    assert_has(
        &flows,
        &[
            "<tr class=\"link\"><td data-label=\"Flow\"><a class=\"mono\" href=\"/ops/runs?range=24h&amp;flow=sync\">sync</a></td>",
            "<a href=\"/ops/runs?range=24h&amp;flow=sync\" data-label=\"Failure rate\" tabindex=\"-1\" aria-hidden=\"true\">50.0%</a>",
            "<a href=\"/ops/runs?range=24h&amp;flow=sync\" data-label=\"Mean duration\" tabindex=\"-1\" aria-hidden=\"true\">2.0 s</a>",
            // A registered flow that did not run has its line.
            "<a class=\"mono\" href=\"/ops/runs?range=24h&amp;flow=idle\">idle</a>",
        ],
    );

    let crons = page(ctx, "/ops/crons", &cookie).await;
    assert_has(
        &crons,
        &[
            "Nightly sync",
            "<span class=\"mono\">0 2 * * *</span>",
            "every installed shop",
            "data-label=\"Turned off by\" tabindex=\"-1\" aria-hidden=\"true\">1 shop</a>",
        ],
    );
    assert!(!crons.contains("never"), "{crons}");
}

#[openshop::test(context(with_dashboard()))]
async fn the_dashboard_only_reads(ctx: &TestContext) {
    let id = run(&ctx.db, SHOP, "failed", Some("boom")).await;
    let cookie = sign_in(ctx).await;

    for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
        for path in ["/ops".to_string(), "/ops/runs".to_string(), format!("/ops/runs/{id}")] {
            let response = ctx
                .http
                .request(method.clone(), format!("{}{path}", ctx.url))
                .header("cookie", &cookie)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
        }
    }
    let (status,): (String,) = sqlx::query_as("SELECT status FROM flow_runs WHERE id = $1")
        .bind(id)
        .fetch_one(&ctx.db)
        .await
        .unwrap();
    assert_eq!(status, "failed");
}
