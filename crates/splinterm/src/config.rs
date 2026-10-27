//! Project-owned MVP configuration and native Omarchy palette integration.
//!
//! This parser deliberately accepts only the documented Foot-compatible subset.
//! Unknown sections and keys are diagnostics, never silently accepted compatibility.

#![allow(
    clippy::too_many_lines,
    clippy::unreadable_literal,
    reason = "configuration tables remain more auditable when values stay adjacent to their keys"
)]

use std::{
    collections::HashMap,
    env,
    ffi::OsString,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use splinterd::dialog_shortcut::DialogShortcut;
use splinterm_protocol::{LaunchDirectory, MAX_COLUMNS, MAX_ROWS};

use crate::{
    font_shaping::{FeatureSettings, FontLigatures},
    geometry::{FontSize, FontSizingPolicy, TerminalPadding},
    keymap::{KeymapProfile, ResolvedKeymap, resolve_keymap},
    preset::PresetCatalog,
    theme::{
        Appearance, BuiltinThemeProvider, ChromeRoles, RoleHints, TerminalPalette, ThemeComponents,
        ThemeInput, ThemeProvider, ThemeShellInput, active_tab_foreground_fallback, derive,
    },
};

pub const APP_ID: &str = "com.oldjobobo.splinterm";
/// The Wayland app ID of every trusted dialog (access consent, upgrade,
/// restart), so the desktop can map them without keyboard focus: the Omarchy
/// integration's Hyprland module gives it `no_initial_focus`.
pub const TRUSTED_DIALOG_APP_ID: &str = splinterd::dialog_shortcut::TRUSTED_DIALOG_APP_ID;
pub const DEFAULT_FONT: &str = "monospace:style=Regular";
pub const STARTUP_FONT_FALLBACK: &str = "JetBrains Mono Nerd Font:style=Regular";
/// The largest `[multiplexer] pane-gap`, in logical px.
pub const MAX_PANE_GAP: u32 = 64;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FontAuthority {
    #[default]
    NativeOmarchy,
    Explicit,
}

#[derive(Clone, Debug, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent shell, cursor, preset, and presentation switches from config.ini"
)]
pub struct AppConfig {
    pub font: String,
    pub font_ligatures: FontLigatures,
    pub font_features: FeatureSettings,
    pub font_authority: FontAuthority,
    pub font_size: FontSize,
    /// `Explicit` when `config.ini` sets a font size; otherwise Splinterm
    /// follows the Omarchy text size (`shell.toml` `[font] base-size`).
    pub font_size_authority: FontAuthority,
    pub font_sizing_policy: FontSizingPolicy,
    /// `[main] ui-font-pixelsize`: the chrome text size in logical px. When
    /// unset, chrome follows Omarchy's text size, otherwise 14 px
    /// ([`resolve_ui_font_size`]). Never the terminal font size.
    pub ui_font_size: Option<FontSize>,
    pub padding: TerminalPadding,
    pub initial_columns: u16,
    pub initial_rows: u16,
    pub shell: Option<String>,
    pub login_shell: bool,
    pub title: Option<String>,
    pub scrollback_lines: usize,
    pub cursor_style: CursorStyle,
    pub cursor_blink: bool,
    /// What a commandless desktop/XDG launch opens (`[main] startup`).
    pub startup: StartupMode,
    pub resize_delay_ms: u64,
    /// Optional Foot-compatible background-alpha override (`[colors] alpha`).
    /// When absent, the active theme owns background translucency.
    pub background_alpha: Option<u16>,
    /// Optional native background-blur override (`[colors] blur`).
    /// When absent, the active theme owns the requested blur state.
    pub background_blur: Option<bool>,
    /// Optional chrome background overrides (`[colors] active-tab-*`,
    /// `tab-strip-alpha`, `explorer-*`). Absent keys keep the theme's value.
    pub chrome_colors: ChromeColorOverrides,
    /// Explicit project JSON override. When absent, Splinterm follows the
    /// active Omarchy theme directly.
    pub theme_path: Option<PathBuf>,
    /// Explicit absolute private directory for user-requested clipboard PNG saves.
    /// Filesystem checks happen in the save worker, not during configuration parsing.
    pub clipboard_image_directory: Option<PathBuf>,
    pub pane_divider_style: PaneDividerStyle,
    /// `[multiplexer] pane-gap`: empty logical px between adjacent Splint
    /// panes' outer edges, outside any frame or around a divider line. Zero
    /// is the classic tiled layout; the Window's outer edges never get a gap.
    pub pane_gap: u32,
    pub frame_title_mode: FrameTitleMode,
    pub multiplexer_lifetime: MultiplexerLifetimeConfig,
    /// Where a new Dojo or Lair created inside a Window starts
    /// (`[multiplexer] new-dojo-directory`).
    pub new_dojo_directory: LaunchDirectory,
    /// Where a new Splint split inside a Dojo starts
    /// (`[multiplexer] new-splint-directory`).
    pub new_splint_directory: LaunchDirectory,
    /// Effective, fully validated client-local keymap.
    pub keymap: ResolvedKeymap,
    pub keymap_profile: KeymapProfile,
    pub keymap_path: Option<PathBuf>,
    pub prefix_timeout_ms: u64,
    /// Explicit client-local static preset catalog, resolved relative to config.ini.
    pub preset_path: Option<PathBuf>,
    pub preset_catalog: Option<PresetCatalog>,
    pub allow_unrestricted_commands: bool,
    /// Whether explicitly reported agent status is shown in the Explorer, on
    /// Dojo tabs, and in navigation accessibility (`[explorer] agent-status`).
    pub explorer_agent_status: bool,
    /// `[explorer] status-colors` and `status-glyphs`.
    pub explorer_status_style: AgentStatusStyle,
    /// Whether a shown tab strip draws a full-width bottom border in the
    /// light `line` Splint divider stroke and colour, under the active tab's
    /// thicker underline (`[multiplexer] tab-strip-border`, default on).
    pub tab_strip_border: bool,
    /// Whether a docked Lair Explorer runs the Window's full height, with the
    /// tab strip beginning at its right edge, instead of sitting below a
    /// full-width tab strip (`[explorer] full-height`, default on).
    pub explorer_full_height: bool,
    /// Whether a new Window's Explorer first shows collapsed to its rail,
    /// one row per Lair, instead of the full tree (`[explorer] collapsed`,
    /// default off).
    pub explorer_collapsed: bool,
    /// Whether a graphical client under Omarchy may show the one-time hint
    /// that recommends `splinterm integration omarchy enable`
    /// (`[main] omarchy-integration-hint`).
    pub omarchy_integration_hint: bool,
    /// The chord that engages a waiting trusted dialog's keyboard, which
    /// Splinterm also binds in Hyprland while a dialog waits; `None` turns
    /// it off (`[integration] dialog-shortcut`, default `ctrl+alt+g`).
    pub dialog_shortcut: Option<DialogShortcut>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CursorStyle {
    #[default]
    Block,
    Beam,
    Underline,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StartupMode {
    #[default]
    NewDojo,
    DojoPicker,
}

/// How agent status is drawn in the Explorer and on Dojo tabs
/// (`[explorer] status-colors` and `status-glyphs`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AgentStatusStyle {
    /// Agent status glyphs, the access "Asking" marker and the remote host
    /// tag take their status colors; off, they take the text around them.
    pub colors: bool,
    pub glyphs: StatusGlyphs,
}

impl Default for AgentStatusStyle {
    fn default() -> Self {
        Self {
            colors: true,
            glyphs: StatusGlyphs::Icons,
        }
    }
}

/// `[explorer] status-glyphs`. Each state keeps its own shape in both, so
/// color is never the only signal.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StatusGlyphs {
    /// Two-cell icons from the embedded icon face.
    #[default]
    Icons,
    /// One-cell geometric shapes from the chrome font.
    Dots,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PaneDividerStyle {
    None,
    #[default]
    Line,
    Frame,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FrameTitleMode {
    None,
    #[default]
    Splint,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MultiplexerLifetimeConfig {
    /// Whether ordinary unnamed local graphical terminals create durable Lairs.
    pub persistent_by_default: bool,
    /// Whether creating or explicitly naming a Dojo promotes its transient Lair.
    pub persist_on_tab_organization: bool,
    /// Whether Close Tab and Window close terminate Dojos that were never used.
    pub close_unused: bool,
}

impl Default for MultiplexerLifetimeConfig {
    fn default() -> Self {
        Self {
            persistent_by_default: true,
            persist_on_tab_organization: true,
            close_unused: false,
        }
    }
}

/// The settings a Window's topology manager reads from its configuration that
/// Reload configuration changes. The Window publishes them on reload and the
/// manager picks up the latest before each decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TopologyReloadSettings {
    pub close_unused: bool,
    pub persist_on_tab_organization: bool,
    pub new_dojo_directory: LaunchDirectory,
    pub new_splint_directory: LaunchDirectory,
}

impl TopologyReloadSettings {
    #[must_use]
    pub fn from_config(config: &AppConfig) -> Self {
        Self {
            close_unused: config.multiplexer_lifetime.close_unused,
            persist_on_tab_organization: config.multiplexer_lifetime.persist_on_tab_organization,
            new_dojo_directory: config.new_dojo_directory.clone(),
            new_splint_directory: config.new_splint_directory.clone(),
        }
    }

    /// Writes these settings over the matching fields of `config`.
    pub fn apply_to(&self, config: &mut AppConfig) {
        config.multiplexer_lifetime.close_unused = self.close_unused;
        config.multiplexer_lifetime.persist_on_tab_organization = self.persist_on_tab_organization;
        config.new_dojo_directory = self.new_dojo_directory.clone();
        config.new_splint_directory = self.new_splint_directory.clone();
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            font: DEFAULT_FONT.to_owned(),
            font_ligatures: FontLigatures::Off,
            font_features: FeatureSettings::default(),
            font_authority: FontAuthority::NativeOmarchy,
            font_size: FontSize::Pixels(14.0),
            font_size_authority: FontAuthority::NativeOmarchy,
            font_sizing_policy: FontSizingPolicy::OutputScale,
            ui_font_size: None,
            padding: TerminalPadding::DEFAULT,
            initial_columns: 80,
            initial_rows: 24,
            shell: None,
            login_shell: true,
            title: None,
            scrollback_lines: 1_000,
            cursor_style: CursorStyle::Block,
            cursor_blink: true,
            startup: StartupMode::NewDojo,
            resize_delay_ms: 100,
            background_alpha: None,
            background_blur: None,
            chrome_colors: ChromeColorOverrides::default(),
            theme_path: None,
            clipboard_image_directory: None,
            pane_divider_style: PaneDividerStyle::Line,
            pane_gap: 0,
            frame_title_mode: FrameTitleMode::Splint,
            multiplexer_lifetime: MultiplexerLifetimeConfig::default(),
            new_dojo_directory: LaunchDirectory::Home,
            new_splint_directory: LaunchDirectory::Inherit,
            keymap: ResolvedKeymap::default(),
            keymap_profile: KeymapProfile::Splinterm,
            keymap_path: None,
            prefix_timeout_ms: 1_000,
            preset_path: None,
            preset_catalog: Some(PresetCatalog::bundled()),
            allow_unrestricted_commands: false,
            explorer_agent_status: true,
            explorer_status_style: AgentStatusStyle::default(),
            tab_strip_border: true,
            explorer_full_height: true,
            explorer_collapsed: false,
            omarchy_integration_hint: true,
            dialog_shortcut: Some(DialogShortcut::default()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConfigLoad {
    pub config: AppConfig,
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ThemeSource {
    Omarchy(OmarchyThemeRoots),
    /// An explicit JSON palette. `shell` names the Omarchy roots whose
    /// `shell.toml` still supplies the text size; panel alpha stays opaque.
    Json {
        path: PathBuf,
        shell: Option<OmarchyThemeRoots>,
    },
}

impl AppConfig {
    #[must_use]
    pub fn theme_source(&self) -> ThemeSource {
        self.theme_path.clone().map_or_else(
            || ThemeSource::Omarchy(OmarchyThemeRoots::from_environment()),
            |path| ThemeSource::Json {
                path,
                shell: Some(OmarchyThemeRoots::from_environment()),
            },
        )
    }

    /// Whether the font size follows Omarchy's `[font] base-size`: only while
    /// `config.ini` sets no font size.
    #[must_use]
    pub fn follows_omarchy_text_size(&self) -> bool {
        self.font_size_authority == FontAuthority::NativeOmarchy
    }
}

/// The active Omarchy theme directories Splinterm may read, highest priority
/// first:
///
/// 1. `${XDG_STATE_HOME:-$HOME/.local/state}/omarchy/current/theme`;
/// 2. `$HOME/.local/state/omarchy/current/theme`, where Omarchy 4 hard-codes
///    its writes, when `XDG_STATE_HOME` names a different directory;
/// 3. Omarchy 3.x `$HOME/.config/omarchy/current/theme`, read-only.
///
/// A candidate is *claimed* while its theme directory, or the theme Omarchy
/// stages beside it as `current/next-theme`, exists. A theme switch stages the
/// next theme before it removes the current one, so a claimed root with a
/// missing theme is a transient gap, not a reason to fall through to a stale
/// lower-priority root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OmarchyThemeRoots {
    candidates: Vec<PathBuf>,
    /// The machine-level `~/.config/omarchy/shell.toml` override, layered over
    /// the active theme's `shell.toml`. Omarchy hard-codes `$HOME/.config`.
    user_shell: Option<PathBuf>,
}

/// Consecutive polls a live watcher keeps its last good root while the walk
/// lands on a lower-priority root, so a brief whole-`current` gap cannot flap
/// to an Omarchy 3.x leftover. A sustained demotion is then accepted.
pub const OMARCHY_ROOT_DEMOTION_GRACE_POLLS: u32 = 4;

impl OmarchyThemeRoots {
    /// One fixed theme directory, for tests and explicit callers.
    #[must_use]
    pub fn fixed(theme_dir: PathBuf) -> Self {
        Self {
            candidates: vec![theme_dir],
            user_shell: None,
        }
    }

    /// Explicit candidates, highest priority first; `None` when empty.
    #[must_use]
    pub fn ordered(candidates: Vec<PathBuf>) -> Option<Self> {
        (!candidates.is_empty()).then_some(Self {
            candidates,
            user_shell: None,
        })
    }

    /// Layers an explicit user `shell.toml` override over the theme's.
    #[must_use]
    pub fn with_user_shell(mut self, path: PathBuf) -> Self {
        self.user_shell = Some(path);
        self
    }

    #[must_use]
    pub fn user_shell(&self) -> Option<&Path> {
        self.user_shell.as_deref()
    }

    #[must_use]
    pub fn from_environment() -> Self {
        Self::from_values(env::var_os("XDG_STATE_HOME"), env::var_os("HOME"))
    }

    fn from_values(xdg_state_home: Option<OsString>, home: Option<OsString>) -> Self {
        let home = home
            .filter(|value| !value.is_empty())
            .map_or_else(|| PathBuf::from("."), PathBuf::from);
        let home_state = home.join(".local/state/omarchy/current/theme");
        let xdg_state = xdg_state_home
            .filter(|value| !value.is_empty())
            .map(|value| PathBuf::from(value).join("omarchy/current/theme"));
        let mut candidates = Vec::with_capacity(3);
        if let Some(xdg_state) = xdg_state.filter(|path| *path != home_state) {
            candidates.push(xdg_state);
        }
        candidates.push(home_state);
        candidates.push(home.join(".config/omarchy/current/theme"));
        Self {
            candidates,
            user_shell: Some(home.join(".config/omarchy/shell.toml")),
        }
    }

    #[must_use]
    pub fn candidates(&self) -> &[PathBuf] {
        &self.candidates
    }

    /// A root is claimed while its theme directory or the theme Omarchy is
    /// staging next to it exists. `omarchy-theme-set` writes
    /// `current/next-theme` before it removes `current/theme`, so one of the two
    /// exists throughout a switch, while a stale empty `current` left in a
    /// higher root never shadows the live theme.
    fn claimed(&self) -> Option<usize> {
        self.candidates.iter().position(|theme_dir| {
            theme_dir.is_dir() || theme_dir.with_file_name("next-theme").is_dir()
        })
    }

    /// Whether any root is claimed. A selected root that is missing while
    /// another is claimed is a transient gap, not an absent Omarchy.
    #[must_use]
    pub fn any_claimed(&self) -> bool {
        self.claimed().is_some()
    }

    /// The stateless choice for one-shot reads: the first claimed root,
    /// otherwise the highest-priority root.
    #[must_use]
    pub fn select(&self) -> &Path {
        &self.candidates[self.claimed().unwrap_or(0)]
    }
}

/// Live root selection. It prefers the last root that loaded successfully
/// while the walk briefly lands on a lower-priority root.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OmarchyThemeSelection {
    last_good: Option<usize>,
    demotion_polls: u32,
}

impl OmarchyThemeSelection {
    /// Chooses the candidate index for one poll.
    pub fn select(&mut self, roots: &OmarchyThemeRoots) -> usize {
        match (roots.claimed(), self.last_good) {
            (Some(claimed), Some(last_good)) if claimed > last_good => {
                self.demotion_polls = self.demotion_polls.saturating_add(1);
                if self.demotion_polls <= OMARCHY_ROOT_DEMOTION_GRACE_POLLS {
                    last_good
                } else {
                    claimed
                }
            }
            (None, Some(last_good)) => last_good,
            (Some(claimed), _) => {
                self.demotion_polls = 0;
                claimed
            }
            (None, None) => 0,
        }
    }

    /// Whether `index` is the root that last loaded successfully, which the
    /// selection keeps through gaps.
    #[must_use]
    pub fn holds(&self, index: usize) -> bool {
        self.last_good == Some(index)
    }

    /// Records that the candidate at `index` produced a coherent theme.
    pub fn accepted(&mut self, index: usize) {
        self.last_good = Some(index);
        self.demotion_polls = 0;
    }
}

/// The Omarchy shell's `[font] base-size` default and floor, in px. The shell
/// uses the default when no layer sets a parseable value and floors smaller
/// values at 1 px; only `omarchy-display-text-size` restricts input to 9–20.
pub const OMARCHY_DEFAULT_TEXT_SIZE_PX: u16 = 12;
const OMARCHY_MIN_TEXT_SIZE_PX: u16 = 1;

/// The point sizes Splinterm's renderer accepts for a base font size.
const RENDERER_POINT_SIZES: std::ops::RangeInclusive<u16> = 6..=96;

/// The Omarchy 4 bar's `[bar] size-horizontal` default, in logical px: the
/// height of the stock top bar, which Splinterm's tab strip matches.
pub const OMARCHY_DEFAULT_BAR_SIZE_PX: u16 = 26;

/// The tab strip heights Splinterm accepts from `[bar] size-horizontal`, in
/// logical px. Below 20 px no readable chrome text fits beside the active-tab
/// indicator; above 48 px the strip is taller than any stock Omarchy bar.
pub const OMARCHY_BAR_SIZE_LIMITS_PX: std::ops::RangeInclusive<u16> = 20..=48;

/// The Omarchy shell settings Splinterm follows from the layered `shell.toml`
/// (the active theme's file, then the user's `~/.config/omarchy/shell.toml`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OmarchyShellSettings {
    /// `[menu] background-alpha`, as Foot-style 16-bit coverage.
    pub menu_alpha: u16,
    /// `[popups] background-alpha`, as Foot-style 16-bit coverage.
    pub popup_alpha: u16,
    /// Effective `[font] base-size` in px for the terminal, or `None` when no
    /// layer sets it or `config.ini` sets a terminal font size.
    pub text_size_px: Option<u16>,
    /// Effective `[font] base-size` in px for chrome text, or `None` when no
    /// layer sets it. Kept whatever `config.ini` says, because chrome follows
    /// Omarchy's text size independently of the terminal font size, and
    /// `[main] ui-font-pixelsize` wins over it in the Window.
    pub ui_text_size_px: Option<u16>,
    /// `[bar] size-horizontal`, the Omarchy top bar's height, in logical px
    /// and within [`OMARCHY_BAR_SIZE_LIMITS_PX`]. The tab strip uses it.
    pub bar_size_px: u16,
    /// A running window's `base-size` became unparseable. `text_size_px`
    /// then repeats the last followed request, and the window keeps the size
    /// it has actually applied instead of retrying that request.
    pub text_size_held: bool,
}

impl Default for OmarchyShellSettings {
    /// Omarchy's template defaults: opaque menus and popups, no text size and
    /// the stock 26 px bar.
    fn default() -> Self {
        Self {
            menu_alpha: u16::MAX,
            popup_alpha: u16::MAX,
            text_size_px: None,
            ui_text_size_px: None,
            bar_size_px: OMARCHY_DEFAULT_BAR_SIZE_PX,
            text_size_held: false,
        }
    }
}

/// The chrome text size, in logical px, when neither
/// `[main] ui-font-pixelsize` nor Omarchy's text size sets one: the size
/// chrome drew at with Splinterm's default 14 px terminal font.
pub const DEFAULT_UI_FONT_PIXELSIZE: f32 = 14.0;

/// The sizes `[main] ui-font-pixelsize` accepts, in logical px, and the range
/// an Omarchy text size is clamped to for chrome. Chrome rows cap what is
/// drawn well below the top of this range.
pub const UI_FONT_PIXELSIZE_LIMITS: std::ops::RangeInclusive<f32> = 6.0..=96.0;

/// The chrome text size: `[main] ui-font-pixelsize` when set; otherwise
/// Omarchy's `[font] base-size` in px, the size Omarchy's own shell draws
/// its text at, clamped to [`UI_FONT_PIXELSIZE_LIMITS`]; otherwise
/// [`DEFAULT_UI_FONT_PIXELSIZE`]. The terminal font size and runtime zoom
/// never take part.
#[must_use]
pub fn resolve_ui_font_size(configured: Option<FontSize>, omarchy_px: Option<u16>) -> FontSize {
    configured.unwrap_or_else(|| {
        FontSize::Pixels(omarchy_px.map_or(DEFAULT_UI_FONT_PIXELSIZE, |px| {
            f32::from(px).clamp(
                *UI_FONT_PIXELSIZE_LIMITS.start(),
                *UI_FONT_PIXELSIZE_LIMITS.end(),
            )
        }))
    })
}

/// Which followed setting a shell diagnostic concerns, so a caller can drop
/// diagnostics for settings it ignores.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OmarchyShellConcern {
    /// A `shell.toml` layer could not be read.
    Read,
    /// `[menu]` or `[popups]` `background-alpha`.
    PanelAlpha,
    /// `[font] base-size`, which the terminal and chrome text both follow.
    TextSize,
    /// `[font] base-size` maps to a point size outside what Splinterm
    /// renders. Only the terminal uses that point size; chrome takes the px.
    TerminalTextSize,
    /// `[bar] size-horizontal`.
    BarSize,
}

/// Effective shell settings plus diagnostics for rejected or adjusted values.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OmarchyShellLoad {
    pub settings: OmarchyShellSettings,
    pub diagnostics: Vec<(OmarchyShellConcern, String)>,
}

/// One `shell.toml` layer: its path, for diagnostics, and its contents when
/// the file exists.
#[derive(Clone, Copy, Debug)]
pub struct OmarchyShellLayer<'a> {
    pub path: &'a Path,
    pub contents: Option<&'a str>,
}

/// `-?\d+(\.\d+)?`: the shell's numeric value shape.
fn is_shell_number(text: &str) -> bool {
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    let unsigned = text.strip_prefix('-').unwrap_or(text);
    match unsigned.split_once('.') {
        Some((whole, fraction)) => digits(whole) && digits(fraction),
        None => digits(unsigned),
    }
}

/// Whether a bare (unquoted) value has one of the shapes the shell's
/// `parseShell` accepts: a number, a border-width list of two to four numbers,
/// or a word `[A-Za-z][A-Za-z0-9_-]*`.
fn is_shell_bare_value(value: &str) -> bool {
    let numbers = value.split_whitespace().collect::<Vec<_>>();
    let word = value
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    word || ((1..=4).contains(&numbers.len()) && numbers.iter().all(|part| is_shell_number(part)))
}

