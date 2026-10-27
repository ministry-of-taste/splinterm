//! The Window's daemon handoff (upgrade) banner and input gate
//! (Plan 0061 M3.12a, reconnection M3.12b).
//!
//! The Window's [`HandoffStatusHub`] is fed by every pane's control
//! subscription. Here the frontend folds its view in once per event-loop
//! turn, pauses the local input paths while the Window is paused, and paints
//! the banner. The controllers drop paused commands too, so input queued
//! before this turn never reaches the daemon either.
//!
//! A daemon connection lost while a handoff is preparing or fenced keeps the
//! Window open with its last screen: the hub turns to "Reconnecting…" and the
//! Window's coordinator resnapshots every pane from the serving daemon, or
//! gives up and leaves the Window paused with a lost-connection message.

use std::{
    task::Waker,
    time::{Duration, Instant},
};

use crate::{
    geometry::Rect,
    handoff_status::{
        HandoffBanner, HandoffPresentation, HandoffStatusHub, HandoffTransition, WindowHandoffPhase,
    },
    renderer::SessionPickerTextCache,
};

use splinterm_core::DojoId;

use super::{
    App, DiagnosticErrorCode, DiagnosticEventCode, DiagnosticLevel, PaneView,
    WindowTopologyCommand, diagnostics, terminal_input_pending_for,
};

/// A daemon connection lost while a handoff was preparing or fenced, which
/// kept the Window open instead of ending it.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum HeldFailure {
    /// A pane's update stream ended unexpectedly.
    PaneStream,
    /// The topology manager stopped.
    TopologyManager,
}

impl HeldFailure {
    /// The diagnostic event and error the failure would have reported had
    /// it ended the Window.
    const fn diagnostic(self) -> (DiagnosticEventCode, DiagnosticErrorCode) {
        match self {
            Self::PaneStream => (
                DiagnosticEventCode::PaneStreamFailure,
                DiagnosticErrorCode::PaneStream,
            ),
            Self::TopologyManager => (
                DiagnosticEventCode::TopologyFailure,
                DiagnosticErrorCode::TopologyManager,
            ),
        }
    }
}

/// The frontend's handoff state: its Window's hub, the presented copy, any
/// failure held open, and the banner's text cache.
#[derive(Default)]
pub(super) struct HandoffFrontend {
    hub: Option<HandoffStatusHub>,
    presentation: HandoffPresentation,
    held_failure: Option<HeldFailure>,
    /// The held failure was reported once the Window was declared lost.
    lost_reported: bool,
    pub(super) text_cache: SessionPickerTextCache,
    /// Where the last draw put the banner and its shadow, buffer px: a
    /// spinner frame damages it.
    pub(super) painted_banner: Option<Rect>,
}

impl HandoffFrontend {
    /// Follows `hub`, waking the event loop through `waker` on each change.
    pub(super) fn new(hub: Option<HandoffStatusHub>, waker: &Waker) -> Self {
        if let Some(hub) = &hub {
            hub.set_waker(waker.clone());
        }
        Self {
            hub,
            ..Self::default()
        }
    }

    /// Folds the hub's view in at `now` and expires an outcome notice;
    /// returns the transition, with `changed` also set by an expiry.
    fn fold(&mut self, now: Instant) -> HandoffTransition {
        let Some(view) = self.hub.as_ref().map(HandoffStatusHub::view) else {
            return HandoffTransition::default();
        };
        let mut transition = self.presentation.apply(view, now);
        transition.changed |= self.presentation.expire(now);
        transition
    }

    /// Whether terminal input, resize, and daemon-mutating actions are paused.
    pub(super) const fn input_paused(&self) -> bool {
        self.presentation.input_paused()
    }

    /// The hub's phase now, which can be ahead of the presented copy.
    fn hub_phase(&self) -> WindowHandoffPhase {
        self.hub
            .as_ref()
            .map_or(WindowHandoffPhase::Idle, |hub| hub.view().phase)
    }

    /// Whether a lost daemon connection keeps the Window open with its
    /// banner rather than ending it: a handoff was preparing or fenced, so
    /// the loss is most likely its exec, or the Window is already
    /// reconnecting or lost. Read from the hub, not the presented copy,
    /// because a stream can fail before this turn folds in.
    pub(super) fn holds_window_on_disconnect(&self) -> bool {
        self.hub_phase().input_paused()
    }

