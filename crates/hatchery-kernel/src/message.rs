//! The provider-facing vocabulary: messages, request options and stream events.
//!
//! These are the kernel's own types, not wire types. `hatchery-llm` translates between them and
//! `openai-interface`, and no provider type ever appears on this side of the seam (ADR-0007).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use hatchery_protocol::{Content, ReasoningBlock, ReasoningEffort, SignatureBlock, Usage};

use crate::error::LlmError;

/// Who a message is from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Instructions assembled by the daemon, never shown as a turn.
    System,
    /// The human.
    User,
    /// The model.
    Assistant,
    /// A tool result being handed back to the model.
    Tool,
}

/// One message in a provider request.
///
/// Deliberately flatter than the protocol's item tree: a provider request is a list, and the
/// daemon's assembler is what turns the tree into one (`docs/design/kernel.md` §6).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// Who is speaking.
    pub role: Role,
    /// What was said.
    pub content: Content,
    /// Reasoning to replay verbatim (ADR-0007): the text the provider emitted, plus any opaque
    /// signature it issued for it. Only meaningful on assistant messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningBlock>,
    /// Tool calls the assistant asked for in this message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCallRequest>,
    /// For a tool result: which call it answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// For a tool result: whether the call failed. The model is told either way, so this is a
    /// hint for its next decision rather than an error channel.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
}

impl Message {
    /// The system prompt.
    #[must_use]
    pub fn system(text: impl Into<String>) -> Self {
        Self::plain(Role::System, Content::text(text))
    }

    /// A user message.
    #[must_use]
    pub fn user(content: impl Into<Content>) -> Self {
        Self::plain(Role::User, content.into())
    }

    /// An assistant message.
    #[must_use]
    pub fn assistant(content: impl Into<Content>) -> Self {
        Self::plain(Role::Assistant, content.into())
    }

    /// A tool result being handed back.
    #[must_use]
    pub fn tool_result(
        call_id: impl Into<String>,
        content: impl Into<Content>,
        is_error: bool,
    ) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            reasoning: None,
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
            is_error,
        }
    }

    fn plain(role: Role, content: Content) -> Self {
        Self {
            role,
            content,
            reasoning: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            is_error: false,
        }
    }

    /// Attaches the reasoning the assistant produced, for verbatim replay.
    #[must_use]
    pub fn with_reasoning(mut self, reasoning: ReasoningBlock) -> Self {
        self.reasoning = Some(reasoning);
        self
    }

    /// Attaches the tool calls the assistant asked for.
    #[must_use]
    pub fn with_tool_calls(mut self, tool_calls: Vec<ToolCallRequest>) -> Self {
        self.tool_calls = tool_calls;
        self
    }

    /// True when this is a tool result.
    #[must_use]
    pub fn is_tool_result(&self) -> bool {
        self.role == Role::Tool
    }
}

/// A tool call the model asked for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCallRequest {
    /// The provider's correlation id, echoed back on the tool result. Distinct from the item id
    /// the kernel mints for the call.
    pub id: String,
    /// Tool name as advertised in [`ToolDef::name`].
    pub name: String,
    /// Raw arguments. Validated by the tool, not by the kernel.
    pub args: Value,
}

/// A tool as advertised to the model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    /// Name the model must use.
    pub name: String,
    /// What it does, in the model's terms.
    pub description: String,
    /// JSON Schema for `args`.
    pub parameters: Value,
}

/// One fragment of a streamed tool call.
///
/// Providers stream tool arguments piecewise, keyed by an index because several calls can
/// interleave; the kernel accumulates them into complete [`ToolCallRequest`]s.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCallDelta {
    /// Position of this call within the round's tool calls.
    pub index: u32,
    /// The provider's call id, sent with the first fragment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The tool name, sent with the first fragment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// A fragment of the JSON arguments to append.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub args_delta: String,
}

/// Why the provider stopped generating.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// The model finished its message.
    Stop,
    /// The output-token limit was reached.
    Length,
    /// The model wants tools to run.
    ToolCalls,
    /// The provider's content filter intervened.
    ContentFilter,
    /// A reason this build does not know; the string is preserved for the log.
    Other(String),
}

impl FinishReason {
    /// Parses the wire spelling providers use, mapping unknown values to [`FinishReason::Other`].
    #[must_use]
    pub fn parse(text: &str) -> Self {
        match text {
            "stop" => Self::Stop,
            "length" => Self::Length,
            "tool_calls" | "function_call" => Self::ToolCalls,
            "content_filter" => Self::ContentFilter,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// What arrives from a provider stream.
///
/// Struct variants, not newtypes: serde cannot serialize an internally tagged newtype variant
/// that holds a primitive, and the named fields read better in a log line anyway.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    /// More assistant text.
    TextDelta {
        /// The increment.
        text: String,
    },
    /// More reasoning text (ADR-0007: stored and replayed verbatim).
    ReasoningDelta {
        /// The increment, verbatim.
        text: String,
    },
    /// Reasoning finished, possibly with an opaque signature to store.
    ReasoningDone {
        /// Provider signature, when there is one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<SignatureBlock>,
    },
    /// A fragment of a tool call.
    ToolCall {
        /// The fragment.
        delta: ToolCallDelta,
    },
    /// Token accounting for this request.
    Usage {
        /// What the provider reported.
        usage: Usage,
    },
    /// The request finished normally.
    Done {
        /// Why it stopped.
        finish_reason: FinishReason,
    },
    /// The stream failed. Whether to retry is in `error.retryable`.
    Error {
        /// What went wrong.
        error: LlmError,
    },
}

/// The knobs a request carries.
///
/// The kernel forwards these and does not interpret them: which effort value turns into which
/// provider field is the llm layer's capability table, not the kernel's business (ADR-0007's
/// "slot, not policy").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatOptions {
    /// Model id as the provider names it.
    pub model: String,
    /// Requested reasoning effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Output-token ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Tools to advertise.
    ///
    /// The kernel overwrites this every turn with the frozen [`crate::ToolHost::snapshot`], so a
    /// caller cannot accidentally advertise a different table than the one calls are dispatched
    /// through.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_defs: Vec<ToolDef>,
    /// Provider-specific fields, passed through untouched.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub extra: Value,
}

impl ChatOptions {
    /// Options for one model, with every optional knob unset.
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            reasoning_effort: None,
            temperature: None,
            max_output_tokens: None,
            tool_defs: Vec::new(),
            extra: Value::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hatchery_protocol::SignatureBlock;

