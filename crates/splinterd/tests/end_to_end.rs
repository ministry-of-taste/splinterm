use std::{
    fs,
    io::Read,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};
use splinterm_automation_client::{
    Connection as AutomationConnection, protocol_error as automation_protocol_error,
};
use splinterm_core::{
    Axis, DojoId, LairId, LayoutNode, SplintId, SplitRatio, SplitSide, TopologyRevision,
};
use splinterm_protocol::{
    AccessScope, ActiveScreen, AutomationLaunch, ClientFrame, ClientRole, ColorSource, ControlMode,
    ControlTransferDecision, ControlTransferOutcome, DojoUse, ErrorCode, HistoryTransition,
    LaunchParameters, MAX_FRAME_BYTES, MAX_SNAPSHOT_SCROLLBACK_ROWS, MAX_SUBSCRIPTIONS,
    MouseTracking, MutationPreflight, MutationTarget, PROTOCOL_VERSION, ProtocolError, Request,
    Response, ServerFrame, SplintLifecycle, SubscriptionEvent, TerminalInputModes,
    TerminalProvenance, TerminalRow, TerminalScrollbackUpdate, TerminalSnapshot, TerminalUpdate,
    TopologyChangeKind, UnusedDojoCloseOutcome, encode_frame,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    time,
};

const DAEMON: &str = env!("CARGO_BIN_EXE_splinterd");
const TEST_TIMEOUT: Duration = Duration::from_secs(20);
const TEST_SHUTDOWN_GRACE_MS: &str = "1000";
const PHASE8_PACED_WORKLOAD: &[u8] = b"i=0; while [ $i -lt 2000 ]; do limit=$((i+20)); while [ $i -lt $limit ]; do printf 'paced-%05d-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\\n' $i; i=$((i+1)); done; sleep 0.01; done; printf 'overflow-%s\\n' finished\n";
const PHASE8_PRESSURE_WORKLOAD: &[u8] = b"i=0; while [ $i -lt 30000 ]; do printf 'pressure-%05d-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\\n' $i; i=$((i+1)); done; printf 'pressure-%s\\n' finished\n";

struct Daemon {
    child: Child,
    runtime: PathBuf,
    socket: PathBuf,
    policy: Option<PathBuf>,
    development_terminal_access: bool,
}

impl Daemon {
    fn spawn_child(runtime: &Path, socket: &Path) -> Child {
        Self::spawn_configured(runtime, socket, None, true)
    }

    fn spawn_configured(
        runtime: &Path,
        socket: &Path,
        policy: Option<&Path>,
        development_terminal_access: bool,
    ) -> Child {
        Self::spawn_configured_with_environment(
            runtime,
            socket,
            policy,
            development_terminal_access,
            &[],
        )
    }

    fn spawn_configured_with_environment(
        runtime: &Path,
        socket: &Path,
        policy: Option<&Path>,
        development_terminal_access: bool,
        environment: &[(&str, &str)],
    ) -> Child {
        // A private, empty Omarchy theme keeps the daemon's Gum lookup inside
        // the sandbox instead of falling back to the real user's `$HOME` theme.
        fs::create_dir_all(runtime.join("state/omarchy/current/theme")).unwrap();
        let stderr = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(runtime.join("daemon.stderr"))
            .unwrap();
        // The daemon's log stream: `tracing` writes to stdout, which the
        // journal captures under systemd.
        let stdout = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(runtime.join("daemon.stdout"))
            .unwrap();
        let mut command = Command::new(DAEMON);
        command
            .env("SPLINTERM_SOCKET", socket)
            .env("XDG_STATE_HOME", runtime.join("state"))
            .env("SPLINTERM_LAIR_ID", "caller-supplied-dojo")
            .env("SPLINTERM_DOJO_ID", "caller-supplied-dojo")
            .env("SPLINTERM_SPLINT_ID", "caller-supplied-splint")
            .env(
                "SPLINTERM_SPLINT_INCARNATION",
                "caller-supplied-incarnation",
            )
            // Integration tests preserve the complete HUP -> TERM -> KILL state
            // machine without paying production's two 30-second grace periods.
            // Release daemon builds do not compile support for this override.
            .env(
                "SPLINTERM_TEST_SHUTDOWN_GRACE_MS",
                TEST_SHUTDOWN_GRACE_MS,
            )
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .envs(environment.iter().copied())
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr);
        if development_terminal_access {
            command.env("SPLINTERM_ENABLE_DEV_ATTACH", "1");
        } else {
            command.env_remove("SPLINTERM_ENABLE_DEV_ATTACH");
        }
        if let Some(policy) = policy {
            command.env("SPLINTERM_POLICY", policy);
        }
        command.spawn().unwrap()
    }

    fn assert_success(&self, status: std::process::ExitStatus) {
        if status.success() {
            return;
        }
        let stderr = fs::read(self.runtime.join("daemon.stderr")).unwrap_or_default();
        let tail = &stderr[stderr.len().saturating_sub(8 * 1024)..];
        panic!(
            "daemon exited as {status:?}; bounded stderr tail:\n{}",
            String::from_utf8_lossy(tail)
        );
    }

    async fn wait_until_ready(socket: &Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() {
            assert!(Instant::now() < deadline, "daemon socket did not appear");
            time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn start() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        // Leave room for the companion `.content` socket under a repo-local TMPDIR.
        let runtime = std::env::temp_dir().join(format!("sp8-{}-{nonce:x}", std::process::id()));
        fs::create_dir(&runtime).unwrap();
        let socket = runtime.join("splinterd.sock");
        let child = Self::spawn_child(&runtime, &socket);
        Self::wait_until_ready(&socket).await;
        Self {
            child,
            runtime,
            socket,
            policy: None,
            development_terminal_access: true,
        }
    }

    /// [`Self::start`] with `environment` added to the daemon's own.
    async fn start_with_environment(environment: &[(&str, &str)]) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let runtime = std::env::temp_dir().join(format!("spenv-{}-{nonce:x}", std::process::id()));
        fs::create_dir(&runtime).unwrap();
        let socket = runtime.join("splinterd.sock");
        let child =
            Self::spawn_configured_with_environment(&runtime, &socket, None, true, environment);
        Self::wait_until_ready(&socket).await;
        Self {
            child,
            runtime,
            socket,
            policy: None,
            development_terminal_access: true,
        }
    }

    async fn start_with_gum_environment(palette: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let runtime = std::env::temp_dir().join(format!("spgum-{}-{nonce:x}", std::process::id()));
        let theme = runtime.join("state/omarchy/current/theme");
        fs::create_dir_all(&theme).unwrap();
        fs::write(theme.join("gum_env.lua"), palette).unwrap();
        let socket = runtime.join("splinterd.sock");
        let child = Self::spawn_configured_with_environment(
            &runtime,
            &socket,
            None,
            true,
            &[
                ("BACKGROUND", "#1b1b1b"),
                ("GUM_CHOOSE_SELECTED_BACKGROUND", "#808080"),
                ("GUM_OLD_THEME_ONLY", "#f4f4f4"),
            ],
        );
        Self::wait_until_ready(&socket).await;
        Self {
            child,
            runtime,
            socket,
            policy: None,
            development_terminal_access: true,
        }
    }

    async fn start_with_policy(policy_contents: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let runtime =
            std::env::temp_dir().join(format!("sppolicy-{}-{nonce:x}", std::process::id()));
        fs::create_dir(&runtime).unwrap();
        let policy = runtime.join("policy.json");
        fs::write(&policy, policy_contents).unwrap();
        fs::set_permissions(&policy, fs::Permissions::from_mode(0o600)).unwrap();
        let socket = runtime.join("splinterd.sock");
        let child = Self::spawn_configured(&runtime, &socket, Some(&policy), false);
        Self::wait_until_ready(&socket).await;
        Self {
            child,
            runtime,
            socket,
            policy: Some(policy),
            development_terminal_access: false,
        }
    }

    async fn connect(&self) -> Connection {
        Connection::connect(&self.socket).await
    }

    fn stop_preserving_state(&mut self) {
        let pid = rustix::process::Pid::from_raw(i32::try_from(self.child.id()).unwrap()).unwrap();
        rustix::process::kill_process(pid, rustix::process::Signal::INT).unwrap();
        let status = self.child.wait().unwrap();
        self.assert_success(status);
        assert!(!self.socket.exists());
    }

    async fn start_again(&mut self) {
        self.start_again_with_environment(&[]).await;
    }

    async fn start_again_with_environment(&mut self, environment: &[(&str, &str)]) {
        self.child = Self::spawn_configured_with_environment(
            &self.runtime,
            &self.socket,
            self.policy.as_deref(),
            self.development_terminal_access,
            environment,
        );
        Self::wait_until_ready(&self.socket).await;
    }

    fn reload_policy(&self) {
        let pid = rustix::process::Pid::from_raw(i32::try_from(self.child.id()).unwrap()).unwrap();
        rustix::process::kill_process(pid, rustix::process::Signal::HUP).unwrap();
    }

    fn shutdown(self) {
        self.shutdown_with_signal(rustix::process::Signal::INT);
    }

    fn shutdown_with_signal(mut self, signal: rustix::process::Signal) {
        let pid = rustix::process::Pid::from_raw(i32::try_from(self.child.id()).unwrap()).unwrap();
        rustix::process::kill_process(pid, signal).unwrap();
        let status = self.child.wait().unwrap();
        self.assert_success(status);
        assert!(!self.socket.exists());
        fs::remove_dir_all(&self.runtime).unwrap();
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let _ = fs::remove_dir_all(&self.runtime);
    }
}

struct Connection {
    stream: UnixStream,
    request_id: u64,
    controller_id: Option<u64>,
}

impl Connection {
    async fn connect(socket: &Path) -> Self {
        let mut stream = UnixStream::connect(socket).await.unwrap();
        write_frame(
            &mut stream,
            &ClientFrame::Hello {
                minimum_version: PROTOCOL_VERSION,
                maximum_version: PROTOCOL_VERSION,
                role: ClientRole::Automation,
                handoff_window: false,
            },
        )
        .await;
        assert!(matches!(
            read_frame(&mut stream).await,
            ServerFrame::Hello {
                version: PROTOCOL_VERSION,
                ..
            }
        ));
        Self {
            stream,
            request_id: 1,
            controller_id: None,
        }
    }

    async fn request(&mut self, request: Request) -> Response {
        self.request_result(request).await.unwrap_or_else(|error| {
            panic!("request failed with {:?}: {}", error.code, error.message)
        })
    }

    async fn topology_revision(&mut self) -> splinterm_core::TopologyRevision {
        let Response::Topology { snapshot } = self.request(Request::InspectTopology).await else {
            panic!("topology response was not returned");
        };
        snapshot.revision
    }

    async fn topology(&mut self) -> splinterm_core::Topology {
        let Response::Topology { snapshot } = self.request(Request::InspectTopology).await else {
            panic!("topology response was not returned");
        };
        snapshot.topology
    }

    async fn request_result(&mut self, request: Request) -> Result<Response, ProtocolError> {
        let request_id = self.request_id;
        self.request_id += 1;
        write_frame(
            &mut self.stream,
            &ClientFrame::Request {
                request_id,
                diagnostic_correlation: None,
                request,
            },
        )
        .await;
        loop {
            match read_frame(&mut self.stream).await {
                ServerFrame::Response {
                    request_id: response_id,
                    result,
                } if response_id == request_id => return Ok(result),
                ServerFrame::Error {
                    request_id: Some(response_id),
                    error,
                } if response_id == request_id => return Err(error),
                ServerFrame::Event { .. } => {}
                frame => panic!("unexpected response: {frame:?}"),
            }
        }
    }

    async fn next_event(&mut self, subscription_id: u64) -> (u64, SubscriptionEvent) {
        loop {
            match read_frame(&mut self.stream).await {
                ServerFrame::Event {
                    subscription_id: event_subscription,
                    sequence,
                    event,
                } if event_subscription == subscription_id => return (sequence, event),
                ServerFrame::Event { .. } => {}
                frame => panic!("unexpected subscription frame: {frame:?}"),
            }
        }
    }

    async fn live_incarnation(&mut self, splint_id: SplintId) -> u64 {
        match self.request(Request::InspectSplint { splint_id }).await {
            Response::Splint { runtime, .. } if runtime.splint_id == splint_id => runtime
                .live_incarnation
                .expect("created Splint must have a live incarnation"),
            response => panic!("unexpected targeted identity response: {response:?}"),
        }
    }

    async fn subscribe_control(&mut self, splint_id: SplintId, incarnation: u64) -> u64 {
        match self
            .request(Request::SubscribeControl {
                splint_id,
                incarnation,
            })
            .await
        {
            Response::ControlSubscribed {
                subscription_id,
                status,
            } => {
                status.validate().unwrap();
                subscription_id
            }
            response => panic!("unexpected control subscription response: {response:?}"),
        }
    }

    async fn acquire_control(&mut self, splint_id: SplintId, incarnation: u64) -> u64 {
        if let Some(controller_id) = self.controller_id {
            return controller_id;
        }
        let Response::ControlGranted { controller_id, .. } = self
            .request(Request::AcquireControl {
                splint_id,
                incarnation,
                modes: vec![ControlMode::Input],
            })
            .await
        else {
            panic!("control was not granted");
        };
        self.controller_id = Some(controller_id);
        controller_id
    }

    async fn release_control(&mut self) {
        let controller_id = self.controller_id.take().expect("controller owned");
        assert_eq!(
            self.request(Request::ReleaseControl { controller_id })
                .await,
            Response::Acknowledged
        );
    }

    async fn input(&mut self, splint_id: SplintId, incarnation: u64, bytes: &[u8]) {
        self.send_input(splint_id, incarnation, bytes, false).await;
    }

    /// Sends bytes the way the graphical client sends its focus reports.
    async fn focus_report(&mut self, splint_id: SplintId, incarnation: u64, bytes: &[u8]) {
        self.send_input(splint_id, incarnation, bytes, true).await;
    }

    async fn send_input(
        &mut self,
        splint_id: SplintId,
        incarnation: u64,
        bytes: &[u8],
        focus_report: bool,
    ) {
        let controller_id = self.acquire_control(splint_id, incarnation).await;
        assert!(matches!(
            self.request(Request::Input {
                controller_id,
                splint_id,
                incarnation,
                bytes: bytes.to_vec(),
                focus_report,
            })
            .await,
            Response::TerminalActionAcknowledged {
                splint_id: response_id,
                incarnation: response_incarnation,
                ..
            } if response_id == splint_id && response_incarnation == incarnation
        ));
    }

    async fn resize(&mut self, splint_id: SplintId, incarnation: u64, columns: u16, rows: u16) {
        let controller_id = self.acquire_control(splint_id, incarnation).await;
        assert!(matches!(
            self.request(Request::Resize {
                controller_id,
                splint_id,
                incarnation,
                columns,
                rows,
                pixel_width: 0,
                pixel_height: 0,
            })
            .await,
            Response::TerminalActionAcknowledged {
                splint_id: response_id,
                incarnation: response_incarnation,
                ..
            } if response_id == splint_id && response_incarnation == incarnation
        ));
    }

    async fn attach(&mut self, splint_id: SplintId, incarnation: u64) -> (u64, TerminalSnapshot) {
        self.attach_with_scrollback(splint_id, incarnation, 16)
            .await
    }

    async fn attach_with_scrollback(
        &mut self,
        splint_id: SplintId,
        incarnation: u64,
        scrollback_rows: usize,
    ) -> (u64, TerminalSnapshot) {
        match self
            .request(Request::Attach {
                splint_id,
                incarnation: Some(incarnation),
                scrollback_rows,
            })
            .await
        {
            Response::Attached {
                subscription_id,
                provenance,
                snapshot,
            } => {
                assert_eq!(provenance.splint_id, splint_id);
                assert_eq!(provenance.incarnation, incarnation);
                assert_eq!(provenance.terminal_revision, snapshot.revision);
                assert_eq!(provenance.history_generation, snapshot.history_generation);
                assert_eq!(provenance.title, snapshot.title);
                assert!(provenance.topology_revision.get() > 0);
                (subscription_id, snapshot)
            }
            response => panic!("unexpected attach response: {response:?}"),
        }
    }
}

async fn write_frame(stream: &mut (impl tokio::io::AsyncWrite + Unpin), frame: &ClientFrame) {
    stream
        .write_all(&encode_frame(frame).unwrap())
        .await
        .unwrap();
}

async fn read_frame_or_eof(
    stream: &mut (impl tokio::io::AsyncRead + Unpin),
) -> Option<ServerFrame> {
    let mut length = [0_u8; 4];
    match time::timeout(TEST_TIMEOUT, stream.read_exact(&mut length))
        .await
        .expect("timed out reading frame length")
    {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return None,
        Err(error) => panic!("failed reading frame length: {error}"),
    }
    let length = u32::from_be_bytes(length) as usize;
    assert!((1..=MAX_FRAME_BYTES).contains(&length));
    let mut body = vec![0_u8; length];
    match time::timeout(TEST_TIMEOUT, stream.read_exact(&mut body))
        .await
        .expect("timed out reading frame body")
    {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return None,
        Err(error) => panic!("failed reading frame body: {error}"),
    }
    Some(serde_json::from_slice(&body).unwrap())
}

async fn read_frame(stream: &mut (impl tokio::io::AsyncRead + Unpin)) -> ServerFrame {
    read_frame_or_eof(stream)
        .await
        .expect("connection closed before the expected frame")
}

fn snapshot_text(snapshot: &TerminalSnapshot) -> String {
    snapshot
        .scrollback_rows
        .iter()
        .chain(&snapshot.visible_rows)
        .flat_map(|row| row.cells.iter())
        .map(|cell| cell.content.as_str())
        .collect()
}

fn update_text(update: &TerminalUpdate) -> String {
    update
        .rows
        .iter()
        .map(|patch| &patch.row)
        .chain(
            update
                .scrollback
                .iter()
                .flat_map(|scrollback| scrollback.rows.iter()),
        )
        .flat_map(|row| row.cells.iter())
        .map(|cell| cell.content.as_str())
        .collect()
}

fn apply_terminal_update(snapshot: &mut TerminalSnapshot, update: TerminalUpdate) {
    update
        .validate_against(
            snapshot.revision,
            snapshot.history_generation,
            snapshot.columns,
            snapshot.rows,
        )
        .expect("subscription update validates against reconstructed state");
    assert_eq!(update.columns.unwrap_or(snapshot.columns), snapshot.columns);
    assert_eq!(update.row_count.unwrap_or(snapshot.rows), snapshot.rows);
    for patch in update.rows {
        snapshot.visible_rows[patch.index] = patch.row;
    }
    if let Some(scrollback) = update.scrollback {
        let append = matches!(scrollback.transition, HistoryTransition::Append { .. });
        match scrollback.transition {
            HistoryTransition::Append { trimmed_rows, .. } => {
                // Server trims first consume history omitted from our bounded tail.
                let cached_trimmed = trimmed_rows
                    .saturating_sub(snapshot.omitted_oldest_scrollback_rows)
                    .min(snapshot.scrollback_rows.len());
                snapshot.scrollback_rows.drain(..cached_trimmed);
                snapshot.scrollback_rows.extend(scrollback.rows.clone());
                // Reconstruct a wire snapshot, not an unbounded client history cache.
                let excess = snapshot
                    .scrollback_rows
                    .len()
                    .saturating_sub(MAX_SNAPSHOT_SCROLLBACK_ROWS);
                snapshot.scrollback_rows.drain(..excess);
            }
            HistoryTransition::Clear => snapshot.scrollback_rows.clear(),
            HistoryTransition::Reflow | HistoryTransition::Replace => {
                snapshot.scrollback_rows.clone_from(&scrollback.rows);
            }
        }
        snapshot.history_generation = scrollback.history_generation;
        snapshot.oldest_available_scrollback_row_id = scrollback.oldest_available_row_id;
        snapshot.newest_available_scrollback_row_id = scrollback.newest_available_row_id;
        snapshot.available_scrollback_rows = scrollback.available_rows;
        snapshot.omitted_oldest_scrollback_rows = if append {
            scrollback
                .available_rows
                .saturating_sub(snapshot.scrollback_rows.len())
        } else {
            scrollback.omitted_oldest_rows
        };
    }
    if let Some(cursor) = update.cursor {
        snapshot.cursor_column = cursor.column;
        snapshot.cursor_row = cursor.row;
        snapshot.cursor_deferred_wrap = cursor.deferred_wrap;
    }
    if let Some(title) = update.title {
        snapshot.title = title;
    }
    if let Some(modes) = update.input_modes {
        snapshot.input_modes = modes;
    }
    if let Some(screen) = update.active_screen {
        snapshot.active_screen = screen;
    }
    if let Some(palette) = update.palette {
        snapshot.palette = palette;
    }
    if let Some(colors) = update.default_colors {
        snapshot.default_colors = colors;
    }
    if let Some(images) = update.images {
        snapshot.images = Some(images);
    }
    snapshot.revision = update.revision;
    snapshot.validate().unwrap_or_else(|error| {
        panic!(
            "reconstructed subscription snapshot remains valid: {error:?}; rows={} available={} omitted={} oldest={:?} newest={:?} revision={}",
            snapshot.scrollback_rows.len(),
            snapshot.available_scrollback_rows,
            snapshot.omitted_oldest_scrollback_rows,
            snapshot.oldest_available_scrollback_row_id,
            snapshot.newest_available_scrollback_row_id,
            snapshot.revision,
        )
    });
}

#[allow(
    clippy::too_many_arguments,
    reason = "terminal provenance is intentionally complete"
)]
fn assert_terminal_provenance(
    provenance: &TerminalProvenance,
    lair_id: LairId,
    dojo_id: DojoId,
    splint_id: SplintId,
    incarnation: u64,
    title: &str,
    terminal_revision: u64,
    history_generation: u64,
) {
    assert_eq!(provenance.lair_id, lair_id);
    assert_eq!(provenance.dojo_id, dojo_id);
    assert_eq!(provenance.splint_id, splint_id);
    assert_eq!(provenance.incarnation, incarnation);
    assert!(provenance.topology_revision.get() > 0);
    assert_eq!(provenance.title, title);
    assert_eq!(provenance.terminal_revision, terminal_revision);
    assert_eq!(provenance.history_generation, history_generation);
}

async fn visible_snapshot_until(
    connection: &mut Connection,
    splint_id: SplintId,
    incarnation: u64,
    marker: &str,
) -> TerminalSnapshot {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (subscription_id, snapshot) = connection
            .attach_with_scrollback(splint_id, incarnation, 0)
            .await;
        assert_eq!(
            connection
                .request(Request::Detach { subscription_id })
                .await,
            Response::Acknowledged
        );
        if snapshot_text(&snapshot).contains(marker) {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "visible snapshot never contained {marker}"
        );
        time::sleep(Duration::from_millis(20)).await;
    }
}

async fn snapshot_until(
    connection: &mut Connection,
    splint_id: SplintId,
    incarnation: u64,
    marker: &str,
) -> TerminalSnapshot {
    snapshot_until_with_timeout(
        connection,
        splint_id,
        incarnation,
        marker,
        Duration::from_secs(10),
    )
    .await
}

async fn snapshot_until_with_timeout(
    connection: &mut Connection,
    splint_id: SplintId,
    incarnation: u64,
    marker: &str,
    timeout: Duration,
) -> TerminalSnapshot {
    let deadline = Instant::now() + timeout;
    loop {
        let (subscription_id, snapshot) = connection.attach(splint_id, incarnation).await;
        assert_eq!(
            connection
                .request(Request::Detach { subscription_id })
                .await,
            Response::Acknowledged
        );
        if snapshot_text(&snapshot).contains(marker) {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "snapshot never contained {marker}"
        );
        time::sleep(Duration::from_millis(20)).await;
    }
}

async fn stable_snapshot_after_marker(
    connection: &mut Connection,
    splint_id: SplintId,
    incarnation: u64,
    marker: &str,
) -> TerminalSnapshot {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut snapshot = snapshot_until(connection, splint_id, incarnation, marker).await;
    let mut stable_samples = 0;
    loop {
        time::sleep(Duration::from_millis(20)).await;
        let next = snapshot_until(connection, splint_id, incarnation, marker).await;
        if next.revision == snapshot.revision {
            stable_samples += 1;
            if stable_samples == 5 {
                return next;
            }
        } else {
            stable_samples = 0;
            snapshot = next;
        }
        assert!(
            Instant::now() < deadline,
            "snapshot revision did not become quiescent after {marker}"
        );
    }
}

async fn wait_for_pid_marker(marker: &Path, label: &str) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(contents) = fs::read_to_string(marker)
            && let Ok(pid) = contents.trim().parse()
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "{label} PID marker was not completed"
        );
        time::sleep(Duration::from_millis(10)).await;
    }
}

fn policy_executable_identity() -> (PathBuf, String) {
    let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
    let mut file = fs::File::open(&executable).unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    let sha256 = format!("{:x}", Sha256::digest(bytes));
    (executable, sha256)
}

fn exact_headless_policy(splint: Option<(SplintId, u64)>) -> String {
    let (executable, sha256) = policy_executable_identity();
    let mut scopes = vec![
        "topology_metadata_read",
        "process_spawn",
        "topology_layout_mutate",
        "audit_inspect",
        "topology_subscribe",
    ];
    let mut resources = vec![serde_json::json!({"kind": "daemon"})];
    if let Some((splint_id, incarnation)) = splint {
        scopes.extend([
            "terminal_visible_read",
            "terminal_subscribe",
            "controller_acquire",
            "input",
        ]);
        resources.push(serde_json::json!({
            "kind": "splint",
            "splint_id": splint_id,
            "incarnation": incarnation
        }));
    }
    serde_json::json!({
        "schema": "splinterm.policy.v2",
        "rules": [{
            "id": "headless-test",
            "executable": {"path": executable, "sha256": sha256},
            "scopes": scopes,
            "resources": resources,
            "limits": {
                "max_spawn_count": 1,
                "max_results": 16,
                "max_live_subscriptions": 2
            }
        }]
    })
    .to_string()
}

fn current_metadata_policy(splint_id: SplintId) -> String {
    let (executable, sha256) = policy_executable_identity();
    serde_json::json!({
        "schema": "splinterm.policy.v2",
        "rules": [{
            "id": "restorable-metadata-test",
            "executable": {"path": executable, "sha256": sha256},
            "scopes": ["topology_metadata_read", "audit_inspect"],
            "resources": [
                {"kind": "daemon"},
                {
                    "kind": "splint",
                    "splint_id": splint_id,
                    "incarnation": "current"
                }
            ],
            "limits": {"max_returned_bytes": 65536, "max_results": 16}
        }]
    })
    .to_string()
}

fn scoped_authorization_policy(splint_id: SplintId, incarnation: u64, scopes: &[&str]) -> String {
    let (executable, sha256) = policy_executable_identity();
    let mut resources = vec![serde_json::json!({
        "kind": "splint",
        "splint_id": splint_id,
        "incarnation": incarnation
    })];
    if scopes.contains(&"audit_inspect") {
        resources.push(serde_json::json!({"kind": "daemon"}));
    }
    serde_json::json!({
        "schema": "splinterm.policy.v2",
        "rules": [{
            "id": "authorization-only-test",
            "executable": {"path": executable, "sha256": sha256},
            "scopes": scopes,
            "resources": resources,
            "limits": {
                "max_returned_rows": 16,
                "max_results": 16,
                "max_returned_bytes": 1_048_576,
                "max_live_subscriptions": 2,
                "max_spawn_count": 1,
                "deadline_ms": 10
            }
        }]
    })
    .to_string()
}

fn parent_snapshot_policy(lair_id: splinterm_core::LairId) -> String {
    let (executable, sha256) = policy_executable_identity();
    serde_json::json!({
        "schema": "splinterm.policy.v2",
        "rules": [{
            "id": "parent-snapshot-test",
            "executable": {"path": executable, "sha256": sha256},
            "scopes": [
                "topology_metadata_read",
                "process_spawn",
                "topology_layout_mutate",
                "terminal_visible_read",
                "terminal_subscribe",
                "scrollback_read"
            ],
            "resources": [
                {"kind": "daemon"},
                {"kind": "lair", "lair_id": lair_id}
            ],
            "limits": {
                "max_spawn_count": 2,
                "max_results": 16,
                "max_returned_rows": 16,
                "max_live_subscriptions": 1
            }
        }]
    })
    .to_string()
}

