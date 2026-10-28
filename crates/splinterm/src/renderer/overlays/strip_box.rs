//! The strips in the popup box (Plan 0065 milestone 6): terminal search at
//! the bottom left of the terminal area, the history position at its top
//! right and the upgrade banner at its top centre, each one row of content
//! in the shared box with its hard shadow and no scrim.
//!
//! Search takes keys, so its frame is the focused one while the Window has
//! focus, with the match count in the frame and its keys in the bottom edge.
//! History is passive: a muted title over the divider frame, its position as
//! text and the return chips, sized to what it shows; a click anywhere on it
//! returns to live. The banner's frame says how the upgrade is going:
//! warning with a spinner while it runs, danger once the connection is lost,
//! the divider once it has settled.

use anyhow::Result;

use crate::{
    geometry::Rect,
    handoff_status::{HandoffBanner, HandoffNotice},
};

use super::{
    super::{ChromeTextStyle, RenderContext},
    actions::InputLineLayout,
    chrome_box::{BOX_EDGE, BOX_SHADOW_OFFSET, ChromeBox, ChromePainter, opaque},
    chrome_rows::{ROW_HEIGHT, ROW_PADDING, chips_width, paint_key_chips, text_cells},
    picker::{PickerTextAlignment, SessionPickerPalette, SessionPickerTextCache},
};

/// Logical px between a strip and the terminal area's edges.
const STRIP_MARGIN: u32 = 8;
/// Logical px between the inside of the frame and the content row, above
/// and below it.
const STRIP_PADDING: u32 = 10;
/// A strip's height, logical px: the frame's edges, the padding and one
/// row.
pub(crate) const STRIP_HEIGHT: u32 = (BOX_EDGE + STRIP_PADDING) * 2 + ROW_HEIGHT;
/// Logical px from the panel's side to the content.
const STRIP_INSET: u32 = BOX_EDGE + ROW_PADDING;
/// The search strip is this wide at most, logical px.
const SEARCH_WIDTH: u32 = 440;
/// A search strip narrower than this is not shown.
const SEARCH_MINIMUM_WIDTH: u32 = 160;
/// The history strip is at least this wide, logical px.
const HISTORY_WIDTH: u32 = 240;
/// A history strip narrower than this is not shown.
const HISTORY_MINIMUM_WIDTH: u32 = 180;
/// The longest position the history strip shows, which sizes it: counts
/// clamp at 999.
const HISTORY_WORST_TEXT: &str = "↑ 999/999  +999 new";
/// What the history strip shows to return to live without a binding.
const RETURN_LIVE: &str = "↓ live";
/// A banner narrower than this is not shown.
const BANNER_MINIMUM_WIDTH: u32 = 160;

fn scaled(logical: u32, scale_120: u32) -> u32 {
    logical.saturating_mul(scale_120).div_ceil(120).max(1)
}

/// What a strip at `panel` (buffer px) covers: the panel and its shadow,
/// so the Window damages both where it was and where it is.
#[must_use]
pub(crate) fn strip_bounds(panel: Rect, scale_120: u32) -> Rect {
    let offset = BOX_SHADOW_OFFSET.saturating_mul(scale_120).div_ceil(120);
    Rect {
        width: panel.width.saturating_add(offset),
        height: panel.height.saturating_add(offset),
        ..panel
    }
}

/// The content row inside a strip's frame, buffer px.
fn content_row(panel: Rect, scale_120: u32) -> Rect {
    let x = scaled(STRIP_INSET, scale_120).min(panel.width / 2);
    let y = scaled(BOX_EDGE + STRIP_PADDING, scale_120).min(panel.height / 2);
    Rect {
        x: panel.x.saturating_add(x),
        y: panel.y.saturating_add(y),
        width: panel.width.saturating_sub(x.saturating_mul(2)),
        height: panel.height.saturating_sub(y.saturating_mul(2)),
    }
}

fn cells_px(cells: u32, cell_width: u32) -> u32 {
    cells.saturating_mul(cell_width)
}

/// The terminal search strip's panel, in buffer pixels, at the bottom left
/// of `terminal`; `None` when the terminal is too small to show it. The
/// painter and the Window's surface damage both use it, so a frame that
/// removes or moves the strip damages exactly where it was drawn.
#[must_use]
pub(crate) fn search_overlay_panel(terminal: Rect, scale_120: u32) -> Option<Rect> {
    if scale_120 == 0 {
        return None;
    }
    let margin = scaled(STRIP_MARGIN, scale_120);
    let width = terminal
        .width
        .saturating_sub(margin.saturating_mul(2))
        .min(scaled(SEARCH_WIDTH, scale_120));
    let height = scaled(STRIP_HEIGHT, scale_120);
    if width < scaled(SEARCH_MINIMUM_WIDTH, scale_120)
        || terminal.height < height.saturating_add(margin.saturating_mul(2))
    {
        return None;
    }
    Some(Rect {
        x: terminal.x.saturating_add(margin),
        y: terminal.y.saturating_add(
            terminal
                .height
                .saturating_sub(margin)
                .saturating_sub(height),
        ),
        width,
        height,
    })
}

/// What the search strip shows.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SearchStrip<'a> {
    /// The query as typed.
    pub(crate) input: &'a str,
    /// The query last searched for.
    pub(crate) searched_query: &'a str,
    pub(crate) matches: usize,
    pub(crate) selected: usize,
    /// Older history is still being searched.
    pub(crate) searching: bool,
    /// A search was sent and is not answered yet: the results shown are
    /// an earlier query's.
    pub(crate) pending: bool,
}

