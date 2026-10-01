//! Single-instance discovery: one daemon per user, found through the state directory
//! (docs/design/daemon.md §2).
//!
//! Three small files under `~/.local/state/hatchery/daemon/`:
//!
//! * `daemon.lock` — an advisory file lock (`fs2`). Whoever holds it is the active daemon; a
//!   second daemon's grab of the lock is the whole startup race, resolved by the OS.
//! * `daemon.json` — `{pid, uds_path, protocol_version, boot_token, started_at}`. Clients read
//!   it, connect to the UDS, and present the boot token on `daemon/hello`.
//! * `hatchery.sock` — the UDS itself, permissions 0700.
//!
//! Two decisions this pins (both daemon.md open questions):
//!
//! * **D1, daemonizing: the CLI spawns a detached foreground process.** `hatchery daemon start`
//!   launches `hatchery daemon run` detached (setsid on unix, DETACHED_PROCESS on Windows) and
//!   polls for the socket; systemd users run `hatchery daemon run` as a normal service. No
//!   double-fork, no `sd_notify` dependency — the handshake lives where the code is.
//! * **D4, boot-token rotation: a fresh token on every daemon start.** `daemon.json` is written
//!   0600 by the new daemon after it holds the lock, so a stale file is replaced atomically;
//!   clients always read it fresh at attach. A file readable only by the user presenting the
//!   token it named is the whole defence.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Why discovery or acquisition failed.
#[derive(Debug, thiserror::Error)]
pub enum DiscoverError {
    /// The state directory could not be created.
    #[error("state directory {path}: {source}")]
    StateDir {
        /// Where.
        path: PathBuf,
        /// The OS's complaint.
        source: std::io::Error,
    },
    /// Another daemon holds the instance lock.
    #[error("another daemon is already running (holds {path})")]
    AlreadyRunning {
        /// The lock file it holds.
        path: PathBuf,
    },
    /// The lock file could not be opened at all.
    #[error("instance lock {path}: {source}")]
    LockIo {
        /// Where.
        path: PathBuf,
        /// The OS's complaint.
        source: std::io::Error,
    },
}

/// What a client needs to reach the daemon.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DaemonInfo {
    /// The daemon's process id, for `daemon status` and stale-file checks.
    pub pid: u32,
    /// The UDS path, absolute.
    pub uds_path: String,
    /// The protocol version this daemon speaks.
    pub protocol_version: String,
    /// The boot token: presented on `daemon/hello`, regenerated every start (D4).
    pub boot_token: String,
    /// Unix seconds at startup.
    pub started_at: u64,
}

/// The held instance lock; dropping it releases the file lock.
///
/// The file is deliberately never deleted: an empty lock file is the normal resting state, and
/// unlinking it would create a window where a new daemon and an old file disagree.
pub struct InstanceGuard {
    _file: std::fs::File,
    lock_path: PathBuf,
}

impl InstanceGuard {
    /// The lock file's path, for the audit log.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.lock_path
    }
}

/// The state directory and its file names.
#[derive(Clone, Debug)]
pub struct StateDir {
    root: PathBuf,
}

