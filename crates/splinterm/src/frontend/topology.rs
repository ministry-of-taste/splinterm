//! Window-local topology commands and updates shared across runtime boundaries.

use splinterm_core::{
    DojoId, LairId, LairRetention, LayoutNode, SplintId, SplitRatio, TopologyRevision,
};
use splinterm_protocol::{MutationTarget, PresetDojoLaunch, PresetTarget};

use super::{
    FontUpdate, SessionPickerItem, ThemeUpdate, WindowAccessCommand, WindowAccessUpdate,
    WindowPaneOptions,
};
use crate::navigation_projection::{
    NavigationAction, NavigationExplorerSplintTarget, NavigationExplorerView, NavigationNodeId,
    WindowAgentStatus,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WindowDojoIdentity {
    pub topology_revision: TopologyRevision,
    pub lair_id: LairId,
    pub dojo_id: DojoId,
    pub lair_name: String,
    pub lair_retention: LairRetention,
    pub dojo_name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectorKind {
    Dojo,
    Lair,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionPickerCreationTarget {
    pub topology_revision: TopologyRevision,
    pub action: NavigationAction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionPickerTarget {
    pub topology_revision: TopologyRevision,
    pub lair_id: LairId,
    pub dojo_id: DojoId,
    pub action: NavigationAction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LairExplorerActivationTarget {
    Dojo(SessionPickerTarget),
    Splint(NavigationExplorerSplintTarget),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionPickerCatalog {
    pub items: Vec<SessionPickerItem>,
    pub targets: Vec<SessionPickerTarget>,
    pub creation_enabled: bool,
    pub creation_blocker: Option<&'static str>,
    pub creation_target: Option<SessionPickerCreationTarget>,
    pub initial_row: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LairDirection {
    Previous,
    Next,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LairPromptKind {
    Rename,
    Preview,
    Restore,
    Terminate,
}

/// A saved layout preview row's launch column for a Splint that has no
/// recipe to restore it with; the prompt draws it in danger.
pub const NOT_RESTORABLE: &str = "not restorable";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LairPromptTarget {
    pub topology_revision: TopologyRevision,
    pub lair_id: LairId,
    pub dojo_id: Option<DojoId>,
    pub name: String,
    pub retention: LairRetention,
    pub preview: String,
    pub targets: Vec<MutationTarget>,
}

/// Exact Explorer row authority, retained through menu and confirmation dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExplorerContextTarget {
    pub topology_revision: TopologyRevision,
    pub node: NavigationNodeId,
    pub live_incarnation: Option<u64>,
}

impl ExplorerContextTarget {
    #[must_use]
    pub fn command(self, command: WindowTopologyCommand) -> WindowTopologyCommand {
        WindowTopologyCommand::ExplorerContext {
            target: self,
            command: Box::new(command),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WindowTopologyCommand {
    /// The compositor closed the Window, as opposed to an explicit detach.
    CloseWindow,
    ExplorerContext {
        target: ExplorerContextTarget,
        command: Box<WindowTopologyCommand>,
    },
    RequestDojoPrompt {
        dojo_id: DojoId,
        kind: LairPromptKind,
    },
    Split {
        dojo_id: DojoId,
        target: SplintId,
        axis: splinterm_core::Axis,
        /// Client-local placeholder already rendered for a remote split.
        pending: Option<SplintId>,
    },
    Close {
        dojo_id: DojoId,
        target: SplintId,
    },
    AdjustRatio {
        dojo_id: DojoId,
        target: SplintId,
        delta: i16,
    },
    SetRatio {
        dojo_id: DojoId,
        target: SplintId,
        ancestor: u16,
        ratio: SplitRatio,
    },
    RequestSessionPicker,
    RequestLairExplorer {
        focused_splint: Option<SplintId>,
    },
    RequestSelector {
        kind: SelectorKind,
        lair_id: LairId,
    },
    OpenDojo {
        target: SessionPickerTarget,
        explorer_target: Option<LairExplorerActivationTarget>,
    },
    FocusSplint {
        target: NavigationExplorerSplintTarget,
    },
    NewLair {
        source_splint_id: SplintId,
    },
    PickerNewLair {
        topology_revision: TopologyRevision,
        source_splint_id: SplintId,
    },
    NewDojo {
        lair_id: LairId,
        source_splint_id: SplintId,
    },
    PickerNewDojo {
        topology_revision: TopologyRevision,
        lair_id: LairId,
        source_splint_id: SplintId,
    },
    MaterializePreset {
        target: PresetTarget,
        dojos: Vec<PresetDojoLaunch>,
    },
    NavigateLair {
        current_lair_id: LairId,
        direction: LairDirection,
    },
    RequestLairPrompt {
        lair_id: LairId,
        kind: LairPromptKind,
        expected_retention: Option<LairRetention>,
    },
    RequestDojoRestorePrompt {
        dojo_id: DojoId,
    },
    RenameLair {
        lair_id: LairId,
        name: String,
    },
    TerminateLair {
        lair_id: LairId,
        targets: Vec<MutationTarget>,
    },
    SetLairRetention {
        lair_id: LairId,
        expected_retention: LairRetention,
        retention: LairRetention,
    },
    RestoreLair {
        expected_topology_revision: TopologyRevision,
        lair_id: LairId,
    },
    RestoreDojo {
        expected_topology_revision: TopologyRevision,
        dojo_id: DojoId,
    },
    RenameDojo {
        dojo_id: DojoId,
        name: String,
    },
    TerminateDojo {
        dojo_id: DojoId,
        splints: Vec<(SplintId, u64)>,
    },
    ActivateTab {
        dojo_id: DojoId,
    },
    CloseTab {
        dojo_id: DojoId,
    },
    CloseTabs {
        retain_dojo_id: DojoId,
        dojo_ids: Vec<DojoId>,
    },
    /// The person's explicit "Clear agent status" for one live incarnation.
    /// The trusted UI may only clear a status, never set one.
    ClearAgentStatus {
        splint_id: SplintId,
        incarnation: u64,
    },
    /// Explorer **Use app name**: undo a Splint rename so its rows show the
    /// running app's name again (Plan 0069).
    ResetSplintTitle {
        splint_id: SplintId,
    },
    /// The **Agent access** list and its revokes (ADR 0014).
    Access(WindowAccessCommand),
}

impl WindowTopologyCommand {
    /// Whether this command waits while the Window is paused for a daemon
    /// handoff (Plan 0061 M3.12a): it changes the session, opens a prompt
    /// that only leads to such a change, or attaches panes, which a fenced
    /// daemon refuses. Browsing pickers, previews, switching between this
    /// Window's tabs, and closing the Window keep working.
    #[must_use]
    pub fn paused_during_handoff(&self) -> bool {
        match self {
            Self::CloseWindow
            | Self::RequestSessionPicker
            | Self::RequestLairExplorer { .. }
            | Self::RequestSelector { .. }
            | Self::ActivateTab { .. }
            // Listing and revoking access only read or reduce authority, so
            // the Window never holds them for a pause. While the Window waits
            // out a handoff, the topology manager abandons them like any
            // other daemon request, and a revoke says nothing was revoked.
            | Self::Access(_) => false,
            Self::ExplorerContext { command, .. } => command.paused_during_handoff(),
            Self::OpenDojo { target, .. } => target.action != NavigationAction::ActivateDojo,
            Self::FocusSplint { target } => {
                target.capability.action != NavigationAction::FocusSplint
            }
            Self::RequestLairPrompt { kind, .. } | Self::RequestDojoPrompt { kind, .. } => {
                *kind != LairPromptKind::Preview
            }
            Self::Split { .. }
            | Self::Close { .. }
            | Self::AdjustRatio { .. }
            | Self::SetRatio { .. }
            | Self::NewLair { .. }
            | Self::PickerNewLair { .. }
            | Self::NewDojo { .. }
            | Self::PickerNewDojo { .. }
            | Self::MaterializePreset { .. }
            | Self::NavigateLair { .. }
            | Self::RequestDojoRestorePrompt { .. }
            | Self::RenameLair { .. }
            | Self::TerminateLair { .. }
            | Self::SetLairRetention { .. }
            | Self::RestoreLair { .. }
            | Self::RestoreDojo { .. }
            | Self::RenameDojo { .. }
            | Self::TerminateDojo { .. }
            | Self::CloseTab { .. }
            | Self::CloseTabs { .. }
            | Self::ClearAgentStatus { .. }
            | Self::ResetSplintTitle { .. } => true,
        }
    }
}

pub enum WindowTopologyUpdate {
    ShowExplorerPrompt {
        guard: ExplorerContextTarget,
        kind: LairPromptKind,
        target: LairPromptTarget,
    },
    Apply {
        topology_revision: TopologyRevision,
        dojo_id: DojoId,
        layout: LayoutNode,
        added: Vec<WindowPaneOptions>,
        removed: Vec<SplintId>,
        focused: Option<SplintId>,
    },
    OpenTab {
        identity: WindowDojoIdentity,
        layout: LayoutNode,
        panes: Vec<WindowPaneOptions>,
        focused: SplintId,
        acknowledged: tokio::sync::oneshot::Sender<std::result::Result<(), String>>,
        explorer_target: Option<LairExplorerActivationTarget>,
        /// `false` appends the tab without switching to it (Plan 0067 M3): the
        /// active tab, its history, and keyboard focus stay as they were.
        activate: bool,
    },
    ActivateTab {
        dojo_id: DojoId,
        explorer_target: Option<LairExplorerActivationTarget>,
    },
    ActivateSplint {
        dojo_id: DojoId,
        splint_id: SplintId,
        live_incarnation: Option<u64>,
        explorer_target: Option<LairExplorerActivationTarget>,
    },
    RemoveTab {
        dojo_id: DojoId,
        acknowledged: tokio::sync::oneshot::Sender<()>,
    },
    UpdateIdentity(WindowDojoIdentity),
    TabFailed {
        dojo_id: Option<DojoId>,
        message: String,
        explorer_target: Option<LairExplorerActivationTarget>,
    },
    ShowSessionPicker {
        catalog: SessionPickerCatalog,
    },
    ShowLairExplorer {
        view: NavigationExplorerView,
    },
    ShowSelector {
        kind: SelectorKind,
        catalog: SessionPickerCatalog,
    },
    ShowLairPrompt {
        kind: LairPromptKind,
        target: LairPromptTarget,
    },
    SessionPickerFailed(String),
    LairExplorerFailed(String),
    /// Daemon lifecycle or topology changed, including work not open in a tab.
    LairExplorerInvalidated,
    /// Agent status of this Window's attached Dojos changed. Display data only.
    AgentStatus(WindowAgentStatus),
    /// The daemon accepted a clear for one live incarnation; the next topology
    /// poll supersedes this local application.
    AgentStatusCleared {
        splint_id: SplintId,
        incarnation: u64,
    },
    Theme(ThemeUpdate),
    Font(FontUpdate),
    /// The access inventory, or the outcome of an access command.
    Access(WindowAccessUpdate),
    /// Plan 0061 M3.12b: after a daemon connection lost during a handoff, the
    /// topology manager reconnected and resnapshotted every tab (the `Apply`
    /// updates ahead of this one). `exited` names the Splints that ended
    /// meanwhile, by tab. The frontend ends the handoff pause on this update.
    Reconnected {
        daemon_generation: u64,
        /// The reconnection epoch this resnapshot answers.
        reconnect_epoch: u64,
        exited: Vec<(DojoId, SplintId)>,
    },
    Closed,
    Shutdown(String),
}

impl WindowTopologyUpdate {
    /// Whether this update waits while the inline Dojo picker is open, so the
    /// picker never acts on a layout it did not show. A background tab
    /// (Plan 0067 M3) changes nothing the picker shows and is never deferred:
    /// the topology manager waits for its acknowledgement.
    #[must_use]
    pub const fn defers_for_inline_picker(&self) -> bool {
        matches!(
            self,
            Self::Apply { .. }
                | Self::OpenTab { activate: true, .. }
                | Self::ActivateTab { .. }
                | Self::ActivateSplint { .. }
                | Self::RemoveTab { .. }
                | Self::UpdateIdentity(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picker_target(action: NavigationAction) -> SessionPickerTarget {
        SessionPickerTarget {
            topology_revision: TopologyRevision::new(3),
            lair_id: LairId::new(),
            dojo_id: DojoId::new(),
            action,
        }
    }

    #[test]
    fn only_a_background_tab_opens_under_the_inline_picker() {
        let lair_id = LairId::new();
        let dojo_id = DojoId::new();
        let splint = splinterm_core::Splint::shell("/tmp".into());
        let focused = splint.id;
        let open = |activate| {
            let (acknowledged, _) = tokio::sync::oneshot::channel();
            WindowTopologyUpdate::OpenTab {
                identity: WindowDojoIdentity {
                    topology_revision: TopologyRevision::new(3),
                    lair_id,
                    dojo_id,
                    lair_name: "agent".into(),
                    lair_retention: LairRetention::default(),
                    dojo_name: "Dojo 2".into(),
                },
                layout: LayoutNode::Leaf(splint.clone()),
                panes: Vec::new(),
                focused,
                acknowledged,
                explorer_target: None,
                activate,
            }
        };
        assert!(open(true).defers_for_inline_picker());
        assert!(!open(false).defers_for_inline_picker());
        assert!(
            WindowTopologyUpdate::ActivateTab {
                dojo_id,
                explorer_target: None,
            }
            .defers_for_inline_picker()
        );
        assert!(!WindowTopologyUpdate::LairExplorerInvalidated.defers_for_inline_picker());
    }

    #[test]
    fn handoff_pause_holds_session_changes_and_lets_local_ui_through() {
        let lair_id = LairId::new();
        let dojo_id = DojoId::new();
        for command in [
            WindowTopologyCommand::CloseWindow,
            WindowTopologyCommand::RequestSessionPicker,
            WindowTopologyCommand::RequestLairExplorer {
                focused_splint: None,
            },
            WindowTopologyCommand::RequestSelector {
                kind: SelectorKind::Dojo,
                lair_id,
            },
            WindowTopologyCommand::ActivateTab { dojo_id },
            WindowTopologyCommand::OpenDojo {
                target: picker_target(NavigationAction::ActivateDojo),
                explorer_target: None,
            },
            WindowTopologyCommand::RequestLairPrompt {
                lair_id,
                kind: LairPromptKind::Preview,
                expected_retention: None,
            },
        ] {
            assert!(!command.paused_during_handoff(), "{command:?}");
        }
        for command in [
            WindowTopologyCommand::OpenDojo {
                target: picker_target(NavigationAction::AttachDojo),
                explorer_target: None,
            },
            WindowTopologyCommand::RequestLairPrompt {
                lair_id,
                kind: LairPromptKind::Rename,
                expected_retention: None,
            },
            WindowTopologyCommand::RequestDojoPrompt {
                dojo_id,
                kind: LairPromptKind::Terminate,
            },
            WindowTopologyCommand::NewDojo {
                lair_id,
                source_splint_id: SplintId::new(),
            },
            WindowTopologyCommand::CloseTab { dojo_id },
            WindowTopologyCommand::NavigateLair {
                current_lair_id: lair_id,
                direction: LairDirection::Next,
            },
            WindowTopologyCommand::AdjustRatio {
                dojo_id,
                target: SplintId::new(),
                delta: 50,
            },
        ] {
            assert!(command.paused_during_handoff(), "{command:?}");
        }
        // An Explorer menu's guarded command is classified by what it wraps.
        let target = ExplorerContextTarget {
            topology_revision: TopologyRevision::new(3),
            node: NavigationNodeId::Lair(lair_id),
            live_incarnation: None,
        };
        assert!(
            target
                .command(WindowTopologyCommand::CloseTab { dojo_id })
                .paused_during_handoff()
        );
        assert!(
            !target
                .command(WindowTopologyCommand::RequestLairPrompt {
                    lair_id,
                    kind: LairPromptKind::Preview,
                    expected_retention: None,
                })
                .paused_during_handoff()
        );
    }
}
