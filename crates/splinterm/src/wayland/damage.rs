//! Pure draw gating, stale-region accumulation, and bounded backing-buffer copying.

use crate::{
    geometry::{Rect, WindowGeometry},
    renderer::snapshot_row_rect,
};

/// The surface damage a frame owes the terminal search field: where it is
/// drawn now, since its text changes as it is typed and searched, and where
/// it was last drawn when that differs. Without the second, a field closed
/// with Escape, hidden by a tab or focus change, or moved by a resize stays
/// on screen: the buffer is repainted, but the compositor is only told about
/// changed terminal rows and keeps showing the old pixels elsewhere.
pub(super) fn search_panel_damage(previous: Option<Rect>, current: Option<Rect>) -> Vec<Rect> {
    let mut rects: Vec<Rect> = current.into_iter().collect();
    if previous != current {
        rects.extend(previous);
    }
    rects
}

pub(super) fn terminal_draw_waits_for_frame(frame_pending: bool, _buffer_available: bool) -> bool {
    frame_pending
}

pub(super) fn pending_draw_waits_for_frame(frame_pending: bool, terminal_priority: bool) -> bool {
    frame_pending && !terminal_priority
}

pub(super) fn take_full_surface_damage(
    full_redraw: &mut bool,
    snapshot_frame_present: bool,
) -> bool {
    let damage_full_surface = !snapshot_frame_present || *full_redraw;
    *full_redraw = false;
    damage_full_surface
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum BackingDamage {
    Clean,
    Partial {
        dirty_rows: Vec<bool>,
        dirty_regions: Vec<Rect>,
    },
    Full,
}

impl BackingDamage {
    fn partial(&mut self) -> Option<(&mut Vec<bool>, &mut Vec<Rect>)> {
        if matches!(self, Self::Clean) {
            *self = Self::Partial {
                dirty_rows: Vec::new(),
                dirty_regions: Vec::new(),
            };
        }
        let Self::Partial {
            dirty_rows,
            dirty_regions,
        } = self
        else {
            return None;
        };
        Some((dirty_rows, dirty_regions))
    }

    pub(super) fn mark_rows(&mut self, rows: &[bool]) {
        if !rows.iter().any(|dirty| *dirty) {
            return;
        }
        let Some((dirty_rows, _)) = self.partial() else {
            return;
        };
        dirty_rows.resize(rows.len(), false);
        for (stale, dirty) in dirty_rows.iter_mut().zip(rows) {
            *stale |= *dirty;
        }
    }

    pub(super) fn mark_regions(&mut self, regions: &[Rect]) {
        if regions.is_empty() {
            return;
        }
        let Some((_, dirty_regions)) = self.partial() else {
            return;
        };
        for region in regions {
            if !dirty_regions.contains(region) {
                dirty_regions.push(*region);
            }
        }
    }

    pub(super) fn mark_full(&mut self) {
        *self = Self::Full;
    }
}

fn backing_row_damage_ranges(
    dirty_rows: &[bool],
    geometry: Option<&WindowGeometry>,
    height: u32,
    stride: usize,
    byte_limit: usize,
) -> Option<Vec<std::ops::Range<usize>>> {
    if !dirty_rows.iter().any(|dirty| *dirty) {
        return Some(Vec::new());
    }
    let geometry = geometry?;
    let mut ranges = Vec::new();
    for (row, dirty) in dirty_rows.iter().copied().enumerate() {
        if !dirty {
            continue;
        }
        let (_, y, _, row_height) = snapshot_row_rect(geometry, row)?;
        let top = u32::try_from(y).ok()?;
        let bottom = top
            .saturating_add(u32::try_from(row_height).ok()?)
            .min(height);
        let start = usize::try_from(top).ok()?.checked_mul(stride)?;
        let end = usize::try_from(bottom).ok()?.checked_mul(stride)?;
        if start > end || end > byte_limit {
            return None;
        }
        ranges.push(start..end);
    }
    Some(ranges)
}

fn backing_region_damage_ranges(
    regions: &[Rect],
    width: u32,
    height: u32,
    stride: usize,
    byte_limit: usize,
) -> Option<Vec<std::ops::Range<usize>>> {
    let mut ranges = Vec::new();
    for region in regions {
        let right = region.x.saturating_add(region.width).min(width);
        let bottom = region.y.saturating_add(region.height).min(height);
        if region.x >= right || region.y >= bottom {
            continue;
        }
        let left = usize::try_from(region.x).ok()?.checked_mul(4)?;
        let right = usize::try_from(right).ok()?.checked_mul(4)?;
        for y in region.y..bottom {
            let scanline = usize::try_from(y).ok()?.checked_mul(stride)?;
            let start = scanline.checked_add(left)?;
            let end = scanline.checked_add(right)?;
            if start > end || end > byte_limit {
                return None;
            }
            ranges.push(start..end);
        }
    }
    Some(ranges)
}

pub(super) fn sync_backing_damage(
    canvas: &mut [u8],
    backing: &[u8],
    width: u32,
    height: u32,
    geometry: Option<&WindowGeometry>,
    damage: &BackingDamage,
) -> usize {
    let full_copy = |canvas: &mut [u8]| {
        canvas.copy_from_slice(backing);
        backing.len()
    };
    let BackingDamage::Partial {
        dirty_rows,
        dirty_regions,
    } = damage
    else {
        return if matches!(damage, BackingDamage::Full) {
            full_copy(canvas)
        } else {
            0
        };
    };
    let Some(stride) = usize::try_from(width)
        .ok()
        .and_then(|width| width.checked_mul(4))
    else {
        return full_copy(canvas);
    };
    let byte_limit = backing.len().min(canvas.len());
    let Some(mut ranges) =
        backing_row_damage_ranges(dirty_rows, geometry, height, stride, byte_limit)
    else {
        return full_copy(canvas);
    };
    let Some(region_ranges) =
        backing_region_damage_ranges(dirty_regions, width, height, stride, byte_limit)
    else {
        return full_copy(canvas);
    };
    ranges.extend(region_ranges);
    ranges.sort_unstable_by_key(|range| range.start);
    let mut copied = 0;
    let mut previous_end = 0;
    for range in ranges {
        let start = range.start.max(previous_end);
        if start < range.end {
            canvas[start..range.end].copy_from_slice(&backing[start..range.end]);
            copied += range.end - start;
            previous_end = range.end;
        }
    }
    copied
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_panel_damage_covers_the_field_and_where_it_was() {
        let at = |x| Rect {
            x,
            y: 500,
            width: 440,
            height: 48,
        };
        // Closed with nothing drawn: no damage.
        assert!(search_panel_damage(None, None).is_empty());
        // Opened, or open and typed into: the field itself.
        assert_eq!(search_panel_damage(None, Some(at(8))), vec![at(8)]);
        assert_eq!(search_panel_damage(Some(at(8)), Some(at(8))), vec![at(8)]);
        // Closed: where it was, so the compositor drops the old pixels.
        assert_eq!(search_panel_damage(Some(at(8)), None), vec![at(8)]);
        // Moved, as by a resize or a docked Explorer: both places.
        assert_eq!(
            search_panel_damage(Some(at(8)), Some(at(300))),
            vec![at(300), at(8)]
        );
    }

    #[test]
    fn terminal_draws_obey_the_compositor_frame_boundary() {
        assert!(!terminal_draw_waits_for_frame(false, false));
        assert!(!terminal_draw_waits_for_frame(false, true));
        assert!(terminal_draw_waits_for_frame(true, true));
        assert!(terminal_draw_waits_for_frame(true, false));
    }

    #[test]
    fn terminal_priority_retries_through_the_frame_gated_scheduler() {
        assert!(!pending_draw_waits_for_frame(false, false));
        assert!(!pending_draw_waits_for_frame(false, true));
        assert!(pending_draw_waits_for_frame(true, false));
        assert!(!pending_draw_waits_for_frame(true, true));
    }

    #[test]
    fn full_redraw_damage_survives_redraw_state_reset() {
        let mut full_redraw = true;
        assert!(take_full_surface_damage(&mut full_redraw, true));
        assert!(!full_redraw);

        assert!(!take_full_surface_damage(&mut full_redraw, true));
        assert!(take_full_surface_damage(&mut full_redraw, false));
    }
}