async fn assert_connection_closed(connection: &mut Connection, reason: &str) {
    let mut notified = false;
    for _ in 0..16 {
        let frame = time::timeout(
            Duration::from_secs(5),
            read_frame_or_eof(&mut connection.stream),
        )
        .await
        .unwrap_or_else(|_| panic!("{reason} did not notify the existing client"));
        match frame {
            Some(ServerFrame::Error {
                request_id: None,
                error:
                    ProtocolError {
                        code: ErrorCode::Unauthorized,
                        ref message,
                        ..
                    },
            }) if message == "persistent policy reloaded; reconnect required" => {
                notified = true;
                break;
            }
            Some(ServerFrame::Event { .. }) => {}
            frame => panic!("{reason} returned an unexpected reload frame: {frame:?}"),
        }
    }
    assert!(notified, "{reason} omitted the policy reload diagnostic");
    let closed = time::timeout(
        Duration::from_secs(5),
        read_frame_or_eof(&mut connection.stream),
    )
    .await
    .unwrap_or_else(|_| panic!("{reason} did not close the existing client"));
    assert!(closed.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn human_roles_require_their_exact_installed_graphical_processes() {
    time::timeout(Duration::from_secs(30), async {
        let daemon = Daemon::start().await;
        for (role, expected) in [
            (
                ClientRole::TrustedUi,
                "trusted UI role requires the installed graphical client",
            ),
            (
                ClientRole::RemoteInteractive,
                "remote interactive role requires the installed graphical relay",
            ),
        ] {
            let mut stream = UnixStream::connect(&daemon.socket).await.unwrap();
            write_frame(
                &mut stream,
                &ClientFrame::Hello {
                    minimum_version: PROTOCOL_VERSION,
                    maximum_version: PROTOCOL_VERSION,
                    role,
                    handoff_window: false,
                },
            )
            .await;
            assert!(matches!(
                read_frame(&mut stream).await,
                ServerFrame::Error {
                    request_id: None,
                    error: ProtocolError {
                        code: ErrorCode::Unauthorized,
                        ref message,
                        ..
                    },
                } if message == expected
            ));
        }
        let mut automation = daemon.connect().await;
        assert!(matches!(
            automation.request(Request::Ping).await,
            Response::Pong
        ));
        let revision = automation.topology_revision().await;
        let denied = automation
            .request_result(Request::CreateTransientLair {
                expected_topology_revision: revision,
                name: "automation-must-not-own-transient".into(),
                launch: LaunchParameters {
                    cwd: daemon.runtime.clone(),
                    command: vec!["/bin/true".into()],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 1_000,
                },
                generated_name: false,
            })
            .await
            .unwrap_err();
        assert_eq!(denied.code, ErrorCode::Unauthorized);
        let denied = automation
            .request_result(Request::MaterializePreset {
                expected_topology_revision: revision,
                target: splinterm_protocol::PresetTarget::NewLair {
                    name: "automation-must-not-materialize-preset".into(),
                },
                dojos: Vec::new(),
                directory_identities: Vec::new(),
            })
            .await
            .unwrap_err();
        assert_eq!(denied.code, ErrorCode::Unauthorized);
        let Response::Topology { snapshot } = automation.request(Request::InspectTopology).await
        else {
            panic!("topology response was not returned");
        };
        assert!(snapshot.topology.lairs().next().is_none());
        daemon.shutdown();
    })
    .await
    .expect("human-role identity validation timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one isolated scenario proves every handoff request refusal and its audit"
)]
async fn daemon_handoff_vocabulary_is_trusted_ui_only_and_refusals_are_audited() {
    time::timeout(Duration::from_secs(30), async {
        // Development terminal access is on, so audit inspection needs no
        // policy; it still never authorizes a trusted-UI-only request.
        let daemon = Daemon::start().await;
        let mut stream = UnixStream::connect(&daemon.socket).await.unwrap();
        write_frame(
            &mut stream,
            &ClientFrame::Hello {
                minimum_version: PROTOCOL_VERSION,
                maximum_version: PROTOCOL_VERSION,
                role: ClientRole::Automation,
                handoff_window: false,
            },
        )
        .await;
        assert!(matches!(
            read_frame(&mut stream).await,
            ServerFrame::Hello {
                version: PROTOCOL_VERSION,
                daemon_generation: 1,
                ..
            }
        ));
        drop(stream);

        let mut automation = daemon.connect().await;
        for request in [
            Request::PrepareDaemonHandoff {
                mode: splinterm_protocol::DaemonHandoffMode::DryRun,
            },
            Request::PrepareDaemonHandoff {
                mode: splinterm_protocol::DaemonHandoffMode::Execute,
            },
            Request::ClaimHandoffControl {
                ticket: splinterm_protocol::HandoffResumeTicket::new(
                    [0x5a; splinterm_protocol::HANDOFF_RESUME_TICKET_BYTES],
                ),
                splint: None,
            },
            Request::AcknowledgeHandoffPrepare {
                daemon_generation: 1,
                prepare_id: 1,
            },
        ] {
            let denied = automation.request_result(request).await.unwrap_err();
            assert_eq!(denied.code, ErrorCode::Unauthorized);
            assert_eq!(
                denied.message,
                "request requires the installed trusted graphical client"
            );
            assert!(denied.handoff_blocking_reasons.is_empty());
        }
        let Response::AuditPage { page } = automation
            .request(Request::AuditInspect {
                after_audit_id: None,
                max_records: 16,
            })
            .await
        else {
            panic!("audit page was not returned");
        };
        let refusals = page
            .records
            .iter()
            .filter(|record| {
                matches!(
                    record.operation,
                    splinterm_protocol::AuditOperation::PrepareDaemonHandoff
                        | splinterm_protocol::AuditOperation::ClaimHandoffControl
                        | splinterm_protocol::AuditOperation::AcknowledgeHandoffPrepare
                )
            })
            .map(|record| {
                (
                    record.operation,
                    record.decision,
                    record.reason.as_str(),
                    record.outcome,
                )
            })
            .collect::<Vec<_>>();
        let denied = |operation| {
            (
                operation,
                splinterm_protocol::AuditDecision::Denied,
                "policy_rejected",
                Some(splinterm_protocol::AuditOutcome::Failed),
            )
        };
        assert_eq!(
            refusals,
            [
                denied(splinterm_protocol::AuditOperation::PrepareDaemonHandoff),
                denied(splinterm_protocol::AuditOperation::PrepareDaemonHandoff),
                denied(splinterm_protocol::AuditOperation::ClaimHandoffControl),
                denied(splinterm_protocol::AuditOperation::AcknowledgeHandoffPrepare),
            ]
        );
        // Refusal leaves the connection usable.
        assert!(matches!(
            automation.request(Request::Ping).await,
            Response::Pong
        ));
        daemon.shutdown();
    })
    .await
    .expect("daemon handoff authorization scenario timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one isolated scenario proves the complete mutation preflight authority boundary"
)]
async fn mutation_preflight_preserves_exact_scope_cas_and_descendant_denial() {
    time::timeout(Duration::from_secs(60), async {
        let daemon = Daemon::start_with_policy(&exact_headless_policy(None)).await;
        let mut setup = daemon.connect().await;
        let revision = setup.topology_revision().await;
        let Response::LairCreated {
            lair: dojo,
            incarnation,
            ..
        } = setup
            .request(Request::CreateLair {
                expected_topology_revision: revision,
                name: "mutation-preflight".to_owned(),
                launch: LaunchParameters {
                    cwd: daemon.runtime.clone(),
                    command: vec![
                        "/bin/sh".to_owned(),
                        "-c".to_owned(),
                        "sleep 3600".to_owned(),
                    ],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 100,
                },
                generated_name: false,
            })
            .await
        else {
            panic!("mutation setup did not create a Lair")
        };
        let LayoutNode::Leaf(splint) = &dojo.dojos[0].root else {
            panic!("mutation setup root was not a leaf")
        };
        let splint_id = splint.id;
        let policy = daemon.policy.as_ref().unwrap();
        fs::write(
            policy,
            scoped_authorization_policy(splint_id, incarnation, &["topology_layout_mutate"]),
        )
        .unwrap();
        fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.reload_policy();
        assert_connection_closed(&mut setup, "missing process-spawn policy reload").await;
        time::sleep(Duration::from_millis(50)).await;
        let mut missing_scope = daemon.connect().await;
        assert_eq!(
            missing_scope
                .request_result(Request::PrepareMutation {
                    mutation: MutationPreflight::SplitSplint { splint_id },
                })
                .await
                .expect_err("split preflight without process-spawn must fail")
                .code,
            ErrorCode::Unauthorized
        );

        fs::write(
            policy,
            scoped_authorization_policy(splint_id, incarnation, &["process_spawn"]),
        )
        .unwrap();
        fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.reload_policy();
        assert_connection_closed(&mut missing_scope, "missing layout scope policy reload").await;
        time::sleep(Duration::from_millis(50)).await;
        let mut missing_scope = daemon.connect().await;
        assert_eq!(
            missing_scope
                .request_result(Request::PrepareMutation {
                    mutation: MutationPreflight::SplitSplint { splint_id },
                })
                .await
                .expect_err("split preflight without layout scope must fail")
                .code,
            ErrorCode::Unauthorized
        );

        fs::write(
            policy,
            scoped_authorization_policy(
                splint_id,
                incarnation,
                &["process_spawn", "topology_layout_mutate", "audit_inspect"],
            ),
        )
        .unwrap();
        fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.reload_policy();
        assert_connection_closed(&mut missing_scope, "mutation policy reload").await;
        time::sleep(Duration::from_millis(50)).await;

        let mut scoped = daemon.connect().await;
        assert_eq!(
            scoped
                .request_result(Request::InspectTopology)
                .await
                .expect_err("mutation-only policy must not permit topology inspection")
                .code,
            ErrorCode::Unauthorized
        );
        let Response::AuditPage { page: baseline } = scoped
            .request(Request::AuditInspect {
                after_audit_id: None,
                max_records: 16,
            })
            .await
        else {
            panic!("audit baseline was not returned")
        };
        let baseline_audit_id = baseline.records.last().map(|record| record.audit_id);

        // A completed preflight followed by client cancellation/disconnect is not
        // a committed mutation and must leave no successful mutation audit.
        let mut cancelled = daemon.connect().await;
        assert!(matches!(
            cancelled
                .request(Request::PrepareMutation {
                    mutation: MutationPreflight::SplitSplint { splint_id },
                })
                .await,
            Response::MutationPrepared { .. }
        ));
        drop(cancelled);

        let Response::MutationPrepared { preparation } = scoped
            .request(Request::PrepareMutation {
                mutation: MutationPreflight::SplitSplint { splint_id },
            })
            .await
        else {
            panic!("split preflight did not return scoped preparation")
        };
        assert_eq!(preparation.splint_id, Some(splint_id));
        assert_eq!(preparation.incarnation, Some(incarnation));
        assert!(preparation.targets.is_empty());
        let Response::SplintStarted {
            splint_id: child_id,
            topology_revision,
            ..
        } = scoped
            .request(Request::SplitSplintAutomation {
                expected_topology_revision: preparation.topology_revision,
                target_splint_id: splint_id,
                axis: Axis::Horizontal,
                side: SplitSide::Second,
                ratio: SplitRatio::new(500).unwrap(),
                launch: AutomationLaunch {
                    cwd: Some(daemon.runtime.clone()),
                    argv: vec![
                        "/bin/sh".to_owned(),
                        "-c".to_owned(),
                        "sleep 3600".to_owned(),
                    ],
                },
            })
            .await
        else {
            panic!("authorized split did not commit")
        };
        assert_eq!(
            topology_revision.get(),
            preparation.topology_revision.get() + 1
        );
        assert_eq!(
            scoped
                .request_result(Request::PrepareMutation {
                    mutation: MutationPreflight::SetSplitRatio {
                        splint_id: child_id
                    },
                })
                .await
                .expect_err("new descendant must remain outside publication snapshot")
                .code,
            ErrorCode::Unauthorized
        );
        assert_eq!(
            scoped
                .request_result(Request::SetSplitRatio {
                    expected_topology_revision: preparation.topology_revision,
                    target_splint_id: splint_id,
                    ancestor: 0,
                    ratio: SplitRatio::new(400).unwrap(),
                })
                .await
                .expect_err("stale mutation CAS must fail")
                .code,
            ErrorCode::StaleTopology
        );
        let Response::AuditPage { page } = scoped
            .request(Request::AuditInspect {
                after_audit_id: baseline_audit_id,
                max_records: 16,
            })
            .await
        else {
            panic!("post-mutation audit page was not returned")
        };
        let split_records = page
            .records
            .iter()
            .filter(|record| record.operation == splinterm_protocol::AuditOperation::SplitSplint)
            .collect::<Vec<_>>();
        assert_eq!(split_records.len(), 1, "preflight polluted mutation audit");
        assert_eq!(
            split_records[0].outcome,
            Some(splinterm_protocol::AuditOutcome::Succeeded)
        );
        assert_eq!(
            split_records[0].decision,
            splinterm_protocol::AuditDecision::Matched
        );
        daemon.shutdown();
    })
    .await
    .expect("mutation preflight scenario timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one real-daemon lifecycle proves both runtime-only MCP mutation revision contracts"
)]
async fn runtime_relaunch_and_restore_preserve_topology_revision() {
    time::timeout(Duration::from_secs(60), async {
        let daemon = Daemon::start().await;
        let mut client = daemon.connect().await;
        let revision = client.topology_revision().await;
        let Response::LairCreated {
            lair: dojo,
            incarnation: first_incarnation,
            topology_revision,
        } = client
            .request(Request::CreateLair {
                expected_topology_revision: revision,
                name: "runtime-relaunch".to_owned(),
                launch: LaunchParameters {
                    cwd: daemon.runtime.clone(),
                    command: vec!["/bin/sh".to_owned(), "-c".to_owned(), "exit 0".to_owned()],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 100,
                },
                generated_name: false,
            })
            .await
        else {
            panic!("runtime relaunch setup did not create a Lair")
        };
        let LayoutNode::Leaf(splint) = &dojo.dojos[0].root else {
            panic!("runtime relaunch setup was not a leaf")
        };
        let splint_id = splint.id;
        assert_eq!(topology_revision.get(), revision.get() + 1);

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let Response::Splint { runtime, .. } =
                client.request(Request::InspectSplint { splint_id }).await
            else {
                panic!("runtime status was not returned")
            };
            if runtime.live_incarnation.is_none() && runtime.restorable {
                break;
            }
            assert!(Instant::now() < deadline, "initial process did not exit");
            time::sleep(Duration::from_millis(20)).await;
        }

        let relaunch = splinterm_mcp::dispatch_mutation_for_integration_test(
            "splinterm.relaunch_splint",
            &serde_json::json!({
                "splint_id": splint_id,
                "cwd": daemon.runtime.to_string_lossy(),
                "argv": ["/bin/sh", "-c", "exit 0"]
            }),
            &daemon.socket,
        )
        .await
        .expect("MCP runtime relaunch did not commit");
        assert_eq!(relaunch["ok"], true);
        assert_eq!(
            relaunch["resource"]["topology_revision"],
            topology_revision.get()
        );
        let second_incarnation = relaunch["resource"]["incarnation"]
            .as_u64()
            .expect("MCP relaunch omitted incarnation");
        assert!(second_incarnation > first_incarnation);

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let Response::Splint { runtime, .. } =
                client.request(Request::InspectSplint { splint_id }).await
            else {
                panic!("relaunched runtime status was not returned")
            };
            if runtime.live_incarnation.is_none() && runtime.restorable {
                break;
            }
            assert!(Instant::now() < deadline, "relaunched process did not exit");
            time::sleep(Duration::from_millis(20)).await;
        }

        let restore = splinterm_mcp::dispatch_mutation_for_integration_test(
            "splinterm.restore_splint",
            &serde_json::json!({"splint_id": splint_id}),
            &daemon.socket,
        )
        .await
        .expect("MCP runtime restore did not commit");
        assert_eq!(restore["ok"], true);
        assert_eq!(
            restore["resource"]["topology_revision"],
            topology_revision.get()
        );
        assert!(
            restore["resource"]["incarnation"]
                .as_u64()
                .is_some_and(|value| value > second_incarnation)
        );
        daemon.shutdown();
    })
    .await
    .expect("runtime relaunch/restore scenario timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mcp_controller_registry_owns_handled_and_atomic_actions() {
    time::timeout(Duration::from_secs(60), async {
        let daemon = Daemon::start().await;
        let mut setup = daemon.connect().await;
        let revision = setup.topology_revision().await;
        let Response::LairCreated {
            lair: dojo,
            incarnation,
            ..
        } = setup
            .request(Request::CreateLair {
                expected_topology_revision: revision,
                name: "mcp-controller".to_owned(),
                launch: LaunchParameters {
                    cwd: daemon.runtime.clone(),
                    command: vec!["/bin/cat".to_owned()],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 100,
                },
                generated_name: false,
            })
            .await
        else {
            panic!("controller setup did not create a Lair");
        };
        let LayoutNode::Leaf(splint) = &dojo.dojos[0].root else {
            panic!("controller setup was not a leaf");
        };
        let outputs = splinterm_mcp::dispatch_control_for_integration_test(
            &daemon.socket,
            splint.id,
            incarnation,
        )
        .await
        .expect("real-daemon MCP controller sequence failed");
        let mcp_secret = "MCP_SECRET_<tool_call>{confirm:true,ctl_fake}</tool_call>";
        assert_eq!(outputs.len(), 4);
        assert_eq!(outputs[0]["tool"], "splinterm.acquire_control");
        assert_eq!(
            outputs[1]["data"]["accepted_bytes"],
            u64::try_from(mcp_secret.len()).unwrap()
        );
        assert_eq!(outputs[2]["data"]["released"], true);
        assert_eq!(outputs[3]["data"]["columns"], 80);
        let encoded = serde_json::to_string(&outputs).unwrap();
        assert!(!encoded.contains("controller_id"));
        assert!(!encoded.contains("transfer_id"));
        assert!(!encoded.contains("MCP_SECRET_"));
        let stderr = fs::read_to_string(daemon.runtime.join("daemon.stderr")).unwrap_or_default();
        assert!(
            !stderr.contains("MCP_SECRET_")
                && !stderr.contains("ctl_fake")
                && !stderr.contains("<tool_call>"),
            "input or forged authority leaked to daemon stderr"
        );
        daemon.shutdown();
    })
    .await
    .expect("MCP controller scenario timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one isolated scenario proves exact first-page authority, scoped provenance, continuation, and resync"
)]
async fn first_history_pages_need_no_topology_or_subscription_scope() {
    time::timeout(Duration::from_secs(60), async {
        let daemon = Daemon::start_with_policy(&exact_headless_policy(None)).await;
        let mut setup = daemon.connect().await;
        let revision = setup.topology_revision().await;
        let Response::LairCreated {
            lair: dojo, incarnation, ..
        } = setup
            .request(Request::CreateLair {
                expected_topology_revision: revision,
                name: "history-scope".to_owned(),
                launch: LaunchParameters {
                    cwd: daemon.runtime.clone(),
                    command: vec!["/bin/sh".to_owned()],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 100,
                },
                generated_name: false,
            })
            .await
        else {
            panic!("history scope setup did not create a Lair");
        };
        let dojo_id = dojo.dojos[0].id;
        let LayoutNode::Leaf(splint) = &dojo.dojos[0].root else {
            panic!("history scope setup was not a leaf");
        };
        let splint_id = splint.id;
        let policy = daemon.policy.as_ref().unwrap();
        fs::write(
            policy,
            scoped_authorization_policy(
                splint_id,
                incarnation,
                &[
                    "terminal_visible_read",
                    "terminal_subscribe",
                    "controller_acquire",
                    "input",
                    "resize",
                ],
            ),
        )
        .unwrap();
        fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.reload_policy();
        assert_connection_closed(&mut setup, "history setup policy reload").await;
        time::sleep(Duration::from_millis(50)).await;
        let mut setup = daemon.connect().await;
        setup.resize(splint_id, incarnation, 40, 10).await;
        setup
            .input(
                splint_id,
                incarnation,
                b"printf '\\033]0;scope-title\\007'; i=1; while [ $i -le 160 ]; do echo line-$i; i=$((i+1)); done; echo needle; printf '\\163\\143\\157\\160\\145\\055\\162\\145\\141\\144\\171\\n'\n",
            )
            .await;
        let setup_snapshot =
            visible_snapshot_until(&mut setup, splint_id, incarnation, "scope-ready").await;
        assert_eq!(setup_snapshot.rows, 10);
        assert!(
            setup_snapshot.available_scrollback_rows > 0,
            "setup snapshot did not retain history: {setup_snapshot:?}"
        );
        let Response::Attached {
            subscription_id,
            provenance: attached_provenance,
            snapshot: attached_snapshot,
        } = setup
            .request(Request::Attach {
                splint_id,
                incarnation: None,
                scrollback_rows: 0,
            })
            .await
        else {
            panic!("current-incarnation attach did not return a snapshot");
        };
        assert_terminal_provenance(
            &attached_provenance,
            dojo.id,
            dojo_id,
            splint_id,
            incarnation,
            "scope-title",
            attached_snapshot.revision,
            attached_snapshot.history_generation,
        );
        assert_eq!(
            setup.request(Request::Detach { subscription_id }).await,
            Response::Acknowledged
        );

        fs::write(
            policy,
            scoped_authorization_policy(splint_id, incarnation, &["scrollback_read"]),
        )
        .unwrap();
        fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.reload_policy();
        assert_connection_closed(&mut setup, "missing-visible-read policy reload").await;
        time::sleep(Duration::from_millis(50)).await;
        let mut missing_scope = daemon.connect().await;
        assert_eq!(
            missing_scope
                .request_result(Request::StartScrollbackPage {
                    splint_id,
                    incarnation: None,
                    max_rows: 8,
                })
                .await
                .expect_err("scrollback without terminal-visible scope must fail")
                .code,
            ErrorCode::Unauthorized
        );

        fs::write(
            policy,
            scoped_authorization_policy(splint_id, incarnation, &["terminal_visible_read"]),
        )
        .unwrap();
        fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.reload_policy();
        assert_connection_closed(&mut missing_scope, "missing-scrollback policy reload").await;
        time::sleep(Duration::from_millis(50)).await;
        let mut missing_scope = daemon.connect().await;
        assert_eq!(
            missing_scope
                .request_result(Request::StartScrollbackPage {
                    splint_id,
                    incarnation: None,
                    max_rows: 8,
                })
                .await
                .expect_err("terminal-visible without scrollback scope must fail")
                .code,
            ErrorCode::Unauthorized
        );

        fs::write(
            policy,
            scoped_authorization_policy(
                splint_id,
                incarnation,
                &["terminal_visible_read", "scrollback_read"],
            ),
        )
        .unwrap();
        fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.reload_policy();
        assert_connection_closed(&mut missing_scope, "history-read policy reload").await;
        time::sleep(Duration::from_millis(50)).await;

        let mut scoped = daemon.connect().await;
        assert_eq!(
            scoped
                .request_result(Request::InspectTopology)
                .await
                .expect_err("history-only policy must not permit topology reads")
                .code,
            ErrorCode::Unauthorized
        );
        assert_eq!(
            scoped
                .request_result(Request::Attach {
                    splint_id,
                    incarnation: None,
                    scrollback_rows: 0,
                })
                .await
                .expect_err("history-only policy must not permit terminal subscriptions")
                .code,
            ErrorCode::Unauthorized
        );

        let Response::ScrollbackPage { provenance, page } = scoped
            .request(Request::StartScrollbackPage {
                splint_id,
                incarnation: None,
                max_rows: 8,
            })
            .await
        else {
            panic!("scoped first scrollback page was not returned");
        };
        page.validate().unwrap();
        assert_terminal_provenance(
            &provenance,
            dojo.id,
            dojo_id,
            splint_id,
            incarnation,
            "scope-title",
            page.terminal_revision,
            page.history_generation,
        );
        assert!(!page.rows.is_empty(), "first page was empty: {page:?}");

        let before_row_id = page.rows.first().and_then(|row| row.row_id).unwrap();
        let Response::ScrollbackPage {
            provenance: continued_provenance,
            page: continued,
        } = scoped
            .request(Request::ScrollbackPage {
                splint_id,
                incarnation: provenance.incarnation,
                terminal_revision: provenance.terminal_revision,
                history_generation: provenance.history_generation,
                before_row_id,
                max_rows: 4,
            })
            .await
        else {
            panic!("scoped scrollback continuation was not returned");
        };
        continued.validate().unwrap();
        assert_terminal_provenance(
            &continued_provenance,
            dojo.id,
            dojo_id,
            splint_id,
            provenance.incarnation,
            "scope-title",
            continued.terminal_revision,
            continued.history_generation,
        );

        let Response::ScrollbackResyncRequired {
            provenance: scrollback_resync,
            current_revision,
            history_generation,
        } = scoped
            .request(Request::ScrollbackPage {
                splint_id,
                incarnation: provenance.incarnation,
                terminal_revision: provenance.terminal_revision.saturating_add(1),
                history_generation: provenance.history_generation,
                before_row_id,
                max_rows: 4,
            })
            .await
        else {
            panic!("stale scrollback page did not return resync provenance");
        };
        assert_terminal_provenance(
            &scrollback_resync,
            dojo.id,
            dojo_id,
            splint_id,
            provenance.incarnation,
            "scope-title",
            current_revision,
            history_generation,
        );

        assert_eq!(
            scoped
                .request_result(Request::StartSearchScrollback {
                    splint_id,
                    incarnation: None,
                    query: "needle".to_owned(),
                    case_sensitive: false,
                    max_results: 4,
                })
                .await
                .expect_err("scrollback-only policy must not permit search")
                .code,
            ErrorCode::Unauthorized
        );

        fs::write(
            policy,
            scoped_authorization_policy(
                splint_id,
                incarnation,
                &[
                    "terminal_visible_read",
                    "scrollback_read",
                    "scrollback_search",
                ],
            ),
        )
        .unwrap();
        fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.reload_policy();
        assert_connection_closed(&mut scoped, "history-search policy reload").await;
        time::sleep(Duration::from_millis(50)).await;

        let mut scoped = daemon.connect().await;
        let Response::SearchResults {
            provenance: search_provenance,
            page: search,
        } = scoped
            .request(Request::StartSearchScrollback {
                splint_id,
                incarnation: None,
                query: "needle".to_owned(),
                case_sensitive: false,
                max_results: 4,
            })
            .await
        else {
            panic!("scoped first search page was not returned");
        };
        search.validate().unwrap();
        assert_terminal_provenance(
            &search_provenance,
            dojo.id,
            dojo_id,
            splint_id,
            incarnation,
            "scope-title",
            search.terminal_revision,
            search.history_generation,
        );
        assert!(search.matches.iter().any(|item| item.preview.contains("needle")));

        let Response::SearchResyncRequired {
            provenance: resync,
            current_revision,
            history_generation,
        } = scoped
            .request(Request::SearchScrollback {
                splint_id,
                incarnation: search_provenance.incarnation,
                terminal_revision: search_provenance.terminal_revision.saturating_add(1),
                history_generation: search_provenance.history_generation,
                query: "needle".to_owned(),
                case_sensitive: false,
                cursor: None,
                max_results: 4,
            })
            .await
        else {
            panic!("stale scoped search did not return resync provenance");
        };
        assert_terminal_provenance(
            &resync,
            dojo.id,
            dojo_id,
            splint_id,
            search_provenance.incarnation,
            "scope-title",
            current_revision,
            history_generation,
        );
        daemon.shutdown();
    })
    .await
    .expect("scoped first history page test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one isolated scenario proves scoped status, grant metadata, revocation, and stale-incarnation behavior"
)]
async fn scoped_authorization_status_needs_no_topology_permission() {
    time::timeout(Duration::from_secs(180), async {
        let mut daemon = Daemon::start_with_policy(&exact_headless_policy(None)).await;
        let mut connection = daemon.connect().await;
        let revision = connection.topology_revision().await;
        let Response::LairCreated {
            lair: dojo,
            incarnation,
            ..
        } = connection
            .request(Request::CreateLair {
                expected_topology_revision: revision,
                name: "authorization-scope".to_owned(),
                launch: LaunchParameters {
                    cwd: daemon.runtime.clone(),
                    command: vec![
                        "/bin/sh".to_owned(),
                        "-c".to_owned(),
                        "sleep 300".to_owned(),
                    ],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 100,
                },
                generated_name: false,
            })
            .await
        else {
            panic!("authorization scope setup did not create a Lair");
        };
        let dojo_id = dojo.dojos[0].id;
        let LayoutNode::Leaf(splint) = &dojo.dojos[0].root else {
            panic!("authorization scope setup was not a leaf");
        };
        let splint_id = splint.id;
        let policy = daemon.policy.as_ref().unwrap();
        fs::write(policy, exact_headless_policy(None)).unwrap();
        fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.reload_policy();
        assert_connection_closed(&mut connection, "wrong-resource setup policy reload").await;
        time::sleep(Duration::from_millis(50)).await;
        let mut connection = daemon.connect().await;
        let second_revision = connection.topology_revision().await;
        let Response::LairCreated {
            lair: other_dojo,
            incarnation: other_incarnation,
            ..
        } = connection
            .request(Request::CreateLair {
                expected_topology_revision: second_revision,
                name: "authorization-other".to_owned(),
                launch: LaunchParameters {
                    cwd: daemon.runtime.clone(),
                    command: vec![
                        "/bin/sh".to_owned(),
                        "-c".to_owned(),
                        "sleep 300".to_owned(),
                    ],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 100,
                },
                generated_name: false,
            })
            .await
        else {
            panic!("authorization wrong-resource setup did not create a Lair");
        };
        let LayoutNode::Leaf(other_splint) = &other_dojo.dojos[0].root else {
            panic!("authorization wrong-resource setup was not a leaf");
        };
        let other_splint_id = other_splint.id;

        fs::write(
            policy,
            scoped_authorization_policy(
                splint_id,
                incarnation,
                &[
                    "authorization_inspect",
                    "authorization_revoke",
                    "terminal_visible_read",
                ],
            ),
        )
        .unwrap();
        fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.reload_policy();
        assert_connection_closed(&mut connection, "authorization-only policy reload").await;
        time::sleep(Duration::from_millis(50)).await;

        let mut scoped = daemon.connect().await;
        let Response::AuthorizationStatus {
            lair_id,
            dojo_id: response_dojo_id,
            incarnation: response_incarnation,
            topology_revision,
            policy_generation,
            persistent,
            development_bypass,
            ..
        } = scoped
            .request(Request::AuthorizationStatus {
                splint_id,
                incarnation: None,
            })
            .await
        else {
            panic!("scoped authorization status response was not returned");
        };
        assert_eq!(lair_id, dojo.id);
        assert_eq!(response_dojo_id, dojo_id);
        assert_eq!(response_incarnation, incarnation);
        assert!(topology_revision.get() > 0);
        assert!(policy_generation > 1);
        assert_eq!(persistent.len(), 1);
        assert!(!development_bypass);

        let Response::AccessGranted {
            lair_id: granted_lair_id,
            dojo_id: granted_dojo_id,
            authorization_revision: granted_revision,
            grant,
        } = scoped
            .request(Request::RequestAccess {
                splint_id,
                incarnation,
                scopes: vec![AccessScope::Observe],
            })
            .await
        else {
            panic!("exact scoped access grant was not returned");
        };
        assert_eq!(granted_lair_id, dojo.id);
        assert_eq!(granted_dojo_id, dojo_id);
        assert_eq!(granted_revision, 1);
        assert!(grant.grant_id > 0);
        assert_eq!(grant.splint_id, splint_id);
        assert_eq!(grant.incarnation, incarnation);

        fs::write(
            policy,
            scoped_authorization_policy(splint_id, incarnation, &["authorization_revoke"]),
        )
        .unwrap();
        fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.reload_policy();
        assert_connection_closed(&mut scoped, "revoke-only policy reload").await;
        time::sleep(Duration::from_millis(50)).await;
        let mut scoped = daemon.connect().await;

        let Response::AccessRevoked {
            lair_id: revoked_lair_id,
            dojo_id: revoked_dojo_id,
            authorization_revision: revoked_revision,
            grant: revoked,
        } = scoped
            .request(Request::RevokeAccess {
                grant_id: grant.grant_id,
            })
            .await
        else {
            panic!("exact scoped access revocation was not returned");
        };
        assert_eq!(revoked_lair_id, dojo.id);
        assert_eq!(revoked_dojo_id, dojo_id);
        assert_eq!(revoked_revision, granted_revision + 1);
        assert_eq!(revoked, grant);
        assert_eq!(
            scoped
                .request_result(Request::AuthorizationStatus {
                    splint_id,
                    incarnation: None,
                })
                .await
                .expect_err("missing authorization_inspect must deny status")
                .code,
            ErrorCode::Unauthorized
        );
        assert_eq!(
            scoped
                .request_result(Request::InspectTopology)
                .await
                .expect_err("revoke-only policy must not permit topology inspection")
                .code,
            ErrorCode::Unauthorized
        );

        fs::write(
            policy,
            scoped_authorization_policy(
                splint_id,
                incarnation,
                &["authorization_inspect", "authorization_revoke"],
            ),
        )
        .unwrap();
        fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.reload_policy();
        assert_connection_closed(&mut scoped, "missing requested-access scope reload").await;
        time::sleep(Duration::from_millis(50)).await;
        let mut scoped = daemon.connect().await;
        assert_eq!(
            scoped
                .request_result(Request::RequestAccess {
                    splint_id,
                    incarnation,
                    scopes: vec![AccessScope::Observe],
                })
                .await
                .expect_err("missing requested terminal access scope must deny")
                .code,
            ErrorCode::ConsentDenied
        );

        fs::write(
            policy,
            scoped_authorization_policy(
                other_splint_id,
                other_incarnation,
                &[
                    "authorization_inspect",
                    "authorization_revoke",
                    "terminal_visible_read",
                ],
            ),
        )
        .unwrap();
        fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.reload_policy();
        assert_connection_closed(&mut scoped, "wrong-resource policy reload").await;
        time::sleep(Duration::from_millis(50)).await;
        let mut scoped = daemon.connect().await;
        assert_eq!(
            scoped
                .request_result(Request::AuthorizationStatus {
                    splint_id,
                    incarnation: None,
                })
                .await
                .expect_err("another Splint policy must not authorize this Splint")
                .code,
            ErrorCode::Unauthorized
        );

        fs::write(
            policy,
            scoped_authorization_policy(
                splint_id,
                incarnation,
                &["authorization_inspect", "terminal_visible_read"],
            ),
        )
        .unwrap();
        fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.reload_policy();
        assert_connection_closed(&mut scoped, "grant setup policy reload").await;
        time::sleep(Duration::from_millis(50)).await;
        let mut scoped = daemon.connect().await;
        let Response::AccessGranted {
            grant: unrevokeable,
            authorization_revision: 3,
            ..
        } = scoped
            .request(Request::RequestAccess {
                splint_id,
                incarnation,
                scopes: vec![AccessScope::Observe],
            })
            .await
        else {
            panic!("missing-revoke setup grant was not returned");
        };
        fs::write(
            policy,
            scoped_authorization_policy(splint_id, incarnation, &["authorization_inspect"]),
        )
        .unwrap();
        fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.reload_policy();
        assert_connection_closed(&mut scoped, "missing authorization_revoke reload").await;
        time::sleep(Duration::from_millis(50)).await;
        let mut scoped = daemon.connect().await;
        assert_eq!(
            scoped
                .request_result(Request::RevokeAccess {
                    grant_id: unrevokeable.grant_id,
                })
                .await
                .expect_err("missing authorization_revoke must deny revocation")
                .code,
            ErrorCode::Unauthorized
        );

        fs::write(policy, r#"{"schema":"splinterm.policy.v2","rules":[]}"#).unwrap();
        fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.reload_policy();
        assert_connection_closed(&mut scoped, "deny-all policy reload").await;
        time::sleep(Duration::from_millis(50)).await;
        let mut scoped = daemon.connect().await;
        assert_eq!(
            scoped
                .request_result(Request::AuthorizationStatus {
                    splint_id,
                    incarnation: None,
                })
                .await
                .expect_err("no-policy status must deny")
                .code,
            ErrorCode::Unauthorized
        );

        fs::write(
            policy,
            scoped_authorization_policy(
                splint_id,
                incarnation,
                &[
                    "authorization_inspect",
                    "authorization_revoke",
                    "terminal_visible_read",
                ],
            ),
        )
        .unwrap();
        fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.reload_policy();
        assert_connection_closed(&mut scoped, "stale-incarnation policy reload").await;
        time::sleep(Duration::from_millis(50)).await;
        let mut scoped = daemon.connect().await;
        assert_eq!(
            scoped
                .request_result(Request::RequestAccess {
                    splint_id,
                    incarnation: incarnation + 1,
                    scopes: vec![AccessScope::Observe],
                })
                .await
                .expect_err("stale exact incarnation must fail closed before consent")
                .code,
            ErrorCode::Unauthorized
        );

        daemon.stop_preserving_state();
        daemon.start_again().await;
        time::sleep(Duration::from_millis(50)).await;
        let mut restarted = daemon.connect().await;
        assert_eq!(
            restarted
                .request_result(Request::AuthorizationStatus {
                    splint_id,
                    incarnation: None,
                })
                .await
                .expect_err("restarted restorable Splint has no current authorization target")
                .code,
            ErrorCode::Unauthorized
        );
        assert_eq!(
            restarted
                .request_result(Request::RequestAccess {
                    splint_id,
                    incarnation,
                    scopes: vec![AccessScope::Observe],
                })
                .await
                .expect_err("exited restorable Splint must deny requested access")
                .code,
            ErrorCode::Unauthorized
        );
        daemon.shutdown();
    })
    .await
    .expect("scoped authorization policy test timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn automation_client_preserves_accepted_and_rejected_reload_diagnostics() {
    time::timeout(Duration::from_secs(60), async {
        let daemon = Daemon::start_with_policy(&exact_headless_policy(None)).await;
        let policy = daemon.policy.as_ref().unwrap();

        for candidate in [
            exact_headless_policy(None),
            r#"{"schema":"wrong","rules":[]}"#.to_owned(),
        ] {
            let mut connection = AutomationConnection::connect_automation_at(&daemon.socket)
                .await
                .unwrap();
            assert!(matches!(
                connection
                    .request(Request::SubscribeTopology)
                    .await
                    .unwrap(),
                Response::TopologySubscribed { .. }
            ));
            fs::write(policy, candidate).unwrap();
            fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
            daemon.reload_policy();

            let error = connection.next_server_frame().await.unwrap_err();
            assert_eq!(
                automation_protocol_error(&error).map(|error| (error.code, error.message.as_str())),
                Some((
                    ErrorCode::Unauthorized,
                    "persistent policy reloaded; reconnect required"
                ))
            );
            let unusable = connection.request(Request::Ping).await.unwrap_err();
            assert!(unusable.to_string().contains("cannot be reused"));
        }

        daemon.shutdown();
    })
    .await
    .expect("automation reload diagnostic scenario timed out");
}

#[allow(
    clippy::too_many_lines,
    reason = "the ordered policy reload, controller cleanup, restart, and process-reap gate is one lifecycle"
)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn headless_policy_reload_fails_closed_and_cleans_up() {
    time::timeout(Duration::from_secs(60), async {
        let mut daemon = Daemon::start_with_policy(&exact_headless_policy(None)).await;
        let marker = daemon.runtime.join("child-pid");
        let mut connection = daemon.connect().await;
        let revision = connection.topology_revision().await;
        let Response::LairCreated {
            lair: dojo,
            incarnation,
            ..
        } = connection
            .request(Request::CreateLair {
                expected_topology_revision: revision,
                name: "headless".into(),
                launch: LaunchParameters {
                    cwd: std::env::current_dir().unwrap(),
                    command: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        format!("printf '%s\\n' $$ > {}; exec sleep 30", marker.display()),
                    ],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 100,
                },
                generated_name: false,
            })
            .await
        else {
            panic!("headless Lair was not created");
        };
        let dojo_id = dojo.dojos[0].id;
        let LayoutNode::Leaf(splint) = &dojo.dojos[0].root else {
            panic!("created headless Lair was not a leaf");
        };
        let splint_id = splint.id;
        let splint_title = splint.title.clone();
        let lair_only_denial = connection
            .request_result(Request::Attach {
                splint_id,
                incarnation: Some(incarnation),
                scrollback_rows: 0,
            })
            .await
            .expect_err("Topology creation authority must not cover the new Lair descendant");
        assert_eq!(lair_only_denial.code, ErrorCode::Unauthorized);
        let child_pid = wait_for_pid_marker(&marker, "child").await;

        let Response::AuditPage { page } = connection
            .request(Request::AuditInspect {
                after_audit_id: None,
                max_records: 16,
            })
            .await
        else {
            panic!("audit page was not returned");
        };
        let create_audit = page
            .records
            .iter()
            .find(|record| record.operation == splinterm_protocol::AuditOperation::CreateLair)
            .expect("authorized create audit record was absent");
        let peer_executable = std::env::current_exe().unwrap().canonicalize().unwrap();
        let peer_metadata = fs::metadata(&peer_executable).unwrap();
        let mut peer_bytes = Vec::new();
        fs::File::open(&peer_executable)
            .unwrap()
            .read_to_end(&mut peer_bytes)
            .unwrap();
        assert_eq!(create_audit.schema, "splinterm.audit.v2");
        assert_eq!(create_audit.retention, "daemon_lifetime");
        assert_ne!(create_audit.audit_id, 0);
        assert_ne!(create_audit.policy_generation, Some(0));
        assert_eq!(
            create_audit.policy_rule_id.as_deref(),
            Some("headless-test")
        );
        assert_eq!(create_audit.peer.uid, rustix::process::geteuid().as_raw());
        assert_eq!(
            create_audit.peer.executable_path,
            peer_executable.to_string_lossy()
        );
        assert_eq!(
            create_audit.peer.executable_sha256,
            format!("{:x}", Sha256::digest(peer_bytes))
        );
        assert_eq!(create_audit.peer.device, Some(peer_metadata.dev()));
        assert_eq!(create_audit.peer.inode, Some(peer_metadata.ino()));
        assert_eq!(create_audit.resource, None);
        assert_eq!(
            create_audit.requested_scopes,
            vec![
                splinterm_protocol::AutomationScope::ProcessSpawn,
                splinterm_protocol::AutomationScope::TopologyLayoutMutate,
            ]
        );
        assert_eq!(
            create_audit.decision,
            splinterm_protocol::AuditDecision::Matched
        );
        assert_eq!(create_audit.reason, "policy_match");
        assert_eq!(
            create_audit.outcome,
            Some(splinterm_protocol::AuditOutcome::Succeeded)
        );
        assert_eq!(create_audit.argument_count, Some(2));
        assert_eq!(create_audit.executable_basename.as_deref(), Some("sh"));

        let policy = daemon.policy.as_ref().unwrap();
        fs::write(
            policy,
            exact_headless_policy(Some((splint_id, incarnation))),
        )
        .unwrap();
        fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.reload_policy();
        assert_connection_closed(&mut connection, "accepted policy reload").await;

        let mut controlled = daemon.connect().await;
        let topology_subscription = match controlled.request(Request::SubscribeTopology).await {
            Response::TopologySubscribed {
                subscription_id, ..
            } => subscription_id,
            response => panic!("unexpected topology subscription response: {response:?}"),
        };
        assert_ne!(topology_subscription, 0);
        let control_subscription = controlled.subscribe_control(splint_id, incarnation).await;
        assert_ne!(control_subscription, 0);
        let controller_id = controlled.acquire_control(splint_id, incarnation).await;
        assert_ne!(controller_id, 0);
        let consent_error = controlled
            .request_result(Request::RequestAccess {
                splint_id,
                incarnation,
                scopes: vec![AccessScope::Resize],
            })
            .await
            .expect_err("under-scoped headless consent request must fail closed");
        assert_eq!(consent_error.code, ErrorCode::ConsentDenied);

        fs::write(policy, r#"{"schema":"wrong","rules":[]}"#).unwrap();
        fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.reload_policy();
        assert_connection_closed(&mut controlled, "rejected policy reload").await;
        let mut denied = daemon.connect().await;
        assert_eq!(
            denied
                .request_result(Request::InspectTopology)
                .await
                .expect_err("rejected reload must install deny-all policy")
                .code,
            ErrorCode::Unauthorized
        );
        fs::write(
            policy,
            exact_headless_policy(Some((splint_id, incarnation))),
        )
        .unwrap();
        fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.reload_policy();
        assert_connection_closed(&mut denied, "accepted recovery policy reload").await;
        let mut reauthorized = daemon.connect().await;
        let replacement_controller = reauthorized.acquire_control(splint_id, incarnation).await;
        assert_ne!(replacement_controller, controller_id);
        drop(reauthorized);
        fs::write(policy, current_metadata_policy(splint_id)).unwrap();
        fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.reload_policy();
        time::sleep(Duration::from_millis(50)).await;

        daemon.stop_preserving_state();
        assert!(!Path::new(&format!("/proc/{child_pid}")).exists());
        let marker_before_restart = fs::read_to_string(&marker).unwrap();
        daemon.start_again().await;
        time::sleep(Duration::from_millis(100)).await;
        assert_eq!(fs::read_to_string(&marker).unwrap(), marker_before_restart);
        let mut restarted = daemon.connect().await;
        let Response::Topology { snapshot } = restarted.request(Request::InspectTopology).await
        else {
            panic!("restarted headless topology was not returned");
        };
        let restarted_runtime = snapshot
            .runtimes
            .iter()
            .find(|runtime| runtime.splint_id == splint_id)
            .expect("persisted Splint runtime metadata was omitted after restart");
        assert_eq!(restarted_runtime.live_incarnation, None);
        assert_eq!(restarted_runtime.last_incarnation, Some(incarnation));
        assert!(restarted_runtime.restorable);
        let restarted_dojo = snapshot.topology.lairs().next().unwrap();
        assert_eq!(restarted_dojo.id, dojo.id);
        let restarted_window = &restarted_dojo.dojos[0];
        assert_eq!(restarted_window.id, dojo_id);
        let LayoutNode::Leaf(restarted_splint) = &restarted_window.root else {
            panic!("restarted persisted topology was not the exact leaf");
        };
        assert_eq!(restarted_splint.id, splint_id);
        assert_eq!(restarted_splint.title, splint_title);
        assert_eq!(restarted_splint.last_incarnation, Some(incarnation));
        assert!(matches!(
            restarted_splint.state,
            splinterm_core::SplintState::Exited(_)
        ));
        let Response::Splint {
            lair_id: inspected_lair_id,
            dojo_id: inspected_dojo_id,
            title: inspected_title,
            topology_revision: inspected_revision,
            runtime: inspected_runtime,
            ..
        } = restarted
            .request(Request::InspectSplint { splint_id })
            .await
        else {
            panic!("restorable Splint targeted inspection was not returned");
        };
        assert_eq!(inspected_lair_id, dojo.id);
        assert_eq!(inspected_dojo_id, dojo_id);
        assert_eq!(inspected_title, splint_title);
        assert_eq!(inspected_revision, snapshot.revision);
        assert_eq!(inspected_runtime, *restarted_runtime);
        let Response::AuditPage { page } = restarted
            .request(Request::AuditInspect {
                after_audit_id: None,
                max_records: 16,
            })
            .await
        else {
            panic!("restarted audit page was not returned");
        };
        assert!(
            page.records.iter().all(|record| {
                record.operation != splinterm_protocol::AuditOperation::CreateLair
            })
        );
        daemon.shutdown();
    })
    .await
    .expect("headless policy integration timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parent_policy_snapshot_excludes_new_splint_until_reload() {
    time::timeout(Duration::from_secs(60), async {
        let daemon = Daemon::start_with_policy(&exact_headless_policy(None)).await;
        let mut bootstrap = daemon.connect().await;
        let revision = bootstrap.topology_revision().await;
        let Response::LairCreated { lair: dojo, .. } = bootstrap
            .request(Request::CreateLair {
                expected_topology_revision: revision,
                name: "snapshot-policy".into(),
                launch: LaunchParameters {
                    cwd: std::env::current_dir().unwrap(),
                    command: vec!["/bin/sh".into(), "-c".into(), "exec sleep 30".into()],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 100,
                },
                generated_name: false,
            })
            .await
        else {
            panic!("snapshot policy Lair was not created");
        };
        let lair_id = dojo.id;
        let LayoutNode::Leaf(original) = &dojo.dojos[0].root else {
            panic!("snapshot policy Lair was not a leaf");
        };
        let original_id = original.id;

        let policy = daemon.policy.as_ref().unwrap();
        fs::write(policy, parent_snapshot_policy(lair_id)).unwrap();
        fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.reload_policy();
        assert_connection_closed(&mut bootstrap, "parent snapshot policy reload").await;

        let mut connection = daemon.connect().await;
        let revision = connection.topology_revision().await;
        let Response::SplintStarted {
            splint_id,
            incarnation,
            ..
        } = connection
            .request(Request::SplitSplint {
                expected_topology_revision: revision,
                target_splint_id: original_id,
                axis: Axis::Horizontal,
                side: SplitSide::Second,
                ratio: SplitRatio::new(500).unwrap(),
                launch: LaunchParameters {
                    cwd: std::env::current_dir().unwrap(),
                    command: vec!["/bin/sh".into(), "-c".into(), "exec sleep 30".into()],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 100,
                },
            })
            .await
        else {
            panic!("snapshot policy split was not created");
        };
        let denied = connection
            .request_result(Request::Attach {
                splint_id,
                incarnation: Some(incarnation),
                scrollback_rows: 1,
            })
            .await
            .expect_err("new descendant must not inherit the published parent snapshot");
        assert_eq!(denied.code, ErrorCode::Unauthorized);

        fs::write(policy, parent_snapshot_policy(lair_id)).unwrap();
        fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.reload_policy();
        assert_connection_closed(&mut connection, "explicit descendant policy reload").await;

        let mut refreshed = daemon.connect().await;
        let (subscription_id, _) = refreshed.attach(splint_id, incarnation).await;
        assert_eq!(
            refreshed.request(Request::Detach { subscription_id }).await,
            Response::Acknowledged
        );
        daemon.shutdown();
    })
    .await
    .expect("parent policy snapshot integration timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sigterm_shutdown_removes_daemon_socket() {
    let daemon = Daemon::start().await;
    let socket = daemon.socket.clone();

    tokio::task::spawn_blocking(move || {
        daemon.shutdown_with_signal(rustix::process::Signal::TERM);
    })
    .await
    .unwrap();
    assert!(!socket.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_reaps_signal_resistant_child_and_removes_socket() {
    time::timeout(Duration::from_secs(80), async {
        let daemon = Daemon::start_with_policy(&exact_headless_policy(None)).await;
        let marker = daemon.runtime.join("resistant-child-pid");
        let mut connection = daemon.connect().await;
        let revision = connection.topology_revision().await;
        let Response::LairCreated { .. } = connection
            .request(Request::CreateLair {
                expected_topology_revision: revision,
                name: "resistant-shutdown".into(),
                launch: LaunchParameters {
                    cwd: std::env::current_dir().unwrap(),
                    command: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        format!(
                            "trap '' HUP TERM; printf '%s\\n' $$ > {}; exec sleep 75",
                            marker.display()
                        ),
                    ],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 100,
                },
                generated_name: false,
            })
            .await
        else {
            panic!("signal-resistant Lair was not created");
        };
        let child_pid = wait_for_pid_marker(&marker, "signal-resistant child").await;
        drop(connection);

        tokio::task::spawn_blocking(move || daemon.shutdown())
            .await
            .unwrap();
        assert!(!Path::new(&format!("/proc/{child_pid}")).exists());
    })
    .await
    .expect("signal-resistant shutdown integration timed out");
}

/// The pids an interactive shell's rc file recorded for one Splint, one per
/// launch, in launch order.
fn recorded_shell_pids(path: &Path, splint_id: SplintId) -> Vec<u32> {
    let splint = splint_id.to_string();
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (recorded, pid) = line.split_once(' ')?;
            (recorded == splint).then(|| pid.trim().parse().ok())?
        })
        .collect()
}

async fn wait_for_shell_pid(path: &Path, splint_id: SplintId, launch: usize) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(pid) = recorded_shell_pids(path, splint_id).get(launch) {
            return *pid;
        }
        assert!(
            Instant::now() < deadline,
            "launch {launch} of {splint_id} never ran its rc file"
        );
        time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_exited_lifecycle(connection: &mut Connection, splint_id: SplintId) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match connection
            .request(Request::InspectSplint { splint_id })
            .await
        {
            Response::Splint { runtime, .. } if runtime.lifecycle == SplintLifecycle::Exited => {
                return;
            }
            Response::Splint { .. } => {}
            response => panic!("unexpected Splint inspection response: {response:?}"),
        }
        assert!(
            Instant::now() < deadline,
            "{splint_id} never reported its exit"
        );
        time::sleep(Duration::from_millis(20)).await;
    }
}

async fn type_into(daemon: &Daemon, splint_id: SplintId, bytes: &[u8]) {
    let mut connection = daemon.connect().await;
    let incarnation = connection.live_incarnation(splint_id).await;
    connection.input(splint_id, incarnation, bytes).await;
}

/// Kills a process the test started when the test ends, passing or not.
struct KillOnDrop(u32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(pid) = i32::try_from(self.0)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario replays the Explorer restore, external kill, and SIGTERM sequence"
)]
async fn sigterm_shutdown_ends_a_restored_splint_whose_job_outlives_its_shell() {
    // Production's 30-second hangup and termination graces: a shutdown that
    // waits for escalation cannot fit the bound below.
    const SHUTDOWN_BOUND: Duration = Duration::from_secs(10);
    time::timeout(TEST_TIMEOUT, async {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let runtime =
            std::env::temp_dir().join(format!("sprestore-{}-{nonce:x}", std::process::id()));
        let home = runtime.join("home");
        fs::create_dir_all(&home).unwrap();
        let pids = runtime.join("shell-pids");
        fs::write(
            home.join(".bashrc"),
            format!(
                "printf '%s %s\\n' \"$SPLINTERM_SPLINT_ID\" \"$$\" >> {}\nPS1='$ '\n",
                pids.display()
            ),
        )
        .unwrap();
        let socket = runtime.join("splinterd.sock");
        // An unparsable override leaves the production graces in place.
        let child = Daemon::spawn_configured_with_environment(
            &runtime,
            &socket,
            None,
            true,
            &[
                ("HOME", home.to_str().unwrap()),
                ("SPLINTERM_TEST_SHUTDOWN_GRACE_MS", ""),
            ],
        );
        Daemon::wait_until_ready(&socket).await;
        let mut daemon = Daemon {
            child,
            runtime,
            socket,
            policy: None,
            development_terminal_access: true,
        };
        let launch = LaunchParameters {
            cwd: home.clone(),
            command: Vec::new(),
            shell: Some("/bin/bash".into()),
            login_shell: false,
            scrollback_lines: 100,
        };
        let mut connection = daemon.connect().await;
        let Response::LairCreated { lair, .. } = connection
            .request(Request::CreateLair {
                expected_topology_revision: TopologyRevision::default(),
                name: "restore-shutdown".into(),
                launch: launch.clone(),
                generated_name: false,
            })
            .await
        else {
            panic!("Lair was not created");
        };
        let dojo_id = lair.dojos[0].id;
        let LayoutNode::Leaf(restored) = &lair.dojos[0].root else {
            panic!("created Dojo was not a leaf");
        };
        let restored_id = restored.id;
        let expected_topology_revision = connection.topology_revision().await;
        let Response::SplintStarted {
            splint_id: killed_id,
            ..
        } = connection
            .request(Request::SplitSplint {
                expected_topology_revision,
                target_splint_id: restored_id,
                axis: Axis::Horizontal,
                side: SplitSide::Second,
                ratio: SplitRatio::new(500).unwrap(),
                launch,
            })
            .await
        else {
            panic!("second Splint was not created");
        };
        wait_for_shell_pid(&pids, restored_id, 0).await;
        wait_for_shell_pid(&pids, killed_id, 0).await;

        // The shell exits, and the Explorer restores the Dojo's exited Splint.
        type_into(&daemon, restored_id, b"exit\n").await;
        wait_for_exited_lifecycle(&mut connection, restored_id).await;
        let expected_topology_revision = connection.topology_revision().await;
        let Response::RestoreCompleted { results, .. } = connection
            .request(Request::RestoreDojo {
                expected_topology_revision,
                dojo_id,
            })
            .await
        else {
            panic!("Dojo restore failed");
        };
        assert!(
            results
                .iter()
                .any(|result| result.splint_id == restored_id && result.incarnation.is_some()),
            "{results:?}"
        );
        let restored_shell = wait_for_shell_pid(&pids, restored_id, 1).await;
        // The restored shell starts a job in its own process group that
        // ignores hangup and keeps the PTY open.
        let holder = daemon.runtime.join("holder-pid");
        type_into(
            &daemon,
            restored_id,
            format!(
                "(trap '' HUP; echo $BASHPID > {}; exec sleep 120) &\n",
                holder.display()
            )
            .as_bytes(),
        )
        .await;
        let holder_pid = time::timeout(Duration::from_secs(5), wait_for_pid_file(&holder))
            .await
            .expect("the restored shell never started its job");
        let _holder = KillOnDrop(holder_pid);
        // Another Splint is killed externally.
        let incarnation = connection.live_incarnation(killed_id).await;
        assert!(matches!(
            connection
                .request(Request::KillSplint {
                    splint_id: killed_id,
                    incarnation,
                })
                .await,
            Response::SplintKilled { .. }
        ));
        drop(connection);

        let started = Instant::now();
        let pid =
            rustix::process::Pid::from_raw(i32::try_from(daemon.child.id()).unwrap()).unwrap();
        rustix::process::kill_process(pid, rustix::process::Signal::TERM).unwrap();
        let status = loop {
            if let Some(status) = daemon.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                started.elapsed() < SHUTDOWN_BOUND,
                "SIGTERM shutdown is still waiting on the restored Splint after {:?}",
                started.elapsed()
            );
            time::sleep(Duration::from_millis(10)).await;
        };
        daemon.assert_success(status);
        assert!(!daemon.socket.exists());
        assert!(
            !Path::new(&format!("/proc/{restored_shell}")).exists(),
            "the hangup ended the restored shell"
        );
    })
    .await
    .expect("restored Splint shutdown scenario timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one lifecycle scenario compares existing and post-theme-change PTY environments"
)]
async fn new_splint_refreshes_the_active_omarchy_gum_environment() {
    time::timeout(TEST_TIMEOUT, async {
        let old_palette = r##"
hl.env("FOREGROUND", "#f4f4f4")
hl.env("BACKGROUND", "#1b1b1b")
hl.env("BORDER_FOREGROUND", "#808080")
hl.env("BORDER_BACKGROUND", "#1b1b1b")
hl.env("GUM_CHOOSE_SELECTED_BACKGROUND", "#808080")
hl.env("GUM_OLD_THEME_ONLY", "#f4f4f4")
"##;
        let current_palette = r##"
hl.env("FOREGROUND", "#afaaa2")
hl.env("BACKGROUND", "#0c1928")
hl.env("BORDER_FOREGROUND", "#d4bda2")
hl.env("BORDER_BACKGROUND", "#0c1928")
hl.env("GUM_CHOOSE_SELECTED_BACKGROUND", "#d4bda2")
"##;
        let daemon = Daemon::start_with_gum_environment(old_palette).await;
        let existing_ready = daemon.runtime.join("existing-gum-ready");
        let existing_trigger = daemon.runtime.join("existing-gum-trigger");
        let existing_marker = daemon.runtime.join("existing-gum-environment");
        let refreshed_marker = daemon.runtime.join("refreshed-gum-environment");
        let malformed_marker = daemon.runtime.join("malformed-gum-environment");
        let mut connection = daemon.connect().await;
        let Response::LairCreated { .. } = connection
            .request(Request::CreateLair {
                expected_topology_revision: TopologyRevision::default(),
                name: "old-gum-environment".into(),
                launch: LaunchParameters {
                    cwd: std::env::current_dir().unwrap(),
                    command: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        format!(
                            "touch {}; while [ ! -e {} ]; do sleep 0.01; done; printf '%s|%s|%s' \"$BACKGROUND\" \"$GUM_CHOOSE_SELECTED_BACKGROUND\" \"${{GUM_OLD_THEME_ONLY-unset}}\" > {}",
                            existing_ready.display(),
                            existing_trigger.display(),
                            existing_marker.display()
                        ),
                    ],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 100,
                },
                generated_name: false,
            })
            .await
        else {
            panic!("existing Gum environment test Lair was not created");
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !existing_ready.exists() {
            assert!(
                Instant::now() < deadline,
                "existing Gum environment did not become ready"
            );
            time::sleep(Duration::from_millis(10)).await;
        }

        let theme = daemon.runtime.join("state/omarchy/current/theme");
        let replacement = theme.join("gum_env.lua.next");
        fs::write(&replacement, current_palette).unwrap();
        fs::rename(replacement, theme.join("gum_env.lua")).unwrap();
        let revision = connection.topology_revision().await;
        let Response::LairCreated { .. } = connection
            .request(Request::CreateLair {
                expected_topology_revision: revision,
                name: "current-gum-environment".into(),
                launch: LaunchParameters {
                    cwd: std::env::current_dir().unwrap(),
                    command: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        format!(
                            "printf '%s|%s|%s' \"$BACKGROUND\" \"$GUM_CHOOSE_SELECTED_BACKGROUND\" \"${{GUM_OLD_THEME_ONLY-unset}}\" > {}",
                            refreshed_marker.display()
                        ),
                    ],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 100,
                },
                generated_name: false,
            })
            .await
        else {
            panic!("refreshed Gum environment test Lair was not created");
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !refreshed_marker.exists() {
            assert!(
                Instant::now() < deadline,
                "refreshed Gum environment marker timed out"
            );
            time::sleep(Duration::from_millis(10)).await;
        }

        let malformed = format!(
            "{current_palette}\nhl.env(\"GUM_FILTER_INDICATOR\", \"#ffffff\""
        );
        let replacement = theme.join("gum_env.lua.next");
        fs::write(&replacement, malformed).unwrap();
        fs::rename(replacement, theme.join("gum_env.lua")).unwrap();
        let revision = connection.topology_revision().await;
        let Response::LairCreated { .. } = connection
            .request(Request::CreateLair {
                expected_topology_revision: revision,
                name: "malformed-gum-environment".into(),
                launch: LaunchParameters {
                    cwd: std::env::current_dir().unwrap(),
                    command: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        format!(
                            "printf '%s|%s|%s' \"$BACKGROUND\" \"$GUM_CHOOSE_SELECTED_BACKGROUND\" \"${{GUM_OLD_THEME_ONLY-unset}}\" > {}",
                            malformed_marker.display()
                        ),
                    ],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 100,
                },
                generated_name: false,
            })
            .await
        else {
            panic!("malformed Gum environment test Lair was not created");
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !malformed_marker.exists() {
            assert!(
                Instant::now() < deadline,
                "malformed Gum environment marker timed out"
            );
            time::sleep(Duration::from_millis(10)).await;
        }

        fs::write(existing_trigger, b"ready").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !existing_marker.exists() {
            assert!(
                Instant::now() < deadline,
                "existing Gum environment marker timed out"
            );
            time::sleep(Duration::from_millis(10)).await;
        }

        assert_eq!(
            fs::read_to_string(refreshed_marker).unwrap(),
            "#0c1928|#d4bda2|unset"
        );
        assert_eq!(
            fs::read_to_string(existing_marker).unwrap(),
            "#1b1b1b|#808080|#f4f4f4"
        );
        assert_eq!(
            fs::read_to_string(malformed_marker).unwrap(),
            "#1b1b1b|#808080|#f4f4f4"
        );
        daemon.shutdown();
    })
    .await
    .expect("Gum environment integration timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one lifecycle scenario correlates create, restore, and new-Dojo context"
)]
async fn new_dojo_and_restore_inject_exact_current_context() {
    time::timeout(TEST_TIMEOUT, async {
        let daemon = Daemon::start().await;
        let restored_marker = daemon.runtime.join("restored-context");
        let dojo_marker = daemon.runtime.join("window-context");
        let context_launch = |marker: &Path| LaunchParameters {
            cwd: std::env::current_dir().unwrap(),
            command: vec![
                "/bin/sh".into(),
                "-c".into(),
                format!(
                    "printf '%s|%s|%s|%s' \"$SPLINTERM_LAIR_ID\" \"$SPLINTERM_DOJO_ID\" \"$SPLINTERM_SPLINT_ID\" \"$SPLINTERM_SPLINT_INCARNATION\" > {}",
                    marker.display()
                ),
            ],
            shell: None,
            login_shell: false,
            scrollback_lines: 100,
        };
        let mut connection = daemon.connect().await;
        let Response::LairCreated {
            lair: dojo,
            incarnation: first_incarnation,
            ..
        } = connection
            .request(Request::CreateLair {
                expected_topology_revision: TopologyRevision::default(),
                name: "context-lifecycle".into(),
                launch: context_launch(&restored_marker),
                generated_name: false,
            })
            .await
        else {
            panic!("context test Lair was not created");
        };
        let lair_id = dojo.id;
        let first_dojo_id = dojo.dojos[0].id;
        let LayoutNode::Leaf(first) = &dojo.dojos[0].root else {
            panic!("context test Lair was not a leaf");
        };
        let first_id = first.id;
        let expected_first =
            format!("{lair_id}|{first_dojo_id}|{first_id}|{first_incarnation}");
        let marker_deadline = Instant::now() + Duration::from_secs(5);
        while !matches!(fs::read_to_string(&restored_marker), Ok(ref value) if value == &expected_first) {
            assert!(Instant::now() < marker_deadline, "initial context marker timed out");
            time::sleep(Duration::from_millis(10)).await;
        }
        loop {
            let Response::Splint { runtime, .. } = connection
                .request(Request::InspectSplint { splint_id: first_id })
                .await
            else {
                panic!("context test Splint was not inspected");
            };
            if matches!(runtime.lifecycle, SplintLifecycle::Exited) {
                break;
            }
            time::sleep(Duration::from_millis(10)).await;
        }

        let revision = connection.topology_revision().await;
        let Response::RestoreCompleted { results, .. } = connection
            .request(Request::RestoreSplint {
                expected_topology_revision: revision,
                splint_id: first_id,
            })
            .await
        else {
            panic!("context test Splint was not restored");
        };
        let restored_incarnation = results[0].incarnation.expect("restore must start process");
        assert_ne!(restored_incarnation, first_incarnation);
        let expected_restored =
            format!("{lair_id}|{first_dojo_id}|{first_id}|{restored_incarnation}");
        let marker_deadline = Instant::now() + Duration::from_secs(5);
        while !matches!(fs::read_to_string(&restored_marker), Ok(ref value) if value == &expected_restored) {
            assert!(Instant::now() < marker_deadline, "restored context marker timed out");
            time::sleep(Duration::from_millis(10)).await;
        }

        let revision = connection.topology_revision().await;
        let Response::DojoStarted {
            dojo_id,
            splint_id,
            incarnation,
            ..
        } = connection
            .request(Request::NewDojo {
                expected_topology_revision: revision,
                lair_id,
                name: "context-dojo".into(),
                launch: context_launch(&dojo_marker),
                promote_transient_lair: false,
                generated_name: false,
            })
            .await
        else {
            panic!("context test Dojo was not created");
        };
        let expected_window = format!("{lair_id}|{dojo_id}|{splint_id}|{incarnation}");
        let marker_deadline = Instant::now() + Duration::from_secs(5);
        while !matches!(fs::read_to_string(&dojo_marker), Ok(ref value) if value == &expected_window) {
            assert!(Instant::now() < marker_deadline, "Dojo context marker timed out");
            time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!expected_restored.contains("caller-supplied"));
        assert!(!expected_window.contains("caller-supplied"));
        daemon.shutdown();
    })
    .await
    .expect("context lifecycle scenario timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_restores_ids_without_running_saved_commands() {
    time::timeout(TEST_TIMEOUT, async {
        let mut daemon = Daemon::start().await;
        let marker = daemon.runtime.join("launch-count");
        let launch = LaunchParameters {
            cwd: std::env::current_dir().unwrap(),
            command: vec![
                "/bin/sh".into(),
                "-c".into(),
                format!("printf 'run\\n' >> {}; exec sleep 30", marker.display()),
            ],
            shell: None,
            login_shell: false,
            scrollback_lines: 100,
        };
        let mut connection = daemon.connect().await;
        let Response::LairCreated { lair: dojo, .. } = connection
            .request(Request::CreateLair {
                expected_topology_revision: splinterm_core::TopologyRevision::default(),
                name: "durable".into(),
                launch,
                generated_name: false,
            })
            .await
        else {
            panic!("dojo was not created");
        };
        let lair_id = dojo.id;
        let dojo_id = dojo.dojos[0].id;
        let LayoutNode::Leaf(splint) = &dojo.dojos[0].root else {
            panic!("created dojo was not a leaf");
        };
        let splint_id = splint.id;
        let marker_deadline = Instant::now() + Duration::from_secs(5);
        while !matches!(fs::read_to_string(&marker), Ok(contents) if contents == "run\n") {
            assert!(
                Instant::now() < marker_deadline,
                "launch marker did not contain expected output"
            );
            time::sleep(Duration::from_millis(10)).await;
        }
        drop(connection);

        daemon.stop_preserving_state();
        let primary = daemon.runtime.join("state/splinterm/topology.json");
        fs::write(&primary, b"{truncated").unwrap();
        daemon.start_again().await;
        time::sleep(Duration::from_millis(100)).await;
        assert!(
            fs::read_dir(primary.parent().unwrap())
                .unwrap()
                .any(|entry| entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("topology.invalid-"))
        );
        assert_eq!(fs::read_to_string(&marker).unwrap(), "run\n");
        let mut restored = daemon.connect().await;
        let Response::Topology { snapshot } = restored.request(Request::InspectTopology).await
        else {
            panic!("restored topology was not returned");
        };
        snapshot.validate().unwrap();
        assert_eq!(snapshot.revision.get(), 1);
        let restored_dojo = snapshot.topology.lairs().next().unwrap();
        assert_eq!(restored_dojo.id, lair_id);
        assert_eq!(restored_dojo.dojos[0].id, dojo_id);
        let LayoutNode::Leaf(restored_splint) = &restored_dojo.dojos[0].root else {
            panic!("restored dojo was not a leaf");
        };
        assert_eq!(restored_splint.id, splint_id);
        assert!(matches!(
            restored_splint.state,
            splinterm_core::SplintState::Exited(_)
        ));
        assert_eq!(snapshot.runtimes[0].live_incarnation, None);

        let expected_topology_revision = snapshot.revision;
        let Response::RestoreCompleted { results, .. } = restored
            .request(Request::RestoreSplint {
                expected_topology_revision,
                splint_id,
            })
            .await
        else {
            panic!("restore result was not returned");
        };
        assert_eq!(results.len(), 1);
        assert!(results[0].incarnation.is_some());
        assert!(results[0].error.is_none());
        while fs::read_to_string(&marker).unwrap() != "run\nrun\n" {
            time::sleep(Duration::from_millis(10)).await;
        }
        daemon.shutdown();
    })
    .await
    .expect("restart persistence scenario timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario covers restore-one, restore-Dojo, restore-Lair, and partial results"
)]
async fn explicit_restore_scopes_report_per_leaf_results() {
    time::timeout(TEST_TIMEOUT, async {
        let daemon = Daemon::start().await;
        let launch = LaunchParameters {
            cwd: std::env::current_dir().unwrap(),
            command: vec!["/bin/sh".into(), "-c".into(), "exec sleep 30".into()],
            shell: None,
            login_shell: false,
            scrollback_lines: 100,
        };
        let mut connection = daemon.connect().await;
        let Response::LairCreated { lair: dojo, .. } = connection
            .request(Request::CreateLair {
                expected_topology_revision: splinterm_core::TopologyRevision::default(),
                name: "restore-scopes".into(),
                launch: launch.clone(),
                generated_name: false,
            })
            .await
        else {
            panic!("dojo was not created");
        };
        let lair_id = dojo.id;
        let dojo_id = dojo.dojos[0].id;
        let LayoutNode::Leaf(first) = &dojo.dojos[0].root else {
            panic!("created dojo was not a leaf");
        };
        let first_id = first.id;
        let first_incarnation = connection.live_incarnation(first_id).await;
        let Response::SplintStarted {
            splint_id: second_id,
            incarnation: second_incarnation,
            topology_revision,
        } = connection
            .request(Request::SplitSplint {
                expected_topology_revision: splinterm_core::TopologyRevision::new(1),
                target_splint_id: first_id,
                axis: Axis::Horizontal,
                side: SplitSide::Second,
                ratio: SplitRatio::new(500).unwrap(),
                launch: launch.clone(),
            })
            .await
        else {
            panic!("second Splint was not created");
        };
        let Response::DojoStarted {
            splint_id: third_id,
            incarnation: third_incarnation,
            ..
        } = connection
            .request(Request::NewDojo {
                expected_topology_revision: topology_revision,
                lair_id,
                name: "second".into(),
                launch,
                promote_transient_lair: false,
                generated_name: false,
            })
            .await
        else {
            panic!("second Dojo was not created");
        };
        for (splint_id, incarnation) in [
            (first_id, first_incarnation),
            (second_id, second_incarnation),
            (third_id, third_incarnation),
        ] {
            assert!(matches!(
                connection
                    .request(Request::KillSplint {
                        splint_id,
                        incarnation,
                    })
                    .await,
                Response::SplintKilled { .. }
            ));
        }

        let expected_topology_revision = connection.topology_revision().await;
        let Response::RestoreCompleted { results, .. } = connection
            .request(Request::RestoreSplint {
                expected_topology_revision,
                splint_id: first_id,
            })
            .await
        else {
            panic!("single restore failed");
        };
        assert_eq!(results.len(), 1);
        assert!(results[0].incarnation.is_some());

        let expected_topology_revision = connection.topology_revision().await;
        let Response::RestoreCompleted { results, .. } = connection
            .request(Request::RestoreDojo {
                expected_topology_revision,
                dojo_id,
            })
            .await
        else {
            panic!("Dojo restore failed");
        };
        assert_eq!(results.len(), 2);
        assert_eq!(
            results
                .iter()
                .filter(|result| result.error.is_some())
                .count(),
            1
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| result.incarnation.is_some())
                .count(),
            1
        );

        let expected_topology_revision = connection.topology_revision().await;
        let Response::RestoreCompleted { results, .. } = connection
            .request(Request::RestoreLair {
                expected_topology_revision,
                lair_id,
            })
            .await
        else {
            panic!("dojo restore failed");
        };
        assert_eq!(results.len(), 3);
        assert_eq!(
            results
                .iter()
                .filter(|result| result.error.is_some())
                .count(),
            2
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| result.incarnation.is_some())
                .count(),
            1
        );
        daemon.shutdown();
    })
    .await
    .expect("explicit restore scope scenario timed out");
}

