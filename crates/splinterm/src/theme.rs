//! Semantic chrome roles and the theme pipeline that fills them.
//!
//! A theme reaches the Window in three steps:
//!
//! 1. A [`ThemeProvider`] (Omarchy, the JSON `main.theme` file, or the
//!    built-in palette) reads its files into a [`ThemeInput`]: the terminal
//!    palette, the colors the theme suggests for roles ([`RoleHints`]), the
//!    explicit component colors a JSON theme names ([`ThemeComponents`]) and
//!    the theme's own shell values. Its [`ThemeProvider::watched_paths`] are
//!    what the live theme watcher fingerprints.
//! 2. The pure [`derive`] turns that input into [`ChromeRoles`], and
//!    `ResolvedTheme::from_input` adds the terminal palette and the chrome
//!    components derived from the roles.
//! 3. `ResolvedTheme::with_chrome_overrides` and `with_color_overrides`
//!    apply `config.ini`'s `[colors]` keys.
//!
//! Every chrome color the Window paints comes from a role or from a
//! component derived from roles. Plan 0064 milestone 2 introduced the roles
//! without changing a painted pixel, so a few roles still keep their legacy
//! derivation; each says so where it is derived.

#![allow(
    clippy::unreadable_literal,
    reason = "palette colors stay readable as plain RRGGBB hex"
)]

use std::path::PathBuf;

use anyhow::Result;

use crate::color::{
    NON_TEXT_MIN_CONTRAST, TEXT_MIN_CONTRAST, contrast_corrected, contrast_corrected_toward,
    contrast_floor_toward, contrast_ratio, legacy_srgb, mix_linear, strongest_contrast_endpoint,
};
use crate::config::OmarchyShellSettings;

/// The semantic colors Splinterm's own chrome is drawn from.
///
/// Which contrast floor a role keeps depends on where its color came from;
/// see [`RoleHints`] and [`derive`]. A derived `text_faint` keeps 4.5:1
/// against `surface`, and `danger`, `warning`, `success`, `info` and
/// `remote_marker` keep 3:1 whether hinted or not. Colors a theme names for
/// surfaces, `selection`, `divider`, `focus_ring` and `accent`, and the
/// legacy derivations kept for `text_muted` and `divider`, carry no floor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChromeRoles {
    /// The chrome background: the tab strip, the Explorer and panels. Today
    /// the terminal background.
    pub surface: u32,
    /// A surface raised above `surface`. Omarchy `lighter_background`.
    pub surface_raised: u32,
    /// A surface sunk below `surface`. Omarchy `dark_background`.
    pub surface_sunken: u32,
    /// The selected row's base color. Today the terminal selection.
    pub selection: u32,
    /// Dividers, frames and inactive pane borders.
    pub divider: u32,
    /// The active pane border and focused frames.
    pub focus_ring: u32,
    /// Primary chrome text.
    pub text: u32,
    /// Secondary chrome text, such as inactive tab labels.
    pub text_muted: u32,
    /// Tertiary chrome text.
    pub text_faint: u32,
    /// The application accent: the active tab underline and selection rails.
    pub accent: u32,
    /// Destructive actions and errors.
    pub danger: u32,
    /// Warnings.
    pub warning: u32,
    /// Success.
    pub success: u32,
    /// Information.
    pub info: u32,
    /// An agent waiting for input.
    pub status_needs_input: u32,
    /// An agent that reported an error.
    pub status_error: u32,
    /// An agent at work.
    pub status_working: u32,
    /// An agent that finished.
    pub status_done: u32,
    /// An idle agent.
    pub status_idle: u32,
    /// A status that is no longer current.
    pub status_stale: u32,
    /// The marker for a remote session.
    pub remote_marker: u32,
}

impl Default for ChromeRoles {
    /// The roles of the built-in palette.
    fn default() -> Self {
        derive(&ThemeInput::builtin())
    }
}

