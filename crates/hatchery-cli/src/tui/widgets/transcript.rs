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
            // A click sets the cell's own override; Ctrl+R's global fold is the default.
            if !cell.expanded.unwrap_or(show_reasoning) {
                let chars = cell.raw.chars().count();
                return vec![Line::from(Span::styled(
                    format!("∴ Thought for {chars} chars (click or Ctrl+R to expand)"),
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
            let mut body = Vec::new();
            if let Some(detail) = tool.detail.as_ref().filter(|detail| !detail.is_empty()) {
                body.push(Line::from(vec![
                    Span::styled("▎ ", bar),
                    Span::styled(detail.clone(), Style::new().fg(theme.dim)),
                ]));
            }
            if !tool.tail.is_empty() {
                body.push(Line::from(vec![
                    Span::styled("▎ ", bar),
                    Span::styled(tool.tail.clone(), Style::new().fg(theme.faint)),
                ]));
            }
            // Folded by a click: the header stays, the body becomes an ellipsis.
            if !cell.expanded.unwrap_or(true) {
                if !body.is_empty() {
                    lines[0]
                        .spans
                        .push(Span::styled(" · ⋯", Style::new().fg(theme.faint)));
                }
                return lines;
            }
            lines.extend(body);
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

/// Wraps one line at `width` columns: whole words move to the next row, wide characters
/// break between each other (CJK has a break opportunity between every pair), and only a
/// word wider than the whole width is split. The space a break falls on is dropped, as
/// `Wrap { trim: true }` would.
#[must_use]
pub fn wrap_line(line: &Line<'_>, width: usize) -> Vec<Line<'static>> {
    let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut used = 0usize;
    let mut at_start = true;
    for atom in atoms(line) {
        if atom.space {
            // Indentation at the very start of the line survives; spaces after a break do not.
            if used == 0 && !at_start {
                continue;
            }
            rows.last_mut()
                .expect("rows starts with one row")
                .push(Span::styled(atom.text, atom.style));
            used += atom.width;
            at_start = false;
            continue;
        }
        let mut word = atom.text.as_str();
        let mut word_width = atom.width;
        loop {
            if used > 0 && used + word_width > width {
                trim_trailing_spaces(&mut rows, &mut used);
                rows.push(Vec::new());
                used = 0;
            }
            if word_width <= width {
                rows.last_mut()
                    .expect("rows starts with one row")
                    .push(Span::styled(word.to_owned(), atom.style));
                used += word_width;
                at_start = false;
                break;
            }
            // A word wider than the whole width is the one thing that gets split.
            let (head, tail) = split_at_width(word, width);
            rows.last_mut()
                .expect("rows starts with one row")
                .push(Span::styled(head.to_owned(), atom.style));
            rows.push(Vec::new());
            used = 0;
            at_start = false;
            word = tail;
            word_width = unicode_width::UnicodeWidthStr::width(tail);
            if tail.is_empty() {
                break;
            }
        }
    }
    trim_trailing_spaces(&mut rows, &mut used);
    rows.into_iter().map(Line::from).collect()
}

/// How a character joins its neighbours: spaces group, wide characters stand alone (every
/// pair is a break opportunity), narrow characters glue into words.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Space,
    Wide,
    Narrow,
}

fn kind_of(ch: char) -> Kind {
    if ch == ' ' {
        Kind::Space
    } else if UnicodeWidthChar::width(ch).unwrap_or(0) >= 2 {
        Kind::Wide
    } else {
        Kind::Narrow
    }
}

/// One style-carrying unit of wrapping: a word, a single wide character, or a run of spaces.
struct Atom {
    text: String,
    style: Style,
    width: usize,
    space: bool,
}

fn atoms(line: &Line<'_>) -> Vec<Atom> {
    let mut out = Vec::new();
    for span in &line.spans {
        let mut text = String::new();
        let mut width = 0usize;
        let mut kind = Kind::Narrow;
        let mut started = false;
        for ch in span.content.chars() {
            let next = kind_of(ch);
            if started && (next != kind || next == Kind::Wide) {
                out.push(Atom {
                    text: std::mem::take(&mut text),
                    style: span.style,
                    width,
                    space: kind == Kind::Space,
                });
                width = 0;
            }
            text.push(ch);
            width += UnicodeWidthChar::width(ch).unwrap_or(0);
            kind = next;
            started = true;
        }
        if started {
            out.push(Atom {
                text,
                style: span.style,
                width,
                space: kind == Kind::Space,
            });
        }
    }
    out
}

/// Splits off the longest prefix that fits in `width` columns.
fn split_at_width(text: &str, width: usize) -> (&str, &str) {
    let mut used = 0usize;
    for (index, ch) in text.char_indices() {
        let columns = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + columns > width {
            return (&text[..index], &text[index..]);
        }
        used += columns;
    }
    (text, "")
}

/// Drops the space atoms a row ends with; they belong to neither side of a break.
fn trim_trailing_spaces(rows: &mut Vec<Vec<Span<'static>>>, used: &mut usize) {
    let row = rows.last_mut().expect("rows starts with one row");
    while row
        .last()
        .is_some_and(|span| !span.content.is_empty() && span.content.chars().all(|ch| ch == ' '))
    {
        let span = row.pop().expect("just checked");
        *used = used.saturating_sub(unicode_width::UnicodeWidthStr::width(span.content.as_ref()));
    }
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
    fn words_move_whole_to_the_next_row() {
        let wrapped = wrap_line(&line("alpha beta gamma"), 11);
        let texts: Vec<String> = wrapped
            .iter()
            .map(|row| row.spans.iter().map(|span| span.content.as_ref()).collect())
            .collect();
        assert_eq!(texts, vec!["alpha beta", "gamma"], "{texts:?}");
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
