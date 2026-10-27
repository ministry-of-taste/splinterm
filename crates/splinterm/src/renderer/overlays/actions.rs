//! Trusted command-palette layout and CPU composition.

use std::collections::HashMap;

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
    super::{ChromeText, ChromeTextStyle, RenderContext, blend_rect, fill_rect},
    picker::{
        ROW_HOVER_ALPHA, ROW_PRESS_ALPHA, SessionPickerPalette, editable_text, truncate_picker_text,
    },
};

const HEADER_HEIGHT: u32 = 52;
const ROW_HEIGHT: u32 = 48;
const FOOTER_HEIGHT: u32 = 36;
const PANEL_MAX_WIDTH: u32 = 680;
const PANEL_HORIZONTAL_MARGIN: u32 = 24;
const PANEL_VERTICAL_MARGIN: u32 = 16;
const CONTENT_INSET: u32 = 24;
const ROW_GUTTER_X: u32 = 8;
const ROW_GUTTER_Y: u32 = 2;
const PANEL_TOP_OFFSET: u32 = 48;
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
    pub(crate) panel: Rect,
    pub(crate) header: Rect,
    pub(crate) list: Rect,
    pub(crate) footer: Rect,
    pub(crate) rows: Vec<CommandPaletteRowLayout>,
    pub(crate) visible_start: usize,
    pub(crate) compact: bool,
}

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
    if available_width == 0 || available_height < HEADER_HEIGHT.saturating_add(FOOTER_HEIGHT) {
        return None;
    }
    let panel_width = available_width.min(PANEL_MAX_WIDTH);
    let available_list_height =
        available_height.saturating_sub(HEADER_HEIGHT.saturating_add(FOOTER_HEIGHT));
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
    let list_height = if filtered.is_empty() || visible_count == 0 {
        available_list_height.min(ROW_HEIGHT)
    } else {
        ROW_HEIGHT.saturating_mul(u32::try_from(visible_count).unwrap_or(u32::MAX))
    };
    let panel_height = HEADER_HEIGHT
        .saturating_add(list_height)
        .saturating_add(FOOTER_HEIGHT)
        .min(available_height);
    let x = content
        .x
        .saturating_add(content.width.saturating_sub(panel_width) / 2);
    let maximum_y = content
        .y
        .saturating_add(content.height.saturating_sub(panel_height));
    let y = content
        .y
        .saturating_add(PANEL_TOP_OFFSET.min(content.height.saturating_sub(panel_height)))
        .min(maximum_y);
    let panel = Rect {
        x,
        y,
        width: panel_width,
        height: panel_height,
    };
    let header = Rect {
        x,
        y,
        width: panel_width,
        height: HEADER_HEIGHT,
    };
    let list = Rect {
        x,
        y: y.saturating_add(HEADER_HEIGHT),
        width: panel_width,
        height: list_height,
    };
    let footer = Rect {
        x,
        y: list.y.saturating_add(list.height),
        width: panel_width,
        height: panel_height.saturating_sub(HEADER_HEIGHT.saturating_add(list_height)),
    };
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
            let horizontal_gutter = ROW_GUTTER_X.min(panel_width / 6);
            let vertical_gutter = ROW_GUTTER_Y.min(ROW_HEIGHT / 4);
            let rect = Rect {
                x: x.saturating_add(horizontal_gutter),
                y: slot_y.saturating_add(vertical_gutter),
                width: panel_width.saturating_sub(horizontal_gutter.saturating_mul(2)),
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
        header,
        list,
        footer,
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

#[derive(Default)]
pub(crate) struct CommandPaletteTextCache {
    scale_120: u32,
    renderer_generation: u64,
    entries: HashMap<(String, ChromeTextStyle), ChromeText>,
}

impl CommandPaletteTextCache {
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.scale_120 = 0;
        self.renderer_generation = 0;
    }

    fn text<'a>(
        &'a mut self,
        context: &RenderContext,
        source: &str,
        style: ChromeTextStyle,
        scale_120: u32,
        renderer_generation: u64,
    ) -> Result<&'a ChromeText> {
        if self.scale_120 != scale_120 || self.renderer_generation != renderer_generation {
            self.entries.clear();
            self.scale_120 = scale_120;
            self.renderer_generation = renderer_generation;
        }
        let key = (source.to_owned(), style);
        if !self.entries.contains_key(&key) {
            self.entries.insert(
                key.clone(),
                ChromeText::load_styled_with_context(source, scale_120, style, context)?,
            );
        }
        Ok(self
            .entries
            .get(&key)
            .expect("inserted command text remains"))
    }

    pub(crate) fn tab_menu_label_width(
        &mut self,
        context: &RenderContext,
        actions: &[crate::frontend::TabMenuActionDescriptor],
        scale_120: u32,
        renderer_generation: u64,
    ) -> Result<u32> {
        let mut width = 0;
        for action in actions {
            for style in [ChromeTextStyle::Regular, ChromeTextStyle::Bold] {
                width = width.max(
                    self.text(context, action.title, style, scale_120, renderer_generation)?
                        .pixel_width(),
                );
            }
        }
        Ok(width.saturating_mul(120).div_ceil(scale_120.max(1)))
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    fn contains_source(&self, source: &str) -> bool {
        self.entries.keys().any(|(text, _)| text == source)
    }
}

fn buffer_rect(rect: Rect, scale_120: u32) -> Rect {
    let scale = |value: u32| value.saturating_mul(scale_120).div_ceil(120);
    Rect {
        x: scale(rect.x),
        y: scale(rect.y),
        width: scale(rect.width),
        height: scale(rect.height),
    }
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
        u8::try_from((color >> 16) & 0xff).unwrap_or(0),
        u8::try_from((color >> 8) & 0xff).unwrap_or(0),
        u8::try_from(color & 0xff).unwrap_or(0),
        u8::MAX,
    ]
}

