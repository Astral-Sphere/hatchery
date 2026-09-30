//! The tool seam: the narrow interface the kernel knows.
//!
//! `Tool`, `ToolCtx` and the filesystem/terminal/approval backends live in `hatchery-capabilities`
//! (L2). If the kernel named any of them it would depend upwards and cycle, so the whole tool
//! layer is hidden behind these three methods (M0a correction, `docs/architecture.md` §3).

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use hatchery_protocol::{ApprovalRequest, ToolCallSummary, ToolOutput, ToolProgress};

use crate::error::KernelError;
use crate::message::ToolDef;

/// What a tool invocation produced.
///
/// `is_error` sits beside the output rather than inside it because both halves are real: a failed
/// call still has text worth reading, and the model must be told which it is.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolInvocation {
    /// What the tool produced.
    pub output: ToolOutput,
    /// True when this reports a failure rather than a result.
    pub is_error: bool,
}

impl ToolInvocation {
    /// A successful invocation.
    #[must_use]
    pub fn ok(output: ToolOutput) -> Self {
        Self {
            output,
            is_error: false,
        }
    }

    /// A failed invocation.
    #[must_use]
    pub fn failed(output: ToolOutput) -> Self {
        Self {
            output,
            is_error: true,
        }
    }
}

/// The tools available to one turn.
#[async_trait]
pub trait ToolHost: Send + Sync {
    /// The tool table, frozen by the kernel when the turn starts.
    ///
    /// Also the source of `tool_defs` in the provider request: the catalogue the model sees and
    /// the catalogue calls are dispatched through must be the same list.
    fn snapshot(&self) -> Vec<ToolDef>;

    /// A one-line description of a call, for the timeline.
    ///
    /// The host's job, not the kernel's: turning JSON arguments into `edit src/main.rs (+12 -3)`
    /// needs to know what the tool means, and the kernel does not.
    fn summarize(&self, name: &str, args: &Value) -> ToolCallSummary;

    /// Whether a call needs the user's approval before it runs.
    ///
    /// `None` means "run it". `Some(request)` makes the kernel pause the turn, emit an
    /// `ApprovalNeeded` event and wait for a decision — the kernel does not know *how* the user is
    /// asked, only that somebody will answer.
    fn approval_for(&self, name: &str, args: &Value) -> Option<ApprovalRequest>;

    /// Runs a tool.
    ///
    /// Progress is reported through `progress` rather than a callback: the kernel has to await
    /// this call *and* forward progress asynchronously while it runs, which a synchronous
    /// callback cannot do.
    ///
    /// # Errors
    ///
    /// Only for failures that stop the loop — the backend is gone, the call could not be
    /// dispatched. A tool that ran and failed returns `Ok` with
    /// [`ToolInvocation::is_error`] set, so the model gets to see what happened.
    async fn invoke(
        &self,
        name: &str,
        args: Value,
        cancel: CancellationToken,
        progress: UnboundedSender<ToolProgress>,
    ) -> Result<ToolInvocation, KernelError>;
}
