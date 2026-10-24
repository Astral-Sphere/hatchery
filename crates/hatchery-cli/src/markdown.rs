//! Markdown to ratatui lines: the D5 decision (2026-09-30) in code, themed.
//!
//! minimad (termimad's parser, no rendering stack of its own) produces the parse tree; this
//! module maps it onto ratatui `Line`s through the [`Theme`] roles. ratatui's own wrapping is
//! deliberately not used here — the transcript wraps once, over whole cells, so scroll offsets
//! stay meaningful (see `tui::widgets::transcript`). Syntax highlighting of code blocks is an M2
//! concern (syntect); for now code renders dim behind a left bar.

use minimad::{Composite, CompositeStyle, Line, Options};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line as RtLine, Span};

use crate::tui::theme::Theme;

/// Parses markdown and renders it as ratatui lines, wrapped later by the transcript.
#[must_use]
pub fn render(md: &str, theme: &Theme) -> Vec<RtLine<'static>> {
    let text = minimad::parse_text(
        md,
        Options {
            keep_code_fences: true,
            ..Options::default()
        },
    );
    let mut out = Vec::new();
    for line in &text.lines {
        match line {
            Line::Normal(composite) => out.push(composite_line(composite, theme)),
            // With `keep_code_fences`, fence markers are empty lines and the fenced content
            // carries `CompositeStyle::Code`; the markers themselves render as a left bar.
            Line::CodeFence(_) => {}
            Line::HorizontalRule => {
                out.push(RtLine::styled("─".repeat(24), Style::new().fg(theme.faint)));
            }
            Line::TableRow(row) => out.push(table_row(row, theme)),
            Line::TableRule(row) => out.push(table_rule(row, theme)),
        }
    }
    out
}

/// A table row as a pipe-joined line: column widths would need the viewport width, and the
/// transcript's wrapping must stay width-independent per cell, so alignment is the M2 diff
/// view's job — here a table at least reads as a table instead of vanishing.
fn table_row(row: &minimad::TableRow<'_>, theme: &Theme) -> RtLine<'static> {
    let cells: Vec<String> = row
        .cells
        .iter()
        .map(|cell| {
            cell.compounds
                .iter()
                .map(|compound| compound.src.to_owned())
                .collect()
        })
        .collect();
    RtLine::from(Span::styled(cells.join(" │ "), Style::new().fg(theme.dim)))
}

/// The rule row under a table header, one dash group per column.
fn table_rule(row: &minimad::TableRule, theme: &Theme) -> RtLine<'static> {
    RtLine::from(Span::styled(
        vec!["───"; row.cells.len().max(1)].join(" "),
        Style::new().fg(theme.faint),
    ))
}

fn composite_line(composite: &Composite<'_>, theme: &Theme) -> RtLine<'static> {
    let base = base_style(composite.style, theme);
    let mut spans: Vec<Span<'static>> = Vec::new();
    match composite.style {
        CompositeStyle::ListItem(level) => {
            spans.push(Span::raw("  ".repeat(level as usize)));
            spans.push(Span::styled("• ", base));
        }
        CompositeStyle::OrderedListItem { level, index } => {
            spans.push(Span::raw("  ".repeat(level as usize)));
            spans.push(Span::styled(format!("{index}. "), base));
        }
        CompositeStyle::Code => spans.push(Span::styled("▎ ", Style::new().fg(theme.code))),
        CompositeStyle::Quote => spans.push(Span::styled("│ ", Style::new().fg(theme.faint))),
        CompositeStyle::Header(_) | CompositeStyle::Paragraph => {}
    }
    for compound in &composite.compounds {
        let mut style = base;
        if compound.code && !matches!(composite.style, CompositeStyle::Code) {
            style = style.fg(theme.accent);
        }
        if compound.bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if compound.italic {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if compound.strikeout {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        spans.push(Span::styled(compound.src.to_owned(), style));
    }
    RtLine::from(spans)
}

/// Block-level base styles: headings take the accent, code the code role, quotes dim italic.
fn base_style(style: CompositeStyle, theme: &Theme) -> Style {
    match style {
        CompositeStyle::Header(1) => Style::new().bold().fg(theme.accent),
        CompositeStyle::Header(_) => Style::new().bold().fg(theme.text),
        CompositeStyle::ListItem(_) | CompositeStyle::OrderedListItem { .. } => {
            Style::new().fg(theme.text)
        }
        CompositeStyle::Code => Style::new().fg(theme.code),
        CompositeStyle::Quote => Style::new().italic().fg(theme.dim),
        CompositeStyle::Paragraph => Style::new().fg(theme.text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    const SAMPLE: &str = "\
# Heading text
A paragraph with **bold**, *italic* and `inline code`, long enough that the narrow
test width forces the transcript to wrap it while keeping the spans styled.
- item one
```
fn main() {}
```
> a quote

| a | b |
|---|---|
| 1 | 2 |
";

    fn rendered(theme: &Theme) -> Vec<RtLine<'static>> {
        render(SAMPLE, theme)
    }

    #[test]
    fn blocks_and_inline_attributes_map_to_themed_spans() {
        let theme = Theme::dark();
        let lines = rendered(&theme);
        assert_eq!(
            lines[0].spans[0].style.fg,
            Some(theme.accent),
            "a heading takes the accent, never plain white: {:?}",
            lines[0]
        );
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        let paragraph = &lines[1];
        let bold = paragraph
            .spans
            .iter()
            .find(|span| span.content.contains("bold"))
            .expect("bold span");
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
        let code = paragraph
            .spans
            .iter()
            .find(|span| span.content.contains("inline code"))
            .expect("code span");
        assert_eq!(code.style.fg, Some(theme.accent));
        assert!(
            lines
                .iter()
                .any(|line| line.spans.iter().any(|span| span.content == "• "))
        );
        // The fenced block renders dim behind its bar, its markers not at all.
        let fence = lines
            .iter()
            .find(|line| {
                line.spans
                    .iter()
                    .any(|span| span.content.contains("fn main()"))
            })
            .expect("fence content");
        assert_eq!(fence.spans[0].content, "▎ ");
        assert_eq!(fence.spans[1].style.fg, Some(theme.code));
    }

    #[test]
    fn tables_render_as_rows_instead_of_vanishing() {
        let lines = rendered(&Theme::light());
        let header = lines
            .iter()
            .find(|line| line.spans.iter().any(|span| span.content.contains("a │ b")))
            .expect("header row");
        assert!(header.spans[0].content.contains("a │ b"));
        let rule = lines
            .iter()
            .find(|line| line.spans.iter().any(|span| span.content.contains("───")))
            .expect("rule row");
        assert!(
            rule.spans[0].content.matches("───").count() == 2,
            "{rule:?}"
        );
        let body = lines
            .iter()
            .find(|line| line.spans.iter().any(|span| span.content.contains("1 │ 2")))
            .expect("body row");
        assert!(body.spans[0].content.contains("1 │ 2"));
    }

    #[test]
    fn the_two_themes_disagree_on_the_heading_colour() {
        let dark = rendered(&Theme::dark());
        let light = rendered(&Theme::light());
        assert_ne!(dark[0].spans[0].style.fg, light[0].spans[0].style.fg);
        assert!(matches!(
            dark[0].spans[0].style.fg,
            Some(Color::Rgb(_, _, _))
        ));
    }
}
