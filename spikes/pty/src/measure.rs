//! The six measurements. Each one prints what it observed, with numbers, and says
//! `HAZARD` when the behaviour would bite a `TerminalBackend` implementation.

use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{Child, ExitStatus, MasterPty};

use crate::platform::{self, Script};
use crate::report as r;
use crate::session::{Collector, Res, Session, SpawnOpts, spawn};

fn describe(status: &ExitStatus) -> String {
    format!(
        "{status} [exit_code={} signal={:?} success={}]",
        status.exit_code(),
        status.signal(),
        status.success()
    )
}

/// Poll `try_wait` on the collector's clock. `None` = still running at the deadline.
fn poll_exit(
    child: &mut Box<dyn Child + Send + Sync>,
    timeout: Duration,
    clock: &Collector,
) -> Option<(u128, String)> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some((clock.elapsed_ms(), describe(&status))),
            Ok(None) => {}
            Err(e) => return Some((clock.elapsed_ms(), format!("try_wait -> Err({e})"))),
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn fmt_alive(a: Option<bool>) -> String {
    match a {
        Some(true) => "ALIVE".to_string(),
        Some(false) => "dead".to_string(),
        None => "unknown (no instrument on this platform)".to_string(),
    }
}

fn ids(pid: u32) -> String {
    let show = |v: Option<u32>| v.map(|v| v.to_string()).unwrap_or_else(|| "?".into());
    format!(
        "pid={pid} pgid={} sid={} kill-0={} /proc-state={:?}",
        show(platform::pid_pgid(pid)),
        show(platform::pid_sid(pid)),
        fmt_alive(platform::pid_alive(pid)),
        platform::proc_state(pid),
    )
}

fn truncate(s: &str, n: usize) -> String {
    let flat = s.replace('\n', "⏎");
    if flat.chars().count() <= n {
        flat
    } else {
        format!("{}…", flat.chars().take(n).collect::<String>())
    }
}

fn master_size(m: &Box<dyn MasterPty + Send>) -> String {
    match m.get_size() {
        Ok(s) => format!(
            "rows={} cols={} px={}x{}",
            s.rows, s.cols, s.pixel_width, s.pixel_height
        ),
        Err(e) => format!("get_size Err({e})"),
    }
}

/// Read on a thread for a bounded time, counting bytes without keeping them.
fn counting_reader(
    reader: Box<dyn Read + Send>,
    budget: Duration,
) -> (Arc<AtomicUsize>, Arc<Mutex<Option<String>>>) {
    let count = Arc::new(AtomicUsize::new(0));
    let outcome: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let (c, o) = (Arc::clone(&count), Arc::clone(&outcome));
    let _ = std::thread::Builder::new()
        .name("pty-drain".to_string())
        .spawn(move || {
            let mut reader = reader;
            let mut buf = vec![0u8; 64 * 1024];
            let t0 = Instant::now();
            loop {
                if t0.elapsed() > budget {
                    *o.lock().expect("poisoned") = Some(format!("budget {budget:?} elapsed"));
                    return;
                }
                match reader.read(&mut buf) {
                    Ok(0) => {
                        *o.lock().expect("poisoned") = Some("EOF (Ok(0))".to_string());
                        return;
                    }
                    Ok(n) => {
                        c.fetch_add(n, Ordering::SeqCst);
                    }
                    Err(e) => {
                        *o.lock().expect("poisoned") =
                            Some(format!("Err({e}) raw_os={:?}", e.raw_os_error()));
                        return;
                    }
                }
            }
        });
    (count, outcome)
}

// ============================================================== M1 streaming

