//! Platform-independent construction options for graphical windows and panes.

use std::{path::PathBuf, sync::mpsc::Sender as StdSender, time::Instant};

use tokio::sync::{
    mpsc::{Receiver, Sender},
    oneshot,
    watch::Sender as WatchSender,
};

use splinterd::dialog_shortcut::DialogShortcut;
use splinterm_automation_client::ImageContentLeaseSet;
use splinterm_core::{LayoutNode, SplintId};
use splinterm_protocol::{ConsentDecision, GrantDuration, MAX_COLUMNS, MAX_ROWS, TerminalSnapshot};

use crate::{
    config::{
        APP_ID, CursorStyle, FrameTitleMode, PaneDividerStyle, ResolvedTheme,
        TopologyReloadSettings,
    },
    geometry::FontSize,
    keymap::ResolvedKeymap,
};

use super::{
    AuthorityStatus, SessionPickerUi, TrustedDialogKey, TrustedDialogKind, WindowCommand,
    WindowDojoIdentity, WindowTopologyCommand, WindowTopologyUpdate, WindowUpdate,
    dialog_text::{self, DialogIdentity, DialogText},
    trusted_dialog::{AccessChoice, ConsentAction, Engagement},
};

/// The access consent dialog's text with the duration at this index
/// selected. Lines that show a duration carry its index
/// ([`super::DialogLine::with_duration`]), so a click on any of their rows
/// selects it.
pub type AccessConsentRenderer = Box<dyn Fn(usize) -> DialogText + Send>;

/// Where a trusted dialog's text is laid out: the columns it wraps at, the
/// grid its snapshot fills, and the most rows of text that fit above the
/// action areas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DialogFrame {
    pub(crate) wrap_columns: usize,
    pub(crate) grid_columns: usize,
    pub(crate) grid_rows: usize,
    pub(crate) max_text_rows: usize,
}

/// What a key press or click did to a trusted dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrustedInput {
    /// The person answered: `true` grants or proceeds.
    Answer(bool),
    /// The selected duration changed; the dialog must be drawn again.
    Redraw,
}

enum TrustedAnswer {
    YesNo(StdSender<bool>),
    Consent(StdSender<ConsentDecision>),
}

struct AccessConsentDialog {
    choice: AccessChoice,
    durations: Vec<GrantDuration>,
    /// The rows of the current layout that show a duration.
    duration_rows: Vec<(usize, usize)>,
    render: AccessConsentRenderer,
}

/// A trusted dialog Window's answer channel and kind. Build it with
/// [`TrustedConsentUi::access_consent`], [`TrustedConsentUi::upgrade`], or
/// [`TrustedConsentUi::restart`].
pub struct TrustedConsentUi {
    answer: TrustedAnswer,
    pub(crate) kind: TrustedDialogKind,
    /// Whether a click inside engaged the yes/no questions' keyboard, and
    /// since when they arm. The access consent dialog keeps its own.
    engagement: Engagement,
    access: Option<AccessConsentDialog>,
    /// The chord that engages the keyboard as a click inside does, or
    /// `None` when the dialog shortcut is off.
    shortcut: Option<DialogShortcut>,
    /// The yes/no questions' text; the access consent dialog renders its
    /// own for each selection.
    text: Option<DialogText>,
    identity: DialogIdentity,
    /// Where the text was last laid out.
    frame: Option<DialogFrame>,
    /// Whether that layout hides part of the text. While it does, nothing
    /// grants, proceeds, or restarts; only the no answers count.
    clipped: bool,
}

impl TrustedConsentUi {
    /// The access consent dialog (ADR 0014): default-deny, inert until a
    /// click inside engages it, then armed after that click, every focus
    /// gain, and every duration change, with the durations the prompt shows
    /// (`true` when offered; the first is "Just this once") and a renderer
    /// of its text for each selection.
    #[must_use]
    pub fn access_consent(
        decision: StdSender<ConsentDecision>,
        durations: Vec<(GrantDuration, bool)>,
        render: AccessConsentRenderer,
    ) -> Self {
        let (durations, offered) = durations.into_iter().unzip();
        Self {
            answer: TrustedAnswer::Consent(decision),
            kind: TrustedDialogKind::AccessConsent,
            engagement: Engagement::default(),
            access: Some(AccessConsentDialog {
                choice: AccessChoice::new(offered),
                durations,
                duration_rows: Vec::new(),
                render,
            }),
            shortcut: None,
            text: None,
            identity: DialogIdentity::default(),
            frame: None,
            clipped: false,
        }
    }

    /// The upgrade question: only `G` or the green area proceeds, after an
    /// arming delay; `Enter`, `Escape` and `D` cancel.
    #[must_use]
    pub fn upgrade(decision: StdSender<bool>) -> Self {
        Self {
            answer: TrustedAnswer::YesNo(decision),
            kind: TrustedDialogKind::Upgrade,
            engagement: Engagement::default(),
            access: None,
            shortcut: None,
            text: None,
            identity: DialogIdentity::default(),
            frame: None,
            clipped: false,
        }
    }

