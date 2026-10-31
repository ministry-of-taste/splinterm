//! Graphical window lifecycle orchestration.

use std::{collections::HashMap, env, future::Future, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use splinterm::{
    PerfTraceCorrelation, PhaseEnd, WindowCommand, WindowOptions, WindowSurface, WindowThemeColors,
    WindowUpdate,
    automation::{
        Connection, DaemonChange, DaemonIdentity, MAX_RENDERER_IMAGE_RESIDENT_BYTES,
        SharedImageContentCache, is_connection_loss, retry_until,
    },
    config::{AppConfig, FontAuthority, TopologyReloadSettings},
    endpoint::{
        ConnectionFactory, ForcedControlTransfer, GraphicalFocusPublication, ImageTransport,
        LaunchSemantics,
    },
    handoff_status::{RECONNECT_WINDOW, WindowHandoffPhase},
    renderer::{self, RendererOptions},
    run_window, run_window_on,
    window_continuation::{
        ContinuationContext, SystemExec, WindowLayoutLedger, installed_client_path,
        run_window_anchor,
    },
};
use splinterm_core::SplintId;
use splinterm_protocol::{
    ControlMode, Request, Response, ServerFrame,
    perf_trace::{PerfTraceEvent, emit_perf_trace, perf_trace_enabled},
};
use tokio::sync::{mpsc, watch};

use super::{
    font_watch::{FontUpdateSink, watch_font},
    pane_bridge::{
        Attachment, ControllerOutputs, EventAction, UNTRUSTED_AFTER_UPGRADE, WINDOW_COMMAND_QUEUE,
        WINDOW_UPDATE_QUEUE, attach, classify_subscription_event, drive_controller,
        handoff_refused, layout_splint_ids, lease_snapshot_images, lease_update_images,
        load_authority_status, optional_pane_controller, pane_access_scopes,
        pane_claims_initial_control, prepare_live_pane, refused_as_untrusted,
        resolve_image_contents, resolve_update_images, resynchronize, terminal_grid_limits,
        unless_splint_gone, update_advances_from, validate_attached_snapshot,
    },
    remote_consent::spawn_remote_consent_responder,
    theme_watch::{ThemeOverrides, ThemeUpdateSink, load_startup_theme_for_watch, watch_theme},
    topology_manager::{
        TopologyRestore, initial_window_dojo_identity, run_topology_manager, spawn_topology_smoke,
    },
};

struct AbortOnDropTask(tokio::task::JoinHandle<()>);

impl AbortOnDropTask {
    fn new(task: tokio::task::JoinHandle<()>) -> Self {
        Self(task)
    }

    fn abort(&self) {
        self.0.abort();
    }
}

impl Drop for AbortOnDropTask {
    fn drop(&mut self) {
        self.abort();
    }
}

/// How often the focus reporter looks whether its Window reconnected.
const FOCUS_RECONNECT_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// How many times the focus reporter reconnects after finding its connection
/// gone with no reconnection running.
const FOCUS_LATE_RECONNECTS: u32 = 3;

/// Publishes the Window's graphical focus on one connection until the
/// Window closes (`Ok`) or the connection fails.
async fn publish_graphical_focus(
    factory: &ConnectionFactory,
    updates: &mut watch::Receiver<Option<SplintId>>,
) -> Result<()> {
    let mut connection = factory.connect().await?;
    loop {
        let focused_splint_id = *updates.borrow_and_update();
        match connection
            .request(Request::PublishGraphicalFocus { focused_splint_id })
            .await
        {
            Ok(Response::Acknowledged) => {}
            Ok(_) => bail!("splinterd rejected graphical focus publication"),
            // Fenced by a handoff: this focus is dropped, the next is sent.
            Err(error) if handoff_refused(&error) => {}
            Err(error) => return Err(error),
        }
        if updates.changed().await.is_err() {
            return Ok(());
        }
    }
}

/// Publishes graphical focus for the Window's lifetime. A connection lost
/// during a handoff comes back once the Window has reconnected, and the
/// current focus is published again (Plan 0061 M3.12b).
async fn run_graphical_focus_reporter(
    factory: ConnectionFactory,
    mut updates: watch::Receiver<Option<SplintId>>,
) -> Result<()> {
    let hub = factory.handoff_status().clone();
    let mut late_losses = 0_u32;
    loop {
        let error = match publish_graphical_focus(&factory, &mut updates).await {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        if !is_connection_loss(&error) {
            return Err(error);
        }
        if !hub.begin_reconnect() {
            // The reporter publishes only on a focus change, so it can find
            // its connection gone after the Window already reconnected. It
            // connects again a few times; a daemon that is gone fails the
            // connect and ends it.
            late_losses += 1;
            if late_losses > FOCUS_LATE_RECONNECTS {
                return Err(error);
            }
            tokio::time::sleep(FOCUS_RECONNECT_POLL).await;
            continue;
        }
        loop {
            tokio::time::sleep(FOCUS_RECONNECT_POLL).await;
            if updates.has_changed().is_err() {
                return Ok(());
            }
            match hub.view().phase {
                // A participating Window waits for its outcome: it is either
                // relaunched, or reconnects once the attempt ends (M3.14).
                WindowHandoffPhase::Reconnecting | WindowHandoffPhase::Continuing => {}
                WindowHandoffPhase::Lost => return Err(error),
                WindowHandoffPhase::Idle
                | WindowHandoffPhase::Preparing
                | WindowHandoffPhase::Fenced => break,
            }
        }
    }
}

/// A single-pane Window's controller task. It hands its command queue back
/// when it ends, so a reconnection can give the queue the frontend sends to
/// to the next controller.
type SinglePaneController = tokio::task::JoinHandle<(Result<()>, mpsc::Receiver<WindowCommand>)>;

/// Starts a single-pane Window's controller; returns it with its resync
/// requests and its cancellation.
#[allow(
    clippy::too_many_arguments,
    reason = "the controller's identity, authority, and queues are passed through"
)]
fn spawn_single_pane_controller(
    factory: &ConnectionFactory,
    control: Connection,
    commands: mpsc::Receiver<WindowCommand>,
    updates: mpsc::Sender<WindowUpdate>,
    controller_id: Option<u64>,
    splint_id: SplintId,
    incarnation: u64,
    resize_delay_ms: u64,
) -> (
    SinglePaneController,
    mpsc::Receiver<()>,
    tokio_util::sync::CancellationToken,
) {
    let (resync_sender, resyncs) = mpsc::channel(1);
    let cancellation = tokio_util::sync::CancellationToken::new();
    let outputs = ControllerOutputs {
        updates,
        resyncs: resync_sender,
        handoff: factory.handoff_status().source(),
        forces: factory.forces_in_flight().clone(),
    };
    let forced_control_transfer = factory.capabilities().forced_control_transfer;
    let token = cancellation.clone();
    let task = tokio::spawn(async move {
        let mut commands = commands;
        let result = drive_controller(
            control,
            &mut commands,
            outputs,
            controller_id,
            splint_id,
            incarnation,
            forced_control_transfer,
            resize_delay_ms,
            token,
        )
        .await;
        (result, commands)
    });
    (task, resyncs, cancellation)
}

