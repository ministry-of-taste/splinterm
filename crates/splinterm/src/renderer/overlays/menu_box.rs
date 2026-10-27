//! The tab and Explorer context menus in the popup box (Plan 0065
//! milestone 4): at the pointer, with what they act on in the frame,
//! one-line rows with their key chips, separators between groups, and
//! Terminate last, in danger. No scrim and no legend: menus are
//! pointer-first, and the arrows, Enter and Escape still work.

use anyhow::Result;

use crate::{
    frontend::{
        TabContextMenuUi, TabMenuActionDescriptor, TabMenuActionId, shortcut_chips,
        tab_menu_starts_group,
    },
    geometry::Rect,
    keymap::ResolvedKeymap,
};

use super::{
    super::{ChromeTextStyle, RenderContext},
    chrome_box::{BOX_CLEARANCE, BOX_EDGE, BOX_SHADOW_OFFSET, ChromeBox, ChromePainter, opaque},
    chrome_rows::{
        ListRow, ROW_HEIGHT, ROW_PADDING, RowState, chips_width, paint_row, row_columns, text_cells,
    },
    picker::{SessionPickerPalette, SessionPickerTextCache},
};

/// A menu is at least this wide, logical px.
const MENU_MINIMUM_WIDTH: u32 = 156;
/// Logical px under the last row, inside the frame, which holds no legend.
const MENU_BOTTOM_CLEARANCE: u32 = 6;
/// Logical px above and below a separator's 1 px rule.
const SEPARATOR_GAP: u32 = 4;
/// A separator's band: its rule and the gaps either side.
const SEPARATOR_HEIGHT: u32 = SEPARATOR_GAP * 2 + 1;
/// Logical px between the pointer and a menu that opens below it.
const MENU_ANCHOR_GAP: u32 = 4;
/// Logical px at least between a label and its chips.
const MENU_CHIP_GAP: u32 = 24;

/// One menu row: its action, its label and its binding's chips.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TabMenuRowLayout {
    pub(crate) action: TabMenuActionId,
    pub(crate) label: &'static str,
    pub(crate) chips: Vec<String>,
    pub(crate) rect: Rect,
}

/// Where a menu's parts go, in logical px.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TabContextMenuLayout {
    pub(crate) panel: Rect,
    pub(crate) rows: Vec<TabMenuRowLayout>,
    /// Each separator's 1 px rule across the rows; the painter starts it
    /// at the labels.
    pub(crate) separators: Vec<Rect>,
}

/// The chips of the binding that does `action` to the current Dojo, Lair
/// or pane, or none.
fn action_chips(action: TabMenuActionId, keymap: &ResolvedKeymap) -> Vec<String> {
    shortcut_chips(action.shortcut(keymap))
}

/// The widest of `actions`' rows in logical px: the padding, the marker
/// column, the label, and its chips at least a gap after it. A row with
/// chips is also at least twice as wide as them, since a row gives its
/// chips at most half its room.
pub(crate) fn tab_menu_content_width(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    actions: &[TabMenuActionDescriptor],
    keymap: &ResolvedKeymap,
    scale_120: u32,
    renderer_generation: u64,
) -> Result<u32> {
    let cell = cache.cell_metrics(
        context,
        ChromeTextStyle::Regular,
        scale_120,
        renderer_generation,
    )?;
    let scaled = |logical: u32| logical.saturating_mul(scale_120).div_ceil(120);
    let gap = scaled(MENU_CHIP_GAP).max(cell.0);
    let widest = actions
        .iter()
        .map(|action| {
            let label = text_cells(action.title).saturating_mul(cell.0);
            let chips = action_chips(action.id, keymap);
            let needed = if chips.is_empty() {
                label
            } else {
                let chips = chips_width(&chips, cell, scale_120);
                label
                    .saturating_add(gap)
                    .saturating_add(chips)
                    .max(chips.saturating_mul(2))
            };
            needed
                .saturating_add(scaled(ROW_PADDING).saturating_mul(2))
                .saturating_add(cell.0.saturating_mul(2))
        })
        .max()
        .unwrap_or(0);
    Ok(widest.saturating_mul(120).div_ceil(scale_120.max(1)))
}

