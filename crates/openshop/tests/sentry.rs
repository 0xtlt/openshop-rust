//! What the framework reports to Sentry: runs that fail, handlers that fail
//! and panics, each with the scope of the unit of work it happened in.

use std::future::Future;
use std::time::{Duration, Instant};

use openshop::Cron;
use openshop::builder::{AdminPage, Flow, Proxy, Route, Webhook};
use openshop::engine::{RunFlowOptions, Scheduler, Worker, run_flow};
use openshop::response;
use openshop::sentry::protocol::Event;
use openshop::sentry::{ClientOptions, Level};
use openshop::testing::{TestContext, TestOptions, sign_webhook_body};
use openshop::{
    AdminPublicError, DispatchOptions, FlowContext, InvalidInput, OpenShopConfig, RunOutcome, SentryConfig, Uuid,
    WorkerConfig, bail, json,
};
use openshop_shared::build_admin_function_rpc_url;
use reqwest::Method;

const FLOW: &str = "syncOrders";
const DSN: &str = "https://public@sentry.invalid/42";

/// Runs a test with the Sentry client an application starts, which here
/// keeps what is reported instead of sending it. The client is bound to the
/// thread of the test, where its server and its runs execute too: what they
/// report is what comes back.
fn captured<F: Future<Output = ()>>(test: F) -> Vec<Event<'static>> {
    captured_with(SentryConfig::new(DSN.to_string()).client_options(false), test)
}

fn captured_with<F: Future<Output = ()>>(options: ClientOptions, test: F) -> Vec<Event<'static>> {
    openshop::sentry::test::with_captured_events_options(
        || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(test)
        },
        options,
    )
}

async fn context(config: OpenShopConfig) -> TestContext {
    TestContext::with_options(config, TestOptions::installed())
        .await
        .expect("test context")
}

fn flow_app(flow: Flow) -> OpenShopConfig {
    OpenShopConfig::builder().flow(flow).build().unwrap()
}

/// The message of the error an event reports: the outermost one comes last.
fn message<'a>(event: &'a Event<'static>) -> &'a str {
    event
        .exception
        .last()
        .and_then(|exception| exception.value.as_deref())
        .unwrap_or_default()
}

fn tag<'a>(event: &'a Event<'static>, key: &str) -> Option<&'a str> {
    event.tags.get(key).map(String::as_str)
}

fn trail<'a>(event: &'a Event<'static>) -> Vec<&'a str> {
    event
        .breadcrumbs
        .iter()
        .filter_map(|breadcrumb| breadcrumb.message.as_deref())
        .collect()
}

