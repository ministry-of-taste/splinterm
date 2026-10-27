//! The one list row every popup list draws: a one-cell marker, the title
//! with its matched characters, key chips, and a trailing word; the group
//! headers between rows; and which rows a list window shows.
//!
//! Sizes are logical px at 1× unless a name says buffer px.

use std::ops::Range;

use anyhow::Result;
use unicode_width::UnicodeWidthChar;

use crate::{
    color::{TEXT_MIN_CONTRAST, contrast_floor_toward, mix_encoded},
    geometry::Rect,
};

use super::{
    super::ChromeTextStyle,
    chrome_box::{ChromePainter, opaque},
    picker::{PickerTextAlignment, ROW_HOVER_ALPHA, ROW_PRESS_ALPHA, SessionPickerPalette},
};

/// One list row.
pub(super) const ROW_HEIGHT: u32 = 28;
/// A group header: its label and a rule to the row's right edge.
pub(super) const GROUP_HEIGHT: u32 = 24;
/// From the row's edges to its marker and to its last chip or word.
pub(super) const ROW_PADDING: u32 = 10;
/// Between two chips of one binding.
pub(super) const CHIP_GAP: u32 = 3;
/// Inside a chip, on each side of its key.
pub(super) const CHIP_PADDING: u32 = 5;
/// A chip's fill: the primary text at 10% over the panel.
pub(super) const CHIP_ALPHA: u8 = 26;
/// A chip's fill on the selected row: the selected text at 12%.
pub(super) const SELECTED_CHIP_ALPHA: u8 = 31;
/// The key bindings list's chips column, in cells.
pub(super) const KEY_COLUMN_CELLS: u32 = 21;
/// A list shows at most this many rows at once.
pub(super) const LIST_MAXIMUM_ROWS: usize = 10;

/// The cells `text` takes as monospaced chrome text.
pub(super) fn text_cells(text: &str) -> u32 {
    let cells = text
        .chars()
        .map(|character| character.width().unwrap_or(0))
        .sum::<usize>();
    u32::try_from(cells).unwrap_or(u32::MAX)
}

/// Where the first case-insensitive occurrence of `query` sits in
/// `title`, to draw those characters matched; `None` for an empty query
/// or one found only in a keyword or the group.
pub(super) fn title_match(title: &str, query: &str) -> Option<Range<usize>> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }
    // ASCII folding keeps byte offsets, so the range is valid in `title`.
    let start = title
        .to_ascii_lowercase()
        .find(&query.to_ascii_lowercase())?;
    Some(start..start.saturating_add(query.len()))
}

/// Whether the row at `index` starts a group: the first row, or one whose
/// group differs from the row above. `groups` is empty for a flat list; a
/// row whose group is empty (a picker's creation row) has no header.
pub(super) fn starts_group(groups: &[&str], index: usize) -> bool {
    index < groups.len()
        && !groups[index].is_empty()
        && (index == 0 || groups[index] != groups[index.saturating_sub(1)])
}

/// The height of rows `start..end` with their group headers.
pub(super) fn window_height(groups: &[&str], start: usize, end: usize) -> u32 {
    (start..end)
        .map(|index| {
            if starts_group(groups, index) {
                ROW_HEIGHT + GROUP_HEIGHT
            } else {
                ROW_HEIGHT
            }
        })
        .fold(0, u32::saturating_add)
}

/// The rows `start..end` of a `len`-row list that one `budget` px window
/// shows: at most [`LIST_MAXIMUM_ROWS`], `selected` among them, starting at
/// `requested_start` when that keeps it, and filled back up when the end of
/// the list leaves room above.
pub(super) fn visible_window(
    len: usize,
    groups: &[&str],
    selected: usize,
    requested_start: usize,
    budget: u32,
) -> Range<usize> {
    if len == 0 || budget < ROW_HEIGHT {
        return 0..0;
    }
    let selected = selected.min(len - 1);
    let fits = |start: usize, end: usize| {
        end.saturating_sub(start) <= LIST_MAXIMUM_ROWS
            && window_height(groups, start, end) <= budget
    };
    let mut start = requested_start.min(selected);
    while start < selected && !fits(start, selected + 1) {
        start += 1;
    }
    let mut end = selected + 1;
    while end < len && fits(start, end + 1) {
        end += 1;
    }
    while start > 0 && fits(start - 1, end) {
        start -= 1;
    }
    start..end
}

/// Where a row's parts go, in buffer px.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RowColumns {
    /// The one-cell marker column: `›` on the selected row.
    pub(super) marker: Rect,
    /// The key bindings' chips column after the marker; zero wide
    /// elsewhere.
    pub(super) chips: Rect,
    pub(super) title: Rect,
    /// The right end, for chips or a word; zero wide when there is none.
    pub(super) trailing: Rect,
}

