//! Which directory each Window creation kind asks the owning daemon for.
use std::path::{Path, PathBuf};

use super::*;
use splinterm_core::{Axis, Lair, Topology};
use splinterm_protocol::{
    ClientFrame, LaunchDirectory, PROTOCOL_VERSION, ProtocolError, ServerFrame, ServerLimits,
    TopologySnapshot,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Replies in order; the final scripted request is rejected so the creation
/// stops before any attachment work, leaving the recorded requests.
async fn scripted<T>(
    replies: Vec<Response>,
    exercise: impl AsyncFnOnce(&mut Connection) -> Result<T>,
) -> Vec<Request> {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let (client, server) = tokio::io::duplex(65536);
        let peer = tokio::spawn(async move {
            let (mut reader, mut writer) = tokio::io::split(server);
            let read = async |reader: &mut tokio::io::ReadHalf<tokio::io::DuplexStream>| {
                let size = reader.read_u32().await.unwrap() as usize;
                let mut bytes = vec![0; size];
                reader.read_exact(&mut bytes).await.unwrap();
                serde_json::from_slice::<ClientFrame>(&bytes).unwrap()
            };
            assert!(matches!(read(&mut reader).await, ClientFrame::Hello { .. }));
            let hello = ServerFrame::Hello {
                version: PROTOCOL_VERSION,
                limits: ServerLimits::default(),
                development_terminal_access: false,
                daemon_hostname: None,
                daemon_generation: 1,
            };
            writer
                .write_all(&splinterm_protocol::encode_frame(&hello).unwrap())
                .await
                .unwrap();
            let mut requests = Vec::new();
            let mut replies = replies.into_iter();
            loop {
                let ClientFrame::Request {
                    request_id,
                    request,
                    ..
                } = read(&mut reader).await
                else {
                    panic!("expected request")
                };
                requests.push(request);
                let frame = match replies.next() {
                    Some(result) => ServerFrame::Response { request_id, result },
                    None => ServerFrame::Error {
                        request_id: Some(request_id),
                        error: ProtocolError::new(ErrorCode::InvalidArgument, "scripted stop"),
                    },
                };
                writer
                    .write_all(&splinterm_protocol::encode_frame(&frame).unwrap())
                    .await
                    .unwrap();
                if matches!(frame, ServerFrame::Error { .. }) {
                    return requests;
                }
            }
        });
        let (reader, writer) = tokio::io::split(client);
        let mut connection = Connection::connect_remote_interactive_transport(reader, writer)
            .await
            .unwrap();
        assert!(exercise(&mut connection).await.is_err());
        drop(connection);
        peer.await.unwrap()
    })
    .await
    .expect("bounded scripted creation")
}

fn resolved(source: SplintId, cwd: &str) -> Response {
    Response::LaunchDirectoryResolved {
        source_splint_id: source,
        cwd: Some(PathBuf::from(cwd)),
    }
}

fn configured(new_dojo: LaunchDirectory, new_splint: LaunchDirectory) -> AppConfig {
    AppConfig {
        new_dojo_directory: new_dojo,
        new_splint_directory: new_splint,
        ..AppConfig::default()
    }
}

async fn split_requests(
    config: AppConfig,
    replies: impl FnOnce(SplintId) -> Vec<Response>,
) -> (Vec<Request>, SplintId) {
    let lair = Lair::new("source", "/owner/project".into());
    let dojo = lair.dojos[0].clone();
    let target = dojo.default_focus;
    let requests = scripted(replies(target), async |connection| {
        apply_topology_command(
            &ConnectionFactory::local(),
            connection,
            &config,
            dojo.id,
            &dojo.root,
            TopologyRevision::default(),
            WindowTopologyCommand::Split {
                dojo_id: dojo.id,
                target,
                axis: Axis::Horizontal,
                pending: None,
            },
        )
        .await
    })
    .await;
    (requests, target)
}

