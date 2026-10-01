//! Between the kernel's vocabulary and the wire's (ADR-0007: this is the only file where
//! `openai_interface` request types are built, and no wire type ever leaves the crate).

use hatchery_kernel::{ChatOptions, FinishReason, Message, Role, StreamEvent, ToolCallDelta};
use hatchery_protocol::{ReasoningEffort, Usage};
use openai_interface::chat::create::request::ReasoningEffort as ReasoningEffortWire;
use openai_interface::chat::create::request::{
    AssistantMessage, AssistantToolCall, Message as WireMessage, MessageContent, RequestBody,
    RequestTool, StreamOptions, Thinking, ThinkingType, ToolCallFunction, ToolFunction,
};
use openai_interface::chat::create::response::streaming::ChatCompletionChunk;

use crate::capability::{CapabilityTable, ModelCapabilities, ReasoningWire, qwen_thinking_budget};
use crate::config::ProviderConfig;

/// Assembles the request body for one round.
///
/// Runs entirely inside the borrowed window of `chat_stream`: everything it reads (options,
/// messages, capabilities) is read here, and everything it returns is owned.
#[must_use]
pub fn request_body(
    config: &ProviderConfig,
    table: &CapabilityTable,
    options: &ChatOptions,
    messages: &[Message],
) -> RequestBody {
    let caps = table.capabilities(&options.model);
    let model = caps.model_for_effort(&options.model, options.reasoning_effort);

    let mut body = RequestBody {
        model,
        messages: messages
            .iter()
            .map(|message| wire_message(message, &caps))
            .collect(),
        stream: Some(true),
        // The usage-only chunk that precedes `data: [DONE]` is the only usage report a stream
        // gives; without this flag rounds are free of charge, which nobody believes.
        stream_options: Some(StreamOptions {
            include_usage: true,
        }),
        ..RequestBody::default()
    };

    apply_effort(&mut body, &caps, options.reasoning_effort);
    body.temperature = options.temperature;
    body.max_tokens = options.max_output_tokens;
    if !options.tool_defs.is_empty() {
        body.tools = Some(
            options
                .tool_defs
                .iter()
                .map(|def| RequestTool::Function {
                    function: ToolFunction {
                        name: def.name.clone(),
                        description: Some(def.description.clone()),
                        parameters: def.parameters.as_object().cloned(),
                        strict: None,
                    },
                })
                .collect(),
        );
    }
    if let Some(extra) = options.extra.as_object() {
        let map = body.extra_body_map.get_or_insert_with(Default::default);
        for (key, value) in extra {
            map.insert(key.clone(), value.clone());
        }
    }
    let _ = config; // headers and the URL travel with the request options, not the body
    body
}

/// Writes the requested effort into the body the way this family expects.
fn apply_effort(body: &mut RequestBody, caps: &ModelCapabilities, effort: Option<ReasoningEffort>) {
    let Some(effort) = effort else { return };
    match &caps.reasoning {
        ReasoningWire::Effort => {
            body.reasoning_effort = Some(wire_effort(effort));
        }
        ReasoningWire::QwenThinking => match effort {
            ReasoningEffort::Off => body.enable_thinking = Some(false),
            other => {
                body.enable_thinking = Some(true);
                body.thinking_budget = qwen_thinking_budget(other);
            }
        },
        ReasoningWire::ThinkingSwitch => {
            // Explicit beats default: the current DeepSeek models think by default, so the
            // off switch is the only way to make a turn's mode deterministic.
            body.thinking = Some(match effort {
                ReasoningEffort::Off => Thinking {
                    type_: ThinkingType::Disabled,
                },
                _ => Thinking {
                    type_: ThinkingType::Enabled,
                },
            });
        }
        ReasoningWire::ModelSwitch { .. } => {
            // Already applied while picking the model; there is no field to set.
        }
        ReasoningWire::None => {
            // One warning per call rather than per provider: tracing dedupes nothing, but the
            // log is the only place this mismatch can be seen from.
            tracing::warn!(
                model = %body.model,
                "reasoning effort requested on a model with no reasoning knob; ignoring"
            );
        }
    }
}