pub fn m1() -> Res<()> {
    r::banner("M1 — streaming output while the child lives");
    r::observed("payload: sh -c 'for i in $(seq 1 20); do echo line-$i; sleep 0.05; done'");

    let mut s = spawn(Script::Stream, SpawnOpts::default())?;
    let child_pid = s.pid.expect("portable-pty gave no process_id");

    let exit = poll_exit(&mut s.child, Duration::from_secs(15), &s.collector);
    let reader_done = s.collector.wait_finished(Duration::from_secs(5));
    let chunks = s.collector.chunks();
    let bytes = s.collector.bytes();
    let text = String::from_utf8_lossy(&bytes).into_owned();

    let (exit_ms, exit_desc) = match &exit {
        Some((ms, d)) => (*ms, d.clone()),
        None => (u128::MAX, "STILL RUNNING at the 15 s deadline".to_string()),
    };
    let before_exit: Vec<_> = chunks.iter().filter(|c| c.t_ms <= exit_ms).collect();
    let bytes_before_exit: usize = before_exit.iter().map(|c| c.len).sum();
    let lines = text.matches("line-").count();
    let lf = bytes.iter().filter(|b| **b == b'\n').count();
    let crlf = text.matches("\r\n").count();

    r::observed(format!("child {}", ids(child_pid)));
    r::observed(format!(
        "reader: {} chunks, {} bytes total; loop ended at {:?} ms with {}",
        chunks.len(),
        bytes.len(),
        reader_done,
        s.collector
            .outcome()
            .unwrap_or_else(|| "<still blocked>".into())
    ));
    r::observed(format!("child exit observed at {exit_ms} ms: {exit_desc}"));
    r::observed(format!(
        "{} of {} chunks ({} of {} bytes) arrived BEFORE the child exited",
        before_exit.len(),
        chunks.len(),
        bytes_before_exit,
        bytes.len()
    ));

    println!();
    println!("          chunk arrival table (t = ms since the reader thread started):");
    let mut offset = 0usize;
    for (i, c) in chunks.iter().enumerate() {
        if i >= 24 {
            println!("            … {} more chunks", chunks.len() - 24);
            break;
        }
        let end = (offset + c.len).min(bytes.len());
        // escape_default once, then print raw: going through {:?} too would double-escape
        // the CRLF that the line discipline inserted.
        let preview = String::from_utf8_lossy(&bytes[offset..end])
            .escape_default()
            .to_string();
        println!(
            "            #{:<3} t={:>5} ms  len={:<5} \"{}\"",
            i + 1,
            c.t_ms,
            c.len,
            truncate(&preview, 40)
        );
        offset = end;
    }

    let sizes: Vec<usize> = chunks.iter().map(|c| c.len).collect();
    let distinct = {
        let mut v = sizes.clone();
        v.sort_unstable();
        v.dedup();
        v.len()
    };
    r::observed(format!(
        "chunk sizes: min={} max={} distinct={} (payload wrote {lines} lines)",
        sizes.iter().min().unwrap_or(&0),
        sizes.iter().max().unwrap_or(&0),
        distinct,
    ));
    r::observed(format!(
        "line discipline: LF bytes = {lf}, CRLF sequences = {crlf} (the payload wrote LF only)"
    ));

    if !before_exit.is_empty() && reader_done.is_some() {
        r::verdict(format!(
            "STREAMING WORKS. {} chunks / {} bytes reached the master while the child was \
             still alive, and the reader returned {} after exit. Granularity is arbitrary \
             read() chunks, not lines — {} distinct sizes across {} chunks — so line \
             splitting and any ring buffer must live in our code, not the library's.",
            before_exit.len(),
            bytes_before_exit,
            match reader_done {
                Some(ms) => format!("EOF at {ms} ms"),
                None => "never".to_string(),
            },
            distinct,
            chunks.len()
        ));
    } else {
        r::verdict(format!(
            "NO streaming observed: {} chunks before exit, reader_done={reader_done:?}",
            before_exit.len()
        ));
    }
    if crlf > 0 {
        r::hazard(format!(
            "the PTY translates LF -> CRLF (ONLCR): {crlf} CRLF sequences for {lf} LF bytes. \
             Output captured through a PTY is NOT byte-identical to the child's stdout, so \
             TerminalOutcome, ToolCallProgress payloads and any golden test must normalise \
             CRLF. portable-pty passes termios = NULL to openpty(), i.e. it never disables \
             the translation and offers no API to change it (get_termios() is read-only).",
        ));
    }
    r::hazard(
        "the reader MUST live on its own thread: MasterPty::try_clone_reader() returns a \
         blocking Box<dyn Read + Send> with no readiness, poll or timeout API, and the unix \
         backend has no async support (the crate's async examples wrap the raw fd in smol's \
         Async). A tokio TerminalBackend needs spawn_blocking + a channel, or \
         tokio::io::unix::AsyncFd over MasterPty::as_raw_fd().",
    );

    m1b()?;
    Ok(())
}

fn m1b() -> Res<()> {
    r::sub("M1b — same child, but the parent keeps its copy of the slave fd");
    let mut s = spawn(
        Script::Stream,
        SpawnOpts {
            keep_slave: true,
            collect: true,
        },
    )?;
    let exit = poll_exit(&mut s.child, Duration::from_secs(15), &s.collector);
    let done_while_slave_open = s.collector.wait_finished(Duration::from_millis(1500));
    r::observed(format!(
        "child pid={} exited at {:?} ms; reader loop finished while the parent's slave fd \
         was still open: {:?}",
        s.pid.unwrap_or(0),
        exit.map(|(ms, _)| ms),
        done_while_slave_open
    ));
    r::observed(format!(
        "reader outcome at that point: {:?}",
        s.collector
            .outcome()
            .unwrap_or_else(|| "<still blocked in read()>".into())
    ));
    s.slave = None; // close the parent's slave fd
    let done_after = s.collector.wait_finished(Duration::from_millis(1500));
    r::observed(format!(
        "after dropping the parent's slave fd the reader finished at {done_after:?} ms with \
         outcome {:?}",
        s.collector.outcome()
    ));
    r::observed(format!("bytes collected: {}", s.collector.len()));
    if done_while_slave_open.is_none() && done_after.is_some() {
        r::hazard(
            "CONFIRMED: while the parent holds the slave fd the reader never sees EOF even \
             though the child is dead — read() blocks indefinitely. PtyPair declares `slave` \
             first so it drops first, but a wrapper that stores the PtyPair (or the slave) \
             inside a session struct will hang its reader thread AND anything waiting for \
             output. LocalPty must drop the slave immediately after spawn_command().",
        );
    }
    r::verdict("dropping the slave right after spawn is mandatory, not optional.");
    s.collector.detach();
    Ok(())
}

// ================================================================ M2 orphans