/// A row's columns in `row` (buffer px): the padding, the marker and a
/// cell of space, the leading chips column and a cell of space, the title,
/// and a cell of space before the trailing part. The leading column takes
/// at most two fifths of the room after the marker and the trailing part
/// half of it, so the title always keeps some.
pub(super) fn row_columns(
    row: Rect,
    padding: u32,
    cell_width: u32,
    leading_width: u32,
    trailing_width: u32,
) -> RowColumns {
    let padding = padding.min(row.width / 4);
    let left = row.x.saturating_add(padding);
    let right = row
        .x
        .saturating_add(row.width)
        .saturating_sub(padding)
        .max(left);
    let marker = Rect {
        x: left,
        width: cell_width.min(right - left),
        ..row
    };
    let content_x = left.saturating_add(cell_width.saturating_mul(2)).min(right);
    let content_width = right - content_x;
    let leading = leading_width.min(content_width.saturating_mul(2) / 5);
    let chips = Rect {
        x: content_x,
        width: leading,
        ..row
    };
    let title_x = if leading > 0 {
        content_x
            .saturating_add(leading)
            .saturating_add(cell_width)
            .min(right)
    } else {
        content_x
    };
    let trailing_width = trailing_width
        .min(content_width / 2)
        .min(right.saturating_sub(title_x));
    let trailing_x = right - trailing_width;
    let title_end = if trailing_width > 0 {
        trailing_x.saturating_sub(cell_width).max(title_x)
    } else {
        right
    };
    RowColumns {
        marker,
        chips,
        title: Rect {
            x: title_x,
            width: title_end - title_x,
            ..row
        },
        trailing: Rect {
            x: trailing_x,
            width: trailing_width,
            ..row
        },
    }
}

/// Where one binding's chips go, in buffer px.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ChipLayout {
    /// The first shown key's index in the tokens: the keys before it
    /// dropped.
    pub(super) first: usize,
    /// One rect per shown key, in order.
    pub(super) chips: Vec<Rect>,
    /// The `…` before the shown keys when earlier ones did not fit.
    pub(super) ellipsis: Option<Rect>,
    /// From the `…`'s (or the first chip's) left edge to the last chip's
    /// right edge.
    pub(super) width: u32,
}

/// One chip per key token, [`CHIP_GAP`] apart, each its key plus
/// [`CHIP_PADDING`] each side, as tall as a cell and centred in `room`.
/// When they do not all fit, the modifiers drop from the front behind a
/// `…` cell (`… shift p`, then `… p`), so the key itself stays and the chips
/// never run past `room`; then the last key alone, without the `…`; and no
/// chips only when not even that fits. `right_aligned` puts the last chip
/// at `room`'s right edge.
pub(super) fn chip_layout(
    tokens: &[String],
    room: Rect,
    cell: (u32, u32),
    scale_120: u32,
    right_aligned: bool,
) -> ChipLayout {
    let scaled = |logical: u32| logical.saturating_mul(scale_120).div_ceil(120);
    let (padding, gap) = (scaled(CHIP_PADDING), scaled(CHIP_GAP));
    let widths = tokens
        .iter()
        .map(|token| {
            text_cells(token)
                .saturating_mul(cell.0)
                .saturating_add(padding.saturating_mul(2))
        })
        .collect::<Vec<_>>();
    // The width of the keys from `first` on, after a `…` when `ellipsis`.
    let total = |first: usize, ellipsis: bool| {
        let shown = widths.len() - first;
        let chips = widths[first..].iter().copied().fold(0, u32::saturating_add);
        let gaps = gap.saturating_mul(u32::try_from(shown.saturating_sub(1)).unwrap_or(u32::MAX));
        let ellipsis = if ellipsis {
            cell.0.saturating_add(gap)
        } else {
            0
        };
        chips.saturating_add(gaps).saturating_add(ellipsis)
    };
    let last = widths.len().saturating_sub(1);
    let fit = (0..widths.len())
        .map(|first| (first, first > 0))
        .chain((!widths.is_empty()).then_some((last, false)))
        .find(|(first, ellipsis)| total(*first, *ellipsis) <= room.width);
    let Some((first, ellipsis)) = fit else {
        return ChipLayout {
            first: widths.len(),
            chips: Vec::new(),
            ellipsis: None,
            width: 0,
        };
    };
    let width = total(first, ellipsis);
    let height = cell.1.min(room.height);
    let y = room
        .y
        .saturating_add(room.height.saturating_sub(height) / 2);
    let mut x = if right_aligned {
        room.x.saturating_add(room.width.saturating_sub(width))
    } else {
        room.x
    };
    let ellipsis = ellipsis.then(|| {
        let rect = Rect {
            x,
            y,
            width: cell.0,
            height,
        };
        x = x.saturating_add(cell.0).saturating_add(gap);
        rect
    });
    let mut chips = Vec::with_capacity(widths.len() - first);
    for chip_width in &widths[first..] {
        chips.push(Rect {
            x,
            y,
            width: *chip_width,
            height,
        });
        x = x.saturating_add(*chip_width).saturating_add(gap);
    }
    ChipLayout {
        first,
        chips,
        ellipsis,
        width,
    }
}

/// The natural width of `tokens`' chips, with nothing dropped.
pub(super) fn chips_width(tokens: &[String], cell: (u32, u32), scale_120: u32) -> u32 {
    let room = Rect {
        x: 0,
        y: 0,
        width: u32::MAX,
        height: cell.1,
    };
    chip_layout(tokens, room, cell, scale_120, false).width
}