/// The terminal palette a theme gives the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalPalette {
    pub background: u32,
    pub foreground: u32,
    pub cursor: u32,
    pub selection: u32,
    pub selection_foreground: u32,
    pub url: u32,
    pub ansi: [u32; 16],
}

/// Colors a theme's own files suggest for roles. `None` leaves the role to
/// [`derive`]'s fallback. [`derive`] treats hints three ways:
///
/// - Used as given, with no contrast floor: `surface_raised`,
///   `surface_sunken`, `selection`, `divider`, `focus_ring` and `accent`.
/// - A starting point for the inactive-tab ramp: `text_muted` is mixed
///   toward `text` in quarter steps until it is readable; see
///   [`inactive_tab_foreground_fallback`].
/// - A starting point raised to the 3:1 non-text floor against `surface` by
///   mixing toward readable text: `danger`, `warning`, `success`, `info` and
///   `remote_marker`.
///
/// Hints are not user overrides. `[colors]` role overrides (Plan 0064 M4)
/// must be applied verbatim, after derivation, in their own layer rather
/// than entering here, or they would be ramped or floored like a hint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RoleHints {
    pub surface_raised: Option<u32>,
    pub surface_sunken: Option<u32>,
    pub selection: Option<u32>,
    pub divider: Option<u32>,
    pub focus_ring: Option<u32>,
    /// The theme's muted color: a starting point for the inactive-tab ramp.
    pub text_muted: Option<u32>,
    pub accent: Option<u32>,
    /// Starting points floored to 3:1 against the surface.
    pub danger: Option<u32>,
    pub warning: Option<u32>,
    pub success: Option<u32>,
    pub info: Option<u32>,
    pub remote_marker: Option<u32>,
}

/// Component colors a JSON theme names explicitly. Each is used exactly as
/// given, even below a contrast floor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThemeComponents {
    /// The selected Dojo tab's body.
    pub active_tab_background: Option<u32>,
    /// The selected Dojo tab's label.
    pub active_tab_foreground: Option<u32>,
    /// Inactive tab labels. When given it pins the `text_muted` role
    /// verbatim: no ramp and no contrast floor.
    pub inactive_tab_foreground: Option<u32>,
}

/// The shell values a theme's own files carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeShellInput {
    /// The terminal background's coverage, from Foot `alpha` or JSON `alpha`.
    pub background_alpha: u16,
    /// Foot `blur` or JSON `blur`.
    pub background_blur: bool,
    /// Omarchy's `shell.toml` settings. Providers give the defaults: the
    /// theme watcher layers the selected root's `shell.toml` afterwards,
    /// with its own gap and diagnostic rules.
    pub settings: OmarchyShellSettings,
}

/// Whether a theme is dark or light.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Appearance {
    #[default]
    Dark,
    Light,
}

impl Appearance {
    /// A background's appearance: light when black text contrasts more with
    /// it than white text does.
    #[must_use]
    pub fn of_background(background: u32) -> Self {
        if strongest_contrast_endpoint(background) == 0 {
            Self::Light
        } else {
            Self::Dark
        }
    }

    /// `dark` or `light`, as Omarchy `mode` and Foot `initial-color-theme`
    /// name them.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }
}

/// Everything a provider reads from one theme source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThemeInput {
    pub terminal: TerminalPalette,
    pub hints: RoleHints,
    pub components: ThemeComponents,
    pub shell: ThemeShellInput,
    /// The theme's appearance: what the theme says, else its background's.
    pub appearance: Appearance,
}

