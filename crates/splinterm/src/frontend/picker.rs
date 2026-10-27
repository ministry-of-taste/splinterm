//! Platform-independent Dojo-picker interaction and standalone presentation state.

use std::sync::mpsc::Sender as StdSender;

use splinterm_core::SplintId;
use splinterm_protocol::{
    ActiveScreen, AgentState, CellAttributes, ColorSource, MouseTracking, TerminalCell,
    TerminalInputModes, TerminalRow, TerminalSnapshot, UnderlineStyle,
};
use unicode_width::UnicodeWidthChar;

use crate::{frontend::agent_status::state_text, renderer::SessionPickerPurpose};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum PickerHitTarget {
    New,
    Open(usize),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionPickerItem {
    pub display_title: String,
    pub breadcrumb: String,
    pub working_directory: String,
    pub pane_count: usize,
    pub running_pane_count: usize,
    /// The most urgent reported agent state among the Dojo's Splints, and
    /// whether it is stale; display data only.
    pub agent: Option<(AgentState, bool)>,
}

/// What a picker row says at its right end.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PickerStatus {
    /// The agent state's dot, and whether it is stale.
    pub(crate) agent: Option<(AgentState, bool)>,
    /// The state in words (`needs input`), or the running panes (`1/2`).
    pub(crate) word: String,
}

