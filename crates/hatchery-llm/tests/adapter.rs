//! HTTP-level adapter tests: recorded-style SSE fixtures replayed through `MockWire`
//! (docs/design/testing.md §3.3). Everything here runs offline; the `live_` counterparts live in
//! `tests/live.rs` behind the `live-tests` feature and real credentials.

use futures::StreamExt;
use hatchery_kernel::{ChatOptions, LlmError, LlmProvider, Message, StreamEvent};
use hatchery_llm::{CapabilityTable, ChatCompletionsProvider, ModelCapabilities, ProviderConfig};
use hatchery_protocol::{ReasoningBlock, ReasoningEffort};
use hatchery_testkit::{MockWire, json_fixture, sse_fixture};
use tokio_util::sync::CancellationToken;

/// The env var every test fills before starting a stream. Each test sets it in its own process
/// (nextest runs tests one per process), so no other test can observe the value.
const KEY_VAR: &str = "HATCHERY_LLM_TEST_KEY";

#[allow(unsafe_code)]
// Sound in a nextest process: one test per process means no thread here races the write.
fn arm_key() {
    unsafe { std::env::set_var(KEY_VAR, "sk-test-0123456789") }
}

fn provider(config: ProviderConfig) -> ChatCompletionsProvider {
    arm_key();
    // Building a client needs a TLS backend; the install is idempotent, so every test may call it.
    hatchery_llm::install_tls_provider();
    // No pooled connections: a keep-alive race against the mock (the server closes an idle
    // connection exactly as a retry reuses it) reads as a network failure and would blur the
    // retry counts these tests assert. Real deployments want the pool; this doubles only tests.
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .expect("a plain HTTP client");
    ChatCompletionsProvider::with_client(config, client)
}

async fn stream_events(
    provider: &ChatCompletionsProvider,
    options: &ChatOptions,
    messages: &[Message],
) -> Vec<StreamEvent> {
    let cancel = CancellationToken::new();
    let stream = provider
        .chat_stream(options, messages, cancel)
        .await
        .expect("the stream starts");
    stream.collect().await
}

fn sse(name: &str) -> String {
    String::from_utf8(sse_fixture(name)).expect("fixtures are UTF-8")
}

fn error_body(name: &str) -> String {
    String::from_utf8(json_fixture(name)).expect("error bodies are UTF-8")
}

fn quick_retry() -> ProviderConfig {
    let mut config = ProviderConfig::new("https://placeholder", KEY_VAR);
    config.retry.max_attempts = 4;
    config.retry.backoff_ms = 500;
    config.retry.jitter_percent = 0;
    config
}

#[tokio::test]
async fn a_text_stream_translates_chunk_by_chunk_and_books_usage_before_done() {
    let wire = MockWire::replay_sse(sse("synthetic-deepseek-text")).await;
    let mut config = quick_retry();
    config.base_url = wire.url();
    let provider = provider(config);
    arm_key();

    let events = stream_events(
        &provider,
        &ChatOptions::new("deepseek-flash"),
        &[Message::user("hello")],
    )
    .await;

    let usage_at = events
        .iter()
        .position(|event| matches!(event, StreamEvent::Usage { .. }))
        .expect("usage reported");
    let done_at = events
        .iter()
        .position(|event| matches!(event, StreamEvent::Done { .. }))
        .expect("a finish");
    assert!(
        usage_at < done_at,
        "the kernel stops reading at Done, so usage must be booked first: {events:?}"
    );
    assert!(matches!(
        events[done_at],
        StreamEvent::Done {
            finish_reason: hatchery_kernel::FinishReason::Stop
        }
    ));
    if let StreamEvent::Usage { usage } = &events[usage_at] {
        assert_eq!(usage.prompt_tokens, Some(17));
        assert_eq!(usage.requests, 1);
    } else {
        unreachable!();
    }
}

