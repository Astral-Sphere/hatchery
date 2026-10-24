//! A scripted terminal: the shell tool's unit-test backend (docs/design/testing.md §2).
//!
//! `MemoryFs` is to files what this is to processes: output is scripted, the exit code is
//! chosen, and "hang" is a first-class script element so timeout and cancellation can be tested
//! without depending on how long a real `sleep` takes.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use async_trait::async_trait;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use hatchery_capabilities::{
    TermError, TerminalBackend, TerminalHandle, TerminalOutcome, TerminalSpec,
};

/// What one scripted process does.
#[derive(Clone, Debug)]
pub struct Script {
    /// Chunks, in order. Each becomes one `next_chunk` result.
    pub chunks: Vec<String>,
    /// The exit code. `None` means killed by a signal.
    pub exit_code: Option<i32>,
    /// Keep running after the chunks are exhausted, until somebody kills it.
    pub hang: bool,
}

impl Default for Script {
    fn default() -> Self {
        Self {
            chunks: Vec::new(),
            exit_code: Some(0),
            hang: false,
        }
    }
}

impl Script {
    /// A process that prints these chunks and exits with `code`.
    #[must_use]
    pub fn echo(chunks: impl IntoIterator<Item = impl Into<String>>, code: i32) -> Self {
        Self {
            chunks: chunks.into_iter().map(Into::into).collect(),
            exit_code: Some(code),
            hang: false,
        }
    }

    /// A process that prints these chunks and then hangs.
    #[must_use]
    pub fn hanging(chunks: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            chunks: chunks.into_iter().map(Into::into).collect(),
            exit_code: None,
            hang: true,
        }
    }
}

/// A terminal that hands out one script per call.
#[derive(Default)]
pub struct MemoryTerminal {
    scripts: Mutex<VecDeque<Script>>,
    requests: Mutex<Vec<TerminalSpec>>,
}

impl MemoryTerminal {
    /// An empty terminal; calls get a script that exits 0 with no output.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues the next call's behaviour.
    pub fn script(&self, script: Script) -> &Self {
        self.scripts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(script);
        self
    }

    /// Queues a process that prints `chunks` and exits with `code`.
    pub fn echo(&self, chunks: impl IntoIterator<Item = impl Into<String>>, code: i32) -> &Self {
        self.script(Script::echo(chunks, code))
    }

    /// Queues a process that prints `chunks` and then waits to be killed.
    pub fn hang(&self, chunks: impl IntoIterator<Item = impl Into<String>>) -> &Self {
        self.script(Script::hanging(chunks))
    }

    /// Every spec the backend was asked to create, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<TerminalSpec> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl TerminalBackend for MemoryTerminal {
    async fn create(
        &self,
        spec: TerminalSpec,
        _cancel: CancellationToken,
    ) -> Result<Box<dyn TerminalHandle>, TermError> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(spec);
        let script = self
            .scripts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .unwrap_or_default();
        Ok(Box::new(MemoryHandle {
            pending: Mutex::new(script.chunks.iter().cloned().collect()),
            whole: script.chunks.concat(),
            exit_code: script.exit_code,
            hang: script.hang,
            killed: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            notify: Notify::new(),
            outcome: tokio::sync::OnceCell::new(),
        }))
    }
}

struct MemoryHandle {
    pending: Mutex<VecDeque<String>>,
    whole: String,
    exit_code: Option<i32>,
    hang: bool,
    killed: AtomicBool,
    finished: AtomicBool,
    notify: Notify,
    /// Computed once: a later `kill` must not change an outcome the contract already returned.
    outcome: tokio::sync::OnceCell<TerminalOutcome>,
}

impl MemoryHandle {
    fn settle(&self) {
        self.finished.store(true, Ordering::SeqCst);
        // `notify_one` stores a permit when nobody is waiting; `notify_waiters` would lose a
        // kill that lands between a waiter's check and its first poll of `notified()`.
        self.notify.notify_one();
    }
}

