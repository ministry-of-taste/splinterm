//! Shaped trusted-chrome text backed by the terminal renderer's existing frame path.

use anyhow::{Context, Result};
use splinterm_core::SplintId;
use splinterm_protocol::{
    ActiveScreen, CellAttributes, ColorSource, TerminalCell, TerminalInputModes, TerminalRow,
    TerminalSnapshot, UnderlineStyle,
};
use unicode_width::UnicodeWidthChar;

use crate::geometry::Rect;
use crate::theme::ChromeTint;

use super::{
    GlyphKey, RenderContext, SnapshotFrame, SnapshotGlyph, blend_glyph,
    icons::{ChromeIcon, ICON_FACE, IconCell, icon_glyph},
    packed_rgb, round_to_i32, u32_to_f32,
};

/// The tightest fixed chrome text line, in logical pixels: an Explorer footer
/// line and a saved-layout preview line are each this tall, and every other
/// chrome row, an Explorer row included, is taller.
///
/// Chrome text uses the terminal font family at its own UI size (the
/// context's chrome font size: `[main] ui-font-pixelsize`, otherwise
/// Omarchy's text size, otherwise 14 px), never the terminal font size or
/// runtime zoom. It follows that size only until its cell fills this line.
/// Past that it stops growing, because chrome rows have fixed heights:
/// glyphs larger than their rows were clipped into unreadable fragments.
pub(crate) const CHROME_MAXIMUM_LINE_HEIGHT: u32 = 24;

/// The tallest chrome text cell, in buffer pixels, at `scale_120`.
pub(crate) const fn chrome_maximum_cell_height(scale_120: u32) -> u32 {
    CHROME_MAXIMUM_LINE_HEIGHT.saturating_mul(scale_120) / 120
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ChromeTextStyle {
    Regular,
    Bold,
}

pub(crate) struct ChromeText {
    pub(super) frame: SnapshotFrame,
    cells: u32,
}

fn text_cells(text: &str) -> usize {
    text.chars()
        .map(|character| character.width().unwrap_or(0).min(2))
        .sum()
}

/// Trusted chrome text with icons in reserved cells.
///
/// Each icon holds [`ChromeIcon::CELLS`] spaces in `text` and is drawn from the
/// embedded icon face. Icons enter only through [`Self::push_icon`]: no
/// character in a label, title, or agent detail ever becomes an icon.
///
/// A run of cells may carry a [`ChromeTint`]: the painter draws its glyphs
/// in that role's color for the background it paints on, instead of the
/// line's own color. The tag, not the color, is part of the source, so a
/// text cache keyed by source stays valid when colors change.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub(crate) struct ChromeSource {
    text: String,
    /// `(first cell, icon)`, in cell order.
    icons: Vec<(u32, ChromeIcon)>,
    /// `(first cell, end cell, tint)`, in cell order, never overlapping.
    tints: Vec<(u32, u32, ChromeTint)>,
}

impl ChromeSource {
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    #[cfg(test)]
    pub(crate) fn icons(&self) -> &[(u32, ChromeIcon)] {
        &self.icons
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Whether nothing would be drawn: no icon and only blank text.
    pub(crate) fn is_blank(&self) -> bool {
        self.icons.is_empty() && self.text.chars().all(char::is_whitespace)
    }

    /// Display cells, counting each icon as [`ChromeIcon::CELLS`].
    pub(crate) fn cells(&self) -> usize {
        text_cells(&self.text)
    }

    pub(crate) fn push_str(&mut self, text: &str) -> &mut Self {
        self.text.push_str(text);
        self
    }

    pub(crate) fn push_icon(&mut self, icon: ChromeIcon) -> &mut Self {
        let cell = u32::try_from(self.cells()).unwrap_or(u32::MAX);
        self.icons.push((cell, icon));
        for _ in 0..ChromeIcon::CELLS {
            self.text.push(' ');
        }
        self
    }

    /// Pushes what `push` adds, tinted by `tint`.
    pub(crate) fn push_tinted(
        &mut self,
        tint: ChromeTint,
        push: impl FnOnce(&mut Self),
    ) -> &mut Self {
        let start = u32::try_from(self.cells()).unwrap_or(u32::MAX);
        push(self);
        let end = u32::try_from(self.cells()).unwrap_or(u32::MAX);
        if end > start {
            self.tints.push((start, end, tint));
        }
        self
    }

    /// The tinted runs, as `(first cell, end cell, tint)`.
    #[cfg(test)]
    pub(crate) fn tints(&self) -> &[(u32, u32, ChromeTint)] {
        &self.tints
    }

    /// This source without its tints: every glyph in the line's color.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn untinted(&self) -> Self {
        Self {
            tints: Vec::new(),
            ..self.clone()
        }
    }