// ---------------------------------------------------------------------------
// Plan 0037 Milestone 5: recipe-only restoration and terminal-body privacy
// ---------------------------------------------------------------------------

/// Every regular file under `root`, recursively, with its contents. Sockets,
/// symbolic links, and other special files are skipped.
fn regular_files_under(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                let bytes = fs::read(&path).unwrap();
                files.push((path, bytes));
            }
        }
    }
    files
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// No file under `root` holds any of `sentinels`.
fn assert_no_file_contains(root: &Path, sentinels: &[&str]) {
    let files = regular_files_under(root);
    assert!(
        !files.is_empty(),
        "nothing to scan under {}",
        root.display()
    );
    for (path, bytes) in files {
        for sentinel in sentinels {
            assert!(
                !contains_bytes(&bytes, sentinel.as_bytes()),
                "{} holds terminal-body sentinel {sentinel:?}",
                path.display()
            );
        }
    }
}

/// No serialized response holds any of `sentinels`.
fn assert_responses_body_free(responses: &[Response], sentinels: &[&str]) {
    for response in responses {
        let encoded = serde_json::to_string(response).unwrap();
        for sentinel in sentinels {
            assert!(
                !encoded.contains(sentinel),
                "an observation holds terminal-body sentinel {sentinel:?}: {encoded}"
            );
        }
    }
}

