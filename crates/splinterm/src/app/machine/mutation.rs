use super::{
    AuthorizationCommand, Axis, CliEnvelopeV2, CliErrorCodeV2, Command, Connection, Context,
    DojoId, HashSet, LairId, LaunchParameters, LayoutNode, MutationIdentityV2, PathBuf, Request,
    Response, Result, SplintId, SplitRatio, SplitSide, TopologyRevision, audit_page_envelope,
    authorization_status_envelope, bail, committed_mutation_envelope, connect_machine,
    created_mutation_envelope, env, finish_machine_envelope, kill_envelope, launch_parameters,
    live_terminal_location, load_default, process_started_envelope, response_protocol_error,
    restore_many_envelope, revoke_envelope, write_machine_read_failure,
};
use super::{DaemonIdentity, explain_machine_result};

pub(super) enum MachineMutation {
    Create {
        name: String,
        cwd: Option<PathBuf>,
        command: Vec<String>,
    },
    Split {
        target_splint_id: SplintId,
        axis: Axis,
        side: SplitSide,
        ratio: SplitRatio,
        expected_incarnation: Option<u64>,
        cwd: Option<PathBuf>,
        command: Vec<String>,
    },
    CloseSplint {
        splint_id: SplintId,
        yes: bool,
    },
    Ratio {
        splint_id: SplintId,
        ratio: SplitRatio,
    },
    NewDojo {
        lair_id: LairId,
        name: Option<String>,
        cwd: Option<PathBuf>,
        command: Vec<String>,
    },
    CloseDojo {
        dojo_id: DojoId,
        yes: bool,
    },
    RenameLair {
        lair_id: LairId,
        name: String,
    },
    RenameDojo {
        dojo_id: DojoId,
        name: String,
    },
    Focus {
        dojo_id: DojoId,
        splint_id: SplintId,
    },
    /// `None` undoes the rename (`--use-app-name`).
    RenameSplint {
        splint_id: SplintId,
        title: Option<String>,
    },
    Relaunch {
        splint_id: SplintId,
        cwd: Option<PathBuf>,
        command: Vec<String>,
    },
    RestoreSplint {
        splint_id: SplintId,
    },
    RestoreDojo {
        dojo_id: DojoId,
    },
    RestoreLair {
        lair_id: LairId,
    },
    Kill {
        splint_id: SplintId,
        yes: bool,
    },
    Revoke {
        grant_id: u64,
        yes: bool,
    },
}

pub(super) fn extract_machine_mutation(
    command: Command,
) -> std::result::Result<MachineMutation, Command> {
    Ok(match command {
        Command::New { name, cwd, command } => MachineMutation::Create { name, cwd, command },
        Command::Split {
            target_splint_id,
            axis,
            side,
            ratio,
            expected_incarnation,
            cwd,
            command,
        } => MachineMutation::Split {
            target_splint_id,
            axis: axis.into(),
            side: side.into(),
            ratio: SplitRatio::new(ratio).unwrap_or_else(|_| unreachable!("Clap bounded ratio")),
            expected_incarnation,
            cwd,
            command,
        },
        Command::Close { splint_id, yes } => MachineMutation::CloseSplint { splint_id, yes },
        Command::Ratio {
            target_splint_id,
            ratio,
        } => MachineMutation::Ratio {
            splint_id: target_splint_id,
            ratio: SplitRatio::new(ratio).unwrap_or_else(|_| unreachable!("Clap bounded ratio")),
        },
        Command::NewDojo {
            lair_id,
            name,
            cwd,
            command,
        } => MachineMutation::NewDojo {
            lair_id,
            name,
            cwd,
            command,
        },
        Command::CloseDojo { dojo_id, yes } => MachineMutation::CloseDojo { dojo_id, yes },
        Command::RenameLair { lair_id, name } => MachineMutation::RenameLair { lair_id, name },
        Command::RenameDojo { dojo_id, name } => MachineMutation::RenameDojo { dojo_id, name },
        Command::DojoFocusHint { dojo_id, splint_id } => {
            MachineMutation::Focus { dojo_id, splint_id }
        }
        Command::RenameSplint {
            splint_id,
            title,
            use_app_name,
        } => MachineMutation::RenameSplint {
            splint_id,
            title: title.filter(|_| !use_app_name),
        },
        Command::Relaunch {
            splint_id,
            cwd,
            command,
        } => MachineMutation::Relaunch {
            splint_id,
            cwd,
            command,
        },
        Command::Restore { splint_id } => MachineMutation::RestoreSplint { splint_id },
        Command::RestoreDojo { dojo_id } => MachineMutation::RestoreDojo { dojo_id },
        Command::RestoreLair { lair_id } => MachineMutation::RestoreLair { lair_id },
        Command::Kill { splint_id, yes } => MachineMutation::Kill { splint_id, yes },
        Command::Authorization {
            command: AuthorizationCommand::Revoke { grant_id, yes },
        } => MachineMutation::Revoke { grant_id, yes },
        other => return Err(other),
    })
}