/// Drops every command still queued for a single-pane Window's controller;
/// returns how many. Commands sent before or during a reconnection would
/// answer a stale view or replay typed input (review S4).
fn drain_queued_commands(commands: &mut mpsc::Receiver<WindowCommand>) -> usize {
    let mut drained = 0;
    while commands.try_recv().is_ok() {
        drained += 1;
    }
    drained
}

async fn finish_controller(controller: SinglePaneController) -> Result<()> {
    controller.await.context("window controller task failed")?.0
}

/// A single-pane Window's Splint reattached on a fresh connection.
struct SinglePaneAttachment {
    connection: Connection,
    attachment: Attachment,
    control: Connection,
    controller_id: Option<u64>,
    daemon: DaemonIdentity,
}

/// What one reattachment attempt found.
enum Reattach {
    Attached(Box<SinglePaneAttachment>),
    /// The Splint's process ended, or another took its place, meanwhile.
    Ended,
    /// A different daemon answers: not a continuation.
    Restarted,
    /// The daemon refused this client build as no longer the installed one.
    Untrusted,
}

/// Attaches the single-pane Window's Splint again after its daemon
/// connection was lost during a handoff, with fresh connection-owned IDs.
async fn reattach_single_pane(
    factory: &ConnectionFactory,
    splint_id: SplintId,
    incarnation: u64,
    image_transport: ImageTransport,
    image_cache: &SharedImageContentCache,
    before: DaemonIdentity,
) -> Result<Reattach> {
    let mut connection = match factory.connect().await {
        Err(error) if refused_as_untrusted(&error) => return Ok(Reattach::Untrusted),
        connection => connection?,
    };
    let daemon = DaemonIdentity::of(&connection);
    if before.change_to(daemon) == DaemonChange::Restarted {
        return Ok(Reattach::Restarted);
    }
    let Some(inspected) = unless_splint_gone(
        connection
            .request(Request::InspectSplint { splint_id })
            .await,
    )?
    else {
        return Ok(Reattach::Ended);
    };
    let Response::Splint { runtime, .. } = inspected else {
        bail!("splinterd did not return the selected Splint identity");
    };
    if runtime.splint_id != splint_id || runtime.live_incarnation != Some(incarnation) {
        return Ok(Reattach::Ended);
    }
    let Some(granted) = unless_splint_gone(
        connection
            .request(Request::RequestAccess {
                splint_id,
                incarnation,
                scopes: pane_access_scopes(),
            })
            .await,
    )?
    else {
        return Ok(Reattach::Ended);
    };
    if !matches!(granted, Response::AccessGranted { .. }) {
        bail!("splinterd did not grant requested terminal access");
    }
    let Some(attachment) =
        unless_splint_gone(attach(&mut connection, splint_id, incarnation).await)?
    else {
        return Ok(Reattach::Ended);
    };
    resolve_image_contents(
        image_transport,
        &mut connection,
        &attachment.snapshot,
        image_cache,
    )
    .await?;
    let mut control = factory.connect().await?;
    let Some(Response::Splint { runtime, .. }) =
        unless_splint_gone(control.request(Request::InspectSplint { splint_id }).await)?
    else {
        return Ok(Reattach::Ended);
    };
    if runtime.live_incarnation != Some(incarnation) {
        return Ok(Reattach::Ended);
    }
    let controller_id = optional_pane_controller(
        control
            .acquire_control(
                splint_id,
                incarnation,
                vec![ControlMode::Input, ControlMode::Resize],
            )
            .await,
    )?;
    Ok(Reattach::Attached(Box::new(SinglePaneAttachment {
        connection,
        attachment,
        control,
        controller_id,
        daemon,
    })))
}

/// How a single-pane Window's reconnection ended.
enum SinglePaneReconnect {
    Attached(Box<SinglePaneAttachment>),
    Ended,
    Lost,
    /// The Window closed meanwhile, with its result.
    WindowClosed(Result<()>),
}

/// Plan 0061 M3.12b: reattaches a single-pane Window for up to
/// [`RECONNECT_WINDOW`], with backoff. Commands the frontend sends meanwhile
/// (scrollback reads, say) are dropped: they would answer a stale view, and
/// a full queue would end the Window.
#[allow(
    clippy::too_many_arguments,
    reason = "the Window, its queue, and the Splint's identity are lent to one reconnection"
)]
async fn reconnect_single_pane(
    factory: &ConnectionFactory,
    window: &mut tokio::task::JoinHandle<Result<()>>,
    commands: &mut mpsc::Receiver<WindowCommand>,
    splint_id: SplintId,
    incarnation: u64,
    image_transport: ImageTransport,
    image_cache: &SharedImageContentCache,
    before: DaemonIdentity,
) -> Result<SinglePaneReconnect> {
    let deadline = tokio::time::Instant::now() + RECONNECT_WINDOW;
    let reattach = retry_until(deadline, || {
        reattach_single_pane(
            factory,
            splint_id,
            incarnation,
            image_transport,
            image_cache,
            before,
        )
    });
    tokio::pin!(reattach);
    loop {
        tokio::select! {
            biased;
            result = &mut *window => {
                return Ok(SinglePaneReconnect::WindowClosed(
                    result.context("Wayland window task failed")?,
                ));
            }
            outcome = &mut reattach => {
                return Ok(match outcome {
                    Ok(Reattach::Attached(attached)) => SinglePaneReconnect::Attached(attached),
                    Ok(Reattach::Ended) => SinglePaneReconnect::Ended,
                    Ok(Reattach::Untrusted) => {
                        eprintln!("{UNTRUSTED_AFTER_UPGRADE}");
                        factory.handoff_status().reconnect_failed();
                        SinglePaneReconnect::Lost
                    }
                    Ok(Reattach::Restarted) => {
                        eprintln!(
                            "splinterm: splinterd restarted during the upgrade; this Window keeps its last screen"
                        );
                        factory.handoff_status().reconnect_failed();
                        SinglePaneReconnect::Lost
                    }
                    Err(error) => {
                        eprintln!("splinterm: could not reconnect to splinterd: {error:#}");
                        factory.handoff_status().reconnect_failed();
                        SinglePaneReconnect::Lost
                    }
                });
            }
            Some(_) = commands.recv() => {}
        }
    }
}

fn spawn_graphical_focus_reporter(
    factory: ConnectionFactory,
    updates: watch::Receiver<Option<SplintId>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if run_graphical_focus_reporter(factory, updates)
            .await
            .is_err()
        {
            eprintln!("splinterm graphical focus reporter failed");
        }
    })
}

fn endpoint_graphical_focus(
    factory: &ConnectionFactory,
) -> (
    Option<watch::Sender<Option<SplintId>>>,
    Option<tokio::task::JoinHandle<()>>,
) {
    if factory.capabilities().graphical_focus_publication == GraphicalFocusPublication::Enabled {
        let (sender, updates) = watch::channel(None);
        (
            Some(sender),
            Some(spawn_graphical_focus_reporter(factory.clone(), updates)),
        )
    } else {
        (None, None)
    }
}

