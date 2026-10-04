//! Slash commands: typed parsing and the protocol actions they mean.
//!
//! Pure functions, no terminal and no client: `parse` turns a line into a command, `actions`
//! turns a command into the protocol calls to send (in order). The TUI's job is just to pipe
//! one into the other and await the replies — projection discipline, no local state machine.

use hatchery_protocol::method::{ConfigPatch, SetConfigParams};
use hatchery_protocol::{ModelRef, ReasoningEffort, SessionId, method as m};

use crate::tui::theme::ThemeSetting;

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
    /// Switch the palette (`/theme dark|light|auto`); frontend-local, no protocol round trip.
    Theme(Option<ThemeSetting>),
    /// Leave the TUI.
    Quit,
    /// A known command whose argument is missing or not understood. The reply is its usage —
    /// "unknown command /effort; try /effort" would be a reply recommending what was typed.
    Usage {
        /// Which command was recognised ("effort" or "model").
        command: &'static str,
        /// The argument as typed ("" when there was none).
        argument: String,
    },
    /// Something that looks like a command but is not one.
    Unknown(String),
}

/// Parses one input line. `None` when the line is not a command: no leading `/`, or a leading
/// `//` — the escape hatch, which leaves the whole line (slashes included) to go out as a
/// literal prompt instead of being read as a command.
#[must_use]
pub fn parse(line: &str) -> Option<SlashCommand> {
    let trimmed = line.trim();
    // Whitespace between the slash and the name is tolerated: `/ effort off` is still /effort.
    let rest = trimmed.strip_prefix('/')?.trim_start();
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
        "theme" => Some(theme(argument)),
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
        _ => return usage("effort", argument),
    };
    SlashCommand::Effort(effort)
}

fn model(argument: &str) -> SlashCommand {
    match argument.split_once('/') {
        Some((provider, model)) if !provider.is_empty() && !model.is_empty() => {
            SlashCommand::Model(ModelRef::new(provider, model))
        }
        _ => usage("model", argument),
    }
}

fn theme(argument: &str) -> SlashCommand {
    match argument {
        "" => SlashCommand::Theme(None),
        value => match ThemeSetting::parse(value) {
            Some(setting) => SlashCommand::Theme(Some(setting)),
            None => usage("theme", argument),
        },
    }
}

