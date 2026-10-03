//! The turn state machine: every path a turn can take, and what it records.
//!
//! Fakes come from `hatchery-testkit`; the assertions are about the event sequence and the items
//! committed, because that is what the daemon projects and what the store writes.

use std::sync::Arc;

use serde_json::json;

use hatchery_kernel::{
    Agent, AgentBuilder, AgentCommand, AgentHandle, ChatOptions, FinishReason, KernelError,
    KernelEvent, LlmError, Message, Ports, StreamEvent, TurnCompletion, TurnLimits, TurnState,
};
use hatchery_protocol::{
    Item, ItemId, ItemKind, ItemKindTag, RiskLevel, SessionId, SignatureBlock, StopReason,
    ToolOutput, ToolStatus,
};
use hatchery_testkit::{
    MemoryHistory, RecordingSink, ScriptedApproval, ScriptedProvider, ScriptedToolHost,
    answer_approvals, error, finished_items, kinds, reason, states, tool_result_texts,
};

/// A kernel wired to scripted fakes, already running.
struct Harness {
    session: SessionId,
    sink: Arc<RecordingSink>,
    provider: Arc<ScriptedProvider>,
    tools: Arc<ScriptedToolHost>,
    /// Kept so a test can play the store between turns: commit what a turn produced, as the
    /// daemon's sink-to-store-to-history loop would.
    history: Arc<MemoryHistory>,
    handle: AgentHandle,
    running: tokio::task::JoinHandle<()>,
}

impl Harness {
    fn new(
        provider: ScriptedProvider,
        tools: ScriptedToolHost,
        history: MemoryHistory,
        limits: TurnLimits,
    ) -> Self {
        let session = SessionId::new();
        let sink = Arc::new(RecordingSink::new());
        let provider = Arc::new(provider);
        let tools = Arc::new(tools);
        let history = Arc::new(history);

        let (agent, handle) = AgentBuilder::new(
            session,
            ChatOptions::new("scripted-model"),
            Ports::new(
                Arc::clone(&provider) as Arc<dyn hatchery_kernel::LlmProvider>,
                Arc::clone(&tools) as Arc<dyn hatchery_kernel::ToolHost>,
                Arc::clone(&history) as Arc<dyn hatchery_kernel::HistorySource>,
                Arc::clone(&sink) as Arc<dyn hatchery_kernel::EventSink>,
            ),
        )
        .limits(limits)
        .build();

        let running = tokio::spawn(Agent::run(agent));
        Self {
            session,
            sink,
            provider,
            tools,
            history,
            handle,
            running,
        }
    }

    async fn prompt(&self, text: &str) {
        self.handle
            .submit(AgentCommand::prompt(text))
            .await
            .expect("the agent is running");
    }

    /// Waits for the turn to end and returns the events it produced.
    async fn finish(&self) -> Vec<KernelEvent> {
        self.sink.wait_for_end().await
    }

    /// Answers approvals as a frontend would.
    fn auto_approve(&self, policy: ScriptedApproval) -> tokio::task::JoinHandle<()> {
        answer_approvals(self.handle.clone(), self.sink.subscribe(), policy)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.running.abort();
    }
}

// ------------------------------------------------------------------ happy paths

#[tokio::test]
async fn a_plain_round_finishes_the_turn() {
    let provider = ScriptedProvider::new(vec![ScriptedProvider::text_round("because it walks")]);
    let harness = Harness::new(
        provider,
        ScriptedToolHost::new(),
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    harness.prompt("why is the store slow?").await;
    let events = harness.finish().await;

    assert_eq!(reason(&events), Some(StopReason::ModelDone));
    assert_eq!(
        kinds(&events),
        vec![ItemKindTag::UserMessage, ItemKindTag::AssistantMessage],
        "one round records the question and the answer"
    );
    assert_eq!(
        states(&events),
        vec!["assembling", "streaming", "idle"],
        "every transition is reported, including the way back to idle"
    );

    let requests = harness.provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].options.model, "scripted-model");
    assert_eq!(
        requests[0].messages.len(),
        1,
        "an empty history plus this turn's input"
    );
    assert_eq!(
        requests[0].messages[0].content.text,
        "why is the store slow?"
    );
}

#[tokio::test]
async fn items_form_one_chain_from_the_history_head() {
    let head: ItemId = "01890f47-0000-7000-8000-0000000000aa"
        .parse()
        .expect("a valid uuid");
    let provider = ScriptedProvider::new(vec![vec![
        StreamEvent::ReasoningDelta {
            text: "thinking".to_owned(),
        },
        StreamEvent::TextDelta {
            text: "answering".to_owned(),
        },
        StreamEvent::Done {
            finish_reason: FinishReason::Stop,
        },
    ]]);
    let harness = Harness::new(
        provider,
        ScriptedToolHost::new(),
        MemoryHistory::empty().with_head(head),
        TurnLimits::default(),
    );
    harness.prompt("hello").await;
    let events = harness.finish().await;

    let items = finished_items(&events);
    assert_eq!(
        kinds(&events),
        vec![
            ItemKindTag::UserMessage,
            ItemKindTag::Reasoning,
            ItemKindTag::AssistantMessage
        ],
        "reasoning and text are separate items"
    );
    assert_eq!(
        items[0].parent,
        Some(head),
        "the chain starts at the branch head"
    );
    assert_eq!(items[1].parent, Some(items[0].id));
    assert_eq!(items[2].parent, Some(items[1].id));
    assert!(
        items.iter().all(|item| item.session == harness.session),
        "every item carries its session"
    );
}

