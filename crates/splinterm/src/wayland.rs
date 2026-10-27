//! Native Wayland xdg-shell and shared-memory lifecycle for the graphical client.
//!
//! Foot 1.27.0 `wayland.c`, `shm.c`, and `render.c` at commit
//! `3c5b584b0eafa772eb4376fb6eaf6643399e190e` are the behavioral reference.
//! The client owns these objects; the daemon remains headless.

mod navigation_accessibility;

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet, VecDeque},
    os::fd::OwnedFd,
    path::{Path, PathBuf},
    pin::Pin,
    process::Command,
    sync::{
        Arc,
        mpsc::{self as std_mpsc, Receiver as StdReceiver, Sender as StdSender},
    },
    task::{Context as TaskContext, Poll, Wake, Waker},
    time::{Duration, Instant},
};

use tokio::sync::mpsc::{Receiver, Sender, error::TrySendError};
use unicode_width::UnicodeWidthChar;

use anyhow::{Context, Result};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    data_device_manager::{
        DataDeviceManagerState, WritePipe,
        data_device::{DataDevice, DataDeviceHandler},
        data_offer::{DataOfferHandler, DragOffer, SelectionOffer},
        data_source::{CopyPasteSource, DataSourceHandler},
    },
    delegate_compositor, delegate_data_device, delegate_keyboard, delegate_output,
    delegate_pointer, delegate_primary_selection, delegate_registry, delegate_seat, delegate_shm,
    delegate_xdg_shell, delegate_xdg_window,
    output::{OutputHandler, OutputState},
    primary_selection::{
        PrimarySelectionManagerState,
        device::{PrimarySelectionDevice, PrimarySelectionDeviceHandler},
        offer::PrimarySelectionOffer,
        selection::{PrimarySelectionSource, PrimarySelectionSourceHandler},
    },
    reexports::{
        calloop::{
            EventLoop, LoopHandle,
            ping::{Ping, make_ping},
        },
        calloop_wayland_source::WaylandSource,
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{
            BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, PointerEvent, PointerEventKind, PointerHandler,
        },
    },
    shell::{
        WaylandSurface,
        xdg::{
            XdgShell,
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
        },
    },
    shm::{
        Shm, ShmHandler,
        slot::{Buffer, SlotPool},
    },
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
    globals::{GlobalList, registry_queue_init},
    protocol::{
        wl_data_device, wl_data_device_manager::DndAction, wl_data_source, wl_keyboard, wl_output,
        wl_pointer, wl_seat, wl_shm, wl_surface,
    },
};

use wayland_protocols::ext::background_effect::v1::client::{
    ext_background_effect_manager_v1::{
        Capability as BackgroundCapability, Event as BackgroundManagerEvent,
        ExtBackgroundEffectManagerV1,
    },
    ext_background_effect_surface_v1::{self, ExtBackgroundEffectSurfaceV1},
};

use splinterm_automation_client::ImageContentLeaseSet;
use splinterm_core::{
    Axis, DojoId, LairId, LairRetention, LayoutNode, Splint, SplintId, SplitRatio, TopologyRevision,
};
use splinterm_protocol::{
    ActiveScreen, ControlTransferDecision, MouseTracking, SearchMatch, SearchPage, TerminalCell,
    TerminalInputModes, TerminalRow, TerminalSnapshot,
    perf_trace::{
        PerfTraceEvent, emit_perf_trace, emit_perf_trace_at, monotonic_raw_ns, perf_trace_enabled,
    },
};
#[cfg(test)]
use splinterm_protocol::{HistoryTransition, TerminalUpdate};

use smithay_client_toolkit::reexports::protocols::wp::{
    fractional_scale::v1::client::{
        wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        wp_fractional_scale_v1::{self, WpFractionalScaleV1},
    },
    primary_selection::zv1::client::{
        zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1,
        zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1,
    },
    text_input::zv3::client::{
        zwp_text_input_manager_v3::ZwpTextInputManagerV3,
        zwp_text_input_v3::{self, ZwpTextInputV3},
    },
    viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
};

use crate::background_effect::{
    BackgroundEffectState, CommitReason as BackgroundCommitReason, EffectAction, EffectDiagnostic,
};
use crate::config::{
    CursorStyle, FrameTitleMode, PaneDividerStyle, ResolvedTheme, omarchy_terminal_point_size,
    resolve_ui_font_size,
};
use crate::diagnostics::{
    DiagnosticErrorCode, DiagnosticEventCode, DiagnosticLevel, ExitClass, global as diagnostics,
};
use crate::frontend::{
    AgentAccessUi, AuthorityStatus, BINDING_HELP_PAGE_ITEMS, BindingHelpUi, BoundedTextEditor,
    BuiltInCommandDispatch, BuiltInCommandId, CommandControlAction, CommandHistoryAction,
    CommandPaletteContext, CommandPaletteUi, CommandTabMoveAvailability, CommandZoomAction,
    DojoPromptUi, FontUpdate, LairDirection, LairExplorerDecision, LairExplorerFilter,
    LairExplorerRow, LairExplorerUi, LairPromptKind, PerfTraceCorrelation, SelectorKind,
    SessionPickerCatalog, SessionPickerCreationTarget, SessionPickerDecision, SessionPickerTarget,
    SessionPickerUi, TabContextMenuUi, TabMenuActionId, TabMenuContext, TabMenuDispatch,
    TabMenuRightPress, TerminalGridLimits, TerminationDecision, ThemeUpdate, TrustedConsentUi,
    TrustedDialogKey, TrustedInput, WindowCommand, WindowDojoIdentity, WindowOptions,
    WindowPaneOptions, WindowThemeColors, WindowTopologyCommand, WindowTopologyUpdate,
    WindowUpdate, close_other_tabs_command, lair_lifecycle_command, tab_menu_right_press,
};
use crate::geometry::{
    FontSize, OutputDpiObservation, Rect, SurfaceGeometry, WindowGeometry, buffer_to_logical_ceil,
    logical_extent_to_buffer,
};
use crate::navigation_projection::{NavigationAction, NavigationNodeId};
#[cfg(test)]
use crate::pane::PaneDivider;
use crate::pane::{
    FocusDirection, LayoutFit, PaneChrome, PaneLayout, PaneResizeMetrics, PaneSplit,
    directional_resize_ratio, split_fits, split_ratio_at, split_ratio_mut,
};
use crate::renderer::{
    ChromeSource, ChromeSurface, ChromeText, ChromeTextStyle, CommandPaletteLayout,
    CommandPaletteTextCache, CursorPresentation, DojoPromptLayout, HistoryOverlayStatus,
    LairExplorerAction, LairExplorerLayout, LairExplorerMetrics, LairExplorerPresentationMode,
    PaletteBody, PickerHitTarget, RenderContext, SessionPickerOverlayLayout, SessionPickerPurpose,
    SessionPickerTextCache, SessionPickerTextItem, SnapshotFrame, SnapshotOverlays,
    TabContextMenuLayout, TextRow, background_bgra, chrome_panel_fill, chrome_panel_palette,
    clear_snapshot_caches, command_palette_hit_test, command_palette_layout, dojo_prompt_hit_test,
    dojo_prompt_layout, explorer_minimum_docked_terminal_width, fill_rect, history_overlay_layout,
    lair_explorer_action_hit_test, lair_explorer_agent_hit_test,
    lair_explorer_agents_label_hit_test, lair_explorer_chrome_cell_width,
    lair_explorer_disclosure_hit_test, lair_explorer_filter_hit_test,
    lair_explorer_footer_hit_test, lair_explorer_hit_test, lair_explorer_shown_layout,
    lair_explorer_with_agents, paint, paint_command_palette, paint_dojo_prompt,
    paint_handoff_banner, paint_history_overlay, paint_lair_explorer, paint_search_overlay,
    paint_session_picker_overlay, paint_snapshot_overlays, paint_snapshot_presented,
    paint_snapshot_region_presented, paint_snapshot_rows_presented, paint_tab_context_menu,
    premultiplied_theme_rgba, scroll_snapshot_pixels, search_overlay_panel,
    session_picker_hit_test, session_picker_overlay_layout, snapshot_row_rect,
    tab_context_menu_hit_test, tab_context_menu_layout, write_ppm,
};
#[cfg(test)]
use crate::renderer::{lair_explorer_layout, paint_box_drawing_cell};
use crate::{
    keymap::{ActionId, KeymapPress, PrefixState, ResolvedKeymap},
    tab::{DojoTab, WindowTabSet, sanitized_tab_label},
    viewport::ScrollbackViewport,
};

mod agent_access;
mod agent_status;
mod chrome;
mod clipboard;
mod clipboard_image;
mod damage;
mod dispatch;
mod file_drop;
mod handoff;
mod input;
mod selection;
mod staged;
mod tabs;
mod terminal_state;
#[cfg(test)]
use chrome::divider_junction;
use chrome::{
    TrustedConsentColors, frame_title_maximum_cells, paint_pane_chrome,
    paint_trusted_consent_chrome, sanitize_frame_title, trusted_dialog_theme,
};
pub use clipboard::encode_bracketed_paste;
use clipboard::{
    ACTIVE_CLIPBOARD_WORKERS, CLIPBOARD_IO_TIMEOUT, ClipboardRead, OwnedFieldTarget, PasteTarget,
    TEXT_MIMES, accepted_text_mime, safe_paste, spawn_clipboard_read, try_clipboard_worker,
    write_clipboard_with_deadline,
};
use damage::{
    BackingDamage, pending_draw_waits_for_frame, search_panel_damage, sync_backing_damage,
    take_full_surface_damage, terminal_draw_waits_for_frame,
};
use file_drop::{
    FileDropTarget, URI_LIST_MIME, accepted_uri_list_mime, copy_action_supported,
    dropped_file_payload, file_drop_target_is_current, pane_drop_target,
};
use handoff::{HandoffFrontend, HeldFailure};
use input::{
    CommandPaletteShortcutAction, CopyModeDesktopAction, FontZoomAction, HistoryNavigation,
    ModalPointerFrame, MouseAction, PaneFocusAction, PaneTopologyAction, PickerImeReconcile,
    PressOwner, SessionPickerShortcutAction, TabShortcutAction, WheelAccumulator, WheelOutcome,
    application_motion, binding_help_repeat_consumed, classify_press, clipboard_read_is_current,
    command_palette_shortcut_action, consume_detached_enter_press, copy_mode_desktop_action,
    font_zoom_action, history_overlay_status, history_return_to_live_hit, key_input,
    keymap_press_consumes_key, keymap_press_for, local_selection_owner, mouse_report,
    owned_field_clipboard_action, pane_focus_action, pane_topology_action,
    pending_selection_drag_anchor, picker_ime_reconcile, picker_release_activation,
    pointer_axis_focus_target, reconciled_focus_report, session_picker_shortcut_action,
    shortcut_action_for, shortcut_paused_during_handoff, tab_action_dispatch_allowed,
    tab_shortcut_action, take_press_owner,
};
use selection::{
    CellPosition, CopyModeState, CopyMotion, CopyMoveOutcome, Selection, SelectionEndpoint,
    copy_mode_enter, copy_mode_is_valid, move_copy_cursor, selection_display_bounds,
    selection_endpoint, selection_is_retained, selection_text, transient_overlay_rows, url_at,
};
pub use staged::{CloseSignal, PhaseEnd, WindowSurface, run_window_on};
use staged::{key_repeat_reaches_phase, phase_end_parks};
#[cfg(test)]
use tabs::DEFAULT_TAB_STRIP_LOGICAL_HEIGHT;
use tabs::{
    DojoTabView, TabHitTarget, TabsState, tab_context_target, tab_strip_hit_test,
    tab_strip_logical_height, tab_text_maximum_cell_height,
};
#[cfg(test)]
use terminal_state::snapshot_is_newer;
use terminal_state::{
    MAX_CACHED_HISTORY_BYTES, MAX_CACHED_HISTORY_ROWS, apply_terminal_update_for_viewport,
    bound_history_page_with_pins, changed_terminal_patch_rows, discard_lagging_history,
    history_cache_bytes, omitted_rows_before_cache, retain_history_for_viewport, snapshot_replaces,
    terminal_update_changes_visible_content, terminal_update_full_frame_reasons,
};
#[cfg(test)]
use terminal_state::{
    apply_scrollback_update, apply_terminal_update, blank_row, coalesce_snapshots,
    terminal_update_requires_full_frame,
};

const INITIAL_WIDTH: u32 = 960;
const INITIAL_HEIGHT: u32 = 600;
const MAX_DEFERRED_TOPOLOGY_UPDATES: usize = 16;
// Keep application mouse reports at one report per wheel step. Local history
// follows Foot's default three-lines-per-step semantic distance; visual motion
// must be smoothed in pixels rather than by increasing this row multiplier.
const SCROLLBACK_WHEEL_MULTIPLIER: f64 = 3.0;
const SCALE_DENOMINATOR: u32 = 120;
const MIN_SCALE_120: u32 = 120;
const MAX_SCALE_120: u32 = 960;
const MAX_PREEDIT_BYTES: usize = 4 * 1024;
const MAX_PENDING_TERMINAL_INPUT_BYTES: usize =
    clipboard::MAX_CLIPBOARD_BYTES + clipboard::BRACKETED_PASTE_OVERHEAD;
const PENDING_INPUT_RETRY_INTERVAL: Duration = Duration::from_millis(5);
const SIGNOFF_TICK_INTERVAL: Duration = Duration::from_millis(50);
const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(500);
const IDLE_EVENT_LOOP_TIMEOUT: Duration = Duration::from_secs(60);
const RECEIVER_DRAIN_BUDGET: usize = 8;
const MAX_SHM_BUFFERS: usize = 2;
const MAX_WINDOW_PRESENTATION_BYTES: usize = 512 * 1024 * 1024;

fn translated_rect(mut rect: Rect, origin: Rect) -> Rect {
    rect.x = rect.x.saturating_add(origin.x);
    rect.y = rect.y.saturating_add(origin.y);
    rect
}

fn translate_picker_layout(
    mut layout: SessionPickerOverlayLayout,
    origin: Rect,
) -> SessionPickerOverlayLayout {
    layout.panel = translated_rect(layout.panel, origin);
    layout.header = translated_rect(layout.header, origin);
    layout.action = translated_rect(layout.action, origin);
    layout.list = translated_rect(layout.list, origin);
    layout.footer = translated_rect(layout.footer, origin);
    for row in &mut layout.rows {
        row.rect = translated_rect(row.rect, origin);
        row.surface = translated_rect(row.surface, origin);
        row.title_clip = translated_rect(row.title_clip, origin);
        row.metadata_clip = translated_rect(row.metadata_clip, origin);
    }
    layout
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FinalTabRemovalAction {
    Continue,
    Exit,
    ExitAndHandoffPicker,
}

const fn final_tab_removal_action(
    tab_removed: bool,
    remaining_tabs: usize,
    picker_requested: bool,
) -> FinalTabRemovalAction {
    if !tab_removed || remaining_tabs != 0 {
        FinalTabRemovalAction::Continue
    } else if picker_requested {
        FinalTabRemovalAction::ExitAndHandoffPicker
    } else {
        FinalTabRemovalAction::Exit
    }
}

/// Picks the tab to show once `closing` closes: the set's usual neighbour when
/// `ready` holds for it, or else the first other tab for which it holds. When
/// no other tab is ready the usual neighbour is returned, so activating it
/// reports the problem.
fn tab_after_close<T>(
    tabs: &WindowTabSet<T>,
    closing: DojoId,
    ready: impl Fn(&DojoTab<T>) -> bool,
) -> Option<DojoId> {
    let usual = tabs.selection_after_close(closing)?;
    if tabs.get(usual).is_some_and(&ready) {
        return Some(usual);
    }
    tabs.iter()
        .find(|tab| tab.dojo_id != closing && ready(tab))
        .map(|tab| tab.dojo_id)
        .or(Some(usual))
}

fn session_picker_handoff_command(executable: &Path) -> Command {
    let mut command = Command::new(executable);
    command.arg("dojos");
    command
}

fn spawn_session_picker_handoff() -> Result<()> {
    let executable = std::env::current_exe().context("locate the running Splinterm client")?;
    crate::child_spawn::spawn(|| session_picker_handoff_command(&executable).spawn())
        .context("launch the Recent Dojos picker after final-tab removal")?;
    Ok(())
}

fn rect_contains(rect: Rect, position: (f64, f64)) -> bool {
    position.0 >= f64::from(rect.x)
        && position.1 >= f64::from(rect.y)
        && position.0 < f64::from(rect.x.saturating_add(rect.width))
        && position.1 < f64::from(rect.y.saturating_add(rect.height))
}

struct UpdateWake(Ping);

impl Wake for UpdateWake {
    fn wake(self: Arc<Self>) {
        self.0.ping();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.ping();
    }
}

enum ReceiverPoll<T> {
    Item(T),
    Pending,
    Disconnected,
}

struct ReceiverDrain<T> {
    items: Vec<T>,
    disconnected: bool,
}

fn poll_receiver<T>(receiver: &mut Receiver<T>, waker: &Waker) -> ReceiverPoll<T> {
    let mut context = TaskContext::from_waker(waker);
    match Pin::new(receiver).poll_recv(&mut context) {
        Poll::Ready(Some(item)) => ReceiverPoll::Item(item),
        Poll::Ready(None) => ReceiverPoll::Disconnected,
        Poll::Pending => ReceiverPoll::Pending,
    }
}

fn drain_receiver<T>(receiver: &mut Receiver<T>, waker: &Waker) -> ReceiverDrain<T> {
    let mut items = Vec::with_capacity(RECEIVER_DRAIN_BUDGET);
    for _ in 0..RECEIVER_DRAIN_BUDGET {
        match poll_receiver(receiver, waker) {
            ReceiverPoll::Item(item) => items.push(item),
            ReceiverPoll::Pending => {
                return ReceiverDrain {
                    items,
                    disconnected: false,
                };
            }
            ReceiverPoll::Disconnected => {
                return ReceiverDrain {
                    items,
                    disconnected: true,
                };
            }
        }
    }
    if receiver.is_closed() {
        // A closed producer cannot refill the queue, so preserve the previous
        // drain-before-disconnect behavior without risking starvation.
        while let Ok(item) = receiver.try_recv() {
            items.push(item);
        }
        return ReceiverDrain {
            items,
            disconnected: true,
        };
    }
    // Yield to drawing and Wayland dispatch even when a producer refills the
    // bounded channel as quickly as items are consumed. Re-arm this loop so
    // the retained backlog is handled without waiting for the periodic tick.
    waker.wake_by_ref();
    ReceiverDrain {
        items,
        disconnected: false,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SignoffStep {
    WaitHistory,
    LoadSelectionWindow,
    BeginSelection,
    ExtendSelection,
    WaitSelectedOutput,
    FinishSelection,
    LocalWheel,
    LoadClientCache,
    WaitMouseTracking,
    ApplicationWheel,
    ReturnLive,
    Complete,
}

struct SignoffProbe {
    report_path: PathBuf,
    started_at: Instant,
    step: SignoffStep,
    selection_revision: u64,
    cache_window: Option<(usize, u64)>,
    evidence: Vec<serde_json::Value>,
}

impl SignoffProbe {
    fn from_environment(development_bypass: bool) -> Result<Option<Self>> {
        let Some(path) = std::env::var_os("SPLINTERM_SIGNOFF_REPORT") else {
            return Ok(None);
        };
        anyhow::ensure!(
            development_bypass,
            "SPLINTERM_SIGNOFF_REPORT requires SPLINTERM_ENABLE_DEV_ATTACH=1"
        );
        anyhow::ensure!(!path.is_empty(), "SPLINTERM_SIGNOFF_REPORT is empty");
        Ok(Some(Self {
            report_path: PathBuf::from(path),
            started_at: Instant::now(),
            step: SignoffStep::WaitHistory,
            selection_revision: 0,
            cache_window: None,
            evidence: Vec::new(),
        }))
    }
}

struct GraphicalInputProbe {
    target_revisions: usize,
    observed_revisions: HashSet<(SplintId, u64, u64)>,
}

impl GraphicalInputProbe {
    fn from_environment(development_bypass: bool) -> Result<Option<Self>> {
        let Some(value) = std::env::var_os("SPLINTERM_GRAPHICAL_INPUT_AFTER_COMMITS") else {
            return Ok(None);
        };
        anyhow::ensure!(
            development_bypass,
            "SPLINTERM_GRAPHICAL_INPUT_AFTER_COMMITS requires SPLINTERM_ENABLE_DEV_ATTACH=1"
        );
        let value = value
            .to_str()
            .context("SPLINTERM_GRAPHICAL_INPUT_AFTER_COMMITS must be UTF-8")?;
        let remaining_revisions = value
            .parse::<usize>()
            .context("SPLINTERM_GRAPHICAL_INPUT_AFTER_COMMITS must be a positive integer")?;
        anyhow::ensure!(
            (1..=1024).contains(&remaining_revisions),
            "SPLINTERM_GRAPHICAL_INPUT_AFTER_COMMITS must be between 1 and 1024"
        );
        Ok(Some(Self {
            target_revisions: remaining_revisions,
            observed_revisions: HashSet::with_capacity(remaining_revisions),
        }))
    }

    fn observe_commit(&mut self, identity: Option<(SplintId, u64, u64)>) -> bool {
        let Some(identity) = identity else {
            return false;
        };
        self.observed_revisions.insert(identity);
        self.observed_revisions.len() >= self.target_revisions
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
enum PendingPreedit {
    #[default]
    Unchanged,
    Clear,
    Set(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ImeBatch {
    preedit: PendingPreedit,
    commit: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ImeState {
    entered: bool,
    focused: bool,
    sent_commit_serial: u32,
    visible_preedit: Option<String>,
    pending: ImeBatch,
}

impl ImeState {
    fn set_preedit(&mut self, text: Option<String>) {
        self.pending.preedit = match text {
            Some(text) if text.len() <= MAX_PREEDIT_BYTES => PendingPreedit::Set(text),
            Some(_) => PendingPreedit::Unchanged,
            None => PendingPreedit::Clear,
        };
    }

    fn set_commit(&mut self, text: Option<String>) {
        self.pending.commit = text.filter(|text| text.len() <= MAX_PREEDIT_BYTES);
    }

    fn note_client_commit(&mut self) {
        self.sent_commit_serial = self.sent_commit_serial.wrapping_add(1);
    }

    fn finish(&mut self, serial: u32) -> (bool, Option<String>, Option<String>) {
        let serial_matches = serial == self.sent_commit_serial;
        let commit = self.pending.commit.take();
        match std::mem::take(&mut self.pending.preedit) {
            PendingPreedit::Unchanged if commit.is_some() => self.visible_preedit = None,
            PendingPreedit::Unchanged => {}
            PendingPreedit::Clear => self.visible_preedit = None,
            PendingPreedit::Set(preedit) => self.visible_preedit = Some(preedit),
        }
        (serial_matches, self.visible_preedit.clone(), commit)
    }

    fn composing(&self) -> bool {
        self.visible_preedit.is_some() || matches!(self.pending.preedit, PendingPreedit::Set(_))
    }

    fn clear_composition(&mut self) {
        self.visible_preedit = None;
        self.pending = ImeBatch::default();
    }
}

fn pane_stream_has_terminal_notice(updates: &[WindowUpdate]) -> bool {
    updates
        .iter()
        .any(|update| matches!(update, WindowUpdate::Exited { .. } | WindowUpdate::Shutdown))
}

/// Closes exited Splints unless the Window is paused for a daemon handoff
/// (Plan 0061 M3.12a): a fenced daemon refuses the close, and a refused close
/// is never retried, so the pending set waits for the resume instead. Returns
/// false only when the topology queue closed.
fn enqueue_exited_splints_unless_paused(
    handoff_paused: bool,
    dojo_id: DojoId,
    lair_retention: LairRetention,
    pending: &mut HashSet<SplintId>,
    commands: &Sender<WindowTopologyCommand>,
) -> bool {
    handoff_paused || enqueue_pending_exited_splints(dojo_id, lair_retention, pending, commands)
}

fn enqueue_pending_exited_splints(
    dojo_id: DojoId,
    lair_retention: LairRetention,
    pending: &mut HashSet<SplintId>,
    commands: &Sender<WindowTopologyCommand>,
) -> bool {
    if lair_retention.is_protected() {
        pending.clear();
        return true;
    }
    let targets = pending.iter().copied().collect::<Vec<_>>();
    for target in targets {
        match commands.try_send(WindowTopologyCommand::Close { dojo_id, target }) {
            Ok(()) => {
                pending.remove(&target);
            }
            Err(TrySendError::Full(_)) => return true,
            Err(TrySendError::Closed(_)) => return false,
        }
    }
    true
}

impl WindowOptions {
    fn activate_multi_pane_input(&mut self) -> Result<Vec<WindowPaneOptions>> {
        if self.panes.is_empty() {
            return Ok(Vec::new());
        }
        anyhow::ensure!(
            self.snapshot.is_none() && self.updates.is_none() && self.commands.is_none(),
            "legacy and multi-pane window inputs cannot be mixed"
        );
        let layout = self
            .layout
            .as_ref()
            .context("multi-pane layout is required")?;
        anyhow::ensure!(
            layout.splint_count() == self.panes.len(),
            "layout and pane input counts differ"
        );
        let mut identities = HashSet::new();
        for pane in &self.panes {
            pane.snapshot
                .validate()
                .map_err(|error| anyhow::anyhow!(error.message))?;
            anyhow::ensure!(
                identities.insert(pane.snapshot.splint_id)
                    && layout.find_splint(pane.snapshot.splint_id).is_some(),
                "pane input identity is duplicate or absent from the layout"
            );
        }
        let active = self
            .active_splint
            .unwrap_or_else(|| layout.first_splint_id());
        let index = self
            .panes
            .iter()
            .position(|pane| pane.snapshot.splint_id == active)
            .context("active Splint is absent from pane inputs")?;
        let mut active = self.panes.remove(index);
        apply_theme(&mut active.snapshot, self.theme);
        self.snapshot = Some(active.snapshot);
        self.updates = Some(active.updates);
        self.commands = Some(active.commands);
        self.authority = active.authority;
        self.controlled = active.controlled;
        self.terminal_grid_limits = active.terminal_grid_limits;
        for pane in &mut self.panes {
            apply_theme(&mut pane.snapshot, self.theme);
        }
        Ok(std::mem::take(&mut self.panes))
    }
}

/// What [`run`] derives from its options before it connects to the
/// compositor.
struct PreparedWindow {
    render_context: RenderContext,
    inactive_options: Vec<WindowPaneOptions>,
    text_row: Option<TextRow>,
    snapshot_frame: Option<SnapshotFrame>,
    initial_width: u32,
    initial_height: u32,
    /// The one logical size a trusted dialog asks the compositor for, as
    /// both its minimum and maximum: its natural size, the initial size.
    fixed_size: Option<(u32, u32)>,
}

/// The part of [`run`] before the compositor: input activation, snapshot
/// validation and theming, font and frame loading, and the initial size.
fn prepare_window(options: &mut WindowOptions) -> Result<PreparedWindow> {
    if options.trusted_consent.is_some() {
        // A trusted dialog draws in its own floored colors from its first
        // frame, even one shown before its text is fitted.
        options.theme = trusted_dialog_theme(options.theme);
    }
    let managed_tabs = options.initial_dojo.is_some();
    let render_context = RenderContext::new(options.theme.background_alpha)
        .with_font_size_override(followed_font_size(options.theme))
        .with_chrome_font_size(chrome_font_size(options.ui_font_size, options.theme));
    let inactive_options = options.activate_multi_pane_input()?;
    if let Some(snapshot) = options.snapshot.as_mut() {
        snapshot
            .validate()
            .map_err(|error| anyhow::anyhow!(error.message))?;
        apply_theme(snapshot, options.theme);
    }
    let text_row = options
        .snapshot
        .is_none()
        .then(|| TextRow::load(1))
        .transpose()?;
    let snapshot_frame = options
        .snapshot
        .as_ref()
        .map(|snapshot| {
            SnapshotFrame::load_scaled_with_sources_and_context(
                snapshot,
                120,
                Some(&options.image_sources),
                &render_context,
            )
        })
        .transpose()?;
    let (initial_width, mut initial_height) = snapshot_frame
        .as_ref()
        .map_or(Ok((INITIAL_WIDTH, INITIAL_HEIGHT)), |frame| {
            frame.initial_logical_size(options.initial_columns, options.initial_rows, 120)
        })?;
    if (managed_tabs && options.initial_tab_strip_visible)
        || options.remote_display_identity.is_some()
    {
        initial_height = initial_height
            .checked_add(tab_strip_logical_height(options.theme))
            .context("initial tab strip height overflow")?;
    }
    // Tiling compositors (Hyprland, sway) float a Window whose minimum and
    // maximum sizes are equal, so a trusted dialog asks for exactly the
    // size its text was laid out for. Its snapshot is that natural layout
    // (`TrustedConsentUi::into_window_options`).
    let fixed_size = options
        .trusted_consent
        .as_ref()
        .and(options.snapshot.as_ref())
        .map(|_| (initial_width, initial_height));
    Ok(PreparedWindow {
        render_context,
        inactive_options,
        text_row,
        snapshot_frame,
        initial_width,
        initial_height,
        fixed_size,
    })
}

/// Runs every check [`run`] makes on `options` that needs no compositor:
/// the same preparation, then the viewport and buffer bounds of the initial
/// size, which it returns. A caller can prove a Window it builds gets as far
/// as connecting to the compositor without a display.
///
/// # Errors
///
/// Returns the error [`run`] would return for these options before it
/// reaches the compositor.
#[doc(hidden)]
pub fn check_window_options(mut options: WindowOptions) -> Result<(u32, u32)> {
    let prepared = prepare_window(&mut options)?;
    viewport_destination(prepared.initial_width, prepared.initial_height)?;
    bounded_window_backing_len(prepared.initial_width, prepared.initial_height)?;
    Ok((prepared.initial_width, prepared.initial_height))
}

/// Opens the native window and runs until compositor close or an explicit shutdown.
///
/// Q/Escape close shortcuts are available only when `evidence_close_shortcuts` is set.
///
/// # Errors
///
/// Returns an error when font setup, required Wayland globals, shared-memory buffers,
/// keyboard state, capture output, or event dispatch cannot be initialized.
pub fn run(mut options: WindowOptions) -> Result<()> {
    anyhow::ensure!(
        !options.keep_surface,
        "a Window that keeps its surface runs through run_window_on"
    );
    note_window_diagnostics(&options);
    let prepared = prepare_window(&mut options)?;
    let mut session = WaylandSession::open()?;
    let mut app = session.new_app(options, prepared)?;
    let event_loop_result = session.run_phase(&mut app);
    session.finish(Some(app), event_loop_result)
}

/// Starts the Window's diagnostic record for a phase's Dojo and Splint.
fn note_window_diagnostics(options: &WindowOptions) {
    if let Some(diagnostics) = diagnostics() {
        let dojo_id = options
            .initial_dojo
            .as_ref()
            .map(|identity| identity.dojo_id);
        let splint_id = options
            .active_splint
            .or_else(|| options.snapshot.as_ref().map(|snapshot| snapshot.splint_id));
        diagnostics.ensure_window(dojo_id, splint_id);
    }
}

/// The compositor connection and event loop a Window runs on. A staged Window
/// ([`run_window_on`]) keeps one across its phases.
struct WaylandSession {
    connection: Connection,
    globals: GlobalList,
    queue_handle: QueueHandle<App>,
    event_loop: EventLoop<'static, App>,
    update_waker: Waker,
}

impl WaylandSession {
    fn open() -> Result<Self> {
        let connection = Connection::connect_to_env().context("connect to Wayland compositor")?;
        let (globals, event_queue) =
            registry_queue_init(&connection).context("read Wayland registry")?;
        let queue_handle = event_queue.handle();
        let event_loop: EventLoop<App> = EventLoop::try_new().context("create event loop")?;
        WaylandSource::new(connection.clone(), event_queue)
            .insert(event_loop.handle())
            .context("register Wayland source")?;
        let (update_ping, update_ping_source) = make_ping().context("create update wake source")?;
        event_loop
            .handle()
            .insert_source(update_ping_source, |(), (), _| {})
            .context("register update wake source")?;
        let update_waker = Waker::from(Arc::new(UpdateWake(update_ping)));
        Ok(Self {
            connection,
            globals,
            queue_handle,
            event_loop,
            update_waker,
        })
    }

    /// Binds the globals and creates the xdg toplevel a new Window draws on.
    #[allow(
        clippy::too_many_lines,
        reason = "Wayland global binding and surface creation form one startup transaction"
    )]
    fn new_platform_and_surface(
        &self,
        options: &mut WindowOptions,
        prepared: &PreparedWindow,
    ) -> Result<(PlatformState, SurfaceState)> {
        let globals = &self.globals;
        let queue_handle = &self.queue_handle;
        let (initial_width, initial_height) = (prepared.initial_width, prepared.initial_height);
        let fixed_size = prepared.fixed_size;
        let compositor = CompositorState::bind(globals, queue_handle)
            .context("compositor does not provide wl_compositor")?;
        let shell = XdgShell::bind(globals, queue_handle)
            .context("compositor does not provide xdg-shell")?;
        let shm = Shm::bind(globals, queue_handle).context("compositor does not provide wl_shm")?;
        let data_device_manager = DataDeviceManagerState::bind(globals, queue_handle)
            .context("compositor does not provide wl_data_device_manager")?;
        let primary_selection_manager =
            PrimarySelectionManagerState::bind(globals, queue_handle).ok();
        let fractional_scale_manager = globals
            .bind::<WpFractionalScaleManagerV1, _, _>(queue_handle, 1..=1, ())
            .ok();
        let viewporter = globals
            .bind::<WpViewporter, _, _>(queue_handle, 1..=1, ())
            .ok();
        // Without the manager no text input is ever created, so nothing is composed.
        let text_input_manager = options
            .input_method
            .then(|| bind_text_input_manager(globals, queue_handle))
            .flatten();
        let surface = compositor.create_surface(queue_handle);
        let window = shell.create_window(surface, WindowDecorations::RequestServer, queue_handle);
        let fractional_scale = fractional_scale_manager
            .as_ref()
            .zip(viewporter.as_ref())
            .map(|(manager, _)| {
                manager.get_fractional_scale(window.wl_surface(), queue_handle, ())
            });
        let viewport = fractional_scale_manager
            .as_ref()
            .zip(viewporter.as_ref())
            .map(|(_, manager)| manager.get_viewport(window.wl_surface(), queue_handle, ()));
        if let Some(viewport) = &viewport {
            let (width, height) = viewport_destination(initial_width, initial_height)?;
            viewport.set_destination(width, height);
        }
        window.set_title(initial_window_title(options));
        window.set_app_id(
            window_app_id(options.trusted_consent.is_some(), &options.app_id).to_owned(),
        );
        if fixed_size.is_some() {
            window.set_min_size(fixed_size);
            window.set_max_size(fixed_size);
        }
        window
            .set_buffer_scale(1)
            .map_err(|_| anyhow::anyhow!("compositor does not support integer buffer scale"))?;
        window.commit();

        let pool_size = bounded_window_backing_len(initial_width, initial_height)?;
        let pool = SlotPool::new(pool_size, &shm).context("create SHM pool")?;
        let background_effect_manager = globals
            .bind::<ExtBackgroundEffectManagerV1, _, _>(queue_handle, 1..=1, ())
            .ok();
        let mut background_effect_state = BackgroundEffectState::default();
        background_effect_state.set_manager_available(background_effect_manager.is_some());
        background_effect_state.set_requested_blur(options.theme.background_blur);
        background_effect_state.set_background_alpha(options.theme.background_alpha);
        let background_effect_trace =
            std::env::var_os("SPLINTERM_BACKGROUND_EFFECT_TRACE").is_some();
        if background_effect_trace {
            if let Some(manager) = &background_effect_manager {
                eprintln!(
                    "splinterm background-effect manager version={} bound",
                    manager.version()
                );
            } else {
                eprintln!("splinterm background-effect manager unavailable");
            }
        }
        let platform = PlatformState {
            registry_state: RegistryState::new(globals),
            compositor,
            seat_state: SeatState::new(globals, queue_handle),
            output_state: OutputState::new(globals, queue_handle),
            data_device_manager,
            primary_selection_manager,
            text_input_manager,
            shm,
            loop_handle: self.event_loop.handle(),
            update_waker: self.update_waker.clone(),
            output_count: 0,
            entered_outputs: Vec::new(),
            seat_count: 0,
        };
        let surface = SurfaceState {
            fractional_scale,
            viewport,
            background_effect_manager,
            background_effect: None,
            background_effect_state,
            background_effect_deferred_commit: None,
            background_effect_reconcile_schedule: BackgroundEffectReconcileSchedule::default(),
            background_effect_capabilities_received: false,
            background_effect_trace,
            window,
            pool,
            buffers: Vec::new(),
            backing: Vec::new(),
            logical_width: initial_width,
            logical_height: initial_height,
            configured: false,
            scale_120: SCALE_DENOMINATOR,
            integer_fallback_scale: 1,
            mapped_notify: options.mapped.take(),
        };
        Ok((platform, surface))
    }

    /// Builds a new Window: a new surface and the phase's content on it.
    fn new_app(&self, mut options: WindowOptions, prepared: PreparedWindow) -> Result<App> {
        let (platform, surface) = self.new_platform_and_surface(&mut options, &prepared)?;
        assemble_app(platform, surface, options, prepared, &self.update_waker)
    }

    /// Runs the event loop until the phase's content asks to end.
    fn run_phase(&mut self, app: &mut App) -> Result<()> {
        let queue_handle = &self.queue_handle;
        while !app.scheduling.exit {
            app.apply_updates(queue_handle)?;
            app.accessibility_turn(queue_handle);
            if !app.scheduling.exit {
                app.flush_pending_terminal_input();
                app.retry_pending_pane_resizes()?;
            }
            app.draw_if_pending(queue_handle)?;
            app.tick_signoff(queue_handle)?;
            app.apply_clipboard_reads()?;
            app.apply_clipboard_image_events();
            app.tick_cursor_blink(queue_handle)?;
            app.tick_agent_spinner(queue_handle)?;
            let Some(dispatch_timeout) = app.event_loop_dispatch_timeout() else {
                break;
            };
            self.event_loop
                .dispatch(dispatch_timeout, app)
                .context("dispatch Wayland events")?;
        }
        Ok(())
    }

    /// Unmaps the Window, if any is left, and ends the compositor connection.
    fn finish(self, app: Option<App>, event_loop_result: Result<()>) -> Result<()> {
        let failure = app.and_then(|mut app| {
            app.release_background_effect();
            let failure = app.scheduling.failure.take();
            drop(app);
            failure
        });
        let Self {
            connection,
            event_loop,
            ..
        } = self;
        drop(event_loop);
        let teardown_result = connection
            .roundtrip()
            .context("complete Wayland surface teardown")
            .map(|_| ());
        if (event_loop_result.is_err() || teardown_result.is_err())
            && let Some(diagnostics) = diagnostics()
        {
            // This line names the event loop or its teardown, unless a
            // handler already recorded where it failed.
            diagnostics.record_failure_location(std::panic::Location::caller());
            diagnostics.request_exit(ExitClass::ErrorWaylandDispatch);
        }
        event_loop_result?;
        if let Some(error) = failure {
            return Err(error);
        }
        teardown_result
    }
}

/// Binds the text-input (IME) manager, when the compositor offers one.
fn bind_text_input_manager(
    globals: &GlobalList,
    queue_handle: &QueueHandle<App>,
) -> Option<ZwpTextInputManagerV3> {
    globals
        .bind::<ZwpTextInputManagerV3, _, _>(queue_handle, 1..=1, ())
        .ok()
}

/// Builds a phase's content on `platform` and `surface`, new or kept.
#[allow(
    clippy::too_many_lines,
    reason = "application state initialization forms one startup transaction"
)]
fn assemble_app(
    platform: PlatformState,
    surface: SurfaceState,
    mut options: WindowOptions,
    prepared: PreparedWindow,
    update_waker: &Waker,
) -> Result<App> {
    let managed_tabs = options.initial_dojo.is_some();
    let PreparedWindow {
        render_context,
        inactive_options,
        text_row,
        snapshot_frame,
        fixed_size,
        ..
    } = prepared;
    let (clipboard_tx, clipboard_rx) = std_mpsc::channel();
    let controller_active = options.controlled && options.commands.is_some();
    let trusted_consent = options.trusted_consent;
    let session_picker = options.session_picker;
    let signoff = SignoffProbe::from_environment(options.authority.development_bypass)?;
    let graphical_input_probe =
        GraphicalInputProbe::from_environment(options.authority.development_bypass)?;
    let inactive_panes = inactive_options
        .into_iter()
        .map(|pane| {
            PaneView::from_inactive_options_with_context(pane, SCALE_DENOMINATOR, &render_context)
        })
        .collect::<Result<Vec<_>>>()?;
    let initial_identity = options
        .initial_dojo
        .take()
        .unwrap_or_else(|| WindowDojoIdentity {
            topology_revision: TopologyRevision::new(0),
            lair_id: LairId::new(),
            dojo_id: DojoId::new(),
            lair_name: String::new(),
            lair_retention: splinterm_core::LairRetention::Disposable,
            dojo_name: String::new(),
        });
    if let Some(diagnostics) = diagnostics() {
        diagnostics.update_topology(
            initial_identity.topology_revision,
            usize::from(managed_tabs),
        );
    }
    let initial_lair_id = initial_identity.lair_id;
    let initial_dojo_id = initial_identity.dojo_id;
    let mut app = App {
        platform,
        surface,
        presentation: PresentationState {
            render_context,
            text_row,
            renderer_generation: 0,
            theme_generation: 0,
            pending_font_update: None,
            applied_text_size_px: options.theme.shell.text_size_px,
            rejected_text_size: None,
            ui_font_size: options.ui_font_size,
            cursor_style: options.cursor_style,
            cursor_blink: options.cursor_blink,
            tab_strip_border: options.tab_strip_border,
            explorer_full_height: options.explorer_full_height,
            explorer_collapsed: options.explorer_collapsed,
            explorer_chrome_cell: std::cell::Cell::new(None),
            title: TitleSettings {
                text: options.title,
                controller_label: options.controller_title,
            },
            theme: options.theme,
            theme_colors: options.theme_colors.clone(),
            pane_divider_style: options.pane_divider_style,
            pane_gap: options.pane_gap,
            frame_title_mode: options.frame_title_mode,
            frame_titles: HashMap::new(),
            frame_title_cell: None,
            evidence_close_shortcuts: options.evidence_close_shortcuts,
            font_zoom_steps: 0,
            zoomed_splint: None,
            capture: options.capture,
            capture_scale: options.capture_scale,
            initial_columns: options.initial_columns,
            explorer_layout: None,
            explorer_text_cache: SessionPickerTextCache::default(),
            search_text_cache: SessionPickerTextCache::default(),
            painted_search_panel: None,
            explorer_visible_start: 0,
            explorer_agents_start: 0,
            explorer_pressed: None,
            full_redraw: true,
        },
        input: InputState {
            keymap: options.keymap,
            prefix_state: PrefixState::Idle,
            prefix_timeout: Duration::from_millis(options.prefix_timeout_ms),
            text_input: None,
            text_input_seat: None,
            ime: ImeState::default(),
            reduced_motion: reduced_motion_requested(),
            keyboard_focused: false,
            graphical_focus: options.graphical_focus,
            forced_control_transfer: options.forced_control_transfer,
            optimistic_remote_splits: options.optimistic_remote_splits,
            input_generation: 0,
            pending_terminal_input: VecDeque::new(),
            terminal_input_saturated: false,
            terminal_focus_reported: false,
            ime_generation: 0,
            ime_modal_barrier: false,
            modifiers: Modifiers::default(),
            keyboard: None,
            keyboard_seat: None,
            pointer: None,
            pointer_seat: None,
            last_pointer_serial: None,
            pressed_buttons: HashMap::new(),
            divider_drag: None,
            vertical_wheel: WheelAccumulator::default(),
            scrollback_wheel: WheelAccumulator::default(),
            cursor_blink_visible: true,
            last_cursor_blink: Instant::now(),
            pressed_keys: HashSet::new(),
            carried_keys: HashSet::new(),
        },
        clipboard: ClipboardState {
            data_device: None,
            primary_device: None,
            clipboard_offer: None,
            primary_offer: None,
            drag_target: None,
            clipboard_sources: Vec::new(),
            primary_sources: Vec::new(),
            image_directory: options.clipboard_image_directory,
            local_endpoint: options.local_endpoint,
            image_request: None,
            palette_image_target: None,
            clipboard_tx,
            clipboard_rx,
        },
        panes: PanesState {
            pane: PaneView {
                snapshot: options.snapshot,
                snapshot_frame,
                terminal_grid_limits: options.terminal_grid_limits,
                grid_cap_diagnostic_emitted: false,
                image_sources: options.image_sources,
                scrollback_viewport: ScrollbackViewport::default(),
                painted_history_status: None,
                history_page_pending: false,
                resync_pending: false,
                history_page_top: None,
                history_selection_pin_blocked: false,
                scroll_started_at: None,
                rendered_viewport_offset: 0,
                rendered_row_ids: Vec::new(),
                viewport_dirty: false,
                updates: options.updates,
                commands: options.commands,
                controller_active,
                control_release_pending: false,
                pending_focus_report: None,
                pending_control_transfer: None,
                search: SearchUiState::default(),
                authority: options.authority,
                last_resize: None,
                pending_resize: None,
                prepare_dirty_rows: Vec::new(),
                raster_dirty_rows: Vec::new(),
                surface_dirty_rows: Vec::new(),
                pending_scrolls: Vec::new(),
                trace_correlation: None,
                trace_pane_role: "focused",
                trace_superseded_revisions: 0,
                selected_text: None,
                selection: None,
                selecting: false,
                pointer_cell: None,
                hovered_url: None,
            },
            inactive_panes,
            layout: options.layout,
            restored_frontend_needs_resize: false,
            pending_exited_splints: HashSet::new(),
            pending_remote_splits: HashMap::new(),
            dirty_inactive_panes: HashSet::new(),
        },
        explorer: LairExplorerUi::new(options.explorer_collapsed),
        accessibility: navigation_accessibility::WindowAccessibility::new(update_waker.clone()),
        access: agent_access::WindowAccess::default(),
        handoff: HandoffFrontend::new(options.handoff_status.take(), update_waker),
        tab_state: TabsState {
            tabs: WindowTabSet::new(DojoTab::new(initial_lair_id, initial_dojo_id, None)),
            active_identity: initial_identity,
            managed_tabs,
            tab_strip_visible: options.initial_tab_strip_visible,
            remote_display_identity: options.remote_display_identity,
            remote_host_text: None,
            remote_host_clipped: None,
            tab_strip_layout: None,
            tab_strip_pressed: None,
            tab_label_cache: HashMap::new(),
            tab_close_text: None,
            tab_new_text: None,
            topology_updates: options.topology_updates,
            topology_commands: options.topology_commands,
            topology_settings: options.topology_settings,
            session_switch_pending: false,
            deferred_topology_updates: Vec::new(),
            agent: agent_status::TabAgentStatus::new(
                options.agent_status_presentation,
                options.agent_status_style,
            ),
            layout_ledger: options.layout_ledger.take(),
        },
        modal: ModalState {
            trusted_consent,
            trusted_layout: chrome::TrustedDialogLayout::new(fixed_size),
            copy_mode: None,
            binding_help: None,
            agent_access: None,
            command_palette: None,
            command_palette_layout: None,
            command_palette_pressed: None,
            command_palette_text_cache: CommandPaletteTextCache::default(),
            command_palette_open_focus: None,
            command_palette_reconcile_pending: false,
            dojo_prompt: None,
            dojo_prompt_explorer_target: None,
            dojo_prompt_layout: None,
            dojo_prompt_pressed: None,
            dojo_prompt_text_cache: CommandPaletteTextCache::default(),
            tab_context_menu: None,
            tab_context_menu_anchor: (0, 0),
            tab_context_menu_layout: None,
            tab_context_menu_pressed: None,
            tab_context_menu_retarget: None,
            tab_context_menu_text_cache: CommandPaletteTextCache::default(),
            session_picker,
            selector_kind: None,
            session_picker_targets: Vec::new(),
            session_picker_creation_target: None,
            session_picker_layout: None,
            session_picker_pressed: None,
            session_picker_text_cache: SessionPickerTextCache::default(),
            session_picker_wheel: WheelAccumulator::default(),
            session_picker_consumed_keys: HashSet::new(),
            session_picker_redraw: false,
            session_picker_reconcile_pending: false,
            session_picker_open_focus: None,
            session_picker_requested: false,
            session_picker_retry_command: None,
            deferred_picker_theme: None,
            remote_profiles: None,
        },
        scheduling: SchedulingState {
            signoff,
            graphical_input_probe,
            scroll_trace: std::env::var_os("SPLINTERM_SCROLL_TRACE").is_some(),
            exit: false,
            failure: None,
            frame_pending: false,
            redraw_pending: false,
            terminal_redraw_pending: false,
            next_commit_sequence: 0,
            pending_frame_trace: None,
            keep_surface: options.keep_surface,
            parked: None,
            busy_notice: false,
        },
    };
    app.explorer
        .set_agent_status_enabled(options.agent_status_presentation);
    app.explorer.set_agent_glyphs(app.tab_state.agent.glyphs());
    Ok(app)
}

fn apply_theme(snapshot: &mut TerminalSnapshot, theme: ResolvedTheme) {
    if snapshot.palette.len() == 256 {
        snapshot.palette[..16].copy_from_slice(&theme.ansi);
    }
    snapshot.default_colors = [theme.foreground, theme.background, theme.cursor];
}

fn viewport_destination(width: u32, height: u32) -> Result<(i32, i32)> {
    Ok((
        i32::try_from(width).context("viewport width fits i32")?,
        i32::try_from(height).context("viewport height fits i32")?,
    ))
}

fn bounded_window_backing_len(width: u32, height: u32) -> Result<usize> {
    let backing_len = usize::try_from(
        width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(4))
            .context("backing dimensions overflow")?,
    )
    .context("backing size fits usize")?;
    let presentation_bytes = backing_len
        .checked_mul(MAX_SHM_BUFFERS + 1)
        .context("Window presentation byte accounting overflow")?;
    anyhow::ensure!(
        presentation_bytes <= MAX_WINDOW_PRESENTATION_BYTES,
        "WindowPresentationLimit: backing and SHM buffers exceed 512 MiB"
    );
    Ok(backing_len)
}

fn buffer_dimensions(
    logical_width: u32,
    logical_height: u32,
    scale_120: u32,
) -> Result<(u32, u32, i32)> {
    SurfaceGeometry::new(logical_width, logical_height, scale_120)?.buffer_layout()
}

#[derive(Debug, PartialEq, Eq)]
enum DrawBufferChoice {
    Reuse(usize),
    Allocate,
    Wait,
}

fn choose_draw_buffer(count: usize, mut available: impl FnMut(usize) -> bool) -> DrawBufferChoice {
    // Index zero is the last committed buffer. Even after wl_buffer.release,
    // keep its completed chrome intact while another canvas is cleared and
    // recomposed; this also avoids exposing intermediate SHM pixels to captures.
    if let Some(index) = (1..count).find(|&index| available(index)) {
        DrawBufferChoice::Reuse(index)
    } else if count < MAX_SHM_BUFFERS {
        DrawBufferChoice::Allocate
    } else {
        DrawBufferChoice::Wait
    }
}

fn window_buffer_dimensions(
    has_pane_layout: bool,
    managed_tabs: bool,
    remote: bool,
    logical_size: (u32, u32),
    scale_120: u32,
    geometry: Option<WindowGeometry>,
) -> Result<(u32, u32, i32)> {
    // Remote observers also translate content below chrome. Translation moves
    // the grid, not its source surface extent, so allocate the whole Window.
    if !has_pane_layout
        && !managed_tabs
        && !remote
        && let Some(geometry) = geometry
    {
        return geometry.buffer_layout();
    }
    buffer_dimensions(logical_size.0.max(1), logical_size.1.max(1), scale_120)
}

fn note_output_enter<T: Clone + Eq>(entered: &mut Vec<T>, output: &T) {
    entered.retain(|candidate| candidate != output);
    entered.push(output.clone());
}

fn note_output_leave<T: Eq>(entered: &mut Vec<T>, output: &T) -> bool {
    let was_most_recent = entered.last() == Some(output);
    entered.retain(|candidate| candidate != output);
    was_most_recent
}

#[derive(Clone, Debug, Default)]
struct SearchUiState {
    input: Option<BoundedTextEditor>,
    query: String,
    matches: Vec<SearchMatch>,
    selected: usize,
    next_cursor: Option<String>,
    pending_reveal: Option<SearchMatch>,
    /// Searches sent and not yet answered, and the query of the latest.
    in_flight: usize,
    submitted_query: String,
    /// Older history is still being searched for the latest query.
    searching: bool,
    /// The history generation and terminal revision of the latest page:
    /// its row ids and its cursor mean something only there.
    page_generation: u64,
    page_revision: u64,
}

impl SearchUiState {
    /// Takes one search page. A page that ran out of time before finding
    /// anything returns the cursor to read the next one with, while the
    /// page answers the latest search and the query is unchanged. Daemon
    /// search reads about 10 ms of rows per request, so without this a match
    /// further back looks missing.
    fn take_page(&mut self, page: SearchPage) -> Option<String> {
        self.in_flight = self.in_flight.saturating_sub(1);
        let latest = self.in_flight == 0;
        let keep_going = latest
            && page.matches.is_empty()
            && page.timed_out
            && self.input.is_some()
            && self.submitted_query == self.query;
        self.matches = page.matches;
        self.selected = 0;
        self.next_cursor = page.next_cursor;
        self.page_generation = page.history_generation;
        self.page_revision = page.terminal_revision;
        self.pending_reveal = self.matches.first().cloned();
        self.searching = keep_going && self.next_cursor.is_some();
        if self.searching {
            self.next_cursor.clone()
        } else {
            None
        }
    }

    /// Closes the search field. Searches already sent are still counted, so
    /// their late pages never pass for a newer search's.
    fn close(&mut self) {
        *self = Self {
            in_flight: self.in_flight,
            ..Self::default()
        };
    }

    fn resync_required(&mut self) {
        self.in_flight = self.in_flight.saturating_sub(1);
        self.forget_results();
    }

    /// Whether the results name rows of history `generation`. Results from
    /// before history was replaced name rows that no longer exist, and are
    /// dropped; a page newer than the snapshot waits for it to catch up.
    fn results_match(&mut self, generation: u64) -> bool {
        match self.page_generation.cmp(&generation) {
            std::cmp::Ordering::Less => {
                self.forget_results();
                false
            }
            std::cmp::Ordering::Greater => false,
            std::cmp::Ordering::Equal => true,
        }
    }

    /// Drops results whose rows are gone, as after history was replaced;
    /// Enter searches again.
    fn forget_results(&mut self) {
        self.matches.clear();
        self.next_cursor = None;
        self.pending_reveal = None;
        self.searching = false;
    }
}

fn new_search_editor() -> BoundedTextEditor {
    BoundedTextEditor::new(
        String::new(),
        splinterm_protocol::MAX_SEARCH_QUERY_BYTES,
        splinterm_protocol::MAX_SEARCH_QUERY_BYTES,
        false,
    )
}

#[derive(Debug)]
struct PendingPaneResize {
    size: (u16, u16, u16, u16),
    command: WindowCommand,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent pane rendering, history, and input flags are not one state machine"
)]
struct PaneView {
    snapshot: Option<TerminalSnapshot>,
    snapshot_frame: Option<SnapshotFrame>,
    terminal_grid_limits: TerminalGridLimits,
    grid_cap_diagnostic_emitted: bool,
    image_sources: ImageContentLeaseSet,
    scrollback_viewport: ScrollbackViewport,
    painted_history_status: Option<HistoryOverlayStatus>,
    /// A return-to-live resynchronization was sent and its authoritative
    /// snapshot has not arrived; another would only queue behind it.
    resync_pending: bool,
    history_page_pending: bool,
    /// The oldest cached history row when older history was last requested.
    /// Pages join the cache only while that row is still its first.
    history_page_top: Option<u64>,
    history_selection_pin_blocked: bool,
    scroll_started_at: Option<Instant>,
    rendered_viewport_offset: usize,
    /// The row ids the pane's frame was last drawn with, top to bottom.
    rendered_row_ids: Vec<Option<u64>>,
    viewport_dirty: bool,
    updates: Option<Receiver<WindowUpdate>>,
    commands: Option<Sender<WindowCommand>>,
    controller_active: bool,
    control_release_pending: bool,
    pending_focus_report: Option<bool>,
    pending_control_transfer: Option<u64>,
    search: SearchUiState,
    authority: AuthorityStatus,
    last_resize: Option<(u16, u16, u16, u16)>,
    pending_resize: Option<PendingPaneResize>,
    prepare_dirty_rows: Vec<bool>,
    raster_dirty_rows: Vec<bool>,
    surface_dirty_rows: Vec<bool>,
    pending_scrolls: Vec<splinterm_protocol::TerminalScroll>,
    trace_correlation: Option<PerfTraceCorrelation>,
    trace_pane_role: &'static str,
    trace_superseded_revisions: u64,
    selected_text: Option<Vec<u8>>,
    selection: Option<Selection>,
    selecting: bool,
    pointer_cell: Option<CellPosition>,
    hovered_url: Option<(CellPosition, CellPosition, String)>,
}

impl PaneView {
    fn window_geometry(
        &self,
        frame: &SnapshotFrame,
        logical_width: u32,
        logical_height: u32,
        scale_120: u32,
    ) -> Result<WindowGeometry> {
        frame.window_geometry_with_limits(
            logical_width,
            logical_height,
            scale_120,
            self.terminal_grid_limits.maximum_columns,
            self.terminal_grid_limits.maximum_rows,
        )
    }
}

#[cfg(test)]
fn rebuild_pane_scaled_frame(pane: &mut PaneView, scale_120: u32) -> Result<bool> {
    let Some(display) = pane.display_snapshot() else {
        return Ok(false);
    };
    pane.snapshot_frame = Some(SnapshotFrame::load_scaled_with_sources(
        &display,
        scale_120,
        Some(&pane.image_sources),
    )?);
    finish_rebuilt_pane_frame(pane);
    Ok(true)
}

fn prepare_pane_scaled_frame_with_context(
    pane: &PaneView,
    scale_120: u32,
    context: &RenderContext,
) -> Result<Option<SnapshotFrame>> {
    let Some(display) = pane.display_snapshot() else {
        return Ok(None);
    };
    Ok(Some(SnapshotFrame::load_scaled_with_sources_and_context(
        &display,
        scale_120,
        Some(&pane.image_sources),
        context,
    )?))
}

fn install_prepared_pane_frame(pane: &mut PaneView, frame: Option<SnapshotFrame>) {
    if let Some(frame) = frame {
        pane.snapshot_frame = Some(frame);
        finish_rebuilt_pane_frame(pane);
    }
}

fn rebuild_pane_scaled_frame_with_context(
    pane: &mut PaneView,
    scale_120: u32,
    context: &RenderContext,
) -> Result<bool> {
    let Some(display) = pane.display_snapshot() else {
        return Ok(false);
    };
    pane.snapshot_frame = Some(SnapshotFrame::load_scaled_with_sources_and_context(
        &display,
        scale_120,
        Some(&pane.image_sources),
        context,
    )?);
    finish_rebuilt_pane_frame(pane);
    Ok(true)
}

fn row_ids(display: &TerminalSnapshot) -> Vec<Option<u64>> {
    display.visible_rows.iter().map(|row| row.row_id).collect()
}

/// Whether moving the view `delta` rows further back (or forward, when
/// negative) shows the drawn rows `previous` shifted by `delta` in `current`.
fn viewport_shift_matches(previous: &[Option<u64>], current: &[Option<u64>], delta: isize) -> bool {
    let rows = current.len();
    let count = delta.unsigned_abs();
    if previous.len() != rows || count == 0 || count >= rows {
        return false;
    }
    let (kept, shown) = if delta > 0 {
        (&previous[..rows - count], &current[count..])
    } else {
        (&previous[count..], &current[..rows - count])
    };
    kept.iter().all(Option::is_some) && kept == shown
}

fn finish_rebuilt_pane_frame(pane: &mut PaneView) {
    pane.rendered_viewport_offset = pane.scrollback_viewport.offset_from_bottom();
    pane.rendered_row_ids = pane.displayed_row_ids();
    pane.viewport_dirty = false;
    pane.scroll_started_at = None;
    pane.prepare_dirty_rows.fill(false);
    pane.raster_dirty_rows.fill(false);
    pane.surface_dirty_rows.fill(false);
    pane.pending_scrolls.clear();
}

#[cfg(test)]
fn rebuild_dirty_pane_viewport_frame(pane: &mut PaneView, scale_120: u32) -> Result<bool> {
    if !pane.viewport_dirty {
        return Ok(false);
    }
    rebuild_pane_scaled_frame(pane, scale_120)
}

fn rebuild_dirty_pane_viewport_frame_with_context(
    pane: &mut PaneView,
    scale_120: u32,
    context: &RenderContext,
) -> Result<bool> {
    if !pane.viewport_dirty {
        return Ok(false);
    }
    rebuild_pane_scaled_frame_with_context(pane, scale_120, context)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct BackgroundUpdateImpact {
    visual_changed: bool,
    frame_dirty: bool,
}

impl BackgroundUpdateImpact {
    const NONE: Self = Self {
        visual_changed: false,
        frame_dirty: false,
    };
    const VISUAL: Self = Self {
        visual_changed: true,
        frame_dirty: false,
    };
    const FRAME: Self = Self {
        visual_changed: true,
        frame_dirty: true,
    };
}

impl PaneView {
    fn retain_trace_correlation(
        &mut self,
        trace: Option<PerfTraceCorrelation>,
        pane_role: &'static str,
    ) {
        let Some(trace) = trace else { return };
        if self
            .trace_correlation
            .is_some_and(|current| current != trace)
        {
            self.trace_superseded_revisions = self.trace_superseded_revisions.saturating_add(1);
        }
        self.trace_correlation = Some(trace);
        self.trace_pane_role = pane_role;
    }

    fn clear_trace_correlation(&mut self) {
        self.trace_correlation = None;
        self.trace_superseded_revisions = 0;
    }

    fn discard_lagging_history(&mut self) {
        if let Some(snapshot) = self.snapshot.as_mut() {
            discard_lagging_history(snapshot, &self.scrollback_viewport);
        }
    }

    fn history_rows_needed_for_viewport_transition(&self) -> Vec<TerminalRow> {
        if self.scrollback_viewport.is_live() {
            Vec::new()
        } else {
            self.snapshot
                .as_ref()
                .map_or_else(Vec::new, |snapshot| snapshot.scrollback_rows.clone())
        }
    }

    fn trace_pane_role(&self, live_role: &'static str) -> &'static str {
        if self.scrollback_viewport.is_live() {
            live_role
        } else {
            "detached-viewport"
        }
    }

    fn pending_commit_trace(&self) -> Option<PaneCommitTrace> {
        let snapshot = self.snapshot.as_ref()?;
        Some(PaneCommitTrace {
            splint_id: snapshot.splint_id,
            incarnation: snapshot.incarnation,
            correlation: self.trace_correlation?,
            pane_role: self.trace_pane_role,
            superseded_revisions: self.trace_superseded_revisions,
        })
    }

    #[cfg(test)]
    fn from_inactive_options(options: WindowPaneOptions, scale_120: u32) -> Result<Self> {
        Self::from_options(options, scale_120)
    }

    fn from_inactive_options_with_context(
        options: WindowPaneOptions,
        scale_120: u32,
        context: &RenderContext,
    ) -> Result<Self> {
        Self::from_options_with_context(options, scale_120, context)
    }

    #[cfg(test)]
    fn from_options(options: WindowPaneOptions, scale_120: u32) -> Result<Self> {
        let snapshot_frame = Some(SnapshotFrame::load_scaled_with_sources(
            &options.snapshot,
            scale_120,
            Some(&options.image_sources),
        )?);
        Ok(Self::from_options_and_frame(options, snapshot_frame))
    }

    fn from_options_with_context(
        options: WindowPaneOptions,
        scale_120: u32,
        context: &RenderContext,
    ) -> Result<Self> {
        let snapshot_frame = Some(SnapshotFrame::load_scaled_with_sources_and_context(
            &options.snapshot,
            scale_120,
            Some(&options.image_sources),
            context,
        )?);
        Ok(Self::from_options_and_frame(options, snapshot_frame))
    }

    fn from_options_and_frame(
        options: WindowPaneOptions,
        snapshot_frame: Option<SnapshotFrame>,
    ) -> Self {
        Self {
            snapshot: Some(options.snapshot),
            snapshot_frame,
            terminal_grid_limits: options.terminal_grid_limits,
            grid_cap_diagnostic_emitted: false,
            image_sources: options.image_sources,
            scrollback_viewport: ScrollbackViewport::default(),
            painted_history_status: None,
            history_page_pending: false,
            resync_pending: false,
            history_page_top: None,
            history_selection_pin_blocked: false,
            scroll_started_at: None,
            rendered_viewport_offset: 0,
            rendered_row_ids: Vec::new(),
            viewport_dirty: false,
            updates: Some(options.updates),
            commands: Some(options.commands),
            controller_active: options.controlled,
            control_release_pending: false,
            pending_focus_report: None,
            pending_control_transfer: None,
            search: SearchUiState::default(),
            authority: options.authority,
            last_resize: None,
            pending_resize: None,
            prepare_dirty_rows: Vec::new(),
            raster_dirty_rows: Vec::new(),
            surface_dirty_rows: Vec::new(),
            pending_scrolls: Vec::new(),
            trace_correlation: None,
            trace_pane_role: "visible-inactive",
            trace_superseded_revisions: 0,
            selected_text: None,
            selection: None,
            selecting: false,
            pointer_cell: None,
            hovered_url: None,
        }
    }

    fn clear_local_content_state(&mut self) {
        self.selected_text = None;
        self.selection = None;
        self.selecting = false;
        self.hovered_url = None;
        self.history_selection_pin_blocked = false;
    }

    /// The row ids this pane shows now, top to bottom.
    fn displayed_row_ids(&self) -> Vec<Option<u64>> {
        self.snapshot.as_ref().map_or_else(Vec::new, |snapshot| {
            self.scrollback_viewport
                .visible_rows(snapshot)
                .into_iter()
                .map(|row| row.row_id)
                .collect()
        })
    }

    /// The screen selections resolve against below the history cache: the
    /// one this pane shows, which is the held screen while a viewport holds.
    fn selection_screen(&self) -> Option<&[TerminalRow]> {
        let snapshot = self.snapshot.as_ref()?;
        Some(
            self.scrollback_viewport
                .held_screen()
                .unwrap_or(&snapshot.visible_rows),
        )
    }

    fn display_snapshot(&self) -> Option<TerminalSnapshot> {
        let snapshot = self.snapshot.as_ref()?;
        if self.scrollback_viewport.is_live() {
            return Some(snapshot.clone());
        }
        // A held viewport shows an older screen; the live cursor is not on it.
        let cursor_row = (!self.scrollback_viewport.is_holding())
            .then(|| {
                viewport_cursor_row(
                    snapshot.cursor_row,
                    self.scrollback_viewport.offset_from_bottom(),
                    snapshot.rows,
                )
            })
            .flatten();
        let mut input_modes = snapshot.input_modes;
        if cursor_row.is_none() {
            input_modes.cursor_visible = false;
        }
        Some(TerminalSnapshot {
            splint_id: snapshot.splint_id,
            incarnation: snapshot.incarnation,
            revision: snapshot.revision,
            columns: snapshot.columns,
            rows: snapshot.rows,
            cursor_column: cursor_row.map_or(-1, |_| snapshot.cursor_column),
            cursor_row: cursor_row.unwrap_or(-1),
            cursor_deferred_wrap: false,
            active_screen: snapshot.active_screen,
            input_modes,
            palette: snapshot.palette.clone(),
            default_colors: snapshot.default_colors,
            title: snapshot.title.clone(),
            visible_rows: self
                .scrollback_viewport
                .visible_rows(snapshot)
                .into_iter()
                .cloned()
                .collect(),
            history_generation: snapshot.history_generation,
            oldest_available_scrollback_row_id: None,
            newest_available_scrollback_row_id: None,
            scrollback_rows: Vec::new(),
            available_scrollback_rows: snapshot.available_scrollback_rows,
            omitted_oldest_scrollback_rows: snapshot.available_scrollback_rows,
            images: snapshot.images.clone(),
            exited_code: snapshot.exited_code,
            exited_signal: snapshot.exited_signal,
        })
    }

    fn display_snapshot_cow(&self) -> Option<Cow<'_, TerminalSnapshot>> {
        if self.scrollback_viewport.is_live() {
            self.snapshot.as_ref().map(Cow::Borrowed)
        } else {
            self.display_snapshot().map(Cow::Owned)
        }
    }

    fn apply_background_pages(
        &mut self,
        pages: Vec<splinterm_protocol::ScrollbackPage>,
    ) -> Result<bool> {
        self.history_page_pending = false;
        let pinned = self
            .selection
            .map(|selection| [selection.anchor.row_id, selection.end.row_id]);
        let snapshot = self
            .snapshot
            .as_mut()
            .context("scrollback pages arrived before initial pane snapshot")?;
        if !pages_join_history(&pages, snapshot, self.history_page_top) {
            return Ok(false);
        }
        let first_loaded = snapshot
            .scrollback_rows
            .first()
            .and_then(|row| row.row_id)
            .unwrap_or(u64::MAX);
        let existing = snapshot
            .scrollback_rows
            .iter()
            .filter_map(|row| row.row_id)
            .collect::<std::collections::BTreeSet<_>>();
        // Only a page read at this revision describes this snapshot's bounds.
        let metadata = pages
            .first()
            .filter(|page| page.terminal_revision == snapshot.revision)
            .map(|page| (page.oldest_available_row_id, page.newest_available_row_id));
        let mut older = pages
            .into_iter()
            .rev()
            .flat_map(|page| page.rows)
            .filter(|row| {
                row.row_id
                    .is_some_and(|id| id < first_loaded && !existing.contains(&id))
            })
            .collect::<Vec<_>>();
        if older.is_empty() {
            return Ok(false);
        }
        older.extend(snapshot.scrollback_rows.iter().cloned());
        // Selections resolve against the held screen while a view holds.
        let screen = self
            .scrollback_viewport
            .held_screen()
            .unwrap_or(&snapshot.visible_rows);
        let Some(older) = bound_history_page_with_pins(older, pinned, screen) else {
            self.history_selection_pin_blocked = true;
            return Ok(false);
        };
        snapshot.scrollback_rows = older;
        snapshot.omitted_oldest_scrollback_rows = omitted_rows_before_cache(
            snapshot.oldest_available_scrollback_row_id,
            &snapshot.scrollback_rows,
            snapshot.available_scrollback_rows,
        );
        if let Some((oldest, newest)) = metadata {
            snapshot.oldest_available_scrollback_row_id = oldest;
            snapshot.newest_available_scrollback_row_id = newest;
        }
        self.scrollback_viewport.reanchor(snapshot);
        Ok(true)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the bounded pane reducer keeps every protocol update transition explicit"
    )]
    fn apply_background_update(
        &mut self,
        update: WindowUpdate,
        theme: ResolvedTheme,
        pane_role: &'static str,
    ) -> Result<BackgroundUpdateImpact> {
        match update {
            WindowUpdate::Snapshot {
                mut snapshot,
                image_sources,
                authoritative,
            } => {
                self.resync_pending &= !authoritative;
                snapshot
                    .validate()
                    .map_err(|error| anyhow::anyhow!(error.message))?;
                apply_theme(&mut snapshot, theme);
                if let Some(current) = self.snapshot.as_ref()
                    && !snapshot_replaces(current, &snapshot, authoritative)?
                {
                    return Ok(BackgroundUpdateImpact::NONE);
                }
                let previous_generation = self
                    .snapshot
                    .as_ref()
                    .map_or(snapshot.history_generation, |current| {
                        current.history_generation
                    });
                let previous_rows = self.history_rows_needed_for_viewport_transition();
                if let Some(current) = self.snapshot.as_ref() {
                    retain_history_for_viewport(
                        current,
                        &mut snapshot,
                        &mut self.scrollback_viewport,
                    );
                }
                self.scrollback_viewport.observe_history_change(
                    previous_generation,
                    &previous_rows,
                    &snapshot,
                );
                discard_lagging_history(&mut snapshot, &self.scrollback_viewport);
                self.clear_local_content_state();
                self.snapshot = Some(snapshot);
                self.image_sources = image_sources;
                self.clear_trace_correlation();
                Ok(BackgroundUpdateImpact::FRAME)
            }
            WindowUpdate::Update {
                update,
                image_sources,
                trace,
            } => {
                let apply_started = trace.map(|_| Instant::now());
                let trace_base_revision = update.base_revision;
                let trace_revision = update.revision;
                let trace_rows = update.rows.len();
                let content_changed = terminal_update_changes_visible_content(&update);
                let mut frame_dirty = content_changed
                    || update.cursor.is_some()
                    || update.input_modes.is_some()
                    || image_sources.is_some();
                let cursor_only = !content_changed
                    && image_sources.is_none()
                    && !self.viewport_dirty
                    && self
                        .snapshot_frame
                        .as_ref()
                        .zip(self.snapshot.as_ref())
                        .is_some_and(|(frame, snapshot)| frame.can_refresh_cursor_rows(snapshot));
                let previous_generation = self
                    .snapshot
                    .as_ref()
                    .map_or(1, |snapshot| snapshot.history_generation);
                let previous_rows = self.history_rows_needed_for_viewport_transition();
                let trace_copied_history_bytes =
                    apply_started.map(|_| history_cache_bytes(&previous_rows));
                {
                    let snapshot = self
                        .snapshot
                        .as_mut()
                        .context("terminal update arrived before initial pane snapshot")?;
                    apply_terminal_update_for_viewport(
                        snapshot,
                        update,
                        &mut self.scrollback_viewport,
                    )?;
                    apply_theme(snapshot, theme);
                    self.scrollback_viewport.observe_history_change(
                        previous_generation,
                        &previous_rows,
                        snapshot,
                    );
                    discard_lagging_history(snapshot, &self.scrollback_viewport);
                }
                if let Some(image_sources) = image_sources {
                    self.image_sources = image_sources;
                }
                if cursor_only && let Some(display) = self.display_snapshot() {
                    self.snapshot_frame
                        .as_mut()
                        .expect("cursor-only frame exists")
                        .refresh_cursor(&display)?;
                    frame_dirty = false;
                }
                let trace_pane_role = self.trace_pane_role(pane_role);
                if frame_dirty {
                    self.retain_trace_correlation(trace, trace_pane_role);
                }
                if content_changed {
                    self.clear_local_content_state();
                }
                if let (Some(started), Some(trace), Some(snapshot)) =
                    (apply_started, trace, self.snapshot.as_ref())
                {
                    emit_perf_trace(
                        "splinterm",
                        "client_apply",
                        PerfTraceEvent {
                            splint_id: Some(snapshot.splint_id),
                            incarnation: Some(snapshot.incarnation),
                            base_revision: Some(trace_base_revision),
                            revision: Some(trace_revision),
                            subscription_id: Some(trace.subscription_id),
                            transaction_sequence: Some(trace.transaction_sequence),
                            duration_ns: Some(
                                u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                            ),
                            rows: Some(u64::try_from(trace_rows).unwrap_or(u64::MAX)),
                            pane_role: Some(trace_pane_role),
                            cached_history_rows: Some(
                                u64::try_from(snapshot.scrollback_rows.len()).unwrap_or(u64::MAX),
                            ),
                            cached_history_bytes: Some(
                                u64::try_from(history_cache_bytes(&snapshot.scrollback_rows))
                                    .unwrap_or(u64::MAX),
                            ),
                            copied_history_rows: Some(
                                u64::try_from(previous_rows.len()).unwrap_or(u64::MAX),
                            ),
                            copied_history_bytes: trace_copied_history_bytes
                                .map(|value| u64::try_from(value).unwrap_or(u64::MAX)),
                            ..PerfTraceEvent::default()
                        },
                    );
                }
                Ok(if frame_dirty {
                    BackgroundUpdateImpact::FRAME
                } else {
                    BackgroundUpdateImpact::VISUAL
                })
            }
            WindowUpdate::Authority(authority) => {
                self.authority = authority;
                Ok(BackgroundUpdateImpact::VISUAL)
            }
            WindowUpdate::Control(active) => {
                self.controller_active = active;
                Ok(BackgroundUpdateImpact::VISUAL)
            }
            WindowUpdate::ControlTransferRequested(transfer_id) => {
                self.pending_control_transfer = Some(transfer_id);
                Ok(BackgroundUpdateImpact::VISUAL)
            }
            WindowUpdate::ControlTransferResolved(_) => {
                self.pending_control_transfer = None;
                Ok(BackgroundUpdateImpact::VISUAL)
            }
            WindowUpdate::SearchResults(page) => {
                // Ctrl+N continues once the pane has focus again.
                let _ = self.search.take_page(page);
                self.search.searching = false;
                Ok(BackgroundUpdateImpact::VISUAL)
            }
            WindowUpdate::SearchResyncRequired => {
                self.search.resync_required();
                Ok(BackgroundUpdateImpact::VISUAL)
            }
            WindowUpdate::ScrollbackResyncRequired => {
                self.history_page_pending = false;
                self.clear_local_content_state();
                Ok(BackgroundUpdateImpact::VISUAL)
            }
            WindowUpdate::ScrollbackPages(pages) => Ok(if self.apply_background_pages(pages)? {
                BackgroundUpdateImpact::FRAME
            } else {
                BackgroundUpdateImpact::NONE
            }),
            WindowUpdate::Theme(_) | WindowUpdate::Font(_) => Ok(BackgroundUpdateImpact::NONE),
            WindowUpdate::Exited { .. } | WindowUpdate::Shutdown => {
                self.controller_active = false;
                self.commands = None;
                self.updates = None;
                Ok(BackgroundUpdateImpact::VISUAL)
            }
        }
    }
}

#[derive(Default)]
struct InactiveUpdateDrain {
    changed: bool,
    theme: Option<ThemeUpdate>,
    dirty_frames: HashSet<SplintId>,
    repaint_panes: HashSet<SplintId>,
    exited: Vec<SplintId>,
}

impl InactiveUpdateDrain {
    fn record(&mut self, splint_id: Option<SplintId>, impact: BackgroundUpdateImpact) {
        self.changed |= impact.visual_changed;
        if let Some(splint_id) = splint_id {
            if impact.frame_dirty {
                self.dirty_frames.insert(splint_id);
            }
            if impact.visual_changed {
                self.repaint_panes.insert(splint_id);
            }
        }
    }

    fn panes_to_repaint(&self) -> &HashSet<SplintId> {
        // Cursor-only updates refresh prepared rows in place. They still need
        // pixels when a peer rebuild selects incremental inactive composition.
        &self.repaint_panes
    }
}

fn apply_inactive_update_batch(
    pane: &mut PaneView,
    updates: impl IntoIterator<Item = WindowUpdate>,
    theme: ResolvedTheme,
) -> Result<BackgroundUpdateImpact> {
    let mut batch = BackgroundUpdateImpact::NONE;
    for update in updates {
        let impact = pane.apply_background_update(update, theme, "visible-inactive")?;
        batch.visual_changed |= impact.visual_changed;
        batch.frame_dirty |= impact.frame_dirty;
    }
    Ok(batch)
}

#[cfg(test)]
fn rebuild_inactive_frames(
    panes: &mut [PaneView],
    dirty: &HashSet<SplintId>,
    rebuild_all: bool,
    scale_120: u32,
) -> Result<usize> {
    let mut rebuilt = 0;
    for pane in panes {
        let selected = rebuild_all
            || pane
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| dirty.contains(&snapshot.splint_id));
        if selected && rebuild_pane_scaled_frame(pane, scale_120)? {
            rebuilt += 1;
        }
    }
    Ok(rebuilt)
}

fn rebuild_inactive_frames_with_context(
    panes: &mut [PaneView],
    dirty: &HashSet<SplintId>,
    rebuild_all: bool,
    scale_120: u32,
    context: &RenderContext,
) -> Result<usize> {
    let mut rebuilt = 0;
    for pane in panes {
        let selected = rebuild_all
            || pane
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| dirty.contains(&snapshot.splint_id));
        if selected && rebuild_pane_scaled_frame_with_context(pane, scale_120, context)? {
            rebuilt += 1;
        }
    }
    Ok(rebuilt)
}

struct PreparedTabFontFrames {
    focused: Option<SnapshotFrame>,
    inactive: Vec<Option<SnapshotFrame>>,
}

struct CachedFrameTitle {
    source: ChromeSource,
    maximum_cells: u32,
    scale_120: u32,
    bold: bool,
    text: ChromeText,
}

/// `((scale_120, terminal cell height, renderer generation), (chrome cell
/// width, chrome cell height))` for frame titles.
type FrameTitleCell = ((u32, u32, u64), (u32, u32));

/// A frame title at the chrome size, never taller than the terminal cell of
/// the top border row it is drawn in.
fn load_frame_title(
    source: &ChromeSource,
    scale_120: u32,
    context: &RenderContext,
    terminal_cell_height: u32,
) -> Result<ChromeText> {
    ChromeText::load_source_styled_within(
        source,
        scale_120,
        ChromeTextStyle::Regular,
        context,
        terminal_cell_height,
    )
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent Wayland lifecycle and evidence-mode flags are not one state machine"
)]
struct ShmFrameBuffer {
    buffer: Buffer,
    stale: BackingDamage,
}

struct PlatformState {
    registry_state: RegistryState,
    compositor: CompositorState,
    seat_state: SeatState,
    output_state: OutputState,
    data_device_manager: DataDeviceManagerState,
    primary_selection_manager: Option<PrimarySelectionManagerState>,
    text_input_manager: Option<ZwpTextInputManagerV3>,
    shm: Shm,
    loop_handle: LoopHandle<'static, App>,
    update_waker: Waker,
    output_count: usize,
    /// Enter order is significant: the last element is Foot's most-recent output.
    entered_outputs: Vec<wl_output::WlOutput>,
    seat_count: usize,
}

struct SurfaceState {
    fractional_scale: Option<WpFractionalScaleV1>,
    viewport: Option<WpViewport>,
    background_effect_manager: Option<ExtBackgroundEffectManagerV1>,
    background_effect: Option<ExtBackgroundEffectSurfaceV1>,
    background_effect_state: BackgroundEffectState,
    background_effect_deferred_commit: Option<BackgroundCommitReason>,
    background_effect_reconcile_schedule: BackgroundEffectReconcileSchedule,
    background_effect_capabilities_received: bool,
    background_effect_trace: bool,
    window: Window,
    pool: SlotPool,
    buffers: Vec<ShmFrameBuffer>,
    backing: Vec<u8>,
    logical_width: u32,
    logical_height: u32,
    configured: bool,
    scale_120: u32,
    integer_fallback_scale: u32,
    /// Signals the Window's owner once the first buffer commit maps it.
    mapped_notify: Option<tokio::sync::oneshot::Sender<()>>,
}

/// The Window's fixed title settings from its options.
struct TitleSettings {
    /// Replaces the terminal's own title when set.
    text: Option<String>,
    /// Whether the title may say "local controller"; input gating ignores it.
    controller_label: bool,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent cursor-blink, tab-strip border, evidence-shortcut and redraw flags"
)]
struct PresentationState {
    render_context: RenderContext,
    text_row: Option<TextRow>,
    renderer_generation: u64,
    theme_generation: u64,
    pending_font_update: Option<FontUpdate>,
    /// The followed Omarchy text size the render context last applied. A theme
    /// whose text size differs is reconciled through the font path.
    applied_text_size_px: Option<u16>,
    /// A followed size the render context rejected, with the zoom steps then.
    rejected_text_size: Option<(Option<u16>, i16)>,
    /// `[main] ui-font-pixelsize`, which configuration reload replaces. With
    /// the theme's Omarchy text size it decides the chrome text size
    /// ([`chrome_font_size`]).
    ui_font_size: Option<FontSize>,
    cursor_style: CursorStyle,
    cursor_blink: bool,
    /// `[multiplexer] tab-strip-border`, which configuration reload replaces.
    tab_strip_border: bool,
    /// `[explorer] full-height`, which configuration reload replaces.
    explorer_full_height: bool,
    /// `[explorer] collapsed` as last loaded. A reload that changes it
    /// collapses or expands the Explorer; one that keeps it leaves the
    /// Window's own toggles alone.
    explorer_collapsed: bool,
    /// One regular chrome cell's width in buffer px, keyed by the renderer
    /// generation and scale it was measured at: the collapsed rail's width
    /// follows it.
    explorer_chrome_cell: std::cell::Cell<Option<((u64, u32), u32)>>,
    title: TitleSettings,
    theme: ResolvedTheme,
    /// This Window's `config.ini` `[colors]` keys, which configuration
    /// reload replaces. Every theme the Window shows is resolved with them.
    theme_colors: WindowThemeColors,
    pane_divider_style: PaneDividerStyle,
    /// `[multiplexer] pane-gap` in logical px, which configuration reload
    /// replaces.
    pane_gap: u32,
    frame_title_mode: FrameTitleMode,
    frame_titles: HashMap<SplintId, CachedFrameTitle>,
    /// The chrome cell (width, height) frame titles are drawn with, keyed by
    /// scale, terminal cell height and renderer generation.
    frame_title_cell: Option<FrameTitleCell>,
    evidence_close_shortcuts: bool,
    font_zoom_steps: i16,
    zoomed_splint: Option<SplintId>,
    capture: Option<PathBuf>,
    capture_scale: Option<u32>,
    initial_columns: u16,
    explorer_layout: Option<LairExplorerLayout>,
    explorer_text_cache: SessionPickerTextCache,
    search_text_cache: SessionPickerTextCache,
    /// Where the last committed frame drew the terminal search field, in
    /// buffer pixels; the next frame damages it if the field closes or moves.
    painted_search_panel: Option<Rect>,
    explorer_visible_start: usize,
    /// The agents section's first row in view, kept as the tree's start is.
    explorer_agents_start: usize,
    explorer_pressed: Option<ExplorerPointerPress>,
    full_redraw: bool,
}

#[derive(Clone, Copy, Debug)]
struct DividerDrag {
    split: PaneSplit,
    ratio: Option<SplitRatio>,
    /// The split's ratio before the drag's first preview, put back when the
    /// final ratio cannot be sent.
    original: Option<SplitRatio>,
}

/// Puts a dragged split back to its ratio from before the drag. Returns
/// whether the layout changed.
fn restore_divider_ratio(layout: Option<&mut LayoutNode>, drag: &DividerDrag) -> bool {
    let (Some(layout), Some(original)) = (layout, drag.original) else {
        return false;
    };
    match split_ratio_mut(layout, drag.split.target, drag.split.ancestor) {
        Some(current) if *current != original => {
            *current = original;
            true
        }
        _ => false,
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent keyboard, IME, focus-reporting, and cursor-blink protocol flags"
)]
struct InputState {
    keymap: ResolvedKeymap,
    prefix_state: PrefixState,
    prefix_timeout: Duration,
    text_input: Option<ZwpTextInputV3>,
    text_input_seat: Option<wl_seat::WlSeat>,
    ime: ImeState,
    reduced_motion: bool,
    keyboard_focused: bool,
    graphical_focus: Option<tokio::sync::watch::Sender<Option<SplintId>>>,
    forced_control_transfer: bool,
    optimistic_remote_splits: bool,
    input_generation: u64,
    pending_terminal_input: VecDeque<PendingTerminalInput>,
    terminal_input_saturated: bool,
    terminal_focus_reported: bool,
    ime_generation: u64,
    ime_modal_barrier: bool,
    modifiers: Modifiers,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    keyboard_seat: Option<wl_seat::WlSeat>,
    pointer: Option<wl_pointer::WlPointer>,
    pointer_seat: Option<wl_seat::WlSeat>,
    last_pointer_serial: Option<u32>,
    pressed_buttons: HashMap<u32, PressOwner>,
    divider_drag: Option<DividerDrag>,
    vertical_wheel: WheelAccumulator,
    scrollback_wheel: WheelAccumulator,
    cursor_blink_visible: bool,
    last_cursor_blink: Instant,
    /// Keys held down now, by raw code, so a staged Window can carry them
    /// into its next phase.
    pressed_keys: HashSet<u32>,
    /// Keys held across a phase switch or pressed while parked: their
    /// repeats are ignored until they are released.
    carried_keys: HashSet<u32>,
}

struct ClipboardState {
    image_directory: Option<PathBuf>,
    local_endpoint: bool,
    image_request: Option<clipboard_image::ImageRequest>,
    palette_image_target: Option<FileDropTarget>,
    data_device: Option<DataDevice>,
    primary_device: Option<PrimarySelectionDevice>,
    clipboard_offer: Option<SelectionOffer>,
    primary_offer: Option<PrimarySelectionOffer>,
    drag_target: Option<FileDropTarget>,
    clipboard_sources: Vec<(CopyPasteSource, Arc<[u8]>)>,
    primary_sources: Vec<(PrimarySelectionSource, Arc<[u8]>)>,
    clipboard_tx: StdSender<ClipboardRead>,
    clipboard_rx: StdReceiver<ClipboardRead>,
}

struct PanesState {
    pane: PaneView,
    inactive_panes: Vec<PaneView>,
    layout: Option<LayoutNode>,
    restored_frontend_needs_resize: bool,
    pending_exited_splints: HashSet<SplintId>,
    pending_remote_splits: HashMap<SplintId, SplintId>,
    dirty_inactive_panes: HashSet<SplintId>,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent modal lifecycle and redraw flags span distinct trusted overlays"
)]
struct ModalState {
    trusted_consent: Option<TrustedConsentUi>,
    /// The trusted dialog's fixed size and current layout.
    trusted_layout: chrome::TrustedDialogLayout,
    copy_mode: Option<CopyModeState>,
    binding_help: Option<BindingHelpUi>,
    /// The **Agent access** list, shown in the open command palette.
    agent_access: Option<AgentAccessUi>,
    command_palette: Option<CommandPaletteUi>,
    command_palette_layout: Option<CommandPaletteLayout>,
    command_palette_pressed: Option<BuiltInCommandId>,
    command_palette_text_cache: CommandPaletteTextCache,
    command_palette_open_focus: Option<bool>,
    command_palette_reconcile_pending: bool,
    dojo_prompt: Option<DojoPromptUi>,
    dojo_prompt_explorer_target: Option<crate::ExplorerContextTarget>,
    dojo_prompt_layout: Option<DojoPromptLayout>,
    dojo_prompt_pressed: Option<TerminationDecision>,
    dojo_prompt_text_cache: CommandPaletteTextCache,
    tab_context_menu: Option<TabContextMenuUi>,
    tab_context_menu_anchor: (u32, u32),
    tab_context_menu_layout: Option<TabContextMenuLayout>,
    tab_context_menu_pressed: Option<TabMenuActionId>,
    tab_context_menu_retarget: Option<TabMenuRightPress>,
    tab_context_menu_text_cache: CommandPaletteTextCache,
    session_picker: Option<SessionPickerUi>,
    selector_kind: Option<SelectorKind>,
    session_picker_targets: Vec<SessionPickerTarget>,
    session_picker_creation_target: Option<SessionPickerCreationTarget>,
    session_picker_layout: Option<SessionPickerOverlayLayout>,
    session_picker_pressed: Option<PickerHitTarget>,
    session_picker_text_cache: SessionPickerTextCache,
    session_picker_wheel: WheelAccumulator,
    session_picker_consumed_keys: HashSet<u32>,
    session_picker_redraw: bool,
    session_picker_reconcile_pending: bool,
    session_picker_open_focus: Option<bool>,
    session_picker_requested: bool,
    session_picker_retry_command: Option<WindowTopologyCommand>,
    deferred_picker_theme: Option<ThemeUpdate>,
    /// The profile names behind the command palette's remote pop-up, while it
    /// is the open inline picker.
    remote_profiles: Option<Vec<String>>,
}

#[derive(Clone, Copy, Debug)]
struct PaneCommitTrace {
    splint_id: SplintId,
    incarnation: u64,
    correlation: PerfTraceCorrelation,
    pane_role: &'static str,
    superseded_revisions: u64,
}

#[derive(Clone, Copy, Debug)]
struct PendingFrameTrace {
    commit_sequence: u64,
    committed_monotonic_raw_ns: u64,
}

fn pending_pane_commit_traces(focused: &PaneView, inactive: &[PaneView]) -> Vec<PaneCommitTrace> {
    let mut traces = Vec::with_capacity(inactive.len().saturating_add(1));
    traces.extend(focused.pending_commit_trace());
    traces.extend(inactive.iter().filter_map(PaneView::pending_commit_trace));
    traces
}

fn pane_commit_event(trace: PaneCommitTrace, commit_sequence: u64) -> PerfTraceEvent {
    PerfTraceEvent {
        splint_id: Some(trace.splint_id),
        incarnation: Some(trace.incarnation),
        base_revision: Some(trace.correlation.base_revision),
        revision: Some(trace.correlation.revision),
        subscription_id: Some(trace.correlation.subscription_id),
        transaction_sequence: Some(trace.correlation.transaction_sequence),
        commit_sequence: Some(commit_sequence),
        pane_role: Some(trace.pane_role),
        superseded_revisions: Some(trace.superseded_revisions),
        ..PerfTraceEvent::default()
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent lifecycle, frame-callback, redraw-priority, and trace flags"
)]
struct SchedulingState {
    signoff: Option<SignoffProbe>,
    graphical_input_probe: Option<GraphicalInputProbe>,
    scroll_trace: bool,
    exit: bool,
    failure: Option<anyhow::Error>,
    frame_pending: bool,
    redraw_pending: bool,
    terminal_redraw_pending: bool,
    next_commit_sequence: u64,
    pending_frame_trace: Option<PendingFrameTrace>,
    /// Whether a session-picker decision or the status Splint's exit parks
    /// the Window for its next phase instead of closing it.
    keep_surface: bool,
    /// The exit that parked a staged Window; its input is discarded.
    parked: Option<ExitClass>,
    /// An action was dropped because the topology manager was busy; the
    /// Window says so in its title on its next update.
    busy_notice: bool,
}

struct App {
    platform: PlatformState,
    surface: SurfaceState,
    presentation: PresentationState,
    input: InputState,
    clipboard: ClipboardState,
    panes: PanesState,
    explorer: LairExplorerUi,
    accessibility: navigation_accessibility::WindowAccessibility,
    access: agent_access::WindowAccess,
    tab_state: TabsState,
    modal: ModalState,
    scheduling: SchedulingState,
    handoff: HandoffFrontend,
}

impl PlatformState {
    fn output_dpi_observation(&self, output: &wl_output::WlOutput) -> OutputDpiObservation {
        let Some(info) = self.output_state.info(output) else {
            return OutputDpiObservation::unavailable("output-info-pending");
        };
        let current_mode = info
            .modes
            .iter()
            .find(|mode| mode.current)
            .map(|mode| mode.dimensions);
        OutputDpiObservation::from_wayland(info.id, info.name, current_mode, info.physical_size)
    }
}

impl PanesState {
    fn focused_splint(&self) -> Option<SplintId> {
        self.pane
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.splint_id)
    }

    fn display_snapshot(&self) -> Option<TerminalSnapshot> {
        self.pane.display_snapshot()
    }

    fn display_snapshot_cow(&self) -> Option<Cow<'_, TerminalSnapshot>> {
        self.pane.display_snapshot_cow()
    }

    fn input_modes(&self) -> TerminalInputModes {
        self.pane.snapshot.as_ref().map_or(
            TerminalInputModes {
                application_cursor: false,
                application_keypad: false,
                focus_reporting: false,
                bracketed_paste: false,
                cursor_visible: true,
                cursor_blink: true,
                mouse_tracking: MouseTracking::None,
                sgr_mouse: false,
            },
            |snapshot| snapshot.input_modes,
        )
    }
}

impl ModalState {
    fn inline_picker_open(&self) -> bool {
        self.session_picker
            .as_ref()
            .is_some_and(SessionPickerUi::is_inline)
    }

    fn input_modal_open(&self) -> bool {
        self.copy_mode.is_some()
            || self.binding_help.is_some()
            || self.inline_picker_open()
            || self.command_palette.is_some()
            || self.dojo_prompt.is_some()
            || self.tab_context_menu.is_some()
    }
}

impl SchedulingState {
    fn request_exit(&mut self, exit_class: ExitClass) {
        if self.keep_surface && phase_end_parks(exit_class) {
            // The Window lives on into its next phase, which records its own exit.
            self.parked = Some(exit_class);
            self.exit = true;
            return;
        }
        if let Some(diagnostics) = diagnostics() {
            diagnostics.request_exit(exit_class);
        }
        self.exit = true;
    }

    /// Records an action dropped because the topology command queue was full,
    /// and asks for the title notice on the next update turn.
    fn note_busy(&mut self, error: &anyhow::Error) {
        if let Some(diagnostics) = diagnostics() {
            diagnostics.emit(
                DiagnosticLevel::Warn,
                DiagnosticEventCode::TopologyFailure,
                Some(DiagnosticErrorCode::TopologyManager),
            );
        }
        eprintln!("splinterm: an action was not sent: {error:#}");
        self.busy_notice = true;
    }

    /// Ends the Window over `error`. The diagnostic records the caller's
    /// source location, so a failure the log cannot describe still names the
    /// path that reported it.
    #[track_caller]
    fn fail(&mut self, error: anyhow::Error) {
        let location = std::panic::Location::caller();
        if error.downcast_ref::<TopologyQueueFull>().is_some() {
            // A busy topology manager is not a broken Window: the action is
            // dropped and the title says so. `note_busy` already logged it
            // where the send failed.
            self.busy_notice = true;
            return;
        }
        if let Some(diagnostics) = diagnostics() {
            diagnostics.emit_failure(
                DiagnosticLevel::Error,
                DiagnosticEventCode::WaylandFailure,
                Some(DiagnosticErrorCode::WaylandDispatch),
                location,
            );
        }
        eprintln!("splinterm Wayland client failed at {location}: {error:#}");
        self.failure = Some(error);
        self.request_exit(ExitClass::ErrorWaylandDispatch);
    }
}

/// Whether a batch of older history pages may be joined to `snapshot`'s cache.
///
/// History rows never change within one generation, so a page read at another
/// terminal revision still holds the right rows, but it joins the cache only if
/// the cache still starts at `requested_top`, the row the pages were read
/// before. Without that, a cache replaced in the meantime would get a seam.
fn pages_join_history(
    pages: &[splinterm_protocol::ScrollbackPage],
    snapshot: &TerminalSnapshot,
    requested_top: Option<u64>,
) -> bool {
    let same_history = pages.iter().all(|page| {
        page.splint_id == snapshot.splint_id
            && page.incarnation == snapshot.incarnation
            && page.history_generation == snapshot.history_generation
    });
    let cache_top = snapshot.scrollback_rows.first().and_then(|row| row.row_id);
    let same_top = cache_top.is_some() && cache_top == requested_top;
    let same_revision = pages
        .iter()
        .all(|page| page.terminal_revision == snapshot.revision);
    same_history && (same_top || same_revision)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HistoryKey {
    Navigate(HistoryNavigation),
    /// Handled with nothing to do; the terminal never receives it.
    Consume,
    Forward,
}

/// What a history shortcut does. Return to live while already live on the
/// normal screen is consumed, so a shell never receives Shift+End and inserts
/// a stray character; a full-screen application still receives it.
fn history_key(action: Option<ActionId>, live: bool, screen: Option<ActiveScreen>) -> HistoryKey {
    match action {
        Some(ActionId::PageUp) => HistoryKey::Navigate(HistoryNavigation::PageUp),
        Some(ActionId::PageDown) => HistoryKey::Navigate(HistoryNavigation::PageDown),
        Some(ActionId::ReturnToLive) if !live => {
            HistoryKey::Navigate(HistoryNavigation::ReturnToLive)
        }
        Some(ActionId::ReturnToLive) if screen == Some(ActiveScreen::Normal) => HistoryKey::Consume,
        _ => HistoryKey::Forward,
    }
}

fn try_window_command(commands: &Sender<WindowCommand>, command: WindowCommand) -> Result<()> {
    commands.try_send(command).map_err(|error| match error {
        TrySendError::Full(_) => anyhow::anyhow!("Wayland command queue overflow"),
        TrySendError::Closed(_) => anyhow::anyhow!("Wayland command receiver disconnected"),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalInputQueueOutcome {
    Queued,
    Pending,
    Saturated,
}

struct PendingTerminalInput {
    commands: Sender<WindowCommand>,
    bytes: Vec<u8>,
}

fn pending_terminal_input_bytes(pending: &VecDeque<PendingTerminalInput>) -> usize {
    pending.iter().map(|batch| batch.bytes.len()).sum()
}

fn terminal_input_pending_for(
    pending: &VecDeque<PendingTerminalInput>,
    commands: Option<&Sender<WindowCommand>>,
) -> bool {
    commands.is_some_and(|commands| {
        pending
            .iter()
            .any(|batch| batch.commands.same_channel(commands))
    })
}

fn try_flush_pending_terminal_input(pending: &mut VecDeque<PendingTerminalInput>) -> bool {
    let batch_count = pending.len();
    for _ in 0..batch_count {
        let batch = pending.pop_front().expect("pending batch count is exact");
        match batch.commands.try_send(WindowCommand::Input(batch.bytes)) {
            Err(TrySendError::Full(WindowCommand::Input(bytes))) => {
                pending.push_back(PendingTerminalInput {
                    commands: batch.commands,
                    bytes,
                });
            }
            // Pane teardown can close a sender while its input is pending. The
            // bytes remain bound to that pane and must not terminate the window
            // or be retargeted to another pane.
            Ok(()) | Err(TrySendError::Closed(_)) => {}
            Err(TrySendError::Full(_)) => unreachable!("queued command remains terminal input"),
        }
    }
    pending.is_empty()
}

fn retain_pending_terminal_input(
    pending: &mut VecDeque<PendingTerminalInput>,
    commands: &Sender<WindowCommand>,
    bytes: Vec<u8>,
) -> TerminalInputQueueOutcome {
    let available =
        MAX_PENDING_TERMINAL_INPUT_BYTES.saturating_sub(pending_terminal_input_bytes(pending));
    if bytes.len() > available {
        return TerminalInputQueueOutcome::Saturated;
    }
    if let Some(batch) = pending
        .iter_mut()
        .find(|batch| batch.commands.same_channel(commands))
    {
        batch.bytes.extend(bytes);
    } else if !bytes.is_empty() {
        pending.push_back(PendingTerminalInput {
            commands: commands.clone(),
            bytes,
        });
    }
    TerminalInputQueueOutcome::Pending
}

fn queue_terminal_input(
    commands: Option<&Sender<WindowCommand>>,
    pending: &mut VecDeque<PendingTerminalInput>,
    bytes: Vec<u8>,
) -> Result<TerminalInputQueueOutcome> {
    let Some(commands) = commands else {
        return Ok(TerminalInputQueueOutcome::Queued);
    };
    if terminal_input_pending_for(pending, Some(commands)) {
        let _ = try_flush_pending_terminal_input(pending);
        if terminal_input_pending_for(pending, Some(commands)) {
            return Ok(retain_pending_terminal_input(pending, commands, bytes));
        }
    }
    match commands.try_send(WindowCommand::Input(bytes)) {
        Ok(()) => Ok(TerminalInputQueueOutcome::Queued),
        Err(TrySendError::Full(WindowCommand::Input(bytes))) => {
            Ok(retain_pending_terminal_input(pending, commands, bytes))
        }
        Err(TrySendError::Closed(_)) => {
            Err(anyhow::anyhow!("Wayland command receiver disconnected"))
        }
        Err(TrySendError::Full(_)) => unreachable!("queued command remains terminal input"),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ControlReleaseOutcome {
    Queued,
    Retry,
    Disconnected,
}

fn try_queue_control_release(commands: &Sender<WindowCommand>) -> ControlReleaseOutcome {
    match commands.try_send(WindowCommand::ReleaseControl) {
        Ok(()) => ControlReleaseOutcome::Queued,
        Err(TrySendError::Full(_)) => ControlReleaseOutcome::Retry,
        Err(TrySendError::Closed(_)) => ControlReleaseOutcome::Disconnected,
    }
}

fn try_queue_focus_report(
    commands: &Sender<WindowCommand>,
    focused: bool,
) -> ControlReleaseOutcome {
    match commands.try_send(WindowCommand::FocusReport(focused)) {
        Ok(()) => ControlReleaseOutcome::Queued,
        Err(TrySendError::Full(_)) => ControlReleaseOutcome::Retry,
        Err(TrySendError::Closed(_)) => ControlReleaseOutcome::Disconnected,
    }
}

fn terminal_update_has_visual_damage(
    full: bool,
    cursor_changed: bool,
    raster_dirty_rows: &[bool],
    surface_dirty_rows: &[bool],
) -> bool {
    full || cursor_changed
        || raster_dirty_rows.iter().any(|dirty| *dirty)
        || surface_dirty_rows.iter().any(|dirty| *dirty)
}

fn propagate_raster_damage_through_scroll(
    dirty_rows: &mut [bool],
    scroll: &splinterm_protocol::TerminalScroll,
) {
    let end = scroll.end_row.min(dirty_rows.len());
    let count = scroll.rows.min(end.saturating_sub(scroll.start_row));
    if count == 0 || scroll.start_row >= end || count >= end - scroll.start_row {
        return;
    }
    match scroll.direction {
        splinterm_protocol::ScrollDirection::Forward => {
            for row in scroll.start_row + count..end {
                if dirty_rows[row] {
                    dirty_rows[row - count] = true;
                }
            }
        }
        splinterm_protocol::ScrollDirection::Reverse => {
            for row in (scroll.start_row..end - count).rev() {
                if dirty_rows[row] {
                    dirty_rows[row + count] = true;
                }
            }
        }
    }
}

/// Sends a command the Window can do without: a resynchronization, a
/// history page or a search. When the queue is full the bridge is behind,
/// for example while it delivers a flood of output, and the command is
/// dropped instead of ending the Window. Returns whether it was sent.
fn try_advisory_command(commands: &Sender<WindowCommand>, command: WindowCommand) -> Result<bool> {
    match commands.try_send(command) {
        Ok(()) => Ok(true),
        Err(TrySendError::Full(_)) => Ok(false),
        Err(TrySendError::Closed(_)) => {
            Err(anyhow::anyhow!("Wayland command receiver disconnected"))
        }
    }
}

/// Asks for an authoritative snapshot when a viewport reaches live, unless
/// one is already on its way. A dropped request is harmless: a live view
/// already drops history that lags, and the next return to live asks again.
fn request_return_live_resync(
    commands: Option<&Sender<WindowCommand>>,
    pending: &mut bool,
    previous_offset: usize,
    current_offset: usize,
) -> Result<bool> {
    if previous_offset == 0 || current_offset != 0 || *pending {
        return Ok(false);
    }
    let Some(commands) = commands else {
        return Ok(false);
    };
    *pending = try_advisory_command(commands, WindowCommand::Resynchronize)?;
    Ok(*pending)
}

/// The topology manager is still busy with earlier commands, for example
/// while "Terminate Dojo…" waits for a Splint to exit after SIGHUP. The
/// Window is fine; only this one action was not sent.
#[derive(Debug)]
struct TopologyQueueFull;

impl std::fmt::Display for TopologyQueueFull {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("topology command queue is full")
    }
}

impl std::error::Error for TopologyQueueFull {}

fn try_topology_command(
    commands: &Sender<WindowTopologyCommand>,
    command: WindowTopologyCommand,
) -> Result<()> {
    commands.try_send(command).map_err(|error| match error {
        TrySendError::Full(_) => anyhow::Error::new(TopologyQueueFull),
        TrySendError::Closed(_) => anyhow::anyhow!("topology command queue is closed"),
    })
}

/// Publishes the settings the topology manager reads after an accepted
/// configuration reload. This is shared state, not a queued command: it never
/// takes a command-queue slot, and a busy manager still sees the latest value
/// at its next decision. A Window without a manager has nothing to update.
fn publish_topology_settings(
    settings: Option<&tokio::sync::watch::Sender<crate::config::TopologyReloadSettings>>,
    config: &crate::config::AppConfig,
) {
    if let Some(settings) = settings {
        settings.send_replace(crate::config::TopologyReloadSettings::from_config(config));
    }
}

fn try_topology_command_with_rollback(
    commands: Option<&Sender<WindowTopologyCommand>>,
    command: WindowTopologyCommand,
    rollback: impl FnOnce(SplintId, SplintId) -> Result<()>,
) -> Result<()> {
    let pending_split = match &command {
        WindowTopologyCommand::Split {
            target,
            pending: Some(pending),
            ..
        } => Some((*target, *pending)),
        _ => None,
    };
    let result = commands
        .context("topology command queue is unavailable")
        .and_then(|commands| try_topology_command(commands, command));
    if let Err(error) = result {
        if let Some((target, pending)) = pending_split {
            rollback(target, pending)
                .context("failed to roll back unsent remote split placeholder")?;
        }
        return Err(error);
    }
    Ok(())
}

fn apply_ime_preedit(snapshot: &mut TerminalSnapshot, text: Option<&str>) -> Option<usize> {
    let row = usize::try_from(snapshot.cursor_row).ok()?;
    if row >= snapshot.rows {
        return None;
    }
    if let Some(text) = text {
        let mut column = usize::try_from(snapshot.cursor_column.max(0)).unwrap_or(0);
        let mut leader: Option<usize> = None;
        for character in text.chars() {
            let width = UnicodeWidthChar::width(character).unwrap_or(1).min(2);
            if width == 0 {
                if let Some(leader) = leader
                    && let Some(cell) = snapshot.visible_rows[row].cells.get_mut(leader)
                {
                    cell.content.push(character);
                }
                continue;
            }
            if column >= snapshot.columns || column + width > snapshot.columns {
                break;
            }
            if let Some(cell) = snapshot.visible_rows[row].cells.get_mut(column) {
                cell.content = character.to_string();
                cell.spacer_remaining = None;
            }
            leader = Some(column);
            if width == 2
                && let Some(spacer) = snapshot.visible_rows[row].cells.get_mut(column + 1)
            {
                spacer.content.clear();
                spacer.spacer_remaining = Some(1);
            }
            column += width;
        }
    }
    Some(row)
}

/// `modal_open` covers input modals, a trusted consent prompt, and a
/// non-inline session picker. Beneath any of them IME is allowed only into
/// the owned field, so composed text never reaches the PTY.
const fn terminal_ime_allowed(
    keyboard_focused: bool,
    modal_open: bool,
    explorer_focused: bool,
    owned_target: Option<OwnedFieldTarget>,
) -> bool {
    keyboard_focused && ((!modal_open && !explorer_focused) || owned_target.is_some())
}

const fn ime_batch_blocked(
    modal_open: bool,
    explorer_focused: bool,
    barrier: bool,
    owned_target: Option<OwnedFieldTarget>,
) -> bool {
    barrier || ((modal_open || explorer_focused) && owned_target.is_none())
}

/// Window facts that decide where IME text may go.
#[derive(Clone, Copy, Debug, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one independent modal layer or owned field in the IME routing table"
)]
struct ImeContext {
    input_modal_open: bool,
    /// A trusted consent prompt or a non-inline session picker.
    consent_or_picker_open: bool,
    explorer_focused: bool,
    binding_help: bool,
    command_palette: bool,
    dojo_prompt_input: bool,
    terminal_search: bool,
    /// The Window is paused for a daemon handoff: no composition reaches the
    /// PTY, while owned fields keep composing (Plan 0061 M3.12a).
    terminal_input_paused: bool,
}

/// The single IME routing decision used by every IME gate in the Window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ImeGate {
    target: Option<OwnedFieldTarget>,
    modal_open: bool,
    explorer_focused: bool,
}

impl ImeGate {
    /// Whether the text input may be enabled for this keyboard focus.
    const fn allowed(self, keyboard_focused: bool) -> bool {
        terminal_ime_allowed(
            keyboard_focused,
            self.modal_open,
            self.explorer_focused,
            self.target,
        )
    }

    /// Whether an IME batch must be dropped before it reaches any field or
    /// the PTY.
    const fn batch_blocked(self, barrier: bool) -> bool {
        ime_batch_blocked(self.modal_open, self.explorer_focused, barrier, self.target)
    }
}

/// Owned-field ownership follows typed text, except that Explorer focus keeps
/// terminal and Explorer-search composition blocked. A consent prompt or a
/// non-inline picker counts as modal, so without an owned field IME stays
/// disabled and no batch reaches the PTY beneath it. A handoff pause blocks
/// terminal composition the same way, without hiding an owned field.
fn ime_gate(context: ImeContext) -> ImeGate {
    let target = App::select_owned_field(
        context.input_modal_open,
        context.consent_or_picker_open,
        context.binding_help.then_some(()),
        context.command_palette.then_some(()),
        context.dojo_prompt_input.then_some(()),
        None,
        (!context.explorer_focused && context.terminal_search).then_some(()),
    )
    .map(|(target, ())| target);
    ImeGate {
        target,
        modal_open: context.input_modal_open
            || context.consent_or_picker_open
            || context.terminal_input_paused,
        explorer_focused: context.explorer_focused,
    }
}

/// Typed, pasted, dropped, and composed bytes never reach the PTY behind an
/// input modal or a pending trusted consent prompt, or while the Window is
/// paused for a daemon handoff (Plan 0061 M3.12a), when they are dropped.
const fn terminal_input_blocked(
    input_modal_open: bool,
    consent_pending: bool,
    handoff_paused: bool,
) -> bool {
    input_modal_open || consent_pending || handoff_paused
}

fn explorer_pointer_returns_to_terminal(
    explorer_focused: bool,
    inside_panel: bool,
    event: &PointerEventKind,
) -> bool {
    explorer_focused
        && !inside_panel
        && matches!(
            event,
            &PointerEventKind::Press {
                button: BTN_LEFT,
                ..
            }
        )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExplorerPointerPress {
    Row(NavigationNodeId, bool),
    /// A row of the agents section, for its Splint.
    Agent(NavigationNodeId),
    Filter(LairExplorerFilter),
    /// A footer action token, or the rail header's `»`, which never takes
    /// keyboard focus.
    Action(LairExplorerAction),
}

fn explorer_released_filter(
    pressed: Option<ExplorerPointerPress>,
    target: Option<LairExplorerFilter>,
) -> Option<LairExplorerFilter> {
    match pressed {
        Some(ExplorerPointerPress::Filter(filter)) if Some(filter) == target => Some(filter),
        _ => None,
    }
}

/// The agents row a left release completes: one only when the press and the
/// release both land on the same row of the agents section.
fn explorer_released_agent(
    pressed: Option<ExplorerPointerPress>,
    agent: Option<NavigationNodeId>,
) -> Option<NavigationNodeId> {
    match pressed {
        Some(ExplorerPointerPress::Agent(pressed)) if Some(pressed) == agent => Some(pressed),
        _ => None,
    }
}

/// What a left press in the Explorer panel starts, and whether it gives the
/// Explorer the keys. A footer action token (`new`, `menu`, `«`) and the
/// rail header's `»` leave keyboard focus where it is, and so does a press
/// anywhere else `on_chrome`: on the footer, between the tokens or on the
/// context line, or on the agents section's label line, which are not
/// rows. Every other press in the panel, a row of the tree, of the agents
/// section, or of the rail alike, gives the Explorer the keys, as a row
/// click always has. `row` is the tree row pressed and the row whose
/// disclosure is under the press; `agent`, the agents row.
fn explorer_left_press(
    action: Option<LairExplorerAction>,
    on_chrome: bool,
    filter: Option<LairExplorerFilter>,
    (target, disclosure): (Option<NavigationNodeId>, Option<NavigationNodeId>),
    agent: Option<NavigationNodeId>,
) -> (Option<ExplorerPointerPress>, bool) {
    if let Some(action) = action {
        return (Some(ExplorerPointerPress::Action(action)), false);
    }
    if on_chrome {
        return (None, false);
    }
    let pressed = filter
        .map(ExplorerPointerPress::Filter)
        .or_else(|| {
            target.map(|target| ExplorerPointerPress::Row(target, disclosure == Some(target)))
        })
        .or_else(|| agent.map(ExplorerPointerPress::Agent));
    (pressed, true)
}

/// The action a left release completes: one only when the press and the
/// release both land on the same token.
fn explorer_released_action(
    pressed: Option<ExplorerPointerPress>,
    on: Option<LairExplorerAction>,
) -> Option<LairExplorerAction> {
    match (pressed, on) {
        (Some(ExplorerPointerPress::Action(pressed)), Some(on)) if pressed == on => Some(on),
        _ => None,
    }
}

/// What a completed footer action does. `menu` opens the selected row's own
/// menu, the one a right-click opens; with no row selected, or a row with
/// no menu, it opens the command palette (the Window's action menu). A
/// selected row whose menu is only unavailable for now, while the
/// navigation view refreshes, is stale, or is disconnected, leaves `menu`
/// doing nothing rather than opening something else.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExplorerActionCommand {
    Toggle,
    NewLair,
    RowMenu,
    Palette,
    Nothing,
}

/// Where `menu` stands for the selected row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExplorerRowMenu {
    /// No row is selected.
    NoSelection,
    /// The selected row has its menu.
    Ready,
    /// The selected row has nothing on its menu.
    Empty,
    /// The selected row's menu cannot be built until the view is current.
    Unavailable,
}

/// `row_menu` is asked only for `menu`.
fn explorer_action_command(
    action: LairExplorerAction,
    row_menu: impl FnOnce() -> ExplorerRowMenu,
) -> ExplorerActionCommand {
    match action {
        LairExplorerAction::Toggle => ExplorerActionCommand::Toggle,
        LairExplorerAction::New => ExplorerActionCommand::NewLair,
        LairExplorerAction::Menu => match row_menu() {
            ExplorerRowMenu::Ready => ExplorerActionCommand::RowMenu,
            ExplorerRowMenu::NoSelection | ExplorerRowMenu::Empty => ExplorerActionCommand::Palette,
            ExplorerRowMenu::Unavailable => ExplorerActionCommand::Nothing,
        },
    }
}

/// The rail's first row in view after a wheel step of `delta` rows from
/// `start`, with `capacity` of its `count` rows in view: never before the
/// first row or past the last screenful.
fn explorer_rail_scrolled_start(
    start: usize,
    capacity: usize,
    count: usize,
    delta: isize,
) -> usize {
    start
        .saturating_add_signed(delta)
        .min(count.saturating_sub(capacity))
}

fn explorer_filter_key(key: Keysym, modifiers: Modifiers, searching: bool) -> bool {
    !searching
        && !modifiers.ctrl
        && !modifiers.alt
        && !modifiers.logo
        && matches!(key, Keysym::f | Keysym::F)
}

fn explorer_context_menu_key(key: Keysym, modifiers: Modifiers) -> bool {
    !modifiers.ctrl
        && !modifiers.alt
        && !modifiers.logo
        && ((key == Keysym::Menu && !modifiers.shift) || (key == Keysym::F10 && modifiers.shift))
}

/// A matching context-menu release belongs to chrome even if its target or
/// admission state changed. Rejection is not a Wayland dispatch failure, and
/// the release must not fall through to terminal input.
fn consume_context_menu_request(open: impl FnOnce() -> Result<()>) -> bool {
    if let Err(error) = open() {
        eprintln!("splinterm context menu request ignored: {error:#}");
    }
    true
}

/// Run the local focus operation only for an eligible exact target, and verify
/// its postcondition. A no-op is successful only when the target is focused.
fn attempt_exact_splint_focus(
    splint_id: SplintId,
    incarnation_matches: bool,
    pending_remote: bool,
    attempt: impl FnOnce() -> (bool, Option<SplintId>),
) -> Option<bool> {
    if !incarnation_matches || pending_remote {
        return None;
    }
    let (changed, focused) = attempt();
    (focused == Some(splint_id)).then_some(changed)
}

const fn presented_cursor_visible(inline_picker_open: bool, blink_phase_visible: bool) -> bool {
    inline_picker_open || blink_phase_visible
}

fn restart_cursor_blink(
    focused_visual_changed: bool,
    visible: &mut bool,
    last_blink: &mut Instant,
) -> bool {
    if !focused_visual_changed {
        return false;
    }
    *visible = true;
    *last_blink = Instant::now();
    true
}

fn cursor_blink_enabled(reduced_motion: bool, focused: bool, modes: TerminalInputModes) -> bool {
    !reduced_motion && focused && modes.cursor_visible && modes.cursor_blink
}

fn event_loop_timeout(
    exiting: bool,
    signoff_active: bool,
    cursor_blink_elapsed: Option<Duration>,
) -> Option<Duration> {
    if exiting {
        None
    } else if signoff_active {
        Some(SIGNOFF_TICK_INTERVAL)
    } else if let Some(elapsed) = cursor_blink_elapsed {
        Some(CURSOR_BLINK_INTERVAL.saturating_sub(elapsed))
    } else {
        Some(IDLE_EVENT_LOOP_TIMEOUT)
    }
}

fn pane_command_retry_pending(pane: &PaneView) -> bool {
    pane.control_release_pending
        || pane.pending_focus_report.is_some()
        || pane.pending_resize.is_some()
}

fn command_retry_dispatch_timeout(
    timeout: Option<Duration>,
    command_retry_pending: bool,
) -> Option<Duration> {
    if command_retry_pending {
        timeout.map(|timeout| timeout.min(PENDING_INPUT_RETRY_INTERVAL))
    } else {
        timeout
    }
}

fn offset_cursor_rectangle(
    cursor: (i32, i32, i32, i32),
    pane: Rect,
) -> Option<(i32, i32, i32, i32)> {
    Some((
        cursor.0.checked_add(i32::try_from(pane.x).ok()?)?,
        cursor.1.checked_add(i32::try_from(pane.y).ok()?)?,
        cursor.2,
        cursor.3,
    ))
}

fn resize_changed(previous: Option<(u16, u16, u16, u16)>, candidate: (u16, u16, u16, u16)) -> bool {
    previous != Some(candidate)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalResizeCause {
    SnapshotAvailable,
    SurfaceConfigure,
    OutputDpiChanged,
    CompositorScaleChanged,
}

const fn terminal_resize_allowed(cause: TerminalResizeCause, resize_known: bool) -> bool {
    match cause {
        TerminalResizeCause::SnapshotAvailable => !resize_known,
        TerminalResizeCause::SurfaceConfigure => true,
        TerminalResizeCause::OutputDpiChanged | TerminalResizeCause::CompositorScaleChanged => {
            false
        }
    }
}

fn reduced_motion_requested() -> bool {
    std::env::var_os("SPLINTERM_REDUCED_MOTION")
        .is_some_and(|value| matches!(value.to_str(), Some("1" | "true" | "yes")))
}

fn search_paint_rect(content: Rect, explorer: Option<&LairExplorerLayout>) -> Option<Rect> {
    let Some(explorer) =
        explorer.filter(|layout| layout.mode == LairExplorerPresentationMode::Drawer)
    else {
        return Some(content);
    };
    let right = content.x.saturating_add(content.width);
    let left = explorer
        .panel
        .x
        .saturating_add(explorer.panel.width)
        .max(content.x);
    (left < right).then_some(Rect {
        x: left,
        width: right.saturating_sub(left),
        ..content
    })
}

/// The trusted dialog key a keysym is, if any.
const fn trusted_dialog_key(keysym: Keysym) -> TrustedDialogKey {
    match keysym {
        Keysym::g | Keysym::G => TrustedDialogKey::G,
        Keysym::d | Keysym::D => TrustedDialogKey::D,
        Keysym::r | Keysym::R => TrustedDialogKey::R,
        Keysym::Return | Keysym::KP_Enter => TrustedDialogKey::Enter,
        Keysym::Escape => TrustedDialogKey::Escape,
        Keysym::Up | Keysym::KP_Up => TrustedDialogKey::Up,
        Keysym::Down | Keysym::KP_Down => TrustedDialogKey::Down,
        Keysym::Left | Keysym::KP_Left => TrustedDialogKey::Left,
        Keysym::Right | Keysym::KP_Right => TrustedDialogKey::Right,
        _ => TrustedDialogKey::Other,
    }
}

/// The trusted dialog key one press is: the exact dialog shortcut engages;
/// any other key with Ctrl, Alt or Super held is inert, so the chord's
/// modifiers can never turn into an answer; otherwise the plain key counts.
fn trusted_dialog_press(
    keysym: Keysym,
    modifiers: Modifiers,
    shortcut: Option<splinterd::dialog_shortcut::DialogShortcut>,
) -> TrustedDialogKey {
    let character = u8::try_from(keysym.raw()).ok().map(char::from);
    if let (Some(shortcut), Some(character)) = (shortcut, character)
        && shortcut.matches(
            modifiers.ctrl,
            modifiers.alt,
            modifiers.shift,
            modifiers.logo,
            character,
        )
    {
        return TrustedDialogKey::Engage;
    }
    if modifiers.ctrl || modifiers.alt || modifiers.logo {
        return TrustedDialogKey::Other;
    }
    trusted_dialog_key(keysym)
}

/// The app ID a Window maps with. Every trusted dialog uses its own, so the
/// desktop can map it without keyboard focus. Splinterm never asks the
/// compositor to activate a Window.
fn window_app_id(trusted_dialog: bool, app_id: &str) -> &str {
    if trusted_dialog {
        crate::config::TRUSTED_DIALOG_APP_ID
    } else {
        app_id
    }
}

/// The title a Window maps with, before any snapshot or modal updates it.
fn initial_window_title(options: &WindowOptions) -> String {
    if let Some(consent) = &options.trusted_consent {
        consent.kind.window_title().to_owned()
    } else if let Some(picker) = &options.session_picker {
        picker.standalone_title().to_owned()
    } else {
        window_title(
            options.title.as_deref().or_else(|| {
                options
                    .snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.title.as_str())
            }),
            options.controlled && options.commands.is_some() && options.controller_title,
            options.local_endpoint,
            &options.authority,
            false,
            None,
        )
    }
}

/// `local_endpoint` is the Window's fixed endpoint authority: a remote Window
/// that holds control is "controller", never "local controller".
fn window_title(
    snapshot_title: Option<&str>,
    controller_active: bool,
    local_endpoint: bool,
    authority: &AuthorityStatus,
    control_transfer_pending: bool,
    search: Option<&SearchUiState>,
) -> String {
    let base = match snapshot_title.map(str::trim) {
        None | Some("" | "Splinterm") => "Splinterm".to_owned(),
        Some(title) => format!("Splinterm — {title}"),
    };
    let controller = controller_active.then_some(if local_endpoint {
        "local controller"
    } else {
        "controller"
    });
    let authority_label = if authority.development_bypass {
        Some("DEVELOPMENT BYPASS")
    } else if authority.grants.is_empty() {
        None
    } else {
        Some("EXTERNAL ACCESS ACTIVE")
    };
    let title = match (controller, authority_label) {
        (Some(controller), Some(authority)) => format!("{base} — {controller} — {authority}"),
        (Some(controller), None) => format!("{base} — {controller}"),
        (None, Some(authority)) => format!("{base} — {authority}"),
        (None, None) => base,
    };
    let title = if control_transfer_pending {
        format!("{title} — CONTROL REQUEST: Ctrl+Shift+Y accept / Ctrl+Shift+N deny")
    } else {
        title
    };
    if let Some(search) = search.filter(|search| search.input.is_some()) {
        let query = search.input.as_ref().map_or("", BoundedTextEditor::text);
        let query = query
            .chars()
            .filter(|ch| !ch.is_control())
            .take(64)
            .collect::<String>();
        format!(
            "{title} — SEARCH: {query} [{} match(es), Ctrl+N/P]",
            search.matches.len()
        )
    } else {
        title
    }
}

const fn deterministic_capture_ready(
    scale_ready: bool,
    minimum_images: usize,
    available_images: usize,
) -> bool {
    scale_ready && available_images >= minimum_images
}

fn capture_minimum_images() -> Result<usize> {
    let explicit = std::env::var("SPLINTERM_CAPTURE_MIN_IMAGES")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()
        .context("SPLINTERM_CAPTURE_MIN_IMAGES must be a nonnegative integer")?;
    Ok(explicit.unwrap_or_else(|| {
        usize::from(std::env::var_os("SPLINTERM_CAPTURE_REQUIRE_IMAGE").is_some())
    }))
}

fn viewport_cursor_row(cursor_row: i32, offset: usize, rows: usize) -> Option<i32> {
    if cursor_row < 0 {
        return None;
    }
    i32::try_from(offset)
        .ok()
        .and_then(|offset| cursor_row.checked_add(offset))
        .filter(|row| *row >= 0 && usize::try_from(*row).is_ok_and(|row| row < rows))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BackgroundEffectCommitMode {
    Immediate,
    DeferToDraw,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent effects of one theme update, each OR-ed into the frame's own flags"
)]
struct ThemeUpdateImpact {
    rebuild_pixels: bool,
    reconcile_effect: bool,
    /// The tab strip height changed, so the content area and the panes'
    /// grids move with it.
    relayout_tab_strip: bool,
    /// The chrome text size changed (the Omarchy text size moved while
    /// `[main] ui-font-pixelsize` is unset): chrome is drawn again at its
    /// new size, and terminal frames stay as they are.
    redraw_chrome: bool,
}

impl ThemeUpdateImpact {
    /// The effects of two theme changes in one pass.
    const fn or(self, other: Self) -> Self {
        Self {
            rebuild_pixels: self.rebuild_pixels | other.rebuild_pixels,
            reconcile_effect: self.reconcile_effect | other.reconcile_effect,
            relayout_tab_strip: self.relayout_tab_strip | other.relayout_tab_strip,
            redraw_chrome: self.redraw_chrome | other.redraw_chrome,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct BackgroundEffectReconcileSchedule {
    on_draw: bool,
}

impl BackgroundEffectReconcileSchedule {
    fn queue_update(
        &mut self,
        effect_changed: bool,
        visual_commit_queued: bool,
        configured: bool,
    ) -> bool {
        if !effect_changed {
            return false;
        }
        if self.on_draw || visual_commit_queued && configured {
            self.on_draw = true;
            false
        } else {
            true
        }
    }

    fn queue_geometry(&mut self) {
        self.on_draw = true;
    }

    fn capability_reconciles_immediately(self) -> bool {
        !self.on_draw
    }

    fn take_for_draw(&mut self) -> bool {
        std::mem::take(&mut self.on_draw)
    }

    fn clear(&mut self) {
        self.on_draw = false;
    }
}

const fn reported_graphical_focus(
    keyboard_focused: bool,
    focused_splint_id: Option<SplintId>,
) -> Option<SplintId> {
    if keyboard_focused {
        focused_splint_id
    } else {
        None
    }
}

fn update_graphical_focus_watch(
    sender: Option<&tokio::sync::watch::Sender<Option<SplintId>>>,
    keyboard_focused: bool,
    focused_splint_id: Option<SplintId>,
) {
    let focused = reported_graphical_focus(keyboard_focused, focused_splint_id);
    if let Some(sender) = sender {
        sender.send_if_modified(|current| {
            if *current == focused {
                false
            } else {
                *current = focused;
                true
            }
        });
    }
}

fn retain_newest_font(slot: &mut Option<FontUpdate>, update: FontUpdate) {
    if slot
        .as_ref()
        .is_none_or(|current| current.generation.id() < update.generation.id())
    {
        *slot = Some(update);
    }
}

fn retain_newest_theme(slot: &mut Option<ThemeUpdate>, update: ThemeUpdate) {
    if slot
        .as_ref()
        .is_none_or(|current| update.generation > current.generation)
    {
        *slot = Some(update);
    }
}

/// The followed Omarchy text size as Omarchy sets it for terminals: px × 9/12
/// points, which output-scale sizing resolves as Foot does with its default
/// `dpi-aware=no`.
fn followed_font_size(theme: ResolvedTheme) -> Option<FontSize> {
    theme
        .shell
        .text_size_px
        .map(|px| FontSize::Points(omarchy_terminal_point_size(px)))
}

/// The chrome text size: `[main] ui-font-pixelsize`, otherwise the theme's
/// Omarchy text size, otherwise 14 px. Neither the terminal font size nor
/// runtime zoom takes part.
fn chrome_font_size(ui_font_size: Option<FontSize>, theme: ResolvedTheme) -> FontSize {
    resolve_ui_font_size(ui_font_size, theme.shell.ui_text_size_px)
}

/// Whether the followed text size still needs applying: the shell value is
/// parseable (`held` is false), it differs from the size last applied, and it
/// was not already rejected at the current zoom.
fn followed_text_size_due(
    followed: Option<u16>,
    held: bool,
    applied: Option<u16>,
    rejected: Option<(Option<u16>, i16)>,
    zoom_steps: i16,
) -> bool {
    !held && followed != applied && rejected != Some((followed, zoom_steps))
}

fn classify_theme_update(current: ResolvedTheme, next: ResolvedTheme) -> ThemeUpdateImpact {
    let reconcile_effect = current.background_alpha != next.background_alpha
        || current.background_blur != next.background_blur;
    let mut current_pixels = current;
    let mut next_pixels = next;
    current_pixels.background_blur = false;
    next_pixels.background_blur = false;
    // A text size change is reconciled through the font path instead, and a
    // chrome text size change by dropping cached chrome text.
    current_pixels.shell.text_size_px = None;
    next_pixels.shell.text_size_px = None;
    current_pixels.shell.ui_text_size_px = None;
    next_pixels.shell.ui_text_size_px = None;
    current_pixels.shell.text_size_held = false;
    next_pixels.shell.text_size_held = false;
    // A strip height change moves the content area, as hiding or showing the
    // strip does, and repaints no pane pixels.
    current_pixels.shell.bar_size_px = 0;
    next_pixels.shell.bar_size_px = 0;
    ThemeUpdateImpact {
        rebuild_pixels: current_pixels != next_pixels,
        reconcile_effect,
        relayout_tab_strip: tab_strip_logical_height(current) != tab_strip_logical_height(next),
        // Decided by the Window, which also knows `[main] ui-font-pixelsize`.
        redraw_chrome: false,
    }
}

/// Whether a live strip height change still needs its own pane resize once
/// a frame's updates are applied. A font reconciliation in the same pass
/// already resized every pane from the new content area with the new cell
/// metrics, so a second resize would only reflow the panes twice.
const fn strip_relayout_needs_resize(
    strip_relayout: bool,
    font_reconciled: bool,
    configured: bool,
) -> bool {
    strip_relayout && !font_reconciled && configured
}

fn background_effect_capability_bits(flags: WEnum<BackgroundCapability>) -> u32 {
    match flags {
        WEnum::Value(flags) => flags.bits(),
        WEnum::Unknown(flags) => flags,
    }
}

fn background_effect_diagnostic_message(diagnostic: EffectDiagnostic) -> &'static str {
    match diagnostic {
        EffectDiagnostic::MissingManager => {
            "splinterm background blur requested, but ext-background-effect-v1 is unavailable"
        }
        EffectDiagnostic::MissingBlurCapability => {
            "splinterm background blur requested, but the compositor advertises no blur capability"
        }
    }
}

fn background_effect_trace_line(action: EffectAction) -> Option<String> {
    match action {
        EffectAction::Diagnostic(_) => None,
        EffectAction::CreateEffect => Some("splinterm background-effect create".to_owned()),
        EffectAction::SetBlurRegion(size) => Some(format!(
            "splinterm background-effect region={}x{}",
            size.width(),
            size.height()
        )),
        EffectAction::DestroyEffect => Some("splinterm background-effect destroy".to_owned()),
        EffectAction::CommitSurface(reason) => {
            Some(format!("splinterm background-effect commit={reason:?}"))
        }
    }
}

fn remote_split_can_begin(pending: &HashMap<SplintId, SplintId>) -> bool {
    pending.is_empty()
}

fn is_pending_remote_splint(pending: &HashMap<SplintId, SplintId>, splint_id: SplintId) -> bool {
    pending.values().any(|candidate| *candidate == splint_id)
}

/// The Splint and axis a command would split, looking through an Explorer
/// context wrapper, so every split path can be admitted the same way.
fn split_request(command: &WindowTopologyCommand) -> Option<(SplintId, Axis)> {
    match command {
        WindowTopologyCommand::Split { target, axis, .. } => Some((*target, *axis)),
        WindowTopologyCommand::ExplorerContext { command, .. } => split_request(command),
        _ => None,
    }
}

fn insert_pending_split(
    node: &mut LayoutNode,
    target: SplintId,
    pending: Splint,
    axis: Axis,
) -> bool {
    match node {
        LayoutNode::Leaf(splint) if splint.id == target => {
            let current = LayoutNode::Leaf(splint.clone());
            *node = LayoutNode::Branch {
                axis,
                ratio: SplitRatio::new(500).expect("fixed pending split ratio is valid"),
                first: Box::new(current),
                second: Box::new(LayoutNode::Leaf(pending)),
            };
            true
        }
        LayoutNode::Leaf(_) => false,
        LayoutNode::Branch { first, second, .. } => {
            if first.find_splint(target).is_some() {
                insert_pending_split(first, target, pending, axis)
            } else {
                insert_pending_split(second, target, pending, axis)
            }
        }
    }
}

fn remove_pending_split(node: LayoutNode, pending: SplintId) -> (Option<LayoutNode>, bool) {
    match node {
        LayoutNode::Leaf(splint) if splint.id == pending => (None, true),
        leaf @ LayoutNode::Leaf(_) => (Some(leaf), false),
        LayoutNode::Branch {
            axis,
            ratio,
            first,
            second,
        } => {
            let (first, removed_first) = remove_pending_split(*first, pending);
            let (second, removed_second) = remove_pending_split(*second, pending);
            let removed = removed_first || removed_second;
            let node = match (first, second) {
                (Some(first), Some(second)) => Some(LayoutNode::Branch {
                    axis,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(remaining), None) | (None, Some(remaining)) => Some(remaining),
                (None, None) => None,
            };
            (node, removed)
        }
    }
}

fn pending_remote_snapshot(splint_id: SplintId, columns: usize, rows: usize) -> TerminalSnapshot {
    let message_cells = "Opening remote pane…"
        .chars()
        .take(columns)
        .map(|character| TerminalCell {
            content: character.to_string(),
            spacer_remaining: None,
            attributes: splinterm_protocol::CellAttributes::default(),
        })
        .collect::<Vec<_>>();
    let visible_rows = (0..rows)
        .map(|index| TerminalRow {
            row_id: u64::try_from(index)
                .ok()
                .and_then(|index| index.checked_add(1)),
            linebreak: false,
            cells: if index == 0 {
                message_cells.clone()
            } else {
                Vec::new()
            },
        })
        .collect();
    TerminalSnapshot {
        splint_id,
        incarnation: 1,
        revision: 1,
        columns,
        rows,
        cursor_column: 0,
        cursor_row: 0,
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
        default_colors: [0xff_d8dee9, 0xff_2e3440, 0xff_d8dee9],
        title: "Opening remote pane…".to_owned(),
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

/// The logical height the strip takes from a window: the theme's strip height
/// while tabs show or a remote tag needs the header, else none.
fn tab_strip_height(
    managed_tabs: bool,
    visible: bool,
    remote: bool,
    strip_height: u32,
    surface_height: u32,
) -> u32 {
    if (managed_tabs && visible) || remote {
        strip_height.min(surface_height)
    } else {
        0
    }
}

fn session_picker_retry_request(command: &WindowTopologyCommand) -> Option<WindowTopologyCommand> {
    match command {
        WindowTopologyCommand::RequestSessionPicker
        | WindowTopologyCommand::RequestSelector { .. } => Some(command.clone()),
        _ => None,
    }
}

const fn session_picker_purpose(selector_kind: Option<SelectorKind>) -> SessionPickerPurpose {
    match selector_kind {
        Some(SelectorKind::Dojo) => SessionPickerPurpose::Dojos,
        Some(SelectorKind::Lair) => SessionPickerPurpose::Lairs,
        None => SessionPickerPurpose::RecentDojos,
    }
}

fn session_picker_status_catalog(message: &'static str) -> SessionPickerCatalog {
    SessionPickerCatalog {
        items: Vec::new(),
        targets: Vec::new(),
        creation_enabled: false,
        creation_blocker: Some(message),
        creation_target: None,
        initial_row: None,
    }
}

const fn session_picker_title(selector_kind: Option<SelectorKind>) -> &'static str {
    match selector_kind {
        Some(SelectorKind::Dojo) => "Splinterm — Dojos",
        Some(SelectorKind::Lair) => "Splinterm — Lairs",
        None => "Splinterm — Recent Dojos",
    }
}

fn session_picker_new_command(
    selector_kind: Option<SelectorKind>,
    lair_id: LairId,
    source_splint_id: SplintId,
    target: SessionPickerCreationTarget,
) -> Result<WindowTopologyCommand> {
    match (selector_kind, target.action) {
        (Some(SelectorKind::Dojo), NavigationAction::CreateDojo) => {
            Ok(WindowTopologyCommand::PickerNewDojo {
                topology_revision: target.topology_revision,
                lair_id,
                source_splint_id,
            })
        }
        (Some(SelectorKind::Lair) | None, NavigationAction::CreateLair) => {
            Ok(WindowTopologyCommand::PickerNewLair {
                topology_revision: target.topology_revision,
                source_splint_id,
            })
        }
        _ => anyhow::bail!("picker creation target disagrees with picker purpose"),
    }
}

/// A docked Explorer laid out over the Window's full height keeps that
/// height; the terminal beside it starts below the tab strip, `strip_height`
/// logical px tall. A drawer, which overlays the terminal, is not used here:
/// it keeps the layout below the tab strip.
fn explorer_beside_tabs(
    mut layout: LairExplorerLayout,
    strip_height: u32,
) -> Option<LairExplorerLayout> {
    if layout.mode != LairExplorerPresentationMode::Docked {
        return None;
    }
    let strip_height = strip_height.min(layout.terminal.height);
    layout.terminal.y = layout.terminal.y.saturating_add(strip_height);
    layout.terminal.height = layout.terminal.height.saturating_sub(strip_height);
    Some(layout)
}

/// A docked Explorer over the Window's whole height `full`, beside a tab
/// strip `strip` logical px tall. Beside a shown strip its header takes the
/// strip's height and sizes its text as the tab labels are at `scale_120`,
/// so the header and the labels share one baseline; with no strip it keeps
/// the row-high header. A `collapsed` Explorer is the rail, as wide as
/// `metrics` say, when it docks beside `minimum_terminal_width` px of
/// terminal. `None` when the Explorer would be a drawer.
#[allow(clippy::too_many_arguments)]
fn full_height_explorer_layout(
    full: Rect,
    strip: u32,
    scale_120: u32,
    (minimum_terminal_width, metrics): (u32, LairExplorerMetrics),
    rows: &[LairExplorerRow],
    selected: Option<NavigationNodeId>,
    requested_start: usize,
    collapsed: bool,
) -> Option<LairExplorerLayout> {
    lair_explorer_shown_layout(
        full,
        (strip != 0).then_some(strip),
        minimum_terminal_width,
        metrics,
        rows,
        selected,
        requested_start,
        collapsed,
    )
    .map(|mut layout| {
        if strip != 0 {
            layout.header_text_cell_cap =
                tab_text_maximum_cell_height(strip, scale_120).unwrap_or(u32::MAX);
        }
        layout
    })
    .and_then(|layout| explorer_beside_tabs(layout, strip))
}

/// Where a tab strip beside a full-height Explorer begins: the docked
/// panel's right edge. A drawer or a hidden Explorer leaves it at 0.
fn tab_strip_left_of(explorer: Option<&LairExplorerLayout>) -> u32 {
    explorer
        .filter(|layout| layout.mode == LairExplorerPresentationMode::Docked)
        .map_or(0, |layout| {
            layout.panel.x.saturating_add(layout.panel.width)
        })
}

impl App {
    fn base_content_rect(&self) -> Rect {
        let y = tab_strip_height(
            self.tab_state.managed_tabs,
            self.tab_state.tab_strip_visible,
            self.tab_state.remote_display_identity.is_some(),
            self.tab_strip_logical_height(),
            self.surface.logical_height,
        );
        Rect {
            x: 0,
            y,
            width: self.surface.logical_width,
            height: self.surface.logical_height.saturating_sub(y),
        }
    }

    fn current_explorer_layout(&self) -> Option<LairExplorerLayout> {
        if !self.explorer.visible() {
            return None;
        }
        let cell_width = self
            .panes
            .pane
            .snapshot_frame
            .as_ref()
            .and_then(|frame| {
                buffer_to_logical_ceil(frame.cell_width(), self.surface.scale_120).ok()
            })
            .unwrap_or(1);
        let minimum_terminal_width =
            explorer_minimum_docked_terminal_width(cell_width, self.presentation.initial_columns);
        let metrics = self.explorer_metrics();
        let rows = self.explorer.rows();
        let below_tabs = self.base_content_rect();
        // The rail shows its selection only while it has the keys, so only
        // then does the selection hold the list in place; otherwise the
        // wheel scrolls the rail freely.
        let selected = self
            .explorer
            .tree_selected()
            .filter(|_| !self.explorer.collapsed() || self.explorer.focused());
        let full_height = self
            .presentation
            .explorer_full_height
            .then(|| {
                let full = Rect {
                    x: 0,
                    y: 0,
                    width: self.surface.logical_width,
                    height: self.surface.logical_height,
                };
                full_height_explorer_layout(
                    full,
                    below_tabs.y,
                    self.surface.scale_120,
                    (minimum_terminal_width, metrics),
                    &rows,
                    selected,
                    self.presentation.explorer_visible_start,
                    self.explorer.collapsed(),
                )
            })
            .flatten();
        let layout = full_height.or_else(|| {
            lair_explorer_shown_layout(
                below_tabs,
                None,
                minimum_terminal_width,
                metrics,
                &rows,
                selected,
                self.presentation.explorer_visible_start,
                self.explorer.collapsed(),
            )
        })?;
        // The agents section splits off the list's bottom, above the footer.
        // The tree scrolls from where it was asked to, not from the start
        // clamped to the list before the split. The section shows from its
        // top while the selection is elsewhere, so a newly urgent agent is
        // in view.
        let agent_selected = self.explorer.agent_selected();
        Some(lair_explorer_with_agents(
            layout,
            &rows,
            (selected, self.presentation.explorer_visible_start),
            &self.explorer.agent_rows(),
            agent_selected,
            if agent_selected.is_some() {
                self.presentation.explorer_agents_start
            } else {
                0
            },
        ))
    }

    /// The Explorer's layout, taken as [`Self::current_explorer_layout`]
    /// does, after telling the Explorer whether it shows the agents
    /// section: a hidden section's rows are skipped, so a selection there
    /// moves to the tree, and the layout is taken again for it.
    fn take_explorer_layout(&mut self) -> Option<LairExplorerLayout> {
        let layout = self.current_explorer_layout();
        let shown = layout
            .as_ref()
            .is_some_and(|layout| layout.agents.is_some());
        if self.explorer.set_agents_shown(shown) {
            self.current_explorer_layout()
        } else {
            layout
        }
    }

    /// What the Explorer's layout sizes from its chrome text: the rail's
    /// width and the footer's action targets.
    fn explorer_metrics(&self) -> LairExplorerMetrics {
        LairExplorerMetrics::measured(self.explorer_chrome_cell(), self.surface.scale_120)
    }

    /// One regular chrome cell's width in buffer px, or 0 when the chrome
    /// font cannot be loaded, measured once per chrome text generation and
    /// scale.
    fn explorer_chrome_cell(&self) -> u32 {
        let key = (
            self.presentation.renderer_generation,
            self.surface.scale_120,
        );
        match self.presentation.explorer_chrome_cell.get() {
            Some((cached, cell)) if cached == key => cell,
            _ => {
                let cell = lair_explorer_chrome_cell_width(
                    &self.presentation.render_context,
                    self.surface.scale_120,
                )
                .unwrap_or(0);
                self.presentation
                    .explorer_chrome_cell
                    .set(Some((key, cell)));
                cell
            }
        }
    }

    /// Where the tab strip begins: at a full-height docked Explorer's right
    /// edge, otherwise at the Window's left edge.
    fn tab_strip_left(&self) -> u32 {
        if !self.presentation.explorer_full_height {
            return 0;
        }
        tab_strip_left_of(self.current_explorer_layout().as_ref())
    }

    fn content_rect(&self) -> Rect {
        self.current_explorer_layout()
            .map_or_else(|| self.base_content_rect(), |layout| layout.terminal)
    }

    fn request_pane_control_release(pane: &mut PaneView, terminal_input_pending: bool) {
        if !pane.controller_active {
            pane.control_release_pending = false;
            return;
        }
        if terminal_input_pending {
            pane.control_release_pending = true;
            return;
        }
        let outcome = pane.commands.as_ref().map_or(
            ControlReleaseOutcome::Disconnected,
            try_queue_control_release,
        );
        match outcome {
            ControlReleaseOutcome::Queued => {
                pane.controller_active = false;
                pane.control_release_pending = false;
            }
            ControlReleaseOutcome::Retry => pane.control_release_pending = true,
            ControlReleaseOutcome::Disconnected => {
                pane.controller_active = false;
                pane.control_release_pending = false;
                pane.commands = None;
            }
        }
    }

    fn retry_pane_control_release(pane: &mut PaneView, terminal_input_pending: bool) {
        if pane.control_release_pending {
            Self::request_pane_control_release(pane, terminal_input_pending);
        }
    }

    fn release_tab_controllers(
        view: &mut DojoTabView,
        pending_input: &VecDeque<PendingTerminalInput>,
    ) {
        for pane in std::iter::once(&mut view.pane).chain(view.inactive_panes.iter_mut()) {
            let input_pending = terminal_input_pending_for(pending_input, pane.commands.as_ref());
            Self::request_pane_control_release(pane, input_pending);
        }
    }

    fn request_pane_focus_report(pane: &mut PaneView, focused: bool, terminal_input_pending: bool) {
        if terminal_input_pending {
            pane.pending_focus_report = Some(focused);
            return;
        }
        let outcome = pane
            .commands
            .as_ref()
            .map_or(ControlReleaseOutcome::Disconnected, |commands| {
                try_queue_focus_report(commands, focused)
            });
        match outcome {
            ControlReleaseOutcome::Queued => pane.pending_focus_report = None,
            ControlReleaseOutcome::Retry => pane.pending_focus_report = Some(focused),
            ControlReleaseOutcome::Disconnected => {
                pane.controller_active = false;
                pane.pending_focus_report = None;
                pane.commands = None;
            }
        }
    }

    fn retry_pane_focus_report(pane: &mut PaneView, terminal_input_pending: bool) {
        if let Some(focused) = pane.pending_focus_report {
            Self::request_pane_focus_report(pane, focused, terminal_input_pending);
        }
    }

    fn active_terminal_input_pending(&self) -> bool {
        terminal_input_pending_for(
            &self.input.pending_terminal_input,
            self.panes.pane.commands.as_ref(),
        )
    }

    fn request_active_pane_focus_report(&mut self, focused: bool) {
        let input_pending = self.active_terminal_input_pending();
        Self::request_pane_focus_report(&mut self.panes.pane, focused, input_pending);
    }

    fn request_active_pane_control_release(&mut self) {
        self.cancel_clipboard_image();
        self.clipboard.palette_image_target = None;
        let input_pending = self.active_terminal_input_pending();
        Self::request_pane_control_release(&mut self.panes.pane, input_pending);
    }

    fn release_visible_pane_controllers(&mut self) {
        for pane in
            std::iter::once(&mut self.panes.pane).chain(self.panes.inactive_panes.iter_mut())
        {
            let input_pending = terminal_input_pending_for(
                &self.input.pending_terminal_input,
                pane.commands.as_ref(),
            );
            Self::request_pane_control_release(pane, input_pending);
        }
    }

    /// Removes a tab the topology manager closed, switching first to the tab
    /// that follows it when it was the active one. Returns whether the Window
    /// changed.
    fn remove_tab_locally(&mut self, dojo_id: DojoId) -> Result<bool> {
        let mut changed = false;
        if self.tab_state.active_dojo_id() == dojo_id {
            if let Some(selected) = self.tab_after_close(dojo_id) {
                changed |= self.activate_tab(selected)?;
            } else {
                self.release_visible_pane_controllers();
            }
        }
        let mut removed = self.tab_state.tabs.close(dojo_id);
        let removal_action = final_tab_removal_action(
            removed.is_some(),
            self.tab_state.tabs.len(),
            self.modal.session_picker_requested,
        );
        if let Some(removed) = removed.as_mut() {
            if let Some(view) = removed.value.as_mut() {
                Self::release_tab_controllers(view, &self.input.pending_terminal_input);
            }
            self.tab_state.tab_label_cache.clear();
            self.presentation.full_redraw = true;
            changed = true;
        }
        match removal_action {
            FinalTabRemovalAction::Continue => {}
            FinalTabRemovalAction::Exit => self
                .scheduling
                .request_exit(ExitClass::CleanFinalTabRemoved),
            FinalTabRemovalAction::ExitAndHandoffPicker => {
                self.modal.session_picker_requested = false;
                if spawn_session_picker_handoff().is_err() {
                    eprintln!("splinterm Dojo picker handoff failed");
                }
                self.scheduling
                    .request_exit(ExitClass::CleanFinalTabRemoved);
            }
        }
        Ok(changed)
    }

    /// The tab to show once `dojo_id` closes: the usual neighbour when its
    /// panes are ready to show, or else the first other tab whose panes are.
    fn tab_after_close(&self, dojo_id: DojoId) -> Option<DojoId> {
        tab_after_close(&self.tab_state.tabs, dojo_id, |tab| tab.value.is_some())
    }

    fn activate_tab(&mut self, dojo_id: DojoId) -> Result<bool> {
        let started = Instant::now();
        let previous_id = self.tab_state.active_dojo_id();
        if previous_id == dojo_id {
            return Ok(false);
        }
        self.close_copy_mode();
        anyhow::ensure!(
            self.tab_state
                .tabs
                .get(dojo_id)
                .is_some_and(|tab| tab.value.is_some()),
            "activated Dojo tab has no hidden frontend"
        );
        self.settle_terminal_presses_for_picker();
        self.input.prefix_state.clear();
        self.presentation.zoomed_splint = None;
        self.input.input_generation = self.input.input_generation.saturating_add(1);
        if self.input.terminal_focus_reported {
            self.request_active_pane_focus_report(false);
            self.input.terminal_focus_reported = false;
        }
        self.release_visible_pane_controllers();
        let next = self
            .tab_state
            .tabs
            .get_mut(dojo_id)
            .and_then(|tab| tab.value.take())
            .context("validated Dojo tab frontend disappeared")?;
        let previous = DojoTabView {
            identity: std::mem::replace(&mut self.tab_state.active_identity, next.identity),
            pane: std::mem::replace(&mut self.panes.pane, next.pane),
            inactive_panes: std::mem::replace(&mut self.panes.inactive_panes, next.inactive_panes),
            layout: std::mem::replace(&mut self.panes.layout, next.layout),
            pending_exited_splints: std::mem::replace(
                &mut self.panes.pending_exited_splints,
                next.pending_exited_splints,
            ),
            pending_remote_splits: std::mem::replace(
                &mut self.panes.pending_remote_splits,
                next.pending_remote_splits,
            ),
            frame_titles: std::mem::replace(&mut self.presentation.frame_titles, next.frame_titles),
            dirty_inactive_panes: std::mem::replace(
                &mut self.panes.dirty_inactive_panes,
                next.dirty_inactive_panes,
            ),
        };
        self.tab_state
            .tabs
            .get_mut(previous_id)
            .context("previous Dojo tab disappeared")?
            .value = Some(previous);
        anyhow::ensure!(
            self.tab_state.tabs.activate(dojo_id),
            "activated Dojo tab is absent"
        );
        self.sync_graphical_focus();
        rebuild_pane_scaled_frame_with_context(
            &mut self.panes.pane,
            self.surface.scale_120,
            &self.presentation.render_context,
        )?;
        for pane in &mut self.panes.inactive_panes {
            rebuild_pane_scaled_frame_with_context(
                pane,
                self.surface.scale_120,
                &self.presentation.render_context,
            )?;
        }
        self.panes.dirty_inactive_panes.clear();
        self.panes.restored_frontend_needs_resize = true;
        self.input.cursor_blink_visible = true;
        self.input.last_cursor_blink = Instant::now();
        self.presentation.full_redraw = true;
        self.update_window_title();
        self.clear_ime_preedit();
        self.update_ime_cursor_rectangle();
        if self.input.keyboard_focused && self.panes.input_modes().focus_reporting {
            self.request_active_pane_focus_report(true);
            self.input.terminal_focus_reported = true;
        }
        if perf_trace_enabled() {
            emit_perf_trace(
                "splinterm",
                "tab_switch",
                PerfTraceEvent {
                    splint_id: self.panes.focused_splint(),
                    duration_ns: Some(
                        u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    ),
                    count: Some(u64::try_from(self.tab_state.tabs.len()).unwrap_or(u64::MAX)),
                    ..PerfTraceEvent::default()
                },
            );
        }
        Ok(true)
    }

    fn trace_background_effect_action(&self, action: EffectAction) {
        if self.surface.background_effect_trace
            && let Some(line) = background_effect_trace_line(action)
        {
            eprintln!("{line}");
        }
    }

    fn execute_background_effect_actions(
        &mut self,
        queue_handle: &QueueHandle<Self>,
        commit_mode: BackgroundEffectCommitMode,
    ) -> Result<()> {
        for action in self.surface.background_effect_state.reconcile() {
            self.trace_background_effect_action(action);
            match action {
                EffectAction::Diagnostic(diagnostic) => {
                    eprintln!("{}", background_effect_diagnostic_message(diagnostic));
                }
                EffectAction::CreateEffect => {
                    anyhow::ensure!(
                        self.surface.background_effect.is_none(),
                        "background effect reducer attempted duplicate object creation"
                    );
                    let manager = self
                        .surface
                        .background_effect_manager
                        .as_ref()
                        .context("background effect manager disappeared before creation")?;
                    self.surface.background_effect = Some(manager.get_background_effect(
                        self.surface.window.wl_surface(),
                        queue_handle,
                        (),
                    ));
                }
                EffectAction::SetBlurRegion(size) => {
                    let effect = self
                        .surface
                        .background_effect
                        .as_ref()
                        .context("background effect region requested without an object")?;
                    let region = Region::new(&self.platform.compositor)
                        .context("create finite background effect region")?;
                    region.add(0, 0, size.width(), size.height());
                    effect.set_blur_region(Some(region.wl_region()));
                    drop(region);
                }
                EffectAction::DestroyEffect => {
                    let effect = self
                        .surface
                        .background_effect
                        .take()
                        .context("background effect destroy requested without an object")?;
                    effect.destroy();
                }
                EffectAction::CommitSurface(reason) => match commit_mode {
                    BackgroundEffectCommitMode::Immediate => {
                        self.surface.window.commit();
                        anyhow::ensure!(
                            self.surface.background_effect_state.surface_committed()
                                == Some(reason),
                            "background effect commit did not match reducer state"
                        );
                    }
                    BackgroundEffectCommitMode::DeferToDraw => {
                        anyhow::ensure!(
                            self.surface
                                .background_effect_deferred_commit
                                .replace(reason)
                                .is_none(),
                            "background effect already has a deferred surface commit"
                        );
                    }
                },
            }
        }
        Ok(())
    }

    fn complete_background_effect_draw_commit(&mut self) -> Result<()> {
        let Some(expected) = self.surface.background_effect_deferred_commit.take() else {
            return Ok(());
        };
        anyhow::ensure!(
            self.surface.background_effect_state.surface_committed() == Some(expected),
            "draw commit did not match deferred background effect state"
        );
        Ok(())
    }

    fn reconcile_background_effect(
        &mut self,
        queue_handle: &QueueHandle<Self>,
        commit_mode: BackgroundEffectCommitMode,
    ) -> Result<()> {
        if self.surface.background_effect_manager.is_none()
            || self.surface.background_effect_capabilities_received
        {
            self.execute_background_effect_actions(queue_handle, commit_mode)?;
        }
        Ok(())
    }

    fn queue_background_effect_geometry_for_draw(&mut self) -> Result<()> {
        self.surface.background_effect_state.set_logical_size(
            i64::from(self.surface.logical_width),
            i64::from(self.surface.logical_height),
        )?;
        self.surface
            .background_effect_reconcile_schedule
            .queue_geometry();
        Ok(())
    }

    fn release_background_effect(&mut self) {
        for action in self.surface.background_effect_state.destroy_surface() {
            self.trace_background_effect_action(action);
            if action == EffectAction::DestroyEffect
                && let Some(effect) = self.surface.background_effect.take()
            {
                effect.destroy();
            }
        }
        self.surface.background_effect_deferred_commit = None;
        self.surface.background_effect_reconcile_schedule.clear();
        if let Some(manager) = self.surface.background_effect_manager.take() {
            manager.destroy();
        }
    }

    fn pane_for_splint(&self, splint_id: SplintId) -> Option<&PaneView> {
        std::iter::once(&self.panes.pane)
            .chain(self.panes.inactive_panes.iter())
            .find(|pane| {
                pane.snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.splint_id == splint_id)
            })
    }

    fn file_drop_target_at(&self, point: (f64, f64)) -> Option<FileDropTarget> {
        let layout = self.computed_pane_layout()?;
        let over_divider = layout.split_at(point, 6).is_some();
        let splint_id = pane_drop_target(
            self.content_rect(),
            layout.panes.iter().map(|pane| (pane.splint_id, pane.rect)),
            point,
            self.modal.input_modal_open(),
            over_divider,
        )?;
        let pane = self.pane_for_splint(splint_id)?;
        let snapshot = pane.snapshot.as_ref()?;
        (pane.controller_active && pane.commands.is_some()).then_some(FileDropTarget {
            topology_revision: self.tab_state.active_identity.topology_revision,
            dojo_id: self.tab_state.active_dojo_id(),
            splint_id,
            incarnation: snapshot.incarnation,
            input_generation: self.input.input_generation,
        })
    }

    fn splint_at_point(&self, point: (f64, f64)) -> Option<SplintId> {
        let layout = self.computed_pane_layout()?;
        layout.panes.into_iter().find_map(|pane| {
            let right = pane.rect.x.checked_add(pane.rect.width)?;
            let bottom = pane.rect.y.checked_add(pane.rect.height)?;
            (point.0 >= f64::from(pane.rect.x)
                && point.0 < f64::from(right)
                && point.1 >= f64::from(pane.rect.y)
                && point.1 < f64::from(bottom))
            .then_some(pane.splint_id)
        })
    }

    fn focus_splint_at_incarnation(
        &mut self,
        splint_id: SplintId,
        live_incarnation: Option<u64>,
    ) -> Option<bool> {
        let pending_remote = is_pending_remote_splint(&self.panes.pending_remote_splits, splint_id);
        let matches = std::iter::once(&self.panes.pane)
            .chain(&self.panes.inactive_panes)
            .filter_map(|pane| pane.snapshot.as_ref())
            .any(|snapshot| {
                snapshot.splint_id == splint_id
                    && live_incarnation.is_none_or(|expected| snapshot.incarnation == expected)
            });
        attempt_exact_splint_focus(splint_id, matches, pending_remote, || {
            let changed = self.focus_splint(splint_id);
            (changed, self.panes.focused_splint())
        })
    }

    fn focus_splint(&mut self, splint_id: SplintId) -> bool {
        if is_pending_remote_splint(&self.panes.pending_remote_splits, splint_id) {
            return false;
        }
        if self.panes.focused_splint() == Some(splint_id) {
            return false;
        }
        let Some(index) = self.panes.inactive_panes.iter().position(|pane| {
            pane.snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.splint_id == splint_id)
        }) else {
            return false;
        };
        if self.presentation.zoomed_splint.is_some() {
            self.presentation.zoomed_splint = None;
            self.panes.restored_frontend_needs_resize = true;
        }
        // A Window showing only the focused Splint shows the newly focused
        // one over the whole area, so its PTY takes that size.
        if matches!(
            self.pane_layout_fit(),
            Ok(Some((_, LayoutFit::FocusedOnly)))
        ) {
            self.panes.restored_frontend_needs_resize = true;
        }
        std::mem::swap(&mut self.panes.pane, &mut self.panes.inactive_panes[index]);
        self.focused_pane_changed();
        true
    }

    fn focused_pane_changed(&mut self) {
        self.input.input_generation = self.input.input_generation.saturating_add(1);
        self.clipboard.drag_target = None;
        self.panes.pane.pointer_cell = None;
        self.panes.pane.hovered_url = None;
        self.presentation.full_redraw = true;
        self.sync_graphical_focus();
    }

    fn directional_splint(&self, direction: FocusDirection) -> Option<SplintId> {
        let (Some(layout), Some(current)) = (&self.panes.layout, self.panes.focused_splint())
        else {
            return None;
        };
        PaneLayout::compute(
            layout,
            Rect {
                x: 0,
                y: 0,
                width: 1_000_000,
                height: 1_000_000,
            },
            1,
            1,
            1,
        )
        .ok()?
        .directional(current, direction)
    }

    fn focus_direction(&mut self, direction: FocusDirection) -> bool {
        if self.presentation.zoomed_splint.is_some() {
            return false;
        }
        self.directional_splint(direction)
            .is_some_and(|next| self.focus_splint(next))
    }

    fn toggle_pane_zoom(&mut self) -> bool {
        let Some(focused) = self.panes.focused_splint() else {
            return false;
        };
        self.presentation.zoomed_splint = if self.presentation.zoomed_splint == Some(focused) {
            None
        } else {
            Some(focused)
        };
        self.panes.restored_frontend_needs_resize = true;
        self.presentation.full_redraw = true;
        self.update_ime_cursor_rectangle();
        true
    }

    fn directional_resize_command(
        &self,
        direction: FocusDirection,
        cells: u16,
    ) -> Result<Option<WindowTopologyCommand>> {
        let (Some(root), Some(layout), Some(current), Some(frame)) = (
            self.panes.layout.as_ref(),
            self.computed_pane_layout(),
            self.panes.focused_splint(),
            self.panes.pane.snapshot_frame.as_ref(),
        ) else {
            return Ok(None);
        };
        let cell_width = buffer_to_logical_ceil(frame.cell_width(), self.surface.scale_120)?;
        let cell_height = buffer_to_logical_ceil(frame.cell_height(), self.surface.scale_120)?;
        let minimum_width = cell_width
            .checked_mul(2)
            .context("minimum pane width overflow")?;
        let minimum_height = cell_height
            .checked_mul(2)
            .context("minimum pane height overflow")?;
        let Some((target, ancestor, ratio)) = directional_resize_ratio(
            root,
            &layout,
            current,
            direction,
            cells,
            PaneResizeMetrics {
                cell_width,
                cell_height,
                minimum_width,
                minimum_height,
            },
        ) else {
            return Ok(None);
        };
        Ok(Some(WindowTopologyCommand::SetRatio {
            dojo_id: self.tab_state.active_dojo_id(),
            target,
            ancestor,
            ratio,
        }))
    }

    /// Prepares each framed pane's title for a top border row of the
    /// terminal's `cell_width` × `cell_height` cells. Titles are chrome text
    /// at the chrome size, but never taller than the terminal cell they sit
    /// in, and are cut to the chrome cells that fit beside the corners.
    fn prepare_frame_titles(
        &mut self,
        pane_layout: Option<&PaneLayout>,
        cell_width: u32,
        cell_height: u32,
    ) -> Result<()> {
        if self.presentation.pane_divider_style != PaneDividerStyle::Frame
            || self.presentation.frame_title_mode != FrameTitleMode::Splint
        {
            self.presentation.frame_titles.clear();
            return Ok(());
        }
        let (Some(pane_layout), Some(topology)) = (pane_layout, self.panes.layout.as_ref()) else {
            self.presentation.frame_titles.clear();
            return Ok(());
        };
        let scale_120 = self.surface.scale_120;
        let cell_key = (
            scale_120,
            cell_height,
            self.presentation.renderer_generation,
        );
        // One chrome cell at the current chrome size and border row, measured
        // once per change. Titles cached at another cell size, such as a
        // hidden tab's from before a zoom, are loaded again.
        let chrome_cell = match self.presentation.frame_title_cell {
            Some((key, cell)) if key == cell_key => cell,
            _ => {
                let reference = load_frame_title(
                    &ChromeSource::from("_"),
                    scale_120,
                    &self.presentation.render_context,
                    cell_height,
                )?;
                let cell = (reference.pixel_width(), reference.pixel_height());
                self.presentation.frame_title_cell = Some((cell_key, cell));
                cell
            }
        };
        let chrome_cell_width = chrome_cell.0;
        let mut requested = Vec::new();
        for pane in &pane_layout.panes {
            let allocation = Self::buffer_rect(pane.allocation, scale_120)?;
            let Some(maximum_cells) =
                frame_title_maximum_cells(allocation.width, cell_width, chrome_cell_width)
            else {
                continue;
            };
            let Some(splint) = topology.find_splint(pane.splint_id) else {
                continue;
            };
            let title = sanitize_frame_title(&splint.title, maximum_cells);
            if !title.is_empty() {
                requested.push((pane.splint_id, title, maximum_cells));
            }
        }
        let requested_ids = requested
            .iter()
            .map(|(splint_id, _, _)| *splint_id)
            .collect::<HashSet<_>>();
        self.presentation
            .frame_titles
            .retain(|splint_id, _| requested_ids.contains(splint_id));
        for (splint_id, source, maximum_cells) in requested {
            let source = ChromeSource::from(source);
            let current = self
                .presentation
                .frame_titles
                .get(&splint_id)
                .is_some_and(|cached| {
                    cached.source == source
                        && cached.maximum_cells == maximum_cells
                        && cached.scale_120 == scale_120
                        && cached.text.pixel_height() == chrome_cell.1
                        && cached.text.pixel_width()
                            == cached.text.cells().saturating_mul(chrome_cell_width)
                });
            if !current {
                self.presentation.frame_titles.insert(
                    splint_id,
                    CachedFrameTitle {
                        text: load_frame_title(
                            &source,
                            scale_120,
                            &self.presentation.render_context,
                            cell_height,
                        )?,
                        source,
                        maximum_cells,
                        scale_120,
                        bold: false,
                    },
                );
            }
        }
        Ok(())
    }

    /// The one pane layout that drawing, PTY sizes, hit-testing, divider
    /// drags, keyboard resize, and the IME rectangle all use. The configured
    /// pane gap shrinks, down to none, when the panes cannot fit with it
    /// ([`PaneLayout::compute_with_preferred_gap`]); the layout's `gap` is the
    /// gap in effect. When the Splints cannot fit even without a gap, only
    /// the focused Splint is laid out, over the whole content area
    /// ([`PaneLayout::compute_fitting`]); the others keep their last PTY
    /// size, since a pane absent from the layout is never resized.
    ///
    /// A Window too small for any layout, or a layout that cannot be
    /// computed at all, gives `None`, and the focused pane fills the content
    /// area as if it were alone: no layout input ever ends the client.
    fn computed_pane_layout(&self) -> Option<PaneLayout> {
        self.pane_layout_fit()
            .ok()
            .flatten()
            .map(|(layout, _)| layout)
    }

    /// [`Self::computed_pane_layout`] with which Splints it shows. A divider
    /// drag accepts only a ratio whose layout still shows every Splint.
    fn pane_layout_fit(&self) -> Result<Option<(PaneLayout, LayoutFit)>> {
        let Some(topology) = self.panes.layout.as_ref() else {
            return Ok(None);
        };
        let zoomed = self
            .presentation
            .zoomed_splint
            .and_then(|splint_id| topology.find_splint(splint_id).cloned())
            .map(LayoutNode::Leaf);
        let layout = zoomed.as_ref().unwrap_or(topology);
        let (chrome, minimum_width, minimum_height) = self.pane_layout_metrics()?;
        PaneLayout::compute_fitting(
            layout,
            self.content_rect(),
            chrome,
            self.presentation.pane_gap,
            minimum_width,
            minimum_height,
            self.panes.focused_splint(),
        )
        .map(Some)
    }

    /// The pane chrome and the normal two-cell minimum pane width and
    /// height, in logical pixels, for the current font and scale.
    fn pane_layout_metrics(&self) -> Result<(PaneChrome, u32, u32)> {
        let frame = self
            .panes
            .pane
            .snapshot_frame
            .as_ref()
            .context("multi-pane layout requires an active snapshot frame")?;
        let cell_width = buffer_to_logical_ceil(frame.cell_width(), self.surface.scale_120)?;
        let cell_height = buffer_to_logical_ceil(frame.cell_height(), self.surface.scale_120)?;
        let chrome = match self.presentation.pane_divider_style {
            PaneDividerStyle::None => PaneChrome::None,
            PaneDividerStyle::Line => PaneChrome::Line {
                vertical_width: cell_width,
                horizontal_height: cell_height,
            },
            PaneDividerStyle::Frame => PaneChrome::Frame {
                vertical_width: cell_width,
                horizontal_height: cell_height,
            },
        };
        let minimum_width = cell_width
            .checked_mul(2)
            .context("minimum pane width overflow")?;
        let minimum_height = cell_height
            .checked_mul(2)
            .context("minimum pane height overflow")?;
        Ok((chrome, minimum_width, minimum_height))
    }

    /// Whether a split of `target` along `axis` would still fit the content
    /// area without a gap at the normal minimum ([`split_fits`]). A refused
    /// split is never sent, so the daemon never gains a Splint this Window
    /// cannot show; the refusal is noted in the Window title and on stderr,
    /// like other refused actions, without opening or focusing anything.
    /// A split the client cannot judge (no topology or font metrics yet, or
    /// a target outside the shown Dojo) is left to the daemon.
    fn admit_split(&self, target: SplintId, axis: Axis) -> bool {
        let (Some(topology), Ok((chrome, minimum_width, minimum_height))) =
            (self.panes.layout.as_ref(), self.pane_layout_metrics())
        else {
            return true;
        };
        let fits = split_fits(
            topology,
            target,
            axis,
            self.content_rect(),
            chrome,
            minimum_width,
            minimum_height,
        );
        if !fits {
            self.window_notice("Split refused: the new pane would not fit in this Window");
        }
        fits
    }

    /// A short notice for an action that did not happen: written to stderr and
    /// shown in the Window title until the title next changes. It opens and
    /// focuses nothing.
    fn window_notice(&self, text: &str) {
        eprintln!("splinterm: {text}");
        self.surface.window.set_title(format!("Splinterm — {text}"));
    }

    fn buffer_rect(rect: Rect, scale_120: u32) -> Result<Rect> {
        let right = rect
            .x
            .checked_add(rect.width)
            .context("pane right overflow")?;
        let bottom = rect
            .y
            .checked_add(rect.height)
            .context("pane bottom overflow")?;
        let x = logical_extent_to_buffer(rect.x, scale_120)?;
        let y = logical_extent_to_buffer(rect.y, scale_120)?;
        Ok(Rect {
            x,
            y,
            width: logical_extent_to_buffer(right, scale_120)?.saturating_sub(x),
            height: logical_extent_to_buffer(bottom, scale_120)?.saturating_sub(y),
        })
    }

    fn pane_geometry(
        pane: &PaneView,
        rect: Rect,
        scale_120: u32,
    ) -> Result<Option<WindowGeometry>> {
        let Some(frame) = pane.snapshot_frame.as_ref() else {
            return Ok(None);
        };
        let geometry = pane.window_geometry(frame, rect.width, rect.height, scale_120)?;
        Ok(Some(geometry.translated(
            logical_extent_to_buffer(rect.x, scale_120)?,
            logical_extent_to_buffer(rect.y, scale_120)?,
        )?))
    }

    fn request_older_history(&mut self) -> Result<()> {
        if self.panes.pane.history_page_pending || self.panes.pane.history_selection_pin_blocked {
            return Ok(());
        }
        let Some(snapshot) = self.panes.pane.snapshot.as_ref() else {
            return Ok(());
        };
        if snapshot.omitted_oldest_scrollback_rows == 0 {
            return Ok(());
        }
        let Some(before_row_id) = snapshot.scrollback_rows.first().and_then(|row| row.row_id)
        else {
            return Ok(());
        };
        let Some(commands) = self.panes.pane.commands.as_ref() else {
            return Ok(());
        };
        if !try_advisory_command(
            commands,
            WindowCommand::FetchScrollback {
                splint_id: snapshot.splint_id,
                incarnation: snapshot.incarnation,
                terminal_revision: snapshot.revision,
                history_generation: snapshot.history_generation,
                before_row_id,
            },
        )? {
            return Ok(());
        }
        self.panes.pane.history_page_pending = true;
        self.panes.pane.history_page_top = Some(before_row_id);
        Ok(())
    }

    fn scroll_history(&mut self, action: MouseAction, lines: usize) -> Result<bool> {
        self.cancel_clipboard_image();
        self.panes.pane.discard_lagging_history();
        let snapshot = self
            .panes
            .pane
            .snapshot
            .as_ref()
            .context("scroll requires snapshot")?;
        let previous_offset = self.panes.pane.scrollback_viewport.offset_from_bottom();
        match action {
            MouseAction::WheelUp => self
                .panes
                .pane
                .scrollback_viewport
                .scroll_up(lines, snapshot),
            MouseAction::WheelDown => self
                .panes
                .pane
                .scrollback_viewport
                .scroll_down(lines, snapshot),
            _ => return Ok(false),
        }
        let current_offset = self.panes.pane.scrollback_viewport.offset_from_bottom();
        let moved = current_offset != previous_offset;
        request_return_live_resync(
            self.panes.pane.commands.as_ref(),
            &mut self.panes.pane.resync_pending,
            previous_offset,
            current_offset,
        )?;
        if action == MouseAction::WheelUp {
            let loaded = snapshot.scrollback_rows.len();
            let remaining =
                loaded.saturating_sub(self.panes.pane.scrollback_viewport.offset_from_bottom());
            let prefetch_distance = snapshot.rows.saturating_mul(2).max(32);
            if remaining <= prefetch_distance {
                self.request_older_history()?;
            }
        }
        // Scrolling back to live ends a hold; its cache no longer joins the screen.
        self.panes.pane.discard_lagging_history();
        if !moved {
            return Ok(false);
        }
        self.panes
            .pane
            .scroll_started_at
            .get_or_insert_with(Instant::now);
        self.invalidate_viewport_local_state();
        self.refresh_ime_preedit()?;
        self.update_ime_cursor_rectangle();
        // Coalesce high-resolution wheel events until the next compositor frame.
        // Re-shaping the entire viewport synchronously for every axis event made
        // fast scrolling stall the Wayland dispatch loop.
        self.panes.pane.viewport_dirty = true;
        Ok(true)
    }

    fn tick_signoff(&mut self, queue_handle: &QueueHandle<Self>) -> Result<()> {
        let Some(mut probe) = self.scheduling.signoff.take() else {
            return Ok(());
        };
        let result = self.advance_signoff(&mut probe);
        if let Err(error) = &result {
            self.write_signoff_report(&probe, false, Some(&error.to_string()))?;
        } else {
            self.write_signoff_report(&probe, probe.step == SignoffStep::Complete, None)?;
        }
        self.scheduling.signoff = Some(probe);
        result?;
        if self.surface.configured && self.panes.pane.viewport_dirty {
            self.schedule_draw(queue_handle)?;
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the development-only sign-off state machine is one auditable scenario"
    )]
    fn advance_signoff(&mut self, probe: &mut SignoffProbe) -> Result<()> {
        anyhow::ensure!(
            probe.started_at.elapsed() < Duration::from_secs(60),
            "sign-off probe timed out at {:?}",
            probe.step
        );
        let Some(snapshot) = self.panes.pane.snapshot.as_ref() else {
            return Ok(());
        };
        match probe.step {
            SignoffStep::WaitHistory => {
                if snapshot.available_scrollback_rows >= 5_000 {
                    probe.step = SignoffStep::LoadSelectionWindow;
                }
            }
            SignoffStep::LoadSelectionWindow => {
                self.scroll_history(MouseAction::WheelUp, usize::MAX)?;
                if self
                    .panes
                    .pane
                    .snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.scrollback_rows.len() >= 640)
                {
                    probe.step = SignoffStep::BeginSelection;
                }
            }
            SignoffStep::LoadClientCache => {
                self.scroll_history(MouseAction::WheelUp, usize::MAX)?;
                let snapshot = self
                    .panes
                    .pane
                    .snapshot
                    .as_ref()
                    .context("sign-off snapshot")?;
                let cache_bytes = history_cache_bytes(&snapshot.scrollback_rows);
                let loaded_rows = snapshot.scrollback_rows.len();
                let first_row_id = snapshot.scrollback_rows.first().and_then(|row| row.row_id);
                let bounded_eviction_observed = probe.cache_window.zip(first_row_id).is_some_and(
                    |((previous_rows, previous_first), current_first)| {
                        loaded_rows <= previous_rows && current_first < previous_first
                    },
                );
                if let Some(first_row_id) = first_row_id {
                    probe.cache_window = Some((loaded_rows, first_row_id));
                }
                let row_capacity_hit = loaded_rows >= MAX_CACHED_HISTORY_ROWS;
                let byte_capacity_hit = cache_bytes >= MAX_CACHED_HISTORY_BYTES;
                if (row_capacity_hit || byte_capacity_hit || bounded_eviction_observed)
                    && self.panes.pane.scrollback_viewport.offset_from_bottom()
                        >= snapshot.scrollback_rows.len().saturating_sub(snapshot.rows)
                {
                    anyhow::ensure!(
                        snapshot.scrollback_rows.len() <= MAX_CACHED_HISTORY_ROWS
                            && cache_bytes <= MAX_CACHED_HISTORY_BYTES,
                        "client history cache exceeded a bound"
                    );
                    probe.evidence.push(serde_json::json!({
                        "check": "client_history_cache",
                        "loaded_rows": loaded_rows,
                        "loaded_bytes": cache_bytes,
                        "available_rows": snapshot.available_scrollback_rows,
                        "omitted_oldest_rows": snapshot.omitted_oldest_scrollback_rows,
                        "offset": self.panes.pane.scrollback_viewport.offset_from_bottom(),
                        "row_capacity_hit": row_capacity_hit,
                        "byte_capacity_hit": byte_capacity_hit,
                        "bounded_eviction_observed": bounded_eviction_observed,
                        "bounded": true,
                    }));
                    probe.step = SignoffStep::WaitMouseTracking;
                }
            }
            SignoffStep::BeginSelection => {
                if self.begin_selection(CellPosition { row: 0, column: 0 }) {
                    probe.selection_revision = self
                        .panes
                        .pane
                        .snapshot
                        .as_ref()
                        .context("selection snapshot")?
                        .revision;
                    probe.step = SignoffStep::ExtendSelection;
                }
            }
            SignoffStep::ExtendSelection => {
                self.scroll_history(MouseAction::WheelDown, 512)?;
                let row = self
                    .panes
                    .display_snapshot()
                    .context("selection display")?
                    .rows
                    .saturating_sub(1);
                anyhow::ensure!(
                    self.extend_selection(CellPosition { row, column: 8 }),
                    "could not extend cross-page selection"
                );
                let selection = self.panes.pane.selection.context("selection exists")?;
                let row_span = selection.anchor.row_id.abs_diff(selection.end.row_id);
                anyhow::ensure!(row_span > 256, "selection did not cross a history page");
                probe.evidence.push(serde_json::json!({
                    "check": "cross_page_selection",
                    "anchor_row_id": selection.anchor.row_id,
                    "end_row_id": selection.end.row_id,
                    "row_id_span": row_span,
                }));
                probe.step = SignoffStep::WaitSelectedOutput;
            }
            SignoffStep::WaitSelectedOutput => {
                if snapshot.revision >= probe.selection_revision.saturating_add(20) {
                    let selection = self
                        .panes
                        .pane
                        .selection
                        .context("selection survived output")?;
                    anyhow::ensure!(
                        selection_is_retained(snapshot, &snapshot.visible_rows, selection),
                        "selection endpoints were not retained during output"
                    );
                    probe.evidence.push(serde_json::json!({
                        "check": "selection_during_detached_output",
                        "revision_before": probe.selection_revision,
                        "revision_after": snapshot.revision,
                        "unseen_rows": self.panes.pane.scrollback_viewport.unseen_rows(),
                        "retained": true,
                    }));
                    probe.step = SignoffStep::FinishSelection;
                }
            }
            SignoffStep::FinishSelection => {
                let copied_bytes = self.finish_selection().map_or(0, <[u8]>::len);
                anyhow::ensure!(copied_bytes > 0, "cross-page copy was empty");
                probe.evidence.push(serde_json::json!({
                    "check": "cross_page_copy",
                    "copied_bytes": copied_bytes,
                    "content_recorded": false,
                }));
                probe.step = SignoffStep::LocalWheel;
            }
            SignoffStep::LocalWheel => {
                let outcome = self.handle_vertical_wheel(None, 0.0, 1, 0)?;
                let WheelOutcome::History { before, after } = outcome else {
                    anyhow::bail!("wheel without mouse tracking did not scroll history");
                };
                anyhow::ensure!(
                    after < before,
                    "local wheel did not move toward live output"
                );
                probe.evidence.push(serde_json::json!({
                    "check": "local_history_wheel",
                    "before": before,
                    "after": after,
                }));
                self.dirty_selection(self.panes.pane.selection);
                self.panes.pane.selection = None;
                self.panes.pane.selected_text = None;
                self.panes.pane.history_selection_pin_blocked = false;
                probe.step = SignoffStep::LoadClientCache;
            }
            SignoffStep::WaitMouseTracking => {
                if snapshot.input_modes.mouse_tracking != MouseTracking::None {
                    probe.step = SignoffStep::ApplicationWheel;
                }
            }
            SignoffStep::ApplicationWheel => {
                let tracking = snapshot.input_modes.mouse_tracking;
                let sgr = snapshot.input_modes.sgr_mouse;
                let outcome = self.handle_vertical_wheel(
                    Some(CellPosition { row: 2, column: 2 }),
                    0.0,
                    0,
                    -120,
                )?;
                let WheelOutcome::Application { reports, bytes } = outcome else {
                    anyhow::bail!("tracked wheel did not emit an application report");
                };
                probe.evidence.push(serde_json::json!({
                    "check": "application_mouse_wheel",
                    "tracking": format!("{tracking:?}"),
                    "sgr": sgr,
                    "reports": reports,
                    "bytes": bytes,
                    "content_recorded": false,
                }));
                probe.step = SignoffStep::ReturnLive;
            }
            SignoffStep::ReturnLive => {
                self.scroll_history(MouseAction::WheelDown, usize::MAX)?;
                if self.panes.pane.scrollback_viewport.is_live() {
                    probe.evidence.push(serde_json::json!({
                        "check": "return_to_live",
                        "offset": 0,
                    }));
                    probe.step = SignoffStep::Complete;
                }
            }
            SignoffStep::Complete => {}
        }
        Ok(())
    }

    fn write_signoff_report(
        &self,
        probe: &SignoffProbe,
        exact: bool,
        error: Option<&str>,
    ) -> Result<()> {
        let snapshot = self.panes.pane.snapshot.as_ref();
        let report = serde_json::json!({
            "schema": "splinterm.signoff.interactions.v1",
            "exact": exact,
            "step": format!("{:?}", probe.step),
            "elapsed_ms": probe.started_at.elapsed().as_millis(),
            "error": error,
            "state": {
                "revision": snapshot.map(|value| value.revision),
                "available_history_rows": snapshot.map(|value| value.available_scrollback_rows),
                "loaded_history_rows": snapshot.map(|value| value.scrollback_rows.len()),
                "loaded_history_bytes": snapshot.map(|value| history_cache_bytes(&value.scrollback_rows)),
                "first_loaded_row_id": snapshot.and_then(|value| value.scrollback_rows.first()).and_then(|row| row.row_id),
                "omitted_oldest_rows": snapshot.map(|value| value.omitted_oldest_scrollback_rows),
                "history_page_pending": self.panes.pane.history_page_pending,
                "viewport_offset": self.panes.pane.scrollback_viewport.offset_from_bottom(),
                "mouse_tracking": snapshot.map(|value| format!("{:?}", value.input_modes.mouse_tracking)),
                "selection_active": self.panes.pane.selection.is_some(),
            },
            "evidence": probe.evidence,
        });
        let temporary = probe.report_path.with_extension("tmp");
        let mut bytes = serde_json::to_vec_pretty(&report)?;
        bytes.push(b'\n');
        std::fs::write(&temporary, bytes).context("write sign-off report")?;
        std::fs::rename(&temporary, &probe.report_path).context("publish sign-off report")
    }

    fn apply_history_navigation(
        &mut self,
        navigation: HistoryNavigation,
        queue_handle: &QueueHandle<Self>,
    ) -> Result<()> {
        let page = self
            .panes
            .pane
            .snapshot
            .as_ref()
            .map_or(1, |snapshot| snapshot.rows.saturating_sub(1).max(1));
        let changed = match navigation {
            HistoryNavigation::PageUp => self.scroll_history(MouseAction::WheelUp, page)?,
            HistoryNavigation::PageDown => self.scroll_history(MouseAction::WheelDown, page)?,
            HistoryNavigation::ReturnToLive => {
                self.scroll_history(MouseAction::WheelDown, usize::MAX)?
            }
        };
        if changed && self.surface.configured {
            self.schedule_draw(queue_handle)?;
        }
        Ok(())
    }

    fn handle_history_key(
        &mut self,
        event: &KeyEvent,
        queue_handle: &QueueHandle<Self>,
    ) -> Result<bool> {
        let action = shortcut_action_for(&self.input.keymap, event, self.input.modifiers);
        let screen = self
            .panes
            .pane
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.active_screen);
        match history_key(
            action,
            self.panes.pane.scrollback_viewport.is_live(),
            screen,
        ) {
            HistoryKey::Navigate(navigation) => {
                self.apply_history_navigation(navigation, queue_handle)?;
                Ok(true)
            }
            HistoryKey::Consume => Ok(true),
            HistoryKey::Forward => Ok(false),
        }
    }

    fn send_input(&mut self, bytes: Vec<u8>) -> Result<()> {
        if self.pty_input_blocked() {
            return Ok(());
        }
        self.queue_terminal_input(bytes)
    }

    fn handle_vertical_wheel(
        &mut self,
        position: Option<CellPosition>,
        absolute: f64,
        discrete: i32,
        value120: i32,
    ) -> Result<WheelOutcome> {
        let modes = self.panes.input_modes();
        let cell_height = self
            .panes
            .pane
            .snapshot_frame
            .as_ref()
            .map_or(1, SnapshotFrame::cell_height);
        if modes.mouse_tracking == MouseTracking::None {
            let before = self.panes.pane.scrollback_viewport.offset_from_bottom();
            let Some((action, count)) = self.input.scrollback_wheel.push_scaled(
                absolute,
                discrete,
                value120,
                SCROLLBACK_WHEEL_MULTIPLIER,
                cell_height,
            ) else {
                return Ok(WheelOutcome::Noop);
            };
            self.scroll_history(action, count)?;
            return Ok(WheelOutcome::History {
                before,
                after: self.panes.pane.scrollback_viewport.offset_from_bottom(),
            });
        }
        let Some((action, count)) =
            self.input
                .vertical_wheel
                .push(absolute, discrete, value120, cell_height)
        else {
            return Ok(WheelOutcome::Noop);
        };
        let Some(position) = position else {
            return Ok(WheelOutcome::Noop);
        };
        let Some(report) = mouse_report(action, position, self.input.modifiers, modes.sgr_mouse)
        else {
            return Ok(WheelOutcome::Noop);
        };
        let bytes = report.len().saturating_mul(count);
        let mut batch = Vec::with_capacity(bytes);
        for _ in 0..count {
            batch.extend_from_slice(&report);
        }
        self.send_command(WindowCommand::Input(batch));
        Ok(WheelOutcome::Application {
            reports: count,
            bytes,
        })
    }

    fn enter_copy_mode(&mut self) -> bool {
        if self.modal.input_modal_open() || self.panes.pane.search.input.is_some() {
            return false;
        }
        let Some(state) = (|| {
            let snapshot = self.panes.pane.snapshot.as_ref()?;
            let display = self.panes.display_snapshot_cow()?;
            copy_mode_enter(snapshot, &display)
        })() else {
            return false;
        };
        self.clear_selection();
        self.modal.copy_mode = Some(state);
        self.panes.pane.selection = Some(state.overlay_selection());
        self.panes.pane.selecting = false;
        self.input.prefix_state.clear();
        self.input.input_generation = self.input.input_generation.saturating_add(1);
        self.clear_ime_preedit();
        self.surface.window.set_title("Splinterm — COPY");
        self.presentation.full_redraw = true;
        true
    }

    fn close_copy_mode(&mut self) -> bool {
        if self.modal.copy_mode.take().is_none() {
            return false;
        }
        self.clear_selection();
        self.input.input_generation = self.input.input_generation.saturating_add(1);
        self.update_window_title();
        self.presentation.full_redraw = true;
        true
    }

    fn cancel_copy_mode_for_topology(&mut self) {
        if self.modal.copy_mode.take().is_none() {
            return;
        }
        self.panes.pane.clear_local_content_state();
        for pane in &mut self.panes.inactive_panes {
            pane.clear_local_content_state();
        }
        for tab in self.tab_state.tabs.iter_mut() {
            if let Some(view) = tab.value.as_mut() {
                view.pane.clear_local_content_state();
                for pane in &mut view.inactive_panes {
                    pane.clear_local_content_state();
                }
            }
        }
        self.input.input_generation = self.input.input_generation.saturating_add(1);
        self.presentation.full_redraw = true;
    }

    fn publish_copy_mode_selection_and_close(
        &mut self,
        state: CopyModeState,
        queue_handle: &QueueHandle<Self>,
        serial: u32,
    ) {
        if state.selecting() {
            self.modal.copy_mode = Some(state);
            self.panes.pane.selection = Some(state.overlay_selection());
            self.panes.pane.selecting = true;
            self.finish_selection();
            self.publish_clipboard(queue_handle, serial, false);
        }
        self.close_copy_mode();
    }

    fn handle_copy_mode_key(
        &mut self,
        event: &KeyEvent,
        queue_handle: &QueueHandle<Self>,
        serial: u32,
    ) -> Result<bool> {
        let Some(mut state) = self.modal.copy_mode else {
            return Ok(false);
        };
        if !self
            .panes
            .pane
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| copy_mode_is_valid(snapshot, state))
        {
            self.close_copy_mode();
            return Ok(true);
        }
        let plain =
            !self.input.modifiers.ctrl && !self.input.modifiers.alt && !self.input.modifiers.logo;
        match copy_mode_desktop_action(event.keysym, self.input.modifiers) {
            Some(CopyModeDesktopAction::CopySelection) => {
                self.publish_copy_mode_selection_and_close(state, queue_handle, serial);
                return Ok(true);
            }
            Some(CopyModeDesktopAction::Consume) => return Ok(true),
            None => {}
        }
        match event.keysym {
            Keysym::Escape => {
                self.close_copy_mode();
                return Ok(true);
            }
            Keysym::v | Keysym::V if plain => {
                state.begin_selection();
                self.modal.copy_mode = Some(state);
                self.dirty_selection(self.panes.pane.selection);
                self.panes.pane.selection = Some(state.overlay_selection());
                self.panes.pane.selecting = true;
                self.dirty_selection(self.panes.pane.selection);
                self.presentation.full_redraw = true;
                return Ok(true);
            }
            Keysym::y | Keysym::Y if plain => {
                self.publish_copy_mode_selection_and_close(state, queue_handle, serial);
                return Ok(true);
            }
            _ => {}
        }
        let page = self
            .panes
            .pane
            .snapshot
            .as_ref()
            .map_or(1, |snapshot| snapshot.rows.saturating_sub(1).max(1));
        let motion = match event.keysym {
            Keysym::h | Keysym::H | Keysym::Left if plain => Some(CopyMotion::Left),
            Keysym::l | Keysym::L | Keysym::Right if plain => Some(CopyMotion::Right),
            Keysym::k | Keysym::K | Keysym::Up if plain => Some(CopyMotion::Up(1)),
            Keysym::j | Keysym::J | Keysym::Down if plain => Some(CopyMotion::Down(1)),
            Keysym::w | Keysym::W if plain => Some(CopyMotion::WordForward),
            Keysym::b | Keysym::B if plain => Some(CopyMotion::WordBackward),
            Keysym::e | Keysym::E if plain => Some(CopyMotion::WordEnd),
            Keysym::Page_Up => Some(CopyMotion::Up(page)),
            Keysym::Page_Down => Some(CopyMotion::Down(page)),
            Keysym::_0 | Keysym::Home if plain => Some(CopyMotion::LineStart),
            Keysym::dollar | Keysym::End if plain => Some(CopyMotion::LineEnd),
            _ => None,
        };
        let Some(motion) = motion else {
            return Ok(true);
        };
        let outcome = self
            .panes
            .pane
            .snapshot
            .as_ref()
            .map_or(CopyMoveOutcome::default(), |snapshot| {
                move_copy_cursor(snapshot, &mut state, motion)
            });
        if outcome.request_older {
            self.request_older_history()?;
        }
        if outcome.moved {
            self.dirty_selection(self.panes.pane.selection);
            if let Some(snapshot) = self.panes.pane.snapshot.as_ref() {
                self.panes
                    .pane
                    .scrollback_viewport
                    .reveal_row(state.cursor.row_id, snapshot);
            }
            self.modal.copy_mode = Some(state);
            self.panes.pane.selection = Some(state.overlay_selection());
            self.panes.pane.selecting = state.selecting();
            self.dirty_selection(self.panes.pane.selection);
            self.panes.pane.viewport_dirty = true;
            self.presentation.full_redraw = true;
        }
        Ok(true)
    }

    fn clear_selection(&mut self) {
        let selection = self.panes.pane.selection.take();
        self.dirty_selection(selection);
        self.panes.pane.selected_text = None;
        self.panes.pane.selecting = false;
        self.panes.pane.history_selection_pin_blocked = false;
    }

    fn begin_selection(&mut self, position: CellPosition) -> bool {
        let Some(snapshot) = self.panes.display_snapshot_cow() else {
            return false;
        };
        let Some(endpoint) = selection_endpoint(&snapshot, position) else {
            return false;
        };
        self.dirty_selection(self.panes.pane.selection);
        let selection = Selection {
            anchor: endpoint,
            end: endpoint,
        };
        self.panes.pane.selection = Some(selection);
        self.panes.pane.selecting = true;
        self.panes.pane.history_selection_pin_blocked = false;
        self.dirty_selection(Some(selection));
        true
    }

    fn extend_selection(&mut self, position: CellPosition) -> bool {
        let (Some(mut selection), Some(snapshot)) =
            (self.panes.pane.selection, self.panes.display_snapshot_cow())
        else {
            return false;
        };
        let Some(endpoint) = selection_endpoint(&snapshot, position) else {
            return false;
        };
        if endpoint == selection.end {
            return false;
        }
        self.dirty_selection(Some(selection));
        selection.end = endpoint;
        self.panes.pane.selection = Some(selection);
        self.dirty_selection(Some(selection));
        true
    }

    fn finish_selection(&mut self) -> Option<&[u8]> {
        self.panes.pane.selecting = false;
        self.panes.pane.selected_text = self.current_selection_text();
        self.panes.pane.selected_text.as_deref()
    }

    fn pointer_cell_at(&self, position: (f64, f64)) -> Option<CellPosition> {
        let frame = self.panes.pane.snapshot_frame.as_ref()?;
        let pane_rect = self.panes.focused_splint().and_then(|splint_id| {
            self.computed_pane_layout()
                .and_then(|layout| layout.rect(splint_id))
        });
        let content = self.content_rect();
        let (logical_width, logical_height, x, y) = pane_rect.map_or(
            (
                content.width,
                content.height,
                f64::from(content.x),
                f64::from(content.y),
            ),
            |rect| {
                (
                    rect.width,
                    rect.height,
                    f64::from(rect.x),
                    f64::from(rect.y),
                )
            },
        );
        let geometry = self
            .panes
            .pane
            .window_geometry(frame, logical_width, logical_height, self.surface.scale_120)
            .ok()?;
        let (row, column) = frame.cell_at(position.0 - x, position.1 - y, &geometry)?;
        Some(CellPosition { row, column })
    }

    fn dirty_row(&mut self, row: usize) {
        let rows = self
            .panes
            .pane
            .snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.rows);
        self.panes.pane.raster_dirty_rows.resize(rows, false);
        self.panes.pane.surface_dirty_rows.resize(rows, false);
        if row < rows {
            self.panes.pane.raster_dirty_rows[row] = true;
            self.panes.pane.surface_dirty_rows[row] = true;
        }
    }

    /// The text the selection covers in the rows this pane shows.
    fn current_selection_text(&self) -> Option<Vec<u8>> {
        let selection = self.panes.pane.selection?;
        let snapshot = self.panes.pane.snapshot.as_ref()?;
        let screen = self.panes.pane.selection_screen()?;
        selection_text(snapshot, screen, selection).map(String::into_bytes)
    }

    fn dirty_selection(&mut self, selection: Option<Selection>) {
        let bounds = selection.and_then(|selection| {
            let snapshot = self.panes.pane.snapshot.as_ref()?;
            let screen = self.panes.pane.selection_screen()?;
            let display = self.panes.display_snapshot_cow()?;
            selection_display_bounds(snapshot, screen, &display, selection)
        });
        if let Some((start, end)) = bounds {
            let rows = self
                .panes
                .pane
                .snapshot
                .as_ref()
                .map_or(0, |snapshot| snapshot.rows);
            self.panes.pane.surface_dirty_rows.resize(rows, false);
            for row in start.row..=end.row.min(rows.saturating_sub(1)) {
                self.panes.pane.surface_dirty_rows[row] = true;
            }
        }
    }

    fn invalidate_viewport_local_state(&mut self) {
        if let Some((start, _, _)) = &self.panes.pane.hovered_url {
            self.dirty_row(start.row);
        }
        self.panes.pane.selected_text = None;
        self.panes.pane.hovered_url = None;
        let selecting = self.panes.pane.selecting;
        self.input.pressed_buttons.retain(|_, owner| {
            matches!(owner, PressOwner::Application { .. })
                || selecting && matches!(owner, PressOwner::Selection)
        });
    }

    fn invalidate_local_content_state(&mut self) {
        self.modal.copy_mode = None;
        self.dirty_selection(self.panes.pane.selection);
        self.panes.pane.selection = None;
        self.panes.pane.selecting = false;
        self.panes.pane.history_selection_pin_blocked = false;
        self.invalidate_viewport_local_state();
    }

    fn reconcile_selection_after_content_change(&mut self) {
        let retained = self.panes.pane.selection.is_none_or(|selection| {
            self.panes
                .pane
                .snapshot
                .as_ref()
                .zip(self.panes.pane.selection_screen())
                .is_some_and(|(snapshot, screen)| {
                    selection_is_retained(snapshot, screen, selection)
                })
        });
        let copy_mode_retained = self.modal.copy_mode.is_none_or(|state| {
            self.panes
                .pane
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| copy_mode_is_valid(snapshot, state))
        });
        if !retained || !copy_mode_retained {
            self.modal.copy_mode = None;
            self.panes.pane.selection = None;
            self.panes.pane.selecting = false;
            self.panes.pane.history_selection_pin_blocked = false;
        }
        let previous_text = self.panes.pane.selected_text.take();
        self.invalidate_viewport_local_state();
        // A finished selection that survives new output still copies. Its
        // rows keep their text, but read them again in case one changed in
        // place; a row that left the cache keeps the earlier text.
        if !self.panes.pane.selecting && self.panes.pane.selection.is_some() {
            self.panes.pane.selected_text = self.current_selection_text().or(previous_text);
        }
    }

    fn recompute_hovered_url(&mut self) {
        let previous = self.panes.pane.hovered_url.take();
        let display = self.panes.display_snapshot();
        self.panes.pane.hovered_url = self.panes.pane.pointer_cell.and_then(|position| {
            display
                .as_ref()
                .and_then(|snapshot| url_at(snapshot, position))
        });
        if previous != self.panes.pane.hovered_url {
            if let Some((start, _, _)) = previous {
                self.dirty_row(start.row);
            }
            if let Some((start, _, _)) = &self.panes.pane.hovered_url {
                self.dirty_row(start.row);
            }
        }
    }

    /// A trusted consent prompt or a non-inline session picker. Each blocks
    /// terminal input and Explorer search, but not an open terminal search.
    fn consent_or_picker_open(&self) -> bool {
        self.modal.session_picker.is_some() || self.modal.trusted_consent.is_some()
    }

    /// Whether bytes bound for the PTY must be dropped now.
    fn pty_input_blocked(&self) -> bool {
        terminal_input_blocked(
            self.modal.input_modal_open(),
            self.modal.trusted_consent.is_some(),
            self.handoff_paused(),
        )
    }

    /// The IME routing decision for the current Window state; see `ime_gate`.
    fn ime_gate(&self) -> ImeGate {
        ime_gate(ImeContext {
            input_modal_open: self.modal.input_modal_open(),
            consent_or_picker_open: self.consent_or_picker_open(),
            explorer_focused: self.explorer.focused(),
            // A prompt above the palette owns the keyboard.
            binding_help: self.modal.binding_help.is_some() && self.modal.dojo_prompt.is_none(),
            command_palette: self.modal.command_palette.is_some()
                && self.modal.dojo_prompt.is_none(),
            dojo_prompt_input: self
                .modal
                .dojo_prompt
                .as_ref()
                .is_some_and(|prompt| prompt.input().is_some()),
            terminal_search: self.panes.pane.search.input.is_some(),
            terminal_input_paused: self.handoff_paused(),
        })
    }

    fn active_owned_field(&mut self) -> Option<(OwnedFieldTarget, &mut BoundedTextEditor)> {
        let input_modal_open = self.modal.input_modal_open();
        let consent_or_picker_open = self.consent_or_picker_open();
        let explorer_search = self.explorer.focused() && self.explorer.search_active();
        // The palette's field is the access list's search while it shows,
        // and no field, nor binding help's, while a prompt stands above it.
        let prompt_open = self.modal.dojo_prompt.is_some();
        let palette_field = if prompt_open {
            None
        } else if let Some(access) = self.modal.agent_access.as_mut() {
            Some(access.editor_mut())
        } else {
            self.modal
                .command_palette
                .as_mut()
                .map(CommandPaletteUi::editor_mut)
        };
        Self::select_owned_field(
            input_modal_open,
            consent_or_picker_open,
            self.modal
                .binding_help
                .as_mut()
                .filter(|_| !prompt_open)
                .map(BindingHelpUi::editor_mut),
            palette_field,
            self.modal
                .dojo_prompt
                .as_mut()
                .and_then(DojoPromptUi::editor_mut),
            explorer_search.then(|| self.explorer.editor_mut()),
            self.panes.pane.search.input.as_mut(),
        )
    }

    /// Chooses the field that owns typed text and IME composition. Binding
    /// help, the command palette, and Dojo prompt input take priority. An
    /// input modal blocks both background searches. A consent prompt or
    /// non-inline picker blocks Explorer search but leaves an open terminal
    /// search as the destination, so text never reaches the PTY beneath it.
    fn select_owned_field<T>(
        input_modal_open: bool,
        consent_or_picker_open: bool,
        binding_help: Option<T>,
        palette: Option<T>,
        prompt: Option<T>,
        explorer: Option<T>,
        terminal_search: Option<T>,
    ) -> Option<(OwnedFieldTarget, T)> {
        if let Some(field) = binding_help {
            return Some((OwnedFieldTarget::BindingHelp, field));
        }
        if let Some(field) = palette {
            return Some((OwnedFieldTarget::CommandPalette, field));
        }
        if let Some(field) = prompt {
            return Some((OwnedFieldTarget::DojoPrompt, field));
        }
        if input_modal_open {
            return None;
        }
        // Explorer keeps its focus while a modal is open, but must not keep
        // ownership of text editing above that modal.
        if !consent_or_picker_open && let Some(field) = explorer {
            return Some((OwnedFieldTarget::ExplorerSearch, field));
        }
        terminal_search.map(|field| (OwnedFieldTarget::Search, field))
    }

    fn owned_field_editor_mut(
        &mut self,
        target: OwnedFieldTarget,
    ) -> Option<&mut BoundedTextEditor> {
        match target {
            OwnedFieldTarget::BindingHelp => self
                .modal
                .binding_help
                .as_mut()
                .map(BindingHelpUi::editor_mut),
            OwnedFieldTarget::CommandPalette => {
                if let Some(access) = self.modal.agent_access.as_mut() {
                    Some(access.editor_mut())
                } else {
                    self.modal
                        .command_palette
                        .as_mut()
                        .map(CommandPaletteUi::editor_mut)
                }
            }
            OwnedFieldTarget::DojoPrompt => self
                .modal
                .dojo_prompt
                .as_mut()
                .and_then(DojoPromptUi::editor_mut),
            OwnedFieldTarget::ExplorerSearch => self
                .explorer
                .search_active()
                .then(|| self.explorer.editor_mut()),
            OwnedFieldTarget::Search => self.panes.pane.search.input.as_mut(),
        }
    }

    fn refresh_owned_field(&mut self, target: OwnedFieldTarget) {
        match target {
            OwnedFieldTarget::BindingHelp => {
                if let Some(help) = self.modal.binding_help.as_mut() {
                    help.editor_changed();
                }
                self.refresh_command_palette();
            }
            OwnedFieldTarget::CommandPalette => {
                if let Some(access) = self.modal.agent_access.as_mut() {
                    access.editor_changed();
                } else if let Some(palette) = self.modal.command_palette.as_mut() {
                    palette.editor_changed();
                }
                self.refresh_command_palette();
            }
            OwnedFieldTarget::DojoPrompt => self.refresh_dojo_prompt(),
            OwnedFieldTarget::ExplorerSearch => {
                self.presentation.full_redraw = true;
            }
            OwnedFieldTarget::Search => {
                self.update_window_title();
                self.presentation.full_redraw = true;
            }
        }
    }

    fn owned_field_defers_to_modal(
        target: OwnedFieldTarget,
        keysym: Keysym,
        modifiers: Modifiers,
    ) -> bool {
        if target == OwnedFieldTarget::BindingHelp
            && modifiers.ctrl
            && !modifiers.alt
            && !modifiers.logo
            && matches!(keysym, Keysym::u | Keysym::U)
        {
            return true;
        }
        match target {
            OwnedFieldTarget::BindingHelp | OwnedFieldTarget::CommandPalette => matches!(
                keysym,
                Keysym::Up
                    | Keysym::Down
                    | Keysym::Home
                    | Keysym::End
                    | Keysym::Page_Up
                    | Keysym::Page_Down
                    | Keysym::Return
                    | Keysym::KP_Enter
                    | Keysym::Escape
            ),
            OwnedFieldTarget::DojoPrompt
            | OwnedFieldTarget::ExplorerSearch
            | OwnedFieldTarget::Search => {
                matches!(keysym, Keysym::Return | Keysym::KP_Enter | Keysym::Escape)
            }
        }
    }

    fn editable_field_text(text: Option<&str>) -> Option<&str> {
        text.filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
    }

    fn owned_field_consumes_repeat(&mut self, event: &KeyEvent) -> bool {
        self.active_owned_field().is_some()
            && (self.input.modifiers.logo
                || owned_field_clipboard_action(
                    &self.input.keymap,
                    event.keysym,
                    self.input.modifiers,
                )
                .is_some())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one ordered boundary keeps field clipboard publication and bounded edits atomic"
    )]
    fn handle_owned_field_key(
        &mut self,
        event: &KeyEvent,
        queue_handle: &QueueHandle<Self>,
        serial: u32,
    ) -> bool {
        let Some((target, _)) = self.active_owned_field() else {
            return false;
        };
        if Self::owned_field_defers_to_modal(target, event.keysym, self.input.modifiers) {
            return false;
        }
        let desktop = self.input.modifiers.logo
            && !self.input.modifiers.ctrl
            && !self.input.modifiers.alt
            && !self.input.modifiers.shift;
        let plain =
            !self.input.modifiers.logo && !self.input.modifiers.ctrl && !self.input.modifiers.alt;
        let extend = self.input.modifiers.shift;
        let clipboard_available = self.clipboard.data_device.is_some();
        let clipboard_action =
            owned_field_clipboard_action(&self.input.keymap, event.keysym, self.input.modifiers);
        let mut changed = false;
        let mut copied = None;
        let mut paste = false;
        let handled = if clipboard_action == Some(ActionId::ClipboardCopy) {
            copied = self
                .owned_field_editor_mut(target)
                .and_then(|editor| editor.selected_text().map(str::as_bytes))
                .map(<[u8]>::to_vec);
            true
        } else if clipboard_action == Some(ActionId::ClipboardPaste) {
            paste = true;
            true
        } else if desktop {
            match event.keysym {
                Keysym::c | Keysym::C => {
                    copied = self
                        .owned_field_editor_mut(target)
                        .and_then(|editor| editor.selected_text().map(str::as_bytes))
                        .map(<[u8]>::to_vec);
                    true
                }
                Keysym::v | Keysym::V => {
                    paste = true;
                    true
                }
                Keysym::x | Keysym::X => {
                    if clipboard_available {
                        copied = self
                            .owned_field_editor_mut(target)
                            .and_then(BoundedTextEditor::cut)
                            .map(String::into_bytes);
                        changed = copied.is_some();
                    }
                    true
                }
                Keysym::z | Keysym::Z => {
                    changed = self
                        .owned_field_editor_mut(target)
                        .is_some_and(BoundedTextEditor::undo);
                    true
                }
                Keysym::a | Keysym::A => {
                    changed = self
                        .owned_field_editor_mut(target)
                        .is_some_and(BoundedTextEditor::select_all);
                    true
                }
                _ => true,
            }
        } else if plain {
            match event.keysym {
                Keysym::BackSpace => {
                    changed = self
                        .owned_field_editor_mut(target)
                        .is_some_and(BoundedTextEditor::backspace);
                    true
                }
                Keysym::Left => {
                    changed = self
                        .owned_field_editor_mut(target)
                        .is_some_and(|editor| editor.move_left(extend));
                    true
                }
                Keysym::Right => {
                    changed = self
                        .owned_field_editor_mut(target)
                        .is_some_and(|editor| editor.move_right(extend));
                    true
                }
                Keysym::Home => {
                    changed = self
                        .owned_field_editor_mut(target)
                        .is_some_and(|editor| editor.move_home(extend));
                    true
                }
                Keysym::End => {
                    changed = self
                        .owned_field_editor_mut(target)
                        .is_some_and(|editor| editor.move_end(extend));
                    true
                }
                _ => Self::editable_field_text(event.utf8.as_deref()).is_some_and(|text| {
                    changed = self
                        .owned_field_editor_mut(target)
                        .is_some_and(|editor| editor.insert(text));
                    true
                }),
            }
        } else {
            false
        };
        if !handled {
            return false;
        }
        if let Some(copied) = copied {
            self.publish_clipboard_payload(queue_handle, serial, false, &copied);
        }
        if paste {
            self.begin_clipboard_read(PasteTarget::OwnedField(target));
        }
        if changed {
            self.refresh_owned_field(target);
        }
        true
    }

    fn begin_clipboard_read(&mut self, target: PasteTarget) {
        if self.pty_input_blocked() && !matches!(target, PasteTarget::OwnedField(_)) {
            return;
        }
        let tx = self.clipboard.clipboard_tx.clone();
        let waker = self.platform.update_waker.clone();
        let input_generation = self.input.input_generation;
        match target {
            PasteTarget::Clipboard | PasteTarget::OwnedField(_) => {
                let Some(offer) = self.clipboard.clipboard_offer.clone() else {
                    return;
                };
                let mime = offer.with_mime_types(accepted_text_mime);
                let Some(mime) = mime else { return };
                if let Ok(pipe) = offer.receive(mime) {
                    spawn_clipboard_read(pipe.into(), target, input_generation, None, tx, waker);
                }
            }
            PasteTarget::Primary => {
                let Some(offer) = self.clipboard.primary_offer.clone() else {
                    return;
                };
                let mime = offer.with_mime_types(accepted_text_mime);
                let Some(mime) = mime else { return };
                if let Ok(pipe) = offer.receive(mime) {
                    spawn_clipboard_read(pipe.into(), target, input_generation, None, tx, waker);
                }
            }
            PasteTarget::FileDrop(_) => {}
        }
    }

    fn apply_clipboard_reads(&mut self) -> Result<()> {
        while let Ok(read) = self.clipboard.clipboard_rx.try_recv() {
            if let Some(offer) = &read.drag_offer {
                offer.finish();
                offer.destroy();
            }
            if self.input.input_generation != read.input_generation {
                if matches!(read.target, PasteTarget::FileDrop(_)) {
                    eprintln!("splinterm file drop rejected: input target changed");
                }
                continue;
            }
            let Ok(bytes) = read.bytes else {
                if matches!(read.target, PasteTarget::FileDrop(_)) {
                    eprintln!("splinterm file drop rejected: offer read failed");
                }
                continue;
            };
            let Ok(bytes) = safe_paste(&bytes) else {
                if matches!(read.target, PasteTarget::FileDrop(_)) {
                    eprintln!("splinterm file drop rejected: unsafe offer bytes");
                }
                continue;
            };
            match read.target {
                PasteTarget::Clipboard | PasteTarget::Primary => {
                    if !clipboard_read_is_current(
                        self.pty_input_blocked(),
                        self.input.input_generation,
                        read.input_generation,
                    ) {
                        continue;
                    }
                    let bracketed = self
                        .panes
                        .pane
                        .snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.input_modes.bracketed_paste);
                    self.send_input(encode_bracketed_paste(bytes, bracketed))?;
                }
                PasteTarget::OwnedField(target) => {
                    let text = std::str::from_utf8(bytes)
                        .expect("safe_paste already validated clipboard UTF-8");
                    if self
                        .owned_field_editor_mut(target)
                        .is_some_and(|editor| editor.insert(text))
                    {
                        self.refresh_owned_field(target);
                    }
                }
                PasteTarget::FileDrop(target) => {
                    let Some(pane) = self.pane_for_splint(target.splint_id) else {
                        eprintln!("splinterm file drop rejected: target pane unavailable");
                        continue;
                    };
                    let Some(snapshot) = pane.snapshot.as_ref() else {
                        eprintln!("splinterm file drop rejected: target snapshot unavailable");
                        continue;
                    };
                    let current = FileDropTarget {
                        topology_revision: self.tab_state.active_identity.topology_revision,
                        dojo_id: self.tab_state.active_dojo_id(),
                        splint_id: snapshot.splint_id,
                        incarnation: snapshot.incarnation,
                        input_generation: self.input.input_generation,
                    };
                    if !file_drop_target_is_current(
                        target,
                        current,
                        self.pty_input_blocked(),
                        pane.controller_active,
                        pane.commands.is_some(),
                    ) {
                        eprintln!("splinterm file drop rejected: target became stale");
                        continue;
                    }
                    let Ok(payload) = dropped_file_payload(bytes) else {
                        eprintln!("splinterm file drop rejected: invalid URI list");
                        continue;
                    };
                    let encoded =
                        encode_bracketed_paste(&payload, snapshot.input_modes.bracketed_paste);
                    let Some(commands) = pane.commands.clone() else {
                        eprintln!("splinterm file drop rejected: command channel unavailable");
                        continue;
                    };
                    self.queue_terminal_input_for(Some(&commands), encoded)?;
                }
            }
        }
        Ok(())
    }

    fn publish_clipboard(&mut self, qh: &QueueHandle<Self>, serial: u32, primary: bool) {
        let Some(text) = self.panes.pane.selected_text.clone() else {
            return;
        };
        self.publish_clipboard_payload(qh, serial, primary, &text);
    }

    fn publish_clipboard_payload(
        &mut self,
        qh: &QueueHandle<Self>,
        serial: u32,
        primary: bool,
        text: &[u8],
    ) {
        if text.is_empty() {
            return;
        }
        let payload = Arc::<[u8]>::from(text.to_vec());
        let mimes: Vec<_> = TEXT_MIMES.iter().map(|mime| (*mime).to_owned()).collect();
        if primary {
            if let (Some(manager), Some(device)) = (
                self.platform.primary_selection_manager.as_ref(),
                self.clipboard.primary_device.as_ref(),
            ) {
                let source = manager.create_selection_source(qh, mimes);
                source.set_selection(device, serial);
                self.clipboard.primary_sources.clear();
                self.clipboard.primary_sources.push((source, payload));
            }
        } else if let Some(device) = self.clipboard.data_device.as_ref() {
            let source = self
                .platform
                .data_device_manager
                .create_copy_paste_source(qh, mimes);
            source.set_selection(device, serial);
            self.clipboard.clipboard_sources.clear();
            self.clipboard.clipboard_sources.push((source, payload));
        }
    }

    fn open_hovered_url(&self) {
        let Some((_, _, url)) = &self.panes.pane.hovered_url else {
            return;
        };
        let _ = crate::child_spawn::spawn(|| Command::new("xdg-open").arg(url).spawn());
    }

    fn settle_terminal_presses_for_picker(&mut self) {
        let pressed = std::mem::take(&mut self.input.pressed_buttons);
        for owner in pressed.into_values() {
            if let PressOwner::Application {
                code,
                sgr,
                modifiers,
                ..
            } = owner
                && let Some(position) = self.panes.pane.pointer_cell
                && let Some(report) =
                    mouse_report(MouseAction::Release(code), position, modifiers, sgr)
            {
                self.send_command(WindowCommand::Input(report));
            }
        }
        if self.panes.pane.selecting {
            let selection = self.panes.pane.selection.take();
            self.dirty_selection(selection);
            self.panes.pane.selecting = false;
            self.panes.pane.selected_text = None;
            self.panes.pane.history_selection_pin_blocked = false;
        }
        self.panes.pane.pointer_cell = None;
        self.panes.pane.hovered_url = None;
    }

    fn reconcile_terminal_focus_report(&mut self, modal_focus_changed: bool) {
        if !modal_focus_changed {
            return;
        }
        let Some(_) = reconciled_focus_report(
            self.panes.input_modes().focus_reporting,
            self.input.terminal_focus_reported,
            self.input.keyboard_focused,
        ) else {
            return;
        };
        self.request_active_pane_focus_report(self.input.keyboard_focused);
        self.input.terminal_focus_reported = self.input.keyboard_focused;
    }

    fn command_palette_available(&self) -> bool {
        self.tab_state.managed_tabs
            && self.tab_state.topology_commands.is_some()
            && self.modal.command_palette.is_none()
            && self.modal.dojo_prompt.is_none()
            && self.modal.tab_context_menu.is_none()
            && self.modal.session_picker.is_none()
            && self.modal.trusted_consent.is_none()
            && !self.modal.session_picker_requested
            && !self.modal.session_picker_reconcile_pending
            && !self.modal.command_palette_reconcile_pending
            && !self.tab_state.session_switch_pending
            && self.panes.pane.search.input.is_none()
            && self.input.divider_drag.is_none()
    }

    fn focused_cwd_source(&self) -> Result<SplintId> {
        let focused = self
            .panes
            .focused_splint()
            .context("cwd inheritance requires a focused Splint")?;
        anyhow::ensure!(
            self.panes
                .layout
                .as_ref()
                .and_then(|layout| layout.find_splint(focused))
                .is_some(),
            "focused Splint is absent from authoritative topology"
        );
        Ok(focused)
    }

    fn show_command_palette(&mut self) -> Result<()> {
        self.input.prefix_state.clear();
        self.modal.binding_help = None;
        self.modal.agent_access = None;
        anyhow::ensure!(
            self.command_palette_available(),
            "command palette is unavailable"
        );
        let splint_id = self
            .panes
            .focused_splint()
            .context("command palette requires a focused Splint")?;
        self.settle_terminal_presses_for_picker();
        self.input.input_generation = self.input.input_generation.saturating_add(1);
        self.input.ime_modal_barrier = self.input.ime.entered && self.input.text_input.is_some();
        if self.input.ime_modal_barrier {
            if let Some(text_input) = &self.input.text_input {
                text_input.disable();
            }
            self.commit_text_input();
        }
        self.clear_ime_preedit();
        self.modal.session_picker_wheel = WheelAccumulator::default();
        let multiple_tabs = self.tab_state.tabs.len() > 1;
        let active_dojo_id = self.tab_state.active_dojo_id();
        let active_tab_index = self
            .tab_state
            .tabs
            .iter()
            .position(|tab| tab.dojo_id == active_dojo_id)
            .context("active Dojo is missing from the Window tab set")?;
        self.clipboard.palette_image_target = self.clipboard_image_target().ok();
        self.modal.command_palette = Some(CommandPaletteUi::new(CommandPaletteContext {
            lair_id: self.tab_state.active_identity.lair_id,
            lair_retention: self.tab_state.active_identity.lair_retention,
            dojo_id: active_dojo_id,
            dojo_name: self.tab_state.active_identity.dojo_name.clone(),
            pane_count: 1_usize.saturating_add(self.panes.inactive_panes.len()),
            splint_id,
            dojo_splints: std::iter::once(&self.panes.pane)
                .chain(self.panes.inactive_panes.iter())
                .filter_map(|pane| {
                    pane.snapshot
                        .as_ref()
                        .map(|snapshot| (snapshot.splint_id, snapshot.incarnation))
                })
                .collect(),
            other_dojo_ids: self
                .tab_state
                .tabs
                .iter()
                .filter_map(|tab| (tab.dojo_id != active_dojo_id).then_some(tab.dojo_id))
                .collect(),
            previous_dojo_id: multiple_tabs
                .then(|| self.tab_state.tabs.previous())
                .flatten(),
            next_dojo_id: multiple_tabs.then(|| self.tab_state.tabs.next()).flatten(),
            tab_move: CommandTabMoveAvailability::from_edges(
                active_tab_index > 0,
                active_tab_index + 1 < self.tab_state.tabs.len(),
            ),
            focus_left: self.directional_splint(FocusDirection::Left),
            focus_right: self.directional_splint(FocusDirection::Right),
            focus_up: self.directional_splint(FocusDirection::Up),
            focus_down: self.directional_splint(FocusDirection::Down),
            clipboard_image_available: self.clipboard.palette_image_target.is_some()
                && self.clipboard.image_request.is_none()
                && self
                    .clipboard
                    .clipboard_offer
                    .as_ref()
                    .is_some_and(|offer| offer.with_mime_types(clipboard_image::png_offered)),
            viewport_detached: !self.panes.pane.scrollback_viewport.is_live(),
            controller_active: self.panes.pane.controller_active,
            forced_control_transfer: self.input.forced_control_transfer,
            revoke_all_available: self.access.revoke_all_available(),
            access_waiting: self.access.waiting(),
            pending_transfer_id: self.panes.pane.pending_control_transfer,
            agent_status_incarnation: self.tab_state.agent.clearable_incarnation(splint_id),
        }));
        let paused = self.handoff_paused();
        if let Some(palette) = self.modal.command_palette.as_mut() {
            palette.set_mutations_paused(paused);
        }
        self.modal.command_palette_layout = None;
        self.modal.command_palette_pressed = None;
        self.modal.command_palette_text_cache.clear();
        self.modal.command_palette_open_focus = Some(self.input.keyboard_focused);
        self.modal.command_palette_reconcile_pending = false;
        self.surface.window.set_title("Splinterm — Commands");
        self.presentation.full_redraw = true;
        Ok(())
    }

    fn show_binding_help(&mut self, queue_handle: &QueueHandle<Self>) -> Result<()> {
        self.show_command_palette()?;
        self.modal.binding_help = Some(BindingHelpUi::new(&self.input.keymap));
        if self.input.ime_modal_barrier {
            self.renew_text_input(queue_handle);
        }
        self.surface.window.set_title("Splinterm — Key bindings");
        Ok(())
    }

    fn reload_keymap_configuration(&mut self) {
        match crate::config::load_default() {
            Ok(loaded) => {
                self.cancel_clipboard_image();
                publish_topology_settings(
                    self.tab_state.topology_settings.as_ref(),
                    &loaded.config,
                );
                // The next update pass re-resolves the theme with them.
                self.presentation
                    .theme_colors
                    .reload(loaded.config.theme_colors());
                self.clipboard.image_directory = loaded.config.clipboard_image_directory;
                self.input.keymap = loaded.config.keymap;
                self.input.prefix_timeout =
                    std::time::Duration::from_millis(loaded.config.prefix_timeout_ms);
                self.input.prefix_state.clear();
                let presentation =
                    self.set_agent_status_presentation(loaded.config.explorer_agent_status);
                let style = self.set_agent_status_style(loaded.config.explorer_status_style);
                if presentation || style {
                    self.presentation.full_redraw = true;
                }
                if self.presentation.tab_strip_border != loaded.config.tab_strip_border {
                    self.presentation.tab_strip_border = loaded.config.tab_strip_border;
                    self.presentation.full_redraw = true;
                }
                if self.presentation.explorer_full_height != loaded.config.explorer_full_height {
                    // The terminal area moves when a docked Explorer is shown.
                    self.presentation.explorer_full_height = loaded.config.explorer_full_height;
                    self.relayout_tab_strip();
                    self.relayout_panes();
                }
                if self.presentation.explorer_collapsed != loaded.config.explorer_collapsed {
                    self.presentation.explorer_collapsed = loaded.config.explorer_collapsed;
                    if self
                        .explorer
                        .set_collapsed(loaded.config.explorer_collapsed)
                    {
                        // The terminal area changes width with the Explorer.
                        self.presentation.explorer_layout = None;
                        self.relayout_tab_strip();
                        self.relayout_panes();
                    }
                }
                self.presentation.ui_font_size = loaded.config.ui_font_size;
                self.reconcile_chrome_font_size();
                self.apply_pane_chrome(
                    loaded.config.pane_divider_style,
                    loaded.config.frame_title_mode,
                );
                self.apply_pane_gap(loaded.config.pane_gap);
                for _ in loaded.diagnostics {
                    eprintln!("splinterm configuration warning");
                }
            }
            Err(_) => eprintln!("splinterm config reload rejected"),
        }
    }

    /// Applies a reloaded `[multiplexer] pane-gap`: the panes are laid out
    /// again and every pane's PTY is resized on the next update pass. A gap
    /// the panes cannot fit shrinks for that layout in
    /// [`Self::computed_pane_layout`], so the new value is always accepted.
    fn apply_pane_gap(&mut self, gap: u32) {
        if gap == self.presentation.pane_gap {
            return;
        }
        self.presentation.pane_gap = gap;
        self.relayout_panes();
    }

    /// Applies a reloaded `[multiplexer] divider-style` and `frame-title`.
    /// The style changes how much room the chrome takes, so the panes are laid
    /// out again and every pane's PTY is resized; cached frame titles of every
    /// tab are dropped so the next draw shapes them for the new chrome.
    fn apply_pane_chrome(&mut self, style: PaneDividerStyle, title_mode: FrameTitleMode) {
        if style == self.presentation.pane_divider_style
            && title_mode == self.presentation.frame_title_mode
        {
            return;
        }
        self.presentation.pane_divider_style = style;
        self.presentation.frame_title_mode = title_mode;
        self.presentation.frame_titles.clear();
        for tab in self.tab_state.tabs.iter_mut() {
            if let Some(view) = tab.value.as_mut() {
                view.frame_titles.clear();
            }
        }
        self.relayout_panes();
    }

    /// Lays the panes out again after a chrome setting changed: every pane's
    /// PTY is resized on the next update pass and the Window is drawn again.
    fn relayout_panes(&mut self) {
        self.panes.restored_frontend_needs_resize = true;
        self.presentation.full_redraw = true;
        if self.surface.configured {
            self.scheduling.redraw_pending = true;
        }
        self.update_ime_cursor_rectangle();
    }

    fn close_command_palette(&mut self) -> bool {
        if self.modal.command_palette.take().is_none() {
            return false;
        }
        self.modal.binding_help = None;
        self.modal.agent_access = None;
        self.modal.command_palette_layout = None;
        self.modal.command_palette_pressed = None;
        self.modal.command_palette_text_cache.clear();
        self.clear_ime_preedit();
        self.modal.command_palette_reconcile_pending = true;
        self.presentation.full_redraw = true;
        true
    }

    fn reconcile_command_palette_close(&mut self, queue_handle: &QueueHandle<Self>) {
        if !std::mem::take(&mut self.modal.command_palette_reconcile_pending) {
            return;
        }
        self.update_window_title();
        match picker_ime_reconcile(
            self.input.ime_modal_barrier,
            self.input.keyboard_focused,
            self.input.ime.entered,
        ) {
            PickerImeReconcile::Renew => self.renew_text_input(queue_handle),
            PickerImeReconcile::Enable => self.enable_text_input_if_allowed(),
            PickerImeReconcile::None => {}
        }
        let modal_focus_changed = self
            .modal
            .command_palette_open_focus
            .take()
            .is_some_and(|focused| focused != self.input.keyboard_focused);
        self.reconcile_terminal_focus_report(modal_focus_changed);
    }

    fn set_explorer_focus(&mut self, focused: bool, queue_handle: &QueueHandle<Self>) {
        if focused {
            if !self.explorer.focused() {
                self.input.input_generation = self.input.input_generation.saturating_add(1);
                self.input.ime_modal_barrier =
                    self.input.ime.entered && self.input.text_input.is_some();
                if self.input.ime_modal_barrier {
                    if let Some(text_input) = &self.input.text_input {
                        text_input.disable();
                    }
                    self.commit_text_input();
                }
                self.clear_ime_preedit();
            }
            self.explorer.focus();
            if self.input.terminal_focus_reported {
                self.request_active_pane_focus_report(false);
                self.input.terminal_focus_reported = false;
            }
        } else {
            self.explorer.return_to_terminal();
            if !self.modal.input_modal_open() {
                match picker_ime_reconcile(
                    self.input.ime_modal_barrier,
                    self.input.keyboard_focused,
                    self.input.ime.entered,
                ) {
                    PickerImeReconcile::Renew => self.renew_text_input(queue_handle),
                    PickerImeReconcile::Enable => self.enable_text_input_if_allowed(),
                    PickerImeReconcile::None => {}
                }
            }
            if self.input.keyboard_focused
                && self.panes.input_modes().focus_reporting
                && !self.input.terminal_focus_reported
            {
                self.request_active_pane_focus_report(true);
                self.input.terminal_focus_reported = true;
            }
        }
        self.presentation.full_redraw = true;
    }

    /// Collapses the Explorer to its rail or expands it; whether that
    /// changed. Keyboard focus stays where it is.
    fn set_explorer_collapsed(&mut self, collapsed: bool) -> Result<bool> {
        if !self.explorer.set_collapsed(collapsed) {
            return Ok(false);
        }
        self.apply_explorer_width_change()?;
        Ok(true)
    }

    /// The Explorer was shown, hidden, collapsed, or expanded, so the
    /// terminal beside it changes width: the layout is taken again, the tab
    /// strip beside a full-height Explorer moves with it, and every pane is
    /// resized.
    fn apply_explorer_width_change(&mut self) -> Result<()> {
        // Taken now, not at the next draw, so a quick second click hit-tests
        // the new layout.
        self.presentation.explorer_layout = self.take_explorer_layout();
        self.presentation.full_redraw = true;
        self.relayout_tab_strip();
        if self.surface.configured {
            self.emit_resize()?;
        }
        Ok(())
    }

    fn refresh_lair_explorer(&mut self) -> Result<()> {
        self.explorer.begin_refresh();
        if let Err(error) = self.send_topology_command(WindowTopologyCommand::RequestLairExplorer {
            focused_splint: self.panes.focused_splint(),
        }) {
            self.explorer.mark_disconnected();
            self.explorer
                .set_notice(&format!("Navigation unavailable · R retry: {error:#}"));
            return Err(error);
        }
        self.presentation.full_redraw = true;
        Ok(())
    }

    fn begin_lair_explorer_action(&mut self, decision: LairExplorerDecision) -> bool {
        if self.tab_state.session_switch_pending || !self.explorer.set_pending(decision) {
            return false;
        }
        self.tab_state.session_switch_pending = true;
        true
    }

    fn execute_lair_explorer_decision(&mut self, decision: LairExplorerDecision) -> bool {
        let returns_focus = decision.returns_focus_to_terminal();
        let command = match decision {
            LairExplorerDecision::Toggle(_) => return true,
            LairExplorerDecision::ExpandRail(_) => {
                // The rail expanded to the tree, so the terminal narrows.
                if let Err(error) = self.apply_explorer_width_change() {
                    eprintln!("splinterm Explorer resize failed: {error:#}");
                }
                return true;
            }
            LairExplorerDecision::OpenDojo(target) => {
                let target = SessionPickerTarget {
                    topology_revision: target.topology_revision,
                    lair_id: target.lair_id,
                    dojo_id: target.dojo_id,
                    action: target.capability.action,
                };
                WindowTopologyCommand::OpenDojo {
                    target,
                    explorer_target: Some(crate::frontend::LairExplorerActivationTarget::Dojo(
                        target,
                    )),
                }
            }
            LairExplorerDecision::FocusSplint(target) => {
                WindowTopologyCommand::FocusSplint { target }
            }
        };
        if self.handoff_paused() && command.paused_during_handoff() {
            self.explorer
                .set_notice("Paused while splinterd upgrades; try again after it finishes");
            self.presentation.full_redraw = true;
            return true;
        }
        if !returns_focus || !self.begin_lair_explorer_action(decision) {
            return false;
        }
        if self.send_topology_command(command).is_err() {
            self.tab_state.session_switch_pending = false;
            self.explorer.mark_disconnected();
        }
        true
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the closed palette catalog routes each typed application action at one modal boundary"
    )]
    fn execute_command_palette(
        &mut self,
        command: BuiltInCommandId,
        queue_handle: &QueueHandle<Self>,
    ) {
        let Some(dispatch) = self
            .modal
            .command_palette
            .as_ref()
            .and_then(|palette| palette.dispatch(command))
        else {
            return;
        };
        let transitions_to_prompt = matches!(
            &dispatch,
            BuiltInCommandDispatch::Rename(_) | BuiltInCommandDispatch::ConfirmTermination(_)
        );
        self.close_command_palette();
        if !transitions_to_prompt {
            self.reconcile_command_palette_close(queue_handle);
        }
        let result = match dispatch {
            BuiltInCommandDispatch::ShowKeybindings => self.show_binding_help(queue_handle),
            BuiltInCommandDispatch::ReloadConfiguration => {
                self.reload_keymap_configuration();
                Ok(())
            }
            BuiltInCommandDispatch::SaveClipboardImage => {
                let captured = self.clipboard.palette_image_target.take();
                if captured.is_some() && self.clipboard_image_target().ok() == captured {
                    self.begin_clipboard_image();
                } else {
                    self.clipboard_image_notice(
                        "Clipboard image save cancelled: palette target changed",
                    );
                }
                Ok(())
            }
            BuiltInCommandDispatch::EnterCopyMode => {
                self.enter_copy_mode();
                Ok(())
            }
            BuiltInCommandDispatch::ToggleFocusedPaneZoom => {
                self.toggle_pane_zoom();
                Ok(())
            }
            BuiltInCommandDispatch::MoveDojo { dojo_id, delta } => (|| -> Result<()> {
                let source = self
                    .tab_state
                    .tabs
                    .iter()
                    .position(|tab| tab.dojo_id == dojo_id)
                    .context("captured Dojo tab is no longer available")?;
                let target = if delta.is_negative() {
                    source.saturating_sub(delta.unsigned_abs())
                } else {
                    source
                        .saturating_add(delta.unsigned_abs())
                        .min(self.tab_state.tabs.len().saturating_sub(1))
                };
                anyhow::ensure!(target != source, "captured Dojo tab cannot move further");
                anyhow::ensure!(
                    self.tab_state.tabs.move_tab(dojo_id, target),
                    "captured Dojo tab move was rejected"
                );
                self.presentation.full_redraw = true;
                Ok(())
            })(),
            BuiltInCommandDispatch::DetachWindow => {
                self.scheduling.request_exit(ExitClass::CleanUserClose);
                Ok(())
            }
            BuiltInCommandDispatch::Topology(mut command) => (|| -> Result<()> {
                if let Some((target, axis)) = split_request(&command)
                    && !self.admit_split(target, axis)
                {
                    return Ok(());
                }
                let picker_request =
                    matches!(&command, WindowTopologyCommand::RequestSelector { .. })
                        .then(|| command.clone());
                let requests_picker = matches!(
                    &command,
                    WindowTopologyCommand::RequestSelector { .. }
                        | WindowTopologyCommand::RequestLairPrompt { .. }
                );
                if let Some(request) = picker_request.as_ref() {
                    let WindowTopologyCommand::RequestSelector { kind, .. } = request else {
                        unreachable!("picker request classification changed");
                    };
                    self.show_embedded_session_picker(
                        session_picker_status_catalog("Loading navigation…"),
                        Some(*kind),
                    )?;
                    self.modal.session_picker_requested = true;
                    self.modal.session_picker_retry_command = picker_request;
                } else if requests_picker {
                    self.modal.session_picker_requested = true;
                    self.modal.session_picker_retry_command = None;
                }
                let pending_started = if self.input.optimistic_remote_splits
                    && let WindowTopologyCommand::Split {
                        target,
                        axis,
                        pending,
                        ..
                    } = &mut command
                    && pending.is_none()
                {
                    match self.begin_pending_remote_split(*target, *axis)? {
                        Some(pending_id) => {
                            *pending = Some(pending_id);
                            true
                        }
                        None => return Ok(()),
                    }
                } else {
                    false
                };
                if let Err(error) = self.send_topology_command(command) {
                    if requests_picker {
                        self.modal.session_picker_requested = false;
                    }
                    return Err(error);
                }
                if pending_started && self.surface.configured {
                    self.schedule_draw(queue_handle)?;
                }
                Ok(())
            })(),
            BuiltInCommandDispatch::Focus(splint_id) => {
                if self.focus_splint(splint_id) {
                    self.update_ime_cursor_rectangle();
                }
                Ok(())
            }
            BuiltInCommandDispatch::Zoom(action) => self
                .apply_font_zoom(
                    match action {
                        CommandZoomAction::Increase => FontZoomAction::Increase,
                        CommandZoomAction::Decrease => FontZoomAction::Decrease,
                        CommandZoomAction::Reset => FontZoomAction::Reset,
                    },
                    queue_handle,
                )
                .map(|_| ()),
            BuiltInCommandDispatch::ToggleTabStrip => (|| -> Result<()> {
                if self.toggle_tab_strip()? && self.surface.configured {
                    self.schedule_draw(queue_handle)?;
                }
                Ok(())
            })(),
            BuiltInCommandDispatch::ToggleLairExplorer => (|| -> Result<()> {
                let was_focused = self.explorer.focused();
                let visible = self.explorer.toggle_visibility();
                if was_focused && !visible {
                    self.set_explorer_focus(false, queue_handle);
                }
                if visible {
                    self.refresh_lair_explorer()?;
                }
                self.apply_explorer_width_change()
            })(),
            BuiltInCommandDispatch::ToggleLairExplorerRail => (|| -> Result<()> {
                let was_hidden = self.explorer.toggle_rail();
                self.apply_explorer_width_change()?;
                if was_hidden {
                    self.refresh_lair_explorer()?;
                }
                Ok(())
            })(),
            BuiltInCommandDispatch::FocusLairExplorer => (|| -> Result<()> {
                let was_visible = self.explorer.visible();
                self.set_explorer_focus(true, queue_handle);
                if !was_visible {
                    self.relayout_tab_strip();
                }
                if !was_visible && self.surface.configured {
                    self.emit_resize()?;
                }
                self.refresh_lair_explorer()
            })(),
            BuiltInCommandDispatch::ReturnFocusToTerminal => {
                self.set_explorer_focus(false, queue_handle);
                Ok(())
            }
            BuiltInCommandDispatch::History { target, action } => (|| -> Result<()> {
                anyhow::ensure!(
                    self.panes.focused_splint() == Some(target),
                    "captured history target is no longer focused"
                );
                match action {
                    CommandHistoryAction::Search => {
                        self.input.input_generation = self.input.input_generation.saturating_add(1);
                        self.panes.pane.search.input = Some(new_search_editor());
                        self.panes.pane.search.matches.clear();
                        self.panes.pane.search.next_cursor = None;
                        self.refresh_search_overlay(queue_handle);
                    }
                    CommandHistoryAction::PageUp | CommandHistoryAction::PageDown => {
                        let page = self
                            .panes
                            .pane
                            .snapshot
                            .as_ref()
                            .map_or(1, |snapshot| snapshot.rows.saturating_sub(1).max(1));
                        let direction = if action == CommandHistoryAction::PageUp {
                            MouseAction::WheelUp
                        } else {
                            MouseAction::WheelDown
                        };
                        if self.scroll_history(direction, page)? && self.surface.configured {
                            self.schedule_draw(queue_handle)?;
                        }
                    }
                    CommandHistoryAction::ReturnToLive => {
                        if self.scroll_history(MouseAction::WheelDown, usize::MAX)?
                            && self.surface.configured
                        {
                            self.schedule_draw(queue_handle)?;
                        }
                    }
                }
                Ok(())
            })(),
            BuiltInCommandDispatch::Control { target, action } => (|| -> Result<()> {
                anyhow::ensure!(
                    self.panes.focused_splint() == Some(target),
                    "captured control target is no longer focused"
                );
                match action {
                    CommandControlAction::Request => {
                        self.send_command(WindowCommand::RequestControlTransfer);
                    }
                    CommandControlAction::Release => {
                        self.request_active_pane_control_release();
                    }
                    CommandControlAction::Force if self.input.forced_control_transfer => {
                        self.send_command(WindowCommand::ForceControlTransfer);
                    }
                    CommandControlAction::Force => {}
                    CommandControlAction::Accept(transfer_id)
                    | CommandControlAction::Deny(transfer_id) => {
                        anyhow::ensure!(
                            self.panes.pane.pending_control_transfer == Some(transfer_id),
                            "captured control transfer is no longer pending"
                        );
                        self.panes.pane.pending_control_transfer = None;
                        self.send_command(WindowCommand::DecideControlTransfer {
                            transfer_id,
                            decision: if matches!(action, CommandControlAction::Accept(_)) {
                                ControlTransferDecision::Accept
                            } else {
                                ControlTransferDecision::Deny
                            },
                        });
                    }
                }
                self.update_window_title();
                Ok(())
            })(),
            BuiltInCommandDispatch::AgentAccess => self.show_agent_access(queue_handle),
            BuiltInCommandDispatch::Rename(target) => {
                self.show_dojo_prompt(DojoPromptUi::rename(
                    target.dojo_id,
                    target.name,
                    target.pane_count,
                    target.splints,
                ));
                Ok(())
            }
            BuiltInCommandDispatch::ConfirmTermination(target) => {
                self.show_dojo_prompt(DojoPromptUi::terminate(
                    target.dojo_id,
                    target.name,
                    target.pane_count,
                    target.splints,
                ));
                Ok(())
            }
            BuiltInCommandDispatch::OpenRemoteChooser => self.show_remote_profiles(),
            BuiltInCommandDispatch::RecentSessions => (|| -> Result<()> {
                self.show_embedded_session_picker(
                    session_picker_status_catalog("Loading navigation…"),
                    None,
                )?;
                self.modal.session_picker_requested = true;
                self.modal.session_picker_retry_command =
                    Some(WindowTopologyCommand::RequestSessionPicker);
                self.send_topology_command(WindowTopologyCommand::RequestSessionPicker)
                    .inspect_err(|_| self.modal.session_picker_requested = false)
            })(),
        };
        if result.is_err() {
            eprintln!("splinterm command palette action failed");
        }
    }

    fn refresh_command_palette(&mut self) {
        self.modal.command_palette_layout = None;
        self.presentation.full_redraw = true;
    }

    fn explorer_context_menu(&self, node: NavigationNodeId) -> Option<TabContextMenuUi> {
        if !self.explorer.can_context(node) {
            return None;
        }
        let view = self.explorer.view()?;
        let pane_controlled = match node {
            NavigationNodeId::Splint {
                dojo_id, splint_id, ..
            } => {
                let incarnation = view
                    .lairs
                    .iter()
                    .flat_map(|lair| &lair.dojos)
                    .flat_map(|dojo| &dojo.splints)
                    .find(|splint| splint.id == node)
                    .and_then(|splint| splint.target.live_incarnation);
                let controlled = |pane: &PaneView| {
                    pane.controller_active
                        && !pane.control_release_pending
                        && pane.snapshot.as_ref().is_some_and(|snapshot| {
                            snapshot.splint_id == splint_id
                                && Some(snapshot.incarnation) == incarnation
                        })
                };
                if self.tab_state.active_dojo_id() == dojo_id {
                    std::iter::once(&self.panes.pane)
                        .chain(self.panes.inactive_panes.iter())
                        .any(controlled)
                } else {
                    self.tab_state
                        .tabs
                        .get(dojo_id)
                        .and_then(|tab| tab.value.as_ref())
                        .is_some_and(|view| {
                            std::iter::once(&view.pane)
                                .chain(view.inactive_panes.iter())
                                .any(controlled)
                        })
                }
            }
            _ => false,
        };
        TabContextMenuUi::for_explorer(
            view,
            node,
            self.focused_cwd_source().ok(),
            pane_controlled,
            self.tab_state.tabs.len() < crate::tab::MAX_WINDOW_TABS,
        )
    }

    fn show_explorer_context_menu(
        &mut self,
        node: NavigationNodeId,
        anchor: (f64, f64),
    ) -> Result<()> {
        let menu = self
            .explorer_context_menu(node)
            .context("Explorer menu target is unavailable")?;
        self.show_context_menu(menu, anchor)
    }

    fn show_tab_context_menu(&mut self, dojo_id: DojoId, anchor: (f64, f64)) -> Result<()> {
        let identity = self
            .tab_state
            .tab_identity(dojo_id)
            .cloned()
            .context("tab menu target is unavailable")?;
        let active = self.tab_state.active_dojo_id() == dojo_id;
        let splints = if active {
            std::iter::once(&self.panes.pane)
                .chain(self.panes.inactive_panes.iter())
                .filter_map(|pane| {
                    pane.snapshot
                        .as_ref()
                        .map(|snapshot| (snapshot.splint_id, snapshot.incarnation))
                })
                .collect::<Vec<_>>()
        } else {
            self.tab_state
                .tabs
                .get(dojo_id)
                .and_then(|tab| tab.value.as_ref())
                .map_or_else(Vec::new, |view| {
                    std::iter::once(&view.pane)
                        .chain(view.inactive_panes.iter())
                        .filter_map(|pane| {
                            pane.snapshot
                                .as_ref()
                                .map(|snapshot| (snapshot.splint_id, snapshot.incarnation))
                        })
                        .collect()
                })
        };
        let pane_count = splints.len();
        let other_dojo_ids = self
            .tab_state
            .tabs
            .iter()
            .filter_map(|tab| (tab.dojo_id != dojo_id).then_some(tab.dojo_id))
            .collect();
        self.show_context_menu(
            TabContextMenuUi::new(TabMenuContext {
                lair_id: identity.lair_id,
                source_splint_id: self.focused_cwd_source()?,
                dojo_id,
                dojo_name: identity.dojo_name,
                pane_count,
                splints,
                active,
                other_dojo_ids,
            }),
            anchor,
        )
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "finite pointer coordinates are clamped to the logical surface"
    )]
    fn show_context_menu(&mut self, mut menu: TabContextMenuUi, anchor: (f64, f64)) -> Result<()> {
        self.input.prefix_state.clear();
        anyhow::ensure!(
            self.tab_state.managed_tabs,
            "tab menu requires managed tabs"
        );
        anyhow::ensure!(
            self.tab_state.topology_commands.is_some(),
            "tab menu topology commands are unavailable"
        );
        anyhow::ensure!(
            self.modal.command_palette.is_none()
                && self.modal.dojo_prompt.is_none()
                && self.modal.session_picker.is_none()
                && self.modal.trusted_consent.is_none()
                && !self.modal.session_picker_requested
                && !self.modal.session_picker_reconcile_pending
                && !self.modal.command_palette_reconcile_pending
                && !self.tab_state.session_switch_pending
                && self.panes.pane.search.input.is_none()
                && self.panes.pane.pending_control_transfer.is_none()
                && self.input.divider_drag.is_none(),
            "tab menu is unavailable"
        );
        self.settle_terminal_presses_for_picker();
        self.input.input_generation = self.input.input_generation.saturating_add(1);
        self.input.ime_modal_barrier = self.input.ime.entered && self.input.text_input.is_some();
        if self.input.ime_modal_barrier {
            if let Some(text_input) = &self.input.text_input {
                text_input.disable();
            }
            self.commit_text_input();
        }
        self.clear_ime_preedit();
        let logical = |value: f64, maximum: u32| {
            if value.is_finite() && value > 0.0 {
                value.floor().min(f64::from(maximum)) as u32
            } else {
                0
            }
        };
        self.presentation.explorer_pressed = None;
        self.explorer.cancel_context_press();
        menu.set_mutations_paused(self.handoff_paused());
        self.modal.tab_context_menu = Some(menu);
        self.modal.tab_context_menu_anchor = (
            logical(anchor.0, self.surface.logical_width),
            logical(anchor.1, self.surface.logical_height),
        );
        self.modal.tab_context_menu_layout = None;
        self.modal.tab_context_menu_pressed = None;
        self.modal.tab_context_menu_retarget = None;
        self.modal.tab_context_menu_text_cache.clear();
        self.modal.command_palette_open_focus = Some(self.input.keyboard_focused);
        self.modal.command_palette_reconcile_pending = false;
        self.presentation.full_redraw = true;
        Ok(())
    }

    fn close_tab_context_menu(&mut self) -> bool {
        if self.modal.tab_context_menu.take().is_none() {
            return false;
        }
        self.modal.tab_context_menu_layout = None;
        self.modal.tab_context_menu_pressed = None;
        self.modal.tab_context_menu_retarget = None;
        self.modal.tab_context_menu_text_cache.clear();
        self.modal.command_palette_reconcile_pending = true;
        self.presentation.full_redraw = true;
        true
    }

    fn execute_tab_context_menu(&mut self, action: TabMenuActionId) {
        if let Some(target) = self
            .modal
            .tab_context_menu
            .as_ref()
            .and_then(TabContextMenuUi::explorer_target)
            && !self.explorer_context_menu(target.node).is_some_and(|menu| {
                menu.explorer_target() == Some(target) && menu.action_enabled(action)
            })
        {
            self.close_tab_context_menu();
            self.explorer
                .set_notice("Menu target changed; reopen its menu and try again");
            return;
        }
        let Some(dispatch) = self
            .modal
            .tab_context_menu
            .as_ref()
            .and_then(|menu| menu.dispatch(action))
        else {
            return;
        };
        self.close_tab_context_menu();
        let result = match dispatch {
            TabMenuDispatch::Topology(mut command) => {
                if let Some((target, axis)) = split_request(&command)
                    && !self.admit_split(target, axis)
                {
                    self.explorer
                        .set_notice("Split refused: the new pane would not fit in this Window");
                    return;
                }
                let mut navigates = false;
                if let WindowTopologyCommand::ExplorerContext { target, command } = &mut command
                    && matches!(
                        command.as_ref(),
                        WindowTopologyCommand::OpenDojo { .. }
                            | WindowTopologyCommand::FocusSplint { .. }
                    )
                {
                    let Some(decision) = self.explorer.activation_decision(target.node) else {
                        return;
                    };
                    if !self.begin_lair_explorer_action(decision) {
                        return;
                    }
                    navigates = true;
                    if let WindowTopologyCommand::OpenDojo {
                        target,
                        explorer_target,
                    } = command.as_mut()
                    {
                        *explorer_target = Some(crate::LairExplorerActivationTarget::Dojo(*target));
                    }
                }
                let requests_prompt = matches!(
                    &command,
                    WindowTopologyCommand::RequestLairPrompt { .. }
                ) || matches!(&command, WindowTopologyCommand::ExplorerContext { command, .. } if matches!(command.as_ref(), WindowTopologyCommand::RequestLairPrompt { .. } | WindowTopologyCommand::RequestDojoPrompt { .. }));
                if requests_prompt {
                    self.modal.session_picker_requested = true;
                    self.modal.session_picker_retry_command = None;
                }
                self.send_topology_command(command).inspect_err(|_| {
                    if requests_prompt {
                        self.modal.session_picker_requested = false;
                    }
                    if navigates {
                        self.tab_state.session_switch_pending = false;
                        self.explorer.mark_disconnected();
                    }
                })
            }
            TabMenuDispatch::Rename(target) => {
                self.show_dojo_prompt(DojoPromptUi::rename(
                    target.dojo_id,
                    target.name,
                    target.pane_count,
                    target.splints,
                ));
                Ok(())
            }
            TabMenuDispatch::ConfirmTermination(target) => {
                self.show_dojo_prompt(DojoPromptUi::terminate(
                    target.dojo_id,
                    target.name,
                    target.pane_count,
                    target.splints,
                ));
                Ok(())
            }
        };
        if let Err(error) = result {
            self.explorer
                .set_notice(&format!("Menu action failed: {error:#}"));
            eprintln!("splinterm tab menu action failed: {error:#}");
        }
    }

    fn show_current_dojo_prompt(&mut self, terminate: bool) {
        let dojo_id = self.tab_state.active_dojo_id();
        let splints = std::iter::once(&self.panes.pane)
            .chain(self.panes.inactive_panes.iter())
            .filter_map(|pane| {
                pane.snapshot
                    .as_ref()
                    .map(|snapshot| (snapshot.splint_id, snapshot.incarnation))
            })
            .collect::<Vec<_>>();
        let name = self.tab_state.active_identity.dojo_name.clone();
        let pane_count = splints.len();
        let prompt = if terminate {
            DojoPromptUi::terminate(dojo_id, name, pane_count, splints)
        } else {
            DojoPromptUi::rename(dojo_id, name, pane_count, splints)
        };
        self.show_dojo_prompt(prompt);
    }

    fn show_dojo_prompt(&mut self, prompt: DojoPromptUi) {
        self.modal.dojo_prompt_explorer_target = None;
        self.input.prefix_state.clear();
        self.input.input_generation = self.input.input_generation.saturating_add(1);
        let title = match &prompt {
            DojoPromptUi::Rename(_) => "Splinterm — Rename Tab",
            DojoPromptUi::Terminate(_) => "Splinterm — Confirm Dojo Termination",
            DojoPromptUi::RenameLair(_) => "Splinterm — Rename Lair",
            DojoPromptUi::PreviewLair(_) => "Splinterm — Saved Lair Preview",
            DojoPromptUi::RestoreLair(_) => "Splinterm — Restore Saved Lair",
            DojoPromptUi::TerminateLair(_) => "Splinterm — Confirm Lair Termination",
            DojoPromptUi::RevokeAccess(prompt) => prompt.window_title(),
        };
        self.modal.dojo_prompt = Some(prompt);
        self.modal.dojo_prompt_layout = None;
        self.modal.dojo_prompt_pressed = None;
        self.modal.dojo_prompt_text_cache.clear();
        self.modal.command_palette_reconcile_pending = false;
        self.surface.window.set_title(title);
        self.presentation.full_redraw = true;
    }

    fn close_dojo_prompt(&mut self) -> bool {
        self.modal.dojo_prompt_explorer_target = None;
        if self.modal.dojo_prompt.take().is_none() {
            return false;
        }
        self.input.input_generation = self.input.input_generation.saturating_add(1);
        self.modal.dojo_prompt_layout = None;
        self.modal.dojo_prompt_pressed = None;
        self.modal.dojo_prompt_text_cache.clear();
        if self.modal.command_palette.is_some() {
            // A revoke prompt opened above the palette returns to it.
            if self.modal.agent_access.is_some() || self.modal.binding_help.is_some() {
                self.update_window_title();
            } else {
                self.surface.window.set_title("Splinterm — Commands");
            }
        } else {
            self.modal.command_palette_reconcile_pending = true;
        }
        self.presentation.full_redraw = true;
        true
    }

    fn execute_dojo_prompt(&mut self) {
        if let Some(DojoPromptUi::RevokeAccess(prompt)) = self.modal.dojo_prompt.as_ref() {
            let command = prompt.command();
            self.close_dojo_prompt();
            if let Some(command) = command {
                self.execute_access_prompt(command);
            }
            return;
        }
        let command = match self.modal.dojo_prompt.as_ref() {
            Some(DojoPromptUi::Rename(prompt)) => prompt.command(),
            Some(DojoPromptUi::Terminate(prompt)) => prompt.command(),
            Some(DojoPromptUi::RenameLair(prompt)) => prompt.command(),
            Some(DojoPromptUi::PreviewLair(_)) | None => None,
            Some(DojoPromptUi::RestoreLair(prompt)) => prompt.command(),
            Some(DojoPromptUi::TerminateLair(prompt)) => prompt.command(),
            Some(DojoPromptUi::RevokeAccess(_)) => unreachable!("access prompts return above"),
        };
        if command.is_none()
            && self
                .modal
                .dojo_prompt
                .as_ref()
                .is_some_and(DojoPromptUi::is_rename)
        {
            return;
        }
        let command = command.map(|command| match self.modal.dojo_prompt_explorer_target {
            Some(target) => target.command(command),
            None => command,
        });
        // Submitted during a daemon handoff: the prompt stays open with its
        // text, to submit once the pause ends, and its title says why.
        if self.handoff_paused()
            && command
                .as_ref()
                .is_some_and(WindowTopologyCommand::paused_during_handoff)
        {
            self.surface
                .window
                .set_title("Splinterm — Paused while splinterd upgrades; submit after it finishes");
            return;
        }
        self.modal.dojo_prompt_explorer_target = None;
        self.close_dojo_prompt();
        if let Some(command) = command
            && self.send_topology_command(command).is_err()
        {
            eprintln!("splinterm Dojo prompt action failed");
        }
    }

    fn refresh_dojo_prompt(&mut self) {
        self.modal.dojo_prompt_layout = None;
        self.presentation.full_redraw = true;
    }

    fn refresh_tab_context_menu(&mut self) {
        // Hover, press, and keyboard selection do not change menu geometry.
        // Retain the committed hit map until redraw: Wayland can batch motion,
        // press, and release before another frame is painted. Opening, closing,
        // retargeting, and resizing invalidate the layout separately.
        self.presentation.full_redraw = true;
    }

    fn show_embedded_session_picker(
        &mut self,
        catalog: SessionPickerCatalog,
        selector_kind: Option<SelectorKind>,
    ) -> Result<()> {
        let SessionPickerCatalog {
            items,
            targets,
            creation_enabled,
            creation_blocker,
            creation_target,
            initial_row,
        } = catalog;
        anyhow::ensure!(items.len() == targets.len(), "Dojo picker targets differ");
        self.install_inline_picker(
            SessionPickerUi::inline(items, creation_enabled, creation_blocker, initial_row),
            selector_kind,
            targets,
            creation_target,
        )?;
        self.surface
            .window
            .set_title(session_picker_title(selector_kind));
        Ok(())
    }

    /// Shows the remote profiles as a pop-up over this Window. Choosing one
    /// opens it in exactly one new Window, which connects and shows that
    /// host's Dojos; this Window only closes the pop-up. Its status and Dojo
    /// list are never shown here: they could need a second SSH
    /// authentication and a masked field inside a live terminal.
    fn show_remote_profiles(&mut self) -> Result<()> {
        let catalog = crate::remote::RemoteCatalog::load_default();
        let (names, items, blocker) = crate::remote_launch::chooser_contents(&catalog);
        self.install_inline_picker(
            SessionPickerUi::inline_remote_profiles(items, blocker),
            None,
            Vec::new(),
            None,
        )?;
        self.modal.remote_profiles = Some(names);
        // R retries a navigation picker; it must never turn this pop-up into one.
        self.modal.session_picker_retry_command = None;
        self.surface.window.set_title("Splinterm — Remotes");
        Ok(())
    }

    fn install_inline_picker(
        &mut self,
        picker: SessionPickerUi,
        selector_kind: Option<SelectorKind>,
        targets: Vec<SessionPickerTarget>,
        creation_target: Option<SessionPickerCreationTarget>,
    ) -> Result<()> {
        self.input.prefix_state.clear();
        anyhow::ensure!(
            self.modal.session_picker.is_none(),
            "Dojo picker is already open"
        );
        self.settle_terminal_presses_for_picker();
        self.input.input_generation = self.input.input_generation.saturating_add(1);
        self.modal.session_picker_wheel = WheelAccumulator::default();
        self.input.ime_modal_barrier = self.input.ime.entered && self.input.text_input.is_some();
        if self.input.ime_modal_barrier {
            if let Some(text_input) = &self.input.text_input {
                text_input.disable();
            }
            self.commit_text_input();
        }
        self.clear_ime_preedit();
        self.modal.session_picker = Some(picker);
        self.modal.remote_profiles = None;
        self.modal.selector_kind = selector_kind;
        self.modal.session_picker_targets = targets;
        self.modal.session_picker_creation_target = creation_target;
        self.modal.session_picker_layout = None;
        self.modal.session_picker_pressed = None;
        self.modal.session_picker_text_cache.clear();
        self.modal.session_picker_redraw = false;
        self.modal.session_picker_reconcile_pending = false;
        self.modal.session_picker_open_focus = Some(self.input.keyboard_focused);
        self.modal.session_picker_requested = false;
        self.presentation.full_redraw = true;
        Ok(())
    }

    fn close_inline_session_picker(&mut self) -> bool {
        if !self.modal.inline_picker_open() {
            return false;
        }
        self.modal.session_picker = None;
        self.modal.remote_profiles = None;
        self.modal.selector_kind = None;
        self.modal.session_picker_targets.clear();
        self.modal.session_picker_creation_target = None;
        self.modal.session_picker_layout = None;
        self.modal.session_picker_pressed = None;
        self.modal.session_picker_text_cache.clear();
        self.modal.session_picker_wheel = WheelAccumulator::default();
        self.modal.session_picker_redraw = false;
        self.modal.session_picker_reconcile_pending = true;
        self.modal.session_picker_requested = false;
        true
    }

    fn send_topology_command(&mut self, command: WindowTopologyCommand) -> Result<()> {
        // The final gate for every entry point, since a menu, picker, or
        // prompt opened before the pause can still dispatch.
        if self.handoff_paused() && command.paused_during_handoff() {
            self.drop_topology_command_for_handoff(&command);
            return Ok(());
        }
        // While the Window reconnects after a lost daemon connection, or once
        // it gave up, no daemon request runs: reads included, they would
        // only queue behind the resnapshot or never be answered.
        if self.handoff.disconnected()
            || (self.tab_state.topology_commands.is_none()
                && self.handoff.holds_window_on_disconnect())
        {
            // Visual tab switching needs no daemon and keeps working
            // (operator decision 1): it happens here at once, and the
            // topology manager's tab order follows once it listens again.
            if let Some(dojo_id) = handoff::tab_switch_while_disconnected(&command) {
                if self.activate_tab(dojo_id)? && self.surface.configured {
                    self.scheduling.redraw_pending = true;
                }
                if let Some(commands) = &self.tab_state.topology_commands {
                    let _ = commands.try_send(command);
                }
                return Ok(());
            }
            self.drop_topology_command_for_handoff(&command);
            return Ok(());
        }
        self.cancel_clipboard_image();
        // Capture all entry points, including keyboard shortcuts, before dispatch so
        // a failed load can retry the exact picker kind and original Lair scope.
        if let Some(request) = session_picker_retry_request(&command) {
            self.modal.session_picker_retry_command = Some(request);
        }
        let commands = self.tab_state.topology_commands.clone();
        let sent =
            try_topology_command_with_rollback(commands.as_ref(), command, |target, pending| {
                self.rollback_pending_remote_split(target, pending)
            });
        // Every caller sees the error and cleans up as before; the notice is
        // given here so a caller that does not end in `fail` still shows it.
        if let Err(error) = &sent
            && error.downcast_ref::<TopologyQueueFull>().is_some()
        {
            self.scheduling.note_busy(error);
        }
        sent
    }

    fn send_command(&mut self, command: WindowCommand) {
        if let WindowCommand::Input(bytes) = command {
            // Keys and IME commits reach the PTY through here; pastes go
            // through `send_input` and file drops through the drop target
            // check. All three share `pty_input_blocked`.
            if !self.pty_input_blocked()
                && let Err(error) = self.queue_terminal_input(bytes)
            {
                self.scheduling.fail(error);
            }
            return;
        }
        let Some(commands) = &self.panes.pane.commands else {
            return;
        };
        if let Err(error) = try_window_command(commands, command) {
            self.scheduling.fail(error);
        }
    }

    fn queue_terminal_input(&mut self, bytes: Vec<u8>) -> Result<()> {
        let commands = self.panes.pane.commands.clone();
        self.queue_terminal_input_for(commands.as_ref(), bytes)
    }

    fn queue_terminal_input_for(
        &mut self,
        commands: Option<&Sender<WindowCommand>>,
        bytes: Vec<u8>,
    ) -> Result<()> {
        let outcome =
            queue_terminal_input(commands, &mut self.input.pending_terminal_input, bytes)?;
        if outcome == TerminalInputQueueOutcome::Saturated && !self.input.terminal_input_saturated {
            eprintln!("splinterm input queue saturated; dropping excess terminal input");
        }
        self.input.terminal_input_saturated = outcome == TerminalInputQueueOutcome::Saturated;
        Ok(())
    }

    fn flush_pending_terminal_input(&mut self) {
        // Input queued before a handoff pause is dropped, never replayed.
        if self.handoff_paused() {
            self.input.pending_terminal_input.clear();
            self.input.terminal_input_saturated = false;
            return;
        }
        if try_flush_pending_terminal_input(&mut self.input.pending_terminal_input) {
            self.input.terminal_input_saturated = false;
        }
    }

    fn send_coalescible_input(&mut self, bytes: Vec<u8>) {
        if self.modal.input_modal_open()
            || self.handoff_paused()
            || !self.input.pending_terminal_input.is_empty()
        {
            return;
        }
        let Some(commands) = &self.panes.pane.commands else {
            return;
        };
        match commands.try_send(WindowCommand::Input(bytes)) {
            Ok(()) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Closed(_)) => {
                self.scheduling
                    .fail(anyhow::anyhow!("Wayland command receiver disconnected"));
            }
        }
    }

    fn commit_text_input(&mut self) {
        if let Some(text_input) = &self.input.text_input {
            text_input.commit();
            self.input.ime.note_client_commit();
        }
    }

    fn focused_logical_rect(&self) -> Rect {
        self.panes
            .focused_splint()
            .and_then(|splint_id| {
                self.computed_pane_layout()
                    .and_then(|layout| layout.rect(splint_id))
            })
            .unwrap_or_else(|| self.content_rect())
    }

    fn ime_cursor_rectangle(&self) -> Option<(i32, i32, i32, i32)> {
        let frame = self.panes.pane.snapshot_frame.as_ref()?;
        let rect = self.focused_logical_rect();
        let geometry = self
            .panes
            .pane
            .window_geometry(frame, rect.width, rect.height, self.surface.scale_120)
            .ok()?;
        offset_cursor_rectangle(frame.cursor_rectangle(&geometry)?, rect)
    }

    fn update_ime_cursor_rectangle(&mut self) {
        if !self.input.ime.entered || !self.input.ime.focused {
            return;
        }
        let Some(text_input) = &self.input.text_input else {
            return;
        };
        if let Some((x, y, width, height)) = self.ime_cursor_rectangle() {
            text_input.set_cursor_rectangle(x, y, width, height);
            self.commit_text_input();
        }
    }

    fn sync_graphical_focus(&self) {
        update_graphical_focus_watch(
            self.input.graphical_focus.as_ref(),
            self.input.keyboard_focused,
            self.panes.focused_splint(),
        );
    }

    fn set_ime_focus(&mut self, focused: bool) {
        if !focused {
            self.cancel_clipboard_image();
            self.clipboard.palette_image_target = None;
        }
        self.input.keyboard_focused = focused;
        self.input.ime.focused = focused;
        self.sync_graphical_focus();
        if !focused && self.input.ime.entered {
            if let Some(text_input) = &self.input.text_input {
                text_input.disable();
            }
            self.commit_text_input();
            self.clear_ime_preedit();
        } else if self.input.ime.entered && self.ime_gate().allowed(focused) {
            self.enable_text_input();
        }
    }

    fn renew_text_input(&mut self, queue_handle: &QueueHandle<Self>) {
        if let Some(text_input) = self.input.text_input.take() {
            text_input.destroy();
        }
        self.input.ime_generation = self.input.ime_generation.saturating_add(1);
        self.input.ime.entered = false;
        self.input.ime.clear_composition();
        self.input.ime_modal_barrier = false;
        if let (Some(manager), Some(seat)) = (
            &self.platform.text_input_manager,
            &self.input.text_input_seat,
        ) {
            self.input.text_input =
                Some(manager.get_text_input(seat, queue_handle, self.input.ime_generation));
        }
    }

    /// Re-enables IME after a modal closes only when the surviving focus may
    /// compose; Explorer focus keeps the text input disabled.
    fn enable_text_input_if_allowed(&mut self) {
        if self.ime_gate().allowed(self.input.keyboard_focused) {
            self.enable_text_input();
            return;
        }
        if let Some(text_input) = &self.input.text_input {
            text_input.disable();
        }
        self.commit_text_input();
        self.clear_ime_preedit();
    }

    fn enable_text_input(&mut self) {
        let Some(text_input) = &self.input.text_input else {
            return;
        };
        text_input.enable();
        text_input.set_content_type(
            zwp_text_input_v3::ContentHint::None,
            zwp_text_input_v3::ContentPurpose::Terminal,
        );
        if let Some((x, y, width, height)) = self.ime_cursor_rectangle() {
            text_input.set_cursor_rectangle(x, y, width, height);
        }
        self.commit_text_input();
    }

    fn clear_ime_preedit(&mut self) {
        let had_preedit = self.input.ime.visible_preedit.is_some();
        self.input.ime.clear_composition();
        if had_preedit {
            let _ = self.refresh_ime_preedit();
        }
    }

    fn refresh_ime_preedit(&mut self) -> Result<()> {
        // The prepared frame represents the display viewport, which may be
        // detached from the live grid. Refreshing it from `self.panes.pane.snapshot`
        // corrupts an unrelated history row when focus loss clears preedit.
        let Some(mut render_snapshot) = self.panes.display_snapshot() else {
            return Ok(());
        };
        let Some(row) = apply_ime_preedit(
            &mut render_snapshot,
            self.input.ime.visible_preedit.as_deref(),
        ) else {
            return Ok(());
        };
        let Some(frame) = &mut self.panes.pane.snapshot_frame else {
            return Ok(());
        };
        let mut dirty = vec![false; render_snapshot.rows];
        dirty[row] = true;
        frame.refresh_rows_with_context(
            &render_snapshot,
            &dirty,
            &self.presentation.render_context,
        )?;
        self.panes
            .pane
            .raster_dirty_rows
            .resize(render_snapshot.rows, false);
        self.panes
            .pane
            .surface_dirty_rows
            .resize(render_snapshot.rows, false);
        self.panes.pane.raster_dirty_rows[row] = true;
        self.panes.pane.surface_dirty_rows[row] = true;
        Ok(())
    }

    fn rebuild_scaled_pane_frames(&mut self, scale_120: u32) -> Result<bool> {
        let mut rebuilt = rebuild_pane_scaled_frame_with_context(
            &mut self.panes.pane,
            scale_120,
            &self.presentation.render_context,
        )?;
        for pane in &mut self.panes.inactive_panes {
            rebuilt |= rebuild_pane_scaled_frame_with_context(
                pane,
                scale_120,
                &self.presentation.render_context,
            )?;
        }
        Ok(rebuilt)
    }

    fn reconcile_font_generation(&mut self, update: FontUpdate) -> Result<bool> {
        let mut candidate_context = self.presentation.render_context.clone();
        if !candidate_context.replace_font_generation(update.generation) {
            return Ok(false);
        }
        self.install_render_context(candidate_context)
    }

    /// Applies the theme's followed Omarchy text size when it differs from the
    /// size last applied. A rejected size, typically because runtime zoom would
    /// push it past the renderer's limit, is not retried until the request or
    /// the zoom changes. Returns whether frames were rebuilt.
    fn reconcile_followed_text_size(&mut self) -> bool {
        let held = self.presentation.theme.shell.text_size_held;
        if held {
            // The shell value became unparseable: keep the size actually
            // applied and cancel any rejected request awaiting a zoom change.
            self.presentation.rejected_text_size = None;
        }
        let followed = self.presentation.theme.shell.text_size_px;
        let attempt = (followed, self.presentation.font_zoom_steps);
        if !followed_text_size_due(
            followed,
            held,
            self.presentation.applied_text_size_px,
            self.presentation.rejected_text_size,
            self.presentation.font_zoom_steps,
        ) {
            return false;
        }
        match self.reconcile_font_size(followed_font_size(self.presentation.theme)) {
            Ok(rebuilt) => {
                self.presentation.applied_text_size_px = followed;
                self.presentation.rejected_text_size = None;
                rebuilt
            }
            Err(error) => {
                self.presentation.rejected_text_size = Some(attempt);
                eprintln!("splinterm live text size update rejected: {error:#}");
                false
            }
        }
    }

    /// Applies a followed Omarchy text size through the same reconciliation as
    /// a live font generation. A base change that leaves the raster size
    /// unchanged is recorded without rebuilding frames.
    fn reconcile_font_size(&mut self, size: Option<FontSize>) -> Result<bool> {
        let mut candidate_context = self.presentation.render_context.clone();
        if !candidate_context.replace_font_size_override(size, self.surface.scale_120)? {
            self.presentation.render_context = candidate_context;
            return Ok(false);
        }
        self.install_render_context(candidate_context)
    }

    /// Applies the chrome text size the configured `[main] ui-font-pixelsize`
    /// and the theme's Omarchy text size resolve to. Chrome text never
    /// shares the terminal's size, so only cached chrome text is dropped:
    /// terminal frames and cell metrics stay as they are, and the PTY size
    /// too unless a shown rail, sized in chrome cells, changes width.
    /// Returns whether the size changed.
    fn reconcile_chrome_font_size(&mut self) -> bool {
        let size = chrome_font_size(self.presentation.ui_font_size, self.presentation.theme);
        let changed = self
            .presentation
            .render_context
            .replace_chrome_font_size(size);
        if changed {
            self.invalidate_chrome_text();
            // The rail is four chrome cells wide, so the terminal and the
            // tab strip beside it move with the chrome text.
            if self.explorer.visible() && self.explorer.collapsed() {
                self.relayout_tab_strip();
                self.relayout_panes();
            }
        }
        changed
    }

    /// Drops every cached chrome text and the layouts measured from it, so
    /// the next draw shapes chrome again at the render context's sizes.
    fn invalidate_chrome_text(&mut self) {
        self.presentation.renderer_generation =
            self.presentation.renderer_generation.saturating_add(1);
        self.presentation.frame_titles.clear();
        for tab in self.tab_state.tabs.iter_mut() {
            if let Some(view) = tab.value.as_mut() {
                view.frame_titles.clear();
            }
        }
        self.modal.session_picker_text_cache.clear();
        self.presentation.explorer_text_cache.clear();
        self.presentation.search_text_cache.clear();
        self.modal.command_palette_text_cache.clear();
        self.modal.dojo_prompt_text_cache.clear();
        self.modal.tab_context_menu_text_cache.clear();
        self.handoff.text_cache.clear();
        self.invalidate_tab_strip_text();
        self.modal.session_picker_layout = None;
        self.presentation.explorer_layout = None;
        self.modal.command_palette_layout = None;
        self.modal.dojo_prompt_layout = None;
        self.modal.tab_context_menu_layout = None;
        self.presentation.full_redraw = true;
    }

    /// Rebuilds every pane frame and text cache for a staged render context,
    /// then installs it and lets the pane controller resize the PTY.
    fn install_render_context(&mut self, candidate_context: RenderContext) -> Result<bool> {
        let focused = if let Some(mut display) = self.panes.display_snapshot() {
            apply_ime_preedit(&mut display, self.input.ime.visible_preedit.as_deref());
            Some(SnapshotFrame::load_scaled_with_sources_and_context(
                &display,
                self.surface.scale_120,
                Some(&self.panes.pane.image_sources),
                &candidate_context,
            )?)
        } else {
            None
        };
        let inactive = self
            .panes
            .inactive_panes
            .iter()
            .map(|pane| {
                prepare_pane_scaled_frame_with_context(
                    pane,
                    self.surface.scale_120,
                    &candidate_context,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let hidden = self
            .tab_state
            .tabs
            .iter()
            .map(|tab| {
                let Some(view) = tab.value.as_ref() else {
                    return Ok(None);
                };
                let focused = prepare_pane_scaled_frame_with_context(
                    &view.pane,
                    self.surface.scale_120,
                    &candidate_context,
                )?;
                let inactive = view
                    .inactive_panes
                    .iter()
                    .map(|pane| {
                        prepare_pane_scaled_frame_with_context(
                            pane,
                            self.surface.scale_120,
                            &candidate_context,
                        )
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(Some(PreparedTabFontFrames { focused, inactive }))
            })
            .collect::<Result<Vec<_>>>()?;

        self.presentation.render_context = candidate_context;
        install_prepared_pane_frame(&mut self.panes.pane, focused);
        for (pane, frame) in self.panes.inactive_panes.iter_mut().zip(inactive) {
            install_prepared_pane_frame(pane, frame);
        }
        for (tab, prepared) in self.tab_state.tabs.iter_mut().zip(hidden) {
            let (Some(view), Some(prepared)) = (tab.value.as_mut(), prepared) else {
                continue;
            };
            install_prepared_pane_frame(&mut view.pane, prepared.focused);
            for (pane, frame) in view.inactive_panes.iter_mut().zip(prepared.inactive) {
                install_prepared_pane_frame(pane, frame);
            }
            view.frame_titles.clear();
            view.dirty_inactive_panes.clear();
        }
        clear_snapshot_caches();
        self.invalidate_chrome_text();
        self.surface.buffers.clear();
        self.surface.backing.clear();
        self.presentation.full_redraw = true;
        self.input.cursor_blink_visible = true;
        self.input.last_cursor_blink = Instant::now();
        self.update_ime_cursor_rectangle();
        if self.surface.configured {
            self.emit_font_resize()?;
        }
        Ok(true)
    }

    fn toggle_tab_strip(&mut self) -> Result<bool> {
        if !self.tab_state.managed_tabs {
            return Ok(false);
        }
        self.tab_state.tab_strip_visible = !self.tab_state.tab_strip_visible;
        self.tab_state.tab_strip_layout = None;
        if !self.tab_state.tab_strip_visible
            && let Some((button, _)) = self.tab_state.tab_strip_pressed.take()
        {
            self.input
                .pressed_buttons
                .insert(button, PressOwner::Ignored);
        }
        self.presentation.full_redraw = true;
        self.update_ime_cursor_rectangle();
        if self.surface.configured {
            self.emit_resize()?;
        }
        Ok(true)
    }

    /// Follows a live strip height change: the strip, and with it the content
    /// area below, takes the new height the way showing or hiding the strip
    /// does. Overlays positioned from the content area follow it on the next
    /// draw. The panes are resized once the frame's updates are applied, so a
    /// text size change in the same pass resizes them only once.
    fn relayout_tab_strip(&mut self) {
        self.invalidate_tab_strip_text();
        self.presentation.full_redraw = true;
        self.update_ime_cursor_rectangle();
    }

    fn apply_font_zoom(
        &mut self,
        action: FontZoomAction,
        queue_handle: &QueueHandle<Self>,
    ) -> Result<bool> {
        let next = match action {
            FontZoomAction::Increase => self.presentation.font_zoom_steps.saturating_add(1),
            FontZoomAction::Decrease => self.presentation.font_zoom_steps.saturating_sub(1),
            FontZoomAction::Reset => 0,
        };
        if next == self.presentation.font_zoom_steps {
            return Ok(true);
        }
        let Some(raster_changed) = self
            .presentation
            .render_context
            .set_font_zoom_steps(next, self.surface.scale_120)?
        else {
            return Ok(true);
        };
        self.presentation.font_zoom_steps = next;
        self.presentation.renderer_generation =
            self.presentation.renderer_generation.saturating_add(1);
        self.modal.session_picker_text_cache.clear();
        self.tab_state.tab_label_cache.clear();
        self.tab_state.tab_close_text = None;
        self.tab_state.remote_host_text = None;
        self.tab_state.remote_host_clipped = None;
        self.tab_state.tab_new_text = None;
        self.modal.session_picker_layout = None;
        // A followed text size rejected at the previous zoom may fit now; its
        // reconciliation rebuilds every frame for the new zoom as well.
        if self.reconcile_followed_text_size() {
            if self.surface.configured {
                self.schedule_draw(queue_handle)?;
            }
            return Ok(true);
        }
        if !raster_changed {
            if self.modal.inline_picker_open() && self.surface.configured {
                self.modal.session_picker_redraw = true;
                self.schedule_draw(queue_handle)?;
            }
            return Ok(true);
        }
        self.rebuild_scaled_pane_frames(self.surface.scale_120)?;
        self.surface.buffers.clear();
        self.surface.backing.clear();
        self.panes.pane.pending_scrolls.clear();
        self.presentation.full_redraw = true;
        self.input.cursor_blink_visible = true;
        self.input.last_cursor_blink = Instant::now();
        self.refresh_ime_preedit()?;
        self.update_ime_cursor_rectangle();
        if self.surface.configured {
            if self.modal.inline_picker_open() {
                self.panes.restored_frontend_needs_resize = true;
            } else {
                self.emit_resize()?;
            }
            self.schedule_draw(queue_handle)?;
        }
        Ok(true)
    }

    fn decide_consent(&mut self, granted: bool) {
        if let Some(consent) = self.modal.trusted_consent.take() {
            consent.answer(granted);
        }
        self.scheduling.request_exit(ExitClass::CleanUserClose);
    }

    /// Acts on a trusted dialog key or click: an answer closes the Window; a
    /// new duration selection draws the dialog again.
    fn trusted_dialog_input(
        &mut self,
        input: Option<TrustedInput>,
        queue_handle: &QueueHandle<Self>,
    ) {
        match input {
            Some(TrustedInput::Answer(granted)) => self.decide_consent(granted),
            Some(TrustedInput::Redraw) => {
                let Some(snapshot) = self
                    .modal
                    .trusted_consent
                    .as_mut()
                    .and_then(|consent| consent.render(Instant::now()))
                else {
                    return;
                };
                if let Err(error) = self
                    .install_trusted_dialog_snapshot(snapshot)
                    .and_then(|()| self.schedule_draw(queue_handle))
                {
                    self.scheduling.fail(error);
                }
            }
            None => {}
        }
    }

    fn refresh_session_picker(&mut self) -> Result<()> {
        let Some(picker) = self.modal.session_picker.as_mut() else {
            return Ok(());
        };
        if picker.is_inline() {
            picker.clear_hovered();
            self.modal.session_picker_layout = None;
            self.modal.session_picker_pressed = None;
            self.modal.session_picker_redraw = true;
            return Ok(());
        }
        let mut snapshot = picker.snapshot();
        apply_theme(&mut snapshot, self.presentation.theme);
        self.panes.pane.snapshot = Some(snapshot);
        rebuild_pane_scaled_frame_with_context(
            &mut self.panes.pane,
            self.surface.scale_120,
            &self.presentation.render_context,
        )?;
        self.surface.buffers.clear();
        self.surface.backing.clear();
        self.presentation.full_redraw = true;
        Ok(())
    }

    fn move_session_picker(&mut self, delta: isize) {
        if let Some(picker) = self.modal.session_picker.as_mut() {
            picker.move_selection(delta);
        }
        if let Err(error) = self.refresh_session_picker() {
            self.scheduling.fail(error);
        }
    }

    fn handle_pane_divider_pointer(&mut self, event: &PointerEvent) -> Result<bool> {
        if let Some(mut drag) = self.input.divider_drag {
            match event.kind {
                PointerEventKind::Motion { .. } | PointerEventKind::Enter { .. } => {
                    let Some(ratio) = split_ratio_at(drag.split, event.position, 1) else {
                        return Ok(true);
                    };
                    if drag.ratio == Some(ratio) {
                        return Ok(true);
                    }
                    let Some(mut candidate) = self.panes.layout.clone() else {
                        self.input.divider_drag = None;
                        return Ok(false);
                    };
                    let Some(current) =
                        split_ratio_mut(&mut candidate, drag.split.target, drag.split.ancestor)
                    else {
                        self.input.divider_drag = None;
                        return Ok(false);
                    };
                    let before = std::mem::replace(current, ratio);
                    let previous = self.panes.layout.replace(candidate);
                    // A drag stops where a pane would reach its minimum; it never
                    // pushes the layout into showing only the focused Splint.
                    if !matches!(self.pane_layout_fit(), Ok(Some((_, LayoutFit::All)))) {
                        self.panes.layout = previous;
                        return Ok(true);
                    }
                    drag.ratio = Some(ratio);
                    drag.original.get_or_insert(before);
                    self.input.divider_drag = Some(drag);
                    self.panes.pane.pointer_cell = None;
                    self.panes.pane.hovered_url = None;
                    self.presentation.full_redraw = true;
                    Ok(true)
                }
                PointerEventKind::Release { button, .. } if button == BTN_LEFT => {
                    self.input.divider_drag = None;
                    if let Some(ratio) = drag.ratio {
                        let sent = self.send_topology_command(WindowTopologyCommand::SetRatio {
                            dojo_id: self.tab_state.active_identity.dojo_id,
                            target: drag.split.target,
                            ancestor: drag.split.ancestor,
                            ratio,
                        });
                        if let Err(error) = sent {
                            // The manager kept its ratio, so the Window shows
                            // it again rather than the unsent preview.
                            if error.downcast_ref::<TopologyQueueFull>().is_some()
                                && restore_divider_ratio(self.panes.layout.as_mut(), &drag)
                            {
                                self.presentation.full_redraw = true;
                                if self.surface.configured {
                                    self.scheduling.redraw_pending = true;
                                }
                            }
                            return Err(error);
                        }
                    }
                    Ok(true)
                }
                PointerEventKind::Leave { .. }
                | PointerEventKind::Press { .. }
                | PointerEventKind::Axis { .. }
                | PointerEventKind::Release { .. } => Ok(true),
            }
        } else if matches!(
            event.kind,
            PointerEventKind::Press {
                button: BTN_LEFT,
                ..
            }
        ) {
            let split = self
                .computed_pane_layout()
                .and_then(|layout| layout.split_at(event.position, 6));
            if let Some(split) = split {
                // A layout change waits out a daemon handoff: the press is
                // consumed without starting a drag.
                if !self.handoff_paused() {
                    self.input.divider_drag = Some(DividerDrag {
                        split,
                        ratio: None,
                        original: None,
                    });
                }
                self.panes.pane.pointer_cell = None;
                self.panes.pane.hovered_url = None;
                return Ok(true);
            }
            Ok(false)
        } else {
            Ok(false)
        }
    }

    fn handle_tab_strip_pointer(&mut self, event: &PointerEvent) -> Result<bool> {
        let Some(layout) = self.tab_state.tab_strip_layout.as_ref() else {
            return Ok(false);
        };
        let target = tab_strip_hit_test(layout, event.position);
        let in_strip = rect_contains(layout.rect, event.position);
        match event.kind {
            PointerEventKind::Press { button, .. }
                if in_strip && matches!(button, BTN_LEFT | BTN_MIDDLE | BTN_RIGHT) =>
            {
                let target = if button == BTN_RIGHT {
                    target
                        .and_then(tab_context_target)
                        .map(TabHitTarget::Activate)
                } else {
                    target
                };
                if let Some(target) = target {
                    self.tab_state.tab_strip_pressed = Some((button, target));
                }
                self.panes.pane.pointer_cell = None;
                self.panes.pane.hovered_url = None;
                Ok(true)
            }
            PointerEventKind::Release { button, .. } => {
                let Some((pressed_button, _)) = self.tab_state.tab_strip_pressed else {
                    return Ok(false);
                };
                if pressed_button != button {
                    return Ok(false);
                }
                let (_, pressed_target) = self
                    .tab_state
                    .tab_strip_pressed
                    .take()
                    .expect("matching chrome press remains present");
                let release_target = if button == BTN_RIGHT {
                    target
                        .and_then(tab_context_target)
                        .map(TabHitTarget::Activate)
                } else {
                    target
                };
                if Some(pressed_target) == release_target {
                    if let (BTN_RIGHT, TabHitTarget::Activate(dojo_id)) = (button, pressed_target) {
                        return Ok(consume_context_menu_request(|| {
                            self.show_tab_context_menu(dojo_id, event.position)
                        }));
                    }
                    let command = match (button, pressed_target) {
                        (BTN_LEFT, TabHitTarget::Activate(dojo_id)) => {
                            Some(WindowTopologyCommand::ActivateTab { dojo_id })
                        }
                        (BTN_LEFT | BTN_MIDDLE, TabHitTarget::Close(dojo_id))
                        | (BTN_MIDDLE, TabHitTarget::Activate(dojo_id)) => {
                            Some(WindowTopologyCommand::CloseTab { dojo_id })
                        }
                        (BTN_LEFT, TabHitTarget::New) => Some(WindowTopologyCommand::NewDojo {
                            lair_id: self.tab_state.active_identity.lair_id,
                            source_splint_id: self.focused_cwd_source()?,
                        }),
                        _ => None,
                    };
                    if let Some(command) = command {
                        self.send_topology_command(command)?;
                    }
                }
                Ok(true)
            }
            PointerEventKind::Leave { .. } => {
                self.tab_state.tab_strip_pressed = None;
                Ok(in_strip)
            }
            _ => Ok(in_strip || self.tab_state.tab_strip_pressed.is_some()),
        }
    }

    fn handle_command_palette_pointer(
        &mut self,
        event: &PointerEvent,
        queue_handle: &QueueHandle<Self>,
    ) -> bool {
        let Some(committed_layout) = self.modal.command_palette_layout.as_ref() else {
            return false;
        };
        let target = command_palette_hit_test(committed_layout, event.position);
        let inside_panel = rect_contains(committed_layout.panel, event.position);
        if self.modal.agent_access.is_some() {
            return self.handle_agent_access_pointer(event, inside_panel);
        }
        let mut changed = false;
        let mut execute = None;
        match event.kind {
            PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                if let Some(palette) = self.modal.command_palette.as_mut() {
                    changed |= palette.update_hovered(target);
                }
            }
            PointerEventKind::Leave { .. } => {
                if let Some(palette) = self.modal.command_palette.as_mut() {
                    changed |= palette.update_hovered(None);
                }
            }
            PointerEventKind::Press { button, .. } if button == BTN_LEFT => {
                if inside_panel {
                    let next = target.filter(|command| {
                        self.modal
                            .command_palette
                            .as_ref()
                            .is_some_and(|palette| palette.command_enabled(*command))
                    });
                    if self.modal.command_palette_pressed != next {
                        self.modal.command_palette_pressed = next;
                        changed = true;
                    }
                } else {
                    changed |= self.close_command_palette();
                }
            }
            PointerEventKind::Release { button, .. } if button == BTN_LEFT => {
                let pressed = self.modal.command_palette_pressed.take();
                changed |= pressed.is_some();
                if self.modal.binding_help.is_none() && pressed.is_some() && pressed == target {
                    execute = target;
                }
            }
            PointerEventKind::Axis { vertical, .. } if !vertical.is_none() => {
                if let Some((direction, count)) = self.modal.session_picker_wheel.push(
                    vertical.absolute,
                    vertical.discrete,
                    vertical.value120,
                    44,
                ) {
                    let count = isize::try_from(count).unwrap_or(isize::MAX);
                    let delta = match direction {
                        MouseAction::WheelUp => -count,
                        MouseAction::WheelDown => count,
                        _ => 0,
                    };
                    if let Some(help) = self.modal.binding_help.as_mut() {
                        changed |= help.move_selection(delta);
                    } else if let Some(palette) = self.modal.command_palette.as_mut() {
                        changed |= palette.move_selection(delta);
                        palette.update_hovered(None);
                    }
                }
            }
            _ => {}
        }
        if let Some(command) = execute {
            self.execute_command_palette(command, queue_handle);
            changed = true;
        }
        if changed {
            self.refresh_command_palette();
        }
        changed
    }

    fn handle_dojo_prompt_pointer(&mut self, event: &PointerEvent) -> bool {
        let Some(committed_layout) = self.modal.dojo_prompt_layout.as_ref() else {
            return false;
        };
        let target = dojo_prompt_hit_test(committed_layout, event.position);
        let inside_panel = rect_contains(committed_layout.panel, event.position);
        let mut changed = false;
        let mut execute = false;
        match event.kind {
            PointerEventKind::Press { button, .. } if button == BTN_LEFT => {
                if inside_panel {
                    if self.modal.dojo_prompt_pressed != target {
                        self.modal.dojo_prompt_pressed = target;
                        changed = true;
                    }
                    if let Some(decision) = target {
                        match self.modal.dojo_prompt.as_mut() {
                            Some(DojoPromptUi::Terminate(confirmation)) => {
                                changed |= confirmation.select(decision);
                            }
                            Some(DojoPromptUi::RestoreLair(confirmation)) => {
                                changed |= confirmation.select(decision);
                            }
                            Some(DojoPromptUi::TerminateLair(confirmation)) => {
                                changed |= confirmation.select(decision);
                            }
                            Some(DojoPromptUi::RevokeAccess(confirmation)) => {
                                changed |= confirmation.select(decision);
                            }
                            Some(
                                DojoPromptUi::Rename(_)
                                | DojoPromptUi::RenameLair(_)
                                | DojoPromptUi::PreviewLair(_),
                            )
                            | None => {}
                        }
                    }
                } else {
                    changed |= self.close_dojo_prompt();
                }
            }
            PointerEventKind::Release { button, .. } if button == BTN_LEFT => {
                let pressed = self.modal.dojo_prompt_pressed.take();
                changed |= pressed.is_some();
                if self
                    .modal
                    .dojo_prompt
                    .as_ref()
                    .is_some_and(DojoPromptUi::is_preview)
                    && pressed == Some(TerminationDecision::Cancel)
                    && pressed == target
                {
                    changed |= self.close_dojo_prompt();
                } else {
                    execute = pressed.is_some() && pressed == target;
                }
            }
            _ => {}
        }
        if execute {
            self.execute_dojo_prompt();
            changed = true;
        }
        if changed && self.modal.dojo_prompt.is_some() {
            self.refresh_dojo_prompt();
        }
        changed
    }

    fn context_menu_right_target(
        &self,
        position: (f64, f64),
        inside_menu: bool,
    ) -> TabMenuRightPress {
        if inside_menu {
            return TabMenuRightPress::Dismiss;
        }
        let tab_target = self
            .tab_state
            .tab_strip_layout
            .as_ref()
            .and_then(|layout| tab_strip_hit_test(layout, position))
            .and_then(tab_context_target);
        let explorer_target = self
            .presentation
            .explorer_layout
            .as_ref()
            .and_then(|layout| lair_explorer_hit_test(layout, position))
            .and_then(|node| self.explorer_context_menu(node))
            .and_then(|menu| menu.explorer_target());
        tab_menu_right_press(tab_target, explorer_target)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "matching press/release and retarget actions share modal pointer ownership"
    )]
    fn handle_tab_context_menu_pointer(&mut self, event: &PointerEvent) -> bool {
        let Some(committed_layout) = self.modal.tab_context_menu_layout.as_ref() else {
            return false;
        };
        let target = tab_context_menu_hit_test(committed_layout, event.position);
        let inside_panel = rect_contains(committed_layout.panel, event.position);
        let right_target = self.context_menu_right_target(event.position, inside_panel);
        let mut changed = false;
        let mut execute = None;
        let mut retarget = None;
        match event.kind {
            PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                if let Some(menu) = self.modal.tab_context_menu.as_mut() {
                    changed |= menu.update_hovered(target);
                }
            }
            PointerEventKind::Leave { .. } => {
                if let Some(menu) = self.modal.tab_context_menu.as_mut() {
                    changed |= menu.update_hovered(None);
                }
            }
            PointerEventKind::Press {
                button: BTN_RIGHT, ..
            } => match right_target {
                target @ (TabMenuRightPress::Retarget(_)
                | TabMenuRightPress::RetargetExplorer(_)) => {
                    self.modal.tab_context_menu_pressed = None;
                    if self.modal.tab_context_menu_retarget != Some(target) {
                        self.modal.tab_context_menu_retarget = Some(target);
                        changed = true;
                    }
                }
                TabMenuRightPress::Dismiss => {
                    self.modal.tab_context_menu_retarget = None;
                    changed |= self.close_tab_context_menu();
                }
            },
            PointerEventKind::Release {
                button: BTN_RIGHT, ..
            } => {
                let pressed = self.modal.tab_context_menu_retarget.take();
                changed |= pressed.is_some();
                if pressed.is_some() && pressed == Some(right_target) {
                    retarget = pressed;
                }
            }
            PointerEventKind::Press {
                button: BTN_LEFT, ..
            } => {
                if inside_panel {
                    let target = target.filter(|action| {
                        self.modal
                            .tab_context_menu
                            .as_ref()
                            .is_some_and(|menu| menu.action_enabled(*action))
                    });
                    if self.modal.tab_context_menu_pressed != target {
                        self.modal.tab_context_menu_pressed = target;
                        changed = true;
                    }
                } else {
                    changed |= self.close_tab_context_menu();
                }
            }
            PointerEventKind::Release {
                button: BTN_LEFT, ..
            } => {
                let pressed = self.modal.tab_context_menu_pressed.take();
                changed |= pressed.is_some();
                execute = self
                    .modal
                    .tab_context_menu
                    .as_ref()
                    .and_then(|menu| menu.release_action(pressed, target));
            }
            PointerEventKind::Press { .. }
            | PointerEventKind::Release { .. }
            | PointerEventKind::Axis { .. } => {}
        }
        if let Some(target) = retarget {
            let result = match target {
                TabMenuRightPress::Retarget(dojo_id) => {
                    self.show_tab_context_menu(dojo_id, event.position)
                }
                TabMenuRightPress::RetargetExplorer(target) => {
                    self.show_explorer_context_menu(target.node, event.position)
                }
                TabMenuRightPress::Dismiss => unreachable!("dismissal does not retain a press"),
            };
            if result.is_err() {
                eprintln!("splinterm tab menu retarget failed");
            }
            changed = true;
        } else if let Some(action) = execute {
            self.execute_tab_context_menu(action);
            changed = true;
        }
        if changed {
            self.refresh_tab_context_menu();
        }
        changed
    }

    fn handle_lair_explorer_pointer(
        &mut self,
        event: &PointerEvent,
        queue_handle: &QueueHandle<Self>,
    ) -> bool {
        let Some(inside_panel) = self
            .presentation
            .explorer_layout
            .as_ref()
            .map(|layout| rect_contains(layout.panel, event.position))
        else {
            return false;
        };
        if explorer_pointer_returns_to_terminal(self.explorer.focused(), inside_panel, &event.kind)
        {
            self.set_explorer_focus(false, queue_handle);
            if self.surface.configured
                && let Err(error) = self.schedule_draw(queue_handle)
            {
                self.scheduling.fail(error);
            }
        }
        if !inside_panel {
            let owned = self.presentation.explorer_pressed.is_some()
                || self.explorer.context_press_pending();
            if matches!(
                event.kind,
                PointerEventKind::Release { .. } | PointerEventKind::Leave { .. }
            ) {
                self.presentation.explorer_pressed = None;
                self.explorer.cancel_context_press();
            }
            return owned;
        }
        let Some(layout) = self.presentation.explorer_layout.as_ref() else {
            return false;
        };
        let target = lair_explorer_hit_test(layout, event.position);
        let filter = lair_explorer_filter_hit_test(layout, event.position);
        let action = lair_explorer_action_hit_test(layout, event.position);
        let on_chrome = lair_explorer_footer_hit_test(layout, event.position)
            || lair_explorer_agents_label_hit_test(layout, event.position);
        let agent = lair_explorer_agent_hit_test(layout, event.position);
        let rail = layout.rail;
        let disclosure =
            lair_explorer_disclosure_hit_test(layout, &self.explorer.rows(), event.position);
        let mut changed = false;
        match event.kind {
            PointerEventKind::Press {
                button: BTN_RIGHT, ..
            } => {
                self.presentation.explorer_pressed = None;
                self.explorer.context_press(target.or(agent));
            }
            PointerEventKind::Release {
                button: BTN_RIGHT, ..
            } => {
                if let Some(node) = self.explorer.context_release(target.or(agent)) {
                    if self
                        .show_explorer_context_menu(node, event.position)
                        .is_err()
                    {
                        eprintln!("splinterm Explorer menu is unavailable");
                    }
                    changed = true;
                }
            }
            PointerEventKind::Press { button, .. } if button == BTN_LEFT => {
                self.explorer.cancel_context_press();
                let (pressed, takes_focus) =
                    explorer_left_press(action, on_chrome, filter, (target, disclosure), agent);
                if takes_focus {
                    self.set_explorer_focus(true, queue_handle);
                }
                self.presentation.explorer_pressed = pressed;
                if let Some(target) = target {
                    changed |= self.explorer.select(target);
                } else if let Some(agent) = agent {
                    changed |= self.explorer.select_agent(agent);
                }
            }
            PointerEventKind::Release { button, .. } if button == BTN_LEFT => {
                changed |=
                    match explorer_released_action(self.presentation.explorer_pressed, action) {
                        Some(action) => self.release_explorer_action(action, event.position),
                        None => {
                            self.release_lair_explorer_pointer(target, disclosure, filter, agent)
                        }
                    };
            }
            PointerEventKind::Axis { vertical, .. } if !vertical.is_none() => {
                let up = vertical.discrete < 0 || vertical.value120 < 0 || vertical.absolute < 0.0;
                changed |= self.explorer_wheel(rail, if up { -1 } else { 1 });
            }
            PointerEventKind::Leave { .. } => {
                self.explorer.cancel_context_press();
                self.presentation.explorer_pressed = None;
            }
            _ => {}
        }
        if changed {
            self.presentation.full_redraw = true;
        }
        true
    }

    /// A wheel step over the Explorer: it moves the selection, except over
    /// an unfocused rail, which shows no selection, where it scrolls the
    /// rail's rows instead.
    fn explorer_wheel(&mut self, rail: bool, delta: isize) -> bool {
        if !rail || self.explorer.focused() {
            return self.explorer.move_selection(delta);
        }
        let Some(layout) = self.presentation.explorer_layout.as_ref() else {
            return false;
        };
        let start = explorer_rail_scrolled_start(
            layout.visible_start,
            layout.visible_capacity,
            self.explorer.rows().len(),
            delta,
        );
        if start == layout.visible_start {
            return false;
        }
        self.presentation.explorer_visible_start = start;
        self.presentation.explorer_layout = self.take_explorer_layout();
        true
    }

    /// Completes a click on a footer action token or the rail header's `»`.
    /// None of them takes keyboard focus; `menu` opens a menu, which, like
    /// a right-click's, takes the keys while it is open.
    fn release_explorer_action(&mut self, action: LairExplorerAction, at: (f64, f64)) -> bool {
        self.presentation.explorer_pressed = None;
        let mut menu = None;
        let command = explorer_action_command(action, || {
            let Some(node) = self.explorer.selected() else {
                return ExplorerRowMenu::NoSelection;
            };
            if let Some(ready) = self.explorer_context_menu(node) {
                menu = Some(ready);
                ExplorerRowMenu::Ready
            } else if self.explorer.can_context(node)
                && self.explorer.view().is_some_and(|view| {
                    view.freshness == crate::navigation_projection::EndpointFreshness::Current
                })
            {
                ExplorerRowMenu::Empty
            } else {
                ExplorerRowMenu::Unavailable
            }
        });
        match command {
            ExplorerActionCommand::Toggle => self.release_explorer_toggle(),
            ExplorerActionCommand::NewLair => {
                self.request_new_lair();
                false
            }
            ExplorerActionCommand::RowMenu => {
                if let Some(menu) = menu
                    && self.show_context_menu(menu, at).is_err()
                {
                    eprintln!("splinterm Explorer menu is unavailable");
                }
                true
            }
            ExplorerActionCommand::Palette => {
                if self.show_command_palette().is_err() {
                    eprintln!("splinterm command palette unavailable");
                }
                true
            }
            ExplorerActionCommand::Nothing => false,
        }
    }

    /// A new Lair from the focused Splint's directory, for the footer's `new`
    /// and the `lair.new` action alike. With no focused Splint to start
    /// from it only logs: asking for a Lair never closes the Window.
    fn request_new_lair(&mut self) {
        if !tab_action_dispatch_allowed([
            self.modal.session_picker.is_some(),
            self.modal.trusted_consent.is_some(),
            self.modal.session_picker_requested,
            self.tab_state.session_switch_pending,
            self.modal.session_picker_reconcile_pending,
        ]) {
            return;
        }
        let source_splint_id = match self.focused_cwd_source() {
            Ok(source_splint_id) => source_splint_id,
            Err(error) => {
                eprintln!("splinterm new Lair is unavailable: {error:#}");
                return;
            }
        };
        if let Err(error) =
            self.send_topology_command(WindowTopologyCommand::NewLair { source_splint_id })
        {
            self.scheduling.fail(error);
        }
    }

    /// Completes a click on the footer's `«` or the rail header's `»`: the
    /// Explorer collapses to its rail or expands, and keyboard focus stays
    /// where it is.
    fn release_explorer_toggle(&mut self) -> bool {
        match self.set_explorer_collapsed(!self.explorer.collapsed()) {
            Ok(toggled) => toggled,
            Err(error) => {
                eprintln!("splinterm Explorer resize failed: {error:#}");
                true
            }
        }
    }

    fn release_lair_explorer_pointer(
        &mut self,
        target: Option<NavigationNodeId>,
        disclosure: Option<NavigationNodeId>,
        filter: Option<LairExplorerFilter>,
        agent: Option<NavigationNodeId>,
    ) -> bool {
        let pressed = self.presentation.explorer_pressed.take();
        if let Some(filter) = explorer_released_filter(pressed, filter) {
            let changed = self.explorer.search_active();
            self.explorer.focus_tree();
            return self.explorer.set_filter(filter) || changed;
        }
        // An agents row activates its Splint as the tree's row does.
        if matches!(pressed, Some(ExplorerPointerPress::Agent(_))) {
            let Some(pressed) = explorer_released_agent(pressed, agent) else {
                return false;
            };
            let mut changed = self.explorer.select_agent(pressed);
            if self.explorer.agent_selected() != Some(pressed) {
                return changed;
            }
            if let Some(decision) = self.explorer.decision() {
                changed |= self.execute_lair_explorer_decision(decision);
            }
            return changed;
        }
        let Some(ExplorerPointerPress::Row(pressed, was_disclosure)) = pressed else {
            return false;
        };
        if Some(pressed) != target {
            return false;
        }
        let mut changed = self.explorer.select(pressed);
        // Keyboard navigation or a refresh between press and release must not
        // redirect this gesture to a different selected row.
        if self.explorer.selected() != Some(pressed) {
            return changed;
        }
        if was_disclosure && disclosure == Some(pressed) {
            changed |= self.explorer.toggle_selected();
        } else if let Some(decision) = self.explorer.decision() {
            changed |= self.execute_lair_explorer_decision(decision);
        }
        changed
    }

    fn handle_session_picker_pointer(&mut self, event: &PointerEvent) -> bool {
        let target = self
            .modal
            .session_picker_layout
            .as_ref()
            .and_then(|layout| session_picker_hit_test(layout, event.position));
        let mut changed = false;
        let mut activate = None;
        match event.kind {
            PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                if let Some(picker) = self.modal.session_picker.as_mut() {
                    changed |= picker.update_hovered(target);
                }
            }
            PointerEventKind::Leave { .. } => {
                if let Some(picker) = self.modal.session_picker.as_mut() {
                    changed |= picker.clear_hovered();
                }
            }
            PointerEventKind::Press { button, .. } => {
                let next = (button == BTN_LEFT).then_some(target).flatten();
                if self.modal.session_picker_pressed != next {
                    self.modal.session_picker_pressed = next;
                    changed = true;
                }
            }
            PointerEventKind::Release { button, .. } => {
                if button == BTN_LEFT {
                    let pressed = self.modal.session_picker_pressed.take();
                    changed |= pressed.is_some();
                    activate = picker_release_activation(pressed, target).filter(|target| {
                        !matches!(target, PickerHitTarget::New)
                            || self
                                .modal
                                .session_picker
                                .as_ref()
                                .is_some_and(SessionPickerUi::new_enabled)
                    });
                }
            }
            PointerEventKind::Axis { vertical, .. } => {
                if !vertical.is_none()
                    && let Some((direction, count)) = self.modal.session_picker_wheel.push(
                        vertical.absolute,
                        vertical.discrete,
                        vertical.value120,
                        44,
                    )
                {
                    let count = isize::try_from(count).unwrap_or(isize::MAX);
                    let delta = match direction {
                        MouseAction::WheelUp => -count,
                        MouseAction::WheelDown => count,
                        _ => 0,
                    };
                    self.move_session_picker(delta);
                    self.modal.session_picker_layout = None;
                    if let Some(picker) = self.modal.session_picker.as_mut() {
                        picker.clear_hovered();
                    }
                    changed = true;
                }
            }
        }
        if let Some(decision) = activate.and_then(|target| {
            self.modal
                .session_picker
                .as_ref()
                .and_then(|picker| picker.decision_for_target(target))
        }) {
            self.decide_session_picker(decision);
            changed = true;
        }
        if changed && self.modal.inline_picker_open() {
            self.modal.session_picker_redraw = true;
        }
        changed
    }

    fn cancel_session_picker(&mut self) {
        self.modal.session_picker_retry_command = None;
        if !self.close_inline_session_picker() {
            self.scheduling
                .request_exit(ExitClass::CleanSessionPickerDecision);
        }
    }

    fn decide_session_picker(&mut self, decision: SessionPickerDecision) {
        if self.modal.inline_picker_open()
            && let Some(names) = self.modal.remote_profiles.take()
        {
            self.close_inline_session_picker();
            let SessionPickerDecision::Open(index) = decision else {
                return;
            };
            if let Err(error) = crate::remote_launch::chosen_profile(&names, index)
                .and_then(|profile| crate::remote_launch::open_remote_window(&profile))
            {
                eprintln!("splinterm remote launch failed: {error:#}");
            }
            return;
        }
        if self.modal.inline_picker_open() {
            let selector_kind = self.modal.selector_kind;
            let command = match decision {
                SessionPickerDecision::New => {
                    let source_splint_id = match self.focused_cwd_source() {
                        Ok(source_splint_id) => source_splint_id,
                        Err(error) => {
                            self.scheduling.fail(error);
                            return;
                        }
                    };
                    let Some(target) = self.modal.session_picker_creation_target else {
                        self.scheduling.fail(anyhow::anyhow!(
                            "Dojo picker selected an unavailable creation target"
                        ));
                        return;
                    };
                    match session_picker_new_command(
                        selector_kind,
                        self.tab_state.active_identity.lair_id,
                        source_splint_id,
                        target,
                    ) {
                        Ok(command) => command,
                        Err(error) => {
                            self.scheduling.fail(error);
                            return;
                        }
                    }
                }
                SessionPickerDecision::Open(index) => {
                    let Some(target) = self.modal.session_picker_targets.get(index).copied() else {
                        self.scheduling
                            .fail(anyhow::anyhow!("Dojo picker selected an invalid target"));
                        return;
                    };
                    WindowTopologyCommand::OpenDojo {
                        target,
                        explorer_target: None,
                    }
                }
            };
            self.modal.session_picker_retry_command = Some(match selector_kind {
                Some(kind) => WindowTopologyCommand::RequestSelector {
                    kind,
                    lair_id: self.tab_state.active_identity.lair_id,
                },
                None => WindowTopologyCommand::RequestSessionPicker,
            });
            self.close_inline_session_picker();
            self.tab_state.session_switch_pending = true;
            if self.send_topology_command(command).is_err() {
                self.tab_state.session_switch_pending = false;
                eprintln!("splinterm session switch failed");
            }
            return;
        }
        if let Some(sender) = self
            .modal
            .session_picker
            .take()
            .and_then(SessionPickerUi::into_standalone_decision)
        {
            let _ = sender.send(decision);
        }
        self.scheduling
            .request_exit(ExitClass::CleanSessionPickerDecision);
    }

    fn update_window_title(&self) {
        if self.modal.copy_mode.is_some() {
            self.surface.window.set_title("Splinterm — COPY");
            return;
        }
        if self.modal.binding_help.is_some() {
            self.surface.window.set_title("Splinterm — Key bindings");
            return;
        }
        if self.modal.agent_access.is_some() {
            self.surface.window.set_title("Splinterm — Agent access");
            return;
        }
        if let Some(title) = self.terminal_window_title() {
            self.surface.window.set_title(title);
        }
    }

    /// The focused pane's terminal title, or `None` before it has a snapshot.
    fn terminal_window_title(&self) -> Option<String> {
        let snapshot = self.panes.pane.snapshot.as_ref()?;
        let title = window_title(
            self.presentation
                .title
                .text
                .as_deref()
                .or(Some(&snapshot.title)),
            self.panes.pane.controller_active && self.presentation.title.controller_label,
            self.clipboard.local_endpoint,
            &self.panes.pane.authority,
            // Accepting or denying waits out a daemon handoff, so the title
            // offers neither while paused.
            self.panes.pane.pending_control_transfer.is_some() && !self.handoff_paused(),
            Some(&self.panes.pane.search),
        );
        // A queued access prompt is announced, never shown with focus
        // (ADR 0014): the person answers it when they get to it.
        let title = match crate::frontend::waiting_title(self.access.waiting()) {
            Some(waiting) => format!("{title} — {waiting}"),
            None => title,
        };
        // The compositor title carries the upgrade banner too, so it stays
        // truthful about whether typing reaches the terminal.
        Some(match self.handoff.banner_text() {
            Some(banner) => format!("{title} — {banner}"),
            None => title,
        })
    }

    fn refresh_search_overlay(&mut self, queue_handle: &QueueHandle<Self>) {
        self.update_window_title();
        for buffer in &mut self.surface.buffers {
            buffer.stale.mark_full();
        }
        if self.surface.configured
            && let Err(error) = self.schedule_draw(queue_handle)
        {
            self.scheduling.fail(error);
        }
    }

    fn reveal_pending_search_match(&mut self) {
        let Some(item) = self.panes.pane.search.pending_reveal.clone() else {
            return;
        };
        let Some(snapshot) = self.panes.pane.snapshot.as_ref() else {
            return;
        };
        if snapshot.active_screen != ActiveScreen::Normal {
            return;
        }
        if !self
            .panes
            .pane
            .search
            .results_match(snapshot.history_generation)
        {
            return;
        }
        if self
            .panes
            .pane
            .scrollback_viewport
            .reveal_row(item.row_id, snapshot)
        {
            let endpoint = |column| SelectionEndpoint {
                active_screen: ActiveScreen::Normal,
                history_generation: snapshot.history_generation,
                row_id: item.row_id,
                column,
            };
            self.panes.pane.selection = Some(Selection {
                anchor: endpoint(item.start_column),
                end: endpoint(item.end_column.saturating_sub(1)),
            });
            self.panes.pane.search.pending_reveal = None;
            self.panes.pane.viewport_dirty = true;
            self.presentation.full_redraw = true;
        } else if !self.panes.pane.history_page_pending
            // A match newer than the cache was found after this Window's
            // latest update; it arrives with the next ones, which retry this.
            && snapshot
                .scrollback_rows
                .first()
                .and_then(|row| row.row_id)
                .is_none_or(|top| item.row_id < top)
        {
            // Nothing older can arrive: the daemon has no older history, or
            // trimmed the match's row. Give up instead of walking forever.
            let unreachable = snapshot.omitted_oldest_scrollback_rows == 0
                || snapshot
                    .oldest_available_scrollback_row_id
                    .is_some_and(|oldest| item.row_id < oldest);
            if unreachable {
                self.panes.pane.search.pending_reveal = None;
                return;
            }
            let before_row_id = snapshot
                .scrollback_rows
                .first()
                .and_then(|row| row.row_id)
                .or_else(|| {
                    snapshot
                        .newest_available_scrollback_row_id
                        .and_then(|id| id.checked_add(1))
                });
            let command = before_row_id.map(|before_row_id| WindowCommand::FetchScrollback {
                splint_id: snapshot.splint_id,
                incarnation: snapshot.incarnation,
                terminal_revision: snapshot.revision,
                history_generation: snapshot.history_generation,
                before_row_id,
            });
            let top = snapshot.scrollback_rows.first().and_then(|row| row.row_id);
            let sent = match (command, self.panes.pane.commands.as_ref()) {
                // Retried with the next update while the match is unrevealed.
                (Some(command), Some(commands)) => match try_advisory_command(commands, command) {
                    Ok(sent) => sent,
                    Err(error) => {
                        self.scheduling.fail(error);
                        false
                    }
                },
                _ => false,
            };
            if !sent {
                return;
            }
            self.panes.pane.history_page_pending = true;
            self.panes.pane.history_page_top = top;
            // A selection pins its rows in the bounded history cache, which
            // then refuses older pages and the walk would never arrive. The
            // match replaces the selection once revealed, so drop it now.
            if self.panes.pane.selection.is_some() {
                self.clear_selection();
            }
            // Walk back from the oldest loaded row with the view there,
            // detached: a live view drops history that lags new output, so
            // each new line would restart the walk and it would never arrive.
            if let Some(snapshot) = self.panes.pane.snapshot.as_ref() {
                self.panes
                    .pane
                    .scrollback_viewport
                    .scroll_up(snapshot.scrollback_rows.len(), snapshot);
            }
            self.panes.pane.viewport_dirty = true;
            self.presentation.full_redraw = true;
        }
    }

    fn submit_search(&mut self, cursor: Option<String>) {
        let Some(snapshot) = self.panes.pane.snapshot.as_ref() else {
            return;
        };
        let query = self.panes.pane.search.query.clone();
        let Some(commands) = self.panes.pane.commands.as_ref() else {
            return;
        };
        if query.is_empty() {
            return;
        }
        let continuing = cursor.is_some();
        // A cursor continues the page it came from. Sending that page's
        // generation and revision lets the daemon say when history has
        // changed under it, instead of reading the cursor against other rows.
        let (terminal_revision, history_generation) = if continuing {
            (
                self.panes.pane.search.page_revision,
                self.panes.pane.search.page_generation,
            )
        } else {
            (snapshot.revision, snapshot.history_generation)
        };
        let sent = try_advisory_command(
            commands,
            WindowCommand::Search {
                terminal_revision,
                history_generation,
                query: query.clone(),
                case_sensitive: false,
                cursor,
            },
        );
        let search = &mut self.panes.pane.search;
        match sent {
            // Counted only when sent: nothing else answers it.
            Ok(true) => {
                search.in_flight = search.in_flight.saturating_add(1);
                search.submitted_query = query;
                search.searching = continuing && search.matches.is_empty();
            }
            // Busy: Enter or Ctrl+N asks again.
            Ok(false) => search.searching = false,
            Err(error) => self.scheduling.fail(error),
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "trusted local shortcuts and search editing share one ordered keyboard boundary"
    )]
    fn handle_key(&mut self, event: &KeyEvent, queue_handle: &QueueHandle<Self>) {
        if self.modal.dojo_prompt.is_some() {
            let mut execute = false;
            let mut close = false;
            let mut changed = false;
            if let Some(prompt) = self.modal.dojo_prompt.as_mut() {
                match prompt {
                    DojoPromptUi::Rename(rename) => match event.keysym {
                        Keysym::Return | Keysym::KP_Enter => execute = true,
                        Keysym::Escape => close = true,
                        Keysym::BackSpace => changed = rename.backspace(),
                        _ if !self.input.modifiers.ctrl && !self.input.modifiers.alt => {
                            if let Some(text) = Self::editable_field_text(event.utf8.as_deref()) {
                                changed = rename.append_text(text);
                            }
                        }
                        _ => {}
                    },
                    DojoPromptUi::Terminate(confirmation) => match event.keysym {
                        Keysym::Left | Keysym::Right | Keysym::Up | Keysym::Down | Keysym::Tab => {
                            changed = confirmation.move_selection();
                        }
                        Keysym::Return | Keysym::KP_Enter => execute = true,
                        Keysym::Escape => close = true,
                        _ => {}
                    },
                    DojoPromptUi::RenameLair(rename) => match event.keysym {
                        Keysym::Return | Keysym::KP_Enter => execute = true,
                        Keysym::Escape => close = true,
                        Keysym::BackSpace => changed = rename.backspace(),
                        _ if !self.input.modifiers.ctrl && !self.input.modifiers.alt => {
                            if let Some(text) = Self::editable_field_text(event.utf8.as_deref()) {
                                changed = rename.append_text(text);
                            }
                        }
                        _ => {}
                    },
                    DojoPromptUi::PreviewLair(_) => match event.keysym {
                        Keysym::Return | Keysym::KP_Enter | Keysym::Escape => close = true,
                        _ => {}
                    },
                    DojoPromptUi::RestoreLair(confirmation) => match event.keysym {
                        Keysym::Left | Keysym::Right | Keysym::Up | Keysym::Down | Keysym::Tab => {
                            changed = confirmation.move_selection();
                        }
                        Keysym::Return | Keysym::KP_Enter => execute = true,
                        Keysym::Escape => close = true,
                        _ => {}
                    },
                    DojoPromptUi::TerminateLair(confirmation) => match event.keysym {
                        Keysym::Left | Keysym::Right | Keysym::Up | Keysym::Down | Keysym::Tab => {
                            changed = confirmation.move_selection();
                        }
                        Keysym::Return | Keysym::KP_Enter => execute = true,
                        Keysym::Escape => close = true,
                        _ => {}
                    },
                    DojoPromptUi::RevokeAccess(confirmation) => match event.keysym {
                        Keysym::Left | Keysym::Right | Keysym::Up | Keysym::Down | Keysym::Tab => {
                            changed = confirmation.move_selection();
                        }
                        Keysym::Return | Keysym::KP_Enter if confirmation.is_notice() => {
                            close = true;
                        }
                        Keysym::Return | Keysym::KP_Enter => execute = true,
                        Keysym::Escape => close = true,
                        _ => {}
                    },
                }
            }
            if execute {
                self.execute_dojo_prompt();
            } else if close {
                self.close_dojo_prompt();
            } else if changed {
                self.refresh_dojo_prompt();
            }
            return;
        }
        if self.modal.tab_context_menu.is_some() {
            let mut execute = None;
            let mut close = false;
            let mut changed = false;
            if let Some(menu) = self.modal.tab_context_menu.as_mut() {
                match event.keysym {
                    Keysym::Up => changed = menu.move_selection(-1),
                    Keysym::Down => changed = menu.move_selection(1),
                    Keysym::Return | Keysym::KP_Enter => execute = Some(menu.selected_action()),
                    Keysym::Escape => close = true,
                    _ => {}
                }
            }
            if let Some(action) = execute {
                self.execute_tab_context_menu(action);
            } else if close {
                self.close_tab_context_menu();
            } else if changed {
                self.refresh_tab_context_menu();
            }
            return;
        }
        if self.modal.binding_help.is_some() {
            let mut close = false;
            let mut changed = false;
            if let Some(help) = self.modal.binding_help.as_mut() {
                match event.keysym {
                    Keysym::Up => changed = help.move_selection(-1),
                    Keysym::Down => changed = help.move_selection(1),
                    Keysym::Home => changed = help.select_first(),
                    Keysym::End => changed = help.select_last(),
                    Keysym::Page_Up => {
                        changed = help.move_selection(
                            -isize::try_from(BINDING_HELP_PAGE_ITEMS).unwrap_or(isize::MAX),
                        );
                    }
                    Keysym::Page_Down => {
                        changed = help.move_selection(
                            isize::try_from(BINDING_HELP_PAGE_ITEMS).unwrap_or(isize::MAX),
                        );
                    }
                    Keysym::u | Keysym::U
                        if self.input.modifiers.ctrl
                            && !self.input.modifiers.alt
                            && !self.input.modifiers.logo =>
                    {
                        changed = help.clear_query();
                    }
                    Keysym::Escape => {
                        if help.query().is_empty() {
                            close = true;
                        } else {
                            changed = help.clear_query();
                        }
                    }
                    _ => {}
                }
            }
            if close {
                self.close_command_palette();
            } else if changed {
                self.refresh_command_palette();
            }
            return;
        }
        if self.modal.agent_access.is_some() {
            self.handle_agent_access_key(event.keysym);
            return;
        }
        if self.modal.command_palette.is_some() {
            let mut execute = None;
            let mut close = false;
            let mut changed = false;
            if let Some(palette) = self.modal.command_palette.as_mut() {
                match event.keysym {
                    Keysym::Up => changed = palette.move_selection(-1),
                    Keysym::Down => changed = palette.move_selection(1),
                    Keysym::Home => changed = palette.select_first(),
                    Keysym::End => changed = palette.select_last(),
                    Keysym::Return | Keysym::KP_Enter => {
                        execute = palette.selected_enabled_command();
                    }
                    Keysym::Escape => close = true,
                    Keysym::BackSpace => changed = palette.backspace(),
                    _ if !self.input.modifiers.ctrl
                        && !self.input.modifiers.alt
                        && !self.input.modifiers.logo =>
                    {
                        if let Some(text) = Self::editable_field_text(event.utf8.as_deref()) {
                            changed = palette.append_text(text);
                        }
                    }
                    _ => {}
                }
            }
            if let Some(command) = execute {
                self.execute_command_palette(command, queue_handle);
            } else if close {
                self.close_command_palette();
            } else if changed {
                self.refresh_command_palette();
            }
            return;
        }
        if self.modal.session_picker.is_some() {
            match event.keysym {
                Keysym::Up => self.move_session_picker(-1),
                Keysym::Down => self.move_session_picker(1),
                Keysym::k | Keysym::K
                    if self
                        .modal
                        .session_picker
                        .as_ref()
                        .is_some_and(|picker| !picker.search_active()) =>
                {
                    self.move_session_picker(-1);
                }
                Keysym::j | Keysym::J
                    if self
                        .modal
                        .session_picker
                        .as_ref()
                        .is_some_and(|picker| !picker.search_active()) =>
                {
                    self.move_session_picker(1);
                }
                Keysym::Page_Up => self.move_session_picker(-7),
                Keysym::Page_Down => self.move_session_picker(7),
                Keysym::Home => {
                    if let Some(picker) = self.modal.session_picker.as_mut() {
                        picker.select_first();
                    }
                    if let Err(error) = self.refresh_session_picker() {
                        self.scheduling.fail(error);
                    }
                }
                Keysym::End => {
                    if let Some(picker) = self.modal.session_picker.as_mut() {
                        picker.select_last();
                    }
                    if let Err(error) = self.refresh_session_picker() {
                        self.scheduling.fail(error);
                    }
                }
                Keysym::Return | Keysym::KP_Enter => {
                    if let Some(decision) = self
                        .modal
                        .session_picker
                        .as_ref()
                        .and_then(SessionPickerUi::selected_decision)
                    {
                        self.decide_session_picker(decision);
                    }
                }
                Keysym::n | Keysym::N
                    if self.input.modifiers.ctrl
                        && self
                            .modal
                            .session_picker
                            .as_ref()
                            .is_some_and(SessionPickerUi::new_enabled) =>
                {
                    self.decide_session_picker(SessionPickerDecision::New);
                }
                Keysym::r | Keysym::R
                    if self.modal.session_picker_retry_command.is_some()
                        && self.modal.remote_profiles.is_none()
                        && self
                            .modal
                            .session_picker
                            .as_ref()
                            .is_some_and(|picker| !picker.search_active()) =>
                {
                    let command = self.modal.session_picker_retry_command.clone();
                    let selector_kind = self.modal.selector_kind;
                    self.close_inline_session_picker();
                    if let Err(error) = self.show_embedded_session_picker(
                        session_picker_status_catalog("Loading navigation…"),
                        selector_kind,
                    ) {
                        self.scheduling.fail(error);
                        return;
                    }
                    self.modal.session_picker_requested = true;
                    if command.is_none_or(|command| self.send_topology_command(command).is_err()) {
                        self.modal.session_picker_requested = false;
                    }
                }
                Keysym::slash | Keysym::f | Keysym::F
                    if matches!(event.keysym, Keysym::slash) || self.input.modifiers.ctrl =>
                {
                    let changed = self
                        .modal
                        .session_picker
                        .as_mut()
                        .is_some_and(SessionPickerUi::begin_search);
                    if changed {
                        let _ = self.refresh_session_picker();
                    }
                }
                Keysym::BackSpace => {
                    let changed = self
                        .modal
                        .session_picker
                        .as_mut()
                        .is_some_and(SessionPickerUi::backspace_search);
                    if changed {
                        let _ = self.refresh_session_picker();
                    }
                }
                Keysym::Escape => {
                    let cleared = self
                        .modal
                        .session_picker
                        .as_mut()
                        .is_some_and(SessionPickerUi::clear_search);
                    if cleared {
                        let _ = self.refresh_session_picker();
                    } else {
                        self.cancel_session_picker();
                    }
                }
                _ if !self.input.modifiers.ctrl
                    && !self.input.modifiers.alt
                    && self
                        .modal
                        .session_picker
                        .as_ref()
                        .is_some_and(SessionPickerUi::search_active) =>
                {
                    if let Some(text) = Self::editable_field_text(event.utf8.as_deref()) {
                        let changed = self
                            .modal
                            .session_picker
                            .as_mut()
                            .is_some_and(|picker| picker.append_search(text));
                        if changed {
                            let _ = self.refresh_session_picker();
                        }
                    }
                }
                _ => {}
            }
            return;
        }
        if let Some(consent) = self.modal.trusted_consent.as_mut() {
            // Repeats never reach here (`repeat_key`), so every key seen is
            // a fresh press.
            let key = trusted_dialog_press(event.keysym, self.input.modifiers, consent.shortcut());
            let input = consent.key_input(key, false, Instant::now());
            self.trusted_dialog_input(input, queue_handle);
            return;
        }
        if self.explorer.focused() {
            let mut changed = false;
            match event.keysym {
                key if explorer_context_menu_key(key, self.input.modifiers) => {
                    let agents = self.explorer.agent_selected().is_some();
                    if let Some(node) = self.explorer.selected()
                        && let Some(rect) = self
                            .presentation
                            .explorer_layout
                            .as_ref()
                            .and_then(|layout| layout.row_rect(node, agents))
                    {
                        let anchor = (f64::from(rect.x), f64::from(rect.y));
                        if let Err(error) = self.show_explorer_context_menu(node, anchor) {
                            eprintln!("splinterm Explorer menu is unavailable: {error:#}");
                        }
                        changed = true;
                    }
                }
                Keysym::Up => changed = self.explorer.move_selection(-1),
                Keysym::Down => changed = self.explorer.move_selection(1),
                Keysym::k | Keysym::K if !self.explorer.search_active() => {
                    changed = self.explorer.move_selection(-1);
                }
                Keysym::j | Keysym::J if !self.explorer.search_active() => {
                    changed = self.explorer.move_selection(1);
                }
                Keysym::Page_Up => {
                    let page = self
                        .presentation
                        .explorer_layout
                        .as_ref()
                        .map_or(1, |layout| layout.visible_capacity.max(1));
                    changed = self
                        .explorer
                        .move_selection(-isize::try_from(page).unwrap_or(isize::MAX));
                }
                Keysym::Page_Down => {
                    let page = self
                        .presentation
                        .explorer_layout
                        .as_ref()
                        .map_or(1, |layout| layout.visible_capacity.max(1));
                    changed = self
                        .explorer
                        .move_selection(isize::try_from(page).unwrap_or(isize::MAX));
                }
                Keysym::Home => changed = self.explorer.select_edge(false),
                Keysym::End => changed = self.explorer.select_edge(true),
                Keysym::Left => changed = self.explorer.move_left(),
                Keysym::Right => changed = self.explorer.move_right(),
                Keysym::space => changed = self.explorer.toggle_selected(),
                Keysym::Return | Keysym::KP_Enter => {
                    if let Some(decision) = self.explorer.decision() {
                        changed |= self.execute_lair_explorer_decision(decision);
                    }
                }
                Keysym::slash | Keysym::f | Keysym::F
                    if matches!(event.keysym, Keysym::slash) || self.input.modifiers.ctrl =>
                {
                    changed = self.explorer.begin_search();
                }
                key if explorer_filter_key(
                    key,
                    self.input.modifiers,
                    self.explorer.search_active(),
                ) =>
                {
                    changed = self.explorer.set_filter(self.explorer.filter().next());
                }
                Keysym::r | Keysym::R if !self.explorer.search_active() => {
                    if self.explorer.retryable() {
                        let _ = self.refresh_lair_explorer();
                        changed = true;
                    } else {
                        changed = self.explorer.reveal_current();
                    }
                }
                Keysym::BackSpace => changed = self.explorer.backspace_search(),
                // The rail shows no search: Escape always returns to the
                // terminal there.
                Keysym::Escape if self.explorer.query().is_empty() || self.explorer.collapsed() => {
                    self.set_explorer_focus(false, queue_handle);
                    changed = true;
                }
                Keysym::Escape => changed = self.explorer.escape(),
                _ if self.explorer.search_active()
                    && !self.input.modifiers.ctrl
                    && !self.input.modifiers.alt
                    && !self.input.modifiers.logo =>
                {
                    if let Some(text) = Self::editable_field_text(event.utf8.as_deref()) {
                        changed = self.explorer.append_search(text);
                    }
                }
                _ => {}
            }
            if changed {
                self.presentation.full_redraw = true;
                if let Err(error) = self.schedule_draw(queue_handle) {
                    self.scheduling.fail(error);
                }
            }
            return;
        }
        if self.panes.pane.search.input.is_some() {
            if self.input.modifiers.ctrl && matches!(event.keysym, Keysym::n | Keysym::N) {
                if self.panes.pane.search.selected + 1 < self.panes.pane.search.matches.len() {
                    self.panes.pane.search.selected += 1;
                    self.panes.pane.search.pending_reveal = self
                        .panes
                        .pane
                        .search
                        .matches
                        .get(self.panes.pane.search.selected)
                        .cloned();
                    self.reveal_pending_search_match();
                } else if let Some(cursor) = self.panes.pane.search.next_cursor.clone() {
                    self.submit_search(Some(cursor));
                }
                self.refresh_search_overlay(queue_handle);
                return;
            }
            if self.input.modifiers.ctrl && matches!(event.keysym, Keysym::p | Keysym::P) {
                self.panes.pane.search.selected = self.panes.pane.search.selected.saturating_sub(1);
                self.panes.pane.search.pending_reveal = self
                    .panes
                    .pane
                    .search
                    .matches
                    .get(self.panes.pane.search.selected)
                    .cloned();
                self.reveal_pending_search_match();
                self.refresh_search_overlay(queue_handle);
                return;
            }
            match event.keysym {
                Keysym::Escape => {
                    self.input.input_generation = self.input.input_generation.saturating_add(1);
                    self.panes.pane.search.close();
                    self.panes.pane.selection = None;
                }
                Keysym::Return | Keysym::KP_Enter => {
                    self.panes.pane.search.query = self
                        .panes
                        .pane
                        .search
                        .input
                        .as_ref()
                        .map_or_else(String::new, |editor| editor.text().to_owned());
                    self.submit_search(None);
                }
                Keysym::BackSpace => {
                    if let Some(input) = self.panes.pane.search.input.as_mut() {
                        input.backspace();
                    }
                }
                _ if !self.input.modifiers.ctrl && !self.input.modifiers.alt => {
                    if let (Some(input), Some(text)) = (
                        self.panes.pane.search.input.as_mut(),
                        Self::editable_field_text(event.utf8.as_deref()),
                    ) {
                        input.insert(text);
                    }
                }
                _ => {}
            }
            self.refresh_search_overlay(queue_handle);
            return;
        }
        match shortcut_action_for(&self.input.keymap, event, self.input.modifiers) {
            Some(ActionId::RevokeAllAccess)
                if self.tab_state.managed_tabs && self.tab_state.topology_commands.is_some() =>
            {
                // Every duration, through the daemon's count and confirmation.
                self.begin_revoke_all_access();
                return;
            }
            Some(ActionId::RevokeAllAccess) => {
                // A single-pane Window has no topology manager: it ends the
                // grants it shows and every pending request.
                let ids: Vec<_> = self
                    .panes
                    .pane
                    .authority
                    .grants
                    .iter()
                    .map(|(id, _)| *id)
                    .collect();
                for id in ids {
                    self.send_command(WindowCommand::RevokeAccess(id));
                }
                self.send_command(WindowCommand::CancelPendingAccess);
                self.panes.pane.authority.grants.clear();
                if let Some(title) = self.terminal_window_title() {
                    self.surface.window.set_title(title);
                }
                return;
            }
            Some(ActionId::RequestControl) => {
                self.send_command(WindowCommand::RequestControlTransfer);
                return;
            }
            Some(ActionId::SearchScrollback) => {
                self.input.input_generation = self.input.input_generation.saturating_add(1);
                self.panes.pane.search.input = Some(new_search_editor());
                self.panes.pane.search.matches.clear();
                self.panes.pane.search.next_cursor = None;
                self.refresh_search_overlay(queue_handle);
                return;
            }
            Some(ActionId::ForceControl) => {
                if self.input.forced_control_transfer {
                    self.send_command(WindowCommand::ForceControlTransfer);
                }
                return;
            }
            Some(ActionId::AcceptControlTransfer) => {
                if let Some(transfer_id) = self.panes.pane.pending_control_transfer.take() {
                    self.send_command(WindowCommand::DecideControlTransfer {
                        transfer_id,
                        decision: ControlTransferDecision::Accept,
                    });
                }
                return;
            }
            Some(ActionId::DenyControlTransfer) => {
                if let Some(transfer_id) = self.panes.pane.pending_control_transfer.take() {
                    self.send_command(WindowCommand::DecideControlTransfer {
                        transfer_id,
                        decision: ControlTransferDecision::Deny,
                    });
                }
                return;
            }
            Some(ActionId::ReleaseControl) => {
                self.request_active_pane_control_release();
                if let Some(title) = self.terminal_window_title() {
                    self.surface.window.set_title(title);
                }
                return;
            }
            _ => {}
        }
        if self.presentation.evidence_close_shortcuts
            && matches!(event.keysym, Keysym::Escape | Keysym::q | Keysym::Q)
        {
            self.scheduling.request_exit(ExitClass::CleanUserClose);
            return;
        }
        let utf8 = if self.input.ime.composing()
            && !self.input.modifiers.ctrl
            && !self.input.modifiers.alt
        {
            None
        } else {
            event.utf8.as_deref()
        };
        if let Some(bytes) = key_input(
            event.keysym,
            utf8,
            self.input.modifiers,
            self.panes.input_modes(),
        ) {
            self.send_command(WindowCommand::Input(bytes));
        }
    }

    fn emit_resize(&mut self) -> Result<()> {
        let layout = self.computed_pane_layout();
        let active_rect = self
            .panes
            .focused_splint()
            .and_then(|splint_id| layout.as_ref().and_then(|layout| layout.rect(splint_id)));
        let content = self.content_rect();
        let (active_width, active_height) = active_rect
            .map_or((content.width, content.height), |rect| {
                (rect.width, rect.height)
            });
        Self::emit_active_pane_resize(
            &mut self.panes.pane,
            active_width,
            active_height,
            self.surface.scale_120,
        )?;
        for pane in &mut self.panes.inactive_panes {
            let Some(splint_id) = pane.snapshot.as_ref().map(|snapshot| snapshot.splint_id) else {
                continue;
            };
            let Some(rect) = layout.as_ref().and_then(|layout| layout.rect(splint_id)) else {
                continue;
            };
            Self::emit_inactive_pane_resize(pane, rect.width, rect.height, self.surface.scale_120)?;
        }
        Ok(())
    }

    fn emit_font_resize(&mut self) -> Result<()> {
        let layout = self.computed_pane_layout();
        let active_rect = self
            .panes
            .focused_splint()
            .and_then(|splint_id| layout.as_ref().and_then(|layout| layout.rect(splint_id)));
        let content = self.content_rect();
        let (active_width, active_height) = active_rect
            .map_or((content.width, content.height), |rect| {
                (rect.width, rect.height)
            });
        let active = self.panes.pane.controller_active;
        Self::emit_pane_resize(
            &mut self.panes.pane,
            active_width,
            active_height,
            self.surface.scale_120,
            active,
        )?;
        for pane in &mut self.panes.inactive_panes {
            let Some(splint_id) = pane.snapshot.as_ref().map(|snapshot| snapshot.splint_id) else {
                continue;
            };
            let Some(rect) = layout.as_ref().and_then(|layout| layout.rect(splint_id)) else {
                continue;
            };
            let active = pane.controller_active;
            Self::emit_pane_resize(
                pane,
                rect.width,
                rect.height,
                self.surface.scale_120,
                active,
            )?;
        }
        Ok(())
    }

    fn emit_active_pane_resize(
        pane: &mut PaneView,
        logical_width: u32,
        logical_height: u32,
        scale_120: u32,
    ) -> Result<()> {
        Self::emit_pane_resize(pane, logical_width, logical_height, scale_120, true)
    }

    fn emit_inactive_pane_resize(
        pane: &mut PaneView,
        logical_width: u32,
        logical_height: u32,
        scale_120: u32,
    ) -> Result<()> {
        // Every visible pane must resize immediately. Deferring an uncontrolled
        // pane leaves its reflowed history stale until later input claims control.
        Self::emit_pane_resize(pane, logical_width, logical_height, scale_120, true)
    }

    fn emit_pane_resize(
        pane: &mut PaneView,
        logical_width: u32,
        logical_height: u32,
        scale_120: u32,
        activate: bool,
    ) -> Result<()> {
        let Some(frame) = &pane.snapshot_frame else {
            return Ok(());
        };
        if pane.commands.is_none() {
            return Ok(());
        }
        let (resize, column_capped, row_capped) = match frame.terminal_size_with_limit_status(
            logical_width,
            logical_height,
            scale_120,
            pane.terminal_grid_limits.maximum_columns,
            pane.terminal_grid_limits.maximum_rows,
        ) {
            Ok(status) => (status.size, status.column_capped, status.row_capped),
            Err(error) if error.to_string().contains("SurfaceTooSmall") => return Ok(()),
            Err(error) => return Err(error),
        };
        if (column_capped || row_capped) && !pane.grid_cap_diagnostic_emitted {
            pane.grid_cap_diagnostic_emitted = true;
            if let Some(diagnostics) = diagnostics() {
                diagnostics.emit(DiagnosticLevel::Warn, DiagnosticEventCode::GridCapped, None);
            }
            eprintln!(
                "splinterm terminal grid capped at {}x{} for a {}x{} logical pane",
                pane.terminal_grid_limits.maximum_columns,
                pane.terminal_grid_limits.maximum_rows,
                logical_width,
                logical_height
            );
        }
        if !resize_changed(pane.last_resize, resize) {
            pane.pending_resize = None;
            return Ok(());
        }
        let resize_command = if activate {
            WindowCommand::Resize {
                columns: resize.0,
                rows: resize.1,
                pixel_width: resize.2,
                pixel_height: resize.3,
            }
        } else {
            WindowCommand::PrepareResize {
                columns: resize.0,
                rows: resize.1,
                pixel_width: resize.2,
                pixel_height: resize.3,
            }
        };
        pane.pending_resize = Some(PendingPaneResize {
            size: resize,
            command: resize_command,
        });
        Self::retry_pending_pane_resize(pane)
    }

    fn retry_pending_pane_resize(pane: &mut PaneView) -> Result<()> {
        let Some(pending) = pane.pending_resize.take() else {
            return Ok(());
        };
        let Some(commands) = &pane.commands else {
            return Ok(());
        };
        match commands.try_send(pending.command) {
            Ok(()) => pane.last_resize = Some(pending.size),
            Err(TrySendError::Full(command)) => {
                // Preserve the exact newest resize across ordinary queue
                // backpressure. The event loop retries deferred pane commands
                // after pending terminal input, without changing authority.
                pane.pending_resize = Some(PendingPaneResize {
                    size: pending.size,
                    command,
                });
            }
            Err(TrySendError::Closed(_)) => {
                anyhow::bail!("Wayland command receiver disconnected");
            }
        }
        Ok(())
    }

    fn begin_pending_remote_split(
        &mut self,
        target: SplintId,
        axis: Axis,
    ) -> Result<Option<SplintId>> {
        if !remote_split_can_begin(&self.panes.pending_remote_splits) {
            return Ok(None);
        }
        let (target_snapshot, target_grid_limits) = std::iter::once(&self.panes.pane)
            .chain(self.panes.inactive_panes.iter())
            .find_map(|pane| {
                pane.snapshot
                    .as_ref()
                    .filter(|snapshot| snapshot.splint_id == target)
                    .map(|snapshot| (snapshot, pane.terminal_grid_limits))
            })
            .context("pending split target has no frontend pane")?;
        let pending_id = SplintId::new();
        let mut pending_splint = Splint::shell(PathBuf::from("/"));
        pending_splint.id = pending_id;
        "Opening remote pane…".clone_into(&mut pending_splint.title);
        let mut layout = self
            .panes
            .layout
            .clone()
            .context("pending split requires a managed layout")?;
        anyhow::ensure!(
            insert_pending_split(&mut layout, target, pending_splint, axis),
            "pending split target is absent from the managed layout"
        );
        let mut snapshot =
            pending_remote_snapshot(pending_id, target_snapshot.columns, target_snapshot.rows);
        apply_theme(&mut snapshot, self.presentation.theme);
        let (update_sender, updates) = tokio::sync::mpsc::channel(1);
        let (commands, command_receiver) = tokio::sync::mpsc::channel(1);
        let options = WindowPaneOptions {
            snapshot,
            terminal_grid_limits: target_grid_limits,
            updates,
            commands,
            authority: AuthorityStatus::default(),
            controlled: false,
            image_sources: ImageContentLeaseSet::default(),
        };
        let mut pane = PaneView::from_inactive_options_with_context(
            options,
            self.surface.scale_120,
            &self.presentation.render_context,
        )?;
        // Placeholder channels never cross the trust boundary and must not emit
        // terminal input, resize, focus, or disconnect events.
        pane.updates = None;
        pane.commands = None;
        drop(update_sender);
        drop(command_receiver);
        self.panes.inactive_panes.push(pane);
        self.panes.layout = Some(layout);
        self.panes.pending_remote_splits.insert(target, pending_id);
        self.presentation.full_redraw = true;
        Ok(Some(pending_id))
    }

    fn rollback_pending_remote_split(&mut self, target: SplintId, pending: SplintId) -> Result<()> {
        anyhow::ensure!(
            self.panes.pending_remote_splits.get(&target) == Some(&pending),
            "remote split placeholder reservation changed before rollback"
        );
        let layout = self
            .panes
            .layout
            .take()
            .context("remote split placeholder has no managed layout")?;
        let (layout, removed) = remove_pending_split(layout, pending);
        anyhow::ensure!(
            removed,
            "remote split placeholder is absent from its layout"
        );
        let layout = layout.context("remote split placeholder consumed the complete layout")?;
        self.panes.inactive_panes.retain(|pane| {
            pane.snapshot
                .as_ref()
                .is_none_or(|snapshot| snapshot.splint_id != pending)
        });
        self.panes.pending_remote_splits.remove(&target);
        self.presentation.frame_titles.remove(&pending);
        self.panes.layout = Some(layout);
        let _ = self.focus_splint(target);
        anyhow::ensure!(
            self.panes.focused_splint() == Some(target),
            "remote split rollback could not restore target focus"
        );
        self.presentation.full_redraw = true;
        Ok(())
    }

    fn apply_topology_replacement(
        &mut self,
        layout: LayoutNode,
        added: Vec<WindowPaneOptions>,
        removed: Vec<SplintId>,
        focused: Option<SplintId>,
    ) -> Result<()> {
        anyhow::ensure!(
            !self.modal.inline_picker_open(),
            "topology replacement arrived while the Dojo picker was open"
        );
        let removed = removed.into_iter().collect::<HashSet<_>>();
        let mut prepared = Vec::with_capacity(added.len());
        for mut pane in added {
            apply_theme(&mut pane.snapshot, self.presentation.theme);
            prepared.push(PaneView::from_inactive_options_with_context(
                pane,
                self.surface.scale_120,
                &self.presentation.render_context,
            )?);
        }
        let prepared_ids = prepared
            .iter()
            .filter_map(|pane| pane.snapshot.as_ref().map(|snapshot| snapshot.splint_id));
        let mut identities = std::iter::once(&self.panes.pane)
            .chain(self.panes.inactive_panes.iter())
            .filter_map(|pane| pane.snapshot.as_ref().map(|snapshot| snapshot.splint_id))
            .filter(|splint_id| !removed.contains(splint_id))
            .chain(prepared_ids)
            .collect::<HashSet<_>>();
        anyhow::ensure!(
            identities.len() == layout.splint_count()
                && identities
                    .iter()
                    .all(|splint_id| layout.find_splint(*splint_id).is_some()),
            "topology update pane identities do not match its layout"
        );
        let next_focus = focused
            .or_else(|| {
                self.panes
                    .focused_splint()
                    .filter(|splint_id| identities.contains(splint_id))
            })
            .unwrap_or_else(|| layout.first_splint_id());
        anyhow::ensure!(
            identities.remove(&next_focus),
            "topology update focus is absent"
        );

        self.panes
            .pending_exited_splints
            .retain(|splint_id| !removed.contains(splint_id));
        let active_replaced = tabs::replace_retired_panes(
            &mut self.panes.pane,
            &mut self.panes.inactive_panes,
            &mut prepared,
            &removed,
            |pane| pane.snapshot.as_ref().map(|snapshot| snapshot.splint_id),
        );
        self.panes.inactive_panes.extend(prepared);
        let focus_changed = self.focus_splint(next_focus);
        if active_replaced && !focus_changed {
            self.focused_pane_changed();
        }
        anyhow::ensure!(
            self.panes.focused_splint() == Some(next_focus),
            "topology update focus could not be applied"
        );
        self.panes.inactive_panes.retain(|pane| {
            pane.snapshot
                .as_ref()
                .is_none_or(|snapshot| layout.find_splint(snapshot.splint_id).is_some())
        });
        self.panes
            .pending_remote_splits
            .retain(|_, pending| layout.find_splint(*pending).is_some());
        self.panes.layout = Some(layout);
        self.presentation.full_redraw = true;
        Ok(())
    }

    fn apply_targeted_topology_replacement(
        &mut self,
        dojo_id: DojoId,
        layout: LayoutNode,
        added: Vec<WindowPaneOptions>,
        removed: Vec<SplintId>,
        focused: Option<SplintId>,
    ) -> Result<bool> {
        if dojo_id == self.tab_state.active_dojo_id() {
            self.apply_topology_replacement(layout, added, removed, focused)?;
            return Ok(true);
        }
        let theme = self.presentation.theme;
        let scale_120 = self.surface.scale_120;
        self.tab_state
            .tabs
            .get_mut(dojo_id)
            .and_then(|tab| tab.value.as_mut())
            .context("updated Dojo tab has no hidden frontend")?
            .apply_topology(
                layout,
                added,
                removed,
                focused,
                theme,
                scale_120,
                &self.presentation.render_context,
            )?;
        Ok(false)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "bounded topology draining, tab reconciliation, and deferred picker updates remain one transaction"
    )]
    fn apply_topology_updates(
        &mut self,
        queue_handle: &QueueHandle<Self>,
    ) -> Result<(bool, Option<ThemeUpdate>)> {
        let mut pending = VecDeque::new();
        if let Some(updates) = &mut self.tab_state.topology_updates {
            let drained = drain_receiver(updates, &self.platform.update_waker);
            pending = drained.items.into();
            if drained.disconnected {
                self.tab_state.topology_updates = None;
            }
        }
        if !self.modal.inline_picker_open() && !self.tab_state.deferred_topology_updates.is_empty()
        {
            let mut deferred = VecDeque::from(std::mem::take(
                &mut self.tab_state.deferred_topology_updates,
            ));
            deferred.append(&mut pending);
            pending = deferred;
        }
        let mut changed = false;
        let mut next_theme = None;
        while let Some(update) = pending.pop_front() {
            // A reconnection's resnapshot must land before input resumes, and
            // a picker opened before it shows stale state: it closes the
            // picker and applies every deferred update in order first.
            let reconnected = matches!(&update, WindowTopologyUpdate::Reconnected { .. });
            let defer_for_picker = self.modal.inline_picker_open()
                && (reconnected
                    || matches!(
                        &update,
                        WindowTopologyUpdate::Apply { .. }
                            | WindowTopologyUpdate::OpenTab { .. }
                            | WindowTopologyUpdate::ActivateTab { .. }
                            | WindowTopologyUpdate::ActivateSplint { .. }
                            | WindowTopologyUpdate::RemoveTab { .. }
                            | WindowTopologyUpdate::UpdateIdentity(_)
                    ));
            if defer_for_picker {
                if !reconnected
                    && self.tab_state.deferred_topology_updates.len()
                        < MAX_DEFERRED_TOPOLOGY_UPDATES
                {
                    self.tab_state.deferred_topology_updates.push(update);
                    continue;
                }
                self.close_inline_session_picker();
                eprintln!("splinterm Dojo picker: cancelled to apply queued topology updates");
                let mut replay = VecDeque::from(std::mem::take(
                    &mut self.tab_state.deferred_topology_updates,
                ));
                replay.push_back(update);
                replay.append(&mut pending);
                pending = replay;
                changed = true;
                continue;
            }
            let refresh_explorer = self.explorer.visible()
                && matches!(
                    &update,
                    WindowTopologyUpdate::Apply { .. }
                        | WindowTopologyUpdate::OpenTab { .. }
                        | WindowTopologyUpdate::ActivateTab { .. }
                        | WindowTopologyUpdate::ActivateSplint { .. }
                        | WindowTopologyUpdate::RemoveTab { .. }
                        | WindowTopologyUpdate::UpdateIdentity(_)
                        | WindowTopologyUpdate::LairExplorerInvalidated
                );
            match update {
                WindowTopologyUpdate::Apply {
                    topology_revision,
                    dojo_id,
                    layout,
                    added,
                    removed,
                    focused,
                } => {
                    if self.modal.inline_picker_open() {
                        let update = WindowTopologyUpdate::Apply {
                            topology_revision,
                            dojo_id,
                            layout,
                            added,
                            removed,
                            focused,
                        };
                        if self.tab_state.deferred_topology_updates.len()
                            < MAX_DEFERRED_TOPOLOGY_UPDATES
                        {
                            self.tab_state.deferred_topology_updates.push(update);
                        } else {
                            self.close_inline_session_picker();
                            eprintln!(
                                "splinterm Dojo picker: cancelled to apply queued topology updates"
                            );
                            let mut replay = VecDeque::from(std::mem::take(
                                &mut self.tab_state.deferred_topology_updates,
                            ));
                            replay.push_back(update);
                            replay.append(&mut pending);
                            pending = replay;
                            changed = true;
                        }
                    } else {
                        changed |= self.apply_targeted_topology_replacement(
                            dojo_id, layout, added, removed, focused,
                        )?;
                        if let Some(diagnostics) = diagnostics() {
                            diagnostics
                                .update_topology(topology_revision, self.tab_state.tabs.len());
                        }
                    }
                }
                WindowTopologyUpdate::OpenTab {
                    identity,
                    layout,
                    panes,
                    focused,
                    acknowledged,
                    explorer_target,
                } => {
                    let topology_revision = identity.topology_revision;
                    let dojo_id = identity.dojo_id;
                    let lair_id = identity.lair_id;
                    let view = match DojoTabView::from_open(
                        identity,
                        layout,
                        panes,
                        focused,
                        self.presentation.theme,
                        self.surface.scale_120,
                        &self.presentation.render_context,
                    ) {
                        Ok(view) => view,
                        Err(error) => {
                            changed |= self.explorer.fail_activation(explorer_target);
                            self.tab_state.session_switch_pending =
                                self.explorer.activation_pending();
                            let message = format!("{error:#}");
                            let _ = acknowledged.send(Err(message));
                            eprintln!("splinterm Dojo tab failed to open");
                            continue;
                        }
                    };
                    let previous_id = self.tab_state.active_dojo_id();
                    self.tab_state
                        .tabs
                        .open_or_activate(DojoTab::new(lair_id, dojo_id, Some(view)))
                        .map_err(anyhow::Error::from)?;
                    anyhow::ensure!(
                        self.tab_state.tabs.activate(previous_id),
                        "active Dojo tab disappeared while opening another"
                    );
                    self.tab_state.session_switch_pending = false;
                    self.modal.session_picker_retry_command = None;
                    changed |= self.activate_tab(dojo_id)?;
                    if explorer_target
                        .is_some_and(|target| self.explorer.finish_activation(target, true))
                    {
                        self.set_explorer_focus(false, queue_handle);
                    }
                    if let Some(diagnostics) = diagnostics() {
                        diagnostics.update_topology(topology_revision, self.tab_state.tabs.len());
                    }
                    let _ = acknowledged.send(Ok(()));
                }
                WindowTopologyUpdate::ActivateTab {
                    dojo_id,
                    explorer_target,
                } => {
                    self.tab_state.session_switch_pending = false;
                    self.modal.session_picker_retry_command = None;
                    changed |= self.activate_tab(dojo_id)?;
                    if explorer_target
                        .is_some_and(|target| self.explorer.finish_activation(target, true))
                    {
                        self.set_explorer_focus(false, queue_handle);
                    }
                }
                WindowTopologyUpdate::ActivateSplint {
                    dojo_id,
                    splint_id,
                    live_incarnation,
                    explorer_target,
                } => {
                    self.tab_state.session_switch_pending = false;
                    changed |= self.activate_tab(dojo_id)?;
                    if let Some(focus_changed) =
                        self.focus_splint_at_incarnation(splint_id, live_incarnation)
                    {
                        changed |= focus_changed;
                        if explorer_target
                            .is_some_and(|target| self.explorer.finish_activation(target, true))
                        {
                            self.set_explorer_focus(false, queue_handle);
                        }
                    } else {
                        if let Some(target) = explorer_target {
                            self.explorer.finish_activation(target, false);
                        }
                        self.explorer.mark_stale_target(splint_id);
                        let _ = self.refresh_lair_explorer();
                        changed = true;
                    }
                }
                WindowTopologyUpdate::RemoveTab {
                    dojo_id,
                    acknowledged,
                } => {
                    // The topology manager waits for this acknowledgement and
                    // stops without it, so it is sent even when removing the
                    // tab here fails.
                    let removed = self.remove_tab_locally(dojo_id);
                    let _ = acknowledged.send(());
                    changed |= removed?;
                }
                WindowTopologyUpdate::UpdateIdentity(identity) => {
                    if let Some(diagnostics) = diagnostics() {
                        diagnostics
                            .update_topology(identity.topology_revision, self.tab_state.tabs.len());
                    }
                    let dojo_id = identity.dojo_id;
                    if dojo_id == self.tab_state.active_dojo_id() {
                        self.tab_state.active_identity = identity;
                    } else if let Some(view) = self
                        .tab_state
                        .tabs
                        .get_mut(dojo_id)
                        .and_then(|tab| tab.value.as_mut())
                    {
                        view.identity = identity;
                    }
                    self.tab_state.tab_label_cache.clear();
                    self.presentation.full_redraw = true;
                    changed = true;
                }
                WindowTopologyUpdate::TabFailed {
                    dojo_id,
                    message,
                    explorer_target,
                } => {
                    let retryable_picker = self.tab_state.session_switch_pending
                        && self.modal.session_picker_retry_command.is_some();
                    self.close_inline_session_picker();
                    self.modal.session_picker_requested = false;
                    changed |= self.explorer.fail_activation(explorer_target);
                    self.tab_state.session_switch_pending = self.explorer.activation_pending();
                    if retryable_picker {
                        let selector_kind = match self.modal.session_picker_retry_command.as_ref() {
                            Some(WindowTopologyCommand::RequestSelector { kind, .. }) => {
                                Some(*kind)
                            }
                            _ => None,
                        };
                        self.show_embedded_session_picker(
                            session_picker_status_catalog(
                                "Target changed or is unavailable · R to refresh",
                            ),
                            selector_kind,
                        )?;
                        changed = true;
                    }
                    if self.explorer.visible() {
                        self.explorer
                            .set_notice(&format!("Action failed: {message}"));
                        self.presentation.full_redraw = true;
                        changed = true;
                    }
                    eprintln!(
                        "splinterm topology action failed{}: {message}",
                        dojo_id.map_or_else(String::new, |id| format!(" for Dojo {id}"))
                    );
                }
                WindowTopologyUpdate::ShowSessionPicker { catalog } => {
                    if self.modal.session_picker_requested
                        && !self.tab_state.session_switch_pending
                        && !self.modal.session_picker_reconcile_pending
                        && !self.modal.command_palette_reconcile_pending
                    {
                        self.close_inline_session_picker();
                        self.show_embedded_session_picker(catalog, None)?;
                        changed = true;
                    }
                }
                WindowTopologyUpdate::ShowLairExplorer { view } => {
                    self.explorer.set_view(view);
                    self.presentation.full_redraw = true;
                    changed = true;
                }
                WindowTopologyUpdate::LairExplorerInvalidated => {}
                WindowTopologyUpdate::AgentStatus(status) => {
                    if self.apply_window_agent_status(status) {
                        self.presentation.full_redraw = true;
                        changed = true;
                    }
                }
                WindowTopologyUpdate::AgentStatusCleared {
                    splint_id,
                    incarnation,
                } => {
                    if self.apply_agent_status_cleared(splint_id, incarnation) {
                        self.presentation.full_redraw = true;
                        changed = true;
                    }
                }
                WindowTopologyUpdate::Access(update) => {
                    if self.apply_access_update(update) {
                        self.presentation.full_redraw = true;
                        changed = true;
                    }
                }
                WindowTopologyUpdate::LairExplorerFailed(message) => {
                    self.explorer.mark_disconnected();
                    self.explorer
                        .set_notice(&format!("Navigation unavailable · R retry: {message}"));
                    self.presentation.full_redraw = true;
                    changed = true;
                }
                WindowTopologyUpdate::ShowSelector { kind, catalog } => {
                    if self.modal.session_picker_requested
                        && !self.tab_state.session_switch_pending
                        && !self.modal.session_picker_reconcile_pending
                        && !self.modal.command_palette_reconcile_pending
                    {
                        self.close_inline_session_picker();
                        self.show_embedded_session_picker(catalog, Some(kind))?;
                        changed = true;
                    }
                }
                WindowTopologyUpdate::ShowExplorerPrompt {
                    guard,
                    kind,
                    target,
                } => {
                    self.modal.session_picker_requested = false;
                    let prompt = if let Some(dojo_id) = target.dojo_id {
                        let splints = target
                            .targets
                            .iter()
                            .map(|target| (target.splint_id, target.incarnation))
                            .collect::<Vec<_>>();
                        match kind {
                            LairPromptKind::Rename => {
                                DojoPromptUi::rename(dojo_id, target.name, splints.len(), splints)
                            }
                            LairPromptKind::Terminate => DojoPromptUi::terminate(
                                dojo_id,
                                target.name,
                                splints.len(),
                                splints,
                            ),
                            LairPromptKind::Restore => DojoPromptUi::restore_lair(target),
                            LairPromptKind::Preview => DojoPromptUi::preview_lair(target),
                        }
                    } else {
                        match kind {
                            LairPromptKind::Rename => DojoPromptUi::rename_lair(target),
                            LairPromptKind::Terminate => DojoPromptUi::terminate_lair(target),
                            LairPromptKind::Restore => DojoPromptUi::restore_lair(target),
                            LairPromptKind::Preview => DojoPromptUi::preview_lair(target),
                        }
                    };
                    self.show_dojo_prompt(prompt);
                    self.modal.dojo_prompt_explorer_target = Some(guard);
                    changed = true;
                }
                WindowTopologyUpdate::ShowLairPrompt { kind, target } => {
                    self.tab_state.session_switch_pending = false;
                    self.explorer.clear_pending();
                    self.modal.session_picker_requested = false;
                    let prompt = match kind {
                        LairPromptKind::Rename => DojoPromptUi::rename_lair(target),
                        LairPromptKind::Preview => DojoPromptUi::preview_lair(target),
                        LairPromptKind::Restore => DojoPromptUi::restore_lair(target),
                        LairPromptKind::Terminate => DojoPromptUi::terminate_lair(target),
                    };
                    self.show_dojo_prompt(prompt);
                    changed = true;
                }
                WindowTopologyUpdate::SessionPickerFailed(_) => {
                    self.close_inline_session_picker();
                    self.modal.session_picker_requested = false;
                    self.tab_state.session_switch_pending = false;
                    let selector_kind = match self.modal.session_picker_retry_command.as_ref() {
                        Some(WindowTopologyCommand::RequestSelector { kind, .. }) => Some(*kind),
                        _ => None,
                    };
                    self.show_embedded_session_picker(
                        session_picker_status_catalog("Could not load navigation · R to retry"),
                        selector_kind,
                    )?;
                    eprintln!("splinterm Dojo picker failed");
                    changed = true;
                }
                WindowTopologyUpdate::Theme(update) => {
                    if self.modal.inline_picker_open() {
                        retain_newest_theme(&mut self.modal.deferred_picker_theme, update);
                    } else {
                        retain_newest_theme(&mut next_theme, update);
                    }
                }
                WindowTopologyUpdate::Font(update) => {
                    retain_newest_font(&mut self.presentation.pending_font_update, update);
                }
                WindowTopologyUpdate::Closed => self
                    .scheduling
                    .request_exit(ExitClass::CleanFinalTabRemoved),
                WindowTopologyUpdate::Reconnected {
                    daemon_generation,
                    reconnect_epoch,
                    exited,
                } => {
                    // Every tab was resnapshotted ahead of this update. Splints
                    // that exited meanwhile take the path an exit event takes.
                    let active_dojo = self.tab_state.active_dojo_id();
                    for (dojo_id, splint_id) in exited {
                        if dojo_id == active_dojo {
                            self.panes.pending_exited_splints.insert(splint_id);
                        } else if let Some(view) = self
                            .tab_state
                            .tabs
                            .get_mut(dojo_id)
                            .and_then(|tab| tab.value.as_mut())
                        {
                            view.pending_exited_splints.insert(splint_id);
                        }
                    }
                    self.handoff.reconnected(daemon_generation, reconnect_epoch);
                    changed = true;
                }
                WindowTopologyUpdate::Shutdown(_) if self.handoff.holds_window_on_disconnect() => {
                    // The manager stopped while a handoff was under way.
                    // Nothing is left to reconnect the Window: it stays open,
                    // paused, with its last screen and the lost connection.
                    self.tab_state.topology_commands = None;
                    self.handoff.give_up(HeldFailure::TopologyManager);
                }
                WindowTopologyUpdate::Shutdown(_) => {
                    if let Some(diagnostics) = diagnostics() {
                        diagnostics.emit(
                            DiagnosticLevel::Error,
                            DiagnosticEventCode::TopologyFailure,
                            Some(DiagnosticErrorCode::TopologyManager),
                        );
                    }
                    self.scheduling
                        .request_exit(ExitClass::ErrorTopologyManager);
                    anyhow::bail!("topology manager stopped");
                }
            }
            if refresh_explorer {
                let _ = self.refresh_lair_explorer();
            }
        }
        self.sync_graphical_focus();
        Ok((changed, next_theme))
    }

    fn apply_resolved_theme(&mut self, update: ThemeUpdate) -> ThemeUpdateImpact {
        if update.generation <= self.presentation.theme_generation {
            return ThemeUpdateImpact::default();
        }
        self.presentation.theme_generation = update.generation;
        // Resolved with this Window's `[colors]` keys: its role keys, which
        // the watcher never applies, and whatever a configuration reload
        // changed since the watcher started.
        let theme = self.presentation.theme_colors.accept(update);
        self.install_theme(theme)
    }

    /// Re-resolves the current theme after a configuration reload changed
    /// this Window's `[colors]` keys. A key the reload removed gives the
    /// theme's own value back.
    fn apply_reloaded_theme_colors(&mut self) -> ThemeUpdateImpact {
        match self
            .presentation
            .theme_colors
            .recolor(self.presentation.theme)
        {
            Some(theme) => self.install_theme(theme),
            None => ThemeUpdateImpact::default(),
        }
    }

    fn install_theme(&mut self, theme: ResolvedTheme) -> ThemeUpdateImpact {
        self.surface
            .background_effect_state
            .set_requested_blur(theme.background_blur);
        self.surface
            .background_effect_state
            .set_background_alpha(theme.background_alpha);
        let mut impact = classify_theme_update(self.presentation.theme, theme);
        self.presentation
            .render_context
            .set_background_alpha(theme.background_alpha);
        self.presentation.theme = theme;
        // A changed Omarchy text size redraws chrome at its new size; the
        // terminal follows it, when it does, through the font path.
        impact.redraw_chrome = self.reconcile_chrome_font_size();
        if impact.relayout_tab_strip {
            self.relayout_tab_strip();
        }
        if !impact.rebuild_pixels {
            return impact;
        }
        if let Some(snapshot) = self.panes.pane.snapshot.as_mut() {
            apply_theme(snapshot, theme);
        }
        for pane in &mut self.panes.inactive_panes {
            if let Some(snapshot) = pane.snapshot.as_mut() {
                apply_theme(snapshot, theme);
            }
        }
        for tab in self.tab_state.tabs.iter_mut() {
            if let Some(view) = tab.value.as_mut() {
                let mut dirty = Vec::new();
                for pane in std::iter::once(&mut view.pane).chain(view.inactive_panes.iter_mut()) {
                    if let Some(snapshot) = pane.snapshot.as_mut() {
                        apply_theme(snapshot, theme);
                        dirty.push(snapshot.splint_id);
                    }
                }
                view.dirty_inactive_panes.extend(dirty);
            }
        }
        self.tab_state.tab_label_cache.clear();
        self.presentation.full_redraw = true;
        impact
    }

    fn apply_inactive_updates(&mut self) -> Result<InactiveUpdateDrain> {
        let mut drain = InactiveUpdateDrain::default();
        for pane in &mut self.panes.inactive_panes {
            let mut pending = Vec::new();
            let mut disconnected = false;
            if let Some(updates) = &mut pane.updates {
                let drained = drain_receiver(updates, &self.platform.update_waker);
                pending = drained.items;
                disconnected = drained.disconnected;
            }
            if disconnected {
                pane.controller_active = false;
                pane.commands = None;
                pane.updates = None;
                drain.changed = true;
            }
            let mut terminal_updates = Vec::with_capacity(pending.len());
            for update in pending {
                match update {
                    WindowUpdate::Theme(update) => {
                        retain_newest_theme(&mut drain.theme, update);
                    }
                    WindowUpdate::Exited { splint_id } => {
                        anyhow::ensure!(
                            pane.snapshot
                                .as_ref()
                                .is_some_and(|snapshot| snapshot.splint_id == splint_id),
                            "inactive pane exit identity does not match its snapshot"
                        );
                        pane.controller_active = false;
                        pane.commands = None;
                        pane.updates = None;
                        drain.exited.push(splint_id);
                        drain.changed = true;
                    }
                    update => terminal_updates.push(update),
                }
            }
            let impact =
                apply_inactive_update_batch(pane, terminal_updates, self.presentation.theme)?;
            drain.record(
                pane.snapshot.as_ref().map(|snapshot| snapshot.splint_id),
                impact,
            );
        }
        Ok(drain)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "bounded update draining and semantic damage coalescing stay adjacent"
    )]
    fn apply_updates(&mut self, queue_handle: &QueueHandle<Self>) -> Result<()> {
        if std::mem::take(&mut self.scheduling.busy_notice) {
            self.window_notice("Busy: that action was not sent, try again");
        }
        // First, so every gate below sees this turn's handoff pause.
        self.apply_handoff_status(Instant::now());
        let (topology_changed, topology_theme) = self.apply_topology_updates(queue_handle)?;
        if topology_changed {
            self.cancel_copy_mode_for_topology();
        }
        // The resume record a handoff seals names exactly what this turn shows.
        if let Some(ledger) = &self.tab_state.layout_ledger
            && let Some(layout) = self.tab_state.window_layout(self.panes.focused_splint())
        {
            ledger.publish(layout);
        }
        let theme = self.presentation.theme;
        let waker = self.platform.update_waker.clone();
        let topology_commands = self.tab_state.topology_commands.clone();
        let handoff_paused = self.handoff_paused();
        let hold_on_disconnect = self.handoff.holds_window_on_disconnect();
        let mut held_hidden_pane = false;
        for tab in self.tab_state.tabs.iter_mut() {
            if let Some(view) = tab.value.as_mut() {
                if view.drain_hidden_updates(&waker, theme, hold_on_disconnect)? {
                    held_hidden_pane = true;
                }
                for pane in std::iter::once(&mut view.pane).chain(view.inactive_panes.iter_mut()) {
                    let input_pending = terminal_input_pending_for(
                        &self.input.pending_terminal_input,
                        pane.commands.as_ref(),
                    );
                    Self::retry_pane_focus_report(pane, input_pending);
                }
                Self::release_tab_controllers(view, &self.input.pending_terminal_input);
                if let Some(commands) = &topology_commands {
                    enqueue_exited_splints_unless_paused(
                        handoff_paused,
                        tab.dojo_id,
                        view.identity.lair_retention,
                        &mut view.pending_exited_splints,
                        commands,
                    );
                }
            }
        }
        if held_hidden_pane {
            self.handoff.hold(HeldFailure::PaneStream);
        }
        if self.scheduling.exit {
            return Ok(());
        }
        let active_input_pending = terminal_input_pending_for(
            &self.input.pending_terminal_input,
            self.panes.pane.commands.as_ref(),
        );
        Self::retry_pane_focus_report(&mut self.panes.pane, active_input_pending);
        Self::retry_pane_control_release(&mut self.panes.pane, active_input_pending);
        let inline_picker_open = self.modal.inline_picker_open();
        let picker_reconcile_pending = self.modal.session_picker_reconcile_pending;
        let mut next_theme = (!inline_picker_open)
            .then(|| self.modal.deferred_picker_theme.take())
            .flatten();
        if let Some(update) = topology_theme {
            retain_newest_theme(&mut next_theme, update);
        }
        let restored_frontend = std::mem::take(&mut self.panes.restored_frontend_needs_resize);
        let final_resize_reconciliation = topology_changed || restored_frontend;
        if inline_picker_open && restored_frontend {
            self.panes.restored_frontend_needs_resize = true;
        } else if final_resize_reconciliation && !picker_reconcile_pending {
            self.emit_resize()?;
        }
        if restored_frontend && !inline_picker_open && !picker_reconcile_pending {
            self.update_ime_cursor_rectangle();
        }

        // Topology may promote a newly added pane to focus. Drain and arm the
        // post-topology focused receiver so it cannot fall back to the timed tick.
        let mut pending = Vec::new();
        let mut disconnected = false;
        if let Some(updates) = &mut self.panes.pane.updates {
            let drained = drain_receiver(updates, &self.platform.update_waker);
            pending = drained.items;
            disconnected = drained.disconnected;
        }
        if disconnected
            && !pane_stream_has_terminal_notice(&pending)
            && self.handoff.holds_window_on_disconnect()
        {
            // Lost while a handoff was preparing or fenced, as when it
            // commits and the daemon execs: the Window stays with its banner
            // and last screen while its coordinator reconnects and replaces
            // this pane with a resnapshotted one.
            self.panes.pane.controller_active = false;
            self.panes.pane.commands = None;
            self.panes.pane.updates = None;
            self.handoff.hold(HeldFailure::PaneStream);
        } else if disconnected && !pane_stream_has_terminal_notice(&pending) {
            if let Some(diagnostics) = diagnostics() {
                diagnostics.emit(
                    DiagnosticLevel::Error,
                    DiagnosticEventCode::PaneStreamFailure,
                    Some(DiagnosticErrorCode::PaneStream),
                );
            }
            self.scheduling.request_exit(ExitClass::ErrorPaneStream);
            return Ok(());
        }
        let receiver_batch_size = pending.len();
        let mut inactive = self.apply_inactive_updates()?;
        self.panes
            .pending_exited_splints
            .extend(inactive.exited.iter().copied());
        if let Some(update) = inactive.theme.take() {
            retain_newest_theme(&mut next_theme, update);
        }
        let mut visual_changed = topology_changed | inactive.changed;
        let mut focused_visual_changed = topology_changed;
        let mut title_changed = false;
        let mut full_frame_reload = false;
        let mut history_content_changed = false;
        let mut rebuild_all_inactive = false;
        let mut effect_desired_changed = false;
        let mut font_reconciled = false;
        let mut strip_relayout = false;
        let mut impact = ThemeUpdateImpact::default();
        if let Some(update) = next_theme {
            if inline_picker_open {
                retain_newest_theme(&mut self.modal.deferred_picker_theme, update);
            } else {
                impact = self.apply_resolved_theme(update);
            }
        }
        if !inline_picker_open {
            // A configuration reload changed this Window's `[colors]` keys.
            impact = impact.or(self.apply_reloaded_theme_colors());
        }
        visual_changed |= impact.rebuild_pixels;
        focused_visual_changed |= impact.rebuild_pixels;
        full_frame_reload |= impact.rebuild_pixels;
        rebuild_all_inactive |= impact.rebuild_pixels;
        effect_desired_changed |= impact.reconcile_effect;
        strip_relayout |= impact.relayout_tab_strip;
        visual_changed |= impact.relayout_tab_strip | impact.redraw_chrome;
        for update in pending {
            match update {
                WindowUpdate::Snapshot {
                    mut snapshot,
                    image_sources,
                    authoritative,
                } => {
                    self.panes.pane.history_page_pending = false;
                    self.panes.pane.resync_pending &= !authoritative;
                    snapshot
                        .validate()
                        .map_err(|error| anyhow::anyhow!(error.message))?;
                    apply_theme(&mut snapshot, self.presentation.theme);
                    let accept = match self.panes.pane.snapshot.as_ref() {
                        Some(current) => snapshot_replaces(current, &snapshot, authoritative)?,
                        None => true,
                    };
                    if accept {
                        let previous_generation = self
                            .panes
                            .pane
                            .snapshot
                            .as_ref()
                            .map_or(snapshot.history_generation, |current| {
                                current.history_generation
                            });
                        let previous_rows = self
                            .panes
                            .pane
                            .history_rows_needed_for_viewport_transition();
                        let pane = &mut self.panes.pane;
                        if let Some(current) = pane.snapshot.as_ref() {
                            retain_history_for_viewport(
                                current,
                                &mut snapshot,
                                &mut pane.scrollback_viewport,
                            );
                        }
                        pane.scrollback_viewport.observe_history_change(
                            previous_generation,
                            &previous_rows,
                            &snapshot,
                        );
                        discard_lagging_history(&mut snapshot, &pane.scrollback_viewport);
                        self.invalidate_local_content_state();
                        self.panes.pane.snapshot = Some(snapshot);
                        self.panes.pane.image_sources = image_sources;
                        self.panes.pane.clear_trace_correlation();
                        self.presentation.full_redraw = true;
                        full_frame_reload = true;
                        visual_changed = true;
                        focused_visual_changed = true;
                        title_changed = true;
                    }
                }
                WindowUpdate::Update {
                    update,
                    image_sources,
                    trace,
                } => {
                    let apply_started = perf_trace_enabled().then(Instant::now);
                    let trace_base_revision = update.base_revision;
                    let trace_revision = update.revision;
                    let trace_rows = update.rows.len();
                    let trace_image_changed = image_sources.is_some();
                    let old_cursor_row = self.panes.pane.snapshot.as_ref().and_then(|snapshot| {
                        usize::try_from(snapshot.cursor_row)
                            .ok()
                            .filter(|row| *row < snapshot.rows)
                    });
                    let scrolls = update.scrolls.clone();
                    let current = self
                        .panes
                        .pane
                        .snapshot
                        .as_ref()
                        .context("terminal update arrived before initial snapshot")?;
                    let patched_rows = changed_terminal_patch_rows(&update, current);
                    let full_frame_reasons = terminal_update_full_frame_reasons(&update, current);
                    let mut full = full_frame_reasons != 0;
                    let content_changed = terminal_update_changes_visible_content(&update);
                    history_content_changed |= content_changed || image_sources.is_some();
                    let cursor_changed = update.cursor.is_some() || update.input_modes.is_some();
                    title_changed |= update.title.is_some();
                    if content_changed {
                        self.dirty_selection(self.panes.pane.selection);
                    }
                    let previous_generation = self
                        .panes
                        .pane
                        .snapshot
                        .as_ref()
                        .map_or(1, |snapshot| snapshot.history_generation);
                    let previous_rows = self
                        .panes
                        .pane
                        .history_rows_needed_for_viewport_transition();
                    let trace_copied_history_bytes =
                        apply_started.map(|_| history_cache_bytes(&previous_rows));
                    let pane = &mut self.panes.pane;
                    let snapshot = pane
                        .snapshot
                        .as_mut()
                        .context("terminal update arrived before initial snapshot")?;
                    apply_terminal_update_for_viewport(
                        snapshot,
                        update,
                        &mut pane.scrollback_viewport,
                    )?;
                    apply_theme(snapshot, self.presentation.theme);
                    pane.scrollback_viewport.observe_history_change(
                        previous_generation,
                        &previous_rows,
                        snapshot,
                    );
                    discard_lagging_history(snapshot, &pane.scrollback_viewport);
                    // The update's dirty rows and scrolls are live-screen
                    // positions; a view off live shows other rows there.
                    if !self.panes.pane.scrollback_viewport.is_live() {
                        full = true;
                    }
                    let trace_pane_role = self.panes.pane.trace_pane_role("focused");
                    if content_changed {
                        self.reconcile_selection_after_content_change();
                    }
                    let snapshot = self
                        .panes
                        .pane
                        .snapshot
                        .as_ref()
                        .context("updated terminal snapshot exists")?;
                    let rows = snapshot.rows;
                    if let Some(image_sources) = image_sources {
                        self.panes.pane.image_sources = image_sources;
                    }
                    self.panes.pane.prepare_dirty_rows.resize(rows, false);
                    self.panes.pane.raster_dirty_rows.resize(rows, false);
                    self.panes.pane.surface_dirty_rows.resize(rows, false);
                    if full {
                        self.presentation.full_redraw = true;
                        full_frame_reload = true;
                    } else {
                        if !scrolls.is_empty()
                            && let Some(row) = old_cursor_row
                        {
                            // The cursor is part of the persistent backing pixels. Seed its old
                            // row as dirty so scroll-copy damage follows the copied cursor block.
                            self.panes.pane.raster_dirty_rows[row] = true;
                        }
                        for scroll in &scrolls {
                            propagate_raster_damage_through_scroll(
                                &mut self.panes.pane.raster_dirty_rows,
                                scroll,
                            );
                            for row in scroll.start_row..scroll.end_row.min(rows) {
                                // Rebuilding the bounded semantic scroll region keeps prepared
                                // row geometry correct while pixel movement still uses scroll-copy.
                                self.panes.pane.prepare_dirty_rows[row] = true;
                                self.panes.pane.surface_dirty_rows[row] = true;
                            }
                            let count = scroll
                                .rows
                                .min(scroll.end_row.saturating_sub(scroll.start_row));
                            let exposed = match scroll.direction {
                                splinterm_protocol::ScrollDirection::Forward => {
                                    scroll.end_row.saturating_sub(count)..scroll.end_row
                                }
                                splinterm_protocol::ScrollDirection::Reverse => {
                                    scroll.start_row..scroll.start_row.saturating_add(count)
                                }
                            };
                            for row in exposed.filter(|row| *row < rows) {
                                self.panes.pane.raster_dirty_rows[row] = true;
                            }
                        }
                        for row in patched_rows.into_iter().filter(|row| *row < rows) {
                            self.panes.pane.prepare_dirty_rows[row] = true;
                            self.panes.pane.raster_dirty_rows[row] = true;
                            self.panes.pane.surface_dirty_rows[row] = true;
                        }
                        if cursor_changed {
                            if let Some(row) = old_cursor_row {
                                self.panes.pane.raster_dirty_rows[row] = true;
                                self.panes.pane.surface_dirty_rows[row] = true;
                            }
                            if let Ok(row) = usize::try_from(snapshot.cursor_row)
                                && row < rows
                            {
                                self.panes.pane.raster_dirty_rows[row] = true;
                                self.panes.pane.surface_dirty_rows[row] = true;
                            }
                        }
                        self.panes.pane.pending_scrolls.extend(scrolls);
                    }
                    let update_visual_changed = terminal_update_has_visual_damage(
                        full,
                        cursor_changed,
                        &self.panes.pane.raster_dirty_rows,
                        &self.panes.pane.surface_dirty_rows,
                    );
                    visual_changed |= update_visual_changed;
                    focused_visual_changed |= update_visual_changed;
                    let trace_draw_expected = full || update_visual_changed || trace_image_changed;
                    if let Some(started) = apply_started {
                        emit_perf_trace(
                            "splinterm",
                            "client_apply",
                            PerfTraceEvent {
                                splint_id: Some(snapshot.splint_id),
                                incarnation: Some(snapshot.incarnation),
                                base_revision: Some(trace_base_revision),
                                revision: Some(trace_revision),
                                subscription_id: trace.map(|value| value.subscription_id),
                                transaction_sequence: trace.map(|value| value.transaction_sequence),
                                duration_ns: Some(
                                    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                                ),
                                rows: Some(u64::try_from(trace_rows).unwrap_or(u64::MAX)),
                                pane_role: Some(trace_pane_role),
                                pane_count: Some(
                                    u64::try_from(
                                        self.panes.inactive_panes.len().saturating_add(1),
                                    )
                                    .unwrap_or(u64::MAX),
                                ),
                                cached_history_rows: Some(
                                    u64::try_from(snapshot.scrollback_rows.len())
                                        .unwrap_or(u64::MAX),
                                ),
                                cached_history_bytes: Some(
                                    u64::try_from(history_cache_bytes(&snapshot.scrollback_rows))
                                        .unwrap_or(u64::MAX),
                                ),
                                copied_history_rows: Some(
                                    u64::try_from(previous_rows.len()).unwrap_or(u64::MAX),
                                ),
                                copied_history_bytes: trace_copied_history_bytes
                                    .map(|value| u64::try_from(value).unwrap_or(u64::MAX)),
                                receiver_batch_size: Some(
                                    u64::try_from(receiver_batch_size).unwrap_or(u64::MAX),
                                ),
                                // Bitset: columns, rows, palette, defaults, screen, images,
                                // or an image-bearing scroll in ascending bit order.
                                count: Some(full_frame_reasons),
                                full_reload: Some(full),
                                ..PerfTraceEvent::default()
                            },
                        );
                    }
                    if trace_draw_expected {
                        self.panes
                            .pane
                            .retain_trace_correlation(trace, trace_pane_role);
                    }
                }
                WindowUpdate::ScrollbackPages(pages) => {
                    history_content_changed = true;
                    self.panes.pane.history_page_pending = false;
                    let pinned_selection_rows = self
                        .panes
                        .pane
                        .selection
                        .map(|selection| [selection.anchor.row_id, selection.end.row_id]);
                    let snapshot = self
                        .panes
                        .pane
                        .snapshot
                        .as_mut()
                        .context("scrollback pages arrived before initial snapshot")?;
                    if !pages_join_history(&pages, snapshot, self.panes.pane.history_page_top) {
                        continue;
                    }
                    let first_loaded = snapshot
                        .scrollback_rows
                        .first()
                        .and_then(|row| row.row_id)
                        .unwrap_or(u64::MAX);
                    let existing = snapshot
                        .scrollback_rows
                        .iter()
                        .filter_map(|row| row.row_id)
                        .collect::<std::collections::BTreeSet<_>>();
                    let metadata = pages
                        .first()
                        .filter(|page| page.terminal_revision == snapshot.revision)
                        .map(|page| (page.oldest_available_row_id, page.newest_available_row_id));
                    let mut older = pages
                        .into_iter()
                        .rev()
                        .flat_map(|page| page.rows)
                        .filter(|row| {
                            row.row_id
                                .is_some_and(|id| id < first_loaded && !existing.contains(&id))
                        })
                        .collect::<Vec<_>>();
                    if !older.is_empty() {
                        older.extend(snapshot.scrollback_rows.iter().cloned());
                        // Keep one contiguous newest history window. If normal bounding
                        // would remove a selected endpoint, reject this older batch so the
                        // existing bounded endpoint window remains pinned and pageable.
                        // Selections resolve against the held screen while
                        // a view holds.
                        let screen = self
                            .panes
                            .pane
                            .scrollback_viewport
                            .held_screen()
                            .unwrap_or(&snapshot.visible_rows);
                        if let Some(older) =
                            bound_history_page_with_pins(older, pinned_selection_rows, screen)
                        {
                            snapshot.scrollback_rows = older;
                            snapshot.omitted_oldest_scrollback_rows = omitted_rows_before_cache(
                                snapshot.oldest_available_scrollback_row_id,
                                &snapshot.scrollback_rows,
                                snapshot.available_scrollback_rows,
                            );
                            if let Some((oldest, newest)) = metadata {
                                snapshot.oldest_available_scrollback_row_id = oldest;
                                snapshot.newest_available_scrollback_row_id = newest;
                            }
                            // A full cache drops its newest rows, below the
                            // view: keep the view on its rows, and rebuild the
                            // frame, whose rows and drawn offset are now stale.
                            if !self.panes.pane.scrollback_viewport.is_live() {
                                self.panes.pane.scrollback_viewport.reanchor(snapshot);
                                visual_changed = true;
                                focused_visual_changed = true;
                            }
                        } else {
                            self.panes.pane.history_selection_pin_blocked = true;
                        }
                    }
                }
                WindowUpdate::ScrollbackResyncRequired => {
                    self.panes.pane.history_page_pending = false;
                    self.invalidate_local_content_state();
                    visual_changed = true;
                    focused_visual_changed = true;
                }
                WindowUpdate::Authority(authority) => {
                    self.panes.pane.authority = authority;
                    title_changed = true;
                    visual_changed = true;
                    focused_visual_changed = true;
                    self.presentation.full_redraw = true;
                }
                WindowUpdate::Control(active) => {
                    self.cancel_clipboard_image();
                    self.clipboard.palette_image_target = None;
                    self.panes.pane.controller_active = active;
                    title_changed = true;
                    visual_changed = true;
                    focused_visual_changed = true;
                    self.presentation.full_redraw = true;
                }
                WindowUpdate::ControlTransferRequested(transfer_id) => {
                    self.panes.pane.pending_control_transfer = Some(transfer_id);
                    title_changed = true;
                }
                WindowUpdate::ControlTransferResolved(_) => {
                    self.panes.pane.pending_control_transfer = None;
                    title_changed = true;
                }
                WindowUpdate::SearchResults(page) => {
                    if let Some(cursor) = self.panes.pane.search.take_page(page) {
                        self.submit_search(Some(cursor));
                    }
                    title_changed = true;
                    visual_changed = true;
                    focused_visual_changed = true;
                    self.presentation.full_redraw = true;
                }
                WindowUpdate::SearchResyncRequired => {
                    self.panes.pane.search.resync_required();
                    title_changed = true;
                    visual_changed = true;
                    focused_visual_changed = true;
                    self.presentation.full_redraw = true;
                }
                WindowUpdate::Theme(update) => {
                    if self.modal.inline_picker_open() {
                        retain_newest_theme(&mut self.modal.deferred_picker_theme, update);
                    } else {
                        let impact = self.apply_resolved_theme(update);
                        visual_changed |= impact.rebuild_pixels;
                        focused_visual_changed |= impact.rebuild_pixels;
                        full_frame_reload |= impact.rebuild_pixels;
                        rebuild_all_inactive |= impact.rebuild_pixels;
                        effect_desired_changed |= impact.reconcile_effect;
                        strip_relayout |= impact.relayout_tab_strip;
                        visual_changed |= impact.relayout_tab_strip | impact.redraw_chrome;
                    }
                }
                WindowUpdate::Font(update) => {
                    retain_newest_font(&mut self.presentation.pending_font_update, update);
                }
                WindowUpdate::Exited { splint_id } => {
                    anyhow::ensure!(
                        self.panes.focused_splint() == Some(splint_id),
                        "focused pane exit identity does not match its snapshot"
                    );
                    if self.tab_state.topology_commands.is_some() {
                        self.panes.pending_exited_splints.insert(splint_id);
                        self.panes.pane.controller_active = false;
                        self.panes.pane.commands = None;
                        self.panes.pane.updates = None;
                        title_changed = true;
                        visual_changed = true;
                        focused_visual_changed = true;
                        self.presentation.full_redraw = true;
                    } else {
                        self.scheduling
                            .request_exit(ExitClass::CleanFinalTabRemoved);
                        return Ok(());
                    }
                }
                WindowUpdate::Shutdown => {
                    if self.panes.layout.is_some() {
                        self.panes.pane.controller_active = false;
                        self.panes.pane.commands = None;
                        self.panes.pane.updates = None;
                        title_changed = true;
                        visual_changed = true;
                        focused_visual_changed = true;
                        self.presentation.full_redraw = true;
                    } else {
                        self.scheduling.request_exit(ExitClass::ErrorPaneStream);
                        return Ok(());
                    }
                }
            }
        }
        if let Some(update) = self.presentation.pending_font_update.take() {
            match self.reconcile_font_generation(update) {
                Ok(true) => {
                    font_reconciled = true;
                    visual_changed = true;
                    focused_visual_changed = true;
                    self.presentation.full_redraw = true;
                }
                Ok(false) => {}
                Err(error) => {
                    eprintln!(
                        "splinterm live font update rejected during frame reconciliation: {error:#}"
                    );
                }
            }
        }
        if self.reconcile_followed_text_size() {
            font_reconciled = true;
            visual_changed = true;
            focused_visual_changed = true;
            self.presentation.full_redraw = true;
        }
        // A strip height change resizes the panes here, once: a font
        // reconciliation above has already resized them from the new content
        // area, while a rejected or unchanged font still needs this resize.
        if strip_relayout_needs_resize(strip_relayout, font_reconciled, self.surface.configured) {
            self.emit_resize()?;
        }
        if !self.panes.pending_exited_splints.is_empty() {
            let handoff_paused = self.handoff_paused();
            let topology_queue_open =
                self.tab_state
                    .topology_commands
                    .as_ref()
                    .is_some_and(|commands| {
                        enqueue_exited_splints_unless_paused(
                            handoff_paused,
                            self.tab_state.active_identity.dojo_id,
                            self.tab_state.active_identity.lair_retention,
                            &mut self.panes.pending_exited_splints,
                            commands,
                        )
                    });
            if !topology_queue_open {
                self.tab_state.topology_commands = None;
            }
        }
        let rebuilt_inactive = rebuild_inactive_frames_with_context(
            &mut self.panes.inactive_panes,
            &inactive.dirty_frames,
            rebuild_all_inactive,
            self.surface.scale_120,
            &self.presentation.render_context,
        )?;
        visual_changed |= rebuilt_inactive > 0;
        if rebuilt_inactive > 0 {
            self.panes
                .dirty_inactive_panes
                .extend(inactive.panes_to_repaint().iter().copied());
        }
        if rebuild_all_inactive {
            self.presentation.full_redraw = true;
        } else if rebuilt_inactive == 0 && inactive.changed {
            // Rare inactive metadata changes do not identify a terminal frame
            // region, so retain the conservative whole-window fallback.
            self.presentation.full_redraw = true;
        }
        if self.panes.pane.search.pending_reveal.is_some() {
            self.reveal_pending_search_match();
            visual_changed = true;
            focused_visual_changed = true;
        }
        if self
            .surface
            .background_effect_reconcile_schedule
            .queue_update(
                effect_desired_changed,
                visual_changed,
                self.surface.configured,
            )
        {
            self.reconcile_background_effect(queue_handle, BackgroundEffectCommitMode::Immediate)?;
        }
        if restart_cursor_blink(
            focused_visual_changed,
            &mut self.input.cursor_blink_visible,
            &mut self.input.last_cursor_blink,
        ) {
            let prepare_started = perf_trace_enabled().then(Instant::now);
            let live_viewport = self.panes.pane.scrollback_viewport.is_live();
            let display_owned = if live_viewport {
                None
            } else {
                Some(
                    self.panes
                        .display_snapshot()
                        .context("updated snapshot exists")?,
                )
            };
            let display = display_owned
                .as_ref()
                .or(self.panes.pane.snapshot.as_ref())
                .context("updated snapshot exists")?;
            let cursor_only_history = !live_viewport
                && !history_content_changed
                && !self.panes.pane.viewport_dirty
                && self
                    .panes
                    .pane
                    .snapshot_frame
                    .as_ref()
                    .is_some_and(|frame| frame.can_refresh_cursor_rows(display));
            if cursor_only_history {
                self.panes.pane.prepare_dirty_rows.fill(false);
            }
            if let Some(frame) = &self.panes.pane.snapshot_frame {
                self.panes
                    .pane
                    .prepare_dirty_rows
                    .resize(display.rows, false);
                self.panes
                    .pane
                    .raster_dirty_rows
                    .resize(display.rows, false);
                self.panes
                    .pane
                    .surface_dirty_rows
                    .resize(display.rows, false);
                frame.mark_cursor_dirty_rows(display, &mut self.panes.pane.prepare_dirty_rows);
                frame.mark_cursor_dirty_rows(display, &mut self.panes.pane.raster_dirty_rows);
                frame.mark_cursor_dirty_rows(display, &mut self.panes.pane.surface_dirty_rows);
            }
            let trace_dirty_rows = self
                .panes
                .pane
                .prepare_dirty_rows
                .iter()
                .filter(|dirty| **dirty)
                .count();
            // A view that moved since its last frame, by scrolling or by a
            // history page joining, shows other rows in most places. The
            // rows marked dirty above cover only what output changed, so
            // repaint the whole pane; the draw step that would have done it
            // for the scroll is skipped once this frame is current.
            let shown_rows_changed = if live_viewport {
                self.panes.pane.viewport_dirty
            } else {
                self.panes.pane.viewport_dirty
                    || self.panes.pane.rendered_row_ids != row_ids(display)
            };
            if shown_rows_changed {
                self.presentation.full_redraw = true;
            }
            if full_frame_reload
                || shown_rows_changed
                || self.panes.pane.snapshot_frame.is_none()
                || (!live_viewport && !cursor_only_history)
            {
                self.panes.pane.snapshot_frame =
                    Some(SnapshotFrame::load_scaled_with_sources_and_context(
                        display,
                        self.surface.scale_120,
                        Some(&self.panes.pane.image_sources),
                        &self.presentation.render_context,
                    )?);
            } else if let Some(frame) = &mut self.panes.pane.snapshot_frame {
                frame.refresh_rows_with_context(
                    display,
                    &self.panes.pane.prepare_dirty_rows,
                    &self.presentation.render_context,
                )?;
                frame.refresh_images(display, &self.panes.pane.image_sources)?;
            }
            self.panes.pane.rendered_viewport_offset =
                self.panes.pane.scrollback_viewport.offset_from_bottom();
            self.panes.pane.rendered_row_ids = row_ids(display);
            self.panes.pane.viewport_dirty = false;
            self.panes.pane.prepare_dirty_rows.fill(false);
            if let Some(started) = prepare_started {
                emit_perf_trace(
                    "splinterm",
                    "frame_prepare",
                    PerfTraceEvent {
                        splint_id: Some(display.splint_id),
                        incarnation: Some(display.incarnation),
                        base_revision: self
                            .panes
                            .pane
                            .trace_correlation
                            .map(|value| value.base_revision),
                        revision: Some(
                            self.panes
                                .pane
                                .trace_correlation
                                .map_or(display.revision, |value| value.revision),
                        ),
                        subscription_id: self
                            .panes
                            .pane
                            .trace_correlation
                            .map(|value| value.subscription_id),
                        transaction_sequence: self
                            .panes
                            .pane
                            .trace_correlation
                            .map(|value| value.transaction_sequence),
                        duration_ns: Some(
                            u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                        ),
                        rows: Some(u64::try_from(display.rows).unwrap_or(u64::MAX)),
                        cells: Some(
                            u64::try_from(display.rows.saturating_mul(display.columns))
                                .unwrap_or(u64::MAX),
                        ),
                        count: Some(u64::try_from(trace_dirty_rows).unwrap_or(u64::MAX)),
                        pane_role: Some(self.panes.pane.trace_pane_role),
                        pane_count: Some(
                            u64::try_from(self.panes.inactive_panes.len().saturating_add(1))
                                .unwrap_or(u64::MAX),
                        ),
                        columns: Some(u64::try_from(display.columns).unwrap_or(u64::MAX)),
                        cached_history_rows: Some(
                            u64::try_from(display.scrollback_rows.len()).unwrap_or(u64::MAX),
                        ),
                        dirty_rows: Some(u64::try_from(trace_dirty_rows).unwrap_or(u64::MAX)),
                        prepared_rows: Some(u64::try_from(display.rows).unwrap_or(u64::MAX)),
                        prepared_cells: Some(
                            u64::try_from(display.rows.saturating_mul(display.columns))
                                .unwrap_or(u64::MAX),
                        ),
                        superseded_revisions: Some(self.panes.pane.trace_superseded_revisions),
                        full_reload: Some(full_frame_reload),
                        ..PerfTraceEvent::default()
                    },
                );
            }
            self.refresh_ime_preedit()?;
            self.update_ime_cursor_rectangle();
            if !font_reconciled
                && !self.modal.inline_picker_open()
                && !self.modal.session_picker_reconcile_pending
                && terminal_resize_allowed(
                    TerminalResizeCause::SnapshotAvailable,
                    self.panes.pane.last_resize.is_some(),
                )
            {
                self.emit_resize()?;
            }
        }
        if visual_changed && self.surface.configured && !self.modal.session_picker_reconcile_pending
        {
            self.schedule_terminal_draw(queue_handle)?;
        }
        if title_changed
            && !self.modal.input_modal_open()
            && !self.modal.session_picker_reconcile_pending
        {
            let title = self
                .terminal_window_title()
                .context("updated snapshot exists")?;
            self.surface.window.set_title(title);
        }
        if picker_reconcile_pending {
            self.modal.session_picker_reconcile_pending = false;
            if final_resize_reconciliation {
                self.emit_resize()?;
                self.update_ime_cursor_rectangle();
            }
            self.update_window_title();
            match picker_ime_reconcile(
                self.input.ime_modal_barrier,
                self.input.keyboard_focused,
                self.input.ime.entered,
            ) {
                PickerImeReconcile::Renew => self.renew_text_input(queue_handle),
                PickerImeReconcile::Enable => self.enable_text_input_if_allowed(),
                PickerImeReconcile::None => {}
            }
            let modal_focus_changed = self
                .modal
                .session_picker_open_focus
                .take()
                .is_some_and(|focused| focused != self.input.keyboard_focused);
            self.reconcile_terminal_focus_report(modal_focus_changed);
            self.presentation.full_redraw = true;
            if self.surface.configured {
                self.schedule_draw(queue_handle)?;
            }
        }
        Ok(())
    }

    fn refresh_output_dpi(
        &mut self,
        output: &wl_output::WlOutput,
        queue_handle: &QueueHandle<Self>,
    ) -> Result<()> {
        let observation = self.platform.output_dpi_observation(output);
        let raster_changed = self
            .presentation
            .render_context
            .update_output_dpi(observation, self.surface.scale_120)?;
        self.presentation.renderer_generation =
            self.presentation.renderer_generation.saturating_add(1);
        self.modal.session_picker_text_cache.clear();
        self.tab_state.tab_label_cache.clear();
        self.tab_state.tab_close_text = None;
        self.tab_state.remote_host_text = None;
        self.tab_state.remote_host_clipped = None;
        self.tab_state.tab_new_text = None;
        self.modal.session_picker_layout = None;
        if !raster_changed {
            if self.modal.inline_picker_open() && self.surface.configured {
                self.modal.session_picker_redraw = true;
                self.schedule_draw(queue_handle)?;
            }
            return Ok(());
        }
        if self.modal.inline_picker_open() {
            self.panes.restored_frontend_needs_resize = true;
        }
        self.rebuild_scaled_pane_frames(self.surface.scale_120)?;
        self.surface.buffers.clear();
        self.surface.backing.clear();
        self.presentation.full_redraw = true;
        self.refresh_ime_preedit()?;
        debug_assert!(!terminal_resize_allowed(
            TerminalResizeCause::OutputDpiChanged,
            self.panes.pane.last_resize.is_some(),
        ));
        self.update_ime_cursor_rectangle();
        if self.surface.configured {
            self.schedule_draw(queue_handle)?;
        }
        Ok(())
    }

    fn apply_scale(
        &mut self,
        requested_scale_120: u32,
        queue_handle: &QueueHandle<Self>,
    ) -> Result<()> {
        let scale_120 = requested_scale_120;
        if !(MIN_SCALE_120..=MAX_SCALE_120).contains(&scale_120)
            || scale_120 == self.surface.scale_120
        {
            return Ok(());
        }
        if self.surface.viewport.is_none() && !scale_120.is_multiple_of(SCALE_DENOMINATOR) {
            return Ok(());
        }
        if self.surface.viewport.is_none() {
            self.surface
                .window
                .set_buffer_scale(scale_120 / SCALE_DENOMINATOR)
                .map_err(|_| anyhow::anyhow!("compositor rejected integer buffer scale"))?;
        } else {
            self.surface
                .window
                .set_buffer_scale(1)
                .map_err(|_| anyhow::anyhow!("compositor rejected unit buffer scale"))?;
        }
        if !self.rebuild_scaled_pane_frames(scale_120)? {
            self.presentation.text_row =
                Some(TextRow::load(scale_120.div_ceil(SCALE_DENOMINATOR))?);
        }
        self.surface.scale_120 = scale_120;
        self.presentation.renderer_generation =
            self.presentation.renderer_generation.saturating_add(1);
        self.modal.session_picker_text_cache.clear();
        self.tab_state.tab_label_cache.clear();
        self.tab_state.tab_close_text = None;
        self.tab_state.remote_host_text = None;
        self.tab_state.remote_host_clipped = None;
        self.tab_state.tab_new_text = None;
        self.modal.session_picker_layout = None;
        if self.modal.inline_picker_open() {
            self.panes.restored_frontend_needs_resize = true;
        }
        self.surface.buffers.clear();
        self.surface.backing.clear();
        self.presentation.full_redraw = true;
        self.refresh_ime_preedit()?;
        debug_assert!(!terminal_resize_allowed(
            TerminalResizeCause::CompositorScaleChanged,
            self.panes.pane.last_resize.is_some(),
        ));
        self.update_ime_cursor_rectangle();
        if self.surface.configured {
            self.schedule_draw(queue_handle)?;
        }
        Ok(())
    }

    fn cursor_is_blinking(&self) -> bool {
        !self.modal.inline_picker_open()
            && self.presentation.cursor_blink
            && self.panes.pane.snapshot.as_ref().is_some_and(|snapshot| {
                cursor_blink_enabled(
                    self.input.reduced_motion,
                    self.input.keyboard_focused,
                    snapshot.input_modes,
                )
            })
    }

    fn retry_pending_pane_resizes(&mut self) -> Result<()> {
        for pane in
            std::iter::once(&mut self.panes.pane).chain(self.panes.inactive_panes.iter_mut())
        {
            Self::retry_pending_pane_resize(pane)?;
        }
        for tab in self.tab_state.tabs.iter_mut() {
            if let Some(view) = tab.value.as_mut() {
                for pane in std::iter::once(&mut view.pane).chain(view.inactive_panes.iter_mut()) {
                    Self::retry_pending_pane_resize(pane)?;
                }
            }
        }
        Ok(())
    }

    fn has_deferred_pane_command(&self) -> bool {
        std::iter::once(&self.panes.pane)
            .chain(self.panes.inactive_panes.iter())
            .any(pane_command_retry_pending)
            || self.tab_state.tabs.iter().any(|tab| {
                tab.value.as_ref().is_some_and(|view| {
                    std::iter::once(&view.pane)
                        .chain(view.inactive_panes.iter())
                        .any(pane_command_retry_pending)
                })
            })
    }

    /// Draws a pending redraw unless it must wait for a frame callback.
    fn draw_if_pending(&mut self, queue_handle: &QueueHandle<Self>) -> Result<()> {
        if self.scheduling.redraw_pending
            && !pending_draw_waits_for_frame(
                self.scheduling.frame_pending,
                self.scheduling.terminal_redraw_pending,
            )
        {
            if self.scheduling.terminal_redraw_pending {
                self.schedule_terminal_draw(queue_handle)?;
            } else {
                self.schedule_draw(queue_handle)?;
            }
        }
        Ok(())
    }

    fn event_loop_dispatch_timeout(&self) -> Option<Duration> {
        let timeout = event_loop_timeout(
            self.scheduling.exit,
            self.scheduling.signoff.is_some(),
            self.cursor_is_blinking()
                .then(|| self.input.last_cursor_blink.elapsed()),
        );
        let command_retry_pending =
            !self.input.pending_terminal_input.is_empty() || self.has_deferred_pane_command();
        let timeout = command_retry_dispatch_timeout(timeout, command_retry_pending);
        // The agent spinner's next frame is one more deadline on the timer
        // that wakes the loop for the cursor blink.
        let timeout = match (timeout, self.agent_spinner_timeout()) {
            (Some(timeout), Some(spinner)) => Some(timeout.min(spinner)),
            (timeout, _) => timeout,
        };
        // Wake to clear an expired upgrade outcome notice.
        match (timeout, self.handoff.timeout(Instant::now())) {
            (Some(timeout), Some(notice)) => Some(timeout.min(notice)),
            (timeout, _) => timeout,
        }
    }

    fn tick_cursor_blink(&mut self, queue_handle: &QueueHandle<Self>) -> Result<()> {
        let blinking = self.cursor_is_blinking();
        if blinking && self.input.last_cursor_blink.elapsed() >= CURSOR_BLINK_INTERVAL {
            self.input.cursor_blink_visible = !self.input.cursor_blink_visible;
            self.input.last_cursor_blink = Instant::now();
            if let Some(snapshot) = &self.panes.pane.snapshot
                && let Ok(row) = usize::try_from(snapshot.cursor_row)
                && row < snapshot.rows
            {
                self.panes
                    .pane
                    .raster_dirty_rows
                    .resize(snapshot.rows, false);
                self.panes
                    .pane
                    .surface_dirty_rows
                    .resize(snapshot.rows, false);
                self.panes.pane.raster_dirty_rows[row] = true;
                self.panes.pane.surface_dirty_rows[row] = true;
            }
            if self.surface.configured {
                self.schedule_draw(queue_handle)?;
            }
        } else if !blinking && !self.input.cursor_blink_visible {
            self.input.cursor_blink_visible = true;
            if let Some(snapshot) = &self.panes.pane.snapshot
                && let Ok(row) = usize::try_from(snapshot.cursor_row)
                && row < snapshot.rows
            {
                self.panes
                    .pane
                    .raster_dirty_rows
                    .resize(snapshot.rows, false);
                self.panes
                    .pane
                    .surface_dirty_rows
                    .resize(snapshot.rows, false);
                self.panes.pane.raster_dirty_rows[row] = true;
                self.panes.pane.surface_dirty_rows[row] = true;
            }
            if self.surface.configured {
                self.schedule_draw(queue_handle)?;
            }
        }
        Ok(())
    }

    fn schedule_draw(&mut self, queue_handle: &QueueHandle<Self>) -> Result<()> {
        if self.scheduling.frame_pending {
            self.scheduling.redraw_pending = true;
            Ok(())
        } else {
            self.draw(queue_handle)
        }
    }

    fn schedule_terminal_draw(&mut self, queue_handle: &QueueHandle<Self>) -> Result<()> {
        self.scheduling.terminal_redraw_pending = true;
        let draw_capacity_available = self.surface.buffers.len() < MAX_SHM_BUFFERS
            || self
                .surface
                .buffers
                .iter()
                .skip(1)
                .any(|buffer| self.surface.pool.canvas(&buffer.buffer).is_some());
        if terminal_draw_waits_for_frame(self.scheduling.frame_pending, draw_capacity_available) {
            self.scheduling.redraw_pending = true;
            Ok(())
        } else {
            // Terminal damage is coalesced while a compositor frame callback is
            // pending so bursty PTY redraws cannot render obsolete intermediate
            // states faster than they can be presented.
            self.draw(queue_handle)
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "SHM acquisition, persistent backing updates, damage submission, and commit form one transaction"
    )]
    fn draw(&mut self, queue_handle: &QueueHandle<Self>) -> Result<()> {
        self.scheduling.redraw_pending = false;
        // A trusted dialog's text is laid out for this size before it is
        // drawn, so drawing and hit testing share one layout.
        self.relayout_trusted_dialog()?;
        self.reconcile_command_palette_close(queue_handle);
        let terminal_priority = std::mem::take(&mut self.scheduling.terminal_redraw_pending);
        let draw_started = Instant::now();
        let scroll_started = self.panes.pane.scroll_started_at.take();
        if self.panes.pane.viewport_dirty {
            let display = self
                .panes
                .display_snapshot()
                .context("scroll display snapshot")?;
            let current_offset = self.panes.pane.scrollback_viewport.offset_from_bottom();
            let delta = isize::try_from(current_offset)
                .ok()
                .zip(isize::try_from(self.panes.pane.rendered_viewport_offset).ok())
                .map(|(current, rendered)| current - rendered);
            // Output prepared but not yet painted means the pixels lag the
            // frame; shifting them would carry the old rows along.
            let unpainted = self.panes.pane.prepare_dirty_rows.contains(&true)
                || self.panes.pane.raster_dirty_rows.contains(&true)
                || self.panes.pane.surface_dirty_rows.contains(&true)
                || !self.panes.pane.pending_scrolls.is_empty();
            self.panes.pane.prepare_dirty_rows.fill(false);
            self.panes.pane.raster_dirty_rows.fill(false);
            self.panes.pane.surface_dirty_rows.fill(false);
            self.panes.pane.pending_scrolls.clear();
            // Shifting the drawn frame is right only when the rows it keeps
            // are the rows the view now shows there. Anything else that
            // moved the view since the last frame needs a full rebuild.
            let shifted_rows_match = !unpainted
                && delta.is_some_and(|delta| {
                    viewport_shift_matches(
                        &self.panes.pane.rendered_row_ids,
                        &row_ids(&display),
                        delta,
                    )
                });
            let incremental = if display.images.is_none() && shifted_rows_match {
                if let (Some(frame), Some(delta)) = (&mut self.panes.pane.snapshot_frame, delta) {
                    frame.scroll_viewport_rows_with_context(
                        &display,
                        delta,
                        &self.presentation.render_context,
                    )?
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(scroll) = incremental {
                let count = scroll.rows.min(display.rows);
                let exposed = match scroll.direction {
                    splinterm_protocol::ScrollDirection::Forward => {
                        display.rows.saturating_sub(count)..display.rows
                    }
                    splinterm_protocol::ScrollDirection::Reverse => 0..count,
                };
                self.panes
                    .pane
                    .raster_dirty_rows
                    .resize(display.rows, false);
                self.panes
                    .pane
                    .surface_dirty_rows
                    .resize(display.rows, false);
                for row in exposed {
                    self.panes.pane.raster_dirty_rows[row] = true;
                }
                self.panes.pane.surface_dirty_rows.fill(true);
                self.panes.pane.pending_scrolls.push(scroll);
            } else {
                self.panes.pane.snapshot_frame =
                    Some(SnapshotFrame::load_scaled_with_sources_and_context(
                        &display,
                        self.surface.scale_120,
                        Some(&self.panes.pane.image_sources),
                        &self.presentation.render_context,
                    )?);
                self.presentation.full_redraw = true;
            }
            self.panes.pane.rendered_viewport_offset = current_offset;
            self.panes.pane.rendered_row_ids = row_ids(&display);
            self.panes.pane.viewport_dirty = false;
        }
        for pane in &mut self.panes.inactive_panes {
            if rebuild_dirty_pane_viewport_frame_with_context(
                pane,
                self.surface.scale_120,
                &self.presentation.render_context,
            )? {
                self.presentation.full_redraw = true;
            }
        }
        let explorer_layout = self.take_explorer_layout();
        if let Some(layout) = &explorer_layout {
            self.presentation.explorer_visible_start = layout.visible_start;
            if let Some(agents) = &layout.agents {
                self.presentation.explorer_agents_start = agents.visible_start;
            }
        }
        self.presentation
            .explorer_layout
            .clone_from(&explorer_layout);
        let pane_layout = self.computed_pane_layout();
        let (pane_cell_width, pane_cell_height) = self
            .panes
            .pane
            .snapshot_frame
            .as_ref()
            .map_or((1, 1), |frame| (frame.cell_width(), frame.cell_height()));
        self.prepare_frame_titles(pane_layout.as_ref(), pane_cell_width, pane_cell_height)?;
        let active_splint = self.panes.focused_splint();
        let active_rect = active_splint.and_then(|splint_id| {
            pane_layout
                .as_ref()
                .and_then(|layout| layout.rect(splint_id))
        });
        let window_geometry = if let Some(rect) = active_rect {
            Self::pane_geometry(&self.panes.pane, rect, self.surface.scale_120)
        } else {
            let content = self.content_rect();
            self.panes
                .pane
                .snapshot_frame
                .as_ref()
                .map_or(Ok(None), |frame| {
                    self.panes
                        .pane
                        .window_geometry(
                            frame,
                            content.width,
                            content.height,
                            self.surface.scale_120,
                        )?
                        .translated(
                            logical_extent_to_buffer(content.x, self.surface.scale_120)?,
                            logical_extent_to_buffer(content.y, self.surface.scale_120)?,
                        )
                        .map(Some)
                })
        }
        .or_else(|error| {
            if error.to_string().contains("SurfaceTooSmall") {
                Ok(None)
            } else {
                Err(error)
            }
        })?;
        let (width, height, stride) = window_buffer_dimensions(
            pane_layout.is_some(),
            self.tab_state.managed_tabs,
            self.tab_state.remote_display_identity.is_some(),
            (self.surface.logical_width, self.surface.logical_height),
            self.surface.scale_120,
            window_geometry,
        )?;
        let width_i32 = i32::try_from(width).context("buffer width fits i32")?;
        let height_i32 = i32::try_from(height).context("buffer height fits i32")?;
        let resolved_selection = self.panes.pane.selection.and_then(|selection| {
            let snapshot = self.panes.pane.snapshot.as_ref()?;
            let screen = self.panes.pane.selection_screen()?;
            let display = self.panes.display_snapshot_cow()?;
            selection_display_bounds(snapshot, screen, &display, selection)
                .map(|(start, end)| ((start.row, start.column), (end.row, end.column)))
        });
        let inline_picker_open = self.modal.inline_picker_open();
        let command_palette_open = self.modal.command_palette.is_some();
        let dojo_prompt_open = self.modal.dojo_prompt.is_some();
        let tab_context_menu_open = self.modal.tab_context_menu.is_some();
        let picker_layout = if inline_picker_open {
            let content = self.content_rect();
            let picker = self
                .modal
                .session_picker
                .as_mut()
                .context("inline picker exists")?;
            let (item_count, selected, visible_start) = picker.layout_state();
            let layout = session_picker_overlay_layout(
                content.width,
                content.height,
                self.surface.scale_120,
                item_count,
                selected,
                visible_start,
            )
            .map(|layout| translate_picker_layout(layout, content));
            if let Some(layout) = &layout {
                picker.set_visible_start(layout.visible_range.start);
            }
            layout
        } else {
            None
        };
        let command_palette_layout = if let Some(access) = self.modal.agent_access.as_ref() {
            let placeholders = vec![BuiltInCommandId::RecentSessions; access.rows().len()];
            command_palette_layout(
                self.content_rect(),
                &placeholders,
                access.selected_index(),
                access.visible_start(),
            )
        } else if let Some(help) = self.modal.binding_help.as_ref() {
            let placeholders = vec![BuiltInCommandId::RecentSessions; help.rows().len()];
            command_palette_layout(
                self.content_rect(),
                &placeholders,
                help.selected_index(),
                help.visible_start(),
            )
        } else {
            self.modal.command_palette.as_ref().and_then(|palette| {
                command_palette_layout(
                    self.content_rect(),
                    palette.filtered(),
                    palette.selected_index(),
                    palette.visible_start(),
                )
            })
        };
        let dojo_prompt_layout = self
            .modal
            .dojo_prompt
            .as_ref()
            .and_then(|prompt| dojo_prompt_layout(self.content_rect(), prompt));
        let tab_context_menu_layout = if let Some(menu) = self.modal.tab_context_menu.as_ref() {
            let label_width = self
                .modal
                .tab_context_menu_text_cache
                .tab_menu_label_width(
                    &self.presentation.render_context,
                    menu.actions(),
                    self.surface.scale_120,
                    self.presentation.renderer_generation,
                )?;
            tab_context_menu_layout(
                Rect {
                    x: 0,
                    y: 0,
                    width: self.surface.logical_width,
                    height: self.surface.logical_height,
                },
                self.modal.tab_context_menu_anchor,
                menu.actions(),
                label_width,
            )
        } else {
            None
        };
        self.prepare_remote_host_text()?;
        let tab_layout = self.current_tab_strip_layout();
        let content_rect = self.content_rect();
        let content_buffer_rect = Self::buffer_rect(content_rect, self.surface.scale_120)?;
        let active_dojo = self.tab_state.active_dojo_id();
        if let Some(layout) = &tab_layout {
            self.prepare_tab_strip_text(layout)?;
        }
        self.tab_state.tab_strip_layout.clone_from(&tab_layout);
        let terminal_cursor_blink = presented_cursor_visible(
            inline_picker_open || command_palette_open || dojo_prompt_open || tab_context_menu_open,
            self.input.cursor_blink_visible,
        );
        let terminal_keyboard_focused = self.input.keyboard_focused
            && !inline_picker_open
            && !command_palette_open
            && !dojo_prompt_open
            && !tab_context_menu_open
            && !self.explorer.focused();
        let backing_len = bounded_window_backing_len(width, height)?;
        let choice = choose_draw_buffer(self.surface.buffers.len(), |index| {
            self.surface
                .pool
                .canvas(&self.surface.buffers[index].buffer)
                .is_some()
        });
        let buffer_index = if let DrawBufferChoice::Reuse(index) = choice {
            index
        } else if choice == DrawBufferChoice::Allocate {
            let buffer = self
                .surface
                .pool
                .create_buffer(width_i32, height_i32, stride, wl_shm::Format::Argb8888)
                .context("create bounded SHM buffer")?
                .0;
            self.surface.buffers.push(ShmFrameBuffer {
                buffer,
                stale: BackingDamage::Full,
            });
            self.surface.buffers.len() - 1
        } else {
            self.scheduling.redraw_pending = true;
            self.scheduling.terminal_redraw_pending = terminal_priority;
            return Ok(());
        };
        // While part of a dialog's text is hidden it cannot say yes, and
        // its green area is dimmed.
        let proceed_enabled = self
            .modal
            .trusted_consent
            .as_ref()
            .is_none_or(|consent| !consent.clipped());
        // Below the dialog's text, which must never be covered while it fits.
        let trusted_action_top = self
            .modal
            .trusted_consent
            .is_some()
            .then(|| self.trusted_consent_action_top(width, height, window_geometry.as_ref()));
        let canvas = self
            .surface
            .pool
            .canvas(&self.surface.buffers[buffer_index].buffer)
            .context("selected SHM buffer became unavailable")?;

        let resized_backing = self.surface.backing.len() != backing_len;
        if resized_backing {
            self.surface.backing.resize(backing_len, 0);
            self.presentation.full_redraw = true;
            for buffer in &mut self.surface.buffers {
                buffer.stale.mark_full();
            }
        }
        let mut copied_backing_bytes = 0;
        let mut inactive_damage_regions = Vec::new();
        let backing_scroll_changed = !self.panes.pane.pending_scrolls.is_empty();
        let capture_minimum_images = capture_minimum_images()?;
        let capture_image_count = self
            .panes
            .pane
            .snapshot_frame
            .as_ref()
            .map_or(0, SnapshotFrame::image_count)
            .saturating_add(
                self.panes
                    .inactive_panes
                    .iter()
                    .filter_map(|pane| pane.snapshot_frame.as_ref())
                    .map(SnapshotFrame::image_count)
                    .sum(),
            );
        let image_composition_started = (std::env::var_os("SPLINTERM_IMAGE_TRACE").is_some()
            && capture_image_count > 0)
            .then(Instant::now);
        if let (Some(frame), Some(geometry)) = (&self.panes.pane.snapshot_frame, &window_geometry) {
            if self.presentation.capture.is_some()
                && capture_minimum_images > 0
                && capture_image_count >= capture_minimum_images
            {
                self.presentation.full_redraw = true;
            }
            if self.presentation.full_redraw {
                if let Some(layout) = pane_layout.as_ref() {
                    let [_, red, green, blue] = self.presentation.theme.background.to_be_bytes();
                    let background = background_bgra(
                        [red, green, blue],
                        self.presentation.render_context.background_alpha(),
                    );
                    for pixel in self.surface.backing.chunks_exact_mut(4) {
                        pixel.copy_from_slice(&background);
                    }
                    for pane in &self.panes.inactive_panes {
                        let Some(splint_id) =
                            pane.snapshot.as_ref().map(|snapshot| snapshot.splint_id)
                        else {
                            continue;
                        };
                        let Some(rect) = layout.rect(splint_id) else {
                            continue;
                        };
                        let (Some(frame), Some(geometry)) = (
                            &pane.snapshot_frame,
                            Self::pane_geometry(pane, rect, self.surface.scale_120)?,
                        ) else {
                            continue;
                        };
                        paint_snapshot_region_presented(
                            &mut self.surface.backing,
                            width,
                            height,
                            frame,
                            &geometry,
                            Self::buffer_rect(rect, self.surface.scale_120)?,
                            terminal_cursor_blink,
                            self.presentation.cursor_style,
                            CursorPresentation::INACTIVE_PANE,
                        );
                    }
                    paint_snapshot_region_presented(
                        &mut self.surface.backing,
                        width,
                        height,
                        frame,
                        geometry,
                        Self::buffer_rect(
                            active_rect.context("active pane rectangle")?,
                            self.surface.scale_120,
                        )?,
                        terminal_cursor_blink,
                        self.presentation.cursor_style,
                        CursorPresentation::for_keyboard_focus(terminal_keyboard_focused),
                    );
                } else {
                    paint_snapshot_presented(
                        &mut self.surface.backing,
                        width,
                        height,
                        frame,
                        geometry,
                        terminal_cursor_blink,
                        self.presentation.cursor_style,
                        CursorPresentation::for_keyboard_focus(terminal_keyboard_focused),
                    );
                }
            } else {
                for scroll in self.panes.pane.pending_scrolls.drain(..) {
                    scroll_snapshot_pixels(
                        &mut self.surface.backing,
                        width,
                        frame,
                        geometry,
                        scroll,
                    );
                }
                paint_snapshot_rows_presented(
                    &mut self.surface.backing,
                    width,
                    height,
                    frame,
                    geometry,
                    &self.panes.pane.raster_dirty_rows,
                    terminal_cursor_blink,
                    self.presentation.cursor_style,
                    CursorPresentation::for_keyboard_focus(terminal_keyboard_focused),
                );
                if let Some(layout) = pane_layout.as_ref() {
                    for pane in &self.panes.inactive_panes {
                        let Some(splint_id) = pane
                            .snapshot
                            .as_ref()
                            .map(|snapshot| snapshot.splint_id)
                            .filter(|splint_id| {
                                self.panes.dirty_inactive_panes.contains(splint_id)
                            })
                        else {
                            continue;
                        };
                        let Some(rect) = layout.rect(splint_id) else {
                            continue;
                        };
                        let (Some(frame), Some(geometry)) = (
                            &pane.snapshot_frame,
                            Self::pane_geometry(pane, rect, self.surface.scale_120)?,
                        ) else {
                            continue;
                        };
                        let region = Self::buffer_rect(rect, self.surface.scale_120)?;
                        paint_snapshot_region_presented(
                            &mut self.surface.backing,
                            width,
                            height,
                            frame,
                            &geometry,
                            region,
                            terminal_cursor_blink,
                            self.presentation.cursor_style,
                            CursorPresentation::INACTIVE_PANE,
                        );
                        inactive_damage_regions.push(region);
                    }
                }
            }
            let selection = resolved_selection;
            let hovered_url = self
                .panes
                .pane
                .hovered_url
                .as_ref()
                .map(|(start, end, _)| ((start.row, start.column), (end.row, end.column)));
            let history_status = history_overlay_status(
                &self.panes.pane.scrollback_viewport,
                self.panes.pane.snapshot.as_ref(),
            );
            // Pane chrome is deterministically repainted after synchronization, so
            // row scanline copies may overwrite it without contaminating reuse.
            let overlay_rows = transient_overlay_rows(
                self.panes
                    .pane
                    .snapshot
                    .as_ref()
                    .map_or(0, |snapshot| snapshot.rows),
                selection,
                hovered_url,
            );
            let full_transient_canvas_content = history_status.is_some()
                || self.modal.trusted_consent.is_some()
                || inline_picker_open
                || command_palette_open
                || dojo_prompt_open
                || tab_context_menu_open
                || explorer_layout.is_some()
                || self.panes.pane.search.input.is_some();
            let full_backing_sync =
                self.presentation.full_redraw || capture_image_count > 0 || backing_scroll_changed;
            for buffer in &mut self.surface.buffers {
                if full_backing_sync {
                    buffer.stale.mark_full();
                } else {
                    buffer.stale.mark_rows(&self.panes.pane.raster_dirty_rows);
                    buffer.stale.mark_regions(&inactive_damage_regions);
                }
            }
            let stale = std::mem::replace(
                &mut self.surface.buffers[buffer_index].stale,
                BackingDamage::Clean,
            );
            copied_backing_bytes = sync_backing_damage(
                canvas,
                &self.surface.backing,
                width,
                height,
                window_geometry.as_ref(),
                &stale,
            );
            paint_snapshot_overlays(
                canvas,
                width,
                height,
                frame,
                geometry,
                SnapshotOverlays {
                    selection,
                    hovered_url,
                    dirty_rows: None,
                    focused: terminal_keyboard_focused,
                    selection_color: self.presentation.theme.selection,
                    selection_foreground: self.presentation.theme.selection_foreground,
                    url_color: self.presentation.theme.url,
                    accent_color: self.presentation.theme.roles.accent,
                },
            );
            if let Some(layout) = pane_layout.as_ref() {
                paint_pane_chrome(
                    canvas,
                    width,
                    height,
                    layout,
                    active_splint,
                    self.presentation.theme,
                    frame.cell_width(),
                    frame.cell_height(),
                    self.surface.scale_120,
                    &self.presentation.frame_titles,
                )?;
            }
            if let Some(status) = history_overlay_status(
                &self.panes.pane.scrollback_viewport,
                self.panes.pane.snapshot.as_ref(),
            ) {
                // The digits are text on the raised surface.
                let roles = self.presentation.theme.roles;
                let digits = crate::color::contrast_corrected(roles.accent, roles.surface_raised);
                paint_history_overlay(
                    canvas,
                    width,
                    height,
                    content_buffer_rect,
                    self.surface.scale_120,
                    status,
                    chrome_panel_fill(
                        roles.surface_raised,
                        ChromeSurface::Popup.shell_alpha(self.presentation.theme.shell),
                        self.presentation.theme.background_alpha,
                        &[digits],
                    ),
                    digits,
                );
            }
            if let Some(button_top) = trusted_action_top {
                paint_trusted_consent_chrome(
                    canvas,
                    width,
                    height,
                    button_top,
                    TrustedConsentColors::from_theme(self.presentation.theme),
                    proceed_enabled,
                );
            }
            if full_transient_canvas_content {
                self.surface.buffers[buffer_index].stale.mark_full();
            } else {
                self.surface.buffers[buffer_index]
                    .stale
                    .mark_rows(&overlay_rows);
            }
        } else if self.panes.pane.snapshot_frame.is_some() {
            let [_, red, green, blue] = self.presentation.theme.background.to_be_bytes();
            let background = background_bgra(
                [red, green, blue],
                self.presentation.render_context.background_alpha(),
            );
            for pixel in self.surface.backing.chunks_exact_mut(4) {
                pixel.copy_from_slice(&background);
            }
            for buffer in &mut self.surface.buffers {
                buffer.stale.mark_full();
            }
            let stale = std::mem::replace(
                &mut self.surface.buffers[buffer_index].stale,
                BackingDamage::Clean,
            );
            copied_backing_bytes =
                sync_backing_damage(canvas, &self.surface.backing, width, height, None, &stale);
        } else if let Some(row) = &self.presentation.text_row {
            paint(canvas, width, height, row);
            self.surface.buffers[buffer_index].stale.mark_full();
        } else {
            anyhow::bail!("window has no prepared renderer content");
        }
        if let Some(layout) = tab_layout.as_ref() {
            Self::paint_tab_strip(
                canvas,
                width,
                height,
                layout,
                self.surface.scale_120,
                self.presentation.theme,
                self.tab_state.agent.colors(),
                active_dojo,
                &self.tab_state.tab_label_cache,
                self.tab_state.tab_close_text.as_ref().map(|(_, text)| text),
                self.tab_state.tab_new_text.as_ref().map(|(_, text)| text),
                self.tab_state
                    .remote_host_clipped
                    .as_ref()
                    .map(|cached| &cached.text),
                tabs::tab_strip_shows_border(
                    self.presentation.tab_strip_border,
                    self.tab_state.managed_tabs,
                    self.tab_state.tab_strip_visible,
                )
                .then_some((pane_cell_width, pane_cell_height)),
            )?;
            self.surface.buffers[buffer_index].stale.mark_full();
        }
        // The agent spinner moves only for a working row this draw shows.
        self.tab_state.agent.explorer_working = false;
        if let Some(layout) = explorer_layout.as_ref() {
            let rows = self.explorer.rows();
            let agents = self.explorer.agent_rows();
            self.tab_state.agent.explorer_working =
                crate::renderer::lair_explorer_shows_working(layout, &rows, &agents);
            let status_message = self.explorer.status_message();
            paint_lair_explorer(
                &mut self.presentation.explorer_text_cache,
                &self.presentation.render_context,
                canvas,
                width,
                height,
                self.surface.scale_120,
                self.presentation.renderer_generation,
                layout,
                crate::renderer::lair_explorer_palette(
                    self.presentation.theme,
                    self.tab_state.agent.colors(),
                ),
                &rows,
                self.explorer.tree_selected(),
                (&agents, self.explorer.agent_selected()),
                self.explorer.query(),
                status_message,
                self.explorer.focused(),
                self.explorer.filter(),
                self.explorer.search_active(),
            )?;
            self.surface.buffers[buffer_index].stale.mark_full();
        }
        let mut search_panel = None;
        if let Some(input) = self.panes.pane.search.input.as_ref() {
            if let Some(terminal) = search_paint_rect(content_rect, explorer_layout.as_ref()) {
                let terminal = Self::buffer_rect(terminal, self.surface.scale_120)?;
                search_panel = search_overlay_panel(terminal, self.surface.scale_120);
                paint_search_overlay(
                    &mut self.presentation.search_text_cache,
                    &self.presentation.render_context,
                    canvas,
                    width,
                    height,
                    terminal,
                    self.surface.scale_120,
                    self.presentation.renderer_generation,
                    chrome_panel_palette(self.presentation.theme, ChromeSurface::Popup),
                    input.text(),
                    &self.panes.pane.search.query,
                    self.panes.pane.search.matches.len(),
                    self.panes.pane.search.selected,
                    self.panes.pane.search.searching,
                )?;
            }
            self.surface.buffers[buffer_index].stale.mark_full();
        }
        // The upgrade banner: inside the terminal area, below the tab strip,
        // clear of the history overlay, and beneath interactive popups.
        if let Some(text) = self.handoff.banner_text() {
            if let Some(terminal) = search_paint_rect(content_rect, explorer_layout.as_ref()) {
                let history = history_overlay_status(
                    &self.panes.pane.scrollback_viewport,
                    self.panes.pane.snapshot.as_ref(),
                )
                .and_then(|_| {
                    history_overlay_layout(
                        content_buffer_rect.width,
                        content_buffer_rect.height,
                        self.surface.scale_120,
                    )
                })
                .map(|layout| {
                    let (x, y, width, height) = layout.panel;
                    Rect {
                        x: content_buffer_rect
                            .x
                            .saturating_add(u32::try_from(x).unwrap_or(0)),
                        y: content_buffer_rect
                            .y
                            .saturating_add(u32::try_from(y).unwrap_or(0)),
                        width,
                        height,
                    }
                });
                paint_handoff_banner(
                    &mut self.handoff.text_cache,
                    &self.presentation.render_context,
                    canvas,
                    width,
                    height,
                    Self::buffer_rect(terminal, self.surface.scale_120)?,
                    history,
                    self.surface.scale_120,
                    self.presentation.renderer_generation,
                    chrome_panel_palette(self.presentation.theme, ChromeSurface::Popup),
                    &text,
                )?;
            }
            self.surface.buffers[buffer_index].stale.mark_full();
        }
        if let (Some(layout), Some(picker)) =
            (picker_layout.as_ref(), self.modal.session_picker.as_ref())
        {
            let items = picker
                .items()
                .map(|item| SessionPickerTextItem {
                    display_title: &item.display_title,
                    breadcrumb: &item.breadcrumb,
                    working_directory: &item.working_directory,
                    pane_count: item.pane_count,
                    running_pane_count: item.running_pane_count,
                })
                .collect::<Vec<_>>();
            paint_session_picker_overlay(
                &mut self.modal.session_picker_text_cache,
                &self.presentation.render_context,
                canvas,
                width,
                height,
                content_buffer_rect,
                self.surface.scale_120,
                self.presentation.renderer_generation,
                layout,
                chrome_panel_palette(self.presentation.theme, ChromeSurface::Menu),
                picker
                    .purpose()
                    .unwrap_or(session_picker_purpose(self.modal.selector_kind)),
                &items,
                picker.new_enabled(),
                picker.new_blocker(),
                picker.search_active(),
                picker.query(),
                picker.selected_target(),
                picker.hovered(),
                self.modal.session_picker_pressed,
                self.input.keyboard_focused,
            )?;
            self.surface.buffers[buffer_index].stale.mark_full();
        }
        if let (Some(layout), Some(palette)) = (
            command_palette_layout.as_ref(),
            self.modal.command_palette.as_ref(),
        ) {
            paint_command_palette(
                &mut self.modal.command_palette_text_cache,
                &self.presentation.render_context,
                canvas,
                width,
                height,
                content_rect,
                self.surface.scale_120,
                self.presentation.renderer_generation,
                layout,
                chrome_panel_palette(self.presentation.theme, ChromeSurface::Menu),
                palette,
                &self.input.keymap,
                self.modal.command_palette_pressed,
                self.input.keyboard_focused,
                if let Some(access) = self.modal.agent_access.as_ref() {
                    PaletteBody::AgentAccess(access)
                } else if let Some(help) = self.modal.binding_help.as_ref() {
                    PaletteBody::BindingHelp(help)
                } else {
                    PaletteBody::Commands
                },
            )?;
            self.surface.buffers[buffer_index].stale.mark_full();
        }
        self.modal.command_palette_layout = command_palette_layout;
        if let (Some(layout), Some(prompt)) =
            (dojo_prompt_layout.as_ref(), self.modal.dojo_prompt.as_ref())
        {
            paint_dojo_prompt(
                &mut self.modal.dojo_prompt_text_cache,
                &self.presentation.render_context,
                canvas,
                width,
                height,
                content_rect,
                self.surface.scale_120,
                self.presentation.renderer_generation,
                layout,
                chrome_panel_palette(self.presentation.theme, ChromeSurface::Popup),
                prompt,
                self.input.keyboard_focused,
            )?;
            self.surface.buffers[buffer_index].stale.mark_full();
        }
        self.modal.dojo_prompt_layout = dojo_prompt_layout;
        if let (Some(layout), Some(menu)) = (
            tab_context_menu_layout.as_ref(),
            self.modal.tab_context_menu.as_ref(),
        ) {
            paint_tab_context_menu(
                &mut self.modal.tab_context_menu_text_cache,
                &self.presentation.render_context,
                canvas,
                width,
                height,
                self.surface.scale_120,
                self.presentation.renderer_generation,
                layout,
                chrome_panel_palette(self.presentation.theme, ChromeSurface::Popup),
                menu,
                self.modal.tab_context_menu_pressed,
                self.input.keyboard_focused,
            )?;
            self.surface.buffers[buffer_index].stale.mark_full();
        }
        self.modal.tab_context_menu_layout = tab_context_menu_layout;
        if let Some(started) = image_composition_started {
            eprintln!(
                "phase5-image-trace composition_ns={} image_count={capture_image_count}",
                started.elapsed().as_nanos(),
            );
        }
        let capture_scale_ready = self
            .presentation
            .capture_scale
            .is_none_or(|expected| expected.saturating_mul(120) == self.surface.scale_120);
        if deterministic_capture_ready(
            capture_scale_ready,
            capture_minimum_images,
            capture_image_count,
        ) && let Some(path) = self.presentation.capture.take()
        {
            write_ppm(&path, canvas, width, height)
                .with_context(|| format!("write {}", path.display()))?;
            eprintln!(
                "Wrote deterministic row capture at {}x scale to {}",
                f64::from(self.surface.scale_120) / 120.0,
                path.display()
            );
        }
        let history_status = history_overlay_status(
            &self.panes.pane.scrollback_viewport,
            self.panes.pane.snapshot.as_ref(),
        );
        // A spinner frame changes only chrome, which partial damage misses.
        let spinner_damage = self.tab_state.agent.take_chrome_damage();
        let picker_damage_full_surface = std::mem::take(&mut self.modal.session_picker_redraw);
        let damage_full_surface = picker_damage_full_surface
            || take_full_surface_damage(
                &mut self.presentation.full_redraw,
                self.panes.pane.snapshot_frame.is_some(),
            );
        if damage_full_surface {
            self.surface
                .window
                .wl_surface()
                .damage_buffer(0, 0, width_i32, height_i32);
        } else {
            if let Some(geometry) = &window_geometry {
                for (row, dirty) in self
                    .panes
                    .pane
                    .surface_dirty_rows
                    .iter()
                    .copied()
                    .enumerate()
                {
                    if !dirty {
                        continue;
                    }
                    if let Some((x, y, row_width, row_height)) = snapshot_row_rect(geometry, row) {
                        self.surface
                            .window
                            .wl_surface()
                            .damage_buffer(x, y, row_width, row_height);
                    }
                }
            }
            for region in &inactive_damage_regions {
                self.surface.window.wl_surface().damage_buffer(
                    i32::try_from(region.x).unwrap_or(i32::MAX),
                    i32::try_from(region.y).unwrap_or(i32::MAX),
                    i32::try_from(region.width).unwrap_or(i32::MAX),
                    i32::try_from(region.height).unwrap_or(i32::MAX),
                );
            }
            let spinner_regions = [
                tab_layout.as_ref().map(|layout| layout.rect),
                explorer_layout.as_ref().map(|layout| layout.panel),
            ];
            for region in spinner_regions
                .into_iter()
                .flatten()
                .filter(|_| spinner_damage)
            {
                let region = Self::buffer_rect(region, self.surface.scale_120)?;
                self.surface.window.wl_surface().damage_buffer(
                    i32::try_from(region.x).unwrap_or(i32::MAX),
                    i32::try_from(region.y).unwrap_or(i32::MAX),
                    i32::try_from(region.width).unwrap_or(i32::MAX),
                    i32::try_from(region.height).unwrap_or(i32::MAX),
                );
            }
        }
        if history_status != self.panes.pane.painted_history_status {
            let content = Self::buffer_rect(self.content_rect(), self.surface.scale_120)?;
            if let Some(layout) =
                history_overlay_layout(content.width, content.height, self.surface.scale_120)
            {
                let (x, y, overlay_width, overlay_height) = layout.panel;
                self.surface.window.wl_surface().damage_buffer(
                    x.saturating_add(i32::try_from(content.x).unwrap_or(i32::MAX)),
                    y.saturating_add(i32::try_from(content.y).unwrap_or(i32::MAX)),
                    i32::try_from(overlay_width).unwrap_or(i32::MAX),
                    i32::try_from(overlay_height).unwrap_or(i32::MAX),
                );
            }
        }
        self.panes.pane.painted_history_status = history_status;
        // The search field is chrome over the terminal: partial damage covers
        // only changed rows, so damage where it is and where it was.
        if !damage_full_surface {
            for region in search_panel_damage(self.presentation.painted_search_panel, search_panel)
            {
                self.surface.window.wl_surface().damage_buffer(
                    i32::try_from(region.x).unwrap_or(i32::MAX),
                    i32::try_from(region.y).unwrap_or(i32::MAX),
                    i32::try_from(region.width).unwrap_or(i32::MAX),
                    i32::try_from(region.height).unwrap_or(i32::MAX),
                );
            }
        }
        self.presentation.painted_search_panel = search_panel;
        self.panes.pane.raster_dirty_rows.fill(false);
        self.panes.pane.surface_dirty_rows.fill(false);
        self.panes.pane.pending_scrolls.clear();
        if self
            .surface
            .background_effect_reconcile_schedule
            .take_for_draw()
        {
            self.reconcile_background_effect(
                queue_handle,
                BackgroundEffectCommitMode::DeferToDraw,
            )?;
        }
        let committed_identity = self
            .panes
            .pane
            .snapshot
            .as_ref()
            .map(|snapshot| (snapshot.splint_id, snapshot.incarnation, snapshot.revision));
        let pane_commit_traces = if perf_trace_enabled() {
            pending_pane_commit_traces(&self.panes.pane, &self.panes.inactive_panes)
        } else {
            Vec::new()
        };
        let commit_sequence = perf_trace_enabled().then(|| {
            let sequence = self.scheduling.next_commit_sequence;
            self.scheduling.next_commit_sequence = sequence.saturating_add(1);
            sequence
        });
        let requests_frame_callback = !self.scheduling.frame_pending;
        if requests_frame_callback {
            self.surface
                .window
                .wl_surface()
                .frame(queue_handle, self.surface.window.wl_surface().clone());
            self.scheduling.frame_pending = true;
        }
        self.surface.buffers[buffer_index]
            .buffer
            .attach_to(self.surface.window.wl_surface())
            .context("attach SHM buffer")?;
        self.surface.window.commit();
        // Move the submitted buffer and its stale-damage state together. The
        // next draw may use the previous submission, but never this one.
        self.surface.buffers.swap(0, buffer_index);
        if let Some(diagnostics) = diagnostics() {
            diagnostics.mark_window_mapped();
        }
        if let Some(mapped) = self.surface.mapped_notify.take() {
            let _ = mapped.send(());
        }
        let draw_duration_ns = u64::try_from(draw_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let committed_monotonic_raw_ns = commit_sequence.map(|_| monotonic_raw_ns());
        if let (Some(commit_sequence), Some(committed_monotonic_raw_ns)) =
            (commit_sequence, committed_monotonic_raw_ns)
        {
            let snapshot = self.panes.pane.snapshot.as_ref();
            emit_perf_trace_at(
                "splinterm",
                "draw_commit",
                PerfTraceEvent {
                    commit_sequence: Some(commit_sequence),
                    duration_ns: Some(draw_duration_ns),
                    bytes: Some(u64::try_from(copied_backing_bytes).unwrap_or(u64::MAX)),
                    rows: snapshot.map(|snapshot| u64::try_from(snapshot.rows).unwrap_or(u64::MAX)),
                    pane_count: Some(
                        u64::try_from(self.panes.inactive_panes.len().saturating_add(1))
                            .unwrap_or(u64::MAX),
                    ),
                    backing_copy_bytes: Some(
                        u64::try_from(copied_backing_bytes).unwrap_or(u64::MAX),
                    ),
                    full_reload: Some(damage_full_surface),
                    ..PerfTraceEvent::default()
                },
                committed_monotonic_raw_ns,
            );
            for trace in &pane_commit_traces {
                emit_perf_trace_at(
                    "splinterm",
                    "pane_commit",
                    pane_commit_event(*trace, commit_sequence),
                    committed_monotonic_raw_ns,
                );
            }
            if requests_frame_callback {
                self.scheduling.pending_frame_trace = Some(PendingFrameTrace {
                    commit_sequence,
                    committed_monotonic_raw_ns,
                });
            }
        }
        if self.panes.pane.trace_correlation.is_some() {
            self.panes.pane.clear_trace_correlation();
        }
        for pane in &mut self.panes.inactive_panes {
            if pane.trace_correlation.is_some() {
                pane.clear_trace_correlation();
            }
        }
        self.panes.dirty_inactive_panes.clear();
        self.modal.session_picker_layout = picker_layout;
        self.complete_background_effect_draw_commit()?;
        let inject_graphical_input = self
            .scheduling
            .graphical_input_probe
            .as_mut()
            .is_some_and(|probe| probe.observe_commit(committed_identity));
        if inject_graphical_input {
            self.scheduling.graphical_input_probe = None;
            self.send_input(vec![b'q'])?;
            if perf_trace_enabled() {
                emit_perf_trace(
                    "splinterm",
                    "graphical_input",
                    PerfTraceEvent {
                        splint_id: committed_identity.map(|identity| identity.0),
                        incarnation: committed_identity.map(|identity| identity.1),
                        revision: committed_identity.map(|identity| identity.2),
                        bytes: Some(1),
                        count: Some(1),
                        ..PerfTraceEvent::default()
                    },
                );
            }
        }
        if self.scheduling.scroll_trace
            && let Some(scroll_started) = scroll_started
        {
            eprintln!(
                "scroll-trace input_to_commit_us={} draw_us={} viewport_offset={} cached_rows={} page_pending={}",
                scroll_started.elapsed().as_micros(),
                draw_started.elapsed().as_micros(),
                self.panes.pane.scrollback_viewport.offset_from_bottom(),
                self.panes
                    .pane
                    .snapshot
                    .as_ref()
                    .map_or(0, |snapshot| snapshot.scrollback_rows.len()),
                self.panes.pane.history_page_pending,
            );
        }
        Ok(())
    }
}

fn write_selection_payload(write_pipe: WritePipe, payload: Arc<[u8]>) {
    let Some(permit) = try_clipboard_worker(&ACTIVE_CLIPBOARD_WORKERS) else {
        return;
    };
    std::thread::spawn(move || {
        let _permit = permit;
        let fd = OwnedFd::from(write_pipe);
        let _ = write_clipboard_with_deadline(&fd, &payload, CLIPBOARD_IO_TIMEOUT);
    });
}

#[cfg(test)]
mod tests {
    /// A footer action token (`new`, `menu`, `«`) and the rail header's `»`
    /// never take keyboard focus; a press on a row, of the tree, of the
    /// agents section, or of the rail, or on a filter gives the Explorer the
    /// keys as before. A token runs only when its press and release both
    /// land on it.
    #[test]
    fn explorer_action_press_keeps_focus_and_needs_a_matching_release() {
        use super::{
            ExplorerPointerPress, LairExplorerAction, LairExplorerFilter, explorer_left_press,
            explorer_released_action,
        };
        use crate::navigation_projection::NavigationNodeId;

        let lair = NavigationNodeId::Lair(splinterm_core::LairId::new());
        let splint = NavigationNodeId::Splint {
            lair_id: splinterm_core::LairId::new(),
            dojo_id: splinterm_core::DojoId::new(),
            splint_id: splinterm_core::SplintId::new(),
        };
        for action in [
            LairExplorerAction::New,
            LairExplorerAction::Menu,
            LairExplorerAction::Toggle,
        ] {
            // A token press wins over whatever else is under it, and leaves
            // the keys where they are.
            assert_eq!(
                explorer_left_press(
                    Some(action),
                    true,
                    Some(LairExplorerFilter::Saved),
                    (Some(lair), None),
                    Some(splint)
                ),
                (Some(ExplorerPointerPress::Action(action)), false)
            );
            let pressed = Some(ExplorerPointerPress::Action(action));
            assert_eq!(
                explorer_released_action(pressed, Some(action)),
                Some(action)
            );
            assert_eq!(explorer_released_action(pressed, None), None);
            assert_eq!(explorer_released_action(None, Some(action)), None);
            assert_eq!(
                explorer_released_action(
                    Some(ExplorerPointerPress::Row(lair, false)),
                    Some(action)
                ),
                None
            );
        }
        // A press on one token and a release on another runs neither.
        assert_eq!(
            explorer_released_action(
                Some(ExplorerPointerPress::Action(LairExplorerAction::New)),
                Some(LairExplorerAction::Menu)
            ),
            None
        );
        assert_eq!(
            explorer_left_press(None, false, None, (Some(lair), None), Some(splint)),
            (Some(ExplorerPointerPress::Row(lair, false)), true)
        );
        assert_eq!(
            explorer_left_press(None, false, None, (Some(lair), Some(lair)), None),
            (Some(ExplorerPointerPress::Row(lair, true)), true)
        );
        assert_eq!(
            explorer_left_press(
                None,
                false,
                Some(LairExplorerFilter::Saved),
                (None, None),
                Some(splint)
            ),
            (
                Some(ExplorerPointerPress::Filter(LairExplorerFilter::Saved)),
                true
            )
        );
        // An agents row takes the keys as a tree row does.
        assert_eq!(
            explorer_left_press(None, false, None, (None, None), Some(splint)),
            (Some(ExplorerPointerPress::Agent(splint)), true)
        );
        // Empty panel space still gives the Explorer the keys.
        assert_eq!(
            explorer_left_press(None, false, None, (None, None), None),
            (None, true)
        );
        // A near miss on the footer, between the tokens or on the context
        // line, or a press on the agents label line, leaves the keys where
        // they are too: neither is rows.
        assert_eq!(
            explorer_left_press(None, true, None, (None, None), None),
            (None, false)
        );
    }

    /// A click on an agents row completes only when the press and the
    /// release land on the same row; a press there never completes a tree
    /// row, a filter, or an action, nor does a press elsewhere complete it.
    #[test]
    fn agents_row_click_needs_a_matching_release() {
        use super::{
            ExplorerPointerPress, LairExplorerAction, LairExplorerFilter, explorer_released_action,
            explorer_released_agent, explorer_released_filter,
        };
        use crate::navigation_projection::NavigationNodeId;

        let splint = |_| NavigationNodeId::Splint {
            lair_id: splinterm_core::LairId::new(),
            dojo_id: splinterm_core::DojoId::new(),
            splint_id: splinterm_core::SplintId::new(),
        };
        let (agent, other) = (splint(0), splint(1));
        let pressed = Some(ExplorerPointerPress::Agent(agent));
        assert_eq!(explorer_released_agent(pressed, Some(agent)), Some(agent));
        assert_eq!(explorer_released_agent(pressed, Some(other)), None);
        assert_eq!(explorer_released_agent(pressed, None), None);
        assert_eq!(explorer_released_agent(None, Some(agent)), None);
        assert_eq!(
            explorer_released_agent(Some(ExplorerPointerPress::Row(agent, false)), Some(agent)),
            None
        );
        assert_eq!(
            explorer_released_filter(pressed, Some(LairExplorerFilter::Live)),
            None
        );
        assert_eq!(
            explorer_released_action(pressed, Some(LairExplorerAction::New)),
            None
        );
    }

    /// `new` makes a Lair and `«` / `»` toggles the rail, neither asking
    /// about the selected row's menu. `menu` opens the selected row's own
    /// menu when it has one, the action menu (the command palette) with no
    /// row selected or a row with nothing on its menu, and nothing at all
    /// while the selected row's menu is unavailable for now.
    #[test]
    fn explorer_actions_dispatch_their_commands() {
        use super::{
            ExplorerActionCommand, ExplorerRowMenu, LairExplorerAction, explorer_action_command,
        };

        for (action, command) in [
            (LairExplorerAction::New, ExplorerActionCommand::NewLair),
            (LairExplorerAction::Toggle, ExplorerActionCommand::Toggle),
        ] {
            assert_eq!(
                explorer_action_command(action, || panic!("{action:?} asked for a menu")),
                command
            );
        }
        for (row_menu, command) in [
            (ExplorerRowMenu::Ready, ExplorerActionCommand::RowMenu),
            (ExplorerRowMenu::NoSelection, ExplorerActionCommand::Palette),
            (ExplorerRowMenu::Empty, ExplorerActionCommand::Palette),
            (ExplorerRowMenu::Unavailable, ExplorerActionCommand::Nothing),
        ] {
            assert_eq!(
                explorer_action_command(LairExplorerAction::Menu, || row_menu),
                command,
                "{row_menu:?}"
            );
        }
    }

    #[test]
    fn explorer_filter_gestures_require_matching_release_and_tree_key_ownership() {
        use super::{
            ExplorerPointerPress, Keysym, LairExplorerFilter, Modifiers, explorer_filter_key,
            explorer_released_filter,
        };
        let pressed = Some(ExplorerPointerPress::Filter(LairExplorerFilter::Live));
        assert_eq!(
            explorer_released_filter(pressed, Some(LairExplorerFilter::Live)),
            Some(LairExplorerFilter::Live)
        );
        assert_eq!(
            explorer_released_filter(pressed, Some(LairExplorerFilter::Saved)),
            None
        );
        assert_eq!(explorer_released_filter(pressed, None), None);
        assert_eq!(
            explorer_released_filter(None, Some(LairExplorerFilter::Live)),
            None
        );
        let row = Some(ExplorerPointerPress::Row(
            crate::navigation_projection::NavigationNodeId::Lair(splinterm_core::LairId::new()),
            false,
        ));
        assert_eq!(
            explorer_released_filter(row, Some(LairExplorerFilter::Live)),
            None
        );
        assert!(explorer_filter_key(Keysym::f, Modifiers::default(), false));
        assert!(explorer_filter_key(Keysym::F, Modifiers::default(), false));
        assert!(!explorer_filter_key(Keysym::f, Modifiers::default(), true));
        assert!(!explorer_filter_key(Keysym::r, Modifiers::default(), false));
        for modifiers in [
            Modifiers {
                ctrl: true,
                ..Modifiers::default()
            },
            Modifiers {
                alt: true,
                ..Modifiers::default()
            },
            Modifiers {
                logo: true,
                ..Modifiers::default()
            },
        ] {
            assert!(!explorer_filter_key(Keysym::f, modifiers, false));
        }
    }

    #[test]
    fn explorer_keyboard_context_menu_accepts_only_menu_and_shift_f10() {
        use super::{Keysym, Modifiers, explorer_context_menu_key};
        let mut modifiers = Modifiers::default();
        assert!(explorer_context_menu_key(Keysym::Menu, modifiers));
        assert!(!explorer_context_menu_key(Keysym::F10, modifiers));
        modifiers.shift = true;
        assert!(explorer_context_menu_key(Keysym::F10, modifiers));
        assert!(!explorer_context_menu_key(Keysym::Menu, modifiers));
        modifiers.ctrl = true;
        assert!(!explorer_context_menu_key(Keysym::F10, modifiers));
        modifiers.ctrl = false;
        modifiers.alt = true;
        assert!(!explorer_context_menu_key(Keysym::F10, modifiers));
        modifiers.alt = false;
        modifiers.logo = true;
        assert!(!explorer_context_menu_key(Keysym::F10, modifiers));
    }

    #[test]
    fn context_menu_release_rejection_is_consumed_not_a_dispatch_failure() {
        for reason in [
            "tab menu is unavailable",
            "tab menu target is unavailable",
            "tab menu topology commands are unavailable",
        ] {
            let mut attempts = 0;
            let consumed = super::consume_context_menu_request(|| {
                attempts += 1;
                anyhow::bail!(reason)
            });
            assert!(
                consumed,
                "rejected menu release must not reach terminal input"
            );
            assert_eq!(attempts, 1);
        }
    }

    #[test]
    fn context_menu_release_success_opens_once_and_is_consumed() {
        let mut attempts = 0;
        assert!(super::consume_context_menu_request(|| {
            attempts += 1;
            Ok(())
        }));
        assert_eq!(attempts, 1);
    }

    use std::{env, fs, path::PathBuf};

    use super::*;
    use sha2::{Digest, Sha256};
    use splinterm_automation_client::{ImageContentSource, SharedImageContentCache};
    use splinterm_core::{Axis, Splint, SplitRatio};
    use splinterm_protocol::{
        ActiveScreen, ImageAlphaMode, ImageContentMetadata, ImageRetention, ImageSourceFormat,
    };

    fn update_test_waker() -> (EventLoop<'static, usize>, Waker) {
        let event_loop = EventLoop::<usize>::try_new().unwrap();
        let (ping, source) = make_ping().unwrap();
        event_loop
            .handle()
            .insert_source(source, |(), (), wake_count| *wake_count += 1)
            .unwrap();
        (event_loop, Waker::from(Arc::new(UpdateWake(ping))))
    }

    #[test]
    fn explorer_focus_blocks_terminal_ime_until_focus_returns() {
        assert!(terminal_ime_allowed(true, false, false, None));
        assert!(!terminal_ime_allowed(true, false, true, None));
        assert!(!terminal_ime_allowed(true, true, false, None));
        assert!(!terminal_ime_allowed(false, false, false, None));
        assert!(terminal_ime_allowed(
            true,
            true,
            true,
            Some(OwnedFieldTarget::CommandPalette)
        ));
        assert!(!terminal_ime_allowed(
            false,
            true,
            true,
            Some(OwnedFieldTarget::BindingHelp)
        ));
        assert!(ime_batch_blocked(false, true, false, None));
        assert!(ime_batch_blocked(false, false, true, None));
        assert!(!ime_batch_blocked(false, false, false, None));
        assert!(!ime_batch_blocked(
            true,
            true,
            false,
            Some(OwnedFieldTarget::BindingHelp)
        ));
        assert!(ime_batch_blocked(
            true,
            true,
            true,
            Some(OwnedFieldTarget::BindingHelp)
        ));
        assert_eq!(
            picker_ime_reconcile(true, true, true),
            PickerImeReconcile::Renew
        );
    }

    #[test]
    fn exact_splint_activation_distinguishes_local_failure_from_already_focused() {
        let target = SplintId::new();
        let other = SplintId::new();
        // A matching snapshot is not sufficient: the owner operation may fail.
        assert_eq!(
            attempt_exact_splint_focus(target, true, false, || (false, Some(other))),
            None
        );
        assert_eq!(
            attempt_exact_splint_focus(target, true, false, || (false, None)),
            None
        );
        assert_eq!(
            attempt_exact_splint_focus(target, true, false, || (false, Some(target))),
            Some(false)
        );
        assert_eq!(
            attempt_exact_splint_focus(target, true, false, || (true, Some(target))),
            Some(true)
        );
        assert_eq!(
            attempt_exact_splint_focus(target, true, true, || panic!(
                "pending remote focus attempted"
            )),
            None
        );
        assert_eq!(
            attempt_exact_splint_focus(target, false, false, || panic!(
                "stale incarnation focus attempted"
            )),
            None
        );
    }

    #[test]
    fn focused_explorer_returns_to_terminal_only_for_an_outside_primary_press() {
        let outside_press = PointerEventKind::Press {
            button: BTN_LEFT,
            serial: 7,
            time: 11,
        };
        assert!(explorer_pointer_returns_to_terminal(
            true,
            false,
            &outside_press
        ));
        assert!(!explorer_pointer_returns_to_terminal(
            false,
            false,
            &outside_press
        ));
        assert!(!explorer_pointer_returns_to_terminal(
            true,
            true,
            &outside_press
        ));
        assert!(!explorer_pointer_returns_to_terminal(
            true,
            false,
            &PointerEventKind::Motion { time: 12 }
        ));
    }

    #[test]
    fn owned_fields_route_palette_typing_above_explorer_search() {
        let mut palette = BoundedTextEditor::new(String::new(), 128, 128, true);
        let mut explorer = BoundedTextEditor::new("keep".into(), 128, 128, true);
        let mut terminal = BoundedTextEditor::new("terminal".into(), 128, 128, true);
        let (target, editor) = App::select_owned_field(
            true,
            false,
            None,
            Some(&mut palette),
            None,
            Some(&mut explorer),
            Some(&mut terminal),
        )
        .unwrap();
        assert_eq!(target, OwnedFieldTarget::CommandPalette);
        assert!(editor.insert("Toggle Lair explorer"));
        assert_eq!(palette.text(), "Toggle Lair explorer");
        assert_eq!(explorer.text(), "keep");
        assert_eq!(terminal.text(), "terminal");
    }

    #[test]
    fn owned_fields_route_prompt_typing_above_background_searches() {
        let mut prompt = BoundedTextEditor::new(String::new(), 128, 128, true);
        let mut explorer = BoundedTextEditor::new("keep".into(), 128, 128, true);
        let mut terminal = BoundedTextEditor::new("terminal".into(), 128, 128, true);
        let (target, editor) = App::select_owned_field(
            true,
            false,
            None,
            None,
            Some(&mut prompt),
            Some(&mut explorer),
            Some(&mut terminal),
        )
        .unwrap();
        assert_eq!(target, OwnedFieldTarget::DojoPrompt);
        assert!(editor.insert("New name"));
        assert_eq!(prompt.text(), "New name");
        assert_eq!(explorer.text(), "keep");
        assert_eq!(terminal.text(), "terminal");
    }

    #[test]
    fn owned_fields_nonediting_modal_blocks_background_searches() {
        for consent_or_picker_open in [false, true] {
            for explorer_active in [false, true] {
                for terminal_active in [false, true] {
                    let mut explorer = BoundedTextEditor::new("keep".into(), 128, 128, true);
                    let mut terminal = BoundedTextEditor::new("terminal".into(), 128, 128, true);
                    assert!(
                        App::select_owned_field(
                            true,
                            consent_or_picker_open,
                            None,
                            None,
                            None,
                            explorer_active.then_some(&mut explorer),
                            terminal_active.then_some(&mut terminal),
                        )
                        .is_none()
                    );
                    assert_eq!(explorer.text(), "keep");
                    assert_eq!(terminal.text(), "terminal");
                }
            }
        }
    }

    #[test]
    fn owned_fields_consent_or_picker_keeps_terminal_search_and_blocks_explorer() {
        let mut explorer = BoundedTextEditor::new("keep".into(), 128, 128, true);
        let mut terminal = BoundedTextEditor::new(String::new(), 128, 128, true);
        let (target, editor) = App::select_owned_field(
            false,
            true,
            None,
            None,
            None,
            Some(&mut explorer),
            Some(&mut terminal),
        )
        .unwrap();
        assert_eq!(target, OwnedFieldTarget::Search);
        assert!(editor.insert("needle"));
        assert_eq!(terminal.text(), "needle");
        assert_eq!(explorer.text(), "keep");
        let mut explorer = BoundedTextEditor::new("keep".into(), 128, 128, true);
        assert!(
            App::select_owned_field(false, true, None, None, None, Some(&mut explorer), None)
                .is_none()
        );
        assert_eq!(explorer.text(), "keep");
    }

    #[test]
    fn owned_fields_restore_background_search_priority_without_a_modal() {
        let mut explorer = BoundedTextEditor::new(String::new(), 128, 128, true);
        let mut terminal = BoundedTextEditor::new(String::new(), 128, 128, true);
        let (target, editor) = App::select_owned_field(
            false,
            false,
            None,
            None,
            None,
            Some(&mut explorer),
            Some(&mut terminal),
        )
        .unwrap();
        assert_eq!(target, OwnedFieldTarget::ExplorerSearch);
        assert!(editor.insert("explorer"));
        assert_eq!(terminal.text(), "");
        let (target, editor) =
            App::select_owned_field(false, false, None, None, None, None, Some(&mut terminal))
                .unwrap();
        assert_eq!(target, OwnedFieldTarget::Search);
        assert!(editor.insert("terminal"));
        assert_eq!(explorer.text(), "explorer");
        assert_eq!(terminal.text(), "terminal");
        assert!(
            App::select_owned_field::<&mut BoundedTextEditor>(
                false, false, None, None, None, None, None
            )
            .is_none()
        );
    }

    /// Runs the production `ime_gate` and returns the IME target, whether IME
    /// may be enabled with keyboard focus, and whether a batch without the
    /// fresh-generation barrier is dropped. A dropped batch never reaches
    /// `WindowCommand::Input`; an undropped batch with no target is the only
    /// IME path that writes to the PTY.
    fn ime_route(context: ImeContext) -> (Option<OwnedFieldTarget>, bool, bool) {
        let gate = ime_gate(context);
        (gate.target, gate.allowed(true), gate.batch_blocked(false))
    }

    #[test]
    fn ime_composes_into_terminal_search_under_consent_or_non_inline_picker() {
        // Trusted consent and a non-inline picker are the same modal layer.
        let search_under_prompt = ImeContext {
            consent_or_picker_open: true,
            terminal_search: true,
            ..ImeContext::default()
        };
        assert_eq!(
            ime_route(search_under_prompt),
            (Some(OwnedFieldTarget::Search), true, false)
        );
        // The fresh-generation barrier still drops the first batch.
        assert!(ime_gate(search_under_prompt).batch_blocked(true));
        // Keyboard focus is still required.
        assert!(!ime_gate(search_under_prompt).allowed(false));
        // Explorer focus still blocks composition beneath them.
        assert_eq!(
            ime_route(ImeContext {
                explorer_focused: true,
                ..search_under_prompt
            }),
            (None, false, true)
        );
        // An inline picker is an input modal and keeps search composition blocked.
        assert_eq!(
            ime_route(ImeContext {
                input_modal_open: true,
                terminal_search: true,
                ..ImeContext::default()
            }),
            (None, false, true)
        );
    }

    #[test]
    fn ime_never_reaches_the_pty_beneath_a_consent_prompt() {
        for explorer_focused in [false, true] {
            let gate = ime_gate(ImeContext {
                consent_or_picker_open: true,
                explorer_focused,
                ..ImeContext::default()
            });
            assert_eq!(gate.target, None);
            assert!(!gate.allowed(true));
            assert!(gate.batch_blocked(false));
        }
        // Without a modal the PTY remains the IME destination.
        assert_eq!(ime_route(ImeContext::default()), (None, true, false));
        assert!(terminal_input_blocked(false, true, false));
    }

    #[test]
    fn ime_keeps_explicit_field_priority_over_terminal_search() {
        for consent_or_picker_open in [false, true] {
            for explorer_focused in [false, true] {
                let base = ImeContext {
                    input_modal_open: true,
                    consent_or_picker_open,
                    explorer_focused,
                    terminal_search: true,
                    ..ImeContext::default()
                };
                assert_eq!(
                    ime_route(ImeContext {
                        binding_help: true,
                        command_palette: true,
                        dojo_prompt_input: true,
                        ..base
                    }),
                    (Some(OwnedFieldTarget::BindingHelp), true, false)
                );
                assert_eq!(
                    ime_route(ImeContext {
                        command_palette: true,
                        dojo_prompt_input: true,
                        ..base
                    }),
                    (Some(OwnedFieldTarget::CommandPalette), true, false)
                );
                assert_eq!(
                    ime_route(ImeContext {
                        dojo_prompt_input: true,
                        ..base
                    }),
                    (Some(OwnedFieldTarget::DojoPrompt), true, false)
                );
            }
        }
    }

    #[test]
    fn terminal_input_blocked_behind_input_modals_and_pending_consent() {
        assert!(!terminal_input_blocked(false, false, false));
        assert!(terminal_input_blocked(true, false, false));
        assert!(terminal_input_blocked(false, true, false));
        assert!(terminal_input_blocked(true, true, false));
        // A clipboard read finishing under a pending consent prompt is stale.
        assert!(!clipboard_read_is_current(
            terminal_input_blocked(false, true, false),
            7,
            7
        ));
    }

    #[test]
    fn owned_fields_defer_modal_control_keys_before_text_editing() {
        let plain = Modifiers::default();
        for target in [
            OwnedFieldTarget::BindingHelp,
            OwnedFieldTarget::CommandPalette,
        ] {
            for keysym in [
                Keysym::Up,
                Keysym::Down,
                Keysym::Home,
                Keysym::End,
                Keysym::Page_Up,
                Keysym::Page_Down,
                Keysym::Return,
                Keysym::KP_Enter,
                Keysym::Escape,
            ] {
                assert!(App::owned_field_defers_to_modal(target, keysym, plain));
            }
        }
        for target in [
            OwnedFieldTarget::DojoPrompt,
            OwnedFieldTarget::ExplorerSearch,
            OwnedFieldTarget::Search,
        ] {
            for keysym in [Keysym::Return, Keysym::KP_Enter, Keysym::Escape] {
                assert!(App::owned_field_defers_to_modal(target, keysym, plain));
            }
            assert!(!App::owned_field_defers_to_modal(
                target,
                Keysym::Left,
                plain
            ));
            assert!(!App::owned_field_defers_to_modal(
                target,
                Keysym::BackSpace,
                plain
            ));
        }
        for target in [
            OwnedFieldTarget::BindingHelp,
            OwnedFieldTarget::CommandPalette,
        ] {
            assert!(!App::owned_field_defers_to_modal(
                target,
                Keysym::BackSpace,
                plain
            ));
            assert!(!App::owned_field_defers_to_modal(
                target,
                Keysym::Left,
                plain
            ));
        }
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        assert!(App::owned_field_defers_to_modal(
            OwnedFieldTarget::BindingHelp,
            Keysym::u,
            ctrl
        ));
        assert!(!App::owned_field_defers_to_modal(
            OwnedFieldTarget::CommandPalette,
            Keysym::u,
            ctrl
        ));
    }

    #[test]
    fn owned_fields_accept_printable_unicode_but_reject_empty_and_control_text() {
        assert_eq!(App::editable_field_text(Some("alpha 界")), Some("alpha 界"));
        assert_eq!(App::editable_field_text(Some("")), None);
        assert_eq!(App::editable_field_text(Some("\r")), None);
        assert_eq!(App::editable_field_text(Some("\u{1b}")), None);
        assert_eq!(App::editable_field_text(Some("a\tb")), None);
        assert_eq!(App::editable_field_text(None), None);
    }

    #[test]
    fn hidden_tab_strip_reclaims_managed_window_height() {
        // The default strip is the stock Omarchy bar's 26 px.
        assert_eq!(
            tab_strip_logical_height(ResolvedTheme::default()),
            DEFAULT_TAB_STRIP_LOGICAL_HEIGHT
        );
        assert_eq!(DEFAULT_TAB_STRIP_LOGICAL_HEIGHT, 26);
        for strip in [DEFAULT_TAB_STRIP_LOGICAL_HEIGHT, 20, 34, 48] {
            assert_eq!(
                tab_strip_height(true, true, false, strip, strip + 20),
                strip
            );
            assert_eq!(
                tab_strip_height(true, true, false, strip, strip - 1),
                strip - 1
            );
            for managed in [false, true] {
                for visible in [false, true] {
                    assert_eq!(tab_strip_height(managed, visible, true, strip, 200), strip);
                    assert_eq!(tab_strip_height(managed, visible, true, strip, 10), 10);
                    assert_eq!(tab_strip_height(managed, visible, true, strip, 0), 0);
                }
            }
            assert_eq!(tab_strip_height(true, false, false, strip, 200), 0);
            assert_eq!(tab_strip_height(false, true, false, strip, 200), 0);
        }
    }

    #[test]
    fn a_live_bar_size_change_relayouts_the_tab_strip() {
        let current = ResolvedTheme::default();
        let mut taller = current;
        taller.shell.bar_size_px = 30;
        // Like hiding or showing the strip, it moves the content area and
        // repaints no pane pixels.
        assert_eq!(
            classify_theme_update(current, taller),
            ThemeUpdateImpact {
                rebuild_pixels: false,
                reconcile_effect: false,
                relayout_tab_strip: true,
                redraw_chrome: false,
            }
        );
        // A palette change alongside it still rebuilds pixels.
        let mut repainted = taller;
        repainted.foreground ^= 0x00ff_ffff;
        let impact = classify_theme_update(current, repainted);
        assert!(impact.relayout_tab_strip && impact.rebuild_pixels);
        // Other shell settings leave the strip where it is.
        let mut panels = current;
        panels.shell.menu_alpha = u16::MAX / 2;
        assert!(!classify_theme_update(current, panels).relayout_tab_strip);
        let mut text = current;
        text.shell.text_size_px = Some(16);
        assert!(!classify_theme_update(current, text).relayout_tab_strip);
        // A base-size edit reaches the strip through the bar size the shell
        // settings resolve with it (round(26 × 16/12) = 35 px).
        let mut scaled = text;
        scaled.shell.bar_size_px = 35;
        let impact = classify_theme_update(current, scaled);
        assert!(impact.relayout_tab_strip && !impact.rebuild_pixels);
    }

    #[test]
    fn a_strip_relayout_resizes_the_panes_once_per_frame() {
        // omarchy-display-text-size 16 changes base-size: the strip relayout
        // and the followed text size land in one pass. The font
        // reconciliation already resized every pane from the new content
        // area, so the strip adds no second resize.
        assert!(!strip_relayout_needs_resize(true, true, true));
        // A bar edit alone, or a font change that was rejected (at maximum
        // zoom) or left the raster unchanged, still resizes the panes.
        assert!(strip_relayout_needs_resize(true, false, true));
        // No strip change, or no configured surface yet: nothing to resize.
        assert!(!strip_relayout_needs_resize(false, false, true));
        assert!(!strip_relayout_needs_resize(false, true, true));
        assert!(!strip_relayout_needs_resize(true, false, false));
    }

    #[test]
    fn picker_retry_requests_preserve_kind_and_original_lair_scope() {
        assert!(matches!(
            session_picker_retry_request(&WindowTopologyCommand::RequestSessionPicker),
            Some(WindowTopologyCommand::RequestSessionPicker)
        ));
        for kind in [SelectorKind::Dojo, SelectorKind::Lair] {
            let lair_id = LairId::new();
            let request = WindowTopologyCommand::RequestSelector { kind, lair_id };
            assert!(matches!(
                session_picker_retry_request(&request),
                Some(WindowTopologyCommand::RequestSelector {
                    kind: retry_kind,
                    lair_id: retry_lair,
                }) if retry_kind == kind && retry_lair == lair_id
            ));
        }
        assert!(
            session_picker_retry_request(&WindowTopologyCommand::NewLair {
                source_splint_id: SplintId::new()
            })
            .is_none()
        );
    }

    #[test]
    fn picker_hierarchy_routes_copy_and_new_actions_at_the_selected_level() {
        let lair_id = LairId::new();
        let source_splint_id = SplintId::new();
        assert_eq!(
            session_picker_purpose(None),
            SessionPickerPurpose::RecentDojos
        );
        assert_eq!(
            session_picker_purpose(Some(SelectorKind::Dojo)),
            SessionPickerPurpose::Dojos
        );
        assert_eq!(
            session_picker_purpose(Some(SelectorKind::Lair)),
            SessionPickerPurpose::Lairs
        );
        assert_eq!(session_picker_title(None), "Splinterm — Recent Dojos");
        assert_eq!(
            session_picker_title(Some(SelectorKind::Dojo)),
            "Splinterm — Dojos"
        );
        assert_eq!(
            session_picker_title(Some(SelectorKind::Lair)),
            "Splinterm — Lairs"
        );
        let revision = TopologyRevision::new(9);
        assert_eq!(
            session_picker_new_command(
                Some(SelectorKind::Dojo),
                lair_id,
                source_splint_id,
                SessionPickerCreationTarget {
                    topology_revision: revision,
                    action: NavigationAction::CreateDojo,
                },
            )
            .unwrap(),
            WindowTopologyCommand::PickerNewDojo {
                topology_revision: revision,
                lair_id,
                source_splint_id,
            }
        );
        for selector_kind in [None, Some(SelectorKind::Lair)] {
            assert_eq!(
                session_picker_new_command(
                    selector_kind,
                    lair_id,
                    source_splint_id,
                    SessionPickerCreationTarget {
                        topology_revision: revision,
                        action: NavigationAction::CreateLair,
                    },
                )
                .unwrap(),
                WindowTopologyCommand::PickerNewLair {
                    topology_revision: revision,
                    source_splint_id,
                }
            );
        }
    }

    #[test]
    fn final_tab_removal_hands_off_one_pending_picker_request() {
        assert_eq!(
            final_tab_removal_action(true, 0, true),
            FinalTabRemovalAction::ExitAndHandoffPicker
        );
        assert_eq!(
            final_tab_removal_action(true, 0, false),
            FinalTabRemovalAction::Exit
        );
        assert_eq!(
            final_tab_removal_action(true, 1, true),
            FinalTabRemovalAction::Continue
        );
        assert_eq!(
            final_tab_removal_action(false, 0, true),
            FinalTabRemovalAction::Continue
        );

        let command = session_picker_handoff_command(Path::new("/opt/splinterm"));
        assert_eq!(command.get_program(), "/opt/splinterm");
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["dojos"]);
    }

    #[test]
    fn a_closed_active_tab_hands_over_to_a_tab_whose_panes_are_ready() {
        let lair = splinterm_core::LairId::new();
        let (first, second, third) = (DojoId::new(), DojoId::new(), DojoId::new());
        let ready = |tab: &DojoTab<bool>| tab.value;

        let mut tabs = WindowTabSet::new(DojoTab::new(lair, first, true));
        tabs.open_or_activate(DojoTab::new(lair, second, false))
            .unwrap();
        tabs.open_or_activate(DojoTab::new(lair, third, true))
            .unwrap();
        assert!(tabs.activate(first));
        // The usual neighbour has no panes ready, so the next ready tab shows.
        assert_eq!(tab_after_close(&tabs, first, ready), Some(third));
        assert!(tabs.activate(third));
        assert_eq!(tab_after_close(&tabs, third, ready), Some(first));

        // With no other ready tab the usual neighbour stands, and activating
        // it reports the problem as before.
        let mut lone = WindowTabSet::new(DojoTab::new(lair, first, true));
        lone.open_or_activate(DojoTab::new(lair, second, false))
            .unwrap();
        assert!(lone.activate(first));
        assert_eq!(tab_after_close(&lone, first, ready), Some(second));

        let single = WindowTabSet::new(DojoTab::new(lair, first, true));
        assert_eq!(tab_after_close(&single, first, ready), None);
    }

    #[test]
    fn only_a_full_topology_queue_is_the_typed_busy_error() {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        try_topology_command(&sender, WindowTopologyCommand::CloseWindow).unwrap();
        let full = try_topology_command(&sender, WindowTopologyCommand::CloseWindow).unwrap_err();
        assert!(full.downcast_ref::<TopologyQueueFull>().is_some());
        drop(receiver);
        let closed = try_topology_command(&sender, WindowTopologyCommand::CloseWindow).unwrap_err();
        assert!(closed.downcast_ref::<TopologyQueueFull>().is_none());
    }

    #[test]
    fn an_unsent_divider_drag_puts_the_split_back_to_its_starting_ratio() {
        let first = Splint::shell(PathBuf::from("/tmp"));
        let first_id = first.id;
        let mut layout = LayoutNode::Branch {
            axis: Axis::Horizontal,
            ratio: SplitRatio::new(700).unwrap(),
            first: Box::new(LayoutNode::Leaf(first)),
            second: Box::new(LayoutNode::Leaf(Splint::shell(PathBuf::from("/")))),
        };
        let split = PaneSplit {
            axis: Axis::Horizontal,
            area: Rect {
                x: 0,
                y: 0,
                width: 100,
                height: 10,
            },
            boundary: 70,
            separator: 1,
            target: first_id,
            ancestor: 0,
        };
        let drag = DividerDrag {
            split,
            ratio: SplitRatio::new(700).ok(),
            original: SplitRatio::new(500).ok(),
        };
        assert!(restore_divider_ratio(Some(&mut layout), &drag));
        let LayoutNode::Branch { ratio, .. } = &layout else {
            panic!("expected a branch");
        };
        assert_eq!(ratio.get(), 500);
        assert!(
            !restore_divider_ratio(Some(&mut layout), &drag),
            "already restored"
        );
        let untouched = DividerDrag {
            original: None,
            ..drag
        };
        assert!(!restore_divider_ratio(Some(&mut layout), &untouched));
        assert!(!restore_divider_ratio(None, &drag));
    }

    #[test]
    fn one_pending_remote_split_blocks_more_splits_and_cannot_receive_focus() {
        let target = SplintId::new();
        let pending = SplintId::new();
        let mut splits = HashMap::new();
        assert!(remote_split_can_begin(&splits));
        assert!(!is_pending_remote_splint(&splits, target));

        splits.insert(target, pending);
        assert!(!remote_split_can_begin(&splits));
        assert!(is_pending_remote_splint(&splits, pending));
        assert!(!is_pending_remote_splint(&splits, target));
    }

    #[test]
    fn pending_remote_snapshot_is_valid_and_identifiable() {
        let splint_id = SplintId::new();
        let snapshot = pending_remote_snapshot(splint_id, 80, 24);

        snapshot.validate().unwrap();
        assert_eq!(snapshot.splint_id, splint_id);
        assert_eq!(snapshot.title, "Opening remote pane…");
        assert_eq!(
            snapshot.visible_rows[0]
                .cells
                .iter()
                .map(|cell| cell.content.as_str())
                .collect::<String>(),
            "Opening remote pane…"
        );
        assert!(!snapshot.input_modes.cursor_visible);
    }

    #[test]
    fn pending_split_inserts_second_leaf_without_changing_target_identity() {
        let target = Splint::shell(PathBuf::from("/tmp"));
        let target_id = target.id;
        let mut pending = Splint::shell(PathBuf::from("/"));
        let pending_id = pending.id;
        "Opening remote pane…".clone_into(&mut pending.title);
        let mut layout = LayoutNode::Leaf(target);

        assert!(insert_pending_split(
            &mut layout,
            target_id,
            pending,
            Axis::Horizontal,
        ));
        assert_eq!(layout.splint_count(), 2);
        assert!(layout.find_splint(target_id).is_some());
        assert_eq!(
            layout
                .find_splint(pending_id)
                .map(|splint| splint.title.as_str()),
            Some("Opening remote pane…")
        );
    }

    #[test]
    fn pending_split_rollback_collapses_only_the_placeholder_leaf() {
        let target = Splint::shell(PathBuf::from("/tmp"));
        let target_id = target.id;
        let sibling = Splint::shell(PathBuf::from("/tmp"));
        let sibling_id = sibling.id;
        let pending = Splint::shell(PathBuf::from("/"));
        let pending_id = pending.id;
        let mut split = LayoutNode::Leaf(target);
        assert!(insert_pending_split(
            &mut split,
            target_id,
            pending,
            Axis::Horizontal,
        ));
        let layout = LayoutNode::Branch {
            axis: Axis::Vertical,
            ratio: SplitRatio::new(500).unwrap(),
            first: Box::new(split),
            second: Box::new(LayoutNode::Leaf(sibling)),
        };

        let (restored, removed) = remove_pending_split(layout, pending_id);
        let restored = restored.unwrap();
        assert!(removed);
        assert_eq!(restored.splint_count(), 2);
        assert!(restored.find_splint(target_id).is_some());
        assert!(restored.find_splint(sibling_id).is_some());
        assert!(restored.find_splint(pending_id).is_none());
    }

    #[test]
    fn cursor_rectangle_offsets_into_pane() {
        assert_eq!(
            offset_cursor_rectangle(
                (7, 11, 8, 16),
                Rect {
                    x: 100,
                    y: 200,
                    width: 400,
                    height: 300,
                },
            ),
            Some((107, 211, 8, 16))
        );
    }

    #[test]
    fn picker_translation_moves_visual_surfaces_with_hit_slots_and_text() {
        let layout = session_picker_overlay_layout(960, 600, 120, 3, 1, 0).unwrap();
        let origin = Rect {
            x: 7,
            y: DEFAULT_TAB_STRIP_LOGICAL_HEIGHT,
            width: 960,
            height: 600,
        };
        let translated = translate_picker_layout(layout.clone(), origin);
        for (source, moved) in layout.rows.iter().zip(&translated.rows) {
            assert_eq!(moved.rect, translated_rect(source.rect, origin));
            assert_eq!(moved.surface, translated_rect(source.surface, origin));
            assert_eq!(moved.title_clip, translated_rect(source.title_clip, origin));
            assert_eq!(
                moved.metadata_clip,
                translated_rect(source.metadata_clip, origin)
            );
        }
    }

    #[test]
    fn blur_only_theme_updates_reconcile_immediately_without_queuing_pixel_work() {
        let current = ResolvedTheme::default();
        let mut blur_only = current;
        blur_only.background_blur = true;
        let impact = classify_theme_update(current, blur_only);
        assert_eq!(
            impact,
            ThemeUpdateImpact {
                rebuild_pixels: false,
                reconcile_effect: true,
                relayout_tab_strip: false,
                redraw_chrome: false,
            }
        );
        let mut schedule = BackgroundEffectReconcileSchedule::default();
        assert!(schedule.queue_update(impact.reconcile_effect, impact.rebuild_pixels, true));
        assert!(!schedule.on_draw);

        let mut alpha = blur_only;
        alpha.background_alpha = u16::MAX - 1;
        assert_eq!(
            classify_theme_update(blur_only, alpha),
            ThemeUpdateImpact {
                rebuild_pixels: true,
                reconcile_effect: true,
                relayout_tab_strip: false,
                redraw_chrome: false,
            }
        );

        let mut palette = blur_only;
        palette.foreground ^= 0x00ff_ffff;
        assert_eq!(
            classify_theme_update(blur_only, palette),
            ThemeUpdateImpact {
                rebuild_pixels: true,
                reconcile_effect: false,
                relayout_tab_strip: false,
                redraw_chrome: false,
            }
        );
        assert_eq!(
            classify_theme_update(current, current),
            ThemeUpdateImpact::default()
        );
    }

    #[test]
    fn shell_panel_alpha_updates_repaint_chrome_without_touching_the_effect() {
        let current = ResolvedTheme::default();
        for (menu_alpha, popup_alpha) in [(u16::MAX / 2, u16::MAX), (u16::MAX, u16::MAX / 2)] {
            let mut next = current;
            next.shell.menu_alpha = menu_alpha;
            next.shell.popup_alpha = popup_alpha;
            assert_eq!(
                classify_theme_update(current, next),
                ThemeUpdateImpact {
                    rebuild_pixels: true,
                    reconcile_effect: false,
                    relayout_tab_strip: false,
                    redraw_chrome: false,
                }
            );
        }
    }

    #[test]
    fn an_unparseable_base_size_cancels_a_rejected_followed_size_retry() {
        // 120 px was published and rejected at +40 zoom steps, so the window
        // still renders its applied 12 px. Resetting the zoom would retry it...
        let rejected = Some((Some(120), 40));
        assert!(followed_text_size_due(
            Some(120),
            false,
            Some(12),
            rejected,
            0
        ));
        // ...but once base-size turns unparseable the watcher republishes the
        // same request marked held, and the applied size stays.
        assert!(!followed_text_size_due(
            Some(120),
            true,
            Some(12),
            rejected,
            0
        ));
        assert!(!followed_text_size_due(Some(120), true, Some(12), None, 0));
        // The marker alone repaints nothing.
        let mut held = ResolvedTheme::default();
        held.shell.text_size_px = Some(120);
        let mut published = held;
        published.shell.text_size_held = true;
        assert_eq!(
            classify_theme_update(held, published),
            ThemeUpdateImpact::default()
        );
    }

    #[test]
    fn a_rejected_followed_text_size_is_retried_once_zoom_changes() {
        // Nothing to do while the followed size is the applied one.
        assert!(!followed_text_size_due(Some(16), false, Some(16), None, 0));
        assert!(followed_text_size_due(Some(16), false, Some(12), None, 0));
        // A size rejected at +40 zoom steps is not retried every frame...
        assert!(!followed_text_size_due(
            Some(120),
            false,
            Some(12),
            Some((Some(120), 40)),
            40
        ));
        // ...but is once the zoom changes or a different size is requested.
        assert!(followed_text_size_due(
            Some(120),
            false,
            Some(12),
            Some((Some(120), 40)),
            0
        ));
        assert!(followed_text_size_due(
            Some(14),
            false,
            Some(12),
            Some((Some(120), 40)),
            40
        ));

        // The render context side: 90 pt plus 40 half-point steps exceeds the
        // supported size and is rejected, then fits after the zoom resets.
        let mut context =
            RenderContext::new(u16::MAX).with_font_size_override(Some(FontSize::Points(9.0)));
        assert_eq!(context.set_font_zoom_steps(40, 120).unwrap(), Some(true));
        assert!(
            context
                .replace_font_size_override(Some(FontSize::Points(90.0)), 120)
                .is_err()
        );
        assert!(context.set_font_zoom_steps(0, 120).unwrap().is_some());
        assert!(
            context
                .replace_font_size_override(Some(FontSize::Points(90.0)), 120)
                .unwrap()
        );
    }

    #[test]
    fn followed_text_size_uses_omarchy_points_and_skips_pixel_rebuilds() {
        let current = ResolvedTheme::default();
        assert_eq!(followed_font_size(current), None);
        let mut next = current;
        for (px, points) in [(12, 9.0), (16, 12.0), (14, 11.0)] {
            next.shell.text_size_px = Some(px);
            assert_eq!(followed_font_size(next), Some(FontSize::Points(points)));
            // The text size goes through font reconciliation, not a palette
            // pixel rebuild.
            assert_eq!(
                classify_theme_update(current, next),
                ThemeUpdateImpact::default()
            );
            // A new window starts at the followed size: Foot's output-scale
            // sizing of the same point size.
            let context =
                RenderContext::new(u16::MAX).with_font_size_override(followed_font_size(next));
            for scale in [1.0_f32, 1.25, 2.0] {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let scale_120 = (scale * 120.0) as u32;
                assert_eq!(
                    context
                        .effective_font_resolution(scale_120)
                        .unwrap()
                        .pixel_size
                        .to_bits(),
                    ((points * (96.0 * scale) / 72.0 * 64.0).round() / 64.0).to_bits(),
                    "{px} px at {scale}x"
                );
            }
        }
    }

    /// Chrome text takes `[main] ui-font-pixelsize`, otherwise Omarchy's
    /// text size, otherwise 14 px, whatever the terminal follows; a chrome
    /// size change alone never rebuilds pane pixels.
    #[test]
    fn chrome_font_size_follows_the_ui_key_then_omarchy_never_the_terminal() {
        let current = ResolvedTheme::default();
        assert_eq!(chrome_font_size(None, current), FontSize::Pixels(14.0));
        let mut omarchy = current;
        omarchy.shell.ui_text_size_px = Some(16);
        // An explicit terminal font size clears only the terminal's size.
        omarchy.shell.text_size_px = None;
        assert_eq!(chrome_font_size(None, omarchy), FontSize::Pixels(16.0));
        assert_eq!(followed_font_size(omarchy), None);
        let key = Some(FontSize::Pixels(11.0));
        assert_eq!(chrome_font_size(key, omarchy), FontSize::Pixels(11.0));
        assert_eq!(chrome_font_size(key, current), FontSize::Pixels(11.0));
        assert_eq!(
            classify_theme_update(current, omarchy),
            ThemeUpdateImpact::default()
        );
        // The context the Window starts with draws chrome at that size, and
        // a live change replaces only the chrome size.
        let mut context = RenderContext::new(u16::MAX)
            .with_font_size_override(followed_font_size(omarchy))
            .with_chrome_font_size(chrome_font_size(None, omarchy));
        let terminal = context.effective_font_resolution(120).unwrap();
        omarchy.shell.ui_text_size_px = Some(18);
        assert!(context.replace_chrome_font_size(chrome_font_size(None, omarchy)));
        assert!(!context.replace_chrome_font_size(chrome_font_size(None, omarchy)));
        assert_eq!(context.effective_font_resolution(120).unwrap(), terminal);
    }

    #[test]
    fn omarchy_glassy_windows_toggle_changes_only_the_background_effect() {
        // Each glassy fixture is the plain one after Omarchy's
        // omarchy-theme-set-glass (4a9c83de) with Glassy Windows on.
        let fixtures = [
            (
                "stock-template",
                include_str!("../tests/fixtures/omarchy4/glass/stock-template.foot.ini"),
                include_str!("../tests/fixtures/omarchy4/glass/stock-template.glassy.foot.ini"),
                false,
            ),
            (
                "user-template",
                include_str!("../tests/fixtures/omarchy4/glass/user-template.foot.ini"),
                include_str!("../tests/fixtures/omarchy4/glass/user-template.glassy.foot.ini"),
                false,
            ),
            // The theme's own alpha=0.9 is kept; glass adds only blur.
            (
                "theme-alpha",
                include_str!("../tests/fixtures/omarchy4/glass/theme-alpha.foot.ini"),
                include_str!("../tests/fixtures/omarchy4/glass/theme-alpha.glassy.foot.ini"),
                true,
            ),
        ];
        let root = std::env::temp_dir().join(format!(
            "splinterm-glass-classification-{}",
            std::process::id()
        ));
        let load = |name: &str, foot: &str| {
            let theme = root.join(name);
            std::fs::create_dir_all(&theme).unwrap();
            std::fs::write(
                theme.join("colors.toml"),
                include_str!("../tests/fixtures/omarchy4/tokyo-night/colors.toml"),
            )
            .unwrap();
            std::fs::write(theme.join("foot.ini"), foot).unwrap();
            crate::config::load_omarchy_theme(&theme).unwrap()
        };
        for (name, plain, glassy, effect_only) in fixtures {
            let plain = load(&format!("{name}-plain"), plain);
            let glassy = load(&format!("{name}-glassy"), glassy);
            assert_ne!(plain, glassy, "{name}");
            assert_eq!(
                glassy.with_color_overrides(
                    Some(plain.background_alpha),
                    Some(plain.background_blur)
                ),
                plain,
                "{name}: glass must not change the palette"
            );
            for (from, to) in [(plain, glassy), (glassy, plain)] {
                let impact = classify_theme_update(from, to);
                assert!(impact.reconcile_effect, "{name}");
                // Only an alpha change repaints the premultiplied default
                // background; a blur-only change is effect-only.
                assert_eq!(impact.rebuild_pixels, !effect_only, "{name}");
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn draw_bound_effect_reconciliation_survives_later_updates_and_capabilities() {
        let mut schedule = BackgroundEffectReconcileSchedule::default();
        assert!(!schedule.queue_update(true, true, true));
        assert!(schedule.on_draw);

        assert!(!schedule.queue_update(true, false, true));
        assert!(schedule.on_draw);
        assert!(!schedule.capability_reconciles_immediately());

        assert!(schedule.take_for_draw());
        assert!(!schedule.on_draw);
        assert!(schedule.capability_reconciles_immediately());
        assert!(!schedule.take_for_draw());

        schedule.queue_geometry();
        assert!(schedule.on_draw);
        schedule.clear();
        assert!(!schedule.on_draw);
    }

    #[test]
    fn background_effect_dispatch_preserves_known_and_unknown_capability_bits() {
        assert_eq!(
            background_effect_capability_bits(WEnum::Value(BackgroundCapability::Blur)),
            crate::background_effect::BLUR_CAPABILITY
        );
        assert_eq!(
            background_effect_capability_bits(WEnum::Unknown(0x8000_0000)),
            0x8000_0000
        );
    }

    #[test]
    fn background_effect_diagnostics_and_trace_are_bounded_metadata_only() {
        let size = crate::background_effect::LogicalSize::new(960, 600).unwrap();
        assert_eq!(
            background_effect_trace_line(EffectAction::SetBlurRegion(size)).as_deref(),
            Some("splinterm background-effect region=960x600")
        );
        assert_eq!(
            background_effect_trace_line(EffectAction::CommitSurface(
                crate::background_effect::CommitReason::Enable
            ))
            .as_deref(),
            Some("splinterm background-effect commit=Enable")
        );
        assert!(
            background_effect_trace_line(EffectAction::Diagnostic(
                EffectDiagnostic::MissingManager
            ))
            .is_none()
        );
        for diagnostic in [
            EffectDiagnostic::MissingManager,
            EffectDiagnostic::MissingBlurCapability,
        ] {
            let message = background_effect_diagnostic_message(diagnostic);
            assert!(message.len() < 128);
            assert!(message.starts_with("splinterm background blur requested"));
        }
    }

    #[test]
    fn graphical_input_probe_counts_only_distinct_committed_revisions() {
        let splint_id = SplintId::new();
        let mut probe = GraphicalInputProbe {
            target_revisions: 3,
            observed_revisions: HashSet::with_capacity(3),
        };
        assert!(!probe.observe_commit(None));
        assert!(!probe.observe_commit(Some((splint_id, 1, 7))));
        assert!(!probe.observe_commit(Some((splint_id, 1, 8))));
        assert!(!probe.observe_commit(Some((splint_id, 1, 7))));
        assert!(probe.observe_commit(Some((splint_id, 1, 9))));
    }

    #[test]
    fn pending_update_receiver_wakes_calloop_and_coalesces_pings() {
        let mut event_loop = EventLoop::<usize>::try_new().unwrap();
        let (ping, source) = make_ping().unwrap();
        event_loop
            .handle()
            .insert_source(source, |(), (), wake_count| *wake_count += 1)
            .unwrap();
        let waker = Waker::from(Arc::new(UpdateWake(ping)));
        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);

        assert!(matches!(
            poll_receiver(&mut receiver, &waker),
            ReceiverPoll::Pending
        ));
        sender.try_send(1).unwrap();
        sender.try_send(2).unwrap();

        let mut wake_count = 0;
        event_loop
            .dispatch(Duration::ZERO, &mut wake_count)
            .unwrap();
        assert_eq!(wake_count, 1);
        assert!(matches!(
            poll_receiver(&mut receiver, &waker),
            ReceiverPoll::Item(1)
        ));
        assert!(matches!(
            poll_receiver(&mut receiver, &waker),
            ReceiverPoll::Item(2)
        ));
        assert!(matches!(
            poll_receiver(&mut receiver, &waker),
            ReceiverPoll::Pending
        ));
    }

    #[test]
    fn receiver_drain_yields_at_budget_and_rearms_calloop() {
        let mut event_loop = EventLoop::<usize>::try_new().unwrap();
        let (ping, source) = make_ping().unwrap();
        event_loop
            .handle()
            .insert_source(source, |(), (), wake_count| *wake_count += 1)
            .unwrap();
        let waker = Waker::from(Arc::new(UpdateWake(ping)));
        let item_count = RECEIVER_DRAIN_BUDGET * 2;
        let (sender, mut receiver) = tokio::sync::mpsc::channel(item_count);
        for item in 0..item_count {
            sender.try_send(item).unwrap();
        }

        let first = drain_receiver(&mut receiver, &waker);
        assert_eq!(first.items, (0..RECEIVER_DRAIN_BUDGET).collect::<Vec<_>>());
        assert!(!first.disconnected);
        assert_eq!(receiver.len(), RECEIVER_DRAIN_BUDGET);

        let mut wake_count = 0;
        event_loop
            .dispatch(Duration::ZERO, &mut wake_count)
            .unwrap();
        assert_eq!(wake_count, 1);

        let second = drain_receiver(&mut receiver, &waker);
        assert_eq!(
            second.items,
            (RECEIVER_DRAIN_BUDGET..item_count).collect::<Vec<_>>()
        );
        assert!(!second.disconnected);
    }

    #[test]
    fn receiver_drain_reports_boundary_disconnect_after_finite_tail() {
        let (_event_loop, waker) = update_test_waker();
        let item_count = RECEIVER_DRAIN_BUDGET + 4;
        let (sender, mut receiver) = tokio::sync::mpsc::channel(item_count);
        for item in 0..item_count {
            sender.try_send(item).unwrap();
        }
        drop(sender);

        let drained = drain_receiver(&mut receiver, &waker);
        assert_eq!(drained.items, (0..item_count).collect::<Vec<_>>());
        assert!(drained.disconnected);
    }

    #[test]
    fn terminal_stream_notices_distinguish_clean_exit_from_unexpected_disconnect() {
        let splint_id = SplintId::new();
        assert!(pane_stream_has_terminal_notice(&[WindowUpdate::Exited {
            splint_id
        }]));
        assert!(pane_stream_has_terminal_notice(&[WindowUpdate::Shutdown]));
        assert!(!pane_stream_has_terminal_notice(&[WindowUpdate::Control(
            false
        )]));
    }

    /// Plan 0061 M3.12a: an exited pane's close waits out a handoff pause,
    /// keeping its pending entry, and goes out once the pause ends.
    #[test]
    fn exited_splints_wait_for_the_end_of_a_handoff_pause() {
        let exited = SplintId::new();
        let mut pending = HashSet::from([exited]);
        let dojo_id = DojoId::new();
        let (commands, mut receiver) = tokio::sync::mpsc::channel(4);
        assert!(enqueue_exited_splints_unless_paused(
            true,
            dojo_id,
            LairRetention::Disposable,
            &mut pending,
            &commands
        ));
        assert!(pending.contains(&exited), "kept for the resume");
        assert!(receiver.try_recv().is_err(), "nothing reaches the daemon");
        assert!(enqueue_exited_splints_unless_paused(
            false,
            dojo_id,
            LairRetention::Disposable,
            &mut pending,
            &commands
        ));
        assert!(pending.is_empty());
        assert_eq!(
            receiver.try_recv().unwrap(),
            WindowTopologyCommand::Close {
                dojo_id,
                target: exited
            }
        );
    }

    #[test]
    fn pending_exited_splints_retry_full_and_closed_topology_queues() {
        let first = SplintId::new();
        let second = SplintId::new();
        let mut pending = HashSet::from([first, second]);
        let dojo_id = DojoId::new();
        let (commands, mut receiver) = tokio::sync::mpsc::channel(1);

        assert!(enqueue_pending_exited_splints(
            dojo_id,
            LairRetention::Disposable,
            &mut pending,
            &commands
        ));
        assert_eq!(pending.len(), 1);
        let WindowTopologyCommand::Close {
            dojo_id: accepted_dojo,
            target: accepted,
        } = receiver.try_recv().unwrap()
        else {
            panic!("expected automatic close command");
        };
        assert_eq!(accepted_dojo, dojo_id);
        assert!(!pending.contains(&accepted));

        assert!(enqueue_pending_exited_splints(
            dojo_id,
            LairRetention::Disposable,
            &mut pending,
            &commands
        ));
        assert!(pending.is_empty());
        let WindowTopologyCommand::Close {
            dojo_id: retained_dojo,
            target: retained,
        } = receiver.try_recv().unwrap()
        else {
            panic!("expected retried automatic close command");
        };
        assert_eq!(retained_dojo, dojo_id);
        drop(receiver);
        pending.insert(retained);
        assert!(!enqueue_pending_exited_splints(
            dojo_id,
            LairRetention::Disposable,
            &mut pending,
            &commands
        ));
        assert_eq!(pending, HashSet::from([retained]));
    }

    #[test]
    fn protected_lairs_retain_exited_splints_without_close_commands() {
        for retention in [LairRetention::Saved, LairRetention::Pinned] {
            let mut pending = HashSet::from([SplintId::new()]);
            let (commands, mut receiver) = tokio::sync::mpsc::channel(1);

            assert!(enqueue_pending_exited_splints(
                DojoId::new(),
                retention,
                &mut pending,
                &commands,
            ));
            assert!(pending.is_empty());
            assert!(receiver.try_recv().is_err());
        }
    }

    #[test]
    fn receiver_drain_yields_to_a_concurrently_refilling_producer() {
        let event_loop = EventLoop::<usize>::try_new().unwrap();
        let (ping, source) = make_ping().unwrap();
        event_loop
            .handle()
            .insert_source(source, |(), (), wake_count| *wake_count += 1)
            .unwrap();
        let waker = Waker::from(Arc::new(UpdateWake(ping)));
        let item_count = RECEIVER_DRAIN_BUDGET * 8;
        let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
        let producer = std::thread::spawn(move || {
            for item in 0..item_count {
                sender.blocking_send(item).unwrap();
            }
        });
        let mut items_seen = Vec::with_capacity(item_count);
        let mut disconnected = false;
        while !disconnected {
            let drained = drain_receiver(&mut receiver, &waker);
            assert!(
                drained.disconnected || drained.items.len() <= RECEIVER_DRAIN_BUDGET,
                "an active producer drain exceeded its cooperative budget"
            );
            items_seen.extend(drained.items);
            disconnected = drained.disconnected;
            std::thread::yield_now();
        }
        producer.join().unwrap();
        assert_eq!(items_seen, (0..item_count).collect::<Vec<_>>());
    }

    #[test]
    fn inactive_only_visual_changes_preserve_focused_cursor_blink_phase() {
        let original = Instant::now()
            .checked_sub(Duration::from_millis(250))
            .unwrap();
        let mut last_blink = original;
        let mut visible = false;
        assert!(!restart_cursor_blink(false, &mut visible, &mut last_blink));
        assert!(!visible);
        assert_eq!(last_blink, original);

        assert!(restart_cursor_blink(true, &mut visible, &mut last_blink));
        assert!(visible);
        assert!(last_blink > original);
    }

    #[test]
    fn transient_overlays_only_stale_the_rows_they_touch() {
        assert_eq!(
            transient_overlay_rows(5, Some(((1, 3), (3, 7))), Some(((4, 0), (4, 8)))),
            vec![false, true, true, true, true]
        );
        assert_eq!(transient_overlay_rows(3, None, None), vec![false; 3]);
        assert!(transient_overlay_rows(0, Some(((0, 0), (2, 0))), None).is_empty());
        assert_eq!(
            transient_overlay_rows(3, Some(((7, 0), (9, 0))), None),
            vec![false; 3]
        );
    }

    #[test]
    fn backing_damage_accumulates_independently_across_reused_buffers() {
        let first_region = Rect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        };
        let second_region = Rect {
            x: 5,
            y: 6,
            width: 7,
            height: 8,
        };
        let mut buffers = [BackingDamage::Clean, BackingDamage::Clean];
        for damage in &mut buffers {
            damage.mark_rows(&[true, false, false]);
            damage.mark_regions(&[first_region, first_region]);
        }
        let first = std::mem::replace(&mut buffers[0], BackingDamage::Clean);
        assert_eq!(
            first,
            BackingDamage::Partial {
                dirty_rows: vec![true, false, false],
                dirty_regions: vec![first_region],
            }
        );

        for damage in &mut buffers {
            damage.mark_rows(&[false, false, true]);
            damage.mark_regions(&[second_region]);
        }
        assert_eq!(
            buffers[0],
            BackingDamage::Partial {
                dirty_rows: vec![false, false, true],
                dirty_regions: vec![second_region],
            }
        );
        assert_eq!(
            buffers[1],
            BackingDamage::Partial {
                dirty_rows: vec![true, false, true],
                dirty_regions: vec![first_region, second_region],
            }
        );
        buffers[1].mark_full();
        buffers[1].mark_rows(&[false, true, false]);
        assert_eq!(buffers[1], BackingDamage::Full);
    }

    #[test]
    fn backing_damage_copies_only_stale_row_scanlines() {
        let cell = crate::geometry::CellGeometry::from_metrics(2, 2, 1, 1, 1).unwrap();
        let geometry = WindowGeometry::for_grid(
            2,
            3,
            cell,
            crate::geometry::TerminalPadding::uniform(0),
            120,
        )
        .unwrap();
        let (width, height, _) = geometry.buffer_layout().unwrap();
        let len = usize::try_from(width * height * 4).unwrap();
        let backing = (0..len)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect::<Vec<_>>();
        let mut canvas = vec![0xff; len];
        let copied = sync_backing_damage(
            &mut canvas,
            &backing,
            width,
            height,
            Some(&geometry),
            &BackingDamage::Partial {
                dirty_rows: vec![false, true, false],
                dirty_regions: Vec::new(),
            },
        );
        let stride = usize::try_from(width).unwrap() * 4;
        assert_eq!(copied, stride * 2);
        assert!(canvas[..stride * 2].iter().all(|byte| *byte == 0xff));
        assert_eq!(
            &canvas[stride * 2..stride * 4],
            &backing[stride * 2..stride * 4]
        );
        assert!(canvas[stride * 4..].iter().all(|byte| *byte == 0xff));
    }

    #[test]
    fn backing_damage_copies_only_stale_pane_regions() {
        let width = 4;
        let height = 3;
        let len = usize::try_from(width * height * 4).unwrap();
        let backing = (0..len)
            .map(|index| u8::try_from(index).unwrap())
            .collect::<Vec<_>>();
        let mut canvas = vec![0xff; len];
        let region = Rect {
            x: 1,
            y: 1,
            width: 2,
            height: 1,
        };
        let copied = sync_backing_damage(
            &mut canvas,
            &backing,
            width,
            height,
            None,
            &BackingDamage::Partial {
                dirty_rows: Vec::new(),
                dirty_regions: vec![region],
            },
        );
        let stride = usize::try_from(width).unwrap() * 4;
        assert_eq!(copied, 8);
        assert!(canvas[..stride + 4].iter().all(|byte| *byte == 0xff));
        assert_eq!(
            &canvas[stride + 4..stride + 12],
            &backing[stride + 4..stride + 12]
        );
        assert!(canvas[stride + 12..].iter().all(|byte| *byte == 0xff));
    }

    #[test]
    fn alternating_backing_buffers_repair_every_missed_row_update() {
        let cell = crate::geometry::CellGeometry::from_metrics(2, 2, 1, 1, 1).unwrap();
        let geometry = WindowGeometry::for_grid(
            2,
            3,
            cell,
            crate::geometry::TerminalPadding::uniform(0),
            120,
        )
        .unwrap();
        let (width, height, _) = geometry.buffer_layout().unwrap();
        let stride = usize::try_from(width).unwrap() * 4;
        let mut backing = vec![0; stride * usize::try_from(height).unwrap()];
        let mut canvases = [backing.clone(), backing.clone()];
        let mut damage = [BackingDamage::Clean, BackingDamage::Clean];

        backing[..stride * 2].fill(1);
        for stale in &mut damage {
            stale.mark_rows(&[true, false, false]);
        }
        let first = std::mem::replace(&mut damage[0], BackingDamage::Clean);
        sync_backing_damage(
            &mut canvases[0],
            &backing,
            width,
            height,
            Some(&geometry),
            &first,
        );
        assert_eq!(canvases[0], backing);
        assert_ne!(canvases[1], backing);

        backing[stride * 4..stride * 6].fill(2);
        for stale in &mut damage {
            stale.mark_rows(&[false, false, true]);
        }
        let second = std::mem::replace(&mut damage[1], BackingDamage::Clean);
        sync_backing_damage(
            &mut canvases[1],
            &backing,
            width,
            height,
            Some(&geometry),
            &second,
        );
        assert_eq!(canvases[1], backing);
        assert_ne!(canvases[0], backing);

        let first = std::mem::replace(&mut damage[0], BackingDamage::Clean);
        sync_backing_damage(
            &mut canvases[0],
            &backing,
            width,
            height,
            Some(&geometry),
            &first,
        );
        assert_eq!(canvases[0], backing);
        assert_eq!(canvases[1], backing);
    }

    #[test]
    fn alternating_backing_buffers_repair_every_missed_pane_region() {
        let width = 4;
        let height = 2;
        let stride = usize::try_from(width).unwrap() * 4;
        let mut backing = vec![0; stride * usize::try_from(height).unwrap()];
        let mut canvases = [backing.clone(), backing.clone()];
        let mut damage = [BackingDamage::Clean, BackingDamage::Clean];
        let left = Rect {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        };
        let right = Rect {
            x: 2,
            y: 1,
            width: 2,
            height: 1,
        };

        backing[..8].fill(1);
        for stale in &mut damage {
            stale.mark_regions(&[left]);
        }
        let first = std::mem::replace(&mut damage[0], BackingDamage::Clean);
        sync_backing_damage(&mut canvases[0], &backing, width, height, None, &first);
        assert_eq!(canvases[0], backing);
        assert_ne!(canvases[1], backing);

        backing[stride + 8..stride + 16].fill(2);
        for stale in &mut damage {
            stale.mark_regions(&[right]);
        }
        let second = std::mem::replace(&mut damage[1], BackingDamage::Clean);
        sync_backing_damage(&mut canvases[1], &backing, width, height, None, &second);
        assert_eq!(canvases[1], backing);
        assert_ne!(canvases[0], backing);

        let first = std::mem::replace(&mut damage[0], BackingDamage::Clean);
        sync_backing_damage(&mut canvases[0], &backing, width, height, None, &first);
        assert_eq!(canvases[0], backing);
        assert_eq!(canvases[1], backing);
    }

    #[test]
    fn backing_full_damage_restores_transient_canvas_bytes() {
        let backing = vec![7; 32];
        let mut canvas = vec![9; 32];
        assert_eq!(
            sync_backing_damage(&mut canvas, &backing, 4, 2, None, &BackingDamage::Full),
            backing.len()
        );
        assert_eq!(canvas, backing);
    }

    #[test]
    fn deterministic_capture_waits_for_required_images_and_scale() {
        assert!(deterministic_capture_ready(true, 0, 0));
        assert!(!deterministic_capture_ready(false, 0, 2));
        assert!(!deterministic_capture_ready(true, 2, 1));
        assert!(deterministic_capture_ready(true, 2, 2));
    }

    fn snapshot(splint_id: SplintId, incarnation: u64, revision: u64) -> TerminalSnapshot {
        TerminalSnapshot {
            splint_id,
            incarnation,
            revision,
            columns: 0,
            rows: 0,
            cursor_column: 0,
            cursor_row: 0,
            cursor_deferred_wrap: false,
            active_screen: ActiveScreen::Normal,
            input_modes: TerminalInputModes {
                application_cursor: false,
                application_keypad: false,
                focus_reporting: false,
                bracketed_paste: false,
                cursor_visible: true,
                cursor_blink: true,
                mouse_tracking: MouseTracking::None,
                sgr_mouse: false,
            },
            palette: vec![0; 256],
            default_colors: [0x00eb_ebeb, 0x000e_1216, 0x00eb_ebeb],
            title: String::new(),
            visible_rows: Vec::new(),
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

    fn valid_snapshot(splint_id: SplintId) -> TerminalSnapshot {
        let mut snapshot = snapshot(splint_id, 1, 1);
        snapshot.columns = 1;
        snapshot.rows = 1;
        let mut row = blank_row(1);
        row.row_id = Some(1);
        snapshot.visible_rows = vec![row];
        snapshot
    }

    #[test]
    fn copy_mode_uses_stable_loaded_rows_and_bounds_older_page_requests() {
        let mut snapshot = snapshot(SplintId::new(), 1, 7);
        snapshot.columns = 4;
        snapshot.rows = 2;
        snapshot.cursor_column = 2;
        snapshot.cursor_row = 1;
        snapshot.omitted_oldest_scrollback_rows = 20;
        snapshot.scrollback_rows = (1..=2)
            .map(|row_id| {
                let mut row = blank_row(4);
                row.row_id = Some(row_id);
                row
            })
            .collect();
        snapshot.visible_rows = (3..=4)
            .map(|row_id| {
                let mut row = blank_row(4);
                row.row_id = Some(row_id);
                row
            })
            .collect();
        let mut state = copy_mode_enter(&snapshot, &snapshot).unwrap();
        assert_eq!(state.cursor.row_id, 4);
        assert_eq!(state.cursor.column, 2);
        state.begin_selection();
        let older = move_copy_cursor(&snapshot, &mut state, CopyMotion::Up(4));
        assert!(older.moved);
        assert!(older.request_older);
        assert_eq!(state.cursor.row_id, 1);
        assert!(copy_mode_is_valid(&snapshot, state));
        let down = move_copy_cursor(&snapshot, &mut state, CopyMotion::Down(2));
        assert!(down.moved);
        assert!(!down.request_older);
        assert_eq!(state.cursor.row_id, 3);
        assert_eq!(state.overlay_selection().anchor.row_id, 4);
        snapshot.history_generation += 1;
        assert!(!copy_mode_is_valid(&snapshot, state));
    }

    #[test]
    fn copy_mode_vim_word_motions_cross_whitespace_and_punctuation() {
        let mut snapshot = snapshot(SplintId::new(), 1, 1);
        snapshot.columns = 17;
        snapshot.rows = 1;
        let mut row = blank_row(snapshot.columns);
        row.row_id = Some(1);
        for (column, content) in "alpha beta, gamma".chars().enumerate() {
            row.cells[column].content = content.to_string();
        }
        snapshot.visible_rows = vec![row];

        let mut state = copy_mode_enter(&snapshot, &snapshot).unwrap();
        assert!(move_copy_cursor(&snapshot, &mut state, CopyMotion::WordEnd).moved);
        assert_eq!(state.cursor.column, 4);

        state.cursor.column = 0;
        assert!(move_copy_cursor(&snapshot, &mut state, CopyMotion::WordForward).moved);
        assert_eq!(state.cursor.column, 6);
        assert!(move_copy_cursor(&snapshot, &mut state, CopyMotion::WordBackward).moved);
        assert_eq!(state.cursor.column, 0);

        state.cursor.column = 10;
        assert!(move_copy_cursor(&snapshot, &mut state, CopyMotion::WordEnd).moved);
        assert_eq!(state.cursor.column, 16);
    }

    #[test]
    fn copy_mode_words_respect_row_boundaries_and_loaded_history() {
        let mut snapshot = snapshot(SplintId::new(), 1, 1);
        snapshot.columns = 3;
        snapshot.rows = 1;
        let mut rows = Vec::new();
        for (index, text) in ["foo", "bar", "   ", "baz"].into_iter().enumerate() {
            let mut row = blank_row(3);
            row.row_id = Some(index as u64 + 1);
            for (column, character) in text.chars().enumerate() {
                row.cells[column].content = character.to_string();
            }
            rows.push(row);
        }
        snapshot.visible_rows = rows.split_off(3);
        snapshot.scrollback_rows = rows;
        let mut state = copy_mode_enter(&snapshot, &snapshot).unwrap();
        for (motion, start, expected) in [
            (CopyMotion::WordEnd, (1, 0), (1, 2)),
            (CopyMotion::WordForward, (1, 0), (2, 0)),
            (CopyMotion::WordBackward, (2, 2), (2, 0)),
            (CopyMotion::WordBackward, (2, 0), (1, 0)),
            (CopyMotion::WordEnd, (1, 2), (2, 2)),
            (CopyMotion::WordForward, (2, 0), (4, 0)),
            (CopyMotion::WordBackward, (4, 0), (2, 0)),
            (CopyMotion::WordEnd, (2, 2), (4, 2)),
            (CopyMotion::WordEnd, (4, 2), (4, 2)),
            (CopyMotion::WordBackward, (1, 0), (1, 0)),
        ] {
            state.cursor.row_id = start.0;
            state.cursor.column = start.1;
            move_copy_cursor(&snapshot, &mut state, motion);
            assert_eq!(
                (state.cursor.row_id, state.cursor.column),
                expected,
                "{motion:?}"
            );
        }
    }

    #[test]
    fn copy_mode_word_motions_skip_wide_character_spacers() {
        let mut snapshot = snapshot(SplintId::new(), 1, 1);
        snapshot.columns = 8;
        snapshot.rows = 1;
        let mut row = blank_row(8);
        row.row_id = Some(1);
        for (column, content) in [(0, "中"), (2, "文"), (4, " "), (5, "f"), (6, "o"), (7, "o")] {
            row.cells[column].content = content.into();
        }
        for column in [1, 3] {
            row.cells[column].content.clear();
            row.cells[column].spacer_remaining = Some(1);
        }
        snapshot.visible_rows = vec![row];
        let mut state = copy_mode_enter(&snapshot, &snapshot).unwrap();
        for (motion, start, expected) in [
            (CopyMotion::WordEnd, 0, 2),
            (CopyMotion::WordEnd, 2, 7),
            (CopyMotion::WordForward, 0, 5),
            (CopyMotion::WordBackward, 5, 0),
            (CopyMotion::WordBackward, 2, 0),
        ] {
            state.cursor.column = start;
            move_copy_cursor(&snapshot, &mut state, motion);
            assert_eq!(state.cursor.column, expected, "{motion:?}");
        }
    }

    fn pane_options(splint_id: SplintId) -> WindowPaneOptions {
        let (_updates, update_receiver) = tokio::sync::mpsc::channel(1);
        let (commands, _command_receiver) = tokio::sync::mpsc::channel(1);
        WindowPaneOptions {
            snapshot: valid_snapshot(splint_id),
            terminal_grid_limits: TerminalGridLimits::default(),
            updates: update_receiver,
            commands,
            authority: AuthorityStatus::default(),
            controlled: false,
            image_sources: ImageContentLeaseSet::default(),
        }
    }

    #[test]
    fn hidden_tab_updates_cache_without_rebuild_resize_or_control_claim() {
        let (_event_loop, waker) = update_test_waker();
        let splint = Splint::shell(PathBuf::from("/tmp"));
        let splint_id = splint.id;
        let (updates, update_receiver) = tokio::sync::mpsc::channel(1);
        let (commands, mut command_receiver) = tokio::sync::mpsc::channel(1);
        let mut view = DojoTabView::from_open(
            WindowDojoIdentity {
                topology_revision: TopologyRevision::new(1),
                lair_id: LairId::new(),
                dojo_id: DojoId::new(),
                lair_name: "hidden lair".to_owned(),
                lair_retention: splinterm_core::LairRetention::Disposable,
                dojo_name: "hidden dojo".to_owned(),
            },
            LayoutNode::Leaf(splint),
            vec![WindowPaneOptions {
                snapshot: valid_snapshot(splint_id),
                terminal_grid_limits: TerminalGridLimits::default(),
                updates: update_receiver,
                commands,
                authority: AuthorityStatus::default(),
                controlled: false,
                image_sources: ImageContentLeaseSet::default(),
            }],
            splint_id,
            ResolvedTheme::default(),
            SCALE_DENOMINATOR,
            &RenderContext::new(u16::MAX),
        )
        .unwrap();
        view.pane.snapshot_frame = None;
        let mut revised = valid_snapshot(splint_id);
        revised.revision = 2;
        revised.visible_rows[0].cells[0].content = "hidden update".to_owned();
        updates
            .try_send(WindowUpdate::Snapshot {
                snapshot: revised,
                image_sources: ImageContentLeaseSet::default(),
                authoritative: false,
            })
            .unwrap();

        view.drain_hidden_updates(&waker, ResolvedTheme::default(), false)
            .unwrap();

        assert_eq!(view.pane.snapshot.as_ref().unwrap().revision, 2);
        assert!(view.dirty_inactive_panes.contains(&splint_id));
        assert!(view.pane.snapshot_frame.is_none());
        assert!(command_receiver.try_recv().is_err());
    }

    /// Plan 0061 M3.12a: a hidden pane's stream lost while a handoff is
    /// fenced leaves the Window open with the pane disconnected; otherwise
    /// the loss still ends the Window.
    #[test]
    fn a_hidden_pane_lost_during_a_fenced_handoff_keeps_the_window() {
        let (_event_loop, waker) = update_test_waker();
        for hold in [true, false] {
            let splint = Splint::shell(PathBuf::from("/tmp"));
            let splint_id = splint.id;
            let (updates, update_receiver) = tokio::sync::mpsc::channel(1);
            let (commands, _command_receiver) = tokio::sync::mpsc::channel(1);
            let mut view = DojoTabView::from_open(
                WindowDojoIdentity {
                    topology_revision: TopologyRevision::new(1),
                    lair_id: LairId::new(),
                    dojo_id: DojoId::new(),
                    lair_name: "hidden lair".to_owned(),
                    lair_retention: splinterm_core::LairRetention::Disposable,
                    dojo_name: "hidden dojo".to_owned(),
                },
                LayoutNode::Leaf(splint),
                vec![WindowPaneOptions {
                    snapshot: valid_snapshot(splint_id),
                    terminal_grid_limits: TerminalGridLimits::default(),
                    updates: update_receiver,
                    commands,
                    authority: AuthorityStatus::default(),
                    controlled: true,
                    image_sources: ImageContentLeaseSet::default(),
                }],
                splint_id,
                ResolvedTheme::default(),
                SCALE_DENOMINATOR,
                &RenderContext::new(u16::MAX),
            )
            .unwrap();
            drop(updates);
            let result = view.drain_hidden_updates(&waker, ResolvedTheme::default(), hold);
            if hold {
                assert!(result.unwrap(), "the loss is reported as held");
                assert!(view.pane.commands.is_none());
                assert!(view.pane.updates.is_none());
                assert!(!view.pane.controller_active);
            } else {
                assert!(result.is_err());
            }
        }
    }

    /// Plan 0061 M3.12a: the resize and focus report a controller dropped
    /// during a handoff pause are sent again when it ends, even though the
    /// geometry did not change.
    #[test]
    fn a_handoff_resume_resends_geometry_and_the_focus_report() {
        let splint = Splint::shell(PathBuf::from("/tmp"));
        let splint_id = splint.id;
        let (_updates, update_receiver) = tokio::sync::mpsc::channel(1);
        let (commands, mut command_receiver) = tokio::sync::mpsc::channel(8);
        let mut view = DojoTabView::from_open(
            WindowDojoIdentity {
                topology_revision: TopologyRevision::new(1),
                lair_id: LairId::new(),
                dojo_id: DojoId::new(),
                lair_name: "lair".to_owned(),
                lair_retention: splinterm_core::LairRetention::Disposable,
                dojo_name: "dojo".to_owned(),
            },
            LayoutNode::Leaf(splint),
            vec![WindowPaneOptions {
                snapshot: valid_snapshot(splint_id),
                terminal_grid_limits: TerminalGridLimits::default(),
                updates: update_receiver,
                commands,
                authority: AuthorityStatus::default(),
                controlled: true,
                image_sources: ImageContentLeaseSet::default(),
            }],
            splint_id,
            ResolvedTheme::default(),
            SCALE_DENOMINATOR,
            &RenderContext::new(u16::MAX),
        )
        .unwrap();
        view.pane
            .snapshot
            .as_mut()
            .unwrap()
            .input_modes
            .focus_reporting = true;
        let resize = |pane: &mut PaneView| {
            App::emit_pane_resize(pane, 800, 480, SCALE_DENOMINATOR, true).unwrap();
        };
        resize(&mut view.pane);
        assert!(matches!(
            command_receiver.try_recv(),
            Ok(WindowCommand::Resize { .. })
        ));
        // Unchanged geometry is not sent again: this is what a controller
        // dropping the resize during the pause would otherwise leave stale.
        resize(&mut view.pane);
        assert!(command_receiver.try_recv().is_err());
        App::ready_pane_for_handoff_resume(&mut view.pane, true, &VecDeque::new());
        assert_eq!(
            command_receiver.try_recv().unwrap(),
            WindowCommand::FocusReport(true)
        );
        resize(&mut view.pane);
        assert!(matches!(
            command_receiver.try_recv(),
            Ok(WindowCommand::Resize { .. })
        ));
        // A terminal that never asked for focus reports hears none.
        view.pane
            .snapshot
            .as_mut()
            .unwrap()
            .input_modes
            .focus_reporting = false;
        App::ready_pane_for_handoff_resume(&mut view.pane, true, &VecDeque::new());
        assert!(command_receiver.try_recv().is_err());
    }

    #[test]
    fn handoff_pause_drops_keys_pastes_drops_and_terminal_composition() {
        // Keys, IME commits, pastes, and file drops share this one gate.
        assert!(terminal_input_blocked(false, false, true));
        assert!(!terminal_input_blocked(false, false, false));
        // A clipboard read that finishes during the pause is dropped.
        assert!(!clipboard_read_is_current(
            terminal_input_blocked(false, false, true),
            7,
            7
        ));
        // No composition reaches the terminal: the text input is disabled
        // for it and any batch is dropped.
        let paused = ImeContext {
            terminal_input_paused: true,
            ..ImeContext::default()
        };
        assert_eq!(ime_route(paused), (None, false, true));
        // Owned fields, such as terminal search and the palette, still compose.
        assert_eq!(
            ime_route(ImeContext {
                terminal_search: true,
                ..paused
            }),
            (Some(OwnedFieldTarget::Search), true, false)
        );
        assert_eq!(
            ime_route(ImeContext {
                input_modal_open: true,
                command_palette: true,
                ..paused
            }),
            (Some(OwnedFieldTarget::CommandPalette), true, false)
        );
    }

    #[test]
    fn hidden_tab_controller_release_covers_every_pane() {
        let first = Splint::shell(PathBuf::from("/tmp"));
        let first_id = first.id;
        let second = Splint::shell(PathBuf::from("/tmp"));
        let second_id = second.id;
        let layout = LayoutNode::Branch {
            axis: Axis::Horizontal,
            ratio: SplitRatio::new(500).unwrap(),
            first: Box::new(LayoutNode::Leaf(first)),
            second: Box::new(LayoutNode::Leaf(second)),
        };
        let (_first_updates, first_update_receiver) = tokio::sync::mpsc::channel(1);
        let (first_commands, mut first_command_receiver) = tokio::sync::mpsc::channel(1);
        let pending_first_commands = first_commands.clone();
        let (_second_updates, second_update_receiver) = tokio::sync::mpsc::channel(1);
        let (second_commands, mut second_command_receiver) = tokio::sync::mpsc::channel(1);
        let mut view = DojoTabView::from_open(
            WindowDojoIdentity {
                topology_revision: TopologyRevision::new(1),
                lair_id: LairId::new(),
                dojo_id: DojoId::new(),
                lair_name: "hidden lair".to_owned(),
                lair_retention: splinterm_core::LairRetention::Disposable,
                dojo_name: "hidden dojo".to_owned(),
            },
            layout,
            vec![
                WindowPaneOptions {
                    snapshot: valid_snapshot(first_id),
                    terminal_grid_limits: TerminalGridLimits::default(),
                    updates: first_update_receiver,
                    commands: first_commands,
                    authority: AuthorityStatus::default(),
                    controlled: true,
                    image_sources: ImageContentLeaseSet::default(),
                },
                WindowPaneOptions {
                    snapshot: valid_snapshot(second_id),
                    terminal_grid_limits: TerminalGridLimits::default(),
                    updates: second_update_receiver,
                    commands: second_commands,
                    authority: AuthorityStatus::default(),
                    controlled: true,
                    image_sources: ImageContentLeaseSet::default(),
                },
            ],
            first_id,
            ResolvedTheme::default(),
            SCALE_DENOMINATOR,
            &RenderContext::new(u16::MAX),
        )
        .unwrap();

        App::request_pane_focus_report(&mut view.pane, true, true);
        assert_eq!(view.pane.pending_focus_report, Some(true));
        assert!(pane_command_retry_pending(&view.pane));
        assert!(first_command_receiver.try_recv().is_err());
        App::retry_pane_focus_report(&mut view.pane, false);
        assert_eq!(
            first_command_receiver.try_recv().unwrap(),
            WindowCommand::FocusReport(true)
        );

        let pending = VecDeque::from([PendingTerminalInput {
            commands: pending_first_commands,
            bytes: vec![0x7f],
        }]);
        App::release_tab_controllers(&mut view, &pending);
        assert!(view.pane.controller_active);
        assert!(view.pane.control_release_pending);
        assert!(pane_command_retry_pending(&view.pane));
        assert!(!view.inactive_panes[0].controller_active);
        assert!(first_command_receiver.try_recv().is_err());
        assert_eq!(
            second_command_receiver.try_recv().unwrap(),
            WindowCommand::ReleaseControl
        );

        App::release_tab_controllers(&mut view, &VecDeque::new());
        assert!(!view.pane.controller_active);
        assert_eq!(
            first_command_receiver.try_recv().unwrap(),
            WindowCommand::ReleaseControl
        );

        view.pane.control_release_pending = true;
        App::retry_pane_control_release(&mut view.pane, false);
        assert!(!view.pane.control_release_pending);
        assert!(!pane_command_retry_pending(&view.pane));
    }

    #[test]
    fn closed_tab_drops_renderer_image_leases() {
        let pixels = vec![1_u8, 2, 3, 255];
        let metadata = ImageContentMetadata {
            content_id: 1,
            generation: 1,
            width: 1,
            height: 1,
            source_format: ImageSourceFormat::Sixel,
            alpha_mode: ImageAlphaMode::Opaque,
            digest: Sha256::digest(&pixels).into(),
            byte_length: pixels.len(),
            retention: ImageRetention::WhilePlaced,
        };
        let cache = SharedImageContentCache::with_maximum_bytes(4).unwrap();
        cache
            .insert_source(&metadata, ImageContentSource::Buffered(Arc::from(pixels)))
            .unwrap();
        let splint = Splint::shell(PathBuf::from("/tmp"));
        let splint_id = splint.id;
        let (_updates, update_receiver) = tokio::sync::mpsc::channel(1);
        let (commands, _command_receiver) = tokio::sync::mpsc::channel(1);
        let lair_id = LairId::new();
        let dojo_id = DojoId::new();
        let view = DojoTabView::from_open(
            WindowDojoIdentity {
                topology_revision: TopologyRevision::new(1),
                lair_id,
                dojo_id,
                lair_name: "image lair".to_owned(),
                lair_retention: splinterm_core::LairRetention::Disposable,
                dojo_name: "image dojo".to_owned(),
            },
            LayoutNode::Leaf(splint),
            vec![WindowPaneOptions {
                snapshot: valid_snapshot(splint_id),
                terminal_grid_limits: TerminalGridLimits::default(),
                updates: update_receiver,
                commands,
                authority: AuthorityStatus::default(),
                controlled: false,
                image_sources: cache.lease(std::slice::from_ref(&metadata)).unwrap(),
            }],
            splint_id,
            ResolvedTheme::default(),
            SCALE_DENOMINATOR,
            &RenderContext::new(u16::MAX),
        )
        .unwrap();
        let mut tabs = WindowTabSet::new(DojoTab::new(lair_id, dojo_id, Some(view)));
        let replacement_pixels = vec![4_u8, 5, 6, 255];
        let mut replacement = metadata.clone();
        replacement.content_id = 2;
        replacement.digest = Sha256::digest(&replacement_pixels).into();
        assert!(
            cache
                .insert_source(
                    &replacement,
                    ImageContentSource::Buffered(Arc::from(replacement_pixels.clone())),
                )
                .is_err()
        );

        drop(tabs.close(dojo_id));

        cache
            .insert_source(
                &replacement,
                ImageContentSource::Buffered(Arc::from(replacement_pixels)),
            )
            .unwrap();
        assert!(!cache.contains(&metadata).unwrap());
        assert!(cache.contains(&replacement).unwrap());
    }

    #[derive(Clone, Copy)]
    enum ReducerBenchMode {
        FocusedRole,
        InactiveBatch,
    }

    #[derive(Clone, Copy)]
    struct ReducerBenchCase {
        history_rows: usize,
        detached: bool,
        batch_size: usize,
        mode: ReducerBenchMode,
    }

    fn reducer_bench_cases(smoke: bool) -> Vec<ReducerBenchCase> {
        let history = if smoke {
            vec![(0, false), (MAX_CACHED_HISTORY_ROWS, false)]
        } else {
            vec![
                (0, false),
                (1_000, false),
                (MAX_CACHED_HISTORY_ROWS, false),
                (MAX_CACHED_HISTORY_ROWS, true),
            ]
        };
        let batches = if smoke { vec![1, 16] } else { vec![1, 16, 64] };
        history
            .into_iter()
            .flat_map(|(history_rows, detached)| {
                batches.iter().copied().flat_map(move |batch_size| {
                    [
                        ReducerBenchMode::FocusedRole,
                        ReducerBenchMode::InactiveBatch,
                    ]
                    .into_iter()
                    .map(move |mode| ReducerBenchCase {
                        history_rows,
                        detached,
                        batch_size,
                        mode,
                    })
                })
            })
            .collect()
    }

    fn reducer_bench_pane(case: ReducerBenchCase) -> PaneView {
        let splint_id = SplintId::new();
        let mut options = pane_options(splint_id);
        options.snapshot.scrollback_rows = (1..=u64::try_from(case.history_rows).unwrap())
            .map(|row_id| {
                let mut row = blank_row(1);
                row.row_id = Some(row_id);
                row
            })
            .collect();
        options.snapshot.available_scrollback_rows = case.history_rows;
        options.snapshot.oldest_available_scrollback_row_id = (case.history_rows > 0).then_some(1);
        options.snapshot.newest_available_scrollback_row_id =
            (case.history_rows > 0).then_some(u64::try_from(case.history_rows).unwrap());
        options.snapshot.visible_rows[0].row_id = Some(10_000_000);
        let mut pane = PaneView::from_options(options, SCALE_DENOMINATOR).unwrap();
        if case.detached {
            pane.scrollback_viewport
                .scroll_up(1, pane.snapshot.as_ref().unwrap());
            assert!(!pane.scrollback_viewport.is_live());
        }
        pane
    }

    fn reducer_bench_updates(snapshot: &TerminalSnapshot, batch_size: usize) -> Vec<WindowUpdate> {
        let mut projected = snapshot.clone();
        (0..batch_size)
            .map(|index| {
                let mut update = empty_update();
                update.base_revision = projected.revision;
                update.revision = projected.revision.saturating_add(1);
                let mut row = projected.visible_rows[0].clone();
                row.cells[0].content = format!("{index:02x}");
                update
                    .rows
                    .push(splinterm_protocol::TerminalRowPatch { index: 0, row });
                apply_terminal_update(&mut projected, update.clone()).unwrap();
                WindowUpdate::Update {
                    update,
                    image_sources: None,
                    trace: None,
                }
            })
            .collect()
    }

    fn measure_reducer_bench_case(case: ReducerBenchCase) -> u64 {
        let mut pane = reducer_bench_pane(case);
        let updates = reducer_bench_updates(pane.snapshot.as_ref().unwrap(), case.batch_size);
        let started = Instant::now();
        match case.mode {
            ReducerBenchMode::FocusedRole => {
                for update in updates {
                    pane.apply_background_update(update, ResolvedTheme::default(), "focused")
                        .unwrap();
                }
            }
            ReducerBenchMode::InactiveBatch => {
                apply_inactive_update_batch(&mut pane, updates, ResolvedTheme::default()).unwrap();
            }
        }
        let elapsed = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        std::hint::black_box((
            pane.snapshot.as_ref().unwrap().revision,
            pane.scrollback_viewport.offset_from_bottom(),
        ));
        elapsed
    }

    #[test]
    fn frame_corner_cells_contain_only_corner_masks() {
        let splint = Splint::shell(PathBuf::from("/tmp"));
        let splint_id = splint.id;
        let layout = PaneLayout::compute_with_chrome(
            &LayoutNode::Leaf(splint),
            Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 64,
            },
            PaneChrome::Frame {
                vertical_width: 8,
                horizontal_height: 16,
            },
            2,
            2,
        )
        .unwrap();
        let mut actual = vec![0; 80 * 64 * 4];
        let theme = ResolvedTheme::default();
        paint_pane_chrome(
            &mut actual,
            80,
            64,
            &layout,
            Some(splint_id),
            theme,
            8,
            16,
            120,
            &HashMap::new(),
        )
        .unwrap();

        let corner = Rect {
            x: 0,
            y: 0,
            width: 8,
            height: 16,
        };
        let mut expected = vec![0; 80 * 64 * 4];
        paint_box_drawing_cell(
            &mut expected,
            80,
            64,
            '┌',
            corner,
            corner,
            theme.roles.focus_ring,
            120,
        );
        let region = |canvas: &[u8]| {
            (0_usize..16)
                .flat_map(|y| {
                    let start = y * 80 * 4;
                    canvas[start..start + 8 * 4].to_vec()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(region(&actual), region(&expected));
    }

    /// Review of #212: frame titles are chrome text inside a border row laid
    /// out in terminal cells. When the chrome cell is larger than the
    /// terminal's (zoomed out, or a UI size above the terminal size), a title
    /// must still fit its row whole and stop clear of the top-right corner.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one case table drives layout, unclipped paint and pane chrome checks"
    )]
    fn frame_titles_fit_their_border_row_at_every_chrome_and_terminal_size() {
        use super::chrome::frame_title_placement;

        let nine_point =
            || RenderContext::new(u16::MAX).with_font_size_override(Some(FontSize::Points(9.0)));
        let cases = [
            // Stock Omarchy, one zoom-out step: 11.3 px terminal, 12 px chrome.
            (
                "zoomed out",
                nine_point().with_chrome_font_size(FontSize::Pixels(12.0)),
                -1,
            ),
            // The default context zoomed out ten steps, chrome far larger.
            (
                "zoom -10",
                RenderContext::new(u16::MAX).at_maximum_chrome_font_size(),
                -10,
            ),
            // An explicit small terminal under a larger UI size.
            (
                "ui above terminal",
                nine_point().with_chrome_font_size(FontSize::Pixels(20.0)),
                -6,
            ),
            // Chrome smaller than the terminal, as before.
            ("ui below terminal", RenderContext::new(u16::MAX), 0),
        ];
        for (case, context, steps) in cases {
            for scale in [120, 240] {
                let (cell, _) = context.zoomed_cell_geometry(steps, scale).unwrap().unwrap();
                let (cell_width, cell_height) = (cell.width, cell.height);
                let splint = Splint::shell(PathBuf::from("/tmp"));
                let splint_id = splint.id;
                let layout = PaneLayout::compute_with_chrome(
                    &LayoutNode::Leaf(splint),
                    Rect {
                        x: 0,
                        y: 0,
                        width: 320,
                        height: 120,
                    },
                    PaneChrome::Frame {
                        vertical_width: 8,
                        horizontal_height: 16,
                    },
                    2,
                    2,
                )
                .unwrap();
                let allocation = App::buffer_rect(layout.panes[0].allocation, scale).unwrap();
                let chrome_cell =
                    load_frame_title(&ChromeSource::from("_"), scale, &context, cell_height)
                        .unwrap()
                        .pixel_width();
                let maximum_cells =
                    frame_title_maximum_cells(allocation.width, cell_width, chrome_cell).unwrap();
                let source = ChromeSource::from(sanitize_frame_title(
                    "a long Splint title that keeps going well past the pane's frame",
                    maximum_cells,
                ));
                let text = load_frame_title(&source, scale, &context, cell_height).unwrap();
                let label = format!("{case} at {scale}");
                assert!(text.pixel_height() <= cell_height, "{label}");
                assert!(text.cells() <= maximum_cells, "{label}");
                let (clear, origin) =
                    frame_title_placement(allocation, cell_width, cell_height, &text);
                // The cleared title area stays in the border row and clear of
                // both top corners, with a terminal cell of padding.
                assert_eq!(clear.y, allocation.y, "{label}");
                assert!(clear.height <= cell_height, "{label}");
                assert!(
                    clear.x + clear.width <= allocation.x + allocation.width - 2 * cell_width,
                    "{label}"
                );
                assert!(origin.1 + text.pixel_height() <= allocation.y + cell_height);

                // Painted whole: clipping to the cleared area changes nothing.
                let (width, height) = (allocation.width + 8, allocation.height + 8);
                let canvas = || vec![0_u8; usize::try_from(width * height * 4).unwrap()];
                let everything = Rect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                };
                let (mut clipped, mut unclipped) = (canvas(), canvas());
                text.paint(&mut clipped, width, height, origin, clear, 0x00ff_ffff);
                text.paint(
                    &mut unclipped,
                    width,
                    height,
                    origin,
                    everything,
                    0x00ff_ffff,
                );
                assert_eq!(clipped, unclipped, "{label}: the title was cut");
                assert!(clipped.iter().any(|byte| *byte != 0), "{label}");

                // The pane chrome draws it there and nowhere else.
                let theme = ResolvedTheme::default();
                let mut titles = HashMap::new();
                titles.insert(
                    splint_id,
                    CachedFrameTitle {
                        source,
                        maximum_cells,
                        scale_120: scale,
                        bold: false,
                        text,
                    },
                );
                let (mut bare, mut with_title) = (canvas(), canvas());
                for (target, titles) in [(&mut bare, &HashMap::new()), (&mut with_title, &titles)] {
                    paint_pane_chrome(
                        target,
                        width,
                        height,
                        &layout,
                        Some(splint_id),
                        theme,
                        cell_width,
                        cell_height,
                        scale,
                        titles,
                    )
                    .unwrap();
                }
                for y in 0..height {
                    for x in 0..width {
                        let index = usize::try_from((y * width + x) * 4).unwrap();
                        if bare[index..index + 4] != with_title[index..index + 4] {
                            assert!(
                                (clear.x..clear.x + clear.width).contains(&x)
                                    && (clear.y..clear.y + clear.height).contains(&y),
                                "{label}: title paint at ({x}, {y}) outside {clear:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn frame_title_cells_are_chrome_cells_beside_terminal_corners() {
        // Three terminal cells on each side; the rest in chrome cells.
        assert_eq!(frame_title_maximum_cells(200, 10, 10), Some(14));
        assert_eq!(frame_title_maximum_cells(200, 10, 14), Some(10));
        assert_eq!(frame_title_maximum_cells(200, 10, 7), Some(20));
        assert_eq!(frame_title_maximum_cells(60, 10, 10), None);
        assert_eq!(frame_title_maximum_cells(65, 10, 10), None);
        assert_eq!(frame_title_maximum_cells(70, 10, 10), Some(1));
        assert_eq!(frame_title_maximum_cells(70, 10, 0), Some(10));
    }

    #[test]
    fn frame_titles_sanitize_controls_collapse_space_and_keep_complete_widths() {
        assert_eq!(
            sanitize_frame_title("  editor\n\t logs  ", 20),
            "editor logs"
        );
        assert_eq!(sanitize_frame_title("界 shell", 4), "界 s");
        assert_eq!(sanitize_frame_title("e\u{301}ditor", 1), "e\u{301}");
        assert_eq!(sanitize_frame_title("ignored", 0), "");
    }

    #[test]
    fn nested_line_dividers_resolve_all_tee_orientations() {
        let vertical = PaneDivider {
            axis: Axis::Horizontal,
            rect: Rect {
                x: 40,
                y: 0,
                width: 8,
                height: 100,
            },
        };
        let from_right = PaneDivider {
            axis: Axis::Vertical,
            rect: Rect {
                x: 48,
                y: 30,
                width: 52,
                height: 16,
            },
        };
        let from_left = PaneDivider {
            rect: Rect {
                x: 0,
                width: 40,
                ..from_right.rect
            },
            ..from_right
        };
        assert_eq!(divider_junction(vertical, from_right).unwrap().0, '├');
        assert_eq!(divider_junction(vertical, from_left).unwrap().0, '┤');

        let horizontal = PaneDivider {
            axis: Axis::Vertical,
            rect: Rect {
                x: 0,
                y: 40,
                width: 100,
                height: 16,
            },
        };
        let from_bottom = PaneDivider {
            axis: Axis::Horizontal,
            rect: Rect {
                x: 30,
                y: 56,
                width: 8,
                height: 44,
            },
        };
        let from_top = PaneDivider {
            rect: Rect {
                y: 0,
                height: 40,
                ..from_bottom.rect
            },
            ..from_bottom
        };
        assert_eq!(divider_junction(horizontal, from_bottom).unwrap().0, '┬');
        assert_eq!(divider_junction(horizontal, from_top).unwrap().0, '┴');
    }

    #[test]
    fn graphical_focus_watch_tracks_keyboard_and_selected_pane() {
        let first = SplintId::new();
        let second = SplintId::new();
        let (sender, receiver) = tokio::sync::watch::channel(None);

        update_graphical_focus_watch(Some(&sender), true, Some(first));
        assert_eq!(*receiver.borrow(), Some(first));
        update_graphical_focus_watch(Some(&sender), true, Some(second));
        assert_eq!(*receiver.borrow(), Some(second));
        update_graphical_focus_watch(Some(&sender), false, Some(second));
        assert_eq!(*receiver.borrow(), None);
        update_graphical_focus_watch(Some(&sender), true, None);
        assert_eq!(*receiver.borrow(), None);
    }

    #[test]
    fn ordinary_windows_keep_project_identity_and_visible_tab_strip_default() {
        let options = WindowOptions::default();
        assert_eq!(options.app_id, crate::config::APP_ID);
        assert!(options.initial_tab_strip_visible);
        assert!(options.controller_title);
    }

    /// A trusted dialog asks for one fixed size, the same as its minimum
    /// and maximum, so tiling compositors float it: the natural size its
    /// text was laid out for, which is also its initial size. Other Windows
    /// ask for none.
    #[test]
    fn trusted_dialogs_ask_for_one_fixed_size_their_text_fits() {
        let lines = (0..18)
            .map(|line| crate::frontend::DialogLine::new(format!("line {line}")))
            .collect();
        let (decision, _receiver) = std::sync::mpsc::channel();
        let mut dialog = TrustedConsentUi::restart(decision)
            .with_text(crate::frontend::DialogText {
                lines,
                minimum_rows: 18,
            })
            .into_window_options();
        let snapshot = dialog.snapshot.clone().unwrap();
        // 18 rows of text take at most 72% of 25 rows; the narrowest width.
        assert_eq!((snapshot.columns, snapshot.rows), (64, 25));
        assert_eq!((dialog.initial_columns, dialog.initial_rows), (64, 25));
        let prepared = prepare_window(&mut dialog).unwrap();
        let fixed = prepared.fixed_size.unwrap();
        let frame = prepared.snapshot_frame.as_ref().unwrap();
        assert_eq!(fixed, frame.initial_logical_size(64, 25, 120).unwrap());
        assert_eq!(fixed, (prepared.initial_width, prepared.initial_height));

        let mut plain = WindowOptions {
            snapshot: Some(snapshot),
            initial_columns: 64,
            initial_rows: 25,
            ..WindowOptions::default()
        };
        assert_eq!(prepare_window(&mut plain).unwrap().fixed_size, None);
    }

    /// A trusted dialog draws in its floored colors from its first frame,
    /// even one shown before its text is fitted: preparing its Window
    /// writes them into its theme and its first snapshot.
    #[test]
    fn trusted_dialogs_draw_their_floored_colors_from_the_first_frame() {
        let (decision, _receiver) = std::sync::mpsc::channel();
        let mut theme = ResolvedTheme {
            background: 0x00_70_70_70,
            ..ResolvedTheme::default()
        };
        theme.roles.text = 0x00_76_76_76;
        let mut dialog = WindowOptions {
            theme,
            ..TrustedConsentUi::restart(decision)
                .with_text(crate::frontend::DialogText {
                    lines: vec![crate::frontend::DialogLine::new("question")],
                    minimum_rows: 1,
                })
                .into_window_options()
        };
        prepare_window(&mut dialog).unwrap();
        let colors = TrustedConsentColors::from_theme(theme);
        assert_eq!(dialog.theme, trusted_dialog_theme(theme));
        let snapshot = dialog.snapshot.as_ref().unwrap();
        assert_eq!(
            snapshot.default_colors[..2],
            [colors.text, colors.background]
        );
        assert!(crate::color::contrast_ratio(colors.text, colors.background) >= 4.5);
    }

    #[test]
    fn trusted_dialogs_map_with_their_own_app_id() {
        assert_eq!(
            window_app_id(true, crate::config::APP_ID),
            "com.oldjobobo.splinterm.trusted-dialog"
        );
        assert_eq!(
            window_app_id(false, crate::config::APP_ID),
            crate::config::APP_ID
        );
    }

    #[test]
    fn trusted_dialogs_map_with_their_own_titles() {
        let (decision, _receiver) = std::sync::mpsc::channel();
        let (consent_decision, _consent_receiver) = std::sync::mpsc::channel();
        let access = TrustedConsentUi::access_consent(
            consent_decision,
            vec![(splinterm_protocol::GrantDuration::Once, true)],
            Box::new(|_| crate::frontend::DialogText::default()),
        );
        let consent = WindowOptions {
            trusted_consent: Some(access),
            ..WindowOptions::default()
        };
        assert_eq!(
            initial_window_title(&consent),
            "Splinterm — Trusted Access Request"
        );
        let upgrade = WindowOptions {
            trusted_consent: Some(TrustedConsentUi::upgrade(decision.clone())),
            ..WindowOptions::default()
        };
        assert_eq!(initial_window_title(&upgrade), "Splinterm — Upgrade");
        let restart = WindowOptions {
            trusted_consent: Some(TrustedConsentUi::restart(decision)),
            ..WindowOptions::default()
        };
        assert_eq!(initial_window_title(&restart), "Splinterm — Upgrade");
    }

    #[test]
    fn trusted_dialog_keys_keep_the_consent_keysyms() {
        for (keysym, key) in [
            (Keysym::g, TrustedDialogKey::G),
            (Keysym::G, TrustedDialogKey::G),
            (Keysym::d, TrustedDialogKey::D),
            (Keysym::D, TrustedDialogKey::D),
            (Keysym::r, TrustedDialogKey::R),
            (Keysym::R, TrustedDialogKey::R),
            (Keysym::Return, TrustedDialogKey::Enter),
            (Keysym::KP_Enter, TrustedDialogKey::Enter),
            (Keysym::Escape, TrustedDialogKey::Escape),
            (Keysym::Up, TrustedDialogKey::Up),
            (Keysym::KP_Down, TrustedDialogKey::Down),
            (Keysym::Left, TrustedDialogKey::Left),
            (Keysym::Right, TrustedDialogKey::Right),
            (Keysym::y, TrustedDialogKey::Other),
            (Keysym::j, TrustedDialogKey::Other),
            (Keysym::space, TrustedDialogKey::Other),
        ] {
            assert_eq!(trusted_dialog_key(keysym), key, "{keysym:?}");
        }
    }

    /// Plan 0053 item 9: only the exact dialog shortcut engages; other
    /// Ctrl, Alt or Super chords, the chord's own `G` among them, never
    /// answer; plain keys keep their meaning.
    #[test]
    fn only_the_exact_dialog_shortcut_engages_and_other_chords_are_inert() {
        use splinterd::dialog_shortcut::DialogShortcut;
        let chord = Some(DialogShortcut::default());
        let held = |ctrl, alt, shift, logo| Modifiers {
            ctrl,
            alt,
            shift,
            logo,
            ..Modifiers::default()
        };
        let none = Modifiers::default();
        let ctrl_alt = held(true, true, false, false);
        assert_eq!(
            trusted_dialog_press(Keysym::g, ctrl_alt, chord),
            TrustedDialogKey::Engage
        );
        assert_eq!(
            trusted_dialog_press(Keysym::G, ctrl_alt, chord),
            TrustedDialogKey::Engage,
            "caps lock"
        );
        // Other Ctrl+Alt chords, and the chord's key with other modifiers,
        // engage nothing and answer nothing.
        for (keysym, modifiers) in [
            (Keysym::d, ctrl_alt),
            (Keysym::r, ctrl_alt),
            (Keysym::Return, ctrl_alt),
            (Keysym::Escape, ctrl_alt),
            (Keysym::h, ctrl_alt),
            (Keysym::G, held(true, true, true, false)),
            (Keysym::g, held(true, true, false, true)),
            (Keysym::g, held(true, false, false, false)),
            (Keysym::g, held(false, true, false, false)),
            (Keysym::g, held(false, false, false, true)),
            (Keysym::d, held(true, false, false, false)),
        ] {
            assert_eq!(
                trusted_dialog_press(keysym, modifiers, chord),
                TrustedDialogKey::Other,
                "{keysym:?} {modifiers:?}"
            );
        }
        // Without the shortcut, the chord is just another inert chord.
        assert_eq!(
            trusted_dialog_press(Keysym::g, ctrl_alt, None),
            TrustedDialogKey::Other
        );
        // Plain keys, and Shift, keep their meaning.
        assert_eq!(
            trusted_dialog_press(Keysym::g, none, chord),
            TrustedDialogKey::G
        );
        assert_eq!(
            trusted_dialog_press(Keysym::G, held(false, false, true, false), chord),
            TrustedDialogKey::G
        );
        assert_eq!(
            trusted_dialog_press(Keysym::Escape, none, None),
            TrustedDialogKey::Escape
        );
        // A configured chord engages only itself.
        let seven = DialogShortcut::parse_setting("ctrl+alt+7").unwrap();
        assert_eq!(
            trusted_dialog_press(Keysym::_7, ctrl_alt, seven),
            TrustedDialogKey::Engage
        );
        assert_eq!(
            trusted_dialog_press(Keysym::g, ctrl_alt, seven),
            TrustedDialogKey::Other
        );
        assert_eq!(
            trusted_dialog_press(Keysym::_7, held(true, true, false, true), seven),
            TrustedDialogKey::Other
        );
    }

    #[test]
    fn controller_title_only_drops_the_local_controller_suffix() {
        let mut snapshot = valid_snapshot(SplintId::new());
        snapshot.title = "shell".into();
        let (commands, _command_receiver) = tokio::sync::mpsc::channel(1);
        let terminal = WindowOptions {
            snapshot: Some(snapshot),
            commands: Some(commands),
            local_endpoint: true,
            ..WindowOptions::default()
        };
        assert_eq!(
            initial_window_title(&terminal),
            "Splinterm — shell — local controller"
        );
        let status = WindowOptions {
            controller_title: false,
            ..terminal
        };
        assert!(status.controlled, "input gating is unchanged");
        assert_eq!(initial_window_title(&status), "Splinterm — shell");
        let external = WindowOptions {
            authority: AuthorityStatus {
                grants: vec![(1, "agent".into())],
                development_bypass: false,
            },
            ..status
        };
        assert_eq!(
            initial_window_title(&external),
            "Splinterm — shell — EXTERNAL ACCESS ACTIVE",
            "authority labels stay visible"
        );
    }

    #[test]
    fn multi_pane_inputs_select_local_focus_and_reject_identity_mismatch() {
        let first = Splint::shell(PathBuf::from("/tmp"));
        let first_id = first.id;
        let second = Splint::shell(PathBuf::from("/tmp"));
        let second_id = second.id;
        let layout = LayoutNode::Branch {
            axis: Axis::Horizontal,
            ratio: SplitRatio::new(500).unwrap(),
            first: Box::new(LayoutNode::Leaf(first)),
            second: Box::new(LayoutNode::Leaf(second)),
        };
        let mut second_options = pane_options(second_id);
        second_options.terminal_grid_limits = TerminalGridLimits {
            maximum_columns: 12,
            maximum_rows: 8,
        };
        let mut options = WindowOptions {
            panes: vec![pane_options(first_id), second_options],
            layout: Some(layout.clone()),
            active_splint: Some(second_id),
            ..WindowOptions::default()
        };
        let inactive = options.activate_multi_pane_input().unwrap();
        assert_eq!(
            options.snapshot.as_ref().map(|snapshot| snapshot.splint_id),
            Some(second_id)
        );
        assert_eq!(
            options.terminal_grid_limits,
            TerminalGridLimits {
                maximum_columns: 12,
                maximum_rows: 8,
            }
        );
        assert_eq!(inactive.len(), 1);
        assert_eq!(inactive[0].snapshot.splint_id, first_id);

        let mut invalid = WindowOptions {
            panes: vec![pane_options(first_id), pane_options(first_id)],
            layout: Some(layout),
            ..WindowOptions::default()
        };
        assert!(invalid.activate_multi_pane_input().is_err());
    }

    #[test]
    fn live_viewport_skips_previous_history_but_detached_retains_transition_rows() {
        let live_case = ReducerBenchCase {
            history_rows: MAX_CACHED_HISTORY_ROWS,
            detached: false,
            batch_size: 1,
            mode: ReducerBenchMode::FocusedRole,
        };
        let live = reducer_bench_pane(live_case);
        assert!(
            live.history_rows_needed_for_viewport_transition()
                .is_empty()
        );

        let detached = reducer_bench_pane(ReducerBenchCase {
            detached: true,
            ..live_case
        });
        let rows = detached.history_rows_needed_for_viewport_transition();
        assert_eq!(rows.len(), MAX_CACHED_HISTORY_ROWS);
        assert_eq!(rows.first().and_then(|row| row.row_id), Some(1));
        assert_eq!(
            rows.last().and_then(|row| row.row_id),
            Some(u64::try_from(MAX_CACHED_HISTORY_ROWS).unwrap())
        );
        assert_eq!(live.trace_pane_role("focused"), "focused");
        assert_eq!(detached.trace_pane_role("focused"), "detached-viewport");
    }

    #[test]
    fn pane_reducer_benchmark_contract_is_bounded_and_role_explicit() {
        let smoke = reducer_bench_cases(true);
        let full = reducer_bench_cases(false);
        assert_eq!(smoke.len(), 8);
        assert_eq!(full.len(), 24);
        assert!(full.iter().all(|case| {
            [0, 1_000, MAX_CACHED_HISTORY_ROWS].contains(&case.history_rows)
                && [1, 16, 64].contains(&case.batch_size)
        }));
        assert!(full.iter().any(|case| case.detached));
        assert!(
            full.iter()
                .any(|case| { matches!(case.mode, ReducerBenchMode::FocusedRole) })
        );
        assert!(
            full.iter()
                .any(|case| { matches!(case.mode, ReducerBenchMode::InactiveBatch) })
        );
        for case in smoke {
            std::hint::black_box(measure_reducer_bench_case(case));
        }
    }

    #[test]
    #[ignore = "manual release timing harness; writes the requested JSON report"]
    fn pane_reducer_timing_harness() {
        let smoke = env::var_os("SPLINTERM_PANE_REDUCER_SMOKE").is_some();
        let warmup_runs = if smoke { 0 } else { 5 };
        let sample_runs = if smoke { 1 } else { 30 };
        let cases = reducer_bench_cases(smoke);
        let mut durations = vec![Vec::with_capacity(sample_runs); cases.len()];
        for round in 0..warmup_runs + sample_runs {
            let rotation = if cases.is_empty() {
                0
            } else {
                (round * 7) % cases.len()
            };
            for offset in 0..cases.len() {
                let index = (rotation + offset) % cases.len();
                let duration = measure_reducer_bench_case(cases[index]);
                if round >= warmup_runs {
                    durations[index].push(duration);
                }
            }
        }
        let records = cases
            .iter()
            .enumerate()
            .map(|(index, case)| {
                let mode = match case.mode {
                    ReducerBenchMode::FocusedRole => "focused-role",
                    ReducerBenchMode::InactiveBatch => "inactive-batch",
                };
                serde_json::json!({
                    "name": format!(
                        "{}-h{}-{}-b{}",
                        mode,
                        case.history_rows,
                        if case.detached { "detached" } else { "live" },
                        case.batch_size,
                    ),
                    "mode": mode,
                    "history_rows": case.history_rows,
                    "viewport": if case.detached { "detached" } else { "live" },
                    "batch_size": case.batch_size,
                    "duration_ns": durations[index],
                })
            })
            .collect::<Vec<_>>();
        let report = serde_json::json!({
            "schema": "splinterm.performance.pane-reducer.v1",
            "clock": "std::time::Instant monotonic process clock",
            "build_profile": if cfg!(debug_assertions) { "debug" } else { "release" },
            "warmup_runs": warmup_runs,
            "sample_runs": sample_runs,
            "history_capacity_rows": MAX_CACHED_HISTORY_ROWS,
            "smoke": smoke,
            "focused_role_scope": "PaneView semantic reducer only; not the full App::apply_updates active path",
            "cases": records,
        });
        let path = env::var_os("SPLINTERM_PANE_REDUCER_REPORT").map_or_else(
            || PathBuf::from("/tmp/splinterm-pane-reducer-report.json"),
            PathBuf::from,
        );
        let temporary = path.with_file_name(format!(
            ".{}.tmp",
            path.file_name().unwrap().to_string_lossy()
        ));
        fs::write(&temporary, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        fs::rename(temporary, path).unwrap();
    }

    #[test]
    fn live_shaping_cached_pane_cursor_updates_match_full_without_frame_rebuild() {
        use crate::font_shaping::{FeatureSettings, FontLigatures};
        let context = RenderContext::new(u16::MAX).with_test_shaping(
            FontLigatures::Cursor,
            FeatureSettings::parse("calt=1").unwrap(),
        );
        let theme = ResolvedTheme::default();
        for role in ["visible-inactive", "hidden"] {
            for history in [false, true] {
                let mut options = pane_options(SplintId::new());
                options.snapshot.columns = 2;
                options.snapshot.rows = 2;
                let rows: Vec<_> = (1..=3)
                    .map(|id| {
                        let mut row = blank_row(2);
                        row.row_id = Some(id);
                        row.cells[0].content = "!".into();
                        row.cells[1].content = "=".into();
                        row
                    })
                    .collect();
                options.snapshot.visible_rows = rows[1..].to_vec();
                options.snapshot.scrollback_rows = rows[..1].to_vec();
                options.snapshot.available_scrollback_rows = 1;
                options.snapshot.oldest_available_scrollback_row_id = Some(1);
                options.snapshot.newest_available_scrollback_row_id = Some(1);
                apply_theme(&mut options.snapshot, theme);
                let mut pane = PaneView::from_options_with_context(options, 120, &context).unwrap();
                if history {
                    pane.scrollback_viewport
                        .scroll_up(1, pane.snapshot.as_ref().unwrap());
                    rebuild_pane_scaled_frame_with_context(&mut pane, 120, &context).unwrap();
                }
                for (column, row, visible) in [(1, 0, true), (0, 1, true), (0, 1, false)] {
                    let snapshot = pane.snapshot.as_ref().unwrap();
                    let mut update = empty_update();
                    update.base_revision = snapshot.revision;
                    update.revision = snapshot.revision + 1;
                    update.cursor = Some(splinterm_protocol::TerminalCursor {
                        column,
                        row,
                        deferred_wrap: false,
                    });
                    let mut modes = snapshot.input_modes;
                    modes.cursor_visible = visible;
                    update.input_modes = Some(modes);
                    let impact = pane
                        .apply_background_update(
                            WindowUpdate::Update {
                                update,
                                image_sources: None,
                                trace: None,
                            },
                            theme,
                            role,
                        )
                        .unwrap();
                    assert!(impact.visual_changed);
                    assert!(
                        !impact.frame_dirty,
                        "cursor-only {role} update must not request a whole frame rebuild"
                    );
                    let display = pane.display_snapshot().unwrap();
                    let frame = pane.snapshot_frame.as_ref().unwrap();
                    let full =
                        SnapshotFrame::load_scaled_with_context(&display, 120, &context).unwrap();
                    let geometry = frame.window_geometry(160, 100, 120).unwrap();
                    let mut actual = vec![0; 160 * 100 * 4];
                    let mut expected = actual.clone();
                    crate::renderer::paint_snapshot(
                        &mut actual,
                        160,
                        100,
                        frame,
                        &geometry,
                        true,
                        crate::config::CursorStyle::Block,
                    );
                    crate::renderer::paint_snapshot(
                        &mut expected,
                        160,
                        100,
                        &full,
                        &geometry,
                        true,
                        crate::config::CursorStyle::Block,
                    );
                    assert_eq!(actual, expected, "cached {role}, history={history}");
                }
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "mixed inactive damage keeps the real reducer, rebuild selection, and pixel checks together"
    )]
    fn live_shaping_mixed_inactive_updates_repaint_both_changed_panes() {
        use crate::font_shaping::{FeatureSettings, FontLigatures};
        let context = RenderContext::new(u16::MAX).with_test_shaping(
            FontLigatures::Cursor,
            FeatureSettings::parse("calt=1").unwrap(),
        );
        let theme = ResolvedTheme::default();
        let render = |frame: &SnapshotFrame| {
            let geometry = frame.window_geometry(160, 100, 120).unwrap();
            let mut pixels = vec![0; 160 * 100 * 4];
            crate::renderer::paint_snapshot(
                &mut pixels,
                160,
                100,
                frame,
                &geometry,
                false,
                crate::config::CursorStyle::Block,
            );
            pixels
        };
        for order in [[0, 1], [1, 0]] {
            let ids = [SplintId::new(), SplintId::new(), SplintId::new()];
            let mut panes: Vec<_> = ids
                .iter()
                .map(|id| {
                    let mut options = pane_options(*id);
                    options.snapshot.columns = 2;
                    options.snapshot.rows = 1;
                    options.snapshot.input_modes.cursor_visible = false;
                    let mut row = blank_row(2);
                    row.row_id = options.snapshot.visible_rows[0].row_id;
                    row.cells[0].content = "!".into();
                    row.cells[1].content = "=".into();
                    options.snapshot.visible_rows = vec![row];
                    apply_theme(&mut options.snapshot, theme);
                    PaneView::from_options_with_context(options, 120, &context).unwrap()
                })
                .collect();
            let mut pixels: Vec<_> = panes
                .iter()
                .map(|pane| render(pane.snapshot_frame.as_ref().unwrap()))
                .collect();
            let before = pixels.clone();
            let mut drain = InactiveUpdateDrain::default();
            for index in order {
                let pane = &mut panes[index];
                let snapshot = pane.snapshot.as_ref().unwrap();
                let mut update = empty_update();
                update.base_revision = snapshot.revision;
                update.revision = snapshot.revision + 1;
                if index == 0 {
                    update.cursor = Some(splinterm_protocol::TerminalCursor {
                        column: 1,
                        row: 0,
                        deferred_wrap: false,
                    });
                    let mut modes = snapshot.input_modes;
                    modes.cursor_visible = true;
                    update.input_modes = Some(modes);
                } else {
                    let mut row = snapshot.visible_rows[0].clone();
                    row.cells[0].content = "a".into();
                    row.cells[1].content = "b".into();
                    update
                        .rows
                        .push(splinterm_protocol::TerminalRowPatch { index: 0, row });
                }
                let impact = apply_inactive_update_batch(
                    pane,
                    [WindowUpdate::Update {
                        update,
                        image_sources: None,
                        trace: None,
                    }],
                    theme,
                )
                .unwrap();
                assert!(impact.visual_changed);
                assert_eq!(impact.frame_dirty, index == 1);
                drain.record(Some(ids[index]), impact);
            }
            assert!(drain.changed);
            assert_eq!(drain.dirty_frames, HashSet::from([ids[1]]));
            assert_eq!(
                rebuild_inactive_frames_with_context(
                    &mut panes,
                    &drain.dirty_frames,
                    false,
                    120,
                    &context,
                )
                .unwrap(),
                1,
                "only the content pane needs a frame rebuild"
            );
            assert_eq!(drain.panes_to_repaint(), &HashSet::from([ids[0], ids[1]]));
            for (index, pane) in panes.iter().enumerate() {
                if drain.panes_to_repaint().contains(&ids[index]) {
                    pixels[index] = render(pane.snapshot_frame.as_ref().unwrap());
                }
                let display = pane.display_snapshot().unwrap();
                let full =
                    SnapshotFrame::load_scaled_with_context(&display, 120, &context).unwrap();
                assert_eq!(
                    pixels[index],
                    render(&full),
                    "pane {index}, order {order:?}"
                );
            }
            assert_ne!(
                pixels[0], before[0],
                "cursor break changes inactive ligature ink"
            );
            assert_ne!(pixels[1], before[1], "content update changes peer ink");
            assert_eq!(pixels[2], before[2], "untouched pane stays unchanged");
        }
    }

    #[test]
    fn inactive_pane_reducer_applies_only_contiguous_matching_updates() {
        let splint_id = SplintId::new();
        let mut pane = PaneView::from_options(pane_options(splint_id), SCALE_DENOMINATOR).unwrap();
        let mut update = empty_update();
        update.title = Some("background pane".into());
        assert!(
            pane.apply_background_update(
                WindowUpdate::Update {
                    update,
                    image_sources: None,
                    trace: None,
                },
                ResolvedTheme::default(),
                "visible-inactive",
            )
            .unwrap()
            .visual_changed
        );
        assert_eq!(pane.snapshot.as_ref().unwrap().revision, 2);
        assert_eq!(pane.snapshot.as_ref().unwrap().title, "background pane");

        let mut stale = empty_update();
        stale.base_revision = 1;
        stale.revision = 3;
        assert!(
            pane.apply_background_update(
                WindowUpdate::Update {
                    update: stale,
                    image_sources: None,
                    trace: None,
                },
                ResolvedTheme::default(),
                "visible-inactive",
            )
            .is_err()
        );
    }

    #[test]
    fn authoritative_equal_revision_snapshot_rebuilds_a_diverged_pane() {
        let splint_id = SplintId::new();
        let mut pane = PaneView::from_options(pane_options(splint_id), SCALE_DENOMINATOR).unwrap();
        let revision = pane.snapshot.as_ref().unwrap().revision;
        let mut authoritative = pane.snapshot.as_ref().unwrap().clone();
        authoritative.visible_rows[0].cells[0].content = "authoritative-live".into();
        pane.snapshot_frame = None;

        let impact = apply_inactive_update_batch(
            &mut pane,
            [WindowUpdate::Snapshot {
                snapshot: authoritative,
                image_sources: ImageContentLeaseSet::default(),
                authoritative: true,
            }],
            ResolvedTheme::default(),
        )
        .unwrap();

        assert_eq!(impact, BackgroundUpdateImpact::FRAME);
        assert_eq!(pane.snapshot.as_ref().unwrap().revision, revision);
        assert_eq!(
            pane.snapshot.as_ref().unwrap().visible_rows[0].cells[0].content,
            "authoritative-live"
        );
        assert_eq!(
            rebuild_inactive_frames(
                std::slice::from_mut(&mut pane),
                &HashSet::from([splint_id]),
                false,
                SCALE_DENOMINATOR,
            )
            .unwrap(),
            1
        );
        assert!(pane.snapshot_frame.is_some());
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one regression preserves the complete inactive-pane burst and rebuild lifecycle"
    )]
    fn inactive_pane_batch_defers_one_rebuild_until_after_contiguous_burst() {
        let splint_id = SplintId::new();
        let mut pane = PaneView::from_options(pane_options(splint_id), SCALE_DENOMINATOR).unwrap();
        let mut first = empty_update();
        let mut first_row = pane.snapshot.as_ref().unwrap().visible_rows[0].clone();
        first_row.cells[0].content = "first".into();
        first.rows.push(splinterm_protocol::TerminalRowPatch {
            index: 0,
            row: first_row,
        });
        let mut second = empty_update();
        second.base_revision = 2;
        second.revision = 3;
        let mut second_row = pane.snapshot.as_ref().unwrap().visible_rows[0].clone();
        second_row.cells[0].content = "second".into();
        second.rows.push(splinterm_protocol::TerminalRowPatch {
            index: 0,
            row: second_row,
        });
        pane.snapshot_frame = None;

        let impact = apply_inactive_update_batch(
            &mut pane,
            [
                WindowUpdate::Update {
                    update: first,
                    image_sources: None,
                    trace: Some(PerfTraceCorrelation {
                        base_revision: 1,
                        revision: 2,
                        subscription_id: 9,
                        transaction_sequence: 1,
                    }),
                },
                WindowUpdate::Update {
                    update: second,
                    image_sources: None,
                    trace: Some(PerfTraceCorrelation {
                        base_revision: 2,
                        revision: 3,
                        subscription_id: 9,
                        transaction_sequence: 2,
                    }),
                },
            ],
            ResolvedTheme::default(),
        )
        .unwrap();
        let mut metadata_only = empty_update();
        metadata_only.base_revision = 3;
        metadata_only.revision = 4;
        metadata_only.title = Some("metadata-only".into());
        assert_eq!(
            pane.apply_background_update(
                WindowUpdate::Update {
                    update: metadata_only,
                    image_sources: None,
                    trace: Some(PerfTraceCorrelation {
                        base_revision: 3,
                        revision: 4,
                        subscription_id: 9,
                        transaction_sequence: 3,
                    }),
                },
                ResolvedTheme::default(),
                "visible-inactive",
            )
            .unwrap(),
            BackgroundUpdateImpact::VISUAL
        );
        assert_eq!(impact, BackgroundUpdateImpact::FRAME);
        assert!(pane.snapshot_frame.is_none());
        assert_eq!(
            pane.trace_correlation,
            Some(PerfTraceCorrelation {
                base_revision: 2,
                revision: 3,
                subscription_id: 9,
                transaction_sequence: 2,
            })
        );
        assert_eq!(pane.trace_superseded_revisions, 1);
        let pending_commit = pane.pending_commit_trace().unwrap();
        assert_eq!(pending_commit.correlation.revision, 3);
        assert_eq!(pending_commit.pane_role, "visible-inactive");
        let commit_event = pane_commit_event(pending_commit, 7);
        assert_eq!(commit_event.revision, Some(3));
        assert_eq!(commit_event.transaction_sequence, Some(2));
        assert_eq!(commit_event.commit_sequence, Some(7));
        assert_eq!(
            rebuild_inactive_frames(
                std::slice::from_mut(&mut pane),
                &HashSet::from([splint_id]),
                false,
                SCALE_DENOMINATOR,
            )
            .unwrap(),
            1
        );
        let snapshot = pane.snapshot.as_ref().unwrap();
        assert_eq!(snapshot.revision, 4);
        assert_eq!(snapshot.visible_rows[0].cells[0].content, "second");

        let other_id = SplintId::new();
        let mut newly_focused =
            PaneView::from_options(pane_options(other_id), SCALE_DENOMINATOR).unwrap();
        newly_focused.retain_trace_correlation(
            Some(PerfTraceCorrelation {
                base_revision: 1,
                revision: 2,
                subscription_id: 10,
                transaction_sequence: 99,
            }),
            "hidden",
        );
        std::mem::swap(&mut pane, &mut newly_focused);
        let traces = pending_pane_commit_traces(&pane, &[newly_focused]);
        assert_eq!(traces.len(), 2);
        assert!(
            traces
                .iter()
                .any(|trace| trace.correlation.transaction_sequence == 2)
        );
        assert!(
            traces
                .iter()
                .any(|trace| trace.correlation.transaction_sequence == 99)
        );
    }

    #[test]
    fn bounded_detached_display_matches_legacy_snapshot_semantics() {
        let mut initial = valid_snapshot(SplintId::new());
        initial.rows = 2;
        initial.visible_rows = vec![history_row(4, 0), history_row(5, 0)];
        initial.scrollback_rows = vec![history_row(1, 0), history_row(2, 0), history_row(3, 0)];
        initial.available_scrollback_rows = 3;
        initial.oldest_available_scrollback_row_id = Some(1);
        initial.newest_available_scrollback_row_id = Some(3);
        let (_updates, update_receiver) = tokio::sync::mpsc::channel(1);
        let (commands, _command_receiver) = tokio::sync::mpsc::channel(1);
        let mut pane = PaneView::from_options(
            WindowPaneOptions {
                snapshot: initial,
                terminal_grid_limits: TerminalGridLimits::default(),
                updates: update_receiver,
                commands,
                authority: AuthorityStatus::default(),
                controlled: false,
                image_sources: ImageContentLeaseSet::default(),
            },
            SCALE_DENOMINATOR,
        )
        .unwrap();
        pane.scrollback_viewport
            .scroll_up(2, pane.snapshot.as_ref().unwrap());

        let snapshot = pane.snapshot.as_ref().unwrap();
        let mut expected = snapshot.clone();
        let cursor_row = viewport_cursor_row(
            snapshot.cursor_row,
            pane.scrollback_viewport.offset_from_bottom(),
            snapshot.rows,
        );
        if cursor_row.is_none() {
            expected.input_modes.cursor_visible = false;
        }
        expected.cursor_column = cursor_row.map_or(-1, |_| snapshot.cursor_column);
        expected.cursor_row = cursor_row.unwrap_or(-1);
        expected.cursor_deferred_wrap = false;
        expected.visible_rows = pane
            .scrollback_viewport
            .visible_rows(snapshot)
            .into_iter()
            .cloned()
            .collect();
        expected.oldest_available_scrollback_row_id = None;
        expected.newest_available_scrollback_row_id = None;
        expected.scrollback_rows.clear();
        expected.omitted_oldest_scrollback_rows = expected.available_scrollback_rows;

        assert_eq!(pane.display_snapshot().unwrap(), expected);
        assert_eq!(snapshot.scrollback_rows.len(), 3);
    }

    #[test]
    fn inactive_detached_pane_batches_snapshot_and_theme_into_one_anchored_frame() {
        let splint_id = SplintId::new();
        let mut initial = valid_snapshot(splint_id);
        initial.visible_rows[0].row_id = Some(3);
        initial.visible_rows[0].cells[0].content = "visible-three".into();
        initial.scrollback_rows = vec![history_row(1, 0), history_row(2, 0)];
        initial.available_scrollback_rows = 2;
        initial.oldest_available_scrollback_row_id = Some(1);
        initial.newest_available_scrollback_row_id = Some(2);
        let (_updates, update_receiver) = tokio::sync::mpsc::channel(1);
        let (commands, _command_receiver) = tokio::sync::mpsc::channel(1);
        let mut pane = PaneView::from_options(
            WindowPaneOptions {
                snapshot: initial,
                terminal_grid_limits: TerminalGridLimits::default(),
                updates: update_receiver,
                commands,
                authority: AuthorityStatus::default(),
                controlled: false,
                image_sources: ImageContentLeaseSet::default(),
            },
            SCALE_DENOMINATOR,
        )
        .unwrap();
        pane.scrollback_viewport
            .scroll_up(1, pane.snapshot.as_ref().unwrap());

        let mut next = pane.snapshot.as_ref().unwrap().clone();
        next.revision = 2;
        next.scrollback_rows.push(next.visible_rows[0].clone());
        next.available_scrollback_rows = 3;
        next.newest_available_scrollback_row_id = Some(3);
        next.visible_rows[0].row_id = Some(4);
        next.visible_rows[0].cells[0].content = "live-four".into();
        pane.snapshot_frame = None;
        let impact = apply_inactive_update_batch(
            &mut pane,
            [WindowUpdate::Snapshot {
                snapshot: next,
                image_sources: ImageContentLeaseSet::default(),
                authoritative: false,
            }],
            ResolvedTheme::default(),
        )
        .unwrap();
        assert_eq!(impact, BackgroundUpdateImpact::FRAME);
        assert!(pane.snapshot_frame.is_none());
        let theme = ResolvedTheme {
            background: 0x12_34_56,
            ..ResolvedTheme::default()
        };
        apply_theme(pane.snapshot.as_mut().unwrap(), theme);
        let expected_offset = pane.scrollback_viewport.offset_from_bottom();
        assert_eq!(
            rebuild_inactive_frames(
                std::slice::from_mut(&mut pane),
                &HashSet::new(),
                true,
                SCALE_DENOMINATOR,
            )
            .unwrap(),
            1
        );

        assert!(!pane.scrollback_viewport.is_live());
        assert_eq!(pane.rendered_viewport_offset, expected_offset);
        let display = pane.display_snapshot().unwrap();
        assert_eq!(display.visible_rows[0].row_id, Some(2));
        assert_ne!(display.visible_rows[0].cells[0].content, "live-four");
    }

    #[test]
    fn dirty_viewport_frame_rebuilds_after_pane_becomes_inactive() {
        let mut initial = valid_snapshot(SplintId::new());
        initial.visible_rows[0].row_id = Some(2);
        initial.scrollback_rows = vec![history_row(1, 0)];
        initial.available_scrollback_rows = 1;
        initial.oldest_available_scrollback_row_id = Some(1);
        initial.newest_available_scrollback_row_id = Some(1);
        let (_updates, update_receiver) = tokio::sync::mpsc::channel(1);
        let (commands, _command_receiver) = tokio::sync::mpsc::channel(1);
        let mut formerly_active = PaneView::from_options(
            WindowPaneOptions {
                snapshot: initial,
                terminal_grid_limits: TerminalGridLimits::default(),
                updates: update_receiver,
                commands,
                authority: AuthorityStatus::default(),
                controlled: false,
                image_sources: ImageContentLeaseSet::default(),
            },
            SCALE_DENOMINATOR,
        )
        .unwrap();
        formerly_active
            .scrollback_viewport
            .scroll_up(1, formerly_active.snapshot.as_ref().unwrap());
        formerly_active.viewport_dirty = true;

        let mut newly_active =
            PaneView::from_options(pane_options(SplintId::new()), SCALE_DENOMINATOR).unwrap();
        std::mem::swap(&mut formerly_active, &mut newly_active);
        assert!(newly_active.viewport_dirty);
        assert_eq!(newly_active.rendered_viewport_offset, 0);
        newly_active.scroll_started_at = Some(Instant::now());
        newly_active
            .pending_scrolls
            .push(splinterm_protocol::TerminalScroll {
                direction: splinterm_protocol::ScrollDirection::Reverse,
                start_row: 0,
                end_row: 1,
                rows: 1,
            });

        assert!(rebuild_dirty_pane_viewport_frame(&mut newly_active, SCALE_DENOMINATOR).unwrap());
        assert!(!newly_active.viewport_dirty);
        assert_eq!(newly_active.rendered_viewport_offset, 1);
        assert!(newly_active.scroll_started_at.is_none());
        assert!(newly_active.pending_scrolls.is_empty());

        newly_active.scrollback_viewport.return_to_live();
        newly_active.viewport_dirty = true;
        assert!(rebuild_dirty_pane_viewport_frame(&mut newly_active, SCALE_DENOMINATOR).unwrap());
        assert_eq!(newly_active.rendered_viewport_offset, 0);
        assert!(newly_active.scrollback_viewport.is_live());
    }

    #[test]
    fn active_and_inactive_pane_frames_rebuild_at_fractional_scale() {
        let mut active = PaneView::from_options(pane_options(SplintId::new()), 120).unwrap();
        let mut inactive = PaneView::from_options(pane_options(SplintId::new()), 120).unwrap();
        assert!(rebuild_pane_scaled_frame(&mut active, 150).unwrap());
        assert!(rebuild_pane_scaled_frame(&mut inactive, 150).unwrap());
        assert_eq!(active.snapshot_frame.as_ref().unwrap().scale_120(), 150);
        assert_eq!(inactive.snapshot_frame.as_ref().unwrap().scale_120(), 150);
    }

    #[test]
    fn pane_resize_uses_its_local_rectangle_and_suppresses_duplicates() {
        let splint_id = SplintId::new();
        let (_updates, update_receiver) = tokio::sync::mpsc::channel(1);
        let (commands, mut command_receiver) = tokio::sync::mpsc::channel(2);
        let mut pane = PaneView::from_options(
            WindowPaneOptions {
                snapshot: valid_snapshot(splint_id),
                terminal_grid_limits: TerminalGridLimits::default(),
                updates: update_receiver,
                commands,
                authority: AuthorityStatus::default(),
                controlled: false,
                image_sources: ImageContentLeaseSet::default(),
            },
            SCALE_DENOMINATOR,
        )
        .unwrap();
        App::emit_pane_resize(&mut pane, 320, 240, SCALE_DENOMINATOR, true).unwrap();
        let first = command_receiver.try_recv().unwrap();
        assert!(matches!(first, WindowCommand::Resize { .. }));
        App::emit_pane_resize(&mut pane, 320, 240, SCALE_DENOMINATOR, true).unwrap();
        assert!(command_receiver.try_recv().is_err());
    }

    #[test]
    fn observer_font_resize_prepares_without_claiming_control() {
        let splint_id = SplintId::new();
        let (_updates, update_receiver) = tokio::sync::mpsc::channel(1);
        let (commands, mut command_receiver) = tokio::sync::mpsc::channel(2);
        let mut pane = PaneView::from_options(
            WindowPaneOptions {
                snapshot: valid_snapshot(splint_id),
                terminal_grid_limits: TerminalGridLimits::default(),
                updates: update_receiver,
                commands,
                authority: AuthorityStatus::default(),
                controlled: false,
                image_sources: ImageContentLeaseSet::default(),
            },
            SCALE_DENOMINATOR,
        )
        .unwrap();
        App::emit_pane_resize(&mut pane, 320, 240, SCALE_DENOMINATOR, false).unwrap();
        assert!(matches!(
            command_receiver.try_recv().unwrap(),
            WindowCommand::PrepareResize { .. }
        ));
        App::emit_pane_resize(&mut pane, 320, 240, SCALE_DENOMINATOR, false).unwrap();
        assert!(command_receiver.try_recv().is_err());
    }

    #[test]
    fn pane_resize_respects_same_version_endpoint_grid_limits() {
        let splint_id = SplintId::new();
        let (_updates, update_receiver) = tokio::sync::mpsc::channel(1);
        let (commands, mut command_receiver) = tokio::sync::mpsc::channel(1);
        let mut pane = PaneView::from_options(
            WindowPaneOptions {
                snapshot: valid_snapshot(splint_id),
                terminal_grid_limits: TerminalGridLimits {
                    maximum_columns: 12,
                    maximum_rows: 8,
                },
                updates: update_receiver,
                commands,
                authority: AuthorityStatus::default(),
                controlled: false,
                image_sources: ImageContentLeaseSet::default(),
            },
            SCALE_DENOMINATOR,
        )
        .unwrap();

        App::emit_pane_resize(&mut pane, 2_000, 2_000, SCALE_DENOMINATOR, true).unwrap();
        assert!(matches!(
            command_receiver.try_recv().unwrap(),
            WindowCommand::Resize {
                columns: 12,
                rows: 8,
                ..
            }
        ));
        assert!(pane.grid_cap_diagnostic_emitted);
        let frame = pane.snapshot_frame.as_ref().unwrap();
        let geometry = pane
            .window_geometry(frame, 2_000, 2_000, SCALE_DENOMINATOR)
            .unwrap();
        assert!(geometry.column_capped && geometry.row_capped);
        assert!(geometry.residual_right > 0 && geometry.residual_bottom > 0);
        assert_eq!(
            frame.cell_at(
                f64::from(geometry.grid_rect.x + geometry.grid_rect.width + 1),
                f64::from(geometry.grid_rect.y + 1),
                &geometry,
            ),
            None
        );
        let (cursor_x, cursor_y, cursor_width, cursor_height) =
            frame.cursor_rectangle(&geometry).unwrap();
        assert!(cursor_x >= 0 && cursor_y >= 0);
        assert!(
            u32::try_from(cursor_x + cursor_width).unwrap()
                <= geometry.grid_rect.x + geometry.grid_rect.width
        );
        assert!(
            u32::try_from(cursor_y + cursor_height).unwrap()
                <= geometry.grid_rect.y + geometry.grid_rect.height
        );
        App::emit_pane_resize(&mut pane, 2_000, 2_000, SCALE_DENOMINATOR, true).unwrap();
        assert!(command_receiver.try_recv().is_err());
        assert!(pane.grid_cap_diagnostic_emitted);
    }

    fn history_row(id: u64, content_bytes: usize) -> TerminalRow {
        let mut row = blank_row(1);
        row.row_id = Some(id);
        row.cells[0].content = "x".repeat(content_bytes);
        row
    }

    #[test]
    fn saturated_pane_resize_defers_without_advancing_and_retries_exactly() {
        let splint_id = SplintId::new();
        let (_updates, update_receiver) = tokio::sync::mpsc::channel(1);
        let (commands, mut command_receiver) = tokio::sync::mpsc::channel(1);
        let mut pane = PaneView::from_options(
            WindowPaneOptions {
                snapshot: valid_snapshot(splint_id),
                terminal_grid_limits: TerminalGridLimits::default(),
                updates: update_receiver,
                commands: commands.clone(),
                authority: AuthorityStatus::default(),
                controlled: false,
                image_sources: ImageContentLeaseSet::default(),
            },
            SCALE_DENOMINATOR,
        )
        .unwrap();
        App::emit_pane_resize(&mut pane, 320, 240, SCALE_DENOMINATOR, true).unwrap();
        assert!(matches!(
            command_receiver.try_recv().unwrap(),
            WindowCommand::Resize { .. }
        ));
        let previous = pane.last_resize;
        commands
            .try_send(WindowCommand::Input(vec![1]))
            .expect("fill pane command queue");

        App::emit_pane_resize(&mut pane, 640, 480, SCALE_DENOMINATOR, false)
            .expect("queue saturation defers resize");
        assert_eq!(pane.last_resize, previous);
        assert!(pane.pending_resize.is_some());
        assert!(pane_command_retry_pending(&pane));
        assert_eq!(
            command_receiver.try_recv().unwrap(),
            WindowCommand::Input(vec![1])
        );

        App::retry_pending_pane_resize(&mut pane).expect("deferred resize retries");
        let retried = command_receiver.try_recv().unwrap();
        assert!(matches!(retried, WindowCommand::PrepareResize { .. }));
        assert_ne!(pane.last_resize, previous);
        assert!(pane.pending_resize.is_none());
        assert!(!pane_command_retry_pending(&pane));

        commands
            .try_send(WindowCommand::Input(vec![2]))
            .expect("refill pane command queue");
        App::emit_pane_resize(&mut pane, 800, 600, SCALE_DENOMINATOR, true)
            .expect("second resize defers");
        assert!(pane.pending_resize.is_some());
        assert_eq!(
            command_receiver.try_recv().unwrap(),
            WindowCommand::Input(vec![2])
        );
        drop(command_receiver);
        let error = App::retry_pending_pane_resize(&mut pane)
            .expect_err("disconnected deferred resize receiver");
        assert!(error.to_string().contains("disconnected"));
        assert!(pane.pending_resize.is_none());
    }

    #[test]
    fn uncontrolled_opened_tab_claims_control_for_every_active_resize() {
        let splint = Splint::shell(PathBuf::from("/tmp"));
        let splint_id = splint.id;
        let (_updates, update_receiver) = tokio::sync::mpsc::channel(1);
        let (commands, mut command_receiver) = tokio::sync::mpsc::channel(2);
        let view = DojoTabView::from_open(
            WindowDojoIdentity {
                topology_revision: TopologyRevision::new(1),
                lair_id: LairId::new(),
                dojo_id: DojoId::new(),
                lair_name: "test lair".to_owned(),
                lair_retention: splinterm_core::LairRetention::Disposable,
                dojo_name: "test dojo".to_owned(),
            },
            LayoutNode::Leaf(splint),
            vec![WindowPaneOptions {
                snapshot: valid_snapshot(splint_id),
                terminal_grid_limits: TerminalGridLimits::default(),
                updates: update_receiver,
                commands,
                authority: AuthorityStatus::default(),
                controlled: false,
                image_sources: ImageContentLeaseSet::default(),
            }],
            splint_id,
            ResolvedTheme::default(),
            SCALE_DENOMINATOR,
            &RenderContext::new(u16::MAX),
        )
        .unwrap();
        let mut pane = view.pane;

        App::emit_active_pane_resize(&mut pane, 320, 240, SCALE_DENOMINATOR).unwrap();
        assert!(matches!(
            command_receiver.try_recv().unwrap(),
            WindowCommand::Resize { .. }
        ));

        App::emit_active_pane_resize(&mut pane, 640, 480, SCALE_DENOMINATOR).unwrap();
        assert!(matches!(
            command_receiver.try_recv().unwrap(),
            WindowCommand::Resize { .. }
        ));
    }

    #[test]
    fn controlled_inactive_pane_applies_resize_after_layout_change() {
        let splint_id = SplintId::new();
        let (_updates, update_receiver) = tokio::sync::mpsc::channel(1);
        let (commands, mut command_receiver) = tokio::sync::mpsc::channel(1);
        let mut pane = PaneView::from_options(
            WindowPaneOptions {
                snapshot: valid_snapshot(splint_id),
                terminal_grid_limits: TerminalGridLimits::default(),
                updates: update_receiver,
                commands,
                authority: AuthorityStatus::default(),
                controlled: true,
                image_sources: ImageContentLeaseSet::default(),
            },
            SCALE_DENOMINATOR,
        )
        .unwrap();

        App::emit_inactive_pane_resize(&mut pane, 320, 240, SCALE_DENOMINATOR).unwrap();
        assert!(matches!(
            command_receiver.try_recv().unwrap(),
            WindowCommand::Resize { .. }
        ));
    }

    #[test]
    fn uncontrolled_visible_inactive_pane_claims_control_for_every_resize() {
        let splint_id = SplintId::new();
        let (_updates, update_receiver) = tokio::sync::mpsc::channel(1);
        let (commands, mut command_receiver) = tokio::sync::mpsc::channel(2);
        let mut pane = PaneView::from_inactive_options(
            WindowPaneOptions {
                snapshot: valid_snapshot(splint_id),
                terminal_grid_limits: TerminalGridLimits::default(),
                updates: update_receiver,
                commands,
                authority: AuthorityStatus::default(),
                controlled: false,
                image_sources: ImageContentLeaseSet::default(),
            },
            SCALE_DENOMINATOR,
        )
        .unwrap();

        App::emit_inactive_pane_resize(&mut pane, 320, 240, SCALE_DENOMINATOR).unwrap();
        assert!(matches!(
            command_receiver.try_recv().unwrap(),
            WindowCommand::Resize { .. }
        ));

        App::emit_inactive_pane_resize(&mut pane, 640, 480, SCALE_DENOMINATOR).unwrap();
        assert!(matches!(
            command_receiver.try_recv().unwrap(),
            WindowCommand::Resize { .. }
        ));
    }

    fn reduce_history_merge_case(
        cached: &[u64],
        previous_newest: u64,
        previous_available: usize,
        transition: HistoryTransition,
        returned: &[u64],
        available: usize,
        oldest: u64,
    ) -> TerminalSnapshot {
        let mut current = snapshot(SplintId::new(), 1, 10);
        current.columns = 1;
        current.rows = 34;
        current.visible_rows = vec![blank_row(1); 34];
        current.scrollback_rows = cached.iter().map(|id| history_row(*id, 0)).collect();
        current.available_scrollback_rows = previous_available;
        current.oldest_available_scrollback_row_id = cached.first().copied();
        current.newest_available_scrollback_row_id = Some(previous_newest);
        let generation = current.history_generation;
        apply_scrollback_update(
            &mut current,
            splinterm_protocol::TerminalScrollbackUpdate {
                transition,
                history_generation: generation,
                oldest_available_row_id: Some(oldest),
                newest_available_row_id: returned.last().copied(),
                rows: returned.iter().map(|id| history_row(*id, 1)).collect(),
                available_rows: available,
                omitted_oldest_rows: available - returned.len(),
            },
        )
        .unwrap();
        current
    }

    #[test]
    fn older_pages_join_only_an_unchanged_cache_top_or_the_same_revision() {
        let mut current = snapshot(SplintId::new(), 1, 10);
        current.scrollback_rows = (50..=60).map(|id| history_row(id, 0)).collect();
        let page = |terminal_revision| splinterm_protocol::ScrollbackPage {
            splint_id: current.splint_id,
            incarnation: current.incarnation,
            terminal_revision,
            history_generation: current.history_generation,
            oldest_available_row_id: Some(1),
            newest_available_row_id: Some(60),
            rows: (40..=49).map(|id| history_row(id, 0)).collect(),
            has_older: true,
        };
        let later = current.revision + 3;
        assert!(pages_join_history(&[page(later)], &current, Some(50)));
        assert!(pages_join_history(
            &[page(current.revision)],
            &current,
            Some(7)
        ));
        assert!(!pages_join_history(&[page(later)], &current, Some(7)));
        assert!(!pages_join_history(&[page(later)], &current, None));
        let mut other_generation = page(later);
        other_generation.history_generation += 1;
        assert!(!pages_join_history(&[other_generation], &current, Some(50)));
        current.scrollback_rows.clear();
        assert!(!pages_join_history(&[page(later)], &current, None));
    }

    #[test]
    fn history_merge_truncated_append_drops_disconnected_blank_prefix() {
        let returned = (74..=89).collect::<Vec<_>>();
        let current = reduce_history_merge_case(
            &[1, 2],
            2,
            2,
            HistoryTransition::Append {
                appended_rows: 87,
                trimmed_rows: 0,
            },
            &returned,
            89,
            1,
        );
        assert_eq!(
            current
                .scrollback_rows
                .iter()
                .filter_map(|r| r.row_id)
                .collect::<Vec<_>>(),
            returned
        );
        assert_eq!(current.omitted_oldest_scrollback_rows, 73);
        let mut viewport = crate::viewport::ScrollbackViewport::default();
        viewport.scroll_up(33, &current);
        assert_eq!(viewport.offset_from_bottom(), 16);
        assert_eq!(viewport.visible_rows(&current)[0].row_id, Some(74));
        assert!(
            !viewport.visible_rows(&current)[0].cells[0]
                .content
                .is_empty()
        );
    }

    #[test]
    fn history_merge_replace_requires_overlap_not_numeric_adjacency() {
        let current = reduce_history_merge_case(
            &[10, 20],
            20,
            2,
            HistoryTransition::Replace,
            &[21, 30],
            4,
            10,
        );
        assert_eq!(
            current
                .scrollback_rows
                .iter()
                .filter_map(|r| r.row_id)
                .collect::<Vec<_>>(),
            vec![21, 30]
        );
        assert_eq!(current.omitted_oldest_scrollback_rows, 2);
    }

    #[test]
    fn history_merge_retains_overlap_with_nonconsecutive_ids() {
        let current = reduce_history_merge_case(
            &[10, 20, 30, 40],
            40,
            4,
            HistoryTransition::Replace,
            &[30, 40, 70],
            5,
            10,
        );
        assert_eq!(
            current
                .scrollback_rows
                .iter()
                .filter_map(|r| r.row_id)
                .collect::<Vec<_>>(),
            vec![10, 20, 30, 40, 70]
        );
        assert_eq!(current.omitted_oldest_scrollback_rows, 0);
    }

    #[test]
    fn history_merge_complete_append_retains_nonconsecutive_ids_and_trim() {
        for (trimmed_rows, oldest, available, expected) in [
            (0, 10, 6, vec![10, 20, 30, 40, 70, 90]),
            (2, 30, 4, vec![30, 40, 70, 90]),
        ] {
            let current = reduce_history_merge_case(
                &[10, 20, 30, 40],
                40,
                4,
                HistoryTransition::Append {
                    appended_rows: 2,
                    trimmed_rows,
                },
                &[70, 90],
                available,
                oldest,
            );
            assert_eq!(
                current
                    .scrollback_rows
                    .iter()
                    .filter_map(|r| r.row_id)
                    .collect::<Vec<_>>(),
                expected
            );
            assert_eq!(current.omitted_oldest_scrollback_rows, 0);
        }
    }

    #[test]
    fn history_merge_complete_append_does_not_bridge_an_older_only_cache() {
        let current = reduce_history_merge_case(
            &[10, 20],
            50,
            5,
            HistoryTransition::Append {
                appended_rows: 1,
                trimmed_rows: 0,
            },
            &[70],
            6,
            10,
        );
        assert_eq!(
            current
                .scrollback_rows
                .iter()
                .filter_map(|r| r.row_id)
                .collect::<Vec<_>>(),
            vec![70]
        );
        assert_eq!(current.omitted_oldest_scrollback_rows, 5);
    }

    #[test]
    fn same_generation_trim_discards_cached_rows_before_daemon_oldest() {
        let mut current = snapshot(SplintId::new(), 1, 10);
        current.columns = 1;
        current.rows = 1;
        current.visible_rows = vec![blank_row(1)];
        current.scrollback_rows = (1..=4).map(|id| history_row(id, 0)).collect();
        current.available_scrollback_rows = 4;
        current.oldest_available_scrollback_row_id = Some(1);
        current.newest_available_scrollback_row_id = Some(4);

        apply_scrollback_update(
            &mut current,
            splinterm_protocol::TerminalScrollbackUpdate {
                transition: HistoryTransition::Append {
                    appended_rows: 2,
                    trimmed_rows: 2,
                },
                history_generation: 1,
                oldest_available_row_id: Some(3),
                newest_available_row_id: Some(6),
                rows: vec![history_row(5, 0), history_row(6, 0)],
                available_rows: 4,
                omitted_oldest_rows: 2,
            },
        )
        .unwrap();

        assert_eq!(
            current
                .scrollback_rows
                .iter()
                .filter_map(|row| row.row_id)
                .collect::<Vec<_>>(),
            vec![3, 4, 5, 6]
        );
        assert_eq!(current.omitted_oldest_scrollback_rows, 0);
    }

    #[test]
    fn bounded_history_replace_applies_visible_marker_without_resync() {
        let mut current = snapshot(SplintId::new(), 7, 10);
        current.columns = 1;
        current.rows = 1;
        current.visible_rows = vec![blank_row(1)];
        current.scrollback_rows = (1..=4).map(|id| history_row(id, 0)).collect();
        current.available_scrollback_rows = 4;
        current.oldest_available_scrollback_row_id = Some(1);
        current.newest_available_scrollback_row_id = Some(4);
        let marker = TerminalRow {
            row_id: Some(21),
            linebreak: true,
            cells: vec![TerminalCell {
                content: "SPLINTERBENCH_DONE".into(),
                spacer_remaining: None,
                attributes: current.visible_rows[0].cells[0].attributes,
            }],
        };

        apply_terminal_update(
            &mut current,
            TerminalUpdate {
                base_revision: 10,
                revision: 11,
                rows: vec![splinterm_protocol::TerminalRowPatch {
                    index: 0,
                    row: marker.clone(),
                }],
                scrolls: Vec::new(),
                cursor: None,
                title: None,
                input_modes: None,
                active_screen: None,
                palette: None,
                default_colors: None,
                columns: None,
                row_count: None,
                scrollback: Some(splinterm_protocol::TerminalScrollbackUpdate {
                    transition: HistoryTransition::Replace,
                    history_generation: 1,
                    oldest_available_row_id: Some(1),
                    newest_available_row_id: Some(20),
                    rows: vec![history_row(19, 0), history_row(20, 0)],
                    available_rows: 20,
                    omitted_oldest_rows: 18,
                }),
                images: None,
            },
        )
        .expect("bounded replace update");

        assert_eq!(current.revision, 11);
        assert_eq!(current.visible_rows[0], marker);
        assert_eq!(current.newest_available_scrollback_row_id, Some(20));
    }

    #[test]
    fn semantic_update_applies_exact_row_cursor_and_title_revision() {
        let mut current = snapshot(SplintId::new(), 7, 10);
        current.columns = 2;
        current.rows = 1;
        current.visible_rows = vec![blank_row(2)];
        let row = TerminalRow {
            row_id: Some(8),
            linebreak: true,
            cells: vec![TerminalCell {
                content: "x".into(),
                spacer_remaining: None,
                attributes: current.visible_rows[0].cells[0].attributes,
            }],
        };
        apply_terminal_update(
            &mut current,
            TerminalUpdate {
                base_revision: 10,
                revision: 11,
                rows: vec![splinterm_protocol::TerminalRowPatch {
                    index: 0,
                    row: row.clone(),
                }],
                scrolls: Vec::new(),
                cursor: Some(splinterm_protocol::TerminalCursor {
                    column: 1,
                    row: 0,
                    deferred_wrap: true,
                }),
                title: Some("revision eleven".into()),
                input_modes: None,
                active_screen: None,
                palette: None,
                default_colors: None,
                columns: None,
                row_count: None,
                scrollback: Some(splinterm_protocol::TerminalScrollbackUpdate {
                    transition: splinterm_protocol::HistoryTransition::Reflow,
                    history_generation: 2,
                    oldest_available_row_id: Some(7),
                    newest_available_row_id: Some(7),
                    rows: vec![TerminalRow {
                        row_id: Some(7),
                        linebreak: true,
                        cells: Vec::new(),
                    }],
                    available_rows: 1,
                    omitted_oldest_rows: 0,
                }),
                images: Some(Box::new(splinterm_protocol::TerminalImagePlane {
                    screen: splinterm_protocol::ActiveScreen::Normal,
                    contents: Vec::new(),
                    placements: Vec::new(),
                })),
            },
        )
        .expect("contiguous semantic update");
        assert_eq!(current.revision, 11);
        assert_eq!(current.visible_rows[0], row);
        assert_eq!((current.cursor_column, current.cursor_row), (1, 0));
        assert!(current.cursor_deferred_wrap);
        assert_eq!(current.title, "revision eleven");
        assert_eq!(current.history_generation, 2);
        assert_eq!(current.oldest_available_scrollback_row_id, Some(7));
        assert_eq!(current.newest_available_scrollback_row_id, Some(7));
        assert_eq!(current.available_scrollback_rows, 1);
        assert_eq!(current.scrollback_rows[0].row_id, Some(7));
        assert!(current.images.is_some());
    }

    #[test]
    fn semantic_scroll_survives_return_to_live_frame_rebuild() {
        let splint_id = SplintId::new();
        let mut initial = valid_snapshot(splint_id);
        initial.rows = 4;
        initial.scrollback_rows = vec![history_row(1, 0)];
        initial.available_scrollback_rows = 1;
        initial.oldest_available_scrollback_row_id = Some(1);
        initial.newest_available_scrollback_row_id = Some(1);
        initial.visible_rows = (2..=5)
            .zip(["a", "b", "c", "d"])
            .map(|(row_id, content)| {
                let mut row = history_row(row_id, 0);
                row.cells[0].content = content.into();
                row
            })
            .collect();
        let (_updates, update_receiver) = tokio::sync::mpsc::channel(1);
        let (commands, _command_receiver) = tokio::sync::mpsc::channel(1);
        let mut pane = PaneView::from_options(
            WindowPaneOptions {
                snapshot: initial,
                terminal_grid_limits: TerminalGridLimits::default(),
                updates: update_receiver,
                commands,
                authority: AuthorityStatus::default(),
                controlled: false,
                image_sources: ImageContentLeaseSet::default(),
            },
            SCALE_DENOMINATOR,
        )
        .unwrap();
        let mut exposed = history_row(6, 0);
        exposed.cells[0].content = "e".into();

        apply_terminal_update(
            pane.snapshot.as_mut().unwrap(),
            TerminalUpdate {
                base_revision: 1,
                revision: 2,
                rows: vec![splinterm_protocol::TerminalRowPatch {
                    index: 3,
                    row: exposed,
                }],
                scrolls: vec![splinterm_protocol::TerminalScroll {
                    direction: splinterm_protocol::ScrollDirection::Forward,
                    start_row: 0,
                    end_row: 4,
                    rows: 1,
                }],
                cursor: None,
                title: None,
                input_modes: None,
                active_screen: None,
                palette: None,
                default_colors: None,
                columns: None,
                row_count: None,
                scrollback: None,
                images: None,
            },
        )
        .unwrap();
        pane.scrollback_viewport
            .scroll_up(1, pane.snapshot.as_ref().unwrap());
        pane.viewport_dirty = true;
        assert!(rebuild_dirty_pane_viewport_frame(&mut pane, SCALE_DENOMINATOR).unwrap());

        pane.scrollback_viewport.return_to_live();
        pane.viewport_dirty = true;
        assert!(rebuild_dirty_pane_viewport_frame(&mut pane, SCALE_DENOMINATOR).unwrap());

        assert!(pane.scrollback_viewport.is_live());
        assert_eq!(pane.rendered_viewport_offset, 0);
        let live = pane.display_snapshot().unwrap();
        assert_eq!(
            live.visible_rows
                .iter()
                .map(|row| row.cells[0].content.as_str())
                .collect::<Vec<_>>(),
            ["b", "c", "d", "e"]
        );
    }

    #[test]
    fn snapshot_order_accepts_only_newer_matching_identity() {
        let splint_id = SplintId::new();
        let current = snapshot(splint_id, 7, 10);
        assert!(snapshot_is_newer(&current, &snapshot(splint_id, 7, 11)).expect("matching"));
        let equal = snapshot(splint_id, 7, 10);
        assert!(!snapshot_is_newer(&current, &equal).expect("duplicate"));
        assert!(!snapshot_replaces(&current, &equal, false).expect("ordinary duplicate"));
        assert!(snapshot_replaces(&current, &equal, true).expect("authoritative duplicate"));
        assert!(!snapshot_is_newer(&current, &snapshot(splint_id, 7, 9)).expect("stale"));
        assert!(snapshot_is_newer(&current, &snapshot(SplintId::new(), 7, 11)).is_err());
        assert!(snapshot_is_newer(&current, &snapshot(splint_id, 8, 11)).is_err());
    }

    #[test]
    fn pending_snapshots_coalesce_to_newest_revision() {
        let splint_id = SplintId::new();
        let current = snapshot(splint_id, 2, 10);
        let latest = coalesce_snapshots(
            Some(&current),
            [
                snapshot(splint_id, 2, 11),
                snapshot(splint_id, 2, 13),
                snapshot(splint_id, 2, 12),
            ],
        )
        .expect("matching snapshots")
        .expect("newer snapshot");
        assert_eq!(latest.revision, 13);
        assert!(coalesce_snapshots(Some(&current), [snapshot(SplintId::new(), 2, 14)]).is_err());
    }

    fn normal_modes() -> TerminalInputModes {
        TerminalInputModes {
            application_cursor: false,
            application_keypad: false,
            focus_reporting: false,
            bracketed_paste: false,
            cursor_visible: true,
            cursor_blink: true,
            mouse_tracking: MouseTracking::None,
            sgr_mouse: false,
        }
    }

    fn empty_update() -> TerminalUpdate {
        TerminalUpdate {
            base_revision: 1,
            revision: 2,
            rows: Vec::new(),
            scrolls: Vec::new(),
            cursor: None,
            title: None,
            input_modes: None,
            active_screen: None,
            palette: None,
            default_colors: None,
            columns: None,
            row_count: None,
            scrollback: None,
            images: None,
        }
    }

    #[test]
    fn viewport_cursor_moves_with_content_until_it_leaves_the_grid() {
        assert_eq!(viewport_cursor_row(2, 0, 6), Some(2));
        assert_eq!(viewport_cursor_row(2, 3, 6), Some(5));
        assert_eq!(viewport_cursor_row(2, 4, 6), None);
        assert_eq!(viewport_cursor_row(-1, 3, 6), None);
    }

    #[test]
    fn local_scrollback_uses_foot_default_wheel_multiplier() {
        let mut wheel = WheelAccumulator::default();
        assert_eq!(
            wheel.push_scaled(0.0, 0, -40, SCROLLBACK_WHEEL_MULTIPLIER, 29),
            Some((MouseAction::WheelUp, 1))
        );
        assert_eq!(
            wheel.push_scaled(0.0, 1, 0, SCROLLBACK_WHEEL_MULTIPLIER, 29),
            Some((MouseAction::WheelDown, 3))
        );
    }

    #[test]
    fn window_title_preserves_app_identity_and_shell_title() {
        let authority = AuthorityStatus::default();
        for title in [
            None,
            Some(""),
            Some(" \t\n"),
            Some("Splinterm"),
            Some(" Splinterm "),
        ] {
            assert_eq!(
                window_title(title, false, true, &authority, false, None),
                "Splinterm"
            );
        }
        for (title, expected) in [
            (
                "tester@nixos-plasma: ~",
                "Splinterm — tester@nixos-plasma: ~",
            ),
            ("  project — editor  ", "Splinterm — project — editor"),
            ("作業", "Splinterm — 作業"),
        ] {
            assert_eq!(
                window_title(Some(title), false, true, &authority, false, None),
                expected
            );
        }
    }

    #[test]
    fn window_title_preserves_app_identity_with_authority_status() {
        for (authority, label) in [
            (AuthorityStatus::default(), ""),
            (
                AuthorityStatus {
                    grants: vec![(1, "agent".into())],
                    development_bypass: false,
                },
                " — EXTERNAL ACCESS ACTIVE",
            ),
            (
                AuthorityStatus {
                    grants: vec![(1, "agent".into())],
                    development_bypass: true,
                },
                " — DEVELOPMENT BYPASS",
            ),
        ] {
            for controller_active in [false, true] {
                let controller = if controller_active {
                    " — local controller"
                } else {
                    ""
                };
                assert_eq!(
                    window_title(
                        Some("shell"),
                        controller_active,
                        true,
                        &authority,
                        false,
                        None
                    ),
                    format!("Splinterm — shell{controller}{label}")
                );
                // A remote Window never calls itself local.
                let remote = window_title(
                    Some("shell"),
                    controller_active,
                    false,
                    &authority,
                    false,
                    None,
                );
                assert!(!remote.contains("local"), "{remote}");
                let remote_controller = if controller_active {
                    " — controller"
                } else {
                    ""
                };
                assert_eq!(
                    remote,
                    format!("Splinterm — shell{remote_controller}{label}")
                );
            }
        }
    }

    /// `[explorer] full-height`: a docked Explorer laid out over the whole
    /// Window keeps that height, the terminal beside it starts below the tab
    /// strip, and the strip begins at the panel's right edge. A drawer keeps
    /// the layout below the strip.
    #[test]
    fn full_height_explorer_docks_beside_the_tab_strip() {
        let window = Rect {
            x: 0,
            y: 0,
            width: 800,
            height: 320,
        };
        let strip = 26;
        let docked = lair_explorer_layout(window, 160, &[], None, 0).unwrap();
        assert_eq!(docked.mode, LairExplorerPresentationMode::Docked);
        let beside = explorer_beside_tabs(docked.clone(), strip).unwrap();
        assert_eq!(beside.panel, docked.panel);
        assert_eq!((beside.panel.y, beside.panel.height), (0, 320));
        assert_eq!(beside.header, docked.header);
        assert_eq!(beside.rows, docked.rows);
        assert_eq!(
            beside.terminal,
            Rect {
                x: docked.panel.width,
                y: strip,
                width: 800 - docked.panel.width,
                height: 320 - strip,
            }
        );
        assert_eq!(tab_strip_left_of(Some(&beside)), docked.panel.width);

        // No tab strip: the terminal keeps the full height.
        assert_eq!(
            explorer_beside_tabs(docked.clone(), 0).unwrap().terminal,
            docked.terminal
        );
        // A strip taller than the Window leaves no terminal, never wraps.
        assert_eq!(
            explorer_beside_tabs(docked, 1_000).unwrap().terminal.height,
            0
        );

        let narrow = Rect {
            width: 400,
            ..window
        };
        let drawer = lair_explorer_layout(narrow, 160, &[], None, 0).unwrap();
        assert_eq!(drawer.mode, LairExplorerPresentationMode::Drawer);
        assert!(explorer_beside_tabs(drawer.clone(), strip).is_none());
        assert_eq!(tab_strip_left_of(Some(&drawer)), 0);
        assert_eq!(tab_strip_left_of(None), 0);
    }

    /// Beside the tab strip, the Explorer's header line is exactly the
    /// strip's height with no gap below the strip's top, its filters span
    /// that line, and its text is capped as the tab labels' is, so header
    /// and labels share a baseline at every strip height and scale.
    #[test]
    fn full_height_explorer_header_matches_the_tab_strip() {
        let window = Rect {
            x: 0,
            y: 0,
            width: 900,
            height: 400,
        };
        for strip in [20, 26, 34, 48] {
            for scale in [120, 150, 180, 240] {
                let case = format!("strip={strip} scale={scale}");
                let layout = full_height_explorer_layout(
                    window,
                    strip,
                    scale,
                    (160, LairExplorerMetrics::UNMEASURED),
                    &[],
                    None,
                    0,
                    false,
                )
                .unwrap();
                assert_eq!(
                    (layout.header.y, layout.header.height),
                    (0, strip),
                    "{case}"
                );
                assert_eq!(
                    layout.header_text_cell_cap,
                    tab_text_maximum_cell_height(strip, scale).unwrap(),
                    "{case}"
                );
                assert_eq!(layout.filters.len(), 3, "{case}");
                for (_, rect) in &layout.filters {
                    assert_eq!((rect.y, rect.height), (0, strip), "{case}");
                }
                assert_eq!(layout.terminal.y, strip, "{case}");
                assert_eq!(layout.panel.height, window.height, "{case}");
            }
        }
        // With the strip hidden the header keeps the row-high line and the
        // chrome text cap.
        let hidden = full_height_explorer_layout(
            window,
            0,
            120,
            (160, LairExplorerMetrics::UNMEASURED),
            &[],
            None,
            0,
            false,
        )
        .unwrap();
        let default = lair_explorer_layout(window, 160, &[], None, 0).unwrap();
        assert_eq!(hidden.header, default.header);
        assert_eq!(hidden.header_text_cell_cap, u32::MAX);
        // A drawer is never laid out beside the strip.
        let narrow = Rect {
            width: 400,
            ..window
        };
        assert!(
            full_height_explorer_layout(
                narrow,
                26,
                120,
                (160, LairExplorerMetrics::UNMEASURED),
                &[],
                None,
                0,
                false
            )
            .is_none()
        );
    }

    /// Collapsing the full-height Explorer to its rail widens the terminal
    /// beside it by the panels' difference and moves the tab strip to the
    /// rail's edge; the rail's `»` keeps the strip-high header. A Window too
    /// narrow to dock the expanded panel still docks the rail, and one too
    /// narrow for even the rail shows the rail as a drawer over the terminal.
    #[test]
    fn collapsing_the_explorer_widens_the_terminal_and_moves_the_tab_strip() {
        let window = Rect {
            x: 0,
            y: 0,
            width: 900,
            height: 400,
        };
        let strip = 26;
        let shown = |content, collapsed| {
            full_height_explorer_layout(
                content,
                strip,
                120,
                (160, LairExplorerMetrics::UNMEASURED),
                &[],
                None,
                0,
                collapsed,
            )
        };
        let expanded = shown(window, false).unwrap();
        let rail = shown(window, true).unwrap();
        assert!(!expanded.rail && rail.rail);
        assert_eq!(rail.mode, LairExplorerPresentationMode::Docked);
        assert!(rail.panel.width * 4 < expanded.panel.width);
        assert_eq!(
            rail.terminal.width - expanded.terminal.width,
            expanded.panel.width - rail.panel.width
        );
        assert_eq!(rail.terminal.x, rail.panel.width);
        assert_eq!(
            (rail.terminal.y, rail.terminal.height),
            (expanded.terminal.y, expanded.terminal.height)
        );
        assert_eq!(rail.panel.height, window.height);
        assert_eq!(tab_strip_left_of(Some(&rail)), rail.panel.width);
        assert_eq!(tab_strip_left_of(Some(&expanded)), expanded.panel.width);
        assert_eq!((rail.header.y, rail.header.height), (0, strip));
        assert_eq!(rail.toggle(), Some(rail.header));
        assert_eq!(
            rail.header_text_cell_cap,
            tab_text_maximum_cell_height(strip, 120).unwrap()
        );
        // Below a full-width strip the rail docks the same way.
        let below = Rect {
            y: strip,
            height: window.height - strip,
            ..window
        };
        let below_rail = lair_explorer_shown_layout(
            below,
            None,
            160,
            LairExplorerMetrics::UNMEASURED,
            &[],
            None,
            0,
            true,
        )
        .unwrap();
        assert!(below_rail.rail);
        assert_eq!(below_rail.terminal.width, rail.terminal.width);
        // Too narrow to dock the expanded panel: the rail still docks.
        let narrow = Rect {
            width: 400,
            ..window
        };
        assert!(shown(narrow, false).is_none());
        let narrow_rail = shown(narrow, true).unwrap();
        assert!(narrow_rail.rail);
        assert_eq!(narrow_rail.terminal.width, 400 - narrow_rail.panel.width);
        // Too narrow for even the rail: the rail is a drawer over the
        // terminal, which keeps its whole width and the strip its place. In
        // full-height mode that drawer falls back below the strip.
        let tiny = Rect {
            width: 190,
            ..below
        };
        let drawer = lair_explorer_shown_layout(
            tiny,
            None,
            160,
            LairExplorerMetrics::UNMEASURED,
            &[],
            None,
            0,
            true,
        )
        .unwrap();
        assert!(drawer.rail);
        assert_eq!(drawer.mode, LairExplorerPresentationMode::Drawer);
        assert_eq!(drawer.terminal, tiny);
        assert_eq!(drawer.toggle(), Some(drawer.header));
        assert_eq!(tab_strip_left_of(Some(&drawer)), 0);
        assert!(
            shown(
                Rect {
                    width: 190,
                    ..window
                },
                true
            )
            .is_none()
        );
    }

    /// A rail widened for larger chrome text takes its width from the
    /// terminal, and the tab strip beside it moves to its edge.
    #[test]
    fn a_wider_rail_narrows_the_terminal_and_moves_the_tab_strip() {
        let window = Rect {
            x: 0,
            y: 0,
            width: 900,
            height: 400,
        };
        let metrics = LairExplorerMetrics::measured(14, 120);
        let wide = metrics.rail_width;
        assert!(wide > LairExplorerMetrics::UNMEASURED.rail_width);
        let wide_rail =
            full_height_explorer_layout(window, 26, 120, (160, metrics), &[], None, 0, true)
                .unwrap();
        assert_eq!(wide_rail.panel.width, wide);
        assert_eq!(wide_rail.terminal.width, 900 - wide);
        assert_eq!(tab_strip_left_of(Some(&wide_rail)), wide);
    }

    /// Over an unfocused rail, which shows no selection, the wheel scrolls
    /// the rows, never before the first or past the last screenful.
    #[test]
    fn unfocused_rail_wheel_scrolls_its_rows() {
        use super::explorer_rail_scrolled_start;

        assert_eq!(explorer_rail_scrolled_start(0, 5, 12, 1), 1);
        assert_eq!(explorer_rail_scrolled_start(0, 5, 12, -1), 0);
        assert_eq!(explorer_rail_scrolled_start(7, 5, 12, 1), 7);
        assert_eq!(explorer_rail_scrolled_start(9, 5, 12, -1), 7);
        assert_eq!(explorer_rail_scrolled_start(0, 20, 12, 1), 0);
    }

    #[test]
    fn search_paint_rect_excludes_explorer_drawer() {
        let content = Rect {
            x: 0,
            y: 0,
            width: 400,
            height: 320,
        };
        let drawer = lair_explorer_layout(content, 160, &[], None, 0).unwrap();
        assert_eq!(drawer.mode, LairExplorerPresentationMode::Drawer);
        let terminal = search_paint_rect(drawer.terminal, Some(&drawer)).unwrap();
        assert_eq!(terminal.x, drawer.panel.width);
        assert_eq!(terminal.width, content.width - drawer.panel.width);
        let too_narrow = Rect {
            width: 200,
            ..content
        };
        let drawer = lair_explorer_layout(too_narrow, 160, &[], None, 0).unwrap();
        assert!(search_paint_rect(drawer.terminal, Some(&drawer)).is_none());
        let wide = Rect {
            width: 800,
            ..content
        };
        let docked = lair_explorer_layout(wide, 160, &[], None, 0).unwrap();
        assert_eq!(docked.mode, LairExplorerPresentationMode::Docked);
        assert_eq!(
            search_paint_rect(docked.terminal, Some(&docked)),
            Some(docked.terminal)
        );
        assert_eq!(search_paint_rect(wide, None), Some(wide));
    }

    #[test]
    fn trusted_title_surfaces_control_decision_and_bounded_search_state() {
        let authority = AuthorityStatus::default();
        let mut search = SearchUiState {
            input: Some(BoundedTextEditor::new(
                "needle\nspoof".into(),
                64,
                64,
                false,
            )),
            ..SearchUiState::default()
        };
        search.matches.push(SearchMatch {
            row_id: 1,
            start_column: 0,
            end_column: 2,
            preview: "needle".into(),
        });
        let title = window_title(Some("shell"), true, true, &authority, true, Some(&search));
        assert!(title.starts_with("Splinterm — shell — local controller"));
        assert!(title.contains("CONTROL REQUEST"));
        assert!(title.contains("SEARCH: needlespoof [1 match(es)"));
        assert!(!title.contains('\n'));
    }

    #[test]
    fn history_overlay_status_is_detached_and_display_bounded() {
        let mut state = snapshot(SplintId::new(), 1, 1);
        state.rows = 1;
        state.scrollback_rows.push(TerminalRow {
            row_id: Some(1),
            linebreak: false,
            cells: Vec::new(),
        });
        state.available_scrollback_rows = 5_000;
        let mut viewport = ScrollbackViewport::default();
        assert_eq!(history_overlay_status(&viewport, Some(&state)), None);
        viewport.scroll_up(1, &state);
        assert_eq!(
            history_overlay_status(&viewport, Some(&state)),
            Some(HistoryOverlayStatus {
                offset_from_bottom: 1,
                available_rows: 999,
                unseen_rows: 0,
            })
        );
    }

    #[test]
    fn selection_text_orders_endpoints_and_skips_wide_spacers() {
        let mut view = snapshot(SplintId::new(), 1, 1);
        view.columns = 4;
        view.rows = 2;
        view.visible_rows = vec![blank_row(4), blank_row(4)];
        view.visible_rows[0].row_id = Some(1);
        view.visible_rows[1].row_id = Some(2);
        view.visible_rows[0].cells[0].content = "A".to_owned();
        view.visible_rows[0].cells[1].content = "界".to_owned();
        view.visible_rows[0].cells[2].spacer_remaining = Some(0);
        view.visible_rows[0].cells[3].content = " ".to_owned();
        view.visible_rows[1].cells[0].content = "B".to_owned();
        view.visible_rows[1].cells[1].content = "C".to_owned();
        let selection = Selection {
            anchor: SelectionEndpoint {
                active_screen: ActiveScreen::Normal,
                history_generation: 1,
                row_id: 2,
                column: 1,
            },
            end: SelectionEndpoint {
                active_screen: ActiveScreen::Normal,
                history_generation: 1,
                row_id: 1,
                column: 0,
            },
        };
        assert_eq!(
            selection_text(&view, &view.visible_rows, selection).as_deref(),
            Some("A界\nBC")
        );
        let stale = Selection {
            anchor: SelectionEndpoint {
                history_generation: 2,
                ..selection.anchor
            },
            ..selection
        };
        assert_eq!(selection_text(&view, &view.visible_rows, stale), None);
    }

    #[test]
    fn selection_identity_survives_live_to_history_and_rejects_reset_or_trim() {
        let mut state = snapshot(SplintId::new(), 1, 1);
        state.columns = 1;
        state.rows = 2;
        state.visible_rows = vec![blank_row(1), blank_row(1)];
        state.visible_rows[0].row_id = Some(10);
        state.visible_rows[1].row_id = Some(11);
        let selection = Selection {
            anchor: SelectionEndpoint {
                active_screen: ActiveScreen::Normal,
                history_generation: 1,
                row_id: 10,
                column: 0,
            },
            end: SelectionEndpoint {
                active_screen: ActiveScreen::Normal,
                history_generation: 1,
                row_id: 11,
                column: 0,
            },
        };
        assert!(selection_is_retained(
            &state,
            &state.visible_rows,
            selection
        ));

        let moved = state.visible_rows.remove(0);
        state.scrollback_rows.push(moved);
        state.rows = 1;
        assert!(selection_is_retained(
            &state,
            &state.visible_rows,
            selection
        ));
        state.history_generation = 2;
        assert!(!selection_is_retained(
            &state,
            &state.visible_rows,
            selection
        ));
        state.history_generation = 1;
        state.scrollback_rows.clear();
        assert!(!selection_is_retained(
            &state,
            &state.visible_rows,
            selection
        ));
        state.active_screen = ActiveScreen::Alternate;
        assert!(!selection_is_retained(
            &state,
            &state.visible_rows,
            selection
        ));
    }

    #[test]
    fn a_selection_on_a_held_screen_copies_its_rows() {
        let row = |row_id, text: &str| {
            let mut row = blank_row(2);
            row.row_id = Some(row_id);
            for (cell, character) in row.cells.iter_mut().zip(text.chars()) {
                cell.content = character.to_string();
            }
            row
        };
        let mut state = snapshot(SplintId::new(), 1, 1);
        state.columns = 2;
        state.rows = 2;
        state.scrollback_rows = vec![row(1, "h1")];
        // The live screen moved on; the held one still shows rows 5 and 6.
        state.visible_rows = vec![row(20, "n1"), row(21, "n2")];
        let held = vec![row(5, "s1"), row(6, "s2")];
        let endpoint = |row_id, column| SelectionEndpoint {
            active_screen: ActiveScreen::Normal,
            history_generation: 1,
            row_id,
            column,
        };
        let selection = Selection {
            anchor: endpoint(1, 0),
            end: endpoint(6, 1),
        };
        assert_eq!(
            selection_text(&state, &held, selection).as_deref(),
            Some("h1\ns1\ns2")
        );
        assert!(selection_is_retained(&state, &held, selection));
        assert_eq!(selection_text(&state, &state.visible_rows, selection), None);
    }

    #[test]
    fn selection_copy_spans_three_loaded_pages_by_row_identity() {
        let mut state = snapshot(SplintId::new(), 1, 1);
        state.columns = 1;
        state.rows = 8;
        let rows = (1..=48)
            .map(|row_id| {
                let mut row = blank_row(1);
                row.row_id = Some(row_id);
                row.cells[0].content = "x".to_owned();
                row
            })
            .collect::<Vec<_>>();
        state.scrollback_rows = rows[..40].to_vec();
        state.visible_rows = rows[40..].to_vec();
        let selection = Selection {
            anchor: SelectionEndpoint {
                active_screen: ActiveScreen::Normal,
                history_generation: 1,
                row_id: 1,
                column: 0,
            },
            end: SelectionEndpoint {
                active_screen: ActiveScreen::Normal,
                history_generation: 1,
                row_id: 48,
                column: 0,
            },
        };
        let copied = selection_text(&state, &state.visible_rows, selection)
            .expect("loaded endpoints resolve");
        assert_eq!(copied.lines().count(), 48);

        let mut display = state.clone();
        display.scrollback_rows.clear();
        display.visible_rows = rows[19..27].to_vec();
        assert_eq!(
            selection_display_bounds(&state, &state.visible_rows, &display, selection),
            Some((
                CellPosition { row: 0, column: 0 },
                CellPosition { row: 7, column: 0 },
            ))
        );
    }

    #[test]
    fn page_bounding_rejects_eviction_of_selected_endpoint() {
        let rows = (1..=u64::try_from(MAX_CACHED_HISTORY_ROWS + 1).unwrap())
            .map(|row_id| {
                let mut row = blank_row(1);
                row.row_id = Some(row_id);
                row
            })
            .collect::<Vec<_>>();
        let oldest = bound_history_page_with_pins(rows.clone(), Some([1, 2]), &[])
            .expect("older paging retains selected oldest endpoints");
        assert_eq!(oldest.len(), MAX_CACHED_HISTORY_ROWS);
        assert_eq!(oldest.first().and_then(|row| row.row_id), Some(1));
        let newest = u64::try_from(MAX_CACHED_HISTORY_ROWS + 1).unwrap();
        assert!(
            bound_history_page_with_pins(rows, Some([newest - 1, newest]), &[]).is_none(),
            "older paging rejects a batch rather than evict selected newest endpoints"
        );
    }

    #[test]
    fn url_detection_is_visible_http_only_and_trims_punctuation() {
        let mut view = snapshot(SplintId::new(), 1, 1);
        let text = "see https://example.com/path). now";
        view.columns = text.chars().count();
        view.rows = 1;
        let mut row = blank_row(view.columns);
        for (cell, character) in row.cells.iter_mut().zip(text.chars()) {
            cell.content = character.to_string();
        }
        view.visible_rows = vec![row];
        let column = text.find("example").expect("URL column");
        let (_, _, url) = url_at(&view, CellPosition { row: 0, column }).expect("visible URL");
        assert_eq!(url, "https://example.com/path");
        assert!(url_at(&view, CellPosition { row: 0, column: 0 }).is_none());
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one regression compares every local overlay invalidation class"
    )]
    fn visible_content_updates_invalidate_local_overlays_but_metadata_does_not() {
        let mut cursor_only = empty_update();
        cursor_only.cursor = Some(splinterm_protocol::TerminalCursor {
            column: 1,
            row: 1,
            deferred_wrap: false,
        });
        assert!(!terminal_update_changes_visible_content(&cursor_only));

        let mut row = empty_update();
        row.rows.push(splinterm_protocol::TerminalRowPatch {
            index: 0,
            row: blank_row(1),
        });
        assert!(terminal_update_changes_visible_content(&row));
        let current_rows = valid_snapshot(SplintId::new());
        let mut repeated = empty_update();
        repeated.rows.push(splinterm_protocol::TerminalRowPatch {
            index: 0,
            row: current_rows.visible_rows[0].clone(),
        });
        assert!(changed_terminal_patch_rows(&repeated, &current_rows).is_empty());
        repeated.rows[0].row.row_id = Some(999);
        assert!(changed_terminal_patch_rows(&repeated, &current_rows).is_empty());
        repeated.rows[0].row.cells[0].content = "changed".into();
        assert_eq!(changed_terminal_patch_rows(&repeated, &current_rows), [0]);

        let mut scroll = empty_update();
        scroll.scrolls.push(splinterm_protocol::TerminalScroll {
            direction: splinterm_protocol::ScrollDirection::Forward,
            start_row: 0,
            end_row: 2,
            rows: 1,
        });
        assert!(terminal_update_changes_visible_content(&scroll));
        let mut current = snapshot(SplintId::new(), 1, 1);
        current.columns = 1;
        current.rows = 2;
        current.visible_rows.resize_with(2, || blank_row(1));
        current.visible_rows[0].cells[0].content = "top".into();
        current.visible_rows[1].cells[0].content = "bottom".into();
        let mut projected_scroll = scroll.clone();
        projected_scroll.rows = vec![
            splinterm_protocol::TerminalRowPatch {
                index: 0,
                row: current.visible_rows[1].clone(),
            },
            splinterm_protocol::TerminalRowPatch {
                index: 1,
                row: blank_row(1),
            },
        ];
        assert!(changed_terminal_patch_rows(&projected_scroll, &current).is_empty());
        projected_scroll.rows[0].row.cells[0].content = "changed after scroll".into();
        assert_eq!(
            changed_terminal_patch_rows(&projected_scroll, &current),
            [0]
        );
        projected_scroll.scrolls[0].rows = 0;
        assert_eq!(
            changed_terminal_patch_rows(&projected_scroll, &current),
            [0, 1]
        );
        projected_scroll.scrolls[0].rows = 1;
        projected_scroll.scrolls[0].end_row = projected_scroll.scrolls[0].start_row;
        assert_eq!(
            changed_terminal_patch_rows(&projected_scroll, &current),
            [0, 1]
        );
        let mut repeated_metadata = empty_update();
        repeated_metadata.columns = Some(current.columns);
        repeated_metadata.row_count = Some(current.rows);
        let mut repeated_palette = current.palette.clone();
        repeated_palette[0] ^= 0x00ff_ffff;
        repeated_metadata.palette = Some(repeated_palette);
        repeated_metadata.default_colors = Some([1, 2, 3]);
        repeated_metadata.active_screen = Some(current.active_screen);
        assert!(!terminal_update_requires_full_frame(
            &repeated_metadata,
            &current
        ));
        repeated_metadata.palette.as_mut().unwrap()[16] ^= 0x00ff_ffff;
        assert!(terminal_update_requires_full_frame(
            &repeated_metadata,
            &current
        ));
        repeated_metadata.palette = Some(current.palette.clone());
        assert!(!terminal_update_requires_full_frame(&scroll, &current));
        current.images = Some(Box::new(splinterm_protocol::TerminalImagePlane {
            screen: ActiveScreen::Normal,
            contents: Vec::new(),
            placements: Vec::new(),
        }));
        assert!(!terminal_update_requires_full_frame(&scroll, &current));
        current
            .images
            .as_mut()
            .unwrap()
            .placements
            .push(splinterm_protocol::ImagePlacement {
                placement_id: 1,
                content_id: 1,
                row_id: 1,
                column: 0,
                source: splinterm_protocol::ImagePixelRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                destination_columns: 1,
                destination_rows: 1,
                source_cell_size: None,
                x_offset: 0,
                y_offset: 0,
                z_index: 0,
                application_image_id: None,
                application_placement_id: None,
                creation_order: 1,
                erase_policy: splinterm_protocol::ImageErasePolicy::TextOverwrite,
            });
        assert!(terminal_update_requires_full_frame(&scroll, &current));
        let mut identity_update = empty_update();
        let mut identity_row = current.visible_rows[0].clone();
        identity_row.row_id = Some(999);
        identity_update
            .rows
            .push(splinterm_protocol::TerminalRowPatch {
                index: 0,
                row: identity_row,
            });
        assert_eq!(changed_terminal_patch_rows(&identity_update, &current), [0]);
        current.images = None;
        let mut resize_with_scroll = scroll.clone();
        resize_with_scroll.columns = Some(80);
        resize_with_scroll.row_count = Some(24);
        assert!(terminal_update_requires_full_frame(
            &resize_with_scroll,
            &current
        ));

        let mut screen = empty_update();
        screen.active_screen = Some(ActiveScreen::Normal);
        assert!(!terminal_update_requires_full_frame(&screen, &current));
        screen.active_screen = Some(ActiveScreen::Alternate);
        assert!(terminal_update_requires_full_frame(&screen, &current));

        let mut image_update = empty_update();
        image_update.images = Some(Box::new(splinterm_protocol::TerminalImagePlane {
            screen: ActiveScreen::Normal,
            contents: Vec::new(),
            placements: Vec::new(),
        }));
        assert!(terminal_update_requires_full_frame(&image_update, &current));
        current.images.clone_from(&image_update.images);
        assert!(!terminal_update_requires_full_frame(
            &image_update,
            &current
        ));
        let mut colors = empty_update();
        colors.default_colors = Some([1, 2, 3]);
        assert!(terminal_update_changes_visible_content(&colors));

        assert!(terminal_update_has_visual_damage(
            false,
            false,
            &[false, false],
            &[false, true]
        ));
        assert!(!terminal_update_has_visual_damage(
            false,
            false,
            &[false, false],
            &[false, false]
        ));
    }

    #[test]
    fn raster_damage_follows_pixels_copied_by_queued_scrolls() {
        let forward = splinterm_protocol::TerminalScroll {
            direction: splinterm_protocol::ScrollDirection::Forward,
            start_row: 0,
            end_row: 6,
            rows: 1,
        };
        let mut dirty = [false; 6];
        dirty[5] = true;
        propagate_raster_damage_through_scroll(&mut dirty, &forward);
        assert_eq!(dirty, [false, false, false, false, true, true]);

        // A second coalesced scroll moves the still-unpainted backing pixels again.
        propagate_raster_damage_through_scroll(&mut dirty, &forward);
        assert_eq!(dirty, [false, false, false, true, true, true]);

        let reverse = splinterm_protocol::TerminalScroll {
            direction: splinterm_protocol::ScrollDirection::Reverse,
            start_row: 0,
            end_row: 6,
            rows: 2,
        };
        let mut dirty = [true, false, false, false, false, false];
        propagate_raster_damage_through_scroll(&mut dirty, &reverse);
        assert_eq!(dirty, [true, false, true, false, false, false]);
    }

    #[test]
    fn stale_url_disappears_when_visible_row_is_replaced() {
        let mut view = snapshot(SplintId::new(), 1, 1);
        let text = "https://example.com";
        view.columns = text.len();
        view.rows = 1;
        let mut row = blank_row(view.columns);
        for (cell, character) in row.cells.iter_mut().zip(text.chars()) {
            cell.content = character.to_string();
        }
        view.visible_rows = vec![row];
        let position = CellPosition { row: 0, column: 10 };
        assert!(url_at(&view, position).is_some());
        view.visible_rows[0] = blank_row(view.columns);
        assert!(url_at(&view, position).is_none());
    }

    #[test]
    fn picker_activation_requires_matching_press_and_release_targets() {
        assert_eq!(
            picker_release_activation(Some(PickerHitTarget::New), Some(PickerHitTarget::New)),
            Some(PickerHitTarget::New)
        );
        assert_eq!(
            picker_release_activation(Some(PickerHitTarget::Open(1)), None),
            None
        );
        assert_eq!(
            picker_release_activation(
                Some(PickerHitTarget::Open(1)),
                Some(PickerHitTarget::Open(2))
            ),
            None
        );
    }

    #[test]
    fn modal_input_generations_reject_stale_clipboard_and_reconcile_focus_once() {
        assert!(clipboard_read_is_current(false, 4, 4));
        assert!(!clipboard_read_is_current(true, 4, 4));
        assert!(!clipboard_read_is_current(false, 5, 4));
        assert_eq!(
            reconciled_focus_report(true, false, true),
            Some(b"\x1b[I".to_vec())
        );
        assert_eq!(
            reconciled_focus_report(true, true, false),
            Some(b"\x1b[O".to_vec())
        );
        assert_eq!(reconciled_focus_report(true, true, true), None);
        assert_eq!(reconciled_focus_report(false, false, true), None);
        assert_eq!(
            picker_ime_reconcile(true, true, true),
            PickerImeReconcile::Renew
        );
        assert_eq!(
            picker_ime_reconcile(false, true, true),
            PickerImeReconcile::Enable
        );
        assert_eq!(
            picker_ime_reconcile(false, false, true),
            PickerImeReconcile::None
        );
    }

    #[test]
    fn theme_updates_coalesce_by_shared_generation() {
        let mut pending = None;
        let theme = ResolvedTheme::default();
        retain_newest_theme(
            &mut pending,
            ThemeUpdate {
                generation: 8,
                theme,
                colors: crate::config::ThemeColorConfig::NONE,
                input: None,
            },
        );
        retain_newest_theme(
            &mut pending,
            ThemeUpdate {
                generation: 3,
                theme,
                colors: crate::config::ThemeColorConfig::NONE,
                input: None,
            },
        );
        retain_newest_theme(
            &mut pending,
            ThemeUpdate {
                generation: 13,
                theme,
                colors: crate::config::ThemeColorConfig::NONE,
                input: None,
            },
        );
        assert_eq!(pending.map(|update| update.generation), Some(13));
    }

    #[test]
    fn a_search_page_keeps_where_its_cursor_belongs_and_stale_results_clear() {
        let mut search = SearchUiState {
            in_flight: 1,
            ..SearchUiState::default()
        };
        let mut page = search_page(2, false, Some("r10"));
        page.history_generation = 4;
        page.terminal_revision = 9;
        assert_eq!(search.take_page(page), None);
        assert_eq!((search.page_generation, search.page_revision), (4, 9));
        assert!(search.pending_reveal.is_some());
        assert!(!search.results_match(3), "the snapshot lags: wait");
        assert!(search.pending_reveal.is_some());
        assert!(search.results_match(4));
        assert!(!search.results_match(5), "history was replaced");
        assert!(search.matches.is_empty());
        assert_eq!(search.next_cursor, None);
        assert_eq!(search.pending_reveal, None);
    }

    fn search_page(matches: usize, timed_out: bool, cursor: Option<&str>) -> SearchPage {
        SearchPage {
            splint_id: SplintId::new(),
            incarnation: 1,
            terminal_revision: 1,
            history_generation: 1,
            matches: (0..matches)
                .map(|index| SearchMatch {
                    row_id: u64::try_from(index).unwrap(),
                    start_column: 0,
                    end_column: 1,
                    preview: "x".to_owned(),
                })
                .collect(),
            next_cursor: cursor.map(str::to_owned),
            timed_out,
        }
    }

    #[test]
    fn a_timed_out_empty_search_page_continues_only_for_the_latest_query() {
        let open = |query: &str, in_flight: usize| SearchUiState {
            input: Some(new_search_editor()),
            query: query.to_owned(),
            submitted_query: "needle".to_owned(),
            in_flight,
            ..SearchUiState::default()
        };

        let mut search = open("needle", 1);
        assert_eq!(
            search.take_page(search_page(0, true, Some("c1"))),
            Some("c1".to_owned())
        );
        assert!(search.searching);

        // A match, the end of history, or an untimed empty page stops it.
        for page in [
            search_page(1, true, Some("c2")),
            search_page(0, true, None),
            search_page(0, false, Some("c2")),
        ] {
            let mut search = open("needle", 1);
            assert_eq!(search.take_page(page), None);
            assert!(!search.searching);
        }

        // An older page while a newer search is in flight, an edited query,
        // or a closed search field never continues.
        assert_eq!(
            open("needle", 2).take_page(search_page(0, true, Some("c"))),
            None
        );
        assert_eq!(
            open("other", 1).take_page(search_page(0, true, Some("c"))),
            None
        );
        let mut closed = open("needle", 1);
        closed.input = None;
        assert_eq!(closed.take_page(search_page(0, true, Some("c"))), None);

        let mut resynced = open("needle", 1);
        resynced.searching = true;
        resynced.resync_required();
        assert!(!resynced.searching);
        assert_eq!(resynced.in_flight, 0);
    }

    #[test]
    fn a_viewport_shift_reuses_the_frame_only_for_the_same_rows() {
        let ids = |ids: &[u64]| ids.iter().copied().map(Some).collect::<Vec<_>>();
        let drawn = ids(&[20, 21, 22, 23]);
        // Back two rows: the drawn top two rows are now the bottom two.
        assert!(viewport_shift_matches(&drawn, &ids(&[18, 19, 20, 21]), 2));
        assert!(viewport_shift_matches(&drawn, &ids(&[22, 23, 24, 25]), -2));
        // The view moved two rows by offset, but the rows below it changed:
        // the old frame is stale.
        assert!(!viewport_shift_matches(&drawn, &ids(&[5, 6, 19, 20]), 2));
        assert!(!viewport_shift_matches(&drawn, &ids(&[16, 17, 18, 19]), 2));
        assert!(!viewport_shift_matches(&drawn, &drawn, 0));
        assert!(!viewport_shift_matches(&drawn, &ids(&[1, 2, 3, 4]), 4));
        assert!(!viewport_shift_matches(
            &drawn[..3],
            &ids(&[18, 19, 20, 21]),
            2
        ));
        let unknown = vec![None, None, Some(22), Some(23)];
        assert!(!viewport_shift_matches(
            &unknown,
            &[None, None, None, None],
            2
        ));
    }

    #[test]
    fn return_to_live_at_live_is_consumed_only_on_the_normal_screen() {
        let normal = Some(ActiveScreen::Normal);
        assert_eq!(
            history_key(Some(ActionId::ReturnToLive), false, normal),
            HistoryKey::Navigate(HistoryNavigation::ReturnToLive)
        );
        assert_eq!(
            history_key(Some(ActionId::ReturnToLive), true, normal),
            HistoryKey::Consume
        );
        assert_eq!(
            history_key(
                Some(ActionId::ReturnToLive),
                true,
                Some(ActiveScreen::Alternate)
            ),
            HistoryKey::Forward
        );
        assert_eq!(
            history_key(Some(ActionId::PageUp), true, normal),
            HistoryKey::Navigate(HistoryNavigation::PageUp)
        );
        assert_eq!(history_key(None, true, normal), HistoryKey::Forward);
    }

    #[test]
    fn return_to_live_queues_one_authoritative_resynchronization() {
        let (commands, mut receiver) = tokio::sync::mpsc::channel(1);

        let mut pending = false;
        assert!(!request_return_live_resync(Some(&commands), &mut pending, 0, 0).unwrap());
        assert!(!request_return_live_resync(Some(&commands), &mut pending, 4, 2).unwrap());
        assert!(request_return_live_resync(Some(&commands), &mut pending, 2, 0).unwrap());
        assert!(pending);
        // One on its way: another return to live does not queue a second.
        assert!(!request_return_live_resync(Some(&commands), &mut pending, 2, 0).unwrap());
        assert_eq!(receiver.try_recv().unwrap(), WindowCommand::Resynchronize);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn a_full_command_queue_drops_advisory_commands_without_failing() {
        let (commands, mut receiver) = tokio::sync::mpsc::channel(1);
        commands.try_send(WindowCommand::ReleaseControl).unwrap();
        let mut pending = false;
        assert!(!request_return_live_resync(Some(&commands), &mut pending, 2, 0).unwrap());
        assert!(!pending, "a dropped request is asked for again");
        assert!(!try_advisory_command(&commands, WindowCommand::Resynchronize).unwrap());
        assert_eq!(receiver.try_recv().unwrap(), WindowCommand::ReleaseControl);
        drop(receiver);
        assert!(try_advisory_command(&commands, WindowCommand::Resynchronize).is_err());
    }

    #[test]
    fn saturated_command_queue_buffers_input_without_blocking_update_drain() {
        let (commands, mut command_receiver) = tokio::sync::mpsc::channel(1);
        commands.try_send(WindowCommand::Input(vec![1])).unwrap();
        let (updates, mut update_receiver) = tokio::sync::mpsc::channel(1);
        updates.try_send(7).unwrap();
        let mut pending = VecDeque::new();

        assert_eq!(
            queue_terminal_input(Some(&commands), &mut pending, vec![2]).unwrap(),
            TerminalInputQueueOutcome::Pending
        );
        assert_eq!(pending.front().unwrap().bytes, [2]);
        assert_eq!(
            update_receiver.try_recv().unwrap(),
            7,
            "pending input must not block the Wayland update consumer"
        );
        assert_eq!(
            queue_terminal_input(Some(&commands), &mut pending, vec![3]).unwrap(),
            TerminalInputQueueOutcome::Pending
        );
        assert_eq!(pending.front().unwrap().bytes, [2, 3]);
        assert_eq!(
            command_receiver.try_recv().unwrap(),
            WindowCommand::Input(vec![1])
        );
        assert!(try_flush_pending_terminal_input(&mut pending));
        assert!(pending.is_empty());
        assert_eq!(
            command_receiver.try_recv().unwrap(),
            WindowCommand::Input(vec![2, 3])
        );
    }

    #[test]
    fn pending_terminal_input_remains_bound_to_its_original_pane() {
        let (first, mut first_receiver) = tokio::sync::mpsc::channel(1);
        first.try_send(WindowCommand::Input(vec![1])).unwrap();
        let (second, mut second_receiver) = tokio::sync::mpsc::channel(1);
        let mut pending = VecDeque::new();

        assert_eq!(
            queue_terminal_input(Some(&first), &mut pending, vec![2]).unwrap(),
            TerminalInputQueueOutcome::Pending
        );
        assert_eq!(
            queue_terminal_input(Some(&second), &mut pending, vec![3]).unwrap(),
            TerminalInputQueueOutcome::Queued
        );
        assert_eq!(
            second_receiver.try_recv().unwrap(),
            WindowCommand::Input(vec![3])
        );
        assert_eq!(
            first_receiver.try_recv().unwrap(),
            WindowCommand::Input(vec![1])
        );
        assert!(try_flush_pending_terminal_input(&mut pending));
        assert_eq!(
            first_receiver.try_recv().unwrap(),
            WindowCommand::Input(vec![2])
        );
    }

    #[test]
    fn saturated_pane_senders_share_the_pending_input_bound_without_loss() {
        let (first, mut first_receiver) = tokio::sync::mpsc::channel(1);
        first.try_send(WindowCommand::Input(vec![1])).unwrap();
        let (second, mut second_receiver) = tokio::sync::mpsc::channel(1);
        second.try_send(WindowCommand::Input(vec![2])).unwrap();
        let mut pending = VecDeque::new();

        assert_eq!(
            queue_terminal_input(Some(&first), &mut pending, vec![3]).unwrap(),
            TerminalInputQueueOutcome::Pending
        );
        assert_eq!(
            queue_terminal_input(Some(&second), &mut pending, vec![4]).unwrap(),
            TerminalInputQueueOutcome::Pending
        );
        assert_eq!(pending_terminal_input_bytes(&pending), 2);
        assert_eq!(
            first_receiver.try_recv().unwrap(),
            WindowCommand::Input(vec![1])
        );
        assert_eq!(
            second_receiver.try_recv().unwrap(),
            WindowCommand::Input(vec![2])
        );
        assert!(try_flush_pending_terminal_input(&mut pending));
        assert_eq!(
            first_receiver.try_recv().unwrap(),
            WindowCommand::Input(vec![3])
        );
        assert_eq!(
            second_receiver.try_recv().unwrap(),
            WindowCommand::Input(vec![4])
        );
    }

    #[test]
    fn closed_pending_pane_is_discarded_without_blocking_surviving_input() {
        let (closed, closed_receiver) = tokio::sync::mpsc::channel(1);
        closed.try_send(WindowCommand::Input(vec![1])).unwrap();
        let (surviving, mut surviving_receiver) = tokio::sync::mpsc::channel(1);
        surviving.try_send(WindowCommand::Input(vec![2])).unwrap();
        let mut pending = VecDeque::new();

        assert_eq!(
            queue_terminal_input(Some(&closed), &mut pending, vec![3]).unwrap(),
            TerminalInputQueueOutcome::Pending
        );
        assert_eq!(
            queue_terminal_input(Some(&surviving), &mut pending, vec![4]).unwrap(),
            TerminalInputQueueOutcome::Pending
        );
        drop(closed_receiver);
        assert_eq!(
            surviving_receiver.try_recv().unwrap(),
            WindowCommand::Input(vec![2])
        );

        assert!(try_flush_pending_terminal_input(&mut pending));
        assert!(pending.is_empty());
        assert_eq!(
            surviving_receiver.try_recv().unwrap(),
            WindowCommand::Input(vec![4])
        );
    }

    #[test]
    fn pending_terminal_input_is_bounded_and_keeps_input_units_atomic() {
        let (commands, _receiver) = tokio::sync::mpsc::channel(1);
        commands.try_send(WindowCommand::Input(vec![1])).unwrap();
        let mut pending = VecDeque::new();
        let retained = vec![2; MAX_PENDING_TERMINAL_INPUT_BYTES - 2];

        assert_eq!(
            queue_terminal_input(Some(&commands), &mut pending, retained).unwrap(),
            TerminalInputQueueOutcome::Pending
        );
        let before = pending.front().unwrap().bytes.clone();
        let bracketed_paste_end = b"\x1b[201~".to_vec();
        assert_eq!(
            queue_terminal_input(Some(&commands), &mut pending, bracketed_paste_end).unwrap(),
            TerminalInputQueueOutcome::Saturated
        );
        assert_eq!(pending.front().unwrap().bytes, before);
        assert_eq!(
            pending_terminal_input_bytes(&pending),
            MAX_PENDING_TERMINAL_INPUT_BYTES - 2
        );
    }

    #[test]
    fn bounded_command_queue_reports_control_overflow_and_disconnect() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        sender.try_send(WindowCommand::Input(vec![3])).unwrap();
        let error = try_window_command(&sender, WindowCommand::ReleaseControl)
            .expect_err("bounded control overflow");
        assert!(error.to_string().contains("overflow"));
        assert_eq!(
            try_queue_control_release(&sender),
            ControlReleaseOutcome::Retry
        );
        assert_eq!(
            try_queue_focus_report(&sender, false),
            ControlReleaseOutcome::Retry
        );
        assert!(receiver.try_recv().is_ok());
        assert_eq!(
            try_queue_focus_report(&sender, false),
            ControlReleaseOutcome::Queued
        );
        assert_eq!(
            receiver.try_recv().unwrap(),
            WindowCommand::FocusReport(false)
        );
        assert_eq!(
            try_queue_control_release(&sender),
            ControlReleaseOutcome::Queued
        );
        assert_eq!(receiver.try_recv().unwrap(), WindowCommand::ReleaseControl);
        drop(receiver);
        let error = try_window_command(&sender, WindowCommand::Input(vec![4]))
            .expect_err("disconnected receiver");
        assert!(error.to_string().contains("disconnected"));
        assert_eq!(
            try_queue_control_release(&sender),
            ControlReleaseOutcome::Disconnected
        );
        assert_eq!(
            try_queue_focus_report(&sender, true),
            ControlReleaseOutcome::Disconnected
        );

        let new_lair = WindowTopologyCommand::NewLair {
            source_splint_id: SplintId::new(),
        };
        let (topology_sender, mut topology_receiver) = tokio::sync::mpsc::channel(1);
        try_topology_command(
            &topology_sender,
            WindowTopologyCommand::RequestSessionPicker,
        )
        .expect("first topology command");
        let error = try_topology_command(&topology_sender, new_lair.clone())
            .expect_err("bounded topology overflow");
        assert!(error.to_string().contains("full"));
        assert!(topology_receiver.try_recv().is_ok());
        drop(topology_receiver);
        let error = try_topology_command(
            &topology_sender,
            WindowTopologyCommand::RequestSessionPicker,
        )
        .expect_err("disconnected topology receiver");
        assert!(error.to_string().contains("closed"));

        let target = SplintId::new();
        let pending = SplintId::new();
        let pending_command = || WindowTopologyCommand::Split {
            dojo_id: DojoId::new(),
            target,
            axis: Axis::Horizontal,
            pending: Some(pending),
        };
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        try_topology_command(&sender, new_lair).unwrap();
        let mut rolled_back = None;
        let error = try_topology_command_with_rollback(
            Some(&sender),
            pending_command(),
            |rollback_target, rollback_pending| {
                rolled_back = Some((rollback_target, rollback_pending));
                Ok(())
            },
        )
        .expect_err("full pending split queue");
        assert!(error.to_string().contains("full"));
        assert_eq!(rolled_back, Some((target, pending)));

        assert!(receiver.try_recv().is_ok());
        drop(receiver);
        rolled_back = None;
        let error = try_topology_command_with_rollback(
            Some(&sender),
            pending_command(),
            |rollback_target, rollback_pending| {
                rolled_back = Some((rollback_target, rollback_pending));
                Ok(())
            },
        )
        .expect_err("closed pending split queue");
        assert!(error.to_string().contains("closed"));
        assert_eq!(rolled_back, Some((target, pending)));
    }

    #[test]
    fn a_reload_reaches_the_topology_manager_even_with_a_full_command_queue() {
        use crate::config::{AppConfig, TopologyReloadSettings};

        let startup = AppConfig::default();
        let (settings, mut manager_view) =
            tokio::sync::watch::channel(TopologyReloadSettings::from_config(&startup));
        // A busy manager: its bounded command queue is full.
        let (commands, mut queued) = tokio::sync::mpsc::channel(1);
        try_topology_command(&commands, WindowTopologyCommand::RequestSessionPicker).unwrap();
        assert!(
            try_topology_command(&commands, WindowTopologyCommand::RequestSessionPicker).is_err()
        );

        let mut reloaded = AppConfig::default();
        reloaded.multiplexer_lifetime.close_unused = true;
        reloaded.multiplexer_lifetime.persist_on_tab_organization = false;
        reloaded.new_dojo_directory = splinterm_protocol::LaunchDirectory::Inherit;
        reloaded.new_splint_directory = splinterm_protocol::LaunchDirectory::Home;
        publish_topology_settings(Some(&settings), &reloaded);

        assert!(manager_view.has_changed().unwrap());
        assert_eq!(
            *manager_view.borrow_and_update(),
            TopologyReloadSettings::from_config(&reloaded)
        );
        // The reload took no queue slot: only the original command is queued.
        assert!(queued.try_recv().is_ok());
        assert!(queued.try_recv().is_err());
        publish_topology_settings(None, &reloaded);
    }

    #[test]
    fn duplicate_resize_is_suppressed() {
        let size = (80, 24, 1_120, 720);
        assert!(resize_changed(None, size));
        assert!(!resize_changed(Some(size), size));
        assert!(resize_changed(Some(size), (81, 24, 1_134, 720)));
    }

    #[test]
    fn output_lifecycle_preserves_grid_until_surface_configure() {
        for cause in [
            TerminalResizeCause::OutputDpiChanged,
            TerminalResizeCause::CompositorScaleChanged,
        ] {
            assert!(!terminal_resize_allowed(cause, false));
            assert!(!terminal_resize_allowed(cause, true));
        }
        assert!(terminal_resize_allowed(
            TerminalResizeCause::SurfaceConfigure,
            true
        ));
        assert!(terminal_resize_allowed(
            TerminalResizeCause::SurfaceConfigure,
            false
        ));
    }

    #[test]
    fn startup_snapshot_emits_only_the_first_grid_resize() {
        assert!(terminal_resize_allowed(
            TerminalResizeCause::SnapshotAvailable,
            false
        ));
        assert!(!terminal_resize_allowed(
            TerminalResizeCause::SnapshotAvailable,
            true
        ));
    }

    #[test]
    fn output_entry_order_selects_most_recent_and_retains_when_unmapped() {
        let mut entered = Vec::new();
        note_output_enter(&mut entered, &1);
        note_output_enter(&mut entered, &2);
        note_output_enter(&mut entered, &1);
        assert_eq!(entered, vec![2, 1]);
        assert!(!note_output_leave(&mut entered, &2));
        assert_eq!(entered.last(), Some(&1));
        assert!(note_output_leave(&mut entered, &1));
        assert!(entered.is_empty());
        // App deliberately leaves renderer's last DPI observation unchanged here.
    }

    #[test]
    fn draw_buffer_selection_preserves_committed_canvas_and_stays_bounded() {
        for count in 0..=MAX_SHM_BUFFERS {
            for mask in 0..(1_usize << count) {
                let choice = choose_draw_buffer(count, |index| mask & (1 << index) != 0);
                let alternative = (1..count).find(|index| mask & (1 << index) != 0);
                let expected = alternative.map_or_else(
                    || {
                        if count < MAX_SHM_BUFFERS {
                            DrawBufferChoice::Allocate
                        } else {
                            DrawBufferChoice::Wait
                        }
                    },
                    DrawBufferChoice::Reuse,
                );
                assert_eq!(choice, expected, "count={count} available={mask:b}");
            }
        }
    }

    #[test]
    fn composing_next_canvas_does_not_clear_committed_chrome() {
        let mut canvases = [vec![7_u8; 32], vec![0_u8; 32]];
        for marker in 8_u8..12 {
            let displayed = canvases[0].clone();
            let DrawBufferChoice::Reuse(index) = choose_draw_buffer(canvases.len(), |_| true)
            else {
                panic!("the alternate released buffer is reusable");
            };
            // Full terminal backing synchronization temporarily clears chrome.
            canvases[index].fill(0);
            assert_eq!(canvases[0], displayed);
            // Chrome is rebuilt before submission; then rotate whole buffers.
            canvases[index][..8].fill(marker);
            canvases.swap(0, index);
            assert_eq!(&canvases[0][..8], &[marker; 8]);
            assert_eq!(canvases[1], displayed);
        }
    }

    #[test]
    fn remote_observer_buffer_contains_header_and_translated_terminal_at_all_scales() {
        use crate::geometry::{CellGeometry, TerminalPadding};
        for scale in [120, 150, 180, 240] {
            let logical_size = (801, 601);
            let header = tab_strip_height(
                false,
                false,
                true,
                DEFAULT_TAB_STRIP_LOGICAL_HEIGHT,
                logical_size.1,
            );
            let geometry = WindowGeometry::fit_window(
                logical_size.0,
                logical_size.1 - header,
                CellGeometry::new(8, 16).unwrap(),
                TerminalPadding::uniform(0),
                scale,
                2,
                65535,
                2,
                65535,
            )
            .unwrap()
            .translated(0, logical_extent_to_buffer(header, scale).unwrap())
            .unwrap();
            let content_only = geometry.buffer_layout().unwrap();
            let full = buffer_dimensions(logical_size.0, logical_size.1, scale).unwrap();
            assert!(content_only.1 < full.1);
            assert_eq!(
                window_buffer_dimensions(false, false, true, logical_size, scale, Some(geometry))
                    .unwrap(),
                full,
                "remote observer must allocate the full surface at scale {scale}"
            );
            let grid = geometry.visible_grid_rect;
            assert_eq!(grid.y, logical_extent_to_buffer(header, scale).unwrap());
            assert!(grid.x + grid.width <= full.0);
            assert!(grid.y + grid.height <= full.1);
            assert!(
                grid.y + grid.height > content_only.1,
                "old buffer clips bottom rows"
            );
            // Retain the existing local geometry path and full-surface pane/tab paths.
            for (panes, tabs, expected) in [
                (false, false, content_only),
                (true, false, full),
                (false, true, full),
            ] {
                assert_eq!(
                    window_buffer_dimensions(
                        panes,
                        tabs,
                        false,
                        logical_size,
                        scale,
                        Some(geometry)
                    )
                    .unwrap(),
                    expected
                );
            }
            assert_eq!(
                window_buffer_dimensions(false, false, false, logical_size, scale, None).unwrap(),
                full
            );
        }
    }

    #[test]
    fn buffer_dimensions_scale_logical_size_and_stride() {
        assert_eq!(
            buffer_dimensions(960, 600, 120).expect("1x"),
            (960, 600, 3_840)
        );
        assert_eq!(
            buffer_dimensions(960, 600, 240).expect("2x"),
            (1_920, 1_200, 7_680)
        );
    }

    #[test]
    fn window_presentation_memory_is_checked_before_backing_or_shm_allocation() {
        let maximum_pixels = MAX_WINDOW_PRESENTATION_BYTES / (MAX_SHM_BUFFERS + 1) / 4;
        let maximum_height = u32::try_from(maximum_pixels).unwrap();
        let backing = bounded_window_backing_len(1, maximum_height).unwrap();
        assert!(backing.checked_mul(MAX_SHM_BUFFERS + 1).unwrap() <= MAX_WINDOW_PRESENTATION_BYTES);
        assert!(bounded_window_backing_len(1, maximum_height + 1).is_err());
        assert!(bounded_window_backing_len(u32::MAX, 2).is_err());
    }

    #[test]
    fn fractional_buffer_dimensions_cover_phase_six_scale_matrix() {
        assert_eq!(buffer_dimensions(801, 601, 120).unwrap(), (801, 601, 3_204));
        assert_eq!(
            buffer_dimensions(801, 601, 150).unwrap(),
            (1_002, 752, 4_008)
        );
        assert_eq!(
            buffer_dimensions(801, 601, 180).unwrap(),
            (1_202, 902, 4_808)
        );
        assert_eq!(
            buffer_dimensions(801, 601, 240).unwrap(),
            (1_602, 1_202, 6_408)
        );
    }

    #[test]
    fn owned_modal_ime_batches_cross_only_the_fresh_generation_barrier() {
        assert!(ime_batch_blocked(
            true,
            false,
            true,
            Some(OwnedFieldTarget::BindingHelp)
        ));
        assert!(!ime_batch_blocked(
            true,
            false,
            false,
            Some(OwnedFieldTarget::BindingHelp)
        ));
        assert!(ime_batch_blocked(true, false, false, None));
        assert!(!ime_batch_blocked(false, false, false, None));
    }

    #[test]
    fn ime_batches_are_bounded_replace_commits_and_clear_committed_preedit() {
        let mut ime = ImeState {
            entered: true,
            focused: true,
            ..ImeState::default()
        };
        assert!(!ime.composing());
        let decomposed = format!("e{}", '\u{301}');
        ime.set_preedit(Some(decomposed.clone()));
        assert!(ime.composing());
        ime.note_client_commit();
        let (_, visible, commit) = ime.finish(1);
        assert_eq!(visible.as_deref(), Some(decomposed.as_str()));
        assert!(commit.is_none());

        ime.set_commit(Some("first".into()));
        ime.set_commit(Some("final".into()));
        let (serial_matches, visible, commit) = ime.finish(1);
        assert!(serial_matches);
        assert_eq!(visible, None);
        assert_eq!(commit.as_deref(), Some("final"));

        ime.set_preedit(Some("x".repeat(MAX_PREEDIT_BYTES + 1)));
        assert!(!ime.composing());
        ime.set_preedit(Some("modal".into()));
        ime.clear_composition();
        assert!(!ime.composing());
        assert!(ime.entered);
        assert!(ime.focused);
    }

    #[test]
    fn detached_ime_repaint_targets_the_display_cursor_row_only() {
        let mut display = snapshot(SplintId::new(), 1, 1);
        display.columns = 2;
        display.rows = 3;
        display.cursor_column = 0;
        display.cursor_row = 1;
        display.visible_rows = ["a0", "b1", "c2"]
            .map(|text| {
                let mut row = blank_row(2);
                for (cell, character) in row.cells.iter_mut().zip(text.chars()) {
                    cell.content = character.to_string();
                }
                row
            })
            .into();

        assert_eq!(apply_ime_preedit(&mut display, Some("Z")), Some(1));
        let contents = display
            .visible_rows
            .iter()
            .map(|row| {
                row.cells
                    .iter()
                    .map(|cell| &cell.content)
                    .cloned()
                    .collect()
            })
            .collect::<Vec<String>>();
        assert_eq!(contents, ["a0", "Z1", "c2"]);

        display.cursor_row = -1;
        assert_eq!(apply_ime_preedit(&mut display, None), None);
        assert_eq!(
            display.visible_rows[0].cells[0].content, "a",
            "a hidden live cursor must not replace a visible history row"
        );
    }

    #[test]
    fn reduced_motion_and_focus_suppress_cursor_blink() {
        let modes = normal_modes();
        assert!(cursor_blink_enabled(false, true, modes));
        assert!(!cursor_blink_enabled(true, true, modes));
        assert!(!cursor_blink_enabled(false, false, modes));
        assert!(presented_cursor_visible(true, false));
        assert!(!presented_cursor_visible(false, false));
    }

    #[test]
    fn event_loop_timeout_is_event_driven_except_for_active_deadlines() {
        assert_eq!(
            event_loop_timeout(false, false, None),
            Some(IDLE_EVENT_LOOP_TIMEOUT)
        );
        assert_eq!(
            event_loop_timeout(false, false, Some(Duration::from_millis(125))),
            Some(Duration::from_millis(375))
        );
        assert_eq!(
            event_loop_timeout(false, false, Some(Duration::from_secs(1))),
            Some(Duration::ZERO)
        );
        assert_eq!(
            event_loop_timeout(false, true, Some(Duration::ZERO)),
            Some(SIGNOFF_TICK_INTERVAL)
        );
        assert_eq!(event_loop_timeout(true, false, None), None);
        assert_eq!(
            command_retry_dispatch_timeout(Some(IDLE_EVENT_LOOP_TIMEOUT), true),
            Some(PENDING_INPUT_RETRY_INTERVAL)
        );
        assert_eq!(
            command_retry_dispatch_timeout(Some(Duration::ZERO), true),
            Some(Duration::ZERO)
        );
        assert_eq!(command_retry_dispatch_timeout(None, true), None);
    }

    #[test]
    fn ime_done_applies_events_even_when_client_state_serial_is_stale() {
        let mut ime = ImeState::default();
        ime.set_commit(Some("界".into()));
        let (serial_matches, _, commit) = ime.finish(9);
        assert!(!serial_matches);
        assert_eq!(commit.as_deref(), Some("界"));
    }

    #[test]
    fn buffer_dimensions_reject_zero_scale_and_overflow() {
        assert!(buffer_dimensions(960, 600, 0).is_err());
        assert!(buffer_dimensions(u32::MAX, 1, 240).is_err());
        assert!(buffer_dimensions(1, u32::MAX, 240).is_err());
        assert!(buffer_dimensions(i32::MAX as u32 / 4 + 1, 1, 120).is_err());
    }
}