    /// Whether the Window has no usable daemon connection: while it
    /// reconnects or after it gave up, no topology request is sent at all.
    pub(super) fn disconnected(&self) -> bool {
        self.hub_phase().disconnected()
    }

    /// Records a failure the Window was kept open across and starts
    /// reconnection, or joins the one already running.
    pub(super) fn hold(&mut self, failure: HeldFailure) {
        self.held_failure = Some(self.held_failure.map_or(failure, |held| held.max(failure)));
        if let Some(hub) = &self.hub {
            let _ = hub.begin_reconnect();
        }
    }

    /// Nothing is left to reconnect the Window, as when its topology manager
    /// stopped: it is lost.
    pub(super) fn give_up(&mut self, failure: HeldFailure) {
        self.hold(failure);
        if let Some(hub) = &self.hub {
            hub.reconnect_failed();
        }
    }

    /// The coordinator resnapshotted every pane from the daemon serving
    /// `daemon_generation` for reconnection `epoch`: the held failure is
    /// gone and the pause ends, unless another connection was lost since
    /// that epoch began, which keeps the Window reconnecting.
    pub(super) fn reconnected(&mut self, daemon_generation: u64, epoch: u64) {
        if self
            .hub
            .as_ref()
            .is_none_or(|hub| hub.reconnected(daemon_generation, epoch))
        {
            self.held_failure = None;
            self.lost_reported = false;
        }
    }

    /// The failure to report once the Window is declared lost, at most once.
    fn take_lost_report(&mut self) -> Option<HeldFailure> {
        if self.presentation.phase() != WindowHandoffPhase::Lost || self.lost_reported {
            return None;
        }
        self.lost_reported = true;
        self.held_failure
    }

    /// The banner's text, also appended to the Window title.
    pub(super) fn banner_text(&self) -> Option<String> {
        self.presentation.banner().map(HandoffBanner::text)
    }

    /// The banner the Window shows, if any.
    pub(super) fn banner(&self) -> Option<HandoffBanner> {
        self.presentation.banner()
    }

    /// Whether the banner shows a running upgrade, whose spinner moves.
    pub(super) fn banner_working(&self) -> bool {
        self.presentation
            .banner()
            .is_some_and(HandoffBanner::is_working)
    }

    /// How long the event loop may sleep before an outcome notice expires.
    pub(super) fn timeout(&self, now: Instant) -> Option<Duration> {
        self.presentation
            .deadline()
            .map(|deadline| deadline.saturating_duration_since(now))
    }
}

/// The tab a topology command switches to when the Window is disconnected
/// (reconnecting or lost), if it is a plain tab switch: the one topology
/// command that touches no daemon state, so it still happens then. Every
/// other command is dropped until the Window is connected again.
pub(super) const fn tab_switch_while_disconnected(
    command: &WindowTopologyCommand,
) -> Option<DojoId> {
    match command {
        WindowTopologyCommand::ActivateTab { dojo_id } => Some(*dojo_id),
        _ => None,
    }
}

/// The focus report a pane's terminal hears when a handoff pause ends: the
/// reports sent while paused were dropped, so a terminal that asked for them
/// hears its current focus again.
pub(super) const fn focus_report_on_resume(focus_reporting: bool, focused: bool) -> Option<bool> {
    if focus_reporting { Some(focused) } else { None }
}

impl App {
    /// Whether terminal input and daemon mutations are paused for a handoff.
    pub(super) const fn handoff_paused(&self) -> bool {
        self.handoff.input_paused()
    }

    /// Folds the hub's view in; returns whether the banner, the title, or the
    /// pause changed, which repaints the Window.
    pub(super) fn apply_handoff_status(&mut self, now: Instant) -> bool {
        let transition = self.handoff.fold(now);
        if transition.pause_changed {
            self.handoff_pause_changed();
        }
        if let Some(failure) = self.handoff.take_lost_report() {
            // The Window stays open; the failure that kept it open is still
            // reported, as it would have been had it ended the Window.
            let (event, error) = failure.diagnostic();
            if let Some(diagnostics) = diagnostics() {
                diagnostics.emit(DiagnosticLevel::Error, event, Some(error));
            }
            eprintln!(
                "splinterm: lost connection to splinterd during an upgrade; reopen this Window from the launcher"
            );
        }
        if transition.changed {
            self.update_window_title();
            self.presentation.full_redraw = true;
            for buffer in &mut self.surface.buffers {
                buffer.stale.mark_full();
            }
            if self.surface.configured {
                self.scheduling.redraw_pending = true;
            }
        }
        transition.changed
    }

