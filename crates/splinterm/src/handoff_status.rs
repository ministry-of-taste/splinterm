//! Window-wide daemon handoff (upgrade) status for the graphical client
//! (Plan 0061 M3.12a, reconnection M3.12b).
//!
//! The daemon broadcasts `SubscriptionEvent::HandoffStatus` on every control
//! subscription, and each pane of a Window holds one. Every pane's controller
//! reports what its subscription delivers to one [`HandoffStatusHub`] per
//! Window, so the first pane to hear a phase pauses the whole Window, and the
//! hub drops duplicates by daemon generation and phase.
//!
//! The hub keeps each pane's last phase apart, since subscriptions deliver at
//! slightly different times. The Window pauses while any pane reports
//! `Preparing` or `Fenced`, and resumes once every pane that reported the
//! attempt also reported `Cancelled`, so one late pane cannot reopen input
//! early or replay an attempt that already ended.
//!
//! While paused, only the local UI works: terminal input is dropped, not
//! queued, and daemon-mutating actions are disabled.
//!
//! A connection lost while a handoff is preparing or fenced is how a
//! committed handoff looks from here: the daemon execs, and `Fenced` is
//! queued per subscription, so a pane may see only `Preparing` before the
//! loss. The Window then shows "Reconnecting…" and stays paused while its
//! coordinator (the topology manager, or the single-pane loop) reconnects
//! for up to [`RECONNECT_WINDOW`] and resnapshots every pane from the serving
//! daemon; [`HandoffStatusHub::reconnected`] ends the pause, and
//! [`HandoffStatusHub::reconnect_failed`] leaves the Window open, paused,
//! with its last screen and a lost-connection message. Nothing typed while
//! paused is replayed.
//!
//! A local Window that takes part in the handoff (Plan 0061 M3.14) instead
//! holds [`WindowHandoffPhase::Continuing`] from the moment it accepts its
//! prepare frame until the continuation reports the outcome. Its ordinary
//! reconnection is gated meanwhile: a connection lost then is the expected
//! exec, and is only remembered ([`HandoffStatusHub::begin_reconnect`] still
//! answers that the Window holds). An adoption or a rollback relaunches the
//! Window's process, so nothing reconnects; a cancelled attempt ends the
//! participation ([`ContinuationParticipation`]), which starts the ordinary
//! reconnection if a connection was lost meanwhile. A relaunched Window that
//! finds the old generation serving shows "Rolled back"
//! ([`HandoffStatusHub::rolled_back`]).

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    task::Waker,
    time::{Duration, Instant},
};

use splinterm_protocol::{HandoffCancelReason, HandoffStatusPhase};

/// How long an outcome notice, "Upgrade cancelled" or "Rolled back", stays.
pub const HANDOFF_NOTICE_DURATION: Duration = Duration::from_secs(3);

/// How long a Window keeps reconnecting after its daemon connection was lost
/// during a handoff before it gives up and shows the lost connection.
pub const RECONNECT_WINDOW: Duration = Duration::from_secs(30);

/// The Window-wide handoff phase.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WindowHandoffPhase {
    /// No handoff: the Window works normally.
    #[default]
    Idle,
    /// A pane heard `Preparing`. Input and daemon mutations are paused.
    Preparing,
    /// A pane heard `Fenced`. Input and daemon mutations are paused.
    Fenced,
    /// A connection was lost while preparing or fenced, as when a committed
    /// handoff execs. Paused while the Window reconnects and resnapshots.
    Reconnecting,
    /// Reconnection failed for [`RECONNECT_WINDOW`]. The Window stays open,
    /// paused, with its last screen; its terminals may still be running in
    /// the daemon.
    Lost,
    /// The Window takes part in the handoff and waits for its continuation's
    /// outcome (M3.14). Paused, with no daemon request sent; its ordinary
    /// reconnection waits for the outcome.
    Continuing,
}

impl WindowHandoffPhase {
    /// Whether terminal input, resize, and daemon mutations are paused.
    #[must_use]
    pub const fn input_paused(self) -> bool {
        !matches!(self, Self::Idle)
    }

    /// Whether the Window has no usable daemon connection right now: every
    /// daemon request, reads included, waits for reconnection or never runs.
    #[must_use]
    pub const fn disconnected(self) -> bool {
        matches!(self, Self::Reconnecting | Self::Lost | Self::Continuing)
    }
}

/// A short-lived outcome shown after an attempt ends.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandoffNotice {
    /// The attempt stopped before exec; the same daemon keeps serving.
    Cancelled(HandoffCancelReason),
    /// The new daemon failed and the old one resumed (M3.14).
    RolledBack,
}

/// What the banner shows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandoffBanner {
    Preparing,
    InputPaused,
    Reconnecting,
    Lost,
    Notice(HandoffNotice),
}

impl HandoffBanner {
    /// The banner text, also appended to the Window title.
    #[must_use]
    pub fn text(self) -> String {
        match self {
            Self::Preparing => "Preparing upgrade…".to_owned(),
            Self::InputPaused => "Upgrading — input paused".to_owned(),
            Self::Reconnecting => "Reconnecting…".to_owned(),
            Self::Lost => "Lost connection to splinterd — reopen from the launcher".to_owned(),
            Self::Notice(HandoffNotice::Cancelled(reason)) => {
                format!("Upgrade cancelled — {}", cancel_reason_words(reason))
            }
            Self::Notice(HandoffNotice::RolledBack) => "Rolled back".to_owned(),
        }
    }

    /// Whether the handoff is still under way: preparing, paused or
    /// reconnecting. The banner then shows a spinner, which says what
    /// the text's trailing `…` does.
    #[must_use]
    pub const fn is_working(self) -> bool {
        matches!(
            self,
            Self::Preparing | Self::InputPaused | Self::Reconnecting
        )
    }

