//! Platform-independent construction options for graphical windows and panes.

use std::{
    path::PathBuf,
    sync::mpsc::Sender as StdSender,
    time::{Duration, Instant},
};

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
    WindowDojoIdentity, WindowThemeColors, WindowTopologyCommand, WindowTopologyUpdate,
    WindowUpdate,
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
    /// The rows of the current snapshot that show a duration.
    duration_rows: Vec<(usize, usize)>,
    render: AccessConsentRenderer,
}

/// How much sooner than the asker's wait a dialog's countdown runs out:
/// the asker starts its clock before the dialog process starts, so the
/// countdown never still shows time once the asker has stopped waiting.
pub(crate) const COUNTDOWN_MARGIN: Duration = Duration::from_secs(1);

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
    /// Whether the current snapshot's legend says the keys are live: it
    /// is drawn again once a click or the shortcut engages the keyboard.
    legend_live: bool,
    /// When the asker stops waiting for an answer, if the dialog knows; the
    /// frame then shows the time left. What happens then is the asker's;
    /// an access consent grants nothing from then on.
    deadline: Option<Instant>,
    /// The time left the current snapshot shows.
    countdown_shown: Option<String>,
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
            legend_live: false,
            deadline: None,
            countdown_shown: None,
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
            legend_live: false,
            deadline: None,
            countdown_shown: None,
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
            legend_live: false,
            deadline: None,
            countdown_shown: None,
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

    /// How long the asker waits for an answer, from now: the frame's top
    /// edge then counts it down. The asker ends the dialog when it runs
    /// out, as before; an access consent that has run out grants nothing
    /// meanwhile, so only a deny still answers it. A zero wait, from an
    /// asker that sent none, shows no countdown.
    ///
    /// The asker started its clock before this dialog started, so the time
    /// shown is [`COUNTDOWN_MARGIN`] short of `within`: it reaches "0:00"
    /// no later than the asker stops waiting.
    #[must_use]
    pub fn with_respond_within(mut self, within: Duration) -> Self {
        self.deadline = if within.is_zero() {
            None
        } else {
            Instant::now().checked_add(within.saturating_sub(COUNTDOWN_MARGIN))
        };
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
            wrap_columns: dialog_text::text_columns(columns),
            grid_columns: columns,
            grid_rows: dialog_text::natural_rows(&variants),
            max_text_rows: usize::MAX,
        })
    }

    /// Lays the current selection's text out in `frame` and returns its
    /// snapshot; later redraws keep that frame. `None` without text.
    ///
    /// The body takes at most the rows the box leaves it
    /// ([`dialog_text::ChromeRows::body_room`]). A layout that hides part of
    /// the text, a body row or a button label, shows
    /// [`TrustedDialogKind::clipped_notice`] in place of the answer hints
    /// and makes every yes answer inert; so does an access consent's
    /// countdown that the top edge has no room for, since the person must
    /// see how long is left. When the whole text shows again, an engaged
    /// dialog arms again from `now`, so a yes counts only once the whole
    /// text has been in view for the arming delay.
    pub(crate) fn lay_out(&mut self, frame: DialogFrame, now: Instant) -> Option<TerminalSnapshot> {
        let variants = self.variants();
        let selected = self.selected_variant();
        let text = variants.get(selected).or_else(|| variants.first())?;
        let max_rows = frame
            .max_text_rows
            .min(dialog_text::chrome_rows(frame.grid_rows).body_room);
        let countdown_hidden = self.access.is_some()
            && self.countdown(now).is_some_and(|countdown| {
                !dialog_text::countdown_fits(text.title(), &countdown, frame.grid_columns)
            });
        let layout = dialog_text::layout_hiding(
            &variants,
            selected,
            frame.wrap_columns,
            max_rows,
            Some(self.kind.clipped_notice()),
            !text.buttons_fit(frame.grid_columns) || countdown_hidden,
        );
        if let Some(access) = self.access.as_mut() {
            // A click lands on a snapshot row; the body starts below the
            // title.
            access.duration_rows = layout
                .duration_rows
                .iter()
                .map(|(row, index)| (row + dialog_text::CHROME_TOP_ROWS, *index))
                .collect();
        }
        if self.clipped && !layout.clipped {
            self.rearm(now);
        }
        self.clipped = layout.clipped;
        self.frame = Some(frame);
        self.legend_live = self.keyboard_engaged();
        self.countdown_shown = self.countdown(now);
        let chrome = self.chrome(now);
        Some(dialog_text::dialog_snapshot(
            dialog_text::compose(text, &layout, &chrome, frame.grid_columns, frame.grid_rows),
            frame.grid_columns,
            frame.grid_rows,
            &self.identity,
        ))
    }

    /// Whether a click inside or the dialog shortcut engaged the keyboard.
    fn keyboard_engaged(&self) -> bool {
        self.access.as_ref().map_or_else(
            || self.engagement.engaged(),
            |access| access.choice.engagement().engaged(),
        )
    }

    /// The box around the current layout: the keys under each button, the
    /// Allow button's state at `now`, and the legend.
    fn chrome(&self, now: Instant) -> dialog_text::DialogChrome {
        let choices = self
            .access
            .as_ref()
            .map_or(0, |access| access.choice.offered());
        dialog_text::DialogChrome {
            deny_keys: self.kind.deny_keys().to_owned(),
            allow_keys: self.kind.allow_keys().to_owned(),
            allow_enabled: !self.clipped && !self.consent_expired(now),
            legend: self
                .kind
                .legend(self.legend_live, choices > 1, self.shortcut),
            countdown: self.countdown_shown.clone(),
        }
    }

    /// The time left to answer at `now`, after what happens then, such as
    /// "cancels in 0:18": the time as "m:ss", rounded up, so it shows
    /// "0:00" only once the time is up. `None` without a deadline.
    fn countdown(&self, now: Instant) -> Option<String> {
        let left = self.deadline?.saturating_duration_since(now);
        let seconds = left.as_secs() + u64::from(left.subsec_nanos() > 0);
        Some(format!(
            "{} {}:{:02}",
            self.kind.countdown_verb(),
            seconds / 60,
            seconds % 60
        ))
    }

    /// How long until the countdown the frame shows changes, from `now`:
    /// the Window wakes then to draw it again, at once while what it shows
    /// is out of date. `None` without one, or once it shows "0:00".
    pub(crate) fn countdown_wait(&self, now: Instant) -> Option<Duration> {
        if self.countdown_stale(now) {
            return Some(Duration::ZERO);
        }
        self.frame?;
        let left = self.deadline?.saturating_duration_since(now);
        if left.is_zero() {
            return None;
        }
        // The shown second changes when `left` crosses a whole second; a
        // millisecond past it, so the Window never wakes just before.
        let into_second = Duration::from_nanos(u64::from(left.subsec_nanos()));
        let wait = if into_second.is_zero() {
            Duration::from_secs(1)
        } else {
            into_second
        };
        Some(wait + Duration::from_millis(1))
    }

    /// Whether the countdown the frame shows is out of date at `now`.
    pub(crate) fn countdown_stale(&self, now: Instant) -> bool {
        self.frame.is_some() && self.countdown_shown != self.countdown(now)
    }

    /// Whether the access consent dialog's time to answer has run out at
    /// `now`: from then on it grants nothing, its Allow button is drawn
    /// disabled, and only a deny answers it, until its asker, which has
    /// stopped waiting, ends it. The request then ends unanswered, as the
    /// countdown's "denies in" says. The upgrade and restart questions
    /// leave all of that to their asker.
    fn consent_expired(&self, now: Instant) -> bool {
        self.access.is_some() && self.deadline.is_some_and(|deadline| now >= deadline)
    }

    /// A redraw when a click that answers nothing, outside any row, just
    /// engaged the keyboard and the legend still says how to engage it.
    pub(crate) fn engagement_redraw(&self) -> Option<TrustedInput> {
        self.with_legend(None)
    }

    /// `input`, or a redraw when the keyboard was just engaged and the
    /// legend still says how to engage it.
    fn with_legend(&self, input: Option<TrustedInput>) -> Option<TrustedInput> {
        match input {
            None if self.legend_live != self.keyboard_engaged() && self.frame.is_some() => {
                Some(TrustedInput::Redraw)
            }
            input => input,
        }
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
        // Nothing grants while part of the request is hidden, or once the
        // time to answer is up.
        let grants = !self.clipped && !self.consent_expired(now);
        let access = self.access.as_mut()?;
        match action? {
            ConsentAction::Deny => Some(TrustedInput::Answer(false)),
            ConsentAction::Grant => grants.then_some(TrustedInput::Answer(true)),
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
            // answers; the arming delay then applies as usual. The legend
            // then says which keys answer.
            if !repeat {
                self.engage(now);
            }
            return self.with_legend(None);
        }
        let input = if let Some(access) = &self.access {
            let action = access.choice.key(key, repeat, now);
            self.consent_input(action, now)
        } else {
            self.kind
                .key_decision(key, repeat, self.engagement, now)
                .filter(|proceed| !(*proceed && self.clipped))
                .map(TrustedInput::Answer)
        };
        self.with_legend(input)
    }

    /// A click on the buttons: the right (Allow) one grants or proceeds.
    pub(crate) fn action_area_input(
        &mut self,
        proceed_area: bool,
        now: Instant,
    ) -> Option<TrustedInput> {
        let input = if let Some(access) = &self.access {
            let action = access.choice.click_action_area(proceed_area, now);
            self.consent_input(action, now)
        } else {
            self.kind
                .click_decision(proceed_area, self.engagement, now)
                .filter(|proceed| !(*proceed && self.clipped))
                .map(TrustedInput::Answer)
        };
        self.with_legend(input)
    }

    /// A click on a snapshot row: selects the duration shown there.
    pub(crate) fn row_input(&mut self, row: usize, now: Instant) -> Option<TrustedInput> {
        let input = self.duration_row_input(row, now);
        self.with_legend(input)
    }

    fn duration_row_input(&mut self, row: usize, now: Instant) -> Option<TrustedInput> {
        let access = self.access.as_ref()?;
        let index = access
            .duration_rows
            .iter()
            .find_map(|(shown, index)| (*shown == row).then_some(*index))?;
        let action = access.choice.click_duration(index);
        self.consent_input(action, now)
    }

    /// The dialog rendered again for its current selection and keyboard
    /// state, laid out where it was last.
    pub(crate) fn render(&mut self, now: Instant) -> Option<TerminalSnapshot> {
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
    /// The `config.ini` `[colors]` keys `theme` was resolved with, and the
    /// input it came from, so a configuration reload can resolve it again.
    pub theme_colors: WindowThemeColors,
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
    /// `[clipboard] middle-click-paste`: a middle click pastes the primary
    /// selection.
    pub middle_click_paste: bool,
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
    /// Whether Reload configuration turns them on and off with
    /// `remote-agent-prompts`: a remote Window confirms a force in its own
    /// dialog (Plan 0072 M3).
    pub forced_control_follows_remote_prompts: bool,
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
    /// `[explorer] row-lines=2`: Splint rows take a second, muted line.
    pub explorer_two_line_rows: bool,
    /// `[explorer] current-marker`: the icon before the current row.
    pub explorer_current_marker: crate::config::ExplorerCurrentMarker,
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
            theme_colors: WindowThemeColors::default(),
            ui_font_size: None,
            pane_divider_style: PaneDividerStyle::Line,
            pane_gap: 0,
            frame_title_mode: FrameTitleMode::Splint,
            keymap: ResolvedKeymap::default(),
            prefix_timeout_ms: 1_000,
            clipboard_image_directory: None,
            middle_click_paste: true,
            local_endpoint: false,
            panes: Vec::new(),
            layout: None,
            active_splint: None,
            topology_updates: None,
            topology_commands: None,
            topology_settings: None,
            graphical_focus: None,
            forced_control_transfer: true,
            forced_control_follows_remote_prompts: false,
            optimistic_remote_splits: false,
            initial_dojo: None,
            initial_tab_strip_visible: true,
            remote_display_identity: None,
            agent_status_presentation: true,
            agent_status_style: crate::config::AgentStatusStyle::default(),
            tab_strip_border: true,
            explorer_full_height: true,
            explorer_collapsed: false,
            explorer_two_line_rows: false,
            explorer_current_marker: crate::config::ExplorerCurrentMarker::Eye,
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

    /// Consent text whose sixth line, a duration, wraps onto two rows in a
    /// box 40 columns wide (36 inside its frame).
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
                DialogLine::deny("Deny"),
                DialogLine::allow("Allow Look once"),
            ],
            minimum_rows: 0,
        }
    }

    /// A frame with room for only five of the consent text's eight body rows.
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

    /// A click inside that answers nothing, as the Window handles it: it
    /// engages the dialog, which then draws its live legend.
    fn click_inside(dialog: &mut TrustedConsentUi, now: Instant) {
        dialog.engage(now);
        assert_eq!(dialog.row_input(0, now), Some(TrustedInput::Redraw));
        dialog.render(now).unwrap();
    }

    fn narrow_frame() -> DialogFrame {
        DialogFrame {
            wrap_columns: 36,
            grid_columns: 40,
            grid_rows: 15,
            max_text_rows: 15,
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
        // wraps onto snapshot rows 7 and 8: the body starts below the title.
        let snapshot = dialog.lay_out(narrow_frame(), Instant::now()).unwrap();
        assert_eq!(snapshot.columns, 40);
        assert_eq!(
            dialog.access.as_ref().unwrap().duration_rows,
            [(6, 0), (7, 1), (8, 1)]
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
        assert_eq!(dialog.row_input(8, later), Some(TrustedInput::Redraw));
        dialog.render(later).unwrap();
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
        // The widest row, 63 columns, inside the frame; seven body rows and
        // the box's.
        assert_eq!(
            natural.grid_columns,
            63 + 2 * dialog_text::FRAME_INSET_COLUMNS
        );
        assert_eq!(natural.wrap_columns, 63);
        assert_eq!(natural.grid_rows, 7 + dialog_text::CHROME_ROWS);
        let options = dialog.into_window_options();
        let snapshot = options.snapshot.as_ref().unwrap();
        assert_eq!(usize::from(options.initial_columns), snapshot.columns);
        assert_eq!(usize::from(options.initial_rows), snapshot.rows);
        assert_eq!((snapshot.columns, snapshot.rows), (67, natural.grid_rows));
        let mut dialog = options.trusted_consent.unwrap();
        assert_eq!(
            dialog.access.as_ref().unwrap().duration_rows,
            [(6, 0), (7, 1)]
        );
        let now = Instant::now();
        dialog.lay_out(narrow_frame(), now).unwrap();
        dialog.engage(now);
        assert_eq!(dialog.row_input(8, now), Some(TrustedInput::Redraw));
        let redrawn = dialog.render(now).unwrap();
        assert_eq!(redrawn.columns, 40);
        let row = |index: usize| -> String {
            redrawn.visible_rows[index]
                .cells
                .iter()
                .map(|cell| cell.content.as_str())
                .collect()
        };
        assert!(row(7).starts_with("    (•) While"), "{}", row(7));
        assert_eq!(
            dialog.access.as_ref().unwrap().duration_rows,
            [(6, 0), (7, 1), (8, 1)]
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
        assert_eq!(options.initial_columns, 67);
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
        click_inside(&mut dialog, start);
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
            click_inside(&mut dialog, start);
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

    fn snapshot_rows(snapshot: &TerminalSnapshot) -> Vec<String> {
        snapshot_text(snapshot)
            .split('\n')
            .map(str::to_owned)
            .collect()
    }

    /// The palette slot a snapshot cell's text is drawn from.
    fn ink_at(snapshot: &TerminalSnapshot, row: usize, column: usize) -> u32 {
        snapshot.visible_rows[row].cells[column]
            .attributes
            .foreground
    }

    /// The box names both buttons in their halves with the keys that
    /// answer under them, and the legend says how to engage until a click
    /// or the shortcut does, then which keys answer. Engaging redraws the
    /// legend but never arms early.
    #[test]
    fn the_box_names_its_buttons_and_its_legend_follows_engagement() {
        let mut dialog = consent_two_durations().with_shortcut(Some(DialogShortcut::default()));
        let start = Instant::now();
        let frame = narrow_frame();
        let places = dialog_text::chrome_rows(frame.grid_rows);
        let snapshot = dialog.lay_out(frame, start).unwrap();
        let rows = snapshot_rows(&snapshot);
        assert!(rows[2].starts_with("  TRUSTED SPLINTERM"), "{rows:#?}");
        let labels = &rows[places.label];
        let deny = labels.find("Deny").unwrap();
        let allow = labels.find("Allow Look once").unwrap();
        assert!(2 * (deny + 4) < frame.grid_columns, "{labels:?}");
        assert!(2 * allow > frame.grid_columns, "{labels:?}");
        assert_eq!(
            ink_at(&snapshot, places.label, allow),
            u32::try_from(dialog_text::DialogInk::AllowLabel.slot().unwrap()).unwrap()
        );
        let keys = &rows[places.keys];
        assert!(
            keys.contains("esc · d · ↵") && keys.trim_end().ends_with('g'),
            "{keys:?}"
        );
        // The long form does not fit 36 columns; the next one does.
        assert_eq!(rows[places.legend].trim(), "click or ctrl alt g for keys");
        // The chord engages, and the legend is drawn again with the keys.
        assert_eq!(
            dialog.key_input(TrustedDialogKey::Engage, false, start),
            Some(TrustedInput::Redraw)
        );
        let engaged = dialog.render(start).unwrap();
        assert_eq!(
            snapshot_rows(&engaged)[places.legend].trim(),
            "↑↓ how long · g allow · esc deny"
        );
        // Once the legend is live nothing more is drawn, and nothing grants
        // before the arming delay.
        assert_eq!(
            dialog.key_input(TrustedDialogKey::Other, false, start),
            None
        );
        assert_eq!(dialog.key_input(TrustedDialogKey::G, false, start), None);
        assert_eq!(
            dialog.key_input(TrustedDialogKey::G, false, start + UPGRADE_ARMING_DELAY),
            Some(TrustedInput::Answer(true))
        );
        // A click that answers nothing engages and redraws the same way.
        let mut clicked = consent_two_durations();
        clicked.lay_out(frame, start).unwrap();
        clicked.engage(start);
        assert_eq!(clicked.row_input(0, start), Some(TrustedInput::Redraw));
        clicked.render(start).unwrap();
        assert_eq!(clicked.row_input(0, start), None);
    }

    /// While the dialog cannot say yes, its Allow label and key are drawn
    /// off; the Deny label never is.
    #[test]
    fn a_hidden_request_draws_its_allow_button_off() {
        let mut dialog = consent_two_durations();
        let frame = clipped_frame();
        let places = dialog_text::chrome_rows(frame.grid_rows);
        let snapshot = dialog.lay_out(frame, Instant::now()).unwrap();
        assert!(dialog.clipped());
        let rows = snapshot_rows(&snapshot);
        let labels = &rows[places.label];
        let off = u32::try_from(dialog_text::DialogInk::AllowOff.slot().unwrap()).unwrap();
        let allow = labels.find("Allow Look once").unwrap();
        assert_eq!(ink_at(&snapshot, places.label, allow), off);
        let deny = labels.find("Deny").unwrap();
        assert_eq!(
            ink_at(&snapshot, places.label, deny),
            u32::try_from(dialog_text::DialogInk::DenyLabel.slot().unwrap()).unwrap()
        );
        // A label too long for its button hides text as a cut body would.
        let (decision, _answers) = std::sync::mpsc::channel();
        let mut long = TrustedConsentUi::restart(decision).with_text(DialogText {
            lines: vec![
                DialogLine::new("Restart?"),
                DialogLine::deny("Not now"),
                DialogLine::allow("Restart now, end all running shells"),
            ],
            minimum_rows: 0,
        });
        let start = Instant::now();
        long.lay_out(narrow_frame(), start).unwrap();
        assert!(long.clipped());
        click_inside(&mut long, start);
        let armed = start + UPGRADE_ARMING_DELAY;
        assert_eq!(long.key_input(TrustedDialogKey::R, false, armed), None);
        assert_eq!(long.action_area_input(true, armed), None);
        assert_eq!(
            long.key_input(TrustedDialogKey::Escape, false, armed),
            Some(TrustedInput::Answer(false))
        );
    }

    /// The top row of a dialog's snapshot, as text, without trailing blanks.
    fn top_row(snapshot: &TerminalSnapshot) -> String {
        snapshot.visible_rows[0]
            .cells
            .iter()
            .map(|cell| cell.content.as_str())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    /// Where the asker's deadline is known, the frame's top edge shows the
    /// time left, rounded up, at its right end; the Window wakes as each
    /// second passes, and redraws only when the time shown is out of date.
    /// It is display only: once the time is up it shows "0:00", stops
    /// waking, and the answers work exactly as before.
    #[test]
    fn a_known_deadline_counts_down_at_the_frame_right_and_changes_nothing() {
        let (decision, _answers) = std::sync::mpsc::channel();
        let before = Instant::now();
        let mut dialog = TrustedConsentUi::upgrade(decision)
            .with_respond_within(Duration::from_secs(125))
            .with_text(consent_text(0));
        // The asker started its clock first, so the dialog counts down a
        // second short of its wait.
        let set = dialog.deadline.unwrap();
        assert!(set >= before + Duration::from_secs(124), "{set:?}");
        assert!(set <= Instant::now() + Duration::from_secs(124), "{set:?}");
        let start = Instant::now();
        dialog.deadline = Some(start + Duration::from_secs(125));
        // Nothing to count down before the dialog has a layout.
        assert_eq!(dialog.countdown_wait(start), None);
        assert!(!dialog.countdown_stale(start));
        let snapshot = dialog.lay_out(narrow_frame(), start).unwrap();
        let top = top_row(&snapshot);
        assert!(top.ends_with(" cancels in 2:05"), "{top:?}");
        // It ends inside the frame, two columns in from the grid's edge.
        assert_eq!(top.chars().count(), 40 - dialog_text::FRAME_INSET_COLUMNS);
        let millisecond = Duration::from_millis(1);
        assert_eq!(
            dialog.countdown_wait(start),
            Some(Duration::from_secs(1) + millisecond)
        );
        let into = start + Duration::from_millis(400);
        assert!(!dialog.countdown_stale(into));
        assert_eq!(
            dialog.countdown_wait(into),
            Some(Duration::from_millis(600) + millisecond)
        );
        let next = start + Duration::from_secs(1) + millisecond;
        assert!(dialog.countdown_stale(next));
        // Out of date: wake at once.
        assert_eq!(dialog.countdown_wait(next), Some(Duration::ZERO));
        assert!(top_row(&dialog.render(next).unwrap()).ends_with("2:04"));
        assert!(!dialog.countdown_stale(next));
        // Half a second left still shows a whole one.
        let deadline = start + Duration::from_secs(125);
        let last = start + Duration::from_millis(124_500);
        assert!(top_row(&dialog.render(last).unwrap()).ends_with("0:01"));
        assert_eq!(dialog.countdown_wait(deadline), Some(Duration::ZERO));
        let after = deadline + Duration::from_secs(10);
        assert!(top_row(&dialog.render(after).unwrap()).ends_with("0:00"));
        assert_eq!(dialog.countdown_wait(after), None);
        assert!(!dialog.countdown_stale(after));
        // The answers ignore it.
        dialog.engage(after);
        let armed = after + UPGRADE_ARMING_DELAY;
        assert_eq!(
            dialog.key_input(TrustedDialogKey::Escape, false, armed),
            Some(TrustedInput::Answer(false))
        );
        assert_eq!(
            dialog.key_input(TrustedDialogKey::G, false, armed),
            Some(TrustedInput::Answer(true))
        );
        assert_eq!(
            dialog.action_area_input(true, armed),
            Some(TrustedInput::Answer(true))
        );
    }

    /// A dialog that does not know its deadline shows no countdown and
    /// never wakes for one: an access consent and a question whose askers
    /// sent a zero wait.
    #[test]
    fn without_a_known_deadline_nothing_counts_down() {
        let (decision, _answers) = std::sync::mpsc::channel();
        let restart = TrustedConsentUi::restart(decision)
            .with_respond_within(Duration::ZERO)
            .with_text(consent_text(0));
        for mut dialog in [consent_two_durations(), restart] {
            let start = Instant::now();
            let snapshot = dialog.lay_out(narrow_frame(), start).unwrap();
            assert!(
                !top_row(&snapshot).contains(':'),
                "{:?}",
                top_row(&snapshot)
            );
            let later = start + Duration::from_secs(300);
            assert_eq!(dialog.countdown_wait(later), None);
            assert!(!dialog.countdown_stale(later));
        }
    }

    /// A click inside that lands on no cell (in the padding around the
    /// grid) engages the dialog and asks for one redraw, so the legend
    /// says the keys are live; once redrawn, such clicks ask for nothing.
    #[test]
    fn a_click_outside_every_cell_redraws_the_legend_once() {
        let mut dialog = consent_dialog();
        let start = Instant::now();
        dialog.lay_out(narrow_frame(), start).unwrap();
        assert_eq!(dialog.engagement_redraw(), None);
        dialog.engage(start);
        assert_eq!(dialog.engagement_redraw(), Some(TrustedInput::Redraw));
        let redrawn = snapshot_text(&dialog.render(start).unwrap());
        assert!(!redrawn.contains("click inside"), "{redrawn}");
        assert_eq!(dialog.engagement_redraw(), None);
    }

    /// The restart question's countdown says that the time running out
    /// means Not now.
    #[test]
    fn the_restart_countdown_says_not_now() {
        let (decision, _answers) = std::sync::mpsc::channel();
        let mut dialog = TrustedConsentUi::restart(decision)
            .with_respond_within(Duration::from_secs(120))
            .with_text(consent_text(0));
        let top = top_row(&dialog.lay_out(narrow_frame(), Instant::now()).unwrap());
        assert!(top.ends_with(" not now in 1:59"), "{top:?}");
    }

    /// The access consent counts its asker's wait down as "denies in". Once
    /// that has run out it grants nothing: `G` and the green button are
    /// inert and the Allow button is drawn disabled, while a deny still
    /// answers, until the asker, which has stopped waiting, ends it and the
    /// request ends unanswered. The upgrade and restart questions are
    /// unchanged.
    #[test]
    fn the_access_consent_grants_nothing_once_its_time_runs_out() {
        let (decision, answers) = std::sync::mpsc::channel();
        let mut dialog = TrustedConsentUi::access_consent(
            decision,
            vec![
                (GrantDuration::Once, true),
                (GrantDuration::WorkspaceSession, true),
            ],
            Box::new(consent_text),
        )
        .with_respond_within(Duration::from_secs(20));
        let start = Instant::now();
        dialog.deadline = Some(start + Duration::from_secs(20));
        let top = top_row(&dialog.lay_out(narrow_frame(), start).unwrap());
        assert!(top.ends_with(" denies in 0:20"), "{top:?}");
        // Before the time is up a grant counts.
        click_inside(&mut dialog, start);
        let armed = start + UPGRADE_ARMING_DELAY;
        assert!(dialog.chrome(armed).allow_enabled);
        assert_eq!(
            dialog.key_input(TrustedDialogKey::G, false, armed),
            Some(TrustedInput::Answer(true))
        );
        assert_eq!(
            dialog.action_area_input(true, armed),
            Some(TrustedInput::Answer(true))
        );
        // Once it is up, nothing grants, and the redraw at "0:00" shows
        // Allow disabled; every deny still answers.
        let deadline = start + Duration::from_secs(20);
        assert!(dialog.countdown_stale(deadline));
        let redrawn = top_row(&dialog.render(deadline).unwrap());
        assert!(redrawn.ends_with(" denies in 0:00"), "{redrawn:?}");
        for at in [deadline, deadline + Duration::from_secs(5)] {
            assert!(!dialog.chrome(at).allow_enabled);
            assert_eq!(dialog.key_input(TrustedDialogKey::G, false, at), None);
            assert_eq!(dialog.action_area_input(true, at), None);
            for key in [
                TrustedDialogKey::D,
                TrustedDialogKey::Escape,
                TrustedDialogKey::Enter,
            ] {
                assert_eq!(
                    dialog.key_input(key, false, at),
                    Some(TrustedInput::Answer(false)),
                    "{key:?}"
                );
            }
            assert_eq!(
                dialog.action_area_input(false, at),
                Some(TrustedInput::Answer(false))
            );
        }
        dialog.answer(false);
        assert_eq!(answers.try_recv().unwrap(), ConsentDecision::Deny);
        // A question's time running out changes none of its answers.
        let (decision, _answers) = std::sync::mpsc::channel();
        let mut question = TrustedConsentUi::upgrade(decision)
            .with_respond_within(Duration::from_secs(20))
            .with_text(consent_text(0));
        let start = Instant::now();
        question.deadline = Some(start + Duration::from_secs(20));
        question.lay_out(narrow_frame(), start).unwrap();
        question.engage(start);
        let after = start + Duration::from_secs(25);
        assert!(question.chrome(after).allow_enabled);
        assert_eq!(
            question.key_input(TrustedDialogKey::G, false, after),
            Some(TrustedInput::Answer(true))
        );
    }

    /// An access consent whose countdown the top edge has no room for
    /// counts as hidden text: nothing grants until the time left shows
    /// again. A question's countdown just goes, as before.
    #[test]
    fn an_access_consent_grants_nothing_while_its_countdown_is_hidden() {
        let titled = |selected: usize| {
            let mut text = consent_text(selected);
            text.lines
                .insert(0, DialogLine::title("remote agent access request"));
            text
        };
        let (decision, _answers) = std::sync::mpsc::channel();
        let mut dialog = TrustedConsentUi::access_consent(
            decision,
            vec![
                (GrantDuration::Once, true),
                (GrantDuration::WorkspaceSession, true),
            ],
            Box::new(titled),
        )
        .with_respond_within(Duration::from_secs(20));
        let start = Instant::now();
        dialog.deadline = Some(start + Duration::from_secs(20));
        // 40 columns: the buttons fit their halves, but the title and
        // "denies in 0:20" need 46 inside the frame.
        let narrow = DialogFrame {
            wrap_columns: 36,
            grid_columns: 40,
            grid_rows: 20,
            max_text_rows: 20,
        };
        assert!(titled(0).buttons_fit(40));
        let snapshot = dialog.lay_out(narrow, start).unwrap();
        assert!(dialog.clipped());
        assert!(!top_row(&snapshot).contains("denies in"));
        click_inside(&mut dialog, start);
        let armed = start + UPGRADE_ARMING_DELAY;
        assert_eq!(dialog.key_input(TrustedDialogKey::G, false, armed), None);
        assert_eq!(dialog.action_area_input(true, armed), None);
        assert_eq!(
            dialog.key_input(TrustedDialogKey::D, false, armed),
            Some(TrustedInput::Answer(false))
        );
        // With room for both, it grants once armed again.
        let wide = DialogFrame {
            wrap_columns: 60,
            grid_columns: 64,
            grid_rows: 20,
            max_text_rows: 20,
        };
        let top = top_row(&dialog.lay_out(wide, armed).unwrap());
        assert!(top.ends_with(" denies in 0:20"), "{top:?}");
        assert!(!dialog.clipped());
        assert_eq!(dialog.key_input(TrustedDialogKey::G, false, armed), None);
        assert_eq!(
            dialog.key_input(TrustedDialogKey::G, false, armed + UPGRADE_ARMING_DELAY),
            Some(TrustedInput::Answer(true))
        );
        // The upgrade question at the same width only drops its countdown.
        let mut question = TrustedConsentUi::upgrade(std::sync::mpsc::channel().0)
            .with_respond_within(Duration::from_secs(20))
            .with_text(titled(0));
        let snapshot = question.lay_out(narrow, Instant::now()).unwrap();
        assert!(!question.clipped());
        assert!(!top_row(&snapshot).contains("cancels in"));
    }
}