fn launch_cwd(request: &Request) -> &Path {
    match request {
        Request::SplitSplint { launch, .. }
        | Request::NewDojo { launch, .. }
        | Request::CreateLair { launch, .. }
        | Request::CreateTransientLair { launch, .. } => &launch.cwd,
        request => panic!("not a launch request: {request:?}"),
    }
}

#[tokio::test]
async fn splits_inherit_the_source_directory_by_default() {
    let (requests, target) = split_requests(AppConfig::default(), |target| {
        vec![Response::Splint {
            lair_id: LairId::new(),
            dojo_id: DojoId::new(),
            title: String::new(),
            topology_revision: TopologyRevision::default(),
            resolved_cwd: Some(PathBuf::from("/owner/project/src")),
            runtime: splinterm_protocol::SplintRuntimeSummary {
                splint_id: target,
                live_incarnation: None,
                last_incarnation: None,
                restorable: false,
                lifecycle: splinterm_protocol::SplintLifecycle::Exited,
                exit_status: None,
                agent_status: None,
                foreground_app: None,
            },
        }]
    })
    .await;
    assert_eq!(requests[0], Request::InspectSplint { splint_id: target });
    assert_eq!(launch_cwd(&requests[1]), Path::new("/owner/project/src"));
}

#[tokio::test]
async fn splits_use_a_configured_directory_from_the_owning_daemon() {
    let (requests, target) = split_requests(
        configured(LaunchDirectory::Inherit, LaunchDirectory::Home),
        |target| vec![resolved(target, "/owner")],
    )
    .await;
    assert_eq!(
        requests[0],
        Request::ResolveLaunchDirectory {
            source_splint_id: target,
            directory: LaunchDirectory::Home,
        }
    );
    assert_eq!(launch_cwd(&requests[1]), Path::new("/owner"));
}

#[tokio::test]
async fn new_dojos_start_in_the_owner_home_by_default() {
    let lair = Lair::new("source", "/owner/project".into());
    let lair_id = lair.id;
    let source = lair.dojos[0].default_focus;
    let replies = vec![
        Response::Lairs {
            lairs: vec![lair],
            topology_revision: TopologyRevision::default(),
        },
        resolved(source, "/owner"),
    ];
    let requests = scripted(replies, async |connection| {
        create_dojo_in_lair(
            &ConnectionFactory::local(),
            connection,
            None,
            &AppConfig::default(),
            lair_id,
            source,
            None,
        )
        .await
    })
    .await;
    assert_eq!(requests[0], Request::ListLairs);
    assert_eq!(
        requests[1],
        Request::ResolveLaunchDirectory {
            source_splint_id: source,
            directory: LaunchDirectory::Home,
        }
    );
    assert_eq!(launch_cwd(&requests[2]), Path::new("/owner"));
}

#[tokio::test]
async fn new_lairs_follow_the_new_dojo_directory() {
    let source = SplintId::new();
    for (directory, cwd) in [
        (LaunchDirectory::Home, "/owner"),
        (
            LaunchDirectory::HomeRelative(PathBuf::from("work")),
            "/owner/work",
        ),
    ] {
        let config = configured(directory.clone(), LaunchDirectory::Inherit);
        let requests = scripted(
            vec![
                Response::Topology {
                    snapshot: TopologySnapshot {
                        revision: TopologyRevision::default(),
                        topology: Topology::default(),
                        runtimes: Vec::new(),
                        presentation_offers: Vec::new(),
                    },
                },
                resolved(source, cwd),
            ],
            async |connection| {
                create_daily_dojo(
                    &ConnectionFactory::local(),
                    connection,
                    &config,
                    source,
                    None,
                )
                .await
            },
        )
        .await;
        assert_eq!(requests[0], Request::InspectTopology);
        assert_eq!(
            requests[1],
            Request::ResolveLaunchDirectory {
                source_splint_id: source,
                directory,
            }
        );
        assert_eq!(launch_cwd(&requests[2]), Path::new(cwd));
    }
}
