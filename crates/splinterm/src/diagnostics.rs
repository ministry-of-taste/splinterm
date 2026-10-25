//! Privacy-preserving, bounded diagnostics for graphical client processes.

use std::{
    env,
    fs::{self, File, Permissions},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    panic::Location,
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use rustix::fs::{FlockOperation, Mode, OFlags, flock, open};
use serde::{Deserialize, Serialize};
use splinterm_core::{DojoId, SplintId, TopologyRevision};
use uuid::Uuid;

const SCHEMA_VERSION: u16 = 1;
const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
const TERMINAL_RESERVE_BYTES: u64 = 16 * 1024;
const OMISSION_RESERVE_BYTES: u64 = 1024;
const MAX_RETAINED_LOGS: usize = 10;
const MAX_RETAINED_BYTES: u64 = 10 * 1024 * 1024;
const JOURNAL_SOCKET: &str = "/run/systemd/journal/socket";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl DiagnosticLevel {
    const fn priority(self) -> u8 {
        match self {
            Self::Error => 3,
            Self::Warn => 4,
            Self::Info => 6,
            Self::Debug | Self::Trace => 7,
        }
    }

    const fn rank(self) -> u8 {
        match self {
            Self::Trace => 0,
            Self::Debug => 1,
            Self::Info => 2,
            Self::Warn => 3,
            Self::Error => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticComponent {
    Splinterm,
    Splinterd,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticModule {
    Client,
    Wayland,
    Topology,
    Pane,
    Input,
}

#[derive(Clone, Copy, Debug)]
struct DiagnosticFilter {
    default: DiagnosticLevel,
    client: Option<DiagnosticLevel>,
    wayland: Option<DiagnosticLevel>,
    topology: Option<DiagnosticLevel>,
    pane: Option<DiagnosticLevel>,
    input: Option<DiagnosticLevel>,
}

impl DiagnosticFilter {
    fn enabled(self, module: DiagnosticModule, level: DiagnosticLevel) -> bool {
        let threshold = match module {
            DiagnosticModule::Client => self.client,
            DiagnosticModule::Wayland => self.wayland,
            DiagnosticModule::Topology => self.topology,
            DiagnosticModule::Pane => self.pane,
            DiagnosticModule::Input => self.input,
        }
        .unwrap_or(self.default);
        level.rank() >= threshold.rank()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticEventCode {
    ClientStarted,
    ConfigWarning,
    ClientExit,
    WaylandFailure,
    TopologyFailure,
    PaneStreamFailure,
    GridCapped,
    Panic,
    TerminationSignal,
    DiagnosticRecordOmitted,
    /// A remote connection attempt failed; the error code carries its category.
    RemoteConnectFailure,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticErrorCode {
    InternalError,
    Config,
    WaylandConnect,
    WaylandDispatch,
    WaylandTeardown,
    TopologyManager,
    PaneStream,
    Io,
    Protocol,
    Panic,
    TerminationSignal,
    RemoteHostKey,
    RemoteAuthentication,
    RemoteUnreachable,
    RemoteInstallation,
    RemoteDaemon,
    RemoteIncompatible,
    RemoteTransport,
    RemoteProfile,
    RemoteLocalSetup,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ExitClass {
    #[serde(rename = "clean:user_close")]
    CleanUserClose,
    #[serde(rename = "clean:final_tab_removed")]
    CleanFinalTabRemoved,
    #[serde(rename = "clean:session_picker_decision")]
    CleanSessionPickerDecision,
    #[serde(rename = "clean:compositor_close")]
    CleanCompositorClose,
    #[serde(rename = "error:wayland_dispatch")]
    ErrorWaylandDispatch,
    #[serde(rename = "error:topology_manager")]
    ErrorTopologyManager,
    #[serde(rename = "error:pane_stream")]
    ErrorPaneStream,
    #[serde(rename = "panic")]
    Panic,
    #[serde(rename = "signal:termination")]
    SignalTermination,
    #[serde(rename = "unknown")]
    Unknown,
}

impl ExitClass {
    #[must_use]
    pub const fn is_clean(self) -> bool {
        matches!(
            self,
            Self::CleanUserClose
                | Self::CleanFinalTabRemoved
                | Self::CleanSessionPickerDecision
                | Self::CleanCompositorClose
        )
    }

    const fn severity(self) -> u8 {
        match self {
            Self::Panic => 3,
            Self::SignalTermination
            | Self::ErrorWaylandDispatch
            | Self::ErrorTopologyManager
            | Self::ErrorPaneStream => 2,
            Self::Unknown => 1,
            Self::CleanUserClose
            | Self::CleanFinalTabRemoved
            | Self::CleanSessionPickerDecision
            | Self::CleanCompositorClose => 0,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiagnosticEvent {
    pub schema_version: u16,
    pub timestamp_unix_ms: u64,
    pub component: DiagnosticComponent,
    pub module: DiagnosticModule,
    pub event: DiagnosticEventCode,
    pub level: DiagnosticLevel,
    pub pid: u32,
    pub client_instance_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dojo_id: Option<DojoId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub splint_id: Option<SplintId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topology_revision: Option<TopologyRevision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_count: Option<u16>,
    pub build_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_class: Option<ExitClass>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<DiagnosticErrorCode>,
    /// The `file:line` in Splinterm's own source that reported a failure:
    /// a compile-time constant naming the failing path, never runtime text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_location: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
struct CorrelationContext {
    window_id: Option<Uuid>,
    window_mapped: bool,
    dojo_id: Option<DojoId>,
    splint_id: Option<SplintId>,
    topology_revision: Option<TopologyRevision>,
    tab_count: Option<u16>,
}

#[derive(Debug)]
struct LogWriter {
    path: PathBuf,
    file: File,
    bytes: u64,
    has_warning: bool,
    omission_written: bool,
}

impl LogWriter {
    fn write_event(&mut self, event: &DiagnosticEvent, terminal: bool) -> io::Result<Vec<u8>> {
        let mut encoded = serde_json::to_vec(event).map_err(io::Error::other)?;
        encoded.push(b'\n');
        let limit = if terminal {
            MAX_LOG_BYTES
        } else {
            MAX_LOG_BYTES - TERMINAL_RESERVE_BYTES - OMISSION_RESERVE_BYTES
        };
        let encoded_len = u64::try_from(encoded.len()).unwrap_or(u64::MAX);
        if self.bytes.saturating_add(encoded_len) > limit {
            if terminal {
                return Err(io::Error::new(
                    io::ErrorKind::FileTooLarge,
                    "terminal diagnostic record exceeds reserved log capacity",
                ));
            }
            if self.omission_written {
                return Ok(Vec::new());
            }
            let mut omitted = event.clone();
            omitted.event = DiagnosticEventCode::DiagnosticRecordOmitted;
            omitted.level = DiagnosticLevel::Warn;
            omitted.error_code = None;
            let mut encoded = serde_json::to_vec(&omitted).map_err(io::Error::other)?;
            encoded.push(b'\n');
            let encoded_len = u64::try_from(encoded.len()).unwrap_or(u64::MAX);
            if self.bytes.saturating_add(encoded_len) > MAX_LOG_BYTES - TERMINAL_RESERVE_BYTES {
                return Ok(Vec::new());
            }
            self.file.write_all(&encoded)?;
            self.bytes += encoded_len;
            self.has_warning = true;
            self.omission_written = true;
            return Ok(encoded);
        }
        self.file.write_all(&encoded)?;
        self.bytes += encoded_len;
        self.has_warning |= event.level.rank() >= DiagnosticLevel::Warn.rank();
        Ok(encoded)
    }

    fn flush_sync(&mut self) -> io::Result<()> {
        self.file.flush()?;
        self.file.sync_data()
    }
}

#[derive(Debug)]
struct DiagnosticState {
    writer: Option<LogWriter>,
    pending_exit: Option<ExitClass>,
    /// The first failure's source location, repeated on the exit record.
    failure_location: Option<String>,
}

#[derive(Debug)]
pub struct ClientDiagnostics {
    client_instance_id: Uuid,
    state_home: PathBuf,
    context: Mutex<CorrelationContext>,
    state: Mutex<DiagnosticState>,
    terminal_committed: AtomicBool,
    handed_off: AtomicBool,
    filter: DiagnosticFilter,
}

static GLOBAL: OnceLock<ClientDiagnostics> = OnceLock::new();
static PANIC_HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);
static SIGNAL_TASK_INSTALLED: AtomicBool = AtomicBool::new(false);

impl ClientDiagnostics {
    /// Creates an isolated diagnostic owner. Production callers normally use
    /// [`initialize_graphical`].
    ///
    /// # Errors
    ///
    /// Returns an error when the private state directories cannot be created or validated.
    pub fn new(state_home: PathBuf) -> io::Result<Self> {
        ensure_private_directory(&state_home.join("splinterm"))?;
        ensure_private_directory(&state_home.join("splinterm/logs"))?;
        Ok(Self {
            client_instance_id: Uuid::new_v4(),
            state_home,
            context: Mutex::new(CorrelationContext::default()),
            state: Mutex::new(DiagnosticState {
                writer: None,
                pending_exit: None,
                failure_location: None,
            }),
            terminal_committed: AtomicBool::new(false),
            handed_off: AtomicBool::new(false),
            filter: configured_filter(),
        })
    }

    #[must_use]
    pub const fn client_instance_id(&self) -> Uuid {
        self.client_instance_id
    }

    pub fn begin_window(&self, dojo_id: Option<DojoId>, splint_id: Option<SplintId>) -> Uuid {
        let window_id = Uuid::new_v4();
        if let Ok(mut context) = self.context.lock() {
            context.window_id = Some(window_id);
            context.window_mapped = false;
            context.dojo_id = dojo_id;
            context.splint_id = splint_id;
            context.topology_revision = None;
            context.tab_count = None;
        }
        if let Ok(mut state) = self.state.lock()
            && !self.terminal_committed.load(Ordering::Acquire)
        {
            state.pending_exit = None;
        }
        window_id
    }

    pub fn ensure_window(&self, dojo_id: Option<DojoId>, splint_id: Option<SplintId>) -> Uuid {
        if let Ok(mut context) = self.context.lock()
            && let Some(window_id) = context.window_id
        {
            if dojo_id.is_some() {
                context.dojo_id = dojo_id;
            }
            if splint_id.is_some() {
                context.splint_id = splint_id;
            }
            return window_id;
        }
        self.begin_window(dojo_id, splint_id)
    }

    pub fn mark_window_mapped(&self) {
        if let Ok(mut context) = self.context.lock() {
            context.window_mapped = context.window_id.is_some();
        }
    }

    #[must_use]
    pub fn protocol_correlation(&self) -> Option<splinterm_protocol::DiagnosticCorrelation> {
        let context = self.context.lock().ok()?;
        Some(splinterm_protocol::DiagnosticCorrelation {
            client_instance_id: self.client_instance_id,
            window_id: context.window_id?,
        })
    }

    pub fn update_topology(&self, revision: TopologyRevision, tab_count: usize) {
        if let Ok(mut context) = self.context.lock() {
            context.topology_revision = Some(revision);
            context.tab_count = u16::try_from(tab_count).ok();
        }
    }

    /// Records that this process handed its work to a detached Splinterm child
    /// and is about to exit. A clean exit after a handoff leaves
    /// `last-client-exit.json` to that child, which may already have recorded an
    /// immediate failure; an abnormal exit is still recorded.
    pub fn mark_detached_handoff(&self) {
        self.handed_off.store(true, Ordering::Release);
    }

    pub fn request_exit(&self, exit_class: ExitClass) {
        if self.terminal_committed.load(Ordering::Acquire) {
            return;
        }
        if let Ok(mut state) = self.state.lock()
            && state
                .pending_exit
                .is_none_or(|current| exit_class.severity() > current.severity())
        {
            state.pending_exit = Some(exit_class);
        }
    }

    pub fn emit(
        &self,
        level: DiagnosticLevel,
        event: DiagnosticEventCode,
        error_code: Option<DiagnosticErrorCode>,
    ) {
        if !self.filter.enabled(module_for_event(event), level) {
            return;
        }
        let record = self.event(level, event, None, error_code);
        self.write_record(&record, level);
    }

    /// Emits a failure event that names where in Splinterm's source it was
    /// reported, and keeps that location for the exit record.
    pub fn emit_failure(
        &self,
        level: DiagnosticLevel,
        event: DiagnosticEventCode,
        error_code: Option<DiagnosticErrorCode>,
        location: &'static Location<'static>,
    ) {
        let location = source_location(location);
        self.remember_failure_location(&location);
        if !self.filter.enabled(module_for_event(event), level) {
            return;
        }
        let mut record = self.event(level, event, None, error_code);
        record.failure_location = Some(location);
        self.write_record(&record, level);
    }

    /// Keeps a failure's source location for the exit record without
    /// emitting an event of its own.
    pub fn record_failure_location(&self, location: &'static Location<'static>) {
        self.remember_failure_location(&source_location(location));
    }

    fn remember_failure_location(&self, location: &str) {
        if let Ok(mut state) = self.state.lock()
            && state.failure_location.is_none()
        {
            state.failure_location = Some(location.to_owned());
        }
    }

    fn write_record(&self, record: &DiagnosticEvent, level: DiagnosticLevel) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.writer.is_none() {
            state.writer = self.open_writer().ok();
        }
        let Some(writer) = state.writer.as_mut() else {
            return;
        };
        let encoded = writer.write_event(record, false).unwrap_or_default();
        if matches!(level, DiagnosticLevel::Warn | DiagnosticLevel::Error) && !encoded.is_empty() {
            submit_journal(&encoded, level);
        }
    }

    pub fn finish(&self, fallback: ExitClass, error_code: Option<DiagnosticErrorCode>) {
        if self.terminal_committed.swap(true, Ordering::AcqRel) {
            return;
        }
        let (pending_exit, failure_location) = self
            .state
            .lock()
            .map(|state| (state.pending_exit, state.failure_location.clone()))
            .unwrap_or_default();
        let exit_class = pending_exit.unwrap_or(fallback);
        let (level, event) = terminal_kind(exit_class);
        let mut record = self.event(level, event, Some(exit_class), error_code);
        if !exit_class.is_clean() {
            record.failure_location = failure_location;
        }
        if !(exit_class.is_clean() && self.handed_off.load(Ordering::Acquire)) {
            let _ = self.write_last_exit(&record);
        }

        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.writer.is_none() && !exit_class.is_clean() {
            state.writer = self.open_writer().ok();
        }
        let Some(mut writer) = state.writer.take() else {
            return;
        };
        let written = writer.write_event(&record, true).unwrap_or_default();
        let _ = writer.flush_sync();
        if !exit_class.is_clean() && !written.is_empty() {
            submit_journal(&written, level);
        }
        if exit_class.is_clean() && !writer.has_warning {
            let _ = fs::remove_file(&writer.path);
        }
    }

    fn finish_panic(&self) {
        if self.terminal_committed.swap(true, Ordering::AcqRel) {
            return;
        }
        let context = self
            .context
            .try_lock()
            .map_or_else(|_| CorrelationContext::default(), |context| *context);
        let record = self.event_with_context(
            context,
            DiagnosticLevel::Error,
            DiagnosticEventCode::Panic,
            Some(ExitClass::Panic),
            Some(DiagnosticErrorCode::Panic),
        );
        let _ = self.write_last_exit(&record);
        let Ok(mut state) = self.state.try_lock() else {
            return;
        };
        if state.writer.is_none() {
            state.writer = self.open_writer().ok();
        }
        let Some(mut writer) = state.writer.take() else {
            return;
        };
        let written = writer.write_event(&record, true).unwrap_or_default();
        let _ = writer.flush_sync();
        if !written.is_empty() {
            submit_journal(&written, DiagnosticLevel::Error);
        }
    }

    fn event(
        &self,
        level: DiagnosticLevel,
        event: DiagnosticEventCode,
        exit_class: Option<ExitClass>,
        error_code: Option<DiagnosticErrorCode>,
    ) -> DiagnosticEvent {
        let context = self
            .context
            .lock()
            .map_or_else(|_| CorrelationContext::default(), |context| *context);
        self.event_with_context(context, level, event, exit_class, error_code)
    }

    fn event_with_context(
        &self,
        context: CorrelationContext,
        level: DiagnosticLevel,
        event: DiagnosticEventCode,
        exit_class: Option<ExitClass>,
        error_code: Option<DiagnosticErrorCode>,
    ) -> DiagnosticEvent {
        DiagnosticEvent {
            schema_version: SCHEMA_VERSION,
            timestamp_unix_ms: unix_millis(),
            component: DiagnosticComponent::Splinterm,
            module: module_for_event(event),
            event,
            level,
            pid: std::process::id(),
            client_instance_id: self.client_instance_id,
            window_id: context.window_mapped.then_some(context.window_id).flatten(),
            dojo_id: context.dojo_id,
            splint_id: context.splint_id,
            topology_revision: context.topology_revision,
            tab_count: context.tab_count,
            build_version: env!("CARGO_PKG_VERSION").to_owned(),
            build_commit: splinterd::SOURCE_COMMIT.map(str::to_owned),
            exit_class,
            error_code,
            failure_location: None,
        }
    }

    fn open_writer(&self) -> io::Result<LogWriter> {
        let logs = self.state_home.join("splinterm/logs");
        ensure_private_directory(&logs)?;
        let timestamp = unix_millis();
        let name = format!(
            "client-{timestamp}Z-{}-{}.jsonl",
            std::process::id(),
            self.client_instance_id
        );
        let path = logs.join(name);
        let owned = open(
            &path,
            OFlags::WRONLY
                | OFlags::CREATE
                | OFlags::EXCL
                | OFlags::APPEND
                | OFlags::CLOEXEC
                | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        )?;
        let file = File::from(owned);
        flock(&file, FlockOperation::LockExclusive)?;
        prune_retained(&logs, &path)?;
        Ok(LogWriter {
            path,
            file,
            bytes: 0,
            has_warning: false,
            omission_written: false,
        })
    }

    fn write_last_exit(&self, event: &DiagnosticEvent) -> io::Result<()> {
        let directory = self.state_home.join("splinterm");
        ensure_private_directory(&directory)?;
        let target = directory.join("last-client-exit.json");
        let temporary = directory.join(format!(".last-client-exit-{}.tmp", Uuid::new_v4()));
        let owned = open(
            &temporary,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        )?;
        let mut file = File::from(owned);
        serde_json::to_writer(&mut file, event).map_err(io::Error::other)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, &target)?;
        File::open(&directory)?.sync_all()
    }
}

#[must_use]
pub fn global() -> Option<&'static ClientDiagnostics> {
    GLOBAL.get()
}

/// Initializes process-global graphical diagnostics and the replacement panic hook.
///
/// # Errors
///
/// Returns an error when the private state directory cannot be created or validated.
pub fn initialize_graphical() -> io::Result<&'static ClientDiagnostics> {
    if let Some(existing) = GLOBAL.get() {
        return Ok(existing);
    }
    let diagnostics = ClientDiagnostics::new(state_home()?)?;
    let _ = GLOBAL.set(diagnostics);
    install_panic_hook();
    GLOBAL
        .get()
        .ok_or_else(|| io::Error::other("diagnostic initialization race"))
}

/// Names a source location as `crates/...:line` (or the bare file name when
/// the path is outside the workspace), so no build machine path is recorded.
fn source_location(location: &Location<'_>) -> String {
    let file = location.file();
    let file = file.find("crates/").map_or_else(
        || {
            Path::new(file)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("unknown")
        },
        |start| &file[start..],
    );
    format!("{file}:{}", location.line())
}

pub fn finish_global(exit_class: ExitClass, error_code: Option<DiagnosticErrorCode>) {
    if let Some(diagnostics) = global() {
        diagnostics.request_exit(exit_class);
        diagnostics.finish(exit_class, error_code);
    }
}

pub fn install_termination_signal_task() {
    use tokio::signal::unix::{SignalKind, signal};

    if SIGNAL_TASK_INSTALLED.swap(true, Ordering::AcqRel) {
        return;
    }
    let Ok(mut terminate) = signal(SignalKind::terminate()) else {
        return;
    };
    let Ok(mut interrupt) = signal(SignalKind::interrupt()) else {
        return;
    };
    tokio::spawn(async move {
        let signal_number = tokio::select! {
            _ = terminate.recv() => libc::SIGTERM,
            _ = interrupt.recv() => libc::SIGINT,
        };
        finish_global(
            ExitClass::SignalTermination,
            Some(DiagnosticErrorCode::TerminationSignal),
        );
        std::process::exit(128 + signal_number);
    });
}

/// Prunes finalized retained logs to the configured count and size budgets.
///
/// # Errors
///
/// Returns an error when private state validation, locking, or pruning fails.
pub fn maintain_retention() -> io::Result<()> {
    let logs = state_home()?.join("splinterm/logs");
    ensure_private_directory(&logs)?;
    prune_retained(&logs, Path::new(""))
}

/// Reads the authoritative typed last-client-exit summary when present.
///
/// # Errors
///
/// Returns an error when the summary fails ownership, size, or schema validation.
pub fn read_last_exit() -> io::Result<Option<DiagnosticEvent>> {
    read_single_event(&state_home()?.join("splinterm/last-client-exit.json"))
}

/// Reads the newest retained abnormal terminal event, optionally restricted to panic events.
///
/// # Errors
///
/// Returns an error when the private log directory or a bounded candidate cannot be read.
pub fn newest_abnormal_event(panic_only: bool) -> io::Result<Option<DiagnosticEvent>> {
    let logs = state_home()?.join("splinterm/logs");
    ensure_private_directory(&logs)?;
    let mut candidates = fs::read_dir(&logs)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).ok()?;
            if !entry.file_name().to_string_lossy().starts_with("client-")
                || path
                    .extension()
                    .is_none_or(|extension| extension != "jsonl")
                || !metadata.file_type().is_file()
                || metadata.uid() != rustix::process::getuid().as_raw()
                || metadata.len() > MAX_LOG_BYTES
            {
                return None;
            }
            Some((path, metadata.modified().unwrap_or(UNIX_EPOCH)))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(_, modified)| std::cmp::Reverse(*modified));
    for (path, _) in candidates {
        let Ok(bytes) = read_secure_bounded(&path, MAX_LOG_BYTES, true) else {
            continue;
        };
        let Ok(content) = String::from_utf8(bytes) else {
            continue;
        };
        if let Some(event) = content
            .lines()
            .filter_map(|line| serde_json::from_str::<DiagnosticEvent>(line).ok())
            .rev()
            .find(|event| {
                event.exit_class.is_some_and(|exit| {
                    if panic_only {
                        exit == ExitClass::Panic
                    } else {
                        !exit.is_clean()
                    }
                })
            })
        {
            return Ok(Some(event));
        }
    }
    Ok(None)
}

fn read_single_event(path: &Path) -> io::Result<Option<DiagnosticEvent>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = read_secure_bounded(path, TERMINAL_RESERVE_BYTES, false)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid diagnostic summary"))
}

fn read_secure_bounded(
    path: &Path,
    maximum_bytes: u64,
    require_finalized: bool,
) -> io::Result<Vec<u8>> {
    let owned = open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )?;
    let mut file = File::from(owned);
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.len() > maximum_bytes
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "diagnostic file failed ownership, type, or size validation",
        ));
    }
    if require_finalized {
        flock(&file, FlockOperation::NonBlockingLockShared)?;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    Read::take(&mut file, maximum_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum_bytes {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            "diagnostic file exceeded its read bound",
        ));
    }
    Ok(bytes)
}

/// Resolves the absolute user state-home directory without creating it.
///
/// # Errors
///
/// Returns an error when `XDG_STATE_HOME` is relative or no home directory is available.
pub fn state_home() -> io::Result<PathBuf> {
    if let Some(path) = env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            return Ok(path);
        }
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "XDG_STATE_HOME must be absolute",
        ));
    }
    let home = env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is unavailable"))?;
    Ok(PathBuf::from(home).join(".local/state"))
}

