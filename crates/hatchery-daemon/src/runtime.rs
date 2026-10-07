//! One session's runtime: the kernel agent and everything it talks to, assembled.
//!
//! Three pieces live here:
//!
//! * [`StoreHistory`] — the `HistorySource`: rebuilds the provider conversation from the store's
//!   active branch, filtering reasoning per the provider's capability table (the daemon-side
//!   assembly of kernel.md §6).
//! * [`HubSink`] — the `EventSink`: projects kernel events onto the wire and the store, committing
//!   each item *before* its `ItemFinished` reaches the hub (invariant 2: model-visible means
//!   logged, and in that order).
//! * [`SessionRuntime`] — the assembled pair plus the running agent, with its generation.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;

use hatchery_kernel::{
    ChatOptions, EventSink, HistorySource, HistoryView, KernelError, KernelEvent, LlmProvider,
    Message, Ports, Role, ToolCallRequest, ToolHost,
};
use hatchery_protocol::{
    Content, EventError, ItemId, ItemKind, ReasoningBlock, ServerEvent, Session, SessionEvent,
    SessionId, SessionStatus,
};
use hatchery_store::SessionStore;

use crate::hub::LiveHub;
use crate::prompt::AssembledSection;

/// The conversation as the provider will see it, rebuilt from the store.
///
/// The mapping is deliberately mechanical — the branch history must be rebuildable byte for byte
/// from what is recorded (invariant 2), so assembly adds nothing to it that the store does not
/// hold:
///
/// * `UserMessage` → a user message.
/// * `Reasoning` → held back, attached to the following assistant message when the provider's
///   capability table says reasoning echoes; dropped otherwise.
/// * `AssistantMessage` → an assistant message; `ToolCall` items that follow attach to it.
/// * `ToolCall` → a `ToolCallRequest` on that message, id = the recorded provider id (rows from
///   before the field existed get a stable synthesized stand-in).
/// * `ToolResult` → a tool message, `tool_call_id` = the answered call's provider id.
/// * `Checkpoint` / `ModeSwitch` / `BranchNote` / `Compaction` → not provider-visible; skipped.
///
/// The one thing in the request that is *not* an item leads it: the system prompt. It is a
/// derived artifact — reproducible from the shipped templates plus the runtime facts frozen when
/// this runtime was assembled — so invariant 2's byte-exactness covers the branch history behind
/// it, and `prompt/render` covers the prompt itself (docs/design/kernel.md §6, D15).
pub struct StoreHistory {
    store: Arc<dyn SessionStore>,
    session: SessionId,
    /// Whether reasoning rides back into requests on this provider (ADR-0007).
    echo_reasoning: bool,
    /// The rendered prompt, frozen for this runtime's life; `None` sends no system message.
    system: Option<String>,
}

impl StoreHistory {
    /// A history for one session, with the provider's echo setting and the prompt to lead with.
    #[must_use]
    pub fn new(
        store: Arc<dyn SessionStore>,
        session: SessionId,
        echo_reasoning: bool,
        system: Option<String>,
    ) -> Self {
        Self {
            store,
            session,
            echo_reasoning,
            system,
        }
    }
}

