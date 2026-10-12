//! `acd-inbox serve`: startup wiring (omarchy spec §4) and logging.
use std::sync::Arc;

use acd_client::{
    Timing,
    inbox::{Inbox, InboxOptions},
    settings::{EndpointConfig, SecretStore, Settings},
};
use anyhow::{Context, Result};
use tracing_subscriber::filter::{EnvFilter, LevelFilter, Targets};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::{SubscriberInitExt, TryInitError};

use crate::prefs::{self, Paths, Prefs};
use crate::server::{SecretControl, ServerOptions, Service};
use crate::startup::{self, LockFile, OwnedSocket};

/// A started service. Dropping it removes the socket (the server task keeps
/// the listener until it is aborted or the process exits).
pub struct Running {
    pub service: Arc<Service>,
    pub inbox: Arc<Inbox>,
    pub server: tokio::task::JoinHandle<()>,
    pub socket: OwnedSocket,
    pub lock: LockFile,
}

/// The keys of the machines whose secrets live in the keyring (remote ones).
pub fn remote_keys(settings: &Settings) -> Vec<String> {
    settings
        .machines
        .iter()
        .filter(|m| matches!(m.endpoint, EndpointConfig::Tcp { .. }))
        .map(|m| m.key.clone())
        .collect()
}

/// §4.4 startup, then settings, `prepare(remote keys)` on the blocking pool,
/// the inbox and the server. Call inside a tokio runtime.
pub async fn start_service(
    paths: &Paths,
    store: Arc<dyn SecretStore>,
    control: Arc<dyn SecretControl>,
    timing: Timing,
    local_name: &str,
    prepare: impl FnOnce(Vec<String>) + Send + 'static,
) -> Result<Running> {
    let startup::Listening { listener, socket, lock } = startup::listen(&paths.runtime_dir, &paths.socket, &paths.lock)?;
    let settings = prefs::init_settings(&paths.settings, local_name).context("loading settings.json")?;
    let keys = remote_keys(&settings);
    tokio::task::spawn_blocking(move || prepare(keys)).await?;
    let prefs = Prefs::load(&paths.service);
    let inbox = Inbox::new(
        settings,
        store,
        InboxOptions {
            timing,
            settings_path: Some(paths.settings.clone()),
            client_name: "acd-inbox".into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
        },
    );
    inbox.start();
    let service = Service::new(inbox.clone(), prefs, Some(paths.service.clone()), control, ServerOptions::default());
    let server = service.spawn(listener);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "acd-inbox serving");
    Ok(Running { service, inbox, server, socket, lock })
}

/// Dependencies whose logs can carry frames, tokens or agent text, capped
/// whatever `RUST_LOG` says (plan D33).
const DENY: &[(&str, LevelFilter)] = &[
    ("tungstenite", LevelFilter::OFF),
    ("tokio_tungstenite", LevelFilter::OFF),
    ("rustls", LevelFilter::WARN),
    ("tokio_rustls", LevelFilter::WARN),
    ("hyper", LevelFilter::WARN),
    ("hyper_util", LevelFilter::WARN),
    ("h2", LevelFilter::WARN),
    ("axum", LevelFilter::WARN),
    ("zbus", LevelFilter::WARN),
    ("zvariant", LevelFilter::WARN),
    ("secret_service", LevelFilter::WARN),
    ("mio", LevelFilter::WARN),
    ("tokio", LevelFilter::WARN),
];

/// The deny-list as a filter that passes everything else.
pub fn deny_list() -> Targets {
    DENY.iter().fold(Targets::new().with_default(LevelFilter::TRACE), |t, (target, level)| t.with_target(*target, *level))
}

/// The production logging: `env` (`RUST_LOG`, default `info`) AND the
/// deny-list, both as filter layers, so an event must pass both; plus the
/// `log` bridge, which `try_init` installs.
pub fn init_logging_to<W>(env: Option<&str>, writer: W) -> Result<(), TryInitError>
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    let env = env.and_then(|e| EnvFilter::try_new(e).ok()).unwrap_or_else(|| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(env)
        .with(deny_list())
        .with(tracing_subscriber::fmt::layer().with_ansi(false).with_target(false).with_writer(writer))
        .try_init()
}

/// Logs to stderr (journald for the unit).
pub fn init_logging() {
    let env = std::env::var("RUST_LOG").ok();
    if let Err(e) = init_logging_to(env.as_deref(), std::io::stderr) {
        eprintln!("acd-inbox: logging is already set up: {e}");
    }
}

/// `acd-inbox serve` (plan D25).
pub fn run() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        linux_run()
    }
    #[cfg(not(target_os = "linux"))]
    {
        anyhow::bail!("acd-inbox serve runs on Linux only")
    }
}

#[cfg(target_os = "linux")]
fn linux_run() -> Result<()> {
    use crate::linux::{dbus::DbusNotify, keyring::SecretServiceBackend};
    use crate::notify::{Notifier, Opener, ShellOpener};
    use crate::secrets::{SecretWorker, WorkerStore, WorkerTimeouts};

    init_logging();
    let paths = Paths::from_env()?;
    let local = prefs::local_name(&gethostname::gethostname().to_string_lossy());
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        let store = Arc::new(WorkerStore::new(SecretWorker::spawn(SecretServiceBackend::new(), WorkerTimeouts::LIVE)));
        let prefill = store.clone();
        let running = start_service(&paths, store.clone(), store, Timing::default(), &local, move |keys| {
            prefill.prefill(&keys)
        })
        .await?;
        match DbusNotify::connect().await {
            Ok((backend, actions)) => {
                let opener = ShellOpener::find(std::env::var_os("PATH").as_deref(), std::path::Path::new("/usr/bin"))
                    .map(|o| Arc::new(o) as Arc<dyn Opener>);
                if opener.is_none() {
                    tracing::info!("omarchy-shell not found; notifications have no Open action");
                }
                let notifier = Notifier::new(Arc::new(backend), opener);
                tokio::spawn(notifier.run(
                    running.inbox.view(),
                    running.service.prefs(),
                    running.service.panel_open(),
                    actions,
                ));
            }
            Err(e) => tracing::warn!(
                class = crate::linux::dbus::error_class(&e),
                "no notification server; notifications are off for this session"
            ),
        }
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            _ = term.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
        tracing::info!("acd-inbox stopping");
        // Aborting drops the server task's close-on-drop guard, which closes
        // every connection and cancels countdowns; waiting for the abort to
        // finish makes sure that happened before the process exits.
        running.server.abort();
        let _ = (&mut { running.server }).await;
        anyhow::Ok(())
    })
}
