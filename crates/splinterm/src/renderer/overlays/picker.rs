//! Trusted inline session-picker layout and CPU composition.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    ops::Range,
};

use anyhow::Result;
use unicode_width::UnicodeWidthChar;

use crate::{
    color::{
        TEXT_MIN_CONTRAST, contrast_corrected, contrast_corrected_toward, contrast_ratio,
        mix_linear, packed_channels, relative_luminance, rgb_channels,
    },
    config::{OmarchyShellSettings, ResolvedTheme},
    frontend::PickerHitTarget,
    geometry::Rect,
    theme::ChromeTints,
};

use super::super::{
    ChromeSource, ChromeText, ChromeTextStyle, RenderContext, blend_rect, fill_rect,
    raster::premultiplied_rgba,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionPickerPresentationMode {
    Normal,
    Compact,
    Minimal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionPickerPurpose {
    RecentDojos,
    Dojos,
    Lairs,
    Remotes,
}

impl SessionPickerPurpose {
    pub(crate) const fn heading(self, minimal: bool) -> &'static str {
        match (self, minimal) {
            (Self::RecentDojos, _) => "RECENT DOJOS",
            (Self::Dojos, _) => "DOJOS",
            (Self::Lairs, _) => "LAIRS",
            (Self::Remotes, _) => "REMOTES",
        }
    }

    pub(crate) const fn guidance(self) -> &'static str {
        match self {
            Self::RecentDojos => "Open a recent running Dojo.",
            Self::Dojos => "Switch to a Dojo in this Lair.",
            Self::Lairs => "Switch to a Lair.",
            Self::Remotes => "Connect to a remote profile.",
        }
    }

    pub(crate) const fn new_title(self) -> &'static str {
        match self {
            Self::RecentDojos => "+ New terminal",
            Self::Dojos => "+ New Dojo",
            Self::Lairs => "+ New Lair",
            Self::Remotes => "+ Add a profile",
        }
    }

    pub(crate) const fn new_description(self) -> &'static str {
        match self {
            Self::RecentDojos => "Start a fresh shell",
            Self::Dojos => "Add a Dojo to this Lair",
            Self::Lairs => "Start a fresh workspace",
            Self::Remotes => "Edit remotes.toml",
        }
    }

    pub(crate) const fn selection_verb(self) -> &'static str {
        match self {
            Self::RecentDojos => "open",
            Self::Dojos | Self::Lairs => "switch",
            Self::Remotes => "connect",
        }
    }

    /// Remote profiles are created by editing `remotes.toml`, not from the picker.
    pub(crate) const fn offers_creation(self) -> bool {
        !matches!(self, Self::Remotes)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionPickerRowLayout {
    pub(crate) target: PickerHitTarget,
    pub(crate) rect: Rect,
    pub(crate) surface: Rect,
    pub(crate) title_clip: Rect,
    pub(crate) metadata_clip: Rect,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionPickerOverlayLayout {
    pub(crate) panel: Rect,
    pub(crate) header: Rect,
    pub(crate) action: Rect,
    pub(crate) list: Rect,
    pub(crate) footer: Rect,
    pub(crate) rows: Vec<SessionPickerRowLayout>,
    pub(crate) visible_range: Range<usize>,
    pub(crate) visible_capacity: usize,
    pub(crate) mode: SessionPickerPresentationMode,
    pub(crate) cache_key: (u32, u32, u32),
}

fn picker_item_rect(slot: Rect, mode: SessionPickerPresentationMode) -> Rect {
    let (horizontal_margin, vertical_margin) = match mode {
        SessionPickerPresentationMode::Normal => (8_u32, 3_u32),
        SessionPickerPresentationMode::Compact => (6_u32, 3_u32),
        SessionPickerPresentationMode::Minimal => (0_u32, 0_u32),
    };
    Rect {
        x: slot.x.saturating_add(horizontal_margin.min(slot.width / 2)),
        y: slot.y.saturating_add(vertical_margin.min(slot.height / 2)),
        width: slot
            .width
            .saturating_sub(horizontal_margin.saturating_mul(2)),
        height: slot
            .height
            .saturating_sub(vertical_margin.saturating_mul(2)),
    }
}

fn picker_row_layout(
    target: PickerHitTarget,
    slot: Rect,
    two_lines: bool,
    mode: SessionPickerPresentationMode,
) -> SessionPickerRowLayout {
    let surface = picker_item_rect(slot, mode);
    let horizontal_inset = 12_u32.min(surface.width / 4);
    let content_x = surface.x.saturating_add(horizontal_inset);
    let content_width = surface
        .width
        .saturating_sub(horizontal_inset.saturating_mul(2));
    let title_height = if two_lines {
        surface.height / 2
    } else {
        surface.height
    };
    SessionPickerRowLayout {
        target,
        rect: slot,
        surface,
        title_clip: Rect {
            x: content_x,
            y: surface.y,
            width: content_width,
            height: title_height,
        },
        metadata_clip: Rect {
            x: content_x,
            y: surface.y.saturating_add(title_height),
            width: content_width,
            height: surface.height.saturating_sub(title_height),
        },
    }
}

fn picker_visible_start(
    item_count: usize,
    selected_action: usize,
    requested_start: usize,
    capacity: usize,
) -> usize {
    if item_count == 0 || capacity == 0 {
        return 0;
    }
    let mut start = requested_start.min(item_count.saturating_sub(capacity));
    if selected_action > 0 {
        let selected_item = selected_action - 1;
        if selected_item < start {
            start = selected_item;
        } else if selected_item >= start.saturating_add(capacity) {
            start = selected_item.saturating_add(1).saturating_sub(capacity);
        }
    }
    start.min(item_count.saturating_sub(capacity))
}

#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "the specialized responsive layout keeps all mode invariants in one pure seam"
)]
pub(crate) fn session_picker_overlay_layout(
    logical_width: u32,
    logical_height: u32,
    scale_120: u32,
    item_count: usize,
    selected_action: usize,
    requested_start: usize,
) -> Option<SessionPickerOverlayLayout> {
    if logical_width == 0 || logical_height == 0 || scale_120 == 0 {
        return None;
    }
    let preferred_mode = if logical_width >= 480 && logical_height >= 320 {
        SessionPickerPresentationMode::Normal
    } else if logical_width >= 280 && logical_height >= 180 {
        SessionPickerPresentationMode::Compact
    } else {
        SessionPickerPresentationMode::Minimal
    };
    let (margin, header_height, row_height, footer_height): (u32, u32, u32, u32) =
        match preferred_mode {
            SessionPickerPresentationMode::Normal => (16, 64, 56, 40),
            SessionPickerPresentationMode::Compact => (8, 44, 44, 36),
            SessionPickerPresentationMode::Minimal => (0, 32, 44, 28),
        };
    let available_width = logical_width.saturating_sub(margin.saturating_mul(2));
    let available_height = logical_height.saturating_sub(margin.saturating_mul(2));
    if available_width == 0 || available_height == 0 {
        return None;
    }
    let panel_width = available_width.min(680);
    let normal_fixed = header_height
        .saturating_add(row_height)
        .saturating_add(footer_height);
    let computed_capacity =
        usize::try_from(available_height.saturating_sub(normal_fixed) / row_height.max(1))
            .unwrap_or(0);
    let mode = if preferred_mode != SessionPickerPresentationMode::Minimal
        && item_count > 0
        && computed_capacity == 0
    {
        SessionPickerPresentationMode::Minimal
    } else {
        preferred_mode
    };
    if mode == SessionPickerPresentationMode::Minimal {
        let action_height = if available_height >= 104 {
            available_height.saturating_sub(60)
        } else {
            available_height.min(44)
        }
        .max(1);
        let chrome_height = available_height.saturating_sub(action_height);
        let header_height = 32.min(chrome_height.div_ceil(2));
        let footer_height = chrome_height.saturating_sub(header_height);
        let panel_height = header_height
            .saturating_add(action_height)
            .saturating_add(footer_height)
            .min(available_height);
        let panel = Rect {
            x: (logical_width.saturating_sub(panel_width)) / 2,
            y: (logical_height.saturating_sub(panel_height)) / 2,
            width: panel_width,
            height: panel_height,
        };
        let header = Rect {
            x: panel.x,
            y: panel.y,
            width: panel.width,
            height: header_height,
        };
        let action = Rect {
            x: panel.x,
            y: header.y.saturating_add(header.height),
            width: panel.width,
            height: action_height,
        };
        let footer = Rect {
            x: panel.x,
            y: action.y.saturating_add(action.height),
            width: panel.width,
            height: footer_height,
        };
        let target = if selected_action == 0 || item_count == 0 {
            PickerHitTarget::New
        } else {
            PickerHitTarget::Open((selected_action - 1).min(item_count - 1))
        };
        let visible_range = match target {
            PickerHitTarget::New => 0..0,
            PickerHitTarget::Open(index) => index..index.saturating_add(1),
        };
        return Some(SessionPickerOverlayLayout {
            panel,
            header,
            action,
            list: action,
            footer,
            rows: vec![picker_row_layout(target, action, false, mode)],
            visible_range,
            visible_capacity: usize::from(item_count > 0),
            mode,
            cache_key: (logical_width, logical_height, scale_120),
        });
    }

    let capacity = computed_capacity.max(usize::from(item_count > 0));
    let visible_count = item_count.min(capacity);
    let panel_height = normal_fixed
        .saturating_add(row_height.saturating_mul(u32::try_from(visible_count).unwrap_or(u32::MAX)))
        .min(available_height);
    let panel = Rect {
        x: (logical_width.saturating_sub(panel_width)) / 2,
        y: (logical_height.saturating_sub(panel_height)) / 2,
        width: panel_width,
        height: panel_height,
    };
    let header = Rect {
        x: panel.x,
        y: panel.y,
        width: panel.width,
        height: header_height,
    };
    let action = Rect {
        x: panel.x,
        y: header.y.saturating_add(header.height),
        width: panel.width,
        height: row_height,
    };
    let list = Rect {
        x: panel.x,
        y: action.y.saturating_add(action.height),
        width: panel.width,
        height: row_height.saturating_mul(u32::try_from(visible_count).unwrap_or(u32::MAX)),
    };
    let footer = Rect {
        x: panel.x,
        y: list.y.saturating_add(list.height),
        width: panel.width,
        height: footer_height,
    };
    let start = picker_visible_start(item_count, selected_action, requested_start, visible_count);
    let visible_range = start..start.saturating_add(visible_count);
    let mut rows = Vec::with_capacity(visible_count.saturating_add(1));
    rows.push(picker_row_layout(PickerHitTarget::New, action, false, mode));
    for (slot, index) in visible_range.clone().enumerate() {
        rows.push(picker_row_layout(
            PickerHitTarget::Open(index),
            Rect {
                x: list.x,
                y: list.y.saturating_add(
                    row_height.saturating_mul(u32::try_from(slot).unwrap_or(u32::MAX)),
                ),
                width: list.width,
                height: row_height,
            },
            mode == SessionPickerPresentationMode::Normal,
            mode,
        ));
    }
    Some(SessionPickerOverlayLayout {
        panel,
        header,
        action,
        list,
        footer,
        rows,
        visible_range,
        visible_capacity: visible_count,
        mode,
        cache_key: (logical_width, logical_height, scale_120),
    })
}