/// The menu at `anchor` in `bounds` (logical px), `content_width` wide
/// inside its frame and at least [`MENU_MINIMUM_WIDTH`] in all: below the
/// pointer when it fits, else above it, clamped inside `bounds` with its
/// shadow. `None` when `bounds` cannot hold every row.
#[must_use]
pub(crate) fn tab_context_menu_layout(
    bounds: Rect,
    anchor: (u32, u32),
    actions: &[TabMenuActionDescriptor],
    keymap: &ResolvedKeymap,
    content_width: u32,
) -> Option<TabContextMenuLayout> {
    let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    let separator_count = (0..actions.len())
        .filter(|index| tab_menu_starts_group(actions, *index))
        .count();
    let rows_height = ROW_HEIGHT
        .saturating_mul(count(actions.len()))
        .saturating_add(SEPARATOR_HEIGHT.saturating_mul(count(separator_count)));
    let panel_height = BOX_EDGE
        .saturating_mul(2)
        .saturating_add(BOX_CLEARANCE)
        .saturating_add(MENU_BOTTOM_CLEARANCE)
        .saturating_add(rows_height);
    if actions.is_empty()
        || bounds.width <= BOX_EDGE.saturating_mul(2).saturating_add(BOX_SHADOW_OFFSET)
        || bounds.height < panel_height.saturating_add(BOX_SHADOW_OFFSET)
    {
        return None;
    }
    let panel_width = content_width
        .saturating_add(BOX_EDGE.saturating_mul(2))
        .max(MENU_MINIMUM_WIDTH)
        .min(bounds.width - BOX_SHADOW_OFFSET);
    let maximum_x = bounds
        .x
        .saturating_add(bounds.width)
        .saturating_sub(panel_width)
        .saturating_sub(BOX_SHADOW_OFFSET);
    let x = anchor.0.min(maximum_x).max(bounds.x);
    let below = anchor.1.saturating_add(MENU_ANCHOR_GAP);
    let maximum_y = bounds
        .y
        .saturating_add(bounds.height)
        .saturating_sub(panel_height)
        .saturating_sub(BOX_SHADOW_OFFSET);
    let y = if below <= maximum_y {
        below.max(bounds.y)
    } else {
        anchor
            .1
            .saturating_sub(panel_height)
            .max(bounds.y)
            .min(maximum_y)
    };
    let inner_x = x.saturating_add(BOX_EDGE);
    let inner_width = panel_width.saturating_sub(BOX_EDGE.saturating_mul(2));
    let mut row_y = y.saturating_add(BOX_EDGE).saturating_add(BOX_CLEARANCE);
    let mut rows = Vec::with_capacity(actions.len());
    let mut separators = Vec::with_capacity(separator_count);
    for (index, action) in actions.iter().enumerate() {
        if tab_menu_starts_group(actions, index) {
            separators.push(Rect {
                x: inner_x,
                y: row_y.saturating_add(SEPARATOR_GAP),
                width: inner_width,
                height: 1,
            });
            row_y = row_y.saturating_add(SEPARATOR_HEIGHT);
        }
        rows.push(TabMenuRowLayout {
            action: action.id,
            label: action.title,
            chips: action_chips(action.id, keymap),
            rect: Rect {
                x: inner_x,
                y: row_y,
                width: inner_width,
                height: ROW_HEIGHT,
            },
        });
        row_y = row_y.saturating_add(ROW_HEIGHT);
    }
    Some(TabContextMenuLayout {
        panel: Rect {
            x,
            y,
            width: panel_width,
            height: panel_height,
        },
        rows,
        separators,
    })
}

