//! Neutral agent turn loop.
//!
//! Layer **L1**, zero business semantics (docs/architecture.md §3): the kernel drives
//! `assemble → LLM stream → tool calls → results → loop` and talks to the outside world only
//! through injected traits ([`LlmProvider`], [`ToolHost`], [`HistorySource`], [`EventSink`]).
//! Modes, workspaces, storage and ACP are all daemon-side concerns.
//!
//! It reuses [`hatchery_protocol`]'s vocabulary — ids, content, tool output, approval requests —
//! rather than mirroring it, which is why the protocol sits below the kernel (M0b layering
//! decision, see `docs/architecture.md` §3). It must still never see `hatchery-capabilities`: the
//! tool layer is hidden behind the three-method [`ToolHost`] seam.
//!
//! Design: `docs/design/kernel.md`.
//!
//! # Examples
//!
//! The kernel's own types stand alone — a request is described by [`ChatOptions`], a turn by
//! [`TurnLimits`]:
//!
//! ```
//! use hatchery_kernel::{AgentCommand, ChatOptions, Message, TurnLimits, TurnState};
//!
//! let options = ChatOptions::new("deepseek-reasoner");
//! assert_eq!(options.model, "deepseek-reasoner");
//! assert!(options.tool_defs.is_empty(), "tools come from the frozen snapshot");
//!
//! assert!(!TurnState::Idle.is_active());
//! assert_eq!(TurnLimits::default().max_rounds, 100);
//! assert!(matches!(
//!     AgentCommand::prompt("hello"),
//!     AgentCommand::TurnInput(_)
//! ));
//! assert_eq!(Message::user("hello").role, hatchery_kernel::Role::User);
//! ```
//!
//! Driving a turn means building an agent from four seams, spawning it, and sending commands:
//!
//! ```
//! # use async_trait::async_trait;
//! # use hatchery_kernel::{
//! #     AgentBuilder, AgentCommand, ChatOptions, EventSink, HistorySource, HistoryView, KernelError,
//! #     KernelEvent, LlmError, Message, Ports, StreamEvent, ToolDef, ToolHost, ToolInvocation,
//! # };
//! # use hatchery_protocol::{ApprovalRequest, SessionId, ToolCallSummary, ToolOutput, ToolProgress};
//! # use serde_json::Value;
//! # use std::sync::Arc;
//! # use tokio::sync::mpsc::UnboundedSender;
//! # use tokio_util::sync::CancellationToken;
//! struct NoProvider;
//! # #[async_trait]
//! # impl hatchery_kernel::LlmProvider for NoProvider {
//! #     async fn chat_stream(
//! #         &self,
//! #         _options: ChatOptions,
//! #         _messages: Vec<Message>,
//! #         _cancel: CancellationToken,
//! #     ) -> Result<futures::stream::BoxStream<'static, StreamEvent>, LlmError> {
//! #         Err(LlmError::fatal("no provider is wired up"))
//! #     }
//! # }
//! # struct NoTools;
//! # #[async_trait]
//! # impl ToolHost for NoTools {
//! #     fn snapshot(&self) -> Vec<ToolDef> {
//! #         Vec::new()
//! #     }
//! #     fn summarize(&self, name: &str, _args: &Value) -> ToolCallSummary {
//! #         ToolCallSummary::new(name)
//! #     }
//! #     fn approval_for(&self, _name: &str, _args: &Value) -> Option<ApprovalRequest> {
//! #         None
//! #     }
//! #     async fn invoke(
//! #         &self,
//! #         name: &str,
//! #         _args: Value,
//! #         _cancel: CancellationToken,
//! #         _progress: UnboundedSender<ToolProgress>,
//! #     ) -> Result<ToolInvocation, KernelError> {
//! #         Err(KernelError::tool(name, "no tools here"))
//! #     }
//! # }
//! # struct EmptyHistory;
//! # #[async_trait]
//! # impl HistorySource for EmptyHistory {
//! #     async fn view(&self) -> Result<HistoryView, KernelError> {
//! #         Ok(HistoryView::default())
//! #     }
//! # }
//! # struct Silent;
//! # #[async_trait]
//! # impl EventSink for Silent {
//! #     async fn emit(&self, _event: KernelEvent) {}
//! # }
//! # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
//! # runtime.block_on(async {
//! let (agent, handle) = AgentBuilder::new(
//!     SessionId::new(),
//!     ChatOptions::new("deepseek-reasoner"),
//!     Ports::new(
//!         Arc::new(NoProvider),
//!         Arc::new(NoTools),
//!         Arc::new(EmptyHistory),
//!         Arc::new(Silent),
//!     ),
//! )
//! .build();
//!
//! let running = tokio::spawn(agent.run());
//! handle
//!     .submit(AgentCommand::prompt("why is the store slow?"))
//!     .await
//!     .expect("the agent is still running");
//! assert!(!handle.is_closed());
//! running.abort();
//! # });
//! ```

mod agent;
mod command;
mod error;
mod history;
mod message;
mod provider;
mod sink;
mod state;
mod tools;

pub use agent::{Agent, AgentBuilder, AgentGone, AgentHandle};
pub use command::AgentCommand;
pub use error::{KernelError, LlmError};
pub use history::{HistorySource, HistoryView};
pub use message::{
    ChatOptions, FinishReason, Message, Role, StreamEvent, ToolCallDelta, ToolCallRequest, ToolDef,
};
pub use provider::LlmProvider;
pub use sink::{EventSink, KernelEvent};
pub use state::{Ports, TurnCompletion, TurnLimits, TurnState};
pub use tools::{ToolHost, ToolInvocation};
