//! The shared popup box and chrome text painter (Plan 0065).
//!
//! Every popup is one TUI-style box: the raised panel fill, a 1 px frame
//! inset inside it, a lowercase title set into the top edge with an optional
//! right label (a count or a scope), a legend of `key verb` pairs set into
//! the bottom edge, and the hard offset shadow. Modal lists and prompts dim
//! the Window behind them with one scrim level.
//!
//! [`ChromePainter`] is the one way popups draw chrome text: it paints
//! through the picker's text cache (left, centre or right, with tints) and
//! records what a frame used, so the cache keeps only recent text.

use std::collections::HashSet;

use anyhow::Result;
use unicode_width::UnicodeWidthChar;

use crate::geometry::Rect;

use super::{
    super::{ChromeTextStyle, RenderContext, blend_rect, fill_rect},
    picker::{
        PickerTextAlignment, SessionPickerPalette, SessionPickerTextCache, SessionPickerTextKey,
        paint_picker_text, picker_buffer_rect, truncate_picker_text,
    },
};

/// Logical px from the panel fill's edge to the frame line.
pub(super) const BOX_FRAME_INSET: u32 = 9;
/// Logical px from the panel fill's edge to just inside the 1 px frame.
pub(super) const BOX_EDGE: u32 = BOX_FRAME_INSET + 1;
/// Logical px between the inside of the frame and the content, at the top
/// and the bottom: the title and the legend, centred on the frame and at
/// most the chrome line cap (24 logical px) tall, stay clear of it.
pub(super) const BOX_CLEARANCE: u32 = 14;
/// Logical px from each end of the frame to the title, the right label and
/// the legend, so the frame's corners stay drawn.
pub(super) const BOX_LABEL_INSET: u32 = 10;
/// Logical px the hard shadow is offset down and right.
pub(super) const BOX_SHADOW_OFFSET: u32 = 6;
/// Cells between two pairs of the legend.
const LEGEND_PAIR_GAP_CELLS: usize = 2;

fn cells(text: &str) -> usize {
    text.chars()
        .map(|character| character.width().unwrap_or(0).min(2))
        .sum()
}

fn cells_u32(text: &str) -> u32 {
    u32::try_from(cells(text)).unwrap_or(u32::MAX)
}

pub(super) fn rect_tuple(rect: Rect) -> (i32, i32, u32, u32) {
    (
        i32::try_from(rect.x).unwrap_or(i32::MAX),
        i32::try_from(rect.y).unwrap_or(i32::MAX),
        rect.width,
        rect.height,
    )
}

pub(super) fn opaque(color: u32) -> [u8; 4] {
    let [_, red, green, blue] = color.to_be_bytes();
    [red, green, blue, u8::MAX]
}

/// One frame of chrome painting on a canvas: the text cache, the canvas and
/// its scale. Text goes through the picker's painter; [`Self::finish`]
/// tells the cache which entries the frame used.
pub(super) struct ChromePainter<'a> {
    cache: &'a mut SessionPickerTextCache,
    context: &'a RenderContext,
    used: HashSet<SessionPickerTextKey>,
    canvas: &'a mut [u8],
    width: u32,
    height: u32,
    pub(super) scale_120: u32,
    renderer_generation: u64,
}

impl<'a> ChromePainter<'a> {
    pub(super) fn new(
        cache: &'a mut SessionPickerTextCache,
        context: &'a RenderContext,
        canvas: &'a mut [u8],
        width: u32,
        height: u32,
        scale_120: u32,
        renderer_generation: u64,
    ) -> Self {
        Self {
            cache,
            context,
            used: HashSet::new(),
            canvas,
            width,
            height,
            scale_120,
            renderer_generation,
        }
    }

    /// `logical` px in buffer px, rounded up.
    pub(super) const fn scaled(&self, logical: u32) -> u32 {
        logical.saturating_mul(self.scale_120).div_ceil(120)
    }

    /// A logical rect in buffer px, edge by edge, so neighbours still meet.
    pub(super) fn buffer_rect(&self, logical: Rect) -> Rect {
        picker_buffer_rect(logical, self.scale_120)
    }

    /// A 1 px logical line in buffer px, never thinner than one pixel.
    pub(super) fn border(&self) -> u32 {
        border_width(self.scale_120)
    }

    pub(super) fn fill(&mut self, rect: Rect, color: [u8; 4]) {
        fill_rect(
            self.canvas,
            self.width,
            self.height,
            rect_tuple(rect),
            color,
        );
    }

    pub(super) fn blend(&mut self, rect: Rect, color: [u8; 4]) {
        blend_rect(
            self.canvas,
            self.width,
            self.height,
            rect_tuple(rect),
            color,
        );
    }

