//! Client-owned bounded scrollback viewport state.

use splinterm_protocol::{ActiveScreen, TerminalRow, TerminalSnapshot};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScrollbackViewport {
    offset_from_bottom: usize,
    unseen_rows: usize,
    anchor_row_id: Option<u64>,
    /// The screen as it was when this detached viewport began holding its
    /// history. While held, the viewport composes the cached history with this
    /// screen instead of the live one, so new output that cannot be joined to
    /// the cache never moves or clears what the user is reading.
    held_screen: Option<Vec<TerminalRow>>,
}

impl ScrollbackViewport {
    #[must_use]
    pub fn offset_from_bottom(&self) -> usize {
        self.offset_from_bottom
    }

    #[must_use]
    pub fn unseen_rows(&self) -> usize {
        self.unseen_rows
    }

    #[must_use]
    pub fn anchor_row_id(&self) -> Option<u64> {
        self.anchor_row_id
    }

    #[must_use]
    pub fn is_live(&self) -> bool {
        self.offset_from_bottom == 0
    }

    pub fn return_to_live(&mut self) {
        self.offset_from_bottom = 0;
        self.unseen_rows = 0;
        self.anchor_row_id = None;
        self.held_screen = None;
    }

    /// Whether this detached viewport holds its history and screen.
    #[must_use]
    pub fn is_holding(&self) -> bool {
        self.held_screen.is_some()
    }

    /// The screen a holding viewport shows in place of the live one.
    #[must_use]
    pub fn held_screen(&self) -> Option<&[TerminalRow]> {
        self.held_screen.as_deref()
    }

    /// Holds the history cache and `screen`, the screen this viewport was
    /// composed with, until the viewport returns to live. A live viewport
    /// never holds.
    pub fn hold(&mut self, screen: Vec<TerminalRow>) {
        if !self.is_live() && self.held_screen.is_none() {
            self.held_screen = Some(screen);
        }
    }

    /// Counts rows of new output that a held viewport did not take in.
    pub fn note_unseen(&mut self, rows: usize) {
        if !self.is_live() {
            self.unseen_rows = self.unseen_rows.saturating_add(rows);
        }
    }

    /// The screen rows this viewport composes below the history cache.
    fn screen<'a>(&'a self, snapshot: &'a TerminalSnapshot) -> &'a [TerminalRow] {
        self.held_screen
            .as_deref()
            .unwrap_or(snapshot.visible_rows.as_slice())
    }

    fn total_rows(&self, snapshot: &TerminalSnapshot) -> usize {
        snapshot
            .scrollback_rows
            .len()
            .saturating_add(self.screen(snapshot).len())
    }

    pub fn scroll_up(&mut self, lines: usize, snapshot: &TerminalSnapshot) {
        if snapshot.active_screen == ActiveScreen::Alternate {
            return;
        }
        self.offset_from_bottom = self
            .offset_from_bottom
            .saturating_add(lines)
            .min(snapshot.scrollback_rows.len());
        self.update_anchor(snapshot);
    }

    pub fn scroll_down(&mut self, lines: usize, snapshot: &TerminalSnapshot) {
        self.offset_from_bottom = self.offset_from_bottom.saturating_sub(lines);
        if self.offset_from_bottom == 0 {
            self.return_to_live();
        } else {
            self.update_anchor(snapshot);
        }
    }

    pub fn reveal_row(&mut self, row_id: u64, snapshot: &TerminalSnapshot) -> bool {
        let Some(index) = snapshot
            .scrollback_rows
            .iter()
            .position(|row| row.row_id == Some(row_id))
        else {
            return self
                .screen(snapshot)
                .iter()
                .any(|row| row.row_id == Some(row_id));
        };
        let total_rows = self.total_rows(snapshot);
        let viewport_height = snapshot.rows.min(total_rows);
        self.offset_from_bottom = total_rows
            .saturating_sub(index.saturating_add(viewport_height))
            .min(snapshot.scrollback_rows.len());
        if self.offset_from_bottom == 0 {
            self.return_to_live();
        } else {
            self.update_anchor(snapshot);
        }
        true
    }

