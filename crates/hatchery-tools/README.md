# hatchery-tools

Built-in tools: `read_file`, `glob`, `grep`, `write_file`, `edit`, `shell`, `web_fetch`,
checkpoint diff/rewind, and later `subagent` and MCP-backed tools.

Every tool reaches the outside world through `FsBackend` / `TerminalBackend` / `ApprovalGate`
— never through `std::fs` or `std::process`. That rule is enforced at compile time by
`clippy::disallowed_methods` (invariant 4, ADR-0004).

Workspace layer **L2** — see [docs/architecture.md](../../docs/architecture.md) §3.
Design: [docs/design/capabilities.md](../../docs/design/capabilities.md) §4 ·
Worklog: [docs/worklog/capabilities.md](../../docs/worklog/capabilities.md)