#[async_trait]
impl HistorySource for StoreHistory {
    async fn view(&self) -> Result<HistoryView, KernelError> {
        let session = self
            .store
            .session(self.session)
            .await
            .map_err(|error| KernelError::history(error.to_string()))?;
        let items = self
            .store
            .rebuild_chain(self.session, None)
            .await
            .map_err(|error| KernelError::history(error.to_string()))?;

        let mut messages = Vec::new();
        if let Some(system) = &self.system {
            messages.push(Message::system(system.clone()));
        }
        let mut pending_reasoning: Option<ReasoningBlock> = None;
        // item id → provider call id, so results can be paired after the fact.
        let mut call_ids: std::collections::HashMap<ItemId, String> =
            std::collections::HashMap::new();
        // Synthesized stand-ins for legacy rows; counted separately so two calls on one
        // assistant message cannot collide.
        let mut synthesized = 0_usize;

        for item in items {
            match item.kind {
                ItemKind::UserMessage(content) => messages.push(Message::user(content)),
                ItemKind::Reasoning(block) => {
                    // Byte-exact or nothing (ADR-0007): the trim that would "tidy" this is the
                    // trim that breaks the provider's cache.
                    if self.echo_reasoning {
                        pending_reasoning = Some(block);
                    }
                }
                ItemKind::AssistantMessage(content) => {
                    let mut message = Message::assistant(content);
                    if let Some(reasoning) = pending_reasoning.take() {
                        message = message.with_reasoning(reasoning);
                    }
                    messages.push(message);
                }
                ItemKind::ToolCall(call) => {
                    synthesized += 1;
                    let provider_id = call
                        .provider_call_id
                        .clone()
                        .unwrap_or_else(|| format!("call-synth-{synthesized}"));
                    call_ids.insert(item.id, provider_id.clone());
                    let request = ToolCallRequest {
                        id: provider_id,
                        name: call.name,
                        args: call.args,
                    };
                    match messages.last_mut() {
                        Some(message) if message.role == Role::Assistant => {
                            message.tool_calls.push(request);
                        }
                        _ => {
                            // A call without its assistant message cannot be paired; the store
                            // recorded it, so the model sees the anomaly instead of silence.
                            messages.push(
                                Message::assistant(Content::text(String::new()))
                                    .with_tool_calls(vec![request]),
                            );
                        }
                    }
                }
                ItemKind::ToolResult(result) => {
                    let call_id = call_ids.get(&result.call).cloned().unwrap_or_default();
                    messages.push(Message::tool_result(
                        call_id,
                        Content::text(result.output.text),
                        result.is_error,
                    ));
                }
                ItemKind::Checkpoint(_)
                | ItemKind::ModeSwitch(_)
                | ItemKind::BranchNote(_)
                | ItemKind::Compaction(_) => {}
            }
        }

        Ok(HistoryView {
            head: session.active_branch_head,
            messages,
        })
    }
}

/// Projects kernel events onto the hub and the store.
///
/// The daemon is the layer that knows the session id and generation, so this is where the
/// envelope is attached — the kernel's events carry neither (kernel.md §7).
pub struct HubSink {
    session: SessionId,
    generation: u64,
    hub: Arc<LiveHub>,
    store: Arc<dyn SessionStore>,
    /// Cleared when the sink sees a turn end: the manager marks a turn in flight the moment it
    /// submits, which is earlier than the kernel's own state machine can say so, and this is
    /// what hands the truth back.
    in_flight: Arc<AtomicBool>,
}

impl HubSink {
    /// A sink bound to one runtime.
    #[must_use]
    pub fn new(
        session: SessionId,
        generation: u64,
        hub: Arc<LiveHub>,
        store: Arc<dyn SessionStore>,
        in_flight: Arc<AtomicBool>,
    ) -> Self {
        Self {
            session,
            generation,
            hub,
            store,
            in_flight,
        }
    }

    fn publish(&self, event: ServerEvent) {
        self.hub
            .publish(SessionEvent::new(self.session, self.generation, event));
    }

    /// The session status a kernel state maps to.
    fn status(state: &hatchery_kernel::TurnState) -> SessionStatus {
        use hatchery_kernel::TurnState;
        match state {
            TurnState::Idle => SessionStatus::Idle,
            TurnState::AwaitingApproval { .. } => SessionStatus::WaitingApproval,
            _ => SessionStatus::Running,
        }
    }
}

