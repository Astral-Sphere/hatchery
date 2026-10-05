//! The TUI model and frame composition: a projection of the event stream, nothing more.
//!
//! Rendering stays a pure function of the [`Model`] (golden frames over ratatui's TestBackend):
//! the model changes only through [`Model::push_event`], input handling, [`Model::tick`] and
//! [`Model::relayout`], and every action leaves through the protocol. The widget layer
//! ([`widgets`]) owns the surfaces; this module owns the state they project and the one
//! constraint that makes scrolling possible — the wrapped-line cache, rebuilt by `relayout`
//! whenever the model or the width changes, so scroll offsets and message jumps address real
//! rendered lines instead of guessing at them.
//!
//! Layout, top to bottom: transcript, toasts, turn indicator, composer, hint line, status line.
//! Both references keep the status at the bottom and the input in a box; the transcript owns
//! everything else, and scrolling it never loses history to the terminal's scrollback because
//! the TUI owns the alternate screen.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;

use hatchery_protocol::{ItemId, ServerEvent, ToolStatus};

pub mod theme;
pub mod widgets;

mod scroll;
pub use scroll::ScrollState;

use theme::{Theme, ThemeSetting};
use widgets::{composer, indicator, scrollbar, status, toasts, transcript};

/// The TUI's whole state: what the events said, what the user is typing, and where the window
/// sits in the wrapped transcript.
#[derive(Debug)]
pub struct Model {
    /// Conversation cells in order, oldest first.
    pub cells: Vec<Cell>,
    /// The input being typed.
    pub input: String,
    /// Status line content: mode / model / effort / session state.
    pub status: Status,
    /// Transient notes above the indicator.
    pub toasts: Vec<Toast>,
    /// Where the transcript window sits; `None` follows the tail.
    pub scroll: ScrollState,
    /// Global reasoning visibility; Ctrl+R toggles, `ui.show_reasoning` seeds it.
    pub show_reasoning: bool,
    /// The palette in effect.
    pub theme: Theme,
    /// What the user asked for; `/theme` and `ui.theme` set it.
    pub theme_setting: ThemeSetting,
    /// Animation clock: four ticks a second.
    pub tick: u64,
    /// Tick the current turn started running, for the indicator's elapsed seconds.
    pub turn_started_tick: Option<u64>,
    probe: Option<(u8, u8, u8)>,
    colorfgbg: Option<String>,
    wrapped: Vec<Line<'static>>,
    /// Which cell each wrapped row belongs to, so a click can fold its cell.
    cell_of: Vec<usize>,
    user_starts: Vec<usize>,
    dirty: bool,
    last_width: u16,
}

/// One renderable conversation piece.
#[derive(Debug)]
pub struct Cell {
    /// What it is (drives the glyph and the fold).
    pub kind: CellKind,
    /// Accumulated raw text (markdown for assistant, plain otherwise).
    pub raw: String,
    /// The protocol item this cell projects, for tool lifecycle updates.
    pub item: Option<ItemId>,
    /// Tool-call detail, when this is a tool cell.
    pub tool: Option<ToolCell>,
    /// Per-cell fold override set by a click; `None` follows the kind's default
    /// (reasoning: the global Ctrl+R state, tool: expanded).
    pub expanded: Option<bool>,
}

/// The kinds of cell; the glyph and the fold behaviour follow from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellKind {
    /// The one-line session summary drawn once at the top.
    Banner,
    /// What the user typed.
    User,
    /// The model's reply, markdown.
    Assistant,
    /// Reasoning, foldable.
    Reasoning,
    /// A tool call cell with a lifecycle glyph.
    Tool,
}

/// What the transcript knows about one tool call.
#[derive(Debug)]
pub struct ToolCell {
    /// Registered tool name, known once the item finishes.
    pub name: Option<String>,
    /// The human-readable summary title.
    pub title: String,
    /// The summary's second line, when there is one.
    pub detail: Option<String>,
    /// Where the call is in its lifecycle.
    pub status: ToolStatus,
    /// Last progress line, capped.
    pub tail: String,
}

/// A transient note's flavour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    /// Something succeeded.
    Ok,
    /// Something failed.
    Error,
    /// Neither.
    Info,
}

/// A transient note.
#[derive(Debug)]
pub struct Toast {
    /// The text.
    pub text: String,
    /// The flavour, and with it the icon and colour.
    pub kind: ToastKind,
    /// Tick it appeared, for expiry.
    pub born: u64,
}

