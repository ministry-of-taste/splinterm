//! Daemon-to-frontend pane protocol bridge.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
#[cfg(test)]
use splinterm::automation::response_protocol_error;
use splinterm::automation::{
    Connection, ImageContentLeaseSet, SharedImageContentCache, protocol_error,
};
use splinterm::config::AppConfig;
use splinterm::endpoint::{ConnectionFactory, ForcedControlTransfer, ImageTransport};
use splinterm::handoff_status::HandoffStatusSource;
use splinterm::{
    AuthorityStatus, PerfTraceCorrelation, TerminalGridLimits, WindowCommand, WindowPaneOptions,
    WindowUpdate,
};
use splinterm_core::{LayoutNode, SplintId};
use splinterm_protocol::{
    AccessGrant, AccessScope, ControlMode, ControlTransferOutcome, ErrorCode, HandoffStatusPhase,
    LairAccessGrant, Request, Response, ServerFrame, ServerLimits, SubscriptionEvent,
    TerminalSnapshot, TerminalUpdate,
    perf_trace::{PerfTraceEvent, emit_perf_trace, perf_trace_enabled},
};
use tokio::sync::mpsc;

pub(in crate::app) const WINDOW_UPDATE_QUEUE: usize = 4;
pub(in crate::app) const WINDOW_COMMAND_QUEUE: usize = 64;

pub(in crate::app) struct Attachment {
    pub(in crate::app) subscription_id: u64,
    pub(in crate::app) snapshot: TerminalSnapshot,
}

#[allow(
    clippy::large_enum_variant,
    reason = "subscription events already own bounded protocol snapshots"
)]
#[derive(Debug, PartialEq)]
pub(in crate::app) enum EventAction {
    Ignore,
    /// An in-order event that changes no terminal state, such as handoff
    /// status, which may ride any subscription; it only advances the sequence.
    Skip {
        sequence: u64,
    },
    Snapshot {
        sequence: u64,
        snapshot: TerminalSnapshot,
    },
    Update {
        sequence: u64,
        update: TerminalUpdate,
    },
    Resynchronize,
    Exited,
    Shutdown,
}

pub(in crate::app) fn classify_subscription_event(
    expected_subscription: u64,
    last_sequence: u64,
    subscription_id: u64,
    sequence: u64,
    event: SubscriptionEvent,
) -> EventAction {
    if subscription_id != expected_subscription {
        return EventAction::Ignore;
    }
    match event {
        SubscriptionEvent::Exited { .. } => EventAction::Exited,
        SubscriptionEvent::AccessRevoked { .. } => EventAction::Shutdown,
        _ if last_sequence.checked_add(1) != Some(sequence) => EventAction::Resynchronize,
        SubscriptionEvent::Snapshot { snapshot } => EventAction::Snapshot { sequence, snapshot },
        SubscriptionEvent::Update { update } => EventAction::Update { sequence, update },
        SubscriptionEvent::ResyncRequired { .. } => EventAction::Resynchronize,
        SubscriptionEvent::HandoffStatus { .. } => EventAction::Skip { sequence },
        SubscriptionEvent::TopologyChanged { .. }
        | SubscriptionEvent::TopologyResyncRequired { .. }
        | SubscriptionEvent::ControlStatusChanged { .. }
        | SubscriptionEvent::ControlTransferRequested { .. }
        | SubscriptionEvent::ControlTransferResolved { .. }
        | SubscriptionEvent::AgentStatusChanged { .. } => EventAction::Ignore,
    }
}

pub(in crate::app) fn update_advances_from(update: &TerminalUpdate, current_revision: u64) -> bool {
    update.base_revision == current_revision && update.revision > current_revision
}

pub(in crate::app) fn validate_attached_snapshot(
    snapshot: &TerminalSnapshot,
    splint_id: SplintId,
    incarnation: u64,
) -> Result<()> {
    snapshot
        .validate()
        .map_err(|error| anyhow::anyhow!(error.message.clone()))?;
    if snapshot.splint_id != splint_id || snapshot.incarnation != incarnation {
        bail!("splinterd returned a snapshot for a different live Splint identity");
    }
    Ok(())
}

pub(in crate::app) async fn attach(
    connection: &mut Connection,
    splint_id: SplintId,
    incarnation: u64,
) -> Result<Attachment> {
    let Response::Attached {
        subscription_id,
        snapshot,
        ..
    } = connection
        .request(Request::Attach {
            splint_id,
            incarnation: Some(incarnation),
            scrollback_rows: splinterm_protocol::MAX_SNAPSHOT_SCROLLBACK_ROWS,
        })
        .await?
    else {
        bail!("splinterd did not return an attached terminal snapshot");
    };
    validate_attached_snapshot(&snapshot, splint_id, incarnation)?;
    Ok(Attachment {
        subscription_id,
        snapshot,
    })
}

pub(in crate::app) async fn resynchronize(
    connection: &mut Connection,
    old_subscription: u64,
    splint_id: SplintId,
    incarnation: u64,
) -> Result<Attachment> {
    let _ = connection
        .request(Request::Detach {
            subscription_id: old_subscription,
        })
        .await?;
    attach(connection, splint_id, incarnation).await
}

pub(in crate::app) fn authority_status(
    grants: Vec<AccessGrant>,
    lair_grants: Vec<LairAccessGrant>,
    development_bypass: bool,
) -> AuthorityStatus {
    let mut status = grants
        .into_iter()
        .filter(|grant| grant.grant_id != 0)
        .map(|grant| {
            let scopes = grant
                .scopes
                .iter()
                .map(|scope| scope.label())
                .collect::<Vec<_>>()
                .join(", ");
            (grant.grant_id, format!("{}: {scopes}", grant.requester))
        })
        .collect::<Vec<_>>();
    status.extend(
        lair_grants
            .into_iter()
            .filter(|grant| grant.grant_id != 0)
            .map(|grant| {
                let scopes = grant
                    .scopes
                    .iter()
                    .map(|scope| scope.label())
                    .collect::<Vec<_>>()
                    .join(", ");
                (
                    grant.grant_id,
                    format!("{} · Lair access: {scopes}", grant.requester),
                )
            }),
    );
    AuthorityStatus {
        grants: status,
        development_bypass,
    }
}

pub(in crate::app) async fn load_authority_status(
    connection: &mut Connection,
    splint_id: SplintId,
    incarnation: u64,
) -> Result<AuthorityStatus> {
    match connection
        .request(Request::AuthorizationStatus {
            splint_id,
            incarnation: Some(incarnation),
        })
        .await?
    {
        Response::AuthorizationStatus {
            grants,
            lair_grants,
            persistent: _,
            development_bypass,
            ..
        } => Ok(authority_status(grants, lair_grants, development_bypass)),
        _ => bail!("splinterd did not return authorization status"),
    }
}

pub(in crate::app) fn validate_scrollback_page_response(
    page: &splinterm_protocol::ScrollbackPage,
    splint_id: SplintId,
    incarnation: u64,
    terminal_revision: u64,
    history_generation: u64,
    before_row_id: u64,
) -> Result<()> {
    page.validate()
        .map_err(|error| anyhow::anyhow!(error.message))?;
    if page.splint_id != splint_id
        || page.incarnation != incarnation
        || page.terminal_revision != terminal_revision
        || page.history_generation != history_generation
        || page
            .rows
            .iter()
            .filter_map(|row| row.row_id)
            .any(|row_id| row_id >= before_row_id)
    {
        bail!("splinterd returned a scrollback page outside the requested bounds");
    }
    Ok(())
}

