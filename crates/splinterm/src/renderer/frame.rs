//! Scale-dependent snapshot frame preparation and incremental row reduction.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use splinterm_automation_client::ImageContentLeaseSet;
use splinterm_protocol::{
    CellAttributes, ColorSource, MAX_COLUMNS, MAX_ROWS, ScrollDirection, TerminalScroll,
    TerminalSnapshot, UnderlineStyle,
};
use swash::{scale::image::Content, shape::ShapeContext};

use crate::{
    box_drawing,
    font_shaping::{FeatureSettings, FontLigatures},
    geometry::{CellGeometry, TerminalPadding, WindowGeometry},
};

use super::{
    BOX_DRAWING_FACE, CachedGlyph, CellMetrics, FontFace, FontGeneration, GlyphKey, RenderContext,
    SNAPSHOT_CJK, SNAPSHOT_EMOJI, SNAPSHOT_FALLBACK_START, SNAPSHOT_PRIMARY_BOLD,
    SNAPSHOT_PRIMARY_BOLD_ITALIC, SNAPSHOT_PRIMARY_ITALIC, SNAPSHOT_PRIMARY_REGULAR, cell_metrics,
    compatibility_render_context,
    decorations::{DecorationMetrics, DecorationSpan},
    images::{SnapshotImage, prepare_snapshot_images},
    snapshot_color_advance, snapshot_glyph, u32_to_f32, with_font_ref,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct SnapshotGlyph {
    pub(super) key: GlyphKey,
    /// Source grid column, not the left edge of ink or contextual dependencies.
    pub(super) column: u32,
    pub(super) row: u32,
    /// Authoritative source span; never used to infer contextual ink bounds.
    pub(super) cells: u32,
    pub(super) cluster_advance: f32,
    pub(super) x_offset: f32,
    pub(super) y_offset: f32,
    pub(super) foreground: [u8; 3],
}

/// One immutable, scale-dependent rendering of an owned daemon snapshot.
pub(crate) struct SnapshotFrame {
    pub(super) font_generation: Arc<FontGeneration>,
    pub(super) font_ligatures: FontLigatures,
    pub(super) font_features: FeatureSettings,
    pub(super) font_size: f32,
    pub(super) glyphs: Vec<SnapshotGlyph>,
    pub(super) decorations: Vec<DecorationSpan>,
    pub(super) cache: HashMap<GlyphKey, Arc<CachedGlyph>>,
    pub(super) backgrounds: Vec<[u8; 3]>,
    pub(super) default_backgrounds: Vec<bool>,
    pub(super) foregrounds: Vec<[u8; 3]>,
    pub(super) cell_metrics: Vec<DecorationMetrics>,
    pub(super) primary_metrics: [DecorationMetrics; 4],
    pub(super) cell_spans: Vec<u32>,
    pub(super) columns: u32,
    pub(super) rows: u32,
    pub(super) cell_width: u32,
    pub(super) cell_height: u32,
    pub(super) ascent: u32,
    pub(super) descent: u32,
    pub(super) baseline: i32,
    #[allow(dead_code, reason = "retained as the regular-face evidence baseline")]
    pub(super) underline_position: i32,
    #[allow(dead_code, reason = "retained as the regular-face evidence baseline")]
    pub(super) underline_thickness: u32,
    #[allow(dead_code, reason = "retained as the regular-face evidence baseline")]
    pub(super) strike_position: i32,
    #[allow(dead_code, reason = "retained as the regular-face evidence baseline")]
    pub(super) strike_thickness: u32,
    pub(super) padding: TerminalPadding,
    pub(super) cursor: Option<(u32, u32)>,
    pub(super) canvas_background: [u8; 3],
    pub(super) background_alpha: u16,
    pub(super) cursor_color: [u8; 3],
    pub(super) images: Vec<SnapshotImage>,
    pub(super) scale_120: u16,
}

pub(crate) struct TerminalSizeWithLimitStatus {
    pub size: (u16, u16, u16, u16),
    pub column_capped: bool,
    pub row_capped: bool,
}

impl SnapshotFrame {
    pub(crate) const fn cell_width(&self) -> u32 {
        self.cell_width
    }

    pub(crate) const fn cell_height(&self) -> u32 {
        self.cell_height
    }

    pub(crate) const fn image_count(&self) -> usize {
        self.images.len()
    }

    #[cfg(test)]
    pub(crate) const fn scale_120(&self) -> u16 {
        self.scale_120
    }

    pub(super) fn cell_geometry(&self) -> Result<CellGeometry> {
        CellGeometry::from_metrics(
            self.cell_width,
            self.cell_height,
            self.ascent,
            self.descent,
            u32::try_from(self.baseline).context("cell baseline is nonnegative")?,
        )
    }

    /// Tight geometry is used only for initial sizing and deterministic captures.
    pub(super) fn tight_geometry(&self) -> Result<WindowGeometry> {
        WindowGeometry::for_grid(
            self.columns,
            self.rows,
            self.cell_geometry()?,
            self.padding,
            u32::from(self.scale_120),
        )
    }

    #[cfg(test)]
    pub(crate) fn window_geometry(
        &self,
        logical_width: u32,
        logical_height: u32,
        scale_120: u32,
    ) -> Result<WindowGeometry> {
        self.window_geometry_with_limits(
            logical_width,
            logical_height,
            scale_120,
            MAX_COLUMNS,
            MAX_ROWS,
        )
    }

    pub(crate) fn window_geometry_with_limits(
        &self,
        logical_width: u32,
        logical_height: u32,
        scale_120: u32,
        maximum_columns: u16,
        maximum_rows: u16,
    ) -> Result<WindowGeometry> {
        WindowGeometry::fit_window(
            logical_width,
            logical_height,
            self.cell_geometry()?,
            self.padding,
            scale_120,
            2,
            u32::from(maximum_columns),
            2,
            u32::from(maximum_rows),
        )
    }

    #[allow(
        clippy::unused_self,
        reason = "cell rectangles are resolved through the frame consumer boundary"
    )]
    pub(super) fn cell_rect(
        &self,
        geometry: &WindowGeometry,
        column: u32,
        row: u32,
    ) -> Option<(i32, i32, u32, u32)> {
        let rect = geometry.cell_rect(column, row)?;
        Some((
            i32::try_from(rect.x).ok()?,
            i32::try_from(rect.y).ok()?,
            rect.width,
            rect.height,
        ))
    }

    pub(crate) fn initial_logical_size(
        &self,
        columns: u16,
        rows: u16,
        scale_120: u32,
    ) -> Result<(u32, u32)> {
        let geometry = WindowGeometry::for_grid(
            u32::from(columns),
            u32::from(rows),
            self.cell_geometry()?,
            self.padding,
            scale_120,
        )?;
        Ok((geometry.logical_width(), geometry.logical_height()))
    }

    pub(crate) fn cell_at(
        &self,
        logical_x: f64,
        logical_y: f64,
        geometry: &WindowGeometry,
    ) -> Option<(usize, usize)> {
        let (row, column) = (geometry.surface_scale_120() == u32::from(self.scale_120))
            .then(|| geometry.cell_at_logical(logical_x, logical_y))??;
        (row < self.rows as usize && column < self.columns as usize).then_some((row, column))
    }

    pub(crate) fn cursor_rectangle(
        &self,
        geometry: &WindowGeometry,
    ) -> Option<(i32, i32, i32, i32)> {
        let (column, row) = self.cursor?;
        if geometry.surface_scale_120() != u32::from(self.scale_120) {
            return None;
        }
        let rect = geometry.logical_cell_rect(column, row)?;
        Some((rect.x, rect.y, rect.width, rect.height))
    }

    #[cfg(test)]
    pub(crate) fn terminal_size(
        &self,
        logical_width: u32,
        logical_height: u32,
        scale_120: u32,
    ) -> Result<(u16, u16, u16, u16)> {
        self.terminal_size_with_limits(
            logical_width,
            logical_height,
            scale_120,
            MAX_COLUMNS,
            MAX_ROWS,
        )
    }

    #[cfg(test)]
    pub(crate) fn terminal_size_with_limits(
        &self,
        logical_width: u32,
        logical_height: u32,
        scale_120: u32,
        maximum_columns: u16,
        maximum_rows: u16,
    ) -> Result<(u16, u16, u16, u16)> {
        self.terminal_size_with_limit_status(
            logical_width,
            logical_height,
            scale_120,
            maximum_columns,
            maximum_rows,
        )
        .map(|status| status.size)
    }

    pub(crate) fn terminal_size_with_limit_status(
        &self,
        logical_width: u32,
        logical_height: u32,
        scale_120: u32,
        maximum_columns: u16,
        maximum_rows: u16,
    ) -> Result<TerminalSizeWithLimitStatus> {
        let geometry = self.window_geometry_with_limits(
            logical_width,
            logical_height,
            scale_120,
            maximum_columns,
            maximum_rows,
        )?;
        let (pixel_width, pixel_height) = geometry.terminal_pixels()?;
        Ok(TerminalSizeWithLimitStatus {
            size: (
                u16::try_from(geometry.columns).context("terminal columns fit u16")?,
                u16::try_from(geometry.rows).context("terminal rows fit u16")?,
                pixel_width,
                pixel_height,
            ),
            column_capped: geometry.column_capped,
            row_capped: geometry.row_capped,
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "static snapshot preparation keeps bounded font, shaping, cache, cell-span, and color conversion together"
    )]
    pub(crate) fn load(snapshot: &TerminalSnapshot, integer_scale: u32) -> Result<Self> {
        let scale_120 = integer_scale
            .checked_mul(120)
            .context("integer scale overflow")?;
        Self::load_scaled(snapshot, scale_120)
    }

    pub(crate) fn load_scaled(snapshot: &TerminalSnapshot, scale_120: u32) -> Result<Self> {
        let context = compatibility_render_context()?;
        Self::load_scaled_with_context(snapshot, scale_120, &context)
    }

    pub(crate) fn load_scaled_with_context(
        snapshot: &TerminalSnapshot,
        scale_120: u32,
        context: &RenderContext,
    ) -> Result<Self> {
        Self::load_scaled_with_sources_and_context(snapshot, scale_120, None, context)
    }

    pub(crate) fn load_scaled_with_sources(
        snapshot: &TerminalSnapshot,
        scale_120: u32,
        sources: Option<&ImageContentLeaseSet>,
    ) -> Result<Self> {
        let context = compatibility_render_context()?;
        Self::load_scaled_with_sources_and_context(snapshot, scale_120, sources, &context)
    }

    pub(crate) fn load_scaled_with_sources_and_context(
        snapshot: &TerminalSnapshot,
        scale_120: u32,
        sources: Option<&ImageContentLeaseSet>,
        context: &RenderContext,
    ) -> Result<Self> {
        Self::load_bounded(snapshot, scale_120, sources, context, None)
    }

    /// Loads trusted chrome text: like [`Self::load_scaled_with_context`],
    /// but at the context's chrome font size instead of the zoomed terminal
    /// size, and shrinking that font, never growing it, until a cell is at
    /// most `maximum_cell_height` buffer pixels tall, so chrome stays inside
    /// its fixed rows.
    pub(super) fn load_chrome_with_maximum_cell_height(
        snapshot: &TerminalSnapshot,
        scale_120: u32,
        context: &RenderContext,
        maximum_cell_height: u32,
    ) -> Result<Self> {
        Self::load_bounded(
            snapshot,
            scale_120,
            None,
            context,
            Some(maximum_cell_height),
        )
    }

    /// `chrome_cell_height` is `Some` only for trusted chrome text, which is
    /// drawn at the context's chrome font size within that cell height.
    #[allow(
        clippy::too_many_lines,
        reason = "bounded text and image source preparation share one immutable frame transaction"
    )]
    fn load_bounded(
        snapshot: &TerminalSnapshot,
        scale_120: u32,
        sources: Option<&ImageContentLeaseSet>,
        context: &RenderContext,
        chrome_cell_height: Option<u32>,
    ) -> Result<Self> {
        if snapshot.columns > usize::from(MAX_COLUMNS) || snapshot.rows > usize::from(MAX_ROWS) {
            bail!(
                "snapshot dimensions {}x{} exceed protocol limits {}x{}",
                snapshot.columns,
                snapshot.rows,
                MAX_COLUMNS,
                MAX_ROWS
            );
        }
        if !(120..=960).contains(&scale_120) {
            bail!("scale must be between 1x and 8x");
        }
        let scale_120 = u16::try_from(scale_120).context("scale fits u16")?;
        let font_generation = Arc::clone(context.font_generation()?);
        let faces = &font_generation.faces;
        let (font_size, metrics) = if let Some(maximum) = chrome_cell_height {
            let chrome_size = context.chrome_font_size(u32::from(scale_120))?;
            bounded_cell_metrics(&faces[0], chrome_size, maximum)?
        } else {
            let font_size = context.effective_font_size(u32::from(scale_120))?;
            (font_size, cell_metrics(&faces[0], font_size)?)
        };
        let cell_width = metrics.width;
        let cell_height = metrics.height;
        let baseline = metrics.baseline;
        let columns = u32::try_from(snapshot.columns).context("snapshot columns fit u32")?;
        let rows = u32::try_from(snapshot.rows).context("snapshot rows fit u32")?;
        let background_len = usize::try_from(columns)
            .ok()
            .and_then(|columns| {
                usize::try_from(rows)
                    .ok()
                    .and_then(|rows| columns.checked_mul(rows))
            })
            .context("snapshot background size overflow")?;
        if snapshot.palette.len() != 256 {
            bail!("snapshot palette must contain exactly 256 colors");
        }
        let default_foreground = packed_rgb(snapshot.default_colors[0]);
        let default_background = packed_rgb(snapshot.default_colors[1]);
        let cursor_color = packed_rgb(snapshot.default_colors[2]);
        let default_metrics = DecorationMetrics::from(metrics);
        let mut backgrounds = vec![default_background; background_len];
        let mut default_backgrounds = vec![true; background_len];
        let mut foregrounds = vec![default_foreground; background_len];
        let mut cell_metrics = vec![default_metrics; background_len];
        let mut cell_spans = vec![1; background_len];
        let mut glyphs = Vec::new();
        let mut decorations = Vec::new();
        let mut cache = HashMap::new();
        let mut shape_context = ShapeContext::new();
        let primary_metrics = primary_decoration_metrics(faces, font_size)?;

        for row_index in 0..snapshot.rows {
            prepare_snapshot_row(
                snapshot,
                row_index,
                faces,
                scale_120,
                font_size,
                cell_width,
                cell_height,
                baseline,
                default_foreground,
                default_background,
                &primary_metrics,
                context.font_ligatures(),
                context.font_features(),
                &mut shape_context,
                &mut backgrounds,
                &mut default_backgrounds,
                &mut foregrounds,
                &mut cell_metrics,
                &mut cell_spans,
                &mut glyphs,
                &mut decorations,
                &mut cache,
            )?;
        }
        let cursor = snapshot_cursor(snapshot, columns, rows);
        let images = prepare_snapshot_images(snapshot, sources)?;
        let mut frame = Self {
            font_generation,
            font_ligatures: context.font_ligatures(),
            font_features: context.font_features().clone(),
            font_size,
            glyphs,
            decorations,
            cache,
            backgrounds,
            default_backgrounds,
            foregrounds,
            cell_metrics,
            primary_metrics,
            cell_spans,
            columns,
            rows,
            cell_width,
            cell_height,
            ascent: metrics.ascent,
            descent: metrics.descent,
            baseline,
            underline_position: metrics.underline_position,
            underline_thickness: metrics.underline_thickness,
            strike_position: metrics.strike_position,
            strike_thickness: metrics.strike_thickness,
            padding: context.padding(),
            cursor,
            canvas_background: default_background,
            background_alpha: context.background_alpha(),
            cursor_color,
            images,
            scale_120,
        };
        frame.enforce_cache_budget();
        Ok(frame)
    }

    /// Rebuilds only rows whose semantic cell content changed.
    pub(crate) fn refresh_rows(
        &mut self,
        snapshot: &TerminalSnapshot,
        dirty_rows: &[bool],
    ) -> Result<()> {
        let context = compatibility_render_context()?;
        self.refresh_rows_with_context(snapshot, dirty_rows, &context)
    }

    pub(crate) fn refresh_rows_with_context(
        &mut self,
        snapshot: &TerminalSnapshot,
        dirty_rows: &[bool],
        context: &RenderContext,
    ) -> Result<()> {
        if snapshot.columns != self.columns as usize || snapshot.rows != self.rows as usize {
            bail!("incremental frame dimensions changed");
        }
        if snapshot.palette.len() != 256 {
            bail!("snapshot palette must contain exactly 256 colors");
        }
        let context_generation = context.font_generation()?;
        anyhow::ensure!(
            self.font_generation.id == context_generation.id,
            "incremental frame font generation changed; a full rebuild is required"
        );
        anyhow::ensure!(
            self.font_ligatures == context.font_ligatures()
                && self.font_features == *context.font_features(),
            "incremental frame shaping settings changed; a full rebuild is required"
        );
        anyhow::ensure!(
            self.font_size.to_bits()
                == context
                    .effective_font_size(u32::from(self.scale_120))?
                    .to_bits(),
            "incremental frame font size changed; a full rebuild is required"
        );
        self.refresh_prepared_rows(snapshot, dirty_rows)
    }

    fn refresh_prepared_rows(
        &mut self,
        snapshot: &TerminalSnapshot,
        dirty_rows: &[bool],
    ) -> Result<()> {
        anyhow::ensure!(
            snapshot.columns == self.columns as usize
                && snapshot.rows == self.rows as usize
                && snapshot.palette.len() == 256,
            "incremental snapshot geometry or palette is invalid"
        );
        let mut dirty_rows = dirty_rows.to_vec();
        dirty_rows.resize(snapshot.rows, false);
        self.mark_cursor_dirty_rows(snapshot, &mut dirty_rows);
        let faces = &self.font_generation.faces;
        let font_size = self.font_size;
        let default_foreground = packed_rgb(snapshot.default_colors[0]);
        let default_background = packed_rgb(snapshot.default_colors[1]);
        let mut shape_context = ShapeContext::new();
        self.glyphs.retain(|glyph| {
            !usize::try_from(glyph.row)
                .ok()
                .and_then(|row| dirty_rows.get(row))
                .copied()
                .unwrap_or(false)
        });
        self.decorations.retain(|decoration| {
            !usize::try_from(decoration.row)
                .ok()
                .and_then(|row| dirty_rows.get(row))
                .copied()
                .unwrap_or(false)
        });
        for (row_index, dirty) in dirty_rows.iter().copied().enumerate().take(snapshot.rows) {
            if !dirty {
                continue;
            }
            let start = row_index
                .checked_mul(snapshot.columns)
                .context("snapshot row start overflow")?;
            let end = start
                .checked_add(snapshot.columns)
                .context("snapshot row end overflow")?;
            self.backgrounds[start..end].fill(default_background);
            self.default_backgrounds[start..end].fill(true);
            self.foregrounds[start..end].fill(default_foreground);
            prepare_snapshot_row(
                snapshot,
                row_index,
                faces,
                self.scale_120,
                font_size,
                self.cell_width,
                self.cell_height,
                self.baseline,
                default_foreground,
                default_background,
                &self.primary_metrics,
                self.font_ligatures,
                &self.font_features,
                &mut shape_context,
                &mut self.backgrounds,
                &mut self.default_backgrounds,
                &mut self.foregrounds,
                &mut self.cell_metrics,
                &mut self.cell_spans,
                &mut self.glyphs,
                &mut self.decorations,
                &mut self.cache,
            )?;
        }
        self.glyphs.sort_by_key(|glyph| (glyph.row, glyph.column));
        self.decorations.sort_by_key(|span| (span.row, span.column));
        self.enforce_cache_budget();
        self.cursor = snapshot_cursor(snapshot, self.columns, self.rows);
        Ok(())
    }

    pub(crate) fn refresh_images(
        &mut self,
        snapshot: &TerminalSnapshot,
        sources: &ImageContentLeaseSet,
    ) -> Result<()> {
        self.images = prepare_snapshot_images(snapshot, Some(sources))?;
        Ok(())
    }

    /// Shifts prepared viewport rows and rebuilds only rows exposed by local
    /// history navigation. Returns the equivalent pixel scroll operation.
    #[cfg(test)]
    pub(crate) fn scroll_viewport_rows(
        &mut self,
        snapshot: &TerminalSnapshot,
        offset_delta: isize,
    ) -> Result<Option<TerminalScroll>> {
        let context = compatibility_render_context()?;
        self.scroll_viewport_rows_with_context(snapshot, offset_delta, &context)
    }

    pub(crate) fn scroll_viewport_rows_with_context(
        &mut self,
        snapshot: &TerminalSnapshot,
        offset_delta: isize,
        context: &RenderContext,
    ) -> Result<Option<TerminalScroll>> {
        // Pixel scroll-copy cannot erase a cursor-dependent context break moved
        // into a reused row. History transitions involving a visible cursor use
        // the caller's existing full-rebuild path; cursor-free history still copies.
        if self.font_ligatures == FontLigatures::Cursor
            && (self.cursor.is_some()
                || snapshot_cursor(snapshot, self.columns, self.rows).is_some())
        {
            return Ok(None);
        }
        if offset_delta == 0 || self.rows == 0 {
            return Ok(None);
        }
        let count = offset_delta.unsigned_abs().min(self.rows as usize);
        if count == 0 || count >= self.rows as usize {
            return Ok(None);
        }
        let rows = self.rows as usize;
        let columns = self.columns as usize;
        let direction = if offset_delta > 0 {
            let source = 0..(rows - count) * columns;
            let destination = count * columns;
            self.backgrounds.copy_within(source.clone(), destination);
            self.default_backgrounds
                .copy_within(source.clone(), destination);
            self.foregrounds.copy_within(source.clone(), destination);
            self.cell_metrics.copy_within(source.clone(), destination);
            self.cell_spans.copy_within(source, destination);
            self.glyphs.retain_mut(|glyph| {
                glyph.row = glyph
                    .row
                    .saturating_add(u32::try_from(count).unwrap_or(u32::MAX));
                glyph.row < self.rows
            });
            self.decorations.retain_mut(|span| {
                span.row = span
                    .row
                    .saturating_add(u32::try_from(count).unwrap_or(u32::MAX));
                span.row < self.rows
            });
            ScrollDirection::Reverse
        } else {
            let source = count * columns..rows * columns;
            self.backgrounds.copy_within(source.clone(), 0);
            self.default_backgrounds.copy_within(source.clone(), 0);
            self.foregrounds.copy_within(source.clone(), 0);
            self.cell_metrics.copy_within(source.clone(), 0);
            self.cell_spans.copy_within(source, 0);
            self.glyphs.retain_mut(|glyph| {
                let keep = glyph.row >= u32::try_from(count).unwrap_or(u32::MAX);
                if keep {
                    glyph.row -= u32::try_from(count).unwrap_or(0);
                }
                keep
            });
            self.decorations.retain_mut(|span| {
                let keep = span.row >= u32::try_from(count).unwrap_or(u32::MAX);
                if keep {
                    span.row -= u32::try_from(count).unwrap_or(0);
                }
                keep
            });
            ScrollDirection::Forward
        };
        let mut dirty = vec![false; rows];
        let exposed = match direction {
            ScrollDirection::Reverse => 0..count,
            ScrollDirection::Forward => rows - count..rows,
        };
        for row in exposed {
            dirty[row] = true;
        }
        self.refresh_rows_with_context(snapshot, &dirty, context)?;
        Ok(Some(TerminalScroll {
            direction,
            start_row: 0,
            end_row: rows,
            rows: count,
        }))
    }

    fn enforce_cache_budget(&mut self) {
        const MAX_FRAME_GLYPH_CACHE_ENTRIES: usize = 4096;
        if self.cache.len() <= MAX_FRAME_GLYPH_CACHE_ENTRIES {
            return;
        }
        let referenced: HashSet<_> = self.glyphs.iter().map(|glyph| glyph.key).collect();
        self.cache.retain(|key, _| referenced.contains(key));
    }

    /// Marks only old/new reported cursor rows, never blink/focus presentation.
    pub(crate) fn mark_cursor_dirty_rows(&self, snapshot: &TerminalSnapshot, dirty: &mut [bool]) {
        let next = snapshot_cursor(snapshot, self.columns, self.rows);
        if self.font_ligatures != FontLigatures::Cursor || self.cursor == next {
            return;
        }
        for (_, row) in self.cursor.into_iter().chain(next) {
            if let Some(dirty) = dirty.get_mut(row as usize) {
                *dirty = true;
            }
        }
    }

    /// Whether the frame was prepared for a grid of `snapshot`'s size, which
    /// refreshing some of its rows from `snapshot` requires.
    pub(crate) fn shows_grid_of(&self, snapshot: &TerminalSnapshot) -> bool {
        self.columns as usize == snapshot.columns && self.rows as usize == snapshot.rows
    }

    pub(crate) fn can_refresh_cursor_rows(&self, snapshot: &TerminalSnapshot) -> bool {
        self.font_ligatures == FontLigatures::Cursor
            && self.columns as usize == snapshot.columns
            && self.rows as usize == snapshot.rows
    }

    /// Uses the frame's immutable shaping inputs, including for cached panes.
    pub(crate) fn refresh_cursor(&mut self, snapshot: &TerminalSnapshot) -> Result<()> {
        if self.font_ligatures == FontLigatures::Cursor {
            self.refresh_prepared_rows(snapshot, &[])
        } else {
            self.cursor = snapshot_cursor(snapshot, self.columns, self.rows);
            Ok(())
        }
    }
}

