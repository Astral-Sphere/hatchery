//! Native desktop frontend: GTK4 + libadwaita (ADR-0008).
//!
//! Layer **D** (docs/architecture.md §3), a thin protocol client like the CLI. The rule that
//! shapes this crate is *logic out of GTK*: every state transformation (event stream → list
//! model, branch tree → visual structure, approval dialog state machine) is a plain Rust type
//! covered by unit tests, and GTK widgets only project them. That is also what makes the GUI
//! testable at all (docs/design/testing.md §3.9).
//!
//! Design: `docs/design/frontends.md` §3. Status: M0 skeleton — implementation lands in M4, so
//! `gtk4-rs` and `libadwaita` are deliberately not dependencies yet.
