//! The tool contract: what a tool is, and the seams one may touch.
//!
//! The kernel sees only `ToolHost` (its three methods, frozen per turn); this is the wider
//! interface behind it (docs/design/kernel.md §5, ADR-0004). Tools are pure logic plus these
//! seams — direct `std::fs` in a tool is a compile error, enforced by the deny lint in
//! `hatchery-tools`.

use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use hatchery_protocol::ToolProgress;

use crate::fs::FsBackend;
use crate::terminal::TerminalBackend;

/// Why a tool invocation did not produce a result.
///
/// This is "did not run", not "ran and failed": a tool that ran and has something to tell the
/// model returns `Ok(ToolOutput)` and lets the registry mark it; only failures that stop the
/// call from existing at all come back as an error.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// The arguments do not fit the tool's schema, or name something unusable.
    #[error("invalid arguments: {0}")]
    InvalidArgs(String),
    /// The seam refused (missing file, no terminal, ...).
    #[error("backend refused: {0}")]
    Backend(String),
    /// The user cancelled before the tool finished.
    #[error("cancelled")]
    Cancelled,
}

impl From<crate::fs::FsError> for ToolError {
    fn from(error: crate::fs::FsError) -> Self {
        Self::Backend(error.to_string())
    }
}

impl From<crate::terminal::TermError> for ToolError {
    fn from(error: crate::terminal::TermError) -> Self {
        Self::Backend(error.to_string())
    }
}

/// Everything a tool may touch. The seams *are* the capability boundary: no field, no power.
pub struct ToolCtx<'a> {
    /// The filesystem, workspace-relative.
    pub fs: &'a dyn FsBackend,
    /// Process execution, when the session has a terminal.
    pub terminal: &'a dyn TerminalBackend,
    /// The user cancelled; checked between steps of anything that walks.
    pub cancel: CancellationToken,
    /// Progress chunks for the timeline (surfaced as `ToolCallProgress`).
    pub emit: &'a (dyn Fn(ToolProgress) + Send + Sync),
}

/// One tool.
#[async_trait]
pub trait Tool: Send + Sync {
    /// The catalogue entry the model sees. One per tool, immutable.
    fn def(&self) -> hatchery_kernel::ToolDef;

    /// Whether this specific call needs the user's OK.
    ///
    /// `None` for everything a Chat session can ask for; write tools (M2) answer per arguments.
    fn needs_approval(&self, args: &Value) -> Option<hatchery_protocol::ApprovalRequest>;

    /// A one-line summary of a specific call, for the timeline. The tool owns the meaning;
    /// a generic "name + first argument" fallback lives in [`crate::ToolRegistry`].
    fn summarize(&self, args: &Value) -> hatchery_protocol::ToolCallSummary;

    /// Runs the call.
    ///
    /// # Errors
    ///
    /// [`ToolError`] when the call could not exist — bad arguments, refused seam, cancelled. A
    /// tool that ran and saw something worth reporting returns `Ok` either way; the model reads
    /// the output and decides.
    async fn execute(
        &self,
        ctx: ToolCtx<'_>,
        args: Value,
    ) -> Result<hatchery_protocol::ToolOutput, ToolError>;
}
