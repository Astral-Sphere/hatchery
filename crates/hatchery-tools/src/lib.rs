//! Built-in agent tools.
//!
//! Layer **L3** (docs/architecture.md §3). Tools implement the `Tool` trait from
//! `hatchery-capabilities` and receive everything they may touch through `ToolCtx`.
//!
//! Invariant 4 is enforced at compile time here: direct `std::fs` / `std::process` use is a
//! clippy error - the `disallowed_*` lints are denied in this crate's own `[lints]` table (a
//! Cargo lint table is a command-line flag and beats a source attribute; the workspace allows
//! them everywhere else), with the lists in the workspace `clippy.toml`. At run time the same
//! invariant is proven by tests that inject only the in-memory backends from `hatchery-testkit`.
//! tests that inject only the in-memory backends from `hatchery-testkit`.
//!
//! The M1 set is Chat mode's read-only trio (ADR-0005): [`read_file`], [`glob`], [`grep`]. The
//! M2 set adds the write pair — [`write_file`], [`edit`] — which runs in Code mode and asks
//! before every write; shell tools land with the rest of M2. The pattern engines are ripgrep's
//! `globset` and `regex` — matching only; the walking itself goes through the `FsBackend` seam
//! (decision D6, docs/worklog/capabilities.md), which is what keeps these tools testable against
//! `MemoryFs` and portable to ACP-delegated backends.
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

mod edit;
mod glob;
mod grep;
mod hard_gate;
mod preview;
mod read_file;
mod shell;
mod walk;
mod web_fetch;
mod write_file;

pub use edit::Edit;
pub use glob::Glob;
pub use grep::Grep;
pub use read_file::{DEFAULT_MAX_BYTES, ReadFile};
pub use shell::{MAX_TIMEOUT_SECS, Shell};
pub use web_fetch::WebFetch;
pub use write_file::WriteFile;

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

/// The `write_file` tool, boxed for registry assembly.
#[must_use]
pub fn write_file() -> std::sync::Arc<dyn Tool> {
    std::sync::Arc::new(WriteFile::new())
}

/// The `edit` tool, boxed for registry assembly.
#[must_use]
pub fn edit() -> std::sync::Arc<dyn Tool> {
    std::sync::Arc::new(Edit::new())
}

/// The `shell` tool, boxed for registry assembly.
#[must_use]
pub fn shell() -> std::sync::Arc<dyn Tool> {
    std::sync::Arc::new(Shell::new())
}

/// The `web_fetch` tool, boxed for registry assembly.
#[must_use]
pub fn web_fetch() -> std::sync::Arc<dyn Tool> {
    std::sync::Arc::new(WebFetch::new())
}

/// A built-in tool by name, boxed for registry assembly.
///
/// There is no per-tool configuration parameter yet: nothing in the tree assembles tools from a
/// spec (the daemon still calls [`chat_tools`]/[`code_tools`] directly), and an argument every
/// caller ignores is a seam nobody has tested (ADR-0009).
#[must_use]
pub fn builtin(name: &str) -> Option<std::sync::Arc<dyn Tool>> {
    match name {
        "read_file" => Some(read_file()),
        "glob" => Some(glob()),
        "grep" => Some(grep()),
        "write_file" => Some(write_file()),
        "edit" => Some(edit()),
        "shell" => Some(shell()),
        "web_fetch" => Some(web_fetch()),
        _ => None,
    }
}

/// Chat mode's toolset, in registration order (the registry sorts for the snapshot).
///
/// The daemon's assembly calls this; keeping the list beside the tools means a new tool cannot
/// be added without deciding whether it belongs in Chat.
#[must_use]
pub fn chat_tools() -> Vec<std::sync::Arc<dyn Tool>> {
    vec![read_file(), glob(), grep()]
}

/// Code mode's toolset: Chat's trio plus the write pair, the shell and the network fetch.
///
/// Same contract as [`chat_tools`] — the list is the decision about what Code may be shown, and
/// every tool that writes, runs processes or reaches the network is here and nowhere in Chat.
/// `web_fetch` is deliberately absent from Chat: ADR-0005 calls Chat approval-free, and a
/// Network-risk tool there would either hang on an approval nobody can answer or need a second
/// policy (capabilities.md §3 records the deviation).
#[must_use]
pub fn code_tools() -> Vec<std::sync::Arc<dyn Tool>> {
    vec![
        read_file(),
        glob(),
        grep(),
        write_file(),
        edit(),
        shell(),
        web_fetch(),
    ]
}
