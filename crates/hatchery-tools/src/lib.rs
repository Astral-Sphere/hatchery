//! Built-in agent tools.
//!
//! Layer **L3** (docs/architecture.md §3). Tools implement the `Tool` trait from
//! `hatchery-capabilities` and receive everything they may touch through `ToolCtx`.
//!
//! Invariant 4 is enforced at compile time here: direct `std::fs` / `std::process` use is a
//! clippy error (see the workspace `clippy.toml`). At run time the same invariant is proven by
//! tests that inject only the in-memory backends from `hatchery-testkit`.
//!
//! The M1 set is Chat mode's read-only trio (ADR-0005): [`read_file`], [`glob`], [`grep`]. The
//! pattern engines are ripgrep's `globset` and `regex` — matching only; the walking itself goes
//! through the `FsBackend` seam (decision D6, docs/worklog/capabilities.md), which is what keeps
//! these tools testable against `MemoryFs` and portable to ACP-delegated backends.
//!
//! # Example
//!
//! A tool runs against whatever backends the session bound — no disk in sight:
//!
//! ```
//! use serde_json::json;
//! use tokio_util::sync::CancellationToken;
//! use hatchery_capabilities::{Tool, ToolCtx};
//! use hatchery_testkit::MemoryFs;
//!
//! # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
//! let fs = MemoryFs::new();
//! fs.file("hello.txt", "hi\n");
//! let ctx = ToolCtx {
//!     fs: &fs,
//!     terminal: &hatchery_capabilities::NoTerminal,
//!     cancel: CancellationToken::new(),
//!     emit: &|_| {},
//! };
//! let output = hatchery_tools::read_file()
//!     .execute(ctx, json!({"path": "hello.txt"}))
//!     .await
//!     .expect("reads");
//! assert_eq!(output.text, "1: hi\n");
//! # });
//! ```
//!
//! Design: `docs/design/capabilities.md` §3. Write and shell tools land in M2.

use hatchery_capabilities::Tool;

mod glob;
mod grep;
mod read_file;
mod walk;

pub use glob::Glob;
pub use grep::Grep;
pub use read_file::{DEFAULT_MAX_BYTES, ReadFile};

/// The `read_file` tool, boxed for registry assembly.
#[must_use]
pub fn read_file() -> std::sync::Arc<dyn Tool> {
    std::sync::Arc::new(ReadFile::new())
}

/// The `glob` tool, boxed for registry assembly.
#[must_use]
pub fn glob() -> std::sync::Arc<dyn Tool> {
    std::sync::Arc::new(Glob)
}

/// The `grep` tool, boxed for registry assembly.
#[must_use]
pub fn grep() -> std::sync::Arc<dyn Tool> {
    std::sync::Arc::new(Grep)
}

/// Chat mode's toolset, in registration order (the registry sorts for the snapshot).
///
/// The daemon's assembly calls this; keeping the list beside the tools means a new tool cannot
/// be added without deciding whether it belongs in Chat.
#[must_use]
pub fn chat_tools() -> Vec<std::sync::Arc<dyn Tool>> {
    vec![read_file(), glob(), grep()]
}
