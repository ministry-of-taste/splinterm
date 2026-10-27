//! The Dojo, Lair and Agent access prompts in the popup box (Plan 0065
//! milestone 5): centred over the dimmed Window, 440 px at most, with the
//! title set into the frame and the keys in its bottom edge. A rename is a
//! sunken field; a confirmation asks its question over bracketed buttons,
//! right-aligned, Cancel first and chosen by default; a saved layout reads
//! as an aligned table.

use anyhow::Result;
use unicode_width::UnicodeWidthChar;

use crate::{
    frontend::{DojoPromptUi, NOT_RESTORABLE, TerminationDecision},
    geometry::Rect,
};

use super::{
    super::{ChromeTextStyle, RenderContext},
    actions::InputLineLayout,
    chrome_box::{BOX_EDGE, ChromeBox, ChromePainter, opaque, paint_outline, paint_scrim},
    chrome_rows::text_cells,
    picker::{
        PickerTextAlignment, SessionPickerPalette, SessionPickerTextCache, picker_buffer_rect,
    },
};

/// A prompt is this wide at most, logical px.
const PROMPT_WIDTH: u32 = 440;
/// A prompt narrower than this is not shown.
const PROMPT_MINIMUM_WIDTH: u32 = 180;
/// Logical px kept clear around a prompt in the content area.
const PROMPT_MARGIN: u32 = 24;
/// Logical px between the inside of the frame and the prompt's content.
const PROMPT_INSET: u32 = 18;
/// Logical px between the body and the buttons.
const PROMPT_GAP: u32 = 14;
/// One body line, logical px.
const PROMPT_LINE_HEIGHT: u32 = 24;
/// A confirmation's body at most: its question and what it means.
const CONFIRM_LINES: usize = 4;
/// A summary's body at most: a saved layout, or what a revoke ends.
const SUMMARY_LINES: usize = 12;
/// The rename field, logical px.
const FIELD_HEIGHT: u32 = 30;
/// Logical px from the field's edge to its `›`.
const FIELD_INSET: u32 = 10;
/// A button, logical px.
const BUTTON_HEIGHT: u32 = 28;
/// Logical px between a button's edge and its brackets.
const BUTTON_PADDING: u32 = 8;
/// Logical px between two buttons.
const BUTTON_GAP: u32 = 10;
/// Cells between two table columns.
const COLUMN_GAP_CELLS: usize = 2;

/// Where a prompt's parts go, in logical px.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DojoPromptLayout {
    pub(crate) panel: Rect,
    /// The body's lines, inside the frame's inset; empty for a rename.
    pub(crate) body: Rect,
    /// How many body lines `body` holds.
    pub(crate) lines: usize,
    /// A rename's field.
    pub(crate) input: Option<Rect>,
    /// Cancel, or a preview's or notice's lone Close.
    pub(crate) cancel: Option<Rect>,
    /// The confirming choice: Terminate, Restore or Revoke.
    pub(crate) terminate: Option<Rect>,
}

/// The keys a prompt answers, set into its bottom frame.
fn prompt_legend(state: &DojoPromptUi) -> &'static [(&'static str, &'static str)] {
    if state.is_rename() {
        &[("↵", "save"), ("esc", "cancel")]
    } else if state.is_preview() {
        &[("↵ esc", "close")]
    } else {
        &[("←→", "choose"), ("↵", "select"), ("esc", "cancel")]
    }
}

/// How many body lines a prompt shows at most: none for a rename.
const fn body_line_cap(state: &DojoPromptUi) -> usize {
    if state.is_rename() {
        0
    } else if state.uses_layout_summary() {
        SUMMARY_LINES
    } else {
        CONFIRM_LINES
    }
}

/// The buttons' labels, Cancel first: Cancel and the confirming choice, a
/// preview's or notice's Close alone, or none for a rename.
fn button_labels(state: &DojoPromptUi) -> Vec<&'static str> {
    if state.is_rename() {
        Vec::new()
    } else if state.is_preview() {
        vec!["Close"]
    } else {
        vec!["Cancel", state.confirmation_label().unwrap_or("Confirm")]
    }
}

/// What a prompt's layout needs measured in its chrome text: every
/// button's width and how many lines its body wraps to.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct DojoPromptMetrics {
    /// The widest label in its brackets, `[ label ]`, with the padding
    /// either side, so both buttons match, in logical px.
    pub(crate) button_width: u32,
    /// The body's lines, wrapped to the body's width, before any cut.
    pub(crate) body_lines: usize,
}

/// A prompt's horizontal place in `content` (logical px): its margin, its
/// left edge and its width.
fn prompt_columns(content: Rect) -> (u32, u32, u32) {
    let margin = PROMPT_MARGIN.min(content.width / 4).min(content.height / 4);
    let width = PROMPT_WIDTH.min(content.width.saturating_sub(margin.saturating_mul(2)));
    let x = content
        .x
        .saturating_add(content.width.saturating_sub(width) / 2);
    (margin, x, width)
}

/// The body's left edge and width in `content`, logical px.
fn body_columns(content: Rect) -> (u32, u32) {
    let (_, x, width) = prompt_columns(content);
    let inset = BOX_EDGE.saturating_add(PROMPT_INSET);
    (
        x.saturating_add(inset),
        width.saturating_sub(inset.saturating_mul(2)),
    )
}

/// Measures `state`'s buttons and body for a prompt in `area`, the
/// content area (logical px), with the cell metrics the painter uses, so
/// the layout sizes the body to its text.
pub(crate) fn dojo_prompt_metrics(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    state: &DojoPromptUi,
    area: Rect,
    scale_120: u32,
    renderer_generation: u64,
) -> Result<DojoPromptMetrics> {
    let (cell_width, _) = cache.cell_metrics(
        context,
        ChromeTextStyle::Regular,
        scale_120,
        renderer_generation,
    )?;
    let cell_width = cell_width.max(1);
    let button_width = button_labels(state)
        .into_iter()
        .map(text_cells)
        .max()
        .map_or(0, |widest| {
            let padding = BUTTON_PADDING.saturating_mul(scale_120).div_ceil(120);
            let buffer = widest
                .saturating_add(4)
                .saturating_mul(cell_width)
                .saturating_add(padding.saturating_mul(2));
            buffer.saturating_mul(120).div_ceil(scale_120.max(1))
        });
    let body_lines = if body_line_cap(state) == 0 {
        0
    } else {
        let (x, width) = body_columns(area);
        let body = picker_buffer_rect(
            Rect {
                x,
                y: area.y,
                width,
                height: 1,
            },
            scale_120,
        );
        let maximum = usize::try_from(body.width / cell_width).unwrap_or(usize::MAX);
        let (_, source) = state.title_and_body();
        body_lines(&source, maximum, usize::MAX).len()
    };
    Ok(DojoPromptMetrics {
        button_width,
        body_lines,
    })
}

