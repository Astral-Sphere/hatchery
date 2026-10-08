# hatchery-capabilities

The capability seam (ADR-0004): `FsBackend`, `TerminalBackend`, `ApprovalGate`, and the
`ToolRegistry` that binds them to tools (it implements `kernel::ToolHost`).

Present today: `FsBackend` (`read_text_file` / `read_dir` / `metadata` / `write_text_file`) with
`LocalFs`, `TerminalBackend` with the refusing `NoTerminal`, the registry, and the shadow-git
checkpoint store (ADR-0006, ADR-0012) — `CheckpointStore` / `CheckpointPool`, plus `CheckpointedFs`,
the decorator that snapshots before every write and hands the undo points to the kernel.

`ApprovalGate` is declared but has no implementation yet. The PTY backend and `DaemonApproval` land
later in M2; registration handles (ADR-0009) are deferred to M5. Nothing calls the write path from
production code yet either — `write_file` and `edit` are the next phase, so the write path is
currently covered at the seam (real `LocalFs` over a real shadow repository) rather than end to end.

Binding a session to ACP-delegated backends instead of local ones is what makes full ACP
support — including host-side file and terminal delegation — possible (M3).

Workspace layer **L2** — see [docs/architecture.md](../../docs/architecture.md) §3.
Design: [docs/design/capabilities.md](../../docs/design/capabilities.md) ·
Worklog: [docs/worklog/capabilities.md](../../docs/worklog/capabilities.md)