    /// Preserves a detached viewport as history grows and clamps it when the
    /// bounded daemon snapshot trims or clears rows.
    pub fn observe_history_change(
        &mut self,
        previous_generation: u64,
        previous_rows: &[TerminalRow],
        snapshot: &TerminalSnapshot,
    ) {
        if snapshot.active_screen == ActiveScreen::Alternate
            || snapshot.history_generation != previous_generation
            || snapshot.available_scrollback_rows == 0
        {
            self.return_to_live();
            return;
        }
        if self.is_live() {
            return;
        }
        let Some(anchor_row_id) = self.anchor_row_id else {
            self.return_to_live();
            return;
        };
        let Some(anchor_index) = snapshot
            .scrollback_rows
            .iter()
            .position(|row| row.row_id == Some(anchor_row_id))
        else {
            self.return_to_live();
            return;
        };
        let previous_ids = previous_rows
            .iter()
            .filter_map(|row| row.row_id)
            .collect::<std::collections::BTreeSet<_>>();
        let appended = snapshot
            .scrollback_rows
            .iter()
            .filter(|row| row.row_id.is_some_and(|id| !previous_ids.contains(&id)))
            .count();
        let total_rows = self.total_rows(snapshot);
        let viewport_height = snapshot.rows.min(total_rows);
        self.offset_from_bottom = total_rows
            .saturating_sub(anchor_index.saturating_add(viewport_height))
            .min(snapshot.scrollback_rows.len());
        self.unseen_rows = self.unseen_rows.saturating_add(appended);
    }

    /// Keeps a detached viewport on the same rows after its history cache
    /// gained older rows above it or dropped newer ones below it, as when an
    /// older page joins a full cache. Without this the same offset from the
    /// bottom would show other rows.
    pub fn reanchor(&mut self, snapshot: &TerminalSnapshot) {
        if self.is_live() {
            return;
        }
        let anchor_index = self.anchor_row_id.and_then(|anchor_row_id| {
            snapshot
                .scrollback_rows
                .iter()
                .position(|row| row.row_id == Some(anchor_row_id))
        });
        let total_rows = self.total_rows(snapshot);
        let viewport_height = snapshot.rows.min(total_rows);
        self.offset_from_bottom = anchor_index
            .map_or(self.offset_from_bottom, |anchor_index| {
                total_rows.saturating_sub(anchor_index.saturating_add(viewport_height))
            })
            .min(snapshot.scrollback_rows.len());
        if anchor_index.is_none() {
            self.update_anchor(snapshot);
        }
    }

    fn update_anchor(&mut self, snapshot: &TerminalSnapshot) {
        if self.is_live() {
            self.anchor_row_id = None;
            return;
        }
        let total_rows = self.total_rows(snapshot);
        let viewport_height = snapshot.rows.min(total_rows);
        let end = total_rows.saturating_sub(self.offset_from_bottom);
        let start = end.saturating_sub(viewport_height);
        self.anchor_row_id = snapshot
            .scrollback_rows
            .get(start)
            .and_then(|row| row.row_id);
        if self.anchor_row_id.is_none() {
            self.return_to_live();
        }
    }

    #[must_use]
    pub fn visible_rows<'a>(&'a self, snapshot: &'a TerminalSnapshot) -> Vec<&'a TerminalRow> {
        if self.is_live() || snapshot.active_screen == ActiveScreen::Alternate {
            return snapshot.visible_rows.iter().collect();
        }
        let mut rows = Vec::with_capacity(self.total_rows(snapshot));
        rows.extend(snapshot.scrollback_rows.iter());
        rows.extend(self.screen(snapshot).iter());
        let viewport_height = snapshot.rows.min(rows.len());
        let end = rows.len().saturating_sub(self.offset_from_bottom);
        let start = end.saturating_sub(viewport_height);
        rows[start..end].to_vec()
    }
}

#[cfg(test)]
mod tests {
    use splinterm_core::SplintId;
    use splinterm_protocol::{TerminalInputModes, TerminalSnapshot};

    use super::*;

