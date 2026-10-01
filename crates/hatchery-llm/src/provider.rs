//! The Chat Completions adapter: [`LlmProvider`] over `openai-interface` (ADR-0007).

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use hatchery_kernel::{ChatOptions, LlmError, LlmProvider, Message, StreamEvent};
use openai_interface::chat::create::request::RequestBody;
use openai_interface::chat::create::response::streaming::ChatCompletionChunk;
use openai_interface::errors::OapiError;
use openai_interface::rest::RequestOptions;
use openai_interface::rest::post::PostStream;
use tokio_util::sync::CancellationToken;

use crate::capability::CapabilityTable;
use crate::config::{ProviderConfig, RetryPolicy};
use crate::error::{self, Verdict};
use crate::translate;

/// The deserialised SSE stream the wire layer hands back, boxed so the retry state machine can
/// hold it without naming the wire layer's anonymous type.
type ChunkStream =
    Pin<Box<dyn futures::Stream<Item = Result<ChatCompletionChunk, OapiError>> + Send>>;

/// One POST in flight, boxed the same way: the future owns clones of everything it reads, which
/// is what lets it be `'static` while the wire layer's own future borrows its arguments.
type StartFuture =
    Pin<Box<dyn std::future::Future<Output = Result<ChunkStream, OapiError>> + Send>>;

/// A provider speaking Chat Completions at one endpoint.
#[derive(Clone)]
pub struct ChatCompletionsProvider {
    config: ProviderConfig,
    table: CapabilityTable,
    client: reqwest::Client,
}

impl ChatCompletionsProvider {
    /// Builds a provider with the library-default HTTP client (60 s connect / 300 s read).
    #[must_use]
    pub fn new(config: ProviderConfig) -> Self {
        Self::with_client(config, openai_interface::rest::default_client())
    }

    /// Builds a provider on a caller-supplied client, for proxies and test doubles alike.
    #[must_use]
    pub fn with_client(config: ProviderConfig, client: reqwest::Client) -> Self {
        let table = config
            .capabilities
            .iter()
            .fold(CapabilityTable::builtin(), |table, (prefix, caps)| {
                table.with_override(prefix.clone(), caps.clone())
            });
        Self {
            config,
            table,
            client,
        }
    }

    /// The config this provider was built from.
    #[must_use]
    pub fn config(&self) -> &ProviderConfig {
        &self.config
    }

    /// Builds the per-request wire options: bearer key plus the configured headers.
    fn request_options(&self, api_key: String) -> Result<RequestOptions, LlmError> {
        let mut options = RequestOptions::bearer(api_key);
        for (name, value) in &self.config.http_headers {
            options = options.with_header(name, value).map_err(|failure| {
                LlmError::fatal(format!("configured header `{name}` is unusable: {failure}"))
            })?;
        }
        Ok(options)
    }
}

#[async_trait]
impl LlmProvider for ChatCompletionsProvider {
    async fn chat_stream(
        &self,
        options: &ChatOptions,
        messages: &[Message],
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, StreamEvent>, LlmError> {
        if self.config.base_url.is_empty() {
            return Err(LlmError::fatal("the provider has no base_url configured"));
        }
        let api_key = self.config.api_key().map_err(LlmError::fatal)?;

        // The borrowed window ends here: body and options are owned from now on, and the stream
        // never sees the kernel's slices (docs/design/kernel.md §4).
        let body = translate::request_body(&self.config, &self.table, options, messages);
        let request_options = self.request_options(api_key)?;

        let first = start(&self.client, &self.config.base_url, &body, &request_options);
        let stream = RetryStream {
            client: self.client.clone(),
            base_url: self.config.base_url.clone(),
            body,
            options: request_options,
            policy: self.config.retry.clone(),
            env_key: self.config.env_key.clone(),
            cancel,
            state: State::Starting {
                attempt: 1,
                future: first,
            },
            pending: VecDeque::new(),
            deferred_done: None,
            in_reasoning: false,
        };
        Ok(stream.boxed())
    }
}

/// Starts one POST attempt, owning everything it reads.
fn start(
    client: &reqwest::Client,
    base_url: &str,
    body: &RequestBody,
    options: &RequestOptions,
) -> StartFuture {
    let client = client.clone();
    let url = base_url.to_owned();
    let body = body.clone();
    let options = options.clone();
    Box::pin(async move {
        let stream = body.get_stream_response(&client, &url, &options).await?;
        Ok(Box::pin(stream) as ChunkStream)
    })
}

/// What the retry machine is doing right now.
enum State {
    /// Attempt `attempt` is in flight.
    Starting { attempt: u32, future: StartFuture },
    /// A retryable failure happened; waiting out the backoff before the next attempt.
    Sleeping {
        attempt: u32,
        sleep: Pin<Box<tokio::time::Sleep>>,
    },
    /// The stream is live.
    Streaming { inner: ChunkStream },
    /// Nothing more will come.
    Finished,
}

/// The stream the kernel holds: connection attempts with backoff, then chunk translation.
///
/// Cancellation is the kernel's to trigger and this machine's to honour: it checks the token on
/// every poll and, more to the point, owns every in-flight future — the kernel drops this stream
/// on interrupt, and the drop tears the HTTP connection down with it.
struct RetryStream {
    client: reqwest::Client,
    base_url: String,
    body: RequestBody,
    options: RequestOptions,
    policy: RetryPolicy,
    env_key: String,
    cancel: CancellationToken,
    state: State,
    pending: VecDeque<StreamEvent>,
    /// A `Done` held back until the usage-only chunk has had its chance: the kernel stops at
    /// `Done`, and usage arrives *after* `finish_reason` on every provider that reports it.
    deferred_done: Option<StreamEvent>,
    in_reasoning: bool,
}

impl RetryStream {
    /// Queues one chunk's events, keeping `Done` behind a possible usage-only chunk.
    fn queue_chunk_events(&mut self, mut events: Vec<StreamEvent>) {
        let done_at = events
            .iter()
            .position(|event| matches!(event, StreamEvent::Done { .. }));
        let has_usage = events
            .iter()
            .any(|event| matches!(event, StreamEvent::Usage { .. }));

        if let (Some(index), false) = (done_at, has_usage) {
            let done = events.remove(index);
            self.pending.extend(events);
            self.deferred_done = Some(done);
            return;
        }

        if let Some(done) = self.deferred_done.take() {
            // The held-back Done is released by the next chunk: usage first, so the round's cost
            // is booked before the kernel stops reading, then the Done itself.
            let (usage, rest): (Vec<_>, Vec<_>) = events
                .into_iter()
                .partition(|event| matches!(event, StreamEvent::Usage { .. }));
            self.pending.extend(usage);
            self.pending.push_back(done);
            self.pending.extend(rest);
        } else {
            self.pending.extend(events);
        }
    }

