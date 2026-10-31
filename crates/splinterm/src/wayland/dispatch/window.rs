use splinterm_protocol::perf_trace::{
    PerfTraceEvent, emit_perf_trace, emit_perf_trace_at, monotonic_raw_ns,
};

use super::super::{
    App, CompositorHandler, Connection, ExitClass, QueueHandle, SCALE_DENOMINATOR,
    TerminalResizeCause, WaylandSurface, Window, WindowConfigure, WindowHandler,
    WindowTopologyCommand, configure_resize_cause, note_output_enter, note_output_leave,
    terminal_resize_allowed, viewport_destination, wl_output, wl_surface,
};
use crate::wayland::chrome::bounded_fixed_size;

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _connection: &Connection,
        queue_handle: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        factor: i32,
    ) {
        if surface != self.surface.window.wl_surface() || factor <= 0 {
            return;
        }
        let Ok(integer_scale) = u32::try_from(factor) else {
            self.scheduling.fail(anyhow::anyhow!(
                "integer output scale does not fit u32: {factor}"
            ));
            return;
        };
        self.surface.integer_fallback_scale = integer_scale;
        if self.surface.fractional_scale.is_none() {
            let scale_120 = integer_scale.saturating_mul(SCALE_DENOMINATOR);
            if let Err(error) = self.apply_scale(scale_120, queue_handle) {
                self.scheduling.fail(error);
            }
        }
    }

    fn transform_changed(
        &mut self,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _connection: &Connection,
        queue_handle: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        if surface == self.surface.window.wl_surface() {
            self.scheduling.frame_pending = false;
            if let Some(trace) = self.scheduling.pending_frame_trace.take() {
                let callback_monotonic_raw_ns = monotonic_raw_ns();
                emit_perf_trace_at(
                    "splinterm",
                    "frame_callback",
                    PerfTraceEvent {
                        commit_sequence: Some(trace.commit_sequence),
                        duration_ns: Some(
                            callback_monotonic_raw_ns
                                .saturating_sub(trace.committed_monotonic_raw_ns),
                        ),
                        ..PerfTraceEvent::default()
                    },
                    callback_monotonic_raw_ns,
                );
            }
            if self.scheduling.redraw_pending
                && let Err(error) = self.draw(queue_handle)
            {
                self.scheduling.fail(error);
            }
        }
    }

    fn surface_enter(
        &mut self,
        _connection: &Connection,
        queue_handle: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        output: &wl_output::WlOutput,
    ) {
        if surface != self.surface.window.wl_surface() {
            return;
        }
        note_output_enter(&mut self.platform.entered_outputs, output);
        self.platform.output_count = self.platform.entered_outputs.len();
        emit_perf_trace(
            "splinterm",
            "window_event",
            PerfTraceEvent {
                output_enter_events: Some(1),
                ..PerfTraceEvent::default()
            },
        );
        if let Err(error) = self.refresh_output_dpi(output, queue_handle) {
            self.scheduling.fail(error);
        }
    }

    fn surface_leave(
        &mut self,
        _connection: &Connection,
        queue_handle: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        output: &wl_output::WlOutput,
    ) {
        if surface != self.surface.window.wl_surface() {
            return;
        }
        let was_most_recent = note_output_leave(&mut self.platform.entered_outputs, output);
        self.platform.output_count = self.platform.entered_outputs.len();
        emit_perf_trace(
            "splinterm",
            "window_event",
            PerfTraceEvent {
                output_leave_events: Some(1),
                ..PerfTraceEvent::default()
            },
        );
        // With no entered output, retain the last observation as Foot does
        // while temporarily unmapped. Otherwise promote the previous output.
        if was_most_recent
            && let Some(current) = self.platform.entered_outputs.last().cloned()
            && let Err(error) = self.refresh_output_dpi(&current, queue_handle)
        {
            self.scheduling.fail(error);
        }
    }
}

impl WindowHandler for App {
    fn request_close(
        &mut self,
        _connection: &Connection,
        _queue_handle: &QueueHandle<Self>,
        _window: &Window,
    ) {
        if self.modal.trusted_consent.is_some() {
            self.decide_consent(false);
        } else {
            // Best effort: the manager treats a missing notice as a detach.
            if let Some(commands) = &self.tab_state.topology_commands {
                let _ = commands.try_send(WindowTopologyCommand::CloseWindow);
            }
            self.scheduling
                .request_exit(ExitClass::CleanCompositorClose);
        }
    }

    fn configure(
        &mut self,
        _connection: &Connection,
        queue_handle: &QueueHandle<Self>,
        _window: &Window,
        configure: WindowConfigure,
        _serial: u32,
    ) {
        let old_width = self.surface.logical_width;
        let old_height = self.surface.logical_height;
        // A trusted dialog that chooses its own size stays within the
        // compositor's suggested bounds, as its fixed size does.
        let (own_width, own_height) = if self.modal.trusted_consent.is_some() {
            self.modal.trusted_layout.bounds = configure.suggested_bounds;
            bounded_fixed_size((old_width, old_height), configure.suggested_bounds)
        } else {
            (old_width, old_height)
        };
        let width = configure
            .new_size
            .0
            .map_or(own_width, std::num::NonZeroU32::get);
        let height = configure
            .new_size
            .1
            .map_or(own_height, std::num::NonZeroU32::get);
        emit_perf_trace(
            "splinterm",
            "window_event",
            PerfTraceEvent {
                configure_count: Some(1),
                old_width: Some(u64::from(old_width)),
                old_height: Some(u64::from(old_height)),
                final_width: Some(u64::from(width)),
                final_height: Some(u64::from(height)),
                ..PerfTraceEvent::default()
            },
        );
        let resized = (width, height) != (old_width, old_height);
        if resized {
            self.surface.logical_width = width;
            self.surface.logical_height = height;
            self.modal.session_picker_layout = None;
            self.modal.command_palette_layout = None;
            self.modal.dojo_prompt_layout = None;
            self.modal.tab_context_menu_layout = None;
            if let Some(viewport) = &self.surface.viewport {
                match viewport_destination(width, height) {
                    Ok((width, height)) => viewport.set_destination(width, height),
                    Err(error) => {
                        self.scheduling.fail(error);
                        return;
                    }
                }
            }
            self.surface.buffers.clear();
            self.presentation.full_redraw = true;
            self.update_ime_cursor_rectangle();
        }
        let initial_configure = !self.surface.configured;
        self.surface.configured = true;
        // Plan 0072 M2b: the opening grace runs from the first configure.
        let since_opened = self.surface.configured_at.map(|at| at.elapsed());
        if initial_configure {
            self.surface.configured_at = Some(std::time::Instant::now());
        }
        if initial_configure || resized {
            debug_assert!(terminal_resize_allowed(
                TerminalResizeCause::SurfaceConfigure,
                self.panes.pane.last_resize.is_some(),
            ));
            // Plan 0072 M2b: a person's resize of the focused Window takes
            // control of its panes. One the inline picker defers does not.
            let cause = configure_resize_cause(since_opened, resized, self.input.keyboard_focused);
            let resize = if self.modal.inline_picker_open() {
                self.panes.restored_frontend_needs_resize = true;
                Ok(())
            } else {
                self.emit_resize_for(cause)
            };
            if let Err(error) = self
                .queue_background_effect_geometry_for_draw()
                .and(resize)
                .and_then(|()| self.schedule_draw(queue_handle))
            {
                self.scheduling.fail(error);
            }
        }
    }
}
