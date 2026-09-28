//! LLM provider adapters implementing the kernel's `LlmProvider` trait.
//!
//! Layer **L1** (docs/architecture.md §3), built solely on `openai-interface` (ADR-0007): this
//! crate is the only place where kernel messages are translated to and from wire types, and the
//! only place that knows about provider quirks — reasoning field names, effort ladders,
//! signature blocks.
//!
//! Two disciplines are load-bearing here:
//!
//! - **Reasoning passback is byte-exact.** Whatever the model emitted is stored and replayed
//!   verbatim; normalising whitespace would break provider-side KV-cache reuse.
//! - **Behaviour is measured, not assumed.** Adapter tests replay recorded SSE fixtures, and
//!   `hatchery doctor --provider` probes the real endpoint.
//!
//! Design: `docs/design/llm.md`. Status: M0 skeleton — the Chat Completions adapter, effort
//! mapping table and fixture recording land in M1.
