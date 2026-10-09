# hatchery-store

Session storage on top of an embedded SQL engine (ADR-0002): an append-only `items` tree,
edit-as-fork branches (ADR-0003), a single writer actor and concurrent read-only connections.

Workspace layer **L2** — see [docs/architecture.md](../../docs/architecture.md) §3.
Design: [docs/design/storage.md](../../docs/design/storage.md) ·
Worklog: [docs/worklog/storage.md](../../docs/worklog/storage.md)
