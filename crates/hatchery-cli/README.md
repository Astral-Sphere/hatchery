# hatchery-cli

The `hatchery` binary: a ratatui TUI and a headless `exec` mode. Both are thin JSON-RPC clients
of the daemon — the TUI embeds no agent runtime, it only projects the event stream
(ADR-0001).

Workspace layer **D** — see [docs/architecture.md](../../docs/architecture.md) §3.
Design: [docs/design/frontends.md](../../docs/design/frontends.md) §2 ·
Worklog: [docs/worklog/cli.md](../../docs/worklog/cli.md)
