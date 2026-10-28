//! Trusted command-palette layout and CPU composition.

use anyhow::Result;
use unicode_width::UnicodeWidthChar;

use crate::{
    frontend::{
        AgentAccessUi, BindingHelpUi, BuiltInCommandId, CommandPaletteUi, command_descriptor,
        shortcut_chips,
    },
    geometry::Rect,
    keymap::ResolvedKeymap,
};

use super::{
    super::{ChromeTextStyle, RenderContext},
    chrome_box::{BOX_CLEARANCE, BOX_EDGE, ChromeBox, ChromePainter, opaque, paint_scrim},
    chrome_rows::{
        GROUP_HEIGHT, LIST_MAXIMUM_ROWS, ListRow, ROW_HEIGHT, ROW_PADDING, RowState,
        paint_group_header, paint_row, row_columns, starts_group, title_match, visible_window,
        window_height,
    },
    picker::{PickerTextAlignment, SessionPickerPalette, SessionPickerTextCache, editable_text},
};

/// The palette's input line, inside the frame.
const INPUT_HEIGHT: u32 = 28;
/// Logical px above and below the rule under the input line.
const RULE_GAP: u32 = 4;
/// Logical px from the inside of the frame to the input line's `›`.
const INPUT_INSET: u32 = 10;
/// Everything above the list: the frame and its clearance, the input line
/// and the rule under it.
const HEADER_HEIGHT: u32 = BOX_EDGE + BOX_CLEARANCE + INPUT_HEIGHT + RULE_GAP * 2 + 1;
/// Everything below the list: the clearance and the frame.
const FOOTER_HEIGHT: u32 = BOX_CLEARANCE + BOX_EDGE;
/// The detail line under the key bindings' and Agent access's lists: what
/// the selected row is, or a notice.
const DETAIL_HEIGHT: u32 = 28;
/// A list longer than its window keeps this height while it scrolls.
#[allow(clippy::cast_possible_truncation, reason = "ten rows")]
const LIST_HEIGHT: u32 = ROW_HEIGHT * LIST_MAXIMUM_ROWS as u32;
/// The command palette's and Agent access's width at most.
const COMMAND_PALETTE_WIDTH: u32 = 560;
/// The key bindings list's width at most: its rows lead with a chips
/// column.
const BINDING_HELP_WIDTH: u32 = 640;
const PANEL_HORIZONTAL_MARGIN: u32 = 24;
const PANEL_VERTICAL_MARGIN: u32 = 16;
/// Lists sit at this share of the content height from its top, and at
/// least [`PANEL_MINIMUM_TOP`] below it.
const PANEL_TOP_PERCENT: u32 = 15;
const PANEL_MINIMUM_TOP: u32 = 48;

