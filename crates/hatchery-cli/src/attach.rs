//! Attach-or-spawn: reach the user's daemon, starting it if it is not there (ADR-0001).
//!
//! The spawn half implements the D1 decision (2026-09-30): no double fork, no sd_notify — the
//! CLI simply launches `hatchery daemon run` as a detached process (`process_group(0)`, stdio
//! appended to a log file) and polls the state directory until the daemon publishes
//! `daemon.json`. The boot token in that file is the handshake, and its mtime-plus-log the
//! user's trail.

use std::path::{Path, PathBuf};
use std::time::Duration;

use hatchery_daemon::discover::{DaemonInfo, StateDir};
use hatchery_protocol::ClientError;

/// How long attach-or-spawn waits for a freshly spawned daemon to publish and bind.
const SPAWN_READY_TIMEOUT: Duration = Duration::from_secs(15);

/// How often a spawned daemon's publication is polled.
const SPAWN_POLL_PERIOD: Duration = Duration::from_millis(100);

/// The file a detached child's stdout and stderr are appended to, under [`StateDir::logs_dir`].
///
/// The daemon's own logger writes `hatchery.log.<date>` in the same directory, but it never sees
/// a refused startup: `entry::run` *returns* that error, and only the foreground path prints it.
/// A detached child has no terminal either (D1), so without this file the fatal words exist
/// nowhere — and "its log is at …" sends the user to an empty one (ADR-0009 is fail-*loud*).
pub const STDIO_LOG: &str = "hatchery-stdio.log";

/// A detached child's captured stdout and stderr, and the file they land in.
pub(crate) struct ChildStdio {
    /// The file both streams append to; a refusal should name this, not its directory.
    pub(crate) path: PathBuf,
    /// The child's stdout.
    pub(crate) stdout: std::process::Stdio,
    /// The child's stderr.
    pub(crate) stderr: std::process::Stdio,
}

/// Opens [`STDIO_LOG`] under the state's log directory for a detached child's stdout and stderr.
///
/// Appended rather than replaced: a spawn must not destroy the words of a daemon that is still
/// running — or of the one that just failed and is about to be reported — and appending is the
/// only mode two CLIs racing to spawn can share.
///
/// `None` when the directory or the file will not open. The spawn still happens then, with its
/// stdio to the void and the log *directory* as the hint: a log that cannot be written is not a
/// reason to refuse to start a daemon.
pub(crate) fn child_stdio(state: &StateDir) -> Option<ChildStdio> {
    let dir = state.logs_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(STDIO_LOG);
    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    let stderr = stdout.try_clone().ok()?;
    Some(ChildStdio {
        path,
        stdout: stdout.into(),
        stderr: stderr.into(),
    })
}

/// The path a failure message should point at: the captured stdio when there is one, else the
/// directory the daemon's own log files live in.
pub(crate) fn log_hint_for(state: &StateDir, captured: Option<&ChildStdio>) -> PathBuf {
    captured.map_or_else(|| state.logs_dir(), |stdio| stdio.path.clone())
}

/// Why reaching the daemon failed. Every variant is the user's business.
#[derive(Debug, thiserror::Error)]
pub enum AttachError {
    /// The spawn happened but the daemon never announced itself.
    #[error(
        "the daemon did not become ready within {timeout_secs}s; its log is at {}",
        .log_hint.display()
    )]
    NotReady {
        /// Seconds waited; kept for the message.
        timeout_secs: u64,
        /// Where the daemon's own words would be.
        log_hint: std::path::PathBuf,
    },
    /// The handshake or a call failed.
    #[error(transparent)]
    Client(#[from] ClientError),
    /// The process could not be started.
    #[error("spawning the daemon failed: {0}")]
    Spawn(std::io::Error),
}

/// A working client plus the daemon that answered.
pub struct Attached {
    /// The client, handshaken.
    pub client: hatchery_protocol::DaemonClient,
    /// The daemon's published coordinates.
    pub info: DaemonInfo,
    /// True when this call spawned the daemon.
    pub spawned: bool,
}

/// Attaches to the daemon under `state`, spawning one when nobody is there.
///
/// `daemon_exe` overrides the binary the child runs (tests point it at the real
/// `hatchery` binary); absent, this process's own executable is used.
///
/// # Errors
///
/// [`AttachError`] when the daemon never became reachable.
pub async fn attach_or_spawn(
    state: StateDir,
    daemon_exe: Option<PathBuf>,
    workspace: Option<&Path>,
) -> Result<Attached, AttachError> {
    if let Some(info) = state.discover_alive() {
        match attach_info(&info).await {
            Ok(client) => {
                return Ok(Attached {
                    client,
                    info,
                    spawned: false,
                });
            }
            Err(error) => {
                // A published-but-unreachable daemon is stale by definition; fall through to
                // the spawn path rather than bounce the user.
                tracing::warn!("a published daemon did not answer ({error}); trying to spawn");
            }
        }
    }
    spawn_and_attach(state, daemon_exe, workspace).await
}

