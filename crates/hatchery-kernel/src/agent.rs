//! The turn loop: assemble, stream, call tools, repeat.

use std::sync::Arc;

use futures::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use hatchery_protocol::{
    ApprovalId, ApprovalOption, ApprovalRequest, Content, Item, ItemId, ItemKind, ItemKindTag,
    ReasoningBlock, SessionId, SignatureBlock, StopReason, ToolCall, ToolOutput, ToolProgress,
    ToolResult, ToolStatus, TurnId, Usage,
};

use crate::command::AgentCommand;
use crate::error::{KernelError, LlmError};
use crate::message::{
    ChatOptions, FinishReason, Message, StreamEvent, ToolCallDelta, ToolCallRequest, ToolDef,
};
use crate::sink::KernelEvent;
use crate::state::{Ports, TurnCompletion, TurnLimits, TurnState};
use crate::tools::ToolInvocation;

/// How many commands may queue before a submitter is made to wait.
const COMMAND_CAPACITY: usize = 64;

/// Returned when the agent's loop has stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the agent is no longer running")]
pub struct AgentGone;

/// The kernel's handle on a running agent.
///
/// Cloneable: several frontends may drive one session, and the daemon decides whose command wins.
#[derive(Clone, Debug)]
pub struct AgentHandle {
    commands: mpsc::Sender<AgentCommand>,
}

impl AgentHandle {
    /// Sends a command, waiting if the queue is full.
    ///
    /// # Errors
    ///
    /// [`AgentGone`] once the agent has stopped; the submitter must not assume its command ran.
    pub async fn submit(&self, command: AgentCommand) -> Result<(), AgentGone> {
        self.commands.send(command).await.map_err(|_| AgentGone)
    }

    /// Sends a command without waiting.
    ///
    /// # Errors
    ///
    /// [`AgentGone`] when the agent has stopped, or when the queue is full.
    pub fn try_submit(&self, command: AgentCommand) -> Result<(), AgentGone> {
        self.commands.try_send(command).map_err(|_| AgentGone)
    }

    /// True once the agent has stopped.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.commands.is_closed()
    }

    /// Asks the running turn to stop.
    ///
    /// # Errors
    ///
    /// See [`AgentHandle::submit`].
    pub async fn interrupt(&self) -> Result<(), AgentGone> {
        self.submit(AgentCommand::interrupt()).await
    }
}

/// Assembles an [`Agent`].
pub struct AgentBuilder {
    session: SessionId,
    options: ChatOptions,
    ports: Ports,
    limits: TurnLimits,
}

impl AgentBuilder {
    /// An agent for one session.
    ///
    /// The session id is here and nowhere else, because items carry it: the kernel is otherwise
    /// blind to sessions, modes and storage.
    #[must_use]
    pub fn new(session: SessionId, options: ChatOptions, ports: Ports) -> Self {
        Self {
            session,
            options,
            ports,
            limits: TurnLimits::default(),
        }
    }

