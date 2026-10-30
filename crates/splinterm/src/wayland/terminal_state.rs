//! Pure terminal snapshot, semantic-update, and bounded-history reduction.

use anyhow::Result;
use splinterm_protocol::{
    ActiveScreen, CellAttributes, ColorSource, HistoryTransition, ScrollDirection, TerminalCell,
    TerminalRow, TerminalScroll, TerminalSnapshot, TerminalUpdate, UnderlineStyle,
};

use crate::viewport::ScrollbackViewport;

/// How an update treats a history cache that a detached viewport is reading.
///
/// A bounded update carries at most a few newest rows, so a burst of output
/// cannot always be joined to the cache. A live viewport then replaces the
/// cache with the newest rows; a detached one keeps its rows instead, or the
/// user's place in history would be lost and the viewport sent back to live.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum HistoryHold {
    /// A live viewport: an unjoinable cache is replaced.
    Off,
    /// A detached viewport: an unjoinable cache is kept as it is.
    IfUnjoinable,
    /// A viewport already holding its history: the cached rows never change.
    Always,
}

impl HistoryHold {
    pub(super) fn for_viewport(viewport: &ScrollbackViewport) -> Self {
        if viewport.is_live() {
            Self::Off
        } else if viewport.is_holding() {
            Self::Always
        } else {
            Self::IfUnjoinable
        }
    }
}

/// Whether an update's history rows were taken into the cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum HistoryApplied {
    Taken,
    /// The cache was kept; `appended_rows` rows of new history were not added.
    Held {
        appended_rows: usize,
    },
}

pub(super) const MAX_CACHED_HISTORY_ROWS: usize = 4096;
pub(super) const MAX_CACHED_HISTORY_BYTES: usize = 16 * 1024 * 1024;

pub(super) fn snapshot_is_newer(
    current: &TerminalSnapshot,
    candidate: &TerminalSnapshot,
) -> Result<bool> {
    if candidate.splint_id != current.splint_id || candidate.incarnation != current.incarnation {
        anyhow::bail!(
            "live snapshot identity changed from {:?}/{} to {:?}/{}",
            current.splint_id,
            current.incarnation,
            candidate.splint_id,
            candidate.incarnation
        );
    }
    Ok(candidate.revision > current.revision)
}

pub(super) fn snapshot_replaces(
    current: &TerminalSnapshot,
    candidate: &TerminalSnapshot,
    authoritative: bool,
) -> Result<bool> {
    Ok(snapshot_is_newer(current, candidate)?
        || (authoritative && candidate.revision == current.revision))
}

#[cfg(test)]
pub(super) fn coalesce_snapshots(
    current: Option<&TerminalSnapshot>,
    pending: impl IntoIterator<Item = TerminalSnapshot>,
) -> Result<Option<TerminalSnapshot>> {
    let mut latest = None;
    for candidate in pending {
        let baseline = latest.as_ref().or(current);
        if match baseline {
            Some(snapshot) => snapshot_is_newer(snapshot, &candidate)?,
            None => true,
        } {
            latest = Some(candidate);
        }
    }
    Ok(latest)
}

pub(super) fn blank_row(columns: usize) -> TerminalRow {
    TerminalRow {
        row_id: None,
        linebreak: false,
        cells: vec![
            TerminalCell {
                content: String::new(),
                spacer_remaining: None,
                attributes: CellAttributes {
                    bold: false,
                    dim: false,
                    italic: false,
                    underline: UnderlineStyle::None,
                    underline_color_source: ColorSource::Default,
                    underline_color: 0,
                    strikethrough: false,
                    blink: false,
                    conceal: false,
                    reverse: false,
                    foreground_source: ColorSource::Default,
                    foreground: 0,
                    background_source: ColorSource::Default,
                    background: 0,
                },
            };
            columns
        ],
    }
}

fn terminal_row_cache_bytes(row: &TerminalRow) -> usize {
    row.cells.iter().fold(32_usize, |total, cell| {
        total.saturating_add(64).saturating_add(cell.content.len())
    })
}

pub(super) fn history_cache_bytes(rows: &[TerminalRow]) -> usize {
    rows.iter().fold(0_usize, |total, row| {
        total.saturating_add(terminal_row_cache_bytes(row))
    })
}