#[tokio::test]
async fn reasoning_is_streamed_and_stored_verbatim() {
    let reasoning = " leading, trailing and\nnewlines \n";
    let signature = SignatureBlock::new(SignatureBlock::OPENAI_ENCRYPTED_CONTENT, "opaque-blob");
    let provider = ScriptedProvider::new(vec![
        vec![
            StreamEvent::ReasoningDelta {
                text: reasoning.to_owned(),
            },
            StreamEvent::ReasoningDone {
                signature: Some(signature.clone()),
            },
            StreamEvent::ToolCall {
                delta: hatchery_kernel::ToolCallDelta {
                    index: 0,
                    id: Some("call-1".to_owned()),
                    name: Some("read_file".to_owned()),
                    args_delta: json!({"path": "a.rs"}).to_string(),
                },
            },
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
            },
        ],
        ScriptedProvider::text_round("done"),
    ]);
    let tools = ScriptedToolHost::new()
        .advertising(&["read_file"])
        .answering("read_file", ToolOutput::text("fn main() {}"));
    let harness = Harness::new(
        provider,
        tools,
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    harness.prompt("read it").await;
    let events = harness.finish().await;

    let item = finished_items(&events)
        .into_iter()
        .find(|item| item.kind_tag() == ItemKindTag::Reasoning)
        .expect("a reasoning item");
    match item.kind {
        ItemKind::Reasoning(block) => {
            assert_eq!(block.text, reasoning, "reasoning must not be normalised");
            assert_eq!(block.signature, Some(signature.clone()));
        }
        other => panic!("expected a reasoning item, got {other:?}"),
    }

    // The second request replays it: that is the whole point of storing it (ADR-0007).
    let messages = &harness.provider.requests()[1].messages;
    let assistant = messages
        .iter()
        .find(|message| !message.tool_calls.is_empty())
        .expect("the assistant message that asked for the tool");
    let replayed = assistant.reasoning.clone().expect("reasoning is replayed");
    assert_eq!(
        replayed.text, reasoning,
        "byte for byte, on the way back out"
    );
    assert_eq!(replayed.signature, Some(signature));
}

#[tokio::test]
async fn a_signature_that_arrives_after_the_text_cannot_be_stored_but_still_replays() {
    // The provider contract (docs/design/kernel.md §4) puts `ReasoningDone` before the text that
    // follows its reasoning block. This is what an adapter that breaks it costs: that text delta
    // closed and committed the reasoning item, and items are append-only, so the stored reasoning
    // replays unsigned. The turn's own next request still carries the signature — losing it there
    // as well would break a conversation that is still running, which is the half of the mistake
    // the kernel can still avoid.
    let reasoning = " thinking ";
    let signature = SignatureBlock::new(SignatureBlock::OPENAI_ENCRYPTED_CONTENT, "late-blob");
    let provider = ScriptedProvider::new(vec![
        vec![
            StreamEvent::ReasoningDelta {
                text: reasoning.to_owned(),
            },
            StreamEvent::TextDelta {
                text: "let me look".to_owned(),
            },
            StreamEvent::ReasoningDone {
                signature: Some(signature.clone()),
            },
            StreamEvent::ToolCall {
                delta: hatchery_kernel::ToolCallDelta {
                    index: 0,
                    id: Some("call-1".to_owned()),
                    name: Some("read_file".to_owned()),
                    args_delta: json!({"path": "a.rs"}).to_string(),
                },
            },
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
            },
        ],
        ScriptedProvider::text_round("done"),
    ]);
    let tools = ScriptedToolHost::new()
        .advertising(&["read_file"])
        .answering("read_file", ToolOutput::text("fn main() {}"));
    let harness = Harness::new(
        provider,
        tools,
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    harness.prompt("read it").await;
    let events = harness.finish().await;

    let item = finished_items(&events)
        .into_iter()
        .find(|item| item.kind_tag() == ItemKindTag::Reasoning)
        .expect("a reasoning item");
    match item.kind {
        ItemKind::Reasoning(block) => {
            assert_eq!(block.text, reasoning, "the reasoning itself is stored");
            assert!(
                block.signature.is_none(),
                "the signature arrived after this item was committed, so it cannot be on it"
            );
        }
        other => panic!("expected a reasoning item, got {other:?}"),
    }

    let messages = &harness.provider.requests()[1].messages;
    let assistant = messages
        .iter()
        .find(|message| !message.tool_calls.is_empty())
        .expect("the assistant message that asked for the tool");
    assert_eq!(
        assistant
            .reasoning
            .clone()
            .expect("reasoning is replayed")
            .signature,
        Some(signature),
        "the running turn keeps replaying under the signature it was given"
    );
}

#[tokio::test]
async fn a_tool_result_is_appended_to_the_next_request() {
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool_round("call-1", "read_file", json!({"path": "a.rs"})),
        ScriptedProvider::text_round("the file defines main"),
    ]);
    let tools = ScriptedToolHost::new()
        .advertising(&["read_file"])
        .answering("read_file", ToolOutput::text("fn main() {}"));
    let harness = Harness::new(
        provider,
        tools,
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    harness.prompt("read it").await;
    let events = harness.finish().await;

    assert_eq!(reason(&events), Some(StopReason::ModelDone));
    assert_eq!(
        kinds(&events),
        vec![
            ItemKindTag::UserMessage,
            ItemKindTag::ToolCall,
            ItemKindTag::ToolResult,
            ItemKindTag::AssistantMessage,
        ]
    );
    assert_eq!(
        states(&events),
        vec!["assembling", "streaming", "executing", "streaming", "idle"]
    );

    let messages = &harness.provider.requests()[1].messages;
    assert_eq!(messages.len(), 3, "user, assistant-with-call, tool result");
    let result = messages.last().expect("a tool result");
    assert!(result.is_tool_result());
    assert_eq!(result.tool_call_id.as_deref(), Some("call-1"));
    assert_eq!(result.content.text, "fn main() {}");
    assert!(!result.is_error);
}

