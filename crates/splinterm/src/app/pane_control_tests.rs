//! Protocol-level controller-loop regressions; no compositor or daemon required.
use super::*;
use splinterm::handoff_status::HandoffStatusHub;
use splinterm_core::{DojoId, LairId};
use splinterm_protocol::{ClientFrame, ClientRole, ControlStatus, PROTOCOL_VERSION, ProtocolError};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

type Reply = std::result::Result<Response, ErrorCode>;

async fn read_client(reader: &mut (impl AsyncRead + Unpin)) -> ClientFrame {
    let length = reader.read_u32().await.unwrap() as usize;
    assert!(length <= 65536);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn acquire(splint_id: SplintId) -> Request {
    Request::AcquireControl {
        splint_id,
        incarnation: 7,
        modes: vec![ControlMode::Input, ControlMode::Resize],
    }
}

fn transfer(splint_id: SplintId) -> Request {
    Request::RequestControlTransfer {
        splint_id,
        incarnation: 7,
        modes: vec![ControlMode::Input, ControlMode::Resize],
    }
}

fn granted() -> Response {
    Response::ControlGranted {
        controller_id: 2,
        lair_id: LairId::new(),
        dojo_id: DojoId::new(),
    }
}

async fn send_frame(writer: &mut (impl AsyncWrite + Unpin), frame: ServerFrame) {
    writer
        .write_all(&splinterm_protocol::encode_frame(&frame).unwrap())
        .await
        .unwrap();
}

const fn server_hello(limits: ServerLimits) -> ServerFrame {
    ServerFrame::Hello {
        version: PROTOCOL_VERSION,
        limits,
        development_terminal_access: false,
        daemon_hostname: None,
        daemon_generation: 1,
    }
}

const fn control_subscribed(splint_id: SplintId, owned: bool) -> Response {
    Response::ControlSubscribed {
        subscription_id: 9,
        status: ControlStatus {
            splint_id,
            incarnation: 7,
            controlled: owned,
            locally_owned: owned,
        },
    }
}

fn input_acknowledged(splint_id: SplintId) -> Response {
    Response::TerminalActionAcknowledged {
        lair_id: LairId::new(),
        dojo_id: DojoId::new(),
        splint_id,
        incarnation: 7,
        terminal_revision: 10,
        history_generation: 0,
    }
}

async fn run_case(
    splint_id: SplintId,
    owned: bool,
    commands: Vec<WindowCommand>,
    steps: Vec<(Request, Reply)>,
) -> (Result<()>, Vec<WindowUpdate>, usize) {
    let steps = steps
        .into_iter()
        .map(|(request, reply)| (request, reply, Vec::new()))
        .collect();
    run_case_with(splint_id, owned, ServerLimits::default(), commands, steps).await
}

/// Like `run_case`, with negotiated limits and event frames sent before each reply.
async fn run_case_with(
    splint_id: SplintId,
    owned: bool,
    limits: ServerLimits,
    commands: Vec<WindowCommand>,
    steps: Vec<(Request, Reply, Vec<ServerFrame>)>,
) -> (Result<()>, Vec<WindowUpdate>, usize) {
    let hub = HandoffStatusHub::default();
    run_case_in_window(splint_id, owned, limits, commands, steps, &hub).await
}

/// Like `run_case_with`, for a pane of the Window whose handoff status is `hub`.
async fn run_case_in_window(
    splint_id: SplintId,
    owned: bool,
    limits: ServerLimits,
    commands: Vec<WindowCommand>,
    steps: Vec<(Request, Reply, Vec<ServerFrame>)>,
    hub: &HandoffStatusHub,
) -> (Result<()>, Vec<WindowUpdate>, usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (client, server) = tokio::io::duplex(65536);
        let (finished, finish) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut reader, mut writer) = tokio::io::split(server);
            let hello = read_client(&mut reader).await;
            assert!(matches!(
                hello,
                ClientFrame::Hello {
                    role: ClientRole::RemoteInteractive,
                    ..
                }
            ));
            send_frame(&mut writer, server_hello(limits)).await;
            let initial = (
                Request::SubscribeControl {
                    splint_id,
                    incarnation: 7,
                },
                Ok(control_subscribed(splint_id, owned)),
                Vec::new(),
            );
            for (expected, reply, events) in std::iter::once(initial).chain(steps) {
                let frame = read_client(&mut reader).await;
                let ClientFrame::Request {
                    request_id,
                    request,
                    ..
                } = frame
                else {
                    panic!("expected request");
                };
                assert_eq!(request, expected);
                for event in events {
                    send_frame(&mut writer, event).await;
                }
                let response = match reply {
                    Ok(result) => ServerFrame::Response { request_id, result },
                    Err(code) => ServerFrame::Error {
                        request_id: Some(request_id),
                        error: ProtocolError::new(code, "scripted rejection"),
                    },
                };
                send_frame(&mut writer, response).await;
            }
            // Do not turn successful script completion into an unrelated EOF.
            let _ = finish.await;
        });
        let (reader, writer) = tokio::io::split(client);
        let connection = Connection::connect_remote_interactive_transport(reader, writer)
            .await
            .unwrap();
        let (sender, receiver) = mpsc::channel(32);
        for command in commands {
            sender.send(command).await.unwrap();
        }
        drop(sender);
        let (updates, mut update_receiver) = mpsc::channel(32);
        // One slot, as in a Window: a second request while one waits is
        // coalesced.
        let (resyncs, mut resync_receiver) = mpsc::channel(1);
        let result = run_controller(
            connection,
            receiver,
            ControllerOutputs {
                updates,
                resyncs,
                handoff: hub.source(),
            },
            owned.then_some(1),
            splint_id,
            7,
            ForcedControlTransfer::Disabled,
            0,
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
        let _ = finished.send(());
        server.await.unwrap();
        let mut updates = Vec::new();
        while let Ok(update) = update_receiver.try_recv() {
            updates.push(update);
        }
        let mut resyncs = 0;
        while resync_receiver.try_recv().is_ok() {
            resyncs += 1;
        }
        (result, updates, resyncs)
    })
    .await
    .expect("controller protocol case must finish without hanging")
}