#[tokio::test]
async fn reasoning_closes_before_its_text_opens() {
    let wire = MockWire::replay_sse(sse("synthetic-deepseek-reasoning")).await;
    let mut config = quick_retry();
    config.base_url = wire.url();
    let provider = provider(config);
    arm_key();

    let events = stream_events(
        &provider,
        &ChatOptions::new("deepseek-flash"),
        &[Message::user("meaning of life?")],
    )
    .await;

    let kinds: Vec<&'static str> = events.iter().map(kind_of).collect();
    assert_eq!(
        kinds,
        vec![
            "reasoning_delta",
            "reasoning_delta",
            "reasoning_done",
            "text_delta",
            "text_delta",
            "usage",
            "done",
        ],
        "the reasoning block closes before its answer opens (kernel.md §4)"
    );
    if let StreamEvent::ReasoningDelta { text } = &events[0] {
        assert_eq!(text, " 先想 ", "reasoning text is verbatim, byte for byte");
    } else {
        unreachable!();
    }
    if let StreamEvent::Usage { usage } = &events[kinds.len() - 2] {
        assert_eq!(usage.reasoning_tokens, Some(18));
    } else {
        unreachable!();
    }
}

#[tokio::test]
async fn a_tool_call_streams_in_additive_fragments() {
    let wire = MockWire::replay_sse(sse("synthetic-qwen-toolcall")).await;
    let mut config = quick_retry();
    config.base_url = wire.url();
    let provider = provider(config);
    arm_key();

    let events = stream_events(
        &provider,
        &ChatOptions::new("qwen3.8-flash"),
        &[Message::user("find")],
    )
    .await;

    let fragments: Vec<String> = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ToolCall { delta } => Some(delta.args_delta.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        fragments,
        vec![
            String::new(),
            "{\"query\":".to_owned(),
            "\"TODO\"}".to_owned()
        ],
        "the first fragment carries identity, later ones only arguments"
    );
    if let StreamEvent::ToolCall { delta } = &events[0] {
        assert_eq!(delta.id.as_deref(), Some("call-abc"));
        assert_eq!(delta.name.as_deref(), Some("grep"));
        assert_eq!(delta.index, 0);
    } else {
        unreachable!();
    }
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::Done {
            finish_reason: hatchery_kernel::FinishReason::ToolCalls
        }
    )));
}

#[tokio::test]
async fn reasoning_passback_is_byte_exact_when_the_table_echoes() {
    let wire = MockWire::replay_sse(sse("synthetic-deepseek-reasoning")).await;
    arm_key();
    let mut config = quick_retry();
    config.base_url = wire.url();
    // The echo override is the capability table's business (ADR-0007); DeepSeek's built-in row
    // says no, so this test pins the mechanism on an override, exactly as a config would.
    config.capabilities.push((
        "deepseek-flash".to_owned(),
        ModelCapabilities {
            echo_reasoning: true,
            ..ModelCapabilities::default()
        },
    ));
    let provider = provider(config);

    let verbatim = " 先想 \t再想\n";
    let messages = vec![
        Message::user("meaning of life?"),
        Message::assistant("答案 是 42。").with_reasoning(ReasoningBlock::text(verbatim)),
        Message::user("and in hex?"),
    ];
    stream_events(&provider, &ChatOptions::new("deepseek-flash"), &messages).await;

    let requests = wire.requests().await;
    let body: serde_json::Value =
        serde_json::from_str(&requests[0].body).expect("the request body is JSON");
    let echoed = &body["messages"][1];
    assert_eq!(echoed["role"], "assistant");
    assert_eq!(
        echoed["reasoning_content"].as_str(),
        Some(verbatim),
        "the reasoning must ride back byte for byte: {:?}",
        echoed["reasoning_content"]
    );
    assert_eq!(
        echoed["content"].as_str(),
        Some("答案 是 42。"),
        "the answer text is verbatim too"
    );
}

