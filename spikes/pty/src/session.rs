//! The `portable-pty` call surface the measurements exercise, plus a reader thread.
//!
//! Every process here is started through `SlavePty::spawn_command` and killed through
//! `ChildKiller::kill` — never `std::process::Command`. The observation instruments in
//! [`crate::platform`] are the only non-library OS calls, and they only look.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use portable_pty::{
    Child, ChildKiller, CommandBuilder, MasterPty, PtyPair, PtySize, SlavePty, native_pty_system,
};

use crate::platform::{self, Script};

pub type Res<T> = Result<T, String>;

/// One read() return from the master.
#[derive(Clone, Copy, Debug)]
pub struct Chunk {
    /// Milliseconds since the reader thread started.
    pub t_ms: u128,
    pub len: usize,
}

/// A reader on its own thread, accumulating into a shared buffer.
///
/// It has to be a thread: `MasterPty::try_clone_reader` hands back a blocking
/// `Box<dyn Read + Send>` with no readiness, poll or timeout API.
pub struct Collector {
    buf: Arc<Mutex<Vec<u8>>>,
    chunks: Arc<Mutex<Vec<Chunk>>>,
    finished: Arc<AtomicBool>,
    finish_ms: Arc<Mutex<Option<u128>>>,
    outcome: Arc<Mutex<Option<String>>>,
    start: Instant,
    handle: Option<JoinHandle<()>>,
}

impl Collector {
    fn spawn(reader: Box<dyn Read + Send>) -> Res<Self> {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let chunks = Arc::new(Mutex::new(Vec::new()));
        let finished = Arc::new(AtomicBool::new(false));
        let finish_ms: Arc<Mutex<Option<u128>>> = Arc::new(Mutex::new(None));
        let outcome: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let (t_buf, t_chunks, t_finished, t_finish, t_outcome) = (
            Arc::clone(&buf),
            Arc::clone(&chunks),
            Arc::clone(&finished),
            Arc::clone(&finish_ms),
            Arc::clone(&outcome),
        );
        let start = Instant::now();
        let handle = std::thread::Builder::new()
            .name("pty-reader".to_string())
            .spawn(move || {
                let mut reader = reader;
                let mut scratch = [0u8; 8192];
                loop {
                    match reader.read(&mut scratch) {
                        Ok(0) => {
                            *t_outcome.lock().expect("poisoned") = Some("Ok(0) — EOF".to_string());
                            break;
                        }
                        Ok(n) => {
                            let t_ms = start.elapsed().as_millis();
                            t_buf
                                .lock()
                                .expect("poisoned")
                                .extend_from_slice(&scratch[..n]);
                            t_chunks
                                .lock()
                                .expect("poisoned")
                                .push(Chunk { t_ms, len: n });
                        }
                        Err(e) => {
                            *t_outcome.lock().expect("poisoned") =
                                Some(format!("Err({e}) raw_os={:?}", e.raw_os_error()));
                            break;
                        }
                    }
                }
                *t_finish.lock().expect("poisoned") = Some(start.elapsed().as_millis());
                t_finished.store(true, Ordering::SeqCst);
            })
            .map_err(|e| format!("reader thread spawn failed: {e}"))?;

        Ok(Self {
            buf,
            chunks,
            finished,
            finish_ms,
            outcome,
            start,
            handle: Some(handle),
        })
    }

    /// A collector that reads nothing — used to measure kernel-side backpressure.
    fn idle() -> Self {
        Self {
            buf: Arc::new(Mutex::new(Vec::new())),
            chunks: Arc::new(Mutex::new(Vec::new())),
            finished: Arc::new(AtomicBool::new(true)),
            finish_ms: Arc::new(Mutex::new(None)),
            outcome: Arc::new(Mutex::new(None)),
            start: Instant::now(),
            handle: None,
        }
    }

    pub fn bytes(&self) -> Vec<u8> {
        self.buf.lock().expect("poisoned").clone()
    }

