//! Pure pointer, wheel, history, modal-input, and mouse-report reduction.

use std::collections::HashMap;

use smithay_client_toolkit::seat::{
    keyboard::Modifiers,
    pointer::{BTN_LEFT, BTN_MIDDLE},
};
use splinterm_core::SplintId;
use splinterm_protocol::{MouseTracking, TerminalInputModes, TerminalSnapshot};

use crate::{
    frontend::PickerHitTarget,
    renderer::{HistoryOverlayLayout, HistoryOverlayStatus},
    viewport::ScrollbackViewport,
};

use super::super::CellPosition;

const BTN_RIGHT: u32 = 0x111;
const WHEEL_VALUE120_STEP: f64 = 120.0;

#[derive(Clone, Copy, Debug)]
pub(in crate::wayland) enum PressOwner {
    Application {
        code: u8,
        tracking: MouseTracking,
        sgr: bool,
        modifiers: Modifiers,
    },
    Selection,
    SelectionPending {
        anchor: CellPosition,
    },
    PrimaryPaste,
    Url,
    Ignored,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WheelUnit {
    Value120,
    Discrete,
    Pixel,
}

#[derive(Debug, Default)]
pub(in crate::wayland) struct WheelAccumulator {
    unit: Option<WheelUnit>,
    remainder: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::wayland) enum MouseAction {
    Press(u8),
    Release(u8),
    Motion(u8),
    WheelUp,
    WheelDown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::wayland) enum WheelOutcome {
    Noop,
    History { before: usize, after: usize },
    Application { reports: usize, bytes: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::wayland) enum HistoryNavigation {
    PageUp,
    PageDown,
    ReturnToLive,
}

pub(in crate::wayland) fn history_overlay_status(
    viewport: &ScrollbackViewport,
    snapshot: Option<&TerminalSnapshot>,
) -> Option<HistoryOverlayStatus> {
    let snapshot = snapshot?;
    (!viewport.is_live()).then_some(HistoryOverlayStatus {
        offset_from_bottom: viewport.offset_from_bottom().min(999),
        available_rows: snapshot.available_scrollback_rows.min(999),
        unseen_rows: viewport.unseen_rows().min(999),
    })
}

pub(in crate::wayland) fn pointer_axis_focus_target(
    has_vertical_axis: bool,
    pointer_grab_active: bool,
    pane_under_pointer: Option<SplintId>,
    focused_pane: Option<SplintId>,
) -> Option<SplintId> {
    (has_vertical_axis && !pointer_grab_active)
        .then_some(pane_under_pointer)
        .flatten()
        .filter(|target| Some(*target) != focused_pane)
}

/// Whether a press at `position` (logical px) is on the history strip the
/// last draw painted (`painted`, buffer px at `scale_120`) while the view
/// is `detached` from live: anywhere on the strip returns to live, so a
/// click on it never starts a selection in the terminal it hides.
pub(in crate::wayland) fn history_return_to_live_hit(
    position: (f64, f64),
    painted: Option<HistoryOverlayLayout>,
    scale_120: u32,
    detached: bool,
) -> bool {
    if !detached || !position.0.is_finite() || !position.1.is_finite() || scale_120 == 0 {
        return false;
    }
    let Some(layout) = painted else {
        return false;
    };
    let scale = f64::from(scale_120) / 120.0;
    let (x, y) = (position.0 * scale, position.1 * scale);
    let (left, top, width, height) = layout.panel;
    let (left, top) = (f64::from(left), f64::from(top));
    x >= left && y >= top && x < left + f64::from(width) && y < top + f64::from(height)
}

fn mouse_button_code(button: u32) -> Option<u8> {
    match button {
        BTN_LEFT => Some(0),
        BTN_MIDDLE => Some(1),
        BTN_RIGHT => Some(2),
        _ => None,
    }
}

pub(in crate::wayland) fn classify_press(
    button: u32,
    has_position: bool,
    modifiers: Modifiers,
    modes: TerminalInputModes,
    has_hovered_url: bool,
) -> PressOwner {
    if button == BTN_MIDDLE {
        return PressOwner::PrimaryPaste;
    }
    if button == BTN_LEFT && modifiers.ctrl && has_hovered_url {
        return PressOwner::Url;
    }
    if has_position && modes.mouse_tracking != MouseTracking::None && !modifiers.shift {
        return mouse_button_code(button).map_or(PressOwner::Ignored, |code| {
            PressOwner::Application {
                code,
                tracking: modes.mouse_tracking,
                sgr: modes.sgr_mouse,
                modifiers,
            }
        });
    }
    if button == BTN_LEFT && has_position {
        PressOwner::Selection
    } else {
        PressOwner::Ignored
    }
}

pub(in crate::wayland) const fn local_selection_owner(
    has_selection: bool,
    anchor: CellPosition,
) -> PressOwner {
    if has_selection {
        PressOwner::SelectionPending { anchor }
    } else {
        PressOwner::Selection
    }
}

pub(in crate::wayland) fn pending_selection_drag_anchor(
    owner: &PressOwner,
    position: CellPosition,
) -> Option<CellPosition> {
    match owner {
        PressOwner::SelectionPending { anchor } if *anchor != position => Some(*anchor),
        _ => None,
    }
}

pub(in crate::wayland) fn take_press_owner(
    pressed: &mut HashMap<u32, PressOwner>,
    button: u32,
) -> PressOwner {
    pressed.remove(&button).unwrap_or(PressOwner::Ignored)
}

pub(in crate::wayland) fn picker_release_activation(
    pressed: Option<PickerHitTarget>,
    released: Option<PickerHitTarget>,
) -> Option<PickerHitTarget> {
    pressed.filter(|target| Some(*target) == released)
}

pub(in crate::wayland) const fn clipboard_read_is_current(
    input_blocked: bool,
    current_generation: u64,
    read_generation: u64,
) -> bool {
    !input_blocked && current_generation == read_generation
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::wayland) struct ModalPointerFrame {
    owned_at_start: bool,
}

impl ModalPointerFrame {
    pub(in crate::wayland) const fn new(owned_at_start: bool) -> Self {
        Self { owned_at_start }
    }

    pub(in crate::wayland) const fn owns_event(self, action_surface_open: bool) -> bool {
        self.owned_at_start || action_surface_open
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::wayland) enum PickerImeReconcile {
    None,
    Renew,
    Enable,
}

pub(in crate::wayland) const fn picker_ime_reconcile(
    modal_barrier: bool,
    keyboard_focused: bool,
    ime_entered: bool,
) -> PickerImeReconcile {
    if modal_barrier {
        PickerImeReconcile::Renew
    } else if keyboard_focused && ime_entered {
        PickerImeReconcile::Enable
    } else {
        PickerImeReconcile::None
    }
}

pub(in crate::wayland) fn reconciled_focus_report(
    focus_reporting: bool,
    reported_focused: bool,
    current_focused: bool,
) -> Option<Vec<u8>> {
    if !focus_reporting || reported_focused == current_focused {
        None
    } else if current_focused {
        Some(b"\x1b[I".to_vec())
    } else {
        Some(b"\x1b[O".to_vec())
    }
}

pub(in crate::wayland) fn application_motion(owner: &PressOwner) -> Option<(u8, bool, Modifiers)> {
    if let PressOwner::Application {
        code,
        tracking: MouseTracking::Button | MouseTracking::Any,
        sgr,
        modifiers,
    } = owner
    {
        Some((*code, *sgr, *modifiers))
    } else {
        None
    }
}

impl WheelAccumulator {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "Wayland axis values are finite and converted to a whole-line count"
    )]
    pub(in crate::wayland) fn push(
        &mut self,
        absolute: f64,
        discrete: i32,
        value120: i32,
        cell_height: u32,
    ) -> Option<(MouseAction, usize)> {
        self.push_scaled(absolute, discrete, value120, 1.0, cell_height)
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "Wayland axis values are finite and converted to a whole-line count"
    )]
    pub(in crate::wayland) fn push_scaled(
        &mut self,
        absolute: f64,
        discrete: i32,
        value120: i32,
        multiplier: f64,
        cell_height: u32,
    ) -> Option<(MouseAction, usize)> {
        let multiplier = if multiplier.is_finite() && multiplier > 0.0 {
            multiplier
        } else {
            1.0
        };
        let (unit, delta, threshold) = if value120 != 0 {
            (
                WheelUnit::Value120,
                f64::from(value120),
                WHEEL_VALUE120_STEP / multiplier,
            )
        } else if discrete != 0 {
            (WheelUnit::Discrete, f64::from(discrete) * multiplier, 1.0)
        } else if absolute != 0.0 && absolute.is_finite() && cell_height > 0 {
            (
                WheelUnit::Pixel,
                absolute,
                f64::from(cell_height) / multiplier,
            )
        } else {
            return None;
        };
        if self.unit != Some(unit) {
            self.unit = Some(unit);
            self.remainder = 0.0;
        }
        self.remainder += delta;
        let count = (self.remainder.abs() / threshold).floor() as usize;
        if count == 0 {
            return None;
        }
        let direction = self.remainder.signum();
        self.remainder -= direction * threshold * count as f64;
        Some((
            if direction.is_sign_negative() {
                MouseAction::WheelUp
            } else {
                MouseAction::WheelDown
            },
            count,
        ))
    }
}

