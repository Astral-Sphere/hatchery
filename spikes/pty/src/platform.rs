//! Observation instruments and per-platform command data.
//!
//! Nothing in this module goes through `portable-pty` on purpose: these are the
//! measuring devices (process liveness, process-group / session ids, kernel buffer
//! occupancy) plus the shell payloads the measurements feed to the library. The
//! platform difference lives in *data* (which program and which script), not in
//! cfg-gated control flow, so the same binary runs everywhere and prints a
//! comparable report.
//!
//! SAFETY: the `libc` calls below are the direct analogues of the `kill -0` /
//! `ps` assertions that `docs/design/testing.md` §3.5 asks the gate to make. The
//! project denies `unsafe_code` workspace-wide; a real `LocalPty` would either need
//! a local `#[allow(unsafe_code)]` or the `nix` crate (already a portable-pty
//! dependency) for safe `killpg` / `getpgid` wrappers. That trade-off is reported.
#![allow(unsafe_code)]

use std::sync::Mutex;
use std::time::Duration;

/// Every pid this probe has caused to exist, so nothing is left behind.
static SPAWNED: Mutex<Vec<u32>> = Mutex::new(Vec::new());

pub fn register_pid(pid: u32) {
    SPAWNED.lock().expect("registry poisoned").push(pid);
}

pub fn registered_pids() -> Vec<u32> {
    SPAWNED.lock().expect("registry poisoned").clone()
}

/// Result of a cleanup sweep.
pub struct Sweep {
    /// Alive (and not a zombie) when the sweep started; hard-killed.
    pub killed: Vec<u32>,
    /// `kill -0` said alive but `/proc` said `Z`: a dead child nobody reaped. Recorded
    /// BEFORE the reap pass below, because it is a measurement, not just bookkeeping.
    pub zombies: Vec<u32>,
    /// `kill -0` said no such process.
    pub gone: Vec<u32>,
    /// Zombies this sweep reaped with `waitpid(.., WNOHANG)`.
    pub reaped: Vec<u32>,
}

