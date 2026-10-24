//! Command line: a hand-rolled parser into typed commands.
//!
//! The surface is small enough that a dependency for argument parsing would outweigh it, and
//! a parser returning a typed [`Command`] is the testable seam: every flag combination is a
//! unit test without spawning a process.

use std::path::PathBuf;

use hatchery_protocol::SessionId;

/// What the user asked the binary to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// The default (and `chat`): the interactive TUI.
    Chat(ChatArgs),
    /// Headless one-shot prompt.
    Exec(ExecArgs),
    /// Manage the daemon process.
    Daemon(DaemonAction),
    /// Environment and provider diagnostics.
    Doctor(DoctorArgs),
    /// `-h` / `--help` was asked for; the text is in [`usage`].
    Help,
    /// `-V` / `--version`.
    Version,
}

/// Flags of `chat` (and of the bare invocation).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChatArgs {
    /// Resume this session instead of creating one.
    pub session: Option<SessionId>,
    /// Workspace root to bind (project config layer and Code-mode default).
    pub workspace: Option<PathBuf>,
    /// Model as `provider/model`.
    pub model: Option<String>,
    /// State directory override (matches the daemon's).
    pub state_dir: Option<PathBuf>,
}

/// Flags of `exec`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExecArgs {
    /// The prompt text.
    pub prompt: String,
    /// Emit item-level JSONL events instead of plain text.
    pub json: bool,
    /// Resume this session instead of creating one.
    pub session: Option<SessionId>,
    /// Workspace root to bind.
    pub workspace: Option<PathBuf>,
    /// Model as `provider/model`.
    pub model: Option<String>,
    /// State directory override.
    pub state_dir: Option<PathBuf>,
}

/// Actions of `daemon`.
#[derive(Clone, Debug, PartialEq)]
pub enum DaemonAction {
    /// Spawn a detached daemon and wait until it serves.
    Start {
        /// State directory override, forwarded to the spawned process.
        state_dir: Option<PathBuf>,
    },
    /// Run the daemon in this terminal, until SIGINT/SIGTERM.
    Run {
        /// State directory override.
        state_dir: Option<PathBuf>,
    },
    /// Report whether a daemon is up, and what it says for itself.
    Status {
        /// State directory override.
        state_dir: Option<PathBuf>,
    },
    /// Ask a running daemon to terminate (SIGTERM).
    Stop {
        /// State directory override.
        state_dir: Option<PathBuf>,
    },
}

/// Flags of `doctor`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DoctorArgs {
    /// Also probe one provider with a real request.
    pub provider: Option<String>,
    /// Probe this model instead of the provider's first.
    pub model: Option<String>,
    /// State directory override.
    pub state_dir: Option<PathBuf>,
}

/// The help text, listing what exists in this milestone.
#[must_use]
pub fn usage() -> String {
    "\
hatchery — an open source AI agent harness

USAGE:
    hatchery                       Start the TUI chat (same as `chat`)
    hatchery chat [--session ID] [--workspace DIR] [--model P/M] [--state-dir DIR]
    hatchery exec [FLAGS] \"prompt\" [--json] [--session ID]
    hatchery daemon start [--state-dir DIR]     Spawn a detached daemon
    hatchery daemon run [--state-dir DIR]       Run the daemon in this terminal
    hatchery daemon status [--state-dir DIR]
    hatchery daemon stop [--state-dir DIR]
    hatchery doctor [--provider ID] [--model M] [--state-dir DIR]

OPTIONS:
    -h, --help       Print this help
    -V, --version    Print version information

`exec` exit codes: 0 completed, 1 failed, 2 cancelled.
"
    .to_owned()
}

/// Parses the argument list (without the binary name).
///
/// # Errors
///
/// A human-readable usage complaint for anything unrecognised or missing.
pub fn parse(args: &[String]) -> Result<Command, String> {
    let mut rest = args.iter();
    let Some(first) = rest.next() else {
        return Ok(Command::Chat(ChatArgs::default()));
    };
    match first.as_str() {
        "-h" | "--help" => Ok(Command::Help),
        "-V" | "--version" => Ok(Command::Version),
        "chat" => chat(&args[1..]).map(Command::Chat),
        "exec" => exec(&args[1..]).map(Command::Exec),
        "daemon" => daemon(&args[1..]).map(Command::Daemon),
        "doctor" => doctor(&args[1..]).map(Command::Doctor),
        other => Err(format!("unknown command {other:?}\n\n{}", usage())),
    }
}