    /// The launcher's restart question: only `R` or the green area restarts,
    /// after the same arming delay; `Enter`, `Escape` and `D` answer "Not now".
    #[must_use]
    pub fn restart(decision: StdSender<bool>) -> Self {
        Self {
            answer: TrustedAnswer::YesNo(decision),
            kind: TrustedDialogKind::Restart,
            engagement: Engagement::default(),
            access: None,
            shortcut: None,
            text: None,
            identity: DialogIdentity::default(),
            frame: None,
            clipped: false,
        }
    }

    /// The dialog shortcut (`[integration] dialog-shortcut`): pressed while
    /// the dialog has keyboard focus, it engages the keyboard as a click
    /// inside does. Without this, only a click engages.
    #[must_use]
    pub fn with_shortcut(mut self, shortcut: Option<DialogShortcut>) -> Self {
        self.shortcut = shortcut;
        self
    }

    /// The text a yes/no question shows. The access consent dialog renders
    /// its own and ignores this.
    #[must_use]
    pub fn with_text(mut self, text: DialogText) -> Self {
        self.text = Some(text);
        self
    }

    /// The title and Splint identity the dialog's snapshots carry.
    #[must_use]
    pub fn with_identity(mut self, identity: DialogIdentity) -> Self {
        self.identity = identity;
        self
    }

    /// The Window that shows this dialog at its natural size
    /// ([`dialog_text::natural_columns`], [`dialog_text::natural_rows`]):
    /// the size it asks the compositor for as both its minimum and maximum.
    #[must_use]
    pub fn into_window_options(mut self) -> WindowOptions {
        let snapshot = self
            .natural_frame()
            .and_then(|frame| self.lay_out(frame, Instant::now()));
        let size = |value: Option<usize>| {
            value.map_or(0, |value| u16::try_from(value).unwrap_or(u16::MAX))
        };
        let initial_columns = size(snapshot.as_ref().map(|snapshot| snapshot.columns));
        let initial_rows = size(snapshot.as_ref().map(|snapshot| snapshot.rows));
        let mut options = WindowOptions {
            snapshot,
            trusted_consent: Some(self),
            ..WindowOptions::default()
        };
        if initial_columns > 0 && initial_rows > 0 {
            options.initial_columns = initial_columns;
            options.initial_rows = initial_rows;
        }
        options
    }

    /// Every rendering of the dialog's text: one per access consent
    /// selection, or the yes/no question's one. Empty without text.
    pub(crate) fn variants(&self) -> Vec<DialogText> {
        match &self.access {
            Some(access) => (0..access.durations.len().max(1))
                .map(|index| (access.render)(index))
                .collect(),
            None => self.text.iter().cloned().collect(),
        }
    }

    fn selected_variant(&self) -> usize {
        self.access
            .as_ref()
            .map_or(0, |access| access.choice.selected())
    }

    /// The dialog's natural layout: every row, at the size it asks for.
    pub(crate) fn natural_frame(&self) -> Option<DialogFrame> {
        let variants = self.variants();
        if variants.is_empty() {
            return None;
        }
        let columns = dialog_text::natural_columns(&variants);
        Some(DialogFrame {
            wrap_columns: columns,
            grid_columns: columns,
            grid_rows: dialog_text::natural_rows(&variants),
            max_text_rows: usize::MAX,
        })
    }

    /// Lays the current selection's text out in `frame` and returns its
    /// snapshot; later redraws keep that frame. `None` without text.
    ///
    /// A layout that hides part of the text shows
    /// [`TrustedDialogKind::clipped_notice`] in place of the answer hints
    /// and makes every yes answer inert. When the whole text shows again,
    /// an engaged dialog arms again from `now`, so a yes counts only once
    /// the whole text has been in view for the arming delay.
    pub(crate) fn lay_out(&mut self, frame: DialogFrame, now: Instant) -> Option<TerminalSnapshot> {
        let variants = self.variants();
        if variants.is_empty() {
            return None;
        }
        let layout = dialog_text::layout_with_notice(
            &variants,
            self.selected_variant(),
            frame.wrap_columns,
            frame.max_text_rows,
            Some(self.kind.clipped_notice()),
        );
        if let Some(access) = self.access.as_mut() {
            access.duration_rows.clone_from(&layout.duration_rows);
        }
        if self.clipped && !layout.clipped {
            self.rearm(now);
        }
        self.clipped = layout.clipped;
        self.frame = Some(frame);
        Some(dialog_text::dialog_snapshot(
            layout.rows,
            frame.grid_columns,
            frame.grid_rows,
            &self.identity,
        ))
    }

    /// Lays the text out for a new Window size, scale, or font
    /// ([`Self::lay_out`]) and arms an engaged dialog again from `now`:
    /// the action areas and duration rows may have moved under the pointer.
    pub(crate) fn lay_out_again(
        &mut self,
        frame: DialogFrame,
        now: Instant,
    ) -> Option<TerminalSnapshot> {
        let snapshot = self.lay_out(frame, now)?;
        self.rearm(now);
        Some(snapshot)
    }

    /// Whether the current layout hides part of the text, so no yes answer
    /// counts.
    pub(crate) const fn clipped(&self) -> bool {
        self.clipped
    }