    /// One cell's width and height in `style`, in buffer px.
    pub(super) fn cell(&mut self, style: ChromeTextStyle) -> Result<(u32, u32)> {
        self.cache.cell_metrics(
            self.context,
            style,
            self.scale_120,
            self.renderer_generation,
        )
    }

    /// The larger cell of the regular and bold styles: room for either.
    pub(super) fn box_cell(&mut self) -> Result<(u32, u32)> {
        let regular = self.cell(ChromeTextStyle::Regular)?;
        let bold = self.cell(ChromeTextStyle::Bold)?;
        Ok((regular.0.max(bold.0), regular.1.max(bold.1)))
    }

    /// Paints `source` in `clip`, vertically centred, ending in `…` when it
    /// does not fit.
    pub(super) fn text(
        &mut self,
        source: &str,
        style: ChromeTextStyle,
        clip: Rect,
        alignment: PickerTextAlignment,
        color: u32,
    ) -> Result<()> {
        paint_picker_text(
            self.cache,
            self.context,
            &mut self.used,
            self.canvas,
            self.width,
            self.height,
            source,
            style,
            self.scale_120,
            self.renderer_generation,
            clip,
            alignment,
            color,
        )
    }

    /// Ends the frame: the cache keeps what this and the last frames drew.
    pub(super) fn finish(self) {
        self.cache.finish_frame(self.used);
    }
}

/// A 1 px logical line in buffer px at `scale_120`, at least one pixel.
pub(super) fn border_width(scale_120: u32) -> u32 {
    scale_120.div_ceil(120).max(1)
}

/// The modal scrim over `rect` (buffer px): the surface toward black at
/// `MODAL_SCRIM_ALPHA`, one level shared by every list and prompt.
pub(super) fn paint_scrim(
    painter: &mut ChromePainter<'_>,
    rect: Rect,
    palette: &SessionPickerPalette,
) {
    painter.blend(rect, palette.scrim);
}

/// The 1 px (scaled) outline of `rect`.
pub(super) fn paint_outline(painter: &mut ChromePainter<'_>, rect: Rect, border: u32, color: u32) {
    let border = border.min(rect.width).min(rect.height);
    let right = rect.x.saturating_add(rect.width).saturating_sub(border);
    let bottom = rect.y.saturating_add(rect.height).saturating_sub(border);
    for edge in [
        Rect {
            height: border,
            ..rect
        },
        Rect {
            y: bottom,
            height: border,
            ..rect
        },
        Rect {
            width: border,
            ..rect
        },
        Rect {
            x: right,
            width: border,
            ..rect
        },
    ] {
        painter.fill(edge, opaque(color));
    }
}

/// The hard shadow of `panel`: the same rect offset down and right by
/// [`BOX_SHADOW_OFFSET`], 35% black.
pub(super) fn shadow_rect(panel: Rect, scale_120: u32) -> Rect {
    let offset = BOX_SHADOW_OFFSET.saturating_mul(scale_120).div_ceil(120);
    Rect {
        x: panel.x.saturating_add(offset),
        y: panel.y.saturating_add(offset),
        ..panel
    }
}

/// Text placed in a box's frame, in buffer px.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct BoxText {
    /// What is drawn: the label itself, or truncated with `…`.
    pub(super) text: String,
    /// The cells the text covers.
    pub(super) rect: Rect,
}

/// A label set into the frame: the panel-coloured knockout that interrupts
/// the frame line, with one cell of padding either side of its text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct BoxLabel {
    pub(super) knockout: Rect,
    pub(super) text: BoxText,
}

/// The legend set into the bottom frame: `key verb` pairs from the left.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct BoxLegend {
    pub(super) knockout: Rect,
    pub(super) pairs: Vec<(BoxText, BoxText)>,
}

/// Where a box's parts go, in buffer px.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ChromeBoxLayout {
    pub(super) panel: Rect,
    pub(super) shadow: Rect,
    /// The frame line's outer edge.
    pub(super) frame: Rect,
    pub(super) border: u32,
    pub(super) title: Option<BoxLabel>,
    pub(super) right_label: Option<BoxLabel>,
    /// The right label in its parts and separators, each at its cells and
    /// whether it is the active part, when one is marked and all fit.
    pub(super) right_parts: Option<Vec<(BoxText, bool)>>,
    pub(super) legend: Option<BoxLegend>,
}

/// What a box shows in its frame.
#[derive(Clone, Copy, Debug)]
pub(super) struct ChromeBox<'a> {
    /// Lowercase, bold, primary text at the left of the top frame.
    pub(super) title: &'a str,
    /// Secondary text at the right of the top frame, in parts, most
    /// important first, joined by ` · `; may be empty.
    pub(super) right_label: &'a [&'a str],
    /// The right label's part drawn bold in the accent, as the `dojos`
    /// picker marks its scope, while every part is shown.
    pub(super) right_active: Option<usize>,
    /// `(key, verb)` pairs in the bottom frame, most important first.
    pub(super) legend: &'a [(&'a str, &'a str)],
}