/// `title` cut to `room_cells` cells, ending in `…` when it does not fit,
/// in runs: each run's text and whether it is the matched part.
pub(super) fn title_runs(
    title: &str,
    matched: Option<Range<usize>>,
    room_cells: u32,
) -> Vec<(String, bool)> {
    if room_cells == 0 {
        return Vec::new();
    }
    let fits = text_cells(title) <= room_cells;
    let kept_end = if fits {
        title.len()
    } else {
        let mut cells = 0;
        let mut end = 0;
        for (index, character) in title.char_indices() {
            let width = u32::try_from(character.width().unwrap_or(0)).unwrap_or(u32::MAX);
            if cells + width > room_cells.saturating_sub(1) {
                break;
            }
            cells += width;
            end = index + character.len_utf8();
        }
        end
    };
    let matched = matched
        .map(|range| range.start.min(kept_end)..range.end.min(kept_end))
        .filter(|range| {
            !range.is_empty()
                && title.is_char_boundary(range.start)
                && title.is_char_boundary(range.end)
        });
    let mut runs = Vec::with_capacity(4);
    match matched {
        Some(range) => {
            for (text, matched) in [
                (&title[..range.start], false),
                (&title[range.clone()], true),
                (&title[range.end..kept_end], false),
            ] {
                if !text.is_empty() {
                    runs.push((text.to_owned(), matched));
                }
            }
        }
        None if kept_end > 0 => runs.push((title[..kept_end].to_owned(), false)),
        None => {}
    }
    if !fits {
        match runs.last_mut() {
            Some((text, false)) => text.push('…'),
            Some((_, true)) | None => runs.push(("…".to_owned(), false)),
        }
    }
    runs
}

/// What one row looks like.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a row's selection, pointer, availability and danger are independent"
)]
pub(super) struct RowState {
    pub(super) selected: bool,
    pub(super) hovered: bool,
    pub(super) pressed: bool,
    pub(super) disabled: bool,
    /// Terminate rows: danger text and marker.
    pub(super) danger: bool,
}

/// One list row's content.
pub(super) struct ListRow<'a> {
    pub(super) title: &'a str,
    /// The characters the query matched, drawn in the accent and bold.
    pub(super) matched: Option<Range<usize>>,
    /// A glyph in the marker column while the row is not selected: a
    /// picker's creation row shows `+`.
    pub(super) marker: Option<&'a str>,
    /// The binding's keys, one chip each.
    pub(super) chips: &'a [String],
    /// Chips in the leading column (key bindings) rather than at the end.
    pub(super) chip_column: bool,
    /// A word at the end: a group tag, a source, a status. Not drawn when
    /// trailing chips take the end.
    pub(super) trailing: &'a str,
    pub(super) trailing_accent: bool,
    pub(super) state: RowState,
}

/// The colours one row paints its title, matched characters and marker in.
struct RowColors {
    title: u32,
    matched: u32,
    marker: u32,
    secondary: u32,
    chip: [u8; 4],
    /// A chip's key: the secondary text, floored toward the primary until
    /// it keeps 4.5:1 on the chip's own composed colour.
    chip_text: u32,
    accent: u32,
}

fn row_colors(palette: &SessionPickerPalette, state: RowState) -> RowColors {
    let highlighted = !state.disabled && !state.selected && (state.hovered || state.pressed);
    let (primary, secondary) = palette.row_text(state.selected, highlighted);
    let title = if state.disabled {
        palette.secondary
    } else if state.danger {
        palette.row_danger(state.selected, highlighted)
    } else if state.selected {
        palette.selected_accent
    } else {
        primary
    };
    let matched = if state.disabled || state.danger {
        title
    } else if state.selected {
        palette.selected_primary
    } else {
        palette.accent_text
    };
    let [red, green, blue, _] = opaque(primary);
    let chip_alpha = if state.selected {
        SELECTED_CHIP_ALPHA
    } else {
        CHIP_ALPHA
    };
    let secondary = if state.disabled {
        palette.secondary
    } else {
        secondary
    };
    // The chip on the row's opaque fill: the selection, or the panel.
    let base = if state.selected {
        palette.selected_fill
    } else {
        palette.panel
    };
    let chip_colour = mix_encoded(base, primary, f64::from(chip_alpha) / 255.0);
    RowColors {
        title,
        matched,
        marker: if state.danger {
            title
        } else {
            palette.selected_accent
        },
        secondary,
        chip: [red, green, blue, chip_alpha],
        chip_text: contrast_floor_toward(secondary, primary, chip_colour, TEXT_MIN_CONTRAST),
        accent: if state.selected {
            palette.selected_accent
        } else {
            palette.accent_text
        },
    }
}