/// Every retained audit page, oldest first, up to the first partial page.
/// (Each `AuditInspect` is itself audited, so the log always grows by one.)
async fn audit_pages(connection: &mut Connection) -> Vec<Response> {
    const PAGE_RECORDS: usize = 64;
    let mut pages = Vec::new();
    let mut after_audit_id = None;
    loop {
        let response = connection
            .request(Request::AuditInspect {
                after_audit_id,
                max_records: PAGE_RECORDS,
            })
            .await;
        let Response::AuditPage { page } = &response else {
            panic!("audit page was not returned: {response:?}");
        };
        let next = page.next_after_audit_id;
        let partial = page.records.len() < PAGE_RECORDS;
        pages.push(response);
        if partial || next.is_none() {
            return pages;
        }
        after_audit_id = next;
    }
}

/// The metadata observations a client can make without reading terminal
/// content, and every audit page.
async fn metadata_observations(connection: &mut Connection, splint_id: SplintId) -> Vec<Response> {
    let mut observations = vec![
        connection.request(Request::ListLairs).await,
        connection.request(Request::InspectTopology).await,
        connection
            .request(Request::InspectSplint { splint_id })
            .await,
    ];
    observations.extend(audit_pages(connection).await);
    observations
}

/// The processes whose parent is `parent`, zombies included.
fn child_pids(parent: u32) -> Vec<u32> {
    fs::read_dir("/proc")
        .unwrap()
        .filter_map(|entry| {
            let pid = entry.ok()?.file_name().to_str()?.parse::<u32>().ok()?;
            let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            let parent_pid = stat[stat.rfind(')')? + 1..]
                .split_whitespace()
                .nth(1)?
                .parse::<u32>()
                .ok()?;
            (parent_pid == parent).then_some(pid)
        })
        .collect()
}

async fn wait_for_file_contents(path: &Path, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while fs::read_to_string(path).unwrap_or_default() != expected {
        assert!(
            Instant::now() < deadline,
            "{} never held {expected:?}: {:?}",
            path.display(),
            fs::read_to_string(path)
        );
        time::sleep(Duration::from_millis(10)).await;
    }
}

/// Plan 0037 M5: a unique sentinel in a live Splint's output (on screen and
/// in scrollback) and in its input never reaches the durable Topology (the
/// primary file, its previous-generation backup, or a quarantined copy),
/// the daemon's log stream at its most verbose level, its audit records, or
/// its metadata observations, before or after a restart.
///
/// The launch recipe assembles the output sentinel at run time from pieces,
/// so the durable recipe (argv) never contains it; the input sentinel only
/// ever travels as `Input` bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario follows one terminal body through every durable and observation sink"
)]
async fn terminal_body_sentinel_stays_out_of_durable_state_audit_and_observation() {
    // Every daemon target at trace and the rest at debug, so a body that
    // reached any log statement would land in the captured log stream
    // (`daemon.stdout`).
    const ENVIRONMENT: &[(&str, &str)] = &[(
        "RUST_LOG",
        "debug,splinterd=trace,splinterm_pty=trace,splinterm_terminal=trace",
    )];
    time::timeout(Duration::from_secs(60), async {
        let nonce = format!(
            "{:x}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let output_sentinel = format!("m5-output-{nonce}-sentinel");
        let input_sentinel = format!("m5-input-{nonce}-sentinel");
        let sentinels = [output_sentinel.as_str(), input_sentinel.as_str()];
        let mut daemon = Daemon::start_with_environment(ENVIRONMENT).await;
        let mut connection = daemon.connect().await;
        let launch = LaunchParameters {
            cwd: daemon.runtime.clone(),
            command: vec![
                "/bin/sh".into(),
                "-c".into(),
                format!(
                    "s=$(printf '%s-%s-%s' m5-output {nonce} sentinel); i=0; \
                     while [ \"$i\" -lt 200 ]; do printf '%s %03d\\n' \"$s\" \"$i\"; \
                     i=$((i+1)); done; exec cat"
                ),
            ],
            shell: None,
            login_shell: false,
            scrollback_lines: 1000,
        };
        let Response::LairCreated {
            lair, incarnation, ..
        } = connection
            .request(Request::CreateLair {
                expected_topology_revision: TopologyRevision::default(),
                name: "privacy".into(),
                launch,
                generated_name: false,
            })
            .await
        else {
            panic!("Lair was not created");
        };
        let lair_id = lair.id;
        let LayoutNode::Leaf(splint) = &lair.dojos[0].root else {
            panic!("created Lair was not a leaf");
        };
        let splint_id = splint.id;

        // The body is really there: on screen, in scrollback, and echoed input.
        snapshot_until(
            &mut connection,
            splint_id,
            incarnation,
            &format!("{output_sentinel} 199"),
        )
        .await;
        let (subscription_id, snapshot) = connection
            .attach_with_scrollback(splint_id, incarnation, 64)
            .await;
        assert_eq!(
            connection
                .request(Request::Detach { subscription_id })
                .await,
            Response::Acknowledged
        );
        let scrollback = snapshot
            .scrollback_rows
            .iter()
            .flat_map(|row| row.cells.iter())
            .map(|cell| cell.content.as_str())
            .collect::<String>();
        assert!(
            scrollback.contains(&output_sentinel),
            "the sentinel never reached scrollback: {scrollback:?}"
        );
        connection
            .input(
                splint_id,
                incarnation,
                format!("{input_sentinel}\n").as_bytes(),
            )
            .await;
        snapshot_until(&mut connection, splint_id, incarnation, &input_sentinel).await;
        connection.release_control().await;

        // A second durable save, so the previous generation is backed up.
        let expected_topology_revision = connection.topology_revision().await;
        assert!(matches!(
            connection
                .request(Request::RenameLair {
                    expected_topology_revision,
                    lair_id,
                    name: "privacy-renamed".into(),
                })
                .await,
            Response::TopologyCommitted { .. }
        ));

        let observations = metadata_observations(&mut connection, splint_id).await;
        assert_responses_body_free(&observations, &sentinels);
        // The body-bearing operations were audited, without their bodies.
        let audited = observations
            .iter()
            .filter_map(|response| match response {
                Response::AuditPage { page } => Some(page.records.iter()),
                _ => None,
            })
            .flatten()
            .map(|record| record.operation)
            .collect::<Vec<_>>();
        for operation in [
            splinterm_protocol::AuditOperation::CreateLair,
            splinterm_protocol::AuditOperation::Attach,
            splinterm_protocol::AuditOperation::Input,
        ] {
            assert!(
                audited.contains(&operation),
                "{operation:?} was not audited: {audited:?}"
            );
        }
        drop(connection);
        daemon.stop_preserving_state();

        // Make the primary file unsafe (group/world-readable), so the next
        // start quarantines it whole and restores from the backup.
        let state = daemon.runtime.join("state/splinterm");
        let primary = state.join("topology.json");
        let backup = state.join("topology.json.previous");
        assert!(
            backup.is_file(),
            "no previous-generation backup was written"
        );
        fs::set_permissions(&primary, fs::Permissions::from_mode(0o644)).unwrap();
        daemon.start_again_with_environment(ENVIRONMENT).await;
        let mut connection = daemon.connect().await;
        let restored = connection.topology().await;
        assert!(matches!(
            restored.find_splint(splint_id).map(|splint| &splint.state),
            Some(splinterm_core::SplintState::Exited(_))
        ));
        let observations = metadata_observations(&mut connection, splint_id).await;
        assert_responses_body_free(&observations, &sentinels);
        drop(connection);
        daemon.stop_preserving_state();

        let quarantined = fs::read_dir(&state)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("topology.invalid-")
            })
            .collect::<Vec<_>>();
        let [quarantined] = quarantined.as_slice() else {
            panic!("expected exactly one quarantined primary: {quarantined:?}");
        };
        // Each durable file holds the real Topology, just never the body.
        for durable in [&primary, &backup, quarantined] {
            let contents = fs::read_to_string(durable).unwrap();
            assert!(
                contents.contains(&splint_id.to_string()),
                "{} does not hold the saved Splint",
                durable.display()
            );
        }
        // The log stream really is verbose, and saw both generations.
        let logs = fs::read_to_string(daemon.runtime.join("daemon.stdout")).unwrap();
        assert_eq!(logs.matches("splinterd ready").count(), 2, "{logs}");
        for expected in ["DEBUG", "quarantined invalid primary metadata"] {
            assert!(logs.contains(expected), "no {expected:?} in the log stream");
        }
        assert_no_file_contains(&daemon.runtime, &sentinels);
    })
    .await
    .expect("terminal-body privacy scenario timed out");
}

/// Plan 0037 M5: startup with a saved Topology runs no saved launch recipe,
/// and neither do the requests the Dojo pickers and every restore preview
/// send. The client test
/// `explorer_context_tests::picker_and_restore_previews_send_only_reads`
/// pins that set to `ListLairs` and `InspectTopology`; a standalone picker
/// that is cancelled has already disconnected after its `ListLairs`. Only
/// the explicit restore at the end runs the recipe again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario covers startup, every preview request, and the explicit restore"
)]
async fn startup_picker_and_restore_previews_execute_no_saved_recipe() {
    time::timeout(TEST_TIMEOUT, async {
        let mut daemon = Daemon::start().await;
        let marker = daemon.runtime.join("recipe-ran");
        let launch = LaunchParameters {
            cwd: std::env::current_dir().unwrap(),
            command: vec![
                "/bin/sh".into(),
                "-c".into(),
                format!("printf 'run\\n' >> {}; exec sleep 30", marker.display()),
            ],
            shell: None,
            login_shell: false,
            scrollback_lines: 100,
        };
        let mut connection = daemon.connect().await;
        let Response::LairCreated { lair, .. } = connection
            .request(Request::CreateLair {
                expected_topology_revision: TopologyRevision::default(),
                name: "saved-recipes".into(),
                launch: launch.clone(),
                generated_name: false,
            })
            .await
        else {
            panic!("Lair was not created");
        };
        let lair_id = lair.id;
        let LayoutNode::Leaf(first) = &lair.dojos[0].root else {
            panic!("created Lair was not a leaf");
        };
        let expected_topology_revision = connection.topology_revision().await;
        let Response::SplintStarted { .. } = connection
            .request(Request::SplitSplint {
                expected_topology_revision,
                target_splint_id: first.id,
                axis: Axis::Horizontal,
                side: SplitSide::Second,
                ratio: SplitRatio::new(500).unwrap(),
                launch,
            })
            .await
        else {
            panic!("second Splint was not created");
        };
        wait_for_file_contents(&marker, "run\nrun\n").await;
        drop(connection);
        daemon.stop_preserving_state();

        daemon.start_again().await;
        // A standalone Recent Dojos picker: one `ListLairs`, then it
        // disconnects before showing anything; cancelling ends there.
        {
            let mut picker = daemon.connect().await;
            assert!(matches!(
                picker.request(Request::ListLairs).await,
                Response::Lairs { .. }
            ));
        }
        // The in-Window picker, Lair Explorer, selectors, and every restore
        // preview, each more than once.
        let mut window = daemon.connect().await;
        for _ in 0..3 {
            let Response::Topology { snapshot } = window.request(Request::InspectTopology).await
            else {
                panic!("Topology was not returned");
            };
            snapshot.validate().unwrap();
            assert_eq!(snapshot.runtimes.len(), 2);
            for runtime in &snapshot.runtimes {
                assert_eq!(runtime.live_incarnation, None);
                assert!(runtime.restorable, "{runtime:?}");
            }
            assert!(matches!(
                window.request(Request::ListLairs).await,
                Response::Lairs { .. }
            ));
        }
        time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            fs::read_to_string(&marker).unwrap(),
            "run\nrun\n",
            "startup or a preview ran a saved recipe"
        );
        assert_eq!(
            child_pids(daemon.child.id()),
            Vec::<u32>::new(),
            "startup or a preview started a process"
        );

        // The explicit restore runs each saved recipe exactly once more.
        let expected_topology_revision = window.topology_revision().await;
        let Response::RestoreCompleted { results, .. } = window
            .request(Request::RestoreLair {
                expected_topology_revision,
                lair_id,
            })
            .await
        else {
            panic!("restore result was not returned");
        };
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| result.error.is_none()));
        wait_for_file_contents(&marker, "run\nrun\nrun\nrun\n").await;
        drop(window);
        daemon.shutdown();
    })
    .await
    .expect("startup and preview scenario timed out");
}

