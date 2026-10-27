//! Responsive layout and CPU composition for the non-modal Lair explorer.
//!
//! One line per row: the current-row dot, the disclosure chevron, and the
//! name on the left, and fixed, right-aligned icon columns for agent access,
//! agent status, live state, and retention. The footer gives the selected
//! row one short status line, only what the row does not already show, then
//! a line for the search field, navigation notices, and who owns keyboard
//! input.
//!
//! The docked Explorer can also collapse to a rail about four cells wide:
//! one row per Lair, its initial and agent glyph, the current Lair marked by
//! the accent bar. The header's `«` collapses the panel and the rail's `»`
//! expands it.

use std::collections::HashSet;

use anyhow::Result;
use unicode_width::UnicodeWidthChar;

use crate::{
    config::ResolvedTheme,
    frontend::{LairExplorerAccess, LairExplorerFilter, LairExplorerRow},
    geometry::Rect,
    navigation_projection::NavigationNodeId,
    theme::ChromeTint,
};
use splinterm_core::LairRetention;

use super::{
    super::{
        ChromeIcon, ChromeSource, ChromeTextStyle, RenderContext, chrome_icons_available,
        fill_rect, premultiplied_theme_rgba, raster::premultiplied_rgba,
    },
    picker::{
        ChromeSurface, PickerTextAlignment, SessionPickerPalette, SessionPickerTextCache,
        SessionPickerTextKey, chrome_panel_palette, editable_text, paint_picker_source,
        paint_picker_source_tinted, paint_picker_source_within, paint_picker_text,
        picker_cell_width, truncate_picker_text,
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
/// One short status line for the selected row, then one context line.
const FOOTER_STATUS_LINES: usize = 1;
const FOOTER_PADDING: u32 = 4;
const FOOTER_HEIGHT: u32 = FOOTER_LINE_HEIGHT * 2 + FOOTER_PADDING;
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

/// The collapsed rail's narrowest width in logical px. It widens with the
/// chrome text: see [`lair_explorer_rail_width`].
const RAIL_WIDTH: u32 = 40;
/// The current-Lair accent bar and the divider beside a rail row's text, in
/// borders: the bar is three borders wide.
const RAIL_BAR_AND_BORDER: u32 = 4;
/// Chrome cells a rail row's text has: a Lair's initial, a blank cell, and
/// its agent glyph.
const RAIL_CELLS: u32 = 4;

/// The collapsed rail's width in logical px for a regular chrome cell
/// `chrome_cell` buffer px wide at `scale_120`: room for [`RAIL_CELLS`]
/// whole cells beside the accent bar and the divider as they are painted,
/// so an initial and its glyph keep a blank cell between them, and never
/// under `RAIL_WIDTH`. At a fractional scale it is one logical px wider,
/// for a panel edge that rounds away a buffer px.
#[must_use]
pub(crate) fn lair_explorer_rail_width(chrome_cell: u32, scale_120: u32) -> u32 {
    let scale_120 = scale_120.max(1);
    let border = scale_120.div_ceil(120).max(1);
    let buffer = chrome_cell
        .saturating_mul(RAIL_CELLS)
        .saturating_add(border.saturating_mul(RAIL_BAR_AND_BORDER));
    let slack = u32::from(scale_120 % 120 != 0);
    RAIL_WIDTH.max(
        buffer
            .saturating_mul(120)
            .div_ceil(scale_120)
            .saturating_add(slack),
    )
}

/// One regular chrome cell's width in buffer px at `scale_120`, as the
/// Explorer's text is drawn with `context`.
///
/// # Errors
/// Returns an error when the chrome font cannot be loaded.
pub(crate) fn lair_explorer_chrome_cell_width(
    context: &RenderContext,
    scale_120: u32,
) -> Result<u32> {
    let text = super::super::ChromeText::load_source_styled_with_context(
        &ChromeSource::from(" "),
        scale_120,
        ChromeTextStyle::Regular,
        context,
    )?;
    Ok(text.frame.cell_width.max(1))
}

/// The `«` target at the right end of the expanded header line, which the
/// filters give up. It reaches the divider.
const TOGGLE_WIDTH: u32 = 24;
/// Collapses the expanded Explorer to its rail.
const COLLAPSE_TEXT: &str = "«";
/// Expands the rail to the full Explorer.
const EXPAND_TEXT: &str = "»";
/// Marks a navigation notice or load state on the rail's header.
const RAIL_NOTICE_TEXT: &str = "!";

/// The footer's context line when no search or notice takes it: who owns
/// keyboard input and how to move.
const fn explorer_hint_text(focused: bool) -> &'static str {
    if focused {
        "↑↓ move · F filter · Esc terminal"
    } else {
        "Terminal has keys · click to move"
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

fn text_cells(text: &str) -> usize {
    text.chars()
        .map(|character| character.width().unwrap_or(0).min(2))
        .sum()
}

/// Fits the selected row's short status into one line of `cells` cells; it
/// never wraps. Text that does not fit is filled word by word, into the next
/// part where it has room, and ends in `…` after the last whole word that
/// fits, never after a dangling ` · `. So an agent's detail keeps its first
/// words on a narrow panel. Only a first word wider than the line is cut, at
/// a character boundary. The line never ends on a bare count such as the `1`
/// of `1 done`: it backs off to the word before.
fn fit_status_line(text: &str, cells: usize) -> String {
    if text_cells(text) <= cells {
        return text.to_owned();
    }
    // Every prefix that ends a word other than a count, without a separator
    // or space left at its end, that leaves a cell for the ellipsis. Prefix
    // widths never shrink, so the scan stops at the first one too wide.
    text.match_indices(' ')
        .map(|(end, _)| trim_status_tail(&text[..end]))
        .filter(|shown| !shown.is_empty() && !ends_on_count(shown))
        .take_while(|shown| text_cells(shown) < cells)
        .last()
        .map_or_else(|| cut_status_line(text, cells), |shown| format!("{shown}…"))
}

/// No whole word fits: cuts at a character boundary, leaving no separator,
/// space, or bare count before the ellipsis.
fn cut_status_line(text: &str, cells: usize) -> String {
    if cells == 0 {
        return String::new();
    }
    let cut = truncate_picker_text(text, cells);
    let mut shown = trim_status_tail(cut.strip_suffix('…').unwrap_or(&cut));
    while ends_on_count(shown) {
        shown = trim_status_tail(&shown[..shown.rfind(' ').unwrap_or(0)]);
    }
    format!("{shown}…")
}

/// `text` without a trailing ` · ` separator or spaces.
fn trim_status_tail(text: &str) -> &str {
    text.trim_end().trim_end_matches('·').trim_end()
}

/// Whether `text` ends on a word that is only a count, such as `1`.
fn ends_on_count(text: &str) -> bool {
    text.rsplit(' ')
        .next()
        .is_some_and(|word| !word.is_empty() && word.bytes().all(|byte| byte.is_ascii_digit()))
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
    /// Whether this is the collapsed rail: one row per Lair, with no filters
    /// and no footer, docked, or a drawer over the terminal in a Window too
    /// narrow to dock even the rail.
    pub(crate) rail: bool,
    /// The `«` (expanded) or `»` (rail) target on the header line. `None`
    /// when the header line is cut short.
    pub(crate) toggle: Option<Rect>,
}

/// Whether a rail `rail_width` logical px wide docked in `content` leaves
/// the terminal at least `minimum_terminal_width` logical px.
fn rail_docks(content: Rect, minimum_terminal_width: u32, rail_width: u32) -> bool {
    content
        .width
        .checked_sub(rail_width)
        .is_some_and(|rest| rest >= minimum_terminal_width)
}

/// The rows in view from `top` logical px below the panel's top through
/// `height` px: the visible start, how many rows fit, and their rects. The
/// selected row is kept in view.
fn row_slots(
    panel: Rect,
    top: u32,
    height: u32,
    rows: &[LairExplorerRow],
    selected: Option<NavigationNodeId>,
    requested_start: usize,
) -> (usize, usize, Vec<LairExplorerRowLayout>) {
    let capacity = usize::try_from(height / ROW_HEIGHT).unwrap_or(0);
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
                y: panel.y.saturating_add(top).saturating_add(
                    ROW_HEIGHT.saturating_mul(u32::try_from(slot).unwrap_or(u32::MAX)),
                ),
                width: panel.width,
                height: ROW_HEIGHT,
            },
        })
        .collect();
    (start, capacity, row_layouts)
}

