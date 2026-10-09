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
    /// The resumed session's active branch, oldest first; empty for a fresh one.
    history: Vec<hatchery_protocol::Item>,
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
    // The subscribing reply carries the session row the status line seeds from, and a resumed
    // session's reply carries the branch to project as well. Decoded once: a long history is
    // not something to clone just to read a model name out of.
    let (row, history) = match args.session {
        Some(_) => {
            let loaded = serde_json::from_value::<m::SessionLoadResult>(reply)
                .map_err(|error| format!("session/load reply: {error}"))?;
            let row = loaded.session.clone();
            let client = attached.client.clone();
            let history = drain_history(loaded, |params| {
                let client = client.clone();
                async move {
                    client
                        .call::<_, m::SessionLoadResult>(m::SESSION_LOAD, &params)
                        .await
                        .map_err(|error| error.to_string())
                }
            })
            .await?;
            (Some(row), history)
        }
        // A fresh session has no branch yet, and a reply that will not decode leaves the status
        // line on its defaults rather than refusing to open the TUI.
        None => (
            serde_json::from_value::<SessionView>(reply)
                .ok()
                .map(|view| view.session),
            Vec::new(),
        ),
    };
    let model_name = row
        .as_ref()
        .map(|row| format!("{}/{}", row.model.provider, row.model.model))
        .unwrap_or_default();
    let effort = row
        .as_ref()
        .and_then(applied_effort)
        .unwrap_or_else(|| "medium".to_owned());
    // `ui.show_reasoning` seeds the fold; a daemon without the key gets the shipped default.
    let show_reasoning = seed_show_reasoning(config_bool(attached, "ui.show_reasoning").await);
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
            history,
        },
    ))
}

/// The whole active branch: the subscribing load's page, plus every page its cursor leads to.
///
/// The cursor is the protocol's contract, not a hint — `next_cursor` set means "call again with
/// `replay_from`" — so a client that reads one page silently truncates the history the moment
/// the store grows into paging. Each follow-up carries the generation the last page reported,
/// which is what makes a runtime that changed mid-replay a refusal instead of a torn transcript
/// (invariant 1).
async fn drain_history<F, Fut>(
    first: m::SessionLoadResult,
    mut next_page: F,
) -> Result<Vec<hatchery_protocol::Item>, String>
where
    F: FnMut(m::SessionLoadParams) -> Fut,
    Fut: std::future::Future<Output = Result<m::SessionLoadResult, String>>,
{
    let m::SessionLoadResult {
        session,
        mut items,
        next_cursor: mut cursor,
    } = first;
    let session_id = session.id;
    let mut generation = Some(session.generation);
    while let Some(replay_from) = cursor {
        let page = next_page(m::SessionLoadParams {
            session_id,
            replay_from: Some(replay_from),
            generation,
        })
        .await?;
        // Handing back the cursor it was given would replay forever, and the TUI has not drawn
        // its first frame yet, so there would be nothing on screen to explain the hang with.
        if page.next_cursor == Some(replay_from) {
            return Err(format!(
                "the replay cursor {replay_from} did not advance; the history is incomplete"
            ));
        }
        generation = Some(page.session.generation);
        cursor = page.next_cursor;
        items.extend(page.items);
    }
    Ok(items)
}

/// Whether reasoning opens folded when the daemon has no `ui.show_reasoning` to seed from.
///
/// The daemon's builtin answer is folded (ruled 2026-10-07); the fallback must agree with it,
/// because the two are the same setting seen from either side of a missing config key.
fn seed_show_reasoning(configured: Option<bool>) -> bool {
    configured.unwrap_or(false)
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

/// The one field the seeders need out of a `session/new` reply. A resumed session decodes the
/// whole `SessionLoadResult` instead, because that reply also carries the branch to project.
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
    // The resumed branch goes up before the first draw, so the window opens on its tail with
    // the history in it rather than filling in afterwards.
    model.push_history(&seed.history);
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
                        let [content, bar] = tui::transcript_areas(messages);
                        let ctx = MouseCtx {
                            bar,
                            content,
                            offset: model.scroll_offset(messages.height as usize),
                            cell_of: model.cell_of_row(),
                        };
                        let (input, next) = mouse_input(mouse, &ctx, dragging);
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
        Input::ToggleCell(index) => model.toggle_cell(index),
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
    /// A click on a foldable transcript cell, by cell index.
    ToggleCell(usize),
}

