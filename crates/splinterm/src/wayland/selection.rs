//! Pure terminal selection, copied-text, overlay-row, and URL interpretation.

use splinterm_core::SplintId;
use splinterm_protocol::{ActiveScreen, TerminalRow, TerminalSnapshot};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct CellPosition {
    pub(in crate::wayland) row: usize,
    pub(in crate::wayland) column: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SelectionEndpoint {
    pub(super) active_screen: ActiveScreen,
    pub(super) history_generation: u64,
    pub(super) row_id: u64,
    pub(super) column: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Selection {
    pub(super) anchor: SelectionEndpoint,
    pub(super) end: SelectionEndpoint,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CopyModeState {
    pub(super) splint_id: SplintId,
    pub(super) incarnation: u64,
    pub(super) cursor: SelectionEndpoint,
    pub(super) anchor: Option<SelectionEndpoint>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CopyMotion {
    Left,
    Right,
    Up(usize),
    Down(usize),
    LineStart,
    LineEnd,
    WordForward,
    WordBackward,
    WordEnd,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct CopyMoveOutcome {
    pub(super) moved: bool,
    pub(super) request_older: bool,
}

impl CopyModeState {
    pub(super) fn overlay_selection(self) -> Selection {
        Selection {
            anchor: self.anchor.unwrap_or(self.cursor),
            end: self.cursor,
        }
    }

    pub(super) fn begin_selection(&mut self) {
        self.anchor = Some(self.cursor);
    }

    pub(super) const fn selecting(self) -> bool {
        self.anchor.is_some()
    }
}

pub(super) fn copy_mode_enter(
    snapshot: &TerminalSnapshot,
    display: &TerminalSnapshot,
) -> Option<CopyModeState> {
    let row = usize::try_from(display.cursor_row)
        .ok()
        .filter(|row| *row < display.visible_rows.len())
        .unwrap_or_else(|| display.visible_rows.len().saturating_sub(1));
    let column = usize::try_from(display.cursor_column)
        .unwrap_or(0)
        .min(snapshot.columns.saturating_sub(1));
    Some(CopyModeState {
        splint_id: snapshot.splint_id,
        incarnation: snapshot.incarnation,
        cursor: selection_endpoint(display, CellPosition { row, column })?,
        anchor: None,
    })
}

pub(super) fn copy_mode_is_valid(snapshot: &TerminalSnapshot, state: CopyModeState) -> bool {
    state.splint_id == snapshot.splint_id
        && state.incarnation == snapshot.incarnation
        && state.cursor.active_screen == snapshot.active_screen
        && state.cursor.history_generation == snapshot.history_generation
        && loaded_row_position(snapshot, state.cursor.row_id).is_some()
        && state.anchor.is_none_or(|anchor| {
            anchor.active_screen == snapshot.active_screen
                && anchor.history_generation == snapshot.history_generation
                && loaded_row_position(snapshot, anchor.row_id).is_some()
        })
}

pub(super) fn move_copy_cursor(
    snapshot: &TerminalSnapshot,
    state: &mut CopyModeState,
    motion: CopyMotion,
) -> CopyMoveOutcome {
    let Some(row) = loaded_row_position(snapshot, state.cursor.row_id) else {
        return CopyMoveOutcome::default();
    };
    let row_count = snapshot
        .scrollback_rows
        .len()
        .saturating_add(snapshot.visible_rows.len());
    if row_count == 0 || snapshot.columns == 0 {
        return CopyMoveOutcome::default();
    }
    let (next_row, next_column, request_older) = match motion {
        CopyMotion::Left => (row, state.cursor.column.saturating_sub(1), false),
        CopyMotion::Right => (
            row,
            state
                .cursor
                .column
                .saturating_add(1)
                .min(snapshot.columns - 1),
            false,
        ),
        CopyMotion::Up(lines) => {
            let next = row.saturating_sub(lines);
            (next, state.cursor.column, next == 0 && lines > row)
        }
        CopyMotion::Down(lines) => (
            row.saturating_add(lines).min(row_count - 1),
            state.cursor.column,
            false,
        ),
        CopyMotion::LineStart => (row, 0, false),
        CopyMotion::LineEnd => (row, snapshot.columns - 1, false),
        CopyMotion::WordForward => {
            let (row, column) = word_forward(snapshot, (row, state.cursor.column));
            (row, column, false)
        }
        CopyMotion::WordBackward => {
            let (row, column) = word_backward(snapshot, (row, state.cursor.column));
            (row, column, false)
        }
        CopyMotion::WordEnd => {
            let (row, column) = word_end(snapshot, (row, state.cursor.column));
            (row, column, false)
        }
    };
    let row_id = snapshot
        .scrollback_rows
        .iter()
        .chain(&snapshot.visible_rows)
        .nth(next_row)
        .and_then(|row| row.row_id);
    let Some(row_id) = row_id else {
        return CopyMoveOutcome {
            moved: false,
            request_older,
        };
    };
    let next = SelectionEndpoint {
        row_id,
        column: next_column,
        ..state.cursor
    };
    let moved = next != state.cursor;
    state.cursor = next;
    CopyMoveOutcome {
        moved,
        request_older,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WordClass {
    Whitespace,
    Keyword,
    Punctuation,
}

fn word_class(snapshot: &TerminalSnapshot, (row, column): (usize, usize)) -> WordClass {
    let cell = snapshot
        .scrollback_rows
        .iter()
        .chain(&snapshot.visible_rows)
        .nth(row)
        .and_then(|row| row.cells.get(column));
    let Some(cell) = cell else {
        return WordClass::Whitespace;
    };
    if cell.spacer_remaining.is_some()
        || cell.content.is_empty()
        || cell.content.chars().all(char::is_whitespace)
    {
        return WordClass::Whitespace;
    }
    if cell
        .content
        .chars()
        .all(|character| character.is_alphanumeric() || character == '_')
    {
        WordClass::Keyword
    } else {
        WordClass::Punctuation
    }
}

fn is_spacer(snapshot: &TerminalSnapshot, (row, column): (usize, usize)) -> bool {
    snapshot
        .scrollback_rows
        .iter()
        .chain(&snapshot.visible_rows)
        .nth(row)
        .and_then(|row| row.cells.get(column))
        .is_some_and(|cell| cell.spacer_remaining.is_some())
}

fn next_cell(snapshot: &TerminalSnapshot, mut position: (usize, usize)) -> Option<(usize, usize)> {
    let row_count = snapshot.scrollback_rows.len() + snapshot.visible_rows.len();
    loop {
        let (row, column) = position;
        position = if column + 1 < snapshot.columns {
            (row, column + 1)
        } else if row + 1 < row_count {
            (row + 1, 0)
        } else {
            return None;
        };
        if !is_spacer(snapshot, position) {
            return Some(position);
        }
    }
}

fn previous_cell(
    snapshot: &TerminalSnapshot,
    mut position: (usize, usize),
) -> Option<(usize, usize)> {
    loop {
        let (row, column) = position;
        position = if let Some(column) = column.checked_sub(1) {
            (row, column)
        } else {
            (row.checked_sub(1)?, snapshot.columns - 1)
        };
        if !is_spacer(snapshot, position) {
            return Some(position);
        }
    }
}

fn word_forward(snapshot: &TerminalSnapshot, position: (usize, usize)) -> (usize, usize) {
    let mut cursor = position;
    let class = word_class(snapshot, cursor);
    if class != WordClass::Whitespace {
        loop {
            let Some(next) = next_cell(snapshot, cursor) else {
                return cursor;
            };
            if next.0 != cursor.0 || word_class(snapshot, next) != class {
                cursor = next;
                break;
            }
            cursor = next;
        }
    }
    while word_class(snapshot, cursor) == WordClass::Whitespace {
        let Some(next) = next_cell(snapshot, cursor) else {
            return cursor;
        };
        cursor = next;
    }
    cursor
}

fn word_backward(snapshot: &TerminalSnapshot, position: (usize, usize)) -> (usize, usize) {
    let mut cursor = position;
    loop {
        let Some(previous) = previous_cell(snapshot, cursor) else {
            return cursor;
        };
        cursor = previous;
        if word_class(snapshot, cursor) != WordClass::Whitespace {
            break;
        }
    }
    let class = word_class(snapshot, cursor);
    while let Some(previous) = previous_cell(snapshot, cursor) {
        if previous.0 != cursor.0 || word_class(snapshot, previous) != class {
            break;
        }
        cursor = previous;
    }
    cursor
}

fn word_end(snapshot: &TerminalSnapshot, position: (usize, usize)) -> (usize, usize) {
    let mut cursor = position;
    let class = word_class(snapshot, cursor);
    if class != WordClass::Whitespace {
        while let Some(next) = next_cell(snapshot, cursor) {
            if next.0 != cursor.0 || word_class(snapshot, next) != class {
                break;
            }
            cursor = next;
        }
        if cursor != position {
            return cursor;
        }
    }
    loop {
        let Some(next) = next_cell(snapshot, cursor) else {
            return cursor;
        };
        cursor = next;
        if word_class(snapshot, cursor) != WordClass::Whitespace {
            break;
        }
    }
    let class = word_class(snapshot, cursor);
    while let Some(next) = next_cell(snapshot, cursor) {
        if next.0 != cursor.0 || word_class(snapshot, next) != class {
            break;
        }
        cursor = next;
    }
    cursor
}

pub(super) fn selection_endpoint(
    snapshot: &TerminalSnapshot,
    position: CellPosition,
) -> Option<SelectionEndpoint> {
    let row_id = snapshot.visible_rows.get(position.row)?.row_id?;
    Some(SelectionEndpoint {
        active_screen: snapshot.active_screen,
        history_generation: snapshot.history_generation,
        row_id,
        column: position.column,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SelectionRange {
    start_row: usize,
    start_column: usize,
    end_row: usize,
    end_column: usize,
}

fn loaded_row_position(snapshot: &TerminalSnapshot, row_id: u64) -> Option<usize> {
    loaded_row_position_on(snapshot, &snapshot.visible_rows, row_id)
}

/// A row's position in the history cache followed by `screen`: the live
/// screen, or the one a holding viewport shows instead.
fn loaded_row_position_on(
    snapshot: &TerminalSnapshot,
    screen: &[TerminalRow],
    row_id: u64,
) -> Option<usize> {
    snapshot
        .scrollback_rows
        .iter()
        .chain(screen)
        .position(|row| row.row_id == Some(row_id))
}

fn selection_range(
    snapshot: &TerminalSnapshot,
    screen: &[TerminalRow],
    selection: Selection,
) -> Option<SelectionRange> {
    if selection.anchor.active_screen != snapshot.active_screen
        || selection.end.active_screen != snapshot.active_screen
        || selection.anchor.history_generation != snapshot.history_generation
        || selection.end.history_generation != snapshot.history_generation
    {
        return None;
    }
    let anchor_row = loaded_row_position_on(snapshot, screen, selection.anchor.row_id)?;
    let end_row = loaded_row_position_on(snapshot, screen, selection.end.row_id)?;
    let anchor = (anchor_row, selection.anchor.column);
    let end = (end_row, selection.end.column);
    let (start, end) = if anchor <= end {
        (anchor, end)
    } else {
        (end, anchor)
    };
    Some(SelectionRange {
        start_row: start.0,
        start_column: start.1,
        end_row: end.0,
        end_column: end.1,
    })
}

pub(super) fn selection_display_bounds(
    snapshot: &TerminalSnapshot,
    screen: &[TerminalRow],
    display: &TerminalSnapshot,
    selection: Selection,
) -> Option<(CellPosition, CellPosition)> {
    let range = selection_range(snapshot, screen, selection)?;
    let mut selected = display
        .visible_rows
        .iter()
        .enumerate()
        .filter_map(|(display_row, row)| {
            let loaded_row = loaded_row_position_on(snapshot, screen, row.row_id?)?;
            (loaded_row >= range.start_row && loaded_row <= range.end_row)
                .then_some((display_row, loaded_row))
        });
    let first = selected.next()?;
    let last = selected.next_back().unwrap_or(first);
    Some((
        CellPosition {
            row: first.0,
            column: if first.1 == range.start_row {
                range.start_column
            } else {
                0
            },
        },
        CellPosition {
            row: last.0,
            column: if last.1 == range.end_row {
                range.end_column
            } else {
                snapshot.columns.saturating_sub(1)
            },
        },
    ))
}

pub(super) fn transient_overlay_rows(
    row_count: usize,
    selection: Option<((usize, usize), (usize, usize))>,
    hovered_url: Option<((usize, usize), (usize, usize))>,
) -> Vec<bool> {
    let mut dirty = vec![false; row_count];
    let mut mark = |start: usize, end: usize| {
        let start = start.min(row_count);
        let end = end.saturating_add(1).min(row_count);
        dirty[start..end].fill(true);
    };
    if let Some((start, end)) = selection {
        mark(start.0, end.0);
    }
    if let Some((start, end)) = hovered_url {
        mark(start.0, end.0);
    }
    dirty
}

pub(super) fn selection_is_retained(
    snapshot: &TerminalSnapshot,
    screen: &[TerminalRow],
    selection: Selection,
) -> bool {
    selection_range(snapshot, screen, selection).is_some()
}

/// The text of `selection`, one line per terminal line.
///
/// A row the terminal soft-wrapped (it has content and no hard
/// `linebreak`) joins the next row without a newline, so a long command or
/// URL copies as the one line it is (Plan 0074). The alternate screen never
/// joins: full-screen programs place text with the cursor, and their rows
/// are separate lines. An empty cell reads as a space, so text a program
/// spaced with tabs or cursor moves keeps its gaps; each copied line loses
/// its trailing spaces, and a wrapped row loses only trailing empty cells,
/// such as the one a wide character left when it wrapped.
pub(super) fn selection_text(
    snapshot: &TerminalSnapshot,
    screen: &[TerminalRow],
    selection: Selection,
) -> Option<String> {
    let range = selection_range(snapshot, screen, selection)?;
    let rows = snapshot.scrollback_rows.iter().chain(screen);
    let joins = snapshot.active_screen != ActiveScreen::Alternate;
    let mut output = String::new();
    for (row_index, row) in rows
        .enumerate()
        .skip(range.start_row)
        .take(range.end_row.saturating_sub(range.start_row) + 1)
    {
        let first = if row_index == range.start_row {
            range.start_column
        } else {
            0
        };
        let last = if row_index == range.end_row {
            range.end_column
        } else {
            snapshot.columns.saturating_sub(1)
        };
        let wraps = joins && row_soft_wraps(row);
        let cells = row.cells.iter().take(last.saturating_add(1)).skip(first);
        let cells: Vec<_> = cells
            .filter(|cell| cell.spacer_remaining.is_none())
            .collect();
        let kept = if wraps {
            cells.len()
                - cells
                    .iter()
                    .rev()
                    .take_while(|cell| cell.content.is_empty())
                    .count()
        } else {
            cells.len()
        };
        let mut line = String::new();
        for cell in &cells[..kept] {
            line.push_str(if cell.content.is_empty() {
                " "
            } else {
                &cell.content
            });
        }
        if wraps && row_index != range.end_row {
            output.push_str(&line);
        } else {
            output.push_str(line.trim_end_matches(' '));
            if row_index != range.end_row {
                output.push('\n');
            }
        }
    }
    Some(output)
}

/// Whether the terminal soft-wrapped `row` into the next: an autowrap
/// clears the row's hard linebreak, and a row with no content never joins,
/// as in the terminal's own reflow.
fn row_soft_wraps(row: &TerminalRow) -> bool {
    !row.linebreak && row.cells.iter().any(|cell| !cell.content.is_empty())
}

pub(super) fn url_at(
    snapshot: &TerminalSnapshot,
    position: CellPosition,
) -> Option<(CellPosition, CellPosition, String)> {
    let row = snapshot.visible_rows.get(position.row)?;
    let mut text = String::new();
    let mut columns = Vec::new();
    for (column, cell) in row.cells.iter().take(snapshot.columns).enumerate() {
        if cell.spacer_remaining.is_some() {
            continue;
        }
        for character in cell.content.chars() {
            columns.push(column);
            text.push(character);
        }
    }
    let byte_at = text
        .char_indices()
        .zip(columns.iter().copied())
        .find_map(|((byte, _), column)| (column == position.column).then_some(byte))?;
    let is_url_char = |character: char| {
        !character.is_whitespace() && !matches!(character, '<' | '>' | '"' | '\'')
    };
    let start = text[..byte_at]
        .char_indices()
        .rev()
        .find_map(|(index, character)| {
            (!is_url_char(character)).then_some(index + character.len_utf8())
        })
        .unwrap_or(0);
    let end = text[byte_at..]
        .char_indices()
        .find_map(|(index, character)| (!is_url_char(character)).then_some(byte_at + index))
        .unwrap_or(text.len());
    let candidate = text[start..end].trim_end_matches(['.', ',', ')', ']', '}', ';', ':']);
    if !(candidate.starts_with("https://") || candidate.starts_with("http://")) {
        return None;
    }
    let start_char = text[..start].chars().count();
    let end_char = start_char + candidate.chars().count().saturating_sub(1);
    Some((
        CellPosition {
            row: position.row,
            column: *columns.get(start_char)?,
        },
        CellPosition {
            row: position.row,
            column: *columns.get(end_char)?,
        },
        candidate.to_owned(),
    ))
}