impl ThemeInput {
    /// The built-in palette, used when no theme source exists.
    #[must_use]
    pub fn builtin() -> Self {
        Self {
            terminal: TerminalPalette {
                background: 0x0e1216,
                foreground: 0xebebeb,
                cursor: 0xebebeb,
                selection: 0x354a60,
                selection_foreground: 0xebebeb,
                url: 0x78beff,
                ansi: [
                    0x1d2021, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0xa89984,
                    0x928374, 0xfb4934, 0xb8bb26, 0xfabd2f, 0x83a598, 0xd3869b, 0x8ec07c, 0xebdbb2,
                ],
            },
            hints: RoleHints {
                accent: Some(0x78d2ff),
                // The built-in muted color is its ANSI 8.
                text_muted: Some(0x928374),
                ..RoleHints::default()
            },
            components: ThemeComponents::default(),
            shell: ThemeShellInput {
                background_alpha: u16::MAX,
                background_blur: false,
                settings: OmarchyShellSettings::default(),
            },
            appearance: Appearance::Dark,
        }
    }
}

/// One theme source.
pub trait ThemeProvider {
    /// Reads the source.
    ///
    /// # Errors
    /// Returns an error when the source exists but cannot be read as one
    /// coherent, valid theme.
    fn load(&self) -> Result<ThemeInput>;

    /// The paths whose file identity decides when `load` must run again. The
    /// live theme watcher fingerprints each one every poll.
    fn watched_paths(&self) -> Vec<PathBuf>;
}

/// The built-in palette as a provider. It reads no files.
#[derive(Clone, Copy, Debug, Default)]
pub struct BuiltinThemeProvider;

impl ThemeProvider for BuiltinThemeProvider {
    fn load(&self) -> Result<ThemeInput> {
        Ok(ThemeInput::builtin())
    }

    fn watched_paths(&self) -> Vec<PathBuf> {
        Vec::new()
    }
}

/// WCAG AA contrast for normal-size text, against the opaque tab-strip
/// background.
pub(crate) const INACTIVE_TAB_MIN_CONTRAST: f64 = TEXT_MIN_CONTRAST;

/// The selected tab's label: whichever of the surface and the text contrasts
/// more with the tab's body.
pub(crate) fn active_tab_foreground_fallback(
    background: u32,
    foreground: u32,
    active_tab: u32,
) -> u32 {
    if contrast_ratio(background, active_tab) > contrast_ratio(foreground, active_tab) {
        background
    } else {
        foreground
    }
}

/// Picks muted inactive-tab label text. Tries the theme's muted color, then
/// mixes of it with 25, 50, and 75 % foreground, and takes the first that is
/// readable on the strip background and not louder than the foreground itself.
/// Otherwise, including when the foreground is below the floor, it keeps the
/// plain foreground.
///
/// The mixes are in encoded sRGB, a legacy kept so roles changed no pixel.
pub(crate) fn inactive_tab_foreground_fallback(
    background: u32,
    foreground: u32,
    muted: u32,
) -> u32 {
    let foreground_contrast = contrast_ratio(foreground, background);
    if foreground_contrast < INACTIVE_TAB_MIN_CONTRAST {
        return foreground;
    }
    (0..4)
        .map(|quarters| legacy_srgb::mix_quarters(muted, foreground, quarters))
        .find(|candidate| {
            let contrast = contrast_ratio(*candidate, background);
            (INACTIVE_TAB_MIN_CONTRAST..=foreground_contrast).contains(&contrast)
        })
        .unwrap_or(foreground)
}

