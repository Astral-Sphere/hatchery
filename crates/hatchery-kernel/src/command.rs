//! Commands the kernel accepts.

use hatchery_protocol::{ApprovalId, ApprovalOption, Content, TurnId};

/// Something a frontend asks the kernel to do.
///
/// Commands in, events out: the kernel is driven rather than polled, and the daemon is a
/// translator between these and the protocol's methods.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentCommand {
    /// Start a turn with what the user typed.
    ///
    /// Only valid while the kernel is idle. The daemon enforces that with the protocol's
    /// `TurnInProgress` error; a mid-turn input is a caller bug and is dropped with a warning
    /// rather than queued, because a second turn is a thing the store has no record of.
    ///
    /// The turn id travels with the command so that the reply to `session/prompt` names the
    /// same turn the events will carry — the daemon mints it, the kernel honours it.
    TurnInput {
        /// The id every item and terminal event of this turn will carry.
        turn: TurnId,
        /// What the user typed.
        content: Content,
    },
    /// Stop the running turn: cancel the provider stream and any tool using the turn's
    /// cancellation token.
    Interrupt,
    /// Answer a pending approval request.
    ApprovalDecision {
        /// The `request_id` from [`crate::KernelEvent::ApprovalNeeded`].
        request_id: ApprovalId,
        /// Which offered option the user chose.
        option: ApprovalOption,
    },
}

impl AgentCommand {
    /// A turn with text input; the kernel mints the turn id.
    #[must_use]
    pub fn prompt(content: impl Into<Content>) -> Self {
        Self::TurnInput {
            turn: TurnId::new(),
            content: content.into(),
        }
    }

    /// A turn whose id the caller chose — the daemon's shape, so `session/prompt`'s reply can
    /// name the turn its events will carry.
    #[must_use]
    pub fn prompt_with_turn(turn: TurnId, content: impl Into<Content>) -> Self {
        Self::TurnInput {
            turn,
            content: content.into(),
        }
    }

    /// An interrupt.
    #[must_use]
    pub const fn interrupt() -> Self {
        Self::Interrupt
    }

    /// An approval decision.
    #[must_use]
    pub const fn decide(request_id: ApprovalId, option: ApprovalOption) -> Self {
        Self::ApprovalDecision { request_id, option }
    }

    /// A short name for logs and test failures.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::TurnInput { .. } => "turn_input",
            Self::Interrupt => "interrupt",
            Self::ApprovalDecision { .. } => "approval_decision",
        }
    }
}