    fn snapshot(history: &[&str], visible: &[&str]) -> TerminalSnapshot {
        let visible_row = |text: &&str| TerminalRow {
            row_id: None,
            linebreak: text.starts_with('h'),
            cells: Vec::new(),
        };
        let history_row = |(index, text): (usize, &&str)| TerminalRow {
            row_id: Some(u64::try_from(index + 1).unwrap()),
            linebreak: text.starts_with('h'),
            cells: Vec::new(),
        };
        TerminalSnapshot {
            splint_id: SplintId::new(),
            incarnation: 1,
            revision: 1,
            columns: 1,
            rows: visible.len(),
            cursor_column: 0,
            cursor_row: 0,
            cursor_deferred_wrap: false,
            active_screen: ActiveScreen::Normal,
            input_modes: TerminalInputModes {
                application_cursor: false,
                application_keypad: false,
                focus_reporting: false,
                bracketed_paste: false,
                cursor_visible: true,
                cursor_blink: false,
                mouse_tracking: splinterm_protocol::MouseTracking::None,
                sgr_mouse: false,
            },
            palette: vec![0; 256],
            default_colors: [0; 3],
            title: String::new(),
            visible_rows: visible.iter().map(visible_row).collect(),
            history_generation: 1,
            oldest_available_scrollback_row_id: (!history.is_empty()).then_some(1),
            newest_available_scrollback_row_id: (!history.is_empty())
                .then(|| u64::try_from(history.len()).unwrap()),
            scrollback_rows: history.iter().enumerate().map(history_row).collect(),
            available_scrollback_rows: history.len(),
            omitted_oldest_scrollback_rows: 0,
            images: None,
            exited_code: None,
            exited_signal: None,
        }
    }

    #[test]
    fn search_reveal_positions_a_stable_history_row() {
        let state = snapshot(&["h1", "h2", "h3", "h4"], &["v1", "v2"]);
        let mut viewport = ScrollbackViewport::default();
        assert!(viewport.reveal_row(2, &state));
        assert!(!viewport.is_live());
        assert!(
            viewport
                .visible_rows(&state)
                .iter()
                .any(|row| row.row_id == Some(2))
        );
        assert!(!viewport.reveal_row(99, &state));
    }

    #[test]
    fn reanchoring_keeps_the_top_row_when_a_page_joins_a_full_cache() {
        let initial = snapshot(&["h1", "h2", "h3", "h4", "h5", "h6"], &["v1", "v2"]);
        let mut viewport = ScrollbackViewport::default();
        viewport.scroll_up(4, &initial);
        let top = |viewport: &ScrollbackViewport, state: &TerminalSnapshot| {
            viewport.visible_rows(state)[0].row_id
        };
        assert_eq!(top(&viewport, &initial), Some(3));

        // An older page joins above and the bounded cache drops the two
        // newest rows below the view.
        let mut joined = initial.clone();
        let mut older = initial.scrollback_rows[..1].to_vec();
        older[0].row_id = Some(0);
        joined.scrollback_rows = [older, initial.scrollback_rows[..4].to_vec()].concat();
        viewport.reanchor(&joined);
        assert_eq!(top(&viewport, &joined), Some(3));
        assert_eq!(viewport.offset_from_bottom(), 2);
        assert_eq!(viewport.unseen_rows(), 0);
    }

    #[test]
    fn scrolling_clamps_and_returns_to_live() {
        let snapshot = snapshot(&["h1", "h2", "h3"], &["v1", "v2"]);
        let mut viewport = ScrollbackViewport::default();
        viewport.scroll_up(20, &snapshot);
        assert_eq!(viewport.offset_from_bottom(), 3);
        viewport.scroll_down(2, &snapshot);
        assert_eq!(viewport.offset_from_bottom(), 1);
        viewport.scroll_down(1, &snapshot);
        assert!(viewport.is_live());
    }

