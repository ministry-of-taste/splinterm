//! Responsive layout and CPU composition for the non-modal Lair explorer.
//!
//! One line per row: the current-row dot, the disclosure chevron, and the
//! name on the left, and fixed, right-aligned icon columns for agent access,
//! agent status, live state, and retention. The selected row's status is
//! spelled out in words in the footer, which also carries the search field,
//! navigation notices, and who owns keyboard input.

use std::collections::HashSet;

use anyhow::Result;
use unicode_width::UnicodeWidthChar;

use crate::{
    config::ResolvedTheme,
    frontend::{LairExplorerAccess, LairExplorerFilter, LairExplorerRow},
    geometry::Rect,
    navigation_projection::NavigationNodeId,
};
use splinterm_core::LairRetention;

use super::{
    super::{
        ChromeIcon, ChromeSource, ChromeTextStyle, RenderContext, chrome_icons_available,
        fill_rect, premultiplied_theme_rgba, raster::premultiplied_rgba,
    },
    picker::{
        ChromeSurface, PickerTextAlignment, SessionPickerPalette, SessionPickerTextCache,
        chrome_panel_palette, editable_text, paint_picker_source, paint_picker_source_within,
        paint_picker_text, picker_cell_width, truncate_picker_text,
    },
};

const PREFERRED_WIDTH: u32 = 288;
/// The title and the live / saved / all filters share one header line, a
/// row tall unless the header sits beside the tab strip and takes its height.
const HEADER_HEIGHT: u32 = ROW_HEIGHT;
/// The gap between the header line and the first row. The header draws no
/// rule or fill of its own, so this space is what sets it apart.
const HEADER_SPACER: u32 = 4;
/// The header title's share of the panel before the filters begin.
const HEADER_TITLE_WIDTH: u32 = 84;
/// One chrome text line: tall enough for the chrome line cap.
const FOOTER_LINE_HEIGHT: u32 = 24;
/// Two lines of the selected row's status words and one context line.
const FOOTER_STATUS_LINES: usize = 2;
const FOOTER_PADDING: u32 = 4;
const FOOTER_HEIGHT: u32 = FOOTER_LINE_HEIGHT * 3 + FOOTER_PADDING;
const ROW_HEIGHT: u32 = 26;
/// Logical pixels each tree level indents a row's name.
const LEVEL_INDENT: u32 = 12;
/// The fewest terminal columns the docked Explorer leaves beside itself.
const MINIMUM_DOCKED_TERMINAL_COLUMNS: u16 = 20;
/// The docking floor never exceeds this many logical pixels, whatever the zoom.
const MAXIMUM_DOCKED_TERMINAL_FLOOR: u32 = 240;

/// The narrowest terminal width, in logical pixels, that keeps the Explorer
/// docked beside the terminal; narrower Windows get the drawer.
///
/// The drawer's opaque panel covers the terminal's leftmost columns, where
/// every prompt and line starts, so a terminal under it looks blank while it
/// still takes input. Docking therefore needs only a small usable terminal,
/// not the configured initial grid (80 columns by default), which sent an
/// ordinary half-width tiled Window to the drawer.
///
/// The drawer exposes the same strip beside the panel that docking would
/// give the terminal, only with line starts hidden, so a docked terminal
/// always shows at least as much. The floor is therefore capped at a fixed
/// logical width: zoomed-in cells must not send a wide Window to the drawer.
#[must_use]
pub(crate) fn explorer_minimum_docked_terminal_width(cell_width: u32, initial_columns: u16) -> u32 {
    cell_width
        .max(1)
        .saturating_mul(u32::from(
            initial_columns.min(MINIMUM_DOCKED_TERMINAL_COLUMNS),
        ))
        .min(MAXIMUM_DOCKED_TERMINAL_FLOOR)
}

const HEADER_TEXT: &str = "lairs";
/// Painted between neighbouring header filters.
const FILTER_SEPARATOR: &str = "·";

/// The footer's context line when no search or notice takes it: who owns
/// keyboard input and how to move.
const fn explorer_hint_text(focused: bool) -> &'static str {
    if focused {
        "↑↓ move · F filter · Esc terminal"
    } else {
        "Click rows · terminal input active"
    }
}

/// The footer's context line: the search field while a search is open, so
/// typing is never blind; else a navigation notice that the list does not
/// show; else a query left behind after leaving the search; else the input
/// hint.
fn explorer_context_text(
    focused: bool,
    searching: bool,
    query: &str,
    notice: Option<&str>,
    maximum_cells: usize,
) -> String {
    if focused && searching {
        editable_text("Search: ", query, maximum_cells)
    } else if let Some(notice) = notice {
        notice.to_owned()
    } else if !query.is_empty() {
        format!("Search: {query}")
    } else {
        explorer_hint_text(focused).to_owned()
    }
}

/// The selected row's status in words: its name, then its status.
fn explorer_status_words(row: &LairExplorerRow) -> String {
    if row.status.is_empty() {
        row.label.clone()
    } else {
        format!("{} · {}", row.label, row.status)
    }
}

fn text_cells(text: &str) -> usize {
    text.chars()
        .map(|character| character.width().unwrap_or(0).min(2))
        .sum()
}

