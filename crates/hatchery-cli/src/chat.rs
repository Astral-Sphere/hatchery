//! The chat loop: terminal setup, the select over input, events and the animation tick, clean
//! restore.
//!
//! The loop is deliberately boring: relayout if stale, draw, wait for a key, an event or a tick,
//! mutate the [`Model`], repeat. All policy (what a key means, what a command sends) lives in
//! `commands` and the model, so the loop has no tests of its own — it is the only untested code
//! here, and it reads like it.

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use hatchery_daemon::discover::StateDir;
use hatchery_protocol::method as m;
use hatchery_protocol::{ClientError, SessionId};
use ratatui::layout::Rect;

use crate::args::ChatArgs;
use crate::attach;
use crate::commands;
use crate::tui::theme::{self, ThemeSetting};
use crate::tui::{self, Model, ToastKind};

/// The animation cadence: four frames a second, enough for a braille wheel and a seconds count.
const TICK: std::time::Duration = std::time::Duration::from_millis(250);

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

    if let Err(error) = app_loop(attached, events, session, seed).await {
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
    theme: ThemeSetting,
    workspace: String,
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
    // Both subscribing replies carry a whole `session` object; read the status line's seed out
    // of it so a resumed session shows its own model and effort, not defaults.
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
    let show_reasoning = config_bool(attached, "ui.show_reasoning")
        .await
        .unwrap_or(true);
    // `ui.theme` seeds the palette; `auto` is resolved against the terminal once raw mode is on.
    let theme = config_str(attached, "ui.theme")
        .await
        .as_deref()
        .and_then(ThemeSetting::parse)
        .unwrap_or(ThemeSetting::Auto);
    let workspace = args
        .workspace
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    Ok((
        events,
        session,
        Seed {
            model_name,
            effort,
            show_reasoning,
            theme,
            workspace,
        },
    ))
}

async fn config_bool(attached: &attach::Attached, key: &str) -> Option<bool> {
    let result = attached
        .client
        .call::<_, m::ConfigGetResult>(
            m::CONFIG_GET,
            &m::ConfigGetParams {
                key_path: Some(key.to_owned()),
            },
        )
        .await
        .ok()?;
    result.entries.first()?.value.as_bool()
}

async fn config_str(attached: &attach::Attached, key: &str) -> Option<String> {
    let result = attached
        .client
        .call::<_, m::ConfigGetResult>(
            m::CONFIG_GET,
            &m::ConfigGetParams {
                key_path: Some(key.to_owned()),
            },
        )
        .await
        .ok()?;
    result.entries.first()?.value.as_str().map(str::to_owned)
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
    seed: Seed,
) -> Result<(), ClientError> {
    let mut terminal = ratatui::try_init().map_err(|error| {
        ClientError::Connection(format!("the terminal could not be claimed: {error}"))
    })?;
    // Raw mode is on now, so the terminal answers OSC 11 immediately; the probe runs before the
    // event stream exists and is therefore the only reader of the tty for its 150 ms window.
    let probe = theme::detect_background();
    let colorfgbg = std::env::var("COLORFGBG").ok();
    let mut model = Model::new(
        seed.model_name,
        seed.effort,
        seed.show_reasoning,
        seed.theme,
        theme::resolve(seed.theme, probe, colorfgbg.as_deref()),
        probe,
        seed.workspace,
    );
    // Bracketed paste on: a pasted block arrives as one `Event::Paste` instead of the first line
    // being submitted and the rest fired off as stray prompts.
    let paste_on =
        crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste).is_ok();
    let mut reader = EventStream::new();
    let mut ticker = tokio::time::interval(TICK);
    ticker.tick().await; // the immediate first tick is not a frame
    let outcome = loop {
        let size = match terminal.size() {
            Ok(size) => size,
            Err(_) => break Ok(()),
        };
        if model.needs_relayout(size.width) {
            model.relayout(size.width);
        }
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
                                    // A refused call is a toast, not a teardown: typing while a
                                    // turn runs must not close the session. The events stream
                                    // ending is what actually means the daemon is gone.
                                    Err(error) => model.note(error.to_string(), ToastKind::Error),
                                }
                            }
                            input => apply(input, &mut model, &attached, &session).await,
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
            _ = ticker.tick() => model.tick(),
        }
    };
    if paste_on {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
    }
    ratatui::restore();
    outcome
}

