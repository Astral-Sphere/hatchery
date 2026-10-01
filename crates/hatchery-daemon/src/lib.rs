//! Runtime host: the single owner of agent runtimes and of the database write path.
//!
//! Layer **L4** (docs/architecture.md §3). The daemon discovers or spawns itself as a
//! single instance per user, accepts JSON-RPC clients over UDS and stdio, assembles one runtime
//! per session, and fans every event out through the live hub so that several frontends can watch
//! the same session. Leases plus a monotonic generation number keep a stale runtime's late events
//! from polluting its replacement (invariant 1).
//!
//! Assembly is profile-based and audited at startup: a missing required component means the
//! daemon refuses to serve and prints the list of what is missing — never a silent runtime
//! failure (ADR-0009). Teardown runs disposers in strict reverse registration order.
//!
//! Layered configuration and prompt assembly also live here: frontends only ever reach
//! configuration through `config/get` and `config/set`, so no second consumer exists
//! (docs/design/platform.md).
//!
//! Design: `docs/design/daemon.md`. Status: M1 — configuration layering, the prompt pipeline,
//! transport, session manager, hub and the production entry (`entry`) are in.

pub mod clock;
pub mod config;
pub mod core;
pub mod discover;
pub mod doctor;
pub mod entry;
pub mod hub;
pub mod logging;
pub mod manager;
pub mod prompt;
pub mod runtime;
pub mod server;
pub mod template;