    /// The banner's label beside its mark: [`Self::text`], without the
    /// trailing `…` a spinner takes the place of.
    #[must_use]
    pub fn label(self) -> String {
        let text = self.text();
        if self.is_working() {
            text.trim_end_matches('…').to_owned()
        } else {
            text
        }
    }
}
/// Why an upgrade was cancelled, in plain words.
#[must_use]
pub const fn cancel_reason_words(reason: HandoffCancelReason) -> &'static str {
    match reason {
        HandoffCancelReason::Requested => "stopped on request",
        HandoffCancelReason::Blocked => "something blocked it",
        HandoffCancelReason::Failed => "preparation failed, nothing changed",
        HandoffCancelReason::TimedOut => "a step took too long",
    }
}

/// A snapshot of the hub for the frontend.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HandoffStatusView {
    pub phase: WindowHandoffPhase,
    /// Bumped on every change, so a frontend can skip unchanged views.
    pub revision: u64,
    /// The latest outcome notice with its serial number.
    pub notice: Option<(u64, HandoffNotice)>,
}

/// What one pane's subscription last reported in this generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourcePhase {
    Preparing,
    Fenced,
}

#[derive(Debug, Default)]
struct HandoffState {
    /// The newest daemon generation heard; zero before any status.
    generation: u64,
    sources: HashMap<u64, SourcePhase>,
    /// Some pane heard this attempt's `Fenced`. Kept until every pane that
    /// reported the attempt ends it, so a slower pane still at `Preparing`
    /// cannot step the banner back.
    fenced: bool,
    /// Panes that already ended this attempt while others still report it.
    /// One of them hearing `Preparing` starts the next attempt, unfenced.
    ended: HashSet<u64>,
    reconnect: Reconnect,
    /// Bumped each time reconnection starts, so a coordinator that already
    /// answered one loss can tell a later one from it.
    reconnect_epoch: u64,
    revision: u64,
    notice: Option<(u64, HandoffNotice)>,
    notices: u64,
    /// Set while the Window takes part in a handoff (M3.14): whether a
    /// connection was lost since it began.
    continuation: Option<ContinuationState>,
}

/// A Window waiting for its continuation's outcome.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ContinuationState {
    /// A connection was lost meanwhile; ending the participation without a
    /// relaunch starts the ordinary reconnection.
    lost_connection: bool,
}

/// Where the Window stands with a daemon connection lost during a handoff.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Reconnect {
    #[default]
    Connected,
    Pending,
    Lost,
}

impl HandoffState {
    fn phase(&self) -> WindowHandoffPhase {
        if self.reconnect == Reconnect::Lost {
            WindowHandoffPhase::Lost
        } else if self.continuation.is_some() {
            WindowHandoffPhase::Continuing
        } else if self.reconnect == Reconnect::Pending {
            WindowHandoffPhase::Reconnecting
        } else if self.sources.is_empty() {
            WindowHandoffPhase::Idle
        } else if self.fenced {
            WindowHandoffPhase::Fenced
        } else {
            WindowHandoffPhase::Preparing
        }
    }

    fn view(&self) -> HandoffStatusView {
        HandoffStatusView {
            phase: self.phase(),
            revision: self.revision,
            notice: self.notice,
        }
    }

    /// Runs `change` and bumps the revision when the visible state changed.
    fn update(&mut self, change: impl FnOnce(&mut Self)) -> bool {
        let before = (self.phase(), self.notice);
        change(self);
        if self.sources.is_empty() {
            // The attempt ended for every pane; the next one starts unfenced.
            self.fenced = false;
            self.ended.clear();
        }
        let changed = before != (self.phase(), self.notice);
        if changed {
            self.revision = self.revision.wrapping_add(1);
        }
        changed
    }

    /// Starts taking part in a handoff; refused once the Window is lost or
    /// while it already takes part.
    fn begin_continuation(&mut self) -> bool {
        if self.reconnect == Reconnect::Lost || self.continuation.is_some() {
            return false;
        }
        self.update(|state| state.continuation = Some(ContinuationState::default()));
        true
    }

    /// Ends the participation without a relaunch. A connection lost meanwhile
    /// starts the ordinary reconnection now, under a new epoch.
    fn end_continuation(&mut self) -> bool {
        self.update(|state| {
            if let Some(continuation) = state.continuation.take()
                && continuation.lost_connection
                && state.reconnect != Reconnect::Lost
            {
                state.reconnect = Reconnect::Pending;
                state.reconnect_epoch = state.reconnect_epoch.wrapping_add(1);
            }
        })
    }

    fn post_notice(&mut self, notice: HandoffNotice) {
        self.notices = self.notices.wrapping_add(1);
        self.notice = Some((self.notices, notice));
    }

    fn observe(&mut self, source: u64, generation: u64, phase: HandoffStatusPhase) -> bool {
        // Generation zero is never valid, and an older daemon's report is
        // stale once a newer one was heard. A lost Window has no connection
        // left that could report anything current.
        if generation == 0 || generation < self.generation || self.reconnect == Reconnect::Lost {
            return false;
        }
        self.update(|state| {
            if generation > state.generation {
                // A newer daemon: whatever older generations reported is void.
                // A reconnection in progress still ends only when its
                // coordinator has resnapshotted every pane.
                state.generation = generation;
                state.sources.clear();
                state.fenced = false;
                state.ended.clear();
            }
            match phase {
                HandoffStatusPhase::Preparing => {
                    if state.ended.contains(&source) {
                        // A pane that ended the last attempt starts the next:
                        // every pane that ended it is in the next one too,
                        // and the panes still ending the last one no longer
                        // decide the banner.
                        state.ended.clear();
                        state.fenced = false;
                    }
                    let previous = state.sources.insert(source, SourcePhase::Preparing);
                    if previous == Some(SourcePhase::Fenced) {
                        // This pane went on to a next attempt, so it lost the
                        // last one's `Cancelled`; only another pane's report
                        // can keep the Window fenced.
                        state.fenced = state
                            .sources
                            .values()
                            .any(|phase| *phase == SourcePhase::Fenced);
                    }
                }
                HandoffStatusPhase::Fenced => {
                    state.sources.insert(source, SourcePhase::Fenced);
                    state.fenced = true;
                }
                HandoffStatusPhase::Cancelled { reason } => {
                    let reported = state.sources.remove(&source).is_some();
                    if reported && !state.sources.is_empty() {
                        state.ended.insert(source);
                    }
                    // The attempt ends once every pane that reported it has
                    // cancelled. A `Cancelled` for an attempt nobody reported,
                    // as replayed after a lag, changes nothing. While some
                    // pane's connection is being replaced, the pause lasts
                    // until that pane is back, so no notice is shown.
                    if state.sources.is_empty()
                        && reported
                        && state.reconnect == Reconnect::Connected
                    {
                        state.post_notice(HandoffNotice::Cancelled(reason));
                    }
                }
            }
        })
    }