/// Where the pointer can land: the scrollbar column, the transcript window, and the
/// row→cell map that turns a click into "fold that cell".
struct MouseCtx<'a> {
    bar: Rect,
    content: Rect,
    offset: usize,
    cell_of: &'a [usize],
}

impl MouseCtx<'_> {
    fn cell_at(&self, event: MouseEvent) -> Option<usize> {
        if event.column >= self.content.x + self.content.width
            || event.row < self.content.y
            || event.row >= self.content.y + self.content.height
        {
            return None;
        }
        let row = self.offset + usize::from(event.row - self.content.y);
        self.cell_of.get(row).copied()
    }
}

/// What a mouse event means for the viewport: a wheel notch scrolls by lines anywhere over
/// the frame, a left press on the scrollbar grabs the window onto that row, a drag keeps
/// grabbing — even off the column — until the button lifts, and a left press on a foldable
/// transcript cell folds or unfolds it. qwen-code's scroll-intent pipeline, minus its frame
/// coalescing window: one event, one applied action.
fn mouse_input(event: MouseEvent, ctx: &MouseCtx<'_>, dragging: bool) -> (Option<Input>, bool) {
    match event.kind {
        MouseEventKind::ScrollUp => (Some(Input::WheelUp), dragging),
        MouseEventKind::ScrollDown => (Some(Input::WheelDown), dragging),
        MouseEventKind::Down(MouseButton::Left) if over_bar(ctx.bar, event) => {
            (Some(Input::Grab(event.row)), true)
        }
        MouseEventKind::Down(MouseButton::Left) => match ctx.cell_at(event) {
            Some(index) => (Some(Input::ToggleCell(index)), dragging),
            None => (None, dragging),
        },
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
            let reply = attached.client.call_raw(method, params).await?;
            match project_reply(&command, method, reply)? {
                Reply::Nothing => {}
                Reply::Note(text) => model.note(text, ToastKind::Info),
                Reply::Prompt(rendered) => model.push_prompt(&rendered),
            }
        }
        return Ok(true);
    }
    model.push_user(line);
    let params = m::SessionPromptParams {
        session_id: *session,
        content: hatchery_protocol::Content::text(line),
        generation: None,
    };
    let accepted = attached
        .client
        .call::<_, m::SessionPromptResult>(m::SESSION_PROMPT, &params)
        .await?;
    // The daemon commits an item for the line just typed and broadcasts it like any other; the
    // turn id is what keeps this client from reading its own message twice.
    model.mark_submitted(accepted.turn);
    Ok(true)
}

/// What a slash command's reply puts on screen.
enum Reply {
    /// Nothing: the status bar carries it.
    Nothing,
    /// A one-line confirmation.
    Note(String),
    /// The assembled prompt, dumped into the transcript.
    Prompt(m::PromptRenderResult),
}

/// Projects a slash command's reply instead of dropping it.
///
/// `/prompt` exists to show the prompt the runtime froze, and `/effort` and `/model` deserve a
/// confirmation naming the value the daemon *applied* rather than the one that was asked for —
/// today their only feedback is the status bar moving when `SessionUpdated` arrives. A reply
/// that does not decode is reported, not swallowed (ADR-0009).
///
/// # Errors
///
/// [`ClientError`] when the reply is not the shape the method documents.
fn project_reply(
    command: &commands::SlashCommand,
    method: &str,
    reply: serde_json::Value,
) -> Result<Reply, ClientError> {
    fn decode<T: serde::de::DeserializeOwned>(
        method: &str,
        reply: serde_json::Value,
    ) -> Result<T, ClientError> {
        serde_json::from_value(reply)
            .map_err(|error| ClientError::Connection(format!("{method} reply: {error}")))
    }
    match method {
        m::PROMPT_RENDER => decode(method, reply).map(Reply::Prompt),
        m::SESSION_SET_CONFIG => {
            let applied = decode::<m::SetConfigResult>(method, reply)?;
            let note = match command {
                commands::SlashCommand::Effort(_) => {
                    applied_effort(&applied.session).map(|effort| format!("effort: {effort}"))
                }
                commands::SlashCommand::Model(_) => Some(format!(
                    "model: {}/{}",
                    applied.session.model.provider, applied.session.model.model
                )),
                _ => None,
            };
            Ok(note.map_or(Reply::Nothing, Reply::Note))
        }
        _ => Ok(Reply::Nothing),
    }
}

