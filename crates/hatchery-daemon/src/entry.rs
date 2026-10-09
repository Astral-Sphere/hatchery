//! The production entry: from [`RunOptions`] to a serving daemon, and back down.
//!
//! This is the `hatchery daemon` process body (D1: a plain foreground process; the CLI spawns
//! it detached). The startup audit refuses a half-wired daemon (ADR-0009), SIGINT/SIGTERM
//! cancel the shutdown token, and teardown runs in strict reverse registration order. Tests
//! go through the same path via [`run_until`], cancelling the token instead of signalling.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use hatchery_protocol::method::ConfigOrigin;
use hatchery_protocol::{PROTOCOL_VERSION, SessionId};
use hatchery_store::SessionStore;

use crate::config::{LayeredConfig, LoadPaths};
use crate::core::{DaemonCore, Disposers};
use crate::discover::{DaemonInfo, DiscoverError, StateDir, unix_now};
use crate::hub::LiveHub;
use crate::manager::SessionManager;

/// How often the idle sweep looks for unwatched, unloaded runtimes.
const SWEEP_PERIOD: std::time::Duration = std::time::Duration::from_secs(60);

/// Options for the daemon process.
#[derive(Debug, Default)]
pub struct RunOptions {
    /// State directory override; absent, the standard XDG location.
    pub state_dir: Option<PathBuf>,
    /// Data directory override; absent, `$XDG_DATA_HOME/hatchery`.
    pub data_dir: Option<PathBuf>,
    /// Workspace binding (enables the project config layer); absent, none.
    pub workspace: Option<PathBuf>,
    /// Additionally serve stdin/stdout beside the UDS.
    pub serve_stdio: bool,
    /// Configuration injected whole (tests, embedding); absent, load the standard files.
    pub config_layers: Option<Vec<(ConfigOrigin, toml::Table)>>,
}