    /// A pane's subscription ended normally: it was closed or cancelled.
    fn remove_source(&mut self, source: u64) -> bool {
        self.update(|state| {
            state.sources.remove(&source);
            state.ended.remove(&source);
        })
    }

    /// Starts reconnecting when a handoff is preparing or fenced, or joins
    /// the reconnection already running; returns whether one is running.
    fn begin_reconnect(&mut self) -> bool {
        if let Some(continuation) = &mut self.continuation {
            // The expected exec of a handoff this Window takes part in: the
            // Window holds, and reconnects only if the attempt ends without
            // relaunching it.
            continuation.lost_connection = true;
            return true;
        }
        match self.phase() {
            WindowHandoffPhase::Preparing | WindowHandoffPhase::Fenced => {
                self.reconnect = Reconnect::Pending;
                self.reconnect_epoch = self.reconnect_epoch.wrapping_add(1);
                true
            }
            WindowHandoffPhase::Reconnecting => {
                // Another loss while a reconnection runs, as when a pane it
                // just resnapshotted dies too: the coordinator must not
                // report the earlier epoch as done (review N2).
                self.reconnect_epoch = self.reconnect_epoch.wrapping_add(1);
                true
            }
            WindowHandoffPhase::Idle | WindowHandoffPhase::Lost => false,
            // Answered above, before the phase is read.
            WindowHandoffPhase::Continuing => true,
        }
    }

    /// A pane's connection failed. While a handoff is preparing or fenced
    /// that is most likely the committed handoff's exec, even when the pane
    /// never read `Fenced`, so the Window reconnects.
    fn source_lost(&mut self, source: u64) -> bool {
        self.update(|state| {
            state.begin_reconnect();
            state.sources.remove(&source);
            state.ended.remove(&source);
        })
    }

    /// Every pane was resnapshotted from the daemon at `generation`. A newer
    /// generation voids the older one's reports; at the same generation the
    /// live panes' reports stand, so an attempt still under way keeps the
    /// Window paused.
    /// Ends the reconnection the coordinator answered as `epoch`; returns
    /// whether it did. A connection lost after that epoch began keeps the
    /// Window reconnecting, under the newer epoch.
    fn reconnected(&mut self, generation: u64, epoch: u64) -> bool {
        if self.reconnect == Reconnect::Pending && epoch != self.reconnect_epoch {
            return false;
        }
        self.update(|state| {
            state.reconnect = Reconnect::Connected;
            if generation > state.generation {
                state.generation = generation;
                state.sources.clear();
                state.fenced = false;
                state.ended.clear();
            }
        });
        true
    }

    fn reconnect_failed(&mut self) -> bool {
        self.update(|state| {
            state.reconnect = Reconnect::Lost;
            state.sources.clear();
        })
    }

    fn rolled_back(&mut self) -> bool {
        self.update(|state| {
            state.sources.clear();
            state.continuation = None;
            state.reconnect = Reconnect::Connected;
            state.post_notice(HandoffNotice::RolledBack);
        })
    }
}

#[derive(Debug, Default)]
struct HubInner {
    state: Mutex<HandoffState>,
    next_source: AtomicU64,
    waker: Mutex<Option<Waker>>,
}

/// One Window's handoff status, shared by its panes' controllers and its
/// frontend. Clones share the same state.
#[derive(Clone, Debug, Default)]
pub struct HandoffStatusHub {
    inner: Arc<HubInner>,
}

impl HandoffStatusHub {
    /// Registers one pane subscription that reports to this hub.
    #[must_use]
    pub fn source(&self) -> HandoffStatusSource {
        HandoffStatusSource {
            hub: self.clone(),
            id: self.inner.next_source.fetch_add(1, Ordering::Relaxed),
            finished: false,
        }
    }

    #[must_use]
    pub fn view(&self) -> HandoffStatusView {
        self.lock().view()
    }

    /// Whether terminal input, resize, and daemon mutations are paused now.
    #[must_use]
    pub fn input_paused(&self) -> bool {
        self.lock().phase().input_paused()
    }

    /// Wakes the frontend's event loop whenever the view changes.
    pub fn set_waker(&self, waker: Waker) {
        *self
            .inner
            .waker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(waker);
    }

    /// A Window piece lost its daemon connection: starts reconnecting when a
    /// handoff is preparing or fenced (the exec may come before a pane reads
    /// `Fenced`), or joins the reconnection already running. Returns whether
    /// the Window is reconnecting; otherwise the loss is an ordinary failure.
    #[must_use]
    pub fn begin_reconnect(&self) -> bool {
        let mut running = false;
        self.update(|state| state.update(|state| running = state.begin_reconnect()));
        running
    }

    /// The reconnection a coordinator should run now, by epoch, if any. An
    /// epoch already answered, whose resnapshot the frontend has not applied
    /// yet, needs no second run.
    #[must_use]
    pub fn reconnect_pending(&self) -> Option<u64> {
        let state = self.lock();
        (state.reconnect == Reconnect::Pending).then_some(state.reconnect_epoch)
    }

