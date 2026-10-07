#!/usr/bin/env bash
# hatchery gate sequence — the single source of truth shared by local runs and CI.
# .github/workflows/pr.yml executes this file verbatim, so "green locally" and "green in CI"
# mean the same thing (docs/design/testing.md §8).
#
# bash 3.2 compatible on purpose: macOS runners ship bash 3.2, so no associative arrays,
# no mapfile, no ${var,,}.
#
# Usage:
#   scripts/ci.sh             full gate
#   scripts/ci.sh --quick     fmt + clippy + build only, for fast local iteration
#   scripts/ci.sh --help
#
# Environment:
#   SKIP_STEPS   space-separated step names to skip locally, e.g. SKIP_STEPS="doctests"
#   INSTA_UPDATE forced to "no" below: the protocol's golden-fixture gate reads it, so a CI run
#                can never rewrite the fixtures it is checking (tests/golden_fixtures.rs)

set -euo pipefail

cd "$(dirname "$0")/.."

export INSTA_UPDATE=no
export INSTA_FORCE_UPDATE=0
export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-auto}"

QUICK=0
SKIP_STEPS="${SKIP_STEPS:-}"
FAILED_STEPS=""
PASSED_STEPS=""

usage() {
    printf '%s\n' \
        "Usage: scripts/ci.sh [--quick] [--help]" \
        "" \
        "  --quick    fmt + clippy + build only" \
        "" \
        "Steps: toolchain fmt clippy build tests invariants doctests determinism" \
        "Not gated yet: i18n — fluent extraction lands in M4." \
        "Skip locally with SKIP_STEPS=\"doctests determinism\"."
}

while [ $# -gt 0 ]; do
    case "$1" in
        --quick) QUICK=1 ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            printf 'ci.sh: unknown argument %s\n\n' "$1" >&2
            usage >&2
            exit 2
            ;;
    esac
    shift
done

is_skipped() {
    case " $SKIP_STEPS " in
        *" $1 "*) return 0 ;;
    esac
    return 1
}

run_step() {
    name=$1
    shift
    if is_skipped "$name"; then
        printf '\n=== %s [skipped]\n' "$name"
        return 0
    fi
    printf '\n=== %s\n' "$name"
    start=$(date +%s)
    if "$@"; then
        elapsed=$(( $(date +%s) - start ))
        printf '%s\n' "--- $name: ok (${elapsed}s)"
        PASSED_STEPS="$PASSED_STEPS $name"
    else
        elapsed=$(( $(date +%s) - start ))
        printf '%s\n' "--- $name: FAILED (${elapsed}s)" >&2
        FAILED_STEPS="$FAILED_STEPS $name"
    fi
}

check_toolchain() {
    command -v cargo >/dev/null 2>&1 || {
        printf 'cargo not found in PATH\n' >&2
        return 1
    }
    command -v git >/dev/null 2>&1 || {
        printf 'git not found in PATH (needed for the determinism step)\n' >&2
        return 1
    }
    rustc --version
    cargo --version
    cargo fmt --version || {
        printf 'rustfmt missing: rustup component add rustfmt\n' >&2
        return 1
    }
    cargo clippy --version || {
        printf 'clippy missing: rustup component add clippy\n' >&2
        return 1
    }
    cargo nextest --version || {
        printf 'cargo-nextest missing: cargo install cargo-nextest --locked\n' >&2
        printf '(docs/design/testing.md §1 — the runner is not optional)\n' >&2
        return 1
    }
}

check_determinism() {
    # A test run must not rewrite committed fixtures or snapshots (docs/design/testing.md §3.1).
    dirty=$(git status --porcelain | grep -E '(tests/(fixtures|snapshots)/|\.snap(\.new)?$)' || true)
    if [ -n "$dirty" ]; then
        printf 'the test run modified committed fixtures or snapshots:\n%s\n' "$dirty" >&2
        printf 'review with `git diff -- crates/hatchery-protocol/tests/fixtures`, then commit the change intentionally\n' >&2
        return 1
    fi
}

GATE_START=$(date +%s)

run_step toolchain check_toolchain
run_step fmt cargo fmt --all --check
run_step clippy cargo clippy --workspace --all-targets -- -D warnings
run_step build cargo build --workspace

if [ "$QUICK" != "1" ]; then
    run_step tests cargo nextest run --workspace --profile ci
    # The invariant suite runs a second time under its own profile, on purpose: `ci` inherits
    # `default` and so happens to include it, which is how the profile stayed dead configuration
    # while the tests still ran. A named step is what makes the suite separately reportable — and
    # what fails if the `invariant_` prefix convention drifts and the filter selects nothing.
    run_step invariants cargo nextest run --workspace --profile invariants
    run_step doctests cargo test --workspace --doc
    run_step determinism check_determinism
    # Not a gate. Extraction lands in M4 with the po/ workflow (docs/worklog/platform.md); printed
    # rather than passed, so the absence stays visible instead of counting as a check that ran.
    printf '\n--- i18n: not gated yet (extraction lands in M4)\n'
fi

printf '\n======================================================\n'
if [ -n "$FAILED_STEPS" ]; then
    printf 'FAILED:%s (%ss)\n' "$FAILED_STEPS" "$(( $(date +%s) - GATE_START ))" >&2
    exit 1
fi
printf 'all gates passed:%s (%ss)\n' "$PASSED_STEPS" "$(( $(date +%s) - GATE_START ))"