/// One row in `rect` (buffer px): its fill for the selected, hovered or
/// pressed state, the accent `›` when selected, the title with its matched
/// characters, the chips and the trailing word.
pub(super) fn paint_row(
    painter: &mut ChromePainter<'_>,
    palette: &SessionPickerPalette,
    rect: Rect,
    row: &ListRow<'_>,
) -> Result<()> {
    let state = row.state;
    paint_row_fill(painter, palette, rect, state);
    let colors = row_colors(palette, state);
    let cell = painter.cell(ChromeTextStyle::Regular)?;
    let scale_120 = painter.scale_120;
    let leading = if row.chip_column {
        KEY_COLUMN_CELLS.saturating_mul(cell.0)
    } else {
        0
    };
    let trailing_chips = !row.chip_column && !row.chips.is_empty();
    let trailing_width = if trailing_chips {
        chips_width(row.chips, cell, scale_120)
    } else {
        text_cells(row.trailing).saturating_mul(cell.0)
    };
    let columns = row_columns(
        rect,
        painter.scaled(ROW_PADDING),
        cell.0,
        leading,
        trailing_width,
    );
    if state.selected {
        painter.text(
            "›",
            ChromeTextStyle::Regular,
            columns.marker,
            PickerTextAlignment::Left,
            colors.marker,
        )?;
    } else if let Some(marker) = row.marker {
        painter.text(
            marker,
            ChromeTextStyle::Regular,
            columns.marker,
            PickerTextAlignment::Left,
            colors.accent,
        )?;
    }
    paint_title(
        painter,
        columns.title,
        row.title,
        row.matched.clone(),
        &colors,
    )?;
    if row.chip_column || trailing_chips {
        let room = if row.chip_column {
            columns.chips
        } else {
            columns.trailing
        };
        let chips = chip_layout(row.chips, room, cell, scale_120, !row.chip_column);
        if let Some(ellipsis) = chips.ellipsis {
            painter.text(
                "…",
                ChromeTextStyle::Regular,
                ellipsis,
                PickerTextAlignment::Center,
                colors.secondary,
            )?;
        }
        for (chip, token) in chips.chips.iter().zip(&row.chips[chips.first..]) {
            painter.blend(*chip, colors.chip);
            painter.text(
                token,
                ChromeTextStyle::Regular,
                *chip,
                PickerTextAlignment::Center,
                colors.chip_text,
            )?;
        }
    }
    if !trailing_chips && !row.trailing.is_empty() {
        painter.text(
            row.trailing,
            ChromeTextStyle::Regular,
            columns.trailing,
            PickerTextAlignment::Right,
            if row.trailing_accent {
                colors.accent
            } else {
                colors.secondary
            },
        )?;
    }
    Ok(())
}

/// The title in `clip`: one run, or the matched characters in the accent
/// and bold between regular runs, each at its own cells.
fn paint_title(
    painter: &mut ChromePainter<'_>,
    clip: Rect,
    title: &str,
    matched: Option<Range<usize>>,
    colors: &RowColors,
) -> Result<()> {
    if matched.is_none() {
        return painter.text(
            title,
            ChromeTextStyle::Regular,
            clip,
            PickerTextAlignment::Left,
            colors.title,
        );
    }
    let regular = painter.cell(ChromeTextStyle::Regular)?.0;
    let bold = painter.cell(ChromeTextStyle::Bold)?.0;
    let room_cells = clip.width / regular.max(bold).max(1);
    let end = clip.x.saturating_add(clip.width);
    let mut x = clip.x;
    for (text, matched) in title_runs(title, matched, room_cells) {
        let (style, cell_width, color) = if matched {
            (ChromeTextStyle::Bold, bold, colors.matched)
        } else {
            (ChromeTextStyle::Regular, regular, colors.title)
        };
        let width = text_cells(&text)
            .saturating_mul(cell_width)
            .min(end.saturating_sub(x));
        painter.text(
            &text,
            style,
            Rect { x, width, ..clip },
            PickerTextAlignment::Left,
            color,
        )?;
        x = x.saturating_add(width);
    }
    Ok(())
}

/// The selected, hovered or pressed fill under a row in `rect` (buffer px).
fn paint_row_fill(
    painter: &mut ChromePainter<'_>,
    palette: &SessionPickerPalette,
    rect: Rect,
    state: RowState,
) {
    if state.selected {
        painter.fill(rect, opaque(palette.selected_fill));
    } else if !state.disabled && state.hovered {
        let [red, green, blue, _] = opaque(palette.selected_fill);
        painter.blend(rect, [red, green, blue, ROW_HOVER_ALPHA]);
    }
    if !state.disabled && state.pressed {
        let [red, green, blue, _] = opaque(palette.selected_rail);
        painter.blend(rect, [red, green, blue, ROW_PRESS_ALPHA]);
    }
}

/// A picker row's name column, in cells, so the locations line up.
pub(super) const PICKER_NAME_CELLS: u32 = 13;
/// A location narrower than this many cells drops instead.
pub(super) const PICKER_LOCATION_MIN_CELLS: u32 = 8;
/// The agent state's dot, logical px square.
const PICKER_DOT_SIZE: u32 = 7;

/// One picker item's content: its name, where it is, and how it is.
pub(super) struct PickerItemRow<'a> {
    pub(super) name: &'a str,
    /// The characters the query matched in the name.
    pub(super) matched: Option<Range<usize>>,
    /// The Lair and the working directory, in secondary text.
    pub(super) location: &'a str,
    /// The status word at the end: the agent state, or the running panes.
    pub(super) status: &'a str,
    /// The agent state's dot before the word, in its colour.
    pub(super) dot: Option<u32>,
    /// The word in the dot's colour (needs input, error) rather than in
    /// secondary text.
    pub(super) status_tinted: bool,
    pub(super) state: RowState,
}

