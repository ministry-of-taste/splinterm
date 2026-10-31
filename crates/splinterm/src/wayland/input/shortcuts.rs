//! Pure shortcut resolution before adapter-side effects.

use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use crate::{
    keymap::{
        ActionId, ActiveModifiers, KeyIdentity, KeymapPress, PrefixState, PressedKey,
        ResolvedKeymap,
    },
    pane::FocusDirection,
};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym, Modifiers};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::wayland) enum PaneTopologyAction {
    Split(splinterm_core::Axis),
    Close,
    AdjustRatio(i16),
    ResizeCells(FocusDirection, u16),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::wayland) enum CommandPaletteShortcutAction {
    Open,
    Consume,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::wayland) enum CopyModeDesktopAction {
    CopySelection,
    Consume,
}

pub(in crate::wayland) fn consume_detached_enter_press(
    keysym: Keysym,
    modifiers: Modifiers,
    viewport_detached: bool,
    raw_code: u32,
    consumed_keys: &mut HashSet<u32>,
) -> Option<super::HistoryNavigation> {
    let navigation = (viewport_detached
        && matches!(keysym, Keysym::Return | Keysym::KP_Enter)
        && !modifiers.ctrl
        && !modifiers.shift
        && !modifiers.alt
        && !modifiers.logo)
        .then_some(super::HistoryNavigation::ReturnToLive)?;
    consumed_keys.insert(raw_code);
    Some(navigation)
}

pub(in crate::wayland) fn copy_mode_desktop_action(
    keysym: Keysym,
    modifiers: Modifiers,
) -> Option<CopyModeDesktopAction> {
    if !modifiers.logo || modifiers.ctrl || modifiers.alt || modifiers.shift {
        return None;
    }
    match keysym {
        Keysym::c | Keysym::C => Some(CopyModeDesktopAction::CopySelection),
        Keysym::v | Keysym::V | Keysym::x | Keysym::X | Keysym::z | Keysym::Z => {
            Some(CopyModeDesktopAction::Consume)
        }
        _ => None,
    }
}

fn active_modifiers(modifiers: Modifiers) -> ActiveModifiers {
    ActiveModifiers {
        ctrl: modifiers.ctrl,
        shift: modifiers.shift,
        alt: modifiers.alt,
        logo: modifiers.logo,
    }
}

pub(in crate::wayland) fn shortcut_action_for(
    keymap: &ResolvedKeymap,
    event: &KeyEvent,
    modifiers: Modifiers,
) -> Option<ActionId> {
    keymap.pressed_action(
        pressed_key(event.keysym, event.raw_code, modifiers)?,
        active_modifiers(modifiers),
    )
}

pub(in crate::wayland) fn binding_help_repeat_consumed(
    binding_help_open: bool,
    keysym: Keysym,
) -> bool {
    binding_help_open && keysym == Keysym::Escape
}

/// Whether a held key must not repeat into an inline picker: Tab would
/// switch the `dojos` scope back and forth, and R ask again while the
/// retry is still loading.
pub(in crate::wayland) fn picker_repeat_consumed(
    inline_open: bool,
    retryable: bool,
    keysym: Keysym,
) -> bool {
    inline_open
        && (matches!(keysym, Keysym::Tab | Keysym::ISO_Left_Tab)
            || (retryable && matches!(keysym, Keysym::r | Keysym::R)))
}

pub(in crate::wayland) fn owned_field_clipboard_action(
    keymap: &ResolvedKeymap,
    keysym: Keysym,
    modifiers: Modifiers,
) -> Option<ActionId> {
    keymap
        .action(key_identity(keysym)?, active_modifiers(modifiers))
        .filter(|action| matches!(action, ActionId::ClipboardCopy | ActionId::ClipboardPaste))
}

/// An edit a chord makes in a Splinterm text field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::wayland) enum FieldEdit {
    Copy,
    Paste,
    Cut,
    Undo,
    SelectAll,
}

/// Plan 0074: the edit a chord makes in a Splinterm text field (the command
/// palette, key binding help, search, and rename prompts): `Super+C/V/X/A/Z`,
/// and `Ctrl+C/V/X/A`, which Omarchy's `Super+C/V/X/A` send to a window
/// without its `terminal` tag. Only those fields use this; in the terminal,
/// `Ctrl+C` and the rest stay the program's keys.
pub(in crate::wayland) fn owned_field_edit(
    keysym: Keysym,
    modifiers: Modifiers,
) -> Option<FieldEdit> {
    let desktop = modifiers.logo && !modifiers.ctrl && !modifiers.alt && !modifiers.shift;
    let ctrl = modifiers.ctrl && !modifiers.logo && !modifiers.alt && !modifiers.shift;
    if !desktop && !ctrl {
        return None;
    }
    match keysym {
        Keysym::c | Keysym::C => Some(FieldEdit::Copy),
        Keysym::v | Keysym::V => Some(FieldEdit::Paste),
        Keysym::x | Keysym::X => Some(FieldEdit::Cut),
        Keysym::a | Keysym::A => Some(FieldEdit::SelectAll),
        Keysym::z | Keysym::Z if desktop => Some(FieldEdit::Undo),
        _ => None,
    }
}

pub(in crate::wayland) fn keymap_press_for(
    keymap: &ResolvedKeymap,
    prefix_state: &mut PrefixState,
    keysym: Keysym,
    modifiers: Modifiers,
    raw_code: u32,
    now: Instant,
    timeout: Duration,
) -> KeymapPress {
    let Some(key) = pressed_key(keysym, raw_code, modifiers) else {
        if prefix_state.is_armed() {
            if is_modifier_key(keysym) {
                return KeymapPress::PrefixModifier;
            }
            prefix_state.clear();
            return KeymapPress::Consumed(None);
        }
        return KeymapPress::PassThrough;
    };
    keymap.press(
        prefix_state,
        key,
        active_modifiers(modifiers),
        raw_code,
        now,
        timeout,
    )
}

