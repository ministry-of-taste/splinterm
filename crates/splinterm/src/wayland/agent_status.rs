//! Window-local agent status for Dojo tab badges and the palette clear.
//!
//! Status is display data from the daemon, never authority and never inferred
//! from terminal content. Tabs show one badge per Dojo and never the
//! free-text detail. A fresh working status shows a Braille spinner here and
//! in the Explorer. It steps at most four times a second on the event loop's
//! existing cursor-blink wakeup, only while the Window has keyboard focus and
//! `SPLINTERM_REDUCED_MOTION` is unset, and otherwise holds its first frame.

use std::time::{Duration, Instant};

use anyhow::Result;

use super::{App, DojoId, QueueHandle, SplintId};
use crate::config::{AgentStatusStyle, StatusGlyphs};
use crate::frontend::{AgentAggregate, AgentGlyphs, AgentSpinner, LairExplorerUi};
use crate::navigation_projection::WindowAgentStatus;
use crate::renderer::{ChromeSource, chrome_icons_available, chrome_primary_faces_cover};

/// The status glyphs `[explorer] status-glyphs` asks for, as far as the
/// fonts draw them. Dots the interface font cannot draw itself fall back to
/// ASCII, reported once.
fn select_glyphs(style: StatusGlyphs) -> AgentGlyphs {
    let dots_available =
        style == StatusGlyphs::Dots && chrome_primary_faces_cover(AgentGlyphs::DOTS);
    let glyphs = AgentGlyphs::select(chrome_icons_available(), dots_available, style);
    if style == StatusGlyphs::Dots && glyphs != AgentGlyphs::Dots {
        static REPORTED: std::sync::Once = std::sync::Once::new();
        REPORTED.call_once(|| {
            eprintln!(
                "splinterm: the interface font does not draw {}; status-glyphs=dots uses ASCII status glyphs",
                AgentGlyphs::DOTS
            );
        });
    }
    glyphs
}

/// Applies a changed style to the tab badges and the Explorer, the glyphs
/// chosen by `select`; returns whether the chrome must be redrawn.
fn apply_agent_status_style(
    agent: &mut TabAgentStatus,
    explorer: &mut LairExplorerUi,
    style: AgentStatusStyle,
    select: impl FnOnce(StatusGlyphs) -> AgentGlyphs,
) -> bool {
    if agent.style == style {
        return false;
    }
    agent.style = style;
    agent.glyphs = select(style.glyphs);
    explorer.set_agent_glyphs(agent.glyphs);
    true
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "presentation, spinner motion, the last draw, and pending damage are independent"
)]
pub(super) struct TabAgentStatus {
    status: WindowAgentStatus,
    enabled: bool,
    glyphs: AgentGlyphs,
    /// `[explorer] status-colors` and `status-glyphs`.
    style: AgentStatusStyle,
    /// The working spinner's frame, shared with the Explorer.
    spinner: AgentSpinner,
    last_spinner_step: Instant,
    /// Whether the spinner moved at the last event-loop pass.
    animating: bool,
    /// Whether the last drawn Explorer had a row in view with a fresh
    /// working status; recorded by each draw, `false` while it is hidden.
    pub(super) explorer_working: bool,
    /// A frame changed since the last draw: that draw must damage the tab
    /// strip and the Explorer, or the compositor keeps showing the old frame.
    chrome_damage: bool,
}

impl TabAgentStatus {
    pub(super) fn new(enabled: bool, style: AgentStatusStyle) -> Self {
        Self {
            status: WindowAgentStatus::default(),
            enabled,
            glyphs: select_glyphs(style.glyphs),
            style,
            spinner: AgentSpinner::STATIC,
            last_spinner_step: Instant::now(),
            animating: false,
            explorer_working: false,
            chrome_damage: false,
        }
    }

    /// One tab badge: the aggregate glyph of the Dojo's Splints.
    pub(super) fn badge(&self, dojo_id: DojoId) -> Option<ChromeSource> {
        if !self.enabled {
            return None;
        }
        AgentAggregate::from_statuses(self.status.dojo_statuses(dojo_id))
            .tab_badge(self.glyphs, self.spinner)
    }

