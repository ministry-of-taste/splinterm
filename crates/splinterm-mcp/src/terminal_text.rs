//! The plain-text form of terminal reads (Plan 0067 M2).
//!
//! `splinterm.read_terminal` and `splinterm.read_scrollback` return the text
//! form unless the caller asks for `format: "cells"`. A per-cell object costs
//! about 24 bytes of JSON for each column, so a full screen of cells can exceed
//! an agent host's tool-result limit; the same screen as text costs about one
//! byte per visible character.

use serde_json::Value;
use splinterm_automation_client::ProjectedTerminalRow;

/// How a terminal read presents its rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalFormat {
    /// One `text` string with `content_encoding: "text"`.
    Text,
    /// One object per cell in `rows`, with `content_encoding: "unicode_scalars"`.
    Cells,
}

impl TerminalFormat {
    /// Reads the optional `format` argument; `None` for an unknown value.
    pub(crate) fn from_arguments(arguments: &Value) -> Option<Self> {
        match arguments.get("format") {
            None => Some(Self::Text),
            Some(value) => match value.as_str()? {
                "text" => Some(Self::Text),
                "cells" => Some(Self::Cells),
                _ => None,
            },
        }
    }
}

/// Where the rows of a text read come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextLayout {
    /// The visible main screen: wrapped rows join, and trailing empty lines
    /// are dropped.
    Screen,
    /// The visible alternate screen of a full-screen program such as vim or
    /// htop: one line per row, never joined, as Foot reflows only the main
    /// screen. Trailing empty lines are dropped.
    AlternateScreen,
    /// A scrollback page: every hard line end is a `\n` and empty lines are
    /// kept. A page whose last row wraps ends without one, partway through a
    /// line, so an older page's text followed by the newer page's text is the
    /// history exactly.
    ScrollbackPage,
}

