use std::time::Instant;

use super::super::{
    ActionId, App, CommandPaletteShortcutAction, Connection, ExitClass, KeyEvent, KeyboardHandler,
    KeymapPress, Keysym, Modifiers, PaneFocusAction, PaneTopologyAction, PasteTarget, QueueHandle,
    RawModifiers, SessionPickerShortcutAction, TabShortcutAction, WaylandSurface, WindowCommand,
    WindowTopologyCommand, close_other_tabs_command, command_palette_shortcut_action,
    consume_detached_enter_press, font_zoom_action, key_repeat_reaches_phase,
    keymap_press_consumes_key, keymap_press_for, lair_lifecycle_command, pane_focus_action,
    pane_topology_action, session_picker_shortcut_action, shortcut_action_for,
    shortcut_paused_during_handoff, tab_action_dispatch_allowed, tab_shortcut_action, wl_keyboard,
    wl_surface,
};

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _connection: &Connection,
        queue_handle: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _serial: u32,
        _raw: &[u32],
        _keysyms: &[Keysym],
    ) {
        if surface == self.surface.window.wl_surface() {
            // A trusted dialog arms its delayed actions from focus: the
            // access consent dialog again on every focus gain.
            if let Some(consent) = self.modal.trusted_consent.as_mut() {
                consent.note_focus(Instant::now());
            }
            self.set_ime_focus(true);
            self.presentation.full_redraw = true;
            if self.panes.input_modes().focus_reporting && !self.modal.input_modal_open() {
                self.request_active_pane_focus_report(true);
                self.input.terminal_focus_reported = true;
            }
            if let Err(error) = self.schedule_draw(queue_handle) {
                self.scheduling.fail(error);
            }
        }
    }

    fn leave(
        &mut self,
        _connection: &Connection,
        queue_handle: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _serial: u32,
    ) {
        if surface == self.surface.window.wl_surface() {
            // Leaving releases every key without release events.
            self.input.pressed_keys.clear();
            self.input.prefix_state.clear();
            self.close_copy_mode();
            self.set_ime_focus(false);
            self.presentation.full_redraw = true;
            if self.panes.input_modes().focus_reporting && !self.modal.input_modal_open() {
                self.request_active_pane_focus_report(false);
                self.input.terminal_focus_reported = false;
            }
            if let Err(error) = self.schedule_draw(queue_handle) {
                self.scheduling.fail(error);
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "ordered modal, shortcut, IME, and terminal dispatch share one keyboard event boundary"
    )]
    fn press_key(
        &mut self,
        _connection: &Connection,
        queue_handle: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        serial: u32,
        event: KeyEvent,
    ) {
        self.input.pressed_keys.insert(event.raw_code);
        if self.scheduling.parked.is_some() {
            // A parked Window takes no input: its phase has ended.
            self.input.carried_keys.insert(event.raw_code);
            return;
        }
        // A new press is this phase's own, even of a key carried over before.
        self.input.carried_keys.remove(&event.raw_code);
        if self.modal.input_modal_open() {
            self.input.prefix_state.clear();
        }
        if self.modal.copy_mode.is_some() {
            self.modal
                .session_picker_consumed_keys
                .insert(event.raw_code);
            if let Err(error) = self.handle_copy_mode_key(&event, queue_handle, serial) {
                self.scheduling.fail(error);
            } else if self.presentation.full_redraw
                && let Err(error) = self.schedule_draw(queue_handle)
            {
                self.scheduling.fail(error);
            }
            return;
        }
        if self.handle_owned_field_key(&event, queue_handle, serial) {
            self.modal
                .session_picker_consumed_keys
                .insert(event.raw_code);
            if self.presentation.full_redraw
                && let Err(error) = self.schedule_draw(queue_handle)
            {
                self.scheduling.fail(error);
            }
            return;
        }
        if self.modal.command_palette.is_some()
            || self.modal.dojo_prompt.is_some()
            || self.modal.tab_context_menu.is_some()
        {
            self.modal
                .session_picker_consumed_keys
                .insert(event.raw_code);
            self.handle_key(&event, queue_handle);
            if self.presentation.full_redraw
                && let Err(error) = self.schedule_draw(queue_handle)
            {
                self.scheduling.fail(error);
            }
            return;
        }
        if self.modal.session_picker.is_some() || self.modal.trusted_consent.is_some() {
            self.modal
                .session_picker_consumed_keys
                .insert(event.raw_code);
            self.handle_key(&event, queue_handle);
            if (self.presentation.full_redraw || self.modal.session_picker_redraw)
                && let Err(error) = self.schedule_draw(queue_handle)
            {
                self.scheduling.fail(error);
            }
            return;
        }
        // With terminal search open, Enter submits the query even while the
        // pane is scrolled back; it returns to live only once search closes.
        if let Some(navigation) = consume_detached_enter_press(
            event.keysym,
            self.input.modifiers,
            !self.panes.pane.scrollback_viewport.is_live()
                && self.panes.pane.search.input.is_none(),
            event.raw_code,
            &mut self.modal.session_picker_consumed_keys,
        ) {
            self.input.prefix_state.clear();
            if let Err(error) = self.apply_history_navigation(navigation, queue_handle) {
                self.scheduling.fail(error);
            }
            return;
        }
        let prefix_was_armed = self.input.prefix_state.is_armed();
        let shortcut = match keymap_press_for(
            &self.input.keymap,
            &mut self.input.prefix_state,
            event.keysym,
            self.input.modifiers,
            event.raw_code,
            Instant::now(),
            self.input.prefix_timeout,
        ) {
            KeymapPress::PassThrough => None,
            KeymapPress::PrefixModifier => {
                self.modal
                    .session_picker_consumed_keys
                    .insert(event.raw_code);
                return;
            }
            KeymapPress::Consumed(None) => {
                self.modal
                    .session_picker_consumed_keys
                    .insert(event.raw_code);
                if prefix_was_armed {
                    eprintln!("splinterm keymap: unknown prefix sequence");
                }
                return;
            }
            KeymapPress::Consumed(Some(action)) => {
                if keymap_press_consumes_key(action, self.tab_state.managed_tabs) {
                    self.modal
                        .session_picker_consumed_keys
                        .insert(event.raw_code);
                }
                Some(action)
            }
        };
        // While splinterd upgrades, a shortcut that changes the session, takes
        // control, or types into the terminal is consumed without effect.
        if let Some(action) = shortcut
            && self.handoff_paused()
            && shortcut_paused_during_handoff(action)
        {
            self.modal
                .session_picker_consumed_keys
                .insert(event.raw_code);
            return;
        }
        if let Some(action) = command_palette_shortcut_action(
            shortcut,
            false,
            self.tab_state.managed_tabs,
            !self.command_palette_available(),
        ) {
            self.modal
                .session_picker_consumed_keys
                .insert(event.raw_code);
            if action == CommandPaletteShortcutAction::Open {
                if self.show_command_palette().is_err() {
                    eprintln!("splinterm command palette unavailable");
                } else if let Err(error) = self.schedule_draw(queue_handle) {
                    self.scheduling.fail(error);
                }
            }
            return;
        }
        if let Some(action) = tab_shortcut_action(shortcut, false, self.tab_state.managed_tabs) {
            self.modal
                .session_picker_consumed_keys
                .insert(event.raw_code);
            if !tab_action_dispatch_allowed([
                self.modal.session_picker.is_some(),
                self.modal.trusted_consent.is_some(),
                self.modal.session_picker_requested,
                self.tab_state.session_switch_pending,
                self.modal.session_picker_reconcile_pending,
            ]) {
                return;
            }
            let target =
                match action {
                    TabShortcutAction::Next => self
                        .tab_state
                        .tabs
                        .next()
                        .map(|dojo_id| WindowTopologyCommand::ActivateTab { dojo_id }),
                    TabShortcutAction::Previous => self
                        .tab_state
                        .tabs
                        .previous()
                        .map(|dojo_id| WindowTopologyCommand::ActivateTab { dojo_id }),
                    TabShortcutAction::NewDojo => match self.focused_cwd_source() {
                        Ok(source_splint_id) => Some(WindowTopologyCommand::NewDojo {
                            lair_id: self.tab_state.active_identity.lair_id,
                            source_splint_id,
                        }),
                        Err(error) => {
                            self.scheduling.fail(error);
                            None
                        }
                    },
                    TabShortcutAction::Close => Some(WindowTopologyCommand::CloseTab {
                        dojo_id: self.tab_state.active_dojo_id(),
                    }),
                    TabShortcutAction::CloseOthers => {
                        let retain_dojo_id = self.tab_state.active_dojo_id();
                        let dojo_ids = self
                            .tab_state
                            .tabs
                            .iter()
                            .map(|tab| tab.dojo_id)
                            .filter(|dojo_id| *dojo_id != retain_dojo_id)
                            .collect::<Vec<_>>();
                        close_other_tabs_command(retain_dojo_id, dojo_ids)
                    }
                    TabShortcutAction::Select(index) => self.tab_state.tabs.at(index).map(|tab| {
                        WindowTopologyCommand::ActivateTab {
                            dojo_id: tab.dojo_id,
                        }
                    }),
                    TabShortcutAction::Move(delta) => {
                        if self.tab_state.tabs.move_active(delta) {
                            self.presentation.full_redraw = true;
                            if let Err(error) = self.schedule_draw(queue_handle) {
                                self.scheduling.fail(error);
                            }
                        }
                        return;
                    }
                    TabShortcutAction::Rename => {
                        self.show_current_dojo_prompt(false);
                        if let Err(error) = self.schedule_draw(queue_handle) {
                            self.scheduling.fail(error);
                        }
                        return;
                    }
                    TabShortcutAction::Terminate => {
                        self.show_current_dojo_prompt(true);
                        if let Err(error) = self.schedule_draw(queue_handle) {
                            self.scheduling.fail(error);
                        }
                        return;
                    }
                    TabShortcutAction::Choose => {
                        self.modal.session_picker_requested = true;
                        let command = WindowTopologyCommand::RequestSelector {
                            kind: super::super::SelectorKind::Dojo,
                            lair_id: self.tab_state.active_identity.lair_id,
                        };
                        if let Err(error) = self.send_topology_command(command) {
                            self.modal.session_picker_requested = false;
                            self.scheduling.fail(error);
                        }
                        return;
                    }
                    TabShortcutAction::Consume => None,
                };
            if let Some(target) = target
                && let Err(error) = self.send_topology_command(target)
            {
                self.scheduling.fail(error);
            }
            return;
        }
        if let Some(action) = session_picker_shortcut_action(
            shortcut,
            false,
            self.modal.session_picker.is_some()
                || self.modal.trusted_consent.is_some()
                || self.modal.session_picker_requested
                || self.tab_state.session_switch_pending
                || self.modal.session_picker_reconcile_pending
                || self.modal.command_palette_reconcile_pending,
        ) {
            if action == SessionPickerShortcutAction::Request {
                if self.tab_state.topology_commands.is_some() {
                    self.modal.session_picker_requested = true;
                    if self
                        .send_topology_command(WindowTopologyCommand::RequestSessionPicker)
                        .is_err()
                    {
                        self.modal.session_picker_requested = false;
                        eprintln!("splinterm Dojo picker request failed");
                    }
                } else {
                    eprintln!("splinterm Dojo picker is unavailable for this attachment");
                }
            }
            return;
        }
        if matches!(
            shortcut,
            Some(
                ActionId::NewSession
                    | ActionId::RenameCurrentLair
                    | ActionId::SaveCurrentLair
                    | ActionId::ToggleCurrentLairPin
                    | ActionId::PreviewCurrentLair
                    | ActionId::RestoreCurrentLair
                    | ActionId::TerminateCurrentLair
                    | ActionId::PreviousLair
                    | ActionId::NextLair
                    | ActionId::LairChooser
            )
        ) {
            if !tab_action_dispatch_allowed([
                self.modal.session_picker.is_some(),
                self.modal.trusted_consent.is_some(),
                self.modal.session_picker_requested,
                self.tab_state.session_switch_pending,
                self.modal.session_picker_reconcile_pending,
            ]) {
                return;
            }
            let command = match shortcut.expect("matched Lair action") {
                ActionId::NewSession => {
                    self.request_new_lair();
                    return;
                }
                ActionId::RenameCurrentLair => {
                    self.modal.session_picker_requested = true;
                    WindowTopologyCommand::RequestLairPrompt {
                        lair_id: self.tab_state.active_identity.lair_id,
                        kind: super::super::LairPromptKind::Rename,
                        expected_retention: None,
                    }
                }
                action @ (ActionId::SaveCurrentLair
                | ActionId::ToggleCurrentLairPin
                | ActionId::PreviewCurrentLair
                | ActionId::RestoreCurrentLair) => {
                    let Some(command) = lair_lifecycle_command(
                        action,
                        self.tab_state.active_identity.lair_id,
                        self.tab_state.active_identity.lair_retention,
                    ) else {
                        return;
                    };
                    if matches!(command, WindowTopologyCommand::RequestLairPrompt { .. }) {
                        self.modal.session_picker_requested = true;
                    }
                    command
                }
                ActionId::TerminateCurrentLair => {
                    self.modal.session_picker_requested = true;
                    WindowTopologyCommand::RequestLairPrompt {
                        lair_id: self.tab_state.active_identity.lair_id,
                        kind: super::super::LairPromptKind::Terminate,
                        expected_retention: None,
                    }
                }
                ActionId::PreviousLair => WindowTopologyCommand::NavigateLair {
                    current_lair_id: self.tab_state.active_identity.lair_id,
                    direction: super::super::LairDirection::Previous,
                },
                ActionId::NextLair => WindowTopologyCommand::NavigateLair {
                    current_lair_id: self.tab_state.active_identity.lair_id,
                    direction: super::super::LairDirection::Next,
                },
                ActionId::LairChooser => {
                    self.modal.session_picker_requested = true;
                    WindowTopologyCommand::RequestSelector {
                        kind: super::super::SelectorKind::Lair,
                        lair_id: self.tab_state.active_identity.lair_id,
                    }
                }
                _ => unreachable!("matched Lair action is exhaustive"),
            };
            if let Err(error) = self.send_topology_command(command) {
                self.modal.session_picker_requested = false;
                self.scheduling.fail(error);
            }
            return;
        }
        match shortcut {
            Some(ActionId::DetachWindow) => {
                self.scheduling.request_exit(ExitClass::CleanUserClose);
                return;
            }
            Some(ActionId::BindingHelp) => {
                if self.show_binding_help(queue_handle).is_err() {
                    eprintln!("splinterm key binding help unavailable");
                } else if let Err(error) = self.schedule_draw(queue_handle) {
                    self.scheduling.fail(error);
                }
                return;
            }
            Some(ActionId::CopyModeEnter) => {
                if self.enter_copy_mode()
                    && let Err(error) = self.schedule_draw(queue_handle)
                {
                    self.scheduling.fail(error);
                }
                return;
            }
            Some(ActionId::ConfigReload) => {
                self.reload_keymap_configuration();
                // A reloaded agent status or chrome text size draws now.
                if self.presentation.full_redraw
                    && self.surface.configured
                    && let Err(error) = self.schedule_draw(queue_handle)
                {
                    self.scheduling.fail(error);
                }
                return;
            }
            Some(ActionId::SendPrefix) => {
                self.send_command(WindowCommand::Input(vec![0]));
                return;
            }
            Some(ActionId::TogglePaneZoom) => {
                if self.toggle_pane_zoom()
                    && let Err(error) = self.schedule_draw(queue_handle)
                {
                    self.scheduling.fail(error);
                }
                return;
            }
            Some(ActionId::ToggleTabStrip) => {
                match self.toggle_tab_strip() {
                    Ok(true) if self.surface.configured => {
                        if let Err(error) = self.schedule_draw(queue_handle) {
                            self.scheduling.fail(error);
                        }
                    }
                    Ok(_) => {}
                    Err(error) => self.scheduling.fail(error),
                }
                return;
            }
            _ => {}
        }
        if self.tab_state.topology_commands.is_some()
            && let Some(action) = pane_topology_action(shortcut)
        {
            if let Some(target) = self.panes.focused_splint() {
                let dojo_id = self.tab_state.active_dojo_id();
                let mut pending_started = false;
                let command = match action {
                    PaneTopologyAction::Split(axis) => {
                        if !self.admit_split(target, axis) {
                            return;
                        }
                        let pending = if self.input.optimistic_remote_splits {
                            match self.begin_pending_remote_split(target, axis) {
                                Ok(Some(pending)) => {
                                    pending_started = true;
                                    Some(pending)
                                }
                                Ok(None) => return,
                                Err(error) => {
                                    self.scheduling.fail(error);
                                    return;
                                }
                            }
                        } else {
                            None
                        };
                        WindowTopologyCommand::Split {
                            dojo_id,
                            target,
                            axis,
                            pending,
                        }
                    }
                    PaneTopologyAction::Close => WindowTopologyCommand::Close { dojo_id, target },
                    PaneTopologyAction::AdjustRatio(delta) => WindowTopologyCommand::AdjustRatio {
                        dojo_id,
                        target,
                        delta,
                    },
                    PaneTopologyAction::ResizeCells(direction, cells) => {
                        match self.directional_resize_command(direction, cells) {
                            Ok(Some(command)) => command,
                            Ok(None) => return,
                            Err(error) => {
                                self.scheduling.fail(error);
                                return;
                            }
                        }
                    }
                };
                if let Err(error) = self.send_topology_command(command) {
                    self.scheduling.fail(error);
                } else if pending_started && let Err(error) = self.schedule_draw(queue_handle) {
                    self.scheduling.fail(error);
                }
            }
            return;
        }
        if let Some(action) = pane_focus_action(shortcut) {
            let changed = match action {
                PaneFocusAction::Direction(direction) => self.focus_direction(direction),
            };
            if changed {
                self.update_ime_cursor_rectangle();
                if let Err(error) = self.schedule_draw(queue_handle) {
                    self.scheduling.fail(error);
                }
            }
            return;
        }
        if let Some(action) = font_zoom_action(shortcut) {
            if let Err(error) = self.apply_font_zoom(action, queue_handle) {
                self.scheduling.fail(error);
            }
            return;
        }
        if shortcut == Some(ActionId::ClipboardCopy) {
            self.publish_clipboard(queue_handle, serial, false);
        } else if shortcut == Some(ActionId::ClipboardSaveImage) {
            self.modal
                .session_picker_consumed_keys
                .insert(event.raw_code);
            self.begin_clipboard_image();
        } else if shortcut == Some(ActionId::ClipboardPaste) {
            self.begin_clipboard_read(PasteTarget::Clipboard);
        } else {
            match self.handle_history_key(&event, queue_handle) {
                Ok(true) => {}
                Ok(false) => {
                    self.handle_key(&event, queue_handle);
                    if self.presentation.full_redraw
                        && let Err(error) = self.schedule_draw(queue_handle)
                    {
                        self.scheduling.fail(error);
                    }
                }
                Err(error) => self.scheduling.fail(error),
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "ordered modal, shortcut, and terminal repeat dispatch share one keyboard event boundary"
    )]
    fn repeat_key(
        &mut self,
        _connection: &Connection,
        queue_handle: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        serial: u32,
        event: KeyEvent,
    ) {
        if !key_repeat_reaches_phase(
            self.scheduling.parked.is_some(),
            &self.input.carried_keys,
            event.raw_code,
        ) {
            return;
        }
        if self.modal.copy_mode.is_some() {
            if let Err(error) = self.handle_copy_mode_key(&event, queue_handle, serial) {
                self.scheduling.fail(error);
            }
            return;
        }
        if self.modal_repeat_consumed(event.keysym) {
            return;
        }
        if self.owned_field_consumes_repeat(&event) {
            return;
        }
        if self.handle_owned_field_key(&event, queue_handle, serial) {
            if self.presentation.full_redraw
                && let Err(error) = self.schedule_draw(queue_handle)
            {
                self.scheduling.fail(error);
            }
            return;
        }
        if self.modal.command_palette.is_some()
            || self.modal.dojo_prompt.is_some()
            || self.modal.tab_context_menu.is_some()
        {
            self.handle_key(&event, queue_handle);
            if self.presentation.full_redraw
                && let Err(error) = self.schedule_draw(queue_handle)
            {
                self.scheduling.fail(error);
            }
            return;
        }
        if self.modal.session_picker.is_some() {
            self.handle_key(&event, queue_handle);
            if (self.presentation.full_redraw || self.modal.session_picker_redraw)
                && let Err(error) = self.schedule_draw(queue_handle)
            {
                self.scheduling.fail(error);
            }
            return;
        }
        if self
            .modal
            .session_picker_consumed_keys
            .contains(&event.raw_code)
        {
            return;
        }
        let shortcut = shortcut_action_for(&self.input.keymap, &event, self.input.modifiers);
        if command_palette_shortcut_action(
            shortcut,
            true,
            self.tab_state.managed_tabs,
            !self.command_palette_available(),
        )
        .is_some()
        {
            self.modal
                .session_picker_consumed_keys
                .insert(event.raw_code);
            return;
        }
        if tab_shortcut_action(shortcut, true, self.tab_state.managed_tabs).is_some() {
            self.modal
                .session_picker_consumed_keys
                .insert(event.raw_code);
            return;
        }
        if session_picker_shortcut_action(
            shortcut,
            true,
            self.modal.session_picker.is_some()
                || self.modal.trusted_consent.is_some()
                || self.modal.session_picker_requested
                || self.tab_state.session_switch_pending
                || self.modal.session_picker_reconcile_pending
                || self.modal.command_palette_reconcile_pending,
        )
        .is_some()
        {
            return;
        }
        if let Some(consent) = &self.modal.trusted_consent {
            // Trusted dialogs ignore repeats, including those of a key
            // already held when it gained focus.
            if !consent.kind.ignores_repeats() {
                self.handle_key(&event, queue_handle);
            }
            return;
        }
        if let Some(action) = font_zoom_action(shortcut) {
            if let Err(error) = self.apply_font_zoom(action, queue_handle) {
                self.scheduling.fail(error);
            }
            return;
        }
        match self.handle_history_key(&event, queue_handle) {
            Ok(true) => {}
            Ok(false) => self.handle_key(&event, queue_handle),
            Err(error) => self.scheduling.fail(error),
        }
    }

    fn release_key(
        &mut self,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        self.input.pressed_keys.remove(&event.raw_code);
        self.input.carried_keys.remove(&event.raw_code);
        self.modal
            .session_picker_consumed_keys
            .remove(&event.raw_code);
    }

    fn update_modifiers(
        &mut self,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        modifiers: Modifiers,
        _raw_modifiers: RawModifiers,
        _layout: u32,
    ) {
        self.input.modifiers = modifiers;
    }
}
