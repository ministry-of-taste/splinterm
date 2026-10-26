//! Logical-line reflow for circular grid resize.
//!
//! Derived from Foot 1.27.0 `grid.c` at commit
//! `3c5b584b0eafa772eb4376fb6eaf6643399e190e`, especially
//! `grid_resize_and_reflow`. URI, underline, shell-integration, selection, and
//! sixel translations are intentionally absent because those features are
//! outside the current terminal-kernel milestone.

use std::collections::BTreeMap;

use super::Grid;
use crate::{Cell, CellContent, Coordinate, Row};

#[derive(Default)]
struct Mappings {
    cursor: Option<(usize, usize)>,
    saved_cursor: Option<(usize, usize)>,
    view: Option<(usize, usize)>,
}

struct Origins {
    cursor: (usize, usize),
    saved_cursor: (usize, usize),
    view: (usize, usize),
}

struct Builder {
    columns: usize,
    rows: Vec<Row>,
    current: Vec<Cell>,
    mappings: Mappings,
    row_mappings: BTreeMap<u64, usize>,
    origins: Origins,
}

impl Builder {
    fn new(columns: usize, origins: Origins) -> Self {
        Self {
            columns,
            rows: Vec::new(),
            current: Vec::with_capacity(columns),
            mappings: Mappings::default(),
            row_mappings: BTreeMap::new(),
            origins,
        }
    }

    fn append_unit(
        &mut self,
        physical_row: usize,
        source_column: usize,
        source_width: usize,
        output: &[Cell],
    ) {
        let output = if output.len() > self.columns {
            &[][..]
        } else {
            output
        };
        let output_width = output.len().max(1);

        if !self.current.is_empty() && self.current.len() + output_width > self.columns {
            self.finish_wrapped();
        }

        let output_column = self.current.len();
        if output.is_empty() {
            self.current.push(Cell::default());
        } else {
            self.current.extend_from_slice(output);
        }

        for offset in 0..source_width {
            let mapped_column = output_column + offset.min(output_width - 1);
            self.map_origin(physical_row, source_column + offset, mapped_column);
        }
    }

    fn finish(&mut self, linebreak: bool) {
        while self.current.len() < self.columns {
            self.current.push(Cell::default());
        }
        self.push_current(linebreak);
    }

    fn finish_wrapped(&mut self) {
        while self.current.len() < self.columns {
            self.current.push(Cell::new(CellContent::Spacer(0)));
        }
        self.push_current(false);
    }

    fn push_current(&mut self, linebreak: bool) {
        let cells = std::mem::replace(&mut self.current, Vec::with_capacity(self.columns));
        self.rows.push(Row::from_cells(cells, linebreak, true));
    }

    fn finish_hard_line(&mut self) {
        self.finish(true);
    }

    fn map_origin(&mut self, physical_row: usize, source_column: usize, output_column: usize) {
        let output = (self.rows.len(), output_column);
        if (physical_row, source_column) == self.origins.cursor {
            self.mappings.cursor = Some(output);
        }
        if (physical_row, source_column) == self.origins.saved_cursor {
            self.mappings.saved_cursor = Some(output);
        }
        if (physical_row, source_column) == self.origins.view {
            self.mappings.view = Some(output);
        }
    }
}

