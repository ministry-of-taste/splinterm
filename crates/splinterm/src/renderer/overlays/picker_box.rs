//! The pickers in the popup box (Plan 0065 milestone 3): the `dojos`,
//! `lairs` and `remotes` lists, placed, framed and typed into like the
//! command palette.

use anyhow::Result;
use splinterm_protocol::AgentState;

use crate::{
    frontend::{BuiltInCommandId, PickerHitTarget, SessionPickerItem, SessionPickerUi, state_tint},
    geometry::Rect,
};

use super::{
    super::{ChromeTextStyle, RenderContext},
    actions::{
        CommandPaletteLayout, PaletteShape, command_palette_layout, match_count_label,
        paint_palette_input, title_column,
    },
    chrome_box::{ChromeBox, ChromePainter, opaque, paint_scrim},
    chrome_rows::{
        ListRow, PickerItemRow, RowState, paint_group_header, paint_picker_item_row, paint_row,
        title_match,
    },
    picker::{
        PickerTextAlignment, SessionPickerPalette, SessionPickerPurpose, SessionPickerTextCache,
    },
};

/// The creation row's key: Ctrl+N wherever a picker offers one.
const CREATE_CHIPS: [&str; 2] = ["ctrl", "n"];
/// The `dojos` picker's scopes, in the order Tab steps through them.
const SCOPES: [&str; 2] = ["recent", "this lair"];

/// Whether a line under the creation row says there is nothing to list:
/// only when creation is open, so it never repeats a blocker the row shows.
fn shows_empty_line(picker: &SessionPickerUi) -> bool {
    picker.create_row_shown() && picker.items().next().is_none() && picker.new_enabled()
}

/// The rows the list lays out: the picker's listed rows, and the empty
/// line when it shows.
fn layout_rows(picker: &SessionPickerUi) -> usize {
    picker
        .list_len()
        .saturating_add(usize::from(shows_empty_line(picker)))
}

/// The picker's box in `content` (logical px): the command palette's
/// width and placement, one 28 px row per listed row and its group
/// headers, ten rows at most.
#[must_use]
pub(crate) fn session_picker_layout(
    content: Rect,
    picker: &SessionPickerUi,
) -> Option<CommandPaletteLayout> {
    let rows = layout_rows(picker);
    let placeholders = vec![BuiltInCommandId::RecentSessions; rows];
    let mut groups = picker.row_groups();
    groups.resize(if groups.is_empty() { 0 } else { rows }, "");
    command_palette_layout(
        content,
        &placeholders,
        &groups,
        picker.list_selected(),
        picker.visible_start(),
        PaletteShape::Commands,
    )
}

/// The keys the picker answers, set into the bottom frame: while a query
/// filters, only what Enter and Escape do.
fn legend(purpose: SessionPickerPurpose, filtering: bool) -> Vec<(&'static str, &'static str)> {
    let mut legend = Vec::with_capacity(4);
    if !filtering {
        legend.push(("↑↓", "move"));
    }
    legend.push(("↵", purpose.selection_verb()));
    if !filtering && purpose.scope().is_some() {
        legend.push(("tab", "scope"));
    }
    legend.push(("esc", "close"));
    legend
}

/// Whether an agent's state is said in its own colour: the ones that ask
/// for attention.
const fn tinted_word(state: AgentState, is_stale: bool) -> bool {
    !is_stale && matches!(state, AgentState::NeedsInput | AgentState::Error)
}

/// A picker in one box: the title and its scope or count in the frame,
/// the input line and its rule, the creation row with its chip, the
/// items, and the keys in the bottom frame.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_session_picker(
    cache: &mut SessionPickerTextCache,
    context: &RenderContext,
    canvas: &mut [u8],
    width: u32,
    height: u32,
    content_rect: Rect,
    scale_120: u32,
    renderer_generation: u64,
    layout: &CommandPaletteLayout,
    palette: SessionPickerPalette,
    picker: &SessionPickerUi,
    purpose: SessionPickerPurpose,
    pressed: Option<PickerHitTarget>,
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
    let scrim = painter.buffer_rect(content_rect);
    paint_scrim(&mut painter, scrim, &palette);
    let cell = painter.box_cell()?;
    let query = picker.query();
    let count = match_count_label(picker.items().count(), picker.total(), query);
    let scope = purpose.scope().filter(|_| query.is_empty());
    let count_part = [count.as_str()];
    let right_label: &[&str] = if scope.is_some() {
        &SCOPES
    } else {
        &count_part
    };
    let legend = legend(purpose, !query.is_empty());
    let chrome = ChromeBox {
        title: purpose.title(),
        right_label,
        right_active: scope,
        legend: &legend,
    }
    .layout(painter.buffer_rect(layout.panel), scale_120, cell);
    chrome.paint_surface(&mut painter, &palette);
    paint_palette_input(&mut painter, layout, &palette, query, purpose.placeholder())?;
    let rule = painter.buffer_rect(layout.rule);
    painter.fill(rule, opaque(palette.frame));
    paint_picker_rows(&mut painter, layout, &palette, picker, purpose, pressed)?;
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

