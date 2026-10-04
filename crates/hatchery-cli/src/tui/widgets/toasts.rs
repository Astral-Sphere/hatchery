//! Toasts: transient notes above the indicator, themed and self-expiring.
//!
//! The old notes area printed bare magenta lines that never went away; a note is news, so it
//! gets an icon by kind, the theme's colour for it, and a tick-based expiry — five seconds at
//! four ticks a second. At most three are shown, newest last.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::tui::theme::Theme;
use crate::tui::{Model, Toast, ToastKind};

/// Ticks a toast lives (five seconds at the 250 ms tick).
pub const TOAST_TICKS: u64 = 20;

/// The rows currently on screen, oldest first.
#[must_use]
pub fn visible(model: &Model) -> Vec<&Toast> {
    let live: Vec<&Toast> = model
        .toasts
        .iter()
        .filter(|toast| model.tick.saturating_sub(toast.born) < TOAST_TICKS)
        .collect();
    live.into_iter().rev().take(3).rev().collect()
}

/// The toast rows' height, for the frame layout.
#[must_use]
pub fn height(model: &Model) -> u16 {
    u16::try_from(visible(model).len()).unwrap_or(0)
}

/// The toast rows.
pub struct Toasts<'a>(pub &'a Model);

impl Widget for Toasts<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let theme = &self.0.theme;
        let lines: Vec<Line<'static>> = visible(self.0)
            .into_iter()
            .map(|toast| {
                Line::from(vec![
                    icon(toast.kind, theme),
                    Span::styled(toast.text.clone(), body_style(toast.kind, theme)),
                ])
            })
            .collect();
        Paragraph::new(lines).render(area, buf);
    }
}

fn icon(kind: ToastKind, theme: &Theme) -> Span<'static> {
    match kind {
        ToastKind::Ok => Span::styled("✓ ", Style::new().fg(theme.success)),
        ToastKind::Error => Span::styled("! ", Style::new().fg(theme.error).bold()),
        ToastKind::Info => Span::styled("· ", Style::new().fg(theme.dim)),
    }
}

fn body_style(kind: ToastKind, theme: &Theme) -> Style {
    match kind {
        ToastKind::Ok => Style::new().fg(theme.dim),
        ToastKind::Error => Style::new().fg(theme.error),
        ToastKind::Info => Style::new().fg(theme.dim),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::ThemeSetting;

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
    fn toasts_expire_and_cap_at_three() {
        let mut model = model();
        model.note("old news", ToastKind::Info);
        model.tick = TOAST_TICKS;
        assert!(visible(&model).is_empty(), "five seconds old is gone");

        model.tick = 0;
        for index in 0..5 {
            model.note(format!("note {index}"), ToastKind::Info);
        }
        let shown: Vec<String> = visible(&model)
            .iter()
            .map(|toast| toast.text.clone())
            .collect();
        assert_eq!(shown, vec!["note 2", "note 3", "note 4"], "newest three");
        assert_eq!(height(&model), 3);
    }

    #[test]
    fn kinds_get_their_icons() {
        let mut model = model();
        model.note("saved", ToastKind::Ok);
        model.note("boom", ToastKind::Error);
        let lines: Vec<String> = visible(&model)
            .iter()
            .map(|toast| match toast.kind {
                ToastKind::Ok => "ok".to_owned(),
                ToastKind::Error => "error".to_owned(),
                ToastKind::Info => "info".to_owned(),
            })
            .collect();
        assert_eq!(lines, vec!["ok", "error"]);
    }
}