/// Plan 0037 M5: after a daemon restart, every explicit restore allocates
/// an incarnation above every incarnation the saved Topology records, even
/// for the Splint whose own saved incarnation is the lowest.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario raises, persists, restarts, and restores incarnations"
)]
async fn explicit_restore_after_restart_allocates_incarnations_above_every_saved_one() {
    time::timeout(TEST_TIMEOUT, async {
        let mut daemon = Daemon::start().await;
        let launch = LaunchParameters {
            cwd: std::env::current_dir().unwrap(),
            command: vec!["/bin/sh".into(), "-c".into(), "exec sleep 30".into()],
            shell: None,
            login_shell: false,
            scrollback_lines: 100,
        };
        let mut connection = daemon.connect().await;
        let Response::LairCreated {
            lair,
            incarnation: first_incarnation,
            ..
        } = connection
            .request(Request::CreateLair {
                expected_topology_revision: TopologyRevision::default(),
                name: "incarnations".into(),
                launch: launch.clone(),
                generated_name: false,
            })
            .await
        else {
            panic!("Lair was not created");
        };
        let LayoutNode::Leaf(first) = &lair.dojos[0].root else {
            panic!("created Lair was not a leaf");
        };
        let first_id = first.id;
        let expected_topology_revision = connection.topology_revision().await;
        let Response::SplintStarted {
            splint_id: second_id,
            incarnation: mut second_incarnation,
            ..
        } = connection
            .request(Request::SplitSplint {
                expected_topology_revision,
                target_splint_id: first_id,
                axis: Axis::Horizontal,
                side: SplitSide::Second,
                ratio: SplitRatio::new(500).unwrap(),
                launch,
            })
            .await
        else {
            panic!("second Splint was not created");
        };
        // Raise the second Splint's incarnation well above the first's.
        for _ in 0..4 {
            assert!(matches!(
                connection
                    .request(Request::KillSplint {
                        splint_id: second_id,
                        incarnation: second_incarnation,
                    })
                    .await,
                Response::SplintKilled { .. }
            ));
            let expected_topology_revision = connection.topology_revision().await;
            let Response::RestoreCompleted { results, .. } = connection
                .request(Request::RestoreSplint {
                    expected_topology_revision,
                    splint_id: second_id,
                })
                .await
            else {
                panic!("restore result was not returned");
            };
            let restored = results[0].incarnation.expect("restore allocated nothing");
            assert!(restored > second_incarnation);
            second_incarnation = restored;
        }
        drop(connection);
        daemon.stop_preserving_state();

        // Exactly what the daemon persisted.
        let saved = splinterm_core::TopologyDocument::decode(
            &fs::read(daemon.runtime.join("state/splinterm/topology.json")).unwrap(),
        )
        .unwrap()
        .into_topology()
        .unwrap();
        let saved_incarnation = |splint_id| {
            saved
                .find_splint(splint_id)
                .and_then(|splint| splint.last_incarnation)
                .expect("saved Splint records its last incarnation")
        };
        assert_eq!(saved_incarnation(first_id), first_incarnation);
        assert_eq!(saved_incarnation(second_id), second_incarnation);
        assert!(first_incarnation < second_incarnation);
        let mut highest = second_incarnation;

        daemon.start_again().await;
        let mut connection = daemon.connect().await;
        // The first Splint first: a fresh allocator would reissue its own
        // saved incarnation, or another saved one.
        for splint_id in [first_id, second_id, first_id] {
            let live = match connection
                .request(Request::InspectSplint { splint_id })
                .await
            {
                Response::Splint { runtime, .. } => runtime.live_incarnation,
                response => panic!("unexpected Splint response: {response:?}"),
            };
            if let Some(incarnation) = live {
                assert!(matches!(
                    connection
                        .request(Request::KillSplint {
                            splint_id,
                            incarnation,
                        })
                        .await,
                    Response::SplintKilled { .. }
                ));
            }
            let expected_topology_revision = connection.topology_revision().await;
            let Response::RestoreCompleted { results, .. } = connection
                .request(Request::RestoreSplint {
                    expected_topology_revision,
                    splint_id,
                })
                .await
            else {
                panic!("restore result was not returned");
            };
            assert_eq!(results.len(), 1);
            assert!(results[0].error.is_none(), "{results:?}");
            let restored = results[0].incarnation.expect("restore allocated nothing");
            assert!(
                restored > highest,
                "restore of {splint_id} reissued incarnation {restored} (highest {highest})"
            );
            highest = restored;
        }
        drop(connection);
        daemon.shutdown();
    })
    .await
    .expect("incarnation restart scenario timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario proves topology CAS, ordered events, and all headless edit types"
)]
async fn topology_cas_stream_and_complete_edits() {
    time::timeout(TEST_TIMEOUT, async {
        let daemon = Daemon::start().await;
        let cwd = std::env::current_dir().unwrap();
        let launch = LaunchParameters {
            cwd: cwd.clone(),
            command: vec!["/bin/sh".into(), "-c".into(), "exec sleep 30".into()],
            shell: None,
            login_shell: false,
            scrollback_lines: 100,
        };
        let mut subscriber = daemon.connect().await;
        let Response::TopologySubscribed {
            subscription_id,
            snapshot,
        } = subscriber.request(Request::SubscribeTopology).await
        else {
            panic!("topology subscription was not created");
        };
        assert_eq!(snapshot.revision.get(), 0);

        let mut first = daemon.connect().await;
        let mut second = daemon.connect().await;
        let first_request = Request::CreateLair {
            expected_topology_revision: snapshot.revision,
            name: "race-first".into(),
            launch: launch.clone(),
            generated_name: false,
        };
        let second_request = Request::CreateLair {
            expected_topology_revision: snapshot.revision,
            name: "race-second".into(),
            launch: launch.clone(),
            generated_name: false,
        };
        let (first_result, second_result) = tokio::join!(
            first.request_result(first_request),
            second.request_result(second_request)
        );
        let (dojo, stale) = match (first_result, second_result) {
            (Ok(Response::LairCreated { lair: dojo, .. }), Err(stale))
            | (Err(stale), Ok(Response::LairCreated { lair: dojo, .. })) => (dojo, stale),
            results => panic!("exactly one racing create must commit: {results:?}"),
        };
        assert_eq!(stale.code, ErrorCode::StaleTopology);
        assert_eq!(
            stale.current_topology_revision,
            Some(splinterm_core::TopologyRevision::new(1))
        );
        let (sequence, event) = subscriber.next_event(subscription_id).await;
        assert_eq!(sequence, 1);
        let SubscriptionEvent::TopologyChanged { change } = &event else {
            panic!("first topology event was not a change");
        };
        change.validate().unwrap();
        assert!(matches!(
            event,
            SubscriptionEvent::TopologyChanged { change }
                if change.revision.get() == 1 && change.kind == TopologyChangeKind::LairCreated
        ));

        let lair_id = dojo.id;
        let dojo_id = dojo.dojos[0].id;
        let LayoutNode::Leaf(initial) = &dojo.dojos[0].root else {
            panic!("created dojo was not a leaf");
        };
        let initial_id = initial.id;
        let mut editor = daemon.connect().await;
        let Response::SplintStarted {
            splint_id: sibling_id,
            incarnation: sibling_incarnation,
            topology_revision,
        } = editor
            .request(Request::SplitSplint {
                expected_topology_revision: splinterm_core::TopologyRevision::new(1),
                target_splint_id: initial_id,
                axis: Axis::Horizontal,
                side: SplitSide::Second,
                ratio: SplitRatio::new(500).unwrap(),
                launch: launch.clone(),
            })
            .await
        else {
            panic!("split did not commit");
        };
        assert_eq!(topology_revision.get(), 2);
        assert!(matches!(
            editor
                .request(Request::SetSplitRatio {
                    expected_topology_revision: topology_revision,
                    target_splint_id: sibling_id,
                    ancestor: 0,
                    ratio: SplitRatio::new(650).unwrap(),
                })
                .await,
            Response::TopologyCommitted { topology_revision } if topology_revision.get() == 3
        ));
        assert!(matches!(
            editor
                .request(Request::RenameLair {
                    expected_topology_revision: splinterm_core::TopologyRevision::new(3),
                    lair_id,
                    name: "renamed-dojo".into(),
                })
                .await,
            Response::TopologyCommitted { topology_revision } if topology_revision.get() == 4
        ));
        assert!(matches!(
            editor
                .request(Request::RenameDojo {
                    expected_topology_revision: splinterm_core::TopologyRevision::new(4),
                    dojo_id,
                    name: "main-dojo".into(),
                    promote_transient_lair: false,
                })
                .await,
            Response::TopologyCommitted { topology_revision } if topology_revision.get() == 5
        ));
        assert!(matches!(
            editor
                .request(Request::RenameSplint {
                    expected_topology_revision: splinterm_core::TopologyRevision::new(5),
                    splint_id: initial_id,
                    title: "primary".into(),
                })
                .await,
            Response::TopologyCommitted { topology_revision } if topology_revision.get() == 6
        ));
        let Response::DojoStarted {
            dojo_id: extra_window,
            splint_id: extra_splint,
            incarnation: extra_incarnation,
            topology_revision,
        } = editor
            .request(Request::NewDojo {
                expected_topology_revision: splinterm_core::TopologyRevision::new(6),
                lair_id,
                name: "extra-dojo".into(),
                launch,
                promote_transient_lair: false,
                generated_name: false,
            })
            .await
        else {
            panic!("new Dojo did not commit");
        };
        assert_eq!(topology_revision.get(), 7);
        let invalid_focus = editor
            .request_result(Request::SetDojoDefaultFocus {
                expected_topology_revision: topology_revision,
                dojo_id,
                splint_id: extra_splint,
            })
            .await
            .unwrap_err();
        assert_eq!(invalid_focus.code, ErrorCode::InvalidArgument);
        let Response::TopologyCommitted { topology_revision } = editor
            .request(Request::SetDojoDefaultFocus {
                expected_topology_revision: topology_revision,
                dojo_id,
                splint_id: sibling_id,
            })
            .await
        else {
            panic!("Dojo focus hint did not commit");
        };
        assert_eq!(topology_revision.get(), 8);
        assert!(matches!(
            editor
                .request(Request::KillSplint {
                    splint_id: extra_splint,
                    incarnation: extra_incarnation,
                })
                .await,
            Response::SplintKilled { .. }
        ));
        assert!(matches!(
            editor
                .request(Request::CloseDojo {
                    expected_topology_revision: topology_revision,
                    dojo_id: extra_window,
                })
                .await,
            Response::TopologyCommitted { topology_revision } if topology_revision.get() == 9
        ));

        assert!(matches!(
            editor
                .request(Request::KillSplint {
                    splint_id: sibling_id,
                    incarnation: sibling_incarnation,
                })
                .await,
            Response::SplintKilled { .. }
        ));
        assert!(matches!(
            editor
                .request(Request::CloseSplint {
                    expected_topology_revision: splinterm_core::TopologyRevision::new(9),
                    splint_id: sibling_id,
                })
                .await,
            Response::TopologyCommitted { topology_revision } if topology_revision.get() == 10
        ));
        let Response::Topology { snapshot } = editor.request(Request::InspectTopology).await else {
            panic!("topology inspection failed");
        };
        snapshot.validate().unwrap();
        assert_eq!(snapshot.revision.get(), 10);
        assert_eq!(
            snapshot.topology.find_dojo(dojo_id).unwrap().default_focus,
            initial_id
        );
        assert_eq!(snapshot.runtimes.len(), 1);
        daemon.shutdown();
    })
    .await
    .expect("topology CAS scenario timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn atomic_lair_termination_rejects_drift_and_removes_exact_runtime_set() {
    time::timeout(TEST_TIMEOUT, async {
        let daemon = Daemon::start().await;
        let mut client = daemon.connect().await;
        let launch = LaunchParameters {
            cwd: std::env::current_dir().unwrap(),
            command: vec!["/bin/sh".into(), "-c".into(), "exec sleep 30".into()],
            shell: None,
            login_shell: false,
            scrollback_lines: 100,
        };
        let Response::LairCreated {
            lair,
            incarnation,
            topology_revision,
        } = client
            .request(Request::CreateLair {
                expected_topology_revision: TopologyRevision::default(),
                name: "terminate-exact".into(),
                launch,
                generated_name: false,
            })
            .await
        else {
            panic!("test Lair was not created");
        };
        let dojo_id = lair.dojos[0].id;
        let splint_id = lair.dojos[0].default_focus;
        let target = MutationTarget {
            lair_id: lair.id,
            dojo_id,
            splint_id,
            incarnation,
        };
        let mut stale = target.clone();
        stale.incarnation = stale.incarnation.saturating_add(1);
        let failure = client
            .request_result(Request::TerminateLair {
                expected_topology_revision: topology_revision,
                lair_id: lair.id,
                targets: vec![stale],
            })
            .await
            .unwrap_err();
        assert_eq!(failure.code, ErrorCode::InvalidArgument);
        let Response::Topology { snapshot } = client.request(Request::InspectTopology).await else {
            panic!("topology inspection failed");
        };
        assert_eq!(snapshot.revision, topology_revision);
        assert!(
            snapshot
                .topology
                .lairs()
                .any(|current| current.id == lair.id)
        );
        assert!(matches!(
            client
                .request(Request::TerminateLair {
                    expected_topology_revision: topology_revision,
                    lair_id: lair.id,
                    targets: vec![target],
                })
                .await,
            Response::TopologyCommitted { topology_revision: committed }
                if committed.get() == topology_revision.get() + 1
        ));
        let Response::Topology { snapshot } = client.request(Request::InspectTopology).await else {
            panic!("topology inspection failed");
        };
        assert!(
            snapshot
                .topology
                .lairs()
                .all(|current| current.id != lair.id)
        );
        assert!(
            snapshot
                .runtimes
                .iter()
                .all(|runtime| runtime.splint_id != splint_id)
        );
        daemon.shutdown();
    })
    .await
    .expect("atomic Lair termination scenario timed out");
}

// Terminate Dojo kills each Splint, then closes the Dojo. A Window treats a
// subscription that ends without an exit notice as a broken stream, so every
// subscriber must still receive the exit after the Dojo closes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killed_splint_still_notifies_subscribers_after_its_dojo_closes() {
    time::timeout(TEST_TIMEOUT, async {
        let daemon = Daemon::start().await;
        let cwd = std::env::current_dir().unwrap();
        let mut admin = daemon.connect().await;
        let Response::LairCreated { lair, .. } = admin
            .request(Request::CreateLair {
                expected_topology_revision: splinterm_core::TopologyRevision::default(),
                name: "terminate-dojo".into(),
                generated_name: false,
                launch: LaunchParameters {
                    cwd,
                    command: vec!["/bin/sh".into()],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 100,
                },
            })
            .await
        else {
            panic!("daemon did not create the Lair");
        };
        let dojo_id = lair.dojos[0].id;
        let LayoutNode::Leaf(splint) = &lair.dojos[0].root else {
            panic!("new Dojo was not a leaf");
        };
        let splint_id = splint.id;
        let incarnation = admin.live_incarnation(splint_id).await;
        let mut subscriber = daemon.connect().await;
        let (subscription_id, _) = subscriber.attach(splint_id, incarnation).await;

        assert!(matches!(
            admin
                .request(Request::KillSplint {
                    splint_id,
                    incarnation,
                })
                .await,
            Response::SplintKilled { .. }
        ));
        let expected_topology_revision = admin.topology_revision().await;
        assert!(matches!(
            admin
                .request(Request::CloseDojo {
                    expected_topology_revision,
                    dojo_id,
                })
                .await,
            Response::TopologyCommitted { .. }
        ));

        loop {
            match read_frame_or_eof(&mut subscriber.stream).await {
                Some(ServerFrame::Event {
                    subscription_id: event_subscription,
                    event: SubscriptionEvent::Exited { .. },
                    ..
                }) if event_subscription == subscription_id => break,
                Some(ServerFrame::Event { .. }) => {}
                Some(frame) => panic!("unexpected subscription frame: {frame:?}"),
                None => panic!("subscription ended without an exit notice"),
            }
        }
        daemon.shutdown();
    })
    .await
    .expect("terminated Splint subscription scenario timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn termination_waiting_on_a_resistant_process_does_not_block_topology_changes() {
    time::timeout(TEST_TIMEOUT, async {
        let daemon = Daemon::start().await;
        let marker = daemon.runtime.join("resistant-terminated-pid");
        let mut closer = daemon.connect().await;
        let mut editor = daemon.connect().await;
        let Response::LairCreated {
            lair,
            incarnation,
            topology_revision,
        } = closer
            .request(Request::CreateLair {
                expected_topology_revision: TopologyRevision::default(),
                name: "resistant-terminate".into(),
                launch: LaunchParameters {
                    cwd: std::env::current_dir().unwrap(),
                    command: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        format!(
                            "trap '' HUP TERM; printf '%s\\n' $$ > {}; exec sleep 75",
                            marker.display()
                        ),
                    ],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 100,
                },
                generated_name: false,
            })
            .await
        else {
            panic!("signal-resistant Lair was not created");
        };
        let child_pid = wait_for_pid_marker(&marker, "signal-resistant child").await;
        let target = MutationTarget {
            lair_id: lair.id,
            dojo_id: lair.dojos[0].id,
            splint_id: lair.dojos[0].default_focus,
            incarnation,
        };
        let lair_id = lair.id;
        let termination = tokio::spawn(async move {
            closer
                .request(Request::TerminateLair {
                    expected_topology_revision: topology_revision,
                    lair_id,
                    targets: vec![target],
                })
                .await
        });

        // The termination commits at once and then waits out both shutdown
        // graces; an unrelated topology change must commit during that wait.
        let deadline = Instant::now() + Duration::from_secs(5);
        let committed = loop {
            let current = editor.topology_revision().await;
            if current != topology_revision {
                break current;
            }
            assert!(Instant::now() < deadline, "termination did not commit");
            time::sleep(Duration::from_millis(10)).await;
        };
        assert!(matches!(
            editor
                .request(Request::CreateLair {
                    expected_topology_revision: committed,
                    name: "created-during-termination".into(),
                    launch: LaunchParameters {
                        cwd: std::env::current_dir().unwrap(),
                        command: vec!["/bin/sh".into(), "-c".into(), "exec sleep 30".into()],
                        shell: None,
                        login_shell: false,
                        scrollback_lines: 100,
                    },
                    generated_name: false,
                })
                .await,
            Response::LairCreated { .. }
        ));
        assert!(
            !termination.is_finished(),
            "the topology change waited for the terminated process"
        );

        assert!(matches!(
            termination.await.unwrap(),
            Response::TopologyCommitted { topology_revision: revision } if revision == committed
        ));
        assert!(!Path::new(&format!("/proc/{child_pid}")).exists());
        daemon.shutdown();
    })
    .await
    .expect("resistant termination scenario timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bounded_scrollback_search_pages_and_cursors_outlive_new_output() {
    time::timeout(TEST_TIMEOUT, async {
        let daemon = Daemon::start().await;
        let mut client = daemon.connect().await;
        let Response::LairCreated { lair: dojo, .. } = client
            .request(Request::CreateLair {
                expected_topology_revision: splinterm_core::TopologyRevision::default(),
                name: "search".into(),
                launch: LaunchParameters {
                    cwd: std::env::current_dir().unwrap(),
                    command: vec!["/bin/sh".into()],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 100,
                },
                generated_name: false,
            })
            .await
        else {
            panic!("daemon did not create search Splint");
        };
        let LayoutNode::Leaf(splint) = &dojo.dojos[0].root else {
            panic!("new dojo was not a leaf");
        };
        let splint_id = splint.id;
        let incarnation = client.live_incarnation(splint_id).await;
        client
            .input(
                splint_id,
                incarnation,
                // The matches scroll into history, where a row cursor
                // outlives new output.
                "printf 'Needle one\\nnoise\\nneedle two 界\\n'; seq 1 80; echo scrolled-$((1+1))\n"
                    .as_bytes(),
            )
            .await;
        snapshot_until(&mut client, splint_id, incarnation, "scrolled-2").await;
        time::sleep(Duration::from_millis(50)).await;
        let (_, snapshot) = client.attach(splint_id, incarnation).await;
        let search_in = |history_generation, cursor| Request::SearchScrollback {
            splint_id,
            incarnation,
            terminal_revision: snapshot.revision,
            history_generation,
            query: "NEEDLE".into(),
            case_sensitive: false,
            cursor,
            max_results: 1,
        };
        let search = |cursor| search_in(snapshot.history_generation, cursor);
        let Response::SearchResults { page: first, .. } = client.request(search(None)).await else {
            panic!("daemon did not return first search page");
        };
        first.validate().unwrap();
        assert_eq!(first.matches.len(), 1);
        let cursor = first.next_cursor.clone().expect("older match cursor");
        let Response::SearchResults { page: second, .. } =
            client.request(search(Some(cursor))).await
        else {
            panic!("daemon did not return second search page");
        };
        assert_eq!(second.matches.len(), 1);
        assert_ne!(first.matches[0].row_id, second.matches[0].row_id);

        // New output keeps the cursor: it names a row, not an offset.
        client
            .input(splint_id, incarnation, b"printf 'revision-change\\n'\n")
            .await;
        snapshot_until(&mut client, splint_id, incarnation, "revision-change").await;
        let Response::SearchResults {
            page: after_output, ..
        } = client.request(search(first.next_cursor.clone())).await
        else {
            panic!("new output staled a row cursor");
        };
        assert_eq!(after_output.matches, second.matches);

        // Another history generation does stale it.
        assert!(matches!(
            client
                .request(search_in(
                    snapshot.history_generation + 1,
                    first.next_cursor
                ))
                .await,
            Response::SearchResyncRequired { .. }
        ));
        daemon.shutdown();
    })
    .await
    .expect("search scenario timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one connection-level scenario proves denial, acceptance, stale ownership, input, and disconnect"
)]
async fn simultaneous_clients_transfer_control_explicitly() {
    time::timeout(TEST_TIMEOUT, async {
        let daemon = Daemon::start().await;
        let mut admin = daemon.connect().await;
        let Response::LairCreated { lair: dojo, .. } = admin
            .request(Request::CreateLair {
                expected_topology_revision: splinterm_core::TopologyRevision::default(),
                name: "control-transfer".into(),
                launch: LaunchParameters {
                    cwd: std::env::current_dir().unwrap(),
                    command: vec!["/bin/sh".into()],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 100,
                },
                generated_name: false,
            })
            .await
        else {
            panic!("daemon did not create transfer Splint");
        };
        let LayoutNode::Leaf(splint) = &dojo.dojos[0].root else {
            panic!("new dojo was not a leaf");
        };
        let splint_id = splint.id;
        let incarnation = admin.live_incarnation(splint_id).await;

        let mut owner = daemon.connect().await;
        let mut requester = daemon.connect().await;
        let owner_subscription = owner.subscribe_control(splint_id, incarnation).await;
        let requester_subscription = requester.subscribe_control(splint_id, incarnation).await;
        let owner_controller = owner.acquire_control(splint_id, incarnation).await;

        let Response::ControlTransferPending { transfer_id, .. } = requester
            .request(Request::RequestControlTransfer {
                splint_id,
                incarnation,
                modes: vec![ControlMode::Input, ControlMode::Resize],
            })
            .await
        else {
            panic!("control transfer was not queued");
        };
        loop {
            if matches!(
                owner.next_event(owner_subscription).await.1,
                SubscriptionEvent::ControlTransferRequested { transfer_id: seen }
                    if seen == transfer_id
            ) {
                break;
            }
        }
        assert_eq!(
            owner
                .request(Request::DecideControlTransfer {
                    transfer_id,
                    decision: ControlTransferDecision::Deny,
                })
                .await,
            Response::ControlTransferDecided {
                outcome: ControlTransferOutcome::Denied,
                controller_id: None,
            }
        );
        assert!(matches!(
            requester.next_event(requester_subscription).await.1,
            SubscriptionEvent::ControlTransferResolved {
                transfer_id: seen,
                outcome: ControlTransferOutcome::Denied,
                controller_id: None,
            } if seen == transfer_id
        ));

        let Response::ControlTransferPending { transfer_id, .. } = requester
            .request(Request::RequestControlTransfer {
                splint_id,
                incarnation,
                modes: vec![ControlMode::Input, ControlMode::Resize],
            })
            .await
        else {
            panic!("second control transfer was not queued");
        };
        loop {
            if matches!(
                owner.next_event(owner_subscription).await.1,
                SubscriptionEvent::ControlTransferRequested { transfer_id: seen }
                    if seen == transfer_id
            ) {
                break;
            }
        }
        assert!(matches!(
            owner
                .request(Request::DecideControlTransfer {
                    transfer_id,
                    decision: ControlTransferDecision::Accept,
                })
                .await,
            Response::ControlTransferDecided {
                outcome: ControlTransferOutcome::Granted,
                controller_id: Some(_),
            }
        ));
        let transferred_controller = loop {
            if let SubscriptionEvent::ControlTransferResolved {
                transfer_id: seen,
                outcome: ControlTransferOutcome::Granted,
                controller_id: Some(controller_id),
            } = requester.next_event(requester_subscription).await.1
            {
                assert_eq!(seen, transfer_id);
                break controller_id;
            }
        };
        let stale = owner
            .request_result(Request::Input {
                controller_id: owner_controller,
                splint_id,
                incarnation,
                bytes: b"stale".to_vec(),
                focus_report: false,
            })
            .await
            .unwrap_err();
        assert_eq!(stale.code, ErrorCode::Unauthorized);
        assert!(matches!(
            requester
                .request(Request::Input {
                    controller_id: transferred_controller,
                    splint_id,
                    incarnation,
                    bytes: b"printf 'transferred-control\\n'\n".to_vec(),
                    focus_report: false,
                })
                .await,
            Response::TerminalActionAcknowledged {
                splint_id: response_id,
                incarnation: response_incarnation,
                ..
            } if response_id == splint_id && response_incarnation == incarnation
        ));
        snapshot_until(&mut admin, splint_id, incarnation, "transferred-control").await;

        drop(requester);
        let mut replacement = daemon.connect().await;
        let replacement_controller = replacement.acquire_control(splint_id, incarnation).await;
        assert_ne!(replacement_controller, transferred_controller);
        daemon.shutdown();
    })
    .await
    .expect("control transfer scenario timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one isolated scenario proves split, independent output, kill, relaunch, and stale rejection"
)]
async fn two_splints_spawn_and_preserve_independent_output() {
    time::timeout(TEST_TIMEOUT, async {
        let daemon = Daemon::start().await;
        let cwd = std::env::current_dir().unwrap();
        let shell_launch = || LaunchParameters {
            cwd: cwd.clone(),
            command: vec!["/bin/sh".into()],
            shell: None,
            login_shell: false,
            scrollback_lines: 100,
        };
        let mut admin = daemon.connect().await;
        let Response::LairCreated { lair: dojo, .. } = admin
            .request(Request::CreateLair {
                expected_topology_revision: splinterm_core::TopologyRevision::default(),
                name: "multiplex".into(),
                launch: shell_launch(),
                generated_name: false,
            })
            .await
        else {
            panic!("daemon did not create initial Splint");
        };
        let lair_id = dojo.id;
        let dojo_id = dojo.dojos[0].id;
        let LayoutNode::Leaf(first) = &dojo.dojos[0].root else {
            panic!("new dojo was not a leaf");
        };
        let first_id = first.id;
        let first_incarnation = admin.live_incarnation(first_id).await;
        admin.resize(first_id, first_incarnation, 91, 43).await;
        admin.release_control().await;

        let Response::SplintStarted {
            splint_id: second_id,
            incarnation: second_incarnation,
            topology_revision,
        } = admin
            .request(Request::SplitSplint {
                expected_topology_revision: splinterm_core::TopologyRevision::new(1),
                target_splint_id: first_id,
                axis: Axis::Horizontal,
                side: SplitSide::Second,
                ratio: SplitRatio::new(500).unwrap(),
                launch: shell_launch(),
            })
            .await
        else {
            panic!("daemon did not split initial Splint");
        };
        assert_eq!(topology_revision.get(), 2);
        assert_ne!(first_id, second_id);
        let mut initial_size_probe = daemon.connect().await;
        let (_, second_initial) = initial_size_probe
            .attach(second_id, second_incarnation)
            .await;
        assert_eq!((second_initial.columns, second_initial.rows), (91, 43));
        assert!(
            second_initial.cursor_row <= 1,
            "new shell prompt started at row {} of {}",
            second_initial.cursor_row,
            second_initial.rows
        );
        drop(initial_size_probe);

        let Response::Topology { snapshot: before } = admin.request(Request::InspectTopology).await
        else {
            panic!("daemon did not return topology");
        };
        let failed = admin
            .request_result(Request::SplitSplint {
                expected_topology_revision: topology_revision,
                target_splint_id: first_id,
                axis: Axis::Vertical,
                side: SplitSide::First,
                ratio: SplitRatio::new(400).unwrap(),
                launch: LaunchParameters {
                    command: vec!["/definitely/missing/splinterm-command".into()],
                    ..shell_launch()
                },
            })
            .await
            .unwrap_err();
        assert_eq!(failed.code, ErrorCode::Internal);
        let Response::Topology { snapshot: after } = admin.request(Request::InspectTopology).await
        else {
            panic!("daemon did not return topology after failed split");
        };
        assert_eq!(after, before);
        after.validate().unwrap();

        let mut first_client = daemon.connect().await;
        let mut second_client = daemon.connect().await;
        let first_controller = first_client
            .acquire_control(first_id, first_incarnation)
            .await;
        let second_controller = second_client
            .acquire_control(second_id, second_incarnation)
            .await;
        assert_ne!(first_controller, second_controller);
        let context_command = b"printf 'CTX=%s|%s|%s|%s\\n' \"$SPLINTERM_LAIR_ID\" \"$SPLINTERM_DOJO_ID\" \"$SPLINTERM_SPLINT_ID\" \"$SPLINTERM_SPLINT_INCARNATION\"\n";
        first_client
            .input(first_id, first_incarnation, context_command)
            .await;
        second_client
            .input(second_id, second_incarnation, context_command)
            .await;
        first_client
            .input(first_id, first_incarnation, b"printf 'first-ready\\n'\n")
            .await;
        second_client
            .input(second_id, second_incarnation, b"printf 'second-ready\\n'\n")
            .await;
        first_client
            .resize(first_id, first_incarnation, 90, 30)
            .await;
        second_client
            .resize(second_id, second_incarnation, 100, 40)
            .await;

        let first_snapshot =
            snapshot_until(&mut admin, first_id, first_incarnation, "first-ready").await;
        let second_snapshot =
            snapshot_until(&mut admin, second_id, second_incarnation, "second-ready").await;
        assert_eq!((first_snapshot.columns, first_snapshot.rows), (90, 30));
        assert_eq!((second_snapshot.columns, second_snapshot.rows), (100, 40));
        let first_text = snapshot_text(&first_snapshot);
        let second_text = snapshot_text(&second_snapshot);
        assert!(first_text.contains(&format!(
            "CTX={lair_id}|{dojo_id}|{first_id}|{first_incarnation}"
        )));
        assert!(second_text.contains(&format!(
            "CTX={lair_id}|{dojo_id}|{second_id}|{second_incarnation}"
        )));
        assert!(!first_text.contains("caller-supplied"));
        assert!(!second_text.contains("caller-supplied"));
        assert!(!first_text.contains("second-ready"));
        assert!(!second_text.contains("first-ready"));

        assert!(matches!(
            admin
                .request(Request::KillSplint {
                    splint_id: first_id,
                    incarnation: first_incarnation,
                })
                .await,
            Response::SplintKilled { .. }
        ));
        second_client
            .input(
                second_id,
                second_incarnation,
                b"printf 'second-after-kill\\n'\n",
            )
            .await;
        snapshot_until(
            &mut admin,
            second_id,
            second_incarnation,
            "second-after-kill",
        )
        .await;

        let expected_topology_revision = admin.topology_revision().await;
        let Response::SplintStarted {
            splint_id: relaunched_id,
            incarnation: relaunched_incarnation,
            topology_revision,
        } = admin
            .request(Request::RelaunchSplint {
                expected_topology_revision,
                splint_id: first_id,
                launch: LaunchParameters {
                    command: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        "printf 'RELAUNCH=%s|%s|%s|%s\\n' \"$SPLINTERM_LAIR_ID\" \"$SPLINTERM_DOJO_ID\" \"$SPLINTERM_SPLINT_ID\" \"$SPLINTERM_SPLINT_INCARNATION\"; exec sleep 30".into(),
                    ],
                    ..shell_launch()
                },
            })
            .await
        else {
            panic!("daemon did not relaunch exited Splint");
        };
        assert_eq!(relaunched_id, first_id);
        assert_ne!(relaunched_incarnation, first_incarnation);
        assert_eq!(topology_revision.get(), 2);
        let stale = first_client
            .request_result(Request::Input {
                controller_id: first_controller,
                splint_id: first_id,
                incarnation: first_incarnation,
                bytes: b"stale".to_vec(),
                focus_report: false,
            })
            .await
            .unwrap_err();
        assert_eq!(stale.code, ErrorCode::StaleIncarnation);
        let relaunched = snapshot_until(
            &mut admin,
            first_id,
            relaunched_incarnation,
            "RELAUNCH=",
        )
        .await;
        let relaunched_text = snapshot_text(&relaunched);
        assert!(relaunched_text.contains(&format!(
            "RELAUNCH={lair_id}|{dojo_id}|{first_id}|{relaunched_incarnation}"
        )));
        assert!(!relaunched_text.contains("caller-supplied"));

        // Both remaining live processes are intentionally left to daemon shutdown,
        // which must drain the registry, reap them, and remove the socket.
        daemon.shutdown();
    })
    .await
    .expect("two-Splint scenario timed out");
}

fn history_test_rows(first: u64, last: u64) -> Vec<TerminalRow> {
    (first..=last)
        .map(|id| TerminalRow {
            row_id: Some(id),
            linebreak: false,
            cells: Vec::new(),
        })
        .collect()
}

fn history_test_snapshot(first: u64, last: u64, available: usize) -> TerminalSnapshot {
    let history = history_test_rows(first, last);
    let snapshot = TerminalSnapshot {
        splint_id: SplintId::new(),
        incarnation: 1,
        revision: 1,
        columns: 1,
        rows: 1,
        cursor_column: 0,
        cursor_row: 0,
        cursor_deferred_wrap: false,
        active_screen: ActiveScreen::Normal,
        input_modes: TerminalInputModes {
            application_cursor: false,
            application_keypad: false,
            focus_reporting: false,
            bracketed_paste: false,
            cursor_visible: true,
            cursor_blink: false,
            mouse_tracking: MouseTracking::None,
            sgr_mouse: false,
        },
        palette: vec![0; 256],
        default_colors: [0; 3],
        title: String::new(),
        visible_rows: history_test_rows(10_000, 10_000),
        history_generation: 1,
        oldest_available_scrollback_row_id: Some(last + 1 - u64::try_from(available).unwrap()),
        newest_available_scrollback_row_id: Some(last),
        omitted_oldest_scrollback_rows: available - history.len(),
        scrollback_rows: history,
        available_scrollback_rows: available,
        images: None,
        exited_code: None,
        exited_signal: None,
    };
    snapshot.validate().expect("valid reconstruction fixture");
    snapshot
}

fn history_test_append(
    snapshot: &TerminalSnapshot,
    appended: usize,
    trimmed: usize,
) -> TerminalUpdate {
    let available = snapshot.available_scrollback_rows + appended - trimmed;
    let newest =
        snapshot.newest_available_scrollback_row_id.unwrap() + u64::try_from(appended).unwrap();
    let returned = appended.min(MAX_SNAPSHOT_SCROLLBACK_ROWS).min(available);
    TerminalUpdate {
        base_revision: snapshot.revision,
        revision: snapshot.revision + 1,
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
        images: None,
        scrollback: Some(TerminalScrollbackUpdate {
            transition: HistoryTransition::Append {
                appended_rows: appended,
                trimmed_rows: trimmed,
            },
            history_generation: snapshot.history_generation,
            oldest_available_row_id: Some(newest + 1 - u64::try_from(available).unwrap()),
            newest_available_row_id: Some(newest),
            rows: history_test_rows(newest + 1 - u64::try_from(returned).unwrap(), newest),
            available_rows: available,
            omitted_oldest_rows: available - returned,
        }),
    }
}

#[test]
fn reconstructed_history_bounds_small_appends_to_the_wire_snapshot_limit() {
    let mut snapshot = history_test_snapshot(1, 16, 16);
    let update = history_test_append(&snapshot, 2, 0);
    apply_terminal_update(&mut snapshot, update);
    assert_eq!(snapshot.scrollback_rows, history_test_rows(3, 18));
    assert_eq!(snapshot.omitted_oldest_scrollback_rows, 2);
}

#[test]
fn reconstructed_history_replaces_cached_rows_when_append_exceeds_returned_tail() {
    let mut snapshot = history_test_snapshot(1, 16, 16);
    let update = history_test_append(&snapshot, 66, 0);
    apply_terminal_update(&mut snapshot, update);
    assert_eq!(snapshot.scrollback_rows, history_test_rows(67, 82));
    assert_eq!(snapshot.omitted_oldest_scrollback_rows, 66);
}

#[test]
fn reconstructed_history_does_not_trim_cached_rows_for_omitted_history() {
    let mut snapshot = history_test_snapshot(13, 20, 20);
    let update = history_test_append(&snapshot, 1, 1);
    apply_terminal_update(&mut snapshot, update);
    assert_eq!(snapshot.scrollback_rows, history_test_rows(13, 21));
    assert_eq!(snapshot.omitted_oldest_scrollback_rows, 11);
}

