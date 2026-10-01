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
    let model_name = serde_json::from_value::<m::SessionNewResult>(reply)
        .ok()
        .map(|result| {
            format!(
                "{}/{}",
                result.session.model.provider, result.session.model.model
            )
        })
        .unwrap_or_default();
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
            effort: "medium".to_owned(),
            show_reasoning,
        },
    ))
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
                                    Err(error) => break Err(error),
                                }
                            }
                        }
                    }
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
    ratatui::restore();
    outcome
}

enum Input {
    Continue,
    Submit,
    Quit,
}

fn key_input(key: KeyEvent, model: &mut Model) -> Input {
    match (key.modifiers, key.code) {
        (KeyModifiers::CONTROL, KeyCode::Char('r')) => {
            model.show_reasoning = !model.show_reasoning;
            Input::Continue
        }
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => Input::Quit,
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
/// `Ok(false)` means quit.
async fn submit(
    attached: &attach::Attached,
    session: &SessionId,
    model: &mut Model,
) -> Result<bool, ClientError> {
    let line = std::mem::take(&mut model.input);
    if let Some(command) = commands::parse(&line) {
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
    model.push_user(&line);
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