#[async_trait]
impl EventSink for HubSink {
    async fn emit(&self, event: KernelEvent) {
        match event {
            KernelEvent::StateChanged { to, .. } => {
                // Every transition is published so the frontend projects SessionStatus without
                // inferring it — including the ones that land back on Idle.
                if let Ok(session) = self
                    .store
                    .update_session(
                        self.session,
                        hatchery_protocol::SessionPatch {
                            status: Some(Self::status(&to)),
                            ..SessionPatch::default()
                        },
                    )
                    .await
                {
                    self.publish(ServerEvent::SessionUpdated { state: session });
                } else {
                    // The row stays stale on the failure; a frontend still hears the state
                    // change as deltas, and recovery straightens the row at the next restart.
                    tracing::error!(session = %self.session, "failed to persist a state change");
                }
            }
            KernelEvent::ItemStarted { item } => self.publish(ServerEvent::ItemStarted { item }),
            KernelEvent::TextDelta { item, text } => {
                self.publish(ServerEvent::TextDelta { item, text });
            }
            KernelEvent::ReasoningDelta { item, text } => {
                self.publish(ServerEvent::ReasoningDelta { item, text });
            }
            KernelEvent::ItemFinished { item } => {
                // Commit, then publish: a subscriber that sees ItemFinished can immediately ask
                // for the session and must find the row (invariant 2, in that order). When the
                // commit fails, publishing anyway would show the subscriber an item the store
                // does not have — the exact divergence the ordering exists to prevent — so the
                // event is held back with the error.
                if let Err(error) = self.store.append_item(item.clone()).await {
                    tracing::error!(
                        session = %self.session,
                        "failed to commit an item; its ItemFinished is withheld: {error}"
                    );
                    return;
                }
                self.publish(ServerEvent::ItemFinished { item });
            }
            KernelEvent::ToolCallStarted { item, summary } => {
                self.publish(ServerEvent::ToolCallStarted { item, summary });
            }
            KernelEvent::ToolCallProgress { item, chunk } => {
                self.publish(ServerEvent::ToolCallProgress { item, chunk });
            }
            KernelEvent::RateLimited { retry_after_ms } => {
                self.publish(ServerEvent::RateLimited { retry_after_ms });
            }
            KernelEvent::ApprovalNeeded {
                request_id,
                request,
            } => {
                self.publish(ServerEvent::ApprovalRequested {
                    request_id,
                    request,
                });
            }
            KernelEvent::TurnStarted { turn } => {
                if let Err(error) = self
                    .store
                    .start_turn(self.session, turn, hatchery_protocol::Timestamp::now())
                    .await
                {
                    tracing::error!(session = %self.session, "failed to record the turn: {error}");
                }
            }
            KernelEvent::TurnEnded { turn, completion } => {
                // However the turn ended, the manager's early marker is released here: this is
                // the one event every turn produces, so it is the one place the flag clears.
                self.in_flight.store(false, Ordering::Release);
                let recorded = completion.is_ok().then(|| {
                    hatchery_protocol::TurnCompletion::new(
                        completion
                            .stop_reason()
                            .unwrap_or(hatchery_protocol::StopReason::ModelDone),
                    )
                    .with_usage(completion.usage())
                });
                if let Err(error) = self.store.finish_turn(self.session, turn, recorded).await {
                    tracing::error!(session = %self.session, "failed to close the turn: {error}");
                }
                if let Some(prot) = recorded {
                    self.publish(ServerEvent::TurnFinished {
                        turn,
                        completion: prot,
                    });
                } else {
                    self.publish(ServerEvent::TurnFailed {
                        turn,
                        error: completion_error(&completion),
                    });
                }
            }
        }
    }
}

use hatchery_protocol::SessionPatch;

/// The kernel's failed completion, in the shape the wire carries.
fn completion_error(completion: &hatchery_kernel::TurnCompletion) -> EventError {
    let hatchery_kernel::TurnCompletion::Failed { error, .. } = completion else {
        return EventError::new(hatchery_protocol::ErrorCode::InternalError, "not a failure");
    };
    error.to_event_error()
}

