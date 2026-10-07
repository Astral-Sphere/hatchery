# hatchery-tools

Built-in tools: `read_file`, `glob`, `grep` — the Chat-mode toolset assembled by `chat_tools()`.
Write and shell tools land in M2 (`write_file`, `edit`, `shell`, `web_fetch`, checkpoint
diff/rewind), `subagent` in M3, MCP-backed tools in M5.

Every tool reaches the outside world through `FsBackend` / `TerminalBackend` / `ApprovalGate`
— never through `std::fs` or `std::process`. That rule is enforced at compile time by
`clippy::disallowed_methods` (invariant 4, ADR-0004); the ban list still has a `tokio::fs` /
`tokio::process` hole, which M2 Phase 0 closes.

Workspace layer **L2** — see [docs/architecture.md](../../docs/architecture.md) §3.
Design: [docs/design/capabilities.md](../../docs/design/capabilities.md) §3 (the tool table) ·
Worklog: [docs/worklog/capabilities.md](../../docs/worklog/capabilities.md)