/// The prompt centred in `content` (logical px), sized by `metrics` from
/// [`dojo_prompt_metrics`]: the body as many lines as its text, up to the
/// prompt's cap and what `content` has room for. `None` when `content`
/// cannot hold the prompt and its margin, or its buttons side by side; the
/// Window then closes the prompt, so no hidden or unreadable prompt takes
/// keys.
#[must_use]
pub(crate) fn dojo_prompt_layout(
    content: Rect,
    state: &DojoPromptUi,
    metrics: DojoPromptMetrics,
) -> Option<DojoPromptLayout> {
    if content.width == 0 || content.height == 0 {
        return None;
    }
    let (margin, x, width) = prompt_columns(content);
    let inset = BOX_EDGE.saturating_add(PROMPT_INSET);
    let controls_height = if state.is_rename() {
        FIELD_HEIGHT
    } else {
        BUTTON_HEIGHT
    };
    let wanted = metrics.body_lines.min(body_line_cap(state));
    let gap = if wanted == 0 { 0 } else { PROMPT_GAP };
    let fixed = inset
        .saturating_mul(2)
        .saturating_add(gap)
        .saturating_add(controls_height);
    let room = content
        .height
        .saturating_sub(margin.saturating_mul(2))
        .saturating_sub(fixed)
        / PROMPT_LINE_HEIGHT;
    let lines = wanted.min(usize::try_from(room).unwrap_or(usize::MAX));
    if width < PROMPT_MINIMUM_WIDTH
        || content.height < fixed.saturating_add(margin.saturating_mul(2))
        || (wanted > 0 && lines == 0)
    {
        return None;
    }
    let body_height = PROMPT_LINE_HEIGHT.saturating_mul(u32::try_from(lines).unwrap_or(0));
    let height = fixed.saturating_add(body_height);
    let panel = Rect {
        x,
        y: content
            .y
            .saturating_add(content.height.saturating_sub(height) / 2),
        width,
        height,
    };
    let inner = Rect {
        x: panel.x.saturating_add(inset),
        y: panel.y.saturating_add(inset),
        width: width.saturating_sub(inset.saturating_mul(2)),
        height: height.saturating_sub(inset.saturating_mul(2)),
    };
    let body = Rect {
        height: body_height,
        ..inner
    };
    let controls_y = inner.y.saturating_add(body_height).saturating_add(gap);
    let mut layout = DojoPromptLayout {
        panel,
        body,
        lines,
        input: None,
        cancel: None,
        terminate: None,
    };
    if state.is_rename() {
        layout.input = Some(Rect {
            y: controls_y,
            height: FIELD_HEIGHT,
            ..inner
        });
        return Some(layout);
    }
    // Buttons side by side, each its widest label: a Window too narrow
    // for them gets no prompt rather than `[ … ]`.
    let count = if state.is_preview() { 1 } else { 2 };
    let button_width = metrics.button_width.max(1);
    if button_width
        .saturating_mul(count)
        .saturating_add(BUTTON_GAP.saturating_mul(count - 1))
        > inner.width
    {
        return None;
    }
    let right = inner.x.saturating_add(inner.width);
    let button = |from_right: u32| Rect {
        x: right.saturating_sub(
            button_width
                .saturating_mul(from_right)
                .saturating_add(BUTTON_GAP.saturating_mul(from_right - 1)),
        ),
        y: controls_y,
        width: button_width,
        height: BUTTON_HEIGHT,
    };
    if state.is_preview() {
        layout.cancel = Some(button(1));
    } else {
        layout.cancel = Some(button(2));
        layout.terminate = Some(button(1));
    }
    Some(layout)
}

/// Whether `point` is in `rect`'s half-open area.
fn contains(rect: Rect, point: (f64, f64)) -> bool {
    point.0 >= f64::from(rect.x)
        && point.1 >= f64::from(rect.y)
        && point.0 < f64::from(rect.x) + f64::from(rect.width)
        && point.1 < f64::from(rect.y) + f64::from(rect.height)
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

/// A button's fill, if any, and the colours of its brackets and label.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ButtonColors {
    fill: Option<u32>,
    bracket: u32,
    label: u32,
}

/// A selected button is on the selection fill in the accent; a selected
/// destructive one, Terminate, on the danger fill. Unselected, both show
/// their brackets in muted text, so neither disappears, and Terminate's
/// label stays in danger.
const fn button_colors(
    palette: &SessionPickerPalette,
    selected: bool,
    destructive: bool,
) -> ButtonColors {
    match (selected, destructive) {
        (true, true) => ButtonColors {
            fill: Some(palette.danger_fill),
            bracket: palette.danger_ink,
            label: palette.danger_ink,
        },
        (true, false) => ButtonColors {
            fill: Some(palette.selected_fill),
            bracket: palette.selected_accent,
            label: palette.selected_accent,
        },
        (false, true) => ButtonColors {
            fill: None,
            bracket: palette.secondary,
            label: palette.danger,
        },
        (false, false) => ButtonColors {
            fill: None,
            bracket: palette.secondary,
            label: palette.primary,
        },
    }
}

fn cells(text: &str) -> usize {
    text.chars()
        .map(|character| character.width().unwrap_or(0).min(2))
        .sum()
}

/// `text` in at most `maximum` cells, its end giving way to `…`.
fn fit_end(text: &str, maximum: usize) -> String {
    if cells(text) <= maximum {
        return text.to_owned();
    }
    if maximum == 0 {
        return String::new();
    }
    let mut fitted = String::new();
    let mut used = 0_usize;
    for character in text.chars() {
        let width = character.width().unwrap_or(0).min(2);
        if used.saturating_add(width).saturating_add(1) > maximum {
            break;
        }
        fitted.push(character);
        used = used.saturating_add(width);
    }
    fitted.push('…');
    fitted
}

/// `text` in at most `maximum` cells, its start giving way to `…`, so a
/// directory keeps its last components.
fn fit_start(text: &str, maximum: usize) -> String {
    if cells(text) <= maximum {
        return text.to_owned();
    }
    if maximum == 0 {
        return String::new();
    }
    let mut kept = Vec::new();
    let mut used = 0_usize;
    for character in text.chars().rev() {
        let width = character.width().unwrap_or(0).min(2);
        if used.saturating_add(width).saturating_add(1) > maximum {
            break;
        }
        kept.push(character);
        used = used.saturating_add(width);
    }
    std::iter::once('…').chain(kept.into_iter().rev()).collect()
}