pub(in crate::wayland) fn command_palette_shortcut_action(
    action: Option<ActionId>,
    repeat: bool,
    managed_tabs: bool,
    blocked: bool,
) -> Option<CommandPaletteShortcutAction> {
    if !managed_tabs || action != Some(ActionId::CommandPalette) {
        return None;
    }
    Some(if repeat || blocked {
        CommandPaletteShortcutAction::Consume
    } else {
        CommandPaletteShortcutAction::Open
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::wayland) enum SessionPickerShortcutAction {
    Request,
    Consume,
}

pub(in crate::wayland) fn session_picker_shortcut_action(
    action: Option<ActionId>,
    repeat: bool,
    request_pending: bool,
) -> Option<SessionPickerShortcutAction> {
    if action != Some(ActionId::RecentSessions) {
        return None;
    }
    Some(if repeat || request_pending {
        SessionPickerShortcutAction::Consume
    } else {
        SessionPickerShortcutAction::Request
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::wayland) enum TabShortcutAction {
    Next,
    Previous,
    NewDojo,
    Close,
    CloseOthers,
    Select(usize),
    Move(isize),
    Rename,
    Terminate,
    Choose,
    Consume,
}

pub(in crate::wayland) fn tab_shortcut_action(
    action: Option<ActionId>,
    repeat: bool,
    managed_tabs: bool,
) -> Option<TabShortcutAction> {
    if !managed_tabs {
        return None;
    }
    let action = match action? {
        ActionId::NextDojo => TabShortcutAction::Next,
        ActionId::PreviousDojo => TabShortcutAction::Previous,
        ActionId::NewDojo => TabShortcutAction::NewDojo,
        ActionId::CloseCurrentTab => TabShortcutAction::Close,
        ActionId::CloseOtherTabs => TabShortcutAction::CloseOthers,
        ActionId::SelectDojo1 => TabShortcutAction::Select(0),
        ActionId::SelectDojo2 => TabShortcutAction::Select(1),
        ActionId::SelectDojo3 => TabShortcutAction::Select(2),
        ActionId::SelectDojo4 => TabShortcutAction::Select(3),
        ActionId::SelectDojo5 => TabShortcutAction::Select(4),
        ActionId::SelectDojo6 => TabShortcutAction::Select(5),
        ActionId::SelectDojo7 => TabShortcutAction::Select(6),
        ActionId::SelectDojo8 => TabShortcutAction::Select(7),
        ActionId::SelectDojo9 => TabShortcutAction::Select(8),
        ActionId::MoveDojoLeft => TabShortcutAction::Move(-1),
        ActionId::MoveDojoRight => TabShortcutAction::Move(1),
        ActionId::RenameCurrentTab => TabShortcutAction::Rename,
        ActionId::TerminateCurrentDojo => TabShortcutAction::Terminate,
        ActionId::DojoChooser => TabShortcutAction::Choose,
        _ => return None,
    };
    Some(if repeat {
        TabShortcutAction::Consume
    } else {
        action
    })
}

/// Plan 0074: whether `action` is a clipboard chord. Its key is the
/// Window's from press to release, so a held copy or paste chord, or a
/// synthetic one left stuck down, never repeats into the program as
/// `Ctrl+C` (SIGINT), `Ctrl+V` or `Shift+Insert`.
pub(in crate::wayland) const fn clipboard_chord(action: Option<ActionId>) -> bool {
    matches!(
        action,
        Some(ActionId::ClipboardCopy | ActionId::ClipboardPaste | ActionId::ClipboardSaveImage)
    )
}

/// Whether a resolved keymap press owns its raw key until release, so the
/// repeat handler drops its auto-repeats. Font zoom keeps repeating, and tab
/// and command-palette actions outside a managed-tab window decline and fall
/// through to the terminal, so their repeats must reach the terminal too.
pub(in crate::wayland) fn keymap_press_consumes_key(action: ActionId, managed_tabs: bool) -> bool {
    if matches!(action, ActionId::ZoomIn | ActionId::ZoomOut) {
        return false;
    }
    managed_tabs
        || (tab_shortcut_action(Some(action), false, true).is_none()
            && command_palette_shortcut_action(Some(action), false, true, false).is_none())
}

pub(in crate::wayland) const fn tab_action_dispatch_allowed(blocking_states: [bool; 5]) -> bool {
    !(blocking_states[0]
        || blocking_states[1]
        || blocking_states[2]
        || blocking_states[3]
        || blocking_states[4])
}

pub(in crate::wayland) fn pane_topology_action(
    action: Option<ActionId>,
) -> Option<PaneTopologyAction> {
    match action? {
        // Action orientation names the divider; core axes name pane placement.
        ActionId::SplitHorizontal => {
            Some(PaneTopologyAction::Split(splinterm_core::Axis::Vertical))
        }
        ActionId::SplitVertical => {
            Some(PaneTopologyAction::Split(splinterm_core::Axis::Horizontal))
        }
        ActionId::CloseFocusedPane => Some(PaneTopologyAction::Close),
        ActionId::ResizePaneSmaller => Some(PaneTopologyAction::AdjustRatio(-50)),
        ActionId::ResizePaneLarger => Some(PaneTopologyAction::AdjustRatio(50)),
        ActionId::ResizePaneLeftFive => {
            Some(PaneTopologyAction::ResizeCells(FocusDirection::Left, 5))
        }
        ActionId::ResizePaneRightFive => {
            Some(PaneTopologyAction::ResizeCells(FocusDirection::Right, 5))
        }
        ActionId::ResizePaneUpFive => Some(PaneTopologyAction::ResizeCells(FocusDirection::Up, 5)),
        ActionId::ResizePaneDownFive => {
            Some(PaneTopologyAction::ResizeCells(FocusDirection::Down, 5))
        }
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::wayland) enum PaneFocusAction {
    Direction(FocusDirection),
}

pub(in crate::wayland) fn pane_focus_action(action: Option<ActionId>) -> Option<PaneFocusAction> {
    let direction = match action? {
        ActionId::FocusLeft => FocusDirection::Left,
        ActionId::FocusRight => FocusDirection::Right,
        ActionId::FocusUp => FocusDirection::Up,
        ActionId::FocusDown => FocusDirection::Down,
        _ => return None,
    };
    Some(PaneFocusAction::Direction(direction))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::wayland) enum FontZoomAction {
    Increase,
    Decrease,
    Reset,
}

pub(in crate::wayland) fn font_zoom_action(action: Option<ActionId>) -> Option<FontZoomAction> {
    match action? {
        ActionId::ZoomIn => Some(FontZoomAction::Increase),
        ActionId::ZoomOut => Some(FontZoomAction::Decrease),
        ActionId::ResetZoom => Some(FontZoomAction::Reset),
        _ => None,
    }
}

fn is_modifier_key(keysym: Keysym) -> bool {
    matches!(
        keysym,
        Keysym::Shift_L
            | Keysym::Shift_R
            | Keysym::Control_L
            | Keysym::Control_R
            | Keysym::Alt_L
            | Keysym::Alt_R
            | Keysym::Super_L
            | Keysym::Super_R
            | Keysym::Meta_L
            | Keysym::Meta_R
            | Keysym::ISO_Level3_Shift
            | Keysym::Caps_Lock
            | Keysym::Num_Lock
    )
}

/// Maps evdev `KEY_1`..`KEY_9` on the main number row to their digits.
const fn number_row_digit(raw_code: u32) -> Option<KeyIdentity> {
    Some(match raw_code {
        2 => KeyIdentity::One,
        3 => KeyIdentity::Two,
        4 => KeyIdentity::Three,
        5 => KeyIdentity::Four,
        6 => KeyIdentity::Five,
        7 => KeyIdentity::Six,
        8 => KeyIdentity::Seven,
        9 => KeyIdentity::Eight,
        10 => KeyIdentity::Nine,
        _ => return None,
    })
}

/// Identifies one press by its produced symbol. With Shift held, the physical
/// number-row digit follows as a fallback, so `Ctrl+Shift+1` matches on layouts
/// where Shift+1 produces `!` while symbol bindings such as `Prefix &` keep
/// priority. `0` is excluded so `Ctrl+Shift+0` never reaches the zoom reset.
fn pressed_key(keysym: Keysym, raw_code: u32, modifiers: Modifiers) -> Option<PressedKey> {
    let fallback = if modifiers.shift {
        number_row_digit(raw_code)
    } else {
        None
    };
    match key_identity(keysym) {
        Some(key) => Some(PressedKey {
            key,
            fallback: fallback.filter(|digit| *digit != key),
        }),
        None => fallback.map(PressedKey::from),
    }
}

fn key_identity(keysym: Keysym) -> Option<KeyIdentity> {
    let key = match keysym {
        Keysym::a | Keysym::A => KeyIdentity::Character('a'),
        Keysym::b | Keysym::B => KeyIdentity::Character('b'),
        Keysym::c | Keysym::C => KeyIdentity::Character('c'),
        Keysym::d | Keysym::D => KeyIdentity::Character('d'),
        Keysym::e | Keysym::E => KeyIdentity::Character('e'),
        Keysym::f | Keysym::F => KeyIdentity::Character('f'),
        Keysym::g | Keysym::G => KeyIdentity::Character('g'),
        Keysym::h | Keysym::H => KeyIdentity::Character('h'),
        Keysym::i | Keysym::I => KeyIdentity::Character('i'),
        Keysym::j | Keysym::J => KeyIdentity::Character('j'),
        Keysym::k | Keysym::K => KeyIdentity::Character('k'),
        Keysym::l | Keysym::L => KeyIdentity::Character('l'),
        Keysym::m | Keysym::M => KeyIdentity::Character('m'),
        Keysym::n | Keysym::N => KeyIdentity::Character('n'),
        Keysym::o | Keysym::O => KeyIdentity::Character('o'),
        Keysym::p | Keysym::P => KeyIdentity::Character('p'),
        Keysym::q | Keysym::Q => KeyIdentity::Character('q'),
        Keysym::r | Keysym::R => KeyIdentity::Character('r'),
        Keysym::s | Keysym::S => KeyIdentity::Character('s'),
        Keysym::t | Keysym::T => KeyIdentity::Character('t'),
        Keysym::u | Keysym::U => KeyIdentity::Character('u'),
        Keysym::v | Keysym::V => KeyIdentity::Character('v'),
        Keysym::w | Keysym::W => KeyIdentity::Character('w'),
        Keysym::x | Keysym::X => KeyIdentity::Character('x'),
        Keysym::y | Keysym::Y => KeyIdentity::Character('y'),
        Keysym::z | Keysym::Z => KeyIdentity::Character('z'),
        Keysym::Tab | Keysym::ISO_Left_Tab => KeyIdentity::Tab,
        Keysym::Return | Keysym::KP_Enter => KeyIdentity::Enter,
        Keysym::Escape => KeyIdentity::Escape,
        Keysym::space => KeyIdentity::Space,
        Keysym::slash | Keysym::question => KeyIdentity::Slash,
        Keysym::backslash | Keysym::bar => KeyIdentity::Backslash,
        Keysym::bracketleft | Keysym::braceleft => KeyIdentity::BracketLeft,
        Keysym::bracketright | Keysym::braceright => KeyIdentity::BracketRight,
        Keysym::Left => KeyIdentity::Left,
        Keysym::Right => KeyIdentity::Right,
        Keysym::Up => KeyIdentity::Up,
        Keysym::Down => KeyIdentity::Down,
        Keysym::Page_Up => KeyIdentity::PageUp,
        Keysym::Page_Down => KeyIdentity::PageDown,
        Keysym::End => KeyIdentity::End,
        Keysym::Insert | Keysym::KP_Insert => KeyIdentity::Insert,
        Keysym::plus | Keysym::KP_Add => KeyIdentity::Plus,
        Keysym::equal => KeyIdentity::Equal,
        Keysym::minus | Keysym::KP_Subtract => KeyIdentity::Minus,
        Keysym::_0 | Keysym::KP_0 => KeyIdentity::Zero,
        Keysym::_1 => KeyIdentity::One,
        Keysym::_2 => KeyIdentity::Two,
        Keysym::_3 => KeyIdentity::Three,
        Keysym::_4 => KeyIdentity::Four,
        Keysym::_5 => KeyIdentity::Five,
        Keysym::_6 => KeyIdentity::Six,
        Keysym::_7 => KeyIdentity::Seven,
        Keysym::_8 => KeyIdentity::Eight,
        Keysym::_9 => KeyIdentity::Nine,
        Keysym::ampersand => KeyIdentity::Ampersand,
        _ => return None,
    };
    Some(key)
}

/// Whether a resolved shortcut is consumed without effect while the Window
/// is paused for a daemon handoff (Plan 0061 M3.12a): it changes the session,
/// opens a prompt that only leads to such a change, attaches another Lair,
/// takes or grants control, or sends terminal input. Help, the palette,
/// pickers, history, search, focus, zoom, tab switching, copying, and
/// releases keep working; a terminal paste is dropped where it is delivered.
pub(in crate::wayland) const fn shortcut_paused_during_handoff(action: ActionId) -> bool {
    match action {
        ActionId::NewSession
        | ActionId::RenameCurrentTab
        | ActionId::NewDojo
        | ActionId::CloseCurrentTab
        | ActionId::CloseOtherTabs
        | ActionId::TerminateCurrentDojo
        | ActionId::RenameCurrentLair
        | ActionId::SaveCurrentLair
        | ActionId::ToggleCurrentLairPin
        | ActionId::RestoreCurrentLair
        | ActionId::TerminateCurrentLair
        | ActionId::PreviousLair
        | ActionId::NextLair
        | ActionId::SplitHorizontal
        | ActionId::SplitVertical
        | ActionId::CloseFocusedPane
        | ActionId::ResizePaneSmaller
        | ActionId::ResizePaneLarger
        | ActionId::ResizePaneLeftFive
        | ActionId::ResizePaneRightFive
        | ActionId::ResizePaneUpFive
        | ActionId::ResizePaneDownFive
        | ActionId::SendPrefix
        | ActionId::RequestControl
        | ActionId::ForceControl
        | ActionId::AcceptControlTransfer
        | ActionId::DenyControlTransfer
        | ActionId::ClipboardSaveImage => true,
        ActionId::CommandPalette
        | ActionId::RecentSessions
        | ActionId::PreviousDojo
        | ActionId::NextDojo
        | ActionId::DojoChooser
        | ActionId::SelectDojo1
        | ActionId::SelectDojo2
        | ActionId::SelectDojo3
        | ActionId::SelectDojo4
        | ActionId::SelectDojo5
        | ActionId::SelectDojo6
        | ActionId::SelectDojo7
        | ActionId::SelectDojo8
        | ActionId::SelectDojo9
        | ActionId::MoveDojoLeft
        | ActionId::MoveDojoRight
        | ActionId::PreviewCurrentLair
        | ActionId::LairChooser
        | ActionId::DetachWindow
        | ActionId::FocusLeft
        | ActionId::FocusRight
        | ActionId::FocusUp
        | ActionId::FocusDown
        | ActionId::TogglePaneZoom
        | ActionId::ToggleTabStrip
        | ActionId::BindingHelp
        | ActionId::CopyModeEnter
        | ActionId::ConfigReload
        | ActionId::SearchScrollback
        | ActionId::PageUp
        | ActionId::PageDown
        | ActionId::ReturnToLive
        | ActionId::ZoomIn
        | ActionId::ZoomOut
        | ActionId::ResetZoom
        | ActionId::ReleaseControl
        | ActionId::RevokeAllAccess
        | ActionId::ClipboardCopy
        | ActionId::ClipboardPaste => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_pause_consumes_session_changing_shortcuts_only() {
        for action in [
            ActionId::NewDojo,
            ActionId::CloseCurrentTab,
            ActionId::TerminateCurrentLair,
            ActionId::NextLair,
            ActionId::SplitVertical,
            ActionId::CloseFocusedPane,
            ActionId::ResizePaneUpFive,
            ActionId::SendPrefix,
            ActionId::RequestControl,
            ActionId::AcceptControlTransfer,
            ActionId::ClipboardSaveImage,
        ] {
            assert!(shortcut_paused_during_handoff(action), "{action:?}");
        }
        for action in [
            ActionId::CommandPalette,
            ActionId::BindingHelp,
            ActionId::NextDojo,
            ActionId::SelectDojo3,
            ActionId::MoveDojoLeft,
            ActionId::DojoChooser,
            ActionId::PreviewCurrentLair,
            ActionId::FocusLeft,
            ActionId::TogglePaneZoom,
            ActionId::SearchScrollback,
            ActionId::PageUp,
            ActionId::ReturnToLive,
            ActionId::ZoomIn,
            ActionId::DetachWindow,
            ActionId::ReleaseControl,
            ActionId::RevokeAllAccess,
            ActionId::ClipboardCopy,
        ] {
            assert!(!shortcut_paused_during_handoff(action), "{action:?}");
        }
    }

    fn action_for_key(
        keymap: &ResolvedKeymap,
        keysym: Keysym,
        raw_code: u32,
        modifiers: Modifiers,
    ) -> Option<ActionId> {
        let event = KeyEvent {
            time: 0,
            raw_code,
            keysym,
            utf8: None,
        };
        shortcut_action_for(keymap, &event, modifiers)
    }

    fn shortcut_action(keysym: Keysym, modifiers: Modifiers) -> Option<ActionId> {
        action_for_key(&ResolvedKeymap::default(), keysym, 0, modifiers)
    }

    fn ctrl_shift() -> Modifiers {
        Modifiers {
            ctrl: true,
            shift: true,
            ..Modifiers::default()
        }
    }

    /// Plan 0074: every built-in copy and paste chord owns its key from
    /// press to release in both profiles and in every Window, so its
    /// repeats, even after a modifier is let go, never reach the terminal.
    #[test]
    fn clipboard_chords_consume_their_key_until_release() {
        let logo = Modifiers {
            logo: true,
            ..Modifiers::default()
        };
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        let chords = [
            (Keysym::C, 46, ctrl_shift(), ActionId::ClipboardCopy),
            (Keysym::V, 47, ctrl_shift(), ActionId::ClipboardPaste),
            (Keysym::c, 46, logo, ActionId::ClipboardCopy),
            (Keysym::v, 47, logo, ActionId::ClipboardPaste),
            (Keysym::Insert, 110, ctrl, ActionId::ClipboardCopy),
            (Keysym::Insert, 110, shift, ActionId::ClipboardPaste),
        ];
        for profile in [
            crate::keymap::KeymapProfile::Splinterm,
            crate::keymap::KeymapProfile::OmarchyTmux,
        ] {
            let keymap = crate::keymap::built_in_keymap(profile);
            for (keysym, raw_code, modifiers, expected) in chords {
                let mut state = PrefixState::Idle;
                let KeymapPress::Consumed(Some(action)) = keymap_press_for(
                    &keymap,
                    &mut state,
                    keysym,
                    modifiers,
                    raw_code,
                    Instant::now(),
                    Duration::from_secs(1),
                ) else {
                    panic!("{keysym:?} {modifiers:?} is a clipboard chord");
                };
                assert_eq!(action, expected);
                assert!(clipboard_chord(Some(action)));
                for managed_tabs in [true, false] {
                    assert!(keymap_press_consumes_key(action, managed_tabs));
                }
                // A repeat resolves the chord again: still the Window's.
                assert!(clipboard_chord(action_for_key(
                    &keymap, keysym, raw_code, modifiers
                )));
            }
        }
        // Ctrl+C, Ctrl+V and plain Insert remain the program's keys.
        assert!(!clipboard_chord(shortcut_action(Keysym::c, ctrl)));
        assert!(!clipboard_chord(shortcut_action(Keysym::v, ctrl)));
        assert!(!clipboard_chord(shortcut_action(
            Keysym::Insert,
            Modifiers::default()
        )));
        assert!(!clipboard_chord(Some(ActionId::ZoomIn)));
    }

    /// Plan 0074: text fields take Omarchy's `Ctrl+A/X/C/V` (what its
    /// `Super+A/X/C/V` send) as well as the Super chords.
    #[test]
    fn text_fields_take_ctrl_and_super_edit_chords() {
        let with = |ctrl, shift, alt, logo| Modifiers {
            ctrl,
            shift,
            alt,
            logo,
            ..Modifiers::default()
        };
        let ctrl = with(true, false, false, false);
        let logo = with(false, false, false, true);
        for modifiers in [ctrl, logo] {
            assert_eq!(
                owned_field_edit(Keysym::a, modifiers),
                Some(FieldEdit::SelectAll)
            );
            assert_eq!(owned_field_edit(Keysym::x, modifiers), Some(FieldEdit::Cut));
            assert_eq!(
                owned_field_edit(Keysym::c, modifiers),
                Some(FieldEdit::Copy)
            );
            assert_eq!(
                owned_field_edit(Keysym::V, modifiers),
                Some(FieldEdit::Paste)
            );
            assert_eq!(owned_field_edit(Keysym::b, modifiers), None);
        }
        assert_eq!(owned_field_edit(Keysym::z, logo), Some(FieldEdit::Undo));
        assert_eq!(owned_field_edit(Keysym::z, ctrl), None);
        // Other chords are not field edits; Ctrl+Shift+C/V arrive as the
        // keymap's clipboard bindings instead.
        for modifiers in [
            Modifiers::default(),
            with(true, true, false, false),
            with(true, false, true, false),
            with(true, false, false, true),
            with(false, true, false, true),
        ] {
            assert_eq!(
                owned_field_edit(Keysym::c, modifiers),
                None,
                "{modifiers:?}"
            );
        }
        assert_eq!(
            owned_field_clipboard_action(&ResolvedKeymap::default(), Keysym::C, ctrl_shift()),
            Some(ActionId::ClipboardCopy)
        );
        assert_eq!(
            owned_field_clipboard_action(&ResolvedKeymap::default(), Keysym::V, ctrl_shift()),
            Some(ActionId::ClipboardPaste)
        );
    }

    #[test]
    fn detached_plain_enter_and_keypad_enter_are_consumed_through_release() {
        for keysym in [Keysym::Return, Keysym::KP_Enter] {
            let mut consumed = HashSet::new();
            assert_eq!(
                consume_detached_enter_press(keysym, Modifiers::default(), true, 28, &mut consumed),
                Some(super::super::HistoryNavigation::ReturnToLive)
            );
            assert!(
                consumed.contains(&28),
                "repeat remains consumed after return-live"
            );
            consumed.remove(&28);
            assert!(!consumed.contains(&28), "matching release clears the key");
            assert_eq!(
                consume_detached_enter_press(
                    keysym,
                    Modifiers::default(),
                    false,
                    28,
                    &mut consumed
                ),
                None,
                "the next live press retains terminal encoding"
            );
            assert!(!consumed.contains(&28));
            assert_eq!(
                consume_detached_enter_press(
                    keysym,
                    Modifiers {
                        shift: true,
                        ..Modifiers::default()
                    },
                    true,
                    28,
                    &mut consumed
                ),
                None
            );
        }
        assert_eq!(
            consume_detached_enter_press(
                Keysym::space,
                Modifiers::default(),
                true,
                57,
                &mut HashSet::new()
            ),
            None
        );
    }

    #[test]
    fn copy_mode_desktop_shortcuts_copy_or_consume_without_terminal_routes() {
        let desktop = Modifiers {
            logo: true,
            ..Modifiers::default()
        };
        assert_eq!(
            copy_mode_desktop_action(Keysym::c, desktop),
            Some(CopyModeDesktopAction::CopySelection)
        );
        for keysym in [Keysym::v, Keysym::x, Keysym::z] {
            assert_eq!(
                copy_mode_desktop_action(keysym, desktop),
                Some(CopyModeDesktopAction::Consume)
            );
        }
        assert_eq!(
            copy_mode_desktop_action(
                Keysym::c,
                Modifiers {
                    logo: true,
                    shift: true,
                    ..Modifiers::default()
                }
            ),
            None
        );
    }

    #[test]
    fn omarchy_universal_clipboard_translation_reaches_terminal_actions() {
        for profile in [
            crate::keymap::KeymapProfile::Splinterm,
            crate::keymap::KeymapProfile::OmarchyTmux,
        ] {
            let keymap = crate::keymap::built_in_keymap(profile);
            for logo in [false, true] {
                assert_eq!(
                    action_for_key(
                        &keymap,
                        Keysym::Insert,
                        0,
                        Modifiers {
                            ctrl: true,
                            logo,
                            ..Modifiers::default()
                        }
                    ),
                    Some(ActionId::ClipboardCopy)
                );
            }
            assert_eq!(
                action_for_key(
                    &keymap,
                    Keysym::Insert,
                    0,
                    Modifiers {
                        shift: true,
                        ..Modifiers::default()
                    }
                ),
                Some(ActionId::ClipboardPaste)
            );
        }
    }

    #[test]
    fn binding_help_consumes_escape_repeat_without_suppressing_editing_or_navigation() {
        assert!(binding_help_repeat_consumed(true, Keysym::Escape));
        for keysym in [Keysym::BackSpace, Keysym::Down, Keysym::a] {
            assert!(!binding_help_repeat_consumed(true, keysym));
        }
        assert!(!binding_help_repeat_consumed(false, Keysym::Escape));
    }

    #[test]
    fn inline_pickers_consume_tab_and_retry_repeats_but_not_typing() {
        for keysym in [Keysym::Tab, Keysym::ISO_Left_Tab] {
            assert!(picker_repeat_consumed(true, false, keysym));
            assert!(!picker_repeat_consumed(false, false, keysym));
        }
        assert!(picker_repeat_consumed(true, true, Keysym::r));
        assert!(picker_repeat_consumed(true, true, Keysym::R));
        // Typed into a list, a held R types r after r.
        assert!(!picker_repeat_consumed(true, false, Keysym::r));
        for keysym in [Keysym::BackSpace, Keysym::Down, Keysym::a] {
            assert!(!picker_repeat_consumed(true, true, keysym));
        }
    }

    #[test]
    fn owned_fields_accept_effective_clipboard_actions_without_other_shortcuts() {
        for profile in [
            crate::keymap::KeymapProfile::Splinterm,
            crate::keymap::KeymapProfile::OmarchyTmux,
        ] {
            let keymap = crate::keymap::built_in_keymap(profile);
            assert_eq!(
                owned_field_clipboard_action(&keymap, Keysym::c, ctrl_shift()),
                Some(ActionId::ClipboardCopy)
            );
            assert_eq!(
                owned_field_clipboard_action(&keymap, Keysym::v, ctrl_shift()),
                Some(ActionId::ClipboardPaste)
            );
            assert_eq!(
                owned_field_clipboard_action(&keymap, Keysym::f, ctrl_shift()),
                None
            );
        }
    }

    #[test]
    fn command_palette_shortcut_is_exact_managed_and_blocked_safely() {
        let action = shortcut_action(Keysym::p, ctrl_shift());
        assert_eq!(
            command_palette_shortcut_action(action, false, true, false),
            Some(CommandPaletteShortcutAction::Open)
        );
        assert_eq!(
            command_palette_shortcut_action(action, true, true, false),
            Some(CommandPaletteShortcutAction::Consume)
        );
        assert_eq!(
            command_palette_shortcut_action(action, false, true, true),
            Some(CommandPaletteShortcutAction::Consume)
        );
        assert_eq!(
            command_palette_shortcut_action(action, false, false, false),
            None
        );
        assert_eq!(shortcut_action(Keysym::p, Modifiers::default()), None);
    }

    #[test]
    fn session_picker_shortcut_is_exact_and_application_owned() {
        let action = shortcut_action(Keysym::s, ctrl_shift());
        assert_eq!(
            session_picker_shortcut_action(action, false, false),
            Some(SessionPickerShortcutAction::Request)
        );
        assert_eq!(
            session_picker_shortcut_action(action, true, false),
            Some(SessionPickerShortcutAction::Consume)
        );
        assert_eq!(
            session_picker_shortcut_action(action, false, true),
            Some(SessionPickerShortcutAction::Consume)
        );
        assert_eq!(shortcut_action(Keysym::s, Modifiers::default()), None);
        assert_eq!(
            shortcut_action(
                Keysym::s,
                Modifiers {
                    alt: true,
                    ..ctrl_shift()
                }
            ),
            None
        );
    }

    #[test]
    fn tab_shortcuts_are_exact_managed_and_repeat_consumed() {
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        assert_eq!(
            tab_shortcut_action(shortcut_action(Keysym::Tab, ctrl), false, true),
            Some(TabShortcutAction::Next)
        );
        assert_eq!(
            tab_shortcut_action(
                shortcut_action(Keysym::ISO_Left_Tab, ctrl_shift()),
                false,
                true,
            ),
            Some(TabShortcutAction::Previous)
        );
        assert_eq!(
            tab_shortcut_action(shortcut_action(Keysym::d, ctrl_shift()), false, true),
            Some(TabShortcutAction::NewDojo)
        );
        assert_eq!(
            tab_shortcut_action(shortcut_action(Keysym::q, ctrl_shift()), false, true),
            Some(TabShortcutAction::Close)
        );
        assert_eq!(
            tab_shortcut_action(Some(ActionId::CloseOtherTabs), false, true),
            Some(TabShortcutAction::CloseOthers)
        );
        assert_eq!(
            tab_shortcut_action(Some(ActionId::CloseOtherTabs), true, true),
            Some(TabShortcutAction::Consume)
        );
        assert_eq!(
            tab_shortcut_action(shortcut_action(Keysym::Tab, ctrl), true, true),
            Some(TabShortcutAction::Consume)
        );
        assert_eq!(
            tab_shortcut_action(shortcut_action(Keysym::Tab, ctrl), false, false),
            None
        );
        assert!(tab_action_dispatch_allowed([false; 5]));
        for blocked in 0..5 {
            let mut states = [false; 5];
            states[blocked] = true;
            assert!(!tab_action_dispatch_allowed(states));
        }
    }

    #[test]
    fn omarchy_numeric_and_reorder_bindings_dispatch_typed_tab_actions() {
        let keymap = crate::keymap::built_in_keymap(crate::keymap::KeymapProfile::OmarchyTmux);
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        let alt_shift = Modifiers {
            alt: true,
            shift: true,
            ..Modifiers::default()
        };
        assert_eq!(
            tab_shortcut_action(action_for_key(&keymap, Keysym::_1, 0, alt), false, true,),
            Some(TabShortcutAction::Select(0))
        );
        assert_eq!(
            tab_shortcut_action(
                action_for_key(&keymap, Keysym::Right, 0, alt_shift),
                false,
                true,
            ),
            Some(TabShortcutAction::Move(1))
        );
        assert_eq!(
            action_for_key(&keymap, Keysym::Down, 0, alt),
            Some(ActionId::NextLair)
        );
    }

    #[test]
    fn every_bindable_action_has_an_explicit_runtime_route() {
        for action in ActionId::BINDABLE {
            let route = match action {
                ActionId::CommandPalette => "command palette boundary",
                ActionId::RecentSessions => "Dojo picker boundary",
                ActionId::NewSession
                | ActionId::RenameCurrentLair
                | ActionId::SaveCurrentLair
                | ActionId::ToggleCurrentLairPin
                | ActionId::PreviewCurrentLair
                | ActionId::RestoreCurrentLair
                | ActionId::TerminateCurrentLair
                | ActionId::PreviousLair
                | ActionId::NextLair
                | ActionId::LairChooser => "Lair topology route",
                ActionId::RenameCurrentTab
                | ActionId::NewDojo
                | ActionId::PreviousDojo
                | ActionId::NextDojo
                | ActionId::CloseCurrentTab
                | ActionId::CloseOtherTabs
                | ActionId::TerminateCurrentDojo
                | ActionId::DojoChooser
                | ActionId::SelectDojo1
                | ActionId::SelectDojo2
                | ActionId::SelectDojo3
                | ActionId::SelectDojo4
                | ActionId::SelectDojo5
                | ActionId::SelectDojo6
                | ActionId::SelectDojo7
                | ActionId::SelectDojo8
                | ActionId::SelectDojo9
                | ActionId::MoveDojoLeft
                | ActionId::MoveDojoRight => "Window-local Dojo route",
                ActionId::SplitHorizontal
                | ActionId::SplitVertical
                | ActionId::CloseFocusedPane
                | ActionId::ResizePaneSmaller
                | ActionId::ResizePaneLarger
                | ActionId::ResizePaneLeftFive
                | ActionId::ResizePaneRightFive
                | ActionId::ResizePaneUpFive
                | ActionId::ResizePaneDownFive => "pane topology route",
                ActionId::FocusLeft
                | ActionId::FocusRight
                | ActionId::FocusUp
                | ActionId::FocusDown => "pane focus route",
                ActionId::ZoomIn | ActionId::ZoomOut | ActionId::ResetZoom => "font zoom route",
                ActionId::DetachWindow
                | ActionId::TogglePaneZoom
                | ActionId::ToggleTabStrip
                | ActionId::BindingHelp
                | ActionId::CopyModeEnter
                | ActionId::ConfigReload
                | ActionId::SendPrefix => "typed local application route",
                ActionId::SearchScrollback
                | ActionId::PageUp
                | ActionId::PageDown
                | ActionId::ReturnToLive => "history route",
                ActionId::RequestControl
                | ActionId::ReleaseControl
                | ActionId::ForceControl
                | ActionId::RevokeAllAccess
                | ActionId::AcceptControlTransfer
                | ActionId::DenyControlTransfer => "authority route",
                ActionId::ClipboardCopy
                | ActionId::ClipboardPaste
                | ActionId::ClipboardSaveImage => "clipboard route",
            };
            assert!(!route.is_empty(), "{}", action.config_name());
        }
    }

    #[test]
    fn named_split_actions_follow_placement_axes() {
        for (action, name, axis) in [
            (
                ActionId::SplitHorizontal,
                "pane.split-below",
                splinterm_core::Axis::Vertical,
            ),
            (
                ActionId::SplitVertical,
                "pane.split-right",
                splinterm_core::Axis::Horizontal,
            ),
        ] {
            assert_eq!(action.config_name(), name);
            assert_eq!(
                pane_topology_action(Some(action)),
                Some(PaneTopologyAction::Split(axis))
            );
        }
    }

    #[test]
    fn pane_bindings_are_typed_and_do_not_capture_plain_keys() {
        assert_eq!(
            pane_focus_action(shortcut_action(Keysym::Left, ctrl_shift())),
            Some(PaneFocusAction::Direction(FocusDirection::Left))
        );
        assert_eq!(
            pane_focus_action(shortcut_action(Keysym::Left, Modifiers::default())),
            None
        );
        assert_eq!(
            pane_topology_action(shortcut_action(Keysym::Return, ctrl_shift())),
            Some(PaneTopologyAction::Split(splinterm_core::Axis::Vertical))
        );
        assert_eq!(
            pane_topology_action(shortcut_action(Keysym::bar, ctrl_shift())),
            Some(PaneTopologyAction::Split(splinterm_core::Axis::Horizontal))
        );
        assert_eq!(
            pane_topology_action(shortcut_action(Keysym::W, ctrl_shift())),
            Some(PaneTopologyAction::Close)
        );
        assert_eq!(
            pane_topology_action(shortcut_action(Keysym::braceleft, ctrl_shift())),
            Some(PaneTopologyAction::AdjustRatio(-50))
        );
    }

    #[test]
    fn font_zoom_preserves_foot_compatible_shift_and_keypad_aliases() {
        let control = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        assert_eq!(
            font_zoom_action(shortcut_action(Keysym::plus, control)),
            Some(FontZoomAction::Increase)
        );
        assert_eq!(
            font_zoom_action(shortcut_action(Keysym::equal, control)),
            Some(FontZoomAction::Increase)
        );
        assert_eq!(
            font_zoom_action(shortcut_action(Keysym::KP_Add, control)),
            Some(FontZoomAction::Increase)
        );
        assert_eq!(
            font_zoom_action(shortcut_action(Keysym::minus, control)),
            Some(FontZoomAction::Decrease)
        );
        assert_eq!(
            font_zoom_action(shortcut_action(Keysym::_0, control)),
            Some(FontZoomAction::Reset)
        );
        assert_eq!(shortcut_action(Keysym::plus, Modifiers::default()), None);
    }

    #[test]
    fn resolved_overlay_changes_dispatch_without_bypassing_the_adapter() {
        let resolved = crate::keymap::resolve_keymap_text(
            crate::keymap::KeymapProfile::Splinterm,
            r#"
version = 1
[[unbind]]
sequence = ["Ctrl+Shift+P"]
[[binding]]
sequence = ["Ctrl+Alt+P"]
action = "app.command-palette"
"#,
            std::path::Path::new("keybindings.toml"),
        )
        .unwrap();
        assert_eq!(
            action_for_key(
                &resolved.keymap,
                Keysym::p,
                0,
                Modifiers {
                    ctrl: true,
                    alt: true,
                    ..Modifiers::default()
                }
            ),
            Some(ActionId::CommandPalette)
        );
        assert_eq!(
            action_for_key(&resolved.keymap, Keysym::p, 0, ctrl_shift()),
            None
        );
    }

    #[test]
    fn strict_prefix_overlay_reaches_the_same_typed_runtime_route() {
        let resolved = crate::keymap::resolve_keymap_text(
            crate::keymap::KeymapProfile::OmarchyTmux,
            r#"
version = 1
inherits = "omarchy-tmux"
[[binding]]
sequence = ["Prefix", "o"]
action = "dojo.close-other-tabs"
"#,
            std::path::Path::new("keybindings.toml"),
        )
        .unwrap();
        let mut state = PrefixState::Idle;
        let now = Instant::now();
        let control = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        assert_eq!(
            keymap_press_for(
                &resolved.keymap,
                &mut state,
                Keysym::space,
                control,
                1,
                now,
                Duration::from_secs(1),
            ),
            KeymapPress::Consumed(None)
        );
        let action = match keymap_press_for(
            &resolved.keymap,
            &mut state,
            Keysym::o,
            Modifiers::default(),
            2,
            now,
            Duration::from_secs(1),
        ) {
            KeymapPress::Consumed(action) => action,
            other => panic!("custom prefix did not dispatch: {other:?}"),
        };
        assert_eq!(action, Some(ActionId::CloseOtherTabs));
        assert_eq!(
            tab_shortcut_action(action, false, true),
            Some(TabShortcutAction::CloseOthers)
        );
    }

    #[test]
    fn omarchy_prefix_resolution_uses_xkb_keys_and_consumes_unknown_seconds() {
        let keymap = crate::keymap::built_in_keymap(crate::keymap::KeymapProfile::OmarchyTmux);
        let mut state = PrefixState::Idle;
        let control = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        let now = Instant::now();
        assert_eq!(
            keymap_press_for(
                &keymap,
                &mut state,
                Keysym::space,
                control,
                1,
                now,
                Duration::from_secs(1),
            ),
            KeymapPress::Consumed(None)
        );
        assert_eq!(
            keymap_press_for(
                &keymap,
                &mut state,
                Keysym::Shift_L,
                Modifiers::default(),
                2,
                now,
                Duration::from_secs(1),
            ),
            KeymapPress::PrefixModifier
        );
        assert!(state.is_armed());
        assert_eq!(
            keymap_press_for(
                &keymap,
                &mut state,
                Keysym::question,
                Modifiers {
                    shift: true,
                    ..Modifiers::default()
                },
                5,
                now,
                Duration::from_secs(1),
            ),
            KeymapPress::Consumed(Some(ActionId::BindingHelp))
        );
        let _ = keymap_press_for(
            &keymap,
            &mut state,
            Keysym::b,
            control,
            3,
            now,
            Duration::from_secs(1),
        );
        assert_eq!(
            keymap_press_for(
                &keymap,
                &mut state,
                Keysym::F1,
                Modifiers::default(),
                4,
                now,
                Duration::from_secs(1),
            ),
            KeymapPress::Consumed(None)
        );
    }

    #[test]
    fn legacy_permissive_modifier_edges_remain_explicit() {
        let ctrl_shift_alt = Modifiers {
            ctrl: true,
            shift: true,
            alt: true,
            ..Modifiers::default()
        };
        assert_eq!(
            shortcut_action(Keysym::c, ctrl_shift_alt),
            Some(ActionId::ClipboardCopy)
        );
        assert_eq!(
            shortcut_action(
                Keysym::Page_Up,
                Modifiers {
                    shift: true,
                    logo: true,
                    ..Modifiers::default()
                }
            ),
            Some(ActionId::PageUp)
        );
        assert_eq!(shortcut_action(Keysym::p, ctrl_shift_alt), None);
    }

    // Evdev codes for the physical number-row keys used below.
    const KEY_1: u32 = 2;
    const KEY_6: u32 = 7;
    const KEY_7: u32 = 8;
    const KEY_9: u32 = 10;
    const KEY_0: u32 = 11;

    #[test]
    fn ctrl_shift_number_row_selects_dojos_whatever_symbol_shift_produces() {
        let keymap = ResolvedKeymap::default();
        for (keysym, raw_code, expected) in [
            // US: Shift+1 produces `!`.
            (Keysym::exclam, KEY_1, ActionId::SelectDojo1),
            // AZERTY: Shift on the number row produces the digit itself.
            (Keysym::_1, KEY_1, ActionId::SelectDojo1),
            // German: Shift+6 produces `&` and Shift+7 produces `/`.
            (Keysym::ampersand, KEY_6, ActionId::SelectDojo6),
            (Keysym::slash, KEY_7, ActionId::SelectDojo7),
            (Keysym::parenleft, KEY_9, ActionId::SelectDojo9),
        ] {
            let action = action_for_key(&keymap, keysym, raw_code, ctrl_shift());
            assert_eq!(action, Some(expected), "{keysym:?} on raw code {raw_code}");
            let mut state = PrefixState::Idle;
            assert_eq!(
                keymap_press_for(
                    &keymap,
                    &mut state,
                    keysym,
                    ctrl_shift(),
                    raw_code,
                    Instant::now(),
                    Duration::from_secs(1),
                ),
                KeymapPress::Consumed(Some(expected)),
                "press and repeat resolve {keysym:?} the same way"
            );
        }
        assert_eq!(
            tab_shortcut_action(
                action_for_key(&keymap, Keysym::exclam, KEY_1, ctrl_shift()),
                false,
                true,
            ),
            Some(TabShortcutAction::Select(0))
        );
    }

    #[test]
    fn number_row_fallback_needs_shift_and_a_digit_key_from_one_to_nine() {
        let keymap = ResolvedKeymap::default();
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        assert_eq!(action_for_key(&keymap, Keysym::exclam, KEY_1, ctrl), None);
        assert_eq!(action_for_key(&keymap, Keysym::_1, KEY_1, ctrl), None);
        assert_eq!(
            action_for_key(&keymap, Keysym::parenright, KEY_0, ctrl_shift()),
            None,
            "Ctrl+Shift+0 does not reach the zoom reset"
        );
        for raw_code in [0, 1, KEY_0, 12, 30] {
            assert_eq!(
                action_for_key(&keymap, Keysym::exclam, raw_code, ctrl_shift()),
                None,
                "raw code {raw_code} is not a 1-9 number-row key"
            );
        }
        let mut state = PrefixState::Idle;
        assert_eq!(
            keymap_press_for(
                &keymap,
                &mut state,
                Keysym::exclam,
                Modifiers {
                    shift: true,
                    ..Modifiers::default()
                },
                KEY_1,
                Instant::now(),
                Duration::from_secs(1),
            ),
            KeymapPress::PassThrough,
            "Shift+1 alone still types"
        );
    }

    #[test]
    fn built_in_symbol_bindings_win_over_the_number_row_fallback() {
        let keymap = ResolvedKeymap::default();
        // Swiss Shift+1 produces `+`, which the any-Shift zoom-in binding owns.
        assert_eq!(
            action_for_key(&keymap, Keysym::plus, KEY_1, ctrl_shift()),
            Some(ActionId::ZoomIn)
        );
        // Hungarian Shift+7 produces `=`, the other zoom-in key.
        assert_eq!(
            action_for_key(&keymap, Keysym::equal, KEY_7, ctrl_shift()),
            Some(ActionId::ZoomIn)
        );
    }

    #[test]
    fn shifted_symbol_bindings_win_over_the_number_row_fallback() {
        let resolved = crate::keymap::resolve_keymap_text(
            crate::keymap::KeymapProfile::Splinterm,
            r#"
version = 1
inherits = "splinterm"
[[binding]]
sequence = ["Ctrl+Shift+&"]
action = "app.command-palette"
"#,
            std::path::Path::new("keybindings.toml"),
        )
        .unwrap();
        assert_eq!(
            action_for_key(&resolved.keymap, Keysym::ampersand, KEY_7, ctrl_shift()),
            Some(ActionId::CommandPalette)
        );
        assert_eq!(
            action_for_key(&resolved.keymap, Keysym::exclam, KEY_1, ctrl_shift()),
            Some(ActionId::SelectDojo1)
        );

        let keymap = crate::keymap::built_in_keymap(crate::keymap::KeymapProfile::OmarchyTmux);
        let mut state = PrefixState::Idle;
        let now = Instant::now();
        let _ = keymap_press_for(
            &keymap,
            &mut state,
            Keysym::space,
            Modifiers {
                ctrl: true,
                ..Modifiers::default()
            },
            57,
            now,
            Duration::from_secs(1),
        );
        assert!(state.is_armed());
        assert_eq!(
            keymap_press_for(
                &keymap,
                &mut state,
                Keysym::ampersand,
                Modifiers {
                    shift: true,
                    ..Modifiers::default()
                },
                KEY_7,
                now,
                Duration::from_secs(1),
            ),
            KeymapPress::Consumed(Some(ActionId::TerminateCurrentDojo)),
            "US Prefix & keeps its shifted-symbol binding"
        );
    }

    #[test]
    fn tab_chords_outside_managed_tabs_leave_their_repeats_to_the_terminal() {
        let keymap = ResolvedKeymap::default();
        let mut state = PrefixState::Idle;
        let KeymapPress::Consumed(Some(action)) = keymap_press_for(
            &keymap,
            &mut state,
            Keysym::exclam,
            ctrl_shift(),
            KEY_1,
            Instant::now(),
            Duration::from_secs(1),
        ) else {
            panic!("Ctrl+Shift+1 resolves through the keymap");
        };
        assert_eq!(action, ActionId::SelectDojo1);
        assert!(
            keymap_press_consumes_key(action, true),
            "a managed-tab window owns the chord and drops its repeats"
        );
        assert!(
            !keymap_press_consumes_key(action, false),
            "a direct single-Splint window forwards the chord, so it keeps repeating"
        );
        assert_eq!(tab_shortcut_action(Some(action), true, false), None);
        for action in [
            ActionId::NextDojo,
            ActionId::NewDojo,
            ActionId::DojoChooser,
            ActionId::CommandPalette,
        ] {
            assert!(keymap_press_consumes_key(action, true), "{action:?}");
            assert!(!keymap_press_consumes_key(action, false), "{action:?}");
        }
        for action in [ActionId::ZoomIn, ActionId::ZoomOut] {
            assert!(!keymap_press_consumes_key(action, true), "{action:?}");
            assert!(!keymap_press_consumes_key(action, false), "{action:?}");
        }
        for action in [
            ActionId::BindingHelp,
            ActionId::NextLair,
            ActionId::SplitHorizontal,
        ] {
            assert!(keymap_press_consumes_key(action, false), "{action:?}");
        }
    }
}
