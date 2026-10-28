//! Plan 0067 M3: Dojos an agent just created, offered to the Windows showing
//! their Lair so exactly one of them opens each as a background tab.
//!
//! Offers are advisory, in memory only, and short-lived. A handoff or restart
//! drops them, and nothing is lost but the tab: the Dojo is still listed in
//! the picker and the Explorer.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use splinterm_core::{DojoId, LairId, Topology};
use splinterm_protocol::{DojoPresentationOffer, MAX_DOJO_PRESENTATION_OFFERS};

/// How long a Window may take to notice and claim an offer.
pub const PRESENTATION_OFFER_LIFETIME: Duration = Duration::from_secs(30);

#[derive(Debug, Default)]
pub struct PresentationOffers {
    offers: HashMap<DojoId, (LairId, Instant)>,
}

fn present(topology: &Topology, lair_id: LairId, dojo_id: DojoId) -> bool {
    topology
        .find_lair(lair_id)
        .is_some_and(|lair| lair.dojos.iter().any(|dojo| dojo.id == dojo_id))
}

impl PresentationOffers {
    /// Offers a just-created Dojo. Expired offers and offers whose Dojo is
    /// gone from `topology` are dropped first; beyond
    /// [`MAX_DOJO_PRESENTATION_OFFERS`], the oldest offer gives way.
    pub fn offer(&mut self, topology: &Topology, lair_id: LairId, dojo_id: DojoId, now: Instant) {
        self.offers.retain(|dojo, (lair, created)| {
            now.saturating_duration_since(*created) < PRESENTATION_OFFER_LIFETIME
                && present(topology, *lair, *dojo)
        });
        self.offers.insert(dojo_id, (lair_id, now));
        while self.offers.len() > MAX_DOJO_PRESENTATION_OFFERS {
            let Some(oldest) = self
                .offers
                .iter()
                .min_by_key(|(dojo, (_, created))| (*created, **dojo))
                .map(|(dojo, _)| *dojo)
            else {
                break;
            };
            self.offers.remove(&oldest);
        }
    }

    /// The unexpired offers whose Dojo `snapshot` shows, oldest first. An
    /// offer the snapshot does not show yet is kept: the snapshot may predate
    /// the Dojo.
    pub fn shown(&mut self, snapshot: &Topology, now: Instant) -> Vec<DojoPresentationOffer> {
        self.offers.retain(|_, (_, created)| {
            now.saturating_duration_since(*created) < PRESENTATION_OFFER_LIFETIME
        });
        let mut shown = self
            .offers
            .iter()
            .filter(|(dojo, (lair, _))| present(snapshot, *lair, **dojo))
            .map(|(dojo, (lair, created))| (*created, *dojo, *lair))
            .collect::<Vec<_>>();
        shown.sort_unstable();
        shown
            .into_iter()
            .map(|(_, dojo_id, lair_id)| DojoPresentationOffer { lair_id, dojo_id })
            .collect()
    }

    /// Takes the offer for `dojo_id`, granting it once: `true` only when the
    /// offer existed, had not expired, and its Dojo is still in its Lair.
    pub fn claim(&mut self, topology: &Topology, dojo_id: DojoId, now: Instant) -> bool {
        self.offers.remove(&dojo_id).is_some_and(|(lair, created)| {
            now.saturating_duration_since(created) < PRESENTATION_OFFER_LIFETIME
                && present(topology, lair, dojo_id)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use splinterm_core::Dojo;

    use super::*;

    fn topology_with_dojos(count: usize) -> (Topology, LairId, Vec<DojoId>) {
        let mut topology = Topology::new();
        let lair_id = topology
            .create_lair("offers", PathBuf::from("/tmp"))
            .unwrap()
            .id;
        for index in 0..count {
            topology
                .new_dojo_at(
                    topology.revision(),
                    lair_id,
                    Dojo::with_shell(format!("Dojo {}", index + 2), PathBuf::from("/tmp")),
                )
                .unwrap();
        }
        let dojos = topology
            .find_lair(lair_id)
            .unwrap()
            .dojos
            .iter()
            .map(|dojo| dojo.id)
            .collect();
        (topology, lair_id, dojos)
    }

    #[test]
    fn an_offer_is_granted_exactly_once() {
        let (topology, lair_id, dojos) = topology_with_dojos(1);
        let now = Instant::now();
        let mut offers = PresentationOffers::default();
        offers.offer(&topology, lair_id, dojos[1], now);
        assert_eq!(
            offers.shown(&topology, now),
            [DojoPresentationOffer {
                lair_id,
                dojo_id: dojos[1],
            }]
        );
        assert!(!offers.claim(&topology, dojos[0], now));
        assert!(offers.claim(&topology, dojos[1], now));
        assert!(!offers.claim(&topology, dojos[1], now));
        assert!(offers.shown(&topology, now).is_empty());
    }

    #[test]
    fn offers_expire_and_follow_the_topology() {
        let (topology, lair_id, dojos) = topology_with_dojos(2);
        let start = Instant::now();
        let mut offers = PresentationOffers::default();
        offers.offer(&topology, lair_id, dojos[1], start);
        let later = start + PRESENTATION_OFFER_LIFETIME;
        assert!(offers.shown(&topology, later).is_empty());
        offers.offer(&topology, lair_id, dojos[1], start);
        assert!(!offers.claim(&topology, dojos[1], later));

        // A snapshot taken before the Dojo existed hides the offer but keeps it.
        let (earlier, _, _) = topology_with_dojos(0);
        offers.offer(&topology, lair_id, dojos[2], start);
        assert!(offers.shown(&earlier, start).is_empty());
        assert_eq!(offers.shown(&topology, start).len(), 1);

        // A Dojo that is gone is never granted, and the next offer prunes it.
        let mut closed = topology.clone();
        closed
            .terminate_dojo_at(closed.revision(), dojos[2])
            .expect("the offered Dojo closes");
        assert!(!offers.claim(&closed, dojos[2], start));
        offers.offer(&topology, lair_id, dojos[2], start);
        offers.offer(&closed, lair_id, dojos[1], start);
        assert_eq!(
            offers.shown(&topology, start),
            [DojoPresentationOffer {
                lair_id,
                dojo_id: dojos[1],
            }]
        );
    }

    #[test]
    fn the_oldest_offer_gives_way_beyond_the_bound() {
        let (topology, lair_id, dojos) = topology_with_dojos(MAX_DOJO_PRESENTATION_OFFERS);
        let start = Instant::now();
        let mut offers = PresentationOffers::default();
        for (index, dojo_id) in dojos.iter().enumerate() {
            let at = start + Duration::from_millis(u64::try_from(index).unwrap());
            offers.offer(&topology, lair_id, *dojo_id, at);
        }
        let shown = offers.shown(&topology, start);
        assert_eq!(shown.len(), MAX_DOJO_PRESENTATION_OFFERS);
        assert_eq!(shown[0].dojo_id, dojos[1]);
        assert!(!offers.claim(&topology, dojos[0], start));
    }
}