    fn handoff_pause_changed(&mut self) {
        let paused = self.handoff_paused();
        if paused {
            // Typed, pasted, and composed input is dropped, not queued.
            self.input.pending_terminal_input.clear();
            self.input.terminal_input_saturated = false;
            self.input.prefix_state.clear();
            self.cancel_clipboard_image();
        } else {
            self.ready_panes_for_handoff_resume();
            if self.surface.configured
                && let Err(error) = self.emit_resize()
            {
                self.scheduling.fail(error);
            }
        }
        if let Some(palette) = self.modal.command_palette.as_mut()
            && palette.set_mutations_paused(paused)
        {
            self.refresh_command_palette();
        }
        if let Some(menu) = self.modal.tab_context_menu.as_mut()
            && menu.set_mutations_paused(paused)
        {
            self.modal.tab_context_menu_layout = None;
            self.presentation.full_redraw = true;
        }
        // Terminal composition follows the pause; owned fields keep IME.
        if self.input.ime.entered {
            self.enable_text_input_if_allowed();
        }
    }

    /// Resizes and focus reports were dropped while paused: every pane sends
    /// its geometry again, even where it matches the last one sent, and every
    /// terminal that asked for focus reports hears its current focus.
    fn ready_panes_for_handoff_resume(&mut self) {
        let pending = &self.input.pending_terminal_input;
        let active_focus = self.input.terminal_focus_reported;
        Self::ready_pane_for_handoff_resume(&mut self.panes.pane, active_focus, pending);
        for pane in &mut self.panes.inactive_panes {
            Self::ready_pane_for_handoff_resume(pane, false, pending);
        }
        for tab in self.tab_state.tabs.iter_mut() {
            if let Some(view) = tab.value.as_mut() {
                for pane in std::iter::once(&mut view.pane).chain(view.inactive_panes.iter_mut()) {
                    Self::ready_pane_for_handoff_resume(pane, false, pending);
                }
            }
        }
    }

