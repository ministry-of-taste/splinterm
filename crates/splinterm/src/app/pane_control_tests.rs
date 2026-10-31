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

// Plan 0073: a pane's display subscription survives a dropped connection.

type DuplexReader = tokio::io::ReadHalf<tokio::io::DuplexStream>;
type DuplexWriter = tokio::io::WriteHalf<tokio::io::DuplexStream>;
type ScriptedAttempts = Arc<std::sync::Mutex<std::collections::VecDeque<Result<StreamReconnect>>>>;

/// One daemon connection after its handshake, `buffer` bytes buffered each
/// way, with the daemon's ends for the test to script.
async fn daemon_end(buffer: usize) -> (Connection, DuplexReader, DuplexWriter) {
    let (client, server) = tokio::io::duplex(buffer);
    let (mut reader, mut writer) = tokio::io::split(server);
    let (client_reader, client_writer) = tokio::io::split(client);
    let (connection, ()) = tokio::join!(
        Connection::connect_remote_interactive_transport(client_reader, client_writer),
        async {
            let _hello = read_client(&mut reader).await;
            send_frame(&mut writer, server_hello(ServerLimits::default())).await;
        },
    );
    (connection.unwrap(), reader, writer)
}

fn stream_snapshot(splint_id: SplintId, revision: u64) -> TerminalSnapshot {
    TerminalSnapshot {
        splint_id,
        incarnation: 7,
        revision,
        columns: 1,
        rows: 1,
        cursor_column: 0,
        cursor_row: 0,
        cursor_deferred_wrap: false,
        active_screen: splinterm_protocol::ActiveScreen::Normal,
        input_modes: splinterm_protocol::TerminalInputModes {
            application_cursor: false,
            application_keypad: false,
            focus_reporting: false,
            bracketed_paste: false,
            cursor_visible: true,
            cursor_blink: true,
            mouse_tracking: splinterm_protocol::MouseTracking::None,
            sgr_mouse: false,
        },
        palette: vec![0; 256],
        default_colors: [0, 0, 0],
        title: String::new(),
        visible_rows: vec![splinterm_protocol::TerminalRow {
            row_id: Some(1),
            linebreak: true,
            cells: Vec::new(),
        }],
        history_generation: 1,
        oldest_available_scrollback_row_id: None,
        newest_available_scrollback_row_id: None,
        scrollback_rows: Vec::new(),
        available_scrollback_rows: 0,
        omitted_oldest_scrollback_rows: 0,
        images: None,
        exited_code: None,
        exited_signal: None,
    }
}

fn update_event(subscription_id: u64, sequence: u64, base_revision: u64) -> ServerFrame {
    ServerFrame::Event {
        subscription_id,
        sequence,
        event: SubscriptionEvent::Update {
            update: TerminalUpdate {
                base_revision,
                revision: base_revision + 1,
                rows: Vec::new(),
                scrolls: Vec::new(),
                cursor: None,
                title: None,
                input_modes: None,
                active_screen: None,
                palette: None,
                default_colors: None,
                columns: None,
                row_count: None,
                scrollback: None,
                images: None,
            },
        },
    }
}

fn attached_stream(
    connection: Connection,
    splint_id: SplintId,
    subscription_id: u64,
    revision: u64,
) -> PaneStream {
    PaneStream {
        connection,
        attachment: Attachment {
            subscription_id,
            snapshot: stream_snapshot(splint_id, revision),
        },
    }
}

/// A controller stand-in that runs until the pane is cancelled.
fn idle_controller(
    cancellation: &tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<Result<()>> {
    let cancellation = cancellation.clone();
    tokio::spawn(async move {
        cancellation.cancelled().await;
        Ok(())
    })
}

fn stream_pane(
    splint_id: SplintId,
    controller: tokio::task::JoinHandle<Result<()>>,
    resyncs: mpsc::Receiver<()>,
    updates: mpsc::Sender<WindowUpdate>,
    cancellation: &tokio_util::sync::CancellationToken,
) -> PaneSubscription {
    PaneSubscription {
        controller,
        controller_completed: false,
        resyncs,
        updates,
        splint_id,
        incarnation: 7,
        image_transport: ImageTransport::Unavailable,
        image_cache: SharedImageContentCache::default(),
        cancellation: cancellation.clone(),
        detach_on_cancel: Arc::new(AtomicBool::new(false)),
        lagged: false,
        control_lost_owed: false,
        retry: StreamRetry::default(),
    }
}

/// Reconnects with each scripted outcome in turn, then with refusals;
/// counts the attempts.
fn scripted_reconnect(
    outcomes: Vec<Result<StreamReconnect>>,
    attempts: &Arc<std::sync::atomic::AtomicUsize>,
) -> impl FnMut() -> std::future::Ready<Result<StreamReconnect>> + Send + 'static {
    let outcomes: ScriptedAttempts = Arc::new(std::sync::Mutex::new(outcomes.into()));
    let attempts = Arc::clone(attempts);
    move || {
        attempts.fetch_add(1, Ordering::SeqCst);
        let next = outcomes.lock().unwrap().pop_front();
        std::future::ready(next.unwrap_or_else(|| Err(anyhow::anyhow!("connection refused"))))
    }
}