/// The shown Explorer's layout with a header line `header` logical px
/// tall, or row-high for `None`: the rail, `rail_width` logical px wide,
/// when `collapsed`, else the expanded panel. Either one docks beside the
/// terminal when that leaves it `minimum_terminal_width` logical px, and is
/// otherwise a drawer over the terminal instead of squeezing it.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub(crate) fn lair_explorer_shown_layout(
    content: Rect,
    header: Option<u32>,
    minimum_terminal_width: u32,
    rail_width: u32,
    rows: &[LairExplorerRow],
    selected: Option<NavigationNodeId>,
    requested_start: usize,
    collapsed: bool,
) -> Option<LairExplorerLayout> {
    let header = header.unwrap_or(HEADER_HEIGHT);
    if collapsed {
        lair_explorer_rail_layout(
            content,
            header,
            minimum_terminal_width,
            rail_width,
            rows,
            selected,
            requested_start,
        )
    } else {
        lair_explorer_layout_with_header(
            content,
            header,
            minimum_terminal_width,
            rows,
            selected,
            requested_start,
        )
    }
}

/// The collapsed rail, `rail_width` logical px wide: the `»` header line, a
/// spacer, then one row per Lair to the panel's bottom. It docks when that
/// leaves the terminal `minimum_terminal_width` logical px, and is otherwise
/// a drawer over the terminal's left edge.
fn lair_explorer_rail_layout(
    content: Rect,
    header: u32,
    minimum_terminal_width: u32,
    rail_width: u32,
    rows: &[LairExplorerRow],
    selected: Option<NavigationNodeId>,
    requested_start: usize,
) -> Option<LairExplorerLayout> {
    if content.width == 0 || content.height == 0 || header == 0 {
        return None;
    }
    let docked = rail_docks(content, minimum_terminal_width, rail_width);
    let panel = Rect {
        width: rail_width.min(content.width),
        ..content
    };
    let header_height = header.min(panel.height);
    let list_top = header_height
        .saturating_add(HEADER_SPACER)
        .min(panel.height);
    let (start, capacity, row_layouts) = row_slots(
        panel,
        list_top,
        panel.height - list_top,
        rows,
        selected,
        requested_start,
    );
    let header_rect = Rect {
        height: header_height,
        ..panel
    };
    Some(LairExplorerLayout {
        panel,
        terminal: if docked {
            Rect {
                x: content.x.saturating_add(panel.width),
                width: content.width - panel.width,
                ..content
            }
        } else {
            content
        },
        header: header_rect,
        header_text_cell_cap: u32::MAX,
        footer: Rect {
            y: panel.y.saturating_add(panel.height),
            height: 0,
            ..panel
        },
        filters: Vec::new(),
        rows: row_layouts,
        visible_start: start,
        visible_capacity: capacity,
        mode: if docked {
            LairExplorerPresentationMode::Docked
        } else {
            LairExplorerPresentationMode::Drawer
        },
        rail: true,
        toggle: (header_height == header).then_some(header_rect),
    })
}

/// The expanded Explorer with the row-high header line.
#[cfg(test)]
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
fn lair_explorer_layout_with_header(
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
    let (start, capacity, row_layouts) = row_slots(
        panel,
        list_top,
        list_height,
        rows,
        selected,
        requested_start,
    );
    // The `«` takes the header line's right end: every Window can collapse
    // to the rail, which is a drawer where it cannot dock.
    let toggle = (header_height == header).then(|| {
        let width = TOGGLE_WIDTH.min(panel.width / 3);
        Rect {
            x: panel.x.saturating_add(panel.width - width),
            y: panel.y,
            width,
            height: header_height,
        }
    });
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
            header_filter_rects(panel, header_height, toggle.map_or(0, |rect| rect.width))
        } else {
            Vec::new()
        },
        rows: row_layouts,
        visible_start: start,
        visible_capacity: capacity,
        mode,
        rail: false,
        toggle,
    })
}