fn preview_text_width(text: &str) -> usize {
    text.chars()
        .map(|character| character.width().unwrap_or(0).min(2))
        .sum()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommandPaletteRowLayout {
    pub(crate) command: BuiltInCommandId,
    /// The whole 28 px row, the inside of the frame wide.
    pub(crate) rect: Rect,
    /// The group header above the row, when the row starts a group, and
    /// its label.
    pub(crate) header: Option<(Rect, &'static str)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommandPaletteLayout {
    /// The panel fill; its frame is inset inside it.
    pub(crate) panel: Rect,
    /// The input line, inside the frame.
    pub(crate) input: Rect,
    /// The 1 px rule under the input line.
    pub(crate) rule: Rect,
    pub(crate) list: Rect,
    /// The detail line under the list, with a rule along its top: what the
    /// selected key binding or Agent access entry is.
    pub(crate) detail: Option<Rect>,
    pub(crate) rows: Vec<CommandPaletteRowLayout>,
    pub(crate) visible_start: usize,
}

/// Which list the palette's panel holds: the key bindings list is wider,
/// and key bindings and Agent access have a detail line under their rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PaletteShape {
    Commands,
    BindingHelp,
    AgentAccess,
}

impl PaletteShape {
    const fn maximum_width(self) -> u32 {
        match self {
            Self::BindingHelp => BINDING_HELP_WIDTH,
            Self::Commands | Self::AgentAccess => COMMAND_PALETTE_WIDTH,
        }
    }

    const fn detail_height(self) -> u32 {
        match self {
            Self::BindingHelp | Self::AgentAccess => DETAIL_HEIGHT,
            Self::Commands => 0,
        }
    }
}

/// The palette's panel in `content`: top-anchored at 15% of its height (at
/// least 48 px down), as wide as its shape allows but 24 px clear of each
/// side, moved up when its rows would not fit below.
///
/// `groups` holds each row's group, or is empty for a flat list: a header
/// goes above each row that starts a group. The list shows at most ten
/// rows; a list longer than its window keeps one height while it scrolls.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "one pure layout seam keeps responsive geometry and hit targets coherent"
)]
pub(crate) fn command_palette_layout(
    content: Rect,
    filtered: &[BuiltInCommandId],
    groups: &[&'static str],
    selected: usize,
    requested_start: usize,
    shape: PaletteShape,
) -> Option<CommandPaletteLayout> {
    if content.width == 0 || content.height == 0 {
        return None;
    }
    let horizontal_margin = PANEL_HORIZONTAL_MARGIN.min(content.width / 4);
    let vertical_margin = PANEL_VERTICAL_MARGIN.min(content.height / 4);
    let available_width = content
        .width
        .saturating_sub(horizontal_margin.saturating_mul(2));
    let available_height = content
        .height
        .saturating_sub(vertical_margin.saturating_mul(2));
    let chrome_height = HEADER_HEIGHT
        .saturating_add(FOOTER_HEIGHT)
        .saturating_add(shape.detail_height());
    if available_width == 0 || available_height < chrome_height {
        return None;
    }
    let panel_width = available_width.min(shape.maximum_width());
    let budget = available_height
        .saturating_sub(chrome_height)
        .min(LIST_HEIGHT);
    // A window too short for a header and its row shows the rows alone.
    let groups = if groups.len() == filtered.len() && budget >= ROW_HEIGHT + GROUP_HEIGHT {
        groups
    } else {
        &[]
    };
    let window = visible_window(filtered.len(), groups, selected, requested_start, budget);
    let whole =
        filtered.len() <= LIST_MAXIMUM_ROWS && window_height(groups, 0, filtered.len()) <= budget;
    // No partial row: when not even one fits, the list is empty.
    let list_height = if budget < ROW_HEIGHT {
        0
    } else if filtered.is_empty() {
        ROW_HEIGHT
    } else if whole {
        window_height(groups, 0, filtered.len())
    } else {
        budget
    };
    let panel_height = chrome_height.saturating_add(list_height);
    let x = content
        .x
        .saturating_add(content.width.saturating_sub(panel_width) / 2);
    let top = (content.height.saturating_mul(PANEL_TOP_PERCENT) / 100).max(PANEL_MINIMUM_TOP);
    let lowest = content
        .y
        .saturating_add(content.height)
        .saturating_sub(vertical_margin)
        .saturating_sub(panel_height);
    let y = content.y.saturating_add(top).min(lowest);
    let panel = Rect {
        x,
        y,
        width: panel_width,
        height: panel_height,
    };
    let inner_x = x.saturating_add(BOX_EDGE);
    let inner_width = panel_width.saturating_sub(BOX_EDGE.saturating_mul(2));
    let input = Rect {
        x: inner_x,
        y: y.saturating_add(BOX_EDGE).saturating_add(BOX_CLEARANCE),
        width: inner_width,
        height: INPUT_HEIGHT,
    };
    let rule = Rect {
        x: inner_x,
        y: input
            .y
            .saturating_add(input.height)
            .saturating_add(RULE_GAP),
        width: inner_width,
        height: 1,
    };
    let list = Rect {
        x: inner_x,
        y: y.saturating_add(HEADER_HEIGHT),
        width: inner_width,
        height: list_height,
    };
    let detail = (shape.detail_height() > 0).then(|| Rect {
        x: inner_x,
        y: list.y.saturating_add(list.height),
        width: inner_width,
        height: shape.detail_height(),
    });
    let mut row_y = list.y;
    let rows = window
        .clone()
        .map(|index| {
            let header = starts_group(groups, index).then(|| {
                let header = Rect {
                    x: list.x,
                    y: row_y,
                    width: list.width,
                    height: GROUP_HEIGHT,
                };
                row_y = row_y.saturating_add(GROUP_HEIGHT);
                (header, groups[index])
            });
            let rect = Rect {
                x: list.x,
                y: row_y,
                width: list.width,
                height: ROW_HEIGHT,
            };
            row_y = row_y.saturating_add(ROW_HEIGHT);
            CommandPaletteRowLayout {
                command: filtered[index],
                rect,
                header,
            }
        })
        .collect();
    Some(CommandPaletteLayout {
        panel,
        input,
        rule,
        list,
        detail,
        rows,
        visible_start: window.start,
    })
}

fn contains(rect: Rect, point: (f64, f64)) -> bool {
    point.0 >= f64::from(rect.x)
        && point.1 >= f64::from(rect.y)
        && point.0 < f64::from(rect.x.saturating_add(rect.width))
        && point.1 < f64::from(rect.y.saturating_add(rect.height))
}

#[must_use]
pub(crate) fn command_palette_hit_test(
    layout: &CommandPaletteLayout,
    point: (f64, f64),
) -> Option<BuiltInCommandId> {
    layout
        .rows
        .iter()
        .find(|row| contains(row.rect, point))
        .map(|row| row.command)
}

/// The list index of the row under `point`, for a list body that is not
/// the command catalog.
#[must_use]
pub(crate) fn command_palette_row_hit_test(
    layout: &CommandPaletteLayout,
    point: (f64, f64),
) -> Option<usize> {
    layout
        .rows
        .iter()
        .position(|row| contains(row.rect, point))
        .map(|offset| layout.visible_start.saturating_add(offset))
}

/// What the palette's list shows: its commands, the key bindings, or the
/// **Agent access** list (ADR 0014).
#[derive(Clone, Copy, Debug)]
pub(crate) enum PaletteBody<'a> {
    Commands,
    BindingHelp(&'a BindingHelpUi),
    AgentAccess(&'a AgentAccessUi),
}

impl<'a> PaletteBody<'a> {
    /// The title set into the frame.
    const fn title(self) -> &'static str {
        match self {
            Self::Commands => "commands",
            Self::BindingHelp(_) => "key bindings",
            Self::AgentAccess(_) => "agent access",
        }
    }

    /// The input line's text when nothing is typed.
    const fn placeholder(self) -> &'static str {
        match self {
            Self::Commands => "type a command",
            Self::BindingHelp(_) => "filter by key or name",
            Self::AgentAccess(_) => "search agent access",
        }
    }

    /// The keys the list answers, set into the bottom frame.
    const fn legend(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Commands => &[("↑↓", "move"), ("↵", "run"), ("esc", "close")],
            Self::BindingHelp(_) => &[
                ("↑↓", "move"),
                ("pgup pgdn", "page"),
                ("ctrl u", "clear"),
                ("esc", "close"),
            ],
            // The detail line says what Enter does for the selected entry.
            Self::AgentAccess(_) => &[("↑↓", "move"), ("esc", "close")],
        }
    }

    fn query<'s>(self, state: &'s CommandPaletteUi) -> &'s str
    where
        'a: 's,
    {
        match self {
            Self::Commands => state.query(),
            Self::BindingHelp(help) => help.query(),
            Self::AgentAccess(access) => access.query(),
        }
    }

    /// The frame's right label, most important first: for Agent access
    /// what waits and whether the read stopped short, then how many rows
    /// match of how many while a query filters, the total otherwise.
    fn count_parts(self, state: &CommandPaletteUi) -> Vec<String> {
        let (shown, total) = match self {
            Self::Commands => (state.filtered().len(), state.total()),
            Self::BindingHelp(help) => (help.rows().len(), help.total()),
            Self::AgentAccess(access) => (access.rows().len(), access.total()),
        };
        let mut parts = match self {
            Self::AgentAccess(access) => access.status_parts(),
            Self::Commands | Self::BindingHelp(_) => Vec::new(),
        };
        parts.push(match_count_label(shown, total, self.query(state)));
        parts
    }
}

/// `shown/total` while `query` filters; `total` otherwise.
pub(super) fn match_count_label(shown: usize, total: usize, query: &str) -> String {
    if query.is_empty() {
        total.to_string()
    } else {
        format!("{shown}/{total}")
    }
}

/// Where the input line's parts go, in buffer px: the `›`, the text, and
/// the block cursor after it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct InputLineLayout {
    pub(super) marker: Rect,
    pub(super) text_x: u32,
    /// The text's end, where the placeholder stops.
    pub(super) end: u32,
    pub(super) cell: (u32, u32),
}

impl InputLineLayout {
    pub(super) fn new(input: Rect, inset: u32, cell: (u32, u32)) -> Self {
        let inset = inset.min(input.width / 2);
        let marker = Rect {
            x: input.x.saturating_add(inset),
            width: cell.0.min(input.width.saturating_sub(inset)),
            ..input
        };
        Self {
            marker,
            // `›`, then one cell of space.
            text_x: marker.x.saturating_add(cell.0.saturating_mul(2)),
            end: input.x.saturating_add(input.width).saturating_sub(inset),
            cell,
        }
    }

    /// The typed text that fits with the cursor's cell after it, keeping
    /// its end, and the cursor; `None` when not even the cursor fits.
    pub(super) fn fitted(&self, input: Rect, query: &str) -> Option<(String, Rect)> {
        let (cell_width, cell_height) = self.cell;
        let room = self.end.saturating_sub(self.text_x) / cell_width.max(1);
        let fitted = editable_text("", query, usize::try_from(room).unwrap_or(usize::MAX));
        let shown = fitted.strip_suffix('_')?.to_owned();
        let cursor_x = self.text_x.saturating_add(
            u32::try_from(preview_text_width(&shown))
                .unwrap_or(u32::MAX)
                .saturating_mul(cell_width),
        );
        let cursor_height = cell_height.min(input.height);
        Some((
            shown,
            Rect {
                x: cursor_x,
                y: input
                    .y
                    .saturating_add(input.height.saturating_sub(cursor_height) / 2),
                width: cell_width,
                height: cursor_height,
            },
        ))
    }
}

