//! The minimal TUI: a projection of the event stream, an input line, and nothing else.
//!
//! Rendering is a pure function of the [`Model`] (TestBackend golden frames in tests), the
//! model only transforms via [`Model::push_event`] and input handling, and every action goes
//! out through the protocol. Reasoning folds behind Ctrl+R and respects `ui.show_reasoning`.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use hatchery_protocol::ServerEvent;

/// The TUI's whole state: what the events said, plus what the user is typing.
#[derive(Debug, Default)]
pub struct Model {
    /// Blocks in order: user messages, reasoning, assistant text, tool summaries.
    pub blocks: Vec<Block>,
    /// The input line being typed.
    pub input: String,
    /// Status bar: mode / model / effort / session status.
    pub status: Status,
    /// Transient notes (unknown command, usage line) shown above the input.
    pub notes: Vec<String>,
    /// Global reasoning visibility; Ctrl+R toggles, config seeds it.
    pub show_reasoning: bool,
    /// Auto-scroll to the newest line.
    pub follow: bool,
}

/// One renderable conversation piece.
#[derive(Debug)]
pub struct Block {
    /// What it is (drives the prefix and the fold).
    pub kind: BlockKind,
    /// Accumulated raw text (markdown for assistant, plain otherwise).
    pub raw: String,
    /// Rendered lines, rebuilt when the block ends or folds.
    pub lines: Vec<Line<'static>>,
}

/// The kinds of block; the tag in the status bar and the fold behaviour follow from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockKind {
    /// What the user typed.
    User,
    /// The model's reply, markdown.
    Assistant,
    /// Reasoning, foldable.
    Reasoning,
    /// A tool call summary.
    Tool { name: String },
}

/// The status bar's content.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// Session mode id.
    pub mode: String,
    /// `provider/model`.
    pub model: String,
    /// Reasoning effort as configured.
    pub effort: String,
    /// Idle / thinking / failed.
    pub state: String,
}

impl Model {
    /// A model seeded from a hello/config round.
    #[must_use]
    pub fn new(model: String, effort: String, show_reasoning: bool) -> Self {
        Self {
            status: Status {
                mode: "chat".to_owned(),
                model,
                effort,
                state: "idle".to_owned(),
            },
            show_reasoning,
            ..Self::default()
        }
    }

    /// Appends one text delta to the streaming assistant block, creating it if needed.
    pub fn push_assistant_text(&mut self, text: &str) {
        self.append_to_last(BlockKind::Assistant, text);
    }

    /// Appends one reasoning delta; the block is what folds.
    pub fn push_reasoning(&mut self, text: &str) {
        self.append_to_last(BlockKind::Reasoning, text);
    }

    /// Echoes the user's own line.
    pub fn push_user(&mut self, text: &str) {
        let mut block = Block {
            kind: BlockKind::User,
            raw: text.to_owned(),
            lines: Vec::new(),
        };
        block.lines = crate::markdown::render(text);
        self.blocks.push(block);
        self.follow = true;
    }

    /// Records a tool call.
    pub fn push_tool(&mut self, name: &str) {
        self.blocks.push(Block {
            kind: BlockKind::Tool {
                name: name.to_owned(),
            },
            raw: String::new(),
            lines: vec![Line::from(Span::styled(
                format!("… {name}"),
                Style::new().fg(ratatui::style::Color::Yellow),
            ))],
        });
        self.follow = true;
    }

    fn append_to_last(&mut self, kind: BlockKind, text: &str) {
        if let Some(last) = self.blocks.last_mut()
            && last.kind == kind
        {
            last.raw.push_str(text);
            last.lines = crate::markdown::render(&last.raw);
            self.follow = true;
            return;
        }
        self.blocks.push(Block {
            kind: kind.clone(),
            raw: text.to_owned(),
            lines: crate::markdown::render(text),
        });
        self.follow = true;
    }