pub(super) fn omitted_rows_before_cache(
    oldest_available_row_id: Option<u64>,
    rows: &[TerminalRow],
    available_rows: usize,
) -> usize {
    match (
        oldest_available_row_id,
        rows.first().and_then(|row| row.row_id),
    ) {
        // A cache that starts at or before the daemon's oldest row, as a held
        // cache can after the daemon trims, has nothing older to fetch.
        (Some(oldest), Some(first)) => usize::try_from(first.saturating_sub(oldest))
            .map_or(available_rows, |omitted| omitted.min(available_rows)),
        _ => available_rows.saturating_sub(rows.len()),
    }
}

fn bound_history_cache(rows: &mut Vec<TerminalRow>, keep_oldest: bool) {
    let mut bytes = history_cache_bytes(rows);
    if keep_oldest {
        if rows.len() > MAX_CACHED_HISTORY_ROWS {
            bytes = bytes.saturating_sub(history_cache_bytes(&rows[MAX_CACHED_HISTORY_ROWS..]));
            rows.truncate(MAX_CACHED_HISTORY_ROWS);
        }
        while bytes > MAX_CACHED_HISTORY_BYTES {
            let Some(row) = rows.pop() else {
                break;
            };
            bytes = bytes.saturating_sub(terminal_row_cache_bytes(&row));
        }
        return;
    }

    let row_excess = rows.len().saturating_sub(MAX_CACHED_HISTORY_ROWS);
    if row_excess > 0 {
        bytes = bytes.saturating_sub(history_cache_bytes(&rows[..row_excess]));
        rows.drain(..row_excess);
    }
    if bytes > MAX_CACHED_HISTORY_BYTES {
        let mut byte_excess_rows = 0;
        for row in rows.iter() {
            if bytes <= MAX_CACHED_HISTORY_BYTES {
                break;
            }
            bytes = bytes.saturating_sub(terminal_row_cache_bytes(row));
            byte_excess_rows += 1;
        }
        rows.drain(..byte_excess_rows);
    }
}

pub(super) fn bound_history_page_with_pins(
    mut rows: Vec<TerminalRow>,
    pinned_selection_rows: Option<[u64; 2]>,
    visible_rows: &[TerminalRow],
) -> Option<Vec<TerminalRow>> {
    bound_history_cache(&mut rows, true);
    pinned_selection_rows
        .is_none_or(|pins| {
            pins.into_iter().all(|row_id| {
                rows.iter().any(|row| row.row_id == Some(row_id))
                    || visible_rows.iter().any(|row| row.row_id == Some(row_id))
            })
        })
        .then_some(rows)
}

pub(super) fn changed_terminal_patch_rows(
    update: &TerminalUpdate,
    current: &TerminalSnapshot,
) -> Vec<usize> {
    if update
        .columns
        .is_some_and(|columns| columns != current.columns)
        || update.row_count.is_some_and(|rows| rows != current.rows)
    {
        return update.rows.iter().map(|patch| patch.index).collect();
    }
    let mut projected_rows = current.visible_rows.clone();
    for scroll in &update.scrolls {
        if scroll.start_row >= scroll.end_row
            || scroll.rows == 0
            || scroll.end_row > projected_rows.len()
            || scroll.rows > scroll.end_row.saturating_sub(scroll.start_row)
        {
            return update.rows.iter().map(|patch| patch.index).collect();
        }
        apply_terminal_scroll(&mut projected_rows, current.columns, *scroll);
    }
    let row_identity_is_visual = current
        .images
        .as_ref()
        .is_some_and(|images| !images.placements.is_empty());
    update
        .rows
        .iter()
        .filter(|patch| {
            projected_rows.get(patch.index).is_none_or(|row| {
                row.cells != patch.row.cells
                    || (row_identity_is_visual && row.row_id != patch.row.row_id)
            })
        })
        .map(|patch| patch.index)
        .collect()
}

pub(super) fn terminal_update_changes_visible_content(update: &TerminalUpdate) -> bool {
    !update.rows.is_empty()
        || !update.scrolls.is_empty()
        || update.columns.is_some()
        || update.row_count.is_some()
        || update.palette.is_some()
        || update.default_colors.is_some()
        || update.active_screen.is_some()
        || update.scrollback.is_some()
        || update.images.is_some()
}

