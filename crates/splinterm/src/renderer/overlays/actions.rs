//! Trusted command-palette layout and CPU composition.

use anyhow::Result;
use unicode_width::UnicodeWidthChar;

#[cfg(test)]
use crate::frontend::{TAB_MENU_ACTIONS, tab_menu_descriptor};
use crate::{
    frontend::{
        AgentAccessUi, BindingHelpUi, BuiltInCommandId, COMMAND_PALETTE_PAGE_ITEMS,
        CommandPaletteUi, DojoPromptUi, TabContextMenuUi, TabMenuActionId, TerminationDecision,
        command_descriptor,
    },
    geometry::Rect,
    keymap::ResolvedKeymap,
};

use super::{
    super::{ChromeTextStyle, RenderContext},
    chrome_box::{
        BOX_CLEARANCE, BOX_EDGE, ChromeBox, ChromePainter, opaque, paint_outline, paint_scrim,
        shadow_rect,
    },
    picker::{
        PickerTextAlignment, ROW_HOVER_ALPHA, ROW_PRESS_ALPHA, SessionPickerPalette,
        SessionPickerTextCache, editable_text,
    },
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
/// Agent access's line under its list: what the selected entry is, or a
/// notice.
const DETAIL_HEIGHT: u32 = 28;
const ROW_HEIGHT: u32 = 48;
/// The command palette's and Agent access's width at most.
const COMMAND_PALETTE_WIDTH: u32 = 560;
/// The key bindings list's width at most: its rows carry a source column.
const BINDING_HELP_WIDTH: u32 = 640;
const PANEL_HORIZONTAL_MARGIN: u32 = 24;
const PANEL_VERTICAL_MARGIN: u32 = 16;
/// Lists sit at this share of the content height from its top, and at
/// least [`PANEL_MINIMUM_TOP`] below it.
const PANEL_TOP_PERCENT: u32 = 15;
const PANEL_MINIMUM_TOP: u32 = 48;
const CONTENT_INSET: u32 = 24;
const ROW_GUTTER_Y: u32 = 2;
const TAB_MENU_WIDTH: u32 = 156;
const TAB_MENU_ROW_HEIGHT: u32 = 28;
const TAB_MENU_PADDING: u32 = 2;
const TAB_MENU_CONTENT_INSET: u32 = 4;
const TAB_MENU_INDICATOR_WIDTH: u32 = 12;
const TAB_MENU_SHADOW: u32 = 6;
const TAB_MENU_ANCHOR_GAP: u32 = 4;
const PROMPT_MAX_WIDTH: u32 = 520;
const PROMPT_MARGIN: u32 = 24;
const PROMPT_PADDING: u32 = 24;
const PROMPT_TITLE_HEIGHT: u32 = 34;
const PROMPT_BODY_HEIGHT: u32 = 42;
const PREVIEW_BODY_HEIGHT: u32 = 168;
const PREVIEW_LINE_HEIGHT: u32 = 24;
const MAX_PREVIEW_LINES: usize = 7;
const MAX_PREVIEW_LINE_SCALARS: usize = 120;
const PROMPT_INPUT_HEIGHT: u32 = 38;
const PROMPT_BUTTON_HEIGHT: u32 = 34;
const PROMPT_GAP: u32 = 12;

fn preview_text_width(text: &str) -> usize {
    text.chars()
        .map(|character| character.width().unwrap_or(0).min(2))
        .sum()
}

fn ellipsize_preview_line(line: &mut String, maximum_cells: usize) {
    if maximum_cells == 0 {
        line.clear();
        return;
    }
    while preview_text_width(line) >= maximum_cells {
        line.pop();
    }
    line.push('…');
}

fn bounded_preview_indentation(source: &str, maximum_cells: usize) -> (String, usize) {
    let mut indentation = String::new();
    let mut cells = 0_usize;
    for character in source
        .chars()
        .take_while(|character| character.is_whitespace())
    {
        let width = character.width().unwrap_or(0).min(2);
        if cells.saturating_add(width) > maximum_cells {
            break;
        }
        indentation.push(character);
        cells = cells.saturating_add(width);
    }
    (indentation, cells)
}

fn preview_line_rect(body: Rect, index: usize, line_height: u32) -> Option<Rect> {
    let y = body.y.saturating_add(
        u32::try_from(index)
            .unwrap_or(u32::MAX)
            .saturating_mul(line_height),
    );
    let bottom = body.y.saturating_add(body.height);
    if y >= bottom {
        return None;
    }
    Some(Rect {
        y,
        height: line_height.min(bottom.saturating_sub(y)),
        ..body
    })
}

fn wrap_preview_body(body: &str, maximum_cells: usize) -> Vec<String> {
    let maximum_cells = maximum_cells.max(1);
    let mut wrapped = Vec::with_capacity(MAX_PREVIEW_LINES);
    let mut overflowed = false;

    'source_lines: for source in body.lines() {
        if wrapped.len() == MAX_PREVIEW_LINES {
            overflowed = true;
            break;
        }
        if source.is_empty() {
            wrapped.push(String::new());
            continue;
        }

        let (indentation, indentation_cells) =
            bounded_preview_indentation(source, maximum_cells / 2);
        let mut remaining = source.trim_start();

        while !remaining.is_empty() {
            if wrapped.len() == MAX_PREVIEW_LINES {
                overflowed = true;
                break 'source_lines;
            }
            let available = maximum_cells.saturating_sub(indentation_cells).max(1);
            let mut used = 0_usize;
            let mut boundary = 0_usize;
            let mut whitespace = None;
            for (offset, character) in remaining.char_indices() {
                let width = character.width().unwrap_or(0).min(2);
                if used.saturating_add(width) > available {
                    break;
                }
                used = used.saturating_add(width);
                boundary = offset.saturating_add(character.len_utf8());
                if character.is_whitespace() {
                    whitespace = Some(offset);
                }
            }
            if boundary == remaining.len() {
                wrapped.push(format!("{indentation}{remaining}"));
                break;
            }
            if boundary == 0 {
                let consumed = remaining
                    .chars()
                    .next()
                    .map_or(remaining.len(), char::len_utf8);
                wrapped.push(format!("{indentation}…"));
                remaining = remaining[consumed..].trim_start();
                continue;
            }
            let split = whitespace.filter(|offset| *offset > 0).unwrap_or(boundary);
            let chunk = remaining[..split].trim_end();
            wrapped.push(format!("{indentation}{chunk}"));
            remaining = remaining[split..].trim_start();
        }
    }

    if overflowed && let Some(last) = wrapped.last_mut() {
        ellipsize_preview_line(last, maximum_cells);
    }
    wrapped
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommandPaletteRowLayout {
    pub(crate) command: BuiltInCommandId,
    pub(crate) rect: Rect,
    pub(crate) category: Rect,
    pub(crate) title: Rect,
    pub(crate) shortcut: Rect,
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
    /// Agent access's line under the list.
    pub(crate) detail: Option<Rect>,
    pub(crate) rows: Vec<CommandPaletteRowLayout>,
    pub(crate) visible_start: usize,
    pub(crate) compact: bool,
}

/// Which list the palette's panel holds: the key bindings list is wider,
/// and Agent access has a detail line under its rows.
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
            Self::AgentAccess => DETAIL_HEIGHT,
            Self::Commands | Self::BindingHelp => 0,
        }
    }
}

