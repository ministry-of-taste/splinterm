//! Semantic terminal revisions, damage, bounded replay, and resnapshot gaps.

use std::mem::size_of;

use crate::{ActiveScreen, Cursor, ScrollDirection, ScrollRegion, TerminalEvent};

/// Monotonic semantic terminal revision.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TerminalRevision(u64);

impl TerminalRevision {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }

    pub(crate) fn next(self) -> Self {
        Self(self.0.checked_add(1).expect("terminal revision exhausted"))
    }
}

/// Renderer-independent semantic damage associated with one revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalDamage {
    FullSnapshot,
    Rows {
        start: usize,
        end: usize,
    },
    Scroll {
        direction: ScrollDirection,
        region: ScrollRegion,
        rows: usize,
    },
    Cursor {
        old: Cursor,
        new: Cursor,
    },
    Modes,
    Viewport,
    Dimensions,
    Scrollback,
    Title,
    Palette {
        index: Option<u16>,
    },
    Images {
        screen: ActiveScreen,
    },
}

/// One committed semantic transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalUpdate {
    revision: TerminalRevision,
    damage: Vec<TerminalDamage>,
    events: Vec<TerminalEvent>,
}

impl TerminalUpdate {
    pub(crate) fn new(
        revision: TerminalRevision,
        damage: Vec<TerminalDamage>,
        events: Vec<TerminalEvent>,
    ) -> Self {
        Self {
            revision,
            damage,
            events,
        }
    }

    #[must_use]
    pub const fn revision(&self) -> TerminalRevision {
        self.revision
    }

    #[must_use]
    pub fn damage(&self) -> impl ExactSizeIterator<Item = &TerminalDamage> {
        self.damage.iter()
    }

    #[must_use]
    pub fn events(&self) -> impl ExactSizeIterator<Item = &TerminalEvent> {
        self.events.iter()
    }

    /// Coalesces a nonempty contiguous update sequence for publication while
    /// preserving damage and event order. The resulting revision is the final
    /// revision in the sequence; callers retain the preceding base separately.
    #[must_use]
    pub fn coalesce_contiguous(updates: Vec<Self>) -> Option<Self> {
        if updates.is_empty()
            || !updates.windows(2).all(|pair| {
                pair[0].revision.value().checked_add(1) == Some(pair[1].revision.value())
            })
        {
            return None;
        }
        let damage_count = updates.iter().try_fold(0_usize, |total, update| {
            total.checked_add(update.damage.len())
        })?;
        let event_count = updates.iter().try_fold(0_usize, |total, update| {
            total.checked_add(update.events.len())
        })?;
        let revision = updates.last()?.revision;
        let mut damage = Vec::with_capacity(damage_count);
        let mut events = Vec::with_capacity(event_count);
        for mut update in updates {
            damage.append(&mut update.damage);
            events.append(&mut update.events);
        }
        Some(Self {
            revision,
            damage,
            events,
        })
    }

    /// Appends an already continuity-validated publication successor. This is
    /// used when each side may itself summarize multiple contiguous revisions.
    pub fn append_publication_successor(&mut self, mut update: Self) {
        self.revision = update.revision;
        self.damage.append(&mut update.damage);
        self.events.append(&mut update.events);
    }

    /// Coalesces externally continuity-validated publication summaries once at
    /// materialization, using exact final vector capacities. Individual
    /// summaries may each cover more than one semantic revision.
    #[must_use]
    pub fn coalesce_publication_summaries(updates: Vec<Self>) -> Option<Self> {
        let damage_count = updates.iter().try_fold(0_usize, |total, update| {
            total.checked_add(update.damage.len())
        })?;
        let event_count = updates.iter().try_fold(0_usize, |total, update| {
            total.checked_add(update.events.len())
        })?;
        let revision = updates.last()?.revision;
        let mut damage = Vec::with_capacity(damage_count);
        let mut events = Vec::with_capacity(event_count);
        for mut update in updates {
            damage.append(&mut update.damage);
            events.append(&mut update.events);
        }
        Some(Self {
            revision,
            damage,
            events,
        })
    }

    /// Returns the bytes allocated by this update's owned semantic vectors and
    /// variable-length event bodies, excluding allocator bookkeeping.
    #[must_use]
    pub fn owned_allocation_bytes(&self) -> Option<usize> {
        let mut total = self
            .damage
            .capacity()
            .checked_mul(size_of::<TerminalDamage>())?
            .checked_add(
                self.events
                    .capacity()
                    .checked_mul(size_of::<TerminalEvent>())?,
            )?;
        for event in &self.events {
            let body = match event {
                TerminalEvent::PtyWrite(bytes) | TerminalEvent::ClipboardSet { bytes, .. } => {
                    bytes.capacity()
                }
                TerminalEvent::TitleChanged(title) => title.capacity(),
                TerminalEvent::Bell
                | TerminalEvent::PaletteChanged { .. }
                | TerminalEvent::ClipboardQuery { .. }
                | TerminalEvent::UnsupportedSequence(_)
                | TerminalEvent::ImageRejected(_)
                | TerminalEvent::StringTruncated(_)
                | TerminalEvent::EventQueueOverflow => 0,
            };
            total = total.checked_add(body)?;
        }
        Some(total)
    }
}

