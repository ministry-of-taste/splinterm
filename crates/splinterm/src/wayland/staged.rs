//! A staged Window: one xdg toplevel that shows several phases in turn.
//!
//! [`super::run`] creates a Window, runs one phase in it, and destroys it. A
//! remote launch instead shows a profile list, then the connection status,
//! then that host's Dojos, and finally its terminal, all in one Window. Each
//! phase is an ordinary [`WindowOptions`]; [`run_window_on`] runs it on the
//! surface the previous phase left parked.
//!
//! The calloop event loop is not `Send` and the phases are driven from
//! different blocking tasks, so the first phase starts one dedicated Wayland
//! thread that owns the connection, globals, event loop, and Window for its
//! whole life. Each later phase sends its options to that thread and blocks
//! until the phase ends.
//!
//! Kept from one phase to the next: the platform state (globals, seats,
//! outputs), the surface state (xdg toplevel, viewport, fractional scale, SHM
//! pool, size, configure state, and scale), the keyboard, pointer, and their
//! seats and modifiers and focus, the clipboard devices and the compositor's
//! current selection offers, and which keys are held. Everything else is
//! rebuilt from the next phase's options, including terminal input, search,
//! selection, history, clipboard sources, and accessibility.
//!
//! Invariants, enforced by [`staged_phase_refusal`] and the switch:
//!
//! 1. A phase that holds a daemon endpoint binds the surface for good: it
//!    cannot keep the surface, so a Window never changes endpoint.
//! 2. The input method only goes from off to on. A phase that keeps the
//!    surface runs without it, the text-input manager is bound only when a
//!    phase that turns it on installs, and a switch that would turn it off
//!    again is refused.
//! 3. Parking and switching wipe pending terminal input and drop clipboard
//!    sources, selection, and search; keys pressed while parked are discarded,
//!    and their repeats are ignored in the next phase until released.

use std::{
    collections::HashSet,
    sync::mpsc::{self as std_mpsc, TryRecvError},
    thread,
};

use anyhow::{Context, Result};
use smithay_client_toolkit::{
    data_device_manager::{data_device::DataDevice, data_offer::SelectionOffer},
    primary_selection::{device::PrimarySelectionDevice, offer::PrimarySelectionOffer},
    reexports::calloop::ping::{Ping, PingSource, make_ping},
    seat::keyboard::Modifiers,
};
use tokio::sync::watch;
use wayland_client::protocol::{wl_keyboard, wl_pointer, wl_seat};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::ZwpTextInputV3;

use super::{
    App, ExitClass, PendingTerminalInput, PlatformState, SCALE_DENOMINATOR, SearchUiState,
    SurfaceState, WaylandSession, WindowOptions, assemble_app, bind_text_input_manager,
    diagnostics, initial_window_title, note_window_diagnostics, prepare_window, window_app_id,
};

/// How one phase of a staged Window ended.
#[derive(Debug)]
pub enum PhaseEnd {
    /// The phase's content ended and the Window waits, parked, for the next
    /// phase. Dropping the handle unmaps it.
    Kept(WindowSurface),
    /// The Window closed: the user or the compositor closed it, or a phase
    /// that could not keep it ended.
    Closed,
}

/// A parked staged Window, ready for its next phase.
///
/// Dropping it unmaps the Window and ends its Wayland thread.
pub struct WindowSurface {
    closed: watch::Receiver<bool>,
    commands: Option<std_mpsc::Sender<Box<WindowOptions>>>,
    wake: Ping,
    results: std_mpsc::Receiver<Result<StagedOutcome>>,
    thread: Option<thread::JoinHandle<()>>,
    /// Whether a phase on this surface has turned the input method on.
    input_method_bound: bool,
}

impl std::fmt::Debug for WindowSurface {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WindowSurface")
            .field("input_method_bound", &self.input_method_bound)
            .finish_non_exhaustive()
    }
}

/// Resolves once a parked Window has closed, whether the compositor or user
/// closed it or its Wayland thread ended. An owner awaiting other work while
/// the Window is parked races its work against this, so a close ends that work
/// (and any SSH connection it holds) at once.
#[derive(Clone, Debug)]
pub struct CloseSignal(watch::Receiver<bool>);

