//! Trusted inline session-picker layout and CPU composition.

use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::Result;
use unicode_width::UnicodeWidthChar;

use crate::{
    color::{
        TEXT_MIN_CONTRAST, contrast_corrected, contrast_corrected_toward, contrast_floor_toward,
        contrast_ratio, mix_encoded, mix_linear, packed_channels, relative_luminance, rgb_channels,
    },
    config::{OmarchyShellSettings, ResolvedTheme},
    geometry::Rect,
    theme::{ChromeTints, Role},
};

use super::super::{
    ChromeSource, ChromeText, ChromeTextStyle, RenderContext, fill_rect, raster::premultiplied_rgba,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionPickerPurpose {
    RecentDojos,
    Dojos,
    Lairs,
    Remotes,
}

impl SessionPickerPurpose {
    /// The title set into the frame.
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::RecentDojos | Self::Dojos => "dojos",
            Self::Lairs => "lairs",
            Self::Remotes => "remotes",
        }
    }

    /// The input line's text while nothing is typed.
    pub(crate) const fn placeholder(self) -> &'static str {
        match self {
            Self::RecentDojos | Self::Dojos => "filter dojos",
            Self::Lairs => "filter lairs",
            Self::Remotes => "filter remotes",
        }
    }

    /// The creation row's label, after its `+`.
    pub(crate) const fn create_title(self) -> &'static str {
        match self {
            Self::RecentDojos => "New terminal",
            Self::Dojos => "New Dojo",
            Self::Lairs => "New Lair",
            Self::Remotes => "Add a profile",
        }
    }

    /// The line under the creation row when there is nothing to list.
    pub(crate) const fn empty_label(self) -> &'static str {
        match self {
            Self::RecentDojos => "No recent Dojos",
            Self::Dojos => "No Dojos",
            Self::Lairs => "No Lairs",
            Self::Remotes => "No remote profiles",
        }
    }

    /// The line when nothing matches what is typed.
    pub(crate) const fn no_match(self) -> &'static str {
        match self {
            Self::RecentDojos | Self::Dojos => "No matching Dojos",
            Self::Lairs => "No matching Lairs",
            Self::Remotes => "No matching remotes",
        }
    }

    pub(crate) const fn selection_verb(self) -> &'static str {
        match self {
            Self::RecentDojos => "open",
            Self::Dojos | Self::Lairs => "switch",
            Self::Remotes => "connect",
        }
    }

    /// The `dojos` picker's scope, `recent · this lair`, that Tab
    /// switches: which of the two this is. `None` for the others.
    pub(crate) const fn scope(self) -> Option<usize> {
        match self {
            Self::RecentDojos => Some(0),
            Self::Dojos => Some(1),
            Self::Lairs | Self::Remotes => None,
        }
    }
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

    /// Every way text meets a panel fill: straight on it, and on an
    /// unselected row under the translucent hover highlight, the press
    /// highlight, or both. Panels draw primary and secondary text and the
    /// danger label of a destructive action, such as Terminate, there; the
    /// accent rail only marks rows. Every panel can draw the text accent, the
    /// input line's `›`; popups also draw the raw accent as text: the Dojo
    /// prompt's unselected Restore or Revoke. The action palette, the
    /// session picker and the tab context menu blend those highlights before
    /// painting row text; selected rows sit on the opaque selected fill.
    fn panel_text_layers(self, palette: &SessionPickerPalette) -> Vec<PanelTextLayer> {
        let row_text = vec![
            palette.highlight_primary,
            palette.highlight_secondary,
            palette.highlight_danger,
        ];
        let mut layers = vec![PanelTextLayer {
            overlays: Vec::new(),
            text: match self {
                Self::Menu => vec![
                    palette.primary,
                    palette.secondary,
                    palette.danger,
                    palette.accent_text,
                ],
                Self::Popup => vec![
                    palette.primary,
                    palette.secondary,
                    palette.danger,
                    palette.accent_text,
                    palette.selected_rail,
                ],
            },
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

/// The least coverage from `configured` up at which every layer keeps the
/// floor; fully opaque when some layer misses it even there.
fn panel_layers_coverage(panel: u32, configured: f64, layers: &[PanelTextLayer]) -> f64 {
    if panel_layers_readable(panel, configured, layers) {
        return configured;
    }
    if !panel_layers_readable(panel, 1.0, layers) {
        return 1.0;
    }
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
/// The panel is drawn over the window's own glass with the shell alpha
/// layered over it, whatever the panel colour, the background or the raised
/// surface: its coverage is `1 - (1 - shell) * (1 - window)`. An opaque window
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
    let coverage = panel_layers_coverage(panel, configured, layers);
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
    /// A destructive action's label, such as Terminate, from the danger role
    /// or ANSI 9, whichever reads better on the panel. `danger` and
    /// `selected_danger` are mixed toward the text until they keep 4.5:1 on
    /// the panel and the selected fill; `highlight_danger` is corrected
    /// toward black or white until it keeps 4.5:1 under every highlight. On
    /// a translucent panel `danger` and `highlight_danger` are also mixed
    /// toward the text until they read at the primary text's coverage.
    pub(crate) danger: u32,
    pub(crate) selected_danger: u32,
    pub(crate) highlight_danger: u32,
    /// The accent as text straight on the panel, such as the input line's
    /// `›`: mixed toward the primary text until it keeps 4.5:1 there, and on
    /// a translucent panel until it reads at the primary text's coverage.
    pub(crate) accent_text: u32,
    /// The accent as text on the selected fill, such as a selected list
    /// row's title and `›`: mixed toward the selected text until it keeps
    /// 4.5:1 there. The selected fill is opaque, so it needs no coverage.
    pub(crate) selected_accent: u32,
    /// A field that edits a value, such as a rename: sunken, the panel
    /// moved a fifth of the way away from the text (darker on a dark
    /// theme, lighter on a light one), always opaque, with its text and
    /// accent kept at 4.5:1 on it.
    pub(crate) field: u32,
    pub(crate) field_primary: u32,
    pub(crate) field_accent: u32,
    /// A selected destructive button, such as Terminate: the danger colour
    /// as a fill, with its label in `danger_ink`, the panel moved until it
    /// keeps 4.5:1 there.
    pub(crate) danger_fill: u32,
    pub(crate) danger_ink: u32,
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

    /// A destructive action's label on the selected fill, under a hover or
    /// press highlight, or on the panel.
    pub(crate) const fn row_danger(&self, selected: bool, highlighted: bool) -> u32 {
        if selected {
            self.selected_danger
        } else if highlighted {
            self.highlight_danger
        } else {
            self.danger
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

/// The opaque palette of a popup or menu, which sits on the raised surface,
/// as Omarchy's own `[menu]` and `[popups]` do. Painters take
/// [`chrome_panel_palette`], which also follows the shell alpha.
#[cfg(test)]
#[must_use]
pub(crate) fn session_picker_palette(theme: ResolvedTheme) -> SessionPickerPalette {
    panel_palette(theme, theme.roles.surface_raised)
}

/// The opaque palette of a panel drawn in `panel`. Its text is the `text`
/// and `text-muted` roles and its selected row the `selection` role, each
/// raised to 4.5:1 against what it sits on; the scrim behind a modal panel
/// darkens the `surface`.
fn panel_palette(theme: ResolvedTheme, panel: u32) -> SessionPickerPalette {
    let roles = theme.roles;
    let primary = contrast_corrected(roles.text, panel);
    let secondary = contrast_corrected_toward(roles.text_muted, primary, panel);
    let selected_fill = roles.selection;
    let selected_primary = contrast_corrected(primary, selected_fill);
    let selected_secondary =
        contrast_corrected_toward(roles.text_muted, selected_primary, selected_fill);
    // Destructive labels are text, so they start from the theme's red or
    // bright red (ANSI 9), whichever reads better on the panel, unless
    // `[colors] danger` chose the colour.
    let danger_text = if roles.verbatim.contains(Role::Danger)
        || contrast_ratio(roles.danger, panel) >= contrast_ratio(theme.ansi[9], panel)
    {
        roles.danger
    } else {
        theme.ansi[9]
    };
    let danger = contrast_floor_toward(danger_text, primary, panel, TEXT_MIN_CONTRAST);
    let selected_danger = contrast_floor_toward(
        danger_text,
        selected_primary,
        selected_fill,
        TEXT_MIN_CONTRAST,
    );
    let accent_text = contrast_floor_toward(roles.accent, primary, panel, TEXT_MIN_CONTRAST);
    let selected_accent = contrast_floor_toward(
        roles.accent,
        selected_primary,
        selected_fill,
        TEXT_MIN_CONTRAST,
    );
    // A focus ring too close to the frame colour reads as no focus, so the
    // accent takes its place, unless `[colors] focus-ring` chose it.
    let focused_frame = if roles.verbatim.contains(Role::FocusRing)
        || contrast_ratio(roles.focus_ring, roles.divider) >= 1.2
    {
        roles.focus_ring
    } else {
        roles.accent
    };
    let [red, green, blue] = rgb_channels(modal_scrim(roles.surface));
    // Every opaque state an unselected highlighted row can be in.
    let highlight_states = [
        vec![(selected_fill, ROW_HOVER_ALPHA)],
        vec![(roles.accent, ROW_PRESS_ALPHA)],
        vec![
            (selected_fill, ROW_HOVER_ALPHA),
            (roles.accent, ROW_PRESS_ALPHA),
        ],
    ]
    .map(|overlays| visible_panel_color(panel, 1.0, &overlays, 0));
    let highlight_primary =
        contrast_corrected_against_all(primary, &highlight_states).unwrap_or(primary);
    let highlight_secondary =
        contrast_corrected_against_all(secondary, &highlight_states).unwrap_or(secondary);
    let highlight_danger =
        contrast_corrected_against_all(danger, &highlight_states).unwrap_or(danger);
    let away_from_text = if relative_luminance(primary) > relative_luminance(panel) {
        0
    } else {
        0x00ff_ffff
    };
    let field = mix_encoded(panel, away_from_text, FIELD_DEPTH);
    let field_primary = contrast_corrected(primary, field);
    let field_accent = contrast_floor_toward(roles.accent, field_primary, field, TEXT_MIN_CONTRAST);
    let danger_ink = contrast_corrected(panel, danger_text);
    let [panel_red, panel_green, panel_blue] = rgb_channels(panel);
    SessionPickerPalette {
        scrim: [red, green, blue, MODAL_SCRIM_ALPHA],
        panel,
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
        danger,
        selected_danger,
        highlight_danger,
        accent_text,
        selected_accent,
        field,
        field_primary,
        field_accent,
        danger_fill: danger_text,
        danger_ink,
        shadow: [0, 0, 0, 89],
        status_tints: Some(ChromeTints::on(&roles, panel)),
        selected_status_tints: Some(ChromeTints::on(&roles, selected_fill)),
    }
}

/// How far a sunken field moves from the panel away from the text.
const FIELD_DEPTH: f64 = 0.2;

/// How much of the scrim behind a modal panel covers the Window: one level
/// for every modal list and prompt (the command palette, key bindings,
/// Agent access, the pickers and the Dojo prompt), half the Window, as
/// Omarchy's own `[menu] scrim-alpha` of 0.5 dims the desktop behind its
/// menus. It replaces the 96, 112 and 140 those surfaces once used.
pub(crate) const MODAL_SCRIM_ALPHA: u8 = 128;

/// The scrim behind a modal panel: the `surface` darkened 80% toward black,
/// so it dims the Window in the theme's own hue, dark or light.
#[must_use]
pub(crate) fn modal_scrim(surface: u32) -> u32 {
    mix_linear(surface, 0, 0.80)
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
    chrome_panel_palette_on(theme, surface, theme.roles.surface_raised)
}

/// [`chrome_panel_palette`] for a panel drawn in `panel` instead of the
/// raised surface: the docked Explorer, which is part of the Window rather
/// than a popup over it.
#[must_use]
pub(crate) fn chrome_panel_palette_on(
    theme: ResolvedTheme,
    surface: ChromeSurface,
    panel: u32,
) -> SessionPickerPalette {
    let mut palette = panel_palette(theme, panel);
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
        // Each colour keeps its opaque correction when none fits both
        // backdrops; the coverage floor then raises the panel instead.
        for color in [
            &mut palette.highlight_primary,
            &mut palette.highlight_secondary,
            &mut palette.highlight_danger,
        ] {
            if let Some(corrected) = contrast_corrected_against_all(*color, &backgrounds) {
                *color = corrected;
            }
        }
        keep_secondary_text_within_primary_coverage(&mut palette, configured);
    }
    palette.panel_fill = chrome_panel_fill_layers(
        palette.panel,
        shell_alpha,
        theme.background_alpha,
        &surface.panel_text_layers(&palette),
    );
    palette
}

/// Muted, danger and accent text start from their roles, often right at the
/// floor on the opaque panel, so over a backdrop they would need a far more
/// opaque panel than the primary text does. On a translucent panel they are
/// mixed toward the primary text, plain or highlighted, only as far as they
/// need to read at the coverage the primary text needs: they never make the
/// panel more opaque than it.
fn keep_secondary_text_within_primary_coverage(
    palette: &mut SessionPickerPalette,
    configured: f64,
) {
    let overlays = highlight_overlays(palette);
    let layer = |overlays: &[(u32, u8)], color: u32| PanelTextLayer {
        overlays: overlays.to_vec(),
        text: vec![color],
    };
    let mut primary_layers = vec![layer(&[], palette.primary)];
    primary_layers.extend(
        overlays
            .iter()
            .map(|overlays| layer(overlays, palette.highlight_primary)),
    );
    let panel = palette.panel;
    let coverage = panel_layers_coverage(panel, configured, &primary_layers);
    let plain = |color| panel_layer_readable(panel, coverage, &layer(&[], color));
    let highlighted = |color| {
        overlays
            .iter()
            .all(|overlays| panel_layer_readable(panel, coverage, &layer(overlays, color)))
    };
    palette.secondary = mixed_until_readable(palette.secondary, palette.primary, plain);
    palette.danger = mixed_until_readable(palette.danger, palette.primary, plain);
    palette.accent_text = mixed_until_readable(palette.accent_text, palette.primary, plain);
    palette.highlight_secondary = mixed_until_readable(
        palette.highlight_secondary,
        palette.highlight_primary,
        highlighted,
    );
    palette.highlight_danger = mixed_until_readable(
        palette.highlight_danger,
        palette.highlight_primary,
        highlighted,
    );
}

/// `color` mixed toward `target`, in linear light, only as far as it takes
/// to become `readable`; `color` itself when it already is or `target` is
/// not.
fn mixed_until_readable(color: u32, target: u32, readable: impl Fn(u32) -> bool) -> u32 {
    if readable(color) || !readable(target) {
        return color;
    }
    let mut failing = 0.0_f64;
    let mut passing = 1.0_f64;
    for _ in 0..24 {
        let weight = f64::midpoint(failing, passing);
        if readable(mix_linear(color, target, weight)) {
            passing = weight;
        } else {
            failing = weight;
        }
    }
    mix_linear(color, target, passing)
}

/// The translucent highlights an unselected row can carry: hover, press, or
/// both, in paint order.
fn highlight_overlays(palette: &SessionPickerPalette) -> [Vec<(u32, u8)>; 3] {
    let hover = (palette.selected_fill, ROW_HOVER_ALPHA);
    let press = (palette.selected_rail, ROW_PRESS_ALPHA);
    [vec![hover], vec![press], vec![hover, press]]
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
    /// The truncated source an entry was shaped from, when it is not the
    /// key's own source: its tints are the ones that were drawn.
    truncated: HashMap<SessionPickerTextKey, ChromeSource>,
    recent_frames: VecDeque<HashSet<SessionPickerTextKey>>,
    /// One cell's width and height per style, at the latest scale and
    /// renderer generation only: what layouts measure before they paint.
    cell_metrics: HashMap<(ChromeTextStyle, u32, u64), (u32, u32)>,
}

impl SessionPickerTextCache {
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.truncated.clear();
        self.recent_frames.clear();
        self.cell_metrics.clear();
    }

    /// The width and height, in buffer pixels, of one chrome cell in
    /// `style`, as [`paint_picker_source`] lays text out. Chrome text is
    /// monospaced, so a label's width is its cells times this width.
    pub(crate) fn cell_metrics(
        &mut self,
        context: &RenderContext,
        style: ChromeTextStyle,
        scale_120: u32,
        renderer_generation: u64,
    ) -> Result<(u32, u32)> {
        let key = (style, scale_120, renderer_generation);
        if let Some(metrics) = self.cell_metrics.get(&key) {
            return Ok(*metrics);
        }
        let text = ChromeText::load_styled_with_context(" ", scale_120, style, context)?;
        let metrics = (text.frame.cell_width.max(1), text.pixel_height());
        self.cell_metrics.retain(|(_, scale, generation), _| {
            *scale == scale_120 && *generation == renderer_generation
        });
        self.cell_metrics.insert(key, metrics);
        Ok(metrics)
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
        self.truncated.retain(|key, _| retained.contains(key));
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    /// The text each cached entry draws, truncated where it was, with its
    /// style.
    #[cfg(test)]
    pub(super) fn drawn(&self) -> impl Iterator<Item = (&str, ChromeTextStyle)> {
        self.entries.keys().map(|key| {
            (
                self.truncated.get(key).unwrap_or(&key.source).text(),
                key.style,
            )
        })
    }

    #[cfg(test)]
    pub(super) fn contains_source(&self, text: &str) -> bool {
        self.drawn().any(|(drawn, _)| drawn == text)
    }
}

fn logical_coordinate_to_buffer(value: u32, scale_120: u32) -> u32 {
    value.saturating_mul(scale_120).div_ceil(120)
}

pub(super) fn picker_buffer_rect(rect: Rect, scale_120: u32) -> Rect {
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
    // The cells before the ellipsis: one fewer than the budget allows when
    // a wide character did not fit.
    let kept = text
        .chars()
        .map(|character| character.width().unwrap_or(0).min(2))
        .sum::<usize>()
        .saturating_sub(1);
    source.with_prefix_text(text, kept)
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
            cache.truncated.insert(key.clone(), truncated);
        }
        cache.entries.insert(key.clone(), text);
    }
    let text = &cache.entries[&key];
    // Tints follow the cells actually drawn: a truncated line's ellipsis
    // takes the line's color, never a clipped glyph's tint.
    let drawn = cache.truncated.get(&key).unwrap_or(source);
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
                .zip(drawn.tint_at(column))
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
#[cfg(test)]
mod tests {
    use super::super::super::pixel_index;
    use super::*;
    use crate::color::mix_encoded;
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
    fn a_truncated_tinted_line_paints_its_ellipsis_in_the_line_color() {
        use crate::theme::{ChromeTint, ChromeTints};

        let tinted = |text: &str| {
            let mut source = ChromeSource::default();
            source.push_tinted(ChromeTint::StatusError, |source| {
                source.push_str(text);
            });
            source
        };
        // The tint stops where the kept cells do, also when a wide
        // character gave its cell to the ellipsis.
        let cut = truncate_picker_source(&tinted("xxxxxx"), 4);
        assert_eq!(cut.text(), "xxx…");
        assert_eq!(cut.tint_at(2), Some(ChromeTint::StatusError));
        assert_eq!(cut.tint_at(3), None);
        let wide = truncate_picker_source(&tinted("界界界"), 4);
        assert_eq!(wide.text(), "界…");
        assert_eq!(wide.tint_at(1), Some(ChromeTint::StatusError));
        assert_eq!(wide.tint_at(2), None);

        // Painted: the cached, truncated source decides each cell's color.
        let context = RenderContext::new(u16::MAX);
        let mut cache = SessionPickerTextCache::default();
        let mut used = HashSet::new();
        let scale = 360;
        let cell = picker_cell_width(
            &mut cache,
            &context,
            &mut used,
            ChromeTextStyle::Regular,
            scale,
            0,
        )
        .unwrap();
        let tints = ChromeTints::on(&ChromeRoles::default(), 0);
        let (width, height) = (cell * 8, 160);
        let plain = 0x00ff_ffff;
        let paints = |canvas: &[u8], color: u32| {
            let [_, red, green, blue] = color.to_be_bytes();
            canvas
                .chunks_exact(4)
                .any(|pixel| pixel[..3] == [blue, green, red])
        };
        for _ in 0..2 {
            // The second paint reads the cache.
            let mut canvas = vec![0; (width * height * 4) as usize];
            paint_picker_source_tinted(
                &mut cache,
                &context,
                &mut used,
                &mut canvas,
                width,
                height,
                &tinted("xxxxxxxxxx"),
                ChromeTextStyle::Regular,
                scale,
                0,
                Rect {
                    x: 0,
                    y: 0,
                    width: cell * 4,
                    height,
                },
                PickerTextAlignment::Left,
                plain,
                Some(tints),
                u32::MAX,
            )
            .unwrap();
            assert!(paints(&canvas, tints.color(ChromeTint::StatusError)));
            assert!(paints(&canvas, plain), "the ellipsis is the line's color");
        }
    }

    #[test]
    fn picker_purpose_keeps_its_copy_distinct() {
        let cases = [
            (
                SessionPickerPurpose::RecentDojos,
                ["dojos", "filter dojos", "New terminal", "open"],
                Some(0),
            ),
            (
                SessionPickerPurpose::Dojos,
                ["dojos", "filter dojos", "New Dojo", "switch"],
                Some(1),
            ),
            (
                SessionPickerPurpose::Lairs,
                ["lairs", "filter lairs", "New Lair", "switch"],
                None,
            ),
            (
                SessionPickerPurpose::Remotes,
                ["remotes", "filter remotes", "Add a profile", "connect"],
                None,
            ),
        ];
        for (purpose, [title, placeholder, create, verb], scope) in cases {
            assert_eq!(purpose.title(), title);
            assert_eq!(purpose.placeholder(), placeholder);
            assert_eq!(purpose.create_title(), create);
            assert_eq!(purpose.selection_verb(), verb);
            assert_eq!(purpose.scope(), scope);
            assert!(purpose.no_match().starts_with("No matching "));
            assert!(purpose.empty_label().starts_with("No "));
        }
    }

    #[test]
    fn a_focus_ring_near_the_frame_gives_way_to_the_accent_unless_it_was_chosen() {
        let builtin = ResolvedTheme::default();
        let mut close = builtin;
        close.roles.focus_ring = close.roles.divider;
        assert_eq!(
            session_picker_palette(close).focused_frame,
            close.roles.accent
        );
        let chosen = ResolvedTheme::from_input_with_colors(
            &crate::theme::ThemeInput::builtin(),
            &crate::config::parse("[colors]\nfocus-ring=#7c7e80\n")
                .unwrap()
                .config
                .theme_colors(),
        );
        assert_eq!(chosen.roles.focus_ring, chosen.roles.divider);
        assert_eq!(session_picker_palette(chosen).focused_frame, 0x7c_7e_80);
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
            // One scrim level for every modal surface (Plan 0065).
            assert_eq!(palette.scrim[3], 128);
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
                surface_raised: 0x1a_1b_26,
                text: 0xc0_ca_f5,
                text_muted: 0x8f_97_bb,
                selection: 0x29_2e_42,
                danger: 0xf7_76_8e,
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
        // Secondary text corrected to exactly the floor on the opaque panel
        // has no headroom, so a translucent panel mixes it toward the primary
        // text instead of turning opaque.
        let mut floored = theme;
        floored.roles.surface = 0xf5_f1_e8;
        floored.roles.surface_raised = 0xf5_f1_e8;
        floored.roles.text = 0x22_24_28;
        // A readable accent: popups also draw it as text.
        floored.roles.accent = 0x2a_4a_8a;
        let opaque = session_picker_palette(floored);
        assert!(contrast_ratio(opaque.secondary, opaque.panel) < 4.6);
        let floored = chrome_panel_palette(floored, ChromeSurface::Popup);
        let alpha = floored.panel_fill[3];
        assert!(alpha < u8::MAX, "{alpha}");
        assert_ne!(floored.secondary, opaque.secondary);
        assert!(panel_text_readable(
            floored.panel,
            coverage(alpha),
            &[floored.secondary]
        ));
        // Text that misses the floor even on the opaque panel keeps it opaque.
        assert_eq!(
            chrome_panel_fill(0x77_77_77, 0, 0, &[0x78_78_78])[3],
            u8::MAX
        );
    }

    #[test]
    fn popup_accent_text_on_the_panel_joins_the_coverage_floor() {
        // The Dojo prompt paints its unselected Restore or Revoke label in
        // the accent (selected_rail) straight onto the popup's panel fill.
        let mut theme = glassy_theme(0.0, 0.0);
        let coverage = |fill: [u8; 4]| f64::from(fill[3]) / 255.0;
        let menu = chrome_panel_palette(theme, ChromeSurface::Menu);
        let popup = chrome_panel_palette(theme, ChromeSurface::Popup);
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
    fn danger_text_on_the_panel_joins_the_coverage_floor() {
        // The Dojo prompt, the tab menu and the command palette paint
        // Terminate in danger straight onto the panel fill.
        let mut theme = glassy_theme(0.0, 0.0);
        let coverage = |fill: [u8; 4]| f64::from(fill[3]) / 255.0;
        for surface in [ChromeSurface::Menu, ChromeSurface::Popup] {
            let palette = chrome_panel_palette(theme, surface);
            assert!(palette.panel_fill[3] < u8::MAX, "{surface:?}");
            assert!(panel_text_readable(
                palette.panel,
                coverage(palette.panel_fill),
                &[palette.primary, palette.secondary, palette.danger],
            ));
        }
        // A danger floored to exactly 4.5:1 on the opaque panel has no
        // headroom over a backdrop, so a translucent panel raises it further
        // rather than turning opaque.
        theme.roles.danger = 0x20_22_30;
        theme.ansi[9] = 0x20_22_30;
        let opaque = session_picker_palette(theme);
        assert!(contrast_ratio(opaque.danger, opaque.panel) < 4.6);
        let palette = chrome_panel_palette(theme, ChromeSurface::Menu);
        assert!(palette.panel_fill[3] < u8::MAX);
        assert_ne!(palette.danger, opaque.danger);
        assert!(panel_text_readable(
            palette.panel,
            coverage(palette.panel_fill),
            &[palette.danger],
        ));
    }

    #[test]
    fn hovered_and_pressed_rows_keep_the_floor_on_translucent_panels() {
        // The review palette: the bare panel alone allowed 240/255 coverage,
        // where the hover highlight dropped secondary text to about 4.15:1, and
        // the press highlight left row text below 4.5:1 even on an opaque panel.
        let theme = ResolvedTheme {
            roles: ChromeRoles {
                surface: 0x63_05_86,
                surface_raised: 0x63_05_86,
                text: 0x6e_dc_dd,
                text_muted: mix_linear(0x6e_dc_dd, 0x63_05_86, 0.30),
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

    /// What popups and menus paint for each theme source (Plan 0064
    /// milestone 5): the raised panel, its text and muted text, the
    /// selected row and its text, a destructive label on the panel and on
    /// the selected row, and the scrim.
    #[allow(clippy::type_complexity)]
    const PANEL_PINS: [(&str, [u32; 8]); 9] = [
        (
            "builtin",
            [
                0x20_23_27, 0xeb_eb_eb, 0x95_87_78, 0x35_4a_60, 0xeb_eb_eb, 0xfb_49_34, 0xf5_a0_9b,
                0x03_04_05,
            ],
        ),
        (
            "json",
            [
                0x21_28_30, 0xe0_e4_ea, 0x8a_91_98, 0x2a_3a_4a, 0xe0_e4_ea, 0xee_5c_6b, 0xeb_83_8d,
                0x03_06_0a,
            ],
        ),
        (
            "json-explicit",
            [
                0x21_28_30, 0xe0_e4_ea, 0x88_99_aa, 0x2a_3a_4a, 0xe0_e4_ea, 0xee_5c_6b, 0xeb_83_8d,
                0x03_06_0a,
            ],
        ),
        (
            "tokyo-night",
            [
                0x24_28_3b, 0xa9_b1_d6, 0x86_8e_af, 0x29_2e_42, 0xa9_b1_d6, 0xff_7a_93, 0xff_7a_93,
                0x07_07_0d,
            ],
        ),
        (
            "flexoki-light",
            [
                0xe6_e4_d9, 0x10_0f_0f, 0x62_61_5c, 0xce_cd_c3, 0x10_0f_0f, 0xb0_41_37, 0x97_37_2f,
                0x7c_7a_74,
            ],
        ),
        (
            "last-horizon",
            [
                0x0c_0b_0c, 0xfa_fc_fb, 0x98_95_96, 0x58_4e_51, 0xfa_fc_fb, 0xc3_8b_7b, 0xd8_bd_b5,
                0x02_02_02,
            ],
        ),
        (
            "legacy-gruvbox",
            [
                0x3c_38_36, 0xd4_be_98, 0xb2_9f_81, 0x50_49_45, 0xd4_be_98, 0xe4_8a_75, 0xd8_b3_90,
                0x0e_0e_0e,
            ],
        ),
        (
            "alacritty-kanagawa",
            [
                0x2e_2e_34, 0xdc_d7_ba, 0xbc_b8_a0, 0x2d_4f_67, 0xdc_d7_ba, 0xe5_7464, 0xe0_b1_99,
                0x09_09_0e,
            ],
        ),
        (
            "semantic-keys",
            [
                0x1f_1a_30, 0xd8_d4_ea, 0x90_8b_a8, 0x2a_25_40, 0xd8_d4_ea, 0xff_8a_94, 0xff_8a_94,
                0x05_04_09,
            ],
        ),
    ];

    #[test]
    fn popups_paint_the_raised_surface_and_the_text_roles_for_every_theme() {
        let themes = crate::config::tests::provider_themes();
        let painted = themes
            .iter()
            .map(|(name, theme)| {
                let palette = session_picker_palette(*theme);
                let [red, green, blue, alpha] = palette.scrim;
                assert_eq!(alpha, MODAL_SCRIM_ALPHA, "{name}");
                (
                    *name,
                    [
                        palette.panel,
                        palette.primary,
                        palette.secondary,
                        palette.selected_fill,
                        palette.selected_primary,
                        palette.danger,
                        palette.selected_danger,
                        packed_channels([red, green, blue]),
                    ],
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(painted, PANEL_PINS, "{painted:#08x?}");
        for (name, theme) in themes {
            let palette = session_picker_palette(theme);
            let roles = theme.roles;
            assert_eq!(palette.panel, roles.surface_raised, "{name}");
            assert_eq!(palette.selected_fill, roles.selection, "{name}");
            assert_eq!(
                packed_channels(palette.scrim[..3].try_into().unwrap()),
                modal_scrim(roles.surface),
                "{name}"
            );
            for (text, under) in [
                (palette.primary, palette.panel),
                (palette.secondary, palette.panel),
                (palette.danger, palette.panel),
                (palette.selected_primary, palette.selected_fill),
                (palette.selected_secondary, palette.selected_fill),
                (palette.selected_danger, palette.selected_fill),
            ] {
                assert!(
                    contrast_ratio(text, under) >= TEXT_MIN_CONTRAST,
                    "{name}: {text:06x} on {under:06x}"
                );
            }
            // Hovered and pressed rows, over the full selection fill.
            let hover = (palette.selected_fill, ROW_HOVER_ALPHA);
            let press = (palette.selected_rail, ROW_PRESS_ALPHA);
            for overlays in [vec![hover], vec![press], vec![hover, press]] {
                let under = visible_panel_color(palette.panel, 1.0, &overlays, 0);
                for text in [
                    palette.highlight_primary,
                    palette.highlight_secondary,
                    palette.highlight_danger,
                ] {
                    assert!(
                        contrast_ratio(text, under) >= TEXT_MIN_CONTRAST,
                        "{name}: {text:06x} under {overlays:x?}"
                    );
                }
            }
            // Text that already keeps its floor is the role itself.
            for (role, painted) in [
                (roles.text, palette.primary),
                (roles.text_muted, palette.secondary),
            ] {
                if contrast_ratio(role, palette.panel) >= TEXT_MIN_CONTRAST {
                    assert_eq!(painted, role, "{name}");
                }
            }
            // Danger text starts from red or bright red, whichever reads
            // better on the panel.
            let red = [roles.danger, theme.ansi[9]]
                .into_iter()
                .max_by(|left, right| {
                    contrast_ratio(*left, palette.panel)
                        .total_cmp(&contrast_ratio(*right, palette.panel))
                })
                .unwrap();
            if contrast_ratio(red, palette.panel) >= TEXT_MIN_CONTRAST {
                assert_eq!(palette.danger, red, "{name}");
            }
        }
    }

    /// The scrim darkens the theme's own surface, for a light theme too.
    #[test]
    fn the_scrim_darkens_the_surface() {
        for surface in [0x000e_1216, 0xff_fc_f0, 0x80_40_20] {
            let scrim = modal_scrim(surface);
            assert_eq!(scrim, mix_linear(surface, 0, 0.80));
            assert!(relative_luminance(scrim) < relative_luminance(surface));
        }
        assert_eq!(modal_scrim(0xff_ff_ff), 0x7c_7c_7c);
    }

    /// `[colors] surface-raised`, `selection` and `danger` recolour every
    /// popup and menu, and their text stays readable on them.
    #[test]
    fn role_overrides_recolor_popups_and_menus() {
        let theme = ResolvedTheme::from_input_with_colors(
            &crate::theme::ThemeInput::builtin(),
            &crate::config::tests::theme_colors(
                "[colors]\nsurface-raised=#2a1c3c\nselection=#5a3c7c\ndanger=#ff4060\n",
            ),
        );
        for palette in [
            session_picker_palette(theme),
            chrome_panel_palette(theme, ChromeSurface::Menu),
            chrome_panel_palette(theme, ChromeSurface::Popup),
        ] {
            assert_eq!(palette.panel, 0x2a_1c_3c);
            assert_eq!(palette.selected_fill, 0x5a_3c_7c);
            assert_eq!(palette.danger, 0xff_40_60);
            assert!(contrast_ratio(palette.selected_danger, 0x5a_3c_7c) >= TEXT_MIN_CONTRAST);
            assert_eq!(palette.row_danger(true, false), palette.selected_danger);
            assert_eq!(palette.row_danger(false, true), palette.highlight_danger);
            assert_eq!(palette.row_danger(false, false), palette.danger);
        }
        // A danger too faint for the panel is raised toward the text, even
        // when set by name: panels keep their text readable.
        let faint = ResolvedTheme::from_input_with_colors(
            &crate::theme::ThemeInput::builtin(),
            &crate::config::tests::theme_colors("[colors]\ndanger=#3a2020\n"),
        );
        let palette = session_picker_palette(faint);
        assert_eq!(faint.roles.danger, 0x3a_20_20);
        assert_ne!(palette.danger, 0x3a_20_20);
        assert!(contrast_ratio(palette.danger, palette.panel) >= TEXT_MIN_CONTRAST);
    }
}
