//! Owner-local bridge from the real Explorer projection to bounded semantics.

use std::collections::HashMap;

use splinterm_core::SplintId;
use splinterm_protocol::AgentState;

use super::{LairExplorerRow, LairExplorerUi, agent_status::state_text};
use crate::accessibility::{
    MAX_ACCESSIBILITY_ITEMS, MAX_ACCESSIBLE_NAME_CHARS, SEARCH_NODE_ID, SemanticAction,
    SemanticActionRequest, SemanticAvailability, SemanticFocus, SemanticNavigationSnapshot,
    SemanticNodeId, SemanticNodeRegistry, SemanticTreeItem, TERMINAL_NODE_ID, TREE_NODE_ID,
    is_bidi_formatting, valid_query_text,
};
use crate::navigation_projection::{EndpointFreshness, NavigationExplorerView, NavigationNodeId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent owner authority axes, not one UI mode"
)]
pub(crate) struct NavigationAccessContext {
    pub keyboard_focused: bool,
    pub modal: bool,
    pub permitted: bool,
    pub pending: bool,
}

impl NavigationAccessContext {
    fn enabled(self) -> bool {
        self.keyboard_focused && !self.modal && self.permitted
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct NavigationIdentity {
    endpoint: String,
    endpoint_generation: u64,
    node: NavigationNodeId,
    live_incarnation: Option<u64>,
    last_incarnation: Option<u64>,
}

/// What one accessible item stands for, so the registry gives a Splint's
/// tree row and its agents row distinct, stable ids.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum NavigationKey {
    /// A row of the tree.
    Tree(NavigationIdentity),
    /// The agents section's group item, for one endpoint generation.
    AgentsGroup {
        endpoint: String,
        endpoint_generation: u64,
    },
    /// A Splint's row in the agents section.
    Agent(NavigationIdentity),
}

/// The row an accessible item acts on, and which list it is in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NavigationTarget {
    Tree(NavigationNodeId),
    Agent(NavigationNodeId),
}

/// The accessible name of the agents section's group item.
const AGENTS_GROUP_NAME: &str = "Agents";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum NavigationAccessAction {
    Terminal,
    Tree,
    Search(String),
    SearchFocus,
    Item(NavigationNodeId),
    Expanded(NavigationNodeId, bool),
    Activate(NavigationNodeId),
    /// Selects a Splint's row in the agents section.
    AgentItem(NavigationNodeId),
    /// Activates a Splint from its row in the agents section.
    ActivateAgent(NavigationNodeId),
}

#[derive(Default)]
pub(crate) struct NavigationAccessibility {
    registry: SemanticNodeRegistry<NavigationKey>,
    targets: HashMap<SemanticNodeId, NavigationTarget>,
    view: Option<NavigationExplorerView>,
    snapshot: Option<SemanticNavigationSnapshot>,
    generation: u64,
    /// Last observed agent state per live incarnation; `None` until the first
    /// available view so existing statuses are not announced on load.
    agent_states: Option<HashMap<AgentStatusKey, (AgentState, bool)>>,
    /// A pending polite announcement and the navigation status it prefixes.
    agent_announcement: Option<(String, String)>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct AgentStatusKey {
    endpoint: String,
    endpoint_generation: u64,
    splint_id: SplintId,
    incarnation: u64,
}

impl NavigationAccessibility {
    pub(crate) fn refresh(
        &mut self,
        explorer: &LairExplorerUi,
        context: NavigationAccessContext,
    ) -> SemanticNavigationSnapshot {
        let identities = identities(explorer.view());
        if let Some(announcement) = self.observe_agent_states(explorer) {
            self.agent_announcement = Some((announcement, String::new()));
        }
        let group = explorer.view().map(|view| NavigationKey::AgentsGroup {
            endpoint: view.endpoint_namespace.clone(),
            endpoint_generation: view.endpoint_generation,
        });
        self.registry.retain(|key| match key {
            NavigationKey::Tree(identity) | NavigationKey::Agent(identity) => {
                identities.get(&identity.node) == Some(identity)
            }
            NavigationKey::AgentsGroup { .. } => group.as_ref() == Some(key),
        });
        self.targets.clear();
        let visible = explorer.visible() && !context.modal;
        if !visible {
            // Announcements are spoken only from a visible Explorer status node.
            self.agent_announcement = None;
        }
        let enabled = visible && context.enabled();
        let availability = |row: &LairExplorerRow| {
            if row.pending {
                SemanticAvailability::Pending
            } else if enabled && row.enabled && !context.pending && !explorer.activation_pending() {
                SemanticAvailability::Enabled
            } else {
                SemanticAvailability::Disabled
            }
        };
        let mut items = Vec::new();
        let (ids, mut limited) = if visible {
            self.push_tree_items(explorer, &identities, &availability, &mut items)
        } else {
            (HashMap::new(), false)
        };
        let result_count = items.len();
        let (agent_ids, agents_limited) = if visible {
            self.push_agent_items(
                explorer,
                group,
                &identities,
                availability,
                &mut items,
                enabled,
            )
        } else {
            (HashMap::new(), false)
        };
        limited |= agents_limited;
        let focus = if !context.keyboard_focused || context.modal || !context.permitted {
            SemanticFocus::None
        } else if visible && explorer.focused() {
            if explorer.search_active() {
                SemanticFocus::Search
            } else {
                explorer
                    .tree_selected()
                    .and_then(|selected| ids.get(&selected).copied())
                    .or_else(|| {
                        explorer
                            .agent_selected()
                            .and_then(|selected| agent_ids.get(&selected).copied())
                    })
                    .map_or(SemanticFocus::Tree, SemanticFocus::Item)
            }
        } else {
            SemanticFocus::Terminal
        };
        let mut next = SemanticNavigationSnapshot {
            generation: self.generation,
            visible,
            enabled,
            result_count,
            status: visible.then(|| {
                self.announced_status(navigation_status(explorer, limited, result_count, &focus))
            }),
            items,
            // The rail has no search: the tree's query waits for it unseen.
            query: if visible && !explorer.collapsed() {
                explorer.query().to_owned()
            } else {
                String::new()
            },
            focus,
        };
        // Keep exact revision/capability/endpoint evidence privately; no such authority
        // is reconstructed from accessible labels or sent back by an AT client.
        if self.snapshot.as_ref() != Some(&next) || self.view.as_ref() != explorer.view() {
            self.generation = self
                .generation
                .checked_add(1)
                .expect("semantic generation exhausted");
            next.generation = self.generation;
            self.view = explorer.view().cloned();
            self.snapshot = Some(next.clone());
        }
        next
    }