#[tokio::test]
async fn resynchronizations_coalesce_while_one_waits() {
    let id = SplintId::new();
    let (result, _updates, resyncs) = run_case(
        id,
        true,
        vec![
            WindowCommand::Resynchronize,
            WindowCommand::Resynchronize,
            WindowCommand::Resynchronize,
            WindowCommand::ReleaseControl,
        ],
        vec![(
            Request::ReleaseControl { controller_id: 1 },
            Ok(Response::Acknowledged),
        )],
    )
    .await;
    result.unwrap();
    assert_eq!(resyncs, 1);
}

#[tokio::test]
async fn release_then_request_reacquires_and_keeps_controller_loop_alive() {
    let id = SplintId::new();
    let (result, updates, resyncs) = run_case(
        id,
        true,
        vec![
            WindowCommand::ReleaseControl,
            WindowCommand::RequestControlTransfer,
            WindowCommand::Resynchronize,
        ],
        vec![
            (
                Request::ReleaseControl { controller_id: 1 },
                Ok(Response::Acknowledged),
            ),
            (acquire(id), Ok(granted())),
            (
                Request::ReleaseControl { controller_id: 2 },
                Ok(Response::Acknowledged),
            ),
        ],
    )
    .await;
    result.unwrap();
    assert_eq!(resyncs, 1);
    let control: Vec<_> = updates
        .into_iter()
        .filter_map(|u| {
            if let WindowUpdate::Control(active) = u {
                Some(active)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(control, vec![true, false, true]);
}

#[tokio::test]
async fn reacquire_applies_requested_resize_before_input_and_releases_new_lease() {
    let id = SplintId::new();
    let ack = || {
        Ok(Response::TerminalActionAcknowledged {
            lair_id: LairId::new(),
            dojo_id: DojoId::new(),
            splint_id: id,
            incarnation: 7,
            terminal_revision: 10,
            history_generation: 0,
        })
    };
    let (result, _, _) = run_case(
        id,
        false,
        vec![
            WindowCommand::PrepareResize {
                columns: 90,
                rows: 30,
                pixel_width: 900,
                pixel_height: 600,
            },
            WindowCommand::RequestControlTransfer,
            WindowCommand::Input(b"marker".to_vec()),
        ],
        vec![
            (acquire(id), Ok(granted())),
            (
                Request::Resize {
                    controller_id: 2,
                    splint_id: id,
                    incarnation: 7,
                    columns: 90,
                    rows: 30,
                    pixel_width: 900,
                    pixel_height: 600,
                },
                ack(),
            ),
            (
                Request::Input {
                    controller_id: 2,
                    splint_id: id,
                    incarnation: 7,
                    bytes: b"marker".to_vec(),
                    focus_report: false,
                },
                ack(),
            ),
            (
                Request::ReleaseControl { controller_id: 2 },
                Ok(Response::Acknowledged),
            ),
        ],
    )
    .await;
    result.unwrap();
}

/// Another client resized the Splint while this pane had lost control. The
/// frontend never resends a size it already sent, so reacquiring control for
/// input must reapply the pane's own size before the input, or the shell keeps
/// the other client's rows and the bottom of the pane is drawn off-screen.
#[tokio::test]
async fn reacquire_after_control_loss_reapplies_the_latest_size_before_input() {
    let id = SplintId::new();
    let lost = ServerFrame::Event {
        subscription_id: 9,
        sequence: 1,
        event: SubscriptionEvent::ControlStatusChanged {
            status: ControlStatus {
                splint_id: id,
                incarnation: 7,
                controlled: true,
                locally_owned: false,
            },
        },
    };
    let resize = |controller_id| Request::Resize {
        controller_id,
        splint_id: id,
        incarnation: 7,
        columns: 222,
        rows: 51,
        pixel_width: 1_554,
        pixel_height: 867,
    };
    let (result, updates, _) = run_case_with(
        id,
        true,
        ServerLimits::default(),
        vec![
            WindowCommand::Resize {
                columns: 222,
                rows: 51,
                pixel_width: 1_554,
                pixel_height: 867,
            },
            WindowCommand::Input(b"a".to_vec()),
        ],
        vec![
            (resize(1), Ok(input_acknowledged(id)), vec![lost]),
            (acquire(id), Ok(granted()), Vec::new()),
            (resize(2), Ok(input_acknowledged(id)), Vec::new()),
            (
                Request::Input {
                    controller_id: 2,
                    splint_id: id,
                    incarnation: 7,
                    bytes: b"a".to_vec(),
                    focus_report: false,
                },
                Ok(input_acknowledged(id)),
                Vec::new(),
            ),
            (
                Request::ReleaseControl { controller_id: 2 },
                Ok(Response::Acknowledged),
                Vec::new(),
            ),
        ],
    )
    .await;
    result.unwrap();
    let control: Vec<_> = updates
        .into_iter()
        .filter_map(|update| match update {
            WindowUpdate::Control(active) => Some(active),
            _ => None,
        })
        .collect();
    assert_eq!(control, vec![true, false, true]);
}

fn page_provenance(
    splint_id: SplintId,
    terminal_revision: u64,
) -> splinterm_protocol::TerminalProvenance {
    splinterm_protocol::TerminalProvenance {
        lair_id: LairId::new(),
        dojo_id: DojoId::new(),
        splint_id,
        incarnation: 7,
        topology_revision: splinterm_core::TopologyRevision::new(1),
        terminal_revision,
        history_generation: 1,
        title: String::new(),
    }
}

fn page_request(splint_id: SplintId, terminal_revision: u64, before_row_id: u64) -> Request {
    Request::ScrollbackPage {
        splint_id,
        incarnation: 7,
        terminal_revision,
        history_generation: 1,
        before_row_id,
        max_rows: splinterm_protocol::MAX_SCROLLBACK_PAGE_ROWS,
    }
}

fn fetch_command(splint_id: SplintId) -> WindowCommand {
    WindowCommand::FetchScrollback {
        splint_id,
        incarnation: 7,
        terminal_revision: 5,
        history_generation: 1,
        before_row_id: 100,
    }
}

/// Output kept arriving while the frontend asked for older history. The rows
/// are re-read at the newer revision instead of resynchronizing, which used to
/// drop a detached viewport back to live.
#[tokio::test]
async fn history_pages_are_reread_at_a_newer_revision_of_the_same_generation() {
    let id = SplintId::new();
    let row = |row_id| splinterm_protocol::TerminalRow {
        row_id: Some(row_id),
        linebreak: true,
        cells: Vec::new(),
    };
    let (result, updates, resyncs) = run_case(
        id,
        true,
        vec![fetch_command(id)],
        vec![
            (
                page_request(id, 5, 100),
                Ok(Response::ScrollbackResyncRequired {
                    provenance: page_provenance(id, 6),
                    current_revision: 6,
                    history_generation: 1,
                }),
            ),
            (
                page_request(id, 6, 100),
                Ok(Response::ScrollbackPage {
                    provenance: page_provenance(id, 6),
                    page: splinterm_protocol::ScrollbackPage {
                        splint_id: id,
                        incarnation: 7,
                        terminal_revision: 6,
                        history_generation: 1,
                        oldest_available_row_id: Some(97),
                        newest_available_row_id: Some(120),
                        rows: vec![row(97), row(98), row(99)],
                        has_older: false,
                    },
                }),
            ),
            (
                Request::ReleaseControl { controller_id: 1 },
                Ok(Response::Acknowledged),
            ),
        ],
    )
    .await;
    result.unwrap();
    assert_eq!(resyncs, 0);
    let pages = updates
        .into_iter()
        .find_map(|update| match update {
            WindowUpdate::ScrollbackPages(pages) => Some(pages),
            _ => None,
        })
        .expect("the re-read page reaches the frontend");
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].terminal_revision, 6);
    assert_eq!(pages[0].rows.len(), 3);
}

/// A batch with no rows still ends the frontend's pending history request.
#[tokio::test]
async fn an_empty_history_batch_still_reaches_the_frontend() {
    let id = SplintId::new();
    let (result, updates, _) = run_case(
        id,
        true,
        vec![fetch_command(id)],
        vec![
            (
                page_request(id, 5, 100),
                Ok(Response::ScrollbackPage {
                    provenance: page_provenance(id, 5),
                    page: splinterm_protocol::ScrollbackPage {
                        splint_id: id,
                        incarnation: 7,
                        terminal_revision: 5,
                        history_generation: 1,
                        oldest_available_row_id: Some(100),
                        newest_available_row_id: Some(120),
                        rows: Vec::new(),
                        has_older: false,
                    },
                }),
            ),
            (
                Request::ReleaseControl { controller_id: 1 },
                Ok(Response::Acknowledged),
            ),
        ],
    )
    .await;
    result.unwrap();
    assert!(updates.iter().any(|update| matches!(
        update,
        WindowUpdate::ScrollbackPages(pages) if pages.is_empty()
    )));
}

#[tokio::test]
async fn contested_request_uses_consent_and_duplicate_pending_is_nonfatal() {
    let id = SplintId::new();
    let (result, updates, resyncs) = run_case(
        id,
        false,
        vec![
            WindowCommand::RequestControlTransfer,
            WindowCommand::RequestControlTransfer,
            WindowCommand::Resynchronize,
        ],
        vec![
            (acquire(id), Err(ErrorCode::ControllerUnavailable)),
            (
                transfer(id),
                Ok(Response::ControlTransferPending {
                    transfer_id: 3,
                    lair_id: LairId::new(),
                    dojo_id: DojoId::new(),
                }),
            ),
            (acquire(id), Err(ErrorCode::ControllerUnavailable)),
            (transfer(id), Err(ErrorCode::ControlTransferUnavailable)),
        ],
    )
    .await;
    result.unwrap();
    assert_eq!(resyncs, 1);
    assert!(
        !updates
            .iter()
            .any(|u| matches!(u, WindowUpdate::Control(true)))
    );
}

#[tokio::test]
async fn owner_release_race_is_nonfatal_and_later_request_can_acquire() {
    let id = SplintId::new();
    let (result, updates, resyncs) = run_case(
        id,
        false,
        vec![
            WindowCommand::RequestControlTransfer,
            WindowCommand::Resynchronize,
            WindowCommand::RequestControlTransfer,
        ],
        vec![
            (acquire(id), Err(ErrorCode::ControllerUnavailable)),
            (transfer(id), Err(ErrorCode::ControlTransferUnavailable)),
            (acquire(id), Ok(granted())),
            (
                Request::ReleaseControl { controller_id: 2 },
                Ok(Response::Acknowledged),
            ),
        ],
    )
    .await;
    result.unwrap();
    assert_eq!(resyncs, 1);
    assert!(
        updates
            .iter()
            .any(|u| matches!(u, WindowUpdate::Control(true)))
    );
}

#[tokio::test]
async fn denied_control_request_remains_observing_without_retry_or_takeover() {
    let id = SplintId::new();
    let (result, updates, resyncs) = run_case(
        id,
        false,
        vec![
            WindowCommand::RequestControlTransfer,
            WindowCommand::Resynchronize,
        ],
        vec![
            (acquire(id), Err(ErrorCode::Unauthorized)),
            (transfer(id), Err(ErrorCode::Unauthorized)),
        ],
    )
    .await;
    result.unwrap();
    assert_eq!(resyncs, 1);
    assert!(
        !updates
            .iter()
            .any(|u| matches!(u, WindowUpdate::Control(true)))
    );
}

#[tokio::test]
async fn already_owned_control_request_is_a_noop() {
    let id = SplintId::new();
    let (result, _, resyncs) = run_case(
        id,
        true,
        vec![
            WindowCommand::RequestControlTransfer,
            WindowCommand::Resynchronize,
        ],
        vec![(
            Request::ReleaseControl { controller_id: 1 },
            Ok(Response::Acknowledged),
        )],
    )
    .await;
    result.unwrap();
    assert_eq!(resyncs, 1);
}

#[tokio::test]
async fn unexpected_control_errors_and_responses_are_still_fatal() {
    for transfer_reply in [Err(ErrorCode::InvalidArgument), Ok(Response::Acknowledged)] {
        let id = SplintId::new();
        let (result, _, resyncs) = run_case(
            id,
            false,
            vec![
                WindowCommand::RequestControlTransfer,
                WindowCommand::Resynchronize,
            ],
            vec![
                (acquire(id), Err(ErrorCode::ControllerUnavailable)),
                (transfer(id), transfer_reply),
            ],
        )
        .await;
        assert!(result.is_err());
        assert_eq!(resyncs, 0);
    }
}

fn controlled_input(id: SplintId, bytes: &[u8], focus_report: bool) -> Request {
    Request::Input {
        controller_id: 1,
        splint_id: id,
        incarnation: 7,
        bytes: bytes.to_vec(),
        focus_report,
    }
}

#[tokio::test]
async fn generated_focus_reports_are_flagged_and_typed_bytes_are_not() {
    let id = SplintId::new();
    let (result, _, _) = run_case(
        id,
        true,
        vec![
            WindowCommand::FocusReport(true),
            // The same bytes typed or pasted are ordinary input.
            WindowCommand::Input(b"\x1b[I".to_vec()),
            WindowCommand::FocusReport(false),
        ],
        vec![
            (
                controlled_input(id, b"\x1b[I", true),
                Ok(input_acknowledged(id)),
            ),
            (
                controlled_input(id, b"\x1b[I", false),
                Ok(input_acknowledged(id)),
            ),
            (
                controlled_input(id, b"\x1b[O", true),
                Ok(input_acknowledged(id)),
            ),
            (
                Request::ReleaseControl { controller_id: 1 },
                Ok(Response::Acknowledged),
            ),
        ],
    )
    .await;
    result.unwrap();
}

#[tokio::test]
async fn queued_input_is_coalesced_between_barriers_into_fewer_requests() {
    let id = SplintId::new();
    let (result, _, _) = run_case(
        id,
        true,
        vec![
            WindowCommand::Input(b"ab".to_vec()),
            WindowCommand::Input(b"c".to_vec()),
            WindowCommand::Input(b"d".to_vec()),
            WindowCommand::Resize {
                columns: 90,
                rows: 30,
                pixel_width: 900,
                pixel_height: 600,
            },
            WindowCommand::Input(b"e".to_vec()),
            WindowCommand::Input(Vec::new()),
            WindowCommand::Input(b"f".to_vec()),
            // A generated focus report is a barrier and keeps its flag.
            WindowCommand::FocusReport(true),
            WindowCommand::Input(b"g".to_vec()),
        ],
        vec![
            (
                controlled_input(id, b"abcd", false),
                Ok(input_acknowledged(id)),
            ),
            (
                Request::Resize {
                    controller_id: 1,
                    splint_id: id,
                    incarnation: 7,
                    columns: 90,
                    rows: 30,
                    pixel_width: 900,
                    pixel_height: 600,
                },
                Ok(input_acknowledged(id)),
            ),
            (
                controlled_input(id, b"ef", false),
                Ok(input_acknowledged(id)),
            ),
            (
                controlled_input(id, b"\x1b[I", true),
                Ok(input_acknowledged(id)),
            ),
            (
                controlled_input(id, b"g", false),
                Ok(input_acknowledged(id)),
            ),
            (
                Request::ReleaseControl { controller_id: 1 },
                Ok(Response::Acknowledged),
            ),
        ],
    )
    .await;
    result.unwrap();
}

#[tokio::test]
async fn control_loss_between_coalesced_input_batches_is_observed_before_the_next_batch() {
    let id = SplintId::new();
    let limits = ServerLimits {
        maximum_input_bytes: 8,
        ..ServerLimits::default()
    };
    let lost = ServerFrame::Event {
        subscription_id: 9,
        sequence: 1,
        event: SubscriptionEvent::ControlStatusChanged {
            status: ControlStatus {
                splint_id: id,
                incarnation: 7,
                controlled: true,
                locally_owned: false,
            },
        },
    };
    // Each input exceeds half the bound, so the second is a coalescing barrier.
    // Control is lost while the first round trip is in flight. The barrier must
    // wait for that event and reacquire, never reuse the stale controller 1.
    let (result, updates, _) = run_case_with(
        id,
        true,
        limits,
        vec![
            WindowCommand::Input(b"abcde".to_vec()),
            WindowCommand::Input(b"fghij".to_vec()),
        ],
        vec![
            (
                Request::Input {
                    controller_id: 1,
                    splint_id: id,
                    incarnation: 7,
                    bytes: b"abcde".to_vec(),
                    focus_report: false,
                },
                Ok(input_acknowledged(id)),
                vec![lost],
            ),
            (
                acquire(id),
                Err(ErrorCode::ControllerUnavailable),
                Vec::new(),
            ),
        ],
    )
    .await;
    result.unwrap();
    let control: Vec<_> = updates
        .into_iter()
        .filter_map(|update| match update {
            WindowUpdate::Control(active) => Some(active),
            _ => None,
        })
        .collect();
    assert_eq!(control, vec![true, false, false]);
}

#[tokio::test]
async fn cancellation_stops_a_chain_of_coalescing_barriers() {
    let id = SplintId::new();
    let limits = ServerLimits {
        maximum_input_bytes: 8,
        ..ServerLimits::default()
    };
    let (client, server) = tokio::io::duplex(65536);
    let cancellation = tokio_util::sync::CancellationToken::new();
    let server_cancellation = cancellation.clone();
    let server = tokio::spawn(async move {
        let (mut reader, mut writer) = tokio::io::split(server);
        read_client(&mut reader).await;
        send_frame(&mut writer, server_hello(limits)).await;
        let mut inputs = 0;
        loop {
            let ClientFrame::Request {
                request_id,
                request,
                ..
            } = read_client(&mut reader).await
            else {
                panic!("expected request");
            };
            let result = match request {
                Request::SubscribeControl { .. } => control_subscribed(id, true),
                Request::Input { .. } => {
                    inputs += 1;
                    // The pane closes while the first batch is in flight.
                    server_cancellation.cancel();
                    input_acknowledged(id)
                }
                Request::ReleaseControl { .. } => {
                    send_frame(
                        &mut writer,
                        ServerFrame::Response {
                            request_id,
                            result: Response::Acknowledged,
                        },
                    )
                    .await;
                    return inputs;
                }
                other => panic!("unexpected request {other:?}"),
            };
            send_frame(&mut writer, ServerFrame::Response { request_id, result }).await;
        }
    });
    let (reader, writer) = tokio::io::split(client);
    let connection = Connection::connect_remote_interactive_transport(reader, writer)
        .await
        .unwrap();
    let (sender, receiver) = mpsc::channel(32);
    for bytes in [b"abcde", b"fghij", b"klmno"] {
        sender
            .send(WindowCommand::Input(bytes.to_vec()))
            .await
            .unwrap();
    }
    let (updates, _update_receiver) = mpsc::channel(32);
    let (resyncs, _resync_receiver) = mpsc::channel(32);
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        run_controller(
            connection,
            receiver,
            ControllerOutputs {
                updates,
                resyncs,
                handoff: HandoffStatusHub::default().source(),
            },
            Some(1),
            id,
            7,
            ForcedControlTransfer::Disabled,
            0,
            cancellation,
        ),
    )
    .await
    .expect("cancelled controller must not keep draining queued input");
    result.unwrap();
    drop(sender);
    assert_eq!(
        server.await.unwrap(),
        1,
        "only the in-flight batch was sent"
    );
}

