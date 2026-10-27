//! Window-local agent access (ADR 0014, Plan 0053 PR 5): the **Agent
//! access** list in the command palette, its revoke prompts, **Revoke all
//! access** with exact counts, the Explorer marks, and the waiting indicator.
//!
//! The topology manager reads the daemon's access inventory and sends it
//! here; nothing in this module decides authority. Every prompt opens only
//! because the person asked for it, inside this Window; a waiting access
//! request is announced in words and never opens or focuses anything.

use super::{
    App, BTN_LEFT, DojoPromptUi, Keysym, MouseAction, PointerEvent, PointerEventKind, QueueHandle,
    Result, SessionPickerUi, WindowTopologyCommand, binding_help_repeat_consumed,
    picker_repeat_consumed,
};
use crate::frontend::{
    AccessActivation, AccessMarks, AccessSnapshot, AgentAccessUi, PromptReconcile, RevokeAccessUi,
    WindowAccessCommand, WindowAccessUpdate, counts_are_empty,
};
use crate::renderer::command_palette_row_hit_test;
use splinterm_protocol::AccessEntryKey;

/// What this Window last heard about access.
#[derive(Debug, Default)]
pub(super) struct WindowAccess {
    snapshot: Option<AccessSnapshot>,
    unavailable: Option<String>,
    waiting: u32,
    /// The entry on the list row a pointer press began on.
    pressed: Option<AccessEntryKey>,
}

impl WindowAccess {
    /// Access requests waiting for an answer, in every workspace.
    pub(super) const fn waiting(&self) -> u32 {
        self.waiting
    }

    /// Whether **Revoke all access** may have anything to revoke: true until
    /// the inventory was read, so a stale or missing read never hides it.
    pub(super) fn revoke_all_available(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_none_or(AccessSnapshot::revoke_all_has_targets)
    }

    /// Keeps a fresh read; whether the waiting count changed.
    fn record_snapshot(&mut self, snapshot: AccessSnapshot, waiting: u32) -> bool {
        self.snapshot = Some(snapshot);
        self.unavailable = None;
        std::mem::replace(&mut self.waiting, waiting) != waiting
    }

    /// Forgets the last read: what it showed may no longer be true, so no
    /// mark or waiting count outlives it. Whether the waiting count changed.
    fn record_unavailable(&mut self, reason: String) -> bool {
        self.snapshot = None;
        self.unavailable = Some(reason);
        std::mem::take(&mut self.waiting) != 0
    }
}

/// Whether a held key must not repeat into the access list or a revoke
/// prompt: a repeat never activates a row or answers a prompt.
fn access_repeat_consumed(open: bool, keysym: Keysym) -> bool {
    open && matches!(keysym, Keysym::Return | Keysym::KP_Enter)
}

impl App {
    /// Whether the access list or a revoke prompt is showing.
    fn access_surface_open(&self) -> bool {
        self.modal.agent_access.is_some()
            || matches!(self.modal.dojo_prompt, Some(DojoPromptUi::RevokeAccess(_)))
    }

    /// Whether a held key must not repeat into binding help, the access
    /// list, a revoke prompt, or an inline picker's Tab and retry.
    pub(super) fn modal_repeat_consumed(&self, keysym: Keysym) -> bool {
        let picker = self
            .modal
            .session_picker
            .as_ref()
            .filter(|picker| picker.is_inline());
        binding_help_repeat_consumed(self.modal.binding_help.is_some(), keysym)
            || access_repeat_consumed(self.access_surface_open(), keysym)
            || picker_repeat_consumed(
                picker.is_some(),
                picker.is_some_and(SessionPickerUi::retryable),
                keysym,
            )
    }

    pub(super) fn show_agent_access(&mut self, queue_handle: &QueueHandle<Self>) -> Result<()> {
        self.show_command_palette()?;
        self.modal.agent_access = Some(AgentAccessUi::new(
            self.access.snapshot.as_ref(),
            self.access.unavailable.as_deref(),
        ));
        self.access.pressed = None;
        if self.input.ime_modal_barrier {
            self.renew_text_input(queue_handle);
        }
        self.surface.window.set_title("Splinterm — Agent access");
        self.send_access_command(WindowAccessCommand::Refresh);
        Ok(())
    }

    /// **Revoke all access**: counts first, so the person confirms the exact
    /// numbers before anything is revoked.
    pub(super) fn begin_revoke_all_access(&mut self) {
        self.send_access_command(WindowAccessCommand::CountRevokeAll);
    }

    fn send_access_command(&mut self, command: WindowAccessCommand) {
        if self
            .send_topology_command(WindowTopologyCommand::Access(command))
            .is_err()
        {
            if let Some(list) = self.modal.agent_access.as_mut() {
                list.set_notice("Agent access is unavailable: this Window lost its daemon");
            }
            eprintln!("splinterm agent access request failed");
        }
    }

