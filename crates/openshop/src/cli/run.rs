//! `run <flow>` and `cron run <cron>`: a flow, or the flow of a cron, run once
//! by hand, the way the scheduler and a worker run it.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

use super::{database_url, shutdown_signal};
use crate::app::{OpenShop, OpenShopConfig};
use crate::db::migrate::{assert_migrated, push_schema};
use crate::engine::Worker;
use crate::engine::dispatch::unknown_flow;
use crate::engine::scheduler::{CronTarget, Queued, queue_now, shop_targets};
use crate::runtime::RuntimeEnv;
use crate::server::shop_domain::strict_shop_domain;
use crate::types::{Cron, CronShops, WorkerConfig};

pub(crate) const RUN_USAGE: &str = "Usage: run <flow> [--shop=<domain> | --all-shops] [--input=<json object>]";
pub(crate) const CRON_USAGE: &str =
    "Usage: cron run <name>, the name of a cron, its key `<flow>:<schedule>`, or its flow";

/// How often the runs are looked at, until they are over.
const POLL: Duration = Duration::from_millis(250);

/// The shops of `run`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Shops {
    /// The one shop the application is installed for, which it has to be.
    Installed,
    /// `--shop`.
    One(String),
    /// `--all-shops`: every shop a cron of every shop runs for.
    All,
}

/// The command line of `run`.
#[derive(Debug, Clone, PartialEq)]
struct RunArgs {
    flow: String,
    shops: Shops,
    input: Option<Value>,
}

/// The value of `--name=value` or `--name value` at `args[*index]`, which
/// moves past it. `None` when the argument is another one.
fn flag_value(args: &[String], index: &mut usize, name: &str) -> Result<Option<String>> {
    let argument = &args[*index];
    if let Some(value) = argument.strip_prefix(name).and_then(|rest| rest.strip_prefix('=')) {
        return Ok(Some(value.to_string()));
    }
    if argument != name {
        return Ok(None);
    }
    *index += 1;
    match args.get(*index) {
        Some(value) => Ok(Some(value.clone())),
        None => bail!("[openshop] {name} needs a value\n{RUN_USAGE}"),
    }
}

/// Reads `<flow> [--shop=<domain> | --all-shops] [--input=<json>]`.
fn parse_run_args(args: &[String]) -> Result<RunArgs> {
    let mut flow = None;
    let mut shop = None;
    let mut all_shops = false;
    let mut input = None;
    let mut index = 0;
    while index < args.len() {
        if let Some(value) = flag_value(args, &mut index, "--shop")? {
            let domain = strict_shop_domain(&value)
                .ok_or_else(|| anyhow!("[openshop] --shop must be a *.myshopify.com domain, got \"{value}\""))?;
            if shop.replace(domain).is_some() {
                bail!("[openshop] --shop is given twice\n{RUN_USAGE}");
            }
        } else if let Some(value) = flag_value(args, &mut index, "--input")? {
            let parsed: Value =
                serde_json::from_str(&value).map_err(|error| anyhow!("[openshop] --input is not JSON: {error}"))?;
            if !parsed.is_object() {
                bail!("[openshop] --input must be a JSON object, got {parsed}");
            }
            input = Some(parsed);
        } else if args[index] == "--all-shops" {
            all_shops = true;
        } else if args[index].starts_with('-') {
            bail!("[openshop] Unknown argument of run: {}\n{RUN_USAGE}", args[index]);
        } else if flow.replace(args[index].clone()).is_some() {
            bail!("[openshop] run takes one flow\n{RUN_USAGE}");
        }
        index += 1;
    }
    let Some(flow) = flow else {
        bail!("[openshop] Name the flow to run\n{RUN_USAGE}");
    };
    let shops = match (shop, all_shops) {
        (Some(_), true) => bail!("[openshop] --shop and --all-shops go one without the other\n{RUN_USAGE}"),
        (Some(shop), false) => Shops::One(shop),
        (None, true) => Shops::All,
        (None, false) => Shops::Installed,
    };
    Ok(RunArgs { flow, shops, input })
}