    /// The tint of the cell at `column`, if any.
    pub(crate) fn tint_at(&self, column: u32) -> Option<ChromeTint> {
        self.tints
            .iter()
            .find(|(start, end, _)| (*start..*end).contains(&column))
            .map(|(_, _, tint)| *tint)
    }

    pub(crate) fn push_source(&mut self, other: &Self) -> &mut Self {
        let offset = u32::try_from(self.cells()).unwrap_or(u32::MAX);
        self.icons.extend(
            other
                .icons
                .iter()
                .map(|(cell, icon)| (cell.saturating_add(offset), *icon)),
        );
        self.tints
            .extend(other.tints.iter().map(|(start, end, tint)| {
                (
                    start.saturating_add(offset),
                    end.saturating_add(offset),
                    *tint,
                )
            }));
        self.text.push_str(&other.text);
        self
    }

    /// Keeps `text`, a prefix of this source's text possibly ended by an
    /// ellipsis, and only the icons wholly inside its first `kept_cells`.
    pub(super) fn with_prefix_text(&self, text: String, kept_cells: usize) -> Self {
        let kept_cells = u32::try_from(kept_cells).unwrap_or(u32::MAX);
        Self {
            text,
            icons: self
                .icons
                .iter()
                .copied()
                .filter(|(cell, _)| cell.saturating_add(ChromeIcon::CELLS) <= kept_cells)
                .collect(),
            tints: self
                .tints
                .iter()
                .filter(|(start, _, _)| *start < kept_cells)
                .map(|(start, end, tint)| (*start, (*end).min(kept_cells), *tint))
                .collect(),
        }
    }

    /// The text with each icon written as `[name]`, for assertions.
    #[cfg(test)]
    pub(crate) fn describe(&self) -> String {
        let mut described = String::new();
        let mut cell = 0_u32;
        let mut skip = 0_u32;
        for character in self.text.chars() {
            let width = u32::try_from(character.width().unwrap_or(0).min(2)).unwrap_or(0);
            if skip > 0 && width > 0 {
                skip -= 1;
                cell += width;
                continue;
            }
            if let Some((_, icon)) = self.icons.iter().find(|(start, _)| *start == cell) {
                described.push('[');
                described.push_str(icon.name());
                described.push(']');
                skip = ChromeIcon::CELLS - 1;
            } else {
                described.push(character);
            }
            cell += width;
        }
        described
    }
}

impl From<&str> for ChromeSource {
    fn from(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            icons: Vec::new(),
            tints: Vec::new(),
        }
    }
}

impl From<String> for ChromeSource {
    fn from(text: String) -> Self {
        Self {
            text,
            icons: Vec::new(),
            tints: Vec::new(),
        }
    }
}

const fn chrome_attributes(style: ChromeTextStyle) -> CellAttributes {
    CellAttributes {
        bold: matches!(style, ChromeTextStyle::Bold),
        dim: false,
        italic: false,
        underline: UnderlineStyle::None,
        underline_color_source: ColorSource::Default,
        underline_color: 0,
        strikethrough: false,
        blink: false,
        conceal: false,
        reverse: false,
        foreground_source: ColorSource::Default,
        foreground: 0,
        background_source: ColorSource::Default,
        background: 0,
    }
}

/// Adds each icon to a chrome frame as a mask glyph at its slot's first cell.
/// Icons past the frame or missing from the face leave their blank cells.
fn place_icons(
    frame: &mut SnapshotFrame,
    icons: &[(u32, ChromeIcon)],
    cell_count: u32,
) -> Result<()> {
    let cell = IconCell {
        width: frame.cell_width,
        height: frame.cell_height,
        ascent: frame.ascent,
        baseline: frame.baseline,
    };
    for (column, icon) in icons {
        if column.saturating_add(ChromeIcon::CELLS) > cell_count {
            continue;
        }
        let Some(glyph) = icon_glyph(*icon, frame.font_generation.id, frame.font_size, cell) else {
            continue;
        };
        let key = GlyphKey {
            face: ICON_FACE,
            glyph: u16::try_from(*icon as usize).context("icon index fits u16")?,
        };
        frame.cache.insert(key, glyph);
        frame.glyphs.push(SnapshotGlyph {
            key,
            column: *column,
            row: 0,
            cells: ChromeIcon::CELLS,
            cluster_advance: u32_to_f32(frame.cell_width.saturating_mul(ChromeIcon::CELLS)),
            x_offset: 0.0,
            y_offset: 0.0,
            foreground: [0xff; 3],
        });
    }
    Ok(())
}

