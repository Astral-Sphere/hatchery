//! The production entry, exercised end to end: startup, serve, teardown — the same path
//! `hatchery daemon` runs, with the signal replaced by a cancellation token.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use hatchery_daemon::discover::{DaemonInfo, StateDir};
use hatchery_daemon::entry::{EntryError, RunOptions, run_until};
use hatchery_protocol::{DaemonClient, method as m};

type Runner = tokio::task::JoinHandle<Result<(), EntryError>>;

/// Layers that pass the audit hermetically: the builtin deepseek/qwen rows always
/// participate in it, so their env requirements are blanked, and the provider under test
/// needs no environment at all.
fn hermetic_layers() -> Vec<(m::ConfigOrigin, toml::Table)> {
    vec![(
        m::ConfigOrigin::User,
        toml::from_str(
            "[providers.deepseek]\nenv_key = \"\"\n\n[providers.qwen]\nenv_key = \"\"\n\n[providers.testprov]\nbase_url = \"http://127.0.0.1:9\"\nenv_key = \"\"\nmodels = [\"m\"]\n",
        )
        .expect("toml"),
    )]
}

/// The state dir both the daemon and the test agree on, under one tempdir root.
fn state_dir(root: &std::path::Path) -> StateDir {
    StateDir::at(root.join("state"))
}

fn options(state: &StateDir, data_root: &std::path::Path) -> RunOptions {
    RunOptions {
        state_dir: Some(
            state
                .lock_path()
                .parent()
                .expect("state root")
                .to_path_buf(),
        ),
        data_dir: Some(data_root.to_path_buf()),
        workspace: None,
        serve_stdio: false,
        config_layers: Some(hermetic_layers()),
    }
}

/// Waits for the published daemon.json. A start that never publishes has failed — but the
/// failure must not turn into a hang: joining a still-serving daemon would block forever, so
/// an unfinished runner after the grace period panics with that fact instead.
async fn wait_published(state: &StateDir, runner: &mut Runner) -> DaemonInfo {
    for _ in 0..200 {
        if let Some(info) = state.discover() {
            return info;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    if tokio::time::timeout(Duration::from_secs(2), &mut *runner)
        .await
        .is_err()
    {
        panic!("the daemon never published daemon.json and is still running");
    }
    let outcome = (&mut *runner).await;
    panic!("the daemon never published daemon.json; runner outcome: {outcome:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_daemon_serves_over_uds_and_teardown_clears_the_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state_dir(dir.path());
    let external = CancellationToken::new();
    let mut runner = {
        let external = external.clone();
        let options = options(&state, &dir.path().join("data"));
        tokio::spawn(async move { run_until(options, external).await })
    };

    let info = wait_published(&state, &mut runner).await;
    let client = DaemonClient::connect(&info.uds_path)
        .await
        .expect("connect");
    let hello = client
        .hello(Some(info.boot_token.clone()))
        .await
        .expect("hello");
    assert_eq!(hello.protocol_version, hatchery_protocol::PROTOCOL_VERSION);

    let created: m::SessionNewResult = client
        .call(
            m::SESSION_NEW,
            &m::SessionNewParams {
                mode: hatchery_protocol::SessionModeId::chat(),
                workspace: None,
                model: Some(hatchery_protocol::ModelRef::new("testprov", "m")),
                title: None,
                config_patch: None,
            },
        )
        .await
        .expect("session/new");
    let listed: m::SessionListResult = client
        .call(m::SESSION_LIST, &m::SessionListParams::default())
        .await
        .expect("session/list");
    assert_eq!(listed.sessions.len(), 1);
    assert_eq!(listed.sessions[0].id, created.session.id);

    external.cancel();
    let outcome = tokio::time::timeout(Duration::from_secs(10), runner)
        .await
        .expect("teardown in time")
        .expect("no panic");
    assert!(outcome.is_ok(), "{outcome:?}");
    assert!(state.discover().is_none(), "daemon.json should be cleared");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_start_is_refused_by_the_instance_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state_dir(dir.path());
    let external = CancellationToken::new();
    let mut runner = {
        let external = external.clone();
        let options = options(&state, &dir.path().join("data"));
        tokio::spawn(async move { run_until(options, external).await })
    };
    let _info = wait_published(&state, &mut runner).await;

    // Same state dir, same lock: the second instance must be refused, not queued.
    let second = run_until(
        options(&state, &dir.path().join("data2")),
        CancellationToken::new(),
    )
    .await
    .expect_err("the second daemon must be refused");
    assert!(
        second.to_string().to_lowercase().contains("already"),
        "the refusal should say the instance exists: {second}"
    );

    external.cancel();
    let outcome = tokio::time::timeout(Duration::from_secs(10), runner)
        .await
        .expect("teardown in time")
        .expect("no panic");
    assert!(outcome.is_ok(), "{outcome:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn startup_audit_names_the_missing_env_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state_dir(dir.path());
    let options = RunOptions {
        state_dir: Some(state.lock_path().parent().expect("state root").to_path_buf()),
        data_dir: Some(dir.path().join("data")),
        workspace: None,
        serve_stdio: false,
        // A configured provider whose env key cannot exist: the audit must name the variable.
        config_layers: Some(vec![(
            m::ConfigOrigin::User,
            toml::from_str(
                "[providers.deepseek]\nenv_key = \"\"\n\n[providers.qwen]\nenv_key = \"\"\n\n[providers.ghost]\nbase_url = \"http://127.0.0.1:9\"\nenv_key = \"HATCHERY_TEST_DEFINITELY_MISSING_KEY\"\nmodels = [\"m\"]\n",
            )
            .expect("toml"),
        )]),
    };
    let error = run_until(options, CancellationToken::new())
        .await
        .expect_err("a daemon missing its env key must refuse to serve");
    let message = error.to_string();
    assert!(
        message.contains("HATCHERY_TEST_DEFINITELY_MISSING_KEY"),
        "the audit should name the missing variable: {message}"
    );
}