pub(in crate::app) async fn fetch_scrollback_pages(
    connection: &mut Connection,
    splint_id: SplintId,
    incarnation: u64,
    terminal_revision: u64,
    history_generation: u64,
    mut before_row_id: u64,
) -> Result<Option<Vec<splinterm_protocol::ScrollbackPage>>> {
    const PREFETCH_PAGE_COUNT: usize = 4;
    // Output that keeps arriving moves the terminal revision between the
    // frontend's request and each page. History rows never change within one
    // generation, so a page is re-read at the newer revision a few times
    // before the frontend resynchronizes and loses its place.
    const REVISION_RETRIES: usize = 4;
    let started = std::time::Instant::now();
    let mut pages = Vec::with_capacity(PREFETCH_PAGE_COUNT);
    let mut retries = 0;
    let mut terminal_revision = terminal_revision;
    while pages.len() < PREFETCH_PAGE_COUNT {
        let response = connection
            .request(Request::ScrollbackPage {
                splint_id,
                incarnation,
                terminal_revision,
                history_generation,
                before_row_id,
                max_rows: splinterm_protocol::MAX_SCROLLBACK_PAGE_ROWS,
            })
            .await?;
        let page = match response {
            Response::ScrollbackPage { page, .. } => page,
            Response::ScrollbackResyncRequired {
                current_revision,
                history_generation: current_generation,
                ..
            } if current_generation == history_generation
                && current_revision > terminal_revision
                && retries < REVISION_RETRIES =>
            {
                retries += 1;
                terminal_revision = current_revision;
                continue;
            }
            Response::ScrollbackResyncRequired { .. } => return Ok(None),
            _ => bail!("splinterd did not return a scrollback page"),
        };
        validate_scrollback_page_response(
            &page,
            splint_id,
            incarnation,
            terminal_revision,
            history_generation,
            before_row_id,
        )?;
        let next_before = page.rows.first().and_then(|row| row.row_id);
        let has_older = page.has_older;
        if page.rows.is_empty() {
            break;
        }
        pages.push(page);
        if !has_older {
            break;
        }
        let Some(next_before) = next_before else {
            break;
        };
        before_row_id = next_before;
    }
    if std::env::var_os("SPLINTERM_SCROLL_TRACE").is_some() {
        eprintln!(
            "scroll-trace page_batch_us={} pages={} rows={}",
            started.elapsed().as_micros(),
            pages.len(),
            pages.iter().map(|page| page.rows.len()).sum::<usize>(),
        );
    }
    Ok(Some(pages))
}

pub(in crate::app) struct ControllerOutputs {
    pub(in crate::app) updates: mpsc::Sender<WindowUpdate>,
    pub(in crate::app) resyncs: mpsc::Sender<()>,
    /// This pane's report into its Window's handoff status (Plan 0061
    /// M3.12a). The controller reports what its control subscription hears,
    /// and drops paused commands whichever pane heard the pause first.
    pub(in crate::app) handoff: HandoffStatusSource,
}

type PaneResize = (u16, u16, u16, u16);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::app) struct PendingPaneResize {
    pub(in crate::app) size: PaneResize,
    pub(in crate::app) claim_control: bool,
}

impl PendingPaneResize {
    fn from_command(command: WindowCommand) -> std::result::Result<Self, WindowCommand> {
        match command {
            WindowCommand::Resize {
                columns,
                rows,
                pixel_width,
                pixel_height,
            } => Ok(Self {
                size: (columns, rows, pixel_width, pixel_height),
                claim_control: true,
            }),
            WindowCommand::PrepareResize {
                columns,
                rows,
                pixel_width,
                pixel_height,
            } => Ok(Self {
                size: (columns, rows, pixel_width, pixel_height),
                claim_control: false,
            }),
            command => Err(command),
        }
    }

    fn merge(&mut self, next: Self) {
        self.size = next.size;
        self.claim_control |= next.claim_control;
    }

    fn into_command(self) -> WindowCommand {
        let (columns, rows, pixel_width, pixel_height) = self.size;
        if self.claim_control {
            WindowCommand::Resize {
                columns,
                rows,
                pixel_width,
                pixel_height,
            }
        } else {
            WindowCommand::PrepareResize {
                columns,
                rows,
                pixel_width,
                pixel_height,
            }
        }
    }
}

pub(in crate::app) fn queue_pane_resize(
    pending: &mut Option<PendingPaneResize>,
    deadline: &mut Option<tokio::time::Instant>,
    next: PendingPaneResize,
    delay: Duration,
    now: tokio::time::Instant,
) -> Option<WindowCommand> {
    if delay.is_zero() {
        return Some(next.into_command());
    }
    if let Some(pending) = pending {
        pending.merge(next);
    } else {
        *pending = Some(next);
    }
    *deadline = Some(now + delay);
    None
}

pub(in crate::app) async fn wait_for_resize_deadline(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// The bytes of a focus report this Window generated under DECSET 1004.
const fn focus_report_bytes(focused: bool) -> &'static [u8] {
    if focused { b"\x1b[I" } else { b"\x1b[O" }
}

/// Terminal input to send for a command, and whether it is a focus report
/// this Window generated rather than input from the user.
fn terminal_input(command: WindowCommand) -> Option<(Vec<u8>, bool)> {
    match command {
        WindowCommand::Input(bytes) => Some((bytes, false)),
        WindowCommand::FocusReport(focused) => Some((focus_report_bytes(focused).to_vec(), true)),
        _ => None,
    }
}

fn input_requests(
    controller_id: u64,
    splint_id: SplintId,
    incarnation: u64,
    bytes: &[u8],
    focus_report: bool,
    maximum_input_bytes: usize,
) -> Result<Vec<Request>> {
    if maximum_input_bytes == 0 {
        bail!("splinterd advertised a zero terminal input limit");
    }
    Ok(bytes
        .chunks(maximum_input_bytes)
        .map(|chunk| Request::Input {
            controller_id,
            splint_id,
            incarnation,
            bytes: chunk.to_vec(),
            focus_report,
        })
        .collect())
}

/// Appends the Input commands queued directly behind `first`, so they cost one
/// daemon round trip instead of one each.
///
/// Coalescing stops at the first queued command that is not Input, or at an
/// Input that would push the total past `limit`, the daemon's per-request
/// input bound. That command is returned unchanged for the caller to handle
/// next, so byte order and order relative to every other command are kept.
/// `first` alone may still exceed `limit`; `input_requests` splits it.
fn coalesce_input(
    first: Vec<u8>,
    limit: usize,
    mut next_queued: impl FnMut() -> Option<WindowCommand>,
) -> (Vec<u8>, Option<WindowCommand>) {
    let mut bytes = first;
    while bytes.len() < limit {
        match next_queued() {
            None => break,
            Some(WindowCommand::Input(more)) if more.len() <= limit - bytes.len() => {
                bytes.extend(more);
            }
            Some(barrier) => return (bytes, Some(barrier)),
        }
    }
    (bytes, None)
}

fn recoverable_input_error(error: &anyhow::Error) -> bool {
    protocol_error(error).is_some_and(|error| error.code == ErrorCode::ResourceLimit)
}

/// A request the daemon refused because a handoff fenced it. The Window's
/// pause may not have arrived yet; the command is dropped, never retried.
pub(in crate::app) fn handoff_refused(error: &anyhow::Error) -> bool {
    protocol_error(error).is_some_and(|error| error.code == ErrorCode::HandoffInProgress)
}

/// A handshake the daemon refused because this client is not the installed
/// graphical client it trusts, as after an upgrade or a rollback replaced
/// the installed build (AGENTS.md section 4.4). Retrying cannot help; only a
/// Window reopened from the launcher runs the trusted build (Plan 0061
/// M3.12b). Window continuation (M3.14-M3.16) relaunches instead.
pub(in crate::app) fn refused_as_untrusted(error: &anyhow::Error) -> bool {
    protocol_error(error).is_some_and(|error| error.code == ErrorCode::Unauthorized)
}

/// What a Window whose client build the daemon no longer trusts reports.
pub(in crate::app) const UNTRUSTED_AFTER_UPGRADE: &str = "splinterm: splinterd no longer trusts this client build, as after an upgrade replaced it; reopen this Window from the launcher";