/// The run of a frame edge that labels may use, between the corners, and
/// the chrome cell they are laid out in.
#[derive(Clone, Copy)]
struct FrameSpan {
    left: u32,
    width: u32,
    cell_width: u32,
    cell_height: u32,
}

impl FrameSpan {
    const fn right(self) -> u32 {
        self.left.saturating_add(self.width)
    }

    /// The width of `text_cells` cells with one cell of padding either side.
    const fn padded(self, text_cells: u32) -> u32 {
        text_cells.saturating_add(2).saturating_mul(self.cell_width)
    }

    /// `text` with its first cell at `x`, on the band at `y`.
    fn text(self, text: &str, x: u32, y: u32) -> BoxText {
        BoxText {
            text: text.to_owned(),
            rect: Rect {
                x,
                y,
                width: cells_u32(text).saturating_mul(self.cell_width),
                height: self.cell_height,
            },
        }
    }

    /// `text` on a knockout starting at `x`, on the band at `y`.
    fn label(self, text: &str, x: u32, y: u32) -> BoxLabel {
        let text = self.text(text, x.saturating_add(self.cell_width), y);
        BoxLabel {
            knockout: Rect {
                x,
                y,
                width: text
                    .rect
                    .width
                    .saturating_add(self.cell_width.saturating_mul(2)),
                height: self.cell_height,
            },
            text,
        }
    }
}