#[must_use]
pub(crate) fn session_picker_hit_test(
    layout: &SessionPickerOverlayLayout,
    position: (f64, f64),
) -> Option<PickerHitTarget> {
    layout.rows.iter().find_map(|row| {
        let right = f64::from(row.rect.x.saturating_add(row.rect.width));
        let bottom = f64::from(row.rect.y.saturating_add(row.rect.height));
        (position.0 >= f64::from(row.rect.x)
            && position.0 < right
            && position.1 >= f64::from(row.rect.y)
            && position.1 < bottom)
            .then_some(row.target)
    })
}

/// Corrects `foreground` toward the strongest endpoint until it keeps the floor
/// against every one of `backgrounds`. `None` when a few passes cannot satisfy
/// them all, as when they pull toward opposite endpoints.
fn contrast_corrected_against_all(foreground: u32, backgrounds: &[u32]) -> Option<u32> {
    let readable = |color: u32| {
        backgrounds
            .iter()
            .all(|&background| contrast_ratio(color, background) >= PANEL_TEXT_MIN_CONTRAST)
    };
    let mut corrected = foreground;
    for _ in 0..4 {
        if readable(corrected) {
            return Some(corrected);
        }
        for &background in backgrounds {
            corrected = contrast_corrected(corrected, background);
        }
    }
    readable(corrected).then_some(corrected)
}

/// WCAG AA for normal text, held for panel text over any backdrop.
const PANEL_TEXT_MIN_CONTRAST: f64 = TEXT_MIN_CONTRAST;

/// The Omarchy shell surface whose `background-alpha` a panel follows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChromeSurface {
    /// `[menu]`: list cards with rows and a selected row. The action
    /// palette and the Dojo and session picker; the docked Lair Explorer takes
    /// these colours but the Window's own background alpha.
    Menu,
    /// `[popups]`: flyouts, prompts and status cards. The tab context menu,
    /// the Dojo prompt, the search bar and the history overlay.
    Popup,
}

impl ChromeSurface {
    pub(crate) const fn shell_alpha(self, shell: OmarchyShellSettings) -> u16 {
        match self {
            Self::Menu => shell.menu_alpha,
            Self::Popup => shell.popup_alpha,
        }
    }

    /// The foregrounds painted directly on this surface's panel fill. Menus
    /// (the action palette, pickers and Explorer) draw primary and secondary
    /// text there; the accent rail only marks rows. Popups also draw the
    /// accent as text: the Dojo prompt's unselected destructive choice.
    fn panel_foregrounds(self, palette: &SessionPickerPalette) -> Vec<u32> {
        match self {
            Self::Menu => vec![palette.primary, palette.secondary],
            Self::Popup => vec![palette.primary, palette.secondary, palette.selected_rail],
        }
    }

    /// Every way text meets this surface's panel fill: straight on it, and on
    /// an unselected row under the translucent hover highlight, the press
    /// highlight, or both. The action palette, the session picker and the tab
    /// context menu blend those highlights before painting primary or
    /// secondary row text; selected rows sit on the opaque selected fill.
    fn panel_text_layers(self, palette: &SessionPickerPalette) -> Vec<PanelTextLayer> {
        let row_text = vec![palette.highlight_primary, palette.highlight_secondary];
        let mut layers = vec![PanelTextLayer {
            overlays: Vec::new(),
            text: self.panel_foregrounds(palette),
        }];
        layers.extend(
            highlight_overlays(palette)
                .into_iter()
                .map(|overlays| PanelTextLayer {
                    overlays,
                    text: row_text.clone(),
                }),
        );
        layers
    }
}

/// Mixes two colours in encoded sRGB, the space the compositor blends in.
#[cfg(test)]
fn mix_encoded(first: u32, second: u32, second_weight: f64) -> u32 {
    let first = rgb_channels(first);
    let second = rgb_channels(second);
    packed_channels(std::array::from_fn(|index| {
        let mixed = f64::from(first[index]) * (1.0 - second_weight)
            + f64::from(second[index]) * second_weight;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let mixed = mixed.round().clamp(0.0, 255.0) as u8;
        mixed
    }))
}

/// Alpha, out of 255, of the translucent highlight blended over a hovered row.
pub(crate) const ROW_HOVER_ALPHA: u8 = 64;
/// Alpha, out of 255, of the translucent highlight blended over a pressed row.
pub(crate) const ROW_PRESS_ALPHA: u8 = 48;

/// One way text appears on a panel fill: the translucent `overlays`
/// (`(colour, alpha out of 255)`, in paint order) blended over the panel first,
/// then the `text` colours painted on top.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PanelTextLayer {
    overlays: Vec<(u32, u8)>,
    text: Vec<u32>,
}

/// The colour seen where `overlays` are blended over `panel` drawn at
/// `coverage` above `backdrop`. Like the canvas and the compositor, it blends
/// in encoded sRGB: the panel over the backdrop, then each overlay over that.
fn visible_panel_color(panel: u32, coverage: f64, overlays: &[(u32, u8)], backdrop: u32) -> u32 {
    let panel = rgb_channels(panel);
    let backdrop = rgb_channels(backdrop);
    let mut visible: [f64; 3] = std::array::from_fn(|index| {
        f64::from(panel[index]) * coverage + f64::from(backdrop[index]) * (1.0 - coverage)
    });
    for &(color, alpha) in overlays {
        let color = rgb_channels(color);
        let alpha = f64::from(alpha) / 255.0;
        for (channel, overlay) in visible.iter_mut().zip(color) {
            *channel = f64::from(overlay) * alpha + *channel * (1.0 - alpha);
        }
    }
    packed_channels(visible.map(|channel| {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let channel = channel.round().clamp(0.0, 255.0) as u8;
        channel
    }))
}

/// Whether every text colour of `layer` keeps the contrast floor over `panel`
/// drawn at `coverage`, whatever the backdrop behind it. The visible
/// background is affine in the backdrop with non-negative weights, and
/// luminance is monotonic in each channel, so the black and white backdrops
/// bound every other one; text whose luminance falls between them can meet an
/// equal backdrop.
fn panel_layer_readable(panel: u32, coverage: f64, layer: &PanelTextLayer) -> bool {
    let darkest = visible_panel_color(panel, coverage, &layer.overlays, 0);
    let lightest = visible_panel_color(panel, coverage, &layer.overlays, 0x00ff_ffff);
    let range = relative_luminance(darkest)..=relative_luminance(lightest);
    layer.text.iter().all(|&color| {
        !range.contains(&relative_luminance(color))
            && contrast_ratio(color, darkest).min(contrast_ratio(color, lightest))
                >= PANEL_TEXT_MIN_CONTRAST
    })
}

fn panel_layers_readable(panel: u32, coverage: f64, layers: &[PanelTextLayer]) -> bool {
    layers
        .iter()
        .all(|layer| panel_layer_readable(panel, coverage, layer))
}

/// Whether `text` painted straight on the panel keeps the floor.
#[cfg(test)]
fn panel_text_readable(panel: u32, coverage: f64, text: &[u32]) -> bool {
    panel_layer_readable(
        panel,
        coverage,
        &PanelTextLayer {
            overlays: Vec::new(),
            text: text.to_vec(),
        },
    )
}

/// The panel coverage a shell alpha asks for over a window's own glass:
/// `1 - (1 - shell) * (1 - window)`.
fn configured_panel_coverage(shell_alpha: u16, window_alpha: u16) -> f64 {
    let shell = f64::from(shell_alpha) / f64::from(u16::MAX);
    let window = f64::from(window_alpha) / f64::from(u16::MAX);
    1.0 - (1.0 - shell) * (1.0 - window)
}

/// The premultiplied fill for a chrome panel that follows an Omarchy shell
/// `background-alpha` (`shell_alpha`) over a window whose background has
/// `window_alpha`, with `text` painted straight on it. See
/// `chrome_panel_fill_layers`.
pub(crate) fn chrome_panel_fill(
    panel: u32,
    shell_alpha: u16,
    window_alpha: u16,
    text: &[u32],
) -> [u8; 4] {
    chrome_panel_fill_layers(
        panel,
        shell_alpha,
        window_alpha,
        &[PanelTextLayer {
            overlays: Vec::new(),
            text: text.to_vec(),
        }],
    )
}