impl CloseSignal {
    /// Waits until the Window has closed.
    pub async fn closed(mut self) {
        let _ = self.0.wait_for(|closed| *closed).await;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StagedOutcome {
    Kept,
    Closed,
}

/// Runs one phase: on a new Window when `surface` is `None`, otherwise on the
/// parked Window `surface`.
///
/// # Errors
///
/// Returns an error when the phase may not run on a staged surface (see the
/// module invariants), or when the Window fails as [`super::run`] would. The
/// Window is gone after an error.
pub fn run_window_on(surface: Option<WindowSurface>, options: WindowOptions) -> Result<PhaseEnd> {
    if let Some(refusal) = staged_phase_refusal(
        surface.as_ref().map(|surface| surface.input_method_bound),
        &options,
    ) {
        anyhow::bail!(refusal);
    }
    let input_method = options.input_method;
    let mut surface = match surface {
        Some(mut surface) => {
            surface.send(options)?;
            surface
        }
        None => WindowSurface::start(options)?,
    };
    surface.input_method_bound |= input_method;
    surface.await_phase()
}

/// Why `options` may not run as a staged phase, if it may not.
/// `input_method_bound` is `None` for the first phase on a new Window.
fn staged_phase_refusal(
    input_method_bound: Option<bool>,
    options: &WindowOptions,
) -> Option<&'static str> {
    if options.trusted_consent.is_some() {
        return Some("a trusted dialog never shares its Window");
    }
    if options.keep_surface && options.session_picker.is_none() && !options.connection_status_screen
    {
        return Some("only a session picker or the connection status screen keeps its Window");
    }
    if options.keep_surface && options.binds_endpoint() {
        return Some("a phase with a daemon endpoint binds its Window and cannot keep it");
    }
    if options.keep_surface && options.input_method {
        return Some("a phase that keeps its Window runs without the input method");
    }
    if input_method_bound == Some(true) && !options.input_method {
        return Some("a Window cannot turn its input method off again");
    }
    None
}

/// Whether ending a phase this way parks a Window that keeps its surface.
/// Every other end, including a compositor or user close, closes it.
pub(super) const fn phase_end_parks(exit_class: ExitClass) -> bool {
    matches!(
        exit_class,
        // A session-picker decision, or Escape in a standalone picker.
        ExitClass::CleanSessionPickerDecision
        // The status Window's own Splint exited.
        | ExitClass::CleanFinalTabRemoved
    )
}

/// Whether a key event reaches the phase: not while the Window is parked, and
/// not a repeat of a key held over from before the phase began.
pub(super) fn key_repeat_reaches_phase(parked: bool, carried: &HashSet<u32>, raw: u32) -> bool {
    !parked && !carried.contains(&raw)
}

/// Wipes queued terminal input bytes, which may be a typed password, and
/// empties the queue.
pub(super) fn wipe_pending_terminal_input(
    pending: &mut std::collections::VecDeque<PendingTerminalInput>,
) {
    for batch in pending.iter_mut() {
        crate::remote_askpass::wipe(&mut batch.bytes);
    }
    pending.clear();
}

impl WindowOptions {
    /// Whether these options attach the Window to a daemon endpoint. The
    /// connection status screen's own snapshot channels are not one.
    pub(super) fn binds_endpoint(&self) -> bool {
        !self.panes.is_empty()
            || self.layout.is_some()
            || self.topology_updates.is_some()
            || self.topology_commands.is_some()
            || self.topology_settings.is_some()
            || self.initial_dojo.is_some()
            || self.remote_display_identity.is_some()
            || self.graphical_focus.is_some()
            || self.handoff_status.is_some()
            || self.layout_ledger.is_some()
            || self.local_endpoint
    }
}

impl WindowSurface {
    /// A signal that resolves once this parked Window closes.
    #[must_use]
    pub fn close_signal(&self) -> CloseSignal {
        CloseSignal(self.closed.clone())
    }