fn the_only(mut events: Vec<Event<'static>>) -> Event<'static> {
    assert_eq!(events.len(), 1, "{events:#?}");
    events.remove(0)
}

#[test]
fn the_setting_takes_a_dsn_and_is_off_without_one() {
    let sentry = |config: OpenShopConfig| config.sentry.is_some();
    let builder = OpenShopConfig::builder;

    assert!(!sentry(builder().build().unwrap()));
    assert!(!sentry(builder().sentry(None).build().unwrap()));
    assert!(!sentry(builder().sentry(Some("  ".to_string())).build().unwrap()));
    assert!(sentry(builder().sentry(DSN).build().unwrap()));
    assert!(sentry(builder().sentry(Some(DSN.to_string())).build().unwrap()));
    let named = SentryConfig::new(DSN.to_string())
        .environment("staging")
        .release("1.2.3")
        .configure(|options| options.sample_rate(0.5));
    assert!(sentry(builder().sentry(named).build().unwrap()));

    // A DSN that is not one stops the application where its configuration is built.
    let error = builder().sentry("not a dsn").build().err().unwrap().to_string();
    assert!(
        error.starts_with("[openshop] Invalid config: sentry.dsn is not a valid DSN"),
        "{error}"
    );
    // The setting given last is the one that counts, and the one that is checked.
    assert!(sentry(builder().sentry("not a dsn").sentry(DSN).build().unwrap()));
    assert!(!sentry(builder().sentry("not a dsn").sentry(None).build().unwrap()));
    assert!(!sentry(builder().sentry(DSN).sentry(None).build().unwrap()));
    assert!(builder().sentry(DSN).sentry("not a dsn").build().is_err());
}

#[test]
fn a_failed_run_is_reported_with_its_flow_its_shop_and_its_log_lines() {
    let mut run_id = None;
    let events = captured(async {
        let flow = Flow::new(FLOW, |ctx: FlowContext| async move {
            ctx.logger
                .info_with("Fetching orders", json!({ "email": "buyer@example.com" }));
            ctx.step.run("push", || async { Ok(()) }).await?;
            bail!("the warehouse refused the orders")
        });
        let ctx = context(flow_app(flow)).await;
        let queued = ctx
            .app
            .dispatch_flow(DispatchOptions::new(FLOW, &ctx.shop))
            .await
            .unwrap();
        run_id = Some(queued.run_id);

        // Attempts are numbered from 1; the retry policy allows three.
        for (attempt, will_retry) in [(1, true), (2, true), (3, false)] {
            sqlx::query("UPDATE flow_runs SET status = 'running', attempts = $2 WHERE id = $1")
                .bind(queued.run_id)
                .bind(attempt)
                .execute(&ctx.db)
                .await
                .unwrap();
            let outcome = run_flow(&ctx.app, RunFlowOptions::new(queued.run_id, FLOW, &ctx.shop))
                .await
                .unwrap();
            assert!(
                matches!(outcome, RunOutcome::Failed { will_retry: retried, .. } if retried == will_retry),
                "{outcome:?}"
            );
        }
        ctx.shutdown().await.unwrap();
    });

    assert_eq!(events.len(), 3, "{events:#?}");
    for (index, event) in events.iter().enumerate() {
        let last = index == 2;
        assert_eq!(message(event), "the warehouse refused the orders");
        // An attempt that will be made again is a warning, the last one an error.
        assert_eq!(event.level, if last { Level::Error } else { Level::Warning });
        assert_eq!(tag(event, "will_retry"), Some(if last { "false" } else { "true" }));
        assert_eq!(tag(event, "attempt"), Some((index + 1).to_string().as_str()));
        assert_eq!(event.transaction.as_deref(), Some("flow syncOrders"));
        assert_eq!(tag(event, "flow"), Some(FLOW));
        assert_eq!(tag(event, "shop"), Some("test.myshopify.com"));
        assert_eq!(tag(event, "shopify_app"), Some("default"));
        assert_eq!(tag(event, "run_id"), run_id.map(|id| id.to_string()).as_deref());
        // The shop is the user of the event: an issue counts the shops it reaches.
        let user = event.user.as_ref().and_then(|user| user.id.as_deref());
        assert_eq!(user, Some("test.myshopify.com"));

        // The lines of this attempt, and not the ones of the attempts before.
        let trail = trail(event);
        assert_eq!(trail[0], "Flow \"syncOrders\" started");
        assert!(trail.contains(&"Fetching orders"), "{trail:?}");
        assert!(trail.iter().any(|line| line.starts_with("Step \"push\"")), "{trail:?}");
        assert_eq!(trail.iter().filter(|line| **line == "Fetching orders").count(), 1);
        // A line is left without its payload.
        assert!(!format!("{event:?}").contains("buyer@example.com"));
    }
}

#[test]
fn a_run_that_does_not_fail_reports_nothing() {
    let events = captured(async {
        let config = OpenShopConfig::builder()
            .flow(Flow::new("completes", |_ctx: FlowContext| async { Ok(()) }))
            .flow(Flow::new("sleeps", |ctx: FlowContext| async move {
                ctx.step.sleep("wait", std::time::Duration::from_secs(3600)).await
            }))
            .flow(Flow::new("cancels", |ctx: FlowContext| async move {
                ctx.signal.cancel();
                ctx.step.run("after", || async { Ok(()) }).await
            }))
            .build()
            .unwrap();
        let ctx = context(config).await;

        assert_eq!(
            ctx.run_flow("completes", json!({})).await.unwrap(),
            RunOutcome::Completed
        );
        let slept = ctx.run_flow("sleeps", json!({})).await.unwrap();
        assert!(matches!(slept, RunOutcome::Sleeping { .. }), "{slept:?}");
        assert_eq!(ctx.run_flow("cancels", json!({})).await.unwrap(), RunOutcome::Canceled);
        ctx.shutdown().await.unwrap();
    });

    assert!(events.is_empty(), "{events:#?}");
}

#[test]
fn a_panic_is_reported_once_where_it_happened() {
    let events = captured(async {
        let flow = Flow::new(FLOW, |_ctx: FlowContext| async move {
            if true {
                panic!("the flow gave up");
            }
            Ok(())
        });
        let ctx = context(flow_app(flow)).await;

        let outcome = ctx.run_flow(FLOW, json!({})).await.unwrap();
        assert_eq!(
            outcome,
            RunOutcome::Failed {
                error: "Flow panicked: the flow gave up".to_string(),
                will_retry: false,
            }
        );
        ctx.shutdown().await.unwrap();
    });

    // The panic hook reports the panic, in the scope of the run: the failure
    // the run is recorded with is not a second event.
    let event = the_only(events);
    assert_eq!(message(&event), "the flow gave up");
    let mechanism = event
        .exception
        .last()
        .and_then(|exception| exception.mechanism.as_ref());
    assert_eq!(mechanism.map(|mechanism| mechanism.ty.as_str()), Some("panic"));
    assert_eq!(event.level, Level::Fatal);
    assert_eq!(tag(&event, "flow"), Some(FLOW));
    assert_eq!(tag(&event, "attempt"), Some("1"));
    assert_eq!(event.transaction.as_deref(), Some("flow syncOrders"));
}

#[test]
fn a_run_that_cannot_be_made_is_reported() {
    let events = captured(async {
        let ctx = context(OpenShopConfig::builder().build().unwrap()).await;
        let error = run_flow(&ctx.app, RunFlowOptions::new(Uuid::new_v4(), "missing", &ctx.shop))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("missing"), "{error}");
        ctx.shutdown().await.unwrap();
    });

    let event = the_only(events);
    assert_eq!(tag(&event, "flow"), Some("missing"));
    assert_eq!(event.level, Level::Error);
}