    /// Every connection was replaced by one to the daemon serving
    /// `daemon_generation` and every pane resnapshotted for the reconnection
    /// `epoch` (from [`Self::reconnect_pending`]), so the pause held across
    /// the lost connection ends. Returns `false`, still reconnecting, when
    /// another connection was lost after that epoch began.
    #[allow(
        clippy::must_use_candidate,
        reason = "a coordinator that loses the race sees the newer epoch pending"
    )]
    pub fn reconnected(&self, daemon_generation: u64, epoch: u64) -> bool {
        let mut ended = false;
        self.update(|state| {
            let revision = state.revision;
            ended = state.reconnected(daemon_generation, epoch);
            state.revision != revision
        });
        ended
    }

    /// Reconnection failed: the Window stays paused with its last screen and
    /// says its connection to splinterd is lost.
    pub fn reconnect_failed(&self) {
        self.update(HandoffState::reconnect_failed);
    }

    /// The forward daemon failed and the old generation resumed: ends the
    /// pause and shows "Rolled back" briefly. A Window relaunched across the
    /// rollback calls it once it finds the old generation serving (M3.15).
    pub fn rolled_back(&self) {
        self.update(HandoffState::rolled_back);
    }

    /// This Window takes part in a handoff (M3.14): it holds
    /// [`WindowHandoffPhase::Continuing`] until the returned participation
    /// ends, and its ordinary reconnection waits meanwhile. `None` when the
    /// Window is lost or already takes part.
    #[must_use]
    pub fn begin_continuation(&self) -> Option<ContinuationParticipation> {
        let mut began = false;
        self.update(|state| {
            let revision = state.revision;
            began = state.begin_continuation();
            state.revision != revision
        });
        began.then(|| ContinuationParticipation {
            hub: self.clone(),
            ended: false,
        })
    }

    /// Whether the Window takes part in a handoff and waits for its outcome.
    #[must_use]
    pub fn continuing(&self) -> bool {
        self.lock().continuation.is_some()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HandoffState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn update(&self, change: impl FnOnce(&mut HandoffState) -> bool) -> bool {
        let changed = change(&mut self.lock());
        if changed
            && let Some(waker) = self
                .inner
                .waker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
        {
            waker.wake_by_ref();
        }
        changed
    }
}

/// One pane subscription's report channel into its Window's hub. Dropping it
/// withdraws the pane's report; [`Self::connection_lost`] instead starts
/// reconnection when a handoff is preparing or fenced.
#[derive(Debug)]
pub struct HandoffStatusSource {
    hub: HandoffStatusHub,
    id: u64,
    finished: bool,
}

impl HandoffStatusSource {
    /// Reports one `HandoffStatus` event; returns whether the Window's view
    /// changed. Duplicates and stale generations change nothing.
    #[allow(
        clippy::must_use_candidate,
        reason = "a controller reports and moves on; the result serves tests and diagnostics"
    )]
    pub fn observe(&self, daemon_generation: u64, phase: HandoffStatusPhase) -> bool {
        let id = self.id;
        self.hub
            .update(|state| state.observe(id, daemon_generation, phase))
    }

    /// Whether the Window is paused, whichever pane heard it first.
    #[must_use]
    pub fn input_paused(&self) -> bool {
        self.hub.input_paused()
    }

    /// The pane's connection failed rather than closing normally.
    pub fn connection_lost(mut self) {
        self.finished = true;
        let id = self.id;
        self.hub.update(|state| state.source_lost(id));
    }
}

impl Drop for HandoffStatusSource {
    fn drop(&mut self) {
        if !self.finished {
            let id = self.id;
            self.hub.update(|state| state.remove_source(id));
        }
    }
}

/// A Window's participation in one handoff (M3.14). Ending it, or dropping
/// it, returns the Window to the ordinary path: when a connection was lost
/// meanwhile the Window starts reconnecting; otherwise the attempt's own
/// status (normally `Cancelled`) decides the pause. A Window that is
/// relaunched never ends it: its process image is replaced.
#[derive(Debug)]
pub struct ContinuationParticipation {
    hub: HandoffStatusHub,
    ended: bool,
}

impl ContinuationParticipation {
    /// Ends the participation without a relaunch.
    pub fn end(mut self) {
        self.finish();
    }

    fn finish(&mut self) {
        if !std::mem::replace(&mut self.ended, true) {
            self.hub.update(HandoffState::end_continuation);
        }
    }
}

impl Drop for ContinuationParticipation {
    fn drop(&mut self) {
        self.finish();
    }
}

/// What a hub view changed in the frontend.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HandoffTransition {
    /// The banner, title, or pause changed: redraw.
    pub changed: bool,
    /// Input was paused or resumed.
    pub pause_changed: bool,
}

/// The frontend's copy of the hub view, with the outcome notice's timer.
#[derive(Debug, Default)]
pub struct HandoffPresentation {
    revision: u64,
    phase: WindowHandoffPhase,
    seen_notice: u64,
    notice: Option<(HandoffNotice, Instant)>,
}

impl HandoffPresentation {
    /// Folds a hub view in at `now`.
    pub fn apply(&mut self, view: HandoffStatusView, now: Instant) -> HandoffTransition {
        if view.revision == self.revision {
            return HandoffTransition::default();
        }
        self.revision = view.revision;
        let was_paused = self.phase.input_paused();
        let mut changed = view.phase != self.phase;
        self.phase = view.phase;
        if let Some((serial, notice)) = view.notice
            && serial != self.seen_notice
        {
            self.seen_notice = serial;
            self.notice = Some((notice, now + HANDOFF_NOTICE_DURATION));
            changed = true;
        }
        // A new attempt supersedes the previous one's outcome.
        if self.phase.input_paused() && self.notice.take().is_some() {
            changed = true;
        }
        HandoffTransition {
            changed,
            pause_changed: was_paused != self.phase.input_paused(),
        }
    }

    /// Drops an outcome notice whose time is up; returns whether it did.
    pub fn expire(&mut self, now: Instant) -> bool {
        if self.notice.is_some_and(|(_, until)| now >= until) {
            self.notice = None;
            return true;
        }
        false
    }

    #[must_use]
    pub const fn phase(&self) -> WindowHandoffPhase {
        self.phase
    }

    #[must_use]
    pub const fn input_paused(&self) -> bool {
        self.phase.input_paused()
    }

    #[must_use]
    pub fn banner(&self) -> Option<HandoffBanner> {
        match self.phase {
            WindowHandoffPhase::Preparing => Some(HandoffBanner::Preparing),
            WindowHandoffPhase::Fenced | WindowHandoffPhase::Continuing => {
                Some(HandoffBanner::InputPaused)
            }
            WindowHandoffPhase::Reconnecting => Some(HandoffBanner::Reconnecting),
            WindowHandoffPhase::Lost => Some(HandoffBanner::Lost),
            WindowHandoffPhase::Idle => {
                self.notice.map(|(notice, _)| HandoffBanner::Notice(notice))
            }
        }
    }

