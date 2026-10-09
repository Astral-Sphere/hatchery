//! The terminal seam: process execution behind an interface (shape finalised with the M2 tools).

use std::collections::BTreeMap;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

/// Why a terminal operation failed.
#[derive(Debug, thiserror::Error)]
pub enum TermError {
    /// The backend cannot run processes at all (a chat session with no terminal bound).
    #[error("no terminal in this session: {0}")]
    Unavailable(&'static str),
    /// The host refused or lost the process.
    #[error("terminal: {0}")]
    Io(String),
}

/// A process to start.
#[derive(Clone, Debug, Default)]
pub struct TerminalSpec {
    /// The program, resolved on the host.
    pub command: String,
    /// Arguments, in order.
    pub args: Vec<String>,
    /// Working directory, workspace-relative like every path at this seam.
    pub cwd: Option<String>,
    /// Extra environment.
    pub env: BTreeMap<String, String>,
}

/// A running process.
///
/// The M1 shape is the narrowest thing the M2 `shell` tool needs; streaming output and
/// `unified_exec`-style session reuse (docs/design/capabilities.md open question 2) extend it
/// without replacing it.
#[async_trait]
pub trait TerminalHandle: Send + Sync {
    /// Waits for the process to end and returns everything it wrote.
    ///
    /// # Errors
    ///
    /// [`TermError::Io`] when the host lost the process.
    async fn wait(&mut self) -> Result<TerminalOutcome, TermError>;

    /// Ends the process. Idempotent.
    fn kill(&mut self);
}

/// How a process ended, and what it said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalOutcome {
    /// Everything written to stdout, in order.
    pub stdout: String,
    /// Everything written to stderr, in order.
    pub stderr: String,
    /// The exit code, when the process reported one (absent when killed).
    pub exit_code: Option<i32>,
}

/// The terminal a session without one gets: every request refused, by name.
///
/// Chat mode binds this (ADR-0005: read-only tools only), so a model that invents a shell call
/// is told "no terminal in this session" instead of the registry failing to dispatch.
pub struct NoTerminal;

#[async_trait]
impl TerminalBackend for NoTerminal {
    async fn create(
        &self,
        _spec: TerminalSpec,
        _cancel: CancellationToken,
    ) -> Result<Box<dyn TerminalHandle>, TermError> {
        Err(TermError::Unavailable(
            "this session has no terminal backend",
        ))
    }
}

/// The process-execution a tool sees.
#[async_trait]
pub trait TerminalBackend: Send + Sync {
    /// Starts a process. Cancellation ends the process, not just the wait.
    ///
    /// # Errors
    ///
    /// [`TermError::Unavailable`] when this session has no terminal — the default for Chat
    /// mode (ADR-0005: read-only tools only).
    async fn create(
        &self,
        spec: TerminalSpec,
        cancel: CancellationToken,
    ) -> Result<Box<dyn TerminalHandle>, TermError>;
}