    /// Appends the tree's rows to `items`, each under its parent. Returns
    /// their ids by row and whether the item limit cut the tree short.
    fn push_tree_items(
        &mut self,
        explorer: &LairExplorerUi,
        identities: &HashMap<NavigationNodeId, NavigationIdentity>,
        availability: &impl Fn(&LairExplorerRow) -> SemanticAvailability,
        items: &mut Vec<SemanticTreeItem>,
    ) -> (HashMap<NavigationNodeId, SemanticNodeId>, bool) {
        let rows = explorer.rows();
        let mut limited = rows.len() > MAX_ACCESSIBILITY_ITEMS;
        let mut ids = HashMap::new();
        for row in rows.into_iter().take(MAX_ACCESSIBILITY_ITEMS) {
            let Some(identity) = identities.get(&row.id) else {
                continue;
            };
            let Ok(id) = self.registry.id_for(NavigationKey::Tree(identity.clone())) else {
                limited = true;
                break;
            };
            let parent = parent(row.id).and_then(|parent| ids.get(&parent).copied());
            if row.level != 1 && parent.is_none() {
                continue;
            }
            ids.insert(row.id, id);
            self.targets.insert(id, NavigationTarget::Tree(row.id));
            items.push(SemanticTreeItem {
                id,
                parent,
                name: safe_text(&row.label),
                status: safe_text(&row.accessible_status),
                level: row.level,
                expanded: row.expanded,
                selected: explorer.tree_selected() == Some(row.id),
                current: row.current,
                availability: availability(&row),
            });
        }
        (ids, limited)
    }

    /// Appends the agents section to `items`: one more top-level item, a
    /// group whose children are the section's rows, so its Splints read
    /// apart from the tree's. Returns the rows' ids by Splint and whether the
    /// item limit cut the section short.
    fn push_agent_items(
        &mut self,
        explorer: &LairExplorerUi,
        group: Option<NavigationKey>,
        identities: &HashMap<NavigationNodeId, NavigationIdentity>,
        availability: impl Fn(&LairExplorerRow) -> SemanticAvailability,
        items: &mut Vec<SemanticTreeItem>,
        enabled: bool,
    ) -> (HashMap<NavigationNodeId, SemanticNodeId>, bool) {
        let mut ids = HashMap::new();
        let agents = explorer.shown_agent_rows();
        if agents.is_empty() {
            return (ids, false);
        }
        let Some(Ok(group_id)) = group.map(|group| self.registry.id_for(group)) else {
            return (ids, true);
        };
        if items.len() >= MAX_ACCESSIBILITY_ITEMS {
            return (ids, true);
        }
        items.push(SemanticTreeItem {
            id: group_id,
            parent: None,
            name: AGENTS_GROUP_NAME.to_owned(),
            status: format!("{} reporting", agents.len()),
            level: 1,
            expanded: None,
            selected: false,
            current: false,
            availability: if enabled {
                SemanticAvailability::Enabled
            } else {
                SemanticAvailability::Disabled
            },
        });
        for agent in &agents {
            let row = &agent.row;
            let Some(identity) = identities.get(&row.id) else {
                continue;
            };
            if items.len() >= MAX_ACCESSIBILITY_ITEMS {
                return (ids, true);
            }
            let Ok(id) = self.registry.id_for(NavigationKey::Agent(identity.clone())) else {
                return (ids, true);
            };
            ids.insert(row.id, id);
            self.targets.insert(id, NavigationTarget::Agent(row.id));
            items.push(SemanticTreeItem {
                id,
                parent: Some(group_id),
                name: safe_text(&row.label),
                status: safe_text(&row.accessible_status),
                level: 2,
                expanded: None,
                selected: explorer.agent_selected() == Some(row.id),
                current: row.current,
                availability: availability(row),
            });
        }
        (ids, false)
    }

    /// Records each live incarnation's agent state and returns a polite
    /// announcement for transitions into needs input, error or done, or out of
    /// stale. Changes to and from working, and the first view, are silent.
    /// A hidden Explorer or unavailable status forgets the baseline, and only
    /// a current view sets a new one: showing the Explorer refreshes the view
    /// it kept while hidden, which must not be the baseline the fresh view is
    /// compared against. Changes made meanwhile are therefore not replayed.
    fn observe_agent_states(&mut self, explorer: &LairExplorerUi) -> Option<String> {
        let Some(view) = explorer
            .view()
            .filter(|_| explorer.visible() && explorer.agent_status_available())
        else {
            self.agent_states = None;
            return None;
        };
        if self.agent_states.is_none() && view.freshness != EndpointFreshness::Current {
            return None;
        }
        let mut current = HashMap::new();
        let mut changes = Vec::new();
        for splint in view
            .lairs
            .iter()
            .flat_map(|lair| &lair.dojos)
            .flat_map(|dojo| &dojo.splints)
        {
            let (Some(status), Some(incarnation)) =
                (&splint.agent_status, splint.target.live_incarnation)
            else {
                continue;
            };
            let key = AgentStatusKey {
                endpoint: view.endpoint_namespace.clone(),
                endpoint_generation: view.endpoint_generation,
                splint_id: splint.target.splint_id,
                incarnation,
            };
            current.insert(key.clone(), (status.state, status.stale));
            changes.push((key, splint.label.as_str(), status.state, status.stale));
        }
        let previous = self.agent_states.replace(current)?;
        let messages = changes
            .into_iter()
            .filter(|(key, _, state, is_stale)| {
                announces_agent_transition(previous.get(key).copied(), *state, *is_stale)
            })
            .map(|(_, label, state, _)| format!("{label}: {}", state_text(state)))
            .collect::<Vec<_>>();
        (!messages.is_empty()).then(|| messages.join("; "))
    }