#[tokio::test]
async fn a_tool_the_model_reported_as_failed_is_recorded_but_the_turn_continues() {
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool_round("call-1", "read_file", json!({"path": "gone.rs"})),
        ScriptedProvider::text_round("the file is not there"),
    ]);
    let tools = ScriptedToolHost::new()
        .advertising(&["read_file"])
        .failing("read_file", ToolOutput::text("no such file: gone.rs"));
    let harness = Harness::new(
        provider,
        tools,
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    harness.prompt("read it").await;
    let events = harness.finish().await;

    assert_eq!(reason(&events), Some(StopReason::ModelDone));
    let call = finished_items(&events)
        .into_iter()
        .find(|item| item.kind_tag() == ItemKindTag::ToolCall)
        .expect("a tool call item");
    assert!(
        matches!(call.kind, ItemKind::ToolCall(ref call) if call.status == ToolStatus::Failed),
        "a tool-level failure belongs in the item status"
    );
    assert!(
        harness.provider.requests()[1]
            .messages
            .last()
            .expect("a tool result")
            .is_error,
        "and the model is told it failed"
    );
}

#[tokio::test]
async fn tool_progress_is_forwarded_while_the_tool_runs() {
    let (tools, gate) = ScriptedToolHost::new()
        .advertising(&["echo"])
        .answering("echo", ToolOutput::text("ok"))
        .gated();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool_round("call-1", "echo", json!({})),
        ScriptedProvider::text_round("done"),
    ]);
    let harness = Harness::new(
        provider,
        tools,
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    harness.prompt("echo").await;

    let events = harness
        .sink
        .wait_for("tool progress", |event| {
            matches!(event, KernelEvent::ToolCallProgress { .. })
        })
        .await;
    let chunks: Vec<&String> = events
        .iter()
        .filter_map(|event| match event {
            KernelEvent::ToolCallProgress { chunk, .. } => Some(chunk),
            _ => None,
        })
        .collect();
    assert!(
        chunks.iter().any(|chunk| chunk.contains("echo started")),
        "the tool's progress must reach the sink: {chunks:?}"
    );

    gate.release(1);
    let events = harness.finish().await;
    assert_eq!(reason(&events), Some(StopReason::ModelDone));
}

#[tokio::test]
async fn progress_sent_as_the_tool_finishes_still_reaches_the_sink() {
    // No gate, and that is the point. An ungated invocation sends its progress and returns Ready
    // within a single poll, so the kernel's biased select finds the progress channel empty, then
    // finds the invoke future ready, and takes the `Done` branch with a message still queued.
    // The gated test above cannot observe that window: releasing the gate hands the select
    // another pass, in which the progress branch wins.
    let tools = ScriptedToolHost::new()
        .advertising(&["echo"])
        .answering("echo", ToolOutput::text("ok"));
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool_round("call-1", "echo", json!({})),
        ScriptedProvider::text_round("done"),
    ]);
    let harness = Harness::new(
        provider,
        tools,
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    harness.prompt("echo").await;
    let events = harness.finish().await;

    let chunks: Vec<&String> = events
        .iter()
        .filter_map(|event| match event {
            KernelEvent::ToolCallProgress { chunk, .. } => Some(chunk),
            _ => None,
        })
        .collect();
    assert!(
        chunks.iter().any(|chunk| chunk.contains("echo started")),
        "a tool's last progress must not die with the invoke future: {chunks:?}"
    );
    assert_eq!(reason(&events), Some(StopReason::ModelDone));
}

// ------------------------------------------------------------------- approvals

#[tokio::test]
async fn an_approval_request_pauses_the_turn_until_it_is_answered() {
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool_round("call-1", "write_file", json!({"path": "a.rs"})),
        ScriptedProvider::text_round("written"),
    ]);
    let tools = ScriptedToolHost::new()
        .advertising(&["write_file"])
        .requiring_approval("write_file", RiskLevel::WritesWorkspace)
        .answering("write_file", ToolOutput::text("wrote a.rs"));
    let harness = Harness::new(
        provider,
        tools,
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    // Nothing answers yet.
    harness.prompt("write it").await;

    let events = harness
        .sink
        .wait_for("the approval request", |event| {
            matches!(event, KernelEvent::ApprovalNeeded { .. })
        })
        .await;
    let request_id = match events.last().expect("the request") {
        KernelEvent::ApprovalNeeded {
            request_id,
            request,
        } => {
            assert_eq!(request.tool, "write_file");
            assert_eq!(request.risk, RiskLevel::WritesWorkspace);
            *request_id
        }
        other => panic!("expected an approval request, got {other:?}"),
    };
    assert!(
        !harness.sink.names().contains(&"turn_ended"),
        "the turn must still be running"
    );
    assert_eq!(
        states(&events),
        vec!["assembling", "streaming", "executing", "awaiting_approval"]
    );

    // Now answer it.
    harness
        .handle
        .submit(AgentCommand::decide(
            request_id,
            hatchery_protocol::ApprovalOption::AllowOnce,
        ))
        .await
        .expect("the agent is running");
    let events = harness.finish().await;

    assert_eq!(reason(&events), Some(StopReason::ModelDone));
    assert_eq!(harness.tools.call_names(), vec!["write_file"]);
    assert_eq!(tool_result_texts(&events), vec!["wrote a.rs".to_owned()]);
    // The design's edge back (docs/design/kernel.md §3): the answer ends the wait, and the tools
    // that run afterwards are executing — a frontend projecting `SessionStatus` from the last
    // `StateChanged` must not be left showing a stale "waiting for approval".
    assert_eq!(
        states(&events),
        vec![
            "assembling",
            "streaming",
            "executing",
            "awaiting_approval",
            "executing",
            "streaming",
            "idle"
        ]
    );
}

