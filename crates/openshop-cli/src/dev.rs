//! `openshop dev`: runs `cargo run -- dev` and restarts it when the project
//! changes, or a crate on disk it is compiled with.

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc, watch};

use crate::{legacy, metadata};

/// Directories watched recursively, including ones created after startup.
const WATCHED_DIRECTORIES: [&str; 3] = ["src", "migrations", "locales"];
/// The files of `locales/`, which `t!` and `app!` read when the crate
/// compiles: cargo compiles it again when they are not the same files.
const LOCALE_FILES_VARIABLE: &str = "OPENSHOP_LOCALE_FILES";
const DEBOUNCE: Duration = Duration::from_millis(500);
/// A worker may be finishing a flow run: give it one lease before killing it.
const PROCESS_EXIT_GRACE: Duration = Duration::from_secs(35);
/// How long the processes of the app have to go once killed.
const KILL_WAIT: Duration = Duration::from_secs(5);
/// How often a stopping app, or a port, is looked at again.
const POLL: Duration = Duration::from_millis(100);
/// How long a port that is still taken is waited for before saying so.
const PORT_WAIT: Duration = Duration::from_secs(5);
/// The port of `dev` when `PORT` is not set.
const DEFAULT_PORT: u16 = 3000;

/// Whether a change to a top-level file should restart the app.
fn is_watched_root_file(name: &str) -> bool {
    matches!(name, "Cargo.toml" | "Cargo.lock" | ".env") || (name.starts_with("shopify.app") && name.ends_with(".toml"))
}