impl SessionPickerItem {
    /// The secondary location after the name: the Lair the breadcrumb
    /// names (or the breadcrumb itself when it is not `Lair / name`) and
    /// the working directory, joined by ` · `.
    #[must_use]
    pub(crate) fn location(&self) -> String {
        let suffix = format!(" / {}", self.display_title);
        let place = if self.breadcrumb == self.display_title {
            ""
        } else {
            self.breadcrumb
                .strip_suffix(&suffix)
                .unwrap_or(&self.breadcrumb)
        };
        [place, self.working_directory.as_str()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// The agent state when one reports, else the running panes; nothing
    /// for a navigation row, which has no panes, or for a stopped Dojo,
    /// which its `stopped` header already names.
    #[must_use]
    pub(crate) fn status(&self) -> Option<PickerStatus> {
        if let Some((state, stale)) = self.agent {
            return Some(PickerStatus {
                agent: Some((state, stale)),
                word: state_text(state).to_lowercase(),
            });
        }
        (self.running_pane_count > 0).then(|| PickerStatus {
            agent: None,
            word: format!("{}/{}", self.running_pane_count, self.pane_count),
        })
    }

    /// The header the row sits under while nothing is typed, in a Dojo
    /// picker: `running`, or `stopped` when none of its panes runs; none
    /// for a navigation row.
    const fn group(&self) -> &'static str {
        if self.pane_count == 0 {
            ""
        } else if self.running_pane_count > 0 || self.agent.is_some() {
            "running"
        } else {
            "stopped"
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionPickerDecision {
    New,
    Open(usize),
}

enum SessionPickerHost {
    Standalone {
        revision: u64,
        synthetic_id: SplintId,
        decision: StdSender<SessionPickerDecision>,
    },
    Inline,
}

/// A picker's list: the creation row first while nothing is typed, then
/// the items that match what is typed. Typing filters at once.
///
/// `selected` and the targets count the creation row as 0 and item `i` as
/// `i + 1` whether or not the creation row is listed; `visible_start` is
/// an index into the listed rows.
pub struct SessionPickerUi {
    items: Vec<SessionPickerItem>,
    filtered_indices: Vec<usize>,
    query: String,
    selected: usize,
    visible_start: usize,
    hovered: Option<PickerHitTarget>,
    new_enabled: bool,
    new_blocker: Option<&'static str>,
    /// A status picker that failed to load: R asks again.
    retryable: bool,
    /// Standalone pickers fix their purpose; inline pickers take it from
    /// the requesting selector.
    purpose: Option<SessionPickerPurpose>,
    host: SessionPickerHost,
}

/// Rows the standalone picker lists at once, as the inline list does.
const SESSION_PICKER_PAGE_ITEMS: usize = 10;
/// The standalone picker's first list row: under the title, the input
/// line and a blank line.
const SESSION_PICKER_LIST_ROW: usize = 3;
/// The query's length at most, in characters.
const MAX_QUERY_CHARS: usize = 64;
/// The standalone picker's row width: its status ends here.
const STANDALONE_COLUMNS: usize = 72;
/// The name column, in cells, so the locations line up, here and in the
/// inline rows.
pub(crate) const PICKER_NAME_CELLS: u32 = 13;

fn text_cells(text: &str) -> usize {
    text.chars()
        .map(|character| character.width().unwrap_or(0))
        .sum()
}

/// `text` in at most `cells` cells, ending in `…` when cut.
fn fitted(text: &str, cells: usize) -> String {
    if text_cells(text) <= cells {
        return text.to_owned();
    }
    let mut fitted = String::new();
    let mut used = 0;
    for character in text.chars() {
        let width = character.width().unwrap_or(0);
        if used + width + 1 > cells {
            break;
        }
        used += width;
        fitted.push(character);
    }
    if cells > 0 {
        fitted.push('…');
    }
    fitted
}

impl SessionPickerUi {
    #[must_use]
    pub fn new(items: Vec<SessionPickerItem>, decision: StdSender<SessionPickerDecision>) -> Self {
        let mut picker = Self {
            items,
            filtered_indices: Vec::new(),
            query: String::new(),
            selected: 0,
            visible_start: 0,
            hovered: None,
            new_enabled: true,
            new_blocker: None,
            retryable: false,
            purpose: Some(SessionPickerPurpose::RecentDojos),
            host: SessionPickerHost::Standalone {
                revision: 0,
                synthetic_id: SplintId::new(),
                decision,
            },
        };
        picker.apply_search();
        picker.selected = usize::from(!picker.filtered_indices.is_empty());
        picker
    }

    /// Builds the standalone remote-profile chooser. It offers no creation row:
    /// `blocker` explains where profiles come from, or why none are listed.
    #[must_use]
    pub fn remote_profiles(
        items: Vec<SessionPickerItem>,
        blocker: &'static str,
        decision: StdSender<SessionPickerDecision>,
    ) -> Self {
        let mut picker = Self {
            new_enabled: false,
            new_blocker: Some(blocker),
            purpose: Some(SessionPickerPurpose::Remotes),
            ..Self::new(items, decision)
        };
        picker.selected = picker.first_selectable();
        picker
    }

    /// Selects the item at `index`, as on returning to a list after choosing
    /// it; an index not listed leaves the selection as it was.
    pub fn select_item(&mut self, index: usize) {
        if let Some(position) = self.filtered_indices.iter().position(|item| *item == index) {
            self.selected = position + 1;
            self.ensure_selected_visible(SESSION_PICKER_PAGE_ITEMS);
        }
    }

    /// Selects the creation row, for a picker whose only items are navigation rows.
    pub fn select_new_row(&mut self) {
        if self.new_enabled && self.create_row_shown() {
            self.selected = 0;
            self.visible_start = 0;
        }
    }

    pub(crate) const fn purpose(&self) -> Option<SessionPickerPurpose> {
        self.purpose
    }

    /// The inline picker for `purpose`: its copy, and whether its rows
    /// group under headers.
    #[must_use]
    pub(crate) fn with_purpose(mut self, purpose: SessionPickerPurpose) -> Self {
        let chosen = match self.selected_target() {
            PickerHitTarget::New => None,
            PickerHitTarget::Open(index) => self.filtered_indices.get(index).copied(),
        };
        self.purpose = Some(purpose);
        self.apply_search();
        if let Some(position) = chosen.and_then(|chosen| {
            self.filtered_indices
                .iter()
                .position(|item| *item == chosen)
        }) {
            self.selected = position + 1;
        }
        self
    }

    /// Returns the Window title of a standalone picker.
    pub(crate) const fn standalone_title(&self) -> &'static str {
        match self.purpose {
            Some(SessionPickerPurpose::Remotes) => "Splinterm — Remotes",
            _ => "Splinterm — Recent Dojos",
        }
    }

    pub(crate) fn inline(
        items: Vec<SessionPickerItem>,
        new_enabled: bool,
        new_blocker: Option<&'static str>,
        initial_row: Option<usize>,
    ) -> Self {
        let mut picker = Self {
            items,
            filtered_indices: Vec::new(),
            query: String::new(),
            selected: 0,
            visible_start: 0,
            hovered: None,
            new_enabled,
            new_blocker,
            retryable: false,
            purpose: None,
            host: SessionPickerHost::Inline,
        };
        picker.apply_search();
        picker.selected = initial_row
            .and_then(|index| {
                picker
                    .filtered_indices
                    .iter()
                    .position(|item| *item == index)
            })
            .map_or_else(
                || usize::from(!picker.filtered_indices.is_empty()),
                |position| position + 1,
            );
        picker
    }

    /// The command palette's remote-profile pop-up: the standalone chooser's
    /// rows over a Window, with no creation row and the same explanation.
    pub(crate) fn inline_remote_profiles(
        items: Vec<SessionPickerItem>,
        blocker: &'static str,
    ) -> Self {
        Self {
            purpose: Some(SessionPickerPurpose::Remotes),
            ..Self::inline(items, false, Some(blocker), Some(0))
        }
    }

    pub(crate) const fn is_inline(&self) -> bool {
        matches!(self.host, SessionPickerHost::Inline)
    }

    pub(crate) fn selected_target(&self) -> PickerHitTarget {
        if self.selected == 0 {
            PickerHitTarget::New
        } else {
            PickerHitTarget::Open(self.selected - 1)
        }
    }

    pub(crate) fn items(&self) -> impl Iterator<Item = &SessionPickerItem> {
        self.filtered_indices
            .iter()
            .filter_map(|index| self.items.get(*index))
    }

    /// The listed item at `index` among the matches.
    pub(crate) fn item(&self, index: usize) -> Option<&SessionPickerItem> {
        self.items.get(*self.filtered_indices.get(index)?)
    }

    /// How many items the picker holds, matching or not.
    pub(crate) fn total(&self) -> usize {
        self.items.len()
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    /// The creation row is listed while nothing is typed; a query lists
    /// only the items it matches. A status picker lists no rows: its line
    /// says why.
    pub(crate) fn create_row_shown(&self) -> bool {
        self.query.is_empty() && !self.shows_status()
    }

    /// The rows listed: the creation row while shown, then the matches.
    pub(crate) fn list_len(&self) -> usize {
        self.filtered_indices
            .len()
            .saturating_add(usize::from(self.create_row_shown()))
    }

    /// The listed row at `index`.
    pub(crate) fn list_target(&self, index: usize) -> Option<PickerHitTarget> {
        let item = if self.create_row_shown() {
            if index == 0 {
                return Some(PickerHitTarget::New);
            }
            index - 1
        } else {
            index
        };
        (item < self.filtered_indices.len()).then_some(PickerHitTarget::Open(item))
    }

    /// The selected row's index among the listed rows.
    pub(crate) fn list_selected(&self) -> usize {
        if self.create_row_shown() {
            self.selected
        } else {
            self.selected.saturating_sub(1)
        }
    }

    /// Each listed row's header while nothing is typed, in a Dojo picker:
    /// none for the creation row, then `running` or `stopped`. Empty for a
    /// flat list.
    pub(crate) fn row_groups(&self) -> Vec<&'static str> {
        if !self.create_row_shown() || !self.groups_rows() {
            return Vec::new();
        }
        std::iter::once("")
            .chain(self.items().map(SessionPickerItem::group))
            .collect()
    }

    /// Inline Dojo pickers group; the standalone one keeps recency order.
    fn groups_rows(&self) -> bool {
        self.is_inline()
            && matches!(
                self.purpose,
                Some(SessionPickerPurpose::RecentDojos | SessionPickerPurpose::Dojos)
            )
    }

    /// Adds `text` to the query and lists what it matches, in the
    /// catalog's order, with the first selected.
    pub(crate) fn append_text(&mut self, text: &str) -> bool {
        let remaining = MAX_QUERY_CHARS.saturating_sub(self.query.chars().count());
        let addition = text
            .chars()
            .filter(|character| !character.is_control())
            .take(remaining)
            .collect::<String>();
        if addition.is_empty() {
            return false;
        }
        self.query.push_str(&addition);
        self.requery();
        true
    }

    pub(crate) fn backspace(&mut self) -> bool {
        if self.query.pop().is_none() {
            return false;
        }
        self.requery();
        true
    }

    pub(crate) fn clear_query(&mut self) -> bool {
        if self.query.is_empty() {
            return false;
        }
        self.query.clear();
        self.requery();
        true
    }

    fn requery(&mut self) {
        self.apply_search();
        self.selected = self.first_selectable();
        self.visible_start = 0;
        self.hovered = None;
    }

    /// Lists the items whose title or breadcrumb holds every word typed,
    /// in their order; while nothing is typed, a Dojo picker lists them
    /// group by group.
    fn apply_search(&mut self) {
        let needle = self.query.to_lowercase();
        let tokens = needle.split_whitespace().collect::<Vec<_>>();
        self.filtered_indices = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let labels = format!(
                    "{} {}",
                    item.display_title.to_lowercase(),
                    item.breadcrumb.to_lowercase()
                );
                tokens
                    .iter()
                    .all(|token| labels.contains(token))
                    .then_some(index)
            })
            .collect();
        if self.query.is_empty() && self.groups_rows() {
            let rank = |item: &SessionPickerItem| match item.group() {
                "running" => 0,
                "stopped" => 1,
                _ => 2,
            };
            let items = &self.items;
            self.filtered_indices
                .sort_by_key(|index| items.get(*index).map_or(2, rank));
        }
    }

    /// The first row Enter can act on: the creation row while it is listed
    /// and enabled, else the first match.
    fn first_selectable(&self) -> usize {
        if self.create_row_shown() && self.new_enabled {
            0
        } else {
            usize::from(!self.filtered_indices.is_empty())
        }
    }

    /// A status picker: no items and no creation, whatever is typed,
    /// only the line that says why (loading, or could not load).
    pub(crate) fn shows_status(&self) -> bool {
        self.items.is_empty() && !self.new_enabled
    }

    /// Whether R asks again: a status picker whose request failed, not one
    /// still loading.
    pub(crate) fn retryable(&self) -> bool {
        self.retryable && self.shows_status()
    }

    /// Marks a status picker as failed, so R retries it.
    pub(crate) fn set_retryable(&mut self) {
        self.retryable = true;
    }

    pub(crate) const fn new_enabled(&self) -> bool {
        self.new_enabled
    }

    pub(crate) const fn new_blocker(&self) -> Option<&'static str> {
        self.new_blocker
    }

