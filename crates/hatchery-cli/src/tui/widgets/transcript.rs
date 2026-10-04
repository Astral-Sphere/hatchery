//! The transcript: history cells rendered to lines, and the wrapping that makes them scrollable.
//!
//! Cell shapes follow the reference vocabulary: a user turn hangs behind an accent `>`, an
//! assistant turn behind an accent `◆` (qwen-code's glyphs), reasoning folds to a dim italic
//! `∴ Thought for …` line, and a tool call is a left-barred cell whose glyph tracks its
//! lifecycle (`⊷`-style spinner while running, `✓`/`✗` once the item finishes) — codex's exec
//! cell, scaled to a bar so a cell's line count stays width-independent and therefore
//! cacheable. Wrapping is ours rather than `Paragraph`'s because scroll offsets and message
//! jumps need to know where every wrapped line landed.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use unicode_width::UnicodeWidthChar;

use crate::tui::theme::Theme;
use crate::tui::{Cell, CellKind, Model};
use hatchery_protocol::ToolStatus;

/// The braille wheel, eight frames at four frames a second.
pub const SPINNER_FRAMES: [&str; 8] = ["⠋", "⠙", "", "", "", "⠴", "⠦", "⠧"];

/// The spinner frame for a tick count.
#[must_use]
pub fn spinner(tick: u64) -> &'static str {
    SPINNER_FRAMES[(tick / 2) as usize % SPINNER_FRAMES.len()]
}

/// Renders one cell to its unwrapped lines; wrapping happens once for the whole transcript.
#[must_use]
pub fn render_cell(
    cell: &Cell,
    theme: &Theme,
    show_reasoning: bool,
    tick: u64,
) -> Vec<Line<'static>> {
    match cell.kind {
        CellKind::Banner => vec![
            Line::from(Span::styled(cell.raw.clone(), Style::new().fg(theme.faint))),
            Line::from(Span::styled("─".repeat(28), Style::new().fg(theme.faint))),
        ],
        CellKind::User => cell
            .raw
            .split('\n')
            .enumerate()
            .map(|(index, line)| {
                let mut spans = Vec::new();
                if index == 0 {
                    spans.push(Span::styled("> ", Style::new().fg(theme.accent).bold()));
                }
                spans.push(Span::styled(line.to_owned(), Style::new().fg(theme.text)));
                Line::from(spans)
            })
            .collect(),
        CellKind::Assistant => {
            let mut lines = crate::markdown::render(&cell.raw, theme);
            let glyph = Span::styled("◆ ", Style::new().fg(theme.accent));
            if lines.is_empty() {
                lines.push(Line::from(glyph));
            } else {
                lines[0].spans.insert(0, glyph);
            }
            lines
        }
        CellKind::Reasoning => {
            if !show_reasoning {
                let chars = cell.raw.chars().count();
                return vec![Line::from(Span::styled(
                    format!("∴ Thought for {chars} chars (Ctrl+R to expand)"),
                    Style::new().fg(theme.faint).italic(),
                ))];
            }
            cell.raw
                .split('\n')
                .enumerate()
                .map(|(index, line)| {
                    let mut spans = Vec::new();
                    if index == 0 {
                        spans.push(Span::styled("∴ ", Style::new().fg(theme.faint).italic()));
                    }
                    spans.push(Span::styled(
                        line.to_owned(),
                        Style::new().fg(theme.thinking).italic(),
                    ));
                    Line::from(spans)
                })
                .collect()
        }
        CellKind::Tool => {
            let Some(tool) = cell.tool.as_ref() else {
                return Vec::new();
            };
            let (glyph, color) = tool_glyph(tool.status, tick, theme);
            let mut spans = vec![
                Span::styled(format!("{glyph} "), Style::new().fg(color)),
                Span::styled(
                    tool.name.clone().unwrap_or_else(|| tool.title.clone()),
                    Style::new().fg(theme.tool).bold(),
                ),
            ];
            if tool.name.is_some() {
                spans.push(Span::styled(
                    format!(" · {}", tool.title),
                    Style::new().fg(theme.dim),
                ));
            }
            let mut lines = vec![Line::from(spans)];
            let bar = Style::new().fg(theme.code);
            if let Some(detail) = tool.detail.as_ref().filter(|detail| !detail.is_empty()) {
                lines.push(Line::from(vec![
                    Span::styled("▎ ", bar),
                    Span::styled(detail.clone(), Style::new().fg(theme.dim)),
                ]));
            }
            if !tool.tail.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("▎ ", bar),
                    Span::styled(tool.tail.clone(), Style::new().fg(theme.faint)),
                ]));
            }
            lines
        }
    }
}