fn install_panic_hook() {
    if PANIC_HOOK_INSTALLED.swap(true, Ordering::AcqRel) {
        return;
    }
    std::panic::set_hook(Box::new(|_| {
        if let Some(diagnostics) = global() {
            diagnostics.finish_panic();
        }
        eprintln!("splinterm terminated after an internal panic");
    }));
}

fn configured_filter() -> DiagnosticFilter {
    let mut filter = DiagnosticFilter {
        default: DiagnosticLevel::Warn,
        client: None,
        wayland: None,
        topology: None,
        pane: None,
        input: None,
    };
    let Some(value) = env::var_os("SPLINTERM_LOG") else {
        return filter;
    };
    for directive in value.to_string_lossy().split(',') {
        let (module, level) = directive
            .split_once('=')
            .map_or((None, directive), |(module, level)| (Some(module), level));
        let level = match level {
            "trace" => DiagnosticLevel::Trace,
            "debug" => DiagnosticLevel::Debug,
            "info" => DiagnosticLevel::Info,
            "warn" => DiagnosticLevel::Warn,
            "error" => DiagnosticLevel::Error,
            _ => continue,
        };
        match module {
            None => filter.default = level,
            Some("client") => filter.client = Some(level),
            Some("wayland") => filter.wayland = Some(level),
            Some("topology") => filter.topology = Some(level),
            Some("pane") => filter.pane = Some(level),
            Some("input") => filter.input = Some(level),
            Some(_) => {}
        }
    }
    filter
}

