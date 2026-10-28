//! Responsive layout and CPU composition for the non-modal Lair explorer.
//!
//! One line per row: the current-row marker, the disclosure chevron, and the
//! name on the left, and fixed, right-aligned icon columns for agent access,
//! agent status, live state, and retention. With `[explorer] row-lines=2`
//! every row of the expanded tree and of the agents section is two lines
//! tall: a Splint row shows the agent's detail or its working directory,
//! muted, under its name, and a Lair or Dojo row centres its one line.
//! The rail keeps one line per row. The footer has two lines: an
//! action line, `new · menu · «`, whose clicks leave keyboard input where it
//! is, then one line for the search field, else a navigation notice, else
//! the selected row's short status, else who owns keyboard input.
//!
//! Below the tree, above the footer, the expanded Explorer lists every
//! Splint reporting an agent status in an agents section of a fixed share of
//! the list (see [`lair_explorer_with_agents`]).
//!
//! The docked Explorer can also collapse to a rail about four cells wide:
//! one row per Lair, its initial and agent glyph, the current Lair marked by
//! the accent bar. The footer's `«` collapses the panel and the rail's `»`
//! expands it.

use std::collections::HashSet;

use anyhow::Result;
use unicode_width::UnicodeWidthChar;

use crate::{
    config::{ExplorerCurrentMarker, ResolvedTheme},
    frontend::{
        LairExplorerAccess, LairExplorerAgentRow, LairExplorerDetail, LairExplorerFilter,
        LairExplorerRow,
    },
    geometry::Rect,
    navigation_projection::NavigationNodeId,
    theme::{ChromeTint, ChromeTints},
};
use splinterm_core::LairRetention;

use super::{
    super::{
        ChromeIcon, ChromeSource, ChromeTextStyle, RenderContext, chrome_icons_available,
        fill_rect, premultiplied_theme_rgba, raster::premultiplied_rgba,
    },
    picker::{
        ChromeSurface, PickerTextAlignment, SessionPickerPalette, SessionPickerTextCache,
        SessionPickerTextKey, chrome_panel_palette_on, editable_text, paint_picker_source,
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
/// The footer's first line holds the actions; the second, the context.
const FOOTER_ACTION_LINE: usize = 0;
const FOOTER_CONTEXT_LINE: usize = 1;
const FOOTER_LINES: u32 = 2;
const FOOTER_PADDING: u32 = 4;
const FOOTER_HEIGHT: u32 = FOOTER_LINE_HEIGHT * FOOTER_LINES + FOOTER_PADDING;
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

/// The `«` target's narrowest width on the footer's action line.
const TOGGLE_WIDTH: u32 = 24;
/// Collapses the expanded Explorer to its rail.
const COLLAPSE_TEXT: &str = "«";
/// Expands the rail to the full Explorer.
const EXPAND_TEXT: &str = "»";
/// Marks a navigation notice or load state on the rail's header.
const RAIL_NOTICE_TEXT: &str = "!";
/// Painted between neighbouring footer actions.
const ACTION_SEPARATOR: &str = "·";
/// Logical px per cell of an action word's target, and the gap between two
/// targets, while the chrome font cannot be measured.
const ACTION_CELL_WIDTH: u32 = 14;
/// Logical px each side of an action word inside its target.
const ACTION_PADDING: u32 = 4;

/// What the Explorer's layout sizes from the chrome text as it is drawn,
/// the collapsed rail's width and the footer's action targets, and how
/// many text lines each expanded row has.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LairExplorerMetrics {
    /// The collapsed rail's width in logical px.
    pub(crate) rail_width: u32,
    /// One regular chrome cell's width in buffer px, or 0 when the chrome
    /// font could not be measured.
    chrome_cell: u32,
    scale_120: u32,
    /// Text lines per row of the expanded tree and the agents section: 1,
    /// or 2 with `[explorer] row-lines=2`. The rail keeps one.
    row_lines: u32,
}

impl LairExplorerMetrics {
    /// The sizes for an unmeasured chrome font: the rail at its narrowest
    /// and action targets of [`ACTION_CELL_WIDTH`] per cell.
    #[cfg(test)]
    pub(crate) const UNMEASURED: Self = Self {
        rail_width: RAIL_WIDTH,
        chrome_cell: 0,
        scale_120: 120,
        row_lines: 1,
    };

    /// The sizes for a regular chrome cell `chrome_cell` buffer px wide at
    /// `scale_120`, as [`lair_explorer_chrome_cell_width`] measures it.
    #[must_use]
    pub(crate) fn measured(chrome_cell: u32, scale_120: u32) -> Self {
        Self {
            rail_width: lair_explorer_rail_width(chrome_cell, scale_120),
            chrome_cell,
            scale_120: scale_120.max(1),
            row_lines: 1,
        }
    }

    /// These sizes with rows of two text lines when `two_line_rows`.
    #[must_use]
    pub(crate) const fn with_two_line_rows(self, two_line_rows: bool) -> Self {
        Self {
            row_lines: if two_line_rows { 2 } else { 1 },
            ..self
        }
    }

    /// A row's height in logical px: [`ROW_HEIGHT`] per text line.
    const fn row_height(self) -> u32 {
        ROW_HEIGHT * self.row_lines
    }

    /// `buffer` px in logical px, rounded up, and one px more at a
    /// fractional scale for an edge that rounds away a buffer px.
    fn logical(self, buffer: u32) -> u32 {
        buffer
            .saturating_mul(120)
            .div_ceil(self.scale_120)
            .saturating_add(u32::from(self.scale_120 % 120 != 0))
    }

    /// An action target's width in logical px: its word in measured chrome
    /// cells with [`ACTION_PADDING`] each side, so the word is never cut;
    /// `«` is never narrower than [`TOGGLE_WIDTH`].
    fn action_width(self, action: LairExplorerAction) -> u32 {
        let cells = u32::try_from(text_cells(action.text())).unwrap_or(u32::MAX);
        let width = if self.chrome_cell == 0 {
            cells
                .saturating_mul(ACTION_CELL_WIDTH)
                .saturating_add(ACTION_PADDING * 2)
        } else {
            let padding = ACTION_PADDING.saturating_mul(self.scale_120).div_ceil(120);
            self.logical(
                cells
                    .saturating_mul(self.chrome_cell)
                    .saturating_add(padding.saturating_mul(2)),
            )
        };
        match action {
            LairExplorerAction::Toggle => width.max(TOGGLE_WIDTH),
            _ => width,
        }
    }

    /// The gap between two action targets, where the `·` sits: one chrome
    /// cell.
    fn separator_width(self) -> u32 {
        if self.chrome_cell == 0 {
            ACTION_CELL_WIDTH
        } else {
            self.logical(self.chrome_cell)
        }
    }
}

/// A click target of the Explorer's own chrome: the footer's action line or
/// the rail's header. A press on one leaves keyboard input where it is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LairExplorerAction {
    /// `new`: a new Lair, as **New terminal** makes one.
    New,
    /// `menu`: the selected row's menu, as a right-click opens it, or the
    /// command palette when no row is selected or the row has nothing on its
    /// menu; nothing while the row's menu is unavailable for now.
    Menu,
    /// `«` on the expanded footer collapses to the rail; `»` on the rail's
    /// header expands it.
    Toggle,
}

impl LairExplorerAction {
    /// The word on the footer's action line.
    const fn text(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Menu => "menu",
            Self::Toggle => COLLAPSE_TEXT,
        }
    }
}

/// The footer's context line when nothing else takes it: who owns
/// keyboard input and how to move.
const fn explorer_hint_text(focused: bool) -> &'static str {
    if focused {
        "↑↓ move · F filter · Esc terminal"
    } else {
        "Terminal has keys · click to move"
    }
}

/// The footer's context line, and whether it is the selected row's status:
/// the search field while a search is open, so typing is never blind; else
/// a navigation notice that the list does not show; else a query left
/// behind after leaving the search, which still narrows the list; else the
/// selected row's short status, fitted to the line; else the input hint.
fn explorer_context_text(
    focused: bool,
    searching: bool,
    query: &str,
    notice: Option<&str>,
    status: Option<&str>,
    maximum_cells: usize,
) -> (String, bool) {
    if focused && searching {
        (editable_text("Search: ", query, maximum_cells), false)
    } else if let Some(notice) = notice {
        (notice.to_owned(), false)
    } else if !query.is_empty() {
        (format!("Search: {query}"), false)
    } else if let Some(status) = status.filter(|status| !status.is_empty()) {
        (fit_status_line(status, maximum_cells), true)
    } else {
        (explorer_hint_text(focused).to_owned(), false)
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
    /// Each row's height in logical px, of the tree and of the agents
    /// section: [`ROW_HEIGHT`] per text line. The rail's rows have one line.
    pub(crate) row_height: u32,
    pub(crate) visible_start: usize,
    pub(crate) visible_capacity: usize,
    pub(crate) mode: LairExplorerPresentationMode,
    /// Whether this is the collapsed rail: one row per Lair, with no filters
    /// and no footer, docked, or a drawer over the terminal in a Window too
    /// narrow to dock even the rail.
    pub(crate) rail: bool,
    /// The click targets of the Explorer's own chrome, left to right: the
    /// footer's `new`, `menu`, and `«` on the expanded panel, the header's
    /// `»` on the rail. Narrow panels drop `menu`, then `new`, then `«`,
    /// and a line cut short has none.
    pub(crate) actions: Vec<(LairExplorerAction, Rect)>,
    /// The expanded Explorer's agents section, when it is shown.
    pub(crate) agents: Option<LairExplorerAgentsLayout>,
}

/// The agents section at the bottom of the expanded Explorer's list, above
/// the footer: a rule, the `agents` label line, then its rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LairExplorerAgentsLayout {
    /// The label line, with the rule along its top.
    pub(crate) label: Rect,
    pub(crate) rows: Vec<LairExplorerRowLayout>,
    pub(crate) visible_start: usize,
    /// How many reporting Splints are not in view, for the label's `+k more`.
    pub(crate) hidden: usize,
}

impl LairExplorerLayout {
    /// Where the row for `node` is in view: the agents section's row when
    /// `agents`, else the tree's.
    #[must_use]
    pub(crate) fn row_rect(&self, node: NavigationNodeId, agents: bool) -> Option<Rect> {
        let rows = if agents {
            &self.agents.as_ref()?.rows
        } else {
            &self.rows
        };
        rows.iter().find(|row| row.id == node).map(|row| row.rect)
    }

    /// The `«` or `»` target, when shown.
    #[must_use]
    pub(crate) fn toggle(&self) -> Option<Rect> {
        self.actions
            .iter()
            .find_map(|(action, rect)| (*action == LairExplorerAction::Toggle).then_some(*rect))
    }
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
/// `height` px, each `row_height` px tall: the visible start, how many rows
/// fit, and their rects. The selected row is kept in view.
fn row_slots(
    panel: Rect,
    (top, height): (u32, u32),
    row_height: u32,
    rows: &[LairExplorerRow],
    selected: Option<NavigationNodeId>,
    requested_start: usize,
) -> (usize, usize, Vec<LairExplorerRowLayout>) {
    let capacity = usize::try_from(height / row_height.max(1)).unwrap_or(0);
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
                    row_height.saturating_mul(u32::try_from(slot).unwrap_or(u32::MAX)),
                ),
                width: panel.width,
                height: row_height,
            },
        })
        .collect();
    (start, capacity, row_layouts)
}

/// The shown Explorer's layout with a header line `header` logical px
/// tall, or row-high for `None`: the rail, as wide as `metrics` say,
/// when `collapsed`, else the expanded panel. Either one docks beside the
/// terminal when that leaves it `minimum_terminal_width` logical px, and is
/// otherwise a drawer over the terminal instead of squeezing it.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub(crate) fn lair_explorer_shown_layout(
    content: Rect,
    header: Option<u32>,
    minimum_terminal_width: u32,
    metrics: LairExplorerMetrics,
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
            metrics.rail_width,
            rows,
            selected,
            requested_start,
        )
    } else {
        lair_explorer_layout_with_header(
            content,
            header,
            minimum_terminal_width,
            metrics,
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
        (list_top, panel.height - list_top),
        ROW_HEIGHT,
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
        row_height: ROW_HEIGHT,
        visible_start: start,
        visible_capacity: capacity,
        mode: if docked {
            LairExplorerPresentationMode::Docked
        } else {
            LairExplorerPresentationMode::Drawer
        },
        rail: true,
        actions: if header_height == header {
            vec![(LairExplorerAction::Toggle, header_rect)]
        } else {
            Vec::new()
        },
        agents: None,
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
        LairExplorerMetrics::UNMEASURED,
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
    metrics: LairExplorerMetrics,
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
    // A list too short for one row of two lines shows rows of one, so the
    // selection stays in view.
    let row_height = if (ROW_HEIGHT..metrics.row_height()).contains(&list_height) {
        ROW_HEIGHT
    } else {
        metrics.row_height()
    };
    let (start, capacity, row_layouts) = row_slots(
        panel,
        (list_top, list_height),
        row_height,
        rows,
        selected,
        requested_start,
    );
    let footer = Rect {
        x: panel.x,
        y: panel
            .y
            .saturating_add(panel.height.saturating_sub(footer_height)),
        width: panel.width,
        height: footer_height,
    };
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
        footer,
        filters: if header_height == header {
            header_filter_rects(panel, header_height)
        } else {
            Vec::new()
        },
        rows: row_layouts,
        row_height,
        visible_start: start,
        visible_capacity: capacity,
        mode,
        rail: false,
        // Every Window can collapse: the rail is a drawer where it cannot
        // dock.
        actions: footer_action_rects(footer, metrics),
        agents: None,
    })
}

/// The most rows the agents section shows at once.
const AGENTS_MAXIMUM_ROWS: u32 = 5;
/// Names the agents section on its label line.
const AGENTS_TEXT: &str = "agents";
/// The fewest cells a row's location is shown in, ellipsized; with less
/// room the row shows its name alone.
const AGENT_LOCATION_MINIMUM_CELLS: usize = 6;

