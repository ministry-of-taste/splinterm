//! Trusted selection and URL overlay presentation over the terminal.

use crate::geometry::WindowGeometry;

use super::super::{
    SnapshotFrame, compose::paint_placed_glyph_clipped, decorations::paint_decoration_span,
    raster::fill_rect,
};

fn themed_rgba(color: u32, alpha: u8) -> [u8; 4] {
    let [_, red, green, blue] = color.to_be_bytes();
    [red, green, blue, alpha]
}

#[derive(Clone, Copy)]
pub(crate) struct SnapshotOverlays<'a> {
    pub(crate) selection: Option<((usize, usize), (usize, usize))>,
    pub(crate) hovered_url: Option<((usize, usize), (usize, usize))>,
    pub(crate) dirty_rows: Option<&'a [bool]>,
    pub(crate) focused: bool,
    /// Packed `0xRRGGBB` project theme roles.
    pub(crate) selection_color: u32,
    pub(crate) selection_foreground: u32,
    pub(crate) url_color: u32,
    pub(crate) accent_color: u32,
    /// The selection is the search's current match: it is outlined in the
    /// accent, on its selection fill, so it stands out from a selection
    /// made by hand.
    pub(crate) search_match: bool,
}

#[allow(
    clippy::too_many_lines,
    reason = "selection background and foreground remain one ordered theme-composition boundary"
)]
pub(crate) fn paint_snapshot_overlays(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    frame: &SnapshotFrame,
    geometry: &WindowGeometry,
    overlays: SnapshotOverlays<'_>,
) {
    let SnapshotOverlays {
        selection,
        hovered_url,
        dirty_rows,
        focused: _focused,
        selection_color,
        selection_foreground,
        url_color,
        accent_color,
        search_match,
    } = overlays;
    // Focus framing belongs to the compositor. Painting a second solid frame
    // inside the client obscures Hyprland's active-border gradient.
    let row_is_dirty =
        |row: usize| dirty_rows.is_none_or(|dirty| dirty.get(row).copied().unwrap_or(false));
    if let Some((start, end)) = selection {
        for row in start.0..=end.0.min(frame.rows.saturating_sub(1) as usize) {
            if !row_is_dirty(row) {
                continue;
            }
            let first = if row == start.0 { start.1 } else { 0 };
            let last = if row == end.0 {
                end.1
            } else {
                frame.columns.saturating_sub(1) as usize
            };
            let last = last.min(frame.columns.saturating_sub(1) as usize);
            let (Ok(first), Ok(last), Ok(row)) = (
                u32::try_from(first),
                u32::try_from(last),
                u32::try_from(row),
            ) else {
                continue;
            };
            let (Some((left, top, cell_width, cell_height)), Some((right, _, _, _))) = (
                frame.cell_rect(geometry, first, row),
                frame.cell_rect(geometry, last, row),
            ) else {
                continue;
            };
            let selection_width = u32::try_from(right.saturating_sub(left))
                .unwrap_or(0)
                .saturating_add(cell_width);
            fill_rect(
                canvas,
                width,
                height,
                (left, top, selection_width, cell_height),
                themed_rgba(selection_color, u8::MAX),
            );
            let outline = (search_match && start.0 == end.0).then_some((
                left,
                top,
                selection_width,
                cell_height,
            ));
            let clip = (
                left,
                top,
                left.saturating_add(i32::try_from(selection_width).unwrap_or(i32::MAX)),
                top.saturating_add(i32::try_from(cell_height).unwrap_or(i32::MAX)),
            );
            let selected_foreground = {
                let [_, red, green, blue] = selection_foreground.to_be_bytes();
                [red, green, blue]
            };
            // Contextual ink may originate outside the selected source span.
            // Opted-in rows use bounded row candidates and the pixel clip below.
            for glyph in frame
                .glyphs
                .iter()
                .filter(|glyph| {
                    glyph.row == row
                        && (frame.font_ligatures != crate::font_shaping::FontLigatures::Off
                            || (glyph.column < last.saturating_add(1)
                                && glyph.column.saturating_add(glyph.cells.max(1)) > first))
                })
                .rev()
            {
                paint_placed_glyph_clipped(
                    canvas,
                    width,
                    height,
                    frame,
                    geometry,
                    glyph,
                    selected_foreground,
                    Some(clip),
                );
            }
            for decoration in frame.decorations.iter().filter(|decoration| {
                decoration.row == row
                    && decoration.column < last.saturating_add(1)
                    && decoration.column.saturating_add(decoration.cells) > first
            }) {
                let mut selected = *decoration;
                let decoration_end = selected.column.saturating_add(selected.cells);
                selected.column = selected.column.max(first);
                selected.cells = decoration_end
                    .min(last.saturating_add(1))
                    .saturating_sub(selected.column);
                paint_decoration_span(
                    canvas,
                    width,
                    height,
                    frame,
                    geometry,
                    &selected,
                    Some(selected_foreground),
                );
            }
            if let Some(rect) = outline {
                paint_match_outline(canvas, width, height, rect, accent_color);
            }
        }
    }
    if let Some((start, end)) = hovered_url
        && start.0 == end.0
        && row_is_dirty(start.0)
    {
        for column in start.1..=end.1.min(frame.columns.saturating_sub(1) as usize) {
            let (Ok(column), Ok(row)) = (u32::try_from(column), u32::try_from(start.0)) else {
                continue;
            };
            let Some((x, y, cell_width, cell_height)) = frame.cell_rect(geometry, column, row)
            else {
                continue;
            };
            fill_rect(
                canvas,
                width,
                height,
                (
                    x,
                    y + i32::try_from(cell_height.saturating_sub(2)).unwrap_or(0),
                    cell_width,
                    2,
                ),
                themed_rgba(url_color, u8::MAX),
            );
        }
    }
}

/// A 1 px accent outline just inside `rect`, the current search match's
/// cells, thicker with taller cells so it reads at every scale.
fn paint_match_outline(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    (x, y, rect_width, rect_height): (i32, i32, u32, u32),
    accent_color: u32,
) {
    let line = (rect_height / 18)
        .max(1)
        .min(rect_width / 2)
        .min(rect_height / 2);
    let color = themed_rgba(accent_color, u8::MAX);
    let offset = |by: u32| i32::try_from(by).unwrap_or(i32::MAX);
    for stripe in [
        (x, y, rect_width, line),
        (
            x,
            y.saturating_add(offset(rect_height.saturating_sub(line))),
            rect_width,
            line,
        ),
        (x, y, line, rect_height),
        (
            x.saturating_add(offset(rect_width.saturating_sub(line))),
            y,
            line,
            rect_height,
        ),
    ] {
        fill_rect(canvas, width, height, stripe, color);
    }
}