    /// Replaces the default fuses.
    #[must_use]
    pub fn limits(mut self, limits: TurnLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Splits into the runnable agent and a handle for sending it commands.
    #[must_use]
    pub fn build(self) -> (Agent, AgentHandle) {
        let (commands_tx, commands_rx) = mpsc::channel(COMMAND_CAPACITY);
        let agent = Agent {
            session: self.session,
            options: self.options,
            ports: self.ports,
            limits: self.limits,
            commands: commands_rx,
            state: TurnState::Idle,
            turn: TurnId::new(),
            tail: None,
            cancel: CancellationToken::new(),
        };
        let handle = AgentHandle {
            commands: commands_tx,
        };
        (agent, handle)
    }
}

/// Drives one session's turns.
///
/// Not `Clone` and not shareable: one runtime owns a session at a time (invariant 1). Commands
/// arrive on a channel, events leave through the sink, and the loop runs one turn at a time.
pub struct Agent {
    session: SessionId,
    options: ChatOptions,
    ports: Ports,
    limits: TurnLimits,
    commands: mpsc::Receiver<AgentCommand>,
    state: TurnState,
    /// The current turn's id. Meaningless while idle.
    turn: TurnId,
    /// Parent for the next item: the last item the store has seen finish.
    ///
    /// Every item chains onto a *finished* item, never onto a reserved-but-open one, because only
    /// finished items are written — chaining onto an id the store never received would break the
    /// foreign key on the next insert.
    tail: Option<ItemId>,
    cancel: CancellationToken,
}

/// An item that is still being streamed into.
struct StreamedItem {
    id: ItemId,
    parent: Option<ItemId>,
    kind: ItemKindTag,
    text: String,
    signature: Option<SignatureBlock>,
}

/// A tool call being assembled from stream fragments.
#[derive(Default)]
struct PartialCall {
    index: u32,
    id: Option<String>,
    name: Option<String>,
    args: String,
}

impl PartialCall {
    fn into_request(self) -> ToolCallRequest {
        ToolCallRequest {
            id: self.id.unwrap_or_else(|| format!("call-{}", self.index)),
            name: self.name.unwrap_or_default(),
            // Arguments that are not valid JSON are passed through as a string: the tool will
            // reject them and report why, which is information the model can act on. Dropping the
            // call would leave it guessing.
            args: serde_json::from_str(&self.args).unwrap_or(Value::String(self.args)),
        }
    }
}

/// What one round of streaming produced.
#[derive(Default)]
struct Round {
    /// The item currently being streamed, if any. At most one at a time: see
    /// [`Agent::push_delta`].
    open: Option<StreamedItem>,
    /// Everything the model said this round.
    text: String,
    /// Everything it thought this round.
    reasoning: String,
    /// The provider's signature for that reasoning, if it issued one.
    signature: Option<SignatureBlock>,
    /// Tool calls, in the order the provider indexed them.
    calls: Vec<PartialCall>,
    /// Why the provider stopped, when it said.
    finish: Option<FinishReason>,
}

/// Whether the loop should carry on after a command.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CommandFlow {
    Continue,
    Stop,
}

/// What the stream loop woke up on.
enum StreamStep {
    Command(Option<AgentCommand>),
    Cancelled,
    Event(Option<StreamEvent>),
}

/// What a tool invocation came back with.
enum ToolRun {
    Done(ToolInvocation),
    Interrupted,
}

/// What the tool loop woke up on.
enum ToolStep {
    Command(Option<AgentCommand>),
    Cancelled,
    Progress(ToolProgress),
    Done(Result<ToolInvocation, KernelError>),
}

/// What an approval wait came back with.
enum Decision {
    Chosen(ApprovalOption),
    Interrupted,
}

/// What a guarded startup await woke up on.
enum Startup<T> {
    Done(Result<T, KernelError>),
    Cancelled,
    Command(Option<AgentCommand>),
}

impl Agent {
    /// Runs until every [`AgentHandle`] is dropped.
    ///
    /// Turn failures are reported as [`KernelEvent::TurnEnded`], not as a return value: a session
    /// survives a failed turn, and the daemon decides what a frontend sees.
    pub async fn run(mut self) {
        while let Some(command) = self.commands.recv().await {
            match command {
                AgentCommand::TurnInput(content) => self.run_turn(content).await,
                AgentCommand::Interrupt => {
                    tracing::debug!("interrupt with no turn running; ignored");
                }
                AgentCommand::ApprovalDecision { request_id, .. } => {
                    tracing::warn!(%request_id, "approval decision with no request pending; ignored");
                }
            }
        }
        tracing::debug!("all handles dropped; the agent loop is done");
    }

    /// Runs one turn to its terminal event.
    async fn run_turn(&mut self, content: Content) {
        let turn = TurnId::new();
        self.turn = turn;
        self.cancel = CancellationToken::new();
        let mut usage = Usage::default();

        self.emit(KernelEvent::TurnStarted { turn }).await;
        let outcome = self.turn_body(content, &mut usage).await;
        self.transition(TurnState::Idle).await;

        let completion = match outcome {
            Ok(reason) => TurnCompletion::Completed { reason, usage },
            // The rounds that ran before the failure were paid for; dropping their usage would
            // under-report exactly the turns a retry will pay for again.
            Err(error) => TurnCompletion::Failed { error, usage },
        };
        self.emit(KernelEvent::TurnEnded { turn, completion }).await;
    }

