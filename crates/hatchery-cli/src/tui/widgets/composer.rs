//! The composer: a rounded box the input lives in, with a prompt glyph and a placeholder.
//!
//! qwen-code's composer is a ruled band, codex's a bordered box with a hint row; this takes the
//! box (the input reads as a place, not as stray text at the bottom of the screen) and puts the
//! key hints on their own dim line below, where they never fight the typed text.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};

use crate::tui::Model;

/// The key hints, one dim line under the box.
pub const HINTS: &str = "enter send · alt+enter newline · ctrl+r reasoning · pgup/pgdn/wheel scroll · ctrl+↑/↓ jump · end follow · esc interrupt · ctrl+c quit";

/// The composer box over the model's input.
pub struct Composer<'a>(pub &'a Model);

/// The box height for the current input: one line per typed newline, capped so a pasted book
/// cannot swallow the transcript, plus the two border rows.
#[must_use]
pub fn height(model: &Model) -> u16 {
    model.input.split('\n').count().clamp(1, 6) as u16 + 2
}

impl Widget for Composer<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let theme = &self.0.theme;
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(theme.faint))
            .border_type(ratatui::widgets::BorderType::Rounded);
        let inner = block.inner(area);
        block.render(area, buf);

        let mut lines: Vec<Line<'static>> = Vec::new();
        if self.0.input.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("> ", Style::new().fg(theme.accent).bold()),
                Span::styled(
                    "type a message — / lists commands",
                    Style::new().fg(theme.faint).italic(),
                ),
            ]));
        } else {
            for (index, line) in self.0.input.split('\n').enumerate() {
                let mut spans = Vec::new();
                if index == 0 {
                    spans.push(Span::styled("> ", Style::new().fg(theme.accent).bold()));
                }
                spans.push(Span::styled(line.to_owned(), Style::new().fg(theme.text)));
                lines.push(Line::from(spans));
            }
        }
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(inner, buf);
    }
}

/// Where the cursor sits for the current input, or `None` when the box is not on screen.
#[must_use]
pub fn cursor(model: &Model, area: Rect) -> Option<(u16, u16)> {
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    let rows: Vec<&str> = model.input.split('\n').collect();
    let row = rows.len().saturating_sub(1).min(inner.height as usize - 1);
    let prompt = if row == 0 { 2 } else { 0 };
    let column = prompt + unicode_width::UnicodeWidthStr::width(rows[row]);
    Some((
        inner.x
            + u16::try_from(column)
                .unwrap_or(inner.width - 1)
                .min(inner.width - 1),
        inner.y + u16::try_from(row).ok()?,
    ))
}

/// The dim hint line under the box.
pub struct Hints<'a>(pub &'a Model);

impl Widget for Hints<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Line::from(Span::styled(HINTS, Style::new().fg(self.0.theme.faint))).render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::{Theme, ThemeSetting};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn model() -> Model {
        Model::new(
            "p/m".to_owned(),
            "high".to_owned(),
            true,
            ThemeSetting::Dark,
            Theme::dark(),
            None,
            "/tmp".to_owned(),
        )
    }

    fn drawn(model: &Model, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                let area = frame.area();
                frame.render_widget(Composer(model), area);
            })
            .expect("draw");
        terminal.backend().to_string()
    }

    #[test]
    fn an_empty_composer_shows_the_placeholder_inside_a_box() {
        let frame = drawn(&model(), 40, 3);
        assert!(frame.contains("╭"), "{frame}");
        assert!(frame.contains("type a message"), "{frame}");
        assert!(frame.contains(">"), "{frame}");
    }

    #[test]
    fn typed_text_replaces_the_placeholder() {
        let mut model = model();
        model.input.push_str("hello");
        let frame = drawn(&model, 40, 3);
        assert!(frame.contains("> hello"), "{frame}");
        assert!(!frame.contains("type a message"), "{frame}");
    }

    #[test]
    fn the_height_grows_with_newlines_and_caps() {
        let mut model = model();
        assert_eq!(height(&model), 3, "one row plus borders");
        model.input.push_str("a\nb\nc");
        assert_eq!(height(&model), 5);
        model.input.push_str("\n\n\n\n\n\n");
        assert_eq!(height(&model), 8, "six rows plus borders, capped");
    }

    #[test]
    fn the_cursor_tracks_the_end_of_the_input() {
        let mut model = model();
        model.input.push_str("hi");
        let area = Rect::new(0, 0, 40, height(&model));
        let (x, y) = cursor(&model, area).expect("cursor");
        assert_eq!((x, y), (1 + 2 + 2, 1), "border + prompt + two chars");
        model.input.push('\n');
        let area = Rect::new(0, 0, 40, height(&model));
        let (x, y) = cursor(&model, area).expect("cursor");
        assert_eq!((x, y), (1, 2), "a fresh row starts after the border");
    }
}