/// The status line's content.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// Session mode id.
    pub mode: String,
    /// `provider/model`.
    pub model: String,
    /// Reasoning effort as configured.
    pub effort: String,
    /// Idle / thinking / awaiting approval / rate limited / failed.
    pub state: String,
}

impl Model {
    /// A model seeded from the hello/config round.
    #[must_use]
    pub fn new(
        model: String,
        effort: String,
        show_reasoning: bool,
        setting: ThemeSetting,
        theme: Theme,
        probe: Option<(u8, u8, u8)>,
        workspace: String,
    ) -> Self {
        let mut seeded = Self {
            cells: Vec::new(),
            input: String::new(),
            status: Status {
                mode: "chat".to_owned(),
                model: model.clone(),
                effort: effort.clone(),
                state: "idle".to_owned(),
            },
            toasts: Vec::new(),
            scroll: ScrollState::default(),
            show_reasoning,
            theme,
            theme_setting: setting,
            tick: 0,
            turn_started_tick: None,
            probe,
            colorfgbg: std::env::var("COLORFGBG").ok(),
            wrapped: Vec::new(),
            cell_of: Vec::new(),
            user_starts: Vec::new(),
            dirty: true,
            last_width: 0,
        };
        seeded.cells.push(Cell {
            kind: CellKind::Banner,
            raw: format!("hatchery · {model} · effort {effort} · {workspace}"),
            item: None,
            tool: None,
            expanded: None,
        });
        seeded
    }

    /// Appends one text delta to the streaming assistant cell, creating it if needed.
    pub fn push_assistant_text(&mut self, text: &str) {
        self.append_to_last(CellKind::Assistant, text);
    }

    /// Appends one reasoning delta; the cell is what folds.
    pub fn push_reasoning(&mut self, text: &str) {
        self.append_to_last(CellKind::Reasoning, text);
    }

    /// Echoes the user's own line, verbatim (a prompt is not markdown).
    pub fn push_user(&mut self, text: &str) {
        self.cells.push(Cell {
            kind: CellKind::User,
            raw: text.to_owned(),
            item: None,
            tool: None,
            expanded: None,
        });
        self.dirty = true;
        self.scroll.follow();
    }

    /// Adds a transient note.
    pub fn note(&mut self, text: impl Into<String>, kind: ToastKind) {
        self.toasts.push(Toast {
            text: text.into(),
            kind,
            born: self.tick,
        });
    }

    fn append_to_last(&mut self, kind: CellKind, text: &str) {
        if let Some(last) = self.cells.last_mut()
            && last.kind == kind
        {
            last.raw.push_str(text);
            self.dirty = true;
            return;
        }
        self.cells.push(Cell {
            kind,
            raw: text.to_owned(),
            item: None,
            tool: None,
            expanded: None,
        });
        self.dirty = true;
    }