impl SearchStrip<'_> {
    /// Whether the typed query is the one the results answer.
    fn searched(&self) -> bool {
        !self.searched_query.is_empty() && self.input == self.searched_query
    }

    /// The count in the frame: the current match and how many, as the
    /// palette counts (`2/17`), once the typed query is searched; nothing
    /// before, and `searching…` while an answer is due, so an earlier
    /// query's count never shows under a new one.
    fn count(&self) -> Option<String> {
        if !self.searched() {
            return None;
        }
        Some(match (self.matches, self.searching || self.pending) {
            (_, true) => "searching…".to_owned(),
            (0, false) => "no matches".to_owned(),
            (matches, false) => {
                format!("{}/{matches}", self.selected.saturating_add(1).min(matches))
            }
        })
    }

    /// The keys in the bottom frame: moving between matches once searched
    /// and answered, searching before.
    fn legend(&self) -> &'static [(&'static str, &'static str)] {
        if self.searched() && self.matches > 0 && !self.searching && !self.pending {
            &[("ctrl n", "next"), ("ctrl p", "prev"), ("esc", "close")]
        } else {
            &[("↵", "search"), ("esc", "close")]
        }
    }
}

/// Paints the search strip inside `terminal`, the terminal side of any
/// Explorer split: `search` in the frame with the count at its right, the
/// accent `›`, the query and a block cursor, and the keys in the bottom
/// edge. Returns whether it was drawn.
#[allow(
    clippy::too_many_arguments,
    reason = "the bounded search strip keeps geometry, palette and cache in one paint seam"
)]
pub(crate) fn paint_search_overlay(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    canvas: &mut [u8],
    canvas_width: u32,
    canvas_height: u32,
    terminal: Rect,
    scale_120: u32,
    renderer_generation: u64,
    palette: SessionPickerPalette,
    search: SearchStrip<'_>,
    keyboard_focused: bool,
) -> Result<bool> {
    let Some(panel) = search_overlay_panel(terminal, scale_120) else {
        return Ok(false);
    };
    let mut painter = ChromePainter::new(
        cache,
        context,
        canvas,
        canvas_width,
        canvas_height,
        scale_120,
        renderer_generation,
    );
    let cell = painter.box_cell()?;
    let count = search.count();
    let right_label = count.as_deref().into_iter().collect::<Vec<_>>();
    let chrome = ChromeBox {
        title: "search",
        right_label: &right_label,
        right_active: None,
        legend: search.legend(),
    }
    .layout(panel, scale_120, cell);
    chrome.paint_surface(&mut painter, &palette);
    let row = content_row(panel, scale_120);
    let regular = painter.cell(ChromeTextStyle::Regular)?;
    let line = InputLineLayout::new(row, 0, regular);
    painter.text(
        "›",
        ChromeTextStyle::Regular,
        line.marker,
        PickerTextAlignment::Left,
        palette.accent_text,
    )?;
    let query = search
        .input
        .chars()
        .filter(|character| !character.is_control())
        .take(256)
        .collect::<String>();
    if let Some((shown, cursor)) = line.fitted(row, &query) {
        painter.text(
            &shown,
            ChromeTextStyle::Regular,
            Rect {
                x: line.text_x,
                width: cursor.x.saturating_sub(line.text_x),
                ..row
            },
            PickerTextAlignment::Left,
            palette.primary,
        )?;
        painter.fill(cursor, opaque(palette.selected_rail));
        if query.is_empty() {
            let after = cursor.x.saturating_add(cursor.width);
            painter.text(
                "search scrollback",
                ChromeTextStyle::Regular,
                Rect {
                    x: after,
                    width: line.end.saturating_sub(after),
                    ..row
                },
                PickerTextAlignment::Left,
                palette.secondary,
            )?;
        }
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
    Ok(true)
}

/// Where the history strip goes in a content area of `width` by `height`,
/// relative to it, in the same px as those.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HistoryOverlayLayout {
    /// The strip; a click anywhere on it returns to live, so a click on
    /// it never starts a selection in the terminal it hides.
    pub(crate) panel: (i32, i32, u32, u32),
    /// The return chips' end of the strip, its full height.
    pub(crate) return_to_live: (i32, i32, u32, u32),
}

/// Where the history position stands while the view is scrolled back.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HistoryOverlayStatus {
    pub(crate) offset_from_bottom: usize,
    pub(crate) available_rows: usize,
    pub(crate) unseen_rows: usize,
}

/// The history strip's return content, buffer px wide: `return_keys` as
/// chips with `cell` (the chrome text cell), or `↓ live` without a binding.
#[must_use]
fn history_return_width(return_keys: &[String], cell: (u32, u32), scale_120: u32) -> u32 {
    if return_keys.is_empty() {
        cells_px(text_cells(RETURN_LIVE), cell.0)
    } else {
        chips_width(return_keys, cell, scale_120)
    }
}

