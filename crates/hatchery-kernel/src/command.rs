//! Commands the kernel accepts.

use hatchery_protocol::{ApprovalId, ApprovalOption, Content};

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
    TurnInput(Content),
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
    /// A turn with text input.
    #[must_use]
    pub fn prompt(content: impl Into<Content>) -> Self {
        Self::TurnInput(content.into())
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
            Self::TurnInput(_) => "turn_input",
            Self::Interrupt => "interrupt",
            Self::ApprovalDecision { .. } => "approval_decision",
        }
    }
}