    /// When the outcome notice expires, for the event loop's timeout.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.notice.map(|(_, until)| until)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::task::Wake;

    use super::*;

    const GENERATION: u64 = 4;

    fn cancelled(reason: HandoffCancelReason) -> HandoffStatusPhase {
        HandoffStatusPhase::Cancelled { reason }
    }

    #[test]
    fn one_pane_walks_preparing_fenced_cancelled() {
        let hub = HandoffStatusHub::default();
        let pane = hub.source();
        assert_eq!(hub.view().phase, WindowHandoffPhase::Idle);
        assert!(pane.observe(GENERATION, HandoffStatusPhase::Preparing));
        assert_eq!(hub.view().phase, WindowHandoffPhase::Preparing);
        assert!(hub.input_paused(), "input pauses from Preparing on");
        assert!(pane.observe(GENERATION, HandoffStatusPhase::Fenced));
        assert_eq!(hub.view().phase, WindowHandoffPhase::Fenced);
        assert!(pane.observe(GENERATION, cancelled(HandoffCancelReason::TimedOut)));
        let view = hub.view();
        assert_eq!(view.phase, WindowHandoffPhase::Idle);
        assert!(!hub.input_paused(), "Cancelled resumes input");
        assert!(matches!(
            view.notice,
            Some((_, HandoffNotice::Cancelled(HandoffCancelReason::TimedOut)))
        ));
    }

    #[test]
    fn duplicates_change_nothing_and_keep_the_revision() {
        let hub = HandoffStatusHub::default();
        let first = hub.source();
        let second = hub.source();
        assert!(first.observe(GENERATION, HandoffStatusPhase::Preparing));
        let revision = hub.view().revision;
        assert!(!first.observe(GENERATION, HandoffStatusPhase::Preparing));
        // Another pane hearing the same phase leaves the Window unchanged.
        assert!(!second.observe(GENERATION, HandoffStatusPhase::Preparing));
        assert_eq!(hub.view().revision, revision);
        assert!(first.observe(GENERATION, HandoffStatusPhase::Fenced));
        assert!(!second.observe(GENERATION, HandoffStatusPhase::Fenced));
        assert_eq!(hub.view().phase, WindowHandoffPhase::Fenced);
    }

    #[test]
    fn the_first_pane_pauses_and_the_last_pane_resumes() {
        let hub = HandoffStatusHub::default();
        let fast = hub.source();
        let slow = hub.source();
        assert!(fast.observe(GENERATION, HandoffStatusPhase::Preparing));
        assert!(
            slow.input_paused(),
            "a pane that heard nothing is paused too"
        );
        assert!(fast.observe(GENERATION, HandoffStatusPhase::Fenced));
        assert!(!slow.observe(GENERATION, HandoffStatusPhase::Preparing));
        // The fast pane's Cancelled arrives while the slow pane still
        // reports the attempt: the Window stays paused, and the banner does
        // not step back to Preparing.
        assert!(!fast.observe(GENERATION, cancelled(HandoffCancelReason::Requested)));
        assert_eq!(hub.view().phase, WindowHandoffPhase::Fenced);
        assert!(hub.view().notice.is_none());
        assert!(!slow.observe(GENERATION, HandoffStatusPhase::Fenced));
        assert!(slow.observe(GENERATION, cancelled(HandoffCancelReason::Requested)));
        let view = hub.view();
        assert_eq!(view.phase, WindowHandoffPhase::Idle);
        assert!(matches!(
            view.notice,
            Some((_, HandoffNotice::Cancelled(HandoffCancelReason::Requested)))
        ));
    }

    #[test]
    fn a_next_attempt_heard_before_a_slow_pane_ends_the_last_is_preparing() {
        let hub = HandoffStatusHub::default();
        let fast = hub.source();
        let slow = hub.source();
        assert!(fast.observe(GENERATION, HandoffStatusPhase::Fenced));
        assert!(!slow.observe(GENERATION, HandoffStatusPhase::Fenced));
        assert!(!fast.observe(GENERATION, cancelled(HandoffCancelReason::Failed)));
        assert_eq!(hub.view().phase, WindowHandoffPhase::Fenced);
        // The fast pane hears the next attempt while the slow pane has not
        // yet ended the last one: the banner says Preparing, still paused.
        assert!(fast.observe(GENERATION, HandoffStatusPhase::Preparing));
        assert_eq!(hub.view().phase, WindowHandoffPhase::Preparing);
        assert!(!slow.observe(GENERATION, cancelled(HandoffCancelReason::Failed)));
        assert_eq!(hub.view().phase, WindowHandoffPhase::Preparing);
        assert!(!slow.observe(GENERATION, HandoffStatusPhase::Preparing));
        assert!(fast.observe(GENERATION, HandoffStatusPhase::Fenced));
        assert_eq!(hub.view().phase, WindowHandoffPhase::Fenced);
    }

    #[test]
    fn out_of_order_and_unreported_cancels_change_nothing() {
        let hub = HandoffStatusHub::default();
        let pane = hub.source();
        // A Cancelled for an attempt this Window never heard of, as a lagged
        // subscription replays it, neither pauses nor shows a notice.
        assert!(!pane.observe(GENERATION, cancelled(HandoffCancelReason::Failed)));
        assert_eq!(hub.view(), HandoffStatusView::default());
        // A Fenced without Preparing still pauses.
        assert!(pane.observe(GENERATION, HandoffStatusPhase::Fenced));
        assert_eq!(hub.view().phase, WindowHandoffPhase::Fenced);
        // A next attempt's Preparing after a lost Cancelled replaces the
        // stale Fenced rather than adding to it.
        assert!(pane.observe(GENERATION, HandoffStatusPhase::Preparing));
        assert_eq!(hub.view().phase, WindowHandoffPhase::Preparing);
    }

