use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::Result;
use splinterm::{
    ThemeUpdate, WindowTopologyUpdate, WindowUpdate,
    config::{
        AppConfig, ChromeColorOverrides, JsonThemeProvider, OmarchyShellConcern,
        OmarchyShellSettings, OmarchyTextSizeFallback, OmarchyThemeProvider, OmarchyThemeRoots,
        OmarchyThemeSelection, ResolvedTheme, ThemeColorConfig, ThemeSource, ThemeSourceLoad,
        load_omarchy_shell, load_theme_source_for_startup,
    },
    theme::{ThemeInput, ThemeProvider},
};
use tokio::sync::mpsc;

static NEXT_THEME_UPDATE_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Device, inode, length, mtime and ctime. The ctime catches a permission or
/// ACL change that makes an unreadable file readable without rewriting it.
type ThemeFileFingerprint = (u64, u64, u64, (i64, i64), (i64, i64));

/// What `config.ini` overrides in the active theme.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::app) struct ThemeOverrides {
    pub(in crate::app) alpha: Option<u16>,
    pub(in crate::app) blur: Option<bool>,
    /// Whether the Omarchy text size applies to the terminal: only while
    /// `config.ini` sets no font size.
    pub(in crate::app) follow_text_size: bool,
    /// Whether chrome text follows the Omarchy text size: only while
    /// `config.ini` sets no `[main] ui-font-pixelsize`. It decides only
    /// whether `base-size` problems are reported. The chrome size itself
    /// (`ui_text_size_px`) is always published, so a Window whose
    /// configuration reload drops the key can fall back to it live.
    pub(in crate::app) follow_ui_text_size: bool,
    /// The `[colors]` chrome background keys. Role keys are applied by the
    /// Window, which re-resolves each update from its theme input with its
    /// own `[colors]` keys whenever they differ from these.
    pub(in crate::app) chrome: ChromeColorOverrides,
}

impl ThemeOverrides {
    pub(in crate::app) fn from_config(config: &AppConfig) -> Self {
        Self {
            alpha: config.background_alpha,
            blur: config.background_blur,
            follow_text_size: config.follows_omarchy_text_size(),
            follow_ui_text_size: config.ui_font_size.is_none(),
            chrome: config.chrome_colors,
        }
    }

    /// The `[colors]` keys the watcher's themes are resolved with.
    fn colors(self) -> ThemeColorConfig {
        ThemeColorConfig {
            alpha: self.alpha,
            blur: self.blur,
            chrome: self.chrome,
            roles: None,
        }
    }

    /// Whether any text follows `base-size`, so its problems are reported.
    const fn reports_text_size(self) -> bool {
        self.follow_text_size || self.follow_ui_text_size
    }

    /// The text size an unparseable live `base-size` keeps: the one in use,
    /// by the terminal or, while it follows none, by chrome.
    fn text_size_fallback(current: &ResolvedTheme) -> OmarchyTextSizeFallback {
        OmarchyTextSizeFallback::Current(
            current.shell.text_size_px.or(current.shell.ui_text_size_px),
        )
    }

    fn apply(self, mut theme: ResolvedTheme) -> ResolvedTheme {
        if !self.follow_text_size {
            theme.shell.text_size_px = None;
            theme.shell.text_size_held = false;
        }
        theme
            .with_color_overrides(self.alpha, self.blur)
            .with_chrome_overrides(self.chrome)
    }
}

/// The `shell.toml` layers one poll reads: the selected theme directory's
/// file and the user's override. `panels` is false for an explicit JSON
/// palette, which follows only the text size.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ShellLayers<'a> {
    theme_dir: &'a Path,
    user: Option<&'a Path>,
    panels: bool,
}

/// The concrete files one poll reads. Omarchy sources pick one of their
/// ordered roots per poll, so a migration or root change is followed live.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveThemeSource<'a> {
    Omarchy {
        index: usize,
        theme_dir: &'a Path,
        user_shell: Option<&'a Path>,
    },
    Json {
        path: &'a Path,
        shell: Option<JsonShellRoot<'a>>,
    },
}

/// The Omarchy root an explicit JSON palette reads `shell.toml` from, chosen
/// with the same live selection and demotion grace as the native palette.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct JsonShellRoot<'a> {
    layers: ShellLayers<'a>,
    index: usize,
    /// The selected theme directory is missing while another root is claimed
    /// or while it is the last good root the selection holds: a theme switch,
    /// a briefly vanished root or an outage of every root. The current shell
    /// settings are kept rather than read from a partial, stale or absent root,
    /// as the native theme keeps its palette.
    gap: bool,
}

impl<'a> ActiveThemeSource<'a> {
    /// The palette provider a live poll fingerprints and reloads. A live
    /// Omarchy read never turns absence into the fallback palette.
    fn provider(self) -> Box<dyn ThemeProvider + 'a> {
        match self {
            Self::Omarchy { theme_dir, .. } => Box::new(OmarchyThemeProvider::live(theme_dir)),
            Self::Json { path, .. } => Box::new(JsonThemeProvider::new(path)),
        }
    }

    fn shell(self) -> Option<ShellLayers<'a>> {
        match self {
            Self::Omarchy {
                theme_dir,
                user_shell,
                ..
            } => Some(ShellLayers {
                theme_dir,
                user: user_shell,
                panels: true,
            }),
            Self::Json { shell, .. } => shell.map(|root| root.layers),
        }
    }
}

/// The stateless JSON shell layers for a one-shot startup read.
fn json_shell_layers(roots: Option<&OmarchyThemeRoots>) -> Option<ShellLayers<'_>> {
    roots.map(|roots| ShellLayers {
        theme_dir: roots.select(),
        user: roots.user_shell(),
        panels: false,
    })
}

fn active_theme_source<'a>(
    source: &'a ThemeSource,
    selection: &mut OmarchyThemeSelection,
) -> ActiveThemeSource<'a> {
    match source {
        ThemeSource::Omarchy(roots) => {
            let index = selection.select(roots);
            ActiveThemeSource::Omarchy {
                index,
                theme_dir: &roots.candidates()[index],
                user_shell: roots.user_shell(),
            }
        }
        ThemeSource::Json { path, shell } => ActiveThemeSource::Json {
            path,
            shell: shell.as_ref().map(|roots| {
                let index = selection.select(roots);
                let theme_dir = roots.candidates()[index].as_path();
                JsonShellRoot {
                    layers: ShellLayers {
                        theme_dir,
                        user: roots.user_shell(),
                        panels: false,
                    },
                    index,
                    // Missing while another root is claimed, or while the selection
                    // holds this last good root through an outage of every root.
                    gap: !theme_dir.is_dir() && (roots.any_claimed() || selection.holds(index)),
                }
            }),
        },
    }
}

fn theme_file_fingerprint(path: &Path) -> Option<ThemeFileFingerprint> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        (metadata.mtime(), metadata.mtime_nsec()),
        (metadata.ctime(), metadata.ctime_nsec()),
    ))
}

/// Each of the active provider's watched paths and its file identity. For
/// Omarchy the paths name the selected root, so a root change is observed.
#[derive(Clone, Debug, Eq, PartialEq)]
struct PaletteFingerprint(Vec<(PathBuf, Option<ThemeFileFingerprint>)>);

impl PaletteFingerprint {
    fn of(provider: &dyn ThemeProvider) -> Self {
        Self(
            provider
                .watched_paths()
                .into_iter()
                .map(|path| {
                    let fingerprint = theme_file_fingerprint(&path);
                    (path, fingerprint)
                })
                .collect(),
        )
    }

    /// No watched path exists.
    fn absent(&self) -> bool {
        self.0.iter().all(|(_, fingerprint)| fingerprint.is_none())
    }
}

/// Everything a shell load depends on: the selected theme directory, like the
/// native palette's root and directory identity, its `shell.toml`, the user's
/// override, and whether the selected root is held through a gap. A root
/// transition is observed even when neither root has a `shell.toml`.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ShellFingerprint {
    root: PathBuf,
    directory: Option<ThemeFileFingerprint>,
    theme: Option<ThemeFileFingerprint>,
    user: Option<ThemeFileFingerprint>,
    gap: bool,
}

