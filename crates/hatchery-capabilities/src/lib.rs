//! Capability seam: filesystem, terminal and approval backends, tool registry.
//!
//! Layer **L2** (docs/architecture.md §3). This crate owns the [`Tool`] / [`ToolCtx`] contract
//! and the [`FsBackend`] / [`TerminalBackend`] / [`ApprovalGate`] seams; the kernel only sees
//! the narrow `ToolHost` trait, which keeps it free of any dependency on the capability layer
//! (ADR-0004).
//!
//! A session binds exactly one backend set at `session/new`: local ([`LocalFs`], plus M2's
//! `LocalPty` and `DaemonApproval`) or ACP-delegated (`AcpClientFs` and friends, M3) — the same
//! tools run over either, which is the whole point of the seam.
//!
//! Writes go through [`CheckpointedFs`], a decorator that takes a shadow-git snapshot before each
//! one and collects it in the kernel's [`hatchery_kernel::CheckpointCollector`] — so an agent's edit
//! to a workspace is undoable even when the workspace is not a git repository, and even when it is
//! one that we must not touch (invariant 6). [`CheckpointStore`] is that shadow repository, built on
//! the findings measured in `tests/spike_shadow_git.rs`.
//!
//! The [`ToolRegistry`] is the session's tool table: it advertises the catalogue the model sees
//! and dispatches calls through the seams, implementing the kernel's `ToolHost` — one registry
//! per session, immutable once assembled.
//!
//! # Example
//!
//! Assemble a registry and run a call the way the daemon does:
//!
//! ```
//! # use std::sync::Arc;
//! # use serde_json::json;
//! # use tokio_util::sync::CancellationToken;
//! use hatchery_capabilities::{Backends, LocalFs, ToolRegistry};
//! use hatchery_kernel::ToolHost;
//!
//! # fn with_backends(fs: Arc<dyn hatchery_capabilities::FsBackend>) {
//! let backends = Backends {
//!     fs,
//!     terminal: Arc::new(hatchery_capabilities::NoTerminal),
//!     // `None` is Chat: nothing writes, so there is nothing to checkpoint.
//!     checkpointer: None,
//! };
//! let mut registry = ToolRegistry::new(backends);
//! // registry.register(Arc::new(hatchery_tools::read_file())); — in the daemon's assembly
//! assert!(registry.is_empty(), "an assembled-but-empty registry is a startup audit finding");
//! # let _ = registry;
//! # }
//! ```
//!
//! Design: `docs/design/capabilities.md`.

mod approval;
mod checkpoint;
mod checkpointed_fs;
mod fs;
mod local_fs;
mod registry;
mod terminal;
mod tool;

pub use approval::ApprovalGate;
pub use checkpoint::{
    CheckpointError, CheckpointOptions, CheckpointPool, CheckpointStore, DEFAULT_MAX_FILE_BYTES,
    RestoreOptions, RestoreReport, SnapshotReport,
};
pub use checkpointed_fs::{CheckpointedFs, Checkpointer, PreWrite};
pub use fs::{FsBackend, FsEntry, FsError, FsMetadata};
pub use local_fs::LocalFs;
pub use registry::{Backends, ToolRegistry};
pub use terminal::{
    NoTerminal, TermError, TerminalBackend, TerminalHandle, TerminalOutcome, TerminalSpec,
};
pub use tool::{Tool, ToolCtx, ToolError};