fn slashed(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Whether a file is one an editor writes beside the one being edited.
fn hidden_or_temporary(source: &str) -> bool {
    source
        .split('/')
        .any(|segment| segment.starts_with('.') || segment.ends_with('~'))
}

/// The project-relative path to report for a changed file, or `None` to ignore it.
pub fn watched_source(cwd: &Path, path: &Path) -> Option<String> {
    let source = slashed(path.strip_prefix(cwd).ok()?);
    let mut segments = source.split('/');
    let first = segments.next()?;

    if segments.next().is_none() {
        return is_watched_root_file(first).then_some(source);
    }
    (WATCHED_DIRECTORIES.contains(&first) && !hidden_or_temporary(&source)).then_some(source)
}

/// The crates on disk the project is compiled with: the other crates of its
/// workspace, most of the time.
#[derive(Default)]
struct LocalCrates {
    /// What their changes are named from: the root of the workspace.
    root: PathBuf,
    directories: Vec<PathBuf>,
}

impl LocalCrates {
    /// `None` when cargo cannot say: the manifest may be half written.
    async fn read(cwd: &Path, cargo_args: &[String]) -> Option<Self> {
        let metadata = match metadata::read(cwd).await {
            Ok(metadata) => metadata,
            Err(error) => {
                eprintln!("{error:#}");
                return None;
            }
        };
        // What the watcher reports is canonical, like `cwd`.
        let canonical = |path: PathBuf| path.canonicalize().unwrap_or(path);
        let manifest = cwd.join("Cargo.toml");
        Some(Self {
            root: canonical(
                metadata["workspace_root"]
                    .as_str()
                    .map_or_else(|| cwd.to_path_buf(), PathBuf::from),
            ),
            directories: metadata::local_crates(&metadata, &manifest, requested_package(cargo_args))
                .into_iter()
                .map(canonical)
                .collect(),
        })
    }

    /// The path to report for a changed file of one of the crates, or `None` to ignore it.
    fn source(&self, path: &Path) -> Option<String> {
        let source = self
            .directories
            .iter()
            .filter_map(|directory| path.strip_prefix(directory).ok())
            .map(slashed)
            .min_by_key(String::len)?;
        let watched = matches!(source.as_str(), "Cargo.toml" | "build.rs")
            || (source.starts_with("src/") && !hidden_or_temporary(&source));
        watched.then(|| slashed(path.strip_prefix(&self.root).unwrap_or(path)))
    }

    /// The crates, as the startup message names them: from the root of the
    /// workspace, and by the name of their directory when they are elsewhere.
    fn names(&self) -> String {
        let names: Vec<String> = self
            .directories
            .iter()
            .map(|directory| match directory.strip_prefix(&self.root) {
                Ok(relative) => slashed(relative),
                Err(_) => slashed(directory.file_name().map_or(directory.as_path(), Path::new)),
            })
            .collect();
        names.join(", ")
    }
}

/// The package `cargo run` is asked for: `-p my-app`, `--package=my-app`.
fn requested_package(cargo_args: &[String]) -> Option<&str> {
    let mut args = cargo_args.iter().map(String::as_str);
    while let Some(arg) = args.next() {
        let package = match arg {
            "-p" | "--package" => args.next(),
            // The arguments of the app follow.
            "--" => return None,
            _ => arg.strip_prefix("--package=").or_else(|| arg.strip_prefix("-p")),
        };
        if let Some(package) = package {
            return package.split('@').next();
        }
    }
    None
}

/// The port the app listens on: `PORT`, else the `PORT` of `.env`, which the
/// app loads for a variable the environment does not set, else 3000. `0` is
/// not a port, as for the app.
fn app_port(env_port: Option<&str>, dot_env: Option<&str>) -> u16 {
    let parse = |value: &str| value.trim().parse::<u16>().ok().filter(|port| *port != 0);
    match env_port {
        Some(value) => parse(value),
        None => dot_env
            .and_then(|content| {
                content.lines().find_map(|line| {
                    let line = line.trim();
                    let line = line
                        .strip_prefix("export")
                        .filter(|rest| rest.starts_with(char::is_whitespace))
                        .map_or(line, str::trim_start);
                    let (key, value) = line.split_once('=')?;
                    (key.trim_end() == "PORT").then(|| value.trim().trim_matches(|quote| quote == '"' || quote == '\''))
                })
            })
            .and_then(parse),
    }
    .unwrap_or(DEFAULT_PORT)
}

/// Whether a process answers on `port` of this machine, in IPv4 or IPv6:
/// what `http://localhost:{port}` reaches. It is asked by connecting, not by
/// listening, which a firewall would ask the user to allow.
async fn port_taken(port: u16) -> bool {
    let answers = |address: std::net::IpAddr| async move {
        let connect = tokio::net::TcpStream::connect((address, port));
        matches!(
            tokio::time::timeout(Duration::from_millis(500), connect).await,
            Ok(Ok(_))
        )
    };
    answers(std::net::Ipv4Addr::LOCALHOST.into()).await || answers(std::net::Ipv6Addr::LOCALHOST.into()).await
}

/// The process that listens on `port`, `PID 123 (name)`, when `lsof` can say.
async fn port_holder(port: u16) -> Option<String> {
    let output = Command::new("lsof")
        .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fpc"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let field = |prefix: char| {
        text.lines()
            .find_map(|line| line.strip_prefix(prefix))
            .map(str::to_string)
    };
    let pid = field('p')?;
    Some(match field('c') {
        Some(name) => format!("PID {pid} ({name})"),
        None => format!("PID {pid}"),
    })
}