/// Where a picker row's parts go, in buffer px. A part that does not fit
/// is `None`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PickerItemColumns {
    pub(super) marker: Rect,
    pub(super) name: Rect,
    pub(super) location: Option<Rect>,
    /// The cell the agent dot sits in.
    pub(super) dot: Option<Rect>,
    pub(super) word: Option<Rect>,
}

/// A picker row's columns in `row` (buffer px): the marker and a cell of
/// space, the name in a [`PICKER_NAME_CELLS`] column, a cell, the
/// location, a cell, and the status at the right: the dot, a cell, the
/// word. A narrow row drops the location first, giving the name its room,
/// then the status word; the dot stays.
pub(super) fn picker_item_columns(
    row: Rect,
    padding: u32,
    cell_width: u32,
    name_cells: u32,
    has_location: bool,
    word_cells: u32,
    has_dot: bool,
) -> PickerItemColumns {
    let base = row_columns(row, padding, cell_width, 0, 0);
    let content = base.title;
    let left = content.x;
    let right = content.x.saturating_add(content.width);
    let cell = cell_width;
    let dot_width = if has_dot { cell } else { 0 };
    let word_width = word_cells.saturating_mul(cell);
    let dot_gap = if has_dot && word_width > 0 { cell } else { 0 };
    let full_status = dot_width.saturating_add(dot_gap).saturating_add(word_width);
    let with_gap = |status: u32| {
        if status > 0 {
            status.saturating_add(cell)
        } else {
            0
        }
    };
    let name_column = PICKER_NAME_CELLS.saturating_mul(cell);
    let name_minimum = name_cells.min(PICKER_NAME_CELLS).saturating_mul(cell);
    let location_need = name_column
        .saturating_add(cell)
        .saturating_add(PICKER_LOCATION_MIN_CELLS.saturating_mul(cell))
        .saturating_add(with_gap(full_status));
    let (show_location, show_word) = if has_location && content.width >= location_need {
        (true, true)
    } else if content.width >= name_minimum.saturating_add(with_gap(full_status)) {
        (false, true)
    } else {
        (false, false)
    };
    let word = (show_word && word_width > 0).then(|| Rect {
        x: right.saturating_sub(word_width).max(left),
        width: word_width.min(content.width),
        ..content
    });
    let dot_right = word.map_or(right, |word| word.x.saturating_sub(dot_gap));
    let dot = has_dot.then(|| Rect {
        x: dot_right.saturating_sub(dot_width).max(left),
        width: dot_width.min(dot_right.saturating_sub(left)),
        ..content
    });
    let status_left = dot.or(word).map_or(right, |status| status.x);
    let name_end = if dot.is_some() || word.is_some() {
        status_left.saturating_sub(cell).max(left)
    } else {
        right
    };
    let (name, location) = if show_location {
        let name = Rect {
            width: name_column.min(name_end.saturating_sub(left)),
            ..content
        };
        let location_x = left.saturating_add(name_column).saturating_add(cell);
        (
            name,
            Some(Rect {
                x: location_x,
                width: name_end.saturating_sub(location_x),
                ..content
            }),
        )
    } else {
        (
            Rect {
                width: name_end.saturating_sub(left),
                ..content
            },
            None,
        )
    };
    PickerItemColumns {
        marker: base.marker,
        name,
        location,
        dot,
        word,
    }
}

/// One picker item row in `rect` (buffer px): the row's fill, the `›`
/// when selected, the name with its matched characters, the location, and
/// the status with its dot.
pub(super) fn paint_picker_item_row(
    painter: &mut ChromePainter<'_>,
    palette: &SessionPickerPalette,
    rect: Rect,
    row: &PickerItemRow<'_>,
) -> Result<()> {
    let state = row.state;
    paint_row_fill(painter, palette, rect, state);
    let colors = row_colors(palette, state);
    let cell = painter.cell(ChromeTextStyle::Regular)?;
    let columns = picker_item_columns(
        rect,
        painter.scaled(ROW_PADDING),
        cell.0,
        text_cells(row.name),
        !row.location.is_empty(),
        text_cells(row.status),
        row.dot.is_some(),
    );
    if state.selected {
        painter.text(
            "›",
            ChromeTextStyle::Regular,
            columns.marker,
            PickerTextAlignment::Left,
            colors.marker,
        )?;
    }
    paint_title(
        painter,
        columns.name,
        row.name,
        row.matched.clone(),
        &colors,
    )?;
    if let Some(location) = columns.location {
        painter.text(
            row.location,
            ChromeTextStyle::Regular,
            location,
            PickerTextAlignment::Left,
            colors.secondary,
        )?;
    }
    if let (Some(dot), Some(color)) = (columns.dot, row.dot) {
        let size = painter
            .scaled(PICKER_DOT_SIZE)
            .min(dot.width)
            .min(dot.height);
        painter.fill(
            Rect {
                x: dot.x.saturating_add(dot.width.saturating_sub(size) / 2),
                y: dot.y.saturating_add(dot.height.saturating_sub(size) / 2),
                width: size,
                height: size,
            },
            opaque(color),
        );
    }
    if let Some(word) = columns.word {
        painter.text(
            row.status,
            ChromeTextStyle::Regular,
            word,
            PickerTextAlignment::Right,
            match row.dot {
                Some(color) if row.status_tinted => color,
                _ => colors.secondary,
            },
        )?;
    }
    Ok(())
}

