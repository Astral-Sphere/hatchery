//! The local PTY against real processes. Unix only: the Windows column of D10 is deferred, and
//! [`LocalPty`] refuses there rather than pretending.
//!
//! Two facts from the probe shape these tests:
//!
//! * `kill -0` cannot tell a zombie from a live orphan, so the process-group test reads
//!   `/proc/<pid>/stat` and accepts only "gone" or `Z` as proof the process stopped running.
//! * The terminal driver rewrites LF to CRLF, so the stream test asserts the normalised form and
//!   would fail immediately if the reader stopped normalising.

#![cfg(unix)]

use std::time::Duration;

use hatchery_capabilities::{LocalPty, TerminalBackend, TerminalHandle, TerminalSpec};
use tokio_util::sync::CancellationToken;

fn sh(script: &str) -> TerminalSpec {
    TerminalSpec {
        command: "sh".to_owned(),
        args: vec!["-c".to_owned(), script.to_owned()],
        ..TerminalSpec::default()
    }
}

async fn read_to_end(handle: &dyn TerminalHandle) -> String {
    let mut output = String::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(10), handle.next_chunk()).await {
            Ok(Some(chunk)) => output.push_str(&chunk),
            Ok(None) => return output,
            Err(_) => panic!("the stream did not end; collected so far: {output:?}"),
        }
    }
}

#[tokio::test]
async fn a_command_streams_its_output_and_reports_its_exit_code() {
    let workspace = tempfile::tempdir().expect("a workspace");
    let pty = LocalPty::new(workspace.path()).expect("a backend");
    let handle = pty
        .create(
            sh("printf 'one\\n'; sleep 0.05; printf 'two\\n'; exit 3"),
            CancellationToken::new(),
        )
        .await
        .expect("spawn");

    let output = read_to_end(handle.as_ref()).await;
    let outcome = handle.wait().await.expect("an outcome");
    assert_eq!(output, "one\ntwo\n", "CRLF was normalised back to LF");
    assert_eq!(outcome.output, "one\ntwo\n");
    assert_eq!(outcome.exit_code, Some(3));
    assert_eq!(outcome.dropped_bytes, 0);
}

#[tokio::test]
async fn lf_only_output_arrives_as_lf_even_though_the_terminal_saw_crlf() {
    let workspace = tempfile::tempdir().expect("a workspace");
    let pty = LocalPty::new(workspace.path()).expect("a backend");
    let handle = pty
        .create(sh("printf 'a\\nb\\n'"), CancellationToken::new())
        .await
        .expect("spawn");
    let outcome = handle.wait().await.expect("an outcome");
    assert_eq!(outcome.output, "a\nb\n");
    assert_eq!(outcome.exit_code, Some(0));
}

#[tokio::test]
async fn the_working_directory_is_the_workspace_not_the_users_home() {
    let workspace = tempfile::tempdir().expect("a workspace");
    std::fs::create_dir(workspace.path().join("sub")).expect("sub");
    let expected = std::fs::canonicalize(workspace.path().join("sub")).expect("canonical");
    let pty = LocalPty::new(workspace.path()).expect("a backend");
    let spec = TerminalSpec {
        cwd: Some("sub".to_owned()),
        ..sh("pwd")
    };
    let handle = pty
        .create(spec, CancellationToken::new())
        .await
        .expect("spawn");
    let outcome = handle.wait().await.expect("an outcome");
    assert_eq!(outcome.output.trim_end(), expected.to_string_lossy());
}

#[tokio::test]
async fn a_working_directory_that_escapes_the_workspace_is_refused() {
    let workspace = tempfile::tempdir().expect("a workspace");
    let pty = LocalPty::new(workspace.path()).expect("a backend");
    for cwd in ["..", "../etc", "/etc"] {
        let spec = TerminalSpec {
            cwd: Some(cwd.to_owned()),
            ..sh("true")
        };
        let error = pty
            .create(spec, CancellationToken::new())
            .await
            .err()
            .unwrap_or_else(|| panic!("{cwd:?} was accepted"));
        assert!(
            error.to_string().contains("workspace") || error.to_string().contains("relative"),
            "{error}"
        );
    }
}

#[tokio::test]
async fn kill_ends_a_command_that_would_otherwise_run_forever() {
    let workspace = tempfile::tempdir().expect("a workspace");
    let pty = LocalPty::new(workspace.path()).expect("a backend");
    let handle = pty
        .create(sh("sleep 60"), CancellationToken::new())
        .await
        .expect("spawn");

    tokio::time::sleep(Duration::from_millis(100)).await;
    handle.kill();
    let outcome = tokio::time::timeout(Duration::from_secs(5), handle.wait())
        .await
        .expect("wait returned promptly")
        .expect("an outcome");
    assert_eq!(outcome.exit_code, None, "killed by a signal, not exited");
}

#[tokio::test]
async fn release_is_kill_on_a_pty() {
    let workspace = tempfile::tempdir().expect("a workspace");
    let pty = LocalPty::new(workspace.path()).expect("a backend");
    let handle = pty
        .create(sh("sleep 60"), CancellationToken::new())
        .await
        .expect("spawn");
    handle.release();
    let outcome = tokio::time::timeout(Duration::from_secs(5), handle.wait())
        .await
        .expect("wait returned promptly")
        .expect("an outcome");
    assert_eq!(outcome.exit_code, None);
}

