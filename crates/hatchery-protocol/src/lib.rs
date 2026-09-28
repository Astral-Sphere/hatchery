//! Wire protocol and data model of the hatchery daemon.
//!
//! Layer **L0** ([docs/architecture.md](https://github.com/Astral-Sphere/hatchery/blob/main/docs/architecture.md) §3):
//! the CLI, the GTK frontend, headless exec and the ACP bridge are all JSON-RPC 2.0 clients
//! speaking these types. The daemon is the only runtime owner (ADR-0001).
//!
//! Design: `docs/design/protocol.md`. Status: M0 skeleton — types land in M0b.