#[tokio::test]
async fn a_denied_call_becomes_an_error_result_the_model_can_read() {
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool_round("call-1", "write_file", json!({"path": "a.rs"})),
        ScriptedProvider::text_round("then I will not"),
    ]);
    let tools = ScriptedToolHost::new()
        .advertising(&["write_file"])
        .requiring_approval("write_file", RiskLevel::WritesWorkspace);
    let harness = Harness::new(
        provider,
        tools,
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    let answers = harness.auto_approve(ScriptedApproval::Deny);
    harness.prompt("write it").await;
    let events = harness.finish().await;
    answers.abort();

    assert_eq!(
        reason(&events),
        Some(StopReason::ModelDone),
        "a refusal is an outcome, not a turn failure"
    );
    assert!(
        harness.tools.calls().is_empty(),
        "a denied tool must never run"
    );
    let texts = tool_result_texts(&events);
    assert!(
        texts[0].contains("denied"),
        "the model must be told why nothing happened: {texts:?}"
    );
    let call = finished_items(&events)
        .into_iter()
        .find(|item| item.kind_tag() == ItemKindTag::ToolCall)
        .expect("a tool call item");
    assert!(
        matches!(call.kind, ItemKind::ToolCall(ref call) if call.status == ToolStatus::Denied),
        "the item records the refusal"
    );
    assert!(
        harness.provider.requests()[1]
            .messages
            .last()
            .expect("a tool result")
            .is_error
    );
}

#[tokio::test]
async fn approving_always_still_only_runs_the_call_once() {
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool_round("call-1", "write_file", json!({"path": "a.rs"})),
        ScriptedProvider::text_round("written"),
    ]);
    let tools = ScriptedToolHost::new()
        .advertising(&["write_file"])
        .requiring_approval("write_file", RiskLevel::WritesWorkspace)
        .answering("write_file", ToolOutput::text("wrote a.rs"));
    let harness = Harness::new(
        provider,
        tools,
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    let answers = harness.auto_approve(ScriptedApproval::AllowAlways);
    harness.prompt("write it").await;
    let events = harness.finish().await;
    answers.abort();

    // Persisting the rule is the approval gate's job (M2); the kernel just runs the call.
    assert_eq!(harness.tools.call_names(), vec!["write_file"]);
    assert_eq!(reason(&events), Some(StopReason::ModelDone));
}

#[tokio::test]
async fn two_calls_in_one_round_are_each_approved_separately() {
    // A round may ask for several tools. Each gets its own request id and its own wait, and one
    // refusal must not settle the other: the model asked for two things and is owed two answers.
    fn call(index: u32, id: &str, name: &str, args: serde_json::Value) -> StreamEvent {
        StreamEvent::ToolCall {
            delta: hatchery_kernel::ToolCallDelta {
                index,
                id: Some(id.to_owned()),
                name: Some(name.to_owned()),
                args_delta: args.to_string(),
            },
        }
    }

    let provider = ScriptedProvider::new(vec![
        vec![
            call(0, "call-1", "write_file", json!({"path": "a.rs"})),
            call(1, "call-2", "shell", json!({"command": "make"})),
            StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls,
            },
        ],
        ScriptedProvider::text_round("one written, one refused"),
    ]);
    let tools = ScriptedToolHost::new()
        .advertising(&["write_file", "shell"])
        .requiring_approval("write_file", RiskLevel::WritesWorkspace)
        .requiring_approval("shell", RiskLevel::Executes)
        .answering("write_file", ToolOutput::text("wrote a.rs"));
    let harness = Harness::new(
        provider,
        tools,
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    let answers = harness.auto_approve(ScriptedApproval::Script(vec![
        hatchery_protocol::ApprovalOption::AllowOnce,
        hatchery_protocol::ApprovalOption::DenyAlways,
    ]));
    harness.prompt("write it, then build").await;
    let events = harness.finish().await;
    answers.abort();

    assert_eq!(reason(&events), Some(StopReason::ModelDone));

    let asked: Vec<hatchery_protocol::ApprovalId> = events
        .iter()
        .filter_map(|event| match event {
            KernelEvent::ApprovalNeeded { request_id, .. } => Some(*request_id),
            _ => None,
        })
        .collect();
    assert_eq!(asked.len(), 2, "one approval request per call");
    assert_ne!(
        asked[0], asked[1],
        "an answer must name the request it answers, so two calls cannot share one id"
    );

    assert_eq!(
        harness.tools.call_names(),
        vec!["write_file"],
        "the refused call never runs"
    );
    let statuses: Vec<ToolStatus> = finished_items(&events)
        .iter()
        .filter_map(|item| match item.kind {
            ItemKind::ToolCall(ref call) => Some(call.status),
            _ => None,
        })
        .collect();
    assert_eq!(
        statuses,
        vec![ToolStatus::Completed, ToolStatus::Denied],
        "each call records its own outcome, in the order the model asked"
    );
    let texts = tool_result_texts(&events);
    assert_eq!(texts.len(), 2, "both calls are answered: {texts:?}");
    assert!(
        texts[1].contains("denied"),
        "the model is told about the refusal, not left guessing: {texts:?}"
    );
    let next_round = harness.provider.requests();
    let results: Vec<&Message> = next_round[1]
        .messages
        .iter()
        .filter(|message| message.is_tool_result())
        .collect();
    assert_eq!(results.len(), 2, "both outcomes go back to the model");
    assert!(results[1].is_error, "and the refusal is marked as one");

    assert_eq!(
        states(&events),
        vec![
            "assembling",
            "streaming",
            "executing",
            "awaiting_approval",
            "executing",
            "awaiting_approval",
            "executing",
            "streaming",
            "idle"
        ],
        "each call waits, and each answer reports the machine back to executing"
    );
}