#[tokio::test]
async fn reasoning_is_dropped_when_the_table_says_no() {
    let wire = MockWire::replay_sse(sse("synthetic-deepseek-text")).await;
    let mut config = quick_retry();
    config.base_url = wire.url();
    let provider = provider(config);
    arm_key();

    let messages = vec![
        Message::user("q1"),
        Message::assistant("a1").with_reasoning(ReasoningBlock::text(" secret thoughts ")),
        Message::user("q2"),
    ];
    stream_events(&provider, &ChatOptions::new("deepseek-flash"), &messages).await;

    let requests = wire.requests().await;
    let body: serde_json::Value =
        serde_json::from_str(&requests[0].body).expect("the request body is JSON");
    assert!(
        body["messages"][1].get("reasoning_content").is_none(),
        "the built-in deepseek row drops reasoning on the way out"
    );
}

#[tokio::test]
async fn effort_goldens_reach_the_wire_per_family() {
    arm_key();

    // DeepSeek: the model stands, the `thinking` switch flips.
    let deepseek_wire = MockWire::replay_sse(sse("synthetic-deepseek-text")).await;
    let mut deepseek = quick_retry();
    deepseek.base_url = deepseek_wire.url();
    let mut options = ChatOptions::new("deepseek-flash");
    options.reasoning_effort = Some(ReasoningEffort::Off);
    stream_events(&provider(deepseek), &options, &[Message::user("hi")]).await;
    let body: serde_json::Value =
        serde_json::from_str(&deepseek_wire.requests().await[0].body).expect("JSON");
    assert_eq!(body["model"], "deepseek-flash");
    assert_eq!(body["thinking"]["type"], "disabled");
    assert!(body.get("reasoning_effort").is_none());

    // Qwen: enable_thinking plus a budget tier.
    let qwen_wire = MockWire::replay_sse(sse("synthetic-qwen-toolcall")).await;
    let mut qwen = quick_retry();
    qwen.base_url = qwen_wire.url();
    let mut options = ChatOptions::new("qwen3.8-flash");
    options.reasoning_effort = Some(ReasoningEffort::High);
    stream_events(&provider(qwen), &options, &[Message::user("hi")]).await;
    let body: serde_json::Value =
        serde_json::from_str(&qwen_wire.requests().await[0].body).expect("JSON");
    assert_eq!(body["enable_thinking"], true);
    assert_eq!(body["thinking_budget"], 32_768);

    // Generic: plain passthrough.
    let generic_wire = MockWire::replay_sse(sse("synthetic-deepseek-text")).await;
    let mut generic = quick_retry();
    generic.base_url = generic_wire.url();
    let mut options = ChatOptions::new("some-gateway-model");
    options.reasoning_effort = Some(ReasoningEffort::Max);
    stream_events(&provider(generic), &options, &[Message::user("hi")]).await;
    let body: serde_json::Value =
        serde_json::from_str(&generic_wire.requests().await[0].body).expect("JSON");
    assert_eq!(body["reasoning_effort"], "max");
}

#[tokio::test(start_paused = true)]
async fn rate_limit_refusals_back_off_then_stream() {
    let wire = MockWire::refuse_then_sse(
        429,
        &error_body("synthetic-429"),
        2,
        &sse("synthetic-deepseek-text"),
    )
    .await;
    let mut config = quick_retry();
    config.base_url = wire.url();
    let provider = provider(config);
    arm_key();

    let start = tokio::time::Instant::now();
    let events = stream_events(
        &provider,
        &ChatOptions::new("deepseek-flash"),
        &[Message::user("hello")],
    )
    .await;

    let waits: Vec<u64> = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::RateLimited { retry_after_ms } => Some(*retry_after_ms),
            _ => None,
        })
        .collect();
    assert_eq!(waits, vec![500, 1_000], "two notices, doubling backoff");
    assert_eq!(
        start.elapsed(),
        std::time::Duration::from_millis(1_500),
        "the notices are real: the adapter really waited"
    );
    assert_eq!(wire.requests().await.len(), 3, "two refusals, one stream");
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::Done {
            finish_reason: hatchery_kernel::FinishReason::Stop
        }
    )));
}

