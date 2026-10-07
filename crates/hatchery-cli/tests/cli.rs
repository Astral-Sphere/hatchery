//! Integration tests over the real protocol stack: a `TestDaemon` on a real socket drives
//! `exec`'s full path, and the attach logic is exercised against published state and against
//! a child process that dies immediately.

use hatchery_cli::args::ExecArgs;
use hatchery_cli::exec::{ExecOut, Outcome};
use hatchery_protocol::method as m;
use hatchery_testkit::daemon::{ClientProbe, TestDaemon};

/// A ChatCompletions stream with reasoning, content, a stop and usage — full field shape,
/// since the wire layer skips chunks it cannot deserialise.
const SSE_RESONING_OK: &str = concat!(
    "data: {\"id\":\"1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}],\"created\":1718345013,\"model\":\"m\",\"object\":\"chat.completion.chunk\",\"usage\":null}\n\n",
    "data: {\"id\":\"1\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"thinking...\"},\"finish_reason\":null}],\"created\":1718345013,\"model\":\"m\",\"object\":\"chat.completion.chunk\"}\n\n",
    "data: {\"id\":\"1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello, world!\"},\"finish_reason\":null}],\"created\":1718345013,\"model\":\"m\",\"object\":\"chat.completion.chunk\"}\n\n",
    "data: {\"id\":\"1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"created\":1718345013,\"model\":\"m\",\"object\":\"chat.completion.chunk\"}\n\n",
    "data: {\"choices\":[],\"created\":1718345013,\"id\":\"1\",\"model\":\"m\",\"object\":\"chat.completion.chunk\",\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":6,\"total_tokens\":15}}\n\n",
    "data: [DONE]\n\n",
);

const BODY_401: &str =
    "{\"error\":{\"message\":\"Invalid API key\",\"type\":\"invalid_request_error\"}}";

/// Captures everything exec writes, so goldens can assert on it.
#[derive(Default)]
struct VecOut {
    text: String,
    json_lines: Vec<String>,
    notes: Vec<String>,
}

impl ExecOut for VecOut {
    fn text(&mut self, text: &str) {
        self.text.push_str(text);
    }
    fn json_line(&mut self, line: &str) {
        self.json_lines.push(line.to_owned());
    }
    fn note(&mut self, note: &str) {
        self.notes.push(note.to_owned());
    }
}

fn exec_args(prompt: &str, json: bool) -> ExecArgs {
    ExecArgs {
        prompt: prompt.to_owned(),
        json,
        session: None,
        workspace: None,
        model: Some("testprov/m".to_owned()),
        state_dir: None,
    }
}

async fn daemon_with_sse(body: &str) -> (TestDaemon, hatchery_testkit::wire::MockWire) {
    let wire = if body.contains("Invalid API key") {
        hatchery_testkit::wire::MockWire::refuse_always(401, body).await
    } else {
        hatchery_testkit::wire::MockWire::replay_sse(body).await
    };
    // The builtin rows keep an empty env_key (the audit skips them); the provider under test
    // uses `PATH` — a variable every environment has — because a real request reads it.
    let config = format!(
        "[providers.deepseek]\nenv_key = \"\"\n\n[providers.qwen]\nenv_key = \"\"\n\n[providers.testprov]\nbase_url = \"{}\"\nenv_key = \"PATH\"\nmodels = [\"m\"]\n",
        wire.url()
    );
    let daemon = TestDaemon::start(vec![(
        m::ConfigOrigin::User,
        toml::from_str(&config).expect("toml"),
    )])
    .await;
    (daemon, wire)
}