async fn next_update(updates: &mut mpsc::Receiver<WindowUpdate>) -> WindowUpdate {
    tokio::time::timeout(Duration::from_secs(60), updates.recv())
        .await
        .expect("an update arrives")
        .expect("the update queue stays open")
}

#[tokio::test(start_paused = true)]
async fn a_display_stream_the_daemon_dropped_reattaches_with_a_snapshot_then_live_updates() {
    let id = SplintId::new();
    let (first, first_reader, first_writer) = daemon_end(65536).await;
    let (second, second_reader, mut second_writer) = daemon_end(65536).await;
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let reconnect = scripted_reconnect(
        vec![Ok(StreamReconnect::Attached(Box::new(attached_stream(
            second, id, 2, 9,
        ))))],
        &attempts,
    );
    let cancellation = tokio_util::sync::CancellationToken::new();
    let (updates, mut window) = mpsc::channel(WINDOW_UPDATE_QUEUE);
    let (_resync_sender, resyncs) = mpsc::channel(1);
    let pane = stream_pane(
        id,
        idle_controller(&cancellation),
        resyncs,
        updates,
        &cancellation,
    );
    let task = tokio::spawn(run_pane_subscription(
        attached_stream(first, id, 1, 5),
        pane,
        HandoffStatusHub::default(),
        reconnect,
    ));
    // The daemon ends the connection, as after a stalled write.
    drop((first_reader, first_writer));
    let WindowUpdate::Snapshot {
        snapshot,
        authoritative: true,
        ..
    } = next_update(&mut window).await
    else {
        panic!("the reattachment's snapshot comes first, as authoritative");
    };
    assert_eq!(snapshot.revision, 9);
    send_frame(&mut second_writer, update_event(2, 1, 9)).await;
    let WindowUpdate::Update { update, .. } = next_update(&mut window).await else {
        panic!("live updates follow the reattachment");
    };
    assert_eq!(update.revision, 10);
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    cancellation.cancel();
    tokio::time::timeout(Duration::from_millis(1), task)
        .await
        .expect("a cancelled pane stops at once")
        .unwrap()
        .unwrap();
    drop(second_reader);
}

