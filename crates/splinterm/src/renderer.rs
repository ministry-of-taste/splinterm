//! Deterministic CPU text-row rasterization for the native client.
//!
//! Font selection, cell placement, fallback, and CPU blending are compared against
//! Foot 1.27.0 `fonts.c` and `render.c` at commit
//! `3c5b584b0eafa772eb4376fb6eaf6643399e190e`. It also owns the persistent,
//! scale-keyed glyph cache and damage-oriented terminal snapshot painter.

#[cfg(test)]
use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::Path,
    sync::Arc,
};

#[cfg(test)]
use crate::config::CursorStyle;
use crate::geometry::OutputDpiObservation;
#[cfg(test)]
use crate::geometry::{Rect, TerminalPadding};
use anyhow::Result;

#[cfg(test)]
use splinterm_automation_client::ImageContentSource;
#[cfg(test)]
use splinterm_core::SplintId;
#[cfg(test)]
use splinterm_protocol::{
    ActiveScreen, CellAttributes, ColorSource, ImageContentMetadata, ImagePlacement, MAX_COLUMNS,
    MAX_ROWS, ScrollDirection, TerminalCell, TerminalInputModes, TerminalRow, TerminalScroll,
    TerminalSnapshot, UnderlineStyle,
};
#[cfg(test)]
use swash::scale::image::Content;
mod capture;
mod compose;
mod cursor;
mod decorations;
mod evidence;
mod fonts;
mod frame;
mod icons;
mod images;
mod overlays;
mod raster;
mod settings;
mod shaping;
mod text;

pub(crate) use crate::frontend::PickerHitTarget;
#[cfg(test)]
use capture::capture_prepared_frame;
pub use capture::{
    FinalBufferCapture, capture_final_buffer, capture_final_buffer_in_context,
    capture_final_buffer_presented, capture_final_buffer_presented_in_context,
    capture_final_buffer_sized, capture_final_buffer_sized_presented,
    capture_final_buffer_with_sources,
};
#[cfg(test)]
use compose::{paint_glyphs, paint_placed_glyph};
pub(crate) use compose::{
    paint_snapshot, paint_snapshot_presented, paint_snapshot_region_presented, paint_snapshot_rows,
    paint_snapshot_rows_presented, scroll_snapshot_pixels, snapshot_row_rect,
};
#[cfg(test)]
use cursor::paint_effective_cursor;
pub use cursor::{
    CursorPresentation, EffectiveCursorShape, UnfocusedCursorStyle, effective_cursor_shape,
};
use decorations::DecorationMetrics;
#[cfg(test)]
use decorations::DecorationSpan;
pub use evidence::{benchmark_json, phase4_benchmark_json, write_ppm};
pub use fonts::*;
pub use fonts::{ascii_glyph_evidence, production_ascii_glyph_evidence, snapshot_cache_metrics};
pub(crate) use frame::SnapshotFrame;
use frame::{SnapshotGlyph, packed_rgb};
#[cfg(test)]
use frame::{
    cell_is_renderable, default_background, default_foreground, leader_span, primary_face_index,
    rendition_colors,
};
pub(crate) use icons::{ChromeIcon, chrome_icons_available};
#[cfg(test)]
use images::{KITTY_BACKGROUND_Z_THRESHOLD, SnapshotImage, compare_snapshot_images, image_tier};
pub(crate) use overlays::actions::{
    CommandPaletteLayout, PaletteBody, PaletteShape, command_palette_hit_test,
    command_palette_layout, command_palette_row_hit_test, paint_command_palette,
};
#[cfg(test)]
pub(crate) use overlays::explorer::lair_explorer_layout;
#[allow(
    unused_imports,
    reason = "preserve the crate-local renderer facade for the inferred layout return type"
)]
pub(crate) use overlays::explorer::{
    LairExplorerAction, LairExplorerLayout, LairExplorerMetrics, LairExplorerPresentationMode,
    explorer_minimum_docked_terminal_width, lair_explorer_action_hit_test,
    lair_explorer_agent_hit_test, lair_explorer_agents_label_hit_test,
    lair_explorer_chrome_cell_width, lair_explorer_disclosure_hit_test,
    lair_explorer_filter_hit_test, lair_explorer_footer_hit_test, lair_explorer_hit_test,
    lair_explorer_palette, lair_explorer_shown_layout, lair_explorer_shows_working,
    lair_explorer_with_agents, paint_lair_explorer,
};
pub(crate) use overlays::handoff::paint_handoff_banner;
pub(crate) use overlays::history::{
    HistoryOverlayStatus, SnapshotOverlays, history_overlay_layout, paint_history_overlay,
    paint_snapshot_overlays,
};
pub(crate) use overlays::menu_box::{
    TabContextMenuLayout, paint_tab_context_menu, tab_context_menu_hit_test,
    tab_context_menu_layout, tab_menu_content_width,
};
#[cfg(test)]
pub(crate) use overlays::picker::session_picker_palette;
pub(crate) use overlays::picker::{
    ChromeSurface, SessionPickerPurpose, SessionPickerTextCache, chrome_panel_fill,
    chrome_panel_palette, paint_search_overlay, search_overlay_panel,
};
pub(crate) use overlays::picker_box::{
    paint_session_picker, picker_status_line, session_picker_layout,
};
pub(crate) use overlays::prompt_box::{
    DojoPromptLayout, dojo_prompt_hit_test, dojo_prompt_layout, dojo_prompt_metrics,
    paint_dojo_prompt,
};
#[cfg(test)]
use raster::pixel_index;
use raster::{alpha_u8, blend_glyph, blend_rect, premultiplied_rgba};
pub(crate) use raster::{background_bgra, fill_rect, paint_box_drawing_cell};
use settings::{
    BASE_FONT_SIZE, PRIMARY_FONT, compatibility_render_context, effective_font_size,
    renderer_options,
};
pub use settings::{
    RenderContext, RendererOptions, RendererResources, configure, effective_font_resolution,
};
pub(crate) use text::{
    ChromeSource, ChromeText, ChromeTextStyle, chrome_maximum_cell_height,
    chrome_primary_faces_cover,
};