// ------------------------------------------------------------------ interrupt

#[tokio::test]
async fn interrupt_during_streaming_ends_the_turn_early_and_keeps_what_was_said() {
    let (provider, gate) = ScriptedProvider::new(vec![vec![
        StreamEvent::TextDelta {
            text: "partial ".to_owned(),
        },
        StreamEvent::TextDelta {
            text: "more".to_owned(),
        },
        StreamEvent::Done {
            finish_reason: FinishReason::Stop,
        },
    ]])
    .gated();
    let harness = Harness::new(
        provider,
        ScriptedToolHost::new(),
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    harness.prompt("hello").await;

    gate.release(1);
    harness
        .sink
        .wait_for("the first delta", |event| {
            matches!(event, KernelEvent::TextDelta { .. })
        })
        .await;
    harness
        .handle
        .interrupt()
        .await
        .expect("the agent is running");

    let events = harness.finish().await;
    assert_eq!(reason(&events), Some(StopReason::Interrupted));
    assert_eq!(
        kinds(&events),
        vec![ItemKindTag::UserMessage, ItemKindTag::AssistantMessage],
        "what the user saw is committed, so the transcript matches"
    );
    let item = finished_items(&events).pop().expect("the partial answer");
    assert!(
        matches!(item.kind, ItemKind::AssistantMessage(ref content) if content.text == "partial "),
        "only the deltas that arrived are kept"
    );
    assert_eq!(states(&events).last(), Some(&"idle"));
}

#[tokio::test]
async fn interrupt_during_tool_execution_cancels_the_tool() {
    // The gate is held but never released: the tool parks on it, and the interrupt is what ends
    // the invocation. Dropping the handle would not release the fake's own clone.
    let (tools, _gate) = ScriptedToolHost::new()
        .advertising(&["slow"])
        .answering("slow", ToolOutput::text("finished"))
        .gated();
    let provider = ScriptedProvider::new(vec![ScriptedProvider::tool_round(
        "call-1",
        "slow",
        json!({}),
    )]);
    let harness = Harness::new(
        provider,
        tools,
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    harness.prompt("run it").await;

    harness
        .sink
        .wait_for("the tool call to start", |event| {
            matches!(event, KernelEvent::ToolCallStarted { .. })
        })
        .await;
    harness
        .handle
        .interrupt()
        .await
        .expect("the agent is running");
    let events = harness.finish().await;

    assert_eq!(reason(&events), Some(StopReason::Interrupted));
    let call = finished_items(&events)
        .into_iter()
        .find(|item| item.kind_tag() == ItemKindTag::ToolCall)
        .expect("the call is recorded even though it was cut short");
    assert!(
        matches!(call.kind, ItemKind::ToolCall(ref call) if call.status == ToolStatus::Cancelled),
        "an interrupted call is cancelled, not failed"
    );
    // The seam contract, observed from the host side: the invocation the kernel dropped saw a
    // cancelled token. The record is written on entry and the verdict by the fake's drop guard,
    // because a kernel that never polls the future again is exactly what cancellation means.
    let recorded = harness.tools.calls();
    assert_eq!(recorded.len(), 1, "the tool was invoked exactly once");
    assert!(
        recorded[0].cancelled,
        "the host must see the cancellation, not just the kernel's bookkeeping"
    );
}

#[tokio::test]
async fn interrupt_while_awaiting_approval_ends_the_turn() {
    let provider = ScriptedProvider::new(vec![ScriptedProvider::tool_round(
        "call-1",
        "write_file",
        json!({"path": "a.rs"}),
    )]);
    let tools = ScriptedToolHost::new()
        .advertising(&["write_file"])
        .requiring_approval("write_file", RiskLevel::WritesWorkspace);
    let harness = Harness::new(
        provider,
        tools,
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    harness.prompt("write it").await;

    harness
        .sink
        .wait_for("the approval request", |event| {
            matches!(event, KernelEvent::ApprovalNeeded { .. })
        })
        .await;
    harness
        .handle
        .interrupt()
        .await
        .expect("the agent is running");
    let events = harness.finish().await;

    assert_eq!(reason(&events), Some(StopReason::Interrupted));
    assert!(harness.tools.calls().is_empty(), "nothing was run");
    assert_eq!(
        states(&events),
        vec![
            "assembling",
            "streaming",
            "executing",
            "awaiting_approval",
            "idle"
        ]
    );
}

#[tokio::test]
async fn an_interrupt_with_no_turn_running_is_ignored() {
    let harness = Harness::new(
        ScriptedProvider::new(vec![ScriptedProvider::text_round("still here")]),
        ScriptedToolHost::new(),
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    harness
        .handle
        .interrupt()
        .await
        .expect("the agent is running");
    // The interrupt above resolves without suspending (the command channel has capacity), so on
    // a current-thread runtime the agent task has not been polled yet — asserting right here
    // would prove nothing, which is what made this test vacuous. Driving a real turn through the
    // same FIFO channel forces the idle interrupt to be handled first: a panic or a wedge in
    // that path now surfaces as a failed or timed-out turn instead of passing silently.
    harness.prompt("hello").await;
    let events = harness.finish().await;

    assert_eq!(
        reason(&events),
        Some(StopReason::ModelDone),
        "the agent is still usable after an ignored interrupt"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, KernelEvent::TurnStarted { .. }))
            .count(),
        1,
        "the ignored interrupt must not start a turn of its own"
    );
    assert!(!harness.handle.is_closed());
}

// ---------------------------------------------------------- turn discipline

#[tokio::test]
async fn a_mid_turn_prompt_is_dropped_not_queued() {
    // One turn at a time: the daemon rejects a second prompt with `TurnInProgress`, so a
    // `TurnInput` that reaches the kernel mid-turn is a caller bug — warned about and dropped,
    // never queued into a second turn (queueing would invent one).
    let (provider, gate) = ScriptedProvider::new(vec![vec![
        StreamEvent::TextDelta {
            text: "first answer".to_owned(),
        },
        StreamEvent::Done {
            finish_reason: FinishReason::Stop,
        },
    ]])
    .gated();
    let harness = Harness::new(
        provider,
        ScriptedToolHost::new(),
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    harness.prompt("first").await;
    gate.release(1);
    harness
        .sink
        .wait_for("the first delta", |event| {
            matches!(event, KernelEvent::TextDelta { .. })
        })
        .await;

    // Mid-stream: a second prompt arrives while the turn is still running.
    harness.prompt("second").await;
    gate.release(1);
    let events = harness.finish().await;

    assert_eq!(reason(&events), Some(StopReason::ModelDone));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, KernelEvent::TurnStarted { .. }))
            .count(),
        1,
        "the mid-turn prompt must not start a second turn"
    );
    let user_messages: Vec<Item> = finished_items(&events)
        .into_iter()
        .filter(|item| item.kind_tag() == ItemKindTag::UserMessage)
        .collect();
    assert_eq!(
        user_messages.len(),
        1,
        "only the running turn's prompt was recorded"
    );
    assert!(
        matches!(user_messages[0].kind, ItemKind::UserMessage(ref content) if content.text == "first"),
        "the dropped prompt never became an item"
    );
}