    /// Starts an engaged dialog's arming delay again; never engages one.
    fn rearm(&mut self, now: Instant) {
        self.engagement.rearm(now);
        if let Some(access) = self.access.as_mut() {
            access.choice.rearm(now);
        }
    }

    /// The chord that engages this dialog, if any.
    #[must_use]
    pub const fn shortcut(&self) -> Option<DialogShortcut> {
        self.shortcut
    }

    /// Records keyboard focus. Focus never engages a dialog; an engaged one
    /// arms again.
    pub(crate) fn note_focus(&mut self, now: Instant) {
        self.engagement.note_focus(now);
        if let Some(access) = self.access.as_mut() {
            access.choice.note_focus(now);
        }
    }

    /// A pointer press inside the dialog, or the dialog shortcut while it
    /// has focus: the first one engages its keyboard, which then arms.
    /// Nothing else does.
    pub(crate) fn engage(&mut self, now: Instant) {
        self.engagement.engage(now);
        if let Some(access) = self.access.as_mut() {
            access.choice.engage(now);
        }
    }

    fn consent_input(
        &mut self,
        action: Option<ConsentAction>,
        now: Instant,
    ) -> Option<TrustedInput> {
        let clipped = self.clipped;
        let access = self.access.as_mut()?;
        match action? {
            ConsentAction::Deny => Some(TrustedInput::Answer(false)),
            // Nothing grants while part of the request is hidden.
            ConsentAction::Grant => (!clipped).then_some(TrustedInput::Answer(true)),
            ConsentAction::Select(index) => access
                .choice
                .select(index, now)
                .then_some(TrustedInput::Redraw),
        }
    }

    pub(crate) fn key_input(
        &mut self,
        key: TrustedDialogKey,
        repeat: bool,
        now: Instant,
    ) -> Option<TrustedInput> {
        if key == TrustedDialogKey::Engage {
            // The dialog shortcut engages as a click inside does and never
            // answers; the arming delay then applies as usual.
            if !repeat {
                self.engage(now);
            }
            return None;
        }
        if let Some(access) = &self.access {
            let action = access.choice.key(key, repeat, now);
            return self.consent_input(action, now);
        }
        self.kind
            .key_decision(key, repeat, self.engagement, now)
            .filter(|proceed| !(*proceed && self.clipped))
            .map(TrustedInput::Answer)
    }

    /// A click in the bottom action areas: the right one grants or proceeds.
    pub(crate) fn action_area_input(
        &mut self,
        proceed_area: bool,
        now: Instant,
    ) -> Option<TrustedInput> {
        if let Some(access) = &self.access {
            let action = access.choice.click_action_area(proceed_area, now);
            return self.consent_input(action, now);
        }
        self.kind
            .click_decision(proceed_area, self.engagement, now)
            .filter(|proceed| !(*proceed && self.clipped))
            .map(TrustedInput::Answer)
    }

    /// A click on a snapshot row: selects the duration shown there.
    pub(crate) fn row_input(&mut self, row: usize, now: Instant) -> Option<TrustedInput> {
        let access = self.access.as_ref()?;
        let index = access
            .duration_rows
            .iter()
            .find_map(|(shown, index)| (*shown == row).then_some(*index))?;
        let action = access.choice.click_duration(index);
        self.consent_input(action, now)
    }

    /// The access consent dialog rendered for its current selection, laid
    /// out where it was last.
    pub(crate) fn render(&mut self, now: Instant) -> Option<TerminalSnapshot> {
        self.access.as_ref()?;
        let frame = self.frame.or_else(|| self.natural_frame())?;
        self.lay_out(frame, now)
    }

