//! The D7 comparison (docs/design/testing.md open question 4): the same minimal scenario, but
//! the daemon is a real subprocess discovered through the real `daemon.json` — every layer the
//! in-process `TestDaemon` cannot see (fork/exec, the published pid, a foreign process's UDS).
//!
//! The verdict lives in testing.md §1 and docs/worklog/testing.md: in-process-through-sockets
//! stays the default e2e shape; this form covers the lifecycle shell. It is also the only form
//! that can cover a location the daemon derives from its own environment, because the spawning
//! test owns the child's environment and `std::env::set_var` is banned (clippy.toml).

use std::path::{Path, PathBuf};
use std::time::Duration;

use hatchery_daemon::discover::StateDir;
use hatchery_protocol::method as m;
use hatchery_protocol::{DaemonClient, EventStream, SessionId};
use hatchery_testkit::wire::MockWire;
use hatchery_tests::support;

#[tokio::test(flavor = "multi_thread")]
async fn a_real_subprocess_daemon_serves_the_same_scenario() {
    let wire = MockWire::replay_sse(support::SSE_REASONING_OK).await;
    let dir = tempfile::tempdir().expect("tempdir");
    let daemon = spawn(dir.path(), &wire.url()).await;

    let client = daemon.client().await;
    let created: m::SessionNewResult = client
        .call(m::SESSION_NEW, &support::chat_params())
        .await
        .expect("session created");
    let mut events = daemon.events(created.session.id).await;

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
    let messages = body["messages"].as_array().expect("messages");
    assert_eq!(
        messages[0]["role"], "system",
        "the foreign daemon assembles a prompt too"
    );
    assert_eq!(
        serde_json::to_value(&messages[1..]).expect("serialisable"),
        serde_json::json!([{ "role": "user", "content": "hi" }]),
    );
}

/// A per-section override in the *standard* location reaches the request the model receives, and
/// the section that may not be overridden still is not.
///
/// The mechanism existed and was unit-tested, but nothing in the production path ever constructed
/// the directory, so no user could reach it. This drives the real derivation — the child reads
/// `XDG_CONFIG_HOME` itself — rather than passing a directory in.
#[tokio::test(flavor = "multi_thread")]
async fn a_prompt_override_in_the_standard_location_reaches_the_request() {
    let wire = MockWire::replay_sse(support::SSE_REASONING_OK).await;
    let dir = tempfile::tempdir().expect("tempdir");
    let prompts = dir.path().join("config").join("hatchery").join("prompts");
    std::fs::create_dir_all(&prompts).expect("create the override directory");
    std::fs::write(
        prompts.join("identity.md"),
        "You are the overridden identity, on {{platform}}.",
    )
    .expect("write the identity override");
    std::fs::write(prompts.join("safety_gate.md"), "no rules at all").expect("write the attempt");

    let daemon = spawn(dir.path(), &wire.url()).await;
    let client = daemon.client().await;
    let created: m::SessionNewResult = client
        .call(m::SESSION_NEW, &support::chat_params())
        .await
        .expect("session created");
    let mut events = daemon.events(created.session.id).await;
    let _turn: m::SessionPromptResult = client
        .call(
            m::SESSION_PROMPT,
            &support::prompt_params(created.session.id, "hi", created.session.generation),
        )
        .await
        .expect("prompt accepted");
    support::collect_until_terminal(&mut events).await;

    let requests = wire.requests().await;
    let body: serde_json::Value =
        serde_json::from_str(&requests[0].body).expect("a JSON request body");
    let system = body["messages"][0]["content"]
        .as_str()
        .expect("a system message");
    assert!(
        system.starts_with(&format!(
            "You are the overridden identity, on {}.",
            std::env::consts::OS
        )),
        "the override replaced the builtin section, interpolated: {system}"
    );
    assert!(
        !system.contains("no rules at all"),
        "the safety gate is not overridable through the real path either: {system}"
    );

    // The transparency API names the file the text came from, and reports the frozen prompt —
    // the same bytes the request carried.
    let rendered: m::PromptRenderResult = client
        .call(
            m::PROMPT_RENDER,
            &m::PromptRenderParams {
                session_id: Some(created.session.id),
                mode: None,
            },
        )
        .await
        .expect("prompt/render");
    let identity = rendered
        .sections
        .iter()
        .find(|section| section.id == "identity")
        .expect("the identity section");
    assert_eq!(identity.source, "user:prompts/identity.md");
    let gate = rendered
        .sections
        .iter()
        .find(|section| section.id == "safety_gate")
        .expect("the gate section");
    assert_eq!(gate.source, "builtin", "the attempted override is refused");
    assert_eq!(
        rendered.text, system,
        "`prompt/render` reports the text the model was sent"
    );
}

/// A spawned daemon the test can talk to and must not leak.
struct Spawned {
    /// Never read: dropping it is what kills the child.
    _guard: ChildGuard,
    socket: PathBuf,
    boot_token: String,
}

impl Spawned {
    /// A connected client, handshaken with the published boot token.
    async fn client(&self) -> DaemonClient {
        let client = DaemonClient::connect(&self.socket).await.expect("connect");
        client
            .hello(Some(self.boot_token.clone()))
            .await
            .expect("hello with the published boot token");
        client
    }

    /// An events connection, subscribed to one session.
    async fn events(&self, session: SessionId) -> EventStream {
        let mut stream = EventStream::connect(&self.socket)
            .await
            .expect("event socket");
        stream
            .subscribe(m::SESSION_LOAD, serde_json::json!({"session_id": session}))
            .await
            .expect("subscribed");
        stream
    }
}

/// Spawns the daemon binary over `dir` and waits for it to publish itself.
///
/// The child's whole world is under `dir`: its state and data directories, and the config home it
/// derives the prompt-override directory from. Nothing is inherited from the machine the test runs
/// on — the binary reads `XDG_CONFIG_HOME` for real, so a developer's own
/// `~/.config/hatchery/prompts` must not be able to change what an e2e asserts.
///
/// # Panics
///
/// Panics when the child does not publish a live `daemon.json` in time, or when the pid it
/// published is this process's — the comparison this whole file exists to make.
async fn spawn(dir: &Path, wire_url: &str) -> Spawned {
    let state = dir.join("state");
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_e2e_daemon"));
    command
        .env("HATCHERY_E2E_STATE", &state)
        .env("HATCHERY_E2E_DATA", dir.join("data"))
        .env("HATCHERY_E2E_BASE_URL", wire_url)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let guard = ChildGuard(command.spawn().expect("spawn the daemon binary"));

    let state_dir = StateDir::at(&state);
    let mut found = None;
    for _ in 0..100 {
        if let Some(info) = state_dir.discover_alive() {
            found = Some(info);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let info = found.expect("the subprocess published its daemon.json in time");
    assert_ne!(
        info.pid,
        std::process::id(),
        "a foreign process, not an in-process runtime"
    );
    Spawned {
        _guard: guard,
        socket: PathBuf::from(&info.endpoint),
        boot_token: info.boot_token,
    }
}

/// Kills the child even when an assertion panics — a test daemon must never leak.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
