//! A Dojo an agent creates appears as a background tab (Plan 0067 M3).
//!
//! The daemon offers each such Dojo, in the topology snapshot, to the Windows
//! showing its Lair. On its poll, a Window whose active tab is in that Lair
//! claims at once; another Window showing the Lair waits
//! [`OTHER_WINDOW_CLAIM_DELAY`] so the one in use can take it first. The
//! daemon grants one claim, and that Window appends the tab without switching
//! to it: the active tab and keyboard focus stay where they were. With no
//! Window showing the Lair, no tab opens.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use splinterm::{
    WindowTopologyUpdate,
    automation::{Connection, SharedImageContentCache},
    config::AppConfig,
    endpoint::ConnectionFactory,
    tab::{DojoTab, WindowTabSet},
};
use splinterm_core::DojoId;
use splinterm_protocol::{DojoPresentationOffer, Request, Response, TopologySnapshot};
use tokio::sync::mpsc;

use super::{
    ManagedDojo, TopologyManagerState, cancel_pane_tasks, prepare_managed_dojo,
    window_dojo_identity, window_has_tab_capacity,
};

/// How long a Window that shows an offer's Lair, but not as its active tab,
/// leaves the offer to the Window in use.
pub(super) const OTHER_WINDOW_CLAIM_DELAY: Duration = Duration::from_millis(750);

/// When this Window first saw each offer it may claim.
#[derive(Default)]
pub(super) struct PresentationOfferTracker {
    first_seen: HashMap<DojoId, Instant>,
}

impl PresentationOfferTracker {
    /// The offers this Window claims now, in the snapshot's order: those for
    /// its active tab's Lair at once, those for another Lair it shows once
    /// they have waited [`OTHER_WINDOW_CLAIM_DELAY`]. An offer for a Lair it
    /// does not show, or for a Dojo it already has, is never due.
    pub(super) fn due<T>(
        &mut self,
        offers: &[DojoPresentationOffer],
        tabs: &WindowTabSet<T>,
        now: Instant,
    ) -> Vec<DojoPresentationOffer> {
        let active_lair = tabs.active().map(|tab| tab.lair_id);
        let mut tracked = HashMap::with_capacity(offers.len());
        let mut due = Vec::new();
        for offer in offers {
            if tabs.get(offer.dojo_id).is_some()
                || !tabs.iter().any(|tab| tab.lair_id == offer.lair_id)
            {
                continue;
            }
            let seen = self.first_seen.get(&offer.dojo_id).copied().unwrap_or(now);
            tracked.insert(offer.dojo_id, seen);
            if active_lair == Some(offer.lair_id)
                || now.saturating_duration_since(seen) >= OTHER_WINDOW_CLAIM_DELAY
            {
                due.push(*offer);
            }
        }
        self.first_seen = tracked;
        due
    }

    /// Forgets an offer this Window claimed, granted or not.
    pub(super) fn forget(&mut self, dojo_id: DojoId) {
        self.first_seen.remove(&dojo_id);
    }
}

/// Claims the offers that are due and opens each granted one as a
/// background tab. Nothing is sent when no offer is due, so an old daemon,
/// which offers nothing, never sees the claim. Returns `false` when the
/// frontend is gone.
#[allow(
    clippy::too_many_arguments,
    reason = "a claim opens a tab with the same inputs as every other managed tab open"
)]
pub(super) async fn claim_presentation_offers(
    factory: &ConnectionFactory,
    connection: &mut Connection,
    config: &AppConfig,
    image_cache: &SharedImageContentCache,
    updates: &mpsc::Sender<WindowTopologyUpdate>,
    state: &mut TopologyManagerState,
    snapshot: &TopologySnapshot,
    tracker: &mut PresentationOfferTracker,
) -> bool {
    let due = tracker.due(&snapshot.presentation_offers, &state.tabs, Instant::now());
    for offer in due {
        // A full Window leaves the offer to another Window.
        if !window_has_tab_capacity(state.tabs.len()) {
            break;
        }
        tracker.forget(offer.dojo_id);
        let Some((lair, dojo)) = offered_dojo(&snapshot.topology, offer) else {
            continue;
        };
        let granted = matches!(
            connection
                .request(Request::ClaimDojoPresentation {
                    dojo_id: offer.dojo_id,
                })
                .await,
            Ok(Response::DojoPresentationClaimed { granted: true })
        );
        if !granted {
            continue;
        }
        let identity = window_dojo_identity(snapshot.revision, lair, dojo);
        match open_background_tab(
            factory,
            config,
            image_cache,
            updates,
            state,
            identity,
            dojo.clone(),
        )
        .await
        {
            Ok(()) => {}
            Err(_) if updates.is_closed() => return false,
            // The Dojo is still in the picker and the Explorer.
            Err(_) => eprintln!("splinterm agent Dojo tab did not open"),
        }
    }
    true
}