    /// Prefixes a pending announcement until the navigation status it was
    /// spoken with changes, so the live region speaks it exactly once. A new
    /// announcement identical to the status already shown is marked as
    /// repeated, since the live region speaks only changed text.
    fn announced_status(&mut self, base: String) -> String {
        let shown = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.status.as_deref());
        let Some((message, anchor)) = &mut self.agent_announcement else {
            return base;
        };
        if anchor.is_empty() {
            anchor.clone_from(&base);
            if shown == Some(safe_text(&format!("{message} · {base}")).as_str()) {
                message.push_str(" again");
            }
        } else if *anchor != base {
            self.agent_announcement = None;
            return base;
        }
        safe_text(&format!("{message} · {base}"))
    }

    pub(crate) fn resolve(
        &self,
        request: SemanticActionRequest,
        explorer: &LairExplorerUi,
        context: NavigationAccessContext,
    ) -> Option<NavigationAccessAction> {
        if !context.enabled()
            || !explorer.visible()
            || request.generation != self.generation
            || self.view.as_ref() != explorer.view()
        {
            return None;
        }
        let snapshot = self.snapshot.as_ref()?;
        if !snapshot.enabled {
            return None;
        }
        let target = |id| self.targets.get(&id).copied();
        // The rail has no search field and no disclosure, so neither may
        // clobber the tree's query or expansion behind it.
        if explorer.collapsed()
            && matches!(
                request.action,
                SemanticAction::Focus(SEARCH_NODE_ID)
                    | SemanticAction::SetSearch(_)
                    | SemanticAction::SetExpanded { .. }
            )
        {
            return None;
        }
        match request.action {
            SemanticAction::Focus(TERMINAL_NODE_ID) => Some(NavigationAccessAction::Terminal),
            SemanticAction::Focus(SEARCH_NODE_ID) => Some(NavigationAccessAction::SearchFocus),
            SemanticAction::Focus(TREE_NODE_ID) => Some(NavigationAccessAction::Tree),
            SemanticAction::Focus(id) => target(id).map(|target| match target {
                NavigationTarget::Tree(node) => NavigationAccessAction::Item(node),
                NavigationTarget::Agent(node) => NavigationAccessAction::AgentItem(node),
            }),
            SemanticAction::SetSearch(query) if valid_query_text(&query).is_ok() => {
                Some(NavigationAccessAction::Search(query))
            }
            SemanticAction::SetExpanded { node, expanded } => {
                snapshot
                    .items
                    .iter()
                    .find(|item| item.id == node && item.expanded.is_some())?;
                match target(node)? {
                    NavigationTarget::Tree(id) => {
                        Some(NavigationAccessAction::Expanded(id, expanded))
                    }
                    NavigationTarget::Agent(_) => None,
                }
            }
            SemanticAction::Activate(id) => {
                if context.pending || explorer.activation_pending() {
                    return None;
                }
                snapshot.items.iter().find(|item| {
                    item.id == id && item.availability == SemanticAvailability::Enabled
                })?;
                target(id).map(|target| match target {
                    NavigationTarget::Tree(node) => NavigationAccessAction::Activate(node),
                    NavigationTarget::Agent(node) => NavigationAccessAction::ActivateAgent(node),
                })
            }
            SemanticAction::SetSearch(_) => None,
        }
    }
}

/// Equivalent states and stale entries are silent; leaving stale is spoken.
fn announces_agent_transition(
    before: Option<(AgentState, bool)>,
    state: AgentState,
    is_stale: bool,
) -> bool {
    if is_stale {
        return false;
    }
    match before {
        Some((_, true)) => true,
        Some((previous, false)) if previous == state => false,
        _ => matches!(
            state,
            AgentState::NeedsInput | AgentState::Error | AgentState::Done
        ),
    }
}

fn navigation_status(
    explorer: &LairExplorerUi,
    limited: bool,
    count: usize,
    focus: &SemanticFocus,
) -> String {
    let status = explorer.status_message().map_or_else(
        || {
            if limited {
                "Navigation item limit reached".to_owned()
            } else {
                format!("{count} results")
            }
        },
        str::to_owned,
    );
    let hint = match focus {
        SemanticFocus::Search => "Typing searches labels",
        SemanticFocus::Tree | SemanticFocus::Item(_) => "F cycles filters",
        SemanticFocus::Terminal => "Focus Explorer to change filters",
        SemanticFocus::None => "Navigation shortcuts unavailable",
    };
    let scope = match (explorer.searching_all_lairs(), explorer.filter()) {
        (false, filter) => format!("{} filter", filter.label()),
        (true, crate::frontend::LairExplorerFilter::All) => "Searching all Lairs".to_owned(),
        // The results come from every Lair; the chosen filter returns when the
        // search is cleared.
        (true, filter) => format!("Searching all Lairs, {} filter kept", filter.label()),
    };
    format!("{scope} · {status} · {hint}")
}

