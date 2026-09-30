# hatchery-capabilities

The capability seam (ADR-0004): `FsBackend`, `TerminalBackend`, `ApprovalGate` and their local
implementations, shadow-git checkpoints (ADR-0006), and the tool registry with registration
handles (ADR-0009).

Binding a session to ACP-delegated backends instead of local ones is what makes full ACP
support — including host-side file and terminal delegation — possible.

Workspace layer **L1** — see [docs/architecture.md](../../docs/architecture.md) §3.
Design: [docs/design/capabilities.md](../../docs/design/capabilities.md) ·
Worklog: [docs/worklog/capabilities.md](../../docs/worklog/capabilities.md)