impl ChromeBox<'_> {
    /// Lays the box out over `panel` (the fill, buffer px) for chrome cells
    /// of `cell` (width, height) buffer px.
    ///
    /// The title truncates first, down to one character and its `…`; then
    /// the right label drops parts from its end, and its first part ends in
    /// `…`, so what matters most stays visible. Legend pairs drop from
    /// the end until the rest fits inside the frame.
    #[must_use]
    pub(super) fn layout(&self, panel: Rect, scale_120: u32, cell: (u32, u32)) -> ChromeBoxLayout {
        let scaled = |logical: u32| logical.saturating_mul(scale_120).div_ceil(120);
        let border = border_width(scale_120);
        let inset = scaled(BOX_FRAME_INSET)
            .min(panel.width / 2)
            .min(panel.height / 2);
        let frame = Rect {
            x: panel.x.saturating_add(inset),
            y: panel.y.saturating_add(inset),
            width: panel.width.saturating_sub(inset.saturating_mul(2)),
            height: panel.height.saturating_sub(inset.saturating_mul(2)),
        };
        let label_inset = scaled(BOX_LABEL_INSET).min(frame.width / 2);
        let span = FrameSpan {
            left: frame.x.saturating_add(label_inset),
            width: frame.width.saturating_sub(label_inset.saturating_mul(2)),
            cell_width: cell.0.max(1),
            cell_height: cell.1.min(panel.height),
        };
        // A label's band: centred on its frame line, inside the panel fill.
        let band = |line_y: u32| {
            let centred = line_y
                .saturating_add(border / 2)
                .saturating_sub(span.cell_height / 2);
            let lowest = panel
                .y
                .saturating_add(panel.height)
                .saturating_sub(span.cell_height);
            centred.max(panel.y).min(lowest)
        };
        let (title, right_label) = self.top_labels(span, band(frame.y));
        let bottom_line = frame.y.saturating_add(frame.height).saturating_sub(border);
        ChromeBoxLayout {
            panel,
            shadow: shadow_rect(panel, scale_120),
            frame,
            border,
            title,
            right_parts: right_label
                .as_ref()
                .and_then(|label| self.right_parts(span, label)),
            right_label,
            legend: self.legend_layout(span, band(bottom_line)),
        }
    }

    fn top_labels(&self, span: FrameSpan, y: u32) -> (Option<BoxLabel>, Option<BoxLabel>) {
        let gap = span.cell_width;
        let title_floor = if self.title.is_empty() {
            0
        } else {
            span.padded(cells_u32(self.title).min(2))
                .saturating_add(gap)
        };
        let right_text = self.right_label_text(span, span.width.saturating_sub(title_floor));
        let right_need = right_text
            .as_deref()
            .map_or(0, |text| span.padded(cells_u32(text)));
        let title_room = if right_need > 0 {
            span.width.saturating_sub(right_need.saturating_add(gap))
        } else {
            span.width
        };
        let title_cells =
            title_room.saturating_sub(span.cell_width.saturating_mul(2)) / span.cell_width;
        let title_text = truncate_picker_text(
            self.title,
            usize::try_from(title_cells).unwrap_or(usize::MAX),
        );
        let title = (!title_text.is_empty()).then(|| span.label(&title_text, span.left, y));
        let right_label =
            right_text.map(|text| span.label(&text, span.right().saturating_sub(right_need), y));
        (title, right_label)
    }

    /// `label`'s parts and the ` · ` between them, each placed at its own
    /// cells, when a part is marked active: all of them, or the active part
    /// alone, marked, when they do not all fit.
    fn right_parts(&self, span: FrameSpan, label: &BoxLabel) -> Option<Vec<(BoxText, bool)>> {
        let active = self.right_active?;
        let parts = self
            .right_label
            .iter()
            .copied()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        if parts.join(" · ") != label.text.text {
            return Some(vec![(label.text.clone(), true)]);
        }
        let mut x = label.text.rect.x;
        let mut placed = Vec::with_capacity(parts.len() * 2);
        for (index, part) in parts.iter().enumerate() {
            if index > 0 {
                placed.push((span.text(" · ", x, label.text.rect.y), false));
                x = x.saturating_add(span.cell_width.saturating_mul(3));
            }
            let text = span.text(part, x, label.text.rect.y);
            x = x.saturating_add(text.rect.width);
            placed.push((text, index == active));
        }
        Some(placed)
    }

    /// The right label's parts that fit in `room` with their padding,
    /// joined by ` · `: later parts drop first, and when even the first
    /// alone does not fit it ends in `…`. With a part marked active, the
    /// label is all of them or that part alone, so it never names another
    /// one. `None` only when not one character and its `…` fit.
    fn right_label_text(&self, span: FrameSpan, room: u32) -> Option<String> {
        let parts = self
            .right_label
            .iter()
            .copied()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        let whole = parts.join(" · ");
        let active = self
            .right_active
            .and_then(|active| self.right_label.get(active).copied())
            .filter(|part| !part.is_empty());
        if let Some(active) = active {
            if span.padded(cells_u32(&whole)) <= room {
                return Some(whole);
            }
            if span.padded(cells_u32(active)) <= room {
                return Some(active.to_owned());
            }
            let cells = (room / span.cell_width).saturating_sub(2);
            return (cells >= 2).then(|| {
                truncate_picker_text(active, usize::try_from(cells).unwrap_or(usize::MAX))
            });
        }
        let first = parts.first()?;
        for shown in (1..=parts.len()).rev() {
            let text = parts[..shown].join(" · ");
            if span.padded(cells_u32(&text)) <= room {
                return Some(text);
            }
        }
        let cells = (room / span.cell_width).saturating_sub(2);
        (cells >= 2)
            .then(|| truncate_picker_text(first, usize::try_from(cells).unwrap_or(usize::MAX)))
    }

    fn legend_layout(&self, span: FrameSpan, y: u32) -> Option<BoxLegend> {
        let pair_cells = |(key, verb): (&str, &str)| cells(key) + 1 + cells(verb);
        let cells_width = |count: usize| span.padded(u32::try_from(count).unwrap_or(u32::MAX));
        let mut shown = 0;
        let mut used_cells = 0_usize;
        for (index, pair) in self.legend.iter().enumerate() {
            let gap = if index == 0 { 0 } else { LEGEND_PAIR_GAP_CELLS };
            let next = used_cells + gap + pair_cells(*pair);
            if cells_width(next) > span.width {
                break;
            }
            used_cells = next;
            shown = index + 1;
        }
        if shown == 0 {
            return None;
        }
        let cell_x = |offset: usize| {
            span.left.saturating_add(
                u32::try_from(offset + 1)
                    .unwrap_or(u32::MAX)
                    .saturating_mul(span.cell_width),
            )
        };
        let mut offset = 0_usize;
        let pairs = self.legend[..shown]
            .iter()
            .map(|&(key, verb)| {
                let verb_offset = offset + cells(key) + 1;
                let placed = (
                    span.text(key, cell_x(offset), y),
                    span.text(verb, cell_x(verb_offset), y),
                );
                offset += pair_cells((key, verb)) + LEGEND_PAIR_GAP_CELLS;
                placed
            })
            .collect();
        Some(BoxLegend {
            knockout: Rect {
                x: span.left,
                y,
                width: cells_width(used_cells),
                height: span.cell_height,
            },
            pairs,
        })
    }
}

impl ChromeBoxLayout {
    /// The hard shadow, then the panel fill: everything under the content.
    pub(super) fn paint_surface(
        &self,
        painter: &mut ChromePainter<'_>,
        palette: &SessionPickerPalette,
    ) {
        painter.blend(self.shadow, palette.shadow);
        painter.fill(self.panel, palette.panel_fill);
    }