pub(super) fn terminal_update_full_frame_reasons(
    update: &TerminalUpdate,
    current: &TerminalSnapshot,
) -> u64 {
    u64::from(
        update
            .columns
            .is_some_and(|columns| columns != current.columns),
    ) | (u64::from(update.row_count.is_some_and(|rows| rows != current.rows)) << 1)
        | (u64::from(
            update
                .palette
                .as_ref()
                .is_some_and(|palette| palette.get(16..) != current.palette.get(16..)),
        ) << 2)
        | (u64::from(
            update
                .active_screen
                .is_some_and(|active_screen| active_screen != current.active_screen),
        ) << 4)
        | (u64::from(
            update
                .images
                .as_ref()
                .is_some_and(|images| current.images.as_ref() != Some(images)),
        ) << 5)
        | (u64::from(
            current
                .images
                .as_ref()
                .is_some_and(|images| !images.placements.is_empty())
                && !update.scrolls.is_empty(),
        ) << 6)
}

#[cfg(test)]
pub(super) fn terminal_update_requires_full_frame(
    update: &TerminalUpdate,
    current: &TerminalSnapshot,
) -> bool {
    terminal_update_full_frame_reasons(update, current) != 0
}

#[cfg(test)]
pub(super) fn apply_scrollback_update(
    snapshot: &mut TerminalSnapshot,
    scrollback: splinterm_protocol::TerminalScrollbackUpdate,
) -> Result<()> {
    apply_scrollback_update_holding(snapshot, scrollback, HistoryHold::Off).map(|_| ())
}

fn apply_scrollback_update_holding(
    snapshot: &mut TerminalSnapshot,
    scrollback: splinterm_protocol::TerminalScrollbackUpdate,
    hold: HistoryHold,
) -> Result<HistoryApplied> {
    match scrollback.transition {
        HistoryTransition::Append { .. }
            if scrollback.history_generation != snapshot.history_generation =>
        {
            anyhow::bail!("history append changed generation");
        }
        HistoryTransition::Clear | HistoryTransition::Reflow
            if scrollback.history_generation <= snapshot.history_generation =>
        {
            anyhow::bail!("history reset did not change generation");
        }
        _ => {}
    }
    let first_returned = scrollback.rows.first().and_then(|row| row.row_id);
    // A bounded update can omit newly appended rows. Joining its tail to an
    // older cache would hide that gap from the viewport. Row IDs are ordered,
    // not necessarily consecutive: prove continuity by counts or overlap.
    let complete_append = match scrollback.transition {
        HistoryTransition::Append {
            appended_rows,
            trimmed_rows,
        } => {
            scrollback.rows.len() >= appended_rows
                && snapshot.newest_available_scrollback_row_id.is_some()
                && snapshot.scrollback_rows.last().and_then(|row| row.row_id)
                    == snapshot.newest_available_scrollback_row_id
                && snapshot
                    .available_scrollback_rows
                    .checked_add(appended_rows)
                    .and_then(|available| available.checked_sub(trimmed_rows))
                    == Some(scrollback.available_rows)
        }
        _ => false,
    };
    let overlapping_tail = first_returned
        .and_then(|first| {
            snapshot
                .scrollback_rows
                .iter()
                .position(|row| row.row_id == Some(first))
        })
        .is_some_and(|start| {
            let suffix = &snapshot.scrollback_rows[start..];
            suffix.len() <= scrollback.rows.len()
                && suffix
                    .iter()
                    .zip(&scrollback.rows)
                    .all(|(cached, returned)| cached.row_id == returned.row_id)
        });
    let same_history = scrollback.history_generation == snapshot.history_generation
        && matches!(
            scrollback.transition,
            HistoryTransition::Append { .. } | HistoryTransition::Replace
        );
    let preserve_cached = same_history && (complete_append || overlapping_tail);
    let keep_cache = same_history
        && match hold {
            HistoryHold::Off => false,
            HistoryHold::IfUnjoinable => !preserve_cached,
            HistoryHold::Always => true,
        };
    if keep_cache {
        // Rows are immutable within one history generation, so the kept rows
        // stay correct even after the daemon trims them.
        snapshot.oldest_available_scrollback_row_id = scrollback.oldest_available_row_id;
        snapshot.newest_available_scrollback_row_id = scrollback.newest_available_row_id;
        snapshot.available_scrollback_rows = scrollback.available_rows;
        snapshot.omitted_oldest_scrollback_rows = omitted_rows_before_cache(
            snapshot.oldest_available_scrollback_row_id,
            &snapshot.scrollback_rows,
            snapshot.available_scrollback_rows,
        );
        let appended_rows = match scrollback.transition {
            HistoryTransition::Append { appended_rows, .. } => appended_rows,
            _ => 0,
        };
        return Ok(HistoryApplied::Held { appended_rows });
    }
    let oldest_available = scrollback.oldest_available_row_id;
    let mut rows = std::mem::take(&mut snapshot.scrollback_rows);
    if preserve_cached {
        rows.retain(|row| {
            row.row_id
                .zip(oldest_available)
                .is_some_and(|(id, oldest)| id >= oldest)
                && row
                    .row_id
                    .zip(first_returned)
                    .is_some_and(|(id, first)| id < first)
        });
    } else {
        rows.clear();
    }
    rows.extend(scrollback.rows);
    bound_history_cache(&mut rows, false);
    snapshot.history_generation = scrollback.history_generation;
    snapshot.oldest_available_scrollback_row_id = scrollback.oldest_available_row_id;
    snapshot.newest_available_scrollback_row_id = scrollback.newest_available_row_id;
    snapshot.scrollback_rows = rows;
    snapshot.available_scrollback_rows = scrollback.available_rows;
    snapshot.omitted_oldest_scrollback_rows = snapshot
        .available_scrollback_rows
        .saturating_sub(snapshot.scrollback_rows.len());
    Ok(HistoryApplied::Taken)
}