    #[test]
    fn visible_rows_compose_history_before_live_grid() {
        let snapshot = snapshot(&["h1", "h2"], &["v1", "v2"]);
        let mut viewport = ScrollbackViewport::default();
        viewport.scroll_up(1, &snapshot);
        let rows = viewport.visible_rows(&snapshot);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].linebreak, "oldest displayed row comes from history");
        assert!(
            !rows[1].linebreak,
            "newest displayed row comes from live grid"
        );
    }

    #[test]
    fn detached_viewport_tracks_new_output_and_unseen_rows() {
        let initial = snapshot(&["h1", "h2"], &["v1", "v2"]);
        let mut viewport = ScrollbackViewport::default();
        viewport.scroll_up(1, &initial);
        assert_eq!(viewport.anchor_row_id(), Some(2));
        let next = snapshot(&["h1", "h2", "v1"], &["v2", "v3"]);
        viewport.observe_history_change(
            initial.history_generation,
            &initial.scrollback_rows,
            &next,
        );
        assert_eq!(viewport.offset_from_bottom(), 2);
        assert_eq!(viewport.unseen_rows(), 1);
    }

    #[test]
    fn detached_viewport_tracks_ring_rollover_at_constant_capacity() {
        let mut initial = snapshot(&["h1", "h2"], &["v1"]);
        initial.scrollback_rows[1].linebreak = false;
        let mut viewport = ScrollbackViewport::default();
        viewport.scroll_up(1, &initial);
        let mut next = snapshot(&["h2", "h3"], &["v2"]);
        next.scrollback_rows[0].row_id = Some(2);
        next.scrollback_rows[1].row_id = Some(3);
        next.oldest_available_scrollback_row_id = Some(2);
        next.newest_available_scrollback_row_id = Some(3);
        next.scrollback_rows[0].linebreak = false;
        next.scrollback_rows[1].linebreak = true;
        viewport.observe_history_change(
            initial.history_generation,
            &initial.scrollback_rows,
            &next,
        );
        assert_eq!(viewport.offset_from_bottom(), 2);
        assert_eq!(viewport.unseen_rows(), 1);
    }

    #[test]
    fn alternate_screen_and_clear_history_return_live() {
        let initial = snapshot(&["h1"], &["v1"]);
        let mut viewport = ScrollbackViewport::default();
        viewport.scroll_up(1, &initial);
        let mut alternate = initial.clone();
        alternate.active_screen = ActiveScreen::Alternate;
        viewport.observe_history_change(
            initial.history_generation,
            &initial.scrollback_rows,
            &alternate,
        );
        assert!(viewport.is_live());

        viewport.scroll_up(1, &initial);
        let mut cleared = snapshot(&[], &["v1"]);
        cleared.history_generation += 1;
        viewport.observe_history_change(
            initial.history_generation,
            &initial.scrollback_rows,
            &cleared,
        );
        assert!(viewport.is_live());
    }

    #[test]
    fn exact_anchor_trim_returns_live_even_when_other_rows_overlap() {
        let initial = snapshot(&["h1", "h2", "h3"], &["v1", "v2"]);
        let mut viewport = ScrollbackViewport::default();
        viewport.scroll_up(1, &initial);
        assert_eq!(viewport.anchor_row_id(), Some(3));

        let mut next = snapshot(&["h2", "h4"], &["v2", "v3"]);
        next.scrollback_rows[0].row_id = Some(2);
        next.scrollback_rows[1].row_id = Some(4);
        next.oldest_available_scrollback_row_id = Some(2);
        next.newest_available_scrollback_row_id = Some(4);
        viewport.observe_history_change(
            initial.history_generation,
            &initial.scrollback_rows,
            &next,
        );
        assert!(viewport.is_live());
        assert_eq!(viewport.anchor_row_id(), None);
    }

    #[test]
    fn duplicate_content_anchors_by_id_and_missing_anchor_returns_live() {
        let initial = snapshot(&["h", "h"], &["v"]);
        let mut viewport = ScrollbackViewport::default();
        viewport.scroll_up(1, &initial);
        let mut next = snapshot(&["h", "h"], &["v"]);
        next.scrollback_rows[0].row_id = Some(2);
        next.scrollback_rows[1].row_id = Some(3);
        next.oldest_available_scrollback_row_id = Some(2);
        next.newest_available_scrollback_row_id = Some(3);
        viewport.observe_history_change(
            initial.history_generation,
            &initial.scrollback_rows,
            &next,
        );
        assert_eq!(viewport.offset_from_bottom(), 2);
        assert_eq!(viewport.unseen_rows(), 1);

        let mut lost = snapshot(&["h", "h"], &["v"]);
        lost.scrollback_rows[0].row_id = Some(8);
        lost.scrollback_rows[1].row_id = Some(9);
        lost.oldest_available_scrollback_row_id = Some(8);
        lost.newest_available_scrollback_row_id = Some(9);
        viewport.observe_history_change(next.history_generation, &next.scrollback_rows, &lost);
        assert!(viewport.is_live());
    }
}