/// The history strip at the top right of a `width` by `height` content
/// area at `scale_120`, sized to its content: the longest position it can
/// show in `cell_width` cells, a cell's gap, and its return content
/// `return_width` wide ([`history_return_width`]), all buffer px. `None`
/// when the area is too small for it. A pure function of its arguments,
/// so the painter and the Window's damage agree.
#[must_use]
pub(crate) fn history_overlay_layout(
    width: u32,
    height: u32,
    scale_120: u32,
    cell_width: u32,
    return_width: u32,
) -> Option<HistoryOverlayLayout> {
    if width == 0 || height == 0 || scale_120 == 0 {
        return None;
    }
    let margin = scaled(STRIP_MARGIN, scale_120)
        .min(width / 4)
        .min(height / 4);
    let inset = scaled(STRIP_INSET, scale_120);
    // The return area: its content and the inset at the panel's end.
    let return_area = return_width.saturating_add(inset);
    let content = inset
        .saturating_mul(2)
        .saturating_add(cells_px(text_cells(HISTORY_WORST_TEXT), cell_width))
        .saturating_add(cell_width)
        .saturating_add(return_area);
    let panel_width = scaled(HISTORY_WIDTH, scale_120)
        .max(content)
        .min(width.saturating_sub(margin.saturating_mul(2)));
    let panel_height = scaled(STRIP_HEIGHT, scale_120);
    if panel_width < scaled(HISTORY_MINIMUM_WIDTH, scale_120)
        || height < panel_height.saturating_add(margin.saturating_mul(2))
    {
        return None;
    }
    let x = width.saturating_sub(margin).saturating_sub(panel_width);
    let action_width = return_area.min(panel_width);
    Some(HistoryOverlayLayout {
        panel: (
            i32::try_from(x).ok()?,
            i32::try_from(margin).ok()?,
            panel_width,
            panel_height,
        ),
        return_to_live: (
            i32::try_from(x.saturating_add(panel_width.saturating_sub(action_width))).ok()?,
            i32::try_from(margin).ok()?,
            action_width,
            panel_height,
        ),
    })
}

fn tuple_rect((x, y, width, height): (i32, i32, u32, u32)) -> Rect {
    Rect {
        x: u32::try_from(x).unwrap_or(0),
        y: u32::try_from(y).unwrap_or(0),
        width,
        height,
    }
}

/// The history position in at most `room_cells` cells, whole or not at all:
/// `17/351`, then `17` once the total does not fit, then nothing.
fn history_position(status: HistoryOverlayStatus, room_cells: u32) -> Option<String> {
    let offset = status.offset_from_bottom.min(999);
    [
        format!("{offset}/{}", status.available_rows.min(999)),
        offset.to_string(),
    ]
    .into_iter()
    .find(|text| text_cells(text) <= room_cells)
}

/// Paints the history strip over `area`, the content area (buffer px): a muted `history`
/// title over the divider frame, the accent `↑`, how far back the view
/// is, how many rows arrived since in muted text when they fit, and
/// `return_keys` as chips at its right end, or `↓ live` with no binding
/// or when the chips do not fit whole. Returns its layout, offset into the
/// canvas.
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "content-relative trusted strip painting keeps geometry, palette and cache explicit"
)]
pub(crate) fn paint_history_overlay(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    canvas: &mut [u8],
    width: u32,
    height: u32,
    area: Rect,
    scale_120: u32,
    renderer_generation: u64,
    palette: SessionPickerPalette,
    status: HistoryOverlayStatus,
    return_keys: &[String],
) -> Result<Option<HistoryOverlayLayout>> {
    if scale_120 == 0 {
        return Ok(None);
    }
    let (Ok(offset_x), Ok(offset_y)) = (i32::try_from(area.x), i32::try_from(area.y)) else {
        return Ok(None);
    };
    let mut painter = ChromePainter::new(
        cache,
        context,
        canvas,
        width,
        height,
        scale_120,
        renderer_generation,
    );
    let regular = painter.cell(ChromeTextStyle::Regular)?;
    let cell_width = regular.0.max(1);
    let Some(mut layout) = history_overlay_layout(
        area.width,
        area.height,
        scale_120,
        cell_width,
        history_return_width(return_keys, regular, scale_120),
    ) else {
        painter.finish();
        return Ok(None);
    };
    for rect in [&mut layout.panel, &mut layout.return_to_live] {
        rect.0 = rect.0.saturating_add(offset_x);
        rect.1 = rect.1.saturating_add(offset_y);
    }
    let panel = tuple_rect(layout.panel);
    let cell = painter.box_cell()?;
    let chrome = ChromeBox {
        title: "history",
        right_label: &[],
        right_active: None,
        legend: &[],
    }
    .layout(panel, scale_120, cell);
    chrome.paint_surface(&mut painter, &palette);
    let row = content_row(panel, scale_120);
    let row_right = row.x.saturating_add(row.width);
    let action = tuple_rect(layout.return_to_live);
    let room = Rect {
        x: action.x.max(row.x),
        width: row_right.saturating_sub(action.x.max(row.x)),
        ..row
    };
    // Chips only whole: a `… end` would hide which keys return.
    let chips_fit =
        !return_keys.is_empty() && chips_width(return_keys, regular, scale_120) <= room.width;
    let chips_start = if chips_fit {
        paint_key_chips(&mut painter, &palette, return_keys, room, true)?
    } else {
        None
    };
    if chips_start.is_none() && cells_px(text_cells(RETURN_LIVE), cell_width) <= room.width {
        painter.text(
            RETURN_LIVE,
            ChromeTextStyle::Regular,
            room,
            PickerTextAlignment::Right,
            palette.accent_text,
        )?;
    }
    let text_end = room.x.saturating_sub(cell_width);
    let room_cells = |from: u32| text_end.saturating_sub(from) / cell_width;
    let at = |cells: u32| row.x.saturating_add(cells_px(cells, cell_width));
    let clip = |from: u32, cells: u32| Rect {
        x: from,
        width: cells_px(cells, cell_width),
        ..row
    };
    if room_cells(row.x) >= 1 {
        painter.text(
            "↑",
            ChromeTextStyle::Regular,
            clip(row.x, 1),
            PickerTextAlignment::Left,
            palette.accent_text,
        )?;
    }
    // Each part is shown whole or not at all, the total dropped first.
    if let Some(position) = history_position(status, room_cells(at(2))) {
        let position_cells = text_cells(&position);
        painter.text(
            &position,
            ChromeTextStyle::Regular,
            clip(at(2), position_cells),
            PickerTextAlignment::Left,
            palette.primary,
        )?;
        if status.unseen_rows > 0 {
            let unseen = format!("+{} new", status.unseen_rows.min(999));
            let from = at(2 + position_cells + 2);
            let unseen_cells = text_cells(&unseen);
            if unseen_cells <= room_cells(from) {
                painter.text(
                    &unseen,
                    ChromeTextStyle::Regular,
                    clip(from, unseen_cells),
                    PickerTextAlignment::Left,
                    palette.secondary,
                )?;
            }
        }
    }
    chrome.paint_frame_titled(
        &mut painter,
        &palette,
        palette.frame,
        (ChromeTextStyle::Regular, palette.secondary),
    )?;
    painter.finish();
    Ok(Some(layout))
}

