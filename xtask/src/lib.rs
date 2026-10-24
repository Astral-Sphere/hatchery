//! Developer tasks for the hatchery workspace (`cargo xtask <subcommand>`).
//!
//! Only checks that are awkward to express in bash live here; the gate sequence itself belongs
//! to `scripts/ci.sh`, which CI and local runs share verbatim.
//!
//! Status: `layering`, `coverage` and `record-fixtures` work; `i18n-extract` fails loudly until
//! M4. `record-fixtures` hits real providers, costs money and never runs in CI.

pub mod coverage;
pub mod layering;
pub mod record;

use anyhow::{Result, anyhow};

const USAGE: &str = "\
xtask — developer tasks for the hatchery workspace

USAGE:
    cargo xtask <SUBCOMMAND>

SUBCOMMANDS:
    layering         Check the crate dependency DAG against docs/architecture.md §3
    coverage         Run the test suite under cargo-llvm-cov [--html]
    i18n-extract     Extract translatable strings (not implemented until M4)
    record-fixtures  Record provider SSE fixtures from real endpoints
                     [--provider deepseek|qwen|all] [--out DIR] [--force]
    help             Print this help
";

/// Runs a subcommand and returns a one-line report for stdout.
pub fn run(args: impl Iterator<Item = String>) -> Result<String> {
    let args: Vec<String> = args.collect();
    match args.first().map(String::as_str) {
        None | Some("help") | Some("-h") | Some("--help") => Ok(USAGE.to_owned()),
        Some("layering") => layering::check().map(|report| report.to_string()),
        Some("coverage") => coverage::run(&args[1..]).map(|()| String::new()),
        Some("i18n-extract") => Err(anyhow!(
            "i18n-extract is not implemented until M4; see docs/worklog/platform.md"
        )),
        Some("record-fixtures") => record::run(&args[1..]),
        Some(other) => Err(anyhow!("unknown subcommand {other:?}\n\n{USAGE}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_args(args: &[&str]) -> Result<String> {
        run(args.iter().map(|a| (*a).to_owned()))
    }

    #[test]
    fn unimplemented_subcommands_fail_loudly() {
        let err = run_args(&["i18n-extract"]).expect_err("unimplemented subcommand");
        assert!(
            err.to_string().contains("M4"),
            "i18n-extract should point at M4: {err}"
        );
    }

    #[test]
    fn recording_rejects_unknown_providers_before_touching_the_network() {
        // Never invoke the bare subcommand in tests: with real credentials present it would
        // record for real, so this validates the argument gate only — it fails before any key
        // is read or any socket is opened.
        let err = run_args(&["record-fixtures", "--provider", "bogus"])
            .expect_err("an unknown provider must be refused");
        assert!(
            err.to_string().contains("unknown provider"),
            "the refusal should say what was wrong: {err}"
        );
    }

    #[test]
    fn unknown_subcommand_is_rejected() {
        assert!(run_args(&["nope"]).is_err());
    }

    #[test]
    fn help_lists_every_subcommand() {
        let usage = run_args(&["help"]).expect("help must succeed");
        for name in ["layering", "coverage", "i18n-extract", "record-fixtures"] {
            assert!(usage.contains(name), "help is missing {name}");
        }
    }
}