pub(super) fn snapshot_cursor(
    snapshot: &TerminalSnapshot,
    columns: u32,
    rows: u32,
) -> Option<(u32, u32)> {
    if !snapshot.input_modes.cursor_visible {
        return None;
    }
    match (
        u32::try_from(snapshot.cursor_column),
        u32::try_from(snapshot.cursor_row),
    ) {
        (Ok(column), Ok(row)) if column < columns && row < rows => Some((column, row)),
        _ => None,
    }
}

/// The smallest pixel size the `FreeType` raster policy accepts.
const MINIMUM_BOUNDED_PIXEL_SIZE: f32 = 6.0;

/// Shrinks `font_size` until the primary face's cell is at most
/// `maximum_cell_height` pixels tall, or the raster floor is reached. A size
/// already within the bound is returned unchanged.
fn bounded_cell_metrics(
    primary_face: &FontFace,
    font_size: f32,
    maximum_cell_height: u32,
) -> Result<(f32, CellMetrics)> {
    let mut size = font_size;
    let mut metrics = cell_metrics(primary_face, size)?;
    // Cell height is close to linear in pixel size, so a proportional step
    // lands at or just above the bound; quarter-pixel steps absorb hinting.
    for _ in 0..16 {
        if metrics.height <= maximum_cell_height || size <= MINIMUM_BOUNDED_PIXEL_SIZE {
            break;
        }
        let proportional = size * u32_to_f32(maximum_cell_height) / u32_to_f32(metrics.height);
        size = proportional
            .min(size - 0.25)
            .max(MINIMUM_BOUNDED_PIXEL_SIZE);
        metrics = cell_metrics(primary_face, size)?;
    }
    Ok((size, metrics))
}

