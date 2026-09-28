//! Session storage: append-only item tree, branches, writer actor.
//!
//! Layer **L2** (docs/architecture.md §3). The daemon depends on the `SessionStore` trait only;
//! the embedded SQL engine behind it is an implementation detail (ADR-0002), which keeps the
//! escape hatch to a future remote server open.
//!
//! Invariant 3 (items are append-only) is enforced in the database, not in Rust code.
//!
//! Design: `docs/design/storage.md`. Status: M0 skeleton — schema, writer actor and branch
//! operations land in M0b, after the engine spike has picked the SQL crate.