#[async_trait]
impl TerminalHandle for MemoryHandle {
    async fn next_chunk(&self) -> Option<String> {
        loop {
            if let Some(chunk) = self
                .pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front()
            {
                return Some(chunk);
            }
            if !self.hang
                || self.killed.load(Ordering::SeqCst)
                || self.finished.load(Ordering::SeqCst)
            {
                return None;
            }
            // A hanging script must not return None: the caller is supposed to kill it first.
            self.notify.notified().await;
        }
    }

    async fn wait(&self) -> Result<TerminalOutcome, TermError> {
        let outcome = self
            .outcome
            .get_or_init(|| async {
                if self.hang && !self.killed.load(Ordering::SeqCst) {
                    // A permit from `settle` covers a kill that lands between this check and
                    // the first poll of `notified()`.
                    self.notify.notified().await;
                }
                let killed = self.killed.load(Ordering::SeqCst);
                TerminalOutcome {
                    output: self.whole.clone(),
                    exit_code: if killed { None } else { self.exit_code },
                    dropped_bytes: 0,
                    stream_incomplete: false,
                }
            })
            .await;
        Ok(outcome.clone())
    }

    fn kill(&self) {
        if self.killed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.settle();
    }

    fn release(&self) {
        self.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_scripted_process_streams_its_chunks_and_reports_its_code() {
        let terminal = MemoryTerminal::new();
        terminal.echo(["one", "two"], 7);
        let handle = terminal
            .create(TerminalSpec::default(), CancellationToken::new())
            .await
            .expect("a script");
        assert_eq!(handle.next_chunk().await.as_deref(), Some("one"));
        assert_eq!(handle.next_chunk().await.as_deref(), Some("two"));
        assert_eq!(handle.next_chunk().await, None);
        let outcome = handle.wait().await.expect("an outcome");
        assert_eq!(outcome.output, "onetwo");
        assert_eq!(outcome.exit_code, Some(7));
    }

    #[tokio::test]
    async fn a_hanging_process_only_ends_when_killed() {
        let terminal = MemoryTerminal::new();
        terminal.hang(["waiting"]);
        let handle = terminal
            .create(TerminalSpec::default(), CancellationToken::new())
            .await
            .expect("a script");
        assert_eq!(handle.next_chunk().await.as_deref(), Some("waiting"));
        handle.kill();
        assert_eq!(handle.next_chunk().await, None);
        let outcome = handle.wait().await.expect("an outcome");
        assert_eq!(outcome.exit_code, None, "killed by a signal");
    }

    #[tokio::test]
    async fn the_backend_records_the_spec_it_was_given() {
        let terminal = MemoryTerminal::new();
        let spec = TerminalSpec {
            command: "sh".to_owned(),
            args: vec!["-c".to_owned(), "true".to_owned()],
            cwd: Some("sub".to_owned()),
            env: [("A".to_owned(), "1".to_owned())].into(),
        };
        let _ = terminal
            .create(spec.clone(), CancellationToken::new())
            .await
            .expect("a script");
        assert_eq!(terminal.requests(), [spec]);
    }

    #[tokio::test]
    async fn wait_is_idempotent_after_a_late_kill() {
        let terminal = MemoryTerminal::new();
        terminal.echo(["done"], 0);
        let handle = terminal
            .create(TerminalSpec::default(), CancellationToken::new())
            .await
            .expect("a script");
        let first = handle.wait().await.expect("an outcome");
        assert_eq!(first.exit_code, Some(0));
        handle.kill();
        let second = handle.wait().await.expect("an outcome");
        assert_eq!(second, first, "a later kill changed a returned outcome");
    }

    #[tokio::test]
    async fn a_kill_before_the_first_chunk_still_wakes_the_reader() {
        let terminal = MemoryTerminal::new();
        terminal.hang(Vec::<String>::new());
        let handle = terminal
            .create(TerminalSpec::default(), CancellationToken::new())
            .await
            .expect("a script");
        handle.kill();
        assert_eq!(
            handle.next_chunk().await,
            None,
            "the kill woke the hanging reader"
        );
    }
}