    pub(crate) const fn visible_start(&self) -> usize {
        self.visible_start
    }

    pub(crate) fn set_visible_start(&mut self, visible_start: usize) {
        self.visible_start = visible_start;
    }

    pub(crate) const fn hovered(&self) -> Option<PickerHitTarget> {
        self.hovered
    }

    pub(crate) fn update_hovered(&mut self, hovered: Option<PickerHitTarget>) -> bool {
        if self.hovered == hovered {
            return false;
        }
        self.hovered = hovered;
        true
    }

    pub(crate) fn clear_hovered(&mut self) -> bool {
        self.hovered.take().is_some()
    }

    pub(crate) fn into_standalone_decision(self) -> Option<StdSender<SessionPickerDecision>> {
        match self.host {
            SessionPickerHost::Standalone { decision, .. } => Some(decision),
            SessionPickerHost::Inline => None,
        }
    }

    pub(crate) fn selected_decision(&self) -> Option<SessionPickerDecision> {
        self.decision_for_target(self.selected_target())
    }

    pub(crate) fn decision_for_target(
        &self,
        target: PickerHitTarget,
    ) -> Option<SessionPickerDecision> {
        match target {
            PickerHitTarget::New => {
                (self.new_enabled && self.create_row_shown()).then_some(SessionPickerDecision::New)
            }
            PickerHitTarget::Open(index) => self
                .filtered_indices
                .get(index)
                .copied()
                .map(SessionPickerDecision::Open),
        }
    }

    pub(crate) fn move_selection(&mut self, delta: isize) {
        let first = usize::from(
            !(self.filtered_indices.is_empty() || (self.create_row_shown() && self.new_enabled)),
        );
        let count = self
            .filtered_indices
            .len()
            .saturating_add(1)
            .saturating_sub(first);
        if count == 0 {
            return;
        }
        let current = self.selected.saturating_sub(first).min(count - 1);
        let magnitude = delta.unsigned_abs() % count;
        let next = if delta.is_negative() {
            current.saturating_add(count).saturating_sub(magnitude) % count
        } else {
            current.saturating_add(magnitude) % count
        };
        self.selected = next.saturating_add(first);
        self.ensure_selected_visible(SESSION_PICKER_PAGE_ITEMS);
    }

    pub(crate) fn select_first(&mut self) {
        self.selected = self.first_selectable();
        self.visible_start = 0;
    }

    pub(crate) fn select_last(&mut self) {
        self.selected = self.filtered_indices.len();
        self.ensure_selected_visible(SESSION_PICKER_PAGE_ITEMS);
    }