pub(super) fn primary_decoration_metrics(
    faces: &[FontFace],
    font_size: f32,
) -> Result<[DecorationMetrics; 4]> {
    Ok([
        cell_metrics(&faces[SNAPSHOT_PRIMARY_REGULAR], font_size)?.into(),
        cell_metrics(&faces[SNAPSHOT_PRIMARY_BOLD], font_size)?.into(),
        cell_metrics(&faces[SNAPSHOT_PRIMARY_ITALIC], font_size)?.into(),
        cell_metrics(&faces[SNAPSHOT_PRIMARY_BOLD_ITALIC], font_size)?.into(),
    ])
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "row preparation keeps one bounded shaping transaction explicit"
)]
pub(super) fn prepare_snapshot_row(
    snapshot: &TerminalSnapshot,
    row_index: usize,
    faces: &[FontFace],
    scale: u16,
    font_size: f32,
    cell_width: u32,
    cell_height: u32,
    baseline: i32,
    default_foreground: [u8; 3],
    default_background: [u8; 3],
    primary_metrics: &[DecorationMetrics; 4],
    font_ligatures: FontLigatures,
    font_features: &FeatureSettings,
    shape_context: &mut ShapeContext,
    backgrounds: &mut [[u8; 3]],
    default_backgrounds: &mut [bool],
    foregrounds: &mut [[u8; 3]],
    cell_metrics_by_cell: &mut [DecorationMetrics],
    cell_spans: &mut [u32],
    glyphs: &mut Vec<SnapshotGlyph>,
    decorations: &mut Vec<DecorationSpan>,
    cache: &mut HashMap<GlyphKey, Arc<CachedGlyph>>,
) -> Result<()> {
    let Some(row) = snapshot.visible_rows.get(row_index) else {
        return Ok(());
    };
    let row_glyph_start = glyphs.len();
    let joined = if font_ligatures == FontLigatures::Off {
        vec![false; snapshot.columns]
    } else {
        prepare_ascii_runs(
            snapshot,
            row_index,
            faces,
            font_size,
            font_ligatures,
            font_features,
            shape_context,
            glyphs,
            cache,
            default_foreground,
            default_background,
        )?
    };
    for (column_index, cell) in row.cells.iter().take(snapshot.columns).enumerate() {
        let (foreground, background) = rendition_colors(
            &cell.attributes,
            &snapshot.palette,
            default_foreground,
            default_background,
        );
        let background_index = row_index
            .checked_mul(snapshot.columns)
            .and_then(|index| index.checked_add(column_index))
            .context("snapshot cell index overflow")?;
        backgrounds[background_index] = background;
        default_backgrounds[background_index] =
            cell.attributes.background_source == ColorSource::Default && !cell.attributes.reverse;
        foregrounds[background_index] = foreground;
        let face_index = primary_face_index(&cell.attributes);
        let metrics = primary_metrics[face_index];
        cell_metrics_by_cell[background_index] = metrics;
        if cell.spacer_remaining.is_some() {
            cell_spans[background_index] = 0;
            continue;
        }
        let cells = leader_span(&row.cells, column_index);
        cell_spans[background_index] = cells;
        if cell.attributes.conceal {
            continue;
        }
        let column = u32::try_from(column_index).context("column fits u32")?;
        let row_number = u32::try_from(row_index).context("row fits u32")?;
        if cell.attributes.underline != UnderlineStyle::None || cell.attributes.strikethrough {
            let underline_color = wire_color(
                cell.attributes.underline_color_source,
                cell.attributes.underline_color,
                &snapshot.palette,
                foreground,
            );
            decorations.push(DecorationSpan {
                column,
                row: row_number,
                cells,
                underline: cell.attributes.underline,
                strikethrough: cell.attributes.strikethrough,
                underline_color,
                underline_uses_foreground: cell.attributes.underline_color_source
                    == ColorSource::Default,
                strike_color: foreground,
                metrics,
            });
        }
        if joined[column_index] || !cell_is_renderable(cell) {
            continue;
        }
        let mut characters = cell.content.chars();
        if let (Some(character), None) = (characters.next(), characters.next())
            && let Ok(glyph) = u16::try_from(u32::from(character))
        {
            let key = GlyphKey {
                face: BOX_DRAWING_FACE,
                glyph,
            };
            let generated =
                if let std::collections::hash_map::Entry::Vacant(entry) = cache.entry(key) {
                    let thickness = box_drawing::default_thickness(cell_width, cell_height, scale);
                    box_drawing::generate(character, cell_width, cell_height, thickness)
                        .is_some_and(|mask| {
                            entry.insert(Arc::new(CachedGlyph {
                                content: Content::Mask,
                                left: 0,
                                top: baseline,
                                width: mask.width,
                                height: mask.height,
                                data: mask.data,
                            }));
                            true
                        })
                } else {
                    true
                };
            if generated {
                glyphs.push(SnapshotGlyph {
                    key,
                    column,
                    row: row_number,
                    cells,
                    cluster_advance: u32_to_f32(cell_width),
                    x_offset: 0.0,
                    y_offset: 0.0,
                    foreground,
                });
                continue;
            }
        }
        let (face_index, content) =
            match select_face_for_text(faces, &cell.content, &cell.attributes) {
                Ok(face_index) => (face_index, cell.content.as_str()),
                Err(_) => (primary_face_index(&cell.attributes), "\u{fffd}"),
            };
        let shaped_glyphs = with_font_ref(&faces[face_index], |font, coords| {
            let mut shaped_glyphs = Vec::new();
            let mut shaper = shape_context
                .builder(font)
                .size(font_size)
                .normalized_coords(coords)
                .features(font_features.iter())
                .build();
            shaper.add_str(content);
            shaper.shape_with(|cluster| {
                let advance = cluster.advance();
                let mut pen = 0.0;
                for glyph in cluster.glyphs {
                    shaped_glyphs.push((glyph.id, advance, pen + glyph.x, glyph.y));
                    pen += glyph.advance;
                }
            });
            Ok(shaped_glyphs)
        })?;
        for (glyph_id, cluster_advance, x_offset, y_offset) in shaped_glyphs {
            let key = GlyphKey {
                face: face_index,
                glyph: glyph_id,
            };
            cache
                .entry(key)
                .or_insert(snapshot_glyph(faces, face_index, glyph_id, font_size)?);
            let cluster_advance = if face_index == SNAPSHOT_EMOJI {
                f32::from(
                    i16::try_from(snapshot_color_advance(
                        faces, face_index, glyph_id, font_size,
                    )?)
                    .context("color glyph advance fits i16")?,
                )
            } else {
                cluster_advance.trunc()
            };
            glyphs.push(SnapshotGlyph {
                key,
                column,
                row: row_number,
                cells,
                cluster_advance,
                x_offset,
                y_offset,
                foreground,
            });
        }
    }
    glyphs[row_glyph_start..].sort_by_key(|glyph| glyph.column);
    Ok(())
}