/// Contiguous updates after a requested base revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateBatch {
    base: TerminalRevision,
    current: TerminalRevision,
    updates: Vec<TerminalUpdate>,
}

impl UpdateBatch {
    pub(crate) fn new(
        base: TerminalRevision,
        current: TerminalRevision,
        updates: Vec<TerminalUpdate>,
    ) -> Self {
        Self {
            base,
            current,
            updates,
        }
    }

    #[must_use]
    pub const fn base(&self) -> TerminalRevision {
        self.base
    }

    #[must_use]
    pub const fn current(&self) -> TerminalRevision {
        self.current
    }

    #[must_use]
    pub fn updates(&self) -> impl ExactSizeIterator<Item = &TerminalUpdate> {
        self.updates.iter()
    }

    /// Consumes the batch and returns its already-owned updates.
    #[must_use]
    pub fn into_updates(self) -> Vec<TerminalUpdate> {
        self.updates
    }
}

/// The requested update base is unavailable; the caller must take a snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResnapshotRequired {
    requested: TerminalRevision,
    oldest_available: TerminalRevision,
    current: TerminalRevision,
}

impl ResnapshotRequired {
    pub(crate) const fn new(
        requested: TerminalRevision,
        oldest_available: TerminalRevision,
        current: TerminalRevision,
    ) -> Self {
        Self {
            requested,
            oldest_available,
            current,
        }
    }

    #[must_use]
    pub const fn requested(self) -> TerminalRevision {
        self.requested
    }

    #[must_use]
    pub const fn oldest_available(self) -> TerminalRevision {
        self.oldest_available
    }

    #[must_use]
    pub const fn current(self) -> TerminalRevision {
        self.current
    }
}

/// Ordered damage retained across merges before one full snapshot replaces it.
///
/// A held synchronized update merges every committed change, so without a
/// bound a producer could grow the list for the whole episode. The bound
/// leaves room for several complete redraws of a large grid, which commit
/// more than one entry per printed cell.
const MERGED_DAMAGE_LIMIT: usize = 262_144;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ChangeSet {
    pub(crate) damage: Vec<TerminalDamage>,
    pub(crate) events: Vec<TerminalEvent>,
}

impl ChangeSet {
    fn is_full(&self) -> bool {
        matches!(self.damage.as_slice(), [TerminalDamage::FullSnapshot])
    }

    pub(crate) fn row(&mut self, row: usize) {
        self.rows(row, row + 1);
    }

    pub(crate) fn rows(&mut self, start: usize, end: usize) {
        if start >= end || self.is_full() {
            return;
        }
        if let Some(TerminalDamage::Rows {
            start: existing_start,
            end: existing_end,
        }) = self
            .damage
            .iter_mut()
            .find(|item| matches!(item, TerminalDamage::Rows { .. }))
            && start <= *existing_end
            && end >= *existing_start
        {
            *existing_start = (*existing_start).min(start);
            *existing_end = (*existing_end).max(end);
            return;
        }
        self.damage.push(TerminalDamage::Rows { start, end });
    }

    pub(crate) fn full(&mut self) {
        self.damage.clear();
        self.damage.push(TerminalDamage::FullSnapshot);
    }

    pub(crate) fn push(&mut self, damage: TerminalDamage) {
        if matches!(damage, TerminalDamage::FullSnapshot) {
            self.full();
        } else if !self.is_full() && !self.damage.contains(&damage) {
            self.damage.push(damage);
        }
    }