    fn start(options: WindowOptions) -> Result<Self> {
        let (wake, wake_source) = make_ping().context("create staged Window wake source")?;
        let (commands, command_receiver) = std_mpsc::channel();
        let (result_sender, results) = std_mpsc::channel();
        let (closed_sender, closed) = watch::channel(false);
        let thread = thread::Builder::new()
            .name("splinterm-wayland".to_owned())
            .spawn(move || {
                staged_thread(
                    options,
                    &command_receiver,
                    wake_source,
                    &result_sender,
                    &closed_sender,
                );
                // Also on every other way the thread ends.
                closed_sender.send_replace(true);
            })
            .context("start the Window's Wayland thread")?;
        Ok(Self {
            closed,
            commands: Some(commands),
            wake,
            results,
            thread: Some(thread),
            input_method_bound: false,
        })
    }

    fn send(&mut self, options: WindowOptions) -> Result<()> {
        self.commands
            .as_ref()
            .context("the staged Window has ended")?
            .send(Box::new(options))
            .map_err(|_| anyhow::anyhow!("the staged Window has ended"))?;
        self.wake.ping();
        Ok(())
    }

    fn await_phase(mut self) -> Result<PhaseEnd> {
        let outcome = self.results.recv();
        if matches!(outcome, Ok(Ok(StagedOutcome::Kept))) {
            return Ok(PhaseEnd::Kept(self));
        }
        let panicked = self.join();
        match outcome {
            Ok(Ok(_)) => Ok(PhaseEnd::Closed),
            Ok(Err(error)) => Err(error),
            Err(_) if panicked => Err(anyhow::anyhow!("the Window's Wayland thread panicked")),
            Err(_) => Err(anyhow::anyhow!("the Window's Wayland thread ended")),
        }
    }

    /// Ends the thread: a parked Window unmaps. True when it panicked.
    fn join(&mut self) -> bool {
        drop(self.commands.take());
        self.wake.ping();
        self.thread
            .take()
            .is_some_and(|thread| thread.join().is_err())
    }
}

impl Drop for WindowSurface {
    fn drop(&mut self) {
        self.join();
    }
}

/// What the Wayland thread keeps of a Window between phases.
struct KeptWindow {
    platform: PlatformState,
    surface: SurfaceState,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    keyboard_seat: Option<wl_seat::WlSeat>,
    pointer: Option<wl_pointer::WlPointer>,
    pointer_seat: Option<wl_seat::WlSeat>,
    text_input: Option<ZwpTextInputV3>,
    text_input_seat: Option<wl_seat::WlSeat>,
    modifiers: Modifiers,
    keyboard_focused: bool,
    last_pointer_serial: Option<u32>,
    held_keys: HashSet<u32>,
    data_device: Option<DataDevice>,
    primary_device: Option<PrimarySelectionDevice>,
    clipboard_offer: Option<SelectionOffer>,
    primary_offer: Option<PrimarySelectionOffer>,
    frame_pending: bool,
}

/// How a parked Window stopped waiting.
enum Parked {
    Next(Box<WindowOptions>),
    /// The handle was dropped: unmap.
    Released,
    /// The compositor or user closed it, or it failed.
    Closed(Result<()>),
}

impl App {
    /// Parks the Window after its phase ended with `exit_class`.
    fn park(&mut self, exit_class: ExitClass) {
        self.scheduling.parked = Some(exit_class);
        self.scheduling.exit = false;
        self.wipe_phase_input();
        // Nothing reaches the ended phase's owner any more.
        self.panes.pane.commands = None;
        self.panes.pane.updates = None;
        self.panes.pane.controller_active = false;
    }

    /// Wipes what the ended phase could still act on or reveal.
    fn wipe_phase_input(&mut self) {
        wipe_pending_terminal_input(&mut self.input.pending_terminal_input);
        self.input.terminal_input_saturated = false;
        self.input.prefix_state.clear();
        self.cancel_clipboard_image();
        self.clipboard.palette_image_target = None;
        self.clipboard.drag_target = None;
        self.clipboard.clipboard_sources.clear();
        self.clipboard.primary_sources.clear();
        self.panes.pane.selection = None;
        self.panes.pane.selected_text = None;
        self.panes.pane.selecting = false;
        self.panes.pane.search = SearchUiState::default();
    }