impl Grid {
    /// Resizes the grid and reflows soft-wrapped logical lines.
    ///
    /// `composed_width` resolves opaque [`CellContent::Composed`] keys to their
    /// display width and is authoritative for composed leaders. Positive
    /// spacer sequences determine widths for other wide-cell leaders. Returned
    /// composed widths are clamped to at least one column.
    ///
    /// # Panics
    ///
    /// Panics if the new dimensions violate [`Grid::new`] or
    /// [`Grid::with_screen_size`] constraints.
    #[allow(
        clippy::too_many_lines,
        reason = "keeping the ordered Foot reflow state transition together aids parity review"
    )]
    pub fn resize_with_reflow<F>(
        &mut self,
        new_row_capacity: usize,
        new_columns: usize,
        new_screen_rows: usize,
        composed_width: F,
    ) -> BTreeMap<u64, u64>
    where
        F: Fn(u32) -> usize,
    {
        let mut dimensions = Grid::new(new_row_capacity, new_columns);
        dimensions.assert_screen_rows(new_screen_rows);

        let mask = self.rows.len() - 1;
        let cursor_position = self.cursor.position();
        let saved_position = self.saved_cursor.position();
        let origins = Origins {
            cursor: (
                self.offset.wrapping_add(
                    usize::try_from(cursor_position.row).expect("cursor row is non-negative"),
                ) & mask,
                usize::try_from(cursor_position.column).expect("cursor column is non-negative"),
            ),
            saved_cursor: (
                self.offset.wrapping_add(
                    usize::try_from(saved_position.row).expect("saved cursor row is non-negative"),
                ) & mask,
                usize::try_from(saved_position.column)
                    .expect("saved cursor column is non-negative"),
            ),
            view: (self.view, 0),
        };
        let view_was_following = self.view_follows_offset();
        let mut builder = Builder::new(new_columns, origins);
        let chronological_start = self.scrollback_start(self.screen_rows);
        let mut saw_row = false;
        let mut last_linebreak = true;
        let mut pending_empty_linebreaks = 0;

        for distance in 0..self.rows.len() {
            let physical_row = chronological_start.wrapping_add(distance) & mask;
            let Some(row) = self.rows[physical_row].as_ref() else {
                continue;
            };
            builder
                .row_mappings
                .insert(self.row_ids[physical_row], builder.rows.len());
            saw_row = true;

            let tracker_column = [
                (physical_row == builder.origins.cursor.0).then_some(builder.origins.cursor.1),
                (physical_row == builder.origins.view.0).then_some(builder.origins.view.1),
            ]
            .into_iter()
            .flatten()
            .max()
            .map_or(0, |column| column + 1);
            // Unlike Foot, the saved cursor does not make an empty row count
            // as content. Restoring it is rare, but it stays wherever its row
            // went, so after a resize it can sit far below a screen cleared
            // since, which would then keep blank rows down to it and lose its
            // text into history when the pane shrinks.
            let saved_cursor_column = if physical_row == builder.origins.saved_cursor.0 {
                builder.origins.saved_cursor.1 + 1
            } else {
                0
            };

            let meaningful = row
                .cells()
                .iter()
                .rposition(|cell| {
                    !matches!(cell.content(), CellContent::Empty | CellContent::Spacer(0))
                })
                .map_or(0, |column| column + 1);
            let mut content_count = meaningful;
            if !row.has_linebreak() && meaningful > 0 {
                while content_count < row.len()
                    && row[content_count].content() == CellContent::Empty
                {
                    content_count += 1;
                }
            }
            let mut count = content_count.max(tracker_column);
            if count > 0 {
                count = count.max(saved_cursor_column);
            }
            let count = count.min(row.len());
            if count > 0 {
                last_linebreak = row.has_linebreak();
            }

            if count > 0 && pending_empty_linebreaks > 0 {
                for _ in 0..pending_empty_linebreaks {
                    builder.finish_hard_line();
                }
                pending_empty_linebreaks = 0;
            }

            let mut column = 0;
            while column < count {
                let (source_width, output) =
                    reflow_unit(row, column, count, new_columns, &composed_width);
                builder.append_unit(physical_row, column, source_width, &output);
                column += source_width;
            }

            if row.has_linebreak() {
                if count > 0 {
                    builder.finish_hard_line();
                } else {
                    pending_empty_linebreaks += 1;
                }
            }
        }

        if !builder.current.is_empty() {
            builder.finish(last_linebreak);
        } else if !saw_row {
            builder.finish_hard_line();
        }

        let trimmed = builder.rows.len().saturating_sub(new_row_capacity);
        if trimmed > 0 {
            builder.rows.drain(0..trimmed);
            adjust_mapping(&mut builder.mappings.cursor, trimmed);
            adjust_mapping(&mut builder.mappings.saved_cursor, trimmed);
            adjust_mapping(&mut builder.mappings.view, trimmed);
            builder.row_mappings.retain(|_, row| {
                let Some(adjusted) = row.checked_sub(trimmed) else {
                    return false;
                };
                *row = adjusted;
                true
            });
        }

        let output_rows = builder.rows.len();
        dimensions.screen_rows = new_screen_rows;
        for (index, row) in builder.rows.into_iter().enumerate() {
            dimensions.rows[index] = Some(row);
        }
        // As in Foot, the last reflowed row ends at the bottom of the screen
        // when there is enough content; otherwise the screen starts at the
        // first row, so a short screen keeps its text at the top and empty
        // rows below it, and none of it moves into history.
        dimensions.offset = output_rows.saturating_sub(new_screen_rows);
        for relative in 0..new_screen_rows {
            let index = dimensions.absolute_index(Grid::signed_row(relative));
            if dimensions.rows[index].is_none() {
                dimensions.rows[index] = Some(Row::new(new_columns));
            }
        }

        dimensions.view = if view_was_following {
            dimensions.offset
        } else {
            builder
                .mappings
                .view
                .map_or(dimensions.offset, |(row, _)| row)
        };
        let max_scrollback_view = new_row_capacity - new_screen_rows;
        let view_relative = dimensions.absolute_to_scrollback(new_screen_rows, dimensions.view);
        if view_relative > max_scrollback_view {
            dimensions.view =
                dimensions.scrollback_to_absolute(new_screen_rows, max_scrollback_view);
        }

        dimensions.cursor = remap_cursor(
            self.cursor,
            builder.mappings.cursor,
            dimensions.offset,
            new_row_capacity,
            new_columns,
            new_screen_rows,
        );
        // A saved cursor on a row that was not content keeps its screen row.
        let saved_cursor_mapping = builder.mappings.saved_cursor.or_else(|| {
            let row = usize::try_from(saved_position.row)
                .expect("saved cursor row is non-negative")
                .min(new_screen_rows - 1);
            let column = usize::try_from(saved_position.column)
                .expect("saved cursor column is non-negative");
            Some((dimensions.absolute_index(Grid::signed_row(row)), column))
        });
        dimensions.saved_cursor = remap_cursor(
            self.saved_cursor,
            saved_cursor_mapping,
            dimensions.offset,
            new_row_capacity,
            new_columns,
            new_screen_rows,
        );
        dimensions.generation = self
            .generation
            .checked_add(1)
            .expect("grid generation exhausted");
        dimensions.next_row_id = self.next_row_id;
        dimensions.history_generation = self
            .history_generation
            .checked_add(1)
            .expect("history generation exhausted");
        dimensions.reidentify_allocated_rows_chronologically();
        let row_mappings = builder
            .row_mappings
            .into_iter()
            .filter_map(|(old_id, row)| {
                let mapped = if row < output_rows {
                    Some(row)
                } else if output_rows < new_screen_rows {
                    // A dropped trailing empty row maps to the first empty
                    // row below the content.
                    Some(dimensions.absolute_index(Grid::signed_row(output_rows)))
                } else {
                    None
                }?;
                dimensions.rows[mapped]
                    .as_ref()
                    .map(|_| (old_id, dimensions.row_ids[mapped]))
            })
            .collect();

        *self = dimensions;
        row_mappings
    }
}