#[test]
fn reconstructed_history_trims_cached_rows_only_after_omitted_history() {
    let mut snapshot = history_test_snapshot(4, 8, 8);
    let update = history_test_append(&snapshot, 5, 5);
    apply_terminal_update(&mut snapshot, update);
    assert_eq!(snapshot.scrollback_rows, history_test_rows(6, 13));
    assert_eq!(snapshot.omitted_oldest_scrollback_rows, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mixed_clear_subscription_reconstructs_exact_final_snapshot_without_resync() {
    time::timeout(Duration::from_secs(60), async {
        let daemon = Daemon::start().await;
        let mut creator = daemon.connect().await;
        let script = r"i=0; while [ $i -lt 5000 ]; do if [ $i -gt 0 ] && [ $((i % 500)) -eq 0 ]; then printf '\033[2J\033[H'; fi; case $((i % 3)) in 0) printf 'plan0043-%08d plain xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n' $i;; 1) printf '\033[3%dmplan0043-%08d ansi xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\033[0m\n' $((i % 8)) $i;; 2) printf 'plan0043-%08d unicode-naive-cafe-lambda-emoji\n' $i;; esac; i=$((i+1)); done; printf 'PLAN0043_%s\n' FINAL; sleep 30";
        let lair = match creator
            .request(Request::CreateLair {
                expected_topology_revision: TopologyRevision::default(),
                name: "plan0043-reconstruction".into(),
                launch: LaunchParameters {
                    cwd: std::env::current_dir().unwrap(),
                    command: vec!["/bin/sh".into()],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 100,
                },
                generated_name: false,
            })
            .await
        {
            Response::LairCreated { lair, .. } => lair,
            response => panic!("unexpected create response: {response:?}"),
        };
        let LayoutNode::Leaf(splint) = &lair.dojos[0].root else {
            unreachable!()
        };
        let splint_id = splint.id;
        let incarnation = creator.live_incarnation(splint_id).await;
        let mut subscriber = daemon.connect().await;
        let (subscription_id, mut reconstructed) = subscriber
            .attach_with_scrollback(splint_id, incarnation, 100)
            .await;

        let mut workload = script.as_bytes().to_vec();
        workload.push(b'\n');
        creator.input(splint_id, incarnation, &workload).await;
        let mut expected_sequence = 1_u64;
        let mut updates = 0_usize;
        loop {
            let (sequence, event) = subscriber.next_event(subscription_id).await;
            assert_eq!(sequence, expected_sequence);
            expected_sequence += 1;
            match event {
                SubscriptionEvent::Update { update } => {
                    apply_terminal_update(&mut reconstructed, update);
                    updates += 1;
                    if snapshot_text(&reconstructed).contains("PLAN0043_FINAL") {
                        break;
                    }
                }
                SubscriptionEvent::Snapshot { snapshot } => reconstructed = snapshot,
                SubscriptionEvent::ResyncRequired { current_revision } => {
                    panic!("fast subscriber required resync at revision {current_revision}")
                }
                SubscriptionEvent::Exited { .. } => {
                    panic!("terminal exited before final reconstruction")
                }
                event => panic!("unexpected terminal event: {event:?}"),
            }
        }
        assert!(updates > 1, "workload must cross publication boundaries");

        let mut observer = daemon.connect().await;
        let (observer_subscription, authoritative) = observer
            .attach_with_scrollback(splint_id, incarnation, 100)
            .await;
        assert_eq!(
            observer
                .request(Request::Detach {
                    subscription_id: observer_subscription,
                })
                .await,
            Response::Acknowledged
        );
        assert_eq!(reconstructed, authoritative);
        assert_eq!(
            subscriber
                .request(Request::Detach { subscription_id })
                .await,
            Response::Acknowledged
        );
        daemon.shutdown();
    })
    .await
    .expect("Plan 0043 reconstruction scenario timed out");
}

#[test]
fn phase8_completion_markers_are_output_only_and_preserve_workload() {
    for (workload, prefix, count, marker) in [
        (PHASE8_PACED_WORKLOAD, "paced", 2000, "overflow-finished"),
        (
            PHASE8_PRESSURE_WORKLOAD,
            "pressure",
            30000,
            "pressure-finished",
        ),
    ] {
        let command = std::str::from_utf8(workload).unwrap();
        assert!(
            !command.contains(marker),
            "terminal echo must not signal completion"
        );
        let output = Command::new("/bin/sh")
            .args(["-c", command])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .output()
            .expect("run the exact pressure fixture");
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let expected: String = (0..count)
            .map(|index| format!("{prefix}-{index:05}-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n"))
            .chain(std::iter::once(format!("{marker}\n")))
            .collect();
        assert_eq!(output.stdout, expected.as_bytes());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "the single scenario intentionally records the complete Phase 8 lifecycle"
)]
async fn phase8_detach_reattach_overflow_resync_and_cleanup() {
    time::timeout(Duration::from_secs(120), async {
        let daemon = Daemon::start().await;
        let mut creator = daemon.connect().await;
        let cwd = std::env::current_dir().unwrap();
        let dojo = match creator
            .request(Request::CreateLair {
                expected_topology_revision: splinterm_core::TopologyRevision::default(),
                name: "phase8".into(),
                launch: LaunchParameters {
                    cwd: cwd.clone(),
                    command: Vec::new(),
                    shell: None,
                    login_shell: true,
                    scrollback_lines: 1_000,
                },
                generated_name: false,
            })
            .await
        {
            Response::LairCreated { lair: dojo, .. } => dojo,
            response => panic!("unexpected create response: {response:?}"),
        };
        let LayoutNode::Leaf(model_splint) = &dojo.dojos[0].root else {
            unreachable!()
        };
        let splint_id = model_splint.id;
        let incarnation = creator.live_incarnation(splint_id).await;

        time::sleep(Duration::from_millis(200)).await;
        creator
            .input(
                splint_id,
                incarnation,
                b"clear\nprintf '\\033[31mRED\\033[0m phase8-initial\\n'; pwd\n",
            )
            .await;
        snapshot_until(&mut creator, splint_id, incarnation, "phase8-initial").await;
        let with_pwd =
            snapshot_until(&mut creator, splint_id, incarnation, cwd.to_str().unwrap()).await;
        assert!(snapshot_text(&with_pwd).contains(cwd.to_str().unwrap()));
        with_pwd
            .validate()
            .expect("daemon snapshot identity is valid");
        assert!(
            with_pwd
                .visible_rows
                .iter()
                .chain(&with_pwd.scrollback_rows)
                .all(|row| row.row_id.is_some_and(|id| id > 0))
        );
        assert!(
            with_pwd
                .visible_rows
                .iter()
                .chain(&with_pwd.scrollback_rows)
                .flat_map(|row| &row.cells)
                .any(|cell| {
                    cell.content == "R" && cell.attributes.foreground_source != ColorSource::Default
                })
        );

        let creator_controller = creator.acquire_control(splint_id, incarnation).await;
        assert!(matches!(
            creator
                .request(Request::Resize {
                    controller_id: creator_controller,
                    splint_id,
                    incarnation,
                    columns: 100,
                    rows: 30,
                    pixel_width: 800,
                    pixel_height: 600,
                })
                .await,
            Response::TerminalActionAcknowledged {
                splint_id: response_id,
                incarnation: response_incarnation,
                ..
            } if response_id == splint_id && response_incarnation == incarnation
        ));
        let resized = snapshot_until(&mut creator, splint_id, incarnation, "phase8-initial").await;
        assert_eq!((resized.columns, resized.rows), (100, 30));
        assert!(resized.history_generation > with_pwd.history_generation);

        drop(creator);
        let mut detached_writer = daemon.connect().await;
        detached_writer
            .input(splint_id, incarnation, b"printf 'while-detached\\n'\n")
            .await;
        drop(detached_writer);
        time::sleep(Duration::from_millis(100)).await;

        let mut reattached = daemon.connect().await;
        let detached =
            snapshot_until(&mut reattached, splint_id, incarnation, "while-detached").await;
        assert!(detached.revision > resized.revision);

        let reattached_controller = reattached.acquire_control(splint_id, incarnation).await;
        assert!(matches!(
            reattached
                .request(Request::Resize {
                    controller_id: reattached_controller,
                    splint_id,
                    incarnation,
                    columns: 40,
                    rows: 10,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .await,
            Response::TerminalActionAcknowledged {
                splint_id: response_id,
                incarnation: response_incarnation,
                ..
            } if response_id == splint_id && response_incarnation == incarnation
        ));
        reattached.release_control().await;
        let mut fast = daemon.connect().await;
        let (fast_subscription, _) = fast.attach(splint_id, incarnation).await;
        let fast_drain = tokio::spawn(async move {
            loop {
                match fast.next_event(fast_subscription).await.1 {
                    SubscriptionEvent::Update { update }
                        if update_text(&update).contains("overflow-finished") =>
                    {
                        break;
                    }
                    SubscriptionEvent::Snapshot { snapshot }
                        if snapshot_text(&snapshot).contains("overflow-finished") =>
                    {
                        break;
                    }
                    SubscriptionEvent::ResyncRequired { .. } => {
                        panic!("drained subscriber required resynchronization")
                    }
                    SubscriptionEvent::Exited { .. } => {
                        panic!("terminal exited before the overflow marker")
                    }
                    _ => {}
                }
            }
        });
        let mut producer = daemon.connect().await;
        // First prove that an actively drained subscriber keeps up with paced
        // producer frames. Slow-runner scheduling must not be conflated with the
        // separate unread-connection overflow proof below.
        producer
            .input(splint_id, incarnation, PHASE8_PACED_WORKLOAD)
            .await;
        let _completion_snapshot = snapshot_until_with_timeout(
            &mut reattached,
            splint_id,
            incarnation,
            "overflow-finished",
            Duration::from_secs(60),
        )
        .await;
        time::timeout(Duration::from_secs(30), fast_drain)
            .await
            .expect("drained subscriber did not observe the overflow marker")
            .expect("drained subscriber task failed");
        let _pre_pressure_snapshot = stable_snapshot_after_marker(
            &mut reattached,
            splint_id,
            incarnation,
            "overflow-finished",
        )
        .await;

        // The actively drained client has exited. Create a fresh unread
        // MAX_SUBSCRIPTIONS connection so the following unpaced pressure stream
        // must resolve through exact coalesced delivery, resynchronization, or
        // disconnection.
        let mut slow = daemon.connect().await;
        nix::sys::socket::setsockopt(&slow.stream, nix::sys::socket::sockopt::RcvBuf, &4096)
            .unwrap();
        let mut slow_subscriptions = std::collections::BTreeMap::new();
        for _ in 0..MAX_SUBSCRIPTIONS {
            let (subscription_id, snapshot) = slow.attach(splint_id, incarnation).await;
            assert!(
                slow_subscriptions
                    .insert(subscription_id, (snapshot, 1_u64))
                    .is_none()
            );
        }
        producer
            .input(splint_id, incarnation, PHASE8_PRESSURE_WORKLOAD)
            .await;
        let _pressure_snapshot = snapshot_until_with_timeout(
            &mut reattached,
            splint_id,
            incarnation,
            "pressure-finished",
            Duration::from_secs(60),
        )
        .await;
        let final_snapshot = stable_snapshot_after_marker(
            &mut reattached,
            splint_id,
            incarnation,
            "pressure-finished",
        )
        .await;

        let mut caught_up = false;
        let mut saw_resync = false;
        let mut disconnected = false;
        let slow_read_deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < slow_read_deadline {
            let Ok(frame) = time::timeout(
                slow_read_deadline.saturating_duration_since(Instant::now()),
                read_frame_or_eof(&mut slow.stream),
            )
            .await
            else {
                break;
            };
            let Some(frame) = frame else {
                disconnected = true;
                break;
            };
            let ServerFrame::Event {
                subscription_id,
                sequence,
                event,
            } = frame
            else {
                continue;
            };
            let (reconstructed, expected_sequence) = slow_subscriptions
                .get_mut(&subscription_id)
                .expect("slow connection emitted an event for an unknown subscription");
            match event {
                SubscriptionEvent::Update { update } => {
                    assert_eq!(sequence, *expected_sequence);
                    *expected_sequence += 1;
                    apply_terminal_update(reconstructed, update);
                    if reconstructed.revision == final_snapshot.revision
                        && snapshot_text(reconstructed).contains("pressure-finished")
                    {
                        assert_eq!(&*reconstructed, &final_snapshot);
                        caught_up = true;
                        break;
                    }
                }
                SubscriptionEvent::Snapshot { snapshot } => {
                    assert_eq!(sequence, *expected_sequence);
                    *expected_sequence += 1;
                    snapshot
                        .validate()
                        .expect("slow subscriber snapshot is valid");
                    assert_eq!(snapshot.splint_id, splint_id);
                    assert_eq!(snapshot.incarnation, incarnation);
                    *reconstructed = snapshot;
                    if reconstructed.revision == final_snapshot.revision
                        && snapshot_text(reconstructed).contains("pressure-finished")
                    {
                        assert_eq!(&*reconstructed, &final_snapshot);
                        caught_up = true;
                        break;
                    }
                }
                SubscriptionEvent::ResyncRequired { .. } => {
                    assert!(sequence >= *expected_sequence);
                    saw_resync = true;
                    break;
                }
                _ => {
                    assert_eq!(sequence, *expected_sequence);
                    *expected_sequence += 1;
                }
            }
        }
        assert!(
            caught_up || saw_resync || disconnected,
            "slow subscriber neither received final state, required resync, nor disconnected"
        );
        drop(producer);
        assert!(final_snapshot.revision > detached.revision);

        let mut before_row_id = final_snapshot
            .scrollback_rows
            .first()
            .and_then(|row| row.row_id)
            .expect("overflow produced paged history");
        let mut paged_ids = std::collections::BTreeSet::new();
        for _ in 0..4 {
            let response = reattached
                .request(Request::ScrollbackPage {
                    splint_id,
                    incarnation,
                    terminal_revision: final_snapshot.revision,
                    history_generation: final_snapshot.history_generation,
                    before_row_id,
                    max_rows: splinterm_protocol::MAX_SCROLLBACK_PAGE_ROWS,
                })
                .await;
            let Response::ScrollbackPage { page, .. } = response else {
                panic!("daemon did not return a scrollback page: {response:?}");
            };
            page.validate().expect("daemon page is valid");
            assert_eq!(
                page.rows.len(),
                splinterm_protocol::MAX_SCROLLBACK_PAGE_ROWS
            );
            assert!(
                page.rows
                    .iter()
                    .all(|row| row.row_id.unwrap() < before_row_id)
            );
            for row_id in page.rows.iter().filter_map(|row| row.row_id) {
                assert!(paged_ids.insert(row_id), "pages must not overlap");
            }
            before_row_id = page.rows.first().and_then(|row| row.row_id).unwrap();
        }
        assert_eq!(
            paged_ids.len(),
            4 * splinterm_protocol::MAX_SCROLLBACK_PAGE_ROWS
        );

        for (revision, generation) in [
            (
                final_snapshot.revision.saturating_sub(1).max(1),
                final_snapshot.history_generation,
            ),
            (
                final_snapshot.revision,
                final_snapshot.history_generation.saturating_add(1),
            ),
        ] {
            assert!(matches!(
                reattached
                    .request(Request::ScrollbackPage {
                        splint_id,
                        incarnation,
                        terminal_revision: revision,
                        history_generation: generation,
                        before_row_id,
                        max_rows: 1,
                    })
                    .await,
                Response::ScrollbackResyncRequired { .. }
            ));
        }

        reattached
            .input(
                splint_id,
                incarnation,
                b"printf 'paging-binding-changed\\n'\n",
            )
            .await;
        let changed = snapshot_until(
            &mut reattached,
            splint_id,
            incarnation,
            "paging-binding-changed",
        )
        .await;
        assert!(changed.revision > final_snapshot.revision);
        assert!(matches!(
            reattached
                .request(Request::ScrollbackPage {
                    splint_id,
                    incarnation,
                    terminal_revision: final_snapshot.revision,
                    history_generation: final_snapshot.history_generation,
                    before_row_id,
                    max_rows: 1,
                })
                .await,
            Response::ScrollbackResyncRequired { .. }
        ));

        match reattached
            .request(Request::KillSplint {
                splint_id,
                incarnation,
            })
            .await
        {
            Response::SplintKilled { exit_status, .. } => {
                assert!(exit_status.code.is_some() || exit_status.signal.is_some());
            }
            response => panic!("unexpected terminate response: {response:?}"),
        }
        daemon.shutdown();
    })
    .await
    .expect("Phase 8 scenario timed out");
}

const AGENT_STATUS_HELPER_RESULTS: &str = "SPLINTERM_E2E_AGENT_STATUS_RESULTS";
const AGENT_STATUS_HELPER_SETSID: &str = "SPLINTERM_E2E_AGENT_STATUS_SETSID";

fn agent_status_policy(reporter: Option<SplintId>) -> String {
    let (executable, sha256) = policy_executable_identity();
    let mut rules = vec![serde_json::json!({
        "id": "agent-status-test",
        "executable": {"path": executable, "sha256": sha256},
        "scopes": [
            "topology_metadata_read",
            "topology_subscribe",
            "process_spawn",
            "topology_layout_mutate",
            "audit_inspect"
        ],
        "resources": [{"kind": "daemon"}],
        "limits": {
            "max_spawn_count": 4,
            "max_results": 64,
            "max_live_subscriptions": 2
        }
    })];
    if let Some(splint_id) = reporter {
        rules.push(serde_json::json!({
            "id": "status-reporter",
            "executable": {"path": executable, "sha256": sha256},
            "scopes": ["agent_status_report", "process_spawn"],
            "resources": [{
                "kind": "splint",
                "splint_id": splint_id,
                "incarnation": "current"
            }],
            "limits": {"max_spawn_count": 2}
        }));
    }
    serde_json::json!({"schema": "splinterm.policy.v2", "rules": rules}).to_string()
}

/// Runs this test binary's in-Splint reporter as the Splint's own process.
/// With `leave_session`, the reporter is a forked child that calls `setsid`,
/// so only the bounded `/proc` ancestry walk can attribute it.
fn agent_status_helper_launch(results: &Path, leave_session: bool) -> LaunchParameters {
    let program = std::env::current_exe().unwrap();
    let script = if leave_session {
        format!(
            "{AGENT_STATUS_HELPER_RESULTS}=\"$1\" {AGENT_STATUS_HELPER_SETSID}=1 \"$0\" \
             --exact agent_status_in_splint_reporter --test-threads=1 -q; exit $?"
        )
    } else {
        format!(
            "{AGENT_STATUS_HELPER_RESULTS}=\"$1\" exec \"$0\" \
             --exact agent_status_in_splint_reporter --test-threads=1 -q"
        )
    };
    LaunchParameters {
        cwd: std::env::current_dir().unwrap(),
        command: vec![
            "/bin/sh".into(),
            "-c".into(),
            script,
            program.to_string_lossy().into_owned(),
            results.to_string_lossy().into_owned(),
        ],
        shell: None,
        login_shell: false,
        scrollback_lines: 100,
    }
}

fn process_group_members(process_group: u32) -> Vec<u32> {
    std::fs::read_dir("/proc")
        .unwrap()
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            // The comm field may contain spaces, so parse after its closing paren.
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|stat| {
                    let fields = stat.rsplit_once(')')?.1.split_whitespace().nth(2)?;
                    fields.parse::<u32>().ok()
                })
                == Some(process_group)
        })
        .collect()
}

/// SIGKILLs a signal-ignoring test process group when a test exits early.
///
/// A timeout or panic drops the test future, and with it this guard; without
/// it, a resistant shell orphaned by the killed daemon would loop forever.
/// Tests disarm the guard once they have proven the group is gone.
struct ProcessGroupGuard {
    group: u32,
    armed: bool,
}

impl ProcessGroupGuard {
    fn new(group: u32) -> Self {
        Self { group, armed: true }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if !self.armed || process_group_members(self.group).is_empty() {
            return;
        }
        if let Some(group) = i32::try_from(self.group)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
    }
}

/// Asserts that a killed resistant process group exits promptly, then
/// disarms its guard.
async fn wait_for_process_group_exit(group: ProcessGroupGuard) {
    let pid = group.group;
    let deadline = Instant::now() + Duration::from_secs(2);
    while !process_group_members(pid).is_empty() {
        assert!(
            Instant::now() < deadline,
            "process group {pid} survived its kill: {:?}",
            process_group_members(pid)
        );
        time::sleep(Duration::from_millis(20)).await;
    }
    group.disarm();
}

// A direct command that ignores hangup, termination and interrupt must still
// be killed with its whole process group, children included.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kill_splint_reaps_a_signal_ignoring_process_group() {
    time::timeout(TEST_TIMEOUT, async {
        let daemon = Daemon::start().await;
        let mut admin = daemon.connect().await;
        let (
            MutationTarget {
                splint_id,
                incarnation,
                ..
            },
            group,
        ) = create_resistant_lair(&daemon, &mut admin, "resistant").await;
        let pid = group.group;
        assert!(!process_group_members(pid).is_empty());

        let Response::SplintKilled { exit_status, .. } = admin
            .request(Request::KillSplint {
                splint_id,
                incarnation,
            })
            .await
        else {
            panic!("daemon did not kill the Splint");
        };
        assert_eq!(exit_status.signal, Some(9));
        wait_for_process_group_exit(group).await;
        daemon.shutdown();
    })
    .await
    .expect("resistant KillSplint scenario timed out");
}

/// Creates a Lair whose shell ignores hangup, termination and interrupt, and
/// returns a guard that kills its process group if the test exits early.
async fn create_resistant_lair(
    daemon: &Daemon,
    admin: &mut Connection,
    name: &str,
) -> (MutationTarget, ProcessGroupGuard) {
    let marker = daemon.runtime.join(format!("{name}-resistant-pid"));
    let expected_topology_revision = admin.topology_revision().await;
    let Response::LairCreated { lair, .. } = admin
        .request(Request::CreateLair {
            expected_topology_revision,
            name: name.into(),
            launch: LaunchParameters {
                cwd: std::env::current_dir().unwrap(),
                command: vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    format!(
                        "trap '' HUP TERM INT; echo $$ > '{}'; while :; do sleep 1; done",
                        marker.display()
                    ),
                ],
                shell: None,
                login_shell: false,
                scrollback_lines: 100,
            },
            generated_name: false,
        })
        .await
    else {
        panic!("daemon did not create the Lair");
    };
    let LayoutNode::Leaf(splint) = &lair.dojos[0].root else {
        panic!("new Dojo was not a leaf");
    };
    // The shell leads its own process group, so its PID names the group.
    let group = ProcessGroupGuard::new(wait_for_pid_marker(&marker, "resistant shell").await);
    let incarnation = admin.live_incarnation(splint.id).await;
    // Let the loop start a child so the group has more than its leader.
    time::sleep(Duration::from_millis(200)).await;
    (
        MutationTarget {
            lair_id: lair.id,
            dojo_id: lair.dojos[0].id,
            splint_id: splint.id,
            incarnation,
        },
        group,
    )
}

// Killing a process that ignores hangup and termination waits through the
// whole shutdown grace. That wait must not hold the topology transaction.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pending_kill_splint_does_not_block_topology_changes() {
    time::timeout(TEST_TIMEOUT, async {
        let daemon = Daemon::start().await;
        let mut admin = daemon.connect().await;
        let (
            MutationTarget {
                splint_id,
                incarnation,
                ..
            },
            group,
        ) = create_resistant_lair(&daemon, &mut admin, "resistant").await;
        let mut kill_client = daemon.connect().await;
        let kill = tokio::spawn(async move {
            kill_client
                .request(Request::KillSplint {
                    splint_id,
                    incarnation,
                })
                .await
        });
        time::sleep(Duration::from_millis(100)).await;
        let expected_topology_revision = admin.topology_revision().await;
        let response = admin
            .request(Request::CreateLair {
                expected_topology_revision,
                name: "unrelated".into(),
                launch: LaunchParameters {
                    cwd: std::env::current_dir().unwrap(),
                    command: vec!["/bin/sh".into()],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 100,
                },
                generated_name: false,
            })
            .await;
        assert!(matches!(response, Response::LairCreated { .. }));
        // The kill escalates through two test graces before SIGKILL.
        assert!(
            !kill.is_finished(),
            "the unrelated change waited for the kill"
        );
        let kill_response = kill.await.unwrap();
        let Response::SplintKilled { exit_status, .. } = kill_response else {
            panic!("daemon did not kill the Splint: {kill_response:?}");
        };
        assert_eq!(exit_status.signal, Some(9));
        let splint = admin
            .topology()
            .await
            .find_splint(splint_id)
            .cloned()
            .expect("killed Splint remains in the topology");
        assert_eq!(splint.state, splinterm_core::SplintState::Exited(137));
        wait_for_process_group_exit(group).await;
        daemon.shutdown();
    })
    .await
    .expect("pending KillSplint scenario timed out");
}

// A Lair termination can race a pending kill of one of its Splints; both must
// succeed and the process is reaped once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminate_lair_during_pending_kill_splint_succeeds() {
    time::timeout(TEST_TIMEOUT, async {
        let daemon = Daemon::start().await;
        let mut admin = daemon.connect().await;
        let (target, group) = create_resistant_lair(&daemon, &mut admin, "resistant").await;
        let mut kill_client = daemon.connect().await;
        let (kill_response, terminated) = tokio::join!(
            kill_client.request(Request::KillSplint {
                splint_id: target.splint_id,
                incarnation: target.incarnation,
            }),
            async {
                time::sleep(Duration::from_millis(100)).await;
                let expected_topology_revision = admin.topology_revision().await;
                admin
                    .request(Request::TerminateLair {
                        expected_topology_revision,
                        lair_id: target.lair_id,
                        targets: vec![target.clone()],
                    })
                    .await
            }
        );
        assert!(
            matches!(kill_response, Response::SplintKilled { .. }),
            "kill failed: {kill_response:?}"
        );
        assert!(
            matches!(terminated, Response::TopologyCommitted { .. }),
            "termination failed: {terminated:?}"
        );
        assert!(
            admin
                .topology()
                .await
                .find_splint(target.splint_id)
                .is_none()
        );
        wait_for_process_group_exit(group).await;
        daemon.shutdown();
    })
    .await
    .expect("Lair termination during KillSplint scenario timed out");
}

/// Writes an executable stand-in for a login shell into the daemon runtime.
fn shell_script(daemon: &Daemon, name: &str, body: &str) -> PathBuf {
    let path = daemon.runtime.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn plain_shell_launch(shell: &Path) -> LaunchParameters {
    LaunchParameters {
        cwd: std::env::current_dir().unwrap(),
        command: Vec::new(),
        shell: Some(shell.display().to_string()),
        login_shell: false,
        scrollback_lines: 100,
    }
}

async fn wait_for_agent_status_results(results: &Path) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(contents) = fs::read(results) {
            return serde_json::from_slice(&contents).unwrap();
        }
        assert!(
            Instant::now() < deadline,
            "in-Splint agent status reporter did not finish"
        );
        time::sleep(Duration::from_millis(20)).await;
    }
}

fn agent_status_report(
    splint_id: SplintId,
    incarnation: u64,
    state: splinterm_protocol::AgentState,
    detail: Option<&str>,
) -> Request {
    Request::ReportAgentStatus {
        splint_id,
        incarnation,
        status: Some(splinterm_protocol::AgentStatusReport {
            state,
            detail: detail.map(str::to_owned),
            ttl_seconds: None,
        }),
    }
}

fn agent_status_outcome(result: Result<Response, ProtocolError>) -> serde_json::Value {
    match result {
        Ok(Response::AgentStatusAccepted {
            sequence,
            expires_at_ms,
        }) => serde_json::json!({
            "accepted": true,
            "sequence": sequence,
            "expires": expires_at_ms.is_some()
        }),
        Ok(response) => serde_json::json!({"unexpected": format!("{response:?}")}),
        Err(error) => serde_json::json!({"accepted": false, "code": error.code}),
    }
}

/// Not a scenario of its own: the agent status scenarios re-execute this test
/// binary inside a Splint, where it reports through its own connection.
#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "the in-Splint reporter's scripted sequence is reviewed as one transcript"
)]
async fn agent_status_in_splint_reporter() {
    use splinterm_protocol::AgentState;

    let Some(results) = std::env::var_os(AGENT_STATUS_HELPER_RESULTS) else {
        return;
    };
    let results = PathBuf::from(results);
    if std::env::var_os(AGENT_STATUS_HELPER_SETSID).is_some() {
        rustix::process::setsid().expect("reporter leaves the Splint session");
    }
    let own_session_leader = rustix::process::getsid(None).unwrap() == rustix::process::getpid();
    // In the setsid variant the parent is the Splint's shell, which still
    // leads the Splint session this reporter has left.
    let parent = rustix::process::getppid().expect("reporter has a parent");
    let parent_leads_other_session = rustix::process::getsid(Some(parent)).ok() == Some(parent)
        && rustix::process::getsid(None).unwrap() != parent;
    let splint_id: SplintId = std::env::var("SPLINTERM_SPLINT_ID")
        .unwrap()
        .parse()
        .unwrap();
    let incarnation: u64 = std::env::var("SPLINTERM_SPLINT_INCARNATION")
        .unwrap()
        .parse()
        .unwrap();
    let socket = PathBuf::from(std::env::var_os("SPLINTERM_SOCKET").unwrap());
    let mut connection = Connection::connect(&socket).await;
    let mut report = async |request| agent_status_outcome(connection.request_result(request).await);
    let working = report(agent_status_report(
        splint_id,
        incarnation,
        AgentState::Working,
        Some("compiling"),
    ))
    .await;
    let refresh = report(agent_status_report(
        splint_id,
        incarnation,
        AgentState::Working,
        Some("compiling"),
    ))
    .await;
    let wrong_incarnation = report(agent_status_report(
        splint_id,
        incarnation + 1,
        AgentState::Done,
        None,
    ))
    .await;
    let invalid_detail = report(agent_status_report(
        splint_id,
        incarnation,
        AgentState::Error,
        Some("two\nlines"),
    ))
    .await;
    let mut burst = Vec::new();
    for index in 0..12 {
        let state = if index % 2 == 0 {
            AgentState::Idle
        } else {
            AgentState::Working
        };
        let outcome = report(agent_status_report(splint_id, incarnation, state, None)).await;
        let limited = outcome["code"] == "rate_limited";
        burst.push(outcome);
        if limited {
            break;
        }
    }
    let mut refused_again = Vec::new();
    for _ in 0..2 {
        refused_again.push(
            report(agent_status_report(
                splint_id,
                incarnation,
                AgentState::Done,
                None,
            ))
            .await,
        );
    }
    time::sleep(Duration::from_millis(1_100)).await;
    let needs_input = report(agent_status_report(
        splint_id,
        incarnation,
        AgentState::NeedsInput,
        Some("waiting for approval"),
    ))
    .await;
    drop(connection);
    let document = serde_json::json!({
        "own_session_leader": own_session_leader,
        "parent_leads_other_session": parent_leads_other_session,
        "working": working,
        "refresh": refresh,
        "wrong_incarnation": wrong_incarnation,
        "invalid_detail": invalid_detail,
        "burst": burst,
        "refused_again": refused_again,
        "needs_input": needs_input,
    });
    let pending = results.with_extension("pending");
    fs::write(&pending, serde_json::to_vec(&document).unwrap()).unwrap();
    fs::rename(&pending, &results).unwrap();
    let release = results.with_extension("release");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !release.exists() && Instant::now() < deadline {
        time::sleep(Duration::from_millis(20)).await;
    }
}

fn assert_in_splint_reporter_results(document: &serde_json::Value) {
    assert_eq!(document["working"]["accepted"], true, "{document}");
    assert_eq!(document["working"]["expires"], true, "active states expire");
    assert_eq!(document["refresh"]["accepted"], true);
    assert_eq!(
        document["refresh"]["sequence"], document["working"]["sequence"],
        "an identical report refreshes without a new sequence"
    );
    assert_eq!(document["wrong_incarnation"]["code"], "stale_incarnation");
    assert_eq!(document["invalid_detail"]["code"], "invalid_argument");
    let burst = document["burst"].as_array().unwrap();
    // Two reports were admitted before the burst; the eleventh is refused.
    assert_eq!(burst.len(), 9, "{document}");
    assert!(burst[..8].iter().all(|outcome| outcome["accepted"] == true));
    assert_eq!(burst[8]["code"], "rate_limited");
    assert!(
        document["refused_again"]
            .as_array()
            .unwrap()
            .iter()
            .all(|outcome| outcome["code"] == "rate_limited")
    );
    assert_eq!(document["needs_input"]["accepted"], true, "{document}");
}

async fn audit_records(
    connection: &mut Connection,
    operation: splinterm_protocol::AuditOperation,
) -> Vec<splinterm_protocol::AuditRecord> {
    let Response::AuditPage { page } = connection
        .request(Request::AuditInspect {
            after_audit_id: None,
            max_records: 64,
        })
        .await
    else {
        panic!("audit page was not returned");
    };
    page.records
        .into_iter()
        .filter(|record| record.operation == operation)
        .collect()
}