impl ChromeText {
    #[cfg(test)]
    pub(crate) fn load(text: &str, scale_120: u32) -> Result<Self> {
        Self::load_styled(text, scale_120, ChromeTextStyle::Regular)
    }

    #[cfg(test)]
    pub(crate) fn load_with_context(
        text: &str,
        scale_120: u32,
        context: &RenderContext,
    ) -> Result<Self> {
        Self::load_styled_with_context(text, scale_120, ChromeTextStyle::Regular, context)
    }

    #[cfg(test)]
    pub(crate) fn load_styled(text: &str, scale_120: u32, style: ChromeTextStyle) -> Result<Self> {
        let context = super::compatibility_render_context()?;
        Self::load_styled_with_context(text, scale_120, style, &context)
    }

    #[cfg(test)]
    pub(crate) fn load_source_styled(
        source: &ChromeSource,
        scale_120: u32,
        style: ChromeTextStyle,
    ) -> Result<Self> {
        let context = super::compatibility_render_context()?;
        Self::load_source_styled_with_context(source, scale_120, style, &context)
    }

    pub(crate) fn load_styled_with_context(
        text: &str,
        scale_120: u32,
        style: ChromeTextStyle,
        context: &RenderContext,
    ) -> Result<Self> {
        Self::load_parts(
            text,
            &[],
            scale_120,
            style,
            context,
            chrome_maximum_cell_height(scale_120),
        )
    }

    pub(crate) fn load_source_styled_with_context(
        source: &ChromeSource,
        scale_120: u32,
        style: ChromeTextStyle,
        context: &RenderContext,
    ) -> Result<Self> {
        Self::load_parts(
            &source.text,
            &source.icons,
            scale_120,
            style,
            context,
            chrome_maximum_cell_height(scale_120),
        )
    }

    /// Loads like [`Self::load_source_styled_with_context`], with a cell at
    /// most `maximum_cell_height` buffer pixels tall and never taller than the
    /// chrome line cap. A chrome row shorter than that cap, such as a tab
    /// strip that follows a short Omarchy bar, keeps its text inside the row.
    pub(crate) fn load_source_styled_within(
        source: &ChromeSource,
        scale_120: u32,
        style: ChromeTextStyle,
        context: &RenderContext,
        maximum_cell_height: u32,
    ) -> Result<Self> {
        Self::load_parts(
            &source.text,
            &source.icons,
            scale_120,
            style,
            context,
            maximum_cell_height.min(chrome_maximum_cell_height(scale_120)),
        )
    }

    fn load_parts(
        text: &str,
        icons: &[(u32, ChromeIcon)],
        scale_120: u32,
        style: ChromeTextStyle,
        context: &RenderContext,
        maximum_cell_height: u32,
    ) -> Result<Self> {
        let attributes = chrome_attributes(style);
        let mut cell_row: Vec<TerminalCell> = Vec::new();
        for character in text.chars() {
            let width = character.width().unwrap_or(0).min(2);
            if width == 0 {
                if let Some(leader) = cell_row
                    .iter_mut()
                    .rev()
                    .find(|cell| cell.spacer_remaining.is_none())
                {
                    leader.content.push(character);
                }
                continue;
            }
            cell_row.push(TerminalCell {
                content: character.to_string(),
                spacer_remaining: None,
                attributes,
            });
            if width == 2 {
                cell_row.push(TerminalCell {
                    content: String::new(),
                    spacer_remaining: Some(1),
                    attributes,
                });
            }
        }
        let cell_count = u32::try_from(cell_row.len()).context("chrome title width fits u32")?;
        let columns = cell_count.max(1);
        let snapshot = TerminalSnapshot {
            splint_id: SplintId::new(),
            incarnation: 1,
            revision: 1,
            columns: usize::try_from(columns).context("chrome title columns fit usize")?,
            rows: 1,
            cursor_column: 0,
            cursor_row: 0,
            cursor_deferred_wrap: false,
            active_screen: ActiveScreen::Normal,
            input_modes: TerminalInputModes {
                application_cursor: false,
                application_keypad: false,
                focus_reporting: false,
                bracketed_paste: false,
                cursor_visible: false,
                cursor_blink: false,
                mouse_tracking: splinterm_protocol::MouseTracking::None,
                sgr_mouse: false,
            },
            palette: vec![0; 256],
            default_colors: [0x00ff_ffff, 0, 0],
            title: String::new(),
            visible_rows: vec![TerminalRow {
                row_id: None,
                linebreak: false,
                cells: {
                    cell_row.resize(
                        usize::try_from(columns).context("chrome title row width fits usize")?,
                        TerminalCell {
                            content: String::new(),
                            spacer_remaining: None,
                            attributes,
                        },
                    );
                    cell_row
                },
            }],
            history_generation: 1,
            oldest_available_scrollback_row_id: None,
            newest_available_scrollback_row_id: None,
            scrollback_rows: Vec::new(),
            available_scrollback_rows: 0,
            omitted_oldest_scrollback_rows: 0,
            images: None,
            exited_code: None,
            exited_signal: None,
        };
        let mut frame = SnapshotFrame::load_chrome_with_maximum_cell_height(
            &snapshot,
            scale_120,
            context,
            maximum_cell_height,
        )?;
        place_icons(&mut frame, icons, cell_count)?;
        Ok(Self {
            frame,
            cells: cell_count,
        })
    }