fn reflow_unit<F>(
    row: &Row,
    column: usize,
    count: usize,
    maximum_width: usize,
    composed_width: &F,
) -> (usize, Vec<Cell>)
where
    F: Fn(u32) -> usize,
{
    let leader = row[column];
    if matches!(leader.content(), CellContent::Spacer(remaining) if remaining > 0) {
        return (1, vec![Cell::default()]);
    }

    if let CellContent::Composed(key) = leader.content() {
        let width = composed_width(key).max(1);
        if width > maximum_width {
            return (width.min(count - column), Vec::new());
        }
        if width <= count - column
            && (1..width).all(|offset| {
                row[column + offset].content()
                    == CellContent::Spacer(u32::try_from(width - offset).unwrap())
            })
        {
            return (width, row.cells()[column..column + width].to_vec());
        }
        return (1, cells_for_composed_width(leader, width));
    }

    if column + 1 < count
        && let CellContent::Spacer(first) = row[column + 1].content()
        && first > 0
    {
        let width = usize::try_from(first)
            .unwrap_or(usize::MAX)
            .saturating_add(1);
        if width <= count - column
            && (1..width).all(|offset| {
                row[column + offset].content()
                    == CellContent::Spacer(u32::try_from(width - offset).unwrap())
            })
        {
            if width > maximum_width {
                return (width, Vec::new());
            }
            return (width, row.cells()[column..column + width].to_vec());
        }
    }

    (1, vec![leader])
}

fn cells_for_composed_width(leader: Cell, width: usize) -> Vec<Cell> {
    let mut output = Vec::with_capacity(width);
    output.push(leader);
    for remaining in (1..width).rev() {
        let Ok(remaining) = u32::try_from(remaining) else {
            return vec![Cell::default()];
        };
        output.push(Cell::new(CellContent::Spacer(remaining)));
    }
    output
}

fn adjust_mapping(mapping: &mut Option<(usize, usize)>, trimmed: usize) {
    *mapping = mapping.and_then(|(row, column)| row.checked_sub(trimmed).map(|row| (row, column)));
}

