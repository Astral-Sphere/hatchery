//! The turn indicator: codex's status line above the composer, shown only while a turn lives.
//!
//! `⠋ working 12s · esc to interrupt` — the spinner and the elapsed seconds are why the TUI has
//! a tick at all; an idle session renders nothing here and gives the row back to the transcript.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::tui::Model;
use crate::tui::widgets::transcript::spinner;

/// One row while the session is busy, zero rows otherwise.
#[must_use]
pub fn height(model: &Model) -> u16 {
    u16::from(line(model).is_some())
}

/// The indicator line, when there is one.
#[must_use]
pub fn line(model: &Model) -> Option<Line<'static>> {
    let theme = &model.theme;
    let (label, style) = match model.status.state.as_str() {
        "thinking" => ("working", Style::new().fg(theme.accent)),
        "awaiting approval" => ("awaiting approval", Style::new().fg(theme.warn)),
        state if state.starts_with("rate limited") => (state, Style::new().fg(theme.warn)),
        _ => return None,
    };
    let seconds = model.turn_seconds();
    let mut spans = vec![
        Span::styled(
            format!("{} ", spinner(model.tick)),
            Style::new().fg(theme.accent),
        ),
        Span::styled(label.to_owned(), style),
    ];
    if seconds > 0 {
        spans.push(Span::styled(
            format!(" {seconds}s"),
            Style::new().fg(theme.dim),
        ));
    }
    spans.push(Span::styled(
        " · esc to interrupt",
        Style::new().fg(theme.faint),
    ));
    Some(Line::from(spans))
}

/// The indicator row.
pub struct Indicator<'a>(pub &'a Model);

impl Widget for Indicator<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if let Some(line) = line(self.0) {
            line.render(area, buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::{Theme, ThemeSetting};

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

    #[test]
    fn idle_renders_nothing_and_thinking_spins() {
        let idle = model();
        assert!(line(&idle).is_none());
        assert_eq!(height(&idle), 0);

        let mut busy = model();
        busy.push_event(&hatchery_protocol::ServerEvent::SessionUpdated {
            state: serde_json::from_value(serde_json::json!({
                "id": hatchery_protocol::SessionId::new(),
                "mode": "chat",
                "model": {"provider": "p", "model": "m"},
                "created_at": 0,
                "updated_at": 0,
                "generation": 0,
                "status": "running"
            }))
            .expect("session"),
        });
        let rendered = line(&busy).expect("a running turn shows");
        let text: String = rendered
            .spans
            .iter()
            .map(|span| span.content.to_string())
            .collect();
        assert!(text.contains("working"), "{text}");
        assert!(text.contains("esc to interrupt"), "{text}");
        assert_eq!(height(&busy), 1);

        for tick in 0..20 {
            busy.tick = tick;
            let seen: String = line(&busy)
                .expect("line")
                .spans
                .first()
                .expect("spinner span")
                .content
                .to_string();
            assert!(
                crate::tui::widgets::transcript::SPINNER_FRAMES.contains(&seen.trim_end()),
                "{seen:?} is a spinner frame"
            );
        }
    }

    #[test]
    fn elapsed_seconds_appear_after_the_first() {
        let mut busy = model();
        busy.status.state = "thinking".to_owned();
        busy.turn_started_tick = Some(0);
        busy.tick = 8; // four frames a second
        let text: String = line(&busy)
            .expect("line")
            .spans
            .iter()
            .map(|span| span.content.to_string())
            .collect();
        assert!(text.contains("2s"), "{text}");
    }
}