fn offered_dojo(
    topology: &splinterm_core::Topology,
    offer: DojoPresentationOffer,
) -> Option<(&splinterm_core::Lair, &splinterm_core::Dojo)> {
    let lair = topology.find_lair(offer.lair_id)?;
    let dojo = lair.dojos.iter().find(|dojo| dojo.id == offer.dojo_id)?;
    Some((lair, dojo))
}

async fn open_background_tab(
    factory: &ConnectionFactory,
    config: &AppConfig,
    image_cache: &SharedImageContentCache,
    updates: &mpsc::Sender<WindowTopologyUpdate>,
    state: &mut TopologyManagerState,
    identity: splinterm::WindowDojoIdentity,
    dojo: splinterm_core::Dojo,
) -> Result<()> {
    let prepared = prepare_managed_dojo(factory, config, image_cache, identity, dojo, None).await?;
    let (acknowledged, acknowledgement) = tokio::sync::oneshot::channel();
    if updates
        .send(WindowTopologyUpdate::OpenTab {
            identity: prepared.identity.clone(),
            layout: prepared.dojo.root.clone(),
            panes: prepared.panes,
            focused: prepared.dojo.default_focus,
            acknowledged,
            explorer_target: None,
            activate: false,
        })
        .await
        .is_err()
    {
        cancel_pane_tasks(prepared.pane_tasks).await;
        bail!("Wayland tab update channel closed");
    }
    match acknowledgement.await {
        Ok(Ok(())) => {}
        Ok(Err(message)) => {
            cancel_pane_tasks(prepared.pane_tasks).await;
            bail!("Wayland rejected the background Dojo tab: {message}");
        }
        Err(_) => {
            cancel_pane_tasks(prepared.pane_tasks).await;
            bail!("Wayland dropped the background Dojo tab acknowledgement");
        }
    }
    let lair_id = prepared.identity.lair_id;
    let dojo_id = prepared.dojo.id;
    state.tabs.open_background(DojoTab::new(
        lair_id,
        dojo_id,
        ManagedDojo {
            identity: prepared.identity,
            root: prepared.dojo.root,
            pending_focus: None,
            pane_tasks: prepared.pane_tasks,
        },
    ))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use splinterm_core::LairId;

    use super::*;

    #[test]
    fn the_window_in_use_claims_at_once_and_another_after_the_delay() {
        let (shown, other, unshown) = (LairId::new(), LairId::new(), LairId::new());
        let (active_dojo, other_dojo) = (DojoId::new(), DojoId::new());
        let mut tabs = WindowTabSet::new(DojoTab::new(shown, active_dojo, ()));
        tabs.open_background(DojoTab::new(other, other_dojo, ()))
            .unwrap();
        let in_active = DojoPresentationOffer {
            lair_id: shown,
            dojo_id: DojoId::new(),
        };
        let in_other = DojoPresentationOffer {
            lair_id: other,
            dojo_id: DojoId::new(),
        };
        let not_shown = DojoPresentationOffer {
            lair_id: unshown,
            dojo_id: DojoId::new(),
        };
        let already_open = DojoPresentationOffer {
            lair_id: other,
            dojo_id: other_dojo,
        };
        let offers = [in_active, in_other, not_shown, already_open];
        let start = Instant::now();
        let mut tracker = PresentationOfferTracker::default();
        assert_eq!(tracker.due(&offers, &tabs, start), [in_active]);
        let almost = start + (OTHER_WINDOW_CLAIM_DELAY - Duration::from_millis(1));
        assert_eq!(tracker.due(&offers, &tabs, almost), [in_active]);
        let later = start + OTHER_WINDOW_CLAIM_DELAY;
        assert_eq!(tracker.due(&offers, &tabs, later), [in_active, in_other]);

        // An offer that leaves the snapshot is forgotten, so if it came back
        // it would wait again.
        assert!(tracker.due(&[], &tabs, later).is_empty());
        assert!(tracker.first_seen.is_empty());
        assert_eq!(tracker.due(&offers, &tabs, later), [in_active]);
        tracker.forget(in_other.dojo_id);
        assert!(!tracker.first_seen.contains_key(&in_other.dojo_id));

        // The Window that switches to the other Lair claims its offer at once.
        tabs.activate(other_dojo);
        let mut tracker = PresentationOfferTracker::default();
        assert_eq!(tracker.due(&offers, &tabs, start), [in_other]);
    }
}
