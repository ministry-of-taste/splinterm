//! Trusted consent and pane chrome painting.

use std::{collections::HashMap, time::Instant};

use anyhow::{Context, Result};
use unicode_width::UnicodeWidthChar;

use crate::{
    color::{
        NON_TEXT_MIN_CONTRAST, contrast_corrected, contrast_floor_toward, contrast_ratio,
        mix_encoded, mix_linear, rgb_channels, strongest_contrast_endpoint,
    },
    config::ResolvedTheme,
    frontend::{
        DialogFrame, DialogText, TrustedInput, dialog_font_step,
        dialog_text::{
            DialogInk, NATURAL_MAX_TEXT_COLUMNS, SELECTION_SLOT, chrome_rows, natural_columns,
            natural_rows, text_columns, text_rows,
        },
    },
    geometry::{CellGeometry, Rect, TerminalPadding, WindowGeometry, logical_extent_to_buffer},
    pane::{PaneChrome, PaneDivider, PaneLayout},
    renderer::{ChromeText, background_bgra, paint_box_drawing_cell},
};
use splinterm_core::SplintId;
use splinterm_protocol::{MAX_COLUMNS, MAX_ROWS, TerminalRow, TerminalSnapshot};
use wayland_client::QueueHandle;

use super::{
    App, CachedFrameTitle, SCALE_DENOMINATOR, apply_theme, rebuild_pane_scaled_frame_with_context,
};

/// The most runtime zoom steps (half a point each) a trusted dialog tries
/// below its configured font. The floor (`DIALOG_FONT_FLOOR_PERCENT`)
/// normally ends the search well before this.
const MAX_DIALOG_FONT_STEPS: i16 = 48;

/// What a trusted dialog's layout depends on. While it stays the same, the
/// laid-out text is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TrustedLayoutKey {
    content: (u32, u32, u32, u32),
    scale_120: u32,
    renderer_generation: u64,
    bounds: Option<(u32, u32)>,
}

/// A trusted dialog's fixed size and the inputs of its current layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct TrustedDialogLayout {
    key: Option<TrustedLayoutKey>,
    /// The logical size asked for as both minimum and maximum.
    pub(super) fixed_size: Option<(u32, u32)>,
    /// The largest size the compositor suggested for the Window
    /// (`xdg_toplevel.configure_bounds`), when it sent one.
    pub(super) bounds: Option<(u32, u32)>,
}

impl TrustedDialogLayout {
    pub(super) const fn new(fixed_size: Option<(u32, u32)>) -> Self {
        Self {
            key: None,
            fixed_size,
            bounds: None,
        }
    }
}

/// The fixed size a dialog asks for: its `natural` size, but no larger than
/// the compositor's suggested `bounds` (usually the output's usable area),
/// so a long prompt never asks to be taller than the screen; its text then
/// wraps or shrinks to fit. A bound of 0 means that dimension is unbounded.
pub(super) fn bounded_fixed_size(natural: (u32, u32), bounds: Option<(u32, u32)>) -> (u32, u32) {
    let Some((width, height)) = bounds else {
        return natural;
    };
    let bound = |size: u32, bound: u32| if bound == 0 { size } else { size.min(bound) };
    (bound(natural.0, width), bound(natural.1, height))
}

/// How a trusted dialog's text fits one candidate cell size in its Window.
#[derive(Clone, Copy, Debug)]
struct TrustedFit {
    geometry: WindowGeometry,
    /// The columns the text wraps at: those that fit, at most its natural
    /// width.
    wrap_columns: usize,
    /// The rows the text takes at `wrap_columns`.
    needed_rows: usize,
    /// The most body rows the box leaves room for.
    room_rows: usize,
    /// Whether every selection's button labels fit their buttons.
    buttons_fit: bool,
}

impl TrustedFit {
    const fn fits(&self) -> bool {
        self.needed_rows <= self.room_rows && self.buttons_fit
    }

    const fn frame(&self) -> DialogFrame {
        DialogFrame {
            wrap_columns: self.wrap_columns,
            grid_columns: self.geometry.columns as usize,
            grid_rows: self.geometry.rows as usize,
            max_text_rows: self.room_rows,
        }
    }
}

/// How `variants` fit a Window whose `content` rectangle (logical pixels)
/// holds a grid of `cell` sized cells, exactly as the Window draws it: the
/// same grid fit and the same translation. The body takes the box's
/// [`chrome_rows`] room, and the button labels the columns of their halves.
/// `None` when the content cannot hold the protocol minimum grid.
fn trusted_dialog_fit(
    content: Rect,
    cell: CellGeometry,
    padding: TerminalPadding,
    scale_120: u32,
    variants: &[DialogText],
) -> Option<TrustedFit> {
    let geometry = WindowGeometry::fit_window(
        content.width,
        content.height,
        cell,
        padding,
        scale_120,
        2,
        u32::from(MAX_COLUMNS),
        2,
        u32::from(MAX_ROWS),
    )
    .ok()?
    .translated(
        logical_extent_to_buffer(content.x, scale_120).ok()?,
        logical_extent_to_buffer(content.y, scale_120).ok()?,
    )
    .ok()?;
    let columns = usize::try_from(geometry.columns).ok()?;
    let wrap_columns = text_columns(columns).min(NATURAL_MAX_TEXT_COLUMNS);
    Some(TrustedFit {
        geometry,
        wrap_columns,
        needed_rows: text_rows(variants, wrap_columns),
        room_rows: chrome_rows(usize::try_from(geometry.rows).ok()?).body_room,
        buttons_fit: variants.iter().all(|variant| variant.buttons_fit(columns)),
    })
}

/// Logical pixels `pixels` at `scale_120`, in buffer pixels, rounded, and
/// at least `minimum`.
fn scaled_pixels(pixels: u32, scale_120: u32, minimum: u32) -> u32 {
    (pixels
        .saturating_mul(scale_120)
        .saturating_add(SCALE_DENOMINATOR / 2)
        / SCALE_DENOMINATOR)
        .max(minimum)
}

/// The frame's thickness, in logical pixels.
const FRAME_LINE_PIXELS: u32 = 2;

/// Each button's edge, in logical pixels.
const BUTTON_EDGE_PIXELS: u32 = 1;

/// How tall each button is, in logical pixels, within 2½ and 3 rows.
const BUTTON_HEIGHT_PIXELS: u32 = 56;

/// The gap between the two buttons, in logical pixels.
const BUTTON_GAP_PIXELS: u32 = 6;

/// How far the box's hard shadow falls below and right of it, in logical
/// pixels: the in-Window popups' offset.
const SHADOW_OFFSET_PIXELS: u32 = 6;

/// How dark the hard shadow is: this much black over the background, as
/// the in-Window popups' shadow is.
const SHADOW_DARKNESS: f64 = 0.35;

/// Where a trusted dialog's box goes in its buffer, in buffer pixels: the
/// frame, the openings in its edges for the title and the legend, and the
/// two buttons. Drawing ([`paint_trusted_consent_chrome`]) and hit testing
/// ([`App::trusted_consent_action_area_at`]) both take it from
/// [`trusted_chrome`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TrustedChrome {
    /// The frame's outer edge; its lines run `line` pixels inside it.
    frame: Rect,
    line: u32,
    /// Spans `(x, end)` of the top and the bottom line left open for the
    /// title and the legend, left to right.
    top_openings: Vec<(u32, u32)>,
    bottom_openings: Vec<(u32, u32)>,
    /// The red left (no) and green right (yes) buttons, edges included.
    deny: Rect,
    allow: Rect,
    edge: u32,
    /// The hard shadow: the bands, right of and below the grid, where the
    /// grid shifted down and right by the offset shows. Nothing is drawn
    /// there but the shadow; the Window's padding holds it.
    shadow: Vec<Rect>,
}

/// Whether a click `y` logical pixels down a Window at `scale_120` is on
/// the rows of `chrome`'s buttons, which share their rows.
fn buttons_hit(y: f64, scale_120: u32, chrome: &TrustedChrome) -> bool {
    let y = y * f64::from(scale_120) / f64::from(SCALE_DENOMINATOR);
    let top = chrome.deny.y;
    y >= f64::from(top) && y < f64::from(top.saturating_add(chrome.deny.height))
}

/// The openings, as `(x, end)` buffer spans, a frame line through `row`
/// leaves for its text: every column within one of a visible character.
fn frame_openings(row: Option<&TerminalRow>, geometry: &WindowGeometry) -> Vec<(u32, u32)> {
    let Some(row) = row else {
        return Vec::new();
    };
    let width = geometry.cell.width;
    let grid = geometry.grid_rect;
    let shown: Vec<bool> = row
        .cells
        .iter()
        .map(|cell| !cell.content.trim().is_empty())
        .collect();
    let mut openings: Vec<(u32, u32)> = Vec::new();
    for column in 0..shown.len() {
        let near = column.saturating_sub(1)..=(column + 1).min(shown.len() - 1);
        if !shown[near].iter().any(|visible| *visible) {
            continue;
        }
        let Ok(column) = u32::try_from(column) else {
            break;
        };
        let x = grid.x.saturating_add(column.saturating_mul(width));
        let end = x.saturating_add(width);
        match openings.last_mut() {
            Some((_, last)) if *last == x => *last = end,
            _ => openings.push((x, end)),
        }
    }
    openings
}

/// The hard shadow of `geometry`'s grid in a `width` by `height` buffer at
/// `scale_120`: the band right of the grid and the band below it, where
/// the grid moved down and right by the shadow's offset would show, cut to
/// the buffer.
///
/// It falls outside the whole grid, not just outside the frame, so the
/// title, the legend and the frame, which sit in the grid's edge cells,
/// never stand on it. The Window's padding, wider than the offset at every
/// scale, holds it, so the natural size already makes room for it.
fn shadow_bands(geometry: &WindowGeometry, scale_120: u32, width: u32, height: u32) -> Vec<Rect> {
    let offset = SHADOW_OFFSET_PIXELS
        .saturating_mul(scale_120)
        .div_ceil(SCALE_DENOMINATOR);
    let grid = geometry.grid_rect;
    let right = grid
        .x
        .saturating_add(geometry.columns.saturating_mul(geometry.cell.width));
    let bottom = grid
        .y
        .saturating_add(geometry.rows.saturating_mul(geometry.cell.height));
    let clip = |x: u32, y: u32, end_x: u32, end_y: u32| {
        let (end_x, end_y) = (end_x.min(width), end_y.min(height));
        Rect {
            x,
            y,
            width: end_x.saturating_sub(x),
            height: end_y.saturating_sub(y),
        }
    };
    [
        clip(
            right,
            grid.y.saturating_add(offset),
            right.saturating_add(offset),
            bottom.saturating_add(offset),
        ),
        clip(
            grid.x.saturating_add(offset),
            bottom,
            right,
            bottom.saturating_add(offset),
        ),
    ]
    .into_iter()
    .filter(|band| band.width > 0 && band.height > 0)
    .collect()
}

/// The red (left) and green (right) buttons between `left` and `right` and
/// from `top` to `bottom`, split at `middle`, where the pointer's halves
/// meet: each keeps clear of it by half of `gap`.
fn button_pair(
    middle: u32,
    gap: u32,
    (left, right): (u32, u32),
    (top, bottom): (u32, u32),
) -> (Rect, Rect) {
    let deny_end = middle.saturating_sub(gap / 2);
    let allow_x = middle.saturating_add(gap - gap / 2);
    let height = bottom.saturating_sub(top);
    (
        Rect {
            x: left,
            y: top,
            width: deny_end.saturating_sub(left),
            height,
        },
        Rect {
            x: allow_x,
            y: top,
            width: right.saturating_sub(allow_x),
            height,
        },
    )
}

