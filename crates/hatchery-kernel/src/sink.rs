//! The event seam: everything the kernel wants the outside world to know.

use async_trait::async_trait;

use hatchery_protocol::{
    ApprovalId, ApprovalRequest, Item, ItemId, ItemStub, ToolCallSummary, TurnId,
};

use crate::state::{TurnCompletion, TurnState};

/// Something the kernel did or wants.
///
/// The daemon projects these onto the wire: `Item` becomes the store row and the
/// `ItemFinished` event, `TurnEnded` becomes `TurnFinished` or `TurnFailed`, and the envelope's
/// session id plus generation are attached by the daemon, which is the only layer that knows them.
///
/// Failure is not a separate event: [`TurnCompletion::Failed`] carries it, so a turn has exactly
/// one terminal event.
#[derive(Clone, Debug, PartialEq)]
pub enum KernelEvent {
    /// A turn began.
    TurnStarted {
        /// The new turn.
        turn: TurnId,
    },
    /// The state machine moved. Emitted for every transition, including the ones back to
    /// [`TurnState::Idle`], so a frontend can project `SessionStatus` without inferring it.
    StateChanged {
        /// Where it was.
        from: TurnState,
        /// Where it is now.
        to: TurnState,
    },
    /// A streamed item exists and will be filled in by deltas.
    ItemStarted {
        /// The item's identity and position.
        item: ItemStub,
    },
    /// More assistant text.
    TextDelta {
        /// Which item is growing.
        item: ItemId,
        /// The increment.
        text: String,
    },
    /// More reasoning text.
    ReasoningDelta {
        /// Which item is growing.
        item: ItemId,
        /// The increment, verbatim.
        text: String,
    },
    /// An item is complete and must be committed by the daemon before anything else happens
    /// (invariant 2: model-visible means logged).
    ItemFinished {
        /// The finished item.
        item: Item,
    },
    /// A tool call began, with a summary a human can read.
    ToolCallStarted {
        /// The tool call item.
        item: ItemId,
        /// One or two lines describing the call.
        summary: ToolCallSummary,
    },
    /// A running tool produced output.
    ToolCallProgress {
        /// The tool call item.
        item: ItemId,
        /// The increment.
        chunk: String,
    },
    /// The provider rate-limited a request; the adapter is retrying after the delay shown.
    ///
    /// Notifies the frontend so it can render a countdown (docs/design/llm.md §6). Carried by the
    /// adapter's `RateLimited` stream event, which the kernel forwards without interpreting it.
    RateLimited {
        /// How long until the adapter retries, in milliseconds.
        retry_after_ms: u64,
    },
    /// The turn is paused until somebody answers.
    ApprovalNeeded {
        /// Echo this back with the decision.
        request_id: ApprovalId,
        /// What is being asked, and which options to offer.
        request: ApprovalRequest,
    },
    /// The turn is over, successfully or not.
    TurnEnded {
        /// Which turn.
        turn: TurnId,
        /// How it ended.
        completion: TurnCompletion,
    },
}

impl KernelEvent {
    /// True for the events the kernel must not coalesce or reorder when it fans out.
    #[must_use]
    pub const fn is_control(&self) -> bool {
        !matches!(self, Self::TextDelta { .. } | Self::ReasoningDelta { .. })
    }

    /// True when the event ends the turn.
    #[must_use]
    pub const fn ends_turn(&self) -> bool {
        matches!(self, Self::TurnEnded { .. })
    }

    /// A short name for logs and test failures.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::TurnStarted { .. } => "turn_started",
            Self::StateChanged { .. } => "state_changed",
            Self::ItemStarted { .. } => "item_started",
            Self::TextDelta { .. } => "text_delta",
            Self::ReasoningDelta { .. } => "reasoning_delta",
            Self::ItemFinished { .. } => "item_finished",
            Self::ToolCallStarted { .. } => "tool_call_started",
            Self::ToolCallProgress { .. } => "tool_call_progress",
            Self::RateLimited { .. } => "rate_limited",
            Self::ApprovalNeeded { .. } => "approval_needed",
            Self::TurnEnded { .. } => "turn_ended",
        }
    }
}

/// Where the kernel's events go.
///
/// Infallible on purpose: the daemon's sink hands events to the live hub and the store writer, and
/// a failure there is the daemon's problem to surface — making the kernel handle it would mean
/// deciding what a half-failed turn looks like, which is exactly the kind of policy the kernel
/// does not own.
///
/// One sink per session, so events do not have to carry the session id.
#[async_trait]
pub trait EventSink: Send + Sync {
    /// Reports an event. Awaited, so a slow consumer applies backpressure instead of losing
    /// events.
    async fn emit(&self, event: KernelEvent);
}
