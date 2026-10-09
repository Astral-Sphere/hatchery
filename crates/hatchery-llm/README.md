# hatchery-llm

Provider adapters on top of [`openai-interface`](https://crates.io/crates/openai-interface):
the Chat Completions wire, per-provider reasoning-effort mapping, and byte-exact
`reasoning_content` passback (ADR-0007). `wire = "responses"` is refused with a fatal error
rather than silently downgraded; that adapter lands later in M2.

Workspace layer **L2** — see [docs/architecture.md](../../docs/architecture.md) §3.
Design: [docs/design/llm.md](../../docs/design/llm.md) ·
Worklog: [docs/worklog/llm.md](../../docs/worklog/llm.md)