/// Source cells select grid pens; advances only place glyphs within a cluster.
/// Failed run shaping leaves all of that run to the existing per-cell fallback.
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one bounded ASCII run transaction retains explicit renderer inputs"
)]
fn prepare_ascii_runs(
    snapshot: &TerminalSnapshot,
    row_index: usize,
    faces: &[FontFace],
    font_size: f32,
    mode: FontLigatures,
    features: &FeatureSettings,
    context: &mut ShapeContext,
    glyphs: &mut Vec<SnapshotGlyph>,
    cache: &mut HashMap<GlyphKey, Arc<CachedGlyph>>,
    default_foreground: [u8; 3],
    default_background: [u8; 3],
) -> Result<Vec<bool>> {
    use super::shaping::{LogicalCell, shape_run};
    let row = &snapshot.visible_rows[row_index];
    let cells = &row.cells[..row.cells.len().min(snapshot.columns)];
    let eligible_faces: Vec<_> = cells
        .iter()
        .enumerate()
        .map(|(column, cell)| {
            (cell.content.len() == 1
                && (b' '..=b'~').contains(&cell.content.as_bytes()[0])
                && cell.spacer_remaining.is_none()
                && leader_span(cells, column) == 1
                && !cell.attributes.conceal)
                .then(|| select_face_for_text(faces, &cell.content, &cell.attributes).ok())
                .flatten()
        })
        .collect();
    let cursor_column = (mode == FontLigatures::Cursor)
        .then(|| {
            snapshot_cursor(
                snapshot,
                u32::try_from(snapshot.columns).expect("bounded columns"),
                u32::try_from(snapshot.rows).expect("bounded rows"),
            )
        })
        .flatten()
        .filter(|(_, row)| *row as usize == row_index)
        .map(|(column, _)| column as usize);
    let mut joined = vec![false; snapshot.columns];
    let mut start = 0;
    while start < cells.len() {
        let Some(face) = eligible_faces[start] else {
            start += 1;
            continue;
        };
        let mut end = start + 1;
        while end < cells.len()
            && eligible_faces[end] == Some(face)
            && cells[end].attributes == cells[start].attributes
            && cursor_column != Some(end)
            && cursor_column != Some(end - 1)
        {
            end += 1;
        }
        let input: Vec<_> = (start..end)
            .map(|column| LogicalCell {
                text: &cells[column].content,
                column,
                width: 1,
            })
            .collect();
        let run = with_font_ref(&faces[face], |font, coords| {
            shape_run(
                context,
                font,
                coords,
                &input,
                snapshot.columns,
                font_size,
                features,
            )
        });
        if let Ok(run) = run
            && run
                .clusters
                .iter()
                .flat_map(|cluster| &cluster.glyphs)
                .all(|glyph| {
                    glyph.id != 0
                        && glyph.x.is_finite()
                        && glyph.y.is_finite()
                        && glyph.advance.is_finite()
                })
        {
            let foreground = rendition_colors(
                &cells[start].attributes,
                &snapshot.palette,
                default_foreground,
                default_background,
            )
            .0;
            for cluster in run.clusters {
                let advance = cluster
                    .glyphs
                    .iter()
                    .map(|glyph| glyph.advance)
                    .sum::<f32>();
                let mut pen = 0.0;
                for glyph in cluster.glyphs {
                    let key = GlyphKey {
                        face,
                        glyph: glyph.id,
                    };
                    if let std::collections::hash_map::Entry::Vacant(entry) = cache.entry(key) {
                        entry.insert(snapshot_glyph(faces, face, glyph.id, font_size)?);
                    }
                    // Spaces remain shaping context, but empty masks need no placement or repaint.
                    if cache[&key].width != 0 && cache[&key].height != 0 {
                        glyphs.push(SnapshotGlyph {
                            key,
                            column: u32::try_from(cluster.source.columns.start)
                                .expect("bounded column"),
                            row: u32::try_from(row_index).expect("bounded row"),
                            cells: u32::try_from(cluster.source.columns.len())
                                .expect("bounded span"),
                            cluster_advance: advance,
                            x_offset: pen + glyph.x,
                            y_offset: glyph.y,
                            foreground,
                        });
                    }
                    pen += glyph.advance;
                }
            }
            joined[start..end].fill(true);
        }
        start = end;
    }
    Ok(joined)
}

