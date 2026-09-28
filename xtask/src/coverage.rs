//! `cargo xtask coverage` — the test suite under `cargo-llvm-cov`.
//!
//! Per-crate thresholds (docs/design/testing.md §8) are reported but not enforced yet: llvm-cov
//! aggregates per file, so mapping files back to crates needs work that is scheduled for M1.

use std::process::Command;

use anyhow::{Context, Result, anyhow};

/// Runs the CI nextest profile under coverage, then prints a summary.
pub fn run(args: &[String]) -> Result<()> {
    let html = args.iter().any(|arg| arg == "--html");
    ensure_installed()?;

    let status = Command::new(env!("CARGO"))
        .args([
            "llvm-cov",
            "--no-report",
            "nextest",
            "--workspace",
            "--profile",
            "ci",
        ])
        .status()
        .context("failed to run `cargo llvm-cov nextest`")?;
    if !status.success() {
        return Err(anyhow!(
            "tests failed under coverage; fix them before reading the report"
        ));
    }

    let mut report = Command::new(env!("CARGO"));
    report.args(["llvm-cov", "report", "--summary-only"]);
    if html {
        report.arg("--html");
    }
    let status = report
        .status()
        .context("failed to run `cargo llvm-cov report`")?;
    if !status.success() {
        return Err(anyhow!("`cargo llvm-cov report` failed"));
    }
    if html {
        println!("html report: target/llvm-cov/html/index.html");
    }
    Ok(())
}

fn ensure_installed() -> Result<()> {
    let probe = Command::new(env!("CARGO"))
        .args(["llvm-cov", "--version"])
        .output();
    match probe {
        Ok(output) if output.status.success() => Ok(()),
        _ => Err(anyhow!(
            "cargo-llvm-cov is not installed; run `cargo install cargo-llvm-cov` \
             (docs/design/testing.md §8)"
        )),
    }
}