/// Reads `run <cron>`, the arguments of `cron`.
fn parse_cron_args(args: &[String]) -> Result<String> {
    match args {
        [command, name] if command == "run" => Ok(name.clone()),
        [command] if command == "run" => bail!("[openshop] Name the cron to run\n{CRON_USAGE}"),
        [command, ..] if command == "run" => bail!("[openshop] cron run takes one cron\n{CRON_USAGE}"),
        [] => bail!("[openshop] Name a cron command\n{CRON_USAGE}"),
        [other, ..] => bail!("[openshop] Unknown cron command: {other}\n{CRON_USAGE}"),
    }
}

/// How a cron is named on the command line: its name, else its key.
fn label(cron: &Cron) -> String {
    match &cron.name {
        Some(name) => format!("\"{name}\" ({})", cron.key()),
        None => cron.key(),
    }
}

/// The cron `name` names: by its name, else its key (`flow:schedule`), else
/// its flow when one cron runs it.
fn find_cron<'a>(crons: &'a [Cron], name: &str) -> Result<&'a Cron> {
    let by = |matches: &dyn Fn(&Cron) -> bool| crons.iter().filter(|cron| matches(cron)).collect::<Vec<_>>();
    let mut found = by(&|cron| cron.name.as_deref() == Some(name));
    if found.is_empty() {
        found = by(&|cron| cron.key() == name);
    }
    if found.is_empty() {
        found = by(&|cron| cron.flow == name);
    }
    let list = |crons: &[&Cron]| crons.iter().map(|cron| label(cron)).collect::<Vec<_>>().join(", ");
    match found.as_slice() {
        [cron] => Ok(cron),
        [] if crons.is_empty() => bail!("[openshop] Unknown cron \"{name}\": the application has no cron"),
        [] => bail!(
            "[openshop] Unknown cron \"{name}\". Crons: {}",
            list(&crons.iter().collect::<Vec<_>>())
        ),
        several => bail!(
            "[openshop] {} crons answer to \"{name}\": name one by its key. {}",
            several.len(),
            list(several)
        ),
    }
}

/// The application of the command, on the database of the environment. The
/// environment is the one of `dev`, `.env` loaded: secrets may be stored in
/// plaintext without an `ENCRYPTION_KEY`, unless it is a production one.
/// Outside production the schema is pushed as `dev` pushes it; in
/// production the database has to be migrated, as for `worker`.
async fn open(config: OpenShopConfig) -> Result<OpenShop> {
    let env = RuntimeEnv {
        development: true,
        ..RuntimeEnv::from_env()
    };
    let pool = crate::db::connect_to(&database_url()).await?;
    if env.production {
        assert_migrated(&pool, &env.cwd).await?;
    } else {
        push_schema(&pool, &config.models, false)
            .await
            .context("[openshop] Schema push failed")?;
    }
    OpenShop::new(config, pool, env)
}

/// `run <flow> [--shop=<domain> | --all-shops] [--input=<json>]`.
pub async fn run_flow_command(config: OpenShopConfig, args: &[String]) -> Result<()> {
    let request = parse_run_args(args)?;
    if !config.flows.contains_key(&request.flow) {
        return Err(unknown_flow(&config, &request.flow));
    }
    let app = open(config).await?;
    let queued = queue_flow(&app, &request).await?;
    finish(&app, &request.flow, queued, shutdown_signal()).await
}

/// `cron run <cron>`.
pub async fn run_cron_command(config: OpenShopConfig, args: &[String]) -> Result<()> {
    let name = parse_cron_args(args)?;
    let cron = find_cron(&config.crons, &name)?.clone();
    let app = open(config).await?;
    println!("[openshop] Running cron {} once, for its shops", label(&cron));
    let queued = queue_cron(&app, &cron).await?;
    finish(&app, &cron.flow, queued, shutdown_signal()).await
}