/// Attaches to a *known* daemon: connect, handshake, go. No liveness discovery — the caller
/// vouches for the info (production discovers first; tests build the info directly).
///
/// # Errors
///
/// [`AttachError::Client`] when the connect or the handshake fails.
pub async fn attach_to(info: &DaemonInfo) -> Result<Attached, AttachError> {
    let client = hatchery_protocol::DaemonClient::connect(&info.endpoint).await?;
    client.hello(Some(info.boot_token.clone())).await?;
    Ok(Attached {
        client,
        info: info.clone(),
        spawned: false,
    })
}

async fn attach_info(info: &DaemonInfo) -> Result<hatchery_protocol::DaemonClient, AttachError> {
    Ok(attach_to(info).await?.client)
}

async fn spawn_and_attach(
    state: StateDir,
    daemon_exe: Option<PathBuf>,
    workspace: Option<&Path>,
) -> Result<Attached, AttachError> {
    let exe = daemon_exe.unwrap_or(std::env::current_exe().map_err(AttachError::Spawn)?);
    let state_root = state
        .lock_path()
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let mut command = std::process::Command::new(&exe);
    command
        .arg("daemon")
        .arg("run")
        .arg("--state-dir")
        .arg(&state_root);
    if let Some(workspace) = workspace {
        command.current_dir(workspace);
    }
    // The child's own words go to a file, not the void: a detached daemon that refuses to start
    // has no other way to say why (see [`STDIO_LOG`]).
    let captured = child_stdio(&state);
    let log_hint = log_hint_for(&state, captured.as_ref());
    command.stdin(std::process::Stdio::null());
    if let Some(captured) = captured {
        command.stdout(captured.stdout).stderr(captured.stderr);
    } else {
        command
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
    }
    #[cfg(unix)]
    {
        // Detached: own process group, so closing the terminal's foreground group does not
        // SIGHUP the daemon (the process_group call performs setsid in the child).
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(AttachError::Spawn)?;
    tracing::info!(pid = child.id(), "spawned a daemon process");

    let deadline = tokio::time::Instant::now() + SPAWN_READY_TIMEOUT;
    loop {
        if let Some(info) = state.discover_alive() {
            match attach_info(&info).await {
                Ok(client) => {
                    return Ok(Attached {
                        client,
                        info,
                        spawned: true,
                    });
                }
                // The publication lands before the socket is bound, so a connect failure here is
                // "not ready yet", not "broken" — the same rule the pre-spawn path applies to a
                // stale publication. Anything else (a refused handshake) is real and surfaces.
                Err(AttachError::Client(ClientError::Connect { .. })) => {}
                Err(error) => return Err(error),
            }
        }
        // A child that already exited is a failed start: no point polling out the clock.
        if child.try_wait().map_err(AttachError::Spawn)?.is_some() {
            return Err(AttachError::NotReady {
                timeout_secs: SPAWN_READY_TIMEOUT.as_secs(),
                log_hint: log_hint.clone(),
            });
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AttachError::NotReady {
                timeout_secs: SPAWN_READY_TIMEOUT.as_secs(),
                log_hint: log_hint.clone(),
            });
        }
        tokio::time::sleep(SPAWN_POLL_PERIOD).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stdio_log_appends_and_the_hint_names_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = StateDir::at(dir.path().join("state"));
        let captured = child_stdio(&state).expect("the log opens");
        assert_eq!(
            captured.path,
            state.logs_dir().join(STDIO_LOG),
            "the capture lives beside the daemon's own log files"
        );
        assert_eq!(
            log_hint_for(&state, Some(&captured)),
            captured.path,
            "a refusal names the file the child's words went to"
        );
        assert_eq!(
            log_hint_for(&state, None),
            state.logs_dir(),
            "with no capture the hint falls back to the directory"
        );
        drop(captured);

        // Reopening appends: the words of the daemon that just failed are the evidence the next
        // troubleshooter needs, and two CLIs racing to spawn must not truncate each other.
        let log = state.logs_dir().join(STDIO_LOG);
        std::fs::write(&log, "the previous daemon died here\n").expect("write");
        drop(child_stdio(&state).expect("the log reopens"));
        let contents = std::fs::read_to_string(&log).expect("read");
        assert!(
            contents.contains("the previous daemon died here"),
            "{contents:?}"
        );
    }
}