/// Parses `shell.toml` the way the Omarchy shell's `Color.parseShell` does:
/// `[section]` headers and `key = value` lines, with keys and sections of
/// `[A-Za-z0-9_-]` and an optional trailing `# comment`. A value is a quoted
/// string without quotes inside, or a bare number, border-width list or word.
/// As in the shell's `["']([^"']+)["']`, either quote opens a string and either
/// closes it, so a mismatched pair is read as the shell reads it.
/// Any other line is dropped, so its key stays unset and a lower layer still
/// applies. Keys outside a section are ignored. The result maps `section.key`
/// to its raw value; a later line wins.
fn omarchy_shell_values(raw: &str) -> HashMap<String, String> {
    fn is_name(text: &str) -> bool {
        !text.is_empty()
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    }
    let mut values = HashMap::new();
    let mut section = None;
    for raw_line in raw.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            let Some((name, tail)) = rest.split_once(']') else {
                continue;
            };
            let tail = tail.trim_start();
            if is_name(name) && (tail.is_empty() || tail.starts_with('#')) {
                section = Some(name.to_owned());
            }
            continue;
        }
        let Some(section) = section.as_deref() else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        let value = if let Some(quoted) = value.strip_prefix(['"', '\'']) {
            let Some(end) = quoted.find(['"', '\'']) else {
                continue;
            };
            let tail = quoted[end + 1..].trim_start();
            if !(tail.is_empty() || tail.starts_with('#')) {
                continue;
            }
            &quoted[..end]
        } else {
            let bare = value
                .split_once('#')
                .map_or(value, |(value, _)| value)
                .trim();
            if !is_shell_bare_value(bare) {
                continue;
            }
            bare
        };
        if is_name(key) && !value.is_empty() {
            values.insert(format!("{section}.{key}"), value.to_owned());
        }
    }
    values
}

/// JavaScript `parseInt(value, 10)`, which the shell uses for `base-size`:
/// leading whitespace, an optional sign and the leading digits; `None` when
/// there are no digits. Saturates instead of growing without bound.
fn js_parse_int(value: &str) -> Option<i64> {
    let value = value.trim_start();
    let (negative, digits) = match value.as_bytes().first() {
        Some(b'-') => (true, &value[1..]),
        Some(b'+') => (false, &value[1..]),
        _ => (false, value),
    };
    let end = digits
        .bytes()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(digits.len());
    if end == 0 {
        return None;
    }
    let magnitude = digits[..end].bytes().fold(0_i64, |total, digit| {
        total
            .saturating_mul(10)
            .saturating_add(i64::from(digit - b'0'))
    });
    Some(if negative { -magnitude } else { magnitude })
}

/// Omarchy's terminal point size for a shell text size, as
/// `omarchy-display-text-size` writes it for alacritty, kitty, ghostty and
/// foot: `px × 9/12`, rounded half up to a whole point (12 px → 9 pt,
/// 16 px → 12 pt), before Splinterm's renderer bounds.
fn omarchy_unbounded_point_size(px: u16) -> u32 {
    (u32::from(px) * 9 + 6) / 12
}

/// The followed point size for a shell text size: Omarchy's terminal mapping,
/// clamped to the 6–96 pt Splinterm's renderer accepts.
#[must_use]
pub fn omarchy_terminal_point_size(px: u16) -> f32 {
    let points = omarchy_unbounded_point_size(px).clamp(
        u32::from(*RENDERER_POINT_SIZES.start()),
        u32::from(*RENDERER_POINT_SIZES.end()),
    );
    f32::from(u16::try_from(points).unwrap_or(u16::MAX))
}

/// What an unparseable `base-size` falls back to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OmarchyTextSizeFallback {
    /// At startup: Omarchy's 12 px default, as the shell uses.
    OmarchyDefault,
    /// In a running window: the size in use. `None` keeps Splinterm's
    /// configured fallback when no shell size was followed before.
    Current(Option<u16>),
}

/// Resolves the effective shell settings from the theme layer and the user
/// override. Like the shell's `mergeShell`, layering is per key: a user key
/// replaces the theme's value for that key only.
///
/// Alpha follows the shell's `pickAlpha`: a number is clamped to 0.0–1.0 and
/// anything else falls back to Omarchy's opaque default. `base-size` follows
/// the shell's `parseInt` and its 1 px floor. A `base-size` with no digits
/// uses `fallback`, so a live edit never changes the size in use.
#[must_use]
pub fn resolve_omarchy_shell(
    theme: OmarchyShellLayer<'_>,
    user: Option<OmarchyShellLayer<'_>>,
    fallback: OmarchyTextSizeFallback,
) -> OmarchyShellLoad {
    let theme_values = theme.contents.map(omarchy_shell_values).unwrap_or_default();
    let user_values = user
        .and_then(|layer| layer.contents)
        .map(omarchy_shell_values)
        .unwrap_or_default();
    let lookup = |key: &str| {
        user_values
            .get(key)
            .map(|value| (user.map_or(theme.path, |layer| layer.path), value.as_str()))
            .or_else(|| {
                theme_values
                    .get(key)
                    .map(|value| (theme.path, value.as_str()))
            })
    };
    let mut load = OmarchyShellLoad::default();
    for (section, slot) in [
        ("menu", &mut load.settings.menu_alpha),
        ("popups", &mut load.settings.popup_alpha),
    ] {
        let Some((origin, raw)) = lookup(&format!("{section}.background-alpha")) else {
            continue;
        };
        match raw.trim().parse::<f32>() {
            Ok(alpha) if alpha.is_finite() => {
                let clamped = alpha.clamp(0.0, 1.0);
                if !(0.0..=1.0).contains(&alpha) {
                    load.diagnostics.push((
                        OmarchyShellConcern::PanelAlpha,
                        format!(
                            "{}: [{section}] background-alpha {raw:?} is outside 0.0 to 1.0; using {clamped:.1}",
                            origin.display()
                        ),
                    ));
                }
                *slot = foot_alpha(clamped);
            }
            _ => load.diagnostics.push((
                OmarchyShellConcern::PanelAlpha,
                format!(
                    "{}: [{section}] background-alpha {raw:?} is not a number; using 1.0",
                    origin.display()
                ),
            )),
        }
    }
    load.settings.bar_size_px = resolve_bar_size(
        lookup("bar.size-horizontal"),
        lookup("bar.scale-with-font"),
        lookup("font.base-size"),
        &mut load.diagnostics,
    );
    load.settings.text_size_px = match lookup("font.base-size") {
        None => None,
        Some((origin, raw)) => {
            if let Some(size) = js_parse_int(raw) {
                let px = u16::try_from(size.max(i64::from(OMARCHY_MIN_TEXT_SIZE_PX)))
                    .unwrap_or(u16::MAX);
                let points = omarchy_unbounded_point_size(px);
                if !RENDERER_POINT_SIZES.contains(&u16::try_from(points).unwrap_or(u16::MAX)) {
                    load.diagnostics.push((
                        OmarchyShellConcern::TerminalTextSize,
                        format!(
                            "{}: [font] base-size {raw:?} maps to {points} pt; using {} pt, the nearest size Splinterm renders",
                            origin.display(),
                            omarchy_terminal_point_size(px)
                        ),
                    ));
                }
                Some(px)
            } else {
                load.settings.text_size_held =
                    matches!(fallback, OmarchyTextSizeFallback::Current(_));
                let fallback = match fallback {
                    OmarchyTextSizeFallback::OmarchyDefault => Some(OMARCHY_DEFAULT_TEXT_SIZE_PX),
                    OmarchyTextSizeFallback::Current(current) => current,
                };
                load.diagnostics.push((
                    OmarchyShellConcern::TextSize,
                    format!(
                        "{}: [font] base-size {raw:?} is not a whole number of px; {}",
                        origin.display(),
                        fallback.map_or_else(
                            || "keeping the current size".to_owned(),
                            |px| format!("using {px} px"),
                        )
                    ),
                ));
                fallback
            }
        }
    };
    load.settings.ui_text_size_px = load.settings.text_size_px;
    load
}

/// The shell's `boolToken` for `[bar] scale-with-font`: `true`, `1`, `yes`
/// or `on`, and `false`, `0`, `no` or `off`, trimmed and in any case.
fn shell_bool_token(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// A `parseInt` result as a bounded f64. Values past 10 000 px only ever
/// clamp to the tab strip's 48 px maximum, so bounding them changes nothing.
fn bounded_px(value: i64) -> f64 {
    f64::from(u32::try_from(value.clamp(1, 10_000)).unwrap_or(10_000))
}

/// The tab strip height: the Omarchy bar's height as the shell's
/// `barToken("size-horizontal", 26)` computes it, clamped to
/// [`OMARCHY_BAR_SIZE_LIMITS_PX`].
///
/// - `size-horizontal` is read with `parseInt`; a value without digits, or
///   one that is not positive, uses the 26 px default.
/// - With `scale-with-font` on, which is the default, the size is multiplied
///   by `max(1/12, base-size / 12)`. `base-size` is read with `parseInt` and
///   floored at 1 px, as the shell does, and is 12 px when unset or without
///   digits.
/// - The result is rounded as `Math.round` does, then clamped.
///
/// A rejected value is reported once, and so is a clamp when the configured
/// `size-horizontal` is itself outside the limits. A bar that `base-size`
/// scaling pushes out of range clamps silently: Omarchy's rendered template
/// always sets `size-horizontal = 26`, so a text size change is never
/// reported as a bar problem.
fn resolve_bar_size(
    size: Option<(&Path, &str)>,
    scale_with_font: Option<(&Path, &str)>,
    base_size: Option<(&Path, &str)>,
    diagnostics: &mut Vec<(OmarchyShellConcern, String)>,
) -> u16 {
    let default_size = f64::from(OMARCHY_DEFAULT_BAR_SIZE_PX);
    let size_px = match size {
        None => default_size,
        Some((origin, raw)) => match js_parse_int(raw) {
            Some(px) if px > 0 => bounded_px(px),
            _ => {
                diagnostics.push((
                    OmarchyShellConcern::BarSize,
                    format!(
                        "{}: [bar] size-horizontal {raw:?} is not a positive whole number of px; using {OMARCHY_DEFAULT_BAR_SIZE_PX} px",
                        origin.display()
                    ),
                ));
                default_size
            }
        },
    };
    let follows_font = match scale_with_font {
        None => true,
        Some((origin, raw)) => shell_bool_token(raw).unwrap_or_else(|| {
            diagnostics.push((
                OmarchyShellConcern::BarSize,
                format!(
                    "{}: [bar] scale-with-font {raw:?} is not true or false; using true",
                    origin.display()
                ),
            ));
            true
        }),
    };
    let font_scale = if follows_font {
        let base = base_size
            .and_then(|(_, raw)| js_parse_int(raw))
            .map_or(f64::from(OMARCHY_DEFAULT_TEXT_SIZE_PX), bounded_px);
        (base / 12.0).max(1.0 / 12.0)
    } else {
        1.0
    };
    // Math.round: the nearest whole px, halves rounding up.
    let scaled = (size_px * font_scale + 0.5).floor().max(1.0);
    let (minimum, maximum) = (
        *OMARCHY_BAR_SIZE_LIMITS_PX.start(),
        *OMARCHY_BAR_SIZE_LIMITS_PX.end(),
    );
    let clamped = scaled.clamp(f64::from(minimum), f64::from(maximum));
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the value is a whole number clamped into the u16 bar size limits"
    )]
    let px = clamped as u16;
    let limits = f64::from(minimum)..=f64::from(maximum);
    if !limits.contains(&scaled)
        && !limits.contains(&size_px)
        && let Some((origin, raw)) = size
    {
        diagnostics.push((
            OmarchyShellConcern::BarSize,
            format!(
                "{}: [bar] size-horizontal {raw:?} is outside {minimum} to {maximum} px; using {px} px for the tab strip",
                origin.display()
            ),
        ));
    }
    px
}

fn read_omarchy_shell_layer(
    path: &Path,
    diagnostics: &mut Vec<(OmarchyShellConcern, String)>,
) -> Option<String> {
    match fs::read_to_string(path) {
        Ok(contents) => Some(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            diagnostics.push((
                OmarchyShellConcern::Read,
                format!("read {}: {error}", path.display()),
            ));
            None
        }
    }
}

/// Loads the effective shell settings for one theme directory and an optional
/// user override. Missing files are ordinary; unreadable ones are reported and
/// treated as missing.
#[must_use]
pub fn load_omarchy_shell(
    theme_dir: &Path,
    user_shell: Option<&Path>,
    fallback: OmarchyTextSizeFallback,
) -> OmarchyShellLoad {
    let theme_path = theme_dir.join("shell.toml");
    let mut diagnostics = Vec::new();
    let theme = read_omarchy_shell_layer(&theme_path, &mut diagnostics);
    let user = user_shell.map(|path| (path, read_omarchy_shell_layer(path, &mut diagnostics)));
    let mut load = resolve_omarchy_shell(
        OmarchyShellLayer {
            path: &theme_path,
            contents: theme.as_deref(),
        },
        user.as_ref().map(|(path, contents)| OmarchyShellLayer {
            path,
            contents: contents.as_deref(),
        }),
        fallback,
    );
    diagnostics.append(&mut load.diagnostics);
    load.diagnostics = diagnostics;
    load
}

pub fn default_config_dir() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map_or_else(
            || {
                env::var_os("HOME")
                    .map_or_else(|| PathBuf::from("."), PathBuf::from)
                    .join(".config")
            },
            PathBuf::from,
        )
        .join("splinterm")
}

/// Loads the supported configuration from the standard or overridden path.
///
/// # Errors
/// Returns an error when the file cannot be read or a supported value is invalid.
pub fn load_default() -> Result<ConfigLoad> {
    let path = env::var_os("SPLINTERM_CONFIG")
        .map_or_else(|| default_config_dir().join("config.ini"), PathBuf::from);
    if !path.exists() {
        return Ok(ConfigLoad {
            config: AppConfig::default(),
            diagnostics: Vec::new(),
        });
    }
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    parse_with_base(&text, path.parent().unwrap_or_else(|| Path::new(".")))
        .with_context(|| format!("parse {}", path.display()))
}

/// Parses the documented MVP configuration subset.
///
/// # Errors
/// Returns an error for malformed syntax or invalid supported values.
pub fn parse(text: &str) -> Result<ConfigLoad> {
    parse_with_base(text, &default_config_dir())
}

fn parse_with_base(text: &str, config_dir: &Path) -> Result<ConfigLoad> {
    let mut config = AppConfig::default();
    let mut diagnostics = Vec::new();
    let mut explicit_font_policy_line = None;
    let mut legacy_dpi_aware_line = None;
    let mut explicit_font_size_line = None;
    let mut section = String::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_ascii_lowercase();
            if !matches!(
                section.as_str(),
                "main"
                    | "scrollback"
                    | "cursor"
                    | "colors"
                    | "key-bindings"
                    | "presets"
                    | "multiplexer"
                    | "clipboard"
                    | "explorer"
                    | "integration"
            ) {
                diagnostics.push(format!(
                    "line {}: unsupported section [{section}]",
                    index + 1
                ));
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            bail!("line {}: expected key=value", index + 1);
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        let full = if section.is_empty() {
            key.clone()
        } else {
            format!("{section}.{key}")
        };
        let unsupported = match full.as_str() {
            "clipboard.image-directory" => {
                let path = PathBuf::from(nonempty(value, index)?);
                crate::clipboard_image::validate_directory_path(&path)
                    .with_context(|| format!("line {}: clipboard.image-directory", index + 1))?;
                if config.clipboard_image_directory.replace(path).is_some() {
                    bail!("line {}: duplicate clipboard.image-directory", index + 1);
                }
                false
            }
            "main.font" | "font" => {
                let font = nonempty(value, index)?;
                let normalized = font.to_ascii_lowercase();
                if normalized.contains(":size=") || normalized.contains(":pixelsize=") {
                    bail!(
                        "line {}: main.font is only a face/style pattern; use main.font-pixelsize or main.font-point-size",
                        index + 1
                    );
                }
                config.font = font;
                config.font_authority = FontAuthority::Explicit;
                false
            }
            "main.font-ligatures" => {
                config.font_ligatures = match value {
                    "off" => FontLigatures::Off,
                    "on" => FontLigatures::On,
                    "cursor" => FontLigatures::Cursor,
                    _ => bail!(
                        "line {}: font-ligatures must be off, on or cursor",
                        index + 1
                    ),
                };
                false
            }
            "main.font-features" => {
                config.font_features = FeatureSettings::parse(value)
                    .with_context(|| format!("line {}: invalid font-features", index + 1))?;
                false
            }
            "main.font-size" | "font-size" | "main.font-pixelsize" => {
                if let Some(previous) = explicit_font_size_line {
                    bail!(
                        "line {}: font size conflicts with line {previous}; configure exactly one size key",
                        index + 1
                    );
                }
                config.font_size = FontSize::Pixels(parse_range(value, 6.0, 96.0, index)?);
                explicit_font_size_line = Some(index + 1);
                config.font_size_authority = FontAuthority::Explicit;
                if full != "main.font-pixelsize" {
                    diagnostics.push(format!(
                        "line {}: font-size is deprecated; use main.font-pixelsize",
                        index + 1
                    ));
                }
                false
            }
            "main.font-point-size" => {
                if let Some(previous) = explicit_font_size_line {
                    bail!(
                        "line {}: font size conflicts with line {previous}; configure exactly one size key",
                        index + 1
                    );
                }
                config.font_size = FontSize::Points(parse_range(value, 6.0, 96.0, index)?);
                explicit_font_size_line = Some(index + 1);
                config.font_size_authority = FontAuthority::Explicit;
                false
            }
            "main.ui-font-pixelsize" => {
                config.ui_font_size = Some(FontSize::Pixels(parse_range(
                    value,
                    *UI_FONT_PIXELSIZE_LIMITS.start(),
                    *UI_FONT_PIXELSIZE_LIMITS.end(),
                    index,
                )?));
                false
            }
            "main.font-sizing-policy" => {
                config.font_sizing_policy = match value.to_ascii_lowercase().as_str() {
                    "output-scale" => FontSizingPolicy::OutputScale,
                    "physical-dpi" => FontSizingPolicy::PhysicalDpi,
                    _ => bail!(
                        "line {}: font-sizing-policy must be output-scale or physical-dpi",
                        index + 1
                    ),
                };
                explicit_font_policy_line = Some(index + 1);
                false
            }
            "main.padding-left" => {
                config.padding.left = parse_range(value, 0, 10_000, index)?;
                false
            }
            "main.padding-right" => {
                config.padding.right = parse_range(value, 0, 10_000, index)?;
                false
            }
            "main.padding-top" => {
                config.padding.top = parse_range(value, 0, 10_000, index)?;
                false
            }
            "main.padding-bottom" => {
                config.padding.bottom = parse_range(value, 0, 10_000, index)?;
                false
            }
            "main.initial-columns" => {
                config.initial_columns = parse_range(value, 2, MAX_COLUMNS, index)?;
                false
            }
            "main.initial-rows" => {
                config.initial_rows = parse_range(value, 2, MAX_ROWS, index)?;
                false
            }
            "main.shell" | "shell" => {
                config.shell = Some(nonempty(value, index)?);
                false
            }
            "main.login-shell" | "login-shell" => {
                config.login_shell = parse_bool(value, index)?;
                false
            }
            "main.title" | "title" => {
                config.title = Some(value.to_owned());
                false
            }
            "main.app-id" | "app-id" => {
                if value != APP_ID {
                    diagnostics.push(format!(
                        "line {}: app-id is fixed to {APP_ID}; requested value ignored",
                        index + 1
                    ));
                }
                false
            }
            "main.resize-delay-ms" => {
                config.resize_delay_ms = parse_range(value, 0, 1_000, index)?;
                false
            }
            "main.dpi-aware" => {
                let value = parse_bool(value, index)?;
                if !value {
                    bail!(
                        "line {}: legacy Splinterm dpi-aware=no forced the whole surface to 1x and cannot be migrated safely; remove it and choose main.font-sizing-policy explicitly",
                        index + 1
                    );
                }
                legacy_dpi_aware_line = Some(index + 1);
                config.font_sizing_policy = FontSizingPolicy::OutputScale;
                diagnostics.push(format!(
                    "line {}: legacy Splinterm dpi-aware=yes is deprecated and maps only to main.font-sizing-policy=output-scale",
                    index + 1
                ));
                false
            }
            "main.theme" => {
                config.theme_path = Some(expand_path(value));
                false
            }
            "colors.alpha" => {
                config.background_alpha = Some(foot_alpha(parse_alpha(value, index)?));
                false
            }
            "colors.blur" => {
                config.background_blur = Some(parse_bool(value, index)?);
                false
            }
            "colors.active-tab-background" => {
                config.chrome_colors.active_tab_background =
                    Some(parse_config_color(value, index)?);
                false
            }
            "colors.active-tab-alpha" => {
                config.chrome_colors.active_tab_alpha =
                    Some(foot_alpha(parse_alpha(value, index)?));
                false
            }
            "colors.tab-strip-alpha" => {
                config.chrome_colors.tab_strip_alpha = Some(foot_alpha(parse_alpha(value, index)?));
                false
            }
            "colors.explorer-background" => {
                config.chrome_colors.explorer_background = Some(parse_config_color(value, index)?);
                false
            }
            "colors.explorer-alpha" => {
                config.chrome_colors.explorer_alpha = Some(foot_alpha(parse_alpha(value, index)?));
                false
            }
            "scrollback.lines" => {
                config.scrollback_lines = parse_range(value, 0, 1_000_000, index)?;
                false
            }
            "multiplexer.divider-style" => {
                config.pane_divider_style = match value.to_ascii_lowercase().as_str() {
                    "none" => PaneDividerStyle::None,
                    "line" => PaneDividerStyle::Line,
                    "frame" => PaneDividerStyle::Frame,
                    _ => bail!(
                        "line {}: divider-style must be none, line, or frame",
                        index + 1
                    ),
                };
                false
            }
            "multiplexer.pane-gap" => {
                config.pane_gap = parse_range(value, 0, MAX_PANE_GAP, index)?;
                false
            }
            "explorer.status-colors" => {
                config.explorer_status_style.colors = parse_bool(value, index)?;
                false
            }
            "explorer.status-glyphs" => {
                config.explorer_status_style.glyphs = match value.to_ascii_lowercase().as_str() {
                    "icons" => StatusGlyphs::Icons,
                    "dots" => StatusGlyphs::Dots,
                    _ => bail!("line {}: status-glyphs must be icons or dots", index + 1),
                };
                false
            }
            "explorer.agent-status" => {
                config.explorer_agent_status = parse_bool(value, index)?;
                false
            }
            "multiplexer.tab-strip-border" => {
                config.tab_strip_border = parse_bool(value, index)?;
                false
            }
            "explorer.full-height" => {
                config.explorer_full_height = parse_bool(value, index)?;
                false
            }
            "explorer.collapsed" => {
                config.explorer_collapsed = parse_bool(value, index)?;
                false
            }
            "main.omarchy-integration-hint" | "omarchy-integration-hint" => {
                config.omarchy_integration_hint = parse_bool(value, index)?;
                false
            }
            "integration.dialog-shortcut" => {
                config.dialog_shortcut =
                    DialogShortcut::parse_setting(value).map_err(|reason| {
                        anyhow::anyhow!("line {}: dialog-shortcut {value:?}: {reason}", index + 1)
                    })?;
                false
            }
            "multiplexer.frame-title" => {
                config.frame_title_mode = match value.to_ascii_lowercase().as_str() {
                    "none" => FrameTitleMode::None,
                    "splint" => FrameTitleMode::Splint,
                    _ => bail!("line {}: frame-title must be none or splint", index + 1),
                };
                false
            }
            "multiplexer.persistent-by-default" => {
                config.multiplexer_lifetime.persistent_by_default = parse_bool(value, index)?;
                false
            }
            "main.startup" => {
                config.startup = match value.to_ascii_lowercase().as_str() {
                    "new-dojo" => StartupMode::NewDojo,
                    "dojo-picker" => StartupMode::DojoPicker,
                    _ => bail!(
                        "line {}: startup must be new-dojo or dojo-picker",
                        index + 1
                    ),
                };
                false
            }
            "multiplexer.persist-on-tab-organization" => {
                config.multiplexer_lifetime.persist_on_tab_organization = parse_bool(value, index)?;
                false
            }
            "multiplexer.new-dojo-directory" => {
                config.new_dojo_directory = parse_launch_directory(value, index)?;
                false
            }
            "multiplexer.new-splint-directory" => {
                config.new_splint_directory = parse_launch_directory(value, index)?;
                false
            }
            "multiplexer.close-unused" => {
                config.multiplexer_lifetime.close_unused = parse_bool(value, index)?;
                false
            }
            "cursor.style" => {
                config.cursor_style = match value.to_ascii_lowercase().as_str() {
                    "block" => CursorStyle::Block,
                    "beam" => CursorStyle::Beam,
                    "underline" => CursorStyle::Underline,
                    _ => bail!(
                        "line {}: cursor style must be block, beam, or underline",
                        index + 1
                    ),
                };
                false
            }
            "cursor.blink" => {
                config.cursor_blink = parse_bool(value, index)?;
                false
            }
            "key-bindings.profile" => {
                config.keymap_profile = KeymapProfile::parse(value)
                    .with_context(|| format!("line {}: invalid keymap profile", index + 1))?;
                false
            }
            "key-bindings.file" => {
                let value = nonempty(value, index)?;
                config.keymap_path = Some(expand_relative_path(&value, config_dir));
                false
            }
            "key-bindings.prefix-timeout-ms" => {
                config.prefix_timeout_ms = parse_range(value, 250, 5_000, index)?;
                false
            }
            "presets.file" => {
                let value = nonempty(value, index)?;
                config.preset_path = Some(expand_relative_path(&value, config_dir));
                false
            }
            "presets.allow-unrestricted-commands" => {
                config.allow_unrestricted_commands = parse_bool(value, index)?;
                false
            }
            key if key.starts_with("colors.") => {
                diagnostics.push(format!(
                    "line {}: colors come from the active Omarchy theme or explicit main.theme JSON; {key} ignored",
                    index + 1
                ));
                false
            }
            _ => true,
        };
        if unsupported {
            diagnostics.push(format!("line {}: unsupported option {full}", index + 1));
        }
    }
    if let (Some(legacy), Some(explicit)) = (legacy_dpi_aware_line, explicit_font_policy_line) {
        bail!(
            "line {legacy}: legacy dpi-aware conflicts with font-sizing-policy on line {explicit}; remove the legacy key"
        );
    }
    let keymap = resolve_keymap(config.keymap_profile, config.keymap_path.as_deref())?;
    config.keymap = keymap.keymap;
    diagnostics.extend(keymap.diagnostics);
    config.preset_catalog = Some(if let Some(path) = config.preset_path.as_deref() {
        PresetCatalog::load_user_overlay(path)?
    } else {
        PresetCatalog::bundled()
    });
    Ok(ConfigLoad {
        config,
        diagnostics,
    })
}