#[tokio::test]
async fn a_second_turn_chains_onto_the_first_turns_head() {
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::text_round("first answer"),
        ScriptedProvider::text_round("second answer"),
    ]);
    let harness = Harness::new(
        provider,
        ScriptedToolHost::new(),
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    harness.prompt("one").await;
    let after_one = harness.finish().await;
    let boundary = after_one.len();
    let committed = finished_items(&after_one);
    let head = committed.last().expect("the first turn committed items").id;

    // Play the daemon: what the sink committed is what the next turn's history sees. Without
    // this the kernel would re-read an empty head and the second turn would chain onto nothing.
    harness.history.push_item(Message::user("one"), head);

    harness.prompt("two").await;
    // From the boundary: `finish()` scans from the beginning and would match turn one's
    // `TurnEnded` again, returning a stale snapshot before turn two ever ran.
    let all = harness.sink.wait_for_end_from(boundary).await;
    let second = &all[boundary..];

    assert_eq!(reason(second), Some(StopReason::ModelDone));
    let user_two = finished_items(second)
        .into_iter()
        .find(|item| item.kind_tag() == ItemKindTag::UserMessage)
        .expect("the second turn records its prompt");
    assert_eq!(
        user_two.parent,
        Some(head),
        "turn two chains onto turn one's committed head"
    );
    assert_eq!(
        all.iter()
            .filter(|event| matches!(event, KernelEvent::TurnEnded { .. }))
            .count(),
        2,
        "each turn ends exactly once"
    );
}

#[tokio::test]
async fn the_documented_event_sequence_is_emitted_exactly() {
    // The full literal sequence, not a subsequence: `ItemStarted` and the delta events are what
    // a frontend renders in-flight items from, and no other test pinned their presence, order
    // or count.
    let provider = ScriptedProvider::new(vec![ScriptedProvider::text_round("hello back")]);
    let harness = Harness::new(
        provider,
        ScriptedToolHost::new(),
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    harness.prompt("hello").await;
    harness.finish().await;
    assert_eq!(
        harness.sink.names(),
        vec![
            "turn_started",
            "state_changed", // idle -> assembling
            "item_finished", // the user message is committed whole, never streamed
            "state_changed", // assembling -> streaming
            "item_started",  // the answer opens on its first delta
            "text_delta",
            "item_finished",
            "state_changed", // streaming -> idle
            "turn_ended",
        ]
    );

    // Reasoning then text: the reasoning item closes when the text starts, so the sequence
    // shows the close-and-reopen that keeps the item tree a chain.
    let provider = ScriptedProvider::new(vec![vec![
        StreamEvent::ReasoningDelta {
            text: "thinking".to_owned(),
        },
        StreamEvent::TextDelta {
            text: "answering".to_owned(),
        },
        StreamEvent::Done {
            finish_reason: FinishReason::Stop,
        },
    ]]);
    let harness = Harness::new(
        provider,
        ScriptedToolHost::new(),
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    harness.prompt("hello").await;
    harness.finish().await;
    assert_eq!(
        harness.sink.names(),
        vec![
            "turn_started",
            "state_changed",
            "item_finished",
            "state_changed",
            "item_started", // the reasoning item opens
            "reasoning_delta",
            "item_finished", // text closes it …
            "item_started",  // … and opens the answer
            "text_delta",
            "item_finished",
            "state_changed",
            "turn_ended",
        ]
    );
}

// --------------------------------------------------------------------- fuses

#[tokio::test]
async fn max_rounds_fuse_trips_at_limit() {
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool_round("call-1", "echo", json!({})),
        ScriptedProvider::tool_round("call-2", "echo", json!({})),
        ScriptedProvider::text_round("never reached"),
    ]);
    let tools = ScriptedToolHost::new()
        .advertising(&["echo"])
        .answering("echo", ToolOutput::text("ok"));
    let harness = Harness::new(
        provider,
        tools,
        MemoryHistory::empty(),
        TurnLimits::with_max_rounds(2),
    );
    harness.prompt("loop").await;
    let events = harness.finish().await;

    assert_eq!(reason(&events), Some(StopReason::MaxRounds));
    assert_eq!(
        harness.tools.call_names(),
        vec!["echo", "echo"],
        "the fuse stops the loop at the limit"
    );
    assert_eq!(
        harness.provider.requests().len(),
        2,
        "the third scripted round is never requested"
    );
}