/// Waits for `port` to be free, for up to [`PORT_WAIT`]. When it is still
/// taken, says so loudly: the app started now cannot listen on it, and what
/// answers there is not its code.
async fn check_port(port: u16) {
    let deadline = tokio::time::Instant::now() + PORT_WAIT;
    while port_taken(port).await {
        if tokio::time::Instant::now() >= deadline {
            let holder = port_holder(port).await.map_or_else(
                || "another process".to_string(),
                |holder| format!("another process, {holder}"),
            );
            eprintln!(
                "\n[openshop] !!! Port {port} is still taken, by {holder}.\n\
                 [openshop] !!! The app starting now cannot listen on it: what answers on http://localhost:{port} is NOT this code.\n\
                 [openshop] !!! Stop that process (`lsof -nP -iTCP:{port} -sTCP:LISTEN` names it), then save a file to restart.\n"
            );
            return;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Sends `signal` (`-TERM`, `-KILL`, `-0` to only ask whether it is there)
/// to every process of the group `group`. False when no process of it was
/// there to receive it.
#[cfg(unix)]
async fn signal_group(group: u32, signal: &str) -> bool {
    Command::new("kill")
        .args([signal, "--", &format!("-{group}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
}

/// The app as `dev` runs it: `cargo run`, and what that starts.
///
/// `cargo run` may not be the app itself. Cargo replaces itself with the app
/// on unix, but a `cargo` on the `PATH` may be a wrapper that runs the real one
/// as its child (a shim of a tool manager), and so may a `runner` of the cargo
/// configuration: a signal to the process `dev` started would not reach the
/// app, which would keep its port and serve the code it was built from. On
/// unix the process leads a process group of its own, which what it starts
/// joins, and the group is what is stopped and waited for.
struct AppProcess {
    child: Child,
    /// The process group of the app: the id of the process `dev` started.
    #[cfg(unix)]
    group: Option<u32>,
    /// Every process of the group is gone: nothing is left to kill when this is dropped.
    gone: bool,
    /// Its exit was told, when it exited by itself.
    exit_reported: bool,
}

impl AppProcess {
    /// Starts `command` as the app: in a process group of its own on unix,
    /// without the terminal's input, which a process outside the group of
    /// the terminal may not read.
    fn spawn(command: &mut Command) -> std::io::Result<Self> {
        command.stdin(Stdio::null()).kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let child = command.spawn()?;
        Ok(Self {
            #[cfg(unix)]
            group: child.id(),
            child,
            gone: false,
            exit_reported: false,
        })
    }

    /// How the process `dev` started exited, once it has: a wrapper may
    /// leave the app behind it, which [`AppProcess::stop`] stops.
    fn exited(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Whether a process of the app is still there.
    async fn running(&mut self) -> bool {
        // The process `dev` started is reaped first: until it is, it is
        // still a member of its group.
        let exited = self.exited().is_some();
        #[cfg(unix)]
        if let Some(group) = self.group {
            return signal_group(group, "-0").await;
        }
        !exited
    }

    /// Waits for every process of the app to be gone, for up to `timeout`.
    async fn gone_within(&mut self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        while self.running().await {
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(POLL).await;
        }
        self.gone = true;
        true
    }

    /// Asks every process of the app to shut down gracefully, waits for them
    /// for up to `grace`, then kills them. False when some are still there.
    async fn stop(&mut self, grace: Duration) -> bool {
        if !self.running().await {
            self.gone = true;
            return true;
        }
        #[cfg(unix)]
        if let Some(group) = self.group {
            signal_group(group, "-TERM").await;
            if self.gone_within(grace).await {
                return true;
            }
            eprintln!("[openshop] Force-killing stale app process");
            signal_group(group, "-KILL").await;
            let _ = self.child.kill().await;
            return self.gone_within(KILL_WAIT).await;
        }
        let _ = grace;
        let _ = self.child.kill().await;
        self.gone_within(KILL_WAIT).await
    }
}

impl Drop for AppProcess {
    /// `dev` stops its app before it exits. If it is torn down without that
    /// (a panic), the whole app goes with it, not only the process it started.
    fn drop(&mut self) {
        if self.gone {
            return;
        }
        #[cfg(unix)]
        if let Some(group) = self.group {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", "--", &format!("-{group}")])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

/// The names of the files of `locales/`, sorted: a file added or removed
/// changes them.
fn locale_files(cwd: &Path) -> String {
    let mut names: Vec<String> = std::fs::read_dir(cwd.join("locales"))
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names.join(",")
}

struct DevProcess {
    cwd: PathBuf,
    cargo_args: Vec<String>,
    child: Mutex<Option<AppProcess>>,
    /// Set once `dev` shuts down: a restart still under way then stops the
    /// app without starting it again.
    closing: AtomicBool,
}

impl DevProcess {
    fn spawn(&self) -> Result<AppProcess> {
        AppProcess::spawn(
            Command::new("cargo")
                .args(["run"])
                .args(&self.cargo_args)
                .args(["--", "dev"])
                .env(LOCALE_FILES_VARIABLE, locale_files(&self.cwd)),
        )
        .context("[openshop] Failed to run cargo")
    }

    /// The port the app is about to listen on.
    fn port(&self) -> u16 {
        let dot_env = std::fs::read_to_string(self.cwd.join(".env")).ok();
        app_port(std::env::var("PORT").ok().as_deref(), dot_env.as_deref())
    }

    async fn start(&self) {
        if self.closing.load(Ordering::SeqCst) {
            return;
        }
        // A process that still holds the port would keep serving the code it
        // was built from, while the new app fails to listen.
        check_port(self.port()).await;
        match self.spawn() {
            Ok(child) => *self.child.lock().await = Some(child),
            Err(error) => eprintln!("{error:#}"),
        }
    }

    async fn shutdown(&self) {
        if let Some(mut child) = self.child.lock().await.take()
            && !child.stop(PROCESS_EXIT_GRACE).await
        {
            eprintln!(
                "\n[openshop] !!! The app is still running after being killed: it may keep its port, and serve the code it was built from.\n"
            );
        }
    }

    async fn restart(&self, reason: String) {
        println!("[openshop] {reason} changed — restarting...");
        self.shutdown().await;
        self.start().await;
    }

    /// Tells, once, that the app exited by itself: it failed to build, to
    /// start, or to listen. Nothing runs until the next change.
    async fn report_exit(&self) {
        // A restart under way holds the app: it is the restart's to tell.
        let Ok(mut child) = self.child.try_lock() else {
            return;
        };
        let Some(child) = child.as_mut().filter(|child| !child.exit_reported) else {
            return;
        };
        if let Some(status) = child.exited() {
            child.exit_reported = true;
            // A wrapper of cargo that went alone leaves the app running.
            if child.running().await {
                eprintln!(
                    "\n[openshop] !!! `cargo run` exited ({status}), and what it started still runs: the app may serve the code it was built from until the next change restarts it.\n"
                );
                return;
            }
            eprintln!(
                "\n[openshop] !!! The app exited ({status}): nothing of the current code is serving. Fix what it printed, then save a file to restart it.\n"
            );
        }
    }
}

/// Restarts `process` for each change `changed` receives, one restart at a
/// time: the changes made during a restart become one more restart, for the
/// last of them. `restarting` is set while a restart runs, and taken back by
/// the first change that queues another.
async fn restart_on_change(
    process: Arc<DevProcess>,
    mut changed: watch::Receiver<String>,
    restarting: Arc<AtomicBool>,
) {
    while changed.changed().await.is_ok() {
        let reason = changed.borrow_and_update().clone();
        restarting.store(true, Ordering::SeqCst);
        process.restart(reason).await;
        restarting.store(false, Ordering::SeqCst);
    }
}

/// Watches the source directories of the project that exist, and the ones of
/// the crates on disk it is compiled with. `watched` is what is watched
/// already, so that new directories can be added later.
fn watch_existing(watcher: &mut RecommendedWatcher, cwd: &Path, local: &LocalCrates, watched: &mut Vec<PathBuf>) {
    let sources = WATCHED_DIRECTORIES.map(|name| (cwd.join(name), RecursiveMode::Recursive));
    let crates = local.directories.iter().flat_map(|directory| {
        [
            (directory.clone(), RecursiveMode::NonRecursive),
            (directory.join("src"), RecursiveMode::Recursive),
        ]
    });
    for (directory, mode) in sources.into_iter().chain(crates) {
        if directory.is_dir() && !watched.contains(&directory) && watcher.watch(&directory, mode).is_ok() {
            watched.push(directory);
        }
    }
}

/// What changed since the app was last started.
struct Change {
    /// When the restart is due: the debounce after the last file that changed.
    /// Events of files that are not watched leave it as it is.
    due: tokio::time::Instant,
    /// The last file that changed: the one the restart is reported for.
    source: String,
    /// A manifest changed: the project may be compiled with other crates.
    dependencies: bool,
}

/// Resolves when `dev` is asked to stop: Ctrl-C, `SIGTERM`, or `SIGHUP` when
/// its terminal closes. The app is in a process group of its own, which
/// neither the terminal nor the shell signals: `dev` stops it.
async fn terminate_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let (Ok(mut terminate), Ok(mut hangup)) = (signal(SignalKind::terminate()), signal(SignalKind::hangup())) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
                _ = hangup.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

/// `cargo_args` are passed to `cargo run` (for example `-p my-app` or `--release`).
pub async fn start_dev(cargo_args: &[String]) -> Result<()> {
    let cwd = std::env::current_dir()?.canonicalize()?;
    println!("[openshop] Starting dev server...");
    // cargo, or the admin, fails next: what to do comes first.
    let leftovers = legacy::leftovers(&cwd);
    if !leftovers.is_empty() {
        eprintln!("{}", legacy::warning(&leftovers));
    }

    let process = Arc::new(DevProcess {
        cwd: cwd.clone(),
        cargo_args: cargo_args.to_vec(),
        child: Mutex::new(None),
        closing: AtomicBool::new(false),
    });
    let mut local = LocalCrates::read(&cwd, cargo_args).await.unwrap_or_default();
    process.start().await;
    let (restart, restarts) = watch::channel(String::new());
    let restarting = Arc::new(AtomicBool::new(false));
    let restarter = tokio::spawn(restart_on_change(process.clone(), restarts, restarting.clone()));

    let (changes, mut changed) = mpsc::unbounded_channel::<PathBuf>();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event
            && !event.kind.is_access()
        {
            for path in event.paths {
                let _ = changes.send(path);
            }
        }
    })?;
    watcher.watch(&cwd, RecursiveMode::NonRecursive)?;
    let mut watched = Vec::new();
    watch_existing(&mut watcher, &cwd, &local, &mut watched);
    println!("[openshop] Watching for changes (src, migrations, Cargo.toml, .env, shopify.app*.toml)");
    if !local.directories.is_empty() {
        println!("[openshop] Watching the crates it is compiled with ({})", local.names());
    }

    let shutdown = terminate_signal();
    tokio::pin!(shutdown);
    let mut pending: Option<Change> = None;
    // An app that exits by itself is told about.
    let mut exits = tokio::time::interval(Duration::from_secs(1));
    exits.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        let due = pending.as_ref().map(|change| change.due);
        let debounce = async move {
            match due {
                Some(due) => tokio::time::sleep_until(due).await,
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            _ = &mut shutdown => break,
            _ = exits.tick() => process.report_exit().await,
            Some(path) = changed.recv() => {
                // A watched directory may have just been created.
                watch_existing(&mut watcher, &cwd, &local, &mut watched);
                let created_directory = WATCHED_DIRECTORIES
                    .iter()
                    .find(|name| cwd.join(name) == path)
                    .map(|name| name.to_string());
                let project = watched_source(&cwd, &path).or(created_directory);
                if let Some(source) = project.or_else(|| local.source(&path)) {
                    let manifest = source.rsplit('/').next().is_some_and(|name| name.starts_with("Cargo."));
                    pending = Some(Change {
                        due: tokio::time::Instant::now() + DEBOUNCE,
                        dependencies: manifest || pending.as_ref().is_some_and(|change| change.dependencies),
                        source,
                    });
                }
            }
            _ = debounce => {
                if let Some(change) = pending.take() {
                    if change.dependencies && let Some(read) = LocalCrates::read(&cwd, cargo_args).await {
                        local = read;
                        watch_existing(&mut watcher, &cwd, &local, &mut watched);
                    }
                    if restarting.swap(false, Ordering::SeqCst) {
                        println!(
                            "[openshop] Reload already in progress — queued one more restart for {}",
                            change.source
                        );
                    }
                    restart.send_replace(change.source);
                }
            }
        }
    }

    // A restart under way ends first: aborting it would drop the app it is
    // stopping, which kills it without its grace period.
    process.closing.store(true, Ordering::SeqCst);
    drop(restart);
    let _ = restarter.await;
    process.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_changes_in_watched_locations_only() {
        let cwd = Path::new("/project");
        let source = |path: &str| watched_source(cwd, &cwd.join(path));

        assert_eq!(source("src/main.rs").as_deref(), Some("src/main.rs"));
        assert_eq!(
            source("src/flows/sync_orders.rs").as_deref(),
            Some("src/flows/sync_orders.rs")
        );
        assert_eq!(
            source("migrations/0001_init.sql").as_deref(),
            Some("migrations/0001_init.sql")
        );
        assert_eq!(source("locales/fr.json").as_deref(), Some("locales/fr.json"));
        assert_eq!(source("Cargo.toml").as_deref(), Some("Cargo.toml"));
        assert_eq!(source(".env").as_deref(), Some(".env"));
        assert_eq!(source("shopify.app.toml").as_deref(), Some("shopify.app.toml"));
        assert_eq!(source("shopify.app.dev.toml").as_deref(), Some("shopify.app.dev.toml"));

        assert_eq!(source("target/debug/app"), None);
        assert_eq!(source("README.md"), None);
        assert_eq!(source("src/.main.rs.swp"), None);
        assert_eq!(source("src/main.rs~"), None);
        assert_eq!(source(".git/HEAD"), None);
        assert_eq!(source("docs/guide.md"), None);
        assert_eq!(watched_source(cwd, Path::new("/elsewhere/src/main.rs")), None);
    }

    #[test]
    fn reports_changes_of_the_crates_the_project_is_compiled_with() {
        let local = LocalCrates {
            root: PathBuf::from("/work"),
            directories: vec![
                PathBuf::from("/work/crates/charts"),
                PathBuf::from("/elsewhere/palette"),
            ],
        };
        let source = |path: &str| local.source(Path::new(path));
        assert_eq!(local.names(), "crates/charts, palette");

        // Named from the root of the workspace, when they are in it.
        assert_eq!(
            source("/work/crates/charts/src/axis/mod.rs").as_deref(),
            Some("crates/charts/src/axis/mod.rs")
        );
        assert_eq!(
            source("/work/crates/charts/Cargo.toml").as_deref(),
            Some("crates/charts/Cargo.toml")
        );
        assert_eq!(
            source("/work/crates/charts/build.rs").as_deref(),
            Some("crates/charts/build.rs")
        );
        assert_eq!(
            source("/elsewhere/palette/src/lib.rs").as_deref(),
            Some("/elsewhere/palette/src/lib.rs")
        );

        assert_eq!(source("/work/crates/charts/README.md"), None);
        assert_eq!(source("/work/crates/charts/src/.lib.rs.swp"), None);
        assert_eq!(source("/work/crates/charts/src/lib.rs~"), None);
        assert_eq!(source("/work/crates/charts/tests/axis.rs"), None);
        assert_eq!(source("/work/crates/charts/target/debug/build.rs"), None);
        assert_eq!(source("/work/crates/other/src/lib.rs"), None);
    }

    #[test]
    fn reads_the_port_of_the_app_as_the_app_does() {
        assert_eq!(app_port(None, None), 3000);
        assert_eq!(app_port(Some("4000"), Some("PORT=5000")), 4000);
        assert_eq!(app_port(None, Some("# PORT=1\nHOST=x\nPORT=5000\n")), 5000);
        assert_eq!(app_port(None, Some("export PORT=\"5001\"")), 5001);
        assert_eq!(app_port(None, Some("PORT='5002'")), 5002);
        assert_eq!(app_port(None, Some("SHOPIFY_PORT=5003")), 3000);
        // What the app does not take as a port is 3000 for it too.
        assert_eq!(app_port(Some("0"), None), 3000);
        assert_eq!(app_port(Some("http"), Some("PORT=5000")), 3000);
    }

    #[tokio::test]
    async fn sees_a_port_another_process_listens_on() {
        let listener = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(port_taken(port).await);
        drop(listener);
        // A process another test is starting may hold the socket until it
        // runs its program, which closes it.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while port_taken(port).await {
            assert!(tokio::time::Instant::now() < deadline, "the port is still taken");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Whether the process `pid` is there.
    #[cfg(unix)]
    async fn alive(pid: u32) -> bool {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .await
            .is_ok_and(|status| status.success())
    }

    /// Starts a wrapper that runs `app` (a command of `sh`) as its child, as
    /// a `cargo` shim runs cargo, and returns it with the id of the app, once
    /// the app runs `sleep`: a signal sent before would reach the shell that
    /// starts it, not the app.
    #[cfg(unix)]
    async fn wrapper(app: &str, name: &str) -> (AppProcess, u32) {
        let pid_file = std::env::temp_dir().join(format!("openshop-dev-{name}-{}.pid", std::process::id()));
        let _ = std::fs::remove_file(&pid_file);
        let script = format!("{app} & echo $! > \"$0.tmp\" && mv \"$0.tmp\" \"$0\"; wait");
        let wrapper = AppProcess::spawn(Command::new("sh").arg("-c").arg(script).arg(&pid_file)).unwrap();
        let pid: u32 = loop {
            if let Ok(pid) = std::fs::read_to_string(&pid_file) {
                break pid.trim().parse().unwrap();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let _ = std::fs::remove_file(&pid_file);
        loop {
            let command = Command::new("ps")
                .args(["-o", "comm=", "-p", &pid.to_string()])
                .output()
                .await
                .unwrap();
            if String::from_utf8_lossy(&command.stdout).trim().ends_with("sleep") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        (wrapper, pid)
    }

    /// The bug of a `cargo` that is a wrapper: it runs the app as its child,
    /// and exits on `SIGTERM` without passing it on. The app ignores the
    /// signal too; it was left listening, serving the code of before.
    #[cfg(unix)]
    #[tokio::test]
    async fn stopping_the_app_stops_what_a_wrapper_started() {
        let (mut app, pid) = wrapper("(trap '' TERM; exec sleep 300)", "ignores-term").await;
        assert!(alive(pid).await);

        assert!(app.stop(Duration::from_millis(300)).await);
        // The wrapper went at once, the app after the grace period.
        assert!(app.exited().is_some());
        assert!(!alive(pid).await, "the app outlived its wrapper");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_app_that_stops_on_sigterm_is_not_killed() {
        let (mut app, pid) = wrapper("sleep 300", "stops-on-term").await;
        let started = tokio::time::Instant::now();
        assert!(app.stop(Duration::from_secs(30)).await);
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!alive(pid).await);
        // Stopped already: nothing to do.
        assert!(app.stop(Duration::from_secs(30)).await);
    }

    #[test]
    fn reads_the_package_cargo_run_is_asked_for() {
        let requested = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
            requested_package(&args).map(str::to_string)
        };
        assert_eq!(requested(&[]), None);
        assert_eq!(requested(&["--release"]), None);
        assert_eq!(requested(&["-p", "my-app"]).as_deref(), Some("my-app"));
        assert_eq!(
            requested(&["--release", "--package", "my-app"]).as_deref(),
            Some("my-app")
        );
        assert_eq!(requested(&["--package=my-app"]).as_deref(), Some("my-app"));
        assert_eq!(requested(&["-pmy-app@0.1.0"]).as_deref(), Some("my-app"));
        // After `--`, the arguments are the ones of the app.
        assert_eq!(requested(&["--", "-p", "my-app"]), None);
    }
}
