//! Failures the kernel can produce, and how they map onto the wire.

use hatchery_protocol::{ErrorCode, EventError};
use serde::{Deserialize, Serialize};

/// A provider failure.
///
/// `retryable` is the llm layer's verdict (ADR-0007: network errors, 5xx and 429 are retryable;
/// other 4xx are not). The kernel does not retry — it reports, so the frontend can show why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct LlmError {
    /// Human-readable description, including a summary of the provider's error body.
    pub message: String,
    /// Whether retrying the same request could succeed.
    pub retryable: bool,
}

impl LlmError {
    /// A failure that is worth retrying.
    #[must_use]
    pub fn retryable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
        }
    }

    /// A failure that will fail again if repeated.
    #[must_use]
    pub fn fatal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
        }
    }
}

/// Why a turn failed.
///
/// A tool that merely *ran and failed* is not a `KernelError`: its output comes back as a
/// `ToolOutput` with `is_error` set, because the model is supposed to see it. This enum is for
/// failures that stop the loop.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum KernelError {
    /// The provider call failed before or during streaming.
    #[error("provider: {0}")]
    Provider(#[from] LlmError),
    /// Rebuilding the conversation from history failed.
    #[error("history: {0}")]
    History(String),
    /// A tool could not be invoked at all.
    #[error("tool {name}: {message}")]
    Tool {
        /// Which tool.
        name: String,
        /// What went wrong.
        message: String,
    },
}

impl KernelError {
    /// A history failure.
    #[must_use]
    pub fn history(message: impl Into<String>) -> Self {
        Self::History(message.into())
    }

    /// A tool that could not be invoked.
    #[must_use]
    pub fn tool(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Tool {
            name: name.into(),
            message: message.into(),
        }
    }

    /// The same failure in the shape the wire carries.
    ///
    /// Lives here rather than in the daemon: the kernel knows what its own failures mean, and the
    /// mapping is part of the contract a frontend reads.
    #[must_use]
    pub fn to_event_error(&self) -> EventError {
        match self {
            Self::Provider(error) => EventError::llm(error.message.clone(), error.retryable),
            Self::History(message) => EventError::new(ErrorCode::StoreError, message.clone()),
            Self::Tool { name, message } => {
                EventError::new(ErrorCode::InternalError, format!("tool {name}: {message}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_provider_failure_keeps_its_retry_verdict() {
        let error = KernelError::from(LlmError::retryable("429 after 4 attempts"));
        let wire = error.to_event_error();
        assert_eq!(wire.code, ErrorCode::LlmError);
        assert_eq!(wire.retryable, Some(true));

        let fatal = KernelError::from(LlmError::fatal("401 unauthorized"));
        assert_eq!(fatal.to_event_error().retryable, Some(false));
    }

    #[test]
    fn a_history_failure_reads_as_a_store_failure() {
        let wire = KernelError::history("database is locked").to_event_error();
        assert_eq!(wire.code, ErrorCode::StoreError);
        assert_eq!(
            wire.retryable, None,
            "retrying is not a meaningful question"
        );
    }

    #[test]
    fn an_invocation_failure_names_the_tool() {
        let wire = KernelError::tool("shell", "backend gone").to_event_error();
        assert_eq!(wire.code, ErrorCode::InternalError);
        assert!(wire.message.contains("shell"), "{}", wire.message);
    }
}
