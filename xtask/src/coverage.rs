//! `cargo xtask coverage` — the test suite under `cargo-llvm-cov`, with the per-crate
//! thresholds of docs/design/testing.md §8 enforced.
//!
//! llvm-cov reports per *file*; the mapping back to crates is by path prefix
//! (`crates/<name>/…`), and a crate's number is its executable lines summed, not an average of
//! file percentages. Enforced crates are the product core (kernel/store/llm/capabilities/tools at
//! ≥ 85%, protocol/daemon at ≥ 80%, cli at ≥ 60%); gui is exempt this milestone and dev-only
//! crates (testkit, hatchery-tests, xtask) are not gates. Coverage is a metric, not a goal —
//! `--report-only` prints the table and skips the refusals.

use std::collections::BTreeMap;
use std::process::Command;

use anyhow::{Context, Result, anyhow};

/// The enforced floors (docs/design/testing.md §8).
pub const THRESHOLDS: &[(&str, u8)] = &[
    ("hatchery-kernel", 85),
    ("hatchery-store", 85),
    ("hatchery-llm", 85),
    ("hatchery-capabilities", 85),
    // M2's four new tools all land here, and the seam ban that keeps them honest is compile-time
    // only: what a tool *does* with an approved call is checked by these tests.
    ("hatchery-tools", 85),
    ("hatchery-protocol", 80),
    ("hatchery-daemon", 80),
    ("hatchery-cli", 60),
];

/// Runs the CI nextest profile under coverage, prints the per-crate table, and enforces the
/// thresholds unless `--report-only` was passed.
pub fn run(args: &[String]) -> Result<()> {
    let report_only = args.iter().any(|arg| arg == "--report-only");
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

    let json_path = std::env::current_dir()
        .context("current dir")?
        .join("target/llvm-cov/coverage.json");
    let report = Command::new(env!("CARGO"))
        .args([
            "llvm-cov",
            "report",
            "--json",
            "--output-path",
            json_path.to_string_lossy().as_ref(),
        ])
        .status()
        .context("failed to run `cargo llvm-cov report --json`")?;
    if !report.success() {
        return Err(anyhow!("`cargo llvm-cov report --json` failed"));
    }

    let raw = std::fs::read_to_string(&json_path)
        .with_context(|| format!("reading {}", json_path.display()))?;
    let totals = crate_totals(&raw)
        .map_err(|error| anyhow!("the llvm-cov JSON was not the shape expected: {error}"))?;
    if totals.is_empty() {
        return Err(anyhow!(
            "no workspace source files found in the report; the path prefix mapping is broken"
        ));
    }

    let mut deficits = Vec::new();
    println!(
        "{:<24}{:>8}{:>10}{:>8}",
        "crate", "lines", "percent", "floor"
    );
    if !report_only {
        // A gated crate absent from the totals is a broken mapping, not a pass: failing to find
        // a deficit must mean the crate was measured, not that it was never looked for.
        let missing: Vec<&str> = THRESHOLDS
            .iter()
            .map(|(name, _)| *name)
            .filter(|name| !totals.contains_key(*name))
            .collect();
        if !missing.is_empty() {
            return Err(anyhow!(
                "the coverage report has no files for: {}; the path mapping or the build is broken",
                missing.join(", ")
            ));
        }
    }
    for (name, lines) in &totals {
        let floor = THRESHOLDS
            .iter()
            .find(|(crate_name, _)| crate_name == name)
            .map(|(_, floor)| *floor);
        let percent = lines.percent();
        println!("{name:<24}{:>8}{percent:>9.1}%", lines.total,);
        if let Some(floor) = floor {
            print!("{floor:>7}%");
            // The epsilon keeps a 84.97% report from flipping a gate on rounding noise.
            if percent + 0.05 < f64::from(floor) {
                deficits.push(format!(
                    "{name} at {percent:.1}% is below its {floor}% floor",
                ));
            }
        } else {
            println!("{:>8}", "—");
        }
    }

    if report_only {
        return Ok(());
    }
    if deficits.is_empty() {
        println!("coverage: every gated crate meets its floor");
        return Ok(());
    }
    Err(anyhow!(
        "coverage below the floors (docs/design/testing.md §8):\n  {}",
        deficits.join("\n  ")
    ))
}

/// Executable-line totals for one crate.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LineTotals {
    /// Executable lines.
    pub total: u64,
    /// Lines the report saw execute at least once.
    pub covered: u64,
}

impl LineTotals {
    /// Coverage as a percentage, 0 when the crate has no executable lines.
    #[must_use]
    pub fn percent(self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        (self.covered as f64 / self.total as f64) * 100.0
    }
}