    #[test]
    fn generations_filter_stale_reports_and_reset_on_a_newer_daemon() {
        let hub = HandoffStatusHub::default();
        let pane = hub.source();
        assert!(
            !pane.observe(0, HandoffStatusPhase::Fenced),
            "generation zero"
        );
        assert!(pane.observe(GENERATION, HandoffStatusPhase::Fenced));
        // An older generation's report is stale.
        assert!(!pane.observe(GENERATION - 1, cancelled(HandoffCancelReason::Failed)));
        assert_eq!(hub.view().phase, WindowHandoffPhase::Fenced);
        // A newer daemon voids the older generation's pause.
        let other = hub.source();
        assert!(other.observe(GENERATION + 1, HandoffStatusPhase::Preparing));
        assert_eq!(hub.view().phase, WindowHandoffPhase::Preparing);
        assert!(other.observe(GENERATION + 1, cancelled(HandoffCancelReason::Blocked)));
        assert_eq!(hub.view().phase, WindowHandoffPhase::Idle);
        // The newer generation sticks.
        assert!(!pane.observe(GENERATION, HandoffStatusPhase::Fenced));
        assert_eq!(hub.view().phase, WindowHandoffPhase::Idle);
    }

    #[test]
    fn a_closed_pane_withdraws_its_report() {
        let hub = HandoffStatusHub::default();
        let pane = hub.source();
        assert!(pane.observe(GENERATION, HandoffStatusPhase::Preparing));
        drop(pane);
        assert_eq!(hub.view().phase, WindowHandoffPhase::Idle);
        assert!(hub.view().notice.is_none(), "closing a pane is no cancel");
    }

    #[test]
    fn a_connection_lost_while_fenced_reconnects_until_reconnected() {
        let hub = HandoffStatusHub::default();
        let first = hub.source();
        let second = hub.source();
        assert!(first.observe(GENERATION, HandoffStatusPhase::Fenced));
        assert!(!second.observe(GENERATION, HandoffStatusPhase::Fenced));
        assert_eq!(hub.reconnect_pending(), None);
        first.connection_lost();
        assert_eq!(hub.view().phase, WindowHandoffPhase::Reconnecting);
        assert!(
            hub.reconnect_pending().is_some(),
            "a coordinator reconnects"
        );
        second.connection_lost();
        assert_eq!(
            hub.view().phase,
            WindowHandoffPhase::Reconnecting,
            "the committed handoff's exec closes every connection"
        );
        assert!(hub.input_paused());
        assert!(hub.reconnected(GENERATION + 1, hub.reconnect_pending().unwrap()));
        assert_eq!(hub.view().phase, WindowHandoffPhase::Idle);
        assert!(!hub.input_paused());
        assert_eq!(hub.reconnect_pending(), None);
        assert!(hub.view().notice.is_none(), "a reconnection is no cancel");
    }

    #[test]
    fn the_exec_race_a_loss_after_only_preparing_reconnects() {
        // `Fenced` is queued per subscription, so the daemon can exec before
        // a pane reads it: the pane sees `Preparing` and then the loss.
        let hub = HandoffStatusHub::default();
        let pane = hub.source();
        assert!(pane.observe(GENERATION, HandoffStatusPhase::Preparing));
        pane.connection_lost();
        assert_eq!(hub.view().phase, WindowHandoffPhase::Reconnecting);
        assert!(hub.input_paused());
        assert!(hub.reconnect_pending().is_some());
    }

    #[test]
    fn a_loss_with_no_handoff_heard_is_an_ordinary_failure() {
        let hub = HandoffStatusHub::default();
        hub.source().connection_lost();
        assert_eq!(hub.view().phase, WindowHandoffPhase::Idle);
        assert!(!hub.begin_reconnect());
        assert_eq!(hub.reconnect_pending(), None);
    }

    #[test]
    fn a_coordinator_loss_during_preparing_starts_reconnection_once() {
        let hub = HandoffStatusHub::default();
        let pane = hub.source();
        pane.observe(GENERATION, HandoffStatusPhase::Preparing);
        assert!(hub.begin_reconnect());
        let epoch = hub.reconnect_pending().unwrap();
        // Later losses join the reconnection already running, under a newer
        // epoch: a resnapshot that began before them is not the answer.
        assert!(hub.begin_reconnect());
        pane.connection_lost();
        let newer = hub.reconnect_pending().unwrap();
        assert_ne!(newer, epoch);
        assert!(!hub.reconnected(GENERATION, epoch), "a stale epoch");
        assert_eq!(hub.view().phase, WindowHandoffPhase::Reconnecting);
        assert!(hub.reconnected(GENERATION, newer));
        assert_eq!(hub.view().phase, WindowHandoffPhase::Idle);
        // The next attempt's loss is a new reconnection.
        let pane = hub.source();
        pane.observe(GENERATION, HandoffStatusPhase::Fenced);
        assert!(hub.begin_reconnect());
        assert_ne!(hub.reconnect_pending(), Some(newer));
    }

    #[test]
    fn reconnecting_at_the_same_generation_keeps_a_live_attempt_paused() {
        let hub = HandoffStatusHub::default();
        let lost = hub.source();
        let live = hub.source();
        lost.observe(GENERATION, HandoffStatusPhase::Preparing);
        live.observe(GENERATION, HandoffStatusPhase::Preparing);
        lost.connection_lost();
        assert_eq!(hub.view().phase, WindowHandoffPhase::Reconnecting);
        // The same daemon answers and the attempt is still preparing: the
        // live pane's report stands.
        assert!(hub.reconnected(GENERATION, hub.reconnect_pending().unwrap()));
        assert_eq!(hub.view().phase, WindowHandoffPhase::Preparing);
        // At the same generation with no attempt left, it simply resumes.
        assert!(live.observe(GENERATION, cancelled(HandoffCancelReason::Failed)));
        assert_eq!(hub.view().phase, WindowHandoffPhase::Idle);
    }