    /// Applies what the topology manager heard; whether anything visible
    /// changed.
    pub(super) fn apply_access_update(&mut self, update: WindowAccessUpdate) -> bool {
        match update {
            WindowAccessUpdate::Snapshot(snapshot) => self.apply_access_snapshot(snapshot),
            WindowAccessUpdate::Unavailable(reason) => {
                let mut changed = self.explorer.set_access_marks(AccessMarks::default());
                if let Some(list) = self.modal.agent_access.as_mut() {
                    list.set_unavailable(&reason);
                    self.refresh_command_palette();
                    changed = true;
                }
                if self.access.record_unavailable(reason) {
                    self.refresh_waiting_title();
                    changed = true;
                }
                changed
            }
            WindowAccessUpdate::RevokeAllCounts { counts, recounted } => {
                let prompt = if counts_are_empty(counts) {
                    RevokeAccessUi::notice(
                        "revoke all access",
                        if recounted {
                            "Nothing is left to revoke, and nothing was revoked."
                        } else {
                            "Nothing to revoke: no agent holds a grant, and no request is waiting."
                        },
                    )
                } else {
                    RevokeAccessUi::all(counts, recounted)
                };
                self.show_access_prompt(prompt)
            }
            WindowAccessUpdate::Revoked {
                message,
                failed,
                all,
                entry,
            } => {
                if !all && let Some(list) = self.modal.agent_access.as_mut() {
                    list.finish_revoke(entry.as_ref(), &message);
                    self.refresh_command_palette();
                    return true;
                }
                if all || failed {
                    return self.show_access_prompt(RevokeAccessUi::notice(
                        if failed {
                            "agent access not fully revoked"
                        } else {
                            "agent access revoked"
                        },
                        &message,
                    ));
                }
                false
            }
        }
    }

    fn apply_access_snapshot(&mut self, snapshot: AccessSnapshot) -> bool {
        let marks = AccessMarks::from_snapshot(&snapshot);
        let waiting = marks.total_waiting();
        let mut changed = self.explorer.set_access_marks(marks);
        if let Some(list) = self.modal.agent_access.as_mut() {
            list.apply_snapshot(&snapshot);
            // Rows carry agent-chosen text: drop the shaped text of earlier
            // reads so it cannot pile up while the list stays open.
            self.modal.command_palette_text_cache.clear();
            self.refresh_command_palette();
            changed = true;
        }
        // A confirmation shows exactly what the person is about to revoke:
        // when that entry changed or ended, the prompt says so.
        if let Some(DojoPromptUi::RevokeAccess(prompt)) = self.modal.dojo_prompt.as_mut() {
            match prompt.reconcile(&snapshot) {
                PromptReconcile::Unchanged => {}
                PromptReconcile::Changed => {
                    self.refresh_dojo_prompt();
                    changed = true;
                }
                PromptReconcile::Gone => {
                    self.close_dojo_prompt();
                    if let Some(list) = self.modal.agent_access.as_mut() {
                        list.mark_selection_stale(
                            "That access ended before you confirmed; nothing was revoked",
                        );
                    }
                    changed = true;
                }
            }
        }
        if self.access.record_snapshot(snapshot, waiting) {
            self.refresh_waiting_title();
            changed = true;
        }
        changed
    }

    /// The waiting indicator is in the title, which a modal owns while it
    /// shows; closing the modal returns to the terminal title.
    fn refresh_waiting_title(&mut self) {
        if !self.modal.input_modal_open() && self.modal.trusted_consent.is_none() {
            self.update_window_title();
        }
    }

    /// Opens a revoke prompt above whatever palette is open, unless another
    /// prompt or a trusted dialog already holds the Window.
    fn show_access_prompt(&mut self, prompt: RevokeAccessUi) -> bool {
        if self.modal.dojo_prompt.is_some()
            || self.modal.trusted_consent.is_some()
            || self.modal.session_picker.is_some()
        {
            eprintln!("splinterm agent access: prompt dropped while another one is open");
            return false;
        }
        self.show_dojo_prompt(DojoPromptUi::RevokeAccess(prompt));
        true
    }

    /// Sends a confirmed revoke prompt's command.
    pub(super) fn execute_access_prompt(&mut self, command: WindowAccessCommand) {
        if let (WindowAccessCommand::RevokeEntry { target }, Some(list)) =
            (&command, self.modal.agent_access.as_mut())
        {
            list.begin_revoke(target);
            self.refresh_command_palette();
        }
        self.send_access_command(command);
    }

    /// Keys for the access list. Text goes to its search field before this.
    pub(super) fn handle_agent_access_key(&mut self, keysym: Keysym) {
        let page = self.command_palette_page();
        let Some(list) = self.modal.agent_access.as_mut() else {
            return;
        };
        let changed = match keysym {
            Keysym::Up => list.move_selection(-1),
            Keysym::Down => list.move_selection(1),
            Keysym::Home => list.select_first(),
            Keysym::End => list.select_last(),
            Keysym::Page_Up => list.move_selection(-page),
            Keysym::Page_Down => list.move_selection(page),
            Keysym::Return | Keysym::KP_Enter => {
                self.activate_agent_access();
                return;
            }
            Keysym::Escape => {
                if list.query().is_empty() {
                    self.close_command_palette();
                    return;
                }
                list.clear_query()
            }
            _ => false,
        };
        if changed {
            self.refresh_command_palette();
        }
    }