fn identities(
    view: Option<&NavigationExplorerView>,
) -> HashMap<NavigationNodeId, NavigationIdentity> {
    let mut identities = HashMap::new();
    if let Some(view) = view {
        let identity = |node, live_incarnation, last_incarnation| NavigationIdentity {
            endpoint: view.endpoint_namespace.clone(),
            endpoint_generation: view.endpoint_generation,
            node,
            live_incarnation,
            last_incarnation,
        };
        for lair in &view.lairs {
            identities.insert(lair.id, identity(lair.id, None, None));
            for dojo in &lair.dojos {
                identities.insert(dojo.id, identity(dojo.id, None, None));
                for splint in &dojo.splints {
                    identities.insert(
                        splint.id,
                        identity(
                            splint.id,
                            splint.target.live_incarnation,
                            splint.target.last_incarnation,
                        ),
                    );
                }
            }
        }
    }
    identities
}

fn parent(id: NavigationNodeId) -> Option<NavigationNodeId> {
    match id {
        NavigationNodeId::Lair(_) => None,
        NavigationNodeId::Dojo { lair_id, .. } => Some(NavigationNodeId::Lair(lair_id)),
        NavigationNodeId::Splint {
            lair_id, dojo_id, ..
        } => Some(NavigationNodeId::Dojo { lair_id, dojo_id }),
    }
}