const fn module_for_event(event: DiagnosticEventCode) -> DiagnosticModule {
    match event {
        DiagnosticEventCode::WaylandFailure => DiagnosticModule::Wayland,
        DiagnosticEventCode::TopologyFailure => DiagnosticModule::Topology,
        DiagnosticEventCode::PaneStreamFailure | DiagnosticEventCode::GridCapped => {
            DiagnosticModule::Pane
        }
        DiagnosticEventCode::ClientStarted
        | DiagnosticEventCode::ConfigWarning
        | DiagnosticEventCode::ClientExit
        | DiagnosticEventCode::Panic
        | DiagnosticEventCode::TerminationSignal
        | DiagnosticEventCode::DiagnosticRecordOmitted
        | DiagnosticEventCode::RemoteConnectFailure => DiagnosticModule::Client,
    }
}

fn terminal_kind(exit_class: ExitClass) -> (DiagnosticLevel, DiagnosticEventCode) {
    match exit_class {
        ExitClass::Panic => (DiagnosticLevel::Error, DiagnosticEventCode::Panic),
        ExitClass::SignalTermination => (
            DiagnosticLevel::Error,
            DiagnosticEventCode::TerminationSignal,
        ),
        ExitClass::ErrorWaylandDispatch => {
            (DiagnosticLevel::Error, DiagnosticEventCode::WaylandFailure)
        }
        ExitClass::ErrorTopologyManager => {
            (DiagnosticLevel::Error, DiagnosticEventCode::TopologyFailure)
        }
        ExitClass::ErrorPaneStream => (
            DiagnosticLevel::Error,
            DiagnosticEventCode::PaneStreamFailure,
        ),
        ExitClass::Unknown => (DiagnosticLevel::Error, DiagnosticEventCode::ClientExit),
        ExitClass::CleanUserClose
        | ExitClass::CleanFinalTabRemoved
        | ExitClass::CleanSessionPickerDecision
        | ExitClass::CleanCompositorClose => {
            (DiagnosticLevel::Info, DiagnosticEventCode::ClientExit)
        }
    }
}