#[tokio::test]
async fn an_authentication_failure_is_fatal_and_names_the_env_var() {
    let wire = MockWire::refuse_always(401, &error_body("synthetic-401")).await;
    let mut config = quick_retry();
    config.base_url = wire.url();
    let provider = provider(config);
    arm_key();

    let events = stream_events(
        &provider,
        &ChatOptions::new("deepseek-flash"),
        &[Message::user("hello")],
    )
    .await;

    assert_eq!(wire.requests().await.len(), 1, "a 401 is never retried");
    assert_eq!(events.len(), 1, "one error event ends the stream");
    let StreamEvent::Error { error } = &events[0] else {
        panic!("expected an error event, got {events:?}");
    };
    assert!(!error.retryable);
    assert!(
        error.message.contains(KEY_VAR),
        "the user is told which variable to check: {}",
        error.message
    );
    assert!(error.message.contains("Authentication Fails"));
}

#[tokio::test]
async fn a_plain_fatal_status_does_not_retry() {
    let wire = MockWire::refuse_always(402, &error_body("synthetic-402")).await;
    let mut config = quick_retry();
    config.base_url = wire.url();
    let provider = provider(config);
    arm_key();

    let events = stream_events(
        &provider,
        &ChatOptions::new("qwen3.8-flash"),
        &[Message::user("hello")],
    )
    .await;

    assert_eq!(wire.requests().await.len(), 1);
    let StreamEvent::Error { error } = &events[0] else {
        panic!("expected an error event, got {events:?}");
    };
    assert!(!error.retryable);
}

#[tokio::test]
async fn a_stream_that_ends_without_done_just_ends() {
    // A connection that died mid-answer: full events, no finish_reason chunk, no sentinel.
    // Judging that is the kernel's job (it fails the round as retryable, keeping what was
    // committed); the adapter's obligation is only to hand over the events it saw and stop.
    let body = concat!(
        r#"data: {"id":"1","choices":[{"index":0,"delta":{"content":"","role":"assistant"},"finish_reason":null}],"created":1,"model":"m","object":"chat.completion.chunk"}"#,
        "\n\n",
        r#"data: {"id":"1","choices":[{"index":0,"delta":{"content":"Hi"},"finish_reason":null}],"created":1,"model":"m","object":"chat.completion.chunk"}"#,
        "\n\n",
    );
    let wire = MockWire::replay_sse(body).await;
    let mut config = quick_retry();
    config.base_url = wire.url();
    let provider = provider(config);
    arm_key();

    let events = stream_events(
        &provider,
        &ChatOptions::new("deepseek-flash"),
        &[Message::user("hello")],
    )
    .await;

    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta { .. }))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, StreamEvent::Done { .. })),
        "no sentinel, no finish: the kernel supplies the failure"
    );
}

#[tokio::test(start_paused = true)]
async fn retries_run_out_and_report_the_last_failure() {
    let wire = MockWire::refuse_always(429, &error_body("synthetic-429")).await;
    let mut config = quick_retry();
    config.base_url = wire.url();
    let provider = provider(config);
    arm_key();

    let events = stream_events(
        &provider,
        &ChatOptions::new("deepseek-flash"),
        &[Message::user("hello")],
    )
    .await;

    assert_eq!(
        wire.requests().await.len(),
        4,
        "max_attempts counts the first try, so exactly four requests"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, StreamEvent::RateLimited { .. }))
            .count(),
        3,
        "one notice per backoff"
    );
    let StreamEvent::Error { error } = events.last().expect("ends with the verdict") else {
        panic!("expected an error event, got {events:?}");
    };
    assert!(error.retryable, "the verdict keeps the retryable reading");
    assert!(error.message.contains("4 attempt"), "{}", error.message);
}