/// Derives every chrome role from one theme input. Pure: the same input
/// always gives the same roles.
///
/// The roles follow Plan 0064's role table:
/// - `surface`, `text` are the terminal background and foreground;
///   `selection` is the theme's, else the terminal selection.
/// - `accent` is the theme's accent, else ANSI 4; `focus_ring` is the
///   theme's, else the accent.
/// - `divider` is the theme's, else the encoded-sRGB midpoint of surface and
///   text, a legacy kept for JSON and built-in themes. The Omarchy provider
///   always hints one, ending at ANSI 8.
/// - `text_muted` is a JSON theme's `inactive_tab_foreground`; else the
///   inactive-tab ramp from the theme's muted color, which keeps every
///   painted inactive tab label of a theme that names one (JSON and
///   built-in themes name their ANSI 8); else text mixed 30% toward the
///   surface, kept at 4.5:1.
/// - `text_faint` is text mixed 55% toward the surface, kept at 4.5:1.
/// - The hue roles are a hint, else the ANSI color for their hue, raised to
///   3:1 against the surface by mixing toward readable text.
/// - The agent status roles take herdr's mapping onto them.
#[must_use]
pub fn derive(input: &ThemeInput) -> ChromeRoles {
    let terminal = &input.terminal;
    let hints = &input.hints;
    let ansi = &terminal.ansi;
    let surface = terminal.background;
    let text = terminal.foreground;
    let accent = hints.accent.unwrap_or(ansi[4]);
    // Floors aim at readable text, not the raw foreground: a foreground
    // below 4.5:1 would otherwise send every floored role to pure black or
    // white, louder than the text and indistinguishable from each other.
    let readable = contrast_corrected(text, surface);
    let text_muted =
        input
            .components
            .inactive_tab_foreground
            .unwrap_or_else(|| match hints.text_muted {
                Some(muted) => inactive_tab_foreground_fallback(surface, text, muted),
                None => {
                    contrast_corrected_toward(mix_linear(text, surface, 0.30), readable, surface)
                }
            });
    let text_faint = contrast_corrected_toward(mix_linear(text, surface, 0.55), readable, surface);
    let signal = |hint: Option<u32>, fallback: u32| {
        contrast_floor_toward(
            hint.unwrap_or(fallback),
            readable,
            surface,
            NON_TEXT_MIN_CONTRAST,
        )
    };
    let danger = signal(hints.danger, ansi[1]);
    let warning = signal(hints.warning, ansi[3]);
    let success = signal(hints.success, ansi[2]);
    let info = signal(hints.info, ansi[6]);
    ChromeRoles {
        surface,
        surface_raised: hints
            .surface_raised
            .unwrap_or_else(|| mix_linear(surface, text, 0.08)),
        surface_sunken: hints
            .surface_sunken
            .unwrap_or_else(|| mix_linear(surface, 0, 0.06)),
        selection: hints.selection.unwrap_or(terminal.selection),
        divider: hints
            .divider
            .unwrap_or_else(|| legacy_srgb::midpoint(surface, text)),
        focus_ring: hints.focus_ring.unwrap_or(accent),
        text,
        text_muted,
        text_faint,
        accent,
        danger,
        warning,
        success,
        info,
        status_needs_input: danger,
        status_error: danger,
        status_working: warning,
        status_done: info,
        status_idle: success,
        status_stale: text_faint,
        remote_marker: signal(hints.remote_marker, ansi[5]),
    }
}

/// A role that colors part of a chrome text line, such as an agent status
/// glyph, independently of the line's own color. Sources carry the tag; the
/// painter picks the color for the background the line actually sits on.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ChromeTint {
    StatusNeedsInput,
    StatusError,
    StatusWorking,
    StatusDone,
    StatusIdle,
    StatusStale,
    /// The access "Asking" marker icon.
    Asking,
    /// The access "Asking" marker in ASCII, `A?`, which is text.
    AskingText,
    /// The remote host tag, which is text.
    Remote,
}

impl ChromeTint {
    const ALL: [Self; 9] = [
        Self::StatusNeedsInput,
        Self::StatusError,
        Self::StatusWorking,
        Self::StatusDone,
        Self::StatusIdle,
        Self::StatusStale,
        Self::Asking,
        Self::AskingText,
        Self::Remote,
    ];

    const fn index(self) -> usize {
        match self {
            Self::StatusNeedsInput => 0,
            Self::StatusError => 1,
            Self::StatusWorking => 2,
            Self::StatusDone => 3,
            Self::StatusIdle => 4,
            Self::StatusStale => 5,
            Self::Asking => 6,
            Self::AskingText => 7,
            Self::Remote => 8,
        }
    }