fn ensure_private_directory(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() || metadata.uid() != rustix::process::getuid().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "diagnostic directory is not a user-owned directory",
        ));
    }
    fs::set_permissions(path, Permissions::from_mode(0o700))
}

fn prune_retained(logs: &Path, current: &Path) -> io::Result<()> {
    let lock_path = logs.join(".retention.lock");
    let owned = open(
        &lock_path,
        OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::RUSR | Mode::WUSR,
    )?;
    let lock = File::from(owned);
    let metadata = lock.metadata()?;
    if !metadata.file_type().is_file() || metadata.uid() != rustix::process::getuid().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "diagnostic retention lock failed ownership validation",
        ));
    }
    flock(&lock, FlockOperation::LockExclusive)?;

    let mut retained = Vec::new();
    for entry in fs::read_dir(logs)? {
        let entry = entry?;
        let path = entry.path();
        if path == current
            || !entry.file_name().to_string_lossy().starts_with("client-")
            || path
                .extension()
                .is_none_or(|extension| extension != "jsonl")
        {
            continue;
        }
        let Ok(owned) = open(
            &path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        ) else {
            continue;
        };
        let candidate = File::from(owned);
        if flock(&candidate, FlockOperation::NonBlockingLockExclusive).is_err() {
            continue;
        }
        let metadata = candidate.metadata()?;
        if !metadata.file_type().is_file() || metadata.uid() != rustix::process::getuid().as_raw() {
            continue;
        }
        retained.push((
            path,
            metadata.modified().unwrap_or(UNIX_EPOCH),
            metadata.len(),
        ));
    }
    retained.sort_by_key(|(_, modified, _)| *modified);
    let mut total: u64 = retained.iter().map(|(_, _, bytes)| *bytes).sum();
    while retained.len() > MAX_RETAINED_LOGS || total > MAX_RETAINED_BYTES {
        let (path, _, bytes) = retained.remove(0);
        fs::remove_file(path)?;
        total = total.saturating_sub(bytes);
    }
    Ok(())
}