    /// How the reasoning block shows when folded.
    #[must_use]
    pub fn reasoning_summary(block: &Block) -> Line<'static> {
        Line::from(Span::styled(
            format!(
                "… thinking ({} chars) — Ctrl+R to expand",
                block.raw.chars().count()
            ),
            Style::new().italic().fg(ratatui::style::Color::DarkGray),
        ))
    }

    /// The scrollable message lines, honouring the fold.
    #[must_use]
    pub fn message_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for block in &self.blocks {
            let folded = matches!(block.kind, BlockKind::Reasoning) && !self.show_reasoning;
            if folded {
                lines.push(Model::reasoning_summary(block));
            } else {
                lines.extend(block.lines.iter().cloned());
            }
            lines.push(Line::from(""));
        }
        lines
    }

    /// Applies one protocol event; the only mutation path besides input handling.
    pub fn push_event(&mut self, event: &ServerEvent) {
        match event {
            ServerEvent::TextDelta { text, .. } => self.push_assistant_text(text),
            ServerEvent::ReasoningDelta { text, .. } => self.push_reasoning(text),
            ServerEvent::ToolCallStarted { summary, .. } => self.push_tool(&summary.title),
            ServerEvent::SessionUpdated { state } => {
                self.status.mode = state.mode.as_str().to_owned();
                self.status.model = format!("{}/{}", state.model.provider, state.model.model);
                if let Some(effort) = effort_of(state) {
                    self.status.effort = effort;
                }
                // The daemon knows what the session is doing; the bar mirrors it instead of
                // guessing "idle" while a whole turn streams past.
                self.status.state = match state.status {
                    hatchery_protocol::SessionStatus::Running => "thinking".to_owned(),
                    hatchery_protocol::SessionStatus::WaitingApproval => {
                        "awaiting approval".to_owned()
                    }
                    hatchery_protocol::SessionStatus::Idle => "idle".to_owned(),
                    hatchery_protocol::SessionStatus::Error => "failed".to_owned(),
                };
            }
            ServerEvent::TurnFinished { completion, .. } => {
                self.status.state = "idle".to_owned();
                if let Some(usage) = completion.usage.as_ref() {
                    self.notes.push(format!(
                        "turn finished: {} completion tokens",
                        usage.completion_tokens.unwrap_or(0)
                    ));
                }
            }
            ServerEvent::TurnFailed { .. } => self.status.state = "failed".to_owned(),
            ServerEvent::RateLimited { retry_after_ms } => {
                self.status.state = format!("rate limited ({}s)", retry_after_ms / 1000);
            }
            _ => {}
        }
    }
}

/// The effort string for the status bar, read out of the session's config patch.
fn effort_of(state: &hatchery_protocol::Session) -> Option<String> {
    state
        .config_patch
        .as_ref()?
        .get("reasoning_effort")?
        .as_str()
        .map(str::to_owned)
}

/// Draws the whole frame. Pure: same model, same buffer.
pub fn draw(model: &Model, frame: &mut Frame) {
    let area = frame.area();
    let [status, messages, notes, input] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(model.notes.len().min(3) as u16),
        Constraint::Length(input_height(model)),
    ])
    .areas(area);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {} ", model.status.mode),
                Style::new().bg(ratatui::style::Color::Blue),
            ),
            Span::raw(format!(
                " {}  effort:{}  {}",
                model.status.model, model.status.effort, model.status.state
            )),
        ])),
        status,
    );

    let lines = model.message_lines();
    let height = messages.height as usize;
    let offset = if model.follow && lines.len() > height {
        lines.len().saturating_sub(height)
    } else {
        0
    };
    frame.render_widget(Paragraph::new(lines).scroll((offset as u16, 0)), messages);

    if !model.notes.is_empty() {
        let shown: Vec<Line<'static>> = model
            .notes
            .iter()
            .rev()
            .take(3)
            .rev()
            .map(|note| {
                Line::from(Span::styled(
                    note.clone(),
                    Style::new().fg(ratatui::style::Color::Magenta),
                ))
            })
            .collect();
        frame.render_widget(Paragraph::new(shown), notes);
    }

    frame.render_widget(Paragraph::new(model.input.clone()), input);
}