impl MachineMutation {
    const fn operation(&self) -> &'static str {
        match self {
            Self::Create { .. } => "create_lair",
            Self::Split { .. } => "split_splint",
            Self::CloseSplint { .. } => "close_splint",
            Self::Ratio { .. } => "set_split_ratio",
            Self::NewDojo { .. } => "new_dojo",
            Self::CloseDojo { .. } => "close_dojo",
            Self::RenameLair { .. } => "rename_lair",
            Self::RenameDojo { .. } => "rename_dojo",
            Self::Focus { .. } => "set_dojo_default_focus",
            Self::RenameSplint { .. } => "rename_splint",
            Self::Relaunch { .. } => "relaunch_splint",
            Self::RestoreSplint { .. } => "restore_splint",
            Self::RestoreDojo { .. } => "restore_dojo",
            Self::RestoreLair { .. } => "restore_lair",
            Self::Kill { .. } => "kill_splint",
            Self::Revoke { .. } => "revoke_access",
        }
    }

    const fn confirmation_missing(&self) -> bool {
        matches!(
            self,
            Self::CloseSplint { yes: false, .. }
                | Self::CloseDojo { yes: false, .. }
                | Self::Kill { yes: false, .. }
                | Self::Revoke { yes: false, .. }
        )
    }
}

fn machine_launch(cwd: Option<PathBuf>, command: Vec<String>) -> Result<LaunchParameters> {
    let config = load_default()?.config;
    Ok(launch_parameters(
        match cwd {
            Some(cwd) => cwd,
            None => env::current_dir().context("failed to read current directory")?,
        },
        command,
        &config,
    ))
}

fn topology_splint_location(
    topology: &splinterm_protocol::TopologySnapshot,
    splint_id: SplintId,
) -> Result<(LairId, DojoId)> {
    topology
        .topology
        .lairs()
        .find_map(|lair| {
            lair.dojos
                .iter()
                .find(|dojo| dojo.root.find_splint(splint_id).is_some())
                .map(|dojo| (lair.id, dojo.id))
        })
        .context("requested Splint was not found")
}

pub(in crate::app) fn require_incarnation(actual: u64, expected: Option<u64>) -> Result<()> {
    if expected.is_some_and(|expected| actual != expected) {
        bail!("selected Splint does not match expected incarnation");
    }
    Ok(())
}

pub(in crate::app) fn require_expected_incarnation(
    topology: &splinterm_protocol::TopologySnapshot,
    splint_id: SplintId,
    expected: Option<u64>,
) -> Result<()> {
    let actual = topology
        .runtimes
        .iter()
        .find(|runtime| runtime.splint_id == splint_id)
        .and_then(|runtime| runtime.live_incarnation)
        .context("selected Splint does not have a live process")?;
    require_incarnation(actual, expected)
}

fn topology_dojo_location(
    topology: &splinterm_protocol::TopologySnapshot,
    dojo_id: DojoId,
) -> Result<LairId> {
    topology
        .topology
        .lairs()
        .find(|lair| lair.dojos.iter().any(|dojo| dojo.id == dojo_id))
        .map(|lair| lair.id)
        .context("requested Dojo was not found")
}