    /// The frame in `frame_color` and the labels set into it, painted after
    /// the content, so a row that reaches the frame never covers it.
    pub(super) fn paint_frame(
        &self,
        painter: &mut ChromePainter<'_>,
        palette: &SessionPickerPalette,
        frame_color: u32,
    ) -> Result<()> {
        self.paint_frame_titled(
            painter,
            palette,
            frame_color,
            (ChromeTextStyle::Bold, palette.primary),
        )
    }

    /// [`Self::paint_frame`] with the title in `title` (its style and
    /// colour): a passive box, such as the history strip, sets its title
    /// regular and muted.
    pub(super) fn paint_frame_titled(
        &self,
        painter: &mut ChromePainter<'_>,
        palette: &SessionPickerPalette,
        frame_color: u32,
        title: (ChromeTextStyle, u32),
    ) -> Result<()> {
        paint_outline(painter, self.frame, self.border, frame_color);
        let (title_style, title_color) = title;
        if let Some(title) = &self.title {
            painter.fill(title.knockout, palette.panel_fill);
            painter.text(
                &title.text.text,
                title_style,
                title.text.rect,
                PickerTextAlignment::Left,
                title_color,
            )?;
        }
        if let Some(label) = &self.right_label {
            painter.fill(label.knockout, palette.panel_fill);
            let whole = [(label.text.clone(), false)];
            for (part, active) in self.right_parts.as_deref().unwrap_or(&whole) {
                let (style, color) = if *active {
                    (ChromeTextStyle::Bold, palette.accent_text)
                } else {
                    (ChromeTextStyle::Regular, palette.secondary)
                };
                painter.text(
                    &part.text,
                    style,
                    part.rect,
                    PickerTextAlignment::Left,
                    color,
                )?;
            }
        }
        if let Some(legend) = &self.legend {
            painter.fill(legend.knockout, palette.panel_fill);
            for (key, verb) in &legend.pairs {
                for (text, color) in [(key, palette.primary), (verb, palette.secondary)] {
                    painter.text(
                        &text.text,
                        ChromeTextStyle::Regular,
                        text.rect,
                        PickerTextAlignment::Left,
                        color,
                    )?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::ResolvedTheme,
        renderer::{ChromeSurface, chrome_panel_palette, text::chrome_maximum_cell_height},
    };

    const SCALES: [u32; 4] = [120, 150, 180, 240];

    fn right(rect: Rect) -> u32 {
        rect.x + rect.width
    }

    fn bottom(rect: Rect) -> u32 {
        rect.y + rect.height
    }

    fn inside(inner: Rect, outer: Rect) -> bool {
        inner.x >= outer.x
            && inner.y >= outer.y
            && right(inner) <= right(outer)
            && bottom(inner) <= bottom(outer)
    }

    /// Every chrome cell the tests use: the default UI size and the largest
    /// at each scale.
    fn cells_at(scale: u32) -> Vec<(u32, u32)> {
        [
            RenderContext::new(u16::MAX),
            RenderContext::new(u16::MAX).at_maximum_chrome_font_size(),
        ]
        .into_iter()
        .map(|context| {
            let mut cache = SessionPickerTextCache::default();
            let regular = cache
                .cell_metrics(&context, ChromeTextStyle::Regular, scale, 1)
                .unwrap();
            let bold = cache
                .cell_metrics(&context, ChromeTextStyle::Bold, scale, 1)
                .unwrap();
            (regular.0.max(bold.0), regular.1.max(bold.1))
        })
        .collect()
    }

    const LEGEND: [(&str, &str); 3] = [("↑↓", "move"), ("↵", "run"), ("esc", "close")];

    fn panel(scale: u32, logical_width: u32) -> Rect {
        picker_buffer_rect(
            Rect {
                x: 37,
                y: 91,
                width: logical_width,
                height: 300,
            },
            scale,
        )
    }

    #[test]
    fn the_frame_is_inset_inside_the_fill_and_the_shadow_is_offset() {
        for scale in SCALES {
            for cell in cells_at(scale) {
                let panel = panel(scale, 560);
                let layout = ChromeBox {
                    title: "commands",
                    right_label: &["62"],
                    right_active: None,
                    legend: &LEGEND,
                }
                .layout(panel, scale, cell);
                let inset = BOX_FRAME_INSET * scale / 120;
                assert!(layout.frame.x - panel.x >= inset, "scale {scale}");
                assert!(layout.frame.x - panel.x <= inset + 1, "scale {scale}");
                assert_eq!(layout.frame.y - panel.y, layout.frame.x - panel.x);
                assert_eq!(right(panel) - right(layout.frame), layout.frame.x - panel.x);
                assert_eq!(
                    bottom(panel) - bottom(layout.frame),
                    layout.frame.x - panel.x
                );
                assert_eq!(layout.border, scale.div_ceil(120));
                let offset = BOX_SHADOW_OFFSET * scale / 120;
                assert!((offset..=offset + 1).contains(&(layout.shadow.x - panel.x)));
                assert_eq!(layout.shadow.y - panel.y, layout.shadow.x - panel.x);
                assert_eq!(
                    (layout.shadow.width, layout.shadow.height),
                    (panel.width, panel.height)
                );
            }
        }
    }

    /// The `dojos` picker's scope: its parts sit where the whole label
    /// would, the active one marked; a label cut short marks none.
    #[test]
    fn a_marked_right_label_part_keeps_its_cells_in_the_label() {
        for scale in SCALES {
            for cell in cells_at(scale) {
                for active in [0, 1] {
                    let scope = ChromeBox {
                        title: "dojos",
                        right_label: &["recent", "this lair"],
                        right_active: Some(active),
                        legend: &LEGEND,
                    };
                    let layout = scope.layout(panel(scale, 560), scale, cell);
                    let label = layout.right_label.as_ref().unwrap();
                    assert_eq!(label.text.text, "recent · this lair");
                    let parts = layout.right_parts.as_ref().unwrap();
                    let texts = parts
                        .iter()
                        .map(|(part, _)| part.text.as_str())
                        .collect::<String>();
                    assert_eq!(texts, label.text.text);
                    assert_eq!(parts[0].0.rect.x, label.text.rect.x);
                    for pair in parts.windows(2) {
                        assert_eq!(pair[1].0.rect.x, right(pair[0].0.rect));
                    }
                    assert_eq!(right(parts[2].0.rect), right(label.text.rect));
                    let marked = parts
                        .iter()
                        .filter(|(_, marked)| *marked)
                        .map(|(part, _)| part.text.as_str())
                        .collect::<Vec<_>>();
                    assert_eq!(marked, [["recent", "this lair"][active]]);
                    // Too narrow for both parts: the active part alone,
                    // marked, never the other one.
                    let active_part = ["recent", "this lair"][active];
                    let narrow = scope.layout(panel(scale, 160), scale, cell);
                    if let Some(label) = narrow
                        .right_label
                        .as_ref()
                        .filter(|label| label.text.text != "recent · this lair")
                    {
                        let shown = label.text.text.trim_end_matches('…');
                        assert!(active_part.starts_with(shown), "{}", label.text.text);
                        assert_eq!(
                            narrow.right_parts.as_deref(),
                            Some(&[(label.text.clone(), true)][..])
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_title_is_knocked_out_of_the_top_frame_clear_of_its_corners() {
        for scale in SCALES {
            for cell in cells_at(scale) {
                let panel = panel(scale, 560);
                let layout = ChromeBox {
                    title: "key bindings",
                    right_label: &["3/62"],
                    right_active: None,
                    legend: &LEGEND,
                }
                .layout(panel, scale, cell);
                let title = layout.title.as_ref().unwrap();
                assert_eq!(title.text.text, "key bindings");
                let corner = (BOX_LABEL_INSET * scale).div_ceil(120);
                // Clear of the corners, inside the fill, across the line.
                assert_eq!(title.knockout.x, layout.frame.x + corner, "scale {scale}");
                assert!(inside(title.knockout, panel), "scale {scale} {cell:?}");
                assert!(title.knockout.y <= layout.frame.y);
                assert!(bottom(title.knockout) >= layout.frame.y + layout.border);
                // One cell of padding either side of the text.
                assert_eq!(title.text.rect.x, title.knockout.x + cell.0);
                assert_eq!(right(title.knockout), right(title.text.rect) + cell.0);
                assert_eq!(title.text.rect.width, 12 * cell.0);
                assert!(inside(title.text.rect, title.knockout));
                // The content starts below it at the tallest chrome cell.
                assert!(cell.1 <= chrome_maximum_cell_height(scale));
                let content = panel.y + (BOX_EDGE + BOX_CLEARANCE) * scale / 120;
                assert!(bottom(title.knockout) <= content, "scale {scale} {cell:?}");

                let right_label = layout.right_label.as_ref().unwrap();
                assert_eq!(right_label.text.text, "3/62");
                assert!(right(right_label.knockout) + corner <= right(layout.frame));
                assert!(inside(right_label.knockout, panel));
                assert!(right(title.knockout) < right_label.knockout.x);
            }
        }
    }

    #[test]
    fn the_title_truncates_before_the_right_label_and_never_overlaps_it() {
        const PARTS: [&str; 3] = ["2 waiting", "first 512 only", "12/40"];
        let whole = PARTS.join(" · ");
        for scale in SCALES {
            for cell in cells_at(scale) {
                for logical_width in [40, 80, 120, 160, 200, 240, 320, 352, 440, 560] {
                    let panel = panel(scale, logical_width);
                    let layout = ChromeBox {
                        title: "agent access",
                        right_label: &PARTS,
                        right_active: None,
                        legend: &LEGEND,
                    }
                    .layout(panel, scale, cell);
                    let frame_right = right(layout.frame);
                    for label in [&layout.title, &layout.right_label].into_iter().flatten() {
                        assert!(label.knockout.x >= layout.frame.x, "{logical_width}");
                        assert!(right(label.knockout) <= frame_right, "{logical_width}");
                    }
                    if let (Some(title), Some(right_label)) = (&layout.title, &layout.right_label) {
                        assert!(
                            right(title.knockout) < right_label.knockout.x,
                            "scale {scale}, width {logical_width}"
                        );
                        assert!(cells(&title.text.text) >= 2);
                    }
                    let corner = (BOX_LABEL_INSET * scale)
                        .div_ceil(120)
                        .min(layout.frame.width / 2);
                    let span = layout.frame.width - 2 * corner;
                    // Title floor (`a…` padded) and the gap after it.
                    let room = span.saturating_sub((2 + 2) * cell.0 + cell.0);
                    match &layout.right_label {
                        // Whole parts from the first, or the first cut.
                        Some(right_label) => {
                            let text = &right_label.text.text;
                            let kept = (1..=PARTS.len())
                                .map(|shown| PARTS[..shown].join(" · "))
                                .any(|parts| &parts == text);
                            assert!(
                                kept || (text.ends_with('…')
                                    && PARTS[0].starts_with(text.trim_end_matches('…'))),
                                "scale {scale}, width {logical_width}: {text}"
                            );
                            assert!(right_label.knockout.width <= room);
                            // A whole part drops only when it would not fit.
                            if let Some(shown) =
                                (1..PARTS.len()).find(|shown| &PARTS[..*shown].join(" · ") == text)
                            {
                                let next = PARTS[..=shown].join(" · ");
                                let next = u32::try_from(cells(&next)).unwrap();
                                assert!((next + 2) * cell.0 > room, "{logical_width}");
                            }
                        }
                        // It goes only when not one character and its `…`
                        // fit beside the title's own.
                        None => assert!((2 + 2) * cell.0 > room, "scale {scale}, {logical_width}"),
                    }
                    if let Some(title) = &layout.title
                        && title.text.text != "agent access"
                    {
                        assert!(title.text.text.ends_with('…'));
                        assert!("agent access".starts_with(title.text.text.trim_end_matches('…')));
                    }
                }
                // In a ~400 px Window (a 352 px panel) what waits stays.
                let layout = ChromeBox {
                    title: "agent access",
                    right_label: &["1 waiting", "first 512 only", "3"],
                    right_active: None,
                    legend: &LEGEND,
                }
                .layout(panel(scale, 352), scale, cell);
                let right_label = layout.right_label.unwrap();
                assert!(
                    right_label.text.text.starts_with("1 waiting"),
                    "scale {scale}: {}",
                    right_label.text.text
                );
                assert!(layout.title.is_some());
                // Wide enough, both whole.
                let layout = ChromeBox {
                    title: "agent access",
                    right_label: &PARTS,
                    right_active: None,
                    legend: &[],
                }
                .layout(panel(scale, 900), scale, cell);
                assert_eq!(layout.title.unwrap().text.text, "agent access");
                assert_eq!(layout.right_label.unwrap().text.text, whole);
                assert!(layout.legend.is_none());
            }
        }
    }

    #[test]
    fn legend_pairs_drop_from_the_end_and_never_pass_the_frame() {
        let legend = [
            ("↑↓", "move"),
            ("pgup pgdn", "page"),
            ("ctrl u", "clear"),
            ("esc", "close"),
        ];
        for scale in SCALES {
            for cell in cells_at(scale) {
                let mut previous = 0;
                for logical_width in (40..=640).step_by(8) {
                    let panel = panel(scale, logical_width);
                    let layout = ChromeBox {
                        title: "key bindings",
                        right_label: &[],
                        right_active: None,
                        legend: &legend,
                    }
                    .layout(panel, scale, cell);
                    let corner = BOX_LABEL_INSET * scale / 120;
                    let shown = layout
                        .legend
                        .as_ref()
                        .map_or(0, |legend| legend.pairs.len());
                    // Wider never shows fewer pairs.
                    assert!(shown >= previous, "scale {scale}, width {logical_width}");
                    previous = shown;
                    let Some(placed) = &layout.legend else {
                        continue;
                    };
                    assert!(placed.knockout.x >= layout.frame.x + corner);
                    assert!(
                        right(placed.knockout) + corner <= right(layout.frame),
                        "scale {scale}, width {logical_width}"
                    );
                    assert!(inside(placed.knockout, panel));
                    assert!(placed.knockout.y + placed.knockout.height >= bottom(layout.frame));
                    assert!(placed.knockout.y < bottom(layout.frame));
                    let content_bottom = bottom(panel) - (BOX_EDGE + BOX_CLEARANCE) * scale / 120;
                    assert!(
                        placed.knockout.y >= content_bottom,
                        "scale {scale} {cell:?}"
                    );
                    // Pairs keep their order; the dropped ones are the last.
                    for ((key, verb), (expected_key, expected_verb)) in
                        placed.pairs.iter().zip(legend)
                    {
                        assert_eq!(
                            (key.text.as_str(), verb.text.as_str()),
                            (expected_key, expected_verb)
                        );
                        // One cell between key and verb.
                        assert_eq!(verb.rect.x, right(key.rect) + cell.0);
                        assert!(inside(key.rect, placed.knockout));
                        assert!(inside(verb.rect, placed.knockout));
                    }
                    // Two cells between pairs.
                    for pair in placed.pairs.windows(2) {
                        assert_eq!(pair[1].0.rect.x, right(pair[0].1.rect) + 2 * cell.0);
                    }
                }
                let wide = ChromeBox {
                    title: "key bindings",
                    right_label: &[],
                    right_active: None,
                    legend: &legend,
                }
                .layout(panel(scale, 1600), scale, cell);
                assert_eq!(
                    wide.legend.unwrap().pairs.len(),
                    legend.len(),
                    "scale {scale}"
                );
            }
        }
    }

    #[test]
    fn every_modal_scrim_shares_one_level() {
        for surface in [ChromeSurface::Menu, ChromeSurface::Popup] {
            let palette = chrome_panel_palette(ResolvedTheme::default(), surface);
            assert_eq!(palette.scrim[3], super::super::picker::MODAL_SCRIM_ALPHA);
            assert_eq!(palette.shadow, [0, 0, 0, 89]);
        }
        assert_eq!(super::super::picker::MODAL_SCRIM_ALPHA, 128);
    }

    #[test]
    fn a_painted_box_draws_its_frame_over_the_content_and_its_labels_on_knockouts() {
        let scale = 120;
        let context = RenderContext::new(u16::MAX);
        let mut cache = SessionPickerTextCache::default();
        let palette = chrome_panel_palette(ResolvedTheme::default(), ChromeSurface::Menu);
        let (width, height) = (640_u32, 400_u32);
        let mut canvas = vec![0_u8; usize::try_from(width * height * 4).unwrap()];
        let panel = Rect {
            x: 40,
            y: 40,
            width: 400,
            height: 200,
        };
        let mut painter =
            ChromePainter::new(&mut cache, &context, &mut canvas, width, height, scale, 1);
        let cell = painter.box_cell().unwrap();
        let layout = ChromeBox {
            title: "commands",
            right_label: &["62"],
            right_active: None,
            legend: &LEGEND,
        }
        .layout(panel, scale, cell);
        layout.paint_surface(&mut painter, &palette);
        // Content that reaches the frame on every side.
        painter.fill(
            Rect {
                x: panel.x + BOX_FRAME_INSET,
                y: panel.y + BOX_FRAME_INSET,
                width: panel.width - 2 * BOX_FRAME_INSET,
                height: panel.height - 2 * BOX_FRAME_INSET,
            },
            [1, 2, 3, 255],
        );
        layout
            .paint_frame(&mut painter, &palette, palette.focused_frame)
            .unwrap();
        painter.finish();
        let pixel = |x: u32, y: u32| {
            let index = usize::try_from((y * width + x) * 4).unwrap();
            <[u8; 4]>::try_from(&canvas[index..index + 4]).unwrap()
        };
        let [_, red, green, blue] = palette.focused_frame.to_be_bytes();
        let frame = [blue, green, red, 255];
        let fill = palette.panel_fill;
        let fill = [fill[2], fill[1], fill[0], fill[3]];
        // The frame's corners and edges are drawn over the content.
        for (x, y) in [
            (layout.frame.x, layout.frame.y),
            (right(layout.frame) - 1, bottom(layout.frame) - 1),
            (layout.frame.x, layout.frame.y + 100),
            (right(layout.frame) - 1, layout.frame.y + 100),
        ] {
            assert_eq!(pixel(x, y), frame, "({x}, {y})");
        }
        // The title's padding interrupts the frame with the panel fill.
        let title = layout.title.unwrap();
        assert_eq!(pixel(title.knockout.x, layout.frame.y), fill);
        let legend = layout.legend.unwrap();
        assert_eq!(pixel(legend.knockout.x, bottom(layout.frame) - 1), fill);
        // The shadow shows past the panel's bottom right.
        assert_ne!(pixel(right(panel) + 2, bottom(panel) + 2), [0, 0, 0, 0]);
        assert_eq!(pixel(right(panel) + 7, bottom(panel) + 7), [0, 0, 0, 0]);
    }
}