// The per-case locals are written inside an inner scope (so that the PTY closes at a
// known point) and read after it, which trips `unused_assignments` on their initialisers.
#[allow(unused_assignments)]
pub fn m2() -> Res<()> {
    r::banner("M2 — orphan grandchildren on kill  (the load-bearing measurement)");
    r::observed(
        "method: spawn in a PTY, read the grandchild pid the payload prints, kill through \
         portable-pty's own Child::kill(), then assert liveness with kill(pid, 0) — the same \
         assertion testing.md §3.5 asks the gate to make. Every payload pid is registered \
         and hard-killed at the end of the run.",
    );

    let cases: &[(&str, Script, bool)] = &[
        (
            "control: the long sleeper IS the direct child (no grandchild)",
            Script::DirectChild,
            false,
        ),
        (
            "background job in the child's group: `sleep 300 & echo $!; wait`",
            Script::OrphanBackground,
            true,
        ),
        (
            "reparented to init, same group: `( sleep 300 & echo $! )`",
            Script::OrphanReparented,
            true,
        ),
        (
            "NEW session: `setsid sh -c 'echo $$; exec sleep 300' &`",
            Script::OrphanSetsid,
            true,
        ),
        (
            "in-group but SIGHUP-immune: `trap '' HUP; sleep 300 & echo $!; wait`",
            Script::OrphanHupIgnored,
            true,
        ),
        (
            "job control ON, own group: `set -m; sleep 300 & echo $!; wait`",
            Script::OrphanJobControl,
            true,
        ),
    ];

    let mut summary: Vec<(String, String)> = Vec::new();

    for (label, script, has_marker) in cases {
        r::sub(&format!("case: {label}"));
        let mut child_pid = 0u32;
        let mut gpid: Option<u32> = None;
        let mut kill_result = String::from("<not run>");
        let mut kill_ms = 0u128;
        let mut after_kill_child: Option<bool> = None;
        let mut after_kill_gc: Option<bool> = None;
        let mut after_close_gc: Option<bool> = None;
        let mut exit_info = String::from("<not observed>");
        let mut skipped: Option<String> = None;
        let mut pre = String::new();

        {
            // This scope ends by dropping slave-then-master, i.e. closing the PTY. That is
            // part of the measurement: does closing the tty reap the orphan?
            let mut s = spawn(*script, SpawnOpts::default())?;
            child_pid = s.pid.expect("no process_id");
            std::thread::sleep(Duration::from_millis(400));

            if *has_marker {
                let text = s.collector.text();
                if text.contains(platform::SKIP_NO_SETSID)
                    || text.contains("SKIP_NO_WINDOWS_ANALOGUE")
                {
                    skipped =
                        Some("the payload reported no such mechanism on this platform".into());
                } else {
                    match s
                        .collector
                        .wait_for_line_with(platform::MARKER, Duration::from_secs(5))
                    {
                        Some(line) => {
                            let raw = line.split('=').nth(1).unwrap_or("");
                            let digits: String = raw
                                .trim()
                                .chars()
                                .take_while(|c| c.is_ascii_digit())
                                .collect();
                            gpid = digits.parse::<u32>().ok();
                            if let Some(g) = gpid {
                                platform::register_pid(g);
                            } else {
                                skipped = Some(format!("could not parse a pid out of {line:?}"));
                            }
                        }
                        None => {
                            skipped = Some(format!(
                                "no {} line within 5 s; collected {:?}",
                                platform::MARKER,
                                truncate(&s.collector.text(), 160)
                            ))
                        }
                    }
                }
            }

            pre = format!(
                "child  {}\n          grandchild {}\n          master.as_raw_fd()={:?} \
                 process_group_leader() [tcgetpgrp]={:?} tty={:?} size={}",
                ids(child_pid),
                gpid.map(ids).unwrap_or_else(|| "<none>".into()),
                s.master_raw_fd(),
                s.process_group_leader(),
                s.tty_name(),
                master_size(&s.master),
            );
            if let Some(g) = gpid
                && let Some(mask) = platform::sig_ign_mask(g)
            {
                pre.push_str(&format!("\n          grandchild /proc SigIgn = {mask}"));
            }

            if skipped.is_none() {
                let t = Instant::now();
                let res = s.child.kill();
                kill_ms = t.elapsed().as_millis();
                kill_result = match &res {
                    Ok(()) => "Ok(())".to_string(),
                    Err(e) => format!("Err({e}) raw_os={:?}", e.raw_os_error()),
                };

                std::thread::sleep(Duration::from_millis(300));
                after_kill_child = platform::pid_alive(child_pid);
                after_kill_gc = gpid.and_then(platform::pid_alive);
                exit_info = match poll_exit(&mut s.child, Duration::from_secs(5), &s.collector) {
                    Some((ms, d)) => format!("{d} (observed {ms} ms after reader start)"),
                    None => "try_wait STILL None 5 s after kill".to_string(),
                };
            }
            s.collector.detach();
            // s drops here: slave, then master, then child.
        }

        std::thread::sleep(Duration::from_millis(400));
        after_close_gc = gpid.and_then(platform::pid_alive);

        if let Some(why) = &skipped {
            r::observed(format!("SKIPPED: {why}"));
            if let Some(g) = gpid {
                platform::hard_kill(g);
            }
            platform::hard_kill(child_pid);
            summary.push(((*label).to_string(), "SKIPPED".to_string()));
            continue;
        }

        r::observed(&pre);
        r::observed(format!(
            "Child::kill() -> {kill_result}; the call itself blocked for {kill_ms} ms"
        ));
        r::observed(format!(
            "+300 ms after kill: child {} | grandchild {}",
            fmt_alive(after_kill_child),
            fmt_alive(after_kill_gc)
        ));
        r::observed(format!("try_wait() after kill: {exit_info}"));
        r::observed(format!(
            "+400 ms after dropping slave and master (PTY closed): grandchild {}",
            fmt_alive(after_close_gc)
        ));

        let verdict = if gpid.is_none() {
            if after_kill_child == Some(false) {
                "control OK — the direct child is terminated by Child::kill()".to_string()
            } else {
                "HAZARD — the direct child SURVIVED Child::kill()".to_string()
            }
        } else if after_close_gc == Some(true) {
            "ORPHANED — the grandchild outlives both Child::kill() and closing the PTY".to_string()
        } else {
            "no orphan — the grandchild died".to_string()
        };
        r::verdict(&verdict);
        summary.push(((*label).to_string(), verdict.clone()));

        // Positive control: what a wrapper would have to do instead.
        if let Some(g) = gpid {
            if after_close_gc == Some(true) {
                // Read the group BEFORE killing anything: a dead pid has no pgid.
                let gc_pgid = platform::pid_pgid(g);
                let killed_via_child_group = platform::kill_pgid(child_pid);
                std::thread::sleep(Duration::from_millis(200));
                let after = platform::pid_alive(g);
                r::observed(format!(
                    "positive control A: our own killpg(child_pid={child_pid}, SIGKILL) \
                     returned {killed_via_child_group}; the grandchild's pgid was \
                     {gc_pgid:?}; grandchild is now {}",
                    fmt_alive(after)
                ));
                if after == Some(true) {
                    let via_gc_group = gc_pgid.map(platform::kill_pgid).unwrap_or(false);
                    std::thread::sleep(Duration::from_millis(200));
                    r::observed(format!(
                        "positive control B: killpg(grandchild_pgid={gc_pgid:?}, SIGKILL) \
                         returned {via_gc_group}; grandchild is now {}",
                        fmt_alive(platform::pid_alive(g))
                    ));
                    r::hazard(
                        "the child's process group is NOT enough: a grandchild that left the \
                         group (setsid, or job control) is unreachable from the pid the \
                         library hands us. Only enumerating descendants — or a Windows Job \
                         Object, which contains by handle inheritance rather than by group — \
                         closes that hole.",
                    );
                }
            }
            platform::hard_kill(g);
        }
        platform::hard_kill(child_pid);
    }

    r::sub("M2 summary");
    for (label, verdict) in &summary {
        println!(
            "          {:<62} {}",
            truncate(label, 62),
            truncate(verdict, 60)
        );
    }
    r::hazard(
        "Child::kill() signals ONE pid. Read from portable-pty 0.9.0 src/lib.rs, \
         `impl ChildKiller for std::process::Child`: it sends SIGHUP to self.id(), polls \
         try_wait 5 times with 50 ms sleeps (~200 ms grace), then falls through to \
         std::process::Child::kill() = SIGKILL to the same single pid. There is no killpg, no \
         group waitpid, nothing that touches descendants.",
    );
    r::observed(
        "INTERPRETATION of the pattern above, not a measurement: the cases where the \
         grandchild died all had it inside the tty's FOREGROUND process group, and the two \
         cases where it survived had it outside (a new session, or SIG_IGN on SIGHUP). That is \
         consistent with the Linux kernel's disassociate_ctty() path sending SIGHUP to the \
         controlling terminal's foreground group when the SESSION LEADER dies — i.e. the \
         cleanup above is a side effect of the child being a session leader, not of anything \
         portable-pty did. The `set -m` job-control case is the discriminating experiment: it \
         puts an ordinary `sleep 300 &` into its own, non-foreground group and should \
         therefore orphan. Read its VERDICT line above to see whether that held.",
    );

    m2b()?;
    Ok(())
}

