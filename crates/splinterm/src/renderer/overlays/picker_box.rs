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
/// filters, no arrows; the `dojos` picker's Tab always. A status picker
/// (`status` is whether it retries) answers R when it failed, and Escape.
fn legend(
    purpose: SessionPickerPurpose,
    filtering: bool,
    status: Option<bool>,
) -> Vec<(&'static str, &'static str)> {
    let mut legend = Vec::with_capacity(4);
    if let Some(retryable) = status {
        if retryable {
            legend.push(("r", "retry"));
        }
        legend.push(("esc", "close"));
        return legend;
    }
    if !filtering {
        legend.push(("↑↓", "move"));
    }
    legend.push(("↵", purpose.selection_verb()));
    if purpose.scope().is_some() {
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
    let status = picker.shows_status();
    let count = match_count_label(picker.items().count(), picker.total(), query);
    let scope = purpose.scope();
    let count_part = [count.as_str()];
    let scoped_count = [SCOPES[0], SCOPES[1], count.as_str()];
    // The scope stays shown while a query filters, since Tab still
    // switches it; a status picker has nothing to count.
    let right_label: &[&str] = match scope {
        Some(_) if query.is_empty() || status => &SCOPES,
        Some(_) => &scoped_count,
        None if status => &[],
        None => &count_part,
    };
    let legend = legend(
        purpose,
        !query.is_empty(),
        status.then(|| picker.retryable()),
    );
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

/// The status line's row, logical px: the list's first row, where a
/// status picker says why it lists nothing. The Window damages only it
/// when the loading spinner steps.
#[must_use]
pub(crate) fn picker_status_line(layout: &CommandPaletteLayout) -> Option<Rect> {
    (layout.list.height > 0).then(|| Rect {
        height: super::chrome_rows::ROW_HEIGHT.min(layout.list.height),
        ..layout.list
    })
}

/// While the picker waits on the daemon: nothing for its first 150 ms,
/// then the shared spinner's frame in the accent and `loading` in
/// secondary text.
fn paint_loading_line(
    painter: &mut ChromePainter<'_>,
    palette: &SessionPickerPalette,
    clip: Rect,
    glyph: Option<char>,
) -> Result<()> {
    let Some(glyph) = glyph else {
        return Ok(());
    };
    let cell_width = painter.cell(ChromeTextStyle::Regular)?.0;
    let end = clip.x.saturating_add(clip.width);
    painter.text(
        glyph.encode_utf8(&mut [0; 4]),
        ChromeTextStyle::Regular,
        Rect {
            width: cell_width.min(clip.width),
            ..clip
        },
        PickerTextAlignment::Left,
        palette.accent_text,
    )?;
    let word = clip.x.saturating_add(cell_width.saturating_mul(2)).min(end);
    painter.text(
        "loading",
        ChromeTextStyle::Regular,
        Rect {
            x: word,
            width: end.saturating_sub(word),
            ..clip
        },
        PickerTextAlignment::Left,
        palette.secondary,
    )
}

/// The listed rows with their group headers, or the one line that says
/// why there are none: the status (loading, could not load), or no match.
fn paint_picker_rows(
    painter: &mut ChromePainter<'_>,
    layout: &CommandPaletteLayout,
    palette: &SessionPickerPalette,
    picker: &SessionPickerUi,
    purpose: SessionPickerPurpose,
    pressed: Option<PickerHitTarget>,
) -> Result<()> {
    let line = if picker.shows_status() {
        Some(picker.new_blocker().unwrap_or(""))
    } else if picker.list_len() == 0 {
        Some(purpose.no_match())
    } else {
        None
    };
    if let Some(line) = line
        && let Some(status) = picker_status_line(layout)
    {
        let row = painter.buffer_rect(status);
        let clip = title_column(painter, row)?;
        if picker.shows_status() && picker.is_loading() {
            paint_loading_line(painter, palette, clip, picker.loading_shown())?;
        } else {
            painter.text(
                line,
                ChromeTextStyle::Regular,
                clip,
                PickerTextAlignment::Left,
                palette.secondary,
            )?;
        }
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
            legend(SessionPickerPurpose::RecentDojos, false, None),
            [
                ("↑↓", "move"),
                ("↵", "open"),
                ("tab", "scope"),
                ("esc", "close")
            ]
        );
        // Tab still switches the scope while a query filters.
        assert_eq!(
            legend(SessionPickerPurpose::Dojos, true, None),
            [("↵", "switch"), ("tab", "scope"), ("esc", "close")]
        );
        assert_eq!(
            legend(SessionPickerPurpose::Lairs, true, None),
            [("↵", "switch"), ("esc", "close")]
        );
        assert_eq!(
            legend(SessionPickerPurpose::Remotes, false, None),
            [("↑↓", "move"), ("↵", "connect"), ("esc", "close")]
        );
        assert_eq!(
            legend(SessionPickerPurpose::Dojos, true, Some(true)),
            [("r", "retry"), ("esc", "close")]
        );
        assert_eq!(
            legend(SessionPickerPurpose::Lairs, false, Some(false)),
            [("esc", "close")]
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
        assert!(
            session_picker_layout(CONTENT, &loading)
                .unwrap()
                .rows
                .is_empty(),
            "a status picker lists no rows, not even a blocked `+` row"
        );
    }

    /// A status picker draws its line alone, typed into or not: no `+`
    /// row, no "No matching", no count; R in its legend only once it failed.
    #[test]
    fn a_status_picker_shows_its_line_alone() {
        let palette = chrome_panel_palette(ResolvedTheme::default(), ChromeSurface::Menu);
        let (width, height) = (960, 640);
        for (query, retryable) in [("", false), ("lo", false), ("", true), ("x", true)] {
            let line = if retryable {
                "Could not load navigation"
            } else {
                "Loading navigation…"
            };
            let mut picker = SessionPickerUi::inline(Vec::new(), false, Some(line), None)
                .with_purpose(SessionPickerPurpose::Dojos);
            if retryable {
                picker.set_retryable();
            }
            picker.append_text(query);
            let layout = session_picker_layout(CONTENT, &picker).unwrap();
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
                SessionPickerPurpose::Dojos,
                None,
                true,
            )
            .unwrap();
            assert!(cache.contains_source(line), "{query:?}: no status line");
            assert!(cache.contains_source("recent"), "{query:?}: no scope");
            for absent in ["New Dojo", "No matching Dojos", "0/0", "+", "↵"] {
                assert!(!cache.contains_source(absent), "{query:?}: drew {absent}");
            }
            assert_eq!(cache.contains_source("retry"), retryable, "{query:?}");
        }
    }

    /// A picker waiting on the daemon draws nothing in its body for its
    /// first 150 ms, then the shared spinner's frame in the accent and
    /// `loading`, never "Loading…".
    #[test]
    fn a_loading_picker_shows_the_spinner_once_the_wait_shows() {
        let palette = chrome_panel_palette(ResolvedTheme::default(), ChromeSurface::Menu);
        let (width, height) = (960, 640);
        let start = std::time::Instant::now();
        let paint = |picker: &SessionPickerUi| {
            let layout = session_picker_layout(CONTENT, picker).unwrap();
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
                picker,
                SessionPickerPurpose::Dojos,
                None,
                true,
            )
            .unwrap();
            cache
        };
        let mut picker =
            SessionPickerUi::inline(Vec::new(), false, Some("Loading navigation…"), None)
                .with_purpose(SessionPickerPurpose::Dojos);
        picker.set_loading(start);
        let early = paint(&picker);
        for absent in ["Loading navigation…", "loading", "⠋"] {
            assert!(
                !early.contains_source(absent),
                "drew {absent} before 150 ms"
            );
        }
        let spinner = crate::frontend::AgentSpinner::STATIC.next();
        assert!(picker.tick_loading(start + std::time::Duration::from_millis(150), spinner));
        let shown = paint(&picker);
        assert!(shown.contains_source(&spinner.glyph().to_string()));
        assert!(shown.contains_source("loading"));
        assert!(!shown.contains_source("Loading navigation…"));
        let layout = session_picker_layout(CONTENT, &picker).unwrap();
        let line = picker_status_line(&layout).unwrap();
        assert_eq!((line.x, line.y), (layout.list.x, layout.list.y));
        assert!(line.height <= layout.list.height);
    }
}
