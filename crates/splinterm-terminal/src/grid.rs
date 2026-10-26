//! Circular row indexing and lazy allocation.
//!
//! Derived from Foot 1.27.0 `terminal.h`, `grid.h`, and `grid.c` at commit
//! `3c5b584b0eafa772eb4376fb6eaf6643399e190e`, specifically `grid.rows`,
//! `grid.offset`, `grid.view`, row-coordinate conversion,
//! `grid_row_absolute`, and `_grid_row_maybe_alloc`.
//!
//! This module contains circular storage, coordinate spaces, scrolling,
//! cursor-preserving resize, and logical-line reflow. Renderer damage and
//! higher terminal-feature coordination remain deferred.

mod reflow;

use std::{collections::HashSet, fmt, marker::PhantomData};

use serde::{
    Deserialize, Deserializer, Serialize,
    de::{SeqAccess, Visitor},
};

use crate::{Color, Coordinate, Cursor, Row, ScrollRegion, row::MAX_CHECKPOINT_COLUMNS};

/// Covers the protocol maximum of 1,000,000 scrollback lines plus the screen,
/// rounded up to the power-of-two ring capacity.
pub(crate) const MAX_CHECKPOINT_GRID_ROWS: usize = 1 << 20;
/// Restored monotonic counters must leave room to keep counting; an exhausted
/// counter would panic on the next mutation.
pub(crate) const CHECKPOINT_COUNTER_HEADROOM: u64 = u64::MAX / 2;

/// Direction of a grid scroll operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScrollDirection {
    /// Content moves toward the top; new rows appear at the bottom.
    Forward,
    /// Content moves toward the bottom; new rows appear at the top.
    Reverse,
}

/// State transition produced by a grid scroll.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScrollResult {
    /// Direction of content movement.
    pub direction: ScrollDirection,
    /// Affected half-open screen region.
    pub region: ScrollRegion,
    /// Number of rows moved.
    pub rows: usize,
    /// Whether the viewport followed the live-screen offset before scrolling.
    pub view_was_following: bool,
    /// Whether viewport position changed.
    pub view_changed: bool,
}

pub(crate) type ScrollbackRows<'a> = (Vec<(u64, &'a Row)>, usize, usize, Option<u64>, Option<u64>);

/// Power-of-two circular row storage for terminal screen and scrollback rows.
///
/// The grid owns row storage, cursor and viewport coordinates, scrolling, and
/// resize/reflow behavior. It remains independent of renderer damage, PTYs,
/// protocols, and higher terminal features.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Grid {
    #[serde(deserialize_with = "deserialize_rows")]
    rows: Vec<Option<Row>>,
    columns: usize,
    offset: usize,
    view: usize,
    screen_rows: usize,
    cursor: Cursor,
    saved_cursor: Cursor,
    generation: u64,
    #[serde(deserialize_with = "deserialize_row_ids")]
    row_ids: Vec<u64>,
    next_row_id: u64,
    history_generation: u64,
}

impl Grid {
    /// Creates empty row slots. Rows are allocated on first mutable use.
    ///
    /// # Panics
    ///
    /// Panics unless `row_capacity` is a non-zero power of two and `columns`
    /// is non-zero.
    #[must_use]
    pub fn new(row_capacity: usize, columns: usize) -> Self {
        assert!(
            row_capacity.is_power_of_two(),
            "grid row capacity must be a non-zero power of two"
        );
        assert!(columns > 0, "grid must have at least one column");
        assert!(
            columns <= usize::try_from(i32::MAX).expect("usize represents i32::MAX"),
            "grid columns must fit in Foot's signed coordinate space"
        );
        assert!(
            row_capacity <= (1_usize << 30),
            "grid row capacity exceeds Foot's signed coordinate limit"
        );

        Self {
            rows: vec![None; row_capacity],
            columns,
            offset: 0,
            view: 0,
            screen_rows: 1,
            cursor: Cursor::default(),
            saved_cursor: Cursor::default(),
            generation: 0,
            row_ids: vec![0; row_capacity],
            next_row_id: 1,
            history_generation: 1,
        }
    }