    /// Applies one protocol event; the only mutation path besides input handling and the tick.
    pub fn push_event(&mut self, event: &ServerEvent) {
        match event {
            ServerEvent::TextDelta { text, .. } => self.push_assistant_text(text),
            ServerEvent::ReasoningDelta { text, .. } => self.push_reasoning(text),
            ServerEvent::ToolCallStarted { item, summary } => {
                self.cells.push(Cell {
                    kind: CellKind::Tool,
                    raw: String::new(),
                    item: Some(*item),
                    tool: Some(ToolCell {
                        name: None,
                        title: summary.title.clone(),
                        detail: summary.detail.clone(),
                        status: ToolStatus::Running,
                        tail: String::new(),
                    }),
                    expanded: None,
                });
                self.dirty = true;
            }
            ServerEvent::ToolCallProgress { item, chunk } => {
                if let Some(tool) = self.tool_mut(*item) {
                    let tail = chunk.lines().next_back().unwrap_or_default().trim_end();
                    tool.tail = tail.chars().take(160).collect();
                    self.dirty = true;
                }
            }
            ServerEvent::ItemFinished { item } => {
                if let hatchery_protocol::ItemKind::ToolCall(call) = &item.kind
                    && let Some(tool) = self.tool_mut(item.id)
                {
                    tool.name = Some(call.name.clone());
                    tool.status = call.status;
                    self.dirty = true;
                }
            }
            ServerEvent::ApprovalRequested { .. } => {
                self.status.state = "awaiting approval".to_owned();
                self.dirty = true;
            }
            ServerEvent::SessionUpdated { state } => {
                self.status.mode = state.mode.as_str().to_owned();
                self.status.model = format!("{}/{}", state.model.provider, state.model.model);
                if let Some(effort) = effort_of(state) {
                    self.status.effort = effort;
                }
                // The daemon knows what the session is doing; the line mirrors it instead of
                // guessing "idle" while a whole turn streams past.
                let running = matches!(state.status, hatchery_protocol::SessionStatus::Running);
                let was_running = self.turn_started_tick.is_some();
                self.status.state = match state.status {
                    hatchery_protocol::SessionStatus::Running => "thinking".to_owned(),
                    hatchery_protocol::SessionStatus::WaitingApproval => {
                        "awaiting approval".to_owned()
                    }
                    hatchery_protocol::SessionStatus::Idle => "idle".to_owned(),
                    hatchery_protocol::SessionStatus::Error => "failed".to_owned(),
                };
                self.turn_started_tick = match (running, was_running) {
                    (true, false) => Some(self.tick),
                    (true, true) => self.turn_started_tick,
                    (false, _) => None,
                };
                self.dirty = true;
            }
            ServerEvent::TurnFinished { completion, .. } => {
                self.status.state = "idle".to_owned();
                self.turn_started_tick = None;
                if let Some(usage) = completion.usage.as_ref() {
                    self.note(
                        format!(
                            "turn finished: {} completion tokens",
                            usage.completion_tokens.unwrap_or(0)
                        ),
                        ToastKind::Ok,
                    );
                }
                self.dirty = true;
            }
            ServerEvent::TurnFailed { error, .. } => {
                self.status.state = "failed".to_owned();
                self.turn_started_tick = None;
                self.note(error.message.clone(), ToastKind::Error);
                self.dirty = true;
            }
            ServerEvent::RateLimited { retry_after_ms } => {
                self.status.state = format!("rate limited ({}s)", retry_after_ms / 1000);
                self.dirty = true;
            }
            _ => {}
        }
    }

    fn tool_mut(&mut self, item: ItemId) -> Option<&mut ToolCell> {
        self.cells
            .iter_mut()
            .rev()
            .find(|cell| cell.item == Some(item))
            .and_then(|cell| cell.tool.as_mut())
    }