fn handoff_event(sequence: u64, phase: HandoffStatusPhase) -> ServerFrame {
    ServerFrame::Event {
        subscription_id: 9,
        sequence,
        event: SubscriptionEvent::HandoffStatus {
            daemon_generation: 1,
            phase,
        },
    }
}

#[test]
fn handoff_pause_drops_input_resize_and_transfer_but_not_reads_or_releases() {
    let resize = WindowCommand::Resize {
        columns: 80,
        rows: 24,
        pixel_width: 800,
        pixel_height: 480,
    };
    let prepare = WindowCommand::PrepareResize {
        columns: 80,
        rows: 24,
        pixel_width: 800,
        pixel_height: 480,
    };
    for paused in [
        WindowCommand::Input(b"x".to_vec()),
        WindowCommand::FocusReport(true),
        resize,
        prepare,
        WindowCommand::RequestControlTransfer,
        WindowCommand::DecideControlTransfer {
            transfer_id: 3,
            decision: splinterm_protocol::ControlTransferDecision::Deny,
        },
        WindowCommand::ForceControlTransfer,
    ] {
        assert!(paused_during_handoff(&paused), "{paused:?}");
    }
    for allowed in [
        WindowCommand::Resynchronize,
        WindowCommand::FetchScrollback {
            splint_id: SplintId::new(),
            incarnation: 7,
            terminal_revision: 1,
            history_generation: 1,
            before_row_id: 1,
        },
        WindowCommand::RevokeAccess(4),
        WindowCommand::CancelPendingAccess,
        WindowCommand::Search {
            terminal_revision: 1,
            history_generation: 1,
            query: "needle".to_owned(),
            case_sensitive: false,
            cursor: None,
        },
        WindowCommand::ReleaseControl,
    ] {
        assert!(!paused_during_handoff(&allowed), "{allowed:?}");
    }
}