async fn splint_runtime(
    connection: &mut Connection,
    splint_id: SplintId,
) -> (TopologyRevision, splinterm_protocol::SplintRuntimeSummary) {
    let Response::Topology { snapshot } = connection.request(Request::InspectTopology).await else {
        panic!("topology was not returned");
    };
    snapshot.validate().unwrap();
    let runtime = snapshot
        .runtimes
        .into_iter()
        .find(|runtime| runtime.splint_id == splint_id)
        .expect("Splint runtime summary");
    (snapshot.revision, runtime)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one real in-Splint reporter proves authority, delivery, audit, and exit clearing"
)]
async fn agent_status_self_report_from_inside_the_splint() {
    use splinterm_protocol::{AgentState, AuditDecision, AuditOperation};

    time::timeout(Duration::from_secs(90), async {
        let daemon = Daemon::start_with_policy(&agent_status_policy(None)).await;
        let results = daemon.runtime.join("agent-status-self.json");
        let mut subscriber = daemon.connect().await;
        let Response::TopologySubscribed {
            subscription_id, ..
        } = subscriber.request(Request::SubscribeTopology).await
        else {
            panic!("topology subscription was not created");
        };
        let mut client = daemon.connect().await;
        let revision = client.topology_revision().await;
        let Response::LairCreated {
            lair,
            incarnation,
            topology_revision: created,
        } = client
            .request(Request::CreateLair {
                expected_topology_revision: revision,
                name: "agent-status".into(),
                launch: agent_status_helper_launch(&results, false),
                generated_name: false,
            })
            .await
        else {
            panic!("agent status Lair was not created");
        };
        let LayoutNode::Leaf(splint) = &lair.dojos[0].root else {
            panic!("agent status Lair was not a leaf");
        };
        let splint_id = splint.id;

        let document = wait_for_agent_status_results(&results).await;
        assert_in_splint_reporter_results(&document);
        assert_eq!(
            document["own_session_leader"], true,
            "the reporter is the Splint's session leader"
        );

        let (current_revision, runtime) = splint_runtime(&mut client, splint_id).await;
        assert_eq!(
            current_revision, created,
            "agent status reports never bump the topology revision"
        );
        let status = runtime.agent_status.expect("summary carries agent status");
        assert_eq!(status.state, AgentState::NeedsInput);
        assert_eq!(status.detail.as_deref(), Some("waiting for approval"));
        assert!(!status.stale);
        assert_eq!(
            serde_json::json!(status.sequence),
            document["needs_input"]["sequence"]
        );

        // Status events ride the topology subscription without a snapshot,
        // coalesced to fewer events than accepted visible changes.
        let mut status_events = 0;
        let mut last_sequence = 0;
        loop {
            let (_, event) = subscriber.next_event(subscription_id).await;
            match event {
                SubscriptionEvent::TopologyChanged { change } => {
                    assert_eq!(
                        change.revision, created,
                        "no topology change follows the creation"
                    );
                    assert_eq!(status_events, 0, "no snapshot is published for a report");
                }
                SubscriptionEvent::AgentStatusChanged {
                    splint_id: event_splint,
                    incarnation: event_incarnation,
                    status,
                    sequence,
                } => {
                    assert_eq!(event_splint, splint_id);
                    assert_eq!(event_incarnation, incarnation);
                    assert!(sequence > last_sequence);
                    last_sequence = sequence;
                    status_events += 1;
                    let status = status.expect("reports set a status");
                    assert!(status.validate());
                    if status.state == AgentState::NeedsInput {
                        assert_eq!(status.sequence, sequence);
                        break;
                    }
                }
                event => panic!("unexpected topology subscription event: {event:?}"),
            }
        }
        // One working change, eight burst changes, and the final change.
        assert!(
            (2..10).contains(&status_events),
            "expected coalesced status events, saw {status_events}"
        );

        let outside = client
            .request_result(agent_status_report(
                splint_id,
                incarnation,
                AgentState::Done,
                None,
            ))
            .await
            .expect_err("a process outside the Splint needs a grant");
        assert_eq!(outside.code, ErrorCode::Unauthorized);
        let records = audit_records(&mut client, AuditOperation::AgentStatusReport).await;
        assert_eq!(
            records.len(),
            2,
            "self-reports are not audited per update: {records:?}"
        );
        assert!(records.iter().all(|record| {
            record
                .resource
                .as_ref()
                .and_then(|resource| resource.splint_id)
                == Some(splint_id)
                && record.outcome == Some(splinterm_protocol::AuditOutcome::Failed)
        }));
        assert_eq!(records[0].decision, AuditDecision::Rejected);
        assert_eq!(records[0].reason, "self_report_rate_limited");
        assert_eq!(records[1].decision, AuditDecision::Denied);
        assert_eq!(
            records[1].requested_scopes,
            vec![splinterm_protocol::AutomationScope::AgentStatusReport]
        );

        fs::write(results.with_extension("release"), b"").unwrap();
        loop {
            let (_, event) = subscriber.next_event(subscription_id).await;
            if let SubscriptionEvent::TopologyChanged { change } = event {
                change.validate().unwrap();
                let runtime = change
                    .snapshot
                    .runtimes
                    .iter()
                    .find(|runtime| runtime.splint_id == splint_id)
                    .unwrap();
                if runtime.lifecycle == SplintLifecycle::Exited {
                    assert_eq!(
                        runtime.agent_status, None,
                        "the exit snapshot drops the incarnation's status"
                    );
                    break;
                }
            }
        }
        let (_, runtime) = splint_runtime(&mut client, splint_id).await;
        assert_eq!(runtime.agent_status, None);
        daemon.shutdown();
    })
    .await
    .expect("agent status self-report scenario timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one daemon proves ancestry attribution, granted reports, and relaunch binding"
)]
async fn agent_status_ancestry_grant_and_relaunch_binding() {
    use splinterm_protocol::{AgentState, AuditDecision, AuditOperation};

    time::timeout(Duration::from_secs(90), async {
        let daemon = Daemon::start_with_policy(&agent_status_policy(None)).await;
        let results = daemon.runtime.join("agent-status-ancestry.json");
        let mut client = daemon.connect().await;
        let revision = client.topology_revision().await;
        let Response::LairCreated {
            lair, incarnation, ..
        } = client
            .request(Request::CreateLair {
                expected_topology_revision: revision,
                name: "agent-status-ancestry".into(),
                launch: agent_status_helper_launch(&results, true),
                generated_name: false,
            })
            .await
        else {
            panic!("agent status Lair was not created");
        };
        let LayoutNode::Leaf(splint) = &lair.dojos[0].root else {
            panic!("agent status Lair was not a leaf");
        };
        let splint_id = splint.id;
        let document = wait_for_agent_status_results(&results).await;
        assert_eq!(
            document["parent_leads_other_session"], true,
            "the reporter left the Splint session led by its parent shell"
        );
        assert_in_splint_reporter_results(&document);

        // A policy grant lets a process outside the Splint report for it.
        let policy = daemon.policy.as_ref().unwrap();
        fs::write(policy, agent_status_policy(Some(splint_id))).unwrap();
        fs::set_permissions(policy, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.reload_policy();
        assert_connection_closed(&mut client, "agent status policy reload").await;
        let mut client = daemon.connect().await;
        let Response::AgentStatusAccepted {
            sequence,
            expires_at_ms,
        } = client
            .request(agent_status_report(
                splint_id,
                incarnation,
                AgentState::Done,
                Some("merged"),
            ))
            .await
        else {
            panic!("granted report was not accepted");
        };
        assert_eq!(expires_at_ms, None, "resting states are untimed");
        let (_, runtime) = splint_runtime(&mut client, splint_id).await;
        let status = runtime.agent_status.expect("granted status is visible");
        assert_eq!(status.state, AgentState::Done);
        assert_eq!(status.detail.as_deref(), Some("merged"));
        assert_eq!(status.sequence, sequence);
        let stale = client
            .request_result(agent_status_report(
                splint_id,
                incarnation + 1,
                AgentState::Done,
                None,
            ))
            .await
            .expect_err("a granted report for another incarnation is stale");
        assert_eq!(stale.code, ErrorCode::StaleIncarnation);
        let records = audit_records(&mut client, AuditOperation::AgentStatusReport).await;
        let granted = records
            .iter()
            .find(|record| record.decision == AuditDecision::Matched)
            .expect("granted reports are audited");
        assert_eq!(granted.policy_rule_id.as_deref(), Some("status-reporter"));
        assert_eq!(
            granted.outcome,
            Some(splinterm_protocol::AuditOutcome::Succeeded)
        );
        assert!(
            records
                .iter()
                .all(|record| record.argument_count.is_none()
                    && record.executable_basename.is_none()),
            "audit records carry outcome metadata only"
        );

        // Exit clears the status; a relaunch starts a new incarnation without one.
        fs::write(results.with_extension("release"), b"").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let exited_revision = loop {
            let (revision, runtime) = splint_runtime(&mut client, splint_id).await;
            if runtime.live_incarnation.is_none() {
                assert_eq!(runtime.agent_status, None);
                break revision;
            }
            assert!(Instant::now() < deadline, "reporter Splint did not exit");
            time::sleep(Duration::from_millis(20)).await;
        };
        let exited = client
            .request_result(agent_status_report(
                splint_id,
                incarnation,
                AgentState::Idle,
                None,
            ))
            .await
            .expect_err("an exited incarnation cannot report");
        assert_eq!(exited.code, ErrorCode::StaleIncarnation);
        let Response::SplintStarted {
            incarnation: relaunched,
            ..
        } = client
            .request(Request::RelaunchSplint {
                expected_topology_revision: exited_revision,
                splint_id,
                launch: LaunchParameters {
                    cwd: std::env::current_dir().unwrap(),
                    command: vec!["/bin/sh".into(), "-c".into(), "exec sleep 30".into()],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 100,
                },
            })
            .await
        else {
            panic!("reporter Splint was not relaunched");
        };
        assert!(relaunched > incarnation);
        let (_, runtime) = splint_runtime(&mut client, splint_id).await;
        assert_eq!(runtime.live_incarnation, Some(relaunched));
        assert_eq!(
            runtime.agent_status, None,
            "a relaunch starts without status"
        );
        assert!(matches!(
            client
                .request(agent_status_report(
                    splint_id,
                    relaunched,
                    AgentState::Working,
                    None,
                ))
                .await,
            Response::AgentStatusAccepted {
                expires_at_ms: Some(_),
                ..
            }
        ));
        let clear = client
            .request(Request::ReportAgentStatus {
                splint_id,
                incarnation: relaunched,
                status: None,
            })
            .await;
        assert!(matches!(clear, Response::AgentStatusAccepted { .. }));
        let (_, runtime) = splint_runtime(&mut client, splint_id).await;
        assert_eq!(runtime.agent_status, None, "a clear removes the status");
        daemon.shutdown();
    })
    .await
    .expect("agent status ancestry and grant scenario timed out");
}

const AGENT_STATUS_MCP_RESULTS: &str = "SPLINTERM_E2E_AGENT_STATUS_MCP_RESULTS";
const AGENT_STATUS_MCP_OTHER_SPLINT: &str = "SPLINTERM_E2E_AGENT_STATUS_OTHER_SPLINT";
const AGENT_STATUS_MCP_OTHER_INCARNATION: &str = "SPLINTERM_E2E_AGENT_STATUS_OTHER_INCARNATION";

async fn wait_for_signal_file(path: &Path, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !path.exists() {
        assert!(Instant::now() < deadline, "{label} was not signalled");
        time::sleep(Duration::from_millis(20)).await;
    }
}

async fn create_lair_with(admin: &mut Connection, launch: LaunchParameters) -> MutationTarget {
    let name = format!("unused {}", admin.topology().await.lairs().count());
    create_named_lair(admin, name, false, launch).await
}

async fn create_named_lair(
    admin: &mut Connection,
    name: String,
    generated_name: bool,
    launch: LaunchParameters,
) -> MutationTarget {
    let expected_topology_revision = admin.topology_revision().await;
    let Response::LairCreated { lair, .. } = admin
        .request(Request::CreateLair {
            expected_topology_revision,
            name,
            launch,
            generated_name,
        })
        .await
    else {
        panic!("daemon did not create the Lair");
    };
    let LayoutNode::Leaf(splint) = &lair.dojos[0].root else {
        panic!("new Dojo was not a leaf");
    };
    let incarnation = admin.live_incarnation(splint.id).await;
    MutationTarget {
        lair_id: lair.id,
        dojo_id: lair.dojos[0].id,
        splint_id: splint.id,
        incarnation,
    }
}

async fn close_unused(connection: &mut Connection, dojo_id: DojoId) -> UnusedDojoCloseOutcome {
    match connection
        .request(Request::CloseUnusedDojo { dojo_id })
        .await
    {
        Response::UnusedDojoClose {
            dojo_id: closed,
            outcome,
        } if closed == dojo_id => outcome,
        response => panic!("unexpected unused Dojo close response: {response:?}"),
    }
}

async fn assert_dojo_used(connection: &mut Connection, dojo_id: DojoId, reason: DojoUse) {
    assert_eq!(
        close_unused(connection, dojo_id).await,
        UnusedDojoCloseOutcome::Used { reason }
    );
    assert!(
        connection.topology().await.find_dojo(dojo_id).is_some(),
        "a used Dojo must stay"
    );
}

async fn wait_for_pid_file(path: &Path) -> u32 {
    loop {
        if let Some(pid) = fs::read_to_string(path)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
        {
            return pid;
        }
        time::sleep(Duration::from_millis(20)).await;
    }
}

fn write_results_atomically(path: &Path, document: &serde_json::Value) {
    let pending = path.with_extension("pending");
    fs::write(&pending, serde_json::to_vec(document).unwrap()).unwrap();
    fs::rename(&pending, path).unwrap();
}

/// Not a scenario of its own: the MCP agent status scenario re-executes this
/// test binary inside a Splint, where the MCP adapter projection fills its
/// target from the inherited Splint environment and self-reports.
#[tokio::test(flavor = "current_thread")]
async fn agent_status_mcp_in_splint_reporter() {
    let Some(results) = std::env::var_os(AGENT_STATUS_MCP_RESULTS) else {
        return;
    };
    let results = PathBuf::from(results);
    let socket = PathBuf::from(std::env::var_os("SPLINTERM_SOCKET").unwrap());
    let outcome = |result: anyhow::Result<serde_json::Value>| match result {
        Ok(value) => value,
        Err(error) => serde_json::json!({"error": error.to_string()}),
    };
    let reported = outcome(
        splinterm_mcp::dispatch_mutation_for_integration_test(
            "splinterm.report_agent_status",
            &serde_json::json!({"state": "needs_input", "detail": "waiting for approval"}),
            &socket,
        )
        .await,
    );
    // Self-report authority covers only the reporter's own Splint: another
    // live Splint in the same daemon needs a grant this policy lacks.
    let other_splint = std::env::var(AGENT_STATUS_MCP_OTHER_SPLINT).unwrap();
    let other_incarnation: u64 = std::env::var(AGENT_STATUS_MCP_OTHER_INCARNATION)
        .unwrap()
        .parse()
        .unwrap();
    let elsewhere = outcome(
        splinterm_mcp::dispatch_mutation_for_integration_test(
            "splinterm.report_agent_status",
            &serde_json::json!({
                "state": "done",
                "splint_id": other_splint,
                "incarnation": other_incarnation
            }),
            &socket,
        )
        .await,
    );
    write_results_atomically(
        &results,
        &serde_json::json!({
            "environment_splint_id": std::env::var("SPLINTERM_SPLINT_ID").unwrap(),
            "environment_incarnation": std::env::var("SPLINTERM_SPLINT_INCARNATION").unwrap(),
            "reported": reported,
            "elsewhere": elsewhere,
        }),
    );
    wait_for_signal_file(&results.with_extension("clear"), "MCP clear").await;
    let cleared = outcome(
        splinterm_mcp::dispatch_mutation_for_integration_test(
            "splinterm.report_agent_status",
            &serde_json::json!({"state": null}),
            &socket,
        )
        .await,
    );
    write_results_atomically(
        &results.with_extension("cleared"),
        &serde_json::json!({"cleared": cleared}),
    );
    wait_for_signal_file(&results.with_extension("release"), "MCP reporter release").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one real in-Splint MCP reporter proves env defaults, authority, projection, and clear"
)]
async fn agent_status_mcp_adapter_reports_from_inside_the_splint() {
    use splinterm_protocol::AgentState;

    time::timeout(Duration::from_secs(90), async {
        // No policy rule grants agent_status_report: authority is self-report.
        let daemon = Daemon::start_with_policy(&agent_status_policy(None)).await;
        let results = daemon.runtime.join("agent-status-mcp.json");
        let program = std::env::current_exe().unwrap();
        let mut client = daemon.connect().await;
        let revision = client.topology_revision().await;
        // A second live Splint the in-Splint reporter must not be able to target.
        let Response::LairCreated {
            lair: other_lair,
            incarnation: other_incarnation,
            topology_revision: revision,
        } = client
            .request(Request::CreateLair {
                expected_topology_revision: revision,
                name: "agent-status-mcp-other".into(),
                launch: LaunchParameters {
                    cwd: std::env::current_dir().unwrap(),
                    command: vec!["/bin/sh".into(), "-c".into(), "exec sleep 60".into()],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 100,
                },
                generated_name: false,
            })
            .await
        else {
            panic!("second agent status Lair was not created");
        };
        let LayoutNode::Leaf(other_splint) = &other_lair.dojos[0].root else {
            panic!("second agent status Lair was not a leaf");
        };
        let other_splint_id = other_splint.id;
        let Response::LairCreated {
            lair,
            incarnation,
            topology_revision: created,
        } = client
            .request(Request::CreateLair {
                expected_topology_revision: revision,
                name: "agent-status-mcp".into(),
                launch: LaunchParameters {
                    cwd: std::env::current_dir().unwrap(),
                    command: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        format!(
                            "{AGENT_STATUS_MCP_RESULTS}=\"$1\" \
                             {AGENT_STATUS_MCP_OTHER_SPLINT}=\"$2\" \
                             {AGENT_STATUS_MCP_OTHER_INCARNATION}=\"$3\" exec \"$0\" \
                             --exact agent_status_mcp_in_splint_reporter --test-threads=1 -q"
                        ),
                        program.to_string_lossy().into_owned(),
                        results.to_string_lossy().into_owned(),
                        other_splint_id.to_string(),
                        other_incarnation.to_string(),
                    ],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 100,
                },
                generated_name: false,
            })
            .await
        else {
            panic!("MCP agent status Lair was not created");
        };
        let LayoutNode::Leaf(splint) = &lair.dojos[0].root else {
            panic!("MCP agent status Lair was not a leaf");
        };
        let splint_id = splint.id;
        let document = wait_for_agent_status_results(&results).await;
        assert_eq!(document["environment_splint_id"], splint_id.to_string());
        assert_eq!(document["environment_incarnation"], incarnation.to_string());
        let reported = &document["reported"];
        assert_eq!(
            reported["tool"], "splinterm.report_agent_status",
            "{document}"
        );
        assert_eq!(
            reported["resource"],
            serde_json::json!({
                "kind": "agent_status",
                "splint_id": splint_id.to_string(),
                "incarnation": incarnation
            })
        );
        assert_eq!(reported["data"]["committed"], true);
        assert_eq!(reported["data"]["state"], "needs_input");
        assert!(reported["data"]["expires_at_ms"].as_u64().is_some());
        assert!(!reported.to_string().contains("waiting for approval"));
        assert_eq!(
            document["elsewhere"]["error"], "the exact policy scope or resource is not authorized",
            "a Splint process cannot report for another live Splint without a grant: {document}"
        );

        let Response::Topology { snapshot } = client.request(Request::InspectTopology).await else {
            panic!("topology was not returned");
        };
        assert_eq!(
            snapshot.revision, created,
            "reports never bump the revision"
        );
        let runtime = snapshot
            .runtimes
            .iter()
            .find(|runtime| runtime.splint_id == splint_id)
            .unwrap();
        let status = runtime
            .agent_status
            .as_ref()
            .expect("MCP status is visible");
        assert_eq!(status.state, AgentState::NeedsInput);
        assert_eq!(status.detail.as_deref(), Some("waiting for approval"));
        let other_runtime = snapshot
            .runtimes
            .iter()
            .find(|runtime| runtime.splint_id == other_splint_id)
            .unwrap();
        assert_eq!(other_runtime.live_incarnation, Some(other_incarnation));
        assert_eq!(
            other_runtime.agent_status, None,
            "the refused report left no status"
        );
        // The CLI v2 projections carry the same status.
        let inspected = serde_json::to_value(
            splinterm_automation_client::inspect_splint_envelope(&snapshot, splint_id).unwrap(),
        )
        .unwrap();
        assert_eq!(inspected["data"]["agent_status"]["state"], "needs_input");
        assert_eq!(
            inspected["data"]["agent_status"]["detail"],
            "waiting for approval"
        );
        let get = serde_json::to_value(
            splinterm_automation_client::agent_status_get_envelope(&snapshot, splint_id).unwrap(),
        )
        .unwrap();
        assert_eq!(get["data"]["current_incarnation"], incarnation);
        assert_eq!(get["data"]["agent_status"]["stale"], false);

        fs::write(results.with_extension("clear"), b"").unwrap();
        let cleared = wait_for_agent_status_results(&results.with_extension("cleared")).await;
        assert_eq!(
            cleared["cleared"]["data"]["state"],
            serde_json::Value::Null,
            "{cleared}"
        );
        assert_eq!(cleared["cleared"]["data"]["committed"], true);
        let (_, runtime) = splint_runtime(&mut client, splint_id).await;
        assert_eq!(runtime.live_incarnation, Some(incarnation));
        assert_eq!(
            runtime.agent_status, None,
            "the MCP clear removed the status"
        );
        fs::write(results.with_extension("release"), b"").unwrap();
        daemon.shutdown();
    })
    .await
    .expect("MCP agent status scenario timed out");
}

fn foreground_group(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(5)?
        .parse()
        .ok()
}

