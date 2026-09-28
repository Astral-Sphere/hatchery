//! Built-in agent tools.
//!
//! Layer **L2** (docs/architecture.md §3). Tools implement the `Tool` trait from
//! `hatchery-capabilities` and receive everything they may touch through `ToolCtx`.
//!
//! Invariant 4 is enforced at compile time here: direct `std::fs` / `std::process` use is a
//! clippy error (see the workspace `clippy.toml`). At run time the same invariant is proven by
//! tests that inject only the in-memory backends from `hatchery-testkit`.
//!
//! Design: `docs/design/capabilities.md` §4. Status: M0 skeleton — read-only tools land in M1,
//! write/shell tools in M2.
#![deny(clippy::disallowed_methods)]
#![deny(clippy::disallowed_types)]