/// The box of a trusted dialog drawn in a `width` by `height` buffer at
/// `scale_120`, whose grid is `geometry` and whose snapshot rows are
/// `rows`.
///
/// The frame runs through the middle of the grid's first and last rows and
/// columns, open where the title (first row) and the legend (last row)
/// show. The buttons span the box inside the frame, split at the middle of
/// the buffer, as the pointer's halves are, and take the label and keys
/// rows ([`chrome_rows`]) with a little room above and below, never
/// reaching the body above or the legend below. Without a grid the buttons
/// take the bottom fifth inside a frame at the buffer's edge.
pub(super) fn trusted_chrome(
    width: u32,
    height: u32,
    scale_120: u32,
    geometry: Option<&WindowGeometry>,
    rows: &[TerminalRow],
) -> TrustedChrome {
    let line = scaled_pixels(FRAME_LINE_PIXELS, scale_120, 2);
    let edge = scaled_pixels(BUTTON_EDGE_PIXELS, scale_120, 1);
    let gap = scaled_pixels(BUTTON_GAP_PIXELS, scale_120, 4);
    let buttons =
        |gap, left, right, top, bottom| button_pair(width / 2, gap, (left, right), (top, bottom));
    let Some(geometry) = geometry else {
        let inset = line.saturating_mul(4);
        let (deny, allow) = buttons(
            gap,
            inset,
            width.saturating_sub(inset),
            height.saturating_mul(78) / 100,
            height.saturating_sub(inset),
        );
        return TrustedChrome {
            frame: Rect {
                x: line,
                y: line,
                width: width.saturating_sub(2 * line),
                height: height.saturating_sub(2 * line),
            },
            line,
            top_openings: Vec::new(),
            bottom_openings: Vec::new(),
            deny,
            allow,
            edge,
            shadow: Vec::new(),
        };
    };
    let (cell_width, cell_height) = (geometry.cell.width.max(1), geometry.cell.height.max(1));
    let grid = geometry.grid_rect;
    let last_column = geometry.columns.saturating_sub(1);
    let last_row = geometry.rows.saturating_sub(1);
    let middle_x = |column: u32| {
        grid.x
            .saturating_add(column.saturating_mul(cell_width))
            .saturating_add(cell_width / 2)
    };
    let middle_y = |row: u32| {
        grid.y
            .saturating_add(row.saturating_mul(cell_height))
            .saturating_add(cell_height / 2)
    };
    let left = middle_x(0).saturating_sub(line / 2);
    let top = middle_y(0).saturating_sub(line / 2);
    let right = middle_x(last_column)
        .saturating_add(line - line / 2)
        .min(width);
    let bottom = middle_y(last_row)
        .saturating_add(line - line / 2)
        .min(height);
    let places = chrome_rows(usize::try_from(geometry.rows).unwrap_or(0));
    let row_top = |row: usize| {
        grid.y.saturating_add(
            u32::try_from(row)
                .unwrap_or(u32::MAX)
                .saturating_mul(cell_height),
        )
    };
    // Two rows of text with room around them, never more than half a row
    // above or below: the body ends a row above, the legend a row below.
    let button_height = scaled_pixels(BUTTON_HEIGHT_PIXELS, scale_120, 0)
        .clamp(cell_height * 2 + cell_height / 2, cell_height * 3);
    let room = (button_height - cell_height * 2) / 2;
    // Never more than half a cell, so the labels, which keep a column clear
    // of the middle ([`dialog_text::button_columns`]), stay inside their
    // buttons even where the grid sits a little off the buffer's middle.
    let (deny, allow) = buttons(
        gap.min(cell_width / 2).max(2),
        middle_x(1),
        middle_x(last_column.saturating_sub(1)),
        row_top(places.label).saturating_sub(room),
        row_top(places.keys + 1).saturating_add(room).min(height),
    );
    TrustedChrome {
        frame: Rect {
            x: left,
            y: top,
            width: right.saturating_sub(left),
            height: bottom.saturating_sub(top),
        },
        line,
        top_openings: frame_openings(rows.first(), geometry),
        bottom_openings: frame_openings(
            rows.get(usize::try_from(last_row).unwrap_or(usize::MAX)),
            geometry,
        ),
        deny,
        allow,
        edge,
        shadow: shadow_bands(geometry, scale_120, width, height),
    }
}

impl App {
    /// This trusted dialog's box ([`trusted_chrome`]) in a `width` by
    /// `height` buffer that shows `geometry`'s grid: the frame, and the
    /// buttons the Window draws and answers clicks on.
    pub(super) fn trusted_consent_action_top(
        &self,
        width: u32,
        height: u32,
        geometry: Option<&WindowGeometry>,
    ) -> TrustedChrome {
        trusted_chrome(
            width,
            height,
            self.surface.scale_120,
            geometry,
            self.panes
                .pane
                .snapshot
                .as_ref()
                .map_or(&[], |snapshot| snapshot.visible_rows.as_slice()),
        )
    }

    /// Whether a click `y` logical pixels down this trusted dialog falls on
    /// the rows of its buttons, exactly where
    /// [`paint_trusted_consent_chrome`] paints them. The pointer's side of
    /// the Window's middle picks the button.
    /// How long until a trusted dialog's countdown needs drawing again;
    /// `None` without a dialog that counts down.
    pub(super) fn trusted_countdown_timeout(&self) -> Option<std::time::Duration> {
        self.modal
            .trusted_consent
            .as_ref()?
            .countdown_wait(Instant::now())
    }

    /// Draws a trusted dialog again when the time left it shows has
    /// changed. Display only: nothing answers when the time runs out.
    pub(super) fn tick_trusted_countdown(&mut self, queue_handle: &QueueHandle<Self>) {
        if self
            .modal
            .trusted_consent
            .as_ref()
            .is_some_and(|consent| consent.countdown_stale(Instant::now()))
        {
            self.trusted_dialog_input(Some(TrustedInput::Redraw), queue_handle);
        }
    }

    pub(super) fn trusted_consent_action_area_at(&self, y: f64) -> bool {
        let scale_120 = self.surface.scale_120;
        let content = self.content_rect();
        let geometry = self.panes.pane.snapshot_frame.as_ref().and_then(|frame| {
            self.panes
                .pane
                .window_geometry(frame, content.width, content.height, scale_120)
                .ok()?
                .translated(
                    logical_extent_to_buffer(content.x, scale_120).ok()?,
                    logical_extent_to_buffer(content.y, scale_120).ok()?,
                )
                .ok()
        });
        let (width, height) = geometry.as_ref().map_or_else(
            || {
                (
                    logical_extent_to_buffer(self.surface.logical_width, scale_120).unwrap_or(0),
                    logical_extent_to_buffer(self.surface.logical_height, scale_120).unwrap_or(0),
                )
            },
            |geometry| (geometry.buffer_width(), geometry.buffer_height()),
        );
        buttons_hit(
            y,
            scale_120,
            &self.trusted_consent_action_top(width, height, geometry.as_ref()),
        )
    }

    /// Lays a trusted dialog's text out again whenever its Window's size,
    /// scale, or font changed since the last layout, before it is drawn.
    ///
    /// The dialog asks for one fixed size, the natural size of its text at
    /// the configured font, as both its minimum and maximum size; this keeps
    /// that request right at the current scale. When the compositor gives
    /// another size anyway (a tiling compositor that ignores the request),
    /// the text wraps to the columns that fit and, when it still needs more
    /// rows than the box leaves it or a button label is wider than its
    /// button, the font steps down to no less than
    /// `DIALOG_FONT_FLOOR_PERCENT` of the configured size. Only below that
    /// are rows dropped, hint rows last (`dialog_text::layout`).
    pub(super) fn relayout_trusted_dialog(&mut self) -> Result<()> {
        if !self.surface.configured || self.modal.trusted_consent.is_none() {
            return Ok(());
        }
        let content = self.content_rect();
        let scale_120 = self.surface.scale_120;
        let bounds = self.modal.trusted_layout.bounds;
        let key = TrustedLayoutKey {
            content: (content.x, content.y, content.width, content.height),
            scale_120,
            renderer_generation: self.presentation.renderer_generation,
            bounds,
        };
        if self.modal.trusted_layout.key == Some(key) {
            return Ok(());
        }
        let Some(variants) = self
            .modal
            .trusted_consent
            .as_ref()
            .map(crate::frontend::TrustedConsentUi::variants)
            .filter(|variants| !variants.is_empty())
        else {
            return Ok(());
        };
        let render_context = &self.presentation.render_context;
        let padding = render_context.terminal_padding();
        let Some((configured_cell, configured_pixels)) =
            render_context.zoomed_cell_geometry(0, scale_120)?
        else {
            return Ok(());
        };
        // The fixed size, at the configured font and the current scale, no
        // larger than the compositor's suggested bounds.
        if let Ok(natural) = WindowGeometry::for_grid(
            u32::try_from(natural_columns(&variants)).unwrap_or(u32::MAX),
            u32::try_from(natural_rows(&variants)).unwrap_or(u32::MAX),
            configured_cell,
            padding,
            scale_120,
        ) {
            let size =
                bounded_fixed_size((natural.logical_width(), natural.logical_height()), bounds);
            if self.modal.trusted_layout.fixed_size != Some(size) {
                self.surface.window.set_min_size(Some(size));
                self.surface.window.set_max_size(Some(size));
                self.modal.trusted_layout.fixed_size = Some(size);
            }
        }
        let step = dialog_font_step(
            configured_pixels,
            (0..=MAX_DIALOG_FONT_STEPS).map_while(|step| {
                let zoom = -step;
                let (cell, pixels) = render_context
                    .zoomed_cell_geometry(zoom, scale_120)
                    .ok()??;
                let fits = trusted_dialog_fit(content, cell, padding, scale_120, &variants)
                    .is_some_and(|fit| fit.fits());
                Some((zoom, pixels, fits))
            }),
        );
        if step.zoom_steps != self.presentation.font_zoom_steps
            && self
                .presentation
                .render_context
                .set_font_zoom_steps(step.zoom_steps, scale_120)?
                .is_some()
        {
            self.presentation.font_zoom_steps = step.zoom_steps;
            self.presentation.renderer_generation =
                self.presentation.renderer_generation.saturating_add(1);
        }
        let fit = self
            .presentation
            .render_context
            .zoomed_cell_geometry(self.presentation.font_zoom_steps, scale_120)?
            .and_then(|(cell, _)| trusted_dialog_fit(content, cell, padding, scale_120, &variants));
        let snapshot = fit.and_then(|fit| {
            self.modal
                .trusted_consent
                .as_mut()
                .and_then(|consent| consent.lay_out_again(fit.frame(), Instant::now()))
        });
        if let Some(snapshot) = snapshot {
            self.install_trusted_dialog_snapshot(snapshot)?;
        }
        self.modal.trusted_layout.key = Some(TrustedLayoutKey {
            renderer_generation: self.presentation.renderer_generation,
            ..key
        });
        Ok(())
    }

    /// Shows a newly laid-out or newly selected trusted dialog snapshot: the
    /// frame is rebuilt from it at once, so drawing and hit testing both use
    /// it.
    pub(super) fn install_trusted_dialog_snapshot(
        &mut self,
        mut snapshot: TerminalSnapshot,
    ) -> Result<()> {
        // The Window's theme already carries the dialog's colors and the
        // palette slots its cells are drawn from ([`trusted_dialog_theme`],
        // applied as the Window is prepared); deriving them again keeps
        // them, and restores them should the theme ever be replaced.
        apply_theme(&mut snapshot, trusted_dialog_theme(self.presentation.theme));
        snapshot.revision = self
            .panes
            .pane
            .snapshot
            .as_ref()
            .map_or(snapshot.revision, |current| {
                current.revision.saturating_add(1)
            });
        self.invalidate_local_content_state();
        self.panes.pane.snapshot = Some(snapshot);
        rebuild_pane_scaled_frame_with_context(
            &mut self.panes.pane,
            self.surface.scale_120,
            &self.presentation.render_context,
        )?;
        self.presentation.full_redraw = true;
        Ok(())
    }
}

/// The colors a trusted dialog paints with, from its Window's theme.
///
/// The dialog's text tells the person to click "the red Deny button
/// (left)" to say no and "the green Allow button (right)" to say yes. Each
/// button is a light tint of its colour with an edge and a label in it, so
/// the edges carry the colour: they must stay recognisably red and green,
/// apart from each other in hue and lightness, and apart from the frame,
/// whatever the theme says (a theme or a `[colors]` key may swap them, and
/// a caller chooses the environment the dialog reads its theme from):
/// - a `danger` that is not red or a `success` that is not green gives way
///   to Splinterm's own red or green, and edges too close to each other
///   give way to Splinterm's own for one of them, then for both;
/// - a frame too close to either edge gives way to Splinterm's own amber,
///   then to black or white;
/// - when even those fail, as on a mid-tone background where the floors
///   leave no hue, the whole dialog takes Splinterm's fixed palette.
///
/// Every non-text color (the frame and both edges) keeps 3:1 against the
/// background, and all text keeps 4.5:1 against what it sits on: the body
/// text, secondary text and title against the background, the selected
/// choice against the selection fill, and each label and its keys against
/// its button's tint. Each is moved toward black or white as needed.
///
/// The hue check is deliberately coarse ([`is_red_hue`], [`is_green_hue`]):
/// a yellow can pass as green and an orange or pink as red. The edges then
/// still differ by at least [`MIN_AREA_HUE_DISTANCE`] in hue and
/// [`MIN_AREA_CONTRAST`] in lightness, and the text names each by its side
/// and its label as well as its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TrustedConsentColors {
    /// The dialog's body, behind its text and around its buttons.
    pub(super) background: u32,
    /// The dialog's text.
    pub(super) text: u32,
    /// The frame: the `warning` role.
    pub(super) border: u32,
    /// The left button's edge, which says no: the `danger` role.
    pub(super) deny: u32,
    /// The right button's edge, which says yes: the `success` role.
    pub(super) allow: u32,
    /// The right button's edge while the dialog cannot say yes (part of its
    /// text is hidden): `allow` faded most of the way into the background.
    pub(super) allow_dimmed: u32,
    /// Secondary text: labels, notes, choices not offered, the legend.
    pub(super) secondary: u32,
    /// The title, in the frame's colour as text.
    pub(super) title: u32,
    /// The fill behind the selected choice.
    pub(super) selection: u32,
    /// The selected choice's text.
    pub(super) selected_text: u32,
    /// The buttons' fills: their edge's colour over the background.
    pub(super) deny_tint: u32,
    pub(super) allow_tint: u32,
    /// The right button's fill while the dialog cannot say yes.
    pub(super) allow_tint_dimmed: u32,
    /// The labels, in their edge's colour as text on their tint.
    pub(super) deny_label: u32,
    pub(super) allow_label: u32,
    /// The keys under the labels, secondary text on their tint.
    pub(super) deny_keys: u32,
    pub(super) allow_keys: u32,
    /// The right button's label and keys while it cannot say yes.
    pub(super) allow_off: u32,
    /// The hard shadow below and right of the box: black over the
    /// background. Nothing is drawn on it, so no contrast is measured
    /// against it.
    pub(super) shadow: u32,
}

/// The theme roles a trusted dialog takes its colors from.
#[derive(Clone, Copy, Debug)]
struct TrustedRoles {
    text: u32,
    muted: u32,
    warning: u32,
    danger: u32,
    success: u32,
    selection: u32,
    accent: u32,
}