    /// Whether a Dojo's tab badge shows a fresh working status. The badge is
    /// the most urgent state, so a working Splint beside one that needs input
    /// shows no spinner.
    fn shows_working(&self, dojo_id: DojoId) -> bool {
        self.enabled
            && AgentAggregate::from_statuses(self.status.dojo_statuses(dojo_id))
                .top()
                .is_some_and(|(state, is_stale, _)| {
                    state == splinterm_protocol::AgentState::Working && !is_stale
                })
    }

    /// One event-loop pass at `now`: decides once whether the spinner moves
    /// (`moves_here`: a focused Window without reduced motion; and a drawn
    /// badge or Explorer row shows fresh work), then steps it when a frame
    /// is due or returns it to its first frame once it stops. A changed frame
    /// is returned and asks the next draw to damage the chrome showing it.
    fn tick_spinner(
        &mut self,
        now: Instant,
        moves_here: bool,
        tabs_working: bool,
    ) -> Option<AgentSpinner> {
        self.animating = moves_here && (tabs_working || self.explorer_working);
        let next = self.spinner.tick(
            self.animating,
            now.saturating_duration_since(self.last_spinner_step),
        )?;
        self.spinner = next;
        self.last_spinner_step = now;
        self.chrome_damage = true;
        Some(next)
    }

    /// How long the event loop may sleep at `now` before the next frame;
    /// `None` while the spinner rests. Reuses the last pass's decision.
    fn spinner_timeout(&self, now: Instant) -> Option<Duration> {
        AgentSpinner::remaining(
            self.animating,
            now.saturating_duration_since(self.last_spinner_step),
        )
    }

    /// Whether this draw must damage the tab strip and the Explorer because
    /// a spinner frame changed; clears the request.
    pub(super) fn take_chrome_damage(&mut self) -> bool {
        std::mem::take(&mut self.chrome_damage)
    }

    /// Whether status glyphs, the "Asking" marker and the remote tag take
    /// their status colors (`[explorer] status-colors`).
    pub(super) const fn colors(&self) -> bool {
        self.style.colors
    }

    /// Icons, dots, or ASCII when icons are asked for but the embedded
    /// icon face is unavailable; shared with the Explorer.
    pub(super) const fn glyphs(&self) -> AgentGlyphs {
        self.glyphs
    }

    /// Live incarnation of a Splint that currently shows a status, which the
    /// palette's "Clear agent status" captures.
    pub(super) fn clearable_incarnation(&self, splint_id: SplintId) -> Option<u64> {
        self.enabled
            .then(|| self.status.splints.get(&splint_id))
            .flatten()
            .map(|splint| splint.incarnation)
    }
}

impl App {
    /// Applies `[explorer] status-colors` and `status-glyphs`; returns
    /// whether the chrome must be redrawn.
    pub(super) fn set_agent_status_style(&mut self, style: AgentStatusStyle) -> bool {
        apply_agent_status_style(
            &mut self.tab_state.agent,
            &mut self.explorer,
            style,
            select_glyphs,
        )
    }

    pub(super) fn set_agent_status_presentation(&mut self, enabled: bool) -> bool {
        let explorer_changed = self.explorer.set_agent_status_enabled(enabled);
        let tabs_changed = self.tab_state.agent.enabled != enabled;
        self.tab_state.agent.enabled = enabled;
        explorer_changed || tabs_changed
    }

    pub(super) fn apply_window_agent_status(&mut self, status: WindowAgentStatus) -> bool {
        if self.tab_state.agent.status == status {
            return false;
        }
        self.tab_state.agent.status = status;
        true
    }

    /// Applies an accepted clear to the matching live incarnation only; the
    /// next topology poll and Explorer refresh supersede it.
    pub(super) fn apply_agent_status_cleared(
        &mut self,
        splint_id: SplintId,
        incarnation: u64,
    ) -> bool {
        let tabs = self
            .tab_state
            .agent
            .status
            .apply_change(splint_id, incarnation, None);
        let explorer = self
            .explorer
            .apply_agent_status(splint_id, incarnation, None);
        tabs || explorer
    }

