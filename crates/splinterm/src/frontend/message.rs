//! Bounded messages exchanged by the application owner and graphical adapter.

use std::sync::Arc;

use splinterm_automation_client::ImageContentLeaseSet;
use splinterm_core::SplintId;
use splinterm_protocol::{
    ControlTransferDecision, ControlTransferOutcome, SearchPage, TerminalSnapshot, TerminalUpdate,
};

use crate::{
    config::{ResolvedTheme, ThemeColorConfig},
    renderer::FontGeneration,
    theme::ThemeInput,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthorityStatus {
    pub grants: Vec<(u64, String)>,
    pub development_bypass: bool,
}

/// Body-free identity that correlates one protocol update with graphical work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PerfTraceCorrelation {
    pub base_revision: u64,
    pub revision: u64,
    pub subscription_id: u64,
    pub transaction_sequence: u64,
}

/// Bounded protocol-to-Wayland messages for the live snapshot viewer.
#[allow(
    clippy::large_enum_variant,
    reason = "the queue is bounded and owned snapshots avoid a second allocation"
)]
#[derive(Debug)]
pub enum WindowUpdate {
    Snapshot {
        snapshot: TerminalSnapshot,
        image_sources: ImageContentLeaseSet,
        authoritative: bool,
    },
    Update {
        update: TerminalUpdate,
        image_sources: Option<ImageContentLeaseSet>,
        trace: Option<PerfTraceCorrelation>,
    },
    ScrollbackPages(Vec<splinterm_protocol::ScrollbackPage>),
    ScrollbackResyncRequired,
    Authority(AuthorityStatus),
    Control(bool),
    ControlTransferRequested(u64),
    ControlTransferResolved(ControlTransferOutcome),
    SearchResults(SearchPage),
    SearchResyncRequired,
    Theme(ThemeUpdate),
    Font(FontUpdate),
    Exited {
        splint_id: SplintId,
    },
    Shutdown,
    /// Plan 0073: the pane's display stream was lost a while ago, and is
    /// `reconnecting` or has stopped trying; the pane says so until an
    /// authoritative snapshot arrives.
    StreamLost {
        reconnecting: bool,
    },
    /// Plan 0074: the daemon's input queue for the pane was full, so the
    /// rest of some input, such as a large paste, was dropped.
    InputDropped,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThemeUpdate {
    pub generation: u64,
    pub theme: ResolvedTheme,
    /// The `[colors]` keys `theme` was resolved with. A Window whose own
    /// keys differ resolves `input` again with them.
    pub colors: ThemeColorConfig,
    /// What the theme source gave; `None` when the sender has no input,
    /// and the Window then shows `theme` as given.
    pub input: Option<Arc<ThemeInput>>,
}

/// A Window's `config.ini` `[colors]` keys, and what it needs to resolve
/// its theme with them again.
///
/// The theme watcher resolves with the keys of the configuration it started
/// from, without role keys. The Window resolves every theme it shows with its
/// own keys, which a configuration reload replaces, from the input the theme
/// came from. A key the reload removed gives the theme's own value back.
#[derive(Clone, Debug, Default)]
pub struct WindowThemeColors {
    /// This Window's keys.
    colors: ThemeColorConfig,
    /// The keys the Window's theme was resolved with.
    applied: ThemeColorConfig,
    /// What the theme source last gave, when the Window knows it.
    input: Option<Arc<ThemeInput>>,
}

impl WindowThemeColors {
    /// The keys `colors`, which the Window's first theme was resolved with
    /// from `input`.
    #[must_use]
    pub fn new(colors: ThemeColorConfig, input: Option<Arc<ThemeInput>>) -> Self {
        Self {
            applied: colors.clone(),
            colors,
            input,
        }
    }

    /// `update`'s theme, resolved with this Window's keys when it carries
    /// its input; else as given.
    #[must_use]
    pub fn accept(&mut self, update: ThemeUpdate) -> ResolvedTheme {
        let Some(input) = update.input else {
            // The input no longer matches the theme shown, so no reload may
            // resolve that older theme again.
            self.applied = update.colors;
            self.input = None;
            return update.theme;
        };
        let theme = update.theme.recolored(&input, &update.colors, &self.colors);
        self.applied = self.colors.clone();
        self.input = Some(input);
        theme
    }

    /// Takes a reloaded configuration's keys, which
    /// [`Self::recolor`] applies.
    pub fn reload(&mut self, colors: ThemeColorConfig) {
        self.colors = colors;
    }

