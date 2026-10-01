//! The D7 comparison (docs/design/testing.md open question 4): the same minimal scenario, but
//! the daemon is a real subprocess discovered through the real `daemon.json` — every layer the
//! in-process `TestDaemon` cannot see (fork/exec, the published pid, a foreign process's UDS).
//!
//! The verdict lives in testing.md §1 and docs/worklog/testing.md: in-process-through-sockets
//! stays the default e2e shape; this form covers the lifecycle shell.

use std::time::Duration;

use hatchery_daemon::discover::StateDir;
use hatchery_protocol::DaemonClient;
use hatchery_protocol::EventStream;
use hatchery_protocol::method as m;
use hatchery_testkit::wire::MockWire;
use hatchery_tests::support;

#[tokio::test(flavor = "multi_thread")]
async fn a_real_subprocess_daemon_serves_the_same_scenario() {
    let wire = MockWire::replay_sse(support::SSE_REASONING_OK).await;
    let dir = tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    let data = dir.path().join("data");

    let mut guard = ChildGuard(
        std::process::Command::new(env!("CARGO_BIN_EXE_e2e_daemon"))
            .env("HATCHERY_E2E_STATE", &state)
            .env("HATCHERY_E2E_DATA", &data)
            .env("HATCHERY_E2E_BASE_URL", wire.url())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn the daemon binary"),
    );

    let state_dir = StateDir::at(&state);
    let info = {
        let mut found = None;
        for _ in 0..100 {
            if let Some(info) = state_dir.discover_alive() {
                found = Some(info);
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        found.expect("the subprocess published its daemon.json in time")
    };
    assert_ne!(
        info.pid,
        std::process::id(),
        "a foreign process, not an in-process runtime"
    );

    let client = DaemonClient::connect(std::path::Path::new(&info.uds_path))
        .await
        .expect("connect");
    client
        .hello(Some(info.boot_token.clone()))
        .await
        .expect("hello with the published boot token");

    let created: m::SessionNewResult = client
        .call(m::SESSION_NEW, &support::chat_params())
        .await
        .expect("session created");

    let mut events = EventStream::connect(std::path::Path::new(&info.uds_path))
        .await
        .expect("event socket");
    events
        .subscribe(
            m::SESSION_LOAD,
            serde_json::json!({"session_id": created.session.id}),
        )
        .await
        .expect("subscribed");

    let _turn: m::SessionPromptResult = client
        .call(
            m::SESSION_PROMPT,
            &support::prompt_params(created.session.id, "hi", created.session.generation),
        )
        .await
        .expect("prompt accepted");

    let turn = support::collect_until_terminal(&mut events).await;
    let types: Vec<String> = turn.iter().map(support::event_type).collect();
    assert!(
        types.contains(&"turn_finished".to_owned()),
        "the foreign daemon streamed the whole turn: {types:?}"
    );

    // The same byte-exact leg of invariant 2, now across a process boundary.
    let requests = wire.requests().await;
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value =
        serde_json::from_str(&requests[0].body).expect("a JSON request body");
    assert_eq!(
        body["messages"],
        serde_json::json!([{ "role": "user", "content": "hi" }]),
    );

    guard.0.kill().expect("kill the child");
    guard.0.wait().expect("reap the child");
}

/// Kills the child even when an assertion panics — a test daemon must never leak.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
