//! The status line: one row at the very bottom, where both references keep it.
//!
//! `mode · model · effort · state` on the left, the scroll position on the right while the
//! window is off the tail. The state segment carries the colour: idle is dim, a running turn is
//! the accent, waits and rate limits warn, failures are red — the line is glanceable without
//! reading it.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::tui::Model;
use crate::tui::widgets::transcript::spinner;

/// The bottom status line; `offset` is the transcript window's top, when it is off the tail.
pub struct StatusBar<'a> {
    /// The model to project.
    pub model: &'a Model,
    /// The window top, or `None` while following.
    pub offset: Option<usize>,
}

impl Widget for StatusBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let model = self.model;
        let theme = &model.theme;
        let mut spans = vec![
            Span::styled(
                model.status.mode.clone(),
                Style::new().fg(theme.accent).bold(),
            ),
            Span::styled(" · ", Style::new().fg(theme.faint)),
            Span::styled(model.status.model.clone(), Style::new().fg(theme.text)),
            Span::styled(" · effort ", Style::new().fg(theme.faint)),
            Span::styled(model.status.effort.clone(), Style::new().fg(theme.text)),
            Span::styled(" · ", Style::new().fg(theme.faint)),
            state_span(model),
        ];
        let left_width = Line::from(spans.clone()).width();
        if let Some(offset) = self.offset {
            let right = Span::styled(
                format!("↑ {}/{} · end follows", offset, model.wrapped_lines().len()),
                Style::new().fg(theme.faint),
            );
            let taken = left_width + right.width();
            if taken < area.width as usize {
                spans.push(Span::raw(" ".repeat(area.width as usize - taken)));
                spans.push(right);
            }
        }
        Line::from(spans).render(area, buf);
    }
}

/// The state segment, coloured by what the daemon says the session is doing.
fn state_span(model: &Model) -> Span<'static> {
    let theme = &model.theme;
    let state = model.status.state.as_str();
    let (text, style) = if state == "idle" {
        (state.to_owned(), Style::new().fg(theme.faint))
    } else if state == "failed" {
        (state.to_owned(), Style::new().fg(theme.error).bold())
    } else if state == "awaiting approval" || state.starts_with("rate limited") {
        (state.to_owned(), Style::new().fg(theme.warn))
    } else {
        (
            format!("{} {state}", spinner(model.tick)),
            Style::new().fg(theme.accent),
        )
    };
    Span::styled(text, style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::{Theme, ThemeSetting};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn model() -> Model {
        Model::new(
            "deepseek/deepseek-flash".to_owned(),
            "high".to_owned(),
            true,
            ThemeSetting::Dark,
            Theme::dark(),
            None,
            "/tmp".to_owned(),
        )
    }

    fn drawn(model: &Model, offset: Option<usize>) -> String {
        let backend = TestBackend::new(90, 1);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                frame.render_widget(StatusBar { model, offset }, frame.area());
            })
            .expect("draw");
        terminal.backend().to_string()
    }

    #[test]
    fn the_line_names_mode_model_effort_and_state() {
        let frame = drawn(&model(), None);
        assert!(frame.contains("chat"), "{frame}");
        assert!(frame.contains("deepseek/deepseek-flash"), "{frame}");
        assert!(frame.contains("effort high"), "{frame}");
        assert!(frame.contains("idle"), "{frame}");
    }

    #[test]
    fn a_scrolled_transcript_shows_its_position_on_the_right() {
        let mut model = model();
        model.push_user("one");
        model.push_assistant_text("two");
        model.relayout(40);
        let frame = drawn(&model, Some(0));
        assert!(frame.contains("end follows"), "{frame}");
        assert!(frame.contains("↑ 0/"), "{frame}");
    }

    #[test]
    fn following_hides_the_position() {
        let frame = drawn(&model(), None);
        assert!(!frame.contains("end follows"), "{frame}");
    }

    #[test]
    fn a_running_turn_spins_in_the_state_segment() {
        let mut model = model();
        model.status.state = "thinking".to_owned();
        let frame = drawn(&model, None);
        assert!(frame.contains("thinking"), "{frame}");
        assert!(
            crate::tui::widgets::transcript::SPINNER_FRAMES
                .iter()
                .any(|glyph| frame.contains(glyph)),
            "{frame}"
        );
    }
}