/// The premultiplied fill for a chrome panel that follows an Omarchy shell
/// `background-alpha` (`shell_alpha`) over a window whose background has
/// `window_alpha`.
///
/// The panel colour is the theme background, the same colour as the window's
/// own glass, so the panel is drawn as that glass with the shell alpha layered
/// over it: its coverage is `1 - (1 - shell) * (1 - window)`. An opaque window
/// therefore keeps opaque panels, and a glassy one shows its blurred backdrop
/// through them no more than through the terminal itself. Text over an
/// unknown backdrop must stay readable, so the coverage is raised to the
/// lowest value at which every text colour of every layer, including text on
/// translucent row highlights, keeps 4.5:1 against a black and a white
/// backdrop alike. Text that misses the floor even on the opaque panel keeps
/// the panel opaque.
fn chrome_panel_fill_layers(
    panel: u32,
    shell_alpha: u16,
    window_alpha: u16,
    layers: &[PanelTextLayer],
) -> [u8; 4] {
    let configured = configured_panel_coverage(shell_alpha, window_alpha);
    let coverage = if panel_layers_readable(panel, configured, layers) {
        configured
    } else if !panel_layers_readable(panel, 1.0, layers) {
        1.0
    } else {
        let mut failing = configured;
        let mut passing = 1.0_f64;
        for _ in 0..24 {
            let candidate = f64::midpoint(failing, passing);
            if panel_layers_readable(panel, candidate, layers) {
                passing = candidate;
            } else {
                failing = candidate;
            }
        }
        passing
    };
    // Round coverage up so quantization never drops below the floor.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let alpha = (coverage * 255.0 - 1e-9).ceil().clamp(0.0, 255.0) as u8;
    premultiplied_rgba(rgb_channels(panel), alpha)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SessionPickerPalette {
    pub(crate) scrim: [u8; 4],
    pub(crate) panel: u32,
    /// Premultiplied panel fill. Opaque `panel` unless the palette follows an
    /// Omarchy shell alpha through `chrome_panel_palette`.
    pub(crate) panel_fill: [u8; 4],
    pub(crate) primary: u32,
    pub(crate) secondary: u32,
    pub(crate) frame: u32,
    pub(crate) focused_frame: u32,
    pub(crate) selected_rail: u32,
    pub(crate) selected_fill: u32,
    pub(crate) selected_primary: u32,
    pub(crate) selected_secondary: u32,
    /// Text for an unselected row under the translucent hover or press
    /// highlight, corrected to 4.5:1 against every opaque highlight state.
    pub(crate) highlight_primary: u32,
    pub(crate) highlight_secondary: u32,
    pub(crate) shadow: [u8; 4],
    /// Status tints on the opaque panel and on the selected fill;
    /// `None` when `[explorer] status-colors=no`.
    pub(crate) status_tints: Option<ChromeTints>,
    pub(crate) selected_status_tints: Option<ChromeTints>,
}

impl SessionPickerPalette {
    /// The primary and secondary text for a row: the selected pair on the
    /// selected fill, the highlight pair under a hover or press highlight, and
    /// the panel pair otherwise.
    pub(crate) const fn row_text(&self, selected: bool, highlighted: bool) -> (u32, u32) {
        if selected {
            (self.selected_primary, self.selected_secondary)
        } else if highlighted {
            (self.highlight_primary, self.highlight_secondary)
        } else {
            (self.primary, self.secondary)
        }
    }

    /// The status tints for a row on the panel or the selected fill.
    pub(crate) const fn row_tints(&self, selected: bool) -> Option<ChromeTints> {
        if selected {
            self.selected_status_tints
        } else {
            self.status_tints
        }
    }

    /// This palette with status tints, or without them for
    /// `[explorer] status-colors=no`.
    #[must_use]
    pub(crate) const fn with_status_colors(mut self, enabled: bool) -> Self {
        if !enabled {
            self.status_tints = None;
            self.selected_status_tints = None;
        }
        self
    }
}

#[must_use]
pub(crate) fn session_picker_palette(theme: ResolvedTheme) -> SessionPickerPalette {
    let roles = theme.roles;
    let primary = contrast_corrected(roles.text, roles.surface);
    let secondary = contrast_corrected_toward(
        mix_linear(primary, roles.surface, 0.30),
        primary,
        roles.surface,
    );
    let selected_fill = mix_linear(roles.surface, roles.selection, 0.24);
    let selected_primary = contrast_corrected(primary, selected_fill);
    let selected_secondary = contrast_corrected_toward(
        mix_linear(selected_primary, selected_fill, 0.30),
        selected_primary,
        selected_fill,
    );
    let focused_frame = if contrast_ratio(roles.focus_ring, roles.divider) >= 1.2 {
        roles.focus_ring
    } else {
        roles.accent
    };
    let [red, green, blue] = rgb_channels(mix_linear(roles.surface, 0, 0.80));
    // Every opaque state an unselected highlighted row can be in.
    let highlight_states = [
        vec![(selected_fill, ROW_HOVER_ALPHA)],
        vec![(roles.accent, ROW_PRESS_ALPHA)],
        vec![
            (selected_fill, ROW_HOVER_ALPHA),
            (roles.accent, ROW_PRESS_ALPHA),
        ],
    ]
    .map(|overlays| visible_panel_color(roles.surface, 1.0, &overlays, 0));
    let highlight_primary =
        contrast_corrected_against_all(primary, &highlight_states).unwrap_or(primary);
    let highlight_secondary =
        contrast_corrected_against_all(secondary, &highlight_states).unwrap_or(secondary);
    let [panel_red, panel_green, panel_blue] = rgb_channels(roles.surface);
    SessionPickerPalette {
        scrim: [red, green, blue, 140],
        panel: roles.surface,
        panel_fill: [panel_red, panel_green, panel_blue, u8::MAX],
        primary,
        secondary,
        frame: roles.divider,
        focused_frame,
        selected_rail: roles.accent,
        selected_fill,
        selected_primary,
        selected_secondary,
        highlight_primary,
        highlight_secondary,
        shadow: [0, 0, 0, 89],
        status_tints: Some(ChromeTints::on(&roles, roles.surface)),
        selected_status_tints: Some(ChromeTints::on(&roles, selected_fill)),
    }
}

/// The overlay palette for a panel that follows the Omarchy shell's
/// `background-alpha` for `surface`. Every foreground a painter draws directly
/// on that surface's panel fill keeps the contrast floor over any backdrop;
/// see `chrome_panel_fill`. Text on the opaque selected fill is excluded.
#[must_use]
pub(crate) fn chrome_panel_palette(
    theme: ResolvedTheme,
    surface: ChromeSurface,
) -> SessionPickerPalette {
    let mut palette = session_picker_palette(theme);
    let shell_alpha = surface.shell_alpha(theme.shell);
    let configured = configured_panel_coverage(shell_alpha, theme.background_alpha);
    if configured < 1.0 {
        // Highlighted rows are transient, so their text is corrected further,
        // against the highlights over black and white backdrops at the asked
        // coverage, before the coverage floor is applied to everything.
        let backgrounds = highlight_overlays(&palette)
            .iter()
            .flat_map(|overlays| {
                [0, 0x00ff_ffff].map(|backdrop| {
                    visible_panel_color(palette.panel, configured, overlays, backdrop)
                })
            })
            .collect::<Vec<_>>();
        // Keep the opaque correction when no colour fits both backdrops; the
        // coverage floor then raises the panel instead.
        if let (Some(primary), Some(secondary)) = (
            contrast_corrected_against_all(palette.highlight_primary, &backgrounds),
            contrast_corrected_against_all(palette.highlight_secondary, &backgrounds),
        ) {
            palette.highlight_primary = primary;
            palette.highlight_secondary = secondary;
        }
    }
    palette.panel_fill = chrome_panel_fill_layers(
        palette.panel,
        shell_alpha,
        theme.background_alpha,
        &surface.panel_text_layers(&palette),
    );
    palette
}

/// The translucent highlights an unselected row can carry: hover, press, or
/// both, in paint order.
fn highlight_overlays(palette: &SessionPickerPalette) -> [Vec<(u32, u8)>; 3] {
    let hover = (palette.selected_fill, ROW_HOVER_ALPHA);
    let press = (palette.selected_rail, ROW_PRESS_ALPHA);
    [vec![hover], vec![press], vec![hover, press]]
}

#[derive(Clone, Copy)]
pub(crate) struct SessionPickerTextItem<'a> {
    pub(crate) display_title: &'a str,
    pub(crate) breadcrumb: &'a str,
    pub(crate) working_directory: &'a str,
    pub(crate) pane_count: usize,
    pub(crate) running_pane_count: usize,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct SessionPickerTextKey {
    source: ChromeSource,
    constrained_width: u32,
    /// The tallest text cell in buffer px; `u32::MAX` for the chrome line cap.
    maximum_cell_height: u32,
    style: ChromeTextStyle,
    scale_120: u32,
    renderer_generation: u64,
}

#[derive(Default)]
pub(crate) struct SessionPickerTextCache {
    entries: HashMap<SessionPickerTextKey, ChromeText>,
    recent_frames: VecDeque<HashSet<SessionPickerTextKey>>,
}

impl SessionPickerTextCache {
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.recent_frames.clear();
    }

    pub(super) fn finish_frame(&mut self, used: HashSet<SessionPickerTextKey>) {
        self.recent_frames.push_back(used);
        while self.recent_frames.len() > 3 {
            self.recent_frames.pop_front();
        }
        let retained = self
            .recent_frames
            .iter()
            .flat_map(HashSet::iter)
            .collect::<HashSet<_>>();
        self.entries.retain(|key, _| retained.contains(key));
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

fn logical_coordinate_to_buffer(value: u32, scale_120: u32) -> u32 {
    value.saturating_mul(scale_120).div_ceil(120)
}

fn picker_buffer_rect(rect: Rect, scale_120: u32) -> Rect {
    let left = logical_coordinate_to_buffer(rect.x, scale_120);
    let top = logical_coordinate_to_buffer(rect.y, scale_120);
    let right = rect
        .x
        .saturating_add(rect.width)
        .saturating_mul(scale_120)
        .div_ceil(120);
    let bottom = rect
        .y
        .saturating_add(rect.height)
        .saturating_mul(scale_120)
        .div_ceil(120);
    Rect {
        x: left,
        y: top,
        width: right.saturating_sub(left),
        height: bottom.saturating_sub(top),
    }
}

fn rect_tuple(rect: Rect) -> (i32, i32, u32, u32) {
    (
        i32::try_from(rect.x).unwrap_or(i32::MAX),
        i32::try_from(rect.y).unwrap_or(i32::MAX),
        rect.width,
        rect.height,
    )
}

fn opaque_rgba(color: u32) -> [u8; 4] {
    let [red, green, blue] = rgb_channels(color);
    [red, green, blue, 0xff]
}

#[derive(Clone, Copy)]
pub(super) enum PickerTextAlignment {
    Left,
    Center,
    Right,
}

pub(super) fn truncate_picker_text(source: &str, maximum_cells: usize) -> String {
    let total_cells = source
        .chars()
        .map(|character| character.width().unwrap_or(0).min(2))
        .sum::<usize>();
    if total_cells <= maximum_cells {
        return source.to_owned();
    }
    if maximum_cells == 0 {
        return String::new();
    }
    let content_cells = maximum_cells - 1;
    let mut cells = 0_usize;
    let mut truncated = String::new();
    for character in source.chars() {
        let width = character.width().unwrap_or(0).min(2);
        if cells.saturating_add(width) > content_cells {
            break;
        }
        truncated.push(character);
        cells = cells.saturating_add(width);
    }
    truncated.push('…');
    truncated
}

/// Truncates like [`truncate_picker_text`], dropping icons cut by the ellipsis.
fn truncate_picker_source(source: &ChromeSource, maximum_cells: usize) -> ChromeSource {
    if source.cells() <= maximum_cells {
        return source.clone();
    }
    let text = truncate_picker_text(source.text(), maximum_cells);
    source.with_prefix_text(text, maximum_cells.saturating_sub(1))
}

fn editable_search_text(query: &str, maximum_cells: usize) -> String {
    editable_text("Search: ", query, maximum_cells)
}

/// An editable `query` after `label` with a trailing `_` caret, within
/// `maximum_cells`. When it does not fit, the start of the query gives way
/// behind a leading `…` (and the label too when space is very short), so the
/// caret and the latest input stay visible.
pub(super) fn editable_text(label: &str, query: &str, maximum_cells: usize) -> String {
    let width = |source: &str| {
        source
            .chars()
            .map(|character| character.width().unwrap_or(0).min(2))
            .sum::<usize>()
    };
    let full = format!("{label}{query}_");
    if width(&full) <= maximum_cells {
        return full;
    }
    if maximum_cells == 0 {
        return String::new();
    }
    if maximum_cells == 1 {
        return "_".to_owned();
    }
    let unlabelled = format!("{query}_");
    if width(&unlabelled) <= maximum_cells {
        return unlabelled;
    }
    let last_character_width = query
        .chars()
        .rev()
        .map(|character| character.width().unwrap_or(0).min(2))
        .find(|width| *width > 0)
        .unwrap_or(0);
    let label = if maximum_cells >= width(label) + 2 + last_character_width {
        label
    } else {
        ""
    };
    let available = maximum_cells.saturating_sub(width(label) + 2);
    let mut cells = 0;
    let mut reversed = Vec::new();
    for character in query.chars().rev() {
        let character_width = character.width().unwrap_or(0).min(2);
        if cells + character_width > available {
            break;
        }
        cells += character_width;
        reversed.push(character);
    }
    let suffix = reversed
        .into_iter()
        .rev()
        .skip_while(|character| character.width().unwrap_or(0) == 0)
        .collect::<String>();
    format!("{label}…{suffix}_")
}

#[allow(clippy::too_many_arguments)]
pub(super) fn paint_picker_text(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    used: &mut HashSet<SessionPickerTextKey>,
    canvas: &mut [u8],
    canvas_width: u32,
    canvas_height: u32,
    source: &str,
    style: ChromeTextStyle,
    scale_120: u32,
    renderer_generation: u64,
    clip: Rect,
    alignment: PickerTextAlignment,
    color: u32,
) -> Result<()> {
    if source.is_empty() {
        return Ok(());
    }
    paint_picker_source(
        cache,
        context,
        used,
        canvas,
        canvas_width,
        canvas_height,
        &ChromeSource::from(source),
        style,
        scale_120,
        renderer_generation,
        clip,
        alignment,
        color,
    )
}

/// Paints trusted chrome text that may hold icons.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_picker_source(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    used: &mut HashSet<SessionPickerTextKey>,
    canvas: &mut [u8],
    canvas_width: u32,
    canvas_height: u32,
    source: &ChromeSource,
    style: ChromeTextStyle,
    scale_120: u32,
    renderer_generation: u64,
    clip: Rect,
    alignment: PickerTextAlignment,
    color: u32,
) -> Result<()> {
    paint_picker_source_within(
        cache,
        context,
        used,
        canvas,
        canvas_width,
        canvas_height,
        source,
        style,
        scale_120,
        renderer_generation,
        clip,
        alignment,
        color,
        u32::MAX,
    )
}

/// Paints chrome text whose cells are at most `maximum_cell_height` buffer px
/// tall, as the tab strip sizes its labels; `u32::MAX` keeps the chrome line
/// cap that [`paint_picker_source`] uses.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_picker_source_within(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    used: &mut HashSet<SessionPickerTextKey>,
    canvas: &mut [u8],
    canvas_width: u32,
    canvas_height: u32,
    source: &ChromeSource,
    style: ChromeTextStyle,
    scale_120: u32,
    renderer_generation: u64,
    clip: Rect,
    alignment: PickerTextAlignment,
    color: u32,
    maximum_cell_height: u32,
) -> Result<()> {
    paint_picker_source_tinted(
        cache,
        context,
        used,
        canvas,
        canvas_width,
        canvas_height,
        source,
        style,
        scale_120,
        renderer_generation,
        clip,
        alignment,
        color,
        None,
        maximum_cell_height,
    )
}