#[tokio::test(flavor = "multi_thread")]
async fn exec_json_streams_item_level_lines_and_completes() {
    let (daemon, _wire) = daemon_with_sse(SSE_RESONING_OK).await;

    let mut out = VecOut::default();
    let info = daemon.state.discover().expect("published");
    let attached = hatchery_cli::attach::attach_to(&info)
        .await
        .expect("attached");
    let outcome =
        hatchery_cli::exec::run_attached(attached, &exec_args("say hi", true), &mut out).await;

    assert_eq!(outcome, Outcome::Completed, "notes: {:?}", out.notes);
    assert_eq!(outcome.exit_code(), 0);
    assert!(
        out.text.is_empty(),
        "json mode writes no plain text: {:?}",
        out.text
    );

    let events: Vec<serde_json::Value> = out
        .json_lines
        .iter()
        .map(|line| serde_json::from_str(line).expect("every line is one JSON object"))
        .collect();
    let types: Vec<&str> = events
        .iter()
        .map(|event| event["event"]["type"].as_str().expect("typed"))
        .collect();
    assert!(types.contains(&"item_started"), "{types:?}");
    assert!(types.contains(&"text_delta"), "{types:?}");
    assert!(types.contains(&"reasoning_delta"), "{types:?}");
    assert!(types.contains(&"item_finished"), "{types:?}");
    assert!(types.contains(&"turn_finished"), "{types:?}");
    // Connection housekeeping stays out of the item-level stream.
    assert!(!types.contains(&"session_updated"), "{types:?}");

    let text_delta = events
        .iter()
        .find(|event| event["event"]["type"] == "text_delta")
        .expect("a text delta");
    assert_eq!(text_delta["event"]["text"], "Hello, world!");
    assert!(text_delta["session"].is_string() && text_delta["generation"].is_number());

    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn exec_plain_prints_text_and_exit_codes_fail_on_401() {
    let (daemon, _wire) = daemon_with_sse(BODY_401).await;

    let mut out = VecOut::default();
    let info = daemon.state.discover().expect("published");
    let attached = hatchery_cli::attach::attach_to(&info)
        .await
        .expect("attached");
    let outcome =
        hatchery_cli::exec::run_attached(attached, &exec_args("hi", false), &mut out).await;

    assert_eq!(outcome, Outcome::Failed);
    assert_eq!(outcome.exit_code(), 1);
    assert!(
        out.notes
            .iter()
            .any(|note| note.contains("the turn failed")),
        "the failure is narrated: {:?}",
        out.notes
    );
    // Only exec's trailing newline reaches stdout: no assistant text came back.
    assert_eq!(
        out.text, "\n",
        "no assistant text came back: {:?}",
        out.text
    );

    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn exec_plain_streams_deltas_as_they_arrive() {
    let (daemon, _wire) = daemon_with_sse(SSE_RESONING_OK).await;

    let mut out = VecOut::default();
    let info = daemon.state.discover().expect("published");
    let attached = hatchery_cli::attach::attach_to(&info)
        .await
        .expect("attached");
    let outcome =
        hatchery_cli::exec::run_attached(attached, &exec_args("hi", false), &mut out).await;

    assert_eq!(outcome, Outcome::Completed);
    // Reasoning is not printed in plain mode; the assistant text is, verbatim.
    assert!(out.text.contains("Hello, world!"), "{:?}", out.text);
    assert!(!out.text.contains("thinking"), "{:?}", out.text);
    assert!(
        out.notes.iter().any(|note| note.contains("session ")),
        "the session line is announced: {:?}",
        out.notes
    );

    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn attach_reuses_a_published_daemon_and_calls_work() {
    let (daemon, _wire) = daemon_with_sse(SSE_RESONING_OK).await;

    let info = daemon.state.discover().expect("published");
    let attached = hatchery_cli::attach::attach_to(&info)
        .await
        .expect("attached");
    assert!(!attached.spawned, "an existing daemon is reused");
    let hello = attached
        .client
        .hello(Some(attached.info.boot_token.clone()))
        .await;
    assert!(hello.is_ok(), "{hello:?}");

    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_spawned_child_that_dies_is_reported_not_polled_out() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = hatchery_daemon::discover::StateDir::at(dir.path().join("state"));
    let error = match hatchery_cli::attach::attach_or_spawn(
        state,
        // `/bin/true` starts, publishes nothing, exits: the attach loop must notice.
        Some(std::path::PathBuf::from("/bin/true")),
        None,
    )
    .await
    {
        Err(error) => error,
        Ok(_) => panic!("a daemon that never published must be refused"),
    };
    assert!(
        error.to_string().contains("did not become ready"),
        "{error}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_detached_childs_fatal_words_reach_the_log_it_is_pointed_at() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    // Stands in for a daemon that dies of a locked database, complaining on stderr exactly the
    // way the foreground `daemon run` does — the words the live acceptance run could only read
    // by giving up on the detached path.
    let doomed = dir.path().join("doomed-daemon.sh");
    std::fs::write(
        &doomed,
        "#!/bin/sh\necho 'store: database: Locking error: Failed locking file' >&2\nexit 1\n",
    )
    .expect("the stand-in is written");
    std::fs::set_permissions(&doomed, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let state_root = dir.path().join("state");
    let state = hatchery_daemon::discover::StateDir::at(state_root.clone());
    let error = match hatchery_cli::attach::attach_or_spawn(state.clone(), Some(doomed), None).await
    {
        Err(error) => error,
        Ok(_) => panic!("a child that publishes nothing must be refused"),
    };
    let message = error.to_string();
    assert!(message.contains("did not become ready"), "{message}");

    let log = state.logs_dir().join(hatchery_cli::attach::STDIO_LOG);
    let captured = std::fs::read_to_string(&log).unwrap_or_else(|error| {
        panic!(
            "the log the refusal points at ({}) holds the child's own words: {error}",
            log.display()
        )
    });
    assert!(
        captured.contains("Locking error"),
        "the fatal startup error survived the detach: {captured:?}"
    );
    assert!(
        message.contains(&log.display().to_string()),
        "and the refusal names that file, not a directory that may hold nothing: {message}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_exec_exit_code_matrix_is_the_documented_one() {
    assert_eq!(Outcome::Completed.exit_code(), 0);
    assert_eq!(Outcome::Failed.exit_code(), 1);
    assert_eq!(Outcome::Cancelled.exit_code(), 2);
    // And the daemon-side config error surfaces as a failed run, not a panic:
    let (daemon, _wire) = daemon_with_sse(SSE_RESONING_OK).await;
    let probe = ClientProbe::attach(&daemon).await;
    let result: Result<m::SessionNewResult, _> = probe
        .try_call(
            m::SESSION_NEW,
            &m::SessionNewParams {
                mode: hatchery_protocol::SessionModeId::chat(),
                workspace: None,
                model: Some(hatchery_protocol::ModelRef::new("ghost", "nope")),
                title: None,
                config_patch: None,
            },
        )
        .await;
    assert!(result.is_err(), "an unknown provider is refused");
    daemon.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_status_and_stop_report_the_published_truth() {
    use hatchery_cli::args::DaemonAction;

    let dir = tempfile::tempdir().expect("tempdir");
    let state_dir = dir.path().join("state");
    let state = hatchery_daemon::discover::StateDir::at(&state_dir);
    state.ensure().expect("state dir");
    let sd = Some(state_dir.clone());

    // Nothing published: status reports not running and exits 1.
    assert_eq!(
        hatchery_cli::daemon_cmd::run(&DaemonAction::Status {
            state_dir: sd.clone(),
        })
        .await,
        1,
        "an empty state dir has no daemon to report"
    );

    // A live publication needs a live pid that is not this process — `discover_alive`
    // deliberately reads a publication naming *our own* pid as stale (a leftover from a crashed
    // same-pid run is exactly the trap it guards against). A sleeping child is the stand-in.
    let mut sleeper = std::process::Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep");
    let info = hatchery_daemon::discover::DaemonInfo {
        pid: sleeper.id(),
        uds_path: state_dir.join("absent.sock").display().to_string(),
        protocol_version: hatchery_protocol::PROTOCOL_VERSION.to_owned(),
        boot_token: "boot-test".to_owned(),
        started_at: 0,
    };
    state.publish(&info).expect("publish");
    assert_eq!(
        hatchery_cli::daemon_cmd::run(&DaemonAction::Status {
            state_dir: sd.clone(),
        })
        .await,
        0,
        "a live foreign pid reads as running"
    );

    // `start` against a live publication reuses it instead of spawning a second daemon.
    assert_eq!(
        hatchery_cli::daemon_cmd::run(&DaemonAction::Start {
            state_dir: sd.clone(),
        })
        .await,
        0,
        "start on a running daemon is a no-op success"
    );

    // A stale publication (the pid is gone): status says so and exits 1.
    let mut died = std::process::Command::new("/bin/true")
        .spawn()
        .expect("spawn /bin/true");
    let dead_pid = died.id();
    assert!(died.wait().expect("waited").success());
    let stale = hatchery_daemon::discover::DaemonInfo {
        pid: dead_pid,
        ..info.clone()
    };
    state.publish(&stale).expect("publish stale");
    assert_eq!(
        hatchery_cli::daemon_cmd::run(&DaemonAction::Status {
            state_dir: sd.clone(),
        })
        .await,
        1,
        "a gone pid reads as stale"
    );

    // `stop` sends SIGTERM to the published pid and waits for it to disappear. The reaper runs
    // concurrently: until the test reaps its own child, the killed pid lingers as a zombie and
    // liveness checks would keep answering "alive".
    state.publish(&info).expect("publish the sleeper again");
    let reaper = tokio::task::spawn_blocking(move || {
        let _ = sleeper.wait();
    });
    assert_eq!(
        hatchery_cli::daemon_cmd::run(&DaemonAction::Stop { state_dir: sd }).await,
        0,
        "the published process was asked to stop and is gone"
    );
    reaper.await.expect("reaped");
    assert!(
        state.discover_alive().is_none(),
        "after stop, nothing answers as alive"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn exec_finishes_cleanly_when_the_daemon_stops_under_it() {
    // A slow provider holds the turn; the daemon is gracefully stopped under it. The daemon
    // cancels the turn first, so the stream ends with a (interrupted) `turn_finished` — exec
    // maps any finished turn to Completed; the exit matrix is client-action-centric (a future
    // refinement may distinguish daemon-initiated interrupts).
    let wire = hatchery_testkit::wire::MockWire::replay_sse_after(
        SSE_RESONING_OK,
        std::time::Duration::from_secs(5),
    )
    .await;
    let config = format!(
        "[providers.deepseek]\nenv_key = \"\"\n\n[providers.qwen]\nenv_key = \"\"\n\n[providers.testprov]\nbase_url = \"{}\"\nenv_key = \"PATH\"\nmodels = [\"m\"]\n",
        wire.url()
    );
    let daemon = TestDaemon::start(vec![(
        m::ConfigOrigin::User,
        toml::from_str(&config).expect("toml"),
    )])
    .await;

    let info = daemon.state.discover().expect("published");
    let attached = hatchery_cli::attach::attach_to(&info)
        .await
        .expect("attached");
    let mut out = VecOut::default();
    // The teardown rides a side task: the run future is polled inline (`VecOut` is not Send).
    let killer = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        daemon.stop().await;
    });
    let outcome =
        hatchery_cli::exec::run_attached(attached, &exec_args("slow", false), &mut out).await;
    killer.await.expect("the teardown task");
    assert_eq!(outcome, Outcome::Completed);
}

#[tokio::test(flavor = "multi_thread")]
async fn exec_json_announces_the_generation_bump() {
    // The assembly's GenerationBumped is item-level news: a script watching the JSONL must see
    // the runtime change it names.
    let (daemon, _wire) = daemon_with_sse(SSE_RESONING_OK).await;

    let mut out = VecOut::default();
    let info = daemon.state.discover().expect("published");
    let attached = hatchery_cli::attach::attach_to(&info)
        .await
        .expect("attached");
    let outcome =
        hatchery_cli::exec::run_attached(attached, &exec_args("hi", true), &mut out).await;
    assert_eq!(outcome, Outcome::Completed);

    let types: Vec<String> = out
        .json_lines
        .iter()
        .filter_map(|line| {
            serde_json::from_str::<serde_json::Value>(line).ok()?["event"]["type"]
                .as_str()
                .map(str::to_owned)
        })
        .collect();
    assert!(
        types.iter().any(|kind| kind == "generation_bumped"),
        "the bump rides the item-level stream: {types:?}"
    );

    daemon.stop().await;
}