    pub(crate) const fn cells(&self) -> u32 {
        self.cells
    }

    pub(crate) const fn pixel_width(&self) -> u32 {
        self.cells.saturating_mul(self.frame.cell_width)
    }

    pub(crate) const fn pixel_height(&self) -> u32 {
        self.frame.cell_height
    }

    pub(crate) fn paint(
        &self,
        canvas: &mut [u8],
        width: u32,
        height: u32,
        origin: (u32, u32),
        clip: Rect,
        color: u32,
    ) {
        self.paint_colored(canvas, width, height, origin, clip, |_| color);
    }

    /// Paints like [`Self::paint`], each glyph in `color_at` its first
    /// cell's column: how a [`ChromeSource`]'s tints reach the canvas
    /// without a separate text load.
    pub(crate) fn paint_colored(
        &self,
        canvas: &mut [u8],
        width: u32,
        height: u32,
        origin: (u32, u32),
        clip: Rect,
        color_at: impl Fn(u32) -> u32,
    ) {
        let clip = (
            i32::try_from(clip.x).unwrap_or(i32::MAX),
            i32::try_from(clip.y).unwrap_or(i32::MAX),
            i32::try_from(clip.x.saturating_add(clip.width)).unwrap_or(i32::MAX),
            i32::try_from(clip.y.saturating_add(clip.height)).unwrap_or(i32::MAX),
        );
        for placed in &self.frame.glyphs {
            let glyph = &self.frame.cache[&placed.key];
            let x = origin
                .0
                .saturating_add(placed.column.saturating_mul(self.frame.cell_width));
            let baseline = origin
                .1
                .saturating_add(u32::try_from(self.frame.baseline).unwrap_or(0));
            blend_glyph(
                canvas,
                width,
                height,
                i32::try_from(x).unwrap_or(i32::MAX) + round_to_i32(placed.x_offset) + glyph.left,
                i32::try_from(baseline).unwrap_or(i32::MAX)
                    - round_to_i32(placed.y_offset)
                    - glyph.top,
                glyph,
                packed_rgb(color_at(placed.column)),
                Some(clip),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_text_keeps_combining_clusters_and_wide_cell_spans() {
        let text = ChromeText::load("e\u{301}界", 120).unwrap();
        assert_eq!(text.cells(), 3);
        let mut canvas = vec![0; 256 * 64 * 4];
        text.paint(
            &mut canvas,
            256,
            64,
            (8, 0),
            Rect {
                x: 0,
                y: 0,
                width: 256,
                height: 64,
            },
            0x12_34_56,
        );
        assert!(canvas.as_chunks::<4>().0.iter().any(|pixel| pixel[3] != 0));
    }

    /// VM case E6: chrome once drew terminal-sized glyphs inside fixed
    /// chrome rows, clipping every label into fragments. At the largest UI
    /// size, far taller than a chrome line, chrome text still stops at it.
    #[test]
    fn chrome_text_stops_growing_at_its_line_height_at_the_largest_ui_size() {
        for scale in [120, 150, 180, 240] {
            let context = RenderContext::new(u16::MAX).at_maximum_chrome_font_size();
            let bound = chrome_maximum_cell_height(scale);
            let unbounded = context.chrome_font_size(scale).unwrap();
            let primary = &context.font_generation().unwrap().faces[0];
            assert!(
                super::super::cell_metrics(primary, unbounded)
                    .unwrap()
                    .height
                    > bound * 3
            );
            for style in [ChromeTextStyle::Regular, ChromeTextStyle::Bold] {
                let text = ChromeText::load_styled_with_context(
                    "Preview saved layout",
                    scale,
                    style,
                    &context,
                )
                .unwrap();
                assert!(
                    text.pixel_height() <= bound,
                    "scale {scale}: {} > {bound}",
                    text.pixel_height()
                );
                // Bounded, not collapsed: it still fills most of the line.
                assert!(text.pixel_height() * 4 >= bound * 3, "scale {scale}");
                assert!(text.frame.font_size < unbounded);
            }
        }
    }

    /// Runtime zoom resizes the terminal only: chrome text keeps its UI
    /// size at every zoom, from the smallest to the largest.
    #[test]
    fn runtime_zoom_never_changes_chrome_text() {
        for scale in [120, 150, 180, 240] {
            let unzoomed = RenderContext::new(u16::MAX);
            let mut zoomed_out = unzoomed.clone();
            zoomed_out
                .apply_font_zoom_steps(-10, scale)
                .unwrap()
                .unwrap();
            let zoomed_in = unzoomed.clone().at_maximum_zoom(scale);
            assert_ne!(
                zoomed_in.effective_font_size(scale).unwrap().to_bits(),
                unzoomed.effective_font_size(scale).unwrap().to_bits()
            );
            let expected = ChromeText::load_with_context("Lair 1", scale, &unzoomed).unwrap();
            // 14 px fits every chrome line, so it is drawn at exactly 14 px.
            assert_eq!(
                expected.frame.font_size.to_bits(),
                (14.0 * u32_to_f32(scale) / 120.0).to_bits(),
                "scale {scale}"
            );
            for context in [&zoomed_out, &zoomed_in] {
                let text = ChromeText::load_with_context("Lair 1", scale, context).unwrap();
                assert_eq!(
                    text.frame.font_size.to_bits(),
                    expected.frame.font_size.to_bits(),
                    "scale {scale}"
                );
                assert_eq!(text.pixel_height(), expected.pixel_height());
                assert_eq!(text.pixel_width(), expected.pixel_width());
            }
        }
    }

    /// Chrome follows its own UI size, scaled by the output scale, and
    /// neither the terminal's configured size nor a followed terminal size.
    #[test]
    fn chrome_text_follows_its_ui_size_not_the_terminal_font_size() {
        use crate::geometry::FontSize;

        let context = RenderContext::new(u16::MAX)
            .with_font_size_override(Some(FontSize::Points(15.0)))
            .with_chrome_font_size(FontSize::Pixels(12.0));
        assert_eq!(
            context.effective_font_size(120).unwrap().to_bits(),
            20.0_f32.to_bits()
        );
        for (scale, pixels) in [(120, 12.0_f32), (180, 18.0), (240, 24.0)] {
            let text = ChromeText::load_with_context("Lair 1", scale, &context).unwrap();
            assert_eq!(text.frame.font_size.to_bits(), pixels.to_bits(), "{scale}");
        }
        let mut live = context;
        assert!(!live.replace_chrome_font_size(FontSize::Pixels(12.0)));
        assert!(live.replace_chrome_font_size(FontSize::Pixels(16.0)));
        let text = ChromeText::load_with_context("Lair 1", 120, &live).unwrap();
        assert_eq!(text.frame.font_size.to_bits(), 16.0_f32.to_bits());
        assert_eq!(
            live.effective_font_size(120).unwrap().to_bits(),
            20.0_f32.to_bits()
        );
    }

    /// Every size `omarchy-display-text-size` offers fits the tightest
    /// chrome line at 1x and 2x: at its own size while that fits, otherwise
    /// at the cap.
    #[test]
    fn omarchy_text_sizes_fit_the_chrome_line_cap() {
        use crate::geometry::FontSize;

        for px in 9_u16..=20 {
            let context =
                RenderContext::new(u16::MAX).with_chrome_font_size(FontSize::Pixels(f32::from(px)));
            for scale in [120, 150, 240] {
                let bound = chrome_maximum_cell_height(scale);
                let wanted = context.chrome_font_size(scale).unwrap();
                let primary = &context.font_generation().unwrap().faces[0];
                let fits = super::super::cell_metrics(primary, wanted).unwrap().height <= bound;
                for style in [ChromeTextStyle::Regular, ChromeTextStyle::Bold] {
                    let text =
                        ChromeText::load_styled_with_context("Lair 1", scale, style, &context)
                            .unwrap();
                    assert!(text.pixel_height() <= bound, "{px} px at {scale}");
                    if fits {
                        assert_eq!(
                            text.frame.font_size.to_bits(),
                            wanted.to_bits(),
                            "{px} px at {scale}"
                        );
                    } else {
                        assert!(text.frame.font_size < wanted, "{px} px at {scale}");
                    }
                }
            }
        }
    }

    #[test]
    fn tints_follow_their_cells_through_composition_and_truncation() {
        let mut source = ChromeSource::from("ab");
        source.push_tinted(ChromeTint::StatusWorking, |source| {
            source.push_str("cd");
        });
        // Pushing nothing adds no run.
        source.push_tinted(ChromeTint::StatusDone, |_| {});
        source.push_str("e");
        source.push_tinted(ChromeTint::Asking, |source| {
            source.push_icon(ChromeIcon::LockQuestion);
        });
        assert_eq!(
            source.tints(),
            [
                (2, 4, ChromeTint::StatusWorking),
                (5, 7, ChromeTint::Asking)
            ]
        );
        let mut outer = ChromeSource::from("x");
        outer.push_source(&source);
        assert_eq!(
            outer.tints(),
            [
                (3, 5, ChromeTint::StatusWorking),
                (6, 8, ChromeTint::Asking)
            ]
        );
        let tints: Vec<_> = (0..9).map(|column| outer.tint_at(column)).collect();
        assert_eq!(
            tints,
            [
                None,
                None,
                None,
                Some(ChromeTint::StatusWorking),
                Some(ChromeTint::StatusWorking),
                None,
                Some(ChromeTint::Asking),
                Some(ChromeTint::Asking),
                None
            ]
        );
        // Truncation keeps a cut run only up to the kept cells.
        let cut = outer.with_prefix_text("xabc…".into(), 4);
        assert_eq!(cut.tints(), [(3, 4, ChromeTint::StatusWorking)]);
        assert!(outer.with_prefix_text("xab…".into(), 3).tints().is_empty());
        // Tints are part of the source, so a cache keyed by it tells them apart.
        assert_ne!(outer, ChromeSource::from(outer.text()));
    }

    #[test]
    fn paint_colored_paints_each_tinted_column_in_its_color() {
        let mut source = ChromeSource::from("ii");
        source.push_tinted(ChromeTint::StatusWorking, |source| {
            source.push_str("ll");
        });
        source.push_str("ii");
        for scale in [120, 180, 240] {
            let text =
                ChromeText::load_source_styled(&source, scale, ChromeTextStyle::Regular).unwrap();
            let cell = text.frame.cell_width;
            let (width, height) = (text.pixel_width(), text.pixel_height());
            let clip = Rect {
                x: 0,
                y: 0,
                width,
                height,
            };
            let mut canvas = vec![0; (width * height * 4) as usize];
            text.paint_colored(&mut canvas, width, height, (0, 0), clip, |column| {
                if source.tint_at(column).is_some() {
                    0x00_ff_00
                } else {
                    0xff_00_00
                }
            });
            let (mut green, mut red) = (0, 0);
            for (index, pixel) in canvas.chunks_exact(4).enumerate() {
                let column = u32::try_from(index).unwrap() % width / cell;
                // BGRA: green is byte 1, red byte 2.
                if pixel[1] > 0 {
                    green += 1;
                    assert!((2..4).contains(&column), "green at column {column}");
                }
                if pixel[2] > 0 {
                    red += 1;
                    assert!(!(2..4).contains(&column), "red at column {column}");
                }
            }
            assert!(green > 0 && red > 0, "scale {scale}");
            // One color everywhere is exactly `paint`.
            let mut plain = vec![0; canvas.len()];
            text.paint(&mut plain, width, height, (0, 0), clip, 0xff_00_00);
            let mut colored = vec![0; canvas.len()];
            text.paint_colored(&mut colored, width, height, (0, 0), clip, |_| 0xff_00_00);
            assert_eq!(plain, colored);
        }
    }
}