    const fn role(self, roles: &ChromeRoles) -> u32 {
        match self {
            Self::StatusNeedsInput => roles.status_needs_input,
            Self::StatusError => roles.status_error,
            Self::StatusWorking => roles.status_working,
            Self::StatusDone => roles.status_done,
            Self::StatusIdle => roles.status_idle,
            Self::StatusStale => roles.status_stale,
            Self::Asking | Self::AskingText => roles.warning,
            Self::Remote => roles.remote_marker,
        }
    }

    /// Glyphs keep the non-text floor, 3:1; the ASCII "Asking" marker and
    /// the remote tag are text and keep 4.5:1.
    const fn floor(self) -> f64 {
        match self {
            Self::AskingText | Self::Remote => TEXT_MIN_CONTRAST,
            _ => NON_TEXT_MIN_CONTRAST,
        }
    }
}

/// Every [`ChromeTint`]'s color on one background, each kept at its floor
/// against that background by mixing toward `text` corrected for it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ChromeTints([u32; 9]);

impl ChromeTints {
    #[must_use]
    pub(crate) fn on(roles: &ChromeRoles, background: u32) -> Self {
        let readable = contrast_corrected(roles.text, background);
        Self(ChromeTint::ALL.map(|tint| {
            contrast_floor_toward(tint.role(roles), readable, background, tint.floor())
        }))
    }