    /// Sends the answer: for the access consent dialog, a grant of the
    /// selected duration or a deny.
    pub(crate) fn answer(self, proceed: bool) {
        match self.answer {
            TrustedAnswer::YesNo(decision) => {
                let _ = decision.send(proceed);
            }
            TrustedAnswer::Consent(decision) => {
                let duration = self
                    .access
                    .as_ref()
                    .and_then(|access| access.durations.get(access.choice.selected()).copied());
                let _ = decision.send(match (proceed, duration) {
                    (true, Some(duration)) => ConsentDecision::Grant { duration },
                    _ => ConsentDecision::Deny,
                });
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalGridLimits {
    pub maximum_columns: u16,
    pub maximum_rows: u16,
}

impl Default for TerminalGridLimits {
    fn default() -> Self {
        Self {
            maximum_columns: MAX_COLUMNS,
            maximum_rows: MAX_ROWS,
        }
    }
}

pub struct WindowPaneOptions {
    pub snapshot: TerminalSnapshot,
    pub terminal_grid_limits: TerminalGridLimits,
    pub updates: Receiver<WindowUpdate>,
    pub commands: Sender<WindowCommand>,
    pub authority: AuthorityStatus,
    pub controlled: bool,
    pub image_sources: ImageContentLeaseSet,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent renderer, input, capture, and endpoint capability flags"
)]
pub struct WindowOptions {
    pub capture: Option<PathBuf>,
    /// Initial owned daemon snapshot. `None` retains the deterministic evidence row.
    pub snapshot: Option<TerminalSnapshot>,
    /// Exact renderer leases paired with the legacy initial snapshot.
    pub image_sources: ImageContentLeaseSet,
    /// Bounded live-update receiver owned by the Wayland thread.
    pub updates: Option<Receiver<WindowUpdate>>,
    /// Bounded command sender from the Wayland thread to the async protocol owner.
    pub commands: Option<Sender<WindowCommand>>,
    /// Retain Q/Escape close shortcuts only for the renderer evidence example.
    pub evidence_close_shortcuts: bool,
    /// Delay capture until this integer output scale is active.
    pub capture_scale: Option<u32>,
    /// Trusted application-owned consent mode. Terminal content cannot enable it.
    pub trusted_consent: Option<TrustedConsentUi>,
    /// Application-owned recent-Dojo picker. Terminal content cannot enable it.
    pub session_picker: Option<SessionPickerUi>,
    /// Trusted authority state rendered in persistent application chrome.
    pub authority: AuthorityStatus,
    /// Whether the legacy single-pane command channel already owns control.
    pub controlled: bool,
    /// Whether the title names a controlling Window "local controller". Only
    /// the title: input still follows `controlled`. The remote connection
    /// status Window turns it off because it controls no terminal.
    pub controller_title: bool,
    /// Endpoint-local terminal limits for the legacy or activated focused pane.
    pub terminal_grid_limits: TerminalGridLimits,
    /// Initial terminal dimensions from the supported configuration subset.
    pub initial_columns: u16,
    pub initial_rows: u16,
    /// Configured cursor presentation policy.
    pub cursor_style: CursorStyle,
    pub cursor_blink: bool,
    /// Optional fixed user title; terminal OSC titles remain active when absent.
    pub title: Option<String>,
    /// Client-local Wayland identity. Only private XDG launch may override the default.
    pub app_id: String,
    /// Current project-owned Omarchy role mapping.
    pub theme: ResolvedTheme,
    /// `[main] ui-font-pixelsize`; `None` lets chrome text follow the theme's
    /// Omarchy text size, otherwise 14 px ([`crate::config::resolve_ui_font_size`]).
    pub ui_font_size: Option<FontSize>,
    pub pane_divider_style: PaneDividerStyle,
    /// `[multiplexer] pane-gap`: logical px between adjacent panes.
    pub pane_gap: u32,
    pub frame_title_mode: FrameTitleMode,
    /// Fully validated client-local keymap for this Window.
    pub keymap: ResolvedKeymap,
    /// Prefix sequence timeout from the validated client configuration.
    pub prefix_timeout_ms: u64,
    /// Explicit opt-in destination; saves also require a local endpoint.
    pub clipboard_image_directory: Option<PathBuf>,
    /// Fixed endpoint authority, not inferred from a UI capability or terminal data.
    pub local_endpoint: bool,
    /// Multi-pane input. Empty retains the legacy one-pane fields above.
    pub panes: Vec<WindowPaneOptions>,
    pub layout: Option<LayoutNode>,
    /// Client-local initial focus; never written back implicitly.
    pub active_splint: Option<SplintId>,
    pub topology_updates: Option<Receiver<WindowTopologyUpdate>>,
    pub topology_commands: Option<Sender<WindowTopologyCommand>>,
    /// Where Reload configuration publishes the settings the topology manager
    /// reads; shared state, so a busy command queue cannot drop a reload.
    pub topology_settings: Option<WatchSender<TopologyReloadSettings>>,
    /// Coalesced ephemeral keyboard/pane focus publication for the supported adapter API.
    pub graphical_focus: Option<WatchSender<Option<SplintId>>>,
    /// Whether trusted graphical force-transfer actions may be offered or dispatched.
    pub forced_control_transfer: bool,
    /// Render a client-local placeholder before dispatching a remote split.
    pub optimistic_remote_splits: bool,
    /// Stable identity for the initial managed Dojo; absent for legacy/evidence windows.
    pub initial_dojo: Option<WindowDojoIdentity>,
    /// Initial tab-strip policy for managed Dojo windows.
    pub initial_tab_strip_visible: bool,
    /// Remote daemon identity from this Window's initial connection; never terminal content.
    pub remote_display_identity: Option<crate::endpoint::RemoteDisplayIdentity>,
    /// `[explorer] agent-status`: show explicitly reported agent status.
    pub agent_status_presentation: bool,
    /// `[explorer] status-colors` and `status-glyphs`.
    pub agent_status_style: crate::config::AgentStatusStyle,
    /// `[multiplexer] tab-strip-border`: draw the light `line` divider stroke
    /// along the bottom of the tab strip.
    pub tab_strip_border: bool,
    /// `[explorer] full-height`: a docked Explorer runs the Window's full
    /// height and the tab strip begins at its right edge.
    pub explorer_full_height: bool,
    /// `[explorer] collapsed`: the docked Explorer first shows as its rail.
    pub explorer_collapsed: bool,
    /// Whether the Window binds the text-input (IME) protocol. The remote
    /// connection status Window turns it off so an input method never sees or
    /// draws a typed password as composing text.
    pub input_method: bool,
    /// Fired once, after the first buffer commit maps the Window. Dropped unfired
    /// when the Window ends without mapping.
    pub mapped: Option<oneshot::Sender<()>>,
    /// The Window's daemon handoff status, fed by its panes' control
    /// subscriptions; drives the upgrade banner and input pause (Plan 0061
    /// M3.12a). `None` for Windows that hold no daemon panes.
    pub handoff_status: Option<crate::handoff_status::HandoffStatusHub>,
    /// Where a local managed Window publishes its tabs and focus each
    /// event-loop turn, for the resume record its handoff anchor seals
    /// (Plan 0061 M3.14). `None` for every other Window.
    pub layout_ledger: Option<crate::window_continuation::WindowLayoutLedger>,
    /// Run as one phase of a staged Window ([`crate::wayland::run_window_on`]):
    /// a session-picker decision, Escape in a standalone picker, or the end of
    /// its own status Splint parks the Window for the next phase instead of
    /// closing it. A compositor or user close still ends it. A phase that holds
    /// a daemon endpoint, or turns the input method on, cannot keep it.
    pub keep_surface: bool,
    /// The Window is the remote connection status screen: an application-owned
    /// snapshot that controls no terminal. Only that screen sets it; with a
    /// session picker, it is the only content that may keep a staged surface.
    pub connection_status_screen: bool,
}

impl Default for WindowOptions {
    fn default() -> Self {
        Self {
            capture: None,
            snapshot: None,
            image_sources: ImageContentLeaseSet::default(),
            updates: None,
            commands: None,
            evidence_close_shortcuts: false,
            capture_scale: None,
            trusted_consent: None,
            session_picker: None,
            authority: AuthorityStatus::default(),
            controlled: true,
            controller_title: true,
            terminal_grid_limits: TerminalGridLimits::default(),
            initial_columns: 80,
            initial_rows: 24,
            cursor_style: CursorStyle::Block,
            cursor_blink: true,
            title: None,
            app_id: APP_ID.to_owned(),
            theme: ResolvedTheme::default(),
            ui_font_size: None,
            pane_divider_style: PaneDividerStyle::Line,
            pane_gap: 0,
            frame_title_mode: FrameTitleMode::Splint,
            keymap: ResolvedKeymap::default(),
            prefix_timeout_ms: 1_000,
            clipboard_image_directory: None,
            local_endpoint: false,
            panes: Vec::new(),
            layout: None,
            active_splint: None,
            topology_updates: None,
            topology_commands: None,
            topology_settings: None,
            graphical_focus: None,
            forced_control_transfer: true,
            optimistic_remote_splits: false,
            initial_dojo: None,
            initial_tab_strip_visible: true,
            remote_display_identity: None,
            agent_status_presentation: true,
            agent_status_style: crate::config::AgentStatusStyle::default(),
            tab_strip_border: true,
            explorer_full_height: true,
            explorer_collapsed: false,
            input_method: true,
            mapped: None,
            handoff_status: None,
            layout_ledger: None,
            keep_surface: false,
            connection_status_screen: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::frontend::{DialogLine, trusted_dialog::UPGRADE_ARMING_DELAY};

    /// Consent text whose fifth line, a duration, wraps onto two rows at
    /// 40 columns.
    fn consent_text(selected: usize) -> DialogText {
        let marker = |index| if index == selected { "(•)" } else { "( )" };
        DialogText {
            lines: vec![
                DialogLine::new("TRUSTED SPLINTERM ACCESS REQUEST"),
                DialogLine::blank(),
                DialogLine::new("Program: /usr/bin/splinterm-mcp"),
                DialogLine::new("How long:"),
                DialogLine::new(format!("  {} Just this once", marker(0))).with_duration(0),
                DialogLine::new(format!(
                    "  {} While 'Workspace' is open: until this workspace is closed",
                    marker(1)
                ))
                .with_indent(6)
                .with_duration(1),
                DialogLine::hint("G or the green area grants."),
            ],
            minimum_rows: 0,
        }
    }

    /// A frame with room for only five of the consent text's eight rows.
    fn clipped_frame() -> DialogFrame {
        DialogFrame {
            max_text_rows: 5,
            ..narrow_frame()
        }
    }

    fn consent_two_durations() -> TrustedConsentUi {
        let (decision, _answers) = std::sync::mpsc::channel();
        TrustedConsentUi::access_consent(
            decision,
            vec![
                (GrantDuration::Once, true),
                (GrantDuration::WorkspaceSession, true),
            ],
            Box::new(consent_text),
        )
    }

    fn snapshot_text(snapshot: &TerminalSnapshot) -> String {
        snapshot
            .visible_rows
            .iter()
            .map(|row| {
                row.cells
                    .iter()
                    .map(|cell| cell.content.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn narrow_frame() -> DialogFrame {
        DialogFrame {
            wrap_columns: 40,
            grid_columns: 40,
            grid_rows: 12,
            max_text_rows: 12,
        }
    }

    /// The operator rule end to end: a dialog that has focus still ignores
    /// every key until a click inside engages it; then it arms, and the
    /// answer names the selected duration.
    #[test]
    fn a_trusted_dialog_answers_only_after_a_click_inside() {
        let (decision, answers) = std::sync::mpsc::channel();
        let mut dialog = TrustedConsentUi::access_consent(
            decision,
            vec![
                (GrantDuration::Once, true),
                (GrantDuration::WorkspaceSession, true),
            ],
            Box::new(consent_text),
        );
        // Laid out narrower than it asked for, the "while open" duration
        // wraps onto rows 5 and 6.
        let snapshot = dialog.lay_out(narrow_frame(), Instant::now()).unwrap();
        assert_eq!(snapshot.columns, 40);
        assert_eq!(
            dialog.access.as_ref().unwrap().duration_rows,
            [(4, 0), (5, 1), (6, 1)]
        );
        let appeared = Instant::now();
        dialog.note_focus(appeared);
        let later = appeared + Duration::from_secs(30);
        for key in [
            TrustedDialogKey::G,
            TrustedDialogKey::Enter,
            TrustedDialogKey::Escape,
            TrustedDialogKey::D,
            TrustedDialogKey::Down,
        ] {
            assert_eq!(dialog.key_input(key, false, later), None, "{key:?}");
        }
        assert_eq!(dialog.action_area_input(true, later), None);
        // A click on the second row of the wrapped "while open" duration
        // engages the dialog and selects it.
        dialog.engage(later);
        assert_eq!(dialog.row_input(6, later), Some(TrustedInput::Redraw));
        assert_eq!(dialog.key_input(TrustedDialogKey::G, false, later), None);
        let armed = later + UPGRADE_ARMING_DELAY;
        assert_eq!(
            dialog.key_input(TrustedDialogKey::G, false, armed),
            Some(TrustedInput::Answer(true))
        );
        dialog.answer(true);
        assert_eq!(
            answers.try_recv().unwrap(),
            ConsentDecision::Grant {
                duration: GrantDuration::WorkspaceSession
            }
        );
    }

    #[test]
    fn the_upgrade_and_restart_questions_need_a_click_too() {
        for build in [TrustedConsentUi::upgrade, TrustedConsentUi::restart] {
            let (decision, answers) = std::sync::mpsc::channel();
            let mut dialog = build(decision);
            let appeared = Instant::now();
            dialog.note_focus(appeared);
            let later = appeared + Duration::from_secs(30);
            assert_eq!(
                dialog.key_input(TrustedDialogKey::Escape, false, later),
                None
            );
            assert_eq!(dialog.action_area_input(true, later), None);
            assert_eq!(
                dialog.action_area_input(false, later),
                Some(TrustedInput::Answer(false)),
                "cancelling by click always counts"
            );
            dialog.engage(later);
            assert_eq!(
                dialog.key_input(
                    TrustedDialogKey::Escape,
                    false,
                    later + UPGRADE_ARMING_DELAY
                ),
                Some(TrustedInput::Answer(false))
            );
            dialog.answer(false);
            assert!(!answers.try_recv().unwrap());
        }
    }

    fn consent_dialog() -> TrustedConsentUi {
        let (decision, _answers) = std::sync::mpsc::channel();
        TrustedConsentUi::access_consent(
            decision,
            vec![(GrantDuration::Once, true)],
            Box::new(consent_text),
        )
        .with_shortcut(Some(DialogShortcut::default()))
    }

    /// Plan 0053 item 9: the dialog shortcut engages a focused dialog as a
    /// click does but never answers; the arming delay then applies, and
    /// stray keys before it stay inert.
    #[test]
    fn the_dialog_shortcut_engages_without_answering_then_arms() {
        let upgrade = |decision| {
            TrustedConsentUi::upgrade(decision).with_shortcut(Some(DialogShortcut::default()))
        };
        let restart = |decision| {
            TrustedConsentUi::restart(decision).with_shortcut(Some(DialogShortcut::default()))
        };
        let builds: [&dyn Fn() -> TrustedConsentUi; 3] = [
            &consent_dialog,
            &|| upgrade(std::sync::mpsc::channel().0),
            &|| restart(std::sync::mpsc::channel().0),
        ];
        for build in builds {
            let mut dialog = build();
            assert_eq!(dialog.shortcut(), Some(DialogShortcut::default()));
            let appeared = Instant::now();
            dialog.note_focus(appeared);
            let later = appeared + Duration::from_secs(30);
            // Stray keys before engagement are still ignored.
            for key in [
                TrustedDialogKey::G,
                TrustedDialogKey::R,
                TrustedDialogKey::D,
                TrustedDialogKey::Enter,
                TrustedDialogKey::Other,
            ] {
                assert_eq!(dialog.key_input(key, false, later), None, "{key:?}");
            }
            assert_eq!(dialog.action_area_input(true, later), None);
            // A repeated chord is not a press and engages nothing.
            assert_eq!(
                dialog.key_input(TrustedDialogKey::Engage, true, later),
                None
            );
            assert_eq!(dialog.action_area_input(true, later), None);
            // The chord engages and answers nothing.
            assert_eq!(
                dialog.key_input(TrustedDialogKey::Engage, false, later),
                None
            );
            // Arming applies after it, exactly as after a click.
            let early = later + Duration::from_millis(599);
            assert_eq!(
                dialog.key_input(TrustedDialogKey::Escape, false, early),
                None
            );
            assert_eq!(dialog.action_area_input(true, early), None);
            // Pressing the chord again, even once armed, still answers nothing
            // and does not restart the delay.
            let armed = later + UPGRADE_ARMING_DELAY;
            assert_eq!(
                dialog.key_input(TrustedDialogKey::Engage, false, armed),
                None
            );
            assert_eq!(
                dialog.key_input(TrustedDialogKey::Escape, false, armed),
                Some(TrustedInput::Answer(false))
            );
        }
    }

    #[test]
    fn a_dialog_built_without_the_shortcut_has_none() {
        let (decision, _answers) = std::sync::mpsc::channel();
        assert_eq!(TrustedConsentUi::upgrade(decision).shortcut(), None);
        assert_eq!(consent_dialog().with_shortcut(None).shortcut(), None);
    }

    /// A dialog's Window asks for its natural size, and a redraw after a
    /// selection keeps the layout it was given last, so the rows a click
    /// selects stay where they are drawn.
    #[test]
    fn dialogs_open_at_their_natural_size_and_redraw_in_their_last_layout() {
        let (decision, _answers) = std::sync::mpsc::channel();
        let dialog = TrustedConsentUi::access_consent(
            decision,
            vec![
                (GrantDuration::Once, true),
                (GrantDuration::WorkspaceSession, true),
            ],
            Box::new(consent_text),
        );
        let natural = dialog.natural_frame().unwrap();
        assert_eq!(natural.grid_columns, 64);
        assert_eq!(natural.grid_rows, (7 * 100_usize).div_ceil(72));
        let options = dialog.into_window_options();
        let snapshot = options.snapshot.as_ref().unwrap();
        assert_eq!(usize::from(options.initial_columns), snapshot.columns);
        assert_eq!(usize::from(options.initial_rows), snapshot.rows);
        assert_eq!((snapshot.columns, snapshot.rows), (64, natural.grid_rows));
        let mut dialog = options.trusted_consent.unwrap();
        assert_eq!(
            dialog.access.as_ref().unwrap().duration_rows,
            [(4, 0), (5, 1)]
        );
        let now = Instant::now();
        dialog.lay_out(narrow_frame(), now).unwrap();
        dialog.engage(now);
        assert_eq!(dialog.row_input(6, now), Some(TrustedInput::Redraw));
        let redrawn = dialog.render(now).unwrap();
        assert_eq!(redrawn.columns, 40);
        let row = |index: usize| -> String {
            redrawn.visible_rows[index]
                .cells
                .iter()
                .map(|cell| cell.content.as_str())
                .collect()
        };
        assert!(row(5).starts_with("  (•) While"), "{}", row(5));
        assert_eq!(
            dialog.access.as_ref().unwrap().duration_rows,
            [(4, 0), (5, 1), (6, 1)]
        );
        // The yes/no questions have no layout until they get text.
        let (decision, _answers) = std::sync::mpsc::channel();
        assert!(
            TrustedConsentUi::upgrade(decision.clone())
                .natural_frame()
                .is_none()
        );
        let options = TrustedConsentUi::upgrade(decision)
            .with_text(consent_text(0))
            .into_window_options();
        assert_eq!(options.initial_columns, 64);
    }

    /// S1: while part of the request is hidden, `G` and the green area
    /// grant nothing, the deny answers still count, and the dialog says
    /// why. Once all of it shows again, granting arms only after the
    /// arming delay.
    #[test]
    fn nothing_grants_while_part_of_the_request_is_hidden() {
        let mut dialog = consent_two_durations();
        let start = Instant::now();
        let snapshot = dialog.lay_out(clipped_frame(), start).unwrap();
        assert!(dialog.clipped());
        let shown = snapshot_text(&snapshot);
        assert!(shown.contains("Part of this request is hidden"), "{shown}");
        assert!(!shown.contains("G or the green area grants"), "{shown}");
        dialog.engage(start);
        let armed = start + UPGRADE_ARMING_DELAY;
        assert_eq!(dialog.key_input(TrustedDialogKey::G, false, armed), None);
        assert_eq!(dialog.action_area_input(true, armed), None);
        for key in [
            TrustedDialogKey::D,
            TrustedDialogKey::Escape,
            TrustedDialogKey::Enter,
        ] {
            assert_eq!(
                dialog.key_input(key, false, armed),
                Some(TrustedInput::Answer(false)),
                "{key:?}"
            );
        }
        assert_eq!(
            dialog.action_area_input(false, armed),
            Some(TrustedInput::Answer(false))
        );
        // The whole request in view again: the delay starts over.
        let shown_again = armed + Duration::from_secs(5);
        dialog.lay_out(narrow_frame(), shown_again).unwrap();
        assert!(!dialog.clipped());
        assert_eq!(
            dialog.key_input(TrustedDialogKey::G, false, shown_again),
            None
        );
        assert_eq!(dialog.action_area_input(true, shown_again), None);
        let rearmed = shown_again + UPGRADE_ARMING_DELAY;
        assert_eq!(
            dialog.key_input(TrustedDialogKey::G, false, rearmed),
            Some(TrustedInput::Answer(true))
        );
        assert_eq!(
            dialog.action_area_input(true, rearmed),
            Some(TrustedInput::Answer(true))
        );
    }

    /// The arrow keys still choose a duration while the request is hidden;
    /// choosing never grants, so that is harmless.
    #[test]
    fn a_hidden_request_still_lets_the_duration_move_but_never_grants() {
        let mut dialog = consent_two_durations();
        let start = Instant::now();
        dialog.lay_out(clipped_frame(), start).unwrap();
        dialog.engage(start);
        let armed = start + UPGRADE_ARMING_DELAY;
        assert_eq!(
            dialog.key_input(TrustedDialogKey::Down, false, armed),
            Some(TrustedInput::Redraw)
        );
        let later = armed + Duration::from_secs(5);
        assert!(dialog.render(later).is_some());
        assert!(dialog.clipped());
        assert_eq!(dialog.key_input(TrustedDialogKey::G, false, later), None);
    }

    /// S1 for the yes/no questions: nothing proceeds or restarts while
    /// text is hidden, on the key or the green area, and the no answers
    /// still count. Showing everything again re-arms but never engages.
    #[test]
    fn yes_no_questions_never_say_yes_while_text_is_hidden() {
        for (build, proceed) in [
            (
                TrustedConsentUi::upgrade as fn(_) -> TrustedConsentUi,
                TrustedDialogKey::G,
            ),
            (
                TrustedConsentUi::restart as fn(_) -> TrustedConsentUi,
                TrustedDialogKey::R,
            ),
        ] {
            let (decision, _answers) = std::sync::mpsc::channel();
            let mut dialog = build(decision).with_text(consent_text(0));
            let start = Instant::now();
            let snapshot = dialog.lay_out(clipped_frame(), start).unwrap();
            assert!(dialog.clipped());
            let shown = snapshot_text(&snapshot);
            assert!(shown.contains("Part of this question is hidden"), "{shown}");
            dialog.engage(start);
            let armed = start + UPGRADE_ARMING_DELAY;
            assert_eq!(dialog.key_input(proceed, false, armed), None);
            assert_eq!(dialog.action_area_input(true, armed), None);
            assert_eq!(
                dialog.key_input(TrustedDialogKey::Escape, false, armed),
                Some(TrustedInput::Answer(false))
            );
            assert_eq!(
                dialog.action_area_input(false, armed),
                Some(TrustedInput::Answer(false))
            );
            let shown_again = armed + Duration::from_secs(5);
            dialog.lay_out(narrow_frame(), shown_again).unwrap();
            assert_eq!(dialog.key_input(proceed, false, shown_again), None);
            let rearmed = shown_again + UPGRADE_ARMING_DELAY;
            assert_eq!(
                dialog.key_input(proceed, false, rearmed),
                Some(TrustedInput::Answer(true))
            );
            assert_eq!(
                dialog.action_area_input(true, rearmed),
                Some(TrustedInput::Answer(true))
            );

            // A dialog nobody engaged stays inert when its text shows again.
            let (decision, _answers) = std::sync::mpsc::channel();
            let mut idle = build(decision).with_text(consent_text(0));
            idle.lay_out(clipped_frame(), start).unwrap();
            idle.lay_out(narrow_frame(), shown_again).unwrap();
            let much_later = shown_again + Duration::from_secs(60);
            assert_eq!(idle.key_input(proceed, false, much_later), None);
            assert_eq!(idle.action_area_input(true, much_later), None);
        }
    }

    /// S2: laying the text out again for a new size, scale, or font arms an
    /// engaged dialog again, since its areas and rows may have moved under
    /// the pointer; an unengaged one stays inert.
    #[test]
    fn laying_out_again_for_the_window_rearms_an_engaged_dialog() {
        let mut dialog = consent_two_durations().into_window_options();
        let mut dialog = dialog.trusted_consent.take().unwrap();
        let start = Instant::now();
        dialog.engage(start);
        let armed = start + UPGRADE_ARMING_DELAY;
        assert_eq!(
            dialog.key_input(TrustedDialogKey::G, false, armed),
            Some(TrustedInput::Answer(true))
        );
        let moved = armed + Duration::from_secs(1);
        dialog.lay_out_again(narrow_frame(), moved).unwrap();
        assert_eq!(dialog.key_input(TrustedDialogKey::G, false, moved), None);
        assert_eq!(dialog.action_area_input(true, moved), None);
        assert_eq!(
            dialog.key_input(TrustedDialogKey::G, false, moved + UPGRADE_ARMING_DELAY),
            Some(TrustedInput::Answer(true))
        );
        let mut idle = consent_two_durations();
        idle.lay_out_again(narrow_frame(), start).unwrap();
        assert_eq!(
            idle.key_input(TrustedDialogKey::G, false, start + Duration::from_secs(60)),
            None
        );
    }
}
