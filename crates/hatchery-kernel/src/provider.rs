//! The provider seam.

use async_trait::async_trait;
use futures::stream::BoxStream;
use tokio_util::sync::CancellationToken;

use crate::error::LlmError;
use crate::message::{ChatOptions, Message, StreamEvent};

/// A streaming chat completion provider.
///
/// Implemented once per wire format by `hatchery-llm` (ADR-0007). The kernel never learns which
/// provider it is talking to: model names, effort ladders and reasoning fields are all the
/// adapter's business.
///
/// Two obligations an adapter carries, both because the kernel commits while it streams:
///
/// - The options and the conversation are **borrowed**. Read them while starting the request —
///   serialising both into the body happens before the first byte comes back — and do not hold the
///   borrows in the returned stream, which is `'static`. Clone inside if the request has to
///   outlive them. Handing over owned values instead would cost a copy of the whole transcript on
///   every round of every turn.
/// - [`StreamEvent::ReasoningDone`] must arrive **before the first `TextDelta` of the text that
///   follows that reasoning block**. The kernel keeps one item open at a time, so a text delta
///   closes — and commits — the reasoning item, and a signature delivered after that cannot be
///   attached to it, because items are append-only. The turn itself still replays correctly (the
///   signature is kept for the next request), but the stored item replays unsigned and the kernel
///   logs a warning.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Starts a streaming request.
    ///
    /// The cancellation token is how an interrupt aborts the HTTP request; dropping the returned
    /// stream is a fallback but does not always close the connection promptly.
    ///
    /// # Errors
    ///
    /// Fails when the request cannot be started at all (bad URL, refused connection, 401). A
    /// failure *after* the stream started arrives as [`StreamEvent::Error`] instead.
    async fn chat_stream(
        &self,
        options: &ChatOptions,
        messages: &[Message],
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, StreamEvent>, LlmError>;
}
