//! LLM provider adapters implementing the kernel's `LlmProvider` trait.
//!
//! Layer **L2** (docs/architecture.md §3), built solely on `openai-interface` (ADR-0007): this
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
//! The adapter owns retries for requests that fail to *start* (network, 5xx, 429: exponential
//! backoff, announced as `RateLimited` stream events) and reports mid-stream failures as error
//! events without restarting — half a round the user watched is already committed. Every
//! provider quirk is a row in the [capability table](capability), overridable from config and
//! audited against reality by `doctor`.
//!
//! # Example
//!
//! One provider, one round, the way the daemon assembles it:
//!
//! ```no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use futures::StreamExt;
//! use hatchery_kernel::{ChatOptions, LlmProvider};
//! use hatchery_llm::{ProviderConfig, ProviderRegistry};
//!
//! let registry = ProviderRegistry::new();
//! let _registration = registry.register("deepseek", ProviderConfig::builtin("deepseek"));
//! let provider = registry.resolve_model("deepseek-flash").expect("registered");
//!
//! let options = ChatOptions::new("deepseek-flash");
//! let messages = vec![hatchery_kernel::Message::user("hello")];
//! let cancel = tokio_util::sync::CancellationToken::new();
//! let mut stream = provider.chat_stream(&options, &messages, cancel).await?;
//! while let Some(event) = stream.next().await {
//!     println!("{event:?}");
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Design: `docs/design/llm.md`.

mod capability;
mod config;
mod error;
mod provider;
mod registry;
mod translate;

pub use capability::{CapabilityTable, ModelCapabilities, ReasoningWire, qwen_thinking_budget};
pub use config::{ProviderConfig, ReasoningConfig, RetryPolicy, WireApi};
pub use provider::ChatCompletionsProvider;
pub use registry::{ProviderRegistry, Registration};

/// Installs the pure-Rust TLS backend as the process default.
///
/// `openai-interface`'s reqwest build ships without a crypto provider, so exactly one must be
/// installed before the first HTTPS client is built. This delegates to the `ferritls` feature's
/// backend; first install wins, so an application that picked its own keeps it. The daemon and
/// the CLI call this once at startup.
pub fn install_tls_provider() {
    // A provider somebody else already installed is kept — that is the documented contract,
    // not a failure worth reporting.
    let _ = openai_interface::rest::install_crypto_provider();
}
