//! Window lifecycle and callback wake integration; transport never owns App.

use super::{App, QueueHandle, Waker};
use crate::accessibility::{SemanticActionQueue, SemanticOwnerTurn, UnixAccessibilityAdapter};
use crate::frontend::{NavigationAccessAction, NavigationAccessContext, NavigationAccessibility};

pub(super) struct WindowAccessibility {
    projection: NavigationAccessibility,
    actions: SemanticActionQueue,
    adapter: Option<UnixAccessibilityAdapter>,
    turn: u64,
}

impl WindowAccessibility {
    pub(super) fn new(waker: Waker) -> Self {
        Self {
            projection: NavigationAccessibility::default(),
            actions: SemanticActionQueue::new(move || waker.wake_by_ref()),
            adapter: None,
            turn: 0,
        }
    }
}

impl App {
    fn navigation_access_context(&self) -> NavigationAccessContext {
        NavigationAccessContext {
            keyboard_focused: self.input.keyboard_focused,
            modal: self.modal.input_modal_open()
                || self.modal.trusted_consent.is_some()
                || self.modal.session_picker.is_some()
                || self.modal.session_picker_requested,
            permitted: self.tab_state.managed_tabs
                && self.tab_state.topology_commands.is_some()
                && self.panes.pane.controller_active
                && !self.scheduling.exit,
            pending: self.tab_state.session_switch_pending,
        }
    }

    pub(super) fn accessibility_turn(&mut self, queue_handle: &QueueHandle<Self>) {
        let context = self.navigation_access_context();
        self.accessibility
            .projection
            .refresh(&self.explorer, context);
        for request in self.accessibility.actions.drain_requests() {
            let context = self.navigation_access_context();
            let Some(action) =
                self.accessibility
                    .projection
                    .resolve(request, &self.explorer, context)
            else {
                continue;
            };
            match action {
                NavigationAccessAction::Terminal => self.set_explorer_focus(false, queue_handle),
                action => {
                    self.set_explorer_focus(true, queue_handle);
                    match action {
                        NavigationAccessAction::Tree => self.explorer.focus_tree(),
                        NavigationAccessAction::SearchFocus => {
                            self.explorer.begin_search();
                        }
                        NavigationAccessAction::Search(query) => self.explorer.set_search(query),
                        NavigationAccessAction::Item(id) => {
                            self.explorer.focus_tree();
                            self.explorer.select(id);
                        }
                        NavigationAccessAction::Expanded(id, expanded) => {
                            self.explorer.focus_tree();
                            self.explorer.select(id);
                            if self
                                .explorer
                                .rows()
                                .iter()
                                .any(|row| row.id == id && row.expanded == Some(!expanded))
                            {
                                self.explorer.toggle_selected();
                            }
                        }
                        NavigationAccessAction::Activate(id) => {
                            self.explorer.focus_tree();
                            self.explorer.select(id);
                            if let Some(decision) = self.explorer.decision() {
                                self.execute_lair_explorer_decision(decision);
                            }
                        }
                        NavigationAccessAction::AgentItem(id) => {
                            self.explorer.focus_tree();
                            self.explorer.select_agent(id);
                        }
                        NavigationAccessAction::ActivateAgent(id) => {
                            self.explorer.focus_tree();
                            self.explorer.select_agent(id);
                            if self.explorer.agent_selected() == Some(id)
                                && let Some(decision) = self.explorer.decision()
                            {
                                self.execute_lair_explorer_decision(decision);
                            }
                        }
                        NavigationAccessAction::Terminal => unreachable!(),
                    }
                }
            }
            self.presentation.full_redraw = true;
            self.scheduling.redraw_pending = true;
            // Each accepted transition invalidates any remaining callbacks from
            // the old semantic state, including search-filtered row actions.
            let context = self.navigation_access_context();
            self.accessibility
                .projection
                .refresh(&self.explorer, context);
        }
        let context = self.navigation_access_context();
        let snapshot = self
            .accessibility
            .projection
            .refresh(&self.explorer, context);
        self.accessibility.turn = self.accessibility.turn.saturating_add(1);
        if let Some(adapter) = &mut self.accessibility.adapter {
            adapter.stage(snapshot);
            // Invalid projections fail closed; they must not terminate the terminal.
            if adapter
                .publish_pending(SemanticOwnerTurn(self.accessibility.turn))
                .is_err()
            {
                self.accessibility.adapter = None;
                return;
            }
        } else {
            self.accessibility.adapter =
                UnixAccessibilityAdapter::new(snapshot, self.accessibility.actions.clone()).ok();
        }
        if let Some(adapter) = &mut self.accessibility.adapter {
            adapter.update_window_focus_state(context.keyboard_focused);
        }
    }
}
