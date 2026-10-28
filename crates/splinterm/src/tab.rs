//! Client-local ordered Dojo tabs for one native Window.
//!
//! This state is deliberately presentation-only. It is never serialized or sent
//! to the daemon, and removing an entry has no topology semantics.

#![allow(
    clippy::must_use_candidate,
    reason = "tab mutations and iterator access are intentionally consumed contextually"
)]

use splinterm_core::{DojoId, LairId};
use unicode_width::UnicodeWidthChar;

/// Maximum number of Dojo references retained by one graphical Window.
pub const MAX_WINDOW_TABS: usize = 32;

pub fn sanitized_tab_label(value: &str, maximum_scalars: usize, maximum_cells: usize) -> String {
    let mut label = String::new();
    let mut cells = 0;
    for character in value.chars().take(maximum_scalars) {
        if matches!(
            character,
            '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
        ) {
            continue;
        }
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        let width = UnicodeWidthChar::width(character).unwrap_or(0).min(2);
        if cells + width > maximum_cells {
            break;
        }
        cells += width;
        label.push(character);
    }
    let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
    if label.is_empty() {
        "Untitled Dojo".to_owned()
    } else {
        label
    }
}

#[derive(Debug)]
pub struct DojoTab<T> {
    pub lair_id: LairId,
    pub dojo_id: DojoId,
    pub value: T,
}

