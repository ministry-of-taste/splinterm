//! Platform-independent contracts shared by application orchestration and presentation adapters.

mod action_menu;
mod agent_access;
mod agent_status;
mod binding_help;
pub mod dialog_text;
mod lair_explorer;
mod message;
mod navigation_accessibility;
mod options;
mod picker;
mod text_edit;
mod topology;
mod trusted_dialog;

pub(crate) use action_menu::{
    BuiltInCommandDispatch, BuiltInCommandId, COMMAND_PALETTE_PAGE_ITEMS, CommandControlAction,
    CommandHistoryAction, CommandPaletteContext, CommandPaletteUi, CommandTabMoveAvailability,
    CommandZoomAction, DojoPromptUi, TabContextMenuUi, TabMenuActionDescriptor, TabMenuActionId,
    TabMenuContext, TabMenuDispatch, TabMenuRightPress, TerminationDecision,
    close_other_tabs_command, command_descriptor, lair_lifecycle_command, tab_menu_right_press,
};
#[cfg(test)]
pub(crate) use action_menu::{TAB_MENU_ACTIONS, tab_menu_descriptor};
pub(crate) use agent_access::{
    AGENT_ACCESS_PAGE_ITEMS, AccessActivation, AccessMarks, AgentAccessUi, PromptReconcile,
    RevokeAccessUi, waiting_title,
};
pub use agent_access::{
    AccessSnapshot, MAX_WINDOW_ACCESS_ENTRIES, RevokeAllCheck, WindowAccessCommand,
    WindowAccessUpdate, check_revoke_all, counts_are_empty, revoke_all_summary, shown, shown_name,
    shown_prompt_name, unsafe_display_character,
};
/// An "Always" inventory entry for `lair_id`, for tests outside `frontend`.
#[cfg(test)]
pub(crate) fn agent_access_test_entry(
    lair_id: splinterm_core::LairId,
) -> splinterm_protocol::AccessEntry {
    agent_access::tests::entry(
        agent_access::tests::remembered_key(),
        splinterm_protocol::AccessEntryKind::Remembered,
        lair_id,
    )
}
#[cfg(test)]
pub(crate) use agent_status::state_tint;
pub(crate) use agent_status::{AgentAggregate, AgentGlyphs, AgentSpinner, badged_tab_label};
pub(crate) use binding_help::{BINDING_HELP_PAGE_ITEMS, BindingHelpUi};
pub use dialog_text::{DialogIdentity, DialogLine, DialogLineRole, DialogText};
#[cfg(test)]
pub(crate) use lair_explorer::LairExplorerRowKind;
pub(crate) use lair_explorer::{
    LairExplorerAccess, LairExplorerDecision, LairExplorerFilter, LairExplorerRow, LairExplorerUi,
};
pub use message::{
    AuthorityStatus, FontUpdate, PerfTraceCorrelation, ThemeUpdate, WindowCommand, WindowUpdate,
};
pub(crate) use navigation_accessibility::{
    NavigationAccessAction, NavigationAccessContext, NavigationAccessibility,
};
pub(crate) use options::DialogFrame;
pub(crate) use options::TrustedInput;
pub use options::{
    AccessConsentRenderer, TerminalGridLimits, TrustedConsentUi, WindowOptions, WindowPaneOptions,
};
pub(crate) use picker::{PickerHitTarget, synthetic_text_snapshot};
pub use picker::{SessionPickerDecision, SessionPickerItem, SessionPickerUi};
pub(crate) use text_edit::BoundedTextEditor;
pub use topology::{
    ExplorerContextTarget, LairDirection, LairExplorerActivationTarget, LairPromptKind,
    LairPromptTarget, SelectorKind, SessionPickerCatalog, SessionPickerCreationTarget,
    SessionPickerTarget, WindowDojoIdentity, WindowTopologyCommand, WindowTopologyUpdate,
};
pub(crate) use trusted_dialog::{
    MINIMUM_ACTION_ROWS, TrustedDialogKey, TrustedDialogKind, action_area_top, dialog_font_step,
    dialog_text_rows,
};
