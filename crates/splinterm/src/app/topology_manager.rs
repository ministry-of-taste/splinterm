use std::{
    collections::{HashMap, HashSet},
    env,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use splinterm::{
    ExplorerContextTarget, LairDirection, LairExplorerActivationTarget, LairPromptKind,
    LairPromptTarget, SelectorKind, SessionPickerCatalog, SessionPickerCreationTarget,
    SessionPickerItem, SessionPickerTarget, WindowDojoIdentity, WindowPaneOptions,
    WindowTopologyCommand, WindowTopologyUpdate,
    automation::{
        Connection, DaemonIdentity, SharedImageContentCache, is_connection_loss, protocol_error,
    },
    config::{AppConfig, TopologyReloadSettings},
    endpoint::{ConnectionFactory, LaunchSemantics},
    frontend::{NOT_RESTORABLE, single_line},
    navigation_projection::{
        EndpointFreshness, NavigationAction, NavigationAgentStatus, NavigationAuthority,
        NavigationAvailability, NavigationExplorerSplintTarget, NavigationExplorerView,
        NavigationNodeId, NavigationPermission, NavigationPickerKind, NavigationPickerView,
        NavigationProjection, NavigationProjectionContext, NavigationWindowState,
        WindowAgentStatus, dojo_labels, lair_display_label,
    },
    session_picker::{SessionEntry, collect_sessions, dojo_has_fully_running_pane_layout},
    tab::{DojoTab, OpenTabOutcome, WindowTabSet},
};
use splinterm_core::{
    Axis, DojoId, LairId, LayoutNode, SplintId, SplintState, SplitRatio, SplitSide,
    TopologyRevision,
};
use splinterm_protocol::{
    ErrorCode, LaunchDirectory, MutationTarget, Request, Response, UnusedDojoCloseOutcome,
    validate_preset_materialized,
};
use tokio::sync::{mpsc, watch};

#[path = "topology_reconnect.rs"]
mod reconnect;

use reconnect::{ManagedReconnect, reconnect_managed_window};

use super::{
    access_monitor::{AccessMonitor, abandoned_access_updates},
    pane_bridge::{PaneTask, layout_splint_ids, prepare_live_pane_at_incarnation},
    session_catalog::{
        GraphicalLairLifetime, automation_launch, creation_directory, graphical_create_request,
        launch_parameters, new_dojo_request_for, recent_dojo_ids, remember_dojo, select_dojo_from,
    },
};

fn parent_ratio(root: &LayoutNode, target: SplintId) -> Option<SplitRatio> {
    match root {
        LayoutNode::Leaf(_) => None,
        LayoutNode::Branch {
            ratio,
            first,
            second,
            ..
        } => {
            let direct_child =
                |node: &LayoutNode| matches!(node, LayoutNode::Leaf(splint) if splint.id == target);
            if direct_child(first) || direct_child(second) {
                Some(*ratio)
            } else {
                parent_ratio(first, target).or_else(|| parent_ratio(second, target))
            }
        }
    }
}

async fn inspect_optional_dojo_state(
    connection: &mut Connection,
    dojo_id: DojoId,
) -> Result<(TopologyRevision, Option<LayoutNode>)> {
    let Response::Topology { snapshot } = connection.request(Request::InspectTopology).await?
    else {
        bail!("splinterd did not return topology after edit");
    };
    snapshot
        .validate()
        .map_err(|error| anyhow::anyhow!(error.message))?;
    let root = snapshot
        .topology
        .lairs()
        .flat_map(|dojo| &dojo.dojos)
        .find(|dojo| dojo.id == dojo_id)
        .map(|dojo| dojo.root.clone());
    Ok((snapshot.revision, root))
}

async fn inspect_dojo_state(
    connection: &mut Connection,
    dojo_id: DojoId,
) -> Result<(TopologyRevision, LayoutNode)> {
    let (revision, root) = inspect_optional_dojo_state(connection, dojo_id).await?;
    Ok((
        revision,
        root.context("edited Dojo is absent from committed topology")?,
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CloseAction {
    CloseExited,
    KillAndClose { incarnation: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingTopologyFocus {
    splint_id: SplintId,
    revision: TopologyRevision,
    placeholder: Option<SplintId>,
}

fn topology_edit_label(command: &WindowTopologyCommand) -> &'static str {
    match command {
        WindowTopologyCommand::Split { .. } => "split",
        WindowTopologyCommand::Close { .. } => "close",
        WindowTopologyCommand::AdjustRatio { .. } | WindowTopologyCommand::SetRatio { .. } => {
            "resize"
        }
        _ => "session",
    }
}

fn command_has_pending_split(command: Option<&WindowTopologyCommand>) -> bool {
    matches!(
        command,
        Some(WindowTopologyCommand::Split {
            pending: Some(_),
            ..
        })
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TopologyCommandOutcome {
    Updated {
        pending_focus: Option<PendingTopologyFocus>,
    },
    WindowClosed,
}

const MAX_CLOSE_TOPOLOGY_RETRIES: usize = 64;

fn close_action(root: &LayoutNode, target: SplintId) -> Result<CloseAction> {
    let splint = root
        .find_splint(target)
        .context("focused pane is absent from committed topology")?;
    if matches!(splint.state, SplintState::Exited(_)) {
        return Ok(CloseAction::CloseExited);
    }
    Ok(CloseAction::KillAndClose {
        incarnation: splint
            .last_incarnation
            .context("live focused pane has no process incarnation")?,
    })
}

fn close_target_already_closed(root: &LayoutNode, target: SplintId) -> bool {
    root.find_splint(target).is_none()
}

fn validate_exited_close_target(
    root: &LayoutNode,
    target: SplintId,
    expected_incarnation: Option<u64>,
) -> Result<bool> {
    let splint = root
        .find_splint(target)
        .context("focused pane is absent from committed topology")?;
    anyhow::ensure!(
        matches!(splint.state, SplintState::Exited(_)),
        "pane remained live before close"
    );
    if let Some(expected) = expected_incarnation {
        anyhow::ensure!(
            splint.last_incarnation == Some(expected),
            "pane incarnation changed before close"
        );
    }
    Ok(root.splint_count() == 1)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RefreshedCloseState {
    WindowClosed,
    TargetClosed,
    Retry,
}

fn refreshed_close_state(
    root: Option<&LayoutNode>,
    target: SplintId,
    expected_incarnation: Option<u64>,
) -> Result<RefreshedCloseState> {
    let Some(root) = root else {
        return Ok(RefreshedCloseState::WindowClosed);
    };
    if root.find_splint(target).is_none() {
        return Ok(RefreshedCloseState::TargetClosed);
    }
    validate_exited_close_target(root, target, expected_incarnation)?;
    Ok(RefreshedCloseState::Retry)
}

fn captured_dojo_kill_targets(
    root: &LayoutNode,
    captured: &[(SplintId, u64)],
) -> Result<Vec<(SplintId, u64)>> {
    anyhow::ensure!(
        !captured.is_empty() && captured.len() == root.splint_count(),
        "captured Dojo pane set changed before termination"
    );
    let mut unique = HashSet::with_capacity(captured.len());
    let mut live = Vec::with_capacity(captured.len());
    for &(splint_id, incarnation) in captured {
        anyhow::ensure!(
            unique.insert(splint_id),
            "captured Dojo pane set contains a duplicate"
        );
        let splint = root
            .find_splint(splint_id)
            .context("captured Dojo pane disappeared before termination")?;
        anyhow::ensure!(
            splint.last_incarnation == Some(incarnation),
            "captured Dojo pane incarnation changed before termination"
        );
        if !matches!(splint.state, SplintState::Exited(_)) {
            live.push((splint_id, incarnation));
        }
    }
    Ok(live)
}

async fn terminate_dojo(
    connection: &mut Connection,
    dojo_id: DojoId,
    captured: &[(SplintId, u64)],
) -> Result<()> {
    for &(splint_id, incarnation) in captured {
        let (_, Some(root)) = inspect_optional_dojo_state(connection, dojo_id).await? else {
            return Ok(());
        };
        let live = captured_dojo_kill_targets(&root, captured)?;
        if !live.contains(&(splint_id, incarnation)) {
            continue;
        }
        match connection
            .request(Request::KillSplint {
                splint_id,
                incarnation,
            })
            .await?
        {
            Response::SplintKilled {
                splint_id: killed_id,
                incarnation: killed_incarnation,
                ..
            } if killed_id == splint_id && killed_incarnation == incarnation => {}
            response => bail!("splinterd returned unexpected Dojo kill response: {response:?}"),
        }
    }

    let (mut revision, root) = inspect_optional_dojo_state(connection, dojo_id).await?;
    let Some(root) = root else {
        return Ok(());
    };
    anyhow::ensure!(
        captured_dojo_kill_targets(&root, captured)?.is_empty(),
        "captured Dojo retained a live pane after termination"
    );
    for attempt in 0..=MAX_CLOSE_TOPOLOGY_RETRIES {
        match connection
            .request(Request::CloseDojo {
                expected_topology_revision: revision,
                dojo_id,
            })
            .await
        {
            Ok(Response::TopologyCommitted { .. }) => return Ok(()),
            Ok(response) => {
                bail!("splinterd returned unexpected Dojo close response: {response:?}");
            }
            Err(error)
                if protocol_error(&error).is_some_and(|failure| {
                    matches!(failure.code, ErrorCode::NotFound | ErrorCode::StaleTopology)
                }) =>
            {
                let (refreshed_revision, refreshed_root) =
                    inspect_optional_dojo_state(connection, dojo_id).await?;
                let Some(refreshed_root) = refreshed_root else {
                    return Ok(());
                };
                anyhow::ensure!(
                    captured_dojo_kill_targets(&refreshed_root, captured)?.is_empty(),
                    "captured Dojo changed during closure"
                );
                if attempt == MAX_CLOSE_TOPOLOGY_RETRIES {
                    return Err(error).context("bounded Dojo close retries exhausted");
                }
                revision = refreshed_revision;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("bounded Dojo close retry loop returns on its final attempt")
}

async fn close_focused_splint(
    connection: &mut Connection,
    dojo_id: DojoId,
    root: &LayoutNode,
    expected_topology_revision: TopologyRevision,
    target: SplintId,
) -> Result<TopologyCommandOutcome> {
    // Closing a live pane ends its process, and the Window then asks to
    // close the exited pane as well; that second request finds the pane
    // already closed, which is what it asked for.
    if close_target_already_closed(root, target) {
        return Ok(TopologyCommandOutcome::Updated {
            pending_focus: None,
        });
    }
    let (mut close_revision, mut close_root, expected_incarnation) =
        match close_action(root, target)? {
            CloseAction::CloseExited => (expected_topology_revision, root.clone(), None),
            CloseAction::KillAndClose { incarnation } => {
                match connection
                    .request(Request::KillSplint {
                        splint_id: target,
                        incarnation,
                    })
                    .await?
                {
                    Response::SplintKilled {
                        splint_id,
                        incarnation: killed_incarnation,
                        ..
                    } if splint_id == target && killed_incarnation == incarnation => {}
                    response => bail!("splinterd returned unexpected kill response: {response:?}"),
                }
                let (revision, refreshed_root) = inspect_dojo_state(connection, dojo_id).await?;
                (revision, refreshed_root, Some(incarnation))
            }
        };

    for attempt in 0..=MAX_CLOSE_TOPOLOGY_RETRIES {
        let final_leaf = validate_exited_close_target(&close_root, target, expected_incarnation)?;
        match connection
            .request(Request::CloseSplint {
                expected_topology_revision: close_revision,
                splint_id: target,
            })
            .await
        {
            Ok(Response::TopologyCommitted { .. }) if final_leaf => {
                return Ok(TopologyCommandOutcome::WindowClosed);
            }
            Ok(Response::TopologyCommitted { .. }) => {
                return Ok(TopologyCommandOutcome::Updated {
                    pending_focus: None,
                });
            }
            Ok(response) => {
                bail!("splinterd returned unexpected close response: {response:?}");
            }
            Err(error)
                if protocol_error(&error)
                    .is_some_and(|failure| failure.code == ErrorCode::StaleTopology)
                    && attempt < MAX_CLOSE_TOPOLOGY_RETRIES =>
            {
                let (revision, refreshed_root) =
                    inspect_optional_dojo_state(connection, dojo_id).await?;
                match refreshed_close_state(refreshed_root.as_ref(), target, expected_incarnation)?
                {
                    RefreshedCloseState::WindowClosed => {
                        return Ok(TopologyCommandOutcome::WindowClosed);
                    }
                    RefreshedCloseState::TargetClosed => {
                        return Ok(TopologyCommandOutcome::Updated {
                            pending_focus: None,
                        });
                    }
                    RefreshedCloseState::Retry => {}
                }
                close_revision = revision;
                close_root = refreshed_root.context("close retry Dojo disappeared")?;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("bounded close retry loop returns on its final attempt")
}

#[allow(
    clippy::too_many_lines,
    reason = "the closed topology command set preserves exact endpoint and revision handling"
)]
async fn apply_topology_command(
    factory: &ConnectionFactory,
    connection: &mut Connection,
    config: &AppConfig,
    dojo_id: DojoId,
    root: &LayoutNode,
    expected_topology_revision: TopologyRevision,
    command: WindowTopologyCommand,
) -> Result<TopologyCommandOutcome> {
    if let WindowTopologyCommand::Close {
        dojo_id: target_dojo,
        target,
    } = &command
    {
        anyhow::ensure!(
            *target_dojo == dojo_id,
            "topology close targeted another Dojo"
        );
        return close_focused_splint(
            connection,
            dojo_id,
            root,
            expected_topology_revision,
            *target,
        )
        .await;
    }
    let pending_placeholder = match &command {
        WindowTopologyCommand::Split { pending, .. } => *pending,
        _ => None,
    };
    let request = match command {
        WindowTopologyCommand::Split {
            dojo_id: target_dojo,
            target,
            axis,
            pending: _,
        } => {
            anyhow::ensure!(
                target_dojo == dojo_id,
                "topology split targeted another Dojo"
            );
            let ratio = SplitRatio::new(500).expect("fixed split ratio is valid");
            match factory.capabilities().launch_semantics {
                LaunchSemantics::LocalTrusted => Request::SplitSplint {
                    expected_topology_revision,
                    target_splint_id: target,
                    axis,
                    side: SplitSide::Second,
                    ratio,
                    launch: launch_parameters(
                        creation_directory(connection, target, &config.new_splint_directory)
                            .await?,
                        Vec::new(),
                        config,
                    ),
                },
                LaunchSemantics::RemoteInteractive => Request::SplitSplintAutomation {
                    expected_topology_revision,
                    target_splint_id: target,
                    axis,
                    side: SplitSide::Second,
                    ratio,
                    // The remote daemon inherits the source cwd itself; other
                    // choices resolve there first.
                    launch: automation_launch(
                        match config.new_splint_directory {
                            LaunchDirectory::Inherit => None,
                            ref directory => {
                                Some(creation_directory(connection, target, directory).await?)
                            }
                        },
                        Vec::new(),
                    ),
                },
            }
        }
        WindowTopologyCommand::AdjustRatio {
            dojo_id: target_dojo,
            target,
            delta,
        } => {
            anyhow::ensure!(target_dojo == dojo_id, "ratio edit targeted another Dojo");
            let current = i32::from(
                parent_ratio(root, target)
                    .context("focused pane has no adjustable parent ratio")?
                    .get(),
            );
            let next = u16::try_from((current + i32::from(delta)).clamp(1, 999))?;
            Request::SetSplitRatio {
                expected_topology_revision,
                target_splint_id: target,
                ancestor: 0,
                ratio: SplitRatio::new(next).map_err(|_| anyhow::anyhow!("invalid ratio"))?,
            }
        }
        WindowTopologyCommand::SetRatio {
            dojo_id: target_dojo,
            target,
            ancestor,
            ratio,
        } => {
            anyhow::ensure!(target_dojo == dojo_id, "ratio edit targeted another Dojo");
            Request::SetSplitRatio {
                expected_topology_revision,
                target_splint_id: target,
                ancestor,
                ratio,
            }
        }
        WindowTopologyCommand::Close { .. } => unreachable!("close handled above"),
        WindowTopologyCommand::CloseWindow
        | WindowTopologyCommand::ExplorerContext { .. }
        | WindowTopologyCommand::RequestDojoPrompt { .. }
        | WindowTopologyCommand::RequestSessionPicker
        | WindowTopologyCommand::RequestLairExplorer { .. }
        | WindowTopologyCommand::RequestSelector { .. }
        | WindowTopologyCommand::OpenDojo { .. }
        | WindowTopologyCommand::FocusSplint { .. }
        | WindowTopologyCommand::NewLair { .. }
        | WindowTopologyCommand::PickerNewLair { .. }
        | WindowTopologyCommand::NewDojo { .. }
        | WindowTopologyCommand::PickerNewDojo { .. }
        | WindowTopologyCommand::MaterializePreset { .. }
        | WindowTopologyCommand::NavigateLair { .. }
        | WindowTopologyCommand::RequestLairPrompt { .. }
        | WindowTopologyCommand::RequestDojoRestorePrompt { .. }
        | WindowTopologyCommand::RenameLair { .. }
        | WindowTopologyCommand::TerminateLair { .. }
        | WindowTopologyCommand::SetLairRetention { .. }
        | WindowTopologyCommand::RestoreLair { .. }
        | WindowTopologyCommand::RestoreDojo { .. }
        | WindowTopologyCommand::RenameDojo { .. }
        | WindowTopologyCommand::TerminateDojo { .. }
        | WindowTopologyCommand::ActivateTab { .. }
        | WindowTopologyCommand::CloseTab { .. }
        | WindowTopologyCommand::CloseTabs { .. }
        | WindowTopologyCommand::ClearAgentStatus { .. }
        | WindowTopologyCommand::Access(_) => {
            unreachable!("session commands are handled by the topology manager")
        }
    };
    topology_command_outcome(connection.request(request).await?, pending_placeholder)
}

fn topology_command_outcome(
    response: Response,
    pending_placeholder: Option<SplintId>,
) -> Result<TopologyCommandOutcome> {
    match response {
        Response::SplintStarted {
            splint_id,
            topology_revision,
            ..
        } => Ok(TopologyCommandOutcome::Updated {
            pending_focus: Some(PendingTopologyFocus {
                splint_id,
                revision: topology_revision,
                placeholder: pending_placeholder,
            }),
        }),
        Response::TopologyCommitted { .. } => Ok(TopologyCommandOutcome::Updated {
            pending_focus: None,
        }),
        response => bail!("splinterd returned unexpected topology response: {response:?}"),
    }
}

fn topology_identity_diff(
    previous: &LayoutNode,
    current: &LayoutNode,
) -> (Vec<SplintId>, Vec<SplintId>) {
    let mut previous_ids = Vec::new();
    let mut current_ids = Vec::new();
    layout_splint_ids(previous, &mut previous_ids);
    layout_splint_ids(current, &mut current_ids);
    let previous_ids = previous_ids
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    let current_ids = current_ids
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    // Restoration keeps the durable ID but replaces the process and its streams.
    // Never try to attach an already-exited incarnation.
    let replacements = current_ids
        .intersection(&previous_ids)
        .copied()
        .filter(|id| {
            let old = previous
                .find_splint(*id)
                .expect("collected previous Splint");
            let next = current.find_splint(*id).expect("collected current Splint");
            next.last_incarnation.is_some()
                && next.last_incarnation != old.last_incarnation
                && !matches!(next.state, SplintState::Exited(_))
        })
        .collect::<Vec<_>>();
    (
        current_ids
            .difference(&previous_ids)
            .copied()
            .chain(replacements.iter().copied())
            .collect(),
        previous_ids
            .difference(&current_ids)
            .copied()
            .chain(replacements)
            .collect(),
    )
}

fn pending_focus_for_observation(
    pending: Option<PendingTopologyFocus>,
    observed_revision: TopologyRevision,
    added: &[SplintId],
) -> (Option<SplintId>, Option<SplintId>, bool) {
    let Some(pending) = pending else {
        return (None, None, false);
    };
    if observed_revision < pending.revision {
        return (None, None, false);
    }
    (
        added
            .contains(&pending.splint_id)
            .then_some(pending.splint_id),
        pending.placeholder,
        true,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "one transactional reconciliation owns identity, layout, updates, and task cleanup"
)]
async fn reconcile_window_topology(
    factory: &ConnectionFactory,
    config: &AppConfig,
    image_cache: &SharedImageContentCache,
    topology_revision: TopologyRevision,
    dojo_id: DojoId,
    root: &mut LayoutNode,
    next: LayoutNode,
    focused: Option<SplintId>,
    placeholder: Option<SplintId>,
    updates: &mpsc::Sender<WindowTopologyUpdate>,
    pane_tasks: &mut HashMap<SplintId, PaneTask>,
) -> Result<bool> {
    if *root == next {
        let Some(placeholder) = placeholder else {
            return Ok(true);
        };
        return Ok(updates
            .send(WindowTopologyUpdate::Apply {
                topology_revision,
                dojo_id,
                layout: next,
                added: Vec::new(),
                removed: vec![placeholder],
                focused,
            })
            .await
            .is_ok());
    }
    let (added_ids, mut removed) = topology_identity_diff(root, &next);
    if let Some(placeholder) = placeholder {
        removed.push(placeholder);
    }
    let mut prepared = Vec::new();
    for splint_id in added_ids {
        let expected_incarnation = next
            .find_splint(splint_id)
            .and_then(|splint| splint.last_incarnation);
        match prepare_live_pane_at_incarnation(
            factory,
            config,
            splint_id,
            expected_incarnation,
            image_cache.clone(),
            false,
        )
        .await
        {
            Ok(pane) => prepared.push((splint_id, pane)),
            Err(error) => {
                let tasks = prepared
                    .into_iter()
                    .map(|(splint_id, pane)| (splint_id, pane.task))
                    .collect();
                cancel_pane_tasks(tasks).await;
                return Err(error);
            }
        }
    }
    let mut added = Vec::with_capacity(prepared.len());
    let mut new_tasks = HashMap::with_capacity(prepared.len());
    for (splint_id, pane) in prepared {
        added.push(pane.options);
        new_tasks.insert(splint_id, pane.task);
    }
    if updates
        .send(WindowTopologyUpdate::Apply {
            topology_revision,
            dojo_id,
            layout: next.clone(),
            added,
            removed: removed.clone(),
            focused,
        })
        .await
        .is_err()
    {
        cancel_pane_tasks(new_tasks).await;
        return Ok(false);
    }
    let mut removed_tasks = HashMap::new();
    for removed_id in &removed {
        if let Some(task) = pane_tasks.remove(removed_id) {
            removed_tasks.insert(*removed_id, task);
        }
    }
    retire_pane_tasks(removed_tasks);
    pane_tasks.extend(new_tasks);
    *root = next;
    Ok(true)
}

fn picker_catalog(view: NavigationPickerView) -> SessionPickerCatalog {
    let (creation_enabled, creation_blocker) = match view.creation {
        Some(capability) => match capability.availability {
            NavigationAvailability::Enabled => (true, None),
            NavigationAvailability::Disabled(blocker) => (false, Some(blocker.message())),
        },
        None => (false, Some("Current Lair unavailable")),
    };
    let mut items = Vec::with_capacity(view.rows.len());
    let mut targets = Vec::with_capacity(view.rows.len());
    for row in view.rows {
        items.push(SessionPickerItem {
            display_title: row.display_title,
            breadcrumb: row.breadcrumb,
            working_directory: row.working_directory,
            pane_count: row.pane_count,
            running_pane_count: row.running_pane_count,
            agent: row.agent,
        });
        targets.push(SessionPickerTarget {
            topology_revision: row.target.topology_revision,
            lair_id: row.target.lair_id,
            dojo_id: row.target.dojo_id,
            action: row.target.action,
        });
    }
    SessionPickerCatalog {
        items,
        targets,
        creation_enabled,
        creation_blocker,
        creation_target: view.creation.map(|capability| SessionPickerCreationTarget {
            topology_revision: view.topology_revision,
            action: capability.action,
        }),
        initial_row: view.initial_row,
    }
}

fn navigation_projection<T>(
    factory: &ConnectionFactory,
    snapshot: &splinterm_protocol::TopologySnapshot,
    current_lair_id: Option<LairId>,
    focused_splint: Option<SplintId>,
    tabs: &WindowTabSet<T>,
) -> Result<NavigationProjection> {
    let attached_dojos = tabs.iter().map(|tab| tab.dojo_id).collect::<Vec<_>>();
    let active_dojo = tabs.active().map(|tab| tab.dojo_id);
    NavigationProjection::build(
        snapshot,
        NavigationProjectionContext {
            endpoint_namespace: &factory.capabilities().recency_namespace,
            endpoint_generation: 1,
            freshness: EndpointFreshness::Current,
            window: NavigationWindowState {
                attached_dojos: &attached_dojos,
                active_dojo,
                focused_splint,
                current_lair: current_lair_id,
                has_tab_capacity: window_has_tab_capacity(tabs.len()),
            },
            authority: NavigationAuthority {
                attach: NavigationPermission::Allowed,
                restore: NavigationPermission::Allowed,
                create_lair: NavigationPermission::Allowed,
                create_dojo: NavigationPermission::Allowed,
            },
        },
    )
    .map_err(anyhow::Error::new)
}

async fn recent_dojo_catalog<T>(
    factory: &ConnectionFactory,
    connection: &mut Connection,
    tabs: &WindowTabSet<T>,
) -> Result<SessionPickerCatalog> {
    let Response::Topology { snapshot } = connection.request(Request::InspectTopology).await?
    else {
        bail!("splinterd did not return its topology");
    };
    let projection = navigation_projection(factory, &snapshot, None, None, tabs)?;
    Ok(picker_catalog(projection.picker_view(
        NavigationPickerKind::RecentDojos,
        &recent_dojo_ids(factory),
        &[],
    )))
}

async fn lair_explorer_view<T>(
    factory: &ConnectionFactory,
    connection: &mut Connection,
    focused_splint: Option<SplintId>,
    tabs: &WindowTabSet<T>,
) -> Result<NavigationExplorerView> {
    let Response::Topology { snapshot } = connection.request(Request::InspectTopology).await?
    else {
        bail!("splinterd did not return its topology");
    };
    let current_lair = tabs.active().map(|tab| tab.lair_id);
    Ok(
        navigation_projection(factory, &snapshot, current_lair, focused_splint, tabs)?
            .explorer_view(),
    )
}

async fn selector_catalog<T>(
    factory: &ConnectionFactory,
    connection: &mut Connection,
    kind: SelectorKind,
    current_lair_id: LairId,
    tabs: &WindowTabSet<T>,
) -> Result<SessionPickerCatalog> {
    let Response::Topology { snapshot } = connection.request(Request::InspectTopology).await?
    else {
        bail!("splinterd did not return its topology");
    };
    let projection = navigation_projection(factory, &snapshot, Some(current_lair_id), None, tabs)?;
    let representatives = projection
        .lairs
        .iter()
        .filter_map(|lair| tabs.recent_in_lair(lair.lair_id))
        .collect::<Vec<_>>();
    let picker_kind = match kind {
        SelectorKind::Dojo => NavigationPickerKind::ChooseDojo {
            lair_id: current_lair_id,
        },
        SelectorKind::Lair => NavigationPickerKind::ChooseLair,
    };
    Ok(picker_catalog(projection.picker_view(
        picker_kind,
        &recent_dojo_ids(factory),
        &representatives,
    )))
}

fn window_dojo_identity(
    topology_revision: TopologyRevision,
    lair: &splinterm_core::Lair,
    dojo: &splinterm_core::Dojo,
) -> WindowDojoIdentity {
    WindowDojoIdentity {
        topology_revision,
        lair_id: lair.id,
        dojo_id: dojo.id,
        lair_name: lair.name.clone(),
        lair_retention: lair.retention,
        dojo_name: dojo.name.clone(),
    }
}

fn materialized_dojo_targets(
    lairs: &[splinterm_core::Lair],
    topology_revision: TopologyRevision,
    lair_id: LairId,
    dojo_ids: &[DojoId],
    panes: &[splinterm_protocol::PresetPaneIdentity],
) -> Result<Vec<(WindowDojoIdentity, splinterm_core::Dojo)>> {
    validate_preset_materialized(dojo_ids, panes)
        .map_err(|error| anyhow::anyhow!(error.message))?;
    let lair = lairs
        .iter()
        .find(|lair| lair.id == lair_id)
        .context("materialized Lair is absent from committed topology")?;
    let mut targets = Vec::with_capacity(dojo_ids.len());
    for dojo_id in dojo_ids {
        let dojo = lair
            .dojos
            .iter()
            .find(|dojo| dojo.id == *dojo_id)
            .context("materialized Dojo is absent from committed topology")?;
        let mapped = panes
            .iter()
            .filter(|pane| pane.dojo_id == *dojo_id)
            .collect::<Vec<_>>();
        anyhow::ensure!(
            mapped.len() == dojo.root.splint_count()
                && mapped
                    .iter()
                    .all(|pane| dojo.root.find_splint(pane.splint_id).is_some()),
            "preset pane mapping disagrees with committed Dojo"
        );
        targets.push((
            window_dojo_identity(topology_revision, lair, dojo),
            dojo.clone(),
        ));
    }
    Ok(targets)
}

fn select_live_dojo_from(
    lairs: &[splinterm_core::Lair],
    lair_id: LairId,
    dojo_id: DojoId,
) -> Result<splinterm_core::Dojo> {
    let dojo = select_dojo_from(lairs, (lair_id, dojo_id))?;
    anyhow::ensure!(
        dojo_has_fully_running_pane_layout(&dojo),
        "selected session no longer has a fully running pane layout"
    );
    Ok(dojo)
}

async fn reopenable_dojo(
    connection: &mut Connection,
    lair_id: LairId,
    dojo_id: DojoId,
) -> Result<(WindowDojoIdentity, splinterm_core::Dojo)> {
    let Response::Lairs {
        lairs,
        topology_revision,
    } = connection.request(Request::ListLairs).await?
    else {
        bail!("splinterd did not return its session list");
    };
    let lair = lairs
        .iter()
        .find(|lair| lair.id == lair_id)
        .context("selected Lair is absent")?;
    let dojo = select_live_dojo_from(&lairs, lair_id, dojo_id)?;
    Ok((window_dojo_identity(topology_revision, lair, &dojo), dojo))
}

async fn revision_bound_picker_dojo(
    connection: &mut Connection,
    target: SessionPickerTarget,
    require_fully_running: bool,
) -> Result<(WindowDojoIdentity, splinterm_core::Dojo)> {
    let Response::Topology { snapshot } = connection.request(Request::InspectTopology).await?
    else {
        bail!("splinterd did not return its topology");
    };
    anyhow::ensure!(
        snapshot.revision == target.topology_revision,
        "selected picker target is stale"
    );
    let lairs = snapshot.topology.lairs().cloned().collect::<Vec<_>>();
    let lair = lairs
        .iter()
        .find(|lair| lair.id == target.lair_id)
        .context("selected Lair is absent")?;
    let dojo = if require_fully_running {
        select_live_dojo_from(&lairs, target.lair_id, target.dojo_id)?
    } else {
        select_dojo_from(&lairs, (target.lair_id, target.dojo_id))?
    };
    Ok((window_dojo_identity(snapshot.revision, lair, &dojo), dojo))
}

fn explorer_splint_from_snapshot(
    snapshot: &splinterm_protocol::TopologySnapshot,
    target: NavigationExplorerSplintTarget,
    require_fully_running: bool,
) -> Result<(WindowDojoIdentity, splinterm_core::Dojo)> {
    anyhow::ensure!(
        target.capability.is_enabled(),
        "selected explorer target is unavailable"
    );
    anyhow::ensure!(
        snapshot.revision == target.topology_revision,
        "selected explorer target is stale"
    );
    let lair = snapshot
        .topology
        .lairs()
        .find(|lair| lair.id == target.lair_id)
        .context("selected Lair is absent")?;
    let dojo = lair
        .dojos
        .iter()
        .find(|dojo| dojo.id == target.dojo_id)
        .context("selected Dojo is absent")?;
    anyhow::ensure!(
        dojo.root.find_splint(target.splint_id).is_some(),
        "selected Splint is absent from its parent Dojo"
    );
    let runtime = snapshot
        .runtimes
        .iter()
        .find(|runtime| runtime.splint_id == target.splint_id)
        .context("selected Splint runtime is absent")?;
    anyhow::ensure!(
        runtime.live_incarnation == target.live_incarnation
            && runtime.last_incarnation == target.last_incarnation,
        "selected Splint incarnation changed"
    );
    match target.capability.action {
        NavigationAction::FocusSplint | NavigationAction::AttachAndFocusSplint => {
            anyhow::ensure!(
                matches!(
                    runtime.lifecycle,
                    splinterm_protocol::SplintLifecycle::Starting
                        | splinterm_protocol::SplintLifecycle::Running
                ),
                "selected Splint is no longer live"
            );
        }
        NavigationAction::PreviewRestoreSplint => {
            anyhow::ensure!(
                runtime.lifecycle == splinterm_protocol::SplintLifecycle::Exited
                    && runtime.restorable,
                "selected Splint is no longer restorable"
            );
        }
        _ => bail!("selected explorer action disagrees with Splint target"),
    }
    if require_fully_running {
        anyhow::ensure!(
            dojo_has_fully_running_pane_layout(dojo),
            "selected Splint parent is no longer fully running"
        );
    }
    Ok((
        window_dojo_identity(snapshot.revision, lair, dojo),
        dojo.clone(),
    ))
}

async fn revision_bound_explorer_splint(
    connection: &mut Connection,
    target: NavigationExplorerSplintTarget,
    require_fully_running: bool,
) -> Result<(WindowDojoIdentity, splinterm_core::Dojo)> {
    let Response::Topology { snapshot } = connection.request(Request::InspectTopology).await?
    else {
        bail!("splinterd did not return its topology");
    };
    explorer_splint_from_snapshot(&snapshot, target, require_fully_running)
}

async fn create_daily_dojo(
    factory: &ConnectionFactory,
    connection: &mut Connection,
    config: &AppConfig,
    source_splint_id: SplintId,
    captured_revision: Option<TopologyRevision>,
) -> Result<ManagedWindowOpen> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let expected = connection.topology_revision().await?;
    if let Some(captured_revision) = captured_revision {
        anyhow::ensure!(
            captured_revision == expected,
            "picker creation target is stale"
        );
    }
    // A new Lair starts like a new Dojo.
    let cwd = creation_directory(connection, source_splint_id, &config.new_dojo_directory).await?;
    let (request, lifetime) = graphical_create_request(
        factory,
        expected,
        splinterm_core::generated_lair_name(stamp, std::process::id()),
        false,
        Some(cwd),
        Vec::new(),
        config,
    )?;
    let (response, transient_owner) = match lifetime {
        GraphicalLairLifetime::Persistent => (connection.request(request).await?, None),
        GraphicalLairLifetime::ClientBound => {
            let mut owner = factory.connect().await?;
            let response = owner.request(request).await?;
            (response, Some(owner))
        }
    };
    let Response::LairCreated {
        lair,
        topology_revision,
        ..
    } = response
    else {
        bail!("splinterd did not create the requested terminal");
    };
    let dojo = lair
        .dojos
        .first()
        .cloned()
        .context("new Lair did not contain a Dojo")?;
    Ok(ManagedWindowOpen {
        identity: window_dojo_identity(topology_revision, &lair, &dojo),
        dojo,
        remember: transient_owner.is_none(),
        transient_owner,
        requested_focus: None,
    })
}

async fn create_dojo_in_lair(
    factory: &ConnectionFactory,
    persistent_connection: &mut Connection,
    transient_owner: Option<&mut Connection>,
    config: &AppConfig,
    lair_id: LairId,
    source_splint_id: SplintId,
    captured_revision: Option<TopologyRevision>,
) -> Result<(WindowDojoIdentity, splinterm_core::Dojo, bool)> {
    let (connection, uses_transient_owner) = match transient_owner {
        Some(owner) => (owner, true),
        None => (persistent_connection, false),
    };
    let Response::Lairs {
        lairs,
        topology_revision: expected_topology_revision,
    } = connection.request(Request::ListLairs).await?
    else {
        bail!("splinterd did not return its Lair catalog");
    };
    if let Some(captured_revision) = captured_revision {
        anyhow::ensure!(
            captured_revision == expected_topology_revision,
            "picker creation target is stale"
        );
    }
    let cwd = creation_directory(connection, source_splint_id, &config.new_dojo_directory).await?;
    let request = new_dojo_request_for(
        factory.capabilities().launch_semantics,
        expected_topology_revision,
        &lairs,
        lair_id,
        None,
        Some(cwd),
        Vec::new(),
        tab_organization_promotes(config, uses_transient_owner),
        config,
    )?;
    let Response::DojoStarted { dojo_id, .. } = connection.request(request).await? else {
        bail!("splinterd did not create the requested Dojo");
    };
    let (identity, dojo) = reopenable_dojo(connection, lair_id, dojo_id).await?;
    Ok((
        identity,
        dojo,
        tab_organization_promotes(config, uses_transient_owner),
    ))
}

#[derive(Clone, Copy)]
enum LairTargetState {
    Exited,
    Live,
    All,
}

fn collect_lair_targets(
    lair: &splinterm_core::Lair,
    target_state: LairTargetState,
) -> Result<Vec<MutationTarget>> {
    collect_scoped_lair_targets(lair, target_state, None)
}

fn collect_scoped_lair_targets(
    lair: &splinterm_core::Lair,
    target_state: LairTargetState,
    dojo_id: Option<DojoId>,
) -> Result<Vec<MutationTarget>> {
    fn collect(
        lair_id: LairId,
        dojo_id: DojoId,
        node: &LayoutNode,
        target_state: LairTargetState,
        targets: &mut Vec<MutationTarget>,
    ) -> Result<()> {
        match node {
            LayoutNode::Leaf(splint)
                if match target_state {
                    LairTargetState::All => true,
                    LairTargetState::Exited => matches!(splint.state, SplintState::Exited(_)),
                    LairTargetState::Live => {
                        matches!(splint.state, SplintState::Starting | SplintState::Running)
                    }
                } =>
            {
                targets.push(MutationTarget {
                    lair_id,
                    dojo_id,
                    splint_id: splint.id,
                    incarnation: splint
                        .last_incarnation
                        .context("captured Lair pane has no process incarnation")?,
                });
            }
            LayoutNode::Leaf(_) => {}
            LayoutNode::Branch { first, second, .. } => {
                collect(lair_id, dojo_id, first, target_state, targets)?;
                collect(lair_id, dojo_id, second, target_state, targets)?;
            }
        }
        Ok(())
    }

    let mut targets = Vec::new();
    for dojo in lair
        .dojos
        .iter()
        .filter(|dojo| dojo_id.is_none_or(|id| dojo.id == id))
    {
        collect(lair.id, dojo.id, &dojo.root, target_state, &mut targets)?;
    }
    Ok(targets)
}

/// The name a prompt shows for `lair`: its Explorer label ("Lair N" for a
/// generated name) from the whole catalog `lairs`, else its stored name, else
/// its ID when it has no name. Callers make it one line before showing it.
fn lair_prompt_label(lairs: &[splinterm_core::Lair], lair: &splinterm_core::Lair) -> String {
    lair_display_label(lairs, lair.id).unwrap_or_else(|| {
        if lair.name.trim().is_empty() {
            lair.id.to_string()
        } else {
            lair.name.clone()
        }
    })
}

/// The name a prompt shows for the Dojo `dojo_id` of `lair`: its Explorer
/// label ("Dojo N" for a generated name).
fn dojo_prompt_label(lair: &splinterm_core::Lair, dojo_id: DojoId) -> Option<String> {
    lair.dojos
        .iter()
        .zip(dojo_labels(lair))
        .find_map(|(dojo, label)| (dojo.id == dojo_id).then_some(label))
}

/// A saved Lair's preview for its prompts: a summary line naming the Lair as
/// the Explorer does (from the whole catalog `lairs`), then one tab-separated
/// row per Splint (its Dojo, what it launches, its directory), which the
/// prompt lines up as a table, then where it came from and what a restore
/// leaves out. `restoring` keeps only the rows of the Splints a restore
/// launches, so its count and its rows agree. Every name and path is made one
/// line, so none can add a line or a cell, and a directory under `home`
/// starts with `~`.
fn saved_layout_preview(
    lairs: &[splinterm_core::Lair],
    lair: &splinterm_core::Lair,
    restoring: Option<&[MutationTarget]>,
    home: Option<&std::path::Path>,
) -> String {
    /// What a restore launches, from the trusted recipe rather than the
    /// title the program set: the executable's file name and its arguments,
    /// or the shell.
    fn recipe(splint: &splinterm_core::Splint) -> String {
        match splint.command.split_first() {
            None => "shell".to_owned(),
            Some((executable, _)) if executable.trim().is_empty() => NOT_RESTORABLE.to_owned(),
            Some((executable, arguments)) => {
                let name = std::path::Path::new(executable).file_name().map_or_else(
                    || executable.clone(),
                    |name| name.to_string_lossy().into_owned(),
                );
                single_line(
                    &std::iter::once(name.as_str())
                        .chain(arguments.iter().map(String::as_str))
                        .collect::<Vec<_>>()
                        .join(" "),
                )
            }
        }
    }
    fn leaves(
        dojo: &str,
        node: &LayoutNode,
        restoring: Option<&[MutationTarget]>,
        home: Option<&std::path::Path>,
        output: &mut Vec<String>,
    ) {
        match node {
            LayoutNode::Leaf(splint)
                if restoring
                    .is_some_and(|targets| !targets.iter().any(|t| t.splint_id == splint.id)) => {}
            LayoutNode::Leaf(splint) => output.push(format!(
                "{}\t{}\t{}",
                single_line(dojo),
                recipe(splint),
                single_line(&home_relative(&splint.cwd, home))
            )),
            LayoutNode::Branch { first, second, .. } => {
                leaves(dojo, first, restoring, home, output);
                leaves(dojo, second, restoring, home, output);
            }
        }
    }
    let dojos = lair.dojos.len();
    let retention = format!("{:?}", lair.retention).to_lowercase();
    let mut lines = vec![format!(
        "{} · {retention} · {dojos} Dojo{}",
        single_line(&lair_prompt_label(lairs, lair)),
        if dojos == 1 { "" } else { "s" }
    )];
    for (shown, label) in lair.dojos.iter().zip(dojo_labels(lair)) {
        leaves(&label, &shown.root, restoring, home, &mut lines);
    }
    if let Some(provenance) = &lair.provenance {
        lines.push(format!("Origin: {}", single_line(&provenance.to_string())));
    }
    lines.push(
        "Not restored: scrollback, running programs' state, shell state, environment, clipboard, images."
            .to_owned(),
    );
    lines.join("\n")
}

/// `path` with a leading `home` shown as `~`.
fn home_relative(path: &std::path::Path, home: Option<&std::path::Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// The user's home directory, for [`saved_layout_preview`].
fn preview_home() -> Option<std::path::PathBuf> {
    env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(std::path::PathBuf::from)
        .filter(|home| home.is_absolute() && home.parent().is_some())
}

fn build_lair_prompt_target(
    topology_revision: TopologyRevision,
    lairs: &[splinterm_core::Lair],
    lair: &splinterm_core::Lair,
    kind: LairPromptKind,
    expected_retention: Option<splinterm_core::LairRetention>,
) -> Result<LairPromptTarget> {
    if let Some(expected_retention) = expected_retention {
        anyhow::ensure!(
            lair.retention == expected_retention,
            "captured Lair retention changed"
        );
    }
    if matches!(kind, LairPromptKind::Preview | LairPromptKind::Restore) {
        anyhow::ensure!(
            lair.retention.is_protected(),
            "saved Lair action targeted a disposable Lair"
        );
    }
    let targets = match kind {
        LairPromptKind::Restore => collect_lair_targets(lair, LairTargetState::Exited)?,
        LairPromptKind::Terminate => collect_lair_targets(lair, LairTargetState::Live)?,
        LairPromptKind::Rename | LairPromptKind::Preview => Vec::new(),
    };
    if kind == LairPromptKind::Restore {
        anyhow::ensure!(!targets.is_empty(), "saved Lair has no restorable Splints");
    }
    Ok(LairPromptTarget {
        topology_revision,
        lair_id: lair.id,
        dojo_id: None,
        // A rename edits the stored name; every other prompt names the Lair
        // as the Explorer does.
        name: if kind == LairPromptKind::Rename {
            lair.name.clone()
        } else {
            lair_prompt_label(lairs, lair)
        },
        retention: lair.retention,
        preview: saved_layout_preview(
            lairs,
            lair,
            (kind == LairPromptKind::Restore).then_some(targets.as_slice()),
            preview_home().as_deref(),
        ),
        targets,
    })
}

async fn lair_prompt_target(
    connection: &mut Connection,
    lair_id: LairId,
    kind: LairPromptKind,
    expected_retention: Option<splinterm_core::LairRetention>,
) -> Result<LairPromptTarget> {
    let Response::Lairs {
        lairs,
        topology_revision,
    } = connection.request(Request::ListLairs).await?
    else {
        bail!("splinterd did not return its Lair catalog");
    };
    let lair = lairs
        .iter()
        .find(|lair| lair.id == lair_id)
        .context("captured Lair is absent")?;
    build_lair_prompt_target(topology_revision, &lairs, lair, kind, expected_retention)
}

fn validate_lair_retention_target(
    lairs: &[splinterm_core::Lair],
    lair_id: LairId,
    expected_retention: splinterm_core::LairRetention,
) -> Result<()> {
    let lair = lairs
        .iter()
        .find(|lair| lair.id == lair_id)
        .context("captured Lair is absent")?;
    anyhow::ensure!(
        lair.retention == expected_retention,
        "captured Lair retention changed"
    );
    Ok(())
}

fn lair_navigation_target<T>(
    ordered: &[LairId],
    entries: &[SessionEntry],
    tabs: &WindowTabSet<T>,
    current_lair_id: LairId,
    direction: LairDirection,
) -> Result<(LairId, DojoId)> {
    anyhow::ensure!(!ordered.is_empty(), "no Lairs are available");
    let current = ordered
        .iter()
        .position(|lair_id| *lair_id == current_lair_id)
        .context("current Lair is absent from the captured catalog")?;
    for distance in 1..=ordered.len() {
        let index = match direction {
            LairDirection::Previous => {
                current
                    .saturating_add(ordered.len())
                    .saturating_sub(distance)
                    % ordered.len()
            }
            LairDirection::Next => current.saturating_add(distance) % ordered.len(),
        };
        let target_lair = ordered[index];
        let target_dojo = tabs.recent_in_lair(target_lair).or_else(|| {
            entries
                .iter()
                .find(|entry| entry.lair_id == target_lair)
                .map(|entry| entry.dojo_id)
        });
        if let Some(target_dojo) = target_dojo {
            return Ok((target_lair, target_dojo));
        }
    }
    bail!("captured Lair catalog has no reopenable Dojo")
}

async fn navigate_lair(
    factory: &ConnectionFactory,
    connection: &mut Connection,
    tabs: &WindowTabSet<ManagedDojo>,
    current_lair_id: LairId,
    direction: LairDirection,
) -> Result<(WindowDojoIdentity, splinterm_core::Dojo)> {
    let Response::Lairs { lairs, .. } = connection.request(Request::ListLairs).await? else {
        bail!("splinterd did not return its Lair catalog");
    };
    let recent = recent_dojo_ids(factory);
    let entries = collect_sessions(&lairs, &recent)
        .into_iter()
        .filter(SessionEntry::reopenable)
        .collect::<Vec<_>>();
    let ordered = lairs
        .iter()
        .filter(|lair| lair.lifetime.is_persistent())
        .map(|lair| lair.id)
        .collect::<Vec<_>>();
    let (target_lair, target_dojo) =
        lair_navigation_target(&ordered, &entries, tabs, current_lair_id, direction)?;
    reopenable_dojo(connection, target_lair, target_dojo).await
}

struct ManagedDojo {
    identity: WindowDojoIdentity,
    root: LayoutNode,
    pending_focus: Option<PendingTopologyFocus>,
    pane_tasks: HashMap<SplintId, PaneTask>,
}

async fn cancel_pane_tasks(pane_tasks: HashMap<SplintId, PaneTask>) {
    let tasks = pane_tasks.into_values().collect::<Vec<_>>();
    for task in &tasks {
        task.cancellation.cancel();
    }
    for task in tasks {
        let _ = task.task.await;
    }
}

fn retire_pane_tasks(pane_tasks: HashMap<SplintId, PaneTask>) {
    if pane_tasks.is_empty() {
        return;
    }
    tokio::spawn(cancel_pane_tasks(pane_tasks));
}

/// Bounds the unused-Dojo requests a closing Window makes before it exits.
const CLOSE_UNUSED_ON_EXIT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// Bounds how long Close Tab waits for its pane to detach.
const CLOSE_TAB_DETACH_DEADLINE: std::time::Duration = std::time::Duration::from_secs(3);

/// Detaches panes explicitly and waits, so the daemon no longer counts this
/// Window as attached when it decides whether their Dojo is unused.
async fn detach_pane_tasks(pane_tasks: HashMap<SplintId, PaneTask>) {
    for task in pane_tasks.values() {
        task.detach_on_cancel
            .store(true, std::sync::atomic::Ordering::Release);
    }
    cancel_pane_tasks(pane_tasks).await;
}

/// Whether closing this tab should ask the daemon to end its Dojo. Only a Dojo
/// with a single pane can be unused; the daemon decides the rest.
fn may_close_unused(config: &AppConfig, pane_tasks: &HashMap<SplintId, PaneTask>) -> bool {
    config.multiplexer_lifetime.close_unused && pane_tasks.len() <= 1
}

/// Asks the daemon to end a Dojo that was never used. A used Dojo, an error,
/// or a daemon without the request all leave the Dojo running, which is the
/// detach a closed tab always performed.
///
/// Returns whether the daemon removed the Dojo's Lair along with it.
async fn close_if_unused(connection: &mut Connection, dojo_id: DojoId) -> bool {
    let response = connection
        .request(Request::CloseUnusedDojo { dojo_id })
        .await;
    unused_dojo_closed(dojo_id, &response) == Some(true)
}

/// Whether the daemon confirmed closing this Dojo, and if so whether its Lair
/// went with it.
fn unused_dojo_closed(dojo_id: DojoId, response: &Result<Response>) -> Option<bool> {
    match response {
        Ok(Response::UnusedDojoClose {
            dojo_id: closed,
            outcome: UnusedDojoCloseOutcome::Closed { lair_removed, .. },
        }) if *closed == dojo_id => Some(*lair_removed),
        _ => None,
    }
}

/// Releases a tab the Window just removed: detach, or end the Dojo when the
/// configuration allows it and the daemon finds it unused.
///
/// Returns whether the daemon removed the tab's Lair too.
async fn release_closed_tab(
    connection: &mut Connection,
    config: &AppConfig,
    dojo_id: DojoId,
    pane_tasks: HashMap<SplintId, PaneTask>,
) -> bool {
    if !may_close_unused(config, &pane_tasks) {
        retire_pane_tasks(pane_tasks);
        return false;
    }
    // A pane that cannot finish detaching leaves the Dojo attached, so the
    // tab just detaches; its task finishes in the background.
    tokio::time::timeout(CLOSE_TAB_DETACH_DEADLINE, detach_pane_tasks(pane_tasks))
        .await
        .is_ok()
        && close_if_unused(connection, dojo_id).await
}

/// Releases a tab this Window closed. A Lair the daemon removed with its
/// Dojo no longer needs the transient owner connection this Window held for
/// it; keeping it would leave an idle daemon connection open for the rest of
/// the Window's life.
async fn release_managed_tab(
    connection: &mut Connection,
    config: &AppConfig,
    state: &mut TopologyManagerState,
    removed: DojoTab<ManagedDojo>,
) {
    let lair_id = removed.lair_id;
    if release_closed_tab(
        connection,
        config,
        removed.dojo_id,
        removed.value.pane_tasks,
    )
    .await
    {
        state.transient_owners.remove(&lair_id);
    }
}

/// On Window close, ends every remaining tab's Dojo that is unused, within a
/// bounded time so a stalled daemon cannot hold the Window open.
async fn close_unused_tabs_on_exit(
    connection: &mut Connection,
    config: &AppConfig,
    state: &mut TopologyManagerState,
) {
    let mut candidates = Vec::new();
    let mut pane_tasks = HashMap::new();
    for tab in state.tabs.iter_mut() {
        if may_close_unused(config, &tab.value.pane_tasks) {
            candidates.push(tab.dojo_id);
            pane_tasks.extend(tab.value.pane_tasks.drain());
        }
    }
    if candidates.is_empty() {
        return;
    }
    let _ = tokio::time::timeout(CLOSE_UNUSED_ON_EXIT_DEADLINE, async {
        detach_pane_tasks(pane_tasks).await;
        for dojo_id in candidates {
            close_if_unused(connection, dojo_id).await;
        }
    })
    .await;
}

struct PreparedManagedDojo {
    identity: WindowDojoIdentity,
    dojo: splinterm_core::Dojo,
    panes: Vec<WindowPaneOptions>,
    pane_tasks: HashMap<SplintId, PaneTask>,
}

async fn prepare_managed_dojo(
    factory: &ConnectionFactory,
    config: &AppConfig,
    image_cache: &SharedImageContentCache,
    identity: WindowDojoIdentity,
    dojo: splinterm_core::Dojo,
    requested_focus: Option<(SplintId, Option<u64>)>,
) -> Result<PreparedManagedDojo> {
    anyhow::ensure!(
        dojo.root.find_splint(dojo.default_focus).is_some(),
        "target Dojo focus is absent from its layout"
    );
    let mut ids = Vec::new();
    layout_splint_ids(&dojo.root, &mut ids);
    let mut panes = Vec::with_capacity(ids.len());
    let mut pane_tasks = HashMap::with_capacity(ids.len());
    for splint_id in ids {
        let expected_incarnation = requested_focus
            .filter(|(target, _)| *target == splint_id)
            .and_then(|(_, incarnation)| incarnation);
        match prepare_live_pane_at_incarnation(
            factory,
            config,
            splint_id,
            expected_incarnation,
            image_cache.clone(),
            false,
        )
        .await
        {
            Ok(pane) => {
                panes.push(pane.options);
                pane_tasks.insert(splint_id, pane.task);
            }
            Err(error) => {
                cancel_pane_tasks(pane_tasks).await;
                return Err(error);
            }
        }
    }
    Ok(PreparedManagedDojo {
        identity,
        dojo,
        panes,
        pane_tasks,
    })
}

enum TopologyManagerCommandOutcome {
    Continue,
    Stop,
    Edit(WindowTopologyCommand),
}

struct TopologyManagerState {
    tabs: WindowTabSet<ManagedDojo>,
    transient_owners: HashMap<LairId, Connection>,
    /// Set when the frontend dropped a status locally after an accepted clear,
    /// so the next poll republishes the daemon's status rather than diffing
    /// against what the frontend no longer shows.
    agent_status_republish: bool,
}

struct ManagedWindowOpen {
    identity: WindowDojoIdentity,
    dojo: splinterm_core::Dojo,
    transient_owner: Option<Connection>,
    remember: bool,
    requested_focus: Option<(SplintId, Option<u64>)>,
}

impl ManagedWindowOpen {
    fn persistent((identity, dojo): (WindowDojoIdentity, splinterm_core::Dojo)) -> Self {
        Self {
            identity,
            dojo,
            transient_owner: None,
            remember: true,
            requested_focus: None,
        }
    }
}

fn persistent_window_open(
    target: Result<(WindowDojoIdentity, splinterm_core::Dojo)>,
) -> Result<ManagedWindowOpen> {
    target.map(ManagedWindowOpen::persistent)
}

fn tab_organization_promotes(config: &AppConfig, transient_owned: bool) -> bool {
    transient_owned && config.multiplexer_lifetime.persist_on_tab_organization
}

const fn window_has_tab_capacity(tab_count: usize) -> bool {
    tab_count < splinterm::tab::MAX_WINDOW_TABS
}

fn topology_edit_target<T>(tabs: &mut WindowTabSet<T>, dojo_id: DojoId) -> Option<&mut T> {
    tabs.get_mut(dojo_id).map(|tab| &mut tab.value)
}

async fn finish_managed_window_open(
    factory: &ConnectionFactory,
    target: Result<ManagedWindowOpen>,
    state: &mut TopologyManagerState,
    config: &AppConfig,
    image_cache: &SharedImageContentCache,
    updates: &mpsc::Sender<WindowTopologyUpdate>,
    explorer_target: Option<LairExplorerActivationTarget>,
) -> TopologyManagerCommandOutcome {
    let target_id = target.as_ref().ok().map(|target| target.dojo.id);
    let result = async {
        let ManagedWindowOpen {
            identity,
            dojo,
            transient_owner,
            remember,
            requested_focus,
        } = target?;
        if state.tabs.activate(dojo.id) {
            anyhow::ensure!(
                transient_owner.is_none(),
                "new transient Lair collided with an existing Window tab"
            );
            remember_dojo(factory, dojo.id);
            send_existing_tab_activation(updates, dojo.id, requested_focus, explorer_target)
                .await?;
            return Ok(OpenTabOutcome::ActivatedExisting);
        }
        anyhow::ensure!(
            state.tabs.len() < splinterm::tab::MAX_WINDOW_TABS,
            "a Window may contain at most {} Dojo tabs",
            splinterm::tab::MAX_WINDOW_TABS
        );
        let prepared = prepare_managed_dojo(
            factory,
            config,
            image_cache,
            identity,
            dojo,
            requested_focus,
        )
        .await?;
        let dojo_id = prepared.dojo.id;
        let lair_id = prepared.identity.lair_id;
        let (acknowledged, acknowledgement) = tokio::sync::oneshot::channel();
        if updates
            .send(WindowTopologyUpdate::OpenTab {
                identity: prepared.identity.clone(),
                layout: prepared.dojo.root.clone(),
                panes: prepared.panes,
                focused: requested_focus
                    .map_or(prepared.dojo.default_focus, |(splint_id, _)| splint_id),
                acknowledged,
                explorer_target,
            })
            .await
            .is_err()
        {
            cancel_pane_tasks(prepared.pane_tasks).await;
            bail!("Wayland tab update channel closed");
        }
        match acknowledgement.await {
            Ok(Ok(())) => {}
            Ok(Err(message)) => {
                cancel_pane_tasks(prepared.pane_tasks).await;
                bail!("Wayland rejected Dojo tab: {message}");
            }
            Err(_) => {
                cancel_pane_tasks(prepared.pane_tasks).await;
                bail!("Wayland dropped Dojo tab acknowledgement");
            }
        }
        state.tabs.open_or_activate(DojoTab::new(
            lair_id,
            dojo_id,
            ManagedDojo {
                identity: prepared.identity,
                root: prepared.dojo.root,
                pending_focus: None,
                pane_tasks: prepared.pane_tasks,
            },
        ))?;
        if let Some(owner) = transient_owner {
            anyhow::ensure!(
                state.transient_owners.insert(lair_id, owner).is_none(),
                "Window already owns the transient Lair"
            );
        }
        if remember {
            remember_dojo(factory, dojo_id);
        }
        Ok(OpenTabOutcome::Opened)
    }
    .await;
    match result {
        Ok(_) => TopologyManagerCommandOutcome::Continue,
        Err(error) => {
            let _ = updates
                .send(WindowTopologyUpdate::TabFailed {
                    explorer_target,
                    dojo_id: target_id,
                    message: format!("{error:#}"),
                })
                .await;
            TopologyManagerCommandOutcome::Continue
        }
    }
}

async fn send_existing_tab_activation(
    updates: &mpsc::Sender<WindowTopologyUpdate>,
    dojo_id: DojoId,
    requested_focus: Option<(SplintId, Option<u64>)>,
    explorer_target: Option<LairExplorerActivationTarget>,
) -> Result<()> {
    let update = requested_focus.map_or(
        WindowTopologyUpdate::ActivateTab {
            dojo_id,
            explorer_target,
        },
        |(splint_id, live_incarnation)| WindowTopologyUpdate::ActivateSplint {
            dojo_id,
            splint_id,
            live_incarnation,
            explorer_target,
        },
    );
    updates
        .send(update)
        .await
        .map_err(|_| anyhow::anyhow!("Wayland tab update channel closed"))
}

async fn remove_frontend_tab(
    updates: &mpsc::Sender<WindowTopologyUpdate>,
    dojo_id: DojoId,
) -> bool {
    let (acknowledged, acknowledgement) = tokio::sync::oneshot::channel();
    if updates
        .send(WindowTopologyUpdate::RemoveTab {
            dojo_id,
            acknowledged,
        })
        .await
        .is_err()
    {
        return false;
    }
    acknowledgement.await.is_ok()
}

fn close_other_tab_targets(
    retain_dojo_id: DojoId,
    retain_present: bool,
    dojo_ids: Vec<DojoId>,
) -> Option<Vec<DojoId>> {
    if !retain_present || dojo_ids.len() > splinterm::tab::MAX_WINDOW_TABS {
        return None;
    }
    let mut unique = HashSet::new();
    Some(
        dojo_ids
            .into_iter()
            .filter(|dojo_id| *dojo_id != retain_dojo_id && unique.insert(*dojo_id))
            .collect(),
    )
}

async fn show_dojo_restore_prompt(
    connection: &mut Connection,
    dojo_id: DojoId,
    expected_splint: Option<(TopologyRevision, SplintId, Option<u64>)>,
    updates: &mpsc::Sender<WindowTopologyUpdate>,
) -> Result<()> {
    let Response::Lairs {
        lairs,
        topology_revision,
    } = connection.request(Request::ListLairs).await?
    else {
        bail!("splinterd did not return its Lair catalog");
    };
    let (lair, dojo) = lairs
        .iter()
        .find_map(|lair| {
            lair.dojos
                .iter()
                .find(|dojo| dojo.id == dojo_id)
                .map(|dojo| (lair, dojo))
        })
        .context("captured Dojo is absent")?;
    // Only the exited Splints are restored. When none is, the prompt says
    // there is nothing to restore rather than offering Restore.
    let targets = collect_scoped_lair_targets(lair, LairTargetState::Exited, Some(dojo_id))?;
    let target = LairPromptTarget {
        topology_revision,
        lair_id: lair.id,
        dojo_id: Some(dojo_id),
        name: dojo_prompt_label(lair, dojo_id).unwrap_or_else(|| dojo.name.clone()),
        retention: lair.retention,
        preview: saved_layout_preview(&lairs, lair, Some(&targets), preview_home().as_deref()),
        targets,
    };
    if let Some((expected_revision, splint_id, last_incarnation)) = expected_splint {
        anyhow::ensure!(
            topology_revision == expected_revision,
            "selected Splint restore target is stale"
        );
        anyhow::ensure!(
            target.targets.iter().any(|candidate| {
                candidate.splint_id == splint_id && Some(candidate.incarnation) == last_incarnation
            }),
            "selected Splint is no longer restorable"
        );
    }
    updates
        .send(WindowTopologyUpdate::ShowLairPrompt {
            kind: LairPromptKind::Restore,
            target,
        })
        .await
        .map_err(|_| anyhow::anyhow!("Wayland tab update channel closed"))
}

// TerminateLair requires the entire retained pane set, not only currently live leaves.
fn validate_explorer_lair_termination_capture(
    lair: &splinterm_core::Lair,
    targets: &[MutationTarget],
) -> Result<()> {
    anyhow::ensure!(
        targets.len()
            == lair
                .dojos
                .iter()
                .map(|dojo| dojo.root.splint_count())
                .sum::<usize>(),
        "captured Lair pane set changed"
    );
    anyhow::ensure!(
        targets.iter().all(|target| target.lair_id == lair.id
            && lair.dojos.iter().any(|dojo| dojo.id == target.dojo_id)),
        "captured Lair containment changed"
    );
    let mut has_live = false;
    for dojo in &lair.dojos {
        let captured = targets
            .iter()
            .filter(|target| target.dojo_id == dojo.id)
            .map(|target| (target.splint_id, target.incarnation))
            .collect::<Vec<_>>();
        has_live |= !captured_dojo_kill_targets(&dojo.root, &captured)?.is_empty();
    }
    anyhow::ensure!(has_live, "Lair has no live panes to terminate");
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "closed nested command catalog validates exact row identity before legacy routing"
)]
fn validate_explorer_context(
    target: ExplorerContextTarget,
    command: &WindowTopologyCommand,
    revision: TopologyRevision,
    lairs: &[splinterm_core::Lair],
) -> Result<()> {
    anyhow::ensure!(
        revision == target.topology_revision,
        "Explorer context target is stale"
    );
    let command_revision = match command {
        WindowTopologyCommand::RestoreLair {
            expected_topology_revision,
            ..
        }
        | WindowTopologyCommand::RestoreDojo {
            expected_topology_revision,
            ..
        } => *expected_topology_revision,
        WindowTopologyCommand::PickerNewDojo {
            topology_revision, ..
        } => *topology_revision,
        _ => revision,
    };
    anyhow::ensure!(
        command_revision == revision,
        "Explorer command revision does not match its capture"
    );
    let lair_id = match target.node {
        NavigationNodeId::Lair(id) => id,
        NavigationNodeId::Dojo { lair_id, .. } | NavigationNodeId::Splint { lair_id, .. } => {
            lair_id
        }
    };
    let lair = lairs
        .iter()
        .find(|lair| lair.id == lair_id && lair.lifetime.is_persistent())
        .context("captured persistent Lair is absent")?;
    let matches = match (target.node, command) {
        (
            NavigationNodeId::Lair(id),
            WindowTopologyCommand::RequestLairPrompt { lair_id, .. }
            | WindowTopologyCommand::RenameLair { lair_id, .. }
            | WindowTopologyCommand::SetLairRetention { lair_id, .. }
            | WindowTopologyCommand::RestoreLair { lair_id, .. }
            | WindowTopologyCommand::PickerNewDojo { lair_id, .. },
        ) => id == *lair_id,
        (NavigationNodeId::Lair(id), WindowTopologyCommand::TerminateLair { lair_id, targets }) => {
            id == *lair_id && validate_explorer_lair_termination_capture(lair, targets).is_ok()
        }
        (
            NavigationNodeId::Dojo { dojo_id, .. },
            WindowTopologyCommand::RequestDojoPrompt { dojo_id: id, .. }
            | WindowTopologyCommand::RenameDojo { dojo_id: id, .. }
            | WindowTopologyCommand::TerminateDojo { dojo_id: id, .. }
            | WindowTopologyCommand::RestoreDojo { dojo_id: id, .. }
            | WindowTopologyCommand::CloseTab { dojo_id: id },
        ) => dojo_id == *id && lair.dojos.iter().any(|dojo| dojo.id == dojo_id),
        (
            NavigationNodeId::Dojo { dojo_id, .. },
            WindowTopologyCommand::OpenDojo { target: open, .. },
        ) => {
            dojo_id == open.dojo_id
                && lair_id == open.lair_id
                && open.topology_revision == revision
                && lair.dojos.iter().any(|dojo| dojo.id == dojo_id)
        }
        (
            NavigationNodeId::Splint {
                dojo_id, splint_id, ..
            },
            WindowTopologyCommand::FocusSplint { target: focus },
        ) => {
            focus.dojo_id == dojo_id
                && focus.splint_id == splint_id
                && focus.lair_id == lair_id
                && focus.topology_revision == revision
                && focus.live_incarnation == target.live_incarnation
        }
        (
            NavigationNodeId::Splint {
                dojo_id, splint_id, ..
            },
            WindowTopologyCommand::Split {
                dojo_id: id,
                target: splint,
                ..
            }
            | WindowTopologyCommand::Close {
                dojo_id: id,
                target: splint,
            },
        ) => {
            let pane = lair
                .dojos
                .iter()
                .find(|dojo| dojo.id == dojo_id)
                .and_then(|dojo| dojo.root.find_splint(splint_id))
                .context("captured Splint is absent")?;
            dojo_id == *id
                && splint_id == *splint
                && pane.state == SplintState::Running
                && target.live_incarnation.is_some()
                && pane.last_incarnation == target.live_incarnation
        }
        _ => false,
    };
    anyhow::ensure!(
        matches,
        "Explorer command does not match its captured target"
    );
    Ok(())
}

async fn explorer_prompt(
    connection: &mut Connection,
    guard: ExplorerContextTarget,
    kind: LairPromptKind,
) -> Result<LairPromptTarget> {
    let Response::Lairs {
        lairs,
        topology_revision,
    } = connection.request(Request::ListLairs).await?
    else {
        bail!("splinterd did not return Lairs");
    };
    explorer_prompt_from_catalog(&lairs, topology_revision, guard, kind)
}

/// The live Splints of `dojo` with their incarnations, which a Dojo's
/// rename carries.
fn live_dojo_targets(lair_id: LairId, dojo: &splinterm_core::Dojo) -> Vec<MutationTarget> {
    let mut ids = Vec::new();
    layout_splint_ids(&dojo.root, &mut ids);
    ids.into_iter()
        .filter_map(|id| {
            let pane = dojo.root.find_splint(id)?;
            matches!(pane.state, SplintState::Starting | SplintState::Running).then_some(())?;
            Some(MutationTarget {
                lair_id,
                dojo_id: dojo.id,
                splint_id: id,
                incarnation: pane.last_incarnation?,
            })
        })
        .collect()
}

fn explorer_prompt_from_catalog(
    lairs: &[splinterm_core::Lair],
    topology_revision: TopologyRevision,
    guard: ExplorerContextTarget,
    kind: LairPromptKind,
) -> Result<LairPromptTarget> {
    anyhow::ensure!(
        topology_revision == guard.topology_revision,
        "Explorer prompt target is stale"
    );
    let (lair_id, dojo_id) = match guard.node {
        NavigationNodeId::Lair(id) => (id, None),
        NavigationNodeId::Dojo { lair_id, dojo_id } => (lair_id, Some(dojo_id)),
        NavigationNodeId::Splint { .. } => bail!("Splint cannot open a management prompt"),
    };
    let lair = lairs
        .iter()
        .find(|lair| lair.id == lair_id && lair.lifetime.is_persistent())
        .context("captured Lair is absent")?;
    // A rename edits the stored name; every other prompt names its target as
    // the Explorer does.
    let mut target = LairPromptTarget {
        topology_revision,
        lair_id,
        dojo_id,
        name: if kind == LairPromptKind::Rename {
            lair.name.clone()
        } else {
            lair_prompt_label(lairs, lair)
        },
        retention: lair.retention,
        preview: String::new(),
        targets: Vec::new(),
    };
    if let Some(dojo_id) = dojo_id {
        let dojo = lair
            .dojos
            .iter()
            .find(|dojo| dojo.id == dojo_id)
            .context("captured Dojo is absent")?;
        target.name = if kind == LairPromptKind::Rename {
            dojo.name.clone()
        } else {
            dojo_prompt_label(lair, dojo_id).unwrap_or_else(|| dojo.name.clone())
        };
        if kind == LairPromptKind::Rename {
            target.targets = live_dojo_targets(lair_id, dojo);
        }
        if kind == LairPromptKind::Terminate {
            let mut ids = Vec::new();
            layout_splint_ids(&dojo.root, &mut ids);
            for id in ids {
                let pane = dojo.root.find_splint(id).expect("layout leaf exists");
                target.targets.push(MutationTarget {
                    lair_id,
                    dojo_id,
                    splint_id: id,
                    incarnation: pane
                        .last_incarnation
                        .context("captured Dojo pane has no incarnation")?,
                });
            }
            anyhow::ensure!(
                !collect_scoped_lair_targets(lair, LairTargetState::Live, Some(dojo_id))?
                    .is_empty(),
                "Dojo has no live panes to terminate"
            );
        }
    }
    if kind == LairPromptKind::Restore || (kind == LairPromptKind::Terminate && dojo_id.is_none()) {
        target.targets = collect_scoped_lair_targets(
            lair,
            if kind == LairPromptKind::Restore {
                LairTargetState::Exited
            } else {
                LairTargetState::All
            },
            dojo_id,
        )?;
        anyhow::ensure!(
            !target.targets.is_empty(),
            "captured target has no applicable panes"
        );
    }
    if kind == LairPromptKind::Terminate && dojo_id.is_none() {
        validate_explorer_lair_termination_capture(lair, &target.targets)?;
    }
    target.preview = saved_layout_preview(
        lairs,
        lair,
        (kind == LairPromptKind::Restore).then_some(target.targets.as_slice()),
        preview_home().as_deref(),
    );
    Ok(target)
}

#[allow(
    clippy::too_many_lines,
    reason = "session-level topology commands share one serialized daemon and frontend reconciliation boundary"
)]
async fn handle_session_manager_command(
    factory: &ConnectionFactory,
    command: WindowTopologyCommand,
    connection: &mut Connection,
    config: &AppConfig,
    image_cache: &SharedImageContentCache,
    updates: &mpsc::Sender<WindowTopologyUpdate>,
    state: &mut TopologyManagerState,
) -> TopologyManagerCommandOutcome {
    let (guard, command) = match command {
        WindowTopologyCommand::ExplorerContext { target, command } => {
            let result = async {
                let Response::Lairs {
                    lairs,
                    topology_revision,
                } = connection.request(Request::ListLairs).await?
                else {
                    bail!("splinterd did not return Lairs");
                };
                validate_explorer_context(target, &command, topology_revision, &lairs)
            }
            .await;
            if let Err(error) = result {
                let _ = updates
                    .send(WindowTopologyUpdate::TabFailed {
                        explorer_target: match command.as_ref() {
                            WindowTopologyCommand::OpenDojo {
                                explorer_target, ..
                            } => *explorer_target,
                            WindowTopologyCommand::FocusSplint { target } => {
                                Some(LairExplorerActivationTarget::Splint(*target))
                            }
                            _ => None,
                        },
                        dojo_id: None,
                        message: format!("{error:#}"),
                    })
                    .await;
                return TopologyManagerCommandOutcome::Continue;
            }
            (Some(target), *command)
        }
        command => (None, command),
    };
    if let Some(guard) = guard {
        let kind = match &command {
            WindowTopologyCommand::RequestLairPrompt { kind, .. }
            | WindowTopologyCommand::RequestDojoPrompt { kind, .. } => Some(*kind),
            _ => None,
        };
        if let Some(kind) = kind {
            let update = match explorer_prompt(connection, guard, kind).await {
                Ok(target) => WindowTopologyUpdate::ShowExplorerPrompt {
                    guard,
                    kind,
                    target,
                },
                Err(error) => WindowTopologyUpdate::TabFailed {
                    explorer_target: None,
                    dojo_id: None,
                    message: format!("{error:#}"),
                },
            };
            return if updates.send(update).await.is_ok() {
                TopologyManagerCommandOutcome::Continue
            } else {
                TopologyManagerCommandOutcome::Stop
            };
        }
    }
    match command {
        WindowTopologyCommand::RequestSessionPicker => {
            match recent_dojo_catalog(factory, connection, &state.tabs).await {
                Ok(catalog) => {
                    if updates
                        .send(WindowTopologyUpdate::ShowSessionPicker { catalog })
                        .await
                        .is_err()
                    {
                        return TopologyManagerCommandOutcome::Stop;
                    }
                }
                Err(error) => {
                    let _ = updates
                        .send(WindowTopologyUpdate::SessionPickerFailed(format!(
                            "{error:#}"
                        )))
                        .await;
                }
            }
            TopologyManagerCommandOutcome::Continue
        }
        WindowTopologyCommand::RequestLairExplorer { focused_splint } => {
            let update =
                match lair_explorer_view(factory, connection, focused_splint, &state.tabs).await {
                    Ok(view) => WindowTopologyUpdate::ShowLairExplorer { view },
                    Err(error) => WindowTopologyUpdate::LairExplorerFailed(format!("{error:#}")),
                };
            if updates.send(update).await.is_err() {
                TopologyManagerCommandOutcome::Stop
            } else {
                TopologyManagerCommandOutcome::Continue
            }
        }
        WindowTopologyCommand::RequestSelector { kind, lair_id } => {
            match selector_catalog(factory, connection, kind, lair_id, &state.tabs).await {
                Ok(catalog) => {
                    if updates
                        .send(WindowTopologyUpdate::ShowSelector { kind, catalog })
                        .await
                        .is_err()
                    {
                        return TopologyManagerCommandOutcome::Stop;
                    }
                }
                Err(error) => {
                    let _ = updates
                        .send(WindowTopologyUpdate::SessionPickerFailed(format!(
                            "{error:#}"
                        )))
                        .await;
                }
            }
            TopologyManagerCommandOutcome::Continue
        }
        WindowTopologyCommand::OpenDojo {
            target,
            explorer_target,
        } => match target.action {
            NavigationAction::ActivateDojo => {
                let current = revision_bound_picker_dojo(connection, target, false).await;
                let valid = current.is_ok()
                    && state
                        .tabs
                        .get(target.dojo_id)
                        .is_some_and(|tab| tab.lair_id == target.lair_id)
                    && state.tabs.activate(target.dojo_id);
                if valid {
                    if updates
                        .send(WindowTopologyUpdate::ActivateTab {
                            dojo_id: target.dojo_id,
                            explorer_target,
                        })
                        .await
                        .is_err()
                    {
                        TopologyManagerCommandOutcome::Stop
                    } else {
                        TopologyManagerCommandOutcome::Continue
                    }
                } else {
                    let _ = updates
                        .send(WindowTopologyUpdate::TabFailed {
                            explorer_target,
                            dojo_id: Some(target.dojo_id),
                            message: "selected picker target is stale; refresh and try again"
                                .into(),
                        })
                        .await;
                    TopologyManagerCommandOutcome::Continue
                }
            }
            NavigationAction::AttachDojo => {
                let selected = revision_bound_picker_dojo(connection, target, true).await;
                let target_open = persistent_window_open(selected);
                finish_managed_window_open(
                    factory,
                    target_open,
                    state,
                    config,
                    image_cache,
                    updates,
                    explorer_target,
                )
                .await
            }
            NavigationAction::PreviewRestoreDojo => {
                let guard = ExplorerContextTarget {
                    topology_revision: target.topology_revision,
                    node: NavigationNodeId::Dojo {
                        lair_id: target.lair_id,
                        dojo_id: target.dojo_id,
                    },
                    live_incarnation: None,
                };
                let update = match explorer_prompt(connection, guard, LairPromptKind::Restore).await
                {
                    Ok(target) => WindowTopologyUpdate::ShowLairPrompt {
                        kind: LairPromptKind::Restore,
                        target,
                    },
                    Err(error) => WindowTopologyUpdate::TabFailed {
                        explorer_target,
                        dojo_id: Some(target.dojo_id),
                        message: format!("selected Dojo restore preview is unavailable: {error:#}"),
                    },
                };
                if updates.send(update).await.is_err() {
                    TopologyManagerCommandOutcome::Stop
                } else {
                    TopologyManagerCommandOutcome::Continue
                }
            }
            _ => {
                let _ = updates
                    .send(WindowTopologyUpdate::TabFailed {
                        explorer_target,
                        dojo_id: Some(target.dojo_id),
                        message: "selected picker action is unavailable".into(),
                    })
                    .await;
                TopologyManagerCommandOutcome::Continue
            }
        },
        WindowTopologyCommand::FocusSplint { target } => match target.capability.action {
            NavigationAction::FocusSplint => {
                let current = revision_bound_explorer_splint(connection, target, false).await;
                let valid = current.is_ok()
                    && state
                        .tabs
                        .get(target.dojo_id)
                        .is_some_and(|tab| tab.lair_id == target.lair_id)
                    && state.tabs.activate(target.dojo_id);
                if valid {
                    if updates
                        .send(WindowTopologyUpdate::ActivateSplint {
                            dojo_id: target.dojo_id,
                            splint_id: target.splint_id,
                            live_incarnation: target.live_incarnation,
                            explorer_target: Some(LairExplorerActivationTarget::Splint(target)),
                        })
                        .await
                        .is_err()
                    {
                        TopologyManagerCommandOutcome::Stop
                    } else {
                        TopologyManagerCommandOutcome::Continue
                    }
                } else {
                    let _ = updates
                        .send(WindowTopologyUpdate::TabFailed {
                            explorer_target: Some(LairExplorerActivationTarget::Splint(target)),
                            dojo_id: Some(target.dojo_id),
                            message: "selected Splint target is stale; refresh and try again"
                                .into(),
                        })
                        .await;
                    TopologyManagerCommandOutcome::Continue
                }
            }
            NavigationAction::AttachAndFocusSplint => {
                let selected = revision_bound_explorer_splint(connection, target, true).await;
                let target_open = persistent_window_open(selected).map(|mut open| {
                    open.requested_focus = Some((target.splint_id, target.live_incarnation));
                    open
                });
                finish_managed_window_open(
                    factory,
                    target_open,
                    state,
                    config,
                    image_cache,
                    updates,
                    Some(LairExplorerActivationTarget::Splint(target)),
                )
                .await
            }
            NavigationAction::PreviewRestoreSplint => {
                let result = revision_bound_explorer_splint(connection, target, false).await;
                if result.is_ok() {
                    if show_dojo_restore_prompt(
                        connection,
                        target.dojo_id,
                        Some((
                            target.topology_revision,
                            target.splint_id,
                            target.last_incarnation,
                        )),
                        updates,
                    )
                    .await
                    .is_err()
                    {
                        let _ = updates
                            .send(WindowTopologyUpdate::TabFailed {
                                explorer_target: Some(LairExplorerActivationTarget::Splint(target)),
                                dojo_id: Some(target.dojo_id),
                                message: "selected Splint restore preview is unavailable".into(),
                            })
                            .await;
                    }
                } else {
                    let _ = updates
                        .send(WindowTopologyUpdate::TabFailed {
                            explorer_target: Some(LairExplorerActivationTarget::Splint(target)),
                            dojo_id: Some(target.dojo_id),
                            message: "selected Splint target is stale; refresh and try again"
                                .into(),
                        })
                        .await;
                }
                TopologyManagerCommandOutcome::Continue
            }
            _ => {
                let _ = updates
                    .send(WindowTopologyUpdate::TabFailed {
                        explorer_target: Some(LairExplorerActivationTarget::Splint(target)),
                        dojo_id: Some(target.dojo_id),
                        message: "selected Splint action is unavailable".into(),
                    })
                    .await;
                TopologyManagerCommandOutcome::Continue
            }
        },
        command @ (WindowTopologyCommand::NewLair { .. }
        | WindowTopologyCommand::PickerNewLair { .. }) => {
            let (source_splint_id, captured_revision) = match command {
                WindowTopologyCommand::NewLair { source_splint_id } => (source_splint_id, None),
                WindowTopologyCommand::PickerNewLair {
                    topology_revision,
                    source_splint_id,
                } => (source_splint_id, Some(topology_revision)),
                _ => unreachable!("new Lair command classification changed"),
            };
            if !window_has_tab_capacity(state.tabs.len()) {
                let _ = updates
                    .send(WindowTopologyUpdate::TabFailed {
                        explorer_target: None,
                        dojo_id: None,
                        message: format!(
                            "a Window may contain at most {} Dojo tabs",
                            splinterm::tab::MAX_WINDOW_TABS
                        ),
                    })
                    .await;
                return TopologyManagerCommandOutcome::Continue;
            }
            let target = create_daily_dojo(
                factory,
                connection,
                config,
                source_splint_id,
                captured_revision,
            )
            .await;
            finish_managed_window_open(factory, target, state, config, image_cache, updates, None)
                .await
        }
        WindowTopologyCommand::MaterializePreset { target, dojos } => {
            if !factory.is_local() {
                let _ = updates
                    .send(WindowTopologyUpdate::TabFailed {
                        explorer_target: None,
                        dojo_id: None,
                        message:
                            "preset materialization is available only to the trusted local client"
                                .into(),
                    })
                    .await;
                return TopologyManagerCommandOutcome::Continue;
            }
            if dojos.is_empty()
                || state.tabs.len().saturating_add(dojos.len()) > splinterm::tab::MAX_WINDOW_TABS
            {
                let _ = updates
                    .send(WindowTopologyUpdate::TabFailed {
                        explorer_target: None,
                        dojo_id: None,
                        message: format!(
                            "a Window may contain at most {} Dojo tabs",
                            splinterm::tab::MAX_WINDOW_TABS
                        ),
                    })
                    .await;
                return TopologyManagerCommandOutcome::Continue;
            }
            let materialized = async {
                let expected = connection.topology_revision().await?;
                let Response::PresetMaterialized {
                    lair_id,
                    dojo_ids,
                    panes,
                    topology_revision,
                } = connection
                    .request(Request::MaterializePreset {
                        expected_topology_revision: expected,
                        target,
                        dojos,
                        directory_identities: Vec::new(),
                    })
                    .await?
                else {
                    bail!("splinterd did not return preset materialization metadata");
                };
                anyhow::ensure!(
                    dojo_ids.len() <= splinterm::tab::MAX_WINDOW_TABS,
                    "preset response exceeds Window tab capacity"
                );
                let Response::Lairs {
                    lairs,
                    topology_revision: inspected_revision,
                } = connection.request(Request::ListLairs).await?
                else {
                    bail!("splinterd did not return sessions after preset materialization");
                };
                anyhow::ensure!(
                    inspected_revision == topology_revision,
                    "preset topology revision drifted before Window reconciliation"
                );
                materialized_dojo_targets(&lairs, topology_revision, lair_id, &dojo_ids, &panes)
            }
            .await;
            let targets = match materialized {
                Ok(targets) => targets,
                Err(error) => {
                    let _ = updates
                        .send(WindowTopologyUpdate::TabFailed {
                            explorer_target: None,
                            dojo_id: None,
                            message: format!("{error:#}"),
                        })
                        .await;
                    return TopologyManagerCommandOutcome::Continue;
                }
            };
            for target in targets {
                if matches!(
                    finish_managed_window_open(
                        factory,
                        Ok(ManagedWindowOpen::persistent(target)),
                        state,
                        config,
                        image_cache,
                        updates,
                        None,
                    )
                    .await,
                    TopologyManagerCommandOutcome::Stop
                ) {
                    return TopologyManagerCommandOutcome::Stop;
                }
            }
            TopologyManagerCommandOutcome::Continue
        }
        command @ (WindowTopologyCommand::NewDojo { .. }
        | WindowTopologyCommand::PickerNewDojo { .. }) => {
            let (lair_id, source_splint_id, captured_revision) = match command {
                WindowTopologyCommand::NewDojo {
                    lair_id,
                    source_splint_id,
                } => (lair_id, source_splint_id, None),
                WindowTopologyCommand::PickerNewDojo {
                    topology_revision,
                    lair_id,
                    source_splint_id,
                } => (lair_id, source_splint_id, Some(topology_revision)),
                _ => unreachable!("new Dojo command classification changed"),
            };
            if !window_has_tab_capacity(state.tabs.len()) {
                let _ = updates
                    .send(WindowTopologyUpdate::TabFailed {
                        explorer_target: None,
                        dojo_id: None,
                        message: format!(
                            "a Window may contain at most {} Dojo tabs",
                            splinterm::tab::MAX_WINDOW_TABS
                        ),
                    })
                    .await;
                return TopologyManagerCommandOutcome::Continue;
            }
            let transient_owned = state.transient_owners.contains_key(&lair_id);
            let created = if transient_owned {
                let owner = state
                    .transient_owners
                    .get_mut(&lair_id)
                    .expect("checked transient owner remains present");
                create_dojo_in_lair(
                    factory,
                    connection,
                    Some(owner),
                    config,
                    lair_id,
                    source_splint_id,
                    captured_revision,
                )
                .await
            } else {
                create_dojo_in_lair(
                    factory,
                    connection,
                    None,
                    config,
                    lair_id,
                    source_splint_id,
                    captured_revision,
                )
                .await
            };
            let target = created.map(|(identity, dojo, promoted)| {
                if promoted {
                    state.transient_owners.remove(&lair_id);
                }
                ManagedWindowOpen {
                    identity,
                    dojo,
                    transient_owner: None,
                    remember: !transient_owned || promoted,
                    requested_focus: None,
                }
            });
            finish_managed_window_open(factory, target, state, config, image_cache, updates, None)
                .await
        }
        WindowTopologyCommand::NavigateLair {
            current_lair_id,
            direction,
        } => {
            let target = persistent_window_open(
                navigate_lair(factory, connection, &state.tabs, current_lair_id, direction).await,
            );
            finish_managed_window_open(factory, target, state, config, image_cache, updates, None)
                .await
        }
        WindowTopologyCommand::RequestLairPrompt {
            lair_id,
            kind,
            expected_retention,
        } => {
            match lair_prompt_target(connection, lair_id, kind, expected_retention).await {
                Ok(target) => {
                    if updates
                        .send(WindowTopologyUpdate::ShowLairPrompt { kind, target })
                        .await
                        .is_err()
                    {
                        return TopologyManagerCommandOutcome::Stop;
                    }
                }
                Err(error) => {
                    let _ = updates
                        .send(WindowTopologyUpdate::TabFailed {
                            explorer_target: None,
                            dojo_id: None,
                            message: format!("{error:#}"),
                        })
                        .await;
                }
            }
            TopologyManagerCommandOutcome::Continue
        }
        WindowTopologyCommand::RequestDojoRestorePrompt { dojo_id } => {
            if let Err(error) = show_dojo_restore_prompt(connection, dojo_id, None, updates).await {
                let _ = updates
                    .send(WindowTopologyUpdate::TabFailed {
                        explorer_target: None,
                        dojo_id: Some(dojo_id),
                        message: format!("{error:#}"),
                    })
                    .await;
            }
            TopologyManagerCommandOutcome::Continue
        }
        WindowTopologyCommand::RenameLair { lair_id, name } => {
            let result = async {
                anyhow::ensure!(
                    guard.is_some() || state.tabs.iter().any(|tab| tab.lair_id == lair_id),
                    "rename targeted a detached Lair"
                );
                let expected_topology_revision = match guard {
                    Some(guard) => guard.topology_revision,
                    None => connection.topology_revision().await?,
                };
                let response = connection
                    .request(Request::RenameLair {
                        expected_topology_revision,
                        lair_id,
                        name,
                    })
                    .await?;
                anyhow::ensure!(
                    matches!(response, Response::TopologyCommitted { .. }),
                    "splinterd did not acknowledge Lair rename"
                );
                Ok::<(), anyhow::Error>(())
            }
            .await;
            if let Err(error) = result {
                let _ = updates
                    .send(WindowTopologyUpdate::TabFailed {
                        explorer_target: None,
                        dojo_id: None,
                        message: format!("{error:#}"),
                    })
                    .await;
            }
            TopologyManagerCommandOutcome::Continue
        }
        WindowTopologyCommand::RestoreLair {
            expected_topology_revision,
            lair_id,
        } => {
            let result = async {
                anyhow::ensure!(
                    guard.is_some() || state.tabs.iter().any(|tab| tab.lair_id == lair_id),
                    "restore targeted a detached Lair"
                );
                let response = connection
                    .request(Request::RestoreLair {
                        expected_topology_revision,
                        lair_id,
                    })
                    .await?;
                anyhow::ensure!(
                    matches!(response, Response::RestoreCompleted { .. }),
                    "splinterd did not acknowledge Lair restore"
                );
                Ok::<(), anyhow::Error>(())
            }
            .await;
            match result {
                Ok(()) => {
                    let _ = updates
                        .send(WindowTopologyUpdate::LairExplorerInvalidated)
                        .await;
                }
                Err(error) => {
                    let _ = updates
                        .send(WindowTopologyUpdate::TabFailed {
                            explorer_target: None,
                            dojo_id: None,
                            message: format!("{error:#}"),
                        })
                        .await;
                }
            }
            TopologyManagerCommandOutcome::Continue
        }
        WindowTopologyCommand::RestoreDojo {
            expected_topology_revision,
            dojo_id,
        } => {
            let result = async {
                let response = connection
                    .request(Request::RestoreDojo {
                        expected_topology_revision,
                        dojo_id,
                    })
                    .await?;
                anyhow::ensure!(
                    matches!(response, Response::RestoreCompleted { .. }),
                    "splinterd did not acknowledge Dojo restore"
                );
                Ok::<(), anyhow::Error>(())
            }
            .await;
            match result {
                Ok(()) => {
                    let _ = updates
                        .send(WindowTopologyUpdate::LairExplorerInvalidated)
                        .await;
                }
                Err(error) => {
                    let _ = updates
                        .send(WindowTopologyUpdate::TabFailed {
                            explorer_target: None,
                            dojo_id: Some(dojo_id),
                            message: format!("{error:#}"),
                        })
                        .await;
                }
            }
            TopologyManagerCommandOutcome::Continue
        }
        WindowTopologyCommand::SetLairRetention {
            lair_id,
            expected_retention,
            retention,
        } => {
            let result = async {
                anyhow::ensure!(
                    guard.is_some() || state.tabs.iter().any(|tab| tab.lair_id == lair_id),
                    "retention change targeted a detached Lair"
                );
                let expected_topology_revision = if let Some(guard) = guard {
                    guard.topology_revision
                } else {
                    let Response::Lairs {
                        lairs,
                        topology_revision,
                    } = connection.request(Request::ListLairs).await?
                    else {
                        bail!("splinterd did not return its Lair catalog");
                    };
                    validate_lair_retention_target(&lairs, lair_id, expected_retention)?;
                    topology_revision
                };
                let response = connection
                    .request(Request::SetLairRetention {
                        expected_topology_revision,
                        lair_id,
                        retention,
                    })
                    .await?;
                let Response::TopologyCommitted {
                    topology_revision, ..
                } = response
                else {
                    bail!("splinterd did not acknowledge Lair retention change");
                };
                for tab in state.tabs.iter_mut().filter(|tab| tab.lair_id == lair_id) {
                    tab.value.identity.topology_revision = topology_revision;
                    tab.value.identity.lair_retention = retention;
                    updates
                        .send(WindowTopologyUpdate::UpdateIdentity(
                            tab.value.identity.clone(),
                        ))
                        .await
                        .map_err(|_| anyhow::anyhow!("Wayland tab update channel closed"))?;
                }
                Ok::<(), anyhow::Error>(())
            }
            .await;
            if let Err(error) = result {
                let _ = updates
                    .send(WindowTopologyUpdate::TabFailed {
                        explorer_target: None,
                        dojo_id: None,
                        message: format!("{error:#}"),
                    })
                    .await;
            }
            TopologyManagerCommandOutcome::Continue
        }
        WindowTopologyCommand::TerminateLair { lair_id, targets } => {
            let result = async {
                let expected_topology_revision = match guard {
                    Some(guard) => guard.topology_revision,
                    None => connection.topology_revision().await?,
                };
                let response = connection
                    .request(Request::TerminateLair {
                        expected_topology_revision,
                        lair_id,
                        targets,
                    })
                    .await?;
                anyhow::ensure!(
                    matches!(response, Response::TopologyCommitted { .. }),
                    "splinterd did not acknowledge Lair termination"
                );
                Ok::<(), anyhow::Error>(())
            }
            .await;
            if let Err(error) = result {
                let _ = updates
                    .send(WindowTopologyUpdate::TabFailed {
                        explorer_target: None,
                        dojo_id: None,
                        message: format!("{error:#}"),
                    })
                    .await;
                return TopologyManagerCommandOutcome::Continue;
            }
            let dojo_ids = state
                .tabs
                .iter()
                .filter_map(|tab| (tab.lair_id == lair_id).then_some(tab.dojo_id))
                .collect::<Vec<_>>();
            for dojo_id in dojo_ids {
                if let Some(removed) = state.tabs.close(dojo_id) {
                    let acknowledged = remove_frontend_tab(updates, dojo_id).await;
                    retire_pane_tasks(removed.value.pane_tasks);
                    if !acknowledged {
                        return TopologyManagerCommandOutcome::Stop;
                    }
                }
            }
            if state.tabs.is_empty() {
                let _ = updates.send(WindowTopologyUpdate::Closed).await;
                return TopologyManagerCommandOutcome::Stop;
            }
            TopologyManagerCommandOutcome::Continue
        }
        WindowTopologyCommand::RenameDojo { dojo_id, name } => {
            let result = async {
                let lair_id = match guard.map(|guard| guard.node) {
                    Some(NavigationNodeId::Dojo { lair_id, .. }) => lair_id,
                    _ => state
                        .tabs
                        .get(dojo_id)
                        .map(|tab| tab.lair_id)
                        .context("rename targeted a closed Dojo tab")?,
                };
                let transient_owned = state.transient_owners.contains_key(&lair_id);
                let promote_transient_lair = tab_organization_promotes(config, transient_owned);
                let request = Request::RenameDojo {
                    expected_topology_revision: match guard {
                        Some(guard) => guard.topology_revision,
                        None => connection.topology_revision().await?,
                    },
                    dojo_id,
                    name,
                    promote_transient_lair,
                };
                let response = if transient_owned {
                    state
                        .transient_owners
                        .get_mut(&lair_id)
                        .expect("checked transient owner remains present")
                        .request(request)
                        .await?
                } else {
                    connection.request(request).await?
                };
                anyhow::ensure!(
                    matches!(response, Response::TopologyCommitted { .. }),
                    "splinterd did not acknowledge Dojo rename"
                );
                if promote_transient_lair {
                    state.transient_owners.remove(&lair_id);
                    remember_dojo(factory, dojo_id);
                }
                Ok::<(), anyhow::Error>(())
            }
            .await;
            if let Err(error) = result {
                let _ = updates
                    .send(WindowTopologyUpdate::TabFailed {
                        explorer_target: None,
                        dojo_id: Some(dojo_id),
                        message: format!("{error:#}"),
                    })
                    .await;
            }
            TopologyManagerCommandOutcome::Continue
        }
        WindowTopologyCommand::TerminateDojo { dojo_id, splints } => {
            let result = async {
                anyhow::ensure!(
                    guard.is_some() || state.tabs.get(dojo_id).is_some(),
                    "termination targeted a closed Dojo tab"
                );
                terminate_dojo(connection, dojo_id, &splints).await
            }
            .await;
            match result {
                Ok(()) => {
                    if let Some(removed) = state.tabs.close(dojo_id) {
                        let acknowledged = remove_frontend_tab(updates, dojo_id).await;
                        retire_pane_tasks(removed.value.pane_tasks);
                        if !acknowledged {
                            return TopologyManagerCommandOutcome::Stop;
                        }
                        if state.tabs.is_empty() {
                            let _ = updates.send(WindowTopologyUpdate::Closed).await;
                            return TopologyManagerCommandOutcome::Stop;
                        }
                    }
                }
                Err(error) => {
                    let _ = updates
                        .send(WindowTopologyUpdate::TabFailed {
                            explorer_target: None,
                            dojo_id: Some(dojo_id),
                            message: format!("{error:#}"),
                        })
                        .await;
                }
            }
            TopologyManagerCommandOutcome::Continue
        }
        WindowTopologyCommand::ActivateTab { dojo_id } => {
            if state.tabs.activate(dojo_id)
                && updates
                    .send(WindowTopologyUpdate::ActivateTab {
                        dojo_id,
                        explorer_target: None,
                    })
                    .await
                    .is_err()
            {
                return TopologyManagerCommandOutcome::Stop;
            }
            TopologyManagerCommandOutcome::Continue
        }
        WindowTopologyCommand::CloseTab { dojo_id } => {
            if let Some(removed) = state.tabs.close(dojo_id) {
                let acknowledged = remove_frontend_tab(updates, dojo_id).await;
                release_managed_tab(connection, config, state, removed).await;
                if !acknowledged {
                    return TopologyManagerCommandOutcome::Stop;
                }
                if state.tabs.is_empty() {
                    let _ = updates.send(WindowTopologyUpdate::Closed).await;
                    return TopologyManagerCommandOutcome::Stop;
                }
            }
            TopologyManagerCommandOutcome::Continue
        }
        WindowTopologyCommand::CloseTabs {
            retain_dojo_id,
            dojo_ids,
        } => {
            let Some(dojo_ids) = close_other_tab_targets(
                retain_dojo_id,
                state.tabs.get(retain_dojo_id).is_some(),
                dojo_ids,
            ) else {
                return TopologyManagerCommandOutcome::Continue;
            };
            for dojo_id in dojo_ids {
                if let Some(removed) = state.tabs.close(dojo_id) {
                    let acknowledged = remove_frontend_tab(updates, dojo_id).await;
                    release_managed_tab(connection, config, state, removed).await;
                    if !acknowledged {
                        return TopologyManagerCommandOutcome::Stop;
                    }
                    if state.tabs.is_empty() {
                        let _ = updates.send(WindowTopologyUpdate::Closed).await;
                        return TopologyManagerCommandOutcome::Stop;
                    }
                }
            }
            TopologyManagerCommandOutcome::Continue
        }
        WindowTopologyCommand::ClearAgentStatus {
            splint_id,
            incarnation,
        } => {
            let update =
                match clear_agent_status(connection, &state.tabs, splint_id, incarnation).await {
                    Ok(()) => {
                        state.agent_status_republish = true;
                        WindowTopologyUpdate::AgentStatusCleared {
                            splint_id,
                            incarnation,
                        }
                    }
                    Err(error) => WindowTopologyUpdate::TabFailed {
                        explorer_target: None,
                        dojo_id: None,
                        message: format!("{error:#}"),
                    },
                };
            if updates.send(update).await.is_err() {
                TopologyManagerCommandOutcome::Stop
            } else {
                TopologyManagerCommandOutcome::Continue
            }
        }
        WindowTopologyCommand::RequestDojoPrompt { .. }
        | WindowTopologyCommand::ExplorerContext { .. } => TopologyManagerCommandOutcome::Continue,
        command => TopologyManagerCommandOutcome::Edit(match guard {
            Some(guard) => guard.command(command),
            None => command,
        }),
    }
}

async fn inspect_managed_topology(
    connection: &mut Connection,
) -> Result<splinterm_protocol::TopologySnapshot> {
    let Response::Topology { snapshot } = connection.request(Request::InspectTopology).await?
    else {
        bail!("splinterd did not return topology for Window reconciliation");
    };
    snapshot
        .validate()
        .map_err(|error| anyhow::anyhow!(error.message))?;
    Ok(snapshot)
}

async fn reconcile_managed_topology(
    factory: &ConnectionFactory,
    config: &AppConfig,
    image_cache: &SharedImageContentCache,
    state: &mut TopologyManagerState,
    snapshot: &splinterm_protocol::TopologySnapshot,
    updates: &mpsc::Sender<WindowTopologyUpdate>,
) -> Result<bool> {
    let authoritative = snapshot
        .topology
        .lairs()
        .flat_map(|lair| {
            lair.dojos.iter().map(move |dojo| {
                (
                    dojo.id,
                    (
                        window_dojo_identity(snapshot.revision, lair, dojo),
                        dojo.root.clone(),
                    ),
                )
            })
        })
        .collect::<HashMap<_, _>>();
    let dojo_ids = state.tabs.iter().map(|tab| tab.dojo_id).collect::<Vec<_>>();
    for dojo_id in dojo_ids {
        let Some((identity, root)) = authoritative.get(&dojo_id).cloned() else {
            if let Some(removed) = state.tabs.close(dojo_id) {
                let acknowledged = remove_frontend_tab(updates, dojo_id).await;
                retire_pane_tasks(removed.value.pane_tasks);
                if !acknowledged {
                    return Ok(false);
                }
            }
            continue;
        };
        let managed = &mut state
            .tabs
            .get_mut(dojo_id)
            .context("managed Dojo disappeared during reconciliation")?
            .value;
        if managed.identity != identity {
            if updates
                .send(WindowTopologyUpdate::UpdateIdentity(identity.clone()))
                .await
                .is_err()
            {
                return Ok(false);
            }
            managed.identity = identity;
        }
        let (added, _) = topology_identity_diff(&managed.root, &root);
        let (focused, placeholder, consumed) =
            pending_focus_for_observation(managed.pending_focus, snapshot.revision, &added);
        match reconcile_window_topology(
            factory,
            config,
            image_cache,
            snapshot.revision,
            dojo_id,
            &mut managed.root,
            root,
            focused,
            placeholder,
            updates,
            &mut managed.pane_tasks,
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => return Ok(false),
            Err(error) => {
                let _ = updates
                    .send(WindowTopologyUpdate::TabFailed {
                        explorer_target: None,
                        dojo_id: Some(dojo_id),
                        message: format!("{error:#}"),
                    })
                    .await;
            }
        }
        if consumed {
            managed.pending_focus = None;
        }
    }
    if state.tabs.is_empty() {
        let _ = updates.send(WindowTopologyUpdate::Closed).await;
        return Ok(false);
    }
    Ok(true)
}

/// Runtime transitions need not advance the structural revision, and detached
/// Dojos produce no managed-tab updates. Observe both, without polling `ListLairs`
/// or waking the frontend on unchanged snapshots (runtime order is immaterial).
/// Agent status is compared as presented: its report age and sequence advance
/// on every poll or heartbeat without changing what the Explorer shows.
#[derive(Default)]
struct ExplorerObservation {
    revision: Option<TopologyRevision>,
    runtimes: HashMap<
        SplintId,
        (
            splinterm_protocol::SplintRuntimeSummary,
            Option<NavigationAgentStatus>,
        ),
    >,
}

impl ExplorerObservation {
    fn observe(&mut self, snapshot: &splinterm_protocol::TopologySnapshot) -> bool {
        let runtimes = snapshot
            .runtimes
            .iter()
            .map(|runtime| {
                let status = NavigationAgentStatus::from_runtime(runtime);
                let mut runtime = runtime.clone();
                runtime.agent_status = None;
                (runtime.splint_id, (runtime, status))
            })
            .collect::<HashMap<_, _>>();
        let changed = self.revision != Some(snapshot.revision) || self.runtimes != runtimes;
        self.revision = Some(snapshot.revision);
        self.runtimes = runtimes;
        changed
    }
}

/// Publishes attached-Dojo agent status for tab badges only when what the
/// badges and palette show changed. The frontend starts with no status, so an
/// empty first observation sends nothing.
#[derive(Default)]
struct AgentStatusObservation {
    published: WindowAgentStatus,
}

impl AgentStatusObservation {
    fn observe(
        &mut self,
        snapshot: &splinterm_protocol::TopologySnapshot,
        tabs: &WindowTabSet<ManagedDojo>,
    ) -> Option<WindowAgentStatus> {
        let attached = tabs.iter().map(|tab| tab.dojo_id).collect::<Vec<_>>();
        let current = WindowAgentStatus::from_snapshot(snapshot, &attached);
        if self.published == current {
            return None;
        }
        self.published.clone_from(&current);
        Some(current)
    }
}

/// The trusted UI may only clear a status, and only for a Splint in one of
/// this Window's Dojos, through the person's explicit palette action.
async fn clear_agent_status(
    connection: &mut Connection,
    tabs: &WindowTabSet<ManagedDojo>,
    splint_id: SplintId,
    incarnation: u64,
) -> Result<()> {
    anyhow::ensure!(
        tabs.iter()
            .any(|tab| tab.value.root.find_splint(splint_id).is_some()),
        "agent status clear targeted a Splint outside this Window"
    );
    let response = connection
        .request(Request::ReportAgentStatus {
            splint_id,
            incarnation,
            status: None,
        })
        .await?;
    anyhow::ensure!(
        matches!(response, Response::AgentStatusAccepted { .. }),
        "splinterd did not acknowledge the agent status clear"
    );
    Ok(())
}

enum TopologyManagerWake {
    Command(WindowTopologyCommand),
    Poll,
    Shutdown,
}

/// Takes the settings the Window last published from Reload configuration.
/// The manager calls this before every command, poll, and the exit-time
/// close, so each decision uses the latest reload; a decision already under
/// way keeps the value it started with.
fn refresh_reloaded_settings(
    config: &mut AppConfig,
    settings: &mut watch::Receiver<TopologyReloadSettings>,
) {
    // A closed sender still holds the last published value.
    if settings.has_changed().unwrap_or(true) {
        settings.borrow_and_update().apply_to(config);
    }
}

/// Drains commands left after the Window dropped its sender, reporting
/// whether a close notice was among them. Nothing else can apply now.
fn close_notice_queued(commands: &mut mpsc::Receiver<WindowTopologyCommand>) -> bool {
    let mut queued = false;
    while let Ok(command) = commands.try_recv() {
        queued |= matches!(command, WindowTopologyCommand::CloseWindow);
    }
    queued
}

async fn next_topology_manager_wake(
    commands: &mut mpsc::Receiver<WindowTopologyCommand>,
    poll: &mut tokio::time::Interval,
    poll_priority: &mut bool,
) -> TopologyManagerWake {
    if commands.is_closed() {
        return TopologyManagerWake::Shutdown;
    }
    let wake = if *poll_priority {
        tokio::select! {
            biased;
            _ = poll.tick() => TopologyManagerWake::Poll,
            command = commands.recv() => command.map_or(
                TopologyManagerWake::Shutdown,
                TopologyManagerWake::Command,
            ),
        }
    } else {
        tokio::select! {
            biased;
            command = commands.recv() => command.map_or(
                TopologyManagerWake::Shutdown,
                TopologyManagerWake::Command,
            ),
            _ = poll.tick() => TopologyManagerWake::Poll,
        }
    };
    if commands.is_closed() {
        return TopologyManagerWake::Shutdown;
    }
    *poll_priority = matches!(wake, TopologyManagerWake::Command(_));
    wake
}

/// How a reconnection the manager ran ended, with what it means for its loop.
enum ReconnectOutcome {
    Resumed {
        connection: Box<Connection>,
        daemon: DaemonIdentity,
        /// The reconnection epoch the resnapshot answered.
        epoch: u64,
        /// The Window's close notice was queued behind the abandoned commands.
        close_window: bool,
    },
    Lost,
    WindowGone,
}

/// What the frontend hears about a topology command the manager did not run
/// because its daemon connection was lost during a handoff.
const ABANDONED_FOR_RECONNECT: &str = "not run: splinterd was upgrading; try again";

/// Plan 0061 M3.12b: what the frontend hears about a command it sent before
/// the Window's daemon connection was lost, which does not run. Running it
/// against the serving daemon after reconnection would replay it; instead an
/// optimistic split placeholder is rolled back and any picker or Explorer
/// activation waiting for an answer is released.
fn abandoned_command_updates(
    command: WindowTopologyCommand,
    state: &TopologyManagerState,
) -> Vec<WindowTopologyUpdate> {
    let command = match command {
        WindowTopologyCommand::ExplorerContext { command, .. } => *command,
        command => command,
    };
    if let Some(updates) = abandoned_access_updates(&command, "splinterd was upgrading; try again")
    {
        return updates;
    }
    let mut updates = Vec::with_capacity(2);
    if let WindowTopologyCommand::Split {
        dojo_id,
        target,
        pending: Some(pending),
        ..
    } = &command
        && let Some(tab) = state.tabs.get(*dojo_id)
    {
        updates.push(WindowTopologyUpdate::Apply {
            topology_revision: tab.value.identity.topology_revision,
            dojo_id: *dojo_id,
            layout: tab.value.root.clone(),
            added: Vec::new(),
            removed: vec![*pending],
            focused: Some(*target),
        });
    }
    let explorer_target = match &command {
        WindowTopologyCommand::OpenDojo {
            explorer_target, ..
        } => *explorer_target,
        _ => None,
    };
    updates.push(WindowTopologyUpdate::TabFailed {
        explorer_target,
        dojo_id: None,
        message: ABANDONED_FOR_RECONNECT.to_owned(),
    });
    updates
}

/// Settles `command` and every command queued now without the daemon:
/// returns what the frontend hears, and whether the Window's close notice
/// was among them. A plain tab switch needs no daemon and is applied (the
/// frontend already switched); every other command is abandoned.
fn abandon_queued_commands(
    command: Option<WindowTopologyCommand>,
    commands: &mut mpsc::Receiver<WindowTopologyCommand>,
    state: &mut TopologyManagerState,
) -> (Vec<WindowTopologyUpdate>, bool) {
    let mut close_window = false;
    let mut pending = Vec::new();
    for command in command
        .into_iter()
        .chain(std::iter::from_fn(|| commands.try_recv().ok()))
    {
        match command {
            WindowTopologyCommand::CloseWindow => close_window = true,
            WindowTopologyCommand::ActivateTab { dojo_id } => {
                pending.extend(tab_switch_update(state, dojo_id));
            }
            command => pending.extend(abandoned_command_updates(command, state)),
        }
    }
    (pending, close_window)
}

/// Applies a plain tab switch to the manager's tab order; the update the
/// frontend hears, if the tab changed.
fn tab_switch_update(
    state: &mut TopologyManagerState,
    dojo_id: DojoId,
) -> Option<WindowTopologyUpdate> {
    state
        .tabs
        .activate(dojo_id)
        .then_some(WindowTopologyUpdate::ActivateTab {
            dojo_id,
            explorer_target: None,
        })
}

async fn send_topology_updates(
    pending: Vec<WindowTopologyUpdate>,
    updates: &mpsc::Sender<WindowTopologyUpdate>,
) {
    for update in pending {
        let _ = updates.send(update).await;
    }
}

/// Reconnects the Window, abandoning `command` and every command queued
/// before or during the reconnection: all were sent before the connection
/// was lost, and none is replayed. While disconnected the frontend sends
/// only tab switches, which are applied, and the close notice, which is
/// kept. Placeholders are rolled back before the resnapshot lands.
#[allow(
    clippy::too_many_arguments,
    reason = "the manager's loop state is lent to one reconnection"
)]
async fn reconnect_and_abandon(
    factory: &ConnectionFactory,
    config: &AppConfig,
    image_cache: &SharedImageContentCache,
    state: &mut TopologyManagerState,
    updates: &mpsc::Sender<WindowTopologyUpdate>,
    commands: &mut mpsc::Receiver<WindowTopologyCommand>,
    command: Option<WindowTopologyCommand>,
    daemon: DaemonIdentity,
) -> ReconnectOutcome {
    let (pending, closing_before) = abandon_queued_commands(command, commands, state);
    send_topology_updates(pending, updates).await;
    // Commands that arrive while it reconnects are settled inside the
    // attempt, before the resume.
    let outcome = reconnect_managed_window(
        factory,
        config,
        image_cache,
        state,
        updates,
        commands,
        daemon,
    )
    .await;
    match outcome {
        ManagedReconnect::Resumed {
            connection,
            daemon,
            epoch,
            close_window,
        } => ReconnectOutcome::Resumed {
            connection,
            daemon,
            epoch,
            close_window: closing_before || close_window,
        },
        ManagedReconnect::Lost => ReconnectOutcome::Lost,
        ManagedReconnect::WindowGone => ReconnectOutcome::WindowGone,
    }
}

/// The tabs a Window relaunched across a daemon upgrade reopens after its
/// first one, and the tab it activates (Plan 0061 M3.15).
pub(in crate::app) struct TopologyRestore {
    pub(in crate::app) tabs: Vec<(DojoId, Option<SplintId>)>,
    pub(in crate::app) active: DojoId,
}

/// Reopens a relaunched Window's other tabs, in their recorded order and each
/// with its recorded focus, from the daemon's topology as it is now, then
/// activates the recorded tab. Every tab is a fresh attachment: nothing from
/// before the relaunch is replayed. A Dojo that ended meanwhile, or that fails
/// to open, is skipped with the frontend's ordinary tab failure.
async fn restore_window_tabs(
    factory: &ConnectionFactory,
    connection: &mut Connection,
    config: &AppConfig,
    image_cache: &SharedImageContentCache,
    updates: &mpsc::Sender<WindowTopologyUpdate>,
    state: &mut TopologyManagerState,
    restore: TopologyRestore,
) {
    let Ok(Response::Lairs {
        lairs,
        topology_revision,
    }) = connection.request(Request::ListLairs).await
    else {
        eprintln!("splinterm: cannot read the topology to restore this Window's other tabs");
        return;
    };
    for (dojo_id, focus) in restore.tabs {
        let Some((lair, dojo)) = lairs.iter().find_map(|lair| {
            lair.dojos
                .iter()
                .find(|dojo| dojo.id == dojo_id)
                .map(|dojo| (lair, dojo))
        }) else {
            continue;
        };
        let requested_focus = focus
            .filter(|focus| dojo.root.find_splint(*focus).is_some())
            .map(|focus| (focus, None));
        let target = Ok(ManagedWindowOpen {
            identity: window_dojo_identity(topology_revision, lair, dojo),
            dojo: dojo.clone(),
            transient_owner: None,
            // The Window shows these again; the recent list is unchanged.
            remember: false,
            requested_focus,
        });
        finish_managed_window_open(factory, target, state, config, image_cache, updates, None)
            .await;
    }
    if state.tabs.activate(restore.active) {
        let _ = updates
            .send(WindowTopologyUpdate::ActivateTab {
                dojo_id: restore.active,
                explorer_target: None,
            })
            .await;
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "endpoint-bound poll reconciliation, command targeting, and task shutdown share one loop"
)]
pub(in crate::app) async fn run_topology_manager(
    factory: ConnectionFactory,
    mut config: AppConfig,
    mut settings: watch::Receiver<TopologyReloadSettings>,
    image_cache: SharedImageContentCache,
    initial_identity: WindowDojoIdentity,
    root: LayoutNode,
    mut commands: mpsc::Receiver<WindowTopologyCommand>,
    updates: mpsc::Sender<WindowTopologyUpdate>,
    pane_tasks: HashMap<SplintId, PaneTask>,
    initial_transient_owner: Option<Connection>,
    restore: Option<TopologyRestore>,
) -> Result<()> {
    let mut connection = factory.connect().await?;
    let mut poll = tokio::time::interval(std::time::Duration::from_millis(250));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut poll_priority = false;
    let mut explorer_observation = ExplorerObservation::default();
    let mut agent_status_observation = AgentStatusObservation::default();
    let mut access = AccessMonitor::default();
    let initial_lair_id = initial_identity.lair_id;
    let initial_dojo_id = initial_identity.dojo_id;
    let mut transient_owners = HashMap::new();
    if let Some(owner) = initial_transient_owner {
        transient_owners.insert(initial_lair_id, owner);
    }
    let mut state = TopologyManagerState {
        tabs: WindowTabSet::new(DojoTab::new(
            initial_lair_id,
            initial_dojo_id,
            ManagedDojo {
                identity: initial_identity,
                root,
                pending_focus: None,
                pane_tasks,
            },
        )),
        transient_owners,
        agent_status_republish: false,
    };
    if let Some(restore) = restore {
        Box::pin(restore_window_tabs(
            &factory,
            &mut connection,
            &config,
            &image_cache,
            &updates,
            &mut state,
            restore,
        ))
        .await;
    }
    let mut window_closing = false;
    // Plan 0061 M3.12b: the daemon this manager is connected to, the
    // reconnection it last answered, and whether the Window gave up.
    let mut daemon = DaemonIdentity::of(&connection);
    let mut answered_reconnect = None;
    let mut lost = false;
    loop {
        let command =
            match next_topology_manager_wake(&mut commands, &mut poll, &mut poll_priority).await {
                TopologyManagerWake::Command(WindowTopologyCommand::CloseWindow) => {
                    window_closing = true;
                    break;
                }
                TopologyManagerWake::Command(command) => Some(command),
                TopologyManagerWake::Poll => None,
                TopologyManagerWake::Shutdown => {
                    // The Window exits right after sending its close notice.
                    window_closing |= close_notice_queued(&mut commands);
                    break;
                }
            };
        refresh_reloaded_settings(&mut config, &mut settings);
        // The Window takes part in a handoff and waits for its outcome
        // (M3.14): nothing reaches the daemon, whose connections are about to
        // be replaced. Commands are settled as during a reconnection: tab
        // switches apply, the rest is abandoned, never replayed. An adoption
        // or rollback relaunches the Window; a cancelled attempt ends the
        // wait and the loop resumes, reconnecting first if a connection was
        // lost meanwhile.
        if factory.handoff_status().continuing() {
            let (pending, close_window) =
                abandon_queued_commands(command, &mut commands, &mut state);
            send_topology_updates(pending, &updates).await;
            if close_window {
                window_closing = true;
                break;
            }
            continue;
        }
        // A pane lost its daemon connection during a handoff: the whole
        // Window reconnects and resnapshots before anything else runs.
        let pending_reconnect = factory
            .handoff_status()
            .reconnect_pending()
            .filter(|epoch| answered_reconnect != Some(*epoch));
        if pending_reconnect.is_some() {
            let outcome = reconnect_and_abandon(
                &factory,
                &config,
                &image_cache,
                &mut state,
                &updates,
                &mut commands,
                command,
                daemon,
            )
            .await;
            match outcome {
                ReconnectOutcome::Resumed {
                    connection: next,
                    daemon: serving,
                    epoch,
                    close_window,
                } => {
                    connection = *next;
                    daemon = serving;
                    answered_reconnect = Some(epoch);
                    explorer_observation = ExplorerObservation::default();
                    agent_status_observation = AgentStatusObservation::default();
                    access.reset();
                    if close_window {
                        window_closing = true;
                        break;
                    }
                    continue;
                }
                ReconnectOutcome::Lost => {
                    lost = true;
                    break;
                }
                ReconnectOutcome::WindowGone => break,
            }
        }
        // The access list and its revokes (ADR 0014) never edit topology.
        let command = match command {
            Some(WindowTopologyCommand::Access(command)) => {
                if !access.handle(command, &mut connection, &updates).await {
                    break;
                }
                continue;
            }
            command => command,
        };
        let command = if let Some(command) = command {
            match handle_session_manager_command(
                &factory,
                command,
                &mut connection,
                &config,
                &image_cache,
                &updates,
                &mut state,
            )
            .await
            {
                TopologyManagerCommandOutcome::Continue => continue,
                TopologyManagerCommandOutcome::Stop => break,
                TopologyManagerCommandOutcome::Edit(command) => Some(command),
            }
        } else {
            None
        };
        let snapshot = match inspect_managed_topology(&mut connection).await {
            Ok(snapshot) => snapshot,
            // This manager's own connection was lost while a handoff was
            // preparing or fenced: it reconnects the Window.
            Err(error)
                if is_connection_loss(&error) && factory.handoff_status().begin_reconnect() =>
            {
                if factory.handoff_status().continuing() {
                    // The Window takes part in that handoff (M3.14): the hub
                    // remembered the loss, and the loop waits at its top for
                    // the outcome instead of reconnecting now.
                    let (pending, close_window) =
                        abandon_queued_commands(command, &mut commands, &mut state);
                    send_topology_updates(pending, &updates).await;
                    if close_window {
                        window_closing = true;
                        break;
                    }
                    continue;
                }
                let outcome = reconnect_and_abandon(
                    &factory,
                    &config,
                    &image_cache,
                    &mut state,
                    &updates,
                    &mut commands,
                    command,
                    daemon,
                )
                .await;
                match outcome {
                    ReconnectOutcome::Resumed {
                        connection: next,
                        daemon: serving,
                        epoch,
                        close_window,
                    } => {
                        connection = *next;
                        daemon = serving;
                        answered_reconnect = Some(epoch);
                        explorer_observation = ExplorerObservation::default();
                        agent_status_observation = AgentStatusObservation::default();
                        access.reset();
                        if close_window {
                            window_closing = true;
                            break;
                        }
                        continue;
                    }
                    ReconnectOutcome::Lost => {
                        lost = true;
                        break;
                    }
                    ReconnectOutcome::WindowGone => break,
                }
            }
            Err(error) => {
                let _ = updates
                    .send(WindowTopologyUpdate::Shutdown(format!("{error:#}")))
                    .await;
                return Err(error);
            }
        };
        if std::mem::take(&mut state.agent_status_republish) {
            explorer_observation = ExplorerObservation::default();
            agent_status_observation = AgentStatusObservation::default();
        }
        if explorer_observation.observe(&snapshot)
            && updates
                .send(WindowTopologyUpdate::LairExplorerInvalidated)
                .await
                .is_err()
        {
            break;
        }
        if let Some(status) = agent_status_observation.observe(&snapshot, &state.tabs)
            && updates
                .send(WindowTopologyUpdate::AgentStatus(status))
                .await
                .is_err()
        {
            break;
        }
        if access.due(std::time::Instant::now()) && !access.poll(&mut connection, &updates).await {
            break;
        }
        // The placeholder exists only in the frontend until this mutation is
        // acknowledged. An unrelated external topology change must therefore
        // reconcile together with the split, not against the placeholder alone.
        if !command_has_pending_split(command.as_ref())
            && !reconcile_managed_topology(
                &factory,
                &config,
                &image_cache,
                &mut state,
                &snapshot,
                &updates,
            )
            .await?
        {
            break;
        }
        let Some(command) = command else {
            continue;
        };
        let command = match command {
            WindowTopologyCommand::ExplorerContext { target, command } => {
                let lairs = snapshot.topology.lairs().cloned().collect::<Vec<_>>();
                if let Err(error) =
                    validate_explorer_context(target, &command, snapshot.revision, &lairs)
                {
                    let _ = updates
                        .send(WindowTopologyUpdate::TabFailed {
                            explorer_target: None,
                            dojo_id: None,
                            message: format!("{error:#}"),
                        })
                        .await;
                    continue;
                }
                *command
            }
            command => command,
        };
        let dojo_id = match &command {
            WindowTopologyCommand::Split { dojo_id, .. }
            | WindowTopologyCommand::Close { dojo_id, .. }
            | WindowTopologyCommand::AdjustRatio { dojo_id, .. }
            | WindowTopologyCommand::SetRatio { dojo_id, .. } => *dojo_id,
            _ => unreachable!("session command escaped manager dispatch"),
        };
        let operation = topology_edit_label(&command);
        let pending_split = match &command {
            WindowTopologyCommand::Split {
                target,
                pending: Some(pending),
                ..
            } => Some((*target, *pending)),
            _ => None,
        };
        let Some(managed) = topology_edit_target(&mut state.tabs, dojo_id) else {
            continue;
        };
        match apply_topology_command(
            &factory,
            &mut connection,
            &config,
            dojo_id,
            &managed.root,
            snapshot.revision,
            command,
        )
        .await
        {
            Ok(TopologyCommandOutcome::Updated { pending_focus }) => {
                managed.pending_focus = pending_focus;
            }
            Ok(TopologyCommandOutcome::WindowClosed) => {
                let removed = state.tabs.close(dojo_id).expect("edited tab remains");
                let acknowledged = remove_frontend_tab(&updates, dojo_id).await;
                retire_pane_tasks(removed.value.pane_tasks);
                if !acknowledged || state.tabs.is_empty() {
                    break;
                }
            }
            Err(error) => {
                if let Some((target, pending)) = pending_split {
                    let _ = updates
                        .send(WindowTopologyUpdate::Apply {
                            topology_revision: snapshot.revision,
                            dojo_id,
                            layout: managed.root.clone(),
                            added: Vec::new(),
                            removed: vec![pending],
                            focused: Some(target),
                        })
                        .await;
                }
                let message = format!("{operation} failed: {error:#}");
                let _ = updates
                    .send(WindowTopologyUpdate::TabFailed {
                        explorer_target: None,
                        dojo_id: Some(dojo_id),
                        message: message.clone(),
                    })
                    .await;
                let _ = message;
                eprintln!("splinterm topology edit rejected");
            }
        }
    }
    if lost {
        // The Window stays open, paused, with its last screen until it is
        // closed. Nothing here talks to the daemon again; its terminals may
        // still be running there.
        while let Some(command) = commands.recv().await {
            match command {
                WindowTopologyCommand::CloseWindow => break,
                // Visual tab switching keeps working (operator decision 1).
                WindowTopologyCommand::ActivateTab { dojo_id } => {
                    if let Some(update) = tab_switch_update(&mut state, dojo_id)
                        && updates.send(update).await.is_err()
                    {
                        break;
                    }
                }
                _ => {}
            }
        }
        window_closing = false;
    }
    if window_closing {
        refresh_reloaded_settings(&mut config, &mut settings);
        close_unused_tabs_on_exit(&mut connection, &config, &mut state).await;
    }
    let mut remaining_tasks = Vec::new();
    for tab in state.tabs.iter_mut() {
        remaining_tasks.extend(tab.value.pane_tasks.drain().map(|(_, task)| task));
    }
    for task in &remaining_tasks {
        task.cancellation.cancel();
    }
    for task in remaining_tasks {
        let _ = task.task.await;
    }
    Ok(())
}

pub(in crate::app) fn spawn_topology_smoke(
    commands: mpsc::Sender<WindowTopologyCommand>,
    dojo_id: DojoId,
    target: SplintId,
) -> Result<Option<tokio::task::JoinHandle<Result<()>>>> {
    if env::var_os("SPLINTERM_TOPOLOGY_SMOKE").is_none() {
        return Ok(None);
    }
    anyhow::ensure!(
        env::var_os("SPLINTERM_ENABLE_DEV_ATTACH").is_some(),
        "SPLINTERM_TOPOLOGY_SMOKE requires development attach"
    );
    Ok(Some(tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(750)).await;
        commands
            .send(WindowTopologyCommand::Split {
                dojo_id,
                target,
                axis: Axis::Horizontal,
                pending: None,
            })
            .await
            .map_err(|_| anyhow::anyhow!("topology smoke split channel closed"))?;
        tokio::time::sleep(std::time::Duration::from_millis(750)).await;
        commands
            .send(WindowTopologyCommand::AdjustRatio {
                dojo_id,
                target,
                delta: 100,
            })
            .await
            .map_err(|_| anyhow::anyhow!("topology smoke ratio channel closed"))
    })))
}

/// The identity of the Window's first Dojo, or `None` when the daemon no
/// longer has it (its terminal ended before the Window read the topology).
pub(in crate::app) async fn initial_window_dojo_identity(
    factory: &ConnectionFactory,
    dojo_id: DojoId,
) -> Result<
    Option<(
        WindowDojoIdentity,
        Option<splinterm::endpoint::RemoteDisplayIdentity>,
    )>,
> {
    let mut connection = factory.connect().await?;
    let remote_display_identity = factory.remote_display_identity(&connection);
    let Response::Lairs {
        lairs,
        topology_revision,
    } = connection.request(Request::ListLairs).await?
    else {
        bail!("splinterd did not return its Lairs for Window identity");
    };
    for lair in &lairs {
        if let Some(dojo) = lair.dojos.iter().find(|dojo| dojo.id == dojo_id) {
            return Ok(Some((
                window_dojo_identity(topology_revision, lair, dojo),
                remote_display_identity,
            )));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    use super::{
        Axis, CloseAction, DojoId, DojoTab, LairDirection, LairExplorerActivationTarget, LairId,
        LairTargetState, LayoutNode, NOT_RESTORABLE, PendingTopologyFocus, RefreshedCloseState,
        Response, SessionEntry, SessionPickerTarget, SplintId, SplintState, SplitRatio,
        TopologyCommandOutcome, TopologyManagerWake, TopologyRevision, WindowTabSet,
        WindowTopologyCommand, WindowTopologyUpdate, build_lair_prompt_target, cancel_pane_tasks,
        captured_dojo_kill_targets, close_action, close_other_tab_targets,
        close_target_already_closed, collect_lair_targets, command_has_pending_split,
        explorer_prompt_from_catalog, explorer_splint_from_snapshot, home_relative,
        lair_navigation_target, materialized_dojo_targets, next_topology_manager_wake,
        parent_ratio, pending_focus_for_observation, picker_catalog, refreshed_close_state,
        saved_layout_preview, select_live_dojo_from, send_existing_tab_activation,
        tab_organization_promotes, topology_command_outcome, topology_edit_target,
        topology_identity_diff, validate_exited_close_target, validate_lair_retention_target,
        window_has_tab_capacity,
    };
    use super::{
        ExplorerContextTarget, LairPromptKind, MutationTarget, NavigationNodeId, dojo_labels,
        dojo_prompt_label, lair_prompt_label,
    };
    use crate::app::pane_bridge::{PaneTask, pane_claims_initial_control};
    use splinterm::navigation_projection::{
        NavigationAction, NavigationAvailability, NavigationCapability,
        NavigationExplorerSplintTarget, NavigationPickerKind, NavigationPickerRow,
        NavigationPickerTarget, NavigationPickerView,
    };
    use splinterm_core::{Lair, Topology};
    use splinterm_protocol::{SplintLifecycle, SplintRuntimeSummary, TopologySnapshot};

    #[tokio::test]
    async fn existing_activation_updates_preserve_exact_explorer_correlation() {
        let lair_id = LairId::new();
        let dojo_id = DojoId::new();
        let splint_id = SplintId::new();
        let dojo_target = SessionPickerTarget {
            topology_revision: TopologyRevision::new(7),
            lair_id,
            dojo_id,
            action: NavigationAction::ActivateDojo,
        };
        let (updates, mut receiver) = mpsc::channel(1);
        send_existing_tab_activation(
            &updates,
            dojo_id,
            None,
            Some(LairExplorerActivationTarget::Dojo(dojo_target)),
        )
        .await
        .unwrap();
        let WindowTopologyUpdate::ActivateTab {
            dojo_id: updated_dojo,
            explorer_target,
        } = receiver.recv().await.unwrap()
        else {
            panic!("expected Dojo activation");
        };
        assert_eq!(updated_dojo, dojo_id);
        assert_eq!(
            explorer_target,
            Some(LairExplorerActivationTarget::Dojo(dojo_target))
        );

        let splint_target = NavigationExplorerSplintTarget {
            topology_revision: TopologyRevision::new(8),
            lair_id,
            dojo_id,
            splint_id,
            live_incarnation: Some(4),
            last_incarnation: Some(4),
            capability: NavigationCapability {
                action: NavigationAction::FocusSplint,
                availability: NavigationAvailability::Enabled,
            },
        };
        send_existing_tab_activation(
            &updates,
            dojo_id,
            Some((splint_id, Some(4))),
            Some(LairExplorerActivationTarget::Splint(splint_target)),
        )
        .await
        .unwrap();
        let WindowTopologyUpdate::ActivateSplint {
            dojo_id: updated_dojo,
            splint_id: updated_splint,
            live_incarnation,
            explorer_target,
        } = receiver.recv().await.unwrap()
        else {
            panic!("expected Splint activation");
        };
        assert_eq!((updated_dojo, updated_splint), (dojo_id, splint_id));
        assert_eq!(live_incarnation, Some(4));
        assert_eq!(
            explorer_target,
            Some(LairExplorerActivationTarget::Splint(splint_target))
        );
    }

    #[test]
    fn explorer_splint_target_rejects_stale_revision_parent_and_incarnation() {
        let mut lair = Lair::new("saved", PathBuf::from("/tmp"));
        let dojo_id = lair.dojos[0].id;
        let splint_id = lair.dojos[0].default_focus;
        let splint = lair.dojos[0].root.find_splint_mut(splint_id).unwrap();
        splint.state = SplintState::Running;
        splint.last_incarnation = Some(7);
        let lair_id = lair.id;
        let mut topology = Topology::new();
        topology.insert_lair_at(topology.revision(), lair).unwrap();
        let snapshot = TopologySnapshot {
            revision: topology.revision(),
            topology,
            runtimes: vec![SplintRuntimeSummary {
                splint_id,
                live_incarnation: Some(7),
                last_incarnation: Some(7),
                restorable: false,
                lifecycle: SplintLifecycle::Running,
                exit_status: None,
                agent_status: None,
            }],
        };
        let target = NavigationExplorerSplintTarget {
            topology_revision: snapshot.revision,
            lair_id,
            dojo_id,
            splint_id,
            live_incarnation: Some(7),
            last_incarnation: Some(7),
            capability: NavigationCapability {
                action: NavigationAction::FocusSplint,
                availability: NavigationAvailability::Enabled,
            },
        };
        assert!(explorer_splint_from_snapshot(&snapshot, target, false).is_ok());
        assert!(
            explorer_splint_from_snapshot(
                &snapshot,
                NavigationExplorerSplintTarget {
                    topology_revision: TopologyRevision::new(999),
                    ..target
                },
                false,
            )
            .is_err()
        );
        assert!(
            explorer_splint_from_snapshot(
                &snapshot,
                NavigationExplorerSplintTarget {
                    lair_id: LairId::new(),
                    ..target
                },
                false,
            )
            .is_err()
        );
        assert!(
            explorer_splint_from_snapshot(
                &snapshot,
                NavigationExplorerSplintTarget {
                    live_incarnation: Some(8),
                    ..target
                },
                false,
            )
            .is_err()
        );
    }

    #[test]
    fn tab_organization_promotion_requires_both_config_and_transient_ownership() {
        let mut config = splinterm::config::AppConfig::default();
        assert!(tab_organization_promotes(&config, true));
        assert!(!tab_organization_promotes(&config, false));
        config.multiplexer_lifetime.persist_on_tab_organization = false;
        assert!(!tab_organization_promotes(&config, true));
    }

    #[test]
    fn live_dojo_selection_accepts_transient_tabs_but_rejects_exited_layouts() {
        let mut transient = splinterm_core::Lair::transient("transient", PathBuf::from("/tmp"));
        let dojo_id = transient.dojos[0].id;
        let splint_id = transient.dojos[0].default_focus;
        transient.dojos[0]
            .root
            .find_splint_mut(splint_id)
            .unwrap()
            .state = SplintState::Running;

        assert!(select_live_dojo_from(&[transient.clone()], transient.id, dojo_id).is_ok());

        transient.dojos[0]
            .root
            .find_splint_mut(splint_id)
            .unwrap()
            .state = SplintState::Exited(0);
        assert!(select_live_dojo_from(&[transient.clone()], transient.id, dojo_id).is_err());
    }

    #[test]
    fn explorer_dojo_restore_preview_is_revision_bound_and_scoped() {
        let mut lair = splinterm_core::Lair::new("saved", PathBuf::from("/tmp"));
        let dojo_id = lair.dojos[0].id;
        let splint_id = lair.dojos[0].default_focus;
        let pane = lair.dojos[0].root.find_splint_mut(splint_id).unwrap();
        pane.state = SplintState::Exited(0);
        pane.last_incarnation = Some(7);
        let other = splinterm_core::Lair::new("other", PathBuf::from("/tmp"));
        lair.dojos.push(other.dojos[0].clone());
        let revision = TopologyRevision::new(7);
        let guard = ExplorerContextTarget {
            topology_revision: revision,
            node: NavigationNodeId::Dojo {
                lair_id: lair.id,
                dojo_id,
            },
            live_incarnation: None,
        };
        let catalog = [lair];
        let target =
            explorer_prompt_from_catalog(&catalog, revision, guard, LairPromptKind::Restore)
                .unwrap();
        assert_eq!(target.dojo_id, Some(dojo_id));
        assert_eq!(target.topology_revision, revision);
        assert_eq!(target.targets.len(), 1);
        assert_eq!(target.targets[0].splint_id, splint_id);
        assert_eq!(target.targets[0].incarnation, 7);
        assert!(
            explorer_prompt_from_catalog(
                &catalog,
                TopologyRevision::new(8),
                guard,
                LairPromptKind::Restore
            )
            .is_err()
        );
        let wrong_parent = ExplorerContextTarget {
            node: NavigationNodeId::Dojo {
                lair_id: LairId::new(),
                dojo_id,
            },
            ..guard
        };
        assert!(
            explorer_prompt_from_catalog(&catalog, revision, wrong_parent, LairPromptKind::Restore)
                .is_err()
        );
        let live = ExplorerContextTarget {
            node: NavigationNodeId::Dojo {
                lair_id: catalog[0].id,
                dojo_id: catalog[0].dojos[1].id,
            },
            ..guard
        };
        assert!(
            explorer_prompt_from_catalog(&catalog, revision, live, LairPromptKind::Restore)
                .is_err()
        );
    }

    #[test]
    fn restore_prompt_targets_only_exited_splints() {
        let mut lair = splinterm_core::Lair::new("saved", PathBuf::from("/tmp"));
        let dojo = &mut lair.dojos[0];
        let mut exited = splinterm_core::Splint::shell(PathBuf::from("/tmp"));
        exited.state = SplintState::Exited(0);
        exited.last_incarnation = Some(7);
        let exited_id = exited.id;
        let mut running = splinterm_core::Splint::shell(PathBuf::from("/tmp"));
        let running_id = running.id;
        running.state = SplintState::Running;
        running.last_incarnation = Some(8);
        let mut starting = splinterm_core::Splint::shell(PathBuf::from("/tmp"));
        let starting_id = starting.id;
        starting.state = SplintState::Starting;
        starting.last_incarnation = Some(9);
        dojo.default_focus = running_id;
        dojo.root = LayoutNode::Branch {
            axis: Axis::Horizontal,
            ratio: SplitRatio::new(500).unwrap(),
            first: Box::new(LayoutNode::Leaf(exited)),
            second: Box::new(LayoutNode::Branch {
                axis: Axis::Vertical,
                ratio: SplitRatio::new(500).unwrap(),
                first: Box::new(LayoutNode::Leaf(running)),
                second: Box::new(LayoutNode::Leaf(starting)),
            }),
        };

        let restore_targets = collect_lair_targets(&lair, LairTargetState::Exited).unwrap();
        assert_eq!(restore_targets.len(), 1);
        assert_eq!(restore_targets[0].splint_id, exited_id);
        assert_eq!(restore_targets[0].incarnation, 7);
        // The restore's preview lists the Splints it launches and no
        // running one, so its count and its rows agree.
        let preview = saved_layout_preview(
            std::slice::from_ref(&lair),
            &lair,
            Some(&restore_targets),
            None,
        );
        assert_eq!(
            preview.lines().filter(|line| line.contains('\t')).count(),
            restore_targets.len()
        );
        // With every Splint running there is nothing to restore and no row.
        let none = saved_layout_preview(std::slice::from_ref(&lair), &lair, Some(&[]), None);
        assert!(!none.contains('\t'));

        let terminate_targets = collect_lair_targets(&lair, LairTargetState::Live).unwrap();
        assert_eq!(terminate_targets.len(), 2);
        assert_eq!(terminate_targets[0].splint_id, running_id);
        assert_eq!(terminate_targets[0].incarnation, 8);
        assert_eq!(terminate_targets[1].splint_id, starting_id);
        assert_eq!(terminate_targets[1].incarnation, 9);
    }

    #[test]
    fn saved_layout_rows_name_the_recipe_and_keep_one_dojo_for_its_restore() {
        use splinterm_core::Dojo;

        let mut lair = splinterm_core::Lair::new("saved", PathBuf::from("/home/user/src"));
        lair.provenance = Some("from\nelsewhere\u{202e}".to_owned());
        let mut second = Dojo::with_shell("build", PathBuf::from("/home/user"));
        let LayoutNode::Leaf(splint) = &mut second.root else {
            unreachable!("a new Dojo is one Splint");
        };
        splint.command = vec![String::new()];
        lair.dojos.push(second);
        let home = PathBuf::from("/home/user");
        let catalog = std::slice::from_ref(&lair);
        let lines = saved_layout_preview(catalog, &lair, None, Some(&home));
        let lines = lines.lines().collect::<Vec<_>>();
        assert_eq!(lines[0], "saved · disposable · 2 Dojos");
        // Dojos are named as the Explorer names them.
        assert_eq!(lines[1], format!("{}\tshell\t~/src", dojo_labels(&lair)[0]));
        assert_eq!(lines[2], format!("build\t{NOT_RESTORABLE}\t~"));
        assert_eq!(lines[3], "Origin: from elsewhere");
        // A restore lists the rows of the Splints it launches only.
        let restoring = [MutationTarget {
            lair_id: lair.id,
            dojo_id: lair.dojos[1].id,
            splint_id: lair.dojos[1].default_focus,
            incarnation: 1,
        }];
        let only = saved_layout_preview(catalog, &lair, Some(&restoring), None);
        let rows = only
            .lines()
            .filter(|line| line.contains('\t'))
            .collect::<Vec<_>>();
        assert_eq!(rows, [format!("build\t{NOT_RESTORABLE}\t/home/user")]);
        assert_eq!(
            home_relative(&PathBuf::from("/home/username"), Some(&home)),
            "/home/username"
        );
    }

    #[test]
    fn saved_layout_preview_names_the_lair_as_the_explorer_does() {
        // A generated name is the Explorer's "Lair N", never the internal
        // name; the numbers count the persistent Lairs before it.
        let first =
            splinterm_core::Lair::new("terminal-1793144588160264270-1363414", "/tmp".into());
        let second =
            splinterm_core::Lair::new("terminal-1793144588160264271-1363415", "/tmp".into());
        let catalog = [first, second];
        let summary = |lairs: &[splinterm_core::Lair], lair: &splinterm_core::Lair| {
            saved_layout_preview(lairs, lair, None, None)
                .lines()
                .next()
                .unwrap()
                .to_owned()
        };
        assert_eq!(
            summary(&catalog, &catalog[1]),
            "Lair 2 · disposable · 1 Dojo"
        );
        // A chosen name is kept, made one line.
        let named = splinterm_core::Lair::new("work\nnotes", "/tmp".into());
        assert_eq!(
            summary(std::slice::from_ref(&named), &named),
            "work notes · disposable · 1 Dojo"
        );
        // With no name and no Explorer label, the ID stands in.
        let unnamed = splinterm_core::Lair::transient("", "/tmp".into());
        assert_eq!(lair_prompt_label(&[], &unnamed), unnamed.id.to_string());
        // A Dojo's prompt names it as the Explorer does too.
        assert_eq!(
            dojo_prompt_label(&catalog[0], catalog[0].dojos[0].id).as_deref(),
            Some(dojo_labels(&catalog[0])[0].as_str())
        );
    }

    #[test]
    fn lair_lifecycle_targets_revalidate_retention_and_restorable_leaves() {
        use splinterm_core::LairRetention;

        let mut lair = splinterm_core::Lair::new("saved", PathBuf::from("/tmp"));
        let splint_id = lair.dojos[0].default_focus;
        lair.retention = LairRetention::Saved;
        lair.dojos[0].root.find_splint_mut(splint_id).unwrap().state = SplintState::Running;
        let revision = TopologyRevision::new(7);

        assert!(
            build_lair_prompt_target(
                revision,
                std::slice::from_ref(&lair),
                &lair,
                LairPromptKind::Restore,
                Some(LairRetention::Saved),
            )
            .is_err()
        );
        assert!(
            build_lair_prompt_target(
                revision,
                std::slice::from_ref(&lair),
                &lair,
                LairPromptKind::Preview,
                Some(LairRetention::Pinned),
            )
            .is_err()
        );

        let splint = lair.dojos[0].root.find_splint_mut(splint_id).unwrap();
        splint.state = SplintState::Exited(0);
        splint.last_incarnation = Some(4);
        // The title is the program's own and is never shown; the recipe is.
        splint.title = "rm -rf\t/".to_owned();
        splint.command = vec!["/usr/bin/nvim".to_owned(), "src/main\n.rs".to_owned()];
        lair.dojos[0].name = "Dojo\n1\tx".to_owned();
        let target = build_lair_prompt_target(
            revision,
            std::slice::from_ref(&lair),
            &lair,
            LairPromptKind::Restore,
            Some(LairRetention::Saved),
        )
        .unwrap();
        assert_eq!(target.targets.len(), 1);
        assert_eq!(target.targets[0].splint_id, splint_id);
        // The preview: a summary line, one tab-separated row per Splint
        // (Dojo, what it launches, directory) that the prompt lines up as a
        // table, a name's own newline and tab made spaces so it adds no
        // line or cell, and what a restore leaves out.
        let lines = target.preview.lines().collect::<Vec<_>>();
        assert_eq!(lines[0], "saved · saved · 1 Dojo");
        assert_eq!(lines[1], "Dojo 1 x\tnvim src/main .rs\t/tmp");
        assert_eq!(
            lines.last().copied(),
            Some(
                "Not restored: scrollback, running programs' state, shell state, environment, clipboard, images."
            )
        );
        assert_eq!(lines.len(), 3);
        assert!(!target.preview.contains("rm -rf"));

        for actual in [
            LairRetention::Disposable,
            LairRetention::Saved,
            LairRetention::Pinned,
        ] {
            lair.retention = actual;
            for expected in [
                LairRetention::Disposable,
                LairRetention::Saved,
                LairRetention::Pinned,
            ] {
                assert_eq!(
                    validate_lair_retention_target(std::slice::from_ref(&lair), lair.id, expected,)
                        .is_ok(),
                    actual == expected,
                    "actual={actual:?} expected={expected:?}"
                );
            }
        }

        lair.retention = LairRetention::Disposable;
        assert!(
            build_lair_prompt_target(
                revision,
                std::slice::from_ref(&lair),
                &lair,
                LairPromptKind::Preview,
                Some(LairRetention::Disposable),
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn closed_window_command_channel_stops_before_another_topology_poll() {
        let (sender, mut commands) = mpsc::channel(1);
        drop(sender);
        let mut poll = tokio::time::interval(Duration::from_secs(60));
        let mut poll_priority = false;

        assert!(matches!(
            next_topology_manager_wake(&mut commands, &mut poll, &mut poll_priority).await,
            TopologyManagerWake::Shutdown
        ));
    }

    #[tokio::test]
    async fn ready_poll_follows_one_command_when_both_remain_ready() {
        let (sender, mut commands) = mpsc::channel(2);
        sender
            .send(WindowTopologyCommand::RequestSessionPicker)
            .await
            .unwrap();
        let mut poll = tokio::time::interval(Duration::from_millis(1));
        let mut poll_priority = false;

        assert!(matches!(
            next_topology_manager_wake(&mut commands, &mut poll, &mut poll_priority).await,
            TopologyManagerWake::Command(_)
        ));
        sender
            .send(WindowTopologyCommand::RequestSessionPicker)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert!(matches!(
            next_topology_manager_wake(&mut commands, &mut poll, &mut poll_priority).await,
            TopologyManagerWake::Poll
        ));
    }

    #[test]
    fn termination_targets_exact_captured_incarnations_without_expanding_scope() {
        let mut live = splinterm_core::Splint::shell(PathBuf::from("/tmp"));
        let live_id = live.id;
        live.state = SplintState::Running;
        live.last_incarnation = Some(7);
        let mut exited = splinterm_core::Splint::shell(PathBuf::from("/tmp"));
        let exited_id = exited.id;
        exited.state = SplintState::Exited(0);
        exited.last_incarnation = Some(8);
        let root = LayoutNode::Branch {
            axis: Axis::Horizontal,
            ratio: SplitRatio::new(500).unwrap(),
            first: Box::new(LayoutNode::Leaf(live)),
            second: Box::new(LayoutNode::Leaf(exited)),
        };
        assert_eq!(
            captured_dojo_kill_targets(&root, &[(live_id, 7), (exited_id, 8)]).unwrap(),
            vec![(live_id, 7)]
        );
        assert!(captured_dojo_kill_targets(&root, &[(live_id, 7)]).is_err());
        assert!(captured_dojo_kill_targets(&root, &[(live_id, 9), (exited_id, 8)]).is_err());
        assert!(captured_dojo_kill_targets(&root, &[(live_id, 7), (live_id, 7)]).is_err());
    }

    #[test]
    fn close_action_kills_live_panes_and_removes_exited_panes() {
        let mut live = splinterm_core::Splint::shell(PathBuf::from("/tmp"));
        let live_id = live.id;
        live.state = SplintState::Running;
        live.last_incarnation = Some(7);
        assert_eq!(
            close_action(&LayoutNode::Leaf(live), live_id).unwrap(),
            CloseAction::KillAndClose { incarnation: 7 }
        );

        let mut exited = splinterm_core::Splint::shell(PathBuf::from("/tmp"));
        let exited_id = exited.id;
        exited.state = SplintState::Exited(0);
        exited.last_incarnation = Some(7);
        let exited_root = LayoutNode::Leaf(exited.clone());
        assert_eq!(
            close_action(&exited_root, exited_id).unwrap(),
            CloseAction::CloseExited
        );
        assert!(!close_target_already_closed(&exited_root, exited_id));
        assert!(close_target_already_closed(&exited_root, SplintId::new()));
        assert!(validate_exited_close_target(&exited_root, exited_id, Some(7)).unwrap());
        assert!(validate_exited_close_target(&exited_root, exited_id, Some(8)).is_err());

        let sibling = splinterm_core::Splint::shell(PathBuf::from("/tmp"));
        let split_root = LayoutNode::Branch {
            axis: Axis::Horizontal,
            ratio: SplitRatio::new(500).unwrap(),
            first: Box::new(LayoutNode::Leaf(exited)),
            second: Box::new(LayoutNode::Leaf(sibling)),
        };
        assert!(!validate_exited_close_target(&split_root, exited_id, Some(7)).unwrap());
        assert_eq!(
            refreshed_close_state(Some(&split_root), exited_id, Some(7)).unwrap(),
            RefreshedCloseState::Retry
        );
        assert_eq!(
            refreshed_close_state(None, exited_id, Some(7)).unwrap(),
            RefreshedCloseState::WindowClosed
        );
        let unrelated = LayoutNode::Leaf(splinterm_core::Splint::shell(PathBuf::from("/tmp")));
        assert_eq!(
            refreshed_close_state(Some(&unrelated), exited_id, Some(7)).unwrap(),
            RefreshedCloseState::TargetClosed
        );

        let missing_incarnation = splinterm_core::Splint::shell(PathBuf::from("/tmp"));
        let missing_id = missing_incarnation.id;
        assert!(close_action(&LayoutNode::Leaf(missing_incarnation), missing_id).is_err());
    }

    #[tokio::test]
    async fn closed_tab_tasks_are_cancelled_and_joined_before_cleanup_returns() {
        let completed = Arc::new(AtomicUsize::new(0));
        let mut tasks = HashMap::new();
        for _ in 0..3 {
            let cancellation = CancellationToken::new();
            let task_cancellation = cancellation.clone();
            let task_completed = Arc::clone(&completed);
            tasks.insert(
                SplintId::new(),
                PaneTask {
                    cancellation,
                    task: tokio::spawn(async move {
                        task_cancellation.cancelled().await;
                        task_completed.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }),
                    detach_on_cancel: Arc::default(),
                },
            );
        }

        cancel_pane_tasks(tasks).await;

        assert_eq!(completed.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn uncooperative_retired_tab_task_does_not_block_topology_work() {
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            task_cancellation.cancelled().await;
            std::future::pending::<()>().await;
            Ok(())
        });
        let abort = task.abort_handle();
        let tasks = HashMap::from([(
            SplintId::new(),
            PaneTask {
                cancellation,
                task,
                detach_on_cancel: Arc::default(),
            },
        )]);

        super::retire_pane_tasks(tasks);
        tokio::task::yield_now().await;

        assert!(!abort.is_finished());
        abort.abort();
    }

    #[test]
    fn closed_dojo_topology_edits_are_stale_instead_of_fatal() {
        let dojo_id = DojoId::new();
        let mut tabs = WindowTabSet::new(DojoTab::new(LairId::new(), dojo_id, 7));
        assert_eq!(topology_edit_target(&mut tabs, dojo_id).copied(), Some(7));

        assert!(tabs.close(dojo_id).is_some());
        assert_eq!(topology_edit_target(&mut tabs, dojo_id), None);
    }

    #[test]
    fn tab_creation_capacity_is_rejected_before_daemon_creation() {
        assert!(window_has_tab_capacity(0));
        assert!(window_has_tab_capacity(splinterm::tab::MAX_WINDOW_TABS - 1));
        assert!(!window_has_tab_capacity(splinterm::tab::MAX_WINDOW_TABS));
    }

    #[test]
    fn preset_reconciliation_requires_every_stable_pane_before_opening() {
        let lair_id = LairId::new();
        let dojo_id = DojoId::new();
        let first = splinterm_core::Splint::shell(PathBuf::from("/tmp"));
        let first_id = first.id;
        let second = splinterm_core::Splint::shell(PathBuf::from("/tmp"));
        let second_id = second.id;
        let dojo = splinterm_core::Dojo {
            id: dojo_id,
            name: "preset".into(),
            default_focus: second_id,
            root: LayoutNode::Branch {
                axis: Axis::Horizontal,
                ratio: SplitRatio::new(500).unwrap(),
                first: Box::new(LayoutNode::Leaf(first)),
                second: Box::new(LayoutNode::Leaf(second)),
            },
        };
        let lair = splinterm_core::Lair {
            id: lair_id,
            name: "main".into(),
            lifetime: splinterm_core::LairLifetime::Persistent,
            retention: splinterm_core::LairRetention::default(),
            provenance: None,
            dojos: vec![dojo],
        };
        let pane = |key: &str, splint_id| splinterm_protocol::PresetPaneIdentity {
            dojo_id,
            key: key.into(),
            splint_id,
        };
        let targets = materialized_dojo_targets(
            std::slice::from_ref(&lair),
            TopologyRevision::new(8),
            lair_id,
            &[dojo_id],
            &[pane("first", first_id), pane("second", second_id)],
        )
        .unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].0.topology_revision, TopologyRevision::new(8));
        assert_eq!(targets[0].1.default_focus, second_id);
        assert!(
            materialized_dojo_targets(
                &[lair],
                TopologyRevision::new(8),
                lair_id,
                &[dojo_id],
                &[pane("first", first_id)],
            )
            .is_err()
        );
    }

    #[test]
    fn close_other_tabs_is_bounded_deduplicated_and_retains_exact_target() {
        let retained = DojoId::new();
        let first = DojoId::new();
        let second = DojoId::new();
        assert_eq!(
            close_other_tab_targets(
                retained,
                true,
                vec![first, retained, second, first, retained]
            ),
            Some(vec![first, second])
        );
        assert_eq!(close_other_tab_targets(retained, false, vec![first]), None);
        assert_eq!(
            close_other_tab_targets(
                retained,
                true,
                vec![first; splinterm::tab::MAX_WINDOW_TABS + 1]
            ),
            None
        );
    }

    #[test]
    fn lair_navigation_prefers_most_recent_attached_dojo_by_stable_id() {
        let first_lair = LairId::new();
        let empty_lair = LairId::new();
        let second_lair = LairId::new();
        let first_dojo = DojoId::new();
        let second_dojo = DojoId::new();
        let recent_second_dojo = DojoId::new();
        let mut tabs = WindowTabSet::new(DojoTab::new(first_lair, first_dojo, ()));
        tabs.open_or_activate(DojoTab::new(second_lair, second_dojo, ()))
            .unwrap();
        tabs.open_or_activate(DojoTab::new(second_lair, recent_second_dojo, ()))
            .unwrap();
        assert!(tabs.activate(second_dojo));
        assert!(tabs.activate(first_dojo));
        let entry = |lair_id, dojo_id| SessionEntry {
            lair_id,
            dojo_id,
            lair_name: "lair".to_owned(),
            dojo_name: "dojo".to_owned(),
            lair_label: "lair".to_owned(),
            dojo_label: "dojo".to_owned(),
            cwd: PathBuf::from("/tmp"),
            pane_count: 1,
            running_panes: 1,
            exited_panes: 0,
        };
        let entries = vec![
            entry(first_lair, first_dojo),
            entry(second_lair, recent_second_dojo),
            entry(second_lair, second_dojo),
        ];
        assert_eq!(
            lair_navigation_target(
                &[first_lair, empty_lair, second_lair],
                &entries,
                &tabs,
                first_lair,
                LairDirection::Next,
            )
            .unwrap(),
            (second_lair, second_dojo)
        );
    }

    #[test]
    fn picker_catalog_preserves_projection_policy_and_breadcrumbs() {
        let lair_id = LairId::new();
        let dojo_id = DojoId::new();
        let target = NavigationPickerTarget {
            topology_revision: TopologyRevision::new(7),
            lair_id,
            dojo_id,
            action: NavigationAction::ActivateDojo,
        };
        let view = NavigationPickerView {
            topology_revision: TopologyRevision::new(7),
            rows: vec![NavigationPickerRow {
                display_title: "editor".to_owned(),
                breadcrumb: "work / editor".to_owned(),
                working_directory: "/work".to_owned(),
                pane_count: 2,
                running_pane_count: 2,
                agent: None,
                target,
            }],
            kind: NavigationPickerKind::RecentDojos,
            creation: Some(NavigationCapability {
                action: NavigationAction::CreateDojo,
                availability: NavigationAvailability::Disabled(
                    splinterm::navigation_projection::NavigationBlocker::TabCapacityReached,
                ),
            }),
            initial_row: Some(0),
        };

        let catalog = picker_catalog(view);
        assert_eq!(catalog.items[0].display_title, "editor");
        assert_eq!(catalog.items[0].breadcrumb, "work / editor");
        assert_eq!(catalog.items[0].working_directory, "/work");
        assert_eq!(
            catalog.targets[0].topology_revision,
            TopologyRevision::new(7)
        );
        assert_eq!(catalog.targets[0].action, target.action);
        assert!(!catalog.creation_enabled);
        assert_eq!(
            catalog.creation_target,
            Some(splinterm::SessionPickerCreationTarget {
                topology_revision: TopologyRevision::new(7),
                action: NavigationAction::CreateDojo,
            })
        );
        assert_eq!(
            catalog.creation_blocker,
            Some("Window tab capacity reached")
        );
    }

    #[test]
    fn topology_diff_replaces_restored_incarnation_not_metadata_or_exit() {
        let mut old = splinterm_core::Splint::shell(PathBuf::from("/tmp"));
        old.state = SplintState::Exited(0);
        old.last_incarnation = Some(7);
        let id = old.id;
        let previous = LayoutNode::Leaf(old.clone());
        for state in [SplintState::Starting, SplintState::Running] {
            let mut restored = old.clone();
            restored.state = state;
            restored.last_incarnation = Some(8);
            let next = LayoutNode::Leaf(restored);
            assert_eq!(
                topology_identity_diff(&previous, &next),
                (vec![id], vec![id])
            );
            assert_eq!(topology_identity_diff(&next, &next), (vec![], vec![]));
        }
        for state in [SplintState::Exited(1), SplintState::Running] {
            let mut metadata = old.clone();
            metadata.state = state;
            metadata.title = "renamed".into();
            assert_eq!(
                topology_identity_diff(&previous, &LayoutNode::Leaf(metadata)),
                (vec![], vec![])
            );
        }
        let mut already_exited = old.clone();
        already_exited.last_incarnation = Some(8);
        assert_eq!(
            topology_identity_diff(&previous, &LayoutNode::Leaf(already_exited)),
            (vec![], vec![])
        );
        let mut first_live = old.clone();
        first_live.state = SplintState::Running;
        old.last_incarnation = None;
        assert_eq!(
            topology_identity_diff(&LayoutNode::Leaf(old), &LayoutNode::Leaf(first_live)),
            (vec![id], vec![id])
        );
    }

    #[test]
    fn topology_diff_and_parent_ratio_are_identity_local() {
        let first = splinterm_core::Splint::shell(PathBuf::from("/tmp"));
        let first_id = first.id;
        let second = splinterm_core::Splint::shell(PathBuf::from("/tmp"));
        let second_id = second.id;
        let third = splinterm_core::Splint::shell(PathBuf::from("/tmp"));
        let third_id = third.id;
        let initial = LayoutNode::Branch {
            axis: Axis::Horizontal,
            ratio: SplitRatio::new(400).unwrap(),
            first: Box::new(LayoutNode::Leaf(first.clone())),
            second: Box::new(LayoutNode::Leaf(second.clone())),
        };
        let nested = LayoutNode::Branch {
            axis: Axis::Horizontal,
            ratio: SplitRatio::new(400).unwrap(),
            first: Box::new(LayoutNode::Leaf(first)),
            second: Box::new(LayoutNode::Branch {
                axis: Axis::Vertical,
                ratio: SplitRatio::new(650).unwrap(),
                first: Box::new(LayoutNode::Leaf(second)),
                second: Box::new(LayoutNode::Leaf(third)),
            }),
        };
        let (added, removed) = topology_identity_diff(&initial, &nested);
        assert_eq!(added, vec![third_id]);
        assert!(removed.is_empty());
        assert_eq!(parent_ratio(&nested, first_id).unwrap().get(), 400);
        assert_eq!(parent_ratio(&nested, second_id).unwrap().get(), 650);
        assert!(pane_claims_initial_control(second_id, second_id));
        assert!(!pane_claims_initial_control(first_id, second_id));
        assert_eq!(parent_ratio(&nested, third_id).unwrap().get(), 650);
    }

    #[test]
    fn optimistic_split_defers_only_its_pre_mutation_reconciliation() {
        let dojo_id = DojoId::new();
        let target = SplintId::new();
        assert!(command_has_pending_split(Some(
            &WindowTopologyCommand::Split {
                dojo_id,
                target,
                axis: Axis::Horizontal,
                pending: Some(SplintId::new()),
            }
        )));
        assert!(!command_has_pending_split(Some(
            &WindowTopologyCommand::Split {
                dojo_id,
                target,
                axis: Axis::Horizontal,
                pending: None,
            }
        )));
        assert!(!command_has_pending_split(None));
    }

    #[test]
    fn successful_split_focuses_the_new_local_splint() {
        let splint_id = SplintId::new();
        let placeholder = SplintId::new();
        assert_eq!(
            topology_command_outcome(
                Response::SplintStarted {
                    splint_id,
                    incarnation: 1,
                    topology_revision: TopologyRevision::new(2),
                },
                Some(placeholder),
            )
            .unwrap(),
            TopologyCommandOutcome::Updated {
                pending_focus: Some(PendingTopologyFocus {
                    splint_id,
                    revision: TopologyRevision::new(2),
                    placeholder: Some(placeholder),
                })
            }
        );
        assert_eq!(
            topology_command_outcome(
                Response::TopologyCommitted {
                    topology_revision: TopologyRevision::new(3),
                },
                None,
            )
            .unwrap(),
            TopologyCommandOutcome::Updated {
                pending_focus: None
            }
        );
    }

    #[test]
    fn pending_split_focus_is_revision_bound_and_requires_the_added_splint() {
        let splint_id = SplintId::new();
        let unrelated = SplintId::new();
        let placeholder = SplintId::new();
        let pending = Some(PendingTopologyFocus {
            splint_id,
            revision: TopologyRevision::new(4),
            placeholder: Some(placeholder),
        });

        assert_eq!(
            pending_focus_for_observation(pending, TopologyRevision::new(3), &[splint_id]),
            (None, None, false)
        );
        assert_eq!(
            pending_focus_for_observation(pending, TopologyRevision::new(4), &[splint_id]),
            (Some(splint_id), Some(placeholder), true)
        );
        assert_eq!(
            pending_focus_for_observation(pending, TopologyRevision::new(4), &[unrelated]),
            (None, Some(placeholder), true)
        );
        assert_eq!(
            pending_focus_for_observation(pending, TopologyRevision::new(5), &[]),
            (None, Some(placeholder), true)
        );
    }
}

#[cfg(test)]
#[path = "explorer_context_tests.rs"]
mod explorer_context_tests;

#[cfg(test)]
#[path = "close_unused_tests.rs"]
mod close_unused_tests;

#[cfg(test)]
#[path = "creation_directory_tests.rs"]
mod creation_directory_tests;

/// Plan 0061 M3.12b review S5 and S6: commands queued before or during a
/// reconnection are settled without the daemon.
#[cfg(test)]
mod reconnect_abandon_tests {
    use std::path::PathBuf;

    use splinterm::navigation_projection::NavigationAction;
    use splinterm_core::{LairRetention, Splint};

    use super::*;

    fn managed(lair_id: LairId, dojo_id: DojoId, root: LayoutNode) -> DojoTab<ManagedDojo> {
        DojoTab::new(
            lair_id,
            dojo_id,
            ManagedDojo {
                identity: WindowDojoIdentity {
                    topology_revision: TopologyRevision::new(9),
                    lair_id,
                    dojo_id,
                    lair_name: "main".to_owned(),
                    lair_retention: LairRetention::default(),
                    dojo_name: "terminal".to_owned(),
                },
                root,
                pending_focus: None,
                pane_tasks: HashMap::new(),
            },
        )
    }

    fn leaf(id: SplintId) -> LayoutNode {
        let mut splint = Splint::shell(PathBuf::from("/tmp"));
        splint.id = id;
        LayoutNode::Leaf(splint)
    }

    /// Two tabs; the first, active, shows `target`.
    fn two_tabs(target: SplintId) -> (TopologyManagerState, DojoId, DojoId) {
        let lair_id = LairId::new();
        let (first, second) = (DojoId::new(), DojoId::new());
        let mut tabs = WindowTabSet::new(managed(lair_id, first, leaf(target)));
        tabs.open_or_activate(managed(lair_id, second, leaf(SplintId::new())))
            .unwrap();
        assert!(tabs.activate(first));
        let state = TopologyManagerState {
            tabs,
            transient_owners: HashMap::new(),
            agent_status_republish: false,
        };
        (state, first, second)
    }

    #[test]
    fn queued_commands_are_abandoned_and_the_close_notice_kept() {
        let target = SplintId::new();
        let placeholder = SplintId::new();
        let (mut state, first, _) = two_tabs(target);
        let root_before = state.tabs.get(first).unwrap().value.root.clone();
        let (sender, mut commands) = mpsc::channel(8);
        sender
            .try_send(WindowTopologyCommand::RequestSessionPicker)
            .unwrap();
        sender
            .try_send(WindowTopologyCommand::OpenDojo {
                target: SessionPickerTarget {
                    topology_revision: TopologyRevision::new(9),
                    lair_id: LairId::new(),
                    dojo_id: DojoId::new(),
                    action: NavigationAction::ActivateDojo,
                },
                explorer_target: None,
            })
            .unwrap();
        sender.try_send(WindowTopologyCommand::CloseWindow).unwrap();
        let split = WindowTopologyCommand::Split {
            dojo_id: first,
            target,
            axis: Axis::Horizontal,
            pending: Some(placeholder),
        };
        let (updates, close_window) =
            abandon_queued_commands(Some(split), &mut commands, &mut state);
        assert!(close_window, "the close notice is kept");
        assert!(commands.try_recv().is_err(), "the queue was drained");
        // The split's placeholder is rolled back to the layout before it.
        let WindowTopologyUpdate::Apply {
            dojo_id,
            layout,
            added,
            removed,
            focused,
            ..
        } = &updates[0]
        else {
            panic!("the placeholder rollback comes first");
        };
        assert_eq!(*dojo_id, first);
        assert_eq!(*layout, root_before);
        assert!(added.is_empty());
        assert_eq!(removed, &vec![placeholder]);
        assert_eq!(*focused, Some(target));
        // Each abandoned command yields exactly one failure; none ran.
        let failures = updates
            .iter()
            .filter(|update| {
                matches!(update, WindowTopologyUpdate::TabFailed { message, .. }
                    if message == ABANDONED_FOR_RECONNECT)
            })
            .count();
        assert_eq!(failures, 3, "split, picker request, and Dojo opening");
        assert_eq!(updates.len(), 4);
        assert_eq!(state.tabs.get(first).unwrap().value.root, root_before);
    }

    #[test]
    fn a_tab_switch_needs_no_daemon_and_is_applied() {
        let (mut state, first, second) = two_tabs(SplintId::new());
        let (sender, mut commands) = mpsc::channel(8);
        sender
            .try_send(WindowTopologyCommand::ActivateTab { dojo_id: second })
            .unwrap();
        // A tab that is gone is not switched to.
        sender
            .try_send(WindowTopologyCommand::ActivateTab {
                dojo_id: DojoId::new(),
            })
            .unwrap();
        let (updates, close_window) = abandon_queued_commands(None, &mut commands, &mut state);
        assert!(!close_window);
        assert_eq!(state.tabs.active().map(|tab| tab.dojo_id), Some(second));
        assert!(matches!(
            updates.as_slice(),
            [WindowTopologyUpdate::ActivateTab { dojo_id, explorer_target: None }] if *dojo_id == second
        ));
        // The lost Window's loop applies switches the same way.
        assert!(matches!(
            tab_switch_update(&mut state, first),
            Some(WindowTopologyUpdate::ActivateTab { dojo_id, .. }) if dojo_id == first
        ));
        assert!(tab_switch_update(&mut state, DojoId::new()).is_none());
        assert_eq!(state.tabs.active().map(|tab| tab.dojo_id), Some(first));
    }
}
