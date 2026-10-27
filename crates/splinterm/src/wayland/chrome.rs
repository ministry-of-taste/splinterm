//! Trusted consent and pane chrome painting.

use std::{collections::HashMap, time::Instant};

use anyhow::{Context, Result};
use unicode_width::UnicodeWidthChar;

use crate::{
    color::{
        NON_TEXT_MIN_CONTRAST, contrast_corrected, contrast_floor_toward, mix_linear, rgb_channels,
        strongest_contrast_endpoint,
    },
    config::ResolvedTheme,
    frontend::{
        DialogFrame, DialogText, MINIMUM_ACTION_ROWS, action_area_top, dialog_font_step,
        dialog_text::{natural_columns, natural_rows, text_rows},
        dialog_text_rows,
    },
    geometry::{CellGeometry, Rect, TerminalPadding, WindowGeometry, logical_extent_to_buffer},
    pane::{PaneChrome, PaneDivider, PaneLayout},
    renderer::{ChromeText, background_bgra, paint_box_drawing_cell},
};
use splinterm_core::SplintId;
use splinterm_protocol::{MAX_COLUMNS, MAX_ROWS, TerminalSnapshot};

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
    /// The most rows of text that end above the shortest action areas.
    room_rows: usize,
}

impl TrustedFit {
    const fn fits(&self) -> bool {
        self.needed_rows <= self.room_rows
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
/// holds a grid of `cell` sized cells, exactly as the Window draws and hit
/// tests it: the same grid fit, the same translation, and the same
/// [`trusted_consent_action_top`]. `None` when the content cannot hold the
/// protocol minimum grid.
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
    let wrap_columns = natural_columns(variants).min(usize::try_from(geometry.columns).ok()?);
    Some(TrustedFit {
        geometry,
        wrap_columns,
        needed_rows: text_rows(variants, wrap_columns),
        room_rows: usize::try_from(trusted_consent_text_room(
            geometry.buffer_width(),
            geometry.buffer_height(),
            geometry.grid_rect.y,
            cell.height,
            geometry.rows,
        ))
        .ok()?,
    })
}

/// The most of a dialog's `rows` grid rows that can hold text and still end
/// above its action areas ([`trusted_consent_action_top`]).
pub(super) fn trusted_consent_text_room(
    width: u32,
    height: u32,
    grid_top: u32,
    cell_height: u32,
    rows: u32,
) -> u32 {
    (0..=rows)
        .rev()
        .find(|&text_rows| {
            let bottom = grid_top.saturating_add(text_rows.saturating_mul(cell_height));
            trusted_consent_action_top(width, height, grid_top, cell_height, text_rows) >= bottom
        })
        .unwrap_or(0)
}

/// Whether a click `y` logical pixels down a Window at `scale_120` lands in
/// action areas that begin `action_top` buffer pixels down.
fn in_action_area(y: f64, scale_120: u32, action_top: u32) -> bool {
    y * f64::from(scale_120) / f64::from(SCALE_DENOMINATOR) >= f64::from(action_top)
}

/// Where the action areas begin in a dialog drawn with `geometry` whose
/// snapshot shows `text_rows` rows of text: the one computation both
/// drawing and hit testing use.
fn geometry_action_top(
    width: u32,
    height: u32,
    geometry: &WindowGeometry,
    text_rows: usize,
) -> u32 {
    trusted_consent_action_top(
        width,
        height,
        geometry.grid_rect.y,
        geometry.cell.height,
        u32::try_from(text_rows)
            .unwrap_or(u32::MAX)
            .min(geometry.rows),
    )
}

/// Whether a click `y` logical pixels down a dialog drawn with `geometry`
/// at `scale_120`, showing `text_rows` rows of text, is in its action areas.
fn action_area_hit(y: f64, scale_120: u32, geometry: &WindowGeometry, text_rows: usize) -> bool {
    let top = geometry_action_top(
        geometry.buffer_width(),
        geometry.buffer_height(),
        geometry,
        text_rows,
    );
    in_action_area(y, scale_120, top)
}

/// The width of a trusted dialog's amber border, in buffer pixels.
fn trusted_consent_border(width: u32, height: u32) -> u32 {
    width.min(height).div_ceil(80).max(4)
}

/// Where a trusted dialog's action areas begin in its `width` by `height`
/// buffer, whose grid starts `grid_top` pixels down and shows `text_rows`
/// rows of text, each `cell_height` pixels tall: below the text, at least
/// [`MINIMUM_ACTION_ROWS`] rows above the border
/// ([`action_area_top`]).
pub(super) fn trusted_consent_action_top(
    width: u32,
    height: u32,
    grid_top: u32,
    cell_height: u32,
    text_rows: u32,
) -> u32 {
    let border = trusted_consent_border(width, height);
    let text_bottom = grid_top.saturating_add(text_rows.saturating_mul(cell_height));
    let minimum = cell_height
        .saturating_mul(MINIMUM_ACTION_ROWS)
        .saturating_add(border);
    action_area_top(height, text_bottom, minimum)
}

impl App {
    /// Where this trusted dialog's action areas begin, in buffer pixels, in
    /// a `width` by `height` buffer that shows `geometry`'s grid. Without a
    /// grid they take the bottom 22%.
    pub(super) fn trusted_consent_action_top(
        &self,
        width: u32,
        height: u32,
        geometry: Option<&WindowGeometry>,
    ) -> u32 {
        let Some(geometry) = geometry else {
            return trusted_consent_action_top(width, height, 0, 0, 0);
        };
        geometry_action_top(width, height, geometry, self.trusted_text_rows())
    }

