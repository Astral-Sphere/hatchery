//! The chat loop: terminal setup, the select over input and events, clean restore.
//!
//! The loop is deliberately boring: draw, wait for a key or an event, mutate the [`Model`],
//! repeat. All policy (what a key means, what a command sends) lives in `commands` and the
//! model, so the loop has no tests of its own — it is the only untested code here, and it
//! reads like it.

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use hatchery_daemon::discover::StateDir;
use hatchery_protocol::method as m;
use hatchery_protocol::{ClientError, SessionId};

use crate::args::ChatArgs;
use crate::attach;
use crate::commands;
use crate::tui::{self, Model};

/// Runs the TUI until `/quit` or EOF on the event stream; the process exit code.
pub async fn run(state: StateDir, args: ChatArgs) -> i32 {
    let attached = match attach::attach_or_spawn(state, None, args.workspace.as_deref()).await {
        Ok(attached) => attached,
        Err(error) => {
            eprintln!("hatchery: {error}");
            return 1;
        }
    };

    let (events, session, seed) = match open_session(&attached, &args).await {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!("hatchery: {error}");
            return 1;
        }
    };

    let model = Model::new(seed.model_name, seed.effort, seed.show_reasoning);
    if let Err(error) = app_loop(attached, events, session, model).await {
        eprintln!("hatchery: {error}");
        return 1;
    }
    0
}

/// What the seed round learned, before the first draw.
struct Seed {
    model_name: String,
    effort: String,
    show_reasoning: bool,
}

async fn open_session(
    attached: &attach::Attached,
    args: &ChatArgs,
) -> Result<(hatchery_protocol::EventStream, SessionId, Seed), String> {
    let (events, session, reply) = crate::exec::open_subscribed(
        attached,
        args.session,
        args.model.as_deref(),
        args.workspace.as_deref(),
    )
    .await?;
    // Both subscribing replies carry a whole `session` object; read the status bar's seed out of
    // it so a resumed session shows its own model and effort, not defaults.
    let view = serde_json::from_value::<SessionView>(reply).ok();
    let model_name = view
        .as_ref()
        .map(|view| {
            format!(
                "{}/{}",
                view.session.model.provider, view.session.model.model
            )
        })
        .unwrap_or_default();
    let effort = view
        .as_ref()
        .and_then(|view| view.session.config_patch.as_ref())
        .and_then(|patch| patch.get("reasoning_effort"))
        .and_then(|value| value.as_str())
        .unwrap_or("medium")
        .to_owned();
    // `ui.show_reasoning` seeds the fold; a daemon without the key gets the default.
    let show_reasoning = match attached
        .client
        .call::<_, m::ConfigGetResult>(
            m::CONFIG_GET,
            &m::ConfigGetParams {
                key_path: Some("ui.show_reasoning".to_owned()),
            },
        )
        .await
    {
        Ok(result) => result
            .entries
            .first()
            .and_then(|entry| entry.value.as_bool())
            .unwrap_or(true),
        Err(_) => true,
    };
    Ok((
        events,
        session,
        Seed {
            model_name,
            effort,
            show_reasoning,
        },
    ))
}

/// The one field the seeders need; both `session/new` and `session/load` reply with it.
#[derive(serde::Deserialize)]
struct SessionView {
    session: hatchery_protocol::Session,
}

async fn app_loop(
    attached: attach::Attached,
    mut events: hatchery_protocol::EventStream,
    session: SessionId,
    mut model: Model,
) -> Result<(), ClientError> {
    let mut terminal = ratatui::try_init().map_err(|error| {
        ClientError::Connection(format!("the terminal could not be claimed: {error}"))
    })?;
    // Bracketed paste on: a pasted block arrives as one `Event::Paste` instead of the first line
    // being submitted and the rest fired off as stray prompts.
    let paste_on =
        crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste).is_ok();
    let mut reader = EventStream::new();
    let outcome = loop {
        if terminal.draw(|frame| tui::draw(&model, frame)).is_err() {
            break Ok(());
        }
        tokio::select! {
            maybe_event = reader.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        match key_input(key, &mut model) {
                            Input::Continue => {}
                            Input::Quit => break Ok(()),
                            Input::Submit => {
                                match submit(&attached, &session, &mut model).await {
                                    Ok(true) => {}
                                    // `/quit`
                                    Ok(false) => break Ok(()),
                                    // A refused call is a note, not a teardown: typing while a
                                    // turn runs must not close the session. The events stream
                                    // ending is what actually means the daemon is gone.
                                    Err(error) => model.notes.push(format!("! {error}")),
                                }
                            }
                        }
                    }
                    Some(Ok(Event::Paste(text))) if paste_on => model.input.push_str(&text),
                    Some(Ok(_)) => {}
                    Some(Err(error)) => break Err(ClientError::Connection(error.to_string())),
                    None => break Ok(()), // the terminal went away
                }
            }
            maybe_event = events.next() => {
                match maybe_event {
                    Some(event) => model.push_event(&event.event),
                    None => break Err(ClientError::Connection("the daemon disconnected".to_owned())),
                }
            }
        }
    };
    if paste_on {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
    }
    ratatui::restore();
    outcome
}