    fn into_kept(mut self) -> KeptWindow {
        self.wipe_phase_input();
        let held_keys = std::mem::take(&mut self.input.pressed_keys);
        let Self {
            platform,
            surface,
            input,
            clipboard,
            scheduling,
            ..
        } = self;
        KeptWindow {
            platform,
            surface,
            keyboard: input.keyboard,
            keyboard_seat: input.keyboard_seat,
            pointer: input.pointer,
            pointer_seat: input.pointer_seat,
            text_input: input.text_input,
            text_input_seat: input.text_input_seat,
            modifiers: input.modifiers,
            keyboard_focused: input.keyboard_focused,
            last_pointer_serial: input.last_pointer_serial,
            held_keys,
            data_device: clipboard.data_device,
            primary_device: clipboard.primary_device,
            clipboard_offer: clipboard.clipboard_offer,
            primary_offer: clipboard.primary_offer,
            frame_pending: scheduling.frame_pending,
        }
    }
}

impl WaylandSession {
    /// Builds the next phase's content on the kept Window.
    fn switch_app(&self, kept: KeptWindow, mut options: WindowOptions) -> Result<App> {
        note_window_diagnostics(&options);
        let prepared = prepare_window(&mut options)?;
        let KeptWindow {
            mut platform,
            mut surface,
            keyboard,
            keyboard_seat,
            pointer,
            pointer_seat,
            text_input,
            text_input_seat,
            modifiers,
            keyboard_focused,
            last_pointer_serial,
            held_keys,
            data_device,
            primary_device,
            clipboard_offer,
            primary_offer,
            frame_pending,
        } = kept;
        anyhow::ensure!(
            options.input_method || platform.text_input_manager.is_none(),
            "a Window cannot turn its input method off again"
        );
        if options.input_method && platform.text_input_manager.is_none() {
            platform.text_input_manager =
                bind_text_input_manager(&self.globals, &self.queue_handle);
        }
        surface.window.set_title(initial_window_title(&options));
        surface
            .window
            .set_app_id(window_app_id(false, &options.app_id).to_owned());
        surface.window.set_min_size(None);
        surface.window.set_max_size(None);
        surface.mapped_notify = options.mapped.take();
        surface
            .background_effect_state
            .set_requested_blur(options.theme.background_blur);
        surface
            .background_effect_state
            .set_background_alpha(options.theme.background_alpha);
        // The new content is laid out at the base scale; the kept scale is
        // applied to it below, as a compositor scale change would be.
        let scale_120 = std::mem::replace(&mut surface.scale_120, SCALE_DENOMINATOR);
        surface.buffers.clear();
        surface.backing.clear();
        let mut app = assemble_app(platform, surface, options, prepared, &self.update_waker)?;
        app.scheduling.frame_pending = frame_pending;
        app.input.keyboard = keyboard;
        app.input.keyboard_seat = keyboard_seat;
        app.input.pointer = pointer;
        app.input.pointer_seat = pointer_seat;
        app.input.text_input = text_input;
        app.input.text_input_seat = text_input_seat;
        app.input.modifiers = modifiers;
        app.input.last_pointer_serial = last_pointer_serial;
        app.input.pressed_keys.clone_from(&held_keys);
        app.input.carried_keys = held_keys;
        app.clipboard.data_device = data_device;
        app.clipboard.primary_device = primary_device;
        app.clipboard.clipboard_offer = clipboard_offer;
        app.clipboard.primary_offer = primary_offer;
        if app.input.text_input.is_none()
            && let (Some(manager), Some(seat)) = (
                &app.platform.text_input_manager,
                app.input.keyboard_seat.clone(),
            )
        {
            app.input.text_input =
                Some(manager.get_text_input(&seat, &self.queue_handle, app.input.ime_generation));
            app.input.text_input_seat = Some(seat);
        }
        app.set_ime_focus(keyboard_focused);
        if keyboard_focused
            && app.panes.input_modes().focus_reporting
            && !app.modal.input_modal_open()
        {
            app.request_active_pane_focus_report(true);
            app.input.terminal_focus_reported = true;
        }
        app.apply_scale(scale_120, &self.queue_handle)?;
        if let Some(output) = app.platform.entered_outputs.last().cloned() {
            app.refresh_output_dpi(&output, &self.queue_handle)?;
        }
        // The configure path: lay the new content out at the Window's size.
        if app.surface.configured {
            app.presentation.full_redraw = true;
            app.queue_background_effect_geometry_for_draw()?;
            if app.modal.inline_picker_open() {
                app.panes.restored_frontend_needs_resize = true;
            } else {
                app.emit_resize()?;
            }
            app.schedule_draw(&self.queue_handle)?;
        }
        Ok(app)
    }