fn nonempty(value: &str, line: usize) -> Result<String> {
    if value.is_empty() {
        bail!("line {}: value cannot be empty", line + 1)
    }
    Ok(value.to_owned())
}
fn parse_bool(value: &str, line: usize) -> Result<bool> {
    match value.to_ascii_lowercase().as_str() {
        "yes" | "true" | "on" | "1" => Ok(true),
        "no" | "false" | "off" | "0" => Ok(false),
        _ => bail!("line {}: expected boolean", line + 1),
    }
}
fn parse_range<T>(value: &str, min: T, max: T, line: usize) -> Result<T>
where
    T: std::str::FromStr + PartialOrd + Copy,
{
    let parsed = value
        .parse::<T>()
        .map_err(|_| anyhow::anyhow!("line {}: invalid number", line + 1))?;
    // A range check rather than two comparisons: NaN fails both `<` and `>`,
    // so `nan` would otherwise pass for a float key.
    if !(min..=max).contains(&parsed) {
        bail!("line {}: number outside supported range", line + 1);
    }
    Ok(parsed)
}

fn parse_config_color(value: &str, line: usize) -> Result<u32> {
    parse_color(value).map_err(|error| anyhow::anyhow!("line {}: {error}", line + 1))
}

fn parse_alpha(value: &str, line: usize) -> Result<f32> {
    let alpha = value
        .parse::<f32>()
        .map_err(|_| anyhow::anyhow!("line {}: invalid number", line + 1))?;
    if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
        bail!("line {}: number outside supported range", line + 1);
    }
    Ok(alpha)
}

/// Parses a Window-creation directory. Paths stay unexpanded: `~/` and `home`
/// resolve against the home of the daemon that owns the source Splint.
fn parse_launch_directory(value: &str, line: usize) -> Result<LaunchDirectory> {
    let directory = if value.eq_ignore_ascii_case("inherit") {
        LaunchDirectory::Inherit
    } else if value.eq_ignore_ascii_case("home") || value == "~" || value == "~/" {
        LaunchDirectory::Home
    } else {
        match value.strip_prefix("~/") {
            Some(relative) => LaunchDirectory::HomeRelative(PathBuf::from(relative)),
            None => LaunchDirectory::Absolute(PathBuf::from(value)),
        }
    };
    directory.validate().map_err(|_| {
        anyhow::anyhow!(
            "line {}: directory must be inherit, home, an absolute path, or ~/ followed by a path inside the home directory",
            line + 1
        )
    })?;
    Ok(directory)
}

fn expand_path(value: &str) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/") {
        env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(rest)
    } else {
        PathBuf::from(value)
    }
}

fn expand_relative_path(value: &str, base: &Path) -> PathBuf {
    let expanded = expand_path(value);
    if expanded.is_absolute() {
        expanded
    } else {
        base.join(expanded)
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemePalette {
    pub background: String,
    pub foreground: String,
    pub cursor: String,
    pub selection: String,
    #[serde(default)]
    pub selection_foreground: Option<String>,
    #[serde(default)]
    pub active_tab_background: Option<String>,
    #[serde(default)]
    pub active_tab_foreground: Option<String>,
    /// Inactive tab labels. When given it pins the `text_muted` role
    /// verbatim, with no ramp and no contrast floor.
    #[serde(default)]
    pub inactive_tab_foreground: Option<String>,
    pub url: String,
    pub ui_accent: String,
    #[serde(default = "opaque_alpha")]
    pub alpha: f32,
    #[serde(default)]
    pub blur: bool,
    #[serde(default)]
    pub pane_border: Option<String>,
    #[serde(default)]
    pub pane_border_active: Option<String>,
    pub ansi: [String; 16],
}

/// A resolved theme: the terminal palette, the chrome roles and components
/// derived from it, and the shell settings. See `crate::theme` for how a
/// theme source becomes one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedTheme {
    /// The terminal palette. Chrome paints from `roles` instead.
    pub background: u32,
    pub foreground: u32,
    pub cursor: u32,
    pub selection: u32,
    pub selection_foreground: u32,
    pub url: u32,
    pub ansi: [u32; 16],
    /// The semantic chrome colors.
    pub roles: ChromeRoles,
    /// The selected Dojo tab's body: a JSON theme's own color, the
    /// `surface_raised` role a theme names (Omarchy `lighter_background`),
    /// else ANSI 8. `[colors] active-tab-background` replaces it.
    pub active_tab_background: u32,
    /// The selected Dojo tab's label: a JSON theme's own color, else the
    /// surface or text role, whichever contrasts more with the body.
    pub active_tab_foreground: u32,
    pub background_alpha: u16,
    pub background_blur: bool,
    /// Coverage of Splinterm's own chrome backgrounds, and the Explorer's
    /// panel colour. Splinterm owns these; themes do not set them.
    pub chrome: ChromeBackgrounds,
    /// Whether the theme is dark or light; see `crate::theme::Appearance`.
    pub appearance: Appearance,
    /// Omarchy shell settings layered from `shell.toml`; Omarchy's defaults
    /// for other theme sources.
    pub shell: OmarchyShellSettings,
}

impl Default for ResolvedTheme {
    /// The built-in palette.
    fn default() -> Self {
        Self::from_input(&ThemeInput::builtin())
    }
}

const fn opaque_alpha() -> f32 {
    1.0
}

/// How Splinterm draws its own chrome backgrounds over the Window's glass.
///
/// The tab strip and the docked Lair Explorer follow the terminal background
/// alpha unless `config.ini` sets their own. The selected Dojo tab is opaque
/// by default, so under Glassy Windows it stays clearly apart from the
/// translucent strip and the inactive tabs on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChromeBackgrounds {
    /// Selected Dojo tab body coverage (`[colors] active-tab-alpha`).
    pub active_tab_alpha: u16,
    /// Tab strip coverage (`[colors] tab-strip-alpha`); `None` follows the
    /// terminal background alpha.
    pub tab_strip_alpha: Option<u16>,
    /// Lair Explorer panel colour (`[colors] explorer-background`); `None`
    /// uses the terminal background.
    pub explorer_background: Option<u32>,
    /// Lair Explorer coverage (`[colors] explorer-alpha`); `None` follows the
    /// terminal background alpha.
    pub explorer_alpha: Option<u16>,
}

impl Default for ChromeBackgrounds {
    fn default() -> Self {
        Self {
            active_tab_alpha: u16::MAX,
            tab_strip_alpha: None,
            explorer_background: None,
            explorer_alpha: None,
        }
    }
}

/// The `[colors]` chrome keys `config.ini` sets. Each absent key keeps the
/// theme's or Splinterm's default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChromeColorOverrides {
    pub active_tab_background: Option<u32>,
    pub active_tab_alpha: Option<u16>,
    pub tab_strip_alpha: Option<u16>,
    pub explorer_background: Option<u32>,
    pub explorer_alpha: Option<u16>,
}

impl ChromeColorOverrides {
    /// No chrome key set.
    pub const NONE: Self = Self {
        active_tab_background: None,
        active_tab_alpha: None,
        tab_strip_alpha: None,
        explorer_background: None,
        explorer_alpha: None,
    };
}

impl ResolvedTheme {
    /// Derives the roles of `input` and the chrome components that follow
    /// from them, before any `config.ini` override.
    #[must_use]
    pub fn from_input(input: &ThemeInput) -> Self {
        let terminal = input.terminal;
        let roles = derive(input);
        let active_tab_background = input.components.active_tab_background.unwrap_or(
            // Without a theme's raised surface, the selected tab keeps the
            // Foot `bright0` ramp rather than the derived `surface_raised`.
            if input.hints.surface_raised.is_some() {
                roles.surface_raised
            } else {
                terminal.ansi[8]
            },
        );
        Self {
            background: terminal.background,
            foreground: terminal.foreground,
            cursor: terminal.cursor,
            selection: terminal.selection,
            selection_foreground: terminal.selection_foreground,
            url: terminal.url,
            ansi: terminal.ansi,
            roles,
            active_tab_background,
            active_tab_foreground: input.components.active_tab_foreground.unwrap_or_else(|| {
                active_tab_foreground_fallback(roles.surface, roles.text, active_tab_background)
            }),
            background_alpha: input.shell.background_alpha,
            background_blur: input.shell.background_blur,
            chrome: ChromeBackgrounds::default(),
            shell: input.shell.settings,
            appearance: input.appearance,
        }
    }

    /// The application accent: the `accent` role.
    #[must_use]
    pub const fn ui_accent(&self) -> u32 {
        self.roles.accent
    }

    /// Inactive pane borders and frames: the `divider` role.
    #[must_use]
    pub const fn pane_border(&self) -> u32 {
        self.roles.divider
    }

    /// The active pane border: the `focus_ring` role.
    #[must_use]
    pub const fn pane_border_active(&self) -> u32 {
        self.roles.focus_ring
    }

    /// Inactive tab labels: the `text_muted` role.
    #[must_use]
    pub const fn inactive_tab_foreground(&self) -> u32 {
        self.roles.text_muted
    }

    /// Applies the `config.ini` chrome keys. A replaced selected-tab colour
    /// gets its label colour chosen again for contrast, as a theme without an
    /// explicit `active_tab_foreground` does.
    #[must_use]
    pub fn with_chrome_overrides(mut self, overrides: ChromeColorOverrides) -> Self {
        if let Some(color) = overrides.active_tab_background {
            self.active_tab_background = color;
            self.active_tab_foreground =
                active_tab_foreground_fallback(self.roles.surface, self.roles.text, color);
        }
        if let Some(alpha) = overrides.active_tab_alpha {
            self.chrome.active_tab_alpha = alpha;
        }
        if overrides.tab_strip_alpha.is_some() {
            self.chrome.tab_strip_alpha = overrides.tab_strip_alpha;
        }
        if overrides.explorer_background.is_some() {
            self.chrome.explorer_background = overrides.explorer_background;
        }
        if overrides.explorer_alpha.is_some() {
            self.chrome.explorer_alpha = overrides.explorer_alpha;
        }
        self
    }

    /// The tab strip's coverage.
    #[must_use]
    pub const fn tab_strip_alpha(&self) -> u16 {
        match self.chrome.tab_strip_alpha {
            Some(alpha) => alpha,
            None => self.background_alpha,
        }
    }

    /// The docked Lair Explorer's panel colour: the `surface` role unless
    /// `[colors] explorer-background` replaces it.
    #[must_use]
    pub const fn explorer_background(&self) -> u32 {
        match self.chrome.explorer_background {
            Some(color) => color,
            None => self.roles.surface,
        }
    }

    /// The docked Lair Explorer's coverage.
    #[must_use]
    pub const fn explorer_alpha(&self) -> u16 {
        match self.chrome.explorer_alpha {
            Some(alpha) => alpha,
            None => self.background_alpha,
        }
    }

    #[must_use]
    pub fn with_color_overrides(mut self, alpha: Option<u16>, blur: Option<bool>) -> Self {
        if let Some(alpha) = alpha {
            self.background_alpha = alpha;
        }
        if let Some(blur) = blur {
            self.background_blur = blur;
        }
        self
    }
}

impl ThemePalette {
    /// Resolves all strict `#RRGGBB` roles to packed colors.
    ///
    /// # Errors
    /// Returns an error when any required role is not a six-digit RGB color.
    #[cfg(test)]
    pub fn resolve(&self) -> Result<ResolvedTheme> {
        self.input().map(|input| ResolvedTheme::from_input(&input))
    }

    /// Reads the palette as theme input. `pane_border` and
    /// `pane_border_active` hint the `divider` and `focus_ring` roles; the
    /// tab keys are components used as given.
    ///
    /// # Errors
    /// Returns an error when any required role is not a six-digit RGB color.
    pub fn input(&self) -> Result<ThemeInput> {
        let mut ansi = [0; 16];
        for (out, value) in ansi.iter_mut().zip(&self.ansi) {
            *out = parse_color(value)?;
        }
        if !self.alpha.is_finite() || !(0.0..=1.0).contains(&self.alpha) {
            bail!("theme alpha must be between 0.0 and 1.0");
        }
        let optional =
            |value: Option<&str>, parse: fn(&str) -> Result<u32>| value.map(parse).transpose();
        let background = parse_color(&self.background)?;
        let foreground = parse_color(&self.foreground)?;
        let selection = parse_color(&self.selection)?;
        let components = ThemeComponents {
            active_tab_background: optional(self.active_tab_background.as_deref(), parse_color)?,
            active_tab_foreground: optional(
                self.active_tab_foreground.as_deref(),
                parse_json_color,
            )?,
            inactive_tab_foreground: optional(
                self.inactive_tab_foreground.as_deref(),
                parse_json_color,
            )?,
        };
        let accent = parse_color(&self.ui_accent)?;
        let cursor = parse_color(&self.cursor)?;
        let selection_foreground =
            optional(self.selection_foreground.as_deref(), parse_color)?.unwrap_or(foreground);
        let url = parse_color(&self.url)?;
        let hints = RoleHints {
            accent: Some(accent),
            // A JSON theme carries its muted color in ANSI 8, as
            // `tools/generate-omarchy-theme.py` fills it from `muted`.
            text_muted: Some(ansi[8]),
            divider: optional(self.pane_border.as_deref(), parse_color)?,
            focus_ring: optional(self.pane_border_active.as_deref(), parse_color)?,
            ..RoleHints::default()
        };
        Ok(ThemeInput {
            terminal: TerminalPalette {
                background,
                foreground,
                cursor,
                selection,
                selection_foreground,
                url,
                ansi,
            },
            hints,
            components,
            shell: ThemeShellInput {
                background_alpha: foot_alpha(self.alpha),
                background_blur: self.blur,
                settings: OmarchyShellSettings::default(),
            },
            appearance: Appearance::of_background(background),
        })
    }
}

fn omarchy_color_values(raw: &str) -> HashMap<String, String> {
    let mut colors = HashMap::new();
    for line in raw.lines() {
        let Some((key, raw_value)) = line.split_once('=') else {
            continue;
        };
        let key: String = key
            .chars()
            .filter(|character| !matches!(character, '"' | '\'' | ' ' | '\t'))
            .collect();
        if key.is_empty() || key.starts_with('#') {
            continue;
        }
        let value = if let Some(start) = raw_value.find(['"', '\'']) {
            let quoted = &raw_value[start + 1..];
            let end = quoted.find(['"', '\'']).unwrap_or(quoted.len());
            quoted[..end].to_owned()
        } else {
            raw_value.trim().to_owned()
        };
        colors.insert(key, value);
    }
    colors
}

/// Resolves the first non-empty `colors.toml` value among `keys`, which list a
/// canonical Omarchy 4 name before its legacy aliases, as in Omarchy's
/// `omarchy-theme-color` cascade.
fn omarchy_role(colors: &HashMap<String, String>, keys: &[&str]) -> Result<Option<u32>> {
    let Some((key, value)) = keys.iter().find_map(|key| {
        colors
            .get(*key)
            .filter(|value| !value.is_empty())
            .map(|value| (*key, value))
    }) else {
        return Ok(None);
    };
    parse_color(value)
        .map(Some)
        .with_context(|| format!("active Omarchy colors.toml has invalid {key}"))
}

/// Like [`omarchy_role`] for a key Splinterm did not read before semantic
/// roles: an invalid value never rejects a theme that loaded before. It is
/// skipped, the cascade goes on, and `diagnostics` says so.
fn omarchy_hint(
    colors: &HashMap<String, String>,
    keys: &[&str],
    diagnostics: &mut Vec<String>,
) -> Option<u32> {
    keys.iter().find_map(|key| {
        let value = colors.get(*key).filter(|value| !value.is_empty())?;
        parse_color(value)
            .inspect_err(|_| {
                diagnostics.push(format!(
                    "active Omarchy colors.toml {key} {value:?} is not #RRGGBB; ignoring it"
                ));
            })
            .ok()
    })
}

/// Selected-Dojo-tab body: canonical `lighter_background`, then the legacy
/// short name. Omarchy's last resort, `color0`/`background`, is the terminal
/// background itself and would hide the selected tab, so Splinterm keeps its
/// Foot `bright0` ramp instead.
const OMARCHY_LIGHTER_BACKGROUND_KEYS: [&str; 2] = ["lighter_background", "lighter_bg"];
/// The keys the inactive-tab text read before semantic roles, in their old
/// order: `muted`, then Omarchy's `color8` and `dark_foreground` (legacy
/// `dark_fg`) fallbacks. They are still read strictly in this order, so a
/// theme that was rejected before is still rejected and no other is.
const OMARCHY_MUTED_KEYS: [&str; 4] = ["muted", "color8", "dark_foreground", "dark_fg"];
/// Application accent: `accent`, a legacy `cursor`, then `blue` before its
/// ANSI `color4` alias.
const OMARCHY_ACCENT_KEYS: [&str; 4] = ["accent", "cursor", "blue", "color4"];

/// Plan 0064's cascades for the role hints read since semantic roles; see
/// [`omarchy_theme_input`].
const OMARCHY_TEXT_MUTED_KEYS: [&str; 3] = ["muted", "dark_foreground", "dark_fg"];
const OMARCHY_DARK_BACKGROUND_KEYS: [&str; 2] = ["dark_background", "dark_bg"];
const OMARCHY_SELECTION_KEYS: [&str; 2] = ["selection_background", "selection"];
const OMARCHY_DIVIDER_KEYS: [&str; 3] = ["border", "muted", "color8"];
const OMARCHY_FOCUS_RING_KEYS: [&str; 1] = ["active_border_color"];
const OMARCHY_DANGER_KEYS: [&str; 2] = ["red", "color1"];
const OMARCHY_WARNING_KEYS: [&str; 3] = ["warning", "yellow", "color3"];
const OMARCHY_SUCCESS_KEYS: [&str; 3] = ["success", "green", "color2"];
const OMARCHY_INFO_KEYS: [&str; 2] = ["cyan", "color6"];
const OMARCHY_REMOTE_MARKER_KEYS: [&str; 3] = ["signal", "magenta", "color5"];

/// Returns the effective Foot palette for the selected color theme.
///
/// Foot 1.27.0 parses the deprecated `[colors]` section into the same dark
/// theme as `[colors-dark]` and `[colors2]` into the light theme, one key at a
/// time in file order, so the sections merge and the last assignment wins.
/// Also returns the appearance `initial-color-theme` names, when it names one.
fn foot_color_values(raw: &str) -> Result<(HashMap<String, String>, Option<Appearance>)> {
    let mut main = HashMap::new();
    let mut dark = HashMap::new();
    let mut light = HashMap::new();
    let mut section = String::new();
    for raw_line in raw.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_ascii_lowercase();
            continue;
        }
        let values = match section.as_str() {
            "" | "main" => &mut main,
            "colors" | "colors-dark" => &mut dark,
            "colors2" | "colors-light" => &mut light,
            _ => continue,
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        values.insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
    }
    let named = main.get("initial-color-theme");
    match named.map_or("dark", String::as_str) {
        "light" => Ok((light, named.map(|_| Appearance::Light))),
        "dark" => Ok((dark, named.map(|_| Appearance::Dark))),
        _ => bail!("active Omarchy foot.ini initial-color-theme must be dark or light"),
    }
}

fn foot_theme_color(values: &HashMap<String, String>, key: &str) -> Result<u32> {
    parse_color(
        values
            .get(key)
            .with_context(|| format!("active Omarchy foot.ini is missing {key}"))?,
    )
    .with_context(|| format!("active Omarchy foot.ini has invalid {key}"))
}

#[cfg(test)]
fn resolve_omarchy_theme(colors_raw: &str, foot_raw: &str) -> Result<ResolvedTheme> {
    omarchy_theme_input(colors_raw, foot_raw, &mut Vec::new())
        .map(|input| ResolvedTheme::from_input(&input))
}