    /// `current` resolved again with keys a reload changed; `None` when they
    /// did not change, or the input is unknown.
    #[must_use]
    pub fn recolor(&mut self, current: ResolvedTheme) -> Option<ResolvedTheme> {
        if self.applied == self.colors {
            return None;
        }
        let input = self.input.as_deref()?;
        let theme = current.recolored(input, &self.applied, &self.colors);
        self.applied = self.colors.clone();
        Some(theme)
    }
}

#[derive(Clone, Debug)]
pub struct FontUpdate {
    pub generation: Arc<FontGeneration>,
}

/// Bounded Wayland-to-protocol commands for the first interactive slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowCommand {
    Input(Vec<u8>),
    /// A DECSET 1004 focus in (`true`) or out report this Window generated.
    /// Kept apart from [`WindowCommand::Input`] so the daemon can tell it
    /// from identical bytes the user typed or pasted.
    FocusReport(bool),
    Resynchronize,
    Resize {
        columns: u16,
        rows: u16,
        pixel_width: u16,
        pixel_height: u16,
    },
    PrepareResize {
        columns: u16,
        rows: u16,
        pixel_width: u16,
        pixel_height: u16,
    },
    FetchScrollback {
        splint_id: SplintId,
        incarnation: u64,
        terminal_revision: u64,
        history_generation: u64,
        before_row_id: u64,
    },
    RevokeAccess(u64),
    /// **Revoke all access** also ends every pending workspace access
    /// request (ADR 0014).
    CancelPendingAccess,
    RequestControlTransfer,
    DecideControlTransfer {
        transfer_id: u64,
        decision: ControlTransferDecision,
    },
    ForceControlTransfer,
    Search {
        terminal_revision: u64,
        history_generation: u64,
        query: String,
        case_sensitive: bool,
        cursor: Option<String>,
    },
    ReleaseControl,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colors(text: &str) -> ThemeColorConfig {
        crate::config::parse(text).unwrap().config.theme_colors()
    }

    #[test]
    fn a_reload_resolves_the_window_theme_again_and_a_removed_key_restores_the_theme() {
        let input = Arc::new(ThemeInput::builtin());
        let themed = ResolvedTheme::from_input(&input);
        let mut window = WindowThemeColors::new(ThemeColorConfig::NONE, Some(Arc::clone(&input)));
        assert_eq!(window.recolor(themed), None, "no reload, nothing to do");

        let set = colors(
            "[colors]\naccent=#ff0000\nalpha=0.5\nblur=yes\nactive-tab-background=#102030\n",
        );
        window.reload(set.clone());
        let overridden = window.recolor(themed).expect("the reload changed the keys");
        assert_eq!(overridden.roles.accent, 0xff_00_00);
        assert_eq!(overridden.roles.focus_ring, 0xff_00_00);
        assert_eq!(Some(overridden.background_alpha), set.alpha);
        assert!(overridden.background_blur);
        assert_eq!(overridden.active_tab_background, 0x10_20_30);
        assert_eq!(window.recolor(overridden), None, "applied once");

        // A reload that removes every key gives the theme's own values back.
        window.reload(ThemeColorConfig::NONE);
        assert_eq!(window.recolor(overridden), Some(themed));
    }

    #[test]
    fn a_window_without_its_theme_input_keeps_the_theme_it_has() {
        let themed = ResolvedTheme::default();
        let mut window = WindowThemeColors::new(ThemeColorConfig::NONE, None);
        window.reload(colors("[colors]\naccent=#ff0000\n"));
        assert_eq!(window.recolor(themed), None);
    }

    #[test]
    fn a_watcher_update_is_shown_with_the_window_keys() {
        let input = Arc::new(ThemeInput::builtin());
        // The watcher resolves with the keys it started from, without role
        // keys; this Window's reloaded keys set a role and drop alpha.
        let watched = colors("[colors]\nalpha=0.5\n");
        let mut theme = ResolvedTheme::from_input_with_colors(&input, &watched);
        theme.shell.menu_alpha = 7;
        let mut window = WindowThemeColors::new(colors("[colors]\ndanger=#123456\n"), None);
        let shown = window.accept(ThemeUpdate {
            generation: 1,
            theme,
            colors: watched.clone(),
            input: Some(Arc::clone(&input)),
        });
        assert_eq!(shown.roles.danger, 0x12_34_56);
        assert_eq!(shown.roles.status_error, 0x12_34_56);
        assert_eq!(shown.background_alpha, u16::MAX);
        assert_eq!(shown.shell.menu_alpha, 7, "the watcher's shell layers stay");
        assert_eq!(window.recolor(shown), None);

        // Keys equal to the watcher's leave its theme untouched.
        let mut same = WindowThemeColors::new(watched.clone(), None);
        let update = ThemeUpdate {
            generation: 2,
            theme,
            colors: watched.clone(),
            input: Some(Arc::clone(&input)),
        };
        assert_eq!(same.accept(update), theme);

        // A sender without input is shown as given, and forgets the input
        // of the theme before it: a later reload resolves nothing again.
        let update = ThemeUpdate {
            generation: 3,
            theme,
            colors: watched,
            input: None,
        };
        let mut forgetting =
            WindowThemeColors::new(ThemeColorConfig::NONE, Some(Arc::clone(&input)));
        assert_eq!(forgetting.accept(update), theme);
        forgetting.reload(colors("[colors]\naccent=#ff0000\n"));
        assert_eq!(forgetting.recolor(theme), None);
    }
}