/// The `release()` path: close the PTY without ever calling kill.
#[allow(unused_assignments)] // locals are written inside the scope that closes the PTY
fn m2b() -> Res<()> {
    r::sub("M2b — release() semantics: close the PTY WITHOUT killing the child");
    let mut child_pid = 0u32;
    let mut gpid: Option<u32> = None;
    let mut before = String::new();
    {
        let mut s = spawn(Script::OrphanBackground, SpawnOpts::default())?;
        child_pid = s.pid.expect("no process_id");
        std::thread::sleep(Duration::from_millis(400));
        if let Some(line) = s
            .collector
            .wait_for_line_with(platform::MARKER, Duration::from_secs(5))
        {
            let digits: String = line
                .split('=')
                .nth(1)
                .unwrap_or("")
                .trim()
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            gpid = digits.parse::<u32>().ok();
        }
        if let Some(g) = gpid {
            platform::register_pid(g);
        }
        before = format!(
            "child {} | grandchild {} | tcgetpgrp={:?}",
            ids(child_pid),
            gpid.map(ids).unwrap_or_else(|| "<none>".into()),
            s.process_group_leader()
        );
        s.collector.detach();
        // s drops here WITHOUT kill(): slave then master close.
    }
    std::thread::sleep(Duration::from_millis(500));
    r::observed(format!("before closing: {before}"));
    let child_alive = platform::pid_alive(child_pid);
    let gc_alive = gpid.and_then(platform::pid_alive);
    r::observed(format!(
        "+500 ms after dropping slave+master with NO kill: child {} | grandchild {}",
        fmt_alive(child_alive),
        fmt_alive(gc_alive)
    ));
    if child_alive == Some(false) {
        r::verdict(
            "dropping the PTY alone DOES terminate the direct child on Linux: the master close \
             hangs up the slave, and the session leader takes SIGHUP. So TerminalHandle::\
             release() is not a no-op — it is a kill. If release() is supposed to hand a live \
             process back (the capabilities.md sketch lists it separately from kill), a PTY \
             backend cannot honour that: closing the master is inherently fatal to the child.",
        );
    } else {
        r::verdict(
            "the child SURVIVED closing the PTY. release() would leave a running process with \
             no way to observe or stop it — the wrapper must kill explicitly and must not rely \
             on drop.",
        );
    }
    if gc_alive == Some(true) {
        r::hazard("the background grandchild also survived a bare PTY close.");
    }
    if let Some(g) = gpid {
        platform::hard_kill(g);
    }
    platform::kill_pgid(child_pid);
    platform::hard_kill(child_pid);
    std::thread::sleep(Duration::from_millis(300));

    // Deliberate measurement: the Child handle above was dropped and the process killed
    // with no wait() anywhere — which is what a fire-and-forget kill path looks like.
    let zombied = platform::is_zombie(child_pid);
    r::observed(format!(
        "ZOMBIE CHECK — the child was killed with no wait() anywhere: kill(pid, 0) = {} (so \
         `kill -0` still reports ALIVE), /proc state = {:?}, is_zombie = {zombied}",
        fmt_alive(platform::pid_alive(child_pid)),
        platform::proc_state(child_pid)
    ));
    if zombied {
        r::hazard(
            "MEASURED: a killed-but-unreaped child still answers kill(pid, 0). The assertion \
             testing.md §3.5 names — \"cancel 后无孤儿进程（kill -0 断言）\" — therefore cannot \
             distinguish a zombie from a live orphan, and a LocalPty test built only on it \
             would pass on a process table full of zombies. portable-pty's unix Child IS a \
             std::process::Child, whose Drop does not reap, so every kill path must be paired \
             with wait()/try_wait() or the daemon accumulates one zombie per shell call.",
        );
    }
    Ok(())
}