/// Splinterm's own red for the deny edge, when the theme's `danger` is not red.
const FALLBACK_DENY: u32 = 0x0070_2020;
/// Splinterm's own green for the allow edge, when the theme's `success` is
/// not green.
const FALLBACK_ALLOW: u32 = 0x0020_7040;
/// Splinterm's own amber for the frame, when the theme's `warning` is too
/// close to an edge.
const FALLBACK_BORDER: u32 = 0x00e0_a030;
/// The fixed palette's background, text, secondary text, selection and
/// accent: the dialog's colors before it followed the theme, and the
/// colors it falls back to.
const FIXED_BACKGROUND: u32 = 0x0014_1820;
const FIXED_TEXT: u32 = 0x00f4_f0e8;
const FIXED_MUTED: u32 = 0x00a0_9a90;
const FIXED_SELECTION: u32 = 0x0028_3040;
const FIXED_ACCENT: u32 = 0x007a_a2f7;
/// How far the dimmed allow edge and fill fade into the background.
const DIMMED_ALLOW_FADE: f64 = 0.75;
/// How much of its edge's colour a button's fill takes over the background.
const BUTTON_TINT: f64 = 0.14;
/// The least contrast between the selection fill and the background, so
/// the selected choice shows as a row and not only by its marker.
const MIN_SELECTION_CONTRAST: f64 = 1.15;
/// The least hue difference, in degrees, between the deny and allow edges.
const MIN_AREA_HUE_DISTANCE: f64 = 60.0;
/// The least contrast between the deny and allow edges, so they differ in
/// lightness as well as hue, for colour-blind eyes.
const MIN_AREA_CONTRAST: f64 = 1.3;
/// The frame is apart from an edge when their hues differ by this much, or
/// their lightness by [`MIN_FRAME_CONTRAST`].
const MIN_FRAME_HUE_DISTANCE: f64 = 30.0;
const MIN_FRAME_CONTRAST: f64 = 1.5;

impl TrustedConsentColors {
    pub(super) fn from_theme(theme: ResolvedTheme) -> Self {
        let roles = theme.roles;
        Self::pick(
            theme.background,
            TrustedRoles {
                text: roles.text,
                muted: roles.text_muted,
                warning: roles.warning,
                danger: roles.danger,
                success: roles.success,
                selection: roles.selection,
                accent: roles.accent,
            },
        )
        .unwrap_or_else(Self::fixed)
    }

    /// Splinterm's fixed palette, floored as any theme's is.
    fn fixed() -> Self {
        let roles = TrustedRoles {
            text: FIXED_TEXT,
            muted: FIXED_MUTED,
            warning: FALLBACK_BORDER,
            danger: FALLBACK_DENY,
            success: FALLBACK_ALLOW,
            selection: FIXED_SELECTION,
            accent: FIXED_ACCENT,
        };
        Self::pick(FIXED_BACKGROUND, roles).unwrap_or_else(|| {
            Self::finish(
                FIXED_BACKGROUND,
                roles,
                FALLBACK_BORDER,
                FALLBACK_DENY,
                FALLBACK_ALLOW,
            )
        })
    }

    /// The colors on `background`, or `None` when the edges cannot be kept
    /// red, green and apart there.
    fn pick(background: u32, roles: TrustedRoles) -> Option<Self> {
        let endpoint = strongest_contrast_endpoint(background);
        let non_text =
            |color| contrast_floor_toward(color, endpoint, background, NON_TEXT_MIN_CONTRAST);
        let hued = |color: u32, hue: fn(f64) -> bool| hue_degrees(color).is_some_and(hue);
        // The role and Splinterm's own color, each floored, that keep `hue`.
        let candidates = |role: u32, fallback: u32, hue: fn(f64) -> bool| {
            [role, fallback]
                .map(non_text)
                .into_iter()
                .filter(move |color| hued(*color, hue))
        };
        let areas = |deny: u32, allow: u32| {
            let (deny, allow) = lightness_apart(deny, allow, endpoint);
            (hued(deny, is_red_hue)
                && hued(allow, is_green_hue)
                && hue_distance(deny, allow).is_some_and(|d| d >= MIN_AREA_HUE_DISTANCE)
                && contrast_ratio(deny, allow) >= MIN_AREA_CONTRAST)
                .then_some((deny, allow))
        };
        // The theme's pair first, then each of its colors with Splinterm's
        // own for the other, then Splinterm's pair.
        let (deny, allow) = candidates(roles.danger, FALLBACK_DENY, is_red_hue)
            .flat_map(|deny| {
                candidates(roles.success, FALLBACK_ALLOW, is_green_hue)
                    .map(move |allow| (deny, allow))
            })
            .find_map(|(deny, allow)| areas(deny, allow))?;
        let border = [roles.warning, FALLBACK_BORDER, endpoint]
            .into_iter()
            .map(non_text)
            .find(|border| frame_apart(*border, deny) && frame_apart(*border, allow))?;
        Some(Self::finish(background, roles, border, deny, allow))
    }

    /// Every color, from the background, the roles, and the frame and edge
    /// colors already chosen: the text floored against what it sits on, and
    /// the fills mixed from the edges.
    fn finish(background: u32, roles: TrustedRoles, border: u32, deny: u32, allow: u32) -> Self {
        let endpoint = strongest_contrast_endpoint(background);
        let selection = contrast_floor_toward(
            roles.selection,
            endpoint,
            background,
            MIN_SELECTION_CONTRAST,
        );
        let secondary = contrast_corrected(roles.muted, background);
        let deny_tint = mix_encoded(background, deny, BUTTON_TINT);
        let allow_tint = mix_encoded(background, allow, BUTTON_TINT);
        let allow_tint_dimmed = mix_linear(allow_tint, background, DIMMED_ALLOW_FADE);
        Self {
            background,
            text: contrast_corrected(roles.text, background),
            border,
            deny,
            allow,
            allow_dimmed: mix_linear(allow, background, DIMMED_ALLOW_FADE),
            secondary,
            title: contrast_corrected(border, background),
            selection,
            selected_text: contrast_corrected(roles.accent, selection),
            deny_tint,
            allow_tint,
            allow_tint_dimmed,
            deny_label: contrast_corrected(deny, deny_tint),
            allow_label: contrast_corrected(allow, allow_tint),
            deny_keys: contrast_corrected(secondary, deny_tint),
            allow_keys: contrast_corrected(secondary, allow_tint),
            allow_off: contrast_corrected(secondary, allow_tint_dimmed),
            shadow: mix_encoded(background, 0, SHADOW_DARKNESS),
        }
    }

    /// The color a dialog cell in `ink` is drawn in.
    pub(super) const fn ink(&self, ink: DialogInk) -> u32 {
        match ink {
            DialogInk::Text => self.text,
            DialogInk::Secondary => self.secondary,
            DialogInk::Title => self.title,
            DialogInk::SelectedText => self.selected_text,
            DialogInk::DenyLabel => self.deny_label,
            DialogInk::AllowLabel => self.allow_label,
            DialogInk::DenyKeys => self.deny_keys,
            DialogInk::AllowKeys => self.allow_keys,
            DialogInk::AllowOff => self.allow_off,
        }
    }
}

/// `deny` and `allow` with the one nearer `endpoint` moved toward it until
/// the two keep [`MIN_AREA_CONTRAST`], as far as it can go. Moving away from
/// the background keeps both above their floor.
fn lightness_apart(deny: u32, allow: u32, endpoint: u32) -> (u32, u32) {
    if contrast_ratio(deny, allow) >= MIN_AREA_CONTRAST {
        return (deny, allow);
    }
    let outer_is_allow = contrast_ratio(allow, endpoint) <= contrast_ratio(deny, endpoint);
    let (outer, inner) = if outer_is_allow {
        (allow, deny)
    } else {
        (deny, allow)
    };
    let moved = contrast_floor_toward(outer, endpoint, inner, MIN_AREA_CONTRAST);
    if outer_is_allow {
        (deny, moved)
    } else {
        (moved, allow)
    }
}

/// Whether a frame color reads apart from an area color.
fn frame_apart(frame: u32, area: u32) -> bool {
    match (hue_degrees(frame), hue_degrees(area)) {
        (Some(_), Some(_)) => {
            hue_distance(frame, area).is_some_and(|d| d >= MIN_FRAME_HUE_DISTANCE)
                || contrast_ratio(frame, area) >= MIN_FRAME_CONTRAST
        }
        (None, None) => contrast_ratio(frame, area) >= MIN_FRAME_CONTRAST,
        // A grey frame is apart from any hued area.
        _ => true,
    }
}

/// The distance between two colors' hues around the wheel, in degrees.
fn hue_distance(left: u32, right: u32) -> Option<f64> {
    let difference = (hue_degrees(left)? - hue_degrees(right)?).abs();
    Some(difference.min(360.0 - difference))
}

/// The hue of `color` in degrees, or `None` for a color too close to grey
/// to have one a person would name.
fn hue_degrees(color: u32) -> Option<f64> {
    let [red, green, blue] = rgb_channels(color).map(i32::from);
    let max = red.max(green).max(blue);
    let chroma = max - red.min(green).min(blue);
    if chroma < 40 {
        return None;
    }
    let (offset, difference) = if max == red {
        (0, green - blue)
    } else if max == green {
        (120, blue - red)
    } else {
        (240, red - green)
    };
    Some((f64::from(offset) + 60.0 * f64::from(difference) / f64::from(chroma)).rem_euclid(360.0))
}

/// Red, from crimson and rose to red-orange: below 30° or from 320°.
fn is_red_hue(hue: f64) -> bool {
    !(30.0..320.0).contains(&hue)
}

/// Green, from olive (Gruvbox's green) to teal-green: 55° to 170°.
fn is_green_hue(hue: f64) -> bool {
    (55.0..=170.0).contains(&hue)
}

/// `theme` with the trusted dialog's colors written into it: its
/// background, text and the roles its chrome paints, and the palette slots
/// its snapshot's cells are drawn from (`dialog_text::DialogInk::slot`,
/// [`SELECTION_SLOT`]). The dialog's Window draws with this theme from its
/// first frame, so its snapshot's colors, its padding and its chrome all
/// agree, and deriving the colors from it again gives the same colors.
pub(super) fn trusted_dialog_theme(theme: ResolvedTheme) -> ResolvedTheme {
    let colors = TrustedConsentColors::from_theme(theme);
    let mut theme = theme;
    theme.background = colors.background;
    theme.foreground = colors.text;
    theme.roles.surface = colors.background;
    theme.roles.text = colors.text;
    theme.roles.text_muted = colors.secondary;
    theme.roles.warning = colors.border;
    theme.roles.danger = colors.deny;
    theme.roles.success = colors.allow;
    theme.roles.selection = colors.selection;
    theme.roles.accent = colors.selected_text;
    for ink in DialogInk::SLOTTED {
        if let Some(slot) = ink.slot().and_then(|slot| theme.ansi.get_mut(slot)) {
            *slot = colors.ink(ink);
        }
    }
    if let Some(slot) = theme.ansi.get_mut(SELECTION_SLOT) {
        *slot = colors.selection;
    }
    theme
}

/// `rgb` as a canvas pixel.
const fn opaque_pixel(rgb: u32) -> [u8; 4] {
    let [_, red, green, blue] = rgb.to_be_bytes();
    [blue, green, red, 0xff]
}

/// The four sides of `button`'s edge, `edge` pixels thick (no more than half
/// the button).
fn edge_rects(button: Rect, edge: u32) -> [Rect; 4] {
    let thick = edge.min(button.width / 2).min(button.height / 2);
    let right = button.x.saturating_add(button.width).saturating_sub(thick);
    let bottom = button.y.saturating_add(button.height).saturating_sub(thick);
    [
        Rect {
            height: thick,
            ..button
        },
        Rect {
            y: bottom,
            height: thick,
            ..button
        },
        Rect {
            width: thick,
            ..button
        },
        Rect {
            x: right,
            width: thick,
            ..button
        },
    ]
}