fn chat(args: &[String]) -> Result<ChatArgs, String> {
    let mut parsed = ChatArgs::default();
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--session" => parsed.session = Some(session_id(rest.next(), "--session")?),
            "--workspace" => parsed.workspace = Some(path(rest.next(), "--workspace")?),
            "--model" => parsed.model = Some(text(rest.next(), "--model")?.to_owned()),
            "--state-dir" => parsed.state_dir = Some(path(rest.next(), "--state-dir")?),
            other => {
                return Err(flag_error(
                    other,
                    &["--session", "--workspace", "--model", "--state-dir"],
                ));
            }
        }
    }
    Ok(parsed)
}

fn exec(args: &[String]) -> Result<ExecArgs, String> {
    let mut parsed = ExecArgs::default();
    let mut prompt_parts: Vec<&str> = Vec::new();
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--json" => parsed.json = true,
            "--session" => parsed.session = Some(session_id(rest.next(), "--session")?),
            "--workspace" => parsed.workspace = Some(path(rest.next(), "--workspace")?),
            "--model" => parsed.model = Some(text(rest.next(), "--model")?.to_owned()),
            "--state-dir" => parsed.state_dir = Some(path(rest.next(), "--state-dir")?),
            _ if flag.starts_with('-') => {
                return Err(flag_error(
                    flag,
                    &[
                        "--json",
                        "--session",
                        "--workspace",
                        "--model",
                        "--state-dir",
                    ],
                ));
            }
            _ => prompt_parts.push(flag),
        }
    }
    if prompt_parts.is_empty() {
        return Err("exec needs a prompt\n\n".to_owned() + &usage());
    }
    parsed.prompt = prompt_parts.join(" ");
    Ok(parsed)
}

fn daemon(args: &[String]) -> Result<DaemonAction, String> {
    let action = args.first().map(String::as_str).ok_or_else(|| {
        format!(
            "daemon needs an action (start, run, status or stop)\n\n{}",
            usage()
        )
    })?;
    let mut state_dir = None;
    for pair in args[1..].chunks(2) {
        match pair.first().map(String::as_str) {
            Some("--state-dir") => {
                state_dir = Some(path(pair.get(1), "--state-dir")?);
            }
            Some(other) => return Err(flag_error(other, &["--state-dir"])),
            None => break,
        }
    }
    match action {
        "start" => Ok(DaemonAction::Start { state_dir }),
        "run" => Ok(DaemonAction::Run { state_dir }),
        "status" => Ok(DaemonAction::Status { state_dir }),
        "stop" => Ok(DaemonAction::Stop { state_dir }),
        other => Err(format!(
            "unknown daemon action {other:?} (start, run, status, stop)\n\n{}",
            usage()
        )),
    }
}

fn doctor(args: &[String]) -> Result<DoctorArgs, String> {
    let mut parsed = DoctorArgs::default();
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--provider" => parsed.provider = Some(text(rest.next(), "--provider")?.to_owned()),
            "--model" => parsed.model = Some(text(rest.next(), "--model")?.to_owned()),
            "--state-dir" => parsed.state_dir = Some(path(rest.next(), "--state-dir")?),
            other => return Err(flag_error(other, &["--provider", "--model", "--state-dir"])),
        }
    }
    Ok(parsed)
}

fn session_id(value: Option<&String>, flag: &str) -> Result<SessionId, String> {
    let raw = text(value, flag)?;
    raw.parse::<SessionId>()
        .map_err(|error| format!("{flag}: not a session id ({error})"))
}

fn path(value: Option<&String>, flag: &str) -> Result<PathBuf, String> {
    text(value, flag).map(PathBuf::from)
}

fn text<'a>(value: Option<&'a String>, flag: &str) -> Result<&'a str, String> {
    value
        .map(String::as_str)
        .ok_or_else(|| format!("{flag} needs a value"))
}