    /// Creates a grid with every visible row safely initialized.
    ///
    /// # Panics
    ///
    /// Panics under the same conditions as [`Self::new`], or when
    /// `screen_rows` is zero or exceeds `row_capacity`.
    #[must_use]
    pub fn with_screen_size(row_capacity: usize, columns: usize, screen_rows: usize) -> Self {
        let mut grid = Self::new(row_capacity, columns);
        grid.assert_screen_rows(screen_rows);
        grid.screen_rows = screen_rows;
        for row in 0..screen_rows {
            grid.row_or_allocate(i32::try_from(row).expect("screen rows fit in i32"));
        }
        grid
    }

    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) const fn history_generation(&self) -> u64 {
        self.history_generation
    }

    pub(crate) const fn history_namespace(&self) -> (u64, u64) {
        (self.next_row_id, self.history_generation)
    }

    pub(crate) fn checkpoint_is_valid(&self, composed_entries: usize) -> bool {
        let capacity = self.rows.len();
        if !capacity.is_power_of_two()
            || capacity > MAX_CHECKPOINT_GRID_ROWS
            || self.row_ids.len() != capacity
            || self.columns == 0
            || self.columns > MAX_CHECKPOINT_COLUMNS
            || self.screen_rows == 0
            || self.screen_rows > capacity
            || self.offset >= capacity
            || self.view >= capacity
            || self.history_generation == 0
            || self.next_row_id == 0
            || self.generation > CHECKPOINT_COUNTER_HEADROOM
            || self.history_generation > CHECKPOINT_COUNTER_HEADROOM
            || self.next_row_id > CHECKPOINT_COUNTER_HEADROOM
        {
            return false;
        }
        let cursor_is_valid = |cursor: Cursor| {
            let position = cursor.position();
            position.column >= 0
                && usize::try_from(position.column).is_ok_and(|column| column < self.columns)
                && position.row >= 0
                && usize::try_from(position.row).is_ok_and(|row| row < self.screen_rows)
        };
        if !cursor_is_valid(self.cursor) || !cursor_is_valid(self.saved_cursor) {
            return false;
        }
        let mut seen_ids = HashSet::new();
        let mut maximum_id = 0_u64;
        for (row, id) in self.rows.iter().zip(&self.row_ids) {
            match row {
                Some(row) => {
                    if *id == 0
                        || row.len() != self.columns
                        || !seen_ids.insert(*id)
                        || !row.checkpoint_is_valid(composed_entries)
                    {
                        return false;
                    }
                    maximum_id = maximum_id.max(*id);
                }
                None if *id == 0 => {}
                None => return false,
            }
        }
        if self.next_row_id <= maximum_id {
            return false;
        }
        let mask = capacity - 1;
        (0..self.screen_rows).all(|row| self.rows[(self.offset + row) & mask].is_some())
    }

    /// Cells held by allocated rows, the amount a checkpoint restore expands.
    pub(crate) fn allocated_cells(&self) -> usize {
        self.rows.iter().flatten().map(Row::len).sum()
    }

    pub(crate) fn continue_history_namespace(&mut self, namespace: (u64, u64)) {
        self.next_row_id = namespace.0;
        self.history_generation = namespace
            .1
            .checked_add(1)
            .expect("history generation exhausted");
        self.reidentify_allocated_rows_chronologically();
    }

    /// Returns the number of physical slots in the circular row array.
    #[must_use]
    pub fn row_capacity(&self) -> usize {
        self.rows.len()
    }

    /// Returns the cell width used to initialize newly allocated rows.
    #[must_use]
    pub fn columns(&self) -> usize {
        self.columns
    }

    /// Returns the visible screen height.
    #[must_use]
    pub fn screen_rows(&self) -> usize {
        self.screen_rows
    }

    /// Returns the active cursor.
    #[must_use]
    pub const fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// Replaces the active cursor after validating it against the screen.
    ///
    /// # Panics
    ///
    /// Panics if the cursor is outside the configured screen.
    pub fn set_cursor(&mut self, cursor: Cursor) {
        self.assert_cursor(cursor);
        if self.cursor != cursor {
            self.bump_generation();
            self.cursor = cursor;
        }
    }

    /// Returns the saved cursor.
    #[must_use]
    pub const fn saved_cursor(&self) -> Cursor {
        self.saved_cursor
    }

    /// Replaces the saved cursor after validating it against the screen.
    ///
    /// # Panics
    ///
    /// Panics if the cursor is outside the configured screen.
    pub fn set_saved_cursor(&mut self, cursor: Cursor) {
        self.assert_cursor(cursor);
        if self.saved_cursor != cursor {
            self.bump_generation();
            self.saved_cursor = cursor;
        }
    }

    /// Maps an offset-relative row to its physical circular slot.
    #[must_use]
    pub fn absolute_index(&self, relative_row: i32) -> usize {
        let mask = self.rows.len() - 1;
        self.offset.wrapping_add_signed(relative_row as isize) & mask
    }

    /// Returns the physical row at the top of the live screen.
    #[must_use]
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Returns the physical row at the top of the current viewport.
    #[must_use]
    pub fn view(&self) -> usize {
        self.view
    }

    /// Returns whether the viewport is following the live screen.
    #[must_use]
    pub fn view_follows_offset(&self) -> bool {
        self.view == self.offset
    }

    /// Returns an allocated row without changing storage.
    #[must_use]
    pub fn row(&self, relative_row: i32) -> Option<&Row> {
        self.rows[self.absolute_index(relative_row)].as_ref()
    }

    /// Returns the stable identity of an allocated offset-relative row.
    #[must_use]
    pub fn row_id(&self, relative_row: i32) -> Option<u64> {
        let index = self.absolute_index(relative_row);
        self.rows[index].as_ref().map(|_| self.row_ids[index])
    }

    pub(crate) fn ordered_retained_row_ids(&self) -> Vec<u64> {
        let start = self.scrollback_start(self.screen_rows);
        let mask = self.rows.len() - 1;
        (0..self.rows.len())
            .filter_map(|distance| {
                let index = start.wrapping_add(distance) & mask;
                self.rows[index].as_ref().map(|_| self.row_ids[index])
            })
            .collect()
    }

    /// Returns every stable row identity currently retained by the grid.
    pub(crate) fn retained_row_ids(&self) -> std::collections::HashSet<u64> {
        self.rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| row.as_ref().map(|_| self.row_ids[index]))
            .collect()
    }

    pub(crate) fn screen_row_ids(&self) -> Vec<u64> {
        (0..self.screen_rows)
            .map(|row| {
                self.row_id(Self::signed_row(row))
                    .expect("every live-screen row is allocated")
            })
            .collect()
    }

    pub(crate) fn newest_scrollback_row_ids(&self, maximum_rows: usize) -> Vec<u64> {
        self.snapshot_scrollback_rows(maximum_rows)
            .0
            .into_iter()
            .map(|(id, _)| id)
            .collect()
    }

    /// Returns an allocated mutable row without allocating it.
    pub fn row_mut(&mut self, relative_row: i32) -> Option<&mut Row> {
        let index = self.absolute_index(relative_row);
        if self.rows[index].is_some() {
            self.bump_generation();
        }
        self.rows[index].as_mut()
    }

    /// Returns a row, safely initializing its cells when allocating its slot.
    ///
    /// # Panics
    ///
    /// Panics if the monotonic grid generation or row identity is exhausted.
    pub fn row_or_allocate(&mut self, relative_row: i32) -> &mut Row {
        let index = self.absolute_index(relative_row);
        self.bump_generation();
        if self.rows[index].is_none() {
            self.rows[index] = Some(Row::new(self.columns));
            self.assign_new_row_id(index);
        }
        self.rows[index].as_mut().expect("row was allocated")
    }

    /// Maps a viewport-relative row to its physical circular slot.
    #[must_use]
    pub fn absolute_index_in_view(&self, relative_row: i32) -> usize {
        let mask = self.rows.len() - 1;
        self.view.wrapping_add_signed(relative_row as isize) & mask
    }

    /// Returns an allocated row relative to the current viewport.
    #[must_use]
    pub fn row_in_view(&self, relative_row: i32) -> Option<&Row> {
        self.rows[self.absolute_index_in_view(relative_row)].as_ref()
    }

    /// Returns an allocated mutable row relative to the current viewport.
    pub fn row_in_view_mut(&mut self, relative_row: i32) -> Option<&mut Row> {
        let index = self.absolute_index_in_view(relative_row);
        if self.rows[index].is_some() {
            self.bump_generation();
        }
        self.rows[index].as_mut()
    }

    /// Converts a physical row index to Foot's scrollback-relative space.
    ///
    /// Scrollback-relative row zero is the oldest circular slot, immediately
    /// after the live screen. Higher rows proceed toward the bottom of the
    /// current screen.
    ///
    /// # Panics
    ///
    /// Panics if `screen_rows` is zero or exceeds the grid capacity, or if
    /// `absolute_row` is not a physical row index in this grid.
    #[must_use]
    pub fn absolute_to_scrollback(&self, screen_rows: usize, absolute_row: usize) -> usize {
        self.assert_coordinate_inputs(screen_rows, absolute_row);
        let start = self.scrollback_start(screen_rows);
        absolute_row.wrapping_sub(start) & (self.rows.len() - 1)
    }

    /// Converts a Foot scrollback-relative row to a physical row index.
    ///
    /// # Panics
    ///
    /// Panics if `screen_rows` is zero or exceeds the grid capacity, or if
    /// `scrollback_row` is not a relative row index in this grid.
    #[must_use]
    pub fn scrollback_to_absolute(&self, screen_rows: usize, scrollback_row: usize) -> usize {
        self.assert_coordinate_inputs(screen_rows, scrollback_row);
        self.scrollback_start(screen_rows)
            .wrapping_add(scrollback_row)
            & (self.rows.len() - 1)
    }

    /// Finds the oldest initialized row, starting at Foot's theoretical
    /// scrollback beginning.
    ///
    /// Unlike Foot's internal helper, this bounded safe variant returns `None`
    /// when the grid has no initialized rows instead of relying on a populated
    /// visible screen.
    ///
    /// # Panics
    ///
    /// Panics if `screen_rows` is zero or exceeds the grid capacity.
    #[must_use]
    pub fn scrollback_start_ignoring_uninitialized(&self, screen_rows: usize) -> Option<usize> {
        self.assert_screen_rows(screen_rows);
        let start = self.scrollback_start(screen_rows);
        (0..self.rows.len())
            .map(|distance| start.wrapping_add(distance) & (self.rows.len() - 1))
            .find(|&index| self.rows[index].is_some())
    }

    /// Removes every allocated row outside the visible screen.
    pub fn clear_scrollback(&mut self) {
        let visible: Vec<usize> = (0..self.screen_rows)
            .map(|row| self.absolute_index(Self::signed_row(row)))
            .collect();
        let had_history = self
            .rows
            .iter()
            .enumerate()
            .any(|(index, row)| row.is_some() && !visible.contains(&index));
        if !had_history {
            return;
        }
        self.bump_generation();
        self.bump_history_generation();
        for (index, row) in self.rows.iter_mut().enumerate() {
            if !visible.contains(&index) {
                *row = None;
                self.row_ids[index] = 0;
            }
        }
        self.view = self.offset;
    }

    /// Clears every visible row and homes both cursors.
    pub fn reset_visible(&mut self, background: Color) {
        for row in 0..self.screen_rows {
            self.row_or_allocate(Self::signed_row(row))
                .erase_all(background);
        }
        self.cursor = Cursor::default();
        self.saved_cursor = Cursor::default();
        self.view = self.offset;
    }

    /// Resizes the visible grid while deliberately discarding scrollback and
    /// not reflowing logical lines.
    ///
    /// # Panics
    ///
    /// Panics if the new dimensions violate [`Self::new`] or
    /// [`Self::with_screen_size`] constraints.
    pub fn resize_without_reflow(
        &mut self,
        new_row_capacity: usize,
        new_columns: usize,
        new_screen_rows: usize,
    ) -> std::collections::BTreeMap<u64, u64> {
        let mut resized = Self::new(new_row_capacity, new_columns);
        resized.assert_screen_rows(new_screen_rows);
        resized.screen_rows = new_screen_rows;

        let copied_rows = self.screen_rows.min(new_screen_rows);
        let mut copied_row_ids = Vec::with_capacity(copied_rows);
        for row_number in 0..copied_rows {
            let old_index = self.absolute_index(Self::signed_row(row_number));
            copied_row_ids.push(self.row_ids[old_index]);
            let mut row = self.rows[old_index]
                .take()
                .unwrap_or_else(|| Row::new_dirty(self.columns));
            row.resize_without_reflow(new_columns);
            // Foot's no-reflow path allocates a new row whose default is a hard
            // linebreak rather than carrying soft-wrap metadata across resize.
            row.set_linebreak(true);
            resized.rows[row_number] = Some(row);
        }
        for row in copied_rows..new_screen_rows {
            resized.rows[row] = Some(Row::new_dirty(new_columns));
        }

        resized.cursor =
            Self::resized_cursor(self.cursor, self.screen_rows, new_columns, new_screen_rows);
        resized.saved_cursor = Self::resized_cursor(
            self.saved_cursor,
            self.screen_rows,
            new_columns,
            new_screen_rows,
        );
        resized.generation = self
            .generation
            .checked_add(1)
            .expect("grid generation exhausted");
        resized.next_row_id = self.next_row_id;
        resized.history_generation = self
            .history_generation
            .checked_add(1)
            .expect("history generation exhausted");
        resized.reidentify_allocated_rows_chronologically();
        let row_mappings = copied_row_ids
            .into_iter()
            .enumerate()
            .map(|(row, old_id)| (old_id, resized.row_ids[row]))
            .collect();
        *self = resized;
        row_mappings
    }

    /// Swaps two rows addressed relative to the live-screen offset.
    pub fn swap_rows(&mut self, first: i32, second: i32) {
        let first = self.absolute_index(first);
        let second = self.absolute_index(second);
        if first != second {
            self.bump_generation();
            self.rows.swap(first, second);
            self.row_ids.swap(first, second);
        }
    }

    /// Scrolls a screen region while preserving Foot's circular-history and
    /// viewport behavior.
    ///
    /// # Panics
    ///
    /// Panics if the region is outside the visible screen or `rows` exceeds
    /// the region height.
    pub fn scroll(
        &mut self,
        direction: ScrollDirection,
        region: ScrollRegion,
        rows: usize,
        erase_background: Color,
    ) -> ScrollResult {
        let start = usize::try_from(region.start()).expect("scroll region starts on screen");
        let end = usize::try_from(region.end()).expect("scroll region ends on screen");
        assert!(
            start < end && end <= self.screen_rows,
            "scroll region must fit within the screen"
        );
        assert!(
            rows <= end - start,
            "scroll amount must fit within the region"
        );

        let old_view = self.view;
        let view_was_following = self.view_follows_offset();
        if rows > 0 {
            match direction {
                ScrollDirection::Forward => {
                    self.scroll_forward_core(
                        start,
                        end,
                        rows,
                        erase_background,
                        view_was_following,
                    );
                }
                ScrollDirection::Reverse => {
                    self.scroll_reverse_core(
                        start,
                        end,
                        rows,
                        erase_background,
                        view_was_following,
                    );
                }
            }
        }

        debug_assert!((0..self.screen_rows).all(|row| self.row(Self::signed_row(row)).is_some()));
        ScrollResult {
            direction,
            region,
            rows,
            view_was_following,
            view_changed: self.view != old_view,
        }
    }

    /// Changes which physical slot corresponds to live-screen row zero.
    ///
    /// This primitive does not move the viewport automatically. Call
    /// [`Self::reset_view`] when the viewport should follow the new offset.
    pub fn set_offset(&mut self, offset: usize) {
        let offset = offset & (self.rows.len() - 1);
        if self.offset != offset {
            self.bump_generation();
            self.offset = offset;
        }
    }

    /// Changes which physical slot corresponds to viewport row zero.
    pub fn set_view(&mut self, view: usize) {
        let view = view & (self.rows.len() - 1);
        if self.view != view {
            self.bump_generation();
            self.view = view;
        }
    }

    /// Returns the viewport to the top of the live screen.
    pub fn reset_view(&mut self) {
        if self.view != self.offset {
            self.bump_generation();
            self.view = self.offset;
        }
    }

    fn scroll_forward_core(
        &mut self,
        start: usize,
        end: usize,
        rows: usize,
        erase_background: Color,
        view_was_following: bool,
    ) {
        let view_distance = self.absolute_to_scrollback(self.screen_rows, self.view);
        self.offset = self.offset.wrapping_add(rows) & (self.rows.len() - 1);

        if view_was_following {
            self.view = self.offset;
        } else if rows > view_distance {
            self.view = self.scrollback_to_absolute(self.screen_rows, 0);
            let distance_to_live = self.offset.wrapping_sub(self.view) & (self.rows.len() - 1);
            self.view = self
                .view
                .wrapping_add((rows - view_distance).min(distance_to_live))
                & (self.rows.len() - 1);
        }

        let amount = Self::signed_row(rows);
        for row in (0..start).rev() {
            let row = Self::signed_row(row);
            self.swap_rows(row - amount, row);
        }
        for row in (end..self.screen_rows).rev() {
            let row = Self::signed_row(row);
            self.swap_rows(row - amount, row);
        }
        for row in end - rows..end {
            self.recycle_row(Self::signed_row(row), erase_background);
        }
        self.reidentify_newest_history_rows(rows);
    }

    fn scroll_reverse_core(
        &mut self,
        start: usize,
        end: usize,
        rows: usize,
        erase_background: Color,
        view_was_following: bool,
    ) {
        for row in end - rows..end {
            let index = self.absolute_index(Self::signed_row(row));
            self.rows[index] = None;
            self.row_ids[index] = 0;
        }

        self.offset = self.offset.wrapping_sub(rows) & (self.rows.len() - 1);
        let view_distance = self.absolute_to_scrollback(self.screen_rows, self.view);
        let offset_distance = self.absolute_to_scrollback(self.screen_rows, self.offset);
        if view_was_following || view_distance > offset_distance {
            self.view = self.offset;
        }

        let amount = Self::signed_row(rows);
        for row in end + rows..self.screen_rows + rows {
            let row = Self::signed_row(row);
            self.swap_rows(row, row - amount);
        }
        for row in rows..start + rows {
            let row = Self::signed_row(row);
            self.swap_rows(row, row - amount);
        }
        for row in start..start + rows {
            self.row_or_allocate(Self::signed_row(row))
                .erase_all(erase_background);
        }
    }

    pub(crate) fn snapshot_view_rows(&self) -> Vec<&Row> {
        (0..self.screen_rows)
            .filter_map(|row| self.row_in_view(Self::signed_row(row)))
            .collect()
    }

    pub(crate) fn snapshot_identified_view_rows(&self) -> Vec<(u64, &Row)> {
        (0..self.screen_rows)
            .filter_map(|row| {
                let index = self.absolute_index_in_view(Self::signed_row(row));
                self.rows[index].as_ref().map(|row| {
                    let id = self.row_ids[index];
                    assert_ne!(id, 0, "allocated visible row has stable identity");
                    (id, row)
                })
            })
            .collect()
    }

    pub(crate) fn snapshot_scrollback_rows(&self, maximum_rows: usize) -> ScrollbackRows<'_> {
        let mut rows = self.scrollback_rows_chronological();
        let available = rows.len();
        let oldest = rows.first().map(|(id, _)| *id);
        let newest = rows.last().map(|(id, _)| *id);
        let omitted = available.saturating_sub(maximum_rows);
        if omitted > 0 {
            rows.drain(0..omitted);
        }
        (rows, available, omitted, oldest, newest)
    }

    pub(crate) fn snapshot_scrollback_page(
        &self,
        before_row_id: u64,
        maximum_rows: usize,
    ) -> (Vec<(u64, &Row)>, bool) {
        if maximum_rows == 0 {
            return (Vec::new(), false);
        }
        let history_capacity = self.rows.len() - self.screen_rows;
        let start = self.scrollback_start(self.screen_rows);
        let mut rows = std::collections::VecDeque::with_capacity(maximum_rows);
        let mut has_older = false;
        for distance in 0..history_capacity {
            let index = start.wrapping_add(distance) & (self.rows.len() - 1);
            let Some(row) = self.rows[index].as_ref() else {
                continue;
            };
            let id = self.row_ids[index];
            assert_ne!(id, 0, "allocated row has stable identity");
            if id >= before_row_id {
                continue;
            }
            if rows.len() == maximum_rows {
                rows.pop_front();
                has_older = true;
            }
            rows.push_back((id, row));
        }
        (rows.into(), has_older)
    }

    /// Retained rows newest-first, each with its id and whether it is in
    /// history rather than on the screen.
    ///
    /// A row gets a new, larger id as it scrolls into history, so history
    /// ids grow with position; screen rows keep older ids until then.
    pub(crate) fn rows_reverse_placed(&self) -> impl Iterator<Item = (u64, &Row, bool)> {
        let start = self.scrollback_start(self.screen_rows);
        let history_capacity = self.rows.len() - self.screen_rows;
        (0..self.rows.len()).rev().filter_map(move |distance| {
            let index = start.wrapping_add(distance) & (self.rows.len() - 1);
            let row = self.rows[index].as_ref()?;
            let id = self.row_ids[index];
            assert_ne!(id, 0, "allocated row has stable identity");
            Some((id, row, distance < history_capacity))
        })
    }

    fn scrollback_rows_chronological(&self) -> Vec<(u64, &Row)> {
        let history_capacity = self.rows.len() - self.screen_rows;
        let start = self.scrollback_start(self.screen_rows);
        (0..history_capacity)
            .filter_map(|distance| {
                let index = start.wrapping_add(distance) & (self.rows.len() - 1);
                self.rows[index].as_ref().map(|row| {
                    let id = self.row_ids[index];
                    assert_ne!(id, 0, "allocated row has stable identity");
                    (id, row)
                })
            })
            .collect()
    }

    pub(crate) fn cursor_in_view(&self) -> Option<Coordinate> {
        let cursor = self.cursor.position();
        let absolute = self.absolute_index(cursor.row);
        let relative = absolute.wrapping_sub(self.view) & (self.rows.len() - 1);
        (relative < self.screen_rows).then(|| {
            Coordinate::new(
                cursor.column,
                i32::try_from(relative).expect("visible row fits i32"),
            )
        })
    }

    fn scrollback_start(&self, screen_rows: usize) -> usize {
        self.offset.wrapping_add(screen_rows) & (self.rows.len() - 1)
    }

    fn assert_screen_rows(&self, screen_rows: usize) {
        assert!(
            (1..=self.rows.len()).contains(&screen_rows),
            "screen rows must fit within the grid"
        );
    }

    fn assert_coordinate_inputs(&self, screen_rows: usize, row: usize) {
        self.assert_screen_rows(screen_rows);
        assert!(row < self.rows.len(), "row index must fit within the grid");
    }

    fn bump_generation(&mut self) {
        self.generation = self
            .generation
            .checked_add(1)
            .expect("grid generation exhausted");
    }

    fn bump_history_generation(&mut self) {
        self.history_generation = self
            .history_generation
            .checked_add(1)
            .expect("history generation exhausted");
    }

    fn assign_new_row_id(&mut self, index: usize) {
        let id = self.next_row_id;
        self.next_row_id = self.next_row_id.checked_add(1).expect("row ID exhausted");
        self.row_ids[index] = id;
    }

    fn recycle_row(&mut self, relative_row: i32, background: Color) {
        let index = self.absolute_index(relative_row);
        self.bump_generation();
        if let Some(row) = self.rows[index].as_mut() {
            row.erase_all(background);
        } else {
            self.rows[index] = Some(Row::new(self.columns));
            self.rows[index]
                .as_mut()
                .expect("row was allocated")
                .erase_all(background);
        }
        self.assign_new_row_id(index);
    }

    fn reidentify_allocated_rows_chronologically(&mut self) {
        let start = self.scrollback_start(self.screen_rows);
        for distance in 0..self.rows.len() {
            let index = start.wrapping_add(distance) & (self.rows.len() - 1);
            if self.rows[index].is_some() {
                self.assign_new_row_id(index);
            }
        }
    }

    fn reidentify_newest_history_rows(&mut self, count: usize) {
        let history_capacity = self.rows.len() - self.screen_rows;
        let mask = self.rows.len() - 1;
        for distance in (1..=count.min(history_capacity)).rev() {
            let index = self.offset.wrapping_sub(distance) & mask;
            if self.rows[index].is_some() {
                self.assign_new_row_id(index);
            }
        }
    }

    fn resized_cursor(
        mut cursor: Cursor,
        old_screen_rows: usize,
        new_columns: usize,
        new_screen_rows: usize,
    ) -> Cursor {
        let position = cursor.position();
        let old_row = usize::try_from(position.row).expect("cursor row is non-negative");
        let old_column = usize::try_from(position.column).expect("cursor column is non-negative");
        let row = if old_row == old_screen_rows - 1 {
            new_screen_rows - 1
        } else {
            old_row.min(new_screen_rows - 1)
        };
        cursor.set_position(Coordinate::new(
            Self::signed_row(old_column.min(new_columns - 1)),
            Self::signed_row(row),
        ));
        cursor.set_deferred_wrap(false);
        cursor
    }

    fn signed_row(row: usize) -> i32 {
        i32::try_from(row).expect("grid dimensions fit in Foot's signed coordinate space")
    }

    fn assert_cursor(&self, cursor: Cursor) {
        let position = cursor.position();
        assert!(
            position.column >= 0
                && usize::try_from(position.column).is_ok_and(|column| column < self.columns)
                && position.row >= 0
                && usize::try_from(position.row).is_ok_and(|row| row < self.screen_rows),
            "cursor must fit within the screen"
        );
    }
}