    /// Decides once per event-loop pass whether the spinner moves, steps it
    /// when its frame is due or returns it to its first frame once it stops,
    /// and redraws the chrome that shows it. Runs beside the cursor blink on
    /// the same event-loop wakeup. Cheap: it reads what the last draw
    /// recorded and never builds Explorer rows.
    pub(super) fn tick_agent_spinner(&mut self, queue_handle: &QueueHandle<Self>) -> Result<()> {
        // Focus and reduced motion first, so an unfocused Window never looks
        // at its tabs.
        let moves_here =
            AgentSpinner::animates(self.input.reduced_motion, self.input.keyboard_focused, true);
        let agent = &self.tab_state.agent;
        let tabs_working = moves_here
            && self
                .tab_state
                .tab_strip_layout
                .as_ref()
                .is_some_and(|layout| {
                    layout
                        .tabs
                        .iter()
                        .any(|tab| agent.shows_working(tab.dojo_id))
                });
        let Some(next) =
            self.tab_state
                .agent
                .tick_spinner(Instant::now(), moves_here, tabs_working)
        else {
            return Ok(());
        };
        self.explorer.set_agent_spinner(next);
        if self.surface.configured {
            self.schedule_draw(queue_handle)?;
        }
        Ok(())
    }

    /// How long the event loop may sleep before the spinner's next frame;
    /// `None` while it rests.
    pub(super) fn agent_spinner_timeout(&self) -> Option<Duration> {
        self.tab_state.agent.spinner_timeout(Instant::now())
    }
}

#[cfg(test)]
mod tests {
    use splinterm_protocol::AgentState;

    use super::*;
    use crate::navigation_projection::{NavigationAgentStatus, WindowSplintAgentStatus};

    fn status(dojo_id: DojoId, incarnation: u64, state: AgentState) -> WindowSplintAgentStatus {
        WindowSplintAgentStatus {
            dojo_id,
            incarnation,
            status: NavigationAgentStatus {
                state,
                detail: Some("never shown in tabs".to_owned()),
                stale: false,
                stale_minutes: None,
            },
        }
    }

    #[test]
    fn a_changed_status_style_switches_tab_and_explorer_glyphs() {
        let mut agent = TabAgentStatus::new(true, AgentStatusStyle::default());
        let mut explorer = LairExplorerUi::default();
        let dots = AgentStatusStyle {
            colors: false,
            glyphs: StatusGlyphs::Dots,
        };
        let select = |style: StatusGlyphs| {
            assert_eq!(style, StatusGlyphs::Dots);
            AgentGlyphs::Dots
        };
        assert!(apply_agent_status_style(
            &mut agent,
            &mut explorer,
            dots,
            select
        ));
        assert_eq!(agent.glyphs(), AgentGlyphs::Dots);
        assert!(!agent.colors());
        // The Explorer already draws dots.
        assert!(!explorer.set_agent_glyphs(AgentGlyphs::Dots));
        // Unchanged: nothing to redraw, and no glyph selection at all.
        assert!(!apply_agent_status_style(
            &mut agent,
            &mut explorer,
            dots,
            |_| unreachable!("an unchanged style selects no glyphs")
        ));
    }

    #[test]
    fn tab_badges_aggregate_per_dojo_and_never_carry_detail() {
        let (first, second) = (DojoId::new(), DojoId::new());
        let mut agent = TabAgentStatus::new(true, AgentStatusStyle::default());
        let blocked = SplintId::new();
        agent.status.splints = [
            (blocked, status(first, 3, AgentState::NeedsInput)),
            (SplintId::new(), status(first, 1, AgentState::Working)),
            (SplintId::new(), status(second, 2, AgentState::Done)),
        ]
        .into_iter()
        .collect();
        let badge = |agent: &TabAgentStatus, dojo| agent.badge(dojo).map(|badge| badge.describe());
        // The embedded icon face is always available in tests.
        assert_eq!(agent.glyphs(), AgentGlyphs::Icons);
        assert_eq!(badge(&agent, first).as_deref(), Some("[message-question]"));
        assert_eq!(badge(&agent, second).as_deref(), Some("[circle-check]"));
        assert_eq!(agent.badge(DojoId::new()), None);
        assert!(
            [first, second]
                .into_iter()
                .filter_map(|dojo| agent.badge(dojo))
                .all(|badge| !badge.text().contains("shown"))
        );
        assert_eq!(agent.clearable_incarnation(blocked), Some(3));
        assert_eq!(agent.clearable_incarnation(SplintId::new()), None);

        // Needs input outranks working, so the spinner is not shown yet.
        assert!(!agent.shows_working(first));
        assert!(!agent.shows_working(second));
        assert!(agent.status.apply_change(blocked, 3, None));
        assert_eq!(badge(&agent, first).as_deref(), Some("⠋"));
        assert!(agent.shows_working(first));
        assert!(!agent.shows_working(second));
        // The badge shows the spinner's current frame in both glyph styles.
        agent.spinner = AgentSpinner::STATIC.next();
        assert_eq!(badge(&agent, first).as_deref(), Some("⠙"));
        agent.glyphs = AgentGlyphs::Ascii;
        assert_eq!(badge(&agent, first).as_deref(), Some("⠙"));

        agent.enabled = false;
        assert_eq!(agent.badge(second), None);
        assert!(!agent.shows_working(first));
        assert_eq!(agent.clearable_incarnation(blocked), None);
    }

