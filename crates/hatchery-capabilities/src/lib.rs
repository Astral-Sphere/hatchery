//! Capability seam: filesystem, terminal and approval backends, shadow-git checkpoints.
//!
//! Layer **L1** (docs/architecture.md §3). This crate owns the `Tool` / `ToolCtx` traits and the
//! `FsBackend` / `TerminalBackend` / `ApprovalGate` seams; the kernel only sees the narrow
//! `ToolHost` trait, which keeps L0 free of any dependency on L1 (see ADR-0004).
//!
//! A session binds exactly one backend set at `session/new`: local (`LocalFs` + shadow-git
//! checkpoints, `LocalPty`, `DaemonApproval`) or ACP-delegated (`AcpClientFs`,
//! `AcpClientTerminal`, `AcpPermission`). Invariant 6 — shadow git never touches the user's own
//! repository — is enforced here and tested in `tests/`.
//!
//! Design: `docs/design/capabilities.md`. Status: M0 skeleton; the git spike lands in M0a and
//! local implementations in M1–M2.