    /// Scrolls the listed rows so the selected one is among the
    /// `visible_count` from `visible_start`.
    fn ensure_selected_visible(&mut self, visible_count: usize) {
        let len = self.list_len();
        if len == 0 || visible_count == 0 {
            self.visible_start = 0;
            return;
        }
        let selected = self.list_selected();
        if selected < self.visible_start {
            self.visible_start = selected;
        } else if selected >= self.visible_start.saturating_add(visible_count) {
            self.visible_start = selected.saturating_add(1).saturating_sub(visible_count);
        }
        self.visible_start = self.visible_start.min(len.saturating_sub(visible_count));
    }

    /// The standalone picker's terminal row `row`, chosen with the pointer.
    pub(crate) fn select_row(&mut self, row: usize) -> Option<SessionPickerDecision> {
        let relative = row.checked_sub(SESSION_PICKER_LIST_ROW)?;
        if relative >= SESSION_PICKER_PAGE_ITEMS {
            return None;
        }
        let target = self.list_target(self.visible_start.checked_add(relative)?)?;
        let decision = self.decision_for_target(target)?;
        self.selected = match target {
            PickerHitTarget::New => 0,
            PickerHitTarget::Open(index) => index + 1,
        };
        Some(decision)
    }

    /// One standalone list row: the marker, the name in its column, the
    /// location, and the status at the row's end.
    fn standalone_row(selected: bool, name: &str, location: &str, status: &str) -> String {
        let marker = if selected { "› " } else { "  " };
        let name_cells = usize::try_from(PICKER_NAME_CELLS).unwrap_or(usize::MAX);
        let name = fitted(name, name_cells);
        let name_pad = name_cells.saturating_sub(text_cells(&name));
        let lead = format!("{marker}{name}{}", " ".repeat(name_pad));
        let status_cells = text_cells(status);
        let room = STANDALONE_COLUMNS
            .saturating_sub(text_cells(&lead))
            .saturating_sub(status_cells)
            .saturating_sub(4);
        let location = if location.is_empty() {
            String::new()
        } else {
            fitted(location, room)
        };
        let gap = STANDALONE_COLUMNS
            .saturating_sub(text_cells(&lead))
            .saturating_sub(2)
            .saturating_sub(text_cells(&location))
            .saturating_sub(status_cells)
            .max(2);
        format!("{lead}  {location}{}{status}", " ".repeat(gap))
            .trim_end()
            .to_owned()
    }

    /// Builds the temporary terminal presentation used only by the standalone
    /// `splinterm dojos` host: the inline picker's copy as text, with the
    /// title, the input line, the rows and the keys.
    ///
    /// # Panics
    ///
    /// Panics if called for the inline host, whose presentation is native chrome.
    #[must_use]
    pub fn snapshot(&mut self) -> TerminalSnapshot {
        let purpose = self.purpose.unwrap_or(SessionPickerPurpose::RecentDojos);
        let mut lines = vec![
            purpose.title().to_owned(),
            if self.query.is_empty() {
                format!("› {}", purpose.placeholder())
            } else {
                // The space after the query is the reversed cursor cell.
                format!("› {} ", self.query)
            },
            String::new(),
        ];
        let len = self.list_len();
        for index in (self.visible_start..len).take(SESSION_PICKER_PAGE_ITEMS) {
            let selected = index == self.list_selected();
            let line = match self.list_target(index) {
                Some(PickerHitTarget::New) => {
                    let trailing = if self.new_enabled {
                        "ctrl n"
                    } else {
                        self.new_blocker.unwrap_or("")
                    };
                    let marker = if selected { "› " } else { "+ " };
                    let title = purpose.create_title();
                    let gap = STANDALONE_COLUMNS
                        .saturating_sub(2 + text_cells(title) + text_cells(trailing))
                        .max(2);
                    format!("{marker}{title}{}{trailing}", " ".repeat(gap))
                }
                Some(PickerHitTarget::Open(item)) => {
                    let Some(item) = self.item(item) else {
                        continue;
                    };
                    let status = item.status().map(|status| status.word).unwrap_or_default();
                    Self::standalone_row(selected, &item.display_title, &item.location(), &status)
                }
                None => continue,
            };
            lines.push(line);
        }
        if self.shows_status() {
            lines.push(format!("  {}", self.new_blocker.unwrap_or("")));
        } else if self.filtered_indices.is_empty() {
            lines.push(format!(
                "  {}",
                if self.query.is_empty() {
                    purpose.empty_label()
                } else {
                    purpose.no_match()
                }
            ));
        }
        while lines.len() < SESSION_PICKER_LIST_ROW + SESSION_PICKER_PAGE_ITEMS {
            lines.push(String::new());
        }
        lines.extend([
            String::new(),
            format!("↑↓ move  ↵ {}  esc close", purpose.selection_verb()),
        ]);
        let SessionPickerHost::Standalone {
            revision,
            synthetic_id,
            ..
        } = &mut self.host
        else {
            panic!("inline Dojo picker does not own a terminal snapshot");
        };
        *revision = revision.saturating_add(1).max(1);
        let mut snapshot = picker_terminal_snapshot(*synthetic_id, *revision, lines);
        if let Some(input) = snapshot.visible_rows.get_mut(1) {
            let start = 2;
            if self.query.is_empty() {
                for cell in input.cells.iter_mut().skip(start) {
                    cell.attributes.dim = true;
                }
            } else if let Some(cursor) = input
                .cells
                .get_mut(start.saturating_add(self.query.chars().count()))
            {
                cursor.attributes.reverse = true;
            }
        }
        snapshot
    }
}

fn picker_terminal_snapshot(
    splint_id: SplintId,
    revision: u64,
    lines: Vec<String>,
) -> TerminalSnapshot {
    synthetic_text_snapshot(splint_id, revision, "Recent Dojos", lines)
}