/// Applies `update` to a pane's snapshot on behalf of its viewport.
///
/// A detached viewport that cannot take the update's history into its cache
/// starts holding its history and the screen it was composed with, and counts
/// the rows it did not take as unseen. Call `observe_history_change` after.
pub(super) fn apply_terminal_update_for_viewport(
    snapshot: &mut TerminalSnapshot,
    update: TerminalUpdate,
    viewport: &mut ScrollbackViewport,
) -> Result<()> {
    let hold = HistoryHold::for_viewport(viewport);
    let screen = (hold == HistoryHold::IfUnjoinable && update.scrollback.is_some())
        .then(|| snapshot.visible_rows.clone());
    let applied = apply_terminal_update_holding(snapshot, update, hold)?;
    if let HistoryApplied::Held { appended_rows } = applied {
        if let Some(screen) = screen {
            viewport.hold(screen);
        }
        viewport.note_unseen(appended_rows);
    }
    Ok(())
}

/// Keeps a detached viewport's history across an authoritative snapshot, which
/// carries only the newest few history rows. The cache is joined to `next`
/// when its rows overlap; otherwise the viewport holds the cache and the screen
/// it was composed with. Call before `observe_history_change`.
pub(super) fn retain_history_for_viewport(
    current: &TerminalSnapshot,
    next: &mut TerminalSnapshot,
    viewport: &mut ScrollbackViewport,
) {
    if viewport.is_live()
        || next.history_generation != current.history_generation
        || next.active_screen == ActiveScreen::Alternate
        || next.available_scrollback_rows == 0
        || current.scrollback_rows.is_empty()
    {
        return;
    }
    let joined_at = (!viewport.is_holding())
        .then(|| next.scrollback_rows.first().and_then(|row| row.row_id))
        .flatten()
        .and_then(|first| {
            current
                .scrollback_rows
                .iter()
                .position(|row| row.row_id == Some(first))
        })
        .filter(|&start| {
            let suffix = &current.scrollback_rows[start..];
            suffix.len() <= next.scrollback_rows.len()
                && suffix
                    .iter()
                    .zip(&next.scrollback_rows)
                    .all(|(cached, returned)| cached.row_id == returned.row_id)
        });
    let mut rows = if let Some(start) = joined_at {
        let mut rows = current.scrollback_rows[..start].to_vec();
        rows.append(&mut next.scrollback_rows);
        bound_history_cache(&mut rows, false);
        rows
    } else {
        viewport.hold(current.visible_rows.clone());
        current.scrollback_rows.clone()
    };
    std::mem::swap(&mut next.scrollback_rows, &mut rows);
    next.omitted_oldest_scrollback_rows = omitted_rows_before_cache(
        next.oldest_available_scrollback_row_id,
        &next.scrollback_rows,
        next.available_scrollback_rows,
    );
}

/// Drops a history cache that no longer ends at the newest history row.
///
/// Only a held viewport keeps such a cache. Once the viewport is live again,
/// joining that cache to the current screen would show a seam, so it is
/// dropped and refilled from older pages on demand.
pub(super) fn discard_lagging_history(
    snapshot: &mut TerminalSnapshot,
    viewport: &ScrollbackViewport,
) {
    if !viewport.is_live() || snapshot.scrollback_rows.is_empty() {
        return;
    }
    let tail = snapshot.scrollback_rows.last().and_then(|row| row.row_id);
    if tail.is_some() && tail == snapshot.newest_available_scrollback_row_id {
        return;
    }
    snapshot.scrollback_rows.clear();
    snapshot.omitted_oldest_scrollback_rows = snapshot.available_scrollback_rows;
}

