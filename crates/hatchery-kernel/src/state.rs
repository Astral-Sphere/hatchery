//! Turn state, limits and how a turn ends.

use std::sync::Arc;

use hatchery_protocol::{ApprovalId, StopReason, Usage};

use crate::error::KernelError;
use crate::history::HistorySource;
use crate::provider::LlmProvider;
use crate::sink::EventSink;
use crate::tools::ToolHost;

/// Where the turn loop is.
///
/// An explicit enum rather than a tangle of booleans: the transition matrix is a test subject
/// (`docs/design/testing.md` §3.2), and every transition is reported as
/// [`crate::KernelEvent::StateChanged`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnState {
    /// No turn in flight.
    Idle,
    /// Reading history and building the first request.
    Assembling,
    /// Waiting on the provider stream.
    Streaming {
        /// Which round, counting from one.
        round: u32,
    },
    /// Waiting for the user to answer an approval request.
    AwaitingApproval {
        /// Which request is pending.
        request_id: ApprovalId,
    },
    /// Running tools.
    Executing {
        /// Which round's tools are running.
        round: u32,
    },
}

impl TurnState {
    /// True when a turn is in flight.
    #[must_use]
    pub const fn is_active(self) -> bool {
        !matches!(self, Self::Idle)
    }

    /// A short name for logs and test failures.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Assembling => "assembling",
            Self::Streaming { .. } => "streaming",
            Self::AwaitingApproval { .. } => "awaiting_approval",
            Self::Executing { .. } => "executing",
        }
    }
}

/// Fuses that keep a runaway turn from running forever.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnLimits {
    /// How many provider rounds one turn may use. A round is one request plus the tools it
    /// triggered, so a model that keeps calling tools is cut off here.
    pub max_rounds: u32,
}

impl Default for TurnLimits {
    fn default() -> Self {
        Self { max_rounds: 100 }
    }
}

impl TurnLimits {
    /// Limits with an explicit round ceiling.
    #[must_use]
    pub const fn with_max_rounds(max_rounds: u32) -> Self {
        Self { max_rounds }
    }
}

/// How a turn ended.
///
/// A tool that ran and failed is not here: that is a `ToolOutput` with `is_error`, and the turn
/// carries on. This enum is for the two ways a turn stops.
#[derive(Clone, Debug, PartialEq)]
pub enum TurnCompletion {
    /// The loop reached a stop reason.
    Completed {
        /// Why it stopped.
        reason: StopReason,
        /// What the turn cost, accumulated over its rounds.
        usage: Usage,
    },
    /// The loop could not continue.
    Failed {
        /// What stopped it.
        error: KernelError,
        /// What the turn cost before it failed. Empty only when no provider ever answered:
        /// rounds that completed were paid for, and a failed turn is exactly the one whose cost
        /// a retry will pay again.
        usage: Usage,
    },
}

impl TurnCompletion {
    /// The stop reason, when the turn completed.
    #[must_use]
    pub const fn stop_reason(&self) -> Option<StopReason> {
        match self {
            Self::Completed { reason, .. } => Some(*reason),
            Self::Failed { .. } => None,
        }
    }

    /// The accumulated usage. A failed turn keeps whatever its rounds reported before the
    /// failure; it is empty only when nothing was ever reported.
    #[must_use]
    pub const fn usage(&self) -> Usage {
        match self {
            Self::Completed { usage, .. } | Self::Failed { usage, .. } => *usage,
        }
    }

    /// True when the turn completed.
    #[must_use]
    pub const fn is_ok(&self) -> bool {
        matches!(self, Self::Completed { .. })
    }
}

/// Everything the kernel talks to.
///
/// Bundled rather than passed as separate arguments: five of the six things an agent needs are
/// `Arc<dyn …>`, and a constructor that takes them positionally is a mistake waiting to happen.
#[derive(Clone)]
pub struct Ports {
    /// The provider.
    pub provider: Arc<dyn LlmProvider>,
    /// The tools available to this session.
    pub tools: Arc<dyn ToolHost>,
    /// Where history comes from.
    pub history: Arc<dyn HistorySource>,
    /// Where events go.
    pub sink: Arc<dyn EventSink>,
}

impl Ports {
    /// Bundles the four seams.
    #[must_use]
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        tools: Arc<dyn ToolHost>,
        history: Arc<dyn HistorySource>,
        sink: Arc<dyn EventSink>,
    ) -> Self {
        Self {
            provider,
            tools,
            history,
            sink,
        }
    }
}

impl std::fmt::Debug for Ports {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ports").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_fuse_is_finite_and_generous() {
        assert_eq!(TurnLimits::default().max_rounds, 100);
        assert_eq!(TurnLimits::with_max_rounds(1).max_rounds, 1);
    }

    #[test]
    fn only_idle_is_inactive() {
        assert!(!TurnState::Idle.is_active());
        assert!(TurnState::Assembling.is_active());
        assert!(TurnState::Streaming { round: 1 }.is_active());
        assert!(
            TurnState::AwaitingApproval {
                request_id: ApprovalId::new()
            }
            .is_active()
        );
        assert!(TurnState::Executing { round: 2 }.is_active());
    }

    #[test]
    fn a_failed_turn_reports_no_reason_but_keeps_its_usage() {
        let failed = TurnCompletion::Failed {
            error: KernelError::history("gone"),
            usage: Usage::default(),
        };
        assert_eq!(failed.stop_reason(), None);
        assert!(
            failed.usage().is_empty(),
            "a turn that failed before any provider answered cost nothing"
        );
        assert!(!failed.is_ok());

        let spent = Usage {
            prompt_tokens: Some(10),
            completion_tokens: Some(4),
            reasoning_tokens: None,
            requests: 2,
        };
        let failed_late = TurnCompletion::Failed {
            error: KernelError::history("gone"),
            usage: spent,
        };
        assert_eq!(
            failed_late.usage(),
            spent,
            "rounds that completed were paid for; a retry pays again, so the cost must survive"
        );

        let completed = TurnCompletion::Completed {
            reason: StopReason::ModelDone,
            usage: Usage {
                prompt_tokens: Some(1),
                completion_tokens: Some(2),
                reasoning_tokens: None,
                requests: 1,
            },
        };
        assert_eq!(completed.stop_reason(), Some(StopReason::ModelDone));
        assert_eq!(completed.usage().prompt_tokens, Some(1));
        assert!(completed.is_ok());
    }

    #[test]
    fn state_names_are_stable_for_logs() {
        assert_eq!(TurnState::Idle.name(), "idle");
        assert_eq!(TurnState::Streaming { round: 1 }.name(), "streaming");
        assert_eq!(
            TurnState::AwaitingApproval {
                request_id: ApprovalId::new()
            }
            .name(),
            "awaiting_approval"
        );
    }
}