fn require_lair(topology: &splinterm_protocol::TopologySnapshot, lair_id: LairId) -> Result<()> {
    if topology.topology.lairs().any(|lair| lair.id == lair_id) {
        Ok(())
    } else {
        bail!("requested Lair was not found")
    }
}

fn machine_new_dojo_request(
    topology: &splinterm_protocol::TopologySnapshot,
    lair_id: LairId,
    name: Option<&str>,
    launch: LaunchParameters,
) -> Result<Request> {
    let lair = topology
        .topology
        .find_lair(lair_id)
        .context("requested Lair was not found")?;
    let generated_name = name.is_none();
    let name = match name {
        Some(name) => name.to_owned(),
        None => lair.next_default_dojo_name()?,
    };
    Ok(Request::NewDojo {
        expected_topology_revision: topology.revision,
        lair_id,
        name,
        launch,
        promote_transient_lair: false,
        generated_name,
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "closed machine mutation request construction stays adjacent for auditability"
)]
fn machine_mutation_request(
    mutation: &MachineMutation,
    topology: &splinterm_protocol::TopologySnapshot,
    inherited_cwd: Option<PathBuf>,
) -> Result<Request> {
    let expected_topology_revision = topology.revision;
    Ok(match mutation {
        MachineMutation::Create { name, cwd, command } => Request::CreateLair {
            expected_topology_revision,
            name: name.clone(),
            launch: machine_launch(cwd.clone(), command.clone())?,
            generated_name: false,
        },
        MachineMutation::Split {
            target_splint_id,
            axis,
            side,
            ratio,
            expected_incarnation,
            cwd,
            command,
        } => {
            topology_splint_location(topology, *target_splint_id)?;
            require_expected_incarnation(topology, *target_splint_id, *expected_incarnation)?;
            Request::SplitSplint {
                expected_topology_revision,
                target_splint_id: *target_splint_id,
                axis: *axis,
                side: *side,
                ratio: *ratio,
                launch: machine_launch(cwd.clone().or(inherited_cwd), command.clone())?,
            }
        }
        MachineMutation::CloseSplint { splint_id, .. } => {
            topology_splint_location(topology, *splint_id)?;
            Request::CloseSplint {
                expected_topology_revision,
                splint_id: *splint_id,
            }
        }
        MachineMutation::Ratio { splint_id, ratio } => {
            topology_splint_location(topology, *splint_id)?;
            Request::SetSplitRatio {
                expected_topology_revision,
                target_splint_id: *splint_id,
                ancestor: 0,
                ratio: *ratio,
            }
        }
        MachineMutation::NewDojo {
            lair_id,
            name,
            cwd,
            command,
        } => machine_new_dojo_request(
            topology,
            *lair_id,
            name.as_deref(),
            machine_launch(cwd.clone(), command.clone())?,
        )?,
        MachineMutation::CloseDojo { dojo_id, .. } => {
            topology_dojo_location(topology, *dojo_id)?;
            Request::CloseDojo {
                expected_topology_revision,
                dojo_id: *dojo_id,
            }
        }
        MachineMutation::RenameLair { lair_id, name } => {
            require_lair(topology, *lair_id)?;
            Request::RenameLair {
                expected_topology_revision,
                lair_id: *lair_id,
                name: name.clone(),
            }
        }
        MachineMutation::RenameDojo { dojo_id, name } => {
            topology_dojo_location(topology, *dojo_id)?;
            Request::RenameDojo {
                expected_topology_revision,
                dojo_id: *dojo_id,
                name: name.clone(),
                promote_transient_lair: false,
            }
        }
        MachineMutation::Focus { dojo_id, splint_id } => {
            let (_, actual_dojo) = topology_splint_location(topology, *splint_id)?;
            if actual_dojo != *dojo_id {
                bail!("selected Splint does not belong to the selected Dojo");
            }
            Request::SetDojoDefaultFocus {
                expected_topology_revision,
                dojo_id: *dojo_id,
                splint_id: *splint_id,
            }
        }
        MachineMutation::RenameSplint { splint_id, title } => {
            topology_splint_location(topology, *splint_id)?;
            match title {
                Some(title) => Request::RenameSplint {
                    expected_topology_revision,
                    splint_id: *splint_id,
                    title: title.clone(),
                },
                None => Request::ResetSplintTitle {
                    expected_topology_revision,
                    splint_id: *splint_id,
                },
            }
        }
        MachineMutation::Relaunch {
            splint_id,
            cwd,
            command,
        } => {
            topology_splint_location(topology, *splint_id)?;
            Request::RelaunchSplint {
                expected_topology_revision,
                splint_id: *splint_id,
                launch: machine_launch(cwd.clone(), command.clone())?,
            }
        }
        MachineMutation::RestoreSplint { splint_id } => {
            topology_splint_location(topology, *splint_id)?;
            Request::RestoreSplint {
                expected_topology_revision,
                splint_id: *splint_id,
            }
        }
        MachineMutation::RestoreDojo { dojo_id } => {
            topology_dojo_location(topology, *dojo_id)?;
            Request::RestoreDojo {
                expected_topology_revision,
                dojo_id: *dojo_id,
            }
        }
        MachineMutation::RestoreLair { lair_id } => {
            require_lair(topology, *lair_id)?;
            Request::RestoreLair {
                expected_topology_revision,
                lair_id: *lair_id,
            }
        }
        MachineMutation::Kill { splint_id, .. } => {
            let (_, _, incarnation) = live_terminal_location(topology, *splint_id)?;
            Request::KillSplint {
                splint_id: *splint_id,
                incarnation,
            }
        }
        MachineMutation::Revoke { grant_id, .. } => Request::RevokeAccess {
            grant_id: *grant_id,
        },
    })
}

fn committed_revision(before: TopologyRevision, committed: TopologyRevision) -> Result<u64> {
    let expected = before
        .get()
        .checked_add(1)
        .context("topology revision exhausted")?;
    if committed.get() != expected {
        bail!("splinterd returned an inconsistent committed topology revision");
    }
    Ok(committed.get())
}

fn topology_identity(
    topology: &splinterm_protocol::TopologySnapshot,
    splint_id: SplintId,
    revision: u64,
    incarnation: Option<u64>,
) -> Result<MutationIdentityV2> {
    let (lair_id, dojo_id) = topology_splint_location(topology, splint_id)?;
    Ok(MutationIdentityV2 {
        lair_id: Some(lair_id),
        dojo_id: Some(dojo_id),
        splint_id: Some(splint_id),
        topology_revision: Some(revision),
        incarnation,
    })
}

fn created_lair_envelope(
    before: &splinterm_protocol::TopologySnapshot,
    lair: &splinterm_core::Lair,
    incarnation: u64,
    topology_revision: TopologyRevision,
) -> Result<CliEnvelopeV2> {
    let revision = committed_revision(before.revision, topology_revision)?;
    if lair.dojos.len() != 1 || incarnation == 0 {
        bail!("splinterd returned inconsistent created Lair topology");
    }
    let dojo = &lair.dojos[0];
    let LayoutNode::Leaf(splint) = &dojo.root else {
        bail!("created Dojo did not contain one Splint leaf");
    };
    if before
        .topology
        .lairs()
        .any(|existing| existing.id == lair.id)
        || before
            .topology
            .lairs()
            .flat_map(|existing| &existing.dojos)
            .any(|existing| existing.id == dojo.id)
        || before.topology.find_splint(splint.id).is_some()
    {
        bail!("create response reused an existing stable identity");
    }
    created_mutation_envelope(
        "create_lair",
        MutationIdentityV2 {
            lair_id: Some(lair.id),
            dojo_id: Some(dojo.id),
            splint_id: Some(splint.id),
            topology_revision: Some(revision),
            incarnation: Some(incarnation),
        },
    )
}

fn topology_commit_envelope(
    mutation: &MachineMutation,
    topology: &splinterm_protocol::TopologySnapshot,
    revision: TopologyRevision,
) -> Result<CliEnvelopeV2> {
    let revision = committed_revision(topology.revision, revision)?;
    let (identity, confirmed) = match mutation {
        MachineMutation::CloseSplint { splint_id, .. }
        | MachineMutation::Ratio { splint_id, .. }
        | MachineMutation::RenameSplint { splint_id, .. } => (
            topology_identity(topology, *splint_id, revision, None)?,
            matches!(mutation, MachineMutation::CloseSplint { .. }),
        ),
        MachineMutation::Focus { dojo_id, splint_id } => {
            let identity = topology_identity(topology, *splint_id, revision, None)?;
            if identity.dojo_id != Some(*dojo_id) {
                bail!("committed focus hint identity is inconsistent");
            }
            (identity, false)
        }
        MachineMutation::CloseDojo { dojo_id, .. }
        | MachineMutation::RenameDojo { dojo_id, .. } => (
            MutationIdentityV2 {
                lair_id: Some(topology_dojo_location(topology, *dojo_id)?),
                dojo_id: Some(*dojo_id),
                splint_id: None,
                topology_revision: Some(revision),
                incarnation: None,
            },
            matches!(mutation, MachineMutation::CloseDojo { .. }),
        ),
        MachineMutation::RenameLair { lair_id, .. } => (
            MutationIdentityV2 {
                lair_id: Some(*lair_id),
                dojo_id: None,
                splint_id: None,
                topology_revision: Some(revision),
                incarnation: None,
            },
            false,
        ),
        _ => bail!("topology commit response does not match mutation"),
    };
    committed_mutation_envelope(mutation.operation(), identity, confirmed)
}

fn layout_ids(node: &LayoutNode, ids: &mut Vec<SplintId>) {
    match node {
        LayoutNode::Leaf(splint) => ids.push(splint.id),
        LayoutNode::Branch { first, second, .. } => {
            layout_ids(first, ids);
            layout_ids(second, ids);
        }
    }
}

fn validate_restore_results(
    topology: &splinterm_protocol::TopologySnapshot,
    mutation: &MachineMutation,
    topology_revision: TopologyRevision,
    results: &[splinterm_protocol::RestoreLeafResult],
) -> Result<()> {
    if topology_revision < topology.revision {
        bail!("restore response regressed topology revision");
    }
    let mut expected = Vec::new();
    match mutation {
        MachineMutation::RestoreDojo { dojo_id } => {
            let dojo = topology
                .topology
                .lairs()
                .flat_map(|dojo| &dojo.dojos)
                .find(|dojo| dojo.id == *dojo_id)
                .context("restore Dojo disappeared from reviewed topology")?;
            layout_ids(&dojo.root, &mut expected);
        }
        MachineMutation::RestoreLair { lair_id } => {
            let dojo = topology
                .topology
                .lairs()
                .find(|dojo| dojo.id == *lair_id)
                .context("restore Dojo disappeared from reviewed topology")?;
            for dojo in &dojo.dojos {
                layout_ids(&dojo.root, &mut expected);
            }
        }
        _ => bail!("restore result validation used for non-aggregate mutation"),
    }
    let expected = expected.into_iter().collect::<HashSet<_>>();
    let actual = results
        .iter()
        .map(|result| result.splint_id)
        .collect::<HashSet<_>>();
    if actual.len() != results.len() || actual != expected {
        bail!("restore response does not exactly cover the selected Splints");
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "closed mutation response identity correlations stay adjacent for auditability"
)]
fn mutation_response_envelope(
    mutation: &MachineMutation,
    topology: &splinterm_protocol::TopologySnapshot,
    response: Response,
) -> Result<CliEnvelopeV2> {
    match (mutation, response) {
        (
            MachineMutation::Split {
                target_splint_id, ..
            },
            Response::SplintStarted {
                splint_id,
                incarnation,
                topology_revision,
            },
        ) => {
            let revision = committed_revision(topology.revision, topology_revision)?;
            if topology.topology.find_splint(splint_id).is_some() {
                bail!("split response reused an existing Splint identity");
            }
            let (lair_id, dojo_id) = topology_splint_location(topology, *target_splint_id)?;
            created_mutation_envelope(
                "split_splint",
                MutationIdentityV2 {
                    lair_id: Some(lair_id),
                    dojo_id: Some(dojo_id),
                    splint_id: Some(splint_id),
                    topology_revision: Some(revision),
                    incarnation: Some(incarnation),
                },
            )
        }
        (
            MachineMutation::NewDojo { lair_id, .. },
            Response::DojoStarted {
                dojo_id,
                splint_id,
                incarnation,
                topology_revision,
            },
        ) => {
            if topology.topology.find_splint(splint_id).is_some()
                || topology
                    .topology
                    .lairs()
                    .flat_map(|dojo| &dojo.dojos)
                    .any(|dojo| dojo.id == dojo_id)
            {
                bail!("new-Dojo response reused an existing stable identity");
            }
            created_mutation_envelope(
                "new_dojo",
                MutationIdentityV2 {
                    lair_id: Some(*lair_id),
                    dojo_id: Some(dojo_id),
                    splint_id: Some(splint_id),
                    topology_revision: Some(committed_revision(
                        topology.revision,
                        topology_revision,
                    )?),
                    incarnation: Some(incarnation),
                },
            )
        }
        (
            MachineMutation::Relaunch { splint_id, .. },
            Response::SplintStarted {
                splint_id: response_id,
                incarnation,
                topology_revision,
            },
        ) if *splint_id == response_id => process_started_envelope(
            mutation.operation(),
            topology_identity(
                topology,
                *splint_id,
                committed_revision(topology.revision, topology_revision)?,
                Some(incarnation),
            )?,
        ),
        (
            MachineMutation::RestoreSplint { splint_id },
            Response::RestoreCompleted {
                topology_revision,
                mut results,
            },
        ) if results.len() == 1 && results[0].splint_id == *splint_id => {
            let result = results.pop().expect("one checked restore result");
            if let Some(error) = result.error {
                return Err(response_protocol_error(error));
            }
            let incarnation = result
                .incarnation
                .context("successful restore omitted process incarnation")?;
            process_started_envelope(
                "restore_splint",
                topology_identity(
                    topology,
                    *splint_id,
                    committed_revision(topology.revision, topology_revision)?,
                    Some(incarnation),
                )?,
            )
        }
        (
            MachineMutation::RestoreDojo { dojo_id },
            Response::RestoreCompleted {
                topology_revision,
                results,
            },
        ) => {
            validate_restore_results(topology, mutation, topology_revision, &results)?;
            restore_many_envelope(
                "restore_dojo",
                MutationIdentityV2 {
                    lair_id: Some(topology_dojo_location(topology, *dojo_id)?),
                    dojo_id: Some(*dojo_id),
                    splint_id: None,
                    topology_revision: Some(topology_revision.get()),
                    incarnation: None,
                },
                &results,
            )
        }
        (
            MachineMutation::RestoreLair { lair_id },
            Response::RestoreCompleted {
                topology_revision,
                results,
            },
        ) => {
            validate_restore_results(topology, mutation, topology_revision, &results)?;
            restore_many_envelope(
                "restore_lair",
                MutationIdentityV2 {
                    lair_id: Some(*lair_id),
                    dojo_id: None,
                    splint_id: None,
                    topology_revision: Some(topology_revision.get()),
                    incarnation: None,
                },
                &results,
            )
        }
        (
            MachineMutation::Kill { splint_id, .. },
            Response::SplintKilled {
                splint_id: response_id,
                incarnation,
                ..
            },
        ) if *splint_id == response_id => {
            let (lair_id, dojo_id, expected_incarnation) =
                live_terminal_location(topology, *splint_id)?;
            if incarnation != expected_incarnation {
                bail!("splinterd returned an inconsistent killed incarnation");
            }
            kill_envelope(lair_id, dojo_id, *splint_id, incarnation)
        }
        (mutation, Response::TopologyCommitted { topology_revision }) => {
            topology_commit_envelope(mutation, topology, topology_revision)
        }
        _ => bail!("splinterd returned a mutation response with inconsistent identity"),
    }
}

async fn machine_mutation_envelope(
    connection: &mut Connection,
    mutation: &MachineMutation,
    deadline: std::time::Duration,
    started: std::time::Instant,
) -> Result<CliEnvelopeV2> {
    if let MachineMutation::Revoke { grant_id, .. } = mutation {
        let response = connection
            .request_with_deadline(
                Request::RevokeAccess {
                    grant_id: *grant_id,
                },
                deadline.saturating_sub(started.elapsed()),
            )
            .await?;
        let Response::AccessRevoked {
            lair_id,
            dojo_id,
            grant,
            ..
        } = response
        else {
            bail!("splinterd returned an inconsistent revoke response");
        };
        if grant.grant_id != *grant_id {
            bail!("splinterd returned an inconsistent revoked grant");
        }
        return revoke_envelope(lair_id, dojo_id, &grant);
    }

    let response = connection
        .request_with_deadline(
            Request::InspectTopology,
            deadline.saturating_sub(started.elapsed()),
        )
        .await?;
    let Response::Topology { snapshot: topology } = response else {
        bail!("splinterd returned an unexpected topology response");
    };
    topology
        .validate()
        .map_err(|error| anyhow::anyhow!(error.message))?;
    let inherited_cwd = if let MachineMutation::Split {
        target_splint_id,
        cwd: None,
        ..
    } = mutation
    {
        let response = connection
            .request_with_deadline(
                Request::InspectSplint {
                    splint_id: *target_splint_id,
                },
                deadline.saturating_sub(started.elapsed()),
            )
            .await?;
        let Response::Splint {
            runtime,
            resolved_cwd,
            ..
        } = response
        else {
            bail!("splinterd returned an unexpected Splint response");
        };
        anyhow::ensure!(
            runtime.splint_id == *target_splint_id,
            "splinterd returned another Splint"
        );
        Some(resolved_cwd.context("source working directory is unavailable")?)
    } else {
        None
    };
    let request = machine_mutation_request(mutation, &topology, inherited_cwd)?;
    let response = connection
        .request_with_deadline(request, deadline.saturating_sub(started.elapsed()))
        .await?;
    if matches!(mutation, MachineMutation::Create { .. }) {
        let Response::LairCreated {
            lair: dojo,
            incarnation,
            topology_revision,
        } = response
        else {
            bail!("splinterd returned an inconsistent create response");
        };
        return created_lair_envelope(&topology, &dojo, incarnation, topology_revision);
    }
    mutation_response_envelope(mutation, &topology, response)
}

pub(super) async fn run_machine_mutation(
    mutation: MachineMutation,
    schema_major: u16,
    timeout_ms: u64,
) -> Result<()> {
    let operation = mutation.operation();
    if schema_major != 2 {
        write_machine_read_failure(
            operation,
            CliErrorCodeV2::UnsupportedSchema,
            format!("unsupported schema major {schema_major}"),
            false,
        )?;
        bail!("unsupported schema major {schema_major}");
    }
    if mutation.confirmation_missing() {
        write_machine_read_failure(
            operation,
            CliErrorCodeV2::ConfirmationRequired,
            "destructive machine command requires --yes",
            false,
        )?;
        bail!("destructive machine command requires --yes");
    }
    let deadline = std::time::Duration::from_millis(timeout_ms);
    let (mut connection, started) = connect_machine(operation, deadline).await?;
    let before = DaemonIdentity::of(&connection);
    let result = machine_mutation_envelope(&mut connection, &mutation, deadline, started).await;
    let result = explain_machine_result(result, before, false, deadline, started).await;
    finish_machine_envelope(operation, result)
}

pub(super) async fn run_machine_authorization_status(
    splint_id: SplintId,
    schema_major: u16,
    timeout_ms: u64,
) -> Result<()> {
    const OPERATION: &str = "authorization_status";
    if schema_major != 2 {
        write_machine_read_failure(
            OPERATION,
            CliErrorCodeV2::UnsupportedSchema,
            format!("unsupported schema major {schema_major}"),
            false,
        )?;
        bail!("unsupported schema major {schema_major}");
    }
    let deadline = std::time::Duration::from_millis(timeout_ms);
    let (mut connection, started) = connect_machine(OPERATION, deadline).await?;
    let before = DaemonIdentity::of(&connection);
    let result = async {
        let response = connection
            .request_with_deadline(
                Request::AuthorizationStatus {
                    splint_id,
                    incarnation: None,
                },
                deadline.saturating_sub(started.elapsed()),
            )
            .await?;
        let Response::AuthorizationStatus {
            lair_id,
            dojo_id,
            incarnation,
            grants,
            persistent,
            development_bypass,
            ..
        } = response
        else {
            bail!("splinterd returned an unexpected authorization response");
        };
        authorization_status_envelope(
            lair_id,
            dojo_id,
            splint_id,
            incarnation,
            &grants,
            &persistent,
            development_bypass,
        )
    }
    .await;
    let result = explain_machine_result(result, before, true, deadline, started).await;
    finish_machine_envelope(OPERATION, result)
}

pub(super) async fn run_machine_audit(
    after_audit_id: Option<u64>,
    max_records: usize,
    schema_major: u16,
    timeout_ms: u64,
) -> Result<()> {
    const OPERATION: &str = "audit_inspect";
    if schema_major != 2 {
        write_machine_read_failure(
            OPERATION,
            CliErrorCodeV2::UnsupportedSchema,
            format!("unsupported schema major {schema_major}"),
            false,
        )?;
        bail!("unsupported schema major {schema_major}");
    }
    let deadline = std::time::Duration::from_millis(timeout_ms);
    let (mut connection, started) = connect_machine(OPERATION, deadline).await?;
    let before = DaemonIdentity::of(&connection);
    let result = async {
        let response = connection
            .request_with_deadline(
                Request::AuditInspect {
                    after_audit_id,
                    max_records,
                },
                deadline.saturating_sub(started.elapsed()),
            )
            .await?;
        let Response::AuditPage { page } = response else {
            bail!("splinterd returned an unexpected audit response");
        };
        audit_page_envelope(&page)
    }
    .await;
    let result = explain_machine_result(result, before, true, deadline, started).await;
    finish_machine_envelope(OPERATION, result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use splinterm_core::{Dojo, Topology};

    fn launch() -> LaunchParameters {
        LaunchParameters {
            cwd: PathBuf::from("/tmp"),
            command: vec!["true".to_owned()],
            shell: None,
            login_shell: false,
            scrollback_lines: 1_000,
        }
    }

    #[test]
    fn machine_new_dojo_requests_resolve_defaults_and_preserve_explicit_names_and_revision() {
        let mut model = Topology::new();
        let lair_id = model.create_lair("main", PathBuf::from("/tmp")).unwrap().id;
        model
            .new_dojo_at(
                model.revision(),
                lair_id,
                Dojo::with_shell("Dojo 3", PathBuf::from("/tmp")),
            )
            .unwrap();
        let revision = model.revision();
        let snapshot = splinterm_protocol::TopologySnapshot {
            revision,
            topology: model,
            runtimes: Vec::new(),
            presentation_offers: Vec::new(),
        };

        let implicit = machine_new_dojo_request(&snapshot, lair_id, None, launch()).unwrap();
        assert!(matches!(
            implicit,
            Request::NewDojo {
                expected_topology_revision,
                lair_id: requested_lair,
                name,
                ..
            } if expected_topology_revision == revision
                && requested_lair == lair_id
                && name == "Dojo 4"
        ));

        let explicit =
            machine_new_dojo_request(&snapshot, lair_id, Some("logs"), launch()).unwrap();
        assert!(matches!(
            explicit,
            Request::NewDojo {
                expected_topology_revision,
                lair_id: requested_lair,
                name,
                ..
            } if expected_topology_revision == revision
                && requested_lair == lair_id
                && name == "logs"
        ));
    }
}
