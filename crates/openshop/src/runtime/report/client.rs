//! The Sentry client of an application: its settings, and its start.

use std::sync::Arc;

use sentry::integrations::backtrace::{AttachStacktraceIntegration, ProcessStacktraceIntegration};
use sentry::integrations::contexts::ContextIntegration;
use sentry::integrations::debug_images::DebugImagesIntegration;
use sentry::{Client, ClientOptions, Hub, Integration};

use super::integration::Reports;
use super::transport::HttpTransportFactory;

/// The `sentry` setting of an application: where it reports its failures.
///
/// A DSN is enough, and the setting takes one as it is:
/// `sentry: std::env::var("SENTRY_DSN").ok()`. Without a DSN, nothing is
/// reported.
///
/// ```
/// use openshop::SentryConfig;
///
/// let sentry = SentryConfig::new(std::env::var("SENTRY_DSN").ok())
///     .environment("staging")
///     .release(env!("CARGO_PKG_VERSION"));
/// # let _ = sentry;
/// ```
#[derive(Clone)]
pub struct SentryConfig {
    options: ClientOptions,
    /// Why the DSN cannot be used, reported when the configuration is built.
    problem: Option<String>,
}

impl SentryConfig {
    /// The application gives the DSN, from wherever it keeps its settings.
    /// `None`, or an empty DSN, leaves Sentry off.
    pub fn new(dsn: impl Into<Option<String>>) -> Self {
        let mut config = Self {
            options: ClientOptions::default(),
            problem: None,
        };
        if let Some(dsn) = dsn.into().filter(|dsn| !dsn.trim().is_empty()) {
            match dsn.trim().parse() {
                Ok(dsn) => config.options.dsn = Some(dsn),
                Err(error) => config.problem = Some(format!("sentry.dsn is not a valid DSN: {error}")),
            }
        }
        config
    }

    /// The environment of the events. Without one it is `production` when
    /// `OPENSHOP_ENV` or `NODE_ENV` is, and `development` otherwise.
    pub fn environment(mut self, environment: impl Into<String>) -> Self {
        self.options.environment = Some(environment.into().into());
        self
    }

    /// The release of the events: a version, the hash of a commit.
    pub fn release(mut self, release: impl Into<String>) -> Self {
        self.options.release = Some(release.into().into());
        self
    }

    /// Sets any other option of the Sentry SDK, with its own methods: a
    /// sample rate, a `before_send` that drops or rewrites events…
    ///
    /// ```
    /// use openshop::SentryConfig;
    ///
    /// let sentry = SentryConfig::new(std::env::var("SENTRY_DSN").ok())
    ///     .configure(|options| options.sample_rate(0.5).max_breadcrumbs(50));
    /// # let _ = sentry;
    /// ```
    pub fn configure(mut self, configure: impl FnOnce(ClientOptions) -> ClientOptions) -> Self {
        self.options = configure(self.options);
        self
    }

    /// The settings when they name a project to report to, or what is wrong
    /// with them.
    pub(crate) fn resolve(self) -> Result<Option<Self>, String> {
        match self.problem {
            Some(problem) => Err(problem),
            None => Ok(self.options.dsn.is_some().then_some(self)),
        }
    }

    /// The options [`SentryConfig::init`] starts the client with: the ones of
    /// the settings, the environment, the transport and the integrations. For
    /// a process that binds the client itself, and for a test that keeps the
    /// events instead of sending them.
    pub fn client_options(&self, production: bool) -> ClientOptions {
        let mut options = self.options.clone();
        if options.environment.is_none() {
            options.environment = Some(if production { "production" } else { "development" }.into());
        }
        if options.transport.is_none() {
            options.transport = Some(Arc::new(HttpTransportFactory));
        }

        // The integrations of `sentry::init`, without reading an environment
        // variable as it does, and with the panic hook of the framework in
        // place of the one of the SDK. The ones the application adds come last.
        let added = std::mem::take(&mut options.integrations);
        if options.default_integrations {
            options.integrations = vec![
                Arc::new(AttachStacktraceIntegration) as Arc<dyn Integration>,
                Arc::new(DebugImagesIntegration::default()),
                Arc::new(ContextIntegration::default()),
                Arc::new(ProcessStacktraceIntegration),
            ];
            // They are in place: `sentry::init` must not add them again.
            options.default_integrations = false;
        }
        options.integrations.push(Arc::new(Reports));
        options.integrations.extend(added);
        options
    }

    /// Starts reporting, for the whole process: the failures of the
    /// framework, the panics, and what the application captures itself with
    /// [`openshop::sentry`](sentry). `production` names the environment when
    /// the settings do not.
    ///
    /// `APP.main()` and `APP.run()` call it for `dev`, `start` and `worker`. A
    /// process that starts the server or a worker itself calls it first, and
    /// keeps the guard for as long as it runs.
    pub fn init(&self, production: bool) -> SentryGuard {
        let client = Arc::new(Client::with_options(self.client_options(production)));
        // The hub every thread starts from, whichever thread this is.
        Hub::main().bind_client(Some(client.clone()));
        SentryGuard(client)
    }
}

impl From<Option<String>> for SentryConfig {
    fn from(dsn: Option<String>) -> Self {
        Self::new(dsn)
    }
}

impl From<String> for SentryConfig {
    fn from(dsn: String) -> Self {
        Self::new(dsn)
    }
}

impl From<&str> for SentryConfig {
    fn from(dsn: &str) -> Self {
        Self::new(dsn.to_string())
    }
}

/// Reporting to Sentry, for as long as it is kept. Dropping it sends what is
/// queued, waiting up to the shutdown timeout of the settings (two seconds),
/// then reports nothing more.
#[must_use = "dropping the guard stops reporting to Sentry"]
pub struct SentryGuard(Arc<Client>);

impl Drop for SentryGuard {
    fn drop(&mut self) {
        self.0.close(None);
        // The process has no client any more, unless another one was started.
        let process = Hub::main();
        if process.client().is_some_and(|client| Arc::ptr_eq(&client, &self.0)) {
            process.bind_client(None);
        }
    }
}
