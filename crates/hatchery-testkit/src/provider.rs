//! A provider whose replies are a script.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio_util::sync::CancellationToken;

use hatchery_kernel::{ChatOptions, LlmError, LlmProvider, Message, StreamEvent};

use crate::gate::Gate;

/// One request the provider was asked to make.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordedRequest {
    /// The options as the kernel sent them.
    pub options: ChatOptions,
    /// The conversation as the kernel assembled it.
    pub messages: Vec<Message>,
}

/// A provider that replays scripted rounds and remembers what it was asked.
///
/// Used by the kernel's state-machine tests. Each call to `chat_stream` takes the next scripted
/// round; running out of script is an error rather than an empty stream, so a test that expects
/// more rounds than it scripted fails visibly instead of waiting for events that never come.
pub struct ScriptedProvider {
    rounds: Mutex<VecDeque<Vec<StreamEvent>>>,
    requests: Mutex<Vec<RecordedRequest>>,
    gate: Option<Gate>,
}

impl ScriptedProvider {
    /// A provider that answers with these rounds, in order.
    #[must_use]
    pub fn new(rounds: Vec<Vec<StreamEvent>>) -> Self {
        Self {
            rounds: Mutex::new(rounds.into()),
            requests: Mutex::new(Vec::new()),
            gate: None,
        }
    }

    /// One round that answers with text.
    #[must_use]
    pub fn text_round(text: &str) -> Vec<StreamEvent> {
        vec![
            StreamEvent::TextDelta {
                text: text.to_owned(),
            },
            StreamEvent::Done {
                finish_reason: hatchery_kernel::FinishReason::Stop,
            },
        ]
    }

    /// One round that asks for a tool.
    #[must_use]
    pub fn tool_round(id: &str, name: &str, args: serde_json::Value) -> Vec<StreamEvent> {
        vec![
            StreamEvent::ToolCall {
                delta: hatchery_kernel::ToolCallDelta {
                    index: 0,
                    id: Some(id.to_owned()),
                    name: Some(name.to_owned()),
                    args_delta: args.to_string(),
                },
            },
            StreamEvent::Done {
                finish_reason: hatchery_kernel::FinishReason::ToolCalls,
            },
        ]
    }

    /// Makes every streamed event wait for a permit, so a test can act mid-stream.
    #[must_use]
    pub fn gated(self) -> (Self, Gate) {
        let gate = Gate::new();
        (
            Self {
                gate: Some(gate.clone()),
                ..self
            },
            gate,
        )
    }

    /// Every request the provider received, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests
            .lock()
            .expect("the mutex is never poisoned")
            .clone()
    }

    /// The messages of the last request.
    #[must_use]
    pub fn last_messages(&self) -> Vec<Message> {
        self.requests
            .lock()
            .expect("the mutex is never poisoned")
            .last()
            .map(|request| request.messages.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl LlmProvider for ScriptedProvider {
    async fn chat_stream(
        &self,
        options: ChatOptions,
        messages: Vec<Message>,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, StreamEvent>, LlmError> {
        let script = {
            let mut rounds = self.rounds.lock().expect("the mutex is never poisoned");
            rounds.pop_front()
        };
        self.requests
            .lock()
            .expect("the mutex is never poisoned")
            .push(RecordedRequest { options, messages });

        let Some(script) = script else {
            return Err(LlmError::fatal(
                "ScriptedProvider ran out of scripted rounds; the test scripted too few",
            ));
        };

        let gate = self.gate.clone();
        let stream = futures::stream::iter(script)
            .then(move |event| {
                let gate = gate.clone();
                let cancel = cancel.clone();
                async move {
                    if let Some(gate) = gate {
                        tokio::select! {
                            biased;
                            // A cancelled stream ends rather than handing over the rest of the
                            // script: that is what a real provider's aborted request does.
                            _ = cancel.cancelled() => return None,
                            _ = gate.acquire() => {}
                        }
                    }
                    Some(event)
                }
            })
            .filter_map(|event| async move { event });
        Ok(stream.boxed())
    }
}