/// Joins projected rows into lines of plain text.
///
/// `linebreak` is the terminal's hard line end. Every row starts with it, and
/// only an autowrap clears it, on the row the text wrapped from. A row that
/// has content and no linebreak therefore continues on the next row, and the
/// two make one line. A row with no content never joins, which matches the
/// terminal's own reflow, so a blank row is always an empty line.
///
/// An empty cell reads as a space, a wide character appears once, and each
/// line that ends in the rows loses its trailing spaces.
pub(crate) fn terminal_text(rows: &[ProjectedTerminalRow], layout: TextLayout) -> String {
    let mut text = String::new();
    let mut line_start = 0;
    for row in rows {
        for cell in &row.cells {
            if cell.text.is_empty() {
                text.push(' ');
            } else {
                text.push_str(&cell.text);
            }
        }
        let continues = layout != TextLayout::AlternateScreen
            && !row.linebreak
            && row.cells.iter().any(|cell| !cell.text.is_empty());
        if !continues {
            let line_end = line_start + text[line_start..].trim_end_matches(' ').len();
            text.truncate(line_end);
            text.push('\n');
            line_start = text.len();
        }
    }
    if layout != TextLayout::ScrollbackPage {
        let end = text.trim_end_matches([' ', '\n']).len();
        text.truncate(end);
    }
    text
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use splinterm_automation_client::project_terminal_rows;
    use splinterm_protocol::{CellAttributes, TerminalCell, TerminalRow};

    use super::*;

    fn cell(content: &str) -> TerminalCell {
        TerminalCell {
            content: content.to_owned(),
            spacer_remaining: None,
            attributes: CellAttributes::default(),
        }
    }

    fn spacer(remaining: u32) -> TerminalCell {
        TerminalCell {
            content: String::new(),
            spacer_remaining: Some(remaining),
            attributes: CellAttributes::default(),
        }
    }

    /// A row of `columns` cells: `text` one scalar per cell, then empty cells.
    fn row(text: &str, columns: usize, linebreak: bool) -> TerminalRow {
        let mut cells = text
            .chars()
            .map(|character| cell(&character.to_string()))
            .collect::<Vec<_>>();
        cells.resize_with(columns.max(cells.len()), || cell(""));
        TerminalRow {
            row_id: None,
            linebreak,
            cells,
        }
    }

    fn laid_out(rows: &[TerminalRow], layout: TextLayout) -> String {
        terminal_text(&project_terminal_rows(rows).unwrap(), layout)
    }

    fn text_of(rows: &[TerminalRow]) -> String {
        laid_out(rows, TextLayout::Screen)
    }

    fn page_of(rows: &[TerminalRow]) -> String {
        laid_out(rows, TextLayout::ScrollbackPage)
    }

    #[test]
    fn the_format_argument_defaults_to_text_and_rejects_unknown_values() {
        assert_eq!(
            TerminalFormat::from_arguments(&json!({})),
            Some(TerminalFormat::Text)
        );
        assert_eq!(
            TerminalFormat::from_arguments(&json!({"format": "text"})),
            Some(TerminalFormat::Text)
        );
        assert_eq!(
            TerminalFormat::from_arguments(&json!({"format": "cells"})),
            Some(TerminalFormat::Cells)
        );
        for format in [json!("rows"), json!(null), json!(1)] {
            assert_eq!(
                TerminalFormat::from_arguments(&json!({"format": format})),
                None
            );
        }
    }

    // `linebreak: true` ends a line; `linebreak: false` after content is an
    // autowrap, so the next row continues the same line.
    #[test]
    fn a_soft_wrapped_row_joins_the_next_row_and_a_hard_linebreak_ends_the_line() {
        let rows = [
            row("$ echo hello wor", 16, false),
            row("ld", 16, true),
            row("hello world", 16, true),
            row("$", 16, true),
        ];
        assert_eq!(text_of(&rows), "$ echo hello world\nhello world\n$");
    }

    #[test]
    fn spaces_at_a_wrap_point_survive_and_only_line_ends_are_trimmed() {
        let rows = [row("hello ", 6, false), row("world   ", 8, true)];
        assert_eq!(text_of(&rows), "hello world");
    }

    #[test]
    fn blank_rows_are_empty_lines_and_trailing_blank_rows_are_dropped() {
        // A blank row never joins, even without a linebreak.
        let rows = [
            row("first", 10, true),
            row("", 10, false),
            row("   ", 10, true),
            row("second  ", 10, true),
            row("", 10, false),
            row("", 10, false),
        ];
        assert_eq!(text_of(&rows), "first\n\n\nsecond");
        assert_eq!(text_of(&[row("", 10, false), row("", 10, true)]), "");
        assert_eq!(text_of(&[]), "");
    }

    #[test]
    fn interior_spaces_and_leading_blank_lines_are_kept() {
        let rows = [row("", 12, false), row("  a    b", 12, true)];
        assert_eq!(text_of(&rows), "\n  a    b");
    }

    #[test]
    fn a_wide_character_appears_once_and_composed_text_is_kept() {
        let rows = [TerminalRow {
            row_id: None,
            linebreak: true,
            cells: vec![cell("e\u{301}"), cell("界"), spacer(1), cell("x")],
        }];
        assert_eq!(text_of(&rows), "e\u{301}界x");
    }

    #[test]
    fn a_wide_character_wrapped_at_the_margin_joins_without_its_padding() {
        // An autowrap before a wide character pads the last column with a
        // zero spacer and clears the row's linebreak.
        let rows = [
            TerminalRow {
                row_id: None,
                linebreak: false,
                cells: vec![cell("a"), cell("b"), spacer(0)],
            },
            TerminalRow {
                row_id: None,
                linebreak: true,
                cells: vec![cell("界"), spacer(1), cell("")],
            },
        ];
        assert_eq!(text_of(&rows), "ab界");
    }

    #[test]
    fn a_last_row_that_wraps_off_the_page_still_ends_the_text() {
        let rows = [row("abc", 3, true), row("def", 3, false)];
        assert_eq!(text_of(&rows), "abc\ndef");
    }

    #[test]
    fn a_scrollback_page_ends_every_hard_line_and_keeps_empty_lines() {
        let rows = [
            row("first", 8, true),
            row("", 8, true),
            row("second  ", 8, true),
            row("", 8, false),
        ];
        assert_eq!(page_of(&rows), "first\n\nsecond\n\n");
        assert_eq!(page_of(&[]), "");
    }

    // Pages run older; an agent that puts each older page before the text it
    // has must rebuild the history exactly, whichever row a page starts at.
    #[test]
    fn scrollback_pages_join_exactly_across_a_wrap_and_blank_lines() {
        let history = [
            row("$ echo hello ", 13, false),
            row("world", 13, true),
            row("", 13, true),
            row("", 13, true),
            row("abc", 3, false),
            row("de f", 4, false),
            row("g", 4, true),
            row("$", 13, true),
        ];
        let whole = page_of(&history);
        assert_eq!(whole, "$ echo hello world\n\n\nabcde fg\n$\n");
        for split in 0..=history.len() {
            let (older, newer) = history.split_at(split);
            assert_eq!(
                page_of(older) + &page_of(newer),
                whole,
                "pages split before row {split}"
            );
        }
        // A page that ends mid-wrap stops partway through the line.
        assert_eq!(page_of(&history[..1]), "$ echo hello ");
        assert_eq!(page_of(&history[4..5]), "abc");
    }

    // Full-screen programs draw the alternate screen row by row; Foot never
    // reflows it, so its rows are never joined.
    #[test]
    fn alternate_screen_rows_are_never_joined() {
        let rows = [row("abc", 3, false), row("def", 3, true), row("", 3, true)];
        assert_eq!(laid_out(&rows, TextLayout::AlternateScreen), "abc\ndef");
        assert_eq!(laid_out(&rows, TextLayout::Screen), "abcdef");
    }

    // A representative 70 x 134 screen: shell and tool output padded with
    // spaces to the margin, as full-screen programs draw it.
    #[test]
    fn a_representative_screen_is_far_smaller_as_text_than_as_cells() {
        let words = "the quick brown fox jumps over the lazy dog while cargo builds ";
        let rows = (0..70)
            .map(|index| {
                let length = (index * 37) % 120;
                let line = words.chars().cycle().take(length).collect::<String>();
                let padded = format!("{line:<134}");
                row(&padded, 134, true)
            })
            .collect::<Vec<_>>();
        let projected = project_terminal_rows(&rows).unwrap();
        let cells = json!({"content_encoding": "unicode_scalars", "rows": projected});
        let text = terminal_text(&projected, TextLayout::Screen);
        let text = json!({"content_encoding": "text", "text": text});
        let cells = serde_json::to_vec(&cells).unwrap().len();
        let text = serde_json::to_vec(&text).unwrap().len();
        eprintln!("70x134 screen: cells {cells} bytes, text {text} bytes");
        assert!(cells > 200_000, "cells form was {cells} bytes");
        assert!(
            text * 30 < cells,
            "text {text} bytes vs cells {cells} bytes"
        );
    }
}