/// Reads an Omarchy theme: the terminal palette, alpha and blur from the
/// generated `foot.ini`, and role hints from `colors.toml`.
///
/// Each hint takes the first valid key of its Plan 0064 cascade, so a
/// semantic key wins over the named hue, which wins over its ANSI alias:
///
/// | Role | `colors.toml` keys | Without one |
/// |---|---|---|
/// | surface-raised | `lighter_background` | mix toward text |
/// | surface-sunken | `dark_background` | mix toward black |
/// | selection | `selection_background`, `selection` | Foot selection |
/// | divider | `border`, `muted`, `color8` | Foot `bright0` |
/// | focus-ring | `active_border_color` | accent |
/// | text-muted | `muted`, `dark_foreground` | text mixed 30% to surface |
/// | accent | `accent`, `cursor`, `blue`, `color4` | Foot cursor |
/// | danger | `red`, `color1` | Foot `regular1` |
/// | warning | `warning`, `yellow`, `color3` | Foot `regular3` |
/// | success | `success`, `green`, `color2` | Foot `regular2` |
/// | info | `cyan`, `color6` | Foot `regular6` |
/// | remote-marker | `signal`, `magenta`, `color5` | Foot `regular5` |
///
/// The appearance is `mode`, else Foot's `initial-color-theme`, else the
/// background's. The keys read before semantic roles (`lighter_background`,
/// `muted`, `accent` and their cascades) still reject an invalid value; an
/// invalid value of any other key is skipped with a note in `diagnostics`.
fn omarchy_theme_input(
    colors_raw: &str,
    foot_raw: &str,
    diagnostics: &mut Vec<String>,
) -> Result<ThemeInput> {
    let colors = omarchy_color_values(colors_raw);
    let (foot, foot_appearance) = foot_color_values(foot_raw)?;
    if foot.is_empty() {
        bail!("active Omarchy foot.ini has no palette for the selected initial-color-theme");
    }

    let mut ansi = [0_u32; 16];
    for (index, output) in ansi.iter_mut().enumerate().take(8) {
        *output = foot_theme_color(&foot, &format!("regular{index}"))?;
    }
    for (index, output) in ansi.iter_mut().enumerate().skip(8) {
        *output = foot_theme_color(&foot, &format!("bright{}", index - 8))?;
    }

    let background = foot_theme_color(&foot, "background")?;
    let foreground = foot_theme_color(&foot, "foreground")?;
    let selection = foot
        .get("selection-background")
        .map(|value| parse_color(value))
        .transpose()
        .context("active Omarchy foot.ini has invalid selection-background")?
        .unwrap_or(ansi[8]);
    let selection_foreground = foot
        .get("selection-foreground")
        .map(|value| parse_color(value))
        .transpose()
        .context("active Omarchy foot.ini has invalid selection-foreground")?
        .unwrap_or(foreground);
    let cursor = foot
        .get("cursor")
        .and_then(|value| value.split_whitespace().last())
        .map(parse_color)
        .transpose()
        .context("active Omarchy foot.ini has invalid cursor")?
        .unwrap_or(foreground);
    // The keys read before semantic roles keep their strictness.
    let surface_raised = omarchy_role(&colors, &OMARCHY_LIGHTER_BACKGROUND_KEYS)?;
    omarchy_role(&colors, &OMARCHY_MUTED_KEYS)?;
    let accent = omarchy_role(&colors, &OMARCHY_ACCENT_KEYS)?.unwrap_or(cursor);
    let mut hint = |keys: &[&str]| omarchy_hint(&colors, keys, diagnostics);
    let hints = RoleHints {
        surface_raised,
        surface_sunken: hint(&OMARCHY_DARK_BACKGROUND_KEYS),
        selection: hint(&OMARCHY_SELECTION_KEYS),
        divider: Some(hint(&OMARCHY_DIVIDER_KEYS).unwrap_or(ansi[8])),
        focus_ring: hint(&OMARCHY_FOCUS_RING_KEYS),
        text_muted: hint(&OMARCHY_TEXT_MUTED_KEYS),
        accent: Some(accent),
        danger: hint(&OMARCHY_DANGER_KEYS),
        warning: hint(&OMARCHY_WARNING_KEYS),
        success: hint(&OMARCHY_SUCCESS_KEYS),
        info: hint(&OMARCHY_INFO_KEYS),
        remote_marker: hint(&OMARCHY_REMOTE_MARKER_KEYS),
    };
    let mode = colors.get("mode").filter(|value| !value.is_empty());
    let appearance = mode
        .and_then(|value| {
            Appearance::parse(value).or_else(|| {
                diagnostics.push(format!(
                    "active Omarchy colors.toml mode {value:?} is not dark or light; ignoring it"
                ));
                None
            })
        })
        .or(foot_appearance)
        .unwrap_or_else(|| Appearance::of_background(background));
    let alpha = foot.get("alpha").map_or(Ok(1.0), |value| {
        value
            .parse::<f32>()
            .context("active Omarchy foot.ini alpha must be a number")
    })?;
    if !alpha.is_finite() || !(0.0..=1.0).contains(&alpha) {
        bail!("active Omarchy foot.ini alpha must be between 0.0 and 1.0");
    }
    let blur = foot.get("blur").map_or(Ok(false), |value| {
        match value.to_ascii_lowercase().as_str() {
            "yes" | "true" | "on" | "1" => Ok(true),
            "no" | "false" | "off" | "0" => Ok(false),
            _ => bail!("active Omarchy foot.ini blur must be a boolean"),
        }
    })?;

    Ok(ThemeInput {
        terminal: TerminalPalette {
            background,
            foreground,
            cursor,
            selection,
            selection_foreground,
            url: ansi[4],
            ansi,
        },
        hints,
        components: ThemeComponents::default(),
        shell: ThemeShellInput {
            background_alpha: foot_alpha(alpha),
            background_blur: blur,
            settings: OmarchyShellSettings::default(),
        },
        appearance,
    })
}

type OmarchySourceIdentity = Option<(u64, u64, u64, i64, i64)>;

fn omarchy_path_identity(path: &Path) -> OmarchySourceIdentity {
    let metadata = fs::metadata(path).ok()?;
    Some((
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
    ))
}

fn omarchy_theme_identity(
    theme_dir: &Path,
) -> (
    OmarchySourceIdentity,
    OmarchySourceIdentity,
    OmarchySourceIdentity,
) {
    (
        omarchy_path_identity(theme_dir),
        omarchy_path_identity(&theme_dir.join("colors.toml")),
        omarchy_path_identity(&theme_dir.join("foot.ini")),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MissingOmarchyTheme {
    UseFallback,
    Reject,
}

/// A palette and the startup read decision that produced it. The fallback
/// flag records the actual loader branch, not a separate filesystem probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ThemeSourceLoad {
    pub theme: ResolvedTheme,
    /// The native loader used its no-source fallback. Callers must still
    /// validate that optional paths are genuinely absent before staying quiet.
    pub used_omarchy_fallback: bool,
}

/// Theme input and the startup read decision that produced it.
struct ThemeInputLoad {
    input: ThemeInput,
    used_omarchy_fallback: bool,
}

#[cfg(test)]
fn load_omarchy_theme_with(
    theme_dir: &Path,
    missing: MissingOmarchyTheme,
    after_identity: impl FnOnce(),
    between_reads: impl FnOnce(),
) -> Result<ResolvedTheme> {
    load_omarchy_input_with_status(theme_dir, missing, after_identity, between_reads)
        .map(|loaded| ResolvedTheme::from_input(&loaded.input))
}

fn load_omarchy_input_with_status(
    theme_dir: &Path,
    missing: MissingOmarchyTheme,
    after_identity: impl FnOnce(),
    between_reads: impl FnOnce(),
) -> Result<ThemeInputLoad> {
    let colors_path = theme_dir.join("colors.toml");
    let foot_path = theme_dir.join("foot.ini");
    let before = omarchy_theme_identity(theme_dir);
    if missing == MissingOmarchyTheme::UseFallback
        && before.0.is_none()
        && before.1.is_none()
        && before.2.is_none()
    {
        return Ok(ThemeInputLoad {
            input: ThemeInput::builtin(),
            used_omarchy_fallback: true,
        });
    }
    after_identity();
    let colors = fs::read_to_string(&colors_path)
        .with_context(|| format!("read {}", colors_path.display()))?;
    between_reads();
    let foot =
        fs::read_to_string(&foot_path).with_context(|| format!("read {}", foot_path.display()))?;
    anyhow::ensure!(
        omarchy_theme_identity(theme_dir) == before,
        "active Omarchy theme changed while its palette was loading"
    );
    let mut diagnostics = Vec::new();
    let input = omarchy_theme_input(&colors, &foot, &mut diagnostics)?;
    for diagnostic in diagnostics {
        if first_theme_diagnostic(&diagnostic) {
            eprintln!("splinterm theme: {diagnostic}");
        }
    }
    Ok(ThemeInputLoad {
        input,
        used_omarchy_fallback: false,
    })
}

/// Whether this process has not reported `diagnostic` before, recording it.
/// The theme watcher reloads a theme on every change, and a value Splinterm
/// ignores is reported once rather than on every reload.
fn first_theme_diagnostic(diagnostic: &str) -> bool {
    static REPORTED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let mut reported = REPORTED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if reported.iter().any(|seen| seen == diagnostic) {
        return false;
    }
    reported.push(diagnostic.to_owned());
    true
}

/// One Omarchy theme directory as a theme provider: its `colors.toml` and
/// the `foot.ini` Omarchy generates from it, read as one generation.
#[derive(Clone, Copy, Debug)]
pub struct OmarchyThemeProvider<'a> {
    theme_dir: &'a Path,
    missing: MissingOmarchyTheme,
}

impl<'a> OmarchyThemeProvider<'a> {
    /// For a startup read: a theme directory that does not exist at all
    /// gives the built-in palette.
    #[must_use]
    pub const fn startup(theme_dir: &'a Path) -> Self {
        Self {
            theme_dir,
            missing: MissingOmarchyTheme::UseFallback,
        }
    }

    /// For a live reload: transient absence is an error, never the fallback.
    #[must_use]
    pub const fn live(theme_dir: &'a Path) -> Self {
        Self {
            theme_dir,
            missing: MissingOmarchyTheme::Reject,
        }
    }

    fn load_with_status(&self) -> Result<ThemeInputLoad> {
        load_omarchy_input_with_status(self.theme_dir, self.missing, || {}, || {})
    }
}

impl ThemeProvider for OmarchyThemeProvider<'_> {
    fn load(&self) -> Result<ThemeInput> {
        self.load_with_status().map(|loaded| loaded.input)
    }

    /// The directory itself, so a theme switch that swaps it is seen, and
    /// the two files read from it.
    fn watched_paths(&self) -> Vec<PathBuf> {
        vec![
            self.theme_dir.to_path_buf(),
            self.theme_dir.join("colors.toml"),
            self.theme_dir.join("foot.ini"),
        ]
    }
}

/// An explicit JSON theme file (`[main] theme`) as a theme provider. An
/// absent file gives the built-in palette.
#[derive(Clone, Copy, Debug)]
pub struct JsonThemeProvider<'a> {
    path: &'a Path,
}

impl<'a> JsonThemeProvider<'a> {
    #[must_use]
    pub const fn new(path: &'a Path) -> Self {
        Self { path }
    }
}

impl ThemeProvider for JsonThemeProvider<'_> {
    fn load(&self) -> Result<ThemeInput> {
        if !self.path.exists() {
            return BuiltinThemeProvider.load();
        }
        let raw = fs::read_to_string(self.path)
            .with_context(|| format!("read {}", self.path.display()))?;
        serde_json::from_str::<ThemePalette>(&raw)
            .context("parse explicit Splinterm theme JSON")?
            .input()
    }

    fn watched_paths(&self) -> Vec<PathBuf> {
        vec![self.path.to_path_buf()]
    }
}

/// Loads the effective active Omarchy terminal palette directly.
///
/// # Errors
/// Returns an error when an active theme exists but its `colors.toml` or
/// generated `foot.ini` cannot be read as one coherent theme generation.
pub fn load_omarchy_theme(theme_dir: &Path) -> Result<ResolvedTheme> {
    load_provider_theme(&OmarchyThemeProvider::startup(theme_dir))
}

/// Loads one live Omarchy theme directory without converting transient
/// absence into the startup fallback palette.
///
/// # Errors
/// Returns an error when the theme is absent, incomplete, changing, or
/// malformed.
#[cfg(test)]
pub fn load_live_omarchy_theme(theme_dir: &Path) -> Result<ResolvedTheme> {
    load_provider_theme(&OmarchyThemeProvider::live(theme_dir))
}

/// Loads a provider's input and derives its theme, before `config.ini`
/// overrides.
///
/// # Errors
/// Returns the provider's load error.
pub fn load_provider_theme(provider: &dyn ThemeProvider) -> Result<ResolvedTheme> {
    provider
        .load()
        .map(|input| ResolvedTheme::from_input(&input))
}

/// Resolves either native Omarchy state or an explicit JSON override.
///
/// # Errors
/// Returns an error when the selected source exists but is malformed.
pub fn load_theme_source(source: &ThemeSource) -> Result<ResolvedTheme> {
    load_theme_source_for_startup(source).map(|loaded| loaded.theme)
}

/// Resolves a startup palette while retaining whether the native loader
/// actually chose its absent-source fallback. Explicit JSON never sets it.
///
/// # Errors
/// Returns an error when the selected source exists but is malformed.
pub fn load_theme_source_for_startup(source: &ThemeSource) -> Result<ThemeSourceLoad> {
    let loaded = match source {
        ThemeSource::Omarchy(roots) => {
            OmarchyThemeProvider::startup(roots.select()).load_with_status()?
        }
        ThemeSource::Json { path, .. } => ThemeInputLoad {
            input: JsonThemeProvider::new(path).load()?,
            used_omarchy_fallback: false,
        },
    };
    Ok(ThemeSourceLoad {
        theme: ResolvedTheme::from_input(&loaded.input),
        used_omarchy_fallback: loaded.used_omarchy_fallback,
    })
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the caller validates Foot alpha in the closed 0.0..=1.0 range"
)]
fn foot_alpha(alpha: f32) -> u16 {
    (alpha * f32::from(u16::MAX)) as u16
}

fn parse_json_color(value: &str) -> Result<u32> {
    if value.len() != 7 || !value.starts_with('#') {
        bail!("JSON theme color {value:?} must be #RRGGBB");
    }
    u32::from_str_radix(&value[1..], 16)
        .with_context(|| format!("invalid JSON theme color {value:?}"))
}