    /// Readies one pane for the end of a handoff pause; `focused` is the
    /// focus its terminal should believe.
    pub(super) fn ready_pane_for_handoff_resume(
        pane: &mut PaneView,
        focused: bool,
        pending: &std::collections::VecDeque<super::PendingTerminalInput>,
    ) {
        pane.last_resize = None;
        let focus_reporting = pane
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.input_modes.focus_reporting);
        if let Some(focused) = focus_report_on_resume(focus_reporting, focused) {
            let input_pending = terminal_input_pending_for(pending, pane.commands.as_ref());
            Self::request_pane_focus_report(pane, focused, input_pending);
        }
    }

    /// Drops a topology command the handoff pause holds back, undoing the
    /// local state its entry point set up while awaiting the topology
    /// manager, so no picker, Explorer row, or split placeholder is left
    /// waiting for an answer that never comes.
    pub(super) fn drop_topology_command_for_handoff(&mut self, command: &WindowTopologyCommand) {
        let command = match command {
            WindowTopologyCommand::ExplorerContext { command, .. } => command.as_ref(),
            command => command,
        };
        match command {
            WindowTopologyCommand::Split {
                target,
                pending: Some(pending),
                ..
            } => {
                if let Err(error) = self.rollback_pending_remote_split(*target, *pending) {
                    self.scheduling.fail(error);
                }
            }
            WindowTopologyCommand::RequestLairPrompt { .. }
            | WindowTopologyCommand::RequestDojoPrompt { .. }
            | WindowTopologyCommand::RequestDojoRestorePrompt { .. }
            | WindowTopologyCommand::RequestSelector { .. }
            | WindowTopologyCommand::RequestSessionPicker => {
                self.modal.session_picker_requested = false;
            }
            WindowTopologyCommand::OpenDojo { .. }
            | WindowTopologyCommand::FocusSplint { .. }
            | WindowTopologyCommand::NavigateLair { .. }
            | WindowTopologyCommand::NewLair { .. }
            | WindowTopologyCommand::PickerNewLair { .. }
            | WindowTopologyCommand::NewDojo { .. }
            | WindowTopologyCommand::PickerNewDojo { .. } => {
                self.tab_state.session_switch_pending = false;
                self.explorer.clear_pending();
            }
            _ => {}
        }
        eprintln!("splinterm: action paused while splinterd upgrades");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::task::Wake;

    use splinterm_protocol::{HandoffCancelReason, HandoffStatusPhase};

    use super::*;

    struct NoWake;

    impl Wake for NoWake {
        fn wake(self: Arc<Self>) {}
    }

    fn frontend(hub: &HandoffStatusHub) -> HandoffFrontend {
        HandoffFrontend::new(Some(hub.clone()), &Waker::from(Arc::new(NoWake)))
    }

    #[test]
    fn a_failure_held_during_preparing_reconnects_and_a_reconnection_clears_it() {
        let hub = HandoffStatusHub::default();
        let mut frontend = frontend(&hub);
        let live = hub.source();
        live.observe(1, HandoffStatusPhase::Preparing);
        assert!(frontend.fold(Instant::now()).pause_changed);
        // The exec race: a pane stream ends after only Preparing was heard.
        assert!(frontend.holds_window_on_disconnect());
        frontend.hold(HeldFailure::PaneStream);
        frontend.hold(HeldFailure::TopologyManager);
        frontend.hold(HeldFailure::PaneStream);
        assert!(!frontend.fold(Instant::now()).pause_changed);
        assert_eq!(
            frontend.presentation.phase(),
            WindowHandoffPhase::Reconnecting
        );
        assert!(frontend.disconnected());
        assert_eq!(frontend.held_failure, Some(HeldFailure::TopologyManager));
        // A live pane's Cancelled does not end the pause: the dead pieces
        // have no stream until the coordinator replaces them.
        live.observe(
            1,
            HandoffStatusPhase::Cancelled {
                reason: HandoffCancelReason::Failed,
            },
        );
        assert!(!frontend.fold(Instant::now()).pause_changed);
        assert!(frontend.input_paused());
        // The resnapshot lands: the held failure is cleared and input resumes.
        let epoch = hub.reconnect_pending().unwrap();
        frontend.reconnected(1, epoch);
        let transition = frontend.fold(Instant::now());
        assert!(transition.pause_changed && !frontend.input_paused());
        assert_eq!(frontend.held_failure, None);
        assert!(!frontend.disconnected());
        assert_eq!(frontend.banner_text(), None);
    }

    #[test]
    fn a_window_that_gives_up_stays_open_paused_and_reports_once() {
        let hub = HandoffStatusHub::default();
        let mut frontend = frontend(&hub);
        hub.source().observe(1, HandoffStatusPhase::Fenced);
        frontend.fold(Instant::now());
        frontend.give_up(HeldFailure::TopologyManager);
        frontend.fold(Instant::now());
        assert!(frontend.input_paused());
        assert!(frontend.holds_window_on_disconnect());
        assert_eq!(
            frontend.banner_text().as_deref(),
            Some("Lost connection to splinterd — reopen from the launcher")
        );
        assert_eq!(
            frontend.take_lost_report(),
            Some(HeldFailure::TopologyManager)
        );
        assert_eq!(frontend.take_lost_report(), None);
    }

    #[test]
    fn an_ordinary_loss_with_no_handoff_is_not_held() {
        let hub = HandoffStatusHub::default();
        let frontend = frontend(&hub);
        assert!(!frontend.holds_window_on_disconnect());
        assert!(!frontend.disconnected());
    }

    #[test]
    fn only_a_plain_tab_switch_happens_while_disconnected() {
        let dojo_id = splinterm_core::DojoId::new();
        assert_eq!(
            tab_switch_while_disconnected(&WindowTopologyCommand::ActivateTab { dojo_id }),
            Some(dojo_id)
        );
        for command in [
            WindowTopologyCommand::CloseTab { dojo_id },
            WindowTopologyCommand::RequestSessionPicker,
            WindowTopologyCommand::RequestLairExplorer {
                focused_splint: None,
            },
        ] {
            assert_eq!(tab_switch_while_disconnected(&command), None, "{command:?}");
        }
    }

    #[test]
    fn a_terminal_hears_its_focus_again_only_when_it_asked_for_reports() {
        assert_eq!(focus_report_on_resume(true, true), Some(true));
        assert_eq!(focus_report_on_resume(true, false), Some(false));
        assert_eq!(focus_report_on_resume(false, true), None);
    }
}