    pub fn len(&self) -> usize {
        self.buf.lock().expect("poisoned").len()
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes()).into_owned()
    }

    pub fn chunks(&self) -> Vec<Chunk> {
        self.chunks.lock().expect("poisoned").clone()
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }

    /// How the reader loop ended, if it ended.
    pub fn outcome(&self) -> Option<String> {
        self.outcome.lock().expect("poisoned").clone()
    }

    /// Milliseconds after reader start at which the loop ended, if it ended.
    pub fn finish_ms(&self) -> Option<u128> {
        *self.finish_ms.lock().expect("poisoned")
    }

    pub fn elapsed_ms(&self) -> u128 {
        self.start.elapsed().as_millis()
    }

    /// Block until `needle` appears in the accumulated bytes; return the rest of that line.
    pub fn wait_for_line_with(&self, needle: &str, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let text = self.text();
            if let Some(line) = text.lines().find(|l| l.contains(needle)) {
                return Some(line.trim().to_string());
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Block until the reader loop ends. `None` means it was still blocked at the deadline.
    pub fn wait_finished(&self, timeout: Duration) -> Option<u128> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.is_finished() {
                return self.finish_ms();
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Detach the thread without joining (the probe must not hang on a stuck reader).
    pub fn detach(&mut self) {
        if let Some(h) = self.handle.take() {
            drop(h);
        }
    }
}

pub struct SpawnOpts {
    /// Keep the parent's copy of the slave fd open. The library's examples keep it,
    /// and it changes when (whether) the reader ever sees EOF — worth measuring.
    pub keep_slave: bool,
    /// Start the reader thread at all.
    pub collect: bool,
}

impl Default for SpawnOpts {
    fn default() -> Self {
        Self {
            keep_slave: false,
            collect: true,
        }
    }
}

pub struct Session {
    /// Declared first so it drops first: closing the parent's slave fd before the master
    /// is what makes the reader see EIO/EOF. `PtyPair` uses the same ordering on purpose.
    pub slave: Option<Box<dyn SlavePty + Send>>,
    pub master: Box<dyn MasterPty + Send>,
    pub child: Box<dyn Child + Send + Sync>,
    pub collector: Collector,
    pub pid: Option<u32>,
    pub started: Instant,
}

impl Session {
    /// A killer that can be moved to another thread while this one blocks in `wait()`.
    pub fn killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        self.child.clone_killer()
    }

    pub fn master_raw_fd(&self) -> Option<i32> {
        #[cfg(unix)]
        {
            self.master.as_raw_fd()
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    pub fn process_group_leader(&self) -> Option<i32> {
        #[cfg(unix)]
        {
            self.master.process_group_leader()
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    pub fn tty_name(&self) -> Option<String> {
        #[cfg(unix)]
        {
            self.master.tty_name().map(|p| p.display().to_string())
        }
        #[cfg(not(unix))]
        {
            None
        }
    }
}

pub fn spawn(script: Script, opts: SpawnOpts) -> Res<Session> {
    let system = native_pty_system();
    let PtyPair { master, slave } = system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("openpty: {e}"))?;

    let (program, args) = platform::command(script);
    let mut builder = CommandBuilder::new(&program);
    for arg in &args {
        builder.arg(arg);
    }
    // Deliberately not calling builder.cwd(...): measurement 3 reports where the child
    // actually lands when the caller does not set one.

    let started = Instant::now();
    let child = slave
        .spawn_command(builder)
        .map_err(|e| format!("spawn_command({program} {args:?}): {e}"))?;
    let reader = master
        .try_clone_reader()
        .map_err(|e| format!("try_clone_reader: {e}"))?;

    let pid = child.process_id();
    if let Some(pid) = pid {
        platform::register_pid(pid);
    }

    let collector = if opts.collect {
        Collector::spawn(reader)?
    } else {
        // Nobody reads: drop the clone so the master fd is the only handle, which is the
        // exact condition the backpressure measurement needs.
        drop(reader);
        Collector::idle()
    };

    let slave = if opts.keep_slave { Some(slave) } else { None };

    Ok(Session {
        slave,
        master,
        child,
        collector,
        pid,
        started,
    })
}
