//! `hatchery daemon {start|run|status|stop}`.
//!
//! `start` is the D1 spawn half: launch `daemon run` detached (`process_group(0)`, stdio
//! appended to a log file so a refused startup stays readable), poll for `daemon.json`, report.
//! `stop` sends SIGTERM through the system `kill` binary — the workspace denies unsafe, and a
//! one-line subprocess beats a syscall binding for one signal (recorded in the worklog); the
//! daemon's own handler does the graceful part.

use std::time::Duration;

use hatchery_daemon::discover::StateDir;

use crate::args::DaemonAction;

const READY_TIMEOUT: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(100);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs the action; returns the process exit code.
pub async fn run(action: &DaemonAction) -> i32 {
    let (state_dir, action) = match action {
        DaemonAction::Start { state_dir }
        | DaemonAction::Run { state_dir }
        | DaemonAction::Status { state_dir }
        | DaemonAction::Stop { state_dir } => (state_dir.clone(), action),
    };
    let state = state_dir.map_or_else(StateDir::standard, hatchery_daemon::discover::StateDir::at);
    match action {
        DaemonAction::Start { .. } => start(&state).await,
        DaemonAction::Run { .. } => run_foreground(state).await,
        DaemonAction::Status { .. } => status(&state),
        DaemonAction::Stop { .. } => stop(&state).await,
    }
}

async fn start(state: &StateDir) -> i32 {
    if let Some(info) = state.discover_alive() {
        println!("the daemon is already running (pid {})", info.pid);
        return 0;
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            eprintln!("hatchery: cannot locate this binary: {error}");
            return 1;
        }
    };
    let state_root = state.lock_path().parent().map_or_else(
        || std::path::PathBuf::from("."),
        std::path::Path::to_path_buf,
    );
    // The detached child's own words go to a file rather than the void, so that "see …" names
    // something that can actually hold the reason it died (`attach::STDIO_LOG`).
    let captured = crate::attach::child_stdio(state);
    let log_hint = crate::attach::log_hint_for(state, captured.as_ref());
    let mut command = std::process::Command::new(&exe);
    command
        .arg("daemon")
        .arg("run")
        .arg("--state-dir")
        .arg(&state_root)
        .stdin(std::process::Stdio::null());
    if let Some(captured) = captured {
        command.stdout(captured.stdout).stderr(captured.stderr);
    } else {
        command
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group: the daemon survives the closing terminal (D1).
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            eprintln!("hatchery: spawning the daemon failed: {error}");
            return 1;
        }
    };
    let pid = child.id();
    let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
    loop {
        if let Some(info) = state.discover_alive() {
            println!(
                "the daemon is up: pid {}, socket {}, log {}",
                info.pid,
                info.endpoint,
                state.logs_dir().display()
            );
            return 0;
        }
        if child.try_wait().is_ok_and(|ended| ended.is_some()) {
            eprintln!(
                "hatchery: the daemon process exited before serving; see {}",
                log_hint.display()
            );
            return 1;
        }
        if tokio::time::Instant::now() >= deadline {
            eprintln!(
                "hatchery: the daemon (pid {pid}) did not publish within {READY_TIMEOUT:?}; see {}",
                log_hint.display()
            );
            return 1;
        }
        tokio::time::sleep(POLL).await;
    }
}

async fn run_foreground(state: StateDir) -> i32 {
    let options = hatchery_daemon::entry::RunOptions {
        state_dir: state.lock_path().parent().map(std::path::Path::to_path_buf),
        data_dir: None,
        workspace: std::env::current_dir().ok(),
        serve_stdio: false,
        config_layers: None,
    };
    match hatchery_daemon::entry::run(options).await {
        Ok(()) => {
            println!("the daemon stopped cleanly");
            0
        }
        Err(error) => {
            eprintln!("hatchery: {error}");
            1
        }
    }
}

fn status(state: &StateDir) -> i32 {
    let Some(info) = state.discover() else {
        println!("the daemon is not running");
        return 1;
    };
    if state.discover_alive().is_none() {
        println!(
            "a stale publication is present (pid {} is gone); `daemon start` will replace it",
            info.pid
        );
        return 1;
    }
    println!(
        "the daemon is running: pid {}, socket {}, protocol {}, since unix {}",
        info.pid, info.endpoint, info.protocol_version, info.started_at
    );
    0
}

async fn stop(state: &StateDir) -> i32 {
    let Some(info) = state.discover_alive() else {
        println!("the daemon is not running");
        return 1;
    };
    let pid = info.pid.to_string();
    let killed = std::process::Command::new("kill").arg(&pid).status();
    match killed {
        Ok(status) if status.success() => {}
        Ok(status) => {
            eprintln!("hatchery: kill {pid} exited with {status}");
            return 1;
        }
        Err(error) => {
            eprintln!(
                "hatchery: could not run `kill` ({error}); send SIGTERM to pid {pid} yourself"
            );
            return 1;
        }
    }
    let deadline = tokio::time::Instant::now() + STOP_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if state.discover_alive().is_none() {
            println!("the daemon (pid {pid}) stopped");
            return 0;
        }
        tokio::time::sleep(POLL).await;
    }
    eprintln!("hatchery: pid {pid} did not stop within {STOP_TIMEOUT:?}");
    1
}