    pub(crate) fn merge(&mut self, mut other: Self, event_limit: usize) {
        if other.is_full() {
            self.full();
        } else if !self.is_full() {
            // Preserve parser order, particularly for repeated scroll operations.
            // Wire publication coalesces row/metadata flags against the final snapshot.
            self.damage.append(&mut other.damage);
            if self.damage.len() > MERGED_DAMAGE_LIMIT {
                self.full();
            }
        }
        let available = event_limit.saturating_sub(self.events.len());
        self.events.extend(other.events.into_iter().take(available));
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.damage.is_empty() && self.events.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scroll_damage() -> TerminalDamage {
        TerminalDamage::Scroll {
            direction: ScrollDirection::Forward,
            region: ScrollRegion::new(0, 4),
            rows: 1,
        }
    }

    #[test]
    fn full_snapshot_damage_is_canonical_and_terminal() {
        let mut change = ChangeSet::default();
        change.push(TerminalDamage::Modes);
        change.push(TerminalDamage::FullSnapshot);
        change.push(TerminalDamage::Title);
        change.rows(0, 2);

        assert!(change.is_full());
        assert_eq!(change.damage, vec![TerminalDamage::FullSnapshot]);
    }

    #[test]
    fn merging_full_snapshot_replaces_damage_but_preserves_bounded_events() {
        let mut accumulated = ChangeSet {
            damage: vec![TerminalDamage::Modes, TerminalDamage::Title],
            events: vec![TerminalEvent::Bell],
        };
        let full = ChangeSet {
            damage: vec![TerminalDamage::FullSnapshot],
            events: vec![TerminalEvent::TitleChanged("frame".to_owned())],
        };

        accumulated.merge(full, 1);

        assert!(accumulated.is_full());
        assert_eq!(accumulated.damage, vec![TerminalDamage::FullSnapshot]);
        assert_eq!(accumulated.events, vec![TerminalEvent::Bell]);
    }

    #[test]
    fn publication_coalescing_preserves_order_and_rejects_revision_gaps() {
        let first = TerminalUpdate::new(
            TerminalRevision::new(4),
            vec![TerminalDamage::Modes],
            vec![TerminalEvent::Bell],
        );
        let second = TerminalUpdate::new(
            TerminalRevision::new(5),
            vec![TerminalDamage::Title],
            vec![TerminalEvent::TitleChanged("next".to_owned())],
        );
        let coalesced = TerminalUpdate::coalesce_contiguous(vec![first.clone(), second]).unwrap();
        assert_eq!(coalesced.revision(), TerminalRevision::new(5));
        assert_eq!(coalesced.damage.len(), 2);
        assert_eq!(coalesced.damage.capacity(), 2);
        assert_eq!(coalesced.events.len(), 2);
        assert_eq!(coalesced.events.capacity(), 2);
        assert_eq!(
            coalesced.damage().cloned().collect::<Vec<_>>(),
            vec![TerminalDamage::Modes, TerminalDamage::Title]
        );
        assert_eq!(
            coalesced.events().cloned().collect::<Vec<_>>(),
            vec![
                TerminalEvent::Bell,
                TerminalEvent::TitleChanged("next".to_owned())
            ]
        );
        assert!(
            TerminalUpdate::coalesce_contiguous(vec![
                first,
                TerminalUpdate::new(TerminalRevision::new(6), Vec::new(), Vec::new()),
            ])
            .is_none()
        );
    }

    #[test]
    fn publication_summaries_coalesce_once_with_exact_capacity_and_order() {
        let first = TerminalUpdate::new(
            TerminalRevision::new(4),
            vec![TerminalDamage::Modes],
            vec![TerminalEvent::Bell],
        );
        let second = TerminalUpdate::new(
            TerminalRevision::new(10),
            vec![TerminalDamage::Title, scroll_damage()],
            vec![TerminalEvent::TitleChanged("summary".to_owned())],
        );

        let coalesced = TerminalUpdate::coalesce_publication_summaries(vec![first, second])
            .expect("nonempty publication summaries");

        assert_eq!(coalesced.revision(), TerminalRevision::new(10));
        assert_eq!(coalesced.damage.len(), 3);
        assert_eq!(coalesced.damage.capacity(), 3);
        assert_eq!(coalesced.events.len(), 2);
        assert_eq!(coalesced.events.capacity(), 2);
        assert_eq!(
            coalesced.damage,
            vec![
                TerminalDamage::Modes,
                TerminalDamage::Title,
                scroll_damage()
            ]
        );
        assert_eq!(
            coalesced.events,
            vec![
                TerminalEvent::Bell,
                TerminalEvent::TitleChanged("summary".to_owned())
            ]
        );
        assert!(TerminalUpdate::coalesce_publication_summaries(Vec::new()).is_none());
    }

    #[test]
    fn merged_damage_collapses_to_a_full_snapshot_at_its_bound() {
        let mut accumulated = ChangeSet::default();
        for _ in 0..=MERGED_DAMAGE_LIMIT {
            let mut next = ChangeSet::default();
            next.push(scroll_damage());
            accumulated.merge(next, 0);
        }
        assert_eq!(accumulated.damage, vec![TerminalDamage::FullSnapshot]);

        let mut next = ChangeSet::default();
        next.push(scroll_damage());
        accumulated.merge(next, 0);
        assert_eq!(accumulated.damage, vec![TerminalDamage::FullSnapshot]);
    }

    #[test]
    fn many_ordered_merges_remain_append_only() {
        const MERGES: usize = 20_000;
        let mut accumulated = ChangeSet::default();

        for _ in 0..MERGES {
            let mut next = ChangeSet::default();
            next.push(scroll_damage());
            accumulated.merge(next, 0);
        }

        assert_eq!(accumulated.damage.len(), MERGES);
        assert!(
            accumulated
                .damage
                .iter()
                .all(|damage| damage == &scroll_damage())
        );
    }
}
