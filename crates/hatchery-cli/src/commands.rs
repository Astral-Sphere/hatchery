//! Slash commands: typed parsing and the protocol actions they mean.
//!
//! Pure functions, no terminal and no client: `parse` turns a line into a command, `actions`
//! turns a command into the protocol calls to send (in order). The TUI's job is just to pipe
//! one into the other and await the replies — projection discipline, no local state machine.

use hatchery_protocol::method::{ConfigPatch, SetConfigParams};
use hatchery_protocol::{ModelRef, ReasoningEffort, SessionId, method as m};

/// A line the user typed that started with `/`.
#[derive(Clone, Debug, PartialEq)]
pub enum SlashCommand {
    /// Set the reasoning effort.
    Effort(ReasoningEffort),
    /// Switch model (`/model provider/model`).
    Model(ModelRef),
    /// Show the prompt as it would be sent.
    Prompt,
    /// Show the current mode (read-only display; Chat is the only mode in M1).
    Mode,
    /// Leave the TUI.
    Quit,
    /// Something that looks like a command but is not one.
    Unknown(String),
}

/// Parses one input line. `None` when the line is not a command: no leading `/`, or a leading
/// `//` — the escape hatch, which leaves the whole line (slashes included) to go out as a
/// literal prompt instead of being read as a command.
#[must_use]
pub fn parse(line: &str) -> Option<SlashCommand> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix('/')?;
    if rest.starts_with('/') {
        // `//usr/local/bin` is a path someone wants to talk about, not a command.
        return None;
    }
    let (name, argument) = match rest.split_once(' ') {
        Some((name, argument)) => (name, argument.trim()),
        None => (rest, ""),
    };
    // Command names are case-insensitive (`/EFFORT high` works); the argument's own case stays.
    match name.to_ascii_lowercase().as_str() {
        "effort" => Some(effort(argument)),
        "model" => Some(model(argument)),
        "prompt" => Some(SlashCommand::Prompt),
        "mode" => Some(SlashCommand::Mode),
        "quit" | "q" => Some(SlashCommand::Quit),
        _ => Some(SlashCommand::Unknown(trimmed.to_owned())),
    }
}

fn effort(argument: &str) -> SlashCommand {
    // The protocol spells these snake_case; the same words here keep one vocabulary.
    let effort = match argument.to_ascii_lowercase().as_str() {
        "off" => ReasoningEffort::Off,
        "low" => ReasoningEffort::Low,
        "medium" => ReasoningEffort::Medium,
        "high" => ReasoningEffort::High,
        "max" => ReasoningEffort::Max,
        _ => {
            let echo = format!("/effort {argument}").trim_end().to_owned();
            return SlashCommand::Unknown(echo);
        }
    };
    SlashCommand::Effort(effort)
}

fn model(argument: &str) -> SlashCommand {
    match argument.split_once('/') {
        Some((provider, model)) if !provider.is_empty() && !model.is_empty() => {
            SlashCommand::Model(ModelRef::new(provider, model))
        }
        _ => SlashCommand::Unknown(format!("/model {argument}").trim_end().to_owned()),
    }
}

/// The protocol calls one command translates to, in the order to send them.
#[must_use]
pub fn actions(
    command: &SlashCommand,
    session: SessionId,
) -> Vec<(&'static str, serde_json::Value)> {
    match command {
        SlashCommand::Effort(effort) => {
            let params = SetConfigParams {
                session_id: session,
                patch: ConfigPatch {
                    model: None,
                    reasoning_effort: Some(*effort),
                    overrides: None,
                },
            };
            let value = serde_json::to_value(&params).expect("typed params always serialise");
            vec![(m::SESSION_SET_CONFIG, value)]
        }
        SlashCommand::Model(model) => {
            let params = SetConfigParams {
                session_id: session,
                patch: ConfigPatch {
                    model: Some(model.clone()),
                    reasoning_effort: None,
                    overrides: None,
                },
            };
            let value = serde_json::to_value(&params).expect("typed params always serialise");
            vec![(m::SESSION_SET_CONFIG, value)]
        }
        SlashCommand::Prompt => {
            let params = m::PromptRenderParams {
                session_id: Some(session),
                mode: None,
            };
            let value = serde_json::to_value(&params).expect("typed params always serialise");
            vec![(m::PROMPT_RENDER, value)]
        }
        // `/mode` reads the session the TUI already has; `/quit` is local.
        SlashCommand::Mode | SlashCommand::Quit => Vec::new(),
        SlashCommand::Unknown(_) => Vec::new(),
    }
}