/// The seams one assembly binds into a runtime.
///
/// Named fields rather than positional arguments: most of these are `Arc`s of the same shape, and
/// transposing two of them at a call site is a mistake the compiler does not always catch.
pub struct RuntimeParts {
    /// Where items are committed and history is read from.
    pub store: Arc<dyn SessionStore>,
    /// The fan-out this runtime's events are published to.
    pub hub: Arc<LiveHub>,
    /// The provider adapter serving the session's model.
    pub provider: Arc<dyn LlmProvider>,
    /// The tool catalogue and dispatcher for the session's mode.
    pub tools: Arc<dyn ToolHost>,
    /// Whether reasoning rides back into requests on this provider (ADR-0007).
    pub echo_reasoning: bool,
    /// The prompt rendered for this assembly, frozen into the runtime (D15).
    pub prompt: Vec<AssembledSection>,
}

/// One assembled, running session.
pub struct SessionRuntime {
    /// The session this runtime owns.
    pub session: SessionId,
    /// Which assembly this is; every event this runtime emits carries it (invariant 1).
    pub generation: u64,
    /// The prompt this runtime was assembled with, section by section.
    ///
    /// Frozen for the runtime's life (D15) and kept in sections so `prompt/render` can answer
    /// "why did the model see this" with the text the requests actually carry, sources included.
    pub prompt: Vec<AssembledSection>,
    /// The command end of the kernel agent.
    pub handle: hatchery_kernel::AgentHandle,
    /// Set the moment the manager accepts a prompt, cleared by the sink's turn-end projection.
    ///
    /// The kernel's own state machine only opens once the turn starts running, which is after
    /// the submit has already succeeded — the gap between the two is exactly where a second
    /// prompt would slip through a plain state check, so acceptance is marked at the source.
    in_flight: Arc<AtomicBool>,
    /// The agent task; aborted on unload.
    task: tokio::task::JoinHandle<()>,
}

impl SessionRuntime {
    /// Assembles and starts a runtime.
    ///
    /// The provider/tool/history/sink quadruple is bound here, once per generation — this is
    /// what "the runtime owns its seams" means in practice.
    ///
    /// # Errors
    ///
    /// Propagates the store's refusal to record the new generation.
    pub async fn spawn(
        session: &Session,
        generation: u64,
        parts: RuntimeParts,
    ) -> Result<Self, hatchery_store::StoreError> {
        let RuntimeParts {
            store,
            hub,
            provider,
            tools,
            echo_reasoning,
            prompt,
        } = parts;
        let in_flight = Arc::new(AtomicBool::new(false));
        let ports = Ports::new(
            provider,
            tools,
            Arc::new(StoreHistory::new(
                Arc::clone(&store),
                session.id,
                echo_reasoning,
                Some(crate::prompt::join(&prompt)),
            )),
            Arc::new(HubSink::new(
                session.id,
                generation,
                hub,
                store,
                Arc::clone(&in_flight),
            )),
        );
        let options = ChatOptions::new(session.model.model.clone());
        let (agent, handle) =
            hatchery_kernel::AgentBuilder::new(session.id, options, ports).build();
        let task = tokio::spawn(agent.run());
        Ok(Self {
            session: session.id,
            generation,
            prompt,
            handle,
            in_flight,
            task,
        })
    }

    /// Marks a turn accepted, refusing when one already is.
    ///
    /// `false` means another prompt got there first; the caller answers the protocol's
    /// `TurnInProgress`. The flag clears when the sink projects the turn's end.
    pub fn begin_turn(&self) -> bool {
        !self.in_flight.swap(true, Ordering::AcqRel)
    }

    /// Releases the in-flight marker after a submit that never landed: no turn will run to
    /// clear it, and a wedged marker would refuse every later prompt.
    pub fn end_turn(&self) {
        self.in_flight.store(false, Ordering::Release);
    }

