//! The terminal seam: process execution behind an interface.
//!
//! A PTY is **one device**, so a process started on one does not have separately capturable
//! stdout and stderr: both are written to the same slave, and the master reads them back
//! interleaved. The shape below therefore carries one merged `output` rather than the two fields
//! the M1 placeholder had. Two consequences are load-bearing and the D10 probe measured both
//! (see `spikes/pty/measured-linux.txt`):
//!
//! * the terminal driver's ONLCR turns every `\n` the child writes into `\r\n`, so captured
//!   output is **not** byte-identical to the child's stdout. [`LocalPty`](crate::LocalPty)
//!   normalises CRLF back to LF at the reader, and that normalisation is part of the contract,
//!   not a test convenience.
//! * the library's `Child::kill()` signals **one pid**. A shell that started background work
//!   leaves it behind. `LocalPty` kills the whole process group, and `release` is documented as
//!   the same operation because a PTY cannot hand back a live process nobody can observe.

use std::collections::BTreeMap;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

/// Why a terminal operation failed.
#[derive(Clone, Debug, thiserror::Error)]
pub enum TermError {
    /// The backend cannot run processes at all (a chat session with no terminal bound, or a
    /// platform whose containment story is not implemented yet).
    #[error("no terminal in this session: {0}")]
    Unavailable(&'static str),
    /// The host refused or lost the process.
    #[error("terminal: {0}")]
    Io(String),
}

/// A process to start.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalSpec {
    /// The program, resolved on the host.
    pub command: String,
    /// Arguments, in order.
    pub args: Vec<String>,
    /// Working directory, workspace-relative like every path at this seam. `None` means the
    /// workspace root. A backend must set this explicitly: `portable-pty`'s `CommandBuilder`
    /// defaults to `$HOME`, which a probe caught before it could surprise a user.
    pub cwd: Option<String>,
    /// Extra environment.
    pub env: BTreeMap<String, String>,
}

/// A running process.
///
/// Every method takes `&self` on purpose: the consumer has to watch for output **and** for the
/// process exiting in one `select!`, and two `&mut self` methods cannot be polled concurrently.
/// Implementations put their state behind interior mutability; none of the calls block the
/// runtime (the blocking parts live on the backend's own threads).
///
/// `wait` is idempotent: it may be called after it already returned, from the same or another
/// task, and yields the same outcome. It resolves when the process has exited, **not** when the
/// output stream reaches EOF — a background grandchild can keep the slave open forever, and a
/// caller waiting for EOF would hang on a process that is already gone.
#[async_trait]
pub trait TerminalHandle: Send + Sync {
    /// The next chunk of output, or `None` once the stream ended.
    ///
    /// Chunks are UTF-8 and have CRLF normalised to LF. A chunk boundary is the tool's decision
    /// point for `ToolCallProgress`, not a line boundary: partial lines are normal.
    async fn next_chunk(&self) -> Option<String>;

    /// Waits for the process to end and returns everything it wrote.
    ///
    /// `output` is the captured stream up to the backend's cap; `dropped_bytes` says how much
    /// was discarded to stay inside it, so a caller can tell truncation from a short command.
    ///
    /// # Errors
    ///
    /// [`TermError::Io`] when the host lost the process.
    async fn wait(&self) -> Result<TerminalOutcome, TermError>;

    /// Ends the process and everything in its process group. Idempotent.
    fn kill(&self);

    /// Ends the process and gives up custody.
    ///
    /// On a PTY this is [`Self::kill`]: the probe measured that closing the master and slave
    /// leaves the child alive for at least 500 ms with nobody able to observe or stop it, so
    /// "hand back a live process" would be a lie. `unified_exec`-style session reuse (M5) is
    /// the feature that would give this a different meaning; it will have to change this
    /// method's contract, not just its implementation.
    fn release(&self);
}

/// How a process ended, and what it said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalOutcome {
    /// Everything written to the terminal, in order. On a PTY this is stdout **and** stderr.
    pub output: String,
    /// The exit code, when the process reported one (absent when killed by a signal).
    pub exit_code: Option<i32>,
    /// Bytes discarded because the capture buffer hit its cap.
    pub dropped_bytes: u64,
    /// The process exited before the reader reached EOF (a descendant may still hold the pty).
    ///
    /// `dropped_bytes` cannot express this: nothing overflowed the capture, the stream simply
    /// stopped being read. A caller that reports truncation must report this flag too, or a
    /// partial capture looks like a short command.
    pub stream_incomplete: bool,
}

impl TerminalOutcome {
    /// True when the process exited cleanly.
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.exit_code == Some(0)
    }
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
    /// mode (ADR-0005: read-only tools only). [`TermError::Io`] when the spec names a working
    /// directory outside the backend's root, or the host refuses the spawn.
    async fn create(
        &self,
        spec: TerminalSpec,
        cancel: CancellationToken,
    ) -> Result<Box<dyn TerminalHandle>, TermError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_outcome_knows_when_it_succeeded() {
        let clean = TerminalOutcome {
            output: String::new(),
            exit_code: Some(0),
            dropped_bytes: 0,
            stream_incomplete: false,
        };
        assert!(clean.is_success());
        assert!(
            !TerminalOutcome {
                exit_code: None,
                ..clean.clone()
            }
            .is_success(),
            "killed by a signal is not success"
        );
        assert!(
            !TerminalOutcome {
                exit_code: Some(2),
                ..clean
            }
            .is_success()
        );
    }

    #[test]
    fn the_default_spec_leaves_cwd_to_the_backend() {
        let spec = TerminalSpec::default();
        assert_eq!(spec.cwd, None);
        assert!(spec.args.is_empty());
        assert!(spec.env.is_empty());
    }
}