    #[test]
    fn a_newer_generation_after_reconnection_voids_the_old_attempt() {
        let hub = HandoffStatusHub::default();
        let retired = hub.source();
        let lost = hub.source();
        retired.observe(GENERATION, HandoffStatusPhase::Fenced);
        lost.observe(GENERATION, HandoffStatusPhase::Fenced);
        lost.connection_lost();
        assert!(hub.reconnected(GENERATION + 1, hub.reconnect_pending().unwrap()));
        assert_eq!(hub.view().phase, WindowHandoffPhase::Idle);
        // A pane not yet retired cannot report the old daemon's attempt.
        assert!(!retired.observe(GENERATION, HandoffStatusPhase::Fenced));
        assert_eq!(hub.view().phase, WindowHandoffPhase::Idle);
    }

    #[test]
    fn a_cancel_while_reconnecting_waits_for_the_resnapshot() {
        let hub = HandoffStatusHub::default();
        let lost = hub.source();
        let live = hub.source();
        assert!(lost.observe(GENERATION, HandoffStatusPhase::Fenced));
        assert!(!live.observe(GENERATION, HandoffStatusPhase::Fenced));
        lost.connection_lost();
        // The daemon kept serving, but the lost pane has no stream until the
        // coordinator replaces it, so the Window stays paused.
        live.observe(GENERATION, cancelled(HandoffCancelReason::Failed));
        assert_eq!(hub.view().phase, WindowHandoffPhase::Reconnecting);
        assert!(hub.input_paused());
        assert!(hub.reconnected(GENERATION, hub.reconnect_pending().unwrap()));
        assert_eq!(hub.view().phase, WindowHandoffPhase::Idle);
    }

    #[test]
    fn a_failed_reconnection_stays_lost_and_paused() {
        let hub = HandoffStatusHub::default();
        let pane = hub.source();
        pane.observe(GENERATION, HandoffStatusPhase::Fenced);
        pane.connection_lost();
        hub.reconnect_failed();
        assert_eq!(hub.view().phase, WindowHandoffPhase::Lost);
        assert!(hub.input_paused());
        assert_eq!(hub.reconnect_pending(), None);
        assert!(!hub.begin_reconnect(), "nothing reconnects a lost Window");
        // No report can revive it: nothing current is connected.
        let late = hub.source();
        assert!(!late.observe(GENERATION + 1, cancelled(HandoffCancelReason::Failed)));
        late.connection_lost();
        assert_eq!(hub.view().phase, WindowHandoffPhase::Lost);
        assert!(hub.input_paused());
    }

    #[test]
    fn rolled_back_ends_the_pause_with_a_notice() {
        let hub = HandoffStatusHub::default();
        let pane = hub.source();
        assert!(pane.observe(GENERATION, HandoffStatusPhase::Fenced));
        pane.connection_lost();
        hub.rolled_back();
        let view = hub.view();
        assert_eq!(view.phase, WindowHandoffPhase::Idle);
        assert!(matches!(view.notice, Some((_, HandoffNotice::RolledBack))));
    }

    /// M3.14: a participating Window holds through the exec without running
    /// its ordinary reconnection; ending the participation after a loss
    /// starts it.
    #[test]
    fn a_participating_window_defers_reconnection_until_it_ends() {
        let hub = HandoffStatusHub::default();
        let pane = hub.source();
        assert!(pane.observe(GENERATION, HandoffStatusPhase::Fenced));
        let participation = hub.begin_continuation().unwrap();
        assert!(
            hub.begin_continuation().is_none(),
            "one participation at a time"
        );
        assert_eq!(hub.view().phase, WindowHandoffPhase::Continuing);
        assert!(hub.input_paused());
        assert!(WindowHandoffPhase::Continuing.disconnected());
        assert!(hub.continuing());

        pane.connection_lost();
        assert!(hub.begin_reconnect(), "the Window holds");
        assert_eq!(hub.view().phase, WindowHandoffPhase::Continuing);
        assert_eq!(hub.reconnect_pending(), None, "no coordinator reconnects");

        participation.end();
        assert!(!hub.continuing());
        assert_eq!(hub.view().phase, WindowHandoffPhase::Reconnecting);
        let epoch = hub.reconnect_pending().unwrap();
        assert!(hub.reconnected(GENERATION, epoch));
        assert_eq!(hub.view().phase, WindowHandoffPhase::Idle);
    }

    #[test]
    fn a_participation_without_a_loss_returns_to_the_attempt() {
        let hub = HandoffStatusHub::default();
        let pane = hub.source();
        assert!(pane.observe(GENERATION, HandoffStatusPhase::Fenced));
        drop(hub.begin_continuation().unwrap());
        assert_eq!(hub.view().phase, WindowHandoffPhase::Fenced);
        assert_eq!(hub.reconnect_pending(), None);
        assert!(pane.observe(GENERATION, cancelled(HandoffCancelReason::Requested)));
        assert_eq!(hub.view().phase, WindowHandoffPhase::Idle);

        // The banner during participation is the input-paused one.
        let mut presentation = HandoffPresentation::default();
        let participation = hub.begin_continuation().unwrap();
        presentation.apply(hub.view(), Instant::now());
        assert_eq!(presentation.banner(), Some(HandoffBanner::InputPaused));
        participation.end();
    }

    #[test]
    fn a_lost_or_rolled_back_window_ends_or_refuses_participation() {
        let hub = HandoffStatusHub::default();
        let participation = hub.begin_continuation().unwrap();
        hub.rolled_back();
        assert!(!hub.continuing());
        assert_eq!(hub.view().phase, WindowHandoffPhase::Idle);
        participation.end();
        assert_eq!(hub.view().phase, WindowHandoffPhase::Idle);

        let pane = hub.source();
        assert!(pane.observe(GENERATION, HandoffStatusPhase::Fenced));
        pane.connection_lost();
        hub.reconnect_failed();
        assert!(hub.begin_continuation().is_none());
        assert_eq!(hub.view().phase, WindowHandoffPhase::Lost);
    }

    struct CountingWake(AtomicUsize);