#[tokio::test]
async fn cancellation_ends_the_process() {
    let workspace = tempfile::tempdir().expect("a workspace");
    let pty = LocalPty::new(workspace.path()).expect("a backend");
    let cancel = CancellationToken::new();
    let handle = pty
        .create(sh("sleep 60"), cancel.clone())
        .await
        .expect("spawn");
    cancel.cancel();
    let outcome = tokio::time::timeout(Duration::from_secs(5), handle.wait())
        .await
        .expect("cancellation reached the process")
        .expect("an outcome");
    assert_eq!(outcome.exit_code, None);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn dropping_the_handle_ends_the_process_too() {
    let workspace = tempfile::tempdir().expect("a workspace");
    let pid_file = workspace.path().join("pid");
    let pty = LocalPty::new(workspace.path()).expect("a backend");
    let spec = sh(&format!("echo $$ > {}; sleep 60", pid_file.display()));
    let handle = pty
        .create(spec, CancellationToken::new())
        .await
        .expect("spawn");
    let pid = read_pid(&pid_file).await;
    drop(handle);
    wait_not_running(pid).await;
}

/// The probe found `Child::kill()` sends SIGHUP and then SIGKILL to **one pid**. A same-group
/// grandchild that ignores SIGHUP survives it (probe M2, "in-group but SIGHUP-immune"); the
/// wrapper signals the process group. This is the payload that distinguishes the two — the
/// ordinary `sleep 60 &` case dies from the foreground group's SIGHUP even under `Child::kill()`.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn kill_reaches_a_background_child_in_the_same_process_group() {
    let workspace = tempfile::tempdir().expect("a workspace");
    let pty = LocalPty::new(workspace.path()).expect("a backend");
    let handle = pty
        .create(
            sh("trap '' HUP; sleep 60 & echo $!; wait"),
            CancellationToken::new(),
        )
        .await
        .expect("spawn");

    let pid = read_first_pid(handle.as_ref()).await;
    handle.kill();
    let _ = tokio::time::timeout(Duration::from_secs(5), handle.wait()).await;
    wait_not_running(pid).await;
}

/// The leader exits immediately and leaves a SIGHUP-immune background child in its group. The
/// handle's drop must still be able to kill that group: the control thread keeps the zombie
/// unreaped (so the pgid is pinned) and drains the kill request before reaping.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn dropping_the_handle_kills_a_background_child_after_the_leader_exits() {
    let workspace = tempfile::tempdir().expect("a workspace");
    let pty = LocalPty::new(workspace.path()).expect("a backend");
    let handle = pty
        .create(
            sh("trap '' HUP; sleep 60 & echo $!; exit 0"),
            CancellationToken::new(),
        )
        .await
        .expect("spawn");

    let pid = read_first_pid(handle.as_ref()).await;
    drop(handle);
    wait_not_running(pid).await;
}

/// A background child holding the pty keeps the reader from seeing EOF. `wait` must return on
/// the child's exit plus a bounded grace, and must say the stream was incomplete.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn wait_returns_when_a_grandchild_holds_the_pty() {
    let workspace = tempfile::tempdir().expect("a workspace");
    let pty = LocalPty::new(workspace.path()).expect("a backend");
    let handle = pty
        .create(
            sh("trap '' HUP; echo job; sleep 60 &"),
            CancellationToken::new(),
        )
        .await
        .expect("spawn");

    let outcome = tokio::time::timeout(Duration::from_secs(3), handle.wait())
        .await
        .expect("wait returned instead of waiting for EOF")
        .expect("an outcome");
    assert!(outcome.output.contains("job"), "{outcome:?}");
    assert!(outcome.stream_incomplete, "the reader never saw EOF");
    drop(handle);
}

#[cfg(target_os = "linux")]
async fn read_first_pid(handle: &dyn TerminalHandle) -> u32 {
    let mut seen = String::new();
    loop {
        let chunk = tokio::time::timeout(Duration::from_secs(5), handle.next_chunk())
            .await
            .expect("the pid line arrives")
            .expect("the stream is open");
        seen.push_str(&chunk);
        if let Some(line) = seen.lines().find(|line| !line.trim().is_empty()) {
            return line
                .trim()
                .parse::<u32>()
                .unwrap_or_else(|_| panic!("not a pid: {line:?}"));
        }
    }
}

#[cfg(target_os = "linux")]
async fn read_pid(path: &std::path::Path) -> u32 {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(text) = std::fs::read_to_string(path)
            && let Ok(pid) = text.trim().parse::<u32>()
        {
            return pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the test process never wrote its pid"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Absent or a zombie counts as stopped; anything else is a live orphan and fails.
#[cfg(target_os = "linux")]
async fn wait_not_running(pid: u32) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let state = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| {
                let after_name = stat.rfind(')')? + 2;
                stat.get(after_name..)?.chars().next()
            });
        match state {
            None => return,
            Some('Z') => return,
            Some(_) => {}
        }
        assert!(
            std::time::Instant::now() < deadline,
            "pid {pid} is still running after the group kill"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