/// Paints like [`paint_picker_source_within`], each tinted run of `source`
/// in its color from `tints`, the tints for the background it paints on.
/// `None` paints every glyph in `color`, as `[explorer] status-colors=no`
/// asks.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_picker_source_tinted(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    used: &mut HashSet<SessionPickerTextKey>,
    canvas: &mut [u8],
    canvas_width: u32,
    canvas_height: u32,
    source: &ChromeSource,
    style: ChromeTextStyle,
    scale_120: u32,
    renderer_generation: u64,
    clip: Rect,
    alignment: PickerTextAlignment,
    color: u32,
    tints: Option<ChromeTints>,
    maximum_cell_height: u32,
) -> Result<()> {
    if source.is_empty() || clip.width == 0 || clip.height == 0 {
        return Ok(());
    }
    let key = SessionPickerTextKey {
        source: source.clone(),
        constrained_width: clip.width,
        maximum_cell_height,
        style,
        scale_120,
        renderer_generation,
    };
    let load = |source: &ChromeSource| {
        if maximum_cell_height == u32::MAX {
            ChromeText::load_source_styled_with_context(source, scale_120, style, context)
        } else {
            ChromeText::load_source_styled_within(
                source,
                scale_120,
                style,
                context,
                maximum_cell_height,
            )
        }
    };
    if !cache.entries.contains_key(&key) {
        let mut text = load(source)?;
        if text.pixel_width() > clip.width {
            let maximum_cells =
                usize::try_from(clip.width / text.frame.cell_width.max(1)).unwrap_or(usize::MAX);
            let truncated = truncate_picker_source(source, maximum_cells);
            text = load(&truncated)?;
        }
        cache.entries.insert(key.clone(), text);
    }
    let text = &cache.entries[&key];
    let x = match alignment {
        PickerTextAlignment::Left => clip.x,
        PickerTextAlignment::Center => clip
            .x
            .saturating_add(clip.width.saturating_sub(text.pixel_width()) / 2),
        PickerTextAlignment::Right => clip
            .x
            .saturating_add(clip.width.saturating_sub(text.pixel_width())),
    };
    let y = clip
        .y
        .saturating_add(clip.height.saturating_sub(text.pixel_height()) / 2);
    text.paint_colored(
        canvas,
        canvas_width,
        canvas_height,
        (x, y),
        clip,
        |column| {
            tints
                .zip(source.tint_at(column))
                .map_or(color, |(tints, tint)| tints.color(tint))
        },
    );
    used.insert(key);
    Ok(())
}

/// The width, in buffer pixels, of one chrome cell as [`paint_picker_source`]
/// lays out text in `style`, so fixed columns can be placed in whole cells.
pub(super) fn picker_cell_width(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    used: &mut HashSet<SessionPickerTextKey>,
    style: ChromeTextStyle,
    scale_120: u32,
    renderer_generation: u64,
) -> Result<u32> {
    let key = SessionPickerTextKey {
        source: ChromeSource::from(" "),
        constrained_width: u32::MAX,
        maximum_cell_height: u32::MAX,
        style,
        scale_120,
        renderer_generation,
    };
    if !cache.entries.contains_key(&key) {
        let text =
            ChromeText::load_source_styled_with_context(&key.source, scale_120, style, context)?;
        cache.entries.insert(key.clone(), text);
    }
    let width = cache.entries[&key].frame.cell_width.max(1);
    used.insert(key);
    Ok(width)
}

