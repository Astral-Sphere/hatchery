//! The chat loop: terminal setup, the select over input, events and the animation tick, clean
//! restore.
//!
//! The loop is deliberately boring: relayout if stale, draw, wait for a key, an event or a tick,
//! mutate the [`Model`], repeat. All policy (what a key means, what a command sends) lives in
//! `commands` and the model, so the loop has no tests of its own — it is the only untested code
//! here, and it reads like it.

use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use futures::StreamExt;
use hatchery_daemon::discover::StateDir;
use hatchery_protocol::method as m;
use hatchery_protocol::{ClientError, SessionId};
use ratatui::layout::Rect;

use crate::args::ChatArgs;
use crate::attach;
use crate::commands;
use crate::tui::theme::{self, ThemeSetting};
use crate::tui::widgets::scrollbar;
use crate::tui::{self, Model, ToastKind};

/// The animation cadence: four frames a second, enough for a braille wheel and a seconds count.
const TICK: std::time::Duration = std::time::Duration::from_millis(250);

/// Lines one wheel notch scrolls: qwen-code's `WHEEL_LINES_PER_TICK`.
const WHEEL_LINES: isize = 3;

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
    // Mouse capture on: the wheel scrolls the transcript and the scrollbar drags. While it is
    // on, the terminal's own text selection needs Shift — the trade every mouse-aware TUI makes.
    let mouse_on =
        crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture).is_ok();
    let mut reader = EventStream::new();
    let mut ticker = tokio::time::interval(TICK);
    ticker.tick().await; // the immediate first tick is not a frame
    let mut dragging = false;
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
                    Some(Ok(Event::Mouse(mouse))) => {
                        let size = terminal_size();
                        let [messages, ..] = tui::layout(&model, Rect::new(0, 0, size.0, size.1));
                        let [_, bar] = tui::transcript_areas(messages);
                        let (input, next) = mouse_input(mouse, bar, dragging);
                        dragging = next;
                        if let Some(input) = input {
                            apply(input, &mut model, &attached, &session).await;
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
            _ = ticker.tick() => model.tick(),
        }
    };
    if paste_on {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
    }
    if mouse_on {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    }
    ratatui::restore();
    outcome
}

/// Applies a viewport action; the loop knows the terminal size, the model knows the transcript.
async fn apply(input: Input, model: &mut Model, attached: &attach::Attached, session: &SessionId) {
    let size = terminal_size();
    let [messages, ..] = tui::layout(model, Rect::new(0, 0, size.0, size.1));
    let [_, bar] = tui::transcript_areas(messages);
    let height = messages.height as usize;
    if let Input::Cancel = input {
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
        return;
    }
    viewport(input, model, height, bar);
}