/// Hard-kill everything the probe spawned, then reap what we killed.
///
/// Two kill passes on purpose: the *measurements* kill through the library and therefore
/// orphan grandchildren by design, so cleanup cannot rely on the library. Pass 1 hits
/// the whole process group of every pid we registered (a portable-pty child is a session
/// leader, so its pid is also its pgid, and `yes`/`head`/`sleep` grandchildren inherit
/// it); pass 2 hits any pid that survived or belonged to a different group.
///
/// The reap pass matters for the report's honesty: `kill -0` succeeds for a zombie, so a
/// sweep that only kills would leave a process table full of pids that still look alive.
pub fn cleanup_all() -> Sweep {
    let pids = registered_pids();

    #[cfg(unix)]
    let our_pgid = unsafe { libc::getpgid(0) };
    #[cfg(unix)]
    for pid in &pids {
        let g = unsafe { libc::getpgid(*pid as libc::pid_t) };
        if g > 0 && g != our_pgid {
            unsafe {
                libc::killpg(g, libc::SIGKILL);
            }
        }
    }

    let mut sweep = Sweep {
        killed: Vec::new(),
        zombies: Vec::new(),
        gone: Vec::new(),
        reaped: Vec::new(),
    };
    for pid in &pids {
        // Classify BEFORE killing: SIGKILL on an unreaped child turns it into a zombie
        // that this process, as the parent, is the only one able to reap.
        if is_zombie(*pid) {
            sweep.zombies.push(*pid);
        } else if pid_alive(*pid) == Some(true) {
            hard_kill(*pid);
            sweep.killed.push(*pid);
        } else {
            sweep.gone.push(*pid);
        }
    }
    // Reaping is asynchronous; give the kernel a moment before we try to collect.
    std::thread::sleep(Duration::from_millis(300));
    for _ in 0..4 {
        let mut progress = false;
        for pid in &pids {
            if reap_once(*pid) && !sweep.reaped.contains(pid) {
                sweep.reaped.push(*pid);
                progress = true;
            }
        }
        if !progress {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    sweep
}

/// One non-blocking `waitpid`. True when this process actually collected `pid`.
#[cfg(unix)]
fn reap_once(pid: u32) -> bool {
    let mut status = 0;
    let r = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
    r == pid as libc::pid_t
}

#[cfg(not(unix))]
fn reap_once(_pid: u32) -> bool {
    false
}

/// Pids that still exist as zombies. They are not stray *running* processes, but they do
/// still answer `kill -0`, which is the assertion testing.md §3.5 names.
pub fn zombies_remaining() -> Vec<u32> {
    registered_pids()
        .into_iter()
        .filter(|pid| pid_alive(*pid) == Some(true) && is_zombie(*pid))
        .collect()
}

// ---------------------------------------------------------------- identity

pub fn identity() -> Vec<(String, String)> {
    let mut rows =
        vec![
        ("target_os".to_string(), std::env::consts::OS.to_string()),
        (
            "target_arch".to_string(),
            std::env::consts::ARCH.to_string(),
        ),
        ("probe".to_string(), "spikes/pty (own workspace, not built by the gate)".to_string()),
        (
            "library".to_string(),
            "portable-pty 0.9.0 (crates.io, same pin as references/codex codex-rs/Cargo.toml:422)"
                .to_string(),
        ),
    ];
    rows.push(("kernel".to_string(), kernel_version()));
    rows
}

#[cfg(unix)]
fn kernel_version() -> String {
    let mut uts: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut uts) } != 0 {
        return "uname failed".to_string();
    }
    let c = |b: &[libc::c_char]| {
        let bytes: Vec<u8> = b
            .iter()
            .take_while(|c| **c != 0)
            .map(|c| *c as u8)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    };
    format!(
        "{} {} {} {}",
        c(&uts.sysname),
        c(&uts.release),
        c(&uts.version),
        c(&uts.machine)
    )
}

#[cfg(not(unix))]
fn kernel_version() -> String {
    "n/a (non-unix)".to_string()
}

// ------------------------------------------------------- liveness / groups

/// `kill -0 <pid>` equivalent. `None` means "this platform cannot tell".
pub fn pid_alive(pid: u32) -> Option<bool> {
    #[cfg(unix)]
    {
        let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if r == 0 {
            return Some(true);
        }
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::ESRCH) => Some(false),
            // EPERM: it exists, we just may not signal it.
            Some(libc::EPERM) => Some(true),
            _ => None,
        }
    }
    #[cfg(windows)]
    {
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        Some(text.contains(&format!("\"{pid}\"")))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        None
    }
}

pub fn pid_pgid(pid: u32) -> Option<u32> {
    #[cfg(unix)]
    {
        let r = unsafe { libc::getpgid(pid as libc::pid_t) };
        (r != -1).then_some(r as u32)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}

pub fn pid_sid(pid: u32) -> Option<u32> {
    #[cfg(unix)]
    {
        let r = unsafe { libc::getsid(pid as libc::pid_t) };
        (r != -1).then_some(r as u32)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}

/// Linux-only: the `SigIgn` mask from `/proc/<pid>/status`, used to confirm that the
/// SIGHUP-ignoring case really did install SIG_IGN before we interpret its survival.
pub fn sig_ign_mask(pid: u32) -> Option<String> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|l| l.strip_prefix("SigIgn:"))
        .map(|v| v.trim().to_string())
}

/// Linux-only: the process state letter from `/proc/<pid>/stat` (`R`, `S`, `D`, `Z`, …).
/// `Z` = zombie: `kill -0` still succeeds for it, so a liveness check alone cannot tell a
/// dead-but-unreaped child from a live one.
pub fn proc_state(pid: u32) -> Option<char> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The comm field is parenthesised and may contain spaces; parse after the last ')'.
    let tail = stat.rsplit_once(')')?.1;
    tail.trim().chars().next()
}