/// Whether the controller drops `command` while its Window's input is paused
/// for a daemon handoff: terminal input, resize, and control transfer. Reads
/// (scrollback, search, resynchronization) and the releases a fenced daemon
/// still serves (control release, access revocation) pass. Dropped commands
/// are not queued; the frontend re-sends its geometry when the pause ends.
pub(in crate::app) const fn paused_during_handoff(command: &WindowCommand) -> bool {
    match command {
        WindowCommand::Input(_)
        | WindowCommand::FocusReport(_)
        | WindowCommand::Resize { .. }
        | WindowCommand::PrepareResize { .. }
        | WindowCommand::RequestControlTransfer
        | WindowCommand::DecideControlTransfer { .. }
        | WindowCommand::ForceControlTransfer => true,
        WindowCommand::Resynchronize
        | WindowCommand::FetchScrollback { .. }
        | WindowCommand::RevokeAccess(_)
        | WindowCommand::CancelPendingAccess
        | WindowCommand::Search { .. }
        | WindowCommand::ReleaseControl => false,
    }
}

/// The handoff status a control-subscription frame carries, if any.
pub(in crate::app) fn handoff_status_event(
    frame: &ServerFrame,
    subscription_id: u64,
) -> Option<(u64, HandoffStatusPhase)> {
    match frame {
        ServerFrame::Event {
            subscription_id: event_subscription,
            event:
                SubscriptionEvent::HandoffStatus {
                    daemon_generation,
                    phase,
                },
            ..
        } if *event_subscription == subscription_id => Some((*daemon_generation, *phase)),
        _ => None,
    }
}

pub(in crate::app) fn terminal_action_matches(
    response: &Response,
    splint_id: SplintId,
    incarnation: u64,
) -> bool {
    matches!(
        response,
        Response::TerminalActionAcknowledged {
            splint_id: acknowledged_splint,
            incarnation: acknowledged_incarnation,
            ..
        } if *acknowledged_splint == splint_id && *acknowledged_incarnation == incarnation
    )
}

/// Applies the pane's latest requested size once this pane controls it.
///
/// Called whenever control is (re)acquired. Another client can resize the
/// Splint while this pane does not control it, and the frontend never resends
/// a size it already sent, so every acquisition re-applies the size the pane
/// last asked for, even one the daemon already applied once.
pub(in crate::app) async fn apply_requested_pane_resize(
    control: &mut Connection,
    controller_id: Option<u64>,
    requested_resize: Option<PaneResize>,
    splint_id: SplintId,
    incarnation: u64,
) -> Result<()> {
    let (Some(controller_id), Some((columns, rows, pixel_width, pixel_height))) =
        (controller_id, requested_resize)
    else {
        return Ok(());
    };
    let response = match control
        .request(Request::Resize {
            controller_id,
            splint_id,
            incarnation,
            columns,
            rows,
            pixel_width,
            pixel_height,
        })
        .await
    {
        Ok(response) => response,
        // Fenced by a handoff: dropped, and re-sent by the frontend on resume.
        Err(error) if handoff_refused(&error) => return Ok(()),
        Err(error) => return Err(error),
    };
    if !terminal_action_matches(&response, splint_id, incarnation) {
        bail!("splinterd did not acknowledge the requested pane resize");
    }
    Ok(())
}

pub(in crate::app) async fn ensure_pane_control(
    control: &mut Connection,
    active_controller: &mut Option<u64>,
    requested_resize: &mut Option<PaneResize>,
    updates: &mpsc::Sender<WindowUpdate>,
    splint_id: SplintId,
    incarnation: u64,
    apply_requested_resize: bool,
) -> Result<Option<u64>> {
    if let Some(controller_id) = *active_controller {
        return Ok(Some(controller_id));
    }
    let Some(controller_id) = optional_pane_controller(
        control
            .acquire_control(
                splint_id,
                incarnation,
                vec![ControlMode::Input, ControlMode::Resize],
            )
            .await,
    )?
    else {
        let _ = updates.send(WindowUpdate::Control(false)).await;
        return Ok(None);
    };
    *active_controller = Some(controller_id);
    let _ = updates.send(WindowUpdate::Control(true)).await;
    if apply_requested_resize {
        apply_requested_pane_resize(
            control,
            *active_controller,
            *requested_resize,
            splint_id,
            incarnation,
        )
        .await?;
    }
    Ok(Some(controller_id))
}

pub(in crate::app) async fn handle_scrollback_fetch(
    control: &mut Connection,
    outputs: &ControllerOutputs,
    splint_id: SplintId,
    incarnation: u64,
    terminal_revision: u64,
    history_generation: u64,
    before_row_id: u64,
) -> Result<bool> {
    let pages = fetch_scrollback_pages(
        control,
        splint_id,
        incarnation,
        terminal_revision,
        history_generation,
        before_row_id,
    )
    .await?;
    // An empty batch is still delivered: it ends the frontend's pending
    // request, which otherwise blocks further history requests until the next
    // terminal update.
    if let Some(pages) = pages {
        return Ok(outputs
            .updates
            .send(WindowUpdate::ScrollbackPages(pages))
            .await
            .is_ok());
    }
    let _ = outputs
        .updates
        .send(WindowUpdate::ScrollbackResyncRequired)
        .await;
    Ok(outputs.resyncs.send(()).await.is_ok())
}

pub(in crate::app) fn resolved_resize_request(
    controller_id: Option<u64>,
    requested_resize: &mut Option<PaneResize>,
    identity: (SplintId, u64),
    resize: PaneResize,
) -> Option<Request> {
    *requested_resize = Some(resize);
    let controller_id = controller_id?;
    let (splint_id, incarnation) = identity;
    Some(Request::Resize {
        controller_id,
        splint_id,
        incarnation,
        columns: resize.0,
        rows: resize.1,
        pixel_width: resize.2,
        pixel_height: resize.3,
    })
}

pub(in crate::app) async fn active_resize_request(
    control: &mut Connection,
    active_controller: &mut Option<u64>,
    requested_resize: &mut Option<PaneResize>,
    updates: &mpsc::Sender<WindowUpdate>,
    identity: (SplintId, u64),
    resize: PaneResize,
) -> Result<Option<Request>> {
    let (splint_id, incarnation) = identity;
    let controller_id = ensure_pane_control(
        control,
        active_controller,
        requested_resize,
        updates,
        splint_id,
        incarnation,
        false,
    )
    .await?;
    Ok(resolved_resize_request(
        controller_id,
        requested_resize,
        (splint_id, incarnation),
        resize,
    ))
}