impl StateDir {
    /// The standard location: `$XDG_STATE_HOME/hatchery/daemon`, defaulting to
    /// `~/.local/state/hatchery/daemon`.
    #[must_use]
    pub fn standard() -> Self {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| Path::new(&home).join(".local").join("state"))
            })
            .unwrap_or_else(|| PathBuf::from("."));
        Self::at(base.join("hatchery").join("daemon"))
    }

    /// A location for tests.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Where the daily log files live, under the state root.
    #[must_use]
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// `daemon.lock`.
    #[must_use]
    pub fn lock_path(&self) -> PathBuf {
        self.root.join("daemon.lock")
    }

    /// `daemon.json`.
    #[must_use]
    pub fn info_path(&self) -> PathBuf {
        self.root.join("daemon.json")
    }

    /// `hatchery.sock`.
    #[must_use]
    pub fn socket_path(&self) -> PathBuf {
        self.root.join("hatchery.sock")
    }

    /// Creates the directory (and parents) with user-only permissions.
    ///
    /// # Errors
    ///
    /// [`DiscoverError::StateDir`] when the OS refuses.
    pub fn ensure(&self) -> Result<(), DiscoverError> {
        std::fs::create_dir_all(&self.root).map_err(|source| DiscoverError::StateDir {
            path: self.root.clone(),
            source,
        })?;
        restrict_to_owner(&self.root);
        Ok(())
    }

    /// Grabs the instance lock, failing fast when a daemon already holds it.
    ///
    /// # Errors
    ///
    /// [`DiscoverError::AlreadyRunning`] when the lock is taken.
    pub fn acquire_instance(&self) -> Result<InstanceGuard, DiscoverError> {
        use fs2::FileExt;
        self.ensure()?;
        let path = self.lock_path();
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|source| DiscoverError::LockIo {
                path: path.clone(),
                source,
            })?;
        file.try_lock_exclusive()
            .map_err(|_| DiscoverError::AlreadyRunning { path: path.clone() })?;
        // The pid line is advisory metadata for `status`; the lock is the truth.
        let mut file = file;
        let _ = file.write_all(format!("pid={}\n", std::process::id()).as_bytes());
        let _ = file.flush();
        Ok(InstanceGuard {
            _file: file,
            lock_path: path,
        })
    }

    /// Publishes the daemon's coordinates (0600), replacing any stale file.
    ///
    /// # Errors
    ///
    /// [`DiscoverError::StateDir`] when the write fails — a daemon whose address nobody can
    /// read must not pretend to serve.
    pub fn publish(&self, info: &DaemonInfo) -> Result<(), DiscoverError> {
        self.ensure()?;
        let body = serde_json::to_string_pretty(info).unwrap_or_else(|_| "{}".to_owned());
        let path = self.info_path();
        // Write-then-rename keeps a reader from seeing a torn file.
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, body).map_err(|source| DiscoverError::StateDir {
            path: temp.clone(),
            source,
        })?;
        restrict_to_owner(&temp);
        std::fs::rename(&temp, &path).map_err(|source| DiscoverError::StateDir {
            path: path.clone(),
            source,
        })?;
        restrict_file(&path);
        Ok(())
    }

    /// The published info, when a daemon has published one.
    #[must_use]
    pub fn discover(&self) -> Option<DaemonInfo> {
        let text = std::fs::read_to_string(self.info_path()).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// The published info, if the daemon it names is still alive.
    ///
    /// The pid probe is the cheap half of a health check; the full check is connecting and
    /// saying hello.
    #[must_use]
    pub fn discover_alive(&self) -> Option<DaemonInfo> {
        let info = self.discover()?;
        if !pid_is_alive(info.pid) {
            return None;
        }
        Some(info)
    }

    /// Removes the published info (daemon shutdown); the lock file stays.
    ///
    /// # Errors
    ///
    /// Ignored by callers that log; surfaced for the graceful path to report.
    pub fn clear(&self) -> std::io::Result<()> {
        match std::fs::remove_file(self.info_path()) {
            Ok(()) => Ok(()),
            // Already gone is the goal reached.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

fn pid_is_alive(pid: u32) -> bool {
    // A zero-pid or self-claiming file is a stale artefact, not a live daemon we should race.
    if pid == 0 || pid == std::process::id() {
        return false;
    }
    // Best-effort, no unsafe: on Linux `/proc` answers directly; elsewhere (macOS, Windows)
    // there is no portable cheap probe without a syscall binding, so the check conservatively
    // says alive and lets the hello handshake — the real check — reject a corpse. A stale file
    // costs one failed connect, not a wrong daemon.
    #[cfg(target_os = "linux")]
    {
        Path::new("/proc").join(pid.to_string()).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

/// Best-effort owner-only permissions for a directory; failures are logged, not fatal.
pub(crate) fn restrict_to_owner(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)) {
            tracing::warn!("could not restrict {}: {error}", path.display());
        }
    }
    #[cfg(windows)]
    {
        let _ = path;
    }
}

/// Best-effort owner-read-only for a file carrying the boot token.
fn restrict_file(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
            tracing::warn!("could not restrict {}: {error}", path.display());
        }
    }
    #[cfg(windows)]
    {
        let _ = path;
    }
}

/// Unix seconds now, for `started_at`.
#[must_use]
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_directory_files_have_standard_names() {
        let dir = StateDir::at("/tmp/hatchery-test-state");
        assert!(dir.lock_path().ends_with("daemon.lock"));
        assert!(dir.info_path().ends_with("daemon.json"));
        assert!(dir.socket_path().ends_with("hatchery.sock"));
    }

    #[test]
    fn the_instance_lock_is_exclusive_and_released_on_drop() {
        let dir = StateDir::at(tempfile::tempdir().expect("tempdir").path());
        let first = dir.acquire_instance().expect("first grab");
        assert!(dir.acquire_instance().is_err(), "the second grab must fail");

        drop(first);
        let again = dir.acquire_instance().expect("released on drop");
        drop(again);
    }

    #[test]
    fn publish_and_discover_round_trip_and_clear() {
        let dir = StateDir::at(tempfile::tempdir().expect("tempdir").path());
        assert!(dir.discover().is_none());

        let info = DaemonInfo {
            pid: std::process::id(),
            uds_path: dir.socket_path().display().to_string(),
            protocol_version: hatchery_protocol::PROTOCOL_VERSION.to_owned(),
            boot_token: "token-1".to_owned(),
            started_at: unix_now(),
        };
        dir.publish(&info).expect("publish");
        let read = dir.discover().expect("published");
        assert_eq!(read.boot_token, "token-1");

        dir.clear().expect("clear");
        assert!(dir.discover().is_none());
        // Clearing twice is fine.
        dir.clear().expect("clear again");
    }

    #[test]
    fn a_dead_pid_or_our_own_pid_reads_as_stale() {
        let dir = StateDir::at(tempfile::tempdir().expect("tempdir").path());
        let mut info = DaemonInfo {
            pid: std::process::id(),
            uds_path: String::new(),
            protocol_version: hatchery_protocol::PROTOCOL_VERSION.to_owned(),
            boot_token: String::new(),
            started_at: 0,
        };
        dir.publish(&info).expect("publish");
        assert!(
            dir.discover_alive().is_none(),
            "our own pid is not a daemon"
        );

        info.pid = 0;
        dir.publish(&info).expect("publish");
        assert!(dir.discover_alive().is_none());
    }

    #[test]
    fn the_published_file_is_owner_only() {
        let dir = StateDir::at(tempfile::tempdir().expect("tempdir").path());
        let info = DaemonInfo {
            pid: 1,
            uds_path: String::new(),
            protocol_version: hatchery_protocol::PROTOCOL_VERSION.to_owned(),
            boot_token: "t".to_owned(),
            started_at: 0,
        };
        dir.publish(&info).expect("publish");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.info_path())
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "the boot token must not be world-readable"
            );
        }
    }
}