#[must_use]
pub(crate) fn tab_context_menu_hit_test(
    layout: &TabContextMenuLayout,
    point: (f64, f64),
) -> Option<TabMenuActionId> {
    layout
        .rows
        .iter()
        .find(|row| contains(row.rect, point))
        .map(|row| row.action)
}

/// Whether `point` is in `rect`'s half-open area.
fn contains(rect: Rect, point: (f64, f64)) -> bool {
    point.0 >= f64::from(rect.x)
        && point.1 >= f64::from(rect.y)
        && point.0 < f64::from(rect.x) + f64::from(rect.width)
        && point.1 < f64::from(rect.y) + f64::from(rect.height)
}

/// A menu in one box over the Window: the target's name in the frame, the
/// rows on the selection and pointer fills with their chips, a disabled
/// row in muted text, Terminate in danger, and the separators from the
/// labels' column to the rows' padding.
#[allow(
    clippy::too_many_arguments,
    reason = "one painter takes the menu's state, pointer and focus together"
)]
pub(crate) fn paint_tab_context_menu(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    canvas: &mut [u8],
    width: u32,
    height: u32,
    scale_120: u32,
    renderer_generation: u64,
    layout: &TabContextMenuLayout,
    palette: SessionPickerPalette,
    state: &TabContextMenuUi,
    pressed: Option<TabMenuActionId>,
    keyboard_focused: bool,
) -> Result<()> {
    let mut painter = ChromePainter::new(
        cache,
        context,
        canvas,
        width,
        height,
        scale_120,
        renderer_generation,
    );
    let cell = painter.box_cell()?;
    let chrome = ChromeBox {
        title: state.title(),
        right_label: &[],
        right_active: None,
        legend: &[],
    }
    .layout(painter.buffer_rect(layout.panel), scale_120, cell);
    chrome.paint_surface(&mut painter, &palette);
    let selected = state.selected_action();
    for row in &layout.rows {
        let enabled = state.action_enabled(row.action);
        let rect = painter.buffer_rect(row.rect);
        paint_row(
            &mut painter,
            &palette,
            rect,
            &ListRow {
                title: row.label,
                matched: None,
                marker: None,
                chips: &row.chips,
                chip_column: false,
                trailing: "",
                trailing_accent: false,
                state: RowState {
                    selected: enabled && selected == row.action,
                    hovered: enabled && state.hovered() == Some(row.action),
                    pressed: enabled && pressed == Some(row.action),
                    disabled: !enabled,
                    // Terminate Lair and Terminate Dojo end running terminals.
                    danger: enabled && row.action.destructive(),
                },
            },
        )?;
    }
    let regular = painter.cell(ChromeTextStyle::Regular)?;
    let padding = painter.scaled(ROW_PADDING);
    for separator in &layout.separators {
        let band = painter.buffer_rect(*separator);
        // The rule spans what a row's title column would.
        let rule = row_columns(band, padding, regular.0, 0, 0).title;
        painter.fill(rule, opaque(palette.frame));
    }
    chrome.paint_frame(
        &mut painter,
        &palette,
        if keyboard_focused {
            palette.focused_frame
        } else {
            palette.frame
        },
    )?;
    painter.finish();
    Ok(())
}

#[cfg(test)]
mod tests {
    use splinterm_core::{DojoId, LairId, SplintId};

    use super::{
        super::{chrome_rows::chip_layout, picker::picker_buffer_rect as buffer_rect},
        *,
    };
    use crate::{
        config::ResolvedTheme,
        frontend::{TAB_MENU_ACTIONS, TabMenuContext, tab_menu_descriptor},
        renderer::session_picker_palette,
    };

    const SCALES: [u32; 4] = [120, 150, 180, 240];
    const BOUNDS: Rect = Rect {
        x: 0,
        y: 0,
        width: 640,
        height: 400,
    };