    impl Wake for CountingWake {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn the_frontend_is_woken_only_for_changes() {
        let hub = HandoffStatusHub::default();
        let wakes = Arc::new(CountingWake(AtomicUsize::new(0)));
        hub.set_waker(Waker::from(Arc::clone(&wakes)));
        let pane = hub.source();
        pane.observe(GENERATION, HandoffStatusPhase::Preparing);
        pane.observe(GENERATION, HandoffStatusPhase::Preparing);
        pane.observe(GENERATION, HandoffStatusPhase::Fenced);
        assert_eq!(wakes.0.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn banner_text_per_state() {
        assert_eq!(HandoffBanner::Preparing.text(), "Preparing upgrade…");
        // Beside the spinner a running upgrade drops its `…`.
        assert_eq!(HandoffBanner::Preparing.label(), "Preparing upgrade");
        assert_eq!(HandoffBanner::Reconnecting.label(), "Reconnecting");
        assert!(HandoffBanner::InputPaused.is_working());
        assert!(!HandoffBanner::Lost.is_working());
        assert_eq!(
            HandoffBanner::Lost.label(),
            "Lost connection to splinterd — reopen from the launcher"
        );
        assert!(!HandoffBanner::Notice(HandoffNotice::RolledBack).is_working());
        assert_eq!(
            HandoffBanner::InputPaused.text(),
            "Upgrading — input paused"
        );
        assert_eq!(
            HandoffBanner::Notice(HandoffNotice::Cancelled(HandoffCancelReason::Requested)).text(),
            "Upgrade cancelled — stopped on request"
        );
        assert_eq!(
            HandoffBanner::Notice(HandoffNotice::Cancelled(HandoffCancelReason::Blocked)).text(),
            "Upgrade cancelled — something blocked it"
        );
        assert_eq!(
            HandoffBanner::Notice(HandoffNotice::Cancelled(HandoffCancelReason::Failed)).text(),
            "Upgrade cancelled — preparation failed, nothing changed"
        );
        assert_eq!(
            HandoffBanner::Notice(HandoffNotice::Cancelled(HandoffCancelReason::TimedOut)).text(),
            "Upgrade cancelled — a step took too long"
        );
        assert_eq!(
            HandoffBanner::Notice(HandoffNotice::RolledBack).text(),
            "Rolled back"
        );
        assert_eq!(HandoffBanner::Reconnecting.text(), "Reconnecting…");
        assert_eq!(
            HandoffBanner::Lost.text(),
            "Lost connection to splinterd — reopen from the launcher"
        );
    }

    #[test]
    fn presentation_shows_each_phase_then_a_timed_cancel_notice() {
        let hub = HandoffStatusHub::default();
        let pane = hub.source();
        let mut presentation = HandoffPresentation::default();
        let start = Instant::now();
        assert_eq!(
            presentation.apply(hub.view(), start),
            HandoffTransition::default()
        );
        assert_eq!(presentation.banner(), None);
        pane.observe(GENERATION, HandoffStatusPhase::Preparing);
        assert_eq!(
            presentation.apply(hub.view(), start),
            HandoffTransition {
                changed: true,
                pause_changed: true,
            }
        );
        assert_eq!(presentation.banner(), Some(HandoffBanner::Preparing));
        pane.observe(GENERATION, HandoffStatusPhase::Fenced);
        assert_eq!(
            presentation.apply(hub.view(), start),
            HandoffTransition {
                changed: true,
                pause_changed: false,
            }
        );
        assert_eq!(presentation.banner(), Some(HandoffBanner::InputPaused));
        pane.observe(GENERATION, cancelled(HandoffCancelReason::Blocked));
        let cancelled_at = start + Duration::from_secs(1);
        assert_eq!(
            presentation.apply(hub.view(), cancelled_at),
            HandoffTransition {
                changed: true,
                pause_changed: true,
            }
        );
        assert!(!presentation.input_paused());
        assert_eq!(
            presentation.banner(),
            Some(HandoffBanner::Notice(HandoffNotice::Cancelled(
                HandoffCancelReason::Blocked
            )))
        );
        assert_eq!(
            presentation.deadline(),
            Some(cancelled_at + HANDOFF_NOTICE_DURATION)
        );
        assert!(!presentation.expire(cancelled_at + Duration::from_millis(2_999)));
        assert!(presentation.expire(cancelled_at + HANDOFF_NOTICE_DURATION));
        assert_eq!(presentation.banner(), None);
        assert_eq!(presentation.deadline(), None);
    }

    #[test]
    fn presentation_shows_reconnecting_then_lost_after_a_lost_connection() {
        let hub = HandoffStatusHub::default();
        let pane = hub.source();
        let mut presentation = HandoffPresentation::default();
        pane.observe(GENERATION, HandoffStatusPhase::Preparing);
        presentation.apply(hub.view(), Instant::now());
        pane.connection_lost();
        assert_eq!(
            presentation.apply(hub.view(), Instant::now()),
            HandoffTransition {
                changed: true,
                pause_changed: false,
            }
        );
        assert_eq!(presentation.phase(), WindowHandoffPhase::Reconnecting);
        assert_eq!(presentation.banner(), Some(HandoffBanner::Reconnecting));
        hub.reconnect_failed();
        assert_eq!(
            presentation.apply(hub.view(), Instant::now()),
            HandoffTransition {
                changed: true,
                pause_changed: false,
            }
        );
        assert!(presentation.input_paused());
        assert_eq!(presentation.banner(), Some(HandoffBanner::Lost));
        assert_eq!(presentation.deadline(), None, "the lost message stays");
    }

    #[test]
    fn a_new_attempt_replaces_the_previous_outcome_notice() {
        let hub = HandoffStatusHub::default();
        let pane = hub.source();
        let mut presentation = HandoffPresentation::default();
        let now = Instant::now();
        pane.observe(GENERATION, HandoffStatusPhase::Preparing);
        pane.observe(GENERATION, cancelled(HandoffCancelReason::Failed));
        presentation.apply(hub.view(), now);
        assert!(matches!(
            presentation.banner(),
            Some(HandoffBanner::Notice(_))
        ));
        pane.observe(GENERATION, HandoffStatusPhase::Preparing);
        presentation.apply(hub.view(), now);
        assert_eq!(presentation.banner(), Some(HandoffBanner::Preparing));
        assert_eq!(presentation.deadline(), None);
    }
}
