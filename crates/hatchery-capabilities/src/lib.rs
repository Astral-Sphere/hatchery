//! Capability seam: filesystem, terminal and approval backends, tool registry.
//!
//! Layer **L2** (docs/architecture.md §3). This crate owns the [`Tool`] / [`ToolCtx`] contract
//! and the [`FsBackend`] / [`TerminalBackend`] / [`ApprovalGate`] seams; the kernel only sees
//! the narrow `ToolHost` trait, which keeps it free of any dependency on the capability layer
//! (ADR-0004).
//!
//! A session binds exactly one backend set at `session/new`: local ([`LocalFs`], plus M2's
//! `LocalPty` and `DaemonApproval`) or ACP-delegated (`AcpClientFs` and friends, M3) — the same
//! tools run over either, which is the whole point of the seam. The M1 subset is read-only:
//! write paths arrive with the shadow-git checkpoints that must precede them (M2), and the
//! shadow-git `CheckpointStore` itself is built on the spike findings in `tests/spike_shadow_git.rs`.
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
mod fs;
mod local_fs;
mod registry;
mod terminal;
mod tool;

pub use approval::ApprovalGate;
pub use fs::{FsBackend, FsEntry, FsError, FsMetadata};
pub use local_fs::LocalFs;
pub use registry::{Backends, ToolRegistry};
pub use terminal::{
    NoTerminal, TermError, TerminalBackend, TerminalHandle, TerminalOutcome, TerminalSpec,
};
pub use tool::{Tool, ToolCtx, ToolError};
