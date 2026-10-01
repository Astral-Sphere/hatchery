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