    fn every_action() -> Vec<TabMenuActionDescriptor> {
        TAB_MENU_ACTIONS
            .into_iter()
            .chain(
                [
                    TabMenuActionId::RenameLair,
                    TabMenuActionId::SaveLayout,
                    TabMenuActionId::PinLair,
                    TabMenuActionId::UnpinLair,
                    TabMenuActionId::PreviewLair,
                    TabMenuActionId::Restore,
                    TabMenuActionId::TerminateLair,
                    TabMenuActionId::FocusSplint,
                    TabMenuActionId::SplitBelow,
                    TabMenuActionId::SplitRight,
                    TabMenuActionId::CloseSplint,
                ]
                .map(tab_menu_descriptor),
            )
            .collect()
    }

    fn tab_menu(active: bool) -> TabContextMenuUi {
        TabContextMenuUi::new(TabMenuContext {
            lair_id: LairId::new(),
            source_splint_id: SplintId::new(),
            dojo_id: DojoId::new(),
            dojo_name: "Dojo 1".to_owned(),
            pane_count: 2,
            splints: vec![(SplintId::new(), 1), (SplintId::new(), 2)],
            active,
            other_dojo_ids: vec![DojoId::new()],
        })
    }

    /// Every label and its chips fit their row whole, at every scale, and
    /// a menu is never narrower than 156.
    #[test]
    fn menus_fit_their_widest_row_and_are_at_least_156_wide() {
        let context = RenderContext::new(u16::MAX);
        let keymap = ResolvedKeymap::default();
        let mut cache = SessionPickerTextCache::default();
        for scale in SCALES {
            let cell = cache
                .cell_metrics(&context, ChromeTextStyle::Regular, scale, 1)
                .unwrap();
            for action in every_action() {
                let width =
                    tab_menu_content_width(&mut cache, &context, &[action], &keymap, scale, 1)
                        .unwrap();
                let layout =
                    tab_context_menu_layout(BOUNDS, (600, 390), &[action], &keymap, width).unwrap();
                assert!(layout.panel.width >= MENU_MINIMUM_WIDTH);
                let row = &layout.rows[0];
                let rect = buffer_rect(row.rect, scale);
                let chips = chips_width(&row.chips, cell, scale);
                let columns = row_columns(
                    rect,
                    ROW_PADDING.saturating_mul(scale).div_ceil(120),
                    cell.0,
                    0,
                    chips,
                );
                assert!(
                    columns.title.width >= text_cells(row.label) * cell.0,
                    "{} at scale {scale}",
                    row.label
                );
                let placed = chip_layout(&row.chips, columns.trailing, cell, scale, true);
                assert_eq!(placed.first, 0, "{} at scale {scale}", row.label);
                assert!(placed.ellipsis.is_none(), "{} at scale {scale}", row.label);
            }
        }
        // A short label with no binding keeps the minimum width.
        let focus = [tab_menu_descriptor(TabMenuActionId::FocusSplint)];
        let width = tab_menu_content_width(&mut cache, &context, &focus, &keymap, 120, 1).unwrap();
        let layout = tab_context_menu_layout(BOUNDS, (0, 0), &focus, &keymap, width).unwrap();
        assert_eq!(layout.panel.width, MENU_MINIMUM_WIDTH);
        // A row with chips is wider than its label alone.
        let split = [tab_menu_descriptor(TabMenuActionId::SplitBelow)];
        assert_eq!(
            tab_context_menu_layout(BOUNDS, (0, 0), &split, &keymap, 0)
                .unwrap()
                .rows[0]
                .chips,
            ["ctrl", "shift", "↵"]
        );
        assert!(
            tab_menu_content_width(&mut cache, &context, &split, &keymap, 120, 1).unwrap()
                > text_cells("Split below") * cell_width(&mut cache, &context)
        );
    }

    fn cell_width(cache: &mut SessionPickerTextCache, context: &RenderContext) -> u32 {
        cache
            .cell_metrics(context, ChromeTextStyle::Regular, 120, 1)
            .unwrap()
            .0
    }

