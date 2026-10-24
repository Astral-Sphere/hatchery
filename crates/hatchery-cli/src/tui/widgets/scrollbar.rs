//! The transcript's scrollbar: a one-column proportional rail in the gutter.
//!
//! The gutter column is reserved whether or not the bar shows (the transcript wraps one
//! column narrower than the terminal), so the bar never overlays text and the wrap cache
//! never reflows when overflow comes and goes. The geometry follows qwen-code's
//! `VirtualizedList`: the thumb spans `track² / total` rows (at least one), sits at
//! `offset / max · (track − thumb)`, and a press or drag on the track maps its row back
//! through the same proportion. Unlike qwen-code's auto-hiding flash the bar stays visible
//! whenever anything overflows: a permanent rail is the affordance that says there is
//! history above the window.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::tui::theme::Theme;

/// The thumb's top row and length in track rows, or `None` when nothing overflows.
#[must_use]
pub fn thumb(total: usize, height: usize, offset: usize) -> Option<(usize, usize)> {
    let max = total.checked_sub(height)?;
    if max == 0 {
        return None;
    }
    let len = ((height * height * 2 + total) / (2 * total)).clamp(1, height);
    let top = (offset.saturating_mul(height - len) + max / 2) / max;
    Some((top.min(height - len), len))
}

/// The scroll offset a track row points at; the caller turns `>= max` into follow, so
/// dragging the thumb to the bottom row lands on the sticky tail.
#[must_use]
pub fn offset_for_row(row: usize, total: usize, height: usize) -> usize {
    let max = total.saturating_sub(height);
    let track = height.saturating_sub(1).max(1);
    (row.min(height.saturating_sub(1)) * max + track / 2) / track
}

/// The proportional rail: a `█` thumb over a `│` track, blank while the content fits.
pub struct Scrollbar<'a> {
    /// Wrapped transcript lines in total.
    pub total: usize,
    /// The resolved window top.
    pub offset: usize,
    /// Palette for the two roles.
    pub theme: &'a Theme,
}

impl Widget for Scrollbar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let height = area.height as usize;
        let Some((top, len)) = thumb(self.total, height, self.offset) else {
            return;
        };
        let lines: Vec<Line<'static>> = (0..height)
            .map(|row| {
                let (glyph, color) = if (top..top + len).contains(&row) {
                    ("█", self.theme.dim)
                } else {
                    ("│", self.theme.faint)
                };
                Line::from(Span::styled(glyph, Style::new().fg(color)))
            })
            .collect();
        Paragraph::new(lines).render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_that_fits_has_no_thumb() {
        assert_eq!(thumb(10, 20, 0), None);
        assert_eq!(thumb(20, 20, 0), None, "exactly full is not overflow");
    }

    #[test]
    fn the_thumb_spans_the_viewport_share_of_the_transcript() {
        // Forty lines in a ten-row window: the thumb is a quarter of the track.
        assert_eq!(thumb(40, 10, 0), Some((0, 3)));
        assert_eq!(thumb(40, 10, 15), Some((4, 3)));
        assert_eq!(
            thumb(40, 10, 30),
            Some((7, 3)),
            "the tail pins the thumb bottom"
        );
    }

    #[test]
    fn a_transcript_barely_longer_than_the_window_still_shows_a_thumb() {
        assert_eq!(thumb(11, 10, 0), Some((0, 9)));
        assert_eq!(thumb(11, 10, 1), Some((1, 9)));
    }

    #[test]
    fn track_rows_map_back_through_the_same_proportion() {
        // A hundred lines in ten rows: max offset 90 over nine track gaps.
        assert_eq!(offset_for_row(0, 100, 10), 0);
        assert_eq!(offset_for_row(4, 100, 10), 40);
        assert_eq!(offset_for_row(9, 100, 10), 90, "the last row is the tail");
        assert_eq!(offset_for_row(99, 100, 10), 90, "rows clamp into the track");
    }

    #[test]
    fn the_bar_draws_thumb_over_track_and_stays_blank_when_it_fits() {
        let theme = Theme::dark();
        let area = Rect::new(0, 0, 1, 10);
        let mut buf = Buffer::empty(area);
        Scrollbar {
            total: 40,
            offset: 0,
            theme: &theme,
        }
        .render(area, &mut buf);
        assert_eq!(buf[(0, 0)].symbol(), "█");
        assert_eq!(buf[(0, 2)].symbol(), "█");
        assert_eq!(buf[(0, 3)].symbol(), "│");
        assert_eq!(buf[(0, 9)].symbol(), "│");

        let mut blank = Buffer::empty(area);
        Scrollbar {
            total: 5,
            offset: 0,
            theme: &theme,
        }
        .render(area, &mut blank);
        assert_eq!(blank[(0, 0)].symbol(), " ");
    }
}
