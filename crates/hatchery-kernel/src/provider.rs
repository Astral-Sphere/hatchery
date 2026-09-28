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
        options: ChatOptions,
        messages: Vec<Message>,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, StreamEvent>, LlmError>;
}