    /// True while a turn is in flight: from the moment a prompt is accepted until the kernel's
    /// terminal event has been projected — the union of the manager's marker and the kernel
    /// state machine's own view, so the second-prompt refusal and the idle sweep cannot see
    /// opposite truths.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.in_flight.load(Ordering::Acquire) || self.handle.turn_running()
    }

    /// Stops the agent. The command channel closes with the task, so a frontend holding the
    /// handle learns of the unload on its next submit.
    pub fn shutdown(&self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hatchery_protocol::{
        Item, ModelRef, SessionModeId, Timestamp, ToolCall, ToolOutput, ToolResult, ToolStatus,
    };

    async fn store() -> (tempfile::TempDir, Arc<dyn SessionStore>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store: Arc<dyn SessionStore> = Arc::new(
            hatchery_store::TursoStore::open(dir.path().join("test.db"))
                .await
                .expect("open"),
        );
        (dir, store)
    }

    fn session_row() -> Session {
        Session {
            id: SessionId::new(),
            title: None,
            mode: SessionModeId::chat(),
            workspace: None,
            model: ModelRef::new("deepseek", "deepseek-flash"),
            config_patch: None,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
            active_branch_head: None,
            generation: 0,
            status: SessionStatus::Idle,
        }
    }

    #[allow(unused_variables)]
    async fn store_with_items(
        echo_reasoning: bool,
    ) -> (tempfile::TempDir, Arc<dyn SessionStore>, SessionId) {
        let (dir, store) = store().await;
        let session = session_row();
        store.create_session(session.clone()).await.expect("create");

        let mut parent: Option<ItemId> = None;
        let mut items = Vec::new();
        let mut push = |kind: ItemKind, parent: &mut Option<ItemId>| {
            let mut item = Item::new(session.id, kind);
            item.parent = *parent;
            *parent = Some(item.id);
            items.push(item);
        };
        push(
            ItemKind::UserMessage(Content::text("what is two plus two?")),
            &mut parent,
        );
        push(
            ItemKind::Reasoning(ReasoningBlock::text(" arithmetic: 2+2 = 4 ")),
            &mut parent,
        );
        let call_item_id = ItemId::new();
        let mut assistant = Item::new(
            session.id,
            ItemKind::AssistantMessage(Content::text("Let me check.")),
        );
        assistant.parent = parent;
        parent = Some(assistant.id);
        items.push(assistant);
        let mut tool_call = Item::with_id(
            call_item_id,
            session.id,
            ItemKind::ToolCall(ToolCall {
                name: "read_file".to_owned(),
                args: serde_json::json!({"path": "a.txt"}),
                status: ToolStatus::Completed,
                provider_call_id: Some("call_00_live".to_owned()),
            }),
        );
        tool_call.parent = parent;
        parent = Some(tool_call.id);
        items.push(tool_call);
        let mut result = Item::new(
            session.id,
            ItemKind::ToolResult(ToolResult {
                call: call_item_id,
                output: ToolOutput::text("4"),
                is_error: false,
            }),
        );
        result.parent = parent;
        parent = Some(result.id);
        items.push(result);
        let mut final_msg = Item::new(
            session.id,
            ItemKind::AssistantMessage(Content::text("Two plus two is 4.")),
        );
        final_msg.parent = parent;
        parent = Some(final_msg.id);
        items.push(final_msg);
        let mut checkpoint = Item::new(
            session.id,
            ItemKind::Checkpoint(hatchery_protocol::Checkpoint {
                commit_id: "abc".to_owned(),
                kind: hatchery_protocol::CheckpointKind::PreWrite,
            }),
        );
        checkpoint.parent = parent;
        items.push(checkpoint);

        store.append_items(items).await.expect("append");
        (dir, store, session.id)
    }

    #[tokio::test]
    async fn the_chain_rebuilds_the_provider_request_exactly() {
        let (_dir, store, session) = store_with_items(true).await;
        let history = StoreHistory::new(store, session, true, None);
        let view = history.view().await.expect("view");

        assert_eq!(
            view.messages.len(),
            4,
            "user, assistant(+call), tool, assistant"
        );
        assert_eq!(view.messages[0].role, Role::User);
        let assistant = &view.messages[1];
        assert_eq!(assistant.role, Role::Assistant);
        assert_eq!(
            assistant.reasoning.as_ref().expect("echoed").text,
            " arithmetic: 2+2 = 4 ",
            "reasoning rides back byte for byte"
        );
        assert_eq!(assistant.tool_calls.len(), 1);
        assert_eq!(
            assistant.tool_calls[0].id, "call_00_live",
            "the provider's own id"
        );
        assert_eq!(assistant.tool_calls[0].name, "read_file");
        assert_eq!(view.messages[2].role, Role::Tool);
        assert_eq!(
            view.messages[2].tool_call_id.as_deref(),
            Some("call_00_live")
        );
        assert_eq!(view.messages[3].content.text, "Two plus two is 4.");
    }

    #[tokio::test]
    async fn reasoning_is_dropped_when_the_provider_does_not_echo() {
        let (_dir, store, session) = store_with_items(false).await;
        let view = StoreHistory::new(store, session, false, None)
            .view()
            .await
            .expect("view");
        assert!(view.messages[1].reasoning.is_none(), "the table said no");
    }

    #[tokio::test]
    async fn a_tool_result_without_a_call_pairing_degrades_to_an_empty_id() {
        // Legacy row (no provider_call_id) → synthesized stand-in, still paired.
        let (_dir, store) = store().await;
        let session = session_row();
        store.create_session(session.clone()).await.expect("create");
        let session_id = session.id;
        let call_id = ItemId::new();
        let mut call = Item::with_id(
            call_id,
            session_id,
            ItemKind::ToolCall(ToolCall {
                name: "glob".to_owned(),
                args: serde_json::json!({"pattern": "*"}),
                status: ToolStatus::Completed,
                provider_call_id: None,
            }),
        );
        call.parent = None;
        let mut result = Item::new(
            session_id,
            ItemKind::ToolResult(ToolResult {
                call: call_id,
                output: ToolOutput::text("a.txt"),
                is_error: false,
            }),
        );
        result.parent = Some(call.id);
        store
            .append_items(vec![call, result])
            .await
            .expect("append");

        let view = StoreHistory::new(store, session_id, false, None)
            .view()
            .await
            .expect("view");
        assert_eq!(view.messages.len(), 2);
        assert_eq!(view.messages[0].tool_calls[0].id, "call-synth-1");
        assert_eq!(
            view.messages[1].tool_call_id.as_deref(),
            Some("call-synth-1"),
            "paired"
        );
    }

    #[tokio::test]
    async fn checkpoints_are_not_provider_visible() {
        let (_dir, store, session) = store_with_items(true).await;
        let view = StoreHistory::new(store, session, true, None)
            .view()
            .await
            .expect("view");
        // The chain holds six items; four of them are conversation. The checkpoint is the one
        // that must not surface — its commit id is the tell.
        assert_eq!(view.messages.len(), 4, "{view:?}");
        assert!(
            view.messages
                .iter()
                .all(|m| !m.content.text.contains("abc")),
            "a checkpoint must not become a message"
        );
    }

    #[tokio::test]
    async fn the_prompt_leads_the_view_and_does_not_move_between_views() {
        let (_dir, store, session) = store_with_items(true).await;
        let history =
            StoreHistory::new(store, session, true, Some("the shipped prompt".to_owned()));

        let first = history.view().await.expect("view");
        assert_eq!(
            first.messages[0].role,
            Role::System,
            "the prompt leads, ahead of the branch history"
        );
        assert_eq!(first.messages[0].content.text, "the shipped prompt");
        assert_eq!(first.messages.len(), 5, "the prompt plus the four messages");

        // Frozen for the runtime's life (D15): the same string on every view, so the provider's
        // prefix cache keeps its head stable across turns.
        let second = history.view().await.expect("view");
        assert_eq!(second.messages[0], first.messages[0], "unchanged");
        assert_eq!(
            second.messages[1..],
            first.messages[1..],
            "and the history behind it is rebuilt the same way"
        );
    }
}