#[test]
fn handoff_status_is_read_only_from_this_control_subscription() {
    let frame = handoff_event(1, HandoffStatusPhase::Fenced);
    assert_eq!(
        handoff_status_event(&frame, 9),
        Some((1, HandoffStatusPhase::Fenced))
    );
    assert_eq!(handoff_status_event(&frame, 10), None);
    let status = ServerFrame::Event {
        subscription_id: 9,
        sequence: 1,
        event: SubscriptionEvent::ControlStatusChanged {
            status: ControlStatus {
                splint_id: SplintId::new(),
                incarnation: 7,
                controlled: true,
                locally_owned: true,
            },
        },
    };
    assert_eq!(handoff_status_event(&status, 9), None);
}

/// Another pane of the Window heard the pause first: this pane's queued
/// input, resize, and transfer requests never reach the daemon; its control
/// release still does.
#[tokio::test]
async fn a_paused_window_drops_queued_commands_before_the_daemon() {
    let id = SplintId::new();
    let hub = HandoffStatusHub::default();
    let other_pane = hub.source();
    assert!(other_pane.observe(1, HandoffStatusPhase::Preparing));
    let (result, _, _) = run_case_in_window(
        id,
        true,
        ServerLimits::default(),
        vec![
            WindowCommand::Input(b"typed".to_vec()),
            WindowCommand::FocusReport(false),
            WindowCommand::Resize {
                columns: 90,
                rows: 30,
                pixel_width: 900,
                pixel_height: 600,
            },
            WindowCommand::RequestControlTransfer,
            WindowCommand::ForceControlTransfer,
            WindowCommand::ReleaseControl,
        ],
        vec![(
            Request::ReleaseControl { controller_id: 1 },
            Ok(Response::Acknowledged),
            Vec::new(),
        )],
        &hub,
    )
    .await;
    result.unwrap();
    assert!(hub.input_paused());
}