    /// How many rows of the dialog's snapshot hold text.
    fn trusted_text_rows(&self) -> usize {
        self.panes
            .pane
            .snapshot
            .as_ref()
            .map_or(0, |snapshot| dialog_text_rows(&snapshot.visible_rows))
    }

    /// Whether a click `y` logical pixels down this trusted dialog falls in
    /// its action areas, exactly where [`paint_trusted_consent_chrome`]
    /// paints them.
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
        let Some(geometry) = geometry else {
            return y >= f64::from(self.surface.logical_height) * 0.78;
        };
        action_area_hit(y, scale_120, &geometry, self.trusted_text_rows())
    }

    /// Lays a trusted dialog's text out again whenever its Window's size,
    /// scale, or font changed since the last layout, before it is drawn.
    ///
    /// The dialog asks for one fixed size, the natural size of its text at
    /// the configured font, as both its minimum and maximum size; this keeps
    /// that request right at the current scale. When the compositor gives
    /// another size anyway (a tiling compositor that ignores the request),
    /// the text wraps to the columns that fit and, when it still runs into
    /// the action areas, the font steps down to no less than
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
        apply_theme(&mut snapshot, self.presentation.theme);
        let colors = TrustedConsentColors::from_theme(self.presentation.theme);
        snapshot.default_colors[0] = colors.text;
        snapshot.default_colors[1] = colors.background;
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
/// The dialog's text tells the person to click "the red left area" to say
/// no and "the green right area" to say yes, so the two areas must stay
/// recognisably red and green whatever the theme says: a `danger` or
/// `success` role that is not red or green (a theme or a `[colors]` key
/// that swaps them, or a monochrome palette) gives way to Splinterm's own
/// red and green. Every non-text color keeps 3:1 against the background and
/// the text keeps 4.5:1, moved toward black or white as needed, which keeps
/// their hue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TrustedConsentColors {
    /// The dialog's body, behind its text and between the action areas.
    pub(super) background: u32,
    /// The dialog's text.
    pub(super) text: u32,
    /// The frame: the `warning` role.
    pub(super) border: u32,
    /// The left area, which says no: the `danger` role.
    pub(super) deny: u32,
    /// The right area, which says yes: the `success` role.
    pub(super) allow: u32,
    /// The right area while the dialog cannot say yes (part of its text is
    /// hidden): `allow` faded most of the way into the background.
    pub(super) allow_dimmed: u32,
}

/// Splinterm's own red for the deny area, when the theme's `danger` is not red.
const FALLBACK_DENY: u32 = 0x0070_2020;
/// Splinterm's own green for the allow area, when the theme's `success` is
/// not green.
const FALLBACK_ALLOW: u32 = 0x0020_7040;
/// How far the dimmed allow area fades into the background.
const DIMMED_ALLOW_FADE: f64 = 0.75;

