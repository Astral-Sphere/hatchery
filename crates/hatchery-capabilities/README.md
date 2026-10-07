# hatchery-capabilities

The capability seam (ADR-0004): `FsBackend`, `TerminalBackend`, `ApprovalGate`, and the
`ToolRegistry` that binds them to tools (it implements `kernel::ToolHost`).

Present today is the M1 read-only slice: `FsBackend` (`read_text_file` / `read_dir` /
`metadata`) with `LocalFs`, `TerminalBackend` with the refusing `NoTerminal`, and the registry.
`ApprovalGate` is declared but has no implementation yet. Shadow-git checkpoints (ADR-0006), the
`FsBackend` write path, the PTY backend and `DaemonApproval` land in M2; registration handles
(ADR-0009) are deferred to M5.

Binding a session to ACP-delegated backends instead of local ones is what makes full ACP
support — including host-side file and terminal delegation — possible (M3).

Workspace layer **L1** — see [docs/architecture.md](../../docs/architecture.md) §3.
Design: [docs/design/capabilities.md](../../docs/design/capabilities.md) ·
Worklog: [docs/worklog/capabilities.md](../../docs/worklog/capabilities.md)