impl<T> DojoTab<T> {
    pub const fn new(lair_id: LairId, dojo_id: DojoId, value: T) -> Self {
        Self {
            lair_id,
            dojo_id,
            value,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenTabOutcome {
    Opened,
    ActivatedExisting,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TabLimitReached;

impl std::fmt::Display for TabLimitReached {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "a Window may contain at most {MAX_WINDOW_TABS} Dojo tabs"
        )
    }
}

impl std::error::Error for TabLimitReached {}

#[derive(Debug)]
pub struct WindowTabSet<T> {
    tabs: Vec<DojoTab<T>>,
    active: usize,
    activation_history: Vec<DojoId>,
}

impl<T> WindowTabSet<T> {
    pub fn new(initial: DojoTab<T>) -> Self {
        let dojo_id = initial.dojo_id;
        Self {
            tabs: vec![initial],
            active: 0,
            activation_history: vec![dojo_id],
        }
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn active(&self) -> Option<&DojoTab<T>> {
        self.tabs.get(self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut DojoTab<T>> {
        self.tabs.get_mut(self.active)
    }

    pub fn get(&self, dojo_id: DojoId) -> Option<&DojoTab<T>> {
        self.tabs.iter().find(|tab| tab.dojo_id == dojo_id)
    }

    pub fn get_mut(&mut self, dojo_id: DojoId) -> Option<&mut DojoTab<T>> {
        self.tabs.iter_mut().find(|tab| tab.dojo_id == dojo_id)
    }

    pub fn at(&self, index: usize) -> Option<&DojoTab<T>> {
        self.tabs.get(index)
    }

    pub fn recent_in_lair(&self, lair_id: LairId) -> Option<DojoId> {
        self.activation_history
            .iter()
            .rev()
            .copied()
            .find(|dojo_id| self.get(*dojo_id).is_some_and(|tab| tab.lair_id == lair_id))
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &DojoTab<T>> {
        self.tabs.iter()
    }

    pub fn iter_mut(&mut self) -> impl ExactSizeIterator<Item = &mut DojoTab<T>> {
        self.tabs.iter_mut()
    }

    pub fn open_or_activate(&mut self, tab: DojoTab<T>) -> Result<OpenTabOutcome, TabLimitReached> {
        if let Some(index) = self.index_of(tab.dojo_id) {
            self.active = index;
            self.record_activation(tab.dojo_id);
            return Ok(OpenTabOutcome::ActivatedExisting);
        }
        if self.tabs.len() == MAX_WINDOW_TABS {
            return Err(TabLimitReached);
        }
        let dojo_id = tab.dojo_id;
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.record_activation(dojo_id);
        Ok(OpenTabOutcome::Opened)
    }

    /// Appends a tab without activating it (Plan 0067 M3): the active tab,
    /// its activation history, and so keyboard focus stay as they were.
    /// Returns `false` when the Dojo already has a tab, which is left where
    /// it is.
    ///
    /// # Errors
    ///
    /// Returns [`TabLimitReached`] when the Window already has
    /// [`MAX_WINDOW_TABS`] tabs.
    pub fn open_background(&mut self, tab: DojoTab<T>) -> Result<bool, TabLimitReached> {
        if self.index_of(tab.dojo_id).is_some() {
            return Ok(false);
        }
        if self.tabs.len() == MAX_WINDOW_TABS {
            return Err(TabLimitReached);
        }
        self.tabs.push(tab);
        Ok(true)
    }

    pub fn activate(&mut self, dojo_id: DojoId) -> bool {
        let Some(index) = self.index_of(dojo_id) else {
            return false;
        };
        self.active = index;
        self.record_activation(dojo_id);
        true
    }

    pub fn next(&self) -> Option<DojoId> {
        (!self.tabs.is_empty()).then(|| self.tabs[(self.active + 1) % self.tabs.len()].dojo_id)
    }

    pub fn previous(&self) -> Option<DojoId> {
        (!self.tabs.is_empty()).then(|| {
            let index = self.active.checked_sub(1).unwrap_or(self.tabs.len() - 1);
            self.tabs[index].dojo_id
        })
    }

    pub fn activate_next(&mut self) -> Option<DojoId> {
        let dojo_id = self.next()?;
        self.activate(dojo_id);
        Some(dojo_id)
    }

    pub fn activate_previous(&mut self) -> Option<DojoId> {
        if self.tabs.is_empty() {
            return None;
        }
        let dojo_id = self.previous()?;
        self.activate(dojo_id);
        Some(dojo_id)
    }

    /// Removes a client-local reference and selects its nearest surviving neighbor.
    pub fn selection_after_close(&self, dojo_id: DojoId) -> Option<DojoId> {
        let index = self.index_of(dojo_id)?;
        if self.tabs.len() == 1 {
            return None;
        }
        let selected = if index == self.active {
            if index + 1 < self.tabs.len() {
                index + 1
            } else {
                index - 1
            }
        } else {
            self.active
        };
        Some(self.tabs[selected].dojo_id)
    }

    pub fn close(&mut self, dojo_id: DojoId) -> Option<DojoTab<T>> {
        let index = self.index_of(dojo_id)?;
        let removed = self.tabs.remove(index);
        self.activation_history
            .retain(|candidate| *candidate != dojo_id);
        if self.tabs.is_empty() {
            self.active = 0;
        } else if index < self.active {
            self.active -= 1;
        } else if index == self.active {
            self.active = index.min(self.tabs.len() - 1);
        }
        Some(removed)
    }

    pub fn move_active(&mut self, delta: isize) -> bool {
        if self.tabs.len() < 2 || delta == 0 {
            return false;
        }
        let target = if delta.is_negative() {
            self.active.saturating_sub(delta.unsigned_abs())
        } else {
            self.active
                .saturating_add(delta.unsigned_abs())
                .min(self.tabs.len() - 1)
        };
        let dojo_id = self.tabs[self.active].dojo_id;
        target != self.active && self.move_tab(dojo_id, target)
    }

    pub fn move_tab(&mut self, dojo_id: DojoId, target: usize) -> bool {
        let Some(source) = self.index_of(dojo_id) else {
            return false;
        };
        if target >= self.tabs.len() || source == target {
            return target < self.tabs.len();
        }
        let active_dojo = self.active().map(|tab| tab.dojo_id);
        let tab = self.tabs.remove(source);
        self.tabs.insert(target, tab);
        if let Some(active_dojo) = active_dojo {
            self.active = self
                .index_of(active_dojo)
                .expect("active tab remains present after reordering");
        }
        true
    }

    fn record_activation(&mut self, dojo_id: DojoId) {
        self.activation_history
            .retain(|candidate| *candidate != dojo_id);
        self.activation_history.push(dojo_id);
    }

    fn index_of(&self, dojo_id: DojoId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.dojo_id == dojo_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(value: usize) -> DojoTab<usize> {
        DojoTab::new(LairId::new(), DojoId::new(), value)
    }

    #[test]
    fn tab_open_activates_duplicates_without_replacing_state() {
        let first = tab(1);
        let first_id = first.dojo_id;
        let mut tabs = WindowTabSet::new(first);
        let second = tab(2);
        let second_id = second.dojo_id;
        assert_eq!(
            tabs.open_or_activate(second).unwrap(),
            OpenTabOutcome::Opened
        );

        assert_eq!(
            tabs.open_or_activate(DojoTab::new(LairId::new(), first_id, 99)),
            Ok(OpenTabOutcome::ActivatedExisting)
        );
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs.active().map(|tab| tab.dojo_id), Some(first_id));
        assert_eq!(tabs.get(first_id).map(|tab| tab.value), Some(1));
        assert_eq!(tabs.get(second_id).map(|tab| tab.value), Some(2));
    }

    #[test]
    fn a_background_tab_keeps_the_active_tab_and_its_history() {
        let lair_id = LairId::new();
        let first = DojoTab::new(lair_id, DojoId::new(), 1);
        let first_id = first.dojo_id;
        let mut tabs = WindowTabSet::new(first);
        let second = tab(2);
        let second_id = second.dojo_id;
        tabs.open_or_activate(second).unwrap();
        tabs.activate(first_id);
        let background = DojoTab::new(lair_id, DojoId::new(), 3);
        let background_id = background.dojo_id;
        assert_eq!(tabs.open_background(background), Ok(true));
        assert_eq!(tabs.len(), 3);
        assert_eq!(tabs.at(2).map(|tab| tab.dojo_id), Some(background_id));
        assert_eq!(tabs.active().map(|tab| tab.dojo_id), Some(first_id));
        // It is not in the activation history, so it is not the recent
        // Dojo of its Lair, and closing the active tab still selects by
        // position.
        assert_eq!(tabs.recent_in_lair(lair_id), Some(first_id));
        assert_eq!(tabs.selection_after_close(first_id), Some(second_id));
        // A Dojo that already has a tab is left where it is.
        assert_eq!(
            tabs.open_background(DojoTab::new(lair_id, second_id, 9)),
            Ok(false)
        );
        assert_eq!(tabs.get(second_id).map(|tab| tab.value), Some(2));
        assert_eq!(tabs.active().map(|tab| tab.dojo_id), Some(first_id));
        while tabs.len() < MAX_WINDOW_TABS {
            tabs.open_background(tab(0)).unwrap();
        }
        assert_eq!(tabs.open_background(tab(0)), Err(TabLimitReached));
        assert_eq!(tabs.active().map(|tab| tab.dojo_id), Some(first_id));
    }

    #[test]
    fn tab_navigation_wraps_in_both_directions() {
        let first = tab(1);
        let first_id = first.dojo_id;
        let mut tabs = WindowTabSet::new(first);
        let second = tab(2);
        let second_id = second.dojo_id;
        tabs.open_or_activate(second).unwrap();

        assert_eq!(tabs.activate_next(), Some(first_id));
        assert_eq!(tabs.activate_previous(), Some(second_id));
    }

    #[test]
    fn tab_close_selects_right_neighbor_then_left_and_handles_external_removal() {
        let first = tab(1);
        let first_id = first.dojo_id;
        let mut tabs = WindowTabSet::new(first);
        let second = tab(2);
        let second_id = second.dojo_id;
        let third = tab(3);
        let third_id = third.dojo_id;
        tabs.open_or_activate(second).unwrap();
        tabs.open_or_activate(third).unwrap();
        assert!(tabs.activate(second_id));
        assert_eq!(tabs.selection_after_close(second_id), Some(third_id));

        assert_eq!(tabs.close(second_id).map(|tab| tab.value), Some(2));
        assert_eq!(tabs.active().map(|tab| tab.dojo_id), Some(third_id));
        assert_eq!(tabs.close(third_id).map(|tab| tab.value), Some(3));
        assert_eq!(tabs.active().map(|tab| tab.dojo_id), Some(first_id));
        assert_eq!(tabs.close(first_id).map(|tab| tab.value), Some(1));
        assert!(tabs.is_empty());
        assert_eq!(tabs.activate_next(), None);
    }

    #[test]
    fn tab_bound_rejects_without_evicting_or_activating() {
        let first = tab(0);
        let mut tabs = WindowTabSet::new(first);
        for value in 1..MAX_WINDOW_TABS {
            tabs.open_or_activate(tab(value)).unwrap();
        }
        let active = tabs.active().map(|tab| tab.dojo_id);

        assert_eq!(tabs.open_or_activate(tab(99)), Err(TabLimitReached));
        assert_eq!(tabs.len(), MAX_WINDOW_TABS);
        assert_eq!(tabs.active().map(|tab| tab.dojo_id), active);
    }

    #[test]
    fn tab_labels_remove_bidi_controls_and_collapse_control_space() {
        assert_eq!(
            sanitized_tab_label("work\u{061c}\u{200e}\u{202e}\n dojo", 128, 48),
            "work dojo"
        );
        assert_eq!(
            sanitized_tab_label("\u{2066}\u{2069}", 128, 48),
            "Untitled Dojo"
        );
    }

    #[test]
    fn lair_recency_and_numeric_order_keep_stable_ids() {
        let first_lair = LairId::new();
        let second_lair = LairId::new();
        let first = DojoTab::new(first_lair, DojoId::new(), 1);
        let first_id = first.dojo_id;
        let mut tabs = WindowTabSet::new(first);
        let second = DojoTab::new(second_lair, DojoId::new(), 2);
        let second_id = second.dojo_id;
        let third = DojoTab::new(second_lair, DojoId::new(), 3);
        let third_id = third.dojo_id;
        tabs.open_or_activate(second).unwrap();
        tabs.open_or_activate(third).unwrap();
        assert_eq!(tabs.recent_in_lair(second_lair), Some(third_id));
        assert!(tabs.activate(second_id));
        assert_eq!(tabs.recent_in_lair(second_lair), Some(second_id));
        assert_eq!(tabs.at(0).map(|tab| tab.dojo_id), Some(first_id));
        assert!(tabs.move_active(-1));
        assert_eq!(tabs.at(0).map(|tab| tab.dojo_id), Some(second_id));
        assert!(!tabs.move_active(-1));
    }

    #[test]
    fn tab_reorder_preserves_stable_active_identity() {
        let first = tab(1);
        let first_id = first.dojo_id;
        let mut tabs = WindowTabSet::new(first);
        let second = tab(2);
        let second_id = second.dojo_id;
        tabs.open_or_activate(second).unwrap();
        assert!(tabs.move_tab(second_id, 0));

        assert_eq!(tabs.active().map(|tab| tab.dojo_id), Some(second_id));
        assert_eq!(
            tabs.iter().map(|tab| tab.dojo_id).collect::<Vec<_>>(),
            vec![second_id, first_id]
        );
    }
}