/// The pane's own control subscription delivers the pause; input queued
/// behind it is dropped, and the Window-wide state records the phase.
#[tokio::test]
async fn a_handoff_status_on_the_control_subscription_pauses_the_window() {
    let id = SplintId::new();
    let hub = HandoffStatusHub::default();
    let (result, _, _) = run_case_in_window(
        id,
        true,
        ServerLimits::default(),
        vec![
            WindowCommand::ReleaseControl,
            WindowCommand::Input(b"after the fence".to_vec()),
        ],
        vec![(
            Request::ReleaseControl { controller_id: 1 },
            Ok(Response::Acknowledged),
            vec![
                handoff_event(1, HandoffStatusPhase::Preparing),
                handoff_event(2, HandoffStatusPhase::Fenced),
            ],
        )],
        &hub,
    )
    .await;
    // An Input sent to the daemon would have hung the script: it was dropped.
    result.unwrap();
    // Preparing and Fenced each changed the Window; the pane closing then
    // withdrew its report, as a closed pane does.
    let view = hub.view();
    assert_eq!(view.revision, 3);
    assert_eq!(
        view.phase,
        splinterm::handoff_status::WindowHandoffPhase::Idle
    );
}

/// The daemon fenced before the pane heard the pause: the refused input is
/// dropped and the controller keeps running.
#[tokio::test]
async fn a_handoff_refusal_drops_input_without_failing_the_controller() {
    let id = SplintId::new();
    let (result, _, _) = run_case(
        id,
        true,
        vec![
            WindowCommand::Input(b"raced".to_vec()),
            WindowCommand::ReleaseControl,
        ],
        vec![
            (
                controlled_input(id, b"raced", false),
                Err(ErrorCode::HandoffInProgress),
            ),
            (
                Request::ReleaseControl { controller_id: 1 },
                Ok(Response::Acknowledged),
            ),
        ],
    )
    .await;
    result.unwrap();
}