    #[test]
    fn messages_omit_what_they_do_not_have() {
        let json = serde_json::to_value(Message::user("hello")).expect("serialize");
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"]["text"], "hello");
        assert!(json.get("reasoning").is_none(), "{json}");
        assert!(json.get("tool_calls").is_none(), "{json}");
        assert!(json.get("tool_call_id").is_none(), "{json}");
        assert!(
            json.get("is_error").is_none(),
            "a false flag is noise, not information: {json}"
        );
    }

    #[test]
    fn a_tool_result_carries_its_correlation_id() {
        let message = Message::tool_result("call-7", "file not found", true);
        assert!(message.is_tool_result());
        assert_eq!(message.tool_call_id.as_deref(), Some("call-7"));
        assert_eq!(message.role, Role::Tool);
        let json = serde_json::to_value(&message).expect("serialize");
        assert_eq!(json["is_error"], true);
    }

    #[test]
    fn an_assistant_message_replays_its_reasoning() {
        let message = Message::assistant("done").with_reasoning(ReasoningBlock {
            text: " let me think \n".to_owned(),
            signature: Some(SignatureBlock::new(
                SignatureBlock::OPENAI_ENCRYPTED_CONTENT,
                "opaque",
            )),
        });
        let json = serde_json::to_string(&message).expect("serialize");
        assert!(json.contains("openai-encrypted-content"), "{json}");
        assert!(
            json.contains(" let me think \\n"),
            "the reasoning text must survive byte for byte: {json}"
        );
        assert_eq!(
            serde_json::from_str::<Message>(&json).expect("deserialize"),
            message
        );
    }

    #[test]
    fn tool_call_deltas_are_additive() {
        let first = ToolCallDelta {
            index: 0,
            id: Some("call-1".to_owned()),
            name: Some("write_file".to_owned()),
            args_delta: "{\"path\":".to_owned(),
        };
        let second = ToolCallDelta {
            index: 0,
            id: None,
            name: None,
            args_delta: "\"a.rs\"}".to_owned(),
        };
        let json = serde_json::to_value(&second).expect("serialize");
        assert!(json.get("id").is_none() && json.get("name").is_none());
        assert_eq!(json["args_delta"], "\"a.rs\"}");
        assert_ne!(first, second);
    }

    #[test]
    fn finish_reasons_keep_unknown_values() {
        assert_eq!(FinishReason::parse("tool_calls"), FinishReason::ToolCalls);
        assert_eq!(
            FinishReason::parse("function_call"),
            FinishReason::ToolCalls
        );
        assert_eq!(
            FinishReason::parse("something_new"),
            FinishReason::Other("something_new".to_owned())
        );
    }

    #[test]
    fn stream_events_name_their_kind() {
        let events = [
            StreamEvent::TextDelta {
                text: "a".to_owned(),
            },
            StreamEvent::ReasoningDelta {
                text: "b".to_owned(),
            },
            StreamEvent::ReasoningDone { signature: None },
            StreamEvent::ToolCall {
                delta: ToolCallDelta {
                    index: 0,
                    id: None,
                    name: None,
                    args_delta: String::new(),
                },
            },
            StreamEvent::Usage {
                usage: Usage::default(),
            },
            StreamEvent::Done {
                finish_reason: FinishReason::Stop,
            },
            StreamEvent::Error {
                error: LlmError::fatal("boom"),
            },
        ];
        let types: Vec<String> = events
            .iter()
            .map(|event| {
                serde_json::to_value(event).expect("serialize")["type"]
                    .as_str()
                    .expect("a type tag")
                    .to_owned()
            })
            .collect();
        assert_eq!(
            types,
            vec![
                "text_delta",
                "reasoning_delta",
                "reasoning_done",
                "tool_call",
                "usage",
                "done",
                "error"
            ]
        );
    }

    #[test]
    fn chat_options_do_not_advertise_tools_the_caller_made_up() {
        // The kernel replaces `tool_defs` with the frozen snapshot each turn; this test pins the
        // serialisation shape the llm layer sees when a turn has no tools at all.
        let json = serde_json::to_value(ChatOptions::new("deepseek-reasoner")).expect("serialize");
        assert_eq!(json["model"], "deepseek-reasoner");
        assert!(json.get("tool_defs").is_none(), "{json}");
        assert!(json.get("extra").is_none(), "{json}");
    }
}