fn safe_text(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control() && !is_bidi_formatting(*character))
        .take(MAX_ACCESSIBLE_NAME_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::lair_explorer::tests::view;
    use super::*;
    use crate::frontend::LairExplorerDecision;
    use crate::navigation_projection::{
        NavigationAction, NavigationAvailability, NavigationBlocker,
    };

    fn context() -> NavigationAccessContext {
        NavigationAccessContext {
            keyboard_focused: true,
            modal: false,
            permitted: true,
            pending: false,
        }
    }
    fn explorer() -> LairExplorerUi {
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(view());
        explorer.focus();
        explorer.reveal_current();
        explorer
    }
    fn request(
        snapshot: &SemanticNavigationSnapshot,
        action: SemanticAction,
    ) -> SemanticActionRequest {
        SemanticActionRequest {
            generation: snapshot.generation,
            action,
        }
    }

    #[test]
    fn navigation_accessibility_projects_real_rows_and_independent_current_axes() {
        let mut explorer = explorer();
        let mut accessibility = NavigationAccessibility::default();
        let snapshot = accessibility.refresh(&explorer, context());
        snapshot.tree_update().unwrap();
        assert_eq!(snapshot.items.len(), explorer.rows().len());
        assert_eq!(
            snapshot
                .items
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            vec!["work", "editor", "shell", "notes"]
        );
        assert_eq!(snapshot.items.iter().filter(|item| item.current).count(), 3);
        assert_eq!(
            snapshot.items.iter().filter(|item| item.selected).count(),
            1
        );
        assert_eq!(snapshot.items[2].parent, Some(snapshot.items[1].id));
        assert_eq!(snapshot.focus, SemanticFocus::Item(snapshot.items[2].id));
        assert_eq!(accessibility.refresh(&explorer, context()), snapshot);
        explorer.begin_search();
        explorer.set_search("no matches".into());
        let empty = accessibility.refresh(&explorer, context());
        assert!(empty.items.is_empty());
        assert_eq!(empty.focus, SemanticFocus::Search);
        assert_eq!(
            empty.status.as_deref(),
            Some("Searching all Lairs · No matches · Typing searches labels")
        );
        assert_eq!(empty.query, "no matches");
        assert!(
            accessibility
                .resolve(
                    request(&snapshot, SemanticAction::Activate(snapshot.items[2].id)),
                    &explorer,
                    context()
                )
                .is_none()
        );
    }

    #[test]
    fn navigation_accessibility_filter_hint_matches_input_owner() {
        let mut explorer = explorer();
        assert!(
            super::navigation_status(&explorer, false, 4, &SemanticFocus::Tree)
                .contains("F cycles filters")
        );
        explorer.begin_search();
        let search = super::navigation_status(&explorer, false, 4, &SemanticFocus::Search);
        assert!(search.contains("Typing searches labels"));
        assert!(!search.contains("F cycles filters"));
        explorer.return_to_terminal();
        let terminal = super::navigation_status(&explorer, false, 4, &SemanticFocus::Terminal);
        assert!(terminal.contains("Focus Explorer"));
        assert!(!terminal.contains("F cycles filters"));
    }

    #[test]
    fn navigation_accessibility_hints_follow_effective_window_focus_and_authority() {
        let mut explorer = explorer();
        let mut accessibility = NavigationAccessibility::default();
        for searching in [false, true] {
            if searching {
                explorer.begin_search();
            }
            for blocked in [
                NavigationAccessContext {
                    keyboard_focused: false,
                    ..context()
                },
                NavigationAccessContext {
                    permitted: false,
                    ..context()
                },
            ] {
                let snapshot = accessibility.refresh(&explorer, blocked);
                assert_eq!(snapshot.focus, SemanticFocus::None);
                let status = snapshot.status.as_deref().unwrap();
                assert!(status.contains("Navigation shortcuts unavailable"));
                assert!(!status.contains("F cycles filters"));
                assert!(!status.contains("Typing searches labels"));
            }
            let restored = accessibility.refresh(&explorer, context());
            assert!(restored.status.as_deref().unwrap().contains(if searching {
                "Typing searches labels"
            } else {
                "F cycles filters"
            }));
        }
    }

    #[test]
    fn navigation_accessibility_tracks_filter_and_rejects_hidden_targets() {
        use crate::frontend::LairExplorerFilter;
        let mut explorer = explorer();
        let mut accessibility = NavigationAccessibility::default();
        let all = accessibility.refresh(&explorer, context());
        let hidden = all.items[3].id;
        explorer.set_filter(LairExplorerFilter::Saved);
        let saved = accessibility.refresh(&explorer, context());
        assert!(saved.generation > all.generation);
        assert_eq!(saved.items, all.items);
        assert!(saved.status.as_deref().unwrap().starts_with("Saved filter"));
        explorer.set_filter(LairExplorerFilter::Live);
        let live = accessibility.refresh(&explorer, context());
        live.tree_update().unwrap();
        assert_eq!(live.items.len(), 3);
        assert!(live.status.as_deref().unwrap().starts_with("Live filter"));
        assert!(
            accessibility
                .resolve(
                    request(&live, SemanticAction::Activate(hidden)),
                    &explorer,
                    context()
                )
                .is_none()
        );
        assert!(
            accessibility
                .resolve(
                    request(&all, SemanticAction::Activate(all.items[2].id)),
                    &explorer,
                    context()
                )
                .is_none()
        );
        explorer.set_filter(LairExplorerFilter::All);
        let restored = accessibility.refresh(&explorer, context());
        assert_eq!(restored.items, all.items);

        // A search under Live or Saved says it covers every Lair and that the
        // filter is kept; clearing it announces the filter again.
        for filter in [LairExplorerFilter::Live, LairExplorerFilter::Saved] {
            explorer.set_filter(filter);
            explorer.set_search("a".into());
            let searching = accessibility.refresh(&explorer, context());
            let status = searching.status.as_deref().unwrap();
            assert!(
                status.starts_with(&format!(
                    "Searching all Lairs, {} filter kept",
                    filter.label()
                )),
                "{status}"
            );
            explorer.set_search(String::new());
            let cleared = accessibility.refresh(&explorer, context());
            assert!(
                cleared
                    .status
                    .as_deref()
                    .unwrap()
                    .starts_with(&format!("{} filter", filter.label()))
            );
        }
    }

    #[test]
    fn navigation_accessibility_rejects_modal_focus_permission_and_hidden_actions() {
        let mut explorer = explorer();
        let mut accessibility = NavigationAccessibility::default();
        let snapshot = accessibility.refresh(&explorer, context());
        let actions = [
            SemanticAction::Focus(TERMINAL_NODE_ID),
            SemanticAction::SetSearch("a".into()),
            SemanticAction::Activate(snapshot.items[2].id),
            SemanticAction::SetExpanded {
                node: snapshot.items[0].id,
                expanded: false,
            },
        ];
        for blocked in [
            NavigationAccessContext {
                modal: true,
                ..context()
            },
            NavigationAccessContext {
                keyboard_focused: false,
                ..context()
            },
            NavigationAccessContext {
                permitted: false,
                ..context()
            },
        ] {
            for action in &actions {
                assert!(
                    accessibility
                        .resolve(request(&snapshot, action.clone()), &explorer, blocked)
                        .is_none()
                );
            }
            let state = accessibility.refresh(&explorer, blocked);
            assert!(!state.enabled);
            if !blocked.permitted {
                assert!(state.visible);
                assert!(!state.items.is_empty());
                assert!(
                    state
                        .items
                        .iter()
                        .all(|item| item.availability == SemanticAvailability::Disabled)
                );
            }
        }
        explorer.toggle_visibility();
        let hidden = accessibility.refresh(&explorer, context());
        assert!(!hidden.visible);
        assert!(hidden.items.is_empty());
        assert!(hidden.query.is_empty());
        assert!(hidden.status.is_none());
    }

    #[test]
    fn navigation_accessibility_never_retargets_incarnation_endpoint_revision_or_removed_identity()
    {
        let mut explorer = explorer();
        let mut accessibility = NavigationAccessibility::default();
        let first = accessibility.refresh(&explorer, context());
        let id = first.items[2].id;
        let mut changed = explorer.view().unwrap().clone();
        changed.lairs[0].dojos[0].splints[0].target.live_incarnation = Some(4);
        explorer.set_view(changed.clone());
        assert!(
            accessibility
                .resolve(
                    request(&first, SemanticAction::Activate(id)),
                    &explorer,
                    context()
                )
                .is_none()
        );
        let next = accessibility.refresh(&explorer, context());
        assert_ne!(next.items[2].id, id);
        assert!(
            accessibility
                .resolve(
                    request(&next, SemanticAction::Activate(id)),
                    &explorer,
                    context()
                )
                .is_none()
        );
        changed.endpoint_generation += 1;
        explorer.set_view(changed.clone());
        let endpoint = accessibility.refresh(&explorer, context());
        assert_ne!(endpoint.items[0].id, next.items[0].id);
        changed.topology_revision = splinterm_core::TopologyRevision::new(99);
        explorer.set_view(changed.clone());
        let revision = accessibility.refresh(&explorer, context());
        assert_eq!(revision.items[2].id, endpoint.items[2].id);
        assert!(
            accessibility
                .resolve(
                    request(&endpoint, SemanticAction::Activate(endpoint.items[2].id)),
                    &explorer,
                    context()
                )
                .is_none()
        );
        let removed = changed.lairs[0].dojos[0].splints.remove(0);
        explorer.set_view(changed.clone());
        accessibility.refresh(&explorer, context());
        changed.lairs[0].dojos[0].splints.push(removed);
        explorer.set_view(changed);
        let reborn = accessibility.refresh(&explorer, context());
        assert_ne!(reborn.items[2].id, revision.items[2].id);
    }

    #[test]
    fn navigation_accessibility_activation_keeps_restore_preview_and_capability_guards() {
        let mut explorer = explorer();
        let mut view = explorer.view().unwrap().clone();
        view.lairs[0].dojos[0].splints[0].target.capability.action =
            NavigationAction::PreviewRestoreSplint;
        explorer.set_view(view.clone());
        let mut accessibility = NavigationAccessibility::default();
        let snapshot = accessibility.refresh(&explorer, context());
        let action = accessibility
            .resolve(
                request(&snapshot, SemanticAction::Activate(snapshot.items[2].id)),
                &explorer,
                context(),
            )
            .unwrap();
        let NavigationAccessAction::Activate(id) = action else {
            panic!("typed activation expected")
        };
        explorer.select(id);
        let Some(LairExplorerDecision::FocusSplint(target)) = explorer.decision() else {
            panic!("existing explorer path expected")
        };
        assert_eq!(
            target.capability.action,
            NavigationAction::PreviewRestoreSplint
        );
        assert!(explorer.set_pending(LairExplorerDecision::FocusSplint(target)));
        let pending = accessibility.refresh(&explorer, context());
        assert_eq!(
            accessibility.resolve(
                request(&pending, SemanticAction::Focus(TERMINAL_NODE_ID)),
                &explorer,
                context()
            ),
            Some(NavigationAccessAction::Terminal)
        );
        assert!(
            accessibility
                .resolve(
                    request(&pending, SemanticAction::Activate(pending.items[2].id)),
                    &explorer,
                    context()
                )
                .is_none()
        );
        explorer.clear_pending();
        view.lairs[0].dojos[0].splints[0]
            .target
            .capability
            .availability = NavigationAvailability::Disabled(NavigationBlocker::PermissionDenied);
        explorer.set_view(view);
        let denied = accessibility.refresh(&explorer, context());
        assert_eq!(denied.items[2].availability, SemanticAvailability::Disabled);
        assert!(
            accessibility
                .resolve(
                    request(&denied, SemanticAction::Activate(denied.items[2].id)),
                    &explorer,
                    context()
                )
                .is_none()
        );
        assert!(explorer.decision().is_none());
    }

    /// While the Explorer is collapsed to its rail, its rows offer no
    /// disclosure, the published query is empty, and search and expansion
    /// requests are refused, so the tree's query and expansion behind the
    /// rail are never changed unseen. Focusing and activating a Lair still
    /// work.
    #[test]
    fn navigation_accessibility_rail_never_touches_the_hidden_search_or_tree() {
        use crate::frontend::LairExplorerFilter;

        let mut explorer = explorer();
        explorer.set_search("work".into());
        explorer.focus_tree();
        assert!(explorer.set_collapsed(true));
        let mut accessibility = NavigationAccessibility::default();
        let snapshot = accessibility.refresh(&explorer, context());
        assert!(!snapshot.items.is_empty());
        assert!(snapshot.items.iter().all(|item| item.level == 1));
        assert!(snapshot.items.iter().all(|item| item.expanded.is_none()));
        assert_eq!(snapshot.query, "");
        let lair = snapshot.items[0].id;
        for action in [
            SemanticAction::Focus(SEARCH_NODE_ID),
            SemanticAction::SetSearch("notes".into()),
            SemanticAction::SetExpanded {
                node: lair,
                expanded: true,
            },
        ] {
            assert_eq!(
                accessibility.resolve(request(&snapshot, action.clone()), &explorer, context()),
                None,
                "{action:?}"
            );
        }
        assert!(
            accessibility
                .resolve(
                    request(&snapshot, SemanticAction::Focus(lair)),
                    &explorer,
                    context()
                )
                .is_some()
        );
        assert!(
            accessibility
                .resolve(
                    request(&snapshot, SemanticAction::Activate(lair)),
                    &explorer,
                    context()
                )
                .is_some()
        );
        assert_eq!(explorer.query(), "work");
        // The rail lists the chosen filter's Lairs whatever query waits in
        // the tree, and its status names that filter.
        for filter in LairExplorerFilter::ALL {
            explorer.set_filter(filter);
            let snapshot = accessibility.refresh(&explorer, context());
            assert!(
                snapshot
                    .status
                    .as_deref()
                    .unwrap()
                    .starts_with(&format!("{} filter ·", filter.label())),
                "{filter:?}: {:?}",
                snapshot.status
            );
        }
        // Expanded again, the query is published as before.
        explorer.set_collapsed(false);
        let snapshot = accessibility.refresh(&explorer, context());
        assert_eq!(snapshot.query, "work");
    }

    #[test]
    fn navigation_accessibility_pending_activation_preserves_nonmutating_navigation() {
        let explorer = explorer();
        let mut accessibility = NavigationAccessibility::default();
        let pending = NavigationAccessContext {
            pending: true,
            ..context()
        };
        let snapshot = accessibility.refresh(&explorer, pending);
        assert!(snapshot.enabled);
        for action in [
            SemanticAction::Focus(TERMINAL_NODE_ID),
            SemanticAction::Focus(snapshot.items[0].id),
            SemanticAction::SetSearch("work".into()),
            SemanticAction::SetExpanded {
                node: snapshot.items[0].id,
                expanded: false,
            },
        ] {
            assert!(
                accessibility
                    .resolve(request(&snapshot, action), &explorer, pending)
                    .is_some()
            );
        }
        assert!(
            accessibility
                .resolve(
                    request(&snapshot, SemanticAction::Activate(snapshot.items[2].id)),
                    &explorer,
                    pending
                )
                .is_none()
        );
        assert!(
            snapshot
                .items
                .iter()
                .all(|item| item.availability == SemanticAvailability::Disabled)
        );
    }

    #[test]
    fn navigation_accessibility_bounds_metadata_and_registry_churn() {
        assert_eq!(
            safe_text(&format!("\n\u{202e}{}", "x".repeat(1000))),
            "x".repeat(MAX_ACCESSIBLE_NAME_CHARS)
        );
        let mut registry = SemanticNodeRegistry::default();
        let first = registry.id_for(0).unwrap();
        for value in 1..MAX_ACCESSIBILITY_ITEMS * 2 {
            registry.retain(|_| false);
            assert_ne!(registry.id_for(value).unwrap(), first);
        }
    }

    fn set_agent(explorer: &mut LairExplorerUi, state: AgentState, is_stale: bool) {
        let splint_id = explorer.view().unwrap().lairs[0].dojos[0].splints[0]
            .target
            .splint_id;
        explorer.apply_agent_status(
            splint_id,
            3,
            Some(crate::navigation_projection::NavigationAgentStatus {
                state,
                detail: Some("approve the plan".to_owned()),
                stale: is_stale,
                stale_minutes: is_stale.then_some(1),
            }),
        );
    }

    #[test]
    fn item_states_carry_agent_state_in_words() {
        let mut explorer = explorer();
        set_agent(&mut explorer, AgentState::NeedsInput, false);
        let snapshot = NavigationAccessibility::default().refresh(&explorer, context());
        snapshot.tree_update().unwrap();
        let statuses = snapshot
            .items
            .iter()
            .map(|item| (item.name.as_str(), item.status.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            statuses,
            [
                ("work", "Running · Saved · 1 needs input"),
                ("editor", "Running · Current · 1 needs input"),
                ("shell", "Running · Needs input · approve the plan"),
                ("notes", "Unavailable · Pinned"),
                // The agents section repeats the Splint after the tree.
                ("Agents", "1 reporting"),
                (
                    "shell",
                    "work / editor · Running · Needs input · approve the plan"
                ),
            ]
        );
        set_agent(&mut explorer, AgentState::Working, true);
        let snapshot = NavigationAccessibility::default().refresh(&explorer, context());
        assert_eq!(
            snapshot.items[2].status,
            "Running · Working · stale (no report for 1 min) · approve the plan"
        );
    }

    #[test]
    fn agent_announcements_speak_only_needs_input_error_done_and_leaving_stale() {
        let mut explorer = explorer();
        let mut accessibility = NavigationAccessibility::default();
        set_agent(&mut explorer, AgentState::NeedsInput, false);
        let mut previous = accessibility.refresh(&explorer, context()).status.unwrap();
        assert!(
            !previous.contains("shell:"),
            "statuses present at first view are not announced"
        );
        for (state, stale, spoken) in [
            (AgentState::NeedsInput, false, None),
            (AgentState::Working, false, None),
            (AgentState::NeedsInput, false, Some("shell: Needs input")),
            (AgentState::Done, false, Some("shell: Done")),
            (AgentState::Working, false, None),
            (AgentState::Working, true, None),
            (AgentState::Working, false, Some("shell: Working")),
            (AgentState::Error, false, Some("shell: Error")),
            (AgentState::Idle, false, None),
            (AgentState::Error, true, None),
            // Ask, work, ask again: the repeat must still change the text.
            (AgentState::NeedsInput, false, Some("shell: Needs input")),
            (AgentState::Working, false, None),
            (
                AgentState::NeedsInput,
                false,
                Some("shell: Needs input again"),
            ),
            (AgentState::Working, false, None),
            (AgentState::NeedsInput, false, Some("shell: Needs input")),
        ] {
            set_agent(&mut explorer, state, stale);
            let status = accessibility.refresh(&explorer, context()).status.unwrap();
            // A second owner refresh in the same turn must not repeat or drop it.
            assert_eq!(
                accessibility.refresh(&explorer, context()).status.as_ref(),
                Some(&status)
            );
            match spoken {
                Some(message) => {
                    assert_ne!(status, previous, "{state:?} stale={stale}");
                    assert!(status.starts_with(&format!("{message} · ")), "{status}");
                }
                None => assert_eq!(status, previous, "{state:?} stale={stale}"),
            }
            previous = status;
        }
        // A changed navigation status supersedes the announcement.
        explorer.set_filter(crate::frontend::LairExplorerFilter::Saved);
        let status = accessibility.refresh(&explorer, context()).status.unwrap();
        assert!(!status.contains("shell:"), "{status}");
    }

    /// A current view carrying `state` for the shell Splint, as a topology
    /// refresh delivers it.
    fn fresh_view(explorer: &LairExplorerUi, state: AgentState) -> NavigationExplorerView {
        let mut view = explorer.view().unwrap().clone();
        view.freshness = EndpointFreshness::Current;
        view.lairs[0].dojos[0].splints[0].agent_status =
            Some(crate::navigation_projection::NavigationAgentStatus {
                state,
                detail: None,
                stale: false,
                stale_minutes: None,
            });
        view
    }

    #[test]
    fn hidden_or_unavailable_explorer_does_not_announce_agent_status() {
        let mut explorer = explorer();
        let mut accessibility = NavigationAccessibility::default();
        set_agent(&mut explorer, AgentState::Working, false);
        accessibility.refresh(&explorer, context());
        explorer.toggle_visibility();
        assert_eq!(accessibility.refresh(&explorer, context()).status, None);
        // Showing refreshes the view kept while hidden; the fresh view then
        // carries what changed meanwhile, which is not replayed.
        explorer.toggle_visibility();
        explorer.begin_refresh();
        let status = accessibility.refresh(&explorer, context()).status.unwrap();
        assert!(!status.contains("shell:"), "{status}");
        explorer.set_view(fresh_view(&explorer, AgentState::NeedsInput));
        let status = accessibility.refresh(&explorer, context()).status.unwrap();
        assert!(!status.contains("shell:"), "{status}");
        // The fresh view is the new baseline for changes seen while visible.
        explorer.set_view(fresh_view(&explorer, AgentState::Done));
        let status = accessibility.refresh(&explorer, context()).status.unwrap();
        assert!(status.starts_with("shell: Done · "), "{status}");

        explorer.mark_disconnected();
        accessibility.refresh(&explorer, context());
        explorer.set_view(fresh_view(&explorer, AgentState::Error));
        let status = accessibility.refresh(&explorer, context()).status.unwrap();
        assert!(!status.contains("shell:"), "{status}");
    }

    /// The agents section is one more top-level item, `Agents`, whose
    /// children are its rows. A Splint in both lists has two items with
    /// distinct, stable ids, only one of them selected; focusing or
    /// activating the agents item acts on the agents row, which has no
    /// disclosure. The section leaves the tree's result count alone and is
    /// gone on the rail.
    #[test]
    fn agents_section_is_a_group_of_its_rows() {
        use crate::navigation_projection::NavigationAgentStatus;

        let mut reporting = view();
        reporting.lairs[0].dojos[0].splints[0].agent_status = Some(NavigationAgentStatus {
            state: AgentState::NeedsInput,
            detail: None,
            stale: false,
            stale_minutes: None,
        });
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(reporting);
        explorer.focus();
        explorer.reveal_current();
        let shell = explorer
            .rows()
            .iter()
            .find(|row| row.label == "shell")
            .unwrap()
            .id;
        let mut accessibility = NavigationAccessibility::default();
        let snapshot = accessibility.refresh(&explorer, context());
        snapshot.tree_update().unwrap();
        let tree = explorer.rows().len();
        assert_eq!(snapshot.result_count, tree);
        assert_eq!(snapshot.items.len(), tree + 2);
        let group = snapshot.items[tree].clone();
        assert_eq!(
            (group.name.as_str(), group.level, group.parent),
            (AGENTS_GROUP_NAME, 1, None)
        );
        assert_eq!(group.status, "1 reporting");
        assert_eq!(group.expanded, None);
        let agent = snapshot.items[tree + 1].clone();
        assert_eq!(
            (agent.name.as_str(), agent.level, agent.parent),
            ("shell", 2, Some(group.id))
        );
        assert!(
            agent.status.starts_with("work / editor · "),
            "{}",
            agent.status
        );
        assert_eq!(agent.availability, SemanticAvailability::Enabled);
        let in_tree = snapshot
            .items
            .iter()
            .find(|item| item.name == "shell" && item.level == 3)
            .unwrap()
            .clone();
        assert_ne!(in_tree.id, agent.id);
        assert!(in_tree.selected && !agent.selected);
        assert_eq!(snapshot.focus, SemanticFocus::Item(in_tree.id));
        let resolve = |accessibility: &NavigationAccessibility, action| {
            accessibility.resolve(request(&snapshot, action), &explorer, context())
        };
        assert_eq!(
            resolve(&accessibility, SemanticAction::Focus(agent.id)),
            Some(NavigationAccessAction::AgentItem(shell))
        );
        assert_eq!(
            resolve(&accessibility, SemanticAction::Activate(agent.id)),
            Some(NavigationAccessAction::ActivateAgent(shell))
        );
        assert_eq!(
            resolve(&accessibility, SemanticAction::Focus(in_tree.id)),
            Some(NavigationAccessAction::Item(shell))
        );
        assert_eq!(
            resolve(
                &accessibility,
                SemanticAction::SetExpanded {
                    node: agent.id,
                    expanded: true
                }
            ),
            None
        );
        assert_eq!(
            resolve(&accessibility, SemanticAction::Activate(group.id)),
            None
        );
        // Selecting the agents row moves the selection and the focus there.
        assert!(explorer.select_agent(shell));
        let selected = accessibility.refresh(&explorer, context());
        selected.tree_update().unwrap();
        let agent_now = selected
            .items
            .iter()
            .find(|item| item.parent == Some(group.id))
            .unwrap();
        assert_eq!(agent_now.id, agent.id);
        assert!(agent_now.selected);
        assert_eq!(
            selected.items.iter().filter(|item| item.selected).count(),
            1
        );
        assert_eq!(selected.focus, SemanticFocus::Item(agent.id));
        // The rail has no section.
        explorer.set_collapsed(true);
        let rail = accessibility.refresh(&explorer, context());
        rail.tree_update().unwrap();
        assert!(rail.items.iter().all(|item| item.name != AGENTS_GROUP_NAME));
    }

    /// While the layout hides the agents section, as in a list too short for
    /// it, assistive technology has no `Agents` group either, and a
    /// selected agents row passes the selection to the Splint's tree row.
    #[test]
    fn hidden_agents_section_has_no_group() {
        use crate::navigation_projection::NavigationAgentStatus;

        let mut reporting = view();
        reporting.lairs[0].dojos[0].splints[0].agent_status = Some(NavigationAgentStatus {
            state: AgentState::NeedsInput,
            detail: None,
            stale: false,
            stale_minutes: None,
        });
        let mut explorer = LairExplorerUi::default();
        explorer.set_view(reporting);
        explorer.focus();
        explorer.reveal_current();
        let shell = explorer
            .rows()
            .iter()
            .find(|row| row.label == "shell")
            .unwrap()
            .id;
        let mut accessibility = NavigationAccessibility::default();
        assert!(explorer.select_agent(shell));
        let shown = accessibility.refresh(&explorer, context());
        assert!(
            shown
                .items
                .iter()
                .any(|item| item.name == AGENTS_GROUP_NAME)
        );
        assert!(explorer.set_agents_shown(false));
        let hidden = accessibility.refresh(&explorer, context());
        hidden.tree_update().unwrap();
        assert!(
            hidden
                .items
                .iter()
                .all(|item| item.name != AGENTS_GROUP_NAME)
        );
        assert_eq!(explorer.tree_selected(), Some(shell));
        let selected = hidden.items.iter().find(|item| item.selected).unwrap();
        assert_eq!(selected.name, "shell");
        assert_eq!(hidden.focus, SemanticFocus::Item(selected.id));
    }
}
