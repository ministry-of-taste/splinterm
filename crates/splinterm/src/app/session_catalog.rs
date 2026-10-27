//! Neutral launch requests and session-catalog projections.

use std::path::PathBuf;

use anyhow::{Context, Result};
use splinterm::{
    SessionPickerItem,
    config::AppConfig,
    endpoint::{ConnectionFactory, LaunchSemantics},
    session_picker::{RecentDojos, SessionEntry},
};
use splinterm_core::{DojoId, LairId, TopologyRevision};
use splinterm_protocol::{AutomationLaunch, LaunchDirectory, LaunchParameters, Request};

pub(in crate::app) async fn source_splint_cwd(
    connection: &mut splinterm::automation::Connection,
    source_splint_id: splinterm_core::SplintId,
) -> Result<PathBuf> {
    let splinterm_protocol::Response::Splint {
        runtime,
        resolved_cwd,
        ..
    } = connection
        .request(Request::InspectSplint {
            splint_id: source_splint_id,
        })
        .await?
    else {
        anyhow::bail!("splinterd returned an unexpected Splint response");
    };
    anyhow::ensure!(
        runtime.splint_id == source_splint_id,
        "splinterd returned another Splint"
    );
    resolved_cwd.context("source working directory is unavailable")
}

/// Resolves where a Window-created Splint, Dojo, or Lair starts, on the daemon
/// that owns `source_splint_id`. Inheritance keeps the exact source-cwd path.
pub(in crate::app) async fn creation_directory(
    connection: &mut splinterm::automation::Connection,
    source_splint_id: splinterm_core::SplintId,
    directory: &LaunchDirectory,
) -> Result<PathBuf> {
    if *directory == LaunchDirectory::Inherit {
        return source_splint_cwd(connection, source_splint_id).await;
    }
    let splinterm_protocol::Response::LaunchDirectoryResolved {
        source_splint_id: resolved_source,
        cwd,
    } = connection
        .request(Request::ResolveLaunchDirectory {
            source_splint_id,
            directory: directory.clone(),
        })
        .await?
    else {
        anyhow::bail!("splinterd returned an unexpected launch-directory response");
    };
    anyhow::ensure!(
        resolved_source == source_splint_id,
        "splinterd resolved another Splint's launch directory"
    );
    cwd.context("configured launch directory and home directory are unavailable")
}

pub(in crate::app) fn launch_parameters(
    cwd: PathBuf,
    command: Vec<String>,
    config: &AppConfig,
) -> LaunchParameters {
    LaunchParameters {
        cwd,
        command,
        shell: config.shell.clone(),
        login_shell: config.login_shell,
        scrollback_lines: config.scrollback_lines,
    }
}