#[test]
fn a_run_the_worker_fails_without_an_attempt_is_reported() {
    let mut runs = Vec::new();
    let events = captured(async {
        let ctx = context(flow_app(Flow::new(FLOW, |_ctx: FlowContext| async { Ok(()) }))).await;
        // A run past its deadline, and the run of a flow this application does not have.
        let expired = ctx.dispatch_flow(FLOW, json!({})).await.unwrap();
        sqlx::query("UPDATE flow_runs SET deadline_at = now() - interval '1 hour' WHERE id = $1")
            .bind(expired)
            .execute(&ctx.db)
            .await
            .unwrap();
        let (unknown,): (Uuid,) = sqlx::query_as(
            "INSERT INTO flow_runs (shop, flow_name, status, available_at) \
             VALUES ($1, 'removed', 'pending', now()) RETURNING id",
        )
        .bind(&ctx.shop)
        .fetch_one(&ctx.db)
        .await
        .unwrap();
        runs = vec![expired, unknown];

        let worker = Worker::new(
            ctx.app.clone(),
            WorkerConfig::new().poll_interval(Duration::from_millis(20)),
        );
        worker.start().await;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let (failed,): (i64,) = sqlx::query_as("SELECT count(*) FROM flow_runs WHERE status = 'failed'")
                .fetch_one(&ctx.db)
                .await
                .unwrap();
            if failed == 2 {
                break;
            }
            assert!(Instant::now() < deadline, "the worker failed {failed} run(s) of 2");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        worker.stop().await;
        ctx.shutdown().await.unwrap();
    });

    assert_eq!(events.len(), 2, "{events:#?}");
    let reported = |run_id: Uuid| {
        let run_id = run_id.to_string();
        events
            .iter()
            .find(|event| tag(event, "run_id") == Some(run_id.as_str()))
            .expect("an event for the run")
    };
    let expired = reported(runs[0]);
    assert_eq!(message(expired), "Flow timed out — exceeded maximum allowed duration");
    assert_eq!(tag(expired, "flow"), Some(FLOW));
    assert_eq!(tag(expired, "shop"), Some("test.myshopify.com"));
    let unknown = reported(runs[1]);
    assert_eq!(message(unknown), "Flow \"removed\" not registered");
    assert_eq!(unknown.transaction.as_deref(), Some("flow removed"));
}