/// Queues the runs of `request`.
async fn queue_flow(app: &OpenShop, request: &RunArgs) -> Result<Vec<(CronTarget, Queued)>> {
    let targets = match &request.shops {
        Shops::One(shop) => shop_targets(app, &CronShops::Shop(shop.clone())).await?,
        Shops::All => shop_targets(app, &CronShops::All).await?,
        Shops::Installed => {
            let targets = shop_targets(app, &CronShops::All).await?;
            if targets.len() != 1 {
                let shops: Vec<&str> = targets.iter().map(|target| target.shop.as_str()).collect();
                bail!(
                    "[openshop] Name the shop to run \"{}\" for, with --shop=<domain> or --all-shops: {}",
                    request.flow,
                    match shops.as_slice() {
                        [] => "no shop has installed the application".to_string(),
                        shops => format!("{} shops have installed it ({})", shops.len(), shops.join(", ")),
                    }
                );
            }
            targets
        }
    };
    if targets.is_empty() {
        bail!(
            "[openshop] No shop to run \"{}\" for: no shop has installed the application",
            request.flow
        );
    }
    queue_now(app, &request.flow, targets, request.input.as_ref(), None).await
}

/// Queues the runs of `cron`, for the shops its schedule would run it for.
async fn queue_cron(app: &OpenShop, cron: &Cron) -> Result<Vec<(CronTarget, Queued)>> {
    let targets = shop_targets(app, &cron.shops).await?;
    if targets.is_empty() {
        bail!("[openshop] Cron {}: no shop to run for", label(cron));
    }
    queue_now(app, &cron.flow, targets, cron.input.as_ref(), Some(&cron.key())).await
}

/// How a run ended, or the state that is the end of what the command waits for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ended {
    Completed,
    Failed(String),
    Canceled,
    /// The flow sleeps until then: the workers of the application wake it.
    Sleeping(Option<DateTime<Utc>>),
}

/// A run of the command, once it ended.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Finished {
    run_id: Uuid,
    shop: String,
    ended: Ended,
}

/// Says what was queued, runs it, waits until every run is over, and says
/// how each ended. Fails when one could not be queued, failed or was
/// canceled, or when `interrupted` resolves first.
async fn finish(
    app: &OpenShop,
    flow: &str,
    queued: Vec<(CronTarget, Queued)>,
    interrupted: impl Future<Output = ()>,
) -> Result<()> {
    // The shops that turned a cron off have no run to count.
    let total = queued
        .iter()
        .filter(|(_, outcome)| !matches!(outcome, Queued::TurnedOff))
        .count();
    let mut runs = Vec::new();
    let mut not_queued = 0;
    for (target, outcome) in queued {
        match outcome {
            Queued::Run(run) => {
                println!("[openshop] Queued run {} of \"{flow}\" for {}", run.run_id, target.shop);
                runs.push((run.run_id, target.shop));
            }
            Queued::TurnedOff => println!(
                "[openshop] {}: the cron is turned off for this shop, skipped",
                target.shop
            ),
            Queued::Failed(error) => {
                not_queued += 1;
                println!("[openshop] {}: could not queue the run: {error:#}", target.shop);
            }
        }
    }

    let finished = execute(app, &runs, interrupted).await?;
    let mut failed = not_queued;
    for Finished { run_id, shop, ended } in &finished {
        match ended {
            Ended::Completed => println!("[openshop] Run {run_id} for {shop}: completed"),
            Ended::Failed(error) => {
                failed += 1;
                println!("[openshop] Run {run_id} for {shop}: failed: {error}");
            }
            Ended::Canceled => {
                failed += 1;
                println!("[openshop] Run {run_id} for {shop}: canceled");
            }
            Ended::Sleeping(until) => println!(
                "[openshop] Run {run_id} for {shop}: sleeping until {}. A worker of the application (`dev`, `worker`) wakes it then.",
                until.map_or_else(|| "it is woken".to_string(), |until| until.to_rfc3339())
            ),
        }
    }
    if failed > 0 {
        bail!("[openshop] {failed} of {total} runs of \"{flow}\" did not complete");
    }
    Ok(())
}