/// The terminal search field's panel, in buffer pixels, at the bottom left
/// of `terminal`; `None` when the terminal is too small to show it. The
/// painter and the Window's surface damage both use it, so a frame that
/// removes or moves the field damages exactly where it was drawn.
#[must_use]
pub(crate) fn search_overlay_panel(terminal: Rect, scale_120: u32) -> Option<Rect> {
    if scale_120 == 0 {
        return None;
    }
    let scaled = |logical: u32| logical.saturating_mul(scale_120).div_ceil(120).max(1);
    let margin = scaled(8);
    let width = terminal
        .width
        .saturating_sub(margin.saturating_mul(2))
        .min(scaled(440));
    let height = terminal
        .height
        .saturating_sub(margin.saturating_mul(2))
        .min(scaled(48));
    if width < scaled(96) || height < scaled(28) {
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

/// Paint a bounded terminal search field inside the terminal side of any Explorer split.
/// The compositor title alone is not a visible search surface on every desktop.
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the bounded search field keeps geometry and two text rows in one paint seam"
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
    input: &str,
    searched_query: &str,
    matches: usize,
    selected: usize,
    searching: bool,
) -> Result<bool> {
    let Some(panel) = search_overlay_panel(terminal, scale_120) else {
        return Ok(false);
    };
    let scaled = |logical: u32| logical.saturating_mul(scale_120).div_ceil(120).max(1);
    let height = panel.height;
    fill_rect(
        canvas,
        canvas_width,
        canvas_height,
        rect_tuple(panel),
        palette.panel_fill,
    );
    let border = scale_120.div_ceil(120).max(1);
    for stripe in [
        (panel.x, panel.y, panel.width, border),
        (panel.x, panel.y, border, panel.height),
    ] {
        fill_rect(
            canvas,
            canvas_width,
            canvas_height,
            (
                i32::try_from(stripe.0).unwrap_or(i32::MAX),
                i32::try_from(stripe.1).unwrap_or(i32::MAX),
                stripe.2,
                stripe.3,
            ),
            opaque_rgba(palette.focused_frame),
        );
    }
    let padding = scaled(10);
    let inner = Rect {
        x: panel.x.saturating_add(padding),
        y: panel.y,
        width: panel.width.saturating_sub(padding.saturating_mul(2)),
        height: panel.height,
    };
    let query = input
        .chars()
        .filter(|ch| !ch.is_control())
        .take(256)
        .collect::<String>();
    let single_line = height < scaled(40);
    let query_clip = Rect {
        height: if single_line {
            inner.height
        } else {
            inner.height / 2
        },
        ..inner
    };
    let cell_width =
        ChromeText::load_styled_with_context("_", scale_120, ChromeTextStyle::Regular, context)?
            .frame
            .cell_width
            .max(1);
    let maximum_cells = usize::try_from(query_clip.width / cell_width).unwrap_or(usize::MAX);
    let display = editable_search_text(&query, maximum_cells);
    let mut used = HashSet::new();
    paint_picker_text(
        cache,
        context,
        &mut used,
        canvas,
        canvas_width,
        canvas_height,
        &display,
        ChromeTextStyle::Regular,
        scale_120,
        renderer_generation,
        query_clip,
        PickerTextAlignment::Left,
        palette.primary,
    )?;
    if !single_line {
        let status = if input == searched_query && searching && matches == 0 {
            "Searching older history… · Esc close".to_owned()
        } else if input == searched_query {
            format!(
                "{}/{} matches · Ctrl+N/P next/prev · Esc close",
                selected.saturating_add(1).min(matches),
                matches
            )
        } else {
            "Enter to search · Esc close".to_owned()
        };
        let status_clip = Rect {
            y: inner.y.saturating_add(query_clip.height),
            height: inner.height.saturating_sub(query_clip.height),
            ..inner
        };
        paint_picker_text(
            cache,
            context,
            &mut used,
            canvas,
            canvas_width,
            canvas_height,
            &status,
            ChromeTextStyle::Regular,
            scale_120,
            renderer_generation,
            status_clip,
            PickerTextAlignment::Left,
            palette.secondary,
        )?;
    }
    cache.finish_frame(used);
    Ok(true)
}

#[allow(
    clippy::similar_names,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the specialized picker painter keeps ordered chrome composition in one bounded seam"
)]
pub(crate) fn paint_session_picker_overlay(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    canvas: &mut [u8],
    canvas_width: u32,
    canvas_height: u32,
    scrim_rect: Rect,
    scale_120: u32,
    renderer_generation: u64,
    layout: &SessionPickerOverlayLayout,
    palette: SessionPickerPalette,
    purpose: SessionPickerPurpose,
    items: &[SessionPickerTextItem<'_>],
    new_enabled: bool,
    new_blocker: Option<&str>,
    search_active: bool,
    search_query: &str,
    selected: PickerHitTarget,
    hovered: Option<PickerHitTarget>,
    pressed: Option<PickerHitTarget>,
    keyboard_focused: bool,
) -> Result<()> {
    blend_rect(
        canvas,
        canvas_width,
        canvas_height,
        rect_tuple(scrim_rect),
        palette.scrim,
    );
    let panel = picker_buffer_rect(layout.panel, scale_120);
    let shadow_offset = 6_u32.saturating_mul(scale_120).div_ceil(120);
    blend_rect(
        canvas,
        canvas_width,
        canvas_height,
        (
            i32::try_from(panel.x.saturating_add(shadow_offset)).unwrap_or(i32::MAX),
            i32::try_from(panel.y.saturating_add(shadow_offset)).unwrap_or(i32::MAX),
            panel.width,
            panel.height,
        ),
        palette.shadow,
    );
    fill_rect(
        canvas,
        canvas_width,
        canvas_height,
        rect_tuple(panel),
        palette.panel_fill,
    );
    let frame_color = if keyboard_focused {
        palette.focused_frame
    } else {
        palette.frame
    };
    let border = scale_120.div_ceil(120).max(1);
    for rect in [
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
            canvas_width,
            canvas_height,
            (
                i32::try_from(rect.0).unwrap_or(i32::MAX),
                i32::try_from(rect.1).unwrap_or(i32::MAX),
                rect.2,
                rect.3,
            ),
            opaque_rgba(frame_color),
        );
    }
    for separator_y in [
        layout.header.y.saturating_add(layout.header.height),
        layout.footer.y,
    ] {
        let separator = picker_buffer_rect(
            Rect {
                x: layout.panel.x,
                y: separator_y,
                width: layout.panel.width,
                height: 1,
            },
            scale_120,
        );
        fill_rect(
            canvas,
            canvas_width,
            canvas_height,
            rect_tuple(separator),
            opaque_rgba(palette.frame),
        );
    }

    let mut used = HashSet::new();
    let header = picker_buffer_rect(layout.header, scale_120);
    let header_inset = 14_u32.saturating_mul(scale_120).div_ceil(120);
    let header_content = Rect {
        x: header.x.saturating_add(header_inset),
        y: header.y,
        width: header.width.saturating_sub(header_inset.saturating_mul(2)),
        height: if layout.mode == SessionPickerPresentationMode::Normal {
            header.height / 2
        } else {
            header.height
        },
    };
    let header_title = Rect {
        width: if layout.mode == SessionPickerPresentationMode::Minimal {
            header_content.width
        } else {
            header_content.width.saturating_mul(2) / 3
        },
        ..header_content
    };
    paint_picker_text(
        cache,
        context,
        &mut used,
        canvas,
        canvas_width,
        canvas_height,
        purpose.heading(layout.mode == SessionPickerPresentationMode::Minimal),
        ChromeTextStyle::Bold,
        scale_120,
        renderer_generation,
        header_title,
        PickerTextAlignment::Left,
        palette.primary,
    )?;
    if layout.mode != SessionPickerPresentationMode::Minimal {
        let count = format!("{} available", items.len());
        paint_picker_text(
            cache,
            context,
            &mut used,
            canvas,
            canvas_width,
            canvas_height,
            &count,
            ChromeTextStyle::Regular,
            scale_120,
            renderer_generation,
            Rect {
                x: header_title.x.saturating_add(header_title.width),
                width: header_content.width.saturating_sub(header_title.width),
                ..header_content
            },
            PickerTextAlignment::Right,
            palette.secondary,
        )?;
    }
    if layout.mode == SessionPickerPresentationMode::Normal {
        paint_picker_text(
            cache,
            context,
            &mut used,
            canvas,
            canvas_width,
            canvas_height,
            &if !search_active {
                purpose.guidance().to_owned()
            } else if items.is_empty() && !search_query.is_empty() {
                format!("Search: {search_query} · No matches")
            } else {
                format!("Search: {search_query}")
            },
            ChromeTextStyle::Regular,
            scale_120,
            renderer_generation,
            Rect {
                x: header_content.x,
                y: header.y.saturating_add(header.height / 2),
                width: header_content.width,
                height: header.height.saturating_sub(header.height / 2),
            },
            PickerTextAlignment::Left,
            palette.secondary,
        )?;
    }

    for row in &layout.rows {
        let row_buffer = picker_buffer_rect(row.surface, scale_120);
        let is_selected = row.target == selected;
        if is_selected {
            fill_rect(
                canvas,
                canvas_width,
                canvas_height,
                rect_tuple(row_buffer),
                opaque_rgba(palette.selected_fill),
            );
            let rail_width = 3_u32.saturating_mul(scale_120).div_ceil(120).max(1);
            fill_rect(
                canvas,
                canvas_width,
                canvas_height,
                (
                    i32::try_from(row_buffer.x).unwrap_or(i32::MAX),
                    i32::try_from(row_buffer.y).unwrap_or(i32::MAX),
                    rail_width,
                    row_buffer.height,
                ),
                opaque_rgba(palette.selected_rail),
            );
        } else if hovered == Some(row.target) {
            blend_rect(
                canvas,
                canvas_width,
                canvas_height,
                rect_tuple(row_buffer),
                [
                    rgb_channels(palette.selected_fill)[0],
                    rgb_channels(palette.selected_fill)[1],
                    rgb_channels(palette.selected_fill)[2],
                    ROW_HOVER_ALPHA,
                ],
            );
        }
        if pressed == Some(row.target) {
            blend_rect(
                canvas,
                canvas_width,
                canvas_height,
                rect_tuple(row_buffer),
                [
                    rgb_channels(palette.selected_rail)[0],
                    rgb_channels(palette.selected_rail)[1],
                    rgb_channels(palette.selected_rail)[2],
                    ROW_PRESS_ALPHA,
                ],
            );
        }
        let highlighted =
            !is_selected && (hovered == Some(row.target) || pressed == Some(row.target));
        let (primary, secondary) = palette.row_text(is_selected, highlighted);
        let marker_width = 22_u32.saturating_mul(scale_120).div_ceil(120);
        if is_selected {
            paint_picker_text(
                cache,
                context,
                &mut used,
                canvas,
                canvas_width,
                canvas_height,
                "›",
                ChromeTextStyle::Bold,
                scale_120,
                renderer_generation,
                Rect {
                    x: row_buffer.x.saturating_add(border),
                    y: row_buffer.y,
                    width: marker_width,
                    height: row_buffer.height,
                },
                PickerTextAlignment::Center,
                primary,
            )?;
        }
        let content = Rect {
            x: row_buffer.x.saturating_add(marker_width),
            y: row_buffer.y,
            width: row_buffer
                .width
                .saturating_sub(marker_width.saturating_add(header_inset)),
            height: row_buffer.height,
        };
        let (title, detail, status) = match row.target {
            PickerHitTarget::New => (
                purpose.new_title(),
                String::new(),
                if new_enabled {
                    purpose.new_description().to_owned()
                } else {
                    new_blocker.unwrap_or("Unavailable").to_owned()
                },
            ),
            PickerHitTarget::Open(index) => {
                let Some(item) = items.get(index) else {
                    continue;
                };
                // Rows without panes are navigation rows (remote profiles or the
                // Recent Dojos remote entry), so they carry no running status.
                let status = if item.pane_count == 0 {
                    String::new()
                } else if layout.mode == SessionPickerPresentationMode::Normal {
                    format!("{}/{} running", item.running_pane_count, item.pane_count)
                } else {
                    format!("{}/{}", item.running_pane_count, item.pane_count)
                };
                let detail = if is_selected
                    && layout.mode == SessionPickerPresentationMode::Normal
                    && !item.working_directory.is_empty()
                {
                    format!("{} · {}", item.breadcrumb, item.working_directory)
                } else {
                    item.breadcrumb.to_owned()
                };
                (item.display_title, detail, status)
            }
        };
        if layout.mode == SessionPickerPresentationMode::Normal
            && matches!(row.target, PickerHitTarget::Open(_))
        {
            let top = Rect {
                x: content.x,
                y: content.y,
                width: content.width,
                height: content.height / 2,
            };
            let title_width = top.width.saturating_mul(2) / 3;
            let title_rect = Rect {
                width: title_width,
                ..top
            };
            let status_rect = Rect {
                x: top.x.saturating_add(title_width),
                width: top.width.saturating_sub(title_width),
                ..top
            };
            paint_picker_text(
                cache,
                context,
                &mut used,
                canvas,
                canvas_width,
                canvas_height,
                title,
                ChromeTextStyle::Bold,
                scale_120,
                renderer_generation,
                title_rect,
                PickerTextAlignment::Left,
                primary,
            )?;
            paint_picker_text(
                cache,
                context,
                &mut used,
                canvas,
                canvas_width,
                canvas_height,
                &status,
                ChromeTextStyle::Regular,
                scale_120,
                renderer_generation,
                status_rect,
                PickerTextAlignment::Right,
                secondary,
            )?;
            paint_picker_text(
                cache,
                context,
                &mut used,
                canvas,
                canvas_width,
                canvas_height,
                &detail,
                ChromeTextStyle::Regular,
                scale_120,
                renderer_generation,
                Rect {
                    x: content.x,
                    y: content.y.saturating_add(content.height / 2),
                    width: content.width,
                    height: content.height.saturating_sub(content.height / 2),
                },
                PickerTextAlignment::Left,
                secondary,
            )?;
        } else {
            let title_width = if layout.mode == SessionPickerPresentationMode::Minimal {
                content.width
            } else {
                content.width.saturating_mul(2) / 3
            };
            let title_rect = Rect {
                width: title_width,
                ..content
            };
            paint_picker_text(
                cache,
                context,
                &mut used,
                canvas,
                canvas_width,
                canvas_height,
                title,
                ChromeTextStyle::Bold,
                scale_120,
                renderer_generation,
                title_rect,
                PickerTextAlignment::Left,
                primary,
            )?;
            if layout.mode != SessionPickerPresentationMode::Minimal {
                paint_picker_text(
                    cache,
                    context,
                    &mut used,
                    canvas,
                    canvas_width,
                    canvas_height,
                    &status,
                    ChromeTextStyle::Regular,
                    scale_120,
                    renderer_generation,
                    Rect {
                        x: content.x.saturating_add(title_width),
                        width: content.width.saturating_sub(title_width),
                        ..content
                    },
                    PickerTextAlignment::Right,
                    secondary,
                )?;
            }
        }
    }

    let footer = picker_buffer_rect(layout.footer, scale_120);
    let footer_text = if items.is_empty() {
        if search_active && !search_query.is_empty() {
            "No matches   Esc clear".to_owned()
        } else if let Some(blocker) = new_blocker {
            blocker.to_owned()
        } else {
            let empty = match purpose {
                SessionPickerPurpose::RecentDojos => "No recent Dojos",
                SessionPickerPurpose::Dojos => "No Dojos",
                SessionPickerPurpose::Lairs => "No Lairs",
                SessionPickerPurpose::Remotes => "No remote profiles",
            };
            if new_enabled {
                format!("{empty}   Ctrl+N new")
            } else {
                empty.to_owned()
            }
        }
    } else {
        match layout.mode {
            SessionPickerPresentationMode::Normal if purpose.offers_creation() => format!(
                "/ search   ↑↓ / J K navigate   Enter {}   Ctrl+N new   Esc cancel",
                purpose.selection_verb()
            ),
            SessionPickerPresentationMode::Normal => format!(
                "/ search   ↑↓ / J K navigate   Enter {}   Esc cancel",
                purpose.selection_verb()
            ),
            SessionPickerPresentationMode::Compact => format!(
                "/ search   ↑↓ navigate   Enter {}   Esc cancel",
                purpose.selection_verb()
            ),
            SessionPickerPresentationMode::Minimal => {
                format!("Enter {}   Esc cancel", purpose.selection_verb())
            }
        }
    };
    paint_picker_text(
        cache,
        context,
        &mut used,
        canvas,
        canvas_width,
        canvas_height,
        &footer_text,
        ChromeTextStyle::Regular,
        scale_120,
        renderer_generation,
        footer,
        PickerTextAlignment::Center,
        palette.secondary,
    )?;
    cache.finish_frame(used);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::super::pixel_index;
    use super::*;
    use crate::theme::ChromeRoles;

    #[test]
    fn scrollback_search_paints_query_inside_terminal_when_explorer_is_open() {
        let content = Rect {
            x: 260,
            y: 40,
            width: 600,
            height: 360,
        };
        let palette = session_picker_palette(ResolvedTheme::default());
        let render = |query: &str| {
            let mut canvas = vec![0_u8; 900 * 480 * 4];
            let mut cache = SessionPickerTextCache::default();
            assert!(
                paint_search_overlay(
                    &mut cache,
                    &RenderContext::new(u16::MAX),
                    &mut canvas,
                    900,
                    480,
                    content,
                    120,
                    1,
                    palette,
                    query,
                    query,
                    3,
                    0,
                    false,
                )
                .unwrap()
            );
            assert!(cache.len() > 0);
            canvas
        };
        let alpha = render("alpha");
        let bravo = render("bravo");
        assert_ne!(
            alpha, bravo,
            "changing the search text must change painted pixels"
        );
        let long = "abcdef".repeat(15);
        assert_ne!(
            render(&format!("{long}x")),
            render(&format!("{long}y")),
            "editing the clipped query tail must change painted pixels"
        );
        // Every painted pixel lies inside `search_overlay_panel`, the rect
        // the Window damages when the field closes or moves, and that rect
        // sits at the bottom left of the terminal.
        let panel = search_overlay_panel(content, 120).unwrap();
        assert_eq!(panel.x, content.x + 8);
        assert_eq!(panel.y + panel.height, content.y + content.height - 8);
        let mut painted_inside = false;
        for y in 0..480 {
            for x in 0..900 {
                let pixel = pixel_index(
                    900,
                    480,
                    i32::try_from(x).unwrap(),
                    i32::try_from(y).unwrap(),
                )
                .unwrap();
                if x >= panel.x
                    && x < panel.x + panel.width
                    && y >= panel.y
                    && y < panel.y + panel.height
                {
                    painted_inside |= alpha[pixel..pixel + 4] != [0; 4];
                    continue;
                }
                assert_eq!(
                    &alpha[pixel..pixel + 4],
                    &[0; 4],
                    "paint escaped the damaged search panel at {x},{y}"
                );
            }
        }
        assert!(painted_inside);
    }

    #[test]
    fn scrollback_search_clips_oversized_input_and_skips_unusable_geometry() {
        let mut canvas = vec![0_u8; 900 * 480 * 4];
        let mut cache = SessionPickerTextCache::default();
        let context = RenderContext::new(u16::MAX);
        let palette = session_picker_palette(ResolvedTheme::default());
        let tiny = Rect {
            x: 260,
            y: 40,
            width: 60,
            height: 50,
        };
        assert!(
            !paint_search_overlay(
                &mut cache,
                &context,
                &mut canvas,
                900,
                480,
                tiny,
                120,
                1,
                palette,
                "needle",
                "needle",
                0,
                0,
                false,
            )
            .unwrap()
        );
        assert!(canvas.iter().all(|byte| *byte == 0));
        assert_eq!(search_overlay_panel(tiny, 120), None);
        assert_eq!(search_overlay_panel(tiny, 0), None);
        let terminal = Rect {
            x: 260,
            y: 40,
            width: 600,
            height: 360,
        };
        assert!(
            paint_search_overlay(
                &mut cache,
                &context,
                &mut canvas,
                900,
                480,
                terminal,
                180,
                1,
                palette,
                &format!("{}\n", "x".repeat(2_000)),
                "",
                0,
                0,
                false,
            )
            .unwrap()
        );
        assert!(
            cache
                .entries
                .keys()
                .all(|key| key.source.text().len() <= 270 && !key.source.text().contains('\n'))
        );
        assert!(cache.len() <= 2);
    }

    #[test]
    fn editable_search_keeps_tail_and_cursor_in_narrow_fields() {
        assert_eq!(editable_search_text("abc", 20), "Search: abc_");
        assert_eq!(
            editable_search_text("abcdefghijklmnopqrstuvwxyz", 14),
            "Search: …wxyz_"
        );
        assert_eq!(
            editable_search_text("abcdefghijklmnopqrstuvwxyz", 8),
            "…uvwxyz_"
        );
        assert_eq!(editable_search_text("abc", 4), "abc_");
        assert_eq!(editable_search_text("abc", 2), "…_");
        assert_eq!(editable_search_text("abc", 1), "_");
        assert_eq!(editable_search_text("abc", 0), "");
        assert_eq!(editable_search_text("ab界e\u{301}新", 6), "…e\u{301}新_");
        assert_eq!(
            editable_search_text("ab界e\u{301}新", 12),
            "ab界e\u{301}新_"
        );
        assert_eq!(
            editable_search_text("ab界e\u{301}新abcd新", 12),
            "Search: …新_"
        );
    }

    #[test]
    fn picker_text_truncation_reserves_ellipsis_and_preserves_combining_marks() {
        assert_eq!(truncate_picker_text("short", 5), "short");
        assert_eq!(truncate_picker_text("abcdef", 5), "abcd…");
        assert_eq!(truncate_picker_text("界界界", 5), "界界…");
        assert_eq!(
            truncate_picker_text("e\u{301}e\u{301}xy", 3),
            "e\u{301}e\u{301}…"
        );
        assert_eq!(truncate_picker_text("anything", 0), "");
    }

    #[test]
    fn picker_source_truncation_drops_only_icons_the_ellipsis_cuts() {
        use super::super::super::ChromeIcon;

        let mut source = ChromeSource::default();
        source
            .push_icon(ChromeIcon::ChevronDown)
            .push_str(" ")
            .push_icon(ChromeIcon::MessageQuestion)
            .push_str(" long label");
        assert_eq!(source.cells(), 16);
        assert_eq!(truncate_picker_source(&source, 16), source);
        let cut = truncate_picker_source(&source, 8);
        assert_eq!(cut.describe(), "[chevron-down] [message-question] l…");
        let split = truncate_picker_source(&source, 5);
        assert_eq!(split.describe(), "[chevron-down]  …");
        assert_eq!(split.icons(), &[(0, ChromeIcon::ChevronDown)]);
        assert!(truncate_picker_source(&source, 0).is_empty());
        // Labels never become icons, even when they contain icon codepoints.
        let spoof = ChromeSource::from("\u{f98b} label");
        assert!(spoof.icons().is_empty());
    }

    #[test]
    fn picker_purpose_keeps_hierarchy_copy_distinct() {
        let cases = [
            (
                SessionPickerPurpose::RecentDojos,
                "RECENT DOJOS",
                "RECENT DOJOS",
                "Open a recent running Dojo.",
                "+ New terminal",
                "Start a fresh shell",
                "open",
            ),
            (
                SessionPickerPurpose::Dojos,
                "DOJOS",
                "DOJOS",
                "Switch to a Dojo in this Lair.",
                "+ New Dojo",
                "Add a Dojo to this Lair",
                "switch",
            ),
            (
                SessionPickerPurpose::Lairs,
                "LAIRS",
                "LAIRS",
                "Switch to a Lair.",
                "+ New Lair",
                "Start a fresh workspace",
                "switch",
            ),
            (
                SessionPickerPurpose::Remotes,
                "REMOTES",
                "REMOTES",
                "Connect to a remote profile.",
                "+ Add a profile",
                "Edit remotes.toml",
                "connect",
            ),
        ];
        for (purpose, normal, minimal, guidance, new_title, new_description, verb) in cases {
            assert_eq!(purpose.heading(false), normal);
            assert_eq!(purpose.heading(true), minimal);
            assert_eq!(purpose.guidance(), guidance);
            assert_eq!(purpose.new_title(), new_title);
            assert_eq!(purpose.new_description(), new_description);
            assert_eq!(purpose.selection_verb(), verb);
        }
        for purpose in [
            SessionPickerPurpose::RecentDojos,
            SessionPickerPurpose::Dojos,
            SessionPickerPurpose::Lairs,
        ] {
            assert!(purpose.offers_creation());
        }
        assert!(!SessionPickerPurpose::Remotes.offers_creation());
    }

    #[test]
    fn session_picker_layout_is_responsive_bounded_and_scale_deterministic() {
        for (width, height, expected_mode) in [
            (960, 600, SessionPickerPresentationMode::Normal),
            (400, 240, SessionPickerPresentationMode::Compact),
            (240, 140, SessionPickerPresentationMode::Minimal),
        ] {
            let mut previous = None;
            for scale in [120, 150, 240] {
                let layout = session_picker_overlay_layout(width, height, scale, 256, 256, 0)
                    .expect("picker layout");
                assert_eq!(layout.mode, expected_mode);
                assert!(layout.panel.x + layout.panel.width <= width);
                assert!(layout.panel.y + layout.panel.height <= height);
                assert!(layout.rows.iter().all(|row| {
                    row.rect.x >= layout.panel.x
                        && row.rect.y >= layout.panel.y
                        && row.rect.x + row.rect.width <= layout.panel.x + layout.panel.width
                        && row.rect.y + row.rect.height <= layout.panel.y + layout.panel.height
                        && row.surface.x >= row.rect.x
                        && row.surface.y >= row.rect.y
                        && row.surface.x + row.surface.width <= row.rect.x + row.rect.width
                        && row.surface.y + row.surface.height <= row.rect.y + row.rect.height
                }));
                for pair in layout.rows.windows(2) {
                    assert_eq!(pair[0].rect.y + pair[0].rect.height, pair[1].rect.y);
                    let first = picker_buffer_rect(pair[0].surface, scale);
                    let second = picker_buffer_rect(pair[1].surface, scale);
                    assert!(first.y + first.height < second.y);
                }
                let first = &layout.rows[0].surface;
                match expected_mode {
                    SessionPickerPresentationMode::Normal => {
                        assert_eq!(first.x, layout.action.x + 8);
                        assert_eq!(first.y, layout.action.y + 3);
                        assert_eq!(first.width, layout.action.width - 16);
                        assert_eq!(first.height, layout.action.height - 6);
                    }
                    SessionPickerPresentationMode::Compact => {
                        assert_eq!(first.x, layout.action.x + 6);
                        assert_eq!(first.y, layout.action.y + 3);
                        assert_eq!(first.width, layout.action.width - 12);
                        assert_eq!(first.height, layout.action.height - 6);
                    }
                    SessionPickerPresentationMode::Minimal => {
                        assert_eq!(*first, layout.action);
                    }
                }
                let header = picker_buffer_rect(layout.header, scale);
                let action = picker_buffer_rect(layout.action, scale);
                let footer = picker_buffer_rect(layout.footer, scale);
                assert_eq!(header.y + header.height, action.y);
                assert!(action.y + action.height <= footer.y);
                assert!(layout.rows.iter().any(|row| {
                    row.target == PickerHitTarget::Open(255)
                        || expected_mode != SessionPickerPresentationMode::Minimal
                            && layout.visible_range.contains(&255)
                }));
                let geometry = (
                    layout.panel,
                    layout.header,
                    layout.action,
                    layout.list,
                    layout.footer,
                    layout.rows.clone(),
                    layout.visible_range.clone(),
                    layout.mode,
                );
                if let Some(previous) = &previous {
                    assert_eq!(previous, &geometry);
                }
                previous = Some(geometry);
            }
        }
    }

    #[test]
    fn minimal_picker_prioritizes_pointer_target_height() {
        let constrained = session_picker_overlay_layout(240, 50, 150, 8, 2, 0).unwrap();
        assert_eq!(constrained.mode, SessionPickerPresentationMode::Minimal);
        assert_eq!(constrained.action.height, 44);
        let impossible = session_picker_overlay_layout(240, 40, 150, 8, 2, 0).unwrap();
        assert_eq!(impossible.action.height, 40);
    }

    #[test]
    fn session_picker_hit_rectangles_are_half_open_and_stable() {
        let layout = session_picker_overlay_layout(960, 600, 120, 8, 1, 0).unwrap();
        let first = &layout.rows[0];
        assert_eq!(
            session_picker_hit_test(&layout, (f64::from(first.rect.x), f64::from(first.rect.y))),
            Some(PickerHitTarget::New)
        );
        assert_ne!(
            session_picker_hit_test(
                &layout,
                (
                    f64::from(first.rect.x + first.rect.width),
                    f64::from(first.rect.y)
                )
            ),
            Some(PickerHitTarget::New)
        );
        assert_eq!(
            session_picker_hit_test(
                &layout,
                (
                    f64::from(layout.action.x + 1),
                    f64::from(layout.action.y + 1)
                )
            ),
            Some(PickerHitTarget::New),
            "visual menu padding must preserve the full pointer target"
        );
    }

    #[test]
    fn session_picker_palette_corrects_low_contrast_text() {
        for theme in [
            ResolvedTheme::default(),
            ResolvedTheme {
                roles: ChromeRoles {
                    surface: 0x77_77_77,
                    text: 0x78_78_78,
                    selection: 0x79_79_79,
                    divider: 0x77_77_77,
                    focus_ring: 0x77_77_77,
                    accent: 0xee_55_44,
                    ..ChromeRoles::default()
                },
                ..ResolvedTheme::default()
            },
            ResolvedTheme {
                roles: ChromeRoles {
                    surface: 0xf5_f1_e8,
                    text: 0x22_24_28,
                    selection: 0xb8_d8_f0,
                    divider: 0x76_78_7c,
                    focus_ring: 0x18_62_9f,
                    accent: 0x18_62_9f,
                    ..ChromeRoles::default()
                },
                ..ResolvedTheme::default()
            },
        ] {
            let palette = session_picker_palette(theme);
            assert!(contrast_ratio(palette.primary, palette.panel) >= 4.5);
            assert!(contrast_ratio(palette.secondary, palette.panel) >= 4.5);
            assert!(contrast_ratio(palette.selected_primary, palette.selected_fill) >= 4.5);
            assert!(contrast_ratio(palette.selected_secondary, palette.selected_fill) >= 4.5);
            assert_eq!(palette.scrim[3], 140);
            assert_eq!(palette.shadow[3], 89);
        }
        let blue_theme = ResolvedTheme {
            roles: ChromeRoles {
                surface: 0xff_ff_ff,
                text: 0x00_66_cc,
                selection: 0xd8_ea_f8,
                ..ChromeRoles::default()
            },
            ..ResolvedTheme::default()
        };
        let blue_palette = session_picker_palette(blue_theme);
        assert!(contrast_ratio(blue_palette.secondary, blue_palette.panel) >= 4.5);
        assert_ne!(blue_palette.secondary, 0);
        assert_ne!(blue_palette.secondary, 0x00ff_ffff);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn session_picker_painter_reuses_text_except_selected_cwd_detail() {
        let theme = ResolvedTheme::default();
        let palette = session_picker_palette(theme);
        let layout = session_picker_overlay_layout(960, 600, 120, 2, 0, 0).unwrap();
        let items = [
            SessionPickerTextItem {
                display_title: "editor",
                breadcrumb: "work / editor",
                working_directory: "/work",
                pane_count: 2,
                running_pane_count: 2,
            },
            SessionPickerTextItem {
                display_title: "notes",
                breadcrumb: "notes / Dojo 1",
                working_directory: "/notes",
                pane_count: 1,
                running_pane_count: 1,
            },
        ];
        let [_, red, green, blue] = theme.background.to_be_bytes();
        let mut canvas = vec![0_u8; 960 * 600 * 4];
        for pixel in canvas.as_chunks_mut::<4>().0 {
            pixel.copy_from_slice(&[blue, green, red, 0xff]);
        }
        let mut cache = SessionPickerTextCache::default();
        paint_session_picker_overlay(
            &mut cache,
            &RenderContext::new(u16::MAX),
            &mut canvas,
            960,
            600,
            Rect {
                x: 0,
                y: 0,
                width: 960,
                height: 600,
            },
            120,
            1,
            &layout,
            palette,
            SessionPickerPurpose::RecentDojos,
            &items,
            true,
            None,
            false,
            "",
            PickerHitTarget::New,
            None,
            None,
            true,
        )
        .unwrap();
        let shaped = cache.len();
        assert!(shaped > 0);
        assert!(
            cache
                .entries
                .iter()
                .all(|(key, text)| text.pixel_width() <= key.constrained_width)
        );
        let outside = pixel_index(960, 600, 0, 0).unwrap();
        assert_ne!(&canvas[outside..outside + 4], &[blue, green, red, 0xff]);
        let frame = pixel_index(
            960,
            600,
            i32::try_from(layout.panel.x).unwrap(),
            i32::try_from(layout.panel.y).unwrap(),
        )
        .unwrap();
        let [_, frame_red, frame_green, frame_blue] = palette.focused_frame.to_be_bytes();
        assert_eq!(
            &canvas[frame..frame + 4],
            &[frame_blue, frame_green, frame_red, 0xff]
        );
        let rail = pixel_index(
            960,
            600,
            i32::try_from(layout.rows[0].surface.x).unwrap(),
            i32::try_from(layout.rows[0].surface.y + layout.rows[0].surface.height / 2).unwrap(),
        )
        .unwrap();
        let [_, accent_red, accent_green, accent_blue] = theme.roles.accent.to_be_bytes();
        assert_eq!(
            &canvas[rail..rail + 4],
            &[accent_blue, accent_green, accent_red, 0xff]
        );
        paint_session_picker_overlay(
            &mut cache,
            &RenderContext::new(u16::MAX),
            &mut canvas,
            960,
            600,
            Rect {
                x: 0,
                y: 0,
                width: 960,
                height: 600,
            },
            120,
            1,
            &layout,
            palette,
            SessionPickerPurpose::RecentDojos,
            &items,
            true,
            None,
            false,
            "",
            PickerHitTarget::Open(0),
            None,
            None,
            true,
        )
        .unwrap();
        assert_eq!(cache.len(), shaped + 1);
    }

    #[test]
    fn compact_and_minimal_pickers_render_no_match_status() {
        let theme = ResolvedTheme::default();
        for (width, height, expected_mode) in [
            (400, 240, SessionPickerPresentationMode::Compact),
            (240, 140, SessionPickerPresentationMode::Minimal),
        ] {
            let layout = session_picker_overlay_layout(width, height, 120, 0, 0, 0).unwrap();
            assert_eq!(layout.mode, expected_mode);
            let mut cache = SessionPickerTextCache::default();
            let mut canvas = vec![0_u8; usize::try_from(width * height * 4).unwrap()];
            paint_session_picker_overlay(
                &mut cache,
                &RenderContext::new(u16::MAX),
                &mut canvas,
                width,
                height,
                Rect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                },
                120,
                1,
                &layout,
                session_picker_palette(theme),
                SessionPickerPurpose::RecentDojos,
                &[],
                true,
                None,
                true,
                "missing",
                PickerHitTarget::New,
                None,
                None,
                true,
            )
            .unwrap();
            assert!(
                cache
                    .entries
                    .keys()
                    .any(|key| key.source.text() == "No matches   Esc clear")
            );
        }
    }

    #[test]
    fn session_picker_text_cache_stays_bounded_for_large_catalogs() {
        let owned = (0..256)
            .map(|index| (format!("session {index}"), format!("/work/{index}")))
            .collect::<Vec<_>>();
        let items = owned
            .iter()
            .map(|(title, cwd)| SessionPickerTextItem {
                display_title: title,
                breadcrumb: title,
                working_directory: cwd,
                pane_count: 2,
                running_pane_count: 2,
            })
            .collect::<Vec<_>>();
        let theme = ResolvedTheme::default();
        let mut cache = SessionPickerTextCache::default();
        let mut canvas = vec![0_u8; 960 * 600 * 4];
        let mut visible_start = 0;
        for selected in [0, 32, 96, 255] {
            let layout = session_picker_overlay_layout(
                960,
                600,
                120,
                items.len(),
                selected + 1,
                visible_start,
            )
            .unwrap();
            visible_start = layout.visible_range.start;
            paint_session_picker_overlay(
                &mut cache,
                &RenderContext::new(u16::MAX),
                &mut canvas,
                960,
                600,
                Rect {
                    x: 0,
                    y: 0,
                    width: 960,
                    height: 600,
                },
                120,
                1,
                &layout,
                session_picker_palette(theme),
                SessionPickerPurpose::RecentDojos,
                &items,
                true,
                None,
                false,
                "",
                PickerHitTarget::Open(selected),
                None,
                None,
                true,
            )
            .unwrap();
        }
        assert!(
            cache.len() < 128,
            "cache held {} shaped strings",
            cache.len()
        );
    }

    /// tokyo-night's background and foreground with Omarchy's Glassy Windows
    /// alpha (0.65) and a given shell alpha.
    fn glassy_theme(menu_alpha: f32, popup_alpha: f32) -> ResolvedTheme {
        let foot_alpha = |alpha: f32| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let alpha = (alpha * f32::from(u16::MAX)) as u16;
            alpha
        };
        ResolvedTheme {
            roles: ChromeRoles {
                surface: 0x1a_1b_26,
                text: 0xc0_ca_f5,
                selection: 0x7a_a2_f7,
                ..ChromeRoles::default()
            },
            background_alpha: foot_alpha(0.65),
            shell: OmarchyShellSettings {
                menu_alpha: foot_alpha(menu_alpha),
                popup_alpha: foot_alpha(popup_alpha),
                text_size_px: None,
                text_size_held: false,
                ..OmarchyShellSettings::default()
            },
            ..ResolvedTheme::default()
        }
    }

    #[test]
    fn panels_follow_the_shell_alpha_over_the_window_glass() {
        // Omarchy's defaults and an opaque window both keep panels opaque.
        let opaque = session_picker_palette(glassy_theme(1.0, 1.0)).panel_fill;
        assert_eq!(opaque, [0x1a, 0x1b, 0x26, 0xff]);
        for surface in [ChromeSurface::Menu, ChromeSurface::Popup] {
            assert_eq!(
                chrome_panel_palette(glassy_theme(1.0, 1.0), surface).panel_fill,
                opaque
            );
            let mut solid_window = glassy_theme(0.0, 0.0);
            solid_window.background_alpha = u16::MAX;
            assert_eq!(
                chrome_panel_palette(solid_window, surface).panel_fill,
                opaque
            );
        }
        // [menu] 0.9 over the 0.65 glass: 1 - 0.1 * 0.35 = 0.965 coverage,
        // readable, so it is used as is; [popups] stays opaque.
        let theme = glassy_theme(0.9, 1.0);
        let menu = chrome_panel_palette(theme, ChromeSurface::Menu).panel_fill;
        assert_eq!(menu[3], 247);
        assert_eq!(menu, premultiplied_rgba([0x1a, 0x1b, 0x26], 247));
        assert_eq!(
            chrome_panel_palette(theme, ChromeSurface::Popup).panel_fill,
            opaque
        );
        let popup = chrome_panel_palette(glassy_theme(1.0, 0.9), ChromeSurface::Popup);
        assert_eq!(popup.panel_fill[3], 247);
    }

    #[test]
    fn panel_alpha_is_clamped_to_keep_text_readable_over_any_backdrop() {
        // A fully transparent menu over the glass would leave 0.65 coverage;
        // the text floor over a white backdrop needs more.
        let theme = glassy_theme(0.0, 0.0);
        let palette = chrome_panel_palette(theme, ChromeSurface::Menu);
        let alpha = palette.panel_fill[3];
        assert!(alpha > 166, "{alpha}");
        assert!(alpha < u8::MAX, "{alpha}");
        let text = [palette.primary, palette.secondary];
        let coverage = |alpha: u8| f64::from(alpha) / 255.0;
        let layers = ChromeSurface::Menu.panel_text_layers(&palette);
        assert!(panel_layers_readable(
            palette.panel,
            coverage(alpha),
            &layers
        ));
        assert!(!panel_layers_readable(
            palette.panel,
            coverage(alpha - 2),
            &layers
        ));
        for backdrop in [0, 0x00ff_ffff, 0x0080_8080, 0x00ff_0000, 0x00c0_caf5] {
            let visible = mix_encoded(palette.panel, backdrop, 1.0 - coverage(alpha));
            for color in text {
                assert!(contrast_ratio(color, visible) >= 4.4, "{backdrop:06x}");
            }
        }
        // Light panels are bounded by a black backdrop instead.
        let light = chrome_panel_fill(0xf5_f1_e8, 0, 42_597, &[0x5a_5a_5a])[3];
        assert!(light > 166 && light < u8::MAX, "{light}");
        assert!(panel_text_readable(
            0xf5_f1_e8,
            coverage(light),
            &[0x5a_5a_5a]
        ));
        assert!(!panel_text_readable(
            0xf5_f1_e8,
            coverage(light - 2),
            &[0x5a_5a_5a]
        ));
        // A palette whose secondary text was corrected to exactly the floor on
        // the opaque panel has no headroom, so its panel stays opaque.
        let mut floored = theme;
        floored.roles.surface = 0xf5_f1_e8;
        floored.roles.text = 0x22_24_28;
        let floored = chrome_panel_palette(floored, ChromeSurface::Popup);
        assert!(contrast_ratio(floored.secondary, floored.panel) < 4.6);
        assert_eq!(floored.panel_fill[3], u8::MAX);
        // Text that misses the floor even on the opaque panel keeps it opaque.
        assert_eq!(
            chrome_panel_fill(0x77_77_77, 0, 0, &[0x78_78_78])[3],
            u8::MAX
        );
    }

    #[test]
    fn popup_accent_text_on_the_panel_joins_the_coverage_floor() {
        // The Dojo prompt paints its unselected Terminate label in the accent
        // (selected_rail) straight onto the popup's panel fill.
        let mut theme = glassy_theme(0.0, 0.0);
        let menu = chrome_panel_palette(theme, ChromeSurface::Menu);
        let popup = chrome_panel_palette(theme, ChromeSurface::Popup);
        let coverage = |fill: [u8; 4]| f64::from(fill[3]) / 255.0;
        assert!(panel_text_readable(
            popup.panel,
            coverage(popup.panel_fill),
            &[popup.primary, popup.secondary, popup.selected_rail],
        ));
        assert!(popup.panel_fill[3] >= menu.panel_fill[3]);

        // An accent too dim to read even on the opaque panel keeps popups
        // opaque, while menus, which draw no accent text, stay translucent.
        theme.roles.accent = 0x20_22_30;
        let menu = chrome_panel_palette(theme, ChromeSurface::Menu);
        let popup = chrome_panel_palette(theme, ChromeSurface::Popup);
        assert!(menu.panel_fill[3] < u8::MAX);
        assert_eq!(popup.panel_fill[3], u8::MAX);
    }

    #[test]
    fn hovered_and_pressed_rows_keep_the_floor_on_translucent_panels() {
        // The review palette: the bare panel alone allowed 240/255 coverage,
        // where the hover highlight dropped secondary text to about 4.15:1, and
        // the press highlight left row text below 4.5:1 even on an opaque panel.
        let theme = ResolvedTheme {
            roles: ChromeRoles {
                surface: 0x63_05_86,
                text: 0x6e_dc_dd,
                selection: 0x9c_c3_58,
                ..ChromeRoles::default()
            },
            background_alpha: 42_597,
            shell: OmarchyShellSettings {
                menu_alpha: 0,
                popup_alpha: 0,
                text_size_px: None,
                text_size_held: false,
                ..OmarchyShellSettings::default()
            },
            ..ResolvedTheme::default()
        };
        for surface in [ChromeSurface::Menu, ChromeSurface::Popup] {
            let palette = chrome_panel_palette(theme, surface);
            let coverage = f64::from(palette.panel_fill[3]) / 255.0;
            let hover = (palette.selected_fill, ROW_HOVER_ALPHA);
            let press = (palette.selected_rail, ROW_PRESS_ALPHA);
            for overlays in [vec![], vec![hover], vec![press], vec![hover, press]] {
                let (primary, secondary) = palette.row_text(false, !overlays.is_empty());
                for backdrop in [0, 0x00ff_ffff, 0x0080_8080, 0x0063_0586, 0x006e_dcdd] {
                    let visible = visible_panel_color(palette.panel, coverage, &overlays, backdrop);
                    for text in [primary, secondary] {
                        let contrast = contrast_ratio(text, visible);
                        assert!(
                            contrast >= 4.5,
                            "{surface:?} {overlays:?} over {backdrop:06x}: {contrast:.2}"
                        );
                    }
                }
            }
        }
    }
}