/// Folds the report's per-file records into per-crate totals, keyed by crate name.
///
/// Only `crates/<name>/src` files count — tests measure the code they cover, not themselves.
pub fn crate_totals(json: &str) -> Result<BTreeMap<String, LineTotals>> {
    let report: CoverageReport = serde_json::from_str(json).map_err(anyhow::Error::from)?;
    let mut totals: BTreeMap<String, LineTotals> = BTreeMap::new();
    let files = report
        .data
        .first()
        .map(|data| data.files.as_slice())
        .unwrap_or(&[]);
    for file in files {
        // Anchored on `crates/hatchery-` and taken from the RIGHT: a checkout living under a
        // path that itself contains `crates/` must not remap every file to a bogus crate whose
        // name matches no threshold — that failure mode passed silently before.
        let Some((_, crate_name)) = file.filename.rsplit_once("crates/hatchery-") else {
            continue;
        };
        let Some((crate_name, rest)) = crate_name.split_once('/') else {
            continue;
        };
        let crate_name = format!("hatchery-{crate_name}");
        if rest.starts_with("tests/") {
            continue;
        }
        let Some(lines) = &file.summary.lines else {
            continue;
        };
        *totals.entry(crate_name.to_owned()).or_default() += LineTotals {
            total: lines.count,
            covered: lines.covered,
        };
    }
    Ok(totals)
}

impl std::ops::AddAssign for LineTotals {
    fn add_assign(&mut self, rhs: Self) {
        self.total += rhs.total;
        self.covered += rhs.covered;
    }
}

// ---- the slice of the llvm-cov JSON schema this reads ----------------------------------------

#[derive(serde::Deserialize)]
struct CoverageReport {
    data: Vec<CoverageData>,
}

#[derive(serde::Deserialize)]
struct CoverageData {
    files: Vec<CoveredFile>,
}

#[derive(serde::Deserialize)]
struct CoveredFile {
    filename: String,
    summary: FileSummary,
}

#[derive(serde::Deserialize)]
struct FileSummary {
    /// Absent for files with no executable lines.
    lines: Option<LinesSummary>,
}

#[derive(serde::Deserialize)]
struct LinesSummary {
    count: u64,
    covered: u64,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_fold_into_crates_and_tests_do_not_count() {
        let json = r#"{"data":[{"files":[
            {"filename":"crates/hatchery-llm/src/lib.rs","summary":{"lines":{"count":100,"covered":90}}},
            {"filename":"crates/hatchery-llm/src/translate.rs","summary":{"lines":{"count":50,"covered":40}}},
            {"filename":"crates/hatchery-llm/tests/adapter.rs","summary":{"lines":{"count":1000,"covered":1000}}},
            {"filename":"crates/hatchery-daemon/src/lib.rs","summary":{"lines":{"count":10,"covered":9}}},
            {"filename":"xtask/src/main.rs","summary":{"lines":{"count":7,"covered":7}}}
        ]}]}"#;
        let totals = crate_totals(json).expect("parses");
        assert_eq!(totals.len(), 2, "xtask is not under crates/");
        let llm = totals["hatchery-llm"];
        assert_eq!(llm.total, 150, "test files never count");
        assert_eq!(llm.covered, 130);
        assert!((llm.percent() - 86.66).abs() < 0.01);
        assert!((totals["hatchery-daemon"].percent() - 90.0).abs() < 0.001);
    }

    #[test]
    fn an_empty_or_foreign_report_is_refused_upstream() {
        assert!(crate_totals("{}").is_err());
        let empty = crate_totals(r#"{"data":[{"files":[]}]}"#).expect("parses");
        assert!(empty.is_empty(), "the caller refuses an empty mapping");
    }

    #[test]
    fn a_checkout_path_containing_crates_still_folds_correctly() {
        let json = r#"{"data":[{"files":[
            {"filename":"/home/dev/crates/fork/hatchery/crates/hatchery-llm/src/lib.rs","summary":{"lines":{"count":100,"covered":90}}},
            {"filename":"/home/dev/crates/fork/hatchery/crates/hatchery-llm/tests/x.rs","summary":{"lines":{"count":5,"covered":5}}}
        ]}]}"#;
        let totals = crate_totals(json).expect("parses");
        assert_eq!(totals.len(), 1, "one crate, not a bogus outer one");
        assert_eq!(totals["hatchery-llm"].total, 100, "tests do not count");
    }

    #[test]
    fn zero_line_crates_report_zero_rather_than_dividing() {
        let totals = LineTotals::default();
        assert_eq!(totals.percent(), 0.0);
    }

    #[test]
    fn the_threshold_table_covers_the_gated_crates() {
        let names: Vec<&str> = THRESHOLDS.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            names,
            [
                "hatchery-kernel",
                "hatchery-store",
                "hatchery-llm",
                "hatchery-capabilities",
                "hatchery-tools",
                "hatchery-protocol",
                "hatchery-daemon",
                "hatchery-cli",
            ]
        );
        assert!(
            THRESHOLDS
                .iter()
                .all(|(_, floor)| (60..=95).contains(floor))
        );
    }
}