/// The input area's height: one line plus however many newlines were typed, capped so a pasted
/// book cannot swallow the transcript.
fn input_height(model: &Model) -> u16 {
    let lines = model.input.split('\n').count() as u16;
    lines.clamp(1, 8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn frame_text(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    fn drawn(model: &Model, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(model, frame)).expect("draw");
        frame_text(&terminal)
    }

    #[test]
    fn the_status_bar_names_mode_model_effort_and_state() {
        let model = Model::new("deepseek/deepseek-chat".to_owned(), "high".to_owned(), true);
        let frame = drawn(&model, 60, 10);
        let status = frame.lines().next().expect("status line");
        assert!(status.contains("chat"), "{frame}");
        assert!(status.contains("deepseek/deepseek-chat"), "{frame}");
        assert!(status.contains("effort:high"), "{frame}");
    }

    #[test]
    fn user_and_assistant_blocks_render_markdown() {
        let mut model = Model::new("p/m".to_owned(), "medium".to_owned(), true);
        model.push_user("what is rust?");
        model.push_assistant_text("a **systems** language");
        let frame = drawn(&model, 60, 10);
        assert!(frame.contains("what is rust?"), "{frame}");
        assert!(frame.contains("systems"), "{frame}");
    }

    #[test]
    fn reasoning_folds_and_ctrl_r_expands() {
        let mut model = Model::new("p/m".to_owned(), "medium".to_owned(), false);
        model.push_reasoning("let me think about this carefully");
        let folded = drawn(&model, 60, 10);
        assert!(folded.contains("thinking (33 chars)"), "{folded}");
        assert!(!folded.contains("let me think"), "{folded}");

        model.show_reasoning = true;
        let expanded = drawn(&model, 60, 10);
        assert!(expanded.contains("let me think"), "{expanded}");
    }

    #[test]
    fn session_updates_move_the_status_bar() {
        let mut model = Model::new("old/p".to_owned(), "medium".to_owned(), true);
        model.push_event(&ServerEvent::SessionUpdated {
            state: fake_session("new-provider/new-model"),
        });
        assert_eq!(model.status.model, "new-provider/new-model");
    }

    #[test]
    fn a_turn_in_flight_shows_in_the_status_bar() {
        let mut model = Model::new("p/m".to_owned(), "medium".to_owned(), true);
        let mut state = fake_session("p/m");
        state.status = hatchery_protocol::SessionStatus::Running;
        model.push_event(&ServerEvent::SessionUpdated { state });
        assert_eq!(model.status.state, "thinking", "the bar mirrors the daemon");
    }

    #[test]
    fn a_multi_line_input_gets_its_own_room() {
        let mut model = Model::new("p/m".to_owned(), "medium".to_owned(), true);
        assert_eq!(input_height(&model), 1);
        model.input.push_str("first");
        model.input.push('\n');
        model.input.push_str("second");
        assert_eq!(input_height(&model), 2);
        let frame = drawn(&model, 60, 10);
        assert!(frame.contains("first"), "{frame}");
        assert!(frame.contains("second"), "{frame}");
    }

    #[test]
    fn reasoning_folds_across_turns_and_delta_kinds() {
        // Reasoning resumes as its own block after text starts (the kernel closes the reasoning
        // item when text begins) — the fold must cover the second block too.
        let mut model = Model::new("p/m".to_owned(), "medium".to_owned(), false);
        model.push_reasoning("first thought");
        model.push_assistant_text("the answer");
        model.push_reasoning("second thought");
        let frame = drawn(&model, 60, 12);
        assert!(frame.matches("thinking (").count() >= 2, "{frame}");
        assert!(!frame.contains("second thought"), "{frame}");
    }

    #[test]
    fn tool_calls_appear_as_summaries() {
        let mut model = Model::new("p/m".to_owned(), "medium".to_owned(), true);
        model.push_tool("read_file");
        let frame = drawn(&model, 60, 10);
        assert!(frame.contains("… read_file"), "{frame}");
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
}