/// `line` wrapped at its spaces to `maximum` cells; a word longer than a
/// line is cut with `…`. A blank line stays one blank line.
fn wrap(line: &str, maximum: usize) -> Vec<String> {
    let maximum = maximum.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_cells = 0_usize;
    for word in line.split_whitespace() {
        let word_cells = cells(word);
        let needed = if current.is_empty() {
            word_cells
        } else {
            current_cells.saturating_add(1).saturating_add(word_cells)
        };
        if needed <= maximum {
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
            current_cells = needed;
            continue;
        }
        if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        if word_cells <= maximum {
            current.push_str(word);
            current_cells = word_cells;
        } else {
            lines.push(fit_end(word, maximum));
            current_cells = 0;
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

/// One line of a prompt's body.
#[derive(Clone, Debug, Eq, PartialEq)]
enum BodyLine {
    /// A sentence's line; the first source line, the question, is primary.
    Text { text: String, primary: bool },
    /// A saved layout's row: the Dojo, the Splint and its directory.
    Row([String; 3]),
}

/// `body` as at most `budget` lines of `maximum` cells: each source line
/// wrapped, and a tab-separated one as a table row. When they do not all
/// fit, table rows give way first, to a `… N more` line, so the question
/// and the notes after the table stay; failing that, the last line that
/// fits is `…`.
fn body_lines(body: &str, maximum: usize, budget: usize) -> Vec<BodyLine> {
    let mut lines = Vec::new();
    for (index, source) in body.lines().enumerate() {
        if source.contains('\t') {
            let mut columns = source.split('\t').map(str::trim);
            lines.push(BodyLine::Row(
                [(); 3].map(|()| columns.next().unwrap_or_default().to_owned()),
            ));
        } else {
            lines.extend(
                wrap(source, maximum)
                    .into_iter()
                    .map(|text| BodyLine::Text {
                        text,
                        primary: index == 0,
                    }),
            );
        }
    }
    if lines.len() <= budget {
        return lines;
    }
    let is_row = |line: &BodyLine| matches!(line, BodyLine::Row(_));
    if let (Some(first), Some(last)) = (
        lines.iter().position(is_row),
        lines.iter().rposition(is_row),
    ) {
        // The rows to drop, and one more line for the `… N more`.
        let cut = lines.len().saturating_sub(budget).saturating_add(1);
        if cut <= last - first + 1 {
            let kept = last + 1 - cut;
            let mut fitted = lines[..kept].to_vec();
            fitted.push(BodyLine::Text {
                text: format!("… {cut} more"),
                primary: false,
            });
            fitted.extend_from_slice(&lines[last + 1..]);
            return fitted;
        }
    }
    lines.truncate(budget.saturating_sub(1));
    lines.push(BodyLine::Text {
        text: "…".to_owned(),
        primary: false,
    });
    lines
}

/// Each table column's width in cells within `maximum`: the Dojo and the
/// Splint as wide as their widest entry, at most a quarter and a third of
/// the line; the directory takes the rest.
fn column_cells(lines: &[BodyLine], maximum: usize) -> [usize; 3] {
    let widest = |column: usize| {
        lines
            .iter()
            .filter_map(|line| match line {
                BodyLine::Row(row) => Some(cells(&row[column])),
                BodyLine::Text { .. } => None,
            })
            .max()
            .unwrap_or(0)
    };
    let first = widest(0).min(maximum / 4);
    let second = widest(1).min(maximum / 3);
    let third = maximum
        .saturating_sub(first)
        .saturating_sub(second)
        .saturating_sub(COLUMN_GAP_CELLS * 2);
    [first, second, third]
}

/// Body line `index` of `body` (buffer px), clipped to the body.
fn line_rect(body: Rect, index: usize, line_height: u32) -> Option<Rect> {
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

/// The body: the question in primary text, the rest in secondary, and a
/// saved layout's rows in aligned columns, what each Splint launches in
/// primary, or in danger when it has nothing to launch.
fn paint_body(
    painter: &mut ChromePainter<'_>,
    palette: &SessionPickerPalette,
    body: Rect,
    source: &str,
    budget: usize,
) -> Result<()> {
    let (cell_width, _) = painter.cell(ChromeTextStyle::Regular)?;
    let cell_width = cell_width.max(1);
    let maximum = usize::try_from(body.width / cell_width).unwrap_or(usize::MAX);
    let line_height = painter.scaled(PROMPT_LINE_HEIGHT);
    let lines = body_lines(source, maximum, budget);
    let columns = column_cells(&lines, maximum);
    let to_px = |cells: usize| {
        u32::try_from(cells)
            .unwrap_or(u32::MAX)
            .saturating_mul(cell_width)
    };
    for (index, line) in lines.iter().enumerate() {
        let Some(rect) = line_rect(body, index, line_height) else {
            break;
        };
        match line {
            BodyLine::Text { text, primary } => painter.text(
                text,
                ChromeTextStyle::Regular,
                rect,
                PickerTextAlignment::Left,
                if *primary {
                    palette.primary
                } else {
                    palette.secondary
                },
            )?,
            BodyLine::Row(row) => {
                let right = rect.x.saturating_add(rect.width);
                let mut x = rect.x;
                for (column, (text, width)) in row.iter().zip(columns).enumerate() {
                    let fitted = if column == 2 {
                        fit_start(text, width)
                    } else {
                        fit_end(text, width)
                    };
                    let clip = Rect {
                        x,
                        width: to_px(width).min(right.saturating_sub(x)),
                        ..rect
                    };
                    if clip.width > 0 {
                        painter.text(
                            &fitted,
                            ChromeTextStyle::Regular,
                            clip,
                            PickerTextAlignment::Left,
                            match column {
                                1 if text == NOT_RESTORABLE => palette.danger,
                                1 => palette.primary,
                                _ => palette.secondary,
                            },
                        )?;
                    }
                    x = x
                        .saturating_add(to_px(width))
                        .saturating_add(to_px(COLUMN_GAP_CELLS));
                }
            }
        }
    }
    Ok(())
}

/// The sunken rename field: darker (or, on a light theme, lighter) than
/// the panel with a 1 px divider edge, so it never reads as a selected
/// row; the accent `›`, the name keeping its end, and a block cursor.
fn paint_field(
    painter: &mut ChromePainter<'_>,
    palette: &SessionPickerPalette,
    input: Rect,
    value: &str,
    selected: bool,
) -> Result<()> {
    painter.fill(input, opaque(palette.field));
    let border = painter.border();
    paint_outline(painter, input, border, palette.frame);
    let cell = painter.cell(ChromeTextStyle::Regular)?;
    let line = InputLineLayout::new(input, painter.scaled(FIELD_INSET), cell);
    painter.text(
        "›",
        ChromeTextStyle::Regular,
        line.marker,
        PickerTextAlignment::Left,
        palette.field_accent,
    )?;
    let Some((shown, cursor)) = line.fitted(input, value) else {
        return Ok(());
    };
    let text = Rect {
        x: line.text_x,
        width: cursor.x.saturating_sub(line.text_x),
        ..input
    };
    // Selected, as the name is when the prompt opens, the text is on the
    // selection fill: the first keystroke replaces it.
    if selected && !shown.is_empty() {
        painter.fill(
            Rect {
                y: cursor.y,
                height: cursor.height,
                ..text
            },
            opaque(palette.selected_fill),
        );
    }
    painter.text(
        &shown,
        ChromeTextStyle::Regular,
        text,
        PickerTextAlignment::Left,
        if selected {
            palette.selected_primary
        } else {
            palette.field_primary
        },
    )?;
    painter.fill(cursor, opaque(palette.field_accent));
    Ok(())
}

/// The buttons, `[ Cancel ]` first: the brackets at the edges and the
/// label centred between them.
fn paint_buttons(
    painter: &mut ChromePainter<'_>,
    palette: &SessionPickerPalette,
    layout: &DojoPromptLayout,
    state: &DojoPromptUi,
) -> Result<()> {
    let decision = state.decision().unwrap_or(TerminationDecision::Cancel);
    let buttons = if state.is_preview() {
        vec![(layout.cancel, "Close", true, false)]
    } else {
        vec![
            (
                layout.cancel,
                "Cancel",
                decision == TerminationDecision::Cancel,
                false,
            ),
            (
                layout.terminate,
                state.confirmation_label().unwrap_or("Confirm"),
                decision == TerminationDecision::Terminate,
                state.confirmation_destructive(),
            ),
        ]
    };
    let (cell_width, _) = painter.cell(ChromeTextStyle::Regular)?;
    for (rect, label, selected, destructive) in buttons {
        let Some(rect) = rect else {
            continue;
        };
        let rect = painter.buffer_rect(rect);
        let colors = button_colors(palette, selected, destructive);
        if let Some(fill) = colors.fill {
            painter.fill(rect, opaque(fill));
        }
        let padding = painter.scaled(BUTTON_PADDING).min(rect.width / 2);
        let inner = Rect {
            x: rect.x.saturating_add(padding),
            width: rect.width.saturating_sub(padding.saturating_mul(2)),
            ..rect
        };
        painter.text(
            "[",
            ChromeTextStyle::Regular,
            Rect {
                width: cell_width.min(inner.width),
                ..inner
            },
            PickerTextAlignment::Left,
            colors.bracket,
        )?;
        painter.text(
            "]",
            ChromeTextStyle::Regular,
            Rect {
                x: inner
                    .x
                    .saturating_add(inner.width.saturating_sub(cell_width)),
                width: cell_width.min(inner.width),
                ..inner
            },
            PickerTextAlignment::Left,
            colors.bracket,
        )?;
        let brackets = cell_width.saturating_mul(2);
        painter.text(
            label,
            ChromeTextStyle::Regular,
            Rect {
                x: inner.x.saturating_add(brackets),
                width: inner.width.saturating_sub(brackets.saturating_mul(2)),
                ..inner
            },
            PickerTextAlignment::Center,
            colors.label,
        )?;
    }
    Ok(())
}

/// A prompt in one box over the dimmed Window: its lowercase title in the
/// frame, the body, the field or the buttons, and its keys in the bottom
/// frame.
#[allow(
    clippy::too_many_arguments,
    reason = "trusted prompts share one bounded painter and composition transaction"
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
    let cell = painter.box_cell()?;
    let (title, body) = state.title_and_body();
    // A rename names what it renames in the frame, cut at its end.
    let target = state.rename_target();
    let right_label = target.as_deref().into_iter().collect::<Vec<_>>();
    let chrome = ChromeBox {
        title,
        right_label: &right_label,
        right_active: None,
        legend: prompt_legend(state),
    }
    .layout(painter.buffer_rect(layout.panel), scale_120, cell);
    chrome.paint_surface(&mut painter, &palette);
    if layout.lines > 0 {
        let body_rect = painter.buffer_rect(layout.body);
        paint_body(&mut painter, &palette, body_rect, &body, layout.lines)?;
    }
    if let (Some(value), Some(input)) = (state.input(), layout.input) {
        let input = painter.buffer_rect(input);
        paint_field(&mut painter, &palette, input, value, state.input_selected())?;
    } else {
        paint_buttons(&mut painter, &palette, layout, state)?;
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

#[cfg(test)]
mod tests {
    use splinterm_core::{DojoId, LairId, LairRetention, SplintId, TopologyRevision};
    use splinterm_protocol::AccessRevokeTarget;

    use super::{super::picker::picker_buffer_rect as buffer_rect, *};
    use crate::{
        color::{contrast_ratio, relative_luminance},
        config::ResolvedTheme,
        frontend::{LairPromptTarget, RevokeAccessUi},
        renderer::session_picker_palette,
    };

    const CONTENT: Rect = Rect {
        x: 0,
        y: 34,
        width: 1280,
        height: 766,
    };

    fn rename() -> DojoPromptUi {
        DojoPromptUi::rename(
            DojoId::new(),
            "captured".to_owned(),
            2,
            vec![(SplintId::new(), 1), (SplintId::new(), 2)],
        )
    }

    fn terminate() -> DojoPromptUi {
        DojoPromptUi::terminate(
            DojoId::new(),
            "Dojo 1".to_owned(),
            1,
            vec![(SplintId::new(), 1)],
        )
    }

    fn lair_target() -> LairPromptTarget {
        LairPromptTarget {
            topology_revision: TopologyRevision::new(9),
            lair_id: LairId::new(),
            dojo_id: None,
            name: "m5a".to_owned(),
            retention: LairRetention::Saved,
            preview: [
                "m5a · saved · 2 Dojos",
                "Dojo 1\tshell\t~/src/splinterm",
                "Dojo 1\tnvim src/main.rs\t~/src/splinterm",
                "build\tcargo watch\t/srv/a/very/long/path/to/the/splinterm",
                "Not restored: scrollback, running programs' state, shell state, environment, clipboard, images.",
            ]
            .join("\n"),
            targets: Vec::new(),
        }
    }

    fn paint(
        state: &DojoPromptUi,
        palette: SessionPickerPalette,
        cache: &mut SessionPickerTextCache,
    ) -> (DojoPromptLayout, Vec<u8>) {
        let context = RenderContext::new(u16::MAX);
        let metrics = dojo_prompt_metrics(cache, &context, state, CONTENT, 120, 1).unwrap();
        let layout = dojo_prompt_layout(CONTENT, state, metrics).unwrap();
        let mut canvas = vec![0_u8; 1280 * 800 * 4];
        paint_dojo_prompt(
            cache,
            &context,
            &mut canvas,
            1280,
            800,
            CONTENT,
            120,
            1,
            &layout,
            palette,
            state,
            true,
        )
        .unwrap();
        (layout, canvas)
    }

    fn pixel(canvas: &[u8], x: u32, y: u32) -> [u8; 3] {
        let index = usize::try_from((y * 1280 + x) * 4).unwrap();
        [canvas[index + 2], canvas[index + 1], canvas[index]]
    }

    fn rgb(color: u32) -> [u8; 3] {
        let [_, red, green, blue] = color.to_be_bytes();
        [red, green, blue]
    }

    fn paints(canvas: &[u8], rect: Rect, color: u32) -> bool {
        (rect.y..rect.y + rect.height)
            .any(|y| (rect.x..rect.x + rect.width).any(|x| pixel(canvas, x, y) == rgb(color)))
    }

    fn metrics(button_width: u32, body_lines: usize) -> DojoPromptMetrics {
        DojoPromptMetrics {
            button_width,
            body_lines,
        }
    }

    /// Every prompt is centred and 440 px wide; a rename is its field
    /// alone, a confirmation and a summary as many lines as their text, up
    /// to four and twelve; the buttons match, sit right-aligned 10 px apart
    /// with Cancel first, and are hit half-open. A Window too small for a
    /// prompt gets none.
    #[test]
    #[allow(clippy::too_many_lines, reason = "one geometry table and its edges")]
    fn prompts_are_centred_440_wide_with_right_aligned_buttons() {
        let inset = BOX_EDGE + PROMPT_INSET;
        let preview = DojoPromptUi::preview_lair(lair_target());
        let restore = DojoPromptUi::restore_lair(lair_target());
        let controls = PROMPT_GAP + BUTTON_HEIGHT;
        for (state, lines, height) in [
            (rename(), 0, inset * 2 + FIELD_HEIGHT),
            (
                terminate(),
                2,
                inset * 2 + PROMPT_LINE_HEIGHT * 2 + controls,
            ),
            (
                terminate(),
                9,
                inset * 2 + PROMPT_LINE_HEIGHT * 4 + controls,
            ),
            (restore, 7, inset * 2 + PROMPT_LINE_HEIGHT * 7 + controls),
            (preview, 30, inset * 2 + PROMPT_LINE_HEIGHT * 12 + controls),
        ] {
            let layout = dojo_prompt_layout(CONTENT, &state, metrics(120, lines)).unwrap();
            assert_eq!(layout.panel.width, PROMPT_WIDTH);
            assert_eq!(layout.panel.height, height);
            assert_eq!(layout.panel.x, (1280 - 440) / 2);
            assert_eq!(layout.panel.y, 34 + (766 - height) / 2);
            let right = layout.panel.x + layout.panel.width - inset;
            if state.is_rename() {
                let input = layout.input.unwrap();
                assert_eq!(input.x, layout.panel.x + inset);
                assert_eq!(input.x + input.width, right);
                assert!(layout.cancel.is_none() && layout.terminate.is_none());
                continue;
            }
            assert!(layout.input.is_none());
            let cancel = layout.cancel.unwrap();
            assert_eq!(cancel.width, 120);
            if state.is_preview() {
                assert!(layout.terminate.is_none());
                assert_eq!(cancel.x + cancel.width, right);
                continue;
            }
            let confirm = layout.terminate.unwrap();
            assert_eq!(confirm.width, 120);
            assert_eq!(confirm.x + confirm.width, right);
            assert_eq!(cancel.x + cancel.width + BUTTON_GAP, confirm.x);
            assert_eq!(confirm.y, cancel.y);
            let y = f64::from(cancel.y);
            assert_eq!(
                dojo_prompt_hit_test(&layout, (f64::from(cancel.x), y)),
                Some(TerminationDecision::Cancel)
            );
            assert_eq!(
                dojo_prompt_hit_test(&layout, (f64::from(cancel.x + cancel.width), y)),
                None,
                "the gap is no button"
            );
            assert_eq!(
                dojo_prompt_hit_test(&layout, (f64::from(confirm.x), y)),
                Some(TerminationDecision::Terminate)
            );
            assert_eq!(
                dojo_prompt_hit_test(&layout, (f64::from(confirm.x + confirm.width), y)),
                None
            );
        }
        // Narrower content: the prompt keeps its margin and its buttons
        // keep their whole labels; too narrow for both side by side, it is
        // not shown rather than show `[ … ]`.
        let context = RenderContext::new(u16::MAX);
        let mut cache = SessionPickerTextCache::default();
        let (cell, _) = cache
            .cell_metrics(&context, ChromeTextStyle::Regular, 120, 1)
            .unwrap();
        for (width, shown) in [(420, true), (300, false)] {
            let narrow = Rect { width, ..CONTENT };
            let measured =
                dojo_prompt_metrics(&mut cache, &context, &terminate(), narrow, 120, 1).unwrap();
            let Some(layout) = dojo_prompt_layout(narrow, &terminate(), measured) else {
                assert!(!shown, "{width} px shows the prompt");
                continue;
            };
            assert!(shown, "{width} px is too narrow for both buttons");
            assert_eq!(layout.panel.width, width - 48);
            let (cancel, confirm) = (layout.cancel.unwrap(), layout.terminate.unwrap());
            assert!(cancel.x >= layout.panel.x + inset);
            assert_eq!(cancel.width, confirm.width);
            assert!(
                confirm.width >= text_cells("[ Terminate ]") * cell + 2 * BUTTON_PADDING,
                "the label fits its button"
            );
        }
        // Too narrow, or too short for one body line: no prompt. Short
        // content shows fewer lines.
        let fixed = inset * 2 + controls;
        for small in [
            Rect {
                width: 180 + 47,
                ..CONTENT
            },
            Rect {
                height: fixed - 1 + 2 * 24,
                ..CONTENT
            },
            Rect {
                height: fixed + PROMPT_LINE_HEIGHT - 1 + 2 * 24,
                ..CONTENT
            },
        ] {
            assert!(dojo_prompt_layout(small, &terminate(), metrics(120, 2)).is_none());
        }
        for (extra, lines) in [(1, 1), (2, 2), (3, 2)] {
            let layout = dojo_prompt_layout(
                Rect {
                    height: fixed + PROMPT_LINE_HEIGHT * extra + 2 * 24,
                    ..CONTENT
                },
                &terminate(),
                metrics(120, 2),
            )
            .unwrap();
            assert_eq!(layout.lines, lines);
        }
    }

    /// Both buttons are as wide as the widest label in its brackets, at
    /// every scale and at the largest UI text; a rename has none.
    #[test]
    fn buttons_fit_their_widest_bracketed_label() {
        for context in [
            RenderContext::new(u16::MAX),
            RenderContext::new(u16::MAX).at_maximum_chrome_font_size(),
        ] {
            let mut cache = SessionPickerTextCache::default();
            for scale in [120, 150, 180, 240] {
                let (cell, _) = cache
                    .cell_metrics(&context, ChromeTextStyle::Regular, scale, 1)
                    .unwrap();
                let width =
                    dojo_prompt_metrics(&mut cache, &context, &terminate(), CONTENT, scale, 1)
                        .unwrap()
                        .button_width;
                let buffer = buffer_rect(
                    Rect {
                        x: 0,
                        y: 0,
                        width,
                        height: 1,
                    },
                    scale,
                )
                .width;
                let padding = BUTTON_PADDING * scale / 120;
                assert!(
                    buffer >= text_cells("[ Terminate ]") * cell + padding * 2,
                    "scale {scale}"
                );
                assert_eq!(
                    dojo_prompt_metrics(&mut cache, &context, &rename(), CONTENT, scale, 1)
                        .unwrap(),
                    DojoPromptMetrics::default()
                );
            }
        }
    }

    /// Cancel is chosen first, on the selection fill; Terminate keeps its
    /// label in danger and its brackets muted. Choosing Terminate fills it
    /// with danger and its label in the ink on it; Restore, chosen, takes
    /// the selection fill. The title is lowercase in the frame and the
    /// keys are in its bottom edge.
    #[test]
    #[allow(clippy::too_many_lines, reason = "one prompt paint scenario")]
    fn buttons_are_bracketed_and_a_chosen_terminate_is_on_the_danger_fill() {
        let palette = session_picker_palette(ResolvedTheme::default());
        let mut cache = SessionPickerTextCache::default();
        let mut state = terminate();
        let (layout, canvas) = paint(&state, palette, &mut cache);
        // The scrim dims the content area only.
        assert!(canvas[..1280 * 4 * 34].iter().all(|byte| *byte == 0));
        assert_ne!(canvas[(40 * 1280 + 4) * 4 + 3], 0, "scrim");
        for text in [
            "terminate dojo",
            "Terminate ‘Dojo 1’ and its 1 pane?",
            "Running programs are stopped.",
            "[",
            "]",
            "Cancel",
            "Terminate",
            "←→",
            "choose",
            "↵",
            "select",
            "esc",
            "cancel",
        ] {
            assert!(cache.contains_source(text), "{text} was not drawn");
        }
        let cancel = layout.cancel.unwrap();
        let confirm = layout.terminate.unwrap();
        let edge = |canvas: &[u8], rect: Rect| pixel(canvas, rect.x + 2, rect.y + 2);
        assert_eq!(edge(&canvas, cancel), rgb(palette.selected_fill));
        assert_eq!(edge(&canvas, confirm), rgb(palette.panel));
        assert!(paints(&canvas, confirm, palette.danger));
        assert!(paints(&canvas, cancel, palette.selected_accent));

        let DojoPromptUi::Terminate(confirmation) = &mut state else {
            unreachable!();
        };
        assert_eq!(confirmation.decision(), TerminationDecision::Cancel);
        assert!(confirmation.move_selection());
        let (_, canvas) = paint(&state, palette, &mut cache);
        assert_eq!(edge(&canvas, confirm), rgb(palette.danger_fill));
        assert_eq!(edge(&canvas, cancel), rgb(palette.panel));
        assert!(paints(&canvas, confirm, palette.danger_ink));
        assert!(paints(&canvas, cancel, palette.primary));

        let mut restore = DojoPromptUi::restore_lair(lair_target());
        let DojoPromptUi::RestoreLair(confirmation) = &mut restore else {
            unreachable!();
        };
        assert!(confirmation.move_selection());
        let (layout, canvas) = paint(&restore, palette, &mut cache);
        let confirm = layout.terminate.unwrap();
        assert_eq!(edge(&canvas, confirm), rgb(palette.selected_fill));
        assert!(!paints(&canvas, confirm, palette.danger));
        assert!(!restore.confirmation_destructive());

        // Terminate Lair is destructive like Terminate: the danger fill.
        let mut terminate_lair = DojoPromptUi::terminate_lair(lair_target());
        assert!(terminate_lair.confirmation_destructive());
        let DojoPromptUi::TerminateLair(confirmation) = &mut terminate_lair else {
            unreachable!();
        };
        assert!(confirmation.move_selection());
        let (layout, canvas) = paint(&terminate_lair, palette, &mut cache);
        let confirm = layout.terminate.unwrap();
        assert_eq!(edge(&canvas, confirm), rgb(palette.danger_fill));
        assert!(paints(&canvas, confirm, palette.danger_ink));

        // Revoke is not: chosen, it takes the selection fill.
        let mut revoke = DojoPromptUi::RevokeAccess(revoke_entry(&["claude-read"]));
        assert!(!revoke.confirmation_destructive());
        let DojoPromptUi::RevokeAccess(confirmation) = &mut revoke else {
            unreachable!();
        };
        assert!(confirmation.move_selection());
        let (layout, canvas) = paint(&revoke, palette, &mut cache);
        let confirm = layout.terminate.unwrap();
        assert_eq!(edge(&canvas, confirm), rgb(palette.selected_fill));
        assert!(!paints(&canvas, confirm, palette.danger_fill));

        assert_eq!(
            button_colors(&palette, true, true),
            ButtonColors {
                fill: Some(palette.danger_fill),
                bracket: palette.danger_ink,
                label: palette.danger_ink,
            }
        );
        assert_eq!(
            button_colors(&palette, false, true),
            ButtonColors {
                fill: None,
                bracket: palette.secondary,
                label: palette.danger,
            }
        );
        assert_eq!(
            button_colors(&palette, true, false).fill,
            Some(palette.selected_fill)
        );
        assert_eq!(button_colors(&palette, false, false).label, palette.primary);
    }

    /// A rename is its sunken field: the field fill inside a 1 px divider
    /// edge, unlike the panel and the selection, with the accent `›` and
    /// block cursor; the keys say save and cancel, and the frame names what
    /// it renames. The name opens selected, on the selection fill, so the
    /// first keystroke visibly replaces it.
    #[test]
    fn the_rename_field_is_sunken_with_a_block_cursor() {
        let palette = session_picker_palette(ResolvedTheme::default());
        let mut cache = SessionPickerTextCache::default();
        let mut state = rename();
        assert!(state.input_selected());
        assert_eq!(state.rename_target().as_deref(), Some("captured"));
        let (layout, canvas) = paint(&state, palette, &mut cache);
        let input = layout.input.unwrap();
        assert_ne!(palette.field, palette.panel);
        assert_ne!(palette.field, palette.selected_fill);
        assert_eq!(
            pixel(&canvas, input.x, input.y + input.height / 2),
            rgb(palette.frame)
        );
        assert_eq!(
            pixel(
                &canvas,
                input.x + input.width - 3,
                input.y + input.height / 2
            ),
            rgb(palette.field)
        );
        assert!(paints(&canvas, input, palette.field_accent), "the cursor");
        for text in ["rename dojo", "›", "captured", "save", "cancel"] {
            assert!(cache.contains_source(text), "{text} was not drawn");
        }
        assert!(!cache.contains_source("Cancel"), "a rename has no buttons");
        assert!(paints(&canvas, input, palette.selected_fill), "selected");
        assert!(state.editor_mut().unwrap().insert("x"));
        assert!(!state.input_selected());
        let (_, canvas) = paint(&state, palette, &mut cache);
        assert!(!paints(&canvas, input, palette.selected_fill));
        assert!(cache.contains_source("x"));
    }

    /// The field is sunken away from the text: darker on a dark theme,
    /// lighter on a light one; its text, the danger ink and the accent stay
    /// readable on their fills.
    #[test]
    fn the_field_sinks_away_from_the_text_and_stays_readable() {
        let mut dark = ResolvedTheme::default();
        dark.roles.surface = 0x1a_1b_26;
        dark.roles.surface_raised = 0x24_28_3b;
        dark.roles.text = 0xc0_ca_f5;
        dark.roles.danger = 0xf7_76_8e;
        let mut light = dark;
        light.roles.surface = 0xef_f1_f5;
        light.roles.surface_raised = 0xe6_e9_ef;
        light.roles.text = 0x4c_4f_69;
        light.roles.text_muted = 0x6c_6f_85;
        light.roles.selection = 0xcc_d0_da;
        light.roles.accent = 0x1e_66_f5;
        light.roles.danger = 0xd2_0f_39;
        light.roles.divider = 0xcd_d0_da;
        for (theme, darker) in [(dark, true), (light, false)] {
            let palette = session_picker_palette(theme);
            assert_eq!(
                relative_luminance(palette.field) < relative_luminance(palette.panel),
                darker
            );
            assert!(contrast_ratio(palette.field_primary, palette.field) >= 4.5);
            assert!(contrast_ratio(palette.field_accent, palette.field) >= 4.5);
            assert!(contrast_ratio(palette.danger_ink, palette.danger_fill) >= 4.5);
        }
    }

    /// A saved layout reads as a table: each row's Dojo, what it launches
    /// and its directory in aligned columns, a long directory keeping its
    /// end; the question first, in primary text, and the summary line,
    /// which repeats it, left out. Too many rows give way to `… N more`,
    /// keeping the notes after them.
    #[test]
    #[allow(clippy::too_many_lines, reason = "one table scenario")]
    fn previews_are_an_aligned_table() {
        let restore = DojoPromptUi::restore_lair(lair_target());
        let (title, body) = restore.title_and_body();
        assert_eq!(title, "restore saved lair");
        assert!(body.starts_with("Restore 0 captured Splints into ‘m5a’?\nDojo 1\t"));
        let lines = body_lines(&body, 48, SUMMARY_LINES);
        assert_eq!(
            lines[0],
            BodyLine::Text {
                text: "Restore 0 captured Splints into ‘m5a’?".to_owned(),
                primary: true
            }
        );
        assert!(matches!(&lines[1], BodyLine::Row(_)));
        let mut dojo = lair_target();
        dojo.dojo_id = Some(DojoId::new());
        assert_eq!(
            DojoPromptUi::restore_lair(dojo).title_and_body().0,
            "restore dojo"
        );
        let rows = lines
            .iter()
            .filter_map(|line| match line {
                BodyLine::Row(row) => Some(row.clone()),
                BodyLine::Text { .. } => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2][1], "cargo watch");
        let columns = column_cells(&lines, 48);
        assert_eq!(columns, [6, 16, 48 - 6 - 16 - 4]);
        let directory = fit_start(&rows[2][2], columns[2]);
        assert!(directory.starts_with('…') && directory.ends_with("/the/splinterm"));
        assert_eq!(cells(&directory), columns[2]);
        assert_eq!(fit_end("cargo watch --all", 11), "cargo watc…");
        // The note wraps at its spaces.
        assert!(lines.len() > 5);
        assert!(lines.iter().all(|line| match line {
            BodyLine::Text { text, .. } => cells(text) <= 48,
            BodyLine::Row(_) => true,
        }));

        // Too many rows for the body: rows give way to `… N more`, and the
        // question and the note after them stay.
        let many = std::iter::once("Restore 20 captured Splints into ‘m5a’?".to_owned())
            .chain((0..20).map(|index| format!("Dojo\tpane {index}\t/tmp")))
            .chain(std::iter::once("Not restored: images.".to_owned()))
            .collect::<Vec<_>>()
            .join("\n");
        let lines = body_lines(&many, 48, SUMMARY_LINES);
        assert_eq!(lines.len(), SUMMARY_LINES);
        assert!(matches!(&lines[0], BodyLine::Text { primary: true, .. }));
        assert_eq!(
            lines[SUMMARY_LINES - 2],
            BodyLine::Text {
                text: "… 11 more".to_owned(),
                primary: false
            }
        );
        assert_eq!(
            lines.last(),
            Some(&BodyLine::Text {
                text: "Not restored: images.".to_owned(),
                primary: false
            })
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| matches!(line, BodyLine::Row(_)))
                .count(),
            SUMMARY_LINES - 3
        );
        // Text alone that does not fit ends in `…`.
        let prose = body_lines(&"word ".repeat(200), 48, 4);
        assert_eq!(prose.len(), 4);
        assert_eq!(
            prose.last(),
            Some(&BodyLine::Text {
                text: "…".to_owned(),
                primary: false
            })
        );
        // A word wider than a line is cut, and a blank line stays.
        assert_eq!(wrap(&"x".repeat(60), 10), ["xxxxxxxxx…"]);
        assert_eq!(wrap("", 10), [""]);

        // Painted: the Splint in primary, its Dojo and directory muted.
        let palette = session_picker_palette(ResolvedTheme::default());
        let mut cache = SessionPickerTextCache::default();
        let preview = DojoPromptUi::preview_lair(lair_target());
        let (layout, _) = paint(&preview, palette, &mut cache);
        for text in [
            "saved lair preview",
            "m5a · saved · 2 Dojos",
            "cargo watch",
            "build",
            "Close",
            "close",
        ] {
            assert!(cache.contains_source(text), "{text} was not drawn");
        }
        assert!(layout.terminate.is_none());
        // Every line is shown: the body is as tall as its text.
        let measured = dojo_prompt_metrics(
            &mut cache,
            &RenderContext::new(u16::MAX),
            &preview,
            CONTENT,
            120,
            1,
        )
        .unwrap();
        assert_eq!(layout.lines, measured.body_lines);
        assert!((5..=SUMMARY_LINES).contains(&layout.lines));

        // A Splint with nothing to launch says so, in danger.
        let mut broken = lair_target();
        broken.preview = format!("m5a · saved · 1 Dojo\nDojo 1\t{NOT_RESTORABLE}\t/tmp");
        let (_, canvas) = paint(&DojoPromptUi::preview_lair(broken), palette, &mut cache);
        assert!(cache.contains_source(NOT_RESTORABLE));
        assert!(paints(
            &canvas,
            Rect {
                x: 0,
                y: 0,
                width: 1280,
                height: 800
            },
            palette.danger
        ));
    }

    fn revoke_entry(rules: &[&str]) -> RevokeAccessUi {
        let mut entry = crate::frontend::agent_access_test_entry(LairId::new());
        entry.policy_rules = rules.iter().map(|rule| (*rule).to_owned()).collect();
        RevokeAccessUi::entry(
            AccessRevokeTarget::Remembered {
                entry_id: format!("ra_{}", "0123456789abcdef".repeat(2)),
            },
            entry,
        )
    }

    /// The text of `lines`, one string per line.
    fn texts(lines: &[BodyLine]) -> Vec<String> {
        lines
            .iter()
            .map(|line| match line {
                BodyLine::Text { text, .. } => text.clone(),
                BodyLine::Row(row) => row.join("\t"),
            })
            .collect()
    }

    /// A destructive question keeps its whole text at ordinary name lengths
    /// on a 42-cell body, then what it means; a name too long for a line is
    /// cut so the count and `?` stay. A revoke keeps its policy line. A name
    /// cannot add a line or a table row.
    #[test]
    fn questions_keep_their_end_and_names_stay_on_one_line() {
        let context = RenderContext::new(u16::MAX);
        let mut cache = SessionPickerTextCache::default();
        for name in [
            "n".repeat(20),
            "lair ".repeat(8).trim_end().to_owned(),
            "x".repeat(40),
        ] {
            let mut target = lair_target();
            target.name.clone_from(&name);
            target.targets = vec![
                splinterm_protocol::MutationTarget {
                    lair_id: target.lair_id,
                    dojo_id: DojoId::new(),
                    splint_id: SplintId::new(),
                    incarnation: 1,
                };
                3
            ];
            let state = DojoPromptUi::terminate_lair(target);
            let (_, body) = state.title_and_body();
            let lines = body_lines(&body, 42, CONFIRM_LINES);
            let shown = texts(&lines);
            assert_eq!(
                shown.last().map(String::as_str),
                Some("Running programs are stopped."),
                "{name}"
            );
            let question = shown[..shown.len() - 1].join(" ");
            assert_eq!(
                question,
                format!("Terminate ‘{name}’ and its 3 captured panes?"),
                "{name}"
            );
            // Measured, the prompt is as tall as that text.
            let measured =
                dojo_prompt_metrics(&mut cache, &context, &state, CONTENT, 120, 1).unwrap();
            assert!(measured.body_lines <= CONFIRM_LINES, "{name}");
        }
        let long = DojoPromptUi::terminate(DojoId::new(), "y".repeat(90), 2, Vec::new());
        let (_, body) = long.title_and_body();
        let question = body.lines().next().unwrap();
        assert!(question.ends_with("…’ and its 2 panes?"), "{question}");

        let revoke = DojoPromptUi::RevokeAccess(revoke_entry(&["claude-read"]));
        let (_, body) = revoke.title_and_body();
        let shown = texts(&body_lines(&body, 42, SUMMARY_LINES)).join(" ");
        assert!(
            shown.contains("Access continues under policy rule")
                && shown.ends_with("to remove it."),
            "{shown}"
        );

        let spoof = DojoPromptUi::terminate(
            DojoId::new(),
            "a\nRunning\tb\u{202e}c\u{2028}d".to_owned(),
            1,
            Vec::new(),
        );
        let (_, body) = spoof.title_and_body();
        let lines = body_lines(&body, 120, CONFIRM_LINES);
        assert_eq!(
            texts(&lines),
            [
                "Terminate ‘a Running bcd’ and its 1 pane?",
                "Running programs are stopped."
            ]
        );
        assert!(
            lines
                .iter()
                .all(|line| matches!(line, BodyLine::Text { .. }))
        );
    }

    /// Body lines stay inside the body at a fractional scale.
    #[test]
    fn fractional_body_lines_stay_inside_the_body() {
        let body = buffer_rect(
            Rect {
                x: 24,
                y: 36,
                width: 400,
                height: PROMPT_LINE_HEIGHT * 7,
            },
            121,
        );
        let line_height = PROMPT_LINE_HEIGHT * 121 / 120 + 1;
        for index in 0..SUMMARY_LINES {
            let Some(line) = line_rect(body, index, line_height) else {
                break;
            };
            assert!(line.y >= body.y);
            assert!(line.y + line.height <= body.y + body.height);
        }
    }

    /// Every fixed prompt line is tall enough for the largest UI text.
    #[test]
    fn fixed_prompt_lines_fit_the_largest_ui_text() {
        use super::super::super::text::CHROME_MAXIMUM_LINE_HEIGHT;

        for height in [PROMPT_LINE_HEIGHT, FIELD_HEIGHT, BUTTON_HEIGHT] {
            assert!(height >= CHROME_MAXIMUM_LINE_HEIGHT);
        }
    }
}