/// Lays `text` out in at most `lines` lines of at most `cells` cells,
/// breaking only between words. A ` · ` separator never starts a line. Text
/// that does not fit ends in `…` after the last whole word that fits; only a
/// word wider than a whole line is cut, at a character boundary.
fn wrap_status_words(text: &str, cells: usize, lines: usize) -> Vec<String> {
    let mut wrapped = Vec::new();
    if cells == 0 || lines == 0 {
        return wrapped;
    }
    // Each word with the separator before it: " · " after a "·" token,
    // otherwise one space. A line drops the separator of its first word.
    let mut words = Vec::new();
    let mut separated = false;
    for word in text.split(' ').filter(|word| !word.is_empty()) {
        if word == "·" {
            separated = true;
        } else {
            words.push((if separated { " · " } else { " " }, word));
            separated = false;
        }
    }
    let joined = |line: &[(&str, &str)]| {
        let mut text = String::new();
        for (index, (separator, word)) in line.iter().enumerate() {
            if index > 0 {
                text.push_str(separator);
            }
            text.push_str(word);
        }
        text
    };
    let mut line: Vec<(&str, &str)> = Vec::new();
    for (separator, word) in words {
        if !line.is_empty() {
            let mut candidate = line.clone();
            candidate.push((separator, word));
            if text_cells(&joined(&candidate)) <= cells {
                line = candidate;
                continue;
            }
            if wrapped.len() + 1 == lines {
                // The last line: end on a whole word and an ellipsis. A
                // single word that fills the line stays whole instead.
                while line.len() > 1 && text_cells(&joined(&line)) >= cells {
                    line.pop();
                }
                let shown = joined(&line);
                wrapped.push(if text_cells(&shown) < cells {
                    format!("{shown}…")
                } else {
                    shown
                });
                return wrapped;
            }
            wrapped.push(joined(&line));
            line.clear();
        }
        if text_cells(word) <= cells {
            line.push((separator, word));
            continue;
        }
        // A word wider than a whole line: the only cut inside a word.
        wrapped.push(truncate_picker_text(word, cells));
        if wrapped.len() == lines {
            return wrapped;
        }
    }
    if !line.is_empty() {
        wrapped.push(joined(&line));
    }
    wrapped
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LairExplorerPresentationMode {
    Docked,
    Drawer,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LairExplorerRowLayout {
    pub(crate) id: NavigationNodeId,
    pub(crate) rect: Rect,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LairExplorerLayout {
    pub(crate) panel: Rect,
    pub(crate) terminal: Rect,
    pub(crate) header: Rect,
    /// The tallest header text cell in buffer px, or `u32::MAX` for the
    /// chrome line cap. A header beside the tab strip sizes its text as the
    /// tab labels are, so both share one baseline.
    pub(crate) header_text_cell_cap: u32,
    pub(crate) footer: Rect,
    pub(crate) filters: Vec<(LairExplorerFilter, Rect)>,
    pub(crate) rows: Vec<LairExplorerRowLayout>,
    pub(crate) visible_start: usize,
    pub(crate) visible_capacity: usize,
    pub(crate) mode: LairExplorerPresentationMode,
}

#[must_use]
pub(crate) fn lair_explorer_layout(
    content: Rect,
    minimum_terminal_width: u32,
    rows: &[LairExplorerRow],
    selected: Option<NavigationNodeId>,
    requested_start: usize,
) -> Option<LairExplorerLayout> {
    lair_explorer_layout_with_header(
        content,
        HEADER_HEIGHT,
        minimum_terminal_width,
        rows,
        selected,
        requested_start,
    )
}

/// The Explorer's layout with a header line `header` logical px tall, such
/// as the tab strip's height for a docked Explorer that runs beside it.
#[must_use]
pub(crate) fn lair_explorer_layout_with_header(
    content: Rect,
    header: u32,
    minimum_terminal_width: u32,
    rows: &[LairExplorerRow],
    selected: Option<NavigationNodeId>,
    requested_start: usize,
) -> Option<LairExplorerLayout> {
    if content.width == 0 || content.height == 0 || header == 0 {
        return None;
    }
    let width = PREFERRED_WIDTH.min(content.width);
    let mode = if content.width.saturating_sub(width) >= minimum_terminal_width {
        LairExplorerPresentationMode::Docked
    } else {
        LairExplorerPresentationMode::Drawer
    };
    let panel = Rect { width, ..content };
    let terminal = match mode {
        LairExplorerPresentationMode::Docked => Rect {
            x: content.x.saturating_add(width),
            y: content.y,
            width: content.width.saturating_sub(width),
            height: content.height,
        },
        LairExplorerPresentationMode::Drawer => content,
    };
    let header_height = header.min(panel.height);
    let footer_height = FOOTER_HEIGHT.min(panel.height.saturating_sub(header_height));
    let list_top = header_height
        .saturating_add(HEADER_SPACER)
        .min(panel.height.saturating_sub(footer_height));
    let list_height = panel
        .height
        .saturating_sub(list_top)
        .saturating_sub(footer_height);
    let capacity = usize::try_from(list_height / ROW_HEIGHT).unwrap_or(0);
    let selected_index = selected.and_then(|id| rows.iter().position(|row| row.id == id));
    let maximum_start = rows.len().saturating_sub(capacity);
    let mut start = requested_start.min(maximum_start);
    if let Some(index) = selected_index {
        if index < start {
            start = index;
        } else if index >= start.saturating_add(capacity) {
            start = index.saturating_add(1).saturating_sub(capacity);
        }
    }
    let row_layouts = rows
        .iter()
        .skip(start)
        .take(capacity)
        .enumerate()
        .map(|(slot, row)| LairExplorerRowLayout {
            id: row.id,
            rect: Rect {
                x: panel.x,
                y: panel.y.saturating_add(list_top).saturating_add(
                    ROW_HEIGHT.saturating_mul(u32::try_from(slot).unwrap_or(u32::MAX)),
                ),
                width: panel.width,
                height: ROW_HEIGHT,
            },
        })
        .collect();
    Some(LairExplorerLayout {
        panel,
        terminal,
        header: Rect {
            x: panel.x,
            y: panel.y,
            width: panel.width,
            height: header_height,
        },
        header_text_cell_cap: u32::MAX,
        footer: Rect {
            x: panel.x,
            y: panel
                .y
                .saturating_add(panel.height.saturating_sub(footer_height)),
            width: panel.width,
            height: footer_height,
        },
        filters: if header_height == header {
            header_filter_rects(panel, header_height)
        } else {
            Vec::new()
        },
        rows: row_layouts,
        visible_start: start,
        visible_capacity: capacity,
        mode,
    })
}

/// The filters take the header line right of the title, with the panel's
/// inset on their right. Each one's target is its whole share of the line,
/// `height` logical px tall.
fn header_filter_rects(panel: Rect, height: u32) -> Vec<(LairExplorerFilter, Rect)> {
    let title = HEADER_TITLE_WIDTH.min(panel.width.saturating_mul(3) / 10);
    let inset = 8.min(panel.width / 2);
    let available = panel.width.saturating_sub(title).saturating_sub(inset);
    LairExplorerFilter::ALL
        .into_iter()
        .enumerate()
        .map(|(index, filter)| {
            let index = u32::try_from(index).unwrap_or(0);
            let left = available * index / 3;
            let right = available * (index + 1) / 3;
            (
                filter,
                Rect {
                    x: panel.x.saturating_add(title).saturating_add(left),
                    y: panel.y,
                    width: right - left,
                    height,
                },
            )
        })
        .collect()
}

#[must_use]
pub(crate) fn lair_explorer_filter_hit_test(
    layout: &LairExplorerLayout,
    position: (f64, f64),
) -> Option<LairExplorerFilter> {
    layout.filters.iter().find_map(|(filter, rect)| {
        (position.0 >= f64::from(rect.x)
            && position.0 < f64::from(rect.x.saturating_add(rect.width))
            && position.1 >= f64::from(rect.y)
            && position.1 < f64::from(rect.y.saturating_add(rect.height)))
        .then_some(*filter)
    })
}

#[must_use]
pub(crate) fn lair_explorer_hit_test(
    layout: &LairExplorerLayout,
    position: (f64, f64),
) -> Option<NavigationNodeId> {
    layout.rows.iter().find_map(|row| {
        let right = f64::from(row.rect.x.saturating_add(row.rect.width));
        let bottom = f64::from(row.rect.y.saturating_add(row.rect.height));
        (position.0 >= f64::from(row.rect.x)
            && position.0 < right
            && position.1 >= f64::from(row.rect.y)
            && position.1 < bottom)
            .then_some(row.id)
    })
}

/// Whether a row in view shows a fresh working status, whose spinner moves.
/// Rows scrolled out of view do not count.
#[must_use]
pub(crate) fn lair_explorer_shows_working(
    layout: &LairExplorerLayout,
    rows: &[LairExplorerRow],
) -> bool {
    rows.iter()
        .skip(layout.visible_start)
        .take(layout.rows.len())
        .any(|row| row.agent_working)
}

/// The logical width from a row's left edge that toggles its disclosure: the
/// row's indent and the current-row dot and chevron after it, with room for
/// the largest chrome cell.
fn disclosure_width(level: usize) -> u32 {
    LEVEL_INDENT.saturating_mul(u32::try_from(level).unwrap_or(u32::MAX).saturating_add(3))
}

#[must_use]
pub(crate) fn lair_explorer_disclosure_hit_test(
    layout: &LairExplorerLayout,
    rows: &[LairExplorerRow],
    position: (f64, f64),
) -> Option<NavigationNodeId> {
    layout
        .rows
        .iter()
        .zip(rows.iter().skip(layout.visible_start))
        .find_map(|(layout_row, row)| {
            row.expanded?;
            let bottom = f64::from(layout_row.rect.y.saturating_add(layout_row.rect.height));
            let right = f64::from(
                layout_row
                    .rect
                    .x
                    .saturating_add(disclosure_width(row.level).min(layout_row.rect.width)),
            );
            (position.0 >= f64::from(layout_row.rect.x)
                && position.0 < right
                && position.1 >= f64::from(layout_row.rect.y)
                && position.1 < bottom)
                .then_some(row.id)
        })
}

/// A row's name: a current-row dot, a two-cell disclosure slot, and the
/// label. The disclosure chevrons are icons; ASCII `>` and `v` stand in only
/// if the embedded icon face fails.
fn row_name_source(row: &LairExplorerRow, icons: bool) -> ChromeSource {
    let mut source = ChromeSource::default();
    if row.pending {
        source.push_str("  …");
    } else {
        source.push_str(if row.current && row.expanded.is_some() {
            "●"
        } else {
            " "
        });
        match (row.expanded, icons) {
            (Some(expanded), true) => {
                source.push_icon(if expanded {
                    ChromeIcon::ChevronDown
                } else {
                    ChromeIcon::ChevronRight
                });
            }
            (Some(expanded), false) => {
                source.push_str(if expanded { " v" } else { " >" });
            }
            // A Splint has no disclosure; its current dot sits in the slot.
            (None, _) => {
                source.push_str(if row.current { " ●" } else { "  " });
            }
        }
    }
    source.push_str(" ").push_str(&row.label);
    source
}

/// Which icon columns the rows in view use. Columns sit at the same place on
/// every row; a column no row uses is left out.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ExplorerColumns {
    access: bool,
    /// Cells of the agent status column, when any Splint in view has one.
    agent: Option<usize>,
    retention: bool,
}

impl ExplorerColumns {
    fn of(rows: &[LairExplorerRow]) -> Self {
        Self {
            access: rows.iter().any(|row| row.access.is_some()),
            agent: rows
                .iter()
                .filter_map(|row| row.agent_column.as_ref())
                .map(ChromeSource::cells)
                .max(),
            retention: rows.iter().any(|row| row.retention.is_some()),
        }
    }
}

fn push_access(source: &mut ChromeSource, access: Option<LairExplorerAccess>, icons: bool) {
    match (access, icons) {
        (None, _) => source.push_str("  "),
        (Some(LairExplorerAccess::Reachable), true) => source.push_icon(ChromeIcon::Plug),
        (Some(LairExplorerAccess::Connected), true) => source.push_icon(ChromeIcon::PlugConnected),
        (Some(LairExplorerAccess::Asking), true) => source.push_icon(ChromeIcon::LockQuestion),
        (Some(LairExplorerAccess::Reachable), false) => source.push_str("a "),
        (Some(LairExplorerAccess::Connected), false) => source.push_str("A "),
        (Some(LairExplorerAccess::Asking), false) => source.push_str("A?"),
    };
}

fn push_retention(source: &mut ChromeSource, retention: Option<LairRetention>, icons: bool) {
    match (retention, icons) {
        (None | Some(LairRetention::Disposable), _) => source.push_str("  "),
        (Some(LairRetention::Saved), true) => source.push_icon(ChromeIcon::Pin),
        (Some(LairRetention::Pinned), true) => source.push_icon(ChromeIcon::Pinned),
        (Some(LairRetention::Saved), false) => source.push_str("s "),
        (Some(LairRetention::Pinned), false) => source.push_str("p "),
    };
}

/// A row's icon columns, left to right: agent access, agent status, the live
/// dot (filled while running or starting, hollow otherwise), and retention,
/// one blank cell apart. Also returns the first cell holding a mark, so the
/// name may run over columns this row leaves blank but never reaches a mark.
fn row_columns_source(
    row: &LairExplorerRow,
    columns: ExplorerColumns,
    icons: bool,
) -> (ChromeSource, usize) {
    let mut source = ChromeSource::default();
    let mut first_mark = None;
    if columns.access {
        if row.access.is_some() {
            first_mark = Some(0);
        }
        push_access(&mut source, row.access, icons);
        source.push_str(" ");
    }
    if let Some(cells) = columns.agent {
        let start = source.cells();
        match &row.agent_column {
            Some(column) => {
                if first_mark.is_none() && !column.is_blank() {
                    first_mark = Some(start);
                }
                source.push_source(column);
            }
            None => {
                source.push_str(&" ".repeat(cells));
            }
        }
        // Pad a narrower column so every row's later columns line up.
        let used = source.cells() - start;
        source.push_str(&" ".repeat(cells.saturating_sub(used)));
        source.push_str(" ");
    }
    let live = source.cells();
    source.push_str(if row.live { "●" } else { "○" });
    if columns.retention {
        source.push_str(" ");
        push_retention(&mut source, row.retention, icons);
    }
    (source, first_mark.unwrap_or(live))
}

/// A row's name and icon-column clips: the columns end at the row's right
/// inset, and the name ends one cell before the row's first mark.
fn row_text_clips(
    rect: Rect,
    level: usize,
    inset: u32,
    cell_width: u32,
    column_cells: usize,
    first_mark: usize,
) -> (Rect, Rect) {
    let indent = inset
        .saturating_mul(u32::try_from(level.max(1)).unwrap_or(u32::MAX))
        .min(rect.width);
    let left = rect.x.saturating_add(indent);
    let right = rect
        .x
        .saturating_add(rect.width.saturating_sub(inset))
        .max(left);
    let cells = |count: usize| cell_width.saturating_mul(u32::try_from(count).unwrap_or(u32::MAX));
    let columns_left = right.saturating_sub(cells(column_cells)).max(left);
    let name_right = columns_left
        .saturating_add(cells(first_mark))
        .saturating_sub(cell_width)
        .clamp(left, right);
    (
        Rect {
            x: left,
            y: rect.y,
            width: name_right - left,
            height: rect.height,
        },
        Rect {
            x: columns_left,
            y: rect.y,
            width: right - columns_left,
            height: rect.height,
        },
    )
}

/// The clip the icon columns are painted with: their cells and the row's
/// right inset, up to the panel divider. The last glyph may be wider than its
/// cell (the hollow live dot in some fonts); clipped at the cell it read as a
/// "C".
fn columns_paint_clip(columns: Rect, divider: u32) -> Rect {
    Rect {
        width: divider.saturating_sub(columns.x),
        ..columns
    }
}

fn buffer_rect(rect: Rect, scale_120: u32) -> Rect {
    let left = rect.x.saturating_mul(scale_120).div_ceil(120);
    let top = rect.y.saturating_mul(scale_120).div_ceil(120);
    let right = rect
        .x
        .saturating_add(rect.width)
        .saturating_mul(scale_120)
        .div_ceil(120);
    let bottom = rect
        .y
        .saturating_add(rect.height)
        .saturating_mul(scale_120)
        .div_ceil(120);
    Rect {
        x: left,
        y: top,
        width: right.saturating_sub(left),
        height: bottom.saturating_sub(top),
    }
}

/// The footer's text lines in buffer pixels: the status lines, then the
/// context line, below the footer's padding and its top rule.
fn footer_lines(footer: Rect, scale_120: u32, inset: u32) -> Vec<Rect> {
    let line = FOOTER_LINE_HEIGHT.saturating_mul(scale_120).div_ceil(120);
    let top = footer
        .y
        .saturating_add(FOOTER_PADDING.saturating_mul(scale_120).div_ceil(120));
    let bottom = footer.y.saturating_add(footer.height);
    (0..=u32::try_from(FOOTER_STATUS_LINES).unwrap_or(0))
        .map(|index| {
            let y = top.saturating_add(line.saturating_mul(index)).min(bottom);
            Rect {
                x: footer.x.saturating_add(inset),
                y,
                width: footer.width.saturating_sub(inset.saturating_mul(2)),
                height: line.min(bottom - y),
            }
        })
        .collect()
}

/// The Explorer's palette: the menu palette's colours, with the panel drawn
/// as the Window's own glass at the theme's background alpha (foot.ini
/// `alpha`, which Omarchy's glassy mode sets), as the terminal and the tab
/// strip are. The docked sidebar is part of the Window, not a floating menu,
/// so it does not take the shell's menu alpha. Text and icons stay opaque.
/// `[colors] explorer-background` and `explorer-alpha` replace the panel's
/// colour and coverage; its text colours are then chosen against that colour.
#[must_use]
pub(crate) fn lair_explorer_palette(theme: ResolvedTheme) -> SessionPickerPalette {
    let panel_theme = ResolvedTheme {
        background: theme.explorer_background(),
        ..theme
    };
    let mut palette = chrome_panel_palette(panel_theme, ChromeSurface::Menu);
    palette.panel_fill = premultiplied_theme_rgba(palette.panel, theme.explorer_alpha());
    palette
}

/// A fill on the panel, such as the selected row or the active filter, at
/// the panel's own glass alpha, as the tab strip draws its active tab.
fn glass_rgba(color: u32, panel_fill: [u8; 4]) -> [u8; 4] {
    let [red, green, blue, _] = rgba(color);
    premultiplied_rgba([red, green, blue], panel_fill[3])
}

fn tuple(rect: Rect) -> (i32, i32, u32, u32) {
    (
        i32::try_from(rect.x).unwrap_or(i32::MAX),
        i32::try_from(rect.y).unwrap_or(i32::MAX),
        rect.width,
        rect.height,
    )
}

fn rgba(color: u32) -> [u8; 4] {
    [
        ((color >> 16) & 0xff) as u8,
        ((color >> 8) & 0xff) as u8,
        (color & 0xff) as u8,
        0xff,
    ]
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(crate) fn paint_lair_explorer(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    canvas: &mut [u8],
    canvas_width: u32,
    canvas_height: u32,
    scale_120: u32,
    renderer_generation: u64,
    layout: &LairExplorerLayout,
    palette: SessionPickerPalette,
    rows: &[LairExplorerRow],
    selected: Option<NavigationNodeId>,
    query: &str,
    status_message: Option<&str>,
    focused: bool,
    filter: LairExplorerFilter,
    searching: bool,
) -> Result<()> {
    let panel = buffer_rect(layout.panel, scale_120);
    fill_rect(
        canvas,
        canvas_width,
        canvas_height,
        tuple(panel),
        palette.panel_fill,
    );
    let border = scale_120.div_ceil(120).max(1);
    let right = panel.x.saturating_add(panel.width.saturating_sub(border));
    let frame = rgba(if focused {
        palette.focused_frame
    } else {
        palette.frame
    });
    fill_rect(
        canvas,
        canvas_width,
        canvas_height,
        (
            i32::try_from(right).unwrap_or(i32::MAX),
            i32::try_from(panel.y).unwrap_or(i32::MAX),
            border,
            panel.height,
        ),
        frame,
    );
    let mut used = HashSet::new();
    let cell_width = picker_cell_width(
        cache,
        context,
        &mut used,
        ChromeTextStyle::Regular,
        scale_120,
        renderer_generation,
    )?;
    let header = buffer_rect(layout.header, scale_120);
    let inset = LEVEL_INDENT.saturating_mul(scale_120).div_ceil(120);
    let title_right = layout
        .filters
        .first()
        .map_or(header.x + header.width, |(_, rect)| {
            buffer_rect(*rect, scale_120).x
        });
    // The header is a line of text, not a bar: no fill and no rule, so it
    // does not read as a second tab strip. The title is muted; the filters
    // are plain tokens with the active one in bold.
    let header_text_cap = layout.header_text_cell_cap;
    paint_picker_source_within(
        cache,
        context,
        &mut used,
        canvas,
        canvas_width,
        canvas_height,
        &ChromeSource::from(HEADER_TEXT),
        ChromeTextStyle::Bold,
        scale_120,
        renderer_generation,
        Rect {
            x: header.x.saturating_add(inset),
            y: header.y,
            width: title_right.saturating_sub(header.x.saturating_add(inset)),
            height: header.height,
        },
        PickerTextAlignment::Left,
        palette.secondary,
        header_text_cap,
    )?;
    // The rule over the footer, inside the divider.
    if header.height != 0 {
        fill_rect(
            canvas,
            canvas_width,
            canvas_height,
            (
                i32::try_from(panel.x).unwrap_or(i32::MAX),
                i32::try_from(buffer_rect(layout.footer, scale_120).y).unwrap_or(i32::MAX),
                right.saturating_sub(panel.x),
                border,
            ),
            rgba(palette.frame),
        );
    }

    // Each token is centred in its share of the line, and a `·` sits on the
    // boundary between neighbours, inside half a cell of each share. Nothing
    // reaches the divider, however narrow the panel.
    let half_cell = cell_width / 2;
    for (index, (choice, rect)) in layout.filters.iter().enumerate() {
        let rect = buffer_rect(*rect, scale_120);
        let rect = Rect {
            width: rect.width.min(right.saturating_sub(rect.x)),
            ..rect
        };
        if index > 0 {
            let separator_x = rect.x.saturating_sub(half_cell).max(panel.x);
            paint_picker_source_within(
                cache,
                context,
                &mut used,
                canvas,
                canvas_width,
                canvas_height,
                &ChromeSource::from(FILTER_SEPARATOR),
                ChromeTextStyle::Regular,
                scale_120,
                renderer_generation,
                Rect {
                    x: separator_x,
                    width: cell_width.min(right.saturating_sub(separator_x)),
                    ..rect
                },
                PickerTextAlignment::Center,
                palette.secondary,
                header_text_cap,
            )?;
        }
        let active = *choice == filter;
        paint_picker_source_within(
            cache,
            context,
            &mut used,
            canvas,
            canvas_width,
            canvas_height,
            &ChromeSource::from(choice.token()),
            if active {
                ChromeTextStyle::Bold
            } else {
                ChromeTextStyle::Regular
            },
            scale_120,
            renderer_generation,
            Rect {
                x: rect.x.saturating_add(half_cell),
                width: rect.width.saturating_sub(half_cell.saturating_mul(2)),
                ..rect
            },
            PickerTextAlignment::Center,
            if active {
                palette.primary
            } else {
                palette.secondary
            },
            header_text_cap,
        )?;
    }

    let icons = chrome_icons_available();
    let columns = ExplorerColumns::of(rows);
    for (layout_row, row) in layout
        .rows
        .iter()
        .zip(rows.iter().skip(layout.visible_start))
    {
        let rect = buffer_rect(layout_row.rect, scale_120);
        let is_selected = selected == Some(row.id);
        if is_selected {
            // Row hit areas span the panel, but selection paint must not cover
            // its right-hand divider, including at fractional buffer scales.
            let highlight = Rect {
                width: rect.width.min(right.saturating_sub(rect.x)),
                ..rect
            };
            fill_rect(
                canvas,
                canvas_width,
                canvas_height,
                tuple(highlight),
                glass_rgba(palette.selected_fill, palette.panel_fill),
            );
            fill_rect(
                canvas,
                canvas_width,
                canvas_height,
                (
                    i32::try_from(rect.x).unwrap_or(i32::MAX),
                    i32::try_from(rect.y).unwrap_or(i32::MAX),
                    border.saturating_mul(3).min(highlight.width),
                    rect.height,
                ),
                rgba(palette.selected_rail),
            );
        }
        let (columns_source, first_mark) = row_columns_source(row, columns, icons);
        let (name_clip, columns_clip) = row_text_clips(
            rect,
            row.level,
            inset,
            cell_width,
            columns_source.cells(),
            first_mark,
        );
        let primary = if is_selected {
            if row.enabled {
                palette.selected_primary
            } else {
                palette.selected_secondary
            }
        } else if row.enabled {
            palette.primary
        } else {
            palette.secondary
        };
        paint_picker_source(
            cache,
            context,
            &mut used,
            canvas,
            canvas_width,
            canvas_height,
            &row_name_source(row, icons),
            ChromeTextStyle::Regular,
            scale_120,
            renderer_generation,
            name_clip,
            PickerTextAlignment::Left,
            primary,
        )?;
        paint_picker_source(
            cache,
            context,
            &mut used,
            canvas,
            canvas_width,
            canvas_height,
            &columns_source,
            ChromeTextStyle::Regular,
            scale_120,
            renderer_generation,
            columns_paint_clip(columns_clip, right),
            PickerTextAlignment::Left,
            primary,
        )?;
    }

    let footer = buffer_rect(layout.footer, scale_120);
    let lines = footer_lines(footer, scale_120, inset);
    let line_cells = |line: &Rect| usize::try_from(line.width / cell_width).unwrap_or(0);
    // With no rows the notice takes the list's place; with rows, or with no
    // room for even one row, the footer's context line carries it.
    let notice = if rows.is_empty() && layout.visible_capacity > 0 {
        if let Some(message) = status_message {
            let list = Rect {
                x: panel.x.saturating_add(inset),
                y: header.y.saturating_add(header.height),
                width: right.saturating_sub(panel.x.saturating_add(inset.saturating_mul(2))),
                height: ROW_HEIGHT.saturating_mul(scale_120).div_ceil(120).min(
                    footer
                        .y
                        .saturating_sub(header.y.saturating_add(header.height)),
                ),
            };
            paint_picker_text(
                cache,
                context,
                &mut used,
                canvas,
                canvas_width,
                canvas_height,
                message,
                ChromeTextStyle::Regular,
                scale_120,
                renderer_generation,
                list,
                PickerTextAlignment::Left,
                palette.secondary,
            )?;
        }
        None
    } else {
        status_message
    };
    if let Some(row) = selected.and_then(|id| rows.iter().find(|row| row.id == id)) {
        let words = explorer_status_words(row);
        let wrapped = wrap_status_words(&words, line_cells(&lines[0]), FOOTER_STATUS_LINES);
        for (line, text) in lines.iter().zip(&wrapped) {
            paint_picker_text(
                cache,
                context,
                &mut used,
                canvas,
                canvas_width,
                canvas_height,
                text,
                ChromeTextStyle::Regular,
                scale_120,
                renderer_generation,
                *line,
                PickerTextAlignment::Left,
                palette.primary,
            )?;
        }
    }
    let context_line = lines[FOOTER_STATUS_LINES];
    paint_picker_text(
        cache,
        context,
        &mut used,
        canvas,
        canvas_width,
        canvas_height,
        &explorer_context_text(focused, searching, query, notice, line_cells(&context_line)),
        ChromeTextStyle::Regular,
        scale_120,
        renderer_generation,
        context_line,
        PickerTextAlignment::Left,
        palette.secondary,
    )?;
    cache.finish_frame(used);
    Ok(())
}

#[cfg(test)]
mod tests {
    use splinterm_core::LairId;

    use super::*;
    use crate::frontend::LairExplorerRowKind;

    fn row(id: NavigationNodeId) -> LairExplorerRow {
        LairExplorerRow {
            id,
            kind: LairExplorerRowKind::Lair,
            label: "work".into(),
            agent_column: None,
            agent_working: false,
            live: false,
            retention: None,
            access: None,
            status: "Saved".into(),
            accessible_status: "Saved".into(),
            level: 1,
            expanded: Some(false),
            current: false,
            enabled: true,
            pending: false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint(
        context: &RenderContext,
        canvas: &mut [u8],
        canvas_width: u32,
        canvas_height: u32,
        scale: u32,
        layout: &LairExplorerLayout,
        palette: SessionPickerPalette,
        rows: &[LairExplorerRow],
        selected: Option<NavigationNodeId>,
        focused: bool,
        filter: LairExplorerFilter,
    ) {
        paint_lair_explorer(
            &mut SessionPickerTextCache::default(),
            context,
            canvas,
            canvas_width,
            canvas_height,
            scale,
            1,
            layout,
            palette,
            rows,
            selected,
            "",
            None,
            focused,
            filter,
            false,
        )
        .unwrap();
    }

    #[test]
    fn selected_row_preserves_divider_pixels_at_all_scales_and_widths() {
        use super::super::picker::session_picker_palette;
        use crate::config::ResolvedTheme;

        let id = NavigationNodeId::Lair(LairId::new());
        let mut selected = row(id);
        selected.live = true;
        selected.retention = Some(LairRetention::Saved);
        selected.access = Some(LairExplorerAccess::Asking);
        let rows = [selected];
        let mut palette = session_picker_palette(ResolvedTheme::default());
        palette.frame = 0x0012_3456;
        palette.focused_frame = 0x0098_abcd;
        let bgra = |color| {
            let [r, g, b, a] = rgba(color);
            [b, g, r, a]
        };
        for (width, filter) in [1000, 700, 2, 1]
            .into_iter()
            .flat_map(|width| LairExplorerFilter::ALL.map(|filter| (width, filter)))
        {
            for scale in [120, 150, 180, 240] {
                for focused in [false, true] {
                    let content = Rect {
                        x: 7,
                        y: 20,
                        width,
                        height: HEADER_HEIGHT + HEADER_SPACER + FOOTER_HEIGHT + ROW_HEIGHT,
                    };
                    let layout = lair_explorer_layout(content, 640, &rows, Some(id), 0).unwrap();
                    let canvas_width = (content.x + content.width) * scale / 120 + 1;
                    let canvas_height = (content.y + content.height) * scale / 120 + 1;
                    let mut canvas = vec![0; (canvas_width * canvas_height * 4) as usize];
                    paint(
                        &RenderContext::new(u16::MAX),
                        &mut canvas,
                        canvas_width,
                        canvas_height,
                        scale,
                        &layout,
                        palette,
                        &rows,
                        Some(id),
                        focused,
                        filter,
                    );
                    let panel = buffer_rect(layout.panel, scale);
                    let border = scale.div_ceil(120).max(1);
                    let right = panel.x + panel.width.saturating_sub(border);
                    let expected = bgra(if focused {
                        palette.focused_frame
                    } else {
                        palette.frame
                    });
                    for y in panel.y..panel.y + panel.height {
                        for x in right..panel.x + panel.width {
                            let offset = ((y * canvas_width + x) * 4) as usize;
                            assert_eq!(
                                &canvas[offset..offset + 4],
                                &expected,
                                "divider at {x},{y}; width={width}, scale={scale}, focused={focused}, filter={filter:?}"
                            );
                        }
                    }
                    if right > panel.x {
                        let y = buffer_rect(layout.rows[0].rect, scale).y + 1;
                        let x = right - 1;
                        let offset = ((y * canvas_width + x) * 4) as usize;
                        let expected = bgra(if right - panel.x <= border * 3 {
                            palette.selected_rail
                        } else {
                            palette.selected_fill
                        });
                        assert_eq!(
                            &canvas[offset..offset + 4],
                            &expected,
                            "selection must reach the inside edge without covering the divider"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn row_name_ends_a_cell_before_its_first_mark_and_columns_end_at_the_inset() {
        for width in [1, 2, 80, 160, PREFERRED_WIDTH] {
            for level in [1, 2, 3] {
                for scale in [120, 150, 180, 240] {
                    for cell in [5, 8, 11, 16, 24] {
                        for (column_cells, first_mark) in [(1, 0), (6, 5), (12, 0), (12, 3)] {
                            let rect = buffer_rect(
                                Rect {
                                    x: 7,
                                    y: 20,
                                    width,
                                    height: ROW_HEIGHT,
                                },
                                scale,
                            );
                            let inset = LEVEL_INDENT.saturating_mul(scale).div_ceil(120);
                            let (name, columns) =
                                row_text_clips(rect, level, inset, cell, column_cells, first_mark);
                            let what = format!(
                                "width {width}, level {level}, scale {scale}, cell {cell}, \
                                 columns {column_cells}/{first_mark}"
                            );
                            for clip in [name, columns] {
                                assert!(clip.x >= rect.x, "{what}");
                                assert!(clip.x + clip.width <= rect.x + rect.width, "{what}");
                                assert_eq!((clip.y, clip.height), (rect.y, rect.height));
                            }
                            // The name never reaches a mark: one blank cell
                            // separates it from the first one.
                            let mark = columns.x + cell * u32::try_from(first_mark).unwrap();
                            assert!(
                                name.x + name.width + cell <= mark || name.width == 0,
                                "{what}"
                            );
                            assert!(name.x + name.width <= columns.x.max(mark), "{what}");
                            // With room, the columns end at the right inset
                            // and hold every cell.
                            let room = rect
                                .width
                                .saturating_sub(inset * (u32::try_from(level).unwrap() + 1));
                            if room >= cell * u32::try_from(column_cells).unwrap() {
                                assert_eq!(
                                    columns.x + columns.width,
                                    rect.x + rect.width - inset,
                                    "{what}"
                                );
                                assert_eq!(
                                    columns.width,
                                    cell * u32::try_from(column_cells).unwrap(),
                                    "{what}"
                                );
                            }
                            if width == PREFERRED_WIDTH && level == 1 && cell <= 11 {
                                // At ordinary sizes a level-one name keeps
                                // well over half the panel beside every
                                // column.
                                assert!(name.width * 2 >= rect.width - cell * 12, "{what}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn columns_line_up_on_every_row_and_skip_unused_columns() {
        let id = NavigationNodeId::Lair(LairId::new());
        let mut lair = row(id);
        lair.live = true;
        lair.retention = Some(LairRetention::Pinned);
        lair.access = Some(LairExplorerAccess::Asking);
        lair.agent_column = Some(ChromeSource::from("    "));
        let mut splint = row(id);
        splint.kind = LairExplorerRowKind::Splint;
        splint.level = 3;
        splint.expanded = None;
        let mut column = ChromeSource::default();
        column.push_str("⠋   ");
        splint.agent_column = Some(column);
        let rows = [lair.clone(), splint.clone()];
        let columns = ExplorerColumns::of(&rows);
        assert_eq!(
            columns,
            ExplorerColumns {
                access: true,
                agent: Some(4),
                retention: true,
            }
        );
        let (lair_columns, lair_mark) = row_columns_source(&lair, columns, true);
        let (splint_columns, splint_mark) = row_columns_source(&splint, columns, true);
        // access, gap, agent, gap, live, gap, retention.
        assert_eq!(lair_columns.cells(), 12);
        assert_eq!(splint_columns.cells(), 12);
        assert_eq!(lair_columns.describe(), "[lock-question]      ● [pinned]");
        assert_eq!(splint_columns.describe(), "   ⠋    ○   ");
        // The Lair's first mark is its access icon; the Splint leaves access
        // blank, so its name may run up to its spinner.
        assert_eq!((lair_mark, splint_mark), (0, 3));
        let (ascii, _) = row_columns_source(&lair, columns, false);
        assert_eq!(ascii.text(), "A?      ● p ");
        // Unused columns are left out; the live dot is always there.
        let bare = [row(id)];
        let columns = ExplorerColumns::of(&bare);
        assert_eq!(columns, ExplorerColumns::default());
        let (source, mark) = row_columns_source(&bare[0], columns, true);
        assert_eq!((source.text(), mark), ("○", 0));
        let mut saved = row(id);
        saved.live = true;
        saved.retention = Some(LairRetention::Saved);
        let (source, mark) =
            row_columns_source(&saved, ExplorerColumns::of(&[saved.clone()]), true);
        assert_eq!((source.describe().as_str(), mark), ("● [pin]", 0));
    }

    #[test]
    fn rows_scroll_to_last_selection_without_losing_hit_targets() {
        let rows = (0..30)
            .map(|_| row(NavigationNodeId::Lair(LairId::new())))
            .collect::<Vec<_>>();
        let selected = rows.last().unwrap().id;
        let layout = lair_explorer_layout(
            Rect {
                x: 0,
                y: 0,
                width: 700,
                height: 400,
            },
            640,
            &rows,
            Some(selected),
            0,
        )
        .unwrap();
        // One line per row: eleven rows fit where two-line rows fitted five.
        assert_eq!(layout.visible_capacity, 11);
        assert_eq!(layout.visible_start, 19);
        let last = layout.rows.last().unwrap();
        assert_eq!(last.id, selected);
        assert!(last.rect.y + last.rect.height <= layout.footer.y);
        assert_eq!(
            lair_explorer_hit_test(&layout, (50.0, f64::from(last.rect.y + 1))),
            Some(selected)
        );
    }

    /// Clicks land on the single-line row under the pointer, from its top to
    /// its bottom pixel and across the icon columns, and only the indent,
    /// dot, and chevron toggle disclosure.
    #[test]
    fn hit_tests_map_every_single_line_row_and_its_chevron() {
        let mut rows = Vec::new();
        for level in [1, 2, 3, 2, 1] {
            let mut entry = row(NavigationNodeId::Lair(LairId::new()));
            entry.level = level;
            entry.expanded = (level < 3).then_some(false);
            rows.push(entry);
        }
        let layout = lair_explorer_layout(
            Rect {
                x: 5,
                y: 34,
                width: 1000,
                height: 716,
            },
            640,
            &rows,
            None,
            0,
        )
        .unwrap();
        assert_eq!(layout.rows.len(), rows.len());
        assert_eq!(
            layout.rows[0].rect.y,
            layout.header.y + HEADER_HEIGHT + HEADER_SPACER
        );
        for (index, (laid, entry)) in layout.rows.iter().zip(&rows).enumerate() {
            assert_eq!(laid.rect.height, ROW_HEIGHT);
            if let Some(next) = layout.rows.get(index + 1) {
                assert_eq!(laid.rect.y + laid.rect.height, next.rect.y);
            }
            let top = f64::from(laid.rect.y);
            let bottom = f64::from(laid.rect.y + laid.rect.height) - 0.5;
            let columns = f64::from(laid.rect.x + laid.rect.width) - 20.0;
            for y in [top, top + 13.0, bottom] {
                assert_eq!(
                    lair_explorer_hit_test(&layout, (columns, y)),
                    Some(entry.id)
                );
                assert_eq!(
                    lair_explorer_disclosure_hit_test(&layout, &rows, (columns, y)),
                    None
                );
                let chevron = f64::from(
                    laid.rect.x + LEVEL_INDENT * u32::try_from(entry.level).unwrap() + 20,
                );
                assert_eq!(
                    lair_explorer_disclosure_hit_test(&layout, &rows, (chevron, y)),
                    entry.expanded.map(|_| entry.id),
                    "level {}",
                    entry.level
                );
            }
        }
        let below = f64::from(layout.rows.last().unwrap().rect.y + ROW_HEIGHT);
        assert_eq!(lair_explorer_hit_test(&layout, (50.0, below)), None);
        assert_eq!(
            lair_explorer_hit_test(&layout, (50.0, f64::from(layout.header.y + 5))),
            None
        );
    }

    #[test]
    fn filter_hit_regions_never_overlap_rows_or_footer() {
        let rows = [row(NavigationNodeId::Lair(LairId::new()))];
        for width in [1, 2, 100, 700, 1000] {
            for height in [1, 29, 30, 75, 104, 134, 182, 400] {
                let layout = lair_explorer_layout(
                    Rect {
                        x: 7,
                        y: 20,
                        width,
                        height,
                    },
                    640,
                    &rows,
                    None,
                    0,
                )
                .unwrap();
                for (filter, rect) in &layout.filters {
                    assert!(rect.y + rect.height <= layout.header.y + layout.header.height);
                    assert!(rect.x + rect.width <= layout.panel.x + layout.panel.width);
                    if rect.width == 0 {
                        continue;
                    }
                    let point = (
                        f64::from(rect.x) + f64::from(rect.width) / 2.0,
                        f64::from(rect.y) + 1.0,
                    );
                    assert_eq!(lair_explorer_filter_hit_test(&layout, point), Some(*filter));
                    assert_eq!(lair_explorer_hit_test(&layout, point), None);
                    assert_eq!(
                        lair_explorer_disclosure_hit_test(&layout, &rows, point),
                        None
                    );
                    assert_eq!(
                        lair_explorer_filter_hit_test(
                            &layout,
                            (point.0, f64::from(rect.y + rect.height))
                        ),
                        None
                    );
                }
                for row in &layout.rows {
                    assert_eq!(
                        lair_explorer_filter_hit_test(
                            &layout,
                            (f64::from(row.rect.x), f64::from(row.rect.y))
                        ),
                        None
                    );
                }
                assert_eq!(
                    lair_explorer_filter_hit_test(
                        &layout,
                        (f64::from(layout.footer.x), f64::from(layout.footer.y))
                    ),
                    None
                );
            }
        }
        // The filters share the title's line, right of the title, in order.
        let layout = lair_explorer_layout(
            Rect {
                x: 0,
                y: 0,
                width: 1000,
                height: 400,
            },
            640,
            &rows,
            None,
            0,
        )
        .unwrap();
        let filters = layout
            .filters
            .iter()
            .map(|(filter, rect)| (filter.label(), rect.x))
            .collect::<Vec<_>>();
        assert_eq!(
            filters.iter().map(|(label, _)| *label).collect::<Vec<_>>(),
            ["Live", "Saved", "All"]
        );
        assert!(filters[0].1 >= HEADER_TITLE_WIDTH);
        assert!(filters.windows(2).all(|pair| pair[0].1 < pair[1].1));
    }

    #[test]
    fn footer_context_line_makes_input_ownership_explicit() {
        assert_eq!(
            explorer_context_text(false, false, "", None, 40),
            "Click rows · terminal input active"
        );
        assert_eq!(
            explorer_context_text(true, false, "", None, 40),
            "↑↓ move · F filter · Esc terminal"
        );
        // A navigation notice takes the line while rows are listed, but never
        // hides an open search: the field stays where the typing goes.
        for notice in [
            "Refreshing navigation…",
            "Navigation is stale · R retry",
            "Navigation unavailable · R retry",
        ] {
            assert_eq!(
                explorer_context_text(true, true, "abc", Some(notice), 40),
                "Search: abc_"
            );
            assert_eq!(
                explorer_context_text(true, false, "", Some(notice), 40),
                notice
            );
            assert_eq!(
                explorer_context_text(true, false, "abc", Some(notice), 40),
                notice
            );
        }
        assert_eq!(HEADER_TEXT, "lairs");
    }

    #[test]
    fn search_footer_does_not_advertise_tree_filter_shortcut() {
        let field = explorer_context_text(true, true, "dojo", None, 40);
        assert_eq!(field, "Search: dojo_");
        assert!(!field.contains("F filter"));
        // A long query keeps its latest input and the caret.
        let long = explorer_context_text(true, true, "a-very-long-search-query", None, 20);
        assert_eq!(long, "Search: …arch-query_");
        assert!(text_cells(&long) <= 20);
        // A query left behind after returning to the tree is still shown.
        assert_eq!(
            explorer_context_text(true, false, "dojo", None, 40),
            "Search: dojo"
        );
        assert_eq!(
            explorer_context_text(false, true, "", None, 40),
            explorer_context_text(false, false, "", None, 40)
        );
    }

    #[test]
    fn status_words_wrap_and_truncate_only_between_words() {
        let words =
            "splinterm · Agent connected to part · Running · Saved · 1 needs input, 2 working";
        for cells in 1..=90 {
            for lines in 1..=3 {
                let wrapped = wrap_status_words(words, cells, lines);
                assert!(wrapped.len() <= lines, "{cells}/{lines}: {wrapped:?}");
                assert!(!wrapped.is_empty());
                for line in &wrapped {
                    assert!(text_cells(line) <= cells, "{cells}/{lines}: {line:?}");
                    assert!(!line.starts_with('·') && !line.starts_with(' '), "{line:?}");
                    assert!(!line.ends_with(" ·") && !line.ends_with(' '), "{line:?}");
                    // Every word is whole unless one word is wider than
                    // the whole line.
                    for word in line.trim_end_matches('…').split(' ') {
                        let whole = words.split(' ').any(|original| original == word);
                        assert!(
                            whole || text_cells(word) >= cells.saturating_sub(1),
                            "{cells}/{lines}: cut {word:?} in {line:?}"
                        );
                    }
                }
                let shown = wrapped.join(" ");
                if wrapped.len() == lines && !shown.trim_end_matches('…').ends_with("working") {
                    assert!(
                        wrapped.last().unwrap().ends_with('…')
                            || text_cells(wrapped.last().unwrap()) == cells,
                        "{cells}/{lines}: {wrapped:?}"
                    );
                }
            }
        }
        assert_eq!(
            wrap_status_words(words, 40, 2),
            [
                "splinterm · Agent connected to part",
                "Running · Saved · 1 needs input, 2…"
            ]
        );
        assert_eq!(
            wrap_status_words("work · Running · Saved", 40, 2),
            ["work · Running · Saved"]
        );
        assert_eq!(
            wrap_status_words("work · Running · Saved", 15, 2),
            ["work · Running", "Saved"]
        );
        assert_eq!(
            wrap_status_words("work · Running · Saved", 15, 1),
            ["work · Running…"]
        );
        assert!(wrap_status_words(words, 0, 2).is_empty());
        assert!(wrap_status_words(words, 20, 0).is_empty());
    }

    #[test]
    fn footer_words_name_the_selected_row_and_its_full_status() {
        let mut entry = row(NavigationNodeId::Lair(LairId::new()));
        entry.label = "splinterm".into();
        entry.status = "Agent connected · Running · Saved · 1 working".into();
        entry.accessible_status = entry.status.clone();
        assert_eq!(
            explorer_status_words(&entry),
            "splinterm · Agent connected · Running · Saved · 1 working"
        );
        // The words come from the same status as the accessible text.
        assert!(explorer_status_words(&entry).ends_with(&entry.accessible_status));
    }

    #[test]
    fn dock_preserves_minimum_terminal_width_and_narrow_surface_draws_over() {
        let id = NavigationNodeId::Lair(LairId::new());
        let rows = [row(id)];
        let dock = lair_explorer_layout(
            Rect {
                x: 0,
                y: 20,
                width: 1000,
                height: 600,
            },
            640,
            &rows,
            Some(id),
            0,
        )
        .unwrap();
        assert_eq!(dock.mode, LairExplorerPresentationMode::Docked);
        assert!(dock.terminal.width >= 640);
        let drawer = lair_explorer_layout(
            Rect {
                x: 0,
                y: 20,
                width: 700,
                height: 600,
            },
            640,
            &rows,
            Some(id),
            0,
        )
        .unwrap();
        assert_eq!(drawer.mode, LairExplorerPresentationMode::Drawer);
        assert_eq!(drawer.terminal.width, 700);
        let row_y = f64::from(drawer.rows[0].rect.y + 1);
        assert_eq!(lair_explorer_hit_test(&drawer, (10.0, row_y)), Some(id));
        assert_eq!(
            lair_explorer_disclosure_hit_test(&drawer, &rows, (10.0, row_y)),
            Some(id)
        );
        assert_eq!(
            lair_explorer_disclosure_hit_test(&drawer, &rows, (100.0, row_y)),
            None
        );
    }

    /// VM report F5/R1: a 628 px half-width tile on a 1280 px output leaves a
    /// 622 px canvas. With the 80-column docking floor the Explorer fell back
    /// to the drawer, covering the terminal's left 288 px where all of its
    /// text was, so the visible terminal area painted nothing.
    #[test]
    fn half_width_tile_docks_beside_a_visible_terminal() {
        let id = NavigationNodeId::Lair(LairId::new());
        let rows = [row(id)];
        let content = Rect {
            x: 0,
            y: 34,
            width: 622,
            height: 716,
        };
        for cell_width in [7, 8, 9] {
            let layout = lair_explorer_layout(
                content,
                explorer_minimum_docked_terminal_width(cell_width, 80),
                &rows,
                Some(id),
                0,
            )
            .unwrap();
            assert_eq!(
                layout.mode,
                LairExplorerPresentationMode::Docked,
                "cell width {cell_width}"
            );
            // The terminal starts where the panel ends: nothing covers it.
            assert_eq!(layout.terminal.x, layout.panel.x + layout.panel.width);
            assert_eq!(
                layout.terminal.x + layout.terminal.width,
                content.x + content.width
            );
            assert!(layout.terminal.width / cell_width >= 20);
        }
    }

    #[test]
    fn docking_floor_is_bounded_and_tiny_windows_keep_the_drawer() {
        assert_eq!(explorer_minimum_docked_terminal_width(8, 80), 160);
        assert_eq!(explorer_minimum_docked_terminal_width(8, 12), 96);
        assert_eq!(explorer_minimum_docked_terminal_width(0, 80), 20);
        let id = NavigationNodeId::Lair(LairId::new());
        let rows = [row(id)];
        let content = |width| Rect {
            x: 0,
            y: 34,
            width,
            height: 716,
        };
        let minimum = explorer_minimum_docked_terminal_width(8, 80);
        let docked =
            lair_explorer_layout(content(PREFERRED_WIDTH + minimum), minimum, &rows, None, 0)
                .unwrap();
        assert_eq!(docked.mode, LairExplorerPresentationMode::Docked);
        assert_eq!(docked.terminal.width, minimum);
        let drawer = lair_explorer_layout(
            content(PREFERRED_WIDTH + minimum - 1),
            minimum,
            &rows,
            None,
            0,
        )
        .unwrap();
        assert_eq!(drawer.mode, LairExplorerPresentationMode::Drawer);
        assert_eq!(drawer.terminal, content(PREFERRED_WIDTH + minimum - 1));
    }

    /// Review of VM E6-0: at maximum zoom the zoomed cell (about 77 px) made
    /// the floor wider than a full-width Window, so the drawer covered line
    /// starts ("rojects/splin" cut at the panel edge).
    #[test]
    fn maximum_zoom_keeps_a_wide_window_docked() {
        let id = NavigationNodeId::Lair(LairId::new());
        let rows = [row(id)];
        let context = RenderContext::new(u16::MAX).at_maximum_zoom(120);
        let size = context.effective_font_size(120).unwrap();
        let primary = &context.font_generation().unwrap().faces[0];
        let cell_width = super::super::super::cell_metrics(primary, size)
            .unwrap()
            .width;
        assert!(cell_width * 20 > 1268, "cell width {cell_width}");
        let window = Rect {
            x: 0,
            y: 34,
            width: 1268,
            height: 754,
        };
        let layout = lair_explorer_layout(
            window,
            explorer_minimum_docked_terminal_width(cell_width, 80),
            &rows,
            Some(id),
            0,
        )
        .unwrap();
        assert_eq!(layout.mode, LairExplorerPresentationMode::Docked);
        assert_eq!(layout.terminal.x, layout.panel.x + layout.panel.width);
        assert_eq!(
            explorer_minimum_docked_terminal_width(u32::MAX, 80),
            MAXIMUM_DOCKED_TERMINAL_FLOOR
        );
    }

    /// VM case E6, for the one-line layout: at every Omarchy text size from
    /// 9 to 20 px and at the largest UI size, at 1x, 1.25x, 1.5x, and 2x,
    /// every Explorer line still holds a whole chrome glyph, a long name is
    /// ellipsized before the icon columns, and the columns fit the row.
    #[test]
    fn every_ui_text_size_fits_every_explorer_line_and_ellipsizes_long_names() {
        use super::super::super::{ChromeText, text::chrome_maximum_cell_height};
        use crate::geometry::FontSize;

        let id = NavigationNodeId::Lair(LairId::new());
        let mut long = row(id);
        long.label = "e-long-label-lair-with-a-very-long-name".into();
        long.access = Some(LairExplorerAccess::Connected);
        long.retention = Some(LairRetention::Pinned);
        long.agent_column = Some(ChromeSource::from("⠋   "));
        let rows = [long];
        let layout = lair_explorer_layout(
            Rect {
                x: 0,
                y: 34,
                width: 1268,
                height: 754,
            },
            160,
            &rows,
            Some(id),
            0,
        )
        .unwrap();
        let contexts = (9_u16..=20)
            .map(|px| {
                RenderContext::new(u16::MAX).with_chrome_font_size(FontSize::Pixels(f32::from(px)))
            })
            .chain([RenderContext::new(u16::MAX)
                .at_maximum_zoom(120)
                .at_maximum_chrome_font_size()]);
        for context in contexts {
            for scale in [120, 150, 180, 240] {
                let name = ChromeText::load_source_styled_with_context(
                    &row_name_source(&rows[0], true),
                    scale,
                    ChromeTextStyle::Regular,
                    &context,
                )
                .unwrap();
                assert!(name.pixel_height() <= chrome_maximum_cell_height(scale));
                let cell = name.frame.cell_width;
                let (columns, first_mark) =
                    row_columns_source(&rows[0], ExplorerColumns::of(&rows), true);
                let inset = LEVEL_INDENT.saturating_mul(scale).div_ceil(120);
                let row_rect = buffer_rect(layout.rows[0].rect, scale);
                let (name_clip, columns_clip) = row_text_clips(
                    row_rect,
                    rows[0].level,
                    inset,
                    cell,
                    columns.cells(),
                    first_mark,
                );
                let footer = footer_lines(buffer_rect(layout.footer, scale), scale, inset);
                let mut lines = vec![
                    ("row", row_rect.height),
                    ("header", buffer_rect(layout.header, scale).height),
                    ("filter", buffer_rect(layout.filters[0].1, scale).height),
                ];
                lines.extend(footer.iter().map(|line| ("footer", line.height)));
                for (what, height) in lines {
                    assert!(
                        name.pixel_height() <= height,
                        "{what} line at scale {scale}: {} > {height}",
                        name.pixel_height()
                    );
                }
                // Every footer line lies inside the footer and below the rows.
                let footer_rect = buffer_rect(layout.footer, scale);
                for line in &footer {
                    assert!(line.y >= footer_rect.y);
                    assert!(line.y + line.height <= footer_rect.y + footer_rect.height);
                }
                // The columns fit whole beside a name of several cells.
                assert_eq!(
                    columns_clip.width,
                    cell * u32::try_from(columns.cells()).unwrap(),
                    "scale {scale}, cell {cell}"
                );
                assert!(name_clip.width >= cell * 6, "scale {scale}, cell {cell}");
                // The name is ellipsized to whole cells within its clip.
                assert!(name.pixel_width() > name_clip.width);
                let cells = usize::try_from(name_clip.width / cell).unwrap();
                let fitted = truncate_picker_text(&rows[0].label, cells.saturating_sub(4));
                assert!(fitted.ends_with('…'));
                let fitted = ChromeText::load_with_context(&fitted, scale, &context).unwrap();
                assert!(fitted.pixel_width() <= name_clip.width);
            }
        }
    }

    /// Paints one unselected row and returns the canvas, its width, the
    /// row's name and column clips, and the chrome cell width.
    fn paint_row(
        context: &RenderContext,
        painted: LairExplorerRow,
        scale: u32,
    ) -> (Vec<u8>, u32, Rect, Rect, u32) {
        use super::super::picker::session_picker_palette;
        use crate::config::ResolvedTheme;

        let rows = [painted];
        let surface = Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: HEADER_HEIGHT + HEADER_SPACER + FOOTER_HEIGHT + ROW_HEIGHT,
        };
        let layout = lair_explorer_layout(surface, 640, &rows, None, 0).unwrap();
        let canvas_width = surface.width * scale / 120 + 1;
        let canvas_height = surface.height * scale / 120 + 1;
        let mut canvas = vec![0; (canvas_width * canvas_height * 4) as usize];
        paint(
            context,
            &mut canvas,
            canvas_width,
            canvas_height,
            scale,
            &layout,
            session_picker_palette(ResolvedTheme::default()),
            &rows,
            None,
            false,
            LairExplorerFilter::All,
        );
        let cell = chrome_cell_width(context, scale);
        let inset = LEVEL_INDENT.saturating_mul(scale).div_ceil(120);
        let (columns, first_mark) = row_columns_source(&rows[0], ExplorerColumns::of(&rows), true);
        let (name, columns) = row_text_clips(
            buffer_rect(layout.rows[0].rect, scale),
            rows[0].level,
            inset,
            cell,
            columns.cells(),
            first_mark,
        );
        (canvas, canvas_width, name, columns, cell)
    }

    /// Asserts that `painted` differs from `blank` only inside cells `cells`
    /// counted from `origin` on the row's line, and returns how many pixels
    /// differ.
    fn changed_only_in_cells(
        painted: &[u8],
        blank: &[u8],
        width: u32,
        origin: Rect,
        cell: u32,
        cells: std::ops::Range<u32>,
        what: &str,
    ) -> usize {
        let left = origin.x + cells.start * cell;
        let right = origin.x + cells.end * cell;
        let mut changed = 0;
        for (index, (a, b)) in painted
            .chunks_exact(4)
            .zip(blank.chunks_exact(4))
            .enumerate()
        {
            if a == b {
                continue;
            }
            changed += 1;
            let x = u32::try_from(index).unwrap() % width;
            let y = u32::try_from(index).unwrap() / width;
            assert!(
                (left..right).contains(&x) && (origin.y..origin.y + origin.height).contains(&y),
                "{what} painted outside cells {cells:?} at {x},{y}"
            );
        }
        changed
    }

    fn chrome_cell_width(context: &RenderContext, scale: u32) -> u32 {
        super::super::super::ChromeText::load_with_context(" ", scale, context)
            .unwrap()
            .frame
            .cell_width
    }

    #[test]
    fn agent_column_paints_only_its_fixed_slots_right_of_the_name() {
        use crate::frontend::{AgentGlyphs, AgentSpinner};
        use splinterm_protocol::AgentState;

        assert!(super::super::super::chrome_icons_available());
        let context = RenderContext::new(u16::MAX);
        let states = [
            AgentState::NeedsInput,
            AgentState::Error,
            AgentState::Working,
            AgentState::Done,
            AgentState::Idle,
        ];
        let agent_row = |column: ChromeSource| {
            let mut agent_row = row(NavigationNodeId::Lair(LairId::new()));
            agent_row.agent_column = Some(column);
            agent_row
        };
        for scale in [120, 150, 180, 240] {
            // The columns are "{agent} {live}": the agent glyph takes cells
            // 0..2 and the stale marker cells 2..4.
            let (blank, width, _, columns, cell) =
                paint_row(&context, agent_row(ChromeSource::from("    ")), scale);
            let mut glyphs = Vec::new();
            for state in states {
                for is_stale in [false, true] {
                    let mut column = ChromeSource::default();
                    match AgentGlyphs::icon(state) {
                        Some(icon) => {
                            column.push_icon(icon);
                        }
                        None => {
                            column.push_str(&format!("{} ", AgentSpinner::STATIC.glyph()));
                        }
                    }
                    if is_stale {
                        column.push_icon(ChromeIcon::Clock);
                    } else {
                        column.push_str("  ");
                    }
                    let (painted, _, _, _, _) = paint_row(&context, agent_row(column), scale);
                    // A fresh glyph stays in its slot; the stale clock takes
                    // the next slot and never reaches the live dot.
                    let slots = if is_stale { 0..4 } else { 0..2 };
                    let what = format!("{state:?} stale={is_stale} at scale {scale}");
                    let changed =
                        changed_only_in_cells(&painted, &blank, width, columns, cell, slots, &what);
                    assert!(changed > 0, "{what} painted nothing");
                    if !is_stale {
                        glyphs.push(painted);
                    }
                }
            }
            // Every state paints a distinct shape.
            for (index, glyph) in glyphs.iter().enumerate() {
                assert!(glyphs[index + 1..].iter().all(|other| other != glyph));
            }
            // Spinner frames are distinct shapes in the same cell.
            let mut frames = Vec::new();
            let mut spinner = AgentSpinner::STATIC;
            // All ten frames.
            for _ in 0..10 {
                let column = ChromeSource::from(format!("{}   ", spinner.glyph()));
                let (painted, _, _, _, _) = paint_row(&context, agent_row(column), scale);
                let what = format!("spinner {spinner:?} at scale {scale}");
                changed_only_in_cells(&painted, &blank, width, columns, cell, 0..1, &what);
                frames.push(painted);
                spinner = spinner.next();
            }
            for (index, frame) in frames.iter().enumerate() {
                assert!(frames[index + 1..].iter().all(|other| other != frame));
            }
            // ASCII fallback columns stay within their two text cells.
            for column in ["?   ", "!   ", "+ ~ ", "-   "] {
                let (painted, _, _, _, _) =
                    paint_row(&context, agent_row(ChromeSource::from(column)), scale);
                let what = format!("{column:?} at scale {scale}");
                let changed =
                    changed_only_in_cells(&painted, &blank, width, columns, cell, 0..4, &what);
                assert!(changed > 0, "{what} painted nothing");
            }
        }
    }

    #[test]
    fn live_retention_and_access_marks_are_distinct_and_stay_in_their_columns() {
        let context = RenderContext::new(u16::MAX);
        let base = |live, retention, access| {
            let mut entry = row(NavigationNodeId::Lair(LairId::new()));
            entry.live = live;
            entry.retention = retention;
            entry.access = access;
            entry
        };
        for scale in [120, 150, 240] {
            // Every variant keeps the access and retention columns: access
            // takes cells 0..2, the live dot cell 3, and retention 5..7.
            let reference = base;
            let (blank_access, width, _, columns, cell) = paint_row(
                &context,
                reference(
                    false,
                    Some(LairRetention::Saved),
                    Some(LairExplorerAccess::Reachable),
                ),
                scale,
            );
            let mut marks = vec![blank_access.clone()];
            for access in [LairExplorerAccess::Connected, LairExplorerAccess::Asking] {
                let (painted, _, _, _, _) = paint_row(
                    &context,
                    reference(false, Some(LairRetention::Saved), Some(access)),
                    scale,
                );
                let what = format!("{access:?} at scale {scale}");
                assert!(
                    changed_only_in_cells(
                        &painted,
                        &blank_access,
                        width,
                        columns,
                        cell,
                        0..2,
                        &what
                    ) > 0
                );
                marks.push(painted);
            }
            let (live, _, _, _, _) = paint_row(
                &context,
                reference(
                    true,
                    Some(LairRetention::Saved),
                    Some(LairExplorerAccess::Reachable),
                ),
                scale,
            );
            // The dot comes from the text font, whose edges may reach the
            // blank cells on either side of it, never another column.
            let what = format!("live at scale {scale}");
            assert!(
                changed_only_in_cells(&live, &blank_access, width, columns, cell, 2..5, &what) > 0
            );
            let (pinned, _, _, _, _) = paint_row(
                &context,
                reference(
                    false,
                    Some(LairRetention::Pinned),
                    Some(LairExplorerAccess::Reachable),
                ),
                scale,
            );
            let what = format!("pinned at scale {scale}");
            assert!(
                changed_only_in_cells(&pinned, &blank_access, width, columns, cell, 5..7, &what)
                    > 0
            );
            marks.push(live);
            marks.push(pinned);
            for (index, mark) in marks.iter().enumerate() {
                assert!(marks[index + 1..].iter().all(|other| other != mark));
            }
        }
    }

    #[test]
    fn long_names_never_reach_the_icon_columns() {
        let context = RenderContext::new(u16::MAX);
        for scale in [120, 150, 180, 240] {
            for level in [1, 2, 3] {
                for access in [None, Some(LairExplorerAccess::Asking)] {
                    let entry = |label: &str| {
                        let mut entry = row(NavigationNodeId::Lair(LairId::new()));
                        entry.level = level;
                        entry.label = label.into();
                        entry.access = access;
                        entry.live = true;
                        entry.retention = Some(LairRetention::Saved);
                        entry.agent_column = Some(ChromeSource::from("⠋   "));
                        entry
                    };
                    let (short, width, name, columns, _) = paint_row(&context, entry(""), scale);
                    let (long, _, _, _, _) = paint_row(
                        &context,
                        entry("a-name-long-enough-to-cross-every-icon-column-of-the-row"),
                        scale,
                    );
                    let what = format!("level {level}, access {access:?}, scale {scale}");
                    let mut changed = 0;
                    for (index, (a, b)) in
                        long.chunks_exact(4).zip(short.chunks_exact(4)).enumerate()
                    {
                        if a != b {
                            changed += 1;
                            let x = u32::try_from(index).unwrap() % width;
                            assert!(x < name.x + name.width, "{what}: name ink at {x}");
                            assert!(x < columns.x, "{what}: name ink in the columns at {x}");
                        }
                    }
                    assert!(changed > 0, "{what}");
                }
            }
        }
    }

    #[test]
    fn disclosure_chevrons_are_distinct_icons_inside_their_slot() {
        let context = RenderContext::new(u16::MAX);
        for scale in [120, 150, 180, 240] {
            let base = row(NavigationNodeId::Lair(LairId::new()));
            let (blank, width, label, _, cell) = paint_row(
                &context,
                LairExplorerRow {
                    expanded: None,
                    ..base.clone()
                },
                scale,
            );
            let mut chevrons = Vec::new();
            for expanded in [false, true] {
                let (painted, _, _, _, _) = paint_row(
                    &context,
                    LairExplorerRow {
                        expanded: Some(expanded),
                        ..base.clone()
                    },
                    scale,
                );
                let what = format!("expanded={expanded} at scale {scale}");
                let changed =
                    changed_only_in_cells(&painted, &blank, width, label, cell, 1..3, &what);
                assert!(changed > 0, "{what} painted nothing");
                chevrons.push(painted);
            }
            assert_ne!(chevrons[0], chevrons[1]);
            // The current-row dot keeps the cell before the chevron: its edge
            // antialiasing may reach the slot, but never the chevron's ink.
            let (current, _, _, _, _) = paint_row(
                &context,
                LairExplorerRow {
                    current: true,
                    ..base.clone()
                },
                scale,
            );
            let what = format!("current dot at scale {scale}");
            let changed =
                changed_only_in_cells(&current, &chevrons[0], width, label, cell, 0..2, &what);
            assert!(changed > 0, "{what} painted nothing");
            for ((dot, chevron), plain) in current
                .chunks_exact(4)
                .zip(chevrons[0].chunks_exact(4))
                .zip(blank.chunks_exact(4))
            {
                if chevron != plain {
                    assert_eq!(dot, chevron, "{what} overlaps the chevron");
                }
            }
        }
    }

    /// The selected row's status is spelled out in the footer, and with no
    /// row selected the footer's status lines stay empty.
    #[test]
    fn footer_paints_the_selected_rows_words_inside_its_status_lines() {
        use super::super::picker::session_picker_palette;
        use crate::config::ResolvedTheme;

        let context = RenderContext::new(u16::MAX);
        let id = NavigationNodeId::Lair(LairId::new());
        let mut entry = row(id);
        entry.status = "Agent connected · Running · Saved · 1 needs input, 2 working".into();
        let rows = [entry];
        let surface = Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: HEADER_HEIGHT + HEADER_SPACER + FOOTER_HEIGHT + ROW_HEIGHT,
        };
        let layout = lair_explorer_layout(surface, 640, &rows, None, 0).unwrap();
        let palette = session_picker_palette(ResolvedTheme::default());
        for scale in [120, 240] {
            let canvas_width = surface.width * scale / 120 + 1;
            let canvas_height = surface.height * scale / 120 + 1;
            let draw = |selected| {
                let mut canvas = vec![0; (canvas_width * canvas_height * 4) as usize];
                paint(
                    &context,
                    &mut canvas,
                    canvas_width,
                    canvas_height,
                    scale,
                    &layout,
                    palette,
                    &rows,
                    selected,
                    true,
                    LairExplorerFilter::All,
                );
                canvas
            };
            let unselected = draw(None);
            let selected = draw(Some(id));
            let inset = LEVEL_INDENT.saturating_mul(scale).div_ceil(120);
            let lines = footer_lines(buffer_rect(layout.footer, scale), scale, inset);
            let row_rect = buffer_rect(layout.rows[0].rect, scale);
            let mut in_lines = [0_usize; FOOTER_STATUS_LINES];
            for (index, (a, b)) in selected
                .chunks_exact(4)
                .zip(unselected.chunks_exact(4))
                .enumerate()
            {
                if a == b {
                    continue;
                }
                let x = u32::try_from(index).unwrap() % canvas_width;
                let y = u32::try_from(index).unwrap() / canvas_width;
                if (row_rect.y..row_rect.y + row_rect.height).contains(&y) {
                    continue;
                }
                let line = lines[..FOOTER_STATUS_LINES]
                    .iter()
                    .position(|line| {
                        (line.y..line.y + line.height).contains(&y)
                            && (line.x..line.x + line.width).contains(&x)
                    })
                    .unwrap_or_else(|| panic!("footer words outside their lines at {x},{y}"));
                in_lines[line] += 1;
            }
            assert!(in_lines[0] > 0, "scale {scale}");
        }
    }

    /// The spinner moves only for a working row in view: one scrolled out
    /// of the list, or a list with no room, does not count.
    #[test]
    fn only_working_rows_in_view_keep_the_spinner_moving() {
        let mut rows = (0..30)
            .map(|_| row(NavigationNodeId::Lair(LairId::new())))
            .collect::<Vec<_>>();
        rows[29].agent_working = true;
        let content = |height| Rect {
            x: 0,
            y: 0,
            width: 700,
            height,
        };
        let top = lair_explorer_layout(content(400), 640, &rows, Some(rows[0].id), 0).unwrap();
        assert_eq!(top.visible_start, 0);
        assert!(!lair_explorer_shows_working(&top, &rows));
        let bottom = lair_explorer_layout(content(400), 640, &rows, Some(rows[29].id), 0).unwrap();
        assert!(lair_explorer_shows_working(&bottom, &rows));
        let no_room = lair_explorer_layout(
            content(HEADER_HEIGHT + HEADER_SPACER + FOOTER_HEIGHT),
            640,
            &rows,
            Some(rows[29].id),
            0,
        )
        .unwrap();
        assert_eq!(no_room.visible_capacity, 0);
        assert!(!lair_explorer_shows_working(&no_room, &rows));
    }

    /// A navigation notice is never lost: with no rows it takes the list's
    /// place, and with no room for a row, whether or not rows exist, the
    /// footer's context line carries it.
    #[test]
    fn notices_stay_visible_when_no_row_fits() {
        use super::super::picker::session_picker_palette;
        use crate::config::ResolvedTheme;

        let context = RenderContext::new(u16::MAX);
        let palette = session_picker_palette(ResolvedTheme::default());
        let some = [row(NavigationNodeId::Lair(LairId::new()))];
        for (rows, height) in [
            (&[][..], HEADER_HEIGHT + HEADER_SPACER + FOOTER_HEIGHT),
            (&some[..], HEADER_HEIGHT + HEADER_SPACER + FOOTER_HEIGHT),
            (
                &[][..],
                HEADER_HEIGHT + HEADER_SPACER + FOOTER_HEIGHT + ROW_HEIGHT,
            ),
        ] {
            let surface = Rect {
                x: 0,
                y: 0,
                width: 1000,
                height,
            };
            let layout = lair_explorer_layout(surface, 640, rows, None, 0).unwrap();
            let scale = 120;
            let width = surface.width + 1;
            let draw = |notice: Option<&str>| {
                let mut canvas = vec![0; (width * (height + 1) * 4) as usize];
                paint_lair_explorer(
                    &mut SessionPickerTextCache::default(),
                    &context,
                    &mut canvas,
                    width,
                    height + 1,
                    scale,
                    1,
                    &layout,
                    palette,
                    rows,
                    None,
                    "",
                    notice,
                    true,
                    LairExplorerFilter::All,
                    false,
                )
                .unwrap();
                canvas
            };
            let noticed = draw(Some("Navigation is stale · R retry"));
            let plain = draw(None);
            let inset = LEVEL_INDENT.saturating_mul(scale).div_ceil(120);
            let context_line =
                footer_lines(buffer_rect(layout.footer, scale), scale, inset)[FOOTER_STATUS_LINES];
            let list_top = layout.header.y + layout.header.height;
            let (mut in_list, mut in_context) = (0, 0);
            for (index, (a, b)) in noticed
                .chunks_exact(4)
                .zip(plain.chunks_exact(4))
                .enumerate()
            {
                if a == b {
                    continue;
                }
                let y = u32::try_from(index).unwrap() / width;
                if (context_line.y..context_line.y + context_line.height).contains(&y) {
                    in_context += 1;
                } else {
                    assert!((list_top..layout.footer.y).contains(&y), "notice at y {y}");
                    in_list += 1;
                }
            }
            let listed = rows.is_empty() && layout.visible_capacity > 0;
            assert_eq!(
                (in_list > 0, in_context > 0),
                (listed, !listed),
                "{} rows, capacity {}",
                rows.len(),
                layout.visible_capacity
            );
        }
    }

    /// VM E6-8: the hollow live dot as the rightmost column was clipped into
    /// a "C" at its cell edge. The columns are painted up to the divider, so
    /// a last glyph wider than its cell keeps all of its ink.
    #[test]
    fn the_last_column_keeps_its_whole_glyph() {
        use super::super::super::ChromeText;

        let context = RenderContext::new(u16::MAX);
        for scale in [120, 150, 180, 240] {
            let text = ChromeText::load_with_context("○", scale, &context).unwrap();
            let cell = text.frame.cell_width;
            let height = text.pixel_height();
            let ink = |clip_width: u32| {
                let width = cell * 4;
                let mut canvas = vec![0_u8; (width * height * 4) as usize];
                text.paint(
                    &mut canvas,
                    width,
                    height,
                    (0, 0),
                    Rect {
                        x: 0,
                        y: 0,
                        width: clip_width,
                        height,
                    },
                    0x00ff_ffff,
                );
                canvas
                    .chunks_exact(4)
                    .filter(|pixel| pixel.iter().any(|channel| *channel != 0))
                    .count()
            };
            let whole = ink(cell * 4);
            assert!(whole > 0);
            let inset = LEVEL_INDENT.saturating_mul(scale).div_ceil(120);
            let columns = Rect {
                x: 0,
                y: 0,
                width: cell,
                height,
            };
            let clip = columns_paint_clip(columns, cell + inset);
            assert_eq!((clip.x, clip.width), (0, cell + inset));
            assert_eq!(ink(clip.width), whole, "scale {scale}");
            // The clip never reaches past the divider.
            assert_eq!(columns_paint_clip(columns, 0).width, 0);
        }
    }

    /// RC2 item 7: the docked sidebar follows the Window's background alpha
    /// (foot.ini `alpha`, Omarchy glassy mode) as the terminal and the tab
    /// strip do. Its panel and fills keep the theme alpha; text, icons, the
    /// divider, the rules, and the selection rail stay opaque.
    /// `[colors] explorer-background` and `explorer-alpha` replace the
    /// panel's colour and coverage, and its text is chosen against the new
    /// colour. Without them the panel is the Window's glass, as above.
    #[test]
    fn explorer_background_and_alpha_overrides_replace_the_panel() {
        use crate::config::ChromeColorOverrides;
        let theme = ResolvedTheme {
            background: 0x80_40_20,
            background_alpha: u16::MAX / 2,
            foreground: 0xee_ee_ee,
            ..ResolvedTheme::default()
        };
        let light = theme.with_chrome_overrides(ChromeColorOverrides {
            explorer_background: Some(0xf0_f0_f0),
            explorer_alpha: Some(u16::MAX),
            ..ChromeColorOverrides::NONE
        });
        let palette = lair_explorer_palette(light);
        assert_eq!(palette.panel_fill, [0xf0, 0xf0, 0xf0, 0xff]);
        let expected = chrome_panel_palette(
            ResolvedTheme {
                background: 0xf0_f0_f0,
                ..light
            },
            ChromeSurface::Menu,
        );
        assert_eq!(palette.primary, expected.primary);
        assert_ne!(palette.primary, lair_explorer_palette(theme).primary);

        let alpha_only = theme.with_chrome_overrides(ChromeColorOverrides {
            explorer_alpha: Some(u16::MAX),
            ..ChromeColorOverrides::NONE
        });
        assert_eq!(
            lair_explorer_palette(alpha_only).panel_fill,
            [0x80, 0x40, 0x20, 0xff]
        );
    }

    #[test]
    fn explorer_respects_background_alpha_with_opaque_text_and_lines() {
        let theme = ResolvedTheme {
            background: 0x80_40_20,
            background_alpha: u16::MAX / 2,
            foreground: 0xee_ee_ee,
            ..ResolvedTheme::default()
        };
        let palette = lair_explorer_palette(theme);
        // The same premultiplied glass as the tab strip and the terminal.
        assert_eq!(
            palette.panel_fill,
            premultiplied_theme_rgba(theme.background, theme.background_alpha)
        );
        assert_eq!(palette.panel_fill, [0x40, 0x20, 0x10, 0x7f]);
        let opaque = lair_explorer_palette(ResolvedTheme::default());
        assert_eq!(opaque.panel_fill[3], 0xff);

        let id = NavigationNodeId::Lair(LairId::new());
        let mut selected = row(id);
        selected.live = true;
        let other = row(NavigationNodeId::Lair(LairId::new()));
        let rows = [selected, other];
        let surface = Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: HEADER_HEIGHT + HEADER_SPACER + FOOTER_HEIGHT + ROW_HEIGHT * 3,
        };
        let layout = lair_explorer_layout(surface, 640, &rows, Some(id), 0).unwrap();
        let (width, height) = (surface.width, surface.height);
        let mut canvas = vec![0; (width * height * 4) as usize];
        paint(
            &RenderContext::new(u16::MAX),
            &mut canvas,
            width,
            height,
            120,
            &layout,
            palette,
            &rows,
            Some(id),
            false,
            LairExplorerFilter::Live,
        );
        let pixel = |x: u32, y: u32| {
            let offset = ((y * width + x) * 4) as usize;
            <[u8; 4]>::try_from(&canvas[offset..offset + 4]).unwrap()
        };
        let bgra = |[r, g, b, a]: [u8; 4]| [b, g, r, a];
        // An empty row area, below the two rows: the panel's glass.
        let empty_y = layout.rows[1].rect.y + ROW_HEIGHT + ROW_HEIGHT / 2;
        assert_eq!(pixel(150, empty_y), bgra(palette.panel_fill));
        // The selected row's fill at the same alpha, inside the rail.
        let selected_y = layout.rows[0].rect.y + 1;
        assert_eq!(
            pixel(layout.panel.width - 4, selected_y),
            bgra(glass_rgba(palette.selected_fill, palette.panel_fill))
        );
        assert_eq!(
            glass_rgba(palette.selected_fill, palette.panel_fill)[3],
            0x7f
        );
        // The header is text on the glass: the active filter has no pill,
        // and no rule runs under the header line or through its spacer.
        let (_, live) = layout.filters[0];
        assert_eq!(pixel(live.x + 1, live.y + 1), bgra(palette.panel_fill));
        for y in layout.header.y + HEADER_HEIGHT - 1..layout.rows[0].rect.y {
            assert_eq!(pixel(150, y), bgra(palette.panel_fill), "y={y}");
        }
        // The rail and divider are opaque.
        assert_eq!(pixel(0, selected_y)[3], 0xff);
        assert_eq!(pixel(layout.panel.width - 1, empty_y)[3], 0xff);
        // Nothing in the panel is more transparent than the glass, and the
        // text's full-coverage pixels are opaque.
        let mut opaque_ink = 0;
        for y in 0..height {
            for x in 0..layout.panel.width {
                let [_, _, _, alpha] = pixel(x, y);
                assert!(alpha >= 0x7f, "{x},{y} alpha {alpha}");
                if alpha == 0xff && pixel(x, y) != bgra(rgba(palette.frame)) {
                    opaque_ink += 1;
                }
            }
        }
        assert!(opaque_ink > 0);
        // The name's ink in the second, unselected row is opaque.
        let name_y = layout.rows[1].rect.y..layout.rows[1].rect.y + ROW_HEIGHT;
        assert!(
            name_y
                .flat_map(|y| (24..120).map(move |x| (x, y)))
                .any(|(x, y)| pixel(x, y)[3] == 0xff)
        );
    }
}