/// Runs a controller whose daemon reports `phase` and then closes the
/// connection, as a committed handoff's exec does; returns the Window's
/// handoff phase afterwards.
async fn handoff_phase_after_connection_loss(
    phase: HandoffStatusPhase,
) -> splinterm::handoff_status::WindowHandoffPhase {
    let id = SplintId::new();
    let hub = HandoffStatusHub::default();
    let (client, server) = tokio::io::duplex(65536);
    let server = tokio::spawn(async move {
        let (mut reader, mut writer) = tokio::io::split(server);
        read_client(&mut reader).await;
        send_frame(&mut writer, server_hello(ServerLimits::default())).await;
        let ClientFrame::Request { request_id, .. } = read_client(&mut reader).await else {
            panic!("expected the control subscription");
        };
        send_frame(
            &mut writer,
            ServerFrame::Response {
                request_id,
                result: control_subscribed(id, false),
            },
        )
        .await;
        send_frame(&mut writer, handoff_event(1, phase)).await;
        // The daemon execs: the connection closes.
    });
    let (reader, writer) = tokio::io::split(client);
    let connection = Connection::connect_remote_interactive_transport(reader, writer)
        .await
        .unwrap();
    let (_sender, receiver) = mpsc::channel(4);
    let (updates, _update_receiver) = mpsc::channel(32);
    let (resyncs, _resync_receiver) = mpsc::channel(32);
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        run_controller(
            connection,
            receiver,
            ControllerOutputs {
                updates,
                resyncs,
                handoff: hub.source(),
            },
            None,
            id,
            7,
            ForcedControlTransfer::Disabled,
            0,
            tokio_util::sync::CancellationToken::new(),
        ),
    )
    .await
    .expect("a lost connection ends the controller");
    server.await.unwrap();
    assert!(result.is_err());
    assert!(
        hub.input_paused(),
        "nothing typed meanwhile reaches the daemon"
    );
    hub.view().phase
}