// ------------------------------------------------------------------- failures

#[tokio::test]
async fn a_provider_that_cannot_start_fails_the_turn() {
    let provider = ScriptedProvider::new(vec![]);
    let harness = Harness::new(
        provider,
        ScriptedToolHost::new(),
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    harness.prompt("hello").await;
    let events = harness.finish().await;

    match error(&events).expect("the turn failed") {
        KernelError::Provider(error) => assert!(
            !error.retryable,
            "a provider that cannot start at all is not worth retrying blindly: {error}"
        ),
        other => panic!("expected a provider failure, got {other:?}"),
    }
    assert_eq!(
        kinds(&events),
        vec![ItemKindTag::UserMessage],
        "the question is still recorded"
    );
}

#[tokio::test]
async fn a_stream_that_breaks_mid_round_keeps_the_partial_answer_and_fails() {
    let provider = ScriptedProvider::new(vec![vec![
        StreamEvent::TextDelta {
            text: "half an answer".to_owned(),
        },
        StreamEvent::Error {
            error: LlmError::retryable("connection reset by peer"),
        },
    ]]);
    let harness = Harness::new(
        provider,
        ScriptedToolHost::new(),
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    harness.prompt("hello").await;
    let events = harness.finish().await;

    let failure = error(&events).expect("the turn failed");
    assert_eq!(failure.to_event_error().retryable, Some(true));
    assert_eq!(
        kinds(&events),
        vec![ItemKindTag::UserMessage, ItemKindTag::AssistantMessage]
    );
}

#[tokio::test]
async fn a_stream_without_a_finish_reason_is_a_retryable_failure() {
    let provider = ScriptedProvider::new(vec![vec![StreamEvent::TextDelta {
        text: "cut off".to_owned(),
    }]]);
    let harness = Harness::new(
        provider,
        ScriptedToolHost::new(),
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    harness.prompt("hello").await;
    let events = harness.finish().await;

    let failure = error(&events).expect("the turn failed");
    assert_eq!(failure.to_event_error().retryable, Some(true));
}

#[tokio::test]
async fn a_tool_that_cannot_be_invoked_fails_the_turn() {
    let provider = ScriptedProvider::new(vec![ScriptedProvider::tool_round(
        "call-1",
        "shell",
        json!({"command": "ls"}),
    )]);
    let tools = ScriptedToolHost::new()
        .advertising(&["shell"])
        .erroring("shell", "the terminal backend is gone");
    let harness = Harness::new(
        provider,
        tools,
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    harness.prompt("run it").await;
    let events = harness.finish().await;

    match error(&events).expect("the turn failed") {
        KernelError::Tool { name, message } => {
            assert_eq!(name, "shell");
            assert!(message.contains("gone"));
        }
        other => panic!("expected a tool failure, got {other:?}"),
    }
}

// --------------------------------------------------------------- snapshots

#[tokio::test]
async fn the_tool_snapshot_is_frozen_for_the_whole_turn() {
    let (tools, gate) = ScriptedToolHost::new()
        .advertising(&["read_file"])
        .answering("read_file", ToolOutput::text("contents"))
        .gated();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool_round("call-1", "read_file", json!({"path": "a.rs"})),
        ScriptedProvider::text_round("done"),
    ]);
    let harness = Harness::new(
        provider,
        tools,
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    harness.prompt("read it").await;

    // While the tool is running, the registry is swapped — as a mode switch would.
    harness
        .sink
        .wait_for("the tool call to start", |event| {
            matches!(event, KernelEvent::ToolCallStarted { .. })
        })
        .await;
    harness.tools.set_defs(&["write_file", "shell"]);
    gate.release(1);
    let events = harness.finish().await;

    assert_eq!(reason(&events), Some(StopReason::ModelDone));
    let requests = harness.provider.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        let names: Vec<&str> = request
            .options
            .tool_defs
            .iter()
            .map(|def| def.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["read_file"],
            "the turn keeps the catalogue it started with"
        );
    }
}

// -------------------------------------------------------------- determinism

#[tokio::test]
async fn the_same_script_produces_the_same_event_sequence_twice() {
    let script = || {
        vec![
            ScriptedProvider::tool_round("call-1", "echo", json!({"n": 1})),
            ScriptedProvider::text_round("done"),
        ]
    };

    let first = {
        let tools = ScriptedToolHost::new()
            .advertising(&["echo"])
            .answering("echo", ToolOutput::text("ok"));
        let harness = Harness::new(
            ScriptedProvider::new(script()),
            tools,
            MemoryHistory::empty(),
            TurnLimits::default(),
        );
        harness.prompt("go").await;
        harness.finish().await
    };

    let second = {
        let tools = ScriptedToolHost::new()
            .advertising(&["echo"])
            .answering("echo", ToolOutput::text("ok"));
        let harness = Harness::new(
            ScriptedProvider::new(script()),
            tools,
            MemoryHistory::empty(),
            TurnLimits::default(),
        );
        harness.prompt("go").await;
        harness.finish().await
    };

    let names = |events: &[KernelEvent]| -> Vec<&'static str> {
        events.iter().map(KernelEvent::name).collect()
    };
    assert_eq!(
        names(&first),
        names(&second),
        "the same script must produce the same events in the same order"
    );
    assert_eq!(kinds(&first), kinds(&second));
    assert_eq!(states(&first), states(&second));
}

#[test]
fn control_events_are_distinguishable_from_deltas() {
    // The daemon's hub needs this to know what may be coalesced (docs/design/testing.md §3.6).
    let delta = KernelEvent::TextDelta {
        item: ItemId::new(),
        text: "hi".to_owned(),
    };
    let control = KernelEvent::StateChanged {
        from: TurnState::Idle,
        to: TurnState::Assembling,
    };
    assert!(!delta.is_control(), "a text delta may be coalesced");
    assert!(control.is_control(), "a state change may never be");
    assert!(
        KernelEvent::TurnEnded {
            turn: hatchery_protocol::TurnId::new(),
            completion: TurnCompletion::Completed {
                reason: StopReason::ModelDone,
                usage: hatchery_protocol::Usage::default(),
            },
        }
        .ends_turn()
    );
}

// ------------------------------------------------------------- turn-busy signal

#[tokio::test]
async fn turn_running_tracks_the_state_machine() {
    // The daemon's second-prompt refusal and the idle sweep both read this flag, so it must be
    // the state machine's own view: false before, true the moment the turn opens, false again
    // once it ends.
    let (provider, gate) =
        ScriptedProvider::new(vec![ScriptedProvider::text_round("slow")]).gated();
    let harness = Harness::new(
        provider,
        ScriptedToolHost::new(),
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    assert!(!harness.handle.turn_running(), "idle at birth");

    harness.prompt("hello").await;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !harness.handle.turn_running() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the turn opened");

    // One permit per scripted event: the delta and the Done.
    gate.release(2);
    harness.finish().await;
    assert!(
        !harness.handle.turn_running(),
        "a finished turn is not running"
    );
}

// ------------------------------------------------------------ control surfaces

#[tokio::test]
async fn a_rate_limited_notice_is_forwarded_to_the_sink() {
    // The adapter emits this while working through its backoff; the kernel must forward it
    // untouched (docs/design/llm.md §6) — a frontend's "hold on, retrying" countdown is built
    // on it, and a rename or a dropped publish would silence that silently.
    let provider = ScriptedProvider::new(vec![vec![
        StreamEvent::RateLimited {
            retry_after_ms: 250,
        },
        StreamEvent::TextDelta {
            text: "eventually".to_owned(),
        },
        StreamEvent::Done {
            finish_reason: FinishReason::Stop,
        },
    ]]);
    let harness = Harness::new(
        provider,
        ScriptedToolHost::new(),
        MemoryHistory::empty(),
        TurnLimits::default(),
    );
    harness.prompt("go").await;
    let events = harness.finish().await;

    assert!(
        events.iter().any(|event| matches!(
            event,
            KernelEvent::RateLimited {
                retry_after_ms: 250
            }
        )),
        "the notice reaches the sink verbatim, among: {events:?}"
    );
    assert_eq!(
        reason(&events),
        Some(StopReason::ModelDone),
        "a rate-limit notice is informational; it must not end the turn"
    );
}

#[tokio::test]
async fn a_fuse_already_at_its_limit_ends_the_turn_without_a_provider_call() {
    // `max_rounds` of zero trips the fuse before the first round. The turn still ends through
    // the normal path — a terminal event with the fuse's own reason — rather than hanging or
    // skipping the turn bookkeeping.
    let harness = Harness::new(
        ScriptedProvider::new(vec![]),
        ScriptedToolHost::new(),
        MemoryHistory::empty(),
        TurnLimits::with_max_rounds(0),
    );
    harness.prompt("hello").await;
    let events = harness.finish().await;

    assert_eq!(reason(&events), Some(StopReason::MaxRounds));
    assert!(
        harness.provider.requests().is_empty(),
        "a tripped fuse never reaches the provider"
    );
    assert_eq!(
        kinds(&events),
        vec![ItemKindTag::UserMessage],
        "the user's message is recorded even though nothing answered it"
    );
}

#[tokio::test]
async fn dropping_every_handle_mid_turn_still_ends_the_turn() {
    // The daemon's unload path drops the last handle; the contract promises a terminal event
    // even then — the turn is cancelled through the command channel closing, never by losing
    // the future.
    let (provider, gate) =
        ScriptedProvider::new(vec![ScriptedProvider::text_round("slow")]).gated();
    let sink = Arc::new(RecordingSink::new());
    let (agent, handle) = AgentBuilder::new(
        SessionId::new(),
        ChatOptions::new("scripted-model"),
        Ports::new(
            Arc::new(provider) as Arc<dyn hatchery_kernel::LlmProvider>,
            Arc::new(ScriptedToolHost::new()) as Arc<dyn hatchery_kernel::ToolHost>,
            Arc::new(MemoryHistory::empty()) as Arc<dyn hatchery_kernel::HistorySource>,
            Arc::clone(&sink) as Arc<dyn hatchery_kernel::EventSink>,
        ),
    )
    .build();
    let running = tokio::spawn(Agent::run(agent));

    handle
        .submit(AgentCommand::prompt("hello"))
        .await
        .expect("submitted");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !handle.turn_running() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the turn opened");

    drop(handle);
    let events = sink.wait_for_end().await;
    assert_eq!(
        reason(&events),
        Some(StopReason::Interrupted),
        "a dropped handle interrupts, it does not abandon"
    );
    gate.release(2);
    tokio::time::timeout(std::time::Duration::from_secs(2), running)
        .await
        .expect("the loop exits")
        .expect("the loop exits cleanly");
}