pub(in crate::wayland) fn mouse_report(
    action: MouseAction,
    position: CellPosition,
    modifiers: Modifiers,
    sgr: bool,
) -> Option<Vec<u8>> {
    let modifier =
        4 * u8::from(modifiers.shift) + 8 * u8::from(modifiers.alt) + 16 * u8::from(modifiers.ctrl);
    let (base, release) = match action {
        MouseAction::Press(button) => (button, false),
        MouseAction::Release(button) => (button, true),
        MouseAction::Motion(button) => (button.saturating_add(32), false),
        MouseAction::WheelUp => (64, false),
        MouseAction::WheelDown => (65, false),
    };
    let code = base.saturating_add(modifier);
    let column = position.column.saturating_add(1);
    let row = position.row.saturating_add(1);
    if sgr {
        Some(
            format!(
                "\x1b[<{code};{column};{row}{}",
                if release { 'm' } else { 'M' }
            )
            .into_bytes(),
        )
    } else {
        let legacy_code = if release {
            3_u8.saturating_add(modifier)
        } else {
            code
        };
        let column = u8::try_from(column.saturating_add(32)).ok()?;
        let row = u8::try_from(row.saturating_add(32)).ok()?;
        Some(vec![
            0x1b,
            b'[',
            b'M',
            legacy_code.saturating_add(32),
            column,
            row,
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use splinterm_core::SplintId;

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

    #[test]
    fn vertical_axis_targets_the_pane_under_the_pointer() {
        let left = SplintId::new();
        let right = SplintId::new();

        assert_eq!(
            pointer_axis_focus_target(true, false, Some(right), Some(left)),
            Some(right)
        );
        assert_eq!(
            pointer_axis_focus_target(true, false, Some(right), Some(right)),
            None
        );
        assert_eq!(
            pointer_axis_focus_target(false, false, Some(right), Some(left)),
            None
        );
        assert_eq!(
            pointer_axis_focus_target(true, true, Some(right), Some(left)),
            None,
            "an active pointer grab must retain pane ownership"
        );
        assert_eq!(
            pointer_axis_focus_target(true, false, None, Some(left)),
            None
        );
    }

    #[test]
    fn trusted_history_return_target_is_the_painted_strip_half_open_and_detached_only() {
        let painted = Some(HistoryOverlayLayout {
            panel: (600, 8, 300, 68),
            return_to_live: (820, 8, 80, 68),
        });
        // The whole strip, not only its chips.
        let inside = (601.0, 9.0);
        assert!(history_return_to_live_hit(inside, painted, 120, true));
        assert!(!history_return_to_live_hit(inside, painted, 120, false));
        assert!(!history_return_to_live_hit(inside, None, 120, true));
        assert!(!history_return_to_live_hit(
            (900.0, 9.0),
            painted,
            120,
            true
        ));
        assert!(!history_return_to_live_hit(
            (601.0, 76.0),
            painted,
            120,
            true
        ));
        assert!(!history_return_to_live_hit(
            (599.0, 9.0),
            painted,
            120,
            true
        ));
        // Logical positions map to buffer px at the Window's scale.
        assert!(history_return_to_live_hit((401.0, 6.0), painted, 180, true));
        assert!(!history_return_to_live_hit(
            (399.0, 6.0),
            painted,
            180,
            true
        ));
        assert!(!history_return_to_live_hit(
            (f64::NAN, 9.0),
            painted,
            120,
            true
        ));
    }

    #[test]
    fn press_ownership_pairs_application_and_local_releases() {
        let position_present = true;
        let mut modes = normal_modes();
        modes.mouse_tracking = MouseTracking::Normal;
        let app = classify_press(
            BTN_LEFT,
            position_present,
            Modifiers::default(),
            modes,
            false,
        );
        assert!(matches!(
            app,
            PressOwner::Application {
                code: 0,
                tracking: MouseTracking::Normal,
                ..
            }
        ));
        assert!(application_motion(&app).is_none());
        modes.mouse_tracking = MouseTracking::Button;
        let button_motion = classify_press(
            BTN_LEFT,
            position_present,
            Modifiers::default(),
            modes,
            false,
        );
        assert!(application_motion(&button_motion).is_some());
        let primary = classify_press(
            BTN_MIDDLE,
            position_present,
            Modifiers::default(),
            modes,
            false,
        );
        assert!(matches!(primary, PressOwner::PrimaryPaste));
        let url = classify_press(
            BTN_LEFT,
            position_present,
            Modifiers {
                ctrl: true,
                ..Modifiers::default()
            },
            modes,
            true,
        );
        assert!(matches!(url, PressOwner::Url));

        let mut pressed = HashMap::from([(BTN_MIDDLE, primary), (BTN_LEFT, url)]);
        assert!(matches!(
            take_press_owner(&mut pressed, BTN_MIDDLE),
            PressOwner::PrimaryPaste
        ));
        assert!(matches!(
            take_press_owner(&mut pressed, BTN_LEFT),
            PressOwner::Url
        ));
        assert!(matches!(
            take_press_owner(&mut pressed, BTN_RIGHT),
            PressOwner::Ignored
        ));
    }

    #[test]
    fn existing_selection_click_clears_until_the_pointer_drags_to_another_cell() {
        let anchor = CellPosition { row: 3, column: 4 };
        let other = CellPosition { row: 3, column: 5 };

        assert!(matches!(
            local_selection_owner(false, anchor),
            PressOwner::Selection
        ));
        let pending = local_selection_owner(true, anchor);
        assert!(matches!(
            pending,
            PressOwner::SelectionPending { anchor: saved } if saved == anchor
        ));
        assert_eq!(pending_selection_drag_anchor(&pending, anchor), None);
        assert_eq!(pending_selection_drag_anchor(&pending, other), Some(anchor));
    }

    #[test]
    fn modal_pointer_frame_keeps_paired_release_owned_after_dismissal() {
        let frame = ModalPointerFrame::new(true);
        assert!(frame.owns_event(true));
        assert!(frame.owns_event(false));

        let normal_frame = ModalPointerFrame::new(false);
        assert!(!normal_frame.owns_event(false));
        assert!(normal_frame.owns_event(true));
    }

    #[test]
    fn axis_accumulates_partial_steps_with_foot_thresholds() {
        let mut wheel = WheelAccumulator::default();
        assert_eq!(wheel.push(0.0, 0, -60, 10), None);
        assert_eq!(wheel.push(0.0, 0, -59, 10), None);
        assert_eq!(wheel.push(0.0, 0, -1, 10), Some((MouseAction::WheelUp, 1)));
        assert_eq!(wheel.push(0.0, 0, 119, 10), None);
        assert_eq!(wheel.push(0.0, 0, 1, 10), Some((MouseAction::WheelDown, 1)));

        assert_eq!(
            wheel.push(0.0, 20, 0, 10),
            Some((MouseAction::WheelDown, 20))
        );
        assert_eq!(
            wheel.push(0.0, 0, 0, 10),
            None,
            "zero frames do not flush a different source implicitly"
        );
        assert_eq!(wheel.push(0.0, 1, 0, 10), Some((MouseAction::WheelDown, 1)));

        assert_eq!(wheel.push(-4.0, 0, 0, 10), None);
        assert_eq!(wheel.push(-6.0, 0, 0, 10), Some((MouseAction::WheelUp, 1)));
    }

    #[test]
    fn mouse_reports_cover_sgr_legacy_modifiers_motion_and_wheel() {
        let position = CellPosition { row: 4, column: 9 };
        assert_eq!(
            mouse_report(MouseAction::Press(0), position, Modifiers::default(), true,),
            Some(b"\x1b[<0;10;5M".to_vec())
        );
        assert_eq!(
            mouse_report(
                MouseAction::Release(0),
                position,
                Modifiers::default(),
                true,
            ),
            Some(b"\x1b[<0;10;5m".to_vec())
        );
        let modifiers = Modifiers {
            shift: true,
            ctrl: true,
            ..Modifiers::default()
        };
        assert_eq!(
            mouse_report(MouseAction::Motion(0), position, modifiers, true),
            Some(b"\x1b[<52;10;5M".to_vec())
        );
        assert_eq!(
            mouse_report(
                MouseAction::WheelUp,
                CellPosition { row: 0, column: 0 },
                Modifiers::default(),
                false,
            ),
            Some(vec![0x1b, b'[', b'M', 96, 33, 33])
        );
        assert!(
            mouse_report(
                MouseAction::WheelDown,
                CellPosition {
                    row: 500,
                    column: 500
                },
                Modifiers::default(),
                false,
            )
            .is_none()
        );
    }
}