fn apply_terminal_scroll(visible_rows: &mut [TerminalRow], columns: usize, scroll: TerminalScroll) {
    let region = &mut visible_rows[scroll.start_row..scroll.end_row];
    match scroll.direction {
        ScrollDirection::Forward => {
            region.rotate_left(scroll.rows);
            let exposed_start = region.len() - scroll.rows;
            for row in &mut region[exposed_start..] {
                *row = blank_row(columns);
            }
        }
        ScrollDirection::Reverse => {
            region.rotate_right(scroll.rows);
            for row in &mut region[..scroll.rows] {
                *row = blank_row(columns);
            }
        }
    }
}

#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "tests and the history benchmark apply updates without a viewport"
    )
)]
pub(super) fn apply_terminal_update(
    snapshot: &mut TerminalSnapshot,
    update: TerminalUpdate,
) -> Result<()> {
    apply_terminal_update_holding(snapshot, update, HistoryHold::Off).map(|_| ())
}

fn apply_terminal_update_holding(
    snapshot: &mut TerminalSnapshot,
    update: TerminalUpdate,
    hold: HistoryHold,
) -> Result<HistoryApplied> {
    update
        .validate_against(
            snapshot.revision,
            snapshot.history_generation,
            snapshot.columns,
            snapshot.rows,
        )
        .map_err(|error| anyhow::anyhow!(error.message))?;
    if let Some(columns) = update.columns {
        if columns == 0 || columns > usize::from(splinterm_protocol::MAX_COLUMNS) {
            anyhow::bail!("terminal update columns exceed protocol limits");
        }
        snapshot.columns = columns;
    }
    if let Some(rows) = update.row_count {
        if rows == 0 || rows > usize::from(splinterm_protocol::MAX_ROWS) {
            anyhow::bail!("terminal update rows exceed protocol limits");
        }
        snapshot.rows = rows;
        snapshot
            .visible_rows
            .resize_with(rows, || blank_row(snapshot.columns));
        snapshot.visible_rows.truncate(rows);
    }
    for scroll in update.scrolls {
        apply_terminal_scroll(&mut snapshot.visible_rows, snapshot.columns, scroll);
    }
    for patch in update.rows {
        if patch.index >= snapshot.rows || patch.row.cells.len() > snapshot.columns {
            anyhow::bail!("terminal row patch exceeds current dimensions");
        }
        snapshot.visible_rows[patch.index] = patch.row;
    }
    if let Some(cursor) = update.cursor {
        snapshot.cursor_column = cursor.column;
        snapshot.cursor_row = cursor.row;
        snapshot.cursor_deferred_wrap = cursor.deferred_wrap;
    }
    if let Some(title) = update.title {
        snapshot.title = title;
    }
    if let Some(modes) = update.input_modes {
        snapshot.input_modes = modes;
    }
    if let Some(screen) = update.active_screen {
        snapshot.active_screen = screen;
    }
    if let Some(palette) = update.palette {
        if palette.len() != 256 {
            anyhow::bail!("terminal update palette must have 256 entries");
        }
        snapshot.palette = palette;
    }
    if let Some(colors) = update.default_colors {
        snapshot.default_colors = colors;
    }
    let applied = match update.scrollback {
        Some(scrollback) => apply_scrollback_update_holding(snapshot, scrollback, hold)?,
        None => HistoryApplied::Taken,
    };
    if let Some(images) = update.images {
        snapshot.images = Some(images);
    }
    snapshot.revision = update.revision;
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history_row(id: u64, content_bytes: usize) -> TerminalRow {
        let mut row = blank_row(1);
        row.row_id = Some(id);
        row.cells[0].content = "x".repeat(content_bytes);
        row
    }

    #[test]
    fn history_cache_enforces_row_budget_from_either_edge() {
        let mut newest = (1..=u64::try_from(MAX_CACHED_HISTORY_ROWS + 4).unwrap())
            .map(|id| history_row(id, 0))
            .collect::<Vec<_>>();
        bound_history_cache(&mut newest, false);
        assert_eq!(newest.len(), MAX_CACHED_HISTORY_ROWS);
        assert_eq!(newest.first().and_then(|row| row.row_id), Some(5));

        let mut oldest = (1..=u64::try_from(MAX_CACHED_HISTORY_ROWS + 4).unwrap())
            .map(|id| history_row(id, 0))
            .collect::<Vec<_>>();
        bound_history_cache(&mut oldest, true);
        assert_eq!(oldest.len(), MAX_CACHED_HISTORY_ROWS);
        assert_eq!(oldest.last().and_then(|row| row.row_id), Some(4096));
    }

    fn reference_bound_history_cache(rows: &mut Vec<TerminalRow>, keep_oldest: bool) {
        while rows.len() > MAX_CACHED_HISTORY_ROWS
            || history_cache_bytes(rows) > MAX_CACHED_HISTORY_BYTES
        {
            if rows.is_empty() {
                break;
            }
            if keep_oldest {
                rows.pop();
            } else {
                rows.remove(0);
            }
        }
    }

    #[test]
    fn batched_history_bounding_matches_the_reference_algorithm() {
        for (row_count, content_bytes) in [
            (0, 0),
            (1, 0),
            (MAX_CACHED_HISTORY_ROWS, 0),
            (MAX_CACHED_HISTORY_ROWS + 4, 0),
            (20, 1024 * 1024),
            (2, MAX_CACHED_HISTORY_BYTES),
        ] {
            let source = (1..=u64::try_from(row_count).unwrap())
                .map(|id| history_row(id, content_bytes))
                .collect::<Vec<_>>();
            for keep_oldest in [false, true] {
                let mut expected = source.clone();
                reference_bound_history_cache(&mut expected, keep_oldest);
                let mut actual = source.clone();
                bound_history_cache(&mut actual, keep_oldest);
                assert_eq!(actual, expected, "rows={row_count} oldest={keep_oldest}");
            }
        }
    }

    #[test]
    fn history_cache_enforces_byte_budget_and_preserves_order() {
        let source = (1..=20)
            .map(|id| history_row(id, 1024 * 1024))
            .collect::<Vec<_>>();

        let mut newest = source.clone();
        bound_history_cache(&mut newest, false);
        assert!(history_cache_bytes(&newest) <= MAX_CACHED_HISTORY_BYTES);
        assert!(
            newest
                .windows(2)
                .all(|pair| pair[0].row_id < pair[1].row_id)
        );
        assert_eq!(newest.last().and_then(|row| row.row_id), Some(20));

        let mut oldest = source;
        bound_history_cache(&mut oldest, true);
        assert!(history_cache_bytes(&oldest) <= MAX_CACHED_HISTORY_BYTES);
        assert!(
            oldest
                .windows(2)
                .all(|pair| pair[0].row_id < pair[1].row_id)
        );
        assert_eq!(oldest.first().and_then(|row| row.row_id), Some(1));
    }

    #[test]
    fn omitted_history_tracks_the_stable_cache_window_position() {
        let rows = |first| {
            (first..first + 100)
                .map(|id| history_row(id, 0))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            omitted_rows_before_cache(Some(100), &rows(1_000), 1_000),
            900
        );
        assert_eq!(omitted_rows_before_cache(Some(100), &rows(500), 1_000), 400);
        assert_eq!(omitted_rows_before_cache(Some(100), &rows(100), 1_000), 0);
        assert_eq!(omitted_rows_before_cache(None, &rows(1_000), 1_000), 900);
        // A held cache older than the daemon's oldest row: nothing to fetch.
        assert_eq!(omitted_rows_before_cache(Some(100), &rows(50), 1_000), 0);
    }

    fn held_case_snapshot(
        cached: std::ops::RangeInclusive<u64>,
        screen_rows: usize,
    ) -> TerminalSnapshot {
        let rows = cached
            .clone()
            .map(|id| history_row(id, 1))
            .collect::<Vec<_>>();
        TerminalSnapshot {
            splint_id: splinterm_core::SplintId::new(),
            incarnation: 1,
            revision: 10,
            columns: 1,
            rows: screen_rows,
            cursor_column: 0,
            cursor_row: 0,
            cursor_deferred_wrap: false,
            active_screen: ActiveScreen::Normal,
            input_modes: splinterm_protocol::TerminalInputModes {
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
            visible_rows: (0..screen_rows)
                .map(|index| {
                    let mut row = blank_row(1);
                    row.cells[0].content = format!("s{index}");
                    row
                })
                .collect(),
            history_generation: 1,
            oldest_available_scrollback_row_id: Some(*cached.start()),
            newest_available_scrollback_row_id: Some(*cached.end()),
            available_scrollback_rows: rows.len(),
            scrollback_rows: rows,
            omitted_oldest_scrollback_rows: 0,
            images: None,
            exited_code: None,
            exited_signal: None,
        }
    }

    /// A burst longer than one bounded update: the newest rows only, with the
    /// screen fully replaced by new output.
    fn burst(snapshot: &TerminalSnapshot, appended: u64) -> TerminalUpdate {
        let newest = snapshot.newest_available_scrollback_row_id.unwrap() + appended;
        let returned = (newest - 15..=newest)
            .map(|id| history_row(id, 1))
            .collect::<Vec<_>>();
        TerminalUpdate {
            base_revision: snapshot.revision,
            revision: snapshot.revision + 1,
            rows: (0..snapshot.rows)
                .map(|index| {
                    let mut row = blank_row(1);
                    row.row_id = Some(newest + 1 + u64::try_from(index).unwrap());
                    row.cells[0].content = format!("n{index}");
                    splinterm_protocol::TerminalRowPatch { index, row }
                })
                .collect(),
            scrolls: Vec::new(),
            cursor: None,
            title: None,
            input_modes: None,
            active_screen: None,
            palette: None,
            default_colors: None,
            columns: None,
            row_count: None,
            scrollback: Some(splinterm_protocol::TerminalScrollbackUpdate {
                transition: HistoryTransition::Append {
                    appended_rows: usize::try_from(appended).unwrap(),
                    trimmed_rows: 0,
                },
                history_generation: snapshot.history_generation,
                oldest_available_row_id: snapshot.oldest_available_scrollback_row_id,
                newest_available_row_id: Some(newest),
                available_rows: snapshot.available_scrollback_rows
                    + usize::try_from(appended).unwrap(),
                omitted_oldest_rows: snapshot.available_scrollback_rows
                    + usize::try_from(appended).unwrap()
                    - returned.len(),
                rows: returned,
            }),
            images: None,
        }
    }

    fn ids(rows: &[&TerminalRow]) -> Vec<Option<u64>> {
        rows.iter().map(|row| row.row_id).collect()
    }

    fn texts(rows: &[&TerminalRow]) -> Vec<String> {
        rows.iter()
            .map(|row| row.cells[0].content.clone())
            .collect()
    }

    fn update_for_viewport(
        snapshot: &mut TerminalSnapshot,
        update: TerminalUpdate,
        viewport: &mut ScrollbackViewport,
    ) {
        let generation = snapshot.history_generation;
        let previous = if viewport.is_live() {
            Vec::new()
        } else {
            snapshot.scrollback_rows.clone()
        };
        apply_terminal_update_for_viewport(snapshot, update, viewport).unwrap();
        viewport.observe_history_change(generation, &previous, snapshot);
        discard_lagging_history(snapshot, viewport);
    }

    #[test]
    fn a_detached_viewport_holds_its_place_through_bursts_longer_than_an_update() {
        let mut snapshot = held_case_snapshot(1..=100, 4);
        let mut viewport = ScrollbackViewport::default();
        viewport.scroll_up(10, &snapshot);
        let before = viewport.visible_rows(&snapshot);
        let (before_ids, before_texts) = (ids(&before), texts(&before));
        assert_eq!(before_ids, [Some(91), Some(92), Some(93), Some(94)]);

        for _ in 0..5 {
            let update = burst(&snapshot, 40);
            update_for_viewport(&mut snapshot, update, &mut viewport);
            assert!(
                !viewport.is_live(),
                "a burst must not send the viewport live"
            );
            assert!(viewport.is_holding());
            let shown = viewport.visible_rows(&snapshot);
            assert_eq!(ids(&shown), before_ids);
            assert_eq!(texts(&shown), before_texts);
        }
        assert_eq!(viewport.unseen_rows(), 200);
        assert_eq!(snapshot.scrollback_rows.len(), 100, "the cache is held");
        assert_eq!(snapshot.newest_available_scrollback_row_id, Some(300));

        // Scrolling up still works while held, and scrolling just short of
        // the bottom shows the held screen, never the live one.
        viewport.scroll_up(50, &snapshot);
        assert_eq!(viewport.offset_from_bottom(), 60);
        viewport.scroll_down(59, &snapshot);
        let shown = viewport.visible_rows(&snapshot);
        assert_eq!(texts(&shown).last().map(String::as_str), Some("s2"));

        // Reaching the bottom returns live and drops the lagging cache.
        viewport.scroll_down(1, &snapshot);
        assert!(viewport.is_live());
        assert!(!viewport.is_holding());
        discard_lagging_history(&mut snapshot, &viewport);
        assert!(snapshot.scrollback_rows.is_empty());
        assert_eq!(snapshot.omitted_oldest_scrollback_rows, 300);
    }

    #[test]
    fn a_live_viewport_still_replaces_an_unjoinable_cache() {
        let mut snapshot = held_case_snapshot(1..=100, 4);
        let mut viewport = ScrollbackViewport::default();
        let update = burst(&snapshot, 40);
        update_for_viewport(&mut snapshot, update, &mut viewport);
        assert!(viewport.is_live());
        assert_eq!(snapshot.scrollback_rows.len(), 16);
        assert_eq!(
            snapshot.scrollback_rows.last().and_then(|row| row.row_id),
            Some(140)
        );
    }

    #[test]
    fn a_detached_viewport_joins_small_appends_without_holding() {
        let mut snapshot = held_case_snapshot(1..=100, 4);
        let mut viewport = ScrollbackViewport::default();
        viewport.scroll_up(10, &snapshot);
        let mut update = burst(&snapshot, 3);
        if let Some(scrollback) = update.scrollback.as_mut() {
            scrollback.rows = (101..=103).map(|id| history_row(id, 1)).collect();
            scrollback.omitted_oldest_rows = 100;
        }
        update_for_viewport(&mut snapshot, update, &mut viewport);
        assert!(!viewport.is_holding());
        assert_eq!(viewport.offset_from_bottom(), 13);
        assert_eq!(snapshot.scrollback_rows.len(), 103);
    }

    #[test]
    fn an_authoritative_snapshot_keeps_a_detached_viewports_history() {
        let current = held_case_snapshot(1..=100, 4);
        let mut viewport = ScrollbackViewport::default();
        viewport.scroll_up(10, &current);

        // Overlapping rows join the cache.
        let mut joined = held_case_snapshot(90..=110, 4);
        joined.scrollback_rows.drain(..4);
        joined.oldest_available_scrollback_row_id = Some(1);
        joined.available_scrollback_rows = 110;
        retain_history_for_viewport(&current, &mut joined, &mut viewport);
        assert!(!viewport.is_holding());
        assert_eq!(joined.scrollback_rows.len(), 110);

        // Disjoint rows: the viewport holds the old cache and screen.
        let mut disjoint = held_case_snapshot(200..=215, 4);
        disjoint.oldest_available_scrollback_row_id = Some(1);
        disjoint.available_scrollback_rows = 215;
        retain_history_for_viewport(&current, &mut disjoint, &mut viewport);
        assert!(viewport.is_holding());
        assert_eq!(disjoint.scrollback_rows, current.scrollback_rows);
        assert_eq!(disjoint.omitted_oldest_scrollback_rows, 0);
        viewport.observe_history_change(1, &current.scrollback_rows, &disjoint);
        assert_eq!(viewport.offset_from_bottom(), 10);
        assert_eq!(
            texts(&viewport.visible_rows(&disjoint)),
            vec!["x".to_owned(); 4]
        );

        // A new history generation still returns live.
        let mut cleared = held_case_snapshot(1..=4, 4);
        cleared.history_generation = 2;
        retain_history_for_viewport(&current, &mut cleared, &mut viewport);
        viewport.observe_history_change(1, &current.scrollback_rows, &cleared);
        assert!(viewport.is_live());
    }

    #[test]
    fn a_view_into_a_taller_grids_hidden_rows_holds_its_place_through_bursts() {
        let mut snapshot = held_case_snapshot(1..=100, 4);
        for (index, row) in snapshot.visible_rows.iter_mut().enumerate() {
            row.row_id = Some(1_000 + u64::try_from(index).unwrap());
        }
        let mut viewport = ScrollbackViewport::default();
        viewport.set_local_rows(Some(2), Some(&snapshot));
        assert_eq!(
            ids(&viewport.visible_rows(&snapshot)),
            [Some(1_002), Some(1_003)]
        );
        viewport.scroll_up(1, &snapshot);
        let before = ids(&viewport.visible_rows(&snapshot));
        assert_eq!(before, [Some(1_001), Some(1_002)]);

        for _ in 0..3 {
            let update = burst(&snapshot, 40);
            update_for_viewport(&mut snapshot, update, &mut viewport);
            assert!(!viewport.is_live(), "output never sends the view live");
            assert_eq!(ids(&viewport.visible_rows(&snapshot)), before);
        }
        assert_eq!(viewport.offset_from_bottom(), 1);

        viewport.scroll_down(usize::MAX, &snapshot);
        assert!(viewport.is_live());
        assert_eq!(
            ids(&viewport.visible_rows(&snapshot)),
            [Some(223), Some(224)]
        );
    }
}