/// A connection lost while fenced starts the Window's reconnection.
#[tokio::test]
async fn a_connection_lost_while_fenced_reconnects_the_window() {
    assert_eq!(
        handoff_phase_after_connection_loss(HandoffStatusPhase::Fenced).await,
        splinterm::handoff_status::WindowHandoffPhase::Reconnecting
    );
}

/// The exec race: the daemon can exec before this subscription delivers
/// `Fenced`, so a loss after only `Preparing` reconnects too.
#[tokio::test]
async fn a_connection_lost_after_only_preparing_reconnects_the_window() {
    assert_eq!(
        handoff_phase_after_connection_loss(HandoffStatusPhase::Preparing).await,
        splinterm::handoff_status::WindowHandoffPhase::Reconnecting
    );
}

/// A scripted daemon for one connection: each expected request gets its
/// reply. Returns how many requests it answered.
async fn scripted_control(
    steps: Vec<(Request, Reply)>,
) -> (Connection, tokio::task::JoinHandle<usize>) {
    let (client, server) = tokio::io::duplex(65536);
    let server = tokio::spawn(async move {
        let (mut reader, mut writer) = tokio::io::split(server);
        let _hello = read_client(&mut reader).await;
        send_frame(&mut writer, server_hello(ServerLimits::default())).await;
        let mut answered = 0;
        for (expected, reply) in steps {
            let ClientFrame::Request {
                request_id,
                request,
                ..
            } = read_client(&mut reader).await
            else {
                panic!("expected a request");
            };
            assert_eq!(request, expected);
            let frame = match reply {
                Ok(result) => ServerFrame::Response { request_id, result },
                Err(code) => ServerFrame::Error {
                    request_id: Some(request_id),
                    error: ProtocolError::new(code, "scripted rejection"),
                },
            };
            send_frame(&mut writer, frame).await;
            answered += 1;
        }
        // Nothing else may be sent: the client end closing ends the read.
        let mut rest = Vec::new();
        let _ = reader.read_to_end(&mut rest).await;
        assert!(rest.is_empty(), "an unexpected request followed the script");
        answered
    });
    let (reader, writer) = tokio::io::split(client);
    let connection = Connection::connect_remote_interactive_transport(reader, writer)
        .await
        .unwrap();
    (connection, server)
}