/// The input line: the accent `›`, the query, and a block cursor (an
/// accent cell) where typing goes; with nothing typed, the placeholder in
/// secondary text after the cursor.
pub(super) fn paint_palette_input(
    painter: &mut ChromePainter<'_>,
    layout: &CommandPaletteLayout,
    palette: &SessionPickerPalette,
    query: &str,
    placeholder: &str,
) -> Result<()> {
    let input = painter.buffer_rect(layout.input);
    let cell = painter.cell(ChromeTextStyle::Regular)?;
    let line = InputLineLayout::new(input, painter.scaled(INPUT_INSET), cell);
    painter.text(
        "›",
        ChromeTextStyle::Regular,
        line.marker,
        PickerTextAlignment::Left,
        palette.accent_text,
    )?;
    let Some((shown, cursor)) = line.fitted(input, query) else {
        return Ok(());
    };
    painter.text(
        &shown,
        ChromeTextStyle::Regular,
        Rect {
            x: line.text_x,
            width: cursor.x.saturating_sub(line.text_x),
            ..input
        },
        PickerTextAlignment::Left,
        palette.primary,
    )?;
    painter.fill(cursor, opaque(palette.selected_rail));
    if query.is_empty() {
        let after = cursor.x.saturating_add(cursor.width);
        painter.text(
            placeholder,
            ChromeTextStyle::Regular,
            Rect {
                x: after,
                width: line.end.saturating_sub(after),
                ..input
            },
            PickerTextAlignment::Left,
            palette.secondary,
        )?;
    }
    Ok(())
}

/// The command palette, the key bindings list or the Agent access list in
/// one box: the title and match count in the frame, the input line and its
/// rule, the rows, and the keys in the bottom frame.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_command_palette(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    canvas: &mut [u8],
    width: u32,
    height: u32,
    content_rect: Rect,
    scale_120: u32,
    renderer_generation: u64,
    layout: &CommandPaletteLayout,
    palette: SessionPickerPalette,
    state: &CommandPaletteUi,
    keymap: &ResolvedKeymap,
    pressed: Option<BuiltInCommandId>,
    keyboard_focused: bool,
    body: PaletteBody<'_>,
) -> Result<()> {
    let mut painter = ChromePainter::new(
        cache,
        context,
        canvas,
        width,
        height,
        scale_120,
        renderer_generation,
    );
    let scrim = painter.buffer_rect(content_rect);
    paint_scrim(&mut painter, scrim, &palette);
    let cell = painter.box_cell()?;
    let count = body.count_parts(state);
    let count = count.iter().map(String::as_str).collect::<Vec<_>>();
    let chrome = ChromeBox {
        title: body.title(),
        right_label: &count,
        right_active: None,
        legend: body.legend(),
    }
    .layout(painter.buffer_rect(layout.panel), scale_120, cell);
    chrome.paint_surface(&mut painter, &palette);
    paint_palette_input(
        &mut painter,
        layout,
        &palette,
        body.query(state),
        body.placeholder(),
    )?;
    let rule = painter.buffer_rect(layout.rule);
    painter.fill(rule, opaque(palette.frame));
    paint_palette_list(&mut painter, layout, &palette, state, keymap, pressed, body)?;
    if let Some(detail) = layout.detail {
        let text = match body {
            PaletteBody::AgentAccess(access) => {
                Some(access.footer()).filter(|text| !text.is_empty())
            }
            PaletteBody::BindingHelp(help) => help
                .rows()
                .get(help.selected_index())
                .map(|row| row.detail.clone()),
            PaletteBody::Commands => None,
        };
        let detail = painter.buffer_rect(detail);
        paint_detail_line(&mut painter, &palette, detail, text.as_deref())?;
    }
    chrome.paint_frame(
        &mut painter,
        &palette,
        if keyboard_focused {
            palette.focused_frame
        } else {
            palette.frame
        },
    )?;
    painter.finish();
    Ok(())
}

/// The palette's group headers and rows, or the line that says none match.
fn paint_palette_list(
    painter: &mut ChromePainter<'_>,
    layout: &CommandPaletteLayout,
    palette: &SessionPickerPalette,
    state: &CommandPaletteUi,
    keymap: &ResolvedKeymap,
    pressed: Option<BuiltInCommandId>,
    body: PaletteBody<'_>,
) -> Result<()> {
    let empty_label = match body {
        PaletteBody::BindingHelp(help) if help.rows().is_empty() => {
            Some("No matching keybindings".to_owned())
        }
        PaletteBody::Commands if state.filtered().is_empty() => {
            Some("No matching commands".to_owned())
        }
        PaletteBody::AgentAccess(access) => access.empty_label(),
        _ => None,
    };
    if let Some(empty_label) = empty_label.as_deref().filter(|_| layout.list.height > 0) {
        let list = painter.buffer_rect(layout.list);
        let row = Rect {
            height: painter.scaled(ROW_HEIGHT).min(list.height),
            ..list
        };
        let clip = title_column(painter, row)?;
        painter.text(
            empty_label,
            ChromeTextStyle::Regular,
            clip,
            PickerTextAlignment::Left,
            palette.secondary,
        )?;
    }
    let query = body.query(state);
    for (offset, row) in layout.rows.iter().enumerate() {
        if let Some((header, label)) = row.header {
            let header = painter.buffer_rect(header);
            paint_group_header(painter, palette, header, label)?;
        }
        let index = layout.visible_start.saturating_add(offset);
        let rect = painter.buffer_rect(row.rect);
        match body {
            PaletteBody::Commands => {
                paint_command_row(painter, palette, rect, row.command, state, keymap, pressed)?;
            }
            PaletteBody::BindingHelp(help) => {
                let Some(binding) = help.rows().get(index) else {
                    continue;
                };
                let trailing = if binding.user {
                    "user"
                } else if query.is_empty() {
                    ""
                } else {
                    binding.group
                };
                paint_row(
                    painter,
                    palette,
                    rect,
                    &ListRow {
                        title: &binding.title,
                        matched: title_match(&binding.title, query),
                        marker: None,
                        chips: &binding.chips,
                        chip_column: true,
                        trailing,
                        trailing_accent: binding.user,
                        state: RowState {
                            selected: index == help.selected_index(),
                            ..RowState::default()
                        },
                    },
                )?;
            }
            PaletteBody::AgentAccess(access) => {
                let Some(entry) = access.rows().get(index) else {
                    continue;
                };
                paint_row(
                    painter,
                    palette,
                    rect,
                    &ListRow {
                        title: &entry.title,
                        matched: title_match(&entry.title, query),
                        marker: None,
                        chips: &[],
                        chip_column: false,
                        trailing: &entry.status,
                        trailing_accent: false,
                        state: RowState {
                            selected: index == access.selected_index(),
                            ..RowState::default()
                        },
                    },
                )?;
            }
        }
    }
    Ok(())
}

/// Where a list row's title goes in `row` (buffer px), for a line that
/// lines up with the titles.
pub(super) fn title_column(painter: &mut ChromePainter<'_>, row: Rect) -> Result<Rect> {
    let cell = painter.cell(ChromeTextStyle::Regular)?;
    Ok(row_columns(row, painter.scaled(ROW_PADDING), cell.0, 0, 0).title)
}

/// The detail line under a list in `rect` (buffer px): a rule along its
/// top and `text` in secondary text where the titles start.
fn paint_detail_line(
    painter: &mut ChromePainter<'_>,
    palette: &SessionPickerPalette,
    rect: Rect,
    text: Option<&str>,
) -> Result<()> {
    let rule = Rect {
        height: painter.border().min(rect.height),
        ..rect
    };
    painter.fill(rule, opaque(palette.frame));
    let Some(text) = text else {
        return Ok(());
    };
    let clip = title_column(painter, rect)?;
    painter.text(
        text,
        ChromeTextStyle::Regular,
        clip,
        PickerTextAlignment::Left,
        palette.secondary,
    )
}