/// The listed rows with their group headers, or the line that says none
/// match.
fn paint_picker_rows(
    painter: &mut ChromePainter<'_>,
    layout: &CommandPaletteLayout,
    palette: &SessionPickerPalette,
    picker: &SessionPickerUi,
    purpose: SessionPickerPurpose,
    pressed: Option<PickerHitTarget>,
) -> Result<()> {
    if picker.list_len() == 0 && layout.list.height > 0 {
        let list = painter.buffer_rect(layout.list);
        let row = Rect {
            height: painter
                .scaled(super::chrome_rows::ROW_HEIGHT)
                .min(list.height),
            ..list
        };
        let clip = title_column(painter, row)?;
        painter.text(
            purpose.no_match(),
            ChromeTextStyle::Regular,
            clip,
            PickerTextAlignment::Left,
            palette.secondary,
        )?;
    }
    let chips = CREATE_CHIPS.map(str::to_owned);
    let query = picker.query();
    let selected = picker.selected_target();
    for (offset, row) in layout.rows.iter().enumerate() {
        if let Some((header, label)) = row.header {
            let header = painter.buffer_rect(header);
            paint_group_header(painter, palette, header, label)?;
        }
        let rect = painter.buffer_rect(row.rect);
        let index = layout.visible_start.saturating_add(offset);
        let Some(target) = picker.list_target(index) else {
            if shows_empty_line(picker) {
                let clip = title_column(painter, rect)?;
                painter.text(
                    purpose.empty_label(),
                    ChromeTextStyle::Regular,
                    clip,
                    PickerTextAlignment::Left,
                    palette.secondary,
                )?;
            }
            continue;
        };
        let enabled = picker.decision_for_target(target).is_some();
        let state = RowState {
            selected: enabled && target == selected,
            hovered: enabled && picker.hovered() == Some(target),
            pressed: enabled && pressed == Some(target),
            disabled: !enabled,
            danger: false,
        };
        match target {
            PickerHitTarget::New => {
                let blocker = picker.new_blocker().unwrap_or("");
                paint_row(
                    painter,
                    palette,
                    rect,
                    &ListRow {
                        title: purpose.create_title(),
                        matched: None,
                        marker: Some("+"),
                        chips: if picker.new_enabled() { &chips } else { &[] },
                        chip_column: false,
                        trailing: if picker.new_enabled() { "" } else { blocker },
                        trailing_accent: false,
                        state,
                    },
                )?;
            }
            PickerHitTarget::Open(item) => {
                if let Some(item) = picker.item(item) {
                    paint_picker_item(painter, palette, rect, item, query, state)?;
                }
            }
        }
    }
    Ok(())
}

