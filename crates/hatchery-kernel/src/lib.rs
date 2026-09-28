//! Neutral agent turn loop.
//!
//! Layer **L0**, zero business semantics (docs/architecture.md §3): the kernel drives
//! `assemble → LLM stream → tool calls → results → loop` and talks to the outside world only
//! through injected traits (`LlmProvider`, `HistorySource`, `EventSink`, `ToolHost`).
//! Modes, workspaces, storage and ACP are all daemon-side concerns.
//!
//! Design: `docs/design/kernel.md`. Status: M0 skeleton — the state machine lands in M0b.