/// The reply text a purely local command produces (`None` waits for the protocol).
#[must_use]
pub fn local_reply(
    command: &SlashCommand,
    mode: &str,
    model: &str,
    effort: &str,
) -> Option<String> {
    match command {
        SlashCommand::Mode => Some(format!("mode: {mode}")),
        SlashCommand::Unknown(line) => Some(format!(
            "unknown command {line}; try /effort, /model, /prompt, /mode, /quit"
        )),
        SlashCommand::Effort(_) | SlashCommand::Model(_) | SlashCommand::Prompt => None,
        SlashCommand::Quit => Some(format!("bye ({mode}, {model}, {effort} remain set)")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(command: &SlashCommand, session: SessionId) -> (String, serde_json::Value) {
        let actions = actions(command, session);
        assert_eq!(actions.len(), 1, "{command:?}");
        let (method, value) = actions.into_iter().next().expect("one");
        (method.to_owned(), value)
    }

    #[test]
    fn effort_maps_to_set_config() {
        let session = SessionId::new();
        let command = parse("/effort high").expect("command");
        assert_eq!(command, SlashCommand::Effort(ReasoningEffort::High));
        let (method, value) = one(&command, session);
        assert_eq!(method, m::SESSION_SET_CONFIG);
        assert_eq!(value["patch"]["reasoning_effort"], "high");
        assert_eq!(value["session_id"], serde_json::to_value(session).unwrap());
    }

    #[test]
    fn model_requires_the_provider_half() {
        assert!(matches!(
            parse("/model deepseek/deepseek-chat"),
            Some(SlashCommand::Model(_))
        ));
        assert!(matches!(
            parse("/model deepseek"),
            Some(SlashCommand::Unknown(_))
        ));
        let session = SessionId::new();
        let command = parse("/model deepseek/deepseek-chat").expect("command");
        let (method, value) = one(&command, session);
        assert_eq!(method, m::SESSION_SET_CONFIG);
        assert_eq!(value["patch"]["model"]["provider"], "deepseek");
        assert_eq!(value["patch"]["model"]["model"], "deepseek-chat");
    }

    #[test]
    fn prompt_maps_to_prompt_render() {
        let session = SessionId::new();
        let command = parse("/prompt").expect("command");
        let (method, value) = one(&command, session);
        assert_eq!(method, m::PROMPT_RENDER);
        assert_eq!(value["session_id"], serde_json::to_value(session).unwrap());
    }

    #[test]
    fn mode_and_quit_send_nothing() {
        let session = SessionId::new();
        for line in ["/mode", "/quit"] {
            let command = parse(line).expect("command");
            assert!(actions(&command, session).is_empty(), "{line}");
        }
        let reply = local_reply(
            &parse("/mode").expect("command"),
            "chat",
            "deepseek/deepseek-chat",
            "high",
        );
        assert_eq!(reply.as_deref(), Some("mode: chat"));
    }

    #[test]
    fn non_commands_do_not_parse() {
        assert!(parse("hello there").is_none());
        assert!(
            parse("/frobnicate").is_some(),
            "unknown commands parse, to be rejected kindly"
        );
    }

    #[test]
    fn double_slash_escapes_command_land() {
        // An absolute path is something to talk about, not to run.
        assert!(parse("//usr/local/bin/tool").is_none());
        assert!(parse("// and a note that starts with slashes").is_none());
        assert!(
            parse("/usr/local/bin/tool").is_some(),
            "single slash still parses"
        );
    }

    #[test]
    fn command_names_are_case_insensitive_and_the_echo_is_trimmed() {
        assert_eq!(
            parse("/EFFORT high"),
            parse("/effort high"),
            "the name's case does not matter"
        );
        match parse("/effort  ") {
            Some(SlashCommand::Unknown(echo)) => assert_eq!(echo, "/effort", "no trailing space"),
            other => panic!("an empty argument is an unknown command, not a crash: {other:?}"),
        }
    }
}
