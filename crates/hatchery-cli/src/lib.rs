//! Terminal frontend: TUI chat and headless exec, both as thin protocol clients.
//!
//! Layer **D** (docs/architecture.md §3). The CLI attaches to the user's daemon (spawning it
//! if needed, D1) and speaks `hatchery-protocol`; it holds no runtime state of its own. The
//! command layer (`commands`) and the TUI model (`tui::Model`) are pure functions, tested
//! without a terminal; rendering is verified by golden frames over `ratatui`'s TestBackend
//! (docs/design/testing.md §3.8).
//!
//! Design: `docs/design/frontends.md` §2. Status: M1 — chat TUI, `exec`, the `daemon`
//! subcommands and `doctor` are in; the ACP entry, approval dialogs and session management
//! land in M2.

pub mod args;
pub mod attach;
pub mod chat;
pub mod commands;
pub mod daemon_cmd;
pub mod doctor_cmd;
pub mod exec;
pub mod markdown;
pub mod tui;