fn deserialize_rows<'de, D>(deserializer: D) -> Result<Vec<Option<Row>>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_sequence(deserializer, MAX_CHECKPOINT_GRID_ROWS, "grid rows")
}

fn deserialize_row_ids<'de, D>(deserializer: D) -> Result<Vec<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_sequence(
        deserializer,
        MAX_CHECKPOINT_GRID_ROWS,
        "grid row identities",
    )
}

fn deserialize_bounded_sequence<'de, D, T>(
    deserializer: D,
    maximum: usize,
    name: &'static str,
) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct BoundedVisitor<T> {
        maximum: usize,
        name: &'static str,
        marker: PhantomData<T>,
    }

    impl<'de, T> Visitor<'de> for BoundedVisitor<T>
    where
        T: Deserialize<'de>,
    {
        type Value = Vec<T>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "at most {} {}", self.maximum, self.name)
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            if sequence
                .size_hint()
                .is_some_and(|length| length > self.maximum)
            {
                return Err(serde::de::Error::custom(format_args!(
                    "{} exceed checkpoint bound",
                    self.name
                )));
            }
            let mut values =
                Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(self.maximum));
            while let Some(value) = sequence.next_element()? {
                if values.len() == self.maximum {
                    return Err(serde::de::Error::custom(format_args!(
                        "{} exceed checkpoint bound",
                        self.name
                    )));
                }
                values.push(value);
            }
            Ok(values)
        }
    }

    deserializer.deserialize_seq(BoundedVisitor {
        maximum,
        name,
        marker: PhantomData,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CellContent;

    fn grid_with_labels(labels: &[char]) -> Grid {
        let mut grid = Grid::with_screen_size(8, 1, labels.len());
        for (row, &label) in labels.iter().enumerate() {
            grid.row_mut(i32::try_from(row).unwrap()).unwrap()[0]
                .set_content(CellContent::Scalar(label));
        }
        grid
    }

    fn visible_content(grid: &Grid) -> Vec<CellContent> {
        (0..grid.screen_rows())
            .map(|row| grid.row(i32::try_from(row).unwrap()).unwrap()[0].content())
            .collect()
    }

    #[test]
    #[should_panic(expected = "non-zero power of two")]
    fn rejects_zero_row_capacity() {
        let _ = Grid::new(0, 80);
    }

    #[test]
    #[should_panic(expected = "non-zero power of two")]
    fn rejects_non_power_of_two_row_capacity() {
        let _ = Grid::new(6, 80);
    }

    #[test]
    #[should_panic(expected = "at least one column")]
    fn rejects_zero_columns() {
        let _ = Grid::new(8, 0);
    }

    #[test]
    fn absolute_indices_wrap_in_both_directions() {
        let mut storage = Grid::new(8, 80);
        storage.set_offset(6);

        assert_eq!(storage.absolute_index(i32::MIN), 6);
        assert_eq!(storage.absolute_index(-7), 7);
        assert_eq!(storage.absolute_index(-1), 5);
        assert_eq!(storage.absolute_index(0), 6);
        assert_eq!(storage.absolute_index(1), 7);
        assert_eq!(storage.absolute_index(2), 0);
        assert_eq!(storage.absolute_index(9), 7);
        assert_eq!(storage.absolute_index(i32::MAX), 5);
    }

    #[test]
    fn rows_are_allocated_lazily_and_initialized_safely() {
        let mut storage = Grid::new(8, 3);
        assert_eq!(storage.row_capacity(), 8);
        assert_eq!(storage.columns(), 3);
        assert!(storage.row(0).is_none());

        let row = storage.row_or_allocate(0);
        assert_eq!(row.len(), 3);
        assert!(row.cells().iter().all(|cell| cell.attributes().clean()));

        storage.row_or_allocate(0)[0].set_content(CellContent::Scalar('x'));
        assert_eq!(
            storage.row_or_allocate(0)[0].content(),
            CellContent::Scalar('x')
        );
        assert!(storage.row(1).is_none());
    }

    #[test]
    fn wrapped_rows_keep_distinct_physical_storage() {
        let mut storage = Grid::new(8, 2);
        storage.set_offset(7);
        storage.row_or_allocate(0)[0].set_content(CellContent::Scalar('a'));
        storage.row_or_allocate(1)[0].set_content(CellContent::Scalar('b'));
        storage.row_or_allocate(-1)[0].set_content(CellContent::Scalar('c'));

        assert_eq!(
            storage.row(0).unwrap()[0].content(),
            CellContent::Scalar('a')
        );
        assert_eq!(
            storage.row(1).unwrap()[0].content(),
            CellContent::Scalar('b')
        );
        assert_eq!(
            storage.row(-1).unwrap()[0].content(),
            CellContent::Scalar('c')
        );
    }

    #[test]
    fn changing_offset_changes_logical_lookup_without_moving_rows() {
        let mut storage = Grid::new(4, 1);
        storage.row_or_allocate(0)[0].set_content(CellContent::Scalar('x'));

        storage.set_offset(1);
        assert!(storage.row(0).is_none());
        assert_eq!(
            storage.row(-1).unwrap()[0].content(),
            CellContent::Scalar('x')
        );

        storage.row_mut(-1).unwrap()[0].set_content(CellContent::Scalar('y'));
        assert_eq!(
            storage.row(-1).unwrap()[0].content(),
            CellContent::Scalar('y')
        );
    }

    #[test]
    fn resize_without_reflow_copies_visible_rows_and_drops_history() {
        let mut grid = Grid::with_screen_size(8, 3, 2);
        grid.row_mut(0).unwrap()[0].set_content(CellContent::Scalar('a'));
        grid.row_mut(1).unwrap()[0].set_content(CellContent::Scalar('b'));
        grid.scroll(
            ScrollDirection::Forward,
            ScrollRegion::new(0, 2),
            1,
            Color::default(),
        );
        grid.row_mut(1).unwrap()[0].set_content(CellContent::Scalar('c'));

        let mut cursor = Cursor::new(Coordinate::new(2, 1));
        cursor.set_deferred_wrap(true);
        grid.set_cursor(cursor);
        grid.set_saved_cursor(cursor);
        grid.resize_without_reflow(4, 2, 3);

        assert_eq!(grid.row_capacity(), 4);
        assert_eq!(grid.columns(), 2);
        assert_eq!(grid.screen_rows(), 3);
        assert_eq!(grid.offset(), 0);
        assert!(grid.view_follows_offset());
        assert_eq!(grid.row(0).unwrap()[0].content(), CellContent::Scalar('b'));
        assert_eq!(grid.row(1).unwrap()[0].content(), CellContent::Scalar('c'));
        assert_eq!(grid.row(2).unwrap()[0].content(), CellContent::Empty);
        assert!(grid.row(-1).is_none());
        assert_eq!(grid.cursor().position(), Coordinate::new(1, 2));
        assert_eq!(grid.saved_cursor().position(), Coordinate::new(1, 2));
        assert!(!grid.cursor().deferred_wrap());
        assert!(!grid.saved_cursor().deferred_wrap());
    }

    #[test]
    fn resize_without_reflow_drops_wide_character_cut_by_new_width() {
        let mut grid = Grid::with_screen_size(4, 4, 1);
        let row = grid.row_mut(0).unwrap();
        row[0].set_content(CellContent::Scalar('界'));
        row[1].set_content(CellContent::Spacer(2));
        row[2].set_content(CellContent::Spacer(1));
        row[3].set_content(CellContent::Scalar('x'));

        grid.resize_without_reflow(4, 2, 1);

        assert!(
            grid.row(0)
                .unwrap()
                .cells()
                .iter()
                .all(|cell| cell.content() == CellContent::Empty)
        );
        assert!(grid.row(0).unwrap().has_valid_wide_cells());
    }

    #[test]
    fn resize_without_reflow_growth_adds_dirty_empty_cells() {
        let mut grid = Grid::with_screen_size(4, 1, 1);
        grid.row_mut(0).unwrap()[0].set_content(CellContent::Scalar('x'));
        grid.row_mut(0).unwrap().set_dirty(false);

        grid.resize_without_reflow(4, 3, 1);

        let row = grid.row(0).unwrap();
        assert_eq!(row[0].content(), CellContent::Scalar('x'));
        assert!(
            row.cells()[1..]
                .iter()
                .all(|cell| cell.content() == CellContent::Empty && !cell.attributes().clean())
        );
        assert!(row.is_dirty());
    }

    #[test]
    fn row_swapping_uses_offset_relative_coordinates() {
        let mut grid = grid_with_labels(&['a', 'b', 'c']);
        grid.set_offset(7);
        grid.swap_rows(1, 2);
        grid.swap_rows(1, 2);
        assert_eq!(grid.absolute_index(0), 7);
    }

    #[test]
    fn full_forward_scroll_preserves_history_and_exposes_dirty_row() {
        let mut grid = grid_with_labels(&['a', 'b', 'c']);
        let result = grid.scroll(
            ScrollDirection::Forward,
            ScrollRegion::new(0, 3),
            1,
            Color::default(),
        );

        assert_eq!(
            visible_content(&grid),
            vec![
                CellContent::Scalar('b'),
                CellContent::Scalar('c'),
                CellContent::Empty,
            ]
        );
        assert_eq!(grid.row(-1).unwrap()[0].content(), CellContent::Scalar('a'));
        assert!(grid.row(2).unwrap().is_dirty());
        assert_eq!(grid.offset(), 1);
        assert_eq!(grid.view(), 1);
        assert!(result.view_was_following);
        assert!(result.view_changed);
    }

    #[test]
    fn full_forward_scroll_by_screen_height_moves_every_row_to_history() {
        let mut grid = grid_with_labels(&['a', 'b', 'c']);
        grid.scroll(
            ScrollDirection::Forward,
            ScrollRegion::new(0, 3),
            3,
            Color::default(),
        );

        assert!(
            visible_content(&grid)
                .iter()
                .all(|content| *content == CellContent::Empty)
        );
        assert_eq!(grid.row(-3).unwrap()[0].content(), CellContent::Scalar('a'));
        assert_eq!(grid.row(-2).unwrap()[0].content(), CellContent::Scalar('b'));
        assert_eq!(grid.row(-1).unwrap()[0].content(), CellContent::Scalar('c'));
        let ids = grid
            .snapshot_scrollback_rows(usize::MAX)
            .0
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        assert_eq!(ids.len(), 3);
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn forward_scroll_skips_unallocated_lazy_history_slots() {
        let mut grid = Grid::new(8, 1);
        grid.row_or_allocate(-1);
        let older_id = grid.row_ids[7];
        let next_row_id = grid.next_row_id;
        grid.scroll(
            ScrollDirection::Forward,
            ScrollRegion::new(0, 1),
            1,
            Color::default(),
        );

        let history = grid.snapshot_scrollback_rows(usize::MAX).0;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].0, older_id);
        assert_eq!(grid.row_ids[7], older_id);
        assert_eq!(grid.next_row_id, next_row_id + 1);
        assert!(grid.row(0).is_some());
    }

    #[test]
    fn overwritten_detached_viewport_advances_like_foot() {
        let mut grid = grid_with_labels(&['a', 'b', 'c']);
        grid.row_or_allocate(3);
        grid.set_view(3);

        grid.scroll(
            ScrollDirection::Forward,
            ScrollRegion::new(0, 3),
            1,
            Color::default(),
        );

        // Foot first moves to the new scrollback start (4), then scrolls down
        // by the overwritten distance (1).
        assert_eq!(grid.view(), 5);
        assert!(!grid.view_follows_offset());
    }

    #[test]
    fn partial_forward_scroll_preserves_non_scrolling_top_rows() {
        let mut grid = grid_with_labels(&['a', 'b', 'c']);
        grid.scroll(
            ScrollDirection::Forward,
            ScrollRegion::new(1, 3),
            1,
            Color::default(),
        );

        assert_eq!(
            visible_content(&grid),
            vec![
                CellContent::Scalar('a'),
                CellContent::Scalar('c'),
                CellContent::Empty,
            ]
        );
    }

    #[test]
    fn partial_then_full_scroll_keeps_history_ids_chronological() {
        let mut grid = grid_with_labels(&['a', 'b', 'c']);
        grid.scroll(
            ScrollDirection::Forward,
            ScrollRegion::new(1, 3),
            1,
            Color::default(),
        );
        grid.scroll(
            ScrollDirection::Forward,
            ScrollRegion::new(0, 3),
            1,
            Color::default(),
        );

        let ids = grid
            .snapshot_scrollback_rows(usize::MAX)
            .0
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        assert_eq!(ids.len(), 2);
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn full_reverse_scroll_drops_bottom_and_exposes_top_row() {
        let mut grid = grid_with_labels(&['a', 'b', 'c']);
        grid.scroll(
            ScrollDirection::Reverse,
            ScrollRegion::new(0, 3),
            1,
            Color::default(),
        );

        assert_eq!(
            visible_content(&grid),
            vec![
                CellContent::Empty,
                CellContent::Scalar('a'),
                CellContent::Scalar('b'),
            ]
        );
        assert_eq!(grid.offset(), 7);
        assert_eq!(grid.view(), 7);
        assert!(grid.row(0).unwrap().is_dirty());
    }

    #[test]
    fn partial_reverse_scroll_preserves_non_scrolling_bottom_rows() {
        let mut grid = grid_with_labels(&['a', 'b', 'c']);
        grid.scroll(
            ScrollDirection::Reverse,
            ScrollRegion::new(0, 2),
            1,
            Color::default(),
        );

        assert_eq!(
            visible_content(&grid),
            vec![
                CellContent::Empty,
                CellContent::Scalar('a'),
                CellContent::Scalar('c'),
            ]
        );
    }

    #[test]
    fn absolute_and_scrollback_coordinates_round_trip_across_wraparound() {
        let mut grid = Grid::new(8, 1);
        grid.set_offset(6);

        assert_eq!(grid.absolute_to_scrollback(3, 1), 0);
        assert_eq!(grid.absolute_to_scrollback(3, 0), 7);
        assert_eq!(grid.scrollback_to_absolute(3, 0), 1);
        assert_eq!(grid.scrollback_to_absolute(3, 7), 0);

        for absolute in 0..grid.row_capacity() {
            let scrollback = grid.absolute_to_scrollback(3, absolute);
            assert_eq!(grid.scrollback_to_absolute(3, scrollback), absolute);
        }
    }

    #[test]
    fn oldest_initialized_row_skips_empty_scrollback_slots() {
        let mut grid = Grid::new(8, 1);
        grid.set_offset(6);
        assert_eq!(grid.scrollback_start_ignoring_uninitialized(3), None);

        // Physical slot 3 is offset-relative row 5 and is the first allocated
        // slot encountered from the theoretical scrollback start at slot 1.
        grid.row_or_allocate(5);
        grid.row_or_allocate(0);
        assert_eq!(grid.scrollback_start_ignoring_uninitialized(3), Some(3));
    }

    #[test]
    fn viewport_lookup_is_independent_until_reset_to_live_screen() {
        let mut grid = Grid::new(8, 1);
        assert!(grid.view_follows_offset());

        grid.set_offset(6);
        grid.set_view(7);
        grid.row_or_allocate(2)[0].set_content(CellContent::Scalar('v'));

        assert_eq!(grid.offset(), 6);
        assert_eq!(grid.view(), 7);
        assert!(!grid.view_follows_offset());
        assert_eq!(grid.absolute_index_in_view(i32::MIN), 7);
        assert_eq!(grid.absolute_index_in_view(-1), 6);
        assert_eq!(grid.absolute_index_in_view(1), 0);
        assert_eq!(grid.absolute_index_in_view(i32::MAX), 6);
        assert_eq!(
            grid.row_in_view(1).unwrap()[0].content(),
            CellContent::Scalar('v')
        );

        grid.row_in_view_mut(1).unwrap()[0].set_content(CellContent::Scalar('w'));
        assert_eq!(grid.row(2).unwrap()[0].content(), CellContent::Scalar('w'));

        grid.reset_view();
        assert!(grid.view_follows_offset());
        assert_eq!(grid.view(), 6);
    }

    #[test]
    #[should_panic(expected = "screen rows must fit within the grid")]
    fn coordinate_conversion_rejects_zero_screen_rows() {
        let grid = Grid::new(8, 1);
        let _ = grid.absolute_to_scrollback(0, 0);
    }

    #[test]
    #[should_panic(expected = "screen rows must fit within the grid")]
    fn coordinate_conversion_rejects_oversized_screen() {
        let grid = Grid::new(8, 1);
        let _ = grid.absolute_to_scrollback(9, 0);
    }

    #[test]
    #[should_panic(expected = "row index must fit within the grid")]
    fn absolute_conversion_rejects_out_of_range_row() {
        let grid = Grid::new(8, 1);
        let _ = grid.absolute_to_scrollback(3, 8);
    }

    #[test]
    #[should_panic(expected = "row index must fit within the grid")]
    fn scrollback_conversion_rejects_out_of_range_row() {
        let grid = Grid::new(8, 1);
        let _ = grid.scrollback_to_absolute(3, 8);
    }
}
