//! Pure input encoding and shortcut classification.

mod keyboard;
mod pointer;
mod shortcuts;

pub(in crate::wayland) use keyboard::key_input;
pub(in crate::wayland) use pointer::{
    HistoryNavigation, ModalPointerFrame, MouseAction, PickerImeReconcile, PressOwner,
    WheelAccumulator, WheelOutcome, application_motion, classify_press, clipboard_read_is_current,
    history_overlay_status, history_return_to_live_hit, local_selection_owner, mouse_report,
    pending_selection_drag_anchor, picker_ime_reconcile, picker_release_activation,
    pointer_axis_focus_target, reconciled_focus_report, take_press_owner,
};
pub(in crate::wayland) use shortcuts::{
    CommandPaletteShortcutAction, CopyModeDesktopAction, FontZoomAction, PaneFocusAction,
    PaneTopologyAction, SessionPickerShortcutAction, TabShortcutAction,
    binding_help_repeat_consumed, command_palette_shortcut_action, consume_detached_enter_press,
    copy_mode_desktop_action, font_zoom_action, keymap_press_consumes_key, keymap_press_for,
    owned_field_clipboard_action, pane_focus_action, pane_topology_action, picker_repeat_consumed,
    session_picker_shortcut_action, shortcut_action_for, shortcut_paused_during_handoff,
    tab_action_dispatch_allowed, tab_shortcut_action,
};
