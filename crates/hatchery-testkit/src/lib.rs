//! Test infrastructure for the hatchery workspace (docs/design/testing.md §2).
//!
//! Dev-only crate, never published: referenced as a `dev-dependency` by the crates under test.
//! Fakes live here so they are written once instead of per crate.
//!
//! Status: M0 skeleton. The M0b subset is `ScriptedProvider`, `MemoryHistory`, `RecordingSink`,
//! `ScriptedToolHost`, `fixture` and `assert_golden`; `MemoryFs` / `MemoryTerminal` /
//! `TestDaemon` / `ClientProbe` land in M1–M2.