fn flag_error(got: &str, known: &[&str]) -> String {
    format!(
        "unknown flag {got:?}; expected one of {}\n\n{}",
        known.join(", "),
        usage()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn bare_invocation_is_chat() {
        assert_eq!(
            parse(&[]).expect("parse"),
            Command::Chat(ChatArgs::default())
        );
    }

    #[test]
    fn exec_joins_prompt_words_and_sets_flags() {
        let command = parse(&args(&["exec", "--json", "say", "hello"])).expect("parse");
        let Command::Exec(exec) = command else {
            panic!("{command:?}")
        };
        assert!(exec.json);
        assert_eq!(exec.prompt, "say hello");
    }

    #[test]
    fn exec_without_prompt_is_refused() {
        let error = parse(&args(&["exec", "--json"])).expect_err("refused");
        assert!(error.contains("exec needs a prompt"), "{error}");
    }

    #[test]
    fn daemon_flags_feed_the_named_action() {
        let command = parse(&args(&["daemon", "start", "--state-dir", "/tmp/x"])).expect("parse");
        let Command::Daemon(DaemonAction::Start { state_dir }) = command else {
            panic!("{command:?}")
        };
        assert_eq!(state_dir, Some(PathBuf::from("/tmp/x")));
    }

    #[test]
    fn unknown_flag_lists_the_known_ones() {
        let error = parse(&args(&["doctor", "--wat"])).expect_err("refused");
        assert!(error.contains("--provider"), "{error}");
    }

    #[test]
    fn help_and_version_are_their_own_commands() {
        assert_eq!(parse(&args(&["--help"])).expect("parse"), Command::Help);
        assert_eq!(parse(&args(&["-V"])).expect("parse"), Command::Version);
    }
}

#[cfg(test)]
mod parse_edges {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn chat_parses_every_flag_and_rejects_bogus_session_ids() {
        let session = SessionId::new();
        let command = parse(&args(&[
            "chat",
            "--session",
            &session.to_string(),
            "--workspace",
            "/ws",
            "--model",
            "p/m",
            "--state-dir",
            "/sd",
        ]))
        .expect("parse");
        let Command::Chat(chat) = command else {
            panic!("{command:?}")
        };
        assert_eq!(chat.session, Some(session));
        assert_eq!(chat.workspace, Some(PathBuf::from("/ws")));
        assert_eq!(chat.model, Some("p/m".to_owned()));
        assert_eq!(chat.state_dir, Some(PathBuf::from("/sd")));

        let error = parse(&args(&["chat", "--session", "not-a-uuid"])).expect_err("refused");
        assert!(error.contains("not a session id"), "{error}");
    }

    #[test]
    fn missing_flag_values_and_unknown_daemon_actions_name_their_fault() {
        assert!(
            parse(&args(&["chat", "--model"]))
                .expect_err("no value")
                .contains("--model needs a value"),
        );
        assert!(
            parse(&args(&["exec", "--wat", "hi"]))
                .expect_err("unknown exec flag")
                .contains("--json"),
        );
        assert!(
            parse(&args(&["daemon"]))
                .expect_err("no action")
                .contains("needs an action"),
        );
        assert!(
            parse(&args(&["daemon", "frobnicate"]))
                .expect_err("unknown action")
                .contains("unknown daemon action"),
        );
        assert!(
            parse(&args(&["daemon", "status", "--wat"]))
                .expect_err("unknown daemon flag")
                .contains("--state-dir"),
        );
        assert!(matches!(
            parse(&args(&["doctor", "--provider", "deepseek", "--model", "m", "--state-dir", "/s"])),
            Ok(Command::Doctor(doctor)) if doctor.provider.as_deref() == Some("deepseek")
                && doctor.model.as_deref() == Some("m")
                && doctor.state_dir == Some(PathBuf::from("/s")),
        ));
    }

    #[test]
    fn an_unknown_top_level_command_prints_the_usage() {
        let error = parse(&args(&["frobnicate"])).expect_err("refused");
        assert!(
            error.contains("unknown command") && error.contains("USAGE"),
            "{error}"
        );
        assert!(usage().contains("exit codes: 0 completed, 1 failed, 2 cancelled"));
    }
}
