//! The local PTY backend: `portable-pty` plus the containment the library does not give.
//!
//! D10's probe (`spikes/pty/measured-linux.txt`) settled what the wrapper has to do; each of
//! these is a measured fact, not a precaution:
//!
//! * `CommandBuilder` defaults its working directory to `$HOME`, so every spawn sets it
//!   explicitly. `None` means the workspace root, and a `..` or an absolute path is refused.
//! * The terminal driver rewrites LF to CRLF (ONLCR). The reader normalises it back, including
//!   when the pair is split across two `read`s.
//! * The library's `kill()` signals one pid. A shell's background work survives it in 3 of the
//!   5 probe scenarios, so this backend kills the **process group** instead. A descendant that
//!   calls `setsid` escapes any process-group signal; that limit is documented rather than
//!   papered over (it is the same limit codex hits without a cgroup).
//! * A reader never sees EOF while the parent holds the slave, so the slave is dropped right
//!   after the spawn. A grandchild can still hold it open afterwards, so `wait` returns when
//!   the **child** exits and only waits a bounded grace for the stream to finish.
//! * The kernel buffer is 4095 bytes and a stalled consumer freezes the child, so the reader
//!   thread always drains into a bounded capture; `next_chunk` is a progress feed, not the
//!   thing keeping the process alive.
//!
//! Windows is deliberately absent: the Windows column of D10 is deferred until after the TUI
//! (2026-10-21 ruling), because containment there needs a Job Object and portable-pty 0.9 has
//! none. [`LocalPty`] refuses on non-unix instead of pretending.

use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::terminal::{TermError, TerminalBackend, TerminalHandle, TerminalSpec};

/// A PTY backend rooted at a workspace.
pub struct LocalPty {
    root: PathBuf,
}

impl LocalPty {
    /// Binds the backend to a workspace root.
    ///
    /// The root is canonicalised once, so a symlinked workspace and its real path resolve to the
    /// same confinement boundary; a `cwd` argument can never escape it afterwards.
    ///
    /// # Errors
    ///
    /// [`TermError::Io`] when the root does not exist or cannot be resolved.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, TermError> {
        let root = root.into();
        let root = std::fs::canonicalize(&root)
            .map_err(|error| TermError::Io(format!("workspace {}: {error}", root.display())))?;
        Ok(Self { root })
    }
}

#[async_trait]
impl TerminalBackend for LocalPty {
    async fn create(
        &self,
        spec: TerminalSpec,
        cancel: CancellationToken,
    ) -> Result<Box<dyn TerminalHandle>, TermError> {
        #[cfg(unix)]
        {
            unix::create(&self.root, spec, cancel).await
        }
        #[cfg(not(unix))]
        {
            let _ = (spec, cancel);
            Err(TermError::Unavailable(
                "local PTY support is not implemented on this platform",
            ))
        }
    }
}

fn resolve_cwd(root: &Path, cwd: Option<&str>) -> Result<PathBuf, TermError> {
    let relative = cwd.unwrap_or(".");
    let path = Path::new(relative);
    if path.is_absolute() {
        return Err(TermError::Io(format!(
            "working directory {relative:?} must be workspace-relative"
        )));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(TermError::Io(format!(
            "working directory {relative:?} may not leave the workspace"
        )));
    }
    let joined = root.join(path);
    let resolved = std::fs::canonicalize(&joined).map_err(|error| {
        TermError::Io(format!("working directory {}: {error}", joined.display()))
    })?;
    if !resolved.starts_with(root) {
        return Err(TermError::Io(format!(
            "working directory {} resolves outside the workspace",
            joined.display()
        )));
    }
    Ok(resolved)
}

