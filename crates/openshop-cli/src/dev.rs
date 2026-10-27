//! `openshop dev`: runs `cargo run -- dev` and restarts it when the project
//! changes, or a crate on disk it is compiled with.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc, watch};

use crate::{bundle, metadata};

/// Directories watched recursively, including ones created after startup.
const WATCHED_DIRECTORIES: [&str; 2] = ["src", "migrations"];
const DEBOUNCE: Duration = Duration::from_millis(500);
/// A worker may be finishing a flow run: give it one lease before killing it.
const PROCESS_EXIT_GRACE: Duration = Duration::from_secs(35);

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

struct DevProcess {
    cargo_args: Vec<String>,
    /// The admin UI bundle the app serves, when the project builds its own.
    ui_dir: std::sync::Mutex<Option<PathBuf>>,
    child: Mutex<Option<Child>>,
}

/// The admin UI bundle of the project, rebuilt by trunk while `dev` runs.
struct BundleWatch {
    cwd: PathBuf,
    trunk: Option<Child>,
    /// trunk could not be started: said once, not at every change.
    unavailable: bool,
}

impl BundleWatch {
    /// Writes the crate of the bundle again, and starts or stops trunk to
    /// match. Returns whether the project has a bundle of its own.
    async fn refresh(&mut self) -> bool {
        match bundle::prepare(&self.cwd).await {
            Ok(Some(prepared)) => {
                if prepared.watch_changed {
                    self.stop().await;
                }
                if self.trunk.is_none() && !self.unavailable {
                    self.trunk = bundle::watch(&prepared);
                    self.unavailable = self.trunk.is_none();
                }
                true
            }
            Ok(None) => {
                self.stop().await;
                false
            }
            // The sources may be half written: what is running is kept.
            Err(error) => {
                eprintln!("{error:#}");
                self.trunk.is_some()
            }
        }
    }

    async fn stop(&mut self) {
        if let Some(mut trunk) = self.trunk.take() {
            let _ = trunk.kill().await;
        }
    }
}

impl DevProcess {
    fn ui_dir(&self) -> Option<PathBuf> {
        self.ui_dir
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn serve(&self, ui_dir: Option<PathBuf>) {
        *self.ui_dir.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = ui_dir;
    }

    fn spawn(&self) -> Result<Child> {
        Command::new("cargo")
            .args(["run"])
            .args(&self.cargo_args)
            .args(["--", "dev"])
            .envs(self.ui_dir().map(|directory| ("OPENSHOP_UI_DIR", directory)))
            .kill_on_drop(true)
            .spawn()
            .context("[openshop] Failed to run cargo")
    }

    /// Asks the process to shut down gracefully, then kills it after the grace period.
    async fn stop(child: &mut Child) {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        let asked = match child.id() {
            #[cfg(unix)]
            Some(pid) => Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status()
                .await
                .is_ok_and(|status| status.success()),
            _ => false,
        };
        if asked && tokio::time::timeout(PROCESS_EXIT_GRACE, child.wait()).await.is_ok() {
            return;
        }
        if asked {
            eprintln!("[openshop] Force-killing stale app process");
        }
        let _ = child.kill().await;
    }

    async fn start(&self) {
        match self.spawn() {
            Ok(child) => *self.child.lock().await = Some(child),
            Err(error) => eprintln!("{error:#}"),
        }
    }

    async fn shutdown(&self) {
        if let Some(mut child) = self.child.lock().await.take() {
            Self::stop(&mut child).await;
        }
    }

    async fn restart(&self, reason: String) {
        println!("[openshop] {reason} changed — restarting...");
        self.shutdown().await;
        self.start().await;
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
    /// The last file that changed: the one the restart is reported for.
    source: String,
    /// A view may have been added to the project, or the last one removed.
    pages: bool,
    /// A manifest changed: the project may be compiled with other crates.
    dependencies: bool,
}

async fn terminate_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut terminate) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
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

    // A project whose admin pages have a view serves its own bundle, rebuilt by trunk.
    let mut bundle = BundleWatch {
        cwd: cwd.clone(),
        trunk: None,
        unavailable: false,
    };
    let chosen = std::env::var_os("OPENSHOP_UI_DIR").is_some();
    let ui_dir = |built: bool| (built && !chosen).then(|| bundle::dist(&cwd));

    let process = Arc::new(DevProcess {
        cargo_args: cargo_args.to_vec(),
        ui_dir: std::sync::Mutex::new(ui_dir(bundle.refresh().await)),
        child: Mutex::new(None),
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

    loop {
        let debounce = async {
            match &pending {
                Some(_) => tokio::time::sleep(DEBOUNCE).await,
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            _ = &mut shutdown => break,
            Some(path) = changed.recv() => {
                // A watched directory may have just been created.
                watch_existing(&mut watcher, &cwd, &local, &mut watched);
                let created_directory = WATCHED_DIRECTORIES
                    .iter()
                    .find(|name| cwd.join(name) == path)
                    .map(|name| name.to_string());
                let project = watched_source(&cwd, &path).or(created_directory);
                let pages = project.as_ref().is_some_and(|source| source.starts_with("src"));
                if let Some(source) = project.or_else(|| local.source(&path)) {
                    let manifest = source.rsplit('/').next().is_some_and(|name| name.starts_with("Cargo."));
                    pending = Some(Change {
                        pages: pages || pending.as_ref().is_some_and(|change| change.pages),
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
                    // A view may have been added, or the last one removed, and
                    // trunk builds the bundle again when one of the crates changes.
                    if change.pages || change.dependencies {
                        process.serve(ui_dir(bundle.refresh().await));
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

    restarter.abort();
    process.shutdown().await;
    bundle.stop().await;
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
