//! The tool seam: the narrow interface the kernel knows.
//!
//! `Tool`, `ToolCtx` and the filesystem/terminal/approval backends live in `hatchery-capabilities`
//! (L2). If the kernel named any of them it would depend upwards and cycle, so the whole tool
//! layer is hidden behind these three methods (M0a correction, `docs/architecture.md` §3).

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use hatchery_protocol::{ApprovalRequest, Checkpoint, ToolCallSummary, ToolOutput, ToolProgress};

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

/// Where one call's undo points are collected, owned by the kernel and lent to the tool host.
///
/// The obvious shape is a `Vec<Checkpoint>` field on [`ToolInvocation`], and that is what D13
/// originally specified. It does not survive cancellation, and cancellation is ordinary: the
/// kernel's tool select is cancel-first, so an interrupted invocation is **dropped without ever
/// being polled again** (the fact is already load-bearing in `hatchery-testkit`'s fake host, which
/// needs a drop guard to observe it at all). Anything the call was going to return dies with it —
/// while the writes it already made to the user's workspace do not. A cancelled `write_file` would
/// then leave a half-written file that no item points at, and a Code rewind aimed at that call
/// would scan forward past it and restore a state that *includes* the damage.
///
/// Handing the host a shared collector instead means the kernel keeps the checkpoints whether or
/// not the call ever returns, which is the only arrangement in which "interrupt" and "undoable"
/// can both be true.
#[derive(Clone, Debug, Default)]
pub struct CheckpointCollector {
    inner: Arc<Mutex<Vec<Checkpoint>>>,
}

impl CheckpointCollector {
    /// An empty collector.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one undo point, in the order the writes happened.
    pub fn push(&self, checkpoint: Checkpoint) {
        self.slots().push(checkpoint);
    }

    /// Takes everything collected so far, leaving the collector empty.
    #[must_use]
    pub fn drain(&self) -> Vec<Checkpoint> {
        std::mem::take(&mut *self.slots())
    }

    /// True when nothing has been collected — every read-only call, and every session without a
    /// shadow repository.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots().is_empty()
    }

    /// How many undo points are waiting.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots().len()
    }

    /// Pushing cannot leave the vector half-updated, so a panic somewhere else in the process is
    /// not a reason to lose the undo points collected before it.
    fn slots(&self) -> MutexGuard<'_, Vec<Checkpoint>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
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
    /// `checkpoints` is the kernel's, lent for the duration of the call: every undo point a write
    /// takes belongs in it, pushed as the write happens. See [`CheckpointCollector`] for why this
    /// is a shared collector rather than a field on the returned invocation.
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
        checkpoints: CheckpointCollector,
    ) -> Result<ToolInvocation, KernelError>;
}