    #[test]
    fn clears_apply_only_to_the_matching_live_incarnation() {
        let dojo = DojoId::new();
        let splint = SplintId::new();
        let mut status_set = WindowAgentStatus::default();
        status_set
            .splints
            .insert(splint, status(dojo, 4, AgentState::Error));
        assert!(!status_set.apply_change(splint, 3, None));
        assert!(!status_set.apply_change(SplintId::new(), 4, None));
        assert!(status_set.splints.contains_key(&splint));
        assert!(status_set.apply_change(splint, 4, None));
        assert!(!status_set.apply_change(splint, 4, None));
    }

    /// VM check of 88d99d63: the spinner showed one frame and moved only when
    /// a status or topology update forced a full redraw. This drives the
    /// Window's spinner path the way the event loop does: each pass ticks,
    /// then sleeps until the returned deadline, with no status update at
    /// all. Every frame change asks for chrome damage, and the tab badge the
    /// strip caches by source changes with it.
    #[test]
    fn spinner_moves_on_its_own_over_simulated_time_and_damages_the_chrome() {
        let dojo = DojoId::new();
        let mut agent = TabAgentStatus::new(true, AgentStatusStyle::default());
        agent.status.splints = [(SplintId::new(), status(dojo, 1, AgentState::Working))]
            .into_iter()
            .collect();
        let start = agent.last_spinner_step;
        let mut now = start;
        let mut badges = vec![agent.badge(dojo).unwrap()];
        let mut passes = 0;
        while now < start + Duration::from_millis(1_100) {
            passes += 1;
            let tabs_working = agent.shows_working(dojo);
            if agent.tick_spinner(now, true, tabs_working).is_some() {
                assert!(agent.take_chrome_damage(), "a new frame must be damaged");
                assert!(!agent.take_chrome_damage(), "damage is requested once");
                badges.push(agent.badge(dojo).unwrap());
            } else {
                assert!(!agent.take_chrome_damage());
            }
            let sleep = agent
                .spinner_timeout(now)
                .expect("a moving spinner keeps the loop's deadline");
            assert!(sleep <= AgentSpinner::STEP);
            now += sleep.max(Duration::from_millis(1));
        }
        let frames = badges
            .iter()
            .map(|badge| badge.text().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(frames, ["⠋", "⠙", "⠹", "⠸", "⠼"], "after {passes} passes");
        // The Explorer alone keeps it moving, as recorded by the last draw.
        let mut explorer_only = TabAgentStatus::new(true, AgentStatusStyle::default());
        explorer_only.explorer_working = true;
        let start = explorer_only.last_spinner_step;
        assert!(explorer_only.tick_spinner(start, true, false).is_none());
        assert!(
            explorer_only
                .tick_spinner(start + AgentSpinner::STEP, true, false)
                .is_some()
        );
        assert!(explorer_only.take_chrome_damage());

        // Unfocused or reduced motion: back to the first frame once, damaged,
        // then at rest with no deadline.
        let later = now + AgentSpinner::STEP;
        assert_eq!(
            agent.tick_spinner(later, false, true),
            Some(AgentSpinner::STATIC)
        );
        assert!(agent.take_chrome_damage());
        assert_eq!(agent.badge(dojo).unwrap().text(), "⠋");
        assert_eq!(
            agent.tick_spinner(later + AgentSpinner::STEP, false, true),
            None
        );
        assert_eq!(agent.spinner_timeout(later), None);
        // Nothing working anywhere: no deadline either.
        let mut idle = TabAgentStatus::new(true, AgentStatusStyle::default());
        assert_eq!(idle.tick_spinner(later, true, false), None);
        assert_eq!(idle.spinner_timeout(later), None);
    }
}