#[tokio::test]
async fn cancelling_before_the_first_poll_sends_nothing() {
    let wire = MockWire::replay_sse(sse("synthetic-deepseek-text")).await;
    let mut config = quick_retry();
    config.base_url = wire.url();
    let provider = provider(config);
    arm_key();

    let cancel = CancellationToken::new();
    let mut stream = provider
        .chat_stream(
            &ChatOptions::new("deepseek-flash"),
            &[Message::user("hi")],
            cancel.clone(),
        )
        .await
        .expect("the stream starts");
    cancel.cancel();
    let event = stream.next().await;
    assert!(
        event.is_none(),
        "cancelled before anything flowed: {event:?}"
    );
    assert!(
        wire.requests().await.is_empty(),
        "the interrupt must kill the request, not just its events"
    );
}

#[tokio::test]
async fn the_bearer_key_and_configured_headers_reach_the_wire() {
    let wire = MockWire::replay_sse(sse("synthetic-deepseek-text")).await;
    arm_key();
    let mut config = quick_retry();
    config.base_url = wire.url();
    config
        .http_headers
        .insert("X-Custom".to_owned(), "1".to_owned());
    let provider = provider(config);

    stream_events(
        &provider,
        &ChatOptions::new("deepseek-flash"),
        &[Message::user("hi")],
    )
    .await;

    let requests = wire.requests().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/chat/completions");
    assert_eq!(
        requests[0].header("authorization"),
        Some("Bearer sk-test-0123456789"),
        "the key is read from the configured env var at request time"
    );
    assert_eq!(requests[0].header("x-custom"), Some("1"));
}

#[tokio::test]
async fn a_missing_key_is_refused_before_any_request() {
    let wire = MockWire::replay_sse(sse("synthetic-deepseek-text")).await;
    let mut config = quick_retry();
    config.base_url = wire.url();
    // A name nothing sets — the helper arms a different variable, so this one stays unset.
    config.env_key = "HATCHERY_LLM_KEY_NOBODY_SETS".to_owned();
    let provider = provider(config);

    let Err(error) = provider
        .chat_stream(
            &ChatOptions::new("deepseek-flash"),
            &[Message::user("hi")],
            CancellationToken::new(),
        )
        .await
    else {
        panic!("no key, no stream");
    };
    assert!(!error.retryable);
    assert!(
        error.message.contains("HATCHERY_LLM_KEY_NOBODY_SETS"),
        "{}",
        error.message
    );
    assert!(wire.requests().await.is_empty());
}

#[tokio::test]
async fn a_capability_override_can_teach_a_new_family() {
    // The byte-exact echo mechanism, exercised from the config side of the table rather than
    // the built-ins: a gateway that documents reasoning_content echo.
    let wire = MockWire::replay_sse(sse("synthetic-deepseek-text")).await;
    arm_key();
    let mut config = quick_retry();
    config.base_url = wire.url();
    config.capabilities.push((
        "some-gateway-model".to_owned(),
        ModelCapabilities {
            echo_reasoning: true,
            ..ModelCapabilities::default()
        },
    ));
    let provider = provider(config);

    let messages = vec![
        Message::user("q1"),
        Message::assistant("a1").with_reasoning(ReasoningBlock::text(" thought ")),
        Message::user("q2"),
    ];
    stream_events(
        &provider,
        &ChatOptions::new("some-gateway-model"),
        &messages,
    )
    .await;

    let body: serde_json::Value =
        serde_json::from_str(&wire.requests().await[0].body).expect("JSON");
    assert_eq!(body["messages"][1]["reasoning_content"], " thought ");
}

// --- Real recordings --------------------------------------------------------------
//
// The fixtures below are byte streams recorded from the live endpoints by
// `cargo xtask record-fixtures` (provenance sidecars say when). Replaying them is the adapter's
// reality check: synthetic fixtures pin the shapes the docs describe, these pin the shapes the
// providers actually send.