// ==================================================== M3 process-group handle

pub fn m3() -> Res<()> {
    r::banner("M3 — does the library give us a process-group handle?");

    // ---- 3a: group structure of a LIVE child -------------------------------
    r::sub("M3a — session / process-group structure of a live child");
    let mut s = spawn(Script::DirectChild, SpawnOpts::default())?;
    let child_pid = s.pid.expect("no process_id");
    std::thread::sleep(Duration::from_millis(300));
    // Everything below is read while the child is alive: a dead pid has no pgid/sid, and
    // tcgetpgrp returns -1 once the foreground group is gone.
    let pgid = platform::pid_pgid(child_pid);
    let sid = platform::pid_sid(child_pid);
    let pgl = s.process_group_leader();
    let raw_fd = s.master_raw_fd();
    let tty = s.tty_name();
    let leader_is_child = pgid == Some(child_pid) && sid == Some(child_pid);

    r::observed(format!("child {}", ids(child_pid)));
    r::observed(format!("PID == PGID == SID ? {leader_is_child}"));
    r::observed(format!(
        "master.as_raw_fd()={raw_fd:?}  master.process_group_leader() [tcgetpgrp]={pgl:?}  \
         master.tty_name()={tty:?}  master.get_size()={}",
        master_size(&s.master)
    ));
    r::observed(format!(
        "tcgetpgrp reports the child's own pid? {}",
        pgl.map(|p| p as u32) == Some(child_pid)
    ));
    let kr = s.child.kill();
    r::observed(format!("Child::kill() -> {kr:?}"));
    s.collector.detach();
    platform::kill_pgid(child_pid);
    platform::hard_kill(child_pid);

    if leader_is_child {
        r::verdict(
            "HALF YES: portable-pty's unix backend calls setsid() in pre_exec, so the child is \
             its own session AND process-group leader (PID == PGID == SID, measured above while \
             alive) and takes the PTY as controlling terminal (TIOCSCTTY; controlling_tty \
             defaults to true). A killpg(child_pid) issued by OUR code therefore reaches every \
             grandchild still in that group — M2's positive control A shows it working.",
        );
    } else {
        r::verdict(
            "PID != PGID on this run: the child is NOT a group leader, so killpg(child_pid) \
             would be wrong. Inspect the numbers above before designing the unix kill path.",
        );
    }

    // ---- 3b: what tcgetpgrp sees when a background job exists --------------
    r::sub("M3b — what process_group_leader() reports when a background job exists");
    let mut s = spawn(Script::OrphanBackground, SpawnOpts::default())?;
    let child_pid = s.pid.expect("no process_id");
    std::thread::sleep(Duration::from_millis(400));
    let mut gpid = None;
    if let Some(line) = s
        .collector
        .wait_for_line_with(platform::MARKER, Duration::from_secs(5))
    {
        let digits: String = line
            .split('=')
            .nth(1)
            .unwrap_or("")
            .trim()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        gpid = digits.parse::<u32>().ok();
    }
    if let Some(g) = gpid {
        platform::register_pid(g);
    }
    r::observed(format!("child      {}", ids(child_pid)));
    r::observed(format!(
        "grandchild {}",
        gpid.map(ids).unwrap_or_else(|| "<marker not seen>".into())
    ));
    r::observed(format!(
        "master.process_group_leader() = {:?} — one value, and it names the child's group",
        s.process_group_leader()
    ));
    if let Some(g) = gpid
        && platform::pid_pgid(g) != Some(child_pid)
    {
        r::observed(format!(
            "the grandchild's own pgid is {:?}, i.e. NOT the child's group — \
             process_group_leader() cannot see it",
            platform::pid_pgid(g)
        ));
    }
    let kr = s.child.kill();
    r::observed(format!("Child::kill() -> {kr:?}"));
    s.collector.detach();
    std::thread::sleep(Duration::from_millis(200));
    if let Some(g) = gpid {
        platform::hard_kill(g);
    }
    platform::kill_pgid(child_pid);
    platform::hard_kill(child_pid);

    // ---- 3c: the environment CommandBuilder gives the child ----------------
    r::sub("M3c — what CommandBuilder gives the child when the caller sets nothing");
    let mut s = spawn(Script::EnvProbe, SpawnOpts::default())?;
    let env_pid = s.pid.expect("no process_id");
    let exit = poll_exit(&mut s.child, Duration::from_secs(10), &s.collector);
    s.collector.wait_finished(Duration::from_secs(2));
    r::observed(format!(
        "the child's own report: {:?}",
        truncate(&s.collector.text(), 300)
    ));
    r::observed(format!(
        "exit: {}",
        exit.map(|(_, d)| d).unwrap_or_else(|| "<timeout>".into())
    ));
    r::observed(format!(
        "the probe's own cwd is {:?} — compare it with CWD= above",
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|e| format!("<{e}>"))
    ));
    s.collector.detach();
    platform::hard_kill(env_pid);

    r::hazard(
        "HALF NO: there is no API to signal the group. The entire termination surface is \
         ChildKiller::kill(&mut self) plus clone_killer(), and both target one pid. \
         MasterPty::process_group_leader() does exist (unix-only) but it is tcgetpgrp() — a \
         READ-ONLY query returning the slave's FOREGROUND group, with no matching signalling \
         call. A wrapper must bring its own killpg (libc, or nix which portable-pty already \
         depends on), and this workspace sets unsafe_code = \"deny\", so that means either a \
         local #[allow(unsafe_code)] or a new safe-wrapper dependency.",
    );
    r::hazard(
        "process_group_leader() reports the foreground group only (M3b): a grandchild that job \
         control moved into its own group, or that called setsid, is invisible to it. Keying \
         the kill off child_pid-as-pgid is strictly more reliable on unix than tcgetpgrp, and \
         even that misses the setsid case.",
    );
    r::hazard(
        "CommandBuilder defaults cwd to $HOME, not the process cwd (src/cmdbuilder.rs \
         as_command(): dir = self.cwd.filter(is_dir).unwrap_or(home)), and it resolves the \
         program through its own search_path. TerminalSpec must set cwd explicitly or every \
         shell tool call runs in the user's home directory.",
    );
    r::from_source(
        "codex's utils/pty/src/process_group.rs (309 lines) exists to fill exactly this gap. \
         It provides set_process_group()/detach_from_tty() for pre_exec, \
         kill_process_group_by_pid()/kill_process_group()/terminate_process_group()/\
         interrupt_process_group() over libc::killpg with SIGKILL/SIGTERM/SIGINT, a \
         Linux-only set_parent_death_signal() using prctl(PR_SET_PDEATHSIG) plus a getppid() \
         re-check to close the fork/exec race, and a macOS-only fallback that enumerates \
         group members with proc_listpgrppids() and signals them individually when killpg \
         returns EPERM. portable-pty supplies none of it. codex's own pty.rs comment records \
         the same finding this section measured: \"portable-pty establishes the spawned PTY \
         child as a new session leader on Unix, so PID == PGID\".",
    );
    Ok(())
}