    /// Awaits a startup future while staying interruptible.
    ///
    /// `history.view()` and the provider's `chat_stream()` both run inside the same select as the
    /// cancel token and the command channel: a startup awaited outside it could not be
    /// interrupted, and a provider that hung while connecting would wedge the agent with no
    /// escape but an abort — which skips the terminal event the contract promises. `None` means
    /// the startup was interrupted; the future is dropped, never resumed.
    async fn guarded_startup<T, E, F>(&mut self, startup: F) -> Result<Option<T>, KernelError>
    where
        E: Into<KernelError>,
        F: std::future::Future<Output = Result<T, E>>,
    {
        tokio::pin!(startup);
        loop {
            let step = {
                // Scoped so the select's borrows of `self` end before the body runs.
                let cancel = self.cancel.clone();
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => Startup::Cancelled,
                    command = self.commands.recv() => Startup::Command(command),
                    result = &mut startup => Startup::Done(result.map_err(Into::into)),
                }
            };
            match step {
                Startup::Done(result) => return result.map(Some),
                Startup::Cancelled => return Ok(None),
                Startup::Command(command) => {
                    if self.apply_command(command).await == CommandFlow::Stop {
                        return Ok(None);
                    }
                }
            }
        }
    }

    /// The turn itself: assemble, then loop over rounds until a stop reason.
    async fn turn_body(
        &mut self,
        content: Content,
        usage: &mut Usage,
    ) -> Result<StopReason, KernelError> {
        self.transition(TurnState::Assembling).await;
        let history = Arc::clone(&self.ports.history);
        let Some(view) = self.guarded_startup(history.view()).await? else {
            return Ok(StopReason::Interrupted);
        };
        self.tail = view.head;
        let mut messages = view.messages;

        // The user's message becomes an item too: whatever the model is shown must be rebuildable
        // from the store (invariant 2), and the same content must not live in two places.
        let text = content.clone();
        self.append(ItemKind::UserMessage(content)).await;
        messages.push(Message::user(text));

        // Frozen once for the whole turn: the catalogue the model sees and the catalogue calls are
        // dispatched through must be the same list, even if the registry is swapped mid-turn.
        let tool_defs = self.ports.tools.snapshot();

        for round in 1..=self.limits.max_rounds {
            if self.cancel.is_cancelled() {
                return Ok(StopReason::Interrupted);
            }
            self.transition(TurnState::Streaming { round }).await;

            let Some(mut stream) = self.stream_round(&tool_defs, &messages, usage).await? else {
                return Ok(StopReason::Interrupted);
            };
            if self.cancel.is_cancelled() {
                return Ok(StopReason::Interrupted);
            }

            let Some(finish) = stream.finish.clone() else {
                return Err(KernelError::Provider(LlmError::retryable(
                    "the provider stream ended without a finish reason",
                )));
            };
            if stream.calls.is_empty() {
                return Ok(stop_reason(finish));
            }

            // The assistant message that asked for the tools is part of the conversation too.
            let mut assistant = Message::assistant(Content::text(&stream.text));
            if !stream.reasoning.is_empty() {
                assistant = assistant.with_reasoning(ReasoningBlock {
                    text: stream.reasoning.clone(),
                    signature: stream.signature.clone(),
                });
            }
            let requests: Vec<ToolCallRequest> = std::mem::take(&mut stream.calls)
                .into_iter()
                .map(PartialCall::into_request)
                .collect();
            messages.push(assistant.with_tool_calls(requests.clone()));

            self.transition(TurnState::Executing { round }).await;
            for request in requests {
                match self.run_call(&request, round).await? {
                    None => return Ok(StopReason::Interrupted),
                    Some(invocation) => {
                        messages.push(Message::tool_result(
                            &request.id,
                            invocation.output.text.clone(),
                            invocation.is_error,
                        ));
                    }
                }
            }
        }

        Ok(StopReason::MaxRounds)
    }

    /// Streams one provider round, committing items as they complete.
    ///
    /// `None` means the user interrupted; the items streamed so far have still been committed, so
    /// the transcript matches what they saw.
    async fn stream_round(
        &mut self,
        tool_defs: &[ToolDef],
        messages: &[Message],
        usage: &mut Usage,
    ) -> Result<Option<Round>, KernelError> {
        let options = ChatOptions {
            tool_defs: tool_defs.to_vec(),
            ..self.options.clone()
        };
        let provider = Arc::clone(&self.ports.provider);
        let startup = provider.chat_stream(options, messages.to_vec(), self.cancel.clone());
        let Some(mut stream) = self.guarded_startup(startup).await? else {
            // Interrupted while the provider was still starting up: nothing was streamed, so
            // there is no open item to commit.
            return Ok(None);
        };

        let mut round = Round::default();
        let mut failure = None;
        let mut interrupted = false;

        loop {
            let step = {
                // Scoped so the select's borrows of `self` end before the body runs, which is
                // what lets the body take `&mut self`.
                let cancel = self.cancel.clone();
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => StreamStep::Cancelled,
                    command = self.commands.recv() => StreamStep::Command(command),
                    event = stream.next() => StreamStep::Event(event),
                }
            };

            match step {
                StreamStep::Cancelled => {
                    interrupted = true;
                    break;
                }
                StreamStep::Command(command) => {
                    if self.apply_command(command).await == CommandFlow::Stop {
                        interrupted = true;
                        break;
                    }
                }
                StreamStep::Event(None) => break,
                StreamStep::Event(Some(event)) => match event {
                    StreamEvent::TextDelta { text } => {
                        round.text.push_str(&text);
                        self.push_delta(&mut round, ItemKindTag::AssistantMessage, &text)
                            .await;
                    }
                    StreamEvent::ReasoningDelta { text } => {
                        round.reasoning.push_str(&text);
                        self.push_delta(&mut round, ItemKindTag::Reasoning, &text)
                            .await;
                    }
                    StreamEvent::ReasoningDone { signature } => {
                        // The signature belongs to the reasoning item that is open right now; a
                        // later one would belong to a later item.
                        if let Some(open) = round
                            .open
                            .as_mut()
                            .filter(|open| open.kind == ItemKindTag::Reasoning)
                        {
                            open.signature = signature.clone();
                        }
                        round.signature = signature;
                    }
                    StreamEvent::ToolCall { delta } => merge_call(&mut round.calls, delta),
                    StreamEvent::Usage { usage: reported } => usage.merge(&reported),
                    StreamEvent::Done { finish_reason } => {
                        round.finish = Some(finish_reason);
                        break;
                    }
                    StreamEvent::Error { error } => {
                        failure = Some(error);
                        break;
                    }
                },
            }
        }

        self.close_open(&mut round).await;
        if let Some(error) = failure {
            return Err(KernelError::Provider(error));
        }
        if interrupted {
            return Ok(None);
        }
        Ok(Some(round))
    }

    /// Runs one tool call: announce it, ask for approval, invoke, record.
    ///
    /// `None` means the turn was interrupted; the call is still recorded, with its cancelled
    /// status, so history says what happened. `round` is the round executing it, so an answered
    /// approval can report the transition back to [`TurnState::Executing`].
    async fn run_call(
        &mut self,
        request: &ToolCallRequest,
        round: u32,
    ) -> Result<Option<ToolInvocation>, KernelError> {
        let id = ItemId::new();
        let parent = self.tail;
        let stub = hatchery_protocol::ItemStub {
            id,
            parent,
            turn: Some(self.turn),
            kind: ItemKindTag::ToolCall,
        };
        self.emit(KernelEvent::ItemStarted { item: stub }).await;
        self.emit(KernelEvent::ToolCallStarted {
            item: id,
            summary: self.ports.tools.summarize(&request.name, &request.args),
        })
        .await;

        let decision = match self.ports.tools.approval_for(&request.name, &request.args) {
            None => Some(ApprovalOption::AllowOnce),
            Some(approval) => {
                let request_id = ApprovalId::new();
                match self.await_approval(request_id, approval).await? {
                    Decision::Interrupted => None,
                    Decision::Chosen(option) => {
                        // The design's edge back (docs/design/kernel.md §3): the wait ended when
                        // the answer arrived. A frontend projects `SessionStatus` from
                        // `StateChanged`, so without this the tools that run now — and every
                        // later call in the round — would report "waiting for approval" under a
                        // request id that is no longer pending.
                        self.transition(TurnState::Executing { round }).await;
                        Some(option)
                    }
                }
            }
        };

        let (invocation, status) = match decision {
            None => {
                self.commit_call(id, parent, request, ToolStatus::Cancelled)
                    .await;
                return Ok(None);
            }
            Some(option) if !option.allows() => (
                ToolInvocation {
                    output: ToolOutput::text(format!(
                        "the user denied this call to `{}`; do not repeat it",
                        request.name
                    )),
                    is_error: true,
                },
                ToolStatus::Denied,
            ),
            Some(_) => match self
                .invoke_tool(&request.name, request.args.clone(), id)
                .await?
            {
                ToolRun::Interrupted => {
                    self.commit_call(id, parent, request, ToolStatus::Cancelled)
                        .await;
                    return Ok(None);
                }
                ToolRun::Done(invocation) => {
                    let status = if invocation.is_error {
                        ToolStatus::Failed
                    } else {
                        ToolStatus::Completed
                    };
                    (invocation, status)
                }
            },
        };

        self.commit_call(id, parent, request, status).await;
        self.append(ItemKind::ToolResult(ToolResult {
            call: id,
            output: invocation.output.clone(),
            is_error: invocation.is_error,
        }))
        .await;
        Ok(Some(invocation))
    }

    /// Invokes a tool, forwarding progress and watching for an interrupt.
    async fn invoke_tool(
        &mut self,
        name: &str,
        args: Value,
        call_item: ItemId,
    ) -> Result<ToolRun, KernelError> {
        let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<ToolProgress>();
        let tools = Arc::clone(&self.ports.tools);
        let invoke = tools.invoke(name, args, self.cancel.clone(), progress_tx);
        tokio::pin!(invoke);

        loop {
            let step = {
                let cancel = self.cancel.clone();
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => ToolStep::Cancelled,
                    command = self.commands.recv() => ToolStep::Command(command),
                    Some(progress) = progress_rx.recv() => ToolStep::Progress(progress),
                    result = &mut invoke => ToolStep::Done(result),
                }
            };

            match step {
                ToolStep::Done(result) => return result.map(ToolRun::Done),
                ToolStep::Progress(progress) => {
                    self.emit(KernelEvent::ToolCallProgress {
                        item: call_item,
                        chunk: progress.chunk,
                    })
                    .await;
                }
                ToolStep::Cancelled => return Ok(ToolRun::Interrupted),
                ToolStep::Command(command) => {
                    if self.apply_command(command).await == CommandFlow::Stop {
                        return Ok(ToolRun::Interrupted);
                    }
                }
            }
        }
    }

    /// Waits for the user's answer to an approval request.
    ///
    /// There is no timeout here: timing out is a policy of the approval gate that answers (it is
    /// fail-closed, `docs/design/capabilities.md` §1), and the kernel has no way to know how long
    /// a human needs.
    async fn await_approval(
        &mut self,
        request_id: ApprovalId,
        request: ApprovalRequest,
    ) -> Result<Decision, KernelError> {
        self.transition(TurnState::AwaitingApproval { request_id })
            .await;
        // The offers outlive the event: an answer that was not on the list is refused below, so
        // a hard gate (`ApprovalRequest::once_only`, which omits the remembered options) cannot
        // be answered with `allow_always` by a buggy or hostile frontend.
        let offers = request.options.clone();
        self.emit(KernelEvent::ApprovalNeeded {
            request_id,
            request,
        })
        .await;

        loop {
            let step = {
                let cancel = self.cancel.clone();
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => StreamStep::Cancelled,
                    command = self.commands.recv() => StreamStep::Command(command),
                }
            };

            match step {
                StreamStep::Cancelled => return Ok(Decision::Interrupted),
                StreamStep::Event(_) => unreachable!("the approval wait subscribes to no stream"),
                StreamStep::Command(None) => {
                    self.cancel.cancel();
                    return Ok(Decision::Interrupted);
                }
                StreamStep::Command(Some(AgentCommand::ApprovalDecision {
                    request_id: answered,
                    option,
                })) => {
                    if answered != request_id {
                        tracing::warn!(%answered, "approval decision for a different request; ignored");
                    } else if !offers.contains(&option) {
                        tracing::warn!(
                            %request_id,
                            answer = ?option,
                            offers = ?offers,
                            "approval answer was not one of the offered options; ignored"
                        );
                    } else {
                        return Ok(Decision::Chosen(option));
                    }
                }
                StreamStep::Command(Some(AgentCommand::Interrupt)) => {
                    self.cancel.cancel();
                    return Ok(Decision::Interrupted);
                }
                StreamStep::Command(Some(AgentCommand::TurnInput(_))) => {
                    tracing::warn!("turn input while waiting for approval; ignored");
                }
            }
        }
    }

    /// Reacts to a command that arrived between awaits.
    async fn apply_command(&mut self, command: Option<AgentCommand>) -> CommandFlow {
        match command {
            Some(AgentCommand::Interrupt) => {
                self.cancel.cancel();
                CommandFlow::Stop
            }
            Some(AgentCommand::TurnInput(_)) => {
                // The daemon rejects a second prompt with `TurnInProgress`, so this is a caller
                // bug. Queueing would invent a turn, which is worse than dropping it loudly.
                tracing::warn!("turn input while a turn is running; dropped");
                CommandFlow::Continue
            }
            Some(AgentCommand::ApprovalDecision { request_id, .. }) => {
                tracing::warn!(%request_id, "no approval request pending; decision dropped");
                CommandFlow::Continue
            }
            None => {
                // Every handle is gone: the daemon is shutting this session down.
                self.cancel.cancel();
                CommandFlow::Stop
            }
        }
    }

    /// Appends a delta to the round's open item, opening or closing items as needed.
    ///
    /// At most one item is open at a time. Providers stream reasoning and text in one stream, and
    /// two open items would both have to chain onto the last *finished* item — a fork, which would
    /// hide one of them from the active branch and lose it on rebuild. Closing the reasoning item
    /// when text starts (and reopening a new one if reasoning resumes) keeps the tree a chain.
    async fn push_delta(&mut self, round: &mut Round, kind: ItemKindTag, text: &str) {
        let same_kind = round.open.as_ref().is_some_and(|open| open.kind == kind);
        if !same_kind {
            self.close_open(round).await;
            let id = ItemId::new();
            let parent = self.tail;
            let stub = hatchery_protocol::ItemStub {
                id,
                parent,
                turn: Some(self.turn),
                kind,
            };
            round.open = Some(StreamedItem {
                id,
                parent,
                kind,
                text: String::new(),
                signature: None,
            });
            self.emit(KernelEvent::ItemStarted { item: stub }).await;
        }

        let open = round.open.as_mut().expect("an item was just opened");
        open.text.push_str(text);
        let item = open.id;
        let text = text.to_owned();
        if kind == ItemKindTag::Reasoning {
            self.emit(KernelEvent::ReasoningDelta { item, text }).await;
        } else {
            self.emit(KernelEvent::TextDelta { item, text }).await;
        }
    }

    /// Commits the round's open item, if any.
    async fn close_open(&mut self, round: &mut Round) {
        let Some(open) = round.open.take() else {
            return;
        };
        let kind = if open.kind == ItemKindTag::Reasoning {
            ItemKind::Reasoning(ReasoningBlock {
                text: open.text,
                signature: open.signature,
            })
        } else {
            ItemKind::AssistantMessage(Content::text(open.text))
        };
        self.commit(open.id, open.parent, kind).await;
    }

    /// Appends a complete item and advances the chain.
    async fn append(&mut self, kind: ItemKind) -> ItemId {
        let id = ItemId::new();
        self.commit(id, self.tail, kind).await;
        id
    }

    /// Commits a tool call item with its final status.
    async fn commit_call(
        &mut self,
        id: ItemId,
        parent: Option<ItemId>,
        request: &ToolCallRequest,
        status: ToolStatus,
    ) {
        self.commit(
            id,
            parent,
            ItemKind::ToolCall(ToolCall {
                name: request.name.clone(),
                args: request.args.clone(),
                status,
            }),
        )
        .await;
    }

    /// Announces a finished item and makes it the parent of the next one.
    async fn commit(&mut self, id: ItemId, parent: Option<ItemId>, kind: ItemKind) {
        let item = Item::with_id(id, self.session, kind).with_turn(self.turn);
        let item = match parent {
            Some(parent) => item.with_parent(parent),
            None => item,
        };
        self.tail = Some(id);
        self.emit(KernelEvent::ItemFinished { item }).await;
    }

    /// Moves the state machine, reporting the transition when it is a change.
    async fn transition(&mut self, to: TurnState) {
        let from = std::mem::replace(&mut self.state, to);
        if from != to {
            self.emit(KernelEvent::StateChanged { from, to }).await;
        }
    }

    /// Reports an event.
    async fn emit(&self, event: KernelEvent) {
        self.ports.sink.emit(event).await;
    }
}

