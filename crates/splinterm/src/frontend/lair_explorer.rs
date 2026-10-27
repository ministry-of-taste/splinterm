//! Pure Window-local state for the optional Lair explorer.

use std::collections::HashSet;

use splinterm_core::{LairRetention, SplintId};

use super::{
    agent_access::{AccessMarks, AccessReach, mark_text},
    agent_status::{self, AgentAggregate, AgentGlyphs, AgentSpinner},
    text_edit::BoundedTextEditor,
    topology::LairExplorerActivationTarget,
};
use crate::navigation_projection::{
    EndpointFreshness, NavigationAction, NavigationAgentStatus, NavigationAvailability,
    NavigationBlocker, NavigationExplorerDojo, NavigationExplorerLair, NavigationExplorerSplint,
    NavigationExplorerSplintTarget, NavigationExplorerTarget, NavigationExplorerView,
    NavigationLifecycle, NavigationNodeId, WindowAttachment,
};
use crate::renderer::ChromeSource;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum LairExplorerFilter {
    #[default]
    Live,
    Saved,
    All,
}

impl LairExplorerFilter {
    /// The header's filters, in the order shown: Live (the default), Saved,
    /// then All.
    pub(crate) const ALL: [Self; 3] = [Self::Live, Self::Saved, Self::All];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Live => "Live",
            Self::Saved => "Saved",
        }
    }

    /// The filter's lowercase token in the Explorer header.
    pub(crate) const fn token(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Live => "live",
            Self::Saved => "saved",
        }
    }

    pub(crate) const fn next(self) -> Self {
        match self {
            Self::Live => Self::Saved,
            Self::Saved => Self::All,
            Self::All => Self::Live,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LairExplorerRowKind {
    Lair,
    Dojo,
    Splint,
}

/// An agent access mark drawn as an icon in its own Explorer column (ADR
/// 0014). The words stay in the row's status and accessible text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LairExplorerAccess {
    /// A grant or policy rule covers the row or part of it.
    Reachable,
    /// An agent covering the row or part of it is connected.
    Connected,
    /// An agent is asking for access to the Lair: the most prominent mark.
    Asking,
}

impl LairExplorerAccess {
    /// The strongest mark of a row, asking first.
    fn of(whole: AccessReach, part: AccessReach, waiting: u32) -> Option<Self> {
        if waiting > 0 {
            return Some(Self::Asking);
        }
        match whole.max(part) {
            AccessReach::None => None,
            AccessReach::Reachable => Some(Self::Reachable),
            AccessReach::Connected => Some(Self::Connected),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent facts about one row, each drawn or announced on its own"
)]
pub(crate) struct LairExplorerRow {
    pub(crate) id: NavigationNodeId,
    pub(crate) kind: LairExplorerRowKind,
    pub(crate) label: String,
    /// Fixed-width agent status icon column, present while any Splint in the
    /// view has a reported status and presentation is enabled.
    pub(crate) agent_column: Option<ChromeSource>,
    /// A Lair row's aggregate glyph alone, without the stale slot, for the
    /// collapsed rail; present when `agent_column` is, `None` for Dojo and
    /// Splint rows.
    pub(crate) rail_glyph: Option<ChromeSource>,
    /// Whether `agent_column` shows a fresh working status, whose spinner
    /// moves.
    pub(crate) agent_working: bool,
    /// Whether any Splint at or under the row is running or starting.
    pub(crate) live: bool,
    /// A saved or pinned Lair's retention; `None` for a disposable Lair and
    /// for every Dojo and Splint.
    pub(crate) retention: Option<LairRetention>,
    /// The agent access mark, drawn as an icon.
    pub(crate) access: Option<LairExplorerAccess>,
    /// The footer's short status line for the selected row: only what the
    /// row does not already show, most urgent first, e.g. "1 needs input ·
    /// 2 working · saved". It leaves out the name, a running row's lifecycle,
    /// a disposable Lair's retention, and a current Dojo's attachment, and is
    /// empty when nothing else is worth saying.
    pub(crate) status: String,
    /// The full status in words for assistive technology and automation, e.g.
    /// "Running · Saved · 1 needs input, 2 working". Icons are visual only, so
    /// this spells out every state, including the ordinary ones `status`
    /// leaves out.
    pub(crate) accessible_status: String,
    pub(crate) level: usize,
    /// A Lair row's place among every Lair of the view, counting from 0,
    /// whatever the filter or search hides: the rail numbers a Lair whose
    /// name has no letter or digit by it, so its mark never shifts as other
    /// Lairs come and go from the list. 0 for Dojo and Splint rows.
    pub(crate) ordinal: usize,
    pub(crate) expanded: Option<bool>,
    pub(crate) current: bool,
    pub(crate) enabled: bool,
    pub(crate) pending: bool,
}

/// One row of the expanded Explorer's agents section: a Splint that reports
/// an agent status, drawn as its glyph, its name, and where it lives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LairExplorerAgentRow {
    /// The Splint's own row, as the tree would show it, at level 1.
    pub(crate) row: LairExplorerRow,
    /// Its Lair and Dojo, e.g. "work / editor".
    pub(crate) location: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LairExplorerDecision {
    Toggle(NavigationNodeId),
    /// A rail row whose Lair has no Dojo that opens at once expanded the
    /// Explorer to the tree with that Lair selected and open: the terminal
    /// beside it narrows, and the keys stay in the Explorer.
    ExpandRail(NavigationNodeId),
    OpenDojo(NavigationExplorerTarget),
    FocusSplint(NavigationExplorerSplintTarget),
}

impl LairExplorerDecision {
    pub(crate) const fn returns_focus_to_terminal(self) -> bool {
        matches!(self, Self::OpenDojo(_) | Self::FocusSplint(_))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LairExplorerPending {
    Dojo(NavigationExplorerTarget),
    Splint(NavigationExplorerSplintTarget),
}

impl LairExplorerPending {
    const fn from_decision(decision: LairExplorerDecision) -> Option<Self> {
        match decision {
            LairExplorerDecision::Toggle(_) | LairExplorerDecision::ExpandRail(_) => None,
            LairExplorerDecision::OpenDojo(target) => Some(Self::Dojo(target)),
            LairExplorerDecision::FocusSplint(target) => Some(Self::Splint(target)),
        }
    }

    const fn id(self) -> NavigationNodeId {
        match self {
            Self::Dojo(target) => NavigationNodeId::Dojo {
                lair_id: target.lair_id,
                dojo_id: target.dojo_id,
            },
            Self::Splint(target) => NavigationNodeId::Splint {
                lair_id: target.lair_id,
                dojo_id: target.dojo_id,
                splint_id: target.splint_id,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExplorerLoadState {
    Waiting,
    Refreshing,
    Ready,
    Stale,
    Disconnected,
}

#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent visibility, focus, search, and presentation axes, not one UI mode"
)]
pub(crate) struct LairExplorerUi {
    visible: bool,
    /// Whether the shown Explorer is collapsed to its rail: one row per Lair.
    /// Kept while the Explorer is hidden, so showing it again returns to the
    /// rail or the full panel, whichever it was.
    collapsed: bool,
    focused: bool,
    search_active: bool,
    filter: LairExplorerFilter,
    query: BoundedTextEditor,
    expanded: HashSet<NavigationNodeId>,
    selected: Option<NavigationNodeId>,
    /// Whether `selected` is a row of the agents section rather than of the
    /// tree: a Splint can be in both.
    agents_selected: bool,
    /// Whether the agents section is in the layout. A list too short for it
    /// hides it; the keys and assistive technology then skip its rows.
    agents_shown: bool,
    /// The selected agents row's place in the section, kept so a row that
    /// leaves the section passes the selection to the row now in its place.
    agent_index: usize,
    pending: Option<LairExplorerPending>,
    view: Option<NavigationExplorerView>,
    load_state: ExplorerLoadState,
    notice: Option<String>,
    context_pressed: Option<(NavigationNodeId, splinterm_core::TopologyRevision)>,
    agent_status_enabled: bool,
    agent_glyphs: AgentGlyphs,
    /// The working spinner's frame, stepped by the Window.
    agent_spinner: AgentSpinner,
    /// Which workspaces, Dojos, and Splints an agent can reach, is connected
    /// to, or asks for (ADR 0014). Shown in words in each row's status.
    access_marks: AccessMarks,
}

impl Default for LairExplorerUi {
    fn default() -> Self {
        Self {
            visible: false,
            collapsed: false,
            focused: false,
            search_active: false,
            filter: LairExplorerFilter::Live,
            query: BoundedTextEditor::new(String::new(), 256, 64, true),
            expanded: HashSet::new(),
            selected: None,
            agents_selected: false,
            agents_shown: true,
            agent_index: 0,
            pending: None,
            view: None,
            load_state: ExplorerLoadState::Waiting,
            notice: None,
            context_pressed: None,
            agent_status_enabled: true,
            agent_glyphs: AgentGlyphs::default(),
            agent_spinner: AgentSpinner::STATIC,
            access_marks: AccessMarks::default(),
        }
    }
}

impl LairExplorerUi {
    /// A hidden Explorer that shows as its rail first when `collapsed`
    /// (`[explorer] collapsed`).
    pub(crate) fn new(collapsed: bool) -> Self {
        Self {
            collapsed,
            ..Self::default()
        }
    }

    pub(crate) fn can_context(&self, node: NavigationNodeId) -> bool {
        self.visible
            && self.load_state == ExplorerLoadState::Ready
            && (self.rows().iter().any(|row| row.id == node) || self.lists_agent(node))
    }

    pub(crate) fn context_press(&mut self, target: Option<NavigationNodeId>) {
        self.context_pressed = target
            .filter(|node| self.can_context(*node))
            .and_then(|node| {
                self.view
                    .as_ref()
                    .map(|view| (node, view.topology_revision))
            });
    }

    pub(crate) fn context_release(
        &mut self,
        target: Option<NavigationNodeId>,
    ) -> Option<NavigationNodeId> {
        self.context_pressed
            .take()
            .filter(|(node, revision)| {
                Some(*node) == target
                    && self.can_context(*node)
                    && self
                        .view
                        .as_ref()
                        .is_some_and(|view| view.topology_revision == *revision)
            })
            .map(|(node, _)| node)
    }

    pub(crate) const fn context_press_pending(&self) -> bool {
        self.context_pressed.is_some()
    }

    pub(crate) fn cancel_context_press(&mut self) {
        self.context_pressed = None;
    }

    pub(crate) const fn visible(&self) -> bool {
        self.visible
    }

    /// Whether the Explorer is collapsed to its rail, shown or not.
    pub(crate) const fn collapsed(&self) -> bool {
        self.collapsed
    }

    /// Collapses the Explorer to its rail, one row per Lair, or expands it
    /// to the full tree; whether that changed. The rail has no search field,
    /// so collapsing stops typing into a search; a query left behind waits
    /// for the expanded tree. The selection moves to its row's Lair.
    pub(crate) fn set_collapsed(&mut self, collapsed: bool) -> bool {
        if self.collapsed == collapsed {
            return false;
        }
        self.collapsed = collapsed;
        self.context_pressed = None;
        if collapsed {
            self.search_active = false;
        }
        self.ensure_selection_visible();
        true
    }

    /// The palette's rail toggle: a hidden Explorer is shown as its rail,
    /// and a shown one switches between the rail and the full tree. Returns
    /// whether the Explorer was hidden before.
    pub(crate) fn toggle_rail(&mut self) -> bool {
        let was_hidden = !self.visible;
        if was_hidden {
            self.visible = true;
            self.set_collapsed(true);
        } else {
            self.set_collapsed(!self.collapsed);
        }
        was_hidden
    }

    pub(crate) const fn focused(&self) -> bool {
        self.focused
    }

    pub(crate) const fn search_active(&self) -> bool {
        self.search_active
    }

    pub(crate) const fn filter(&self) -> LairExplorerFilter {
        self.filter
    }

    pub(crate) fn set_filter(&mut self, filter: LairExplorerFilter) -> bool {
        if self.filter == filter {
            return false;
        }
        self.filter = filter;
        self.context_pressed = None;
        self.notice = None;
        self.ensure_selection_visible();
        true
    }

    /// The filter rows are drawn with: the chosen one, except that a search
    /// looks through every Lair, so a saved or stopped one is found while the
    /// default Live filter is chosen. Clearing the search returns to it. The
    /// rail shows no search, so it always uses the chosen filter.
    fn effective_filter(&self) -> LairExplorerFilter {
        if self.collapsed || normalized_query(self.query.text()).is_empty() {
            self.filter
        } else {
            LairExplorerFilter::All
        }
    }

    fn includes_lifecycle(&self, lifecycle: NavigationLifecycle) -> bool {
        self.effective_filter() != LairExplorerFilter::Live || is_live(lifecycle)
    }

    fn includes_dojo(&self, dojo: &NavigationExplorerDojo) -> bool {
        self.effective_filter() != LairExplorerFilter::Live
            || dojo
                .splints
                .iter()
                .any(|splint| self.includes_lifecycle(splint.lifecycle))
    }

    pub(crate) fn view(&self) -> Option<&NavigationExplorerView> {
        self.view.as_ref()
    }

    /// `[explorer] agent-status`: hides all agent status presentation.
    pub(crate) fn set_agent_status_enabled(&mut self, enabled: bool) -> bool {
        if self.agent_status_enabled == enabled {
            return false;
        }
        self.agent_status_enabled = enabled;
        self.ensure_selection_visible();
        true
    }

    /// Replaces the agent access marks; whether any row may change.
    pub(crate) fn set_access_marks(&mut self, marks: AccessMarks) -> bool {
        if self.access_marks == marks {
            return false;
        }
        self.access_marks = marks;
        true
    }

    fn splint_reach(&self, splint: &NavigationExplorerSplint) -> AccessReach {
        match splint.id {
            NavigationNodeId::Splint { splint_id, .. } => self.access_marks.splint(splint_id),
            _ => AccessReach::None,
        }
    }

    /// How far an agent reaches the whole Dojo, and some Splint in it.
    fn dojo_reach(&self, dojo: &NavigationExplorerDojo) -> (AccessReach, AccessReach) {
        let whole = match dojo.id {
            NavigationNodeId::Dojo { dojo_id, .. } => self.access_marks.dojo(dojo_id),
            _ => AccessReach::None,
        };
        let part = dojo
            .splints
            .iter()
            .map(|splint| self.splint_reach(splint))
            .max()
            .unwrap_or_default();
        (whole, part)
    }

    /// How far an agent reaches the whole Lair and some part of it, and how
    /// many agents are asking for access to it.
    fn lair_reach(
        &self,
        lair: &crate::navigation_projection::NavigationExplorerLair,
    ) -> (AccessReach, AccessReach, u32) {
        let NavigationNodeId::Lair(lair_id) = lair.id else {
            return (AccessReach::None, AccessReach::None, 0);
        };
        let part = lair
            .dojos
            .iter()
            .map(|dojo| {
                let (whole, part) = self.dojo_reach(dojo);
                whole.max(part)
            })
            .max()
            .unwrap_or_default();
        (
            self.access_marks.lair(lair_id),
            part,
            self.access_marks.waiting(lair_id),
        )
    }

    /// Icons, or ASCII when the embedded icon face is unavailable.
    pub(crate) fn set_agent_glyphs(&mut self, glyphs: AgentGlyphs) -> bool {
        let changed = self.agent_glyphs != glyphs;
        self.agent_glyphs = glyphs;
        changed
    }

    /// Whether agent status may be shown from the current view: endpoint
    /// freshness wins, so stale or disconnected data renders as unavailable.
    pub(crate) fn agent_status_available(&self) -> bool {
        self.agent_status_enabled
            && self.view.as_ref().is_some_and(|view| {
                matches!(
                    view.freshness,
                    EndpointFreshness::Current | EndpointFreshness::Refreshing
                )
            })
    }

    /// Applies one status change to the matching live incarnation only.
    pub(crate) fn apply_agent_status(
        &mut self,
        splint_id: SplintId,
        incarnation: u64,
        status: Option<NavigationAgentStatus>,
    ) -> bool {
        let changed = self
            .view
            .as_mut()
            .is_some_and(|view| view.apply_agent_status(splint_id, incarnation, status));
        if changed {
            self.ensure_selection_visible();
        }
        changed
    }

    fn splint_matches(&self, splint: &NavigationExplorerSplint, query: &[String]) -> bool {
        match splint
            .agent_status
            .as_ref()
            .filter(|_| self.agent_status_available())
        {
            Some(status) => matches_query(
                &format!("{} {}", splint.label, agent_status::search_terms(status)),
                query,
            ),
            None => matches_query(&splint.label, query),
        }
    }

    /// Appends aggregate agent status to a parent row's text, counting states
    /// in words; the icon column carries the most urgent state visually.
    fn aggregate_suffix(&self, aggregate: &AgentAggregate) -> String {
        if !self.agent_status_enabled || aggregate.is_empty() {
            String::new()
        } else if self.agent_status_available() {
            format!(" · {}", aggregate.counts_words())
        } else {
            format!(" · {}", agent_status::UNAVAILABLE_TEXT)
        }
    }

    fn splint_suffix(&self, status: Option<&NavigationAgentStatus>) -> String {
        match status.filter(|_| self.agent_status_enabled) {
            None => String::new(),
            Some(status) if self.agent_status_available() => {
                format!(" · {}", agent_status::splint_text(status))
            }
            Some(_) => format!(" · {}", agent_status::UNAVAILABLE_TEXT),
        }
    }

    /// A parent row's agent counts for the footer's short status line.
    fn aggregate_brief(&self, aggregate: &AgentAggregate) -> Option<String> {
        if !self.agent_status_enabled || aggregate.is_empty() {
            None
        } else if self.agent_status_available() {
            Some(aggregate.counts_brief())
        } else {
            Some(lower_first(agent_status::UNAVAILABLE_TEXT))
        }
    }

    /// A Splint row's agent status for the footer's short status line.
    fn splint_brief(&self, status: Option<&NavigationAgentStatus>) -> Option<String> {
        let status = status.filter(|_| self.agent_status_enabled)?;
        Some(if self.agent_status_available() {
            agent_status::splint_brief(status)
        } else {
            lower_first(agent_status::UNAVAILABLE_TEXT)
        })
    }

    fn agent_column_visible(&self) -> bool {
        self.agent_status_enabled
            && self
                .view
                .as_ref()
                .is_some_and(NavigationExplorerView::has_agent_status)
    }

    /// The glyph column hides last values while status is unavailable.
    fn aggregate_column(&self, aggregate: &AgentAggregate) -> Option<ChromeSource> {
        self.agent_column_visible().then(|| {
            if self.agent_status_available() {
                aggregate.glyph_column(self.agent_glyphs, self.agent_spinner)
            } else {
                AgentAggregate::default().glyph_column(self.agent_glyphs, AgentSpinner::STATIC)
            }
        })
    }

    /// A Splint row's glyph column, blank while status is unavailable.
    fn splint_column(&self, splint: &NavigationExplorerSplint) -> Option<ChromeSource> {
        self.agent_column_visible().then(|| {
            agent_status::glyph_column(
                splint
                    .agent_status
                    .as_ref()
                    .filter(|_| self.agent_status_available()),
                self.agent_glyphs,
                self.agent_spinner,
            )
        })
    }

    /// A Lair's rail glyph, present and blank exactly as its glyph column.
    fn aggregate_glyph(&self, aggregate: &AgentAggregate) -> Option<ChromeSource> {
        self.agent_column_visible().then(|| {
            if self.agent_status_available() {
                aggregate.glyph_slot(self.agent_glyphs, self.agent_spinner)
            } else {
                AgentAggregate::default().glyph_slot(self.agent_glyphs, AgentSpinner::STATIC)
            }
        })
    }

    /// Whether a parent row's column shows a fresh working status.
    fn aggregate_working(&self, aggregate: &AgentAggregate) -> bool {
        self.agent_column_visible()
            && self.agent_status_available()
            && aggregate.top().is_some_and(|(state, is_stale, _)| {
                state == splinterm_protocol::AgentState::Working && !is_stale
            })
    }

    /// Sets the working spinner's frame; whether it changed.
    pub(crate) fn set_agent_spinner(&mut self, spinner: AgentSpinner) -> bool {
        let changed = self.agent_spinner != spinner;
        self.agent_spinner = spinner;
        changed
    }

    pub(crate) fn set_search(&mut self, query: String) {
        self.query = BoundedTextEditor::new(query, 256, 64, false);
        self.begin_search();
        self.ensure_selection_visible();
    }

    pub(crate) fn focus_tree(&mut self) {
        self.search_active = false;
    }

    pub(crate) fn query(&self) -> &str {
        self.query.text()
    }

    /// Whether a search is active, so rows come from every Lair whatever the
    /// chosen filter. The rail never searches.
    pub(crate) fn searching_all_lairs(&self) -> bool {
        !self.collapsed && !normalized_query(self.query.text()).is_empty()
    }

    pub(crate) fn editor_mut(&mut self) -> &mut BoundedTextEditor {
        &mut self.query
    }

    pub(crate) fn toggle_visibility(&mut self) -> bool {
        self.visible = !self.visible;
        if !self.visible {
            self.focused = false;
            self.search_active = false;
        }
        self.visible
    }

    pub(crate) fn focus(&mut self) {
        self.visible = true;
        self.focused = true;
    }

    pub(crate) fn return_to_terminal(&mut self) {
        self.focused = false;
        self.search_active = false;
    }

    pub(crate) fn set_view(&mut self, view: NavigationExplorerView) {
        let retained = view
            .lairs
            .iter()
            .flat_map(|lair| std::iter::once(lair.id).chain(lair.dojos.iter().map(|dojo| dojo.id)))
            .collect::<HashSet<_>>();
        self.expanded.retain(|id| retained.contains(id));
        let selected_exists = self
            .selected
            .is_some_and(|selected| view_contains(&view, selected));
        let nearest_surviving = self.selected.and_then(|selected| {
            self.view
                .as_ref()
                .and_then(|previous| nearest_surviving_parent(previous, &view, selected))
        });
        if self.pending.is_some_and(|pending| {
            let pending = pending.id();
            !view.lairs.iter().any(|lair| {
                lair.id == pending
                    || lair.dojos.iter().any(|dojo| {
                        dojo.id == pending || dojo.splints.iter().any(|splint| splint.id == pending)
                    })
            })
        }) {
            self.pending = None;
        }
        // A selected agents row that goes passes the selection on as
        // `ensure_selection_visible` decides, below.
        if !selected_exists && !self.agents_selected {
            self.selected = nearest_surviving.or_else(|| {
                view.current
                    .and_then(|current| {
                        view.lairs
                            .iter()
                            .find(|lair| {
                                lair.dojos.iter().any(|dojo| {
                                    dojo.id == current
                                        || dojo.splints.iter().any(|splint| splint.id == current)
                                })
                            })
                            .map(|lair| lair.id)
                    })
                    .or_else(|| view.lairs.first().map(|lair| lair.id))
            });
            self.agents_selected = false;
        }
        self.load_state = match view.freshness {
            EndpointFreshness::Current => ExplorerLoadState::Ready,
            EndpointFreshness::Refreshing => ExplorerLoadState::Refreshing,
            EndpointFreshness::Stale => ExplorerLoadState::Stale,
            EndpointFreshness::Disconnected => ExplorerLoadState::Disconnected,
        };
        self.view = Some(view);
        self.notice = None;
        self.ensure_selection_visible();
    }

    pub(crate) fn begin_refresh(&mut self) {
        self.notice = None;
        if let Some(view) = &mut self.view {
            view.freshness = EndpointFreshness::Refreshing;
            for dojo in view.lairs.iter_mut().flat_map(|lair| &mut lair.dojos) {
                dojo.target.capability.availability = NavigationAvailability::Disabled(
                    crate::navigation_projection::NavigationBlocker::Refreshing,
                );
                for splint in &mut dojo.splints {
                    splint.target.capability.availability = NavigationAvailability::Disabled(
                        crate::navigation_projection::NavigationBlocker::Refreshing,
                    );
                }
            }
        }
        self.load_state = ExplorerLoadState::Refreshing;
    }

    pub(crate) fn mark_disconnected(&mut self) {
        self.load_state = ExplorerLoadState::Disconnected;
        self.pending = None;
        if let Some(view) = &mut self.view {
            view.freshness = EndpointFreshness::Disconnected;
            for dojo in view.lairs.iter_mut().flat_map(|lair| &mut lair.dojos) {
                dojo.target.capability.availability = NavigationAvailability::Disabled(
                    crate::navigation_projection::NavigationBlocker::Disconnected,
                );
                for splint in &mut dojo.splints {
                    splint.target.capability.availability = NavigationAvailability::Disabled(
                        crate::navigation_projection::NavigationBlocker::Disconnected,
                    );
                }
            }
        }
        self.ensure_selection_visible();
    }

    pub(crate) fn set_pending(&mut self, decision: LairExplorerDecision) -> bool {
        let Some(pending) = LairExplorerPending::from_decision(decision) else {
            return false;
        };
        let id = pending.id();
        // The rail lists Lairs only: its switch opens a Dojo through the
        // Lair's row.
        let row_id = if self.collapsed { lair_node(id) } else { id };
        if self.pending.is_some()
            || !(self
                .rows()
                .iter()
                .any(|row| row.id == row_id && row.enabled)
                // An agents row starts its Splint's activation whether or
                // not the tree shows that Splint, as under a closed Lair or
                // past a search.
                || self
                    .shown_agent_rows()
                    .iter()
                    .any(|agent| agent.row.id == id && agent.row.enabled))
        {
            return false;
        }
        self.notice = None;
        self.pending = Some(pending);
        true
    }

    /// Retire only the acknowledged request. Return whether terminal focus may resume.
    /// A matching local failure releases pending ownership, never keyboard ownership.
    pub(crate) fn finish_activation(
        &mut self,
        completed: LairExplorerActivationTarget,
        succeeded: bool,
    ) -> bool {
        let matches = self
            .pending
            .is_some_and(|pending| match (pending, completed) {
                (
                    LairExplorerPending::Dojo(pending),
                    LairExplorerActivationTarget::Dojo(completed),
                ) => {
                    pending.topology_revision == completed.topology_revision
                        && pending.lair_id == completed.lair_id
                        && pending.dojo_id == completed.dojo_id
                        && pending.capability.action == completed.action
                }
                (
                    LairExplorerPending::Splint(pending),
                    LairExplorerActivationTarget::Splint(completed),
                ) => pending == completed,
                _ => false,
            });
        if matches {
            if succeeded {
                self.pending = None;
            } else {
                self.fail_pending();
            }
        }
        matches && succeeded
    }

    pub(crate) fn activation_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Unrelated and stale failures cannot release a newer activation's ownership.
    pub(crate) fn fail_activation(
        &mut self,
        completed: Option<LairExplorerActivationTarget>,
    ) -> bool {
        let was_pending = self.activation_pending();
        if let Some(completed) = completed {
            self.finish_activation(completed, false);
        }
        was_pending && !self.activation_pending()
    }

    pub(crate) fn clear_pending(&mut self) -> bool {
        self.pending.take().is_some()
    }

    fn fail_pending(&mut self) -> bool {
        let Some(pending) = self.pending.take() else {
            return false;
        };
        if let NavigationNodeId::Splint { splint_id, .. } = pending.id() {
            self.mark_stale_target(splint_id);
        }
        true
    }

    pub(crate) fn mark_stale_target(&mut self, splint_id: SplintId) {
        if let Some(splint) = self
            .view
            .iter_mut()
            .flat_map(|view| &mut view.lairs)
            .flat_map(|lair| &mut lair.dojos)
            .flat_map(|dojo| &mut dojo.splints)
            .find(|splint| {
                matches!(
                    splint.id,
                    NavigationNodeId::Splint {
                        splint_id: id,
                        ..
                    } if id == splint_id
                )
            })
        {
            splint.target.capability.availability = NavigationAvailability::Disabled(
                crate::navigation_projection::NavigationBlocker::Stale,
            );
        }
    }

    pub(crate) fn retryable(&self) -> bool {
        matches!(
            self.load_state,
            ExplorerLoadState::Stale | ExplorerLoadState::Disconnected
        )
    }

    pub(crate) fn set_notice(&mut self, message: &str) {
        let mut text = BoundedTextEditor::new(String::new(), 256, 160, false);
        text.insert(message);
        self.notice = Some(text.text().to_owned());
    }

    pub(crate) fn status_message(&self) -> Option<&str> {
        if let Some(notice) = self.notice.as_deref() {
            return Some(notice);
        }
        if self.load_state == ExplorerLoadState::Disconnected {
            Some("Navigation unavailable · R retry")
        } else if self.load_state == ExplorerLoadState::Stale {
            Some("Navigation is stale · R retry")
        } else if self.load_state == ExplorerLoadState::Refreshing && self.view.is_some() {
            Some("Refreshing navigation…")
        } else if self.view.is_none() {
            Some("Loading navigation…")
        } else if self.rows().is_empty() {
            if self.query.text().is_empty() {
                Some(match self.filter {
                    LairExplorerFilter::All => "No Lairs",
                    LairExplorerFilter::Live => "No live work",
                    LairExplorerFilter::Saved => "No saved Lairs",
                })
            } else {
                Some("No matches")
            }
        } else {
            None
        }
    }

    #[cfg(test)]
    pub(crate) fn breadcrumb(&self) -> Option<String> {
        let view = self.view.as_ref()?;
        let current = view.current?;
        view.lairs.iter().find_map(|lair| {
            lair.dojos.iter().find_map(|dojo| {
                if dojo.id == current {
                    Some(format!("{} / {}", lair.label, dojo.label))
                } else {
                    dojo.splints
                        .iter()
                        .find(|splint| splint.id == current)
                        .map(|splint| format!("{} / {} / {}", lair.label, dojo.label, splint.label))
                }
            })
        })
    }

    pub(crate) fn reveal_current(&mut self) -> bool {
        let Some(view) = &self.view else {
            return false;
        };
        let Some(current) = view.current else {
            return false;
        };
        let Some((lair_id, dojo_id)) = view.lairs.iter().find_map(|lair| {
            lair.dojos.iter().find_map(|dojo| {
                (dojo.id == current || dojo.splints.iter().any(|splint| splint.id == current))
                    .then_some((lair.id, dojo.id))
            })
        }) else {
            return false;
        };
        let mut changed = !self.query.text().is_empty() || self.filter != LairExplorerFilter::All;
        self.filter = LairExplorerFilter::All;
        self.context_pressed = None;
        self.query = BoundedTextEditor::new(String::new(), 256, 64, true);
        changed |= self.expanded.insert(lair_id);
        if matches!(current, NavigationNodeId::Splint { .. }) {
            changed |= self.expanded.insert(dojo_id);
        }
        // The rail lists Lairs only, so it selects the current Lair.
        let target = if self.collapsed {
            lair_node(current)
        } else {
            current
        };
        changed |= self.selected != Some(target) || self.agents_selected;
        self.selected = Some(target);
        self.agents_selected = false;
        changed
    }

    /// The rows in view: the tree, or, while collapsed to the rail, one row
    /// per Lair under the chosen filter, with no search.
    pub(crate) fn rows(&self) -> Vec<LairExplorerRow> {
        let Some(view) = &self.view else {
            return Vec::new();
        };
        let query = if self.collapsed {
            Vec::new()
        } else {
            normalized_query(self.query.text())
        };
        let searching = !query.is_empty();
        let mut rows = Vec::new();
        let filter = self.effective_filter();
        for (ordinal, lair) in view.lairs.iter().enumerate() {
            if (filter == LairExplorerFilter::Saved && !lair.retention.is_protected())
                || (filter == LairExplorerFilter::Live
                    && !lair.dojos.iter().any(|dojo| self.includes_dojo(dojo)))
            {
                continue;
            }
            let lair_matches = matches_query(&lair.label, &query);
            let lair_has_descendant_match = lair
                .dojos
                .iter()
                .filter(|dojo| self.includes_dojo(dojo))
                .any(|dojo| {
                    matches_query(&dojo.label, &query)
                        || dojo.splints.iter().any(|splint| {
                            self.includes_lifecycle(splint.lifecycle)
                                && self.splint_matches(splint, &query)
                        })
                });
            if searching && !lair_matches && !lair_has_descendant_match {
                continue;
            }
            let lair_expanded = self.expanded.contains(&lair.id);
            let aggregate = AgentAggregate::from_statuses(
                lair.dojos
                    .iter()
                    .flat_map(|dojo| &dojo.splints)
                    .filter_map(|splint| splint.agent_status.as_ref()),
            );
            let (whole, part, waiting) = self.lair_reach(lair);
            let accessible_status = with_access_mark(
                mark_text(whole, part, waiting),
                format!(
                    "{} · {}{}",
                    lifecycle_label(lair.lifecycle()),
                    retention_label(lair.retention),
                    self.aggregate_suffix(&aggregate)
                ),
            );
            let status = brief_status([
                waiting_brief(waiting),
                self.aggregate_brief(&aggregate),
                lifecycle_brief(lair.lifecycle()),
                retention_brief(lair.retention),
                mark_text(whole, part, 0).map(|text| lower_first(&text)),
            ]);
            // A rail row switches to its Lair, or expands the tree where
            // nothing opens at once, so it waits only while its switch is
            // pending. A refresh, which disables every Dojo for a moment,
            // does not dim it.
            let rail_pending = self.collapsed
                && self
                    .pending
                    .is_some_and(|pending| lair_node(pending.id()) == lair.id);
            let enabled = !rail_pending;
            rows.push(LairExplorerRow {
                id: lair.id,
                kind: LairExplorerRowKind::Lair,
                label: lair.label.clone(),
                agent_column: self.aggregate_column(&aggregate),
                rail_glyph: self.aggregate_glyph(&aggregate),
                agent_working: self.aggregate_working(&aggregate),
                live: lair
                    .dojos
                    .iter()
                    .flat_map(|dojo| &dojo.splints)
                    .any(|splint| is_live(splint.lifecycle)),
                retention: lair.retention.is_protected().then_some(lair.retention),
                access: LairExplorerAccess::of(whole, part, waiting),
                accessible_status,
                status,
                level: 1,
                ordinal,
                // The rail has no disclosure.
                expanded: (!self.collapsed).then_some(lair_expanded),
                current: lair.current_here,
                enabled,
                pending: rail_pending,
            });
            if self.collapsed || (!lair_expanded && !searching) {
                continue;
            }
            for dojo in &lair.dojos {
                self.push_dojo_rows(&mut rows, dojo, searching, lair_matches, &query);
            }
        }
        rows
    }

    fn push_dojo_rows(
        &self,
        rows: &mut Vec<LairExplorerRow>,
        dojo: &NavigationExplorerDojo,
        searching: bool,
        lair_matches: bool,
        query: &[String],
    ) {
        if !self.includes_dojo(dojo) {
            return;
        }
        let dojo_matches = matches_query(&dojo.label, query);
        let descendant_matches = dojo.splints.iter().any(|splint| {
            self.includes_lifecycle(splint.lifecycle) && self.splint_matches(splint, query)
        });
        if searching && !lair_matches && !dojo_matches && !descendant_matches {
            return;
        }
        let (enabled, blocker) = availability(dojo.target.capability.availability);
        let attachment = match dojo.attachment {
            WindowAttachment::Here if dojo.active_here => "Current",
            WindowAttachment::Here => "Open here",
            WindowAttachment::NotHere => "Not open here",
        };
        let dojo_expanded = self.expanded.contains(&dojo.id);
        let pending = self.pending.is_some_and(|pending| pending.id() == dojo.id);
        let aggregate = AgentAggregate::from_statuses(
            dojo.splints
                .iter()
                .filter_map(|splint| splint.agent_status.as_ref()),
        );
        let base = format!("{} · {attachment}", lifecycle_label(dojo.lifecycle()));
        let (whole, part) = self.dojo_reach(dojo);
        let accessible_status = with_access_mark(
            mark_text(whole, part, 0),
            row_status(
                pending,
                &format!("{base}{}", self.aggregate_suffix(&aggregate)),
                blocker,
            ),
        );
        // The current-row dot already marks the current Dojo, and most Dojos
        // in the list are not open here; only a background tab is worth words.
        let attachment_brief = (dojo.attachment == WindowAttachment::Here && !dojo.active_here)
            .then(|| "open here".to_owned());
        let status = brief_status([
            pending.then(|| "pending".to_owned()),
            self.aggregate_brief(&aggregate),
            lifecycle_brief(dojo.lifecycle()),
            attachment_brief,
            blocker.map(str::to_owned),
            mark_text(whole, part, 0).map(|text| lower_first(&text)),
        ]);
        rows.push(LairExplorerRow {
            id: dojo.id,
            kind: LairExplorerRowKind::Dojo,
            label: dojo.label.clone(),
            agent_column: self.aggregate_column(&aggregate),
            rail_glyph: None,
            agent_working: self.aggregate_working(&aggregate),
            live: dojo.splints.iter().any(|splint| is_live(splint.lifecycle)),
            retention: None,
            access: LairExplorerAccess::of(whole, part, 0),
            accessible_status,
            status,
            level: 2,
            ordinal: 0,
            expanded: Some(dojo_expanded),
            current: dojo.active_here,
            enabled: enabled && !pending,
            pending,
        });
        if !dojo_expanded && !searching {
            return;
        }
        for splint in &dojo.splints {
            if !self.includes_lifecycle(splint.lifecycle) {
                continue;
            }
            if searching && !lair_matches && !dojo_matches && !self.splint_matches(splint, query) {
                continue;
            }
            rows.push(self.splint_row(splint));
        }
    }

    fn splint_row(&self, splint: &NavigationExplorerSplint) -> LairExplorerRow {
        let (enabled, blocker) = availability(splint.target.capability.availability);
        let state = format!(
            "{}{}",
            lifecycle_label(splint.lifecycle),
            self.splint_suffix(splint.agent_status.as_ref())
        );
        let pending = self
            .pending
            .is_some_and(|pending| pending.id() == splint.id);
        let reach = self.splint_reach(splint);
        let accessible_status = with_access_mark(
            mark_text(reach, AccessReach::None, 0),
            row_status(pending, &state, blocker),
        );
        let status = brief_status([
            pending.then(|| "pending".to_owned()),
            self.splint_brief(splint.agent_status.as_ref()),
            lifecycle_brief(splint.lifecycle),
            blocker.map(str::to_owned),
            mark_text(reach, AccessReach::None, 0).map(|text| lower_first(&text)),
        ]);
        LairExplorerRow {
            id: splint.id,
            kind: LairExplorerRowKind::Splint,
            label: splint.label.clone(),
            agent_column: self.splint_column(splint),
            rail_glyph: None,
            agent_working: self.agent_column_visible()
                && self.agent_status_available()
                && agent_status::shows_working(splint.agent_status.as_ref()),
            live: is_live(splint.lifecycle),
            retention: None,
            access: LairExplorerAccess::of(reach, AccessReach::None, 0),
            accessible_status,
            status,
            level: 3,
            ordinal: 0,
            expanded: None,
            current: splint.focused_here,
            enabled: enabled && !pending,
            pending,
        }
    }

    /// The expanded Explorer's agents section: every Splint reporting an
    /// agent status, most urgent first (needs input, error, working, done,
    /// idle, and any stale status after every fresh one), in tree order
    /// within a state. It ignores the filter and the search, so an agent
    /// asking for input is never hidden. Empty on the rail and while agent
    /// status is off or unavailable.
    pub(crate) fn agent_rows(&self) -> Vec<LairExplorerAgentRow> {
        if self.collapsed || !self.agent_status_available() {
            return Vec::new();
        }
        let Some(view) = &self.view else {
            return Vec::new();
        };
        let mut agents = Vec::new();
        for lair in &view.lairs {
            for dojo in &lair.dojos {
                for splint in &dojo.splints {
                    let Some(status) = &splint.agent_status else {
                        continue;
                    };
                    let location = format!("{} / {}", lair.label, dojo.label);
                    let mut row = self.splint_row(splint);
                    row.level = 1;
                    row.accessible_status = format!("{location} · {}", row.accessible_status);
                    agents.push((
                        agent_status::rank(status),
                        LairExplorerAgentRow { row, location },
                    ));
                }
            }
        }
        agents.sort_by_key(|(rank, _)| *rank);
        agents.into_iter().map(|(_, agent)| agent).collect()
    }

    /// The agents section's rows the keys, the pointer, and assistive
    /// technology reach: [`Self::agent_rows`] while the layout shows the
    /// section, none while it is hidden.
    pub(crate) fn shown_agent_rows(&self) -> Vec<LairExplorerAgentRow> {
        if self.agents_shown {
            self.agent_rows()
        } else {
            Vec::new()
        }
    }

    /// Whether the shown agents section lists the Splint `id`.
    fn lists_agent(&self, id: NavigationNodeId) -> bool {
        self.shown_agent_rows()
            .iter()
            .any(|agent| agent.row.id == id)
    }

    /// Records whether the layout shows the agents section. While it is
    /// hidden its rows are skipped, and a selected agents row passes the
    /// selection to the Splint's tree row, else its Lair's. Returns whether
    /// that changed.
    pub(crate) fn set_agents_shown(&mut self, shown: bool) -> bool {
        if self.agents_shown == shown {
            return false;
        }
        self.agents_shown = shown;
        self.ensure_selection_visible();
        true
    }

    /// The selected row, of the tree or of the agents section.
    pub(crate) fn selected(&self) -> Option<NavigationNodeId> {
        self.selected
    }

    /// The selection when it is a row of the tree.
    pub(crate) fn tree_selected(&self) -> Option<NavigationNodeId> {
        self.selected.filter(|_| !self.agents_selected)
    }

    /// The selection when it is a row of the agents section.
    pub(crate) fn agent_selected(&self) -> Option<NavigationNodeId> {
        self.selected.filter(|_| self.agents_selected)
    }

    pub(crate) fn select(&mut self, id: NavigationNodeId) -> bool {
        if self.rows().iter().any(|row| row.id == id)
            && (self.selected != Some(id) || self.agents_selected)
        {
            self.selected = Some(id);
            self.agents_selected = false;
            true
        } else {
            false
        }
    }

    /// Selects the agents section's row for the Splint `id`, when it is
    /// listed there.
    pub(crate) fn select_agent(&mut self, id: NavigationNodeId) -> bool {
        let Some(index) = self
            .shown_agent_rows()
            .iter()
            .position(|agent| agent.row.id == id)
        else {
            return false;
        };
        let changed = self.selected != Some(id) || !self.agents_selected;
        self.selected = Some(id);
        self.agents_selected = true;
        self.agent_index = index;
        changed
    }

    /// Moves the selection to `id` as its menu activates it, so the
    /// selection and the footer's status follow the row acted on: to its
    /// agents row while the selection is in the agents section or the tree
    /// does not show it, else to its tree row.
    pub(crate) fn select_activated(&mut self, id: NavigationNodeId) -> bool {
        let in_tree = self.rows().iter().any(|row| row.id == id);
        if (self.agents_selected || !in_tree)
            && self
                .shown_agent_rows()
                .iter()
                .any(|agent| agent.row.id == id)
        {
            return self.select_agent(id);
        }
        self.select(id)
    }

    /// The order the keys move through: the tree's rows, then the shown
    /// agents section's, each with whether it is an agent row.
    fn navigation_order(&self) -> Vec<(NavigationNodeId, bool)> {
        self.rows()
            .iter()
            .map(|row| (row.id, false))
            .chain(
                self.shown_agent_rows()
                    .iter()
                    .map(|agent| (agent.row.id, true)),
            )
            .collect()
    }

    /// Moves the selection `delta` rows through the tree and on into the
    /// agents section below it, or back.
    pub(crate) fn move_selection(&mut self, delta: isize) -> bool {
        let order = self.navigation_order();
        if order.is_empty() {
            self.selected = None;
            self.agents_selected = false;
            return false;
        }
        // With no position in the list, as when a search leaves the tree
        // empty, the first step lands on the first row, of either list.
        let Some(current) = self.selected.and_then(|selected| {
            order
                .iter()
                .position(|&entry| entry == (selected, self.agents_selected))
        }) else {
            return self.select_entry(Some(order[0]));
        };
        let next = if delta.is_negative() {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            current
                .saturating_add(delta.unsigned_abs())
                .min(order.len() - 1)
        };
        self.select_entry(Some(order[next]))
    }

    /// Selects the first row of the tree, or the last of the agents section
    /// below it when `end`.
    pub(crate) fn select_edge(&mut self, end: bool) -> bool {
        let order = self.navigation_order();
        self.select_entry(if end { order.last() } else { order.first() }.copied())
    }

    fn select_entry(&mut self, entry: Option<(NavigationNodeId, bool)>) -> bool {
        let next = entry.map(|(id, _)| id);
        let agents = entry.is_some_and(|(_, agents)| agents);
        let changed = self.selected != next || self.agents_selected != agents;
        self.selected = next;
        self.agents_selected = agents;
        if let Some(id) = next.filter(|_| agents)
            && let Some(index) = self
                .shown_agent_rows()
                .iter()
                .position(|agent| agent.row.id == id)
        {
            self.agent_index = index;
        }
        changed
    }

    /// Opens or closes the selected row's disclosure. The rail has none.
    pub(crate) fn toggle_selected(&mut self) -> bool {
        let Some(selected @ (NavigationNodeId::Lair(_) | NavigationNodeId::Dojo { .. })) =
            self.selected.filter(|_| !self.collapsed)
        else {
            return false;
        };
        if !self.expanded.remove(&selected) {
            self.expanded.insert(selected);
        }
        self.ensure_selection_visible();
        true
    }

    pub(crate) fn move_left(&mut self) -> bool {
        // An agents row has no parent there to move to.
        let Some(selected) = self
            .selected
            .filter(|_| !self.collapsed && !self.agents_selected)
        else {
            return false;
        };
        if self.query.text().is_empty()
            && matches!(
                selected,
                NavigationNodeId::Lair(_) | NavigationNodeId::Dojo { .. }
            )
            && self.expanded.remove(&selected)
        {
            return true;
        }
        let Some(view) = &self.view else {
            return false;
        };
        let parent = view.lairs.iter().find_map(|lair| {
            lair.dojos.iter().find_map(|dojo| {
                if dojo.id == selected {
                    Some(lair.id)
                } else if dojo.splints.iter().any(|splint| splint.id == selected) {
                    Some(dojo.id)
                } else {
                    None
                }
            })
        });
        if parent.is_some() && parent != self.selected {
            self.selected = parent;
            true
        } else {
            false
        }
    }

    pub(crate) fn move_right(&mut self) -> bool {
        let Some(selected) = self
            .selected
            .filter(|_| !self.collapsed && !self.agents_selected)
        else {
            return false;
        };
        let rows = self.rows();
        let Some(index) = rows.iter().position(|row| row.id == selected) else {
            return false;
        };
        let first_child = rows
            .get(index + 1)
            .filter(|row| row.level > rows[index].level)
            .map(|row| row.id);
        if !matches!(
            selected,
            NavigationNodeId::Lair(_) | NavigationNodeId::Dojo { .. }
        ) {
            return false;
        }
        if self.query.text().is_empty() && !self.expanded.contains(&selected) {
            return self.expanded.insert(selected);
        }
        if let Some(first) = first_child {
            self.selected = Some(first);
            true
        } else {
            false
        }
    }

    pub(crate) fn decision(&mut self) -> Option<LairExplorerDecision> {
        let selected = self.selected?;
        if self.pending.is_some_and(|pending| pending.id() == selected) {
            return None;
        }
        if matches!(selected, NavigationNodeId::Lair(_)) {
            if self.collapsed {
                return self.rail_decision(selected);
            }
            self.toggle_selected();
            return Some(LairExplorerDecision::Toggle(selected));
        }
        self.activation_decision(selected)
    }

    /// A rail row's switch to its Lair: opens the Lair's Dojo that
    /// [`rail_dojo`] picks. A Lair with no Dojo that opens at once, such as
    /// one that only restores behind a confirmation, instead expands the
    /// Explorer to the tree with that Lair selected and open, so its Dojo is
    /// chosen there. `None` for a row not in the rail, one whose switch is
    /// already pending, or a Lair whose Dojos wait on navigation, as while it
    /// refreshes.
    fn rail_decision(&mut self, lair_id: NavigationNodeId) -> Option<LairExplorerDecision> {
        if !self
            .rows()
            .iter()
            .any(|row| row.id == lair_id && row.enabled)
        {
            return None;
        }
        let lair = self
            .view
            .as_ref()?
            .lairs
            .iter()
            .find(|lair| lair.id == lair_id)?;
        if let Some(dojo) = rail_dojo(lair) {
            return Some(LairExplorerDecision::OpenDojo(dojo.target));
        }
        if rail_waits(lair) {
            return None;
        }
        self.expand_from_rail(lair_id);
        Some(LairExplorerDecision::ExpandRail(lair_id))
    }

    /// Expands the rail to the tree with `lair_id` selected and open. A
    /// query left in the tree that would hide that Lair is cleared.
    fn expand_from_rail(&mut self, lair_id: NavigationNodeId) {
        self.set_collapsed(false);
        self.expanded.insert(lair_id);
        if !self.rows().iter().any(|row| row.id == lair_id) {
            self.query = BoundedTextEditor::new(String::new(), 256, 64, true);
        }
        self.selected = Some(lair_id);
        self.agents_selected = false;
        self.ensure_selection_visible();
    }

    pub(crate) fn activation_decision(
        &self,
        selected: NavigationNodeId,
    ) -> Option<LairExplorerDecision> {
        if !self.rows().iter().any(|row| row.id == selected) && !self.lists_agent(selected) {
            return None;
        }
        let view = self.view.as_ref()?;
        for dojo in view.lairs.iter().flat_map(|lair| &lair.dojos) {
            if dojo.id == selected {
                return dojo
                    .target
                    .capability
                    .is_enabled()
                    .then_some(LairExplorerDecision::OpenDojo(dojo.target));
            }
            if let Some(splint) = dojo.splints.iter().find(|splint| splint.id == selected) {
                return splint
                    .target
                    .capability
                    .is_enabled()
                    .then_some(LairExplorerDecision::FocusSplint(splint.target));
            }
        }
        None
    }

    /// Starts typing into the search field. The rail has none.
    pub(crate) fn begin_search(&mut self) -> bool {
        if self.collapsed {
            return false;
        }
        let changed = !self.search_active;
        self.search_active = true;
        changed
    }

    pub(crate) fn append_search(&mut self, text: &str) -> bool {
        if !self.search_active || !self.query.insert(text) {
            return false;
        }
        self.ensure_selection_visible();
        true
    }

    pub(crate) fn backspace_search(&mut self) -> bool {
        if !self.search_active || !self.query.backspace() {
            return false;
        }
        self.ensure_selection_visible();
        true
    }

    /// Clears a search, else returns to the terminal; the rail, which shows
    /// no search, always returns.
    pub(crate) fn escape(&mut self) -> bool {
        if self.collapsed || self.query.text().is_empty() {
            self.return_to_terminal();
        } else {
            self.query = BoundedTextEditor::new(String::new(), 256, 64, true);
            self.ensure_selection_visible();
        }
        true
    }

    fn ensure_selection_visible(&mut self) {
        // A selected agents row stays while the shown section lists it.
        // When it goes, the row now in its place takes the selection; with
        // no row left there, the same Splint's tree row does, else its
        // Lair's, else the first row.
        if self.agents_selected {
            let agents = self.shown_agent_rows();
            if let Some(index) = self
                .selected
                .and_then(|selected| agents.iter().position(|agent| agent.row.id == selected))
            {
                self.agent_index = index;
                return;
            }
            if let Some(agent) = agents.get(self.agent_index.min(agents.len().saturating_sub(1))) {
                self.agent_index = self.agent_index.min(agents.len() - 1);
                self.selected = Some(agent.row.id);
                return;
            }
            self.agents_selected = false;
            let rows = self.rows();
            self.selected = self.selected.and_then(|selected| {
                [selected, lair_node(selected)]
                    .into_iter()
                    .find(|id| rows.iter().any(|row| row.id == *id))
            });
        }
        // The rail keeps the selection on the selected row's Lair.
        if self.collapsed {
            self.selected = self.selected.map(lair_node);
        }
        let rows = self.rows();
        if !self
            .selected
            .is_some_and(|selected| rows.iter().any(|row| row.id == selected))
        {
            self.selected = rows.first().map(|row| row.id);
        }
    }
}

fn view_contains(view: &NavigationExplorerView, selected: NavigationNodeId) -> bool {
    view.lairs.iter().any(|lair| {
        lair.id == selected
            || lair.dojos.iter().any(|dojo| {
                dojo.id == selected || dojo.splints.iter().any(|splint| splint.id == selected)
            })
    })
}

fn nearest_surviving_parent(
    previous: &NavigationExplorerView,
    current: &NavigationExplorerView,
    selected: NavigationNodeId,
) -> Option<NavigationNodeId> {
    previous.lairs.iter().find_map(|lair| {
        lair.dojos.iter().find_map(|dojo| {
            let selected_child = dojo.splints.iter().any(|splint| splint.id == selected);
            if selected_child && view_contains(current, dojo.id) {
                Some(dojo.id)
            } else if (selected_child || dojo.id == selected) && view_contains(current, lair.id) {
                Some(lair.id)
            } else {
                None
            }
        })
    })
}

/// Puts the agent access mark first, in words, so a narrow Explorer keeps it
/// and assistive technology reads it with the row (ADR 0014).
fn with_access_mark(mark: Option<String>, status: String) -> String {
    match mark {
        Some(mark) => format!("{mark} · {status}"),
        None => status,
    }
}

fn row_status(pending: bool, state: &str, blocker: Option<&str>) -> String {
    if pending {
        format!("Pending · {state}")
    } else {
        blocker.map_or_else(|| state.to_owned(), |reason| format!("{state} · {reason}"))
    }
}

fn availability(availability: NavigationAvailability) -> (bool, Option<&'static str>) {
    match availability {
        NavigationAvailability::Enabled => (true, None),
        NavigationAvailability::Disabled(blocker) => (false, Some(blocker.message())),
    }
}

/// Whether a Splint is live: running or starting, as the Live filter counts.
const fn is_live(lifecycle: NavigationLifecycle) -> bool {
    matches!(
        lifecycle,
        NavigationLifecycle::Running | NavigationLifecycle::Starting
    )
}

/// The Lair row a node belongs to: the node itself for a Lair.
const fn lair_node(id: NavigationNodeId) -> NavigationNodeId {
    match id {
        NavigationNodeId::Lair(_) => id,
        NavigationNodeId::Dojo { lair_id, .. } | NavigationNodeId::Splint { lair_id, .. } => {
            NavigationNodeId::Lair(lair_id)
        }
    }
}

/// Whether a Dojo opens at once, with no confirmation: it is activated or
/// attached, never restored, which asks first.
const fn opens_directly(dojo: &NavigationExplorerDojo) -> bool {
    matches!(
        dojo.target.capability.action,
        NavigationAction::ActivateDojo | NavigationAction::AttachDojo
    )
}

/// The Dojo a rail row opens to switch to its Lair, among those that open
/// at once and can now: the Lair's active Dojo here, else one open in a tab
/// here, else a live one, else the first.
fn rail_dojo(lair: &NavigationExplorerLair) -> Option<&NavigationExplorerDojo> {
    let openable = || {
        lair.dojos
            .iter()
            .filter(|dojo| opens_directly(dojo) && dojo.target.capability.is_enabled())
    };
    openable()
        .find(|dojo| dojo.active_here)
        .or_else(|| openable().find(|dojo| dojo.attachment == WindowAttachment::Here))
        .or_else(|| openable().find(|dojo| is_live(dojo.lifecycle())))
        .or_else(|| openable().next())
}

/// Whether a rail row's Lair has a Dojo that would open at once but waits
/// on navigation, as while it refreshes or reconnects: its click then does
/// nothing yet, rather than expanding the tree.
fn rail_waits(lair: &NavigationExplorerLair) -> bool {
    lair.dojos.iter().any(|dojo| {
        opens_directly(dojo)
            && matches!(
                dojo.target.capability.availability,
                NavigationAvailability::Disabled(
                    NavigationBlocker::Refreshing
                        | NavigationBlocker::Stale
                        | NavigationBlocker::Disconnected
                )
            )
    })
}

const fn lifecycle_label(lifecycle: NavigationLifecycle) -> &'static str {
    match lifecycle {
        NavigationLifecycle::Starting => "Starting",
        NavigationLifecycle::Running => "Running",
        NavigationLifecycle::Mixed => "Mixed state",
        NavigationLifecycle::Exited => "Stopped",
        NavigationLifecycle::Restorable => "Stopped · Restorable",
        NavigationLifecycle::Unavailable => "Unavailable",
    }
}

fn retention_label(retention: LairRetention) -> &'static str {
    match retention {
        LairRetention::Disposable => "Disposable",
        LairRetention::Saved => "Saved",
        LairRetention::Pinned => "Pinned",
    }
}

/// The lifecycle on the footer's short status line: nothing for a running
/// row, the ordinary case the live dot already shows.
fn lifecycle_brief(lifecycle: NavigationLifecycle) -> Option<String> {
    let text = match lifecycle {
        NavigationLifecycle::Running => return None,
        NavigationLifecycle::Starting => "starting",
        NavigationLifecycle::Mixed => "mixed state",
        NavigationLifecycle::Exited => "stopped",
        NavigationLifecycle::Restorable => "restorable",
        NavigationLifecycle::Unavailable => "unavailable",
    };
    Some(text.to_owned())
}

/// The retention on the footer's short status line: nothing for a
/// disposable Lair, the default every new Lair starts with.
fn retention_brief(retention: LairRetention) -> Option<String> {
    let text = match retention {
        LairRetention::Disposable => return None,
        LairRetention::Saved => "saved",
        LairRetention::Pinned => "pinned",
    };
    Some(text.to_owned())
}

/// The footer's short status line: the parts there are, in the order given,
/// joined by ` · `; empty when there are none.
fn brief_status(parts: impl IntoIterator<Item = Option<String>>) -> String {
    parts
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// A waiting access request on the footer's short status line, e.g.
/// "asking for access" or "2 asking for access"; the access icon and the
/// accessible text already say an agent is asking.
fn waiting_brief(waiting: u32) -> Option<String> {
    match waiting {
        0 => None,
        1 => Some("asking for access".to_owned()),
        count => Some(format!("{count} asking for access")),
    }
}

/// Splinterm's own sentence-case agent words, such as access marks, in the
/// footer's lowercase style. Blocker messages keep their own case, since
/// they name product nouns such as Dojo and Window.
fn lower_first(text: &str) -> String {
    let mut characters = text.chars();
    characters.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(characters).collect()
    })
}

fn normalized_query(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

fn matches_query(label: &str, query: &[String]) -> bool {
    let label = label.to_lowercase();
    query.iter().all(|token| label.contains(token))
}

#[cfg(test)]
pub(super) mod tests {
    use splinterm_core::{DojoId, LairId, SplintId, TopologyRevision};

    use super::*;
    use crate::frontend::topology::SessionPickerTarget;
    use crate::navigation_projection::{
        NavigationAction, NavigationAvailability, NavigationCapability, NavigationExplorerDojo,
        NavigationExplorerLair, NavigationExplorerSplint, NavigationExplorerSplintTarget,
        NavigationExplorerTarget,
    };

    #[test]
    fn search_looks_through_every_lair_and_clearing_it_returns_to_the_filter() {
        let view = view();
        let ids = view.lairs.iter().map(|lair| lair.id).collect::<Vec<_>>();
        let stopped = view.lairs[1].label.clone();
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(view);
        assert_eq!(explorer.filter(), LairExplorerFilter::Live);
        let live_rows = explorer.rows();
        assert!(live_rows.iter().all(|row| row.id != ids[1]));
        explorer.set_search(stopped);
        assert!(explorer.rows().iter().any(|row| row.id == ids[1]));
        assert_eq!(explorer.filter(), LairExplorerFilter::Live);
        explorer.set_search(String::new());
        assert_eq!(explorer.rows(), live_rows);
    }

    #[test]
    fn filters_show_live_first_and_cycle_live_saved_all() {
        assert_eq!(
            LairExplorerFilter::ALL,
            [
                LairExplorerFilter::Live,
                LairExplorerFilter::Saved,
                LairExplorerFilter::All
            ]
        );
        assert_eq!(LairExplorerFilter::default(), LairExplorerFilter::Live);
        assert_eq!(LairExplorerUi::default().filter(), LairExplorerFilter::Live);
        let mut filter = LairExplorerFilter::Live;
        let mut seen = Vec::new();
        for _ in 0..3 {
            filter = filter.next();
            seen.push(filter.label());
        }
        assert_eq!(seen, ["Saved", "All", "Live"]);
    }

    #[test]
    fn activity_filters_keep_identity_order_and_default_stopped_collapse() {
        let view = view();
        let ids = view.lairs.iter().map(|lair| lair.id).collect::<Vec<_>>();
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(view);
        assert_eq!(explorer.filter(), LairExplorerFilter::Live);
        explorer.set_filter(LairExplorerFilter::All);
        assert!(
            explorer
                .rows()
                .iter()
                .all(|row| row.expanded == Some(false))
        );
        explorer.set_filter(LairExplorerFilter::Saved);
        assert_eq!(
            explorer.rows().iter().map(|row| row.id).collect::<Vec<_>>(),
            ids
        );
        explorer.set_filter(LairExplorerFilter::Live);
        assert_eq!(
            explorer.rows().iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![ids[0]]
        );
        explorer.set_filter(LairExplorerFilter::All);
        assert_eq!(
            explorer.rows().iter().map(|row| row.id).collect::<Vec<_>>(),
            ids
        );
    }

    #[test]
    fn saved_filter_hides_disposable_current_until_reveal() {
        let mut view = view();
        view.lairs[0].retention = LairRetention::Disposable;
        let current = view.current.unwrap();
        let retained = view.lairs[1].id;
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(view);
        explorer.reveal_current();
        explorer.focus();
        explorer.context_press(Some(current));
        assert!(explorer.context_press_pending());
        assert!(explorer.set_filter(LairExplorerFilter::Saved));
        assert_eq!(explorer.selected(), Some(retained));
        assert!(!explorer.rows().iter().any(|row| row.id == current));
        assert!(!explorer.can_context(current));
        assert!(explorer.activation_decision(current).is_none());
        assert!(!explorer.context_press_pending());
        assert!(explorer.reveal_current());
        assert_eq!(explorer.filter(), LairExplorerFilter::All);
        assert_eq!(explorer.selected(), Some(current));
        assert!(explorer.rows().iter().any(|row| row.id == current));
    }

    #[test]
    fn live_filter_removes_stopped_selection_on_refresh_without_reordering() {
        let mut view = view();
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(view.clone());
        explorer.reveal_current();
        explorer.set_filter(LairExplorerFilter::Live);
        assert!(explorer.selected().is_some());
        view.lairs[0].dojos[0].splints[0].lifecycle = NavigationLifecycle::Restorable;
        explorer.set_view(view.clone());
        assert!(explorer.rows().is_empty());
        assert_eq!(explorer.selected(), None);
        assert_eq!(explorer.status_message(), Some("No live work"));
        view.lairs[0].dojos[0].splints[0].lifecycle = NavigationLifecycle::Starting;
        explorer.set_view(view);
        assert!(!explorer.rows().is_empty());
        assert!(explorer.selected().is_some());
    }

    #[test]
    fn live_filter_excludes_non_live_mixed_aggregates_but_saved_keeps_them() {
        let mut view = view();
        let splints = &mut view.lairs[0].dojos[0].splints;
        splints[0].lifecycle = NavigationLifecycle::Restorable;
        let mut unavailable = splints[0].clone();
        unavailable.target.splint_id = SplintId::new();
        unavailable.id = NavigationNodeId::Splint {
            lair_id: unavailable.target.lair_id,
            dojo_id: unavailable.target.dojo_id,
            splint_id: unavailable.target.splint_id,
        };
        unavailable.lifecycle = NavigationLifecycle::Unavailable;
        splints.push(unavailable);
        assert_eq!(
            view.lairs[0].dojos[0].lifecycle(),
            NavigationLifecycle::Mixed
        );
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(view);
        explorer.set_filter(LairExplorerFilter::Live);
        assert!(explorer.rows().is_empty());
        explorer.set_filter(LairExplorerFilter::Saved);
        assert_eq!(explorer.rows().len(), 2);
        assert!(
            explorer
                .rows()
                .iter()
                .all(|row| row.expanded == Some(false))
        );
    }

    #[test]
    fn search_under_live_finds_stopped_splints_with_their_ancestors() {
        let mut view = view();
        let first = &mut view.lairs[0].dojos[0].splints[0];
        first.label = "stopped-only-label".into();
        first.lifecycle = NavigationLifecycle::Restorable;
        let mut live = first.clone();
        live.target.splint_id = SplintId::new();
        live.id = NavigationNodeId::Splint {
            lair_id: live.target.lair_id,
            dojo_id: live.target.dojo_id,
            splint_id: live.target.splint_id,
        };
        live.label = "live-label".into();
        live.lifecycle = NavigationLifecycle::Running;
        view.lairs[0].dojos[0].splints.push(live);
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(view);
        explorer.set_filter(LairExplorerFilter::Live);
        // Search looks through every Lair: a stopped Splint is found under
        // Live, with its Lair and Dojo, exactly as under All.
        explorer.set_search("stopped-only-label".into());
        let under_live = explorer.rows();
        assert_eq!(under_live.len(), 3);
        explorer.set_search("live-label".into());
        assert_eq!(explorer.rows().len(), 3);
        explorer.set_filter(LairExplorerFilter::All);
        explorer.set_search("stopped-only-label".into());
        assert_eq!(explorer.rows(), under_live);
        // Without a search, Live still hides the stopped-only branch's Splint.
        explorer.set_filter(LairExplorerFilter::Live);
        explorer.set_search(String::new());
        assert!(
            explorer
                .rows()
                .iter()
                .all(|row| row.label != "stopped-only-label")
        );
    }

    pub(crate) fn view() -> NavigationExplorerView {
        let lair = LairId::new();
        let other_lair = LairId::new();
        let dojo = DojoId::new();
        let other_dojo = DojoId::new();
        let splint = SplintId::new();
        NavigationExplorerView {
            endpoint_namespace: "local".into(),
            endpoint_generation: 1,
            topology_revision: TopologyRevision::new(7),
            freshness: EndpointFreshness::Current,
            current: Some(NavigationNodeId::Splint {
                lair_id: lair,
                dojo_id: dojo,
                splint_id: splint,
            }),
            lairs: vec![
                NavigationExplorerLair {
                    id: NavigationNodeId::Lair(lair),
                    label: "work".into(),
                    retention: LairRetention::Saved,
                    can_create_dojo: true,
                    current_here: true,
                    dojos: vec![NavigationExplorerDojo {
                        id: NavigationNodeId::Dojo {
                            lair_id: lair,
                            dojo_id: dojo,
                        },
                        label: "editor".into(),
                        attachment: WindowAttachment::Here,
                        active_here: true,
                        target: NavigationExplorerTarget {
                            topology_revision: TopologyRevision::new(7),
                            lair_id: lair,
                            dojo_id: dojo,
                            capability: NavigationCapability {
                                action: NavigationAction::ActivateDojo,
                                availability: NavigationAvailability::Enabled,
                            },
                        },
                        splints: vec![NavigationExplorerSplint {
                            id: NavigationNodeId::Splint {
                                lair_id: lair,
                                dojo_id: dojo,
                                splint_id: splint,
                            },
                            label: "shell".into(),
                            lifecycle: NavigationLifecycle::Running,
                            focused_here: true,
                            target: NavigationExplorerSplintTarget {
                                topology_revision: TopologyRevision::new(7),
                                lair_id: lair,
                                dojo_id: dojo,
                                splint_id: splint,
                                live_incarnation: Some(3),
                                last_incarnation: Some(3),
                                capability: NavigationCapability {
                                    action: NavigationAction::FocusSplint,
                                    availability: NavigationAvailability::Enabled,
                                },
                            },
                            agent_status: None,
                        }],
                    }],
                },
                NavigationExplorerLair {
                    id: NavigationNodeId::Lair(other_lair),
                    label: "notes".into(),
                    retention: LairRetention::Pinned,
                    can_create_dojo: true,
                    current_here: false,
                    dojos: vec![NavigationExplorerDojo {
                        id: NavigationNodeId::Dojo {
                            lair_id: other_lair,
                            dojo_id: other_dojo,
                        },
                        label: "draft".into(),
                        attachment: WindowAttachment::NotHere,
                        active_here: false,
                        target: NavigationExplorerTarget {
                            topology_revision: TopologyRevision::new(7),
                            lair_id: other_lair,
                            dojo_id: other_dojo,
                            capability: NavigationCapability {
                                action: NavigationAction::AttachDojo,
                                availability: NavigationAvailability::Enabled,
                            },
                        },
                        splints: Vec::new(),
                    }],
                },
            ],
        }
    }

    #[test]
    fn context_click_captures_non_current_lair_without_activation_or_selection() {
        let mut explorer = LairExplorerUi::default();
        // The target is a stopped Lair, which the default Live filter hides.
        explorer.set_filter(LairExplorerFilter::All);
        let view = view();
        let target = view.lairs[1].id;
        explorer.set_view(view);
        explorer.toggle_visibility();
        let selected = explorer.selected;
        let rows = explorer.rows();
        explorer.context_press(Some(target));
        assert_eq!(explorer.context_release(Some(target)), Some(target));
        assert_eq!(explorer.selected, selected);
        assert_eq!(explorer.rows(), rows);
        assert_eq!(explorer.pending, None);
        assert!(!explorer.focused());
    }

    #[test]
    fn context_click_requires_matching_uncancelled_visible_fresh_lair() {
        let mut explorer = LairExplorerUi::default();
        let view = view();
        let target = view.lairs[1].id;
        let other = view.lairs[0].id;
        let dojo = view.lairs[0].dojos[0].id;
        explorer.set_view(view.clone());
        explorer.toggle_visibility();
        assert_eq!(explorer.context_release(Some(target)), None);
        explorer.context_press(Some(target));
        assert_eq!(explorer.context_release(Some(other)), None);
        explorer.context_press(Some(target));
        explorer.cancel_context_press();
        assert_eq!(explorer.context_release(Some(target)), None);
        explorer.context_press(Some(dojo));
        assert_eq!(explorer.context_release(Some(dojo)), None);
        explorer.context_press(Some(target));
        explorer.begin_refresh();
        assert_eq!(explorer.context_release(Some(target)), None);
        explorer.set_view(view.clone());
        explorer.context_press(Some(target));
        explorer.mark_disconnected();
        assert_eq!(explorer.context_release(Some(target)), None);
        explorer.set_view(view.clone());
        explorer.context_press(Some(target));
        explorer.set_search("work".into());
        assert_eq!(explorer.context_release(Some(target)), None);
        explorer.set_search(String::new());
        explorer.context_press(Some(target));
        let mut deleted = view;
        deleted.lairs.remove(1);
        explorer.set_view(deleted);
        assert_eq!(explorer.context_release(Some(target)), None);
        explorer.context_press(Some(other));
        explorer.toggle_visibility();
        assert_eq!(explorer.context_release(Some(other)), None);
    }

    #[test]
    fn context_click_on_visible_dojo_and_splint_keeps_selection_and_rejects_new_revision() {
        let mut explorer = LairExplorerUi::default();
        let view = view();
        explorer.set_view(view.clone());
        explorer.toggle_visibility();
        explorer.reveal_current();
        let selected = explorer.selected();
        for node in [
            view.lairs[0].dojos[0].id,
            view.lairs[0].dojos[0].splints[0].id,
        ] {
            explorer.context_press(Some(node));
            assert_eq!(explorer.context_release(Some(node)), Some(node));
            assert_eq!(explorer.selected(), selected);
            assert!(!explorer.activation_pending());
            explorer.context_press(Some(node));
            let mut updated = view.clone();
            updated.topology_revision = TopologyRevision::new(8);
            explorer.set_view(updated);
            assert_eq!(explorer.context_release(Some(node)), None);
            explorer.set_view(view.clone());
        }
    }

    #[test]
    fn explicit_context_focus_reuses_activation_ownership_without_selecting_the_row() {
        let mut explorer = LairExplorerUi::default();
        let view = view();
        let dojo = view.lairs[0].dojos[0].id;
        let splint = view.lairs[0].dojos[0].splints[0].target;
        explorer.set_view(view);
        explorer.toggle_visibility();
        explorer.reveal_current();
        explorer.select(dojo);
        let decision = explorer
            .activation_decision(NavigationNodeId::Splint {
                lair_id: splint.lair_id,
                dojo_id: splint.dojo_id,
                splint_id: splint.splint_id,
            })
            .unwrap();
        assert!(explorer.set_pending(decision));
        assert_eq!(explorer.selected(), Some(dojo));
        assert!(explorer.finish_activation(LairExplorerActivationTarget::Splint(splint), true));
        assert!(!explorer.activation_pending());
    }

    #[test]
    fn explorer_is_default_hidden_and_focus_actions_are_independent() {
        let mut explorer = LairExplorerUi::default();
        assert!(!explorer.visible());
        explorer.toggle_visibility();
        assert!(explorer.visible());
        assert!(!explorer.focused());
        explorer.focus();
        assert!(explorer.focused());
        explorer.return_to_terminal();
        assert!(explorer.visible());
        assert!(!explorer.focused());
    }

    #[test]
    fn terminal_targets_return_focus_while_disclosures_keep_it() {
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(view());
        assert!(explorer.reveal_current());
        assert!(
            explorer
                .decision()
                .is_some_and(LairExplorerDecision::returns_focus_to_terminal)
        );
        assert!(explorer.move_left());
        assert!(
            explorer
                .decision()
                .is_some_and(LairExplorerDecision::returns_focus_to_terminal)
        );
        assert!(explorer.move_left()); // Collapse the expanded Dojo without navigating.
        assert!(matches!(
            explorer.selected(),
            Some(NavigationNodeId::Dojo { .. })
        ));
        assert!(explorer.move_left()); // Then move to its Lair.
        assert!(
            explorer
                .decision()
                .is_some_and(|decision| !decision.returns_focus_to_terminal())
        );
    }

    fn row_ids(explorer: &LairExplorerUi) -> Vec<NavigationNodeId> {
        explorer.rows().iter().map(|row| row.id).collect()
    }

    /// The Explorer is hidden, a rail, or expanded. The rail lists one row
    /// per Lair under the chosen filter and no search; collapsing moves the
    /// selection to its Lair, and hiding keeps the rail for the next show.
    #[test]
    fn explorer_moves_between_hidden_rail_and_expanded() {
        let view = view();
        let lairs = view.lairs.iter().map(|lair| lair.id).collect::<Vec<_>>();
        let mut explorer = LairExplorerUi::default();
        explorer.set_filter(LairExplorerFilter::All);
        explorer.set_view(view);
        assert!(!explorer.visible() && !explorer.collapsed());
        // The palette toggle shows a hidden Explorer as its rail.
        assert!(explorer.toggle_rail());
        assert!(explorer.visible() && explorer.collapsed());
        assert!(explorer.reveal_current());
        assert_eq!(explorer.selected(), Some(lairs[0]));
        assert_eq!(row_ids(&explorer), lairs);
        // Then it expands to the tree, the current Splint revealed.
        assert!(!explorer.toggle_rail());
        assert!(explorer.visible() && !explorer.collapsed());
        assert_eq!(row_ids(&explorer).len(), 4);
        explorer.reveal_current();
        assert!(matches!(
            explorer.selected(),
            Some(NavigationNodeId::Splint { .. })
        ));
        // Collapsing keeps the selection on that Splint's Lair and stops a
        // search, whose query waits for the expanded tree.
        explorer.focus();
        explorer.set_search("draft".into());
        assert!(explorer.search_active());
        assert!(explorer.set_collapsed(true));
        assert!(!explorer.set_collapsed(true));
        assert!(!explorer.search_active() && !explorer.searching_all_lairs());
        assert_eq!(row_ids(&explorer), lairs);
        assert!(lairs.contains(&explorer.selected().unwrap()));
        assert!(!explorer.begin_search());
        // Escape returns to the terminal even with a query left behind.
        assert!(explorer.escape());
        assert!(!explorer.focused());
        assert!(explorer.set_collapsed(false));
        assert_eq!(explorer.query(), "draft");
        assert_eq!(row_ids(&explorer)[0], lairs[1]);
        // Hiding keeps the state for the next show.
        assert!(explorer.set_collapsed(true));
        assert!(!explorer.toggle_visibility());
        assert!(explorer.collapsed());
        assert!(explorer.toggle_visibility());
        assert!(explorer.collapsed());
    }

    /// A Lair row's ordinal is its place among every Lair of the view, so a
    /// Lair the rail numbers keeps its number when a filter or a search
    /// hides the Lairs before it. Dojo and Splint rows carry 0.
    #[test]
    fn lair_ordinals_ignore_the_filter_and_the_search() {
        let mut view = view();
        let mut beta = view.lairs[1].clone();
        let beta_id = NavigationNodeId::Lair(LairId::new());
        beta.id = beta_id;
        beta.label = "beta".into();
        beta.retention = LairRetention::Disposable;
        view.lairs.insert(1, beta);
        view.lairs[2].label = "__".into();
        let unnamed = view.lairs[2].id;
        let ordinal = |explorer: &LairExplorerUi, id| {
            explorer
                .rows()
                .into_iter()
                .find(|row| row.id == id)
                .map(|row| row.ordinal)
        };
        let mut explorer = LairExplorerUi::default();
        explorer.set_filter(LairExplorerFilter::All);
        explorer.set_view(view);
        explorer.toggle_visibility();
        assert_eq!(ordinal(&explorer, beta_id), Some(1));
        assert_eq!(ordinal(&explorer, unnamed), Some(2));
        // Saved hides the disposable `beta` before it; the number stays.
        explorer.set_filter(LairExplorerFilter::Saved);
        assert_eq!(ordinal(&explorer, beta_id), None);
        assert_eq!(ordinal(&explorer, unnamed), Some(2));
        explorer.toggle_rail();
        assert_eq!(ordinal(&explorer, unnamed), Some(2));
        explorer.toggle_rail();
        // A search that leaves it alone keeps it too, and the tree's other
        // rows carry 0.
        explorer.set_filter(LairExplorerFilter::All);
        explorer.set_search("draft".to_owned());
        assert_eq!(ordinal(&explorer, unnamed), Some(2));
        assert!(
            explorer
                .rows()
                .iter()
                .filter(|row| row.kind != LairExplorerRowKind::Lair)
                .all(|row| row.ordinal == 0)
        );
    }

    /// A rail row switches to its Lair: it opens the Lair's active Dojo,
    /// else one open here, else a live one, else the first that can open,
    /// and waits while that switch is pending. It has no disclosure.
    #[test]
    fn rail_rows_switch_to_their_lair_and_wait_while_pending() {
        use crate::navigation_projection::NavigationBlocker;

        let mut view = view();
        let lairs = view.lairs.iter().map(|lair| lair.id).collect::<Vec<_>>();
        let current = view.lairs[0].dojos[0].target;
        // The other Lair's first Dojo is not open here; its second is.
        let mut open_here = view.lairs[1].dojos[0].clone();
        open_here.target.dojo_id = DojoId::new();
        open_here.id = NavigationNodeId::Dojo {
            lair_id: open_here.target.lair_id,
            dojo_id: open_here.target.dojo_id,
        };
        open_here.attachment = WindowAttachment::Here;
        view.lairs[1].dojos.push(open_here.clone());
        let mut explorer = LairExplorerUi::default();
        explorer.set_filter(LairExplorerFilter::All);
        explorer.set_view(view.clone());
        explorer.toggle_rail();
        assert!(explorer.select(lairs[1]));
        let decision = explorer.decision().unwrap();
        assert_eq!(decision, LairExplorerDecision::OpenDojo(open_here.target));
        assert!(decision.returns_focus_to_terminal());
        assert!(!explorer.toggle_selected());
        assert!(!explorer.move_right());
        assert!(!explorer.move_left());
        assert_eq!(row_ids(&explorer), lairs);
        // While the switch is pending its row waits and opens nothing more.
        assert!(explorer.set_pending(decision));
        let row = explorer.rows().remove(1);
        assert!(row.pending && !row.enabled);
        assert_eq!(explorer.decision(), None);
        assert!(explorer.clear_pending());
        // The current Lair opens its active Dojo.
        assert!(explorer.select(lairs[0]));
        assert_eq!(
            explorer.decision(),
            Some(LairExplorerDecision::OpenDojo(current))
        );
        // With the Dojo open here unavailable, the first that can open.
        view.lairs[1].dojos[1].target.capability.availability =
            NavigationAvailability::Disabled(NavigationBlocker::Stale);
        explorer.set_view(view.clone());
        assert!(explorer.select(lairs[1]));
        assert_eq!(
            explorer.decision(),
            Some(LairExplorerDecision::OpenDojo(
                view.lairs[1].dojos[0].target
            ))
        );
        // A Lair whose Dojos wait on navigation stays lit and switches
        // nowhere yet.
        view.lairs[1].dojos[0].target.capability.availability =
            NavigationAvailability::Disabled(NavigationBlocker::Stale);
        explorer.set_view(view);
        assert!(explorer.rows()[1].enabled);
        assert_eq!(explorer.decision(), None);
        assert!(explorer.collapsed());
        // Expanded, the same row toggles its disclosure again.
        explorer.set_collapsed(false);
        assert!(matches!(
            explorer.decision(),
            Some(LairExplorerDecision::Toggle(_))
        ));
    }

    /// One rail click never asks to restore: a Lair whose Dojos only
    /// restore, behind a confirmation, is skipped by the switch, and its
    /// click expands the Explorer to the tree with that Lair selected and
    /// open instead, so its Dojo is chosen there. A query that would hide
    /// the Lair is cleared; one that shows it is kept. Rail rows have no
    /// disclosure, and a refresh, which disables every Dojo for a moment,
    /// neither dims them nor makes a click expand.
    #[test]
    fn rail_clicks_never_restore_and_stay_lit_through_a_refresh() {
        let mut view = view();
        let lairs = view.lairs.iter().map(|lair| lair.id).collect::<Vec<_>>();
        // The second Lair's only Dojo restores; the first keeps a direct
        // Dojo, so it still switches.
        for dojo in &mut view.lairs[1].dojos {
            dojo.target.capability.action = NavigationAction::PreviewRestoreDojo;
            dojo.target.capability.availability = NavigationAvailability::Enabled;
        }
        let mut explorer = LairExplorerUi::default();
        explorer.set_filter(LairExplorerFilter::All);
        explorer.set_view(view.clone());
        explorer.toggle_rail();
        assert!(explorer.rows().iter().all(|row| row.expanded.is_none()));
        explorer.select(lairs[0]);
        assert!(matches!(
            explorer.decision(),
            Some(LairExplorerDecision::OpenDojo(_))
        ));
        // Through a refresh every row stays lit, and a click waits.
        explorer.begin_refresh();
        assert!(explorer.rows().iter().all(|row| row.enabled));
        assert_eq!(explorer.decision(), None);
        assert!(explorer.collapsed());
        explorer.set_view(view.clone());
        // A query left in the tree that would hide the Lair is cleared.
        explorer.set_collapsed(false);
        explorer.set_search("shell".into());
        explorer.set_collapsed(true);
        assert!(explorer.select(lairs[1]));
        let decision = explorer.decision().unwrap();
        assert_eq!(decision, LairExplorerDecision::ExpandRail(lairs[1]));
        assert!(!decision.returns_focus_to_terminal());
        assert!(!explorer.set_pending(decision));
        assert!(!explorer.collapsed());
        assert_eq!(explorer.selected(), Some(lairs[1]));
        assert_eq!(explorer.query(), "");
        let rows = explorer.rows();
        let lair = rows.iter().position(|row| row.id == lairs[1]).unwrap();
        assert_eq!(rows[lair].expanded, Some(true));
        assert!(matches!(
            rows.get(lair + 1).map(|row| row.id),
            Some(NavigationNodeId::Dojo { .. })
        ));
        // A query that keeps the Lair in view is kept.
        let mut explorer = LairExplorerUi::default();
        explorer.set_filter(LairExplorerFilter::All);
        explorer.set_view(view);
        explorer.set_search("notes".into());
        explorer.toggle_rail();
        explorer.select(lairs[1]);
        assert_eq!(
            explorer.decision(),
            Some(LairExplorerDecision::ExpandRail(lairs[1]))
        );
        assert_eq!(explorer.query(), "notes");
        assert_eq!(explorer.selected(), Some(lairs[1]));
    }

    #[test]
    fn reveal_and_navigation_preserve_stable_hierarchy() {
        let mut explorer = LairExplorerUi::default();
        let view = view();
        let current = view.current;
        explorer.set_view(view);
        assert_eq!(
            explorer.breadcrumb().as_deref(),
            Some("work / editor / shell")
        );
        assert!(explorer.reveal_current());
        assert_eq!(explorer.rows().len(), 4);
        assert_eq!(explorer.selected(), current);
        assert!(explorer.move_left());
        assert!(matches!(
            explorer.selected(),
            Some(NavigationNodeId::Dojo { .. })
        ));
        assert!(explorer.move_left());
        let selected = explorer.selected();
        assert!(matches!(selected, Some(NavigationNodeId::Dojo { .. })));
        assert_eq!(
            explorer
                .rows()
                .iter()
                .find(|row| Some(row.id) == selected)
                .unwrap()
                .expanded,
            Some(false)
        );
        assert!(explorer.move_left());
        assert!(matches!(
            explorer.selected(),
            Some(NavigationNodeId::Lair(_))
        ));
        assert!(explorer.move_right());
        assert!(explorer.move_right());
        assert!(explorer.move_right());
        assert_eq!(explorer.selected(), current);
    }

    #[test]
    fn filtered_refresh_never_keeps_a_hidden_activation_target() {
        let mut explorer = LairExplorerUi::default();
        let mut view = view();
        let id = view.lairs[0].dojos[0].splints[0].id;
        explorer.set_view(view.clone());
        explorer.set_search("shell".into());
        explorer.select(id);
        view.lairs[0].dojos[0].splints[0].label = "renamed".into();
        explorer.set_view(view);
        assert!(explorer.rows().is_empty());
        assert_eq!(explorer.selected(), None);
        assert_eq!(explorer.activation_decision(id), None);
        assert_eq!(explorer.decision(), None);
        assert!(explorer.reveal_current());
        assert!(explorer.query().is_empty());
        assert_eq!(explorer.selected(), Some(id));
        assert!(explorer.decision().is_some());
    }

    #[test]
    fn right_navigation_uses_filtered_children_not_hidden_siblings() {
        let mut explorer = LairExplorerUi::default();
        let mut view = view();
        let mut child = view.lairs[0].dojos[0].splints[0].clone();
        child.target.splint_id = SplintId::new();
        child.id = NavigationNodeId::Splint {
            lair_id: child.target.lair_id,
            dojo_id: child.target.dojo_id,
            splint_id: child.target.splint_id,
        };
        child.label = "needle".into();
        child.focused_here = false;
        let id = child.id;
        view.lairs[0].dojos[0].splints.push(child);
        let dojo = view.lairs[0].dojos[0].id;
        explorer.set_view(view);
        explorer.set_search("needle".into());
        explorer.focus_tree();
        explorer.select(dojo);
        assert!(explorer.move_right());
        assert_eq!(explorer.selected(), Some(id));
        assert!(explorer.rows().iter().any(|row| row.id == id));
    }

    #[test]
    fn search_is_bounded_label_only_and_keeps_parent_context() {
        let mut explorer = LairExplorerUi::default();
        // Search looks through every Lair, so these matches in a stopped Lair
        // are found under the default Live filter.
        explorer.set_view(view());
        explorer.begin_search();
        assert!(explorer.append_search("draft"));
        let rows = explorer.rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label, "notes");
        assert_eq!(rows[1].label, "draft");
        assert!(!explorer.append_search("\n\u{202e}"));
    }

    #[test]
    fn failure_notice_is_bounded_sanitized_and_cleared_by_retry() {
        let mut explorer = LairExplorerUi::default();
        explorer.mark_disconnected();
        explorer.set_notice(&format!("failed\n\u{202e}{}", "界".repeat(300)));
        let message = explorer.status_message().unwrap();
        assert!(message.len() <= 256);
        assert!(!message.contains('\n'));
        assert!(!message.contains('\u{202e}'));
        assert!(explorer.retryable());
        explorer.begin_refresh();
        assert_eq!(explorer.notice, None);
    }

    #[test]
    fn empty_and_disconnected_states_are_explicit() {
        let mut explorer = LairExplorerUi::default();
        assert_eq!(explorer.status_message(), Some("Loading navigation…"));
        explorer.mark_disconnected();
        assert!(explorer.retryable());
        assert_eq!(
            explorer.status_message(),
            Some("Navigation unavailable · R retry")
        );
    }

    #[test]
    fn only_the_activated_splint_enters_pending_state() {
        let mut explorer = LairExplorerUi::default();
        let view = view();
        let selected = view.current.unwrap();
        explorer.set_view(view);
        explorer.reveal_current();
        let decision = explorer.decision().unwrap();
        assert!(explorer.set_pending(decision));
        assert!(!explorer.set_pending(decision));
        let rows = explorer.rows();
        let pending = rows.iter().find(|row| row.id == selected).unwrap();
        assert!(pending.pending);
        assert!(!pending.enabled);
        assert_eq!(pending.accessible_status, "Pending · Running");
        assert_eq!(pending.status, "pending");
        assert_eq!(rows.iter().filter(|row| row.pending).count(), 1);
        assert_eq!(explorer.decision(), None);
        let LairExplorerDecision::FocusSplint(target) = decision else {
            panic!("expected exact Splint decision");
        };
        let mut wrong = target;
        wrong.topology_revision = TopologyRevision::new(8);
        assert!(!explorer.finish_activation(LairExplorerActivationTarget::Splint(wrong), true));
        wrong = target;
        wrong.live_incarnation = Some(99);
        assert!(!explorer.finish_activation(LairExplorerActivationTarget::Splint(wrong), true));
        assert!(explorer.finish_activation(LairExplorerActivationTarget::Splint(target), true));
        assert!(explorer.decision().is_some());
    }

    #[test]
    fn unrelated_tab_failure_preserves_pending_activation_and_later_focus_ack() {
        for select_dojo in [false, true] {
            let mut explorer = LairExplorerUi::default();
            explorer.set_view(view());
            explorer.reveal_current();
            explorer.focus();
            if select_dojo {
                explorer.move_left();
            }
            let decision = explorer.decision().unwrap();
            let (completed, dojo_id) = match decision {
                LairExplorerDecision::OpenDojo(target) => (
                    LairExplorerActivationTarget::Dojo(SessionPickerTarget {
                        topology_revision: target.topology_revision,
                        lair_id: target.lair_id,
                        dojo_id: target.dojo_id,
                        action: target.capability.action,
                    }),
                    target.dojo_id,
                ),
                LairExplorerDecision::FocusSplint(target) => {
                    (LairExplorerActivationTarget::Splint(target), target.dojo_id)
                }
                LairExplorerDecision::Toggle(_) | LairExplorerDecision::ExpandRail(_) => {
                    panic!("expected activation")
                }
            };
            assert!(explorer.set_pending(decision));
            let pending = explorer.pending;
            // Even a failure naming the same Dojo is unrelated without an exact
            // Explorer activation target. Exercise the production failure helper.
            let failure = crate::frontend::WindowTopologyUpdate::TabFailed {
                dojo_id: Some(dojo_id),
                message: "queued unrelated topology failure".into(),
                explorer_target: None,
            };
            let crate::frontend::WindowTopologyUpdate::TabFailed {
                explorer_target, ..
            } = failure
            else {
                unreachable!();
            };
            assert!(!explorer.fail_activation(explorer_target));
            assert_eq!(explorer.pending, pending);
            assert!(explorer.activation_pending());
            assert!(explorer.focused());
            assert!(explorer.finish_activation(completed, true));
            explorer.return_to_terminal();
            assert!(!explorer.activation_pending());
            assert!(!explorer.focused());
        }
    }

    #[test]
    fn failed_activation_retires_only_exact_request_and_allows_retry_after_refresh() {
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(view());
        explorer.reveal_current();
        explorer.focus();
        let decision = explorer.decision().unwrap();
        let LairExplorerDecision::FocusSplint(target) = decision else {
            panic!("expected Splint target");
        };
        assert!(explorer.set_pending(decision));

        let mut wrong_lair = target;
        wrong_lair.lair_id = LairId::new();
        let mut wrong_dojo = target;
        wrong_dojo.dojo_id = DojoId::new();
        let mut wrong_splint = target;
        wrong_splint.splint_id = SplintId::new();
        let mut wrong_revision = target;
        wrong_revision.topology_revision = TopologyRevision::new(99);
        let mut wrong_incarnation = target;
        wrong_incarnation.live_incarnation = Some(99);
        for wrong in [
            wrong_lair,
            wrong_dojo,
            wrong_splint,
            wrong_revision,
            wrong_incarnation,
        ] {
            for succeeded in [false, true] {
                assert!(
                    !explorer
                        .finish_activation(LairExplorerActivationTarget::Splint(wrong), succeeded,)
                );
                assert_eq!(explorer.pending, Some(LairExplorerPending::Splint(target)));
                assert!(explorer.focused());
            }
        }

        // The owner could not focus the captured incarnation. Keep keyboard
        // ownership, but release the exact request and disable its stale row.
        assert!(explorer.fail_activation(Some(LairExplorerActivationTarget::Splint(target))));
        assert!(explorer.pending.is_none());
        assert!(explorer.focused());
        assert!(explorer.rows().iter().all(|row| !row.pending));
        assert!(explorer.decision().is_none());

        // A fresh projection retains the same stable ID under a new incarnation.
        let mut refreshed = explorer.view.clone().unwrap();
        let splint = &mut refreshed.lairs[0].dojos[0].splints[0];
        assert_eq!(splint.target.splint_id, target.splint_id);
        splint.target.live_incarnation = Some(99);
        splint.target.last_incarnation = Some(99);
        splint.target.capability.availability = NavigationAvailability::Enabled;
        explorer.set_view(refreshed);
        let retry = explorer.decision().unwrap();
        let LairExplorerDecision::FocusSplint(retry_target) = retry else {
            panic!("expected refreshed Splint target");
        };
        assert!(explorer.set_pending(retry));
        // A delayed old failure or acknowledgement cannot retire the new request.
        assert!(!explorer.fail_activation(Some(LairExplorerActivationTarget::Splint(target))));
        assert!(explorer.activation_pending());
        assert!(!explorer.finish_activation(LairExplorerActivationTarget::Splint(target), true));
        assert_eq!(
            explorer.pending,
            Some(LairExplorerPending::Splint(retry_target))
        );
        assert!(
            explorer.finish_activation(LairExplorerActivationTarget::Splint(retry_target), true)
        );
        explorer.return_to_terminal();
        assert!(!explorer.focused());
        assert!(explorer.pending.is_none());
        assert!(
            !explorer.finish_activation(LairExplorerActivationTarget::Splint(retry_target), true)
        );
    }

    #[test]
    fn stale_refresh_and_lifecycle_states_are_explicit() {
        let mut explorer = LairExplorerUi::default();
        let mut view = view();
        view.freshness = EndpointFreshness::Stale;
        view.lairs[0].dojos[0].splints[0].lifecycle = NavigationLifecycle::Restorable;
        explorer.set_view(view);
        assert!(explorer.retryable());
        assert_eq!(
            explorer.status_message(),
            Some("Navigation is stale · R retry")
        );
        explorer.begin_refresh();
        assert_eq!(explorer.status_message(), Some("Refreshing navigation…"));
        explorer.reveal_current();
        let splint = explorer
            .rows()
            .into_iter()
            .find(|row| row.kind == LairExplorerRowKind::Splint)
            .unwrap();
        assert!(splint.accessible_status.contains("Restorable"));
        assert!(splint.accessible_status.contains("Refreshing topology"));
        assert!(!splint.enabled);
    }

    #[test]
    fn dojo_decision_retains_exact_revision_and_identity() {
        let mut explorer = LairExplorerUi::default();
        let view = view();
        let target = view.lairs[0].dojos[0].target;
        explorer.set_view(view);
        explorer.reveal_current();
        explorer.move_left();
        let decision = LairExplorerDecision::OpenDojo(target);
        assert_eq!(explorer.decision(), Some(decision));
        assert!(explorer.set_pending(decision));
        let completed = SessionPickerTarget {
            topology_revision: target.topology_revision,
            lair_id: target.lair_id,
            dojo_id: target.dojo_id,
            action: target.capability.action,
        };
        let mut wrong = completed;
        wrong.lair_id = LairId::new();
        assert!(!explorer.finish_activation(LairExplorerActivationTarget::Dojo(wrong), true));
        wrong = completed;
        wrong.topology_revision = TopologyRevision::new(8);
        assert!(!explorer.finish_activation(LairExplorerActivationTarget::Dojo(wrong), true));
        assert!(explorer.finish_activation(LairExplorerActivationTarget::Dojo(completed), true));
    }

    #[test]
    fn parent_status_separates_runtime_attachment_and_retention() {
        let mut explorer = LairExplorerUi::default();
        let mut view = view();
        let lair = view.lairs[0].id;
        let dojo = view.lairs[0].dojos[0].id;
        explorer.set_view(view.clone());
        explorer.reveal_current();
        let rows = explorer.rows();
        assert_eq!(
            rows.iter()
                .find(|row| row.id == lair)
                .unwrap()
                .accessible_status,
            "Running · Saved"
        );
        assert_eq!(
            rows.iter()
                .find(|row| row.id == dojo)
                .unwrap()
                .accessible_status,
            "Running · Current"
        );
        view.lairs[0].dojos[0].active_here = false;
        view.lairs[0].dojos[0].attachment = WindowAttachment::NotHere;
        view.lairs[0].dojos[0].splints[0].lifecycle = NavigationLifecycle::Restorable;
        explorer.set_view(view);
        let rows = explorer.rows();
        assert_eq!(
            rows.iter()
                .find(|row| row.id == lair)
                .unwrap()
                .accessible_status,
            "Stopped · Restorable · Saved"
        );
        assert_eq!(
            rows.iter()
                .find(|row| row.id == dojo)
                .unwrap()
                .accessible_status,
            "Stopped · Restorable · Not open here"
        );
    }

    #[test]
    fn restorable_splint_routes_to_the_existing_preview_action() {
        let mut explorer = LairExplorerUi::default();
        let mut view = view();
        let splint = &mut view.lairs[0].dojos[0].splints[0];
        splint.lifecycle = NavigationLifecycle::Restorable;
        splint.target.live_incarnation = None;
        splint.target.capability.action = NavigationAction::PreviewRestoreSplint;
        let target = splint.target;
        explorer.set_view(view);
        explorer.reveal_current();
        assert_eq!(
            explorer.decision(),
            Some(LairExplorerDecision::FocusSplint(target))
        );
    }

    #[test]
    fn splint_selection_survives_reorder_and_falls_back_after_removal() {
        let mut explorer = LairExplorerUi::default();
        let mut view = view();
        let selected = view.lairs[0].dojos[0].splints[0].id;
        let mut sibling = view.lairs[0].dojos[0].splints[0].clone();
        let sibling_id = SplintId::new();
        sibling.id = NavigationNodeId::Splint {
            lair_id: sibling.target.lair_id,
            dojo_id: sibling.target.dojo_id,
            splint_id: sibling_id,
        };
        sibling.target.splint_id = sibling_id;
        sibling.label = "tests".into();
        sibling.focused_here = false;
        view.lairs[0].dojos[0].splints.push(sibling);
        explorer.set_view(view.clone());
        explorer.reveal_current();
        view.lairs[0].dojos[0].splints.swap(0, 1);
        explorer.set_view(view.clone());
        assert_eq!(explorer.selected(), Some(selected));
        let decision = explorer.decision().unwrap();
        assert!(explorer.set_pending(decision));

        view.lairs[0].dojos[0]
            .splints
            .retain(|splint| splint.id != selected);
        view.current = None;
        explorer.set_view(view);
        assert!(matches!(
            explorer.selected(),
            Some(NavigationNodeId::Dojo { .. })
        ));
        assert!(!explorer.clear_pending());
    }

    #[test]
    fn splint_decision_retains_parent_revision_and_incarnation() {
        let mut explorer = LairExplorerUi::default();
        let view = view();
        let target = view.lairs[0].dojos[0].splints[0].target;
        explorer.set_view(view);
        explorer.reveal_current();
        assert_eq!(
            explorer.decision(),
            Some(LairExplorerDecision::FocusSplint(target))
        );
    }

    fn agent(state: splinterm_protocol::AgentState, is_stale: bool) -> NavigationAgentStatus {
        NavigationAgentStatus {
            state,
            detail: Some("waiting for approval".to_owned()),
            stale: is_stale,
            stale_minutes: is_stale.then_some(2),
        }
    }

    /// The fixture view with a second Splint so Dojo aggregates have counts.
    fn agent_view(statuses: [Option<NavigationAgentStatus>; 2]) -> NavigationExplorerView {
        let mut view = view();
        let dojo = &mut view.lairs[0].dojos[0];
        let mut second = dojo.splints[0].clone();
        let splint_id = SplintId::new();
        second.id = NavigationNodeId::Splint {
            lair_id: second.target.lair_id,
            dojo_id: second.target.dojo_id,
            splint_id,
        };
        second.target.splint_id = splint_id;
        second.label = "agent".into();
        second.focused_here = false;
        dojo.splints.push(second);
        for (splint, status) in dojo.splints.iter_mut().zip(statuses) {
            splint.agent_status = status;
        }
        view
    }

    fn expanded_rows(explorer: &mut LairExplorerUi) -> Vec<LairExplorerRow> {
        explorer.reveal_current();
        explorer.rows()
    }

    /// The row's icon column with each icon written as `[name]`.
    fn column(row: &LairExplorerRow) -> Option<String> {
        row.agent_column.as_ref().map(ChromeSource::describe)
    }

    #[test]
    fn rows_show_glyph_column_state_text_and_priority_aggregates() {
        use splinterm_protocol::AgentState;
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(agent_view([
            Some(agent(AgentState::Working, false)),
            Some(agent(AgentState::NeedsInput, false)),
        ]));
        let rows = expanded_rows(&mut explorer);
        let labels = rows
            .iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>();
        assert_eq!(labels, ["work", "editor", "shell", "agent", "notes"]);
        let columns = rows.iter().map(column).collect::<Vec<_>>();
        let blocked = Some("[message-question]  ".to_owned());
        assert_eq!(
            columns,
            [
                blocked.clone(),
                blocked.clone(),
                Some("⠋   ".to_owned()),
                blocked,
                Some("    ".to_owned())
            ]
        );
        // Parent rows count states in words for accessibility, every state
        // spelled out.
        assert_eq!(
            rows[0].accessible_status,
            "Running · Saved · 1 needs input, 1 working"
        );
        assert_eq!(
            rows[1].accessible_status,
            "Running · Current · 1 needs input, 1 working"
        );
        assert_eq!(
            rows[3].accessible_status,
            "Running · Needs input · waiting for approval"
        );
        assert_eq!(rows[4].accessible_status, "Unavailable · Pinned");
        // The footer's short line leads with the agents, needs input first,
        // and drops what is ordinary: running, the current Dojo.
        let statuses = rows
            .iter()
            .map(|row| row.status.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            statuses,
            [
                "1 needs input · 1 working · saved",
                "1 needs input · 1 working",
                "working · waiting for approval",
                "needs input · waiting for approval",
                "unavailable · pinned",
            ]
        );

        // Without the icon face the column falls back to ASCII text.
        assert!(explorer.set_agent_glyphs(AgentGlyphs::Ascii));
        let rows = explorer.rows();
        assert_eq!(column(&rows[2]).as_deref(), Some("⠋ "));
        assert!(rows[2].agent_column.as_ref().unwrap().icons().is_empty());
        assert_eq!(column(&rows[4]).as_deref(), Some("  "));
    }

    /// A Lair row's rail glyph is its most urgent glyph alone, without the
    /// stale slot the rail has no room for, blank for a Lair where nothing
    /// reports; Dojo and Splint rows have none.
    #[test]
    fn lair_rows_carry_the_aggregate_glyph_for_the_rail() {
        use splinterm_protocol::AgentState;
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(agent_view([
            None,
            Some(agent(AgentState::NeedsInput, true)),
        ]));
        let glyphs = |rows: &[LairExplorerRow]| {
            rows.iter()
                .map(|row| row.rail_glyph.as_ref().map(ChromeSource::describe))
                .collect::<Vec<_>>()
        };
        let rows = expanded_rows(&mut explorer);
        assert_eq!(
            column(&rows[0]).as_deref(),
            Some("[message-question][clock]")
        );
        let asking = Some("[message-question]".to_owned());
        let blank = Some("  ".to_owned());
        assert_eq!(
            glyphs(&rows),
            [asking.clone(), None, None, None, blank.clone()]
        );
        explorer.set_collapsed(true);
        assert_eq!(glyphs(&explorer.rows()), [asking, blank]);
        assert!(explorer.set_agent_glyphs(AgentGlyphs::Ascii));
        assert_eq!(
            glyphs(&explorer.rows()),
            [Some("?".to_owned()), Some(" ".to_owned())]
        );
    }

    /// The footer's short line never repeats the row's name and leaves out
    /// what is ordinary: a running lifecycle, a disposable Lair, the current
    /// Dojo. The accessible text still spells all of it out.
    #[test]
    fn footer_status_says_only_what_the_row_does_not_show() {
        use splinterm_protocol::AgentState;
        let mut live = agent_view([
            Some(agent(AgentState::Done, false)),
            Some(agent(AgentState::Working, false)),
        ]);
        live.lairs[0].retention = LairRetention::Disposable;
        live.lairs[0].label = "Lair 7".into();
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(live);
        let rows = expanded_rows(&mut explorer);
        assert_eq!(
            rows[0].accessible_status,
            "Running · Disposable · 1 working, 1 done"
        );
        // Working outranks done, and states no Splint is in are left out.
        assert_eq!(rows[0].status, "1 working · 1 done");
        assert_eq!(rows[1].status, "1 working · 1 done");
        assert!(
            rows.iter()
                .all(|row| !row.status.contains(row.label.as_str())),
            "{rows:?}"
        );
        // Without agents a live disposable Lair and its current Dojo and
        // running Splint have nothing more to say.
        let mut plain = view();
        plain.lairs[0].retention = LairRetention::Disposable;
        explorer.set_view(plain);
        let rows = expanded_rows(&mut explorer);
        assert_eq!(
            rows.iter()
                .take(3)
                .map(|row| row.status.as_str())
                .collect::<Vec<_>>(),
            ["", "", ""]
        );
        assert_eq!(rows[0].accessible_status, "Running · Disposable");
        assert_eq!(rows[1].accessible_status, "Running · Current");
        assert_eq!(rows[2].accessible_status, "Running");

        // A stopped, restorable, or starting saved Lair keeps both words.
        explorer.set_filter(LairExplorerFilter::All);
        for (lifecycle, words) in [
            (NavigationLifecycle::Exited, "stopped · saved"),
            (NavigationLifecycle::Restorable, "restorable · saved"),
            (NavigationLifecycle::Starting, "starting · saved"),
        ] {
            let mut stopped = view();
            stopped.lairs[0].dojos[0].splints[0].lifecycle = lifecycle;
            explorer.set_view(stopped);
            let rows = explorer.rows();
            let work = rows.iter().find(|row| row.label == "work").unwrap();
            assert_eq!(work.status, words);
            assert!(work.accessible_status.contains("Saved"));
        }

        // A Dojo open here in a background tab says so; most Dojos in the
        // list are not open here and say nothing about it.
        let mut background = view();
        background.lairs[0].dojos[0].active_here = false;
        let notes = background.lairs[1].id;
        explorer.set_view(background);
        expanded_rows(&mut explorer);
        assert!(explorer.select(notes) && explorer.toggle_selected());
        let rows = explorer.rows();
        let editor = rows.iter().find(|row| row.label == "editor").unwrap();
        assert_eq!(editor.status, "open here");
        assert_eq!(editor.accessible_status, "Running · Open here");
        let draft = rows.iter().find(|row| row.label == "draft").unwrap();
        assert!(!draft.status.contains("open here"), "{}", draft.status);
        assert!(draft.accessible_status.contains("Not open here"));
    }

    #[test]
    fn stale_status_keeps_last_glyph_with_marker_and_age() {
        use splinterm_protocol::AgentState;
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(agent_view([Some(agent(AgentState::Working, true)), None]));
        let rows = expanded_rows(&mut explorer);
        assert_eq!(column(&rows[2]).as_deref(), Some("⠋ [clock]"));
        assert_eq!(
            rows[2].accessible_status,
            "Running · Working · stale (no report for 2 min) · waiting for approval"
        );
        assert_eq!(column(&rows[3]).as_deref(), Some("    "));
        assert_eq!(rows[3].accessible_status, "Running");
        assert_eq!(
            rows[1].accessible_status,
            "Running · Current · 1 working (stale)"
        );
        // The short line keeps the stale marker and its age.
        assert_eq!(
            rows[2].status,
            "working · stale 2 min · waiting for approval"
        );
        assert_eq!(rows[1].status, "1 working (stale)");
        // A plain running Splint has nothing more to say.
        assert_eq!(rows[3].status, "");
    }

    #[test]
    fn endpoint_freshness_wins_over_the_last_agent_status() {
        use splinterm_protocol::AgentState;
        let mut explorer = LairExplorerUi::default();
        let base = agent_view([Some(agent(AgentState::Error, false)), None]);
        explorer.set_view(base.clone());
        explorer.reveal_current();
        explorer.begin_refresh();
        assert_eq!(
            explorer.rows()[2].accessible_status,
            "Running · Error · waiting for approval · Refreshing topology"
        );
        assert_eq!(
            explorer.rows()[2].status,
            "error · waiting for approval · Refreshing topology"
        );
        explorer.mark_disconnected();
        let rows = explorer.rows();
        assert_eq!(
            rows[2].accessible_status,
            "Running · Agent status unavailable · Endpoint is disconnected"
        );
        assert_eq!(
            rows[2].status,
            "agent status unavailable · Endpoint is disconnected"
        );
        assert_eq!(column(&rows[2]).as_deref(), Some("    "));
        assert_eq!(column(&rows[1]).as_deref(), Some("    "));
        assert!(
            rows[1]
                .accessible_status
                .contains("Agent status unavailable")
        );
        assert!(
            rows.iter()
                .all(|row| !row.accessible_status.contains("Error"))
        );

        let mut stale = base;
        stale.freshness = EndpointFreshness::Stale;
        explorer.set_view(stale);
        assert!(
            explorer.rows()[2]
                .accessible_status
                .contains("Agent status unavailable")
        );
    }

    #[test]
    fn search_matches_state_words_only_while_status_is_available() {
        use splinterm_protocol::AgentState;
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(agent_view([
            Some(agent(AgentState::Working, false)),
            Some(agent(AgentState::NeedsInput, false)),
        ]));
        explorer.begin_search();
        assert!(explorer.append_search("needs input"));
        let labels = explorer
            .rows()
            .iter()
            .map(|row| row.label.clone())
            .collect::<Vec<_>>();
        assert_eq!(labels, ["work", "editor", "agent"]);
        explorer.set_search("needs_input".into());
        assert_eq!(explorer.rows().last().unwrap().label, "agent");
        explorer.set_search("working".into());
        assert_eq!(explorer.rows().last().unwrap().label, "shell");
        // Free-text detail is not a search term.
        explorer.set_search("approval".into());
        assert!(explorer.rows().is_empty());
        explorer.set_search("needs input".into());
        explorer.mark_disconnected();
        assert!(explorer.rows().is_empty());
    }

    #[test]
    fn disabled_presentation_hides_every_agent_status_trace() {
        use splinterm_protocol::AgentState;
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(agent_view([
            Some(agent(AgentState::NeedsInput, true)),
            Some(agent(AgentState::Done, false)),
        ]));
        assert!(explorer.set_agent_status_enabled(false));
        assert!(!explorer.set_agent_status_enabled(false));
        let rows = expanded_rows(&mut explorer);
        assert!(rows.iter().all(|row| row.agent_column.is_none()));
        assert_eq!(rows[2].accessible_status, "Running");
        assert_eq!(rows[1].accessible_status, "Running · Current");
        assert!(rows[1..4].iter().all(|row| row.status.is_empty()));
        explorer.begin_search();
        explorer.set_search("done".into());
        assert!(explorer.rows().is_empty());
    }

    #[test]
    fn glyph_column_appears_only_while_any_status_exists_and_clears_apply_by_incarnation() {
        use splinterm_protocol::AgentState;
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(view());
        assert!(
            expanded_rows(&mut explorer)
                .iter()
                .all(|row| row.agent_column.is_none())
        );
        explorer.set_view(agent_view([Some(agent(AgentState::Done, false)), None]));
        let splint_id = explorer.view().unwrap().lairs[0].dojos[0].splints[0]
            .target
            .splint_id;
        assert!(!explorer.apply_agent_status(splint_id, 4, None));
        assert!(expanded_rows(&mut explorer)[2].agent_column.is_some());
        assert!(explorer.apply_agent_status(splint_id, 3, None));
        assert!(explorer.rows().iter().all(|row| row.agent_column.is_none()));
    }

    #[test]
    fn access_marks_say_in_words_who_can_reach_or_is_connected_and_who_waits() {
        use crate::frontend::agent_access::{
            AccessSnapshot,
            tests::{entry, remembered_key},
        };
        use splinterm_protocol::{AccessEntryKey, AccessEntryKind, RememberedStoreStatus};
        let view = view();
        let NavigationNodeId::Lair(work) = view.lairs[0].id else {
            unreachable!()
        };
        let NavigationNodeId::Lair(notes) = view.lairs[1].id else {
            unreachable!()
        };
        let NavigationNodeId::Splint { splint_id, .. } = view.lairs[0].dojos[0].splints[0].id
        else {
            unreachable!()
        };
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(view);
        let before = expanded_rows(&mut explorer);
        assert!(
            before
                .iter()
                .all(|row| !row.accessible_status.contains("Agent"))
        );

        // A Splint once grant, connected; a while-open grant for "notes",
        // not connected; and a request waiting about "notes".
        let mut once = entry(
            AccessEntryKey::Grant { grant_id: 17 },
            AccessEntryKind::Once,
            work,
        );
        once.splint_id = Some(splint_id);
        once.incarnation = Some(3);
        once.connections = 1;
        let session = entry(
            AccessEntryKey::Grant { grant_id: 18 },
            AccessEntryKind::WorkspaceSession,
            notes,
        );
        let pending = entry(
            AccessEntryKey::Pending { request_id: 4 },
            AccessEntryKind::Pending,
            notes,
        );
        let mut snapshot = AccessSnapshot::new(9, RememberedStoreStatus::Available);
        snapshot.entries = vec![once, session, pending];
        assert!(explorer.set_access_marks(AccessMarks::from_snapshot(&snapshot)));
        assert!(!explorer.set_access_marks(AccessMarks::from_snapshot(&snapshot)));
        let rows = expanded_rows(&mut explorer);
        let row = |label: &str| rows.iter().find(|row| row.label == label).unwrap();
        let status = |label: &str| row(label).accessible_status.clone();
        let brief = |label: &str| row(label).status.as_str();
        let access = |label: &str| row(label).access;
        assert_eq!(access("work"), Some(LairExplorerAccess::Connected));
        assert_eq!(access("editor"), Some(LairExplorerAccess::Connected));
        assert_eq!(access("shell"), Some(LairExplorerAccess::Connected));
        assert_eq!(access("notes"), Some(LairExplorerAccess::Asking));
        assert_eq!(status("work"), "Agent connected to part · Running · Saved");
        assert_eq!(
            status("editor"),
            "Agent connected to part · Running · Current"
        );
        assert_eq!(status("shell"), "Agent connected · Running");
        assert!(
            status("notes").starts_with("Agent asking for access · Agent can reach · "),
            "{}",
            status("notes")
        );
        // The short line keeps every mark in words: a waiting request first,
        // reach last, after the row's other state.
        assert_eq!(brief("work"), "saved · agent connected to part");
        assert_eq!(brief("editor"), "agent connected to part");
        assert_eq!(brief("shell"), "agent connected");
        assert_eq!(
            brief("notes"),
            "asking for access · unavailable · pinned · agent can reach"
        );

        // A remembered grant for the whole workspace marks it, not its rows.
        let mut remembered = entry(remembered_key(), AccessEntryKind::Remembered, work);
        remembered.connections = 0;
        snapshot.entries = vec![remembered];
        assert!(explorer.set_access_marks(AccessMarks::from_snapshot(&snapshot)));
        let rows = expanded_rows(&mut explorer);
        assert!(rows[0].accessible_status.starts_with("Agent can reach · "));
        assert_eq!(rows[0].access, Some(LairExplorerAccess::Reachable));
        assert!(
            rows[1..]
                .iter()
                .all(|row| !row.accessible_status.contains("Agent"))
        );
        assert!(rows[1..].iter().all(|row| row.access.is_none()));

        // The marks end with the access.
        assert!(explorer.set_access_marks(AccessMarks::default()));
        assert!(
            expanded_rows(&mut explorer)
                .iter()
                .all(|row| !row.accessible_status.contains("Agent"))
        );
    }

    #[test]
    fn rows_carry_live_retention_and_access_icon_marks() {
        let mut view = view();
        view.lairs.push({
            let mut scratch = view.lairs[1].clone();
            scratch.id = NavigationNodeId::Lair(LairId::new());
            scratch.label = "scratch".into();
            scratch.retention = LairRetention::Disposable;
            scratch
        });
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(view);
        explorer.set_filter(LairExplorerFilter::All);
        let rows = expanded_rows(&mut explorer);
        let row = |label: &str| rows.iter().find(|row| row.label == label).unwrap();
        // The running Lair, its Dojo, and its Splint are live; the stopped
        // Lairs and their Dojos are not.
        assert!(row("work").live && row("editor").live && row("shell").live);
        assert!(!row("notes").live && !row("scratch").live);
        // Only a saved or pinned Lair has a retention mark.
        assert_eq!(row("work").retention, Some(LairRetention::Saved));
        assert_eq!(row("notes").retention, Some(LairRetention::Pinned));
        assert_eq!(row("scratch").retention, None);
        assert!(
            rows.iter()
                .filter(|row| row.kind != LairExplorerRowKind::Lair)
                .all(|row| row.retention.is_none())
        );
        // The marks are visual only; the words say the same.
        assert!(row("work").accessible_status.contains("Saved"));
        assert!(row("notes").accessible_status.contains("Pinned"));
        assert!(row("scratch").accessible_status.contains("Disposable"));
        // The short line keeps only the retention that is not the default:
        // every new Lair starts disposable.
        assert!(row("work").status.contains("saved"));
        assert!(row("notes").status.contains("pinned"));
        assert!(!row("scratch").status.contains("disposable"));
        assert!(rows.iter().all(|row| row.access.is_none()));
        assert_eq!(
            LairExplorerAccess::of(AccessReach::Reachable, AccessReach::Connected, 0),
            Some(LairExplorerAccess::Connected)
        );
        assert_eq!(
            LairExplorerAccess::of(AccessReach::Connected, AccessReach::None, 1),
            Some(LairExplorerAccess::Asking)
        );
        assert_eq!(
            LairExplorerAccess::of(AccessReach::None, AccessReach::None, 0),
            None
        );
    }

    #[test]
    fn working_rows_show_the_spinner_frame_and_only_fresh_work_moves() {
        use splinterm_protocol::AgentState;
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(agent_view([
            Some(agent(AgentState::Working, false)),
            Some(agent(AgentState::Done, false)),
        ]));
        let moving =
            |explorer: &LairExplorerUi| explorer.rows().iter().any(|row| row.agent_working);
        let rows = expanded_rows(&mut explorer);
        // The working Splint and its parents, whose most urgent state it is,
        // show the spinner.
        assert!(moving(&explorer));
        let working = rows
            .iter()
            .map(|row| (row.label.as_str(), row.agent_working))
            .collect::<Vec<_>>();
        assert_eq!(
            working,
            [
                ("work", true),
                ("editor", true),
                ("shell", true),
                ("agent", false),
                ("notes", false)
            ]
        );
        assert_eq!(column(&rows[2]).as_deref(), Some("⠋   "));
        assert!(explorer.set_agent_spinner(AgentSpinner::STATIC.next()));
        assert!(!explorer.set_agent_spinner(AgentSpinner::STATIC.next()));
        let rows = explorer.rows();
        for index in [0, 1, 2] {
            assert_eq!(column(&rows[index]).as_deref(), Some("⠙   "));
        }
        // The words never move: status and accessible text stay the same.
        assert_eq!(
            rows[2].accessible_status,
            "Running · Working · waiting for approval"
        );
        assert_eq!(rows[2].status, "working · waiting for approval");

        // A stale working status and one outranked by needs input rest.
        explorer.set_view(agent_view([
            Some(agent(AgentState::Working, true)),
            Some(agent(AgentState::NeedsInput, false)),
        ]));
        let rows = expanded_rows(&mut explorer);
        assert!(rows.iter().all(|row| !row.agent_working));
        assert_eq!(column(&rows[2]).as_deref(), Some("⠋ [clock]"));
        // While status is unavailable nothing moves either.
        explorer.set_view(agent_view([Some(agent(AgentState::Working, false)), None]));
        expanded_rows(&mut explorer);
        assert!(moving(&explorer));
        explorer.mark_disconnected();
        assert!(!moving(&explorer));
    }

    /// [`agent_view`] with a third Splint, in the other Lair, reporting
    /// `third`.
    fn agents_view(
        statuses: [Option<NavigationAgentStatus>; 2],
        third: Option<NavigationAgentStatus>,
    ) -> NavigationExplorerView {
        let mut view = agent_view(statuses);
        let mut splint = view.lairs[0].dojos[0].splints[0].clone();
        let dojo = &mut view.lairs[1].dojos[0];
        let splint_id = SplintId::new();
        splint.id = NavigationNodeId::Splint {
            lair_id: dojo.target.lair_id,
            dojo_id: dojo.target.dojo_id,
            splint_id,
        };
        splint.target.lair_id = dojo.target.lair_id;
        splint.target.dojo_id = dojo.target.dojo_id;
        splint.target.splint_id = splint_id;
        splint.label = "scribe".into();
        splint.focused_here = false;
        splint.agent_status = third;
        dojo.splints.push(splint);
        view
    }

    fn agent_labels(explorer: &LairExplorerUi) -> Vec<String> {
        explorer
            .agent_rows()
            .iter()
            .map(|agent| agent.row.label.clone())
            .collect()
    }

    /// The agents section lists every Splint that reports, most urgent
    /// first and in tree order within a state, with its Lair and Dojo, its
    /// glyph column, and the location leading its accessible text. A stale
    /// status follows every fresh one, and a Splint that reports nothing is
    /// not listed.
    #[test]
    fn agents_section_lists_reporting_splints_most_urgent_first() {
        use splinterm_protocol::AgentState;
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(agents_view(
            [
                Some(agent(AgentState::Working, false)),
                Some(agent(AgentState::Error, false)),
            ],
            Some(agent(AgentState::NeedsInput, false)),
        ));
        assert_eq!(agent_labels(&explorer), ["scribe", "agent", "shell"]);
        let agents = explorer.agent_rows();
        assert_eq!(agents[0].location, "notes / draft");
        assert_eq!(agents[2].location, "work / editor");
        for agent in &agents {
            assert_eq!(agent.row.kind, LairExplorerRowKind::Splint);
            assert_eq!(agent.row.level, 1);
            assert!(agent.row.agent_column.is_some());
            assert!(
                agent
                    .row
                    .accessible_status
                    .starts_with(&format!("{} · ", agent.location)),
                "{}",
                agent.row.accessible_status
            );
        }
        assert!(agents[0].row.accessible_status.contains("Needs input"));
        // Stale after fresh, and ties keep the tree's order.
        explorer.set_view(agents_view(
            [
                Some(agent(AgentState::NeedsInput, true)),
                Some(agent(AgentState::Idle, false)),
            ],
            Some(agent(AgentState::Idle, false)),
        ));
        assert_eq!(agent_labels(&explorer), ["agent", "scribe", "shell"]);
        explorer.set_view(agents_view(
            [Some(agent(AgentState::Done, false)), None],
            Some(agent(AgentState::Done, false)),
        ));
        assert_eq!(agent_labels(&explorer), ["shell", "scribe"]);
    }

    /// The section ignores the filter and the search, and is empty on the
    /// rail, with agent status turned off, while navigation is stale or
    /// disconnected, and when nothing reports.
    #[test]
    fn agents_section_hides_on_the_rail_without_status_or_reports() {
        use splinterm_protocol::AgentState;
        let reporting = || {
            agents_view(
                [Some(agent(AgentState::Working, false)), None],
                Some(agent(AgentState::NeedsInput, false)),
            )
        };
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(reporting());
        assert_eq!(agent_labels(&explorer), ["scribe", "shell"]);
        for filter in [LairExplorerFilter::Saved, LairExplorerFilter::Live] {
            explorer.set_filter(filter);
            assert_eq!(agent_labels(&explorer), ["scribe", "shell"], "{filter:?}");
        }
        explorer.set_search("no-such-row".into());
        assert!(explorer.rows().is_empty());
        assert_eq!(agent_labels(&explorer), ["scribe", "shell"]);
        explorer.set_search(String::new());
        explorer.set_collapsed(true);
        assert!(explorer.agent_rows().is_empty());
        explorer.set_collapsed(false);
        assert_eq!(explorer.agent_rows().len(), 2);
        explorer.set_agent_status_enabled(false);
        assert!(explorer.agent_rows().is_empty());
        explorer.set_agent_status_enabled(true);
        explorer.begin_refresh();
        assert_eq!(explorer.agent_rows().len(), 2, "a refresh keeps the list");
        explorer.mark_disconnected();
        assert!(explorer.agent_rows().is_empty());
        let mut stale = reporting();
        stale.freshness = EndpointFreshness::Stale;
        explorer.set_view(stale);
        assert!(explorer.agent_rows().is_empty());
        explorer.set_view(agents_view([None, None], None));
        assert!(explorer.agent_rows().is_empty());
    }

    /// Down from the tree's last row enters the agents section and Up from
    /// its first row returns; Home and End reach the tree's first row and
    /// the section's last. An agents row is selected apart from the same
    /// Splint's tree row, has no parent to move Left to, activates its
    /// Splint as the tree row does, and, when its status clears, the
    /// selection goes to that Splint in the tree.
    #[test]
    fn keys_cross_from_the_tree_into_the_agents_section_and_back() {
        use splinterm_protocol::AgentState;
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(agents_view(
            [Some(agent(AgentState::Working, false)), None],
            Some(agent(AgentState::NeedsInput, false)),
        ));
        let rows = expanded_rows(&mut explorer);
        let shell = rows.iter().find(|row| row.label == "shell").unwrap().id;
        let agents: Vec<_> = explorer
            .agent_rows()
            .iter()
            .map(|agent| agent.row.id)
            .collect();
        explorer.select_edge(false);
        assert_eq!(explorer.tree_selected(), Some(rows[0].id));
        assert!(explorer.move_selection(isize::try_from(rows.len() - 1).unwrap()));
        assert_eq!(explorer.tree_selected(), rows.last().map(|row| row.id));
        assert!(explorer.move_selection(1));
        assert_eq!(explorer.tree_selected(), None);
        assert_eq!(explorer.agent_selected(), Some(agents[0]));
        assert_eq!(explorer.selected(), Some(agents[0]));
        assert!(explorer.move_selection(1));
        assert_eq!(explorer.agent_selected(), Some(shell));
        assert!(!explorer.move_selection(1), "the section's last row stops");
        assert!(!explorer.move_left());
        assert!(!explorer.move_right());
        assert!(explorer.move_selection(-2));
        assert_eq!(explorer.tree_selected(), rows.last().map(|row| row.id));
        assert!(explorer.select_edge(true));
        assert_eq!(explorer.agent_selected(), Some(shell));
        // The tree's own row for the same Splint is a different selection.
        assert!(explorer.select(shell));
        assert_eq!(explorer.tree_selected(), Some(shell));
        assert!(explorer.select_agent(shell));
        assert_eq!(explorer.agent_selected(), Some(shell));
        let Some(LairExplorerDecision::FocusSplint(target)) = explorer.decision() else {
            panic!("an agents row focuses its Splint");
        };
        assert_eq!(target.splint_id, shell_splint(shell));
        // A status that clears takes the row away; the row now in its place
        // takes the selection.
        assert!(explorer.apply_agent_status(shell_splint(shell), 3, None));
        assert_eq!(explorer.agent_selected(), Some(agents[0]));
        // Collapsing to the rail leaves the section.
        explorer.set_collapsed(true);
        assert_eq!(explorer.agent_selected(), None);
        assert!(explorer.tree_selected().is_some());
    }

    /// A selected agents row that leaves the section passes the selection
    /// to the row now at its place, clamped to the section's end. With no
    /// row left there, as while the section is hidden, the Splint's tree row
    /// takes it, else its Lair's. A hidden section's rows are skipped by the
    /// keys, cannot be selected, and have no menu.
    #[test]
    fn agents_selection_falls_back_to_the_next_row_then_the_tree() {
        use splinterm_protocol::AgentState;
        let mut explorer = LairExplorerUi::default();
        let view = agents_view(
            [
                Some(agent(AgentState::Working, false)),
                Some(agent(AgentState::Done, false)),
            ],
            Some(agent(AgentState::NeedsInput, false)),
        );
        explorer.focus();
        explorer.set_view(view.clone());
        let rows = expanded_rows(&mut explorer);
        let id = |label: &str| {
            explorer
                .agent_rows()
                .iter()
                .find(|agent| agent.row.label == label)
                .unwrap()
                .row
                .id
        };
        let (scribe, shell, second) = (id("scribe"), id("shell"), id("agent"));
        assert_eq!(agent_labels(&explorer), ["scribe", "shell", "agent"]);
        // The next row moves up into the place left.
        assert!(explorer.select_agent(shell));
        assert!(explorer.apply_agent_status(shell_splint(shell), 3, None));
        assert_eq!(explorer.agent_selected(), Some(second));
        // A Splint that goes from the view entirely: the place is clamped
        // to the section's last row.
        let mut gone = view.clone();
        gone.lairs[0].dojos[0]
            .splints
            .retain(|splint| splint.id != second);
        gone.lairs[0].dojos[0].splints[0].agent_status = None;
        explorer.set_view(gone);
        assert_eq!(agent_labels(&explorer), ["scribe"]);
        assert_eq!(explorer.agent_selected(), Some(scribe));
        // Hiding the section: the Splint's tree row takes the selection.
        explorer.set_view(view);
        assert!(explorer.select_agent(shell));
        assert!(explorer.set_agents_shown(false));
        assert!(!explorer.set_agents_shown(false));
        assert_eq!(explorer.agent_selected(), None);
        assert_eq!(explorer.tree_selected(), Some(shell));
        // While hidden its rows are out of reach.
        assert!(!explorer.select_agent(scribe));
        assert!(!explorer.can_context(scribe));
        assert!(explorer.shown_agent_rows().is_empty());
        assert_eq!(explorer.agent_rows().len(), 3, "the layout still asks");
        assert!(explorer.select_edge(true));
        assert_eq!(explorer.tree_selected(), rows.last().map(|row| row.id));
        assert!(!explorer.move_selection(1));
        // A Splint the tree does not show passes it to its Lair's row.
        assert!(explorer.set_agents_shown(true));
        assert!(explorer.select_agent(scribe));
        assert!(explorer.set_agents_shown(false));
        assert_eq!(explorer.tree_selected(), Some(lair_node(scribe)));
    }

    /// A menu's Focus on a row not selected moves the selection, and with it
    /// the footer's status, to that row: to its agents row while the
    /// selection is in the agents section or the tree does not show the
    /// Splint, else to its tree row.
    #[test]
    fn menu_activation_moves_the_selection_to_the_row_acted_on() {
        use splinterm_protocol::AgentState;
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(agents_view(
            [
                Some(agent(AgentState::Working, false)),
                Some(agent(AgentState::Idle, false)),
            ],
            Some(agent(AgentState::NeedsInput, false)),
        ));
        let rows = expanded_rows(&mut explorer);
        let id = |label: &str| {
            explorer
                .agent_rows()
                .iter()
                .find(|agent| agent.row.label == label)
                .unwrap()
                .row
                .id
        };
        let (scribe, shell, second) = (id("scribe"), id("shell"), id("agent"));
        // As the menu does: decide, start the activation, then select.
        let activate = |explorer: &mut LairExplorerUi, id| {
            let decision = explorer.activation_decision(id).unwrap();
            assert!(explorer.set_pending(decision));
            let changed = explorer.select_activated(id);
            assert!(explorer.clear_pending());
            changed
        };
        // From one agents row to another, though the tree shows both.
        assert!(explorer.select_agent(shell));
        assert!(rows.iter().any(|row| row.id == second));
        assert!(activate(&mut explorer, second));
        assert_eq!(explorer.agent_selected(), Some(second));
        assert!(!activate(&mut explorer, second), "already selected");
        assert_eq!(explorer.agent_selected(), Some(second));
        // From the tree: a Splint the tree shows keeps the selection there.
        assert!(explorer.select(rows[0].id));
        assert!(activate(&mut explorer, shell));
        assert_eq!(explorer.tree_selected(), Some(shell));
        // One the tree does not show is selected in the agents section.
        assert!(activate(&mut explorer, scribe));
        assert_eq!(explorer.agent_selected(), Some(scribe));
    }

    /// With no position in the list, as when a search leaves the tree
    /// empty, Down or Up selects the first agents row rather than skipping
    /// it.
    #[test]
    fn a_step_with_no_position_lands_on_the_first_agents_row() {
        use splinterm_protocol::AgentState;
        for delta in [1, -1] {
            let mut explorer = LairExplorerUi::default();
            explorer.set_view(agents_view(
                [Some(agent(AgentState::Working, false)), None],
                Some(agent(AgentState::NeedsInput, false)),
            ));
            explorer.set_search("no-such-row".into());
            assert!(explorer.rows().is_empty());
            assert_eq!(explorer.selected(), None);
            let first = explorer.agent_rows()[0].row.id;
            assert!(explorer.move_selection(delta));
            assert_eq!(explorer.agent_selected(), Some(first), "{delta}");
        }
    }

    /// An agents row starts its Splint's activation though the tree does
    /// not show that Splint, its Lair being closed: Enter or a click, and
    /// the Splint's menu's Focus item, all reach pending, which the agents
    /// row shows.
    #[test]
    fn agents_row_activation_reaches_pending_under_a_closed_lair() {
        use splinterm_protocol::AgentState;
        for from_menu in [false, true] {
            let mut explorer = LairExplorerUi::default();
            explorer.set_view(agents_view(
                [None, None],
                Some(agent(AgentState::NeedsInput, false)),
            ));
            explorer.focus();
            let scribe = explorer.agent_rows()[0].row.id;
            assert!(
                !explorer.rows().iter().any(|row| row.id == scribe),
                "its Lair is closed"
            );
            let decision = if from_menu {
                assert!(explorer.can_context(scribe));
                explorer.activation_decision(scribe)
            } else {
                assert!(explorer.select_agent(scribe));
                explorer.decision()
            };
            let Some(decision @ LairExplorerDecision::FocusSplint(target)) = decision else {
                panic!("an agents row focuses its Splint");
            };
            assert_eq!(target.splint_id, shell_splint(scribe));
            assert!(explorer.set_pending(decision), "menu {from_menu}");
            let row = explorer.agent_rows()[0].row.clone();
            assert!(row.pending);
            assert!(!row.enabled);
            assert!(row.accessible_status.contains("Pending"));
        }
    }

    fn shell_splint(id: NavigationNodeId) -> SplintId {
        match id {
            NavigationNodeId::Splint { splint_id, .. } => splint_id,
            _ => panic!("{id:?} is not a Splint"),
        }
    }
}
