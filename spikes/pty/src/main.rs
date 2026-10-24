//! D10 measurement spike — `portable-pty` 0.9.0 semantics on a real platform.
//!
//! This binary is a measuring instrument, not a test: it has no `#[test]`s and no
//! `tests/` directory, so no test runner can pick it up, and it lives in its own cargo
//! workspace so the project's gate never builds it and `portable-pty` never enters the
//! project's `Cargo.lock` while D10 is still open.
//!
//! Run it with:
//!
//! ```sh
//! cargo run --manifest-path spikes/pty/Cargo.toml
//! ```
//!
//! It prints which platform it is on so the same binary yields a comparable report on
//! macOS and Windows. It always exits 0: a failed measurement is itself a result, and a
//! non-zero exit would only obscure it. Every process it spawns is registered and
//! hard-killed before exit, including on panic.

use std::io::Write;
use std::panic::AssertUnwindSafe;

mod measure;
mod platform;
mod report;
mod session;

fn main() {
    std::panic::set_hook(Box::new(|info| {
        let _ = std::io::stdout().flush();
        eprintln!("\nPANIC during the spike: {info}");
        let sweep = platform::cleanup_all();
        eprintln!(
            "panic-hook cleanup: hard-killed {:?}, zombies {:?}, already gone {:?} \
             ({} pids registered)",
            sweep.killed,
            sweep.zombies,
            sweep.gone,
            platform::registered_pids().len()
        );
    }));

    report::banner("D10 PTY spike — portable-pty 0.9.0 — MEASURED ON THIS MACHINE");
    for (k, v) in platform::identity() {
        report::kv(&k, v);
    }
    report::kv(
        "label convention",
        "OBSERVED / HAZARD / VERDICT = measured here.  READ-FROM-SOURCE = not measured.",
    );

    let steps: Vec<(&str, fn() -> session::Res<()>)> = vec![
        ("M1 streaming output while the child lives", measure::m1),
        ("M2 orphan grandchildren on kill", measure::m2),
        ("M3 process-group handle", measure::m3),
        ("M4 cancel while output is flowing", measure::m4),
        ("M5 backpressure and truncation headroom", measure::m5),
    ];

    let mut failures = Vec::new();
    let mut zombies_seen: Vec<u32> = Vec::new();
    for (name, f) in steps {
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(f));
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                report::verdict(format!("MEASUREMENT FAILED: {e}"));
                failures.push(format!("{name}: {e}"));
            }
            Err(_) => {
                report::verdict("MEASUREMENT PANICKED (see stderr)");
                failures.push(format!("{name}: panicked"));
            }
        }
        // Belt and braces between steps, so one case cannot leak into the next.
        zombies_seen.extend(platform::cleanup_all().zombies);
    }
    measure::m6();

    report::banner("CLEANUP");
    let registered = platform::registered_pids();
    report::observed(format!(
        "{} pids were registered by this run: {registered:?}",
        registered.len()
    ));
    let sweep = platform::cleanup_all();
    zombies_seen.extend(sweep.zombies.clone());
    zombies_seen.sort_unstable();
    zombies_seen.dedup();
    report::observed(format!(
        "final sweep — still running and hard-killed: {:?}; already zombies: {:?}; \
         already gone: {:?}; reaped by waitpid: {:?}",
        sweep.killed, sweep.zombies, sweep.gone, sweep.reaped
    ));
    let strays = stray_sleepers();
    report::observed(format!(
        "sweep of the whole process table for the payload's signature (`sleep 300`) plus any \
         registered pid that shows as `<defunct>`:\n{}",
        if strays.is_empty() {
            "          <empty — no stray processes>".to_string()
        } else {
            strays
                .iter()
                .map(|l| format!("          {l}"))
                .collect::<Vec<_>>()
                .join("\n")
        }
    ));
    let zombies = platform::zombies_remaining();
    report::observed(format!(
        "pids still answering kill(pid, 0) after the sweep: {zombies:?} (/proc usable here: {})",
        platform::proc_available()
    ));
    if strays.is_empty() && zombies.is_empty() {
        report::verdict("no stray processes left behind — nothing running, nothing defunct.");
    } else {
        report::hazard(format!(
            "{} table entr(ies) attributable to this run and {:?} pid(s) still answering \
             kill(pid, 0) — see the listings above.",
            strays.len(),
            zombies
        ));
    }
    if !zombies_seen.is_empty() {
        r_zombie_hazard(&zombies_seen);
    }

    report::banner("END");
    if failures.is_empty() {
        report::observed("all measurements ran to completion on this platform.");
    } else {
        report::observed(format!("measurements that did not complete: {failures:?}"));
    }
    report::observed(
        "This run measured ONE platform. macOS and Windows are not measured here; the \
         Windows/macOS mechanisms in the accompanying report are read from \
         references/codex/codex-rs/utils/pty/src and from portable-pty 0.9.0's own source.",
    );

    let _ = std::io::stdout().flush();
    std::process::exit(0);
}

fn r_zombie_hazard(zombies: &[u32]) {
    report::hazard(format!(
        "{n} pid(s) were observed as ZOMBIES during this run ({zombies:?}): killed but never \
         reaped, because the Child handle was dropped without a wait(). std::process::Child's \
         Drop does not reap, and portable-pty's unix Child IS a std::process::Child — so a \
         LocalPty that kills without waiting leaks one zombie per shell call for the lifetime \
         of the daemon. Consequence for the gate: `kill -0 <pid>`, the assertion testing.md \
         §3.5 names, SUCCEEDS for a zombie, so on its own it cannot prove \"no orphan \
         process\". The LocalPty test must also reap (Child::wait/try_wait) or read the \
         process state, or it will pass on a table full of zombies. This probe reaps in its \
         own sweep, which is why the listing above is empty.",
        n = zombies.len()
    ));
}

/// The process-table sweep the report quotes. An instrument, so it deliberately does not
/// go through `portable-pty`.
fn stray_sleepers() -> Vec<String> {
    #[cfg(unix)]
    let out = std::process::Command::new("ps")
        .args(["-eo", "pid,args"])
        .output();
    #[cfg(windows)]
    let out = std::process::Command::new("tasklist")
        .args(["/V", "/FO", "CSV"])
        .output();
    #[cfg(not(any(unix, windows)))]
    let out: Result<std::process::Output, std::io::Error> = Err(std::io::Error::other(
        "no process-table instrument on this platform",
    ));

    let Ok(out) = out else {
        return vec!["<ps/tasklist unavailable>".to_string()];
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let needle = if cfg!(windows) { "ping" } else { "sleep 300" };
    let mine = std::process::id();
    let registered = platform::registered_pids();
    text.lines()
        .filter(|l| {
            let Some(pid) = l
                .split_whitespace()
                .next()
                .and_then(|p| p.parse::<u32>().ok())
            else {
                return false; // header line
            };
            if pid == mine {
                return false; // never report the probe itself
            }
            // A live match on the payload signature, or a zombie we know we spawned.
            // `<defunct>` lines lose their argv, so the registry is the only way to
            // attribute them — and it keeps unrelated zombies off the report.
            l.contains(needle) || (l.contains("<defunct>") && registered.contains(&pid))
        })
        .map(|l| l.to_string())
        .collect()
}