/// The reasoning effort the session reports as applied, if it reports one.
fn applied_effort(session: &hatchery_protocol::Session) -> Option<String> {
    session
        .config_patch
        .as_ref()?
        .get("reasoning_effort")?
        .as_str()
        .map(str::to_owned)
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
        let ctx = MouseCtx {
            bar: Rect::new(79, 2, 1, 10),
            content: Rect::new(0, 2, 79, 10),
            offset: 0,
            cell_of: &[],
        };
        let (input, dragging) = mouse_input(mouse(MouseEventKind::ScrollUp), &ctx, false);
        assert_eq!(input, Some(Input::WheelUp));
        assert!(!dragging, "the wheel never starts a drag");
        let (input, _) = mouse_input(mouse(MouseEventKind::ScrollDown), &ctx, false);
        assert_eq!(input, Some(Input::WheelDown));

        let (input, dragging) = mouse_input(
            mouse_at(MouseEventKind::Down(MouseButton::Left), 79, 5),
            &ctx,
            false,
        );
        assert_eq!(input, Some(Input::Grab(5)));
        assert!(dragging, "a press on the bar starts a drag");

        let (input, dragging) = mouse_input(
            mouse_at(MouseEventKind::Down(MouseButton::Left), 10, 5),
            &ctx,
            false,
        );
        assert_eq!(input, None, "a press in the text is not a grab");
        assert!(!dragging);

        // A drag keeps grabbing after the pointer leaves the column, until the button lifts.
        let (input, dragging) = mouse_input(
            mouse_at(MouseEventKind::Drag(MouseButton::Left), 3, 8),
            &ctx,
            true,
        );
        assert_eq!(input, Some(Input::Grab(8)));
        assert!(dragging);
        let (input, dragging) = mouse_input(
            mouse_at(MouseEventKind::Drag(MouseButton::Left), 3, 8),
            &ctx,
            false,
        );
        assert_eq!(input, None, "motion without a held button is nothing");
        assert!(!dragging);

        let (input, dragging) =
            mouse_input(mouse(MouseEventKind::Up(MouseButton::Left)), &ctx, true);
        assert_eq!(input, None);
        assert!(!dragging, "the release ends the drag");
    }

    #[test]
    fn a_click_addresses_the_cell_under_the_pointer_through_the_window_offset() {
        // Rows 0-1 are the banner, row 2 a reasoning fold line, row 3 its neighbour's.
        let cell_of = [0, 0, 1, 2];
        let ctx = MouseCtx {
            bar: Rect::new(79, 0, 1, 4),
            content: Rect::new(0, 0, 79, 4),
            offset: 0,
            cell_of: &cell_of,
        };
        let (input, _) = mouse_input(
            mouse_at(MouseEventKind::Down(MouseButton::Left), 5, 2),
            &ctx,
            false,
        );
        assert_eq!(input, Some(Input::ToggleCell(1)));

        // A scrolled window addresses the same screen row further down the transcript.
        let scrolled = MouseCtx { offset: 2, ..ctx };
        let (input, _) = mouse_input(
            mouse_at(MouseEventKind::Down(MouseButton::Left), 5, 1),
            &scrolled,
            false,
        );
        assert_eq!(input, Some(Input::ToggleCell(2)));

        // Below the transcript and on the gutter column, a click is not a fold.
        let (input, _) = mouse_input(
            mouse_at(MouseEventKind::Down(MouseButton::Left), 5, 4),
            &ctx,
            false,
        );
        assert_eq!(input, None);
        let (input, _) = mouse_input(
            mouse_at(MouseEventKind::Down(MouseButton::Left), 79, 2),
            &ctx,
            false,
        );
        assert_eq!(input, Some(Input::Grab(2)), "the bar wins over the fold");
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

    /// The reasoning fold a daemon with no `ui.show_reasoning` key gets: folded, matching the
    /// daemon's own builtin answer for the same setting.
    #[test]
    fn reasoning_starts_folded_when_the_daemon_says_nothing() {
        assert!(
            !seed_show_reasoning(None),
            "the CLI's fallback and the daemon's builtin must agree"
        );
        assert!(seed_show_reasoning(Some(true)));
        assert!(!seed_show_reasoning(Some(false)));
    }

    fn fake_session(session: SessionId, generation: u64) -> hatchery_protocol::Session {
        serde_json::from_value(serde_json::json!({
            "id": session,
            "mode": "chat",
            "model": {"provider": "p", "model": "m"},
            "created_at": 0,
            "updated_at": 0,
            "generation": generation,
            "status": "idle"
        }))
        .expect("session")
    }

    fn load_page(
        session: SessionId,
        texts: &[&str],
        next_cursor: Option<hatchery_protocol::ItemId>,
        generation: u64,
    ) -> m::SessionLoadResult {
        m::SessionLoadResult {
            session: fake_session(session, generation),
            items: texts
                .iter()
                .map(|text| {
                    hatchery_protocol::Item::new(
                        session,
                        hatchery_protocol::ItemKind::UserMessage(hatchery_protocol::Content::text(
                            *text,
                        )),
                    )
                })
                .collect(),
            next_cursor,
        }
    }

    fn texts(items: &[hatchery_protocol::Item]) -> Vec<String> {
        items
            .iter()
            .map(|item| match &item.kind {
                hatchery_protocol::ItemKind::UserMessage(content) => content.text.clone(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[tokio::test]
    async fn the_replay_cursor_is_followed_to_the_end_of_the_branch() {
        let session = SessionId::new();
        let first_cursor = hatchery_protocol::ItemId::new();
        let second_cursor = hatchery_protocol::ItemId::new();
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = std::sync::Arc::clone(&asked);
        let items = drain_history(
            load_page(session, &["one"], Some(first_cursor), 1),
            move |params| {
                let recorder = std::sync::Arc::clone(&recorder);
                async move {
                    let cursor = params.replay_from.expect("a follow-up asks from a cursor");
                    recorder.lock().expect("recorder").push(params);
                    Ok(if cursor == first_cursor {
                        load_page(session, &["two"], Some(second_cursor), 2)
                    } else {
                        load_page(session, &["three"], None, 3)
                    })
                }
            },
        )
        .await
        .expect("replay");
        assert_eq!(
            texts(&items),
            vec!["one".to_owned(), "two".to_owned(), "three".to_owned()],
            "every page of the branch is projected, oldest first"
        );
        let asked = asked.lock().expect("recorder");
        assert_eq!(asked.len(), 2, "one call per cursor: {asked:?}");
        assert_eq!(asked[0].session_id, session);
        assert_eq!(asked[0].replay_from, Some(first_cursor));
        assert_eq!(
            asked[0].generation,
            Some(1),
            "each follow-up carries the generation the previous page reported"
        );
        assert_eq!(asked[1].replay_from, Some(second_cursor));
        assert_eq!(asked[1].generation, Some(2));
    }

    #[tokio::test]
    async fn a_replay_cursor_that_does_not_advance_is_refused() {
        let session = SessionId::new();
        let stuck = hatchery_protocol::ItemId::new();
        let error = drain_history(
            load_page(session, &["one"], Some(stuck), 1),
            move |params| async move {
                // A daemon that hands back the cursor it was given would spin the TUI forever.
                Ok(load_page(session, &[], params.replay_from, 1))
            },
        )
        .await
        .expect_err("a cursor that never advances is a refusal, not a hang");
        assert!(error.contains("cursor"), "{error}");
    }

    /// A chat/completions stream with one answer, a stop and usage — the full field shape,
    /// since the wire layer skips chunks it cannot deserialise.
    const SSE_ONE_ANSWER: &str = concat!(
        "data: {\"id\":\"1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"the answer you are owed\"},\"finish_reason\":\"stop\"}],\"created\":1718345013,\"model\":\"m\",\"object\":\"chat.completion.chunk\",\"usage\":null}\n\n",
        "data: {\"choices\":[],\"created\":1718345013,\"id\":\"1\",\"model\":\"m\",\"object\":\"chat.completion.chunk\",\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":4,\"total_tokens\":7}}\n\n",
        "data: [DONE]\n\n",
    );

    /// The two builtin rows the audit expects, plus the provider under test. `PATH` stands in
    /// for a credential: every environment has it, and a real request reads it.
    fn provider_toml(base_url: &str) -> toml::Table {
        toml::from_str(&format!(
            "[providers.deepseek]\nenv_key = \"\"\n\n[providers.qwen]\nenv_key = \"\"\n\n[providers.testprov]\nbase_url = \"{base_url}\"\nenv_key = \"PATH\"\nmodels = [\"m\"]\n"
        ))
        .expect("toml")
    }

    fn new_params() -> m::SessionNewParams {
        m::SessionNewParams {
            mode: hatchery_protocol::SessionModeId::chat(),
            workspace: None,
            model: Some(hatchery_protocol::ModelRef::new("testprov", "m")),
            title: None,
            config_patch: None,
        }
    }

    fn cells(model: &Model) -> Vec<(crate::tui::CellKind, String)> {
        model
            .cells
            .iter()
            .map(|cell| (cell.kind, cell.raw.clone()))
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resuming_a_session_projects_the_branch_it_loaded() {
        use hatchery_testkit::{ClientProbe, TestDaemon, wire::MockWire};

        let wire = MockWire::replay_sse(SSE_ONE_ANSWER).await;
        let daemon =
            TestDaemon::start(vec![(m::ConfigOrigin::User, provider_toml(&wire.url()))]).await;
        let probe = ClientProbe::attach(&daemon).await;
        let created: m::SessionNewResult = probe.call(m::SESSION_NEW, &new_params()).await;
        let session = created.session.id;
        let mut events = probe.events(&daemon, session).await;
        let accepted: m::SessionPromptResult = probe
            .call(
                m::SESSION_PROMPT,
                &m::SessionPromptParams {
                    session_id: session,
                    content: hatchery_protocol::Content::text("what did I ask before?"),
                    generation: None,
                },
            )
            .await;
        // The branch is only loadable once the turn that wrote it has ended.
        loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(30), events.next())
                .await
                .expect("the turn ends")
                .expect("the stream stays up");
            if matches!(event.event, hatchery_protocol::ServerEvent::TurnFinished { turn, .. } if turn == accepted.turn)
            {
                break;
            }
        }
        drop(events);

        let info = daemon.state.discover().expect("published");
        let attached = attach::attach_to(&info).await.expect("attached");
        let args = ChatArgs {
            session: Some(session),
            workspace: None,
            model: None,
            state_dir: None,
        };
        let (_stream, _id, seed) = open_session(&attached, &args)
            .await
            .expect("the resumed session opens");
        assert_eq!(
            seed.model_name, "testprov/m",
            "the status line seeds from the resumed session's own row"
        );
        assert!(
            !seed.history.is_empty(),
            "the load reply carries the branch it just served"
        );

        let mut model = model();
        model.push_history(&seed.history);
        model.relayout(80);
        let projected = cells(&model);
        assert!(
            projected
                .iter()
                .any(|(kind, raw)| *kind == crate::tui::CellKind::User
                    && raw.contains("what did I ask before?")),
            "the question the previous turn asked is on screen: {projected:?}"
        );
        assert!(
            projected
                .iter()
                .any(|(kind, raw)| *kind == crate::tui::CellKind::Assistant
                    && raw.contains("the answer you are owed")),
            "and so is the answer: {projected:?}"
        );
        daemon.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_prompt_from_another_client_reaches_the_observing_transcript() {
        use hatchery_testkit::{ClientProbe, TestDaemon, wire::MockWire};

        let wire = MockWire::replay_sse(SSE_ONE_ANSWER).await;
        let daemon =
            TestDaemon::start(vec![(m::ConfigOrigin::User, provider_toml(&wire.url()))]).await;
        // The observing front-end: subscribed to the session, typing nothing itself.
        let observer = ClientProbe::attach(&daemon).await;
        let created: m::SessionNewResult = observer.call(m::SESSION_NEW, &new_params()).await;
        let mut events = observer.events(&daemon, created.session.id).await;

        // The prompting front-end, on its own connection.
        let info = daemon.state.discover().expect("published");
        let prompter = attach::attach_to(&info).await.expect("attached");
        let _: m::SessionPromptResult = prompter
            .client
            .call(
                m::SESSION_PROMPT,
                &m::SessionPromptParams {
                    session_id: created.session.id,
                    content: hatchery_protocol::Content::text("typed elsewhere"),
                    generation: None,
                },
            )
            .await
            .expect("the other client's prompt is accepted");

        let mut model = model();
        loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(30), events.next())
                .await
                .expect("the turn ends")
                .expect("the stream stays up");
            let last = matches!(
                event.event,
                hatchery_protocol::ServerEvent::TurnFinished { .. }
            );
            model.push_event(&event.event);
            if last {
                break;
            }
        }
        model.relayout(80);
        let projected = cells(&model);
        let users: Vec<&String> = projected
            .iter()
            .filter(|(kind, _)| *kind == crate::tui::CellKind::User)
            .map(|(_, raw)| raw)
            .collect();
        assert_eq!(
            users.len(),
            1,
            "the question arrives once, through its item: {projected:?}"
        );
        assert!(
            users[0].contains("typed elsewhere"),
            "both front-ends see the same stream: {projected:?}"
        );
        let answers: Vec<&String> = projected
            .iter()
            .filter(|(kind, _)| *kind == crate::tui::CellKind::Assistant)
            .map(|(_, raw)| raw)
            .collect();
        assert_eq!(
            answers.len(),
            1,
            "the answer is the deltas, not the deltas plus its item: {projected:?}"
        );
        assert!(
            answers[0].contains("the answer you are owed"),
            "{projected:?}"
        );
        daemon.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_prompt_command_puts_the_assembled_prompt_on_screen() {
        use hatchery_testkit::TestDaemon;

        let daemon = TestDaemon::start(vec![(
            m::ConfigOrigin::User,
            provider_toml("http://127.0.0.1:1"),
        )])
        .await;
        let info = daemon.state.discover().expect("published");
        let attached = attach::attach_to(&info).await.expect("attached");
        let created: m::SessionNewResult = attached
            .client
            .call(m::SESSION_NEW, &new_params())
            .await
            .expect("session/new");

        let mut model = model();
        submit_line(&attached, &created.session.id, &mut model, "/prompt")
            .await
            .expect("/prompt is accepted");
        model.relayout(80);
        let projected = cells(&model);
        let dump = projected
            .iter()
            .find(|(kind, _)| *kind == crate::tui::CellKind::Prompt)
            .map(|(_, raw)| raw.clone());
        let Some(dump) = dump else {
            panic!("`/prompt` exists to show the assembled prompt: {projected:?}");
        };
        assert!(dump.contains("identity"), "sections keep their ids: {dump}");
        assert!(
            dump.contains("safety_gate"),
            "every section is rendered, not just the first: {dump}"
        );
        assert!(dump.contains("builtin"), "and where each came from: {dump}");
        daemon.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn effort_and_model_answer_with_a_confirmation_from_the_reply() {
        use hatchery_testkit::TestDaemon;

        let daemon = TestDaemon::start(vec![(
            m::ConfigOrigin::User,
            provider_toml("http://127.0.0.1:1"),
        )])
        .await;
        let info = daemon.state.discover().expect("published");
        let attached = attach::attach_to(&info).await.expect("attached");
        let created: m::SessionNewResult = attached
            .client
            .call(m::SESSION_NEW, &new_params())
            .await
            .expect("session/new");

        let mut model = model();
        submit_line(&attached, &created.session.id, &mut model, "/effort low")
            .await
            .expect("/effort is accepted");
        let notes: Vec<String> = model
            .toasts
            .iter()
            .map(|toast| toast.text.clone())
            .collect();
        assert!(
            notes.iter().any(|note| note.contains("low")),
            "the applied effort is confirmed from the reply: {notes:?}"
        );

        submit_line(
            &attached,
            &created.session.id,
            &mut model,
            "/model testprov/m",
        )
        .await
        .expect("/model is accepted");
        let notes: Vec<String> = model
            .toasts
            .iter()
            .map(|toast| toast.text.clone())
            .collect();
        assert!(
            notes.iter().any(|note| note.contains("testprov/m")),
            "and so is the applied model: {notes:?}"
        );
        daemon.stop().await;
    }
}