/// One command row: its title with the query's characters marked, its
/// binding's chips at the end, or a live badge, or its group while a query
/// filters the list. Selected, hovered, pressed, disabled, and Terminate in
/// danger.
fn paint_command_row(
    painter: &mut ChromePainter<'_>,
    palette: &SessionPickerPalette,
    rect: Rect,
    command: BuiltInCommandId,
    state: &CommandPaletteUi,
    keymap: &ResolvedKeymap,
    pressed: Option<BuiltInCommandId>,
) -> Result<()> {
    let enabled = state.command_enabled(command);
    let descriptor = command_descriptor(command);
    let badge = state.badge(command);
    let chips = if badge.is_some() {
        Vec::new()
    } else {
        shortcut_chips(descriptor.shortcut(keymap))
    };
    let query = state.query();
    let trailing = match &badge {
        Some(badge) => badge.as_str(),
        None if chips.is_empty() && !query.is_empty() => descriptor.group().label(),
        None => "",
    };
    paint_row(
        painter,
        palette,
        rect,
        &ListRow {
            title: descriptor.title,
            matched: title_match(descriptor.title, query),
            marker: None,
            chips: &chips,
            chip_column: false,
            trailing,
            trailing_accent: badge.is_some(),
            state: RowState {
                selected: enabled && state.selected_command() == Some(command),
                hovered: enabled && state.hovered() == Some(command),
                pressed: enabled && pressed == Some(command),
                disabled: !enabled,
                danger: enabled && command.destructive(),
            },
        },
    )
}

#[cfg(test)]
mod tests {
    use super::{
        super::{
            super::{ChromeText, text::chrome_maximum_cell_height},
            chrome_box::BOX_LABEL_INSET,
            picker::picker_buffer_rect as buffer_rect,
        },
        *,
    };
    use crate::{
        config::ResolvedTheme,
        frontend::{CommandPaletteContext, CommandTabMoveAvailability, TabMenuActionId},
        renderer::session_picker_palette,
    };
    use splinterm_core::{DojoId, LairId, SplintId};

    fn palette_context() -> CommandPaletteContext {
        CommandPaletteContext {
            lair_id: LairId::new(),
            lair_retention: splinterm_core::LairRetention::Disposable,
            dojo_id: DojoId::new(),
            dojo_name: "test".to_owned(),
            pane_count: 1,
            splint_id: SplintId::new(),
            dojo_splints: vec![(SplintId::new(), 1)],
            other_dojo_ids: Vec::new(),
            previous_dojo_id: None,
            next_dojo_id: None,
            tab_move: CommandTabMoveAvailability::Neither,
            focus_left: None,
            focus_right: None,
            focus_up: None,
            focus_down: None,
            clipboard_image_available: false,
            viewport_detached: false,
            controller_active: false,
            forced_control_transfer: true,
            revoke_all_available: false,
            access_waiting: 0,
            pending_transfer_id: None,
            agent_status_incarnation: None,
        }
    }

    /// Paints `source` into a scratch canvas through the one chrome text
    /// painter, as the popups do.
    fn paint_label(
        cache: &mut SessionPickerTextCache,
        context: &RenderContext,
        source: &str,
        style: ChromeTextStyle,
        scale: u32,
        clip_width: u32,
    ) {
        let mut canvas = vec![0_u8; 4];
        let mut painter = ChromePainter::new(cache, context, &mut canvas, 1, 1, scale, 1);
        let clip = Rect {
            x: 0,
            y: 0,
            width: clip_width,
            height: 1,
        };
        painter
            .text(source, style, clip, PickerTextAlignment::Left, 0)
            .unwrap();
        painter.finish();
    }

    /// The width of `source` as chrome text.
    fn text_width(
        context: &RenderContext,
        source: &str,
        style: ChromeTextStyle,
        scale: u32,
    ) -> u32 {
        ChromeText::load_styled_with_context(source, scale, style, context)
            .unwrap()
            .pixel_width()
    }

    #[test]
    fn layout_is_bounded_selected_visible_and_hit_targets_are_half_open() {
        let commands = vec![
            BuiltInCommandId::NewDojo,
            BuiltInCommandId::SplitHorizontal,
            BuiltInCommandId::SplitVertical,
        ];
        for groups in [&[][..], &["dojo", "pane", "pane"][..]] {
            for content in [
                Rect {
                    x: 0,
                    y: 34,
                    width: 960,
                    height: 566,
                },
                Rect {
                    x: 0,
                    y: 34,
                    width: 360,
                    height: 240,
                },
            ] {
                let layout = command_palette_layout(
                    content,
                    &commands,
                    groups,
                    2,
                    0,
                    PaletteShape::Commands,
                )
                .unwrap();
                assert!(layout.panel.x >= content.x && layout.panel.y >= content.y);
                assert!(layout.panel.x + layout.panel.width <= content.x + content.width);
                assert!(layout.panel.y + layout.panel.height <= content.y + content.height);
                assert!(layout.rows.iter().any(|row| row.command == commands[2]));
                let mut y = layout.list.y;
                for (offset, row) in layout.rows.iter().enumerate() {
                    let index = layout.visible_start + offset;
                    // A header sits right above the row that starts its
                    // group, and rows follow one another without gaps.
                    match row.header {
                        Some((header, label)) => {
                            assert_eq!(label, groups[index]);
                            assert_eq!((header.y, header.height), (y, GROUP_HEIGHT));
                            assert_eq!((header.x, header.width), (row.rect.x, row.rect.width));
                            y += GROUP_HEIGHT;
                        }
                        None => assert!(!starts_group(groups, index)),
                    }
                    assert_eq!((row.rect.y, row.rect.height), (y, ROW_HEIGHT));
                    y += ROW_HEIGHT;
                    // Rows fill the inside of the frame.
                    assert_eq!(row.rect.x, layout.panel.x + BOX_EDGE);
                    assert_eq!(
                        row.rect.x + row.rect.width + BOX_EDGE,
                        layout.panel.x + layout.panel.width
                    );
                    assert!(row.rect.y >= layout.rule.y + layout.rule.height);
                    assert_eq!(
                        command_palette_hit_test(
                            &layout,
                            (f64::from(row.rect.x), f64::from(row.rect.y))
                        ),
                        Some(row.command)
                    );
                    assert_ne!(
                        command_palette_hit_test(
                            &layout,
                            (
                                f64::from(row.rect.x + row.rect.width),
                                f64::from(row.rect.y)
                            )
                        ),
                        Some(row.command)
                    );
                    if let Some((header, _)) = row.header {
                        // A header is not a hit target.
                        assert_eq!(
                            command_palette_hit_test(
                                &layout,
                                (f64::from(header.x), f64::from(header.y))
                            ),
                            None
                        );
                    }
                }
                assert!(y <= layout.list.y + layout.list.height);
            }
        }
    }