/// A recognised command with nothing (or nothing sensible) to act on.
fn usage(command: &'static str, argument: &str) -> SlashCommand {
    SlashCommand::Usage {
        command,
        argument: argument.to_owned(),
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
        // `/mode` reads the session the TUI already has; `/quit` and `/theme` are local. A usage
        // reply has nothing to send — the whole point is that the line did not parse into an
        // action.
        SlashCommand::Mode
        | SlashCommand::Theme(_)
        | SlashCommand::Usage { .. }
        | SlashCommand::Quit => Vec::new(),
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
    theme: &str,
) -> Option<String> {
    match command {
        SlashCommand::Mode => Some(format!("mode: {mode}")),
        SlashCommand::Theme(None) => Some(format!(
            "theme is {theme}; set it with /theme dark|light|auto"
        )),
        SlashCommand::Theme(Some(setting)) => Some(format!("theme: {}", setting.as_str())),
        SlashCommand::Usage { command, argument } => Some(match *command {
            // A bare `/effort` is a question, so the status bar's own value rides along.
            "effort" if argument.is_empty() => {
                format!("effort is {effort}; set it with /effort off|low|medium|high|max")
            }
            "effort" => {
                format!("not an effort: {argument}; usage: /effort off|low|medium|high|max")
            }
            "model" if argument.is_empty() => {
                format!("model is {model}; set it with /model provider/model")
            }
            "model" => format!("not a model: {argument}; usage: /model provider/model"),
            "theme" if argument.is_empty() => {
                format!("theme is {theme}; set it with /theme dark|light|auto")
            }
            "theme" => format!("not a theme: {argument}; usage: /theme dark|light|auto"),
            other => unreachable!("usage is only minted for effort, model and theme, not {other}"),
        }),
        SlashCommand::Effort(_) | SlashCommand::Model(_) | SlashCommand::Prompt => None,
        SlashCommand::Quit => Some(format!("bye ({mode}, {model}, {effort} remain set)")),
        SlashCommand::Unknown(line) => Some(format!(
            "unknown command {line}; try /effort, /model, /theme, /prompt, /mode, /quit"
        )),
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
        assert!(
            matches!(parse("/model deepseek"), Some(SlashCommand::Usage { .. })),
            "a half model reference is a usage reply, not an unknown command"
        );
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
            "auto",
        );
        assert_eq!(reply.as_deref(), Some("mode: chat"));
    }

    #[test]
    fn theme_is_local_and_answers_with_its_usage() {
        let session = SessionId::new();
        assert_eq!(
            parse("/theme dark"),
            Some(SlashCommand::Theme(Some(ThemeSetting::Dark)))
        );
        assert_eq!(parse("/theme"), Some(SlashCommand::Theme(None)));
        match parse("/theme banana") {
            Some(SlashCommand::Usage { command, argument }) => {
                assert_eq!(command, "theme");
                assert_eq!(argument, "banana");
            }
            other => panic!("a bad theme is a usage reply: {other:?}"),
        }
        for line in ["/theme", "/theme light"] {
            let command = parse(line).expect("command");
            assert!(actions(&command, session).is_empty(), "{line}");
        }
        let bare = local_reply(
            &parse("/theme").expect("command"),
            "chat",
            "m",
            "high",
            "auto",
        );
        assert_eq!(
            bare.as_deref(),
            Some("theme is auto; set it with /theme dark|light|auto")
        );
        let mistyped = local_reply(
            &parse("/theme banana").expect("command"),
            "chat",
            "m",
            "high",
            "dark",
        );
        assert_eq!(
            mistyped.as_deref(),
            Some("not a theme: banana; usage: /theme dark|light|auto")
        );
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
    fn command_names_are_case_insensitive_and_whitespace_never_names_the_command() {
        assert_eq!(
            parse("/EFFORT high"),
            parse("/effort high"),
            "the name's case does not matter"
        );
        match parse("/effort  ") {
            Some(SlashCommand::Usage { command, argument }) => {
                assert_eq!(command, "effort");
                assert_eq!(argument, "", "trailing whitespace is no argument");
            }
            other => panic!("an empty argument is a usage reply, not a crash: {other:?}"),
        }
        assert_eq!(
            parse("/ effort off"),
            Some(SlashCommand::Effort(ReasoningEffort::Off)),
            "whitespace after the slash still finds the command"
        );
    }

    #[test]
    fn a_known_command_with_a_bad_argument_gets_its_usage_not_unknown() {
        // `/effort` bare used to answer "unknown command /effort; try /effort" — a reply
        // recommending what was just typed.
        for (line, command) in [
            ("/effort", "effort"),
            ("/effort banana", "effort"),
            ("/model", "model"),
            ("/model deepseek", "model"),
        ] {
            match parse(line) {
                Some(SlashCommand::Usage { command: which, .. }) => {
                    assert_eq!(which, command, "{line}");
                }
                other => panic!("{line} should be a usage reply: {other:?}"),
            }
        }
        let session = SessionId::new();
        let bare = parse("/effort").expect("command");
        assert!(actions(&bare, session).is_empty(), "usage sends nothing");
        assert_eq!(
            local_reply(&bare, "chat", "deepseek/deepseek-flash", "high", "auto").expect("local"),
            "effort is high; set it with /effort off|low|medium|high|max"
        );
        let mistyped = parse("/effort banana").expect("command");
        assert_eq!(
            local_reply(&mistyped, "chat", "deepseek/deepseek-flash", "high", "auto")
                .expect("local"),
            "not an effort: banana; usage: /effort off|low|medium|high|max"
        );
        // The genuinely unknown keep their old reply.
        let stranger = parse("/frobnicate").expect("command");
        let reply = local_reply(&stranger, "chat", "m", "high", "auto").expect("local");
        assert!(reply.contains("unknown command"), "{reply}");
    }
}
