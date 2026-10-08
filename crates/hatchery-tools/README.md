# hatchery-tools

Built-in tools: `read_file`, `glob`, `grep` — the Chat-mode toolset assembled by `chat_tools()`.
Write and shell tools land in M2 (`write_file`, `edit`, `shell`, `web_fetch`, checkpoint
diff/rewind), `subagent` in M3, MCP-backed tools in M5.

Every tool reaches the outside world through `FsBackend` / `TerminalBackend` / `ApprovalGate`
— never through `std::fs` or `std::process`. That rule is enforced at compile time by
`clippy::disallowed_methods` (invariant 4, ADR-0004). M2 Phase 0 closed the `tokio::fs` half of
the ban list; the `tokio::process` twins are deliberately absent (no crate enables that feature,
so clippy would warn about an unreachable path on every gate run — see the header of
`clippy.toml`).

Workspace layer **L3** — see [docs/architecture.md](../../docs/architecture.md) §3.
Design: [docs/design/capabilities.md](../../docs/design/capabilities.md) §3 (the tool table) ·
Worklog: [docs/worklog/capabilities.md](../../docs/worklog/capabilities.md)