/// One item row: its name with the query's characters marked, where it
/// is, and its status with the agent's dot in the state's colour.
fn paint_picker_item(
    painter: &mut ChromePainter<'_>,
    palette: &SessionPickerPalette,
    rect: Rect,
    item: &SessionPickerItem,
    query: &str,
    state: RowState,
) -> Result<()> {
    let status = item.status();
    let agent = status.as_ref().and_then(|status| status.agent);
    let dot = agent.map(|(agent_state, stale)| {
        palette
            .row_tints(state.selected)
            .map_or(palette.secondary, |tints| {
                tints.color(state_tint(agent_state, stale))
            })
    });
    let location = item.location();
    paint_picker_item_row(
        painter,
        palette,
        rect,
        &PickerItemRow {
            name: &item.display_title,
            matched: title_match(&item.display_title, query),
            location: &location,
            status: status.as_ref().map_or("", |status| status.word.as_str()),
            dot,
            status_tinted: agent
                .is_some_and(|(agent_state, stale)| tinted_word(agent_state, stale)),
            state,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::ResolvedTheme,
        renderer::{ChromeSurface, chrome_panel_palette, command_palette_row_hit_test},
    };

    fn item(title: &str, lair: &str, running: usize, panes: usize) -> SessionPickerItem {
        SessionPickerItem {
            display_title: title.to_owned(),
            breadcrumb: format!("{lair} / {title}"),
            working_directory: format!("~/src/{title}"),
            pane_count: panes,
            running_pane_count: running,
            agent: None,
        }
    }

    fn dojos() -> SessionPickerUi {
        let mut agent = item("agent", "Lair 1", 1, 1);
        agent.agent = Some((AgentState::NeedsInput, false));
        let mut build = item("build", "m5a", 1, 1);
        build.agent = Some((AgentState::Working, false));
        SessionPickerUi::inline(
            vec![
                item("Dojo 1", "Lair 1", 1, 1),
                agent,
                build,
                item("logs", "m5b", 2, 3),
                item("old", "m5b", 0, 1),
            ],
            true,
            None,
            Some(0),
        )
        .with_purpose(SessionPickerPurpose::RecentDojos)
    }

    const CONTENT: Rect = Rect {
        x: 0,
        y: 34,
        width: 960,
        height: 600,
    };

    #[test]
    fn the_legend_and_scope_follow_the_purpose_and_the_query() {
        assert_eq!(
            legend(SessionPickerPurpose::RecentDojos, false),
            [
                ("↑↓", "move"),
                ("↵", "open"),
                ("tab", "scope"),
                ("esc", "close")
            ]
        );
        assert_eq!(
            legend(SessionPickerPurpose::Dojos, true),
            [("↵", "switch"), ("esc", "close")]
        );
        assert_eq!(
            legend(SessionPickerPurpose::Remotes, false),
            [("↑↓", "move"), ("↵", "connect"), ("esc", "close")]
        );
    }

    /// The layout's rows are what the painter draws and what the pointer
    /// hits: each row's centre hits its own list index, which names the
    /// same target the painter drew there.
    #[test]
    fn painted_rows_and_hit_targets_agree() {
        let mut picker = dojos();
        let layout = session_picker_layout(CONTENT, &picker).unwrap();
        // The creation row, a `running` header, four running Dojos, then
        // `stopped` and its Dojo.
        assert_eq!(layout.rows.len(), 6);
        assert!(layout.rows[0].header.is_none());
        assert_eq!(
            layout.rows[1].header.map(|(_, label)| label),
            Some("running")
        );
        assert_eq!(
            layout.rows[5].header.map(|(_, label)| label),
            Some("stopped")
        );
        assert_eq!(layout.panel.width, 560);
        for (offset, row) in layout.rows.iter().enumerate() {
            let centre = (
                f64::from(row.rect.x + row.rect.width / 2),
                f64::from(row.rect.y + row.rect.height / 2),
            );
            let index = command_palette_row_hit_test(&layout, centre).unwrap();
            assert_eq!(index, layout.visible_start + offset);
            assert!(picker.list_target(index).is_some());
            if let Some((header, _)) = row.header {
                let on_header = (
                    f64::from(header.x + header.width / 2),
                    f64::from(header.y + header.height / 2),
                );
                assert_eq!(command_palette_row_hit_test(&layout, on_header), None);
            }
        }
        let theme = ResolvedTheme::default();
        let palette = chrome_panel_palette(theme, ChromeSurface::Menu);
        let (width, height) = (960, 640);
        let mut canvas = vec![0_u8; usize::try_from(width * height * 4).unwrap()];
        let mut cache = SessionPickerTextCache::default();
        paint_session_picker(
            &mut cache,
            &RenderContext::new(u16::MAX),
            &mut canvas,
            width,
            height,
            CONTENT,
            120,
            1,
            &layout,
            palette,
            &picker,
            SessionPickerPurpose::RecentDojos,
            None,
            true,
        )
        .unwrap();
        for text in [
            "dojos",
            "recent",
            "this lair",
            "filter dojos",
            "New terminal",
            "+",
            "ctrl",
            "running",
            "stopped",
            "needs input",
            "working",
            "2/3",
            "Lair 1 · ~/src/agent",
            "tab",
            "scope",
        ] {
            assert!(cache.contains_source(text), "{text} was not drawn");
        }
        // The selected row is on the selection fill where the layout put it.
        let selected = layout
            .rows
            .iter()
            .enumerate()
            .find(|(offset, _)| {
                picker.list_target(layout.visible_start + offset) == Some(picker.selected_target())
            })
            .map(|(_, row)| row.rect)
            .unwrap();
        let pixel = |x: u32, y: u32| {
            let index = usize::try_from((y * width + x) * 4).unwrap();
            [canvas[index + 2], canvas[index + 1], canvas[index]]
        };
        let [_, red, green, blue] = palette.selected_fill.to_be_bytes();
        assert_eq!(
            pixel(selected.x + selected.width - 4, selected.y + 2),
            [red, green, blue]
        );

        // Filtering: no headers, no creation row, the count in the frame.
        assert!(picker.append_text("b"));
        let filtered = session_picker_layout(CONTENT, &picker).unwrap();
        assert!(filtered.rows.iter().all(|row| row.header.is_none()));
        assert_eq!(filtered.rows.len(), picker.list_len());
        assert_eq!(picker.list_target(0), Some(PickerHitTarget::Open(0)));
    }

    #[test]
    fn an_empty_list_says_so_without_repeating_a_blocker() {
        let empty = SessionPickerUi::inline(Vec::new(), true, None, None)
            .with_purpose(SessionPickerPurpose::Dojos);
        assert!(shows_empty_line(&empty));
        let layout = session_picker_layout(CONTENT, &empty).unwrap();
        assert_eq!(layout.rows.len(), 2);
        assert_eq!(
            command_palette_row_hit_test(&layout, (480.0, f64::from(layout.rows[1].rect.y + 1)))
                .and_then(|index| empty.list_target(index)),
            None,
            "the empty line is not a target"
        );
        let loading = SessionPickerUi::inline(Vec::new(), false, Some("Loading navigation…"), None);
        assert!(!shows_empty_line(&loading));
        assert_eq!(
            session_picker_layout(CONTENT, &loading).unwrap().rows.len(),
            1
        );
    }
}