/// The lifecycle glyph and its theme role: qwen-code's fixed-width status column, codex's
/// spinner while the call is still in flight.
fn tool_glyph(
    status: ToolStatus,
    tick: u64,
    theme: &Theme,
) -> (&'static str, ratatui::style::Color) {
    match status {
        ToolStatus::Running => (spinner(tick), theme.accent),
        ToolStatus::Completed => ("✓", theme.success),
        ToolStatus::Failed => ("✗", theme.error),
        ToolStatus::Denied => ("−", theme.warn),
        ToolStatus::Cancelled => ("·", theme.dim),
        ToolStatus::Pending => ("…", theme.dim),
    }
}

/// Wraps one line at `width` columns, word-breaking at spaces and hard-breaking anything wider
/// than the whole width. The space a break falls on is dropped, as `Wrap { trim: true }` would.
#[must_use]
pub fn wrap_line(line: &Line<'_>, width: usize) -> Vec<Line<'static>> {
    let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut used = 0usize;
    for span in &line.spans {
        let mut current = String::new();
        for ch in span.content.chars() {
            let columns = UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + columns > width && used > 0 {
                // The space a break falls on is dropped, as `Wrap { trim: true }` would.
                let trimmed = current.trim_end();
                if !trimmed.is_empty() {
                    rows.last_mut()
                        .expect("rows starts with one row")
                        .push(Span::styled(trimmed.to_owned(), span.style));
                }
                current.clear();
                rows.push(Vec::new());
                used = 0;
                if ch == ' ' {
                    continue;
                }
            }
            current.push(ch);
            used += columns;
        }
        if !current.is_empty() {
            rows.last_mut()
                .expect("rows starts with one row")
                .push(Span::styled(current, span.style));
        }
    }
    rows.into_iter().map(Line::from).collect()
}

/// Wraps every line, keeping empty lines as single empty rows.
#[must_use]
pub fn wrap_lines(lines: &[Line<'static>], width: usize) -> Vec<Line<'static>> {
    lines
        .iter()
        .flat_map(|line| wrap_line(line, width))
        .collect()
}

/// The transcript window: the wrapped lines from `offset`, clipped to the area.
pub struct Transcript<'a> {
    /// The model to project.
    pub model: &'a Model,
    /// First wrapped line shown.
    pub offset: usize,
}

impl Widget for Transcript<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let window: Vec<Line<'static>> = self
            .model
            .wrapped_lines()
            .iter()
            .skip(self.offset)
            .take(area.height as usize)
            .cloned()
            .collect();
        Paragraph::new(window).render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn line(text: &str) -> Line<'static> {
        Line::from(text.to_owned())
    }

    #[test]
    fn wrapping_breaks_at_spaces_and_drops_the_seam_space() {
        let wrapped = wrap_line(&line("hello wide world"), 11);
        let texts: Vec<String> = wrapped
            .iter()
            .map(|row| row.spans.iter().map(|span| span.content.as_ref()).collect())
            .collect();
        assert_eq!(texts, vec!["hello wide", "world"], "{texts:?}");
    }

    #[test]
    fn wrapping_hard_breaks_words_wider_than_the_width() {
        let wrapped = wrap_line(&line("abcdefghij"), 4);
        let texts: Vec<String> = wrapped
            .iter()
            .map(|row| row.spans.iter().map(|span| span.content.as_ref()).collect())
            .collect();
        assert_eq!(texts, vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn wrapping_counts_wide_characters_as_two_columns() {
        // Four CJK columns fill a width-4 row exactly; the fifth needs a row of its own.
        let wrapped = wrap_line(&line("你好世界呀"), 4);
        assert_eq!(wrapped.len(), 3, "{wrapped:?}");
        let first: String = wrapped[0]
            .spans
            .iter()
            .map(|span| span.content.to_string())
            .collect();
        assert_eq!(first, "你好");
    }

    #[test]
    fn wrapping_keeps_indentation_and_span_styles() {
        let styled = Line::from(vec![
            Span::styled("    indented ", Style::new().fg(Color::Red)),
            Span::styled("code line", Style::new().fg(Color::Blue)),
        ]);
        let wrapped = wrap_line(&styled, 14);
        assert!(
            wrapped[0].spans[0].content.starts_with("    "),
            "indent survives"
        );
        assert_eq!(wrapped[0].spans[0].style.fg, Some(Color::Red));
    }

    #[test]
    fn empty_lines_stay_one_row() {
        assert_eq!(wrap_lines(&[Line::from("")], 40).len(), 1);
    }

    #[test]
    fn the_spinner_cycles_through_every_frame() {
        let seen: Vec<&str> = (0..16).map(spinner).collect();
        for frame in SPINNER_FRAMES {
            assert!(seen.contains(&frame), "{frame} never shows");
        }
    }
}
