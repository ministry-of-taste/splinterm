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
    ManagedDojo, PreparedManagedDojo, TopologyManagerState, cancel_pane_tasks,
    prepare_managed_dojo, window_dojo_identity, window_has_tab_capacity,
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
    show_background_tab(updates, state, prepared).await
}

/// Hands a prepared Dojo to the frontend as a background tab and records it
/// once the frontend has it. A tab that is not shown stops its panes.
async fn show_background_tab(
    updates: &mpsc::Sender<WindowTopologyUpdate>,
    state: &mut TopologyManagerState,
    prepared: PreparedManagedDojo,
) -> Result<()> {
    let lair_id = prepared.identity.lair_id;
    let dojo_id = prepared.dojo.id;
    // Nothing else changes this Window's tabs meanwhile, so this only guards.
    if state.tabs.get(dojo_id).is_some() || !window_has_tab_capacity(state.tabs.len()) {
        cancel_pane_tasks(prepared.pane_tasks).await;
        bail!("the Window already shows the offered Dojo or is full");
    }
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
    use std::{collections::VecDeque, path::PathBuf};

    use splinterm_core::{Dojo, LairId, Topology};
    use splinterm_protocol::{
        ClientFrame, PROTOCOL_VERSION, ServerFrame, ServerLimits, encode_frame,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

    /// A Lair holding the Window's Dojo and an agent's new one, which the
    /// daemon offers.
    fn offered_snapshot() -> (TopologySnapshot, DojoId) {
        let mut topology = Topology::new();
        let lair_id = topology
            .create_lair("agent", PathBuf::from("/tmp"))
            .unwrap()
            .id;
        topology
            .new_dojo_at(
                topology.revision(),
                lair_id,
                Dojo::with_shell("Dojo 2", PathBuf::from("/tmp")),
            )
            .unwrap();
        let dojo_id = topology.find_lair(lair_id).unwrap().dojos[1].id;
        let snapshot = TopologySnapshot {
            revision: topology.revision(),
            topology,
            runtimes: Vec::new(),
            presentation_offers: vec![DojoPresentationOffer { lair_id, dojo_id }],
        };
        (snapshot, dojo_id)
    }

    /// A Window whose one, active tab is the Lair's first Dojo.
    fn window_state(snapshot: &TopologySnapshot) -> TopologyManagerState {
        let lair = snapshot.topology.lairs().next().unwrap();
        let dojo = &lair.dojos[0];
        let managed = ManagedDojo {
            identity: window_dojo_identity(snapshot.revision, lair, dojo),
            root: dojo.root.clone(),
            pending_focus: None,
            pane_tasks: HashMap::new(),
        };
        TopologyManagerState {
            tabs: WindowTabSet::new(DojoTab::new(lair.id, dojo.id, managed)),
            transient_owners: HashMap::new(),
            agent_status_republish: false,
        }
    }

    /// A daemon end that answers each request with the next reply and,
    /// once the client closes, returns the requests it saw.
    async fn scripted_daemon(
        replies: Vec<Response>,
    ) -> (Connection, tokio::task::JoinHandle<Vec<Request>>) {
        let (client, server) = tokio::io::duplex(1 << 16);
        let daemon = tokio::spawn(async move {
            let (mut reader, mut writer) = tokio::io::split(server);
            let mut replies = VecDeque::from(replies);
            let mut requests = Vec::new();
            while let Ok(size) = reader.read_u32().await {
                let mut bytes = vec![0; size as usize];
                reader.read_exact(&mut bytes).await.unwrap();
                let frame = match serde_json::from_slice(&bytes).unwrap() {
                    ClientFrame::Hello { .. } => ServerFrame::Hello {
                        version: PROTOCOL_VERSION,
                        limits: ServerLimits::default(),
                        development_terminal_access: false,
                        daemon_hostname: None,
                        daemon_generation: 1,
                    },
                    ClientFrame::Request {
                        request_id,
                        request,
                        ..
                    } => {
                        requests.push(request);
                        let result = replies.pop_front().expect("an unscripted request");
                        ServerFrame::Response { request_id, result }
                    }
                    frame => panic!("unexpected client frame: {frame:?}"),
                };
                writer
                    .write_all(&encode_frame(&frame).unwrap())
                    .await
                    .unwrap();
            }
            requests
        });
        let (reader, writer) = tokio::io::split(client);
        let connection = Connection::connect_remote_interactive_transport(reader, writer)
            .await
            .unwrap();
        (connection, daemon)
    }

    #[tokio::test]
    async fn only_an_offer_is_claimed_and_a_refused_claim_opens_nothing() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let (offered, dojo_id) = offered_snapshot();
            let mut quiet = offered.clone();
            quiet.presentation_offers.clear();
            let (mut connection, daemon) =
                scripted_daemon(vec![Response::DojoPresentationClaimed { granted: false }]).await;
            let mut state = window_state(&offered);
            let (updates, mut frontend) = mpsc::channel(4);
            let mut tracker = PresentationOfferTracker::default();
            let factory = ConnectionFactory::local();
            let config = AppConfig::default();
            let image_cache = SharedImageContentCache::default();
            // Without offers nothing is sent, so an older daemon never sees
            // the claim; the one offer, due at once in the active tab's
            // Lair, is claimed once.
            for snapshot in [&quiet, &offered] {
                assert!(
                    claim_presentation_offers(
                        &factory,
                        &mut connection,
                        &config,
                        &image_cache,
                        &updates,
                        &mut state,
                        snapshot,
                        &mut tracker,
                    )
                    .await
                );
            }
            drop(connection);
            assert_eq!(
                daemon.await.unwrap(),
                [Request::ClaimDojoPresentation { dojo_id }]
            );
            assert!(frontend.try_recv().is_err());
            assert_eq!(state.tabs.len(), 1);
        })
        .await
        .expect("the claim scenario timed out");
    }

    #[tokio::test]
    async fn a_granted_dojo_opens_behind_the_active_tab() {
        let (snapshot, dojo_id) = offered_snapshot();
        let mut state = window_state(&snapshot);
        let active = state.tabs.active().map(|tab| tab.dojo_id);
        let lair = snapshot.topology.lairs().next().unwrap();
        let prepared = || PreparedManagedDojo {
            identity: window_dojo_identity(snapshot.revision, lair, &lair.dojos[1]),
            dojo: lair.dojos[1].clone(),
            panes: Vec::new(),
            pane_tasks: HashMap::new(),
        };
        let (updates, mut frontend) = mpsc::channel(4);
        let (shown, opened) = tokio::join!(
            show_background_tab(&updates, &mut state, prepared()),
            async {
                let Some(WindowTopologyUpdate::OpenTab {
                    identity,
                    acknowledged,
                    explorer_target,
                    activate,
                    ..
                }) = frontend.recv().await
                else {
                    panic!("the frontend was not asked to open a tab");
                };
                assert!(!activate, "the agent's Dojo must not take the active tab");
                assert!(explorer_target.is_none());
                acknowledged.send(Ok(())).unwrap();
                identity.dojo_id
            }
        );
        shown.unwrap();
        assert_eq!(opened, dojo_id);
        assert_eq!(state.tabs.len(), 2);
        assert_eq!(state.tabs.active().map(|tab| tab.dojo_id), active);
        assert!(state.tabs.get(dojo_id).is_some());

        // A Dojo the Window already shows is neither sent nor recorded again.
        assert!(
            show_background_tab(&updates, &mut state, prepared())
                .await
                .is_err()
        );
        assert!(frontend.try_recv().is_err());
        assert_eq!(state.tabs.len(), 2);
    }
}