#[test]
fn a_route_that_fails_is_reported_by_its_pattern() {
    let events = captured(async {
        let config = OpenShopConfig::builder()
            .route(
                "/callbacks/:token",
                Route::public().get(|ctx| async move {
                    match ctx.params["token"].as_str() {
                        "refused" => bail!(InvalidInput("the token is not known".to_string())),
                        "fine" => Ok(response::json(json!({ "ok": true }))),
                        _ => bail!("the carrier is down"),
                    }
                }),
            )
            .build()
            .unwrap();
        let ctx = context(config).await;
        let status = async |token: &str| {
            let url = ctx.url_for(&format!("/routes/callbacks/{token}"));
            ctx.http.get(url).send().await.unwrap().status().as_u16()
        };

        assert_eq!(status("fine").await, 200);
        // An input the handler refuses is an answer, not a failure.
        assert_eq!(status("refused").await, 422);
        assert_eq!(status("s3cr3t-token").await, 500);
        ctx.shutdown().await.unwrap();
    });

    let event = the_only(events);
    assert_eq!(message(&event), "the carrier is down");
    assert_eq!(event.level, Level::Error);
    assert_eq!(tag(&event, "route"), Some("GET /callbacks/:token"));
    assert_eq!(event.transaction.as_deref(), Some("route GET /callbacks/:token"));
    // The path of the request, which can carry a secret, is not sent.
    assert!(!format!("{event:?}").contains("s3cr3t-token"));
}

#[test]
fn each_request_has_a_scope_of_its_own() {
    let events = captured(async {
        let config = OpenShopConfig::builder()
            .webhook(
                "orders/create",
                Webhook::new(|_ctx| async { bail!("the order could not be read") }),
            )
            .proxy(
                "/reviews",
                Proxy::json().get(|_ctx| async { bail!("the reviews are gone") }),
            )
            .route("/ping", Route::public().get(|_ctx| async { bail!("no answer") }))
            .build()
            .unwrap();
        let ctx = context(config).await;

        let body = r#"{"id":1}"#;
        let delivered = ctx
            .http
            .post(ctx.url_for("/webhooks"))
            .header("content-type", "application/json")
            .header("x-shopify-hmac-sha256", sign_webhook_body(body, &ctx.secret))
            .header("x-shopify-topic", "orders/create")
            .header("x-shopify-shop-domain", "sender.myshopify.com")
            .body(body)
            .send()
            .await
            .unwrap();
        // Shopify is answered `200` whatever the handler did.
        assert_eq!(delivered.status(), 200);
        assert_eq!(ctx.proxy().get("/reviews").send().await.unwrap().status, 500);
        let pinged = ctx.http.get(ctx.url_for("/routes/ping")).send().await.unwrap();
        assert_eq!(pinged.status(), 500);
        ctx.shutdown().await.unwrap();
    });

    assert_eq!(events.len(), 3, "{events:#?}");
    let [webhook, proxy, route] = &events[..] else {
        unreachable!()
    };

    assert_eq!(message(webhook), "the order could not be read");
    assert_eq!(webhook.transaction.as_deref(), Some("webhook orders/create"));
    assert_eq!(tag(webhook, "webhook"), Some("orders/create"));
    assert_eq!(tag(webhook, "shop"), Some("sender.myshopify.com"));

    assert_eq!(message(proxy), "the reviews are gone");
    assert_eq!(proxy.transaction.as_deref(), Some("proxy GET /reviews"));
    assert_eq!(tag(proxy, "proxy"), Some("GET /reviews"));
    assert_eq!(tag(proxy, "shop"), Some("test.myshopify.com"));
    assert_eq!(tag(proxy, "webhook"), None);

    // The requests before left nothing on the one after.
    assert_eq!(message(route), "no answer");
    assert_eq!(tag(route, "route"), Some("GET /ping"));
    assert_eq!(tag(route, "shop"), None);
    assert_eq!(tag(route, "webhook"), None);
    assert_eq!(tag(route, "proxy"), None);
    assert!(route.user.is_none());
}