/// Where the banner goes in buffer pixels, its content `content_width`
/// buffer px wide, inside `terminal` and clear of `avoid` (the history
/// strip): left of it when that shares the banner's band and leaves a
/// legible banner room, else in the band below it. `None` when the
/// terminal is too small for a legible banner.
#[must_use]
pub(crate) fn handoff_banner_layout(
    terminal: Rect,
    avoid: Option<Rect>,
    content_width: u32,
    scale_120: u32,
) -> Option<Rect> {
    if scale_120 == 0 || terminal.width == 0 || terminal.height == 0 {
        return None;
    }
    let margin = scaled(STRIP_MARGIN, scale_120);
    let height = scaled(STRIP_HEIGHT, scale_120);
    if terminal.height < height.saturating_add(margin.saturating_mul(2)) {
        return None;
    }
    let minimum = scaled(BANNER_MINIMUM_WIDTH, scale_120);
    let mut y = terminal.y.saturating_add(margin);
    let left = terminal.x.saturating_add(margin);
    let mut right = terminal
        .x
        .saturating_add(terminal.width)
        .saturating_sub(margin);
    if let Some(avoid) = avoid {
        let band = y..y.saturating_add(height);
        let shares_band = avoid.y < band.end && avoid.y.saturating_add(avoid.height) > band.start;
        let overlaps = avoid.x < right && avoid.x.saturating_add(avoid.width) > left;
        if shares_band && overlaps {
            let beside = avoid.x.saturating_sub(margin);
            if avoid.x > left && beside.saturating_sub(left) >= minimum {
                right = beside;
            } else {
                // Too narrow beside it, or it spans the band: below it.
                y = avoid.y.saturating_add(avoid.height).saturating_add(margin);
            }
        }
    }
    let bottom = terminal
        .y
        .saturating_add(terminal.height)
        .saturating_sub(margin);
    let available = right.saturating_sub(left);
    if available < minimum || y.saturating_add(height) > bottom {
        return None;
    }
    let width = content_width
        .saturating_add(scaled(STRIP_INSET, scale_120).saturating_mul(2))
        .min(available);
    Some(Rect {
        x: left.saturating_add(available.saturating_sub(width) / 2),
        y,
        width,
        height,
    })
}

/// The banner's marks: the spinner's frame while the upgrade runs, and
/// the lost connection's cross, `✕` where the interface font draws it and
/// `x` where it does not ([`BannerMarks::LOST`]).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BannerMarks {
    pub(crate) spinner: char,
    pub(crate) lost: char,
}

impl BannerMarks {
    /// The lost connection's cross, when the interface font draws it.
    pub(crate) const LOST: char = '✕';
    /// Its ASCII stand-in otherwise.
    pub(crate) const LOST_ASCII: char = 'x';
}

/// How a banner looks: its frame, its mark and the mark's colour.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BannerLook {
    frame: u32,
    mark: char,
    mark_color: u32,
}

/// Running, the banner is in the warning colour with the spinner as its
/// mark; lost, in danger with the cross; settled, on the divider with `!`
/// in warning, since a rolled-back upgrade failed (the old daemon came
/// back) and a cancelled one did not happen.
const fn banner_look(
    banner: HandoffBanner,
    palette: &SessionPickerPalette,
    marks: BannerMarks,
) -> BannerLook {
    match banner {
        HandoffBanner::Preparing | HandoffBanner::InputPaused | HandoffBanner::Reconnecting => {
            BannerLook {
                frame: palette.warning,
                mark: marks.spinner,
                mark_color: palette.warning,
            }
        }
        HandoffBanner::Lost => BannerLook {
            frame: palette.danger,
            mark: marks.lost,
            mark_color: palette.danger,
        },
        HandoffBanner::Notice(HandoffNotice::RolledBack | HandoffNotice::Cancelled(_)) => {
            BannerLook {
                frame: palette.frame,
                mark: '!',
                mark_color: palette.warning,
            }
        }
    }
}