/// Paints a trusted dialog's box, `chrome` ([`trusted_chrome`]), over its
/// text, in `colors`: the hard shadow below and right of it, the red (left)
/// and green (right) buttons, then the frame, open where the title and the
/// legend show.
///
/// Each button's tint goes behind the text: only its pixels that still
/// show the dialog's background (or an earlier tint) take it, so the label
/// and keys drawn there stay whole. Its edge is drawn around it. While the
/// dialog cannot say yes (part of its text is hidden), the green button is
/// drawn dimmed.
pub(super) fn paint_trusted_consent_chrome(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    chrome: TrustedChrome,
    colors: TrustedConsentColors,
    proceed_enabled: bool,
) {
    let TrustedChrome {
        frame,
        line,
        top_openings,
        bottom_openings,
        deny,
        allow,
        edge: edge_pixels,
        shadow,
    } = chrome;
    let index = |x: u32, y: u32| {
        (x < width && y < height)
            .then(|| usize::try_from((u64::from(y) * u64::from(width) + u64::from(x)) * 4).ok())
            .flatten()
    };
    let fill = |canvas: &mut [u8], rect: Rect, rgb: u32| {
        let pixel = opaque_pixel(rgb);
        for y in rect.y..rect.y.saturating_add(rect.height).min(height) {
            for x in rect.x..rect.x.saturating_add(rect.width).min(width) {
                if let Some(target) = index(x, y).and_then(|at| canvas.get_mut(at..at + 4)) {
                    target.copy_from_slice(&pixel);
                }
            }
        }
    };
    let behind_text = [
        colors.background,
        colors.deny_tint,
        colors.allow_tint,
        colors.allow_tint_dimmed,
    ]
    .map(opaque_pixel);
    for band in shadow {
        fill(canvas, band, colors.shadow);
    }
    let (allow_tint, allow_edge) = if proceed_enabled {
        (colors.allow_tint, colors.allow)
    } else {
        (colors.allow_tint_dimmed, colors.allow_dimmed)
    };
    for (button, tint, edge) in [
        (deny, colors.deny_tint, colors.deny),
        (allow, allow_tint, allow_edge),
    ] {
        let tint = opaque_pixel(tint);
        for y in button.y..button.y.saturating_add(button.height).min(height) {
            for x in button.x..button.x.saturating_add(button.width).min(width) {
                if let Some(target) = index(x, y).and_then(|at| canvas.get_mut(at..at + 4))
                    && behind_text.iter().any(|pixel| *pixel == *target)
                {
                    target.copy_from_slice(&tint);
                }
            }
        }
        for side in edge_rects(button, edge_pixels) {
            fill(canvas, side, edge);
        }
    }
    let right = frame.x.saturating_add(frame.width).saturating_sub(line);
    let bottom = frame.y.saturating_add(frame.height).saturating_sub(line);
    for (y, openings) in [(frame.y, top_openings), (bottom, bottom_openings)] {
        let mut x = frame.x;
        for (open, end) in openings.into_iter().chain([(u32::MAX, u32::MAX)]) {
            let stop = open.min(frame.x.saturating_add(frame.width));
            fill(
                canvas,
                Rect {
                    x,
                    y,
                    width: stop.saturating_sub(x),
                    height: line,
                },
                colors.border,
            );
            x = x.max(end);
        }
    }
    for x in [frame.x, right] {
        fill(
            canvas,
            Rect {
                x,
                y: frame.y,
                width: line,
                height: frame.height,
            },
            colors.border,
        );
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "bounded box cells require explicit canvas, clip, metrics, color, scale, and direction"
)]
fn paint_box_sequence(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    character: char,
    clip: Rect,
    cell_width: u32,
    cell_height: u32,
    color: u32,
    scale_120: u16,
    horizontal: bool,
) {
    let step = if horizontal { cell_width } else { cell_height };
    if step == 0 {
        return;
    }
    let end = if horizontal {
        clip.x.saturating_add(clip.width)
    } else {
        clip.y.saturating_add(clip.height)
    };
    let mut position = if horizontal { clip.x } else { clip.y };
    while position < end {
        let cell = if horizontal {
            Rect {
                x: position,
                y: clip.y,
                width: cell_width,
                height: cell_height,
            }
        } else {
            Rect {
                x: clip.x,
                y: position,
                width: cell_width,
                height: cell_height,
            }
        };
        paint_box_drawing_cell(
            canvas, width, height, character, cell, clip, color, scale_120,
        );
        position = position.saturating_add(step);
    }
}

pub(super) fn sanitize_frame_title(title: &str, maximum_cells: u32) -> String {
    let mut output = String::new();
    let mut cells = 0_u32;
    let mut previous_space = false;
    for character in title.chars() {
        let character = if character.is_control() || character.is_whitespace() {
            ' '
        } else {
            character
        };
        if character == ' ' && (previous_space || output.is_empty()) {
            continue;
        }
        let width = u32::try_from(character.width().unwrap_or(0)).unwrap_or(0);
        if width > 0 && cells.saturating_add(width) > maximum_cells {
            break;
        }
        output.push(character);
        cells = cells.saturating_add(width);
        previous_space = character == ' ';
    }
    output.trim_end().to_owned()
}

/// How many chrome cells a frame title may take in a pane `allocation_width`
/// buffer pixels wide. The frame keeps three terminal cells on each side
/// (corner, gap and padding), laid out in the terminal's cell geometry, and
/// the title fills the rest in chrome cells, which have their own size.
/// `None` when no title fits.
pub(super) fn frame_title_maximum_cells(
    allocation_width: u32,
    terminal_cell_width: u32,
    chrome_cell_width: u32,
) -> Option<u32> {
    allocation_width
        .checked_sub(terminal_cell_width.saturating_mul(6))
        .map(|room| room / chrome_cell_width.max(1))
        .filter(|cells| *cells > 0)
}

/// Where a frame title is drawn in a pane's top border row: the cleared
/// rect, which keeps one terminal cell of padding on each side of the text,
/// and the text origin, three terminal cells in and centred in the row. The
/// title text is loaded no taller than the terminal cell, so it stays inside
/// the border row at every chrome and terminal size.
pub(super) fn frame_title_placement(
    allocation: Rect,
    cell_width: u32,
    cell_height: u32,
    text: &ChromeText,
) -> (Rect, (u32, u32)) {
    let clear = Rect {
        x: allocation.x.saturating_add(cell_width.saturating_mul(2)),
        y: allocation.y,
        width: text
            .pixel_width()
            .saturating_add(cell_width.saturating_mul(2)),
        height: cell_height,
    };
    let origin = (
        allocation.x.saturating_add(cell_width.saturating_mul(3)),
        allocation
            .y
            .saturating_add(cell_height.saturating_sub(text.pixel_height()) / 2),
    );
    (clear, origin)
}