enum Input {
    Continue,
    Submit,
    Quit,
}

#[cfg(test)]
impl PartialEq for Input {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Self::Continue, Self::Continue)
                | (Self::Submit, Self::Submit)
                | (Self::Quit, Self::Quit)
        )
    }
}

#[cfg(test)]
impl std::fmt::Debug for Input {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Continue => write!(formatter, "Continue"),
            Self::Submit => write!(formatter, "Submit"),
            Self::Quit => write!(formatter, "Quit"),
        }
    }
}

fn key_input(key: KeyEvent, model: &mut Model) -> Input {
    match (key.modifiers, key.code) {
        (KeyModifiers::CONTROL, KeyCode::Char('r')) => {
            model.show_reasoning = !model.show_reasoning;
            Input::Continue
        }
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => Input::Quit,
        // Alt+Enter (Shift+Enter where the terminal reports it) starts a new line; Enter alone
        // submits. That is the whole multi-line story: the input renders wrapped below.
        (modifiers, KeyCode::Enter)
            if modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
        {
            model.input.push('\n');
            Input::Continue
        }
        (_, KeyCode::Enter) => Input::Submit,
        (_, KeyCode::Backspace) => {
            model.input.pop();
            Input::Continue
        }
        (_, KeyCode::Char(character)) => {
            model.input.push(character);
            Input::Continue
        }
        _ => Input::Continue,
    }
}

/// A submitted line: a slash command, or a prompt for the agent.
///
/// `Ok(false)` means quit. An error means the daemon refused the call — the caller turns it
/// into a note, so the refused prompt's text is handed back for editing rather than lost.
async fn submit(
    attached: &attach::Attached,
    session: &SessionId,
    model: &mut Model,
) -> Result<bool, ClientError> {
    let line = std::mem::take(&mut model.input);
    // An empty line is a keypress, not a message; sending it would start a turn about nothing.
    if !line.starts_with('/') && line.trim().is_empty() {
        return Ok(true);
    }
    let outcome = submit_line(attached, session, model, &line).await;
    if outcome.is_err() && !line.starts_with('/') {
        model.input = line;
    }
    outcome
}

async fn submit_line(
    attached: &attach::Attached,
    session: &SessionId,
    model: &mut Model,
    line: &str,
) -> Result<bool, ClientError> {
    if let Some(command) = commands::parse(line) {
        match command {
            commands::SlashCommand::Quit => {
                model.notes.push("/quit".to_owned());
                return Ok(false);
            }
            commands::SlashCommand::Mode | commands::SlashCommand::Unknown(_) => {
                if let Some(reply) = commands::local_reply(
                    &command,
                    &model.status.mode,
                    &model.status.model,
                    &model.status.effort,
                ) {
                    model.notes.push(reply);
                }
                return Ok(true);
            }
            commands::SlashCommand::Effort(_)
            | commands::SlashCommand::Model(_)
            | commands::SlashCommand::Prompt => {}
        }
        for (method, params) in commands::actions(&command, *session) {
            attached.client.call_raw(method, params).await?;
        }
        return Ok(true);
    }
    model.push_user(line);
    let params = m::SessionPromptParams {
        session_id: *session,
        content: hatchery_protocol::Content::text(line),
        generation: None,
    };
    attached
        .client
        .call::<_, m::SessionPromptResult>(m::SESSION_PROMPT, &params)
        .await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(modifiers: KeyModifiers, code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn alt_enter_starts_a_new_line_and_enter_submits() {
        let mut model = Model::new("p/m".to_owned(), "medium".to_owned(), true);
        model.input.push_str("first");
        assert_eq!(
            key_input(key(KeyModifiers::ALT, KeyCode::Enter), &mut model),
            Input::Continue,
            "Alt+Enter is a newline, not a submit"
        );
        assert_eq!(model.input, "first\n");
        assert_eq!(
            key_input(key(KeyModifiers::SHIFT, KeyCode::Enter), &mut model),
            Input::Continue,
            "Shift+Enter behaves the same where the terminal reports it"
        );
        assert_eq!(
            key_input(key(KeyModifiers::empty(), KeyCode::Enter), &mut model),
            Input::Submit,
            "plain Enter still submits"
        );
    }

    #[test]
    fn ctrl_r_toggles_reasoning_and_ctrl_c_quits() {
        let mut model = Model::new("p/m".to_owned(), "medium".to_owned(), false);
        assert_eq!(
            key_input(key(KeyModifiers::CONTROL, KeyCode::Char('r')), &mut model),
            Input::Continue
        );
        assert!(model.show_reasoning);
        assert_eq!(
            key_input(key(KeyModifiers::CONTROL, KeyCode::Char('c')), &mut model),
            Input::Quit
        );
    }
}