    pub(crate) const fn color(&self, tint: ChromeTint) -> u32 {
        self.0[tint.index()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_roles_keep_the_builtin_chrome_colors() {
        let roles = ChromeRoles::default();
        assert_eq!(roles.surface, 0x0e1216);
        assert_eq!(roles.text, 0xeb_eb_eb);
        assert_eq!(roles.selection, 0x35_4a_60);
        assert_eq!(roles.accent, 0x78_d2_ff);
        assert_eq!(roles.focus_ring, 0x78_d2_ff);
        assert_eq!(roles.divider, 0x7c_7e_80);
        assert_eq!(roles.text_muted, 0x92_83_74);
        assert!(BuiltinThemeProvider.watched_paths().is_empty());
        assert_eq!(BuiltinThemeProvider.load().unwrap(), ThemeInput::builtin());
    }

    #[test]
    fn unpainted_roles_follow_the_ansi_palette_and_their_contrast_floors() {
        let roles = ChromeRoles::default();
        let ansi = ThemeInput::builtin().terminal.ansi;
        // The built-in hues already clear 3:1 on the built-in surface.
        assert_eq!(roles.danger, ansi[1]);
        assert_eq!(roles.warning, ansi[3]);
        assert_eq!(roles.success, ansi[2]);
        assert_eq!(roles.info, ansi[6]);
        assert_eq!(roles.remote_marker, ansi[5]);
        assert_eq!(roles.status_needs_input, roles.danger);
        assert_eq!(roles.status_error, roles.danger);
        assert_eq!(roles.status_working, roles.warning);
        assert_eq!(roles.status_done, roles.info);
        assert_eq!(roles.status_idle, roles.success);
        assert_eq!(roles.status_stale, roles.text_faint);
        assert!(contrast_ratio(roles.text_faint, roles.surface) >= TEXT_MIN_CONTRAST);
        assert!(contrast_ratio(roles.text_faint, roles.surface) < 15.0);
        assert_ne!(roles.surface_raised, roles.surface);
        assert_ne!(roles.surface_sunken, roles.surface);

        // A hue too dark for its surface is mixed toward the text until it
        // clears the non-text floor.
        let mut input = ThemeInput::builtin();
        input.terminal.ansi[1] = 0x20_00_00;
        let roles = derive(&input);
        assert_ne!(roles.danger, 0x20_00_00);
        assert!(contrast_ratio(roles.danger, roles.surface) >= NON_TEXT_MIN_CONTRAST);

        // Hints win over the ANSI palette.
        input.hints.warning = Some(0xff_aa_00);
        input.hints.surface_raised = Some(0x22_22_22);
        let roles = derive(&input);
        assert_eq!(roles.warning, 0xff_aa_00);
        assert_eq!(roles.surface_raised, 0x22_22_22);
    }

    #[test]
    fn floors_aim_at_readable_text_when_the_foreground_is_too_faint() {
        // A foreground well under 4.5:1 on its background.
        let mut input = ThemeInput::builtin();
        input.terminal.background = 0x30_30_30;
        input.terminal.foreground = 0x50_50_50;
        input.terminal.ansi[1] = 0x40_10_10;
        input.terminal.ansi[2] = 0x10_40_10;
        input.terminal.ansi[3] = 0x40_40_10;
        input.terminal.ansi[6] = 0x10_40_40;
        input.terminal.ansi[5] = 0x40_10_40;
        let roles = derive(&input);
        let text_contrast = contrast_ratio(roles.text, roles.surface);
        assert!(text_contrast < TEXT_MIN_CONTRAST);
        let faint = contrast_ratio(roles.text_faint, roles.surface);
        assert!(faint >= TEXT_MIN_CONTRAST);
        assert!(
            faint < 7.0,
            "faint text stays near its floor instead of pure white: {faint}"
        );
        let signals = [
            roles.danger,
            roles.warning,
            roles.success,
            roles.info,
            roles.remote_marker,
        ];
        for signal in signals {
            assert!(contrast_ratio(signal, roles.surface) >= NON_TEXT_MIN_CONTRAST);
            assert_ne!(signal, 0x00ff_ffff);
            assert_ne!(signal, 0);
        }
        // The hues stay apart instead of collapsing onto one endpoint.
        for (index, signal) in signals.iter().enumerate() {
            assert!(!signals[index + 1..].contains(signal));
        }
    }

    #[test]
    fn tints_keep_their_floors_on_the_background_they_paint_on() {
        let roles = ChromeRoles::default();
        // Dark, light and mid-grey bodies, and one the size of the text.
        for background in [0x0e1216, 0xfffcf0, 0x928374, 0x777777, roles.text] {
            let tints = ChromeTints::on(&roles, background);
            for tint in ChromeTint::ALL {
                let contrast = contrast_ratio(tints.color(tint), background);
                let floor = if matches!(tint, ChromeTint::AskingText | ChromeTint::Remote) {
                    TEXT_MIN_CONTRAST
                } else {
                    NON_TEXT_MIN_CONTRAST
                };
                assert!(
                    contrast >= floor,
                    "{tint:?} on {background:06x}: {contrast}"
                );
            }
        }
        // A role that already clears its floor is painted as the theme gave it.
        let tints = ChromeTints::on(&roles, roles.surface);
        assert_eq!(tints.color(ChromeTint::StatusWorking), roles.status_working);
        assert_eq!(tints.color(ChromeTint::StatusDone), roles.status_done);
        assert_eq!(tints.color(ChromeTint::Asking), roles.warning);
        assert_eq!(tints.color(ChromeTint::StatusStale), roles.status_stale);
        // Needs input and error share danger; their glyphs tell them apart.
        assert_eq!(
            tints.color(ChromeTint::StatusNeedsInput),
            tints.color(ChromeTint::StatusError)
        );
    }

    #[test]
    fn appearance_reads_dark_or_light() {
        assert_eq!(Appearance::parse(" Light "), Some(Appearance::Light));
        assert_eq!(Appearance::parse("dark"), Some(Appearance::Dark));
        assert_eq!(Appearance::parse("dusk"), None);
        assert_eq!(Appearance::of_background(0x0e1216), Appearance::Dark);
        assert_eq!(Appearance::of_background(0xfffcf0), Appearance::Light);
        assert_eq!(ThemeInput::builtin().appearance, Appearance::Dark);
    }
}