/// How many agent rows the section shows in a list `lines` text lines
/// tall, for `count` reporting Splints whose rows are `row_lines` lines
/// each: up to [`AGENTS_MAXIMUM_ROWS`] and a third of the list's lines, but
/// at least one row, below its one-line label, leaving the tree at least
/// one row. `None` when nothing reports or the list has no room for the
/// label, one agents row, and one tree row.
fn agents_section_rows(lines: u32, count: usize, row_lines: u32) -> Option<u32> {
    let count = u32::try_from(count).unwrap_or(u32::MAX);
    let row_lines = row_lines.max(1);
    let fewest = row_lines.saturating_mul(2).saturating_add(1);
    if count == 0 || lines < fewest {
        return None;
    }
    Some(
        count
            .min(AGENTS_MAXIMUM_ROWS)
            .min((lines / 3 / row_lines).max(1))
            .min((lines - 1 - row_lines) / row_lines),
    )
}

/// The expanded `layout` with the agents section split off the bottom of
/// its list, above the footer, for the reporting Splints `agents`, most
/// urgent first. The split is fixed by [`agents_section_rows`], counting
/// text lines: the label is one line and each row has the layout's
/// lines. The tree scrolls in the rows left above it from
/// `requested_start`, the start it was asked for rather than the one
/// clamped to the list before the split, keeping `selected` in view. The
/// section scrolls from `agent_start`, keeping `agent_selected` in view.
/// The rail, and a list with nothing reporting or too short for the
/// section, are returned as they are.
#[must_use]
pub(crate) fn lair_explorer_with_agents(
    mut layout: LairExplorerLayout,
    rows: &[LairExplorerRow],
    (selected, requested_start): (Option<NavigationNodeId>, usize),
    agents: &[LairExplorerAgentRow],
    agent_selected: Option<NavigationNodeId>,
    agent_start: usize,
) -> LairExplorerLayout {
    if layout.rail {
        return layout;
    }
    let panel = layout.panel;
    let row_height = layout.row_height.max(ROW_HEIGHT);
    let list_bottom = layout.footer.y.saturating_sub(panel.y);
    let list_top = layout
        .header
        .height
        .saturating_add(HEADER_SPACER)
        .min(list_bottom);
    let list_height = list_bottom - list_top;
    let Some(shown) = agents_section_rows(
        list_height / ROW_HEIGHT,
        agents.len(),
        row_height / ROW_HEIGHT,
    ) else {
        return layout;
    };
    let section_height = ROW_HEIGHT.saturating_add(row_height.saturating_mul(shown));
    let section_top = list_bottom.saturating_sub(section_height);
    let (start, capacity, tree_rows) = row_slots(
        panel,
        (list_top, section_top.saturating_sub(list_top)),
        row_height,
        rows,
        selected,
        requested_start,
    );
    layout.visible_start = start;
    layout.visible_capacity = capacity;
    layout.rows = tree_rows;
    let agent_rows: Vec<_> = agents.iter().map(|agent| agent.row.clone()).collect();
    let (agent_visible_start, _, agent_layouts) = row_slots(
        panel,
        (
            section_top.saturating_add(ROW_HEIGHT),
            row_height.saturating_mul(shown),
        ),
        row_height,
        &agent_rows,
        agent_selected,
        agent_start,
    );
    layout.agents = Some(LairExplorerAgentsLayout {
        label: Rect {
            x: panel.x,
            y: panel.y.saturating_add(section_top),
            width: panel.width,
            height: ROW_HEIGHT,
        },
        hidden: agents.len().saturating_sub(agent_layouts.len()),
        rows: agent_layouts,
        visible_start: agent_visible_start,
    });
    layout
}

/// The agents section's row at `position`, if any.
#[must_use]
pub(crate) fn lair_explorer_agent_hit_test(
    layout: &LairExplorerLayout,
    position: (f64, f64),
) -> Option<NavigationNodeId> {
    layout.agents.as_ref()?.rows.iter().find_map(|row| {
        (position.0 >= f64::from(row.rect.x)
            && position.0 < f64::from(row.rect.x.saturating_add(row.rect.width))
            && position.1 >= f64::from(row.rect.y)
            && position.1 < f64::from(row.rect.y.saturating_add(row.rect.height)))
        .then_some(row.id)
    })
}

/// Whether `position` is on the agents section's label line: a press there
/// is not a row press and leaves keyboard input where it is.
#[must_use]
pub(crate) fn lair_explorer_agents_label_hit_test(
    layout: &LairExplorerLayout,
    position: (f64, f64),
) -> bool {
    layout.agents.as_ref().is_some_and(|section| {
        let label = section.label;
        position.0 >= f64::from(label.x)
            && position.0 < f64::from(label.x.saturating_add(label.width))
            && position.1 >= f64::from(label.y)
            && position.1 < f64::from(label.y.saturating_add(label.height))
    })
}

/// An agent row's name and location for `cells` cells after its glyph: both
/// when they fit with two cells between, else the name whole with the
/// location ellipsized into what is left when that is at least
/// [`AGENT_LOCATION_MINIMUM_CELLS`], else the name alone, ellipsized. A cut
/// location never ends on the ` / ` between the Lair and the Dojo or on a
/// space before its `…`; one with nothing left before the `…` is dropped.
fn agent_row_text(label: &str, location: &str, cells: usize) -> (String, Option<String>) {
    let name = text_cells(label);
    let room = cells.saturating_sub(name.saturating_add(2));
    if name.saturating_add(2) >= cells
        || room < AGENT_LOCATION_MINIMUM_CELLS.min(text_cells(location))
    {
        return (truncate_picker_text(label, cells), None);
    }
    let cut = truncate_picker_text(location, room);
    if cut == location {
        return (label.to_owned(), Some(cut));
    }
    let kept = cut.trim_end_matches('…').trim_end_matches([' ', '/']);
    (
        label.to_owned(),
        (!kept.is_empty()).then(|| format!("{kept}…")),
    )
}

