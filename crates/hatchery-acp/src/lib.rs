//! Agent Client Protocol server and client (docs/design/acp.md).
//!
//! Layer **L2** (docs/architecture.md §3). As a *server*, each ACP connection maps onto one
//! daemon runtime session, and the client's advertised capabilities decide which backends the
//! session binds: host-delegated `AcpClientFs` / `AcpClientTerminal` / `AcpPermission`, or the
//! local ones as fallback. As a *client*, hatchery spawns external harnesses and drives them as
//! subagents.
//!
//! The seam is what makes delegation possible at all — the reason atomcode's ACP v1 shipped
//! without file and terminal support is that its single in-process runtime had nowhere to bind
//! them (ADR-0004).
//!
//! Status: M0 skeleton; implementation lands in M3.