fn handoff_claim_request(
    ticket: splinterm_protocol::HandoffResumeTicket,
    splint_id: SplintId,
) -> Request {
    Request::ClaimHandoffControl {
        ticket,
        splint: Some(splinterm_protocol::HandoffClaimTarget {
            splint_id,
            incarnation: 7,
        }),
    }
}

/// A relaunched Window's pane presents its ticket once for its own Splint
/// and keeps the controller the daemon restores (Plan 0061 M3.16).
#[tokio::test]
async fn a_pane_restores_its_controller_with_the_window_ticket_once() {
    let ticket = splinterm_protocol::HandoffResumeTicket::new(
        [9; splinterm_protocol::HANDOFF_RESUME_TICKET_BYTES],
    );
    let id = SplintId::new();
    let restore = splinterm::handoff_control::HandoffControlRestore::default();
    restore.arm(
        ticket,
        [id],
        splinterm::handoff_control::CONTROL_RESTORE_WINDOW,
    );
    let (mut control, server) = scripted_control(vec![(
        handoff_claim_request(ticket, id),
        Ok(Response::HandoffControlClaimed {
            controllers: vec![splinterm_protocol::HandoffClaimedController {
                splint_id: id,
                incarnation: 7,
                controller_id: 31,
                modes: vec![ControlMode::Input, ControlMode::Resize],
            }],
        }),
    )])
    .await;
    assert_eq!(
        restore_handoff_control(&restore, &mut control, id, 7)
            .await
            .unwrap(),
        Some(31)
    );
    // Taken once: a later pane of the same Splint sends nothing.
    assert_eq!(
        restore_handoff_control(&restore, &mut control, id, 7)
            .await
            .unwrap(),
        None
    );
    drop(control);
    assert_eq!(server.await.unwrap(), 1);
}

/// Without a ticket nothing is sent; a refused or empty claim falls back to
/// the ordinary path instead of failing the pane.
#[tokio::test]
async fn a_pane_without_a_ticket_or_with_a_refused_claim_falls_back() {
    let ticket = splinterm_protocol::HandoffResumeTicket::new(
        [4; splinterm_protocol::HANDOFF_RESUME_TICKET_BYTES],
    );
    let (unarmed, refused, empty) = (SplintId::new(), SplintId::new(), SplintId::new());
    let restore = splinterm::handoff_control::HandoffControlRestore::default();
    let (mut control, server) = scripted_control(Vec::new()).await;
    assert_eq!(
        restore_handoff_control(&restore, &mut control, unarmed, 7)
            .await
            .unwrap(),
        None
    );
    drop(control);
    assert_eq!(server.await.unwrap(), 0);

    restore.arm(
        ticket,
        [refused, empty],
        splinterm::handoff_control::CONTROL_RESTORE_WINDOW,
    );
    let (mut control, server) = scripted_control(vec![
        (
            handoff_claim_request(ticket, refused),
            Err(ErrorCode::InvalidHandoffTicket),
        ),
        (
            handoff_claim_request(ticket, empty),
            Ok(Response::HandoffControlClaimed {
                controllers: Vec::new(),
            }),
        ),
    ])
    .await;
    for splint_id in [refused, empty] {
        assert_eq!(
            restore_handoff_control(&restore, &mut control, splint_id, 7)
                .await
                .unwrap(),
            None
        );
    }
    drop(control);
    assert_eq!(server.await.unwrap(), 2);
}