/// Folds a streamed tool-call fragment into the round's calls.
fn merge_call(calls: &mut Vec<PartialCall>, delta: ToolCallDelta) {
    if let Some(call) = calls.iter_mut().find(|call| call.index == delta.index) {
        if let Some(id) = delta.id {
            call.id = Some(id);
        }
        if let Some(name) = delta.name {
            call.name = Some(name);
        }
        call.args.push_str(&delta.args_delta);
        return;
    }
    calls.push(PartialCall {
        index: delta.index,
        id: delta.id,
        name: delta.name,
        args: delta.args_delta,
    });
}

/// Maps the provider's finish reason onto the turn's stop reason.
fn stop_reason(finish: FinishReason) -> StopReason {
    match finish {
        FinishReason::Length => StopReason::MaxTokens,
        // A refusal on policy grounds is not a clean finish: the user must be able to tell that
        // the answer was cut by the filter rather than completed by the model.
        FinishReason::ContentFilter => StopReason::ContentFilter,
        FinishReason::Stop | FinishReason::ToolCalls | FinishReason::Other(_) => {
            StopReason::ModelDone
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delta(index: u32, id: Option<&str>, name: Option<&str>, args: &str) -> ToolCallDelta {
        ToolCallDelta {
            index,
            id: id.map(str::to_owned),
            name: name.map(str::to_owned),
            args_delta: args.to_owned(),
        }
    }

    #[test]
    fn streamed_call_fragments_accumulate() {
        let mut calls = Vec::new();
        merge_call(
            &mut calls,
            delta(0, Some("call-1"), Some("write_file"), "{\"path\":"),
        );
        merge_call(&mut calls, delta(0, None, None, "\"a.rs\""));
        merge_call(&mut calls, delta(0, None, None, "}"));
        assert_eq!(calls.len(), 1);
        let request = calls.remove(0).into_request();
        assert_eq!(request.id, "call-1");
        assert_eq!(request.name, "write_file");
        assert_eq!(request.args, serde_json::json!({"path": "a.rs"}));
    }

    #[test]
    fn parallel_tool_calls_stay_separate_by_index() {
        let mut calls = Vec::new();
        merge_call(
            &mut calls,
            delta(0, Some("call-1"), Some("read_file"), "{}"),
        );
        merge_call(&mut calls, delta(1, Some("call-2"), Some("glob"), "{}"));
        merge_call(&mut calls, delta(0, None, None, " "));
        assert_eq!(calls.len(), 2);
        let ids: Vec<String> = calls
            .into_iter()
            .map(|call| call.into_request().id)
            .collect();
        assert_eq!(ids, vec!["call-1", "call-2"]);
    }

    #[test]
    fn a_call_without_an_id_gets_a_stable_one() {
        let mut calls = Vec::new();
        merge_call(&mut calls, delta(3, None, Some("shell"), "{}"));
        assert_eq!(calls.remove(0).into_request().id, "call-3");
    }

    #[test]
    fn arguments_that_are_not_json_survive_as_text() {
        let mut calls = Vec::new();
        merge_call(&mut calls, delta(0, Some("c"), Some("shell"), "not json"));
        let request = calls.remove(0).into_request();
        assert_eq!(request.args, Value::String("not json".to_owned()));
    }

    #[test]
    fn finish_reasons_map_onto_stop_reasons() {
        assert_eq!(stop_reason(FinishReason::Length), StopReason::MaxTokens);
        assert_eq!(stop_reason(FinishReason::Stop), StopReason::ModelDone);
        assert_eq!(stop_reason(FinishReason::ToolCalls), StopReason::ModelDone);
        assert_eq!(
            stop_reason(FinishReason::ContentFilter),
            StopReason::ContentFilter,
            "a filtered answer is not a clean finish"
        );
        assert_eq!(
            stop_reason(FinishReason::Other("weird".to_owned())),
            StopReason::ModelDone
        );
    }
}