#[cfg(unix)]
mod unix {
    use std::collections::VecDeque;
    use std::io::Read;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};

    use async_trait::async_trait;
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;
    use portable_pty::{Child, CommandBuilder, PtySize, native_pty_system};
    use tokio::sync::{Mutex as AsyncMutex, Notify, OnceCell, mpsc, watch};
    use tokio_util::sync::CancellationToken;

    use super::resolve_cwd;
    use crate::terminal::{TermError, TerminalHandle, TerminalOutcome, TerminalSpec};

    const ROWS: u16 = 24;
    const COLS: u16 = 80;
    /// The first megabyte and the last seven are kept; anything between is counted and dropped.
    /// A build log's tail explains a failure, its head explains the command.
    const HEAD_CAP: usize = 1024 * 1024;
    const TAIL_CAP: usize = 7 * 1024 * 1024;
    /// How long `wait` gives the reader to see EOF after the child exited. A background
    /// grandchild holding the pty can keep it open forever; this is what stops that from
    /// becoming a hung tool.
    const EOF_GRACE: std::time::Duration = std::time::Duration::from_millis(250);
    /// How often the control thread polls the child. It owns both the reaping and the
    /// signalling, which is what makes "kill after the pid was reaped and reused" impossible:
    /// the group signal is only ever sent while `try_wait` says the child is still running.
    const POLL: std::time::Duration = std::time::Duration::from_millis(20);

    pub(super) async fn create(
        root: &std::path::Path,
        spec: TerminalSpec,
        cancel: CancellationToken,
    ) -> Result<Box<dyn TerminalHandle>, TermError> {
        if spec.command.is_empty() {
            return Err(TermError::Io("empty command".to_owned()));
        }
        let cwd = resolve_cwd(root, spec.cwd.as_deref())?;

        let mut builder = CommandBuilder::new(&spec.command);
        for arg in &spec.args {
            builder.arg(arg);
        }
        builder.cwd(cwd);
        for (key, value) in &spec.env {
            builder.env(key, value);
        }

        let pair = native_pty_system()
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| TermError::Io(format!("openpty: {error}")))?;
        let child = pair
            .slave
            .spawn_command(builder)
            .map_err(|error| TermError::Io(format!("spawn {}: {error}", spec.command)))?;
        let pid = child
            .process_id()
            .ok_or_else(|| TermError::Io("the spawned process has no pid".to_owned()))?;
        let pgid = pair.master.process_group_leader().unwrap_or(pid as i32);
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| TermError::Io(format!("pty reader: {error}")))?;
        // Never hold the slave: while the parent keeps it open the reader cannot see EOF (probe
        // M4). The master handle is not needed after the reader duplicated its fd.
        drop(pair.slave);
        drop(pair.master);

        let capture = Arc::new(Mutex::new(Capture::default()));
        let (chunk_tx, chunk_rx) = mpsc::channel::<String>(256);
        let reader_done = Arc::new(Notify::new());
        let reader_finished = Arc::new(AtomicBool::new(false));
        spawn_reader(
            reader,
            Arc::clone(&capture),
            chunk_tx,
            Arc::clone(&reader_done),
            Arc::clone(&reader_finished),
        );

        let (exit_tx, exit_rx) = watch::channel::<Option<Exit>>(None);
        let (kill_tx, kill_rx) = std::sync::mpsc::channel::<()>();
        spawn_control(child, pgid, kill_rx, exit_tx);

        let handle = Arc::new(LocalPtyHandle {
            capture,
            chunks: AsyncMutex::new(chunk_rx),
            exit: exit_rx,
            outcome: OnceCell::new(),
            reader_done,
            reader_finished,
            kill_tx: Mutex::new(kill_tx),
            watcher: Mutex::new(None),
        });
        // Cancellation must end the process even when the drop path is not the one that runs.
        // The watcher upgrades a weak handle, so a call that ends normally leaves no task behind;
        // the abort handle is also used by `LocalPtyHandle::drop`.
        if !cancel.is_cancelled() {
            let weak = Arc::downgrade(&handle);
            let task = tokio::spawn(async move {
                cancel.cancelled().await;
                if let Some(handle) = weak.upgrade() {
                    handle.kill();
                }
            });
            *handle
                .watcher
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(task.abort_handle());
        }
        Ok(Box::new(handle))
    }

    struct LocalPtyHandle {
        capture: Arc<Mutex<Capture>>,
        chunks: AsyncMutex<mpsc::Receiver<String>>,
        exit: watch::Receiver<Option<Exit>>,
        outcome: OnceCell<Result<TerminalOutcome, TermError>>,
        reader_done: Arc<Notify>,
        reader_finished: Arc<AtomicBool>,
        kill_tx: Mutex<std::sync::mpsc::Sender<()>>,
        watcher: Mutex<Option<tokio::task::AbortHandle>>,
    }

    impl LocalPtyHandle {
        fn kill(&self) {
            // A closed channel means the control thread already reaped the child; sending then
            // is a no-op by construction, and no signal can target a reused pid.
            let _ = self
                .kill_tx
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .send(());
        }
    }

    impl Drop for LocalPtyHandle {
        fn drop(&mut self) {
            self.kill();
            if let Some(watcher) = self
                .watcher
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                watcher.abort();
            }
        }
    }

    #[async_trait]
    impl TerminalHandle for Arc<LocalPtyHandle> {
        async fn next_chunk(&self) -> Option<String> {
            self.chunks.lock().await.recv().await
        }

        async fn wait(&self) -> Result<TerminalOutcome, TermError> {
            let result = self
                .outcome
                .get_or_init(|| async {
                    let mut exit = self.exit.clone();
                    loop {
                        if exit.borrow().is_some() {
                            break;
                        }
                        if exit.changed().await.is_err() {
                            break;
                        }
                    }
                    let status = exit.borrow().clone();
                    // Give the reader a bounded moment to append what the process wrote just
                    // before exiting. A grandchild can hold the pty open forever; the grace is
                    // what keeps that from becoming a hang.
                    if !self.reader_finished.load(Ordering::SeqCst) {
                        let _ = tokio::time::timeout(EOF_GRACE, self.reader_done.notified()).await;
                    }
                    let (output, dropped_bytes) = self
                        .capture
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .render();
                    match status {
                        Some(Exit { code }) => Ok(TerminalOutcome {
                            output,
                            exit_code: code,
                            dropped_bytes,
                        }),
                        None => Err(TermError::Io(
                            "the control thread ended without an exit status".to_owned(),
                        )),
                    }
                })
                .await;
            result.clone()
        }

        fn kill(&self) {
            LocalPtyHandle::kill(self);
        }

        fn release(&self) {
            LocalPtyHandle::kill(self);
        }
    }

    #[derive(Clone, Debug)]
    struct Exit {
        code: Option<i32>,
    }

    fn spawn_reader(
        mut reader: Box<dyn Read + Send>,
        capture: Arc<Mutex<Capture>>,
        tx: mpsc::Sender<String>,
        done: Arc<Notify>,
        finished: Arc<AtomicBool>,
    ) {
        std::thread::Builder::new()
            .name("pty-reader".to_owned())
            .spawn(move || {
                let mut decoder = Utf8Decoder::default();
                let mut normalizer = Normalizer::default();
                let mut scratch = [0_u8; 8192];
                loop {
                    match reader.read(&mut scratch) {
                        Ok(0) => break,
                        Ok(count) => {
                            let text = decoder.push(&scratch[..count]);
                            let text = normalizer.push(&text);
                            if !text.is_empty() {
                                capture
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .push(&text);
                                // A slow consumer blocks this thread, which is exactly the
                                // backpressure the kernel buffer applies (4095 bytes). The
                                // capture above already has the bytes, so progress is the only
                                // thing that waits — never the final output.
                                if tx.blocking_send(text).is_err() {
                                    break;
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
                let tail = normalizer.finish();
                if !tail.is_empty() {
                    capture
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(&tail);
                    let _ = tx.blocking_send(tail);
                }
                finished.store(true, Ordering::SeqCst);
                done.notify_one();
                // Dropping `tx` closes the channel: `next_chunk` yields None.
            })
            .expect("a reader thread");
    }

    fn spawn_control(
        mut child: Box<dyn Child + Send + Sync>,
        pgid: i32,
        kill_rx: std::sync::mpsc::Receiver<()>,
        exit_tx: watch::Sender<Option<Exit>>,
    ) {
        std::thread::Builder::new()
            .name("pty-control".to_owned())
            .spawn(move || {
                let mut signalled = false;
                loop {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            let code = if status.signal().is_some() {
                                None
                            } else {
                                i32::try_from(status.exit_code()).ok()
                            };
                            let _ = exit_tx.send(Some(Exit { code }));
                            break;
                        }
                        Ok(None) => {}
                        Err(_) => {
                            let _ = exit_tx.send(Some(Exit { code: None }));
                            break;
                        }
                    }
                    match kill_rx.recv_timeout(POLL) {
                        Ok(()) => {
                            if !signalled {
                                signalled = true;
                                // `killpg(0)` would signal the daemon's own group. A pgid at or
                                // below 1 is not a group this child owns.
                                if pgid > 1 {
                                    let _ = killpg(Pid::from_raw(pgid), Signal::SIGKILL);
                                } else {
                                    let _ = child.kill();
                                }
                            }
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            // The handle is gone. Its drop sends the kill before dropping the
                            // sender; keep polling so the child is still reaped.
                        }
                    }
                }
            })
            .expect("a control thread");
    }

    /// A bounded capture: the head and tail of the stream, plus a count of what was dropped.
    #[derive(Default)]
    struct Capture {
        head: String,
        tail: VecDeque<u8>,
        dropped: u64,
    }

    impl Capture {
        fn push(&mut self, text: &str) {
            let mut rest = text;
            if self.head.len() < HEAD_CAP {
                let room = HEAD_CAP - self.head.len();
                let take = floor_boundary(rest, room);
                self.head.push_str(&rest[..take]);
                rest = &rest[take..];
            }
            if rest.is_empty() {
                return;
            }
            self.tail.extend(rest.as_bytes());
            self.trim();
        }

        fn trim(&mut self) {
            while self.tail.len() > TAIL_CAP {
                let excess = self.tail.len() - TAIL_CAP;
                for _ in 0..excess {
                    if self.tail.pop_front().is_some() {
                        self.dropped += 1;
                    }
                }
                // Start the tail on a character boundary: dropping a lead byte would render a
                // replacement character that was never in the output.
                while let Some(&byte) = self.tail.front() {
                    if byte & 0xC0 == 0x80 {
                        self.tail.pop_front();
                        self.dropped += 1;
                    } else {
                        break;
                    }
                }
            }
        }

        fn render(&mut self) -> (String, u64) {
            let tail = String::from_utf8_lossy(self.tail.make_contiguous());
            if self.dropped == 0 {
                (format!("{}{tail}", self.head), 0)
            } else {
                (
                    format!("{}\n… {} bytes dropped …\n{tail}", self.head, self.dropped),
                    self.dropped,
                )
            }
        }
    }

    fn floor_boundary(text: &str, maximum: usize) -> usize {
        let mut index = maximum.min(text.len());
        while index > 0 && !text.is_char_boundary(index) {
            index -= 1;
        }
        index
    }

    /// Keeps an incomplete UTF-8 sequence until the bytes that complete it arrive.
    #[derive(Default)]
    struct Utf8Decoder {
        pending: Vec<u8>,
    }

    impl Utf8Decoder {
        fn push(&mut self, bytes: &[u8]) -> String {
            self.pending.extend_from_slice(bytes);
            let mut out = String::new();
            loop {
                let (valid, invalid) = match std::str::from_utf8(&self.pending) {
                    Ok(text) => {
                        out.push_str(text);
                        self.pending.clear();
                        break;
                    }
                    Err(error) => (error.valid_up_to(), error.error_len()),
                };
                if valid > 0 {
                    out.push_str(std::str::from_utf8(&self.pending[..valid]).expect("valid_up_to"));
                }
                match invalid {
                    Some(length) => {
                        out.push('\u{FFFD}');
                        self.pending.drain(..valid + length);
                    }
                    None => {
                        self.pending.drain(..valid);
                        break;
                    }
                }
            }
            out
        }
    }

    /// Rewrites CRLF to LF, holding a trailing CR until the next chunk proves whether an LF
    /// follows. The probe measured that ONLCR makes this necessary; a split across two reads
    /// makes the holding necessary too.
    #[derive(Default)]
    struct Normalizer {
        pending_cr: bool,
    }

    impl Normalizer {
        fn push(&mut self, text: &str) -> String {
            let mut out = String::with_capacity(text.len());
            let mut chars = text.chars().peekable();
            if self.pending_cr {
                self.pending_cr = false;
                if chars.peek() == Some(&'\n') {
                    chars.next();
                    out.push('\n');
                } else {
                    out.push('\r');
                }
            }
            while let Some(character) = chars.next() {
                if character == '\r' {
                    match chars.peek() {
                        Some('\n') => {
                            chars.next();
                            out.push('\n');
                        }
                        Some(_) => out.push('\r'),
                        None => self.pending_cr = true,
                    }
                } else {
                    out.push(character);
                }
            }
            out
        }

        fn finish(&mut self) -> String {
            if std::mem::take(&mut self.pending_cr) {
                "\r".to_owned()
            } else {
                String::new()
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn crlf_is_normalised_even_when_split_across_chunks() {
            let mut normalizer = Normalizer::default();
            assert_eq!(normalizer.push("a\r"), "a");
            assert_eq!(normalizer.push("\nb\r\nc"), "\nb\nc");
            assert_eq!(normalizer.finish(), "");
        }

        #[test]
        fn a_lone_carriage_return_survives() {
            let mut normalizer = Normalizer::default();
            assert_eq!(normalizer.push("a\rb"), "a\rb");
            assert_eq!(normalizer.push("\r"), "");
            assert_eq!(normalizer.finish(), "\r");
        }

        #[test]
        fn a_split_utf8_sequence_is_decoded_once_it_completes() {
            let bytes = "héllo".as_bytes();
            let split = 2; // inside the two-byte é
            let mut decoder = Utf8Decoder::default();
            let first = decoder.push(&bytes[..split]);
            let second = decoder.push(&bytes[split..]);
            assert_eq!(format!("{first}{second}"), "héllo");
        }

        #[test]
        fn invalid_utf8_becomes_one_replacement_character() {
            let mut decoder = Utf8Decoder::default();
            assert_eq!(decoder.push(&[b'a', 0xFF, b'b']), "a\u{FFFD}b");
        }

        #[test]
        fn the_capture_keeps_the_head_and_tail_and_counts_the_middle() {
            let mut capture = Capture {
                head: "H".repeat(HEAD_CAP),
                ..Capture::default()
            };
            capture.push(&"t".repeat(TAIL_CAP + 10));
            let (rendered, dropped) = capture.render();
            assert_eq!(dropped, 10);
            assert!(rendered.starts_with('H'));
            assert!(rendered.contains("10 bytes dropped"));
            assert!(rendered.ends_with('t'));
        }

        #[test]
        fn a_short_stream_is_returned_verbatim() {
            let mut capture = Capture::default();
            capture.push("hello ");
            capture.push("world\n");
            assert_eq!(capture.render(), ("hello world\n".to_owned(), 0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_parent_directory_in_the_working_directory_is_refused() {
        let root = std::env::temp_dir();
        let error = resolve_cwd(&root, Some("../etc")).expect_err("refused");
        assert!(error.to_string().contains("may not leave"), "{error}");
    }

    #[test]
    fn an_absolute_working_directory_is_refused() {
        let root = std::env::temp_dir();
        let error = resolve_cwd(&root, Some("/etc")).expect_err("refused");
        assert!(error.to_string().contains("workspace-relative"), "{error}");
    }
}