/// Starts a local managed Window's handoff anchor (Plan 0061 M3.14): its one
/// dedicated connection that declares `Hello.handoff_window`, receives the
/// daemon's prepare frame, and relaunches the Window across the upgrade.
/// Returns the ledger its frontend publishes tabs and focus to, and the
/// anchor task, which ends with the Window. A remote Window has neither: its
/// daemon's handoff does not concern this machine's client.
fn spawn_window_anchor(
    factory: &ConnectionFactory,
    app_id: Option<String>,
) -> (Option<WindowLayoutLedger>, Option<AbortOnDropTask>) {
    if !factory.is_local() {
        return (None, None);
    }
    let Ok(socket) = splinterm::automation::socket_path() else {
        return (None, None);
    };
    let ledger = WindowLayoutLedger::default();
    let context = ContinuationContext {
        hub: factory.handoff_status().clone(),
        ledger: ledger.clone(),
        app_id,
        installed_client: installed_client_path(),
        exec: Arc::new(SystemExec),
    };
    let anchor = AbortOnDropTask::new(tokio::spawn(run_window_anchor(context, socket)));
    (Some(ledger), Some(anchor))
}

fn pane_chrome_capture() -> Result<Option<PathBuf>> {
    let Some(path) = env::var_os("SPLINTERM_PANE_CHROME_CAPTURE") else {
        return Ok(None);
    };
    anyhow::ensure!(
        env::var_os("SPLINTERM_ENABLE_DEV_ATTACH").is_some(),
        "SPLINTERM_PANE_CHROME_CAPTURE requires development attach"
    );
    Ok(Some(PathBuf::from(path)))
}

fn finish_topology_manager(
    result: std::result::Result<Result<()>, tokio::task::JoinError>,
) -> Result<()> {
    result
        .context("topology manager task failed")?
        .context("topology manager stopped")
}

pub(super) fn resolved_window_app_id(app_id: Option<String>) -> String {
    app_id.unwrap_or_else(|| splinterm::config::APP_ID.to_owned())
}

/// Runs one Window phase: on `surface` when given, or else in a new Window. It
/// returns the parked Window when the phase kept its surface and the Window
/// was not closed. A plain phase on a new Window runs exactly as before.
pub(super) fn run_window_phase(
    surface: Option<WindowSurface>,
    options: WindowOptions,
) -> Result<Option<WindowSurface>> {
    if surface.is_none() && !options.keep_surface {
        run_window(options)?;
        return Ok(None);
    }
    Ok(match run_window_on(surface, options)? {
        PhaseEnd::Kept(surface) => Some(surface),
        PhaseEnd::Closed => None,
    })
}

/// What daemon work done while a staged Window is parked ended with.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum WhileParked<T> {
    Done(T),
    /// The parked Window closed first; the work, and any SSH connection it
    /// held, was dropped.
    Closed,
}

/// The bound on parked work for an endpoint without a profile timeout.
const PARKED_WORK_DEADLINE: Duration = Duration::from_secs(30);

/// How long daemon work may take while a staged Window waits for it: the
/// remote profile's connect timeout.
pub(super) fn parked_deadline(factory: &ConnectionFactory) -> Duration {
    factory
        .remote_operation_timeout()
        .unwrap_or(PARKED_WORK_DEADLINE)
}

/// Runs daemon `work` while a staged Window is parked, `closed` being that
/// Window's close signal. A close ends the work at once, and it fails after
/// `deadline`, so a stalled daemon never keeps an unmapped launcher and its
/// SSH connection alive. Without a parked Window the work runs as before.
///
/// # Errors
///
/// Returns the work's error, or an error when the deadline passes first.
pub(super) async fn while_parked<T>(
    closed: Option<impl Future<Output = ()>>,
    deadline: Duration,
    work: impl Future<Output = Result<T>>,
) -> Result<WhileParked<T>> {
    // Boxed so a launch future that waits on daemon work stays small.
    let work = Box::pin(work);
    let Some(closed) = closed else {
        return work.await.map(WhileParked::Done);
    };
    tokio::select! {
        biased;
        () = closed => Ok(WhileParked::Closed),
        result = tokio::time::timeout(deadline, work) => match result {
            Ok(result) => result.map(WhileParked::Done),
            Err(_) => bail!("the daemon did not answer in time"),
        },
    }
}

/// The close signal of a parked staged Window, if there is one.
pub(super) fn parked_close(
    surface: Option<&WindowSurface>,
) -> Option<impl Future<Output = ()> + use<>> {
    surface.map(|surface| surface.close_signal().closed())
}

pub(super) async fn run_live_multipane_window(
    config: AppConfig,
    dojo_model: splinterm_core::Dojo,
    factory: ConnectionFactory,
) -> Result<()> {
    run_live_multipane_window_inner(config, dojo_model, factory, None, None, true, None, None).await
}

/// What a Window relaunched across a daemon upgrade restores after its first
/// tab (Plan 0061 M3.15).
pub(super) struct WindowResume {
    /// The other tabs, in order, with their recorded focus.
    pub(super) tabs: Vec<(splinterm_core::DojoId, Option<SplintId>)>,
    /// The tab to activate once every tab is open.
    pub(super) active: splinterm_core::DojoId,
}

/// Runs a Window relaunched across a daemon upgrade: `dojo_model` is its
/// first surviving tab, and `resume` reopens the rest.
pub(super) async fn run_resumed_multipane_window(
    config: AppConfig,
    dojo_model: splinterm_core::Dojo,
    factory: ConnectionFactory,
    app_id: Option<String>,
    resume: WindowResume,
) -> Result<()> {
    run_live_multipane_window_inner(
        config,
        dojo_model,
        factory,
        None,
        app_id,
        true,
        Some(resume),
        None,
    )
    .await
}

pub(super) async fn run_live_multipane_window_with_app_id(
    config: AppConfig,
    dojo_model: splinterm_core::Dojo,
    factory: ConnectionFactory,
    app_id: Option<String>,
    initial_tab_strip_visible: bool,
    surface: Option<WindowSurface>,
) -> Result<()> {
    run_live_multipane_window_inner(
        config,
        dojo_model,
        factory,
        None,
        app_id,
        initial_tab_strip_visible,
        None,
        surface,
    )
    .await
}

pub(super) async fn run_owned_live_multipane_window_with_app_id(
    config: AppConfig,
    dojo_model: splinterm_core::Dojo,
    factory: ConnectionFactory,
    owner: Connection,
    app_id: Option<String>,
    initial_tab_strip_visible: bool,
    surface: Option<WindowSurface>,
) -> Result<()> {
    run_live_multipane_window_inner(
        config,
        dojo_model,
        factory,
        Some(owner),
        app_id,
        initial_tab_strip_visible,
        None,
        surface,
    )
    .await
}