    /// The tab menu: three groups, two separators between them, Terminate
    /// last; the rows and separators stack inside the frame, and only rows
    /// are hit, half-open.
    #[test]
    fn separators_group_the_rows_and_only_rows_are_hit() {
        let keymap = ResolvedKeymap::default();
        let layout =
            tab_context_menu_layout(BOUNDS, (200, 40), &TAB_MENU_ACTIONS, &keymap, 0).unwrap();
        assert_eq!(layout.rows.len(), 6);
        assert_eq!(layout.separators.len(), 2);
        assert_eq!(
            layout.rows.last().map(|row| row.action),
            Some(TabMenuActionId::TerminateDojo)
        );
        assert_eq!(
            layout.rows[0].rect.y,
            layout.panel.y + BOX_EDGE + BOX_CLEARANCE
        );
        for (separator, after) in layout.separators.iter().zip([3, 5]) {
            let above = layout.rows[after - 1].rect;
            let below = layout.rows[after].rect;
            assert_eq!(separator.y, above.y + above.height + SEPARATOR_GAP);
            assert_eq!(below.y, separator.y + 1 + SEPARATOR_GAP);
            let on_rule = (
                f64::from(separator.x + separator.width / 2),
                f64::from(separator.y),
            );
            assert_eq!(tab_context_menu_hit_test(&layout, on_rule), None);
        }
        let last = layout.rows.last().unwrap().rect;
        assert_eq!(
            layout.panel.y + layout.panel.height,
            last.y + last.height + MENU_BOTTOM_CLEARANCE + BOX_EDGE
        );
        // The chips come from the keymap.
        let close = &layout.rows[3];
        assert_eq!(close.label, "Close tab");
        assert_eq!(close.chips, ["ctrl", "shift", "q"]);
        assert!(
            layout.rows[1].chips.is_empty(),
            "Activate tab has no binding"
        );
        for row in &layout.rows {
            assert_eq!(row.rect.height, ROW_HEIGHT);
            assert_eq!(row.rect.x, layout.panel.x + BOX_EDGE);
            assert_eq!(row.rect.width, layout.panel.width - BOX_EDGE * 2);
            let y = f64::from(row.rect.y);
            assert_eq!(
                tab_context_menu_hit_test(&layout, (f64::from(row.rect.x), y)),
                Some(row.action)
            );
            assert_eq!(
                tab_context_menu_hit_test(&layout, (f64::from(row.rect.x + row.rect.width), y)),
                None
            );
        }
    }

    /// At every corner and in every narrow viewport the menu and its
    /// shadow stay inside, or there is no menu.
    #[test]
    fn menus_clamp_inside_the_window() {
        let keymap = ResolvedKeymap::default();
        for anchor in [(0, 0), (639, 0), (0, 399), (639, 399)] {
            let layout =
                tab_context_menu_layout(BOUNDS, anchor, &TAB_MENU_ACTIONS, &keymap, 0).unwrap();
            assert_eq!(layout.panel.width, MENU_MINIMUM_WIDTH);
            assert!(layout.panel.x + layout.panel.width + BOX_SHADOW_OFFSET <= BOUNDS.width);
            assert!(layout.panel.y + layout.panel.height + BOX_SHADOW_OFFSET <= BOUNDS.height);
        }
        for width in [1, 6, 20, 26, 27, 40, 156, 161, 162, 240, 640] {
            let bounds = Rect {
                x: 17,
                y: 23,
                width,
                height: 400,
            };
            for anchor in [(0, 0), (bounds.x + width, bounds.y + bounds.height)] {
                let Some(layout) =
                    tab_context_menu_layout(bounds, anchor, &TAB_MENU_ACTIONS, &keymap, 400)
                else {
                    assert!(width <= BOX_EDGE * 2 + BOX_SHADOW_OFFSET, "width {width}");
                    continue;
                };
                assert!(layout.panel.x >= bounds.x);
                assert!(
                    layout.panel.x + layout.panel.width + BOX_SHADOW_OFFSET <= bounds.x + width
                );
                for row in &layout.rows {
                    assert!(row.rect.width > 0);
                    assert_eq!(
                        tab_context_menu_hit_test(
                            &layout,
                            (f64::from(row.rect.x), f64::from(row.rect.y))
                        ),
                        Some(row.action)
                    );
                }
            }
        }
        // Too short for every row: no menu.
        let short = Rect {
            height: 120,
            ..BOUNDS
        };
        assert!(tab_context_menu_layout(short, (0, 0), &TAB_MENU_ACTIONS, &keymap, 0).is_none());
    }