    /// Handles a failure to *start*: either schedules the next attempt (returning the
    /// `RateLimited` notice to show while it sleeps) or ends the stream with the final error.
    fn startup_failure(&mut self, failure: OapiError, attempt: u32) -> StreamEvent {
        match error::startup(failure, &self.env_key) {
            Verdict::Retry if attempt < self.policy.max_attempts => {
                let backoff = self.policy.backoff_for(attempt + 1);
                self.state = State::Sleeping {
                    attempt: attempt + 1,
                    sleep: Box::pin(tokio::time::sleep(backoff)),
                };
                StreamEvent::RateLimited {
                    retry_after_ms: backoff.as_millis() as u64,
                }
            }
            Verdict::Retry => {
                self.state = State::Finished;
                StreamEvent::Error {
                    error: LlmError::retryable(format!(
                        "still failing after {} attempt(s)",
                        self.policy.max_attempts
                    )),
                }
            }
            Verdict::Fatal(error) => {
                self.state = State::Finished;
                StreamEvent::Error { error }
            }
        }
    }
}

impl futures::Stream for RetryStream {
    type Item = StreamEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(event) = self.pending.pop_front() {
            return Poll::Ready(Some(event));
        }
        if self.cancel.is_cancelled() {
            self.state = State::Finished;
            return Poll::Ready(None);
        }

        // The state is taken out of the slot for the duration of one step, which is what lets a
        // step start a new attempt (`&self`) while the old state is still being matched on.
        loop {
            match std::mem::replace(&mut self.state, State::Finished) {
                State::Starting {
                    attempt,
                    mut future,
                } => match future.as_mut().poll(cx) {
                    Poll::Ready(Ok(inner)) => {
                        self.state = State::Streaming { inner };
                    }
                    Poll::Ready(Err(failure)) => {
                        return Poll::Ready(Some(self.startup_failure(failure, attempt)));
                    }
                    Poll::Pending => {
                        self.state = State::Starting { attempt, future };
                        return Poll::Pending;
                    }
                },
                State::Sleeping { attempt, mut sleep } => match sleep.as_mut().poll(cx) {
                    Poll::Ready(()) => {
                        self.state = State::Starting {
                            attempt,
                            future: start(&self.client, &self.base_url, &self.body, &self.options),
                        };
                    }
                    Poll::Pending => {
                        self.state = State::Sleeping { attempt, sleep };
                        return Poll::Pending;
                    }
                },
                State::Streaming { mut inner } => match inner.as_mut().poll_next(cx) {
                    Poll::Ready(Some(Ok(chunk))) => {
                        let events = translate::events_from_chunk(&chunk, &mut self.in_reasoning);
                        self.state = State::Streaming { inner };
                        if events.is_empty() {
                            continue;
                        }
                        self.queue_chunk_events(events);
                        if let Some(event) = self.pending.pop_front() {
                            return Poll::Ready(Some(event));
                        }
                    }
                    Poll::Ready(Some(Err(failure))) => match failure {
                        // One malformed chunk is the provider's hiccup, not the round's end;
                        // the wire layer surfaced it as an item precisely so it can be dropped
                        // here (its own `skip_deserialization_errors` does the same).
                        OapiError::DeserializationError(cause) => {
                            tracing::warn!("skipping an unparseable chunk: {cause}");
                            self.state = State::Streaming { inner };
                            continue;
                        }
                        other => {
                            self.state = State::Finished;
                            return Poll::Ready(Some(StreamEvent::Error {
                                error: error::mid_stream(&other),
                            }));
                        }
                    },
                    Poll::Ready(None) => {
                        // End without Done is the kernel's to judge: it fails the round as
                        // retryable while keeping the items it already committed.
                        self.state = State::Finished;
                        if let Some(done) = self.deferred_done.take() {
                            return Poll::Ready(Some(done));
                        }
                        return Poll::Ready(None);
                    }
                    Poll::Pending => {
                        self.state = State::Streaming { inner };
                        return Poll::Pending;
                    }
                },
                State::Finished => return Poll::Ready(None),
            }
        }
    }
}