/// Runs `runs` with a worker of their own, the one `worker` runs but for
/// them alone, and waits until each is over: completed, failed for good,
/// canceled, or asleep. A run another worker takes, such as the one of
/// `dev`, is waited for the same way. The worker is stopped then, or when
/// `interrupted` resolves first, which hands back to the queue what it still
/// runs.
async fn execute(
    app: &OpenShop,
    runs: &[(Uuid, String)],
    interrupted: impl Future<Output = ()>,
) -> Result<Vec<Finished>> {
    if runs.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = runs.iter().map(|(run_id, _)| *run_id).collect();
    let worker = Worker::for_runs(app.clone(), WorkerConfig::default(), ids.clone());
    worker.start().await;
    let ended = tokio::select! {
        ended = wait_for(app, &ids) => ended,
        () = interrupted => Err(anyhow!(
            "[openshop] Interrupted: the runs that did not end are left in the queue, for the workers of the application"
        )),
    };
    worker.stop().await;
    let mut ended = ended?;
    Ok(runs
        .iter()
        .map(|(run_id, shop)| Finished {
            run_id: *run_id,
            shop: shop.clone(),
            ended: ended.remove(run_id).unwrap_or(Ended::Canceled),
        })
        .collect())
}

/// Waits until each of `runs` is over, and says how. An attempt that failed
/// and will be made again is said as it happens.
async fn wait_for(app: &OpenShop, runs: &[Uuid]) -> Result<HashMap<Uuid, Ended>> {
    type Row = (Uuid, String, Option<String>, Option<DateTime<Utc>>, i32);
    let mut ended = HashMap::new();
    let mut retries_said = HashSet::new();
    loop {
        let rows: Vec<Row> =
            sqlx::query_as("SELECT id, status, error, available_at, attempts FROM flow_runs WHERE id = ANY($1)")
                .bind(runs)
                .fetch_all(app.db())
                .await?;
        for run_id in runs {
            if ended.contains_key(run_id) {
                continue;
            }
            let Some((_, status, error, available_at, attempts)) = rows.iter().find(|row| row.0 == *run_id) else {
                ended.insert(*run_id, Ended::Failed("the run was deleted".to_string()));
                continue;
            };
            let over = match status.as_str() {
                "completed" => Ended::Completed,
                "failed" => Ended::Failed(error.clone().unwrap_or_default()),
                "canceled" => Ended::Canceled,
                "sleeping" => Ended::Sleeping(*available_at),
                "pending" if *attempts > 0 && retries_said.insert((*run_id, *attempts)) => {
                    println!(
                        "[openshop] Run {run_id}: attempt {attempts} failed{}, tried again{}",
                        error.as_deref().map(|error| format!(": {error}")).unwrap_or_default(),
                        available_at
                            .map(|at| format!(" at {}", at.to_rfc3339()))
                            .unwrap_or_default()
                    );
                    continue;
                }
                _ => continue,
            };
            ended.insert(*run_id, over);
        }
        if ended.len() == runs.len() {
            return Ok(ended);
        }
        tokio::time::sleep(POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::Flow;
    use crate::testing::{TestContext, TestOptions};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    fn refusal(arguments: &[&str]) -> String {
        parse_run_args(&args(arguments)).unwrap_err().to_string()
    }

    #[test]
    fn reads_the_flow_its_shops_and_its_input() {
        let run = |arguments: &[&str]| parse_run_args(&args(arguments)).unwrap();
        assert_eq!(
            run(&["syncOrders"]),
            RunArgs {
                flow: "syncOrders".into(),
                shops: Shops::Installed,
                input: None
            }
        );
        assert_eq!(
            run(&["syncOrders", "--shop=Demo.myshopify.com", "--input={\"limit\":10}"]),
            RunArgs {
                flow: "syncOrders".into(),
                shops: Shops::One("demo.myshopify.com".into()),
                input: Some(json!({ "limit": 10 }))
            }
        );
        assert_eq!(
            run(&["--shop", "demo", "syncOrders", "--input", "{}"]),
            RunArgs {
                flow: "syncOrders".into(),
                shops: Shops::One("demo.myshopify.com".into()),
                input: Some(json!({}))
            }
        );
        assert_eq!(run(&["--all-shops", "syncOrders"]).shops, Shops::All);
    }

    #[test]
    fn refuses_what_it_cannot_run() {
        assert!(refusal(&[]).starts_with("[openshop] Name the flow to run\nUsage: run <flow>"));
        assert!(refusal(&["a", "b"]).starts_with("[openshop] run takes one flow"));
        assert!(
            refusal(&["a", "--shop=x.myshopify.com", "--all-shops"])
                .starts_with("[openshop] --shop and --all-shops go one without the other")
        );
        assert!(refusal(&["a", "--shop=a", "--shop=b"]).starts_with("[openshop] --shop is given twice"));
        assert_eq!(
            refusal(&["a", "--shop=https://evil.example.com"]),
            "[openshop] --shop must be a *.myshopify.com domain, got \"https://evil.example.com\""
        );
        assert!(refusal(&["a", "--shop"]).starts_with("[openshop] --shop needs a value"));
        assert!(refusal(&["a", "--input={"]).starts_with("[openshop] --input is not JSON: "));
        assert_eq!(
            refusal(&["a", "--input=[1]"]),
            "[openshop] --input must be a JSON object, got [1]"
        );
        assert!(refusal(&["a", "--al-shops"]).starts_with("[openshop] Unknown argument of run: --al-shops"));
    }

    #[test]
    fn reads_the_cron_to_run() {
        assert_eq!(
            parse_cron_args(&args(&["run", "Nightly sync"])).unwrap(),
            "Nightly sync"
        );
        for (arguments, refusal) in [
            (&[][..], "[openshop] Name a cron command"),
            (&["run"][..], "[openshop] Name the cron to run"),
            (&["run", "a", "b"][..], "[openshop] cron run takes one cron"),
            (&["list"][..], "[openshop] Unknown cron command: list"),
        ] {
            let error = parse_cron_args(&args(arguments)).unwrap_err().to_string();
            assert!(error.starts_with(refusal), "{error}");
            assert!(error.ends_with(CRON_USAGE), "{error}");
        }
    }

    #[test]
    fn finds_a_cron_by_its_name_its_key_or_its_flow() {
        let crons = vec![
            Cron::new("0 2 * * *", "sync").name("Nightly sync"),
            Cron::new("*/5 * * * *", "sync"),
            Cron::new("0 4 * * *", "report"),
        ];
        let found = |name| find_cron(&crons, name).map(|cron| cron.key());
        assert_eq!(found("Nightly sync").unwrap(), "sync:0 2 * * *");
        assert_eq!(found("sync:*/5 * * * *").unwrap(), "sync:*/5 * * * *");
        assert_eq!(found("report").unwrap(), "report:0 4 * * *");
        assert_eq!(
            found("sync").unwrap_err().to_string(),
            "[openshop] 2 crons answer to \"sync\": name one by its key. \"Nightly sync\" (sync:0 2 * * *), sync:*/5 * * * *"
        );
        assert_eq!(
            found("nope").unwrap_err().to_string(),
            "[openshop] Unknown cron \"nope\". Crons: \"Nightly sync\" (sync:0 2 * * *), sync:*/5 * * * *, report:0 4 * * *"
        );
        assert_eq!(
            find_cron(&[], "nope").unwrap_err().to_string(),
            "[openshop] Unknown cron \"nope\": the application has no cron"
        );
    }

    /// An application whose flow `record` records the shop and the input of
    /// each run, and whose flow `fail` fails.
    async fn context(seen: Arc<Mutex<Vec<(String, Value)>>>, crons: Vec<Cron>) -> TestContext {
        let config = OpenShopConfig::builder()
            .flow(Flow::new("record", move |ctx| {
                let seen = seen.clone();
                async move {
                    seen.lock().unwrap().push((ctx.shop.clone(), ctx.input.clone()));
                    Ok(())
                }
            }))
            .flow(Flow::new("fail", |_ctx| async { anyhow::bail!("the flow fails") }))
            .crons(crons)
            .build()
            .unwrap();
        TestContext::with_options(config, TestOptions::installed())
            .await
            .unwrap()
    }

    async fn install(ctx: &TestContext, shop: &str) {
        sqlx::query("INSERT INTO installations (shop, access_token, scopes) VALUES ($1, 'token', '')")
            .bind(shop)
            .execute(&ctx.db)
            .await
            .unwrap();
    }

    fn shops(seen: &Mutex<Vec<(String, Value)>>) -> Vec<String> {
        let mut shops: Vec<String> = seen.lock().unwrap().iter().map(|(shop, _)| shop.clone()).collect();
        shops.sort();
        shops
    }

    #[tokio::test]
    async fn runs_a_flow_once_for_its_shops_through_a_worker() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let ctx = context(seen.clone(), Vec::new()).await;
        let request = |shops, input| RunArgs {
            flow: "record".into(),
            shops,
            input,
        };

        // The one shop installed, by default.
        let queued = queue_flow(&ctx.app, &request(Shops::Installed, Some(json!({ "limit": 1 }))))
            .await
            .unwrap();
        finish(&ctx.app, "record", queued, std::future::pending())
            .await
            .unwrap();
        assert_eq!(*seen.lock().unwrap(), [(ctx.shop.clone(), json!({ "limit": 1 }))]);

        // Two shops: one has to be named, or all of them.
        install(&ctx, "second.myshopify.com").await;
        let error = queue_flow(&ctx.app, &request(Shops::Installed, None))
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "[openshop] Name the shop to run \"record\" for, with --shop=<domain> or --all-shops: 2 shops have installed it (test.myshopify.com, second.myshopify.com)"
        );
        seen.lock().unwrap().clear();
        let queued = queue_flow(&ctx.app, &request(Shops::All, None)).await.unwrap();
        finish(&ctx.app, "record", queued, std::future::pending())
            .await
            .unwrap();
        assert_eq!(shops(&seen), ["second.myshopify.com", "test.myshopify.com"]);
        seen.lock().unwrap().clear();
        let queued = queue_flow(&ctx.app, &request(Shops::One("second.myshopify.com".into()), None))
            .await
            .unwrap();
        finish(&ctx.app, "record", queued, std::future::pending())
            .await
            .unwrap();
        assert_eq!(shops(&seen), ["second.myshopify.com"]);

        // The runs went through the queue, and a worker ran them.
        let (completed, by_a_worker): (i64, i64) = sqlx::query_as(
            "SELECT count(*) FILTER (WHERE status = 'completed'), count(*) FILTER (WHERE attempts = 1) FROM flow_runs",
        )
        .fetch_one(&ctx.db)
        .await
        .unwrap();
        assert_eq!((completed, by_a_worker), (4, 4));
        ctx.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn a_run_that_fails_or_cannot_be_queued_fails_the_command() {
        let ctx = context(Arc::default(), Vec::new()).await;
        let request = RunArgs {
            flow: "fail".into(),
            shops: Shops::Installed,
            input: None,
        };
        let queued = queue_flow(&ctx.app, &request).await.unwrap();
        let error = finish(&ctx.app, "fail", queued, std::future::pending())
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "[openshop] 1 of 1 runs of \"fail\" did not complete");
        let (error,): (Option<String>,) = sqlx::query_as("SELECT error FROM flow_runs")
            .fetch_one(&ctx.db)
            .await
            .unwrap();
        assert!(error.unwrap().contains("the flow fails"));

        // A flow without the shop: nothing to queue.
        sqlx::query("DELETE FROM installations").execute(&ctx.db).await.unwrap();
        let error = queue_flow(
            &ctx.app,
            &RunArgs {
                shops: Shops::All,
                ..request
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "[openshop] No shop to run \"fail\" for: no shop has installed the application"
        );
        ctx.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn runs_a_cron_for_its_shops_with_its_input_but_the_shops_that_turned_it_off() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let cron = Cron::new("0 2 * * *", "record")
            .name("Nightly")
            .input(json!({ "from": "cron" }))
            .all_shops();
        let ctx = context(seen.clone(), vec![cron.clone()]).await;
        install(&ctx, "second.myshopify.com").await;
        install(&ctx, "off.myshopify.com").await;
        sqlx::query("INSERT INTO cron_overrides (shop, cron_key, enabled) VALUES ('off.myshopify.com', $1, false)")
            .bind(cron.key())
            .execute(&ctx.db)
            .await
            .unwrap();

        let cron = find_cron(&ctx.app.config().crons, "Nightly").unwrap().clone();
        let queued = queue_cron(&ctx.app, &cron).await.unwrap();
        assert!(matches!(
            queued.iter().find(|(target, _)| target.shop == "off.myshopify.com"),
            Some((_, Queued::TurnedOff))
        ));
        finish(&ctx.app, "record", queued, std::future::pending())
            .await
            .unwrap();
        assert_eq!(shops(&seen), ["second.myshopify.com", "test.myshopify.com"]);
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .all(|(_, input)| *input == json!({ "from": "cron" }))
        );
        // No occurrence was claimed: the schedule fires as it would have.
        let (ticks,): (i64,) = sqlx::query_as("SELECT count(*) FROM cron_ticks")
            .fetch_one(&ctx.db)
            .await
            .unwrap();
        assert_eq!(ticks, 0);
        ctx.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn an_attempt_that_fails_is_retried_as_the_flow_says_and_waited_for() {
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let config = OpenShopConfig::builder()
            .flow(
                Flow::new("flaky", {
                    let attempts = attempts.clone();
                    move |_ctx| {
                        let attempt = attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        async move {
                            if attempt == 0 {
                                anyhow::bail!("the first attempt fails");
                            }
                            Ok(())
                        }
                    }
                })
                .retry_policy(
                    crate::RetryPolicy::new()
                        .max_attempts(2)
                        .initial_interval(Duration::from_millis(10)),
                ),
            )
            .build()
            .unwrap();
        let ctx = TestContext::with_options(config, TestOptions::installed())
            .await
            .unwrap();
        let request = RunArgs {
            flow: "flaky".into(),
            shops: Shops::Installed,
            input: None,
        };
        let queued = queue_flow(&ctx.app, &request).await.unwrap();
        finish(&ctx.app, "flaky", queued, std::future::pending()).await.unwrap();
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
        ctx.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn an_interrupted_command_leaves_its_runs_to_the_workers() {
        let ctx = context(Arc::default(), Vec::new()).await;
        let request = RunArgs {
            flow: "record".into(),
            shops: Shops::Installed,
            input: None,
        };
        let queued = queue_flow(&ctx.app, &request).await.unwrap();
        let error = finish(&ctx.app, "record", queued, std::future::ready(()))
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "[openshop] Interrupted: the runs that did not end are left in the queue, for the workers of the application"
        );
        ctx.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn the_worker_of_the_command_takes_its_runs_only() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let ctx = context(seen.clone(), Vec::new()).await;
        // A run of another shop: a flow rejects a second run of a shop by default.
        let other = ctx
            .dispatch_flow_for("record", json!({ "queued": "elsewhere" }), "other.myshopify.com")
            .await
            .unwrap();

        let request = RunArgs {
            flow: "record".into(),
            shops: Shops::Installed,
            input: Some(json!({ "mine": true })),
        };
        let queued = queue_flow(&ctx.app, &request).await.unwrap();
        finish(&ctx.app, "record", queued, std::future::pending())
            .await
            .unwrap();

        assert_eq!(*seen.lock().unwrap(), [(ctx.shop.clone(), json!({ "mine": true }))]);
        let (status,): (String,) = sqlx::query_as("SELECT status FROM flow_runs WHERE id = $1")
            .bind(other)
            .fetch_one(&ctx.db)
            .await
            .unwrap();
        assert_eq!(status, "pending");
        ctx.shutdown().await.unwrap();
    }
}