pub(super) fn cell_is_renderable(cell: &splinterm_protocol::TerminalCell) -> bool {
    !cell.content.is_empty()
        && !cell.content.bytes().all(|byte| byte == b' ')
        && cell.spacer_remaining.is_none()
        && !cell.attributes.conceal
}

pub(super) fn leader_span(cells: &[splinterm_protocol::TerminalCell], leader: usize) -> u32 {
    let mut span = 1_u32;
    for following in cells.iter().skip(leader + 1) {
        if following
            .spacer_remaining
            .is_none_or(|remaining| remaining == 0)
        {
            break;
        }
        span = span.saturating_add(1);
    }
    span
}

pub(super) fn select_face_for_text(
    faces: &[FontFace],
    text: &str,
    attributes: &CellAttributes,
) -> Result<usize> {
    let primary = primary_face_index(attributes);
    [primary, SNAPSHOT_CJK, SNAPSHOT_EMOJI]
        .into_iter()
        .chain(SNAPSHOT_FALLBACK_START..faces.len())
        .find(|index| {
            let face = &faces[*index];
            face.covers_text(text)
                && with_font_ref(face, |font, _coords| {
                    Ok(text
                        .chars()
                        .all(|character| font.charmap().map(character) != 0))
                })
                .unwrap_or(false)
        })
        .with_context(|| format!("no Fontconfig face covers snapshot cell {text:?}"))
}