    /// Keeps a parked Window's last frame and handles compositor events until
    /// the next phase arrives, the handle is dropped, or the Window closes.
    fn wait_parked(
        &mut self,
        app: &mut App,
        commands: &std_mpsc::Receiver<Box<WindowOptions>>,
    ) -> Parked {
        loop {
            match commands.try_recv() {
                Ok(options) => return Parked::Next(options),
                Err(TryRecvError::Disconnected) => return Parked::Released,
                Err(TryRecvError::Empty) => {}
            }
            if app.scheduling.exit {
                return Parked::Closed(Ok(()));
            }
            if let Err(error) = app.draw_if_pending(&self.queue_handle) {
                return Parked::Closed(Err(error));
            }
            if let Err(error) = self
                .event_loop
                .dispatch(None, app)
                .context("dispatch Wayland events")
            {
                return Parked::Closed(Err(error));
            }
        }
    }
}

/// Records the exit of a parked Window that its owner unmapped.
fn record_parked_exit(exit_class: ExitClass) {
    if let Some(diagnostics) = diagnostics() {
        diagnostics.request_exit(exit_class);
    }
}

/// The staged Window's Wayland thread: runs phases until the Window closes.
fn staged_thread(
    first: WindowOptions,
    commands: &std_mpsc::Receiver<Box<WindowOptions>>,
    wake_source: PingSource,
    results: &std_mpsc::Sender<Result<StagedOutcome>>,
    closed_signal: &watch::Sender<bool>,
) {
    let started = (|| -> Result<(WaylandSession, App)> {
        let mut options = first;
        note_window_diagnostics(&options);
        let prepared = prepare_window(&mut options)?;
        let session = WaylandSession::open()?;
        session
            .event_loop
            .handle()
            .insert_source(wake_source, |(), (), _| {})
            .map_err(|error| anyhow::anyhow!("register staged Window wake source: {error}"))?;
        let app = session.new_app(options, prepared)?;
        Ok((session, app))
    })();
    let (mut session, mut app) = match started {
        Ok(started) => started,
        Err(error) => {
            let _ = results.send(Err(error));
            return;
        }
    };
    loop {
        let phase = session.run_phase(&mut app);
        let parked = app
            .scheduling
            .parked
            .take()
            .filter(|_| phase.is_ok() && app.scheduling.failure.is_none());
        let Some(parked) = parked else {
            let closed = session.finish(Some(app), phase);
            let _ = results.send(closed.map(|()| StagedOutcome::Closed));
            return;
        };
        app.park(parked);
        if results.send(Ok(StagedOutcome::Kept)).is_err() {
            record_parked_exit(parked);
            let _ = session.finish(Some(app), Ok(()));
            return;
        }
        match session.wait_parked(&mut app, commands) {
            Parked::Next(options) => match session.switch_app(app.into_kept(), *options) {
                Ok(next) => app = next,
                Err(error) => {
                    let failed = session.finish(None, Err(error));
                    let _ = results.send(failed.map(|()| StagedOutcome::Closed));
                    return;
                }
            },
            Parked::Released => {
                record_parked_exit(parked);
                let _ = session.finish(Some(app), Ok(()));
                return;
            }
            Parked::Closed(result) => {
                // Unmap now; the next phase's caller learns of the close, and
                // its owner's close signal resolves when this thread ends.
                let closed = session.finish(Some(app), result);
                closed_signal.send_replace(true);
                if commands.recv().is_ok() {
                    let _ = results.send(closed.map(|()| StagedOutcome::Closed));
                }
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use tokio::sync::mpsc;

    use super::*;
    use crate::{WindowDojoIdentity, wayland::SchedulingState};

    fn scheduling(keep_surface: bool) -> SchedulingState {
        SchedulingState {
            signoff: None,
            graphical_input_probe: None,
            scroll_trace: false,
            exit: false,
            failure: None,
            frame_pending: false,
            redraw_pending: false,
            terminal_redraw_pending: false,
            next_commit_sequence: 0,
            pending_frame_trace: None,
            keep_surface,
            parked: None,
            busy_notice: false,
        }
    }

    #[test]
    fn only_a_picker_decision_or_the_status_exit_parks_a_kept_window() {
        for exit_class in [
            ExitClass::CleanSessionPickerDecision,
            ExitClass::CleanFinalTabRemoved,
        ] {
            let mut kept = scheduling(true);
            kept.request_exit(exit_class);
            assert!(kept.exit);
            assert_eq!(kept.parked, Some(exit_class));

            let mut plain = scheduling(false);
            plain.request_exit(exit_class);
            assert!(plain.exit);
            assert_eq!(plain.parked, None, "a plain Window closes");
        }
        for exit_class in [
            ExitClass::CleanCompositorClose,
            ExitClass::CleanUserClose,
            ExitClass::ErrorWaylandDispatch,
            ExitClass::ErrorPaneStream,
            ExitClass::ErrorTopologyManager,
        ] {
            let mut kept = scheduling(true);
            kept.request_exit(exit_class);
            assert!(kept.exit);
            assert_eq!(kept.parked, None, "{exit_class:?} closes a kept Window");
        }
        let mut failed = scheduling(true);
        failed.fail(anyhow::anyhow!("dispatch failed"));
        assert!(failed.exit && failed.parked.is_none() && failed.failure.is_some());
    }

    #[test]
    fn a_busy_topology_queue_drops_the_action_and_keeps_the_window() {
        let mut busy = scheduling(false);
        busy.fail(anyhow::Error::new(crate::wayland::TopologyQueueFull).context("activate a tab"));
        assert!(busy.busy_notice);
        assert!(!busy.exit && busy.failure.is_none() && busy.parked.is_none());

        let mut broken = scheduling(false);
        broken.fail(anyhow::anyhow!("topology command queue is closed"));
        assert!(broken.exit && broken.failure.is_some() && !broken.busy_notice);
    }

    fn identity() -> WindowDojoIdentity {
        WindowDojoIdentity {
            topology_revision: splinterm_core::TopologyRevision::new(0),
            lair_id: splinterm_core::LairId::new(),
            dojo_id: splinterm_core::DojoId::new(),
            lair_name: String::new(),
            lair_retention: splinterm_core::LairRetention::Disposable,
            dojo_name: String::new(),
        }
    }

    /// A status-screen phase: the explicit marker, no input method.
    fn pre_terminal() -> WindowOptions {
        WindowOptions {
            keep_surface: true,
            input_method: false,
            connection_status_screen: true,
            ..WindowOptions::default()
        }
    }

    fn picker_phase() -> WindowOptions {
        WindowOptions {
            session_picker: Some(crate::SessionPickerUi::new(
                Vec::new(),
                std_mpsc::channel().0,
            )),
            connection_status_screen: false,
            ..pre_terminal()
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one table covers every allowed and refused phase shape"
    )]
    fn staged_phases_follow_the_endpoint_and_input_method_invariants() {
        // A picker or status phase keeps the surface without the input method.
        assert_eq!(staged_phase_refusal(None, &pre_terminal()), None);
        assert_eq!(staged_phase_refusal(Some(false), &pre_terminal()), None);
        let (updates_sender, updates) = mpsc::channel(1);
        let (commands, _command_receiver) = mpsc::channel(1);
        let status = WindowOptions {
            updates: Some(updates),
            commands: Some(commands),
            ..pre_terminal()
        };
        assert!(!status.binds_endpoint(), "the status screen is no endpoint");
        assert_eq!(staged_phase_refusal(Some(false), &status), None);
        drop(updates_sender);
        assert_eq!(staged_phase_refusal(None, &picker_phase()), None);
        assert_eq!(staged_phase_refusal(Some(false), &picker_phase()), None);

        // Only a session picker or the marked status screen may keep it: a
        // live single-Splint Window (snapshot, updates, and commands, as an
        // attached Splint has) looks like the status screen but is refused.
        let (_live_updates, updates) = mpsc::channel(1);
        let (commands, _live_commands) = mpsc::channel(1);
        let live_splint = WindowOptions {
            snapshot: Some(
                crate::remote_status::RemoteStatus::new("work", "work.example")
                    .snapshot(splinterm_core::SplintId::new(), 1),
            ),
            updates: Some(updates),
            commands: Some(commands),
            controlled: true,
            connection_status_screen: false,
            ..pre_terminal()
        };
        assert!(!live_splint.binds_endpoint());
        assert!(
            staged_phase_refusal(Some(false), &live_splint)
                .is_some_and(|refusal| refusal.contains("only a session picker"))
        );
        let unmarked = WindowOptions {
            connection_status_screen: false,
            ..pre_terminal()
        };
        assert!(staged_phase_refusal(None, &unmarked).is_some());

        // The terminal phase binds the surface, with the input method on.
        let terminal = WindowOptions {
            initial_dojo: Some(identity()),
            local_endpoint: true,
            ..WindowOptions::default()
        };
        assert!(terminal.binds_endpoint());
        assert_eq!(staged_phase_refusal(Some(false), &terminal), None);

        // (1) An endpoint phase cannot keep the surface.
        for endpoint in [
            WindowOptions {
                initial_dojo: Some(identity()),
                ..pre_terminal()
            },
            WindowOptions {
                local_endpoint: true,
                ..pre_terminal()
            },
            WindowOptions {
                topology_commands: Some(mpsc::channel(1).0),
                ..pre_terminal()
            },
            WindowOptions {
                layout: Some(
                    splinterm_core::Lair::new("x", std::path::PathBuf::from("/")).dojos[0]
                        .root
                        .clone(),
                ),
                ..pre_terminal()
            },
        ] {
            assert!(
                staged_phase_refusal(Some(false), &endpoint)
                    .is_some_and(|refusal| refusal.contains("endpoint"))
            );
        }
        assert!(
            staged_phase_refusal(
                Some(false),
                &WindowOptions {
                    initial_dojo: Some(identity()),
                    ..picker_phase()
                }
            )
            .is_some_and(|refusal| refusal.contains("endpoint"))
        );
        // (2) A kept phase runs without the input method, and a bound input
        // method never turns off again.
        for composing in [
            WindowOptions {
                input_method: true,
                ..pre_terminal()
            },
            WindowOptions {
                input_method: true,
                ..picker_phase()
            },
        ] {
            assert!(
                staged_phase_refusal(None, &composing)
                    .is_some_and(|refusal| refusal.contains("input method"))
            );
        }
        let off_again = WindowOptions {
            input_method: false,
            ..WindowOptions::default()
        };
        assert!(staged_phase_refusal(Some(true), &off_again).is_some());
        assert_eq!(staged_phase_refusal(Some(false), &off_again), None);
    }

    #[test]
    fn a_trusted_dialog_never_runs_as_a_staged_phase() {
        let (decision, _answers) = std_mpsc::channel();
        let dialog = WindowOptions {
            trusted_consent: Some(crate::TrustedConsentUi::upgrade(decision)),
            input_method: false,
            ..WindowOptions::default()
        };
        assert!(staged_phase_refusal(None, &dialog).is_some());
    }

    #[test]
    fn parked_and_held_keys_never_reach_the_phase() {
        let carried = HashSet::from([36_u32]);
        assert!(key_repeat_reaches_phase(false, &carried, 9));
        assert!(!key_repeat_reaches_phase(false, &carried, 36));
        assert!(!key_repeat_reaches_phase(true, &HashSet::new(), 9));
    }

    #[test]
    fn parking_wipes_queued_terminal_input() {
        let (commands, _receiver) = mpsc::channel(1);
        let mut pending = VecDeque::from([
            PendingTerminalInput {
                commands: commands.clone(),
                bytes: b"hunter2".to_vec(),
            },
            PendingTerminalInput {
                commands,
                bytes: b"\r".to_vec(),
            },
        ]);
        wipe_pending_terminal_input(&mut pending);
        assert!(pending.is_empty());
    }
}