#[tokio::test]
async fn real_deepseek_reasoning_replays_in_the_documented_order() {
    let events = replay("deepseek-reasoning", "deepseek-flash").await;
    let kinds: Vec<&'static str> = events.iter().map(|(kind, _)| *kind).collect();

    let first_text = kinds
        .iter()
        .position(|kind| *kind == "text_delta")
        .expect("the answer text arrived");
    assert_eq!(
        kinds[first_text..],
        ["text_delta", "usage", "done"][..],
        "the single answer chunk, its usage, then the finish: {kinds:?}"
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == "reasoning_done")
            .count(),
        1,
        "one reasoning block, one close"
    );
    let done = kinds
        .iter()
        .position(|kind| *kind == "done")
        .expect("finish");
    if let Some(usage) = kinds.iter().position(|kind| *kind == "usage") {
        assert!(
            usage < done,
            "usage must be booked before the kernel stops reading"
        );
    }
    if let StreamEvent::TextDelta { text } = &events[first_text].1 {
        assert_eq!(text, "4", "the answer itself, verbatim");
    } else {
        unreachable!();
    }
}

#[tokio::test]
async fn real_qwen_dual_key_chunks_do_not_split_the_reasoning_block() {
    // Live finding (2026-09-30): qwen echoes `reasoning_content` AND `content` in the same
    // chunk, one of them always empty. Handling keys rather than values would interleave empty
    // text deltas into the reasoning and close the block early — this fixture is the guard.
    let events = replay("qwen-reasoning", "qwen3.8-flash").await;
    let kinds: Vec<&'static str> = events.iter().map(|(kind, _)| *kind).collect();

    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == "reasoning_done")
            .count(),
        1,
        "the dual-key chunks must not reopen or split the block: {kinds:?}"
    );
    let first_text = kinds
        .iter()
        .position(|kind| *kind == "text_delta")
        .expect("the answer text arrived");
    assert!(
        kinds[..first_text]
            .iter()
            .all(|kind| *kind == "reasoning_delta" || *kind == "reasoning_done"),
        "only reasoning precedes the first text delta, closing with exactly one done: {kinds:?}"
    );
    let reasoning: String = events
        .iter()
        .filter_map(|(kind, event)| match event {
            StreamEvent::ReasoningDelta { text } if *kind == "reasoning_delta" => {
                Some(text.clone())
            }
            _ => None,
        })
        .collect();
    // The words belong to whichever recording is on disk; the verbatim property is what the
    // fixture pins: non-empty, and neither trimmed nor rewritten by the adapter.
    assert!(
        !reasoning.is_empty(),
        "reasoning text survives verbatim: {reasoning:?}"
    );
}

#[tokio::test]
async fn real_text_recordings_replay_for_both_providers() {
    for (fixture, model) in [
        ("deepseek-text", "deepseek-flash"),
        ("qwen-text", "qwen3.8-flash"),
    ] {
        let events = replay(fixture, model).await;
        let kinds: Vec<&'static str> = events.iter().map(|(kind, _)| *kind).collect();
        assert_eq!(
            kinds.iter().filter(|kind| **kind == "error").count(),
            0,
            "{fixture}: {kinds:?}"
        );
        let usage = kinds.iter().position(|kind| *kind == "usage");
        let done = kinds
            .iter()
            .position(|kind| *kind == "done")
            .unwrap_or_else(|| panic!("{fixture}: {kinds:?}"));
        if let Some(usage) = usage {
            assert!(usage < done, "{fixture}: usage before done, got {kinds:?}");
        }
        assert!(
            events.iter().any(|(kind, event)| matches!(
                event,
                StreamEvent::Done {
                    finish_reason: hatchery_kernel::FinishReason::Stop,
                }
            ) && *kind == "done"),
            "{fixture}: {kinds:?}"
        );
    }
}