/// `source` truncated to its last `maximum_cells` cells behind a leading `…`.
fn truncate_text_start(source: &str, maximum_cells: usize) -> String {
    let cells = |character: char| character.width().unwrap_or(0).min(2);
    if source.chars().map(cells).sum::<usize>() <= maximum_cells {
        return source.to_owned();
    }
    if maximum_cells == 0 {
        return String::new();
    }
    let mut used = 0_usize;
    let mut reversed = Vec::new();
    for character in source.chars().rev() {
        if used.saturating_add(cells(character)) > maximum_cells - 1 {
            break;
        }
        used = used.saturating_add(cells(character));
        reversed.push(character);
    }
    let tail = reversed
        .into_iter()
        .rev()
        .skip_while(|character| cells(*character) == 0)
        .collect::<String>();
    format!("…{tail}")
}

/// `source` as chrome text bounded by `clip_width` buffer pixels, so a long
/// label is never cut off silently: it ends with `…`, or with `keep_end` it
/// starts with `…` and keeps its end (a right-aligned shortcut keeps its key).
#[allow(clippy::too_many_arguments)]
fn fitted_text<'a>(
    cache: &'a mut CommandPaletteTextCache,
    context: &RenderContext,
    source: &str,
    style: ChromeTextStyle,
    scale_120: u32,
    renderer_generation: u64,
    clip_width: u32,
    keep_end: bool,
) -> Result<&'a ChromeText> {
    let (fits, cell_width) = {
        let text = cache.text(context, source, style, scale_120, renderer_generation)?;
        (
            text.pixel_width() <= clip_width,
            text.frame.cell_width.max(1),
        )
    };
    if fits {
        return cache.text(context, source, style, scale_120, renderer_generation);
    }
    let maximum_cells = usize::try_from(clip_width / cell_width).unwrap_or(usize::MAX);
    let truncated = if keep_end {
        truncate_text_start(source, maximum_cells)
    } else {
        truncate_picker_text(source, maximum_cells)
    };
    cache.text(context, &truncated, style, scale_120, renderer_generation)
}

/// An editable `value` after `prefix` with its caret, fitted to `clip_width`
/// buffer pixels by keeping its end: the caret and the latest input stay
/// visible and the start gives way behind `…`.
fn editable_field_text(
    cache: &mut CommandPaletteTextCache,
    context: &RenderContext,
    prefix: &str,
    value: &str,
    scale_120: u32,
    renderer_generation: u64,
    clip_width: u32,
) -> Result<String> {
    let cell_width = cache
        .text(
            context,
            "_",
            ChromeTextStyle::Regular,
            scale_120,
            renderer_generation,
        )?
        .frame
        .cell_width
        .max(1);
    let maximum_cells = usize::try_from(clip_width / cell_width).unwrap_or(usize::MAX);
    Ok(editable_text(prefix, value, maximum_cells))
}

#[allow(clippy::too_many_arguments)]
fn paint_text(
    cache: &mut CommandPaletteTextCache,
    context: &RenderContext,
    canvas: &mut [u8],
    width: u32,
    height: u32,
    source: &str,
    style: ChromeTextStyle,
    scale_120: u32,
    renderer_generation: u64,
    clip: Rect,
    color: u32,
    right_aligned: bool,
) -> Result<()> {
    let text = fitted_text(
        cache,
        context,
        source,
        style,
        scale_120,
        renderer_generation,
        clip.width,
        right_aligned,
    )?;
    let x = if right_aligned {
        clip.x
            .saturating_add(clip.width.saturating_sub(text.pixel_width()))
    } else {
        clip.x
    };
    let y = clip
        .y
        .saturating_add(clip.height.saturating_sub(text.pixel_height()) / 2);
    text.paint(canvas, width, height, (x, y), clip, color);
    Ok(())
}

const fn palette_title(binding_help: bool) -> &'static str {
    if binding_help {
        "KEY BINDINGS"
    } else {
        "COMMANDS"
    }
}

/// What the palette's list shows: its commands, the key bindings, or the
/// **Agent access** list (ADR 0014).
#[derive(Clone, Copy, Debug)]
pub(crate) enum PaletteBody<'a> {
    Commands,
    BindingHelp(&'a BindingHelpUi),
    AgentAccess(&'a AgentAccessUi),
}

/// One read-only list row: category, title, one-line form, and the text in
/// the shortcut column.
struct ListRowText<'a> {
    category: &'a str,
    title: &'a str,
    compact: &'a str,
    trailing: &'a str,
}

