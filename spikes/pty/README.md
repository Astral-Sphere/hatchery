# D10 PTY spike

A measuring instrument for decision **D10** (`docs/roadmap.md` → 决策点, risk 1), not a test and
not a product crate.

```sh
cargo run --manifest-path spikes/pty/Cargo.toml
```

Full run ≈ 40 s. It prints which platform it is on, so the same binary yields a comparable report
on macOS and Windows. It always exits 0 — a failed measurement is itself a result, and a non-zero
exit would only obscure it. Every pid it causes to exist is registered and hard-killed before exit,
including through a panic hook.

## Why this directory is its own cargo workspace

The empty `[workspace]` table in `Cargo.toml` is load-bearing. It makes this crate its own workspace
root, so:

- `cargo metadata` / `cargo build --workspace` from the repository root never see it;
- the project's `Cargo.lock` never gains `portable-pty`;
- no gate, profile or test runner can pick it up (there are no `#[test]`s and no `tests/`).

D10 is still open. A dependency must not enter the project's lock file before the decision that
would justify it has been made. Verified: `cargo metadata --no-deps` lists 13 members and not
`pty-spike`; `grep -c portable-pty Cargo.lock` at the root is 0.

## What is in here

| file | what it is |
|---|---|
| `src/main.rs` | orchestrator: banner → M1…M6 → cleanup proof |
| `src/measure.rs` | the seven measurement groups |
| `src/session.rs` | the portable-pty call surface + the reader thread |
| `src/platform.rs` | observation instruments (`kill(pid,0)`, `getpgid`, `getsid`, `FIONREAD`, `/proc` state and `SigIgn`, `/proc/<pid>/task/*/children`) and the per-platform payloads |
| `src/report.rs` | the `OBSERVED` / `HAZARD` / `VERDICT` / `READ-FROM-SOURCE` printers |
| `measured-linux.txt` | **verbatim** output of one Linux run — evidence, reproducible by re-running |

The report labels every line. `OBSERVED` / `HAZARD` / `VERDICT` were produced by running something on
the machine that produced the file. `READ-FROM-SOURCE` was read out of
`references/codex/codex-rs/utils/pty/src/` or out of the unpacked `portable-pty-0.9.0` crate, and is
**not** evidence about behaviour. The two categories are kept apart because a decision built on the
second while believing it is the first is how a spike fails.

## Platform payload status — read this before trusting a non-Linux number

- **Linux**: measured. `measured-linux.txt` is the transcript.
- **macOS**: the payloads reuse the POSIX scripts unchanged, except that `setsid` is guarded with
  `command -v setsid` because macOS does not ship it. So the `setsid` case may report "payload
  unavailable" rather than a result, and the rest should be comparable. Not yet run.
- **Windows**: the payloads are PowerShell strings written from documentation and **never executed**.
  Expect to fix them before the run means anything, and treat every Windows number they produce as
  unverified until somebody has read the payload and the output together.

This matters because D10's whole content is the per-platform divergence: the disagreement that
decides the library is Windows' Job Object semantics, and that is exactly the column with the
weakest instrument.

## M6 — build cost, measured outside the binary

A binary cannot time its own compilation, so `measured-linux.txt`'s M6 section points here instead of
carrying numbers. These were measured on the same machine as that run (Linux x86_64, rustc 1.99.0
stable, dependencies already in `~/.cargo/registry`):

```sh
# cold build, empty target directory
CARGO_TARGET_DIR=$(mktemp -d) /usr/bin/time -v cargo build --manifest-path spikes/pty/Cargo.toml
#   wall = 1.74 s   maxrss = 259108 kB   warnings = 0
cargo build --manifest-path spikes/pty/Cargo.toml      # warm: 0.01 s
cargo tree --manifest-path spikes/pty/Cargo.toml | wc -l   # 29
wc -l < spikes/pty/Cargo.lock                          # 28 packages
# MSRV mirror of the project's gate
CARGO_TARGET_DIR=$(mktemp -d) cargo +1.90.0 build --locked --manifest-path spikes/pty/Cargo.toml
#   OK, 2.09 s, exit 0
```

Crates the **root gate** would newly compile if `portable-pty` were added, diffed against
`cargo tree --workspace -e normal,build` (root graph = 320 crates; `anyhow`, `libc`, `log`, `cfg-if`,
`bitflags`, `thiserror`, `syn`/`quote`/`proc-macro2` are already built):

- **Linux: 6** — `portable-pty`, `nix`, `filedescriptor`, `downcast-rs`, `serial2`, `shell-words`.
- **windows-msys2-ucrt64: 8** — those 6 plus `shared_library` and `winreg`; `winapi`,
  `winapi-x86_64-pc-windows-gnu`, `windows-sys`, `windows-link`, `lazy_static` are already present,
  though portable-pty's `winapi` feature set would unify into the existing build and grow it.

`serial2` is dead weight for us: portable-pty depends on it unconditionally for its serial-port
backend, which a PTY-only consumer never touches. MSRV is not a problem — portable-pty declares
`edition = "2018"` and no `rust-version`, and the whole graph builds under `+1.90.0 --locked`.

**Verdict: build cost is not a reason to reject portable-pty** — ~1.7 s cold and 6 new crates against
a 320-crate workspace that already vendors libgit2.

## What the Linux run settled, and what it did not

Settled (all in `measured-linux.txt`): streaming before exit works and is genuinely incremental;
`kill -0` **cannot** distinguish a zombie from a live orphan; `Child::kill()` signals one pid and 3 of
5 grandchild scenarios survive it; the PTY's ONLCR rewrites LF to CRLF so captured output is not
byte-identical to the child's stdout; a reader never sees EOF while the parent holds the slave fd; the
kernel backpressures at 4095 bytes so a stalled consumer freezes the child; `CommandBuilder` defaults
cwd to `$HOME`; `clone_killer()` really does release a thread blocked in `wait()`; output transport is
lossless and the line discipline costs 13.5× throughput on newline-heavy payloads.

Not settled: everything about macOS and Windows, and therefore D10 itself. The recommendation the
Linux column supports is "portable-pty is a sound PTY and streaming substrate and an inadequate
process-tree containment substrate, so a wrapper is mandatory rather than optional" — which is also
where codex landed, except that on Windows they **replaced** the library's child with their own ConPTY
implementation rather than wrapping it (portable-pty 0.9.0 contains no Job Object code at all).

Two document corrections came out of this and were applied to `docs/`:
`design/testing.md` §3.5 no longer recommends `kill -0` as the no-orphan assertion, and
`design/capabilities.md` §1 records that `TerminalHandle::release()` cannot mean "hand back a live
process" on a PTY backend.