/// The canonical ladder onto the wire's spelling: `off` is `none`, `max` survives as `max`.
fn wire_effort(effort: ReasoningEffort) -> ReasoningEffortWire {
    match effort {
        ReasoningEffort::Off => ReasoningEffortWire::None,
        ReasoningEffort::Low => ReasoningEffortWire::Low,
        ReasoningEffort::Medium => ReasoningEffortWire::Medium,
        ReasoningEffort::High => ReasoningEffortWire::High,
        ReasoningEffort::Max => ReasoningEffortWire::Max,
    }
}

/// Translates one kernel message into one wire message.
///
/// Reasoning passback is the capability table's call (ADR-0007): the text goes back byte for
/// byte or not at all — never trimmed, never rewritten. Signatures have no Chat Completions
/// field to ride in, so a stored signature is simply not sent on this wire.
#[must_use]
pub fn wire_message(message: &Message, caps: &ModelCapabilities) -> WireMessage {
    let text = message.content.as_text().unwrap_or_default();
    match message.role {
        Role::System => WireMessage::system(text),
        Role::User => {
            if !message.content.parts.is_empty() {
                tracing::warn!(
                    "attachments have no wire form on this transport yet; sending text only"
                );
            }
            WireMessage::user(text)
        }
        Role::Assistant => {
            let mut assistant = AssistantMessage {
                content: Some(text.to_owned()),
                reasoning_content: None,
                tool_calls: None,
                ..AssistantMessage::default()
            };
            if caps.echo_reasoning
                && let Some(block) = &message.reasoning
            {
                assistant.reasoning_content = Some(block.text.clone());
            }
            if !message.tool_calls.is_empty() {
                assistant.tool_calls = Some(
                    message
                        .tool_calls
                        .iter()
                        .map(|call| AssistantToolCall::Function {
                            id: call.id.clone(),
                            function: ToolCallFunction {
                                name: call.name.clone(),
                                arguments: call.args.to_string(),
                            },
                        })
                        .collect(),
                );
            }
            WireMessage::Assistant(assistant)
        }
        Role::Tool => WireMessage::Tool(openai_interface::chat::create::request::ToolMessage {
            content: MessageContent::Text(text.to_owned()),
            tool_call_id: message.tool_call_id.clone().unwrap_or_default(),
        }),
    }
}

/// One chunk's worth of events, in emission order.
///
/// `ReasoningDone` is emitted at the reasoning→text boundary so the reasoning block is closed
/// before anything can commit it unsigned (docs/design/kernel.md §4); Chat Completions carries
/// no signature, so the marker is always empty on this wire.
#[must_use]
pub fn events_from_chunk(chunk: &ChatCompletionChunk, in_reasoning: &mut bool) -> Vec<StreamEvent> {
    let mut events = Vec::new();
    let Some(choice) = chunk.choices.first() else {
        if let Some(usage) = &chunk.usage {
            events.push(StreamEvent::Usage {
                usage: wire_usage(usage),
            });
        }
        return events;
    };

    let delta = &choice.delta;
    if let Some(reasoning) = delta.reasoning_content.as_deref().filter(|r| !r.is_empty()) {
        *in_reasoning = true;
        events.push(StreamEvent::ReasoningDelta {
            text: reasoning.to_owned(),
        });
    }
    if let Some(text) = delta.content.as_deref().filter(|t| !t.is_empty()) {
        if *in_reasoning {
            *in_reasoning = false;
            events.push(StreamEvent::ReasoningDone { signature: None });
        }
        events.push(StreamEvent::TextDelta {
            text: text.to_owned(),
        });
    }
    if let Some(calls) = delta.tool_calls.as_deref() {
        for call in calls {
            // An empty string is this wire's "absent": qwen echoes `"id": ""` on every
            // continuation fragment (recorded 2026-10-01), and handing it through would let the
            // kernel overwrite the real call id, breaking result pairing. Same rule as the
            // dual-key reasoning chunks: values, not key presence.
            let fragment = ToolCallDelta {
                index: call.index,
                id: call.id.clone().filter(|id| !id.is_empty()),
                name: call
                    .function
                    .as_ref()
                    .and_then(|f| f.name.clone())
                    .filter(|name| !name.is_empty()),
                args_delta: call
                    .function
                    .as_ref()
                    .and_then(|f| f.arguments.clone())
                    .unwrap_or_default(),
            };
            if fragment.id.is_some() || fragment.name.is_some() || !fragment.args_delta.is_empty() {
                events.push(StreamEvent::ToolCall { delta: fragment });
            }
        }
    }
    // Usage precedes Done within a chunk, in case a provider packs them together — the kernel
    // stops at Done, so anything after it in the same batch would be lost.
    if let Some(usage) = &chunk.usage {
        events.push(StreamEvent::Usage {
            usage: wire_usage(usage),
        });
    }
    if let Some(reason) = &choice.finish_reason {
        events.push(StreamEvent::Done {
            finish_reason: FinishReason::parse(reason.as_str()),
        });
    }
    events
}