pub(crate) fn premultiplied_theme_rgba(color: u32, alpha: u16) -> [u8; 4] {
    premultiplied_rgba(
        [
            ((color >> 16) & 0xff) as u8,
            ((color >> 8) & 0xff) as u8,
            (color & 0xff) as u8,
        ],
        alpha_u8(alpha),
    )
}

impl RenderContext {
    /// The cell geometry and effective pixel size this context would draw
    /// with at `steps` of runtime zoom and `surface_scale_120`, or `None`
    /// when the renderer refuses that zoom. Changes nothing: a trusted
    /// dialog measures candidate fonts with it before it picks one.
    ///
    /// # Errors
    /// Returns an error for invalid scale, DPI, or font state.
    pub(crate) fn zoomed_cell_geometry(
        &self,
        steps: i16,
        surface_scale_120: u32,
    ) -> Result<Option<(crate::geometry::CellGeometry, f32)>> {
        let mut candidate = self.clone();
        if candidate
            .apply_font_zoom_steps(steps, surface_scale_120)?
            .is_none()
        {
            return Ok(None);
        }
        let size = candidate.effective_font_size(surface_scale_120)?;
        let generation = candidate.font_generation()?;
        let face = generation
            .faces
            .first()
            .ok_or_else(|| anyhow::anyhow!("font generation has no primary face"))?;
        let metrics = fonts::cell_metrics(face, size)?;
        let cell = crate::geometry::CellGeometry::from_metrics(
            metrics.width,
            metrics.height,
            metrics.ascent,
            metrics.descent,
            u32::try_from(metrics.baseline)
                .map_err(|_| anyhow::anyhow!("cell baseline is negative"))?,
        )?;
        Ok(Some((cell, size)))
    }

    /// The padding this context places around the terminal grid.
    pub(crate) fn terminal_padding(&self) -> crate::geometry::TerminalPadding {
        self.padding()
    }

    pub(crate) fn set_font_zoom_steps(
        &mut self,
        steps: i16,
        surface_scale_120: u32,
    ) -> Result<Option<bool>> {
        let changed = self.apply_font_zoom_steps(steps, surface_scale_120)?;
        if changed.is_some() {
            clear_snapshot_caches();
        }
        Ok(changed)
    }

    /// Updates this context's output observation and invalidates size-dependent caches.
    ///
    /// # Errors
    /// Returns an error for invalid scale, DPI, font resolution, or cache state.
    pub fn update_output_dpi(
        &mut self,
        observation: OutputDpiObservation,
        surface_scale_120: u32,
    ) -> Result<bool> {
        let changed = self.apply_output_dpi(observation, surface_scale_120)?;
        if changed {
            clear_snapshot_caches();
        }
        Ok(changed)
    }
}

/// Updates the active output DPI and clears font caches when its raster size changes.
///
/// # Errors
/// Returns an error for invalid scale/DPI/font resolution or poisoned renderer state.
pub fn update_output_dpi(
    observation: OutputDpiObservation,
    surface_scale_120: u32,
) -> Result<bool> {
    let changed = settings::update_output_dpi(observation, surface_scale_120)?;
    if changed {
        clear_snapshot_caches();
    }
    Ok(changed)
}

#[cfg(test)]
mod tests;