fn parse_color(value: &str) -> Result<u32> {
    let value = value.trim();
    let hex = value
        .strip_prefix('#')
        .or_else(|| value.strip_prefix("0x"))
        .unwrap_or(value);
    if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("color {value:?} must be #RRGGBB or 0xRRGGBB");
    }
    let hex = if value.starts_with("0x") && hex.len() == 8 {
        &hex[2..]
    } else {
        hex
    };
    if hex.len() != 6 {
        bail!("color {value:?} must be #RRGGBB or 0xRRGGBB");
    }
    u32::from_str_radix(hex, 16).with_context(|| format!("invalid color {value:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::{contrast_ratio, legacy_srgb::mix_quarters as mix_rgb_quarters};
    use crate::theme::{INACTIVE_TAB_MIN_CONTRAST, inactive_tab_foreground_fallback};

    #[test]
    fn colors_reject_non_hexadecimal_and_multibyte_input_without_panicking() {
        assert_eq!(parse_color("#1a2B3c").unwrap(), 0x1a_2b3c);
        assert_eq!(parse_color("0x1a2b3c").unwrap(), 0x1a_2b3c);
        assert_eq!(parse_color("0xff1a2b3c").unwrap(), 0x1a_2b3c);
        for value in [
            "0xzz1a2b3c",
            "0x\u{e9}123456",
            "0xa\u{e9}12345",
            "+1a2b3",
            "-1a2b3",
            "#12345",
            "1a2b3g",
            "\u{e9}\u{e9}\u{e9}",
        ] {
            assert!(parse_color(value).is_err(), "{value:?}");
        }
    }

    fn complete_foot_palette(
        background: &str,
        foreground: &str,
        selection_background: &str,
        selection_foreground: &str,
    ) -> String {
        format!(
            "[colors-dark]\nforeground={foreground}\nbackground={background}\nselection-background={selection_background}\nselection-foreground={selection_foreground}\nregular0=000000\nregular1=000001\nregular2=000002\nregular3=000003\nregular4=000004\nregular5=000005\nregular6=000006\nregular7=000007\nbright0=000008\nbright1=000009\nbright2=00000a\nbright3=00000b\nbright4=00000c\nbright5=00000d\nbright6=00000e\nbright7=00000f\n"
        )
    }

    #[test]
    fn font_shaping_startup_settings_are_explicit_bounded_and_line_numbered() {
        let defaults = parse("").unwrap().config;
        assert_eq!(defaults.font_ligatures, FontLigatures::Off);
        assert_eq!(defaults.font_features, FeatureSettings::default());
        for (text, mode) in [
            ("off", FontLigatures::Off),
            ("on", FontLigatures::On),
            ("cursor", FontLigatures::Cursor),
        ] {
            let loaded = parse(&format!(
                "[main]\nfont-ligatures={text}\nfont-features=calt=1,zero=1,ss01=2\n"
            ))
            .unwrap();
            assert!(loaded.diagnostics.is_empty());
            assert_eq!(loaded.config.font_ligatures, mode);
            assert_eq!(
                loaded.config.font_features,
                FeatureSettings::new(&[("ss01", 2), ("zero", 1), ("calt", 1)]).unwrap()
            );
        }
        assert_eq!(
            parse("main.font-features=").unwrap().config.font_features,
            FeatureSettings::default()
        );
        for value in [
            "calt",
            "cal=1",
            "caltt=1",
            "cált=1",
            "calt=-1",
            "calt=65536",
            "calt=+1",
            "calt=1,calt=0",
            "calt=1,",
            "calt=true",
            "calt=",
            "calt=1.0",
        ] {
            let error = parse(&format!("[main]\nfont-features={value}")).unwrap_err();
            assert!(
                format!("{error:#}").contains("line 2"),
                "{value}: {error:#}"
            );
        }
        let too_many = (0..65)
            .map(|i| format!("f{i:03}=1"))
            .collect::<Vec<_>>()
            .join(",");
        assert!(
            format!(
                "{:#}",
                parse(&format!("main.font-features={too_many}")).unwrap_err()
            )
            .contains("too many")
        );
        assert!(parse("main.font-features=ss01=65535").is_ok());
        assert!(parse("main.font-features=calt=1,CALT=0").is_ok());
        assert!(
            format!("{:#}", parse("[main]\nfont-ligatures=yes").unwrap_err()).contains("line 2")
        );
    }

    #[test]
    fn defaults_match_foot_font_and_resize_behavior() {
        let defaults = AppConfig::default();
        assert_eq!(defaults.font, "monospace:style=Regular");
        assert_eq!(defaults.font_authority, FontAuthority::NativeOmarchy);
        assert_eq!(defaults.font_size, FontSize::Pixels(14.0));
        assert_eq!(defaults.resize_delay_ms, 100);
        assert_eq!(defaults.keymap_profile, KeymapProfile::Splinterm);
        assert_eq!(
            defaults
                .keymap
                .primary_shortcut(crate::keymap::ActionId::ClipboardCopy),
            "Super+C"
        );
        assert_eq!(
            defaults
                .keymap
                .primary_shortcut(crate::keymap::ActionId::ClipboardPaste),
            "Super+V"
        );
        assert_eq!(defaults.prefix_timeout_ms, 1_000);
        assert_eq!(defaults.preset_path, None);
        assert!(
            defaults
                .preset_catalog
                .as_ref()
                .is_some_and(|catalog| catalog.contains("omarchy.tdl"))
        );
        assert!(!defaults.allow_unrestricted_commands);
        assert!(defaults.multiplexer_lifetime.persistent_by_default);
        assert!(defaults.multiplexer_lifetime.persist_on_tab_organization);
        assert!(!defaults.multiplexer_lifetime.close_unused);
    }

    #[test]
    fn close_unused_is_opt_in_and_strict() {
        assert!(!parse("").unwrap().config.multiplexer_lifetime.close_unused);
        for enabled in ["yes", "true", "on", "1"] {
            let loaded = parse(&format!("[multiplexer]\nclose-unused={enabled}\n")).unwrap();
            assert!(loaded.config.multiplexer_lifetime.close_unused, "{enabled}");
            assert!(loaded.diagnostics.is_empty());
        }
        for disabled in ["no", "false", "off", "0"] {
            let loaded = parse(&format!("[multiplexer]\nclose-unused={disabled}\n")).unwrap();
            assert!(
                !loaded.config.multiplexer_lifetime.close_unused,
                "{disabled}"
            );
        }
        assert!(parse("[multiplexer]\nclose-unused=sometimes\n").is_err());
    }

    #[test]
    fn clipboard_image_directory_is_opt_in_absolute_and_transactional() {
        assert_eq!(parse("").unwrap().config.clipboard_image_directory, None);
        let loaded = parse("[clipboard]\nimage-directory=/home/user/Private images\n").unwrap();
        assert_eq!(
            loaded.config.clipboard_image_directory,
            Some(PathBuf::from("/home/user/Private images"))
        );
        assert!(loaded.diagnostics.is_empty());
        for value in [
            "",
            "relative",
            "~/Images",
            "$HOME/Images",
            "/a/../b",
            "/a//b",
            "/a/",
            "/a\tb",
        ] {
            assert!(
                parse(&format!("[clipboard]\nimage-directory={value}\n")).is_err(),
                "{value:?}"
            );
        }
        assert!(parse("[clipboard]\nimage-directory=/a\nimage-directory=/b\n").is_err());
        // A late error cannot return a partially changed configuration to reload.
        assert!(
            parse("[clipboard]\nimage-directory=/a\n[main]\nfont-pixelsize=invalid\n").is_err()
        );
        assert_eq!(
            parse("[clipboard]\nunknown=x\n").unwrap().diagnostics.len(),
            1
        );
    }

    #[test]
    fn creation_directories_default_to_home_for_dojos_and_inherit_for_splints() {
        let defaults = parse("").unwrap().config;
        assert_eq!(defaults.new_dojo_directory, LaunchDirectory::Home);
        assert_eq!(defaults.new_splint_directory, LaunchDirectory::Inherit);
        for (value, expected) in [
            ("inherit", LaunchDirectory::Inherit),
            ("INHERIT", LaunchDirectory::Inherit),
            ("home", LaunchDirectory::Home),
            ("Home", LaunchDirectory::Home),
            ("~", LaunchDirectory::Home),
            ("~/", LaunchDirectory::Home),
            (
                "~/src/work",
                LaunchDirectory::HomeRelative(PathBuf::from("src/work")),
            ),
            (
                "/srv/projects",
                LaunchDirectory::Absolute(PathBuf::from("/srv/projects")),
            ),
        ] {
            let loaded = parse(&format!(
                "[multiplexer]\nnew-dojo-directory={value}\nnew-splint-directory={value}\n"
            ))
            .unwrap();
            assert!(loaded.diagnostics.is_empty(), "{value}");
            assert_eq!(loaded.config.new_dojo_directory, expected, "{value}");
            assert_eq!(loaded.config.new_splint_directory, expected, "{value}");
        }
        // Paths are resolved by the owning daemon, so `~` is never expanded here.
        assert_eq!(
            parse("[multiplexer]\nnew-dojo-directory=~/x\n")
                .unwrap()
                .config
                .new_dojo_directory,
            LaunchDirectory::HomeRelative(PathBuf::from("x"))
        );
        for value in [
            "",
            "projects",
            "./here",
            "~user/x",
            "~/../escape",
            "~/a/../b",
        ] {
            let error = parse(&format!("[multiplexer]\nnew-dojo-directory={value}\n"))
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("line 2: directory must be"),
                "{value}: {error}"
            );
            assert!(parse(&format!("[multiplexer]\nnew-splint-directory={value}\n")).is_err());
        }
    }

    #[test]
    fn multiplexer_lifetime_defaults_are_backward_compatible_and_strict() {
        let configured =
            parse("[multiplexer]\npersistent-by-default=no\npersist-on-tab-organization=off\n")
                .unwrap();
        assert!(!configured.config.multiplexer_lifetime.persistent_by_default);
        assert!(
            !configured
                .config
                .multiplexer_lifetime
                .persist_on_tab_organization
        );
        assert!(configured.diagnostics.is_empty());

        for enabled in ["yes", "true", "on", "1"] {
            let loaded =
                parse(&format!("[multiplexer]\npersistent-by-default={enabled}\n")).unwrap();
            assert!(loaded.config.multiplexer_lifetime.persistent_by_default);
        }
        for disabled in ["no", "false", "off", "0"] {
            let loaded = parse(&format!(
                "[multiplexer]\npersist-on-tab-organization={disabled}\n"
            ))
            .unwrap();
            assert!(
                !loaded
                    .config
                    .multiplexer_lifetime
                    .persist_on_tab_organization
            );
        }
        assert!(
            parse("[multiplexer]\npersistent-by-default=sometimes\n")
                .unwrap_err()
                .to_string()
                .contains("line 2: expected boolean")
        );
        let unknown = parse("[multiplexer]\npersistence-by-default=no\n").unwrap();
        assert_eq!(unknown.diagnostics.len(), 1);
        assert!(unknown.diagnostics[0].contains("multiplexer.persistence-by-default"));
    }

    #[test]
    fn explicit_monospace_pattern_retains_explicit_authority() {
        let loaded = parse("[main]\nfont=monospace:style=Regular\n").unwrap();
        assert_eq!(loaded.config.font, "monospace:style=Regular");
        assert_eq!(loaded.config.font_authority, FontAuthority::Explicit);
    }

    #[test]
    fn supported_subset_and_diagnostics_are_explicit() {
        let loaded = parse("[main]\nfont=Mono\nfont-size=14\ninitial-columns=100\napp-id=spoof\nunknown=x\n[cursor]\nstyle=beam\nblink=no\n").unwrap();
        assert_eq!(loaded.config.font, "Mono");
        assert_eq!(loaded.config.font_size, FontSize::Pixels(14.0));
        assert_eq!(loaded.config.initial_columns, 100);
        assert_eq!(loaded.config.cursor_style, CursorStyle::Beam);
        assert!(!loaded.config.cursor_blink);
        assert_eq!(loaded.config.startup, StartupMode::NewDojo);
        assert_eq!(
            parse("[main]\nstartup=dojo-picker\n")
                .unwrap()
                .config
                .startup,
            StartupMode::DojoPicker
        );
        assert!(
            parse("[main]\nstartup=picker\n")
                .unwrap_err()
                .to_string()
                .contains("startup must be new-dojo or dojo-picker")
        );
        assert_eq!(loaded.diagnostics.len(), 3);
        assert!(
            loaded
                .diagnostics
                .iter()
                .any(|line| line.contains("font-size is deprecated"))
        );
    }

    #[test]
    fn initial_grid_accepts_protocol_maxima_and_rejects_larger_values() {
        let loaded = parse("[main]\ninitial-columns=480\ninitial-rows=128\n").unwrap();
        assert_eq!(loaded.config.initial_columns, MAX_COLUMNS);
        assert_eq!(loaded.config.initial_rows, MAX_ROWS);
        assert!(parse("[main]\ninitial-columns=481\n").is_err());
        assert!(parse("[main]\ninitial-rows=129\n").is_err());
    }

    #[test]
    fn geometry_font_units_and_padding_are_explicit() {
        let loaded = parse(
            "[main]\nfont-point-size=12\nfont-sizing-policy=physical-dpi\npadding-left=1\npadding-right=2\npadding-top=3\npadding-bottom=4\n",
        )
        .unwrap();
        assert_eq!(loaded.config.font_size, FontSize::Points(12.0));
        assert_eq!(
            loaded.config.font_sizing_policy,
            FontSizingPolicy::PhysicalDpi
        );
        assert_eq!(
            loaded.config.padding,
            TerminalPadding {
                left: 1,
                right: 2,
                top: 3,
                bottom: 4
            }
        );
    }

    #[test]
    fn legacy_splinterm_dpi_aware_migration_is_safe_and_conflicts_fail() {
        let legacy = parse("[main]\ndpi-aware=yes\n").unwrap();
        assert_eq!(
            legacy.config.font_sizing_policy,
            FontSizingPolicy::OutputScale
        );
        assert!(legacy.diagnostics[0].contains("legacy Splinterm"));
        let no = parse("[main]\ndpi-aware=no\n").unwrap_err().to_string();
        assert!(no.contains("forced the whole surface to 1x"));
        assert!(
            parse("[main]\ndpi-aware=yes\nfont-sizing-policy=output-scale\n")
                .unwrap_err()
                .to_string()
                .contains("conflicts")
        );
        assert!(
            parse("[main]\nfont-sizing-policy=output-scale\ndpi-aware=yes\n")
                .unwrap_err()
                .to_string()
                .contains("conflicts")
        );
    }

    #[test]
    fn font_sizing_authorities_cannot_conflict_or_hide_in_face_pattern() {
        assert!(parse("[main]\nfont-pixelsize=12\nfont-point-size=12\n").is_err());
        assert!(
            parse("[main]\nfont=Mono:size=12\n")
                .unwrap_err()
                .to_string()
                .contains("face/style")
        );
    }

    #[test]
    fn chrome_color_keys_parse_strictly_and_default_to_the_theme() {
        let loaded = parse(
            "[colors]\nactive-tab-background=#102030\nactive-tab-alpha=0.5\n\
             tab-strip-alpha=0.9\nexplorer-background=0x405060\nexplorer-alpha=1.0\n",
        )
        .unwrap()
        .config;
        assert_eq!(
            loaded.chrome_colors,
            ChromeColorOverrides {
                active_tab_background: Some(0x10_20_30),
                active_tab_alpha: Some(foot_alpha(0.5)),
                tab_strip_alpha: Some(foot_alpha(0.9)),
                explorer_background: Some(0x40_50_60),
                explorer_alpha: Some(u16::MAX),
            }
        );
        assert_eq!(
            parse("").unwrap().config.chrome_colors,
            ChromeColorOverrides::NONE
        );
        for bad in [
            "active-tab-background=red",
            "explorer-background=#12345",
            "active-tab-alpha=1.5",
            "tab-strip-alpha=-0.1",
            "explorer-alpha=nan",
        ] {
            let error = parse(&format!("[colors]\n{bad}\n"))
                .unwrap_err()
                .to_string();
            assert!(error.starts_with("line 2:"), "{bad}: {error}");
        }
    }

    #[test]
    fn chrome_overrides_keep_the_selected_tab_opaque_and_follow_the_theme_otherwise() {
        let glassy = ResolvedTheme {
            background_alpha: foot_alpha(0.65),
            ..ResolvedTheme::default()
        };
        let theme = glassy.with_chrome_overrides(ChromeColorOverrides::NONE);
        assert_eq!(theme, glassy);
        assert_eq!(theme.chrome.active_tab_alpha, u16::MAX);
        assert_eq!(theme.tab_strip_alpha(), foot_alpha(0.65));
        assert_eq!(theme.explorer_alpha(), foot_alpha(0.65));
        assert_eq!(theme.explorer_background(), theme.background);

        let light_tab = glassy.with_chrome_overrides(ChromeColorOverrides {
            active_tab_background: Some(0xf0_f0_f0),
            active_tab_alpha: Some(foot_alpha(0.8)),
            tab_strip_alpha: Some(u16::MAX),
            explorer_background: Some(0x20_20_20),
            explorer_alpha: Some(foot_alpha(0.4)),
        });
        assert_eq!(light_tab.active_tab_background, 0xf0_f0_f0);
        // The label is chosen again: the dark background reads on a light tab.
        assert_eq!(light_tab.active_tab_foreground, glassy.background);
        assert!(contrast_ratio(light_tab.active_tab_foreground, 0xf0_f0_f0) >= 4.5);
        assert_eq!(light_tab.chrome.active_tab_alpha, foot_alpha(0.8));
        assert_eq!(light_tab.tab_strip_alpha(), u16::MAX);
        assert_eq!(light_tab.explorer_background(), 0x20_20_20);
        assert_eq!(light_tab.explorer_alpha(), foot_alpha(0.4));
        // The terminal's own glass is untouched.
        assert_eq!(light_tab.background_alpha, foot_alpha(0.65));
    }

    #[test]
    fn foot_background_alpha_and_blur_are_strict_last_assignment_overrides() {
        let loaded = parse("[colors]\nalpha=0.5\nblur=no\nalpha=0.888\nblur=yes\n").unwrap();
        assert_eq!(loaded.config.background_alpha, Some(foot_alpha(0.888)));
        assert_eq!(loaded.config.background_blur, Some(true));

        let defaults = parse("").unwrap().config;
        assert_eq!(defaults.background_alpha, None);
        assert_eq!(defaults.background_blur, None);
        assert_eq!(
            parse("[colors]\nblur=yes\nblur=no\n")
                .unwrap()
                .config
                .background_blur,
            Some(false)
        );
        for alpha in ["-0.1", "1.1", "NaN", "inf", "-inf"] {
            let error = parse(&format!("[colors]\nalpha={alpha}\n"))
                .unwrap_err()
                .to_string();
            assert_eq!(error, "line 2: number outside supported range");
        }
        assert!(
            parse("[colors]\nblur=perhaps\n")
                .unwrap_err()
                .to_string()
                .contains("line 2: expected boolean")
        );
    }

    #[test]
    fn invalid_ranges_and_values_fail() {
        assert!(parse("font-size=2").is_err());
        assert!(parse("[cursor]\nstyle=round").is_err());
        assert!(parse("login-shell=perhaps").is_err());
    }

    #[test]
    fn keymap_selection_is_strict_and_resolves_the_builtin_profile() {
        let loaded =
            parse("[key-bindings]\nprofile=splinterm\nprefix-timeout-ms=750\nunknown=value\n")
                .unwrap();
        assert_eq!(loaded.config.keymap.profile(), KeymapProfile::Splinterm);
        assert_eq!(
            loaded
                .config
                .keymap
                .primary_shortcut(crate::keymap::ActionId::ClipboardCopy),
            "Super+C"
        );
        assert_eq!(loaded.config.prefix_timeout_ms, 750);
        assert_eq!(loaded.diagnostics.len(), 1);
        assert!(loaded.diagnostics[0].contains("key-bindings.unknown"));
        let omarchy = parse("[key-bindings]\nprofile=omarchy-tmux\n").unwrap();
        assert_eq!(omarchy.config.keymap.profile(), KeymapProfile::OmarchyTmux);
        assert_eq!(omarchy.config.keymap.prefixes().len(), 2);
        assert!(parse("[key-bindings]\nprofile=missing\n").is_err());
        assert!(parse("[key-bindings]\nprefix-timeout-ms=249\n").is_err());
        assert!(parse("[key-bindings]\nprefix-timeout-ms=5001\n").is_err());
    }
    #[test]
    fn preset_selection_resolves_relative_paths_and_validates_explicit_files() {
        let root = std::env::temp_dir().join(format!(
            "splinterm-preset-config-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("presets.toml"),
            "version=1\n[commands.run]\nkind=\"argv\"\nargv=[\"true\"]\n[presets.one]\nkind=\"dojo\"\nname=\"one\"\nroot=\"pane\"\nfocus=\"pane\"\n[presets.one.nodes.pane]\ntype=\"pane\"\ncommand=\"run\"\n",
        )
        .unwrap();
        let loaded = parse_with_base(
            "[presets]\nfile=presets.toml\nallow-unrestricted-commands=yes\n",
            &root,
        )
        .unwrap();
        assert_eq!(loaded.config.preset_path, Some(root.join("presets.toml")));
        assert!(
            loaded
                .config
                .preset_catalog
                .as_ref()
                .unwrap()
                .contains("one")
        );
        assert!(loaded.config.allow_unrestricted_commands);
        assert!(parse_with_base("[presets]\nfile=missing.toml\n", &root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pane_divider_configuration_is_explicit_and_bounded() {
        let defaults = parse("").unwrap().config;
        assert_eq!(defaults.pane_divider_style, PaneDividerStyle::Line);
        assert_eq!(defaults.frame_title_mode, FrameTitleMode::Splint);
        for (value, expected) in [
            ("none", PaneDividerStyle::None),
            ("line", PaneDividerStyle::Line),
            ("frame", PaneDividerStyle::Frame),
        ] {
            let loaded = parse(&format!("[multiplexer]\ndivider-style={value}\n")).unwrap();
            assert_eq!(loaded.config.pane_divider_style, expected);
        }
        for (value, expected) in [
            ("none", FrameTitleMode::None),
            ("splint", FrameTitleMode::Splint),
        ] {
            let loaded = parse(&format!("[multiplexer]\nframe-title={value}\n")).unwrap();
            assert_eq!(loaded.config.frame_title_mode, expected);
        }
        assert!(parse("[multiplexer]\ndivider-style=tmux\n").is_err());
        assert!(parse("[multiplexer]\nframe-title=osc\n").is_err());
    }

    #[test]
    fn pane_gap_is_bounded_logical_pixels_defaulting_to_none() {
        assert_eq!(parse("").unwrap().config.pane_gap, 0);
        assert_eq!(AppConfig::default().pane_gap, 0);
        for (value, expected) in [("0", 0), ("8", 8), (" 12 ", 12), ("64", MAX_PANE_GAP)] {
            let loaded = parse(&format!("[multiplexer]\npane-gap={value}\n")).unwrap();
            assert_eq!(loaded.config.pane_gap, expected, "{value:?}");
            assert!(loaded.diagnostics.is_empty(), "{value:?}");
        }
        for value in ["65", "-1", "4.5", "nan", "NaN", "inf", "", "eight", "1e1"] {
            assert!(
                parse(&format!("[multiplexer]\npane-gap={value}\n")).is_err(),
                "{value:?}"
            );
        }
        // The key belongs to [multiplexer]; elsewhere it is an unknown key.
        let misplaced = parse("[main]\npane-gap=8\n").unwrap();
        assert_eq!(misplaced.config.pane_gap, 0);
        assert!(!misplaced.diagnostics.is_empty());
    }

    #[test]
    fn explorer_agent_status_presentation_defaults_on_and_can_be_hidden() {
        assert!(AppConfig::default().explorer_agent_status);
        for (value, expected) in [("yes", true), ("no", false), ("off", false), ("1", true)] {
            let loaded = parse(&format!("[explorer]\nagent-status={value}\n")).unwrap();
            assert_eq!(loaded.config.explorer_agent_status, expected);
            assert!(loaded.diagnostics.is_empty());
        }
        assert!(parse("[explorer]\nagent-status=spinner\n").is_err());
        let shipped = parse(include_str!("../../../config/splinterm/config.ini")).unwrap();
        assert!(shipped.diagnostics.is_empty(), "{:?}", shipped.diagnostics);
        assert!(shipped.config.explorer_agent_status);
    }

    #[test]
    fn explorer_status_colors_and_glyphs_default_on_and_icons() {
        let defaults = AppConfig::default().explorer_status_style;
        assert!(defaults.colors);
        assert_eq!(defaults.glyphs, StatusGlyphs::Icons);
        let loaded = parse("[explorer]\nstatus-colors=no\nstatus-glyphs=dots\n").unwrap();
        assert!(loaded.diagnostics.is_empty());
        assert_eq!(
            loaded.config.explorer_status_style,
            AgentStatusStyle {
                colors: false,
                glyphs: StatusGlyphs::Dots,
            }
        );
        let icons = parse("[explorer]\nstatus-glyphs=Icons\n").unwrap();
        assert_eq!(
            icons.config.explorer_status_style.glyphs,
            StatusGlyphs::Icons
        );
        assert!(parse("[explorer]\nstatus-glyphs=ascii\n").is_err());
        assert!(parse("[explorer]\nstatus-colors=maybe\n").is_err());
        let shipped = parse(include_str!("../../../config/splinterm/config.ini")).unwrap();
        assert_eq!(
            shipped.config.explorer_status_style,
            AgentStatusStyle::default()
        );
    }

    #[test]
    fn explorer_full_height_defaults_on_and_can_be_turned_off() {
        assert!(AppConfig::default().explorer_full_height);
        assert!(parse("[explorer]\n").unwrap().config.explorer_full_height);
        assert!(
            !parse("[explorer]\nfull-height=no\n")
                .unwrap()
                .config
                .explorer_full_height
        );
        let error = parse("[explorer]\nfull-height=maybe\n")
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("line 2:"), "{error}");
    }

    #[test]
    fn explorer_collapsed_defaults_off_and_starts_the_rail() {
        assert!(!AppConfig::default().explorer_collapsed);
        assert!(!parse("[explorer]\n").unwrap().config.explorer_collapsed);
        for (value, expected) in [("yes", true), ("on", true), ("no", false), ("0", false)] {
            let loaded = parse(&format!("[explorer]\ncollapsed={value}\n")).unwrap();
            assert_eq!(loaded.config.explorer_collapsed, expected, "{value}");
            assert!(loaded.diagnostics.is_empty());
        }
        let error = parse("[explorer]\ncollapsed=rail\n")
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("line 2:"), "{error}");
        // The key belongs to [explorer]; elsewhere it is an unknown key.
        let misplaced = parse("[main]\ncollapsed=yes\n").unwrap();
        assert!(!misplaced.config.explorer_collapsed);
        assert!(!misplaced.diagnostics.is_empty());
    }

    #[test]
    fn tab_strip_border_defaults_on_and_can_be_turned_off() {
        assert!(AppConfig::default().tab_strip_border);
        assert!(parse("[multiplexer]\n").unwrap().config.tab_strip_border);
        for (value, expected) in [
            ("yes", true),
            ("on", true),
            ("true", true),
            ("1", true),
            ("no", false),
            ("off", false),
            ("false", false),
            ("0", false),
            ("No", false),
        ] {
            let loaded = parse(&format!("[multiplexer]\ntab-strip-border={value}\n")).unwrap();
            assert_eq!(loaded.config.tab_strip_border, expected, "{value}");
            assert!(loaded.diagnostics.is_empty());
        }
        for invalid in ["", "thin", "2", "line"] {
            assert!(
                parse(&format!("[multiplexer]\ntab-strip-border={invalid}\n")).is_err(),
                "{invalid:?}"
            );
        }
        let shipped = parse(include_str!("../../../config/splinterm/config.ini")).unwrap();
        assert!(shipped.diagnostics.is_empty(), "{:?}", shipped.diagnostics);
        assert!(shipped.config.tab_strip_border);
    }

    #[test]
    fn omarchy_integration_hint_defaults_on_and_can_be_suppressed() {
        assert!(AppConfig::default().omarchy_integration_hint);
        for (value, expected) in [("yes", true), ("no", false), ("off", false), ("1", true)] {
            let loaded = parse(&format!("[main]\nomarchy-integration-hint={value}\n")).unwrap();
            assert_eq!(loaded.config.omarchy_integration_hint, expected);
            assert_eq!(loaded.diagnostics, Vec::<String>::new());
        }
        assert!(parse("[main]\nomarchy-integration-hint=later\n").is_err());
        let shipped = parse(include_str!("../../../config/splinterm/config.ini")).unwrap();
        assert!(shipped.diagnostics.is_empty(), "{:?}", shipped.diagnostics);
        assert!(shipped.config.omarchy_integration_hint);
    }

    #[test]
    fn the_dialog_shortcut_defaults_to_ctrl_alt_g_and_can_change_or_turn_off() {
        let default = Some(DialogShortcut::default());
        assert_eq!(AppConfig::default().dialog_shortcut, default);
        let loaded = parse("[integration]\ndialog-shortcut=Ctrl+Alt+G\n").unwrap();
        assert_eq!(loaded.config.dialog_shortcut, default);
        assert_eq!(loaded.diagnostics, Vec::<String>::new());
        let changed = parse("[integration]\ndialog-shortcut=alt+ctrl+7\n").unwrap();
        assert_eq!(
            changed.config.dialog_shortcut.map(DialogShortcut::label),
            Some("Ctrl+Alt+7".to_owned())
        );
        for off in ["none", "NONE", " none "] {
            let loaded = parse(&format!("[integration]\ndialog-shortcut={off}\n")).unwrap();
            assert_eq!(loaded.config.dialog_shortcut, None, "{off:?}");
        }
        for invalid in [
            "g",
            "ctrl+c",
            "alt+g",
            "super+alt+g",
            "ctrl+alt+shift+g",
            "ctrl+alt+enter",
            "ctrl+alt+",
            "",
        ] {
            let error = parse(&format!("[integration]\ndialog-shortcut={invalid}\n"))
                .expect_err(invalid)
                .to_string();
            assert!(error.contains("line 2: dialog-shortcut"), "{error}");
        }
        assert_eq!(
            parse("[integration]\nunknown=x\n")
                .unwrap()
                .diagnostics
                .len(),
            1,
            "unknown keys in the section are still reported"
        );
        let shipped = parse(include_str!("../../../config/splinterm/config.ini")).unwrap();
        assert!(shipped.diagnostics.is_empty(), "{:?}", shipped.diagnostics);
        assert_eq!(shipped.config.dialog_shortcut, default);
    }

    #[test]
    fn theme_source_defaults_to_native_omarchy_and_allows_explicit_json() {
        let defaults = AppConfig::default();
        assert!(defaults.theme_path.is_none());
        assert!(matches!(defaults.theme_source(), ThemeSource::Omarchy(_)));

        let loaded = parse("[main]\ntheme=/tmp/splinterm-theme.json\n")
            .unwrap()
            .config;
        assert_eq!(
            loaded.theme_source(),
            ThemeSource::Json {
                path: PathBuf::from("/tmp/splinterm-theme.json"),
                shell: Some(OmarchyThemeRoots::from_environment()),
            }
        );
    }

    #[test]
    fn native_omarchy_theme_uses_effective_foot_palette_and_semantic_accent() {
        let colors = "accent = \"0x010203\" # inline comment\nlighter_bg = \"#070809\"\n";
        let foot = "[colors]\nforeground=000003\nbackground=000001\nselection-foreground=000002\nselection-background=000004\ncursor=000001 000006\nalpha=0.75\nblur=yes\nregular0=000000\nregular1=000001\nregular2=000002\nregular3=000003\nregular4=000004\nregular5=000005\nregular6=000006\nregular7=000007\nbright0=000008\nbright1=000009\nbright2=00000a\nbright3=00000b\nbright4=00000c\nbright5=00000d\nbright6=00000e\nbright7=00000f\n";
        let theme = resolve_omarchy_theme(colors, foot).unwrap();
        assert_eq!(theme.background, 1);
        assert_eq!(theme.foreground, 3);
        assert_eq!(theme.cursor, 6);
        assert_eq!(theme.selection, 4);
        assert_eq!(theme.selection_foreground, 2);
        assert_eq!(theme.active_tab_background, 0x07_08_09);
        assert_eq!(theme.active_tab_foreground, theme.background);
        assert_eq!(theme.url, 4);
        assert_eq!(theme.ui_accent(), 0x01_02_03);
        assert_eq!(theme.pane_border(), 8);
        assert_eq!(theme.pane_border_active(), 0x01_02_03);
        assert_eq!(
            theme.ansi,
            std::array::from_fn(|index| u32::try_from(index).unwrap())
        );
        assert_eq!(theme.background_alpha, foot_alpha(0.75));
        assert!(theme.background_blur);
    }

    #[test]
    fn native_omarchy_theme_merges_colors_into_colors_dark_and_rejects_incomplete_input() {
        let complete = |background: &str| {
            format!(
                "foreground=000003\nbackground={background}\nregular0=000000\nregular1=000001\nregular2=000002\nregular3=000003\nregular4=000004\nregular5=000005\nregular6=000006\nregular7=000007\nbright0=000008\nbright1=000009\nbright2=00000a\nbright3=00000b\nbright4=00000c\nbright5=00000d\nbright6=00000e\nbright7=00000f\n"
            )
        };
        let foot = format!(
            "[colors]\n{}[colors-dark]\n{}",
            complete("000001"),
            complete("000002")
        );
        let resolved = resolve_omarchy_theme("cursor=\"#000006\"", &foot).unwrap();
        assert_eq!(resolved.background, 2);
        assert_eq!(resolved.selection_foreground, resolved.foreground);
        assert_eq!(resolved.active_tab_background, resolved.ansi[8]);
        assert!(
            resolve_omarchy_theme("accent=\"#000006\"", "[colors-dark]\nbackground=000001")
                .unwrap_err()
                .to_string()
                .contains("missing regular0")
        );
        // Foot parses [colors] into the dark theme, so an empty [colors-dark]
        // keeps the [colors] palette, a later key overrides one key, and file
        // order decides between the two headers.
        let empty_dark = format!("[colors]\n{}[colors-dark]\n", complete("000001"));
        assert_eq!(
            resolve_omarchy_theme("accent=\"#000006\"", &empty_dark)
                .unwrap()
                .background,
            1
        );
        let one_key = format!(
            "[colors]\n{}[colors-dark]\nforeground=000009\n",
            complete("000001")
        );
        let one_key = resolve_omarchy_theme("", &one_key).unwrap();
        assert_eq!((one_key.background, one_key.foreground), (1, 9));
        let dark_first = format!(
            "[colors-dark]\n{}[colors]\nbackground=000005\n",
            complete("000002")
        );
        assert_eq!(
            resolve_omarchy_theme("", &dark_first).unwrap().background,
            5
        );
        // [colors2] is Foot's deprecated light section.
        let colors2 = format!(
            "initial-color-theme=light\n[colors2]\n{}[colors-dark]\n{}",
            complete("000007"),
            complete("000002")
        );
        assert_eq!(resolve_omarchy_theme("", &colors2).unwrap().background, 7);
        assert!(
            resolve_omarchy_theme("accent=\"#000006\"", "[colors-light]\n")
                .unwrap_err()
                .to_string()
                .contains("no palette for the selected initial-color-theme")
        );
    }

    /// Verbatim stock Omarchy 4 `colors.toml` files (`4a9c83de`): tokyo-night,
    /// the light flexoki-light, and last-horizon with its gradient and extra
    /// keys; gruvbox from before the canonical rename (`afa2839a^`); and kanagawa
    /// converted by `omarchy-theme-colors-from-alacritty`. Each `foot.ini` is the
    /// stock `foot.ini.tpl` rendered with `omarchy-theme-color --all`.
    const OMARCHY4_FIXTURES: [(&str, &str, &str); 5] = [
        (
            "tokyo-night",
            include_str!("../tests/fixtures/omarchy4/tokyo-night/colors.toml"),
            include_str!("../tests/fixtures/omarchy4/tokyo-night/foot.ini"),
        ),
        (
            "flexoki-light",
            include_str!("../tests/fixtures/omarchy4/flexoki-light/colors.toml"),
            include_str!("../tests/fixtures/omarchy4/flexoki-light/foot.ini"),
        ),
        (
            "last-horizon",
            include_str!("../tests/fixtures/omarchy4/last-horizon/colors.toml"),
            include_str!("../tests/fixtures/omarchy4/last-horizon/foot.ini"),
        ),
        (
            "legacy-gruvbox",
            include_str!("../tests/fixtures/omarchy4/legacy-gruvbox/colors.toml"),
            include_str!("../tests/fixtures/omarchy4/legacy-gruvbox/foot.ini"),
        ),
        (
            "alacritty-kanagawa",
            include_str!("../tests/fixtures/omarchy4/alacritty-kanagawa/colors.toml"),
            include_str!("../tests/fixtures/omarchy4/alacritty-kanagawa/foot.ini"),
        ),
    ];

    #[test]
    fn stock_omarchy_4_themes_drive_the_selected_tab_from_lighter_background() {
        // (fixture, selected tab, accent, active border, muted, terminal
        // background)
        let expected = [
            (
                "tokyo-night",
                0x24283b,
                0x7aa2f7,
                0x7aa2f7,
                Some(0x414868),
                0x1a1b26,
            ),
            (
                "flexoki-light",
                0xe6e4d9,
                0x205ea6,
                0x205ea6,
                Some(0xb7b5ac),
                0xfffcf0,
            ),
            // Its active_border_color is the focus ring.
            (
                "last-horizon",
                0x0c0b0c,
                0xb59790,
                0xd6d3de,
                Some(0x584e51),
                0x0c0b0c,
            ),
            (
                "legacy-gruvbox",
                0x3c3836,
                0x7daea3,
                0x7daea3,
                Some(0x665c54),
                0x282828,
            ),
            // No lighter_background or lighter_bg: Foot bright0 (color8). No
            // muted or dark_foreground: the inactive tab mixes the text.
            (
                "alacritty-kanagawa",
                0x727169,
                0x7e9cd8,
                0x7e9cd8,
                None,
                0x1f1f28,
            ),
        ];
        for (
            (name, colors, foot),
            (expected_name, tab, accent, active_border, muted, background),
        ) in OMARCHY4_FIXTURES.iter().zip(expected)
        {
            assert_eq!(*name, expected_name);
            let theme = resolve_omarchy_theme(colors, foot).unwrap();
            assert_eq!(theme.active_tab_background, tab, "{name} selected tab");
            assert_eq!(theme.ui_accent(), accent, "{name} accent");
            assert_eq!(
                theme.pane_border_active(),
                active_border,
                "{name} active border"
            );
            assert_eq!(theme.background, background, "{name} background");
            assert_eq!(
                theme.inactive_tab_foreground(),
                muted.map_or_else(
                    || mixed_muted(&theme),
                    |muted| inactive_tab_foreground_fallback(
                        theme.background,
                        theme.foreground,
                        muted
                    )
                ),
                "{name} inactive tab"
            );
            assert_eq!(
                theme.active_tab_foreground,
                active_tab_foreground_fallback(theme.background, theme.foreground, tab),
                "{name} selected tab label"
            );
            // The stock template sets blur=yes and no alpha.
            assert_eq!(theme.background_alpha, u16::MAX, "{name} alpha");
            assert!(theme.background_blur, "{name} blur");
        }
        // last-horizon's own active_tab_background extension stays ignored.
        assert!(
            OMARCHY4_FIXTURES[2]
                .1
                .contains("active_tab_background = \"#a5a0b6\"")
        );
    }

    #[test]
    fn omarchy_roles_read_canonical_names_before_legacy_aliases() {
        let foot = complete_foot_palette("101112", "d0d1d2", "f0e0d0", "c0b0a0");
        let both = resolve_omarchy_theme(
            "lighter_bg = \"#111111\"\nlighter_background = \"#222222\"\nmuted = \"\"\ncolor8 = \"#333333\"\ncolor4 = \"#555555\"\nblue = \"#444444\"\n",
            &foot,
        )
        .unwrap();
        assert_eq!(both.active_tab_background, 0x22_22_22);
        // An empty muted is absent: color8 is the divider, and the inactive
        // tab, with no muted or dark_foreground, mixes the text.
        assert_eq!(both.pane_border(), 0x33_33_33);
        assert_eq!(both.inactive_tab_foreground(), mixed_muted(&both));
        assert_eq!(both.ui_accent(), 0x44_44_44);

        let legacy =
            resolve_omarchy_theme("lighter_bg=\"#111111\"\ndark_fg=\"#666666\"\n", &foot).unwrap();
        assert_eq!(legacy.active_tab_background, 0x11_11_11);
        assert_eq!(
            legacy.inactive_tab_foreground(),
            inactive_tab_foreground_fallback(legacy.background, legacy.foreground, 0x66_66_66)
        );
        assert_eq!(legacy.ui_accent(), legacy.cursor);

        let error = resolve_omarchy_theme("lighter_background=\"rgba(1,2,3)\"\n", &foot)
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid lighter_background"), "{error}");
    }

    #[test]
    fn legacy_colors_only_theme_keeps_its_palette_under_an_appended_glass_block() {
        // Produced by Omarchy's omarchy-theme-set-glass (4a9c83de) with
        // Glassy Windows on: the file has no [colors-dark] header, so the
        // script appends a [colors-dark] section holding only alpha and blur.
        let plain = include_str!("../tests/fixtures/omarchy4/glass/legacy-colors.foot.ini");
        let glassy = include_str!("../tests/fixtures/omarchy4/glass/legacy-colors.glassy.foot.ini");
        assert!(plain.starts_with("[colors]\n") && !plain.contains("[colors-dark]"));
        assert!(glassy.ends_with(
            "# omarchy-glass begin\n[colors-dark]\nalpha=0.65\nblur=yes\n# omarchy-glass end\n"
        ));
        let colors = OMARCHY4_FIXTURES[0].1;
        let plain = resolve_omarchy_theme(colors, plain).unwrap();
        let glassy = resolve_omarchy_theme(colors, glassy).unwrap();
        assert_eq!(plain.background, 0x1a_1b_26);
        assert_eq!(
            (plain.background_alpha, plain.background_blur),
            (u16::MAX, false)
        );
        assert_eq!(
            (glassy.background_alpha, glassy.background_blur),
            (foot_alpha(0.65), true)
        );
        assert_eq!(
            glassy.with_color_overrides(Some(u16::MAX), Some(false)),
            plain
        );
    }

    #[test]
    fn omarchy_theme_roots_prefer_xdg_then_home_state_then_omarchy_3() {
        let home = || Some(OsString::from("/home/test"));
        let state = PathBuf::from("/home/test/.local/state/omarchy/current/theme");
        let legacy = PathBuf::from("/home/test/.config/omarchy/current/theme");
        for xdg in [None, Some(""), Some("/home/test/.local/state")] {
            let roots = OmarchyThemeRoots::from_values(xdg.map(OsString::from), home());
            assert_eq!(
                roots.candidates(),
                [state.clone(), legacy.clone()],
                "{xdg:?}"
            );
        }
        let roots = OmarchyThemeRoots::from_values(Some(OsString::from("/srv/state")), home());
        assert_eq!(
            roots.candidates(),
            [
                PathBuf::from("/srv/state/omarchy/current/theme"),
                state.clone(),
                legacy.clone()
            ]
        );
        assert!(OmarchyThemeRoots::ordered(Vec::new()).is_none());

        // The stateless walk takes the first claimed root: one whose theme, or
        // the theme Omarchy stages as current/next-theme, exists. A stale
        // empty current in a higher root never shadows the live theme.
        let root = std::env::temp_dir().join(format!(
            "splinterm-omarchy-theme-roots-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let roots = OmarchyThemeRoots::from_values(
            Some(root.join("xdg").into_os_string()),
            Some(root.join("home").into_os_string()),
        );
        let [xdg, state, legacy] = roots.candidates() else {
            panic!("three candidates");
        };
        assert_eq!(roots.select(), xdg);
        fs::create_dir_all(legacy).unwrap();
        assert_eq!(roots.select(), legacy);
        fs::create_dir_all(state.parent().unwrap()).unwrap();
        assert_eq!(roots.select(), legacy, "an empty current claims nothing");
        fs::create_dir_all(state.with_file_name("next-theme")).unwrap();
        assert_eq!(roots.select(), state, "a staged theme claims its root");
        fs::create_dir_all(xdg.parent().unwrap()).unwrap();
        assert_eq!(roots.select(), state, "a stale empty current never shadows");
        fs::create_dir_all(xdg).unwrap();
        assert_eq!(roots.select(), xdg);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_omarchy_theme_honors_initial_light_palette_without_mixing_sections() {
        let dark = complete_foot_palette("101112", "d0d1d2", "303132", "e0e1e2");
        let light = complete_foot_palette("f8f9fa", "202122", "c0c1c2", "101112")
            .replace("[colors-dark]", "[colors-light]")
            + "alpha=0.85\nblur=yes\n";
        let expected =
            resolve_omarchy_theme("", &format!("[main]\ninitial-color-theme=light\n{light}"))
                .unwrap();
        assert_eq!(expected.background, 0xf8_f9_fa);
        assert_eq!(expected.foreground, 0x20_21_22);
        assert_eq!(expected.selection, 0xc0_c1_c2);
        assert_eq!(expected.selection_foreground, 0x10_11_12);
        assert_eq!(expected.background_alpha, foot_alpha(0.85));
        assert!(expected.background_blur);
        for palettes in [format!("{dark}{light}"), format!("{light}{dark}")] {
            for main in ["[main]\n", ""] {
                let foot = format!("{main}initial-color-theme=light\n{palettes}");
                assert_eq!(resolve_omarchy_theme("", &foot).unwrap(), expected);
            }
            assert_eq!(
                resolve_omarchy_theme("", &palettes).unwrap().background,
                0x10_11_12
            );
        }
        for selected in ["[colors-light]\n", "[colors-light]\nbackground=f8f9fa\n"] {
            assert!(
                resolve_omarchy_theme(
                    "",
                    &format!("[main]\ninitial-color-theme=light\n{dark}{selected}")
                )
                .is_err()
            );
        }
        assert!(
            resolve_omarchy_theme("", &format!("[main]\ninitial-color-theme=invalid\n{dark}"))
                .is_err()
        );
    }

    #[test]
    fn missing_active_tab_background_uses_the_normal_background_ramp() {
        let foot = complete_foot_palette("101112", "d0d1d2", "f0e0d0", "c0b0a0");

        let semantic = resolve_omarchy_theme("lighter_bg=\"#f8f9fa\"\n", &foot).unwrap();
        assert_eq!(semantic.selection, 0xf0_e0_d0);
        assert_eq!(semantic.active_tab_background, 0xf8_f9_fa);
        assert_eq!(semantic.active_tab_foreground, semantic.background);

        let ignored_extensions = resolve_omarchy_theme(
            "lighter_bg=\"#f8f9fa\"\nactive_tab_background=\"#010203\"\nactive_tab_foreground=\"#ffffff\"\n",
            &foot,
        )
        .unwrap();
        assert_eq!(ignored_extensions, semantic);

        let legacy = resolve_omarchy_theme("", &foot).unwrap();
        assert_eq!(legacy.selection, 0xf0_e0_d0);
        assert_eq!(legacy.active_tab_background, legacy.ansi[8]);
        assert_ne!(legacy.active_tab_background, legacy.selection);
    }

    #[test]
    fn startup_theme_load_records_the_actual_native_fallback_decision() {
        let root = std::env::temp_dir().join(format!(
            "splinterm-startup-theme-decision-{}",
            std::process::id()
        ));
        assert!(!root.exists());
        let theme = root.join("theme");
        let source = ThemeSource::Omarchy(OmarchyThemeRoots::fixed(theme.clone()));
        let missing = load_theme_source_for_startup(&source).unwrap();
        assert!(missing.used_omarchy_fallback);
        assert_eq!(missing.theme, ResolvedTheme::default());
        let json = ThemeSource::Json {
            path: root.join("theme.json"),
            shell: None,
        };
        let explicit = load_theme_source_for_startup(&json).unwrap();
        assert!(!explicit.used_omarchy_fallback);
        assert_eq!(
            explicit.theme, missing.theme,
            "palette equality is not provenance"
        );

        std::fs::create_dir_all(&theme).unwrap();
        std::fs::write(
            theme.join("colors.toml"),
            include_str!("../tests/fixtures/omarchy4/tokyo-night/colors.toml"),
        )
        .unwrap();
        std::fs::write(
            theme.join("foot.ini"),
            include_str!("../tests/fixtures/omarchy4/tokyo-night/foot.ini"),
        )
        .unwrap();
        let loaded = load_theme_source_for_startup(&source).unwrap();
        assert!(!loaded.used_omarchy_fallback);
        assert_eq!(loaded.theme, load_theme_source(&source).unwrap());
        std::fs::remove_dir_all(root).unwrap();
        assert!(load_live_omarchy_theme(&theme).is_err());
    }

    #[test]
    fn native_omarchy_theme_rejects_cross_generation_reads() {
        let root = std::env::temp_dir().join(format!(
            "splinterm-native-theme-snapshot-{}",
            std::process::id()
        ));
        let theme = root.join("theme");
        let old = root.join("old");
        let next = root.join("next");
        let foot = |background: &str| {
            format!(
                "[colors-dark]\nforeground=000003\nbackground={background}\nregular0=000000\nregular1=000001\nregular2=000002\nregular3=000003\nregular4=000004\nregular5=000005\nregular6=000006\nregular7=000007\nbright0=000008\nbright1=000009\nbright2=00000a\nbright3=00000b\nbright4=00000c\nbright5=00000d\nbright6=00000e\nbright7=00000f\n"
            )
        };
        for (directory, accent, background) in
            [(&theme, "#000006", "000001"), (&next, "#000007", "000002")]
        {
            std::fs::create_dir_all(directory).unwrap();
            std::fs::write(
                directory.join("colors.toml"),
                format!("accent=\"{accent}\"\n"),
            )
            .unwrap();
            std::fs::write(directory.join("foot.ini"), foot(background)).unwrap();
        }
        let error = load_omarchy_theme_with(
            &theme,
            MissingOmarchyTheme::Reject,
            || {},
            || {
                std::fs::rename(&theme, &old).unwrap();
                std::fs::rename(&next, &theme).unwrap();
            },
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("changed while its palette was loading"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_live_theme_never_publishes_fallback_during_replacement_gap() {
        let root =
            std::env::temp_dir().join(format!("splinterm-native-theme-gap-{}", std::process::id()));
        let theme = root.join("theme");
        let displaced = root.join("displaced");
        std::fs::create_dir_all(&theme).unwrap();
        std::fs::write(theme.join("colors.toml"), "accent=\"#000006\"\n").unwrap();
        std::fs::write(theme.join("foot.ini"), "[colors-dark]\n").unwrap();

        let error = load_omarchy_theme_with(
            &theme,
            MissingOmarchyTheme::Reject,
            || std::fs::rename(&theme, &displaced).unwrap(),
            || {},
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("colors.toml"));
        assert!(load_live_omarchy_theme(&theme).is_err());
        assert_eq!(
            load_omarchy_theme(&theme).unwrap(),
            ResolvedTheme::default()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn theme_maps_every_omarchy_role_and_defaults_new_pane_roles() {
        let json = r##"{"background":"#000001","foreground":"#000003","cursor":"#000003","selection":"#000004","url":"#000005","ui_accent":"#000006","ansi":["#000000","#000001","#000002","#000003","#000004","#000005","#000006","#000007","#000008","#000009","#00000a","#00000b","#00000c","#00000d","#00000e","#00000f"]}"##;
        let theme: ThemePalette = serde_json::from_str(json).unwrap();
        let resolved = theme.resolve().unwrap();
        assert_eq!(resolved.background, 1);
        assert_eq!(resolved.selection_foreground, 3);
        assert_eq!(resolved.active_tab_background, resolved.ansi[8]);
        assert_eq!(resolved.active_tab_foreground, resolved.background);
        assert_ne!(resolved.active_tab_background, resolved.selection);
        assert_eq!(resolved.ui_accent(), 6);
        assert_eq!(resolved.pane_border(), 2);
        assert_eq!(resolved.pane_border_active(), 6);
        assert_eq!(resolved.background_alpha, u16::MAX);
        assert!(!resolved.background_blur);
        assert_eq!(resolved.ansi[15], 15);

        let explicit = json.replace(
            "\"ansi\"",
            "\"selection_foreground\":\"#000002\",\"active_tab_background\":\"#000009\",\"active_tab_foreground\":\"#00000a\",\"pane_border\":\"#000007\",\"pane_border_active\":\"#000008\",\"ansi\"",
        );
        let resolved = serde_json::from_str::<ThemePalette>(&explicit)
            .unwrap()
            .resolve()
            .unwrap();
        assert_eq!(resolved.selection_foreground, 2);
        assert_eq!(resolved.active_tab_background, 9);
        assert_eq!(resolved.active_tab_foreground, 10);
        assert_eq!(resolved.pane_border(), 7);
        assert_eq!(resolved.pane_border_active(), 8);

        let themed = json.replace(
            "\"ui_accent\"",
            "\"alpha\":0.85,\"blur\":true,\"ui_accent\"",
        );
        let resolved = serde_json::from_str::<ThemePalette>(&themed)
            .unwrap()
            .resolve()
            .unwrap();
        assert_eq!(resolved.background_alpha, foot_alpha(0.85));
        assert!(resolved.background_blur);
        let overridden = resolved.with_color_overrides(Some(foot_alpha(0.7)), Some(false));
        assert_eq!(overridden.background_alpha, foot_alpha(0.7));
        assert!(!overridden.background_blur);
        assert!(
            serde_json::from_str::<ThemePalette>(
                &json.replace("\"ui_accent\"", "\"alpha\":1.1,\"ui_accent\"")
            )
            .unwrap()
            .resolve()
            .is_err()
        );
        assert!(
            serde_json::from_str::<ThemePalette>(
                &json.replace("\"ui_accent\"", "\"blur\":\"yes\",\"ui_accent\"")
            )
            .is_err()
        );
    }

    #[test]
    fn active_tab_foreground_fallback_uses_the_higher_contrast_endpoint() {
        assert_eq!(
            active_tab_foreground_fallback(0x00_00_00, 0xff_ff_ff, 0xff_ff_ff),
            0x00_00_00
        );
        assert_eq!(
            active_tab_foreground_fallback(0x00_00_00, 0xff_ff_ff, 0x00_00_00),
            0xff_ff_ff
        );
        assert_eq!(
            active_tab_foreground_fallback(0x12_34_56, 0x12_34_56, 0x78_9a_bc),
            0x12_34_56
        );

        let json = r##"{"background":"#000000","foreground":"#ffffff","cursor":"#ffffff","selection":"#ffffff","url":"#0000ff","ui_accent":"#00ffff","ansi":["#000000","#000001","#000002","#000003","#000004","#000005","#000006","#000007","#000008","#000009","#00000a","#00000b","#00000c","#00000d","#00000e","#00000f"]}"##;
        let resolved = serde_json::from_str::<ThemePalette>(json)
            .unwrap()
            .resolve()
            .unwrap();
        assert_eq!(resolved.active_tab_background, resolved.ansi[8]);
        assert_eq!(resolved.active_tab_foreground, resolved.foreground);

        let different_selection =
            json.replace("\"selection\":\"#ffffff\"", "\"selection\":\"#000000\"");
        let resolved = serde_json::from_str::<ThemePalette>(&different_selection)
            .unwrap()
            .resolve()
            .unwrap();
        assert_eq!(resolved.active_tab_background, resolved.ansi[8]);
        assert_eq!(resolved.active_tab_foreground, resolved.foreground);
    }

    #[test]
    fn native_active_tab_foreground_is_derived_from_standard_omarchy_roles() {
        let foot = complete_foot_palette("141d23", "f2e8d5", "e6c93a", "b69f80");
        let dispatch =
            resolve_omarchy_theme("lighter_bg=\"#e6c93a\"\naccent=\"#00aaff\"\n", &foot).unwrap();
        assert_eq!(dispatch.active_tab_background, 0xe6_c9_3a);
        assert_eq!(dispatch.active_tab_foreground, 0x14_1d_23);
        assert!((contrast_ratio(0x14_1d_23, 0xe6_c9_3a) - 10.39).abs() < 0.01);
        assert_eq!(dispatch.selection_foreground, 0xb6_9f_80);

        let json = r##"{"background":"#000000","foreground":"#ffffff","cursor":"#ffffff","selection":"#ffffff","active_tab_foreground":"#141d23","url":"#0000ff","ui_accent":"#00ffff","ansi":["#000000","#000001","#000002","#000003","#000004","#000005","#000006","#000007","#000008","#000009","#00000a","#00000b","#00000c","#00000d","#00000e","#00000f"]}"##;
        for invalid in ["invalid", "141d23", "0x141d23"] {
            let invalid_json = json.replace("#141d23", invalid);
            assert!(
                serde_json::from_str::<ThemePalette>(&invalid_json)
                    .unwrap()
                    .resolve()
                    .is_err(),
                "accepted malformed JSON active_tab_foreground {invalid:?}"
            );
        }
    }

    fn inactive_contrast(theme: &ResolvedTheme) -> f64 {
        contrast_ratio(theme.inactive_tab_foreground(), theme.background)
    }

    #[test]
    fn default_inactive_tab_foreground_is_its_resolved_ansi_8() {
        let theme = ResolvedTheme::default();
        assert_eq!(theme.inactive_tab_foreground(), 0x92_83_74);
        assert_eq!(
            inactive_tab_foreground_fallback(theme.background, theme.foreground, theme.ansi[8]),
            theme.inactive_tab_foreground()
        );
        assert!((inactive_contrast(&theme) - 5.13).abs() < 0.01);
    }

    /// Plan 0064's text-muted without a hint: text mixed 30% toward the
    /// surface in linear light, kept at 4.5:1.
    fn mixed_muted(theme: &ResolvedTheme) -> u32 {
        use crate::color::{contrast_corrected, contrast_corrected_toward, mix_linear};
        contrast_corrected_toward(
            mix_linear(theme.foreground, theme.background, 0.30),
            contrast_corrected(theme.foreground, theme.background),
            theme.background,
        )
    }

    #[test]
    fn inactive_tab_foreground_keeps_a_readable_muted_color_or_mixes_toward_foreground() {
        // A muted color that meets the floor is used as is.
        let readable = inactive_tab_foreground_fallback(0x0e1216, 0xeb_eb_eb, 0x92_83_74);
        assert_eq!(readable, 0x92_83_74);

        // A faint muted color steps to the first fixed mix that meets the floor.
        let (background, foreground, muted) = (0x1e_1e_2e, 0xcd_d6_f4, 0x58_5b_70);
        assert!(contrast_ratio(muted, background) < INACTIVE_TAB_MIN_CONTRAST);
        let mixed = inactive_tab_foreground_fallback(background, foreground, muted);
        let first_passing = (1..4)
            .map(|quarters| mix_rgb_quarters(muted, foreground, quarters))
            .find(|candidate| contrast_ratio(*candidate, background) >= INACTIVE_TAB_MIN_CONTRAST)
            .unwrap();
        assert_eq!(mixed, first_passing);
        assert_ne!(mixed, foreground);
        let contrast = contrast_ratio(mixed, background);
        assert!(contrast >= INACTIVE_TAB_MIN_CONTRAST);
        assert!(contrast < contrast_ratio(foreground, background));

        // A light theme (Omarchy Friendly Wave) mixes its faint muted color
        // darker until it is readable, without exceeding the foreground.
        let (background, foreground, muted) = (0xff_fc_f0, 0x10_0f_0f, 0xb7_b5_ac);
        let light = inactive_tab_foreground_fallback(background, foreground, muted);
        assert_eq!(light, 0x64_62_5e);
        let contrast = contrast_ratio(light, background);
        assert!(contrast >= INACTIVE_TAB_MIN_CONTRAST);
        assert!(contrast < contrast_ratio(foreground, background));
        assert!((contrast - 5.92).abs() < 0.01);

        // A "muted" color louder than the foreground is never used or mixed in.
        assert_eq!(
            inactive_tab_foreground_fallback(0xff_ff_ff, 0x33_33_33, 0x00_00_00),
            0x33_33_33
        );
        // A foreground below the floor stays the foreground.
        assert_eq!(
            inactive_tab_foreground_fallback(0x00_00_00, 0x40_40_40, 0x30_30_30),
            0x40_40_40
        );
        // Mixes are exact quarters with rounding.
        assert_eq!(mix_rgb_quarters(0x00_00_00, 0xff_ff_ff, 0), 0x00_00_00);
        assert_eq!(mix_rgb_quarters(0x00_00_00, 0xff_ff_ff, 2), 0x80_80_80);
        assert_eq!(mix_rgb_quarters(0x00_00_00, 0xff_ff_ff, 4), 0xff_ff_ff);
    }

    #[test]
    fn json_inactive_tab_foreground_is_explicit_or_derived_from_ansi_8() {
        let json = r##"{"background":"#0e1216","foreground":"#ebebeb","cursor":"#ebebeb","selection":"#354a60","url":"#0000ff","ui_accent":"#00ffff","ansi":["#000000","#000001","#000002","#000003","#000004","#000005","#000006","#000007","#928374","#000009","#00000a","#00000b","#00000c","#00000d","#00000e","#00000f"]}"##;
        let derived = serde_json::from_str::<ThemePalette>(json)
            .unwrap()
            .resolve()
            .unwrap();
        // A JSON theme's ANSI 8 is its muted color.
        assert_eq!(derived.inactive_tab_foreground(), 0x92_83_74);

        // An explicit role is used verbatim, even below the floor.
        let explicit = json.replace(
            r#""url":"#,
            r##""inactive_tab_foreground":"#1a1e22","url":"##,
        );
        let explicit = serde_json::from_str::<ThemePalette>(&explicit)
            .unwrap()
            .resolve()
            .unwrap();
        assert_eq!(explicit.inactive_tab_foreground(), 0x1a_1e_22);
        assert!(inactive_contrast(&explicit) < INACTIVE_TAB_MIN_CONTRAST);

        for invalid in ["invalid", "1a1e22", "0x1a1e22"] {
            let invalid_json = json.replace(
                r#""url":"#,
                &format!(r#""inactive_tab_foreground":"{invalid}","url":"#),
            );
            assert!(
                serde_json::from_str::<ThemePalette>(&invalid_json)
                    .unwrap()
                    .resolve()
                    .is_err(),
                "accepted malformed JSON inactive_tab_foreground {invalid:?}"
            );
        }
    }

    #[test]
    fn native_inactive_tab_foreground_prefers_omarchy_muted_then_mixes_text() {
        let foot = complete_foot_palette("0e1216", "ebebeb", "354a60", "ebebeb")
            .replace("bright0=000008", "bright0=7c7e80");
        let muted = resolve_omarchy_theme("muted=\"#928374\"\n", &foot).unwrap();
        assert_eq!(muted.inactive_tab_foreground(), 0x92_83_74);
        let dark_foreground =
            resolve_omarchy_theme("dark_foreground=\"#928374\"\n", &foot).unwrap();
        assert_eq!(dark_foreground.inactive_tab_foreground(), 0x92_83_74);

        // Neither Foot bright0 nor color8 is a muted color: the inactive tab
        // mixes the text toward the surface.
        for colors in ["accent=\"#00aaff\"\n", "color8=\"#7c7e80\"\n"] {
            let mixed = resolve_omarchy_theme(colors, &foot).unwrap();
            assert_eq!(mixed.ansi[8], 0x7c_7e_80);
            assert_eq!(mixed.inactive_tab_foreground(), mixed_muted(&mixed));
            assert_eq!(mixed.inactive_tab_foreground(), 0xc9_c9_c9);
            assert!(inactive_contrast(&mixed) >= INACTIVE_TAB_MIN_CONTRAST);
        }
        let faint = complete_foot_palette("0e1216", "ebebeb", "354a60", "ebebeb");
        let faint = resolve_omarchy_theme("", &faint).unwrap();
        assert_eq!(faint.inactive_tab_foreground(), mixed_muted(&faint));
        assert!(inactive_contrast(&faint) >= INACTIVE_TAB_MIN_CONTRAST);

        assert!(resolve_omarchy_theme("muted=\"nope\"\n", &foot).is_err());
    }

    fn shell_layer<'a>(path: &'a str, contents: &'a str) -> OmarchyShellLayer<'a> {
        OmarchyShellLayer {
            path: Path::new(path),
            contents: Some(contents),
        }
    }

    #[test]
    fn omarchy_shell_parser_mirrors_the_shell_line_grammar() {
        let values = omarchy_shell_values(
            "top = 1\n# comment\n[menu] # trailing comment\nbackground-alpha = 0.9 # note\ntext = \"#ffffff\" # quoted hash\nborder='hyprland.active-border'\nnot a key\n[bad section]\nbackground-alpha = 0.1\n[popups]\nbackground-alpha=0.5\nbackground-alpha=0.6\n",
        );
        // The malformed header keeps [menu], so its key replaces the earlier one.
        assert_eq!(values.get("menu.background-alpha").unwrap(), "0.1");
        assert_eq!(values.get("menu.text").unwrap(), "#ffffff");
        assert_eq!(values.get("menu.border").unwrap(), "hyprland.active-border");
        // Keys outside a section are ignored, and a malformed header keeps the
        // previous section, as in the shell's parseShell.
        assert!(!values.contains_key(".top") && !values.contains_key("top"));
        assert_eq!(values.get("popups.background-alpha").unwrap(), "0.6");
        assert_eq!(values.len(), 4);
    }

    #[test]
    fn omarchy_shell_quotes_follow_the_shell_string_pattern() {
        // parseShell's string pattern is ["']([^"']+)["']: either quote opens
        // and either closes, the value holds no quote, and it is not empty.
        // Omarchy therefore reads a mismatched pair as the quoted value, and
        // Splinterm does the same rather than dropping a line the shell keeps.
        let values = omarchy_shell_values(
            "[menu]\nbackground-alpha = \"0.5'\n[font]\nbase-size = '16\"\n[popups]\nbackground-alpha = \"\"\ntext = \"a\"b\"\nborder = \"open\n",
        );
        assert_eq!(values.get("menu.background-alpha").unwrap(), "0.5");
        assert_eq!(values.get("font.base-size").unwrap(), "16");
        // Empty, interrupted and unterminated strings match no shell pattern,
        // so their lines are dropped.
        assert!(!values.contains_key("popups.background-alpha"));
        assert!(!values.contains_key("popups.text"));
        assert!(!values.contains_key("popups.border"));
        assert_eq!(values.len(), 2);
    }

    #[test]
    fn omarchy_shell_layers_user_keys_over_the_theme_with_omarchy_defaults() {
        let template =
            include_str!("../tests/fixtures/omarchy4/shell/rendered-template.shell.toml");
        // The rendered stock template: opaque menus and popups, base-size 12.
        let stock = resolve_omarchy_shell(
            shell_layer("theme/shell.toml", template),
            None,
            OmarchyTextSizeFallback::OmarchyDefault,
        );
        assert!(stock.diagnostics.is_empty(), "{:?}", stock.diagnostics);
        assert_eq!(
            stock.settings,
            OmarchyShellSettings {
                menu_alpha: u16::MAX,
                popup_alpha: u16::MAX,
                text_size_px: Some(12),
                ui_text_size_px: Some(12),
                bar_size_px: 26,
                text_size_held: false,
            }
        );

        // A missing theme file and no user file give Omarchy's defaults and no
        // text size, so Splinterm keeps its own default size.
        let missing = resolve_omarchy_shell(
            OmarchyShellLayer {
                path: Path::new("theme/shell.toml"),
                contents: None,
            },
            None,
            OmarchyTextSizeFallback::Current(Some(14)),
        );
        assert_eq!(missing, OmarchyShellLoad::default());

        // omarchy-display-text-size writes only [font] base-size; every other
        // key still comes from the theme.
        let theme = "[menu]\nbackground-alpha = 0.8\n[popups]\nbackground-alpha = 0.9\n[font]\nbase-size = 12\n";
        let layered = resolve_omarchy_shell(
            shell_layer("theme/shell.toml", theme),
            Some(shell_layer("user/shell.toml", "[font]\nbase-size = 16\n")),
            OmarchyTextSizeFallback::OmarchyDefault,
        );
        assert_eq!(layered.diagnostics, Vec::new());
        assert_eq!(layered.settings.menu_alpha, foot_alpha(0.8));
        assert_eq!(layered.settings.popup_alpha, foot_alpha(0.9));
        assert_eq!(layered.settings.text_size_px, Some(16));

        let user_alpha = resolve_omarchy_shell(
            shell_layer("theme/shell.toml", theme),
            Some(shell_layer(
                "user/shell.toml",
                "[menu]\nbackground-alpha = 0.5\n",
            )),
            OmarchyTextSizeFallback::OmarchyDefault,
        );
        assert_eq!(user_alpha.settings.menu_alpha, foot_alpha(0.5));
        assert_eq!(user_alpha.settings.popup_alpha, foot_alpha(0.9));
        assert_eq!(user_alpha.settings.text_size_px, Some(12));

        // A theme base-size applies when the user sets none.
        let theme_size = resolve_omarchy_shell(
            shell_layer("theme/shell.toml", "[font]\nbase-size = 14\n"),
            Some(OmarchyShellLayer {
                path: Path::new("user/shell.toml"),
                contents: None,
            }),
            OmarchyTextSizeFallback::OmarchyDefault,
        );
        assert_eq!(theme_size.settings.text_size_px, Some(14));
    }

    #[test]
    fn omarchy_shell_rejects_invalid_values_with_diagnostics() {
        let user_alpha = |raw: &str| {
            resolve_omarchy_shell(
                shell_layer("theme/shell.toml", "[popups]\nbackground-alpha = 0.7\n"),
                Some(shell_layer(
                    "user/shell.toml",
                    &format!("[popups]\nbackground-alpha = {raw}\n"),
                )),
                OmarchyTextSizeFallback::OmarchyDefault,
            )
        };
        // Bare values outside parseShell's number, width-list and word shapes
        // drop the line, so the theme's value still applies, silently.
        for raw in [".5", "1.", "+0.5", "1e-1", "0.5x", "-", "1 2 3 4 5"] {
            let load = user_alpha(raw);
            assert_eq!(load.settings.popup_alpha, foot_alpha(0.7), "{raw}");
            assert!(load.diagnostics.is_empty(), "{raw}: {:?}", load.diagnostics);
        }
        // Numbers are clamped to 0..1 like the shell's pickAlpha, and reported.
        for (raw, alpha, diagnostics) in [
            ("-0.2", 0.0, 1),
            ("1.5", 1.0, 1),
            ("\"1e-1\"", 0.1, 0),
            ("\".5\"", 0.5, 0),
        ] {
            let load = user_alpha(raw);
            assert_eq!(load.settings.popup_alpha, foot_alpha(alpha), "{raw}");
            assert_eq!(load.diagnostics.len(), diagnostics, "{raw}");
        }
        // Stored values that are not numbers fall back to the opaque default.
        for raw in ["opaque", "\"0.5x\"", "\"inf\"", "0.5 0.5"] {
            let load = user_alpha(raw);
            assert_eq!(load.settings.popup_alpha, u16::MAX, "{raw}");
            assert_eq!(load.diagnostics.len(), 1, "{raw}");
            assert_eq!(load.diagnostics[0].0, OmarchyShellConcern::PanelAlpha);
            assert!(
                load.diagnostics[0]
                    .1
                    .starts_with("user/shell.toml: [popups]")
            );
        }

        // base-size follows the shell's parseInt and its 1 px floor; only the
        // rendered point size is bounded, and that is reported.
        let user_size = |raw: &str, fallback: OmarchyTextSizeFallback| {
            resolve_omarchy_shell(
                shell_layer("theme/shell.toml", "[font]\nbase-size = 14\n"),
                Some(shell_layer(
                    "user/shell.toml",
                    &format!("[font]\nbase-size = {raw}\n"),
                )),
                fallback,
            )
        };
        for (raw, px, diagnostics) in [
            ("12.0", 12, 0),
            ("\"12px\"", 12, 0),
            ("\"+16\"", 16, 0),
            ("8", 8, 0),
            ("21", 21, 0),
            ("120", 120, 0),
            ("2", 2, 1),
            ("0", 1, 1),
            ("-12", 1, 1),
            ("200", 200, 1),
            ("1 2", 1, 1),
        ] {
            let load = user_size(raw, OmarchyTextSizeFallback::Current(Some(13)));
            assert_eq!(load.settings.text_size_px, Some(px), "{raw}");
            assert_eq!(load.diagnostics.len(), diagnostics, "{raw}");
        }
        // Dropped lines leave the theme's base-size in place.
        for raw in ["+12", "1e1", ".5"] {
            let load = user_size(raw, OmarchyTextSizeFallback::Current(Some(13)));
            assert_eq!(load.settings.text_size_px, Some(14), "{raw}");
            assert!(load.diagnostics.is_empty(), "{raw}");
        }
        // A stored value without digits keeps the size in use: the previous
        // shell size, Splinterm's own fallback when a running window followed
        // none, and Omarchy's 12 px default at startup.
        for (fallback, px) in [
            (OmarchyTextSizeFallback::Current(Some(13)), Some(13)),
            (OmarchyTextSizeFallback::Current(None), None),
            (OmarchyTextSizeFallback::OmarchyDefault, Some(12)),
        ] {
            let load = user_size("big", fallback);
            assert_eq!(load.settings.text_size_px, px);
            assert_eq!(
                load.settings.text_size_held,
                matches!(fallback, OmarchyTextSizeFallback::Current(_))
            );
            assert_eq!(load.diagnostics.len(), 1);
            assert_eq!(load.diagnostics[0].0, OmarchyShellConcern::TextSize);
        }
    }

    #[test]
    fn omarchy_bar_size_layers_clamps_and_falls_back_to_the_stock_bar() {
        // Omarchy's default and the rendered template both give a 26 px bar.
        assert_eq!(OmarchyShellSettings::default().bar_size_px, 26);
        let template =
            include_str!("../tests/fixtures/omarchy4/shell/rendered-template.shell.toml");
        let stock = resolve_omarchy_shell(
            shell_layer("theme/shell.toml", template),
            None,
            OmarchyTextSizeFallback::OmarchyDefault,
        );
        assert_eq!(stock.settings.bar_size_px, 26);
        assert!(stock.diagnostics.is_empty(), "{:?}", stock.diagnostics);

        let user_bar = |raw: &str| {
            resolve_omarchy_shell(
                shell_layer("theme/shell.toml", "[bar]\nsize-horizontal = 30\n"),
                Some(shell_layer(
                    "user/shell.toml",
                    &format!("[bar]\nsize-horizontal = {raw}\n"),
                )),
                OmarchyTextSizeFallback::OmarchyDefault,
            )
        };
        // The user key replaces the theme's; the theme's applies otherwise.
        assert_eq!(user_bar("32").settings.bar_size_px, 32);
        let theme_only = resolve_omarchy_shell(
            shell_layer("theme/shell.toml", "[bar]\nsize-horizontal = 30\n"),
            Some(shell_layer("user/shell.toml", "[font]\nbase-size = 14\n")),
            OmarchyTextSizeFallback::OmarchyDefault,
        );
        // The theme's 30 px, scaled by the user's base-size: round(30 × 14/12).
        assert_eq!(theme_only.settings.bar_size_px, 35);
        // `size-vertical` sizes side bars, not the top bar.
        let vertical = resolve_omarchy_shell(
            shell_layer("theme/shell.toml", "[bar]\nsize-vertical = 40\n"),
            None,
            OmarchyTextSizeFallback::OmarchyDefault,
        );
        assert_eq!(vertical.settings.bar_size_px, 26);

        // The shell reads the size with parseInt; the result clamps to
        // 20..=48, reported when outside.
        for (raw, px, diagnostics) in [
            ("20", 20, 0),
            ("48", 48, 0),
            ("26.7", 26, 0),
            ("\"24\"", 24, 0),
            ("\"+28\"", 28, 0),
            ("\"26px\"", 26, 0),
            ("26 28", 26, 0),
            ("\"1e2\"", 20, 1),
            ("19", 20, 1),
            ("49", 48, 1),
            ("200", 48, 1),
        ] {
            let load = user_bar(raw);
            assert_eq!(load.settings.bar_size_px, px, "{raw}");
            assert_eq!(load.diagnostics.len(), diagnostics, "{raw}");
            if diagnostics > 0 {
                assert_eq!(load.diagnostics[0].0, OmarchyShellConcern::BarSize);
            }
        }
        // A value without digits, or one that is not positive, uses the shell's
        // 26 px default even over a theme value, as alpha falls back to
        // Omarchy's default.
        for raw in ["tall", "\"inf\"", "0", "-30"] {
            let load = user_bar(raw);
            assert_eq!(load.settings.bar_size_px, 26, "{raw}");
            assert_eq!(load.diagnostics.len(), 1, "{raw}");
            assert_eq!(load.diagnostics[0].0, OmarchyShellConcern::BarSize);
            assert!(
                load.diagnostics[0]
                    .1
                    .starts_with("user/shell.toml: [bar] size-horizontal"),
                "{:?}",
                load.diagnostics
            );
        }
        // Lines the shell's parser drops leave the theme's value in place,
        // silently, as for every other key.
        for raw in ["+28", ".5", "1e2"] {
            let load = user_bar(raw);
            assert_eq!(load.settings.bar_size_px, 30, "{raw}");
            assert!(load.diagnostics.is_empty(), "{raw}");
        }
    }

    #[test]
    fn omarchy_bar_size_scales_with_the_font_base_size_like_the_shell() {
        let bar = |theme: &str, user: &str| {
            resolve_omarchy_shell(
                shell_layer("theme/shell.toml", theme),
                Some(shell_layer("user/shell.toml", user)),
                OmarchyTextSizeFallback::OmarchyDefault,
            )
        };
        // round(26 × base-size / 12): the stock bar at base-size 12, 9, 16, 20.
        for (base, px) in [(12, 26), (9, 20), (16, 35), (20, 43), (15, 33)] {
            let load = bar(
                "[bar]\nscale-with-font = true\nsize-horizontal = 26\n",
                &format!("[font]\nbase-size = {base}\n"),
            );
            assert_eq!(load.settings.bar_size_px, px, "base-size {base}");
            assert!(load.diagnostics.is_empty(), "{:?}", load.diagnostics);
        }
        // Scaling is on when no layer sets scale-with-font, and with no
        // size-horizontal the 26 px default scales.
        assert_eq!(bar("", "[font]\nbase-size = 16\n").settings.bar_size_px, 35);
        assert_eq!(
            bar("[bar]\nsize-horizontal = 30\n", "[font]\nbase-size = 16\n")
                .settings
                .bar_size_px,
            40
        );
        // The clamp applies after scaling. A bar that base-size pushes out of
        // range clamps silently, even with the template's size-horizontal =
        // 26 in the theme layer as on every stock Omarchy desktop.
        let large = bar("[bar]\nsize-horizontal = 26\n", "[font]\nbase-size = 24\n");
        assert_eq!(large.settings.bar_size_px, 48);
        assert!(large.diagnostics.is_empty(), "{:?}", large.diagnostics);
        // Here only the 6 px text size's own point-size diagnostic remains.
        for theme in ["", "[bar]\nsize-horizontal = 26\n"] {
            let small = bar(theme, "[font]\nbase-size = 6\n");
            assert_eq!(small.settings.bar_size_px, 20);
            assert_eq!(small.diagnostics.len(), 1, "{:?}", small.diagnostics);
            assert_eq!(
                small.diagnostics[0].0,
                OmarchyShellConcern::TerminalTextSize
            );
        }
        // A configured size outside the limits is reported when it is
        // clamped, against the layer that set it...
        let huge = bar("[bar]\nsize-horizontal = 60\n", "[font]\nbase-size = 12\n");
        assert_eq!(huge.settings.bar_size_px, 48);
        assert_eq!(huge.diagnostics.len(), 1, "{:?}", huge.diagnostics);
        assert!(
            huge.diagnostics[0]
                .1
                .starts_with("theme/shell.toml: [bar] size-horizontal \"60\" is outside"),
            "{:?}",
            huge.diagnostics
        );
        // ...but not when base-size scales it back into range.
        let rescaled = bar("[bar]\nsize-horizontal = 60\n", "[font]\nbase-size = 8\n");
        assert_eq!(rescaled.settings.bar_size_px, 40);
        assert!(
            rescaled.diagnostics.is_empty(),
            "{:?}",
            rescaled.diagnostics
        );
        // base-size follows parseInt with the shell's 1 px floor; one without
        // digits leaves the 12 px scale, and only its text-size diagnostic.
        assert_eq!(
            bar("", "[font]\nbase-size = \"16px\"\n")
                .settings
                .bar_size_px,
            35
        );
        let unscaled = bar("", "[font]\nbase-size = big\n");
        assert_eq!(unscaled.settings.bar_size_px, 26);
        assert_eq!(unscaled.diagnostics.len(), 1);
        assert_eq!(unscaled.diagnostics[0].0, OmarchyShellConcern::TextSize);

        // boolToken: trimmed, any case; anything else keeps the default, true.
        for raw in ["false", "FALSE", "0", "no", "Off", "\" off \""] {
            let load = bar(
                &format!("[bar]\nscale-with-font = {raw}\nsize-horizontal = 26\n"),
                "[font]\nbase-size = 16\n",
            );
            assert_eq!(load.settings.bar_size_px, 26, "{raw}");
            assert!(load.diagnostics.is_empty(), "{raw}");
        }
        for raw in ["true", "TRUE", "1", "yes", "On", "\" YES \""] {
            let load = bar(
                &format!("[bar]\nscale-with-font = {raw}\n"),
                "[font]\nbase-size = 16\n",
            );
            assert_eq!(load.settings.bar_size_px, 35, "{raw}");
            assert!(load.diagnostics.is_empty(), "{raw}");
        }
        for raw in ["maybe", "2", "\"\u{a0}\""] {
            let load = bar(
                &format!("[bar]\nscale-with-font = {raw}\n"),
                "[font]\nbase-size = 16\n",
            );
            assert_eq!(load.settings.bar_size_px, 35, "{raw}");
            assert_eq!(load.diagnostics.len(), 1, "{raw}");
            assert_eq!(load.diagnostics[0].0, OmarchyShellConcern::BarSize);
        }
        // The user's scale-with-font replaces the theme's, key by key.
        let user_off = bar(
            "[bar]\nscale-with-font = true\n",
            "[bar]\nscale-with-font = off\n[font]\nbase-size = 20\n",
        );
        assert_eq!(user_off.settings.bar_size_px, 26);
    }

    #[test]
    fn omarchy_text_size_points_are_bounded_to_the_renderer_range() {
        for (px, points) in [(1, 6.0), (8, 6.0), (9, 7.0), (120, 90.0), (128, 96.0)] {
            assert_eq!(
                omarchy_terminal_point_size(px).to_bits(),
                f32::to_bits(points),
                "{px} px"
            );
        }
        assert_eq!(
            omarchy_terminal_point_size(u16::MAX).to_bits(),
            96.0_f32.to_bits()
        );
    }

    #[test]
    fn omarchy_shell_files_load_from_the_theme_dir_and_user_override() {
        let root =
            std::env::temp_dir().join(format!("splinterm-omarchy-shell-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let theme = root.join("theme");
        let user = root.join("user/shell.toml");
        fs::create_dir_all(&theme).unwrap();
        assert_eq!(
            load_omarchy_shell(&theme, Some(&user), OmarchyTextSizeFallback::OmarchyDefault),
            OmarchyShellLoad::default()
        );
        fs::write(theme.join("shell.toml"), "[font]\nbase-size = 12\n").unwrap();
        fs::create_dir_all(user.parent().unwrap()).unwrap();
        fs::write(&user, "[font]\nbase-size = 18\n").unwrap();
        assert_eq!(
            load_omarchy_shell(&theme, Some(&user), OmarchyTextSizeFallback::OmarchyDefault)
                .settings
                .text_size_px,
            Some(18)
        );
        assert_eq!(
            load_omarchy_shell(&theme, None, OmarchyTextSizeFallback::OmarchyDefault)
                .settings
                .text_size_px,
            Some(12)
        );
        // An unreadable layer is reported and treated as missing.
        fs::remove_file(&user).unwrap();
        fs::create_dir(&user).unwrap();
        let unreadable =
            load_omarchy_shell(&theme, Some(&user), OmarchyTextSizeFallback::OmarchyDefault);
        assert_eq!(unreadable.settings.text_size_px, Some(12));
        assert_eq!(unreadable.diagnostics.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn omarchy_theme_roots_layer_the_hard_coded_home_shell_override() {
        let roots = OmarchyThemeRoots::from_values(
            Some(OsString::from("/srv/state")),
            Some(OsString::from("/home/test")),
        );
        assert_eq!(
            roots.user_shell(),
            Some(Path::new("/home/test/.config/omarchy/shell.toml"))
        );
        assert_eq!(
            OmarchyThemeRoots::fixed(PathBuf::from("/theme")).user_shell(),
            None
        );
    }

    #[test]
    fn omarchy_text_size_maps_px_to_omarchy_terminal_points() {
        // omarchy-display-text-size: int(px * 9 / 12 + 0.5).
        for (px, points) in [
            (9, 7.0),
            (10, 8.0),
            (11, 8.0),
            (12, 9.0),
            (13, 10.0),
            (14, 11.0),
            (15, 11.0),
            (16, 12.0),
            (18, 14.0),
            (20, 15.0),
        ] {
            assert_eq!(
                omarchy_terminal_point_size(px).to_bits(),
                f32::to_bits(points),
                "{px} px"
            );
        }
    }

    #[test]
    fn only_an_unset_font_size_follows_the_omarchy_text_size() {
        assert!(AppConfig::default().follows_omarchy_text_size());
        let shipped = parse(include_str!("../../../config/splinterm/config.ini")).unwrap();
        assert!(shipped.config.follows_omarchy_text_size());
        assert!(
            parse("[main]\nfont=monospace\n")
                .unwrap()
                .config
                .follows_omarchy_text_size()
        );
        for key in ["font-pixelsize=14", "font-point-size=10", "font-size=14"] {
            let config = parse(&format!("[main]\n{key}\n")).unwrap().config;
            assert_eq!(config.font_size_authority, FontAuthority::Explicit, "{key}");
            assert!(!config.follows_omarchy_text_size(), "{key}");
        }
    }

    #[test]
    fn ui_font_pixelsize_is_its_own_key_independent_of_the_terminal_size() {
        assert_eq!(AppConfig::default().ui_font_size, None);
        let shipped = parse(include_str!("../../../config/splinterm/config.ini")).unwrap();
        assert_eq!(shipped.config.ui_font_size, None);
        let both = parse("[main]\nfont-pixelsize=20\nui-font-pixelsize=11.5\n").unwrap();
        assert!(both.diagnostics.is_empty(), "{:?}", both.diagnostics);
        assert_eq!(both.config.ui_font_size, Some(FontSize::Pixels(11.5)));
        assert_eq!(both.config.font_size, FontSize::Pixels(20.0));
        // The UI size never stops the terminal following Omarchy.
        let ui_only = parse("[main]\nui-font-pixelsize=16\n").unwrap().config;
        assert!(ui_only.follows_omarchy_text_size());
        for bounds in ["6", "96"] {
            assert!(parse(&format!("[main]\nui-font-pixelsize={bounds}\n")).is_ok());
        }
        for invalid in ["5.9", "97", "big", "", "nan", "NaN", "inf", "-inf"] {
            assert!(
                parse(&format!("[main]\nui-font-pixelsize={invalid}\n")).is_err(),
                "{invalid:?}"
            );
        }
        // The same range check guards the terminal's float size keys.
        for key in ["font-pixelsize", "font-point-size", "font-size"] {
            for invalid in ["nan", "inf"] {
                assert!(
                    parse(&format!("[main]\n{key}={invalid}\n")).is_err(),
                    "{key}={invalid}"
                );
            }
        }
        // Only [main] owns it.
        let elsewhere = parse("[explorer]\nui-font-pixelsize=16\n").unwrap();
        assert_eq!(elsewhere.config.ui_font_size, None);
        assert_eq!(elsewhere.diagnostics.len(), 1);
    }

    #[test]
    fn ui_font_size_resolves_key_then_omarchy_then_the_default() {
        let key = Some(FontSize::Pixels(17.0));
        assert_eq!(resolve_ui_font_size(key, Some(12)), FontSize::Pixels(17.0));
        assert_eq!(resolve_ui_font_size(key, None), FontSize::Pixels(17.0));
        // Omarchy's text size is the px its own shell draws at, not the
        // terminal's rounded point size.
        for px in 9..=20 {
            assert_eq!(
                resolve_ui_font_size(None, Some(px)),
                FontSize::Pixels(f32::from(px))
            );
        }
        assert_eq!(resolve_ui_font_size(None, Some(1)), FontSize::Pixels(6.0));
        assert_eq!(
            resolve_ui_font_size(None, Some(500)),
            FontSize::Pixels(96.0)
        );
        assert_eq!(
            resolve_ui_font_size(None, None),
            FontSize::Pixels(DEFAULT_UI_FONT_PIXELSIZE)
        );
        assert_eq!(DEFAULT_UI_FONT_PIXELSIZE.to_bits(), 14.0_f32.to_bits());
    }

    /// Shaped like a theme that names semantic colors; see its `colors.toml`.
    const SEMANTIC_KEYS_FIXTURE: (&str, &str) = (
        include_str!("../tests/fixtures/omarchy4/semantic-keys/colors.toml"),
        include_str!("../tests/fixtures/omarchy4/semantic-keys/foot.ini"),
    );

    /// A JSON theme naming only its required keys, and one naming every
    /// optional key as well.
    const JSON_THEME: &str = r##"{"background":"#101820","foreground":"#e0e4ea","cursor":"#f0f0f0","selection":"#2a3a4a","url":"#5599ff","ui_accent":"#33bbee","ansi":["#101820","#cc3344","#44aa55","#ccaa33","#3377cc","#aa55bb","#33aaaa","#c0c4ca","#56606a","#ee5566","#66cc77","#eecc55","#5599ee","#cc77dd","#55cccc","#f0f4fa"]}"##;
    const JSON_THEME_OPTIONAL_KEYS: &str = "\"selection_foreground\":\"#fafafa\",\"active_tab_background\":\"#203040\",\"active_tab_foreground\":\"#ffeedd\",\"inactive_tab_foreground\":\"#8899aa\",\"pane_border\":\"#445566\",\"pane_border_active\":\"#ddaa33\",\"alpha\":0.8,\"blur\":true,\"ansi\"";

    fn json_theme(json: &str) -> ResolvedTheme {
        serde_json::from_str::<ThemePalette>(json)
            .unwrap()
            .resolve()
            .unwrap()
    }

    /// Every theme source the pins below cover: built-in, JSON, and the
    /// stock Omarchy 4 and semantic-key fixtures.
    fn provider_themes() -> Vec<(&'static str, ResolvedTheme)> {
        let mut themes = vec![
            ("builtin", ResolvedTheme::default()),
            ("json", json_theme(JSON_THEME)),
            (
                "json-explicit",
                json_theme(&JSON_THEME.replace("\"ansi\"", JSON_THEME_OPTIONAL_KEYS)),
            ),
        ];
        for (name, colors, foot) in OMARCHY4_FIXTURES {
            themes.push((name, resolve_omarchy_theme(colors, foot).unwrap()));
        }
        let (colors, foot) = SEMANTIC_KEYS_FIXTURE;
        themes.push((
            "semantic-keys",
            resolve_omarchy_theme(colors, foot).unwrap(),
        ));
        themes
    }

    /// The colors each source paints: background, foreground, cursor,
    /// selection, selection text, URL, selected tab body and label, inactive
    /// tab label, accent, pane border, active pane border, then alpha and
    /// blur. The rows were recorded from the resolver before chrome roles
    /// existed (Plan 0064 milestone 2), except for the `semantic-keys`
    /// fixture, new with roles, and three colors milestone 3 changed on
    /// purpose: `alacritty-kanagawa`'s inactive tab label (was 8d8b7d),
    /// which names no `muted` or `dark_foreground` and now mixes its text;
    /// `last-horizon`'s active pane border (b59790), now its
    /// `active_border_color`; and `semantic-keys`' pane border (6a6488),
    /// now its `border`.
    #[allow(clippy::type_complexity)]
    const PROVIDER_PINS: [(&str, [u32; 12], u16, bool); 9] = [
        (
            "builtin",
            [
                0x0e1216, 0xebebeb, 0xebebeb, 0x354a60, 0xebebeb, 0x78beff, 0x928374, 0x0e1216,
                0x928374, 0x78d2ff, 0x7c7e80, 0x78d2ff,
            ],
            u16::MAX,
            false,
        ),
        (
            "json",
            [
                0x101820, 0xe0e4ea, 0xf0f0f0, 0x2a3a4a, 0xe0e4ea, 0x5599ff, 0x56606a, 0xe0e4ea,
                0x79818a, 0x33bbee, 0x787e85, 0x33bbee,
            ],
            u16::MAX,
            false,
        ),
        (
            "json-explicit",
            [
                0x101820, 0xe0e4ea, 0xf0f0f0, 0x2a3a4a, 0xfafafa, 0x5599ff, 0x203040, 0xffeedd,
                0x8899aa, 0x33bbee, 0x445566, 0xddaa33,
            ],
            52428,
            true,
        ),
        (
            "tokyo-night",
            [
                0x1a1b26, 0xa9b1d6, 0xc0caf5, 0x292e42, 0xc0caf5, 0x7aa2f7, 0x24283b, 0xa9b1d6,
                0x8f97bb, 0x7aa2f7, 0x414868, 0x7aa2f7,
            ],
            u16::MAX,
            true,
        ),
        (
            "flexoki-light",
            [
                0xfffcf0, 0x100f0f, 0x100f0f, 0xcecdc3, 0x100f0f, 0x205ea6, 0xe6e4d9, 0x100f0f,
                0x64625e, 0x205ea6, 0xb7b5ac, 0x205ea6,
            ],
            u16::MAX,
            true,
        ),
        (
            "last-horizon",
            [
                0x0c0b0c, 0xfafcfb, 0xe2dddc, 0x584e51, 0xe2dddc, 0xb59790, 0x0c0b0c, 0xfafcfb,
                0x817a7c, 0xb59790, 0x584e51, 0xd6d3de,
            ],
            u16::MAX,
            true,
        ),
        (
            "legacy-gruvbox",
            [
                0x282828, 0xd4be98, 0xd4be98, 0x504945, 0xd4be98, 0x7daea3, 0x3c3836, 0xd4be98,
                0x9d8d76, 0x7daea3, 0x665c54, 0x7daea3,
            ],
            u16::MAX,
            true,
        ),
        (
            "alacritty-kanagawa",
            [
                0x1f1f28, 0xdcd7ba, 0xdcd7ba, 0x2d4f67, 0xdcd7ba, 0x7e9cd8, 0x727169, 0xdcd7ba,
                0xbcb8a0, 0x7e9cd8, 0x727169, 0x7e9cd8,
            ],
            u16::MAX,
            true,
        ),
        (
            "semantic-keys",
            [
                0x14111f, 0xd8d4ea, 0xf2efff, 0x2a2540, 0xf2efff, 0x5ec4ff, 0x1f1a30, 0xd8d4ea,
                0x8680a1, 0x5ec4ff, 0x3d3654, 0x5ec4ff,
            ],
            u16::MAX,
            true,
        ),
    ];

    #[test]
    fn provider_themes_paint_their_pinned_colors() {
        let themes = provider_themes();
        assert_eq!(themes.len(), PROVIDER_PINS.len());
        for ((name, theme), (pinned, colors, alpha, blur)) in themes.into_iter().zip(PROVIDER_PINS)
        {
            assert_eq!(name, pinned);
            assert_eq!(
                [
                    theme.background,
                    theme.foreground,
                    theme.cursor,
                    theme.selection,
                    theme.selection_foreground,
                    theme.url,
                    theme.active_tab_background,
                    theme.active_tab_foreground,
                    theme.roles.text_muted,
                    theme.roles.accent,
                    theme.roles.divider,
                    theme.roles.focus_ring,
                ],
                colors,
                "{name}"
            );
            assert_eq!(
                (theme.background_alpha, theme.background_blur),
                (alpha, blur),
                "{name}"
            );
        }
    }

    #[test]
    fn builtin_theme_keeps_its_exact_palette() {
        let theme = ResolvedTheme::default();
        assert_eq!(
            (
                theme.background,
                theme.foreground,
                theme.cursor,
                theme.selection,
                theme.selection_foreground,
                theme.url,
            ),
            (0x0e1216, 0xebebeb, 0xebebeb, 0x354a60, 0xebebeb, 0x78beff)
        );
        assert_eq!(
            theme.ansi,
            [
                0x1d2021, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0xa89984,
                0x928374, 0xfb4934, 0xb8bb26, 0xfabd2f, 0x83a598, 0xd3869b, 0x8ec07c, 0xebdbb2,
            ]
        );
        assert_eq!(theme.background_alpha, u16::MAX);
        assert!(!theme.background_blur);
        assert_eq!(theme.chrome, ChromeBackgrounds::default());
        assert_eq!(theme.shell, OmarchyShellSettings::default());
        assert_eq!(theme.roles, ChromeRoles::default());
    }

    #[test]
    fn chrome_color_accessors_alias_their_roles() {
        let mut themes = provider_themes();
        let overridden: Vec<_> = themes
            .iter()
            .map(|(name, theme)| {
                let theme = theme.with_chrome_overrides(ChromeColorOverrides {
                    active_tab_background: Some(0xf0_e0_d0),
                    ..ChromeColorOverrides::NONE
                });
                (
                    *name,
                    theme.with_color_overrides(Some(foot_alpha(0.5)), Some(true)),
                )
            })
            .collect();
        themes.extend(overridden);
        for (name, theme) in themes {
            let roles = theme.roles;
            assert_eq!(theme.ui_accent(), roles.accent, "{name}");
            assert_eq!(theme.pane_border(), roles.divider, "{name}");
            assert_eq!(theme.pane_border_active(), roles.focus_ring, "{name}");
            assert_eq!(theme.inactive_tab_foreground(), roles.text_muted, "{name}");
            assert_eq!(theme.explorer_background(), roles.surface, "{name}");
            // Chrome's surface, text and selection are still the terminal's.
            assert_eq!(roles.surface, theme.background, "{name}");
            assert_eq!(roles.text, theme.foreground, "{name}");
            assert_eq!(roles.selection, theme.selection, "{name}");
        }
    }

    #[test]
    fn omarchy_semantic_keys_change_their_roles() {
        let (colors, foot) = SEMANTIC_KEYS_FIXTURE;
        let theme = resolve_omarchy_theme(colors, foot).unwrap();
        let roles = theme.roles;
        // The theme puts ultraviolet in its ANSI yellow slot on purpose:
        // `warning` wins over both `yellow` and ANSI 3.
        assert_eq!(theme.ansi[3], 0x9b_6b_ff);
        assert_eq!(roles.warning, 0xff_b3_47);
        assert_eq!(roles.status_working, roles.warning);
        assert_eq!(roles.success, 0x5f_d7_8f);
        assert_eq!(roles.remote_marker, 0xff_5f_d2);
        assert_eq!(roles.divider, 0x3d_36_54);
        assert_eq!(theme.pane_border(), 0x3d_36_54);
        assert_eq!(roles.surface_sunken, 0x0d_0b_15);
        assert_eq!(roles.surface_raised, 0x1f_1a_30);
        assert_eq!(roles.selection, 0x2a_25_40);
        assert_eq!(roles.danger, 0xff_5f_6d);
        assert_eq!(roles.info, 0x4f_d1_c5);
        assert_eq!(theme.appearance, Appearance::Dark);
        // Without its semantic keys the named hues, then muted, take over.
        let without = colors
            .lines()
            .filter(|line| {
                !["warning", "signal", "border"]
                    .iter()
                    .any(|key| line.starts_with(&format!("{key} = ")))
            })
            .collect::<Vec<_>>()
            .join("\n");
        let hues = resolve_omarchy_theme(&without, foot).unwrap().roles;
        assert_eq!(hues.warning, 0x9b_6b_ff);
        assert_eq!(hues.remote_marker, 0xff_5f_d2);
        assert_eq!(hues.divider, 0x6a_64_88);
    }

    #[test]
    fn omarchy_role_cascades_take_their_first_key_then_ansi() {
        use crate::color::{
            NON_TEXT_MIN_CONTRAST, contrast_corrected, contrast_floor_toward, mix_linear,
        };
        type ReadRole = fn(&ChromeRoles) -> u32;

        let foot = complete_foot_palette("101112", "d0d1d2", "203040", "d0d1d2");
        // Each role's cascade, first key first, with values that clear 3:1 on
        // the dark background so no floor moves them.
        let cascades: [(&str, &[&str], ReadRole); 9] = [
            ("surface_sunken", &["dark_background", "dark_bg"], |roles| {
                roles.surface_sunken
            }),
            (
                "selection",
                &["selection_background", "selection"],
                |roles| roles.selection,
            ),
            ("divider", &["border", "muted", "color8"], |roles| {
                roles.divider
            }),
            ("focus_ring", &["active_border_color", "accent"], |roles| {
                roles.focus_ring
            }),
            ("danger", &["red", "color1"], |roles| roles.danger),
            ("warning", &["warning", "yellow", "color3"], |roles| {
                roles.warning
            }),
            ("success", &["success", "green", "color2"], |roles| {
                roles.success
            }),
            ("info", &["cyan", "color6"], |roles| roles.info),
            ("remote_marker", &["signal", "magenta", "color5"], |roles| {
                roles.remote_marker
            }),
        ];
        let values = ["#ff8080", "#80ff80", "#8080ff"];
        for (role, keys, read) in cascades {
            for first in 0..keys.len() {
                let colors = keys[first..]
                    .iter()
                    .zip(&values[first..])
                    .map(|(key, value)| format!("{key} = \"{value}\"\n"))
                    .collect::<Vec<_>>()
                    .concat();
                let roles = resolve_omarchy_theme(&colors, &foot).unwrap().roles;
                assert_eq!(
                    read(&roles),
                    parse_color(values[first]).unwrap(),
                    "{role} from {}",
                    keys[first]
                );
            }
            // No key: the ANSI hue, raised to 3:1, or the derived fallback.
            let theme = resolve_omarchy_theme("", &foot).unwrap();
            let readable = contrast_corrected(theme.foreground, theme.background);
            let floored = |ansi: u32| {
                contrast_floor_toward(ansi, readable, theme.background, NON_TEXT_MIN_CONTRAST)
            };
            let fallback = match role {
                "surface_sunken" => mix_linear(theme.background, 0, 0.06),
                "selection" => 0x20_30_40,
                "divider" => theme.ansi[8],
                "focus_ring" => theme.roles.accent,
                "danger" => floored(theme.ansi[1]),
                "warning" => floored(theme.ansi[3]),
                "success" => floored(theme.ansi[2]),
                "info" => floored(theme.ansi[6]),
                _ => floored(theme.ansi[5]),
            };
            assert_eq!(read(&theme.roles), fallback, "{role} without a key");
        }
    }

    #[test]
    fn omarchy_appearance_follows_mode_then_foot_then_the_background() {
        let dark = complete_foot_palette("101112", "d0d1d2", "203040", "d0d1d2");
        let light = complete_foot_palette("f0f0e0", "202020", "c0c0c0", "202020");
        let named = |palette: &str, theme: &str| {
            format!("[main]\ninitial-color-theme={theme}\n{palette}").replace(
                "[colors-dark]",
                if theme == "light" {
                    "[colors-light]"
                } else {
                    "[colors-dark]"
                },
            )
        };
        let appearance =
            |colors: &str, foot: &str| resolve_omarchy_theme(colors, foot).unwrap().appearance;
        // The background alone.
        assert_eq!(appearance("", &dark), Appearance::Dark);
        assert_eq!(appearance("", &light), Appearance::Light);
        // Foot's initial-color-theme wins over the background.
        assert_eq!(appearance("", &named(&light, "dark")), Appearance::Dark);
        assert_eq!(appearance("", &named(&dark, "light")), Appearance::Light);
        // colors.toml mode wins over both.
        assert_eq!(
            appearance("mode = \"light\"\n", &named(&dark, "dark")),
            Appearance::Light
        );
        assert_eq!(appearance("mode = \"dark\"\n", &light), Appearance::Dark);
        // An unknown mode is ignored with a diagnostic.
        let mut diagnostics = Vec::new();
        let input = omarchy_theme_input("mode = \"dusk\"\n", &light, &mut diagnostics).unwrap();
        assert_eq!(input.appearance, Appearance::Light);
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].contains("mode \"dusk\""), "{diagnostics:?}");
        // The stock fixtures: flexoki is light, the others dark.
        for (name, colors, foot) in OMARCHY4_FIXTURES {
            let expected = if name == "flexoki-light" {
                Appearance::Light
            } else {
                Appearance::Dark
            };
            assert_eq!(appearance(colors, foot), expected, "{name}");
        }
        // JSON and built-in themes take their background's.
        assert_eq!(ResolvedTheme::default().appearance, Appearance::Dark);
        assert_eq!(json_theme(JSON_THEME).appearance, Appearance::Dark);
        assert_eq!(Appearance::of_background(0xff_ff_ff), Appearance::Light);
        assert_eq!(Appearance::of_background(0), Appearance::Dark);
    }

    #[test]
    fn invalid_new_omarchy_keys_are_skipped_once_and_old_keys_stay_strict() {
        let foot = complete_foot_palette("101112", "d0d1d2", "203040", "d0d1d2");
        // A key read only since semantic roles never rejects the theme: it is
        // skipped, its cascade goes on, and a diagnostic says so.
        let mut diagnostics = Vec::new();
        let input = omarchy_theme_input(
            "warning = \"nope\"\nyellow = \"#ffaa00\"\nborder = \"rgba(1,2,3)\"\nsignal = \"#12345\"\n",
            &foot,
            &mut diagnostics,
        )
        .unwrap();
        let roles = derive(&input);
        assert_eq!(roles.warning, 0xff_aa_00);
        assert_eq!(roles.divider, input.terminal.ansi[8]);
        assert_eq!(diagnostics.len(), 3, "{diagnostics:?}");
        for key in [
            "warning \"nope\"",
            "border \"rgba(1,2,3)\"",
            "signal \"#12345\"",
        ] {
            assert!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.contains(key)),
                "{key}: {diagnostics:?}"
            );
        }
        // The keys read before keep rejecting an invalid value.
        for colors in [
            "muted = \"nope\"\n",
            "color8 = \"nope\"\n",
            "lighter_background = \"nope\"\n",
            "accent = \"nope\"\n",
        ] {
            assert!(resolve_omarchy_theme(colors, &foot).is_err(), "{colors}");
        }
        // dark_foreground was strict only when muted and color8 were absent.
        assert!(resolve_omarchy_theme("dark_foreground = \"nope\"\n", &foot).is_err());
        let mut diagnostics = Vec::new();
        omarchy_theme_input(
            "color8 = \"#444444\"\ndark_foreground = \"nope\"\n",
            &foot,
            &mut diagnostics,
        )
        .unwrap();
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        // Each diagnostic is reported once per process, however often the
        // watcher reloads the theme.
        let diagnostic = "test diagnostic reported once";
        assert!(first_theme_diagnostic(diagnostic));
        assert!(!first_theme_diagnostic(diagnostic));
    }

    #[test]
    fn providers_report_the_files_the_watcher_fingerprints() {
        let theme_dir = Path::new("/theme");
        assert_eq!(
            OmarchyThemeProvider::live(theme_dir).watched_paths(),
            [
                PathBuf::from("/theme"),
                PathBuf::from("/theme/colors.toml"),
                PathBuf::from("/theme/foot.ini"),
            ]
        );
        assert_eq!(
            OmarchyThemeProvider::startup(theme_dir).watched_paths(),
            OmarchyThemeProvider::live(theme_dir).watched_paths()
        );
        let json = Path::new("/config/theme.json");
        assert_eq!(
            JsonThemeProvider::new(json).watched_paths(),
            [json.to_path_buf()]
        );

        // Absence: the startup Omarchy read and a missing JSON file give the
        // built-in input, and a live Omarchy read refuses it.
        let root =
            std::env::temp_dir().join(format!("splinterm-theme-providers-{}", std::process::id()));
        assert!(!root.exists());
        let builtin = BuiltinThemeProvider.load().unwrap();
        assert_eq!(
            OmarchyThemeProvider::startup(&root).load().unwrap(),
            builtin
        );
        assert!(OmarchyThemeProvider::live(&root).load().is_err());
        assert_eq!(
            JsonThemeProvider::new(&root.join("theme.json"))
                .load()
                .unwrap(),
            builtin
        );
        assert_eq!(
            load_provider_theme(&BuiltinThemeProvider).unwrap(),
            ResolvedTheme::default()
        );
    }
}