    /// Every command's title fits its slot whole in the 560 px palette with
    /// the default chrome cell, beside its binding's chips.
    #[test]
    fn every_command_title_fits_the_560_px_palette() {
        use super::super::chrome_rows::chips_width;

        let content = Rect {
            x: 0,
            y: 0,
            width: 1280,
            height: 800,
        };
        let layout = command_palette_layout(
            content,
            BuiltInCommandId::ALL,
            &[],
            0,
            0,
            PaletteShape::Commands,
        )
        .unwrap();
        assert_eq!(layout.panel.width, 560);
        let render_context = RenderContext::new(u16::MAX);
        let mut cache = SessionPickerTextCache::default();
        let regular = cache
            .cell_metrics(&render_context, ChromeTextStyle::Regular, 120, 1)
            .unwrap();
        let bold = cache
            .cell_metrics(&render_context, ChromeTextStyle::Bold, 120, 1)
            .unwrap();
        let row = buffer_rect(layout.rows[0].rect, 120);
        for keymap in [
            ResolvedKeymap::default(),
            crate::keymap::built_in_keymap(crate::keymap::KeymapProfile::OmarchyTmux),
        ] {
            for descriptor in BuiltInCommandId::ALL
                .iter()
                .map(|id| command_descriptor(*id))
            {
                let chips = shortcut_chips(descriptor.shortcut(&keymap));
                let columns = row_columns(
                    row,
                    ROW_PADDING,
                    regular.0,
                    0,
                    chips_width(&chips, regular, 120),
                );
                for (style, cell) in [
                    (ChromeTextStyle::Regular, regular),
                    (ChromeTextStyle::Bold, bold),
                ] {
                    let width = text_width(&render_context, descriptor.title, style, 120);
                    assert!(
                        width <= columns.title.width
                            && super::super::chrome_rows::text_cells(descriptor.title) * cell.0
                                <= columns.title.width,
                        "{:?} ({width} px) does not fit {} px",
                        descriptor.title,
                        columns.title.width
                    );
                }
            }
        }
    }

    /// Each list paints in a small Window at every scale.
    #[test]
    fn the_palette_paints_in_a_small_window_at_every_scale() {
        let content = Rect {
            x: 0,
            y: 20,
            width: 360,
            height: 240,
        };
        let state = CommandPaletteUi::new(palette_context());
        let keymap = ResolvedKeymap::default();
        let help = BindingHelpUi::new(&keymap);
        let access = AgentAccessUi::new(None, None);
        for scale in [120, 150, 180, 240] {
            let width = 360 * scale / 120;
            let height = 260 * scale / 120;
            for (body, shape, rows) in [
                (
                    PaletteBody::Commands,
                    PaletteShape::Commands,
                    state.filtered().len(),
                ),
                (
                    PaletteBody::BindingHelp(&help),
                    PaletteShape::BindingHelp,
                    help.rows().len(),
                ),
                (
                    PaletteBody::AgentAccess(&access),
                    PaletteShape::AgentAccess,
                    0,
                ),
            ] {
                let filtered = match body {
                    PaletteBody::Commands => state.filtered().to_vec(),
                    _ => vec![BuiltInCommandId::RecentSessions; rows],
                };
                let Some(layout) = command_palette_layout(content, &filtered, &[], 0, 0, shape)
                else {
                    continue;
                };
                let mut canvas = vec![0_u8; usize::try_from(width * height * 4).unwrap()];
                paint_command_palette(
                    &mut SessionPickerTextCache::default(),
                    &RenderContext::new(u16::MAX),
                    &mut canvas,
                    width,
                    height,
                    content,
                    scale,
                    1,
                    &layout,
                    session_picker_palette(ResolvedTheme::default()),
                    &state,
                    &keymap,
                    None,
                    true,
                    body,
                )
                .unwrap();
            }
        }
    }

    #[test]
    fn ultra_short_layout_is_rowless_and_keeps_children_inside_panel() {
        let content = Rect {
            x: 0,
            y: 34,
            width: 640,
            height: 120,
        };
        let commands = [BuiltInCommandId::NewDojo];
        let layout =
            command_palette_layout(content, &commands, &[], 0, 0, PaletteShape::Commands).unwrap();
        assert!(layout.rows.is_empty());
        assert_eq!(layout.list.height, 0);
        assert_eq!(
            layout.list.y + layout.list.height + FOOTER_HEIGHT,
            layout.panel.y + layout.panel.height
        );
        assert!(layout.input.y >= layout.panel.y + BOX_EDGE);
        assert!(layout.rule.y + layout.rule.height <= layout.list.y);
        assert!(layout.list.y + layout.list.height <= layout.panel.y + layout.panel.height);
        // Agent access's detail line needs more than this.
        assert!(
            command_palette_layout(content, &commands, &[], 0, 0, PaletteShape::AgentAccess)
                .is_none()
        );
    }

    /// Lists sit at 15% of the content height (at least 48 px down), as
    /// wide as 560 px (key bindings 640) and 24 px clear of each side, and
    /// move up only when their rows would not fit below.
    #[test]
    fn the_palette_is_top_anchored_and_sized_by_its_list() {
        let commands = BuiltInCommandId::ALL;
        for (width, height) in [(1280, 800), (960, 566), (640, 400), (420, 1000), (360, 240)] {
            let content = Rect {
                x: 17,
                y: 34,
                width,
                height,
            };
            for (shape, maximum) in [
                (PaletteShape::Commands, 560),
                (PaletteShape::BindingHelp, 640),
                (PaletteShape::AgentAccess, 560),
            ] {
                let layout = command_palette_layout(content, commands, &[], 0, 0, shape).unwrap();
                let panel = layout.panel;
                assert_eq!(panel.width, (width - 48).min(maximum), "{width}x{height}");
                assert_eq!(panel.x - content.x, (width - panel.width) / 2);
                let top = (height * 15 / 100).max(48);
                let lowest = content.y + height - 16 - panel.height;
                assert_eq!(panel.y, (content.y + top).min(lowest), "{width}x{height}");
                assert!(panel.y >= content.y + 16);
                assert_eq!(layout.detail.is_some(), shape != PaletteShape::Commands);
                if let Some(detail) = layout.detail {
                    assert_eq!(detail.y, layout.list.y + layout.list.height);
                    assert_eq!(
                        detail.y + detail.height + FOOTER_HEIGHT,
                        panel.y + panel.height
                    );
                }
            }
        }
        // A tall Window shows a full page at 15% down.
        let content = Rect {
            x: 0,
            y: 0,
            width: 1280,
            height: 1000,
        };
        let layout =
            command_palette_layout(content, commands, &[], 0, 0, PaletteShape::Commands).unwrap();
        assert_eq!(layout.panel.y, 150);
        assert_eq!(layout.rows.len(), LIST_MAXIMUM_ROWS);
        assert_eq!(layout.list.height, LIST_HEIGHT);
    }

