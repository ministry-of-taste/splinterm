use super::super::{
    App, BTN_LEFT, Connection, ModalPointerFrame, MouseAction, MouseTracking, PasteTarget,
    PointerEvent, PointerEventKind, PointerHandler, PressOwner, QueueHandle, WaylandSurface,
    WindowCommand, application_motion, classify_press, history_return_to_live_hit,
    local_selection_owner, mouse_report, pending_selection_drag_anchor, pointer_axis_focus_target,
    take_press_owner, url_at, wl_pointer,
};

impl PointerHandler for App {
    #[allow(
        clippy::too_many_lines,
        reason = "one Wayland pointer frame preserves event order across selection, mouse reporting, paste, and URL gestures"
    )]
    fn pointer_frame(
        &mut self,
        _connection: &Connection,
        queue_handle: &QueueHandle<Self>,
        _pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        if self.scheduling.parked.is_some() {
            return;
        }
        let modal_frame = ModalPointerFrame::new(self.modal.input_modal_open());
        let mut picker_changed = false;
        let mut explorer_changed = false;
        let mut pane_focus_changed = false;
        let mut pane_divider_changed = false;
        for event in events {
            if &event.surface != self.surface.window.wl_surface() {
                continue;
            }
            if modal_frame.owns_event(self.modal.input_modal_open()) {
                // A prompt can stand above the palette (a revoke from the
                // access list); the topmost layer takes the pointer.
                picker_changed |= if self.modal.dojo_prompt.is_some() {
                    self.handle_dojo_prompt_pointer(event)
                } else if self.modal.command_palette.is_some() {
                    self.handle_command_palette_pointer(event, queue_handle)
                } else if self.modal.tab_context_menu.is_some() {
                    self.handle_tab_context_menu_pointer(event)
                } else {
                    self.handle_session_picker_pointer(event)
                };
                continue;
            }
            if self.handle_lair_explorer_pointer(event, queue_handle) {
                explorer_changed = true;
                continue;
            }
            match self.handle_pane_divider_pointer(event) {
                Ok(true) => {
                    pane_divider_changed = true;
                    continue;
                }
                Ok(false) => {}
                Err(error) => {
                    self.scheduling.fail(error);
                    return;
                }
            }
            match self.handle_tab_strip_pointer(event) {
                Ok(true) => {
                    picker_changed |= self.modal.input_modal_open();
                    continue;
                }
                Ok(false) => {}
                Err(error) => {
                    self.scheduling.fail(error);
                    return;
                }
            }
            let previous_hover = self.panes.pane.hovered_url.clone();
            let mut cell = self.pointer_cell_at(event.position);
            match event.kind {
                PointerEventKind::Enter { serial } => {
                    self.input.last_pointer_serial = Some(serial);
                    self.panes.pane.pointer_cell = cell;
                }
                PointerEventKind::Leave { .. } => {
                    self.panes.pane.pointer_cell = None;
                    self.panes.pane.hovered_url = None;
                }
                PointerEventKind::Motion { .. } => {
                    self.panes.pane.pointer_cell = cell;
                    if let Some(position) = cell
                        && let Some(anchor) = self
                            .input
                            .pressed_buttons
                            .get(&BTN_LEFT)
                            .and_then(|owner| pending_selection_drag_anchor(owner, position))
                    {
                        self.input
                            .pressed_buttons
                            .insert(BTN_LEFT, PressOwner::Selection);
                        self.begin_selection(anchor);
                    }
                    self.panes.pane.hovered_url = if self.panes.pane.selecting {
                        None
                    } else {
                        cell.and_then(|position| {
                            let display = self.panes.display_snapshot_cow()?;
                            url_at(&display, position)
                        })
                    };
                    if self.panes.pane.selecting {
                        if let Some(position) = cell {
                            self.extend_selection(position);
                        }
                    } else if let Some(position) = cell {
                        let active_press = self
                            .input
                            .pressed_buttons
                            .values()
                            .find_map(application_motion);
                        let report = if let Some((code, sgr, modifiers)) = active_press {
                            mouse_report(MouseAction::Motion(code), position, modifiers, sgr)
                        } else {
                            let modes = self.panes.input_modes();
                            (modes.mouse_tracking == MouseTracking::Any)
                                .then(|| {
                                    mouse_report(
                                        MouseAction::Motion(3),
                                        position,
                                        self.input.modifiers,
                                        modes.sgr_mouse,
                                    )
                                })
                                .flatten()
                        };
                        if let Some(report) = report {
                            self.send_coalescible_input(report);
                        }
                    }
                }
                PointerEventKind::Press { button, serial, .. } => {
                    self.input.last_pointer_serial = Some(serial);
                    if button == BTN_LEFT
                        && self
                            .modal
                            .session_picker
                            .as_ref()
                            .is_some_and(|picker| !picker.is_inline())
                    {
                        let decision = cell.and_then(|position| {
                            self.modal
                                .session_picker
                                .as_mut()
                                .and_then(|picker| picker.select_row(position.row))
                        });
                        if let Some(decision) = decision {
                            self.decide_session_picker(decision);
                        }
                        continue;
                    }
                    if let Some(splint_id) = self.splint_at_point(event.position)
                        && self.focus_splint(splint_id)
                    {
                        cell = self.pointer_cell_at(event.position);
                        self.panes.pane.pointer_cell = cell;
                        self.update_ime_cursor_rectangle();
                        if let Err(error) = self.schedule_draw(queue_handle) {
                            self.scheduling.fail(error);
                            return;
                        }
                    }
                    if button == BTN_LEFT
                        && history_return_to_live_hit(
                            event.position,
                            self.panes.pane.painted_history,
                            self.surface.scale_120,
                            !self.panes.pane.scrollback_viewport.is_live(),
                        )
                    {
                        if let Err(error) = self
                            .scroll_history(MouseAction::WheelDown, usize::MAX)
                            .and_then(|moved| {
                                if moved && self.surface.configured {
                                    self.schedule_draw(queue_handle)?;
                                }
                                Ok(())
                            })
                        {
                            self.scheduling.fail(error);
                        }
                        continue;
                    }
                    if self.modal.trusted_consent.is_some() && button == BTN_LEFT {
                        let (x, y) = event.position;
                        // Only a click inside the painted green button can
                        // say yes; one inside the red button says no.
                        let on_button = self.trusted_consent_button_at(x, y);
                        let Some(consent) = self.modal.trusted_consent.as_mut() else {
                            continue;
                        };
                        let now = std::time::Instant::now();
                        // A click inside is the only thing that engages a
                        // trusted dialog's keyboard; focus never does.
                        consent.engage(now);
                        // The buttons answer; a click on a duration's row
                        // only selects it.
                        let input = if let Some(proceed) = on_button {
                            consent.action_area_input(proceed, now)
                        } else {
                            match cell {
                                Some(position) => consent.row_input(position.row, now),
                                // Outside every cell: nothing to answer or
                                // select, but the legend may need redrawing.
                                None => consent.engagement_redraw(),
                            }
                        };
                        self.trusted_dialog_input(input, queue_handle);
                        continue;
                    }
                    self.panes.pane.pointer_cell = cell;
                    self.recompute_hovered_url();
                    let mut owner = classify_press(
                        button,
                        cell.is_some(),
                        self.input.modifiers,
                        self.panes.input_modes(),
                        self.panes.pane.hovered_url.is_some(),
                    );
                    match owner {
                        PressOwner::Application {
                            code,
                            tracking: _,
                            sgr,
                            modifiers,
                        } => {
                            if let Some(position) = cell
                                && let Some(report) =
                                    mouse_report(MouseAction::Press(code), position, modifiers, sgr)
                            {
                                self.send_command(WindowCommand::Input(report));
                            }
                        }
                        PressOwner::Selection => {
                            if let Some(position) = cell {
                                owner = local_selection_owner(
                                    self.panes.pane.selection.is_some(),
                                    position,
                                );
                                if matches!(owner, PressOwner::SelectionPending { .. }) {
                                    self.clear_selection();
                                } else {
                                    self.begin_selection(position);
                                }
                            }
                        }
                        PressOwner::PrimaryPaste => {
                            self.begin_clipboard_read(PasteTarget::Primary);
                        }
                        PressOwner::Url => self.open_hovered_url(),
                        PressOwner::SelectionPending { .. } | PressOwner::Ignored => {}
                    }
                    self.input.pressed_buttons.insert(button, owner);
                }
                PointerEventKind::Release { button, serial, .. } => {
                    self.input.last_pointer_serial = Some(serial);
                    match take_press_owner(&mut self.input.pressed_buttons, button) {
                        PressOwner::Application {
                            code,
                            tracking: _,
                            sgr,
                            modifiers,
                        } => {
                            if let Some(position) = cell.or(self.panes.pane.pointer_cell)
                                && let Some(report) = mouse_report(
                                    MouseAction::Release(code),
                                    position,
                                    modifiers,
                                    sgr,
                                )
                            {
                                self.send_command(WindowCommand::Input(report));
                            }
                        }
                        PressOwner::Selection => {
                            self.finish_selection();
                            self.publish_clipboard(queue_handle, serial, true);
                        }
                        PressOwner::SelectionPending { .. }
                        | PressOwner::PrimaryPaste
                        | PressOwner::Url
                        | PressOwner::Ignored => {}
                    }
                }
                PointerEventKind::Axis {
                    horizontal: _,
                    vertical,
                    ..
                } => {
                    // Xterm's mouse protocol has only vertical wheel button codes 4/5;
                    // Foot does not synthesize horizontal wheel reports into unrelated buttons.
                    if vertical.is_none() {
                        continue;
                    }
                    if let Some(splint_id) = pointer_axis_focus_target(
                        true,
                        !self.input.pressed_buttons.is_empty(),
                        self.splint_at_point(event.position),
                        self.panes.focused_splint(),
                    ) && self.focus_splint(splint_id)
                    {
                        cell = self.pointer_cell_at(event.position);
                        self.panes.pane.pointer_cell = cell;
                        self.update_ime_cursor_rectangle();
                        pane_focus_changed = true;
                    }
                    if let Err(error) = self.handle_vertical_wheel(
                        cell,
                        vertical.absolute,
                        vertical.discrete,
                        vertical.value120,
                    ) {
                        self.scheduling.fail(error);
                    }
                }
            }
            if previous_hover != self.panes.pane.hovered_url {
                if let Some((start, _, _)) = previous_hover {
                    self.dirty_row(start.row);
                }
                if let Some((start, _, _)) = &self.panes.pane.hovered_url {
                    self.dirty_row(start.row);
                }
            }
        }
        if self.surface.configured
            && (picker_changed
                && (self.modal.input_modal_open()
                    || self.modal.session_picker_reconcile_pending
                    || self.modal.command_palette_reconcile_pending)
                || !self.modal.session_picker_reconcile_pending
                    && (pane_focus_changed
                        || pane_divider_changed
                        || explorer_changed
                        || self.panes.pane.viewport_dirty
                        || self.panes.pane.raster_dirty_rows.iter().any(|dirty| *dirty)
                        || self
                            .panes
                            .pane
                            .surface_dirty_rows
                            .iter()
                            .any(|dirty| *dirty)))
            && let Err(error) = self.schedule_draw(queue_handle)
        {
            self.scheduling.fail(error);
        }
    }
}