/// What a Window prints when its own command ended before it opened.
const COMMAND_ENDED_LINE: &str = "splinterm: the command ended before its Window opened.";

/// Whether `connection`'s daemon still lists the Dojo `dojo_id`.
async fn dojo_listed(connection: &mut Connection, dojo_id: splinterm_core::DojoId) -> Result<bool> {
    let Response::Lairs { lairs, .. } = connection.request(Request::ListLairs).await? else {
        bail!("splinterd did not return its Lairs");
    };
    Ok(lairs
        .iter()
        .any(|lair| lair.dojos.iter().any(|dojo| dojo.id == dojo_id)))
}

/// What a Window does when its first Dojo is gone before it reads the
/// topology. `owner_still_lists` is what the connection that created a Dojo
/// this client owns (a launched command, such as
/// `splinterm-xdg-terminal-exec -- true`) reports, or `None` for any other
/// Dojo.
///
/// Only that still-open connection proves it is the same daemon. If it no
/// longer lists the Dojo, the Dojo ended with its command, which simply
/// finished: say so and exit 0, as it would a moment later with the Window
/// open. A closed connection means the daemon went away, and a Dojo it still
/// lists means the lookup reached a different daemon; both fail, as does
/// any other first Dojo vanishing.
fn first_dojo_gone(owner_still_lists: Option<Result<bool>>) -> Result<()> {
    match owner_still_lists {
        Some(Ok(false)) => {
            eprintln!("{COMMAND_ENDED_LINE}");
            Ok(())
        }
        Some(Ok(true)) => bail!(
            "a different splinterd answered than the one that created this terminal, so its Window did not open"
        ),
        Some(Err(error)) => Err(error.context(
            "splinterd closed the connection that created this terminal before its Window opened",
        )),
        None => bail!("the terminal ended before its Window opened"),
    }
}

#[allow(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    reason = "multi-pane startup keeps renderer, topology, theme, and input authorities in one lifecycle"
)]
async fn run_live_multipane_window_inner(
    config: AppConfig,
    dojo_model: splinterm_core::Dojo,
    factory: ConnectionFactory,
    initial_transient_owner: Option<Connection>,
    app_id: Option<String>,
    initial_tab_strip_visible: bool,
    resume: Option<WindowResume>,
    surface: Option<WindowSurface>,
) -> Result<()> {
    if let Some(diagnostics) = splinterm::diagnostics::global() {
        diagnostics.ensure_window(Some(dojo_model.id), Some(dojo_model.default_focus));
    }
    let restore = resume.map(|resume| TopologyRestore {
        tabs: resume.tabs,
        active: resume.active,
    });
    let deadline = parked_deadline(&factory);
    let identity = while_parked(
        parked_close(surface.as_ref()),
        deadline,
        initial_window_dojo_identity(&factory, dojo_model.id),
    )
    .await?;
    let WhileParked::Done(identity) = identity else {
        return Ok(());
    };
    let Some((initial_identity, remote_display_identity)) = identity else {
        // The lookup used a new connection, which could have reached a
        // restarted daemon, so ask the connection that created the Dojo.
        let owner_still_lists = match initial_transient_owner {
            Some(mut owner) => Some(dojo_listed(&mut owner, dojo_model.id).await),
            None => None,
        };
        return first_dojo_gone(owner_still_lists);
    };
    let startup_theme = load_startup_theme_for_watch(&config);
    let colors = config.theme_colors();
    let theme = startup_theme.for_window(&colors);
    let theme_colors = WindowThemeColors::new(colors, Some(Arc::clone(&startup_theme.input)));
    renderer::configure(RendererOptions {
        font: config.font.clone(),
        font_ligatures: config.font_ligatures,
        font_features: config.font_features.clone(),
        font_authority: config.font_authority,
        font_size: config.font_size,
        font_sizing_policy: config.font_sizing_policy,
        physical_dpi: 96.0,
        padding: config.padding,
        background_alpha: theme.background_alpha,
    })?;
    let initial_font_generation = Arc::clone(renderer::snapshot_font_generation()?);
    let mut ids = Vec::new();
    layout_splint_ids(&dojo_model.root, &mut ids);
    let image_cache =
        SharedImageContentCache::with_maximum_bytes(MAX_RENDERER_IMAGE_RESIDENT_BYTES)?;
    let prepared = while_parked(parked_close(surface.as_ref()), deadline, async {
        let mut prepared = Vec::with_capacity(ids.len());
        for splint_id in ids {
            prepared.push(
                prepare_live_pane(
                    &factory,
                    &config,
                    splint_id,
                    image_cache.clone(),
                    pane_claims_initial_control(splint_id, dojo_model.default_focus),
                )
                .await?,
            );
        }
        Ok(prepared)
    })
    .await?;
    let WhileParked::Done(prepared) = prepared else {
        return Ok(());
    };
    let (topology_commands, topology_command_receiver) = mpsc::channel(8);
    let (topology_settings, topology_settings_receiver) =
        watch::channel(TopologyReloadSettings::from_config(&config));
    let (topology_update_sender, topology_updates) = mpsc::channel(4);
    let (graphical_focus, _graphical_focus_reporter) = endpoint_graphical_focus(&factory);
    // Beside the focus reporter, a remote Window answers its daemon's agent
    // access prompts (ADR 0015) for as long as it lasts, following Reload
    // configuration; a local Window never starts one.
    let remote_consent = spawn_remote_consent_responder(
        &factory,
        remote_display_identity.as_ref(),
        topology_settings_receiver.clone(),
    )
    .map(AbortOnDropTask::new);
    let local_endpoint = factory.is_local();
    let handoff_status = factory.handoff_status().clone();
    let (layout_ledger, _window_anchor) = spawn_window_anchor(&factory, app_id.clone());
    let (forced_control_transfer, follows_remote_prompts) = forced_control_offer(
        factory.capabilities().forced_control_transfer,
        local_endpoint,
        remote_consent.is_some(),
        config.remote_agent_prompts,
    );
    let optimistic_remote_splits =
        factory.capabilities().launch_semantics == LaunchSemantics::RemoteInteractive;
    let theme_task = AbortOnDropTask::new(tokio::spawn(watch_theme(
        config.theme_source(),
        ThemeOverrides::from_config(&config),
        startup_theme,
        ThemeUpdateSink::Topology(topology_update_sender.clone()),
    )));
    let font_task = (config.font_authority == FontAuthority::NativeOmarchy).then(|| {
        AbortOnDropTask::new(tokio::spawn(watch_font(
            config.font.clone(),
            config.font_authority,
            initial_font_generation,
            FontUpdateSink::Topology(topology_update_sender.clone()),
        )))
    });
    let mut panes = Vec::with_capacity(prepared.len());
    let mut tasks = HashMap::with_capacity(prepared.len());
    for pane in prepared {
        let splint_id = pane.options.snapshot.splint_id;
        panes.push(pane.options);
        tasks.insert(splint_id, pane.task);
    }
    let topology_smoke = spawn_topology_smoke(
        topology_commands.clone(),
        dojo_model.id,
        dojo_model.default_focus,
    )?;
    let window_config = config.clone();
    let root = dojo_model.root;
    let (manager_root, active_splint) = (root.clone(), dojo_model.default_focus);
    let topology_manager = tokio::spawn(run_topology_manager(
        factory,
        config,
        topology_settings_receiver,
        image_cache,
        initial_identity.clone(),
        manager_root,
        topology_command_receiver,
        topology_update_sender,
        tasks,
        initial_transient_owner,
        restore,
    ));
    let result = tokio::task::spawn_blocking(move || {
        // The terminal binds a staged Window's surface for good.
        run_window_phase(
            surface,
            WindowOptions {
                capture: pane_chrome_capture()?,
                panes,
                layout: Some(root),
                active_splint: Some(active_splint),
                topology_updates: Some(topology_updates),
                topology_commands: Some(topology_commands),
                topology_settings: Some(topology_settings),
                graphical_focus,
                forced_control_transfer,
                forced_control_follows_remote_prompts: follows_remote_prompts,
                remote_display_identity,
                optimistic_remote_splits,
                local_endpoint,
                clipboard_image_directory: window_config.clipboard_image_directory,
                middle_click_paste: window_config.middle_click_paste,
                initial_dojo: Some(initial_identity),
                initial_tab_strip_visible,
                initial_columns: window_config.initial_columns,
                initial_rows: window_config.initial_rows,
                cursor_style: window_config.cursor_style,
                cursor_blink: window_config.cursor_blink,
                title: window_config.title,
                app_id: resolved_window_app_id(app_id),
                theme,
                theme_colors,
                ui_font_size: window_config.ui_font_size,
                pane_divider_style: window_config.pane_divider_style,
                pane_gap: window_config.pane_gap,
                frame_title_mode: window_config.frame_title_mode,
                agent_status_presentation: window_config.explorer_agent_status,
                agent_status_style: window_config.explorer_status_style,
                tab_strip_border: window_config.tab_strip_border,
                explorer_full_height: window_config.explorer_full_height,
                explorer_collapsed: window_config.explorer_collapsed,
                explorer_two_line_rows: window_config.explorer_two_line_rows,
                explorer_current_marker: window_config.explorer_current_marker,
                keymap: window_config.keymap,
                prefix_timeout_ms: window_config.prefix_timeout_ms,
                handoff_status: Some(handoff_status),
                layout_ledger,
                ..WindowOptions::default()
            },
        )
        .map(drop)
    })
    .await
    .context("Wayland window task failed")?;
    theme_task.abort();
    if let Some(font_task) = font_task {
        font_task.abort();
    }
    if let Some(smoke) = topology_smoke {
        smoke.await.context("topology smoke task failed")??;
    }
    finish_topology_manager(topology_manager.await)?;
    result
}