/// Builds a read-only, cursorless text snapshot for a standalone client Window.
pub(crate) fn synthetic_text_snapshot(
    splint_id: SplintId,
    revision: u64,
    title: &str,
    lines: Vec<String>,
) -> TerminalSnapshot {
    let columns = lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(1)
        .clamp(72, 120);
    let rows = lines.len().max(24);
    let attributes = CellAttributes {
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
    };
    let mut visible_rows: Vec<_> = lines
        .into_iter()
        .map(|line| TerminalRow {
            row_id: None,
            linebreak: false,
            cells: line
                .chars()
                .take(columns)
                .map(|character| TerminalCell {
                    content: character.to_string(),
                    spacer_remaining: None,
                    attributes,
                })
                .collect(),
        })
        .collect();
    visible_rows.resize_with(rows, || TerminalRow {
        row_id: None,
        linebreak: false,
        cells: Vec::new(),
    });
    for (index, row) in visible_rows.iter_mut().enumerate() {
        row.row_id = u64::try_from(index)
            .ok()
            .and_then(|index| index.checked_add(1));
    }
    TerminalSnapshot {
        splint_id,
        incarnation: 1,
        revision,
        columns,
        rows,
        cursor_column: -1,
        cursor_row: -1,
        cursor_deferred_wrap: false,
        active_screen: ActiveScreen::Normal,
        input_modes: TerminalInputModes {
            application_cursor: false,
            application_keypad: false,
            focus_reporting: false,
            bracketed_paste: false,
            cursor_visible: false,
            cursor_blink: false,
            mouse_tracking: MouseTracking::None,
            sgr_mouse: false,
        },
        palette: vec![0; 256],
        default_colors: [0x00f4_f0e8, 0x0014_1820, 0x00e0_a030],
        title: title.to_owned(),
        visible_rows,
        history_generation: 1,
        oldest_available_scrollback_row_id: None,
        newest_available_scrollback_row_id: None,
        scrollback_rows: Vec::new(),
        available_scrollback_rows: 0,
        omitted_oldest_scrollback_rows: 0,
        images: None,
        exited_code: None,
        exited_signal: None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc as std_mpsc;

    use super::*;

    fn item(title: &str, breadcrumb: &str, running: usize, panes: usize) -> SessionPickerItem {
        SessionPickerItem {
            display_title: title.to_owned(),
            breadcrumb: breadcrumb.to_owned(),
            working_directory: format!("/work/{title}"),
            pane_count: panes,
            running_pane_count: running,
            agent: None,
        }
    }

    fn sessions(count: usize) -> Vec<SessionPickerItem> {
        (0..count)
            .map(|index| item(&format!("session {index}"), "work", 1, 1))
            .collect()
    }

    #[test]
    fn inline_picker_state_has_no_terminal_snapshot_identity() {
        let picker = SessionPickerUi::inline(
            vec![item("editor", "work / editor", 2, 2)],
            true,
            None,
            Some(0),
        );
        assert!(picker.is_inline());
        assert_eq!(picker.selected_target(), PickerHitTarget::Open(0));
        assert!(matches!(picker.host, SessionPickerHost::Inline));
    }

    #[test]
    fn session_picker_wraps_pages_and_maps_visible_rows() {
        let (decision, _receiver) = std_mpsc::channel();
        let mut picker = SessionPickerUi::new(sessions(12), decision);
        assert_eq!(
            picker.selected_decision(),
            Some(SessionPickerDecision::Open(0))
        );
        picker.move_selection(-2);
        assert_eq!(
            picker.selected_decision(),
            Some(SessionPickerDecision::Open(11))
        );
        // Thirteen rows with the creation row; the last ten are shown.
        assert_eq!(picker.visible_start, 3);
        assert_eq!(
            picker.select_row(SESSION_PICKER_LIST_ROW),
            Some(SessionPickerDecision::Open(2))
        );
        assert_eq!(
            picker.selected_decision(),
            Some(SessionPickerDecision::Open(2))
        );
        let snapshot = picker.snapshot();
        assert!(snapshot.validate().is_ok());
        assert!(row_text(&snapshot, SESSION_PICKER_LIST_ROW).starts_with("› session 2"));
        assert_eq!(picker.select_row(SESSION_PICKER_LIST_ROW - 1), None);
        assert_eq!(
            picker.select_row(SESSION_PICKER_LIST_ROW + SESSION_PICKER_PAGE_ITEMS),
            None
        );
    }

    #[test]
    fn typing_filters_at_once_and_lists_only_matches() {
        let mut picker = SessionPickerUi::inline(
            vec![
                item("build", "m5a / build", 1, 1),
                item("journal", "notes / journal", 1, 1),
                item("logs", "m5b / logs", 2, 3),
            ],
            true,
            None,
            Some(2),
        );
        assert_eq!(picker.list_len(), 4);
        assert_eq!(picker.list_target(0), Some(PickerHitTarget::New));
        assert_eq!(picker.list_selected(), 3);
        // `j` is text, not a move: it filters to the one match and selects
        // it, and the creation row is not listed.
        assert!(picker.append_text("j"));
        assert!(!picker.create_row_shown());
        assert_eq!(picker.list_len(), 1);
        assert_eq!(picker.list_target(0), Some(PickerHitTarget::Open(0)));
        assert_eq!(picker.list_target(1), None);
        assert_eq!(picker.list_selected(), 0);
        assert_eq!(
            picker.selected_decision(),
            Some(SessionPickerDecision::Open(1))
        );
        assert_eq!(picker.decision_for_target(PickerHitTarget::New), None);
        // Nothing matches: Enter does nothing.
        assert!(picker.append_text("zz"));
        assert_eq!(picker.list_len(), 0);
        assert_eq!(picker.selected_decision(), None);
        // Clearing lists the creation row again and selects it.
        assert!(picker.clear_query());
        assert!(!picker.clear_query());
        assert!(picker.create_row_shown());
        assert_eq!(picker.selected_decision(), Some(SessionPickerDecision::New));
        assert!(picker.append_text("l"));
        assert!(picker.backspace());
        assert!(!picker.backspace());
        assert_eq!(picker.selected_decision(), Some(SessionPickerDecision::New));
    }

    #[test]
    fn inline_picker_search_is_stable_bounded_and_excludes_cwd() {
        let mut first = item("Éditor", "work / Éditor", 1, 1);
        first.working_directory = "/secret/match".to_owned();
        let items = vec![first, item("Éditor", "other / Éditor", 1, 1)];
        let mut picker = SessionPickerUi::inline(items, false, Some("Unavailable"), Some(0));
        assert!(picker.append_text("ÉDITOR"));
        assert_eq!(picker.items().count(), 2);
        assert_eq!(
            picker.selected_decision(),
            Some(SessionPickerDecision::Open(0))
        );
        picker.move_selection(1);
        assert_eq!(
            picker.selected_decision(),
            Some(SessionPickerDecision::Open(1))
        );
        assert!(picker.clear_query());
        assert!(picker.append_text("match"));
        assert_eq!(picker.items().count(), 0);
        assert_eq!(picker.selected_decision(), None);
        assert!(picker.clear_query());
        assert!(picker.append_text(&"x".repeat(100)));
        assert_eq!(picker.query().chars().count(), 64);
        assert!(!picker.append_text("\u{7}"));
    }

    #[test]
    fn filtered_pointer_targets_resolve_original_catalog_indices() {
        let mut picker = SessionPickerUi::inline(sessions(10), false, Some("Unavailable"), Some(0));
        picker.append_text("session 9");
        assert_eq!(
            picker.decision_for_target(PickerHitTarget::Open(0)),
            Some(SessionPickerDecision::Open(9))
        );
        assert_eq!(
            picker.decision_for_target(picker.selected_target()),
            picker.selected_decision()
        );
        assert_eq!(picker.decision_for_target(PickerHitTarget::Open(1)), None);
        assert_eq!(picker.decision_for_target(PickerHitTarget::New), None);
        picker.append_text(" missing");
        assert_eq!(picker.decision_for_target(PickerHitTarget::Open(0)), None);
        picker.clear_query();
        assert_eq!(
            picker.decision_for_target(PickerHitTarget::Open(9)),
            Some(SessionPickerDecision::Open(9))
        );
    }

    #[test]
    fn session_picker_adapts_visibility_for_empty_and_large_catalogs() {
        for count in [0, 1, 7, 8, 64, 256] {
            let (decision, _receiver) = std_mpsc::channel();
            let mut picker = SessionPickerUi::new(sessions(count), decision);
            picker.select_first();
            picker.move_selection(-1);
            let expected = if count == 0 {
                SessionPickerDecision::New
            } else {
                SessionPickerDecision::Open(count - 1)
            };
            assert_eq!(picker.selected_decision(), Some(expected));
            picker.ensure_selected_visible(3);
            let len = picker.list_len();
            assert!(picker.visible_start <= len.saturating_sub(3));
            let selected = picker.list_selected();
            assert!(selected >= picker.visible_start);
            assert!(selected < picker.visible_start + 3.min(len));
            picker.select_first();
            assert_eq!(picker.selected_decision(), Some(SessionPickerDecision::New));
            assert_eq!(picker.visible_start, 0);
            picker.select_last();
            assert_eq!(picker.selected_decision(), Some(expected));
        }
    }

    #[test]
    fn rows_say_where_and_how_each_dojo_is() {
        let mut agent = item("agent", "Lair 1 / agent", 1, 1);
        agent.working_directory = "~/Projects/splinterm".to_owned();
        agent.agent = Some((AgentState::NeedsInput, false));
        assert_eq!(agent.location(), "Lair 1 · ~/Projects/splinterm");
        assert_eq!(
            agent.status(),
            Some(PickerStatus {
                agent: Some((AgentState::NeedsInput, false)),
                word: "needs input".to_owned(),
            })
        );
        let logs = item("logs", "m5b / logs", 2, 3);
        assert_eq!(logs.location(), "m5b · /work/logs");
        assert_eq!(logs.status().unwrap().word, "2/3");
        assert!(logs.status().unwrap().agent.is_none());
        // A session entry's breadcrumb is its title: only the directory.
        assert_eq!(item("shell", "shell", 1, 1).location(), "/work/shell");
        // A Lair row's breadcrumb says what it opens.
        assert_eq!(
            item("Lair 2", "Opens build", 1, 1).location(),
            "Opens build · /work/Lair 2"
        );
        // Navigation rows have no status.
        let profile = navigation_item("wintermute", "case@wintermute.example:2222");
        assert_eq!(profile.location(), "case@wintermute.example:2222");
        assert_eq!(profile.status(), None);
    }

    #[test]
    fn dojo_pickers_group_running_before_stopped_until_a_query_filters() {
        let items = vec![
            item("old", "Lair / old", 0, 1),
            item("live", "Lair / live", 1, 1),
        ];
        let picker = SessionPickerUi::inline(items.clone(), true, None, Some(0))
            .with_purpose(SessionPickerPurpose::Dojos);
        assert_eq!(picker.row_groups(), ["", "running", "stopped"]);
        assert_eq!(picker.item(0).unwrap().display_title, "live");
        // The initial row stays selected through the regrouping.
        assert_eq!(
            picker.selected_decision(),
            Some(SessionPickerDecision::Open(0))
        );
        let mut filtered = picker;
        filtered.append_text("o");
        assert!(filtered.row_groups().is_empty());
        let lairs = SessionPickerUi::inline(items.clone(), true, None, None)
            .with_purpose(SessionPickerPurpose::Lairs);
        assert!(lairs.row_groups().is_empty());
        assert_eq!(lairs.item(0).unwrap().display_title, "old");
        // The standalone `splinterm dojos` keeps recency order, ungrouped.
        let (decision, _receiver) = std_mpsc::channel();
        let standalone = SessionPickerUi::new(items, decision);
        assert!(standalone.row_groups().is_empty());
        assert_eq!(standalone.item(0).unwrap().display_title, "old");
        // A stopped Dojo says nothing at its end: its header says it.
        assert_eq!(standalone.item(0).unwrap().status(), None);
    }

    #[test]
    fn status_pickers_have_no_items_and_no_creation_whatever_is_typed() {
        let mut loading =
            SessionPickerUi::inline(Vec::new(), false, Some("Loading navigation…"), None);
        assert!(loading.shows_status());
        assert!(!loading.retryable(), "loading is not retried");
        assert!(!loading.create_row_shown());
        assert_eq!(loading.list_len(), 0);
        assert!(loading.append_text("r"));
        assert!(loading.shows_status(), "a query keeps the status line");
        assert_eq!(loading.list_len(), 0);
        let mut failed =
            SessionPickerUi::inline(Vec::new(), false, Some("Could not load navigation"), None);
        failed.set_retryable();
        assert!(failed.retryable());
        assert!(failed.append_text("x"));
        assert!(failed.retryable());
        // Items, or creation, make it a list.
        let creation = SessionPickerUi::inline(Vec::new(), true, None, None);
        assert!(!creation.shows_status());
        assert!(creation.create_row_shown());
        let mut listed =
            SessionPickerUi::inline(vec![item("logs", "logs", 1, 1)], false, Some("Full"), None);
        listed.set_retryable();
        assert!(!listed.shows_status());
        assert!(!listed.retryable());
        // A blocked creation row stays listed among items.
        assert!(listed.create_row_shown());
    }

    fn row_text(snapshot: &TerminalSnapshot, row: usize) -> String {
        snapshot.visible_rows[row]
            .cells
            .iter()
            .map(|cell| cell.content.as_str())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    fn navigation_item(title: &str, breadcrumb: &str) -> SessionPickerItem {
        SessionPickerItem {
            display_title: title.to_owned(),
            breadcrumb: breadcrumb.to_owned(),
            working_directory: String::new(),
            pane_count: 0,
            running_pane_count: 0,
            agent: None,
        }
    }

    fn snapshot_lines(snapshot: &TerminalSnapshot) -> Vec<String> {
        (0..snapshot.visible_rows.len())
            .map(|row| row_text(snapshot, row))
            .collect()
    }

    /// A Recent Dojos row in the standalone host: the marker, the Dojo's
    /// name in its column, the Lair and directory, and the running panes at
    /// the row's end, as the inline row lays them out.
    #[test]
    fn the_standalone_row_names_the_dojo_then_its_lair() {
        let (decision, _receiver) = std_mpsc::channel();
        let mut picker = SessionPickerUi::new(
            vec![SessionPickerItem {
                display_title: "Dojo 1".to_owned(),
                breadcrumb: "Lair 1 / Dojo 1".to_owned(),
                working_directory: "/home/user/Projects/splinterm".to_owned(),
                pane_count: 1,
                running_pane_count: 1,
                agent: None,
            }],
            decision,
        );
        let lines = snapshot_lines(&picker.snapshot());
        let row = &lines[SESSION_PICKER_LIST_ROW + 1];
        assert_eq!(
            *row,
            format!(
                "› Dojo 1{}Lair 1 · /home/user/Projects/splinterm{}1/1",
                " ".repeat(9),
                " ".repeat(14)
            )
        );
        assert_eq!(text_cells(row), STANDALONE_COLUMNS);
    }

    #[test]
    fn the_standalone_picker_reads_like_the_inline_one() {
        let (decision, _receiver) = std_mpsc::channel();
        let mut picker = SessionPickerUi::new(
            vec![item("build", "build", 1, 1), item("logs", "logs", 2, 3)],
            decision,
        );
        let snapshot = picker.snapshot();
        let lines = snapshot_lines(&snapshot);
        assert_eq!(lines[0], "dojos");
        assert_eq!(lines[1], "› filter dojos");
        // The placeholder is dim; the marker is not.
        let input = &snapshot.visible_rows[1].cells;
        assert!(!input[0].attributes.dim);
        assert!(input[2..].iter().all(|cell| cell.attributes.dim));
        assert!(lines[SESSION_PICKER_LIST_ROW].starts_with("+ New terminal"));
        assert!(lines[SESSION_PICKER_LIST_ROW].ends_with("ctrl n"));
        let first = &lines[SESSION_PICKER_LIST_ROW + 1];
        assert!(first.starts_with("› build"), "{first}");
        assert!(first.contains("/work/build") && first.ends_with("1/1"));
        assert_eq!(
            text_cells(&lines[SESSION_PICKER_LIST_ROW + 2]),
            STANDALONE_COLUMNS,
            "the status ends at the row's end"
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "↑↓ move  ↵ open  esc close")
        );
        // Typing filters at once; the creation row gives way.
        assert!(picker.append_text("lo"));
        let snapshot = picker.snapshot();
        let lines = snapshot_lines(&snapshot);
        assert_eq!(lines[1], "› lo");
        // A block cursor: the cell after the query is reversed.
        let input = &snapshot.visible_rows[1].cells;
        assert_eq!(input[4].content, " ");
        assert!(input[4].attributes.reverse);
        assert!(
            input[..4]
                .iter()
                .all(|cell| !cell.attributes.reverse && !cell.attributes.dim)
        );
        assert!(lines[SESSION_PICKER_LIST_ROW].starts_with("› logs"));
        assert_eq!(
            picker.select_row(SESSION_PICKER_LIST_ROW),
            Some(SessionPickerDecision::Open(1))
        );
        assert!(picker.append_text("x"));
        let lines = snapshot_lines(&picker.snapshot());
        assert_eq!(lines[SESSION_PICKER_LIST_ROW], "  No matching Dojos");
        assert_eq!(picker.select_row(SESSION_PICKER_LIST_ROW), None);
    }

    #[test]
    fn remote_profile_chooser_offers_no_creation_and_opens_profiles() {
        let (decision, _receiver) = std_mpsc::channel();
        let mut picker = SessionPickerUi::remote_profiles(
            vec![
                navigation_item("straylight", "straylight"),
                navigation_item("wintermute", "case@wintermute.example:2222"),
            ],
            "Add profiles in remotes.toml",
            decision,
        );
        assert_eq!(picker.purpose(), Some(SessionPickerPurpose::Remotes));
        assert_eq!(picker.standalone_title(), "Splinterm — Remotes");
        assert!(!picker.new_enabled());
        assert_eq!(picker.new_blocker(), Some("Add profiles in remotes.toml"));
        assert_eq!(
            picker.selected_decision(),
            Some(SessionPickerDecision::Open(0))
        );
        assert_eq!(picker.decision_for_target(PickerHitTarget::New), None);
        picker.select_first();
        assert_eq!(
            picker.selected_decision(),
            Some(SessionPickerDecision::Open(0))
        );
        assert_eq!(picker.select_row(SESSION_PICKER_LIST_ROW), None);
        picker.move_selection(1);
        assert_eq!(
            picker.selected_decision(),
            Some(SessionPickerDecision::Open(1))
        );

        let snapshot = picker.snapshot();
        assert!(snapshot.validate().is_ok());
        let lines = snapshot_lines(&snapshot);
        assert_eq!(lines[0], "remotes");
        assert!(lines[SESSION_PICKER_LIST_ROW].starts_with("+ Add a profile"));
        assert!(lines[SESSION_PICKER_LIST_ROW].ends_with("Add profiles in remotes.toml"));
        let profile = &lines[SESSION_PICKER_LIST_ROW + 2];
        assert!(profile.starts_with("› wintermute"));
        assert!(
            profile.ends_with("case@wintermute.example:2222"),
            "navigation rows show no pane counts"
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "↑↓ move  ↵ connect  esc close")
        );

        // Back from a profile: that profile stays selected.
        let (decision, _receiver) = std_mpsc::channel();
        let mut returned = SessionPickerUi::remote_profiles(
            (0..12)
                .map(|index| navigation_item(&format!("host{index}"), "host"))
                .collect(),
            "Add profiles in remotes.toml",
            decision,
        );
        returned.select_item(10);
        assert_eq!(
            returned.selected_decision(),
            Some(SessionPickerDecision::Open(10))
        );
        assert!(
            returned.visible_start > 0,
            "the selection is scrolled into view"
        );
        returned.select_item(12);
        assert_eq!(
            returned.selected_decision(),
            Some(SessionPickerDecision::Open(10)),
            "an unlisted row changes nothing"
        );

        let (decision, _receiver) = std_mpsc::channel();
        let empty = SessionPickerUi::remote_profiles(Vec::new(), "No remote profiles", decision);
        assert_eq!(empty.selected_decision(), None);
    }

    #[test]
    fn the_palette_remote_pop_up_lists_profiles_inline_without_creation() {
        let mut picker = SessionPickerUi::inline_remote_profiles(
            vec![
                navigation_item("straylight", "straylight"),
                navigation_item("wintermute", "case@wintermute.example:2222"),
            ],
            "Add profiles in remotes.toml",
        );
        assert!(picker.is_inline());
        assert_eq!(picker.purpose(), Some(SessionPickerPurpose::Remotes));
        assert!(!picker.new_enabled());
        assert_eq!(picker.new_blocker(), Some("Add profiles in remotes.toml"));
        assert_eq!(
            picker.selected_decision(),
            Some(SessionPickerDecision::Open(0))
        );
        assert_eq!(picker.decision_for_target(PickerHitTarget::New), None);
        picker.select_first();
        assert_eq!(
            picker.selected_decision(),
            Some(SessionPickerDecision::Open(0))
        );
        picker.move_selection(1);
        assert_eq!(
            picker.selected_decision(),
            Some(SessionPickerDecision::Open(1))
        );
        assert!(picker.row_groups().is_empty());
        assert!(picker.into_standalone_decision().is_none());

        let empty = SessionPickerUi::inline_remote_profiles(Vec::new(), "No remote profiles");
        assert_eq!(empty.selected_decision(), None);
        assert_eq!(empty.new_blocker(), Some("No remote profiles"));
    }

    #[test]
    fn recent_dojos_keeps_new_default_when_only_the_remote_row_is_listed() {
        let (decision, _receiver) = std_mpsc::channel();
        let mut picker = SessionPickerUi::new(
            vec![navigation_item("Remote Dojos…", "2 remote profiles")],
            decision,
        );
        assert_eq!(picker.purpose(), Some(SessionPickerPurpose::RecentDojos));
        assert_eq!(picker.standalone_title(), "Splinterm — Recent Dojos");
        picker.select_new_row();
        assert_eq!(picker.selected_decision(), Some(SessionPickerDecision::New));
        picker.move_selection(1);
        assert_eq!(
            picker.selected_decision(),
            Some(SessionPickerDecision::Open(0))
        );
        let lines = snapshot_lines(&picker.snapshot());
        assert_eq!(lines[0], "dojos");
        let remote = &lines[SESSION_PICKER_LIST_ROW + 1];
        assert!(remote.starts_with("› Remote Dojos…"), "{remote}");
        assert!(remote.ends_with("2 remote profiles"));
    }
}
