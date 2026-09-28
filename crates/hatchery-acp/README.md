# hatchery-acp

[Agent Client Protocol](https://agentclientprotocol.com/) in both directions:

- **Server** — hatchery driven by a host such as Zed, including host-side file delegation
  (`fs/read_text_file`, `fs/write_text_file`) and host-side terminal delegation
  (`terminal/create|output|wait_for_exit|release`). Those two are exactly what ADR-0004 exists
  for: the capability seam lets a session bind its backends to the ACP connection.
- **Client** — external harnesses orchestrated as subagents through a tool.

Workspace layer **L2** — see [docs/architecture.md](../../docs/architecture.md) §3.
Design: [docs/design/acp.md](../../docs/design/acp.md) ·
Worklog: [docs/worklog/acp.md](../../docs/worklog/acp.md)