impl ShellFingerprint {
    fn of(layers: ShellLayers<'_>, gap: bool) -> Self {
        Self {
            root: layers.theme_dir.to_path_buf(),
            directory: theme_file_fingerprint(layers.theme_dir),
            theme: theme_file_fingerprint(&layers.theme_dir.join("shell.toml")),
            user: layers.user.and_then(theme_file_fingerprint),
            gap,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ThemeSourceFingerprint {
    palette: PaletteFingerprint,
    shell: Option<ShellFingerprint>,
}

fn theme_source_fingerprint(source: ActiveThemeSource<'_>) -> ThemeSourceFingerprint {
    ThemeSourceFingerprint {
        palette: PaletteFingerprint::of(source.provider().as_ref()),
        shell: match source {
            ActiveThemeSource::Omarchy { .. } => source
                .shell()
                .map(|layers| ShellFingerprint::of(layers, false)),
            ActiveThemeSource::Json { shell, .. } => {
                shell.map(|root| ShellFingerprint::of(root.layers, root.gap))
            }
        },
    }
}

/// Treats completely absent optional Omarchy state as already observed at
/// startup. The first live poll then stays quiet unless a source appears.
/// Existing directories, staging paths, dangling links and metadata errors
/// still take the strict live-read path; losing a previously loaded theme
/// must not be converted into the startup fallback.
fn absent_startup_observation(source: &ThemeSource) -> Option<ThemeSourceFingerprint> {
    let ThemeSource::Omarchy(roots) = source else {
        return None;
    };
    let absent = roots.candidates().iter().all(|theme_dir| {
        [theme_dir.clone(), theme_dir.with_file_name("next-theme")]
            .iter()
            .all(|path| {
                matches!(
                    std::fs::symlink_metadata(path),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound
                )
            })
    });
    if !absent {
        return None;
    }
    let observed = theme_source_fingerprint(active_theme_source(
        source,
        &mut OmarchyThemeSelection::default(),
    ));
    // A source may have appeared between the absence check and fingerprint.
    observed.palette.absent().then_some(observed)
}

/// Layers the Omarchy shell settings onto a resolved palette. The bar height,
/// which sizes the tab strip, is not a palette setting, so every palette
/// follows it. A JSON palette also takes the text size but keeps Omarchy's
/// opaque panel defaults. Diagnostics for a setting Splinterm ignores are
/// dropped: alpha for a JSON palette, the text size while `config.ini` sets
/// both a font size and a UI font size, and the terminal's point mapping of
/// it while `config.ini` sets a terminal font size.
fn with_omarchy_shell(
    mut theme: ResolvedTheme,
    layers: ShellLayers<'_>,
    overrides: ThemeOverrides,
    fallback: OmarchyTextSizeFallback,
    diagnostics: &mut Vec<String>,
) -> ResolvedTheme {
    let load = load_omarchy_shell(layers.theme_dir, layers.user, fallback);
    diagnostics.extend(
        load.diagnostics
            .into_iter()
            .filter(|(concern, _)| match concern {
                OmarchyShellConcern::Read | OmarchyShellConcern::BarSize => true,
                OmarchyShellConcern::PanelAlpha => layers.panels,
                OmarchyShellConcern::TextSize => overrides.reports_text_size(),
                OmarchyShellConcern::TerminalTextSize => overrides.follow_text_size,
            })
            .map(|(_, diagnostic)| diagnostic),
    );
    theme.shell = if layers.panels {
        load.settings
    } else {
        OmarchyShellSettings {
            text_size_px: load.settings.text_size_px,
            ui_text_size_px: load.settings.ui_text_size_px,
            text_size_held: load.settings.text_size_held,
            bar_size_px: load.settings.bar_size_px,
            ..OmarchyShellSettings::default()
        }
    };
    theme
}

struct StartupThemeLoad {
    theme: ResolvedTheme,
    /// What the source gave, its shell settings layered as `theme`'s are.
    input: ThemeInput,
    /// The `[colors]` keys `theme` was resolved with.
    colors: ThemeColorConfig,
    /// Everything to print at startup, prefixed.
    diagnostics: Vec<String>,
    /// The raw shell diagnostics among them, which seed the live watcher.
    shell_diagnostics: Vec<String>,
    observed: Option<ThemeSourceFingerprint>,
}

fn prepare_startup_theme(source: &ThemeSource, overrides: ThemeOverrides) -> StartupThemeLoad {
    prepare_startup_theme_with_loader(source, overrides, load_theme_source_for_startup)
}

fn prepare_startup_theme_with_loader(
    source: &ThemeSource,
    overrides: ThemeOverrides,
    load: impl FnOnce(&ThemeSource) -> Result<ThemeSourceLoad>,
) -> StartupThemeLoad {
    let mut diagnostics = Vec::new();
    let (theme, mut input, observed) = match load(source) {
        Ok(loaded) => {
            let observed = if loaded.used_omarchy_fallback {
                absent_startup_observation(source)
            } else {
                None
            };
            (loaded.theme, loaded.input, observed)
        }
        Err(error) => {
            diagnostics.push(format!("{error:#}; using safe fallback palette"));
            (ResolvedTheme::default(), ThemeInput::builtin(), None)
        }
    };
    let layers = match source {
        ThemeSource::Omarchy(roots) => Some(ShellLayers {
            theme_dir: roots.select(),
            user: roots.user_shell(),
            panels: true,
        }),
        ThemeSource::Json { shell, .. } => json_shell_layers(shell.as_ref()),
    };
    let mut shell_diagnostics = Vec::new();
    let theme = match layers {
        Some(layers) => with_omarchy_shell(
            theme,
            layers,
            overrides,
            OmarchyTextSizeFallback::OmarchyDefault,
            &mut shell_diagnostics,
        ),
        None => theme,
    };
    input.shell.settings = theme.shell;
    diagnostics.extend(shell_diagnostics.iter().cloned());
    StartupThemeLoad {
        theme: overrides.apply(theme),
        input,
        colors: overrides.colors(),
        diagnostics: diagnostics
            .into_iter()
            .map(|diagnostic| format!("splinterm theme: {diagnostic}"))
            .collect(),
        shell_diagnostics,
        observed,
    }
}

/// The startup theme a live watcher continues from, with the shell
/// diagnostics startup already printed and optional absence already observed.
pub(in crate::app) struct StartupTheme {
    /// The theme as the watcher resolves it: without `[colors]` role keys,
    /// which each Window applies itself; see [`Self::for_window`].
    pub(in crate::app) theme: ResolvedTheme,
    /// What the source gave, for a Window to resolve again.
    pub(in crate::app) input: Arc<ThemeInput>,
    /// The `[colors]` keys `theme` was resolved with.
    pub(in crate::app) colors: ThemeColorConfig,
    shell_reported: Vec<String>,
    // Only the validated absence decision crosses the async startup boundary;
    // the watcher revalidates it and builds its own fingerprint at each poll.
    optional_absence: bool,
}

fn load_startup_theme_with_reporter(
    config: &AppConfig,
    mut report: impl FnMut(&str),
) -> StartupTheme {
    let loaded = prepare_startup_theme(&config.theme_source(), ThemeOverrides::from_config(config));
    for diagnostic in &loaded.diagnostics {
        report(diagnostic);
    }
    StartupTheme::from(loaded)
}

impl From<StartupThemeLoad> for StartupTheme {
    fn from(loaded: StartupThemeLoad) -> Self {
        Self {
            theme: loaded.theme,
            input: Arc::new(loaded.input),
            colors: loaded.colors,
            shell_reported: loaded.shell_diagnostics,
            optional_absence: loaded.observed.is_some(),
        }
    }
}

impl StartupTheme {
    /// The theme a Window with the `[colors]` keys `colors` opens with.
    pub(in crate::app) fn for_window(&self, colors: &ThemeColorConfig) -> ResolvedTheme {
        self.theme.recolored(&self.input, &self.colors, colors)
    }
}

/// A one-shot theme read for windows that start no `watch_theme`, such as
/// the standalone Dojo and session picker and the remote-status window: they
/// keep the palette and shell settings as of opening.
pub(in crate::app) fn load_startup_theme(config: &AppConfig) -> ResolvedTheme {
    load_startup_theme_for_watch(config).for_window(&config.theme_colors())
}

/// The theme a trusted dialog child opens with, read once as
/// [`load_startup_theme`] does but without printing the source's
/// diagnostics, which other Windows report: what a dialog child prints is
/// kept only to explain why it failed.
pub(in crate::app) fn load_dialog_theme(config: &AppConfig) -> ResolvedTheme {
    load_startup_theme_with_reporter(config, |_| {}).for_window(&config.theme_colors())
}

/// Loads the startup theme for a Window that also starts `watch_theme`.
pub(in crate::app) fn load_startup_theme_for_watch(config: &AppConfig) -> StartupTheme {
    load_startup_theme_with_reporter(config, |diagnostic| eprintln!("{diagnostic}"))
}

/// Loads the active source. A coherent Omarchy load records its root as the
/// last good one, which `OmarchyThemeSelection` holds through transient gaps.
/// Rejected shell settings are reported through `shell_diagnostics` and never
/// reject the palette; an invalid text size keeps the current one.
#[cfg(test)]
fn resolve_live_theme_update(
    source: ActiveThemeSource<'_>,
    selection: &mut OmarchyThemeSelection,
    overrides: ThemeOverrides,
    current: ResolvedTheme,
    shell_diagnostics: &mut Vec<String>,
) -> Result<Option<ResolvedTheme>> {
    resolve_live_theme(source, selection, overrides, current, shell_diagnostics)
        .map(|next| next.map(|(theme, _)| theme))
}

/// Loads the active source as [`resolve_live_theme_update`] does, with the
/// input the theme was resolved from, its shell settings layered as the
/// theme's are.
fn resolve_live_theme(
    source: ActiveThemeSource<'_>,
    selection: &mut OmarchyThemeSelection,
    overrides: ThemeOverrides,
    current: ResolvedTheme,
    shell_diagnostics: &mut Vec<String>,
) -> Result<Option<(ResolvedTheme, ThemeInput)>> {
    let mut input = source.provider().load()?;
    let palette = ResolvedTheme::from_input(&input);
    if let ActiveThemeSource::Omarchy { index, .. } = source {
        selection.accepted(index);
    }
    if let ActiveThemeSource::Json {
        shell: Some(root), ..
    } = source
    {
        if root.gap {
            let mut theme = palette;
            theme.shell = current.shell;
            input.shell.settings = current.shell;
            let next = overrides.apply(theme);
            return Ok((next != current).then_some((next, input)));
        }
        if root.layers.theme_dir.is_dir() {
            selection.accepted(root.index);
        }
    }
    let layered = match source.shell() {
        Some(layers) => with_omarchy_shell(
            palette,
            layers,
            overrides,
            ThemeOverrides::text_size_fallback(&current),
            shell_diagnostics,
        ),
        None => palette,
    };
    input.shell.settings = layered.shell;
    let next = overrides.apply(layered);
    Ok((next != current).then_some((next, input)))
}

#[derive(Default)]
struct ThemeReloadDiagnostics {
    rejection_reported: bool,
    shell_reported: Vec<String>,
}

impl ThemeReloadDiagnostics {
    /// Continues from startup, which already printed its shell diagnostics.
    fn after_startup(startup: &StartupTheme) -> Self {
        Self {
            rejection_reported: false,
            shell_reported: startup.shell_reported.clone(),
        }
    }

    fn accepted(&mut self) {
        self.rejection_reported = false;
    }

    fn rejected(&mut self, error: &anyhow::Error) -> Option<String> {
        if self.rejection_reported {
            return None;
        }
        self.rejection_reported = true;
        Some(format!("splinterm theme reload rejected: {error:#}"))
    }

    /// Returns the shell diagnostics not already reported by the previous
    /// load, so a persisting invalid value is reported once.
    fn shell(&mut self, diagnostics: Vec<String>) -> Vec<String> {
        let fresh = diagnostics
            .iter()
            .filter(|diagnostic| !self.shell_reported.contains(diagnostic))
            .map(|diagnostic| format!("splinterm theme: {diagnostic}"))
            .collect();
        self.shell_reported = diagnostics;
        fresh
    }
}

pub(in crate::app) enum ThemeUpdateSink {
    Panes(Vec<mpsc::Sender<WindowUpdate>>),
    Topology(mpsc::Sender<WindowTopologyUpdate>),
}

pub(in crate::app) async fn watch_theme(
    source: ThemeSource,
    overrides: ThemeOverrides,
    startup: StartupTheme,
    sink: ThemeUpdateSink,
) {
    watch_theme_with_reporter(source, overrides, startup, sink, |diagnostic| {
        eprintln!("{diagnostic}");
    })
    .await;
}

async fn watch_theme_with_reporter(
    source: ThemeSource,
    overrides: ThemeOverrides,
    startup: StartupTheme,
    sink: ThemeUpdateSink,
    mut report: impl FnMut(&str) + Send,
) {
    let mut current = startup.theme;
    let mut diagnostics = ThemeReloadDiagnostics::after_startup(&startup);
    let mut optional_absence = startup.optional_absence;
    let mut observed = None;
    let mut selection = OmarchyThemeSelection::default();
    let mut poll = tokio::time::interval(std::time::Duration::from_millis(500));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        poll.tick().await;
        let active = active_theme_source(&source, &mut selection);
        let next_fingerprint = theme_source_fingerprint(active);
        if optional_absence {
            // The regular fingerprint follows symlinks and omits staging.
            // Recheck actual absence, so a newly dangling/staged/unreadable
            // source cannot be silently skipped with the same fingerprint.
            if absent_startup_observation(&source).as_ref() == Some(&next_fingerprint) {
                observed = Some(next_fingerprint);
                continue;
            }
            optional_absence = false;
        } else if observed.as_ref() == Some(&next_fingerprint) {
            continue;
        }
        observed = Some(next_fingerprint);
        let mut shell_diagnostics = Vec::new();
        let resolved = resolve_live_theme(
            active,
            &mut selection,
            overrides,
            current,
            &mut shell_diagnostics,
        );
        if resolved.is_ok() {
            for diagnostic in diagnostics.shell(shell_diagnostics) {
                report(&diagnostic);
            }
        }
        match resolved {
            Ok(Some((next, input))) => {
                current = next;
                diagnostics.accepted();
                let update = ThemeUpdate {
                    generation: NEXT_THEME_UPDATE_GENERATION.fetch_add(1, Ordering::Relaxed),
                    theme: next,
                    colors: overrides.colors(),
                    input: Some(Arc::new(input)),
                };
                let delivered = match &sink {
                    ThemeUpdateSink::Panes(updates) => {
                        let mut delivered = false;
                        for updates in updates {
                            delivered |= updates
                                .send(WindowUpdate::Theme(update.clone()))
                                .await
                                .is_ok();
                        }
                        delivered
                    }
                    ThemeUpdateSink::Topology(updates) => updates
                        .send(WindowTopologyUpdate::Theme(update))
                        .await
                        .is_ok(),
                };
                if !delivered {
                    break;
                }
            }
            Ok(None) => diagnostics.accepted(),
            Err(error) => {
                if let Some(diagnostic) = diagnostics.rejected(&error) {
                    report(&diagnostic);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use splinterm::config::{OMARCHY_ROOT_DEMOTION_GRACE_POLLS, load_theme_source};

    const NO_OVERRIDES: ThemeOverrides = ThemeOverrides {
        alpha: None,
        blur: None,
        follow_text_size: true,
        follow_ui_text_size: true,
        chrome: ChromeColorOverrides::NONE,
    };

    #[tokio::test(start_paused = true)]
    async fn absent_optional_omarchy_does_not_emit_a_live_reload_rejection() {
        use std::sync::{Arc, Mutex};

        let root = std::env::temp_dir().join(format!(
            "splinterm-absent-optional-theme-{}",
            std::process::id()
        ));
        assert!(!root.exists());
        let source = ThemeSource::Omarchy(OmarchyThemeRoots::fixed(root.join("theme")));
        let loaded = prepare_startup_theme(&source, NO_OVERRIDES);
        assert!(loaded.diagnostics.is_empty());
        let startup = StartupTheme::from(loaded);
        let (updates, mut received) = mpsc::channel(1);
        let reported = Arc::new(Mutex::new(Vec::new()));
        let live_reported = Arc::clone(&reported);
        let watcher = tokio::spawn(watch_theme_with_reporter(
            source,
            NO_OVERRIDES,
            startup,
            ThemeUpdateSink::Panes(vec![updates]),
            move |diagnostic| live_reported.lock().unwrap().push(diagnostic.to_owned()),
        ));
        tokio::task::yield_now().await;
        for _ in 0..3 {
            tokio::time::advance(std::time::Duration::from_millis(500)).await;
            tokio::task::yield_now().await;
        }
        watcher.abort();
        assert!(received.try_recv().is_err());
        let reported = reported.lock().unwrap();
        assert!(
            reported.is_empty(),
            "optional absence must stay quiet: {reported:?}"
        );
        assert!(
            !root.exists(),
            "the watcher must not create integration state"
        );
    }

    struct OptionalThemeRoot(PathBuf);

    impl OptionalThemeRoot {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "splinterm-optional-theme-{label}-{}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for OptionalThemeRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    async fn theme_poll() {
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(500)).await;
        tokio::task::yield_now().await;
    }

    #[tokio::test(start_paused = true)]
    async fn an_optional_theme_can_appear_live_and_later_loss_is_still_rejected() {
        use std::sync::{Arc, Mutex};

        let root = OptionalThemeRoot::new("discovery");
        let first = root.0.join("xdg/theme");
        let later = root.0.join("legacy/theme");
        let source =
            ThemeSource::Omarchy(OmarchyThemeRoots::ordered(vec![first, later.clone()]).unwrap());
        let startup = StartupTheme::from(prepare_startup_theme(&source, NO_OVERRIDES));
        let (updates, mut received) = mpsc::channel(8);
        let reported = Arc::new(Mutex::new(Vec::new()));
        let live_reported = Arc::clone(&reported);
        let watcher = tokio::spawn(watch_theme_with_reporter(
            source,
            NO_OVERRIDES,
            startup,
            ThemeUpdateSink::Panes(vec![updates]),
            move |diagnostic| live_reported.lock().unwrap().push(diagnostic.to_owned()),
        ));
        theme_poll().await;
        assert!(reported.lock().unwrap().is_empty());
        assert!(received.try_recv().is_err());

        write_theme(&later, 0x12_34_56);
        theme_poll().await;
        let WindowUpdate::Theme(update) = received.try_recv().unwrap() else {
            panic!("a newly discovered theme must publish its palette");
        };
        assert_eq!(update.theme.background, 0x12_34_56);
        std::fs::remove_dir_all(&later).unwrap();
        theme_poll().await;
        theme_poll().await;
        assert!(received.try_recv().is_err(), "loss keeps the last palette");
        assert_eq!(reported.lock().unwrap().len(), 1, "bounded rejection");
        assert!(reported.lock().unwrap()[0].contains("theme reload rejected"));

        write_theme(&later, 0x65_43_21);
        theme_poll().await;
        let WindowUpdate::Theme(update) = received.try_recv().unwrap() else {
            panic!("a restored theme must be followed live");
        };
        assert_eq!(update.theme.background, 0x65_43_21);
        watcher.abort();
    }

    #[test]
    fn config_chrome_keys_reach_every_theme_the_window_applies() {
        let overrides = ThemeOverrides::from_config(
            &splinterm::config::parse(
                "[colors]\nactive-tab-alpha=0.5\ntab-strip-alpha=1.0\nexplorer-alpha=0.8\n",
            )
            .unwrap()
            .config,
        );
        // Startup and every live theme update pass through `apply`.
        let mut input = splinterm::theme::ThemeInput::builtin();
        input.shell.background_alpha = u16::MAX / 2;
        let glassy = ResolvedTheme::from_input(&input);
        let applied = overrides.apply(glassy);
        assert_eq!(applied.chrome.active_tab_alpha, u16::MAX / 2);
        assert_eq!(applied.tab_strip_alpha(), u16::MAX);
        assert_eq!(applied.explorer_alpha(), 52_428);
        assert_eq!(applied.background_alpha, u16::MAX / 2);
        assert_eq!(NO_OVERRIDES.apply(glassy).chrome.active_tab_alpha, u16::MAX);
    }

    #[test]
    fn claimed_or_broken_optional_roots_are_not_treated_as_absent() {
        use std::os::unix::fs::symlink;

        let root = OptionalThemeRoot::new("claimed");
        for label in ["incomplete", "staging", "dangling", "not-a-directory"] {
            let base = root.0.join(label);
            std::fs::create_dir(&base).unwrap();
            let theme = base.join("theme");
            match label {
                "incomplete" => std::fs::create_dir(&theme).unwrap(),
                "staging" => write_theme(&base.join("next-theme"), 0x12_34_56),
                "dangling" => symlink(base.join("missing"), &theme).unwrap(),
                "not-a-directory" => std::fs::write(&theme, "not a theme directory").unwrap(),
                _ => unreachable!(),
            }
            let source = ThemeSource::Omarchy(OmarchyThemeRoots::fixed(theme));
            let startup = prepare_startup_theme(&source, NO_OVERRIDES);
            assert!(startup.observed.is_none(), "{label}");
            let mut selection = OmarchyThemeSelection::default();
            let active = active_theme_source(&source, &mut selection);
            assert!(
                resolve_live_theme_update(
                    active,
                    &mut selection,
                    NO_OVERRIDES,
                    startup.theme,
                    &mut Vec::new(),
                )
                .is_err(),
                "{label} must still reject a live read"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_theme_lost_between_startup_and_the_first_poll_is_still_rejected() {
        use std::sync::{Arc, Mutex};

        let root = OptionalThemeRoot::new("startup-loss");
        let theme = root.0.join("theme");
        write_theme(&theme, 0x12_34_56);
        let source = ThemeSource::Omarchy(OmarchyThemeRoots::fixed(theme.clone()));
        let loaded = prepare_startup_theme(&source, NO_OVERRIDES);
        assert!(loaded.observed.is_none());
        assert_eq!(loaded.theme.background, 0x12_34_56);
        let startup = StartupTheme::from(loaded);
        std::fs::remove_dir_all(&theme).unwrap();
        let (updates, mut received) = mpsc::channel(1);
        let reported = Arc::new(Mutex::new(Vec::new()));
        let live_reported = Arc::clone(&reported);
        let watcher = tokio::spawn(watch_theme_with_reporter(
            source,
            NO_OVERRIDES,
            startup,
            ThemeUpdateSink::Panes(vec![updates]),
            move |diagnostic| live_reported.lock().unwrap().push(diagnostic.to_owned()),
        ));
        theme_poll().await;
        watcher.abort();
        assert!(received.try_recv().is_err());
        assert_eq!(reported.lock().unwrap().len(), 1);
        assert!(reported.lock().unwrap()[0].contains("theme reload rejected"));
    }

    #[test]
    fn explicit_json_does_not_use_the_optional_omarchy_startup_observation() {
        let root = OptionalThemeRoot::new("explicit-json");
        let path = root.0.join("theme.json");
        let source = ThemeSource::Json {
            path: path.clone(),
            shell: None,
        };
        assert!(
            prepare_startup_theme(&source, NO_OVERRIDES)
                .observed
                .is_none()
        );
        std::fs::write(path, "invalid JSON").unwrap();
        let startup = prepare_startup_theme(&source, NO_OVERRIDES);
        assert!(startup.observed.is_none());
        assert_eq!(startup.diagnostics.len(), 1);
        assert!(live_update(&source, None, None, startup.theme).is_err());
    }

    async fn bounded_live_rejections(source: ThemeSource, startup: StartupTheme) -> Vec<String> {
        use std::sync::{Arc, Mutex};

        let (updates, mut received) = mpsc::channel(1);
        let reported = Arc::new(Mutex::new(Vec::new()));
        let live_reported = Arc::clone(&reported);
        let watcher = tokio::spawn(watch_theme_with_reporter(
            source,
            NO_OVERRIDES,
            startup,
            ThemeUpdateSink::Panes(vec![updates]),
            move |diagnostic| live_reported.lock().unwrap().push(diagnostic.to_owned()),
        ));
        theme_poll().await;
        theme_poll().await;
        watcher.abort();
        let _ = watcher.await;
        assert!(
            received.try_recv().is_err(),
            "a rejected load keeps its palette"
        );
        reported.lock().unwrap().clone()
    }

    #[tokio::test(start_paused = true)]
    async fn delayed_staging_after_absent_startup_still_rejects_the_live_load() {
        let root = OptionalThemeRoot::new("delayed-staging");
        let theme = root.0.join("theme");
        let source = ThemeSource::Omarchy(OmarchyThemeRoots::fixed(theme));
        let startup = StartupTheme::from(prepare_startup_theme(&source, NO_OVERRIDES));
        assert!(startup.optional_absence);
        write_theme(&root.0.join("next-theme"), 0x12_34_56);
        let messages = bounded_live_rejections(source, startup).await;
        assert_eq!(messages.len(), 1, "staging must not remain quietly absent");
        assert!(messages[0].contains("theme reload rejected"));
    }

    #[tokio::test(start_paused = true)]
    async fn delayed_dangling_link_after_absent_startup_still_rejects_the_live_load() {
        let root = OptionalThemeRoot::new("delayed-dangling");
        let theme = root.0.join("theme");
        let source = ThemeSource::Omarchy(OmarchyThemeRoots::fixed(theme.clone()));
        let startup = StartupTheme::from(prepare_startup_theme(&source, NO_OVERRIDES));
        assert!(startup.optional_absence);
        std::os::unix::fs::symlink(root.0.join("missing"), &theme).unwrap();
        let messages = bounded_live_rejections(source, startup).await;
        assert_eq!(
            messages.len(),
            1,
            "a dangling link must not remain quietly absent"
        );
        assert!(messages[0].contains("theme reload rejected"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_successful_startup_read_cannot_retain_an_earlier_absence_seed() {
        let root = OptionalThemeRoot::new("startup-interleaving");
        let theme = root.0.join("theme");
        let source = ThemeSource::Omarchy(OmarchyThemeRoots::fixed(theme.clone()));
        let loaded = prepare_startup_theme_with_loader(&source, NO_OVERRIDES, |source| {
            write_theme(&theme, 0x12_34_56);
            let loaded = load_theme_source_for_startup(source);
            std::fs::remove_dir_all(&theme).unwrap();
            loaded
        });
        assert_eq!(loaded.theme.background, 0x12_34_56);
        assert!(loaded.diagnostics.is_empty());
        assert!(
            loaded.observed.is_none(),
            "a real palette load invalidates absence"
        );
        let messages = bounded_live_rejections(source, StartupTheme::from(loaded)).await;
        assert_eq!(
            messages.len(),
            1,
            "the loss of the loaded theme must be reported"
        );
        assert!(messages[0].contains("theme reload rejected"));
    }

    fn fingerprint(source: &ThemeSource) -> ThemeSourceFingerprint {
        theme_source_fingerprint(active_theme_source(
            source,
            &mut OmarchyThemeSelection::default(),
        ))
    }

    fn live_update(
        source: &ThemeSource,
        alpha_override: Option<u16>,
        blur_override: Option<bool>,
        current: ResolvedTheme,
    ) -> Result<Option<ResolvedTheme>> {
        let mut selection = OmarchyThemeSelection::default();
        resolve_live_theme_update(
            active_theme_source(source, &mut selection),
            &mut selection,
            ThemeOverrides {
                alpha: alpha_override,
                blur: blur_override,
                ..NO_OVERRIDES
            },
            current,
            &mut Vec::new(),
        )
    }

    fn write_theme(theme_dir: &Path, background: u32) {
        use std::fmt::Write as _;

        std::fs::create_dir_all(theme_dir).unwrap();
        std::fs::write(theme_dir.join("colors.toml"), "accent=\"#010203\"\n").unwrap();
        let mut foot = format!("[colors-dark]\nbackground={background:06x}\nforeground=e0e1e2\n");
        for index in 0..8 {
            write!(
                foot,
                "regular{index}={index:06x}\nbright{index}={:06x}\n",
                index + 8
            )
            .unwrap();
        }
        std::fs::write(theme_dir.join("foot.ini"), foot).unwrap();
    }

    /// `None` marks a rejected read, which keeps the last published palette.
    fn poll(source: &ThemeSource, selection: &mut OmarchyThemeSelection) -> (PathBuf, Option<u32>) {
        let active = active_theme_source(source, selection);
        let ActiveThemeSource::Omarchy { theme_dir, .. } = active else {
            unreachable!("Omarchy source");
        };
        let loaded = resolve_live_theme_update(
            active,
            selection,
            NO_OVERRIDES,
            ResolvedTheme::default(),
            &mut Vec::new(),
        )
        .map(|theme| {
            theme.map_or(ResolvedTheme::default().background, |theme| {
                theme.background
            })
        })
        .ok();
        (theme_dir.to_path_buf(), loaded)
    }

    #[test]
    fn omarchy_roots_follow_xdg_then_home_then_omarchy_3_and_migrations_live() {
        let root = std::env::temp_dir().join(format!(
            "splinterm-omarchy-root-order-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let xdg = root.join("xdg/omarchy/current/theme");
        let home_state = root.join("home/.local/state/omarchy/current/theme");
        let legacy = root.join("home/.config/omarchy/current/theme");
        let source = ThemeSource::Omarchy(
            OmarchyThemeRoots::ordered(vec![xdg.clone(), home_state.clone(), legacy.clone()])
                .unwrap(),
        );
        let mut selection = OmarchyThemeSelection::default();

        // Omarchy 3.x only: the read-only fallback is used.
        write_theme(&legacy, 0x30_30_30);
        assert_eq!(
            poll(&source, &mut selection),
            (legacy.clone(), Some(0x30_30_30))
        );

        // The Omarchy 4 migration moves current/ to ~/.local/state; a higher
        // root is followed immediately.
        write_theme(&home_state, 0x40_40_40);
        assert_eq!(
            poll(&source, &mut selection),
            (home_state.clone(), Some(0x40_40_40))
        );

        // XDG_STATE_HOME outranks Omarchy's hard-coded $HOME path.
        write_theme(&xdg, 0x50_50_50);
        assert_eq!(
            poll(&source, &mut selection),
            (xdg.clone(), Some(0x50_50_50))
        );
        std::fs::remove_dir_all(root.join("xdg")).unwrap();
        for _ in 0..OMARCHY_ROOT_DEMOTION_GRACE_POLLS {
            let (selected, loaded) = poll(&source, &mut selection);
            assert_eq!(selected, xdg);
            assert!(loaded.is_none());
        }
        assert_eq!(
            poll(&source, &mut selection),
            (home_state.clone(), Some(0x40_40_40))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one bounded on-off-on scenario per fixture"
    )]
    fn glassy_windows_on_off_on_is_followed_live_through_in_place_edits() {
        // Each glassy fixture is the plain one after Omarchy's
        // omarchy-theme-set-glass (4a9c83de) with Glassy Windows on; turning
        // it off restores the plain file byte for byte.
        let glass_alpha = splinterm::config::parse("[colors]\nalpha=0.65\n")
            .unwrap()
            .config
            .background_alpha
            .unwrap();
        let theme_alpha = splinterm::config::parse("[colors]\nalpha=0.9\n")
            .unwrap()
            .config
            .background_alpha
            .unwrap();
        // (fixture, plain foot.ini, glassy foot.ini, plain alpha/blur, glassy alpha/blur)
        let cases = [
            (
                "stock-template",
                include_str!("../../tests/fixtures/omarchy4/glass/stock-template.foot.ini"),
                include_str!("../../tests/fixtures/omarchy4/glass/stock-template.glassy.foot.ini"),
                (u16::MAX, true),
                (glass_alpha, true),
            ),
            (
                "user-template",
                include_str!("../../tests/fixtures/omarchy4/glass/user-template.foot.ini"),
                include_str!("../../tests/fixtures/omarchy4/glass/user-template.glassy.foot.ini"),
                (u16::MAX, false),
                (glass_alpha, true),
            ),
            (
                "theme-alpha",
                include_str!("../../tests/fixtures/omarchy4/glass/theme-alpha.foot.ini"),
                include_str!("../../tests/fixtures/omarchy4/glass/theme-alpha.glassy.foot.ini"),
                (theme_alpha, false),
                (theme_alpha, true),
            ),
        ];
        let root =
            std::env::temp_dir().join(format!("splinterm-glass-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (name, plain, glassy, plain_effect, glassy_effect) in cases {
            let theme = root.join(name);
            std::fs::create_dir_all(&theme).unwrap();
            std::fs::write(
                theme.join("colors.toml"),
                include_str!("../../tests/fixtures/omarchy4/tokyo-night/colors.toml"),
            )
            .unwrap();
            std::fs::write(theme.join("foot.ini"), plain).unwrap();
            let source = ThemeSource::Omarchy(OmarchyThemeRoots::fixed(theme.clone()));
            let mut selection = OmarchyThemeSelection::default();
            let mut observed = fingerprint(&source);
            let mut current = live_update(&source, None, None, ResolvedTheme::default())
                .unwrap()
                .unwrap();
            assert_eq!(
                (current.background_alpha, current.background_blur),
                plain_effect,
                "{name}"
            );
            let palette = current;
            for (step, (contents, effect)) in [
                (glassy, glassy_effect),
                (plain, plain_effect),
                (glassy, glassy_effect),
            ]
            .into_iter()
            .enumerate()
            {
                let foot = theme.join("foot.ini");
                if step == 1 {
                    // Truncate and rewrite the same inode.
                    std::fs::write(&foot, contents).unwrap();
                } else {
                    // sed -i, as omarchy-theme-set-glass uses: a new inode.
                    let staged = theme.join("foot.ini.sed");
                    std::fs::write(&staged, contents).unwrap();
                    std::fs::rename(&staged, &foot).unwrap();
                }
                let next_fingerprint = fingerprint(&source);
                assert_ne!(next_fingerprint, observed, "{name} step {step}");
                observed = next_fingerprint;
                let active = active_theme_source(&source, &mut selection);
                let next = resolve_live_theme_update(
                    active,
                    &mut selection,
                    NO_OVERRIDES,
                    current,
                    &mut Vec::new(),
                )
                .unwrap()
                .expect("a glass toggle publishes a theme update");
                assert_eq!(
                    (next.background_alpha, next.background_blur),
                    effect,
                    "{name} step {step}"
                );
                assert_eq!(
                    next.with_color_overrides(
                        Some(palette.background_alpha),
                        Some(palette.background_blur)
                    ),
                    palette,
                    "{name} step {step}: only the background effect changes"
                );
                current = next;
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn omarchy_theme_switch_gap_never_flaps_to_a_stale_omarchy_3_root() {
        let root =
            std::env::temp_dir().join(format!("splinterm-omarchy-root-gap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let state_current = root.join("home/.local/state/omarchy/current");
        let home_state = state_current.join("theme");
        let legacy = root.join("home/.config/omarchy/current/theme");
        let source = ThemeSource::Omarchy(
            OmarchyThemeRoots::ordered(vec![home_state.clone(), legacy.clone()]).unwrap(),
        );
        let mut selection = OmarchyThemeSelection::default();
        write_theme(&legacy, 0x30_30_30);
        write_theme(&home_state, 0x40_40_40);
        assert_eq!(
            poll(&source, &mut selection),
            (home_state.clone(), Some(0x40_40_40))
        );

        // omarchy-theme-set stages current/next-theme, then runs
        // rm -rf current/theme; mv current/next-theme current/theme. The staged
        // theme keeps the root claimed for as long as the gap lasts, and each
        // read is rejected.
        write_theme(&state_current.join("next-theme"), 0x41_41_41);
        std::fs::remove_dir_all(&home_state).unwrap();
        for _ in 0..=OMARCHY_ROOT_DEMOTION_GRACE_POLLS * 2 {
            let (selected, loaded) = poll(&source, &mut selection);
            assert_eq!(selected, home_state);
            assert!(loaded.is_none());
        }
        std::fs::rename(state_current.join("next-theme"), &home_state).unwrap();
        assert_eq!(
            poll(&source, &mut selection),
            (home_state.clone(), Some(0x41_41_41))
        );

        // A whole-current replacement keeps the last good root through the
        // bounded grace window.
        let displaced = root.join("displaced-current");
        std::fs::rename(&state_current, &displaced).unwrap();
        for _ in 0..OMARCHY_ROOT_DEMOTION_GRACE_POLLS {
            let (selected, loaded) = poll(&source, &mut selection);
            assert_eq!(selected, home_state);
            assert!(loaded.is_none());
        }
        std::fs::rename(&displaced, &state_current).unwrap();
        assert_eq!(
            poll(&source, &mut selection),
            (home_state.clone(), Some(0x41_41_41))
        );

        // A sustained removal, such as a downgrade, is accepted after it.
        std::fs::rename(&state_current, &displaced).unwrap();
        for _ in 0..OMARCHY_ROOT_DEMOTION_GRACE_POLLS {
            assert_eq!(poll(&source, &mut selection).0, home_state);
        }
        assert_eq!(
            poll(&source, &mut selection),
            (legacy.clone(), Some(0x30_30_30))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_theme_reload_switches_dark_light_dark() {
        use std::fmt::Write as _;

        let root = std::env::temp_dir().join(format!(
            "splinterm-light-theme-reload-{}",
            std::process::id()
        ));
        let theme = root.join("theme");
        std::fs::create_dir_all(&theme).unwrap();
        let source = ThemeSource::Omarchy(OmarchyThemeRoots::fixed(theme.clone()));
        let mut current = ResolvedTheme::default();
        for (generation, mode, background, foreground) in [
            (0, "dark", 0x10_11_12, 0xe0_e1_e2),
            (1, "light", 0xf8_f9_fa, 0x20_21_22),
            (2, "dark", 0x10_11_12, 0xe0_e1_e2),
        ] {
            let before = fingerprint(&source);
            std::fs::rename(&theme, root.join(format!("previous-{generation}"))).unwrap();
            assert!(live_update(&source, None, None, current).is_err());
            std::fs::create_dir(&theme).unwrap();
            std::fs::write(theme.join("colors.toml"), "accent=\"#010203\"\n").unwrap();
            let mut foot = format!(
                "[main]\ninitial-color-theme={mode}\n[colors-{mode}]\nbackground={background:06x}\nforeground={foreground:06x}\nalpha=0.85\nblur=yes\n"
            );
            for index in 0..8 {
                write!(
                    foot,
                    "regular{index}={index:06x}\nbright{index}={:06x}\n",
                    index + 8
                )
                .unwrap();
            }
            std::fs::write(theme.join("foot.ini"), foot).unwrap();
            assert_ne!(fingerprint(&source), before);
            let next = live_update(&source, Some(4321), Some(false), current)
                .unwrap()
                .expect("theme switch must publish a palette");
            assert_eq!(next.background, background);
            assert_eq!(next.foreground, foreground);
            assert_eq!(next.background_alpha, 4321);
            assert!(!next.background_blur);
            assert_eq!(load_theme_source(&source).unwrap().background, background);
            assert!(
                live_update(&source, Some(4321), Some(false), next)
                    .unwrap()
                    .is_none()
            );
            current = next;
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_theme_fingerprint_tracks_omarchy_atomic_directory_replacement() {
        let root = std::env::temp_dir().join(format!(
            "splinterm-omarchy-theme-fingerprint-{}",
            std::process::id()
        ));
        let theme = root.join("theme");
        let previous = root.join("previous");
        std::fs::create_dir_all(&theme).unwrap();
        std::fs::write(theme.join("colors.toml"), b"accent = \"#010203\"\n").unwrap();
        std::fs::write(theme.join("foot.ini"), b"[colors-dark]\n").unwrap();
        let source = ThemeSource::Omarchy(OmarchyThemeRoots::fixed(theme.clone()));
        let first = fingerprint(&source);

        std::fs::rename(&theme, &previous).unwrap();
        assert!(live_update(&source, None, None, ResolvedTheme::default()).is_err());
        std::fs::create_dir(&theme).unwrap();
        std::fs::write(theme.join("colors.toml"), b"accent = \"#010203\"\n").unwrap();
        std::fs::write(theme.join("foot.ini"), b"[colors-dark]\n").unwrap();
        assert_ne!(fingerprint(&source), first);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn theme_fingerprint_changes_without_parsing_unchanged_content() {
        let path = std::env::temp_dir().join(format!(
            "splinterm-theme-fingerprint-{}",
            std::process::id()
        ));
        std::fs::write(&path, b"one").unwrap();
        let first = theme_file_fingerprint(&path).unwrap();
        assert_eq!(theme_file_fingerprint(&path), Some(first));
        std::fs::write(&path, b"different-length").unwrap();
        assert_ne!(theme_file_fingerprint(&path), Some(first));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn theme_fingerprint_changes_when_read_access_is_restored() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!(
            "splinterm-theme-fingerprint-access-{}",
            std::process::id()
        ));
        std::fs::write(&path, b"[menu]\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = theme_file_fingerprint(&path).unwrap();
        // ctime comes from the kernel's coarse clock; step past one tick.
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        // Only the mode changed: same device, inode, length and mtime.
        let readable = theme_file_fingerprint(&path).unwrap();
        assert_eq!(
            (readable.0, readable.1, readable.2, readable.3),
            (unreadable.0, unreadable.1, unreadable.2, unreadable.3)
        );
        assert_ne!(readable, unreadable);
        std::fs::remove_file(path).unwrap();
    }

    fn valid_theme_json(alpha: f32, blur: bool) -> String {
        format!(
            r##"{{"background":"#000001","foreground":"#000003","cursor":"#000003","selection":"#000004","url":"#000005","ui_accent":"#000006","alpha":{alpha},"blur":{blur},"ansi":["#000000","#000001","#000002","#000003","#000004","#000005","#000006","#000007","#000008","#000009","#00000a","#00000b","#00000c","#00000d","#00000e","#00000f"]}}"##
        )
    }

    #[test]
    fn malformed_startup_and_live_themes_preserve_bounded_safe_state() {
        let path =
            std::env::temp_dir().join(format!("splinterm-theme-resolution-{}", std::process::id()));
        std::fs::write(&path, b"{}").unwrap();

        let mut config = AppConfig {
            theme_path: Some(path.clone()),
            background_alpha: Some(1234),
            background_blur: Some(true),
            // Explicit terminal and UI sizes and a JSON palette ignore every
            // shell.toml setting except the bar size, which a stock or absent
            // Omarchy install on this host always sets validly, so its files
            // add no output.
            font_size_authority: splinterm::config::FontAuthority::Explicit,
            ui_font_size: Some(splinterm::geometry::FontSize::Pixels(14.0)),
            ..AppConfig::default()
        };
        for _launch_path in ["single-pane", "multi-pane"] {
            let mut startup_diagnostics = Vec::new();
            let startup = load_startup_theme_with_reporter(&config, |diagnostic| {
                startup_diagnostics.push(diagnostic.to_owned());
            })
            .theme;
            assert_eq!(startup_diagnostics.len(), 1);
            assert_eq!(startup.background_alpha, 1234);
            assert!(startup.background_blur);
        }

        std::fs::write(&path, valid_theme_json(0.25, true)).unwrap();
        config.background_alpha = None;
        config.background_blur = None;
        let current = load_startup_theme_with_reporter(&config, |_| {}).theme;
        assert_eq!(current.background, 1);
        assert_eq!(current.foreground, 3);
        assert!(current.background_blur);
        let preserved = current;

        std::fs::write(&path, b"{}").unwrap();
        let source = ThemeSource::Json {
            path: path.clone(),
            shell: None,
        };
        let error = live_update(&source, None, None, current).unwrap_err();
        assert_eq!(current, preserved);

        let mut diagnostics = ThemeReloadDiagnostics::default();
        assert!(diagnostics.rejected(&error).is_some());
        assert!(diagnostics.rejected(&error).is_none());
        diagnostics.accepted();
        assert!(diagnostics.rejected(&error).is_some());

        std::fs::write(&path, valid_theme_json(0.5, true)).unwrap();
        let next = live_update(&source, Some(2468), Some(false), current)
            .unwrap()
            .unwrap();
        assert_eq!(next.background_alpha, 2468);
        assert!(!next.background_blur);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn shell_toml_layers_are_fingerprinted_and_followed_live() {
        let root =
            std::env::temp_dir().join(format!("splinterm-shell-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let theme = root.join("theme");
        let user = root.join("home/.config/omarchy/shell.toml");
        write_theme(&theme, 0x10_10_10);
        let source = ThemeSource::Omarchy(
            OmarchyThemeRoots::fixed(theme.clone()).with_user_shell(user.clone()),
        );
        let mut selection = OmarchyThemeSelection::default();
        let mut current = ResolvedTheme::default();
        let mut observed = fingerprint(&source);
        let mut step = |edit: &dyn Fn(), expected: (u16, u16, Option<u16>), diagnostics: usize| {
            edit();
            let next_fingerprint = fingerprint(&source);
            assert_ne!(next_fingerprint, observed);
            observed = next_fingerprint;
            let mut reported = Vec::new();
            let active = active_theme_source(&source, &mut selection);
            if let Some(next) = resolve_live_theme_update(
                active,
                &mut selection,
                NO_OVERRIDES,
                current,
                &mut reported,
            )
            .unwrap()
            {
                current = next;
            }
            let shell = current.shell;
            assert_eq!(
                (shell.menu_alpha, shell.popup_alpha, shell.text_size_px),
                expected
            );
            assert_eq!(reported.len(), diagnostics, "{reported:?}");
        };
        let half = splinterm::config::parse("[colors]\nalpha=0.5\n")
            .unwrap()
            .config
            .background_alpha
            .unwrap();
        let rewrite = |path: &Path, contents: &str| {
            // omarchy-display-text-size writes a temporary file and renames it.
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let staged = path.with_extension("tmp");
            std::fs::write(&staged, contents).unwrap();
            std::fs::rename(&staged, path).unwrap();
        };
        step(
            &|| {
                rewrite(
                    &theme.join("shell.toml"),
                    "[menu]\nbackground-alpha = 0.5\n[font]\nbase-size = 12\n",
                );
            },
            (half, u16::MAX, Some(12)),
            0,
        );
        step(
            &|| rewrite(&user, "[font]\nbase-size = 16\n"),
            (half, u16::MAX, Some(16)),
            0,
        );
        step(
            &|| {
                rewrite(
                    &user,
                    "[font]\nbase-size = big\n[popups]\nbackground-alpha = 0.5\n",
                );
            },
            (half, half, Some(16)),
            1,
        );
        // omarchy-display-text-size reset drops the user key; the theme's
        // base-size applies again.
        step(&|| rewrite(&user, ""), (half, u16::MAX, Some(12)), 0);
        step(
            &|| std::fs::remove_file(theme.join("shell.toml")).unwrap(),
            (u16::MAX, u16::MAX, None),
            0,
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_bar_size_is_followed_live_from_the_user_shell_toml() {
        let root = std::env::temp_dir().join(format!("splinterm-bar-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let theme = root.join("theme");
        let user = root.join("home/.config/omarchy/shell.toml");
        write_theme(&theme, 0x10_10_10);
        std::fs::write(theme.join("shell.toml"), "[bar]\nsize-horizontal = 26\n").unwrap();
        let source = ThemeSource::Omarchy(
            OmarchyThemeRoots::fixed(theme.clone()).with_user_shell(user.clone()),
        );
        let mut selection = OmarchyThemeSelection::default();
        let mut current = ResolvedTheme::default();
        let mut observed = fingerprint(&source);
        let mut step = |contents: &str, expected: u16, diagnostics: usize| {
            std::fs::create_dir_all(user.parent().unwrap()).unwrap();
            let staged = user.with_extension("tmp");
            std::fs::write(&staged, contents).unwrap();
            std::fs::rename(&staged, &user).unwrap();
            let next_fingerprint = fingerprint(&source);
            assert_ne!(next_fingerprint, observed);
            observed = next_fingerprint;
            let mut reported = Vec::new();
            let active = active_theme_source(&source, &mut selection);
            if let Some(next) = resolve_live_theme_update(
                active,
                &mut selection,
                NO_OVERRIDES,
                current,
                &mut reported,
            )
            .unwrap()
            {
                current = next;
            }
            assert_eq!(current.shell.bar_size_px, expected, "{contents:?}");
            assert_eq!(reported.len(), diagnostics, "{reported:?}");
        };
        step("[bar]\nsize-horizontal = 32\n", 32, 0);
        step("[bar]\nsize-horizontal = 90\n", 48, 1);
        step("[bar]\nsize-horizontal = tall\n", 26, 1);
        step("", 26, 0);
        // omarchy-display-text-size writes base-size to the user file; the bar,
        // and the strip with it, grows by base-size / 12.
        step("[font]\nbase-size = 16\n", 35, 0);
        step("[font]\nbase-size = 20\n", 43, 0);
        step(
            "[font]\nbase-size = 20\n[bar]\nscale-with-font = off\n",
            26,
            0,
        );
        step("[font]\nbase-size = 12\n", 26, 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_bar_follows_base_size_while_config_ini_sets_the_font_size() {
        // An explicit config.ini font size stops Splinterm following the text
        // size, but the Omarchy bar still grows with base-size, so the strip
        // does too.
        let root =
            std::env::temp_dir().join(format!("splinterm-bar-explicit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let theme = root.join("theme");
        std::fs::create_dir_all(&theme).unwrap();
        std::fs::write(theme.join("shell.toml"), "[font]\nbase-size = 16\n").unwrap();
        let mut diagnostics = Vec::new();
        let explicit = ThemeOverrides {
            follow_text_size: false,
            ..NO_OVERRIDES
        };
        let resolved = with_omarchy_shell(
            ResolvedTheme::default(),
            ShellLayers {
                theme_dir: &theme,
                user: None,
                panels: true,
            },
            explicit,
            OmarchyTextSizeFallback::OmarchyDefault,
            &mut diagnostics,
        );
        let applied = explicit.apply(resolved);
        assert_eq!(applied.shell.text_size_px, None);
        // Chrome text still follows base-size.
        assert_eq!(applied.shell.ui_text_size_px, Some(16));
        assert_eq!(applied.shell.bar_size_px, 35);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The terminal's point mapping of `base-size` concerns only the
    /// terminal: with an explicit terminal font size it is not reported,
    /// though chrome still follows the px and an unparseable value still is.
    #[test]
    fn the_terminal_point_mapping_is_reported_only_while_the_terminal_follows() {
        let root =
            std::env::temp_dir().join(format!("splinterm-points-filter-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let theme = root.join("theme");
        std::fs::create_dir_all(&theme).unwrap();
        let explicit_font = ThemeOverrides {
            follow_text_size: false,
            ..NO_OVERRIDES
        };
        let reported = |contents: &str, overrides| {
            std::fs::write(theme.join("shell.toml"), contents).unwrap();
            let mut diagnostics = Vec::new();
            let resolved = with_omarchy_shell(
                ResolvedTheme::default(),
                ShellLayers {
                    theme_dir: &theme,
                    user: None,
                    panels: true,
                },
                overrides,
                OmarchyTextSizeFallback::OmarchyDefault,
                &mut diagnostics,
            );
            (resolved.shell.ui_text_size_px, diagnostics.len())
        };
        // 200 px maps to 150 pt, past the 96 pt the terminal renders.
        let huge = "[bar]\nscale-with-font = off\n[font]\nbase-size = 200\n";
        assert_eq!(reported(huge, NO_OVERRIDES), (Some(200), 1));
        assert_eq!(reported(huge, explicit_font), (Some(200), 0));
        let big = "[bar]\nscale-with-font = off\n[font]\nbase-size = big\n";
        assert_eq!(reported(big, explicit_font), (Some(12), 1));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_json_palette_follows_the_omarchy_bar_size() {
        // The bar height is not a palette setting: a JSON palette follows the
        // Omarchy bar like the native one, whether or not config.ini sets a
        // font size, while it keeps opaque panels.
        let root = std::env::temp_dir().join(format!("splinterm-bar-json-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let theme = root.join("theme");
        std::fs::create_dir_all(&theme).unwrap();
        let resolve = |contents: &str, follow_text_size: bool| {
            std::fs::write(theme.join("shell.toml"), contents).unwrap();
            let mut diagnostics = Vec::new();
            let shell = with_omarchy_shell(
                ResolvedTheme::default(),
                ShellLayers {
                    theme_dir: &theme,
                    user: None,
                    panels: false,
                },
                ThemeOverrides {
                    follow_text_size,
                    follow_ui_text_size: follow_text_size,
                    ..NO_OVERRIDES
                },
                OmarchyTextSizeFallback::OmarchyDefault,
                &mut diagnostics,
            )
            .shell;
            (shell, diagnostics)
        };
        let scaled =
            "[bar]\nsize-horizontal = 30\n[font]\nbase-size = 16\n[menu]\nbackground-alpha = 0.5\n";
        for follow_text_size in [true, false] {
            let (shell, diagnostics) = resolve(scaled, follow_text_size);
            // round(30 × 16/12) = 40 px.
            assert_eq!(shell.bar_size_px, 40, "{follow_text_size}");
            assert_eq!(shell.menu_alpha, u16::MAX, "{follow_text_size}");
            assert!(diagnostics.is_empty(), "{diagnostics:?}");
            // A rejected bar size is reported for a JSON palette too.
            let (shell, diagnostics) = resolve("[bar]\nsize-horizontal = tall\n", follow_text_size);
            assert_eq!(shell.bar_size_px, 26);
            assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn repeated_shell_diagnostics_are_reported_once_until_they_change() {
        let mut diagnostics = ThemeReloadDiagnostics::default();
        let invalid = vec!["user/shell.toml: bad".to_owned()];
        assert_eq!(diagnostics.shell(invalid.clone()).len(), 1);
        assert_eq!(diagnostics.shell(invalid.clone()), Vec::<String>::new());
        assert_eq!(diagnostics.shell(Vec::new()), Vec::<String>::new());
        assert_eq!(diagnostics.shell(invalid).len(), 1);
    }

    #[test]
    fn startup_theme_layers_shell_settings_and_reports_rejections() {
        let root =
            std::env::temp_dir().join(format!("splinterm-shell-startup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let theme = root.join("theme");
        let user = root.join("user-shell.toml");
        write_theme(&theme, 0x20_20_20);
        std::fs::write(theme.join("shell.toml"), "[font]\nbase-size = 13\n").unwrap();
        std::fs::write(&user, "[menu]\nbackground-alpha = 2\n").unwrap();
        let source = ThemeSource::Omarchy(OmarchyThemeRoots::fixed(theme).with_user_shell(user));
        let loaded = prepare_startup_theme(&source, NO_OVERRIDES);
        assert_eq!(loaded.theme.background, 0x20_20_20);
        assert_eq!(loaded.theme.shell.text_size_px, Some(13));
        assert_eq!(loaded.theme.shell.menu_alpha, u16::MAX);
        assert_eq!(loaded.diagnostics.len(), 1);
        assert!(loaded.diagnostics[0].starts_with("splinterm theme: "));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_explicit_font_size_never_follows_the_omarchy_text_size() {
        let root =
            std::env::temp_dir().join(format!("splinterm-shell-explicit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let theme = root.join("theme");
        let user = root.join("user-shell.toml");
        write_theme(&theme, 0x30_30_30);
        std::fs::write(theme.join("shell.toml"), "[font]\nbase-size = 12\n").unwrap();
        std::fs::write(&user, "[font]\nbase-size = 16\n").unwrap();
        let source =
            ThemeSource::Omarchy(OmarchyThemeRoots::fixed(theme).with_user_shell(user.clone()));
        let explicit = ThemeOverrides::from_config(
            &splinterm::config::parse("[main]\nfont-pixelsize=15\n")
                .unwrap()
                .config,
        );
        assert!(!explicit.follow_text_size);
        assert!(explicit.follow_ui_text_size);
        let startup = prepare_startup_theme(&source, explicit).theme;
        assert_eq!(startup.shell.text_size_px, None);
        // Chrome text follows base-size whatever the terminal font size.
        assert_eq!(startup.shell.ui_text_size_px, Some(16));
        assert_eq!(
            prepare_startup_theme(&source, NO_OVERRIDES)
                .theme
                .shell
                .text_size_px,
            Some(16)
        );
        // The Omarchy bar still grows with base-size, so a base-size edit
        // publishes the new bar height and chrome size, never the terminal's
        // text size.
        assert_eq!(startup.shell.bar_size_px, 35);
        std::fs::write(&user, "[font]\nbase-size = 18\n").unwrap();
        let mut selection = OmarchyThemeSelection::default();
        let active = active_theme_source(&source, &mut selection);
        let next =
            resolve_live_theme_update(active, &mut selection, explicit, startup, &mut Vec::new())
                .unwrap()
                .expect("the bar follows base-size");
        assert_eq!(next.shell.text_size_px, None);
        assert_eq!(next.shell.ui_text_size_px, Some(18));
        assert_eq!(next.shell.bar_size_px, 39);
        // With the bar's scale-with-font off, a base-size edit still
        // publishes the chrome size, and only that, while config.ini owns
        // the terminal size.
        std::fs::write(
            &user,
            "[bar]\nscale-with-font = off\n[font]\nbase-size = 18\n",
        )
        .unwrap();
        let unscaled = prepare_startup_theme(&source, explicit).theme;
        assert_eq!(unscaled.shell.bar_size_px, 26);
        std::fs::write(
            &user,
            "[bar]\nscale-with-font = off\n[font]\nbase-size = 20\n",
        )
        .unwrap();
        let active = active_theme_source(&source, &mut selection);
        let next =
            resolve_live_theme_update(active, &mut selection, explicit, unscaled, &mut Vec::new())
                .unwrap()
                .expect("chrome follows base-size");
        assert_eq!(
            next.shell,
            OmarchyShellSettings {
                ui_text_size_px: Some(20),
                ..unscaled.shell
            }
        );
        // An unparseable edit keeps the chrome size in use.
        std::fs::write(
            &user,
            "[bar]\nscale-with-font = off\n[font]\nbase-size = big\n",
        )
        .unwrap();
        let active = active_theme_source(&source, &mut selection);
        let mut reported = Vec::new();
        let held = resolve_live_theme_update(active, &mut selection, explicit, next, &mut reported)
            .unwrap();
        assert!(held.is_none(), "{held:?}");
        assert_eq!(reported.len(), 1, "chrome follows, so it is reported");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_explicit_json_palette_follows_only_the_shell_text_size() {
        let root =
            std::env::temp_dir().join(format!("splinterm-shell-json-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let theme = root.join("theme");
        let user = root.join("user-shell.toml");
        let json = root.join("theme.json");
        std::fs::create_dir_all(&theme).unwrap();
        std::fs::write(&json, valid_theme_json(1.0, false)).unwrap();
        std::fs::write(
            theme.join("shell.toml"),
            "[menu]\nbackground-alpha = 0.5\n[font]\nbase-size = 12\n",
        )
        .unwrap();
        let source = ThemeSource::Json {
            path: json,
            shell: Some(OmarchyThemeRoots::fixed(theme).with_user_shell(user.clone())),
        };
        let startup = prepare_startup_theme(&source, NO_OVERRIDES).theme;
        assert_eq!(startup.background, 1);
        assert_eq!(
            startup.shell,
            OmarchyShellSettings {
                text_size_px: Some(12),
                ui_text_size_px: Some(12),
                ..OmarchyShellSettings::default()
            }
        );
        let before = fingerprint(&source);
        std::fs::write(&user, "[font]\nbase-size = 16\n").unwrap();
        assert_ne!(fingerprint(&source), before);
        let next = live_update(&source, None, None, startup)
            .unwrap()
            .expect("a text size edit publishes an update");
        assert_eq!(next.shell.text_size_px, Some(16));
        assert_eq!(next.shell.menu_alpha, u16::MAX);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_first_live_poll_does_not_repeat_startup_shell_diagnostics() {
        let root =
            std::env::temp_dir().join(format!("splinterm-shell-seed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let theme = root.join("theme");
        let user = root.join("user-shell.toml");
        write_theme(&theme, 0x40_40_40);
        std::fs::write(&user, "[menu]\nbackground-alpha = opaque\n").unwrap();
        let source =
            ThemeSource::Omarchy(OmarchyThemeRoots::fixed(theme).with_user_shell(user.clone()));
        let loaded = prepare_startup_theme(&source, NO_OVERRIDES);
        assert_eq!(loaded.diagnostics.len(), 1, "startup prints the problem");
        let startup = StartupTheme::from(loaded);
        let mut diagnostics = ThemeReloadDiagnostics::after_startup(&startup);

        // The watcher's first tick fires at once with nothing observed yet and
        // reloads the same files.
        let mut selection = OmarchyThemeSelection::default();
        let mut reported = Vec::new();
        let active = active_theme_source(&source, &mut selection);
        assert!(
            resolve_live_theme_update(
                active,
                &mut selection,
                NO_OVERRIDES,
                startup.theme,
                &mut reported
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(reported.len(), 1);
        assert_eq!(diagnostics.shell(reported), Vec::<String>::new());

        // A new problem is still reported.
        std::fs::write(&user, "[menu]\nbackground-alpha = never\n").unwrap();
        let mut reported = Vec::new();
        let active = active_theme_source(&source, &mut selection);
        resolve_live_theme_update(
            active,
            &mut selection,
            NO_OVERRIDES,
            startup.theme,
            &mut reported,
        )
        .unwrap();
        assert_eq!(diagnostics.shell(reported).len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn diagnostics_for_ignored_shell_settings_are_dropped() {
        let root =
            std::env::temp_dir().join(format!("splinterm-shell-ignored-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let theme = root.join("theme");
        let user = root.join("user-shell.toml");
        let json = root.join("theme.json");
        write_theme(&theme, 0x50_50_50);
        std::fs::write(&json, valid_theme_json(1.0, false)).unwrap();
        std::fs::write(
            &user,
            "[menu]\nbackground-alpha = opaque\n[font]\nbase-size = big\n[bar]\nsize-horizontal = tall\n",
        )
        .unwrap();
        let roots = OmarchyThemeRoots::fixed(theme).with_user_shell(user);
        let explicit_font = ThemeOverrides {
            follow_text_size: false,
            ..NO_OVERRIDES
        };
        let explicit = ThemeOverrides {
            follow_text_size: false,
            follow_ui_text_size: false,
            ..NO_OVERRIDES
        };
        let omarchy = ThemeSource::Omarchy(roots.clone());
        let json_source = ThemeSource::Json {
            path: json,
            shell: Some(roots),
        };
        // Every palette follows, and so reports, the bar size.
        for (source, overrides, expected) in [
            // All three settings are followed and all are reported.
            (&omarchy, NO_OVERRIDES, 3),
            // An explicit terminal font size alone: chrome still follows
            // the text size, so it is still reported.
            (&omarchy, explicit_font, 3),
            // Explicit terminal and UI font sizes ignore the text size.
            (&omarchy, explicit, 2),
            // A JSON palette ignores panel alpha.
            (&json_source, NO_OVERRIDES, 2),
            (&json_source, explicit_font, 2),
            // A JSON palette with both sizes explicit ignores both, but not
            // the bar.
            (&json_source, explicit, 1),
        ] {
            let loaded = prepare_startup_theme(source, overrides);
            assert_eq!(loaded.shell_diagnostics.len(), expected, "{overrides:?}");
            let mut selection = OmarchyThemeSelection::default();
            let mut reported = Vec::new();
            let active = active_theme_source(source, &mut selection);
            resolve_live_theme_update(
                active,
                &mut selection,
                overrides,
                ResolvedTheme::default(),
                &mut reported,
            )
            .unwrap();
            assert_eq!(reported.len(), expected, "{overrides:?}");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_json_palette_follows_shell_roots_with_the_native_demotion_grace() {
        let root =
            std::env::temp_dir().join(format!("splinterm-shell-json-roots-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let high = root.join("state/omarchy/current/theme");
        let low = root.join("legacy/omarchy/current/theme");
        let json = root.join("theme.json");
        for (theme, size) in [(&high, 16), (&low, 10)] {
            std::fs::create_dir_all(theme).unwrap();
            std::fs::write(
                theme.join("shell.toml"),
                format!("[font]\nbase-size = {size}\n"),
            )
            .unwrap();
        }
        std::fs::write(&json, valid_theme_json(1.0, false)).unwrap();
        let source = ThemeSource::Json {
            path: json,
            shell: OmarchyThemeRoots::ordered(vec![high.clone(), low]),
        };
        let mut selection = OmarchyThemeSelection::default();
        let mut current = ResolvedTheme::default();
        let mut poll = |current: &mut ResolvedTheme| {
            let active = active_theme_source(&source, &mut selection);
            if let Some(next) = resolve_live_theme_update(
                active,
                &mut selection,
                NO_OVERRIDES,
                *current,
                &mut Vec::new(),
            )
            .unwrap()
            {
                *current = next;
            }
            current.shell.text_size_px
        };
        assert_eq!(poll(&mut current), Some(16));

        // The higher root vanishes briefly: its size is kept through the grace
        // polls rather than snapping to the stale lower root.
        let displaced = root.join("displaced");
        std::fs::rename(&high, &displaced).unwrap();
        for _ in 0..OMARCHY_ROOT_DEMOTION_GRACE_POLLS {
            assert_eq!(poll(&mut current), Some(16));
        }
        std::fs::rename(&displaced, &high).unwrap();
        assert_eq!(poll(&mut current), Some(16));

        // A sustained removal is accepted after the grace polls.
        std::fs::rename(&high, &displaced).unwrap();
        for _ in 0..OMARCHY_ROOT_DEMOTION_GRACE_POLLS {
            assert_eq!(poll(&mut current), Some(16));
        }
        assert_eq!(poll(&mut current), Some(10));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_invalid_live_base_size_keeps_the_configured_fallback_in_use() {
        let root =
            std::env::temp_dir().join(format!("splinterm-shell-fallback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let theme = root.join("theme");
        let user = root.join("user-shell.toml");
        write_theme(&theme, 0x60_60_60);
        let source =
            ThemeSource::Omarchy(OmarchyThemeRoots::fixed(theme).with_user_shell(user.clone()));
        // No shell size anywhere: the window renders at Splinterm's fallback.
        let startup = prepare_startup_theme(&source, NO_OVERRIDES).theme;
        assert_eq!(startup.shell.text_size_px, None);

        // Adding an unparseable base-size live keeps that fallback rather than
        // switching to the startup-only 12 px default.
        std::fs::write(&user, "[font]\nbase-size = big\n").unwrap();
        let mut selection = OmarchyThemeSelection::default();
        let mut reported = Vec::new();
        let active = active_theme_source(&source, &mut selection);
        // The published update only marks the size as held; no shell size is
        // followed, so the window keeps rendering at its configured fallback.
        let next =
            resolve_live_theme_update(active, &mut selection, NO_OVERRIDES, startup, &mut reported)
                .unwrap()
                .expect("the held marker is published");
        assert_eq!(next.shell.text_size_px, None);
        assert!(next.shell.text_size_held);
        assert_eq!(
            next,
            ResolvedTheme {
                shell: OmarchyShellSettings {
                    text_size_held: true,
                    ..startup.shell
                },
                ..startup
            },
            "nothing but the held marker changes"
        );
        assert_eq!(reported.len(), 1);

        // The same file at startup uses Omarchy's 12 px, as the shell does.
        assert_eq!(
            prepare_startup_theme(&source, NO_OVERRIDES)
                .theme
                .shell
                .text_size_px,
            Some(12)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_json_palette_keeps_shell_state_while_every_root_is_missing() {
        let root = std::env::temp_dir().join(format!(
            "splinterm-shell-json-outage-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let high = root.join("state/omarchy/current/theme");
        let low = root.join("legacy/omarchy/current/theme");
        let json = root.join("theme.json");
        std::fs::create_dir_all(&high).unwrap();
        std::fs::write(high.join("shell.toml"), "[font]\nbase-size = 16\n").unwrap();
        std::fs::write(&json, valid_theme_json(1.0, false)).unwrap();
        let source = ThemeSource::Json {
            path: json,
            shell: OmarchyThemeRoots::ordered(vec![high.clone(), low]),
        };
        let mut selection = OmarchyThemeSelection::default();
        let mut current = ResolvedTheme::default();
        let mut poll = |current: &mut ResolvedTheme| {
            let active = active_theme_source(&source, &mut selection);
            if let Some(next) = resolve_live_theme_update(
                active,
                &mut selection,
                NO_OVERRIDES,
                *current,
                &mut Vec::new(),
            )
            .unwrap()
            {
                *current = next;
            }
            current.shell.text_size_px
        };
        assert_eq!(poll(&mut current), Some(16));

        // Every root disappears, so nothing is claimed; the selection holds
        // the last good root, and the followed size is kept like the native
        // palette rather than reset to defaults.
        let displaced = root.join("displaced");
        std::fs::rename(&high, &displaced).unwrap();
        for _ in 0..=OMARCHY_ROOT_DEMOTION_GRACE_POLLS * 2 {
            assert_eq!(poll(&mut current), Some(16));
        }
        std::fs::rename(&displaced, &high).unwrap();
        std::fs::write(high.join("shell.toml"), "[font]\nbase-size = 18\n").unwrap();
        assert_eq!(poll(&mut current), Some(18));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_json_shell_root_demotion_is_observed_without_any_shell_toml() {
        let root = std::env::temp_dir().join(format!(
            "splinterm-shell-json-demote-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let high = root.join("state/omarchy/current/theme");
        let low = root.join("legacy/omarchy/current/theme");
        let json = root.join("theme.json");
        std::fs::create_dir_all(&high).unwrap();
        std::fs::write(high.join("shell.toml"), "[font]\nbase-size = 16\n").unwrap();
        // An Omarchy 3.x root has no shell.toml.
        std::fs::create_dir_all(&low).unwrap();
        std::fs::write(&json, valid_theme_json(1.0, false)).unwrap();
        let source = ThemeSource::Json {
            path: json,
            shell: OmarchyThemeRoots::ordered(vec![high.clone(), low]),
        };
        // Poll as watch_theme does: resolve only when the fingerprint changes.
        let mut selection = OmarchyThemeSelection::default();
        let mut observed = None;
        let mut current = ResolvedTheme::default();
        let mut poll = |current: &mut ResolvedTheme| {
            let active = active_theme_source(&source, &mut selection);
            let fingerprint = theme_source_fingerprint(active);
            if observed.as_ref() != Some(&fingerprint) {
                observed = Some(fingerprint);
                if let Some(next) = resolve_live_theme_update(
                    active,
                    &mut selection,
                    NO_OVERRIDES,
                    *current,
                    &mut Vec::new(),
                )
                .unwrap()
                {
                    *current = next;
                }
            }
            current.shell.text_size_px
        };
        assert_eq!(poll(&mut current), Some(16));

        // The higher root is held through the grace polls...
        std::fs::remove_dir_all(&high).unwrap();
        for _ in 0..OMARCHY_ROOT_DEMOTION_GRACE_POLLS {
            assert_eq!(poll(&mut current), Some(16));
        }
        // ...then the demotion to the shell-less lower root is observed and
        // its (absent) base-size applies, so Splinterm's fallback returns.
        assert_eq!(poll(&mut current), None);
        assert_eq!(poll(&mut current), None);
        std::fs::remove_dir_all(root).unwrap();
    }
}