    /// No scrim: nothing outside the menu and its shadow is painted. The
    /// frame names the Dojo; the selected row is on the selection fill;
    /// the separator is a rule from the labels' column.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one paint, pointer and narrow viewport scenario"
    )]
    fn the_menu_paints_in_its_box_without_a_scrim() {
        let context = RenderContext::new(u16::MAX);
        let keymap = ResolvedKeymap::default();
        let palette = session_picker_palette(ResolvedTheme::default());
        let mut cache = SessionPickerTextCache::default();
        let mut state = tab_menu(false);
        let content_width =
            tab_menu_content_width(&mut cache, &context, state.actions(), &keymap, 120, 1).unwrap();
        let layout =
            tab_context_menu_layout(BOUNDS, (240, 40), state.actions(), &keymap, content_width)
                .unwrap();
        let mut canvas = vec![0_u8; 640 * 400 * 4];
        let pixel = |canvas: &[u8], x: u32, y: u32| {
            let index = usize::try_from((y * 640 + x) * 4).unwrap();
            [canvas[index + 2], canvas[index + 1], canvas[index]]
        };
        let rgb = |color: u32| {
            let [_, red, green, blue] = color.to_be_bytes();
            [red, green, blue]
        };
        for (hovered, pressed) in [
            (None, None),
            (Some(TabMenuActionId::CloseTab), None),
            (
                Some(TabMenuActionId::CloseTab),
                Some(TabMenuActionId::CloseTab),
            ),
        ] {
            state.update_hovered(hovered);
            canvas.fill(0);
            paint_tab_context_menu(
                &mut cache,
                &context,
                &mut canvas,
                640,
                400,
                120,
                1,
                &layout,
                palette,
                &state,
                pressed,
                true,
            )
            .unwrap();
            assert_eq!(&canvas[..4], &[0, 0, 0, 0], "no scrim");
            for text in [
                "Dojo 1",
                "Rename tab…",
                "Close other tabs",
                "Terminate Dojo…",
                "ctrl",
                "shift",
                "q",
            ] {
                assert!(cache.contains_source(text), "{text} was not drawn");
            }
            let selected = layout.rows[0].rect;
            assert_eq!(state.selected_action(), layout.rows[0].action);
            assert_eq!(
                pixel(&canvas, selected.x + selected.width - 4, selected.y + 2),
                rgb(palette.selected_fill)
            );
            let separator = layout.separators[0];
            assert_eq!(
                pixel(&canvas, separator.x + separator.width / 2, separator.y),
                rgb(palette.frame)
            );
            // The rule starts at the labels, not at the frame.
            assert_ne!(
                pixel(&canvas, separator.x + 1, separator.y),
                rgb(palette.frame)
            );
        }
        // A narrow viewport paints nothing past its edge.
        for viewport_width in [27, 40, 120] {
            let narrow = Rect {
                width: viewport_width,
                ..BOUNDS
            };
            let layout =
                tab_context_menu_layout(narrow, (0, 0), state.actions(), &keymap, 400).unwrap();
            canvas.fill(0);
            paint_tab_context_menu(
                &mut cache,
                &context,
                &mut canvas,
                640,
                400,
                120,
                1,
                &layout,
                palette,
                &state,
                None,
                true,
            )
            .unwrap();
            for y in 0..400 {
                let outside = (y * 640 + viewport_width as usize) * 4;
                let end = (y + 1) * 640 * 4;
                assert!(canvas[outside..end].iter().all(|byte| *byte == 0));
            }
        }
    }
}