    /// The input line sits inside the frame below the title, whatever the
    /// scale or UI text size: the title's knockout never reaches it, and
    /// the cursor follows the typed text with the start giving way.
    #[test]
    fn the_input_line_clears_the_title_and_its_cursor_follows_the_text() {
        let content = Rect {
            x: 0,
            y: 34,
            width: 960,
            height: 566,
        };
        let layout = command_palette_layout(
            content,
            BuiltInCommandId::ALL,
            &[],
            0,
            0,
            PaletteShape::Commands,
        )
        .unwrap();
        for scale in [120, 150, 180, 240] {
            for context in [
                RenderContext::new(u16::MAX),
                RenderContext::new(u16::MAX).at_maximum_chrome_font_size(),
            ] {
                let mut cache = SessionPickerTextCache::default();
                let regular = cache
                    .cell_metrics(&context, ChromeTextStyle::Regular, scale, 1)
                    .unwrap();
                let bold = cache
                    .cell_metrics(&context, ChromeTextStyle::Bold, scale, 1)
                    .unwrap();
                let cell = (regular.0.max(bold.0), regular.1.max(bold.1));
                assert!(cell.1 <= chrome_maximum_cell_height(scale));
                let chrome = ChromeBox {
                    title: "commands",
                    right_label: &["3/62"],
                    right_active: None,
                    legend: PaletteBody::Commands.legend(),
                }
                .layout(buffer_rect(layout.panel, scale), scale, cell);
                let input = buffer_rect(layout.input, scale);
                let title = chrome.title.unwrap();
                assert!(
                    title.knockout.y + title.knockout.height <= input.y,
                    "scale {scale}"
                );
                assert!(input.y >= chrome.frame.y + chrome.border);
                let list = buffer_rect(layout.list, scale);
                let legend = chrome.legend.unwrap();
                assert!(list.y + list.height <= legend.knockout.y, "scale {scale}");
                assert_eq!(legend.pairs.len(), 3);

                let line = InputLineLayout::new(input, INPUT_INSET * scale / 120, regular);
                let (shown, cursor) = line.fitted(input, "split").unwrap();
                assert_eq!(shown, "split");
                assert_eq!(cursor.x, line.text_x + 5 * regular.0);
                assert_eq!(cursor.width, regular.0);
                assert!(cursor.y >= input.y && cursor.y + cursor.height <= input.y + input.height);
                // An empty query puts the cursor where typing goes.
                let (shown, cursor) = line.fitted(input, "").unwrap();
                assert!(shown.is_empty());
                assert_eq!(cursor.x, line.text_x);
                // A long query keeps its end and the cursor stays inside.
                let long = "x".repeat(400);
                let (shown, cursor) = line.fitted(input, &long).unwrap();
                assert!(shown.starts_with('…') && shown.ends_with('x'));
                assert!(cursor.x + cursor.width <= line.end, "scale {scale}");
                assert!(line.marker.x >= chrome.frame.x + chrome.border);
            }
        }
    }
    #[test]
    fn the_count_label_says_how_many_match_of_how_many() {
        assert_eq!(match_count_label(62, 62, ""), "62");
        assert_eq!(match_count_label(3, 62, "split"), "3/62");
        assert_eq!(match_count_label(0, 62, "zzz"), "0/62");
        let mut state = CommandPaletteUi::new(palette_context());
        let total = state.total();
        assert_eq!(total, state.filtered().len());
        assert_eq!(
            PaletteBody::Commands.count_parts(&state),
            vec![total.to_string()]
        );
        assert!(state.append_text("split"));
        let shown = state.filtered().len();
        assert!(shown > 0 && shown < total);
        assert_eq!(
            PaletteBody::Commands.count_parts(&state),
            vec![format!("{shown}/{total}")]
        );
        let keymap = ResolvedKeymap::default();
        let help = BindingHelpUi::new(&keymap);
        assert_eq!(
            PaletteBody::BindingHelp(&help).count_parts(&state),
            vec![help.total().to_string()]
        );
        // Agent access puts what waits and a short read first, so they
        // are the last to drop.
        let lair_id = LairId::new();
        let mut pending = crate::frontend::agent_access_test_entry(lair_id);
        pending.kind = splinterm_protocol::AccessEntryKind::Pending;
        pending.state = splinterm_protocol::AccessEntryState::Prompting;
        let mut snapshot = crate::frontend::AccessSnapshot::new(
            3,
            splinterm_protocol::RememberedStoreStatus::Available,
        );
        snapshot.entries = vec![crate::frontend::agent_access_test_entry(lair_id), pending];
        snapshot.truncated = true;
        let access = AgentAccessUi::new(Some(&snapshot), None);
        assert_eq!(access.total(), 2);
        assert_eq!(
            PaletteBody::AgentAccess(&access).count_parts(&state),
            vec![
                "1 waiting".to_owned(),
                "first 512 only".to_owned(),
                "2".to_owned()
            ]
        );
        snapshot.truncated = false;
        snapshot.entries.pop();
        let access = AgentAccessUi::new(Some(&snapshot), None);
        assert_eq!(
            PaletteBody::AgentAccess(&access).count_parts(&state),
            vec!["1".to_owned()]
        );
    }

    #[test]
    fn empty_results_keep_a_visible_inert_list_row() {
        let content = Rect {
            x: 0,
            y: 34,
            width: 640,
            height: 400,
        };
        let layout =
            command_palette_layout(content, &[], &[], 0, 0, PaletteShape::Commands).unwrap();
        assert!(layout.rows.is_empty());
        assert_eq!(layout.list.height, ROW_HEIGHT);
        assert_eq!(command_palette_hit_test(&layout, (10.0, 100.0)), None);
    }

    /// With no query the palette shows its groups and keeps one height
    /// while it scrolls; filtering lists one ranked run without headers,
    /// ten rows at most, and a short result is only as tall as its rows.
    #[test]
    fn the_palette_groups_without_a_query_and_lists_flat_while_filtering() {
        let content = Rect {
            x: 0,
            y: 34,
            width: 960,
            height: 900,
        };
        let layout = |state: &CommandPaletteUi| {
            command_palette_layout(
                content,
                state.filtered(),
                &state.row_groups(),
                state.selected_index(),
                state.visible_start(),
                PaletteShape::Commands,
            )
            .unwrap()
        };
        let mut state = CommandPaletteUi::new(palette_context());
        let groups = state.row_groups();
        let first = layout(&state);
        assert_eq!(first.list.height, LIST_HEIGHT);
        assert_eq!(first.rows[0].header.map(|(_, label)| label), Some("dojo"));
        assert!(first.rows.len() <= LIST_MAXIMUM_ROWS);
        assert!(first.rows.iter().any(|row| row.header.is_some()));
        // Every group starts once: dojo, pane, history, lair, view, app.
        assert!(groups.windows(2).filter(|pair| pair[0] != pair[1]).count() + 1 >= 6);
        assert!(
            window_height(
                &groups,
                first.visible_start,
                first.visible_start + first.rows.len()
            ) <= LIST_HEIGHT
        );
        // Moving to the end scrolls; the panel keeps its place and height.
        assert!(state.select_last());
        let last = layout(&state);
        assert_eq!(last.panel, first.panel);
        assert!(
            last.rows
                .iter()
                .any(|row| Some(row.command) == state.selected_command())
        );
        assert!(last.visible_start > 0);
        // Filtering: no headers, ten rows at most.
        assert!(state.append_text("o"));
        assert!(state.row_groups().is_empty());
        assert!(state.filtered().len() > LIST_MAXIMUM_ROWS);
        let filtered = layout(&state);
        assert!(filtered.rows.iter().all(|row| row.header.is_none()));
        assert_eq!(filtered.rows.len(), LIST_MAXIMUM_ROWS);
        assert_eq!(filtered.list.height, LIST_HEIGHT);
        while state.backspace() {}
        assert!(state.append_text("split"));
        let few = layout(&state);
        assert_eq!(few.rows.len(), 2);
        assert_eq!(few.list.height, 2 * ROW_HEIGHT);
    }