/// The viewport actions: pure model mutations, keyed by the window height and the scrollbar's
/// column so tests need neither a terminal nor a daemon.
fn viewport(input: Input, model: &mut Model, height: usize, bar: Rect) {
    match input {
        Input::PageUp => model.scroll_by(-(height as isize), height),
        Input::PageDown => model.scroll_by(height as isize, height),
        Input::Top => model.scroll.top(),
        Input::Bottom => model.scroll.follow(),
        Input::JumpPrev => model.jump_prev(height),
        Input::JumpNext => model.jump_next(height),
        Input::WheelUp => model.scroll_by(-WHEEL_LINES, height),
        Input::WheelDown => model.scroll_by(WHEEL_LINES, height),
        Input::Grab(row) => {
            // A press or drag maps the pointer's track row back to an offset; the bottom row
            // is the sticky tail, exactly where wheeling or keying down to it would land.
            let total = model.wrapped_lines().len();
            let max = total.saturating_sub(height);
            if max > 0 {
                let row_in_track = usize::from(row).saturating_sub(usize::from(bar.y));
                let target = scrollbar::offset_for_row(row_in_track, total, height);
                if target >= max {
                    model.scroll.follow();
                } else {
                    model.scroll.jump_to(target, total, height);
                }
            }
        }
        Input::Cancel | Input::Continue | Input::Submit | Input::Quit => {}
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
    WheelUp,
    WheelDown,
    /// A scrollbar press or drag, carrying the pointer's terminal row.
    Grab(u16),
}

/// What a mouse event means for the viewport: a wheel notch scrolls by lines anywhere over
/// the frame, a left press on the scrollbar grabs the window onto that row, and a drag keeps
/// grabbing — even off the column — until the button lifts. qwen-code's scroll-intent
/// pipeline, minus its frame coalescing window: one event, one applied action.
fn mouse_input(event: MouseEvent, bar: Rect, dragging: bool) -> (Option<Input>, bool) {
    match event.kind {
        MouseEventKind::ScrollUp => (Some(Input::WheelUp), dragging),
        MouseEventKind::ScrollDown => (Some(Input::WheelDown), dragging),
        MouseEventKind::Down(MouseButton::Left) if over_bar(bar, event) => {
            (Some(Input::Grab(event.row)), true)
        }
        MouseEventKind::Drag(MouseButton::Left) if dragging => (Some(Input::Grab(event.row)), true),
        MouseEventKind::Up(MouseButton::Left) => (None, false),
        _ => (None, dragging),
    }
}

fn over_bar(bar: Rect, event: MouseEvent) -> bool {
    event.column >= bar.x
        && event.column < bar.x + bar.width
        && event.row >= bar.y
        && event.row < bar.y + bar.height
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

    fn mouse(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::empty(),
        }
    }

    fn mouse_at(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::empty(),
        }
    }

    #[test]
    fn the_wheel_scrolls_and_the_bar_grabs() {
        let bar = Rect::new(79, 2, 1, 10);
        let (input, dragging) = mouse_input(mouse(MouseEventKind::ScrollUp), bar, false);
        assert_eq!(input, Some(Input::WheelUp));
        assert!(!dragging, "the wheel never starts a drag");
        let (input, _) = mouse_input(mouse(MouseEventKind::ScrollDown), bar, false);
        assert_eq!(input, Some(Input::WheelDown));

        let (input, dragging) = mouse_input(
            mouse_at(MouseEventKind::Down(MouseButton::Left), 79, 5),
            bar,
            false,
        );
        assert_eq!(input, Some(Input::Grab(5)));
        assert!(dragging, "a press on the bar starts a drag");

        let (input, dragging) = mouse_input(
            mouse_at(MouseEventKind::Down(MouseButton::Left), 10, 5),
            bar,
            false,
        );
        assert_eq!(input, None, "a press in the text is not a grab");
        assert!(!dragging);

        // A drag keeps grabbing after the pointer leaves the column, until the button lifts.
        let (input, dragging) = mouse_input(
            mouse_at(MouseEventKind::Drag(MouseButton::Left), 3, 8),
            bar,
            true,
        );
        assert_eq!(input, Some(Input::Grab(8)));
        assert!(dragging);
        let (input, dragging) = mouse_input(
            mouse_at(MouseEventKind::Drag(MouseButton::Left), 3, 8),
            bar,
            false,
        );
        assert_eq!(input, None, "motion without a held button is nothing");
        assert!(!dragging);

        let (input, dragging) =
            mouse_input(mouse(MouseEventKind::Up(MouseButton::Left)), bar, true);
        assert_eq!(input, None);
        assert!(!dragging, "the release ends the drag");
    }

    #[test]
    fn wheel_ticks_walk_the_transcript_three_lines_at_a_time() {
        let mut model = model();
        for index in 0..20 {
            model.push_user(&format!("question {index}"));
            model.push_assistant_text(&format!("answer {index}"));
        }
        model.relayout(60);
        let height = 10;
        let bar = Rect::new(59, 0, 1, 10);
        assert!(model.scroll.following());
        viewport(Input::WheelUp, &mut model, height, bar);
        let tail = model.wrapped_lines().len() - height;
        assert_eq!(model.scroll_offset(height), tail - 3);
        viewport(Input::WheelUp, &mut model, height, bar);
        assert_eq!(model.scroll_offset(height), tail - 6);
        viewport(Input::WheelDown, &mut model, height, bar);
        viewport(Input::WheelDown, &mut model, height, bar);
        assert!(
            model.scroll.following(),
            "wheeling back to the tail resumes follow"
        );
    }

    #[test]
    fn grabbing_the_bar_puts_the_pointed_row_at_the_top_and_its_bottom_at_the_tail() {
        let mut model = model();
        for index in 0..20 {
            model.push_user(&format!("question {index}"));
            model.push_assistant_text(&format!("answer {index}"));
        }
        model.relayout(60);
        let height = 10;
        let bar = Rect::new(59, 0, 1, 10);
        let total = model.wrapped_lines().len();

        viewport(Input::Grab(0), &mut model, height, bar);
        assert_eq!(model.scroll_offset(height), 0);
        assert!(!model.scroll.following(), "a grab pins the window");

        viewport(Input::Grab(5), &mut model, height, bar);
        assert_eq!(
            model.scroll_offset(height),
            scrollbar::offset_for_row(5, total, height)
        );

        viewport(Input::Grab(9), &mut model, height, bar);
        assert!(
            model.scroll.following(),
            "the bottom track row is the sticky tail"
        );
    }
}