/// Why startup gave up. Every variant is already loud on its own; [`EntryError::Audit`]
/// carries the complete list, because fail-loud means naming everything at once.
#[derive(Debug, thiserror::Error)]
pub enum EntryError {
    /// Configuration failed to load.
    #[error("configuration: {0}")]
    Config(#[from] crate::config::ConfigError),
    /// The audit found missing pieces.
    #[error(
        "startup audit failed:\n{}",
        .0.iter().map(|m| format!("  - {m}")).collect::<Vec<_>>().join("\n")
    )]
    Audit(Vec<String>),
    /// Discovery failed (mostly: another daemon already holds the instance lock).
    #[error("{0}")]
    Discover(#[from] DiscoverError),
    /// The store would not open.
    #[error("store: {0}")]
    Store(#[from] hatchery_store::StoreError),
    /// An OS-level failure (socket bind, state directory).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Crash recovery or another manager-level failure.
    #[error("{0}")]
    Manager(#[from] crate::manager::ManagerError),
}

/// Runs the daemon until a signal arrives.
///
/// # Errors
///
/// [`EntryError`] on any refused startup; a refused startup serves nothing.
pub async fn run(options: RunOptions) -> Result<(), EntryError> {
    run_until(options, CancellationToken::new()).await
}

/// Runs the daemon until the external token *or* a signal cancels it. The test seam: tests
/// hold the token, production passes a fresh one only the signal watcher holds.
///
/// # Errors
///
/// See [`EntryError`].
pub async fn run_until(options: RunOptions, external: CancellationToken) -> Result<(), EntryError> {
    hatchery_llm::install_tls_provider();
    let state = options
        .state_dir
        .map_or_else(StateDir::standard, StateDir::at);
    // Logging first: everything after it may need to explain itself to a file, because a
    // detached daemon has no terminal to complain to.
    let _logging = crate::logging::init(&state.logs_dir())?;

    let config = match options.config_layers {
        Some(layers) => LayeredConfig::from_layers(layers),
        None => LayeredConfig::load(&LoadPaths::detect(options.workspace.as_deref()))?,
    };
    let data_dir = options.data_dir.unwrap_or_else(default_data_dir);
    let missing = crate::core::audit(&config, &state, &data_dir);
    if !missing.is_empty() {
        return Err(EntryError::Audit(missing));
    }

    let instance = state.acquire_instance()?;
    let store: Arc<dyn SessionStore> =
        Arc::new(hatchery_store::TursoStore::open(data_dir.join("sessions.db")).await?);
    let config = Arc::new(config);
    let hub = Arc::new(LiveHub::new());
    let manager = Arc::new(SessionManager::new(
        Arc::clone(&store),
        Arc::clone(&config),
        Arc::clone(&hub),
        data_dir,
        crate::prompt::default_prompts_dir(),
    ));
    let recovered = manager.recover_crashed_sessions().await?;
    if recovered > 0 {
        tracing::warn!("{recovered} interrupted turn(s) marked from a previous run");
    }
    // Reclaims the shadow repositories whose sessions are all gone (storage.md open question 3).
    // Deleting a session cascades its `checkpoints` rows away, and nothing else ever goes back to
    // look at the directory those rows pointed into, so this is the only place an orphan can be
    // noticed. Startup rather than a timer: it is one directory scan plus one query per entry, and
    // the budget ladder runs it again whenever a workspace is over its limit.
    let reclaimed = manager.checkpoints().sweep_orphans().await;
    if reclaimed > 0 {
        tracing::info!("{reclaimed} orphan shadow repositories reclaimed");
    }

    let boot_token = format!("boot-{}", SessionId::new());
    let core = Arc::new(DaemonCore::new(
        Arc::clone(&manager),
        Arc::clone(&config),
        Arc::clone(&store),
        boot_token.clone(),
    ));

    let socket_path = state.socket_path();
    state.publish(&DaemonInfo {
        pid: std::process::id(),
        endpoint: socket_path.display().to_string(),
        protocol_version: PROTOCOL_VERSION.to_owned(),
        boot_token,
        started_at: unix_now(),
    })?;

    // Teardown, registered in the order the pieces came up; `run_reverse` undoes them.
    let mut disposers = Disposers::new();
    {
        let info_path = state.info_path();
        disposers.push("published daemon.json", move || {
            if let Err(error) = std::fs::remove_file(&info_path) {
                tracing::warn!("daemon.json could not be removed: {error}");
            }
        });
    }
    {
        let socket_path = socket_path.clone();
        disposers.push("the daemon's socket", move || {
            if let Err(error) = hatchery_protocol::transport::discard(&socket_path) {
                tracing::warn!("the socket could not be removed: {error}");
            }
        });
    }

    let shutdown = CancellationToken::new();
    {
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            wait_for_signal().await;
            tracing::info!("a termination signal arrived; shutting down");
            shutdown.cancel();
        });
    }
    {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(SWEEP_PERIOD);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                manager.sweep_idle().await;
            }
        });
    }
    if options.serve_stdio {
        let core = Arc::clone(&core);
        let manager = Arc::clone(&manager);
        let hub = Arc::clone(&hub);
        tokio::spawn(crate::server::serve_stdio(core, manager, hub));
    }

    tracing::info!("the daemon is up");
    let listener_outcome = tokio::select! {
        _ = external.cancelled() => Ok(()),
        _ = shutdown.cancelled() => Ok(()),
        result = crate::server::serve_local(Arc::clone(&core), Arc::clone(&manager), Arc::clone(&hub), socket_path.clone()) => {
            // `serve_local` only returns when the listener dies; that is a teardown-worthy event
            // either way — and `daemon.json` is already published at this point, so a bind
            // failure must fall through to the teardown below, not return past it and leave a
            // publication pointing at a socket that never served.
            match result {
                Ok(()) => {
                    tracing::warn!("the listener ended without an error");
                    Ok(())
                }
                Err(error) => Err(error),
            }
        },
    };

    let order = disposers.run_reverse();
    tracing::info!("teardown ran {order:?}; releasing the instance lock");
    drop(instance);
    listener_outcome.map_err(EntryError::Io)
}

/// Blocks until SIGINT or SIGTERM (unix) or Ctrl-C (elsewhere).
async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).expect("SIGTERM handler");
        let mut interrupt = signal(SignalKind::interrupt()).expect("SIGINT handler");
        tokio::select! {
            _ = terminate.recv() => {},
            _ = interrupt.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// `$XDG_DATA_HOME/hatchery`, defaulting to `~/.local/share/hatchery`. Public because the
/// CLI's doctor reports on the same directory the daemon would create.
#[must_use]
pub fn default_data_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("hatchery")
}