/// CompletionUsage → Usage: the fields the protocol asks about, dropped where the wire stayed
/// silent, with one request accounted.
#[must_use]
pub fn wire_usage(usage: &openai_interface::chat::CompletionUsage) -> Usage {
    Usage {
        prompt_tokens: Some(usage.prompt_tokens),
        completion_tokens: Some(usage.completion_tokens),
        reasoning_tokens: usage
            .completion_tokens_details
            .as_ref()
            .and_then(|details| details.reasoning_tokens),
        requests: 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hatchery_kernel::{ToolCallRequest, ToolDef};
    use hatchery_protocol::{ReasoningBlock, SignatureBlock};
    use serde_json::json;

    fn deepseek_table() -> CapabilityTable {
        CapabilityTable::builtin()
    }

    #[test]
    fn a_minimal_request_is_three_keys_and_a_flag() {
        let config = ProviderConfig::new("https://x", "K");
        let options = ChatOptions::new("deepseek-flash");
        let body = request_body(&config, &deepseek_table(), &options, &[Message::user("hi")]);
        let value = serde_json::to_value(&body).expect("serialize");
        assert_eq!(value["model"], "deepseek-flash");
        assert_eq!(value["stream"], true);
        assert_eq!(value["stream_options"]["include_usage"], true);
        assert_eq!(value["messages"][0]["role"], "user");
        assert_eq!(value["messages"][0]["content"], "hi");
        assert!(value.get("tools").is_none(), "no tools advertised, no key");
    }

    #[test]
    fn deepseek_effort_flips_the_thinking_switch_and_keeps_the_model() {
        let config = ProviderConfig::new("https://x", "K");
        let mut options = ChatOptions::new("deepseek-flash");
        options.reasoning_effort = Some(ReasoningEffort::Off);
        let body = request_body(&config, &deepseek_table(), &options, &[]);
        assert_eq!(body.model, "deepseek-flash");
        let value = serde_json::to_value(&body).expect("serialize");
        assert_eq!(value["thinking"]["type"], "disabled");
        assert!(value.get("reasoning_effort").is_none());

        options.reasoning_effort = Some(ReasoningEffort::High);
        let body = request_body(&config, &deepseek_table(), &options, &[]);
        let value = serde_json::to_value(&body).expect("serialize");
        assert_eq!(value["thinking"]["type"], "enabled");

        // No effort requested: no key at all, and the server default (thinking on) rules.
        let options = ChatOptions::new("deepseek-flash");
        let body = request_body(&config, &deepseek_table(), &options, &[]);
        let value = serde_json::to_value(&body).expect("serialize");
        assert!(value.get("thinking").is_none());
    }

    #[test]
    fn qwen_effort_becomes_a_flag_and_a_budget() {
        let config = ProviderConfig::new("https://x", "K");
        let mut options = ChatOptions::new("qwen3.8-flash");
        options.reasoning_effort = Some(ReasoningEffort::High);
        let body = request_body(&config, &deepseek_table(), &options, &[]);
        assert_eq!(body.enable_thinking, Some(true));
        assert_eq!(
            body.thinking_budget,
            qwen_thinking_budget(ReasoningEffort::High)
        );

        options.reasoning_effort = Some(ReasoningEffort::Off);
        let body = request_body(&config, &deepseek_table(), &options, &[]);
        assert_eq!(body.enable_thinking, Some(false));
        assert_eq!(body.thinking_budget, None);
    }

    #[test]
    fn generic_effort_passes_through() {
        let config = ProviderConfig::new("https://x", "K");
        let mut options = ChatOptions::new("some-model");
        options.reasoning_effort = Some(ReasoningEffort::Max);
        let body = request_body(&config, &deepseek_table(), &options, &[]);
        assert_eq!(body.reasoning_effort, Some(ReasoningEffortWire::Max));
    }

    #[test]
    fn reasoning_is_echoed_byte_for_byte_only_when_the_table_says_so() {
        let text = " let me think \n\t";
        let assistant = Message::assistant("done").with_reasoning(ReasoningBlock {
            text: text.to_owned(),
            signature: Some(SignatureBlock::new(
                SignatureBlock::OPENAI_ENCRYPTED_CONTENT,
                "opaque",
            )),
        });

        let dropping = wire_message(
            &assistant,
            &CapabilityTable::builtin().capabilities("qwen3.8-flash"),
        );
        let WireMessage::Assistant(dropped) = &dropping else {
            panic!("assistant must stay an assistant");
        };
        assert_eq!(dropped.reasoning_content, None);

        let echoing_caps = ModelCapabilities {
            echo_reasoning: true,
            ..ModelCapabilities::default()
        };
        let echoed = wire_message(&assistant, &echoing_caps);
        let WireMessage::Assistant(sent) = &echoed else {
            panic!("assistant must stay an assistant");
        };
        assert_eq!(
            sent.reasoning_content.as_deref(),
            Some(text),
            "the reasoning text must ride back byte for byte, whitespace and all"
        );
    }

    #[test]
    fn tool_calls_and_results_round_the_pairing() {
        let call = Message::assistant("").with_tool_calls(vec![ToolCallRequest {
            id: "call-7".to_owned(),
            name: "read_file".to_owned(),
            args: json!({"path": "a.rs"}),
        }]);
        let WireMessage::Assistant(sent) = wire_message(&call, &ModelCapabilities::default())
        else {
            panic!();
        };
        let wire_call = sent
            .tool_calls
            .as_ref()
            .expect("tool calls")
            .first()
            .unwrap();
        let AssistantToolCall::Function { id, function } = wire_call else {
            panic!("function tool call");
        };
        assert_eq!(id, "call-7");
        assert_eq!(function.name, "read_file");
        assert_eq!(function.arguments, r#"{"path":"a.rs"}"#);

        let result = Message::tool_result("call-7", "42 lines", false);
        let WireMessage::Tool(tool) = wire_message(&result, &ModelCapabilities::default()) else {
            panic!();
        };
        assert_eq!(tool.tool_call_id, "call-7");
    }

    #[test]
    fn tool_definitions_advertise_the_kernel_catalogue() {
        let config = ProviderConfig::new("https://x", "K");
        let mut options = ChatOptions::new("m");
        options.tool_defs = vec![ToolDef {
            name: "read_file".to_owned(),
            description: "reads a file".to_owned(),
            parameters: json!({"type":"object","properties":{"path":{"type":"string"}}}),
        }];
        let body = request_body(&config, &deepseek_table(), &options, &[]);
        let value = serde_json::to_value(&body).expect("serialize");
        assert_eq!(value["tools"][0]["type"], "function");
        assert_eq!(value["tools"][0]["function"]["name"], "read_file");
        assert_eq!(
            value["tools"][0]["function"]["parameters"]["type"],
            "object"
        );
    }

    #[test]
    fn extra_fields_arrive_through_the_escape_hatch() {
        let config = ProviderConfig::new("https://x", "K");
        let mut options = ChatOptions::new("m");
        options.extra = json!({"vendor_beta": {"mode": 2}});
        let body = request_body(&config, &deepseek_table(), &options, &[]);
        let value = serde_json::to_value(&body).expect("serialize");
        assert_eq!(value["vendor_beta"]["mode"], 2);
    }

    fn chunk(json: serde_json::Value) -> ChatCompletionChunk {
        serde_json::from_value(json).expect("chunk")
    }

    #[test]
    fn a_chunk_stream_becomes_the_documented_event_sequence() {
        let mut in_reasoning = false;
        let reasoning = chunk(json!({
            "id": "1", "created": 1, "model": "m", "object": "chat.completion.chunk",
            "choices": [{"index": 0, "delta": {"reasoning_content": "thinking"}, "finish_reason": null}]
        }));
        assert_eq!(
            events_from_chunk(&reasoning, &mut in_reasoning),
            vec![StreamEvent::ReasoningDelta {
                text: "thinking".to_owned()
            }]
        );

        let boundary = chunk(json!({
            "id": "1", "created": 1, "model": "m", "object": "chat.completion.chunk",
            "choices": [{"index": 0, "delta": {"content": "answer"}, "finish_reason": null}]
        }));
        assert_eq!(
            events_from_chunk(&boundary, &mut in_reasoning),
            vec![
                StreamEvent::ReasoningDone { signature: None },
                StreamEvent::TextDelta {
                    text: "answer".to_owned()
                },
            ],
            "the reasoning block closes before its answer opens"
        );

        let tool = chunk(json!({
            "id": "1", "created": 1, "model": "m", "object": "chat.completion.chunk",
            "choices": [{"index": 0, "delta": {"tool_calls": [
                {"index": 0, "id": "call-1", "function": {"name": "grep", "arguments": "{\"q\":"}}
            ]}, "finish_reason": null}]
        }));
        assert_eq!(
            events_from_chunk(&tool, &mut in_reasoning),
            vec![StreamEvent::ToolCall {
                delta: ToolCallDelta {
                    index: 0,
                    id: Some("call-1".to_owned()),
                    name: Some("grep".to_owned()),
                    args_delta: "{\"q\":".to_owned(),
                }
            }]
        );

        let finish = chunk(json!({
            "id": "1", "created": 1, "model": "m", "object": "chat.completion.chunk",
            "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
        }));
        assert_eq!(
            events_from_chunk(&finish, &mut in_reasoning),
            vec![StreamEvent::Done {
                finish_reason: FinishReason::ToolCalls
            }]
        );

        let usage = chunk(json!({
            "id": "1", "created": 1, "model": "m", "object": "chat.completion.chunk",
            "choices": [],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5,
                      "total_tokens": 15,
                      "completion_tokens_details": {"reasoning_tokens": 3}}
        }));
        assert_eq!(
            events_from_chunk(&usage, &mut in_reasoning),
            vec![StreamEvent::Usage {
                usage: Usage {
                    prompt_tokens: Some(10),
                    completion_tokens: Some(5),
                    reasoning_tokens: Some(3),
                    requests: 1,
                }
            }]
        );
    }

    #[test]
    fn usage_precedes_done_inside_one_chunk() {
        let mut in_reasoning = false;
        let packed = chunk(json!({
            "id": "1", "created": 1, "model": "m", "object": "chat.completion.chunk",
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        }));
        let events = events_from_chunk(&packed, &mut in_reasoning);
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], StreamEvent::Usage { .. }));
        assert!(matches!(events[1], StreamEvent::Done { .. }));
    }

    #[test]
    fn an_unknown_finish_reason_is_kept_verbatim() {
        let mut in_reasoning = false;
        let odd = chunk(json!({
            "id": "1", "created": 1, "model": "m", "object": "chat.completion.chunk",
            "choices": [{"index": 0, "delta": {}, "finish_reason": "eos"}]
        }));
        assert_eq!(
            events_from_chunk(&odd, &mut in_reasoning),
            vec![StreamEvent::Done {
                finish_reason: FinishReason::Other("eos".to_owned())
            }]
        );
    }
}