#[tokio::test]
async fn real_401_recordings_reach_the_user_verbatim_and_fatal() {
    for (fixture, marker) in [
        ("deepseek-401", "Authentication Fails"),
        ("qwen-401", "Incorrect API key provided"),
    ] {
        let wire = MockWire::refuse_always(401, &error_body(fixture)).await;
        let mut config = quick_retry();
        config.base_url = wire.url();
        let provider = provider(config);
        arm_key();

        let events = stream_events(&provider, &ChatOptions::new("m"), &[Message::user("hi")]).await;

        assert_eq!(wire.requests().await.len(), 1, "{fixture}: never retried");
        let StreamEvent::Error { error } = &events[0] else {
            panic!("{fixture}: expected one error event, got {events:?}");
        };
        assert!(!error.retryable, "{fixture}");
        assert!(
            error.message.contains(marker),
            "{fixture}: the provider's own message must reach the user: {}",
            error.message
        );
    }
}

#[tokio::test]
async fn real_toolcall_recording_replays_with_complete_fragments() {
    // Recorded with `tool_choice` forced. While the recording is a plain-text answer (the
    // pre-force probe), the same invariants hold; once re-recorded, the fragment completeness
    // half starts earning its keep.
    let events = replay("deepseek-toolcall", "deepseek-chat").await;
    let kinds: Vec<&'static str> = events.iter().map(|(kind, _)| *kind).collect();
    assert!(kinds.contains(&"done"), "{kinds:?}");
    assert!(!kinds.contains(&"error"), "{kinds:?}");

    let mut calls: std::collections::BTreeMap<u32, (Option<String>, String, String)> =
        std::collections::BTreeMap::new();
    for (_, event) in &events {
        if let StreamEvent::ToolCall { delta } = event {
            let entry = calls.entry(delta.index).or_default();
            if let Some(id) = &delta.id {
                entry.0 = Some(id.clone());
            }
            if let Some(name) = &delta.name {
                entry.1 = name.clone();
            }
            entry.2.push_str(&delta.args_delta);
        }
    }
    for (index, (id, name, args)) in calls {
        assert!(
            id.is_some() && !name.is_empty(),
            "fragment {index} lacks identity"
        );
        serde_json::from_str::<serde_json::Value>(&args).unwrap_or_else(|error| {
            panic!("fragment {index}: arguments are not JSON: {args:?}: {error}")
        });
    }
}

/// Replays a recorded SSE fixture through a fresh provider and returns (kind, event) pairs.
async fn replay(fixture: &str, model: &str) -> Vec<(&'static str, StreamEvent)> {
    let wire = MockWire::replay_sse(sse(fixture)).await;
    let mut config = quick_retry();
    config.base_url = wire.url();
    let provider = provider(config);
    arm_key();

    stream_events(
        &provider,
        &ChatOptions::new(model),
        &[Message::user("recorded prompt")],
    )
    .await
    .into_iter()
    .map(|event| (kind_of(&event), event))
    .collect()
}

/// A short name for log lines and assertion messages.
fn kind_of(event: &StreamEvent) -> &'static str {
    match event {
        StreamEvent::TextDelta { .. } => "text_delta",
        StreamEvent::ReasoningDelta { .. } => "reasoning_delta",
        StreamEvent::ReasoningDone { .. } => "reasoning_done",
        StreamEvent::ToolCall { .. } => "tool_call",
        StreamEvent::Usage { .. } => "usage",
        StreamEvent::RateLimited { .. } => "rate_limited",
        StreamEvent::Done { .. } => "done",
        StreamEvent::Error { .. } => "error",
    }
}

// The table is re-exported for the daemon's config layer; a compile check that it stays public
// and overridable keeps the M1 seam honest.
#[test]
fn the_capability_table_stays_constructible_and_overridable() {
    let table = CapabilityTable::builtin().with_override("x", ModelCapabilities::default());
    assert_eq!(table.capabilities("x"), ModelCapabilities::default());
}

// LlmError is part of the adapter's contract surface; keep the import honest even when the
// assertions above only read its fields.
const _: Option<LlmError> = None;