// ============================================================ M4 cancel flow

pub fn m4() -> Res<()> {
    r::banner("M4 — cancel while output is flowing");

    r::sub("M4a — kill from the main thread while the reader thread is blocked in read()");
    let mut s = spawn(Script::Stream, SpawnOpts::default())?;
    let child_pid = s.pid.expect("no process_id");

    let t0 = Instant::now();
    while s.collector.chunks().len() < 5 && t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(5));
    }
    let chunks_at_kill = s.collector.chunks().len();
    let bytes_at_kill = s.collector.len();
    let kill_at_ms = s.collector.elapsed_ms();
    let t = Instant::now();
    let kr = s.child.kill();
    let kill_blocked_ms = t.elapsed().as_millis();

    let reader_done = s.collector.wait_finished(Duration::from_secs(5));
    let exit = poll_exit(&mut s.child, Duration::from_secs(5), &s.collector);

    r::observed(format!(
        "killed pid={child_pid} mid-stream after {chunks_at_kill} chunks / {bytes_at_kill} \
         bytes ({kill_at_ms} ms in); Child::kill() itself blocked for {kill_blocked_ms} ms -> {}",
        match &kr {
            Ok(()) => "Ok(())".to_string(),
            Err(e) => format!("Err({e})"),
        }
    ));
    r::observed(format!(
        "reader thread: finished={reader_done:?} ms, outcome={:?}, bytes={}",
        s.collector.outcome(),
        s.collector.len()
    ));
    r::observed(format!(
        "try_wait after kill: {}",
        exit.as_ref()
            .map(|(_, d)| d.clone())
            .unwrap_or_else(|| "<STILL RUNNING>".into())
    ));
    r::observed(format!("child {} after kill+wait", ids(child_pid)));

    match reader_done {
        Some(ms) => r::verdict(format!(
            "reader UNBLOCKED ~{} ms after the kill, outcome {:?}. No hang, no error to \
             handle: the library maps the pty's EIO to Ok(0) so the reader sees clean EOF.",
            ms.saturating_sub(kill_at_ms),
            s.collector.outcome()
        )),
        None => {
            r::verdict("reader thread STILL BLOCKED 5 s after the kill.");
            r::hazard(
                "a cancel that parks the reader thread forever would wedge the tool-call task; \
                 the wrapper would have to close the master fd to force EIO.",
            );
        }
    }
    match exit {
        Some(_) => r::observed("wait()/try_wait() returns after kill — no hang."),
        None => r::hazard("wait() did NOT return after kill — a defect to design around."),
    }
    s.collector.detach();
    platform::hard_kill(child_pid);

    r::sub("M4b — clone_killer() on another thread while this one blocks in Child::wait()");
    let s = spawn(Script::Stream, SpawnOpts::default())?;
    let mut killer = s.killer();
    // Destructure so `child` can move into the waiter thread while master and slave stay
    // alive in this scope (dropping the master here would EOF the reader prematurely).
    let Session {
        slave: _slave,
        master: _master,
        child,
        mut collector,
        pid,
        started: _started,
    } = s;
    let child_pid = pid.expect("no process_id");

    let result: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let done = Arc::new(AtomicBool::new(false));
    let t0 = Instant::now();
    {
        let result = Arc::clone(&result);
        let done = Arc::clone(&done);
        let mut child = child;
        std::thread::Builder::new()
            .name("pty-waiter".to_string())
            .spawn(move || {
                let status = child.wait(); // the blocking call under test
                let text = match status {
                    Ok(st) => describe(&st),
                    Err(e) => format!("Err({e})"),
                };
                *result.lock().expect("poisoned") = Some(format!(
                    "{text} — returned after {} ms",
                    t0.elapsed().as_millis()
                ));
                done.store(true, Ordering::SeqCst);
            })
            .map_err(|e| format!("waiter thread: {e}"))?;
    }

    while collector.chunks().len() < 5 && t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(5));
    }
    let kill_at_ms = t0.elapsed().as_millis();
    let kr = killer.kill();
    r::observed(format!(
        "clone_killer().kill() from the main thread at {kill_at_ms} ms -> {}",
        match &kr {
            Ok(()) => "Ok(())".to_string(),
            Err(e) => format!("Err({e})"),
        }
    ));

    let deadline = Instant::now() + Duration::from_secs(5);
    while !done.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let waited = result.lock().expect("poisoned").clone();
    r::observed(format!(
        "the thread blocked in Child::wait(): {}",
        waited.as_deref().unwrap_or("<STILL BLOCKED after 5 s")
    ));
    let reader_done = collector.wait_finished(Duration::from_secs(2));
    r::observed(format!(
        "reader thread finished={reader_done:?} outcome={:?} bytes={}",
        collector.outcome(),
        collector.len()
    ));
    std::thread::sleep(Duration::from_millis(200));
    r::observed(format!(
        "child {child_pid} now {}",
        fmt_alive(platform::pid_alive(child_pid))
    ));

    match waited {
        Some(_) => r::verdict(
            "cancel from another thread works: clone_killer() is Send + Sync exactly as its doc \
             comment promises (\"send it signals independently from a thread that may be \
             blocked in .wait\"), and the blocked wait() returns. This is the shape LocalPty's \
             cancel path needs.",
        ),
        None => {
            r::verdict("cancel from another thread did NOT release wait() within 5 s.");
            r::hazard("a wait() that survives a kill would wedge the tool-call task.");
        }
    }
    collector.detach();
    platform::hard_kill(child_pid);
    Ok(())
}