/// True when the pid exists only as an unreaped exit status. Always false where
/// `/proc` is unavailable — callers must treat that as "cannot tell", not "not a zombie".
pub fn is_zombie(pid: u32) -> bool {
    proc_state(pid) == Some('Z')
}

/// Is `/proc` usable here? Decides whether `is_zombie` results mean anything.
pub fn proc_available() -> bool {
    std::path::Path::new("/proc/self/stat").exists()
}

/// Linux-only: the direct children of `pid`, from `/proc/<pid>/task/*/children`.
/// Used to enumerate a pipeline (`yes | head`) whose pids the child never printed.
pub fn child_pids(pid: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return out;
    };
    for entry in entries.flatten() {
        if let Ok(text) = std::fs::read_to_string(entry.path().join("children")) {
            out.extend(
                text.split_whitespace()
                    .filter_map(|t| t.parse::<u32>().ok()),
            );
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(unix)]
pub fn hard_kill(pid: u32) {
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
pub fn hard_kill(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .output();
}

/// `killpg(pgid, SIGKILL)`. The library offers no equivalent; this exists so the probe
/// can (a) demonstrate what a wrapper would have to add and (b) clean up after itself.
/// Returns true when the call itself succeeded.
#[cfg(unix)]
pub fn kill_pgid(pgid: u32) -> bool {
    unsafe { libc::killpg(pgid as libc::pid_t, libc::SIGKILL) == 0 }
}

#[cfg(not(unix))]
pub fn kill_pgid(_pgid: u32) -> bool {
    false
}

/// Bytes the kernel has buffered on the master side and not yet handed to a reader
/// (`FIONREAD`). This is the concrete "how much can pile up before the writer blocks".
#[cfg(unix)]
pub fn master_buffered_bytes(raw_fd: i32) -> Option<usize> {
    let mut n: libc::c_int = 0;
    let r = unsafe { libc::ioctl(raw_fd, libc::FIONREAD, &mut n as *mut libc::c_int) };
    (r == 0).then_some(n.max(0) as usize)
}

#[cfg(not(unix))]
pub fn master_buffered_bytes(_raw_fd: i32) -> Option<usize> {
    None
}

// ------------------------------------------------------------- the payloads

#[derive(Clone, Copy, Debug)]
pub enum Script {
    /// Prints 20 lines ~50 ms apart, then exits. Measures streaming granularity.
    Stream,
    /// Control: the long sleeper *is* the direct child. No grandchild.
    DirectChild,
    /// `sleep 300 &` then `wait`: grandchild in the child's own process group.
    OrphanBackground,
    /// `( sleep 300 & )`: grandchild reparented to init, still in the child's group.
    OrphanReparented,
    /// `setsid … exec sleep 300`: grandchild in a brand-new session.
    OrphanSetsid,
    /// `trap '' HUP; sleep 300 &`: grandchild in the child's group, SIGHUP-immune.
    OrphanHupIgnored,
    /// `set -m; sleep 300 &`: job control ON, so the grandchild gets its OWN process
    /// group and is no longer in the tty's foreground group. This is the case that
    /// discriminates "the library killed it" from "the kernel SIGHUP'd the foreground
    /// group when the session leader died".
    OrphanJobControl,
    /// ~100 MiB as fast as the kernel allows; never stops on its own.
    Flood,
    /// Exactly `bytes` of `X`, no newlines, so the count is checkable byte-for-byte.
    Bulk(usize),
    /// Reports the environment the library actually gives the child (cwd, tty).
    EnvProbe,
}

/// Marker line the orphan payloads print so the harness learns the grandchild pid.
pub const MARKER: &str = "GRANDCHILD=";
pub const SKIP_NO_SETSID: &str = "SKIP_NO_SETSID";

pub fn command(script: Script) -> (String, Vec<String>) {
    #[cfg(unix)]
    {
        unix_command(script)
    }
    #[cfg(windows)]
    {
        windows_command(script)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = script;
        ("false".to_string(), Vec::new())
    }
}

#[cfg(unix)]
fn sh(script: &str) -> (String, Vec<String>) {
    ("sh".to_string(), vec!["-c".to_string(), script.to_string()])
}

#[cfg(unix)]
fn unix_command(script: Script) -> (String, Vec<String>) {
    match script {
        Script::Stream => sh("for i in $(seq 1 20); do echo line-$i; sleep 0.05; done"),
        Script::DirectChild => sh("exec sleep 300"),
        Script::OrphanBackground => sh("sleep 300 & echo GRANDCHILD=$!; wait"),
        Script::OrphanReparented => sh("( sleep 300 & echo GRANDCHILD=$! ); read -r _"),
        // `setsid` is util-linux; macOS does not ship it, so guard rather than fail.
        Script::OrphanSetsid => sh(
            "command -v setsid >/dev/null 2>&1 || { echo SKIP_NO_SETSID; exit 0; }; \
             setsid sh -c 'echo GRANDCHILD=$$; exec sleep 300' & echo STARTED; read -r _",
        ),
        Script::OrphanHupIgnored => sh("trap '' HUP; sleep 300 & echo GRANDCHILD=$!; wait"),
        Script::OrphanJobControl => sh("set -m; sleep 300 & echo GRANDCHILD=$!; wait"),
        Script::Flood => sh("yes | head -c 100000000"),
        Script::Bulk(bytes) => {
            let mib = bytes / (1024 * 1024);
            sh(&format!(
                "dd if=/dev/zero bs=1048576 count={mib} 2>/dev/null | tr '\\0' 'X'"
            ))
        }
        Script::EnvProbe => sh("echo CWD=$(pwd); echo TTY=$(tty); echo SHELL_VAR=$SHELL"),
    }
}

/// UNTESTED: never executed. This spike ran on Linux only; these payloads exist so the
/// same binary produces a comparable report on a Windows CI runner, but they are
/// written-from-documentation and may need fixing on first run.
#[cfg(windows)]
fn windows_command(script: Script) -> (String, Vec<String>) {
    let ps = |body: &str| {
        (
            "powershell".to_string(),
            vec![
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
                body.to_string(),
            ],
        )
    };
    match script {
        Script::Stream => ps(
            "1..20 | ForEach-Object { Write-Output ('line-' + $_); Start-Sleep -Milliseconds 50 }",
        ),
        Script::DirectChild => ps("Start-Sleep -Seconds 300"),
        Script::OrphanBackground => ps(
            "$p = Start-Process -FilePath cmd.exe -ArgumentList '/c','ping -n 300 127.0.0.1 > nul' \
             -PassThru -WindowStyle Hidden; Write-Output ('GRANDCHILD=' + $p.Id); \
             Start-Sleep -Seconds 300",
        ),
        // Windows has no reparent/setsid/job-control analogue: containment there is the
        // Job Object, which is exactly what the spike report covers from codex's source.
        Script::OrphanReparented
        | Script::OrphanSetsid
        | Script::OrphanHupIgnored
        | Script::OrphanJobControl => ps("Write-Output 'SKIP_NO_WINDOWS_ANALOGUE'"),
        Script::Flood => ps(
            "$s = [Console]::OpenStandardOutput(); $b = New-Object byte[] 1048576; \
             [Array]::Fill($b, [byte]88); for ($i = 0; $i -lt 96; $i++) { $s.Write($b, 0, $b.Length) }",
        ),
        Script::Bulk(bytes) => ps(&format!(
            "$s = [Console]::OpenStandardOutput(); $b = New-Object byte[] 1048576; \
             [Array]::Fill($b, [byte]88); $left = {bytes}; \
             while ($left -gt 0) {{ $n = [Math]::Min($b.Length, $left); $s.Write($b, 0, $n); $left -= $n }}"
        )),
        Script::EnvProbe => ps("Write-Output ('CWD=' + (Get-Location).Path); \
             Write-Output ('TTY=' + [Console]::IsOutputRedirected)"),
    }
}
