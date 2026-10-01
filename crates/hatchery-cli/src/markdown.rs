//! Markdown to ratatui lines: the D5 decision (2026-09-30) in code.
//!
//! minimad (termimad's parser, no rendering stack of its own) produces the parse tree; this
//! module maps it onto ratatui `Line`s with a fixed skin. ratatui's `Paragraph` wrapping keeps
//! span styles across line breaks — the spike verified that cell-by-cell — so wrapping stays
//! delegated and no line-width logic lives here. Syntax highlighting of code blocks is an M2
//! concern (syntect); for now code renders dim.

use minimad::{Composite, CompositeStyle, Line, Options};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as RtLine, Span};

/// The skin: block-level base styles and the two bullets.
pub struct Skin;

impl Skin {
    #[must_use]
    pub fn composite(&self, style: CompositeStyle) -> Style {
        match style {
            CompositeStyle::Header(depth) => Style::new().bold().fg(match depth {
                1 => Color::LightBlue,
                _ => Color::White,
            }),
            CompositeStyle::ListItem(_) | CompositeStyle::OrderedListItem { .. } => {
                Style::new().fg(Color::LightGreen)
            }
            CompositeStyle::Code => Style::new().fg(Color::DarkGray),
            CompositeStyle::Quote => Style::new().italic().fg(Color::Gray),
            CompositeStyle::Paragraph => Style::new(),
        }
    }

    #[must_use]
    pub fn code_block(&self) -> Style {
        Style::new().fg(Color::DarkGray)
    }

    #[must_use]
    pub fn horizontal_rule(&self) -> Style {
        Style::new().fg(Color::DarkGray)
    }

    #[must_use]
    pub fn bullet(&self) -> &'static str {
        "• "
    }
}

/// Parses markdown and renders it as ratatui lines, wrapped later by the widget.
#[must_use]
pub fn render(md: &str) -> Vec<RtLine<'static>> {
    let text = minimad::parse_text(
        md,
        Options {
            keep_code_fences: true,
            ..Options::default()
        },
    );
    let skin = Skin;
    let mut out = Vec::new();
    for line in &text.lines {
        match line {
            Line::Normal(composite) => out.push(composite_line(composite, None, &skin)),
            // With `keep_code_fences`, fence markers are empty lines and the fenced content
            // carries `CompositeStyle::Code`; the markers themselves render as nothing.
            Line::CodeFence(_) => {}
            Line::HorizontalRule => {
                out.push(RtLine::styled(
                    "────────────────────────",
                    skin.horizontal_rule(),
                ));
            }
            Line::TableRow(_) | Line::TableRule(_) => out.push(RtLine::from("")),
        }
    }
    out
}

fn composite_line<'a>(
    composite: &Composite<'a>,
    force: Option<Style>,
    skin: &Skin,
) -> RtLine<'static> {
    let base = force.unwrap_or_else(|| skin.composite(composite.style));
    let mut spans: Vec<Span<'static>> = Vec::new();
    match composite.style {
        CompositeStyle::ListItem(level) => {
            spans.push(Span::raw("  ".repeat(level as usize)));
            spans.push(Span::styled(skin.bullet().to_owned(), base));
        }
        CompositeStyle::OrderedListItem { level, index } => {
            spans.push(Span::raw("  ".repeat(level as usize)));
            spans.push(Span::styled(format!("{index}. "), base));
        }
        _ => {}
    }
    for compound in &composite.compounds {
        let mut style = base;
        if compound.code {
            style = style.fg(Color::LightCyan);
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::{Paragraph, Wrap};

    const SAMPLE: &str = "\
# Heading text
A paragraph with **bold**, *italic* and `inline code`, long enough that the narrow
test width forces ratatui to wrap it while keeping the spans styled.
- item one
```
fn main() {}
```
> a quote
";

    #[test]
    fn blocks_and_inline_attributes_map_to_spans() {
        let lines = render(SAMPLE);
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.style.fg == Some(Color::LightBlue)),
            "heading style: {:?}",
            lines[0]
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
        assert_eq!(code.style.fg, Some(Color::LightCyan));
        assert!(
            lines
                .iter()
                .any(|line| line.spans.iter().any(|span| span.content == "• "))
        );
        // The fenced block renders dim, its markers not at all.
        let fence = lines
            .iter()
            .find(|line| {
                line.spans
                    .iter()
                    .any(|span| span.content.contains("fn main()"))
            })
            .expect("fence content");
        assert_eq!(fence.spans[0].style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn wrapped_paragraph_keeps_span_styles() {
        let lines = render(SAMPLE);
        let backend = TestBackend::new(40, 12);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                let area = frame.area();
                frame.render_widget(
                    Paragraph::new(lines.clone()).wrap(Wrap { trim: true }),
                    area,
                );
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        // The bold word survived wrapping with its modifier on every cell it covers.
        let bold_cells = (0..12u16)
            .flat_map(|y| (0..40u16).map(move |x| (x, y)))
            .filter(|(x, y)| {
                let cell = &buffer[(*x, *y)];
                cell.symbol() == "b"
                    || cell.symbol() == "o"
                    || cell.symbol() == "l"
                    || cell.symbol() == "d"
            })
            .count();
        let bold_styled = (0..12u16)
            .flat_map(|y| (0..40u16).map(move |x| (x, y)))
            .filter(|(x, y)| buffer[(*x, *y)].modifier.contains(Modifier::BOLD))
            .count();
        assert!(bold_cells > 0, "the word should be present");
        assert!(
            bold_styled >= bold_cells / 2,
            "bold cells must carry the modifier"
        );
    }
}