/// Applies a viewport action; the loop knows the terminal size, the model knows the transcript.
async fn apply(input: Input, model: &mut Model, attached: &attach::Attached, session: &SessionId) {
    let size = terminal_size();
    let [messages, ..] = tui::layout(model, Rect::new(0, 0, size.0, size.1));
    let height = messages.height as usize;
    match input {
        Input::PageUp => model.scroll_by(-(height as isize), height),
        Input::PageDown => model.scroll_by(height as isize, height),
        Input::Top => model.scroll.top(),
        Input::Bottom => model.scroll.follow(),
        Input::JumpPrev => model.jump_prev(height),
        Input::JumpNext => model.jump_next(height),
        Input::Cancel => {
            // Only a turn in flight can be interrupted; while an approval waits, M2's dialog
            // owns Esc, and idle Esc is a mispress, not an error.
            if model.status.state == "thinking" || model.status.state.starts_with("rate limited") {
                let params = m::SessionCancelParams {
                    session_id: *session,
                };
                if let Err(error) = attached
                    .client
                    .call::<_, m::SessionCancelResult>(m::SESSION_CANCEL, &params)
                    .await
                {
                    model.note(error.to_string(), ToastKind::Error);
                }
            }
        }
        Input::Continue | Input::Submit | Input::Quit => {}
    }
}

/// The terminal size without a `Terminal` handle, for the scroll metrics.
fn terminal_size() -> (u16, u16) {
    crossterm::terminal::size().unwrap_or((80, 24))
}

#[derive(Debug, PartialEq)]
enum Input {
    Continue,
    Submit,
    Quit,
    Cancel,
    PageUp,
    PageDown,
    Top,
    Bottom,
    JumpPrev,
    JumpNext,
}

fn key_input(key: KeyEvent, model: &mut Model) -> Input {
    match (key.modifiers, key.code) {
        (KeyModifiers::CONTROL, KeyCode::Char('r')) => {
            model.toggle_reasoning();
            Input::Continue
        }
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => Input::Quit,
        (KeyModifiers::CONTROL, KeyCode::Up) => Input::JumpPrev,
        (KeyModifiers::CONTROL, KeyCode::Down) => Input::JumpNext,
        (_, KeyCode::PageUp) => Input::PageUp,
        (_, KeyCode::PageDown) => Input::PageDown,
        (_, KeyCode::Home) => Input::Top,
        (_, KeyCode::End) => Input::Bottom,
        (_, KeyCode::Esc) => Input::Cancel,
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
/// into a toast, so the refused prompt's text is handed back for editing rather than lost.
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
            commands::SlashCommand::Quit => return Ok(false),
            // The theme is a frontend concern: it applies locally, no protocol round trip.
            commands::SlashCommand::Theme(Some(setting)) => {
                model.set_theme(setting);
                model.note(format!("theme: {}", setting.as_str()), ToastKind::Info);
                return Ok(true);
            }
            commands::SlashCommand::Mode
            | commands::SlashCommand::Theme(None)
            | commands::SlashCommand::Usage { .. }
            | commands::SlashCommand::Unknown(_) => {
                if let Some(reply) = commands::local_reply(
                    &command,
                    &model.status.mode,
                    &model.status.model,
                    &model.status.effort,
                    model.theme_setting.as_str(),
                ) {
                    model.note(reply, ToastKind::Info);
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

    fn model() -> Model {
        Model::new(
            "p/m".to_owned(),
            "medium".to_owned(),
            false,
            ThemeSetting::Dark,
            theme::Theme::dark(),
            None,
            "/tmp".to_owned(),
        )
    }

    #[test]
    fn alt_enter_starts_a_new_line_and_enter_submits() {
        let mut model = model();
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
        let mut model = model();
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

    #[test]
    fn the_scroll_and_jump_keys_map_to_viewport_actions() {
        let mut model = model();
        for (key, expected) in [
            (key(KeyModifiers::NONE, KeyCode::PageUp), Input::PageUp),
            (key(KeyModifiers::NONE, KeyCode::PageDown), Input::PageDown),
            (key(KeyModifiers::NONE, KeyCode::Home), Input::Top),
            (key(KeyModifiers::NONE, KeyCode::End), Input::Bottom),
            (key(KeyModifiers::CONTROL, KeyCode::Up), Input::JumpPrev),
            (key(KeyModifiers::CONTROL, KeyCode::Down), Input::JumpNext),
            (key(KeyModifiers::NONE, KeyCode::Esc), Input::Cancel),
        ] {
            assert_eq!(key_input(key, &mut model), expected, "{key:?}");
        }
    }

    #[test]
    fn typing_still_wins_over_the_navigation_keys() {
        let mut model = model();
        assert_eq!(
            key_input(key(KeyModifiers::NONE, KeyCode::Char('x')), &mut model),
            Input::Continue
        );
        assert_eq!(model.input, "x");
    }
}