#[allow(
    clippy::too_many_lines,
    reason = "subscription resync, controller ownership, and window task shutdown are one lifecycle"
)]
pub(super) async fn run_live_window(
    config: AppConfig,
    splint_id: SplintId,
    factory: ConnectionFactory,
) -> Result<()> {
    if let Some(diagnostics) = splinterm::diagnostics::global() {
        diagnostics.ensure_window(None, Some(splint_id));
    }
    let startup_theme = load_startup_theme_for_watch(&config);
    let colors = config.theme_colors();
    let theme = startup_theme.for_window(&colors);
    let theme_colors = WindowThemeColors::new(colors, Some(Arc::clone(&startup_theme.input)));
    renderer::configure(RendererOptions {
        font: config.font.clone(),
        font_ligatures: config.font_ligatures,
        font_features: config.font_features.clone(),
        font_authority: config.font_authority,
        font_size: config.font_size,
        font_sizing_policy: config.font_sizing_policy,
        physical_dpi: 96.0,
        padding: config.padding,
        background_alpha: theme.background_alpha,
    })?;
    let initial_font_generation = Arc::clone(renderer::snapshot_font_generation()?);
    let mut connection = factory.connect().await?;
    let remote_display_identity = factory.remote_display_identity(&connection);
    let terminal_grid_limits = terminal_grid_limits(connection.limits());
    let incarnation = connection.live_incarnation(splint_id).await?;
    let requested_scopes = pane_access_scopes();
    if !matches!(
        connection
            .request(Request::RequestAccess {
                splint_id,
                incarnation,
                scopes: requested_scopes,
            })
            .await?,
        Response::AccessGranted { .. }
    ) {
        bail!("splinterd did not grant requested terminal access");
    }
    let authority = load_authority_status(&mut connection, splint_id, incarnation).await?;
    let mut attachment = attach(&mut connection, splint_id, incarnation).await?;
    let image_cache =
        SharedImageContentCache::with_maximum_bytes(MAX_RENDERER_IMAGE_RESIDENT_BYTES)?;
    let image_transport = factory.capabilities().image_transport;
    resolve_image_contents(
        image_transport,
        &mut connection,
        &attachment.snapshot,
        &image_cache,
    )
    .await?;
    let initial_image_sources = lease_snapshot_images(&image_cache, &attachment.snapshot)?;
    let mut control = factory.connect().await?;
    let control_incarnation = control.live_incarnation(splint_id).await?;
    if control_incarnation != incarnation {
        bail!("control connection observed a different process incarnation");
    }
    let controller_id = optional_pane_controller(
        control
            .acquire_control(
                splint_id,
                incarnation,
                vec![ControlMode::Input, ControlMode::Resize],
            )
            .await,
    )?;
    if let Some(controller_id) = controller_id {
        println!("Controller lease {controller_id} granted for live Splint");
    }
    let (updates, receiver) = mpsc::channel(WINDOW_UPDATE_QUEUE);
    let _theme_watcher = AbortOnDropTask::new(tokio::spawn(watch_theme(
        config.theme_source(),
        ThemeOverrides::from_config(&config),
        startup_theme,
        ThemeUpdateSink::Panes(vec![updates.clone()]),
    )));
    let _font_watcher = (config.font_authority == FontAuthority::NativeOmarchy).then(|| {
        AbortOnDropTask::new(tokio::spawn(watch_font(
            config.font.clone(),
            config.font_authority,
            initial_font_generation,
            FontUpdateSink::Panes(vec![updates.clone()]),
        )))
    });
    let (command_sender, commands) = mpsc::channel(WINDOW_COMMAND_QUEUE);
    let mut daemon = DaemonIdentity::of(&connection);
    let (mut controller, mut resyncs, mut controller_cancellation) = spawn_single_pane_controller(
        &factory,
        control,
        commands,
        updates.clone(),
        controller_id,
        splint_id,
        incarnation,
        config.resize_delay_ms,
    );
    let mut last_revision = attachment.snapshot.revision;
    let initial_snapshot = attachment.snapshot;
    let window_config = config.clone();
    let (graphical_focus, _graphical_focus_reporter) = endpoint_graphical_focus(&factory);
    // A single-Splint Window has no Reload configuration to publish, so it
    // keeps the opt-out it opened with, for as long as the Window lasts.
    let (_remote_consent_settings, remote_consent_settings) =
        watch::channel(TopologyReloadSettings::from_config(&config));
    let remote_consent = spawn_remote_consent_responder(
        &factory,
        remote_display_identity.as_ref(),
        remote_consent_settings,
    )
    .map(AbortOnDropTask::new);
    let local_endpoint = factory.is_local();
    let handoff_status = factory.handoff_status().clone();
    // Its responder keeps the opt-out it opened with, and so does Force
    // control.
    let (forced_control_transfer, _) = forced_control_offer(
        factory.capabilities().forced_control_transfer,
        local_endpoint,
        remote_consent.is_some(),
        config.remote_agent_prompts,
    );
    let mut window = tokio::task::spawn_blocking(move || {
        run_window(WindowOptions {
            snapshot: Some(initial_snapshot),
            image_sources: initial_image_sources,
            updates: Some(receiver),
            commands: Some(command_sender),
            authority,
            controlled: controller_id.is_some(),
            graphical_focus,
            forced_control_transfer,
            remote_display_identity,
            terminal_grid_limits,
            local_endpoint,
            clipboard_image_directory: window_config.clipboard_image_directory,
            middle_click_paste: window_config.middle_click_paste,
            initial_columns: window_config.initial_columns,
            initial_rows: window_config.initial_rows,
            cursor_style: window_config.cursor_style,
            cursor_blink: window_config.cursor_blink,
            title: window_config.title,
            theme,
            theme_colors,
            ui_font_size: window_config.ui_font_size,
            pane_divider_style: window_config.pane_divider_style,
            pane_gap: window_config.pane_gap,
            frame_title_mode: window_config.frame_title_mode,
            agent_status_presentation: window_config.explorer_agent_status,
            agent_status_style: window_config.explorer_status_style,
            tab_strip_border: window_config.tab_strip_border,
            explorer_full_height: window_config.explorer_full_height,
            explorer_collapsed: window_config.explorer_collapsed,
            explorer_two_line_rows: window_config.explorer_two_line_rows,
            explorer_current_marker: window_config.explorer_current_marker,
            keymap: window_config.keymap,
            prefix_timeout_ms: window_config.prefix_timeout_ms,
            handoff_status: Some(handoff_status),
            ..WindowOptions::default()
        })
    });
    let mut last_sequence = 0_u64;

    loop {
        // Set when a daemon connection was lost during a handoff, with the
        // command queue the next controller takes over (Plan 0061 M3.12b).
        let mut reconnect_with = None;
        tokio::select! {
            biased;
            result = &mut window => {
                // Dropping the window closes its command sender. The controller task then
                // releases the lease explicitly; connection teardown is the fallback.
                let window_result = result.context("Wayland window task failed")?;
                finish_controller(controller).await?;
                return window_result;
            }
            result = &mut controller => {
                let (controller_result, commands) =
                    result.context("window controller task failed")?;
                if controller_result.is_err()
                    && factory.handoff_status().reconnect_pending().is_some()
                {
                    // The controller's connection was lost while a handoff
                    // was preparing or fenced; it already started the
                    // Window's reconnection.
                    reconnect_with = Some(commands);
                } else {
                    let _ = updates.send(WindowUpdate::Shutdown).await;
                    let window_result = window.await.context("Wayland window task failed")?;
                    controller_result?;
                    return window_result;
                }
            }
            Some(()) = resyncs.recv() => {
                if std::env::var_os("SPLINTERM_SCROLL_TRACE").is_some() {
                    eprintln!("scroll-trace resync=controller_page");
                }
                attachment = resynchronize(
                    &mut connection,
                    attachment.subscription_id,
                    splint_id,
                    incarnation,
                ).await?;
                resolve_image_contents(
                    image_transport, &mut connection, &attachment.snapshot, &image_cache,
                ).await?;
                let image_sources = lease_snapshot_images(&image_cache, &attachment.snapshot)?;
                if updates
                    .send(WindowUpdate::Snapshot {
                        snapshot: attachment.snapshot.clone(),
                        image_sources,
                        authoritative: true,
                    })
                    .await
                    .is_err()
                {
                    let window_result = window.await.context("Wayland window task failed")?;
                    finish_controller(controller).await?;
                    return window_result;
                }
                last_revision = attachment.snapshot.revision;
                last_sequence = 0;
            }
            frame = connection.next_server_frame() => {
                match frame {
                    // The attachment's connection was lost while a handoff
                    // was preparing or fenced: the Window reconnects. The
                    // controller is stopped first to take its queue back.
                    Err(error)
                        if is_connection_loss(&error) && factory.handoff_status().begin_reconnect() =>
                    {
                        controller_cancellation.cancel();
                        let (_, commands) = (&mut controller)
                            .await
                            .context("window controller task failed")?;
                        reconnect_with = Some(commands);
                    }
                    Err(error) => return Err(error),
                    Ok(ServerFrame::Event {
                        subscription_id,
                        sequence,
                        event,
                    }) => match classify_subscription_event(
                        attachment.subscription_id,
                        last_sequence,
                        subscription_id,
                        sequence,
                        event,
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
                                let window_result = window.await.context("Wayland window task failed")?;
                                finish_controller(controller).await?;
                                return window_result;
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
                                        rows: Some(
                                            u64::try_from(update.rows.len()).unwrap_or(u64::MAX),
                                        ),
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
                                let window_result =
                                    window.await.context("Wayland window task failed")?;
                                finish_controller(controller).await?;
                                return window_result;
                            }
                            last_sequence = sequence;
                        }
                        EventAction::Update { update, .. } => {
                            if std::env::var_os("SPLINTERM_SCROLL_TRACE").is_some() {
                                eprintln!(
                                    "scroll-trace resync=revision last={} base={} final={}",
                                    last_revision, update.base_revision, update.revision
                                );
                            }
                            attachment = resynchronize(
                                &mut connection,
                                attachment.subscription_id,
                                splint_id,
                                incarnation,
                            ).await?;
                            resolve_image_contents(
                                image_transport, &mut connection, &attachment.snapshot, &image_cache,
                            ).await?;
                            let image_sources = lease_snapshot_images(&image_cache, &attachment.snapshot)?;
                            if updates
                                .send(WindowUpdate::Snapshot {
                                    snapshot: attachment.snapshot.clone(),
                                    image_sources,
                                    authoritative: true,
                                })
                                .await
                                .is_err()
                            {
                                let window_result = window.await.context("Wayland window task failed")?;
                                finish_controller(controller).await?;
                                return window_result;
                            }
                            last_revision = attachment.snapshot.revision;
                            last_sequence = 0;
                        }
                        EventAction::Resynchronize => {
                            if std::env::var_os("SPLINTERM_SCROLL_TRACE").is_some() {
                                eprintln!(
                                    "scroll-trace resync=subscription_sequence last_sequence={last_sequence} received_sequence={sequence}"
                                );
                            }
                            attachment = resynchronize(
                                &mut connection,
                                attachment.subscription_id,
                                splint_id,
                                incarnation,
                            ).await?;
                            resolve_image_contents(
                                image_transport, &mut connection, &attachment.snapshot, &image_cache,
                            ).await?;
                            let image_sources = lease_snapshot_images(&image_cache, &attachment.snapshot)?;
                            if updates
                                .send(WindowUpdate::Snapshot {
                                    snapshot: attachment.snapshot.clone(),
                                    image_sources,
                                    authoritative: true,
                                })
                                .await
                                .is_err()
                            {
                                let window_result = window.await.context("Wayland window task failed")?;
                                finish_controller(controller).await?;
                                return window_result;
                            }
                            last_revision = attachment.snapshot.revision;
                            last_sequence = 0;
                        }
                        EventAction::Exited | EventAction::Shutdown => {
                            let _ = updates.send(WindowUpdate::Shutdown).await;
                            let window_result = window.await.context("Wayland window task failed")?;
                            finish_controller(controller).await?;
                            return window_result;
                        }
                    },
                    Ok(ServerFrame::Error { error, .. }) => {
                        bail!("splinterd: {}", error.message);
                    }
                    Ok(_) => bail!("splinterd sent an unexpected frame while subscribed"),
                }
            }
        }
        let Some(mut commands) = reconnect_with else {
            continue;
        };
        let outcome = reconnect_single_pane(
            &factory,
            &mut window,
            &mut commands,
            splint_id,
            incarnation,
            image_transport,
            &image_cache,
            daemon,
        )
        .await?;
        match outcome {
            SinglePaneReconnect::WindowClosed(window_result) => return window_result,
            SinglePaneReconnect::Attached(attached) => {
                let SinglePaneAttachment {
                    connection: next_connection,
                    attachment: next_attachment,
                    control,
                    controller_id,
                    daemon: serving,
                } = *attached;
                if let DaemonChange::Upgraded { from, to } = daemon.change_to(serving) {
                    eprintln!(
                        "splinterm: reconnected to upgraded splinterd (generation {from} → {to})"
                    );
                }
                connection = next_connection;
                attachment = next_attachment;
                daemon = serving;
                // The epoch this reattachment answers: a loss after it, as
                // when the new controller dies at once, keeps the Window
                // reconnecting (review N2).
                let epoch = factory
                    .handoff_status()
                    .reconnect_pending()
                    .unwrap_or_default();
                // Nothing queued before or during the reconnection reaches
                // the new controller (review S4).
                drain_queued_commands(&mut commands);
                (controller, resyncs, controller_cancellation) = spawn_single_pane_controller(
                    &factory,
                    control,
                    commands,
                    updates.clone(),
                    controller_id,
                    splint_id,
                    incarnation,
                    config.resize_delay_ms,
                );
                let image_sources = lease_snapshot_images(&image_cache, &attachment.snapshot)?;
                if updates
                    .send(WindowUpdate::Snapshot {
                        snapshot: attachment.snapshot.clone(),
                        image_sources,
                        authoritative: true,
                    })
                    .await
                    .is_err()
                {
                    let window_result = window.await.context("Wayland window task failed")?;
                    finish_controller(controller).await?;
                    return window_result;
                }
                last_revision = attachment.snapshot.revision;
                last_sequence = 0;
                // The resnapshot is queued before the pause ends, so input
                // never opens on the screen from before the loss. Another
                // piece that lost its connection meanwhile (the focus
                // reporter, say) does not hold up a Window whose controller
                // still runs; a controller that died reconnects again.
                let hub = factory.handoff_status();
                if !hub.reconnected(daemon.generation, epoch) && !controller.is_finished() {
                    let _ = hub
                        .reconnected(daemon.generation, hub.reconnect_pending().unwrap_or(epoch));
                }
            }
            SinglePaneReconnect::Ended => {
                // The Splint ended meanwhile, which ends a single-pane Window.
                let hub = factory.handoff_status();
                let _ = hub.reconnected(
                    daemon.generation,
                    hub.reconnect_pending().unwrap_or_default(),
                );
                let _ = updates.send(WindowUpdate::Shutdown).await;
                return window.await.context("Wayland window task failed")?;
            }
            SinglePaneReconnect::Lost => {
                // The Window stays open, paused, with its last screen until
                // it is closed; its commands go nowhere.
                loop {
                    tokio::select! {
                        biased;
                        result = &mut window => {
                            return result.context("Wayland window task failed")?;
                        }
                        Some(_) = commands.recv() => {}
                    }
                }
            }
        }
    }
}