/// The palette's panel in `content`: top-anchored at 15% of its height (at
/// least 48 px down), as wide as its shape allows but 24 px clear of each
/// side, moved up when its rows would not fit below.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "one pure layout seam keeps responsive geometry and hit targets coherent"
)]
pub(crate) fn command_palette_layout(
    content: Rect,
    filtered: &[BuiltInCommandId],
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
    let available_list_height = available_height.saturating_sub(chrome_height);
    let capacity = usize::try_from(available_list_height / ROW_HEIGHT)
        .unwrap_or(0)
        .min(COMMAND_PALETTE_PAGE_ITEMS);
    let visible_count = filtered.len().min(capacity);
    let mut visible_start = requested_start.min(filtered.len().saturating_sub(visible_count));
    if !filtered.is_empty() {
        let selected = selected.min(filtered.len() - 1);
        if selected < visible_start {
            visible_start = selected;
        } else if selected >= visible_start.saturating_add(visible_count) {
            visible_start = selected.saturating_add(1).saturating_sub(visible_count);
        }
    }
    // No partial row: when not even one row fits, the list is empty.
    let list_height = if capacity == 0 {
        0
    } else if filtered.is_empty() {
        ROW_HEIGHT
    } else {
        ROW_HEIGHT.saturating_mul(u32::try_from(visible_count).unwrap_or(u32::MAX))
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
    let compact = panel_width < 420;
    let rows = filtered
        .iter()
        .copied()
        .skip(visible_start)
        .take(visible_count)
        .enumerate()
        .map(|(slot, command)| {
            let slot_y = list
                .y
                .saturating_add(ROW_HEIGHT.saturating_mul(u32::try_from(slot).unwrap_or(u32::MAX)));
            let vertical_gutter = ROW_GUTTER_Y.min(ROW_HEIGHT / 4);
            let rect = Rect {
                x: list.x,
                y: slot_y.saturating_add(vertical_gutter),
                width: list.width,
                height: ROW_HEIGHT.saturating_sub(vertical_gutter.saturating_mul(2)),
            };
            let inset = CONTENT_INSET.min(rect.width / 6);
            let shortcut_width = if compact {
                0
            } else {
                (rect.width / 3).min(180)
            };
            let content_x = rect.x.saturating_add(inset).saturating_add(20);
            let category_width = if compact { 0 } else { 72.min(rect.width / 5) };
            let category_gap = if category_width == 0 { 0 } else { 12 };
            let title_x = content_x
                .saturating_add(category_width)
                .saturating_add(category_gap);
            CommandPaletteRowLayout {
                command,
                rect,
                category: Rect {
                    x: content_x,
                    y: rect.y,
                    width: category_width,
                    height: rect.height,
                },
                title: Rect {
                    x: title_x,
                    y: rect.y,
                    width: rect
                        .x
                        .saturating_add(rect.width)
                        .saturating_sub(title_x)
                        .saturating_sub(shortcut_width)
                        .saturating_sub(inset),
                    height: rect.height,
                },
                shortcut: Rect {
                    x: rect
                        .x
                        .saturating_add(rect.width)
                        .saturating_sub(shortcut_width)
                        .saturating_sub(inset),
                    y: rect.y,
                    width: shortcut_width,
                    height: rect.height,
                },
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
        visible_start,
        compact,
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

/// The widest of `actions`' labels in logical px, regular or bold: chrome
/// text is monospaced, so it is the most cells times the wider cell.
pub(crate) fn tab_menu_label_width(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    actions: &[crate::frontend::TabMenuActionDescriptor],
    scale_120: u32,
    renderer_generation: u64,
) -> Result<u32> {
    let mut cell_width = 0;
    for style in [ChromeTextStyle::Regular, ChromeTextStyle::Bold] {
        cell_width = cell_width.max(
            cache
                .cell_metrics(context, style, scale_120, renderer_generation)?
                .0,
        );
    }
    let cells = actions
        .iter()
        .map(|action| preview_text_width(action.title))
        .max()
        .unwrap_or(0);
    Ok(u32::try_from(cells)
        .unwrap_or(u32::MAX)
        .saturating_mul(cell_width)
        .saturating_mul(120)
        .div_ceil(scale_120.max(1)))
}

/// An editable `value` after `prefix` with its caret, fitted to `clip_width`
/// buffer pixels by keeping its end: the caret and the latest input stay
/// visible and the start gives way behind `…`.
fn editable_field_text(
    painter: &mut ChromePainter<'_>,
    prefix: &str,
    value: &str,
    clip_width: u32,
) -> Result<String> {
    let (cell_width, _) = painter.cell(ChromeTextStyle::Regular)?;
    let maximum_cells = usize::try_from(clip_width / cell_width).unwrap_or(usize::MAX);
    Ok(editable_text(prefix, value, maximum_cells))
}

/// `color` at `alpha` out of 255, to blend.
fn translucent(color: u32, alpha: u8) -> [u8; 4] {
    let [red, green, blue, _] = opaque(color);
    [red, green, blue, alpha]
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
            Self::BindingHelp(_) => "search key bindings",
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

    /// The frame's right label: how many rows match of how many while a
    /// query filters, the total otherwise. Agent access adds what waits.
    fn count_label(self, state: &CommandPaletteUi) -> String {
        let (shown, total) = match self {
            Self::Commands => (state.filtered().len(), state.total()),
            Self::BindingHelp(help) => (help.rows().len(), help.total()),
            Self::AgentAccess(access) => (access.rows().len(), access.total()),
        };
        let count = match_count_label(shown, total, self.query(state));
        match self {
            Self::AgentAccess(access) => {
                let status = access.status_label();
                if status.is_empty() {
                    count
                } else {
                    format!("{count} · {status}")
                }
            }
            Self::Commands | Self::BindingHelp(_) => count,
        }
    }
}

/// `shown/total` while `query` filters; `total` otherwise.
fn match_count_label(shown: usize, total: usize, query: &str) -> String {
    if query.is_empty() {
        total.to_string()
    } else {
        format!("{shown}/{total}")
    }
}

/// One read-only list row: category, title, one-line form, and the text in
/// the shortcut column.
struct ListRowText<'a> {
    category: &'a str,
    title: &'a str,
    compact: &'a str,
    trailing: &'a str,
}

/// Where the input line's parts go, in buffer px: the `›`, the text, and
/// the block cursor after it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InputLineLayout {
    marker: Rect,
    text_x: u32,
    /// The text's end, where the placeholder stops.
    end: u32,
    cell: (u32, u32),
}

impl InputLineLayout {
    fn new(input: Rect, inset: u32, cell: (u32, u32)) -> Self {
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
    fn fitted(&self, input: Rect, query: &str) -> Option<(String, Rect)> {
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
fn paint_palette_input(
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
    let count = body.count_label(state);
    let chrome = ChromeBox {
        title: body.title(),
        right_label: &count,
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
    if let (Some(detail), PaletteBody::AgentAccess(access)) = (layout.detail, body) {
        let detail = painter.buffer_rect(detail);
        let inset = painter.scaled(INPUT_INSET).min(detail.width / 4);
        painter.text(
            &access.footer(),
            ChromeTextStyle::Regular,
            Rect {
                x: detail.x.saturating_add(inset),
                width: detail.width.saturating_sub(inset.saturating_mul(2)),
                ..detail
            },
            PickerTextAlignment::Left,
            palette.secondary,
        )?;
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

/// The palette's rows, or the line that says none match.
fn paint_palette_list(
    painter: &mut ChromePainter<'_>,
    layout: &CommandPaletteLayout,
    palette: &SessionPickerPalette,
    state: &CommandPaletteUi,
    keymap: &ResolvedKeymap,
    pressed: Option<BuiltInCommandId>,
    body: PaletteBody<'_>,
) -> Result<()> {
    let inset = painter.scaled(CONTENT_INSET);
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
        let empty = painter.buffer_rect(layout.list);
        painter.text(
            empty_label,
            ChromeTextStyle::Regular,
            Rect {
                x: empty.x.saturating_add(inset),
                width: empty.width.saturating_sub(inset.saturating_mul(2)),
                ..empty
            },
            PickerTextAlignment::Left,
            palette.secondary,
        )?;
    }
    match body {
        PaletteBody::BindingHelp(help) => {
            let rows = help
                .rows()
                .iter()
                .map(|row| ListRowText {
                    category: &row.source,
                    title: &row.action,
                    compact: &row.compact,
                    trailing: &row.shortcut,
                })
                .collect::<Vec<_>>();
            paint_list_rows(painter, layout, palette, &rows, help.selected_index())
        }
        PaletteBody::AgentAccess(access) => {
            let rows = access
                .rows()
                .iter()
                .map(|row| ListRowText {
                    category: row.category,
                    title: &row.title,
                    compact: &row.compact,
                    trailing: &row.status,
                })
                .collect::<Vec<_>>();
            paint_list_rows(painter, layout, palette, &rows, access.selected_index())
        }
        PaletteBody::Commands => {
            paint_command_rows(painter, layout, palette, state, keymap, pressed)
        }
    }
}

/// The selected row's fill and accent rail.
fn paint_selected_row(painter: &mut ChromePainter<'_>, rect: Rect, palette: &SessionPickerPalette) {
    painter.fill(rect, opaque(palette.selected_fill));
    let rail = painter.scaled(3).max(1);
    painter.fill(
        Rect {
            width: rail.min(rect.width),
            ..rect
        },
        opaque(palette.selected_rail),
    );
}

/// The key bindings' or Agent access's read-only rows.
fn paint_list_rows(
    painter: &mut ChromePainter<'_>,
    layout: &CommandPaletteLayout,
    palette: &SessionPickerPalette,
    list_rows: &[ListRowText<'_>],
    selected_index: usize,
) -> Result<()> {
    for (offset, row) in layout.rows.iter().enumerate() {
        let index = layout.visible_start.saturating_add(offset);
        let Some(list_row) = list_rows.get(index) else {
            continue;
        };
        let rect = painter.buffer_rect(row.rect);
        let is_selected = index == selected_index;
        if is_selected {
            paint_selected_row(painter, rect, palette);
        }
        let (primary, secondary) = palette.row_text(is_selected, false);
        if row.category.width > 0 {
            let clip = painter.buffer_rect(row.category);
            painter.text(
                list_row.category,
                ChromeTextStyle::Regular,
                clip,
                PickerTextAlignment::Left,
                secondary,
            )?;
        }
        let clip = painter.buffer_rect(row.title);
        painter.text(
            if layout.compact {
                list_row.compact
            } else {
                list_row.title
            },
            if is_selected {
                ChromeTextStyle::Bold
            } else {
                ChromeTextStyle::Regular
            },
            clip,
            PickerTextAlignment::Left,
            primary,
        )?;
        if !layout.compact {
            let clip = painter.buffer_rect(row.shortcut);
            painter.text_keep_end(list_row.trailing, ChromeTextStyle::Regular, clip, secondary)?;
        }
    }
    Ok(())
}

/// The command rows: selected, hovered, pressed, disabled, and Terminate in
/// danger.
fn paint_command_rows(
    painter: &mut ChromePainter<'_>,
    layout: &CommandPaletteLayout,
    palette: &SessionPickerPalette,
    state: &CommandPaletteUi,
    keymap: &ResolvedKeymap,
    pressed: Option<BuiltInCommandId>,
) -> Result<()> {
    let inset = painter.scaled(CONTENT_INSET);
    let selected = state.selected_command();
    for row in &layout.rows {
        let rect = painter.buffer_rect(row.rect);
        let enabled = state.command_enabled(row.command);
        let is_selected = enabled && selected == Some(row.command);
        if is_selected {
            paint_selected_row(painter, rect, palette);
        } else if enabled && state.hovered() == Some(row.command) {
            painter.blend(rect, translucent(palette.selected_fill, ROW_HOVER_ALPHA));
        }
        if enabled && pressed == Some(row.command) {
            painter.blend(rect, translucent(palette.selected_rail, ROW_PRESS_ALPHA));
        }
        let highlighted = enabled
            && !is_selected
            && (state.hovered() == Some(row.command) || pressed == Some(row.command));
        let (row_primary, row_secondary) = palette.row_text(is_selected, highlighted);
        let primary = if enabled {
            row_primary
        } else {
            palette.secondary
        };
        if is_selected {
            let marker = Rect {
                x: rect.x.saturating_add(inset / 2),
                width: painter.scaled(20),
                ..rect
            };
            painter.text(
                "›",
                ChromeTextStyle::Bold,
                marker,
                PickerTextAlignment::Left,
                primary,
            )?;
        }
        let descriptor = command_descriptor(row.command);
        if row.category.width > 0 {
            let clip = painter.buffer_rect(row.category);
            painter.text(
                descriptor.category.label(),
                ChromeTextStyle::Regular,
                clip,
                PickerTextAlignment::Left,
                row_secondary,
            )?;
        }
        let clip = painter.buffer_rect(row.title);
        painter.text(
            descriptor.title,
            if is_selected {
                ChromeTextStyle::Bold
            } else {
                ChromeTextStyle::Regular
            },
            clip,
            PickerTextAlignment::Left,
            row_label_color(
                palette,
                enabled && row.command.destructive(),
                is_selected,
                highlighted,
                primary,
            ),
        )?;
        if !layout.compact {
            let clip = painter.buffer_rect(row.shortcut);
            let badge = state.badge(row.command);
            painter.text_keep_end(
                badge
                    .as_deref()
                    .unwrap_or_else(|| descriptor.shortcut(keymap)),
                ChromeTextStyle::Regular,
                clip,
                row_secondary,
            )?;
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TabMenuRowLayout {
    pub(crate) action: TabMenuActionId,
    pub(crate) label: &'static str,
    pub(crate) rect: Rect,
    pub(crate) title: Rect,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TabContextMenuLayout {
    pub(crate) panel: Rect,
    pub(crate) rows: Vec<TabMenuRowLayout>,
}

#[must_use]
pub(crate) fn tab_context_menu_layout(
    bounds: Rect,
    anchor: (u32, u32),
    actions: &[crate::frontend::TabMenuActionDescriptor],
    label_width: u32,
) -> Option<TabContextMenuLayout> {
    let rows_height =
        TAB_MENU_ROW_HEIGHT.saturating_mul(u32::try_from(actions.len()).unwrap_or(u32::MAX));
    if bounds.width <= TAB_MENU_SHADOW
        || bounds.height < rows_height.saturating_add(TAB_MENU_SHADOW)
    {
        return None;
    }
    let preferred_width = label_width
        .saturating_add(TAB_MENU_CONTENT_INSET.saturating_mul(2))
        .saturating_add(TAB_MENU_INDICATOR_WIDTH)
        .max(TAB_MENU_WIDTH);
    let panel_width = preferred_width.min(bounds.width.saturating_sub(TAB_MENU_SHADOW));
    let available_height = bounds.height.saturating_sub(TAB_MENU_SHADOW);
    let vertical_padding = TAB_MENU_PADDING.min(available_height.saturating_sub(rows_height) / 2);
    let panel_height = rows_height.saturating_add(vertical_padding.saturating_mul(2));
    let maximum_x = bounds
        .x
        .saturating_add(bounds.width)
        .saturating_sub(panel_width)
        .saturating_sub(TAB_MENU_SHADOW);
    let x = anchor.0.min(maximum_x).max(bounds.x);
    let below = anchor.1.saturating_add(TAB_MENU_ANCHOR_GAP);
    let maximum_y = bounds
        .y
        .saturating_add(bounds.height)
        .saturating_sub(panel_height)
        .saturating_sub(TAB_MENU_SHADOW);
    let y = if below <= maximum_y {
        below.max(bounds.y)
    } else {
        anchor
            .1
            .saturating_sub(panel_height)
            .max(bounds.y)
            .min(maximum_y)
    };
    let panel = Rect {
        x,
        y,
        width: panel_width,
        height: panel_height,
    };
    let horizontal_inset = TAB_MENU_CONTENT_INSET.min(panel_width / 6);
    let indicator_width = TAB_MENU_INDICATOR_WIDTH
        .min(panel_width.saturating_sub(horizontal_inset.saturating_mul(2)));
    let rows = actions
        .iter()
        .enumerate()
        .map(|(index, descriptor)| {
            let rect = Rect {
                x,
                y: y.saturating_add(vertical_padding).saturating_add(
                    TAB_MENU_ROW_HEIGHT.saturating_mul(u32::try_from(index).unwrap_or(u32::MAX)),
                ),
                width: panel_width,
                height: TAB_MENU_ROW_HEIGHT,
            };
            TabMenuRowLayout {
                action: descriptor.id,
                label: descriptor.title,
                rect,
                title: Rect {
                    x: rect
                        .x
                        .saturating_add(horizontal_inset)
                        .saturating_add(indicator_width),
                    y: rect.y,
                    width: rect
                        .width
                        .saturating_sub(horizontal_inset.saturating_mul(2))
                        .saturating_sub(indicator_width),
                    height: rect.height,
                },
            }
        })
        .collect();
    Some(TabContextMenuLayout { panel, rows })
}

#[must_use]
pub(crate) fn tab_context_menu_hit_test(
    layout: &TabContextMenuLayout,
    point: (f64, f64),
) -> Option<TabMenuActionId> {
    layout
        .rows
        .iter()
        .find(|row| contains(row.rect, point))
        .map(|row| row.action)
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one specialized painter keeps tab-menu composition and selection treatment coherent"
)]
pub(crate) fn paint_tab_context_menu(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    canvas: &mut [u8],
    width: u32,
    height: u32,
    scale_120: u32,
    renderer_generation: u64,
    layout: &TabContextMenuLayout,
    palette: SessionPickerPalette,
    state: &TabContextMenuUi,
    pressed: Option<TabMenuActionId>,
    keyboard_focused: bool,
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
    let panel = painter.buffer_rect(layout.panel);
    painter.blend(shadow_rect(panel, scale_120), palette.shadow);
    painter.fill(panel, palette.panel_fill);
    let border = painter.border();
    let frame = if keyboard_focused {
        palette.focused_frame
    } else {
        palette.frame
    };
    paint_outline(&mut painter, panel, border, frame);
    for row in &layout.rows {
        let rect = painter.buffer_rect(row.rect);
        let enabled = state.action_enabled(row.action);
        let selected = enabled && state.selected_action() == row.action;
        if selected {
            paint_selected_row(&mut painter, rect, &palette);
        } else if enabled && state.hovered() == Some(row.action) {
            painter.blend(rect, translucent(palette.selected_fill, ROW_HOVER_ALPHA));
        }
        if enabled && pressed == Some(row.action) {
            painter.blend(rect, translucent(palette.selected_rail, ROW_PRESS_ALPHA));
        }
        let highlighted = enabled
            && !selected
            && (state.hovered() == Some(row.action) || pressed == Some(row.action));
        let primary = if enabled {
            palette.row_text(selected, highlighted).0
        } else {
            palette.secondary
        };
        // Terminate Lair and Terminate Dojo end running terminals.
        let label_color = row_label_color(
            &palette,
            enabled && row.action.destructive(),
            selected,
            highlighted,
            primary,
        );
        if selected {
            let inset = painter.scaled(3).min(rect.width);
            let indicator = Rect {
                x: rect.x.saturating_add(inset),
                width: painter
                    .scaled(TAB_MENU_INDICATOR_WIDTH)
                    .min(rect.width.saturating_sub(inset)),
                ..rect
            };
            painter.text(
                "›",
                ChromeTextStyle::Bold,
                indicator,
                PickerTextAlignment::Left,
                primary,
            )?;
        }
        let clip = painter.buffer_rect(row.title);
        painter.text(
            row.label,
            if selected {
                ChromeTextStyle::Bold
            } else {
                ChromeTextStyle::Regular
            },
            clip,
            PickerTextAlignment::Left,
            label_color,
        )?;
    }
    // Rows reach the panel's right edge; draw it over them.
    painter.fill(
        Rect {
            x: panel.x.saturating_add(panel.width.saturating_sub(border)),
            width: border,
            ..panel
        },
        opaque(frame),
    );
    painter.finish();
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DojoPromptLayout {
    pub(crate) panel: Rect,
    pub(crate) title: Rect,
    pub(crate) body: Rect,
    pub(crate) input: Option<Rect>,
    pub(crate) cancel: Option<Rect>,
    pub(crate) terminate: Option<Rect>,
}

#[must_use]
#[allow(clippy::too_many_lines, reason = "shared bounded prompt geometry")]
pub(crate) fn dojo_prompt_layout(content: Rect, state: &DojoPromptUi) -> Option<DojoPromptLayout> {
    if content.width == 0 || content.height == 0 {
        return None;
    }
    let margin = PROMPT_MARGIN.min(content.width / 4).min(content.height / 4);
    let width = PROMPT_MAX_WIDTH.min(content.width.saturating_sub(margin.saturating_mul(2)));
    let controls_height = if state.is_rename() {
        PROMPT_INPUT_HEIGHT
    } else {
        PROMPT_BUTTON_HEIGHT
    };
    let body_height = if state.uses_layout_summary() {
        PREVIEW_BODY_HEIGHT
    } else {
        PROMPT_BODY_HEIGHT
    };
    let height = PROMPT_PADDING
        .saturating_mul(2)
        .saturating_add(PROMPT_TITLE_HEIGHT)
        .saturating_add(body_height)
        .saturating_add(PROMPT_GAP.saturating_mul(2))
        .saturating_add(controls_height);
    if width < 180 || content.height < height.saturating_add(margin.saturating_mul(2)) {
        return None;
    }
    let panel = Rect {
        x: content
            .x
            .saturating_add(content.width.saturating_sub(width) / 2),
        y: content
            .y
            .saturating_add(content.height.saturating_sub(height) / 2),
        width,
        height,
    };
    let inner_width = width.saturating_sub(PROMPT_PADDING.saturating_mul(2));
    let title = Rect {
        x: panel.x.saturating_add(PROMPT_PADDING),
        y: panel.y.saturating_add(PROMPT_PADDING),
        width: inner_width,
        height: PROMPT_TITLE_HEIGHT,
    };
    let body = Rect {
        x: title.x,
        y: title
            .y
            .saturating_add(title.height)
            .saturating_add(PROMPT_GAP),
        width: inner_width,
        height: body_height,
    };
    let controls_y = body
        .y
        .saturating_add(body.height)
        .saturating_add(PROMPT_GAP);
    let (input, cancel, terminate) = if state.is_rename() {
        (
            Some(Rect {
                x: title.x,
                y: controls_y,
                width: inner_width,
                height: PROMPT_INPUT_HEIGHT,
            }),
            None,
            None,
        )
    } else if state.is_preview() {
        (
            None,
            Some(Rect {
                x: title.x,
                y: controls_y,
                width: inner_width,
                height: PROMPT_BUTTON_HEIGHT,
            }),
            None,
        )
    } else {
        let button_width = inner_width.saturating_sub(PROMPT_GAP) / 2;
        (
            None,
            Some(Rect {
                x: title.x,
                y: controls_y,
                width: button_width,
                height: PROMPT_BUTTON_HEIGHT,
            }),
            Some(Rect {
                x: title
                    .x
                    .saturating_add(button_width)
                    .saturating_add(PROMPT_GAP),
                y: controls_y,
                width: button_width,
                height: PROMPT_BUTTON_HEIGHT,
            }),
        )
    };
    Some(DojoPromptLayout {
        panel,
        title,
        body,
        input,
        cancel,
        terminate,
    })
}

#[must_use]
pub(crate) fn dojo_prompt_hit_test(
    layout: &DojoPromptLayout,
    point: (f64, f64),
) -> Option<TerminationDecision> {
    if layout.cancel.is_some_and(|rect| contains(rect, point)) {
        Some(TerminationDecision::Cancel)
    } else if layout.terminate.is_some_and(|rect| contains(rect, point)) {
        Some(TerminationDecision::Terminate)
    } else {
        None
    }
}

/// A menu row's label: a destructive action that can run, such as
/// Terminate, in danger; any other row in `primary`.
fn row_label_color(
    palette: &SessionPickerPalette,
    destructive: bool,
    selected: bool,
    highlighted: bool,
    primary: u32,
) -> u32 {
    if destructive {
        palette.row_danger(selected, highlighted)
    } else {
        primary
    }
}

/// A Dojo prompt choice's label. The confirming choice of a prompt that
/// ends running terminals, Terminate a Dojo or a Lair, is drawn in danger,
/// selected or not; another prompt's, Restore or Revoke, in the accent
/// until it is selected, as before.
fn dojo_prompt_choice_color(
    palette: &SessionPickerPalette,
    state: &DojoPromptUi,
    decision: TerminationDecision,
    selected: bool,
) -> u32 {
    let confirming = decision == TerminationDecision::Terminate;
    match (confirming && state.confirmation_destructive(), selected) {
        (true, true) => palette.selected_danger,
        (true, false) => palette.danger,
        (false, true) => palette.selected_primary,
        (false, false) if confirming => palette.selected_rail,
        (false, false) => palette.primary,
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "trusted Dojo prompts share one bounded painter and composition transaction"
)]
pub(crate) fn paint_dojo_prompt(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    canvas: &mut [u8],
    width: u32,
    height: u32,
    content_rect: Rect,
    scale_120: u32,
    renderer_generation: u64,
    layout: &DojoPromptLayout,
    palette: SessionPickerPalette,
    state: &DojoPromptUi,
    keyboard_focused: bool,
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
    let panel = painter.buffer_rect(layout.panel);
    painter.fill(panel, palette.panel_fill);
    let border = painter.border();
    paint_outline(
        &mut painter,
        panel,
        border,
        if keyboard_focused {
            palette.focused_frame
        } else {
            palette.frame
        },
    );
    let (title, body) = state.title_and_body();
    let title_rect = painter.buffer_rect(layout.title);
    painter.text(
        title,
        ChromeTextStyle::Bold,
        title_rect,
        PickerTextAlignment::Left,
        palette.primary,
    )?;
    let body_rect = painter.buffer_rect(layout.body);
    if state.uses_layout_summary() {
        let line_height = painter.scaled(PREVIEW_LINE_HEIGHT);
        let (cell_width, _) = painter.cell(ChromeTextStyle::Regular)?;
        let maximum_cells = usize::try_from(body_rect.width / cell_width)
            .unwrap_or(MAX_PREVIEW_LINE_SCALARS)
            .min(MAX_PREVIEW_LINE_SCALARS);
        for (index, line) in wrap_preview_body(&body, maximum_cells).iter().enumerate() {
            let Some(line_rect) = preview_line_rect(body_rect, index, line_height) else {
                break;
            };
            painter.text(
                line,
                ChromeTextStyle::Regular,
                line_rect,
                PickerTextAlignment::Left,
                palette.secondary,
            )?;
        }
    } else {
        painter.text(
            &body,
            ChromeTextStyle::Regular,
            body_rect,
            PickerTextAlignment::Left,
            palette.secondary,
        )?;
    }
    if let Some(prompt_input) = state.input() {
        let input = painter.buffer_rect(layout.input.expect("rename input exists"));
        painter.fill(input, opaque(palette.selected_fill));
        let inset = painter.scaled(12);
        let input_clip = Rect {
            x: input.x.saturating_add(inset),
            width: input.width.saturating_sub(inset.saturating_mul(2)),
            ..input
        };
        let field = editable_field_text(&mut painter, "> ", prompt_input, input_clip.width)?;
        painter.text(
            &field,
            ChromeTextStyle::Regular,
            input_clip,
            PickerTextAlignment::Left,
            palette.selected_primary,
        )?;
    } else if state.is_preview() {
        let rect = painter.buffer_rect(layout.cancel.expect("preview close exists"));
        painter.fill(rect, opaque(palette.selected_fill));
        painter.text(
            "Close",
            ChromeTextStyle::Bold,
            rect,
            PickerTextAlignment::Left,
            palette.selected_primary,
        )?;
    } else {
        let selected_decision = state.decision().expect("confirmation decision exists");
        let confirmation_label = state
            .confirmation_label()
            .expect("confirmation label exists");
        for (decision, rect, label) in [
            (
                TerminationDecision::Cancel,
                layout.cancel.expect("cancel exists"),
                "Cancel",
            ),
            (
                TerminationDecision::Terminate,
                layout.terminate.expect("terminate exists"),
                confirmation_label,
            ),
        ] {
            let rect = painter.buffer_rect(rect);
            let selected = selected_decision == decision;
            painter.fill(
                rect,
                if selected {
                    opaque(palette.selected_fill)
                } else {
                    palette.panel_fill
                },
            );
            painter.text(
                label,
                if selected {
                    ChromeTextStyle::Bold
                } else {
                    ChromeTextStyle::Regular
                },
                rect,
                PickerTextAlignment::Left,
                dojo_prompt_choice_color(&palette, state, decision, selected),
            )?;
        }
    }
    painter.finish();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        super::{
            super::{ChromeText, text::chrome_maximum_cell_height},
            chrome_box::{BOX_LABEL_INSET, truncate_text_start},
            picker::picker_buffer_rect as buffer_rect,
        },
        *,
    };
    use crate::{
        config::ResolvedTheme,
        frontend::{CommandPaletteContext, CommandTabMoveAvailability},
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
        keep_end: bool,
    ) {
        let mut canvas = vec![0_u8; 4];
        let mut painter = ChromePainter::new(cache, context, &mut canvas, 1, 1, scale, 1);
        let clip = Rect {
            x: 0,
            y: 0,
            width: clip_width,
            height: 1,
        };
        if keep_end {
            painter.text_keep_end(source, style, clip, 0).unwrap();
        } else {
            painter
                .text(source, style, clip, PickerTextAlignment::Left, 0)
                .unwrap();
        }
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
    fn preview_body_wraps_to_available_cells_and_marks_bounded_overflow() {
        let body = "Not restored: terminal/scrollback bodies, process memory, shell state, environment, clipboard, images";
        let wrapped = wrap_preview_body(body, 48);
        assert!(wrapped.len() > 1);
        assert!(wrapped.iter().all(|line| preview_text_width(line) <= 48));
        assert_eq!(wrapped.concat().replace(' ', ""), body.replace(' ', ""));

        let overflow = wrap_preview_body(&format!("{body}\n").repeat(8), 24);
        assert_eq!(overflow.len(), MAX_PREVIEW_LINES);
        assert!(overflow.last().is_some_and(|line| line.ends_with('…')));
        assert!(overflow.iter().all(|line| preview_text_width(line) <= 24));

        let wide_indentation = wrap_preview_body("\u{3000}\u{3000}wide words here", 8);
        assert!(
            wide_indentation
                .iter()
                .all(|line| preview_text_width(line) <= 8)
        );
    }

    #[test]
    fn fractional_preview_line_rects_remain_inside_the_buffer_body() {
        let body = buffer_rect(
            Rect {
                x: 24,
                y: 36,
                width: 472,
                height: PREVIEW_BODY_HEIGHT,
            },
            121,
        );
        let line_height = PREVIEW_LINE_HEIGHT.saturating_mul(121).div_ceil(120);
        for index in 0..MAX_PREVIEW_LINES {
            let line = preview_line_rect(body, index, line_height).unwrap();
            assert!(line.y >= body.y);
            assert!(line.y.saturating_add(line.height) <= body.y.saturating_add(body.height));
        }
    }

    #[test]
    fn layout_is_bounded_selected_visible_and_hit_targets_are_half_open() {
        let commands = vec![
            BuiltInCommandId::NewDojo,
            BuiltInCommandId::SplitHorizontal,
            BuiltInCommandId::SplitVertical,
        ];
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
            let layout =
                command_palette_layout(content, &commands, 2, 0, PaletteShape::Commands).unwrap();
            assert!(layout.panel.x >= content.x && layout.panel.y >= content.y);
            assert!(layout.panel.x + layout.panel.width <= content.x + content.width);
            assert!(layout.panel.y + layout.panel.height <= content.y + content.height);
            assert!(layout.rows.iter().any(|row| row.command == commands[2]));
            for row in &layout.rows {
                assert_eq!(row.rect.height, ROW_HEIGHT - ROW_GUTTER_Y * 2);
                // Rows fill the inside of the frame.
                assert_eq!(row.rect.x, layout.panel.x + BOX_EDGE);
                assert_eq!(
                    row.rect.x + row.rect.width + BOX_EDGE,
                    layout.panel.x + layout.panel.width
                );
                assert!(row.rect.y >= layout.rule.y + layout.rule.height);
                assert!(row.title.x >= row.rect.x + CONTENT_INSET);
                assert_eq!(row.category.width == 0, layout.compact);
                if !layout.compact {
                    assert!(row.title.x > row.category.x + row.category.width);
                    assert_eq!(row.shortcut.width, (row.rect.width / 3).min(180));
                    assert!(row.shortcut.width > 60);
                }
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
            command_palette_layout(content, &commands, 0, 0, PaletteShape::Commands).unwrap();
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
            command_palette_layout(content, &commands, 0, 0, PaletteShape::AgentAccess).is_none()
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
                let layout = command_palette_layout(content, commands, 0, 0, shape).unwrap();
                let panel = layout.panel;
                assert_eq!(panel.width, (width - 48).min(maximum), "{width}x{height}");
                assert_eq!(panel.x - content.x, (width - panel.width) / 2);
                let top = (height * 15 / 100).max(48);
                let lowest = content.y + height - 16 - panel.height;
                assert_eq!(panel.y, (content.y + top).min(lowest), "{width}x{height}");
                assert!(panel.y >= content.y + 16);
                assert_eq!(layout.compact, panel.width < 420);
                assert_eq!(layout.detail.is_some(), shape == PaletteShape::AgentAccess);
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
            command_palette_layout(content, commands, 0, 0, PaletteShape::Commands).unwrap();
        assert_eq!(layout.panel.y, 150);
        assert_eq!(layout.rows.len(), COMMAND_PALETTE_PAGE_ITEMS);
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
        let layout =
            command_palette_layout(content, BuiltInCommandId::ALL, 0, 0, PaletteShape::Commands)
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
                    right_label: "3/62",
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
        assert_eq!(PaletteBody::Commands.count_label(&state), total.to_string());
        assert!(state.append_text("split"));
        let shown = state.filtered().len();
        assert!(shown > 0 && shown < total);
        assert_eq!(
            PaletteBody::Commands.count_label(&state),
            format!("{shown}/{total}")
        );
        let keymap = ResolvedKeymap::default();
        let help = BindingHelpUi::new(&keymap);
        assert_eq!(
            PaletteBody::BindingHelp(&help).count_label(&state),
            help.total().to_string()
        );
        let mut snapshot = crate::frontend::AccessSnapshot::new(
            3,
            splinterm_protocol::RememberedStoreStatus::Available,
        );
        snapshot.entries = vec![crate::frontend::agent_access_test_entry(LairId::new())];
        let access = AgentAccessUi::new(Some(&snapshot), None);
        let label = PaletteBody::AgentAccess(&access).count_label(&state);
        assert!(label.starts_with(&access.total().to_string()), "{label}");
        if !access.status_label().is_empty() {
            assert!(label.ends_with(&format!(" · {}", access.status_label())));
        }
    }

    #[test]
    fn empty_results_keep_a_visible_inert_list_row() {
        let content = Rect {
            x: 0,
            y: 34,
            width: 640,
            height: 400,
        };
        let layout = command_palette_layout(content, &[], 0, 0, PaletteShape::Commands).unwrap();
        assert!(layout.rows.is_empty());
        assert_eq!(layout.list.height, ROW_HEIGHT);
        assert_eq!(command_palette_hit_test(&layout, (10.0, 100.0)), None);
    }

    #[test]
    fn lair_context_menu_layout_uses_the_available_action_catalog() {
        let actions = [
            TabMenuActionId::RenameLair,
            TabMenuActionId::NewDojo,
            TabMenuActionId::PinLair,
            TabMenuActionId::PreviewLair,
            TabMenuActionId::TerminateLair,
        ]
        .map(tab_menu_descriptor);
        let bounds = Rect {
            x: 0,
            y: 0,
            width: 640,
            height: 400,
        };
        let layout = tab_context_menu_layout(bounds, (639, 399), &actions, 0).unwrap();
        assert_eq!(layout.rows.len(), actions.len());
        assert_eq!(layout.rows[0].action, TabMenuActionId::RenameLair);
        assert_eq!(
            layout.panel.height,
            TAB_MENU_ROW_HEIGHT * u32::try_from(actions.len()).unwrap() + TAB_MENU_PADDING * 2
        );
        let row = layout.rows[0].rect;
        assert_eq!(
            tab_context_menu_hit_test(&layout, (f64::from(row.x), f64::from(row.y))),
            Some(TabMenuActionId::RenameLair)
        );
        assert_eq!(
            tab_context_menu_hit_test(&layout, (f64::from(row.x), f64::from(row.y + row.height))),
            Some(actions[1].id)
        );
    }

    #[test]
    fn context_menu_layout_retains_explorer_label_instead_of_tab_label() {
        let descriptor = crate::frontend::TabMenuActionDescriptor {
            id: TabMenuActionId::ActivateTab,
            title: "Open / Activate",
        };
        let bounds = Rect {
            x: 0,
            y: 0,
            width: 640,
            height: 400,
        };
        let layout = tab_context_menu_layout(bounds, (0, 0), &[descriptor], 0).unwrap();
        assert_eq!(layout.rows[0].label, "Open / Activate");
        assert_ne!(
            layout.rows[0].label,
            tab_menu_descriptor(descriptor.id).title
        );
    }

    #[test]
    fn context_menu_full_action_labels_fit_at_supported_scales() {
        let context = RenderContext::new(u16::MAX);
        let mut cache = SessionPickerTextCache::default();
        let actions = TAB_MENU_ACTIONS.into_iter().chain(
            [
                TabMenuActionId::RenameLair,
                TabMenuActionId::SaveLayout,
                TabMenuActionId::PinLair,
                TabMenuActionId::UnpinLair,
                TabMenuActionId::PreviewLair,
                TabMenuActionId::Restore,
                TabMenuActionId::TerminateLair,
                TabMenuActionId::FocusSplint,
                TabMenuActionId::SplitBelow,
                TabMenuActionId::SplitRight,
                TabMenuActionId::CloseSplint,
            ]
            .map(tab_menu_descriptor),
        );
        for action in actions {
            for width in [320, 640] {
                let bounds = Rect {
                    x: 17,
                    y: 23,
                    width,
                    height: 400,
                };
                for scale in [120, 150, 180, 240] {
                    let label_width =
                        tab_menu_label_width(&mut cache, &context, &[action], scale, 1).unwrap();
                    let layout = tab_context_menu_layout(
                        bounds,
                        (bounds.x + width, 400),
                        &[action],
                        label_width,
                    )
                    .unwrap();
                    let clip = buffer_rect(layout.rows[0].title, scale);
                    for style in [ChromeTextStyle::Regular, ChromeTextStyle::Bold] {
                        let text_width = text_width(&context, action.title, style, scale);
                        assert!(
                            text_width <= clip.width,
                            "{} at width {width}, scale {scale}: {text_width} > {}",
                            action.title,
                            clip.width
                        );
                    }
                }
            }
        }
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
                false,
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
    fn editable_fields_keep_their_caret_and_shortcuts_keep_their_key() {
        assert_eq!(truncate_text_start("Ctrl+Shift+P", 7), "…hift+P");
        assert_eq!(editable_text("> ", "dojo", 20), "> dojo_");
        assert_eq!(editable_text("> ", "my-long-dojo-name", 12), "> …ojo-name_");

        let context = RenderContext::new(u16::MAX);
        let mut cache = SessionPickerTextCache::default();
        for scale in [120, 150, 180, 240] {
            let (cell, _) = cache
                .cell_metrics(&context, ChromeTextStyle::Regular, scale, 1)
                .unwrap();
            let clip = cell * 12;
            let mut canvas = vec![0_u8; 4];
            let mut painter = ChromePainter::new(&mut cache, &context, &mut canvas, 1, 1, scale, 1);
            let field = editable_field_text(&mut painter, "> ", "my-long-dojo-name", clip).unwrap();
            assert_eq!(field, "> …ojo-name_", "scale {scale}");
            let short = editable_field_text(&mut painter, "> ", "dojo", clip).unwrap();
            assert_eq!(short, "> dojo_");
            painter.finish();
            let shortcut = "Ctrl+Shift+PageDown";
            paint_label(
                &mut cache,
                &context,
                shortcut,
                ChromeTextStyle::Regular,
                scale,
                cell * 10,
                true,
            );
            assert!(cache.contains_source("…+PageDown"), "scale {scale}");
        }
    }

    /// VM case E6: at maximum zoom "Preview saved layout" was clipped into
    /// fragments. It must fit its menu row whole or end in an ellipsis.
    /// Chrome no longer follows zoom, so the largest UI text size now
    /// stands in for it.
    #[test]
    fn maximum_zoom_menu_labels_fit_their_rows_or_end_in_an_ellipsis() {
        use super::super::super::text::CHROME_MAXIMUM_LINE_HEIGHT;

        for height in [
            TAB_MENU_ROW_HEIGHT,
            ROW_HEIGHT - ROW_GUTTER_Y * 2,
            INPUT_HEIGHT,
            DETAIL_HEIGHT,
            PREVIEW_LINE_HEIGHT,
            PROMPT_TITLE_HEIGHT,
            PROMPT_INPUT_HEIGHT,
            PROMPT_BUTTON_HEIGHT,
        ] {
            assert!(height >= CHROME_MAXIMUM_LINE_HEIGHT);
        }
        let action = tab_menu_descriptor(TabMenuActionId::PreviewLair);
        for scale in [120, 150, 180, 240] {
            let context = RenderContext::new(u16::MAX)
                .at_maximum_zoom(scale)
                .at_maximum_chrome_font_size();
            let mut cache = SessionPickerTextCache::default();
            for width in [120, 160, 240, 640, 1268] {
                let bounds = Rect {
                    x: 0,
                    y: 0,
                    width,
                    height: 754,
                };
                let label_width =
                    tab_menu_label_width(&mut cache, &context, &[action], scale, 1).unwrap();
                let layout =
                    tab_context_menu_layout(bounds, (width / 2, 300), &[action], label_width)
                        .unwrap();
                let row = buffer_rect(layout.rows[0].rect, scale);
                let clip = buffer_rect(layout.rows[0].title, scale);
                for style in [ChromeTextStyle::Regular, ChromeTextStyle::Bold] {
                    let full_width = text_width(&context, action.title, style, scale);
                    paint_label(
                        &mut cache,
                        &context,
                        action.title,
                        style,
                        scale,
                        clip.width,
                        false,
                    );
                    let (drawn, _) = cache
                        .drawn()
                        .find(|(text, drawn_style)| {
                            *drawn_style == style
                                && action.title.starts_with(text.trim_end_matches('…'))
                                && text_width(&context, text, style, scale) <= clip.width
                        })
                        .unwrap_or_else(|| panic!("scale {scale}, width {width}: no label"));
                    let drawn = ChromeText::load_styled_with_context(drawn, scale, style, &context)
                        .unwrap();
                    assert!(drawn.pixel_height() <= chrome_maximum_cell_height(scale));
                    assert!(
                        drawn.pixel_height() <= row.height,
                        "scale {scale}, width {width}: {} > {}",
                        drawn.pixel_height(),
                        row.height
                    );
                    if full_width > clip.width {
                        assert!(
                            cache.drawn().any(|(text, drawn_style)| {
                                drawn_style == style
                                    && text.ends_with('…')
                                    && action.title.starts_with(text.trim_end_matches('…'))
                            }),
                            "scale {scale}, width {width}: no ellipsized label"
                        );
                    }
                }
            }
            // A wide Window shows the whole label at the largest size.
            let label_width =
                tab_menu_label_width(&mut cache, &context, &[action], scale, 1).unwrap();
            let bounds = Rect {
                x: 0,
                y: 0,
                width: 1268,
                height: 754,
            };
            let layout =
                tab_context_menu_layout(bounds, (600, 300), &[action], label_width).unwrap();
            let clip = buffer_rect(layout.rows[0].title, scale);
            assert!(
                text_width(&context, action.title, ChromeTextStyle::Bold, scale) <= clip.width,
                "scale {scale}"
            );
        }
    }

    #[test]
    fn context_menu_narrow_viewports_bound_labels_and_action_hits() {
        for width in [1, 6, 7, 12, 40, 156, 223, 224, 230, 240, 640] {
            let bounds = Rect {
                x: 17,
                y: 23,
                width,
                height: 400,
            };
            for anchor in [(0, 0), (bounds.x + width, bounds.y + bounds.height)] {
                let Some(layout) =
                    tab_context_menu_layout(bounds, anchor, &TAB_MENU_ACTIONS, u32::MAX)
                else {
                    assert!(width <= TAB_MENU_SHADOW);
                    continue;
                };
                assert!(layout.panel.x >= bounds.x);
                assert!(layout.panel.x + layout.panel.width + TAB_MENU_SHADOW <= bounds.x + width);
                for row in &layout.rows {
                    assert!(row.title.x >= row.rect.x);
                    assert!(row.title.x + row.title.width <= row.rect.x + row.rect.width);
                    let y = f64::from(row.rect.y);
                    assert_eq!(
                        tab_context_menu_hit_test(&layout, (f64::from(row.rect.x), y)),
                        Some(row.action)
                    );
                    assert_eq!(
                        tab_context_menu_hit_test(
                            &layout,
                            (f64::from(row.rect.x + row.rect.width), y)
                        ),
                        None
                    );
                }
            }
        }
    }

    #[test]
    fn tab_context_menu_clamps_all_edges_and_keeps_half_open_rows() {
        let bounds = Rect {
            x: 0,
            y: 0,
            width: 640,
            height: 400,
        };
        for anchor in [(0, 0), (639, 0), (0, 399), (639, 399)] {
            let layout = tab_context_menu_layout(bounds, anchor, &TAB_MENU_ACTIONS, 0).unwrap();
            assert_eq!(layout.panel.width, TAB_MENU_WIDTH);
            assert_eq!(
                layout.panel.height,
                TAB_MENU_ROW_HEIGHT * u32::try_from(TAB_MENU_ACTIONS.len()).unwrap()
                    + TAB_MENU_PADDING * 2
            );
            assert!(layout.panel.x >= bounds.x && layout.panel.y >= bounds.y);
            assert!(
                layout.panel.x + layout.panel.width + TAB_MENU_SHADOW <= bounds.x + bounds.width
            );
            assert!(
                layout.panel.y + layout.panel.height + TAB_MENU_SHADOW <= bounds.y + bounds.height
            );
            assert_eq!(layout.rows.len(), TAB_MENU_ACTIONS.len());
            for row in &layout.rows {
                assert_eq!(row.rect.height, TAB_MENU_ROW_HEIGHT);
                assert_eq!(
                    row.title.x - row.rect.x,
                    TAB_MENU_CONTENT_INSET + TAB_MENU_INDICATOR_WIDTH
                );
                assert_eq!(
                    tab_context_menu_hit_test(
                        &layout,
                        (f64::from(row.rect.x), f64::from(row.rect.y))
                    ),
                    Some(row.action)
                );
                assert_ne!(
                    tab_context_menu_hit_test(
                        &layout,
                        (
                            f64::from(row.rect.x + row.rect.width),
                            f64::from(row.rect.y)
                        )
                    ),
                    Some(row.action)
                );
            }
            assert!(layout.rows[0].rect.y + layout.rows[0].rect.height <= layout.rows[1].rect.y);
        }
    }

    #[test]
    #[allow(clippy::too_many_lines, reason = "one shared prompt paint scenario")]
    fn dojo_prompts_are_bounded_default_cancel_and_half_open() {
        let content = Rect {
            x: 0,
            y: 34,
            width: 640,
            height: 366,
        };
        let rename = DojoPromptUi::rename(
            DojoId::new(),
            "captured".to_owned(),
            2,
            vec![(SplintId::new(), 1), (SplintId::new(), 2)],
        );
        let rename_layout = dojo_prompt_layout(content, &rename).unwrap();
        // The Dojo may be renamed from the Explorer without being open as a tab.
        assert_eq!(rename.title_and_body().0, "RENAME DOJO");
        assert!(rename_layout.input.is_some());
        assert!(rename_layout.cancel.is_none());
        assert!(contains(
            content,
            (
                f64::from(rename_layout.panel.x),
                f64::from(rename_layout.panel.y)
            )
        ));

        let terminate = DojoPromptUi::terminate(
            DojoId::new(),
            "captured".to_owned(),
            3,
            vec![
                (SplintId::new(), 1),
                (SplintId::new(), 2),
                (SplintId::new(), 3),
            ],
        );
        let layout = dojo_prompt_layout(content, &terminate).unwrap();
        let cancel = layout.cancel.unwrap();
        let destructive = layout.terminate.unwrap();
        assert_eq!(
            dojo_prompt_hit_test(&layout, (f64::from(cancel.x), f64::from(cancel.y))),
            Some(TerminationDecision::Cancel)
        );
        assert_eq!(
            dojo_prompt_hit_test(
                &layout,
                (
                    f64::from(destructive.x + destructive.width),
                    f64::from(destructive.y)
                )
            ),
            None
        );
        let mut canvas = vec![0_u8; 640 * 400 * 4];
        let mut cache = SessionPickerTextCache::default();
        paint_dojo_prompt(
            &mut cache,
            &RenderContext::new(u16::MAX),
            &mut canvas,
            640,
            400,
            content,
            120,
            1,
            &layout,
            session_picker_palette(ResolvedTheme::default()),
            &terminate,
            true,
        )
        .unwrap();
        assert!(canvas.iter().any(|byte| *byte != 0));
        assert!(cache.len() <= 4);
        let DojoPromptUi::Terminate(confirmation) = terminate else {
            unreachable!();
        };
        assert_eq!(confirmation.decision(), TerminationDecision::Cancel);

        let target = crate::frontend::LairPromptTarget {
            topology_revision: splinterm_core::TopologyRevision::new(9),
            lair_id: LairId::new(),
            dojo_id: None,
            name: "saved".to_owned(),
            retention: splinterm_core::LairRetention::Saved,
            preview: format!(
                "Lair: saved\nDojo: shell\n  Horizontal split 758/1000\n    shell — Application: /usr/bin/bash — /home/oldjobobo/Projects/splinterm\n    shell — Shell — /home/oldjobobo/Projects/splinterm\n  shell — Shell — /home/oldjobobo/Projects/splinterm\nNot restored: {}",
                "terminal/scrollback bodies, process memory, shell state, environment, clipboard, images".repeat(4)
            ),
            targets: Vec::new(),
        };
        let preview = DojoPromptUi::preview_lair(target.clone());
        let preview_layout = dojo_prompt_layout(content, &preview).unwrap();
        assert!(preview_layout.cancel.is_some());
        assert!(preview_layout.terminate.is_none());
        assert_eq!(preview_layout.body.height, PREVIEW_BODY_HEIGHT);
        canvas.fill(0);
        paint_dojo_prompt(
            &mut cache,
            &RenderContext::new(u16::MAX),
            &mut canvas,
            640,
            400,
            content,
            120,
            1,
            &preview_layout,
            session_picker_palette(ResolvedTheme::default()),
            &preview,
            true,
        )
        .unwrap();
        assert!(canvas.iter().any(|byte| *byte != 0));

        let restore = DojoPromptUi::restore_lair(target);
        let restore_layout = dojo_prompt_layout(content, &restore).unwrap();
        assert!(restore_layout.cancel.is_some());
        assert!(restore_layout.terminate.is_some());
        assert_eq!(restore_layout.body.height, PREVIEW_BODY_HEIGHT);
        assert!(
            restore
                .title_and_body()
                .1
                .starts_with("Restore 0 captured Splints?")
        );
        assert_eq!(restore.confirmation_label(), Some("Restore"));
        canvas.fill(0);
        paint_dojo_prompt(
            &mut cache,
            &RenderContext::new(u16::MAX),
            &mut canvas,
            640,
            400,
            content,
            120,
            1,
            &restore_layout,
            session_picker_palette(ResolvedTheme::default()),
            &restore,
            true,
        )
        .unwrap();
        assert!(canvas.iter().any(|byte| *byte != 0));
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "bounded paint, cache and narrow viewport scenario"
    )]
    fn tab_context_menu_paints_without_a_window_scrim() {
        let bounds = Rect {
            x: 0,
            y: 0,
            width: 640,
            height: 400,
        };
        let layout = tab_context_menu_layout(bounds, (240, 40), &TAB_MENU_ACTIONS, 0).unwrap();
        let mut state = TabContextMenuUi::new(crate::frontend::TabMenuContext {
            lair_id: LairId::new(),
            source_splint_id: SplintId::new(),
            dojo_id: DojoId::new(),
            dojo_name: "test".to_owned(),
            pane_count: 2,
            splints: vec![(SplintId::new(), 1), (SplintId::new(), 2)],
            active: false,
            other_dojo_ids: vec![DojoId::new()],
        });
        let mut canvas = vec![0_u8; 640 * 400 * 4];
        let mut cache = SessionPickerTextCache::default();
        let right_x = layout.panel.x + layout.panel.width - 1;
        let top_edge = usize::try_from((layout.panel.y * 640 + right_x) * 4).unwrap();
        let close_row = layout
            .rows
            .iter()
            .position(|row| row.action == TabMenuActionId::CloseTab)
            .unwrap();
        for (hovered, pressed, row) in [
            (None, None, 0),
            (Some(TabMenuActionId::CloseTab), None, close_row),
            (
                Some(TabMenuActionId::CloseTab),
                Some(TabMenuActionId::CloseTab),
                close_row,
            ),
        ] {
            state.update_hovered(hovered);
            canvas.fill(0);
            paint_tab_context_menu(
                &mut cache,
                &RenderContext::new(u16::MAX),
                &mut canvas,
                640,
                400,
                120,
                1,
                &layout,
                session_picker_palette(ResolvedTheme::default()),
                &state,
                pressed,
                true,
            )
            .unwrap();
            assert_eq!(&canvas[..4], &[0, 0, 0, 0]);
            let panel_offset =
                usize::try_from((layout.panel.y * 640 + layout.panel.x) * 4).unwrap();
            assert_ne!(&canvas[panel_offset..panel_offset + 4], &[0, 0, 0, 0]);
            let row_y = layout.rows[row].rect.y + layout.rows[row].rect.height / 2;
            let row_edge = usize::try_from((row_y * 640 + right_x) * 4).unwrap();
            assert_eq!(
                &canvas[row_edge..row_edge + 4],
                &canvas[top_edge..top_edge + 4]
            );
        }
        // Repaints reuse entries: each label at most once whole and once
        // ellipsized to a row too narrow for it.
        assert!(cache.len() <= TAB_MENU_ACTIONS.len() * 2 + 1);
        for viewport_width in [7, 12, 40, 230] {
            let narrow = Rect {
                width: viewport_width,
                ..bounds
            };
            let layout = tab_context_menu_layout(narrow, (0, 0), &TAB_MENU_ACTIONS, 260).unwrap();
            canvas.fill(0);
            paint_tab_context_menu(
                &mut cache,
                &RenderContext::new(u16::MAX),
                &mut canvas,
                640,
                400,
                120,
                1,
                &layout,
                session_picker_palette(ResolvedTheme::default()),
                &state,
                None,
                true,
            )
            .unwrap();
            for y in 0..400 {
                let outside = (y * 640 + viewport_width as usize) * 4;
                let end = (y + 1) * 640 * 4;
                assert!(canvas[outside..end].iter().all(|byte| *byte == 0));
            }
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
        for (key, verb) in PaletteBody::Commands.legend() {
            assert!(cache.contains_source(key) && cache.contains_source(verb));
        }
        paint(&mut cache, &mut canvas, &layout, PaletteBody::Commands);
        assert_eq!(cache.len(), shaped);
        let mut help = BindingHelpUi::new(&keymap);
        let help_layout = command_palette_layout(
            content,
            &vec![BuiltInCommandId::RecentSessions; help.rows().len()],
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
        assert!(cache.contains_source("search key bindings"));
        assert!(cache.contains_source("key bindings"));
        assert!(cache.contains_source("pgup pgdn"));

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
        assert!(cache.contains_source("ALWAYS"));
        assert!(cache.contains_source("not connected"));
        // The detail line, whole or ending in `…`.
        let footer = access.footer();
        assert!(cache.drawn().any(|(text, _)| text == footer
            || (text.ends_with('…') && footer.starts_with(text.trim_end_matches('…')))));
        assert!(cache.contains_source(&PaletteBody::AgentAccess(&access).count_label(&state)));

        assert!(help.append_text("definitely absent xyz"));
        let empty_layout =
            command_palette_layout(content, &[], 0, 0, PaletteShape::BindingHelp).unwrap();
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
        assert!(!cache.contains_source("search key bindings"));
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

    /// Terminate is drawn in danger in the command palette, the tab menu and
    /// the Dojo prompt; every other row and choice keeps its colour, and
    /// Restore and Revoke confirm in the accent, as before.
    #[test]
    fn only_terminate_rows_and_choices_are_drawn_in_danger() {
        let palette = session_picker_palette(ResolvedTheme::default());
        assert_ne!(palette.danger, palette.primary);
        for command in BuiltInCommandId::ALL.iter().copied() {
            let destructive = matches!(
                command,
                BuiltInCommandId::TerminateCurrentDojo | BuiltInCommandId::TerminateCurrentLair
            );
            assert_eq!(command.destructive(), destructive, "{command:?}");
            for (selected, highlighted) in [(false, false), (false, true), (true, false)] {
                let painted = row_label_color(
                    &palette,
                    command.destructive(),
                    selected,
                    highlighted,
                    palette.primary,
                );
                let expected = if destructive {
                    palette.row_danger(selected, highlighted)
                } else {
                    palette.primary
                };
                assert_eq!(painted, expected, "{command:?}");
            }
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
        // A disabled Terminate row is not drawn in danger.
        assert_eq!(
            row_label_color(&palette, false, false, false, palette.secondary),
            palette.secondary
        );

        let terminate = DojoPromptUi::terminate(
            DojoId::new(),
            "captured".to_owned(),
            1,
            vec![(SplintId::new(), 1)],
        );
        let target = crate::frontend::LairPromptTarget {
            topology_revision: splinterm_core::TopologyRevision::new(9),
            lair_id: LairId::new(),
            dojo_id: None,
            name: "saved".to_owned(),
            retention: splinterm_core::LairRetention::Saved,
            preview: "Lair: saved".to_owned(),
            targets: Vec::new(),
        };
        let terminate_lair = DojoPromptUi::terminate_lair(target.clone());
        let restore = DojoPromptUi::restore_lair(target);
        let confirm = TerminationDecision::Terminate;
        let cancel = TerminationDecision::Cancel;
        for prompt in [&terminate, &terminate_lair] {
            assert!(prompt.confirmation_destructive());
            assert_eq!(
                dojo_prompt_choice_color(&palette, prompt, confirm, false),
                palette.danger
            );
            assert_eq!(
                dojo_prompt_choice_color(&palette, prompt, confirm, true),
                palette.selected_danger
            );
            assert_eq!(
                dojo_prompt_choice_color(&palette, prompt, cancel, false),
                palette.primary
            );
        }
        assert!(!restore.confirmation_destructive());
        assert_eq!(restore.confirmation_label(), Some("Restore"));
        assert_eq!(
            dojo_prompt_choice_color(&palette, &restore, confirm, false),
            palette.selected_rail
        );
        assert_eq!(
            dojo_prompt_choice_color(&palette, &restore, confirm, true),
            palette.selected_primary
        );
        assert_eq!(
            dojo_prompt_choice_color(&palette, &restore, cancel, true),
            palette.selected_primary
        );
    }
}