async fn wait_for_exit(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Path::new(&format!("/proc/{pid}")).exists() {
        assert!(Instant::now() < deadline, "process {pid} was never reaped");
        time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unused_dojo_closes_and_its_shell_is_reaped() {
    time::timeout(TEST_TIMEOUT, async {
        let daemon = Daemon::start().await;
        let pid_file = daemon.runtime.join("shell.pid");
        let shell = shell_script(
            &daemon,
            "pid-shell",
            &format!("echo $$ > '{}'\nexec /bin/sh\n", pid_file.display()),
        );
        let mut admin = daemon.connect().await;
        let target = create_lair_with(&mut admin, plain_shell_launch(&shell)).await;
        let pid = wait_for_pid_file(&pid_file).await;
        // Focus reports the client generates leave it unused.
        admin
            .focus_report(target.splint_id, target.incarnation, b"\x1b[I\x1b[O")
            .await;
        admin.release_control().await;
        let UnusedDojoCloseOutcome::Closed {
            topology_revision,
            lair_removed,
        } = close_unused(&mut admin, target.dojo_id).await
        else {
            panic!("an unused Dojo was kept");
        };
        // The Lair has a name the user chose, so it stays, empty.
        assert!(!lair_removed);
        let topology = admin.topology().await;
        assert_eq!(topology.revision(), topology_revision);
        assert!(topology.find_dojo(target.dojo_id).is_none());
        assert!(topology.find_splint(target.splint_id).is_none());
        let lair = topology.find_lair(target.lair_id).expect("its Lair stays");
        assert!(lair.dojos.is_empty());
        wait_for_exit(pid).await;
        // A second request finds nothing to close.
        let error = admin
            .request_result(Request::CloseUnusedDojo {
                dojo_id: target.dojo_id,
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound);
        daemon.shutdown();
    })
    .await
    .expect("unused Dojo close scenario timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_split_dojo_counts_as_used() {
    time::timeout(TEST_TIMEOUT, async {
        let daemon = Daemon::start().await;
        let shell = PathBuf::from("/bin/sh");
        let mut admin = daemon.connect().await;

        let split = create_lair_with(&mut admin, plain_shell_launch(&shell)).await;
        let expected_topology_revision = admin.topology_revision().await;
        let Response::SplintStarted {
            splint_id: added, ..
        } = admin
            .request(Request::SplitSplint {
                expected_topology_revision,
                target_splint_id: split.splint_id,
                axis: Axis::Horizontal,
                side: SplitSide::Second,
                ratio: SplitRatio::new(500).unwrap(),
                launch: plain_shell_launch(&shell),
            })
            .await
        else {
            panic!("daemon did not split the Splint");
        };
        assert_dojo_used(&mut admin, split.dojo_id, DojoUse::Split).await;
        // Closing the original leaves a single Splint, but not the original.
        let killed = admin
            .request(Request::KillSplint {
                splint_id: split.splint_id,
                incarnation: split.incarnation,
            })
            .await;
        assert!(matches!(killed, Response::SplintKilled { .. }));
        let expected_topology_revision = admin.topology_revision().await;
        admin
            .request(Request::CloseSplint {
                expected_topology_revision,
                splint_id: split.splint_id,
            })
            .await;
        assert!(admin.topology().await.find_splint(added).is_some());
        assert_dojo_used(&mut admin, split.dojo_id, DojoUse::Split).await;

        // A split stays a user action after its extra pane closes again.
        let reverted = create_lair_with(&mut admin, plain_shell_launch(&shell)).await;
        let expected_topology_revision = admin.topology_revision().await;
        let Response::SplintStarted {
            splint_id: extra,
            incarnation: extra_incarnation,
            ..
        } = admin
            .request(Request::SplitSplint {
                expected_topology_revision,
                target_splint_id: reverted.splint_id,
                axis: Axis::Vertical,
                side: SplitSide::Second,
                ratio: SplitRatio::new(500).unwrap(),
                launch: plain_shell_launch(&shell),
            })
            .await
        else {
            panic!("daemon did not split the Splint");
        };
        admin
            .request(Request::KillSplint {
                splint_id: extra,
                incarnation: extra_incarnation,
            })
            .await;
        let expected_topology_revision = admin.topology_revision().await;
        admin
            .request(Request::CloseSplint {
                expected_topology_revision,
                splint_id: extra,
            })
            .await;
        let topology = admin.topology().await;
        let dojo = topology.find_dojo(reverted.dojo_id).unwrap();
        assert!(matches!(&dojo.root, LayoutNode::Leaf(splint) if splint.id == reverted.splint_id));
        assert_dojo_used(&mut admin, reverted.dojo_id, DojoUse::Split).await;
        daemon.shutdown();
    })
    .await
    .expect("split Dojo scenario timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dojos_count_as_used_after_input_or_rename() {
    time::timeout(TEST_TIMEOUT, async {
        let daemon = Daemon::start().await;
        let shell = PathBuf::from("/bin/sh");
        let mut admin = daemon.connect().await;
        let typed = create_lair_with(&mut admin, plain_shell_launch(&shell)).await;
        admin.input(typed.splint_id, typed.incarnation, b"x").await;
        admin.release_control().await;
        assert_dojo_used(&mut admin, typed.dojo_id, DojoUse::Input).await;

        // Focus-shaped bytes the client did not flag, from automation or an
        // unbracketed paste, reached the shell and count as input.
        let pasted = create_lair_with(&mut admin, plain_shell_launch(&shell)).await;
        admin
            .input(pasted.splint_id, pasted.incarnation, b"\x1b[I\x1b[O")
            .await;
        admin.release_control().await;
        assert_dojo_used(&mut admin, pasted.dojo_id, DojoUse::Input).await;

        let renamed = create_lair_with(&mut admin, plain_shell_launch(&shell)).await;
        let expected_topology_revision = admin.topology_revision().await;
        admin
            .request(Request::RenameDojo {
                expected_topology_revision,
                dojo_id: renamed.dojo_id,
                name: "kept".into(),
                promote_transient_lair: false,
            })
            .await;
        assert_dojo_used(&mut admin, renamed.dojo_id, DojoUse::Renamed).await;

        let pane_renamed = create_lair_with(&mut admin, plain_shell_launch(&shell)).await;
        let expected_topology_revision = admin.topology_revision().await;
        admin
            .request(Request::RenameSplint {
                expected_topology_revision,
                splint_id: pane_renamed.splint_id,
                title: "kept".into(),
            })
            .await;
        assert_dojo_used(&mut admin, pane_renamed.dojo_id, DojoUse::Renamed).await;
        daemon.shutdown();
    })
    .await
    .expect("used Dojo scenario timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_plain_default_shell_dojos_can_be_unused() {
    time::timeout(TEST_TIMEOUT, async {
        let daemon = Daemon::start().await;
        let shell = PathBuf::from("/bin/sh");
        let mut admin = daemon.connect().await;

        let mut command_bearing = plain_shell_launch(&shell);
        command_bearing.command = vec!["/bin/sh".into()];
        let command = create_lair_with(&mut admin, command_bearing).await;
        assert_dojo_used(&mut admin, command.dojo_id, DojoUse::Launch).await;

        // A Window's new Dojo whose name Splinterm generated qualifies; a name
        // the user chose is use, even one equal to the default label. An
        // agent's new Dojo never qualifies (Plan 0067 M3).
        let host = create_lair_with(&mut admin, plain_shell_launch(&shell)).await;
        let (relay, multiplexer) = start_graphical_relay(&daemon).await;
        let mut window = RelayedConnection::open(&multiplexer).await;
        for (name, generated_name, from_window, used) in [
            ("Dojo 2", true, true, None),
            ("Dojo 2", false, true, Some(DojoUse::Renamed)),
            ("chosen", false, true, Some(DojoUse::Renamed)),
            ("Dojo 2", true, false, Some(DojoUse::Launch)),
        ] {
            let expected_topology_revision = admin.topology_revision().await;
            let request = Request::NewDojo {
                expected_topology_revision,
                lair_id: host.lair_id,
                name: name.into(),
                launch: plain_shell_launch(&shell),
                promote_transient_lair: false,
                generated_name,
            };
            let response = if from_window {
                window.request(request).await.unwrap()
            } else {
                admin.request(request).await
            };
            let Response::DojoStarted { dojo_id, .. } = response else {
                panic!("daemon did not create the Dojo");
            };
            match used {
                Some(reason) => assert_dojo_used(&mut admin, dojo_id, reason).await,
                None => assert!(matches!(
                    close_unused(&mut admin, dojo_id).await,
                    UnusedDojoCloseOutcome::Closed { .. }
                )),
            }
        }

        // A relaunched Splint is no longer the original incarnation.
        let relaunched = create_lair_with(&mut admin, plain_shell_launch(&shell)).await;
        admin
            .request(Request::KillSplint {
                splint_id: relaunched.splint_id,
                incarnation: relaunched.incarnation,
            })
            .await;
        assert_dojo_used(&mut admin, relaunched.dojo_id, DojoUse::NotRunning).await;
        let expected_topology_revision = admin.topology_revision().await;
        admin
            .request(Request::RelaunchSplint {
                expected_topology_revision,
                splint_id: relaunched.splint_id,
                launch: plain_shell_launch(&shell),
            })
            .await;
        assert_dojo_used(&mut admin, relaunched.dojo_id, DojoUse::Launch).await;
        drop(window);
        drop(multiplexer);
        drop(relay);
        daemon.shutdown();
    })
    .await
    .expect("plain launch scenario timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attached_window_keeps_the_dojo_until_it_detaches() {
    time::timeout(TEST_TIMEOUT, async {
        let daemon = Daemon::start().await;
        let mut admin = daemon.connect().await;
        let target = create_lair_with(&mut admin, plain_shell_launch(Path::new("/bin/sh"))).await;
        let mut window = daemon.connect().await;
        let (subscription_id, _) = window.attach(target.splint_id, target.incarnation).await;
        assert_dojo_used(&mut admin, target.dojo_id, DojoUse::Attached).await;
        assert_eq!(
            window.request(Request::Detach { subscription_id }).await,
            Response::Acknowledged
        );
        // An explicit detach is released before its acknowledgement.
        assert!(matches!(
            close_unused(&mut admin, target.dojo_id).await,
            UnusedDojoCloseOutcome::Closed { .. }
        ));
        daemon.shutdown();
    })
    .await
    .expect("attached Window scenario timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_program_in_the_foreground_counts_as_use() {
    time::timeout(TEST_TIMEOUT, async {
        let daemon = Daemon::start().await;
        let pid_file = daemon.runtime.join("startup.pid");
        // Job control gives the startup program its own foreground group,
        // as an interactive shell does for a program run from its rc file.
        let shell = shell_script(
            &daemon,
            "startup-shell",
            &format!(
                "echo $$ > '{}'\nset -m\nsleep 30\nexec /bin/sh\n",
                pid_file.display()
            ),
        );
        let mut admin = daemon.connect().await;
        let target = create_lair_with(&mut admin, plain_shell_launch(&shell)).await;
        let pid = wait_for_pid_file(&pid_file).await;
        let deadline = Instant::now() + Duration::from_secs(5);
        while foreground_group(pid).is_none_or(|group| group == pid) {
            assert!(
                Instant::now() < deadline,
                "the startup program never took the foreground"
            );
            time::sleep(Duration::from_millis(20)).await;
        }
        assert_dojo_used(&mut admin, target.dojo_id, DojoUse::Foreground).await;
        daemon.shutdown();
    })
    .await
    .expect("foreground program scenario timed out");
}

enum InputOutcome {
    Delivered,
    Rejected,
    Disconnected,
}

/// Sends input, tolerating the controller revocation that ends the
/// connection when the Dojo closes first.
async fn racing_input(
    connection: &mut Connection,
    target: &MutationTarget,
    controller_id: u64,
) -> InputOutcome {
    let request_id = connection.request_id;
    connection.request_id += 1;
    let frame = ClientFrame::Request {
        request_id,
        diagnostic_correlation: None,
        request: Request::Input {
            controller_id,
            splint_id: target.splint_id,
            incarnation: target.incarnation,
            bytes: b"x".to_vec(),
            focus_report: false,
        },
    };
    if connection
        .stream
        .write_all(&encode_frame(&frame).unwrap())
        .await
        .is_err()
    {
        return InputOutcome::Disconnected;
    }
    loop {
        let mut length = [0_u8; 4];
        if connection.stream.read_exact(&mut length).await.is_err() {
            return InputOutcome::Disconnected;
        }
        let mut body = vec![0_u8; u32::from_be_bytes(length) as usize];
        if connection.stream.read_exact(&mut body).await.is_err() {
            return InputOutcome::Disconnected;
        }
        match serde_json::from_slice::<ServerFrame>(&body).unwrap() {
            ServerFrame::Response {
                request_id: id,
                result: Response::TerminalActionAcknowledged { .. },
            } if id == request_id => return InputOutcome::Delivered,
            ServerFrame::Error {
                request_id: Some(id),
                ..
            } if id == request_id => return InputOutcome::Rejected,
            ServerFrame::Error {
                request_id: None, ..
            } => return InputOutcome::Disconnected,
            ServerFrame::Event { .. } => {}
            frame => panic!("unexpected input frame: {frame:?}"),
        }
    }
}

// Input racing the close either lands, keeping the Dojo, or is refused because
// the close won. It never reaches a shell that the close then ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn input_racing_an_unused_close_never_reaches_a_closed_shell() {
    time::timeout(Duration::from_secs(60), async {
        let daemon = Daemon::start().await;
        let shell = PathBuf::from("/bin/sh");
        let mut admin = daemon.connect().await;
        let (mut kept, mut closed) = (0, 0);
        for round in 0..12_u64 {
            let target = create_lair_with(&mut admin, plain_shell_launch(&shell)).await;
            let mut typist = daemon.connect().await;
            let controller_id = typist
                .acquire_control(target.splint_id, target.incarnation)
                .await;
            // Delay alternate inputs so each order occurs: immediate input lands
            // first, a delayed one finds the close holding the Splint.
            let (typed, outcome) = tokio::join!(
                async {
                    if round % 2 == 1 {
                        time::sleep(Duration::from_millis(1)).await;
                    }
                    racing_input(&mut typist, &target, controller_id).await
                },
                close_unused(&mut admin, target.dojo_id),
            );
            let present = admin.topology().await.find_dojo(target.dojo_id).is_some();
            match (typed, outcome) {
                (
                    InputOutcome::Delivered,
                    UnusedDojoCloseOutcome::Used {
                        reason: DojoUse::Input,
                    },
                ) if present => kept += 1,
                (
                    InputOutcome::Rejected | InputOutcome::Disconnected,
                    UnusedDojoCloseOutcome::Closed { .. },
                ) if !present => closed += 1,
                (_, outcome) => panic!("input and close disagreed: {outcome:?}, present={present}"),
            }
        }
        assert_eq!(kept + closed, 12);
        daemon.shutdown();
    })
    .await
    .expect("input race scenario timed out");
}

// Ending a shell that ignores hangup takes the whole shutdown grace. The close
// must answer, and unrelated topology changes proceed, while it retires.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unused_close_does_not_wait_for_the_shell_under_the_transaction() {
    time::timeout(TEST_TIMEOUT, async {
        let daemon = Daemon::start().await;
        let pid_file = daemon.runtime.join("resistant.pid");
        let shell = shell_script(
            &daemon,
            "resistant-shell",
            &format!(
                "echo $$ > '{}'\ntrap '' HUP TERM INT\nwhile :; do sleep 1; done\n",
                pid_file.display()
            ),
        );
        let mut admin = daemon.connect().await;
        let target = create_lair_with(&mut admin, plain_shell_launch(&shell)).await;
        let pid = wait_for_pid_file(&pid_file).await;
        let group = ProcessGroupGuard::new(pid);
        time::sleep(Duration::from_millis(200)).await;
        let started = Instant::now();
        assert!(matches!(
            close_unused(&mut admin, target.dojo_id).await,
            UnusedDojoCloseOutcome::Closed { .. }
        ));
        let expected_topology_revision = admin.topology_revision().await;
        let created = admin
            .request(Request::CreateLair {
                expected_topology_revision,
                name: "unrelated".into(),
                launch: plain_shell_launch(Path::new("/bin/sh")),
                generated_name: false,
            })
            .await;
        assert!(matches!(created, Response::LairCreated { .. }));
        // Hangup then termination each wait one test grace before SIGKILL.
        assert!(
            started.elapsed() < Duration::from_millis(900),
            "the close or the unrelated change waited for the shell"
        );
        assert!(
            Path::new(&format!("/proc/{pid}")).exists(),
            "the resistant shell should still be retiring"
        );
        wait_for_exit(pid).await;
        group.disarm();
        daemon.shutdown();
    })
    .await
    .expect("retiring unused Dojo scenario timed out");
}

// A Lair the user never named goes with its last unused Dojo. A Lair that
// still holds another Dojo, or that was renamed, stays.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unused_close_removes_an_empty_unnamed_lair() {
    time::timeout(TEST_TIMEOUT, async {
        let daemon = Daemon::start().await;
        let shell = PathBuf::from("/bin/sh");
        let mut admin = daemon.connect().await;
        let generated =
            |index: u128| splinterm_core::generated_lair_name(1_787_899_189_000 + index, 42);

        let unnamed =
            create_named_lair(&mut admin, generated(1), true, plain_shell_launch(&shell)).await;
        // A Window opens the second Dojo: an agent's is never unused.
        let (relay, multiplexer) = start_graphical_relay(&daemon).await;
        let mut window = RelayedConnection::open(&multiplexer).await;
        let expected_topology_revision = admin.topology_revision().await;
        let Ok(Response::DojoStarted {
            dojo_id: second, ..
        }) = window
            .request(Request::NewDojo {
                expected_topology_revision,
                lair_id: unnamed.lair_id,
                name: "Dojo 2".into(),
                launch: plain_shell_launch(&shell),
                promote_transient_lair: false,
                generated_name: true,
            })
            .await
        else {
            panic!("daemon did not create the Dojo");
        };
        drop(window);
        drop(multiplexer);
        drop(relay);
        // With another Dojo left, the Lair stays.
        assert!(matches!(
            close_unused(&mut admin, second).await,
            UnusedDojoCloseOutcome::Closed {
                lair_removed: false,
                ..
            }
        ));
        assert!(admin.topology().await.find_lair(unnamed.lair_id).is_some());
        // Its last Dojo takes it along, in one topology revision.
        let before = admin.topology_revision().await;
        let UnusedDojoCloseOutcome::Closed {
            topology_revision,
            lair_removed: true,
        } = close_unused(&mut admin, unnamed.dojo_id).await
        else {
            panic!("the empty unnamed Lair was kept");
        };
        let topology = admin.topology().await;
        assert!(topology.find_lair(unnamed.lair_id).is_none());
        assert_eq!(topology.revision(), topology_revision);
        assert!(topology_revision > before);

        // Renaming names it.
        let renamed =
            create_named_lair(&mut admin, generated(2), true, plain_shell_launch(&shell)).await;
        let expected_topology_revision = admin.topology_revision().await;
        admin
            .request(Request::RenameLair {
                expected_topology_revision,
                lair_id: renamed.lair_id,
                name: "kept".into(),
            })
            .await;
        assert!(matches!(
            close_unused(&mut admin, renamed.dojo_id).await,
            UnusedDojoCloseOutcome::Closed {
                lair_removed: false,
                ..
            }
        ));
        let kept = admin.topology().await;
        assert!(kept.find_lair(renamed.lair_id).unwrap().dojos.is_empty());

        // A name the user chose stays, even one shaped like a generated name.
        let lookalike =
            create_named_lair(&mut admin, generated(3), false, plain_shell_launch(&shell)).await;
        assert!(matches!(
            close_unused(&mut admin, lookalike.dojo_id).await,
            UnusedDojoCloseOutcome::Closed {
                lair_removed: false,
                ..
            }
        ));
        assert!(
            admin
                .topology()
                .await
                .find_lair(lookalike.lair_id)
                .is_some()
        );
        daemon.shutdown();
    })
    .await
    .expect("unnamed Lair scenario timed out");
}

// ---- ADR 0014 remembered grants (Plan 0053 PR 2) ----

const REMEMBERED_PROBE_SOCKET: &str = "SPLINTERM_REMEMBERED_PROBE_SOCKET";
const REMEMBERED_PROBE_SPLINT: &str = "SPLINTERM_REMEMBERED_PROBE_SPLINT";
const REMEMBERED_PROBE_REQUEST: &str = "SPLINTERM_REMEMBERED_PROBE_REQUEST";
const REMEMBERED_PROBE_RESULT: &str = "SPLINTERM_REMEMBERED_PROBE_RESULT";
/// The debug-build daemon seam that lets an ordinary user's tree pass the
/// protected-install ownership rule (`agent_access::TEST_TRUSTED_OWNER_ENV`).
const DURABLE_IDENTITY_OWNER_ENV: &str = "SPLINTERM_TEST_DURABLE_IDENTITY_OWNER_UID";

/// Not a scenario of its own: the remembered-grant scenario runs a copy of
/// this test binary, installed in a protected tree, as the agent. It makes
/// one request on a fresh connection and records whether authorization
/// refused it.
#[tokio::test(flavor = "current_thread")]
async fn remembered_grant_probe() {
    let Some(result) = std::env::var_os(REMEMBERED_PROBE_RESULT) else {
        return;
    };
    let socket = PathBuf::from(std::env::var_os(REMEMBERED_PROBE_SOCKET).unwrap());
    let splint_id: SplintId = std::env::var(REMEMBERED_PROBE_SPLINT)
        .unwrap()
        .parse()
        .unwrap();
    let mut connection = Connection::connect(&socket).await;
    let request = match std::env::var(REMEMBERED_PROBE_REQUEST).unwrap().as_str() {
        "inspect" => Request::InspectSplint { splint_id },
        "rename" => Request::RenameSplint {
            expected_topology_revision: TopologyRevision::new(0),
            splint_id,
            title: "renamed".into(),
        },
        // ADR 0014: asking for Look again with no wait budget. A covered
        // request is answered from the grant; anything else would be
        // pending, since this daemon has no client to show a prompt.
        "request_look" => {
            let Response::Splint { lair_id, .. } = connection
                .request(Request::InspectSplint { splint_id })
                .await
            else {
                panic!("the probe cannot find the Lair");
            };
            Request::RequestLairAccess {
                lair_id,
                level: Some(splinterm_protocol::AccessLevel::Look),
                scopes: Vec::new(),
                wait_millis: 0,
                agent_name: Some("end-to-end agent".into()),
            }
        }
        other => panic!("unknown probe request {other}"),
    };
    let outcome = match connection.request_result(request).await {
        Err(error) if error.code == ErrorCode::Unauthorized => "unauthorized",
        Ok(Response::LairAccessGranted { grant, .. })
            if grant.source == splinterm_protocol::AccessGrantSource::Remembered =>
        {
            "remembered"
        }
        Ok(Response::LairAccessPending { .. }) => "pending",
        _ => "authorized",
    };
    fs::write(result, outcome).unwrap();
}

/// Copies this test binary into `<target tmp>/<unique>/bin/end_to_end-agent`
/// and returns its canonical path. Every directory on that path must be
/// owned by root or this user and be neither group- nor world-writable, as
/// the daemon's test owner seam requires. `None`, with the reason on stderr,
/// when the target directory cannot qualify (for example a build under a
/// world-writable `/tmp` or a group-writable home): the scenario then skips
/// instead of failing an unrelated environment such as a package `check()`.
fn install_protected_agent(label: &str) -> Option<PathBuf> {
    let uid = rustix::process::geteuid().as_raw();
    let base = fs::canonicalize(env!("CARGO_TARGET_TMPDIR")).unwrap();
    for ancestor in base.ancestors() {
        let metadata = fs::symlink_metadata(ancestor).unwrap();
        if !(metadata.is_dir()
            && (metadata.uid() == 0 || metadata.uid() == uid)
            && metadata.mode() & 0o022 == 0)
        {
            eprintln!(
                "skipping the remembered-grant scenario: {} is not a directory owned by root \
                 or the test user that is neither group- nor world-writable",
                ancestor.display()
            );
            return None;
        }
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let tree = base.join(format!(
        "remembered-{label}-{}-{nonce:x}",
        std::process::id()
    ));
    let directory = tree.join("bin");
    fs::create_dir_all(&directory).unwrap();
    for created in [&tree, &directory] {
        fs::set_permissions(created, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let agent = directory.join("end_to_end-agent");
    fs::copy(std::env::current_exe().unwrap(), &agent).unwrap();
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
    Some(fs::canonicalize(agent).unwrap())
}

/// Runs the installed agent copy's probe and returns its outcome.
fn run_remembered_probe(socket: &Path, agent: &Path, splint_id: SplintId, request: &str) -> String {
    let result = agent.with_file_name(format!("result-{request}"));
    let _ = fs::remove_file(&result);
    // A sibling test thread's fork can briefly inherit the descriptor that
    // wrote the copy, so its exec may see ETXTBSY.
    let mut status = None;
    for _ in 0..200 {
        match Command::new(agent)
            .args([
                "--exact",
                "remembered_grant_probe",
                "--test-threads=1",
                "-q",
            ])
            .env(REMEMBERED_PROBE_SOCKET, socket)
            .env(REMEMBERED_PROBE_SPLINT, splint_id.to_string())
            .env(REMEMBERED_PROBE_REQUEST, request)
            .env(REMEMBERED_PROBE_RESULT, &result)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            Ok(exit) => {
                status = Some(exit);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("run the agent probe: {error}"),
        }
    }
    assert!(
        status.expect("the agent copy stayed busy").success(),
        "the agent probe failed"
    );
    fs::read_to_string(result).unwrap()
}

fn remembered_store(agent: &Path, lair_id: LairId, level: &str) -> Vec<u8> {
    serde_json::to_vec_pretty(&serde_json::json!({
        "format": "splinterm.agent-access",
        "version": 1,
        "entries": [{
            "id": "ra_0123456789abcdef0123456789abcdef",
            "identity": {"kind": "protected_install", "path": agent},
            "lair_id": lair_id,
            "access": {"level": level},
            "created_unix_seconds": 1,
            "last_used_unix_seconds": null,
            "display_name": "End-to-end agent",
        }],
    }))
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one private daemon proves remembered access across restarts and a rejected store"
)]
async fn a_remembered_grant_survives_a_daemon_restart() {
    let (Some(agent), Some(stranger)) = (
        install_protected_agent("agent"),
        install_protected_agent("stranger"),
    ) else {
        return;
    };
    time::timeout(Duration::from_secs(180), async {
        let mut daemon = Daemon::start_with_policy(&exact_headless_policy(None)).await;
        let owner = rustix::process::geteuid().as_raw().to_string();
        let seam = [(DURABLE_IDENTITY_OWNER_ENV, owner.as_str())];
        let mut connection = daemon.connect().await;
        let revision = connection.topology_revision().await;
        let Response::LairCreated { lair, .. } = connection
            .request(Request::CreateLair {
                expected_topology_revision: revision,
                name: "remembered".to_owned(),
                launch: LaunchParameters {
                    cwd: daemon.runtime.clone(),
                    command: vec!["/bin/sh".into(), "-c".into(), "sleep 300".into()],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 100,
                },
                generated_name: false,
            })
            .await
        else {
            panic!("the remembered-grant setup did not create a Lair");
        };
        let lair_id = lair.id;
        let LayoutNode::Leaf(splint) = &lair.dojos[0].root else {
            panic!("the remembered-grant Lair is not one leaf");
        };
        let splint_id = splint.id;
        drop(connection);

        // The answer "Always allow Look" was committed in an earlier
        // generation: its store entry is on disk when this daemon starts.
        daemon.stop_preserving_state();
        let store = daemon
            .runtime
            .join("state/splinterm")
            .join("agent-access.json");
        fs::write(&store, remembered_store(&agent, lair_id, "look")).unwrap();
        fs::set_permissions(&store, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.start_again_with_environment(&seam).await;
        assert_eq!(
            run_remembered_probe(&daemon.socket, &agent, splint_id, "inspect"),
            "authorized"
        );
        // Asking for Look again is answered from the remembered grant, at
        // once and with no prompt (ADR 0014).
        assert_eq!(
            run_remembered_probe(&daemon.socket, &agent, splint_id, "request_look"),
            "remembered"
        );
        // Exactly Look: no rename.
        assert_eq!(
            run_remembered_probe(&daemon.socket, &agent, splint_id, "rename"),
            "unauthorized"
        );
        // The same bytes at another path are a different identity.
        assert_eq!(
            run_remembered_probe(&daemon.socket, &stranger, splint_id, "inspect"),
            "unauthorized"
        );

        // A daemon restart: the grant is still there, checked afresh against
        // the new connection.
        daemon.stop_preserving_state();
        daemon.start_again_with_environment(&seam).await;
        assert_eq!(
            run_remembered_probe(&daemon.socket, &agent, splint_id, "inspect"),
            "authorized"
        );
        daemon.stop_preserving_state();
        // The daemon wrote the last-used time it saw, and kept the entry.
        let written: serde_json::Value =
            serde_json::from_slice(&fs::read(&store).unwrap()).unwrap();
        assert_eq!(
            written["entries"][0]["id"],
            "ra_0123456789abcdef0123456789abcdef"
        );
        assert!(written["entries"][0]["last_used_unix_seconds"].is_u64());
        assert_eq!(fs::metadata(&store).unwrap().mode() & 0o7777, 0o600);

        // An unsafe store denies only the remembered source, and is never
        // rewritten; policy keeps working.
        fs::set_permissions(&store, fs::Permissions::from_mode(0o644)).unwrap();
        let rejected = fs::read(&store).unwrap();
        daemon.start_again_with_environment(&seam).await;
        assert_eq!(
            run_remembered_probe(&daemon.socket, &agent, splint_id, "inspect"),
            "unauthorized"
        );
        let mut policy_connection = daemon.connect().await;
        policy_connection.topology_revision().await;
        drop(policy_connection);
        daemon.stop_preserving_state();
        assert_eq!(fs::read(&store).unwrap(), rejected);
        assert_eq!(fs::metadata(&store).unwrap().mode() & 0o7777, 0o644);

        // Repaired by the person, the store grants again.
        fs::set_permissions(&store, fs::Permissions::from_mode(0o600)).unwrap();
        daemon.start_again_with_environment(&seam).await;
        assert_eq!(
            run_remembered_probe(&daemon.socket, &agent, splint_id, "inspect"),
            "authorized"
        );
        daemon.shutdown();
        for installed in [&agent, &stranger] {
            fs::remove_dir_all(installed.parent().unwrap().parent().unwrap()).unwrap();
        }
    })
    .await
    .unwrap();
}

// ---- ADR 0015 remote consent (Plan 0066 M4) ----

const REMOTE_CONSENT_PROBE_SOCKET: &str = "SPLINTERM_REMOTE_CONSENT_PROBE_SOCKET";
const REMOTE_CONSENT_PROBE_LAIR: &str = "SPLINTERM_REMOTE_CONSENT_PROBE_LAIR";
const REMOTE_CONSENT_PROBE_RESULT: &str = "SPLINTERM_REMOTE_CONSENT_PROBE_RESULT";

/// Not a scenario of its own: the remote consent scenario runs this test
/// binary as the agent asking for access. It leads a session of its own, so
/// the relay the scenario started is outside its process tree, and records
/// how its one waiting request ended.
#[tokio::test(flavor = "current_thread")]
async fn remote_consent_requester_probe() {
    let Some(result) = std::env::var_os(REMOTE_CONSENT_PROBE_RESULT) else {
        return;
    };
    rustix::process::setsid().expect("the requester leads a session of its own");
    let socket = PathBuf::from(std::env::var_os(REMOTE_CONSENT_PROBE_SOCKET).unwrap());
    let lair_id: LairId = std::env::var(REMOTE_CONSENT_PROBE_LAIR)
        .unwrap()
        .parse()
        .unwrap();
    let mut connection = Connection::connect(&socket).await;
    let outcome = match connection
        .request_result(Request::RequestLairAccess {
            lair_id,
            level: Some(splinterm_protocol::AccessLevel::Look),
            scopes: Vec::new(),
            wait_millis: 15_000,
            agent_name: Some("remote consent agent".into()),
        })
        .await
    {
        Ok(Response::LairAccessGranted { grant, .. }) => format!("granted:{:?}", grant.source),
        Ok(response) => format!("unexpected:{response:?}"),
        Err(error) => format!("error:{:?}:{}", error.code, error.message),
    };
    let pending = PathBuf::from(&result).with_extension("pending");
    fs::write(&pending, outcome).unwrap();
    fs::rename(&pending, result).unwrap();
}

/// A process the scenario started, killed and reaped when it ends.
struct ReapOnDrop(Child);

impl Drop for ReapOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// One daemon connection carried by the real graphical relay, as a remote
/// Window's is: its requests' replies, with the events that arrive between
/// them kept in order.
struct RelayedConnection {
    channel: splinterm_graphical_relay::LogicalChannel,
    request_id: u64,
    events: std::collections::VecDeque<(u64, SubscriptionEvent)>,
}

impl RelayedConnection {
    async fn open(relay: &splinterm_graphical_relay::ClientMultiplexer) -> Self {
        let mut channel = relay.open_channel().await.unwrap();
        write_frame(
            &mut channel,
            &ClientFrame::Hello {
                minimum_version: PROTOCOL_VERSION,
                maximum_version: PROTOCOL_VERSION,
                role: ClientRole::RemoteInteractive,
                handoff_window: false,
            },
        )
        .await;
        match read_frame(&mut channel).await {
            ServerFrame::Hello {
                version: PROTOCOL_VERSION,
                ..
            } => {}
            frame => panic!("the relayed Window was not admitted: {frame:?}"),
        }
        Self {
            channel,
            request_id: 1,
            events: std::collections::VecDeque::new(),
        }
    }

    async fn request(&mut self, request: Request) -> Result<Response, ProtocolError> {
        let request_id = self.request_id;
        self.request_id += 1;
        write_frame(
            &mut self.channel,
            &ClientFrame::Request {
                request_id,
                diagnostic_correlation: None,
                request,
            },
        )
        .await;
        loop {
            match read_frame(&mut self.channel).await {
                ServerFrame::Response {
                    request_id: response_id,
                    result,
                } if response_id == request_id => return Ok(result),
                ServerFrame::Error {
                    request_id: Some(response_id),
                    error,
                } if response_id == request_id => return Err(error),
                ServerFrame::Event {
                    subscription_id,
                    event,
                    ..
                } => self.events.push_back((subscription_id, event)),
                frame => panic!("unexpected relayed frame: {frame:?}"),
            }
        }
    }

    async fn next_event(&mut self) -> (u64, SubscriptionEvent) {
        if let Some(event) = self.events.pop_front() {
            return event;
        }
        match read_frame(&mut self.channel).await {
            ServerFrame::Event {
                subscription_id,
                event,
                ..
            } => (subscription_id, event),
            frame => panic!("unexpected relayed frame: {frame:?}"),
        }
    }
}

/// Starts the real `splinterm-relay --graphical-stdio` against `daemon`, as
/// a remote Window's host does, and negotiates its channels; each
/// [`RelayedConnection`] opened on them is one remote Window. Keep the relay
/// until the multiplexer is dropped.
async fn start_graphical_relay(
    daemon: &Daemon,
) -> (ReapOnDrop, splinterm_graphical_relay::ClientMultiplexer) {
    let relay_binary = Path::new(DAEMON).with_file_name("splinterm-relay");
    assert!(
        relay_binary.is_file(),
        "{} is missing; build it first with `cargo build -p splinterm-relay --bin \
         splinterm-relay`",
        relay_binary.display()
    );
    // The relay only connects through an owner-only directory.
    fs::set_permissions(&daemon.runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let mut relay = Command::new(&relay_binary)
        .arg("--graphical-stdio")
        .env("SPLINTERM_SOCKET", &daemon.socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(daemon.runtime.join("relay.stderr"))
                .unwrap(),
        )
        .spawn()
        .unwrap();
    let to_relay = tokio::net::unix::pipe::Sender::from_file(fs::File::from(
        std::os::fd::OwnedFd::from(relay.stdin.take().unwrap()),
    ))
    .unwrap();
    let from_relay = tokio::net::unix::pipe::Receiver::from_file(fs::File::from(
        std::os::fd::OwnedFd::from(relay.stdout.take().unwrap()),
    ))
    .unwrap();
    let relay = ReapOnDrop(relay);
    let multiplexer = splinterm_graphical_relay::ClientMultiplexer::negotiate(from_relay, to_relay)
        .await
        .unwrap();
    (relay, multiplexer)
}

async fn wait_for_probe_result(result: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        if let Ok(outcome) = fs::read_to_string(result) {
            return outcome;
        }
        assert!(Instant::now() < deadline, "the requester did not finish");
        time::sleep(Duration::from_millis(20)).await;
    }
}

/// ADR 0015: a remote Window, connected through the real installed-layout
/// `splinterm-relay --graphical-stdio`, is offered an automation client's
/// workspace access prompt by a daemon with no display of its own, and its
/// answer grants the access.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario follows a prompt from request to remote answer to grant"
)]
async fn a_remote_window_answers_a_workspace_access_prompt_through_the_real_relay() {
    let relay_binary = Path::new(DAEMON).with_file_name("splinterm-relay");
    assert!(
        relay_binary.is_file(),
        "{} is missing; build it first with `cargo build -p splinterm-relay --bin \
         splinterm-relay`",
        relay_binary.display()
    );
    time::timeout(Duration::from_secs(60), async {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let runtime =
            std::env::temp_dir().join(format!("spremote-{}-{nonce:x}", std::process::id()));
        fs::create_dir(&runtime).unwrap();
        // The relay only connects through an owner-only directory.
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let socket = runtime.join("splinterd.sock");
        // Without development access nothing grants automation access
        // except an answered prompt, and the daemon has no display.
        let child = Daemon::spawn_configured(&runtime, &socket, None, false);
        Daemon::wait_until_ready(&socket).await;
        let daemon = Daemon {
            child,
            runtime,
            socket,
            policy: None,
            development_terminal_access: false,
        };

        let mut relay = Command::new(&relay_binary)
            .arg("--graphical-stdio")
            .env("SPLINTERM_SOCKET", &daemon.socket)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(
                fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(daemon.runtime.join("relay.stderr"))
                    .unwrap(),
            )
            .spawn()
            .unwrap();
        let to_relay = tokio::net::unix::pipe::Sender::from_file(fs::File::from(
            std::os::fd::OwnedFd::from(relay.stdin.take().unwrap()),
        ))
        .unwrap();
        let from_relay = tokio::net::unix::pipe::Receiver::from_file(fs::File::from(
            std::os::fd::OwnedFd::from(relay.stdout.take().unwrap()),
        ))
        .unwrap();
        let relay = ReapOnDrop(relay);
        let multiplexer =
            splinterm_graphical_relay::ClientMultiplexer::negotiate(from_relay, to_relay)
                .await
                .unwrap();
        let mut window = RelayedConnection::open(&multiplexer).await;

        // The Window opens a workspace, then subscribes to consent offers.
        let mut automation = daemon.connect().await;
        let Ok(Response::Topology { snapshot }) = window.request(Request::InspectTopology).await
        else {
            panic!("the remote Window cannot read the topology");
        };
        let revision = snapshot.revision;
        let Ok(Response::LairCreated { lair, .. }) = window
            .request(Request::CreateLair {
                expected_topology_revision: revision,
                name: "remote-consent".to_owned(),
                launch: LaunchParameters {
                    cwd: daemon.runtime.clone(),
                    command: vec!["/bin/sh".into(), "-c".into(), "sleep 3600".into()],
                    shell: None,
                    login_shell: false,
                    scrollback_lines: 100,
                },
                generated_name: false,
            })
            .await
        else {
            panic!("the remote Window could not open a workspace");
        };
        let Ok(Response::ConsentOffersSubscribed { subscription_id }) = window
            .request(Request::SubscribeConsentOffers {
                kind: splinterm_protocol::ConsentResponderKind::Window,
            })
            .await
        else {
            panic!("the relayed Window is not a consent responder");
        };
        // Automation itself cannot answer.
        assert_eq!(
            automation
                .request_result(Request::SubscribeConsentOffers {
                    kind: splinterm_protocol::ConsentResponderKind::Window,
                })
                .await
                .unwrap_err()
                .code,
            ErrorCode::Unauthorized
        );

        // The agent asks, and waits.
        let result = daemon.runtime.join("requester.result");
        let requester = ReapOnDrop(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "remote_consent_requester_probe",
                    "--test-threads=1",
                    "-q",
                ])
                .env(REMOTE_CONSENT_PROBE_SOCKET, &daemon.socket)
                .env(REMOTE_CONSENT_PROBE_LAIR, lair.id.to_string())
                .env(REMOTE_CONSENT_PROBE_RESULT, &result)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let (event_subscription, event) = window.next_event().await;
        assert_eq!(event_subscription, subscription_id);
        let SubscriptionEvent::ConsentOffered { offer } = event else {
            panic!("expected a consent offer, got {event:?}");
        };
        assert_eq!(offer.validate(), Ok(()));
        assert!(matches!(
            &offer.prompt.target,
            splinterm_protocol::ConsentTarget::Lair { lair_id, .. } if *lair_id == lair.id
        ));
        assert!((1..=118).contains(&offer.respond_within_seconds));
        // A wrong nonce answers nothing.
        let mut forged = [0_u8; splinterm_protocol::CONSENT_OFFER_NONCE_BYTES];
        forged[0] = 1;
        let answer = |nonce| Request::AnswerConsentOffer {
            daemon_generation: offer.daemon_generation,
            offer_id: offer.offer_id,
            nonce,
            decision: splinterm_protocol::ConsentDecision::Grant {
                duration: splinterm_protocol::GrantDuration::Once,
            },
        };
        let forged = splinterm_protocol::ConsentOfferNonce::new(forged);
        if forged != offer.nonce {
            assert_eq!(
                window.request(answer(forged)).await.unwrap_err().code,
                ErrorCode::NotFound
            );
        }
        assert_eq!(
            window.request(answer(offer.nonce)).await,
            Ok(Response::ConsentOfferAnswered)
        );
        assert_eq!(wait_for_probe_result(&result).await, "granted:Ephemeral");
        // The answered offer is gone.
        assert_eq!(
            window.request(answer(offer.nonce)).await.unwrap_err().code,
            ErrorCode::NotFound
        );
        drop(requester);
        drop(window);
        drop(multiplexer);
        drop(relay);
        drop(automation);
        daemon.shutdown();
    })
    .await
    .expect("the remote consent scenario timed out");
}

/// Plan 0067 M3: an agent's `new_dojo` with nothing but its Lair names the
/// Dojo as a person's New Dojo is named, and runs the owner's shell as a
/// login shell in their home.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_new_dojo_runs_the_login_shell_at_home_as_the_next_dojo() {
    time::timeout(TEST_TIMEOUT, async {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox =
            std::env::temp_dir().join(format!("spdefaults-{}-{nonce:x}", std::process::id()));
        let home = sandbox.join("home");
        let bin = sandbox.join("bin");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&bin).unwrap();
        // Only a login shell reads `$HOME/.profile`, so its marker shows the
        // shell was a login shell, started in the home.
        let marker = sandbox.join("login-marker");
        fs::write(
            home.join(".profile"),
            format!(
                "printf '%s|%s' \"$0\" \"$PWD\" > '{marker}.pending' && mv '{marker}.pending' '{marker}'\n",
                marker = marker.display()
            ),
        )
        .unwrap();
        let shell = bin.join("sh");
        std::os::unix::fs::symlink("/bin/sh", &shell).unwrap();
        let daemon = Daemon::start_with_environment(&[
            ("SHELL", shell.to_str().unwrap()),
            ("HOME", home.to_str().unwrap()),
        ])
        .await;
        let mut connection = daemon.connect().await;
        let Response::LairCreated { lair, .. } = connection
            .request(Request::CreateLairAutomation {
                expected_topology_revision: TopologyRevision::default(),
                name: "agent-defaults".into(),
                launch: AutomationLaunch {
                    cwd: Some(daemon.runtime.clone()),
                    argv: vec!["/bin/sh".into(), "-c".into(), "sleep 30".into()],
                },
                generated_name: false,
            })
            .await
        else {
            panic!("the agent's Lair was not created");
        };
        let revision = connection.topology_revision().await;
        let Response::DojoStarted { dojo_id, .. } = connection
            .request(Request::NewDojoAutomation {
                expected_topology_revision: revision,
                lair_id: lair.id,
                name: None,
                launch: AutomationLaunch {
                    cwd: None,
                    argv: Vec::new(),
                },
                generated_name: false,
            })
            .await
        else {
            panic!("the defaulted Dojo was not created");
        };
        let topology = connection.topology().await;
        assert_eq!(topology.find_dojo(dojo_id).unwrap().name, "Dojo 2");

        let deadline = Instant::now() + Duration::from_secs(10);
        let recorded = loop {
            if let Ok(recorded) = fs::read_to_string(&marker) {
                break recorded;
            }
            assert!(Instant::now() < deadline, "the login shell never read .profile");
            time::sleep(Duration::from_millis(20)).await;
        };
        let (argv0, cwd) = recorded.split_once('|').unwrap();
        assert_eq!(argv0, format!("-{}", shell.display()));
        assert_eq!(fs::canonicalize(cwd).unwrap(), fs::canonicalize(&home).unwrap());

        // Automation itself is never offered the Dojo's tab.
        let Response::Topology { snapshot } = connection.request(Request::InspectTopology).await
        else {
            panic!("topology was not returned");
        };
        assert!(snapshot.presentation_offers.is_empty());
        daemon.shutdown();
        let _ = fs::remove_dir_all(&sandbox);
    })
    .await
    .expect("the agent Dojo defaults scenario timed out");
}

/// Plan 0067 M3: two remote Windows, through the real relay, are both
/// offered an agent's new Dojo; exactly one claim is granted, and automation
/// neither sees nor claims the offer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario follows an offer from creation to its single grant"
)]
async fn an_agent_dojo_is_granted_to_exactly_one_of_two_relayed_windows() {
    time::timeout(Duration::from_secs(60), async {
        let daemon = Daemon::start().await;
        let (relay, multiplexer) = start_graphical_relay(&daemon).await;
        let mut first = RelayedConnection::open(&multiplexer).await;
        let mut second = RelayedConnection::open(&multiplexer).await;

        let mut automation = daemon.connect().await;
        let sleeper = || AutomationLaunch {
            cwd: Some(daemon.runtime.clone()),
            argv: vec!["/bin/sh".into(), "-c".into(), "sleep 30".into()],
        };
        let Response::LairCreated { lair, .. } = automation
            .request(Request::CreateLairAutomation {
                expected_topology_revision: TopologyRevision::default(),
                name: "agent-offers".into(),
                launch: sleeper(),
                generated_name: false,
            })
            .await
        else {
            panic!("the agent's Lair was not created");
        };
        let revision = automation.topology_revision().await;
        let Response::DojoStarted { dojo_id, .. } = automation
            .request(Request::NewDojoAutomation {
                expected_topology_revision: revision,
                lair_id: lair.id,
                name: Some("agent".into()),
                launch: sleeper(),
                generated_name: false,
            })
            .await
        else {
            panic!("the agent's Dojo was not created");
        };
        let offer = splinterm_protocol::DojoPresentationOffer {
            lair_id: lair.id,
            dojo_id,
        };
        for window in [&mut first, &mut second] {
            let Ok(Response::Topology { snapshot }) =
                window.request(Request::InspectTopology).await
            else {
                panic!("the relayed Window cannot read the topology");
            };
            assert_eq!(snapshot.validate(), Ok(()));
            assert_eq!(snapshot.presentation_offers, [offer]);
        }
        let Response::Topology { snapshot } = automation.request(Request::InspectTopology).await
        else {
            panic!("topology was not returned");
        };
        assert!(snapshot.presentation_offers.is_empty());
        let claim = Request::ClaimDojoPresentation { dojo_id };
        assert_eq!(
            automation
                .request_result(claim.clone())
                .await
                .unwrap_err()
                .code,
            ErrorCode::Unauthorized
        );

        assert_eq!(
            first.request(claim.clone()).await,
            Ok(Response::DojoPresentationClaimed { granted: true })
        );
        assert_eq!(
            second.request(claim).await,
            Ok(Response::DojoPresentationClaimed { granted: false })
        );
        let Ok(Response::Topology { snapshot }) = second.request(Request::InspectTopology).await
        else {
            panic!("the relayed Window cannot read the topology");
        };
        assert!(snapshot.presentation_offers.is_empty());
        drop(first);
        drop(second);
        drop(multiplexer);
        drop(relay);
        drop(automation);
        daemon.shutdown();
    })
    .await
    .expect("the agent Dojo offer scenario timed out");
}