// =========================================================== M5 backpressure

pub fn m5() -> Res<()> {
    r::banner("M5 — runaway output: kernel buffering, backpressure, truncation headroom");

    r::sub("M5a — `yes | head -c 100000000` with nobody reading for 2 s");
    let mut s = spawn(
        Script::Flood,
        SpawnOpts {
            collect: false,
            keep_slave: false,
        },
    )?;
    let child_pid = s.pid.expect("no process_id");
    std::thread::sleep(Duration::from_secs(2));

    let fd = s.master_raw_fd();
    let buffered = fd.and_then(platform::master_buffered_bytes);
    let alive = child_state(&mut s);
    r::observed(format!(
        "after 2000 ms with no reader: FIONREAD(master fd {fd:?}) = {buffered:?} bytes held in \
         the kernel"
    ));
    r::observed(format!(
        "child {child_pid}: {alive}; /proc state = {:?} (S = interruptible sleep, i.e. blocked \
         on write)",
        platform::proc_state(child_pid)
    ));
    r::observed(format!(
        "a 100 MB payload had delivered only {buffered:?} bytes after 2 s of nobody reading, \
         and the child was still running => the writer is BLOCKED, not buffered without limit \
         and not silently dropping."
    ));

    // Enumerate the pipeline the child never told us about, then timeout-kill it.
    let pipeline = platform::child_pids(child_pid);
    for p in &pipeline {
        platform::register_pid(*p);
    }
    r::observed(format!(
        "the pipeline's own pids, enumerated from /proc/{child_pid}/task/*/children: \
         {pipeline:?}"
    ));
    for p in &pipeline {
        r::observed(format!("  {}", ids(*p)));
    }
    let kr = s.child.kill();
    std::thread::sleep(Duration::from_millis(600));
    r::observed(format!(
        "Child::kill() on the BLOCKED flood child -> {kr:?}; +600 ms the child is {}",
        fmt_alive(platform::pid_alive(child_pid))
    ));
    let survivors: Vec<u32> = pipeline
        .iter()
        .copied()
        .filter(|p| platform::pid_alive(*p) == Some(true) && !platform::is_zombie(*p))
        .collect();
    for p in &pipeline {
        r::observed(format!("  pipeline member after the kill: {}", ids(*p)));
    }
    r::observed(format!(
        "pipeline members still RUNNING after Child::kill(): {survivors:?}"
    ));
    if survivors.is_empty() {
        r::verdict(
            "the timeout kill DID take the whole pipeline with it — but not because the library \
             signalled it. `yes` and `head` were both in the child's process group, which is \
             also the tty's FOREGROUND group, so they died from the same kernel SIGHUP that M2 \
             measured. M2's `set -m` case shows the identical payload shape orphaning as soon \
             as the grandchild is not in the foreground group. Do not read this as \"pipelines \
             are safe\".",
        );
    } else {
        r::verdict(format!(
            "ORPHANED PIPELINE: {} of {} pipeline members are still running after the \
             library's kill(). This is the timeout path of a shell tool: `cmd | tee | …` \
             keeps running after the tool reports a timeout.",
            survivors.len(),
            pipeline.len()
        ));
        let pg = platform::kill_pgid(child_pid);
        std::thread::sleep(Duration::from_millis(300));
        let still: Vec<u32> = survivors
            .iter()
            .copied()
            .filter(|p| platform::pid_alive(*p) == Some(true) && !platform::is_zombie(*p))
            .collect();
        r::observed(format!(
            "positive control: our own killpg({child_pid}, SIGKILL) returned {pg}; still \
             running afterwards: {still:?}"
        ));
    }
    for p in &pipeline {
        platform::hard_kill(*p);
    }
    platform::kill_pgid(child_pid);
    platform::hard_kill(child_pid);
    drop(s);

    match buffered {
        Some(n) => r::verdict(format!(
            "backpressure is real and the ceiling is small: {n} bytes sat in the kernel and \
             the child blocked. Nothing grows without bound and nothing is dropped silently, so \
             the ring buffer is a policy choice on OUR side (drop-oldest + a truncation \
             marker), not something the OS does for us. It also means a stalled consumer \
             FREEZES the child — the reader must never stop draining while a tool runs, or the \
             timeout kill fires on a child that was merely waiting for us.",
        )),
        None => r::verdict(
            "FIONREAD is unavailable on this platform, so the buffer ceiling could not be \
             measured; the child-blocked evidence above still holds.",
        ),
    }

    r::sub("M5b — draining the same flood: throughput and line-discipline inflation");
    let mut s = spawn(
        Script::Flood,
        SpawnOpts {
            collect: false,
            keep_slave: false,
        },
    )?;
    let child_pid = s.pid.expect("no process_id");
    let reader = s
        .master
        .try_clone_reader()
        .map_err(|e| format!("try_clone_reader: {e}"))?;
    let t = Instant::now();
    let (count, outcome) = counting_reader(reader, Duration::from_secs(60));
    let deadline = Instant::now() + Duration::from_secs(70);
    while outcome.lock().expect("poisoned").is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let drained = count.load(Ordering::SeqCst);
    let secs = t.elapsed().as_secs_f64().max(0.001);
    let flood_mibs = drained as f64 / 1048576.0 / secs;
    // `yes` emits the 2-byte record "y\n", so head -c 100000000 passes 50_000_000 LFs.
    const RAW: usize = 100_000_000;
    const LFS: usize = RAW / 2;
    r::observed(format!(
        "drained {drained} bytes in {secs:.2} s = {:.1} MiB/s; the reader stopped because {:?}",
        drained as f64 / 1048576.0 / secs,
        outcome.lock().expect("poisoned").clone()
    ));
    r::observed(format!(
        "the payload wrote {RAW} bytes containing {LFS} LF bytes; with ONLCR the master should \
         yield {} bytes. Measured {drained}. Matches: {}",
        RAW + LFS,
        drained == RAW + LFS
    ));
    r::observed(format!("child after the drain: {}", child_state(&mut s)));
    if drained == RAW + LFS {
        r::hazard(format!(
            "CONFIRMED at scale: the line discipline inflated the child's output by {} bytes \
             ({:.0}%). A newline-heavy payload can nearly DOUBLE in size between the child's \
             write() and our ring buffer, so the D12 spill threshold and any per-call output \
             budget must be sized against what the PTY delivers, not what the child wrote.",
            drained - RAW,
            (drained - RAW) as f64 / RAW as f64 * 100.0
        ));
    }
    platform::kill_pgid(child_pid);
    platform::hard_kill(child_pid);
    drop(s);

    r::sub("M5c — byte-exact integrity of a 4 MiB payload with no newlines");
    const BYTES: usize = 4 * 1024 * 1024;
    let mut s = spawn(Script::Bulk(BYTES), SpawnOpts::default())?;
    let child_pid = s.pid.expect("no process_id");
    let exit = poll_exit(&mut s.child, Duration::from_secs(30), &s.collector);
    let reader_done = s.collector.wait_finished(Duration::from_secs(10));
    let got = s.collector.bytes();
    let all_x = got.iter().all(|b| *b == b'X');
    let elapsed = s.started.elapsed().as_secs_f64().max(0.001);
    r::observed(format!(
        "child {child_pid} exit={:?} reader_done={reader_done:?} outcome={:?}",
        exit.map(|(_, d)| d),
        s.collector.outcome()
    ));
    r::observed(format!(
        "expected {BYTES} bytes of 'X'; received {} bytes; every byte is 'X': {all_x}; \
         {} chunks; wall {elapsed:.2} s = {:.1} MiB/s",
        got.len(),
        s.collector.chunks().len(),
        BYTES as f64 / 1048576.0 / elapsed
    ));
    if got.len() == BYTES && all_x {
        let bulk_mibs = BYTES as f64 / 1048576.0 / elapsed;
        r::verdict(format!(
            "lossless: all {BYTES} bytes arrived byte-for-byte, nothing dropped or reordered."
        ));
        r::observed(format!(
            "throughput contrast: {bulk_mibs:.1} MiB/s for newline-free output vs \
             {flood_mibs:.1} MiB/s for the `yes` payload in M5b — a {:.1}x penalty purely from \
             the line discipline processing one LF every 2 bytes. Sizing D12's spill threshold \
             or a tool-call timeout on the fast number would be wrong for real shell output, \
             which is newline-heavy.",
            bulk_mibs / flood_mibs.max(0.001)
        ));
        r::verdict(format!(
            "at {bulk_mibs:.0} MiB/s a runaway child produces tens of MiB per second, so the \
             ring-buffer budget (D12's spill threshold) is the only thing between `yes` and the \
             model's context window.",
        ));
    } else {
        r::verdict(format!(
            "MISMATCH: expected {BYTES}, got {} (all 'X': {all_x}) — investigate before \
             trusting any output-completeness assertion.",
            got.len()
        ));
    }
    r::hazard(
        "the largest read() chunk is bounded by OUR buffer, not by the library: a truncating \
         ring buffer must count bytes as they are read, because the kernel will block the child \
         rather than discard. There is no library-side cap to rely on, and portable-pty offers \
         no buffered/capped reader of its own.",
    );
    s.collector.detach();
    platform::kill_pgid(child_pid);
    platform::hard_kill(child_pid);
    Ok(())
}

fn child_state(s: &mut Session) -> String {
    let pid = s.pid.unwrap_or(0);
    match s.child.try_wait() {
        Ok(None) => format!(
            "try_wait = None (still running); kill-0 = {}",
            fmt_alive(platform::pid_alive(pid))
        ),
        Ok(Some(st)) => format!(
            "try_wait = Some({}); kill-0 = {}",
            describe(&st),
            fmt_alive(platform::pid_alive(pid))
        ),
        Err(e) => format!("try_wait = Err({e})"),
    }
}

// ================================================================ M6 build cost

pub fn m6() {
    r::banner("M6 — build cost (measured outside the binary)");
    r::observed(
        "a binary cannot time its own compilation. The cold-build wall time and the \
         dependency counts are measured by the shell commands recorded in the spike report: \
         `cargo build` from a clean target directory, and `cargo tree | wc -l`.",
    );
}