fn submit_journal(encoded: &[u8], level: DiagnosticLevel) {
    let _ = submit_journal_to(encoded, level, Path::new(JOURNAL_SOCKET));
}

fn submit_journal_to(encoded: &[u8], level: DiagnosticLevel, socket_path: &Path) -> io::Result<()> {
    use std::os::unix::net::UnixDatagram;

    let socket = UnixDatagram::unbound()?;
    socket.connect(socket_path)?;
    let payload = [
        b"PRIORITY=".as_slice(),
        level.priority().to_string().as_bytes(),
        b"\nSYSLOG_IDENTIFIER=splinterm\nMESSAGE=".as_slice(),
        encoded,
    ]
    .concat();
    socket.send(&payload)?;
    Ok(())
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{thread, time::Duration};

    fn test_directory(label: &str) -> PathBuf {
        let path = env::temp_dir().join(format!(
            // Keep journal socket paths within SUN_LEN under a repo-local TMPDIR.
            "sd-{label}-{}-{}",
            std::process::id(),
            Uuid::new_v4().simple()
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    fn retained_logs(root: &Path) -> Vec<PathBuf> {
        let logs = root.join("splinterm/logs");
        fs::read_dir(logs)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "jsonl")
            })
            .collect()
    }

    #[test]
    fn clean_uneventful_exit_writes_summary_without_retaining_log() {
        let root = test_directory("clean");
        let diagnostics = ClientDiagnostics::new(root.clone()).unwrap();
        diagnostics.begin_window(None, None);
        diagnostics.mark_window_mapped();
        diagnostics.finish(ExitClass::CleanUserClose, None);

        assert!(retained_logs(&root).is_empty());
        let summary: DiagnosticEvent = serde_json::from_slice(
            &fs::read(root.join("splinterm/last-client-exit.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(summary.exit_class, Some(ExitClass::CleanUserClose));
        assert!(summary.window_id.is_some());
        assert_eq!(
            fs::metadata(root.join("splinterm/last-client-exit.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn clean_exit_after_a_detached_handoff_keeps_the_child_failure_as_last_exit() {
        let root = test_directory("handoff");
        let summary = || {
            read_single_event(&root.join("splinterm/last-client-exit.json"))
                .unwrap()
                .unwrap()
        };
        let chooser = ClientDiagnostics::new(root.clone()).unwrap();
        chooser.begin_window(None, None);
        chooser.mark_window_mapped();
        chooser.request_exit(ExitClass::CleanSessionPickerDecision);
        chooser.mark_detached_handoff();

        // The detached remote child fails before the chooser finishes.
        let child = ClientDiagnostics::new(root.clone()).unwrap();
        child.begin_window(None, None);
        child.finish(ExitClass::Unknown, Some(DiagnosticErrorCode::InternalError));
        chooser.finish(ExitClass::Unknown, None);

        let last = summary();
        assert_eq!(last.client_instance_id, child.client_instance_id());
        assert_eq!(last.exit_class, Some(ExitClass::Unknown));
        assert_eq!(last.error_code, Some(DiagnosticErrorCode::InternalError));
        assert!(retained_logs(&root).iter().all(|path| {
            !path
                .to_string_lossy()
                .contains(&chooser.client_instance_id().to_string())
        }));

        // Without a handoff a clean exit is recorded as before.
        let plain = ClientDiagnostics::new(root.clone()).unwrap();
        plain.finish(ExitClass::CleanUserClose, None);
        assert_eq!(summary().client_instance_id, plain.client_instance_id());

        // A handed-off process that then exits abnormally is still recorded.
        let failed = ClientDiagnostics::new(root.clone()).unwrap();
        failed.mark_detached_handoff();
        failed.finish(ExitClass::ErrorWaylandDispatch, None);
        let last = summary();
        assert_eq!(last.client_instance_id, failed.client_instance_id());
        assert_eq!(last.exit_class, Some(ExitClass::ErrorWaylandDispatch));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn warning_retains_log_and_terminal_record() {
        let root = test_directory("warning");
        let diagnostics = ClientDiagnostics::new(root.clone()).unwrap();
        diagnostics.emit(
            DiagnosticLevel::Warn,
            DiagnosticEventCode::TopologyFailure,
            Some(DiagnosticErrorCode::TopologyManager),
        );
        diagnostics.finish(ExitClass::CleanUserClose, None);

        let logs = retained_logs(&root);
        assert_eq!(logs.len(), 1);
        let records: Vec<DiagnosticEvent> = fs::read_to_string(&logs[0])
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.len(), 2);
        assert_eq!(records[1].exit_class, Some(ExitClass::CleanUserClose));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn panic_finalization_never_waits_on_reentrant_context_lock() {
        let root = test_directory("panic-reentrant");
        let diagnostics = ClientDiagnostics::new(root.clone()).unwrap();
        let context_guard = diagnostics.context.lock().unwrap();
        diagnostics.finish_panic();
        let summary = read_single_event(&root.join("splinterm/last-client-exit.json"))
            .unwrap()
            .unwrap();
        assert_eq!(summary.exit_class, Some(ExitClass::Panic));
        drop(context_guard);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sequential_windows_rotate_correlation_and_omit_pre_map_identity() {
        let root = test_directory("window-rotation");
        let diagnostics = ClientDiagnostics::new(root.clone()).unwrap();
        let first = diagnostics.begin_window(None, None);
        assert_eq!(diagnostics.protocol_correlation().unwrap().window_id, first);
        assert!(
            diagnostics
                .event(
                    DiagnosticLevel::Error,
                    DiagnosticEventCode::WaylandFailure,
                    None,
                    None,
                )
                .window_id
                .is_none()
        );
        diagnostics.mark_window_mapped();
        assert_eq!(
            diagnostics
                .event(
                    DiagnosticLevel::Info,
                    DiagnosticEventCode::ClientStarted,
                    None,
                    None,
                )
                .window_id,
            Some(first)
        );

        let second = diagnostics.begin_window(None, None);
        assert_ne!(first, second);
        assert_eq!(
            diagnostics.protocol_correlation().unwrap().window_id,
            second
        );
        assert!(
            diagnostics
                .event(
                    DiagnosticLevel::Error,
                    DiagnosticEventCode::WaylandFailure,
                    None,
                    None,
                )
                .window_id
                .is_none()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn secure_readers_reject_symlinks_and_skip_active_logs() {
        use std::os::unix::fs::symlink;

        let root = test_directory("secure-read");
        let diagnostics = ClientDiagnostics::new(root.clone()).unwrap();
        let outside = root.join("outside.json");
        let event = diagnostics.event(
            DiagnosticLevel::Error,
            DiagnosticEventCode::Panic,
            Some(ExitClass::Panic),
            Some(DiagnosticErrorCode::Panic),
        );
        fs::write(&outside, serde_json::to_vec(&event).unwrap()).unwrap();
        let summary_path = root.join("splinterm/last-client-exit.json");
        symlink(&outside, &summary_path).unwrap();
        assert!(read_single_event(&summary_path).is_err());

        let log_path = root.join("splinterm/logs/client-active.jsonl");
        fs::write(
            &log_path,
            [serde_json::to_vec(&event).unwrap(), b"\n".to_vec()].concat(),
        )
        .unwrap();
        let active = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&log_path)
            .unwrap();
        flock(&active, FlockOperation::LockExclusive).unwrap();
        assert!(read_secure_bounded(&log_path, MAX_LOG_BYTES, true).is_err());
        drop(active);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn error_precedence_and_terminal_commit_are_exactly_once() {
        let root = test_directory("precedence");
        let diagnostics = ClientDiagnostics::new(root.clone()).unwrap();
        diagnostics.request_exit(ExitClass::CleanCompositorClose);
        diagnostics.request_exit(ExitClass::ErrorPaneStream);
        diagnostics.finish(ExitClass::Unknown, Some(DiagnosticErrorCode::PaneStream));
        diagnostics.finish(ExitClass::Panic, Some(DiagnosticErrorCode::Panic));

        let summary: DiagnosticEvent = serde_json::from_slice(
            &fs::read(root.join("splinterm/last-client-exit.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(summary.exit_class, Some(ExitClass::ErrorPaneStream));
        let log = fs::read_to_string(retained_logs(&root).remove(0)).unwrap();
        assert_eq!(log.lines().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retention_prunes_oldest_finalized_logs() {
        let root = test_directory("retention");
        let logs = root.join("splinterm/logs");
        ensure_private_directory(&logs).unwrap();
        for index in 0..=MAX_RETAINED_LOGS {
            let path = logs.join(format!("client-{index:02}.jsonl"));
            fs::write(&path, b"{}\n").unwrap();
            thread::sleep(Duration::from_millis(2));
        }
        let current = logs.join("current.jsonl");
        fs::write(&current, b"").unwrap();
        prune_retained(&logs, &current).unwrap();
        let count = fs::read_dir(&logs)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("client-"))
            .count();
        assert_eq!(count, MAX_RETAINED_LOGS);
        assert!(!logs.join("client-00.jsonl").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retention_enforces_total_bytes_under_concurrent_pruning() {
        let root = test_directory("retention-bytes");
        let logs = root.join("splinterm/logs");
        ensure_private_directory(&logs).unwrap();
        for index in 0..6 {
            let file = File::create(logs.join(format!("client-{index:02}.jsonl"))).unwrap();
            file.set_len(2 * 1024 * 1024).unwrap();
            thread::sleep(Duration::from_millis(2));
        }
        let first_logs = logs.clone();
        let second_logs = logs.clone();
        let first = thread::spawn(move || prune_retained(&first_logs, Path::new("")));
        let second = thread::spawn(move || prune_retained(&second_logs, Path::new("")));
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
        let total = fs::read_dir(&logs)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("client-"))
            .map(|entry| entry.metadata().unwrap().len())
            .sum::<u64>();
        assert!(total <= MAX_RETAINED_BYTES);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn active_logs_are_skipped_by_retention_pruning() {
        let root = test_directory("active-retention");
        let logs = root.join("splinterm/logs");
        ensure_private_directory(&logs).unwrap();
        let active_path = logs.join("client-00.jsonl");
        fs::write(&active_path, b"{}\n").unwrap();
        let active = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&active_path)
            .unwrap();
        flock(&active, FlockOperation::LockExclusive).unwrap();
        for index in 1..=MAX_RETAINED_LOGS + 1 {
            fs::write(logs.join(format!("client-{index:02}.jsonl")), b"{}\n").unwrap();
            thread::sleep(Duration::from_millis(2));
        }
        prune_retained(&logs, Path::new("")).unwrap();
        assert!(active_path.exists());
        assert_eq!(
            fs::read_dir(&logs)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("client-"))
                .count(),
            MAX_RETAINED_LOGS + 1
        );
        drop(active);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bounded_writer_preserves_terminal_reserve_and_marks_omission() {
        let root = test_directory("bounded");
        let diagnostics = ClientDiagnostics::new(root.clone()).unwrap();
        for _ in 0..10_000 {
            diagnostics.emit(
                DiagnosticLevel::Warn,
                DiagnosticEventCode::TopologyFailure,
                Some(DiagnosticErrorCode::TopologyManager),
            );
        }
        diagnostics.finish(
            ExitClass::ErrorTopologyManager,
            Some(DiagnosticErrorCode::TopologyManager),
        );
        let path = retained_logs(&root).remove(0);
        assert!(fs::metadata(&path).unwrap().len() <= MAX_LOG_BYTES);
        let content = fs::read_to_string(path).unwrap();
        let records = content
            .lines()
            .map(|line| serde_json::from_str::<DiagnosticEvent>(line).unwrap())
            .collect::<Vec<_>>();
        assert!(
            records
                .iter()
                .any(|record| { record.event == DiagnosticEventCode::DiagnosticRecordOmitted })
        );
        assert_eq!(
            records.last().and_then(|record| record.exit_class),
            Some(ExitClass::ErrorTopologyManager)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn private_paths_reject_symlinked_diagnostic_directory() {
        use std::os::unix::fs::symlink;

        let root = test_directory("symlink");
        let outside = test_directory("symlink-outside");
        symlink(&outside, root.join("splinterm")).unwrap();
        assert_eq!(
            ClientDiagnostics::new(root.clone()).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        fs::remove_file(root.join("splinterm")).unwrap();
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn last_exit_is_atomically_replaced_with_private_mode() {
        let root = test_directory("atomic-summary");
        let first = ClientDiagnostics::new(root.clone()).unwrap();
        first.finish(ExitClass::CleanUserClose, None);
        let second = ClientDiagnostics::new(root.clone()).unwrap();
        second.finish(
            ExitClass::ErrorWaylandDispatch,
            Some(DiagnosticErrorCode::WaylandDispatch),
        );
        let summary = read_single_event(&root.join("splinterm/last-client-exit.json"))
            .unwrap()
            .unwrap();
        assert_eq!(summary.exit_class, Some(ExitClass::ErrorWaylandDispatch));
        assert_eq!(
            fs::metadata(root.join("splinterm/last-client-exit.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(
            fs::read_dir(root.join("splinterm"))
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".last-client-exit-"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn journal_transport_is_exact_and_unavailability_is_bounded() {
        use std::os::unix::net::UnixDatagram;

        let root = test_directory("journal");
        let socket_path = root.join("journal.sock");
        let receiver = UnixDatagram::bind(&socket_path).unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let event = b"{\"schema_version\":1}\n";
        submit_journal_to(event, DiagnosticLevel::Error, &socket_path).unwrap();
        let mut payload_buffer = [0_u8; 256];
        let length = receiver.recv(&mut payload_buffer).unwrap();
        assert_eq!(
            &payload_buffer[..length],
            [
                b"PRIORITY=3\nSYSLOG_IDENTIFIER=splinterm\nMESSAGE=".as_slice(),
                event.as_slice(),
            ]
            .concat()
        );
        assert!(
            submit_journal_to(event, DiagnosticLevel::Error, &root.join("missing.sock")).is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn module_filter_applies_overrides_without_widening_other_modules() {
        let filter = DiagnosticFilter {
            default: DiagnosticLevel::Info,
            client: None,
            wayland: Some(DiagnosticLevel::Trace),
            topology: Some(DiagnosticLevel::Error),
            pane: None,
            input: None,
        };
        assert!(filter.enabled(DiagnosticModule::Wayland, DiagnosticLevel::Trace));
        assert!(!filter.enabled(DiagnosticModule::Topology, DiagnosticLevel::Debug));
        assert!(!filter.enabled(DiagnosticModule::Pane, DiagnosticLevel::Debug));
        assert!(filter.enabled(DiagnosticModule::Pane, DiagnosticLevel::Info));
    }

    #[test]
    fn schema_contains_only_typed_correlation_fields() {
        let root = test_directory("schema");
        let diagnostics = ClientDiagnostics::new(root.clone()).unwrap();
        let event = diagnostics.event(
            DiagnosticLevel::Error,
            DiagnosticEventCode::WaylandFailure,
            Some(ExitClass::ErrorWaylandDispatch),
            Some(DiagnosticErrorCode::WaylandDispatch),
        );
        let value = serde_json::to_value(event).unwrap();
        assert!(value.get("message").is_none());
        assert!(value.get("error").is_none());
        assert!(value.get("path").is_none());
        assert!(value.get("argv").is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remote_connect_failures_record_only_a_documented_category_code() {
        assert_eq!(
            serde_json::to_string(&DiagnosticEventCode::RemoteConnectFailure).unwrap(),
            "\"remote_connect_failure\""
        );
        assert_eq!(
            module_for_event(DiagnosticEventCode::RemoteConnectFailure),
            DiagnosticModule::Client
        );
        let codes = [
            (DiagnosticErrorCode::RemoteHostKey, "remote_host_key"),
            (
                DiagnosticErrorCode::RemoteAuthentication,
                "remote_authentication",
            ),
            (DiagnosticErrorCode::RemoteUnreachable, "remote_unreachable"),
            (
                DiagnosticErrorCode::RemoteInstallation,
                "remote_installation",
            ),
            (DiagnosticErrorCode::RemoteDaemon, "remote_daemon"),
            (
                DiagnosticErrorCode::RemoteIncompatible,
                "remote_incompatible",
            ),
            (DiagnosticErrorCode::RemoteTransport, "remote_transport"),
            (DiagnosticErrorCode::RemoteProfile, "remote_profile"),
            (DiagnosticErrorCode::RemoteLocalSetup, "remote_local_setup"),
        ];
        for (code, name) in codes {
            assert_eq!(serde_json::to_string(&code).unwrap(), format!("\"{name}\""));
        }
    }

    #[test]
    fn a_failure_names_its_source_location_on_the_event_and_the_exit_record() {
        let root = test_directory("failure-location");
        let diagnostics = ClientDiagnostics::new(root.clone()).unwrap();
        let location = Location::caller();
        diagnostics.emit_failure(
            DiagnosticLevel::Error,
            DiagnosticEventCode::WaylandFailure,
            Some(DiagnosticErrorCode::WaylandDispatch),
            location,
        );
        // A later failure does not replace the first one on the exit record.
        diagnostics.record_failure_location(Location::caller());
        diagnostics.finish(
            ExitClass::ErrorWaylandDispatch,
            Some(DiagnosticErrorCode::InternalError),
        );
        let expected = format!("crates/splinterm/src/diagnostics.rs:{}", location.line());

        let logs = retained_logs(&root);
        assert_eq!(logs.len(), 1);
        let records: Vec<DiagnosticEvent> = fs::read_to_string(&logs[0])
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].event, DiagnosticEventCode::WaylandFailure);
        assert_eq!(records[0].failure_location.as_deref(), Some(&*expected));
        assert_eq!(records[1].exit_class, Some(ExitClass::ErrorWaylandDispatch));
        assert_eq!(records[1].failure_location.as_deref(), Some(&*expected));
        let summary = read_single_event(&root.join("splinterm/last-client-exit.json"))
            .unwrap()
            .unwrap();
        assert_eq!(summary.failure_location.as_deref(), Some(&*expected));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_clean_exit_carries_no_failure_location() {
        let root = test_directory("failure-location-clean");
        let diagnostics = ClientDiagnostics::new(root.clone()).unwrap();
        diagnostics.record_failure_location(Location::caller());
        diagnostics.finish(ExitClass::CleanUserClose, None);
        let summary = read_single_event(&root.join("splinterm/last-client-exit.json"))
            .unwrap()
            .unwrap();
        assert_eq!(summary.exit_class, Some(ExitClass::CleanUserClose));
        assert_eq!(summary.failure_location, None);
        let value = serde_json::to_value(&summary).unwrap();
        assert!(value.get("failure_location").is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn records_without_a_failure_location_still_parse() {
        let line = r#"{"schema_version":1,"timestamp_unix_ms":1,"component":"splinterm","module":"wayland","event":"wayland_failure","level":"error","pid":1,"client_instance_id":"00000000-0000-0000-0000-000000000000","build_version":"0.2.0","error_code":"wayland_dispatch"}"#;
        let event: DiagnosticEvent = serde_json::from_str(line).unwrap();
        assert_eq!(event.failure_location, None);
    }

    #[test]
    fn a_source_location_keeps_only_the_workspace_relative_path() {
        let location = Location::caller();
        assert_eq!(
            source_location(location),
            format!("crates/splinterm/src/diagnostics.rs:{}", location.line())
        );
    }
}