/// The footer action line's targets, left to right with a `·` between
/// neighbours: `new`, `menu`, and `«`, each as wide as its word in the
/// measured chrome cells of `metrics`. The first word starts at the context
/// line's inset. A line too narrow for all of them drops `menu`, then
/// `new`, so `«` stays longest; none overlaps another or reaches the
/// divider. A footer cut short by a tiny Window has none.
fn footer_action_rects(
    footer: Rect,
    metrics: LairExplorerMetrics,
) -> Vec<(LairExplorerAction, Rect)> {
    use LairExplorerAction::{Menu, New, Toggle};

    if footer.height < FOOTER_HEIGHT {
        return Vec::new();
    }
    let left = footer
        .x
        .saturating_add(LEVEL_INDENT.saturating_sub(ACTION_PADDING));
    let right = footer
        .x
        .saturating_add(footer.width.saturating_sub(FOOTER_PADDING));
    let separator = metrics.separator_width();
    let choices: [&[LairExplorerAction]; 3] = [&[New, Menu, Toggle], &[New, Toggle], &[Toggle]];
    for shown in choices {
        let width = shown
            .iter()
            .map(|&action| metrics.action_width(action))
            .sum::<u32>()
            .saturating_add(
                separator.saturating_mul(u32::try_from(shown.len().saturating_sub(1)).unwrap_or(0)),
            );
        if left.saturating_add(width) > right {
            continue;
        }
        let mut x = left;
        return shown
            .iter()
            .map(|&action| {
                let rect = Rect {
                    x,
                    y: footer.y.saturating_add(FOOTER_PADDING),
                    width: metrics.action_width(action),
                    height: FOOTER_LINE_HEIGHT,
                };
                x = x.saturating_add(rect.width).saturating_add(separator);
                (action, rect)
            })
            .collect();
    }
    Vec::new()
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

/// The footer action or rail toggle at `position`, if any.
#[must_use]
pub(crate) fn lair_explorer_action_hit_test(
    layout: &LairExplorerLayout,
    position: (f64, f64),
) -> Option<LairExplorerAction> {
    layout.actions.iter().find_map(|(action, rect)| {
        (position.0 >= f64::from(rect.x)
            && position.0 < f64::from(rect.x.saturating_add(rect.width))
            && position.1 >= f64::from(rect.y)
            && position.1 < f64::from(rect.y.saturating_add(rect.height)))
        .then_some(*action)
    })
}

/// Whether `position` is anywhere on the expanded footer, both lines and
/// their padding: a press there that misses a target is not a row press.
/// The rail has no footer.
#[must_use]
pub(crate) fn lair_explorer_footer_hit_test(
    layout: &LairExplorerLayout,
    position: (f64, f64),
) -> bool {
    let footer = layout.footer;
    position.0 >= f64::from(footer.x)
        && position.0 < f64::from(footer.x.saturating_add(footer.width))
        && position.1 >= f64::from(footer.y)
        && position.1 < f64::from(footer.y.saturating_add(footer.height))
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

/// Whether a row in view, of the tree or of the agents section, shows a
/// fresh working status, whose spinner moves. Rows scrolled out of view do
/// not count.
#[must_use]
pub(crate) fn lair_explorer_shows_working(
    layout: &LairExplorerLayout,
    rows: &[LairExplorerRow],
    agents: &[LairExplorerAgentRow],
) -> bool {
    rows.iter()
        .skip(layout.visible_start)
        .take(layout.rows.len())
        .any(|row| row.agent_working)
        || layout.agents.as_ref().is_some_and(|section| {
            agents
                .iter()
                .skip(section.visible_start)
                .take(section.rows.len())
                .any(|agent| agent.row.agent_working)
        })
}

/// The logical width from a row's left edge that toggles its disclosure: the
/// row's indent and the current-row marker and chevron after it, with room
/// for the largest chrome cell.
fn disclosure_width(level: usize, icons: bool) -> u32 {
    let level = u32::try_from(level).unwrap_or(u32::MAX);
    LEVEL_INDENT.saturating_mul(level.saturating_add(marker_cells(icons) + ChromeIcon::CELLS))
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
    let icons = chrome_icons_available();
    layout
        .rows
        .iter()
        .zip(rows.iter().skip(layout.visible_start))
        .find_map(|(layout_row, row)| {
            row.expanded?;
            let bottom = f64::from(layout_row.rect.y.saturating_add(layout_row.rect.height));
            let width = disclosure_width(row.level, icons).min(layout_row.rect.width);
            let right = f64::from(layout_row.rect.x.saturating_add(width));
            (position.0 >= f64::from(layout_row.rect.x)
                && position.0 < right
                && position.1 >= f64::from(layout_row.rect.y)
                && position.1 < bottom)
                .then_some(row.id)
        })
}

/// The cells of the current-row marker before a row's disclosure slot: the
/// `[explorer] current-marker` icon, or a one-cell `●` when the embedded
/// icon face fails.
const fn marker_cells(icons: bool) -> u32 {
    if icons { ChromeIcon::CELLS } else { 1 }
}

/// A row's name: the current-row marker, a two-cell disclosure slot, and the
/// label. The marker is the `marker` icon and the disclosure chevrons are
/// icons; a one-cell `●` and ASCII `>` and `v` stand in only if the embedded
/// icon face fails. A row that is not current leaves the marker's cells
/// blank, so every row's label starts at the same cell.
fn row_name_source(row: &LairExplorerRow, icons: bool, marker: ChromeIcon) -> ChromeSource {
    let mut source = ChromeSource::default();
    let push_marker = |source: &mut ChromeSource, current: bool| {
        match (current, icons) {
            (true, true) => source.push_icon(marker),
            (true, false) => source.push_str("●"),
            (false, true) => source.push_str("  "),
            (false, false) => source.push_str(" "),
        };
    };
    if row.pending {
        push_marker(&mut source, false);
        source.push_str(" …");
    } else {
        match (row.expanded, icons) {
            (Some(expanded), true) => {
                push_marker(&mut source, row.current);
                source.push_icon(if expanded {
                    ChromeIcon::ChevronDown
                } else {
                    ChromeIcon::ChevronRight
                });
            }
            (Some(expanded), false) => {
                push_marker(&mut source, row.current);
                source.push_str(if expanded { " v" } else { " >" });
            }
            // A Splint has no disclosure: its current marker sits in the
            // slot, the one-cell dot in the slot's second cell.
            (None, true) => {
                push_marker(&mut source, false);
                push_marker(&mut source, row.current);
            }
            (None, false) => {
                push_marker(&mut source, false);
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

/// The footer's text lines in buffer pixels: the action line, then the
/// context line, below the footer's padding and its top rule.
fn footer_lines(footer: Rect, scale_120: u32, inset: u32) -> Vec<Rect> {
    let line = FOOTER_LINE_HEIGHT.saturating_mul(scale_120).div_ceil(120);
    let top = footer
        .y
        .saturating_add(FOOTER_PADDING.saturating_mul(scale_120).div_ceil(120));
    let bottom = footer.y.saturating_add(footer.height);
    (0..FOOTER_LINES)
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
    let mut palette =
        chrome_panel_palette_on(theme, ChromeSurface::Menu, theme.explorer_background());
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
    agents: (&[LairExplorerAgentRow], Option<NavigationNodeId>),
    query: &str,
    status_message: Option<&str>,
    focused: bool,
    filter: LairExplorerFilter,
    searching: bool,
    current_marker: ExplorerCurrentMarker,
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

    let icons = chrome_icons_available();
    let marker = ChromeIcon::from(current_marker);
    let columns = ExplorerColumns::of(rows);
    for (layout_row, row) in layout
        .rows
        .iter()
        .zip(rows.iter().skip(layout.visible_start))
    {
        let rect = buffer_rect(layout_row.rect, scale_120);
        let is_selected = selected == Some(row.id);
        if is_selected {
            // The fill and the accent bar cover every line of the row.
            paint_row_highlight(
                canvas,
                (canvas_width, canvas_height),
                rect,
                (border, right),
                palette,
            );
        }
        let (first_line, second_line) =
            row_text_lines(layout_row.rect, row.detail.is_some(), scale_120);
        let (columns_source, first_mark) = row_columns_source(row, columns, icons);
        let (name_clip, columns_clip) = row_text_clips(
            first_line,
            row.level,
            inset,
            cell_width,
            columns_source.cells(),
            first_mark,
        );
        let primary = row_primary(palette, is_selected, row.enabled);
        if let (Some(detail), Some(line)) = (&row.detail, second_line) {
            // Under the name, past its marker and disclosure slot, to the
            // row's right inset: the icon columns are on the first line.
            let left = name_clip
                .x
                .saturating_add(cell_width.saturating_mul(row_name_prefix_cells(icons)))
                .min(name_clip.x.saturating_add(name_clip.width));
            let end = columns_clip.x.saturating_add(columns_clip.width);
            paint_row_detail(
                cache,
                context,
                &mut used,
                canvas,
                (canvas_width, canvas_height),
                (scale_120, renderer_generation),
                detail,
                Rect {
                    x: left,
                    y: line.y,
                    width: end.saturating_sub(left),
                    height: line.height,
                },
                (palette, is_selected, cell_width),
            )?;
        }
        paint_picker_source(
            cache,
            context,
            &mut used,
            canvas,
            canvas_width,
            canvas_height,
            &row_name_source(row, icons, marker),
            ChromeTextStyle::Regular,
            scale_120,
            renderer_generation,
            name_clip,
            PickerTextAlignment::Left,
            primary,
        )?;
        paint_row_glyphs(
            cache,
            context,
            &mut used,
            canvas,
            (canvas_width, canvas_height),
            (scale_120, renderer_generation),
            &columns_source,
            columns_paint_clip(columns_clip, right),
            primary,
            palette.row_tints(is_selected),
        )?;
    }

    if let Some(section) = &layout.agents {
        paint_agents_section(
            cache,
            context,
            &mut used,
            canvas,
            (canvas_width, canvas_height),
            (scale_120, renderer_generation),
            section,
            palette,
            agents,
            (cell_width, border, right, inset),
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
    // The action words are muted tokens, each left-aligned in its target
    // past the padding, so the first starts where the context line does,
    // with a `·` centred in the gap between neighbours.
    let action_line = lines[FOOTER_ACTION_LINE];
    let padding = ACTION_PADDING.saturating_mul(scale_120).div_ceil(120);
    let mut previous_end = None;
    for (action, rect) in &layout.actions {
        let target = Rect {
            y: action_line.y,
            height: action_line.height,
            ..action_clip(*rect, scale_120, right)
        };
        if let Some(previous_end) = previous_end {
            paint_picker_text(
                cache,
                context,
                &mut used,
                canvas,
                canvas_width,
                canvas_height,
                ACTION_SEPARATOR,
                ChromeTextStyle::Regular,
                scale_120,
                renderer_generation,
                Rect {
                    x: previous_end,
                    width: target.x.saturating_sub(previous_end),
                    ..target
                },
                PickerTextAlignment::Center,
                palette.secondary,
            )?;
        }
        previous_end = Some(target.x.saturating_add(target.width));
        paint_picker_text(
            cache,
            context,
            &mut used,
            canvas,
            canvas_width,
            canvas_height,
            action.text(),
            ChromeTextStyle::Regular,
            scale_120,
            renderer_generation,
            action_text_clip(target, padding),
            PickerTextAlignment::Left,
            palette.secondary,
        )?;
    }
    // The selected row is highlighted just above, so its status never
    // repeats the name; a row with nothing more to say leaves the line to
    // the input hint.
    let context_line = lines[FOOTER_CONTEXT_LINE];
    let (context_text, is_status) = explorer_context_text(
        focused,
        searching,
        query,
        notice,
        selected
            .and_then(|id| rows.iter().find(|row| row.id == id))
            .or_else(|| {
                agents.1.and_then(|id| {
                    agents
                        .0
                        .iter()
                        .map(|agent| &agent.row)
                        .find(|row| row.id == id)
                })
            })
            .map(|row| row.status.as_str()),
        line_cells(&context_line),
    );
    paint_picker_text(
        cache,
        context,
        &mut used,
        canvas,
        canvas_width,
        canvas_height,
        &context_text,
        ChromeTextStyle::Regular,
        scale_120,
        renderer_generation,
        context_line,
        PickerTextAlignment::Left,
        if is_status {
            palette.primary
        } else {
            palette.secondary
        },
    )?;
    cache.finish_frame(used);
    Ok(())
}

/// Paints a row's icon columns, or an agents row's glyph column alone, in
/// `clip`: the one paint path for agent glyphs, so the tree and the agents
/// section draw them alike.
#[allow(clippy::too_many_arguments)]
fn paint_row_glyphs(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    used: &mut HashSet<SessionPickerTextKey>,
    canvas: &mut [u8],
    (canvas_width, canvas_height): (u32, u32),
    (scale_120, renderer_generation): (u32, u64),
    source: &ChromeSource,
    clip: Rect,
    color: u32,
    tints: Option<ChromeTints>,
) -> Result<()> {
    paint_picker_source_tinted(
        cache,
        context,
        used,
        canvas,
        canvas_width,
        canvas_height,
        source,
        ChromeTextStyle::Regular,
        scale_120,
        renderer_generation,
        clip,
        PickerTextAlignment::Left,
        color,
        tints,
        u32::MAX,
    )
}

/// The cells of a row's name before its label: the current-row marker, the
/// two-cell disclosure slot, and a space. A second line starts under the
/// label.
const fn row_name_prefix_cells(icons: bool) -> u32 {
    marker_cells(icons) + ChromeIcon::CELLS + 1
}

/// A row's text lines in buffer px, from its logical `row`: when it has a
/// `second` line and is taller than one line, its first [`ROW_HEIGHT`] for
/// the name and the rest below for the second line; otherwise the whole
/// row, whose one line is centred in it.
fn row_text_lines(row: Rect, second: bool, scale_120: u32) -> (Rect, Option<Rect>) {
    if !second || row.height <= ROW_HEIGHT {
        return (buffer_rect(row, scale_120), None);
    }
    let first = Rect {
        height: ROW_HEIGHT,
        ..row
    };
    let rest = Rect {
        y: row.y.saturating_add(ROW_HEIGHT),
        height: row.height - ROW_HEIGHT,
        ..row
    };
    (
        buffer_rect(first, scale_120),
        Some(buffer_rect(rest, scale_120)),
    )
}

/// A row's second line fitted to `cells` cells: an agent's detail keeps its
/// first words, as the footer's status does; a working directory keeps its
/// start and its tail (see [`fit_directory`]).
fn row_detail_text(detail: &LairExplorerDetail, cells: usize) -> String {
    match detail {
        LairExplorerDetail::Agent(text) => fit_status_line(text, cells),
        LairExplorerDetail::Directory(path) => fit_directory(path, cells),
    }
}

/// Fits a working directory into `cells` cells by eliding its middle, so
/// where it starts and the directory it is in both stay readable: its root
/// (`~/` or `/`), then `…`, then as many of its last path components as fit
/// whole, `~/…/crates/splinterm`. When not even the last component fits
/// whole, the tail is cut inside it, `~/…linterm`. A path with no root, or
/// a line with no room for it, is `…` and the path's last cells.
fn fit_directory(path: &str, cells: usize) -> String {
    if text_cells(path) <= cells {
        return path.to_owned();
    }
    if cells == 0 {
        return String::new();
    }
    let root = if path.starts_with("~/") {
        "~/"
    } else if path.starts_with('/') {
        "/"
    } else {
        ""
    };
    let rest = &path[root.len()..];
    let tail_cells = cells.saturating_sub(text_cells(root).saturating_add(1));
    if root.is_empty() || tail_cells == 0 {
        return format!("…{}", trailing_cells(path, cells - 1));
    }
    let tail = trailing_cells(rest, tail_cells);
    // From the first separator in what fits, the tail is whole components.
    let whole = tail
        .find('/')
        .map(|at| &tail[at..])
        .filter(|whole| whole.len() > 1);
    format!("{root}…{}", whole.unwrap_or(tail))
}

/// The longest end of `text` at most `cells` cells wide, starting on a
/// character that takes a cell.
fn trailing_cells(text: &str, cells: usize) -> &str {
    let mut used = 0_usize;
    let mut start = text.len();
    for (at, character) in text.char_indices().rev() {
        let width = character.width().unwrap_or(0).min(2);
        if used.saturating_add(width) > cells {
            break;
        }
        used = used.saturating_add(width);
        start = at;
    }
    let tail = &text[start..];
    tail.trim_start_matches(|character: char| character.width().unwrap_or(0) == 0)
}

/// Paints a row's second line, muted, in `clip`: the agent's detail or
/// the working directory, fitted to the whole cells it has.
#[allow(clippy::too_many_arguments)]
fn paint_row_detail(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    used: &mut HashSet<SessionPickerTextKey>,
    canvas: &mut [u8],
    (canvas_width, canvas_height): (u32, u32),
    (scale_120, renderer_generation): (u32, u64),
    detail: &LairExplorerDetail,
    clip: Rect,
    (palette, is_selected, cell_width): (SessionPickerPalette, bool, u32),
) -> Result<()> {
    let cells = usize::try_from(clip.width / cell_width.max(1)).unwrap_or(0);
    paint_picker_text(
        cache,
        context,
        used,
        canvas,
        canvas_width,
        canvas_height,
        &row_detail_text(detail, cells),
        ChromeTextStyle::Regular,
        scale_120,
        renderer_generation,
        clip,
        PickerTextAlignment::Left,
        if is_selected {
            palette.selected_secondary
        } else {
            palette.secondary
        },
    )
}

/// A row's text colour: brighter on the selection, muted while disabled.
const fn row_primary(palette: SessionPickerPalette, is_selected: bool, enabled: bool) -> u32 {
    match (is_selected, enabled) {
        (true, true) => palette.selected_primary,
        (true, false) => palette.selected_secondary,
        (false, true) => palette.primary,
        (false, false) => palette.secondary,
    }
}

/// The selected row's fill and accent bar in buffer px. Row hit areas span
/// the panel, but selection paint must not cover its right-hand divider at
/// `right`, including at fractional buffer scales.
fn paint_row_highlight(
    canvas: &mut [u8],
    (canvas_width, canvas_height): (u32, u32),
    rect: Rect,
    (border, right): (u32, u32),
    palette: SessionPickerPalette,
) {
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

/// One line of the agents section in buffer px: `rect` between the row's
/// `inset` and the same inset short of the divider at `right`.
fn agents_text_line(rect: Rect, inset: u32, right: u32) -> Rect {
    let left = rect.x.saturating_add(inset).min(right);
    Rect {
        x: left,
        width: right.saturating_sub(inset).saturating_sub(left),
        ..rect
    }
}

/// The agents section: a rule along the top of its label line, the muted
/// bold `agents` label with a muted `+k more` at its right while reporting
/// Splints are out of view, then one row per Splint in view, the selected
/// one highlighted as in the tree.
#[allow(clippy::too_many_arguments)]
fn paint_agents_section(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    used: &mut HashSet<SessionPickerTextKey>,
    canvas: &mut [u8],
    (canvas_width, canvas_height): (u32, u32),
    (scale_120, renderer_generation): (u32, u64),
    section: &LairExplorerAgentsLayout,
    palette: SessionPickerPalette,
    (agents, selected): (&[LairExplorerAgentRow], Option<NavigationNodeId>),
    (cell_width, border, right, inset): (u32, u32, u32, u32),
) -> Result<()> {
    let label = buffer_rect(section.label, scale_120);
    fill_rect(
        canvas,
        canvas_width,
        canvas_height,
        (
            i32::try_from(label.x).unwrap_or(i32::MAX),
            i32::try_from(label.y).unwrap_or(i32::MAX),
            right.saturating_sub(label.x),
            border,
        ),
        rgba(palette.frame),
    );
    let label_line = agents_text_line(label, inset, right);
    let half = label_line.width / 2;
    paint_picker_source(
        cache,
        context,
        used,
        canvas,
        canvas_width,
        canvas_height,
        &ChromeSource::from(AGENTS_TEXT),
        ChromeTextStyle::Bold,
        scale_120,
        renderer_generation,
        Rect {
            width: half,
            ..label_line
        },
        PickerTextAlignment::Left,
        palette.secondary,
    )?;
    if section.hidden > 0 {
        paint_picker_text(
            cache,
            context,
            used,
            canvas,
            canvas_width,
            canvas_height,
            &format!("+{} more", section.hidden),
            ChromeTextStyle::Regular,
            scale_120,
            renderer_generation,
            Rect {
                x: label_line.x.saturating_add(half),
                width: label_line.width - half,
                ..label_line
            },
            PickerTextAlignment::Right,
            palette.secondary,
        )?;
    }
    for (layout_row, agent) in section
        .rows
        .iter()
        .zip(agents.iter().skip(section.visible_start))
    {
        let rect = buffer_rect(layout_row.rect, scale_120);
        let is_selected = selected == Some(agent.row.id);
        if is_selected {
            paint_row_highlight(
                canvas,
                (canvas_width, canvas_height),
                rect,
                (border, right),
                palette,
            );
        }
        let (first_line, second_line) =
            row_text_lines(layout_row.rect, agent.row.detail.is_some(), scale_120);
        paint_agent_row(
            cache,
            context,
            used,
            canvas,
            (canvas_width, canvas_height),
            (scale_120, renderer_generation),
            (
                agents_text_line(first_line, inset, right),
                second_line.map(|line| agents_text_line(line, inset, right)),
            ),
            agent,
            palette,
            (is_selected, cell_width),
        )?;
    }
    Ok(())
}

/// One agents row's text on its `line`: the Splint's glyph column at the
/// left, then its name, and its muted location at the right when it fits.
/// A `second` line shows the row's detail under the name.
#[allow(clippy::too_many_arguments)]
fn paint_agent_row(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    used: &mut HashSet<SessionPickerTextKey>,
    canvas: &mut [u8],
    (canvas_width, canvas_height): (u32, u32),
    (scale_120, renderer_generation): (u32, u64),
    (line, second): (Rect, Option<Rect>),
    agent: &LairExplorerAgentRow,
    palette: SessionPickerPalette,
    (is_selected, cell_width): (bool, u32),
) -> Result<()> {
    let row = &agent.row;
    let primary = row_primary(palette, is_selected, row.enabled);
    let glyph_width = row.agent_column.as_ref().map_or(0, |column| {
        cell_width.saturating_mul(u32::try_from(column.cells()).unwrap_or(u32::MAX))
    });
    if let Some(column) = &row.agent_column {
        paint_row_glyphs(
            cache,
            context,
            used,
            canvas,
            (canvas_width, canvas_height),
            (scale_120, renderer_generation),
            column,
            Rect {
                width: glyph_width.min(line.width),
                ..line
            },
            primary,
            palette.row_tints(is_selected),
        )?;
    }
    let gap = glyph_width.saturating_add(cell_width).min(line.width);
    let text = Rect {
        x: line.x.saturating_add(gap),
        width: line.width - gap,
        ..line
    };
    let cells = usize::try_from(text.width / cell_width.max(1)).unwrap_or(0);
    let (name, location) = agent_row_text(&row.label, &agent.location, cells);
    paint_picker_text(
        cache,
        context,
        used,
        canvas,
        canvas_width,
        canvas_height,
        &name,
        ChromeTextStyle::Regular,
        scale_120,
        renderer_generation,
        text,
        PickerTextAlignment::Left,
        primary,
    )?;
    if let Some(location) = location {
        paint_picker_text(
            cache,
            context,
            used,
            canvas,
            canvas_width,
            canvas_height,
            &location,
            ChromeTextStyle::Regular,
            scale_120,
            renderer_generation,
            text,
            PickerTextAlignment::Right,
            if is_selected {
                palette.selected_secondary
            } else {
                palette.secondary
            },
        )?;
    }
    if let (Some(detail), Some(second)) = (&row.detail, second) {
        paint_row_detail(
            cache,
            context,
            used,
            canvas,
            (canvas_width, canvas_height),
            (scale_120, renderer_generation),
            detail,
            Rect {
                y: second.y,
                height: second.height,
                ..text
            },
            (palette, is_selected, cell_width),
        )?;
    }
    Ok(())
}

/// An action target in buffer px, never past the divider at `right`.
fn action_clip(target: Rect, scale_120: u32, right: u32) -> Rect {
    let clip = buffer_rect(target, scale_120);
    Rect {
        width: clip.width.min(right.saturating_sub(clip.x)),
        ..clip
    }
}

/// Where an action word goes inside its buffer `target`: past `padding`
/// buffer px on each side.
fn action_text_clip(target: Rect, padding: u32) -> Rect {
    let padding = padding.min(target.width / 2);
    Rect {
        x: target.x.saturating_add(padding),
        width: target.width - padding * 2,
        ..target
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

/// The rail header's `»`, muted and centred in its target.
#[allow(clippy::too_many_arguments)]
fn paint_rail_toggle(
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
    let Some(toggle) = layout.toggle() else {
        return Ok(());
    };
    paint_picker_source_within(
        cache,
        context,
        used,
        canvas,
        canvas_width,
        canvas_height,
        &ChromeSource::from(EXPAND_TEXT),
        ChromeTextStyle::Regular,
        scale_120,
        renderer_generation,
        action_clip(toggle, scale_120, right),
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
    paint_rail_toggle(
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

    const UNMEASURED: LairExplorerMetrics = LairExplorerMetrics::UNMEASURED;

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
            detail: None,
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
            (&[], None),
            "",
            None,
            focused,
            filter,
            false,
            ExplorerCurrentMarker::default(),
        )
        .unwrap();
    }

    fn lair_rows(count: usize) -> Vec<LairExplorerRow> {
        (0..count)
            .map(|_| row(NavigationNodeId::Lair(LairId::new())))
            .collect()
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
            UNMEASURED,
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
        assert_eq!(rail.toggle(), Some(rail.header));
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
        let rail = lair_explorer_shown_layout(content, None, 640, UNMEASURED, &many, None, 0, true)
            .unwrap();
        let expanded =
            lair_explorer_shown_layout(content, None, 640, UNMEASURED, &many, None, 0, false)
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
            lair_explorer_shown_layout(content, None, 640, UNMEASURED, &many, Some(last), 0, true)
                .unwrap();
        assert_eq!(rail.rows.last().unwrap().id, last);
    }

    /// A rail row is hit across its whole width and height and has no
    /// disclosure; the `»` is the header line, the rail's only action.
    #[test]
    fn rail_rows_and_the_header_toggle_hit_test_apart() {
        let rows = lair_rows(2);
        let content = Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 400,
        };
        let rail = lair_explorer_shown_layout(content, None, 640, UNMEASURED, &rows, None, 0, true)
            .unwrap();
        assert_eq!(
            rail.actions,
            vec![(LairExplorerAction::Toggle, rail.header)]
        );
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
                    assert_eq!(lair_explorer_action_hit_test(&rail, (x, y)), None);
                }
            }
        }
        assert_eq!(
            lair_explorer_action_hit_test(&rail, (20.0, 5.0)),
            Some(LairExplorerAction::Toggle)
        );
        assert_eq!(lair_explorer_hit_test(&rail, (20.0, 5.0)), None);
        assert_eq!(lair_explorer_filter_hit_test(&rail, (20.0, 5.0)), None);
        // Past the rail is the terminal.
        let beside = (f64::from(RAIL_WIDTH), f64::from(rail.rows[0].rect.y + 1));
        assert_eq!(lair_explorer_hit_test(&rail, beside), None);
        // A header cut short by a tiny Window shows no toggle.
        let short = Rect {
            height: HEADER_HEIGHT - 1,
            ..content
        };
        for collapsed in [false, true] {
            let layout =
                lair_explorer_shown_layout(short, None, 640, UNMEASURED, &rows, None, 0, collapsed)
                    .unwrap();
            assert!(layout.actions.is_empty(), "collapsed={collapsed}");
        }
    }

    /// The expanded footer's first line is `new · menu · «`, left to right
    /// from the text inset, each word a muted target with dead space for the
    /// `·` between neighbours. The targets lie on the action line, inside
    /// the footer and short of the divider, and hit-test apart from the
    /// rows, the filters, and each other. The filters run to the panel's
    /// inset again: the header no longer holds the `«`.
    #[test]
    fn footer_action_line_lays_out_new_menu_and_collapse() {
        use LairExplorerAction::{Menu, New, Toggle};

        let rows = lair_rows(3);
        let content = Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 400,
        };
        let layout =
            lair_explorer_shown_layout(content, None, 640, UNMEASURED, &rows, None, 0, false)
                .unwrap();
        let shown: Vec<_> = layout.actions.iter().map(|(action, _)| *action).collect();
        assert_eq!(shown, [New, Menu, Toggle]);
        let line_y = layout.footer.y + FOOTER_PADDING;
        for pair in layout.actions.windows(2) {
            let (left, right) = (pair[0].1, pair[1].1);
            assert_eq!(left.x + left.width + UNMEASURED.separator_width(), right.x);
        }
        for (action, rect) in &layout.actions {
            assert_eq!((rect.y, rect.height), (line_y, FOOTER_LINE_HEIGHT));
            assert!(rect.x >= layout.panel.x + LEVEL_INDENT - ACTION_PADDING);
            assert!(rect.x + rect.width <= layout.panel.x + layout.panel.width - FOOTER_PADDING);
            assert!(rect.y + rect.height <= layout.footer.y + layout.footer.height);
            let inside = |dx: u32, dy: u32| (f64::from(rect.x + dx), f64::from(rect.y + dy));
            for point in [
                inside(0, 0),
                inside(rect.width - 1, rect.height - 1),
                inside(rect.width / 2, rect.height / 2),
            ] {
                assert_eq!(lair_explorer_action_hit_test(&layout, point), Some(*action));
                assert_eq!(lair_explorer_hit_test(&layout, point), None);
                assert_eq!(lair_explorer_filter_hit_test(&layout, point), None);
            }
            // The `·` between targets is dead space.
            let gap = (f64::from(rect.x + rect.width) + 1.0, f64::from(line_y + 2));
            assert_eq!(lair_explorer_action_hit_test(&layout, gap), None);
        }
        assert_eq!(layout.toggle().map(|rect| rect.width), Some(TOGGLE_WIDTH));
        // The first word starts at the context line's inset.
        assert_eq!(
            layout.actions[0].1.x + ACTION_PADDING,
            layout.panel.x + LEVEL_INDENT
        );
        // Nothing on the header or the context line is an action, but the
        // whole footer, both lines and the gaps between targets, is footer
        // rather than rows; the rows and the header are not.
        assert_eq!(lair_explorer_action_hit_test(&layout, (280.0, 5.0)), None);
        assert!(!lair_explorer_footer_hit_test(&layout, (280.0, 5.0)));
        let context_y = f64::from(line_y + FOOTER_LINE_HEIGHT + 2);
        for x in [20.0, 60.0, 120.0] {
            assert_eq!(lair_explorer_action_hit_test(&layout, (x, context_y)), None);
            assert!(lair_explorer_footer_hit_test(&layout, (x, context_y)));
        }
        for (_, rect) in &layout.actions {
            let gap = (f64::from(rect.x + rect.width) + 1.0, f64::from(line_y + 2));
            assert!(lair_explorer_footer_hit_test(&layout, gap));
        }
        for x in [0.0, f64::from(layout.panel.width) - 1.0] {
            let padding = (x, f64::from(layout.footer.y));
            assert!(lair_explorer_footer_hit_test(&layout, padding));
            let bottom = (x, f64::from(layout.footer.y + layout.footer.height) - 0.5);
            assert!(lair_explorer_footer_hit_test(&layout, bottom));
        }
        for laid in &layout.rows {
            let row = (20.0, f64::from(laid.rect.y + 1));
            assert!(!lair_explorer_footer_hit_test(&layout, row));
        }
        let past = (
            f64::from(layout.panel.x + layout.panel.width),
            f64::from(layout.footer.y + 2),
        );
        assert!(!lair_explorer_footer_hit_test(&layout, past));
        // The filters take back the header's right end: they run to the
        // panel's inset.
        let (_, last_filter) = layout.filters[2];
        assert_eq!(
            last_filter.x + last_filter.width,
            layout.panel.x + layout.panel.width - 8
        );
        assert_eq!(
            header_filter_rects(layout.panel, HEADER_HEIGHT),
            layout.filters
        );
        // Every expanded panel can collapse, a drawer too, even where the
        // rail itself would be a drawer.
        for width in [700, 650] {
            let drawer = Rect { width, ..content };
            let layout =
                lair_explorer_shown_layout(drawer, None, 640, UNMEASURED, &rows, None, 0, false)
                    .unwrap();
            assert_eq!(layout.mode, LairExplorerPresentationMode::Drawer);
            let shown: Vec<_> = layout.actions.iter().map(|(action, _)| *action).collect();
            assert_eq!(shown, [New, Menu, Toggle]);
        }
        // A footer cut short by a short Window has no actions.
        let short = Rect {
            height: HEADER_HEIGHT + FOOTER_HEIGHT - 1,
            ..content
        };
        let layout =
            lair_explorer_shown_layout(short, None, 640, UNMEASURED, &rows, None, 0, false)
                .unwrap();
        assert!(layout.footer.height < FOOTER_HEIGHT);
        assert!(layout.actions.is_empty());
    }
    /// A narrow footer drops `menu` first, then `new`, keeping `«` longest;
    /// a footer too narrow for even `«` shows none. No two targets ever
    /// overlap, and none reaches the divider, whatever the chrome cell.
    #[test]
    fn narrow_footers_drop_menu_then_new_and_keep_the_collapse() {
        use LairExplorerAction::{Menu, New, Toggle};

        let footer = |width| Rect {
            x: 10,
            y: 100,
            width,
            height: FOOTER_HEIGHT,
        };
        for metrics in [
            UNMEASURED,
            LairExplorerMetrics::measured(8, 120),
            LairExplorerMetrics::measured(14, 120),
            LairExplorerMetrics::measured(17, 150),
            LairExplorerMetrics::measured(21, 180),
            LairExplorerMetrics::measured(29, 240),
        ] {
            let width = |action| metrics.action_width(action);
            let separator = metrics.separator_width();
            let left = LEVEL_INDENT - ACTION_PADDING;
            let full =
                left + width(New) + width(Menu) + width(Toggle) + separator * 2 + FOOTER_PADDING;
            let without_menu = left + width(New) + width(Toggle) + separator + FOOTER_PADDING;
            let only_toggle = left + width(Toggle) + FOOTER_PADDING;
            let shown = |width| -> Vec<LairExplorerAction> {
                footer_action_rects(footer(width), metrics)
                    .into_iter()
                    .map(|(action, _)| action)
                    .collect()
            };
            assert_eq!(shown(full), [New, Menu, Toggle], "{metrics:?}");
            assert_eq!(shown(full - 1), [New, Toggle], "{metrics:?}");
            assert_eq!(shown(without_menu), [New, Toggle], "{metrics:?}");
            assert_eq!(shown(without_menu - 1), [Toggle], "{metrics:?}");
            assert_eq!(shown(only_toggle), [Toggle], "{metrics:?}");
            assert!(shown(only_toggle - 1).is_empty(), "{metrics:?}");
            // The expanded panel's own width keeps every action.
            assert!(full <= PREFERRED_WIDTH, "{metrics:?}");
            for width in 0..=PREFERRED_WIDTH {
                let rects = footer_action_rects(footer(width), metrics);
                for (index, (_, rect)) in rects.iter().enumerate() {
                    assert!(
                        rect.x + rect.width <= 10 + width - FOOTER_PADDING,
                        "{metrics:?} {width}"
                    );
                    if let Some((_, next)) = rects.get(index + 1) {
                        assert_eq!(
                            rect.x + rect.width + separator,
                            next.x,
                            "{metrics:?} {width}"
                        );
                    }
                }
            }
        }
    }

    /// The footer's second line, in priority: the search field while a
    /// search is open, a navigation notice, a query left behind, the
    /// selected row's short status, then the input hint. Only the status
    /// is painted as the row's own text.
    #[test]
    fn footer_context_line_follows_its_priority_order() {
        let (status_text, notice_text) = ("1 needs input · saved", "Navigation is stale · R retry");
        let (status, notice) = (Some(status_text), Some(notice_text));
        assert_eq!(
            explorer_context_text(true, true, "abc", notice, status, 40),
            ("Search: abc_".to_owned(), false)
        );
        assert_eq!(
            explorer_context_text(true, false, "", notice, status, 40),
            (notice_text.to_owned(), false)
        );
        assert_eq!(
            explorer_context_text(true, false, "abc", None, status, 40),
            ("Search: abc".to_owned(), false)
        );
        for focused in [false, true] {
            assert_eq!(
                explorer_context_text(focused, false, "", None, status, 40),
                (status_text.to_owned(), true)
            );
            // An empty status, or no selection, leaves the line to the hint.
            for empty in [None, Some("")] {
                assert_eq!(
                    explorer_context_text(focused, false, "", None, empty, 40),
                    (explorer_hint_text(focused).to_owned(), false)
                );
            }
        }
        // The status keeps the compact line's fitting rules.
        let long = "asking for access · 1 needs input · 2 working · saved";
        let (fitted, is_status) = explorer_context_text(false, false, "", None, Some(long), 20);
        assert!(is_status);
        assert_eq!(fitted, fit_status_line(long, 20));
        assert!(text_cells(&fitted) <= 20);
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
        let metrics = LairExplorerMetrics::measured(14, 120);
        let wide = metrics.rail_width;
        let rail =
            lair_explorer_shown_layout(content, None, 640, metrics, &rows, None, 0, true).unwrap();
        assert_eq!(rail.panel.width, wide);
        assert_eq!(rail.terminal.x, wide);
        assert_eq!(rail.terminal.width, 1000 - wide);
        assert_eq!(rail.toggle().map(|toggle| toggle.width), Some(wide));
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
            lair_explorer_shown_layout(edge, None, 640, metrics, &rows, None, 0, true).unwrap();
        assert_eq!(docked.mode, LairExplorerPresentationMode::Docked);
        let narrower = Rect {
            width: 640 + wide - 1,
            ..content
        };
        let drawer =
            lair_explorer_shown_layout(narrower, None, 640, metrics, &rows, None, 0, true).unwrap();
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
        let rail = lair_explorer_shown_layout(content, None, 640, UNMEASURED, &rows, None, 0, true)
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
        assert_eq!(rail.toggle(), Some(rail.header));
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
        let rail = lair_explorer_shown_layout(sliver, None, 640, UNMEASURED, &rows, None, 0, true)
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
            lair_explorer_shown_layout(content, None, 640, UNMEASURED, &rows, None, 0, true)
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
                    (&[], None),
                    "",
                    notice,
                    false,
                    LairExplorerFilter::Live,
                    false,
                    ExplorerCurrentMarker::default(),
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
            lair_explorer_shown_layout(content, None, 640, UNMEASURED, &rows, None, 0, true)
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
            UNMEASURED,
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

    /// The context line with no row status to show.
    fn context_text(
        focused: bool,
        searching: bool,
        query: &str,
        notice: Option<&str>,
        cells: usize,
    ) -> String {
        explorer_context_text(focused, searching, query, notice, None, cells).0
    }

    #[test]
    fn footer_context_line_makes_input_ownership_explicit() {
        assert_eq!(
            context_text(false, false, "", None, 40),
            "Terminal has keys · click to move"
        );
        assert_eq!(
            context_text(true, false, "", None, 40),
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
                context_text(true, true, "abc", Some(notice), 40),
                "Search: abc_"
            );
            assert_eq!(context_text(true, false, "", Some(notice), 40), notice);
            assert_eq!(context_text(true, false, "abc", Some(notice), 40), notice);
        }
        assert_eq!(HEADER_TEXT, "lairs");
    }

    #[test]
    fn search_footer_does_not_advertise_tree_filter_shortcut() {
        let field = context_text(true, true, "dojo", None, 40);
        assert_eq!(field, "Search: dojo_");
        assert!(!field.contains("F filter"));
        // A long query keeps its latest input and the caret.
        let long = context_text(true, true, "a-very-long-search-query", None, 20);
        assert_eq!(long, "Search: …arch-query_");
        assert!(text_cells(&long) <= 20);
        // A query left behind after returning to the tree is still shown.
        assert_eq!(context_text(true, false, "dojo", None, 40), "Search: dojo");
        assert_eq!(
            context_text(false, true, "", None, 40),
            context_text(false, false, "", None, 40)
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
    /// ellipsized before the icon columns, the columns fit the row, and the
    /// footer's action words and their `·`s fit their targets and gaps.
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
                    &row_name_source(&rows[0], true, ChromeIcon::Eye),
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
                assert_action_words_fit(&context, scale, &layout);
            }
        }
    }

    /// Sized from `context`'s measured chrome cell at `scale`, every action
    /// word on `layout`'s footer fits its target whole past the padding, and
    /// each `·` fits the gap before its word.
    fn assert_action_words_fit(context: &RenderContext, scale: u32, layout: &LairExplorerLayout) {
        use super::super::super::ChromeText;

        let cell = lair_explorer_chrome_cell_width(context, scale).unwrap();
        let metrics = LairExplorerMetrics::measured(cell, scale);
        let panel = buffer_rect(layout.panel, scale);
        let right = panel.x + panel.width - scale.div_ceil(120);
        let padding = ACTION_PADDING.saturating_mul(scale).div_ceil(120);
        let actions = footer_action_rects(layout.footer, metrics);
        assert_eq!(actions.len(), 3, "scale {scale}, cell {cell}");
        let dot = ChromeText::load_with_context(ACTION_SEPARATOR, scale, context).unwrap();
        let mut previous_end = None;
        for (action, rect) in &actions {
            let target = action_clip(*rect, scale, right);
            let clip = action_text_clip(target, padding);
            let word = ChromeText::load_with_context(action.text(), scale, context).unwrap();
            assert!(
                word.pixel_width() <= clip.width,
                "{action:?} at scale {scale}, cell {cell}: {} > {}",
                word.pixel_width(),
                clip.width
            );
            if let Some(end) = previous_end {
                assert!(
                    dot.pixel_width() <= target.x - end,
                    "`·` before {action:?} at scale {scale}, cell {cell}"
                );
            }
            previous_end = Some(target.x + target.width);
        }
    }

    /// Paints one unselected row and returns the canvas, its width, the
    /// row's name and column clips, and the chrome cell width.
    fn paint_row(
        context: &RenderContext,
        painted: LairExplorerRow,
        scale: u32,
    ) -> (Vec<u8>, u32, Rect, Rect, u32) {
        paint_row_marked(context, painted, scale, ExplorerCurrentMarker::default())
    }

    /// [`paint_row`] with the current-row `marker`.
    fn paint_row_marked(
        context: &RenderContext,
        painted: LairExplorerRow,
        scale: u32,
        marker: ExplorerCurrentMarker,
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
        paint_lair_explorer(
            &mut SessionPickerTextCache::default(),
            context,
            &mut canvas,
            canvas_width,
            canvas_height,
            scale,
            1,
            &layout,
            session_picker_palette(ResolvedTheme::default()),
            &rows,
            None,
            (&[], None),
            "",
            None,
            false,
            LairExplorerFilter::All,
            false,
            marker,
        )
        .unwrap();
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
                    changed_only_in_cells(&painted, &blank, width, label, cell, 2..4, &what);
                assert!(changed > 0, "{what} painted nothing");
                chevrons.push(painted);
            }
            assert_ne!(chevrons[0], chevrons[1]);
            // Every current-row marker keeps the two cells before the
            // chevron, and a current Splint's keeps the slot, so the label
            // and the chevron stay where they are on every row.
            let mut markers = Vec::new();
            for marker in ExplorerCurrentMarker::ALL {
                let what = format!("{} marker at scale {scale}", marker.name());
                let (current, _, _, _, _) = paint_row_marked(
                    &context,
                    LairExplorerRow {
                        current: true,
                        ..base.clone()
                    },
                    scale,
                    marker,
                );
                let changed =
                    changed_only_in_cells(&current, &chevrons[0], width, label, cell, 0..2, &what);
                assert!(changed > 0, "{what} painted nothing");
                let (splint, _, _, _, _) = paint_row_marked(
                    &context,
                    LairExplorerRow {
                        current: true,
                        expanded: None,
                        ..base.clone()
                    },
                    scale,
                    marker,
                );
                let what = format!("{what} on a Splint");
                let changed =
                    changed_only_in_cells(&splint, &blank, width, label, cell, 2..4, &what);
                assert!(changed > 0, "{what} painted nothing");
                markers.push(current);
            }
            let mut distinct = markers.clone();
            distinct.sort_unstable();
            distinct.dedup();
            assert_eq!(distinct.len(), markers.len(), "scale {scale}");
        }
    }

    /// With the icon face, the current-row marker is a two-cell icon and a
    /// row that is not current leaves both cells blank; without it, the
    /// one-cell `●` and its spacing are as before. Either way every row's
    /// label starts at the same cell.
    #[test]
    fn current_marker_marks_only_current_rows_and_keeps_labels_aligned() {
        let lair = row(NavigationNodeId::Lair(LairId::new()));
        let splint = LairExplorerRow {
            expanded: None,
            ..lair.clone()
        };
        let pending = LairExplorerRow {
            pending: true,
            ..lair.clone()
        };
        let current = |row: &LairExplorerRow| LairExplorerRow {
            current: true,
            ..row.clone()
        };
        let label = |source: &ChromeSource| source.cells() - text_cells("work");
        for marker in ExplorerCurrentMarker::ALL {
            let icon = ChromeIcon::from(marker);
            let name = icon.name();
            let cases = [
                (&lair, "  [chevron-right] work".to_owned()),
                (&current(&lair), format!("[{name}][chevron-right] work")),
                (&splint, "     work".to_owned()),
                (&current(&splint), format!("  [{name}] work")),
                (&pending, "   … work".to_owned()),
                (&current(&pending), "   … work".to_owned()),
            ];
            for (row, described) in cases {
                let source = row_name_source(row, true, icon);
                assert_eq!(source.describe(), described);
                assert_eq!(label(&source), 5, "{described}");
            }
            let expanded = LairExplorerRow {
                expanded: Some(true),
                ..current(&lair)
            };
            assert_eq!(
                row_name_source(&expanded, true, icon).describe(),
                format!("[{name}][chevron-down] work")
            );
            let fallback = [
                (&lair, "  > work"),
                (&current(&lair), "● > work"),
                (&expanded, "● v work"),
                (&splint, "    work"),
                (&current(&splint), "  ● work"),
                (&pending, "  … work"),
            ];
            for (row, text) in fallback {
                let source = row_name_source(row, false, icon);
                assert_eq!(source.text(), text);
                assert_eq!(source.describe(), text);
                assert_eq!(label(&source), 4, "{text}");
            }
        }
        assert_eq!(row_name_prefix_cells(true), 5);
        assert_eq!(row_name_prefix_cells(false), 4);
        assert_eq!(disclosure_width(1, false), LEVEL_INDENT * 4);
        assert_eq!(disclosure_width(1, true), LEVEL_INDENT * 5);
    }

    /// The selected row's short status fills the footer's context line in
    /// place of the input hint, and a row with nothing more to say leaves
    /// the hint there rather than repeating its name, as does having no row
    /// selected. Selection changes nothing else outside the row.
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
                // The action line, then the context line.
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
                    let line = lines[FOOTER_CONTEXT_LINE];
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

    /// Everything the action line paints, its words and their `·`s, stays
    /// on the action line's rows, between the first target's left edge and
    /// the last one's right edge, and left of the divider, at every scale
    /// and at the largest chrome font, with the targets sized from the
    /// measured chrome cell.
    #[test]
    fn action_line_paints_inside_its_targets_and_gaps() {
        use super::super::picker::session_picker_palette;
        use crate::config::ResolvedTheme;

        let rows = lair_rows(2);
        let surface = Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: HEADER_HEIGHT + HEADER_SPACER + FOOTER_HEIGHT + ROW_HEIGHT * 2,
        };
        let palette = session_picker_palette(ResolvedTheme::default());
        for context in [
            RenderContext::new(u16::MAX),
            RenderContext::new(u16::MAX).at_maximum_chrome_font_size(),
        ] {
            for scale in [120, 150, 180, 240] {
                let metrics = LairExplorerMetrics::measured(
                    lair_explorer_chrome_cell_width(&context, scale).unwrap(),
                    scale,
                );
                let layout =
                    lair_explorer_shown_layout(surface, None, 640, metrics, &rows, None, 0, false)
                        .unwrap();
                assert_eq!(layout.actions.len(), 3, "scale {scale}");
                let without = LairExplorerLayout {
                    actions: Vec::new(),
                    ..layout.clone()
                };
                let canvas_width = surface.width * scale / 120 + 1;
                let canvas_height = surface.height * scale / 120 + 1;
                let draw = |layout: &LairExplorerLayout| {
                    let mut canvas = vec![0; (canvas_width * canvas_height * 4) as usize];
                    paint(
                        &context,
                        &mut canvas,
                        canvas_width,
                        canvas_height,
                        scale,
                        layout,
                        palette,
                        &rows,
                        None,
                        true,
                        LairExplorerFilter::All,
                    );
                    canvas
                };
                let (with, bare) = (draw(&layout), draw(&without));
                let inset = LEVEL_INDENT.saturating_mul(scale).div_ceil(120);
                let line = footer_lines(buffer_rect(layout.footer, scale), scale, inset)
                    [FOOTER_ACTION_LINE];
                let panel = buffer_rect(layout.panel, scale);
                let right = panel.x + panel.width - scale.div_ceil(120);
                let first = buffer_rect(layout.actions[0].1, scale);
                let last = action_clip(layout.actions[2].1, scale, right);
                let mut painted = 0_usize;
                for (index, (a, b)) in with.chunks_exact(4).zip(bare.chunks_exact(4)).enumerate() {
                    if a == b {
                        continue;
                    }
                    let x = u32::try_from(index).unwrap() % canvas_width;
                    let y = u32::try_from(index).unwrap() / canvas_width;
                    assert!(
                        (line.y..line.y + line.height).contains(&y),
                        "scale {scale}: action ink off its line at {x},{y}"
                    );
                    assert!(
                        (first.x..last.x + last.width).contains(&x) && x < right,
                        "scale {scale}: action ink outside its targets at {x},{y}"
                    );
                    painted += 1;
                }
                assert!(painted > 0, "scale {scale}");
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
        assert!(!lair_explorer_shows_working(&top, &rows, &[]));
        let bottom = lair_explorer_layout(content(400), 640, &rows, Some(rows[29].id), 0).unwrap();
        assert!(lair_explorer_shows_working(&bottom, &rows, &[]));
        let no_room = lair_explorer_layout(
            content(HEADER_HEIGHT + HEADER_SPACER + FOOTER_HEIGHT),
            640,
            &rows,
            Some(rows[29].id),
            0,
        )
        .unwrap();
        assert_eq!(no_room.visible_capacity, 0);
        assert!(!lair_explorer_shows_working(&no_room, &rows, &[]));
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
                    (&[], None),
                    "",
                    notice,
                    true,
                    LairExplorerFilter::All,
                    false,
                    ExplorerCurrentMarker::default(),
                )
                .unwrap();
                canvas
            };
            let noticed = draw(Some("Navigation is stale · R retry"));
            let plain = draw(None);
            let inset = LEVEL_INDENT.saturating_mul(scale).div_ceil(120);
            let context_line =
                footer_lines(buffer_rect(layout.footer, scale), scale, inset)[FOOTER_CONTEXT_LINE];
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
        let expected = chrome_panel_palette_on(light, ChromeSurface::Menu, 0xf0_f0_f0);
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

    /// `count` reporting Splints for the agents section, named `agent-0`
    /// on, each with a glyph column.
    fn agent_rows(count: usize) -> Vec<LairExplorerAgentRow> {
        (0..count)
            .map(|index| {
                let id = NavigationNodeId::Splint {
                    lair_id: LairId::new(),
                    dojo_id: splinterm_core::DojoId::new(),
                    splint_id: splinterm_core::SplintId::new(),
                };
                let mut agent = row(id);
                agent.kind = LairExplorerRowKind::Splint;
                agent.label = format!("agent-{index}");
                agent.expanded = None;
                agent.agent_column = Some(ChromeSource::from("! "));
                LairExplorerAgentRow {
                    row: agent,
                    location: "work / editor".into(),
                }
            })
            .collect()
    }

    /// The expanded layout of `content` with `rows` and the agents section
    /// for `agents`.
    fn agents_layout(
        content: Rect,
        rows: &[LairExplorerRow],
        agents: &[LairExplorerAgentRow],
        agent_selected: Option<NavigationNodeId>,
    ) -> LairExplorerLayout {
        lair_explorer_with_agents(
            lair_explorer_layout(content, 640, rows, None, 0).unwrap(),
            rows,
            (None, 0),
            agents,
            agent_selected,
            0,
        )
    }

    /// The fixed split: up to five agent rows and a third of the list, at
    /// least one, under the label line, always leaving the tree a row; no
    /// section in a list under three rows or with nothing reporting.
    #[test]
    fn agents_section_takes_a_fixed_share_of_the_list() {
        for capacity in 0..3 {
            assert_eq!(agents_section_rows(capacity, 4, 1), None, "{capacity}");
        }
        assert_eq!(agents_section_rows(20, 0, 1), None);
        for (capacity, count, shown) in [
            (3, 4, 1),
            (5, 4, 1),
            (6, 4, 2),
            (9, 4, 3),
            (12, 4, 4),
            (15, 4, 4),
            (15, 9, 5),
            (40, 9, 5),
            (40, 2, 2),
            (4, 1, 1),
        ] {
            assert_eq!(
                agents_section_rows(capacity, count, 1),
                Some(shown),
                "{capacity} rows, {count} reporting"
            );
        }
        for capacity in 3..60 {
            for count in 1..12 {
                let shown = agents_section_rows(capacity, count, 1).unwrap();
                assert!((1..=AGENTS_MAXIMUM_ROWS).contains(&shown));
                assert!(shown + 2 <= capacity, "the label and one tree row fit");
            }
        }
    }

    /// At several Window heights the section sits right above the footer:
    /// its label line, then its rows, back to back, with the tree's rows all
    /// above the label. The tree keeps scrolling in what is left, the
    /// section counts the Splints it cannot show, and the rail, a list with
    /// nothing reporting, or one too short is laid out as before.
    #[test]
    fn agents_section_splits_off_the_bottom_of_the_list_at_every_height() {
        let rows = lair_rows(30);
        let agents = agent_rows(7);
        for height in [180, 240, 400, 600, 900] {
            let content = Rect {
                x: 0,
                y: 34,
                width: 1000,
                height,
            };
            let plain = lair_explorer_layout(content, 640, &rows, None, 0).unwrap();
            let layout = agents_layout(content, &rows, &agents, None);
            let section = layout.agents.as_ref().expect("a section");
            let capacity = u32::try_from(plain.visible_capacity).unwrap();
            let shown = agents_section_rows(capacity, agents.len(), 1).unwrap();
            assert_eq!(section.rows.len(), shown as usize, "height {height}");
            assert_eq!(section.hidden, agents.len() - shown as usize);
            assert_eq!(
                section.label.y + ROW_HEIGHT * (shown + 1),
                layout.footer.y,
                "height {height}"
            );
            assert_eq!(section.label.height, ROW_HEIGHT);
            for (index, laid) in section.rows.iter().enumerate() {
                let index = u32::try_from(index).unwrap();
                assert_eq!(laid.rect.y, section.label.y + ROW_HEIGHT * (index + 1));
                assert_eq!(laid.id, agents[index as usize].row.id);
                assert_eq!((laid.rect.x, laid.rect.width), (0, layout.panel.width));
            }
            for laid in &layout.rows {
                assert!(laid.rect.y + laid.rect.height <= section.label.y);
            }
            assert_eq!(
                layout.rows.first().map(|laid| laid.rect.y),
                plain.rows.first().map(|laid| laid.rect.y)
            );
            assert!(layout.visible_capacity >= 1);
            assert!(layout.visible_capacity < plain.visible_capacity);
        }
        // Nothing reporting, or no room: the layout is as it was.
        let content = Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 400,
        };
        let plain = lair_explorer_layout(content, 640, &rows, None, 0).unwrap();
        assert_eq!(agents_layout(content, &rows, &[], None), plain);
        let short = Rect {
            height: HEADER_HEIGHT + HEADER_SPACER + FOOTER_HEIGHT + ROW_HEIGHT * 2,
            ..content
        };
        let plain_short = lair_explorer_layout(short, 640, &rows, None, 0).unwrap();
        assert_eq!(plain_short.visible_capacity, 2);
        assert_eq!(agents_layout(short, &rows, &agents, None), plain_short);
        // The rail never has the section.
        let rail = lair_explorer_shown_layout(content, None, 640, UNMEASURED, &rows, None, 0, true)
            .unwrap();
        assert_eq!(
            lair_explorer_with_agents(rail.clone(), &rows, (None, 0), &agents, None, 0),
            rail
        );
        // The tree keeps its selection in view in the rows left to it.
        let last = rows[29].id;
        let layout = lair_explorer_with_agents(
            lair_explorer_layout(content, 640, &rows, Some(last), 0).unwrap(),
            &rows,
            (Some(last), 0),
            &agents,
            None,
            0,
        );
        assert_eq!(layout.rows.last().map(|laid| laid.id), Some(last));
    }

    /// The section scrolls to keep its selected row in view, from the start
    /// it was left at, and its label counts the rows out of view.
    #[test]
    fn agents_section_scrolls_to_its_selection() {
        let rows = lair_rows(3);
        let agents = agent_rows(9);
        let content = Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 900,
        };
        let layout = agents_layout(content, &rows, &agents, Some(agents[8].row.id));
        let section = layout.agents.unwrap();
        assert_eq!(section.rows.len(), 5);
        assert_eq!(section.visible_start, 4);
        assert_eq!(
            section.rows.last().map(|laid| laid.id),
            Some(agents[8].row.id)
        );
        assert_eq!(section.hidden, 4);
        let back = lair_explorer_with_agents(
            lair_explorer_layout(content, 640, &rows, None, 0).unwrap(),
            &rows,
            (None, 0),
            &agents,
            Some(agents[2].row.id),
            4,
        )
        .agents
        .unwrap();
        assert_eq!(back.visible_start, 2);
    }

    /// A tree scrolled to its bottom stays there when the selection moves
    /// into the section: it scrolls from the start it was asked for, not
    /// from that start clamped to the taller list before the split.
    #[test]
    fn tree_keeps_its_start_while_an_agents_row_is_selected() {
        let rows = lair_rows(30);
        let agents = agent_rows(4);
        let content = Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 600,
        };
        let split = agents_layout(content, &rows, &agents, None);
        let bottom = rows.len() - split.visible_capacity;
        let plain = lair_explorer_layout(content, 640, &rows, None, bottom).unwrap();
        assert!(plain.visible_start < bottom, "the plain list clamps it");
        let layout = lair_explorer_with_agents(
            plain,
            &rows,
            (None, bottom),
            &agents,
            Some(agents[3].row.id),
            0,
        );
        assert_eq!(layout.visible_start, bottom);
        assert_eq!(layout.rows.last().map(|laid| laid.id), Some(rows[29].id));
        assert_eq!(
            layout.row_rect(agents[3].row.id, true),
            layout
                .agents
                .as_ref()
                .unwrap()
                .rows
                .last()
                .map(|laid| laid.rect)
        );
        assert_eq!(layout.row_rect(agents[3].row.id, false), None);
        assert_eq!(
            layout.row_rect(rows[29].id, false),
            layout.rows.last().map(|laid| laid.rect)
        );
    }

    /// An agents row hit-tests to its Splint, apart from the tree: neither
    /// the label line nor a tree row is an agents row, and no agents row is
    /// a tree row or the footer.
    #[test]
    fn agents_rows_hit_test_apart_from_the_tree_and_the_label() {
        let rows = lair_rows(4);
        let agents = agent_rows(3);
        let content = Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 600,
        };
        let layout = agents_layout(content, &rows, &agents, None);
        let section = layout.agents.as_ref().unwrap();
        for laid in &section.rows {
            for point in [
                (0.0, f64::from(laid.rect.y)),
                (
                    f64::from(laid.rect.width) - 0.5,
                    f64::from(laid.rect.y + laid.rect.height) - 0.5,
                ),
            ] {
                assert_eq!(lair_explorer_agent_hit_test(&layout, point), Some(laid.id));
                assert_eq!(lair_explorer_hit_test(&layout, point), None);
                assert!(!lair_explorer_footer_hit_test(&layout, point));
                assert_eq!(lair_explorer_action_hit_test(&layout, point), None);
            }
        }
        let label = (20.0, f64::from(section.label.y + 2));
        assert_eq!(lair_explorer_agent_hit_test(&layout, label), None);
        assert_eq!(lair_explorer_hit_test(&layout, label), None);
        assert!(lair_explorer_agents_label_hit_test(&layout, label));
        let label_end = (
            f64::from(layout.panel.width) - 0.5,
            f64::from(section.label.y + section.label.height) - 0.5,
        );
        assert!(lair_explorer_agents_label_hit_test(&layout, label_end));
        for laid in &section.rows {
            let point = (20.0, f64::from(laid.rect.y));
            assert!(!lair_explorer_agents_label_hit_test(&layout, point));
        }
        for laid in &layout.rows {
            let point = (20.0, f64::from(laid.rect.y + 2));
            assert_eq!(lair_explorer_agent_hit_test(&layout, point), None);
            assert_eq!(lair_explorer_hit_test(&layout, point), Some(laid.id));
            assert!(!lair_explorer_agents_label_hit_test(&layout, point));
        }
        let past = (
            f64::from(layout.panel.width),
            f64::from(section.rows[0].rect.y + 2),
        );
        assert_eq!(lair_explorer_agent_hit_test(&layout, past), None);
        let plain = lair_explorer_layout(content, 640, &rows, None, 0).unwrap();
        assert_eq!(lair_explorer_agent_hit_test(&plain, (20.0, 300.0)), None);
        assert!(!lair_explorer_agents_label_hit_test(&plain, label));
    }

    /// A row's name and location: both with two cells between when they
    /// fit, else the whole name and the location ellipsized into at least
    /// six cells, else the name alone, ellipsized.
    #[test]
    fn agent_rows_fit_the_name_first_then_the_location() {
        let location = "work / editor";
        assert_eq!(
            agent_row_text("shell", location, 40),
            ("shell".to_owned(), Some(location.to_owned()))
        );
        assert_eq!(
            agent_row_text("shell", location, 20),
            ("shell".to_owned(), Some(location.to_owned()))
        );
        assert_eq!(
            agent_row_text("shell", location, 16),
            ("shell".to_owned(), Some("work / e…".to_owned()))
        );
        // A cut never leaves the ` / ` or a space before the `…`.
        assert_eq!(
            agent_row_text("shell", location, 15),
            ("shell".to_owned(), Some("work…".to_owned()))
        );
        assert_eq!(
            agent_row_text("shell", "my work / editor", 16),
            ("shell".to_owned(), Some("my work…".to_owned()))
        );
        assert_eq!(
            agent_row_text("shell", "/ / / / editor", 15),
            ("shell".to_owned(), None)
        );
        assert_eq!(
            agent_row_text("shell", location, 12),
            ("shell".to_owned(), None)
        );
        let (name, shown) = agent_row_text("a-very-long-agent-name", location, 10);
        assert_eq!(text_cells(&name), 10);
        assert!(name.ends_with('…'));
        assert_eq!(shown, None);
        // A location shorter than six cells shows whenever it fits whole.
        assert_eq!(
            agent_row_text("shell", "a / b", 12),
            ("shell".to_owned(), Some("a / b".to_owned()))
        );
    }

    /// The spinner keeps moving for a working Splint in the agents section's
    /// view, even when its tree row is out of view or collapsed.
    #[test]
    fn working_agents_rows_in_view_keep_the_spinner_moving() {
        let rows = lair_rows(2);
        let mut agents = agent_rows(7);
        let content = Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 600,
        };
        let layout = agents_layout(content, &rows, &agents, None);
        assert!(!lair_explorer_shows_working(&layout, &rows, &agents));
        agents[0].row.agent_working = true;
        assert!(lair_explorer_shows_working(&layout, &rows, &agents));
        agents[0].row.agent_working = false;
        agents[6].row.agent_working = true;
        assert!(
            !lair_explorer_shows_working(&layout, &rows, &agents),
            "out of the section's view"
        );
    }

    /// Everything the section paints stays between its rule and the footer
    /// and left of the divider; the rule spans the panel, the label and each
    /// row paint, `+k more` appears only while rows are out of view, and the
    /// selected row is highlighted with the tree's accent bar.
    #[test]
    fn agents_section_paints_inside_its_rows() {
        use super::super::picker::session_picker_palette;
        use crate::config::ResolvedTheme;

        let context = RenderContext::new(u16::MAX);
        let rows = lair_rows(2);
        let palette = session_picker_palette(ResolvedTheme::default());
        let surface = Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 600,
        };
        for scale in [120, 150, 240] {
            let canvas_width = surface.width * scale / 120 + 1;
            let canvas_height = surface.height * scale / 120 + 1;
            let draw = |layout: &LairExplorerLayout,
                        agents: &[LairExplorerAgentRow],
                        selected: Option<NavigationNodeId>| {
                let mut canvas = vec![0; (canvas_width * canvas_height * 4) as usize];
                paint_lair_explorer(
                    &mut SessionPickerTextCache::default(),
                    &context,
                    &mut canvas,
                    canvas_width,
                    canvas_height,
                    scale,
                    1,
                    layout,
                    palette,
                    &rows,
                    None,
                    (agents, selected),
                    "",
                    None,
                    true,
                    LairExplorerFilter::All,
                    false,
                    ExplorerCurrentMarker::default(),
                )
                .unwrap();
                canvas
            };
            for count in [3, 7] {
                let agents = agent_rows(count);
                let layout = agents_layout(surface, &rows, &agents, None);
                let section = layout.agents.clone().unwrap();
                let plain = LairExplorerLayout {
                    agents: None,
                    ..layout.clone()
                };
                let with = draw(&layout, &agents, None);
                let bare = draw(&plain, &agents, None);
                let label = buffer_rect(section.label, scale);
                let footer = buffer_rect(layout.footer, scale);
                let panel = buffer_rect(layout.panel, scale);
                let right = panel.x + panel.width - scale.div_ceil(120);
                let mut lines = HashSet::new();
                for (index, (a, b)) in with.chunks_exact(4).zip(bare.chunks_exact(4)).enumerate() {
                    if a == b {
                        continue;
                    }
                    let x = u32::try_from(index).unwrap() % canvas_width;
                    let y = u32::try_from(index).unwrap() / canvas_width;
                    assert!(
                        (label.y..footer.y).contains(&y) && x < right,
                        "scale {scale}: section ink at {x},{y}"
                    );
                    lines.insert((y - label.y) * 120 / (ROW_HEIGHT * scale));
                }
                let shown = u32::try_from(section.rows.len()).unwrap();
                assert_eq!(lines.len(), (shown + 1) as usize, "scale {scale}");
                // `+k more` takes the label line's right half.
                let more = |canvas: &[u8]| {
                    (label.y + scale.div_ceil(120)..label.y + label.height).any(|y| {
                        (panel.x + panel.width / 2..right).any(|x| {
                            let at = ((y * canvas_width + x) * 4) as usize;
                            canvas[at..at + 4] != bare[at..at + 4]
                        })
                    })
                };
                assert_eq!(more(&with), section.hidden > 0, "scale {scale}, {count}");
                // The selected row is highlighted from its left edge, as in the
                // tree.
                let chosen = section.rows[0].id;
                let selected = draw(&layout, &agents, Some(chosen));
                let row = buffer_rect(section.rows[0].rect, scale);
                let at = (((row.y + row.height / 2) * canvas_width + row.x) * 4) as usize;
                assert_ne!(with[at..at + 4], selected[at..at + 4]);
            }
        }
    }

    /// The metrics of a two-line Explorer whose chrome font is unmeasured.
    const TWO_LINES: LairExplorerMetrics = UNMEASURED.with_two_line_rows(true);

    /// The expanded layout of `content` with two-line rows.
    fn two_line_layout(
        content: Rect,
        rows: &[LairExplorerRow],
        selected: Option<NavigationNodeId>,
    ) -> LairExplorerLayout {
        lair_explorer_layout_with_header(content, HEADER_HEIGHT, 640, TWO_LINES, rows, selected, 0)
            .unwrap()
    }

    /// Splint rows that each show `detail` on their second line.
    fn splint_rows(count: usize, detail: Option<&LairExplorerDetail>) -> Vec<LairExplorerRow> {
        let rows: Vec<_> = agent_rows(count)
            .into_iter()
            .map(|agent| LairExplorerRow {
                level: 3,
                label: "shell".into(),
                ..agent.row
            })
            .collect();
        redetailed(&rows, detail)
    }

    /// `rows` with `detail` on every second line.
    fn redetailed(
        rows: &[LairExplorerRow],
        detail: Option<&LairExplorerDetail>,
    ) -> Vec<LairExplorerRow> {
        rows.iter()
            .map(|row| LairExplorerRow {
                detail: detail.cloned(),
                ..row.clone()
            })
            .collect()
    }

    /// `rows` as agents rows in "work / editor".
    fn as_agents(rows: Vec<LairExplorerRow>) -> Vec<LairExplorerAgentRow> {
        rows.into_iter()
            .map(|row| LairExplorerAgentRow {
                row,
                location: "work / editor".into(),
            })
            .collect()
    }

    /// With two-line rows every row of the expanded tree is two lines tall,
    /// back to back from the list's top, as many as fit whole above the
    /// footer, and the selection stays in view. The rail keeps one line.
    #[test]
    fn two_line_rows_are_twice_as_tall_and_the_rail_keeps_one_line() {
        let rows = lair_rows(40);
        for height in [180, 400, 600, 901] {
            let content = Rect {
                x: 0,
                y: 34,
                width: 1000,
                height,
            };
            let last = rows[39].id;
            let two = two_line_layout(content, &rows, Some(last));
            assert_eq!(two.row_height, ROW_HEIGHT * 2);
            let list_top = content.y + HEADER_HEIGHT + HEADER_SPACER;
            let list_height = two.footer.y - list_top;
            assert_eq!(
                two.visible_capacity,
                (list_height / (ROW_HEIGHT * 2)) as usize
            );
            assert!(two.visible_capacity >= 1, "{height}");
            assert_eq!(two.rows.len(), two.visible_capacity);
            for (index, laid) in two.rows.iter().enumerate() {
                let index = u32::try_from(index).unwrap();
                assert_eq!(laid.rect.y, list_top + ROW_HEIGHT * 2 * index);
                assert_eq!(laid.rect.height, ROW_HEIGHT * 2);
                assert!(laid.rect.y + laid.rect.height <= two.footer.y);
            }
            assert_eq!(two.rows.last().map(|laid| laid.id), Some(last));
            for collapsed in [false, true] {
                let shown = |metrics| {
                    lair_explorer_shown_layout(
                        content,
                        None,
                        640,
                        metrics,
                        &rows,
                        Some(last),
                        0,
                        collapsed,
                    )
                    .unwrap()
                };
                if collapsed {
                    assert_eq!(shown(TWO_LINES), shown(UNMEASURED), "the rail");
                } else {
                    assert_eq!(shown(TWO_LINES), two);
                }
            }
        }
    }

    /// A list with room for a row of one line but not of two shows rows of
    /// one line, with the selection in view, rather than none; a list with
    /// room for a row of two shows rows of two, and one with no room for a
    /// line shows none.
    #[test]
    fn short_lists_fall_back_to_one_line_rows() {
        let rows = lair_rows(5);
        let last = rows[4].id;
        let mut seen = [false; 3];
        for height in 1..=200 {
            let content = Rect {
                x: 0,
                y: 34,
                width: 1000,
                height,
            };
            let Some(two) = lair_explorer_layout_with_header(
                content,
                HEADER_HEIGHT,
                640,
                TWO_LINES,
                &rows,
                Some(last),
                0,
            ) else {
                continue;
            };
            let list_top = content.y + HEADER_HEIGHT + HEADER_SPACER;
            let list_height = two.footer.y.saturating_sub(list_top);
            let expected = if list_height < ROW_HEIGHT {
                seen[0] = true;
                0
            } else if list_height < ROW_HEIGHT * 2 {
                seen[1] = true;
                assert_eq!(two.row_height, ROW_HEIGHT, "list {list_height}");
                1
            } else {
                seen[2] = true;
                assert_eq!(two.row_height, ROW_HEIGHT * 2, "list {list_height}");
                (list_height / (ROW_HEIGHT * 2)) as usize
            };
            assert_eq!(two.visible_capacity, expected, "list {list_height}");
            assert_eq!(two.rows.len(), expected, "list {list_height}");
            for laid in &two.rows {
                assert_eq!(laid.rect.height, two.row_height);
                assert!(laid.rect.y + laid.rect.height <= two.footer.y);
            }
            if expected > 0 {
                assert_eq!(two.rows.last().map(|laid| laid.id), Some(last));
            }
        }
        assert_eq!(seen, [true; 3], "every kind of list is exercised");
    }

    /// A press anywhere on either line of a two-line row hits that row,
    /// and its disclosure slot spans both lines.
    #[test]
    fn two_line_rows_hit_test_on_both_lines() {
        let rows = lair_rows(6);
        let content = Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 600,
        };
        let layout = two_line_layout(content, &rows, None);
        for laid in &layout.rows {
            let top = f64::from(laid.rect.y);
            let bottom = f64::from(laid.rect.y + laid.rect.height);
            for y in [
                top,
                top + f64::from(ROW_HEIGHT) - 0.5,
                top + f64::from(ROW_HEIGHT),
                bottom - 0.5,
            ] {
                assert_eq!(lair_explorer_hit_test(&layout, (20.0, y)), Some(laid.id));
                assert_eq!(
                    lair_explorer_disclosure_hit_test(&layout, &rows, (2.0, y)),
                    Some(laid.id)
                );
            }
            assert_ne!(
                lair_explorer_hit_test(&layout, (20.0, bottom)),
                Some(laid.id)
            );
        }
    }

    /// The agents section counts lines: its label is one line and each of
    /// its rows two, right above the footer, with the tree above it keeping
    /// at least one two-line row.
    #[test]
    fn agents_section_counts_two_line_rows_in_lines() {
        for lines in 0..5 {
            assert_eq!(agents_section_rows(lines, 4, 2), None, "{lines}");
        }
        for (lines, count, shown) in [(5, 4, 1), (12, 4, 2), (20, 4, 3), (30, 9, 5), (30, 2, 2)] {
            assert_eq!(
                agents_section_rows(lines, count, 2),
                Some(shown),
                "{lines} lines, {count} reporting"
            );
        }
        for lines in 5..90 {
            for count in 1..12 {
                let shown = agents_section_rows(lines, count, 2).unwrap();
                assert!((1..=AGENTS_MAXIMUM_ROWS).contains(&shown));
                assert!(1 + shown * 2 + 2 <= lines, "the label and one tree row fit");
            }
        }
        let rows = lair_rows(30);
        let agents = agent_rows(7);
        for height in [240, 400, 600, 900] {
            let content = Rect {
                x: 0,
                y: 34,
                width: 1000,
                height,
            };
            let plain = two_line_layout(content, &rows, None);
            let layout =
                lair_explorer_with_agents(plain.clone(), &rows, (None, 0), &agents, None, 0);
            let section = layout.agents.as_ref().expect("a section");
            let list_top = content.y + HEADER_HEIGHT + HEADER_SPACER;
            let lines = (plain.footer.y - list_top) / ROW_HEIGHT;
            let shown = agents_section_rows(lines, agents.len(), 2).unwrap();
            assert_eq!(section.rows.len(), shown as usize, "height {height}");
            assert_eq!(section.label.height, ROW_HEIGHT);
            assert_eq!(
                section.label.y + ROW_HEIGHT + ROW_HEIGHT * 2 * shown,
                layout.footer.y,
                "height {height}"
            );
            for (index, laid) in section.rows.iter().enumerate() {
                let index = u32::try_from(index).unwrap();
                assert_eq!(
                    laid.rect.y,
                    section.label.y + ROW_HEIGHT + ROW_HEIGHT * 2 * index
                );
                assert_eq!(laid.rect.height, ROW_HEIGHT * 2);
                let second = f64::from(laid.rect.y + ROW_HEIGHT + 2);
                assert_eq!(
                    lair_explorer_agent_hit_test(&layout, (20.0, second)),
                    Some(laid.id)
                );
            }
            assert!(layout.visible_capacity >= 1);
            for laid in &layout.rows {
                assert_eq!(laid.rect.height, ROW_HEIGHT * 2);
                assert!(laid.rect.y + laid.rect.height <= section.label.y);
            }
        }
    }

    /// A row's two lines tile its buffer rect at every scale; a row with no
    /// second line, or one line tall, keeps its whole rect.
    #[test]
    fn row_text_lines_tile_the_row() {
        let row = Rect {
            x: 0,
            y: 33,
            width: 288,
            height: ROW_HEIGHT * 2,
        };
        for scale in [120, 150, 180, 240] {
            let whole = buffer_rect(row, scale);
            assert_eq!(row_text_lines(row, false, scale), (whole, None));
            let (first, second) = row_text_lines(row, true, scale);
            let second = second.unwrap();
            assert_eq!(first.y, whole.y);
            assert_eq!(first.y + first.height, second.y);
            assert_eq!(second.y + second.height, whole.y + whole.height);
            assert_eq!(
                first.height,
                buffer_rect(
                    Rect {
                        height: ROW_HEIGHT,
                        ..row
                    },
                    scale
                )
                .height
            );
            let one = Rect {
                height: ROW_HEIGHT,
                ..row
            };
            assert_eq!(
                row_text_lines(one, true, scale),
                (buffer_rect(one, scale), None)
            );
        }
    }

    /// A long working directory keeps its root and its last whole
    /// components, `~/…/splinterm`; a last component too long is cut at its
    /// start; a short line keeps its last cells after `…`. Nothing is wider
    /// than the cells it has.
    #[test]
    fn long_directories_keep_their_root_and_tail() {
        let path = "~/Projects/splinterm-worktrees/explorer-two-line-rows";
        assert_eq!(fit_directory(path, 80), path);
        assert_eq!(fit_directory(path, 30), "~/…/explorer-two-line-rows");
        assert_eq!(fit_directory(path, 20), "~/…rer-two-line-rows");
        assert_eq!(
            fit_directory("/usr/local/share/splinterm", 18),
            "/…/share/splinterm"
        );
        assert_eq!(fit_directory("/usr/local/share/splinterm", 3), "/…m");
        assert_eq!(fit_directory(path, 3), "…ws");
        assert_eq!(fit_directory(path, 2), "…s");
        assert_eq!(fit_directory(path, 1), "…");
        assert_eq!(fit_directory(path, 0), "");
        assert_eq!(fit_directory("relative/dir/name", 8), "…ir/name");
        // Wide characters count two cells; the cut never splits one.
        let wide = "~/文書/プロジェクト/端末";
        for cells in 0..=text_cells(wide) {
            let fitted = fit_directory(wide, cells);
            assert!(text_cells(&fitted) <= cells, "{cells}: {fitted}");
        }
        assert_eq!(fit_directory(wide, 8), "~/…/端末");
        for cells in 0..=60 {
            let fitted = fit_directory(path, cells);
            assert!(text_cells(&fitted) <= cells, "{cells}: {fitted}");
            if cells > 1 && cells < text_cells(path) {
                assert!(fitted.contains('…'), "{cells}: {fitted}");
                assert!(fitted.ends_with('s'), "{cells}: {fitted}");
            }
        }
        // An agent's detail keeps its first words instead.
        let detail = LairExplorerDetail::Agent("running the integration tests".into());
        assert_eq!(row_detail_text(&detail, 18), "running the…");
        let directory = LairExplorerDetail::Directory(path.into());
        assert_eq!(
            row_detail_text(&directory, 30),
            "~/…/explorer-two-line-rows"
        );
    }

    /// The second line paints only inside its row's lower line, between the
    /// label's start and the divider, at every scale, in the tree and in
    /// the agents section; the selection's fill and accent bar cover both
    /// lines; and a row without a second line keeps its one line.
    #[test]
    fn second_lines_paint_inside_their_rows() {
        use super::super::picker::session_picker_palette;
        use crate::config::ResolvedTheme;

        let context = RenderContext::new(u16::MAX);
        let palette = session_picker_palette(ResolvedTheme::default());
        let detail = Some(LairExplorerDetail::Directory(
            "~/Projects/splinterm-worktrees/explorer-two-line-rows".into(),
        ));
        let surface = Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 600,
        };
        for scale in [120, 150, 180, 240] {
            let canvas_width = surface.width * scale / 120 + 1;
            let canvas_height = surface.height * scale / 120 + 1;
            let with_rows = splint_rows(3, detail.as_ref());
            // An empty second line keeps the two-line placement and paints
            // nothing.
            let blank = LairExplorerDetail::Directory(String::new());
            let bare_rows = redetailed(&with_rows, Some(&blank));
            let agent_base = splint_rows(2, detail.as_ref());
            let with_agents = as_agents(agent_base.clone());
            let bare_agents = as_agents(redetailed(&agent_base, Some(&blank)));
            let layout = lair_explorer_with_agents(
                two_line_layout(surface, &with_rows, None),
                &with_rows,
                (None, 0),
                &with_agents,
                None,
                0,
            );
            let section = layout.agents.clone().unwrap();
            assert_eq!(section.rows.len(), 2, "scale {scale}");
            let draw = |rows: &[LairExplorerRow],
                        agents: &[LairExplorerAgentRow],
                        selected: Option<NavigationNodeId>| {
                let mut canvas = vec![0; (canvas_width * canvas_height * 4) as usize];
                paint_lair_explorer(
                    &mut SessionPickerTextCache::default(),
                    &context,
                    &mut canvas,
                    canvas_width,
                    canvas_height,
                    scale,
                    1,
                    &layout,
                    palette,
                    rows,
                    selected,
                    (agents, None),
                    "",
                    None,
                    true,
                    LairExplorerFilter::All,
                    false,
                    ExplorerCurrentMarker::default(),
                )
                .unwrap();
                canvas
            };
            let with = draw(&with_rows, &with_agents, None);
            let bare = draw(&bare_rows, &bare_agents, None);
            let bounds = second_line_bounds(&context, &layout, (&with_rows, &with_agents), scale);
            assert_eq!(bounds.len(), 5, "scale {scale}");
            let mut inked = vec![false; bounds.len()];
            for (index, (a, b)) in with.chunks_exact(4).zip(bare.chunks_exact(4)).enumerate() {
                if a == b {
                    continue;
                }
                let x = u32::try_from(index).unwrap() % canvas_width;
                let y = u32::try_from(index).unwrap() / canvas_width;
                let line = bounds
                    .iter()
                    .position(|(line, ..)| (line.y..line.y + line.height).contains(&y));
                let Some(line) = line else {
                    panic!("scale {scale}: second-line ink at {x},{y} off every second line");
                };
                let (_, left, right) = bounds[line];
                assert!(
                    (left..right).contains(&x),
                    "scale {scale}: row {line}'s second-line ink at {x},{y} outside {left}..{right}"
                );
                inked[line] = true;
            }
            assert!(inked.iter().all(|&inked| inked), "scale {scale}: {inked:?}");
            // The selection covers both lines.
            let chosen = &layout.rows[1];
            let selected = draw(&with_rows, &with_agents, Some(chosen.id));
            let rect = buffer_rect(chosen.rect, scale);
            assert_selection_covers_both_lines((&with, &selected), canvas_width, rect, bounds[1].2);
        }
    }

    /// Asserts that the selection changes `rect` on both lines: its accent
    /// bar at the row's left edge, and its fill on the second line in the
    /// middle and in the row's right inset, past the text ending at
    /// `text_end`.
    fn assert_selection_covers_both_lines(
        (plain, selected): (&[u8], &[u8]),
        canvas_width: u32,
        rect: Rect,
        text_end: u32,
    ) {
        let bottom = rect.y + rect.height - 2;
        assert!(text_end + 1 < rect.x + rect.width, "{rect:?}");
        for (x, y) in [
            (rect.x, rect.y + 1),
            (rect.x, bottom),
            (rect.x + rect.width / 2, bottom),
            (text_end + 1, bottom),
        ] {
            let at = ((y * canvas_width + x) * 4) as usize;
            assert_ne!(
                plain[at..at + 4],
                selected[at..at + 4],
                "{rect:?} at {x},{y}"
            );
        }
    }

    /// Each painted row's second line in buffer px, with the left and right
    /// edges its detail paints between: in the tree from past the name's
    /// dot and disclosure slot to the row's right inset, and in the agents
    /// section from past the glyph column and its gap to the divider's
    /// inset.
    fn second_line_bounds(
        context: &RenderContext,
        layout: &LairExplorerLayout,
        (rows, agents): (&[LairExplorerRow], &[LairExplorerAgentRow]),
        scale: u32,
    ) -> Vec<(Rect, u32, u32)> {
        let cell = chrome_cell_width(context, scale);
        let inset = LEVEL_INDENT.saturating_mul(scale).div_ceil(120);
        let columns = ExplorerColumns::of(rows);
        let mut bounds = Vec::new();
        for (laid, row) in layout
            .rows
            .iter()
            .zip(rows.iter().skip(layout.visible_start))
        {
            let (first, second) = row_text_lines(laid.rect, true, scale);
            let (source, first_mark) = row_columns_source(row, columns, chrome_icons_available());
            let (name, column_clip) =
                row_text_clips(first, row.level, inset, cell, source.cells(), first_mark);
            let left = (name.x + cell * row_name_prefix_cells(chrome_icons_available()))
                .min(name.x + name.width);
            bounds.push((second.unwrap(), left, column_clip.x + column_clip.width));
        }
        if let Some(section) = &layout.agents {
            let panel = buffer_rect(layout.panel, scale);
            let divider = panel.x + panel.width - scale.div_ceil(120).max(1);
            for (laid, agent) in section
                .rows
                .iter()
                .zip(agents.iter().skip(section.visible_start))
            {
                let (first, second) = row_text_lines(laid.rect, true, scale);
                let line = agents_text_line(first, inset, divider);
                let glyphs = agent
                    .row
                    .agent_column
                    .as_ref()
                    .map_or(0, |column| cell * u32::try_from(column.cells()).unwrap());
                let left = line.x + (glyphs + cell).min(line.width);
                bounds.push((second.unwrap(), left, line.x + line.width));
            }
        }
        bounds
    }

    /// With two-line rows a Lair, a Dojo, and a Splint with no second line
    /// keep one line, centred in their two-line row.
    #[test]
    fn rows_without_a_second_line_centre_their_one_line() {
        use super::super::picker::session_picker_palette;
        use crate::config::ResolvedTheme;

        let context = RenderContext::new(u16::MAX);
        let lair_id = LairId::new();
        let dojo_id = splinterm_core::DojoId::new();
        let lair = row(NavigationNodeId::Lair(lair_id));
        let dojo = LairExplorerRow {
            id: NavigationNodeId::Dojo { lair_id, dojo_id },
            kind: LairExplorerRowKind::Dojo,
            label: "editor".into(),
            level: 2,
            ..lair.clone()
        };
        let splint = LairExplorerRow {
            id: NavigationNodeId::Splint {
                lair_id,
                dojo_id,
                splint_id: splinterm_core::SplintId::new(),
            },
            kind: LairExplorerRowKind::Splint,
            label: "shell".into(),
            level: 3,
            expanded: None,
            ..lair.clone()
        };
        let surface = Rect {
            x: 0,
            y: 0,
            width: 1000,
            height: 400,
        };
        for scale in [120, 150, 180, 240] {
            let canvas_width = surface.width * scale / 120 + 1;
            let canvas_height = surface.height * scale / 120 + 1;
            // The top and bottom of a row's ink in its row of a layout of
            // `metrics`, from the row's top, and the row's height.
            let ink = |painted: &LairExplorerRow, metrics: LairExplorerMetrics| {
                let rows = [painted.clone()];
                let layout = lair_explorer_layout_with_header(
                    surface,
                    HEADER_HEIGHT,
                    640,
                    metrics,
                    &rows,
                    None,
                    0,
                )
                .unwrap();
                let draw = |rows: &[LairExplorerRow]| {
                    let mut canvas = vec![0; (canvas_width * canvas_height * 4) as usize];
                    paint(
                        &context,
                        &mut canvas,
                        canvas_width,
                        canvas_height,
                        scale,
                        &layout,
                        session_picker_palette(ResolvedTheme::default()),
                        rows,
                        None,
                        false,
                        LairExplorerFilter::All,
                    );
                    canvas
                };
                let (with, without) = (draw(&rows), draw(&[]));
                let rect = buffer_rect(layout.rows[0].rect, scale);
                let ys: Vec<u32> = with
                    .chunks_exact(4)
                    .zip(without.chunks_exact(4))
                    .enumerate()
                    .filter(|(_, (a, b))| a != b)
                    .map(|(index, _)| u32::try_from(index).unwrap())
                    .map(|index| (index % canvas_width, index / canvas_width))
                    .filter(|&(x, y)| {
                        (rect.x..rect.x + rect.width).contains(&x)
                            && (rect.y..rect.y + rect.height).contains(&y)
                    })
                    .map(|(_, y)| y - rect.y)
                    .collect();
                let top = *ys.iter().min().expect("the row paints");
                let bottom = *ys.iter().max().unwrap();
                (top, bottom, rect.height)
            };
            for painted in [&lair, &dojo, &splint] {
                let (top, bottom, one) = ink(painted, UNMEASURED);
                let (top_two, bottom_two, two) = ink(painted, TWO_LINES);
                let shift = (two - one) / 2;
                let what = format!("scale {scale}, {:?}", painted.kind);
                assert!(top_two.abs_diff(top + shift) <= 1, "{what}: {top_two}");
                assert!(
                    bottom_two.abs_diff(bottom + shift) <= 1,
                    "{what}: {bottom_two}"
                );
            }
        }
    }
}