/// Paints the upgrade banner and returns its panel, or `None` when the
/// terminal is too small for it: no title, its mark (from `marks`) and
/// label centred in the frame its state colours.
#[allow(
    clippy::too_many_arguments,
    reason = "trusted strip painting keeps geometry, palette, and cache explicit"
)]
pub(crate) fn paint_handoff_banner(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    canvas: &mut [u8],
    canvas_width: u32,
    canvas_height: u32,
    terminal: Rect,
    avoid: Option<Rect>,
    scale_120: u32,
    renderer_generation: u64,
    palette: SessionPickerPalette,
    banner: HandoffBanner,
    marks: BannerMarks,
) -> Result<Option<Rect>> {
    if scale_120 == 0 {
        return Ok(None);
    }
    let mut painter = ChromePainter::new(
        cache,
        context,
        canvas,
        canvas_width,
        canvas_height,
        scale_120,
        renderer_generation,
    );
    let (cell_width, _) = painter.cell(ChromeTextStyle::Regular)?;
    let label = banner.label();
    // The mark, a space, the label.
    let content_cells = text_cells(&label).saturating_add(2);
    let Some(panel) = handoff_banner_layout(
        terminal,
        avoid,
        cells_px(content_cells, cell_width),
        scale_120,
    ) else {
        painter.finish();
        return Ok(None);
    };
    let look = banner_look(banner, &palette, marks);
    let cell = painter.box_cell()?;
    let chrome = ChromeBox {
        title: "",
        right_label: &[],
        right_active: None,
        legend: &[],
    }
    .layout(panel, scale_120, cell);
    chrome.paint_surface(&mut painter, &palette);
    let row = content_row(panel, scale_120);
    let row_right = row.x.saturating_add(row.width);
    let start = row.x.saturating_add(
        row.width
            .saturating_sub(cells_px(content_cells, cell_width))
            / 2,
    );
    painter.text(
        look.mark.encode_utf8(&mut [0; 4]),
        ChromeTextStyle::Regular,
        Rect {
            x: start,
            width: cell_width.min(row_right.saturating_sub(start)),
            ..row
        },
        PickerTextAlignment::Left,
        look.mark_color,
    )?;
    let label_x = start.saturating_add(cells_px(2, cell_width));
    painter.text(
        &label,
        ChromeTextStyle::Regular,
        Rect {
            x: label_x,
            width: row_right.saturating_sub(label_x),
            ..row
        },
        PickerTextAlignment::Left,
        palette.primary,
    )?;
    chrome.paint_frame(&mut painter, &palette, look.frame)?;
    painter.finish();
    Ok(Some(panel))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::ResolvedTheme, renderer::session_picker_palette};
    use splinterm_protocol::HandoffCancelReason;

    const WIDTH: u32 = 900;
    const HEIGHT: u32 = 480;
    const TERMINAL: Rect = Rect {
        x: 260,
        y: 40,
        width: 600,
        height: 360,
    };

    fn pixel(canvas: &[u8], x: u32, y: u32) -> [u8; 3] {
        let index = usize::try_from((y * WIDTH + x) * 4).unwrap();
        [canvas[index + 2], canvas[index + 1], canvas[index]]
    }

    fn rgb(color: u32) -> [u8; 3] {
        let [_, red, green, blue] = color.to_be_bytes();
        [red, green, blue]
    }

    /// Every painted pixel is inside `bounds`.
    fn painted_inside(canvas: &[u8], bounds: Rect) -> bool {
        let mut painted = false;
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let index = usize::try_from((y * WIDTH + x) * 4).unwrap();
                let blank = canvas[index..index + 4] == [0; 4];
                let inside = x >= bounds.x
                    && x < bounds.x + bounds.width
                    && y >= bounds.y
                    && y < bounds.y + bounds.height;
                if !blank && !inside {
                    return false;
                }
                painted |= !blank;
            }
        }
        painted
    }

    fn search(
        cache: &mut SessionPickerTextCache,
        strip: SearchStrip<'_>,
        focused: bool,
    ) -> (bool, Vec<u8>) {
        let mut canvas = vec![0_u8; usize::try_from(WIDTH * HEIGHT * 4).unwrap()];
        let drawn = paint_search_overlay(
            cache,
            &RenderContext::new(u16::MAX),
            &mut canvas,
            WIDTH,
            HEIGHT,
            TERMINAL,
            120,
            1,
            session_picker_palette(ResolvedTheme::default()),
            strip,
            focused,
        )
        .unwrap();
        (drawn, canvas)
    }

    const SEARCHED: SearchStrip<'static> = SearchStrip {
        input: "alpha",
        searched_query: "alpha",
        matches: 3,
        selected: 1,
        searching: false,
        pending: false,
    };

    /// The search strip sits at the bottom left of the terminal area in
    /// the box: the focused frame while the Window has focus, `search` and
    /// the match count (`2/3`, as the palette counts) in it, the query after the accent `›`, and its keys in the bottom edge.
    /// Nothing is drawn outside the strip and its shadow, which the Window
    /// damages.
    #[test]
    fn search_is_a_box_with_the_count_in_its_focused_frame() {
        let palette = session_picker_palette(ResolvedTheme::default());
        let mut cache = SessionPickerTextCache::default();
        let (drawn, canvas) = search(&mut cache, SEARCHED, true);
        assert!(drawn);
        let panel = search_overlay_panel(TERMINAL, 120).unwrap();
        assert_eq!(panel.x, TERMINAL.x + 8);
        assert_eq!(panel.y + panel.height, TERMINAL.y + TERMINAL.height - 8);
        assert_eq!((panel.width, panel.height), (440, STRIP_HEIGHT));
        assert!(painted_inside(&canvas, strip_bounds(panel, 120)));
        assert!(!painted_inside(&canvas, panel), "the shadow");
        for text in [
            "search", "2/3", "›", "alpha", "ctrl n", "next", "ctrl p", "esc",
        ] {
            assert!(cache.contains_source(text), "{text} was not drawn");
        }
        let (left, top) = (panel.x + 9, panel.y + 9);
        let (right, bottom) = (panel.x + panel.width - 10, panel.y + panel.height - 10);
        for (x, y) in [
            (left, panel.y + 34),
            (right, panel.y + 34),
            (panel.x + 220, top),
            (right - 4, bottom),
        ] {
            assert_eq!(pixel(&canvas, x, y), rgb(palette.focused_frame), "{x},{y}");
        }
        let (_, unfocused) = search(&mut cache, SEARCHED, false);
        assert_eq!(pixel(&unfocused, left, panel.y + 34), rgb(palette.frame));

        // Before a search: no count, and Enter searches.
        let mut fresh = SessionPickerTextCache::default();
        search(
            &mut fresh,
            SearchStrip {
                input: "bravo",
                ..SEARCHED
            },
            true,
        );
        assert!(fresh.contains_source("↵") && !fresh.contains_source("2/3"));
        // Enter on a new query: until it is answered, the earlier query's
        // count and its next/prev keys do not show under it.
        let mut fresh = SessionPickerTextCache::default();
        search(
            &mut fresh,
            SearchStrip {
                pending: true,
                ..SEARCHED
            },
            true,
        );
        assert!(fresh.contains_source("searching…"));
        assert!(!fresh.contains_source("2/3") && !fresh.contains_source("ctrl n"));
        // An empty field says what it searches.
        let mut fresh = SessionPickerTextCache::default();
        search(
            &mut fresh,
            SearchStrip {
                input: "",
                ..SEARCHED
            },
            true,
        );
        assert!(fresh.contains_source("search scrollback"));
        for (matches, searching, count) in [(0, true, "searching…"), (0, false, "no matches")] {
            let mut fresh = SessionPickerTextCache::default();
            search(
                &mut fresh,
                SearchStrip {
                    matches,
                    searching,
                    ..SEARCHED
                },
                true,
            );
            assert!(fresh.contains_source(count), "{count}");
        }
    }

    /// A terminal area too small for the strip gets none, and nothing is
    /// drawn; a long query keeps its end.
    #[test]
    fn search_skips_unusable_geometry_and_keeps_a_long_querys_end() {
        let tiny = Rect {
            width: 150,
            ..TERMINAL
        };
        assert_eq!(search_overlay_panel(tiny, 120), None);
        assert_eq!(search_overlay_panel(TERMINAL, 0), None);
        let short = Rect {
            height: STRIP_HEIGHT + 15,
            ..TERMINAL
        };
        assert_eq!(search_overlay_panel(short, 120), None);
        let mut cache = SessionPickerTextCache::default();
        let long = format!("{}tail\n", "x".repeat(2_000));
        let (drawn, _) = search(
            &mut cache,
            SearchStrip {
                input: &long,
                ..SEARCHED
            },
            true,
        );
        assert!(drawn);
        assert!(cache.contains_source("esc"));
    }

    fn history(
        cache: &mut SessionPickerTextCache,
        status: HistoryOverlayStatus,
        keys: &[String],
        scale_120: u32,
    ) -> (Option<HistoryOverlayLayout>, Vec<u8>) {
        let mut canvas = vec![0_u8; usize::try_from(WIDTH * HEIGHT * 4).unwrap()];
        let layout = paint_history_overlay(
            cache,
            &RenderContext::new(u16::MAX),
            &mut canvas,
            WIDTH,
            HEIGHT,
            TERMINAL,
            scale_120,
            1,
            session_picker_palette(ResolvedTheme::default()),
            status,
            keys,
        )
        .unwrap();
        (layout, canvas)
    }

    /// The history strip is passive text at the top right: `history` over
    /// the divider frame, the accent `↑`, the position (`17/351`, as the
    /// palette counts), the rows that arrived since, and the return chips at
    /// its right end, inside its return area. It is sized to its content:
    /// the longest position, a cell, and the return area. Counts clamp at
    /// 999.
    #[test]
    fn history_is_passive_text_sized_to_its_content() {
        let palette = session_picker_palette(ResolvedTheme::default());
        // 2·20 inset + 19 cells × 8 + 8 + (60 return + 20 inset) = 280.
        let layout = history_overlay_layout(960, 600, 120, 8, 60).unwrap();
        assert_eq!(layout.panel, (960 - 8 - 280, 8, 280, STRIP_HEIGHT));
        assert_eq!(layout.return_to_live, (960 - 8 - 80, 8, 80, STRIP_HEIGHT));
        // Never narrower than 240, and capped by the area.
        let narrow = history_overlay_layout(960, 600, 120, 2, 10).unwrap();
        assert_eq!(narrow.panel.2, 240);
        let capped = history_overlay_layout(260, 600, 120, 8, 60).unwrap();
        assert_eq!(capped.panel, (8, 8, 244, STRIP_HEIGHT));
        assert!(history_overlay_layout(150, 600, 120, 8, 60).is_none());
        assert!(history_overlay_layout(960, 70, 120, 8, 60).is_none());
        assert!(history_overlay_layout(960, 600, 0, 8, 60).is_none());

        let mut cache = SessionPickerTextCache::default();
        let keys = vec!["shift".to_owned(), "end".to_owned()];
        let status = HistoryOverlayStatus {
            offset_from_bottom: 17,
            available_rows: 351,
            unseen_rows: 4,
        };
        let (painted, canvas) = history(&mut cache, status, &keys, 120);
        let painted = painted.unwrap();
        for text in ["history", "↑", "17/351", "+4 new", "shift", "end"] {
            assert!(cache.contains_source(text), "{text} was not drawn");
        }
        assert!(!cache.contains_source("…"), "the chips are whole");
        let panel = tuple_rect(painted.panel);
        assert_eq!(panel.x + panel.width, TERMINAL.x + TERMINAL.width - 8);
        assert_eq!(panel.y, TERMINAL.y + 8);
        assert!(painted_inside(&canvas, strip_bounds(panel, 120)));
        assert_eq!(
            pixel(&canvas, panel.x + 9, panel.y + 34),
            rgb(palette.frame),
            "a passive frame"
        );
        // The painted chips lie inside the return area.
        let action = tuple_rect(painted.return_to_live);
        assert_eq!(action.x + action.width, panel.x + panel.width);
        let row = content_row(panel, 120);
        let room = Rect {
            x: action.x,
            width: row.x + row.width - action.x,
            ..row
        };
        let mut scratch = vec![0_u8; usize::try_from(WIDTH * HEIGHT * 4).unwrap()];
        let cell = ChromePainter::new(
            &mut cache,
            &RenderContext::new(u16::MAX),
            &mut scratch,
            WIDTH,
            HEIGHT,
            120,
            1,
        )
        .cell(ChromeTextStyle::Regular)
        .unwrap();
        let chips = super::super::chrome_rows::chip_layout(&keys, room, cell, 120, true);
        assert_eq!((chips.first, chips.ellipsis), (0, None));
        for chip in &chips.chips {
            assert!(chip.x >= action.x && chip.x + chip.width <= action.x + action.width);
            let (x, y) = (chip.x + chip.width / 2, chip.y + 1);
            assert_ne!(
                pixel(&canvas, x, y),
                rgb(palette.panel),
                "a chip at {x},{y}"
            );
        }

        let mut fresh = SessionPickerTextCache::default();
        history(
            &mut fresh,
            HistoryOverlayStatus {
                offset_from_bottom: 4_096,
                available_rows: 4_096,
                unseen_rows: 1_000,
            },
            &[],
            120,
        );
        for text in ["999/999", "+999 new", "↓ live"] {
            assert!(fresh.contains_source(text), "{text} was not drawn");
        }
    }

    /// The position is whole or not there, its total dropped first; at a
    /// large chrome font the strip grows so its chips stay whole.
    #[test]
    fn history_text_is_whole_and_its_chips_never_cut() {
        let status = HistoryOverlayStatus {
            offset_from_bottom: 999,
            available_rows: 999,
            unseen_rows: 0,
        };
        assert_eq!(history_position(status, 7).as_deref(), Some("999/999"));
        assert_eq!(history_position(status, 6).as_deref(), Some("999"));
        assert_eq!(history_position(status, 2), None);
        let keys = vec!["ctrl".to_owned(), "shift".to_owned(), "end".to_owned()];
        for cell in [(7, 14), (12, 22), (16, 24)] {
            let chips = chips_width(&keys, cell, 120);
            let layout = history_overlay_layout(1_600, 600, 120, cell.0, chips).unwrap();
            let panel = tuple_rect(layout.panel);
            let row = content_row(panel, 120);
            let action = tuple_rect(layout.return_to_live);
            let room = Rect {
                x: action.x,
                width: row.x + row.width - action.x,
                ..row
            };
            let placed = super::super::chrome_rows::chip_layout(&keys, room, cell, 120, true);
            assert_eq!((placed.first, placed.ellipsis), (0, None), "{cell:?}");
            // The longest position fits left of the chips with a cell's gap.
            let text = cells_px(text_cells(HISTORY_WORST_TEXT), cell.0);
            assert!(row.x + text + cell.0 <= action.x, "{cell:?}");
        }
    }

    /// At a fractional scale the strip and its return area stay inside
    /// the area, right-anchored, as tall as the scaled strip.
    #[test]
    fn history_layout_holds_at_fractional_scales() {
        for scale in [150, 180] {
            let cell = (8 * scale / 120, 16 * scale / 120);
            let keys = vec!["shift".to_owned(), "end".to_owned()];
            let chips = chips_width(&keys, cell, scale);
            let layout = history_overlay_layout(1_440, 900, scale, cell.0, chips).unwrap();
            let panel = tuple_rect(layout.panel);
            let margin = scaled(STRIP_MARGIN, scale);
            assert_eq!(panel.x + panel.width, 1_440 - margin, "{scale}");
            assert_eq!(panel.height, scaled(STRIP_HEIGHT, scale), "{scale}");
            assert!(panel.width >= scaled(HISTORY_WIDTH, scale), "{scale}");
            let action = tuple_rect(layout.return_to_live);
            assert_eq!(action.x + action.width, panel.x + panel.width, "{scale}");
            assert_eq!(action.width, chips + scaled(STRIP_INSET, scale), "{scale}");
        }
        let mut cache = SessionPickerTextCache::default();
        let (painted, canvas) = history(
            &mut cache,
            HistoryOverlayStatus {
                offset_from_bottom: 3,
                available_rows: 40,
                unseen_rows: 0,
            },
            &["end".to_owned()],
            180,
        );
        let panel = tuple_rect(painted.unwrap().panel);
        assert!(painted_inside(&canvas, strip_bounds(panel, 180)));
        assert!(cache.contains_source("3/40"));
    }

    fn banner(
        cache: &mut SessionPickerTextCache,
        banner: HandoffBanner,
        avoid: Option<Rect>,
    ) -> (Option<Rect>, Vec<u8>) {
        let mut canvas = vec![0_u8; usize::try_from(WIDTH * HEIGHT * 4).unwrap()];
        let panel = paint_handoff_banner(
            cache,
            &RenderContext::new(u16::MAX),
            &mut canvas,
            WIDTH,
            HEIGHT,
            TERMINAL,
            avoid,
            120,
            1,
            session_picker_palette(ResolvedTheme::default()),
            banner,
            BannerMarks {
                spinner: '⠙',
                lost: BannerMarks::LOST,
            },
        )
        .unwrap();
        (panel, canvas)
    }

    /// The banner sits centred at the top of the terminal area, clear of
    /// the history strip when it shares its band; a terminal too small for
    /// it gets none.
    #[test]
    fn the_banner_is_centred_at_the_top_and_clear_of_history() {
        let panel = handoff_banner_layout(TERMINAL, None, 240, 120).unwrap();
        assert_eq!((panel.width, panel.height), (240 + 40, STRIP_HEIGHT));
        assert_eq!(panel.y, TERMINAL.y + 8);
        assert_eq!(panel.x + panel.width / 2, TERMINAL.x + TERMINAL.width / 2);
        let wide = handoff_banner_layout(TERMINAL, None, 5_000, 120).unwrap();
        assert_eq!(wide.width, TERMINAL.width - 16);
        let history = Rect {
            x: TERMINAL.x + TERMINAL.width - 308,
            y: TERMINAL.y + 8,
            width: 300,
            height: STRIP_HEIGHT,
        };
        let clear = handoff_banner_layout(TERMINAL, Some(history), 5_000, 120).unwrap();
        assert!(clear.x + clear.width + 8 <= history.x);
        let below = Rect {
            y: TERMINAL.y + 200,
            ..history
        };
        assert_eq!(
            handoff_banner_layout(TERMINAL, Some(below), 5_000, 120),
            Some(wide)
        );
        let tiny = Rect {
            height: STRIP_HEIGHT,
            ..TERMINAL
        };
        assert_eq!(handoff_banner_layout(tiny, None, 100, 120), None);
        assert_eq!(handoff_banner_layout(TERMINAL, None, 100, 0), None);
    }

    /// In a narrow terminal, where the band left of history leaves no
    /// legible banner, or where history spans the band, the banner goes in
    /// the band below history instead of vanishing or covering it; with no
    /// room below either, there is none.
    #[test]
    fn a_narrow_banner_goes_below_history() {
        let narrow = Rect {
            x: 0,
            y: 0,
            width: 420,
            height: 300,
        };
        let history = Rect {
            x: narrow.width - 8 - 280,
            y: 8,
            width: 280,
            height: STRIP_HEIGHT,
        };
        let below = handoff_banner_layout(narrow, Some(history), 200, 120).unwrap();
        assert_eq!(below.y, history.y + history.height + 8);
        assert_eq!(below.x + below.width / 2, narrow.width / 2);
        let spanning = Rect {
            x: 8,
            width: narrow.width - 16,
            ..history
        };
        let under = handoff_banner_layout(narrow, Some(spanning), 200, 120).unwrap();
        assert_eq!(under.y, spanning.y + spanning.height + 8);
        assert!(under.y >= spanning.y + spanning.height);
        let short = Rect {
            height: STRIP_HEIGHT * 2 + 16,
            ..narrow
        };
        assert_eq!(handoff_banner_layout(short, Some(history), 200, 120), None);
        // At a fractional scale too.
        let scale = 150;
        let wide_history = Rect {
            x: 8 * scale / 120,
            y: 10,
            width: 600,
            height: scaled(STRIP_HEIGHT, scale),
        };
        let area = Rect {
            x: 0,
            y: 0,
            width: 640,
            height: 400,
        };
        let placed = handoff_banner_layout(area, Some(wide_history), 300, scale).unwrap();
        assert_eq!(
            placed.y,
            wide_history.y + wide_history.height + scaled(STRIP_MARGIN, scale)
        );
        assert_eq!(placed.height, scaled(STRIP_HEIGHT, scale));
        let beside = handoff_banner_layout(
            Rect {
                width: 1_600,
                ..area
            },
            Some(Rect {
                x: 1_600 - 12 - 420,
                ..wide_history
            }),
            300,
            scale,
        )
        .unwrap();
        assert_eq!(beside.y, scaled(STRIP_MARGIN, scale));
        assert!(beside.x + beside.width + scaled(STRIP_MARGIN, scale) <= 1_600 - 12 - 420);
    }

    /// A running upgrade's banner is framed in the warning colour with the
    /// spinner in place of its `…`; a lost one in danger with `✕`; a
    /// rolled-back or cancelled one on the divider with `!`, since neither
    /// upgraded.
    #[test]
    fn the_banner_frame_and_mark_follow_the_upgrade() {
        let palette = session_picker_palette(ResolvedTheme::default());
        assert_ne!(palette.warning, palette.frame);
        for (state, frame, mark, label) in [
            (
                HandoffBanner::InputPaused,
                palette.warning,
                "⠙",
                "Upgrading — input paused",
            ),
            (
                HandoffBanner::Preparing,
                palette.warning,
                "⠙",
                "Preparing upgrade",
            ),
            (
                HandoffBanner::Lost,
                palette.danger,
                "✕",
                "Lost connection to splinterd — reopen from the launcher",
            ),
            (
                HandoffBanner::Notice(HandoffNotice::RolledBack),
                palette.frame,
                "!",
                "Rolled back",
            ),
            (
                HandoffBanner::Notice(HandoffNotice::Cancelled(HandoffCancelReason::Blocked)),
                palette.frame,
                "!",
                "Upgrade cancelled — something blocked it",
            ),
        ] {
            let mut cache = SessionPickerTextCache::default();
            let (panel, canvas) = banner(&mut cache, state, None);
            let panel = panel.unwrap();
            assert!(painted_inside(&canvas, strip_bounds(panel, 120)));
            assert_eq!(
                pixel(&canvas, panel.x + 9, panel.y + 34),
                rgb(frame),
                "{label}"
            );
            assert!(cache.contains_source(mark), "{label}");
            assert!(cache.contains_source(label), "{label}");
            assert!(!cache.contains_source("Preparing upgrade…"));
        }
        let ascii = BannerMarks {
            spinner: '⠙',
            lost: BannerMarks::LOST_ASCII,
        };
        assert_eq!(banner_look(HandoffBanner::Lost, &palette, ascii).mark, 'x');
        let rolled_back = banner_look(
            HandoffBanner::Notice(HandoffNotice::RolledBack),
            &palette,
            ascii,
        );
        assert_eq!(
            (rolled_back.frame, rolled_back.mark, rolled_back.mark_color),
            (palette.frame, '!', palette.warning)
        );
    }

    /// Every strip row is tall enough for the largest UI text.
    #[test]
    fn strip_rows_fit_the_largest_ui_text() {
        use super::super::super::text::CHROME_MAXIMUM_LINE_HEIGHT;

        const { assert!(ROW_HEIGHT >= CHROME_MAXIMUM_LINE_HEIGHT) };
        assert_eq!(STRIP_HEIGHT, 68);
    }
}