/// A group header in `rect` (buffer px): `label` in secondary text where
/// the rows' titles start, and a rule from after it to the row's right
/// padding.
pub(super) fn paint_group_header(
    painter: &mut ChromePainter<'_>,
    palette: &SessionPickerPalette,
    rect: Rect,
    label: &str,
) -> Result<()> {
    let cell = painter.cell(ChromeTextStyle::Regular)?;
    let columns = row_columns(rect, painter.scaled(ROW_PADDING), cell.0, 0, 0);
    let title = columns.title;
    let label_width = text_cells(label).saturating_mul(cell.0).min(title.width);
    painter.text(
        label,
        ChromeTextStyle::Regular,
        Rect {
            width: label_width,
            ..title
        },
        PickerTextAlignment::Left,
        palette.secondary,
    )?;
    let rule_x = title.x.saturating_add(label_width).saturating_add(cell.0);
    let right = title.x.saturating_add(title.width);
    if rule_x < right {
        let border = painter.border();
        painter.fill(
            Rect {
                x: rule_x,
                y: rect.y.saturating_add(rect.height / 2),
                width: right - rule_x,
                height: border,
            },
            opaque(palette.frame),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::RenderContext;

    use super::super::picker::{SessionPickerTextCache, picker_buffer_rect};

    fn tokens(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|key| (*key).to_owned()).collect()
    }

    /// Rows are 28 px and headers 24 at every scale; the marker, title and
    /// trailing part keep their padding and never overlap or leave the
    /// row, at each UI text size.
    #[test]
    fn row_columns_stay_inside_the_row_at_every_scale() {
        for scale in [120, 150, 180, 240] {
            for context in [
                RenderContext::new(u16::MAX),
                RenderContext::new(u16::MAX).at_maximum_chrome_font_size(),
            ] {
                let mut cache = SessionPickerTextCache::default();
                let cell = cache
                    .cell_metrics(&context, ChromeTextStyle::Regular, scale, 1)
                    .unwrap();
                let padding = ROW_PADDING.saturating_mul(scale).div_ceil(120);
                for width in [120, 300, 540, 620] {
                    let logical = Rect {
                        x: 33,
                        y: 101,
                        width,
                        height: ROW_HEIGHT,
                    };
                    let row = picker_buffer_rect(logical, scale);
                    assert_eq!(row.height, ROW_HEIGHT * scale / 120, "scale {scale}");
                    for (leading, trailing) in [
                        (0, 0),
                        (0, 9 * cell.0),
                        (KEY_COLUMN_CELLS * cell.0, 4 * cell.0),
                    ] {
                        let columns = row_columns(row, padding, cell.0, leading, trailing);
                        let right = row.x + row.width;
                        assert!(columns.marker.x >= row.x + padding.min(row.width / 4));
                        assert!(columns.marker.x + columns.marker.width <= columns.title.x);
                        if leading > 0 {
                            assert!(columns.chips.x + columns.chips.width <= columns.title.x);
                            assert!(columns.chips.width <= leading);
                        }
                        assert!(columns.title.x + columns.title.width <= columns.trailing.x);
                        assert!(columns.trailing.x + columns.trailing.width <= right);
                        if trailing > 0 && columns.trailing.width > 0 {
                            assert!(
                                columns.title.x + columns.title.width + cell.0
                                    <= columns.trailing.x
                                    || columns.title.width == 0
                            );
                        }
                        for part in [
                            columns.marker,
                            columns.chips,
                            columns.title,
                            columns.trailing,
                        ] {
                            assert_eq!((part.y, part.height), (row.y, row.height));
                        }
                    }
                }
                let header = picker_buffer_rect(
                    Rect {
                        x: 0,
                        y: 0,
                        width: 300,
                        height: GROUP_HEIGHT,
                    },
                    scale,
                );
                assert!(cell.1 <= header.height, "scale {scale}");
            }
        }
    }

    /// Chips never run past their room: keys drop from the end behind a
    /// `…`, and the chips are 3 px apart with 5 px inside each side.
    #[test]
    fn chips_drop_modifiers_from_the_front_and_never_run_past_the_row() {
        let keys = tokens(&["ctrl", "shift", "↵"]);
        for scale in [120, 150, 180, 240] {
            let cell = (8 * scale / 120, 17 * scale / 120);
            let padding = (CHIP_PADDING * scale).div_ceil(120);
            let gap = CHIP_GAP.saturating_mul(scale).div_ceil(120);
            let natural = chips_width(&keys, cell, scale);
            for room_width in (0..natural + 40).step_by(3) {
                let room = Rect {
                    x: 50,
                    y: 10,
                    width: room_width,
                    height: ROW_HEIGHT * scale / 120,
                };
                for right_aligned in [false, true] {
                    let layout = chip_layout(&keys, room, cell, scale, right_aligned);
                    let mut parts = layout.chips.clone();
                    parts.extend(layout.ellipsis);
                    for part in &parts {
                        assert!(part.x >= room.x, "scale {scale}, room {room_width}");
                        assert!(part.x + part.width <= room.x + room.width);
                        assert!(part.y >= room.y && part.y + part.height <= room.y + room.height);
                    }
                    for pair in layout.chips.windows(2) {
                        assert_eq!(pair[1].x, pair[0].x + pair[0].width + gap);
                    }
                    if let (Some(first), Some(ellipsis)) = (layout.chips.first(), layout.ellipsis) {
                        assert_eq!(first.x, ellipsis.x + ellipsis.width + gap);
                    }
                    assert!(layout.width <= room.width);
                    // The shown keys are the last ones: the key survives.
                    assert_eq!(layout.first + layout.chips.len(), keys.len());
                    // `… shift ↵`, `… ↵`, then `↵` alone, then nothing.
                    let alone = cell.0 + 2 * padding;
                    if layout.chips.len() == keys.len() {
                        assert_eq!(layout.width, natural);
                        assert!(layout.ellipsis.is_none());
                    } else {
                        assert!(room_width < natural);
                        assert_eq!(
                            layout.ellipsis.is_some(),
                            room_width >= cell.0 + gap + alone
                        );
                    }
                    assert_eq!(
                        layout.chips.is_empty(),
                        room_width < alone,
                        "the last key shows while it fits alone"
                    );
                    if right_aligned && layout.width > 0 {
                        assert_eq!(
                            parts.iter().map(|part| part.x + part.width).max(),
                            Some(room.x + room.width)
                        );
                    }
                }
            }
            // Each chip is its key plus the padding each side.
            let layout = chip_layout(
                &keys,
                Rect {
                    x: 0,
                    y: 0,
                    width: natural,
                    height: cell.1,
                },
                cell,
                scale,
                false,
            );
            assert_eq!(layout.chips[0].width, 4 * cell.0 + 2 * padding);
            assert_eq!(layout.chips[2].width, cell.0 + 2 * padding);
        }
        assert!(
            chip_layout(
                &[],
                Rect {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 0
                },
                (8, 17),
                120,
                false
            )
            .chips
            .is_empty()
        );
    }

    /// A picker row narrows by dropping its location first, then its
    /// status word; the agent dot stays. No part overlaps another or runs
    /// past the row, and the name keeps its room while the word shows.
    #[test]
    fn picker_rows_drop_the_location_then_the_status_word() {
        let right = |rect: Rect| rect.x + rect.width;
        for scale in [120, 150, 180, 240] {
            let cell = 8 * scale / 120;
            let padding = ROW_PADDING * scale / 120;
            let (mut had_location, mut had_word) = (true, true);
            for logical in (40..=600).rev() {
                let row = Rect {
                    x: 7,
                    y: 3,
                    width: logical * scale / 120,
                    height: ROW_HEIGHT * scale / 120,
                };
                // `build` · `m5a · ~/src/splinterm` · ● `working`.
                let columns = picker_item_columns(row, padding, cell, 5, true, 7, true);
                let dot = columns.dot.expect("the dot stays");
                assert!(right(dot) <= right(row) - padding.min(row.width / 4));
                assert!(columns.name.x >= right(columns.marker));
                assert!(
                    right(columns.name) <= dot.x,
                    "scale {scale} width {logical}"
                );
                if let Some(location) = columns.location {
                    assert!(had_location, "a dropped location came back at {logical}");
                    assert!(columns.word.is_some(), "the word drops after the location");
                    assert_eq!(columns.name.width, PICKER_NAME_CELLS * cell);
                    assert!(location.x >= right(columns.name) + cell);
                    assert!(right(location) + cell <= dot.x);
                    assert!(location.width >= PICKER_LOCATION_MIN_CELLS * cell);
                }
                if let Some(word) = columns.word {
                    assert!(had_word, "a dropped word came back at {logical}");
                    assert_eq!(word.width, 7 * cell);
                    assert_eq!(right(dot) + cell, word.x);
                    assert!(right(word) <= right(row));
                    assert!(columns.name.width >= 5 * cell);
                }
                had_location = columns.location.is_some();
                had_word = columns.word.is_some();
                if logical == 560 {
                    assert!(had_location && had_word, "a 560 px row shows everything");
                }
            }
            assert!(!had_location && !had_word, "a 40 px row keeps only the dot");
        }
        // No agent: the running count is the word, and it drops with no
        // dot left behind.
        let row = Rect {
            x: 0,
            y: 0,
            width: 120,
            height: ROW_HEIGHT,
        };
        let columns = picker_item_columns(row, ROW_PADDING, 8, 10, true, 3, false);
        assert_eq!(columns.dot, None);
        assert!(columns.location.is_none() && columns.word.is_none());
        assert_eq!(right(columns.name), right(row) - ROW_PADDING);
    }

    /// `ctrl shift p` narrows to `… shift p`, then `… p`, then `p` alone:
    /// the final key survives while it fits at all.
    #[test]
    fn the_final_key_survives_a_narrow_row() {
        let keys = tokens(&["ctrl", "shift", "p"]);
        let cell = (8, 17);
        // At 1×: chips 42, 50 and 18 px wide, 3 px apart; `…` is a cell.
        let shown = |width| {
            let layout = chip_layout(
                &keys,
                Rect {
                    x: 0,
                    y: 0,
                    width,
                    height: 28,
                },
                cell,
                120,
                true,
            );
            let mut text = layout.ellipsis.map(|_| vec!["…"]).unwrap_or_default();
            text.extend(keys[layout.first..].iter().map(String::as_str));
            assert_eq!(layout.chips.len(), keys.len() - layout.first);
            text.join(" ")
        };
        assert_eq!(shown(116), "ctrl shift p");
        assert_eq!(shown(115), "… shift p");
        assert_eq!(shown(82), "… shift p");
        assert_eq!(shown(81), "… p");
        assert_eq!(shown(29), "… p");
        assert_eq!(shown(28), "p");
        assert_eq!(shown(18), "p");
        assert_eq!(shown(17), "");
    }

    /// A chip's key keeps 4.5:1 on the chip's composed colour, on the
    /// panel and on the selection, in every theme.
    #[test]
    fn chip_keys_keep_the_text_floor_on_their_chips() {
        use crate::color::{contrast_ratio, packed_channels};
        use crate::renderer::{ChromeSurface, chrome_panel_palette};

        for (name, theme) in crate::config::tests::provider_themes() {
            let palette = chrome_panel_palette(theme, ChromeSurface::Menu);
            for selected in [false, true] {
                let state = RowState {
                    selected,
                    ..RowState::default()
                };
                let colors = row_colors(&palette, state);
                let base = if selected {
                    palette.selected_fill
                } else {
                    palette.panel
                };
                let [red, green, blue, alpha] = colors.chip;
                let chip = mix_encoded(
                    base,
                    packed_channels([red, green, blue]),
                    f64::from(alpha) / 255.0,
                );
                let ratio = contrast_ratio(colors.chip_text, chip);
                assert!(ratio >= 4.5, "{name} selected {selected}: {ratio:.2}");
            }
        }
    }

    #[test]
    fn titles_split_into_matched_runs_and_end_in_an_ellipsis() {
        let title = "Split pane horizontally";
        assert_eq!(title_match(title, "PANE"), Some(6..10));
        assert_eq!(title_match(title, "  "), None);
        assert_eq!(title_match(title, "zoom"), None);
        let runs = |matched, room| title_runs(title, matched, room);
        assert_eq!(
            runs(Some(6..10), 40),
            vec![
                ("Split ".to_owned(), false),
                ("pane".to_owned(), true),
                (" horizontally".to_owned(), false)
            ]
        );
        // Cut inside the match: the match keeps what fits, then `…`.
        assert_eq!(
            runs(Some(6..10), 9),
            vec![
                ("Split ".to_owned(), false),
                ("pa".to_owned(), true),
                ("…".to_owned(), false)
            ]
        );
        assert_eq!(runs(Some(6..10), 5), vec![("Spli…".to_owned(), false)]);
        assert_eq!(runs(None, 40), vec![(title.to_owned(), false)]);
        assert!(runs(None, 0).is_empty());
        for room in 1..30 {
            let cells = runs(Some(0..5), room)
                .iter()
                .map(|(text, _)| text_cells(text))
                .sum::<u32>();
            assert!(cells <= room, "room {room}");
        }
    }

    /// Group headers sit above the row that starts each group; the window
    /// keeps the selected row, at most ten rows, and its height in budget.
    #[test]
    fn the_window_keeps_the_selected_row_and_counts_group_headers() {
        let groups = ["app", "app", "dojo", "dojo", "dojo", "pane", "pane", "view"];
        assert!(starts_group(&groups, 0) && !starts_group(&groups, 1));
        assert!(starts_group(&groups, 2) && starts_group(&groups, 7));
        assert!(!starts_group(&[], 0));
        assert_eq!(
            window_height(&groups, 0, 3),
            3 * ROW_HEIGHT + 2 * GROUP_HEIGHT
        );
        assert_eq!(window_height(&[], 0, 3), 3 * ROW_HEIGHT);
        let all = window_height(&groups, 0, groups.len());
        assert_eq!(
            visible_window(groups.len(), &groups, 3, 0, all),
            0..groups.len()
        );
        for budget in [ROW_HEIGHT, 100, 160, 200, 280] {
            for selected in 0..groups.len() {
                for requested in 0..groups.len() {
                    let window = visible_window(groups.len(), &groups, selected, requested, budget);
                    assert!(
                        window.contains(&selected),
                        "{budget} {selected} {requested}"
                    );
                    assert!(window.len() <= LIST_MAXIMUM_ROWS);
                    assert!(
                        window_height(&groups, window.start, window.end) <= budget
                            || window.len() == 1
                    );
                }
            }
        }
        assert_eq!(visible_window(0, &[], 0, 0, 280), 0..0);
        assert_eq!(visible_window(5, &[], 0, 0, ROW_HEIGHT - 1), 0..0);
        assert_eq!(visible_window(40, &[], 39, 0, 1000), 30..40);
        assert_eq!(visible_window(40, &[], 3, 2, 1000), 2..12);
    }
}