/// The filters take the header line right of the title, with the panel's
/// inset, or the `toggle` px of the `«` when wider, on their right. Each
/// one's target is its whole share of the line, `height` logical px tall.
fn header_filter_rects(panel: Rect, height: u32, toggle: u32) -> Vec<(LairExplorerFilter, Rect)> {
    let title = HEADER_TITLE_WIDTH.min(panel.width.saturating_mul(3) / 10);
    let inset = 8.min(panel.width / 2).max(toggle);
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

/// Whether `position` is on the header's `«` or `»`.
#[must_use]
pub(crate) fn lair_explorer_toggle_hit_test(
    layout: &LairExplorerLayout,
    position: (f64, f64),
) -> bool {
    layout.toggle.is_some_and(|rect| {
        position.0 >= f64::from(rect.x)
            && position.0 < f64::from(rect.x.saturating_add(rect.width))
            && position.1 >= f64::from(rect.y)
            && position.1 < f64::from(rect.y.saturating_add(rect.height))
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
    // A rail row has no disclosure: the whole row switches to its Lair.
    if layout.rail {
        return None;
    }
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
        (Some(LairExplorerAccess::Asking), true) => {
            source.push_tinted(ChromeTint::Asking, |source| {
                source.push_icon(ChromeIcon::LockQuestion);
            })
        }
        (Some(LairExplorerAccess::Reachable), false) => source.push_str("a "),
        (Some(LairExplorerAccess::Connected), false) => source.push_str("A "),
        (Some(LairExplorerAccess::Asking), false) => {
            source.push_tinted(ChromeTint::AskingText, |source| {
                source.push_str("A?");
            })
        }
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

/// The footer's text lines in buffer pixels: the status line, then the
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
/// `status_colors` is `[explorer] status-colors`.
#[must_use]
pub(crate) fn lair_explorer_palette(
    theme: ResolvedTheme,
    status_colors: bool,
) -> SessionPickerPalette {
    let mut panel_theme = theme;
    panel_theme.roles.surface = theme.explorer_background();
    let mut palette = chrome_panel_palette(panel_theme, ChromeSurface::Menu);
    palette.panel_fill = premultiplied_theme_rgba(palette.panel, theme.explorer_alpha());
    palette.with_status_colors(status_colors)
}

/// Where one header filter's text goes, in buffer px.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FilterTextClip {
    token: Rect,
    /// The `·` on the boundary before this filter; never before the first,
    /// and left out when either neighbouring share is narrower than a cell.
    separator: Option<Rect>,
}

/// Disjoint clips for the header filters' tokens and separators. `shares`
/// are the filters' buffer rects, left to right; `right` is where the
/// divider begins. A separator takes the `cell` px around its boundary,
/// `cell / 2` left of it and the rest right of it, and each token is inset by
/// the same amounts, so neither an odd cell nor a fractional scale makes two
/// clips overlap.
fn filter_text_clips(shares: &[Rect], cell: u32, right: u32) -> Vec<FilterTextClip> {
    let left_half = cell / 2;
    let right_half = cell - left_half;
    // Each share runs to the next one's start, which also absorbs any
    // overlap from rounding at fractional scales, and never past `right`.
    let spans: Vec<(u32, u32)> = shares
        .iter()
        .enumerate()
        .map(|(index, share)| {
            let start = share.x.min(right);
            let end = shares
                .get(index + 1)
                .map_or(share.x.saturating_add(share.width), |next| next.x)
                .min(right)
                .max(start);
            (start, end)
        })
        .collect();
    spans
        .iter()
        .enumerate()
        .map(|(index, &(start, end))| {
            let token_left = start.saturating_add(right_half).min(end);
            let token_right = end.saturating_sub(left_half).max(token_left);
            let separator = index
                .checked_sub(1)
                .map(|previous| spans[previous])
                .filter(|&(previous_start, previous_end)| {
                    previous_end.saturating_sub(previous_start) >= cell
                        && end.saturating_sub(start) >= cell
                })
                .map(|_| Rect {
                    x: start.saturating_sub(left_half),
                    y: shares[index].y,
                    width: cell,
                    height: shares[index].height,
                });
            FilterTextClip {
                token: Rect {
                    x: token_left,
                    y: shares[index].y,
                    width: token_right - token_left,
                    height: shares[index].height,
                },
                separator,
            }
        })
        .collect()
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
    if layout.rail {
        paint_rail(
            cache,
            context,
            &mut used,
            canvas,
            (canvas_width, canvas_height),
            scale_120,
            renderer_generation,
            layout,
            palette,
            rows,
            selected,
            (focused, status_message.is_some(), filter),
            (cell_width, border, right),
        )?;
        cache.finish_frame(used);
        return Ok(());
    }
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
    // boundary between neighbours. No clip overlaps another or reaches the
    // divider, however narrow the panel.
    let shares: Vec<Rect> = layout
        .filters
        .iter()
        .map(|(_, rect)| buffer_rect(*rect, scale_120))
        .collect();
    let clips = filter_text_clips(&shares, cell_width, right);
    for ((choice, _), clip) in layout.filters.iter().zip(&clips) {
        if let Some(separator) = clip.separator {
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
                separator,
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
            clip.token,
            PickerTextAlignment::Center,
            if active {
                palette.primary
            } else {
                palette.secondary
            },
            header_text_cap,
        )?;
    }
    paint_header_toggle(
        cache,
        context,
        &mut used,
        canvas,
        (canvas_width, canvas_height),
        scale_120,
        renderer_generation,
        layout,
        palette,
        right,
    )?;

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
        paint_picker_source_tinted(
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
            palette.row_tints(is_selected),
            u32::MAX,
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
    // The selected row is highlighted just above, so its line never repeats
    // the name; a row with nothing more to say leaves the line blank.
    if let Some(row) = selected
        .and_then(|id| rows.iter().find(|row| row.id == id))
        .filter(|row| !row.status.is_empty())
    {
        let status_line = lines[0];
        paint_picker_text(
            cache,
            context,
            &mut used,
            canvas,
            canvas_width,
            canvas_height,
            &fit_status_line(&row.status, line_cells(&status_line)),
            ChromeTextStyle::Regular,
            scale_120,
            renderer_generation,
            status_line,
            PickerTextAlignment::Left,
            palette.primary,
        )?;
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

/// The header toggle's text clip in buffer px: its target, never past the
/// divider at `right`.
fn toggle_clip(toggle: Rect, scale_120: u32, right: u32) -> Rect {
    let clip = buffer_rect(toggle, scale_120);
    Rect {
        width: clip.width.min(right.saturating_sub(clip.x)),
        ..clip
    }
}

/// Where the rail's notice `!` goes in buffer px: one chrome cell at the
/// left of its `header`, past the accent bar's width and short of the
/// divider at `right`.
fn rail_notice_clip(header: Rect, border: u32, cell_width: u32, right: u32) -> Rect {
    let left = header.x.saturating_add(border.saturating_mul(3));
    Rect {
        x: left,
        width: cell_width.min(right.saturating_sub(left)),
        ..header
    }
}

/// The rail header's filter mark: none for the default Live filter, else
/// the chosen filter's initial, so a rail under Saved or All never reads as
/// every Lair there is.
fn rail_filter_mark(filter: LairExplorerFilter) -> Option<&'static str> {
    match filter {
        LairExplorerFilter::Live => None,
        LairExplorerFilter::Saved => Some("s"),
        LairExplorerFilter::All => Some("a"),
    }
}

/// Where the rail's filter mark goes in buffer px: the last chrome cell of
/// its `header` before the divider at `right`, clear of the centred `»`.
fn rail_filter_clip(header: Rect, cell_width: u32, right: u32) -> Rect {
    let left = right.saturating_sub(cell_width).max(header.x);
    Rect {
        x: left,
        width: right.saturating_sub(left),
        ..header
    }
}

/// A rail row's buffer `rect` short of the divider at `right`.
fn rail_row_inner(rect: Rect, right: u32) -> Rect {
    Rect {
        width: rect.width.min(right.saturating_sub(rect.x)),
        ..rect
    }
}

/// Where a rail row's text goes: its `inner` rect right of the accent bar,
/// three borders wide.
fn rail_row_text(inner: Rect, border: u32) -> Rect {
    let bar = border.saturating_mul(3).min(inner.width);
    Rect {
        x: inner.x.saturating_add(bar),
        width: inner.width - bar,
        ..inner
    }
}

/// The header's `«`, or `»` on the rail, muted and centred in its target.
#[allow(clippy::too_many_arguments)]
fn paint_header_toggle(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    used: &mut HashSet<SessionPickerTextKey>,
    canvas: &mut [u8],
    (canvas_width, canvas_height): (u32, u32),
    scale_120: u32,
    renderer_generation: u64,
    layout: &LairExplorerLayout,
    palette: SessionPickerPalette,
    right: u32,
) -> Result<()> {
    let Some(toggle) = layout.toggle else {
        return Ok(());
    };
    paint_picker_source_within(
        cache,
        context,
        used,
        canvas,
        canvas_width,
        canvas_height,
        &ChromeSource::from(if layout.rail {
            EXPAND_TEXT
        } else {
            COLLAPSE_TEXT
        }),
        ChromeTextStyle::Regular,
        scale_120,
        renderer_generation,
        toggle_clip(toggle, scale_120, right),
        PickerTextAlignment::Center,
        palette.secondary,
        layout.header_text_cell_cap,
    )
}

/// A Lair's mark on the rail: the first letter or digit of its name that
/// takes a cell, as one upper-case character, else its `position` among
/// every Lair counting from 1, which no filter changes.
fn rail_initial(label: &str, position: usize) -> String {
    label
        .chars()
        .filter(|character| character.width().unwrap_or(0) > 0)
        .find(|character| character.is_alphanumeric())
        .and_then(|character| character.to_uppercase().next())
        .map_or_else(
            || position.saturating_add(1).to_string(),
            |character| character.to_string(),
        )
}

/// The longest start of `text` that fits `cells` cells.
fn leading_cells(text: &str, cells: usize) -> String {
    let mut used = 0;
    text.chars()
        .take_while(|character| {
            used += character.width().unwrap_or(0);
            used <= cells
        })
        .collect()
}

/// A rail row's text, fitted to `cells` cells so the painter never cuts
/// it: the Lair's initial, or `…` while its switch is pending, then its
/// agent glyph slot, a blank cell apart where that fits. An all-blank slot,
/// which keeps the initials lined up while other Lairs report, is left out
/// where it does not fit beside the whole initial. When the initial and a
/// glyph do not both fit, the glyph stays and the initial gives up cells,
/// down to none; a glyph wider than the row gives way to the initial.
fn rail_row_source(row: &LairExplorerRow, position: usize, cells: usize) -> ChromeSource {
    let initial = if row.pending {
        "…".to_owned()
    } else {
        rail_initial(&row.label, position)
    };
    let initial_cells = text_cells(&initial);
    let Some(glyph) = row.rail_glyph.as_ref().filter(|glyph| {
        glyph.cells() <= cells && (!glyph.is_blank() || initial_cells + glyph.cells() <= cells)
    }) else {
        return ChromeSource::from(leading_cells(&initial, cells));
    };
    let room = cells - glyph.cells();
    let mut source = ChromeSource::from(leading_cells(&initial, room));
    if initial_cells < room && !source.is_empty() {
        source.push_str(" ");
    }
    source.push_source(glyph);
    source
}

/// The rail's header: its `»`, the `!` while navigation has a notice, and
/// the filter mark under a filter other than Live.
#[allow(clippy::too_many_arguments)]
fn paint_rail_header(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    used: &mut HashSet<SessionPickerTextKey>,
    canvas: &mut [u8],
    (canvas_width, canvas_height): (u32, u32),
    (scale_120, renderer_generation): (u32, u64),
    layout: &LairExplorerLayout,
    palette: SessionPickerPalette,
    (notice, filter): (bool, LairExplorerFilter),
    (cell_width, border, right): (u32, u32, u32),
) -> Result<()> {
    paint_header_toggle(
        cache,
        context,
        used,
        canvas,
        (canvas_width, canvas_height),
        scale_120,
        renderer_generation,
        layout,
        palette,
        right,
    )?;
    // The rail has no footer for a navigation notice or a load state: a
    // `!` in the accent colour at the header's left marks one, and screen
    // readers get the message itself. A filter other than Live is named by
    // its muted initial at the header's right, so a short rail never reads
    // as every Lair there is.
    let header = buffer_rect(layout.header, scale_120);
    let marks = [
        notice.then(|| {
            (
                RAIL_NOTICE_TEXT,
                ChromeTextStyle::Bold,
                rail_notice_clip(header, border, cell_width, right),
                palette.selected_rail,
            )
        }),
        rail_filter_mark(filter).map(|mark| {
            (
                mark,
                ChromeTextStyle::Regular,
                rail_filter_clip(header, cell_width, right),
                palette.secondary,
            )
        }),
    ];
    for (text, style, clip, color) in marks.into_iter().flatten() {
        paint_picker_source_within(
            cache,
            context,
            used,
            canvas,
            canvas_width,
            canvas_height,
            &ChromeSource::from(text),
            style,
            scale_120,
            renderer_generation,
            clip,
            PickerTextAlignment::Center,
            color,
            layout.header_text_cell_cap,
        )?;
    }
    Ok(())
}

/// The collapsed rail below its `»`: one row per Lair, its initial and
/// agent glyph centred, the current Lair marked by the accent bar the
/// expanded panel gives its selection, and the selection filled only while
/// the Explorer has the keys. `metrics` are the chrome cell width, the
/// border, and the divider's left edge in buffer px.
#[allow(clippy::too_many_arguments)]
fn paint_rail(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    used: &mut HashSet<SessionPickerTextKey>,
    canvas: &mut [u8],
    (canvas_width, canvas_height): (u32, u32),
    scale_120: u32,
    renderer_generation: u64,
    layout: &LairExplorerLayout,
    palette: SessionPickerPalette,
    rows: &[LairExplorerRow],
    selected: Option<NavigationNodeId>,
    (focused, notice, filter): (bool, bool, LairExplorerFilter),
    (cell_width, border, right): (u32, u32, u32),
) -> Result<()> {
    paint_rail_header(
        cache,
        context,
        used,
        canvas,
        (canvas_width, canvas_height),
        (scale_120, renderer_generation),
        layout,
        palette,
        (notice, filter),
        (cell_width, border, right),
    )?;
    for (layout_row, row) in layout
        .rows
        .iter()
        .zip(rows.iter().skip(layout.visible_start))
    {
        let rect = buffer_rect(layout_row.rect, scale_120);
        let inner = rail_row_inner(rect, right);
        let is_selected = focused && selected == Some(row.id);
        if is_selected {
            fill_rect(
                canvas,
                canvas_width,
                canvas_height,
                tuple(inner),
                glass_rgba(palette.selected_fill, palette.panel_fill),
            );
        }
        let bar = border.saturating_mul(3).min(inner.width);
        if row.current {
            fill_rect(
                canvas,
                canvas_width,
                canvas_height,
                (
                    i32::try_from(rect.x).unwrap_or(i32::MAX),
                    i32::try_from(rect.y).unwrap_or(i32::MAX),
                    bar,
                    rect.height,
                ),
                rgba(palette.selected_rail),
            );
        }
        let text = rail_row_text(inner, border);
        let cells = usize::try_from(text.width / cell_width.max(1)).unwrap_or(0);
        let color = match (is_selected, row.enabled) {
            (true, true) => palette.selected_primary,
            (true, false) => palette.selected_secondary,
            (false, true) => palette.primary,
            (false, false) => palette.secondary,
        };
        paint_picker_source_tinted(
            cache,
            context,
            used,
            canvas,
            canvas_width,
            canvas_height,
            &rail_row_source(row, row.ordinal, cells),
            ChromeTextStyle::Regular,
            scale_120,
            renderer_generation,
            text,
            PickerTextAlignment::Center,
            color,
            palette.row_tints(is_selected),
            u32::MAX,
        )?;
    }
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
            rail_glyph: None,
            agent_working: false,
            live: false,
            retention: None,
            access: None,
            status: "stopped · saved".into(),
            accessible_status: "Stopped · Saved".into(),
            level: 1,
            ordinal: 0,
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

    fn lair_rows(count: usize) -> Vec<LairExplorerRow> {
        (0..count)
            .map(|_| row(NavigationNodeId::Lair(LairId::new())))
            .collect()
    }

    fn contains(rect: Rect, (x, y): (f64, f64)) -> bool {
        x >= f64::from(rect.x)
            && x < f64::from(rect.x + rect.width)
            && y >= f64::from(rect.y)
            && y < f64::from(rect.y + rect.height)
    }

    /// The rail is about four cells wide and docked: its `»` header line,
    /// the spacer, then one row per Lair down to the panel's bottom, with no
    /// filters and no footer. The terminal takes the rest of the width.
    #[test]
    fn rail_is_narrow_docked_and_lists_one_row_per_lair() {
        let rows = lair_rows(3);
        let content = Rect {
            x: 0,
            y: 34,
            width: 1000,
            height: 600,
        };
        let rail = lair_explorer_shown_layout(
            content,
            None,
            640,
            RAIL_WIDTH,
            &rows,
            Some(rows[2].id),
            0,
            true,
        )
        .unwrap();
        assert!(rail.rail);
        assert_eq!(rail.mode, LairExplorerPresentationMode::Docked);
        assert_eq!(
            rail.panel,
            Rect {
                width: RAIL_WIDTH,
                ..content
            }
        );
        assert_eq!(
            rail.terminal,
            Rect {
                x: RAIL_WIDTH,
                width: 1000 - RAIL_WIDTH,
                ..content
            }
        );
        assert_eq!(rail.header.height, HEADER_HEIGHT);
        assert_eq!(rail.toggle, Some(rail.header));
        assert!(rail.filters.is_empty());
        assert_eq!(rail.footer.height, 0);
        assert_eq!(rail.footer.y, content.y + content.height);
        assert_eq!(
            rail.rows.iter().map(|laid| laid.id).collect::<Vec<_>>(),
            rows.iter().map(|row| row.id).collect::<Vec<_>>()
        );
        assert_eq!(
            rail.rows[0].rect.y,
            content.y + HEADER_HEIGHT + HEADER_SPACER
        );
        for (index, laid) in rail.rows.iter().enumerate() {
            assert_eq!((laid.rect.x, laid.rect.width), (0, RAIL_WIDTH));
            assert_eq!(laid.rect.height, ROW_HEIGHT);
            if let Some(next) = rail.rows.get(index + 1) {
                assert_eq!(laid.rect.y + ROW_HEIGHT, next.rect.y);
            }
        }
        // With no footer the rail fits more rows than the expanded panel.
        let many = lair_rows(40);
        let rail = lair_explorer_shown_layout(content, None, 640, RAIL_WIDTH, &many, None, 0, true)
            .unwrap();
        let expanded =
            lair_explorer_shown_layout(content, None, 640, RAIL_WIDTH, &many, None, 0, false)
                .unwrap();
        assert!(!expanded.rail);
        assert_eq!(
            rail.visible_capacity,
            usize::try_from((600 - HEADER_HEIGHT - HEADER_SPACER) / ROW_HEIGHT).unwrap()
        );
        assert!(rail.visible_capacity > expanded.visible_capacity);
        // The selected Lair stays in view.
        let last = many.last().unwrap().id;
        let rail =
            lair_explorer_shown_layout(content, None, 640, RAIL_WIDTH, &many, Some(last), 0, true)
                .unwrap();
        assert_eq!(rail.rows.last().unwrap().id, last);
    }

    /// A rail row is hit across its whole width and height and has no
    /// disclosure; the `»` is the header line. In the expanded panel the `«`
    /// takes the header line's right end, which the filters give up, and is
    /// shown only while collapsing would dock a rail.
    #[test]
    fn rail_rows_and_the_header_toggles_hit_test_apart() {
        let rows = lair_rows(2);
        let content = Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 400,
        };
        let rail = lair_explorer_shown_layout(content, None, 640, RAIL_WIDTH, &rows, None, 0, true)
            .unwrap();
        for (laid, entry) in rail.rows.iter().zip(&rows) {
            let top = f64::from(laid.rect.y);
            let bottom = f64::from(laid.rect.y + laid.rect.height) - 0.5;
            for x in [0.0, 20.0, f64::from(RAIL_WIDTH) - 0.5] {
                for y in [top, bottom] {
                    assert_eq!(lair_explorer_hit_test(&rail, (x, y)), Some(entry.id));
                    assert_eq!(
                        lair_explorer_disclosure_hit_test(&rail, &rows, (x, y)),
                        None
                    );
                    assert!(!lair_explorer_toggle_hit_test(&rail, (x, y)));
                }
            }
        }
        assert!(lair_explorer_toggle_hit_test(&rail, (20.0, 5.0)));
        assert_eq!(lair_explorer_hit_test(&rail, (20.0, 5.0)), None);
        assert_eq!(lair_explorer_filter_hit_test(&rail, (20.0, 5.0)), None);
        // Past the rail is the terminal.
        let beside = (f64::from(RAIL_WIDTH), f64::from(rail.rows[0].rect.y + 1));
        assert_eq!(lair_explorer_hit_test(&rail, beside), None);

        let expanded =
            lair_explorer_shown_layout(content, None, 640, RAIL_WIDTH, &rows, None, 0, false)
                .unwrap();
        let toggle = expanded.toggle.unwrap();
        assert_eq!(
            toggle.x + toggle.width,
            expanded.panel.x + expanded.panel.width
        );
        assert_eq!((toggle.y, toggle.height), (0, HEADER_HEIGHT));
        assert_eq!(expanded.filters.len(), 3);
        let (_, last_filter) = expanded.filters[2];
        assert_eq!(last_filter.x + last_filter.width, toggle.x);
        let point = (f64::from(toggle.x) + 2.0, 5.0);
        assert!(lair_explorer_toggle_hit_test(&expanded, point));
        assert_eq!(lair_explorer_filter_hit_test(&expanded, point), None);
        assert!(!lair_explorer_toggle_hit_test(
            &expanded,
            (f64::from(toggle.x) - 1.0, 5.0)
        ));
        for laid in &expanded.rows {
            let corner = (
                f64::from(laid.rect.x + laid.rect.width - 1),
                f64::from(laid.rect.y),
            );
            assert!(!contains(toggle, corner));
            assert!(!lair_explorer_toggle_hit_test(&expanded, corner));
        }
        // Every expanded panel can collapse, a drawer too: the rail docks
        // where it fits and is a drawer itself where it does not.
        for width in [700, 650] {
            let drawer = Rect { width, ..content };
            let layout =
                lair_explorer_shown_layout(drawer, None, 640, RAIL_WIDTH, &rows, None, 0, false)
                    .unwrap();
            assert_eq!(layout.mode, LairExplorerPresentationMode::Drawer);
            assert!(layout.toggle.is_some());
        }
        // A header cut short by a tiny Window shows no toggle.
        let short = Rect {
            height: HEADER_HEIGHT - 1,
            ..content
        };
        for collapsed in [false, true] {
            let layout =
                lair_explorer_shown_layout(short, None, 640, RAIL_WIDTH, &rows, None, 0, collapsed)
                    .unwrap();
            assert_eq!(layout.toggle, None, "collapsed={collapsed}");
        }
    }

    /// A rail row reads as the Lair's initial, upper case, else its place in
    /// the list, then its glyph a blank cell apart when there is room; `…`
    /// stands in while its switch is pending.
    #[test]
    fn rail_rows_show_an_initial_and_the_agent_glyph() {
        let mut lair = row(NavigationNodeId::Lair(LairId::new()));
        assert_eq!(rail_row_source(&lair, 0, 4).text(), "W");
        lair.label = "-- 9lives".into();
        assert_eq!(rail_row_source(&lair, 0, 4).text(), "9");
        lair.label = "··".into();
        assert_eq!(rail_row_source(&lair, 6, 4).text(), "7");
        lair.label = "work".into();
        let mut glyph = ChromeSource::default();
        glyph.push_icon(ChromeIcon::MessageQuestion);
        lair.rail_glyph = Some(glyph);
        assert_eq!(
            rail_row_source(&lair, 0, 4).describe(),
            "W [message-question]"
        );
        assert_eq!(
            rail_row_source(&lair, 0, 3).describe(),
            "W[message-question]"
        );
        lair.pending = true;
        assert_eq!(
            rail_row_source(&lair, 0, 4).describe(),
            "… [message-question]"
        );
        // A Lair with nothing reporting keeps the blank slot, so every
        // initial in the rail lines up.
        lair.pending = false;
        lair.rail_glyph = Some(ChromeSource::from("  "));
        assert_eq!(rail_row_source(&lair, 0, 4).text(), "W   ");
        // Zero-width marks never stand in for the initial, and one upper-case
        // character stands for a letter that upper-cases to more.
        lair.rail_glyph = None;
        lair.label = "\u{200b}\u{301}straße".into();
        assert_eq!(rail_row_source(&lair, 0, 4).text(), "S");
        lair.label = "ßeta".into();
        assert_eq!(rail_row_source(&lair, 0, 4).text(), "S");
    }

    /// A rail row's text always fits its cells, so the painter never cuts it
    /// to `W…`: a wide initial (a two-digit place, a CJK letter) and the
    /// glyph share the row, the initial giving up cells first; an all-blank
    /// slot is dropped where the whole initial would not fit beside it; and
    /// a glyph wider than the row gives way to the initial.
    #[test]
    fn rail_row_text_fits_its_cells_and_keeps_the_glyph() {
        let mut lair = row(NavigationNodeId::Lair(LairId::new()));
        let mut glyph = ChromeSource::default();
        glyph.push_icon(ChromeIcon::MessageQuestion);
        let glyph_cells = glyph.cells();
        let blank = ChromeSource::from(" ".repeat(glyph_cells).as_str());
        for label in ["work", "··", "語り", "ßeta"] {
            for position in [0, 9, 99, 999] {
                for slot in [None, Some(glyph.clone()), Some(blank.clone())] {
                    for cells in 0..=6 {
                        lair.label = label.into();
                        lair.rail_glyph.clone_from(&slot);
                        let source = rail_row_source(&lair, position, cells);
                        let case = format!("{label:?} #{position} {slot:?} in {cells}");
                        assert!(source.cells() <= cells, "{case}: {source:?}");
                        assert!(!source.text().contains('…'), "{case}");
                        if slot.as_ref() == Some(&glyph) && glyph_cells <= cells {
                            assert!(
                                source.describe().ends_with("[message-question]"),
                                "{case}: the glyph stays"
                            );
                        }
                    }
                }
            }
        }
        // Two-digit places and wide letters next to the glyph in four cells.
        lair.rail_glyph = Some(glyph.clone());
        lair.label = "··".into();
        assert_eq!(
            rail_row_source(&lair, 11, 4).describe(),
            "12[message-question]"
        );
        assert_eq!(
            rail_row_source(&lair, 11, 3).describe(),
            "1[message-question]"
        );
        lair.label = "語り".into();
        assert_eq!(
            rail_row_source(&lair, 0, 4).describe(),
            "語[message-question]"
        );
        assert_eq!(
            rail_row_source(&lair, 0, 3).describe(),
            "[message-question]"
        );
        // The blank slot gives way to a wide initial.
        lair.rail_glyph = Some(blank);
        assert_eq!(rail_row_source(&lair, 11, 2).text(), "語");
        // A glyph wider than the row gives way to the initial.
        lair.rail_glyph = Some(glyph);
        lair.label = "work".into();
        assert_eq!(rail_row_source(&lair, 0, 1).text(), "W");
    }

    /// The rail widens with the chrome text so four cells always fit beside
    /// The rail widens with the chrome text so four whole cells always fit
    /// beside the accent bar and the divider as they are painted, at every
    /// scale and wherever the panel starts, so an initial and a two-cell
    /// glyph keep a blank cell between them. It is never narrower than
    /// `RAIL_WIDTH`.
    #[test]
    fn rail_width_follows_the_chrome_cell() {
        assert_eq!(lair_explorer_rail_width(0, 120), RAIL_WIDTH);
        assert_eq!(lair_explorer_rail_width(9, 120), RAIL_WIDTH);
        assert_eq!(lair_explorer_rail_width(14, 120), 14 * RAIL_CELLS + 4);
        assert_eq!(lair_explorer_rail_width(28, 240), 14 * RAIL_CELLS + 4);
        let mut lair = row(NavigationNodeId::Lair(LairId::new()));
        let mut glyph = ChromeSource::default();
        glyph.push_icon(ChromeIcon::MessageQuestion);
        lair.rail_glyph = Some(glyph);
        for scale in [120, 144, 150, 180, 210, 240, 300] {
            let border = u32::div_ceil(scale, 120);
            for cell in 6..=48 {
                let width = lair_explorer_rail_width(cell, scale);
                for x in [0, 1, 3, 7] {
                    let panel = buffer_rect(
                        Rect {
                            x,
                            y: 0,
                            width,
                            height: ROW_HEIGHT,
                        },
                        scale,
                    );
                    let right = panel.x + panel.width - border;
                    let text = rail_row_text(rail_row_inner(panel, right), border);
                    let what = format!("cell {cell} at scale {scale} from x {x}");
                    assert!(text.width / cell >= RAIL_CELLS, "{what}");
                    let cells = usize::try_from(text.width / cell).unwrap();
                    assert_eq!(
                        rail_row_source(&lair, 0, cells).describe(),
                        "W [message-question]",
                        "{what}"
                    );
                }
            }
        }
        let content = Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 300,
        };
        let rows = lair_rows(2);
        let wide = lair_explorer_rail_width(14, 120);
        let rail =
            lair_explorer_shown_layout(content, None, 640, wide, &rows, None, 0, true).unwrap();
        assert_eq!(rail.panel.width, wide);
        assert_eq!(rail.terminal.x, wide);
        assert_eq!(rail.terminal.width, 1000 - wide);
        assert_eq!(rail.toggle.map(|toggle| toggle.width), Some(wide));
        for laid in &rail.rows {
            assert_eq!(laid.rect.width, wide);
            let inside = (f64::from(wide) - 0.5, f64::from(laid.rect.y));
            assert_eq!(lair_explorer_hit_test(&rail, inside), Some(laid.id));
            assert_eq!(
                lair_explorer_hit_test(&rail, (f64::from(wide), f64::from(laid.rect.y))),
                None
            );
        }
        // The wider rail's own width decides whether it docks.
        let edge = Rect {
            width: 640 + wide,
            ..content
        };
        let docked =
            lair_explorer_shown_layout(edge, None, 640, wide, &rows, None, 0, true).unwrap();
        assert_eq!(docked.mode, LairExplorerPresentationMode::Docked);
        let narrower = Rect {
            width: 640 + wide - 1,
            ..content
        };
        let drawer =
            lair_explorer_shown_layout(narrower, None, 640, wide, &rows, None, 0, true).unwrap();
        assert_eq!(drawer.mode, LairExplorerPresentationMode::Drawer);
    }

    /// A Window too narrow to dock even the rail shows the rail itself as a
    /// drawer over the terminal's left edge, with its `»` and one row per
    /// Lair, so the collapsed state and what is drawn always agree. The
    /// terminal keeps the whole width under it.
    #[test]
    fn a_rail_that_cannot_dock_is_a_drawer_over_the_terminal() {
        let rows = lair_rows(3);
        let content = Rect {
            x: 0,
            y: 30,
            width: 600,
            height: 300,
        };
        let rail = lair_explorer_shown_layout(content, None, 640, RAIL_WIDTH, &rows, None, 0, true)
            .unwrap();
        assert!(rail.rail);
        assert_eq!(rail.mode, LairExplorerPresentationMode::Drawer);
        assert_eq!(rail.terminal, content);
        assert_eq!(
            rail.panel,
            Rect {
                width: RAIL_WIDTH,
                ..content
            }
        );
        assert_eq!(rail.toggle, Some(rail.header));
        assert!(rail.filters.is_empty());
        assert_eq!(
            rail.rows.iter().map(|laid| laid.id).collect::<Vec<_>>(),
            rows.iter().map(|row| row.id).collect::<Vec<_>>()
        );
        for laid in &rail.rows {
            assert_eq!(
                lair_explorer_disclosure_hit_test(&rail, &rows, (5.0, f64::from(laid.rect.y))),
                None
            );
        }
        // A Window narrower than the rail gets a rail as wide as it is.
        let sliver = Rect {
            width: 30,
            ..content
        };
        let rail = lair_explorer_shown_layout(sliver, None, 640, RAIL_WIDTH, &rows, None, 0, true)
            .unwrap();
        assert!(rail.rail);
        assert_eq!(rail.panel.width, 30);
    }

    /// The rail has no footer, so a navigation notice or load state is
    /// marked by an accent `!` at its header's left, apart from the `»`;
    /// nothing else changes.
    #[test]
    fn rail_header_marks_a_notice() {
        use super::super::picker::session_picker_palette;

        let rows = lair_rows(2);
        let palette = session_picker_palette(ResolvedTheme::default());
        let content = Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 200,
        };
        let layout =
            lair_explorer_shown_layout(content, None, 640, RAIL_WIDTH, &rows, None, 0, true)
                .unwrap();
        let render = RenderContext::new(u16::MAX);
        for scale in [120, 240] {
            let width = 1000 * scale / 120;
            let height = 200 * scale / 120 + 1;
            let draw = |notice: Option<&str>| {
                let mut canvas = vec![0; (width * height * 4) as usize];
                paint_lair_explorer(
                    &mut SessionPickerTextCache::default(),
                    &render,
                    &mut canvas,
                    width,
                    height,
                    scale,
                    1,
                    &layout,
                    palette,
                    &rows,
                    None,
                    "",
                    notice,
                    false,
                    LairExplorerFilter::Live,
                    false,
                )
                .unwrap();
                canvas
            };
            let plain = draw(None);
            let noticed = draw(Some("Navigation is stale · R retry"));
            let header = buffer_rect(layout.header, scale);
            let toggle_left = header.x + header.width / 2 - header.width / 5;
            let mut marked = 0;
            for (index, (a, b)) in noticed
                .chunks_exact(4)
                .zip(plain.chunks_exact(4))
                .enumerate()
            {
                if a == b {
                    continue;
                }
                let x = u32::try_from(index).unwrap() % width;
                let y = u32::try_from(index).unwrap() / width;
                assert!(y < header.y + header.height, "scale {scale}: mark at y {y}");
                assert!(
                    x < toggle_left,
                    "scale {scale}: mark at x {x} reaches the `»`"
                );
                marked += 1;
            }
            assert!(marked > 0, "scale {scale}");
        }
    }

    /// Under a filter other than Live the rail header names it by a muted
    /// initial at its right, `s` for Saved and `a` for All, apart from the
    /// `»` and the notice `!`, so a short rail never reads as every Lair
    /// there is. Live, the default, adds no mark.
    #[test]
    fn rail_header_marks_a_filter_other_than_live() {
        use super::super::picker::session_picker_palette;

        assert_eq!(rail_filter_mark(LairExplorerFilter::Live), None);
        assert_eq!(rail_filter_mark(LairExplorerFilter::Saved), Some("s"));
        assert_eq!(rail_filter_mark(LairExplorerFilter::All), Some("a"));
        let rows = lair_rows(2);
        let palette = session_picker_palette(ResolvedTheme::default());
        let content = Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 200,
        };
        let layout =
            lair_explorer_shown_layout(content, None, 640, RAIL_WIDTH, &rows, None, 0, true)
                .unwrap();
        let render = RenderContext::new(u16::MAX);
        for scale in [120, 240] {
            let width = 1000 * scale / 120;
            let height = 200 * scale / 120 + 1;
            let draw = |filter| {
                let mut canvas = vec![0; (width * height * 4) as usize];
                paint(
                    &render,
                    &mut canvas,
                    width,
                    height,
                    scale,
                    &layout,
                    palette,
                    &rows,
                    None,
                    false,
                    filter,
                );
                canvas
            };
            let live = draw(LairExplorerFilter::Live);
            let header = buffer_rect(layout.header, scale);
            let toggle_right = header.x + header.width / 2 + header.width / 5;
            let mut marks = Vec::new();
            for filter in [LairExplorerFilter::Saved, LairExplorerFilter::All] {
                let marked = draw(filter);
                let mut changed = 0;
                for (index, (a, b)) in marked.chunks_exact(4).zip(live.chunks_exact(4)).enumerate()
                {
                    if a == b {
                        continue;
                    }
                    let x = u32::try_from(index).unwrap() % width;
                    let y = u32::try_from(index).unwrap() / width;
                    let what = format!("{filter:?} at scale {scale}: mark at ({x}, {y})");
                    assert!(y < header.y + header.height, "{what}");
                    assert!(x > toggle_right, "{what} reaches the `»`");
                    assert!(x < header.x + header.width, "{what} passes the divider");
                    changed += 1;
                }
                assert!(changed > 0, "{filter:?} at scale {scale}");
                marks.push(marked);
            }
            assert_ne!(marks[0], marks[1], "scale {scale}");
        }
        // The mark's cell never meets the notice's.
        for (cell, border) in [(9, 1), (14, 2), (24, 2)] {
            let header = Rect {
                x: 0,
                y: 0,
                width: cell * RAIL_CELLS + border * RAIL_BAR_AND_BORDER,
                height: HEADER_HEIGHT,
            };
            let right = header.width - border;
            let notice = rail_notice_clip(header, border, cell, right);
            let filter = rail_filter_clip(header, cell, right);
            assert!(notice.x + notice.width <= filter.x, "cell {cell}");
            assert_eq!(filter.x + filter.width, right);
        }
    }

    /// The current Lair carries the accent bar the expanded panel gives its
    /// selection; the selection is filled only while the Explorer has the
    /// keys. The rail keeps its divider and paints its `»`.
    #[test]
    fn rail_marks_the_current_lair_with_the_accent_bar() {
        use super::super::picker::session_picker_palette;

        let mut rows = lair_rows(3);
        rows[1].current = true;
        let mut palette = session_picker_palette(ResolvedTheme::default());
        palette.frame = 0x0012_3456;
        palette.focused_frame = 0x0098_abcd;
        let bgra = |[r, g, b, a]: [u8; 4]| [b, g, r, a];
        let content = Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 200,
        };
        let layout = lair_explorer_shown_layout(
            content,
            None,
            640,
            RAIL_WIDTH,
            &rows,
            Some(rows[2].id),
            0,
            true,
        )
        .unwrap();
        for scale in [120, 150, 240] {
            for focused in [false, true] {
                let width = 1000 * scale / 120;
                let height = 200 * scale / 120 + 1;
                let draw = |rows: &[LairExplorerRow]| {
                    let mut canvas = vec![0; (width * height * 4) as usize];
                    paint(
                        &RenderContext::new(u16::MAX),
                        &mut canvas,
                        width,
                        height,
                        scale,
                        &layout,
                        palette,
                        rows,
                        Some(rows[2].id),
                        focused,
                        LairExplorerFilter::Live,
                    );
                    canvas
                };
                let canvas = draw(&rows);
                let pixel = |x: u32, y: u32| {
                    let offset = ((y * width + x) * 4) as usize;
                    <[u8; 4]>::try_from(&canvas[offset..offset + 4]).unwrap()
                };
                let case = format!("scale={scale} focused={focused}");
                let row_top = |index: usize| buffer_rect(layout.rows[index].rect, scale).y + 1;
                assert_eq!(
                    pixel(0, row_top(1)),
                    bgra(rgba(palette.selected_rail)),
                    "{case}"
                );
                assert_eq!(pixel(0, row_top(0)), bgra(palette.panel_fill), "{case}");
                let panel = buffer_rect(layout.panel, scale);
                let border = scale.div_ceil(120).max(1);
                let right = panel.x + panel.width - border;
                assert_eq!(
                    pixel(right - 1, row_top(2)),
                    bgra(if focused {
                        glass_rgba(palette.selected_fill, palette.panel_fill)
                    } else {
                        palette.panel_fill
                    }),
                    "{case}"
                );
                let frame = rgba(if focused {
                    palette.focused_frame
                } else {
                    palette.frame
                });
                for y in [0, row_top(1), panel.y + panel.height - 1] {
                    for x in right..panel.x + panel.width {
                        assert_eq!(pixel(x, y), bgra(frame), "{case} divider {x},{y}");
                    }
                }
                // Nothing reaches past the rail into the terminal.
                assert!(
                    (0..height).all(|y| pixel(panel.x + panel.width, y) == [0; 4]),
                    "{case}"
                );
                // The `»` and the initials are painted.
                let header = buffer_rect(layout.header, scale);
                let header_ink = (header.y..header.y + header.height)
                    .any(|y| (panel.x..right).any(|x| pixel(x, y) != bgra(palette.panel_fill)));
                assert!(header_ink, "{case}: no »");
                let mut blank = rows.clone();
                for entry in &mut blank {
                    entry.label = " ".into();
                }
                assert_ne!(draw(&blank), canvas, "{case}: no initials");
            }
        }
    }

    /// The header filters' token and separator clips never overlap one
    /// another or reach the divider: odd and even cells, fractional scales,
    /// and shares narrower than a cell.
    #[test]
    fn filter_text_clips_are_disjoint_and_stay_left_of_the_divider() {
        for width in [1, 2, 20, 40, 60, 100, 200, 288] {
            for scale in [120, 150, 175, 180, 240] {
                for cell in [1, 6, 7, 8, 9, 15] {
                    let case = format!("width={width} scale={scale} cell={cell}");
                    let content = Rect {
                        x: 3,
                        y: 0,
                        width,
                        height: 400,
                    };
                    let layout = lair_explorer_layout(content, 0, &[], None, 0).unwrap();
                    let panel = buffer_rect(layout.panel, scale);
                    let border = scale.div_ceil(120).max(1);
                    let right = panel.x + panel.width.saturating_sub(border);
                    let shares: Vec<Rect> = layout
                        .filters
                        .iter()
                        .map(|(_, rect)| buffer_rect(*rect, scale))
                        .collect();
                    let clips = filter_text_clips(&shares, cell, right);
                    assert_eq!(clips.len(), shares.len(), "{case}");
                    assert!(clips.first().is_none_or(|clip| clip.separator.is_none()));
                    let mut spans: Vec<(u32, u32)> = clips
                        .iter()
                        .flat_map(|clip| clip.separator.into_iter().chain([clip.token]))
                        .filter(|rect| rect.width > 0)
                        .map(|rect| (rect.x, rect.x + rect.width))
                        .collect();
                    for &(left, end) in &spans {
                        assert!(left >= panel.x && end <= right, "{case}: {left}..{end}");
                    }
                    spans.sort_unstable();
                    for pair in spans.windows(2) {
                        assert!(pair[0].1 <= pair[1].0, "{case}: {pair:?}");
                    }
                    for (index, clip) in clips.iter().enumerate().skip(1) {
                        if clip.separator.is_some() {
                            assert_eq!(clip.separator.unwrap().width, cell, "{case}");
                            assert!(shares[index].width >= cell, "{case}");
                        }
                    }
                }
            }
        }
        // A wide panel at 1x with an 8 px cell keeps both separators.
        let shares = [
            Rect {
                x: 84,
                y: 0,
                width: 65,
                height: 26,
            },
            Rect {
                x: 149,
                y: 0,
                width: 66,
                height: 26,
            },
            Rect {
                x: 215,
                y: 0,
                width: 65,
                height: 26,
            },
        ];
        let clips = filter_text_clips(&shares, 8, 287);
        assert_eq!(clips[1].separator.unwrap().x, 145);
        assert_eq!(clips[2].separator.unwrap().x, 211);
        assert_eq!((clips[0].token.x, clips[0].token.width), (88, 57));
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
        // One line per row and a two-line footer: twelve rows fit where
        // two-line rows over a three-line footer fitted five.
        assert_eq!(layout.visible_capacity, 12);
        assert_eq!(layout.visible_start, 18);
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
            "Terminal has keys · click to move"
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
    fn status_line_never_wraps_and_fills_word_by_word_before_an_ellipsis() {
        let status = "asking for access · 1 needs input · 2 working · saved";
        let counts = "1 error · 1 working · 1 done · 1 idle";
        let is_count = |word: &str| !word.is_empty() && word.bytes().all(|b| b.is_ascii_digit());
        for (text, first_unit) in [(status, "asking"), (counts, "1 error")] {
            for cells in 0..=90 {
                let fitted = fit_status_line(text, cells);
                assert!(text_cells(&fitted) <= cells, "{cells}: {fitted:?}");
                assert!(!fitted.contains('\n'), "{cells}: {fitted:?}");
                if text_cells(text) <= cells {
                    assert_eq!(fitted, text);
                    continue;
                }
                if cells == 0 {
                    assert!(fitted.is_empty());
                    continue;
                }
                assert!(fitted.ends_with('…'), "{cells}: {fitted:?}");
                let shown = fitted.trim_end_matches('…');
                assert!(!shown.ends_with('·') && !shown.ends_with(' '), "{fitted:?}");
                // Never a bare count before the ellipsis, such as `· 1…`.
                assert!(
                    !is_count(shown.rsplit(' ').next().unwrap()),
                    "{cells}: {fitted:?}"
                );
                // Whole words, unless the first word, with its count, is
                // wider than the line.
                if text_cells(first_unit) < cells {
                    assert!(
                        text.starts_with(shown) && text[shown.len()..].starts_with(' '),
                        "{cells}: cut a word in {fitted:?}"
                    );
                    // As many words as fit: the next one, with the word after
                    // it when it is a count, would not.
                    let rest = &text[shown.len()..];
                    let after = rest.trim_start_matches([' ', '·']);
                    let mut words = after.split(' ');
                    let next = words.next().unwrap();
                    let mut end = shown.len() + rest.len() - after.len() + next.len();
                    if is_count(next) {
                        end += 1 + words.next().unwrap().len();
                    }
                    assert!(text_cells(&text[..end]) >= cells, "{cells}: {fitted:?}");
                }
            }
        }
        // A Lair's counts never end on the next count's number.
        assert_eq!(fit_status_line(counts, 34), "1 error · 1 working · 1 done…");
        assert_eq!(fit_status_line(counts, 12), "1 error…");
        assert_eq!(fit_status_line(counts, 4), "1 e…");
        assert_eq!(fit_status_line(counts, 3), "…");
        // The default panel holds about 28 cells: an agent's detail keeps its
        // first words instead of vanishing after the state.
        assert_eq!(
            fit_status_line("needs input · waiting for approval", 28),
            "needs input · waiting for…"
        );
        // A blocker after a short state fills the same way.
        assert_eq!(
            fit_status_line("starting · Work is still starting", 28),
            "starting · Work is still…"
        );
        // A narrow panel: the attention parts first, then the ellipsis.
        assert_eq!(
            fit_status_line("1 needs input · 2 working · 1 done · saved", 26),
            "1 needs input · 2 working…"
        );
        assert_eq!(fit_status_line(status, 30), "asking for access · 1 needs…");
        assert_eq!(fit_status_line(status, 22), "asking for access…");
        assert_eq!(fit_status_line(status, 16), "asking for…");
        assert_eq!(fit_status_line(status, 4), "ask…");
        // Wide characters count two cells.
        assert_eq!(fit_status_line("作業中 · saved", 8), "作業中…");
        assert_eq!(fit_status_line("2 working", 9), "2 working");
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

    /// The selected row's short status fills the footer's one status line,
    /// and a row with nothing more to say leaves that line blank rather than
    /// repeating its name, as does having no row selected.
    #[test]
    fn footer_paints_the_selected_rows_short_status_on_one_line() {
        use super::super::picker::session_picker_palette;
        use crate::config::ResolvedTheme;

        let context = RenderContext::new(u16::MAX);
        let id = NavigationNodeId::Lair(LairId::new());
        let mut entry = row(id);
        entry.status = "1 needs input · 2 working · saved · agent connected".into();
        let mut plain = entry.clone();
        plain.status = String::new();
        let surface = Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: HEADER_HEIGHT + HEADER_SPACER + FOOTER_HEIGHT + ROW_HEIGHT,
        };
        let palette = session_picker_palette(ResolvedTheme::default());
        for (rows, says_something) in [([entry], true), ([plain], false)] {
            let layout = lair_explorer_layout(surface, 640, &rows, None, 0).unwrap();
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
                // One status line, then the context line.
                assert_eq!(lines.len(), 2);
                let row_rect = buffer_rect(layout.rows[0].rect, scale);
                let mut on_status_line = 0_usize;
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
                    let line = lines[0];
                    assert!(
                        (line.y..line.y + line.height).contains(&y)
                            && (line.x..line.x + line.width).contains(&x),
                        "footer status outside its line at {x},{y}"
                    );
                    on_status_line += 1;
                }
                assert_eq!(
                    on_status_line > 0,
                    says_something,
                    "scale {scale}: {:?}",
                    rows[0].status
                );
            }
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
            background_alpha: u16::MAX / 2,
            roles: crate::theme::ChromeRoles {
                surface: 0x80_40_20,
                text: 0xee_ee_ee,
                ..crate::theme::ChromeRoles::default()
            },
            ..ResolvedTheme::default()
        };
        let light = theme.with_chrome_overrides(ChromeColorOverrides {
            explorer_background: Some(0xf0_f0_f0),
            explorer_alpha: Some(u16::MAX),
            ..ChromeColorOverrides::NONE
        });
        let palette = lair_explorer_palette(light, true);
        assert_eq!(palette.panel_fill, [0xf0, 0xf0, 0xf0, 0xff]);
        let expected = chrome_panel_palette(
            ResolvedTheme {
                roles: crate::theme::ChromeRoles {
                    surface: 0xf0_f0_f0,
                    ..light.roles
                },
                ..light
            },
            ChromeSurface::Menu,
        );
        assert_eq!(palette.primary, expected.primary);
        assert_ne!(palette.primary, lair_explorer_palette(theme, true).primary);

        let alpha_only = theme.with_chrome_overrides(ChromeColorOverrides {
            explorer_alpha: Some(u16::MAX),
            ..ChromeColorOverrides::NONE
        });
        assert_eq!(
            lair_explorer_palette(alpha_only, true).panel_fill,
            [0x80, 0x40, 0x20, 0xff]
        );
    }

    #[test]
    fn explorer_respects_background_alpha_with_opaque_text_and_lines() {
        let theme = ResolvedTheme {
            background_alpha: u16::MAX / 2,
            roles: crate::theme::ChromeRoles {
                surface: 0x80_40_20,
                text: 0xee_ee_ee,
                ..crate::theme::ChromeRoles::default()
            },
            ..ResolvedTheme::default()
        };
        let palette = lair_explorer_palette(theme, true);
        // The same premultiplied glass as the tab strip and the terminal.
        assert_eq!(
            palette.panel_fill,
            premultiplied_theme_rgba(theme.roles.surface, theme.background_alpha)
        );
        assert_eq!(palette.panel_fill, [0x40, 0x20, 0x10, 0x7f]);
        let opaque = lair_explorer_palette(ResolvedTheme::default(), true);
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

    /// Whether any pixel of a BGRA canvas is exactly `color`.
    fn paints_color(canvas: &[u8], color: u32) -> bool {
        let [red, green, blue, _] = rgba(color);
        canvas
            .chunks_exact(4)
            .any(|pixel| pixel[..3] == [blue, green, red])
    }

    #[test]
    fn the_ascii_asking_marker_is_tinted_as_text() {
        let marker = |icons| {
            let mut source = ChromeSource::default();
            push_access(&mut source, Some(LairExplorerAccess::Asking), icons);
            source.tint_at(0)
        };
        assert_eq!(marker(true), Some(ChromeTint::Asking));
        assert_eq!(marker(false), Some(ChromeTint::AskingText));
    }

    #[test]
    fn status_glyphs_and_the_asking_marker_take_their_tints_on_their_row() {
        use crate::color::{NON_TEXT_MIN_CONTRAST, contrast_ratio};
        use crate::frontend::{AgentAggregate, AgentGlyphs, AgentSpinner};
        use crate::navigation_projection::NavigationAgentStatus;
        use splinterm_protocol::AgentState;

        let context = RenderContext::new(u16::MAX);
        let scale = 120;
        let theme = ResolvedTheme::default();
        let palette = lair_explorer_palette(theme, true);
        let paint_one = |entry: &LairExplorerRow, palette: SessionPickerPalette, selected: bool| {
            let rows = [entry.clone()];
            let surface = Rect {
                x: 0,
                y: 0,
                width: 1000,
                height: HEADER_HEIGHT + HEADER_SPACER + FOOTER_HEIGHT + ROW_HEIGHT,
            };
            let layout = lair_explorer_layout(surface, 640, &rows, None, 0).unwrap();
            let mut canvas = vec![0; (surface.width * surface.height * 4) as usize];
            paint(
                &context,
                &mut canvas,
                surface.width,
                surface.height,
                scale,
                &layout,
                palette,
                &rows,
                selected.then_some(entry.id),
                false,
                LairExplorerFilter::All,
            );
            canvas
        };
        for state in [
            AgentState::NeedsInput,
            AgentState::Error,
            AgentState::Working,
            AgentState::Done,
            AgentState::Idle,
        ] {
            for is_stale in [false, true] {
                let status = NavigationAgentStatus {
                    state,
                    detail: None,
                    stale: is_stale,
                    stale_minutes: is_stale.then_some(2),
                };
                let column = AgentAggregate::from_statuses(&[status])
                    .glyph_column(AgentGlyphs::Icons, AgentSpinner::STATIC);
                let mut entry = row(NavigationNodeId::Lair(LairId::new()));
                entry.agent_column = Some(column.clone());
                entry.access = Some(LairExplorerAccess::Asking);
                let tint = crate::frontend::state_tint(state, is_stale);
                for selected in [false, true] {
                    let what = format!("{state:?} stale={is_stale} selected={selected}");
                    let tints = palette.row_tints(selected).unwrap();
                    let background = if selected {
                        palette.selected_fill
                    } else {
                        palette.panel
                    };
                    for marked in [tint, ChromeTint::Asking] {
                        let color = tints.color(marked);
                        assert!(
                            contrast_ratio(color, background) >= NON_TEXT_MIN_CONTRAST,
                            "{what} {marked:?}"
                        );
                    }
                    let tinted = paint_one(&entry, palette, selected);
                    assert!(paints_color(&tinted, tints.color(tint)), "{what}");
                    assert!(
                        paints_color(&tinted, tints.color(ChromeTint::Asking)),
                        "{what}"
                    );
                    // status-colors=no paints every mark in the row's text
                    // color, exactly as an untinted source does.
                    let off = paint_one(&entry, palette.with_status_colors(false), selected);
                    let mut plain = entry.clone();
                    plain.agent_column = Some(column.untinted());
                    let untinted = paint_one(&plain, palette.with_status_colors(false), selected);
                    assert_eq!(off, untinted, "{what}");
                    assert!(!paints_color(&off, tints.color(tint)), "{what}");
                    assert!(
                        !paints_color(&off, tints.color(ChromeTint::Asking)),
                        "{what}"
                    );
                    assert_ne!(off, tinted, "{what}");
                }
            }
        }
    }
}