#[test]
fn an_admin_function_that_fails_is_found_by_its_request_id() {
    let mut request_id = None;
    let events = captured(async {
        let page = AdminPage::new("/reviews")
            .loader("list", |_ctx, _input| async { bail!("the reviews table is locked") })
            .loader("refused", |_ctx, _input| async {
                bail!(AdminPublicError::new("NOT_READY", "Come back later").status(409))
            });
        let ctx = context(OpenShopConfig::builder().admin_page(page).build().unwrap()).await;
        let load = async |name: &str| {
            let url = build_admin_function_rpc_url("loader", name, "/reviews");
            let response = ctx
                .request(Method::POST, &url)
                .json(&json!({ "input": null }))
                .send()
                .await
                .unwrap();
            let status = response.status().as_u16();
            (status, response.json::<openshop::Value>().await.unwrap())
        };

        // An error written for the merchant is an answer, not a failure.
        assert_eq!(load("refused").await.0, 409);
        let (status, body) = load("list").await;
        assert_eq!(status, 500);
        request_id = body["requestId"].as_str().map(str::to_string);
        ctx.shutdown().await.unwrap();
    });

    let event = the_only(events);
    assert_eq!(message(&event), "the reviews table is locked");
    assert_eq!(event.transaction.as_deref(), Some("admin /reviews list"));
    assert_eq!(tag(&event, "shop"), Some("test.myshopify.com"));
    // The id the page shows the merchant is a tag of the event.
    assert!(request_id.is_some());
    assert_eq!(tag(&event, "request_id"), request_id.as_deref());
}

#[test]
fn a_request_the_admin_api_cannot_answer_is_reported_by_its_route() {
    let run_id = Uuid::new_v4();
    let events = captured(async {
        let ctx = context(OpenShopConfig::builder().build().unwrap()).await;
        sqlx::query("DROP TABLE flow_runs CASCADE")
            .execute(&ctx.db)
            .await
            .unwrap();

        for path in ["/api/runs".to_string(), format!("/api/runs/{run_id}")] {
            let response = ctx.get(&path).send().await.unwrap();
            assert_eq!(response.status(), 500, "{path}");
        }
        // A path no route matches has no name, and fails nothing here.
        assert_eq!(ctx.get("/api/nothing/here").send().await.unwrap().status(), 404);
        ctx.shutdown().await.unwrap();
    });

    assert_eq!(events.len(), 2, "{events:#?}");
    assert!(message(&events[0]).contains("flow_runs"), "{}", message(&events[0]));
    assert_eq!(events[0].transaction.as_deref(), Some("GET /api/runs"));
    assert_eq!(tag(&events[0], "shop"), Some("test.myshopify.com"));
    // The route as it is declared: the identifier in the path is not sent.
    assert_eq!(events[1].transaction.as_deref(), Some("GET /api/runs/{id}"));
    assert!(!format!("{:?}", events[1]).contains(&run_id.to_string()));
}

#[test]
fn a_handler_that_panics_is_reported_once_where_it_happened() {
    let events = captured(async {
        let config = OpenShopConfig::builder()
            .route(
                "/ping",
                Route::public().get(|ctx| async move {
                    if ctx.params.is_empty() {
                        panic!("the route gave up");
                    }
                    Ok(response::json(json!({})))
                }),
            )
            .build()
            .unwrap();
        let ctx = context(config).await;
        let pinged = ctx.http.get(ctx.url_for("/routes/ping")).send().await.unwrap();
        assert_eq!(pinged.status(), 500);
        ctx.shutdown().await.unwrap();
    });

    let event = the_only(events);
    assert_eq!(message(&event), "the route gave up");
    assert_eq!(event.level, Level::Fatal);
    assert_eq!(tag(&event, "route"), Some("GET /ping"));
}