#[tokio::test(start_paused = true)]
async fn a_resynchronization_during_an_outage_never_ends_the_controller() {
    let id = SplintId::new();
    let (control, control_server) = scripted_control(vec![(
        Request::SubscribeControl {
            splint_id: id,
            incarnation: 7,
        },
        Ok(control_subscribed(id, true)),
    )])
    .await;
    let cancellation = tokio_util::sync::CancellationToken::new();
    let (updates, mut window) = mpsc::channel(WINDOW_UPDATE_QUEUE);
    let (resync_sender, resyncs) = mpsc::channel(1);
    let (command_sender, commands) = mpsc::channel(WINDOW_COMMAND_QUEUE);
    let (ended_sender, mut ended) = tokio::sync::oneshot::channel();
    let hub = HandoffStatusHub::default();
    let outputs = ControllerOutputs {
        updates: updates.clone(),
        resyncs: resync_sender,
        handoff: hub.source(),
    };
    let token = cancellation.clone();
    let controller = tokio::spawn(async move {
        let result = run_controller(
            control,
            commands,
            outputs,
            None,
            id,
            7,
            ForcedControlTransfer::Disabled,
            0,
            token,
        )
        .await;
        let _ = ended_sender.send(result.is_ok());
        result
    });
    let (first, first_reader, first_writer) = daemon_end(65536).await;
    drop((first_reader, first_writer));
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let task = tokio::spawn(run_pane_subscription(
        attached_stream(first, id, 1, 5),
        stream_pane(id, controller, resyncs, updates, &cancellation),
        hub.clone(),
        scripted_reconnect(Vec::new(), &attempts),
    ));
    assert!(matches!(
        next_update(&mut window).await,
        WindowUpdate::Control(true)
    ));
    // Return to live, again and again, while the display cannot reconnect.
    for _ in 0..3 {
        command_sender
            .send(WindowCommand::Resynchronize)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert!(attempts.load(Ordering::SeqCst) > 1);
    assert!(
        matches!(
            ended.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ),
        "the controller keeps serving through the outage"
    );
    cancellation.cancel();
    tokio::time::timeout(Duration::from_millis(1), task)
        .await
        .expect("a cancelled pane stops at once")
        .unwrap()
        .unwrap();
    assert_eq!(ended.await, Ok(true));
    drop(command_sender);
    assert_eq!(control_server.await.unwrap(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_display_lost_for_a_while_says_so_until_its_reattachment() {
    let id = SplintId::new();
    let (first, first_reader, first_writer) = daemon_end(65536).await;
    let (second, second_reader, second_writer) = daemon_end(65536).await;
    drop((first_reader, first_writer));
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let refused = || Err(anyhow::anyhow!("connection refused"));
    let reconnect = scripted_reconnect(
        vec![
            refused(),
            refused(),
            refused(),
            refused(),
            Ok(StreamReconnect::Attached(Box::new(attached_stream(
                second, id, 2, 9,
            )))),
        ],
        &attempts,
    );
    let cancellation = tokio_util::sync::CancellationToken::new();
    let (updates, mut window) = mpsc::channel(WINDOW_UPDATE_QUEUE);
    let (_resync_sender, resyncs) = mpsc::channel(1);
    let started = tokio::time::Instant::now();
    let task = tokio::spawn(run_pane_subscription(
        attached_stream(first, id, 1, 5),
        stream_pane(
            id,
            idle_controller(&cancellation),
            resyncs,
            updates,
            &cancellation,
        ),
        HandoffStatusHub::default(),
        reconnect,
    ));
    assert!(matches!(
        next_update(&mut window).await,
        WindowUpdate::StreamLost { reconnecting: true }
    ));
    assert_eq!(started.elapsed(), PANE_STREAM_NOTICE_DELAY);
    let WindowUpdate::Snapshot {
        authoritative: true,
        ..
    } = next_update(&mut window).await
    else {
        panic!("the reattachment's authoritative snapshot ends the notice");
    };
    // Attempts back off from 250 ms, doubling: 0.25 + 0.5 + 1 + 2 + 4 s.
    assert_eq!(started.elapsed(), Duration::from_millis(7750));
    assert_eq!(attempts.load(Ordering::SeqCst), 5);
    cancellation.cancel();
    task.await.unwrap().unwrap();
    drop((second_reader, second_writer));
}

#[tokio::test(start_paused = true)]
async fn cancelling_a_pane_stops_its_reconnection_at_once() {
    let id = SplintId::new();
    let (first, first_reader, first_writer) = daemon_end(65536).await;
    drop((first_reader, first_writer));
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let cancellation = tokio_util::sync::CancellationToken::new();
    let (updates, _window) = mpsc::channel(WINDOW_UPDATE_QUEUE);
    let (_resync_sender, resyncs) = mpsc::channel(1);
    let task = tokio::spawn(run_pane_subscription(
        attached_stream(first, id, 1, 5),
        stream_pane(
            id,
            idle_controller(&cancellation),
            resyncs,
            updates,
            &cancellation,
        ),
        HandoffStatusHub::default(),
        scripted_reconnect(Vec::new(), &attempts),
    ));
    tokio::time::sleep(Duration::from_secs(3)).await;
    let tried = attempts.load(Ordering::SeqCst);
    assert!(tried >= 2);
    cancellation.cancel();
    tokio::time::timeout(Duration::from_millis(1), task)
        .await
        .expect("a cancelled pane stops at once")
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(attempts.load(Ordering::SeqCst), tried);

    // An attempt still waiting on the daemon is abandoned as promptly.
    let (first, first_reader, first_writer) = daemon_end(65536).await;
    drop((first_reader, first_writer));
    let cancellation = tokio_util::sync::CancellationToken::new();
    let (updates, _window) = mpsc::channel(WINDOW_UPDATE_QUEUE);
    let (_resync_sender, resyncs) = mpsc::channel(1);
    let task = tokio::spawn(run_pane_subscription(
        attached_stream(first, id, 1, 5),
        stream_pane(
            id,
            idle_controller(&cancellation),
            resyncs,
            updates,
            &cancellation,
        ),
        HandoffStatusHub::default(),
        std::future::pending::<Result<StreamReconnect>>,
    ));
    tokio::time::sleep(Duration::from_secs(1)).await;
    cancellation.cancel();
    tokio::time::timeout(Duration::from_millis(1), task)
        .await
        .expect("a cancelled pane abandons its attempt")
        .unwrap()
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_reconnection_that_cannot_succeed_stops_and_the_pane_waits_to_close() {
    let id = SplintId::new();
    let (first, first_reader, first_writer) = daemon_end(65536).await;
    drop((first_reader, first_writer));
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let cancellation = tokio_util::sync::CancellationToken::new();
    let (updates, mut window) = mpsc::channel(WINDOW_UPDATE_QUEUE);
    let (_resync_sender, resyncs) = mpsc::channel(1);
    let task = tokio::spawn(run_pane_subscription(
        attached_stream(first, id, 1, 5),
        stream_pane(
            id,
            idle_controller(&cancellation),
            resyncs,
            updates,
            &cancellation,
        ),
        HandoffStatusHub::default(),
        scripted_reconnect(
            vec![
                Err(anyhow::anyhow!("connection refused")),
                Err(anyhow::anyhow!("connection refused")),
                Err(anyhow::anyhow!("connection refused")),
                Ok(StreamReconnect::Stop("the Splint is gone")),
            ],
            &attempts,
        ),
    ));
    let started = tokio::time::Instant::now();
    assert!(matches!(
        next_update(&mut window).await,
        WindowUpdate::StreamLost { reconnecting: true }
    ));
    // The last attempt, at 0.25 + 0.5 + 1 + 2 s, stops reconnecting, and
    // the pane says so at once.
    assert!(matches!(
        next_update(&mut window).await,
        WindowUpdate::StreamLost {
            reconnecting: false
        }
    ));
    assert_eq!(started.elapsed(), Duration::from_millis(3750));
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 4);
    // The pane keeps its queue open: a closed queue would end the Window.
    assert!(matches!(
        window.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    assert!(!task.is_finished());
    cancellation.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_process_that_exited_while_the_display_was_lost_takes_the_exit_path() {
    let id = SplintId::new();
    let (first, first_reader, first_writer) = daemon_end(65536).await;
    drop((first_reader, first_writer));
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let cancellation = tokio_util::sync::CancellationToken::new();
    let (updates, mut window) = mpsc::channel(WINDOW_UPDATE_QUEUE);
    let (_resync_sender, resyncs) = mpsc::channel(1);
    let task = tokio::spawn(run_pane_subscription(
        attached_stream(first, id, 1, 5),
        stream_pane(
            id,
            idle_controller(&cancellation),
            resyncs,
            updates,
            &cancellation,
        ),
        HandoffStatusHub::default(),
        scripted_reconnect(vec![Ok(StreamReconnect::Exited)], &attempts),
    ));
    let WindowUpdate::Exited { splint_id } = next_update(&mut window).await else {
        panic!("the Window takes the exit path the stream would have started");
    };
    assert_eq!(splint_id, id);
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    cancellation.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn no_reconnection_is_tried_while_a_handoff_pauses_the_window() {
    let id = SplintId::new();
    let (first, first_reader, first_writer) = daemon_end(65536).await;
    drop((first_reader, first_writer));
    let hub = HandoffStatusHub::default();
    let pane_source = hub.source();
    pane_source.observe(4, HandoffStatusPhase::Fenced);
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let cancellation = tokio_util::sync::CancellationToken::new();
    let (updates, mut window) = mpsc::channel(WINDOW_UPDATE_QUEUE);
    let (_resync_sender, resyncs) = mpsc::channel(1);
    let task = tokio::spawn(run_pane_subscription(
        attached_stream(first, id, 1, 5),
        stream_pane(
            id,
            idle_controller(&cancellation),
            resyncs,
            updates,
            &cancellation,
        ),
        hub.clone(),
        scripted_reconnect(Vec::new(), &attempts),
    ));
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 0);
    // The handoff banner speaks for the Window: no notice of its own.
    assert!(matches!(
        window.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    cancellation.cancel();
    task.await.unwrap().unwrap();
    drop(pane_source);
}

fn live_splint(splint_id: SplintId, live_incarnation: Option<u64>) -> Response {
    Response::Splint {
        lair_id: LairId::new(),
        dojo_id: DojoId::new(),
        title: String::new(),
        topology_revision: splinterm_core::TopologyRevision::new(1),
        resolved_cwd: None,
        runtime: splinterm_protocol::SplintRuntimeSummary {
            splint_id,
            live_incarnation,
            last_incarnation: live_incarnation,
            restorable: false,
            lifecycle: splinterm_protocol::SplintLifecycle::Running,
            exit_status: None,
            agent_status: None,
            foreground_app: None,
        },
    }
}

const fn daemon_at(generation: u64) -> DaemonIdentity {
    DaemonIdentity {
        generation,
        pid: None,
    }
}

async fn reattach_against(
    steps: Vec<(Request, Reply)>,
    before: DaemonIdentity,
    splint_id: SplintId,
) -> (Result<StreamReconnect>, usize) {
    let (connection, server) = scripted_control(steps).await;
    let outcome = reattach_pane_stream(
        Ok(connection),
        before,
        splint_id,
        7,
        ImageTransport::Unavailable,
        &SharedImageContentCache::default(),
    )
    .await;
    if let Ok(StreamReconnect::Attached(stream)) = &outcome {
        assert_eq!(stream.attachment.subscription_id, 3);
        assert_eq!(stream.attachment.snapshot.revision, 9);
        // Its stream keeps the connection, and so the script, open.
        return (outcome, 3);
    }
    (outcome, server.await.unwrap())
}

#[tokio::test]
async fn reattachment_follows_the_first_attachment_and_stops_when_the_splint_changed() {
    let id = SplintId::new();
    let inspect = Request::InspectSplint { splint_id: id };
    // A new process took the Splint's place, or none runs: the pane's
    // process exited meanwhile.
    for live_incarnation in [Some(8), None] {
        let (outcome, answered) = reattach_against(
            vec![(inspect.clone(), Ok(live_splint(id, live_incarnation)))],
            daemon_at(1),
            id,
        )
        .await;
        assert!(matches!(outcome, Ok(StreamReconnect::Exited)));
        assert_eq!(answered, 1);
    }
    // The Splint is gone.
    let (outcome, _) = reattach_against(
        vec![(inspect.clone(), Err(ErrorCode::NotFound))],
        daemon_at(1),
        id,
    )
    .await;
    assert!(matches!(outcome, Ok(StreamReconnect::Stop(_))));
    // A restarted daemon (an older generation) is not asked anything.
    let (outcome, answered) = reattach_against(Vec::new(), daemon_at(5), id).await;
    assert!(matches!(outcome, Ok(StreamReconnect::Stop(_))));
    assert_eq!(answered, 0);
    // Nor is an upgraded one: the handoff's coordinator replaces the pane.
    let (outcome, answered) = reattach_against(Vec::new(), daemon_at(0), id).await;
    assert!(matches!(outcome, Ok(StreamReconnect::Handoff)));
    assert_eq!(answered, 0);
    // Refused access stops too; another failure is retried.
    let access = Request::RequestAccess {
        splint_id: id,
        incarnation: 7,
        scopes: pane_access_scopes(),
    };
    let (outcome, _) = reattach_against(
        vec![
            (inspect.clone(), Ok(live_splint(id, Some(7)))),
            (access.clone(), Err(ErrorCode::Unauthorized)),
        ],
        daemon_at(1),
        id,
    )
    .await;
    assert!(matches!(outcome, Ok(StreamReconnect::Stop(_))));
    let (outcome, _) = reattach_against(
        vec![
            (inspect.clone(), Ok(live_splint(id, Some(7)))),
            (access.clone(), Err(ErrorCode::ResourceLimit)),
        ],
        daemon_at(1),
        id,
    )
    .await;
    assert!(outcome.is_err());
    // The same steps as the pane's first attachment.
    let granted = Response::AccessGranted {
        lair_id: LairId::new(),
        dojo_id: DojoId::new(),
        authorization_revision: 1,
        grant: splinterm_protocol::AccessGrant {
            grant_id: 0,
            splint_id: id,
            incarnation: 7,
            scopes: pane_access_scopes(),
            requester: String::new(),
            expires_at_unix_seconds: 0,
        },
    };
    let (outcome, _) = reattach_against(
        vec![
            (inspect, Ok(live_splint(id, Some(7)))),
            (access, Ok(granted)),
            (
                Request::Attach {
                    splint_id: id,
                    incarnation: Some(7),
                    scrollback_rows: splinterm_protocol::MAX_SNAPSHOT_SCROLLBACK_ROWS,
                },
                Ok(Response::Attached {
                    subscription_id: 3,
                    provenance: page_provenance(id, 9),
                    snapshot: stream_snapshot(id, 9),
                }),
            ),
        ],
        daemon_at(1),
        id,
    )
    .await;
    assert!(matches!(outcome, Ok(StreamReconnect::Attached(_))));
}

/// The Window's update queue is full and nobody drains it, as for a pane held
/// back while the Dojo picker is open: frames sent meanwhile are read anyway,
/// so splinterd never finds the connection stalled.
async fn serve_while_the_window_is_away(
    mut reader: DuplexReader,
    mut writer: DuplexWriter,
    splint_id: SplintId,
    written: tokio::sync::oneshot::Sender<()>,
) {
    for sequence in 1..=100 {
        send_frame(&mut writer, update_event(1, sequence, 4 + sequence)).await;
    }
    let _ = written.send(());
    for (expected, result) in [
        (
            Request::Detach { subscription_id: 1 },
            Response::Acknowledged,
        ),
        (
            Request::Attach {
                splint_id,
                incarnation: Some(7),
                scrollback_rows: splinterm_protocol::MAX_SNAPSHOT_SCROLLBACK_ROWS,
            },
            Response::Attached {
                subscription_id: 2,
                provenance: page_provenance(splint_id, 300),
                snapshot: stream_snapshot(splint_id, 300),
            },
        ),
    ] {
        let ClientFrame::Request {
            request_id,
            request,
            ..
        } = read_client(&mut reader).await
        else {
            panic!("expected a request");
        };
        assert_eq!(request, expected);
        send_frame(&mut writer, ServerFrame::Response { request_id, result }).await;
    }
    let mut rest = Vec::new();
    let _ = reader.read_to_end(&mut rest).await;
}

#[tokio::test(start_paused = true)]
async fn a_window_that_stops_draining_never_stalls_the_daemon_and_gets_a_snapshot_later() {
    let id = SplintId::new();
    // Far less buffering than the frames sent: only reading drains them.
    let (connection, reader, writer) = daemon_end(4096).await;
    let (written_sender, written) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(serve_while_the_window_is_away(
        reader,
        writer,
        id,
        written_sender,
    ));
    let cancellation = tokio_util::sync::CancellationToken::new();
    let (updates, mut window) = mpsc::channel(WINDOW_UPDATE_QUEUE);
    let (_resync_sender, resyncs) = mpsc::channel(1);
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (end_controller, controller_ends) = tokio::sync::oneshot::channel::<()>();
    let controller = tokio::spawn(async move {
        let _ = controller_ends.await;
        Ok(())
    });
    let task = tokio::spawn(run_pane_subscription(
        attached_stream(connection, id, 1, 5),
        stream_pane(id, controller, resyncs, updates, &cancellation),
        HandoffStatusHub::default(),
        scripted_reconnect(Vec::new(), &attempts),
    ));
    tokio::time::timeout(Duration::from_secs(30), written)
        .await
        .expect("every frame was read while the Window was away")
        .unwrap();
    // The controller ends while the Window is still away.
    end_controller.send(()).unwrap();
    tokio::time::sleep(PANE_UPDATE_SEND_GRACE * 3).await;
    // The Window comes back: what it had queued, then the snapshot it is
    // owed, then the controller's end.
    for revision in 6..=9 {
        let WindowUpdate::Update { update, .. } = next_update(&mut window).await else {
            panic!("the queued updates come first");
        };
        assert_eq!(update.revision, revision);
    }
    let WindowUpdate::Snapshot {
        snapshot,
        authoritative: true,
        ..
    } = next_update(&mut window).await
    else {
        panic!("an authoritative snapshot replaces what was dropped");
    };
    assert_eq!(snapshot.revision, 300);
    assert!(matches!(
        next_update(&mut window).await,
        WindowUpdate::Control(false)
    ));
    assert_eq!(attempts.load(Ordering::SeqCst), 0);
    cancellation.cancel();
    task.await.unwrap().unwrap();
    server.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_controller_that_ends_during_an_outage_is_reported_once_the_window_has_room() {
    let id = SplintId::new();
    let (first, first_reader, first_writer) = daemon_end(65536).await;
    drop((first_reader, first_writer));
    let cancellation = tokio_util::sync::CancellationToken::new();
    let (updates, mut window) = mpsc::channel(1);
    // The Window's queue is full.
    updates.try_send(WindowUpdate::Control(true)).unwrap();
    let (_resync_sender, resyncs) = mpsc::channel(1);
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (end_controller, controller_ends) = tokio::sync::oneshot::channel::<()>();
    let controller = tokio::spawn(async move {
        let _ = controller_ends.await;
        Ok(())
    });
    let task = tokio::spawn(run_pane_subscription(
        attached_stream(first, id, 1, 5),
        stream_pane(id, controller, resyncs, updates, &cancellation),
        HandoffStatusHub::default(),
        scripted_reconnect(Vec::new(), &attempts),
    ));
    tokio::time::sleep(Duration::from_secs(1)).await;
    end_controller.send(()).unwrap();
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(matches!(window.try_recv(), Ok(WindowUpdate::Control(true))));
    let mut seen = Vec::new();
    for _ in 0..2 {
        seen.push(next_update(&mut window).await);
    }
    assert!(
        seen.iter()
            .any(|update| matches!(update, WindowUpdate::Control(false))),
        "{seen:?}"
    );
    cancellation.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn an_upgraded_daemon_leaves_the_pane_to_its_handoff_without_a_notice() {
    let id = SplintId::new();
    let (first, first_reader, first_writer) = daemon_end(65536).await;
    drop((first_reader, first_writer));
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let cancellation = tokio_util::sync::CancellationToken::new();
    let (updates, mut window) = mpsc::channel(WINDOW_UPDATE_QUEUE);
    let (_resync_sender, resyncs) = mpsc::channel(1);
    let task = tokio::spawn(run_pane_subscription(
        attached_stream(first, id, 1, 5),
        stream_pane(
            id,
            idle_controller(&cancellation),
            resyncs,
            updates,
            &cancellation,
        ),
        HandoffStatusHub::default(),
        scripted_reconnect(vec![Ok(StreamReconnect::Handoff)], &attempts),
    ));
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert!(matches!(
        window.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    assert!(!task.is_finished());
    cancellation.cancel();
    task.await.unwrap().unwrap();
}

/// Reattaches with a stream whose daemon end is already closed, so each
/// reattachment fails again at once; records when each attempt came.
fn failing_reattachments(
    splint_id: SplintId,
    attempted_at: &Arc<std::sync::Mutex<Vec<tokio::time::Instant>>>,
) -> impl FnMut() -> std::pin::Pin<Box<dyn Future<Output = Result<StreamReconnect>> + Send>>
+ Send
+ 'static {
    let attempted_at = Arc::clone(attempted_at);
    move || {
        let attempted_at = Arc::clone(&attempted_at);
        Box::pin(async move {
            let subscription_id = {
                let mut attempted_at = attempted_at.lock().unwrap();
                attempted_at.push(tokio::time::Instant::now());
                u64::try_from(attempted_at.len()).unwrap() + 1
            };
            let (connection, reader, writer) = daemon_end(65536).await;
            drop((reader, writer));
            Ok(StreamReconnect::Attached(Box::new(attached_stream(
                connection,
                splint_id,
                subscription_id,
                9,
            ))))
        })
    }
}

#[tokio::test(start_paused = true)]
async fn a_stream_that_fails_right_after_each_reattachment_backs_off_and_says_so() {
    let id = SplintId::new();
    let (first, first_reader, first_writer) = daemon_end(65536).await;
    drop((first_reader, first_writer));
    let attempted_at = Arc::new(std::sync::Mutex::new(Vec::new()));
    let cancellation = tokio_util::sync::CancellationToken::new();
    let (updates, mut window) = mpsc::channel(WINDOW_UPDATE_QUEUE);
    let (_resync_sender, resyncs) = mpsc::channel(1);
    let started = tokio::time::Instant::now();
    let task = tokio::spawn(run_pane_subscription(
        attached_stream(first, id, 1, 5),
        stream_pane(
            id,
            idle_controller(&cancellation),
            resyncs,
            updates,
            &cancellation,
        ),
        HandoffStatusHub::default(),
        failing_reattachments(id, &attempted_at),
    ));
    let mut first_notice = None;
    while attempted_at.lock().unwrap().len() < 7 {
        if let WindowUpdate::StreamLost { reconnecting: true } = next_update(&mut window).await {
            first_notice.get_or_insert(started.elapsed());
        }
    }
    let gaps: Vec<Duration> = std::iter::once(started)
        .chain(attempted_at.lock().unwrap().iter().copied())
        .collect::<Vec<_>>()
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .collect();
    assert_eq!(
        gaps,
        [250, 500, 1000, 2000, 4000, 5000, 5000].map(Duration::from_millis)
    );
    // The short-lived reattachments count toward the notice.
    assert_eq!(first_notice, Some(PANE_STREAM_NOTICE_DELAY));
    cancellation.cancel();
    task.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_reattachment_that_stays_up_starts_the_next_outage_afresh() {
    let mut retry = StreamRetry::default();
    let start = tokio::time::Instant::now();
    assert_eq!(retry.lost(start), start + PANE_STREAM_NOTICE_DELAY);
    assert_eq!(retry.delay, PANE_STREAM_RETRY_INITIAL);
    retry.backed_off();
    retry.reattached_at = Some(start + Duration::from_secs(1));
    // Lost again soon: the outage goes on, and so does its backoff.
    let soon = start + Duration::from_secs(3);
    assert_eq!(retry.lost(soon), soon);
    assert_eq!(retry.delay, PANE_STREAM_RETRY_INITIAL * 4);
    retry.reattached_at = Some(soon);
    // Lost after it stayed up: a new outage.
    let later = soon + PANE_STREAM_STABLE;
    assert_eq!(retry.lost(later), later + PANE_STREAM_NOTICE_DELAY);
    assert_eq!(retry.delay, PANE_STREAM_RETRY_INITIAL);
}

#[test]
fn a_bracketed_paste_is_open_until_its_closing_marker() {
    assert!(!bracketed_paste_left_open(b""));
    assert!(!bracketed_paste_left_open(b"typed"));
    assert!(bracketed_paste_left_open(b"\x1b[200~half a pa"));
    assert!(!bracketed_paste_left_open(b"\x1b[200~whole\x1b[201~"));
    assert!(bracketed_paste_left_open(
        b"\x1b[200~one\x1b[201~\x1b[200~two"
    ));
    // A closing marker cut in half has not closed the paste.
    assert!(bracketed_paste_left_open(b"\x1b[200~text\x1b[20"));
}

fn paste_cut_steps(id: SplintId) -> Vec<(Request, Reply, Vec<ServerFrame>)> {
    vec![
        (
            controlled_input(id, b"\x1b[200~ab", false),
            Ok(input_acknowledged(id)),
            Vec::new(),
        ),
        (
            controlled_input(id, b"cdefghij", false),
            Err(ErrorCode::ResourceLimit),
            Vec::new(),
        ),
    ]
}

fn input_dropped_count(updates: &[WindowUpdate]) -> usize {
    updates
        .iter()
        .filter(|update| matches!(update, WindowUpdate::InputDropped))
        .count()
}

/// Plan 0074: a full daemon input queue that cuts a bracketed paste short
/// still gets the paste's closing marker, and the Window hears of the cut.
#[tokio::test]
async fn a_bracketed_paste_cut_short_still_sends_its_closing_marker() {
    let id = SplintId::new();
    let limits = ServerLimits {
        maximum_input_bytes: 8,
        ..ServerLimits::default()
    };
    let mut steps = paste_cut_steps(id);
    steps.push((
        controlled_input(id, b"\x1b[201~", false),
        Ok(input_acknowledged(id)),
        Vec::new(),
    ));
    steps.push((
        Request::ReleaseControl { controller_id: 1 },
        Ok(Response::Acknowledged),
        Vec::new(),
    ));
    let (result, updates, _) = run_case_with(
        id,
        true,
        limits,
        vec![WindowCommand::Input(
            b"\x1b[200~abcdefghij\x1b[201~".to_vec(),
        )],
        steps,
    )
    .await;
    result.unwrap();
    assert_eq!(input_dropped_count(&updates), 1);
}

/// A queue that stays full keeps the closing marker for the next input.
#[tokio::test(start_paused = true)]
async fn a_closing_marker_the_full_queue_refuses_goes_ahead_of_the_next_input() {
    let id = SplintId::new();
    let limits = ServerLimits {
        maximum_input_bytes: 8,
        ..ServerLimits::default()
    };
    let mut steps = paste_cut_steps(id);
    for _ in 0..PASTE_CLOSE_ATTEMPTS {
        steps.push((
            controlled_input(id, b"\x1b[201~", false),
            Err(ErrorCode::ResourceLimit),
            Vec::new(),
        ));
    }
    steps.push((
        controlled_input(id, b"\x1b[201~x", false),
        Ok(input_acknowledged(id)),
        Vec::new(),
    ));
    steps.push((
        Request::ReleaseControl { controller_id: 1 },
        Ok(Response::Acknowledged),
        Vec::new(),
    ));
    let (result, _, _) = run_case_with(
        id,
        true,
        limits,
        vec![
            WindowCommand::Input(b"\x1b[200~abcdefghij\x1b[201~".to_vec()),
            WindowCommand::Input(b"x".to_vec()),
        ],
        steps,
    )
    .await;
    result.unwrap();
}

/// A closing marker put ahead of input the full queue refuses whole stays
/// owed, and goes ahead of the input after that.
#[tokio::test(start_paused = true)]
async fn a_closing_marker_refused_with_the_next_input_stays_owed() {
    let id = SplintId::new();
    let limits = ServerLimits {
        maximum_input_bytes: 8,
        ..ServerLimits::default()
    };
    let mut steps = paste_cut_steps(id);
    for _ in 0..PASTE_CLOSE_ATTEMPTS {
        steps.push((
            controlled_input(id, b"[201~", false),
            Err(ErrorCode::ResourceLimit),
            Vec::new(),
        ));
    }
    steps.push((
        controlled_input(id, b"[201~xx", false),
        Err(ErrorCode::ResourceLimit),
        Vec::new(),
    ));
    steps.push((
        controlled_input(id, b"[201~y", false),
        Ok(input_acknowledged(id)),
        Vec::new(),
    ));
    steps.push((
        Request::ReleaseControl { controller_id: 1 },
        Ok(Response::Acknowledged),
        Vec::new(),
    ));
    let (result, updates, _) = run_case_with(
        id,
        true,
        limits,
        vec![
            WindowCommand::Input(b"[200~abcdefghij[201~".to_vec()),
            // A full request's worth, so the next input is not merged in.
            WindowCommand::Input(b"xxxxxxxx".to_vec()),
            WindowCommand::Input(b"y".to_vec()),
        ],
        steps,
    )
    .await;
    result.unwrap();
    assert_eq!(input_dropped_count(&updates), 2);
}

/// Typed input cut short sends no closing marker of its own.
#[tokio::test]
async fn input_cut_short_outside_a_paste_sends_no_closing_marker() {
    let id = SplintId::new();
    let limits = ServerLimits {
        maximum_input_bytes: 8,
        ..ServerLimits::default()
    };
    let (result, updates, _) = run_case_with(
        id,
        true,
        limits,
        vec![WindowCommand::Input(b"abcdefghijkl".to_vec())],
        vec![
            (
                controlled_input(id, b"abcdefgh", false),
                Err(ErrorCode::ResourceLimit),
                Vec::new(),
            ),
            (
                Request::ReleaseControl { controller_id: 1 },
                Ok(Response::Acknowledged),
                Vec::new(),
            ),
        ],
    )
    .await;
    result.unwrap();
    assert_eq!(input_dropped_count(&updates), 1);
}
