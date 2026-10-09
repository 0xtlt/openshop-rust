//! `openshop dev`: runs `cargo run -- dev` and restarts it when the project changes.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc};

use crate::bundle;
use crate::restart::{RestartCoordinator, Restartable};

/// Directories watched recursively, including ones created after startup.
const WATCHED_DIRECTORIES: [&str; 2] = ["src", "migrations"];
const DEBOUNCE: Duration = Duration::from_millis(500);
/// A worker may be finishing a flow run: give it one lease before killing it.
const PROCESS_EXIT_GRACE: Duration = Duration::from_secs(35);

/// Whether a change to a top-level file should restart the app.
fn is_watched_root_file(name: &str) -> bool {
    matches!(name, "Cargo.toml" | "Cargo.lock" | ".env") || (name.starts_with("shopify.app") && name.ends_with(".toml"))
}

/// The project-relative path to report for a changed file, or `None` to ignore it.
pub fn watched_source(cwd: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(cwd).ok()?;
    let source = relative.to_string_lossy().replace('\\', "/");
    let mut segments = source.split('/');
    let first = segments.next()?;

    if segments.next().is_none() {
        return is_watched_root_file(first).then_some(source);
    }
    let hidden_or_temporary = source
        .split('/')
        .any(|segment| segment.starts_with('.') || segment.ends_with('~'));
    (WATCHED_DIRECTORIES.contains(&first) && !hidden_or_temporary).then_some(source)
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
}

impl Restartable for DevProcess {
    async fn restart(&self, reason: String) {
        println!("[openshop] {reason} changed — restarting...");
        self.shutdown().await;
        self.start().await;
    }

    fn queued(&self, reason: &str) {
        println!("[openshop] Reload already in progress — queued one more restart for {reason}");
    }
}

/// Watches the project root and the source directories that exist; returns the
/// directories being watched so that new ones can be added later.
fn watch_existing(watcher: &mut RecommendedWatcher, cwd: &Path, watched: &mut Vec<PathBuf>) {
    for name in WATCHED_DIRECTORIES {
        let directory = cwd.join(name);
        if directory.is_dir()
            && !watched.contains(&directory)
            && watcher.watch(&directory, RecursiveMode::Recursive).is_ok()
        {
            watched.push(directory);
        }
    }
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
    process.start().await;
    let coordinator = RestartCoordinator::new(process.clone());

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
    watch_existing(&mut watcher, &cwd, &mut watched);
    println!("[openshop] Watching for changes (src, migrations, Cargo.toml, .env, shopify.app*.toml)");

    let shutdown = terminate_signal();
    tokio::pin!(shutdown);
    let mut pending: Option<String> = None;

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
                watch_existing(&mut watcher, &cwd, &mut watched);
                let created_directory = WATCHED_DIRECTORIES
                    .iter()
                    .find(|name| cwd.join(name) == path)
                    .map(|name| name.to_string());
                if let Some(source) = watched_source(&cwd, &path).or(created_directory) {
                    pending = Some(source);
                }
            }
            _ = debounce => {
                if let Some(source) = pending.take() {
                    // A view may have been added, or the last one removed.
                    if source.starts_with("src") || source.starts_with("Cargo.") {
                        process.serve(ui_dir(bundle.refresh().await));
                    }
                    let coordinator = coordinator.clone();
                    tokio::spawn(async move { coordinator.request_restart(source).await });
                }
            }
        }
    }

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
}