pub(in crate::app) fn automation_launch(
    cwd: Option<PathBuf>,
    argv: Vec<String>,
) -> AutomationLaunch {
    AutomationLaunch { cwd, argv }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the shared local/remote request boundary keeps every launch field explicit"
)]
pub(in crate::app) fn new_dojo_request_for(
    semantics: LaunchSemantics,
    expected_topology_revision: TopologyRevision,
    lairs: &[splinterm_core::Lair],
    lair_id: LairId,
    name: Option<String>,
    cwd: Option<PathBuf>,
    command: Vec<String>,
    promote_transient_lair: bool,
    config: &AppConfig,
) -> Result<Request> {
    let generated_name = name.is_none();
    let name = match name {
        Some(name) => name,
        None => next_default_dojo_name(lairs, lair_id)?,
    };
    Ok(match semantics {
        LaunchSemantics::LocalTrusted => Request::NewDojo {
            expected_topology_revision,
            lair_id,
            name,
            launch: launch_parameters(
                cwd.context("local Dojo working directory is unavailable")?,
                command,
                config,
            ),
            promote_transient_lair,
            generated_name,
        },
        LaunchSemantics::RemoteInteractive => Request::NewDojoAutomation {
            expected_topology_revision,
            lair_id,
            name,
            launch: automation_launch(cwd, command),
            generated_name,
        },
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::app) enum GraphicalLairLifetime {
    Persistent,
    ClientBound,
}

#[must_use]
pub(in crate::app) fn graphical_lair_lifetime(
    semantics: LaunchSemantics,
    explicit_name: bool,
    command: &[String],
    config: &AppConfig,
) -> GraphicalLairLifetime {
    if semantics == LaunchSemantics::LocalTrusted
        && !explicit_name
        && command.is_empty()
        && !config.multiplexer_lifetime.persistent_by_default
    {
        GraphicalLairLifetime::ClientBound
    } else {
        GraphicalLairLifetime::Persistent
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the graphical lifetime boundary keeps naming and launch intent explicit"
)]
pub(in crate::app) fn graphical_create_request(
    factory: &ConnectionFactory,
    expected_topology_revision: TopologyRevision,
    name: String,
    explicit_name: bool,
    cwd: Option<PathBuf>,
    command: Vec<String>,
    config: &AppConfig,
) -> Result<(Request, GraphicalLairLifetime)> {
    let semantics = factory.capabilities().launch_semantics;
    let lifetime = graphical_lair_lifetime(semantics, explicit_name, &command, config);
    let request = match lifetime {
        GraphicalLairLifetime::Persistent => create_request_for(
            semantics,
            expected_topology_revision,
            name,
            !explicit_name,
            cwd,
            command,
            config,
        )?,
        GraphicalLairLifetime::ClientBound => Request::CreateTransientLair {
            expected_topology_revision,
            name,
            launch: launch_parameters(
                cwd.context("local launch working directory is unavailable")?,
                command,
                config,
            ),
            generated_name: !explicit_name,
        },
    };
    Ok((request, lifetime))
}

/// A request for a Lair the user named, as the `new` command does.
pub(in crate::app) fn create_request(
    factory: &ConnectionFactory,
    expected_topology_revision: TopologyRevision,
    name: String,
    cwd: Option<PathBuf>,
    command: Vec<String>,
    config: &AppConfig,
) -> Result<Request> {
    create_request_for(
        factory.capabilities().launch_semantics,
        expected_topology_revision,
        name,
        false,
        cwd,
        command,
        config,
    )
}

pub(in crate::app) fn create_request_for(
    semantics: LaunchSemantics,
    expected_topology_revision: TopologyRevision,
    name: String,
    generated_name: bool,
    cwd: Option<PathBuf>,
    command: Vec<String>,
    config: &AppConfig,
) -> Result<Request> {
    Ok(match semantics {
        LaunchSemantics::LocalTrusted => Request::CreateLair {
            expected_topology_revision,
            name,
            launch: launch_parameters(
                cwd.context("local launch working directory is unavailable")?,
                command,
                config,
            ),
            generated_name,
        },
        LaunchSemantics::RemoteInteractive => Request::CreateLairAutomation {
            expected_topology_revision,
            name,
            launch: automation_launch(cwd, command),
            generated_name,
        },
    })
}

pub(in crate::app) fn next_default_dojo_name(
    lairs: &[splinterm_core::Lair],
    lair_id: LairId,
) -> Result<String> {
    lairs
        .iter()
        .find(|lair| lair.id == lair_id)
        .context("selected Lair is not present in the current topology")?
        .next_default_dojo_name()
        .map_err(Into::into)
}

pub(in crate::app) fn select_dojo_from(
    lairs: &[splinterm_core::Lair],
    selection: (LairId, DojoId),
) -> Result<splinterm_core::Dojo> {
    let (lair_id, dojo_id) = selection;
    let lair = lairs
        .iter()
        .find(|lair| lair.id == lair_id)
        .context("selected Lair is not present in the current topology")?;
    let dojo = lair
        .dojos
        .iter()
        .find(|dojo| dojo.id == dojo_id)
        .context("selected Dojo does not belong to the selected Dojo")?;
    dojo.root
        .find_splint(dojo.default_focus)
        .context("selected Dojo has an invalid default-focus hint")?;
    Ok(dojo.clone())
}

pub(in crate::app) fn recent_dojo_ids(factory: &ConnectionFactory) -> Vec<DojoId> {
    RecentDojos::discover_namespace(&factory.capabilities().recency_namespace).map_or_else(
        |error| {
            eprintln!("splinterm recent Dojos unavailable: {error:#}");
            Vec::new()
        },
        |store| store.load(),
    )
}

pub(in crate::app) fn remember_dojo(factory: &ConnectionFactory, dojo_id: DojoId) {
    match RecentDojos::discover_namespace(&factory.capabilities().recency_namespace)
        .and_then(|store| store.record(dojo_id))
    {
        Ok(()) => {}
        Err(error) => eprintln!("splinterm could not update recent Dojos: {error:#}"),
    }
}

/// A Recent Dojos row, as the inline picker shows it: the Dojo's name, and
/// the `Lair / Dojo` breadcrumb that gives its Lair as the location. A Dojo
/// named as its Lair is one level, its Lair's label alone.
pub(in crate::app) fn session_picker_item(entry: &SessionEntry) -> SessionPickerItem {
    let breadcrumb = entry.display_title();
    SessionPickerItem {
        display_title: if entry.lair_name == entry.dojo_name {
            breadcrumb.clone()
        } else {
            entry.dojo_display_title()
        },
        breadcrumb,
        working_directory: entry.working_directory(),
        pane_count: entry.pane_count,
        running_pane_count: entry.running_panes,
        agent: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use splinterm_core::Dojo;

    /// The standalone `splinterm dojos` row has the inline row's columns:
    /// the Dojo's name, then its Lair and directory, then the status (the
    /// row itself is pinned in the picker's tests).
    #[test]
    fn recent_dojo_rows_name_the_dojo_and_place_it_in_its_lair() {
        let entry = SessionEntry {
            lair_id: splinterm_core::LairId::new(),
            dojo_id: splinterm_core::DojoId::new(),
            lair_name: "lair-1".to_owned(),
            dojo_name: "dojo-1".to_owned(),
            lair_label: "Lair 1".to_owned(),
            dojo_label: "Dojo 1".to_owned(),
            cwd: PathBuf::from("/home/user/Projects/splinterm"),
            pane_count: 1,
            running_panes: 1,
            exited_panes: 0,
        };
        let item = session_picker_item(&entry);
        assert_eq!(item.display_title, "Dojo 1");
        assert_eq!(item.breadcrumb, "Lair 1 / Dojo 1");
        assert_eq!(item.working_directory, "/home/user/Projects/splinterm");
        let one_level = SessionEntry {
            dojo_name: "lair-1".to_owned(),
            ..entry
        };
        let item = session_picker_item(&one_level);
        assert_eq!(item.display_title, "Lair 1");
        assert_eq!(item.breadcrumb, "Lair 1");
    }

    #[test]
    fn graphical_lifetime_selection_is_narrow_and_backward_compatible() {
        let defaults = AppConfig::default();
        assert_eq!(
            graphical_lair_lifetime(LaunchSemantics::LocalTrusted, false, &[], &defaults),
            GraphicalLairLifetime::Persistent
        );

        let mut transient_defaults = AppConfig::default();
        transient_defaults
            .multiplexer_lifetime
            .persistent_by_default = false;
        assert_eq!(
            graphical_lair_lifetime(
                LaunchSemantics::LocalTrusted,
                false,
                &[],
                &transient_defaults
            ),
            GraphicalLairLifetime::ClientBound
        );
        assert_eq!(
            graphical_lair_lifetime(
                LaunchSemantics::LocalTrusted,
                true,
                &[],
                &transient_defaults
            ),
            GraphicalLairLifetime::Persistent
        );
        assert_eq!(
            graphical_lair_lifetime(
                LaunchSemantics::LocalTrusted,
                false,
                &["command".to_owned()],
                &transient_defaults
            ),
            GraphicalLairLifetime::Persistent
        );
        assert_eq!(
            graphical_lair_lifetime(
                LaunchSemantics::RemoteInteractive,
                false,
                &[],
                &transient_defaults
            ),
            GraphicalLairLifetime::Persistent
        );
    }

    /// Inheritance, home, then a wrong-Splint answer and an unavailable answer.
    fn launch_directory_script(
        source: splinterm_core::SplintId,
        other: splinterm_core::SplintId,
        relative: &LaunchDirectory,
    ) -> Vec<(Request, splinterm_protocol::Response)> {
        use splinterm_protocol::{Response, SplintLifecycle, SplintRuntimeSummary};

        let resolve = |directory: &LaunchDirectory| Request::ResolveLaunchDirectory {
            source_splint_id: source,
            directory: directory.clone(),
        };
        let resolved = |splint, cwd: Option<&str>| Response::LaunchDirectoryResolved {
            source_splint_id: splint,
            cwd: cwd.map(PathBuf::from),
        };
        vec![
            (
                Request::InspectSplint { splint_id: source },
                Response::Splint {
                    lair_id: LairId::new(),
                    dojo_id: DojoId::new(),
                    title: String::new(),
                    topology_revision: TopologyRevision::default(),
                    resolved_cwd: Some(PathBuf::from("/remote/owner/live")),
                    runtime: SplintRuntimeSummary {
                        splint_id: source,
                        live_incarnation: None,
                        last_incarnation: None,
                        restorable: false,
                        lifecycle: SplintLifecycle::Exited,
                        exit_status: None,
                        agent_status: None,
                    },
                },
            ),
            // Home is the remote owner's home, never the client's.
            (
                resolve(&LaunchDirectory::Home),
                resolved(source, Some("/remote/owner")),
            ),
            (resolve(relative), resolved(other, Some("/remote/other"))),
            (resolve(relative), resolved(source, None)),
        ]
    }

    #[tokio::test]
    async fn creation_directory_resolves_configured_choices_on_the_owning_daemon() {
        use splinterm::automation::Connection;
        use splinterm_core::SplintId;
        use splinterm_protocol::{
            ClientFrame, PROTOCOL_VERSION, ServerFrame, ServerLimits, encode_frame,
        };
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        async fn read_request(stream: &mut tokio::io::DuplexStream) -> ClientFrame {
            let length = stream.read_u32().await.unwrap();
            let mut body = vec![0; length as usize];
            stream.read_exact(&mut body).await.unwrap();
            serde_json::from_slice(&body).unwrap()
        }
        let source = SplintId::new();
        let other = SplintId::new();
        let relative = LaunchDirectory::HomeRelative(PathBuf::from("projects"));
        let script = launch_directory_script(source, other, &relative);
        let (client, mut server) = tokio::io::duplex(8192);
        let (reader, writer) = tokio::io::split(client);
        let task = tokio::spawn(async move {
            assert!(matches!(
                read_request(&mut server).await,
                ClientFrame::Hello { .. }
            ));
            server
                .write_all(
                    &encode_frame(&ServerFrame::Hello {
                        version: PROTOCOL_VERSION,
                        limits: ServerLimits::default(),
                        development_terminal_access: false,
                        daemon_hostname: None,
                        daemon_generation: 1,
                    })
                    .unwrap(),
                )
                .await
                .unwrap();
            for (expected, result) in script {
                let ClientFrame::Request {
                    request_id,
                    request,
                    ..
                } = read_request(&mut server).await
                else {
                    panic!("expected a launch-directory request")
                };
                assert_eq!(request, expected);
                server
                    .write_all(
                        &encode_frame(&ServerFrame::Response { request_id, result }).unwrap(),
                    )
                    .await
                    .unwrap();
            }
        });
        let mut connection = Connection::connect_remote_interactive_transport(reader, writer)
            .await
            .unwrap();
        assert_eq!(
            creation_directory(&mut connection, source, &LaunchDirectory::Inherit)
                .await
                .unwrap(),
            PathBuf::from("/remote/owner/live")
        );
        assert_eq!(
            creation_directory(&mut connection, source, &LaunchDirectory::Home)
                .await
                .unwrap(),
            PathBuf::from("/remote/owner")
        );
        assert!(
            creation_directory(&mut connection, source, &relative)
                .await
                .unwrap_err()
                .to_string()
                .contains("another Splint")
        );
        assert!(
            creation_directory(&mut connection, source, &relative)
                .await
                .unwrap_err()
                .to_string()
                .contains("unavailable")
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn source_splint_cwd_requests_exact_identity_and_rejects_unavailable_responses() {
        use splinterm::automation::Connection;
        use splinterm_core::SplintId;
        use splinterm_protocol::{
            ClientFrame, PROTOCOL_VERSION, Response, ServerFrame, ServerLimits, SplintLifecycle,
            SplintRuntimeSummary, encode_frame,
        };
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        async fn read_request(stream: &mut tokio::io::DuplexStream) -> ClientFrame {
            let length = stream.read_u32().await.unwrap();
            let mut body = vec![0; length as usize];
            stream.read_exact(&mut body).await.unwrap();
            serde_json::from_slice(&body).unwrap()
        }
        let source = SplintId::new();
        let other = SplintId::new();
        let (client, mut server) = tokio::io::duplex(8192);
        let (reader, writer) = tokio::io::split(client);
        let task = tokio::spawn(async move {
            assert!(matches!(
                read_request(&mut server).await,
                ClientFrame::Hello { .. }
            ));
            server
                .write_all(
                    &encode_frame(&ServerFrame::Hello {
                        version: PROTOCOL_VERSION,
                        limits: ServerLimits::default(),
                        development_terminal_access: false,
                        daemon_hostname: None,
                        daemon_generation: 1,
                    })
                    .unwrap(),
                )
                .await
                .unwrap();
            for (returned_id, resolved_cwd) in [
                (source, Some(PathBuf::from("/owner/live"))),
                (other, Some(PathBuf::from("/owner/other"))),
                (source, None),
            ] {
                let ClientFrame::Request {
                    request_id,
                    request,
                    ..
                } = read_request(&mut server).await
                else {
                    panic!("expected inspection request")
                };
                assert_eq!(request, Request::InspectSplint { splint_id: source });
                server
                    .write_all(
                        &encode_frame(&ServerFrame::Response {
                            request_id,
                            result: Response::Splint {
                                lair_id: LairId::new(),
                                dojo_id: DojoId::new(),
                                title: String::new(),
                                topology_revision: TopologyRevision::default(),
                                resolved_cwd,
                                runtime: SplintRuntimeSummary {
                                    splint_id: returned_id,
                                    live_incarnation: None,
                                    last_incarnation: None,
                                    restorable: false,
                                    lifecycle: SplintLifecycle::Exited,
                                    exit_status: None,
                                    agent_status: None,
                                },
                            },
                        })
                        .unwrap(),
                    )
                    .await
                    .unwrap();
            }
        });
        let mut connection = Connection::connect_remote_interactive_transport(reader, writer)
            .await
            .unwrap();
        assert_eq!(
            source_splint_cwd(&mut connection, source).await.unwrap(),
            PathBuf::from("/owner/live")
        );
        assert!(
            source_splint_cwd(&mut connection, source)
                .await
                .unwrap_err()
                .to_string()
                .contains("another Splint")
        );
        assert!(
            source_splint_cwd(&mut connection, source)
                .await
                .unwrap_err()
                .to_string()
                .contains("unavailable")
        );
        task.await.unwrap();
    }

    #[test]
    fn inherited_cwd_preserves_local_launch_configuration_and_remote_argv() {
        let cwd = PathBuf::from("/owner/live-project");
        let config = AppConfig {
            shell: Some("/bin/custom-shell".into()),
            login_shell: true,
            scrollback_lines: 1234,
            ..AppConfig::default()
        };
        let launch = launch_parameters(cwd.clone(), vec!["command".into()], &config);
        assert_eq!(launch.cwd, cwd);
        assert_eq!(launch.shell, config.shell);
        assert!(launch.login_shell);
        assert_eq!(launch.scrollback_lines, 1234);
        assert_eq!(launch.command, ["command"]);
        let remote = automation_launch(Some(cwd.clone()), vec!["remote-command".into()]);
        assert_eq!(remote.cwd, Some(cwd));
        assert_eq!(remote.argv, ["remote-command"]);
    }

    #[test]
    fn graphical_request_selection_preserves_explicit_creation_contracts() {
        let mut transient_defaults = AppConfig::default();
        transient_defaults
            .multiplexer_lifetime
            .persistent_by_default = false;
        let revision = TopologyRevision::new(17);
        let (ordinary, lifetime) = graphical_create_request(
            &ConnectionFactory::local(),
            revision,
            "generated".to_owned(),
            false,
            Some(PathBuf::from("/tmp")),
            Vec::new(),
            &transient_defaults,
        )
        .unwrap();
        assert_eq!(lifetime, GraphicalLairLifetime::ClientBound);
        assert!(matches!(
            ordinary,
            Request::CreateTransientLair {
                expected_topology_revision,
                name,
                launch,
                generated_name,
            } if expected_topology_revision == revision
                && name == "generated"
                && launch.command.is_empty()
                && generated_name
        ));

        let (named, lifetime) = graphical_create_request(
            &ConnectionFactory::local(),
            revision,
            "named".to_owned(),
            true,
            Some(PathBuf::from("/tmp")),
            Vec::new(),
            &transient_defaults,
        )
        .unwrap();
        assert_eq!(lifetime, GraphicalLairLifetime::Persistent);
        assert!(matches!(
            named,
            Request::CreateLair { name, generated_name: false, .. } if name == "named"
        ));

        let (command, lifetime) = graphical_create_request(
            &ConnectionFactory::local(),
            revision,
            "generated".to_owned(),
            false,
            Some(PathBuf::from("/tmp")),
            vec!["printf".to_owned()],
            &transient_defaults,
        )
        .unwrap();
        assert_eq!(lifetime, GraphicalLairLifetime::Persistent);
        assert!(
            matches!(command, Request::CreateLair { launch, .. } if launch.command == ["printf"])
        );

        let remote = create_request_for(
            LaunchSemantics::RemoteInteractive,
            revision,
            "generated".to_owned(),
            true,
            None,
            Vec::new(),
            &transient_defaults,
        )
        .unwrap();
        assert!(matches!(
            remote,
            Request::CreateLairAutomation {
                generated_name: true,
                ..
            }
        ));
    }

    #[test]
    fn new_dojo_requests_preserve_revision_and_resolve_names_for_both_semantics() {
        let mut lair = splinterm_core::Lair::new("main", PathBuf::from("/tmp"));
        let lair_id = lair.id;
        lair.dojos
            .push(Dojo::with_shell("Dojo 3", PathBuf::from("/tmp")));
        let revision = TopologyRevision::new(41);
        let config = AppConfig::default();

        let local = new_dojo_request_for(
            LaunchSemantics::LocalTrusted,
            revision,
            std::slice::from_ref(&lair),
            lair_id,
            None,
            Some(PathBuf::from("/work")),
            vec!["local".to_owned()],
            true,
            &config,
        )
        .unwrap();
        assert!(matches!(
            local,
            Request::NewDojo {
                expected_topology_revision,
                lair_id: requested_lair,
                name,
                launch,
                promote_transient_lair,
                generated_name: true,
            } if expected_topology_revision == revision
                && requested_lair == lair_id
                && name == "Dojo 4"
                && promote_transient_lair
                && launch.cwd == std::path::Path::new("/work")
                && launch.command == ["local"]
        ));

        let local_explicit = new_dojo_request_for(
            LaunchSemantics::LocalTrusted,
            revision,
            std::slice::from_ref(&lair),
            lair_id,
            Some("logs".to_owned()),
            Some(PathBuf::from("/work")),
            Vec::new(),
            false,
            &config,
        )
        .unwrap();
        assert!(matches!(
            local_explicit,
            Request::NewDojo {
                expected_topology_revision,
                name,
                ..
            } if expected_topology_revision == revision && name == "logs"
        ));

        let remote_implicit = new_dojo_request_for(
            LaunchSemantics::RemoteInteractive,
            revision,
            std::slice::from_ref(&lair),
            lair_id,
            None,
            None,
            Vec::new(),
            false,
            &config,
        )
        .unwrap();
        assert!(matches!(
            remote_implicit,
            Request::NewDojoAutomation {
                expected_topology_revision,
                name,
                ..
            } if expected_topology_revision == revision && name == "Dojo 4"
        ));

        let remote = new_dojo_request_for(
            LaunchSemantics::RemoteInteractive,
            revision,
            &[lair],
            lair_id,
            Some("logs".to_owned()),
            None,
            vec!["remote".to_owned()],
            false,
            &config,
        )
        .unwrap();
        assert!(matches!(
            remote,
            Request::NewDojoAutomation {
                expected_topology_revision,
                lair_id: requested_lair,
                name,
                launch,
                generated_name: false,
            } if expected_topology_revision == revision
                && requested_lair == lair_id
                && name == "logs"
                && launch.cwd.is_none()
                && launch.argv == ["remote"]
        ));
    }

    // Only a name Splinterm generated is marked so; the daemon trusts this
    // provenance instead of comparing the name with the default label.
    #[test]
    fn new_dojo_requests_mark_only_generated_names() {
        let lair = splinterm_core::Lair::new("main", PathBuf::from("/tmp"));
        let config = AppConfig::default();
        for semantics in [
            LaunchSemantics::LocalTrusted,
            LaunchSemantics::RemoteInteractive,
        ] {
            for (name, expected) in [
                (None, true),
                (Some("Dojo 2".to_owned()), false),
                (Some("logs".to_owned()), false),
            ] {
                let request = new_dojo_request_for(
                    semantics,
                    TopologyRevision::new(3),
                    std::slice::from_ref(&lair),
                    lair.id,
                    name.clone(),
                    Some(PathBuf::from("/work")),
                    Vec::new(),
                    false,
                    &config,
                )
                .unwrap();
                let (Request::NewDojo {
                    name: sent,
                    generated_name,
                    ..
                }
                | Request::NewDojoAutomation {
                    name: sent,
                    generated_name,
                    ..
                }) = request
                else {
                    panic!("expected a new Dojo request");
                };
                assert_eq!(sent, name.unwrap_or_else(|| "Dojo 2".to_owned()));
                assert_eq!(generated_name, expected, "{semantics:?} {sent}");
            }
        }
    }
}