impl TrustedConsentColors {
    pub(super) fn from_theme(theme: ResolvedTheme) -> Self {
        let background = theme.background;
        let endpoint = strongest_contrast_endpoint(background);
        let non_text =
            |color| contrast_floor_toward(color, endpoint, background, NON_TEXT_MIN_CONTRAST);
        let area = |role, fallback, hue: fn(f64) -> bool| {
            let color = non_text(role);
            if hue_degrees(color).is_some_and(hue) {
                color
            } else {
                non_text(fallback)
            }
        };
        let allow = area(theme.roles.success, FALLBACK_ALLOW, is_green_hue);
        Self {
            background,
            text: contrast_corrected(theme.roles.text, background),
            border: non_text(theme.roles.warning),
            deny: area(theme.roles.danger, FALLBACK_DENY, is_red_hue),
            allow,
            allow_dimmed: mix_linear(allow, background, DIMMED_ALLOW_FADE),
        }
    }
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

/// Red, from crimson and rose to red-orange.
fn is_red_hue(hue: f64) -> bool {
    !(30.0..320.0).contains(&hue)
}

/// Green, from olive (Gruvbox's green) to teal-green.
fn is_green_hue(hue: f64) -> bool {
    (55.0..=170.0).contains(&hue)
}

/// Paints a trusted dialog's border and its red (left) and green (right)
/// action areas, which begin `button_top` pixels down
/// ([`App::trusted_consent_action_top`]), in `colors`. The gap between the
/// areas is filled with the background, so nothing drawn below
/// `button_top` shows through it. While the dialog cannot say yes (part of
/// its text is hidden), the green area is drawn dimmed.
pub(super) fn paint_trusted_consent_chrome(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    button_top: u32,
    colors: TrustedConsentColors,
    proceed_enabled: bool,
) {
    fn fill(canvas: &mut [u8], width: u32, x0: u32, y0: u32, x1: u32, y1: u32, rgb: u32) {
        let [_, red, green, blue] = rgb.to_be_bytes();
        for y in y0.min(y1)..y1 {
            for x in x0.min(x1)..x1 {
                let Ok(index) = usize::try_from((y * width + x) * 4) else {
                    continue;
                };
                if let Some(pixel) = canvas.get_mut(index..index + 4) {
                    pixel.copy_from_slice(&[blue, green, red, 0xff]);
                }
            }
        }
    }
    let border = trusted_consent_border(width, height);
    fill(canvas, width, 0, 0, width, border, colors.border);
    fill(
        canvas,
        width,
        0,
        height.saturating_sub(border),
        width,
        height,
        colors.border,
    );
    fill(canvas, width, 0, 0, border, height, colors.border);
    fill(
        canvas,
        width,
        width.saturating_sub(border),
        0,
        width,
        height,
        colors.border,
    );
    let middle = width / 2;
    fill(
        canvas,
        width,
        middle.saturating_sub(2),
        button_top,
        middle.saturating_add(2),
        height.saturating_sub(border),
        colors.background,
    );
    fill(
        canvas,
        width,
        border,
        button_top,
        middle.saturating_sub(2),
        height.saturating_sub(border),
        colors.deny,
    );
    fill(
        canvas,
        width,
        middle.saturating_add(2),
        button_top,
        width.saturating_sub(border),
        height.saturating_sub(border),
        if proceed_enabled {
            colors.allow
        } else {
            colors.allow_dimmed
        },
    );
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
    use crate::color::{TEXT_MIN_CONTRAST, contrast_ratio};
    use crate::frontend::{DialogLine, dialog_text};
    use crate::pane::PaneGeometry;
    use splinterm_core::Axis;

    /// The restart question's shape: prose paragraphs wrapped at 76, then
    /// the deadline and the three hint lines.
    fn restart_text() -> Vec<DialogText> {
        let prose = |text: &str| DialogLine::new(text).with_wrap_at(76);
        vec![DialogText {
            lines: vec![
                DialogLine::new("TRUSTED SPLINTERM UPGRADE QUESTION"),
                DialogLine::blank(),
                prose(
                    "The running Splinterm daemon does not speak this client's protocol. It is an older release, such as 0.1.x, or another incompatible build, so this client (Splinterm 0.2.1) cannot ask it for its shells or ask it to hand them over.",
                ),
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
                    "[D / Esc / Enter  Not now]     [R  Restart now (ends about 3 shells)]",
                ),
                DialogLine::hint(
                    "Click the red left area for Not now or the green right area to restart.",
                ),
                DialogLine::hint(
                    "Click inside this dialog or press Ctrl+Alt+G first to answer with the keyboard.",
                ),
            ],
            minimum_rows: 18,
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

    const RED: [u8; 4] = [0x20, 0x20, 0x70, 0xff];

    /// Every answer hint shows when the text fits; when it does not, the
    /// notice that the question cannot say yes takes their place.
    fn assert_answer_rows(
        dialog: &crate::frontend::TrustedConsentUi,
        shown: &[String],
        text_rows: usize,
        size: &str,
    ) {
        if dialog.clipped() {
            assert!(
                shown
                    .iter()
                    .any(|row| row.starts_with("Part of this question is hidden")),
                "{size}: {shown:#?}"
            );
            assert!(
                !shown.iter().any(|row| row.starts_with("[D / Esc")),
                "{size}"
            );
        } else {
            assert!(!shown.iter().any(|row| row == dialog_text::CLIPPED_MARKER));
            for hint in ["[D / Esc / Enter", "Click the red left area"] {
                assert!(
                    shown.iter().any(|row| row.starts_with(hint)),
                    "{size}: {hint}: {shown:#?}"
                );
            }
            assert!(
                shown[text_rows - 1].ends_with("keyboard."),
                "{size}: {shown:#?}"
            );
        }
    }

    /// At its natural size the dialog fits exactly, and in every smaller or
    /// odd Window (the 2026-10-22 tiles among them), at 1x and at 1.25x, its
    /// text is laid out again, ends above the action areas as they are
    /// painted, and a click (in logical pixels, converted to buffer pixels)
    /// is in an action area exactly where those areas are painted. Every
    /// size shows how to answer: the hints, or the notice that replaces
    /// them while text is hidden.
    #[test]
    fn relaid_text_stays_above_the_drawn_action_areas_and_clicks_agree() {
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
        assert!(natural_fit.fits());
        assert_eq!(natural_fit.wrap_columns, natural_columns(&variants));
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
                let text_rows = dialog_text_rows(&snapshot.visible_rows);
                let (buffer_width, buffer_height) =
                    (geometry.buffer_width(), geometry.buffer_height());
                let top = geometry_action_top(buffer_width, buffer_height, &geometry, text_rows);
                let text_bottom =
                    geometry.grid_rect.y + u32::try_from(text_rows).unwrap() * geometry.cell.height;
                assert!(
                    text_bottom <= top,
                    "{size}: text ends at {text_bottom}, actions at {top}"
                );
                // The painted red area and the hit test agree on every
                // buffer row above the bottom border.
                let mut canvas =
                    vec![0; usize::try_from(buffer_width * buffer_height * 4).unwrap()];
                paint_trusted_consent_chrome(
                    &mut canvas,
                    buffer_width,
                    buffer_height,
                    top,
                    FIXED_COLORS,
                    !dialog.clipped(),
                );
                let border = trusted_consent_border(buffer_width, buffer_height);
                let x = buffer_width / 4;
                for y in border..buffer_height - border {
                    let index = usize::try_from((y * buffer_width + x) * 4).unwrap();
                    let painted_red = canvas[index..index + 4] == RED;
                    let logical_y =
                        (f64::from(y) + 0.5) * f64::from(SCALE_DENOMINATOR) / f64::from(scale_120);
                    assert_eq!(
                        action_area_hit(logical_y, scale_120, &geometry, text_rows),
                        painted_red,
                        "{size} at buffer row {y}"
                    );
                }
                assert_answer_rows(&dialog, &shown_rows(&snapshot), text_rows, &size);
            }
        }
        // The reported 621x368 tile does not fit at this font, so it says
        // so and cannot restart.
        let mut dialog = restart_dialog();
        laid_out(&mut dialog, &fit_at(&variants, 621, 368, SCALE_DENOMINATOR));
        assert!(dialog.clipped());
    }

    /// A narrower Window with room enough rewraps and cuts nothing; one too
    /// short at this font marks the cut body and says it cannot say yes.
    #[test]
    fn narrow_windows_rewrap_and_short_ones_say_what_is_hidden() {
        let variants = restart_text();
        let narrow = fit_at(&variants, 621, 600, SCALE_DENOMINATOR);
        assert!(narrow.wrap_columns < natural_columns(&variants));
        assert!(narrow.fits(), "{narrow:?}");
        let short = fit_at(&variants, 621, 200, SCALE_DENOMINATOR);
        assert!(!short.fits());
        let mut dialog = restart_dialog();
        let shown = shown_rows(&laid_out(&mut dialog, &short));
        assert!(
            shown.iter().any(|row| row == dialog_text::CLIPPED_MARKER),
            "{shown:#?}"
        );
        assert!(
            shown
                .iter()
                .any(|row| row.starts_with("Part of this question is hidden")),
            "{shown:#?}"
        );
    }

    /// The font decision fed by the real fit: in the reported 621x368 tile
    /// a 9x19 cell does not fit and a 7x14 one does, so the dialog steps
    /// down to it, unless that is below 70% of the configured size.
    #[test]
    fn the_font_steps_down_to_a_cell_that_really_fits() {
        let variants = restart_text();
        let large = CellGeometry::from_metrics(9, 19, 15, 4, 15).unwrap();
        let small = CellGeometry::from_metrics(7, 14, 11, 3, 11).unwrap();
        let fits = |cell| {
            trusted_dialog_fit(
                content(621, 368),
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

    #[test]
    fn text_room_ends_above_the_shortest_action_areas() {
        // 368 px tall, 17 px rows from 4 px down: the border is 5 px, so
        // the shortest action areas take 2 * 17 + 5 px.
        let room = trusted_consent_text_room(621, 368, 4, 17, 21);
        assert_eq!(room, (368 - 39 - 4) / 17);
        let bottom = 4 + room * 17;
        assert!(trusted_consent_action_top(621, 368, 4, 17, room) >= bottom);
        assert!(trusted_consent_action_top(621, 368, 4, 17, room + 1) < bottom + 17);
        assert_eq!(trusted_consent_text_room(621, 368, 4, 17, 3), 3);
        assert_eq!(trusted_consent_text_room(100, 30, 4, 17, 1), 1);
    }

    /// Nothing drawn under the action areas shows between them.
    #[test]
    fn the_gap_between_the_action_areas_is_painted_over() {
        let (width, height) = (200, 100);
        let mut canvas = vec![0xff; usize::try_from(width * height * 4).unwrap()];
        paint_trusted_consent_chrome(&mut canvas, width, height, 70, FIXED_COLORS, true);
        let pixel = |x: u32, y: u32| {
            let index = usize::try_from((y * width + x) * 4).unwrap();
            [
                canvas[index],
                canvas[index + 1],
                canvas[index + 2],
                canvas[index + 3],
            ]
        };
        for y in 70..96 {
            for x in 98..102 {
                assert_eq!(pixel(x, y), [0x20, 0x18, 0x14, 0xff], "{x},{y}");
            }
        }
        assert_eq!(pixel(50, 80), [0x20, 0x20, 0x70, 0xff]);
        assert_eq!(pixel(150, 80), [0x40, 0x70, 0x20, 0xff]);
        // Above the action areas the canvas is untouched.
        assert_eq!(pixel(100, 60), [0xff, 0xff, 0xff, 0xff]);
        // A dialog that cannot say yes dims its green area.
        paint_trusted_consent_chrome(&mut canvas, width, height, 70, FIXED_COLORS, false);
        let index = usize::try_from((80 * width + 150) * 4).unwrap();
        assert_eq!(canvas[index..index + 4], [0x2a, 0x30, 0x24, 0xff]);
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

    /// The colors trusted dialogs painted before they followed the theme.
    const FIXED_COLORS: TrustedConsentColors = TrustedConsentColors {
        background: 0x0014_1820,
        text: 0x00f4_f0e8,
        border: 0x00e0_a030,
        deny: 0x0070_2020,
        allow: 0x0020_7040,
        allow_dimmed: 0x0024_302a,
    };

    /// Each theme source's trusted dialog: background, text, border, deny
    /// area, allow area, and the dimmed allow area.
    const CONSENT_PINS: [(&str, [u32; 6]); 9] = [
        (
            "builtin",
            [
                0x000e_1216,
                0xeb_eb_eb,
                0xd7_99_21,
                0xcc_24_1d,
                0x98_97_1a,
                0x51_51_17,
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
                0xff_fc_f0, 0x10_0f_0f, 0xb6_8d_14, 0xd1_4d_41, 0x87_9a_39, 0xe8_e8_d5,
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
            contrast_ratio(colors.allow_dimmed, background)
                < contrast_ratio(colors.allow, background),
            "{name}"
        );
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
        let fallback = TrustedConsentColors {
            deny: contrast_floor_toward(
                FALLBACK_DENY,
                0xff_ff_ff,
                builtin.background,
                NON_TEXT_MIN_CONTRAST,
            ),
            allow: contrast_floor_toward(
                FALLBACK_ALLOW,
                0xff_ff_ff,
                builtin.background,
                NON_TEXT_MIN_CONTRAST,
            ),
            ..TrustedConsentColors::from_theme(builtin)
        };
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
            assert_eq!(colors.deny, fallback.deny, "{danger:06x}");
            assert_eq!(colors.allow, fallback.allow, "{success:06x}");
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