fn remap_cursor(
    mut cursor: crate::Cursor,
    mapping: Option<(usize, usize)>,
    offset: usize,
    capacity: usize,
    columns: usize,
    screen_rows: usize,
) -> crate::Cursor {
    let (absolute_row, mut column) = mapping.unwrap_or((offset, 0));
    let row = absolute_row.wrapping_sub(offset) & (capacity - 1);
    let row = row.min(screen_rows - 1);
    column = column.min(columns - 1);

    if cursor.deferred_wrap() && column + 1 < columns {
        column += 1;
        cursor.set_deferred_wrap(false);
    }
    cursor.set_position(Coordinate::new(
        i32::try_from(column).expect("grid columns fit in signed coordinates"),
        i32::try_from(row).expect("grid rows fit in signed coordinates"),
    ));
    cursor
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Color, Cursor, ScrollDirection, ScrollRegion};

    fn put_text(row: &mut Row, text: &str, linebreak: bool) {
        for (column, character) in text.chars().enumerate() {
            row[column].set_content(CellContent::Scalar(character));
        }
        row.set_linebreak(linebreak);
    }

    fn row_text(row: &Row) -> String {
        row.cells()
            .iter()
            .filter_map(|cell| match cell.content() {
                CellContent::Scalar(character) => Some(character),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn narrowing_and_widening_preserves_a_soft_wrapped_logical_line() {
        let mut grid = Grid::with_screen_size(8, 4, 2);
        put_text(grid.row_mut(0).unwrap(), "abcd", false);
        put_text(grid.row_mut(1).unwrap(), "ef", true);

        grid.resize_with_reflow(8, 3, 2, |_| 1);
        assert_eq!(row_text(grid.row(0).unwrap()), "abc");
        assert!(!grid.row(0).unwrap().has_linebreak());
        assert_eq!(row_text(grid.row(1).unwrap()), "def");
        assert!(grid.row(1).unwrap().has_linebreak());

        grid.resize_with_reflow(8, 6, 2, |_| 1);
        assert_eq!(row_text(grid.row(0).unwrap()), "abcdef");
        assert!(grid.row(0).unwrap().has_linebreak());
        assert_eq!(row_text(grid.row(1).unwrap()), "");
    }

    #[test]
    fn hard_linebreaks_are_not_joined_when_width_grows() {
        let mut grid = Grid::with_screen_size(8, 2, 2);
        put_text(grid.row_mut(0).unwrap(), "ab", true);
        put_text(grid.row_mut(1).unwrap(), "cd", true);

        grid.resize_with_reflow(8, 4, 2, |_| 1);

        assert_eq!(row_text(grid.row(0).unwrap()), "ab");
        assert_eq!(row_text(grid.row(1).unwrap()), "cd");
        assert!(grid.row(0).unwrap().has_linebreak());
        assert!(grid.row(1).unwrap().has_linebreak());
    }

    #[test]
    fn trailing_empty_hard_row_does_not_harden_preceding_soft_wrap() {
        let mut grid = Grid::with_screen_size(4, 2, 2);
        put_text(grid.row_mut(0).unwrap(), "ab", false);

        grid.resize_with_reflow(4, 2, 2, |_| 1);

        assert_eq!(row_text(grid.row(0).unwrap()), "ab");
        assert!(!grid.row(0).unwrap().has_linebreak());
    }

    #[test]
    fn soft_row_extension_stops_at_bare_spacer_padding() {
        let mut grid = Grid::with_screen_size(8, 4, 2);
        let first = grid.row_mut(0).unwrap();
        first[0].set_content(CellContent::Scalar('a'));
        first[2].set_content(CellContent::Spacer(0));
        first[3].set_content(CellContent::Spacer(0));
        first.set_linebreak(false);
        put_text(grid.row_mut(1).unwrap(), "b", true);

        grid.resize_with_reflow(8, 3, 2, |_| 1);

        let joined = grid.row(0).unwrap();
        assert_eq!(joined[0].content(), CellContent::Scalar('a'));
        assert_eq!(joined[1].content(), CellContent::Empty);
        assert_eq!(joined[2].content(), CellContent::Scalar('b'));
        assert!(joined.has_linebreak());
    }

    #[test]
    fn trailing_empty_hard_rows_leave_short_content_at_the_top() {
        let mut grid = Grid::with_screen_size(4, 2, 2);
        put_text(grid.row_mut(0).unwrap(), "ok", true);

        grid.resize_with_reflow(4, 2, 2, |_| 1);

        assert_eq!(row_text(grid.row(0).unwrap()), "ok");
        assert_eq!(row_text(grid.row(1).unwrap()), "");
    }

    #[test]
    fn internal_empty_hard_rows_are_preserved_before_later_content() {
        let mut grid = Grid::with_screen_size(8, 2, 3);
        put_text(grid.row_mut(0).unwrap(), "ab", true);
        put_text(grid.row_mut(2).unwrap(), "cd", true);

        grid.resize_with_reflow(8, 2, 3, |_| 1);

        assert_eq!(row_text(grid.row(0).unwrap()), "ab");
        assert_eq!(row_text(grid.row(1).unwrap()), "");
        assert_eq!(row_text(grid.row(2).unwrap()), "cd");
    }

    #[test]
    fn wide_cells_move_as_an_indivisible_unit() {
        let mut grid = Grid::with_screen_size(8, 4, 1);
        let row = grid.row_mut(0).unwrap();
        row[0].set_content(CellContent::Scalar('a'));
        row[1].set_content(CellContent::Scalar('界'));
        row[2].set_content(CellContent::Spacer(1));
        row[3].set_content(CellContent::Scalar('b'));
        row.set_linebreak(true);

        grid.resize_with_reflow(8, 2, 3, |_| 1);

        assert_eq!(grid.row(0).unwrap()[0].content(), CellContent::Scalar('a'));
        assert_eq!(grid.row(0).unwrap()[1].content(), CellContent::Spacer(0));
        assert_eq!(grid.row(1).unwrap()[0].content(), CellContent::Scalar('界'));
        assert_eq!(grid.row(1).unwrap()[1].content(), CellContent::Spacer(1));
        assert_eq!(grid.row(2).unwrap()[0].content(), CellContent::Scalar('b'));
        assert!((0..3).all(|row| grid.row(row).unwrap().has_valid_wide_cells()));
    }

    #[test]
    fn composed_width_resolver_can_create_or_discard_continuations() {
        let mut grid = Grid::with_screen_size(4, 2, 1);
        grid.row_mut(0).unwrap()[0].set_content(CellContent::Composed(7));

        grid.resize_with_reflow(4, 3, 1, |key| if key == 7 { 2 } else { 1 });
        assert_eq!(grid.row(0).unwrap()[0].content(), CellContent::Composed(7));
        assert_eq!(grid.row(0).unwrap()[1].content(), CellContent::Spacer(1));

        grid.resize_with_reflow(4, 1, 1, |_| usize::MAX);
        assert_eq!(grid.row(0).unwrap()[0].content(), CellContent::Empty);
    }

    #[test]
    fn cursor_and_deferred_wrap_follow_reflowed_content() {
        let mut grid = Grid::with_screen_size(8, 4, 2);
        put_text(grid.row_mut(0).unwrap(), "abcd", false);
        put_text(grid.row_mut(1).unwrap(), "ef", true);
        let mut cursor = Cursor::new(Coordinate::new(1, 1));
        cursor.set_deferred_wrap(true);
        grid.set_cursor(cursor);

        grid.resize_with_reflow(8, 4, 2, |_| 1);

        assert_eq!(grid.cursor().position(), Coordinate::new(2, 1));
        assert!(!grid.cursor().deferred_wrap());
    }

    #[test]
    fn randomized_grid_operations_preserve_core_invariants() {
        let mut grid = Grid::with_screen_size(16, 6, 4);
        let mut state = 0x1234_5678_u64;

        for step in 0..200 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let row = usize::try_from(state % grid.screen_rows() as u64).unwrap();
            let column = usize::try_from((state >> 8) % grid.columns() as u64).unwrap();
            let character = char::from(b'a' + u8::try_from(step % 26).unwrap());
            grid.row_mut(i32::try_from(row).unwrap()).unwrap()[column]
                .set_content(CellContent::Scalar(character));

            match (state >> 16) % 4 {
                0 => {
                    let rows = 1 + usize::try_from((state >> 24) % 4).unwrap();
                    grid.scroll(
                        ScrollDirection::Forward,
                        ScrollRegion::new(0, 4),
                        rows,
                        Color::default(),
                    );
                }
                1 => {
                    grid.resize_without_reflow(16, 6, 4);
                }
                2 => {
                    grid.resize_with_reflow(16, 5, 4, |_| 1);
                }
                _ => {
                    grid.resize_with_reflow(16, 6, 4, |_| 1);
                }
            }

            assert!(grid.offset() < grid.row_capacity());
            assert!(grid.view() < grid.row_capacity());
            assert!((0..grid.screen_rows()).all(|visible| {
                let row = grid.row(i32::try_from(visible).unwrap()).unwrap();
                row.len() == grid.columns() && row.has_valid_wide_cells()
            }));
            let cursor = grid.cursor().position();
            assert!(cursor.column >= 0 && usize::try_from(cursor.column).unwrap() < grid.columns());
            assert!(cursor.row >= 0 && usize::try_from(cursor.row).unwrap() < grid.screen_rows());
        }
    }
}
