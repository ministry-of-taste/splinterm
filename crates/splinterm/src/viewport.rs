//! Client-owned bounded scrollback viewport state.

use splinterm_protocol::{ActiveScreen, CellAttributes, TerminalRow, TerminalSnapshot};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScrollbackViewport {
    /// Composed rows (history, then screen) below the view's bottom row.
    offset_from_bottom: usize,
    unseen_rows: usize,
    anchor_row_id: Option<u64>,
    /// The screen as it was when this detached viewport began holding its
    /// history. While held, the viewport composes the cached history with this
    /// screen instead of the live one, so new output that cannot be joined to
    /// the cache never moves or clears what the user is reading.
    held_screen: Option<Vec<TerminalRow>>,
    /// The rows the pane's area shows, once known. A grid taller than this,
    /// as when another Window sets the size, shows the bottom of its content
    /// while live; the rows above are reached by scrolling, like history.
    local_rows: Option<usize>,
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

    /// Records the rows the pane's area shows. A detached view keeps its top
    /// row. Returns whether the view now shows other rows of `snapshot`.
    pub fn set_local_rows(
        &mut self,
        rows: Option<usize>,
        snapshot: Option<&TerminalSnapshot>,
    ) -> bool {
        let rows = rows.filter(|rows| *rows > 0);
        if self.local_rows == rows {
            return false;
        }
        let Some(snapshot) = snapshot else {
            self.local_rows = rows;
            return false;
        };
        let before = (self.rows_shown(snapshot), self.offset_from_bottom);
        self.local_rows = rows;
        self.reanchor(snapshot);
        before != (self.rows_shown(snapshot), self.offset_from_bottom)
    }

    /// How many rows this viewport shows: the pane's own rows when known
    /// and fewer than the grid's, else the grid's.
    #[must_use]
    pub fn rows_shown(&self, snapshot: &TerminalSnapshot) -> usize {
        self.local_rows
            .map_or(snapshot.rows, |rows| rows.min(snapshot.rows))
    }

    /// Whether the grid is taller than the pane, which then shows part of it.
    fn crops(&self, snapshot: &TerminalSnapshot) -> bool {
        self.local_rows.is_some_and(|rows| rows < snapshot.rows)
    }

    /// The grid rows this viewport composes below history. A grid the pane
    /// fits is composed whole. A taller one is composed down to its content's
    /// last row, the cursor's or the last with anything on it, so a live view
    /// shows the content's bottom and leaves out only blank rows below it;
    /// never fewer rows than the pane shows.
    fn screen_rows(&self, snapshot: &TerminalSnapshot) -> usize {
        let screen = self.screen(snapshot);
        if !self.crops(snapshot) {
            return screen.len();
        }
        // A held screen is an older one; the live cursor is not on it.
        let cursor = (!self.is_holding() && snapshot.input_modes.cursor_visible)
            .then(|| usize::try_from(snapshot.cursor_row).ok())
            .flatten()
            .map(|row| row.saturating_add(1));
        let content = screen
            .iter()
            .rposition(|row| !row_is_blank(row, snapshot))
            .map(|row| row.saturating_add(1));
        cursor
            .max(content)
            .unwrap_or(0)
            .max(self.rows_shown(snapshot))
            .min(screen.len())
    }

    /// The composed screen rows: the screen down to its content's bottom.
    fn composed_screen<'a>(&'a self, snapshot: &'a TerminalSnapshot) -> &'a [TerminalRow] {
        &self.screen(snapshot)[..self.screen_rows(snapshot)]
    }

    /// The content rows above the pane's that a live view leaves out: none
    /// unless the grid is taller than the pane and its content is too.
    #[must_use]
    pub fn hidden_screen_rows(&self, snapshot: &TerminalSnapshot) -> usize {
        if self.crops(snapshot) {
            self.screen_rows(snapshot)
                .saturating_sub(self.rows_shown(snapshot))
        } else {
            0
        }
    }

    /// Whether this viewport shows `snapshot`'s screen exactly as it is.
    #[must_use]
    pub fn shows_screen_unchanged(&self, snapshot: &TerminalSnapshot) -> bool {
        self.is_live() && !self.crops(snapshot)
    }

    /// The furthest this viewport scrolls up: past the grid's hidden top
    /// rows and every loaded history row.
    #[must_use]
    pub fn max_offset(&self, snapshot: &TerminalSnapshot) -> usize {
        snapshot
            .scrollback_rows
            .len()
            .saturating_add(self.hidden_screen_rows(snapshot))
    }

    /// The grid row a shown row is on, for reports to the application:
    /// shown rows move down by the grid's hidden top rows the view leaves
    /// out below its bottom. A view further back reports shown rows as is.
    #[must_use]
    pub fn screen_row_for_shown(&self, row: usize, snapshot: &TerminalSnapshot) -> usize {
        row.saturating_add(
            self.hidden_screen_rows(snapshot)
                .saturating_sub(self.offset_from_bottom),
        )
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
            .saturating_add(self.screen_rows(snapshot))
    }

    /// Where `row_id` is among the composed rows: history, then screen.
    fn composed_position(&self, snapshot: &TerminalSnapshot, row_id: u64) -> Option<usize> {
        snapshot
            .scrollback_rows
            .iter()
            .chain(self.composed_screen(snapshot))
            .position(|row| row.row_id == Some(row_id))
    }

    /// The offset that shows composed row `index` at the top of the view.
    fn offset_with_top(&self, snapshot: &TerminalSnapshot, index: usize) -> usize {
        let total_rows = self.total_rows(snapshot);
        let viewport_height = self.rows_shown(snapshot).min(total_rows);
        total_rows
            .saturating_sub(index.saturating_add(viewport_height))
            .min(self.max_offset(snapshot))
    }

    /// The composed rows this view shows, as a start and end index.
    fn shown_range(&self, snapshot: &TerminalSnapshot) -> (usize, usize) {
        let total_rows = self.total_rows(snapshot);
        let viewport_height = self.rows_shown(snapshot).min(total_rows);
        let end = total_rows.saturating_sub(self.offset_from_bottom);
        (end.saturating_sub(viewport_height), end)
    }

    pub fn scroll_up(&mut self, lines: usize, snapshot: &TerminalSnapshot) {
        if snapshot.active_screen == ActiveScreen::Alternate {
            return;
        }
        self.offset_from_bottom = self
            .offset_from_bottom
            .saturating_add(lines)
            .min(self.max_offset(snapshot));
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
        let history_rows = snapshot.scrollback_rows.len();
        let offset = if let Some(index) = snapshot
            .scrollback_rows
            .iter()
            .position(|row| row.row_id == Some(row_id))
        {
            self.offset_with_top(snapshot, index)
        } else {
            let Some(index) = self
                .screen(snapshot)
                .iter()
                .position(|row| row.row_id == Some(row_id))
                .map(|row| history_rows.saturating_add(row))
            else {
                return false;
            };
            let (start, end) = self.shown_range(snapshot);
            // A screen row is in view unless a grid taller than the pane
            // keeps it out; then the view moves just far enough to show it.
            if !self.crops(snapshot) || (start..end).contains(&index) {
                return true;
            }
            if index < start {
                self.offset_with_top(snapshot, index)
            } else {
                self.total_rows(snapshot)
                    .saturating_sub(index.saturating_add(1))
            }
        };
        self.offset_from_bottom = offset;
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
        // A view anchored on the grid's hidden top rows needs no history.
        let anchored_on_screen = self.anchor_row_id.is_some_and(|anchor_row_id| {
            self.screen(snapshot)
                .iter()
                .any(|row| row.row_id == Some(anchor_row_id))
        });
        if snapshot.active_screen == ActiveScreen::Alternate
            || snapshot.history_generation != previous_generation
            || (snapshot.available_scrollback_rows == 0 && !anchored_on_screen)
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
        let Some(anchor_index) = self.composed_position(snapshot, anchor_row_id) else {
            // A history anchor that left the cache returns live. A screen
            // row that left the grid in place, as a scroll region does,
            // keeps the view where it is.
            if previous_rows
                .iter()
                .any(|row| row.row_id == Some(anchor_row_id))
            {
                self.return_to_live();
            } else {
                self.offset_from_bottom = self.offset_from_bottom.min(self.max_offset(snapshot));
                if self.offset_from_bottom == 0 {
                    self.return_to_live();
                } else {
                    self.update_anchor(snapshot);
                }
            }
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
        self.offset_from_bottom = self.offset_with_top(snapshot, anchor_index);
        self.unseen_rows = self.unseen_rows.saturating_add(appended);
        if self.offset_from_bottom == 0 {
            // Output alone never returns a detached view to live, as when a
            // reverse scroll moves its top row down to the live view's.
            if self.max_offset(snapshot) == 0 {
                self.return_to_live();
            } else {
                self.offset_from_bottom = 1;
                self.update_anchor(snapshot);
            }
        }
    }

    /// Keeps a detached viewport on the same rows after its history cache
    /// gained older rows above it or dropped newer ones below it, as when an
    /// older page joins a full cache, or after the pane's rows changed.
    /// Without this the same offset from the bottom would show other rows.
    pub fn reanchor(&mut self, snapshot: &TerminalSnapshot) {
        if self.is_live() {
            return;
        }
        let anchor_index = self
            .anchor_row_id
            .and_then(|anchor_row_id| self.composed_position(snapshot, anchor_row_id));
        self.offset_from_bottom = anchor_index
            .map_or(self.offset_from_bottom, |anchor_index| {
                self.offset_with_top(snapshot, anchor_index)
            })
            .min(self.max_offset(snapshot));
        if self.offset_from_bottom == 0 {
            self.return_to_live();
        } else if anchor_index.is_none() {
            self.update_anchor(snapshot);
        }
    }

    fn update_anchor(&mut self, snapshot: &TerminalSnapshot) {
        if self.is_live() {
            self.anchor_row_id = None;
            return;
        }
        let (start, _) = self.shown_range(snapshot);
        let history = &snapshot.scrollback_rows;
        self.anchor_row_id = history
            .get(start)
            .or_else(|| {
                start
                    .checked_sub(history.len())
                    .and_then(|row| self.composed_screen(snapshot).get(row))
            })
            .and_then(|row| row.row_id);
        if self.anchor_row_id.is_none() {
            self.return_to_live();
        }
    }

    #[must_use]
    pub fn visible_rows<'a>(&'a self, snapshot: &'a TerminalSnapshot) -> Vec<&'a TerminalRow> {
        if self.is_live() || snapshot.active_screen == ActiveScreen::Alternate {
            let rows = &snapshot.visible_rows;
            let rows = &rows[..self.screen_rows(snapshot).min(rows.len())];
            let start = rows.len().saturating_sub(self.rows_shown(snapshot));
            return rows[start..].iter().collect();
        }
        let mut rows = Vec::with_capacity(self.total_rows(snapshot));
        rows.extend(snapshot.scrollback_rows.iter());
        rows.extend(self.composed_screen(snapshot).iter());
        let (start, end) = self.shown_range(snapshot);
        rows[start..end].to_vec()
    }
}

/// Whether `row` shows nothing: no text, default attributes, no image.
fn row_is_blank(row: &TerminalRow, snapshot: &TerminalSnapshot) -> bool {
    row.cells.iter().all(|cell| {
        cell.content.chars().all(|character| character == ' ')
            && cell.attributes == CellAttributes::default()
    }) && !snapshot.images.as_ref().is_some_and(|plane| {
        plane
            .placements
            .iter()
            .any(|placement| row.row_id == Some(placement.row_id))
    })
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

    /// `history` history rows over a `rows`-row grid, with row ids counting
    /// up from 1 through both, and the cursor on the grid's last row.
    fn grid(history: usize, rows: usize) -> TerminalSnapshot {
        let mut state = snapshot(&vec!["h"; history], &vec!["v"; rows]);
        for (index, row) in state.visible_rows.iter_mut().enumerate() {
            row.row_id = Some(u64::try_from(history + index + 1).unwrap());
        }
        state.cursor_row = i32::try_from(rows - 1).unwrap();
        state
    }

    fn shown(viewport: &ScrollbackViewport, state: &TerminalSnapshot) -> Vec<u64> {
        viewport
            .visible_rows(state)
            .iter()
            .filter_map(|row| row.row_id)
            .collect()
    }

    #[test]
    fn a_live_view_shorter_than_the_grid_shows_the_grids_bottom_rows() {
        let state = grid(3, 6);
        let mut viewport = ScrollbackViewport::default();
        // Until the pane's rows are known, the whole grid shows as today.
        assert!(viewport.shows_screen_unchanged(&state));
        assert_eq!(shown(&viewport, &state), [4, 5, 6, 7, 8, 9]);

        assert!(viewport.set_local_rows(Some(4), Some(&state)));
        assert!(viewport.is_live());
        assert!(!viewport.shows_screen_unchanged(&state));
        assert_eq!(viewport.rows_shown(&state), 4);
        assert_eq!(viewport.hidden_screen_rows(&state), 2);
        assert_eq!(shown(&viewport, &state), [6, 7, 8, 9]);
        assert_eq!(viewport.max_offset(&state), 5);
        // The application's grid rows sit two below the rows shown.
        assert_eq!(viewport.screen_row_for_shown(0, &state), 2);
        assert_eq!(viewport.screen_row_for_shown(3, &state), 5);

        // A pane as tall as the grid, or taller, shows it unchanged.
        assert!(viewport.set_local_rows(Some(6), Some(&state)));
        assert!(viewport.shows_screen_unchanged(&state));
        assert_eq!(shown(&viewport, &state), [4, 5, 6, 7, 8, 9]);
        assert!(!viewport.set_local_rows(Some(8), Some(&state)));
        assert!(viewport.shows_screen_unchanged(&state));
        assert_eq!(viewport.screen_row_for_shown(0, &state), 0);
    }

    #[test]
    fn scrolling_from_a_cropped_live_view_moves_one_row_at_a_time() {
        let state = grid(3, 6);
        let mut viewport = ScrollbackViewport::default();
        viewport.set_local_rows(Some(4), Some(&state));
        viewport.scroll_up(1, &state);
        assert_eq!(viewport.offset_from_bottom(), 1);
        assert_eq!(shown(&viewport, &state), [5, 6, 7, 8]);
        assert_eq!(viewport.anchor_row_id(), Some(5));
        assert_eq!(viewport.screen_row_for_shown(0, &state), 1);
        viewport.scroll_up(1, &state);
        assert_eq!(shown(&viewport, &state), [4, 5, 6, 7]);
        viewport.scroll_up(1, &state);
        assert_eq!(shown(&viewport, &state), [3, 4, 5, 6], "into history");
        viewport.scroll_up(usize::MAX, &state);
        assert_eq!(viewport.offset_from_bottom(), 5);
        assert_eq!(shown(&viewport, &state), [1, 2, 3, 4]);

        viewport.scroll_down(4, &state);
        assert_eq!(shown(&viewport, &state), [5, 6, 7, 8]);
        viewport.scroll_down(1, &state);
        assert!(viewport.is_live());
        assert_eq!(shown(&viewport, &state), [6, 7, 8, 9]);

        // Return to live from anywhere lands on the grid's bottom rows.
        viewport.scroll_up(4, &state);
        viewport.scroll_down(usize::MAX, &state);
        assert!(viewport.is_live());
        assert_eq!(viewport.anchor_row_id(), None);
        assert_eq!(shown(&viewport, &state), [6, 7, 8, 9]);
    }

    #[test]
    fn output_leaves_a_view_scrolled_into_the_grids_hidden_rows_in_place() {
        let state = grid(3, 6);
        let mut viewport = ScrollbackViewport::default();
        viewport.set_local_rows(Some(4), Some(&state));
        viewport.scroll_up(1, &state);

        // Output that rewrites rows in place.
        let mut rewritten = state.clone();
        rewritten.revision += 1;
        rewritten.visible_rows[5].linebreak = true;
        viewport.observe_history_change(1, &state.scrollback_rows, &rewritten);
        assert_eq!(viewport.offset_from_bottom(), 1);
        assert_eq!(shown(&viewport, &rewritten), [5, 6, 7, 8]);

        // A new line scrolls the grid's top row into history.
        let scrolled = grid(4, 6);
        viewport.observe_history_change(1, &rewritten.scrollback_rows, &scrolled);
        assert!(!viewport.is_live());
        assert_eq!(viewport.offset_from_bottom(), 2);
        assert_eq!(viewport.unseen_rows(), 1);
        assert_eq!(shown(&viewport, &scrolled), [5, 6, 7, 8]);

        // A grid with no history at all keeps the view too.
        let fresh = grid(0, 6);
        let mut viewport = ScrollbackViewport::default();
        viewport.set_local_rows(Some(4), Some(&fresh));
        viewport.scroll_up(1, &fresh);
        let mut next = fresh.clone();
        next.revision += 1;
        viewport.observe_history_change(1, &fresh.scrollback_rows, &next);
        assert_eq!(shown(&viewport, &next), [2, 3, 4, 5]);
    }

    #[test]
    fn a_cropped_view_keeps_its_top_row_and_reveals_hidden_grid_rows() {
        let state = grid(3, 6);
        let mut viewport = ScrollbackViewport::default();
        viewport.set_local_rows(Some(4), Some(&state));
        viewport.scroll_up(3, &state);
        assert_eq!(shown(&viewport, &state), [3, 4, 5, 6]);
        // A taller pane keeps the view's top row.
        assert!(viewport.set_local_rows(Some(5), Some(&state)));
        assert_eq!(shown(&viewport, &state), [3, 4, 5, 6, 7]);

        let mut viewport = ScrollbackViewport::default();
        viewport.set_local_rows(Some(4), Some(&state));
        // A shown row needs no move; a hidden grid row comes into view.
        assert!(viewport.reveal_row(8, &state));
        assert!(viewport.is_live());
        assert!(viewport.reveal_row(4, &state));
        assert_eq!(shown(&viewport, &state), [4, 5, 6, 7]);
        assert!(viewport.reveal_row(9, &state));
        assert!(viewport.is_live());
        assert_eq!(shown(&viewport, &state), [6, 7, 8, 9]);
    }

    #[test]
    fn a_pane_as_tall_as_its_grid_scrolls_exactly_as_before() {
        let state = grid(3, 6);
        let mut unknown = ScrollbackViewport::default();
        let mut controller = ScrollbackViewport::default();
        assert!(!controller.set_local_rows(Some(6), Some(&state)));
        for step in [1, 1, 5, 0] {
            unknown.scroll_up(step, &state);
            controller.scroll_up(step, &state);
            assert_eq!(
                controller.offset_from_bottom(),
                unknown.offset_from_bottom()
            );
            assert_eq!(shown(&controller, &state), shown(&unknown, &state));
        }
        assert_eq!(controller.offset_from_bottom(), 3);
        assert!(controller.reveal_row(8, &state));
        assert_eq!(
            controller.offset_from_bottom(),
            3,
            "a screen row never moves it"
        );
        controller.scroll_down(usize::MAX, &state);
        assert!(controller.is_live());
        assert!(controller.shows_screen_unchanged(&state));
    }

    /// Writes text on grid row `row`, so the row has content.
    fn write(state: &mut TerminalSnapshot, row: usize) {
        state.visible_rows[row].cells = vec![splinterm_protocol::TerminalCell {
            content: "$".to_owned(),
            spacer_remaining: None,
            attributes: CellAttributes::default(),
        }];
    }

    #[test]
    fn a_cropped_live_view_shows_the_bottom_of_the_content_not_blank_rows() {
        // A fresh shell, or one just cleared: the prompt on the top row.
        let mut state = grid(3, 6);
        state.cursor_row = 0;
        write(&mut state, 0);
        let mut viewport = ScrollbackViewport::default();
        viewport.set_local_rows(Some(4), Some(&state));
        assert_eq!(shown(&viewport, &state), [4, 5, 6, 7]);
        assert_eq!(viewport.hidden_screen_rows(&state), 0);
        assert_eq!(viewport.max_offset(&state), 3, "history only");
        assert_eq!(viewport.screen_row_for_shown(0, &state), 0);

        // Content grows past the pane: the view follows the cursor.
        state.cursor_row = 4;
        assert_eq!(shown(&viewport, &state), [5, 6, 7, 8]);
        assert_eq!(viewport.hidden_screen_rows(&state), 1);
        assert_eq!(viewport.screen_row_for_shown(3, &state), 4);
        // A hidden cursor counts only the rows with content.
        state.input_modes.cursor_visible = false;
        write(&mut state, 5);
        assert_eq!(shown(&viewport, &state), [6, 7, 8, 9]);
        state.visible_rows[5].cells.clear();
        assert_eq!(shown(&viewport, &state), [4, 5, 6, 7]);
    }

    #[test]
    fn a_view_scrolled_back_stays_put_while_the_content_grows() {
        let mut state = grid(3, 6);
        state.cursor_row = 4;
        let mut viewport = ScrollbackViewport::default();
        viewport.set_local_rows(Some(4), Some(&state));
        assert_eq!(shown(&viewport, &state), [5, 6, 7, 8]);
        viewport.scroll_up(2, &state);
        let before = shown(&viewport, &state);
        assert_eq!(before, [3, 4, 5, 6]);

        let mut grown = state.clone();
        grown.revision += 1;
        grown.cursor_row = 5;
        viewport.observe_history_change(1, &state.scrollback_rows, &grown);
        assert!(!viewport.is_live());
        assert_eq!(viewport.offset_from_bottom(), 3);
        assert_eq!(shown(&viewport, &grown), before);
        viewport.scroll_down(usize::MAX, &grown);
        assert_eq!(shown(&viewport, &grown), [6, 7, 8, 9]);
    }

    #[test]
    fn a_reverse_scroll_never_returns_a_detached_view_to_live() {
        let state = grid(3, 6);
        let mut viewport = ScrollbackViewport::default();
        viewport.set_local_rows(Some(4), Some(&state));
        viewport.scroll_up(1, &state);
        assert_eq!(shown(&viewport, &state), [5, 6, 7, 8]);

        // Reverse index at the top: a new top row, the rest one lower.
        let mut reversed = state.clone();
        reversed.revision += 1;
        reversed.visible_rows.rotate_right(1);
        reversed.visible_rows[0].row_id = Some(10);
        viewport.observe_history_change(1, &state.scrollback_rows, &reversed);
        assert!(!viewport.is_live(), "output never sends the view live");
        assert_eq!(viewport.offset_from_bottom(), 1);
        assert_eq!(shown(&viewport, &reversed), [4, 5, 6, 7]);

        // The viewer's own Window growing to the live bottom still is live.
        viewport.set_local_rows(Some(5), Some(&reversed));
        assert!(viewport.is_live());
    }
}