    #[test]
    fn list_titles_are_lowercase_and_the_source_column_ellipsizes() {
        assert_eq!(PaletteBody::Commands.title(), "commands");
        let keymap = ResolvedKeymap::default();
        let help = BindingHelpUi::new(&keymap);
        assert_eq!(PaletteBody::BindingHelp(&help).title(), "key bindings");
        let access = AgentAccessUi::new(None, None);
        assert_eq!(PaletteBody::AgentAccess(&access).title(), "agent access");
        assert_eq!(
            PaletteBody::Commands.legend(),
            &[("↑↓", "move"), ("↵", "run"), ("esc", "close")]
        );
        assert_eq!(
            PaletteBody::BindingHelp(&help).legend(),
            &[
                ("↑↓", "move"),
                ("pgup pgdn", "page"),
                ("ctrl u", "clear"),
                ("esc", "close"),
            ]
        );
        let context = RenderContext::new(u16::MAX);
        let mut cache = SessionPickerTextCache::default();
        for scale in [120, 150, 180, 240] {
            let source = "built-in profile splinterm";
            let clip = 72_u32.saturating_mul(scale).div_ceil(120);
            paint_label(
                &mut cache,
                &context,
                source,
                ChromeTextStyle::Regular,
                scale,
                clip,
            );
            assert!(
                cache.drawn().any(|(text, _)| text.ends_with('…')
                    && source.starts_with(text.trim_end_matches('…'))
                    && text_width(&context, text, ChromeTextStyle::Regular, scale) <= clip),
                "scale {scale}"
            );
        }
    }

    #[test]
    fn editable_fields_keep_their_caret() {
        assert_eq!(editable_text("> ", "dojo", 20), "> dojo_");
        assert_eq!(editable_text("> ", "my-long-dojo-name", 12), "> …ojo-name_");
    }

    /// Every fixed chrome line is tall enough for the largest UI text.
    #[test]
    fn fixed_chrome_lines_fit_the_largest_ui_text() {
        use super::super::super::text::CHROME_MAXIMUM_LINE_HEIGHT;

        for height in [ROW_HEIGHT, GROUP_HEIGHT, INPUT_HEIGHT, DETAIL_HEIGHT] {
            assert!(height >= CHROME_MAXIMUM_LINE_HEIGHT);
        }
    }

    #[test]
    #[allow(clippy::too_many_lines, reason = "one bounded paint/cache scenario")]
    fn painter_marks_transient_panel_and_reuses_bounded_text() {
        let content = Rect {
            x: 0,
            y: 34,
            width: 640,
            height: 366,
        };
        let state = CommandPaletteUi::new(palette_context());
        let layout = command_palette_layout(
            content,
            state.filtered(),
            &state.row_groups(),
            state.selected_index(),
            state.visible_start(),
            PaletteShape::Commands,
        )
        .unwrap();
        let theme = ResolvedTheme::default();
        let mut canvas = vec![0_u8; 640 * 400 * 4];
        let before = canvas.clone();
        let mut cache = SessionPickerTextCache::default();
        let keymap = ResolvedKeymap::default();
        let paint = |cache: &mut SessionPickerTextCache,
                     canvas: &mut [u8],
                     layout: &CommandPaletteLayout,
                     body: PaletteBody<'_>| {
            paint_command_palette(
                cache,
                &RenderContext::new(u16::MAX),
                canvas,
                640,
                400,
                content,
                120,
                1,
                layout,
                session_picker_palette(theme),
                &state,
                &keymap,
                None,
                true,
                body,
            )
            .unwrap();
        };
        paint(&mut cache, &mut canvas, &layout, PaletteBody::Commands);
        let shaped = cache.len();
        assert!(shaped > 0);
        assert_ne!(canvas, before);
        assert!(cache.contains_source("commands"));
        assert!(cache.contains_source(&state.total().to_string()));
        assert!(cache.contains_source("type a command"));
        assert!(cache.contains_source("›"));
        // Groups with no query, and the bindings as chips.
        assert!(cache.contains_source("dojo"));
        assert!(cache.contains_source("ctrl"));
        assert!(!cache.contains_source("DOJO"));
        for (key, verb) in PaletteBody::Commands.legend() {
            assert!(cache.contains_source(key) && cache.contains_source(verb));
        }
        paint(&mut cache, &mut canvas, &layout, PaletteBody::Commands);
        assert_eq!(cache.len(), shaped);
        let mut help = BindingHelpUi::new(&keymap);
        let help_layout = command_palette_layout(
            content,
            &vec![BuiltInCommandId::RecentSessions; help.rows().len()],
            &help.row_groups(),
            0,
            0,
            PaletteShape::BindingHelp,
        )
        .unwrap();
        paint(
            &mut cache,
            &mut canvas,
            &help_layout,
            PaletteBody::BindingHelp(&help),
        );
        assert!(cache.contains_source("filter by key or name"));
        assert!(cache.contains_source("key bindings"));
        assert!(cache.contains_source("pgup pgdn"));
        // Human titles, chips, and the selected binding's config name and
        // source on the detail line.
        let selected = &help.rows()[help.selected_index()];
        assert!(cache.contains_source(&selected.title));
        assert!(cache.contains_source(&selected.chips[0]));
        assert!(cache.contains_source(selected.group));
        assert!(cache.drawn().any(|(text, _)| text == selected.detail
            || (text.ends_with('…') && selected.detail.starts_with(text.trim_end_matches('…')))));
        assert!(!cache.contains_source(&selected.action));

        // The access list paints its rows as words, never colour alone.
        let lair_id = LairId::new();
        let mut snapshot = crate::frontend::AccessSnapshot::new(
            3,
            splinterm_protocol::RememberedStoreStatus::Available,
        );
        snapshot.entries = vec![crate::frontend::agent_access_test_entry(lair_id)];
        let access = AgentAccessUi::new(Some(&snapshot), None);
        let access_layout = command_palette_layout(
            content,
            &vec![BuiltInCommandId::RecentSessions; access.rows().len()],
            &access.row_groups(),
            0,
            0,
            PaletteShape::AgentAccess,
        )
        .unwrap();
        paint(
            &mut cache,
            &mut canvas,
            &access_layout,
            PaletteBody::AgentAccess(&access),
        );
        assert!(cache.contains_source("search agent access"));
        assert!(cache.contains_source("agent access"));
        assert!(cache.contains_source("always"));
        assert!(cache.contains_source("not connected"));
        // The detail line, whole or ending in `…`.
        let footer = access.footer();
        assert!(cache.drawn().any(|(text, _)| text == footer
            || (text.ends_with('…') && footer.starts_with(text.trim_end_matches('…')))));
        assert!(
            cache.contains_source(
                &PaletteBody::AgentAccess(&access)
                    .count_parts(&state)
                    .join(" · ")
            )
        );

