//! File logging: daily rotation in the state directory, retention pruning at startup.
//!
//! The daemon's face to its user is the frontend; its own voice goes to files, because a
//! detached daemon has no terminal to complain to (D1). `RUST_LOG` filters; absent, `info`.

use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// Log files are `hatchery.log.YYYY-MM-DD`; the prefix must match what
/// `tracing_appender::rolling::daily` produces.
const LOG_PREFIX: &str = "hatchery.log.";

/// Retention: files older than this many days are pruned at startup.
const RETENTION_DAYS: i64 = 14;

/// Keeps the non-blocking writer's worker alive; dropping it flushes and joins.
pub struct LoggingGuard {
    _worker: WorkerGuard,
}

/// Installs a file logger under `dir` and returns the guard that must live as long as the
/// process logs.
///
/// The subscriber is set only once: a second call (tests, an embedding host that already
/// installed one) leaves whatever is installed in place and still returns a guard, because
/// `set_global_default` does not need the worker kept by us to stay alive.
///
/// # Errors
///
/// [`std::io::Error`] when the log directory cannot be created.
pub fn init(dir: &Path) -> std::io::Result<LoggingGuard> {
    std::fs::create_dir_all(dir)?;
    prune_retired(dir);
    let appender = tracing_appender::rolling::daily(dir, LOG_PREFIX.trim_end_matches('.'));
    let (writer, worker) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_ansi(false).with_writer(writer))
        .try_init();
    Ok(LoggingGuard { _worker: worker })
}

/// Deletes rotated files strictly older than the retention window.
///
/// `YYYY-MM-DD` sorts lexicographically, so a string comparison is a chronological one. Files
/// whose suffix is not a date (or names that do not match the prefix) are not ours to judge —
/// left alone.
fn prune_retired(dir: &Path) {
    let cutoff = crate::clock::date_days_before(RETENTION_DAYS);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(suffix) = name.to_str().and_then(|n| n.strip_prefix(LOG_PREFIX)) else {
            continue;
        };
        // Stale needs the exact shape: `2026-9-3` would sort after `2026-10-01`, so a loose
        // string compare lies. Anything not matching is not ours to judge.
        let dated = suffix.len() == 10
            && suffix
                .chars()
                .enumerate()
                .all(|(i, c)| c.is_ascii_digit() || (i == 4 || i == 7) && c == '-');
        let stale = dated && suffix < cutoff.as_str();
        if !stale {
            continue;
        }
        if std::fs::remove_file(entry.path()).is_ok() {
            tracing::debug!(file = %name.to_string_lossy(), "pruned an expired log file");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_dated_files_past_the_window_are_pruned() {
        let dir = tempfile::tempdir().expect("tempdir");
        let names = [
            ("hatchery.log.2020-01-01", true),  // ancient: pruned
            ("hatchery.log.2099-01-01", false), // future (clock skew): kept
            ("hatchery.log.notadate", false),   // not ours to judge
            ("other.log.2020-01-01", false),    // not the log prefix
            ("hatchery.log.", false),           // no suffix at all
        ];
        for (name, _) in &names {
            std::fs::write(dir.path().join(name), b"").expect("write");
        }
        prune_retired(dir.path());
        for (name, gone) in &names {
            let exists = dir.path().join(name).exists();
            assert_eq!(!exists, *gone, "{name} pruned={gone}");
        }
    }

    #[test]
    fn init_creates_the_directory_and_installs_silently() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("logs");
        let _guard = init(&path).expect("init");
        assert!(path.is_dir());
    }
}