/// Whether a Window offers Force control (Plan 0072 M3), and whether that
/// follows the viewer's `remote-agent-prompts` on Reload configuration. Its
/// endpoint must allow it (`capability`). A remote Window confirms a force
/// in its own dialog, so it offers Force control only when it runs a
/// consent `responder` and the viewer allows remote prompts; otherwise
/// every force would fail.
fn forced_control_offer(
    capability: ForcedControlTransfer,
    local: bool,
    responder: bool,
    remote_agent_prompts: bool,
) -> (bool, bool) {
    let enabled = capability == ForcedControlTransfer::Enabled;
    if local {
        return (enabled, false);
    }
    let follows = enabled && responder;
    (follows && remote_agent_prompts, follows)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use anyhow::Result;

    use splinterm::endpoint::ForcedControlTransfer;

    use super::{
        AbortOnDropTask, COMMAND_ENDED_LINE, WhileParked, first_dojo_gone, forced_control_offer,
        resolved_window_app_id, while_parked,
    };

    /// Plan 0072 M3: a remote Window offers Force control only while it can
    /// show the confirmation; a local Window whenever its endpoint allows.
    #[test]
    fn a_remote_window_offers_force_control_only_while_it_can_confirm_it() {
        // (capability, local, responder, remote prompts) → (offered, follows)
        let on = ForcedControlTransfer::Enabled;
        let off = ForcedControlTransfer::Disabled;
        let offer = forced_control_offer;
        assert_eq!(offer(on, true, false, false), (true, false));
        assert_eq!(offer(off, true, false, true), (false, false));
        assert_eq!(offer(on, false, true, true), (true, true));
        // Remote prompts off: offered again once a reload turns them on.
        assert_eq!(offer(on, false, true, false), (false, true));
        // Without a responder, never.
        assert_eq!(offer(on, false, false, true), (false, false));
        assert_eq!(offer(off, false, true, true), (false, false));
    }

    #[tokio::test(start_paused = true)]
    async fn parked_work_ends_at_a_close_or_its_deadline_and_runs_unbounded_unparked() {
        // A remote that never answers: a close ends the request at once.
        let closed = while_parked(
            Some(std::future::ready(())),
            Duration::from_secs(3600),
            std::future::pending::<Result<()>>(),
        )
        .await
        .unwrap();
        assert_eq!(closed, WhileParked::Closed);

        // Without a close, the deadline ends it.
        let started = tokio::time::Instant::now();
        let stalled = while_parked(
            Some(std::future::pending::<()>()),
            Duration::from_secs(7),
            std::future::pending::<Result<()>>(),
        )
        .await;
        assert!(stalled.is_err());
        assert_eq!(started.elapsed(), Duration::from_secs(7));

        // Work that answers in time is returned.
        let answered = while_parked(
            Some(std::future::pending::<()>()),
            Duration::from_secs(7),
            async { Ok(5) },
        )
        .await
        .unwrap();
        assert_eq!(answered, WhileParked::Done(5));

        // Not parked: no close and no deadline apply.
        let unparked = while_parked(
            None::<std::future::Pending<()>>,
            Duration::from_secs(1),
            async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok("slow but fine")
            },
        )
        .await
        .unwrap();
        assert_eq!(unparked, WhileParked::Done("slow but fine"));
    }

    #[test]
    fn a_first_dojo_gone_is_a_finished_command_only_when_its_own_daemon_says_so() {
        // The creating connection's daemon no longer lists it: finished.
        first_dojo_gone(Some(Ok(false))).expect("an owned command that ended has finished");
        assert!(COMMAND_ENDED_LINE.contains("ended before its Window opened"));
        // The creating daemon still lists it: the lookup reached another
        // (restarted) daemon.
        let error = first_dojo_gone(Some(Ok(true))).expect_err("a replacement daemon fails");
        assert!(error.to_string().contains("different splinterd"), "{error}");
        // The creating connection is gone: the daemon went away.
        let error = first_dojo_gone(Some(Err(anyhow::anyhow!("connection closed"))))
            .expect_err("a lost daemon fails");
        assert!(
            error
                .to_string()
                .contains("closed the connection that created"),
            "{error}"
        );
        // Not owned by this client.
        let error = first_dojo_gone(None).expect_err("another Dojo vanishing fails");
        assert_eq!(
            error.to_string(),
            "the terminal ended before its Window opened"
        );
    }

    #[test]
    fn xdg_window_identity_defaults_and_preserves_exact_override() {
        assert_eq!(resolved_window_app_id(None), splinterm::config::APP_ID);
        assert_eq!(
            resolved_window_app_id(Some("org.omarchy.screensaver".to_owned())),
            "org.omarchy.screensaver"
        );
    }

    #[tokio::test]
    async fn watcher_tasks_abort_on_scope_exit() {
        let (sender, receiver) = tokio::sync::oneshot::channel::<()>();
        let watcher = AbortOnDropTask::new(tokio::spawn(async move {
            std::future::pending::<()>().await;
            let _ = sender.send(());
        }));

        drop(watcher);

        assert!(receiver.await.is_err());
    }

    /// Review S4: commands queued for a single-pane Window before or during
    /// a reconnection never reach the controller that takes over: the
    /// daemon sees only the new control subscription, no control claim or
    /// input.
    #[tokio::test]
    async fn a_resumed_controller_never_sends_commands_queued_before_it() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        use splinterm::{WindowCommand, endpoint::ConnectionFactory};
        use splinterm_protocol::{
            ClientFrame, ControlStatus, PROTOCOL_VERSION, Request, Response, ServerFrame,
            ServerLimits, encode_frame,
        };

        use super::{WINDOW_COMMAND_QUEUE, drain_queued_commands, spawn_single_pane_controller};

        async fn read_client(reader: &mut (impl AsyncReadExt + Unpin)) -> ClientFrame {
            let mut length = [0_u8; 4];
            reader.read_exact(&mut length).await.unwrap();
            let mut body = vec![0_u8; u32::from_be_bytes(length) as usize];
            reader.read_exact(&mut body).await.unwrap();
            serde_json::from_slice(&body).unwrap()
        }

        let splint_id = splinterm_core::SplintId::new();
        let (client, server) = tokio::io::duplex(65_536);
        let daemon = tokio::spawn(async move {
            let (mut reader, mut writer) = tokio::io::split(server);
            read_client(&mut reader).await;
            let hello = ServerFrame::Hello {
                version: PROTOCOL_VERSION,
                limits: ServerLimits::default(),
                development_terminal_access: false,
                daemon_hostname: None,
                daemon_generation: 2,
            };
            writer
                .write_all(&encode_frame(&hello).unwrap())
                .await
                .unwrap();
            let ClientFrame::Request {
                request_id,
                request: Request::SubscribeControl { .. },
                ..
            } = read_client(&mut reader).await
            else {
                panic!("expected only the control subscription");
            };
            let subscribed = ServerFrame::Response {
                request_id,
                result: Response::ControlSubscribed {
                    subscription_id: 3,
                    status: ControlStatus {
                        splint_id,
                        incarnation: 7,
                        controlled: false,
                        locally_owned: false,
                        lost_reason: None,
                    },
                },
            };
            writer
                .write_all(&encode_frame(&subscribed).unwrap())
                .await
                .unwrap();
            let mut rest = Vec::new();
            reader.read_to_end(&mut rest).await.unwrap();
            assert!(
                rest.is_empty(),
                "a command queued before the reconnection was sent"
            );
        });
        let (reader, writer) = tokio::io::split(client);
        let control =
            splinterm::automation::Connection::connect_remote_interactive_transport(reader, writer)
                .await
                .unwrap();
        let (sender, mut commands) = tokio::sync::mpsc::channel(WINDOW_COMMAND_QUEUE);
        sender
            .try_send(WindowCommand::Input(b"rm -rf build\n".to_vec()))
            .unwrap();
        sender
            .try_send(WindowCommand::RequestControlTransfer)
            .unwrap();
        assert_eq!(drain_queued_commands(&mut commands), 2);
        let (updates, _update_receiver) = tokio::sync::mpsc::channel(8);
        let (task, _resyncs, _cancellation) = spawn_single_pane_controller(
            &ConnectionFactory::local(),
            control,
            commands,
            updates,
            None,
            splint_id,
            7,
            0,
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        // The frontend closes; the controller ends without a request.
        drop(sender);
        let (result, _) = task.await.unwrap();
        result.unwrap();
        daemon.await.unwrap();
    }

    /// Review S8: a Splint removed, or whose process changed, during the
    /// upgrade ends a single-pane Window instead of leaving it to time out.
    #[test]
    fn a_removed_or_replaced_splint_ends_the_single_pane_window() {
        use splinterm::automation::response_protocol_error;
        use splinterm_protocol::{ErrorCode, ProtocolError};

        use super::unless_splint_gone;

        for code in [ErrorCode::NotFound, ErrorCode::StaleIncarnation] {
            let gone = response_protocol_error(ProtocolError::new(code, "gone"));
            assert!(
                unless_splint_gone::<()>(Err(gone)).unwrap().is_none(),
                "{code:?}"
            );
        }
        let refused = response_protocol_error(ProtocolError::new(
            ErrorCode::Unauthorized,
            "not this client",
        ));
        assert!(
            unless_splint_gone::<()>(Err(refused)).is_err(),
            "retried, not ended"
        );
        assert_eq!(unless_splint_gone(Ok(3)).unwrap(), Some(3));
    }
}
