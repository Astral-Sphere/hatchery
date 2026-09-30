//! Terminal frontend: TUI and headless exec.
//!
//! Layer **D** (docs/architecture.md §3). The CLI attaches to the user's daemon (spawning it if
//! needed) and speaks `hatchery-protocol`; it holds no runtime state of its own. Slash-command
//! handling is kept in this library as pure functions so it can be tested without a terminal,
//! while rendering stays in the ratatui layer and is covered by golden-frame snapshots
//! (docs/design/testing.md §3.8).
//!
//! Design: `docs/design/frontends.md` §2. Status: M0 skeleton — the TUI and `exec` land in M1,
//! so `ratatui` is deliberately not a dependency yet.