    /// Advances the animation clock; spinner frames, elapsed seconds and toast expiry read it.
    pub fn tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        if self.busy() {
            self.dirty = true;
        }
    }

    /// True while anything animates: a running turn or an in-flight tool call.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.status.state == "thinking"
            || self.status.state == "awaiting approval"
            || self.status.state.starts_with("rate limited")
            || self.cells.iter().any(|cell| {
                cell.tool
                    .as_ref()
                    .is_some_and(|tool| tool.status == ToolStatus::Running)
            })
    }

    /// Whole seconds the current turn has been running.
    #[must_use]
    pub fn turn_seconds(&self) -> u64 {
        self.turn_started_tick
            .map(|started| self.tick.saturating_sub(started) / 4)
            .unwrap_or(0)
    }

    /// Toggles the reasoning fold; the transcript must re-wrap, so this dirties the cache.
    /// The master switch speaks for every reasoning cell again: per-cell clicks were
    /// exceptions to the old global state, not to the new one.
    pub fn toggle_reasoning(&mut self) {
        self.show_reasoning = !self.show_reasoning;
        for cell in &mut self.cells {
            if cell.kind == CellKind::Reasoning {
                cell.expanded = None;
            }
        }
        self.dirty = true;
    }

    /// Folds or unfolds one cell against its kind's default; a click sets the override.
    pub fn toggle_cell(&mut self, index: usize) {
        let Some(cell) = self.cells.get_mut(index) else {
            return;
        };
        let current = match cell.kind {
            CellKind::Reasoning => cell.expanded.unwrap_or(self.show_reasoning),
            CellKind::Tool => cell.expanded.unwrap_or(true),
            _ => return,
        };
        cell.expanded = Some(!current);
        self.dirty = true;
    }

    /// Re-resolves the palette for a new setting, reusing the startup probe.
    pub fn set_theme(&mut self, setting: ThemeSetting) {
        self.theme_setting = setting;
        self.theme = theme::resolve(setting, self.probe, self.colorfgbg.as_deref());
        self.dirty = true;
    }

    /// True when the wrapped-line cache is stale for this width.
    #[must_use]
    pub fn needs_relayout(&self, width: u16) -> bool {
        self.dirty || width != self.last_width
    }

    /// Rebuilds the wrapped-line cache: every cell rendered, then wrapped at `width`, with the
    /// start of each user cell recorded for message jumps.
    pub fn relayout(&mut self, width: u16) {
        if !self.needs_relayout(width) {
            return;
        }
        let width = usize::from(width.max(8));
        // The scrollbar gutter keeps its column whether or not the bar shows, so the wrap
        // width never depends on overflow and the cache never reflows mid-scroll.
        let content = width - 1;
        let mut wrapped = Vec::new();
        let mut cell_of = Vec::new();
        let mut starts = Vec::new();
        for (index, cell) in self.cells.iter().enumerate() {
            if index > 0 {
                wrapped.push(Line::from(""));
                cell_of.push(index);
            }
            if cell.kind == CellKind::User {
                starts.push(wrapped.len());
            }
            let lines = transcript::render_cell(cell, &self.theme, self.show_reasoning, self.tick);
            let rows = transcript::wrap_lines(&lines, content);
            cell_of.extend(std::iter::repeat_n(index, rows.len()));
            wrapped.extend(rows);
        }
        self.wrapped = wrapped;
        self.cell_of = cell_of;
        self.user_starts = starts;
        self.dirty = false;
        self.last_width = width as u16;
    }

    /// The wrapped transcript lines the window is cut from.
    #[must_use]
    pub fn wrapped_lines(&self) -> &[Line<'static>] {
        &self.wrapped
    }

    /// Which cell each wrapped row belongs to, for click-to-fold hit testing.
    #[must_use]
    pub fn cell_of_row(&self) -> &[usize] {
        &self.cell_of
    }

    /// Scrolls by whole lines (negative up); the chat loop passes a viewport height.
    pub fn scroll_by(&mut self, delta: isize, height: usize) {
        let total = self.wrapped.len();
        self.scroll.scroll_by(delta, total, height);
    }

    /// Puts the start of the previous user cell at the top of the window.
    pub fn jump_prev(&mut self, height: usize) {
        let total = self.wrapped.len();
        let current = self.scroll.resolve(total, height);
        if let Some(&target) = self
            .user_starts
            .iter()
            .rev()
            .find(|&&start| start < current)
        {
            self.scroll.jump_to(target, total, height);
        }
    }

    /// Puts the start of the next user cell at the top; past the last one, follows the tail.
    pub fn jump_next(&mut self, height: usize) {
        let total = self.wrapped.len();
        let current = self.scroll.resolve(total, height);
        match self.user_starts.iter().find(|&&start| start > current) {
            Some(&target) => {
                // A start beyond the last full window clamps onto where we already are;
                // jumping there would look like nothing happened, so it means "the tail".
                if target.min(total.saturating_sub(height.max(1))) == current {
                    self.scroll.follow();
                } else {
                    self.scroll.jump_to(target, total, height);
                }
            }
            None => self.scroll.follow(),
        }
    }

    /// The resolved window top, for the status line's position readout.
    #[must_use]
    pub fn scroll_offset(&self, height: usize) -> usize {
        self.scroll.resolve(self.wrapped.len(), height)
    }
}

/// The effort string for the status line, read out of the session's config patch.
fn effort_of(state: &hatchery_protocol::Session) -> Option<String> {
    state
        .config_patch
        .as_ref()?
        .get("reasoning_effort")?
        .as_str()
        .map(str::to_owned)
}

/// The frame's six rows, in order: transcript, toasts, indicator, composer, hints, status.
#[must_use]
pub fn layout(model: &Model, area: Rect) -> [Rect; 6] {
    Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(toasts::height(model)),
        Constraint::Length(indicator::height(model)),
        Constraint::Length(composer::height(model)),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area)
}

/// The transcript's two columns: the wrapped text and the scrollbar gutter beside it. The
/// gutter is always one column wide so the bar never overlays text and the wrap width never
/// changes with overflow.
#[must_use]
pub fn transcript_areas(messages: Rect) -> [Rect; 2] {
    let gutter = messages.width.min(1);
    let content = Rect {
        width: messages.width - gutter,
        ..messages
    };
    let bar = Rect {
        x: content.right(),
        width: gutter,
        ..messages
    };
    [content, bar]
}

