//! Platform-independent built-in command-palette state.

use splinterm_core::{Axis, DojoId, LairId, LairRetention, SplintId};

use super::{ExplorerContextTarget, SessionPickerTarget};
use crate::keymap::{ActionId, ResolvedKeymap};
use crate::navigation_projection::{
    NavigationAction, NavigationExplorerView, NavigationLifecycle, NavigationNodeId,
    WindowAttachment,
};

use super::{
    LairDirection, LairPromptKind, LairPromptTarget, SelectorKind, WindowTopologyCommand,
    agent_access::{RevokeAccessUi, WindowAccessCommand},
    text_edit::BoundedTextEditor,
};

const MAX_QUERY_BYTES: usize = 256;
const MAX_QUERY_SCALARS: usize = 128;
pub(crate) const COMMAND_PALETTE_PAGE_ITEMS: usize = 7;
const MAX_DOJO_NAME_BYTES: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DojoActionTarget {
    pub(crate) dojo_id: DojoId,
    pub(crate) name: String,
    pub(crate) pane_count: usize,
    pub(crate) splints: Vec<(SplintId, u64)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RenameDojoUi {
    target: DojoActionTarget,
    editor: BoundedTextEditor,
}

impl RenameDojoUi {
    pub(crate) fn new(target: DojoActionTarget) -> Self {
        Self {
            editor: BoundedTextEditor::new(
                target.name.clone(),
                MAX_DOJO_NAME_BYTES,
                MAX_DOJO_NAME_BYTES,
                true,
            ),
            target,
        }
    }

    pub(crate) const fn target(&self) -> &DojoActionTarget {
        &self.target
    }

    pub(crate) fn input(&self) -> &str {
        self.editor.text()
    }

    pub(crate) fn append_text(&mut self, text: &str) -> bool {
        self.editor.insert(text)
    }

    pub(crate) fn backspace(&mut self) -> bool {
        self.editor.backspace()
    }

    pub(crate) fn editor_mut(&mut self) -> &mut BoundedTextEditor {
        &mut self.editor
    }

    pub(crate) fn command(&self) -> Option<WindowTopologyCommand> {
        let name = self.editor.text().trim();
        (!name.is_empty() && name.len() <= MAX_DOJO_NAME_BYTES).then(|| {
            WindowTopologyCommand::RenameDojo {
                dojo_id: self.target.dojo_id,
                name: name.to_owned(),
            }
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TerminationDecision {
    #[default]
    Cancel,
    Terminate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TerminateDojoUi {
    target: DojoActionTarget,
    decision: TerminationDecision,
}

impl TerminateDojoUi {
    pub(crate) fn new(target: DojoActionTarget) -> Self {
        Self {
            target,
            decision: TerminationDecision::Cancel,
        }
    }

    pub(crate) const fn target(&self) -> &DojoActionTarget {
        &self.target
    }

    pub(crate) const fn decision(&self) -> TerminationDecision {
        self.decision
    }

    pub(crate) fn move_selection(&mut self) -> bool {
        self.decision = match self.decision {
            TerminationDecision::Cancel => TerminationDecision::Terminate,
            TerminationDecision::Terminate => TerminationDecision::Cancel,
        };
        true
    }

    pub(crate) fn select(&mut self, decision: TerminationDecision) -> bool {
        if self.decision == decision {
            return false;
        }
        self.decision = decision;
        true
    }

    pub(crate) fn command(&self) -> Option<WindowTopologyCommand> {
        (self.decision == TerminationDecision::Terminate).then_some(
            WindowTopologyCommand::TerminateDojo {
                dojo_id: self.target.dojo_id,
                splints: self.target.splints.clone(),
            },
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RenameLairUi {
    target: LairPromptTarget,
    editor: BoundedTextEditor,
}

impl RenameLairUi {
    pub(crate) fn new(target: LairPromptTarget) -> Self {
        Self {
            editor: BoundedTextEditor::new(
                target.name.clone(),
                MAX_DOJO_NAME_BYTES,
                MAX_DOJO_NAME_BYTES,
                true,
            ),
            target,
        }
    }

    pub(crate) const fn target(&self) -> &LairPromptTarget {
        &self.target
    }

    pub(crate) fn input(&self) -> &str {
        self.editor.text()
    }

    pub(crate) fn append_text(&mut self, text: &str) -> bool {
        self.editor.insert(text)
    }

    pub(crate) fn backspace(&mut self) -> bool {
        self.editor.backspace()
    }

    pub(crate) fn editor_mut(&mut self) -> &mut BoundedTextEditor {
        &mut self.editor
    }

    pub(crate) fn command(&self) -> Option<WindowTopologyCommand> {
        let name = self.editor.text().trim();
        (!name.is_empty() && name.len() <= MAX_DOJO_NAME_BYTES).then(|| {
            WindowTopologyCommand::RenameLair {
                lair_id: self.target.lair_id,
                name: name.to_owned(),
            }
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RestoreLairUi {
    target: LairPromptTarget,
    decision: TerminationDecision,
}

impl RestoreLairUi {
    pub(crate) fn new(target: LairPromptTarget) -> Self {
        Self {
            target,
            decision: TerminationDecision::Cancel,
        }
    }

    pub(crate) const fn target(&self) -> &LairPromptTarget {
        &self.target
    }

    pub(crate) const fn decision(&self) -> TerminationDecision {
        self.decision
    }

    pub(crate) fn move_selection(&mut self) -> bool {
        self.decision = match self.decision {
            TerminationDecision::Cancel => TerminationDecision::Terminate,
            TerminationDecision::Terminate => TerminationDecision::Cancel,
        };
        true
    }

    pub(crate) fn select(&mut self, decision: TerminationDecision) -> bool {
        if self.decision == decision {
            return false;
        }
        self.decision = decision;
        true
    }

    pub(crate) fn command(&self) -> Option<WindowTopologyCommand> {
        (self.decision == TerminationDecision::Terminate).then_some(self.target.dojo_id.map_or(
            WindowTopologyCommand::RestoreLair {
                expected_topology_revision: self.target.topology_revision,
                lair_id: self.target.lair_id,
            },
            |dojo_id| WindowTopologyCommand::RestoreDojo {
                expected_topology_revision: self.target.topology_revision,
                dojo_id,
            },
        ))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TerminateLairUi {
    target: LairPromptTarget,
    decision: TerminationDecision,
}

impl TerminateLairUi {
    pub(crate) fn new(target: LairPromptTarget) -> Self {
        Self {
            target,
            decision: TerminationDecision::Cancel,
        }
    }

    pub(crate) const fn target(&self) -> &LairPromptTarget {
        &self.target
    }

    pub(crate) const fn decision(&self) -> TerminationDecision {
        self.decision
    }

    pub(crate) fn move_selection(&mut self) -> bool {
        self.decision = match self.decision {
            TerminationDecision::Cancel => TerminationDecision::Terminate,
            TerminationDecision::Terminate => TerminationDecision::Cancel,
        };
        true
    }

    pub(crate) fn select(&mut self, decision: TerminationDecision) -> bool {
        if self.decision == decision {
            return false;
        }
        self.decision = decision;
        true
    }

    pub(crate) fn command(&self) -> Option<WindowTopologyCommand> {
        (self.decision == TerminationDecision::Terminate).then(|| {
            WindowTopologyCommand::TerminateLair {
                lair_id: self.target.lair_id,
                targets: self.target.targets.clone(),
            }
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DojoPromptUi {
    Rename(RenameDojoUi),
    Terminate(TerminateDojoUi),
    RenameLair(RenameLairUi),
    PreviewLair(LairPromptTarget),
    RestoreLair(RestoreLairUi),
    TerminateLair(TerminateLairUi),
    /// Revoking agent access, or its result (ADR 0014).
    RevokeAccess(RevokeAccessUi),
}

impl DojoPromptUi {
    pub(crate) const fn is_rename(&self) -> bool {
        matches!(self, Self::Rename(_) | Self::RenameLair(_))
    }

    pub(crate) const fn is_preview(&self) -> bool {
        match self {
            Self::PreviewLair(_) => true,
            Self::RevokeAccess(prompt) => prompt.is_notice(),
            _ => false,
        }
    }

    pub(crate) const fn uses_layout_summary(&self) -> bool {
        matches!(
            self,
            Self::PreviewLair(_) | Self::RestoreLair(_) | Self::RevokeAccess(_)
        )
    }

    pub(crate) fn title_and_body(&self) -> (&'static str, String) {
        match self {
            Self::Rename(prompt) => (
                "RENAME DOJO",
                format!("Name for ‘{}’", prompt.target().name),
            ),
            Self::Terminate(prompt) => (
                "TERMINATE DOJO",
                format!(
                    "Terminate ‘{}’ and its {} pane{}?",
                    prompt.target().name,
                    prompt.target().pane_count,
                    if prompt.target().pane_count == 1 {
                        ""
                    } else {
                        "s"
                    }
                ),
            ),
            Self::RenameLair(prompt) => (
                "RENAME LAIR",
                format!("Name for ‘{}’", prompt.target().name),
            ),
            Self::PreviewLair(target) => ("SAVED LAIR PREVIEW", target.preview.clone()),
            Self::RestoreLair(prompt) => (
                "RESTORE SAVED LAIR",
                format!(
                    "Restore {} captured Splint{}?\n\n{}",
                    prompt.target().targets.len(),
                    if prompt.target().targets.len() == 1 {
                        ""
                    } else {
                        "s"
                    },
                    prompt.target().preview,
                ),
            ),
            Self::TerminateLair(prompt) => (
                "TERMINATE LAIR",
                format!(
                    "Terminate ‘{}’ and its {} captured pane{}?",
                    prompt.target().name,
                    prompt.target().targets.len(),
                    if prompt.target().targets.len() == 1 {
                        ""
                    } else {
                        "s"
                    }
                ),
            ),
            Self::RevokeAccess(prompt) => prompt.title_and_body(),
        }
    }

    pub(crate) fn input(&self) -> Option<&str> {
        match self {
            Self::Rename(prompt) => Some(prompt.input()),
            Self::RenameLair(prompt) => Some(prompt.input()),
            Self::Terminate(_)
            | Self::PreviewLair(_)
            | Self::RestoreLair(_)
            | Self::TerminateLair(_)
            | Self::RevokeAccess(_) => None,
        }
    }

    pub(crate) fn editor_mut(&mut self) -> Option<&mut BoundedTextEditor> {
        match self {
            Self::Rename(prompt) => Some(prompt.editor_mut()),
            Self::RenameLair(prompt) => Some(prompt.editor_mut()),
            Self::Terminate(_)
            | Self::PreviewLair(_)
            | Self::RestoreLair(_)
            | Self::TerminateLair(_)
            | Self::RevokeAccess(_) => None,
        }
    }

    pub(crate) const fn confirmation_label(&self) -> Option<&'static str> {
        match self {
            Self::RestoreLair(_) => Some("Restore"),
            Self::Terminate(_) | Self::TerminateLair(_) => Some("Terminate"),
            Self::RevokeAccess(prompt) => prompt.confirmation_label(),
            Self::Rename(_) | Self::RenameLair(_) | Self::PreviewLair(_) => None,
        }
    }

    pub(crate) const fn decision(&self) -> Option<TerminationDecision> {
        match self {
            Self::Terminate(prompt) => Some(prompt.decision()),
            Self::RestoreLair(prompt) => Some(prompt.decision()),
            Self::TerminateLair(prompt) => Some(prompt.decision()),
            Self::RevokeAccess(prompt) => prompt.decision(),
            Self::Rename(_) | Self::RenameLair(_) | Self::PreviewLair(_) => None,
        }
    }

    pub(crate) fn rename(
        dojo_id: DojoId,
        name: String,
        pane_count: usize,
        splints: Vec<(SplintId, u64)>,
    ) -> Self {
        Self::Rename(RenameDojoUi::new(DojoActionTarget {
            dojo_id,
            name,
            pane_count,
            splints,
        }))
    }

    pub(crate) fn rename_lair(target: LairPromptTarget) -> Self {
        Self::RenameLair(RenameLairUi::new(target))
    }

    pub(crate) fn preview_lair(target: LairPromptTarget) -> Self {
        Self::PreviewLair(target)
    }

    pub(crate) fn restore_lair(target: LairPromptTarget) -> Self {
        Self::RestoreLair(RestoreLairUi::new(target))
    }

    pub(crate) fn terminate_lair(target: LairPromptTarget) -> Self {
        Self::TerminateLair(TerminateLairUi::new(target))
    }

    pub(crate) fn terminate(
        dojo_id: DojoId,
        name: String,
        pane_count: usize,
        splints: Vec<(SplintId, u64)>,
    ) -> Self {
        Self::Terminate(TerminateDojoUi::new(DojoActionTarget {
            dojo_id,
            name,
            pane_count,
            splints,
        }))
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum TabMenuActionId {
    RenameTab,
    RenameLair,
    SaveLayout,
    PinLair,
    UnpinLair,
    PreviewLair,
    Restore,
    TerminateLair,
    FocusSplint,
    SplitBelow,
    SplitRight,
    CloseSplint,
    ActivateTab,
    NewDojo,
    CloseTab,
    CloseOtherTabs,
    TerminateDojo,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TabMenuContext {
    pub(crate) lair_id: LairId,
    pub(crate) source_splint_id: SplintId,
    pub(crate) dojo_id: DojoId,
    pub(crate) dojo_name: String,
    pub(crate) pane_count: usize,
    pub(crate) splints: Vec<(SplintId, u64)>,
    pub(crate) active: bool,
    pub(crate) other_dojo_ids: Vec<DojoId>,
}

impl TabMenuContext {
    fn action_target(&self) -> DojoActionTarget {
        DojoActionTarget {
            dojo_id: self.dojo_id,
            name: self.dojo_name.clone(),
            pane_count: self.pane_count,
            splints: self.splints.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TabMenuActionDescriptor {
    pub(crate) id: TabMenuActionId,
    pub(crate) title: &'static str,
}

pub(crate) const TAB_MENU_ACTIONS: [TabMenuActionDescriptor; 6] = [
    TabMenuActionDescriptor {
        id: TabMenuActionId::RenameTab,
        title: "Rename Tab",
    },
    TabMenuActionDescriptor {
        id: TabMenuActionId::ActivateTab,
        title: "Activate Tab",
    },
    TabMenuActionDescriptor {
        id: TabMenuActionId::NewDojo,
        title: "New Dojo",
    },
    TabMenuActionDescriptor {
        id: TabMenuActionId::CloseTab,
        title: "Close Tab",
    },
    TabMenuActionDescriptor {
        id: TabMenuActionId::CloseOtherTabs,
        title: "Close Other Tabs",
    },
    TabMenuActionDescriptor {
        id: TabMenuActionId::TerminateDojo,
        title: "Terminate Dojo…",
    },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TabMenuRightPress {
    Retarget(DojoId),
    RetargetExplorer(ExplorerContextTarget),
    Dismiss,
}

pub(crate) const fn tab_menu_right_press(
    target: Option<DojoId>,
    explorer: Option<ExplorerContextTarget>,
) -> TabMenuRightPress {
    if let Some(target) = explorer {
        return TabMenuRightPress::RetargetExplorer(target);
    }
    match target {
        Some(dojo_id) => TabMenuRightPress::Retarget(dojo_id),
        None => TabMenuRightPress::Dismiss,
    }
}

pub(crate) fn tab_menu_action_enabled(id: TabMenuActionId, context: &TabMenuContext) -> bool {
    match id {
        TabMenuActionId::RenameLair
        | TabMenuActionId::SaveLayout
        | TabMenuActionId::PinLair
        | TabMenuActionId::UnpinLair
        | TabMenuActionId::PreviewLair
        | TabMenuActionId::Restore
        | TabMenuActionId::TerminateLair
        | TabMenuActionId::FocusSplint
        | TabMenuActionId::SplitBelow
        | TabMenuActionId::SplitRight
        | TabMenuActionId::CloseSplint => false,
        TabMenuActionId::ActivateTab => !context.active,
        TabMenuActionId::CloseOtherTabs => !context.other_dojo_ids.is_empty(),
        TabMenuActionId::RenameTab
        | TabMenuActionId::NewDojo
        | TabMenuActionId::CloseTab
        | TabMenuActionId::TerminateDojo => true,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ContextMenuTarget {
    Tab(TabMenuContext),
    Explorer {
        target: ExplorerContextTarget,
        actions: Vec<TabMenuActionDescriptor>,
        commands: Vec<WindowTopologyCommand>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TabContextMenuUi {
    context: ContextMenuTarget,
    selected: usize,
    hovered: Option<TabMenuActionId>,
    /// The Window is paused for a daemon handoff: daemon-mutating actions
    /// are shown disabled (Plan 0061 M3.12a).
    mutations_paused: bool,
}

impl TabContextMenuUi {
    pub(crate) fn new(context: TabMenuContext) -> Self {
        let selected = TAB_MENU_ACTIONS
            .iter()
            .position(|action| tab_menu_action_enabled(action.id, &context))
            .unwrap_or(0);
        Self {
            context: ContextMenuTarget::Tab(context),
            selected,
            hovered: None,
            mutations_paused: false,
        }
    }

    /// Disables or re-enables daemon-mutating actions for a daemon handoff,
    /// keeping the selection on an enabled action. Returns whether it changed.
    pub(crate) fn set_mutations_paused(&mut self, paused: bool) -> bool {
        if self.mutations_paused == paused {
            return false;
        }
        self.mutations_paused = paused;
        if !self.action_enabled(self.selected_action())
            && let Some(index) = self
                .actions()
                .iter()
                .position(|action| self.action_enabled(action.id))
        {
            self.selected = index;
        }
        if self
            .hovered
            .is_some_and(|action| !self.action_enabled(action))
        {
            self.hovered = None;
        }
        true
    }

    #[allow(
        clippy::too_many_lines,
        reason = "closed row-specific action catalogs share one exact capture boundary"
    )]
    pub(crate) fn for_explorer(
        view: &NavigationExplorerView,
        node: NavigationNodeId,
        source_splint_id: Option<SplintId>,
        pane_controlled: bool,
        has_tab_capacity: bool,
    ) -> Option<Self> {
        if view.freshness != crate::navigation_projection::EndpointFreshness::Current {
            return None;
        }
        let lair_id = match node {
            NavigationNodeId::Lair(id) => id,
            NavigationNodeId::Dojo { lair_id, .. } | NavigationNodeId::Splint { lair_id, .. } => {
                lair_id
            }
        };
        let lair = view
            .lairs
            .iter()
            .find(|lair| lair.id == NavigationNodeId::Lair(lair_id))?;
        let mut target = ExplorerContextTarget {
            topology_revision: view.topology_revision,
            node,
            live_incarnation: None,
        };
        let mut entries = Vec::new();
        let live = |state| {
            matches!(
                state,
                NavigationLifecycle::Running | NavigationLifecycle::Starting
            )
        };
        match node {
            NavigationNodeId::Lair(_) => {
                entries.push((
                    TabMenuActionId::RenameLair,
                    WindowTopologyCommand::RequestLairPrompt {
                        lair_id,
                        kind: LairPromptKind::Rename,
                        expected_retention: None,
                    },
                ));
                if has_tab_capacity
                    && lair.can_create_dojo
                    && let Some(source_splint_id) = source_splint_id
                {
                    entries.push((
                        TabMenuActionId::NewDojo,
                        WindowTopologyCommand::PickerNewDojo {
                            topology_revision: view.topology_revision,
                            lair_id,
                            source_splint_id,
                        },
                    ));
                }
                if lair.retention == LairRetention::Disposable {
                    entries.push((
                        TabMenuActionId::SaveLayout,
                        WindowTopologyCommand::SetLairRetention {
                            lair_id,
                            expected_retention: lair.retention,
                            retention: LairRetention::Saved,
                        },
                    ));
                }
                let (id, retention) = if lair.retention == LairRetention::Pinned {
                    (TabMenuActionId::UnpinLair, LairRetention::Saved)
                } else {
                    (TabMenuActionId::PinLair, LairRetention::Pinned)
                };
                entries.push((
                    id,
                    WindowTopologyCommand::SetLairRetention {
                        lair_id,
                        expected_retention: lair.retention,
                        retention,
                    },
                ));
                if lair.retention != LairRetention::Disposable {
                    entries.push((
                        TabMenuActionId::PreviewLair,
                        WindowTopologyCommand::RequestLairPrompt {
                            lair_id,
                            kind: LairPromptKind::Preview,
                            expected_retention: None,
                        },
                    ));
                }
                if lair
                    .dojos
                    .iter()
                    .flat_map(|dojo| &dojo.splints)
                    .any(|splint| {
                        splint.target.capability.is_enabled()
                            && matches!(
                                splint.target.capability.action,
                                NavigationAction::PreviewRestoreSplint
                            )
                    })
                {
                    entries.push((
                        TabMenuActionId::Restore,
                        WindowTopologyCommand::RequestLairPrompt {
                            lair_id,
                            kind: LairPromptKind::Restore,
                            expected_retention: None,
                        },
                    ));
                }
                if lair
                    .dojos
                    .iter()
                    .flat_map(|dojo| &dojo.splints)
                    .any(|splint| live(splint.lifecycle))
                    && lair
                        .dojos
                        .iter()
                        .flat_map(|dojo| &dojo.splints)
                        .all(|splint| splint.target.last_incarnation.is_some())
                {
                    entries.push((
                        TabMenuActionId::TerminateLair,
                        WindowTopologyCommand::RequestLairPrompt {
                            lair_id,
                            kind: LairPromptKind::Terminate,
                            expected_retention: None,
                        },
                    ));
                }
            }
            NavigationNodeId::Dojo { dojo_id, .. } => {
                let dojo = lair
                    .dojos
                    .iter()
                    .find(|dojo| dojo.target.dojo_id == dojo_id)?;
                let capability = dojo.target.capability;
                if capability.is_enabled()
                    && matches!(
                        capability.action,
                        NavigationAction::AttachDojo | NavigationAction::ActivateDojo
                    )
                    && !dojo.active_here
                {
                    entries.push((
                        TabMenuActionId::ActivateTab,
                        WindowTopologyCommand::OpenDojo {
                            target: SessionPickerTarget {
                                topology_revision: view.topology_revision,
                                lair_id,
                                dojo_id,
                                action: capability.action,
                            },
                            explorer_target: None,
                        },
                    ));
                }
                entries.push((
                    TabMenuActionId::RenameTab,
                    WindowTopologyCommand::RequestDojoPrompt {
                        dojo_id,
                        kind: LairPromptKind::Rename,
                    },
                ));
                if dojo.attachment == WindowAttachment::Here {
                    entries.push((
                        TabMenuActionId::CloseTab,
                        WindowTopologyCommand::CloseTab { dojo_id },
                    ));
                }
                if dojo.splints.iter().any(|splint| {
                    splint.target.capability.is_enabled()
                        && splint.target.capability.action == NavigationAction::PreviewRestoreSplint
                }) {
                    entries.push((
                        TabMenuActionId::Restore,
                        WindowTopologyCommand::RequestDojoPrompt {
                            dojo_id,
                            kind: LairPromptKind::Restore,
                        },
                    ));
                }
                if dojo.splints.iter().any(|splint| live(splint.lifecycle))
                    && dojo
                        .splints
                        .iter()
                        .all(|splint| splint.target.last_incarnation.is_some())
                {
                    entries.push((
                        TabMenuActionId::TerminateDojo,
                        WindowTopologyCommand::RequestDojoPrompt {
                            dojo_id,
                            kind: LairPromptKind::Terminate,
                        },
                    ));
                }
            }
            NavigationNodeId::Splint {
                dojo_id, splint_id, ..
            } => {
                let dojo = lair
                    .dojos
                    .iter()
                    .find(|dojo| dojo.target.dojo_id == dojo_id)?;
                let splint = dojo
                    .splints
                    .iter()
                    .find(|splint| splint.target.splint_id == splint_id)?;
                target.live_incarnation = splint.target.live_incarnation;
                if splint.target.capability.is_enabled()
                    && matches!(
                        splint.target.capability.action,
                        NavigationAction::FocusSplint | NavigationAction::AttachAndFocusSplint
                    )
                {
                    entries.push((
                        TabMenuActionId::FocusSplint,
                        WindowTopologyCommand::FocusSplint {
                            target: splint.target,
                        },
                    ));
                }
                if splint.target.capability.is_enabled()
                    && splint.target.capability.action == NavigationAction::PreviewRestoreSplint
                {
                    entries.push((
                        TabMenuActionId::Restore,
                        WindowTopologyCommand::FocusSplint {
                            target: splint.target,
                        },
                    ));
                }
                if pane_controlled
                    && dojo.attachment == WindowAttachment::Here
                    && splint.lifecycle == NavigationLifecycle::Running
                    && target.live_incarnation.is_some()
                {
                    entries.push((
                        TabMenuActionId::SplitBelow,
                        WindowTopologyCommand::Split {
                            dojo_id,
                            target: splint_id,
                            axis: Axis::Vertical,
                            pending: None,
                        },
                    ));
                    entries.push((
                        TabMenuActionId::SplitRight,
                        WindowTopologyCommand::Split {
                            dojo_id,
                            target: splint_id,
                            axis: Axis::Horizontal,
                            pending: None,
                        },
                    ));
                    entries.push((
                        TabMenuActionId::CloseSplint,
                        WindowTopologyCommand::Close {
                            dojo_id,
                            target: splint_id,
                        },
                    ));
                }
            }
        }
        if entries.is_empty() {
            return None;
        }
        let (actions, commands) = entries
            .into_iter()
            .map(|(id, command)| {
                (
                    TabMenuActionDescriptor {
                        id,
                        title: match id {
                            TabMenuActionId::RenameTab => "Rename…",
                            TabMenuActionId::ActivateTab => "Open / Activate",
                            TabMenuActionId::CloseTab => "Close tab",
                            _ => tab_menu_descriptor(id).title,
                        },
                    },
                    target.command(command),
                )
            })
            .unzip();
        Some(Self {
            context: ContextMenuTarget::Explorer {
                target,
                actions,
                commands,
            },
            selected: 0,
            hovered: None,
            mutations_paused: false,
        })
    }

    pub(crate) const fn explorer_target(&self) -> Option<ExplorerContextTarget> {
        match self.context {
            ContextMenuTarget::Explorer { target, .. } => Some(target),
            ContextMenuTarget::Tab(_) => None,
        }
    }

    pub(crate) fn actions(&self) -> &[TabMenuActionDescriptor] {
        match &self.context {
            ContextMenuTarget::Explorer { actions, .. } => actions,
            ContextMenuTarget::Tab(_) => &TAB_MENU_ACTIONS,
        }
    }

    pub(crate) fn dispatch(&self, action: TabMenuActionId) -> Option<TabMenuDispatch> {
        if !self.action_enabled(action) {
            return None;
        }
        match &self.context {
            ContextMenuTarget::Explorer {
                actions, commands, ..
            } => actions
                .iter()
                .position(|entry| entry.id == action)
                .map(|index| TabMenuDispatch::Topology(commands[index].clone())),
            ContextMenuTarget::Tab(context) => tab_menu_dispatch(action, context),
        }
    }

    pub(crate) fn release_action(
        &self,
        pressed: Option<TabMenuActionId>,
        released: Option<TabMenuActionId>,
    ) -> Option<TabMenuActionId> {
        pressed.filter(|action| Some(*action) == released && self.action_enabled(*action))
    }

    pub(crate) fn action_enabled(&self, action: TabMenuActionId) -> bool {
        match &self.context {
            ContextMenuTarget::Explorer {
                actions, commands, ..
            } => actions
                .iter()
                .position(|entry| entry.id == action)
                .is_some_and(|index| {
                    !(self.mutations_paused && commands[index].paused_during_handoff())
                }),
            ContextMenuTarget::Tab(context) => {
                tab_menu_action_enabled(action, context)
                    && !(self.mutations_paused && tab_menu_action_paused_during_handoff(action))
            }
        }
    }

    pub(crate) fn selected_action(&self) -> TabMenuActionId {
        self.actions()[self.selected].id
    }

    pub(crate) const fn hovered(&self) -> Option<TabMenuActionId> {
        self.hovered
    }

    pub(crate) fn update_hovered(&mut self, hovered: Option<TabMenuActionId>) -> bool {
        let hovered = hovered.filter(|action| self.action_enabled(*action));
        if self.hovered == hovered {
            return false;
        }
        self.hovered = hovered;
        true
    }

    pub(crate) fn move_selection(&mut self, delta: isize) -> bool {
        let previous = self.selected;
        let count = self.actions().len();
        let enabled_count = self
            .actions()
            .iter()
            .filter(|action| self.action_enabled(action.id))
            .count();
        if enabled_count == 0 || delta == 0 {
            return false;
        }
        let steps = delta.unsigned_abs() % enabled_count;
        for _ in 0..steps {
            for distance in 1..=count {
                let candidate = if delta.is_negative() {
                    self.selected.saturating_add(count).saturating_sub(distance) % count
                } else {
                    self.selected.saturating_add(distance) % count
                };
                if self.action_enabled(self.actions()[candidate].id) {
                    self.selected = candidate;
                    break;
                }
            }
        }
        self.selected != previous
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TabMenuDispatch {
    Topology(WindowTopologyCommand),
    Rename(DojoActionTarget),
    ConfirmTermination(DojoActionTarget),
}

pub(crate) fn close_other_tabs_command(
    retain_dojo_id: DojoId,
    dojo_ids: Vec<DojoId>,
) -> Option<WindowTopologyCommand> {
    (!dojo_ids.is_empty()).then_some(WindowTopologyCommand::CloseTabs {
        retain_dojo_id,
        dojo_ids,
    })
}

pub(crate) fn tab_menu_dispatch(
    id: TabMenuActionId,
    context: &TabMenuContext,
) -> Option<TabMenuDispatch> {
    if !tab_menu_action_enabled(id, context) {
        return None;
    }
    match id {
        TabMenuActionId::RenameLair
        | TabMenuActionId::SaveLayout
        | TabMenuActionId::PinLair
        | TabMenuActionId::UnpinLair
        | TabMenuActionId::PreviewLair
        | TabMenuActionId::Restore
        | TabMenuActionId::TerminateLair
        | TabMenuActionId::FocusSplint
        | TabMenuActionId::SplitBelow
        | TabMenuActionId::SplitRight
        | TabMenuActionId::CloseSplint => None,
        TabMenuActionId::RenameTab => Some(TabMenuDispatch::Rename(context.action_target())),
        TabMenuActionId::ActivateTab => Some(TabMenuDispatch::Topology(
            WindowTopologyCommand::ActivateTab {
                dojo_id: context.dojo_id,
            },
        )),
        TabMenuActionId::NewDojo => {
            Some(TabMenuDispatch::Topology(WindowTopologyCommand::NewDojo {
                lair_id: context.lair_id,
                source_splint_id: context.source_splint_id,
            }))
        }
        TabMenuActionId::CloseTab => {
            Some(TabMenuDispatch::Topology(WindowTopologyCommand::CloseTab {
                dojo_id: context.dojo_id,
            }))
        }
        TabMenuActionId::CloseOtherTabs => Some(TabMenuDispatch::Topology(
            close_other_tabs_command(context.dojo_id, context.other_dojo_ids.clone())?,
        )),
        TabMenuActionId::TerminateDojo => {
            Some(TabMenuDispatch::ConfirmTermination(context.action_target()))
        }
    }
}

pub(crate) fn tab_menu_descriptor(id: TabMenuActionId) -> TabMenuActionDescriptor {
    const EXPLORER_ACTIONS: &[TabMenuActionDescriptor] = &[
        TabMenuActionDescriptor {
            id: TabMenuActionId::RenameLair,
            title: "Rename…",
        },
        TabMenuActionDescriptor {
            id: TabMenuActionId::SaveLayout,
            title: "Save layout",
        },
        TabMenuActionDescriptor {
            id: TabMenuActionId::PinLair,
            title: "Pin",
        },
        TabMenuActionDescriptor {
            id: TabMenuActionId::UnpinLair,
            title: "Unpin",
        },
        TabMenuActionDescriptor {
            id: TabMenuActionId::PreviewLair,
            title: "Preview saved layout",
        },
        TabMenuActionDescriptor {
            id: TabMenuActionId::Restore,
            title: "Restore…",
        },
        TabMenuActionDescriptor {
            id: TabMenuActionId::TerminateLair,
            title: "Terminate…",
        },
        TabMenuActionDescriptor {
            id: TabMenuActionId::FocusSplint,
            title: "Focus",
        },
        TabMenuActionDescriptor {
            id: TabMenuActionId::SplitBelow,
            title: "Split below",
        },
        TabMenuActionDescriptor {
            id: TabMenuActionId::SplitRight,
            title: "Split right",
        },
        TabMenuActionDescriptor {
            id: TabMenuActionId::CloseSplint,
            title: "Close",
        },
    ];
    TAB_MENU_ACTIONS
        .iter()
        .chain(EXPLORER_ACTIONS.iter())
        .copied()
        .find(|descriptor| descriptor.id == id)
        .expect("tab menu action identity has one descriptor")
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum BuiltInCommandId {
    ShowKeybindings,
    ReloadConfiguration,
    RecentSessions,
    NewSession,
    OpenRemote,
    RenameCurrentTab,
    NewDojo,
    ChooseDojo,
    PreviousDojo,
    NextDojo,
    MoveDojoLeft,
    MoveDojoRight,
    CloseCurrentTab,
    CloseOtherTabs,
    TerminateCurrentDojo,
    RestoreCurrentDojo,
    ChooseLair,
    RenameCurrentLair,
    SaveCurrentLair,
    PinCurrentLair,
    UnpinCurrentLair,
    PreviewCurrentLair,
    RestoreCurrentLair,
    PreviousLair,
    NextLair,
    TerminateCurrentLair,
    SplitHorizontal,
    SplitVertical,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    CloseFocusedPane,
    ClearAgentStatus,
    ResizePaneSmaller,
    ResizePaneLarger,
    EnterCopyMode,
    SaveClipboardImage,
    ToggleFocusedPaneZoom,
    SearchScrollback,
    PageUp,
    PageDown,
    ReturnToLive,
    ToggleTabStrip,
    ToggleLairExplorer,
    ToggleLairExplorerRail,
    FocusLairExplorer,
    ReturnFocusToTerminal,
    ZoomIn,
    ZoomOut,
    ResetZoom,
    DetachWindow,
    RequestControl,
    ReleaseControl,
    ForceControl,
    AgentAccess,
    RevokeAllAccess,
    AcceptControlTransfer,
    DenyControlTransfer,
}

impl BuiltInCommandId {
    pub(crate) const ALL: &[Self] = &[
        Self::ShowKeybindings,
        Self::ReloadConfiguration,
        Self::RecentSessions,
        Self::NewSession,
        Self::OpenRemote,
        Self::RenameCurrentTab,
        Self::NewDojo,
        Self::ChooseDojo,
        Self::PreviousDojo,
        Self::NextDojo,
        Self::MoveDojoLeft,
        Self::MoveDojoRight,
        Self::CloseCurrentTab,
        Self::CloseOtherTabs,
        Self::TerminateCurrentDojo,
        Self::RestoreCurrentDojo,
        Self::ChooseLair,
        Self::RenameCurrentLair,
        Self::SaveCurrentLair,
        Self::PinCurrentLair,
        Self::UnpinCurrentLair,
        Self::PreviewCurrentLair,
        Self::RestoreCurrentLair,
        Self::PreviousLair,
        Self::NextLair,
        Self::TerminateCurrentLair,
        Self::SplitHorizontal,
        Self::SplitVertical,
        Self::FocusLeft,
        Self::FocusRight,
        Self::FocusUp,
        Self::FocusDown,
        Self::CloseFocusedPane,
        Self::ClearAgentStatus,
        Self::ResizePaneSmaller,
        Self::ResizePaneLarger,
        Self::EnterCopyMode,
        Self::SaveClipboardImage,
        Self::ToggleFocusedPaneZoom,
        Self::SearchScrollback,
        Self::PageUp,
        Self::PageDown,
        Self::ReturnToLive,
        Self::ToggleTabStrip,
        Self::ToggleLairExplorer,
        Self::ToggleLairExplorerRail,
        Self::FocusLairExplorer,
        Self::ReturnFocusToTerminal,
        Self::ZoomIn,
        Self::ZoomOut,
        Self::ResetZoom,
        Self::DetachWindow,
        Self::RequestControl,
        Self::ReleaseControl,
        Self::ForceControl,
        Self::AgentAccess,
        Self::RevokeAllAccess,
        Self::AcceptControlTransfer,
        Self::DenyControlTransfer,
    ];
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum CommandCategory {
    Application,
    Dojos,
    Tabs,
    Panes,
    History,
    View,
    Control,
}

impl CommandCategory {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Application => "APP",
            Self::Dojos => "DOJO",
            Self::Tabs => "TAB",
            Self::Panes => "PANE",
            Self::History => "HISTORY",
            Self::View => "VIEW",
            Self::Control => "CONTROL",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandTabMoveAvailability {
    Neither,
    Left,
    Right,
    Both,
}

impl CommandTabMoveAvailability {
    pub(crate) const fn from_edges(can_move_left: bool, can_move_right: bool) -> Self {
        match (can_move_left, can_move_right) {
            (false, false) => Self::Neither,
            (true, false) => Self::Left,
            (false, true) => Self::Right,
            (true, true) => Self::Both,
        }
    }

    const fn can_move_left(self) -> bool {
        matches!(self, Self::Left | Self::Both)
    }

    const fn can_move_right(self) -> bool {
        matches!(self, Self::Right | Self::Both)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(
    clippy::struct_field_names,
    clippy::struct_excessive_bools,
    reason = "independent captured availability flags and the explicit identity suffixes keep domain identifiers and captured destinations unambiguous"
)]
pub(crate) struct CommandPaletteContext {
    pub(crate) lair_id: LairId,
    pub(crate) lair_retention: LairRetention,
    pub(crate) dojo_id: DojoId,
    pub(crate) dojo_name: String,
    pub(crate) pane_count: usize,
    pub(crate) splint_id: SplintId,
    pub(crate) dojo_splints: Vec<(SplintId, u64)>,
    pub(crate) other_dojo_ids: Vec<DojoId>,
    pub(crate) previous_dojo_id: Option<DojoId>,
    pub(crate) next_dojo_id: Option<DojoId>,
    pub(crate) tab_move: CommandTabMoveAvailability,
    pub(crate) focus_left: Option<SplintId>,
    pub(crate) focus_right: Option<SplintId>,
    pub(crate) focus_up: Option<SplintId>,
    pub(crate) focus_down: Option<SplintId>,
    pub(crate) clipboard_image_available: bool,
    pub(crate) viewport_detached: bool,
    pub(crate) controller_active: bool,
    pub(crate) forced_control_transfer: bool,
    /// Whether **Revoke all access** may have something to revoke: unknown
    /// (true) until the access inventory was read, then whether it lists a
    /// consent grant or a pending request.
    pub(crate) revoke_all_available: bool,
    /// Access requests waiting for the person's answer (ADR 0014).
    pub(crate) access_waiting: u32,
    pub(crate) pending_transfer_id: Option<u64>,
    /// Live incarnation of the focused Splint while it shows agent status.
    pub(crate) agent_status_incarnation: Option<u64>,
}

impl CommandPaletteContext {
    fn action_target(&self) -> DojoActionTarget {
        DojoActionTarget {
            dojo_id: self.dojo_id,
            name: self.dojo_name.clone(),
            pane_count: self.pane_count,
            splints: self.dojo_splints.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandZoomAction {
    Increase,
    Decrease,
    Reset,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandHistoryAction {
    Search,
    PageUp,
    PageDown,
    ReturnToLive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandControlAction {
    Request,
    Release,
    Force,
    Accept(u64),
    Deny(u64),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BuiltInCommandDispatch {
    Topology(WindowTopologyCommand),
    ShowKeybindings,
    ReloadConfiguration,
    EnterCopyMode,
    SaveClipboardImage,
    ToggleFocusedPaneZoom,
    MoveDojo {
        dojo_id: DojoId,
        delta: isize,
    },
    DetachWindow,
    Focus(SplintId),
    Zoom(CommandZoomAction),
    ToggleTabStrip,
    ToggleLairExplorer,
    /// Collapses the shown Explorer to its rail or expands it; shows a
    /// hidden one as its rail.
    ToggleLairExplorerRail,
    FocusLairExplorer,
    ReturnFocusToTerminal,
    History {
        target: SplintId,
        action: CommandHistoryAction,
    },
    Control {
        target: SplintId,
        action: CommandControlAction,
    },
    /// Opens the **Agent access** list.
    AgentAccess,
    Rename(DojoActionTarget),
    ConfirmTermination(DojoActionTarget),
    RecentSessions,
    OpenRemoteChooser,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BuiltInCommandDescriptor {
    pub(crate) id: BuiltInCommandId,
    pub(crate) category: CommandCategory,
    pub(crate) title: &'static str,
    pub(crate) keywords: &'static [&'static str],
    shortcut_action: Option<ActionId>,
}

pub(crate) fn action_search_metadata(
    action: ActionId,
) -> Option<(&'static str, &'static str, &'static [&'static str])> {
    if action == ActionId::ToggleCurrentLairPin {
        return Some((
            "Toggle current Lair pin",
            CommandCategory::Dojos.label(),
            &["pin", "unpin", "favorite", "protect", "lair", "workspace"],
        ));
    }
    BUILT_IN_COMMANDS
        .iter()
        .find(|descriptor| descriptor.shortcut_action == Some(action))
        .map(|descriptor| {
            (
                descriptor.title,
                descriptor.category.label(),
                descriptor.keywords,
            )
        })
}

pub(crate) fn lair_lifecycle_command(
    action: ActionId,
    lair_id: LairId,
    retention: LairRetention,
) -> Option<WindowTopologyCommand> {
    match action {
        ActionId::SaveCurrentLair if retention == LairRetention::Disposable => {
            Some(WindowTopologyCommand::SetLairRetention {
                lair_id,
                expected_retention: retention,
                retention: LairRetention::Saved,
            })
        }
        ActionId::ToggleCurrentLairPin => Some(WindowTopologyCommand::SetLairRetention {
            lair_id,
            expected_retention: retention,
            retention: if retention == LairRetention::Pinned {
                LairRetention::Saved
            } else {
                LairRetention::Pinned
            },
        }),
        ActionId::PreviewCurrentLair if retention.is_protected() => {
            Some(WindowTopologyCommand::RequestLairPrompt {
                lair_id,
                kind: LairPromptKind::Preview,
                expected_retention: Some(retention),
            })
        }
        ActionId::RestoreCurrentLair if retention.is_protected() => {
            Some(WindowTopologyCommand::RequestLairPrompt {
                lair_id,
                kind: LairPromptKind::Restore,
                expected_retention: Some(retention),
            })
        }
        _ => None,
    }
}

impl BuiltInCommandDescriptor {
    pub(crate) fn shortcut(self, keymap: &ResolvedKeymap) -> &str {
        self.shortcut_action
            .map_or("", |action| keymap.primary_shortcut(action))
    }
}

pub(crate) const BUILT_IN_COMMANDS: [BuiltInCommandDescriptor; BuiltInCommandId::ALL.len()] = [
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::SaveClipboardImage,
        category: CommandCategory::Application,
        title: "Save clipboard image and insert path",
        keywords: &["clipboard", "png", "image", "save", "path", "screenshot"],
        shortcut_action: Some(ActionId::ClipboardSaveImage),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::ShowKeybindings,
        category: CommandCategory::Application,
        title: "Show keybindings",
        keywords: &["show", "help", "keys", "keybindings", "shortcuts"],
        shortcut_action: Some(ActionId::BindingHelp),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::ReloadConfiguration,
        category: CommandCategory::Application,
        title: "Reload configuration",
        keywords: &["reload", "configuration", "config", "keymap"],
        shortcut_action: Some(ActionId::ConfigReload),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::RecentSessions,
        category: CommandCategory::Dojos,
        title: "Open recent Dojos",
        keywords: &["open", "recent", "dojo", "session", "lair", "reopen"],
        shortcut_action: Some(ActionId::RecentSessions),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::NewSession,
        category: CommandCategory::Dojos,
        title: "New terminal",
        keywords: &["new", "terminal", "dojo", "session", "lair", "window"],
        shortcut_action: Some(ActionId::NewSession),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::OpenRemote,
        category: CommandCategory::Dojos,
        title: "Open Remote…",
        keywords: &[
            "open", "remote", "ssh", "host", "server", "connect", "window",
        ],
        shortcut_action: None,
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::RenameCurrentTab,
        category: CommandCategory::Tabs,
        title: "Rename current tab",
        keywords: &["rename", "name", "current", "dojo", "tab"],
        shortcut_action: Some(ActionId::RenameCurrentTab),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::NewDojo,
        category: CommandCategory::Tabs,
        title: "New Dojo",
        keywords: &["new", "dojo", "tab"],
        shortcut_action: Some(ActionId::NewDojo),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::ChooseDojo,
        category: CommandCategory::Tabs,
        title: "Choose Dojo",
        keywords: &["choose", "select", "switch", "dojo", "tab"],
        shortcut_action: Some(ActionId::DojoChooser),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::PreviousDojo,
        category: CommandCategory::Tabs,
        title: "Previous Dojo",
        keywords: &["previous", "dojo", "tab", "left"],
        shortcut_action: Some(ActionId::PreviousDojo),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::NextDojo,
        category: CommandCategory::Tabs,
        title: "Next Dojo",
        keywords: &["next", "dojo", "tab", "right"],
        shortcut_action: Some(ActionId::NextDojo),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::MoveDojoLeft,
        category: CommandCategory::Tabs,
        title: "Move Dojo left",
        keywords: &["move", "reorder", "dojo", "tab", "left"],
        shortcut_action: Some(ActionId::MoveDojoLeft),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::MoveDojoRight,
        category: CommandCategory::Tabs,
        title: "Move Dojo right",
        keywords: &["move", "reorder", "dojo", "tab", "right"],
        shortcut_action: Some(ActionId::MoveDojoRight),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::CloseCurrentTab,
        category: CommandCategory::Tabs,
        title: "Close current tab",
        keywords: &["close", "detach", "current", "dojo", "tab"],
        shortcut_action: Some(ActionId::CloseCurrentTab),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::CloseOtherTabs,
        category: CommandCategory::Tabs,
        title: "Close other tabs",
        keywords: &["close", "detach", "other", "dojo", "tabs"],
        shortcut_action: Some(ActionId::CloseOtherTabs),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::TerminateCurrentDojo,
        category: CommandCategory::Tabs,
        title: "Terminate current Dojo…",
        keywords: &["terminate", "kill", "close", "current", "dojo", "panes"],
        shortcut_action: Some(ActionId::TerminateCurrentDojo),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::RestoreCurrentDojo,
        category: CommandCategory::Tabs,
        title: "Restore current Dojo…",
        keywords: &["restore", "launch", "current", "dojo", "tab"],
        shortcut_action: None,
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::ChooseLair,
        category: CommandCategory::Dojos,
        title: "Choose Lair",
        keywords: &["choose", "select", "switch", "lair", "workspace"],
        shortcut_action: Some(ActionId::LairChooser),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::RenameCurrentLair,
        category: CommandCategory::Dojos,
        title: "Rename current Lair",
        keywords: &["rename", "name", "current", "lair", "workspace"],
        shortcut_action: Some(ActionId::RenameCurrentLair),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::SaveCurrentLair,
        category: CommandCategory::Dojos,
        title: "Save current Lair layout",
        keywords: &["save", "retain", "lair", "layout", "workspace"],
        shortcut_action: Some(ActionId::SaveCurrentLair),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::PinCurrentLair,
        category: CommandCategory::Dojos,
        title: "Pin current Lair",
        keywords: &["pin", "protect", "lair", "workspace"],
        shortcut_action: Some(ActionId::ToggleCurrentLairPin),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::UnpinCurrentLair,
        category: CommandCategory::Dojos,
        title: "Unpin current Lair",
        keywords: &["unpin", "saved", "lair", "workspace"],
        shortcut_action: Some(ActionId::ToggleCurrentLairPin),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::PreviewCurrentLair,
        category: CommandCategory::Dojos,
        title: "Preview saved Lair layout",
        keywords: &["preview", "saved", "lair", "layout", "recipe"],
        shortcut_action: Some(ActionId::PreviewCurrentLair),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::RestoreCurrentLair,
        category: CommandCategory::Dojos,
        title: "Restore saved Lair…",
        keywords: &["restore", "launch", "saved", "lair", "layout"],
        shortcut_action: Some(ActionId::RestoreCurrentLair),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::PreviousLair,
        category: CommandCategory::Dojos,
        title: "Previous Lair",
        keywords: &["previous", "switch", "lair", "workspace"],
        shortcut_action: Some(ActionId::PreviousLair),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::NextLair,
        category: CommandCategory::Dojos,
        title: "Next Lair",
        keywords: &["next", "switch", "lair", "workspace"],
        shortcut_action: Some(ActionId::NextLair),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::TerminateCurrentLair,
        category: CommandCategory::Dojos,
        title: "Terminate current Lair…",
        keywords: &["terminate", "kill", "close", "current", "lair", "workspace"],
        shortcut_action: Some(ActionId::TerminateCurrentLair),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::SplitHorizontal,
        category: CommandCategory::Panes,
        title: "Split pane horizontally",
        keywords: &["split", "pane", "horizontal", "down"],
        shortcut_action: Some(ActionId::SplitHorizontal),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::SplitVertical,
        category: CommandCategory::Panes,
        title: "Split pane vertically",
        keywords: &["split", "pane", "vertical", "right"],
        shortcut_action: Some(ActionId::SplitVertical),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::FocusLeft,
        category: CommandCategory::Panes,
        title: "Focus pane left",
        keywords: &["focus", "pane", "move", "left"],
        shortcut_action: Some(ActionId::FocusLeft),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::FocusRight,
        category: CommandCategory::Panes,
        title: "Focus pane right",
        keywords: &["focus", "pane", "move", "right"],
        shortcut_action: Some(ActionId::FocusRight),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::FocusUp,
        category: CommandCategory::Panes,
        title: "Focus pane up",
        keywords: &["focus", "pane", "move", "up"],
        shortcut_action: Some(ActionId::FocusUp),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::FocusDown,
        category: CommandCategory::Panes,
        title: "Focus pane down",
        keywords: &["focus", "pane", "move", "down"],
        shortcut_action: Some(ActionId::FocusDown),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::CloseFocusedPane,
        category: CommandCategory::Panes,
        title: "Close focused pane",
        keywords: &["close", "kill", "focused", "pane", "shell"],
        shortcut_action: Some(ActionId::CloseFocusedPane),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::ClearAgentStatus,
        category: CommandCategory::Panes,
        title: "Clear agent status",
        keywords: &["clear", "agent", "status", "badge", "reset", "pane"],
        shortcut_action: None,
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::ResizePaneSmaller,
        category: CommandCategory::Panes,
        title: "Resize pane smaller",
        keywords: &["resize", "pane", "ratio", "smaller", "decrease"],
        shortcut_action: Some(ActionId::ResizePaneSmaller),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::ResizePaneLarger,
        category: CommandCategory::Panes,
        title: "Resize pane larger",
        keywords: &["resize", "pane", "ratio", "larger", "increase"],
        shortcut_action: Some(ActionId::ResizePaneLarger),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::EnterCopyMode,
        category: CommandCategory::History,
        title: "Enter copy mode",
        keywords: &["enter", "copy", "mode", "history", "keyboard"],
        shortcut_action: Some(ActionId::CopyModeEnter),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::ToggleFocusedPaneZoom,
        category: CommandCategory::Panes,
        title: "Toggle focused pane zoom",
        keywords: &["toggle", "focused", "pane", "zoom", "maximize"],
        shortcut_action: Some(ActionId::TogglePaneZoom),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::SearchScrollback,
        category: CommandCategory::History,
        title: "Search scrollback",
        keywords: &["search", "find", "history", "scrollback"],
        shortcut_action: Some(ActionId::SearchScrollback),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::PageUp,
        category: CommandCategory::History,
        title: "Page up",
        keywords: &["history", "scrollback", "page", "up", "older"],
        shortcut_action: Some(ActionId::PageUp),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::PageDown,
        category: CommandCategory::History,
        title: "Page down",
        keywords: &["history", "scrollback", "page", "down", "newer"],
        shortcut_action: Some(ActionId::PageDown),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::ReturnToLive,
        category: CommandCategory::History,
        title: "Return to live output",
        keywords: &["history", "scrollback", "return", "live", "bottom"],
        shortcut_action: Some(ActionId::ReturnToLive),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::ToggleTabStrip,
        category: CommandCategory::View,
        title: "Toggle Dojo tab strip",
        keywords: &["toggle", "show", "hide", "dojo", "tabs", "strip", "view"],
        shortcut_action: Some(ActionId::ToggleTabStrip),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::ToggleLairExplorer,
        category: CommandCategory::View,
        title: "Toggle Lair explorer",
        keywords: &["toggle", "show", "hide", "lair", "explorer", "tree", "view"],
        shortcut_action: None,
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::ToggleLairExplorerRail,
        category: CommandCategory::View,
        title: "Toggle Lair explorer rail",
        keywords: &[
            "toggle", "collapse", "expand", "rail", "narrow", "lair", "explorer", "view",
        ],
        shortcut_action: None,
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::FocusLairExplorer,
        category: CommandCategory::View,
        title: "Focus Lair explorer",
        keywords: &["focus", "lair", "explorer", "tree", "navigation"],
        shortcut_action: None,
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::ReturnFocusToTerminal,
        category: CommandCategory::View,
        title: "Return focus to terminal",
        keywords: &["focus", "return", "terminal", "explorer", "navigation"],
        shortcut_action: None,
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::ZoomIn,
        category: CommandCategory::View,
        title: "Zoom in",
        keywords: &["zoom", "font", "increase", "larger", "view"],
        shortcut_action: Some(ActionId::ZoomIn),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::ZoomOut,
        category: CommandCategory::View,
        title: "Zoom out",
        keywords: &["zoom", "font", "decrease", "smaller", "view"],
        shortcut_action: Some(ActionId::ZoomOut),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::ResetZoom,
        category: CommandCategory::View,
        title: "Reset zoom",
        keywords: &["zoom", "font", "reset", "default", "view"],
        shortcut_action: Some(ActionId::ResetZoom),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::DetachWindow,
        category: CommandCategory::View,
        title: "Detach Window",
        keywords: &["detach", "close", "window", "leave", "keep", "running"],
        shortcut_action: Some(ActionId::DetachWindow),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::RequestControl,
        category: CommandCategory::Control,
        title: "Request control",
        keywords: &["request", "control", "transfer", "input"],
        shortcut_action: Some(ActionId::RequestControl),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::ReleaseControl,
        category: CommandCategory::Control,
        title: "Release control",
        keywords: &["release", "control", "transfer", "input"],
        shortcut_action: Some(ActionId::ReleaseControl),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::ForceControl,
        category: CommandCategory::Control,
        title: "Force control transfer",
        keywords: &["force", "control", "transfer", "input"],
        shortcut_action: Some(ActionId::ForceControl),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::AgentAccess,
        category: CommandCategory::Control,
        title: "Agent access",
        keywords: &[
            "agent", "access", "grants", "list", "revoke", "waiting", "pending", "request", "mcp",
        ],
        shortcut_action: None,
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::RevokeAllAccess,
        category: CommandCategory::Control,
        title: "Revoke all access",
        keywords: &["revoke", "all", "access", "grants", "clients"],
        shortcut_action: Some(ActionId::RevokeAllAccess),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::AcceptControlTransfer,
        category: CommandCategory::Control,
        title: "Accept pending control transfer",
        keywords: &["accept", "pending", "control", "transfer", "yes"],
        shortcut_action: Some(ActionId::AcceptControlTransfer),
    },
    BuiltInCommandDescriptor {
        id: BuiltInCommandId::DenyControlTransfer,
        category: CommandCategory::Control,
        title: "Deny pending control transfer",
        keywords: &["deny", "pending", "control", "transfer", "no"],
        shortcut_action: Some(ActionId::DenyControlTransfer),
    },
];

fn lifecycle_command_visible(id: BuiltInCommandId, retention: LairRetention) -> bool {
    match id {
        BuiltInCommandId::SaveCurrentLair => retention == LairRetention::Disposable,
        BuiltInCommandId::PinCurrentLair => retention != LairRetention::Pinned,
        BuiltInCommandId::UnpinCurrentLair => retention == LairRetention::Pinned,
        BuiltInCommandId::PreviewCurrentLair | BuiltInCommandId::RestoreCurrentLair => {
            retention.is_protected()
        }
        _ => true,
    }
}

fn descriptor_matches(descriptor: BuiltInCommandDescriptor, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let query = query.to_lowercase();
    descriptor.title.to_lowercase().contains(&query)
        || descriptor.category.label().to_lowercase().contains(&query)
        || descriptor
            .keywords
            .iter()
            .any(|keyword| keyword.contains(&query))
}

/// Whether a palette command is disabled while the Window is paused for a
/// daemon handoff (Plan 0061 M3.12a): it changes the session, opens a prompt
/// that only leads to such a change, attaches another Lair's panes, takes or
/// grants control, or sends terminal input. Browsing, previews, help, focus,
/// zoom, history, tab switching, and releases keep working.
pub(crate) const fn command_paused_during_handoff(id: BuiltInCommandId) -> bool {
    match id {
        BuiltInCommandId::NewSession
        | BuiltInCommandId::RenameCurrentTab
        | BuiltInCommandId::NewDojo
        | BuiltInCommandId::CloseCurrentTab
        | BuiltInCommandId::CloseOtherTabs
        | BuiltInCommandId::TerminateCurrentDojo
        | BuiltInCommandId::RestoreCurrentDojo
        | BuiltInCommandId::RenameCurrentLair
        | BuiltInCommandId::SaveCurrentLair
        | BuiltInCommandId::PinCurrentLair
        | BuiltInCommandId::UnpinCurrentLair
        | BuiltInCommandId::RestoreCurrentLair
        | BuiltInCommandId::PreviousLair
        | BuiltInCommandId::NextLair
        | BuiltInCommandId::TerminateCurrentLair
        | BuiltInCommandId::SplitHorizontal
        | BuiltInCommandId::SplitVertical
        | BuiltInCommandId::CloseFocusedPane
        | BuiltInCommandId::ClearAgentStatus
        | BuiltInCommandId::ResizePaneSmaller
        | BuiltInCommandId::ResizePaneLarger
        | BuiltInCommandId::SaveClipboardImage
        | BuiltInCommandId::RequestControl
        | BuiltInCommandId::ForceControl
        | BuiltInCommandId::AcceptControlTransfer
        | BuiltInCommandId::DenyControlTransfer => true,
        BuiltInCommandId::ShowKeybindings
        | BuiltInCommandId::ReloadConfiguration
        | BuiltInCommandId::RecentSessions
        | BuiltInCommandId::OpenRemote
        | BuiltInCommandId::ChooseDojo
        | BuiltInCommandId::PreviousDojo
        | BuiltInCommandId::NextDojo
        | BuiltInCommandId::MoveDojoLeft
        | BuiltInCommandId::MoveDojoRight
        | BuiltInCommandId::ChooseLair
        | BuiltInCommandId::PreviewCurrentLair
        | BuiltInCommandId::FocusLeft
        | BuiltInCommandId::FocusRight
        | BuiltInCommandId::FocusUp
        | BuiltInCommandId::FocusDown
        | BuiltInCommandId::EnterCopyMode
        | BuiltInCommandId::ToggleFocusedPaneZoom
        | BuiltInCommandId::SearchScrollback
        | BuiltInCommandId::PageUp
        | BuiltInCommandId::PageDown
        | BuiltInCommandId::ReturnToLive
        | BuiltInCommandId::ToggleTabStrip
        | BuiltInCommandId::ToggleLairExplorer
        | BuiltInCommandId::ToggleLairExplorerRail
        | BuiltInCommandId::FocusLairExplorer
        | BuiltInCommandId::ReturnFocusToTerminal
        | BuiltInCommandId::ZoomIn
        | BuiltInCommandId::ZoomOut
        | BuiltInCommandId::ResetZoom
        | BuiltInCommandId::DetachWindow
        | BuiltInCommandId::ReleaseControl
        | BuiltInCommandId::AgentAccess
        | BuiltInCommandId::RevokeAllAccess => false,
    }
}

/// Whether a tab-strip menu action is disabled during a daemon handoff;
/// Explorer menus classify their captured topology command instead.
pub(crate) const fn tab_menu_action_paused_during_handoff(id: TabMenuActionId) -> bool {
    !matches!(id, TabMenuActionId::ActivateTab)
}

pub(crate) fn command_enabled(id: BuiltInCommandId, context: &CommandPaletteContext) -> bool {
    match id {
        BuiltInCommandId::SaveClipboardImage => context.clipboard_image_available,
        BuiltInCommandId::PreviousDojo => context.previous_dojo_id.is_some(),
        BuiltInCommandId::NextDojo => context.next_dojo_id.is_some(),
        BuiltInCommandId::MoveDojoLeft => context.tab_move.can_move_left(),
        BuiltInCommandId::SaveCurrentLair => context.lair_retention == LairRetention::Disposable,
        BuiltInCommandId::PinCurrentLair => context.lair_retention != LairRetention::Pinned,
        BuiltInCommandId::UnpinCurrentLair => context.lair_retention == LairRetention::Pinned,
        BuiltInCommandId::PreviewCurrentLair | BuiltInCommandId::RestoreCurrentLair => {
            context.lair_retention.is_protected()
        }
        BuiltInCommandId::MoveDojoRight => context.tab_move.can_move_right(),
        BuiltInCommandId::FocusLeft => context.focus_left.is_some(),
        BuiltInCommandId::FocusRight => context.focus_right.is_some(),
        BuiltInCommandId::FocusUp => context.focus_up.is_some(),
        BuiltInCommandId::FocusDown => context.focus_down.is_some(),
        BuiltInCommandId::CloseOtherTabs => !context.other_dojo_ids.is_empty(),
        BuiltInCommandId::ToggleFocusedPaneZoom => context.pane_count > 1,
        BuiltInCommandId::ReturnToLive => context.viewport_detached,
        BuiltInCommandId::RequestControl => !context.controller_active,
        BuiltInCommandId::ForceControl => {
            context.forced_control_transfer && !context.controller_active
        }
        BuiltInCommandId::ReleaseControl => context.controller_active,
        BuiltInCommandId::RevokeAllAccess => context.revoke_all_available,
        BuiltInCommandId::ClearAgentStatus => context.agent_status_incarnation.is_some(),
        BuiltInCommandId::AcceptControlTransfer | BuiltInCommandId::DenyControlTransfer => {
            context.pending_transfer_id.is_some()
        }
        BuiltInCommandId::ShowKeybindings
        | BuiltInCommandId::ReloadConfiguration
        | BuiltInCommandId::RecentSessions
        | BuiltInCommandId::OpenRemote
        | BuiltInCommandId::NewSession
        | BuiltInCommandId::RenameCurrentTab
        | BuiltInCommandId::NewDojo
        | BuiltInCommandId::ChooseDojo
        | BuiltInCommandId::CloseCurrentTab
        | BuiltInCommandId::TerminateCurrentDojo
        | BuiltInCommandId::RestoreCurrentDojo
        | BuiltInCommandId::ChooseLair
        | BuiltInCommandId::RenameCurrentLair
        | BuiltInCommandId::PreviousLair
        | BuiltInCommandId::NextLair
        | BuiltInCommandId::TerminateCurrentLair
        | BuiltInCommandId::SplitHorizontal
        | BuiltInCommandId::SplitVertical
        | BuiltInCommandId::CloseFocusedPane
        | BuiltInCommandId::ResizePaneSmaller
        | BuiltInCommandId::ResizePaneLarger
        | BuiltInCommandId::EnterCopyMode
        | BuiltInCommandId::SearchScrollback
        | BuiltInCommandId::PageUp
        | BuiltInCommandId::PageDown
        | BuiltInCommandId::ToggleTabStrip
        | BuiltInCommandId::ToggleLairExplorer
        | BuiltInCommandId::ToggleLairExplorerRail
        | BuiltInCommandId::FocusLairExplorer
        | BuiltInCommandId::ReturnFocusToTerminal
        | BuiltInCommandId::ZoomIn
        | BuiltInCommandId::ZoomOut
        | BuiltInCommandId::ResetZoom
        | BuiltInCommandId::DetachWindow
        | BuiltInCommandId::AgentAccess => true,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommandPaletteUi {
    context: CommandPaletteContext,
    editor: BoundedTextEditor,
    filtered: Vec<BuiltInCommandId>,
    selected: usize,
    visible_start: usize,
    hovered: Option<BuiltInCommandId>,
    /// The Window is paused for a daemon handoff: daemon-mutating commands
    /// are shown disabled (Plan 0061 M3.12a).
    mutations_paused: bool,
}

impl CommandPaletteUi {
    pub(crate) fn new(context: CommandPaletteContext) -> Self {
        let lair_retention = context.lair_retention;
        Self {
            context,
            editor: BoundedTextEditor::new(
                String::new(),
                MAX_QUERY_BYTES,
                MAX_QUERY_SCALARS,
                false,
            ),
            filtered: BUILT_IN_COMMANDS
                .iter()
                .filter(|command| lifecycle_command_visible(command.id, lair_retention))
                .map(|command| command.id)
                .collect(),
            selected: 0,
            visible_start: 0,
            hovered: None,
            mutations_paused: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn context(&self) -> CommandPaletteContext {
        self.context.clone()
    }

    pub(crate) fn query(&self) -> &str {
        self.editor.text()
    }

    pub(crate) fn editor_mut(&mut self) -> &mut BoundedTextEditor {
        &mut self.editor
    }

    pub(crate) fn filtered(&self) -> &[BuiltInCommandId] {
        &self.filtered
    }

    pub(crate) const fn selected_index(&self) -> usize {
        self.selected
    }

    pub(crate) const fn visible_start(&self) -> usize {
        self.visible_start
    }

    pub(crate) fn selected_command(&self) -> Option<BuiltInCommandId> {
        self.filtered.get(self.selected).copied()
    }

    pub(crate) fn selected_enabled_command(&self) -> Option<BuiltInCommandId> {
        self.selected_command()
            .filter(|command| self.command_enabled(*command))
    }

    pub(crate) fn command_enabled(&self, command: BuiltInCommandId) -> bool {
        command_enabled(command, &self.context)
            && !(self.mutations_paused && command_paused_during_handoff(command))
    }

    /// The typed dispatch for an enabled command, under this palette's
    /// current pause state as well as its captured context.
    pub(crate) fn dispatch(&self, command: BuiltInCommandId) -> Option<BuiltInCommandDispatch> {
        if !self.command_enabled(command) {
            return None;
        }
        command_dispatch(command, &self.context)
    }

    /// Disables or re-enables daemon-mutating commands for a daemon handoff,
    /// keeping the selection on an enabled command. Returns whether it changed.
    pub(crate) fn set_mutations_paused(&mut self, paused: bool) -> bool {
        if self.mutations_paused == paused {
            return false;
        }
        self.mutations_paused = paused;
        self.refilter();
        true
    }

    pub(crate) const fn hovered(&self) -> Option<BuiltInCommandId> {
        self.hovered
    }

    /// Live text a row shows in its shortcut column instead: **Agent access**
    /// says how many requests wait for an answer, so a queued prompt is
    /// visible from the palette (ADR 0014).
    pub(crate) fn badge(&self, command: BuiltInCommandId) -> Option<String> {
        (command == BuiltInCommandId::AgentAccess && self.context.access_waiting > 0)
            .then(|| format!("{} waiting", self.context.access_waiting))
    }

    pub(crate) fn update_hovered(&mut self, hovered: Option<BuiltInCommandId>) -> bool {
        let hovered = hovered.filter(|command| self.command_enabled(*command));
        if self.hovered == hovered {
            return false;
        }
        self.hovered = hovered;
        true
    }

    pub(crate) fn move_selection(&mut self, delta: isize) -> bool {
        let count = self.filtered.len();
        let enabled_count = self
            .filtered
            .iter()
            .filter(|command| self.command_enabled(**command))
            .count();
        if count == 0 || enabled_count == 0 || delta == 0 {
            return false;
        }
        let previous = self.selected;
        let steps = delta.unsigned_abs() % enabled_count;
        for _ in 0..steps {
            for distance in 1..=count {
                let candidate = if delta.is_negative() {
                    self.selected.saturating_add(count).saturating_sub(distance) % count
                } else {
                    self.selected.saturating_add(distance) % count
                };
                if self.command_enabled(self.filtered[candidate]) {
                    self.selected = candidate;
                    break;
                }
            }
        }
        self.ensure_selected_visible(COMMAND_PALETTE_PAGE_ITEMS);
        self.selected != previous
    }

    pub(crate) fn select_first(&mut self) -> bool {
        let Some(first) = self
            .filtered
            .iter()
            .position(|command| self.command_enabled(*command))
        else {
            return false;
        };
        let changed = self.selected != first || self.visible_start != 0;
        self.selected = first;
        self.visible_start = 0;
        changed
    }

    pub(crate) fn select_last(&mut self) -> bool {
        let Some(last) = self
            .filtered
            .iter()
            .rposition(|command| self.command_enabled(*command))
        else {
            return false;
        };
        let changed = self.selected != last;
        self.selected = last;
        self.ensure_selected_visible(COMMAND_PALETTE_PAGE_ITEMS);
        changed
    }

    pub(crate) fn append_text(&mut self, text: &str) -> bool {
        if !self.editor.insert(text) {
            return false;
        }
        self.refilter();
        true
    }

    pub(crate) fn backspace(&mut self) -> bool {
        if !self.editor.backspace() {
            return false;
        }
        self.refilter();
        true
    }

    pub(crate) fn editor_changed(&mut self) {
        self.refilter();
    }

    fn refilter(&mut self) {
        let selected = self.selected_command();
        self.filtered = BUILT_IN_COMMANDS
            .iter()
            .copied()
            .filter(|descriptor| {
                lifecycle_command_visible(descriptor.id, self.context.lair_retention)
            })
            .filter(|descriptor| descriptor_matches(*descriptor, self.editor.text()))
            .map(|descriptor| descriptor.id)
            .collect();
        self.selected = selected
            .filter(|selected| self.command_enabled(*selected))
            .and_then(|selected| {
                self.filtered
                    .iter()
                    .position(|candidate| *candidate == selected)
            })
            .or_else(|| {
                self.filtered
                    .iter()
                    .position(|command| self.command_enabled(*command))
            })
            .unwrap_or(0);
        self.ensure_selected_visible(COMMAND_PALETTE_PAGE_ITEMS);
        self.hovered = None;
    }

    fn ensure_selected_visible(&mut self, capacity: usize) {
        if self.filtered.is_empty() || capacity == 0 {
            self.selected = 0;
            self.visible_start = 0;
            return;
        }
        self.selected = self.selected.min(self.filtered.len() - 1);
        if self.selected < self.visible_start {
            self.visible_start = self.selected;
        } else if self.selected >= self.visible_start.saturating_add(capacity) {
            self.visible_start = self.selected.saturating_add(1).saturating_sub(capacity);
        }
        self.visible_start = self
            .visible_start
            .min(self.filtered.len().saturating_sub(capacity));
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the closed built-in catalog keeps availability and typed dispatch visibly exhaustive"
)]
pub(crate) fn command_dispatch(
    id: BuiltInCommandId,
    context: &CommandPaletteContext,
) -> Option<BuiltInCommandDispatch> {
    if !command_enabled(id, context) {
        return None;
    }
    let dispatch = match id {
        BuiltInCommandId::SaveClipboardImage => BuiltInCommandDispatch::SaveClipboardImage,
        BuiltInCommandId::ShowKeybindings => BuiltInCommandDispatch::ShowKeybindings,
        BuiltInCommandId::ReloadConfiguration => BuiltInCommandDispatch::ReloadConfiguration,
        BuiltInCommandId::RecentSessions => BuiltInCommandDispatch::RecentSessions,
        BuiltInCommandId::OpenRemote => BuiltInCommandDispatch::OpenRemoteChooser,
        BuiltInCommandId::NewSession => {
            BuiltInCommandDispatch::Topology(WindowTopologyCommand::NewLair {
                source_splint_id: context.splint_id,
            })
        }
        BuiltInCommandId::RenameCurrentTab => {
            BuiltInCommandDispatch::Rename(context.action_target())
        }
        BuiltInCommandId::NewDojo => {
            BuiltInCommandDispatch::Topology(WindowTopologyCommand::NewDojo {
                lair_id: context.lair_id,
                source_splint_id: context.splint_id,
            })
        }
        BuiltInCommandId::ChooseDojo => {
            BuiltInCommandDispatch::Topology(WindowTopologyCommand::RequestSelector {
                kind: SelectorKind::Dojo,
                lair_id: context.lair_id,
            })
        }
        BuiltInCommandId::PreviousDojo => {
            BuiltInCommandDispatch::Topology(WindowTopologyCommand::ActivateTab {
                dojo_id: context.previous_dojo_id?,
            })
        }
        BuiltInCommandId::NextDojo => {
            BuiltInCommandDispatch::Topology(WindowTopologyCommand::ActivateTab {
                dojo_id: context.next_dojo_id?,
            })
        }
        BuiltInCommandId::MoveDojoLeft => BuiltInCommandDispatch::MoveDojo {
            dojo_id: context.dojo_id,
            delta: -1,
        },
        BuiltInCommandId::MoveDojoRight => BuiltInCommandDispatch::MoveDojo {
            dojo_id: context.dojo_id,
            delta: 1,
        },
        BuiltInCommandId::CloseCurrentTab => {
            BuiltInCommandDispatch::Topology(WindowTopologyCommand::CloseTab {
                dojo_id: context.dojo_id,
            })
        }
        BuiltInCommandId::CloseOtherTabs => BuiltInCommandDispatch::Topology(
            close_other_tabs_command(context.dojo_id, context.other_dojo_ids.clone())?,
        ),
        BuiltInCommandId::TerminateCurrentDojo => {
            BuiltInCommandDispatch::ConfirmTermination(context.action_target())
        }
        BuiltInCommandId::RestoreCurrentDojo => {
            BuiltInCommandDispatch::Topology(WindowTopologyCommand::RequestDojoRestorePrompt {
                dojo_id: context.dojo_id,
            })
        }
        BuiltInCommandId::ChooseLair => {
            BuiltInCommandDispatch::Topology(WindowTopologyCommand::RequestSelector {
                kind: SelectorKind::Lair,
                lair_id: context.lair_id,
            })
        }
        BuiltInCommandId::RenameCurrentLair => {
            BuiltInCommandDispatch::Topology(WindowTopologyCommand::RequestLairPrompt {
                lair_id: context.lair_id,
                kind: LairPromptKind::Rename,
                expected_retention: None,
            })
        }
        BuiltInCommandId::SaveCurrentLair => BuiltInCommandDispatch::Topology(
            lair_lifecycle_command(
                ActionId::SaveCurrentLair,
                context.lair_id,
                context.lair_retention,
            )
            .expect("enabled save action has one lifecycle command"),
        ),
        BuiltInCommandId::PinCurrentLair | BuiltInCommandId::UnpinCurrentLair => {
            BuiltInCommandDispatch::Topology(
                lair_lifecycle_command(
                    ActionId::ToggleCurrentLairPin,
                    context.lair_id,
                    context.lair_retention,
                )
                .expect("enabled pin-toggle action has one lifecycle command"),
            )
        }
        BuiltInCommandId::PreviewCurrentLair => BuiltInCommandDispatch::Topology(
            lair_lifecycle_command(
                ActionId::PreviewCurrentLair,
                context.lair_id,
                context.lair_retention,
            )
            .expect("enabled preview action has one lifecycle command"),
        ),
        BuiltInCommandId::RestoreCurrentLair => BuiltInCommandDispatch::Topology(
            lair_lifecycle_command(
                ActionId::RestoreCurrentLair,
                context.lair_id,
                context.lair_retention,
            )
            .expect("enabled restore action has one lifecycle command"),
        ),
        BuiltInCommandId::PreviousLair => {
            BuiltInCommandDispatch::Topology(WindowTopologyCommand::NavigateLair {
                current_lair_id: context.lair_id,
                direction: LairDirection::Previous,
            })
        }
        BuiltInCommandId::NextLair => {
            BuiltInCommandDispatch::Topology(WindowTopologyCommand::NavigateLair {
                current_lair_id: context.lair_id,
                direction: LairDirection::Next,
            })
        }
        BuiltInCommandId::TerminateCurrentLair => {
            BuiltInCommandDispatch::Topology(WindowTopologyCommand::RequestLairPrompt {
                lair_id: context.lair_id,
                kind: LairPromptKind::Terminate,
                expected_retention: None,
            })
        }
        // Command orientation names the divider; core axes name pane placement.
        BuiltInCommandId::SplitHorizontal => {
            BuiltInCommandDispatch::Topology(WindowTopologyCommand::Split {
                dojo_id: context.dojo_id,
                target: context.splint_id,
                axis: Axis::Vertical,
                pending: None,
            })
        }
        BuiltInCommandId::SplitVertical => {
            BuiltInCommandDispatch::Topology(WindowTopologyCommand::Split {
                dojo_id: context.dojo_id,
                target: context.splint_id,
                axis: Axis::Horizontal,
                pending: None,
            })
        }
        BuiltInCommandId::FocusLeft => BuiltInCommandDispatch::Focus(context.focus_left?),
        BuiltInCommandId::FocusRight => BuiltInCommandDispatch::Focus(context.focus_right?),
        BuiltInCommandId::FocusUp => BuiltInCommandDispatch::Focus(context.focus_up?),
        BuiltInCommandId::FocusDown => BuiltInCommandDispatch::Focus(context.focus_down?),
        BuiltInCommandId::CloseFocusedPane => {
            BuiltInCommandDispatch::Topology(WindowTopologyCommand::Close {
                dojo_id: context.dojo_id,
                target: context.splint_id,
            })
        }
        BuiltInCommandId::ResizePaneSmaller => {
            BuiltInCommandDispatch::Topology(WindowTopologyCommand::AdjustRatio {
                dojo_id: context.dojo_id,
                target: context.splint_id,
                delta: -50,
            })
        }
        BuiltInCommandId::ResizePaneLarger => {
            BuiltInCommandDispatch::Topology(WindowTopologyCommand::AdjustRatio {
                dojo_id: context.dojo_id,
                target: context.splint_id,
                delta: 50,
            })
        }
        BuiltInCommandId::EnterCopyMode => BuiltInCommandDispatch::EnterCopyMode,
        BuiltInCommandId::ToggleFocusedPaneZoom => BuiltInCommandDispatch::ToggleFocusedPaneZoom,
        BuiltInCommandId::SearchScrollback => BuiltInCommandDispatch::History {
            target: context.splint_id,
            action: CommandHistoryAction::Search,
        },
        BuiltInCommandId::PageUp => BuiltInCommandDispatch::History {
            target: context.splint_id,
            action: CommandHistoryAction::PageUp,
        },
        BuiltInCommandId::PageDown => BuiltInCommandDispatch::History {
            target: context.splint_id,
            action: CommandHistoryAction::PageDown,
        },
        BuiltInCommandId::ReturnToLive => BuiltInCommandDispatch::History {
            target: context.splint_id,
            action: CommandHistoryAction::ReturnToLive,
        },
        BuiltInCommandId::ToggleTabStrip => BuiltInCommandDispatch::ToggleTabStrip,
        BuiltInCommandId::ToggleLairExplorer => BuiltInCommandDispatch::ToggleLairExplorer,
        BuiltInCommandId::ToggleLairExplorerRail => BuiltInCommandDispatch::ToggleLairExplorerRail,
        BuiltInCommandId::FocusLairExplorer => BuiltInCommandDispatch::FocusLairExplorer,
        BuiltInCommandId::ReturnFocusToTerminal => BuiltInCommandDispatch::ReturnFocusToTerminal,
        BuiltInCommandId::ZoomIn => BuiltInCommandDispatch::Zoom(CommandZoomAction::Increase),
        BuiltInCommandId::ZoomOut => BuiltInCommandDispatch::Zoom(CommandZoomAction::Decrease),
        BuiltInCommandId::ResetZoom => BuiltInCommandDispatch::Zoom(CommandZoomAction::Reset),
        BuiltInCommandId::DetachWindow => BuiltInCommandDispatch::DetachWindow,
        BuiltInCommandId::RequestControl => BuiltInCommandDispatch::Control {
            target: context.splint_id,
            action: CommandControlAction::Request,
        },
        BuiltInCommandId::ReleaseControl => BuiltInCommandDispatch::Control {
            target: context.splint_id,
            action: CommandControlAction::Release,
        },
        BuiltInCommandId::ForceControl => BuiltInCommandDispatch::Control {
            target: context.splint_id,
            action: CommandControlAction::Force,
        },
        // The trusted UI may only clear a status, never set one.
        BuiltInCommandId::ClearAgentStatus => {
            BuiltInCommandDispatch::Topology(WindowTopologyCommand::ClearAgentStatus {
                splint_id: context.splint_id,
                incarnation: context.agent_status_incarnation?,
            })
        }
        BuiltInCommandId::AgentAccess => BuiltInCommandDispatch::AgentAccess,
        // Counts first: the person confirms the exact numbers a dry run
        // reports, and the daemon revokes every duration (ADR 0014).
        BuiltInCommandId::RevokeAllAccess => BuiltInCommandDispatch::Topology(
            WindowTopologyCommand::Access(WindowAccessCommand::CountRevokeAll),
        ),
        BuiltInCommandId::AcceptControlTransfer => BuiltInCommandDispatch::Control {
            target: context.splint_id,
            action: CommandControlAction::Accept(context.pending_transfer_id?),
        },
        BuiltInCommandId::DenyControlTransfer => BuiltInCommandDispatch::Control {
            target: context.splint_id,
            action: CommandControlAction::Deny(context.pending_transfer_id?),
        },
    };
    Some(dispatch)
}

pub(crate) fn command_descriptor(id: BuiltInCommandId) -> BuiltInCommandDescriptor {
    BUILT_IN_COMMANDS
        .iter()
        .copied()
        .find(|descriptor| descriptor.id == id)
        .expect("built-in command identity has one descriptor")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette() -> CommandPaletteUi {
        let splint_id = SplintId::new();
        CommandPaletteUi::new(CommandPaletteContext {
            lair_id: LairId::new(),
            lair_retention: LairRetention::Disposable,
            dojo_id: DojoId::new(),
            dojo_name: "current".to_owned(),
            pane_count: 3,
            splint_id,
            dojo_splints: vec![(splint_id, 1), (SplintId::new(), 2), (SplintId::new(), 3)],
            other_dojo_ids: vec![DojoId::new(), DojoId::new()],
            previous_dojo_id: Some(DojoId::new()),
            next_dojo_id: Some(DojoId::new()),
            tab_move: CommandTabMoveAvailability::Both,
            focus_left: Some(SplintId::new()),
            focus_right: Some(SplintId::new()),
            focus_up: Some(SplintId::new()),
            focus_down: Some(SplintId::new()),
            clipboard_image_available: false,
            viewport_detached: true,
            controller_active: false,
            forced_control_transfer: true,
            revoke_all_available: true,
            access_waiting: 0,
            pending_transfer_id: Some(42),
            agent_status_incarnation: Some(9),
        })
    }

    #[test]
    fn clipboard_image_palette_dispatch_is_opt_in_and_has_no_default_shortcut() {
        let mut context = palette().context();
        let id = BuiltInCommandId::SaveClipboardImage;
        assert!(!command_enabled(id, &context));
        assert!(command_dispatch(id, &context).is_none());
        context.clipboard_image_available = true;
        assert_eq!(
            command_dispatch(id, &context),
            Some(BuiltInCommandDispatch::SaveClipboardImage)
        );
        assert_eq!(
            command_descriptor(id).shortcut(&ResolvedKeymap::default()),
            ""
        );
        let mut ui = CommandPaletteUi::new(context);
        ui.append_text("screenshot");
        assert_eq!(ui.filtered(), &[id]);
    }

    #[test]
    fn rename_prompt_is_prefilled_bounded_sanitized_and_exact() {
        let dojo_id = DojoId::new();
        let target = DojoActionTarget {
            dojo_id,
            name: "terminal".to_owned(),
            pane_count: 3,
            splints: vec![(SplintId::new(), 1)],
        };
        let mut rename = RenameDojoUi::new(target.clone());
        assert_eq!(rename.target(), &target);
        assert_eq!(rename.input(), "terminal");
        assert!(!rename.append_text("\n\u{202e}"));
        assert_eq!(rename.input(), "terminal");
        assert!(rename.append_text("work"));
        assert_eq!(rename.input(), "work");
        assert!(rename.append_text(&"界".repeat(100)));
        assert!(rename.input().len() <= MAX_DOJO_NAME_BYTES);
        assert_eq!(
            rename.command(),
            Some(WindowTopologyCommand::RenameDojo {
                dojo_id,
                name: rename.input().to_owned(),
            })
        );
        while rename.backspace() {}
        assert_eq!(rename.command(), None);
    }

    #[test]
    fn termination_prompt_defaults_to_cancel_and_keeps_exact_target() {
        let target = DojoActionTarget {
            dojo_id: DojoId::new(),
            name: "build".to_owned(),
            pane_count: 4,
            splints: vec![(SplintId::new(), 7), (SplintId::new(), 8)],
        };
        let mut confirmation = TerminateDojoUi::new(target.clone());
        assert_eq!(confirmation.target(), &target);
        assert_eq!(confirmation.decision(), TerminationDecision::Cancel);
        assert_eq!(confirmation.command(), None);
        assert!(confirmation.move_selection());
        assert_eq!(confirmation.decision(), TerminationDecision::Terminate);
        assert_eq!(
            confirmation.command(),
            Some(WindowTopologyCommand::TerminateDojo {
                dojo_id: target.dojo_id,
                splints: target.splints.clone(),
            })
        );
        assert!(confirmation.move_selection());
        assert_eq!(confirmation.command(), None);
    }

    #[test]
    fn lair_termination_defaults_cancel_and_preserves_the_exact_capture() {
        let lair_id = LairId::new();
        let target = LairPromptTarget {
            topology_revision: splinterm_core::TopologyRevision::new(7),
            lair_id,
            dojo_id: None,
            name: "work".to_owned(),
            retention: LairRetention::Saved,
            preview: "work — Saved".to_owned(),
            targets: vec![splinterm_protocol::MutationTarget {
                lair_id,
                dojo_id: DojoId::new(),
                splint_id: SplintId::new(),
                incarnation: 9,
            }],
        };
        let mut confirmation = TerminateLairUi::new(target.clone());
        assert_eq!(confirmation.target(), &target);
        assert_eq!(confirmation.decision(), TerminationDecision::Cancel);
        assert_eq!(confirmation.command(), None);
        assert!(confirmation.move_selection());
        assert_eq!(
            confirmation.command(),
            Some(WindowTopologyCommand::TerminateLair {
                lair_id,
                targets: target.targets,
            })
        );
    }

    #[test]
    fn explorer_context_catalogs_are_closed_and_capture_exact_targets() {
        let mut view = super::super::lair_explorer::tests::view();
        let ids = |menu: &TabContextMenuUi| {
            menu.actions()
                .iter()
                .map(|action| action.id)
                .collect::<Vec<_>>()
        };
        let menu = |view: &NavigationExplorerView, node, controlled| {
            TabContextMenuUi::for_explorer(view, node, Some(SplintId::new()), controlled, true)
                .unwrap()
        };
        let lair = view.lairs[0].id;
        let dojo = view.lairs[0].dojos[0].id;
        let splint = view.lairs[0].dojos[0].splints[0].id;
        assert_eq!(
            ids(&menu(&view, lair, false)),
            vec![
                TabMenuActionId::RenameLair,
                TabMenuActionId::NewDojo,
                TabMenuActionId::PinLair,
                TabMenuActionId::PreviewLair,
                TabMenuActionId::TerminateLair
            ]
        );
        assert_eq!(
            ids(&menu(&view, dojo, false)),
            vec![
                TabMenuActionId::RenameTab,
                TabMenuActionId::CloseTab,
                TabMenuActionId::TerminateDojo
            ]
        );
        let mut panes = menu(&view, splint, true);
        assert_eq!(
            ids(&panes),
            vec![
                TabMenuActionId::FocusSplint,
                TabMenuActionId::SplitBelow,
                TabMenuActionId::SplitRight,
                TabMenuActionId::CloseSplint
            ]
        );
        assert_eq!(
            ids(&menu(&view, splint, false)),
            vec![TabMenuActionId::FocusSplint]
        );
        for action in panes.actions() {
            let Some(TabMenuDispatch::Topology(WindowTopologyCommand::ExplorerContext {
                target,
                ..
            })) = panes.dispatch(action.id)
            else {
                panic!("expected guarded command")
            };
            assert_eq!(target.node, splint);
            assert_eq!(target.topology_revision, view.topology_revision);
            assert_eq!(target.live_incarnation, Some(3));
        }
        assert!(panes.move_selection(-1));
        assert_eq!(panes.selected_action(), TabMenuActionId::CloseSplint);
        let close = Some(TabMenuActionId::CloseSplint);
        assert_eq!(panes.release_action(close, close), close);
        assert_eq!(panes.release_action(None, close), None);
        assert_eq!(
            panes.release_action(close, Some(TabMenuActionId::SplitRight)),
            None
        );
        assert_eq!(panes.dispatch(TabMenuActionId::TerminateDojo), None);
        let no_cwd = TabContextMenuUi::for_explorer(&view, lair, None, false, true).unwrap();
        assert!(no_cwd.action_enabled(TabMenuActionId::RenameLair));
        assert!(!no_cwd.action_enabled(TabMenuActionId::NewDojo));
        view.lairs[0].retention = LairRetention::Disposable;
        assert!(ids(&menu(&view, lair, false)).contains(&TabMenuActionId::SaveLayout));
        view.lairs[0].retention = LairRetention::Pinned;
        assert!(ids(&menu(&view, lair, false)).contains(&TabMenuActionId::UnpinLair));
        view.lairs[0].can_create_dojo = false;
        assert!(!ids(&menu(&view, lair, false)).contains(&TabMenuActionId::NewDojo));
        view.freshness = crate::navigation_projection::EndpointFreshness::Stale;
        assert!(
            TabContextMenuUi::for_explorer(&view, lair, Some(SplintId::new()), true, true)
                .is_none()
        );
    }

    #[test]
    fn explorer_detached_saved_and_missing_incarnation_actions_are_applicable_only() {
        let mut view = super::super::lair_explorer::tests::view();
        let node = view.lairs[0].dojos[0].id;
        view.lairs[0].dojos[0].attachment = WindowAttachment::NotHere;
        view.lairs[0].dojos[0].active_here = false;
        view.lairs[0].dojos[0].target.capability.action = NavigationAction::AttachDojo;
        let menu = |view: &NavigationExplorerView| {
            TabContextMenuUi::for_explorer(view, node, Some(SplintId::new()), true, true).unwrap()
        };
        assert!(menu(&view).action_enabled(TabMenuActionId::ActivateTab));
        assert!(!menu(&view).action_enabled(TabMenuActionId::CloseTab));
        view.lairs[0].dojos[0].splints[0].target.last_incarnation = None;
        assert!(!menu(&view).action_enabled(TabMenuActionId::TerminateDojo));
        let splint = &mut view.lairs[0].dojos[0].splints[0];
        splint.lifecycle = NavigationLifecycle::Restorable;
        splint.target.live_incarnation = None;
        splint.target.last_incarnation = Some(3);
        splint.target.capability.action = NavigationAction::PreviewRestoreSplint;
        view.lairs[0].dojos[0].target.capability.action = NavigationAction::PreviewRestoreDojo;
        assert!(menu(&view).action_enabled(TabMenuActionId::Restore));
        assert!(!menu(&view).action_enabled(TabMenuActionId::TerminateDojo));
        assert!(!menu(&view).action_enabled(TabMenuActionId::ActivateTab));
        let splint = &view.lairs[0].dojos[0].splints[0];
        let menu = TabContextMenuUi::for_explorer(&view, splint.id, None, false, true).unwrap();
        assert_eq!(menu.actions().len(), 1);
        assert!(menu.action_enabled(TabMenuActionId::Restore));
        assert_eq!(
            menu.dispatch(TabMenuActionId::Restore),
            Some(TabMenuDispatch::Topology(
                ExplorerContextTarget {
                    topology_revision: view.topology_revision,
                    node: splint.id,
                    live_incarnation: None,
                }
                .command(WindowTopologyCommand::FocusSplint {
                    target: splint.target
                })
            ))
        );
        view.lairs[0].dojos[0].splints[0]
            .target
            .capability
            .availability = crate::navigation_projection::NavigationAvailability::Disabled(
            crate::navigation_projection::NavigationBlocker::PermissionDenied,
        );
        assert!(
            TabContextMenuUi::for_explorer(
                &view,
                view.lairs[0].dojos[0].splints[0].id,
                None,
                false,
                true
            )
            .is_none()
        );
    }

    #[test]
    fn explorer_lair_termination_requires_complete_capture_and_some_live_work() {
        let mut view = super::super::lair_explorer::tests::view();
        let node = view.lairs[0].id;
        let mut exited = view.lairs[0].dojos[0].splints[0].clone();
        exited.target.splint_id = SplintId::new();
        exited.lifecycle = NavigationLifecycle::Restorable;
        exited.target.live_incarnation = None;
        view.lairs[0].dojos[0].splints.push(exited);
        let enabled = |view: &NavigationExplorerView| {
            TabContextMenuUi::for_explorer(view, node, None, false, true)
                .unwrap()
                .action_enabled(TabMenuActionId::TerminateLair)
        };
        assert!(enabled(&view));
        view.lairs[0].dojos[0].splints[1].target.last_incarnation = None;
        assert!(!enabled(&view));
        view.lairs[0].dojos[0].splints[1].target.last_incarnation = Some(3);
        view.lairs[0].dojos[0].splints[0].lifecycle = NavigationLifecycle::Restorable;
        assert!(!enabled(&view));
    }

    #[test]
    fn rename_lair_prompt_preserves_authoritative_name_and_identity_for_all_retentions() {
        for retention in [
            LairRetention::Saved,
            LairRetention::Disposable,
            LairRetention::Pinned,
        ] {
            let id = LairId::new();
            let DojoPromptUi::RenameLair(prompt) = DojoPromptUi::rename_lair(LairPromptTarget {
                topology_revision: splinterm_core::TopologyRevision::new(8),
                lair_id: id,
                dojo_id: None,
                name: "latest name".into(),
                retention,
                preview: String::new(),
                targets: Vec::new(),
            }) else {
                unreachable!()
            };
            assert_eq!(prompt.input(), "latest name");
            assert_eq!(
                prompt.command(),
                Some(WindowTopologyCommand::RenameLair {
                    lair_id: id,
                    name: "latest name".into(),
                })
            );
        }
    }

    #[test]
    fn right_press_retargets_only_visible_tabs_and_otherwise_dismisses() {
        let dojo_id = DojoId::new();
        assert_eq!(
            tab_menu_right_press(Some(dojo_id), None),
            TabMenuRightPress::Retarget(dojo_id)
        );
        assert_eq!(tab_menu_right_press(None, None), TabMenuRightPress::Dismiss);
        let target = ExplorerContextTarget {
            topology_revision: splinterm_core::TopologyRevision::new(7),
            node: NavigationNodeId::Lair(LairId::new()),
            live_incarnation: None,
        };
        assert_eq!(
            tab_menu_right_press(None, Some(target)),
            TabMenuRightPress::RetargetExplorer(target)
        );
    }

    #[test]
    fn tab_menu_skips_disabled_and_dispatches_exact_captured_identity() {
        let other_dojo_ids = vec![DojoId::new(), DojoId::new()];
        let context = TabMenuContext {
            lair_id: LairId::new(),
            source_splint_id: SplintId::new(),
            dojo_id: DojoId::new(),
            dojo_name: "captured".to_owned(),
            pane_count: 3,
            splints: vec![
                (SplintId::new(), 4),
                (SplintId::new(), 5),
                (SplintId::new(), 6),
            ],
            active: false,
            other_dojo_ids: other_dojo_ids.clone(),
        };
        let mut menu = TabContextMenuUi::new(context.clone());
        assert_eq!(
            menu.dispatch(TabMenuActionId::NewDojo),
            Some(TabMenuDispatch::Topology(WindowTopologyCommand::NewDojo {
                lair_id: context.lair_id,
                source_splint_id: context.source_splint_id,
            }))
        );
        assert_eq!(menu.selected_action(), TabMenuActionId::RenameTab);
        assert!(menu.move_selection(-1));
        assert_eq!(menu.selected_action(), TabMenuActionId::TerminateDojo);
        assert_eq!(menu.context, ContextMenuTarget::Tab(context.clone()));
        assert_eq!(
            menu.dispatch(TabMenuActionId::RenameTab),
            Some(TabMenuDispatch::Rename(DojoActionTarget {
                dojo_id: context.dojo_id,
                name: "captured".to_owned(),
                pane_count: 3,
                splints: context.splints.clone(),
            }))
        );
        assert_eq!(
            tab_menu_dispatch(TabMenuActionId::CloseOtherTabs, &context),
            Some(TabMenuDispatch::Topology(
                WindowTopologyCommand::CloseTabs {
                    retain_dojo_id: context.dojo_id,
                    dojo_ids: other_dojo_ids.clone(),
                }
            ))
        );
        assert_eq!(
            close_other_tabs_command(context.dojo_id, other_dojo_ids),
            Some(WindowTopologyCommand::CloseTabs {
                retain_dojo_id: context.dojo_id,
                dojo_ids: context.other_dojo_ids.clone(),
            })
        );
        assert_eq!(close_other_tabs_command(context.dojo_id, Vec::new()), None);

        assert_eq!(
            tab_menu_dispatch(TabMenuActionId::TerminateDojo, &context),
            Some(TabMenuDispatch::ConfirmTermination(DojoActionTarget {
                dojo_id: context.dojo_id,
                name: "captured".to_owned(),
                pane_count: 3,
                splints: context.splints.clone(),
            }))
        );

        let mut active = TabContextMenuUi::new(TabMenuContext {
            active: true,
            other_dojo_ids: Vec::new(),
            ..context
        });
        assert_eq!(active.selected_action(), TabMenuActionId::RenameTab);
        assert!(!active.action_enabled(TabMenuActionId::ActivateTab));
        assert!(!active.action_enabled(TabMenuActionId::CloseOtherTabs));
        assert!(!active.update_hovered(Some(TabMenuActionId::ActivateTab)));
    }

    #[test]
    fn filtering_is_case_insensitive_stable_category_and_keyword_aware() {
        let mut palette = palette();
        let visible_count = BUILT_IN_COMMANDS
            .iter()
            .filter(|descriptor| {
                lifecycle_command_visible(descriptor.id, palette.context.lair_retention)
            })
            .count();
        assert_eq!(palette.filtered.len(), visible_count);
        assert!(palette.append_text("SPLIT"));
        assert_eq!(
            palette.filtered,
            vec![
                BuiltInCommandId::SplitHorizontal,
                BuiltInCommandId::SplitVertical
            ]
        );
        assert!(palette.append_text(" horizontal"));
        assert!(palette.filtered.is_empty());
        while palette.backspace() {}
        assert_eq!(palette.filtered.len(), visible_count);
        assert!(palette.append_text("horizontal"));
        assert_eq!(palette.filtered, vec![BuiltInCommandId::SplitHorizontal]);
        while palette.backspace() {}
        assert!(palette.append_text("view"));
        assert_eq!(
            palette.filtered,
            vec![
                BuiltInCommandId::ToggleTabStrip,
                BuiltInCommandId::ToggleLairExplorer,
                BuiltInCommandId::ToggleLairExplorerRail,
                BuiltInCommandId::FocusLairExplorer,
                BuiltInCommandId::ReturnFocusToTerminal,
                BuiltInCommandId::ZoomIn,
                BuiltInCommandId::ZoomOut,
                BuiltInCommandId::ResetZoom,
                BuiltInCommandId::DetachWindow
            ]
        );
        // The rail is found by what it does.
        while palette.backspace() {}
        assert!(palette.append_text("collapse"));
        assert_eq!(
            palette.filtered,
            vec![BuiltInCommandId::ToggleLairExplorerRail]
        );
    }

    #[test]
    fn lifecycle_commands_show_only_actions_applicable_to_the_captured_lair() {
        let disposable = palette();
        assert!(
            disposable
                .filtered()
                .contains(&BuiltInCommandId::SaveCurrentLair)
        );
        assert!(
            disposable
                .filtered()
                .contains(&BuiltInCommandId::PinCurrentLair)
        );
        assert!(
            !disposable
                .filtered()
                .contains(&BuiltInCommandId::UnpinCurrentLair)
        );
        assert!(
            !disposable
                .filtered()
                .contains(&BuiltInCommandId::PreviewCurrentLair)
        );
        assert!(
            !disposable
                .filtered()
                .contains(&BuiltInCommandId::RestoreCurrentLair)
        );

        let saved = CommandPaletteUi::new(CommandPaletteContext {
            lair_retention: LairRetention::Saved,
            ..disposable.context()
        });
        assert!(
            !saved
                .filtered()
                .contains(&BuiltInCommandId::SaveCurrentLair)
        );
        assert!(saved.filtered().contains(&BuiltInCommandId::PinCurrentLair));
        assert!(
            !saved
                .filtered()
                .contains(&BuiltInCommandId::UnpinCurrentLair)
        );
        assert!(
            saved
                .filtered()
                .contains(&BuiltInCommandId::PreviewCurrentLair)
        );
        assert!(
            saved
                .filtered()
                .contains(&BuiltInCommandId::RestoreCurrentLair)
        );

        let pinned = CommandPaletteUi::new(CommandPaletteContext {
            lair_retention: LairRetention::Pinned,
            ..saved.context()
        });
        assert!(
            !pinned
                .filtered()
                .contains(&BuiltInCommandId::SaveCurrentLair)
        );
        assert!(
            !pinned
                .filtered()
                .contains(&BuiltInCommandId::PinCurrentLair)
        );
        assert!(
            pinned
                .filtered()
                .contains(&BuiltInCommandId::UnpinCurrentLair)
        );
        assert!(
            pinned
                .filtered()
                .contains(&BuiltInCommandId::PreviewCurrentLair)
        );
        assert!(
            pinned
                .filtered()
                .contains(&BuiltInCommandId::RestoreCurrentLair)
        );
    }

    #[test]
    fn lifecycle_keyboard_and_palette_paths_share_exact_commands() {
        let disposable = palette().context();
        let saved = CommandPaletteContext {
            lair_retention: LairRetention::Saved,
            ..disposable.clone()
        };
        let pinned = CommandPaletteContext {
            lair_retention: LairRetention::Pinned,
            ..disposable.clone()
        };
        for (command, action, context) in [
            (
                BuiltInCommandId::SaveCurrentLair,
                ActionId::SaveCurrentLair,
                &disposable,
            ),
            (
                BuiltInCommandId::PinCurrentLair,
                ActionId::ToggleCurrentLairPin,
                &saved,
            ),
            (
                BuiltInCommandId::UnpinCurrentLair,
                ActionId::ToggleCurrentLairPin,
                &pinned,
            ),
            (
                BuiltInCommandId::PreviewCurrentLair,
                ActionId::PreviewCurrentLair,
                &saved,
            ),
            (
                BuiltInCommandId::RestoreCurrentLair,
                ActionId::RestoreCurrentLair,
                &pinned,
            ),
        ] {
            let expected = lair_lifecycle_command(action, context.lair_id, context.lair_retention)
                .expect("applicable lifecycle action has one command");
            assert_eq!(
                command_dispatch(command, context),
                Some(BuiltInCommandDispatch::Topology(expected))
            );
        }
        assert!(
            lair_lifecycle_command(
                ActionId::SaveCurrentLair,
                saved.lair_id,
                saved.lair_retention
            )
            .is_none()
        );
        assert!(
            lair_lifecycle_command(
                ActionId::PreviewCurrentLair,
                disposable.lair_id,
                disposable.lair_retention
            )
            .is_none()
        );
    }

    #[test]
    fn selection_wraps_skips_disabled_and_preserves_enabled_command() {
        let mut palette = palette();
        assert!(palette.move_selection(-1));
        assert_eq!(
            palette.selected_command(),
            Some(BuiltInCommandId::DenyControlTransfer)
        );
        assert!(palette.append_text("zoom"));
        assert_eq!(
            palette.selected_command(),
            Some(BuiltInCommandId::ToggleFocusedPaneZoom)
        );
        assert!(!palette.select_first());
        assert!(palette.select_last());
        assert_eq!(
            palette.selected_command(),
            Some(BuiltInCommandId::ResetZoom)
        );

        let context = CommandPaletteContext {
            previous_dojo_id: None,
            next_dojo_id: None,
            tab_move: CommandTabMoveAvailability::Neither,
            focus_left: None,
            focus_right: None,
            focus_up: None,
            focus_down: None,
            ..palette.context()
        };
        let mut disabled = CommandPaletteUi::new(context);
        disabled.select_first();
        assert_eq!(
            disabled.selected_command(),
            Some(BuiltInCommandId::ShowKeybindings)
        );
        disabled.move_selection(1);
        assert_eq!(
            disabled.selected_command(),
            Some(BuiltInCommandId::ReloadConfiguration)
        );
        assert!(!disabled.command_enabled(BuiltInCommandId::PreviousDojo));
        assert!(!disabled.update_hovered(Some(BuiltInCommandId::FocusLeft)));
    }

    #[test]
    fn query_is_scalar_safe_bounded_and_removes_control_and_bidi() {
        let mut palette = palette();
        assert!(palette.append_text("sp\n\u{202e}lit"));
        assert_eq!(palette.query(), "split");
        while palette.backspace() {}
        assert!(palette.append_text(&"界".repeat(200)));
        assert!(palette.query().len() <= MAX_QUERY_BYTES);
        assert!(palette.query().chars().count() <= MAX_QUERY_SCALARS);
        assert!(palette.backspace());
        assert!(std::str::from_utf8(palette.query().as_bytes()).is_ok());
    }

    #[test]
    fn owned_palette_and_rename_editors_refilter_and_undo_in_place() {
        let mut palette = palette();
        assert!(palette.append_text("zoom"));
        assert!(palette.filtered().contains(&BuiltInCommandId::ZoomIn));
        assert!(palette.editor_mut().select_all());
        assert_eq!(palette.editor_mut().cut().as_deref(), Some("zoom"));
        palette.editor_changed();
        assert_eq!(palette.query(), "");
        assert!(palette.editor_mut().undo());
        palette.editor_changed();
        assert_eq!(palette.query(), "zoom");

        let target = DojoActionTarget {
            dojo_id: DojoId::new(),
            name: "old".into(),
            pane_count: 1,
            splints: vec![(SplintId::new(), 1)],
        };
        let mut rename = RenameDojoUi::new(target);
        assert_eq!(rename.editor_mut().cut().as_deref(), Some("old"));
        assert!(rename.editor_mut().undo());
        assert_eq!(rename.input(), "old");
    }

    #[test]
    fn captured_context_does_not_change_with_query_or_selection() {
        let mut palette = palette();
        let context = palette.context();
        palette.append_text("split");
        palette.move_selection(1);
        assert_eq!(palette.context(), context);
    }

    #[test]
    fn shortcut_labels_project_from_the_resolved_keymap() {
        let keymap = ResolvedKeymap::default();
        assert_eq!(
            command_descriptor(BuiltInCommandId::RecentSessions).shortcut(&keymap),
            "Ctrl+Shift+S"
        );
        assert_eq!(
            command_descriptor(BuiltInCommandId::SplitVertical).shortcut(&keymap),
            "Ctrl+Shift+\\"
        );
        assert_eq!(
            command_descriptor(BuiltInCommandId::NewSession).shortcut(&keymap),
            ""
        );
        let omarchy = crate::keymap::built_in_keymap(crate::keymap::KeymapProfile::OmarchyTmux);
        for (command, shortcut) in [
            (BuiltInCommandId::SaveCurrentLair, "Prefix Shift+S"),
            (BuiltInCommandId::PinCurrentLair, "Prefix Shift+F"),
            (BuiltInCommandId::UnpinCurrentLair, "Prefix Shift+F"),
            (BuiltInCommandId::PreviewCurrentLair, "Prefix Shift+V"),
            (BuiltInCommandId::RestoreCurrentLair, "Prefix Shift+O"),
        ] {
            assert_eq!(command_descriptor(command).shortcut(&omarchy), shortcut);
        }
        let custom = crate::keymap::resolve_keymap_text(
            crate::keymap::KeymapProfile::Splinterm,
            "version = 1\n[[unbind]]\nsequence = [\"Ctrl+Shift+S\"]\n[[binding]]\nsequence = [\"Ctrl+Alt+S\"]\naction = \"session.recent\"",
            std::path::Path::new("keybindings.toml"),
        )
        .unwrap();
        assert_eq!(
            command_descriptor(BuiltInCommandId::RecentSessions).shortcut(&custom.keymap),
            "Ctrl+Alt+S"
        );
        assert!(BUILT_IN_COMMANDS.iter().all(|descriptor| {
            descriptor
                .shortcut_action
                .is_none_or(|action| ActionId::BINDABLE.contains(&action))
        }));
    }

    #[test]
    fn broad_catalog_is_closed_unique_and_category_searchable() {
        let ids = BUILT_IN_COMMANDS
            .iter()
            .map(|descriptor| descriptor.id)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), BUILT_IN_COMMANDS.len());
        assert_eq!(
            ids,
            BuiltInCommandId::ALL.iter().copied().collect(),
            "every command identity has exactly one descriptor"
        );
        assert!(BUILT_IN_COMMANDS.iter().all(|descriptor| {
            !descriptor.title.trim().is_empty()
                && !descriptor.category.label().trim().is_empty()
                && !descriptor.keywords.is_empty()
        }));
        let titles = BUILT_IN_COMMANDS
            .iter()
            .map(|descriptor| descriptor.title)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(
            titles.len(),
            BUILT_IN_COMMANDS.len(),
            "command titles remain unique"
        );
        let mut palette = palette();
        assert!(palette.append_text("control"));
        assert_eq!(palette.filtered().len(), 7);
        while palette.backspace() {}
        assert!(palette.append_text("history"));
        assert_eq!(palette.filtered().len(), 5);
    }

    #[test]
    fn clear_agent_status_targets_the_captured_incarnation_and_only_clears() {
        let context = palette().context();
        assert!(command_enabled(
            BuiltInCommandId::ClearAgentStatus,
            &context
        ));
        assert_eq!(
            command_dispatch(BuiltInCommandId::ClearAgentStatus, &context),
            Some(BuiltInCommandDispatch::Topology(
                WindowTopologyCommand::ClearAgentStatus {
                    splint_id: context.splint_id,
                    incarnation: 9,
                }
            ))
        );
        let descriptor = command_descriptor(BuiltInCommandId::ClearAgentStatus);
        assert_eq!(descriptor.title, "Clear agent status");
        assert_eq!(descriptor.category, CommandCategory::Panes);
        assert!(descriptor_matches(descriptor, "agent"));
        let without_status = CommandPaletteContext {
            agent_status_incarnation: None,
            ..context
        };
        assert!(!command_enabled(
            BuiltInCommandId::ClearAgentStatus,
            &without_status
        ));
        assert_eq!(
            command_dispatch(BuiltInCommandId::ClearAgentStatus, &without_status),
            None
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive stable-target dispatch table is easier to audit"
    )]
    fn command_dispatch_keeps_exact_captured_identity_and_availability() {
        let context = palette().context();
        for descriptor in BUILT_IN_COMMANDS {
            assert_eq!(
                command_dispatch(descriptor.id, &context).is_some(),
                command_enabled(descriptor.id, &context),
                "availability and dispatch agree for {}",
                descriptor.title
            );
        }
        let controller = CommandPaletteContext {
            controller_active: true,
            ..context.clone()
        };
        assert!(command_dispatch(BuiltInCommandId::ReleaseControl, &controller).is_some());
        assert!(command_dispatch(BuiltInCommandId::RequestControl, &controller).is_none());
        let remote = CommandPaletteContext {
            forced_control_transfer: false,
            ..context.clone()
        };
        assert!(!command_enabled(BuiltInCommandId::ForceControl, &remote));
        assert!(command_dispatch(BuiltInCommandId::ForceControl, &remote).is_none());
        assert_eq!(
            command_dispatch(BuiltInCommandId::AcceptControlTransfer, &context),
            Some(BuiltInCommandDispatch::Control {
                target: context.splint_id,
                action: CommandControlAction::Accept(42),
            })
        );
        assert_eq!(
            command_dispatch(BuiltInCommandId::RevokeAllAccess, &context),
            Some(BuiltInCommandDispatch::Topology(
                WindowTopologyCommand::Access(WindowAccessCommand::CountRevokeAll)
            ))
        );
        let nothing_to_revoke = CommandPaletteContext {
            revoke_all_available: false,
            ..context.clone()
        };
        assert!(!command_enabled(
            BuiltInCommandId::RevokeAllAccess,
            &nothing_to_revoke
        ));
        // The list opens whatever it holds, including "no access".
        assert_eq!(
            command_dispatch(BuiltInCommandId::AgentAccess, &nothing_to_revoke),
            Some(BuiltInCommandDispatch::AgentAccess)
        );
        assert_eq!(
            command_dispatch(BuiltInCommandId::NewSession, &context),
            Some(BuiltInCommandDispatch::Topology(
                WindowTopologyCommand::NewLair {
                    source_splint_id: context.splint_id,
                }
            ))
        );
        assert_eq!(
            command_dispatch(BuiltInCommandId::NewDojo, &context),
            Some(BuiltInCommandDispatch::Topology(
                WindowTopologyCommand::NewDojo {
                    lair_id: context.lair_id,
                    source_splint_id: context.splint_id,
                }
            ))
        );
        assert_eq!(
            command_dispatch(BuiltInCommandId::ShowKeybindings, &context),
            Some(BuiltInCommandDispatch::ShowKeybindings)
        );
        assert_eq!(
            command_dispatch(BuiltInCommandId::ReloadConfiguration, &context),
            Some(BuiltInCommandDispatch::ReloadConfiguration)
        );
        assert_eq!(
            command_dispatch(BuiltInCommandId::EnterCopyMode, &context),
            Some(BuiltInCommandDispatch::EnterCopyMode)
        );
        assert_eq!(
            command_dispatch(BuiltInCommandId::ToggleFocusedPaneZoom, &context),
            Some(BuiltInCommandDispatch::ToggleFocusedPaneZoom)
        );
        assert_eq!(
            command_dispatch(BuiltInCommandId::MoveDojoLeft, &context),
            Some(BuiltInCommandDispatch::MoveDojo {
                dojo_id: context.dojo_id,
                delta: -1,
            })
        );
        assert_eq!(
            command_dispatch(BuiltInCommandId::ChooseDojo, &context),
            Some(BuiltInCommandDispatch::Topology(
                WindowTopologyCommand::RequestSelector {
                    kind: SelectorKind::Dojo,
                    lair_id: context.lair_id,
                }
            ))
        );
        assert_eq!(
            command_dispatch(BuiltInCommandId::ChooseLair, &context),
            Some(BuiltInCommandDispatch::Topology(
                WindowTopologyCommand::RequestSelector {
                    kind: SelectorKind::Lair,
                    lair_id: context.lair_id,
                }
            ))
        );
        assert_eq!(
            command_dispatch(BuiltInCommandId::RenameCurrentLair, &context),
            Some(BuiltInCommandDispatch::Topology(
                WindowTopologyCommand::RequestLairPrompt {
                    lair_id: context.lair_id,
                    kind: LairPromptKind::Rename,
                    expected_retention: None,
                }
            ))
        );
        assert_eq!(
            command_dispatch(BuiltInCommandId::PreviousLair, &context),
            Some(BuiltInCommandDispatch::Topology(
                WindowTopologyCommand::NavigateLair {
                    current_lair_id: context.lair_id,
                    direction: LairDirection::Previous,
                }
            ))
        );
        assert_eq!(
            command_dispatch(BuiltInCommandId::TerminateCurrentLair, &context),
            Some(BuiltInCommandDispatch::Topology(
                WindowTopologyCommand::RequestLairPrompt {
                    lair_id: context.lair_id,
                    kind: LairPromptKind::Terminate,
                    expected_retention: None,
                }
            ))
        );
        assert_eq!(
            command_dispatch(BuiltInCommandId::DetachWindow, &context),
            Some(BuiltInCommandDispatch::DetachWindow)
        );
        assert_eq!(
            command_dispatch(BuiltInCommandId::TerminateCurrentDojo, &context),
            Some(BuiltInCommandDispatch::ConfirmTermination(
                DojoActionTarget {
                    dojo_id: context.dojo_id,
                    name: context.dojo_name.clone(),
                    pane_count: context.pane_count,
                    splints: context.dojo_splints.clone(),
                }
            ))
        );
        assert_eq!(
            command_dispatch(BuiltInCommandId::SplitHorizontal, &context),
            Some(BuiltInCommandDispatch::Topology(
                WindowTopologyCommand::Split {
                    dojo_id: context.dojo_id,
                    target: context.splint_id,
                    axis: Axis::Vertical,
                    pending: None,
                }
            ))
        );
        assert_eq!(
            command_dispatch(BuiltInCommandId::SplitVertical, &context),
            Some(BuiltInCommandDispatch::Topology(
                WindowTopologyCommand::Split {
                    dojo_id: context.dojo_id,
                    target: context.splint_id,
                    axis: Axis::Horizontal,
                    pending: None,
                }
            ))
        );
        assert_eq!(
            command_dispatch(BuiltInCommandId::PreviousDojo, &context),
            Some(BuiltInCommandDispatch::Topology(
                WindowTopologyCommand::ActivateTab {
                    dojo_id: context.previous_dojo_id.unwrap(),
                }
            ))
        );
        assert_eq!(
            command_dispatch(BuiltInCommandId::FocusRight, &context),
            Some(BuiltInCommandDispatch::Focus(context.focus_right.unwrap()))
        );
        assert_eq!(
            command_dispatch(BuiltInCommandId::CloseFocusedPane, &context),
            Some(BuiltInCommandDispatch::Topology(
                WindowTopologyCommand::Close {
                    dojo_id: context.dojo_id,
                    target: context.splint_id,
                }
            ))
        );
        assert_eq!(
            command_dispatch(BuiltInCommandId::ToggleTabStrip, &context),
            Some(BuiltInCommandDispatch::ToggleTabStrip)
        );
        for (id, dispatch) in [
            (
                BuiltInCommandId::ToggleLairExplorer,
                BuiltInCommandDispatch::ToggleLairExplorer,
            ),
            // The rail toggle is palette-only too: no default chord.
            (
                BuiltInCommandId::ToggleLairExplorerRail,
                BuiltInCommandDispatch::ToggleLairExplorerRail,
            ),
            (
                BuiltInCommandId::FocusLairExplorer,
                BuiltInCommandDispatch::FocusLairExplorer,
            ),
            (
                BuiltInCommandId::ReturnFocusToTerminal,
                BuiltInCommandDispatch::ReturnFocusToTerminal,
            ),
        ] {
            assert_eq!(command_dispatch(id, &context), Some(dispatch));
            assert_eq!(
                command_descriptor(id).shortcut(&ResolvedKeymap::default()),
                ""
            );
        }
        assert_eq!(
            command_dispatch(BuiltInCommandId::ZoomOut, &context),
            Some(BuiltInCommandDispatch::Zoom(CommandZoomAction::Decrease))
        );
        assert_eq!(
            command_dispatch(
                BuiltInCommandId::NextDojo,
                &CommandPaletteContext {
                    next_dojo_id: None,
                    ..context.clone()
                }
            ),
            None
        );
        assert_eq!(
            command_dispatch(
                BuiltInCommandId::ToggleFocusedPaneZoom,
                &CommandPaletteContext {
                    pane_count: 1,
                    ..context
                }
            ),
            None
        );
    }

    #[test]
    fn open_remote_is_an_always_available_palette_command_without_a_chord() {
        let context = palette().context();
        let id = BuiltInCommandId::OpenRemote;
        assert!(BuiltInCommandId::ALL.contains(&id));
        assert!(command_enabled(id, &context));
        assert_eq!(
            command_dispatch(id, &context),
            Some(BuiltInCommandDispatch::OpenRemoteChooser)
        );
        let descriptor = command_descriptor(id);
        assert_eq!(descriptor.title, "Open Remote…");
        assert_eq!(descriptor.shortcut(&ResolvedKeymap::default()), "");
        for query in ["remote", "SSH", "open rem", "host"] {
            let mut ui = palette();
            assert!(ui.append_text(query));
            assert!(ui.filtered().contains(&id), "query {query:?}");
        }
        let mut ui = palette();
        assert!(ui.append_text("ssh"));
        assert_eq!(ui.filtered(), &[id]);
    }

    #[test]
    fn handoff_pause_disables_mutating_palette_commands_and_their_dispatch() {
        let mut ui = palette();
        assert!(ui.command_enabled(BuiltInCommandId::NewDojo));
        assert!(ui.dispatch(BuiltInCommandId::SplitVertical).is_some());
        assert!(ui.set_mutations_paused(true));
        assert!(!ui.set_mutations_paused(true), "unchanged");
        for id in BuiltInCommandId::ALL {
            if command_paused_during_handoff(*id) {
                assert!(!ui.command_enabled(*id), "{id:?}");
                assert_eq!(ui.dispatch(*id), None, "{id:?}");
            }
        }
        // Local UI keeps working: help, browsing, tab switching, focus,
        // history, zoom, detaching, and the releases a fenced daemon serves.
        for id in [
            BuiltInCommandId::ShowKeybindings,
            BuiltInCommandId::RecentSessions,
            BuiltInCommandId::ChooseDojo,
            BuiltInCommandId::PreviousDojo,
            BuiltInCommandId::MoveDojoLeft,
            BuiltInCommandId::FocusLeft,
            BuiltInCommandId::ToggleFocusedPaneZoom,
            BuiltInCommandId::SearchScrollback,
            BuiltInCommandId::PageUp,
            BuiltInCommandId::ReturnToLive,
            BuiltInCommandId::ToggleLairExplorer,
            BuiltInCommandId::ZoomIn,
            BuiltInCommandId::RevokeAllAccess,
            BuiltInCommandId::DetachWindow,
        ] {
            assert!(ui.command_enabled(id), "{id:?}");
            assert!(ui.dispatch(id).is_some(), "{id:?}");
        }
        assert!(
            ui.selected_enabled_command().is_some(),
            "the selection moves to an enabled command"
        );
        assert!(ui.set_mutations_paused(false));
        assert!(ui.command_enabled(BuiltInCommandId::NewDojo));
        assert!(ui.dispatch(BuiltInCommandId::NewDojo).is_some());
    }

    #[test]
    fn palette_pause_classification_matches_the_topology_command_gate() {
        let ui = palette();
        for id in BuiltInCommandId::ALL {
            if let Some(BuiltInCommandDispatch::Topology(command)) = ui.dispatch(*id) {
                assert_eq!(
                    command_paused_during_handoff(*id),
                    command.paused_during_handoff(),
                    "{id:?} dispatches {command:?}"
                );
            }
        }
    }

    #[test]
    fn handoff_pause_disables_tab_and_explorer_menu_mutations_but_not_activation() {
        let context = TabMenuContext {
            lair_id: LairId::new(),
            source_splint_id: SplintId::new(),
            dojo_id: DojoId::new(),
            dojo_name: "captured".to_owned(),
            pane_count: 1,
            splints: vec![(SplintId::new(), 4)],
            active: false,
            other_dojo_ids: vec![DojoId::new()],
        };
        let mut menu = TabContextMenuUi::new(context);
        assert_eq!(menu.selected_action(), TabMenuActionId::RenameTab);
        assert!(menu.set_mutations_paused(true));
        for id in [
            TabMenuActionId::RenameTab,
            TabMenuActionId::NewDojo,
            TabMenuActionId::CloseTab,
            TabMenuActionId::CloseOtherTabs,
            TabMenuActionId::TerminateDojo,
        ] {
            assert!(!menu.action_enabled(id), "{id:?}");
            assert_eq!(menu.dispatch(id), None, "{id:?}");
        }
        assert_eq!(menu.selected_action(), TabMenuActionId::ActivateTab);
        assert!(matches!(
            menu.dispatch(TabMenuActionId::ActivateTab),
            Some(TabMenuDispatch::Topology(
                WindowTopologyCommand::ActivateTab { .. }
            ))
        ));
        assert!(menu.set_mutations_paused(false));
        assert!(menu.action_enabled(TabMenuActionId::NewDojo));

        let view = super::super::lair_explorer::tests::view();
        let lair = view.lairs[0].id;
        let mut lair_menu =
            TabContextMenuUi::for_explorer(&view, lair, Some(SplintId::new()), false, true)
                .unwrap();
        assert!(lair_menu.set_mutations_paused(true));
        let enabled = lair_menu
            .actions()
            .iter()
            .map(|action| action.id)
            .filter(|id| lair_menu.action_enabled(*id))
            .collect::<Vec<_>>();
        assert_eq!(enabled, vec![TabMenuActionId::PreviewLair]);
        assert_eq!(lair_menu.selected_action(), TabMenuActionId::PreviewLair);
        let splint = view.lairs[0].dojos[0].splints[0].id;
        let mut pane_menu =
            TabContextMenuUi::for_explorer(&view, splint, Some(SplintId::new()), true, true)
                .unwrap();
        pane_menu.set_mutations_paused(true);
        for id in [
            TabMenuActionId::SplitBelow,
            TabMenuActionId::SplitRight,
            TabMenuActionId::CloseSplint,
        ] {
            assert!(!pane_menu.action_enabled(id), "{id:?}");
            assert_eq!(pane_menu.dispatch(id), None, "{id:?}");
        }
    }
}

#[cfg(test)]
mod agent_access_palette_tests {
    use super::*;

    fn palette(access_waiting: u32, revoke_all_available: bool) -> CommandPaletteUi {
        let mut context = tests_context();
        context.access_waiting = access_waiting;
        context.revoke_all_available = revoke_all_available;
        CommandPaletteUi::new(context)
    }

    fn tests_context() -> CommandPaletteContext {
        let splint_id = SplintId::new();
        CommandPaletteContext {
            lair_id: LairId::new(),
            lair_retention: LairRetention::Disposable,
            dojo_id: DojoId::new(),
            dojo_name: "current".to_owned(),
            pane_count: 1,
            splint_id,
            dojo_splints: vec![(splint_id, 1)],
            other_dojo_ids: Vec::new(),
            previous_dojo_id: None,
            next_dojo_id: None,
            tab_move: CommandTabMoveAvailability::Neither,
            focus_left: None,
            focus_right: None,
            focus_up: None,
            focus_down: None,
            clipboard_image_available: false,
            viewport_detached: false,
            controller_active: true,
            forced_control_transfer: false,
            revoke_all_available: true,
            access_waiting: 0,
            pending_transfer_id: None,
            agent_status_incarnation: None,
        }
    }

    #[test]
    fn agent_access_is_a_palette_entry_found_by_waiting_and_badged_while_requests_wait() {
        let mut ui = palette(2, true);
        assert!(ui.append_text("waiting"));
        assert_eq!(ui.filtered(), [BuiltInCommandId::AgentAccess]);
        assert_eq!(
            ui.selected_enabled_command(),
            Some(BuiltInCommandId::AgentAccess)
        );
        assert_eq!(
            ui.badge(BuiltInCommandId::AgentAccess).as_deref(),
            Some("2 waiting")
        );
        assert_eq!(ui.badge(BuiltInCommandId::RevokeAllAccess), None);
        assert_eq!(palette(0, true).badge(BuiltInCommandId::AgentAccess), None);
        // Reading and revoking access never waits out a daemon handoff: both
        // only read or reduce authority.
        assert!(!command_paused_during_handoff(
            BuiltInCommandId::AgentAccess
        ));
        assert!(!command_paused_during_handoff(
            BuiltInCommandId::RevokeAllAccess
        ));
        let mut paused = palette(0, true);
        paused.set_mutations_paused(true);
        assert_eq!(
            paused.dispatch(BuiltInCommandId::AgentAccess),
            Some(BuiltInCommandDispatch::AgentAccess)
        );
        assert!(paused.dispatch(BuiltInCommandId::RevokeAllAccess).is_some());
        assert_eq!(
            command_descriptor(BuiltInCommandId::AgentAccess).title,
            "Agent access"
        );
    }

    #[test]
    fn revoke_prompts_are_bounded_default_cancel_and_send_only_access_commands() {
        use splinterm_protocol::AccessRevocationCounts;
        let counts = AccessRevocationCounts {
            remembered: 1,
            ..AccessRevocationCounts::default()
        };
        let mut prompt = DojoPromptUi::RevokeAccess(RevokeAccessUi::all(counts, false));
        assert!(prompt.uses_layout_summary());
        assert!(!prompt.is_preview());
        assert!(!prompt.is_rename());
        assert_eq!(prompt.input(), None);
        assert!(prompt.editor_mut().is_none());
        assert_eq!(prompt.confirmation_label(), Some("Revoke all"));
        assert_eq!(prompt.decision(), Some(TerminationDecision::Cancel));
        let notice = DojoPromptUi::RevokeAccess(RevokeAccessUi::notice(
            "REVOKE ALL ACCESS",
            "Nothing to revoke.",
        ));
        assert!(notice.is_preview());
        assert_eq!(notice.decision(), None);
        assert_eq!(notice.confirmation_label(), None);
        assert_eq!(
            notice.title_and_body(),
            ("REVOKE ALL ACCESS", "Nothing to revoke.".to_owned())
        );
    }
}