        assert!(help.append_text("definitely absent xyz"));
        let empty_layout =
            command_palette_layout(content, &[], &[], 0, 0, PaletteShape::BindingHelp).unwrap();
        // The cache keeps three frames; repaint until the help list's
        // placeholder has aged out.
        for _ in 0..3 {
            paint(
                &mut cache,
                &mut canvas,
                &empty_layout,
                PaletteBody::BindingHelp(&help),
            );
        }
        assert!(cache.contains_source("definitely absent xyz"));
        assert!(!cache.contains_source("filter by key or name"));
        assert!(cache.contains_source(&format!("0/{}", help.total())));
        assert!(cache.contains_source("No matching keybindings"));
    }

    /// The input line ends in a block cursor, an accent cell right after
    /// the typed text, and the rule under it is drawn in the divider.
    #[test]
    fn the_input_line_draws_a_block_cursor_after_the_query() {
        let content = Rect {
            x: 0,
            y: 34,
            width: 640,
            height: 366,
        };
        let mut state = CommandPaletteUi::new(palette_context());
        assert!(state.append_text("split"));
        let layout = command_palette_layout(
            content,
            state.filtered(),
            &state.row_groups(),
            state.selected_index(),
            state.visible_start(),
            PaletteShape::Commands,
        )
        .unwrap();
        let theme = ResolvedTheme::default();
        let palette = session_picker_palette(theme);
        let render_context = RenderContext::new(u16::MAX);
        let mut cache = SessionPickerTextCache::default();
        let mut canvas = vec![0_u8; 640 * 400 * 4];
        paint_command_palette(
            &mut cache,
            &render_context,
            &mut canvas,
            640,
            400,
            content,
            120,
            1,
            &layout,
            palette,
            &state,
            &ResolvedKeymap::default(),
            None,
            true,
            PaletteBody::Commands,
        )
        .unwrap();
        assert!(cache.contains_source("split"));
        assert!(!cache.contains_source("type a command"));
        let pixel = |x: u32, y: u32| {
            let index = usize::try_from((y * 640 + x) * 4).unwrap();
            <[u8; 4]>::try_from(&canvas[index..index + 4]).unwrap()
        };
        let bgra = |color: u32| {
            let [_, red, green, blue] = color.to_be_bytes();
            [blue, green, red, u8::MAX]
        };
        // The matched characters are bold, the rest of the title regular.
        assert!(
            cache
                .drawn()
                .any(|(text, style)| text == "Split" && style == ChromeTextStyle::Bold)
        );
        assert!(cache.drawn().any(|(text, style)| {
            text == " pane horizontally" && style == ChromeTextStyle::Regular
        }));
        // The selected row is the selection fill edge to edge: no rail.
        let selected = layout.rows[0].rect;
        assert_eq!(state.selected_command(), Some(layout.rows[0].command));
        for x in [selected.x, selected.x + 1, selected.x + selected.width - 1] {
            assert_eq!(
                pixel(x, selected.y + selected.height - 1),
                bgra(palette.selected_fill),
                "x {x}"
            );
        }
        let cell = cache
            .cell_metrics(&render_context, ChromeTextStyle::Regular, 120, 1)
            .unwrap();
        let line = InputLineLayout::new(layout.input, INPUT_INSET, cell);
        let (_, cursor) = line.fitted(layout.input, "split").unwrap();
        assert_eq!(
            cursor.x,
            layout.input.x + INPUT_INSET + 2 * cell.0 + 5 * cell.0
        );
        let accent = bgra(palette.selected_rail);
        for (x, y) in [
            (cursor.x, cursor.y),
            (cursor.x + cursor.width - 1, cursor.y + cursor.height - 1),
        ] {
            assert_eq!(pixel(x, y), accent, "({x}, {y})");
        }
        assert_ne!(pixel(cursor.x + cursor.width, cursor.y), accent);
        assert_eq!(
            pixel(layout.rule.x + layout.rule.width / 2, layout.rule.y),
            bgra(palette.frame)
        );
        // The title's knockout breaks the focused frame.
        let frame_y = layout.panel.y + 9;
        assert_eq!(
            pixel(layout.panel.x + 9, frame_y),
            bgra(palette.focused_frame)
        );
        let knockout_x = layout.panel.x + 9 + BOX_LABEL_INSET;
        assert_ne!(pixel(knockout_x, frame_y), bgra(palette.focused_frame));
    }

    #[test]
    fn translucent_menu_panel_follows_shell_alpha_with_opaque_text() {
        use crate::{
            config::OmarchyShellSettings,
            renderer::{ChromeSurface, chrome_panel_palette},
        };

        let content = Rect {
            x: 0,
            y: 34,
            width: 640,
            height: 366,
        };
        let state = CommandPaletteUi::new(palette_context());
        let layout = command_palette_layout(
            content,
            state.filtered(),
            &state.row_groups(),
            state.selected_index(),
            state.visible_start(),
            PaletteShape::Commands,
        )
        .unwrap();
        // Glassy Windows (0.65) with [menu] background-alpha 0.9.
        let theme = ResolvedTheme {
            background_alpha: 42_597,
            shell: OmarchyShellSettings {
                menu_alpha: 58_981,
                ..OmarchyShellSettings::default()
            },
            roles: crate::theme::ChromeRoles {
                surface: 0x1a_1b_26,
                surface_raised: 0x1a_1b_26,
                text: 0xc0_ca_f5,
                text_muted: 0x8f_97_bb,
                danger: 0xf7_76_8e,
                ..crate::theme::ChromeRoles::default()
            },
            ..ResolvedTheme::default()
        };
        let palette = chrome_panel_palette(theme, ChromeSurface::Menu);
        assert_eq!(palette.panel_fill[3], 247);
        let mut canvas = vec![0_u8; 640 * 400 * 4];
        paint_command_palette(
            &mut SessionPickerTextCache::default(),
            &RenderContext::new(theme.background_alpha),
            &mut canvas,
            640,
            400,
            content,
            120,
            1,
            &layout,
            palette,
            &state,
            &ResolvedKeymap::default(),
            None,
            true,
            PaletteBody::Commands,
        )
        .unwrap();
        let panel = buffer_rect(layout.panel, 120);
        let fill = palette.panel_fill;
        let fill_bgra = [fill[2], fill[1], fill[0], fill[3]];
        let [_, red, green, blue] = palette.primary.to_be_bytes();
        let text_bgra = [blue, green, red, u8::MAX];
        let (mut filled, mut text, mut total) = (0, 0, 0);
        for y in panel.y + 1..panel.y + panel.height - 1 {
            for x in panel.x + 1..panel.x + panel.width - 1 {
                let index = usize::try_from((y * 640 + x) * 4).unwrap();
                let pixel: [u8; 4] = canvas[index..index + 4].try_into().unwrap();
                total += 1;
                filled += usize::from(pixel == fill_bgra);
                text += usize::from(pixel == text_bgra);
            }
        }
        assert!(filled * 2 > total, "{filled} of {total} panel pixels");
        assert!(text > 0, "fully covered glyph pixels stay opaque text");
    }

    /// Terminate is drawn in danger in the command palette and the tab menu;
    /// every other row keeps its colour. The prompts' buttons are pinned in
    /// `prompt_box`.
    #[test]
    fn only_terminate_rows_are_drawn_in_danger() {
        let palette = session_picker_palette(ResolvedTheme::default());
        assert_ne!(palette.danger, palette.primary);
        for command in BuiltInCommandId::ALL.iter().copied() {
            let destructive = matches!(
                command,
                BuiltInCommandId::TerminateCurrentDojo | BuiltInCommandId::TerminateCurrentLair
            );
            assert_eq!(command.destructive(), destructive, "{command:?}");
        }
        for action in [
            TabMenuActionId::TerminateLair,
            TabMenuActionId::TerminateDojo,
            TabMenuActionId::CloseTab,
            TabMenuActionId::Restore,
            TabMenuActionId::RenameTab,
        ] {
            let destructive = matches!(
                action,
                TabMenuActionId::TerminateLair | TabMenuActionId::TerminateDojo
            );
            assert_eq!(action.destructive(), destructive, "{action:?}");
        }
    }
}