#[test]
fn without_a_panic_hook_a_caught_panic_is_reported_as_the_failure_it_became() {
    // A client the application built itself, with no integration at all.
    let events = captured_with(ClientOptions::new(), async {
        let config = OpenShopConfig::builder()
            .flow(Flow::new(FLOW, |ctx: FlowContext| async move {
                if ctx.shop.is_empty() {
                    return Ok(());
                }
                panic!("the flow gave up")
            }))
            .webhook(
                "orders/create",
                Webhook::new(|ctx| async move {
                    if ctx.shop.is_empty() {
                        return Ok(());
                    }
                    panic!("the webhook gave up")
                }),
            )
            .build()
            .unwrap();
        let ctx = context(config).await;

        let outcome = ctx.run_flow(FLOW, json!({})).await.unwrap();
        assert!(matches!(outcome, RunOutcome::Failed { .. }), "{outcome:?}");
        let body = "{}";
        let delivered = ctx
            .http
            .post(ctx.url_for("/webhooks"))
            .header("x-shopify-hmac-sha256", sign_webhook_body(body, &ctx.secret))
            .header("x-shopify-topic", "orders/create")
            .header("x-shopify-shop-domain", "sender.myshopify.com")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(delivered.status(), 200);
        ctx.shutdown().await.unwrap();
    });

    assert_eq!(events.len(), 2, "{events:#?}");
    assert_eq!(message(&events[0]), "Flow panicked: the flow gave up");
    assert_eq!(events[0].level, Level::Error);
    assert_eq!(tag(&events[0], "flow"), Some(FLOW));
    assert_eq!(message(&events[1]), "handler panicked");
    assert_eq!(tag(&events[1], "webhook"), Some("orders/create"));
}

#[test]
fn a_cron_that_cannot_queue_its_run_is_reported() {
    let events = captured(async {
        let config = OpenShopConfig::builder()
            .flow(Flow::new(FLOW, |_ctx: FlowContext| async { Ok(()) }))
            .cron(Cron::new("* * * * * *", FLOW))
            .build()
            .unwrap();
        let ctx = context(config).await;
        sqlx::query("DROP TABLE flow_runs CASCADE")
            .execute(&ctx.db)
            .await
            .unwrap();

        // The cron fires every second: twice at least while the test waits.
        let mut scheduler = Scheduler::start(&ctx.app).unwrap();
        tokio::time::sleep(Duration::from_millis(2300)).await;
        scheduler.stop();
        ctx.shutdown().await.unwrap();
    });

    assert!(!events.is_empty());
    for event in &events {
        assert!(message(event).contains("flow_runs"), "{}", message(event));
        assert_eq!(event.transaction.as_deref(), Some("cron syncOrders"));
        assert_eq!(tag(event, "cron"), Some(FLOW));
        assert_eq!(tag(event, "shop"), Some("__global__"));
    }
}

#[test]
fn what_a_url_carries_of_secrets_is_not_sent() {
    let events = captured(async {
        let flow = Flow::new(FLOW, |ctx: FlowContext| async move {
            ctx.logger
                .info("Calling https://warehouse:hunter2@api.example.com/orders");
            bail!("error sending request for url (https://api.example.com/orders?api_key=SECRET)")
        });
        let ctx = context(flow_app(flow)).await;
        ctx.run_flow(FLOW, json!({})).await.unwrap();
        ctx.shutdown().await.unwrap();
    });

    let event = the_only(events);
    assert_eq!(
        message(&event),
        "error sending request for url (https://api.example.com/orders?[redacted])"
    );
    let trail = trail(&event);
    assert!(
        trail.contains(&"Calling https://[redacted]@api.example.com/orders"),
        "{trail:?}"
    );
    let sent = format!("{event:?}");
    assert!(!sent.contains("SECRET") && !sent.contains("hunter2"), "{sent}");
}