pub(super) fn primary_face_index(attributes: &CellAttributes) -> usize {
    match (attributes.bold, attributes.italic) {
        (false, false) => SNAPSHOT_PRIMARY_REGULAR,
        (true, false) => SNAPSHOT_PRIMARY_BOLD,
        (false, true) => SNAPSHOT_PRIMARY_ITALIC,
        (true, true) => SNAPSHOT_PRIMARY_BOLD_ITALIC,
    }
}

pub(super) fn packed_rgb(value: u32) -> [u8; 3] {
    [
        u8::try_from((value >> 16) & 0xff).expect("red fits"),
        u8::try_from((value >> 8) & 0xff).expect("green fits"),
        u8::try_from(value & 0xff).expect("blue fits"),
    ]
}

#[cfg(test)]
pub(super) fn default_foreground() -> [u8; 3] {
    [0xeb, 0xeb, 0xeb]
}
#[cfg(test)]
pub(super) fn default_background() -> [u8; 3] {
    [0x0e, 0x12, 0x16]
}

pub(super) fn wire_color(
    source: ColorSource,
    value: u32,
    palette: &[u32],
    default: [u8; 3],
) -> [u8; 3] {
    match source {
        ColorSource::Default => default,
        ColorSource::Base16 | ColorSource::Base256 => usize::try_from(value)
            .ok()
            .and_then(|index| palette.get(index))
            .copied()
            .map_or(default, packed_rgb),
        ColorSource::Rgb => packed_rgb(value),
    }
}

pub(super) fn rendition_colors(
    attributes: &CellAttributes,
    palette: &[u32],
    default_foreground: [u8; 3],
    default_background: [u8; 3],
) -> ([u8; 3], [u8; 3]) {
    let mut foreground = wire_color(
        attributes.foreground_source,
        attributes.foreground,
        palette,
        default_foreground,
    );
    let mut background = wire_color(
        attributes.background_source,
        attributes.background,
        palette,
        default_background,
    );
    if attributes.reverse {
        std::mem::swap(&mut foreground, &mut background);
    }
    if attributes.dim {
        for component in &mut foreground {
            *component = u8::try_from(u16::from(*component) * 2 / 3).expect("dimmed color fits u8");
        }
    }
    (foreground, background)
}