    fn activate_agent_access(&mut self) {
        let Some(activation) = self
            .modal
            .agent_access
            .as_mut()
            .and_then(AgentAccessUi::activate)
        else {
            self.refresh_command_palette();
            return;
        };
        self.refresh_command_palette();
        match activation {
            AccessActivation::Revoke(target) => {
                self.send_access_command(WindowAccessCommand::RevokeEntry { target });
            }
            AccessActivation::Confirm(prompt) => {
                self.show_access_prompt(prompt);
            }
        }
    }

    /// Pointer input over the palette while it shows the access list: a
    /// press and release on one row selects it, and never revokes it; only
    /// Enter does. Whether anything changed.
    pub(super) fn handle_agent_access_pointer(
        &mut self,
        event: &PointerEvent,
        inside_panel: bool,
    ) -> bool {
        let Some(layout) = self.modal.command_palette_layout.as_ref() else {
            return false;
        };
        let target = command_palette_row_hit_test(layout, event.position);
        match event.kind {
            PointerEventKind::Press { button, .. } if button == BTN_LEFT => {
                if !inside_panel {
                    return self.close_command_palette();
                }
                // The entry, not the row: a refresh between press and release
                // can move another entry under the pointer.
                self.access.pressed = target.and_then(|index| {
                    self.modal
                        .agent_access
                        .as_ref()
                        .and_then(|list| list.row_key(index).cloned())
                });
                false
            }
            PointerEventKind::Release { button, .. } if button == BTN_LEFT => {
                let (Some(pressed), Some(index)) = (self.access.pressed.take(), target) else {
                    return false;
                };
                let Some(list) = self.modal.agent_access.as_mut() else {
                    return false;
                };
                if !list.release_click(&pressed, index) {
                    return false;
                }
                self.refresh_command_palette();
                true
            }
            PointerEventKind::Axis { vertical, .. } if !vertical.is_none() => {
                let Some((direction, count)) = self.modal.session_picker_wheel.push(
                    vertical.absolute,
                    vertical.discrete,
                    vertical.value120,
                    44,
                ) else {
                    return false;
                };
                let count = isize::try_from(count).unwrap_or(isize::MAX);
                let delta = match direction {
                    MouseAction::WheelUp => -count,
                    MouseAction::WheelDown => count,
                    _ => 0,
                };
                let changed = self
                    .modal
                    .agent_access
                    .as_mut()
                    .is_some_and(|list| list.move_selection(delta));
                if changed {
                    self.refresh_command_palette();
                }
                changed
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use splinterm_protocol::{AccessEntryKey, AccessEntryKind, RememberedStoreStatus};

    #[test]
    fn revoke_all_stays_available_until_a_read_shows_nothing_to_revoke() {
        let mut access = WindowAccess::default();
        assert!(access.revoke_all_available());
        let mut snapshot = AccessSnapshot::new(1, RememberedStoreStatus::Available);
        access.snapshot = Some(snapshot.clone());
        assert!(!access.revoke_all_available());
        let mut pending = crate::frontend::agent_access_test_entry(splinterm_core::LairId::new());
        pending.key = AccessEntryKey::Pending { request_id: 2 };
        pending.kind = AccessEntryKind::Pending;
        snapshot.entries.push(pending);
        access.snapshot = Some(snapshot);
        assert!(access.revoke_all_available());
    }

    #[test]
    fn held_enter_never_repeats_into_the_list_or_a_revoke_prompt() {
        assert!(access_repeat_consumed(true, Keysym::Return));
        assert!(access_repeat_consumed(true, Keysym::KP_Enter));
        assert!(!access_repeat_consumed(true, Keysym::Down));
        assert!(!access_repeat_consumed(false, Keysym::Return));
    }

    #[test]
    fn an_unavailable_inventory_forgets_the_last_read_and_its_waiting_count() {
        let mut access = WindowAccess::default();
        let snapshot = AccessSnapshot::new(1, RememberedStoreStatus::Available);
        assert!(
            access.record_snapshot(snapshot.clone(), 2),
            "waiting changed"
        );
        assert!(!access.record_snapshot(snapshot.clone(), 2));
        assert_eq!(access.waiting(), 2);
        assert!(!access.revoke_all_available(), "the read showed nothing");
        // Unavailable: no waiting indicator or mark outlives the read, and
        // Revoke all stays offered because nothing is known any more.
        assert!(access.record_unavailable("not authorized".into()));
        assert_eq!(access.waiting(), 0);
        assert!(access.snapshot.is_none());
        assert_eq!(access.unavailable.as_deref(), Some("not authorized"));
        assert!(access.revoke_all_available());
        assert!(!access.record_unavailable("still not".into()));
        assert!(!access.record_snapshot(snapshot, 0));
        assert_eq!(access.unavailable, None);
    }
}