/// The header title slot, in buffer pixels: the 98 logical px the palette has
/// always reserved, widened to fit a longer title and a gap before the query,
/// but by no more than half of `available`, so the query keeps room.
fn palette_title_width(title_width: u32, available: u32, scale_120: u32) -> u32 {
    let scaled = |value: u32| value.saturating_mul(scale_120).div_ceil(120);
    scaled(98).max(title_width.saturating_add(scaled(12)).min(available / 2))
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(crate) fn paint_command_palette(
    cache: &mut CommandPaletteTextCache,
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
    blend_rect(
        canvas,
        width,
        height,
        tuple(buffer_rect(content_rect, scale_120)),
        [palette.scrim[0], palette.scrim[1], palette.scrim[2], 96],
    );
    let panel = buffer_rect(layout.panel, scale_120);
    let shadow_offset = 6_u32.saturating_mul(scale_120).div_ceil(120);
    blend_rect(
        canvas,
        width,
        height,
        (
            i32::try_from(panel.x.saturating_add(shadow_offset)).unwrap_or(i32::MAX),
            i32::try_from(panel.y.saturating_add(shadow_offset)).unwrap_or(i32::MAX),
            panel.width,
            panel.height,
        ),
        palette.shadow,
    );
    fill_rect(canvas, width, height, tuple(panel), palette.panel_fill);
    let border = scale_120.div_ceil(120).max(1);
    let frame = if keyboard_focused {
        palette.focused_frame
    } else {
        palette.frame
    };
    for edge in [
        (panel.x, panel.y, panel.width, border),
        (
            panel.x,
            panel.y.saturating_add(panel.height.saturating_sub(border)),
            panel.width,
            border,
        ),
        (panel.x, panel.y, border, panel.height),
        (
            panel.x.saturating_add(panel.width.saturating_sub(border)),
            panel.y,
            border,
            panel.height,
        ),
    ] {
        fill_rect(
            canvas,
            width,
            height,
            (
                i32::try_from(edge.0).unwrap_or(i32::MAX),
                i32::try_from(edge.1).unwrap_or(i32::MAX),
                edge.2,
                edge.3,
            ),
            rgba(frame),
        );
    }
    for y in [
        layout.header.y.saturating_add(layout.header.height),
        layout.footer.y,
    ] {
        let divider_inset = CONTENT_INSET.min(layout.panel.width / 4);
        fill_rect(
            canvas,
            width,
            height,
            tuple(buffer_rect(
                Rect {
                    x: layout.panel.x.saturating_add(divider_inset),
                    y,
                    width: layout
                        .panel
                        .width
                        .saturating_sub(divider_inset.saturating_mul(2)),
                    height: 1,
                },
                scale_120,
            )),
            rgba(palette.frame),
        );
    }

    let header = buffer_rect(layout.header, scale_120);
    let inset = CONTENT_INSET.saturating_mul(scale_120).div_ceil(120);
    let title = match body {
        PaletteBody::AgentAccess(access) => access.heading(),
        PaletteBody::BindingHelp(_) => palette_title(true).to_owned(),
        PaletteBody::Commands => palette_title(false).to_owned(),
    };
    let title = title.as_str();
    let title_width = palette_title_width(
        cache
            .text(
                context,
                title,
                ChromeTextStyle::Bold,
                scale_120,
                renderer_generation,
            )?
            .pixel_width(),
        header.width.saturating_sub(inset.saturating_mul(2)),
        scale_120,
    );
    paint_text(
        cache,
        context,
        canvas,
        width,
        height,
        title,
        ChromeTextStyle::Bold,
        scale_120,
        renderer_generation,
        Rect {
            x: header.x.saturating_add(inset),
            y: header.y,
            width: title_width,
            height: header.height,
        },
        palette.primary,
        false,
    )?;
    let query_clip = Rect {
        x: header.x.saturating_add(inset).saturating_add(title_width),
        y: header.y,
        width: header
            .width
            .saturating_sub(inset.saturating_mul(2))
            .saturating_sub(title_width),
        height: header.height,
    };
    let typed = match body {
        PaletteBody::Commands => state.query(),
        PaletteBody::BindingHelp(help) => help.query(),
        PaletteBody::AgentAccess(access) => access.query(),
    };
    let query_is_placeholder = typed.is_empty();
    let query = if query_is_placeholder {
        match body {
            PaletteBody::BindingHelp(_) => "> Search keybindings",
            PaletteBody::AgentAccess(_) => "> Search agent access",
            PaletteBody::Commands => "> Type a command",
        }
        .to_owned()
    } else {
        editable_field_text(
            cache,
            context,
            "> ",
            typed,
            scale_120,
            renderer_generation,
            query_clip.width,
        )?
    };
    paint_text(
        cache,
        context,
        canvas,
        width,
        height,
        &query,
        ChromeTextStyle::Regular,
        scale_120,
        renderer_generation,
        query_clip,
        if query_is_placeholder {
            palette.secondary
        } else {
            palette.primary
        },
        false,
    )?;

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
    if let Some(empty_label) = empty_label.as_deref() {
        let empty = buffer_rect(layout.list, scale_120);
        paint_text(
            cache,
            context,
            canvas,
            width,
            height,
            empty_label,
            ChromeTextStyle::Regular,
            scale_120,
            renderer_generation,
            Rect {
                x: empty.x.saturating_add(inset),
                width: empty.width.saturating_sub(inset.saturating_mul(2)),
                ..empty
            },
            palette.secondary,
            false,
        )?;
    }
    let list_rows: Option<(Vec<ListRowText<'_>>, usize)> = match body {
        PaletteBody::BindingHelp(help) => Some((
            help.rows()
                .iter()
                .map(|row| ListRowText {
                    category: &row.source,
                    title: &row.action,
                    compact: &row.compact,
                    trailing: &row.shortcut,
                })
                .collect(),
            help.selected_index(),
        )),
        PaletteBody::AgentAccess(access) => Some((
            access
                .rows()
                .iter()
                .map(|row| ListRowText {
                    category: row.category,
                    title: &row.title,
                    compact: &row.compact,
                    trailing: &row.status,
                })
                .collect(),
            access.selected_index(),
        )),
        PaletteBody::Commands => None,
    };
    if let Some((list_rows, selected_index)) = list_rows {
        for (offset, row) in layout.rows.iter().enumerate() {
            let index = layout.visible_start.saturating_add(offset);
            let Some(help_row) = list_rows.get(index) else {
                continue;
            };
            let rect = buffer_rect(row.rect, scale_120);
            let is_selected = index == selected_index;
            if is_selected {
                fill_rect(
                    canvas,
                    width,
                    height,
                    tuple(rect),
                    rgba(palette.selected_fill),
                );
                fill_rect(
                    canvas,
                    width,
                    height,
                    (
                        i32::try_from(rect.x).unwrap_or(i32::MAX),
                        i32::try_from(rect.y).unwrap_or(i32::MAX),
                        3_u32.saturating_mul(scale_120).div_ceil(120).max(1),
                        rect.height,
                    ),
                    rgba(palette.selected_rail),
                );
            }
            let primary = if is_selected {
                palette.selected_primary
            } else {
                palette.primary
            };
            if row.category.width > 0 {
                paint_text(
                    cache,
                    context,
                    canvas,
                    width,
                    height,
                    help_row.category,
                    ChromeTextStyle::Regular,
                    scale_120,
                    renderer_generation,
                    buffer_rect(row.category, scale_120),
                    if is_selected {
                        palette.selected_secondary
                    } else {
                        palette.secondary
                    },
                    false,
                )?;
            }
            paint_text(
                cache,
                context,
                canvas,
                width,
                height,
                if layout.compact {
                    help_row.compact
                } else {
                    help_row.title
                },
                if is_selected {
                    ChromeTextStyle::Bold
                } else {
                    ChromeTextStyle::Regular
                },
                scale_120,
                renderer_generation,
                buffer_rect(row.title, scale_120),
                primary,
                false,
            )?;
            if !layout.compact {
                paint_text(
                    cache,
                    context,
                    canvas,
                    width,
                    height,
                    help_row.trailing,
                    ChromeTextStyle::Regular,
                    scale_120,
                    renderer_generation,
                    buffer_rect(row.shortcut, scale_120),
                    if is_selected {
                        palette.selected_secondary
                    } else {
                        palette.secondary
                    },
                    true,
                )?;
            }
        }
    } else {
        let selected = state.selected_command();
        for row in &layout.rows {
            let rect = buffer_rect(row.rect, scale_120);
            let enabled = state.command_enabled(row.command);
            let is_selected = enabled && selected == Some(row.command);
            if is_selected {
                fill_rect(
                    canvas,
                    width,
                    height,
                    tuple(rect),
                    rgba(palette.selected_fill),
                );
                fill_rect(
                    canvas,
                    width,
                    height,
                    (
                        i32::try_from(rect.x).unwrap_or(i32::MAX),
                        i32::try_from(rect.y).unwrap_or(i32::MAX),
                        3_u32.saturating_mul(scale_120).div_ceil(120).max(1),
                        rect.height,
                    ),
                    rgba(palette.selected_rail),
                );
            } else if enabled && state.hovered() == Some(row.command) {
                blend_rect(
                    canvas,
                    width,
                    height,
                    tuple(rect),
                    [
                        u8::try_from((palette.selected_fill >> 16) & 0xff).unwrap_or(0),
                        u8::try_from((palette.selected_fill >> 8) & 0xff).unwrap_or(0),
                        u8::try_from(palette.selected_fill & 0xff).unwrap_or(0),
                        ROW_HOVER_ALPHA,
                    ],
                );
            }
            if enabled && pressed == Some(row.command) {
                blend_rect(
                    canvas,
                    width,
                    height,
                    tuple(rect),
                    [
                        u8::try_from((palette.selected_rail >> 16) & 0xff).unwrap_or(0),
                        u8::try_from((palette.selected_rail >> 8) & 0xff).unwrap_or(0),
                        u8::try_from(palette.selected_rail & 0xff).unwrap_or(0),
                        ROW_PRESS_ALPHA,
                    ],
                );
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
                paint_text(
                    cache,
                    context,
                    canvas,
                    width,
                    height,
                    "›",
                    ChromeTextStyle::Bold,
                    scale_120,
                    renderer_generation,
                    Rect {
                        x: rect.x.saturating_add(inset / 2),
                        y: rect.y,
                        width: 20_u32.saturating_mul(scale_120).div_ceil(120),
                        height: rect.height,
                    },
                    primary,
                    false,
                )?;
            }
            let descriptor = command_descriptor(row.command);
            if row.category.width > 0 {
                paint_text(
                    cache,
                    context,
                    canvas,
                    width,
                    height,
                    descriptor.category.label(),
                    ChromeTextStyle::Regular,
                    scale_120,
                    renderer_generation,
                    buffer_rect(row.category, scale_120),
                    row_secondary,
                    false,
                )?;
            }
            paint_text(
                cache,
                context,
                canvas,
                width,
                height,
                descriptor.title,
                if is_selected {
                    ChromeTextStyle::Bold
                } else {
                    ChromeTextStyle::Regular
                },
                scale_120,
                renderer_generation,
                buffer_rect(row.title, scale_120),
                if enabled && row.command.destructive() {
                    palette.row_danger(is_selected, highlighted)
                } else {
                    primary
                },
                false,
            )?;
            if !layout.compact {
                paint_text(
                    cache,
                    context,
                    canvas,
                    width,
                    height,
                    state
                        .badge(row.command)
                        .as_deref()
                        .unwrap_or_else(|| descriptor.shortcut(keymap)),
                    ChromeTextStyle::Regular,
                    scale_120,
                    renderer_generation,
                    buffer_rect(row.shortcut, scale_120),
                    row_secondary,
                    true,
                )?;
            }
        }
    }
    let footer = buffer_rect(layout.footer, scale_120);
    let footer_text = match body {
        PaletteBody::BindingHelp(_) => {
            "Type to search · ↑↓/Pg navigate · Ctrl+U clear · Esc clear/close".to_owned()
        }
        PaletteBody::AgentAccess(access) => access.footer(),
        PaletteBody::Commands => "↑↓ navigate   Enter run   Esc close".to_owned(),
    };
    paint_text(
        cache,
        context,
        canvas,
        width,
        height,
        &footer_text,
        ChromeTextStyle::Regular,
        scale_120,
        renderer_generation,
        Rect {
            x: footer.x.saturating_add(inset),
            width: footer.width.saturating_sub(inset.saturating_mul(2)),
            ..footer
        },
        palette.secondary,
        false,
    )?;
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
    cache: &mut CommandPaletteTextCache,
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
    let panel = buffer_rect(layout.panel, scale_120);
    let shadow_offset = TAB_MENU_SHADOW.saturating_mul(scale_120).div_ceil(120);
    blend_rect(
        canvas,
        width,
        height,
        (
            i32::try_from(panel.x.saturating_add(shadow_offset)).unwrap_or(i32::MAX),
            i32::try_from(panel.y.saturating_add(shadow_offset)).unwrap_or(i32::MAX),
            panel.width,
            panel.height,
        ),
        palette.shadow,
    );
    fill_rect(canvas, width, height, tuple(panel), palette.panel_fill);
    let border = scale_120.div_ceil(120).max(1);
    let frame = if keyboard_focused {
        palette.focused_frame
    } else {
        palette.frame
    };
    for edge in [
        (panel.x, panel.y, panel.width, border),
        (
            panel.x,
            panel.y.saturating_add(panel.height.saturating_sub(border)),
            panel.width,
            border,
        ),
        (panel.x, panel.y, border, panel.height),
        (
            panel.x.saturating_add(panel.width.saturating_sub(border)),
            panel.y,
            border,
            panel.height,
        ),
    ] {
        fill_rect(
            canvas,
            width,
            height,
            (
                i32::try_from(edge.0).unwrap_or(i32::MAX),
                i32::try_from(edge.1).unwrap_or(i32::MAX),
                edge.2,
                edge.3,
            ),
            rgba(frame),
        );
    }
    for row in &layout.rows {
        let rect = buffer_rect(row.rect, scale_120);
        let enabled = state.action_enabled(row.action);
        let selected = enabled && state.selected_action() == row.action;
        if selected {
            fill_rect(
                canvas,
                width,
                height,
                tuple(rect),
                rgba(palette.selected_fill),
            );
            fill_rect(
                canvas,
                width,
                height,
                (
                    i32::try_from(rect.x).unwrap_or(i32::MAX),
                    i32::try_from(rect.y).unwrap_or(i32::MAX),
                    3_u32.saturating_mul(scale_120).div_ceil(120).max(1),
                    rect.height,
                ),
                rgba(palette.selected_rail),
            );
        } else if enabled && state.hovered() == Some(row.action) {
            blend_rect(
                canvas,
                width,
                height,
                tuple(rect),
                [
                    u8::try_from((palette.selected_fill >> 16) & 0xff).unwrap_or(0),
                    u8::try_from((palette.selected_fill >> 8) & 0xff).unwrap_or(0),
                    u8::try_from(palette.selected_fill & 0xff).unwrap_or(0),
                    ROW_HOVER_ALPHA,
                ],
            );
        }
        if enabled && pressed == Some(row.action) {
            blend_rect(
                canvas,
                width,
                height,
                tuple(rect),
                [
                    u8::try_from((palette.selected_rail >> 16) & 0xff).unwrap_or(0),
                    u8::try_from((palette.selected_rail >> 8) & 0xff).unwrap_or(0),
                    u8::try_from(palette.selected_rail & 0xff).unwrap_or(0),
                    ROW_PRESS_ALPHA,
                ],
            );
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
        let label_color = if enabled && row.action.destructive() {
            palette.row_danger(selected, highlighted)
        } else {
            primary
        };
        if selected {
            let inset = 3_u32
                .saturating_mul(scale_120)
                .div_ceil(120)
                .min(rect.width);
            let indicator = Rect {
                x: rect.x.saturating_add(inset),
                y: rect.y,
                width: TAB_MENU_INDICATOR_WIDTH
                    .saturating_mul(scale_120)
                    .div_ceil(120)
                    .min(rect.width.saturating_sub(inset)),
                height: rect.height,
            };
            paint_text(
                cache,
                context,
                canvas,
                width,
                height,
                "›",
                ChromeTextStyle::Bold,
                scale_120,
                renderer_generation,
                indicator,
                primary,
                false,
            )?;
        }
        paint_text(
            cache,
            context,
            canvas,
            width,
            height,
            row.label,
            if selected {
                ChromeTextStyle::Bold
            } else {
                ChromeTextStyle::Regular
            },
            scale_120,
            renderer_generation,
            buffer_rect(row.title, scale_120),
            label_color,
            false,
        )?;
    }
    fill_rect(
        canvas,
        width,
        height,
        (
            i32::try_from(panel.x.saturating_add(panel.width.saturating_sub(border)))
                .unwrap_or(i32::MAX),
            i32::try_from(panel.y).unwrap_or(i32::MAX),
            border,
            panel.height,
        ),
        rgba(frame),
    );
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

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "trusted Dojo prompts share one bounded painter and composition transaction"
)]
pub(crate) fn paint_dojo_prompt(
    cache: &mut CommandPaletteTextCache,
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
    blend_rect(
        canvas,
        width,
        height,
        tuple(buffer_rect(content_rect, scale_120)),
        [palette.scrim[0], palette.scrim[1], palette.scrim[2], 112],
    );
    let panel = buffer_rect(layout.panel, scale_120);
    fill_rect(canvas, width, height, tuple(panel), palette.panel_fill);
    let border = scale_120.div_ceil(120).max(1);
    let frame = if keyboard_focused {
        palette.focused_frame
    } else {
        palette.frame
    };
    for edge in [
        (panel.x, panel.y, panel.width, border),
        (
            panel.x,
            panel.y.saturating_add(panel.height.saturating_sub(border)),
            panel.width,
            border,
        ),
        (panel.x, panel.y, border, panel.height),
        (
            panel.x.saturating_add(panel.width.saturating_sub(border)),
            panel.y,
            border,
            panel.height,
        ),
    ] {
        fill_rect(
            canvas,
            width,
            height,
            (
                i32::try_from(edge.0).unwrap_or(i32::MAX),
                i32::try_from(edge.1).unwrap_or(i32::MAX),
                edge.2,
                edge.3,
            ),
            rgba(frame),
        );
    }
    let (title, body) = state.title_and_body();
    paint_text(
        cache,
        context,
        canvas,
        width,
        height,
        title,
        ChromeTextStyle::Bold,
        scale_120,
        renderer_generation,
        buffer_rect(layout.title, scale_120),
        palette.primary,
        false,
    )?;
    if state.uses_layout_summary() {
        let body_rect = buffer_rect(layout.body, scale_120);
        let line_height = PREVIEW_LINE_HEIGHT.saturating_mul(scale_120).div_ceil(120);
        let metrics = ChromeText::load_styled_with_context(
            "M",
            scale_120,
            ChromeTextStyle::Regular,
            context,
        )?;
        let maximum_cells = usize::try_from(body_rect.width / metrics.frame.cell_width.max(1))
            .unwrap_or(MAX_PREVIEW_LINE_SCALARS)
            .min(MAX_PREVIEW_LINE_SCALARS);
        for (index, line) in wrap_preview_body(&body, maximum_cells).iter().enumerate() {
            let Some(line_rect) = preview_line_rect(body_rect, index, line_height) else {
                break;
            };
            paint_text(
                cache,
                context,
                canvas,
                width,
                height,
                line,
                ChromeTextStyle::Regular,
                scale_120,
                renderer_generation,
                line_rect,
                palette.secondary,
                false,
            )?;
        }
    } else {
        paint_text(
            cache,
            context,
            canvas,
            width,
            height,
            &body,
            ChromeTextStyle::Regular,
            scale_120,
            renderer_generation,
            buffer_rect(layout.body, scale_120),
            palette.secondary,
            false,
        )?;
    }
    if let Some(prompt_input) = state.input() {
        let input = buffer_rect(layout.input.expect("rename input exists"), scale_120);
        fill_rect(
            canvas,
            width,
            height,
            tuple(input),
            rgba(palette.selected_fill),
        );
        let inset = 12_u32.saturating_mul(scale_120).div_ceil(120);
        let input_clip = Rect {
            x: input.x.saturating_add(inset),
            width: input.width.saturating_sub(inset.saturating_mul(2)),
            ..input
        };
        let field = editable_field_text(
            cache,
            context,
            "> ",
            prompt_input,
            scale_120,
            renderer_generation,
            input_clip.width,
        )?;
        paint_text(
            cache,
            context,
            canvas,
            width,
            height,
            &field,
            ChromeTextStyle::Regular,
            scale_120,
            renderer_generation,
            input_clip,
            palette.selected_primary,
            false,
        )?;
    } else if state.is_preview() {
        let rect = buffer_rect(layout.cancel.expect("preview close exists"), scale_120);
        fill_rect(
            canvas,
            width,
            height,
            tuple(rect),
            rgba(palette.selected_fill),
        );
        paint_text(
            cache,
            context,
            canvas,
            width,
            height,
            "Close",
            ChromeTextStyle::Bold,
            scale_120,
            renderer_generation,
            rect,
            palette.selected_primary,
            false,
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
            let rect = buffer_rect(rect, scale_120);
            let selected = selected_decision == decision;
            fill_rect(
                canvas,
                width,
                height,
                tuple(rect),
                if selected {
                    rgba(palette.selected_fill)
                } else {
                    palette.panel_fill
                },
            );
            paint_text(
                cache,
                context,
                canvas,
                width,
                height,
                label,
                if selected {
                    ChromeTextStyle::Bold
                } else {
                    ChromeTextStyle::Regular
                },
                scale_120,
                renderer_generation,
                rect,
                // Terminate is drawn in danger, selected or not.
                match (decision == TerminationDecision::Terminate, selected) {
                    (true, true) => palette.selected_danger,
                    (true, false) => palette.danger,
                    (false, true) => palette.selected_primary,
                    (false, false) => palette.primary,
                },
                false,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::ResolvedTheme,
        frontend::{CommandPaletteContext, CommandTabMoveAvailability},
        renderer::session_picker_palette,
    };
    use splinterm_core::{DojoId, LairId, SplintId};

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
            let layout = command_palette_layout(content, &commands, 2, 0).unwrap();
            assert!(layout.panel.x >= content.x && layout.panel.y >= content.y);
            assert!(layout.panel.x + layout.panel.width <= content.x + content.width);
            assert!(layout.panel.y + layout.panel.height <= content.y + content.height);
            assert!(layout.rows.iter().any(|row| row.command == commands[2]));
            for row in &layout.rows {
                assert_eq!(row.rect.height, ROW_HEIGHT - ROW_GUTTER_Y * 2);
                assert!(row.rect.x > layout.panel.x);
                assert!(row.rect.x + row.rect.width < layout.panel.x + layout.panel.width);
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
        let layout = command_palette_layout(content, &commands, 0, 0).unwrap();
        assert!(layout.rows.is_empty());
        assert_eq!(layout.list.height, 0);
        assert_eq!(
            layout.footer.y + layout.footer.height,
            layout.panel.y + layout.panel.height
        );
        assert!(layout.header.y + layout.header.height <= layout.panel.y + layout.panel.height);
        assert!(layout.list.y + layout.list.height <= layout.panel.y + layout.panel.height);
    }

    #[test]
    fn empty_results_keep_a_visible_inert_list_row() {
        let content = Rect {
            x: 0,
            y: 34,
            width: 640,
            height: 400,
        };
        let layout = command_palette_layout(content, &[], 0, 0).unwrap();
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
        let mut cache = CommandPaletteTextCache::default();
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
                    let label_width = cache
                        .tab_menu_label_width(&context, &[action], scale, 1)
                        .unwrap();
                    let layout = tab_context_menu_layout(
                        bounds,
                        (bounds.x + width, 400),
                        &[action],
                        label_width,
                    )
                    .unwrap();
                    let clip = buffer_rect(layout.rows[0].title, scale);
                    for style in [ChromeTextStyle::Regular, ChromeTextStyle::Bold] {
                        let text = cache.text(&context, action.title, style, scale, 1).unwrap();
                        assert!(
                            text.pixel_width() <= clip.width,
                            "{} at width {width}, scale {scale}: {} > {}",
                            action.title,
                            text.pixel_width(),
                            clip.width
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn key_binding_help_titles_itself_and_ellipsizes_its_source_column() {
        let context = RenderContext::new(u16::MAX);
        let mut cache = CommandPaletteTextCache::default();
        assert_eq!(palette_title(true), "KEY BINDINGS");
        assert_eq!(palette_title(false), "COMMANDS");
        for scale in [120, 150, 180, 240] {
            let available = buffer_rect(
                Rect {
                    x: 0,
                    y: 0,
                    width: PANEL_MAX_WIDTH - CONTENT_INSET * 2,
                    height: 1,
                },
                scale,
            )
            .width;
            for title in [palette_title(true), palette_title(false)] {
                let text_width = cache
                    .text(&context, title, ChromeTextStyle::Bold, scale, 1)
                    .unwrap()
                    .pixel_width();
                let slot = palette_title_width(text_width, available, scale);
                assert!(text_width < slot, "{title} at scale {scale}");
                assert!(slot <= available / 2);
            }
            // A narrow header keeps the original slot rather than none.
            assert_eq!(
                palette_title_width(u32::MAX, 0, scale),
                98_u32.saturating_mul(scale).div_ceil(120)
            );
            let source = "built-in profile splinterm";
            let clip = 72_u32.saturating_mul(scale).div_ceil(120);
            let text = fitted_text(
                &mut cache,
                &context,
                source,
                ChromeTextStyle::Regular,
                scale,
                1,
                clip,
                false,
            )
            .unwrap();
            assert!(text.pixel_width() <= clip);
            assert!(
                cache.entries.keys().any(|(text, _)| text.ends_with('…')
                    && source.starts_with(text.trim_end_matches('…')))
            );
        }
    }

    #[test]
    fn editable_fields_keep_their_caret_and_shortcuts_keep_their_key() {
        assert_eq!(truncate_text_start("Ctrl+Shift+P", 12), "Ctrl+Shift+P");
        assert_eq!(truncate_text_start("Ctrl+Shift+P", 7), "…hift+P");
        assert_eq!(truncate_text_start("Ctrl+界", 3), "…界");
        assert_eq!(truncate_text_start("abc", 0), "");
        assert_eq!(editable_text("> ", "dojo", 20), "> dojo_");
        assert_eq!(editable_text("> ", "my-long-dojo-name", 12), "> …ojo-name_");

        let context = RenderContext::new(u16::MAX);
        let mut cache = CommandPaletteTextCache::default();
        for scale in [120, 150, 180, 240] {
            let cell = cache
                .text(&context, "_", ChromeTextStyle::Regular, scale, 1)
                .unwrap()
                .frame
                .cell_width;
            let clip = cell * 12;
            let field = editable_field_text(
                &mut cache,
                &context,
                "> ",
                "my-long-dojo-name",
                scale,
                1,
                clip,
            )
            .unwrap();
            assert_eq!(field, "> …ojo-name_", "scale {scale}");
            let short =
                editable_field_text(&mut cache, &context, "> ", "dojo", scale, 1, clip).unwrap();
            assert_eq!(short, "> dojo_");
            let shortcut = "Ctrl+Shift+PageDown";
            let text = fitted_text(
                &mut cache,
                &context,
                shortcut,
                ChromeTextStyle::Regular,
                scale,
                1,
                cell * 10,
                true,
            )
            .unwrap();
            assert!(text.pixel_width() <= cell * 10);
            assert!(cache.contains_source("…+PageDown"), "scale {scale}");
        }
    }

    /// VM case E6: at maximum zoom "Preview saved layout" was clipped into
    /// fragments. It must fit its menu row whole or end in an ellipsis.
    /// Chrome no longer follows zoom, so the largest UI text size now
    /// stands in for it.
    #[test]
    fn maximum_zoom_menu_labels_fit_their_rows_or_end_in_an_ellipsis() {
        use super::super::super::text::{CHROME_MAXIMUM_LINE_HEIGHT, chrome_maximum_cell_height};

        for height in [
            TAB_MENU_ROW_HEIGHT,
            ROW_HEIGHT - ROW_GUTTER_Y * 2,
            HEADER_HEIGHT,
            PREVIEW_LINE_HEIGHT,
            PROMPT_TITLE_HEIGHT,
            PROMPT_INPUT_HEIGHT,
            PROMPT_BUTTON_HEIGHT,
            FOOTER_HEIGHT,
        ] {
            assert!(height >= CHROME_MAXIMUM_LINE_HEIGHT);
        }
        let action = tab_menu_descriptor(TabMenuActionId::PreviewLair);
        for scale in [120, 150, 180, 240] {
            let context = RenderContext::new(u16::MAX)
                .at_maximum_zoom(scale)
                .at_maximum_chrome_font_size();
            let mut cache = CommandPaletteTextCache::default();
            for width in [120, 160, 240, 640, 1268] {
                let bounds = Rect {
                    x: 0,
                    y: 0,
                    width,
                    height: 754,
                };
                let label_width = cache
                    .tab_menu_label_width(&context, &[action], scale, 1)
                    .unwrap();
                let layout =
                    tab_context_menu_layout(bounds, (width / 2, 300), &[action], label_width)
                        .unwrap();
                let row = buffer_rect(layout.rows[0].rect, scale);
                let clip = buffer_rect(layout.rows[0].title, scale);
                for style in [ChromeTextStyle::Regular, ChromeTextStyle::Bold] {
                    let full_width = cache
                        .text(&context, action.title, style, scale, 1)
                        .unwrap()
                        .pixel_width();
                    let text = fitted_text(
                        &mut cache,
                        &context,
                        action.title,
                        style,
                        scale,
                        1,
                        clip.width,
                        false,
                    )
                    .unwrap();
                    assert!(text.pixel_height() <= chrome_maximum_cell_height(scale));
                    assert!(
                        text.pixel_height() <= row.height,
                        "scale {scale}, width {width}: {} > {}",
                        text.pixel_height(),
                        row.height
                    );
                    assert!(
                        text.pixel_width() <= clip.width,
                        "scale {scale}, width {width}"
                    );
                    if full_width > clip.width {
                        assert!(
                            cache.entries.keys().any(|(source, entry_style)| {
                                *entry_style == style
                                    && source.ends_with('…')
                                    && action.title.starts_with(source.trim_end_matches('…'))
                            }),
                            "scale {scale}, width {width}: no ellipsized label"
                        );
                    }
                }
            }
            // A wide Window shows the whole label at the largest size.
            let label_width = cache
                .tab_menu_label_width(&context, &[action], scale, 1)
                .unwrap();
            let bounds = Rect {
                x: 0,
                y: 0,
                width: 1268,
                height: 754,
            };
            let layout =
                tab_context_menu_layout(bounds, (600, 300), &[action], label_width).unwrap();
            let clip = buffer_rect(layout.rows[0].title, scale);
            let full = cache
                .text(&context, action.title, ChromeTextStyle::Bold, scale, 1)
                .unwrap();
            assert!(full.pixel_width() <= clip.width, "scale {scale}");
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
        let mut cache = CommandPaletteTextCache::default();
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
        let mut cache = CommandPaletteTextCache::default();
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
        let state = CommandPaletteUi::new(CommandPaletteContext {
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
        });
        let layout = command_palette_layout(
            content,
            state.filtered(),
            state.selected_index(),
            state.visible_start(),
        )
        .unwrap();
        let theme = ResolvedTheme::default();
        let mut canvas = vec![0_u8; 640 * 400 * 4];
        let before = canvas.clone();
        let mut cache = CommandPaletteTextCache::default();
        let keymap = ResolvedKeymap::default();
        paint_command_palette(
            &mut cache,
            &RenderContext::new(u16::MAX),
            &mut canvas,
            640,
            400,
            content,
            120,
            1,
            &layout,
            session_picker_palette(theme),
            &state,
            &keymap,
            None,
            true,
            PaletteBody::Commands,
        )
        .unwrap();
        let shaped = cache.len();
        assert!(shaped > 0);
        assert_ne!(canvas, before);
        paint_command_palette(
            &mut cache,
            &RenderContext::new(u16::MAX),
            &mut canvas,
            640,
            400,
            content,
            120,
            1,
            &layout,
            session_picker_palette(theme),
            &state,
            &keymap,
            None,
            true,
            PaletteBody::Commands,
        )
        .unwrap();
        assert_eq!(cache.len(), shaped);
        let mut help = BindingHelpUi::new(&keymap);
        paint_command_palette(
            &mut cache,
            &RenderContext::new(u16::MAX),
            &mut canvas,
            640,
            400,
            content,
            120,
            1,
            &layout,
            session_picker_palette(theme),
            &state,
            &keymap,
            None,
            true,
            PaletteBody::BindingHelp(&help),
        )
        .unwrap();
        assert!(cache.len() > shaped);
        assert!(cache.contains_source("> Search keybindings"));

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
        )
        .unwrap();
        paint_command_palette(
            &mut cache,
            &RenderContext::new(u16::MAX),
            &mut canvas,
            640,
            400,
            content,
            120,
            1,
            &access_layout,
            session_picker_palette(theme),
            &state,
            &keymap,
            None,
            true,
            PaletteBody::AgentAccess(&access),
        )
        .unwrap();
        assert!(cache.contains_source("> Search agent access"));
        assert!(cache.contains_source("AGENT ACCESS"));
        assert!(cache.contains_source("ALWAYS"));
        assert!(cache.contains_source("not connected"));
        assert!(cache.contains_source(&access.footer()));

        assert!(help.append_text("definitely absent xyz"));
        let empty_layout = command_palette_layout(content, &[], 0, 0).unwrap();
        paint_command_palette(
            &mut cache,
            &RenderContext::new(u16::MAX),
            &mut canvas,
            640,
            400,
            content,
            120,
            1,
            &empty_layout,
            session_picker_palette(theme),
            &state,
            &keymap,
            None,
            true,
            PaletteBody::BindingHelp(&help),
        )
        .unwrap();
        assert!(cache.contains_source("> definitely absent xyz_"));
        assert!(cache.contains_source("No matching keybindings"));
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
        let state = CommandPaletteUi::new(CommandPaletteContext {
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
        });
        let layout = command_palette_layout(
            content,
            state.filtered(),
            state.selected_index(),
            state.visible_start(),
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
            &mut CommandPaletteTextCache::default(),
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
}
