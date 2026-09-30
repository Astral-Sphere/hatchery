# hatchery-kernel

The neutral agent turn loop: assemble context → stream an LLM response → execute tool calls
through the capability seam → feed results back → repeat until the turn terminates.

It knows nothing about Chat/Code modes, workspaces, storage formats, frontends or ACP.

Workspace layer **L0** — see [docs/architecture.md](../../docs/architecture.md) §3.
Design: [docs/design/kernel.md](../../docs/design/kernel.md) ·
Worklog: [docs/worklog/kernel.md](../../docs/worklog/kernel.md)