fn fill_chrome_background(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    rect: Rect,
    color: u32,
    background_alpha: u16,
) {
    let [_, red, green, blue] = color.to_be_bytes();
    let pixel = background_bgra([red, green, blue], background_alpha);
    let right = rect.x.saturating_add(rect.width).min(width);
    let bottom = rect.y.saturating_add(rect.height).min(height);
    for y in rect.y.min(height)..bottom {
        for x in rect.x.min(width)..right {
            let Ok(index) = usize::try_from((y * width + x) * 4) else {
                continue;
            };
            if let Some(target) = canvas.get_mut(index..index + 4) {
                target.copy_from_slice(&pixel);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DividerSide {
    First,
    Second,
}

fn divider_active_segment(
    divider: PaneDivider,
    pane: Rect,
    split_shared_border: bool,
) -> Option<Rect> {
    let divider_right = divider.rect.x.checked_add(divider.rect.width)?;
    let divider_bottom = divider.rect.y.checked_add(divider.rect.height)?;
    let pane_right = pane.x.checked_add(pane.width)?;
    let pane_bottom = pane.y.checked_add(pane.height)?;
    let (side, mut segment) = match divider.axis {
        splinterm_core::Axis::Horizontal => {
            let side = if pane_right == divider.rect.x {
                DividerSide::First
            } else if pane.x == divider_right {
                DividerSide::Second
            } else {
                return None;
            };
            let top = pane.y.max(divider.rect.y);
            let bottom = pane_bottom.min(divider_bottom);
            (bottom > top).then_some((
                side,
                Rect {
                    y: top,
                    height: bottom - top,
                    ..divider.rect
                },
            ))?
        }
        splinterm_core::Axis::Vertical => {
            let side = if pane_bottom == divider.rect.y {
                DividerSide::First
            } else if pane.y == divider_bottom {
                DividerSide::Second
            } else {
                return None;
            };
            let left = pane.x.max(divider.rect.x);
            let right = pane_right.min(divider_right);
            (right > left).then_some((
                side,
                Rect {
                    x: left,
                    width: right - left,
                    ..divider.rect
                },
            ))?
        }
    };

    // A single shared border touches both leaves over its complete length. tmux
    // disambiguates that case by assigning the leading half to the first
    // (left/top) pane and the trailing half to the second (right/bottom) pane.
    if split_shared_border && segment == divider.rect {
        match divider.axis {
            splinterm_core::Axis::Horizontal => {
                let leading = segment.height / 2 + segment.height % 2;
                if side == DividerSide::First {
                    segment.height = leading;
                } else {
                    segment.y = segment.y.checked_add(leading)?;
                    segment.height = segment.height.checked_sub(leading)?;
                }
            }
            splinterm_core::Axis::Vertical => {
                let leading = segment.width / 2 + segment.width % 2;
                if side == DividerSide::First {
                    segment.width = leading;
                } else {
                    segment.x = segment.x.checked_add(leading)?;
                    segment.width = segment.width.checked_sub(leading)?;
                }
            }
        }
    }
    (segment.width > 0 && segment.height > 0).then_some(segment)
}

fn active_junction_arm_clips(
    divider: PaneDivider,
    active_segment: Rect,
    junction: Rect,
) -> Vec<(char, Rect)> {
    let mut arms = Vec::with_capacity(2);
    let active_right = active_segment.x.saturating_add(active_segment.width);
    let active_bottom = active_segment.y.saturating_add(active_segment.height);
    let junction_right = junction.x.saturating_add(junction.width);
    let junction_bottom = junction.y.saturating_add(junction.height);
    match divider.axis {
        splinterm_core::Axis::Horizontal => {
            let half = junction.height / 2;
            if active_segment.y < junction.y && active_bottom >= junction.y {
                arms.push((
                    '│',
                    Rect {
                        height: half + junction.height % 2,
                        ..junction
                    },
                ));
            }
            if active_segment.y <= junction_bottom && active_bottom > junction_bottom {
                arms.push((
                    '│',
                    Rect {
                        y: junction.y.saturating_add(half),
                        height: junction.height.saturating_sub(half),
                        ..junction
                    },
                ));
            }
        }
        splinterm_core::Axis::Vertical => {
            let half = junction.width / 2;
            if active_segment.x < junction.x && active_right >= junction.x {
                arms.push((
                    '─',
                    Rect {
                        width: half + junction.width % 2,
                        ..junction
                    },
                ));
            }
            if active_segment.x <= junction_right && active_right > junction_right {
                arms.push((
                    '─',
                    Rect {
                        x: junction.x.saturating_add(half),
                        width: junction.width.saturating_sub(half),
                        ..junction
                    },
                ));
            }
        }
    }
    arms
}

pub(super) fn divider_junction(first: PaneDivider, second: PaneDivider) -> Option<(char, Rect)> {
    let (vertical, horizontal) = match (first.axis, second.axis) {
        (splinterm_core::Axis::Horizontal, splinterm_core::Axis::Vertical) => (first, second),
        (splinterm_core::Axis::Vertical, splinterm_core::Axis::Horizontal) => (second, first),
        _ => return None,
    };
    let vertical_right = vertical.rect.x.checked_add(vertical.rect.width)?;
    let vertical_bottom = vertical.rect.y.checked_add(vertical.rect.height)?;
    let horizontal_right = horizontal.rect.x.checked_add(horizontal.rect.width)?;
    let horizontal_bottom = horizontal.rect.y.checked_add(horizontal.rect.height)?;
    if horizontal_right == vertical.rect.x
        && horizontal.rect.y < vertical_bottom
        && vertical.rect.y < horizontal_bottom
    {
        return Some((
            '┤',
            Rect {
                x: vertical.rect.x,
                y: horizontal.rect.y,
                width: vertical.rect.width,
                height: horizontal.rect.height,
            },
        ));
    }
    if horizontal.rect.x == vertical_right
        && horizontal.rect.y < vertical_bottom
        && vertical.rect.y < horizontal_bottom
    {
        return Some((
            '├',
            Rect {
                x: vertical.rect.x,
                y: horizontal.rect.y,
                width: vertical.rect.width,
                height: horizontal.rect.height,
            },
        ));
    }
    if vertical_bottom == horizontal.rect.y
        && vertical.rect.x < horizontal_right
        && horizontal.rect.x < vertical_right
    {
        return Some((
            '┴',
            Rect {
                x: vertical.rect.x,
                y: horizontal.rect.y,
                width: vertical.rect.width,
                height: horizontal.rect.height,
            },
        ));
    }
    if vertical.rect.y == horizontal_bottom
        && vertical.rect.x < horizontal_right
        && horizontal.rect.x < vertical_right
    {
        return Some((
            '┬',
            Rect {
                x: vertical.rect.x,
                y: horizontal.rect.y,
                width: vertical.rect.width,
                height: horizontal.rect.height,
            },
        ));
    }
    None
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "line junctions and complete framed panels share one trusted clipped chrome pass"
)]
pub(super) fn paint_pane_chrome(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    layout: &PaneLayout,
    active_splint: Option<SplintId>,
    theme: ResolvedTheme,
    cell_width: u32,
    cell_height: u32,
    scale_120: u32,
    frame_titles: &HashMap<SplintId, CachedFrameTitle>,
) -> Result<()> {
    let scale = u16::try_from(scale_120).context("pane chrome scale fits u16")?;
    match layout.chrome {
        PaneChrome::None => {}
        PaneChrome::Line { .. } => {
            // The extent reaches across the pane gap to the divider lines, so
            // the active border still meets its junctions when the gap is set.
            let active_rect = active_splint.and_then(|id| layout.extent(id));
            let split_shared_border = layout.panes.len() == 2;

            // Establish the complete inactive network first. Active segments and
            // junction arms are overlays so focus never erases connectivity.
            for divider in &layout.separators {
                let clip = App::buffer_rect(divider.rect, scale_120)?;
                let horizontal = divider.axis == splinterm_core::Axis::Vertical;
                paint_box_sequence(
                    canvas,
                    width,
                    height,
                    if horizontal { '─' } else { '│' },
                    clip,
                    cell_width,
                    cell_height,
                    theme.roles.divider,
                    scale,
                    horizontal,
                );
            }
            for (index, first) in layout.separators.iter().copied().enumerate() {
                for second in layout.separators[index + 1..].iter().copied() {
                    let Some((character, logical_cell)) = divider_junction(first, second) else {
                        continue;
                    };
                    let clip = App::buffer_rect(logical_cell, scale_120)?;
                    paint_box_drawing_cell(
                        canvas,
                        width,
                        height,
                        character,
                        Rect {
                            x: clip.x,
                            y: clip.y,
                            width: cell_width,
                            height: cell_height,
                        },
                        clip,
                        theme.roles.divider,
                        scale,
                    );
                }
            }

            if let Some(pane) = active_rect {
                for divider in &layout.separators {
                    let Some(segment) = divider_active_segment(*divider, pane, split_shared_border)
                    else {
                        continue;
                    };
                    let clip = App::buffer_rect(segment, scale_120)?;
                    let horizontal = divider.axis == splinterm_core::Axis::Vertical;
                    paint_box_sequence(
                        canvas,
                        width,
                        height,
                        if horizontal { '─' } else { '│' },
                        clip,
                        cell_width,
                        cell_height,
                        theme.roles.focus_ring,
                        scale,
                        horizontal,
                    );
                }

                // A junction can border more than one pane. Overlay each active
                // arm independently instead of promoting the complete tee.
                for (index, first) in layout.separators.iter().copied().enumerate() {
                    for second in layout.separators[index + 1..].iter().copied() {
                        let Some((_, logical_cell)) = divider_junction(first, second) else {
                            continue;
                        };
                        let cell = App::buffer_rect(logical_cell, scale_120)?;
                        for divider in [first, second] {
                            let Some(segment) =
                                divider_active_segment(divider, pane, split_shared_border)
                            else {
                                continue;
                            };
                            for (character, logical_clip) in
                                active_junction_arm_clips(divider, segment, logical_cell)
                            {
                                let clip = App::buffer_rect(logical_clip, scale_120)?;
                                paint_box_drawing_cell(
                                    canvas,
                                    width,
                                    height,
                                    character,
                                    Rect {
                                        x: cell.x,
                                        y: cell.y,
                                        width: cell_width,
                                        height: cell_height,
                                    },
                                    clip,
                                    theme.roles.focus_ring,
                                    scale,
                                );
                            }
                        }
                    }
                }
            }
        }
        PaneChrome::Frame { .. } => {
            for pane in &layout.panes {
                let allocation = App::buffer_rect(pane.allocation, scale_120)?;
                let content = App::buffer_rect(pane.rect, scale_120)?;
                let right = allocation.x.saturating_add(allocation.width);
                let bottom = allocation.y.saturating_add(allocation.height);
                let content_right = content.x.saturating_add(content.width);
                let content_bottom = content.y.saturating_add(content.height);
                let color = if Some(pane.splint_id) == active_splint {
                    theme.roles.focus_ring
                } else {
                    theme.roles.divider
                };
                let top = Rect {
                    x: allocation.x.saturating_add(cell_width),
                    y: allocation.y,
                    width: allocation
                        .width
                        .saturating_sub(cell_width.saturating_mul(2)),
                    height: content.y.saturating_sub(allocation.y),
                };
                let bottom_edge = Rect {
                    x: allocation.x.saturating_add(cell_width),
                    y: content_bottom,
                    width: allocation
                        .width
                        .saturating_sub(cell_width.saturating_mul(2)),
                    height: bottom.saturating_sub(content_bottom),
                };
                let left = Rect {
                    x: allocation.x,
                    y: allocation.y.saturating_add(cell_height),
                    width: content.x.saturating_sub(allocation.x),
                    height: allocation
                        .height
                        .saturating_sub(cell_height.saturating_mul(2)),
                };
                let right_edge = Rect {
                    x: content_right,
                    y: allocation.y.saturating_add(cell_height),
                    width: right.saturating_sub(content_right),
                    height: allocation
                        .height
                        .saturating_sub(cell_height.saturating_mul(2)),
                };
                paint_box_sequence(
                    canvas,
                    width,
                    height,
                    '─',
                    top,
                    cell_width,
                    cell_height,
                    color,
                    scale,
                    true,
                );
                paint_box_sequence(
                    canvas,
                    width,
                    height,
                    '─',
                    bottom_edge,
                    cell_width,
                    cell_height,
                    color,
                    scale,
                    true,
                );
                paint_box_sequence(
                    canvas,
                    width,
                    height,
                    '│',
                    left,
                    cell_width,
                    cell_height,
                    color,
                    scale,
                    false,
                );
                paint_box_sequence(
                    canvas,
                    width,
                    height,
                    '│',
                    right_edge,
                    cell_width,
                    cell_height,
                    color,
                    scale,
                    false,
                );
                let top_left = Rect {
                    x: allocation.x,
                    y: allocation.y,
                    width: cell_width,
                    height: cell_height,
                };
                let top_right = Rect {
                    x: right.saturating_sub(cell_width),
                    y: allocation.y,
                    width: cell_width,
                    height: cell_height,
                };
                let bottom_left = Rect {
                    x: allocation.x,
                    y: bottom.saturating_sub(cell_height),
                    width: cell_width,
                    height: cell_height,
                };
                let bottom_right = Rect {
                    x: right.saturating_sub(cell_width),
                    y: bottom.saturating_sub(cell_height),
                    width: cell_width,
                    height: cell_height,
                };
                for (character, cell) in [
                    ('┌', top_left),
                    ('┐', top_right),
                    ('└', bottom_left),
                    ('┘', bottom_right),
                ] {
                    let clip = cell;
                    paint_box_drawing_cell(
                        canvas, width, height, character, cell, clip, color, scale,
                    );
                }
                if let Some(title) = frame_titles.get(&pane.splint_id) {
                    let (clear, origin) =
                        frame_title_placement(allocation, cell_width, cell_height, &title.text);
                    fill_chrome_background(
                        canvas,
                        width,
                        height,
                        clear,
                        theme.roles.surface,
                        theme.background_alpha,
                    );
                    title
                        .text
                        .paint(canvas, width, height, origin, clear, color);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::TEXT_MIN_CONTRAST;
    use crate::frontend::{DialogLine, DialogStyle, dialog_text};
    use crate::pane::PaneGeometry;
    use splinterm_core::Axis;

    /// The restart question's shape: a title, prose paragraphs wrapped at
    /// 76, the deadline, the hint that names the buttons, and the two
    /// button labels.
    fn restart_text() -> Vec<DialogText> {
        let prose = |text: &str| DialogLine::new(text).with_wrap_at(76);
        vec![DialogText {
            lines: vec![
                DialogLine::title("restart splinterd?"),
                prose(
                    "The running Splinterm daemon does not speak this client's protocol. It is an older release, such as 0.1.x, or another incompatible build, so this client (Splinterm 0.2.1) cannot ask it for its shells or ask it to hand them over.",
                )
                .with_style(DialogStyle::Strong),
                prose(
                    "About 3 shells appear to be running: a best-effort count of its running workload units.",
                ),
                DialogLine::blank(),
                prose(
                    "Restarting it starts the installed build and ends every shell it runs and the programs in them. Open Splinterm Windows must be reopened afterwards.",
                ),
                DialogLine::blank(),
                prose("Not now opens nothing and changes nothing."),
                DialogLine::new(
                    "Closing this dialog, or no answer within 120 seconds, means Not now.",
                ),
                DialogLine::blank(),
                DialogLine::hint(
                    "R or the green Restart button (right) restarts. Enter, Escape, D, or the red Not now button (left) answer Not now.",
                )
                .with_wrap_at(76),
                DialogLine::deny("Not now"),
                DialogLine::allow("Restart now, end about 3 shells"),
            ],
            minimum_rows: 0,
        }]
    }

    fn cell() -> CellGeometry {
        CellGeometry::from_metrics(8, 17, 13, 4, 13).unwrap()
    }

    fn content(width: u32, height: u32) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    /// The restart question as its dialog child builds it.
    fn restart_dialog() -> crate::frontend::TrustedConsentUi {
        let (decision, _answers) = std::sync::mpsc::channel();
        crate::frontend::TrustedConsentUi::restart(decision).with_text(restart_text().remove(0))
    }

    /// `dialog` laid out for `fit` exactly as the Window lays it out.
    fn laid_out(
        dialog: &mut crate::frontend::TrustedConsentUi,
        fit: &TrustedFit,
    ) -> TerminalSnapshot {
        dialog.lay_out_again(fit.frame(), Instant::now()).unwrap()
    }

    fn row_text(snapshot: &TerminalSnapshot, row: usize) -> String {
        snapshot.visible_rows[row]
            .cells
            .iter()
            .map(|cell| cell.content.as_str())
            .collect()
    }

    fn fit_at(variants: &[DialogText], width: u32, height: u32, scale_120: u32) -> TrustedFit {
        trusted_dialog_fit(
            content(width, height),
            cell(),
            TerminalPadding::DEFAULT,
            scale_120,
            variants,
        )
        .unwrap()
    }

    fn shown_rows(snapshot: &TerminalSnapshot) -> Vec<String> {
        (0..snapshot.rows)
            .map(|row| row_text(snapshot, row))
            .collect()
    }

    /// The columns of `row` that show a character.
    fn visible_columns(snapshot: &TerminalSnapshot, row: usize) -> Vec<u32> {
        snapshot.visible_rows[row]
            .cells
            .iter()
            .enumerate()
            .filter(|(_, cell)| !cell.content.trim().is_empty())
            .map(|(column, _)| u32::try_from(column).unwrap())
            .collect()
    }

    /// The answer hint shows when the text fits; when it does not, the
    /// notice that the question cannot say yes takes its place.
    fn assert_answer_rows(
        dialog: &crate::frontend::TrustedConsentUi,
        shown: &[String],
        size: &str,
    ) {
        let starts = |text: &str| shown.iter().any(|row| row.trim_start().starts_with(text));
        if dialog.clipped() {
            assert!(starts("Part of this question"), "{size}: {shown:#?}");
            assert!(!starts("R or the green"), "{size}");
        } else {
            assert!(!starts(dialog_text::CLIPPED_MARKER), "{size}: {shown:#?}");
            assert!(starts("R or the green"), "{size}: {shown:#?}");
        }
    }

    /// A canvas `width` by `height` in the dialog's background.
    fn background_canvas(width: u32, height: u32, colors: TrustedConsentColors) -> Vec<u8> {
        opaque_pixel(colors.background).repeat(usize::try_from(width * height).unwrap())
    }

    fn pixel_at(canvas: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let index = usize::try_from((y * width + x) * 4).unwrap();
        canvas[index..index + 4].try_into().unwrap()
    }

    fn overlaps(a: Rect, b: Rect) -> bool {
        a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
    }

    /// The hard shadow stays inside the buffer and off the box: no band
    /// touches the frame, a button, or a cell that shows a character, so
    /// none of them is measured against a background it does not sit on.
    fn assert_shadow_clear(
        chrome: &TrustedChrome,
        snapshot: &TerminalSnapshot,
        geometry: &WindowGeometry,
        size: &str,
    ) {
        let (width, height) = (geometry.buffer_width(), geometry.buffer_height());
        let grid = geometry.grid_rect;
        let (cell_width, cell_height) = (geometry.cell.width, geometry.cell.height);
        assert!(!chrome.shadow.is_empty(), "{size}");
        for band in chrome.shadow.iter().copied() {
            assert!(
                band.x + band.width <= width && band.y + band.height <= height,
                "{size}: {band:?}"
            );
            for part in [chrome.frame, chrome.deny, chrome.allow] {
                assert!(!overlaps(band, part), "{size}: {band:?} on {part:?}");
            }
            for row in 0..snapshot.rows {
                for column in visible_columns(snapshot, row) {
                    let cell = Rect {
                        x: grid.x + column * cell_width,
                        y: grid.y + u32::try_from(row).unwrap() * cell_height,
                        width: cell_width,
                        height: cell_height,
                    };
                    assert!(
                        !overlaps(band, cell),
                        "{size}: {band:?} on row {row} column {column}"
                    );
                }
            }
        }
    }

    /// At its natural size, at 1x and at 1.25x, the dialog leaves room for
    /// its hard shadow: the frame ends at least the shadow's offset inside
    /// the buffer, and the two bands, right of and below the grid and as
    /// thick as the offset, are painted 35% black over the background, and
    /// nothing else is. A click on them is on no button.
    #[test]
    fn the_hard_shadow_falls_below_and_right_of_the_box_inside_the_window() {
        let variants = restart_text();
        let colors = TrustedConsentColors::fixed();
        assert_eq!(colors.shadow, mix_encoded(colors.background, 0, 0.35));
        assert_ne!(colors.shadow, colors.background);
        let shadow_pixel = opaque_pixel(colors.shadow);
        for (scale_120, offset) in [(SCALE_DENOMINATOR, 6), (150, 8)] {
            let natural = WindowGeometry::for_grid(
                u32::try_from(natural_columns(&variants)).unwrap(),
                u32::try_from(natural_rows(&variants)).unwrap(),
                cell(),
                TerminalPadding::DEFAULT,
                scale_120,
            )
            .unwrap();
            let fit = fit_at(
                &variants,
                natural.logical_width(),
                natural.logical_height(),
                scale_120,
            );
            assert!(fit.fits(), "{scale_120}: {fit:?}");
            let mut dialog = restart_dialog();
            let snapshot = laid_out(&mut dialog, &fit);
            let geometry = fit.geometry;
            let (width, height) = (geometry.buffer_width(), geometry.buffer_height());
            let chrome = trusted_chrome(
                width,
                height,
                scale_120,
                Some(&geometry),
                &snapshot.visible_rows,
            );
            let frame = chrome.frame;
            assert!(frame.x + frame.width + offset <= width, "{scale_120}");
            assert!(frame.y + frame.height + offset <= height, "{scale_120}");
            let grid = geometry.grid_rect;
            let right = grid.x + geometry.columns * geometry.cell.width;
            let bottom = grid.y + geometry.rows * geometry.cell.height;
            assert_eq!(
                chrome.shadow,
                vec![
                    Rect {
                        x: right,
                        y: grid.y + offset,
                        width: offset,
                        height: bottom - grid.y,
                    },
                    Rect {
                        x: grid.x + offset,
                        y: bottom,
                        width: right - grid.x - offset,
                        height: offset,
                    },
                ],
                "{scale_120}"
            );
            assert_shadow_clear(&chrome, &snapshot, &geometry, &format!("{scale_120}"));
            let mut canvas = background_canvas(width, height, colors);
            paint_trusted_consent_chrome(&mut canvas, width, height, chrome.clone(), colors, true);
            let in_shadow = |x: u32, y: u32| {
                chrome.shadow.iter().any(|band| {
                    (band.x..band.x + band.width).contains(&x)
                        && (band.y..band.y + band.height).contains(&y)
                })
            };
            for y in 0..height {
                for x in 0..width {
                    assert_eq!(
                        pixel_at(&canvas, width, x, y) == shadow_pixel,
                        in_shadow(x, y),
                        "{scale_120} at {x},{y}"
                    );
                }
            }
            let below = f64::from(bottom + offset / 2) * f64::from(SCALE_DENOMINATOR)
                / f64::from(scale_120);
            assert!(!buttons_hit(below, scale_120, &chrome), "{scale_120}");
        }
    }

    /// At its natural size the dialog fits exactly, and in every smaller or
    /// odd Window (the 2026-10-22 tiles among them), at 1x and at 1.25x, its
    /// box holds it: the body ends above the buttons, the buttons above the
    /// legend, each label and its keys inside its own button, the legend
    /// and the title inside the frame, which is open behind them, and a
    /// click (in logical pixels, converted to buffer pixels) is on a button
    /// exactly where the buttons are painted. Every size shows how to
    /// answer: the hint, or the notice that replaces it while text is
    /// hidden.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one walk over every size checks the box, its paint and its hit test together"
    )]
    fn the_box_holds_the_text_and_its_painted_buttons_and_clicks_agree() {
        let variants = restart_text();
        let natural = WindowGeometry::for_grid(
            u32::try_from(natural_columns(&variants)).unwrap(),
            u32::try_from(natural_rows(&variants)).unwrap(),
            cell(),
            TerminalPadding::DEFAULT,
            SCALE_DENOMINATOR,
        )
        .unwrap();
        let natural_fit = fit_at(
            &variants,
            natural.logical_width(),
            natural.logical_height(),
            SCALE_DENOMINATOR,
        );
        assert!(natural_fit.fits(), "{natural_fit:?}");
        assert_eq!(
            usize::try_from(natural_fit.geometry.columns).unwrap(),
            natural_columns(&variants)
        );
        assert_eq!(natural_fit.needed_rows, natural_fit.room_rows);
        let colors = TrustedConsentColors::fixed();
        let sizes = [
            (natural.logical_width(), natural.logical_height()),
            (621, 368),
            (621, 375),
            (500, 420),
            (400, 300),
            (300, 200),
            (1200, 900),
        ];
        for scale_120 in [SCALE_DENOMINATOR, 150] {
            for (width, height) in sizes {
                let size = format!("{width}x{height} at {scale_120}");
                let fit = fit_at(&variants, width, height, scale_120);
                let mut dialog = restart_dialog();
                let snapshot = laid_out(&mut dialog, &fit);
                assert_eq!(dialog.clipped(), !fit.fits(), "{size}");
                let geometry = fit.geometry;
                assert!(snapshot.columns <= geometry.columns as usize, "{size}");
                assert!(snapshot.rows <= geometry.rows as usize, "{size}");
                let (buffer_width, buffer_height) =
                    (geometry.buffer_width(), geometry.buffer_height());
                let chrome = trusted_chrome(
                    buffer_width,
                    buffer_height,
                    scale_120,
                    Some(&geometry),
                    &snapshot.visible_rows,
                );
                let grid = geometry.grid_rect;
                let (cell_width, cell_height) = (geometry.cell.width, geometry.cell.height);
                let row_top = |row: usize| grid.y + u32::try_from(row).unwrap() * cell_height;
                let places = chrome_rows(geometry.rows as usize);
                assert_shadow_clear(&chrome, &snapshot, &geometry, &size);
                // The body ends above the buttons; the buttons end above
                // the legend's row.
                if let Some(last) =
                    (1..places.label).rfind(|row| !row_text(&snapshot, *row).trim().is_empty())
                {
                    assert!(row_top(last + 1) <= chrome.deny.y, "{size}: {chrome:?}");
                }
                assert!(
                    chrome.deny.y + chrome.deny.height <= row_top(places.legend),
                    "{size}: {chrome:?}"
                );
                // Each label and its keys sit inside their own button.
                for row in [places.label, places.keys] {
                    for column in visible_columns(&snapshot, row) {
                        let x = grid.x + column * cell_width;
                        let end = x + cell_width;
                        let button = if x >= buffer_width / 2 {
                            chrome.allow
                        } else {
                            chrome.deny
                        };
                        assert!(
                            x >= button.x + chrome.edge
                                && end + chrome.edge <= button.x + button.width,
                            "{size}: column {column} of row {row} outside {button:?}"
                        );
                    }
                }
                let labels = row_text(&snapshot, places.label);
                assert!(labels.contains("Not now"), "{size}: {labels:?}");
                assert_eq!(
                    labels.contains("Restart now, end about 3 shells"),
                    fit.buttons_fit,
                    "{size}: {labels:?}"
                );
                assert!(
                    row_text(&snapshot, places.keys).contains("esc · d · ↵"),
                    "{size}"
                );
                // The legend and the title stay inside the frame.
                for row in [0, places.legend] {
                    for column in visible_columns(&snapshot, row) {
                        assert!(
                            (2..geometry.columns - 2).contains(&column),
                            "{size}: column {column} of row {row}"
                        );
                    }
                }
                let mut canvas = background_canvas(buffer_width, buffer_height, colors);
                paint_trusted_consent_chrome(
                    &mut canvas,
                    buffer_width,
                    buffer_height,
                    chrome.clone(),
                    colors,
                    !dialog.clipped(),
                );
                // The frame is open behind every title character.
                for column in visible_columns(&snapshot, 0) {
                    let x = grid.x + column * cell_width + cell_width / 2;
                    assert_ne!(
                        pixel_at(&canvas, buffer_width, x, chrome.frame.y),
                        opaque_pixel(colors.border),
                        "{size}: title column {column}"
                    );
                }
                // The painted red edge and the hit test agree on every
                // buffer row.
                for y in 0..buffer_height {
                    let painted_red = pixel_at(&canvas, buffer_width, chrome.deny.x, y)
                        == opaque_pixel(colors.deny);
                    let logical_y =
                        (f64::from(y) + 0.5) * f64::from(SCALE_DENOMINATOR) / f64::from(scale_120);
                    assert_eq!(
                        buttons_hit(logical_y, scale_120, &chrome),
                        painted_red,
                        "{size} at buffer row {y}"
                    );
                }
                assert_answer_rows(&dialog, &shown_rows(&snapshot), &size);
            }
        }
        // The smallest tile does not fit at this font, so it says so and
        // cannot restart.
        let mut dialog = restart_dialog();
        laid_out(&mut dialog, &fit_at(&variants, 300, 200, SCALE_DENOMINATOR));
        assert!(dialog.clipped());
    }

    /// A narrower Window with room enough rewraps and cuts nothing; one too
    /// short at this font marks the cut body and says it cannot say yes.
    #[test]
    fn narrow_windows_rewrap_and_short_ones_say_what_is_hidden() {
        let variants = restart_text();
        let narrow = fit_at(&variants, 621, 700, SCALE_DENOMINATOR);
        assert!(narrow.wrap_columns < dialog_text::natural_text_columns(&variants));
        assert!(narrow.fits(), "{narrow:?}");
        let mut dialog = restart_dialog();
        laid_out(&mut dialog, &narrow);
        assert!(!dialog.clipped());
        let short = fit_at(&variants, 621, 260, SCALE_DENOMINATOR);
        assert!(!short.fits());
        let mut dialog = restart_dialog();
        let shown = shown_rows(&laid_out(&mut dialog, &short));
        assert!(
            shown
                .iter()
                .any(|row| row.trim() == dialog_text::CLIPPED_MARKER),
            "{shown:#?}"
        );
        assert!(
            shown
                .iter()
                .any(|row| row.trim_start().starts_with("Part of this question")),
            "{shown:#?}"
        );
        assert!(dialog.clipped());
    }

    /// A Window whose body fits but whose Allow label is wider than its
    /// button cuts the label, says what is hidden, and cannot say yes: its
    /// green button is painted dimmed.
    #[test]
    fn a_cut_allow_label_counts_as_hidden_text() {
        let variants = restart_text();
        // 60 columns: each button holds 27 characters; the label has 31.
        let width = 60 * 8 + 24;
        let fit = fit_at(&variants, width, 900, SCALE_DENOMINATOR);
        assert_eq!(fit.geometry.columns, 60);
        assert!(fit.needed_rows <= fit.room_rows, "{fit:?}");
        assert!(!fit.buttons_fit);
        assert!(!fit.fits());
        let mut dialog = restart_dialog();
        let snapshot = laid_out(&mut dialog, &fit);
        assert!(dialog.clipped());
        let places = chrome_rows(fit.geometry.rows as usize);
        let labels = row_text(&snapshot, places.label);
        assert!(labels.contains("Restart now, end about 3 s…"), "{labels:?}");
        assert_answer_rows(&dialog, &shown_rows(&snapshot), "cut label");
        let geometry = fit.geometry;
        let (buffer_width, buffer_height) = (geometry.buffer_width(), geometry.buffer_height());
        let chrome = trusted_chrome(
            buffer_width,
            buffer_height,
            SCALE_DENOMINATOR,
            Some(&geometry),
            &snapshot.visible_rows,
        );
        let colors = TrustedConsentColors::fixed();
        let mut canvas = background_canvas(buffer_width, buffer_height, colors);
        paint_trusted_consent_chrome(
            &mut canvas,
            buffer_width,
            buffer_height,
            chrome.clone(),
            colors,
            !dialog.clipped(),
        );
        assert_eq!(
            pixel_at(&canvas, buffer_width, chrome.allow.x, chrome.allow.y + 4),
            opaque_pixel(colors.allow_dimmed)
        );
        assert_eq!(
            pixel_at(
                &canvas,
                buffer_width,
                chrome.allow.x + 4,
                chrome.allow.y + 4
            ),
            opaque_pixel(colors.allow_tint_dimmed)
        );
    }

    /// The font decision fed by the real fit: a Window that holds the
    /// dialog at a 7x14 cell does not at a 9x19 one, so the dialog steps
    /// down to it, unless that is below 70% of the configured size.
    #[test]
    fn the_font_steps_down_to_a_cell_that_really_fits() {
        let variants = restart_text();
        let large = CellGeometry::from_metrics(9, 19, 15, 4, 15).unwrap();
        let small = CellGeometry::from_metrics(7, 14, 11, 3, 11).unwrap();
        let tile = WindowGeometry::for_grid(
            u32::try_from(natural_columns(&variants)).unwrap(),
            u32::try_from(natural_rows(&variants)).unwrap(),
            small,
            TerminalPadding::DEFAULT,
            SCALE_DENOMINATOR,
        )
        .unwrap();
        let fits = |cell| {
            trusted_dialog_fit(
                content(tile.logical_width(), tile.logical_height()),
                cell,
                TerminalPadding::DEFAULT,
                SCALE_DENOMINATOR,
                &variants,
            )
            .unwrap()
            .fits()
        };
        assert!(!fits(large));
        assert!(fits(small));
        let candidates = [(0, 19.0, fits(large)), (-1, 14.0, fits(small))];
        let step = dialog_font_step(19.0, candidates);
        assert_eq!((step.zoom_steps, step.fits), (-1, true));
        // 14 px is below 70% of a 21 px configured size: not used.
        let step = dialog_font_step(21.0, [(0, 21.0, fits(large)), (-1, 14.0, fits(small))]);
        assert_eq!((step.zoom_steps, step.fits), (0, false));
    }

    /// C1: the fixed size never asks for more than the compositor's
    /// suggested bounds; without bounds, or along an unbounded (0) side, it
    /// is the natural size.
    #[test]
    fn the_fixed_size_stays_within_the_suggested_bounds() {
        // A long consent prompt: about 63 rows of 17 px.
        let natural = (824, 1095);
        assert_eq!(bounded_fixed_size(natural, None), natural);
        assert_eq!(bounded_fixed_size(natural, Some((1920, 1040))), (824, 1040));
        assert_eq!(bounded_fixed_size(natural, Some((600, 400))), (600, 400));
        assert_eq!(bounded_fixed_size(natural, Some((0, 900))), (824, 900));
        assert_eq!(bounded_fixed_size(natural, Some((700, 0))), (700, 1095));
        assert_eq!(bounded_fixed_size(natural, Some((3840, 2160))), natural);
    }

    /// The buttons' tints go behind the text and their edges around them;
    /// the frame is open where its text shows, and nothing else is touched.
    #[test]
    fn buttons_are_tinted_behind_their_text_and_the_frame_left_open() {
        let (width, height) = (200, 100);
        let colors = TrustedConsentColors::fixed();
        let chrome = TrustedChrome {
            frame: Rect {
                x: 4,
                y: 4,
                width: 192,
                height: 92,
            },
            line: 2,
            top_openings: vec![(20, 60)],
            bottom_openings: vec![(20, 40), (40, 80)],
            deny: Rect {
                x: 10,
                y: 60,
                width: 87,
                height: 30,
            },
            allow: Rect {
                x: 103,
                y: 60,
                width: 87,
                height: 30,
            },
            edge: 1,
            shadow: Vec::new(),
        };
        let glyph = [1, 2, 3, 0xff];
        let mut canvas = background_canvas(width, height, colors);
        let at = usize::try_from((75 * width + 50) * 4).unwrap();
        canvas[at..at + 4].copy_from_slice(&glyph);
        paint_trusted_consent_chrome(&mut canvas, width, height, chrome.clone(), colors, true);
        let pixel = |canvas: &[u8], x, y| pixel_at(canvas, width, x, y);
        assert_eq!(pixel(&canvas, 30, 75), opaque_pixel(colors.deny_tint));
        assert_eq!(pixel(&canvas, 150, 75), opaque_pixel(colors.allow_tint));
        // Text drawn in a button stays.
        assert_eq!(pixel(&canvas, 50, 75), glyph);
        assert_eq!(pixel(&canvas, 10, 75), opaque_pixel(colors.deny));
        assert_eq!(pixel(&canvas, 96, 75), opaque_pixel(colors.deny));
        assert_eq!(pixel(&canvas, 150, 60), opaque_pixel(colors.allow));
        assert_eq!(pixel(&canvas, 150, 89), opaque_pixel(colors.allow));
        // The gap between the buttons and the body above are untouched.
        let background = opaque_pixel(colors.background);
        assert_eq!(pixel(&canvas, 100, 75), background);
        assert_eq!(pixel(&canvas, 100, 40), background);
        // The frame, open behind its title and legend.
        let border = opaque_pixel(colors.border);
        assert_eq!(pixel(&canvas, 10, 4), border);
        assert_eq!(pixel(&canvas, 40, 5), background);
        assert_eq!(pixel(&canvas, 60, 4), border);
        assert_eq!(pixel(&canvas, 30, 94), background);
        assert_eq!(pixel(&canvas, 79, 95), background);
        assert_eq!(pixel(&canvas, 80, 94), border);
        assert_eq!(pixel(&canvas, 4, 50), border);
        assert_eq!(pixel(&canvas, 195, 50), border);
        assert_eq!(pixel(&canvas, 2, 50), background);
        // A dialog that cannot say yes dims its green button; painting
        // again replaces the earlier tint.
        paint_trusted_consent_chrome(&mut canvas, width, height, chrome, colors, false);
        assert_eq!(
            pixel(&canvas, 150, 75),
            opaque_pixel(colors.allow_tint_dimmed)
        );
        assert_eq!(pixel(&canvas, 150, 60), opaque_pixel(colors.allow_dimmed));
        assert_eq!(pixel(&canvas, 30, 75), opaque_pixel(colors.deny_tint));
    }

    /// Without a grid the buttons take the bottom fifth, and the hit test
    /// agrees.
    #[test]
    fn without_a_grid_the_buttons_take_the_bottom_fifth() {
        let chrome = trusted_chrome(400, 300, SCALE_DENOMINATOR, None, &[]);
        assert_eq!(chrome.deny.y, 234);
        assert!(buttons_hit(250.0, SCALE_DENOMINATOR, &chrome));
        assert!(!buttons_hit(200.0, SCALE_DENOMINATOR, &chrome));
        assert!(chrome.deny.x + chrome.deny.width < 200);
        assert!(chrome.allow.x > 200);
    }

    #[test]
    fn two_pane_active_dividers_use_tmux_directional_halves() {
        let vertical = PaneDivider {
            axis: Axis::Horizontal,
            rect: Rect {
                x: 40,
                y: 0,
                width: 8,
                height: 100,
            },
        };
        let left = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 100,
        };
        let right = Rect {
            x: 48,
            y: 0,
            width: 52,
            height: 100,
        };
        assert_eq!(
            divider_active_segment(vertical, left, true),
            Some(Rect {
                height: 50,
                ..vertical.rect
            })
        );
        assert_eq!(
            divider_active_segment(vertical, right, true),
            Some(Rect {
                y: 50,
                height: 50,
                ..vertical.rect
            })
        );

        let horizontal = PaneDivider {
            axis: Axis::Vertical,
            rect: Rect {
                x: 0,
                y: 40,
                width: 100,
                height: 16,
            },
        };
        let top = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 40,
        };
        let bottom = Rect {
            x: 0,
            y: 56,
            width: 100,
            height: 44,
        };
        assert_eq!(
            divider_active_segment(horizontal, top, true),
            Some(Rect {
                width: 50,
                ..horizontal.rect
            })
        );
        assert_eq!(
            divider_active_segment(horizontal, bottom, true),
            Some(Rect {
                x: 50,
                width: 50,
                ..horizontal.rect
            })
        );
    }

    #[test]
    fn odd_shared_dividers_assign_extra_unit_to_leading_half() {
        let vertical = PaneDivider {
            axis: Axis::Horizontal,
            rect: Rect {
                x: 40,
                y: 10,
                width: 8,
                height: 5,
            },
        };
        let left = Rect {
            x: 0,
            y: 10,
            width: 40,
            height: 5,
        };
        let right = Rect {
            x: 48,
            y: 10,
            width: 52,
            height: 5,
        };
        assert_eq!(
            divider_active_segment(vertical, left, true),
            Some(Rect {
                height: 3,
                ..vertical.rect
            })
        );
        assert_eq!(
            divider_active_segment(vertical, right, true),
            Some(Rect {
                y: 13,
                height: 2,
                ..vertical.rect
            })
        );

        let horizontal = PaneDivider {
            axis: Axis::Vertical,
            rect: Rect {
                x: 10,
                y: 40,
                width: 5,
                height: 8,
            },
        };
        let top = Rect {
            x: 10,
            y: 0,
            width: 5,
            height: 40,
        };
        let bottom = Rect {
            x: 10,
            y: 48,
            width: 5,
            height: 52,
        };
        assert_eq!(
            divider_active_segment(horizontal, top, true),
            Some(Rect {
                width: 3,
                ..horizontal.rect
            })
        );
        assert_eq!(
            divider_active_segment(horizontal, bottom, true),
            Some(Rect {
                x: 13,
                width: 2,
                ..horizontal.rect
            })
        );
    }

    #[test]
    fn nested_active_dividers_keep_overlap_and_directional_junction_arms() {
        let vertical = PaneDivider {
            axis: Axis::Horizontal,
            rect: Rect {
                x: 40,
                y: 0,
                width: 8,
                height: 100,
            },
        };
        let horizontal = PaneDivider {
            axis: Axis::Vertical,
            rect: Rect {
                x: 48,
                y: 40,
                width: 52,
                height: 16,
            },
        };
        let active_top_right = Rect {
            x: 48,
            y: 0,
            width: 52,
            height: 40,
        };
        let vertical_segment = divider_active_segment(vertical, active_top_right, false).unwrap();
        let horizontal_segment =
            divider_active_segment(horizontal, active_top_right, false).unwrap();
        assert_eq!(
            vertical_segment,
            Rect {
                y: 0,
                height: 40,
                ..vertical.rect
            }
        );
        assert_eq!(horizontal_segment, horizontal.rect);

        let (character, junction) = divider_junction(vertical, horizontal).unwrap();
        assert_eq!(character, '├');
        assert_eq!(
            active_junction_arm_clips(vertical, vertical_segment, junction),
            vec![(
                '│',
                Rect {
                    height: 8,
                    ..junction
                }
            )]
        );
        assert_eq!(
            active_junction_arm_clips(horizontal, horizontal_segment, junction),
            vec![(
                '─',
                Rect {
                    x: 44,
                    width: 4,
                    ..junction
                }
            )]
        );
    }

    fn scaled_junction_layout() -> (PaneLayout, SplintId) {
        let left_id = SplintId::new();
        let active_id = SplintId::new();
        let bottom_id = SplintId::new();
        let left = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 100,
        };
        let top_right = Rect {
            x: 48,
            y: 0,
            width: 52,
            height: 40,
        };
        let bottom_right = Rect {
            x: 48,
            y: 56,
            width: 52,
            height: 44,
        };
        let panes = [
            (left_id, left),
            (active_id, top_right),
            (bottom_id, bottom_right),
        ]
        .map(|(splint_id, rect)| PaneGeometry {
            splint_id,
            rect,
            allocation: rect,
            extent: rect,
        })
        .to_vec();
        let separators = vec![
            PaneDivider {
                axis: Axis::Horizontal,
                rect: Rect {
                    x: 40,
                    y: 0,
                    width: 8,
                    height: 100,
                },
            },
            PaneDivider {
                axis: Axis::Vertical,
                rect: Rect {
                    x: 48,
                    y: 40,
                    width: 52,
                    height: 16,
                },
            },
        ];
        (
            PaneLayout {
                panes,
                separators,
                splits: Vec::new(),
                chrome: PaneChrome::Line {
                    vertical_width: 8,
                    horizontal_height: 16,
                },
                gap: 0,
                area: Rect {
                    x: 0,
                    y: 0,
                    width: 100,
                    height: 100,
                },
            },
            active_id,
        )
    }

    fn rect_has_color(canvas: &[u8], canvas_width: u32, rect: Rect, color: [u8; 4]) -> bool {
        (rect.y..rect.y + rect.height).any(|y| {
            (rect.x..rect.x + rect.width).any(|x| {
                let index = usize::try_from((y * canvas_width + x) * 4).unwrap();
                canvas[index..index + 4] == color
            })
        })
    }

    #[test]
    fn scaled_junction_paint_preserves_inactive_arm_and_overlays_active_arms() {
        let (layout, active_id) = scaled_junction_layout();
        let width = 125;
        let height = 125;
        let mut canvas = vec![0; usize::try_from(width * height * 4).unwrap()];
        let theme = ResolvedTheme {
            roles: crate::theme::ChromeRoles {
                divider: 0x11_22_33,
                focus_ring: 0xcc_88_44,
                ..crate::theme::ChromeRoles::default()
            },
            ..ResolvedTheme::default()
        };
        paint_pane_chrome(
            &mut canvas,
            width,
            height,
            &layout,
            Some(active_id),
            theme,
            10,
            20,
            150,
            &HashMap::new(),
        )
        .unwrap();

        let inactive = [0x33, 0x22, 0x11, 0xff];
        let active = [0x44, 0x88, 0xcc, 0xff];
        let top_arm = Rect {
            x: 50,
            y: 50,
            width: 10,
            height: 5,
        };
        let bottom_arm = Rect {
            x: 50,
            y: 65,
            width: 10,
            height: 5,
        };
        let right_arm = Rect {
            x: 57,
            y: 50,
            width: 3,
            height: 20,
        };

        // The 150/120 scale maps the logical junction to x=50..60, y=50..70.
        assert!(rect_has_color(&canvas, width, top_arm, active));
        assert!(rect_has_color(&canvas, width, bottom_arm, inactive));
        assert!(!rect_has_color(&canvas, width, bottom_arm, active));
        assert!(rect_has_color(&canvas, width, right_arm, active));
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one gapped layout checks junctions, active arms and every empty gap strip"
    )]
    fn gapped_line_dividers_meet_and_leave_the_gap_to_the_background() {
        let leaf = || {
            let splint = splinterm_core::Splint::shell(std::path::PathBuf::from("/tmp"));
            let id = splint.id;
            (splinterm_core::LayoutNode::Leaf(splint), id)
        };
        let (left, left_id) = leaf();
        let (top, top_id) = leaf();
        let (bottom, bottom_id) = leaf();
        let root = splinterm_core::LayoutNode::Branch {
            axis: Axis::Horizontal,
            ratio: splinterm_core::SplitRatio::new(500).unwrap(),
            first: Box::new(left),
            second: Box::new(splinterm_core::LayoutNode::Branch {
                axis: Axis::Vertical,
                ratio: splinterm_core::SplitRatio::new(500).unwrap(),
                first: Box::new(top),
                second: Box::new(bottom),
            }),
        };
        let (width, height) = (200, 160);
        let layout = PaneLayout::compute_with_gap(
            &root,
            Rect {
                x: 0,
                y: 0,
                width,
                height,
            },
            PaneChrome::Line {
                vertical_width: 8,
                horizontal_height: 16,
            },
            8,
            10,
            10,
        )
        .unwrap();
        let [vertical, horizontal] = [layout.separators[0], layout.separators[1]];
        let (character, junction) = divider_junction(vertical, horizontal).unwrap();
        assert_eq!(character, '├');
        assert_eq!(junction.x, vertical.rect.x);
        assert_eq!(junction.y, horizontal.rect.y);

        // The active border runs along the whole pane, half gaps included, so
        // it still reaches the junction.
        let active = layout.extent(top_id).unwrap();
        let vertical_segment = divider_active_segment(vertical, active, false).unwrap();
        // The panes and lines start inside the 8 px outer gap.
        assert_eq!(vertical_segment.y, 8);
        assert_eq!(
            vertical_segment.y + vertical_segment.height,
            horizontal.rect.y
        );
        assert_eq!(
            divider_active_segment(horizontal, active, false),
            Some(horizontal.rect)
        );
        let left_extent = layout.extent(left_id).unwrap();
        assert!(divider_active_segment(horizontal, left_extent, false).is_none());

        let mut canvas = vec![0; usize::try_from(width * height * 4).unwrap()];
        let theme = ResolvedTheme {
            roles: crate::theme::ChromeRoles {
                divider: 0x11_22_33,
                focus_ring: 0xcc_88_44,
                ..crate::theme::ChromeRoles::default()
            },
            ..ResolvedTheme::default()
        };
        paint_pane_chrome(
            &mut canvas,
            width,
            height,
            &layout,
            Some(top_id),
            theme,
            8,
            16,
            120,
            &HashMap::new(),
        )
        .unwrap();
        let empty = |rect: Rect| {
            (rect.y..rect.y + rect.height).all(|y| {
                (rect.x..rect.x + rect.width).all(|x| {
                    let index = usize::try_from((y * width + x) * 4).unwrap();
                    canvas[index..index + 4] == [0, 0, 0, 0]
                })
            })
        };
        let allocation = |id| {
            layout
                .panes
                .iter()
                .find(|pane| pane.splint_id == id)
                .unwrap()
                .allocation
        };
        let (left, top, bottom) = (
            allocation(left_id),
            allocation(top_id),
            allocation(bottom_id),
        );
        // Nothing is painted in the gaps beside the vertical line, or between
        // the horizontal line and the panes above and below it.
        let vertical_right = vertical.rect.x + vertical.rect.width;
        assert!(empty(Rect {
            x: left.x + left.width,
            width: vertical.rect.x - (left.x + left.width),
            ..vertical.rect
        }));
        assert!(empty(Rect {
            x: vertical_right,
            y: 0,
            width: top.x - vertical_right,
            height: horizontal.rect.y,
        }));
        assert!(empty(Rect {
            x: top.x,
            y: top.y + top.height,
            width: top.width,
            height: horizontal.rect.y - (top.y + top.height),
        }));
        let horizontal_bottom = horizontal.rect.y + horizontal.rect.height;
        assert!(empty(Rect {
            x: bottom.x,
            y: horizontal_bottom,
            width: bottom.width,
            height: bottom.y - horizontal_bottom,
        }));
        // The outer gap around the pane area stays empty on all four sides.
        for outer in [
            Rect {
                x: 0,
                y: 0,
                width,
                height: 8,
            },
            Rect {
                x: 0,
                y: height - 8,
                width,
                height: 8,
            },
            Rect {
                x: 0,
                y: 0,
                width: 8,
                height,
            },
            Rect {
                x: width - 8,
                y: 0,
                width: 8,
                height,
            },
        ] {
            assert!(empty(outer), "{outer:?}");
        }
        assert_eq!((left.x, left.y, top.y), (8, 8, 8));
        assert_eq!(bottom.y + bottom.height, height - 8);
        assert_eq!(top.x + top.width, width - 8);
        // The horizontal line crosses its half of the gap to the vertical line
        // in the active colour.
        assert!(rect_has_color(
            &canvas,
            width,
            Rect {
                x: vertical_right,
                width: top.x - vertical_right,
                ..horizontal.rect
            },
            [0x44, 0x88, 0xcc, 0xff],
        ));
    }

    /// Each theme source's trusted dialog: background, text, border, deny
    /// area, allow area, and the dimmed allow area.
    const CONSENT_PINS: [(&str, [u32; 6]); 9] = [
        (
            "builtin",
            [
                0x000e_1216,
                0xeb_eb_eb,
                0xd7_99_21,
                0xce_43_3f,
                0x20_70_40,
                0x14_3c_25,
            ],
        ),
        (
            "json",
            [
                0x10_18_20, 0xe0_e4_ea, 0xcc_aa_33, 0xcc_33_44, 0x44_aa_55, 0x25_5c_33,
            ],
        ),
        (
            "json-explicit",
            [
                0x10_18_20, 0xe0_e4_ea, 0xcc_aa_33, 0xcc_33_44, 0x44_aa_55, 0x25_5c_33,
            ],
        ),
        (
            "tokyo-night",
            [
                0x1a_1b_26, 0xa9_b1_d6, 0xe0_af_68, 0xf7_76_8e, 0x9e_ce_6a, 0x56_70_40,
            ],
        ),
        (
            "flexoki-light",
            [
                0xff_fc_f0, 0x10_0f_0f, 0xc0_88_28, 0xd1_4d_41, 0x87_9a_39, 0xe8_e8_d5,
            ],
        ),
        (
            "last-horizon",
            [
                0x0c_0b_0c, 0xfa_fc_fb, 0x6b_5e_73, 0xc3_8b_7b, 0x20_70_40, 0x13_3a_21,
            ],
        ),
        (
            "legacy-gruvbox",
            [
                0x28_28_28, 0xd4_be_98, 0xd8_a6_57, 0xea_69_62, 0xa9_b6_65, 0x5f_66_3e,
            ],
        ),
        (
            "alacritty-kanagawa",
            [
                0x1f_1f_28, 0xdc_d7_ba, 0xc0_a3_6e, 0xc3_40_43, 0x76_94_6a, 0x43_52_40,
            ],
        ),
        (
            "semantic-keys",
            [
                0x14_11_1f, 0xd8_d4_ea, 0xff_b3_47, 0xff_5f_6d, 0x5f_d7_8f, 0x34_74_4f,
            ],
        ),
    ];

    fn consent_pins(colors: TrustedConsentColors) -> [u32; 6] {
        [
            colors.background,
            colors.text,
            colors.border,
            colors.deny,
            colors.allow,
            colors.allow_dimmed,
        ]
    }

    /// The dialog's frame and areas keep 3:1 and its text 4.5:1 against its
    /// background, and the areas stay red and green as its text says.
    fn assert_consent_colors_hold(name: &str, colors: TrustedConsentColors) {
        let background = colors.background;
        assert!(
            contrast_ratio(colors.text, background) >= TEXT_MIN_CONTRAST,
            "{name}"
        );
        for color in [colors.border, colors.deny, colors.allow] {
            assert!(
                contrast_ratio(color, background) >= NON_TEXT_MIN_CONTRAST,
                "{name}: {color:06x} on {background:06x}"
            );
        }
        assert!(hue_degrees(colors.deny).is_some_and(is_red_hue), "{name}");
        assert!(
            hue_degrees(colors.allow).is_some_and(is_green_hue),
            "{name}"
        );
        assert!(
            hue_distance(colors.deny, colors.allow).is_some_and(|d| d >= MIN_AREA_HUE_DISTANCE),
            "{name}: {:06x} and {:06x} hues",
            colors.deny,
            colors.allow
        );
        assert!(
            contrast_ratio(colors.deny, colors.allow) >= MIN_AREA_CONTRAST - 0.01,
            "{name}: {:06x} and {:06x} lightness",
            colors.deny,
            colors.allow
        );
        assert!(
            frame_apart(colors.border, colors.deny) && frame_apart(colors.border, colors.allow),
            "{name}: frame {:06x}",
            colors.border
        );
        assert!(
            contrast_ratio(colors.allow_dimmed, background)
                < contrast_ratio(colors.allow, background),
            "{name}"
        );
        // Every text keeps 4.5:1 on what it sits on: the background, the
        // selection fill, or its button's tint.
        for (what, text, under) in [
            ("text", colors.text, background),
            ("secondary", colors.secondary, background),
            ("title", colors.title, background),
            ("selected", colors.selected_text, colors.selection),
            ("deny label", colors.deny_label, colors.deny_tint),
            ("allow label", colors.allow_label, colors.allow_tint),
            ("deny keys", colors.deny_keys, colors.deny_tint),
            ("allow keys", colors.allow_keys, colors.allow_tint),
            ("allow off", colors.allow_off, colors.allow_tint_dimmed),
        ] {
            assert!(
                contrast_ratio(text, under) >= TEXT_MIN_CONTRAST,
                "{name}: {what} {text:06x} on {under:06x}"
            );
        }
        // The selection shows as a row, and each button as a fill.
        assert!(
            contrast_ratio(colors.selection, background) >= MIN_SELECTION_CONTRAST - 0.01,
            "{name}: selection {:06x}",
            colors.selection
        );
        assert_ne!(colors.deny_tint, background, "{name}");
        assert_ne!(colors.allow_tint, background, "{name}");
        assert_ne!(colors.deny_tint, colors.allow_tint, "{name}");
    }

    /// The dialog's theme carries its inks in the palette slots its
    /// snapshot's cells name, so the text the Window draws is in the colors
    /// the guard checked.
    #[test]
    fn trusted_dialog_theme_fills_the_dialog_inks() {
        for (name, theme) in crate::config::tests::provider_themes() {
            let colors = TrustedConsentColors::from_theme(theme);
            let dialog = trusted_dialog_theme(theme);
            for ink in DialogInk::SLOTTED {
                let slot = ink.slot().unwrap();
                assert_eq!(dialog.ansi[slot], colors.ink(ink), "{name}: {ink:?}");
            }
            assert_eq!(dialog.ansi[SELECTION_SLOT], colors.selection, "{name}");
            assert_eq!(dialog.foreground, colors.text, "{name}");
        }
        assert_eq!(DialogInk::Text.slot(), None);
    }

    #[test]
    fn trusted_dialogs_take_their_theme_colors() {
        let themes = crate::config::tests::provider_themes();
        let painted = themes
            .iter()
            .map(|(name, theme)| {
                (
                    *name,
                    consent_pins(TrustedConsentColors::from_theme(*theme)),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(painted, CONSENT_PINS, "{painted:#08x?}");
        for (name, theme) in themes {
            let colors = TrustedConsentColors::from_theme(theme);
            assert_eq!(colors.background, theme.background, "{name}");
            assert_consent_colors_hold(name, colors);
        }
    }

    /// A danger that is not red, or a success that is not green, gives way
    /// to Splinterm's own, since the dialog's text names the areas by color.
    #[test]
    fn trusted_dialog_areas_stay_red_and_green() {
        let builtin = ResolvedTheme::default();
        for (danger, success) in [
            // Swapped.
            (0x44_cc_55, 0xee_44_44),
            // Grey and blue.
            (0x88_88_88, 0x33_66_ee),
            // Yellow and magenta.
            (0xee_cc_33, 0xcc_33_cc),
        ] {
            let mut theme = builtin;
            theme.roles.danger = danger;
            theme.roles.success = success;
            let colors = TrustedConsentColors::from_theme(theme);
            assert_ne!(colors.deny, danger, "{danger:06x}");
            assert_ne!(colors.allow, success, "{success:06x}");
            assert_consent_colors_hold("fallback", colors);
        }
        // Light backgrounds keep the fallback red and green readable too.
        let mut light = builtin;
        light.background = 0xff_fc_f0;
        light.roles.danger = 0x33_66_ee;
        light.roles.success = 0x33_66_ee;
        assert_consent_colors_hold("light fallback", TrustedConsentColors::from_theme(light));
        // Hues near each boundary.
        assert!(hue_degrees(0xf9_26_72).is_some_and(is_red_hue));
        assert!(hue_degrees(0xb8_bb_26).is_some_and(is_green_hue));
        assert!(!hue_degrees(0xff_80_00).is_some_and(is_red_hue));
        assert_eq!(hue_degrees(0x80_80_80), None);
    }

    /// Whatever the background and roles, the dialog keeps its floors, its
    /// areas red and green, apart from each other in hue and lightness and
    /// apart from the frame. Where a mid-tone background leaves no room for
    /// that, the whole dialog takes the fixed palette. Its colors written
    /// back into the theme give the same colors again.
    #[test]
    fn trusted_dialog_colors_hold_on_every_background() {
        let mut backgrounds = (0..=0xff_u32)
            .step_by(0x11)
            .map(|grey| grey << 16 | grey << 8 | grey)
            .collect::<Vec<_>>();
        backgrounds.extend([
            0x00_28_28_28,
            0x00_40_40_40,
            0x00_70_70_70,
            0x00_80_20_20,
            0x00_20_70_30,
            0x00_d0_c0_40,
            0x00_30_30_90,
            0x00_ff_fc_f0,
        ]);
        let roles = [
            // The theme's own.
            (0x00cc_241d, 0x0098_971a, 0x00d7_9921),
            // Swapped.
            (0x0044_cc55, 0x00ee_4444, 0x00ff_aa00),
            // No hue at all.
            (0x0088_8888, 0x0088_8888, 0x0088_8888),
            // Near misses: orange and yellow areas, a yellow frame.
            (0x00ff_7000, 0x00e0_e000, 0x00ff_ff00),
            // Pink and yellow-green, a frame matching the allow area.
            (0x00ff_00aa, 0x0080_ff00, 0x0080_ff00),
        ];
        let mut fixed = 0;
        for background in backgrounds {
            for (danger, success, warning) in roles {
                let mut theme = ResolvedTheme {
                    background,
                    ..ResolvedTheme::default()
                };
                theme.roles.danger = danger;
                theme.roles.success = success;
                theme.roles.warning = warning;
                let name = format!("{background:06x} {danger:06x}/{success:06x}/{warning:06x}");
                let colors = TrustedConsentColors::from_theme(theme);
                assert_consent_colors_hold(&name, colors);
                assert!(
                    colors.background == background || colors == TrustedConsentColors::fixed(),
                    "{name}"
                );
                fixed += usize::from(colors.background != background);
                assert_eq!(
                    TrustedConsentColors::from_theme(trusted_dialog_theme(theme)),
                    colors,
                    "{name}"
                );
            }
        }
        // Mid-tone greys leave no room, so some combinations fall back.
        assert!(fixed > 0);
        assert_eq!(TrustedConsentColors::fixed().background, FIXED_BACKGROUND);
        let grey = ResolvedTheme {
            background: 0x00_70_70_70,
            ..ResolvedTheme::default()
        };
        assert_eq!(
            TrustedConsentColors::from_theme(grey),
            TrustedConsentColors::fixed()
        );
    }

    /// `[colors] danger`, `success`, `warning` and `text` recolour the
    /// dialog, within its floors.
    #[test]
    fn role_overrides_recolor_trusted_dialogs() {
        let theme = ResolvedTheme::from_input_with_colors(
            &crate::theme::ThemeInput::builtin(),
            &crate::config::tests::theme_colors(
                "[colors]\ndanger=#ff3355\nsuccess=#33dd66\nwarning=#ffaa00\ntext=#202020\n",
            ),
        );
        let colors = TrustedConsentColors::from_theme(theme);
        assert_eq!(colors.deny, 0xff_33_55);
        assert_eq!(colors.allow, 0x33_dd_66);
        assert_eq!(colors.border, 0xff_aa_00);
        // Text too dark for the dialog is raised to 4.5:1.
        assert_ne!(colors.text, 0x20_20_20);
        assert_consent_colors_hold("overrides", colors);
    }
}