pub(in crate::app) async fn handle_control_event(
    frame: ServerFrame,
    subscription_id: u64,
    active_controller: &mut Option<u64>,
    updates: &mpsc::Sender<WindowUpdate>,
) -> Result<bool> {
    let ServerFrame::Event {
        subscription_id: event_subscription,
        event,
        ..
    } = frame
    else {
        bail!("splinterd sent an unexpected control-subscription frame");
    };
    if event_subscription != subscription_id {
        return Ok(false);
    }
    let mut control_acquired = false;
    match event {
        SubscriptionEvent::ControlStatusChanged { status } => {
            if !status.locally_owned {
                *active_controller = None;
            }
            let _ = updates
                .send(WindowUpdate::Control(status.locally_owned))
                .await;
        }
        SubscriptionEvent::ControlTransferRequested { transfer_id } => {
            let _ = updates
                .send(WindowUpdate::ControlTransferRequested(transfer_id))
                .await;
        }
        SubscriptionEvent::ControlTransferResolved {
            outcome,
            controller_id,
            ..
        } => {
            if outcome == ControlTransferOutcome::Granted {
                *active_controller = controller_id;
                control_acquired = controller_id.is_some();
            }
            let _ = updates
                .send(WindowUpdate::ControlTransferResolved(outcome))
                .await;
        }
        _ => {}
    }
    Ok(control_acquired)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the controller task's whole configuration is passed through"
)]
pub(in crate::app) async fn run_controller(
    control: Connection,
    mut commands: mpsc::Receiver<WindowCommand>,
    outputs: ControllerOutputs,
    controller_id: Option<u64>,
    splint_id: SplintId,
    incarnation: u64,
    forced_control_transfer: ForcedControlTransfer,
    resize_delay_ms: u64,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<()> {
    drive_controller(
        control,
        &mut commands,
        outputs,
        controller_id,
        splint_id,
        incarnation,
        forced_control_transfer,
        resize_delay_ms,
        cancellation,
    )
    .await
}

/// [`run_controller`] on a borrowed command queue, so a single-pane Window
/// keeps the queue its frontend sends to when a reconnection replaces the
/// controller (Plan 0061 M3.12b).
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one task serializes control ownership, search, resize, input, and cancellation"
)]
pub(in crate::app) async fn drive_controller(
    mut control: Connection,
    commands: &mut mpsc::Receiver<WindowCommand>,
    outputs: ControllerOutputs,
    controller_id: Option<u64>,
    splint_id: SplintId,
    incarnation: u64,
    forced_control_transfer: ForcedControlTransfer,
    resize_delay_ms: u64,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<()> {
    let mut active_controller = controller_id;
    let mut requested_resize = None;
    let mut pending_resize = None;
    let mut resize_deadline = None;
    let mut deferred_command = None;
    let mut coalescing_barrier = None;
    let resize_delay = Duration::from_millis(resize_delay_ms);
    let Response::ControlSubscribed {
        subscription_id: control_subscription,
        status: initial_status,
    } = control
        .request(Request::SubscribeControl {
            splint_id,
            incarnation,
        })
        .await?
    else {
        bail!("splinterd did not establish a control subscription");
    };
    active_controller = active_controller.filter(|_| initial_status.locally_owned);
    let _ = outputs
        .updates
        .send(WindowUpdate::Control(initial_status.locally_owned))
        .await;
    let result = async {
        loop {
            let (command, debounce_incoming_resize) = if let Some(command) = deferred_command.take()
            {
                (Some(command), false)
            } else {
                tokio::select! {
                    biased;
                    () = cancellation.cancelled() => break,
                    frame = control.next_server_frame() => {
                        let frame = frame?;
                        if let Some((generation, phase)) =
                            handoff_status_event(&frame, control_subscription)
                        {
                            outputs.handoff.observe(generation, phase);
                            continue;
                        }
                        let control_acquired = handle_control_event(
                            frame,
                            control_subscription,
                            &mut active_controller,
                            &outputs.updates,
                        ).await?;
                        if control_acquired
                            && pending_resize.is_none()
                            && !outputs.handoff.input_paused()
                        {
                            apply_requested_pane_resize(
                                &mut control,
                                active_controller,
                                requested_resize,
                                splint_id,
                                incarnation,
                            ).await?;
                        }
                        continue;
                    }
                    // A command taken from the queue while coalescing input is
                    // handled as if `recv` had just returned it, but only after
                    // cancellation and control changes, so a chain of oversized
                    // inputs cannot outrun a lost controller or a closing pane.
                    // The async block takes the barrier only when this branch wins.
                    command = async { coalescing_barrier.take() }, if coalescing_barrier.is_some() => {
                        (command, true)
                    }
                    command = commands.recv() => (command, true),
                    () = wait_for_resize_deadline(resize_deadline) => {
                        resize_deadline = None;
                        (
                            pending_resize.take().map(PendingPaneResize::into_command),
                            false,
                        )
                    }
                }
            };
            let (command, debounce_incoming_resize) = if let Some(command) = command {
                (command, debounce_incoming_resize)
            } else {
                resize_deadline = None;
                let Some(pending) = pending_resize.take() else {
                    break;
                };
                (pending.into_command(), false)
            };
            let command = if debounce_incoming_resize {
                match PendingPaneResize::from_command(command) {
                    Ok(next) => {
                        let Some(command) = queue_pane_resize(
                            &mut pending_resize,
                            &mut resize_deadline,
                            next,
                            resize_delay,
                            tokio::time::Instant::now(),
                        ) else {
                            continue;
                        };
                        command
                    }
                    Err(command) => {
                        if let Some(pending) = pending_resize.take() {
                            resize_deadline = None;
                            deferred_command = Some(command);
                            pending.into_command()
                        } else {
                            command
                        }
                    }
                }
            } else {
                command
            };
            // Plan 0061 M3.12a: while the Window is paused for a daemon
            // handoff, terminal input, resize, and control transfer are
            // dropped here, not queued, even when they were queued before the
            // frontend saw the pause. The frontend re-sends its geometry once
            // the pause ends.
            if paused_during_handoff(&command) && outputs.handoff.input_paused() {
                continue;
            }
            let request = match command {
                WindowCommand::Input(_) | WindowCommand::FocusReport(_) => {
                    let Some((bytes, focus_report)) = terminal_input(command) else {
                        unreachable!("matched terminal input");
                    };
                    // One daemon round trip per queued keystroke makes remote
                    // input lag behind typing; send what is already queued at
                    // once. A generated focus report keeps its own flagged
                    // request, and the coalescer stops at one as a barrier.
                    let bytes = if focus_report {
                        bytes
                    } else {
                        let (bytes, barrier) = coalesce_input(
                            bytes,
                            control.limits().maximum_input_bytes,
                            || commands.try_recv().ok(),
                        );
                        coalescing_barrier = barrier;
                        bytes
                    };
                    let controller_id = match ensure_pane_control(
                        &mut control,
                        &mut active_controller,
                        &mut requested_resize,
                        &outputs.updates,
                        splint_id,
                        incarnation,
                        true,
                    )
                    .await
                    {
                        Ok(controller_id) => controller_id,
                        // Fenced before this pane heard the pause: dropped.
                        Err(error) if handoff_refused(&error) => continue,
                        Err(error) => return Err(error),
                    };
                    let Some(controller_id) = controller_id else {
                        continue;
                    };
                    let maximum_input_bytes = control.limits().maximum_input_bytes;
                    for request in input_requests(
                        controller_id,
                        splint_id,
                        incarnation,
                        &bytes,
                        focus_report,
                        maximum_input_bytes,
                    )? {
                        match control.request(request).await {
                            Ok(response)
                                if terminal_action_matches(&response, splint_id, incarnation) => {}
                            Ok(_) => {
                                bail!("splinterd did not acknowledge a window input command");
                            }
                            Err(error) if recoverable_input_error(&error) => {
                                eprintln!(
                                    "splinterm input queue saturated; dropping remaining terminal input"
                                );
                                break;
                            }
                            // Fenced before this pane heard the pause: the
                            // rest of the batch is dropped, not queued.
                            Err(error) if handoff_refused(&error) => break,
                            Err(error) => return Err(error),
                        }
                    }
                    continue;
                }
                WindowCommand::Resynchronize => {
                    // A resynchronization already waiting covers this one: it
                    // starts after this request, so its snapshot is at least as
                    // new. Waiting for room instead would stall this loop
                    // behind a slow snapshot while the Window's commands pile
                    // up, and a full Window queue ends the Window.
                    match outputs.resyncs.try_send(()) {
                        Ok(()) | Err(mpsc::error::TrySendError::Full(())) => {}
                        Err(mpsc::error::TrySendError::Closed(())) => {
                            bail!("pane subscription stopped before resynchronization");
                        }
                    }
                    continue;
                }
                WindowCommand::Resize {
                    columns,
                    rows,
                    pixel_width,
                    pixel_height,
                } => {
                    let request = match active_resize_request(
                        &mut control,
                        &mut active_controller,
                        &mut requested_resize,
                        &outputs.updates,
                        (splint_id, incarnation),
                        (columns, rows, pixel_width, pixel_height),
                    )
                    .await
                    {
                        Ok(request) => request,
                        Err(error) if handoff_refused(&error) => continue,
                        Err(error) => return Err(error),
                    };
                    let Some(request) = request else {
                        continue;
                    };
                    request
                }
                WindowCommand::PrepareResize {
                    columns,
                    rows,
                    pixel_width,
                    pixel_height,
                } => {
                    let Some(request) = resolved_resize_request(
                        active_controller,
                        &mut requested_resize,
                        (splint_id, incarnation),
                        (columns, rows, pixel_width, pixel_height),
                    ) else {
                        continue;
                    };
                    request
                }
                WindowCommand::FetchScrollback {
                    splint_id,
                    incarnation,
                    terminal_revision,
                    history_generation,
                    before_row_id,
                } => {
                    if !handle_scrollback_fetch(
                        &mut control,
                        &outputs,
                        splint_id,
                        incarnation,
                        terminal_revision,
                        history_generation,
                        before_row_id,
                    )
                    .await?
                    {
                        break;
                    }
                    continue;
                }
                WindowCommand::RevokeAccess(grant_id) => Request::RevokeAccess { grant_id },
                WindowCommand::CancelPendingAccess => {
                    // Best effort: a daemon that refuses it still revoked
                    // every grant this Window named, and the pane keeps
                    // serving.
                    if let Err(error) = control.request(Request::CancelAllLairAccessRequests).await {
                        eprintln!("splinterm could not cancel pending access requests: {error:#}");
                    }
                    continue;
                }
                WindowCommand::RequestControlTransfer => {
                    // A released/unowned pane has nobody to consent to a
                    // transfer. Reuse ordinary acquisition, including deferred
                    // geometry, before asking an existing owner for consent.
                    match ensure_pane_control(
                        &mut control,
                        &mut active_controller,
                        &mut requested_resize,
                        &outputs.updates,
                        splint_id,
                        incarnation,
                        true,
                    )
                    .await
                    {
                        Ok(Some(_)) => continue,
                        Ok(None) => {}
                        Err(error) if handoff_refused(&error) => continue,
                        Err(error) => return Err(error),
                    }
                    match control
                        .request(Request::RequestControlTransfer {
                            splint_id,
                            incarnation,
                            modes: vec![ControlMode::Input, ControlMode::Resize],
                        })
                        .await
                    {
                        Ok(Response::ControlTransferPending { .. }) => {}
                        Err(error)
                            if protocol_error(&error).is_some_and(|error| {
                                matches!(
                                    error.code,
                                    ErrorCode::ControlTransferUnavailable
                                        | ErrorCode::Unauthorized
                                        | ErrorCode::HandoffInProgress
                                )
                            }) =>
                        {
                            // Ownership can change between acquisition and the
                            // consent request, or a transfer can already be
                            // pending. Stay observing; a later explicit request
                            // may retry without closing the pane or forcing it.
                            eprintln!("splinterm control request rejected: {error}");
                        }
                        Err(error) => return Err(error),
                        Ok(_) => bail!("splinterd did not queue the control transfer"),
                    }
                    continue;
                }
                WindowCommand::DecideControlTransfer {
                    transfer_id,
                    decision,
                } => Request::DecideControlTransfer {
                    transfer_id,
                    decision,
                },
                WindowCommand::ForceControlTransfer => {
                    let Some(request) =
                        forced_control_request(forced_control_transfer, splint_id, incarnation)
                    else {
                        continue;
                    };
                    active_controller = match control.request(request).await {
                        Ok(Response::ControlGranted { controller_id, .. }) => Some(controller_id),
                        Err(error) if handoff_refused(&error) => continue,
                        Err(error) => return Err(error),
                        Ok(_) => bail!("splinterd did not grant forced control"),
                    };
                    let _ = outputs.updates.send(WindowUpdate::Control(true)).await;
                    apply_requested_pane_resize(
                        &mut control,
                        active_controller,
                        requested_resize,
                        splint_id,
                        incarnation,
                    )
                    .await?;
                    continue;
                }
                WindowCommand::Search {
                    terminal_revision,
                    history_generation,
                    query,
                    case_sensitive,
                    cursor,
                } => {
                    match control
                        .request(Request::SearchScrollback {
                            splint_id,
                            incarnation,
                            terminal_revision,
                            history_generation,
                            query,
                            case_sensitive,
                            cursor,
                            max_results: splinterm_protocol::MAX_SEARCH_RESULTS,
                        })
                        .await?
                    {
                        Response::SearchResults { page, .. } => {
                            let _ = outputs
                                .updates
                                .send(WindowUpdate::SearchResults(page))
                                .await;
                        }
                        Response::SearchResyncRequired { .. } => {
                            let _ = outputs
                                .updates
                                .send(WindowUpdate::SearchResyncRequired)
                                .await;
                            let _ = outputs.resyncs.send(()).await;
                        }
                        _ => bail!("splinterd did not return search results"),
                    }
                    continue;
                }
                WindowCommand::ReleaseControl => {
                    let Some(controller_id) = active_controller.take() else {
                        continue;
                    };
                    let _ = outputs.updates.send(WindowUpdate::Control(false)).await;
                    Request::ReleaseControl { controller_id }
                }
            };
            let expects_terminal_action =
                matches!(&request, Request::Input { .. } | Request::Resize { .. });
            let expects_transfer_decision =
                matches!(&request, Request::DecideControlTransfer { .. });
            let response = match control.request(request).await {
                Ok(response) => response,
                // Fenced before this pane heard the pause: dropped.
                Err(error) if handoff_refused(&error) => continue,
                Err(error) => return Err(error),
            };
            let acknowledged = if expects_terminal_action {
                terminal_action_matches(&response, splint_id, incarnation)
            } else if expects_transfer_decision {
                matches!(response, Response::ControlTransferDecided { .. })
            } else {
                matches!(response, Response::Acknowledged)
            };
            if !acknowledged {
                bail!("splinterd did not acknowledge a window control command");
            }
        }
        Ok(())
    }
    .await;
    if let Some(controller_id) = active_controller {
        let _ = control.release_control(controller_id).await;
    }
    // A pane closed or cancelled withdraws its handoff report when `outputs`
    // drops. A failed connection while a handoff is preparing or fenced
    // instead starts the Window's reconnection: that is how a committed
    // handoff's exec looks from here, even before `Fenced` was read.
    if result.is_err() && !cancellation.is_cancelled() {
        outputs.handoff.connection_lost();
    }
    result
}

fn forced_control_request(
    capability: ForcedControlTransfer,
    splint_id: SplintId,
    incarnation: u64,
) -> Option<Request> {
    (capability == ForcedControlTransfer::Enabled).then_some(Request::ForceControlTransfer {
        splint_id,
        incarnation,
    })
}

/// Bounds the explicit detach a closing tab waits for.
const PANE_DETACH_DEADLINE: Duration = Duration::from_secs(2);

pub(in crate::app) struct PaneTask {
    pub(in crate::app) cancellation: tokio_util::sync::CancellationToken,
    pub(in crate::app) task: tokio::task::JoinHandle<Result<()>>,
    /// Set before cancelling to detach explicitly, so the daemon has released
    /// the attachment by the time the task ends.
    pub(in crate::app) detach_on_cancel: Arc<AtomicBool>,
}

impl PaneTask {
    fn new(
        cancellation: tokio_util::sync::CancellationToken,
        task: tokio::task::JoinHandle<Result<()>>,
        detach_on_cancel: Arc<AtomicBool>,
    ) -> Self {
        Self {
            cancellation,
            task,
            detach_on_cancel,
        }
    }
}

pub(in crate::app) struct PreparedPane {
    pub(in crate::app) options: WindowPaneOptions,
    pub(in crate::app) task: PaneTask,
}

pub(in crate::app) fn pane_claims_initial_control(
    splint_id: SplintId,
    default_focus: SplintId,
) -> bool {
    splint_id == default_focus
}

pub(in crate::app) fn layout_splint_ids(root: &LayoutNode, ids: &mut Vec<SplintId>) {
    match root {
        LayoutNode::Leaf(splint) => ids.push(splint.id),
        LayoutNode::Branch { first, second, .. } => {
            layout_splint_ids(first, ids);
            layout_splint_ids(second, ids);
        }
    }
}

pub(in crate::app) fn optional_pane_controller(result: Result<u64>) -> Result<Option<u64>> {
    match result {
        Ok(controller_id) => Ok(Some(controller_id)),
        Err(error)
            if protocol_error(&error).is_some_and(|error| {
                matches!(
                    error.code,
                    ErrorCode::ControllerUnavailable | ErrorCode::Unauthorized
                )
            }) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Presents a relaunched Window's resume ticket once for this pane's Splint
/// on its control connection (Plan 0061 M3.16), when the Window holds one.
///
/// Returns the restored controller, or `None` when the Window holds no
/// ticket for the Splint or the daemon restored nothing: the pane then takes
/// control the ordinary way. Any daemon refusal is a fallback with a short
/// notice, never a failure; only a broken connection fails the pane, as it
/// would at `AcquireControl`.
pub(in crate::app) async fn restore_handoff_control(
    restore: &splinterm::handoff_control::HandoffControlRestore,
    control: &mut Connection,
    splint_id: SplintId,
    incarnation: u64,
) -> Result<Option<u64>> {
    let Some(ticket) = restore.take(splint_id) else {
        return Ok(None);
    };
    match control
        .claim_handoff_control(ticket, splint_id, incarnation)
        .await
    {
        Ok(restored) => Ok(restored),
        Err(error) => match protocol_error(&error) {
            Some(refusal) => {
                eprintln!(
                    "splinterm: this pane's control was not restored after the upgrade ({:?}); it takes control the ordinary way",
                    refusal.code
                );
                Ok(None)
            }
            None => Err(error),
        },
    }
}

/// A new pane's controller: in a Window relaunched across a daemon upgrade,
/// the pane's pre-upgrade controller restored with the Window's ticket
/// first; otherwise, when the pane claims control at start, an ordinary
/// `AcquireControl`.
async fn initial_pane_controller(
    factory: &ConnectionFactory,
    control: &mut Connection,
    splint_id: SplintId,
    incarnation: u64,
    claim_control: bool,
) -> Result<Option<u64>> {
    let restored =
        restore_handoff_control(factory.handoff_control(), control, splint_id, incarnation).await?;
    if restored.is_some() || !claim_control {
        return Ok(restored);
    }
    optional_pane_controller(
        control
            .acquire_control(
                splint_id,
                incarnation,
                vec![ControlMode::Input, ControlMode::Resize],
            )
            .await,
    )
}

pub(in crate::app) fn pane_access_scopes() -> Vec<AccessScope> {
    vec![AccessScope::Observe, AccessScope::Scrollback]
}

pub(in crate::app) const fn terminal_grid_limits(
    server_limits: ServerLimits,
) -> TerminalGridLimits {
    TerminalGridLimits {
        maximum_columns: server_limits.maximum_columns,
        maximum_rows: server_limits.maximum_rows,
    }
}

pub(in crate::app) async fn prepare_live_pane(
    factory: &ConnectionFactory,
    config: &AppConfig,
    splint_id: SplintId,
    image_cache: SharedImageContentCache,
    claim_control: bool,
) -> Result<PreparedPane> {
    prepare_live_pane_at_incarnation(factory, config, splint_id, None, image_cache, claim_control)
        .await
}

pub(in crate::app) async fn prepare_live_pane_at_incarnation(
    factory: &ConnectionFactory,
    config: &AppConfig,
    splint_id: SplintId,
    expected_incarnation: Option<u64>,
    image_cache: SharedImageContentCache,
    claim_control: bool,
) -> Result<PreparedPane> {
    let mut connection = factory.connect().await?;
    let terminal_grid_limits = terminal_grid_limits(connection.limits());
    let incarnation = connection.live_incarnation(splint_id).await?;
    anyhow::ensure!(
        expected_incarnation.is_none_or(|expected| expected == incarnation),
        "selected Splint incarnation changed before attachment"
    );
    let scopes = pane_access_scopes();
    if !matches!(
        connection
            .request(Request::RequestAccess {
                splint_id,
                incarnation,
                scopes,
            })
            .await?,
        Response::AccessGranted { .. }
    ) {
        bail!("splinterd did not grant requested terminal access");
    }
    let authority = load_authority_status(&mut connection, splint_id, incarnation).await?;
    let attachment = attach(&mut connection, splint_id, incarnation).await?;
    let snapshot = attachment.snapshot.clone();
    resolve_image_contents(
        factory.capabilities().image_transport,
        &mut connection,
        &snapshot,
        &image_cache,
    )
    .await?;
    let image_sources = lease_snapshot_images(&image_cache, &snapshot)?;
    let mut control = factory.connect().await?;
    if control.live_incarnation(splint_id).await? != incarnation {
        bail!("control connection observed a different process incarnation");
    }
    // After the pane's own resnapshot above.
    let controller_id =
        initial_pane_controller(factory, &mut control, splint_id, incarnation, claim_control)
            .await?;
    let (updates, receiver) = mpsc::channel(WINDOW_UPDATE_QUEUE);
    let (command_sender, commands) = mpsc::channel(WINDOW_COMMAND_QUEUE);
    let (resync_sender, resyncs) = mpsc::channel(1);
    let controller_updates = updates.clone();
    let resize_delay_ms = config.resize_delay_ms;
    let cancellation = tokio_util::sync::CancellationToken::new();
    let controller = tokio::spawn(run_controller(
        control,
        commands,
        ControllerOutputs {
            updates: controller_updates,
            resyncs: resync_sender,
            handoff: factory.handoff_status().source(),
        },
        controller_id,
        splint_id,
        incarnation,
        factory.capabilities().forced_control_transfer,
        resize_delay_ms,
        cancellation.clone(),
    ));
    let task_updates = updates.clone();
    let detach_on_cancel = Arc::new(AtomicBool::new(false));
    let task = tokio::spawn(run_pane_subscription(
        connection,
        attachment,
        controller,
        resyncs,
        task_updates,
        splint_id,
        incarnation,
        factory.capabilities().image_transport,
        image_cache.clone(),
        cancellation.clone(),
        Arc::clone(&detach_on_cancel),
    ));
    Ok(PreparedPane {
        options: WindowPaneOptions {
            snapshot,
            terminal_grid_limits,
            updates: receiver,
            commands: command_sender,
            authority,
            controlled: controller_id.is_some(),
            image_sources,
        },
        task: PaneTask::new(cancellation, task, detach_on_cancel),
    })
}

pub(in crate::app) fn lease_snapshot_images(
    cache: &SharedImageContentCache,
    snapshot: &TerminalSnapshot,
) -> Result<ImageContentLeaseSet> {
    snapshot.images.as_ref().map_or_else(
        || Ok(ImageContentLeaseSet::default()),
        |images| cache.lease(&images.contents),
    )
}

pub(in crate::app) fn lease_update_images(
    cache: &SharedImageContentCache,
    update: &TerminalUpdate,
) -> Result<Option<ImageContentLeaseSet>> {
    update
        .images
        .as_ref()
        .map(|images| cache.lease(&images.contents))
        .transpose()
}

fn ensure_image_transport(transport: ImageTransport, metadata_present: bool) -> Result<()> {
    if transport == ImageTransport::Unavailable && metadata_present {
        bail!("remote endpoint supplied forbidden terminal image metadata");
    }
    Ok(())
}

pub(in crate::app) async fn resolve_image_contents(
    transport: ImageTransport,
    connection: &mut Connection,
    snapshot: &TerminalSnapshot,
    cache: &SharedImageContentCache,
) -> Result<()> {
    ensure_image_transport(transport, snapshot.images.is_some())?;
    let Some(images) = &snapshot.images else {
        return Ok(());
    };
    let cancellation = tokio_util::sync::CancellationToken::new();
    for metadata in &images.contents {
        if !cache.contains(metadata)? {
            let source = connection
                .image_content_source(
                    snapshot.splint_id,
                    snapshot.incarnation,
                    metadata,
                    &cancellation,
                )
                .await?;
            cache.insert_source(metadata, source)?;
        }
    }
    Ok(())
}

pub(in crate::app) async fn resolve_update_images(
    transport: ImageTransport,
    connection: &mut Connection,
    update: &TerminalUpdate,
    splint_id: SplintId,
    incarnation: u64,
    cache: &SharedImageContentCache,
) -> Result<()> {
    ensure_image_transport(transport, update.images.is_some())?;
    let Some(images) = &update.images else {
        return Ok(());
    };
    let cancellation = tokio_util::sync::CancellationToken::new();
    for metadata in &images.contents {
        if !cache.contains(metadata)? {
            let source = connection
                .image_content_source(splint_id, incarnation, metadata, &cancellation)
                .await?;
            cache.insert_source(metadata, source)?;
        }
    }
    Ok(())
}

fn report_pane_controller_result(result: Result<Result<()>, tokio::task::JoinError>) {
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => eprintln!("splinterm pane controller stopped: {error:#}"),
        Err(error) => eprintln!("splinterm pane controller task failed: {error}"),
    }
}

async fn finish_pane_controller(
    controller: &mut tokio::task::JoinHandle<Result<()>>,
    completed: bool,
) -> Result<()> {
    if !completed {
        report_pane_controller_result(controller.await);
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "subscription ordering and controller resynchronization form one pane lifecycle"
)]
pub(in crate::app) async fn run_pane_subscription(
    mut connection: Connection,
    mut attachment: Attachment,
    mut controller: tokio::task::JoinHandle<Result<()>>,
    mut resyncs: mpsc::Receiver<()>,
    updates: mpsc::Sender<WindowUpdate>,
    splint_id: SplintId,
    incarnation: u64,
    image_transport: ImageTransport,
    image_cache: SharedImageContentCache,
    cancellation: tokio_util::sync::CancellationToken,
    detach_on_cancel: Arc<AtomicBool>,
) -> Result<()> {
    let mut last_revision = attachment.snapshot.revision;
    let mut last_sequence = 0_u64;
    let mut controller_completed = false;
    loop {
        tokio::select! {
            () = cancellation.cancelled() => {
                if detach_on_cancel.load(Ordering::Acquire) {
                    let _ = connection
                        .request_with_deadline(
                            Request::Detach {
                                subscription_id: attachment.subscription_id,
                            },
                            PANE_DETACH_DEADLINE,
                        )
                        .await;
                }
                return finish_pane_controller(&mut controller, controller_completed).await;
            }
            result = &mut controller, if !controller_completed => {
                report_pane_controller_result(result);
                controller_completed = true;
                let _ = updates.send(WindowUpdate::Control(false)).await;
            }
            Some(()) = resyncs.recv() => {
                attachment = resynchronize(
                    &mut connection, attachment.subscription_id, splint_id, incarnation,
                ).await?;
                last_revision = attachment.snapshot.revision;
                last_sequence = 0;
                resolve_image_contents(
                    image_transport, &mut connection, &attachment.snapshot, &image_cache,
                ).await?;
                let image_sources = lease_snapshot_images(&image_cache, &attachment.snapshot)?;
                if updates.send(WindowUpdate::Snapshot {
                    snapshot: attachment.snapshot.clone(),
                    image_sources,
                    authoritative: true,
                }).await.is_err() {
                    return finish_pane_controller(&mut controller, controller_completed).await;
                }
            }
            frame = connection.next_server_frame() => {
                let ServerFrame::Event { subscription_id, sequence, event } = frame? else {
                    bail!("splinterd sent an unexpected frame while subscribed");
                };
                match classify_subscription_event(
                    attachment.subscription_id, last_sequence, subscription_id, sequence, event,
                ) {
                    EventAction::Ignore => {}
                    EventAction::Skip { sequence } => last_sequence = sequence,
                    EventAction::Snapshot { sequence, snapshot } => {
                        validate_attached_snapshot(&snapshot, splint_id, incarnation)?;
                        last_revision = snapshot.revision;
                        resolve_image_contents(
                            image_transport, &mut connection, &snapshot, &image_cache,
                        ).await?;
                        let image_sources = lease_snapshot_images(&image_cache, &snapshot)?;
                        if updates.send(WindowUpdate::Snapshot {
                            snapshot,
                            image_sources,
                            authoritative: false,
                        }).await.is_err() {
                            return finish_pane_controller(&mut controller, controller_completed).await;
                        }
                        last_sequence = sequence;
                    }
                    EventAction::Update { sequence, update }
                        if update_advances_from(&update, last_revision) => {
                        if perf_trace_enabled() {
                            emit_perf_trace(
                                "splinterm",
                                "client_receive",
                                PerfTraceEvent {
                                    splint_id: Some(splint_id),
                                    incarnation: Some(incarnation),
                                    base_revision: Some(update.base_revision),
                                    revision: Some(update.revision),
                                    subscription_id: Some(attachment.subscription_id),
                                    transaction_sequence: Some(sequence),
                                    rows: Some(u64::try_from(update.rows.len()).unwrap_or(u64::MAX)),
                                    count: Some(1),
                                    ..PerfTraceEvent::default()
                                },
                            );
                        }
                        last_revision = update.revision;
                        resolve_update_images(
                            image_transport,
                            &mut connection,
                            &update,
                            splint_id,
                            incarnation,
                            &image_cache,
                        ).await?;
                        let image_sources = lease_update_images(&image_cache, &update)?;
                        let base_revision = update.base_revision;
                        let revision = update.revision;
                        let queue_depth = updates.max_capacity().saturating_sub(updates.capacity());
                        let enqueue_started = perf_trace_enabled().then(Instant::now);
                        if updates
                            .send(WindowUpdate::Update {
                                update,
                                image_sources,
                                trace: perf_trace_enabled().then_some(PerfTraceCorrelation {
                                    base_revision,
                                    revision,
                                    subscription_id: attachment.subscription_id,
                                    transaction_sequence: sequence,
                                }),
                            })
                            .await
                            .is_err()
                        {
                            return finish_pane_controller(&mut controller, controller_completed).await;
                        }
                        if let Some(started) = enqueue_started {
                            emit_perf_trace(
                                "splinterm",
                                "client_enqueue",
                                PerfTraceEvent {
                                    splint_id: Some(splint_id),
                                    incarnation: Some(incarnation),
                                    base_revision: Some(base_revision),
                                    revision: Some(revision),
                                    subscription_id: Some(attachment.subscription_id),
                                    transaction_sequence: Some(sequence),
                                    duration_ns: Some(
                                        u64::try_from(started.elapsed().as_nanos())
                                            .unwrap_or(u64::MAX),
                                    ),
                                    queue_depth: Some(
                                        u64::try_from(queue_depth).unwrap_or(u64::MAX),
                                    ),
                                    ..PerfTraceEvent::default()
                                },
                            );
                        }
                        last_sequence = sequence;
                    }
                    EventAction::Update { .. } | EventAction::Resynchronize => {
                        attachment = resynchronize(
                            &mut connection, attachment.subscription_id, splint_id, incarnation,
                        ).await?;
                        last_revision = attachment.snapshot.revision;
                        last_sequence = 0;
                        resolve_image_contents(
                            image_transport, &mut connection, &attachment.snapshot, &image_cache,
                        ).await?;
                        let image_sources = lease_snapshot_images(&image_cache, &attachment.snapshot)?;
                        if updates.send(WindowUpdate::Snapshot {
                            snapshot: attachment.snapshot.clone(),
                            image_sources,
                            authoritative: true,
                        }).await.is_err() {
                            return finish_pane_controller(&mut controller, controller_completed).await;
                        }
                    }
                    EventAction::Exited => {
                        let _ = updates.send(WindowUpdate::Exited { splint_id }).await;
                        return finish_pane_controller(&mut controller, controller_completed).await;
                    }
                    EventAction::Shutdown => {
                        let _ = updates.send(WindowUpdate::Shutdown).await;
                        return finish_pane_controller(&mut controller, controller_completed).await;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "pane_control_tests.rs"]
mod control_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_image_transport_fails_closed_on_metadata() {
        assert!(ensure_image_transport(ImageTransport::Unavailable, false).is_ok());
        assert!(ensure_image_transport(ImageTransport::Unavailable, true).is_err());
        assert!(ensure_image_transport(ImageTransport::LocalTrusted, true).is_ok());
    }

    #[test]
    fn pane_attachment_access_does_not_preemptively_require_interactive_policy() {
        assert_eq!(
            pane_access_scopes(),
            vec![AccessScope::Observe, AccessScope::Scrollback]
        );
    }

    #[test]
    fn endpoint_terminal_limits_preserve_smaller_negotiated_dimensions() {
        let limits = terminal_grid_limits(ServerLimits {
            maximum_columns: 120,
            maximum_rows: 64,
            ..ServerLimits::default()
        });
        assert_eq!(limits.maximum_columns, 120);
        assert_eq!(limits.maximum_rows, 64);
    }

    #[test]
    fn terminal_input_is_chunked_to_the_negotiated_server_limit() {
        let splint_id = SplintId::new();
        let bytes = vec![7; 17];
        let requests = input_requests(3, splint_id, 9, &bytes, false, 8).unwrap();

        assert_eq!(requests.len(), 3);
        let chunks = requests
            .into_iter()
            .map(|request| match request {
                Request::Input {
                    controller_id,
                    splint_id: request_splint_id,
                    incarnation,
                    bytes,
                    focus_report,
                } => {
                    assert_eq!(controller_id, 3);
                    assert_eq!(request_splint_id, splint_id);
                    assert_eq!(incarnation, 9);
                    assert!(!focus_report);
                    bytes
                }
                _ => panic!("expected an input request"),
            })
            .collect::<Vec<_>>();
        assert_eq!(chunks.iter().map(Vec::len).collect::<Vec<_>>(), [8, 8, 1]);
        assert_eq!(chunks.concat(), bytes);
        assert!(input_requests(3, splint_id, 9, &bytes, false, 0).is_err());
    }

    #[test]
    fn only_generated_focus_reports_are_flagged() {
        assert_eq!(
            terminal_input(WindowCommand::FocusReport(true)),
            Some((b"\x1b[I".to_vec(), true))
        );
        assert_eq!(
            terminal_input(WindowCommand::FocusReport(false)),
            Some((b"\x1b[O".to_vec(), true))
        );
        assert_eq!(
            terminal_input(WindowCommand::Input(b"\x1b[I".to_vec())),
            Some((b"\x1b[I".to_vec(), false))
        );
        assert_eq!(terminal_input(WindowCommand::Resynchronize), None);
    }

    fn coalesce_from(
        first: &[u8],
        limit: usize,
        queued: Vec<WindowCommand>,
    ) -> (Vec<u8>, Option<WindowCommand>, Vec<WindowCommand>) {
        let mut queue = std::collections::VecDeque::from(queued);
        let (bytes, barrier) = coalesce_input(first.to_vec(), limit, || queue.pop_front());
        (bytes, barrier, queue.into())
    }

    #[test]
    fn single_input_passes_through_unchanged() {
        assert_eq!(
            coalesce_from(b"x", 8, Vec::new()),
            (b"x".to_vec(), None, Vec::new())
        );
    }

    #[test]
    fn consecutive_input_is_coalesced_in_order_and_stops_at_a_barrier() {
        let resize = WindowCommand::Resize {
            columns: 80,
            rows: 24,
            pixel_width: 800,
            pixel_height: 480,
        };
        let (bytes, barrier, rest) = coalesce_from(
            b"a",
            64,
            vec![
                WindowCommand::Input(b"bc".to_vec()),
                WindowCommand::Input(Vec::new()),
                WindowCommand::Input(b"d".to_vec()),
                resize.clone(),
                WindowCommand::Input(b"e".to_vec()),
            ],
        );
        assert_eq!(bytes, b"abcd");
        assert_eq!(barrier, Some(resize));
        assert_eq!(rest, vec![WindowCommand::Input(b"e".to_vec())]);

        for barrier_command in [
            WindowCommand::Resynchronize,
            WindowCommand::ReleaseControl,
            WindowCommand::RequestControlTransfer,
        ] {
            let (bytes, barrier, rest) = coalesce_from(
                b"a",
                64,
                vec![barrier_command.clone(), WindowCommand::Input(b"b".to_vec())],
            );
            assert_eq!(bytes, b"a");
            assert_eq!(barrier, Some(barrier_command));
            assert_eq!(rest, vec![WindowCommand::Input(b"b".to_vec())]);
        }
    }

    #[test]
    fn coalescing_fills_exactly_to_the_input_bound_and_never_past_it() {
        // Exactly reaching the bound is allowed and stops further reads.
        let (bytes, barrier, rest) = coalesce_from(
            b"abc",
            8,
            vec![
                WindowCommand::Input(b"defgh".to_vec()),
                WindowCommand::Input(b"i".to_vec()),
            ],
        );
        assert_eq!(bytes, b"abcdefgh");
        assert_eq!(barrier, None);
        assert_eq!(rest, vec![WindowCommand::Input(b"i".to_vec())]);

        // One byte over the bound is handed back intact for its own request.
        let (bytes, barrier, rest) = coalesce_from(
            b"abc",
            8,
            vec![
                WindowCommand::Input(b"defghi".to_vec()),
                WindowCommand::Input(b"j".to_vec()),
            ],
        );
        assert_eq!(bytes, b"abc");
        assert_eq!(barrier, Some(WindowCommand::Input(b"defghi".to_vec())));
        assert_eq!(rest, vec![WindowCommand::Input(b"j".to_vec())]);

        // An oversized first command is not merged; input_requests splits it.
        let (bytes, barrier, rest) =
            coalesce_from(&[7; 9], 8, vec![WindowCommand::Input(b"z".to_vec())]);
        assert_eq!(bytes, vec![7; 9]);
        assert_eq!(barrier, None);
        assert_eq!(rest, vec![WindowCommand::Input(b"z".to_vec())]);
        let requests = input_requests(1, SplintId::new(), 1, &bytes, false, 8).unwrap();
        assert_eq!(requests.len(), 2);
    }

    #[test]
    fn empty_input_coalesces_without_sending_bytes() {
        let (bytes, barrier, rest) = coalesce_from(b"", 8, vec![WindowCommand::Input(Vec::new())]);
        assert!(bytes.is_empty());
        assert_eq!(barrier, None);
        assert!(rest.is_empty());
        assert!(
            input_requests(1, SplintId::new(), 1, &bytes, false, 8)
                .unwrap()
                .is_empty()
        );
        // A zero bound reads nothing; input_requests still rejects the bound.
        let (bytes, barrier, rest) =
            coalesce_from(b"a", 0, vec![WindowCommand::Input(b"b".to_vec())]);
        assert_eq!((bytes, barrier), (b"a".to_vec(), None));
        assert_eq!(rest.len(), 1);
    }

    #[test]
    fn input_resource_saturation_is_recoverable() {
        let error = response_protocol_error(splinterm_protocol::ProtocolError::new(
            ErrorCode::ResourceLimit,
            "input queue limit exceeded",
        ));
        assert!(recoverable_input_error(&error));

        let error = response_protocol_error(splinterm_protocol::ProtocolError::new(
            ErrorCode::Internal,
            "operation failed",
        ));
        assert!(!recoverable_input_error(&error));
    }

    /// Plan 0061 M3.12b: a reconnecting Window gives up at once when the
    /// daemon no longer trusts its client build, rather than retrying.
    #[test]
    fn only_an_unauthorized_handshake_marks_the_client_untrusted() {
        let untrusted = response_protocol_error(splinterm_protocol::ProtocolError::new(
            ErrorCode::Unauthorized,
            "trusted UI role requires the installed graphical client",
        ));
        assert!(refused_as_untrusted(&untrusted));
        let fenced = response_protocol_error(splinterm_protocol::ProtocolError::new(
            ErrorCode::HandoffInProgress,
            "daemon handoff in progress",
        ));
        assert!(!refused_as_untrusted(&fenced));
        let refused =
            anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
        assert!(
            !refused_as_untrusted(&refused),
            "an adopting daemon is retried"
        );
    }

    #[test]
    fn remote_forced_control_transfer_is_rejected_before_request_construction() {
        let splint_id = SplintId::new();
        assert_eq!(
            forced_control_request(ForcedControlTransfer::Disabled, splint_id, 7),
            None
        );
        assert_eq!(
            forced_control_request(ForcedControlTransfer::Enabled, splint_id, 7),
            Some(Request::ForceControlTransfer {
                splint_id,
                incarnation: 7,
            })
        );
    }
}