/// Draws the whole frame. Pure: same model, same buffer.
pub fn draw(model: &Model, frame: &mut Frame) {
    let [messages, notes, indicator, composer, hints, status] = layout(model, frame.area());
    let [content, bar] = transcript_areas(messages);
    let total = model.wrapped_lines().len();
    let offset = model.scroll.resolve(total, messages.height as usize);

    frame.render_widget(transcript::Transcript { model, offset }, content);
    frame.render_widget(
        scrollbar::Scrollbar {
            total,
            offset,
            theme: &model.theme,
        },
        bar,
    );
    frame.render_widget(toasts::Toasts(model), notes);
    frame.render_widget(indicator::Indicator(model), indicator);
    frame.render_widget(composer::Composer(model), composer);
    frame.render_widget(composer::Hints(model), hints);
    frame.render_widget(
        status::StatusBar {
            model,
            offset: (!model.scroll.following()).then_some(offset),
        },
        status,
    );
    if let Some((x, y)) = composer::cursor(model, composer) {
        frame.set_cursor_position((x, y));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn model() -> Model {
        Model::new(
            "deepseek/deepseek-chat".to_owned(),
            "high".to_owned(),
            true,
            ThemeSetting::Dark,
            Theme::dark(),
            None,
            "/tmp/ws".to_owned(),
        )
    }

    fn drawn(model: &Model, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(model, frame)).expect("draw");
        terminal.backend().to_string()
    }

    fn fake_session(model: &str) -> hatchery_protocol::Session {
        let (provider, model_name) = model.split_once('/').expect("shape");
        serde_json::from_value(serde_json::json!({
            "id": hatchery_protocol::SessionId::new(),
            "mode": "chat",
            "model": {"provider": provider, "model": model_name},
            "created_at": 0,
            "updated_at": 0,
            "generation": 0,
            "status": "idle"
        }))
        .expect("session")
    }

    #[test]
    fn the_banner_opens_the_transcript_and_the_status_closes_the_frame() {
        let mut model = model();
        model.relayout(60);
        let frame = drawn(&model, 60, 10);
        let first = frame.lines().next().expect("first row");
        assert!(
            first.contains("hatchery · deepseek/deepseek-chat · effort high"),
            "{frame}"
        );
        let last = frame.lines().last().expect("last row");
        assert!(last.contains("chat"), "{last}");
        assert!(last.contains("effort high"), "{last}");
        assert!(first.contains("/tmp/ws"), "{frame}");
    }

    #[test]
    fn user_and_assistant_cells_carry_their_glyphs() {
        let mut model = model();
        model.push_user("what is rust?");
        model.push_assistant_text("a **systems** language");
        model.relayout(60);
        let frame = drawn(&model, 60, 12);
        assert!(frame.contains("> what is rust?"), "{frame}");
        assert!(frame.contains("◆ a systems language"), "{frame}");
    }

    #[test]
    fn long_lines_wrap_instead_of_clipping() {
        let mut model = model();
        model.push_assistant_text("one two three four five six seven eight nine ten eleven");
        model.relayout(20);
        let window: Vec<String> = model
            .wrapped_lines()
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.to_string())
                    .collect()
            })
            .collect();
        assert!(window.iter().any(|row| row.len() <= 20), "{window:?}");
        let joined = window.join(" ");
        assert!(
            joined.contains("eleven"),
            "the tail of the line survives wrapping"
        );
    }

    #[test]
    fn the_transcript_wraps_one_column_narrower_than_the_terminal() {
        let mut model = model();
        model.push_assistant_text(&"a".repeat(120));
        model.relayout(41);
        let widest = model
            .wrapped_lines()
            .iter()
            .map(Line::width)
            .max()
            .unwrap_or(0);
        assert_eq!(widest, 40, "the gutter column stays free");
    }

    #[test]
    fn the_scrollbar_marks_the_window_and_stays_out_of_a_transcript_that_fits() {
        let mut long = model();
        for index in 0..20 {
            long.push_user(&format!("question {index}"));
            long.push_assistant_text(&format!("answer {index}"));
        }
        long.relayout(60);
        long.scroll.top();
        // `TestBackend::to_string` quotes every row (its buffer view marks multi-width
        // overwrites), so edge assertions strip the quotes first.
        let scrolled = drawn(&long, 60, 12);
        let rows: Vec<&str> = scrolled.lines().map(|row| row.trim_matches('"')).collect();
        assert!(
            rows[0].ends_with('█'),
            "the thumb opens at the top: {rows:?}"
        );
        assert!(rows[6].ends_with('│'), "the track fills the rest: {rows:?}");

        let fits = drawn(&model(), 60, 12);
        let rows: Vec<&str> = fits.lines().map(|row| row.trim_matches('"')).collect();
        assert!(rows[0].ends_with(' '), "no overflow, no bar: {rows:?}");
    }

    #[test]
    fn reasoning_folds_and_ctrl_r_expands() {
        let mut model = Model::new(
            "p/m".to_owned(),
            "medium".to_owned(),
            false,
            ThemeSetting::Dark,
            Theme::dark(),
            None,
            "/tmp".to_owned(),
        );
        model.push_reasoning("let me think about this carefully");
        model.relayout(60);
        let folded = drawn(&model, 60, 10);
        assert!(folded.contains("∴ Thought for 33 chars"), "{folded}");
        assert!(!folded.contains("let me think"), "{folded}");

        model.show_reasoning = true;
        model.dirty = true;
        model.relayout(60);
        let expanded = drawn(&model, 60, 10);
        assert!(expanded.contains("let me think"), "{expanded}");
    }

    #[test]
    fn a_click_folds_one_reasoning_cell_and_ctrl_r_speaks_for_all_again() {
        let mut model = Model::new(
            "p/m".to_owned(),
            "medium".to_owned(),
            false,
            ThemeSetting::Dark,
            Theme::dark(),
            None,
            "/tmp".to_owned(),
        );
        model.push_reasoning("secret thoughts");
        model.relayout(60);
        assert!(
            drawn(&model, 60, 10).contains("∴ Thought for 15 chars"),
            "the global fold starts closed"
        );

        model.toggle_cell(1);
        model.relayout(60);
        let open = drawn(&model, 60, 10);
        assert!(open.contains("secret thoughts"), "{open}");
        assert_eq!(model.cells[1].expanded, Some(true));

        model.toggle_cell(1);
        model.relayout(60);
        assert!(!drawn(&model, 60, 10).contains("secret thoughts"));

        // A per-cell exception survives until the master switch moves, which clears them.
        model.toggle_cell(1);
        assert_eq!(model.cells[1].expanded, Some(true));
        model.toggle_reasoning();
        assert_eq!(
            model.cells[1].expanded, None,
            "Ctrl+R clears the exceptions"
        );
        model.relayout(60);
        assert!(
            drawn(&model, 60, 10).contains("secret thoughts"),
            "the global fold is now open"
        );

        model.toggle_cell(0);
        assert_eq!(model.cells[0].expanded, None, "the banner is not foldable");
    }

    #[test]
    fn clicking_a_tool_cell_hides_its_body_behind_an_ellipsis() {
        let mut model = model();
        let item = hatchery_protocol::ItemId::new();
        model.push_event(&ServerEvent::ToolCallStarted {
            item,
            summary: hatchery_protocol::ToolCallSummary::new("read src/main.rs")
                .with_detail("the whole file"),
        });
        model.relayout(60);
        assert!(drawn(&model, 60, 10).contains("the whole file"));

        model.toggle_cell(1);
        model.relayout(60);
        let folded = drawn(&model, 60, 10);
        assert!(!folded.contains("the whole file"), "{folded}");
        assert!(folded.contains("read src/main.rs"), "{folded}");
        assert!(folded.contains("⋯"), "{folded}");

        model.toggle_cell(1);
        model.relayout(60);
        assert!(drawn(&model, 60, 10).contains("the whole file"));
    }

    #[test]
    fn tool_cells_track_their_lifecycle() {
        let mut model = model();
        let item = hatchery_protocol::ItemId::new();
        model.push_event(&ServerEvent::ToolCallStarted {
            item,
            summary: hatchery_protocol::ToolCallSummary::new("read src/main.rs")
                .with_detail("the whole file"),
        });
        model.relayout(60);
        let running = drawn(&model, 60, 10);
        assert!(running.contains("read src/main.rs"), "{running}");
        assert!(running.contains("the whole file"), "{running}");
        assert!(!running.contains("✓"), "{running}");

        model.push_event(&ServerEvent::ItemFinished {
            item: serde_json::from_value(serde_json::json!({
                "id": item,
                "session": hatchery_protocol::SessionId::new(),
                "kind": "tool_call",
                "payload": {
                    "name": "read_file",
                    "args": {},
                    "status": "completed"
                },
                "created_at": 0
            }))
            .expect("item"),
        });
        model.relayout(60);
        let done = drawn(&model, 60, 10);
        assert!(done.contains("✓ read_file · read src/main.rs"), "{done}");
    }

    #[test]
    fn scrolling_up_reaches_the_first_line_and_end_follows_again() {
        let mut model = model();
        for index in 0..30 {
            model.push_user(&format!("question {index}"));
            model.push_assistant_text(&format!("answer {index} with a little more text"));
        }
        model.relayout(60);
        let height = 10;
        assert!(model.scroll.following());
        model.scroll_by(-1000, height);
        assert_eq!(model.scroll_offset(height), 0);
        let frame = drawn(&model, 80, 16);
        assert!(frame.contains("question 0"), "{frame}");
        assert!(frame.contains("end follows"), "{frame}");

        model.scroll.follow();
        model.relayout(60);
        let tail = drawn(&model, 80, 16);
        assert!(tail.contains("question 29"), "{tail}");
        assert!(!tail.contains("end follows"), "{tail}");
    }

    #[test]
    fn jumps_land_on_user_cells_in_order() {
        let mut model = model();
        for index in 0..5 {
            model.push_user(&format!("question {index}"));
            model.push_assistant_text(&format!("a long answer {index} ").repeat(4));
        }
        model.relayout(60);
        let height = 8;
        model.jump_prev(height);
        let first_stop = model.scroll_offset(height);
        model.jump_prev(height);
        let second_stop = model.scroll_offset(height);
        assert!(second_stop < first_stop, "{second_stop} < {first_stop}");
        // The window top sits on a user cell's first wrapped line.
        assert!(model.user_starts.contains(&second_stop), "{model:?}");
        model.jump_next(height);
        assert_eq!(model.scroll_offset(height), first_stop);
        for _ in 0..10 {
            model.jump_next(height);
        }
        assert!(
            model.scroll.following(),
            "past the last message is the tail"
        );
    }

    #[test]
    fn session_updates_move_the_status_line_and_start_the_clock() {
        let mut model = model();
        let mut state = fake_session("new-provider/new-model");
        state.status = hatchery_protocol::SessionStatus::Running;
        model.push_event(&ServerEvent::SessionUpdated { state });
        assert_eq!(model.status.model, "new-provider/new-model");
        assert_eq!(model.status.state, "thinking");
        assert!(model.turn_started_tick.is_some());
        model.tick();
        model.tick();
        assert!(model.busy(), "a running turn animates");
    }

    #[test]
    fn a_finished_turn_toasts_its_usage() {
        let mut model = model();
        model.push_event(&ServerEvent::TurnFinished {
            turn: hatchery_protocol::TurnId::new(),
            completion: serde_json::from_value(serde_json::json!({
                "reason": "model_done",
                "usage": {"prompt_tokens": 1, "completion_tokens": 6}
            }))
            .expect("completion"),
        });
        assert_eq!(model.status.state, "idle");
        assert!(
            model
                .toasts
                .last()
                .expect("toast")
                .text
                .contains("6 completion tokens"),
            "{:?}",
            model.toasts
        );
    }

    #[test]
    fn a_multi_line_input_gets_its_own_room() {
        let mut model = model();
        assert_eq!(composer::height(&model), 3);
        model.input.push_str("first\nsecond");
        assert_eq!(composer::height(&model), 4);
        model.relayout(60);
        let frame = drawn(&model, 60, 10);
        assert!(frame.contains("first"), "{frame}");
        assert!(frame.contains("second"), "{frame}");
    }

    #[test]
    fn switching_themes_repaints_every_cell() {
        let mut model = model();
        model.push_user("hello");
        model.push_assistant_text("body `code`");
        let dark = transcript::render_cell(&model.cells[2], &Theme::dark(), true, 0);
        let light = transcript::render_cell(&model.cells[2], &Theme::light(), true, 0);
        let fg = |lines: &[Line<'static>]| {
            lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .map(|span| span.style.fg)
                .collect::<Vec<_>>()
        };
        assert_ne!(
            fg(&dark),
            fg(&light),
            "every styled span follows the palette"
        );
    }
}
