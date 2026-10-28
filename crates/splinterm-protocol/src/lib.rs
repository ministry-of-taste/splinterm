//! Versioned, transport-independent messages exchanged over the local socket.
//!
//! Terminal DTOs in this crate are intentionally distinct from the borrowed
//! `splinterm-terminal` and daemon runtime representations.

pub mod handoff_continuation;
pub mod handoff_outcome;
pub mod handoff_prepare;
pub mod perf_trace;
pub mod window_resume;

use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde::{Deserialize, Serialize};
use splinterm_core::{
    Axis, DojoId, Lair, LairId, LairRetention, SplintId, SplitRatio, SplitSide, Topology,
    TopologyRevision,
};

pub const PROTOCOL_VERSION: u16 = 45;
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_TERMINAL_TRANSACTION_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_TERMINAL_TRANSACTION_CHUNK_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_TERMINAL_TRANSACTION_CHUNK_BASE64_BYTES: usize =
    4 * MAX_TERMINAL_TRANSACTION_CHUNK_BYTES.div_ceil(3);
pub const MAX_TERMINAL_TRANSACTION_CHUNKS: usize =
    MAX_TERMINAL_TRANSACTION_BYTES / MAX_TERMINAL_TRANSACTION_CHUNK_BYTES;
pub const MAX_SNAPSHOT_SCROLLBACK_ROWS: usize = 16;
pub const MAX_SCROLLBACK_PAGE_ROWS: usize = 16;
pub const MAX_SEARCH_QUERY_BYTES: usize = 256;
pub const MAX_SEARCH_RESULTS: usize = 64;
pub const MAX_AUDIT_PAGE_RECORDS: usize = 128;
pub const MAX_SEARCH_PREVIEW_BYTES: usize = 256;
pub const MAX_SEARCH_CURSOR_BYTES: usize = 32;
pub const MAX_INPUT_BYTES: usize = 64 * 1024;
pub const MAX_LAUNCH_ARGUMENTS: usize = 256;
pub const MAX_LAUNCH_ARGUMENT_BYTES: usize = 4096;
pub const MAX_CWD_BYTES: usize = 4096;
pub const MAX_SCROLLBACK_LINES: usize = 1_000_000;
pub const MAX_PRESET_DOJOS: usize = 32;
pub const MAX_PRESET_PANES_PER_DOJO: usize = 32;
pub const MAX_PRESET_TOTAL_PANES: usize = 128;
pub const MAX_PRESET_DIRECTORY_IDENTITIES: usize = 32;
pub const MAX_PRESET_DEPTH: usize = 32;
pub const MAX_PRESET_KEY_BYTES: usize = 64;
pub const MAX_PRESET_LABEL_BYTES: usize = 128;
pub const MAX_COLUMNS: u16 = 480;
pub const MAX_ROWS: u16 = 128;
pub const MAX_GRID_CELLS: usize = MAX_COLUMNS as usize * MAX_ROWS as usize;
pub const MAX_OUTSTANDING_REQUESTS: usize = 1;
pub const MAX_SUBSCRIPTIONS: usize = 4;
pub const MAX_UPDATE_ROW_PATCHES: usize = MAX_ROWS as usize;
pub const MAX_UPDATE_SCROLLS: usize = MAX_ROWS as usize;
pub const MAX_CONSENT_FRAME_BYTES: usize = 16 * 1024;
pub const CONSENT_CAPABILITY_BYTES: usize = 32;
pub const MAX_ACCESS_SCOPES: usize = 16;
/// The longest a `RequestLairAccess` call waits for the person before the
/// daemon replies `pending` (ADR 0014). Larger budgets are capped to it.
pub const MAX_CONSENT_WAIT_MILLIS: u32 = 25_000;
/// How long a pending workspace access request lives after it opens.
pub const PENDING_ACCESS_LIFETIME_SECONDS: u64 = 120;
/// The longest countdown a consent prompt or offer carries: a pending
/// workspace request never lives longer (ADR 0015).
pub const MAX_CONSENT_RESPOND_WITHIN_SECONDS: u64 = PENDING_ACCESS_LIFETIME_SECONDS;
/// Bytes in one consent offer nonce (ADR 0015). The fixed-size wire array
/// bounds decoding.
pub const CONSENT_OFFER_NONCE_BYTES: usize = 32;
/// The longest host-reported agent name a request may carry, in characters.
/// Display data only; never an identity.
pub const MAX_AGENT_NAME_CHARS: usize = 128;
/// The longest path a consent prompt shows in its shared-identity line, in
/// characters.
pub const MAX_CONSENT_PATH_CHARS: usize = 1024;
/// A consent prompt offers at most the three ADR 0014 durations.
pub const MAX_CONSENT_DURATIONS: usize = 3;
/// The most entries one access inventory page carries (ADR 0014).
pub const MAX_ACCESS_INVENTORY_PAGE: u16 = 64;
/// The longest executable path an access inventory entry carries, in bytes.
pub const MAX_ACCESS_PATH_BYTES: usize = 4096;
/// The longest agent display name an access inventory entry carries, in
/// bytes. Display data only; never an identity.
pub const MAX_ACCESS_DISPLAY_NAME_BYTES: usize = 256;
/// The longest workspace name an access inventory entry carries, in bytes.
pub const MAX_ACCESS_WORKSPACE_NAME_BYTES: usize = 1024;
/// The most policy rule IDs or resource selectors one access inventory entry
/// names (the policy file holds at most 64 rules of at most 64 selectors).
pub const MAX_ACCESS_POLICY_ITEMS: usize = 64;
/// The longest policy rule ID, in bytes.
pub const MAX_ACCESS_RULE_ID_BYTES: usize = 64;
/// The longest store failure or durable-removal message, in bytes.
pub const MAX_ACCESS_FAULT_BYTES: usize = 256;
/// A remembered entry ID: `ra_` and 32 lowercase hex digits.
pub const REMEMBERED_ENTRY_ID_BYTES: usize = 35;
pub const MAX_IMAGE_CONTENT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_IMAGE_BYTES_PER_SPLINT: usize = 32 * 1024 * 1024;
pub const MAX_IMAGE_BYTES_PER_DAEMON: usize = 64 * 1024 * 1024;
pub const MAX_IMAGE_CONTENTS_PER_SPLINT: usize = 64;
pub const MAX_IMAGE_PLACEMENTS_PER_SPLINT: usize = 256;
pub const MAX_IMAGE_DIMENSION: u32 = 4096;
pub const MAX_IMAGE_PIXELS: usize = 4_194_304;
pub const MAX_IMAGE_CHUNK_BYTES: usize = 64 * 1024;
pub const MAX_IMAGE_CHUNK_WINDOW: usize = 4;
pub const MAX_IMAGE_TRANSFERS_PER_SPLINT: usize = 2;
pub const MAX_IMAGE_TRANSFERS_PER_DAEMON: usize = 4;
pub const IMAGE_TRANSFER_TOKEN_BYTES: usize = 32;
pub const IMAGE_TRANSFER_TOKEN_TTL_MILLIS: u32 = 5_000;
pub const MAX_AGENT_STATUS_DETAIL_SCALARS: usize = 80;
pub const MAX_AGENT_STATUS_DETAIL_BYTES: usize = 240;
pub const MIN_AGENT_STATUS_TTL_SECONDS: u32 = 10;
pub const MAX_AGENT_STATUS_TTL_SECONDS: u32 = 900;
pub const DEFAULT_AGENT_STATUS_ACTIVE_TTL_SECONDS: u32 = 120;
/// Bytes in one opaque, single-use, generation-bound handoff resume ticket
/// (ADR 0011). The fixed-size wire array bounds decoding.
pub const HANDOFF_RESUME_TICKET_BYTES: usize = 32;
/// Bytes in one immutable daemon/client pair build identity.
pub const HANDOFF_BUILD_IDENTITY_BYTES: usize = 32;
/// Maximum UTF-8 byte length of a reported daemon build version.
pub const MAX_HANDOFF_BUILD_VERSION_BYTES: usize = 64;
/// Maximum blocking reasons in one handoff report: at most one of each kind.
pub const MAX_HANDOFF_BLOCKING_REASONS: usize = 4;
/// Maximum transient Lair identities one blocking reason names; its count
/// stays exact when more are open.
pub const MAX_HANDOFF_REPORTED_LAIRS: usize = 16;
/// Maximum controllers one handoff control claim restores: one per live
/// Splint a daemon can own.
pub const MAX_HANDOFF_CLAIMED_CONTROLLERS: usize = 256;
/// Exact descriptor count of a [`ServerFrame::HandoffPrepare`] frame: the
/// sealed forward client snapshot, then the client end of the continuation
/// channel.
pub const HANDOFF_PREPARE_DESCRIPTORS: usize = 2;
/// Largest sealed forward client snapshot a handoff may pass.
pub const MAX_HANDOFF_CLIENT_SNAPSHOT_BYTES: u64 = 256 * 1024 * 1024;
/// Maximum trusted local Windows one handoff names, in a
/// [`ServerFrame::HandoffDecisionRequired`] prompt or its answer. Each Window
/// is named by an index below this bound.
pub const MAX_HANDOFF_WINDOWS: usize = 64;
/// Maximum Dojos a decision prompt names for one Window: the open Dojos one
/// Window resume record may carry.
pub const MAX_HANDOFF_WINDOW_DOJOS: usize = 32;
/// Maximum UTF-8 byte length of a Dojo name in a decision prompt, the
/// topology's own name bound.
pub const MAX_HANDOFF_DOJO_NAME_BYTES: usize = 128;
/// Longest a daemon waits for a [`ClientFrame::HandoffDecision`] answer.
pub const MAX_HANDOFF_DECISION_DEADLINE_MS: u32 = 60_000;

#[must_use]
pub fn image_content_socket_path(control_socket: &Path) -> PathBuf {
    let mut name = control_socket.as_os_str().to_os_string();
    name.push(".content");
    PathBuf::from(name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientRole {
    TrustedUi,
    RemoteInteractive,
    Automation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticCorrelation {
    pub client_instance_id: uuid::Uuid,
    pub window_id: uuid::Uuid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonDiagnosticComponent {
    Splinterd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonDiagnosticLevel {
    Info,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonDiagnosticEventCode {
    LairCreated,
    SplintSplit,
    DojoCreated,
    PresetMaterialized,
    SplintClosed,
    DojoClosed,
    LairTerminated,
    LairRenamed,
    DojoRenamed,
    TopologyRestored,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonDiagnosticEvent {
    pub schema_version: u16,
    pub timestamp_unix_ms: u64,
    pub component: DaemonDiagnosticComponent,
    pub event: DaemonDiagnosticEventCode,
    pub level: DaemonDiagnosticLevel,
    pub pid: u32,
    pub client_instance_id: uuid::Uuid,
    pub window_id: uuid::Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topology_revision: Option<TopologyRevision>,
    pub build_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_commit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    Hello {
        minimum_version: u16,
        maximum_version: u16,
        role: ClientRole,
        /// Declares this connection the anchor of a trusted local Window: the
        /// one connection a handing-off daemon sends that Window's
        /// [`ServerFrame::HandoffPrepare`] on. Omitted when false.
        ///
        /// A client must not set it before it can receive that frame's
        /// descriptors (Plan 0061 M3.14). Until then any `HandoffPrepare`
        /// ends the client's connection, so a declared Window would lose its
        /// anchor in every handoff instead of being listed as undeclared.
        ///
        /// The daemon accepts it only with the trusted UI role from the
        /// installed graphical client, at most once per Window process and
        /// for at most [`MAX_HANDOFF_WINDOWS`] Windows, and never while a
        /// handoff already holds its Windows; any other declaration fails
        /// the handshake. Only the anchor may acknowledge its prepare frame
        /// ([`Request::AcknowledgeHandoffPrepare`]), so an anchor connection
        /// cannot itself start a handoff: `PrepareDaemonHandoff { Execute }`
        /// on it fails with [`ErrorCode::InvalidArgument`] before anything
        /// begins, while a dry run is answered. The initiator uses a separate
        /// connection.
        #[serde(default, skip_serializing_if = "bool_is_false")]
        handoff_window: bool,
    },
    Request {
        request_id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diagnostic_correlation: Option<DiagnosticCorrelation>,
        request: Request,
    },
    Cancel {
        request_id: u64,
    },
    /// Answers the [`ServerFrame::HandoffDecisionRequired`] prompt for the
    /// in-flight request `request_id`: proceed with the handoff without the
    /// Windows whose prompt indices `proceed_without` lists. Those Windows
    /// keep no continuity and reconnect after the handoff. To cancel the
    /// handoff instead, the client sends [`ClientFrame::Cancel`] for the
    /// request. Any other use is a protocol violation; see
    /// [`validate_handoff_decision`].
    HandoffDecision {
        request_id: u64,
        #[serde(deserialize_with = "deserialize_handoff_window_indices")]
        proceed_without: Vec<u8>,
    },
}

/// Maximum UTF-8 byte length of informational daemon hostname metadata.
pub const MAX_DAEMON_HOSTNAME_BYTES: usize = 255;

/// The longest remote profile name, in bytes.
pub const MAX_REMOTE_PROFILE_NAME_BYTES: usize = 64;

/// Whether `name` is a usable remote profile name: 1 to
/// [`MAX_REMOTE_PROFILE_NAME_BYTES`] bytes of ASCII letters, digits, `.`,
/// `_`, or `-`, not starting with `-` (so it can never read as an SSH
/// option). Such a name has no control, whitespace, or bidi characters.
#[must_use]
pub fn usable_remote_profile_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_REMOTE_PROFILE_NAME_BYTES
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Accept only bounded hostname-like text. Reject controls, whitespace, formatting
/// and bidi characters rather than attempting to repair an untrusted identity.
#[must_use]
pub fn usable_daemon_hostname(value: &str) -> Option<&str> {
    (!value.is_empty()
        && value.len() <= MAX_DAEMON_HOSTNAME_BYTES
        && value
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_')))
    .then_some(value)
}

fn deserialize_daemon_hostname<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // Frame decoding is transport-bounded; retain only usable, bounded metadata.
    let value = Option::<String>::deserialize(deserializer)?;
    Ok(value.filter(|hostname| usable_daemon_hostname(hostname).is_some()))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    Hello {
        version: u16,
        limits: ServerLimits,
        development_terminal_access: bool,
        /// Informational OS hostname at connection time, never authentication authority.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_daemon_hostname"
        )]
        daemon_hostname: Option<String>,
        /// Generation of the daemon serving this connection. A fresh daemon
        /// start is generation 1; each adopted handoff publishes a later one.
        daemon_generation: u64,
    },
    Response {
        request_id: u64,
        result: Response,
    },
    Event {
        subscription_id: u64,
        sequence: u64,
        event: SubscriptionEvent,
    },
    Error {
        request_id: Option<u64>,
        error: ProtocolError,
    },
    TerminalTransactionStart {
        total_bytes: usize,
        chunks: usize,
    },
    TerminalTransactionChunk {
        index: usize,
        data: String,
    },
    /// Sent by a handing-off daemon to an eligible trusted local Window with
    /// exactly [`HANDOFF_PREPARE_DESCRIPTORS`] descriptors attached through
    /// `SCM_RIGHTS`; see [`handoff_prepare`]. It is valid only inside a
    /// handoff the Window participates in; any other receipt is a protocol
    /// violation.
    HandoffPrepare {
        prepare: HandoffPrepare,
    },
    /// Sent on the initiating trusted UI's connection while its
    /// `PrepareDaemonHandoff { Execute }` request `request_id` is in flight,
    /// when trusted local Windows cannot take part in the handoff (operator
    /// decision D10). The fence stays up while the daemon waits up to
    /// `deadline_ms` for one [`ClientFrame::HandoffDecision`] naming every
    /// listed Window, or a [`ClientFrame::Cancel`]; no answer cancels the
    /// handoff with `TimedOut`. A client with no such request in flight
    /// treats it as a protocol violation. See
    /// [`validate_handoff_decision_required`].
    HandoffDecisionRequired {
        request_id: u64,
        /// Generation of the daemon that is handing off.
        daemon_generation: u64,
        /// Milliseconds the daemon waits for the answer after sending this
        /// frame, at most [`MAX_HANDOFF_DECISION_DEADLINE_MS`].
        deadline_ms: u32,
        #[serde(deserialize_with = "deserialize_unresponsive_windows")]
        windows: Vec<HandoffUnresponsiveWindow>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageServerCapabilities {
    pub metadata_version: u16,
    pub binary_chunks: bool,
    pub sealed_memfd: bool,
    pub maximum_content_bytes: usize,
    pub maximum_bytes_per_splint: usize,
    pub maximum_bytes_per_daemon: usize,
    pub maximum_contents_per_splint: usize,
    pub maximum_placements_per_splint: usize,
    pub maximum_dimension: u32,
    pub maximum_pixels: usize,
    pub maximum_chunk_bytes: usize,
    pub maximum_chunk_window: usize,
    pub maximum_transfers_per_splint: usize,
    pub maximum_transfers_per_daemon: usize,
}

impl Default for ImageServerCapabilities {
    fn default() -> Self {
        Self {
            metadata_version: 1,
            binary_chunks: true,
            sealed_memfd: cfg!(target_os = "linux"),
            maximum_content_bytes: MAX_IMAGE_CONTENT_BYTES,
            maximum_bytes_per_splint: MAX_IMAGE_BYTES_PER_SPLINT,
            maximum_bytes_per_daemon: MAX_IMAGE_BYTES_PER_DAEMON,
            maximum_contents_per_splint: MAX_IMAGE_CONTENTS_PER_SPLINT,
            maximum_placements_per_splint: MAX_IMAGE_PLACEMENTS_PER_SPLINT,
            maximum_dimension: MAX_IMAGE_DIMENSION,
            maximum_pixels: MAX_IMAGE_PIXELS,
            maximum_chunk_bytes: MAX_IMAGE_CHUNK_BYTES,
            maximum_chunk_window: MAX_IMAGE_CHUNK_WINDOW,
            maximum_transfers_per_splint: MAX_IMAGE_TRANSFERS_PER_SPLINT,
            maximum_transfers_per_daemon: MAX_IMAGE_TRANSFERS_PER_DAEMON,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerLimits {
    pub maximum_frame_bytes: usize,
    pub maximum_input_bytes: usize,
    pub maximum_columns: u16,
    pub maximum_rows: u16,
    pub maximum_outstanding_requests: usize,
    pub maximum_subscriptions: usize,
    pub maximum_snapshot_scrollback_rows: usize,
    pub image: ImageServerCapabilities,
}

impl Default for ServerLimits {
    fn default() -> Self {
        Self {
            maximum_frame_bytes: MAX_FRAME_BYTES,
            maximum_input_bytes: MAX_INPUT_BYTES,
            maximum_columns: MAX_COLUMNS,
            maximum_rows: MAX_ROWS,
            maximum_outstanding_requests: MAX_OUTSTANDING_REQUESTS,
            maximum_subscriptions: MAX_SUBSCRIPTIONS,
            maximum_snapshot_scrollback_rows: MAX_SNAPSHOT_SCROLLBACK_ROWS,
            image: ImageServerCapabilities::default(),
        }
    }
}

impl ServerLimits {
    /// Validates the exact-version terminal transport envelope before a client
    /// uses any advertised dimensions or frame budget.
    ///
    /// # Errors
    /// Returns `InvalidArgument` when the fixed frame limit, input limit, or
    /// endpoint-local grid bounds are outside the protocol's absolute envelope.
    pub fn validate_terminal_transport(self) -> Result<(), ProtocolError> {
        if self.maximum_frame_bytes != MAX_FRAME_BYTES {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "server frame limit does not match the protocol version",
            ));
        }
        if !(1..=MAX_INPUT_BYTES).contains(&self.maximum_input_bytes) {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "server input limit is outside the protocol envelope",
            ));
        }
        if !(2..=MAX_COLUMNS).contains(&self.maximum_columns)
            || !(2..=MAX_ROWS).contains(&self.maximum_rows)
        {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "server terminal dimensions exceed protocol limits",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationLaunch {
    pub cwd: Option<PathBuf>,
    /// Direct executable plus argv. Empty selects the daemon-owned default shell.
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum MutationPreflight {
    CreateLair,
    SplitSplint {
        splint_id: SplintId,
    },
    NewDojo {
        lair_id: LairId,
    },
    RelaunchSplint {
        splint_id: SplintId,
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
    CloseSplint {
        splint_id: SplintId,
    },
    CloseDojo {
        dojo_id: DojoId,
    },
    TerminateLair {
        lair_id: LairId,
    },
    KillSplint {
        splint_id: SplintId,
        incarnation: u64,
    },
    SetSplitRatio {
        splint_id: SplintId,
    },
    RenameLair {
        lair_id: LairId,
    },
    RenameDojo {
        dojo_id: DojoId,
    },
    RenameSplint {
        splint_id: SplintId,
    },
    SetDojoDefaultFocus {
        dojo_id: DojoId,
        splint_id: SplintId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationTarget {
    pub lair_id: LairId,
    pub dojo_id: DojoId,
    pub splint_id: SplintId,
    /// Current incarnation when live, otherwise the last durable incarnation.
    pub incarnation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationPreparation {
    pub topology_revision: TopologyRevision,
    pub lair_id: Option<LairId>,
    pub dojo_id: Option<DojoId>,
    pub splint_id: Option<SplintId>,
    pub incarnation: Option<u64>,
    /// Exact bounded expansion for aggregate restore validation.
    pub targets: Vec<MutationTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PresetTarget {
    NewLair {
        name: String,
    },
    ExistingLair {
        lair_id: LairId,
        rename: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PresetLayoutLaunch {
    Pane {
        key: String,
        title: String,
        launch: LaunchParameters,
    },
    Split {
        axis: Axis,
        ratio: SplitRatio,
        first: Box<Self>,
        second: Box<Self>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetDojoLaunch {
    pub name: String,
    pub focus_key: String,
    pub root: PresetLayoutLaunch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetDirectoryIdentity {
    pub path: PathBuf,
    pub device: u64,
    pub inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetPaneIdentity {
    pub dojo_id: DojoId,
    pub key: String,
    pub splint_id: SplintId,
}

/// Where a Splint, Dojo, or Lair created from inside a Window starts.
///
/// Every choice is resolved on the daemon that owns the source Splint, so a
/// remote Window resolves remote paths and the remote owner's home.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "path", rename_all = "snake_case")]
pub enum LaunchDirectory {
    /// The source Splint's live root-process directory, then its saved launch
    /// directory, then the owner's home.
    Inherit,
    /// The daemon owner's home directory.
    Home,
    /// An absolute directory; falls back to the owner's home when unusable.
    Absolute(PathBuf),
    /// A directory below the owner's home; falls back to the home itself.
    HomeRelative(PathBuf),
}

impl LaunchDirectory {
    /// Validates path shape without touching the filesystem.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` for an empty, oversized, NUL-bearing, or
    /// wrongly anchored path, or a home-relative path that leaves the home.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let valid = match self {
            Self::Inherit | Self::Home => true,
            Self::Absolute(path) => bounded_path(path) && path.is_absolute(),
            Self::HomeRelative(path) => {
                bounded_path(path)
                    && path
                        .components()
                        .all(|component| matches!(component, std::path::Component::Normal(_)))
            }
        };
        if !valid {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "launch directory is not a bounded absolute or home-relative path",
            ));
        }
        Ok(())
    }
}

fn bounded_path(path: &std::path::Path) -> bool {
    let bytes = path.as_os_str().as_encoded_bytes();
    !bytes.is_empty() && !bytes.contains(&0) && bytes.len() <= MAX_CWD_BYTES
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Ping,
    ListLairs,
    InspectTopology,
    SubscribeTopology,
    InspectSplint {
        splint_id: SplintId,
    },
    /// Resolves where a Window-created Splint, Dojo, or Lair starts, on the
    /// daemon that owns the source Splint.
    ResolveLaunchDirectory {
        source_splint_id: SplintId,
        directory: LaunchDirectory,
    },
    ReadGraphicalFocus,
    PublishGraphicalFocus {
        focused_splint_id: Option<SplintId>,
    },
    RequestAccess {
        splint_id: SplintId,
        incarnation: u64,
        scopes: Vec<AccessScope>,
    },
    /// Asks for access to one workspace (ADR 0014): exactly one of `level`,
    /// which the daemon expands to its exact bundle, or an explicit `scopes`
    /// set. A request a grant already covers is answered at once. Otherwise
    /// it becomes a bounded pending request whose prompt the daemon shows,
    /// and this call waits at most `wait_millis` (capped at
    /// [`MAX_CONSENT_WAIT_MILLIS`]) before replying
    /// [`Response::LairAccessPending`]. Repeating the same request joins it.
    RequestLairAccess {
        lair_id: LairId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        level: Option<AccessLevel>,
        #[serde(
            default,
            skip_serializing_if = "Vec::is_empty",
            deserialize_with = "deserialize_access_scopes"
        )]
        scopes: Vec<AccessScope>,
        wait_millis: u32,
        /// The host-reported agent name, at most [`MAX_AGENT_NAME_CHARS`]
        /// characters: shown to the person as data, never an identity.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_name: Option<String>,
    },
    /// Cancels the requesting process's own pending request for this
    /// workspace.
    CancelLairAccessRequest {
        lair_id: LairId,
        request_id: u64,
    },
    /// Trusted UI only (**Revoke all access**): ends every pending workspace
    /// request without a grant.
    CancelAllLairAccessRequests,
    AuthorizationStatus {
        splint_id: SplintId,
        incarnation: Option<u64>,
    },
    RevokeAccess {
        grant_id: u64,
    },
    /// One page of the daemon-wide access inventory (ADR 0014): every grant
    /// definition of every duration, pending requests, and the policy rules
    /// that match a connected agent. Entries come in [`AccessEntryKey`]
    /// order; `after` continues after the last key of the previous page.
    /// Trusted UI, or `authorization_inspect` on the daemon from policy.
    ListAccess {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<AccessEntryKey>,
        /// At most [`MAX_ACCESS_INVENTORY_PAGE`].
        max_entries: u16,
    },
    /// Revokes one grant definition of any duration at once, cancelling its
    /// subscriptions and releasing its controllers; a remembered entry is
    /// also removed durably. Trusted UI, or `authorization_revoke` from
    /// policy on the grant's workspace.
    RevokeAccessEntry {
        target: AccessRevokeTarget,
    },
    /// **Revoke all access**: every consent-derived grant of every duration
    /// and every pending request; never a policy rule or the trusted UI.
    /// `dry_run` only counts, so the person can confirm the exact number of
    /// remembered grants first. Trusted UI, or `authorization_revoke` on the
    /// daemon from policy.
    RevokeAllAccess {
        dry_run: bool,
    },
    PrepareMutation {
        mutation: MutationPreflight,
    },
    CreateLairAutomation {
        expected_topology_revision: TopologyRevision,
        name: String,
        launch: AutomationLaunch,
        /// Set only when Splinterm generated `name`; see [`Request::CreateLair`].
        #[serde(default, skip_serializing_if = "bool_is_false")]
        generated_name: bool,
    },
    SplitSplintAutomation {
        expected_topology_revision: TopologyRevision,
        target_splint_id: SplintId,
        axis: Axis,
        side: SplitSide,
        ratio: SplitRatio,
        launch: AutomationLaunch,
    },
    RelaunchSplintAutomation {
        expected_topology_revision: TopologyRevision,
        splint_id: SplintId,
        launch: AutomationLaunch,
    },
    NewDojoAutomation {
        expected_topology_revision: TopologyRevision,
        lair_id: LairId,
        name: String,
        launch: AutomationLaunch,
        /// Set only when Splinterm generated `name`; see [`Request::NewDojo`].
        #[serde(default, skip_serializing_if = "bool_is_false")]
        generated_name: bool,
    },
    CreateLair {
        expected_topology_revision: TopologyRevision,
        name: String,
        launch: LaunchParameters,
        /// Set only when Splinterm generated `name` rather than the user
        /// choosing it; an unused close may then remove the Lair with its
        /// last Dojo.
        #[serde(default, skip_serializing_if = "bool_is_false")]
        generated_name: bool,
    },
    CreateTransientLair {
        expected_topology_revision: TopologyRevision,
        name: String,
        launch: LaunchParameters,
        /// Set only when Splinterm generated `name` rather than the user
        /// choosing it; an unused close may then remove the Lair with its
        /// last Dojo.
        #[serde(default, skip_serializing_if = "bool_is_false")]
        generated_name: bool,
    },
    SplitSplint {
        expected_topology_revision: TopologyRevision,
        target_splint_id: SplintId,
        axis: Axis,
        side: SplitSide,
        ratio: SplitRatio,
        launch: LaunchParameters,
    },
    RelaunchSplint {
        expected_topology_revision: TopologyRevision,
        splint_id: SplintId,
        launch: LaunchParameters,
    },
    RestoreSplint {
        expected_topology_revision: TopologyRevision,
        splint_id: SplintId,
    },
    RestoreDojo {
        expected_topology_revision: TopologyRevision,
        dojo_id: DojoId,
    },
    RestoreLair {
        expected_topology_revision: TopologyRevision,
        lair_id: LairId,
    },
    SetLairRetention {
        expected_topology_revision: TopologyRevision,
        lair_id: LairId,
        retention: LairRetention,
    },
    CloseSplint {
        expected_topology_revision: TopologyRevision,
        splint_id: SplintId,
    },
    SetSplitRatio {
        expected_topology_revision: TopologyRevision,
        target_splint_id: SplintId,
        /// Zero selects the immediate parent; larger values select successively
        /// older ancestors of the target leaf.
        ancestor: u16,
        ratio: SplitRatio,
    },
    NewDojo {
        expected_topology_revision: TopologyRevision,
        lair_id: LairId,
        name: String,
        launch: LaunchParameters,
        /// Trusted owner request to atomically make a transient Lair durable.
        #[serde(default, skip_serializing_if = "bool_is_false")]
        promote_transient_lair: bool,
        /// Set only when Splinterm generated `name` rather than the user
        /// choosing it; a chosen name keeps the Dojo from an unused close.
        #[serde(default, skip_serializing_if = "bool_is_false")]
        generated_name: bool,
    },
    MaterializePreset {
        expected_topology_revision: TopologyRevision,
        target: PresetTarget,
        dojos: Vec<PresetDojoLaunch>,
        directory_identities: Vec<PresetDirectoryIdentity>,
    },
    CloseDojo {
        expected_topology_revision: TopologyRevision,
        dojo_id: DojoId,
    },
    /// Terminates the Dojo only when it was never used. The daemon decides and
    /// applies this atomically against its current state; a used Dojo is left
    /// unchanged.
    CloseUnusedDojo {
        dojo_id: DojoId,
    },
    TerminateLair {
        expected_topology_revision: TopologyRevision,
        lair_id: LairId,
        /// Exact captured membership and process incarnations for drift rejection.
        targets: Vec<MutationTarget>,
    },
    RenameLair {
        expected_topology_revision: TopologyRevision,
        lair_id: LairId,
        name: String,
    },
    RenameDojo {
        expected_topology_revision: TopologyRevision,
        dojo_id: DojoId,
        name: String,
        /// Trusted owner request to atomically make a transient Lair durable.
        #[serde(default, skip_serializing_if = "bool_is_false")]
        promote_transient_lair: bool,
    },
    SetDojoDefaultFocus {
        expected_topology_revision: TopologyRevision,
        dojo_id: DojoId,
        splint_id: SplintId,
    },
    RenameSplint {
        expected_topology_revision: TopologyRevision,
        splint_id: SplintId,
        title: String,
    },
    Attach {
        splint_id: SplintId,
        /// Exact incarnation, or `None` to bind the current incarnation before authorization.
        incarnation: Option<u64>,
        scrollback_rows: usize,
    },
    RequestImageContent {
        request: ImageContentRequest,
    },
    StartScrollbackPage {
        splint_id: SplintId,
        /// Exact incarnation, or `None` to bind the current incarnation before authorization.
        incarnation: Option<u64>,
        max_rows: usize,
    },
    ScrollbackPage {
        splint_id: SplintId,
        incarnation: u64,
        terminal_revision: u64,
        history_generation: u64,
        before_row_id: u64,
        max_rows: usize,
    },
    StartSearchScrollback {
        splint_id: SplintId,
        /// Exact incarnation, or `None` to bind the current incarnation before authorization.
        incarnation: Option<u64>,
        query: String,
        case_sensitive: bool,
        max_results: usize,
    },
    SearchScrollback {
        splint_id: SplintId,
        incarnation: u64,
        terminal_revision: u64,
        history_generation: u64,
        query: String,
        case_sensitive: bool,
        cursor: Option<String>,
        max_results: usize,
    },
    AcquireControl {
        splint_id: SplintId,
        incarnation: u64,
        modes: Vec<ControlMode>,
    },
    SubscribeControl {
        splint_id: SplintId,
        incarnation: u64,
    },
    RequestControlTransfer {
        splint_id: SplintId,
        incarnation: u64,
        modes: Vec<ControlMode>,
    },
    DecideControlTransfer {
        transfer_id: u64,
        decision: ControlTransferDecision,
    },
    ForceControlTransfer {
        splint_id: SplintId,
        incarnation: u64,
    },
    ReleaseControl {
        controller_id: u64,
    },
    Input {
        controller_id: u64,
        splint_id: SplintId,
        incarnation: u64,
        bytes: Vec<u8>,
        /// Set only by the graphical client for the focus in and out reports
        /// it generates under DECSET 1004. Such bytes leave a Dojo unused;
        /// identical bytes sent without this flag count as use.
        #[serde(default, skip_serializing_if = "bool_is_false")]
        focus_report: bool,
    },
    Resize {
        controller_id: u64,
        splint_id: SplintId,
        incarnation: u64,
        columns: u16,
        rows: u16,
        pixel_width: u16,
        pixel_height: u16,
    },
    Detach {
        subscription_id: u64,
    },
    KillSplint {
        splint_id: SplintId,
        incarnation: u64,
    },
    AuditInspect {
        after_audit_id: Option<u64>,
        max_records: usize,
    },
    /// Explicitly reported display status for one live Splint incarnation.
    /// `None` clears the status. Never inferred from terminal content.
    ReportAgentStatus {
        splint_id: SplintId,
        incarnation: u64,
        status: Option<AgentStatusReport>,
    },
    /// Trusted local graphical UI only. `DryRun` reports whether this daemon
    /// could hand off to its installed build without fencing or capturing
    /// anything; `Execute` starts a handoff, or fails with
    /// [`ErrorCode::HandoffUnavailable`] naming every blocking reason.
    PrepareDaemonHandoff {
        mode: DaemonHandoffMode,
    },
    /// Trusted local graphical UI only. A relaunched Window restores one
    /// pre-fence controller with the resume ticket the adopted daemon issued
    /// it over its continuation channel (ADR 0011; Plan 0061 M3.16).
    ///
    /// A Window holds one control connection per pane and a controller lease
    /// is bound to its connection, so each pane's control connection sends
    /// its own claim naming its Splint in `splint`. The ticket is single-use
    /// per held controller: each controller it carries is restored at most
    /// once, and the ticket ends with its last one or with the daemon's hold.
    /// A claim from any other process than the Window the ticket was issued
    /// to, or one not executing the pinned forward client snapshot, ends it.
    ///
    /// An invalid, expired, ended, transferred, or replayed ticket fails with
    /// [`ErrorCode::InvalidHandoffTicket`]; a claim without `splint` fails
    /// with [`ErrorCode::InvalidArgument`]. A valid ticket naming a Splint it
    /// holds no controller for, whose incarnation changed, or that has a
    /// controller now restores nothing and answers an empty
    /// [`Response::HandoffControlClaimed`].
    ClaimHandoffControl {
        ticket: HandoffResumeTicket,
        /// The Splint whose controller this connection restores. Optional on
        /// the wire only so a protocol 42 peer that omits it still decodes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        splint: Option<HandoffClaimTarget>,
    },
    /// Trusted local graphical UI only. A Window that received a
    /// [`ServerFrame::HandoffPrepare`] frame acknowledges it once it holds
    /// the descriptors and its resume record, echoing the frame's generation
    /// and `prepare_id`, on the anchor connection the frame came on
    /// (`Hello.handoff_window`). The daemon authorizes it by that binding:
    /// that connection only, once, while the daemon waits for exactly that
    /// prepare, from the same pinned process. It does not re-check the
    /// installed client, because after a package upgrade every running Window
    /// executes the replaced image. A zero generation or id is
    /// [`ErrorCode::InvalidArgument`] ([`validate_handoff_acknowledgement`]).
    /// An acknowledgement that matches no pending handoff prepare, because
    /// none was sent, it was cancelled or already acknowledged, the wait
    /// ended, or the generation or id differs (a late answer to an earlier
    /// attempt's frame among them), fails with [`ErrorCode::NotFound`] and
    /// changes nothing.
    AcknowledgeHandoffPrepare {
        daemon_generation: u64,
        prepare_id: u64,
    },
    /// Registers this connection as a consent responder (ADR 0015): the
    /// daemon sends it [`SubscriptionEvent::ConsentOffered`] for each
    /// workspace access prompt it may answer, and
    /// [`SubscriptionEvent::ConsentOfferWithdrawn`] when one ends unanswered.
    ///
    /// A [`ConsentResponderKind::Window`] responder is a remote Window's
    /// graphical relay connection; a [`ConsentResponderKind::Terminal`]
    /// responder is the approver command from the installed client.
    /// Automation is never a responder. Responder status belongs to this
    /// connection only.
    SubscribeConsentOffers {
        kind: ConsentResponderKind,
    },
    /// Answers one [`ConsentOffer`] sent on this connection, echoing its
    /// generation, id, and nonce; see [`validate_consent_answer`]. The first
    /// valid answer from any route settles the request; every other answer,
    /// a duplicate, a late answer, or one on another connection fails with
    /// [`ErrorCode::NotFound`] and grants nothing.
    AnswerConsentOffer {
        daemon_generation: u64,
        offer_id: u64,
        nonce: ConsentOfferNonce,
        decision: ConsentDecision,
    },
}

/// Validates one [`Request::AnswerConsentOffer`]: a nonzero generation and
/// offer id.
///
/// # Errors
/// Returns `InvalidArgument` when either is zero.
pub fn validate_consent_answer(daemon_generation: u64, offer_id: u64) -> Result<(), ProtocolError> {
    if daemon_generation == 0 || offer_id == 0 {
        return Err(ProtocolError::new(
            ErrorCode::InvalidArgument,
            "a consent answer names a zero generation or offer",
        ));
    }
    Ok(())
}

/// Who answers a [`Request::SubscribeConsentOffers`] responder's offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentResponderKind {
    /// A remote graphical Window over the SSH graphical relay.
    Window,
    /// The `splinterm approvals` command in the person's own terminal.
    Terminal,
}

/// One single-use, offer-bound consent offer nonce (ADR 0015).
///
/// On the wire it is exactly [`CONSENT_OFFER_NONCE_BYTES`] bytes, so
/// decoding rejects any other length. `Debug` never prints the bytes, and
/// equality does not stop at the first differing byte.
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConsentOfferNonce([u8; CONSENT_OFFER_NONCE_BYTES]);

impl ConsentOfferNonce {
    #[must_use]
    pub const fn new(bytes: [u8; CONSENT_OFFER_NONCE_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; CONSENT_OFFER_NONCE_BYTES] {
        &self.0
    }
}

impl PartialEq for ConsentOfferNonce {
    fn eq(&self, other: &Self) -> bool {
        self.0
            .iter()
            .zip(other.0.iter())
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            })
            == 0
    }
}

impl Eq for ConsentOfferNonce {}

impl std::fmt::Debug for ConsentOfferNonce {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "ConsentOfferNonce(<{CONSENT_OFFER_NONCE_BYTES} bytes redacted>)"
        )
    }
}

/// One workspace access prompt the daemon offers a consent responder
/// (ADR 0015), bound to its generation, offer, nonce, and connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentOffer {
    pub daemon_generation: u64,
    /// Monotonic and never reused within a daemon generation.
    pub offer_id: u64,
    /// The pending request's agent-visible ID.
    pub request_id: u64,
    pub nonce: ConsentOfferNonce,
    /// Seconds the responder has to answer, at least one and at most
    /// [`MAX_CONSENT_RESPOND_WITHIN_SECONDS`].
    pub respond_within_seconds: u64,
    pub prompt: OfferedConsentPrompt,
}

impl ConsentOffer {
    /// # Errors
    /// Returns `InvalidArgument` for a zero generation or offer id, a
    /// countdown outside one to [`MAX_CONSENT_RESPOND_WITHIN_SECONDS`]
    /// seconds, or an invalid prompt.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.daemon_generation == 0 || self.offer_id == 0 {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "a consent offer names a zero generation or offer",
            ));
        }
        if !(1..=MAX_CONSENT_RESPOND_WITHIN_SECONDS).contains(&self.respond_within_seconds) {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "a consent offer's countdown is outside the pending request lifetime",
            ));
        }
        self.prompt
            .validate()
            .map_err(|message| ProtocolError::new(ErrorCode::InvalidArgument, message))
    }
}

/// Why the daemon withdrew a [`ConsentOffer`] before this responder
/// answered it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentOfferEnd {
    /// The agent cancelled, its process exited, or the person revoked all
    /// access.
    Cancelled,
    /// The request's time ran out.
    Expired,
    /// The request ended otherwise, for example because its workspace ended.
    RequestEnded,
    /// A daemon handoff fenced the daemon.
    Fenced,
    /// Another route answered first.
    AnsweredElsewhere,
}

/// The Splint a [`Request::ClaimHandoffControl`] restores a controller for:
/// the live incarnation the claiming pane attached to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffClaimTarget {
    pub splint_id: SplintId,
    pub incarnation: u64,
}

/// One opaque, single-use, generation-bound handoff resume ticket (ADR 0011).
///
/// On the wire it is exactly [`HANDOFF_RESUME_TICKET_BYTES`] bytes, so
/// decoding rejects any other length. `Debug` never prints the bytes.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HandoffResumeTicket([u8; HANDOFF_RESUME_TICKET_BYTES]);

impl HandoffResumeTicket {
    #[must_use]
    pub const fn new(bytes: [u8; HANDOFF_RESUME_TICKET_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; HANDOFF_RESUME_TICKET_BYTES] {
        &self.0
    }
}

impl std::fmt::Debug for HandoffResumeTicket {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "HandoffResumeTicket(<{HANDOFF_RESUME_TICKET_BYTES} bytes redacted>)"
        )
    }
}

/// What [`Request::PrepareDaemonHandoff`] asks the daemon to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonHandoffMode {
    /// Report compatibility and blocking reasons; change nothing.
    DryRun,
    /// Start the handoff when nothing blocks it.
    Execute,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Pong,
    MutationPrepared {
        preparation: MutationPreparation,
    },
    Lairs {
        lairs: Vec<Lair>,
        topology_revision: TopologyRevision,
    },
    LairCreated {
        lair: Lair,
        incarnation: u64,
        topology_revision: TopologyRevision,
    },
    SplintStarted {
        splint_id: SplintId,
        incarnation: u64,
        topology_revision: TopologyRevision,
    },
    DojoStarted {
        dojo_id: DojoId,
        splint_id: SplintId,
        incarnation: u64,
        topology_revision: TopologyRevision,
    },
    PresetMaterialized {
        lair_id: LairId,
        dojo_ids: Vec<DojoId>,
        panes: Vec<PresetPaneIdentity>,
        topology_revision: TopologyRevision,
    },
    TopologyCommitted {
        topology_revision: TopologyRevision,
    },
    RestoreCompleted {
        topology_revision: TopologyRevision,
        results: Vec<RestoreLeafResult>,
    },
    Topology {
        snapshot: TopologySnapshot,
    },
    TopologySubscribed {
        subscription_id: u64,
        snapshot: TopologySnapshot,
    },
    Splint {
        lair_id: LairId,
        dojo_id: DojoId,
        title: String,
        topology_revision: TopologyRevision,
        runtime: SplintRuntimeSummary,
        /// Point-in-time launch default resolved on the owning daemon, not durable topology.
        resolved_cwd: Option<PathBuf>,
    },
    LaunchDirectoryResolved {
        source_splint_id: SplintId,
        /// Point-in-time usable directory, or `None` when neither the requested
        /// directory nor its fallbacks are usable.
        cwd: Option<PathBuf>,
    },
    GraphicalFocus {
        focused_splint_id: Option<SplintId>,
        cwd: Option<PathBuf>,
    },
    AccessGranted {
        lair_id: LairId,
        dojo_id: DojoId,
        authorization_revision: u64,
        grant: AccessGrant,
    },
    LairAccessGranted {
        topology_revision: TopologyRevision,
        authorization_revision: u64,
        grant: LairAccessGrant,
    },
    AccessRevoked {
        lair_id: LairId,
        dojo_id: DojoId,
        authorization_revision: u64,
        grant: AccessGrant,
    },
    LairAccessRevoked {
        topology_revision: TopologyRevision,
        authorization_revision: u64,
        grant: LairAccessGrant,
    },
    /// The person has not answered yet. The request stays pending until
    /// `expires_at_unix_seconds`; repeating it collects the answer.
    LairAccessPending {
        lair_id: LairId,
        topology_revision: TopologyRevision,
        request_id: u64,
        expires_at_unix_seconds: u64,
    },
    LairAccessRequestCancelled {
        lair_id: LairId,
        topology_revision: TopologyRevision,
        request_id: u64,
    },
    LairAccessRequestsCancelled {
        cancelled: u32,
    },
    /// One page of the access inventory. `authorization_revision` advances
    /// on every grant or pending-request change, so a client paging through
    /// sees whether the inventory changed between pages.
    AccessInventory {
        authorization_revision: u64,
        policy_generation: u64,
        remembered_store: RememberedStoreStatus,
        /// Every entry in the inventory, not only this page.
        total_entries: u32,
        #[serde(deserialize_with = "deserialize_access_entries")]
        entries: Vec<AccessEntry>,
        /// The key to continue after; `None` on the last page.
        next_after: Option<AccessEntryKey>,
    },
    AccessEntryRevoked {
        authorization_revision: u64,
        revocation: AccessRevocation,
    },
    /// What **Revoke all access** revoked, or with `dry_run` would revoke.
    AllAccessRevoked {
        authorization_revision: u64,
        dry_run: bool,
        counts: AccessRevocationCounts,
        /// The durable removal of the remembered grants; `NotNeeded` for a
        /// dry run or when none were removed.
        durable_removal: DurableRemoval,
    },
    AuthorizationStatus {
        lair_id: LairId,
        dojo_id: DojoId,
        incarnation: u64,
        topology_revision: TopologyRevision,
        policy_generation: u64,
        grants: Vec<AccessGrant>,
        lair_grants: Vec<LairAccessGrant>,
        persistent: Vec<PersistentAuthorizationStatus>,
        development_bypass: bool,
    },
    Attached {
        subscription_id: u64,
        provenance: TerminalProvenance,
        snapshot: TerminalSnapshot,
    },
    ImageContentReady {
        transfer: ImageContentTransfer,
    },
    ScrollbackPage {
        provenance: TerminalProvenance,
        page: ScrollbackPage,
    },
    ScrollbackResyncRequired {
        provenance: TerminalProvenance,
        current_revision: u64,
        history_generation: u64,
    },
    SearchResults {
        provenance: TerminalProvenance,
        page: SearchPage,
    },
    SearchResyncRequired {
        provenance: TerminalProvenance,
        current_revision: u64,
        history_generation: u64,
    },
    ControlGranted {
        controller_id: u64,
        lair_id: LairId,
        dojo_id: DojoId,
    },
    ControlSubscribed {
        subscription_id: u64,
        status: ControlStatus,
    },
    ControlTransferPending {
        transfer_id: u64,
        lair_id: LairId,
        dojo_id: DojoId,
    },
    ControlTransferDecided {
        outcome: ControlTransferOutcome,
        controller_id: Option<u64>,
    },
    AuditPage {
        page: AuditPage,
    },
    TerminalActionAcknowledged {
        lair_id: LairId,
        dojo_id: DojoId,
        splint_id: SplintId,
        incarnation: u64,
        terminal_revision: u64,
        history_generation: u64,
    },
    Acknowledged,
    SplintKilled {
        splint_id: SplintId,
        incarnation: u64,
        exit_status: ProcessExitStatus,
    },
    AgentStatusAccepted {
        /// Daemon-lifetime status sequence current after this report.
        sequence: u64,
        /// Unix milliseconds when an expiring status becomes stale.
        expires_at_ms: Option<u64>,
    },
    UnusedDojoClose {
        dojo_id: DojoId,
        outcome: UnusedDojoCloseOutcome,
    },
    /// Result of a [`DaemonHandoffMode::DryRun`] handoff preparation.
    HandoffReport {
        report: HandoffReport,
    },
    /// Result of a valid [`Request::ClaimHandoffControl`]: the controller
    /// restored to this connection for the named Splint, or none when that
    /// Splint's controller could not be restored. A controller lease is bound
    /// to one connection, so a claim restores at most one.
    HandoffControlClaimed {
        #[serde(deserialize_with = "deserialize_claimed_controllers")]
        controllers: Vec<HandoffClaimedController>,
    },
    /// Result of [`Request::SubscribeConsentOffers`]: offers and their
    /// withdrawals arrive as events of `subscription_id`.
    ConsentOffersSubscribed {
        subscription_id: u64,
    },
    /// Result of a valid [`Request::AnswerConsentOffer`]: the answer settled
    /// the request it was offered for.
    ConsentOfferAnswered,
}

/// Result of [`Request::CloseUnusedDojo`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum UnusedDojoCloseOutcome {
    /// The Dojo was removed and its process is shutting down.
    Closed {
        topology_revision: TopologyRevision,
        /// Its Lair, never named by the user, held no other Dojo and went too.
        #[serde(default, skip_serializing_if = "bool_is_false")]
        lair_removed: bool,
    },
    /// The Dojo counts as used and was left unchanged.
    Used { reason: DojoUse },
}

/// Why a Dojo counts as used, so it is never closed automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DojoUse {
    /// It no longer holds only its original Splint.
    Split,
    /// It did not start in this daemon as a plain default-shell terminal, or
    /// its Splint was relaunched or restored since.
    Launch,
    /// It or its Splint was renamed.
    Renamed,
    /// Its Splint received client input.
    Input,
    /// Another Window is attached to it.
    Attached,
    /// A program other than the shell holds the terminal foreground.
    Foreground,
    /// Its Splint is not running.
    NotRunning,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SubscriptionEvent {
    Snapshot {
        snapshot: TerminalSnapshot,
    },
    Update {
        update: TerminalUpdate,
    },
    ResyncRequired {
        current_revision: u64,
    },
    AccessRevoked {
        grant_id: u64,
    },
    ControlStatusChanged {
        status: ControlStatus,
    },
    ControlTransferRequested {
        transfer_id: u64,
    },
    ControlTransferResolved {
        transfer_id: u64,
        outcome: ControlTransferOutcome,
        controller_id: Option<u64>,
    },
    TopologyChanged {
        change: TopologyChange,
    },
    TopologyResyncRequired {
        current_revision: TopologyRevision,
    },
    Exited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    /// Delivered on topology subscriptions without a topology revision bump.
    /// Clients apply it only to a matching live incarnation; the next topology
    /// snapshot supersedes it.
    AgentStatusChanged {
        splint_id: SplintId,
        incarnation: u64,
        status: Option<AgentStatusSummary>,
        sequence: u64,
    },
    /// Sent while the serving daemon prepares a handoff, without a topology
    /// revision bump. It may arrive on any subscription, terminal, control,
    /// or topology, and every client must tolerate it anywhere: it consumes
    /// one subscription sequence number and changes no subscribed state. The
    /// daemon sends it on topology and control subscriptions; the graphical
    /// client's upgrade banner reads its per-pane control subscriptions
    /// (Plan 0061 M3.12a). A control subscription that starts during an
    /// attempt receives the current phase first, and one that lags receives
    /// the latest phase again. The outcome after exec reaches a participating
    /// Window through its continuation channel, and every other client
    /// through the next connection's `ServerFrame::Hello::daemon_generation`.
    HandoffStatus {
        /// Generation of the daemon that is handing off.
        daemon_generation: u64,
        phase: HandoffStatusPhase,
    },
    /// Sent only on a consent responder subscription
    /// ([`Request::SubscribeConsentOffers`]): a workspace access prompt this
    /// responder may answer with [`Request::AnswerConsentOffer`].
    ConsentOffered {
        offer: ConsentOffer,
    },
    /// Sent only on a consent responder subscription: the offer ended
    /// without this responder's answer, and the responder closes its prompt
    /// without answering.
    ConsentOfferWithdrawn {
        offer_id: u64,
        reason: ConsentOfferEnd,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopologyChange {
    pub revision: TopologyRevision,
    pub kind: TopologyChangeKind,
    pub snapshot: TopologySnapshot,
}

impl TopologyChange {
    /// Validates the change/snapshot correlation.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when revisions or snapshot identities disagree.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.snapshot.validate()?;
        if self.revision != self.snapshot.revision {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "topology change revision does not match its snapshot",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlMode {
    Input,
    Resize,
}

/// Validates a nonempty, duplicate-free public controller mode set.
///
/// # Errors
/// Returns `InvalidArgument` when no mode, a duplicate, or more than two modes are present.
pub fn validate_control_modes(modes: &[ControlMode]) -> Result<(), ProtocolError> {
    let valid = (1..=2).contains(&modes.len()) && !(modes.len() == 2 && modes[0] == modes[1]);
    if !valid {
        return Err(ProtocolError::new(
            ErrorCode::InvalidArgument,
            "invalid controller modes",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlTransferDecision {
    Accept,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlTransferOutcome {
    Granted,
    Denied,
    TimedOut,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlStatus {
    pub splint_id: SplintId,
    pub incarnation: u64,
    pub controlled: bool,
    pub locally_owned: bool,
}

impl ControlStatus {
    /// Validates a subscriber-specific control status snapshot.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for malformed identity or ownership state.
    pub fn validate(self) -> Result<(), ProtocolError> {
        if self.incarnation == 0 || (self.locally_owned && !self.controlled) {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "invalid control status",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TopologyChangeKind {
    LairCreated,
    SplintSplit,
    SplintClosed,
    SplitRatioChanged,
    DojoCreated,
    PresetMaterialized,
    DojoClosed,
    LairTerminated,
    LairRenamed,
    LairRetentionChanged,
    DojoRenamed,
    DojoDefaultFocusChanged,
    SplintRenamed,
    RuntimeChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchParameters {
    pub cwd: PathBuf,
    /// Direct executable plus argv. Empty selects `shell` or the default shell.
    pub command: Vec<String>,
    pub shell: Option<String>,
    pub login_shell: bool,
    pub scrollback_lines: usize,
}

impl AutomationLaunch {
    /// Validates bounded structured automation launch input without resolving defaults.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when an explicit path or argv exceeds protocol bounds.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let argv_bytes = self
            .argv
            .iter()
            .try_fold(0_usize, |total, item| total.checked_add(item.len()));
        let valid = self.cwd.as_ref().is_none_or(|cwd| {
            cwd.is_absolute()
                && !cwd.as_os_str().is_empty()
                && !cwd.as_os_str().as_encoded_bytes().contains(&0)
                && cwd.as_os_str().as_encoded_bytes().len() <= MAX_CWD_BYTES
        }) && self.argv.len() <= MAX_LAUNCH_ARGUMENTS
            && self
                .argv
                .iter()
                .all(|item| !item.contains('\0') && item.len() <= MAX_LAUNCH_ARGUMENT_BYTES)
            && self.argv.first().is_none_or(|program| !program.is_empty())
            && argv_bytes.is_some_and(|bytes| bytes <= MAX_INPUT_BYTES);
        if !valid {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "automation launch parameters exceed protocol limits",
            ));
        }
        Ok(())
    }
}

impl LaunchParameters {
    /// Validates wire allocation and launch-policy bounds.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when any field exceeds protocol limits.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let command_bytes = self
            .command
            .iter()
            .try_fold(0_usize, |total, item| total.checked_add(item.len()));
        let valid = !self.cwd.as_os_str().is_empty()
            && !self.cwd.as_os_str().as_encoded_bytes().contains(&0)
            && self.cwd.as_os_str().as_encoded_bytes().len() <= MAX_CWD_BYTES
            && self.command.len() <= MAX_LAUNCH_ARGUMENTS
            && self
                .command
                .iter()
                .all(|item| !item.contains('\0') && item.len() <= MAX_LAUNCH_ARGUMENT_BYTES)
            && self
                .command
                .first()
                .is_none_or(|program| !program.is_empty())
            && command_bytes.is_some_and(|bytes| bytes <= MAX_INPUT_BYTES)
            && self.shell.as_ref().is_none_or(|shell| {
                !shell.is_empty()
                    && !shell.contains('\0')
                    && shell.len() <= MAX_LAUNCH_ARGUMENT_BYTES
            })
            && self.scrollback_lines <= MAX_SCROLLBACK_LINES;
        if !valid {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "launch parameters exceed protocol limits",
            ));
        }
        Ok(())
    }
}

fn valid_preset_label(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.len() <= maximum
        && !value.chars().any(char::is_control)
}

fn valid_preset_key(value: &str) -> bool {
    valid_preset_label(value, MAX_PRESET_KEY_BYTES)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

impl PresetLayoutLaunch {
    fn validate_into(
        &self,
        depth: usize,
        keys: &mut std::collections::HashSet<String>,
    ) -> Result<usize, ProtocolError> {
        if depth > MAX_PRESET_DEPTH {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "preset layout exceeds depth limit",
            ));
        }
        match self {
            Self::Pane { key, title, launch } => {
                if !valid_preset_key(key) || !valid_preset_label(title, MAX_PRESET_LABEL_BYTES) {
                    return Err(ProtocolError::new(
                        ErrorCode::InvalidArgument,
                        "preset pane key or title is invalid",
                    ));
                }
                if !keys.insert(key.clone()) {
                    return Err(ProtocolError::new(
                        ErrorCode::InvalidArgument,
                        "preset pane keys must be unique within a Dojo",
                    ));
                }
                launch.validate()?;
                if !launch.cwd.is_absolute() {
                    return Err(ProtocolError::new(
                        ErrorCode::InvalidArgument,
                        "preset pane cwd must be absolute",
                    ));
                }
                Ok(1)
            }
            Self::Split { first, second, .. } => {
                let first_count = first.validate_into(depth + 1, keys)?;
                let second_count = second.validate_into(depth + 1, keys)?;
                first_count.checked_add(second_count).ok_or_else(|| {
                    ProtocolError::new(ErrorCode::InvalidArgument, "preset pane count overflow")
                })
            }
        }
    }
}

impl PresetDojoLaunch {
    /// Validates one bounded neutral launch tree before daemon materialization.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for malformed labels, keys, launches, or bounds.
    pub fn validate(&self) -> Result<usize, ProtocolError> {
        if !valid_preset_label(&self.name, MAX_PRESET_LABEL_BYTES)
            || !valid_preset_key(&self.focus_key)
        {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "preset Dojo name or focus key is invalid",
            ));
        }
        let mut keys = std::collections::HashSet::new();
        let pane_count = self.root.validate_into(0, &mut keys)?;
        if pane_count > MAX_PRESET_PANES_PER_DOJO || !keys.contains(&self.focus_key) {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "preset Dojo pane count or focus key is invalid",
            ));
        }
        Ok(pane_count)
    }
}

impl PresetDirectoryIdentity {
    /// Validates one no-follow directory identity without reading the filesystem.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for an unsafe or unbounded path or zero identity.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let path = self.path.as_os_str().as_encoded_bytes();
        if !self.path.is_absolute()
            || self.path.to_str().is_none()
            || path.is_empty()
            || path.len() > MAX_INPUT_BYTES
            || path.contains(&0)
            || self.device == 0
            || self.inode == 0
        {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "preset directory identity is invalid",
            ));
        }
        Ok(())
    }
}

impl PresetLayoutLaunch {
    fn contains_cwd(&self, path: &Path) -> bool {
        match self {
            Self::Pane { launch, .. } => launch.cwd == path,
            Self::Split { first, second, .. } => {
                first.contains_cwd(path) || second.contains_cwd(path)
            }
        }
    }
}

impl PresetTarget {
    /// Validates bounded target naming without resolving daemon topology.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for an invalid new or replacement Lair name.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let valid = match self {
            Self::NewLair { name } => valid_preset_label(name, MAX_PRESET_LABEL_BYTES),
            Self::ExistingLair { rename, .. } => rename
                .as_ref()
                .is_none_or(|name| valid_preset_label(name, MAX_PRESET_LABEL_BYTES)),
        };
        if !valid {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "preset target name is invalid",
            ));
        }
        Ok(())
    }
}

/// Validates one complete atomic preset request before topology mutation.
///
/// # Errors
/// Returns `InvalidArgument` when request-level Dojo or pane bounds are exceeded.
pub fn validate_preset_materialization(
    target: &PresetTarget,
    dojos: &[PresetDojoLaunch],
    directory_identities: &[PresetDirectoryIdentity],
) -> Result<(), ProtocolError> {
    target.validate()?;
    if dojos.is_empty() || dojos.len() > MAX_PRESET_DOJOS {
        return Err(ProtocolError::new(
            ErrorCode::InvalidArgument,
            "preset Dojo count is invalid",
        ));
    }
    let pane_count = dojos.iter().try_fold(0_usize, |total, dojo| {
        total.checked_add(dojo.validate()?).ok_or_else(|| {
            ProtocolError::new(ErrorCode::InvalidArgument, "preset pane count overflow")
        })
    })?;
    if directory_identities.len() > MAX_PRESET_DIRECTORY_IDENTITIES {
        return Err(ProtocolError::new(
            ErrorCode::InvalidArgument,
            "preset directory identity count exceeds limit",
        ));
    }
    let mut identity_paths = std::collections::HashSet::new();
    for identity in directory_identities {
        identity.validate()?;
        if !identity_paths.insert(identity.path.clone())
            || !dojos
                .iter()
                .any(|dojo| dojo.root.contains_cwd(&identity.path))
        {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "preset directory identities are inconsistent",
            ));
        }
    }
    if pane_count > MAX_PRESET_TOTAL_PANES {
        return Err(ProtocolError::new(
            ErrorCode::InvalidArgument,
            "preset total pane count exceeds limit",
        ));
    }
    Ok(())
}

/// Validates stable pane mappings returned by one atomic preset transaction.
///
/// # Errors
/// Returns `InvalidArgument` for duplicate, missing, or out-of-scope identities.
pub fn validate_preset_materialized(
    dojo_ids: &[DojoId],
    panes: &[PresetPaneIdentity],
) -> Result<(), ProtocolError> {
    if dojo_ids.is_empty()
        || dojo_ids.len() > MAX_PRESET_DOJOS
        || panes.is_empty()
        || panes.len() > MAX_PRESET_TOTAL_PANES
    {
        return Err(ProtocolError::new(
            ErrorCode::InvalidArgument,
            "preset materialization response cardinality is invalid",
        ));
    }
    let dojo_count = dojo_ids.len();
    let dojo_ids = dojo_ids
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    if dojo_ids.len() != dojo_count {
        return Err(ProtocolError::new(
            ErrorCode::InvalidArgument,
            "preset materialization repeats Dojo identities",
        ));
    }
    let mut pane_keys = std::collections::HashSet::new();
    let mut splint_ids = std::collections::HashSet::new();
    if panes.iter().any(|pane| {
        !dojo_ids.contains(&pane.dojo_id)
            || !valid_preset_key(&pane.key)
            || !pane_keys.insert((pane.dojo_id, pane.key.clone()))
            || !splint_ids.insert(pane.splint_id)
    }) || dojo_ids
        .iter()
        .any(|dojo_id| !panes.iter().any(|pane| pane.dojo_id == *dojo_id))
    {
        return Err(ProtocolError::new(
            ErrorCode::InvalidArgument,
            "preset materialization pane mappings are invalid",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopologySnapshot {
    pub revision: TopologyRevision,
    pub topology: Topology,
    pub runtimes: Vec<SplintRuntimeSummary>,
}

impl TopologySnapshot {
    /// Validates topology/runtime identity correlation.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` for duplicate, stale, or unreachable runtime entries.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let mut identities = std::collections::HashSet::new();
        let runtime_count = self
            .topology
            .lairs()
            .flat_map(|lair| &lair.dojos)
            .map(|dojo| dojo.root.splint_count())
            .sum::<usize>();
        let valid = self.revision == self.topology.revision()
            && runtime_count == self.runtimes.len()
            && self.runtimes.iter().all(|runtime| {
                identities.insert(runtime.splint_id)
                    && self
                        .topology
                        .find_splint(runtime.splint_id)
                        .is_some_and(|splint| splint.last_incarnation == runtime.last_incarnation)
                    && runtime.validate()
            });
        if !valid {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "topology runtime metadata is inconsistent",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreLeafResult {
    pub splint_id: SplintId,
    pub incarnation: Option<u64>,
    pub error: Option<ProtocolError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplintRuntimeSummary {
    pub splint_id: SplintId,
    pub live_incarnation: Option<u64>,
    pub last_incarnation: Option<u64>,
    pub restorable: bool,
    pub lifecycle: SplintLifecycle,
    pub exit_status: Option<ProcessExitStatus>,
    /// Explicitly reported display status of the live incarnation only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_status: Option<AgentStatusSummary>,
}

impl SplintRuntimeSummary {
    #[must_use]
    pub fn validate(&self) -> bool {
        let valid_last = self.last_incarnation.is_none_or(|value| value > 0);
        if let Some(status) = &self.agent_status
            && (self.live_incarnation.is_none() || !status.validate())
        {
            return false;
        }
        match self.lifecycle {
            SplintLifecycle::Starting => {
                !self.restorable
                    && valid_last
                    && self.live_incarnation.is_none_or(|value| value > 0)
                    && self.exit_status.is_none()
            }
            SplintLifecycle::Running => {
                !self.restorable
                    && valid_last
                    && self.live_incarnation.is_some_and(|value| value > 0)
                    && self.live_incarnation == self.last_incarnation
                    && self.exit_status.is_none()
            }
            SplintLifecycle::Exited => valid_last && self.live_incarnation.is_none(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplintLifecycle {
    Starting,
    Running,
    Exited,
}

/// Closed vocabulary of explicitly reported agent display states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Working,
    NeedsInput,
    Done,
    Error,
    Idle,
}

impl AgentState {
    /// Active states always expire; resting states last until replaced.
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Working | Self::NeedsInput)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::NeedsInput => "needs_input",
            Self::Done => "done",
            Self::Error => "error",
            Self::Idle => "idle",
        }
    }
}

/// One explicit agent status report. Display data only, never authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentStatusReport {
    pub state: AgentState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_seconds: Option<u32>,
}

impl AgentStatusReport {
    /// Validates the bounded detail and TTL.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` for an out-of-range TTL or a detail that is
    /// empty, oversized, or contains control, bidi-formatting, or line-break
    /// characters. Invalid text is rejected, never stripped.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if let Some(detail) = &self.detail {
            validate_agent_status_detail(detail)?;
        }
        if self.ttl_seconds.is_some_and(|ttl| {
            !(MIN_AGENT_STATUS_TTL_SECONDS..=MAX_AGENT_STATUS_TTL_SECONDS).contains(&ttl)
        }) {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "agent status TTL is outside 10 to 900 seconds",
            ));
        }
        Ok(())
    }

    /// The TTL that applies: the reported one, else the active-state default.
    #[must_use]
    pub fn effective_ttl_seconds(&self) -> Option<u32> {
        self.ttl_seconds.or_else(|| {
            self.state
                .is_active()
                .then_some(DEFAULT_AGENT_STATUS_ACTIVE_TTL_SECONDS)
        })
    }
}

fn is_agent_status_forbidden_character(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{061c}'
                | '\u{200e}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
                | '\u{2028}'..='\u{2029}'
        )
}

/// Validates explicitly reported agent status detail text.
///
/// # Errors
///
/// Returns `InvalidArgument` unless the detail is nonempty, at most 80 Unicode
/// scalars and 240 bytes, and free of control, bidi-formatting, and line-break
/// characters.
pub fn validate_agent_status_detail(detail: &str) -> Result<(), ProtocolError> {
    if detail.is_empty()
        || detail.len() > MAX_AGENT_STATUS_DETAIL_BYTES
        || detail.chars().count() > MAX_AGENT_STATUS_DETAIL_SCALARS
    {
        return Err(ProtocolError::new(
            ErrorCode::InvalidArgument,
            "agent status detail is empty or exceeds 80 characters or 240 bytes",
        ));
    }
    if detail.chars().any(is_agent_status_forbidden_character) {
        return Err(ProtocolError::new(
            ErrorCode::InvalidArgument,
            "agent status detail contains control, bidi-formatting, or line-break characters",
        ));
    }
    Ok(())
}

/// Daemon-held agent status for one live incarnation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentStatusSummary {
    pub state: AgentState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub sequence: u64,
    /// An active state's TTL lapsed; the last state and detail are kept.
    pub stale: bool,
    /// Milliseconds since the last accepted report, at summary time.
    pub reported_age_ms: u64,
}

impl AgentStatusSummary {
    #[must_use]
    pub fn validate(&self) -> bool {
        self.sequence > 0
            && self
                .detail
                .as_deref()
                .is_none_or(|detail| validate_agent_status_detail(detail).is_ok())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessExitStatus {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_topology_revision: Option<TopologyRevision>,
    /// Every reason a [`ErrorCode::HandoffUnavailable`] handoff was refused;
    /// empty for every other error.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_blocking_reasons"
    )]
    pub handoff_blocking_reasons: Vec<HandoffBlockingReason>,
    /// The pending workspace request that refused a different request from
    /// the same process for the same workspace (ADR 0014).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_request_id: Option<u64>,
}

impl ProtocolError {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            current_topology_revision: None,
            handoff_blocking_reasons: Vec::new(),
            pending_request_id: None,
        }
    }

    /// A refused handoff that names every reason it cannot start.
    #[must_use]
    pub fn handoff_unavailable(reasons: Vec<HandoffBlockingReason>) -> Self {
        Self {
            handoff_blocking_reasons: reasons,
            ..Self::new(
                ErrorCode::HandoffUnavailable,
                "daemon handoff is unavailable",
            )
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationScope {
    TopologyMetadataRead,
    TopologySubscribe,
    TerminalVisibleRead,
    TerminalSubscribe,
    ScrollbackRead,
    ScrollbackSearch,
    ControllerAcquire,
    ControllerTransfer,
    Input,
    Resize,
    ProcessSpawn,
    ProcessRestore,
    ProcessTerminate,
    TopologyLayoutMutate,
    TopologyNameMutate,
    AuthorizationInspect,
    AuthorizationRevoke,
    AuditInspect,
    AgentStatusReport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOperation {
    Ping,
    RequestAccess,
    RequestLairAccess,
    AuthorizationStatus,
    RevokeAccess,
    ListLairs,
    InspectTopology,
    SubscribeTopology,
    InspectSplint,
    ResolveLaunchDirectory,
    ReadGraphicalFocus,
    PublishGraphicalFocus,
    CreateLair,
    SplitSplint,
    RelaunchSplint,
    RestoreSplint,
    RestoreDojo,
    RestoreLair,
    SetLairRetention,
    CloseSplint,
    SetSplitRatio,
    NewDojo,
    MaterializePreset,
    CloseDojo,
    TerminateLair,
    RenameLair,
    RenameDojo,
    SetDojoDefaultFocus,
    RenameSplint,
    Attach,
    ScrollbackPage,
    SearchScrollback,
    AcquireControl,
    SubscribeControl,
    RequestControlTransfer,
    DecideControlTransfer,
    ForceControlTransfer,
    ReleaseControl,
    Input,
    Resize,
    Detach,
    KillSplint,
    ProcessExit,
    AuditInspect,
    PolicyReload,
    AgentStatusReport,
    PrepareDaemonHandoff,
    ClaimHandoffControl,
    AcknowledgeHandoffPrepare,
    SubscribeConsentOffers,
    AnswerConsentOffer,
}

impl AuditOperation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ping => "ping",
            Self::RequestAccess => "request_access",
            Self::RequestLairAccess => "request_lair_access",
            Self::AuthorizationStatus => "authorization_status",
            Self::RevokeAccess => "revoke_access",
            Self::ListLairs => "list_lairs",
            Self::InspectTopology => "inspect_topology",
            Self::SubscribeTopology => "subscribe_topology",
            Self::InspectSplint => "inspect_splint",
            Self::ResolveLaunchDirectory => "resolve_launch_directory",
            Self::ReadGraphicalFocus => "read_graphical_focus",
            Self::PublishGraphicalFocus => "publish_graphical_focus",
            Self::CreateLair => "create_lair",
            Self::SplitSplint => "split_splint",
            Self::RelaunchSplint => "relaunch_splint",
            Self::RestoreSplint => "restore_splint",
            Self::RestoreDojo => "restore_dojo",
            Self::RestoreLair => "restore_lair",
            Self::SetLairRetention => "set_lair_retention",
            Self::CloseSplint => "close_splint",
            Self::SetSplitRatio => "set_split_ratio",
            Self::NewDojo => "new_dojo",
            Self::MaterializePreset => "materialize_preset",
            Self::CloseDojo => "close_dojo",
            Self::TerminateLair => "terminate_lair",
            Self::RenameLair => "rename_lair",
            Self::RenameDojo => "rename_dojo",
            Self::SetDojoDefaultFocus => "set_dojo_default_focus",
            Self::RenameSplint => "rename_splint",
            Self::Attach => "attach",
            Self::ScrollbackPage => "scrollback_page",
            Self::SearchScrollback => "search_scrollback",
            Self::AcquireControl => "acquire_control",
            Self::SubscribeControl => "subscribe_control",
            Self::RequestControlTransfer => "request_control_transfer",
            Self::DecideControlTransfer => "decide_control_transfer",
            Self::ForceControlTransfer => "force_control_transfer",
            Self::ReleaseControl => "release_control",
            Self::Input => "input",
            Self::Resize => "resize",
            Self::Detach => "detach",
            Self::KillSplint => "kill_splint",
            Self::ProcessExit => "process_exit",
            Self::AuditInspect => "audit_inspect",
            Self::PolicyReload => "policy_reload",
            Self::AgentStatusReport => "agent_status_report",
            Self::PrepareDaemonHandoff => "prepare_daemon_handoff",
            Self::ClaimHandoffControl => "claim_handoff_control",
            Self::AcknowledgeHandoffPrepare => "acknowledge_handoff_prepare",
            Self::SubscribeConsentOffers => "subscribe_consent_offers",
            Self::AnswerConsentOffer => "answer_consent_offer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditDecision {
    Allowed,
    Denied,
    Revoked,
    Expired,
    Matched,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditPeer {
    pub uid: u32,
    pub executable_path: String,
    pub executable_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inode: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditResource {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lair_id: Option<LairId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dojo_id: Option<DojoId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub splint_id: Option<SplintId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incarnation: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub schema: String,
    pub retention: String,
    pub audit_id: u64,
    pub unix_seconds: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_rule_id: Option<String>,
    pub peer: AuditPeer,
    pub operation: AuditOperation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<AuditResource>,
    pub requested_scopes: Vec<AutomationScope>,
    pub decision: AuditDecision,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<AuditOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argument_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable_basename: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditPage {
    pub records: Vec<AuditRecord>,
    pub retention_gap: bool,
    pub oldest_available_audit_id: Option<u64>,
    pub newest_available_audit_id: Option<u64>,
    pub next_after_audit_id: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessScope {
    Observe,
    Scrollback,
    Input,
    Resize,
    ClipboardRead,
    ClipboardWrite,
    Terminate,
    ControlTakeover,
    TopologyObserve,
    TopologyLayout,
    TopologyName,
    ProcessSpawn,
    ProcessRestore,
    AgentStatusReport,
}

impl AccessScope {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Observe => "observe visible terminal",
            Self::Scrollback => "read scrollback",
            Self::Input => "send input",
            Self::Resize => "resize terminal",
            Self::ClipboardRead => "read clipboard metadata",
            Self::ClipboardWrite => "write clipboard metadata",
            Self::Terminate => "terminate process",
            Self::ControlTakeover => "take over terminal control",
            Self::TopologyObserve => "inspect this Lair topology",
            Self::TopologyLayout => "change this Lair layout",
            Self::TopologyName => "rename this Lair and its sessions",
            Self::ProcessSpawn => "start processes in this Lair",
            Self::ProcessRestore => "restore processes in this Lair",
            Self::AgentStatusReport => "report agent status",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConsentTarget {
    Splint {
        splint_id: SplintId,
        incarnation: u64,
    },
    Lair {
        lair_id: LairId,
        lair_name: String,
    },
}

const LOOK_SCOPES: &[AccessScope] = &[
    AccessScope::TopologyObserve,
    AccessScope::Observe,
    AccessScope::Scrollback,
    AccessScope::AgentStatusReport,
];
const LOOK_AND_TYPE_SCOPES: &[AccessScope] = &[
    AccessScope::TopologyObserve,
    AccessScope::Observe,
    AccessScope::Scrollback,
    AccessScope::AgentStatusReport,
    AccessScope::Input,
    AccessScope::Resize,
    AccessScope::ControlTakeover,
];
const MANAGE_SCOPES: &[AccessScope] = &[
    AccessScope::TopologyObserve,
    AccessScope::Observe,
    AccessScope::Scrollback,
    AccessScope::AgentStatusReport,
    AccessScope::Input,
    AccessScope::Resize,
    AccessScope::ControlTakeover,
    AccessScope::ProcessSpawn,
    AccessScope::TopologyLayout,
    AccessScope::TopologyName,
    AccessScope::Terminate,
    AccessScope::ProcessRestore,
];

/// The three fixed access levels of ADR 0014, each an exact bundle of
/// consent scopes containing the smaller ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessLevel {
    Look,
    LookAndType,
    Manage,
}

impl AccessLevel {
    pub const ALL: [Self; 3] = [Self::Look, Self::LookAndType, Self::Manage];

    /// The exact consent scopes of this level.
    #[must_use]
    pub const fn scopes(self) -> &'static [AccessScope] {
        match self {
            Self::Look => LOOK_SCOPES,
            Self::LookAndType => LOOK_AND_TYPE_SCOPES,
            Self::Manage => MANAGE_SCOPES,
        }
    }

    /// The level's name as the prompt shows it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Look => "Look",
            Self::LookAndType => "Look and type",
            Self::Manage => "Manage",
        }
    }

    /// The one line a prompt shows for this level.
    #[must_use]
    pub const fn summary(self) -> &'static str {
        match self {
            Self::Look => "Read terminals and scrollback, and report agent status",
            Self::LookAndType => "Read terminals, type commands, and take typing control",
            Self::Manage => "Read terminals, type commands, and create or close sessions",
        }
    }

    /// The level whose bundle is exactly `scopes`, if any. Never rounds up.
    #[must_use]
    pub fn exactly(scopes: &[AccessScope]) -> Option<Self> {
        let mut requested = scopes.to_vec();
        requested.sort_unstable();
        requested.dedup();
        Self::ALL.into_iter().find(|level| {
            let mut bundle = level.scopes().to_vec();
            bundle.sort_unstable();
            bundle == requested
        })
    }
}

/// How long a granted answer lasts (ADR 0014), and therefore which identity
/// it binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantDuration {
    /// "Just this once": five minutes, bound to the requesting process.
    Once,
    /// "While this workspace is open": while the Lair exists in this daemon
    /// generation, bound to the verified executable identity.
    WorkspaceSession,
    /// "Always for this workspace": until revoked or the workspace is
    /// removed, bound to a durable identity.
    Remembered,
}

impl GrantDuration {
    pub const ALL: [Self; 3] = [Self::Once, Self::WorkspaceSession, Self::Remembered];
}

/// Why the daemon withholds a duration; each has the prompt's one-line
/// reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurationWithheld {
    NoPinnedIdentity,
    CustomTakeover,
    Relay,
    Interpreter,
    NoDurableIdentity,
    StoreUnavailable,
}

impl DurationWithheld {
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::NoPinnedIdentity => {
                "Splinterm cannot pin this program's identity, so it can only allow it once."
            }
            Self::CustomTakeover => "Taking control outside a level can only be allowed once.",
            Self::Relay => {
                "The SSH relay serves every remote caller; remote automation uses the policy file."
            }
            Self::Interpreter => {
                "This program runs scripts; any script it runs would share this access."
            }
            Self::NoDurableIdentity => {
                "This program's file can be changed by your account, so it cannot be remembered."
            }
            Self::StoreUnavailable => {
                "Remembered access is unavailable until its store is repaired."
            }
        }
    }
}

/// An access definition a longer answer would replace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentReplacement {
    /// The level it names, or `None` for Custom access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<AccessLevel>,
    #[serde(deserialize_with = "deserialize_access_scopes")]
    pub scopes: Vec<AccessScope>,
}

/// One duration the prompt shows, offered or withheld.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentDurationOffer {
    pub duration: GrantDuration,
    /// Why it is withheld; `None` when it is offered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withheld: Option<DurationWithheld>,
    /// For an offered longer duration, the executable path every program
    /// sharing the grant runs (the prompt's shared-identity line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_by: Option<String>,
    /// The existing definition of this duration, for the same identity and
    /// workspace, that granting it would replace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<ConsentReplacement>,
}

/// The line a prompt shows when a longer duration is selected, in the same
/// form for every executable (ADR 0014).
#[must_use]
pub fn shared_identity_line(path: &str) -> String {
    format!("Any program that runs {path} shares this access.")
}

/// One trusted consent dialog's prompt, as its launcher sends it over the
/// private descriptor.
///
/// Every string in the content (`requester`, `agent_name`, a Lair target's
/// `lair_name`, each duration's `shared_by`) is untrusted: an agent, its
/// host, or a remote daemon chose it. Validation bounds its length only.
/// Every responder that shows it, the dialog and the terminal approver
/// alike, must neutralise C0 and C1 controls, ESC, and bidi and invisible
/// formatting characters before display, exactly as the consent dialog and
/// the **Agent access** list escape them today (`splinterm::frontend`'s
/// `shown_name`), and quote names so they cannot pass for prompt text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentPrompt {
    pub capability: Vec<u8>,
    /// Untrusted display text; see the type documentation.
    pub requester: String,
    pub requester_pid: u32,
    pub requester_uid: u32,
    /// The host-reported agent name: untrusted display data only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    pub target: ConsentTarget,
    #[serde(deserialize_with = "deserialize_access_scopes")]
    pub scopes: Vec<AccessScope>,
    /// The level whose exact bundle `scopes` is, or `None` for Custom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<AccessLevel>,
    /// Every duration the prompt shows, in [`GrantDuration::ALL`] order;
    /// "Just this once" first and always offered.
    #[serde(deserialize_with = "deserialize_consent_durations")]
    pub durations: Vec<ConsentDurationOffer>,
    /// Seconds left to answer, at most
    /// [`MAX_CONSENT_RESPOND_WITHIN_SECONDS`]; zero shows no countdown.
    #[serde(default)]
    pub respond_within_seconds: u64,
    /// Where the request comes from; see [`ConsentOrigin`] for who sets it.
    #[serde(default)]
    pub origin: ConsentOrigin,
}

/// Where a consent prompt's request comes from (ADR 0015).
///
/// The process that launches the dialog sets it, never the offer it
/// answers. The daemon's own prompts are [`ConsentOrigin::Local`]. A remote
/// Window sets [`ConsentOrigin::Remote`] and its SSH `profile` from its own
/// endpoint, so a remote daemon can claim any hostname but can never
/// produce a Local origin. A Window that cannot build a valid Remote origin
/// skips or denies the offer; it never falls back to Local.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConsentOrigin {
    /// This computer's daemon.
    #[default]
    Local,
    /// The daemon a remote Window reaches through its SSH profile.
    Remote {
        /// The Window's own SSH profile name, as
        /// [`usable_remote_profile_name`] accepts it.
        profile: String,
        /// The remote daemon's self-reported `Hello` hostname, as
        /// [`usable_daemon_hostname`] accepts it: display only, never an
        /// identity, and absent when the daemon reported none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        daemon_hostname: Option<String>,
    },
}

impl ConsentOrigin {
    /// # Errors
    /// A remote origin whose profile is not a usable remote profile name, or
    /// whose hostname, when present, is not a usable daemon hostname.
    pub fn validate(&self) -> Result<(), &'static str> {
        let Self::Remote {
            profile,
            daemon_hostname,
        } = self
        else {
            return Ok(());
        };
        if !usable_remote_profile_name(profile) {
            return Err("consent origin profile is not a usable remote profile name");
        }
        if daemon_hostname
            .as_deref()
            .is_some_and(|hostname| usable_daemon_hostname(hostname).is_none())
        {
            return Err("consent origin hostname is not a usable daemon hostname");
        }
        Ok(())
    }
}

/// A [`ConsentPrompt`]'s content as a [`ConsentOffer`] carries it to a
/// consent responder: without the capability, which the responder's own
/// dialog mints; the origin, which the responder sets itself; and the
/// countdown, which the offer carries.
///
/// Its strings are untrusted exactly as [`ConsentPrompt`]'s are, and every
/// responder neutralises them the same way before display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferedConsentPrompt {
    /// Untrusted display text; see [`ConsentPrompt`].
    pub requester: String,
    pub requester_pid: u32,
    pub requester_uid: u32,
    /// The host-reported agent name: untrusted display data only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    pub target: ConsentTarget,
    #[serde(deserialize_with = "deserialize_access_scopes")]
    pub scopes: Vec<AccessScope>,
    /// The level whose exact bundle `scopes` is, or `None` for Custom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<AccessLevel>,
    /// As [`ConsentPrompt::durations`].
    #[serde(deserialize_with = "deserialize_consent_durations")]
    pub durations: Vec<ConsentDurationOffer>,
}

/// The prompt content both [`ConsentPrompt`] and [`OfferedConsentPrompt`]
/// carry, borrowed so that both validate by the same rules.
struct ConsentContent<'a> {
    requester: &'a str,
    agent_name: Option<&'a str>,
    target: &'a ConsentTarget,
    scopes: &'a [AccessScope],
    level: Option<AccessLevel>,
    durations: &'a [ConsentDurationOffer],
}

impl ConsentContent<'_> {
    fn validate(&self) -> Result<(), &'static str> {
        if self.scopes.is_empty() || self.scopes.len() > MAX_ACCESS_SCOPES {
            return Err("consent scopes are empty or exceed the limit");
        }
        if self.requester.chars().count() > MAX_CONSENT_PATH_CHARS {
            return Err("consent requester label exceeds the limit");
        }
        if self
            .agent_name
            .is_some_and(|name| name.chars().count() > MAX_AGENT_NAME_CHARS)
        {
            return Err("consent agent name exceeds the limit");
        }
        if let ConsentTarget::Lair { lair_name, .. } = self.target
            && lair_name.chars().count() > MAX_CONSENT_PATH_CHARS
        {
            return Err("consent workspace name exceeds the limit");
        }
        if self
            .level
            .is_some_and(|level| AccessLevel::exactly(self.scopes) != Some(level))
        {
            return Err("consent level does not name exactly the scopes");
        }
        let Some((first, longer)) = self.durations.split_first() else {
            return Err("consent prompt offers no duration");
        };
        if first.duration != GrantDuration::Once
            || first.withheld.is_some()
            || first.shared_by.is_some()
            || first.replaces.is_some()
        {
            return Err("consent prompt must offer just this once first");
        }
        let mut previous = GrantDuration::Once;
        for offer in longer {
            if offer.duration <= previous {
                return Err("consent durations are repeated or out of order");
            }
            previous = offer.duration;
            if offer.withheld.is_some() && (offer.shared_by.is_some() || offer.replaces.is_some()) {
                return Err("a withheld consent duration carries offer details");
            }
            if offer
                .shared_by
                .as_ref()
                .is_some_and(|path| path.chars().count() > MAX_CONSENT_PATH_CHARS)
            {
                return Err("consent shared-identity path exceeds the limit");
            }
            if offer.replaces.as_ref().is_some_and(|replaced| {
                replaced.scopes.is_empty()
                    || replaced.scopes.len() > MAX_ACCESS_SCOPES
                    || replaced
                        .level
                        .is_some_and(|level| AccessLevel::exactly(&replaced.scopes) != Some(level))
            }) {
                return Err("consent replacement is invalid");
            }
        }
        Ok(())
    }

    fn offers(&self, duration: GrantDuration) -> bool {
        self.durations
            .iter()
            .any(|offer| offer.duration == duration && offer.withheld.is_none())
    }
}

impl ConsentPrompt {
    /// Checks every bound the trusted dialog relies on.
    ///
    /// # Errors
    /// A missing or oversized capability, scope list, label, or path; a
    /// level that is not exactly the scopes; durations that are not "Just
    /// this once" (offered) followed by distinct longer ones in order; a
    /// countdown longer than [`MAX_CONSENT_RESPOND_WITHIN_SECONDS`]; or an
    /// invalid origin.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.capability.len() != CONSENT_CAPABILITY_BYTES {
            return Err("consent capability has the wrong length");
        }
        self.content().validate()?;
        if self.respond_within_seconds > MAX_CONSENT_RESPOND_WITHIN_SECONDS {
            return Err("consent countdown exceeds the pending request lifetime");
        }
        self.origin.validate()
    }

    /// Whether the prompt offers `duration`.
    #[must_use]
    pub fn offers(&self, duration: GrantDuration) -> bool {
        self.content().offers(duration)
    }

    /// This prompt's content as a [`ConsentOffer`] carries it.
    #[must_use]
    pub fn offered(&self) -> OfferedConsentPrompt {
        OfferedConsentPrompt {
            requester: self.requester.clone(),
            requester_pid: self.requester_pid,
            requester_uid: self.requester_uid,
            agent_name: self.agent_name.clone(),
            target: self.target.clone(),
            scopes: self.scopes.clone(),
            level: self.level,
            durations: self.durations.clone(),
        }
    }

    fn content(&self) -> ConsentContent<'_> {
        ConsentContent {
            requester: &self.requester,
            agent_name: self.agent_name.as_deref(),
            target: &self.target,
            scopes: &self.scopes,
            level: self.level,
            durations: &self.durations,
        }
    }
}

impl OfferedConsentPrompt {
    /// Checks the bounds of [`ConsentPrompt::validate`] that apply to the
    /// content, and that it asks for a workspace: only Lair access is
    /// offered to consent responders.
    ///
    /// # Errors
    /// Any content error [`ConsentPrompt::validate`] reports, or a target
    /// that is not a Lair.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !matches!(self.target, ConsentTarget::Lair { .. }) {
            return Err("a consent offer asks for something other than a workspace");
        }
        self.content().validate()
    }

    /// Whether the offered prompt offers `duration`.
    #[must_use]
    pub fn offers(&self, duration: GrantDuration) -> bool {
        self.content().offers(duration)
    }

    /// The dialog prompt a responder shows for this offer, with its own
    /// fresh `capability`, the offer's countdown, and its own `origin`.
    #[must_use]
    pub fn into_prompt(
        self,
        capability: Vec<u8>,
        respond_within_seconds: u64,
        origin: ConsentOrigin,
    ) -> ConsentPrompt {
        ConsentPrompt {
            capability,
            requester: self.requester,
            requester_pid: self.requester_pid,
            requester_uid: self.requester_uid,
            agent_name: self.agent_name,
            target: self.target,
            scopes: self.scopes,
            level: self.level,
            durations: self.durations,
            respond_within_seconds,
            origin,
        }
    }

    fn content(&self) -> ConsentContent<'_> {
        ConsentContent {
            requester: &self.requester,
            agent_name: self.agent_name.as_deref(),
            target: &self.target,
            scopes: &self.scopes,
            level: self.level,
            durations: &self.durations,
        }
    }
}

/// The person's typed answer to a consent prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum ConsentDecision {
    Deny,
    Grant { duration: GrantDuration },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentReply {
    pub capability: Vec<u8>,
    pub decision: ConsentDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessGrant {
    pub grant_id: u64,
    pub splint_id: SplintId,
    pub incarnation: u64,
    pub scopes: Vec<AccessScope>,
    pub requester: String,
    pub expires_at_unix_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessGrantSource {
    Ephemeral,
    PersistentPolicy,
    Development,
    /// A "while this workspace is open" grant (ADR 0014).
    WorkspaceSession,
    /// An "Always for this workspace" grant (ADR 0014).
    Remembered,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LairAccessGrant {
    pub grant_id: u64,
    pub source: AccessGrantSource,
    pub lair_id: LairId,
    pub scopes: Vec<AccessScope>,
    pub requester: String,
    pub expires_at_unix_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistentAuthorizationStatus {
    pub policy_rule_id: String,
    pub scopes: Vec<AutomationScope>,
    pub expires_at_unix_seconds: Option<u64>,
}

/// Names one access inventory entry, and orders the inventory: remembered
/// entries, then runtime grants, then pending requests, then policy rules.
///
/// A remembered entry's durable ID and every runtime grant ID are distinct
/// kinds, so the inventory never confuses the two (ADR 0014).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AccessEntryKey {
    /// A remembered ("Always") definition, by its durable entry ID.
    Remembered {
        entry_id: String,
    },
    /// A "just this once" or "while this workspace is open" grant, by its
    /// runtime grant ID.
    Grant {
        grant_id: u64,
    },
    Pending {
        request_id: u64,
    },
    /// A policy-file rule; listed only, never revoked here.
    Policy {
        rule_id: String,
    },
}

impl AccessEntryKey {
    /// Whether the key is within protocol bounds.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Remembered { entry_id } => is_remembered_entry_id(entry_id),
            Self::Grant { grant_id } => *grant_id != 0,
            Self::Pending { request_id } => *request_id != 0,
            Self::Policy { rule_id } => {
                !rule_id.is_empty() && rule_id.len() <= MAX_ACCESS_RULE_ID_BYTES
            }
        }
    }
}

/// Whether `id` is a remembered entry ID: `ra_` and 32 lowercase hex digits.
#[must_use]
pub fn is_remembered_entry_id(id: &str) -> bool {
    id.len() == REMEMBERED_ENTRY_ID_BYTES
        && id.strip_prefix("ra_").is_some_and(|hex| {
            hex.bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

/// What a per-entry revoke names: a runtime grant (once or while open) or a
/// remembered entry. A remembered entry is revoked by its durable entry ID,
/// never by its runtime grant ID; pending requests and policy rules are not
/// revocable here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AccessRevokeTarget {
    Grant { grant_id: u64 },
    Remembered { entry_id: String },
}

impl AccessRevokeTarget {
    #[must_use]
    pub fn key(&self) -> AccessEntryKey {
        match self {
            Self::Grant { grant_id } => AccessEntryKey::Grant {
                grant_id: *grant_id,
            },
            Self::Remembered { entry_id } => AccessEntryKey::Remembered {
                entry_id: entry_id.clone(),
            },
        }
    }
}

/// What kind of authority an inventory entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessEntryKind {
    /// "Just this once", for a Lair or for one Splint incarnation.
    Once,
    WorkspaceSession,
    Remembered,
    /// A workspace request waiting for the person's answer.
    Pending,
    /// A policy-file rule that matches a connected agent: read-only here.
    PersistentPolicy,
}

/// The state of an inventory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessEntryState {
    Active,
    /// A remembered entry whose workspace was absent when the store loaded:
    /// it authorizes nothing until revoked.
    WorkspaceMissing,
    /// A remembered entry revoked in memory whose durable removal has not
    /// succeeded yet: it authorizes nothing now, and returns after a restart
    /// until a later write removes it.
    RemovalPending,
    /// A pending request whose prompt is showing.
    Prompting,
    /// A pending request waiting behind another prompt.
    Queued,
}

/// How an entry's agent is identified (ADR 0014).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessIdentityKind {
    /// One requesting process ("just this once", or a pending request).
    Process,
    /// The exact executable path and digest ("while this workspace is open").
    ExactBuild,
    ProtectedInstall,
    NixProfile,
    /// A policy rule's executable path and SHA-256 digest.
    PolicyDigest,
}

/// The agent behind an inventory entry. The path is the identity; the name
/// is host-reported display data, never an identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessIdentity {
    pub kind: AccessIdentityKind,
    pub executable_path: String,
    /// The requesting process, for a process-bound entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// The workspace an entry covers, with its user-chosen name when the Lair
/// still exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessWorkspace {
    pub lair_id: LairId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// One policy resource selector, as configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AccessPolicyResource {
    Daemon,
    Lair { lair_id: LairId },
    Dojo { dojo_id: DojoId },
    Splint { splint_id: SplintId },
}

/// One access inventory entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessEntry {
    pub key: AccessEntryKey,
    pub kind: AccessEntryKind,
    pub state: AccessEntryState,
    /// Whether a per-entry revoke may name it. Never for pending requests or
    /// policy rules.
    pub revocable: bool,
    /// The runtime grant ID controllers and subscriptions are bound to;
    /// for a remembered entry it is distinct from its durable entry ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_id: Option<u64>,
    pub identity: AccessIdentity,
    /// The workspace, for every entry except a policy rule. A once grant for
    /// a Splint that left topology has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<AccessWorkspace>,
    /// The Splint incarnation a once grant for one Splint covers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub splint_id: Option<SplintId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incarnation: Option<u64>,
    /// The level the consent scopes are exactly, or `None` for Custom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<AccessLevel>,
    /// Consent scopes, for every entry except a policy rule.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_access_scopes"
    )]
    pub scopes: Vec<AccessScope>,
    /// Policy scopes and selectors, for a policy rule.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_policy_scopes"
    )]
    pub policy_scopes: Vec<AutomationScope>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_policy_resources"
    )]
    pub policy_resources: Vec<AccessPolicyResource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at_unix_seconds: Option<u64>,
    /// For a remembered entry: display data that never extends or ends it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at_unix_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_unix_seconds: Option<u64>,
    /// Connected automation connections this entry authorizes now.
    pub connections: u32,
    /// Controllers this entry authorized that are held now.
    pub controllers: u32,
    /// Policy rules under which the same agent keeps reaching this
    /// workspace after a revoke; they must be edited in the policy file.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_policy_rule_ids"
    )]
    pub policy_rules: Vec<String>,
}

impl AccessEntry {
    /// Checks the bounds and the per-kind shape of one entry.
    ///
    /// # Errors
    /// Names the first violated rule.
    pub fn validate(&self) -> Result<(), &'static str> {
        self.validate_bounds()?;
        self.validate_shape()
    }

    fn validate_bounds(&self) -> Result<(), &'static str> {
        if !self.key.is_valid() {
            return Err("access entry key is out of bounds");
        }
        let identity = &self.identity;
        if identity.executable_path.is_empty()
            || identity.executable_path.len() > MAX_ACCESS_PATH_BYTES
        {
            return Err("access entry executable path is out of bounds");
        }
        if identity
            .display_name
            .as_ref()
            .is_some_and(|name| name.len() > MAX_ACCESS_DISPLAY_NAME_BYTES)
        {
            return Err("access entry display name exceeds the limit");
        }
        if self
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.name.as_ref())
            .is_some_and(|name| name.len() > MAX_ACCESS_WORKSPACE_NAME_BYTES)
        {
            return Err("access entry workspace name exceeds the limit");
        }
        if self
            .policy_rules
            .iter()
            .any(|rule| rule.is_empty() || rule.len() > MAX_ACCESS_RULE_ID_BYTES)
        {
            return Err("access entry policy rule ID is out of bounds");
        }
        if self
            .level
            .is_some_and(|level| AccessLevel::exactly(&self.scopes) != Some(level))
        {
            return Err("access entry level does not match its scopes");
        }
        Ok(())
    }

    fn validate_shape(&self) -> Result<(), &'static str> {
        let policy = self.kind == AccessEntryKind::PersistentPolicy;
        let key_matches = matches!(
            (&self.key, self.kind),
            (
                AccessEntryKey::Remembered { .. },
                AccessEntryKind::Remembered
            ) | (
                AccessEntryKey::Grant { .. },
                AccessEntryKind::Once | AccessEntryKind::WorkspaceSession
            ) | (AccessEntryKey::Pending { .. }, AccessEntryKind::Pending)
                | (
                    AccessEntryKey::Policy { .. },
                    AccessEntryKind::PersistentPolicy
                )
        );
        if !key_matches {
            return Err("access entry key does not match its kind");
        }
        if self.revocable
            != matches!(
                (self.kind, self.state),
                (
                    AccessEntryKind::Once | AccessEntryKind::WorkspaceSession,
                    AccessEntryState::Active
                ) | (
                    AccessEntryKind::Remembered,
                    AccessEntryState::Active | AccessEntryState::WorkspaceMissing
                )
            )
        {
            return Err("access entry revocability does not match its kind and state");
        }
        if policy {
            if self.workspace.is_some()
                || !self.scopes.is_empty()
                || self.policy_scopes.is_empty()
                || self.policy_resources.is_empty()
                || self.grant_id.is_some()
                || self.state != AccessEntryState::Active
            {
                return Err("policy access entry has a grant shape");
            }
        } else if (self.workspace.is_none() && self.splint_id.is_none())
            || self.scopes.is_empty()
            || !self.policy_scopes.is_empty()
            || !self.policy_resources.is_empty()
        {
            return Err("grant access entry has a policy shape");
        }
        if let AccessEntryKey::Grant { grant_id } = self.key
            && self.grant_id != Some(grant_id)
        {
            return Err("runtime grant entry names another grant ID");
        }
        if self.kind == AccessEntryKind::Pending && self.grant_id.is_some() {
            return Err("pending access entry carries a grant ID");
        }
        if self.splint_id.is_some() != self.incarnation.is_some()
            || (self.splint_id.is_some() && self.kind != AccessEntryKind::Once)
        {
            return Err("only a once grant for one Splint names a Splint incarnation");
        }
        let state_fits = match self.kind {
            AccessEntryKind::Pending => {
                matches!(
                    self.state,
                    AccessEntryState::Prompting | AccessEntryState::Queued
                )
            }
            AccessEntryKind::Remembered => !matches!(
                self.state,
                AccessEntryState::Prompting | AccessEntryState::Queued
            ),
            _ => self.state == AccessEntryState::Active,
        };
        if !state_fits {
            return Err("access entry state does not fit its kind");
        }
        Ok(())
    }
}

/// Whether the remembered source can be used, as the inventory reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum RememberedStoreStatus {
    Available,
    /// Unsafe, corrupt, oversized, of an unsupported version, or retired:
    /// remembered access denies everything until it is repaired.
    Unavailable {
        reason: String,
    },
    /// This daemon has no state directory (tests).
    NotConfigured,
}

/// The durable side of revoking remembered grants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum DurableRemoval {
    /// Nothing durable was involved.
    NotNeeded,
    /// The store no longer holds it.
    Written,
    /// The grant is revoked now, but the store still holds it, so it returns
    /// after a restart until a later write (or shutdown) removes it.
    Failed { message: String },
}

/// What a per-entry revoke did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessRevocation {
    pub key: AccessEntryKey,
    pub kind: AccessEntryKind,
    /// The runtime grant whose controllers and subscriptions ended.
    pub grant_id: u64,
    /// The workspace it covered, when it is still known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lair_id: Option<LairId>,
    pub durable_removal: DurableRemoval,
    /// Policy rules under which the agent still reaches the workspace.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "deserialize_policy_rule_ids"
    )]
    pub policy_rules: Vec<String>,
}

/// Exact counts for **Revoke all access**.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessRevocationCounts {
    /// "Just this once" grants, for a Lair or one Splint.
    pub once: u32,
    pub workspace_session: u32,
    /// Remembered grants removed, the number the person confirms.
    pub remembered: u32,
    pub pending_requests: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    AuthenticationFailed,
    HandshakeRequired,
    IncompatibleVersion,
    InvalidFrame,
    FrameTooLarge,
    InvalidRequestId,
    DuplicateRequestId,
    TooManyOutstandingRequests,
    DevelopmentFeatureDisabled,
    UnsupportedOperation,
    ConsentUnavailable,
    ConsentDenied,
    Unauthorized,
    ControllerUnavailable,
    ControlTransferUnavailable,
    StaleTopology,
    NotFound,
    StaleIncarnation,
    ImageContentNotFound,
    StaleImageContent,
    InvalidArgument,
    ResourceLimit,
    /// A per-target admission rate was exceeded; retry after a short delay.
    RateLimited,
    Cancelled,
    RequestNotFound,
    Internal,
    /// The daemon is handing off: mutations, input, resize, and control
    /// transfer are fenced until the handoff completes or is cancelled.
    HandoffInProgress,
    /// A daemon handoff cannot start; `ProtocolError::handoff_blocking_reasons`
    /// names every reason.
    HandoffUnavailable,
    /// A handoff resume ticket is invalid, expired, replayed, or mismatched.
    InvalidHandoffTicket,
}

/// Deserializes a sequence, failing as soon as it holds more than `MAX`
/// elements, so the resulting `Vec` never grows past the bound.
///
/// This caps the decoded list, not peak decoding memory: the enclosing
/// `#[serde(tag = "type")]` frame enums buffer the whole frame body into
/// serde content before any visitor runs, so peak memory is bounded by
/// [`MAX_FRAME_BYTES`].
fn deserialize_bounded_vec<'de, D, T, const MAX: usize>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct BoundedVec<T, const MAX: usize>(std::marker::PhantomData<T>);

    impl<'de, T: Deserialize<'de>, const MAX: usize> serde::de::Visitor<'de> for BoundedVec<T, MAX> {
        type Value = Vec<T>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "a sequence of at most {MAX} elements")
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Vec<T>, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(MAX));
            while let Some(value) = sequence.next_element()? {
                if values.len() == MAX {
                    return Err(serde::de::Error::invalid_length(MAX + 1, &self));
                }
                values.push(value);
            }
            Ok(values)
        }
    }

    deserializer.deserialize_seq(BoundedVec::<T, MAX>(std::marker::PhantomData))
}

fn deserialize_access_entries<'de, D>(deserializer: D) -> Result<Vec<AccessEntry>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_bounded_vec::<D, _, { MAX_ACCESS_INVENTORY_PAGE as usize }>(deserializer)
}

/// Every closed policy scope once.
const MAX_POLICY_SCOPES: usize = 19;

fn deserialize_policy_scopes<'de, D>(deserializer: D) -> Result<Vec<AutomationScope>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_bounded_vec::<D, _, MAX_POLICY_SCOPES>(deserializer)
}

fn deserialize_policy_resources<'de, D>(
    deserializer: D,
) -> Result<Vec<AccessPolicyResource>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_bounded_vec::<D, _, MAX_ACCESS_POLICY_ITEMS>(deserializer)
}

fn deserialize_policy_rule_ids<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_bounded_vec::<D, _, MAX_ACCESS_POLICY_ITEMS>(deserializer)
}

fn deserialize_access_scopes<'de, D>(deserializer: D) -> Result<Vec<AccessScope>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_bounded_vec::<D, _, MAX_ACCESS_SCOPES>(deserializer)
}

fn deserialize_consent_durations<'de, D>(
    deserializer: D,
) -> Result<Vec<ConsentDurationOffer>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_bounded_vec::<D, _, MAX_CONSENT_DURATIONS>(deserializer)
}

fn deserialize_blocking_reasons<'de, D>(
    deserializer: D,
) -> Result<Vec<HandoffBlockingReason>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_bounded_vec::<D, _, MAX_HANDOFF_BLOCKING_REASONS>(deserializer)
}

fn deserialize_reported_lairs<'de, D>(deserializer: D) -> Result<Vec<LairId>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_bounded_vec::<D, _, MAX_HANDOFF_REPORTED_LAIRS>(deserializer)
}

fn deserialize_claimed_controllers<'de, D>(
    deserializer: D,
) -> Result<Vec<HandoffClaimedController>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_bounded_vec::<D, _, MAX_HANDOFF_CLAIMED_CONTROLLERS>(deserializer)
}

fn deserialize_unresponsive_windows<'de, D>(
    deserializer: D,
) -> Result<Vec<HandoffUnresponsiveWindow>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_bounded_vec::<D, _, MAX_HANDOFF_WINDOWS>(deserializer)
}

fn deserialize_window_dojos<'de, D>(deserializer: D) -> Result<Vec<HandoffWindowDojo>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_bounded_vec::<D, _, MAX_HANDOFF_WINDOW_DOJOS>(deserializer)
}

fn deserialize_handoff_window_indices<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_bounded_vec::<D, _, MAX_HANDOFF_WINDOWS>(deserializer)
}

/// One inclusive, nonzero version range a daemon build supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffVersionRange {
    pub minimum: u16,
    pub maximum: u16,
}

impl HandoffVersionRange {
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.minimum != 0 && self.minimum <= self.maximum
    }
}

/// How a daemon build was produced, as compiled into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffBuildProvenance {
    Unpackaged,
    LocalCommittedHead,
    ReleaseCandidate,
    NixStore,
}

/// The running daemon build, as it reports itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffBuild {
    /// Package version, such as `0.2.0-dev`; at most
    /// [`MAX_HANDOFF_BUILD_VERSION_BYTES`] of ASCII alphanumerics, `.`, `-`,
    /// and `+`.
    pub version: String,
    /// Immutable daemon/client pair identity, shared with packaging and
    /// sealed preflight. Never all zero.
    pub identity: [u8; HANDOFF_BUILD_IDENTITY_BYTES],
    pub provenance: HandoffBuildProvenance,
}

/// Why a daemon handoff cannot start now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum HandoffBlockingReason {
    /// Transient Lairs never cross a handoff (operator decision D1), so each
    /// must close first. `count` is exact; `lair_ids` names at most
    /// [`MAX_HANDOFF_REPORTED_LAIRS`] of them, so a Window can tell whether
    /// its own transient Lair blocks the upgrade.
    TransientLairsOpen {
        count: usize,
        #[serde(deserialize_with = "deserialize_reported_lairs")]
        lair_ids: Vec<LairId>,
    },
    /// The running daemon is not installed from a package: there is no
    /// installed build to hand off to and no rollback authority.
    NotPackaged,
    /// The running daemon is packaged, but its startup rollback snapshot pair
    /// could not be retained, so a failed handoff could not roll back.
    RollbackUnavailable,
    /// This daemon build cannot perform a live handoff.
    Unsupported,
}

impl HandoffBlockingReason {
    const fn kind(&self) -> u8 {
        match self {
            Self::TransientLairsOpen { .. } => 0,
            Self::NotPackaged => 1,
            Self::RollbackUnavailable => 2,
            Self::Unsupported => 3,
        }
    }
}

/// Validates one bounded blocking-reason set: at most one reason of each
/// kind, and an exact, consistent transient Lair reason.
///
/// # Errors
/// Returns `InvalidArgument` for an oversized, duplicated, or inconsistent set.
pub fn validate_handoff_blocking_reasons(
    reasons: &[HandoffBlockingReason],
) -> Result<(), ProtocolError> {
    let invalid = || {
        ProtocolError::new(
            ErrorCode::InvalidArgument,
            "handoff blocking reasons are invalid",
        )
    };
    if reasons.len() > MAX_HANDOFF_BLOCKING_REASONS {
        return Err(invalid());
    }
    let mut kinds = 0_u8;
    for reason in reasons {
        let bit = 1_u8 << reason.kind();
        if kinds & bit != 0 {
            return Err(invalid());
        }
        kinds |= bit;
        if let HandoffBlockingReason::TransientLairsOpen { count, lair_ids } = reason {
            let mut unique = lair_ids.clone();
            unique.sort_unstable();
            unique.dedup();
            if *count == 0
                || lair_ids.len() > MAX_HANDOFF_REPORTED_LAIRS.min(*count)
                || unique.len() != lair_ids.len()
            {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

/// What a running daemon knows cheaply about a handoff, without opening,
/// sealing, or preflighting an installed candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffReport {
    pub daemon_generation: u64,
    pub build: HandoffBuild,
    pub private_protocol: HandoffVersionRange,
    pub handoff_protocol: HandoffVersionRange,
    pub terminal_checkpoint: HandoffVersionRange,
    pub descriptor_manifest: HandoffVersionRange,
    pub lairs: usize,
    pub transient_lairs: usize,
    pub dojos: usize,
    /// Registered live Splint runtimes, transient Lairs' included.
    pub live_splints: usize,
    /// Whether this daemon retained its startup rollback snapshot pair.
    pub rollback_available: bool,
    /// Every reason a handoff cannot start now; empty only when it could.
    #[serde(deserialize_with = "deserialize_blocking_reasons")]
    pub blocking_reasons: Vec<HandoffBlockingReason>,
}

impl HandoffReport {
    /// Validates identity, ranges, and blocking-reason consistency.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for a malformed or self-contradictory report.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_handoff_blocking_reasons(&self.blocking_reasons)?;
        let version = &self.build.version;
        let transient_count = self
            .blocking_reasons
            .iter()
            .find_map(|reason| match reason {
                HandoffBlockingReason::TransientLairsOpen { count, .. } => Some(*count),
                _ => None,
            });
        let rollback_reasons = self
            .blocking_reasons
            .iter()
            .filter(|reason| {
                matches!(
                    reason,
                    HandoffBlockingReason::NotPackaged | HandoffBlockingReason::RollbackUnavailable
                )
            })
            .count();
        if self.daemon_generation == 0
            || version.is_empty()
            || version.len() > MAX_HANDOFF_BUILD_VERSION_BYTES
            || !version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
            || self.build.identity == [0; HANDOFF_BUILD_IDENTITY_BYTES]
            || ![
                self.private_protocol,
                self.handoff_protocol,
                self.terminal_checkpoint,
                self.descriptor_manifest,
            ]
            .into_iter()
            .all(HandoffVersionRange::is_valid)
            || self.transient_lairs > self.lairs
            || transient_count != (self.transient_lairs > 0).then_some(self.transient_lairs)
            // Exactly one rollback reason explains missing rollback authority.
            || rollback_reasons != usize::from(!self.rollback_available)
            // An unpackaged build has no installed pair to roll back to.
            || (self.build.provenance == HandoffBuildProvenance::Unpackaged
                && !self
                    .blocking_reasons
                    .iter()
                    .any(|reason| matches!(reason, HandoffBlockingReason::NotPackaged)))
        {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "handoff report is invalid",
            ));
        }
        Ok(())
    }
}

/// One phase of a handoff the serving daemon broadcasts before exec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HandoffStatusPhase {
    /// The daemon is building checkpoint and descriptor authority and still
    /// serves normally.
    Preparing,
    /// Mutations, input, resize, and control transfer are paused.
    Fenced,
    /// The handoff stopped before exec. The same generation keeps serving and
    /// the fence is lifted.
    Cancelled { reason: HandoffCancelReason },
}

/// Why a handoff stopped before exec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffCancelReason {
    /// The requesting Window cancelled it.
    Requested,
    /// A blocking condition appeared after it started.
    Blocked,
    /// Preparation failed; nothing changed.
    Failed,
    /// A participant or step missed its deadline.
    TimedOut,
}

/// One controller a valid handoff control claim restored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffClaimedController {
    pub splint_id: SplintId,
    pub incarnation: u64,
    pub controller_id: u64,
    pub modes: Vec<ControlMode>,
}

/// Validates a bounded claim result with one controller per Splint.
///
/// # Errors
/// Returns `InvalidArgument` for an oversized, duplicated, or malformed set.
pub fn validate_handoff_claimed_controllers(
    controllers: &[HandoffClaimedController],
) -> Result<(), ProtocolError> {
    let invalid = || {
        ProtocolError::new(
            ErrorCode::InvalidArgument,
            "handoff claimed controllers are invalid",
        )
    };
    if controllers.len() > MAX_HANDOFF_CLAIMED_CONTROLLERS {
        return Err(invalid());
    }
    let mut splints = std::collections::HashSet::with_capacity(controllers.len());
    let mut controller_ids = std::collections::HashSet::with_capacity(controllers.len());
    for controller in controllers {
        validate_control_modes(&controller.modes)?;
        if controller.incarnation == 0
            || controller.controller_id == 0
            || !splints.insert(controller.splint_id)
            || !controller_ids.insert(controller.controller_id)
        {
            return Err(invalid());
        }
    }
    Ok(())
}

/// Identity of the sealed forward client snapshot a
/// [`ServerFrame::HandoffPrepare`] frame passes as its first descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffClientSnapshot {
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub sha256: [u8; 32],
}

/// Metadata of a [`ServerFrame::HandoffPrepare`] frame. Its descriptors
/// travel beside the frame bytes; see [`handoff_prepare`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffPrepare {
    /// Generation of the daemon that is handing off.
    pub daemon_generation: u64,
    /// Identifies this frame among every prepare frame the daemon process
    /// sends: nonzero and never reused, so one attempt's late answer never
    /// matches a later attempt's frame, which names the same generation.
    /// The Window echoes it in [`Request::AcknowledgeHandoffPrepare`].
    pub prepare_id: u64,
    /// Attached descriptor count; always [`HANDOFF_PREPARE_DESCRIPTORS`].
    pub descriptors: usize,
    pub client_snapshot: HandoffClientSnapshot,
}

impl HandoffPrepare {
    /// Validates generation, prepare id, descriptor count, and snapshot
    /// bounds.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for a malformed or out-of-bounds frame.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.daemon_generation == 0
            || self.prepare_id == 0
            || self.descriptors != HANDOFF_PREPARE_DESCRIPTORS
            || !(1..=MAX_HANDOFF_CLIENT_SNAPSHOT_BYTES).contains(&self.client_snapshot.size)
            || self.client_snapshot.sha256 == [0; 32]
        {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "handoff prepare frame is invalid",
            ));
        }
        Ok(())
    }
}

/// Validates one [`Request::AcknowledgeHandoffPrepare`]: a nonzero
/// generation and prepare id, as every prepare frame carries.
///
/// # Errors
/// Returns `InvalidArgument` for a zero generation or prepare id.
pub fn validate_handoff_acknowledgement(
    daemon_generation: u64,
    prepare_id: u64,
) -> Result<(), ProtocolError> {
    if daemon_generation == 0 || prepare_id == 0 {
        return Err(ProtocolError::new(
            ErrorCode::InvalidArgument,
            "handoff prepare acknowledgement is invalid",
        ));
    }
    Ok(())
}

/// Why a trusted local Window cannot take part in a handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffWindowIssue {
    /// The Window received its [`ServerFrame::HandoffPrepare`] but did not
    /// acknowledge it before the deadline.
    NoAcknowledgement,
    /// The daemon could not pin the Window's process, so it sent no prepare.
    Unpinned,
    /// The Window's process holds trusted connections but declared no
    /// anchor connection (`Hello.handoff_window`), so it cannot take part.
    Undeclared,
}

/// One Dojo an unresponsive Window has open, named for the prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffWindowDojo {
    pub lair_id: LairId,
    pub dojo_id: DojoId,
    /// The Dojo's name: nonempty, without surrounding whitespace, and at
    /// most [`MAX_HANDOFF_DOJO_NAME_BYTES`].
    pub name: String,
}

/// One Window a [`ServerFrame::HandoffDecisionRequired`] prompt names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffUnresponsiveWindow {
    /// The Window's index in this handoff, below [`MAX_HANDOFF_WINDOWS`].
    /// A [`ClientFrame::HandoffDecision`] names Windows by it.
    pub index: u8,
    /// The Window's client process.
    pub pid: u32,
    pub reason: HandoffWindowIssue,
    /// The Window's open Dojos the daemon can name, at most
    /// [`MAX_HANDOFF_WINDOW_DOJOS`].
    #[serde(deserialize_with = "deserialize_window_dojos")]
    pub dojos: Vec<HandoffWindowDojo>,
}

/// Validates one decision prompt: a nonzero request and generation, a
/// bounded nonzero deadline, and one to [`MAX_HANDOFF_WINDOWS`] Windows with
/// distinct in-range indices, positive Linux PIDs, and at most
/// [`MAX_HANDOFF_WINDOW_DOJOS`] distinct, well-named Dojos each.
///
/// # Errors
/// Returns `InvalidArgument` for a malformed or out-of-bounds prompt.
pub fn validate_handoff_decision_required(
    request_id: u64,
    daemon_generation: u64,
    deadline_ms: u32,
    windows: &[HandoffUnresponsiveWindow],
) -> Result<(), ProtocolError> {
    let invalid = || {
        ProtocolError::new(
            ErrorCode::InvalidArgument,
            "handoff decision prompt is invalid",
        )
    };
    if request_id == 0
        || daemon_generation == 0
        || !(1..=MAX_HANDOFF_DECISION_DEADLINE_MS).contains(&deadline_ms)
        || windows.is_empty()
        || windows.len() > MAX_HANDOFF_WINDOWS
    {
        return Err(invalid());
    }
    let mut indices = std::collections::HashSet::with_capacity(windows.len());
    let mut pids = std::collections::HashSet::with_capacity(windows.len());
    for window in windows {
        if usize::from(window.index) >= MAX_HANDOFF_WINDOWS
            || !indices.insert(window.index)
            || !i32::try_from(window.pid).is_ok_and(|pid| pid > 0)
            || !pids.insert(window.pid)
            || window.dojos.len() > MAX_HANDOFF_WINDOW_DOJOS
        {
            return Err(invalid());
        }
        let mut dojos = std::collections::HashSet::with_capacity(window.dojos.len());
        for dojo in &window.dojos {
            if !dojos.insert(dojo.dojo_id)
                || dojo.name.is_empty()
                || dojo.name.len() > MAX_HANDOFF_DOJO_NAME_BYTES
                || dojo.name.trim() != dojo.name
            {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

/// Validates one decision answer on its own: a nonzero request and one to
/// [`MAX_HANDOFF_WINDOWS`] distinct in-range Window indices. The daemon also
/// requires the indices to be exactly the Windows its prompt named.
///
/// # Errors
/// Returns `InvalidArgument` for a malformed or out-of-bounds answer.
pub fn validate_handoff_decision(
    request_id: u64,
    proceed_without: &[u8],
) -> Result<(), ProtocolError> {
    let mut indices = std::collections::HashSet::with_capacity(proceed_without.len());
    if request_id == 0
        || proceed_without.is_empty()
        || proceed_without.len() > MAX_HANDOFF_WINDOWS
        || !proceed_without
            .iter()
            .all(|&index| usize::from(index) < MAX_HANDOFF_WINDOWS && indices.insert(index))
    {
        return Err(ProtocolError::new(
            ErrorCode::InvalidArgument,
            "handoff decision is invalid",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchMatch {
    pub row_id: u64,
    pub start_column: usize,
    pub end_column: usize,
    pub preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchPage {
    pub splint_id: SplintId,
    pub incarnation: u64,
    pub terminal_revision: u64,
    pub history_generation: u64,
    pub matches: Vec<SearchMatch>,
    pub next_cursor: Option<String>,
    pub timed_out: bool,
}

impl SearchPage {
    /// Validates bounded search results and their identity correlation.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for malformed identities, ranges, previews, or cursors.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let valid_matches = self.matches.iter().all(|item| {
            item.row_id > 0
                && item.start_column < item.end_column
                && item.preview.len() <= MAX_SEARCH_PREVIEW_BYTES
        });
        if self.incarnation == 0
            || self.history_generation == 0
            || self.matches.len() > MAX_SEARCH_RESULTS
            || self
                .next_cursor
                .as_ref()
                .is_some_and(|cursor| cursor.is_empty() || cursor.len() > MAX_SEARCH_CURSOR_BYTES)
            || !valid_matches
        {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "invalid search page",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScrollbackPage {
    pub splint_id: SplintId,
    pub incarnation: u64,
    pub terminal_revision: u64,
    pub history_generation: u64,
    pub oldest_available_row_id: Option<u64>,
    pub newest_available_row_id: Option<u64>,
    pub rows: Vec<TerminalRow>,
    pub has_older: bool,
}

impl ScrollbackPage {
    /// Validates one bounded history page.
    ///
    /// # Errors
    /// Returns `InvalidArgument` for malformed identity metadata or bounds.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let ids = self
            .rows
            .iter()
            .map(|row| row.row_id)
            .collect::<Option<Vec<_>>>();
        let valid_bounds = match (self.oldest_available_row_id, self.newest_available_row_id) {
            (None, None) => self.rows.is_empty() && !self.has_older,
            (Some(oldest), Some(newest)) if oldest > 0 && oldest <= newest => {
                ids.as_ref().is_some_and(|ids| {
                    ids.iter().all(|id| *id > 0)
                        && ids.windows(2).all(|pair| pair[0] < pair[1])
                        && ids.first().is_none_or(|id| *id >= oldest)
                        && ids.last().is_none_or(|id| *id <= newest)
                        && (!self.has_older || ids.first().is_some_and(|first| *first > oldest))
                })
            }
            _ => false,
        };
        if self.incarnation == 0
            || self.history_generation == 0
            || self.rows.len() > MAX_SCROLLBACK_PAGE_ROWS
            || self
                .rows
                .iter()
                .any(|row| row.cells.len() > usize::from(MAX_COLUMNS))
            || !valid_bounds
        {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "scrollback page metadata is inconsistent",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageTransferMode {
    BinaryChunks,
    SealedMemfd,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageContentRequest {
    pub splint_id: SplintId,
    pub incarnation: u64,
    pub content_id: u64,
    pub generation: u64,
    pub digest: [u8; 32],
    pub accepted_transfers: Vec<ImageTransferMode>,
}

impl ImageContentRequest {
    /// Validates exact content identity and the bounded transport offer.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when identity or transfer metadata is malformed.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.incarnation == 0
            || self.content_id == 0
            || self.generation == 0
            || self.digest == [0; 32]
            || self.accepted_transfers.is_empty()
            || self.accepted_transfers.len() > 2
            || self
                .accepted_transfers
                .windows(2)
                .any(|pair| pair[0] == pair[1])
        {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "image content request identity or transfer offer is invalid",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageContentTransfer {
    pub splint_id: SplintId,
    pub incarnation: u64,
    pub content_id: u64,
    pub generation: u64,
    pub digest: [u8; 32],
    pub byte_length: usize,
    pub transfer: ImageTransferMode,
    pub token: [u8; IMAGE_TRANSFER_TOKEN_BYTES],
    pub token_ttl_millis: u32,
}

impl ImageContentTransfer {
    /// Validates a bounded single-use transfer grant.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when identity or transfer metadata is malformed.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.incarnation == 0
            || self.content_id == 0
            || self.generation == 0
            || self.digest == [0; 32]
            || self.byte_length == 0
            || self.byte_length > MAX_IMAGE_CONTENT_BYTES
            || self.token == [0; IMAGE_TRANSFER_TOKEN_BYTES]
            || self.token_ttl_millis == 0
            || self.token_ttl_millis > IMAGE_TRANSFER_TOKEN_TTL_MILLIS
        {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "image content transfer identity or bounds are invalid",
            ));
        }
        Ok(())
    }

    /// Validates this transfer against the exact request and advertised content.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when identity, length, digest, or negotiated mode differs.
    pub fn validate_for(
        &self,
        request: &ImageContentRequest,
        metadata: &ImageContentMetadata,
    ) -> Result<(), ProtocolError> {
        request.validate()?;
        self.validate()?;
        metadata.validate()?;
        if self.splint_id != request.splint_id
            || self.incarnation != request.incarnation
            || self.content_id != request.content_id
            || self.content_id != metadata.content_id
            || self.generation != request.generation
            || self.generation != metadata.generation
            || self.digest != request.digest
            || self.digest != metadata.digest
            || self.byte_length != metadata.byte_length
            || !request.accepted_transfers.contains(&self.transfer)
        {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "image transfer does not match its request and content metadata",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageSourceFormat {
    Sixel,
    KittyRgb,
    KittyRgba,
    KittyPng,
    Iterm2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageAlphaMode {
    Opaque,
    Premultiplied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageRetention {
    WhilePlaced,
    ExplicitDelete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageErasePolicy {
    TextOverwrite,
    ExplicitDelete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageContentMetadata {
    pub content_id: u64,
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    pub source_format: ImageSourceFormat,
    pub alpha_mode: ImageAlphaMode,
    pub digest: [u8; 32],
    pub byte_length: usize,
    pub retention: ImageRetention,
}

impl ImageContentMetadata {
    fn validate(&self) -> Result<(), ProtocolError> {
        let pixels = usize::try_from(self.width)
            .ok()
            .and_then(|width| {
                usize::try_from(self.height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .filter(|pixels| *pixels <= MAX_IMAGE_PIXELS);
        let expected = pixels.and_then(|pixels| pixels.checked_mul(4));
        if self.content_id == 0
            || self.generation == 0
            || self.width == 0
            || self.height == 0
            || self.width > MAX_IMAGE_DIMENSION
            || self.height > MAX_IMAGE_DIMENSION
            || self.digest == [0; 32]
            || expected != Some(self.byte_length)
            || self.byte_length > MAX_IMAGE_CONTENT_BYTES
        {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "image content metadata is invalid or exceeds limits",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImagePixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImagePixelSize {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImagePlacement {
    pub placement_id: u64,
    pub content_id: u64,
    pub row_id: u64,
    pub column: usize,
    pub source: ImagePixelRect,
    pub destination_columns: usize,
    pub destination_rows: usize,
    pub source_cell_size: Option<ImagePixelSize>,
    pub x_offset: i32,
    pub y_offset: i32,
    pub z_index: i32,
    pub application_image_id: Option<u32>,
    pub application_placement_id: Option<u32>,
    pub creation_order: u64,
    pub erase_policy: ImageErasePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalImagePlane {
    pub screen: ActiveScreen,
    pub contents: Vec<ImageContentMetadata>,
    pub placements: Vec<ImagePlacement>,
}

impl TerminalImagePlane {
    /// Validates bounded image metadata and placement references.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` or `ResourceLimit` for malformed or oversized planes.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.contents.len() > MAX_IMAGE_CONTENTS_PER_SPLINT
            || self.placements.len() > MAX_IMAGE_PLACEMENTS_PER_SPLINT
        {
            return Err(ProtocolError::new(
                ErrorCode::ResourceLimit,
                "terminal image plane exceeds object limits",
            ));
        }
        let mut contents = std::collections::BTreeMap::new();
        let mut total_bytes = 0_usize;
        for content in &self.contents {
            content.validate()?;
            if contents.insert(content.content_id, content).is_some() {
                return Err(ProtocolError::new(
                    ErrorCode::InvalidArgument,
                    "terminal image plane contains duplicate content identity",
                ));
            }
            total_bytes = total_bytes
                .checked_add(content.byte_length)
                .filter(|bytes| *bytes <= MAX_IMAGE_BYTES_PER_SPLINT)
                .ok_or_else(|| {
                    ProtocolError::new(
                        ErrorCode::ResourceLimit,
                        "terminal image plane exceeds its byte limit",
                    )
                })?;
        }
        let mut placement_ids = std::collections::BTreeSet::new();
        for placement in &self.placements {
            let Some(content) = contents.get(&placement.content_id) else {
                return Err(ProtocolError::new(
                    ErrorCode::InvalidArgument,
                    "terminal image placement references unknown content",
                ));
            };
            let source_end_x = placement.source.x.checked_add(placement.source.width);
            let source_end_y = placement.source.y.checked_add(placement.source.height);
            let valid_cell_size = placement.source_cell_size.is_none_or(|size| {
                size.width > 0
                    && size.height > 0
                    && size.width <= MAX_IMAGE_DIMENSION
                    && size.height <= MAX_IMAGE_DIMENSION
            });
            if placement.placement_id == 0
                || placement.row_id == 0
                || placement.creation_order == 0
                || !placement_ids.insert(placement.placement_id)
                || placement.column >= usize::from(MAX_COLUMNS)
                || placement.source.width == 0
                || placement.source.height == 0
                || source_end_x.is_none_or(|end| end > content.width)
                || source_end_y.is_none_or(|end| end > content.height)
                || placement.destination_columns == 0
                || placement.destination_columns > usize::from(MAX_COLUMNS)
                || placement.destination_rows == 0
                || placement.destination_rows > usize::from(MAX_ROWS)
                || !valid_cell_size
            {
                return Err(ProtocolError::new(
                    ErrorCode::InvalidArgument,
                    "terminal image placement is invalid or exceeds dimensions",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalUpdate {
    pub base_revision: u64,
    pub revision: u64,
    pub rows: Vec<TerminalRowPatch>,
    pub scrolls: Vec<TerminalScroll>,
    pub cursor: Option<TerminalCursor>,
    pub title: Option<String>,
    pub input_modes: Option<TerminalInputModes>,
    pub active_screen: Option<ActiveScreen>,
    pub palette: Option<Vec<u32>>,
    pub default_colors: Option<[u32; 3]>,
    pub columns: Option<usize>,
    pub row_count: Option<usize>,
    pub scrollback: Option<TerminalScrollbackUpdate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Box<TerminalImagePlane>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalScrollbackUpdate {
    pub transition: HistoryTransition,
    pub history_generation: u64,
    pub oldest_available_row_id: Option<u64>,
    pub newest_available_row_id: Option<u64>,
    pub rows: Vec<TerminalRow>,
    pub available_rows: usize,
    pub omitted_oldest_rows: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryTransition {
    Append {
        appended_rows: usize,
        trimmed_rows: usize,
    },
    Clear,
    Reflow,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalRowPatch {
    pub index: usize,
    pub row: TerminalRow,
}

impl TerminalUpdate {
    /// Validates an advancing aggregate revision interval and every bound against
    /// the client's current semantic view.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when the update cannot be applied without an
    /// allocation or index exceeding negotiated protocol limits.
    #[allow(
        clippy::too_many_lines,
        reason = "wire update validation keeps every bounded field check in one transaction"
    )]
    pub fn validate_against(
        &self,
        current_revision: u64,
        current_history_generation: u64,
        current_columns: usize,
        current_rows: usize,
    ) -> Result<(), ProtocolError> {
        if self.base_revision != current_revision || self.revision <= current_revision {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "terminal update revision interval does not advance from the current state",
            ));
        }
        let columns = self.columns.unwrap_or(current_columns);
        let rows = self.row_count.unwrap_or(current_rows);
        if columns == 0 || columns > usize::from(MAX_COLUMNS) {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "terminal update columns exceed limits",
            ));
        }
        if rows == 0 || rows > usize::from(MAX_ROWS) {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "terminal update rows exceed limits",
            ));
        }
        if self.rows.len() > MAX_UPDATE_ROW_PATCHES {
            return Err(ProtocolError::new(
                ErrorCode::ResourceLimit,
                "terminal update contains too many row patches",
            ));
        }
        if self.scrolls.len() > MAX_UPDATE_SCROLLS {
            return Err(ProtocolError::new(
                ErrorCode::ResourceLimit,
                "terminal update contains too many scroll operations",
            ));
        }
        if let Some(scrollback) = &self.scrollback {
            let generation_valid = match scrollback.transition {
                HistoryTransition::Append { .. } => {
                    scrollback.history_generation == current_history_generation
                }
                HistoryTransition::Clear | HistoryTransition::Reflow => {
                    scrollback.history_generation > current_history_generation
                }
                HistoryTransition::Replace => {
                    scrollback.history_generation >= current_history_generation
                }
            };
            if !generation_valid {
                return Err(ProtocolError::new(
                    ErrorCode::InvalidArgument,
                    "terminal update history generation transition is invalid",
                ));
            }
            if matches!(
                scrollback.transition,
                HistoryTransition::Append {
                    appended_rows,
                    trimmed_rows,
                } if appended_rows == 0
                    || appended_rows > usize::from(MAX_ROWS)
                    || trimmed_rows > appended_rows
            ) {
                return Err(ProtocolError::new(
                    ErrorCode::InvalidArgument,
                    "terminal update history transition is invalid",
                ));
            }
            if matches!(scrollback.transition, HistoryTransition::Clear)
                && scrollback.available_rows != 0
            {
                return Err(ProtocolError::new(
                    ErrorCode::InvalidArgument,
                    "terminal update clear transition retains history",
                ));
            }
            if scrollback.history_generation == 0 {
                return Err(ProtocolError::new(
                    ErrorCode::InvalidArgument,
                    "terminal update history generation must be non-zero",
                ));
            }
            if scrollback.rows.len() > MAX_SNAPSHOT_SCROLLBACK_ROWS {
                return Err(ProtocolError::new(
                    ErrorCode::ResourceLimit,
                    "terminal update has too many scrollback rows",
                ));
            }
            if scrollback
                .rows
                .iter()
                .any(|row| row.cells.len() > usize::from(MAX_COLUMNS))
            {
                return Err(ProtocolError::new(
                    ErrorCode::ResourceLimit,
                    "terminal update scrollback row exceeds column limit",
                ));
            }
            if scrollback.rows.len() > scrollback.available_rows {
                return Err(ProtocolError::new(
                    ErrorCode::InvalidArgument,
                    "terminal update returns more scrollback rows than available",
                ));
            }
            if !valid_scrollback_identity(
                &scrollback.rows,
                scrollback.available_rows,
                scrollback.omitted_oldest_rows,
                scrollback.oldest_available_row_id,
                scrollback.newest_available_row_id,
            ) {
                return Err(ProtocolError::new(
                    ErrorCode::InvalidArgument,
                    "terminal update scrollback identity metadata is inconsistent",
                ));
            }
            if scrollback.omitted_oldest_rows
                != scrollback
                    .available_rows
                    .saturating_sub(scrollback.rows.len())
            {
                return Err(ProtocolError::new(
                    ErrorCode::InvalidArgument,
                    "terminal update scrollback omitted count is inconsistent",
                ));
            }
        }
        let mut seen = vec![false; rows];
        let mut seen_row_ids = std::collections::BTreeSet::new();
        for patch in &self.rows {
            let valid_row_id = patch
                .row
                .row_id
                .is_some_and(|id| id > 0 && seen_row_ids.insert(id));
            if patch.index >= rows
                || !valid_row_id
                || patch.row.cells.len() > columns
                || seen[patch.index]
            {
                return Err(ProtocolError::new(
                    ErrorCode::InvalidArgument,
                    "terminal row patch is duplicate or exceeds dimensions",
                ));
            }
            seen[patch.index] = true;
        }
        for scroll in &self.scrolls {
            let region_rows = scroll.end_row.saturating_sub(scroll.start_row);
            if scroll.start_row >= scroll.end_row
                || scroll.end_row > rows
                || scroll.rows == 0
                || scroll.rows > region_rows
            {
                return Err(ProtocolError::new(
                    ErrorCode::InvalidArgument,
                    "terminal scroll exceeds dimensions",
                ));
            }
        }
        if self
            .palette
            .as_ref()
            .is_some_and(|palette| palette.len() != 256)
        {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "terminal update palette must contain 256 colors",
            ));
        }
        if let Some(images) = &self.images {
            images.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCursor {
    pub column: i32,
    pub row: i32,
    pub deferred_wrap: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalScroll {
    pub direction: ScrollDirection,
    pub start_row: usize,
    pub end_row: usize,
    pub rows: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrollDirection {
    Forward,
    Reverse,
}

/// Scoped public-facing identity for one authorized terminal result.
///
/// This private wire DTO lets non-graphical clients project terminal results
/// without issuing a broader topology request. Terminal revision and history
/// generation must match the adjacent snapshot, page, or resync state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalProvenance {
    pub lair_id: LairId,
    pub dojo_id: DojoId,
    pub splint_id: SplintId,
    pub incarnation: u64,
    pub topology_revision: TopologyRevision,
    pub terminal_revision: u64,
    pub history_generation: u64,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalSnapshot {
    pub splint_id: SplintId,
    pub incarnation: u64,
    pub revision: u64,
    pub columns: usize,
    pub rows: usize,
    pub cursor_column: i32,
    pub cursor_row: i32,
    pub cursor_deferred_wrap: bool,
    pub active_screen: ActiveScreen,
    pub input_modes: TerminalInputModes,
    pub palette: Vec<u32>,
    pub default_colors: [u32; 3],
    pub title: String,
    pub visible_rows: Vec<TerminalRow>,
    pub history_generation: u64,
    pub oldest_available_scrollback_row_id: Option<u64>,
    pub newest_available_scrollback_row_id: Option<u64>,
    pub scrollback_rows: Vec<TerminalRow>,
    pub available_scrollback_rows: usize,
    pub omitted_oldest_scrollback_rows: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Box<TerminalImagePlane>>,
    pub exited_code: Option<i32>,
    pub exited_signal: Option<i32>,
}

impl TerminalSnapshot {
    /// Validates all snapshot dimensions and bounded scrollback identity metadata.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when the snapshot is inconsistent or exceeds
    /// negotiated protocol limits.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.columns == 0
            || self.columns > usize::from(MAX_COLUMNS)
            || self.rows == 0
            || self.rows > usize::from(MAX_ROWS)
            || self.visible_rows.len() != self.rows
            || self.palette.len() != 256
            || self.history_generation == 0
        {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "terminal snapshot dimensions or metadata are invalid",
            ));
        }
        if self
            .visible_rows
            .iter()
            .any(|row| row.cells.len() > self.columns)
            || !valid_visible_identity(&self.visible_rows, &self.scrollback_rows)
            || self.scrollback_rows.len() > MAX_SNAPSHOT_SCROLLBACK_ROWS
            || self
                .scrollback_rows
                .iter()
                .any(|row| row.cells.len() > self.columns)
            || self.omitted_oldest_scrollback_rows
                != self
                    .available_scrollback_rows
                    .saturating_sub(self.scrollback_rows.len())
            || !valid_scrollback_identity(
                &self.scrollback_rows,
                self.available_scrollback_rows,
                self.omitted_oldest_scrollback_rows,
                self.oldest_available_scrollback_row_id,
                self.newest_available_scrollback_row_id,
            )
        {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "terminal snapshot scrollback metadata is inconsistent",
            ));
        }
        if let Some(images) = &self.images {
            images.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveScreen {
    Normal,
    Alternate,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "wire input modes are independent terminal semantics"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalInputModes {
    pub application_cursor: bool,
    pub application_keypad: bool,
    pub focus_reporting: bool,
    pub bracketed_paste: bool,
    pub cursor_visible: bool,
    pub cursor_blink: bool,
    pub mouse_tracking: MouseTracking,
    pub sgr_mouse: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseTracking {
    None,
    Normal,
    Button,
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalRow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_id: Option<u64>,
    #[serde(default, skip_serializing_if = "bool_is_false")]
    pub linebreak: bool,
    pub cells: Vec<TerminalCell>,
}

fn valid_visible_identity(visible: &[TerminalRow], scrollback: &[TerminalRow]) -> bool {
    let mut ids = std::collections::BTreeSet::new();
    visible
        .iter()
        .all(|row| row.row_id.is_some_and(|id| id > 0 && ids.insert(id)))
        && scrollback
            .iter()
            .all(|row| row.row_id.is_none_or(|id| !ids.contains(&id)))
}

fn valid_scrollback_identity(
    rows: &[TerminalRow],
    available_rows: usize,
    omitted_oldest_rows: usize,
    oldest: Option<u64>,
    newest: Option<u64>,
) -> bool {
    if available_rows == 0 {
        return rows.is_empty() && oldest.is_none() && newest.is_none();
    }
    if rows.is_empty() {
        return oldest.is_none() && newest.is_none();
    }
    let (Some(oldest), Some(newest)) = (oldest, newest) else {
        return false;
    };
    let ids = rows
        .iter()
        .map(|row| row.row_id)
        .collect::<Option<Vec<_>>>();
    let Some(ids) = ids else {
        return false;
    };
    oldest > 0
        && newest > 0
        && ids.iter().all(|id| *id > 0)
        && ids.windows(2).all(|pair| pair[0] < pair[1])
        && ids.last().copied() == Some(newest)
        && (omitted_oldest_rows > 0 && ids.first().is_some_and(|first| oldest < *first)
            || omitted_oldest_rows == 0 && ids.first().copied() == Some(oldest))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCell {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spacer_remaining: Option<u32>,
    #[serde(default, skip_serializing_if = "cell_attributes_are_default")]
    pub attributes: CellAttributes,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnderlineStyle {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip predicate receives a reference"
)]
fn bool_is_false(value: &bool) -> bool {
    !*value
}

fn cell_attributes_are_default(attributes: &CellAttributes) -> bool {
    *attributes == CellAttributes::default()
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "wire rendition flags are independent terminal semantics"
)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CellAttributes {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: UnderlineStyle,
    pub underline_color_source: ColorSource,
    pub underline_color: u32,
    pub strikethrough: bool,
    pub blink: bool,
    pub conceal: bool,
    pub reverse: bool,
    pub foreground_source: ColorSource,
    pub foreground: u32,
    pub background_source: ColorSource,
    pub background: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorSource {
    #[default]
    Default,
    Base16,
    Base256,
    Rgb,
}

const CELL_ATTRIBUTE_FLAG_BOLD: u8 = 1 << 0;
const CELL_ATTRIBUTE_FLAG_DIM: u8 = 1 << 1;
const CELL_ATTRIBUTE_FLAG_ITALIC: u8 = 1 << 2;
const CELL_ATTRIBUTE_FLAG_STRIKETHROUGH: u8 = 1 << 3;
const CELL_ATTRIBUTE_FLAG_BLINK: u8 = 1 << 4;
const CELL_ATTRIBUTE_FLAG_CONCEAL: u8 = 1 << 5;
const CELL_ATTRIBUTE_FLAG_REVERSE: u8 = 1 << 6;
const CELL_ATTRIBUTE_KNOWN_FLAGS: u8 = CELL_ATTRIBUTE_FLAG_BOLD
    | CELL_ATTRIBUTE_FLAG_DIM
    | CELL_ATTRIBUTE_FLAG_ITALIC
    | CELL_ATTRIBUTE_FLAG_STRIKETHROUGH
    | CELL_ATTRIBUTE_FLAG_BLINK
    | CELL_ATTRIBUTE_FLAG_CONCEAL
    | CELL_ATTRIBUTE_FLAG_REVERSE;

type WireCellAttributes = (u8, u8, u8, u32, u8, u32, u8, u32);

impl Serialize for CellAttributes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut flags = 0;
        flags |= u8::from(self.bold) * CELL_ATTRIBUTE_FLAG_BOLD;
        flags |= u8::from(self.dim) * CELL_ATTRIBUTE_FLAG_DIM;
        flags |= u8::from(self.italic) * CELL_ATTRIBUTE_FLAG_ITALIC;
        flags |= u8::from(self.strikethrough) * CELL_ATTRIBUTE_FLAG_STRIKETHROUGH;
        flags |= u8::from(self.blink) * CELL_ATTRIBUTE_FLAG_BLINK;
        flags |= u8::from(self.conceal) * CELL_ATTRIBUTE_FLAG_CONCEAL;
        flags |= u8::from(self.reverse) * CELL_ATTRIBUTE_FLAG_REVERSE;
        (
            flags,
            underline_style_to_wire(self.underline),
            color_source_to_wire(self.underline_color_source),
            self.underline_color,
            color_source_to_wire(self.foreground_source),
            self.foreground,
            color_source_to_wire(self.background_source),
            self.background,
        )
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CellAttributes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let (
            flags,
            underline,
            underline_source,
            underline_color,
            foreground_source,
            foreground,
            background_source,
            background,
        ) = WireCellAttributes::deserialize(deserializer)?;
        if flags & !CELL_ATTRIBUTE_KNOWN_FLAGS != 0 {
            return Err(serde::de::Error::custom(
                "terminal cell attributes contain unknown flags",
            ));
        }
        Ok(Self {
            bold: flags & CELL_ATTRIBUTE_FLAG_BOLD != 0,
            dim: flags & CELL_ATTRIBUTE_FLAG_DIM != 0,
            italic: flags & CELL_ATTRIBUTE_FLAG_ITALIC != 0,
            underline: underline_style_from_wire(underline).ok_or_else(|| {
                serde::de::Error::custom("terminal cell underline style is invalid")
            })?,
            underline_color_source: color_source_from_wire(underline_source).ok_or_else(|| {
                serde::de::Error::custom("terminal cell underline color source is invalid")
            })?,
            underline_color,
            strikethrough: flags & CELL_ATTRIBUTE_FLAG_STRIKETHROUGH != 0,
            blink: flags & CELL_ATTRIBUTE_FLAG_BLINK != 0,
            conceal: flags & CELL_ATTRIBUTE_FLAG_CONCEAL != 0,
            reverse: flags & CELL_ATTRIBUTE_FLAG_REVERSE != 0,
            foreground_source: color_source_from_wire(foreground_source).ok_or_else(|| {
                serde::de::Error::custom("terminal cell foreground source is invalid")
            })?,
            foreground,
            background_source: color_source_from_wire(background_source).ok_or_else(|| {
                serde::de::Error::custom("terminal cell background source is invalid")
            })?,
            background,
        })
    }
}

const fn underline_style_to_wire(style: UnderlineStyle) -> u8 {
    match style {
        UnderlineStyle::None => 0,
        UnderlineStyle::Single => 1,
        UnderlineStyle::Double => 2,
        UnderlineStyle::Curly => 3,
        UnderlineStyle::Dotted => 4,
        UnderlineStyle::Dashed => 5,
    }
}

const fn underline_style_from_wire(value: u8) -> Option<UnderlineStyle> {
    match value {
        0 => Some(UnderlineStyle::None),
        1 => Some(UnderlineStyle::Single),
        2 => Some(UnderlineStyle::Double),
        3 => Some(UnderlineStyle::Curly),
        4 => Some(UnderlineStyle::Dotted),
        5 => Some(UnderlineStyle::Dashed),
        _ => None,
    }
}

const fn color_source_to_wire(source: ColorSource) -> u8 {
    match source {
        ColorSource::Default => 0,
        ColorSource::Base16 => 1,
        ColorSource::Base256 => 2,
        ColorSource::Rgb => 3,
    }
}

const fn color_source_from_wire(value: u8) -> Option<ColorSource> {
    match value {
        0 => Some(ColorSource::Default),
        1 => Some(ColorSource::Base16),
        2 => Some(ColorSource::Base256),
        3 => Some(ColorSource::Rgb),
        _ => None,
    }
}

/// Encodes one bounded JSON frame with a network-order 32-bit length prefix.
///
/// # Errors
/// Returns a serialization error or [`FrameEncodeError::TooLarge`].
pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, FrameEncodeError> {
    let mut frame = Vec::new();
    frame.extend_from_slice(&[0; 4]);
    serde_json::to_writer(&mut frame, value).map_err(FrameEncodeError::Serialize)?;
    let body_len = frame.len() - 4;
    if body_len > MAX_FRAME_BYTES || body_len > u32::MAX as usize {
        return Err(FrameEncodeError::TooLarge);
    }
    let length = u32::try_from(body_len).map_err(|_| FrameEncodeError::TooLarge)?;
    frame[..4].copy_from_slice(&length.to_be_bytes());
    Ok(frame)
}

#[derive(Debug, thiserror::Error)]
pub enum FrameEncodeError {
    #[error("frame exceeds the protocol limit")]
    TooLarge,
    #[error("frame serialization failed")]
    Serialize(#[source] serde_json::Error),
}

fn terminal_frame_transaction_eligible(value: &ServerFrame) -> bool {
    matches!(
        value,
        ServerFrame::Response {
            result: Response::Attached { .. },
            ..
        } | ServerFrame::Event {
            event: SubscriptionEvent::Snapshot { .. } | SubscriptionEvent::Update { .. },
            ..
        }
    )
}

/// Encodes one server frame directly or as one bounded, contiguous terminal
/// transaction when its exact JSON body exceeds the individual frame limit.
///
/// # Errors
/// Returns `TooLarge` when the complete body exceeds the aggregate transaction
/// ceiling, or `Serialize` when the server frame cannot be encoded.
pub fn encode_server_frame_transaction(
    value: &ServerFrame,
) -> Result<Vec<Vec<u8>>, FrameEncodeError> {
    let body = serde_json::to_vec(value).map_err(FrameEncodeError::Serialize)?;
    if body.len() <= MAX_FRAME_BYTES {
        return encode_frame(value).map(|frame| vec![frame]);
    }
    if !terminal_frame_transaction_eligible(value) || body.len() > MAX_TERMINAL_TRANSACTION_BYTES {
        return Err(FrameEncodeError::TooLarge);
    }
    let chunks = body.len().div_ceil(MAX_TERMINAL_TRANSACTION_CHUNK_BYTES);
    if !(2..=MAX_TERMINAL_TRANSACTION_CHUNKS).contains(&chunks) {
        return Err(FrameEncodeError::TooLarge);
    }
    let mut frames = Vec::with_capacity(chunks + 1);
    frames.push(encode_frame(&ServerFrame::TerminalTransactionStart {
        total_bytes: body.len(),
        chunks,
    })?);
    for (index, chunk) in body
        .chunks(MAX_TERMINAL_TRANSACTION_CHUNK_BYTES)
        .enumerate()
    {
        frames.push(encode_frame(&ServerFrame::TerminalTransactionChunk {
            index,
            data: BASE64_STANDARD.encode(chunk),
        })?);
    }
    Ok(frames)
}

#[derive(Debug, Default)]
pub struct ServerFrameTransactionAssembler {
    pending: Option<PendingServerFrameTransaction>,
}

#[derive(Debug)]
struct PendingServerFrameTransaction {
    total_bytes: usize,
    chunks: usize,
    next_index: usize,
    body: Vec<u8>,
}

impl ServerFrameTransactionAssembler {
    #[must_use]
    pub const fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Accepts one ordered wire frame and publishes a logical server frame only
    /// after a complete transaction has been validated and decoded.
    ///
    /// # Errors
    /// Returns `InvalidArgument` or `ResourceLimit` for nested, interrupted,
    /// reordered, malformed, or oversized transactions. Any failure discards
    /// the pending aggregate.
    pub fn push(&mut self, frame: ServerFrame) -> Result<Option<ServerFrame>, ProtocolError> {
        let result = self.push_inner(frame);
        if result.is_err() {
            self.pending = None;
        }
        result
    }

    fn push_inner(&mut self, frame: ServerFrame) -> Result<Option<ServerFrame>, ProtocolError> {
        match frame {
            ServerFrame::TerminalTransactionStart {
                total_bytes,
                chunks,
            } => {
                if self.pending.is_some()
                    || !(MAX_FRAME_BYTES + 1..=MAX_TERMINAL_TRANSACTION_BYTES)
                        .contains(&total_bytes)
                    || !(2..=MAX_TERMINAL_TRANSACTION_CHUNKS).contains(&chunks)
                    || total_bytes.div_ceil(MAX_TERMINAL_TRANSACTION_CHUNK_BYTES) != chunks
                {
                    return Err(ProtocolError::new(
                        ErrorCode::InvalidArgument,
                        "terminal frame transaction start is invalid",
                    ));
                }
                self.pending = Some(PendingServerFrameTransaction {
                    total_bytes,
                    chunks,
                    next_index: 0,
                    body: Vec::with_capacity(total_bytes),
                });
                Ok(None)
            }
            ServerFrame::TerminalTransactionChunk { index, data } => {
                let pending = self.pending.as_mut().ok_or_else(|| {
                    ProtocolError::new(
                        ErrorCode::InvalidArgument,
                        "terminal frame transaction chunk has no start",
                    )
                })?;
                if index != pending.next_index || index >= pending.chunks {
                    return Err(ProtocolError::new(
                        ErrorCode::InvalidArgument,
                        "terminal frame transaction chunks are out of order",
                    ));
                }
                if data.len() > MAX_TERMINAL_TRANSACTION_CHUNK_BASE64_BYTES {
                    return Err(ProtocolError::new(
                        ErrorCode::ResourceLimit,
                        "terminal frame transaction base64 chunk exceeds limits",
                    ));
                }
                let decoded = BASE64_STANDARD.decode(data).map_err(|_| {
                    ProtocolError::new(
                        ErrorCode::InvalidArgument,
                        "terminal frame transaction chunk is not valid base64",
                    )
                })?;
                if decoded.is_empty()
                    || decoded.len() > MAX_TERMINAL_TRANSACTION_CHUNK_BYTES
                    || pending.body.len().saturating_add(decoded.len()) > pending.total_bytes
                {
                    return Err(ProtocolError::new(
                        ErrorCode::ResourceLimit,
                        "terminal frame transaction chunk exceeds limits",
                    ));
                }
                pending.body.extend_from_slice(&decoded);
                pending.next_index += 1;
                if pending.next_index < pending.chunks {
                    return Ok(None);
                }
                if pending.body.len() != pending.total_bytes {
                    return Err(ProtocolError::new(
                        ErrorCode::InvalidArgument,
                        "terminal frame transaction length is inconsistent",
                    ));
                }
                let completed = self.pending.take().expect("pending transaction exists");
                let decoded =
                    serde_json::from_slice::<ServerFrame>(&completed.body).map_err(|_| {
                        ProtocolError::new(
                            ErrorCode::InvalidArgument,
                            "terminal frame transaction body is invalid",
                        )
                    })?;
                if !terminal_frame_transaction_eligible(&decoded) {
                    return Err(ProtocolError::new(
                        ErrorCode::InvalidArgument,
                        "terminal frame transaction body is not eligible",
                    ));
                }
                Ok(Some(decoded))
            }
            _frame if self.pending.is_some() => Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "terminal frame transaction was interrupted",
            )),
            frame => Ok(Some(frame)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_directories_are_bounded_and_anchored() {
        for valid in [
            LaunchDirectory::Inherit,
            LaunchDirectory::Home,
            LaunchDirectory::Absolute(PathBuf::from("/srv/projects")),
            LaunchDirectory::HomeRelative(PathBuf::from("src/work")),
        ] {
            assert!(valid.validate().is_ok(), "{valid:?}");
            let request = Request::ResolveLaunchDirectory {
                source_splint_id: SplintId::new(),
                directory: valid,
            };
            let round_trip: Request =
                serde_json::from_value(serde_json::to_value(&request).unwrap()).unwrap();
            assert_eq!(round_trip, request);
        }
        for invalid in [
            LaunchDirectory::Absolute(PathBuf::from("relative")),
            LaunchDirectory::Absolute(PathBuf::new()),
            LaunchDirectory::Absolute(PathBuf::from(format!("/{}", "x".repeat(MAX_CWD_BYTES)))),
            LaunchDirectory::HomeRelative(PathBuf::from("/absolute")),
            LaunchDirectory::HomeRelative(PathBuf::from("../escape")),
            LaunchDirectory::HomeRelative(PathBuf::from("./a")),
            LaunchDirectory::HomeRelative(PathBuf::new()),
        ] {
            assert_eq!(
                invalid.validate().unwrap_err().code,
                ErrorCode::InvalidArgument,
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn audit_operation_name_matches_its_wire_representation() {
        for operation in [
            AuditOperation::Ping,
            AuditOperation::RequestAccess,
            AuditOperation::RequestLairAccess,
            AuditOperation::AuthorizationStatus,
            AuditOperation::RevokeAccess,
            AuditOperation::ListLairs,
            AuditOperation::InspectTopology,
            AuditOperation::SubscribeTopology,
            AuditOperation::InspectSplint,
            AuditOperation::ResolveLaunchDirectory,
            AuditOperation::ReadGraphicalFocus,
            AuditOperation::PublishGraphicalFocus,
            AuditOperation::CreateLair,
            AuditOperation::SplitSplint,
            AuditOperation::RelaunchSplint,
            AuditOperation::RestoreSplint,
            AuditOperation::RestoreDojo,
            AuditOperation::RestoreLair,
            AuditOperation::SetLairRetention,
            AuditOperation::CloseSplint,
            AuditOperation::SetSplitRatio,
            AuditOperation::NewDojo,
            AuditOperation::MaterializePreset,
            AuditOperation::CloseDojo,
            AuditOperation::TerminateLair,
            AuditOperation::RenameLair,
            AuditOperation::RenameDojo,
            AuditOperation::SetDojoDefaultFocus,
            AuditOperation::RenameSplint,
            AuditOperation::Attach,
            AuditOperation::ScrollbackPage,
            AuditOperation::SearchScrollback,
            AuditOperation::AcquireControl,
            AuditOperation::SubscribeControl,
            AuditOperation::RequestControlTransfer,
            AuditOperation::DecideControlTransfer,
            AuditOperation::ForceControlTransfer,
            AuditOperation::ReleaseControl,
            AuditOperation::Input,
            AuditOperation::Resize,
            AuditOperation::Detach,
            AuditOperation::KillSplint,
            AuditOperation::ProcessExit,
            AuditOperation::AuditInspect,
            AuditOperation::PolicyReload,
            AuditOperation::AgentStatusReport,
            AuditOperation::PrepareDaemonHandoff,
            AuditOperation::ClaimHandoffControl,
            AuditOperation::AcknowledgeHandoffPrepare,
            AuditOperation::SubscribeConsentOffers,
            AuditOperation::AnswerConsentOffer,
        ] {
            assert_eq!(
                serde_json::to_string(&operation).unwrap(),
                format!("\"{}\"", operation.as_str())
            );
        }
    }

    #[test]
    fn hello_hostname_is_additive_and_old_clients_ignore_it() {
        #[derive(serde::Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum OldServerFrame {
            Hello {
                version: u16,
                limits: ServerLimits,
                development_terminal_access: bool,
            },
        }
        let old = ServerFrame::Hello {
            version: PROTOCOL_VERSION,
            limits: ServerLimits::default(),
            development_terminal_access: false,
            daemon_hostname: None,
            daemon_generation: 1,
        };
        let old_json = serde_json::to_value(&old).unwrap();
        assert!(old_json.get("daemon_hostname").is_none());
        assert_eq!(
            serde_json::from_value::<ServerFrame>(old_json.clone()).unwrap(),
            old
        );
        let mut new_json = old_json;
        new_json["daemon_hostname"] = serde_json::json!("daemon-actual.example");
        let decoded: ServerFrame = serde_json::from_value(new_json.clone()).unwrap();
        assert!(
            matches!(decoded, ServerFrame::Hello { daemon_hostname: Some(ref host), .. } if host == "daemon-actual.example")
        );
        let OldServerFrame::Hello {
            version,
            limits,
            development_terminal_access,
        } = serde_json::from_value(new_json).unwrap();
        assert_eq!(version, PROTOCOL_VERSION);
        assert_eq!(limits, ServerLimits::default());
        assert!(!development_terminal_access);
    }

    #[test]
    fn hello_hostname_rejects_unusable_metadata_without_losing_the_handshake() {
        for hostname in [
            String::new(),
            "a".repeat(MAX_DAEMON_HOSTNAME_BYTES + 1),
            "bad\nname".to_owned(),
            "bad\0name".to_owned(),
            "bad\u{202e}name".to_owned(),
            "bad\u{2066}name".to_owned(),
            "bad\u{200b}name".to_owned(),
            "white space".to_owned(),
        ] {
            let frame = ServerFrame::Hello {
                version: PROTOCOL_VERSION,
                limits: ServerLimits::default(),
                development_terminal_access: false,
                daemon_hostname: Some(hostname),
                daemon_generation: 1,
            };
            let decoded: ServerFrame =
                serde_json::from_slice(&serde_json::to_vec(&frame).unwrap()).unwrap();
            assert!(matches!(
                decoded,
                ServerFrame::Hello {
                    daemon_hostname: None,
                    ..
                }
            ));
        }
        assert_eq!(
            usable_daemon_hostname("node-界.example"),
            Some("node-界.example")
        );
        assert!(usable_daemon_hostname(&"a".repeat(MAX_DAEMON_HOSTNAME_BYTES)).is_some());
    }

    #[test]
    fn frames_are_length_prefixed_and_explicit() {
        let value = ClientFrame::Hello {
            minimum_version: PROTOCOL_VERSION,
            maximum_version: PROTOCOL_VERSION,
            role: ClientRole::Automation,
            handoff_window: false,
        };
        let expected_body = serde_json::to_vec(&value).unwrap();
        let frame = encode_frame(&value).unwrap();
        assert_eq!(
            u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize,
            frame.len() - 4
        );
        assert_eq!(&frame[4..], expected_body);
        assert!(
            std::str::from_utf8(&frame[4..])
                .unwrap()
                .contains("\"type\":\"hello\"")
        );
        let remote = encode_frame(&ClientFrame::Hello {
            minimum_version: PROTOCOL_VERSION,
            maximum_version: PROTOCOL_VERSION,
            role: ClientRole::RemoteInteractive,
            handoff_window: false,
        })
        .unwrap();
        assert!(
            std::str::from_utf8(&remote[4..])
                .unwrap()
                .contains("\"role\":\"remote_interactive\"")
        );
    }

    fn decode_test_server_frame(frame: &[u8]) -> ServerFrame {
        let length = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
        assert_eq!(length, frame.len() - 4);
        serde_json::from_slice(&frame[4..]).unwrap()
    }

    #[test]
    fn oversized_terminal_frames_use_one_atomic_bounded_transaction() {
        let mut large = snapshot();
        large.columns = usize::from(MAX_COLUMNS);
        large.rows = usize::from(MAX_ROWS);
        large.visible_rows = (1..=u64::from(MAX_ROWS))
            .map(|row_id| TerminalRow {
                row_id: Some(row_id),
                linebreak: false,
                cells: vec![
                    TerminalCell {
                        content: "x".repeat(300),
                        spacer_remaining: None,
                        attributes: CellAttributes::default(),
                    };
                    usize::from(MAX_COLUMNS)
                ],
            })
            .collect();
        large.scrollback_rows.clear();
        large.available_scrollback_rows = 0;
        large.omitted_oldest_scrollback_rows = 0;
        large.oldest_available_scrollback_row_id = None;
        large.newest_available_scrollback_row_id = None;
        let logical = ServerFrame::Event {
            subscription_id: 7,
            sequence: 9,
            event: SubscriptionEvent::Snapshot { snapshot: large },
        };
        let frames = encode_server_frame_transaction(&logical).unwrap();
        assert!(frames.len() >= 3);
        assert!(
            frames
                .iter()
                .all(|frame| frame.len() - 4 <= MAX_FRAME_BYTES)
        );

        let mut assembler = ServerFrameTransactionAssembler::default();
        let mut published = None;
        for frame in frames {
            let next = assembler.push(decode_test_server_frame(&frame)).unwrap();
            assert!(published.is_none());
            published = next;
        }
        assert_eq!(published, Some(logical));
    }

    #[test]
    fn oversized_terminal_updates_use_the_same_atomic_transaction_boundary() {
        let cell = TerminalCell {
            content: "x".repeat(300),
            spacer_remaining: None,
            attributes: CellAttributes::default(),
        };
        let mut large = update();
        large.columns = Some(usize::from(MAX_COLUMNS));
        large.row_count = Some(usize::from(MAX_ROWS));
        large.rows = (0..usize::from(MAX_ROWS))
            .map(|index| TerminalRowPatch {
                index,
                row: TerminalRow {
                    row_id: Some(u64::try_from(index + 1).unwrap()),
                    linebreak: false,
                    cells: vec![cell.clone(); usize::from(MAX_COLUMNS)],
                },
            })
            .collect();
        large
            .validate_against(
                large.base_revision,
                1,
                usize::from(MAX_COLUMNS),
                usize::from(MAX_ROWS),
            )
            .unwrap();
        let logical = ServerFrame::Event {
            subscription_id: 7,
            sequence: 10,
            event: SubscriptionEvent::Update { update: large },
        };
        let frames = encode_server_frame_transaction(&logical).unwrap();
        assert!(frames.len() >= 3);
        let mut assembler = ServerFrameTransactionAssembler::default();
        let mut published = None;
        for frame in frames {
            published = assembler.push(decode_test_server_frame(&frame)).unwrap();
        }
        assert_eq!(published, Some(logical));
    }

    #[test]
    fn terminal_frame_transactions_fail_closed_on_order_and_interruption() {
        let start = ServerFrame::TerminalTransactionStart {
            total_bytes: MAX_FRAME_BYTES + 1,
            chunks: 3,
        };
        let mut assembler = ServerFrameTransactionAssembler::default();
        assert_eq!(assembler.push(start.clone()).unwrap(), None);
        assert!(
            assembler
                .push(ServerFrame::Response {
                    request_id: 1,
                    result: Response::Pong,
                })
                .is_err()
        );
        assert_eq!(assembler.push(start).unwrap(), None);
        assert!(
            assembler
                .push(ServerFrame::TerminalTransactionChunk {
                    index: 1,
                    data: BASE64_STANDARD.encode([1]),
                })
                .is_err()
        );
        assert_eq!(
            assembler
                .push(ServerFrame::Response {
                    request_id: 1,
                    result: Response::Pong,
                })
                .unwrap(),
            Some(ServerFrame::Response {
                request_id: 1,
                result: Response::Pong,
            })
        );
    }

    #[test]
    fn terminal_frame_transactions_reject_corrupt_duplicate_and_inconsistent_chunks() {
        let start = ServerFrame::TerminalTransactionStart {
            total_bytes: MAX_FRAME_BYTES + 1,
            chunks: 3,
        };
        let chunk = |index, data: &str| ServerFrame::TerminalTransactionChunk {
            index,
            data: data.to_owned(),
        };
        let mut assembler = ServerFrameTransactionAssembler::default();

        assert_eq!(assembler.push(start.clone()).unwrap(), None);
        assert!(assembler.push(chunk(0, "not base64")).is_err());

        assert_eq!(assembler.push(start.clone()).unwrap(), None);
        assert!(
            assembler
                .push(chunk(
                    0,
                    &"A".repeat(MAX_TERMINAL_TRANSACTION_CHUNK_BASE64_BYTES + 1),
                ))
                .is_err()
        );

        assert_eq!(assembler.push(start.clone()).unwrap(), None);
        assert_eq!(assembler.push(chunk(0, "AQ==")).unwrap(), None);
        assert!(assembler.push(chunk(0, "AQ==")).is_err());

        assert_eq!(assembler.push(start).unwrap(), None);
        assert_eq!(assembler.push(chunk(0, "AQ==")).unwrap(), None);
        assert_eq!(assembler.push(chunk(1, "AQ==")).unwrap(), None);
        assert!(assembler.push(chunk(2, "AQ==")).is_err());

        assert!(
            assembler
                .push(ServerFrame::TerminalTransactionStart {
                    total_bytes: MAX_TERMINAL_TRANSACTION_BYTES + 1,
                    chunks: MAX_TERMINAL_TRANSACTION_CHUNKS + 1,
                })
                .is_err()
        );
        assert!(!assembler.has_pending());
    }

    #[test]
    fn transaction_wrapped_nonterminal_frames_are_rejected_after_reassembly() {
        let logical = ServerFrame::Response {
            request_id: 1,
            result: Response::Pong,
        };
        let mut body = serde_json::to_vec(&logical).unwrap();
        body.resize(MAX_FRAME_BYTES + 1, b' ');
        let chunks = body.len().div_ceil(MAX_TERMINAL_TRANSACTION_CHUNK_BYTES);
        let mut assembler = ServerFrameTransactionAssembler::default();
        assert_eq!(
            assembler
                .push(ServerFrame::TerminalTransactionStart {
                    total_bytes: body.len(),
                    chunks,
                })
                .unwrap(),
            None
        );
        for (index, bytes) in body
            .chunks(MAX_TERMINAL_TRANSACTION_CHUNK_BYTES)
            .enumerate()
        {
            let result = assembler.push(ServerFrame::TerminalTransactionChunk {
                index,
                data: BASE64_STANDARD.encode(bytes),
            });
            if index + 1 == chunks {
                assert!(result.is_err());
            } else {
                assert_eq!(result.unwrap(), None);
            }
        }
        assert!(!assembler.has_pending());
    }

    #[test]
    fn nonterminal_frames_cannot_expand_into_aggregate_transactions() {
        let oversized = ServerFrame::Error {
            request_id: None,
            error: ProtocolError::new(ErrorCode::Internal, "x".repeat(MAX_FRAME_BYTES)),
        };
        assert!(matches!(
            encode_server_frame_transaction(&oversized),
            Err(FrameEncodeError::TooLarge)
        ));
    }

    #[test]
    fn request_correlation_is_optional_and_backward_readable() {
        let legacy = serde_json::json!({
            "type": "request",
            "request_id": 7,
            "request": { "type": "ping" }
        });
        assert!(matches!(
            serde_json::from_value::<ClientFrame>(legacy).unwrap(),
            ClientFrame::Request {
                request_id: 7,
                diagnostic_correlation: None,
                request: Request::Ping,
            }
        ));

        let correlation = DiagnosticCorrelation {
            client_instance_id: "71d82a68-11e8-47c4-9193-dd83f4b03f1a".parse().unwrap(),
            window_id: "727b26c3-2b28-4ea2-b94a-2bbfb8ce74f1".parse().unwrap(),
        };
        let frame = ClientFrame::Request {
            request_id: 8,
            diagnostic_correlation: Some(correlation),
            request: Request::Ping,
        };
        assert_eq!(
            serde_json::from_value::<ClientFrame>(serde_json::to_value(frame).unwrap()).unwrap(),
            ClientFrame::Request {
                request_id: 8,
                diagnostic_correlation: Some(correlation),
                request: Request::Ping,
            }
        );
    }

    #[test]
    fn daemon_diagnostic_schema_is_typed_and_contains_no_free_form_error_fields() {
        let event = DaemonDiagnosticEvent {
            schema_version: 1,
            timestamp_unix_ms: 42,
            component: DaemonDiagnosticComponent::Splinterd,
            event: DaemonDiagnosticEventCode::SplintClosed,
            level: DaemonDiagnosticLevel::Info,
            pid: 7,
            client_instance_id: "71d82a68-11e8-47c4-9193-dd83f4b03f1a".parse().unwrap(),
            window_id: "727b26c3-2b28-4ea2-b94a-2bbfb8ce74f1".parse().unwrap(),
            topology_revision: None,
            build_version: "test".to_owned(),
            build_commit: None,
        };
        let value = serde_json::to_value(event).unwrap();
        assert!(value.get("error").is_none());
        assert!(value.get("message").is_none());
        assert!(value.get("name").is_none());
        assert!(value.get("path").is_none());
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one fixture covers the request, its pending and cancel messages, and the grant"
    )]
    fn lair_access_request_and_grant_are_typed_and_bounded() {
        let lair_id = LairId::new();
        let request = Request::RequestLairAccess {
            lair_id,
            level: None,
            scopes: vec![
                AccessScope::Input,
                AccessScope::ControlTakeover,
                AccessScope::TopologyLayout,
            ],
            wait_millis: 4_000,
            agent_name: Some("Claude Code".to_owned()),
        };
        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(encoded["type"], "request_lair_access");
        assert!(encoded.get("level").is_none());
        assert_eq!(serde_json::from_value::<Request>(encoded).unwrap(), request);
        let by_level = Request::RequestLairAccess {
            lair_id,
            level: Some(AccessLevel::LookAndType),
            scopes: Vec::new(),
            wait_millis: 0,
            agent_name: None,
        };
        let encoded = serde_json::to_value(&by_level).unwrap();
        assert_eq!(encoded["level"], "look_and_type");
        assert!(encoded.get("scopes").is_none());
        assert!(encoded.get("agent_name").is_none());
        assert_eq!(
            serde_json::from_value::<Request>(encoded).unwrap(),
            by_level
        );
        // The wait budget is required, scopes are bounded, and unknown
        // levels never decode.
        for invalid in [
            serde_json::json!({
                "type": "request_lair_access",
                "lair_id": lair_id,
                "level": "look",
            }),
            serde_json::json!({
                "type": "request_lair_access",
                "lair_id": lair_id,
                "level": "everything",
                "wait_millis": 1,
            }),
            serde_json::json!({
                "type": "request_lair_access",
                "lair_id": lair_id,
                "scopes": vec!["observe"; MAX_ACCESS_SCOPES + 1],
                "wait_millis": 1,
            }),
        ] {
            assert!(serde_json::from_value::<Request>(invalid).is_err());
        }
        for (request, name) in [
            (
                Request::CancelLairAccessRequest {
                    lair_id,
                    request_id: 9,
                },
                "cancel_lair_access_request",
            ),
            (
                Request::CancelAllLairAccessRequests,
                "cancel_all_lair_access_requests",
            ),
        ] {
            let encoded = serde_json::to_value(&request).unwrap();
            assert_eq!(encoded["type"], name);
            assert_eq!(serde_json::from_value::<Request>(encoded).unwrap(), request);
        }
        for (response, name) in [
            (
                Response::LairAccessPending {
                    lair_id,
                    topology_revision: TopologyRevision::new(3),
                    request_id: 9,
                    expires_at_unix_seconds: 1_800_000_120,
                },
                "lair_access_pending",
            ),
            (
                Response::LairAccessRequestCancelled {
                    lair_id,
                    topology_revision: TopologyRevision::new(3),
                    request_id: 9,
                },
                "lair_access_request_cancelled",
            ),
            (
                Response::LairAccessRequestsCancelled { cancelled: 2 },
                "lair_access_requests_cancelled",
            ),
        ] {
            let encoded = serde_json::to_value(&response).unwrap();
            assert_eq!(encoded["type"], name);
            assert_eq!(
                serde_json::from_value::<Response>(encoded).unwrap(),
                response
            );
        }
        let mut refused = ProtocolError::new(ErrorCode::ResourceLimit, "pending");
        assert!(
            serde_json::to_value(&refused)
                .unwrap()
                .get("pending_request_id")
                .is_none()
        );
        refused.pending_request_id = Some(9);
        let encoded = serde_json::to_value(&refused).unwrap();
        assert_eq!(encoded["pending_request_id"], 9);
        assert_eq!(
            serde_json::from_value::<ProtocolError>(encoded).unwrap(),
            refused
        );

        let grant = LairAccessGrant {
            grant_id: 7,
            source: AccessGrantSource::Ephemeral,
            lair_id,
            scopes: vec![AccessScope::Input, AccessScope::ControlTakeover],
            requester: "/usr/bin/splinterm-mcp".to_owned(),
            expires_at_unix_seconds: 42,
        };
        let encoded = serde_json::to_value(&grant).unwrap();
        assert_eq!(encoded["lair_id"], lair_id.to_string());
        assert!(encoded.get("splint_id").is_none());
        assert_eq!(
            serde_json::from_value::<LairAccessGrant>(encoded).unwrap(),
            grant
        );
        for (source, name) in [
            (AccessGrantSource::WorkspaceSession, "workspace_session"),
            (AccessGrantSource::Remembered, "remembered"),
        ] {
            assert_eq!(serde_json::to_value(source).unwrap(), name);
        }
    }

    fn inventory_entry(key: AccessEntryKey, kind: AccessEntryKind) -> AccessEntry {
        let grant_id = match &key {
            AccessEntryKey::Grant { grant_id } => Some(*grant_id),
            AccessEntryKey::Remembered { .. } => Some(77),
            _ => None,
        };
        let policy = kind == AccessEntryKind::PersistentPolicy;
        AccessEntry {
            revocable: matches!(
                kind,
                AccessEntryKind::Once
                    | AccessEntryKind::WorkspaceSession
                    | AccessEntryKind::Remembered
            ),
            state: if kind == AccessEntryKind::Pending {
                AccessEntryState::Queued
            } else {
                AccessEntryState::Active
            },
            key,
            kind,
            grant_id,
            identity: AccessIdentity {
                kind: AccessIdentityKind::ProtectedInstall,
                executable_path: "/usr/bin/splinterm-mcp".to_owned(),
                pid: None,
                display_name: Some("claude-code".to_owned()),
            },
            workspace: (!policy).then(|| AccessWorkspace {
                lair_id: LairId::new(),
                name: Some("work".to_owned()),
            }),
            splint_id: None,
            incarnation: None,
            level: (!policy).then_some(AccessLevel::Look),
            scopes: if policy {
                Vec::new()
            } else {
                AccessLevel::Look.scopes().to_vec()
            },
            policy_scopes: if policy {
                vec![AutomationScope::TopologyMetadataRead]
            } else {
                Vec::new()
            },
            policy_resources: if policy {
                vec![AccessPolicyResource::Daemon]
            } else {
                Vec::new()
            },
            created_at_unix_seconds: Some(1_800_000_000),
            last_used_at_unix_seconds: None,
            expires_at_unix_seconds: None,
            connections: 1,
            controllers: 0,
            policy_rules: Vec::new(),
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "every new frame and its bounds are checked together"
    )]
    fn access_inventory_frames_round_trip_within_their_bounds() {
        let entry_id = format!("ra_{}", "0123456789abcdef".repeat(2));
        let keys = [
            AccessEntryKey::Remembered {
                entry_id: entry_id.clone(),
            },
            AccessEntryKey::Grant { grant_id: 3 },
            AccessEntryKey::Pending { request_id: 2 },
            AccessEntryKey::Policy {
                rule_id: "mcp-read".to_owned(),
            },
        ];
        // The inventory order: remembered, grants, pending, policy.
        let mut sorted = keys.to_vec();
        sorted.reverse();
        sorted.sort();
        assert_eq!(sorted, keys);
        for key in &keys {
            assert!(key.is_valid());
        }
        for invalid in [
            AccessEntryKey::Remembered {
                entry_id: "ra_short".to_owned(),
            },
            AccessEntryKey::Remembered {
                entry_id: format!("ra_{}", "0123456789ABCDEF".repeat(2)),
            },
            AccessEntryKey::Grant { grant_id: 0 },
            AccessEntryKey::Pending { request_id: 0 },
            AccessEntryKey::Policy {
                rule_id: "r".repeat(MAX_ACCESS_RULE_ID_BYTES + 1),
            },
        ] {
            assert!(!invalid.is_valid(), "{invalid:?}");
        }

        for (request, name) in [
            (
                Request::ListAccess {
                    after: Some(AccessEntryKey::Grant { grant_id: 3 }),
                    max_entries: MAX_ACCESS_INVENTORY_PAGE,
                },
                "list_access",
            ),
            (
                Request::RevokeAccessEntry {
                    target: AccessRevokeTarget::Remembered {
                        entry_id: entry_id.clone(),
                    },
                },
                "revoke_access_entry",
            ),
            (
                Request::RevokeAllAccess { dry_run: true },
                "revoke_all_access",
            ),
        ] {
            let encoded = serde_json::to_value(&request).unwrap();
            assert_eq!(encoded["type"], name);
            assert_eq!(serde_json::from_value::<Request>(encoded).unwrap(), request);
        }
        assert_eq!(
            serde_json::to_value(Request::ListAccess {
                after: None,
                max_entries: 1
            })
            .unwrap(),
            serde_json::json!({"type": "list_access", "max_entries": 1})
        );
        // A target is a grant or a remembered entry only.
        for invalid in [
            serde_json::json!({"type": "revoke_access_entry", "target": {"kind": "pending", "request_id": 2}}),
            serde_json::json!({"type": "revoke_access_entry", "target": {"kind": "policy", "rule_id": "r"}}),
            serde_json::json!({"type": "revoke_access_entry", "target": {"kind": "grant", "grant_id": 1, "entry_id": "x"}}),
        ] {
            assert!(serde_json::from_value::<Request>(invalid).is_err());
        }

        let entries: Vec<_> = keys
            .iter()
            .cloned()
            .zip([
                AccessEntryKind::Remembered,
                AccessEntryKind::Once,
                AccessEntryKind::Pending,
                AccessEntryKind::PersistentPolicy,
            ])
            .map(|(key, kind)| inventory_entry(key, kind))
            .collect();
        for entry in &entries {
            assert_eq!(entry.validate(), Ok(()), "{entry:?}");
        }
        let page = Response::AccessInventory {
            authorization_revision: 9,
            policy_generation: 2,
            remembered_store: RememberedStoreStatus::Unavailable {
                reason: "corrupt".to_owned(),
            },
            total_entries: 4,
            entries: entries.clone(),
            next_after: None,
        };
        let encoded = serde_json::to_value(&page).unwrap();
        assert_eq!(encoded["type"], "access_inventory");
        assert_eq!(encoded["remembered_store"]["status"], "unavailable");
        assert_eq!(encoded["entries"][0]["key"]["kind"], "remembered");
        assert_eq!(encoded["entries"][0]["grant_id"], 77);
        assert!(encoded["entries"][3].get("workspace").is_none());
        assert_eq!(serde_json::from_value::<Response>(encoded).unwrap(), page);

        let oversized = Response::AccessInventory {
            authorization_revision: 9,
            policy_generation: 2,
            remembered_store: RememberedStoreStatus::Available,
            total_entries: 65,
            entries: vec![entries[1].clone(); usize::from(MAX_ACCESS_INVENTORY_PAGE) + 1],
            next_after: None,
        };
        assert!(
            serde_json::from_value::<Response>(serde_json::to_value(&oversized).unwrap()).is_err()
        );

        for response in [
            Response::AccessEntryRevoked {
                authorization_revision: 10,
                revocation: AccessRevocation {
                    key: keys[0].clone(),
                    kind: AccessEntryKind::Remembered,
                    grant_id: 77,
                    lair_id: Some(LairId::new()),
                    durable_removal: DurableRemoval::Failed {
                        message: "read-only file system".to_owned(),
                    },
                    policy_rules: vec!["mcp-read".to_owned()],
                },
            },
            Response::AllAccessRevoked {
                authorization_revision: 11,
                dry_run: true,
                counts: AccessRevocationCounts {
                    once: 1,
                    workspace_session: 2,
                    remembered: 3,
                    pending_requests: 4,
                },
                durable_removal: DurableRemoval::NotNeeded,
            },
        ] {
            let encoded = serde_json::to_value(&response).unwrap();
            assert_eq!(
                serde_json::from_value::<Response>(encoded).unwrap(),
                response
            );
        }
    }

    #[test]
    fn access_entries_validate_their_per_kind_shape() {
        let remembered = AccessEntryKey::Remembered {
            entry_id: format!("ra_{}", "a".repeat(32)),
        };
        let valid = inventory_entry(AccessEntryKey::Grant { grant_id: 5 }, AccessEntryKind::Once);
        assert_eq!(valid.validate(), Ok(()));

        let mut wrong_kind = valid.clone();
        wrong_kind.kind = AccessEntryKind::Remembered;
        assert!(wrong_kind.validate().is_err());

        let mut other_grant = valid.clone();
        other_grant.grant_id = Some(6);
        assert!(other_grant.validate().is_err());

        let mut custom_as_level = valid.clone();
        custom_as_level.scopes = vec![AccessScope::Observe];
        assert!(custom_as_level.validate().is_err());
        custom_as_level.level = None;
        assert_eq!(custom_as_level.validate(), Ok(()));

        let mut policy_revocable = inventory_entry(
            AccessEntryKey::Policy {
                rule_id: "r".to_owned(),
            },
            AccessEntryKind::PersistentPolicy,
        );
        policy_revocable.revocable = true;
        assert!(policy_revocable.validate().is_err());

        let mut pending_revocable = inventory_entry(
            AccessEntryKey::Pending { request_id: 1 },
            AccessEntryKind::Pending,
        );
        pending_revocable.revocable = true;
        assert!(pending_revocable.validate().is_err());

        let mut removal_pending = inventory_entry(remembered, AccessEntryKind::Remembered);
        removal_pending.state = AccessEntryState::RemovalPending;
        assert!(removal_pending.validate().is_err(), "no longer revocable");
        removal_pending.revocable = false;
        assert_eq!(removal_pending.validate(), Ok(()));

        let mut splint_session = inventory_entry(
            AccessEntryKey::Grant { grant_id: 5 },
            AccessEntryKind::WorkspaceSession,
        );
        splint_session.splint_id = Some(SplintId::new());
        splint_session.incarnation = Some(1);
        assert!(splint_session.validate().is_err());

        let mut long_path = valid.clone();
        long_path.identity.executable_path = "/".repeat(MAX_ACCESS_PATH_BYTES + 1);
        assert!(long_path.validate().is_err());

        let mut long_name = valid;
        long_name.identity.display_name = Some("n".repeat(MAX_ACCESS_DISPLAY_NAME_BYTES + 1));
        assert!(long_name.validate().is_err());
    }

    #[test]
    fn levels_are_exact_bundles_and_never_round_up() {
        for level in AccessLevel::ALL {
            assert_eq!(AccessLevel::exactly(level.scopes()), Some(level));
        }
        assert!(
            AccessLevel::Look
                .scopes()
                .iter()
                .all(|scope| AccessLevel::LookAndType.scopes().contains(scope))
        );
        assert!(
            AccessLevel::LookAndType
                .scopes()
                .iter()
                .all(|scope| AccessLevel::Manage.scopes().contains(scope))
        );
        // One scope short of Look is Custom, never Look; one more is Custom
        // too, never Look and type.
        assert_eq!(AccessLevel::exactly(&AccessLevel::Look.scopes()[..3]), None);
        let mut extra = AccessLevel::Look.scopes().to_vec();
        extra.push(AccessScope::Input);
        assert_eq!(AccessLevel::exactly(&extra), None);
        assert_eq!(
            serde_json::to_value(AccessLevel::LookAndType).unwrap(),
            "look_and_type"
        );
        assert!(serde_json::from_value::<AccessLevel>(serde_json::json!("admin")).is_err());
    }

    fn consent_prompt() -> ConsentPrompt {
        let offer = |duration, shared_by: Option<&str>| ConsentDurationOffer {
            duration,
            withheld: None,
            shared_by: shared_by.map(str::to_owned),
            replaces: None,
        };
        let mut remembered = offer(GrantDuration::Remembered, Some("/usr/bin/splinterm-mcp"));
        remembered.replaces = Some(ConsentReplacement {
            level: Some(AccessLevel::Look),
            scopes: AccessLevel::Look.scopes().to_vec(),
        });
        ConsentPrompt {
            capability: vec![7; CONSENT_CAPABILITY_BYTES],
            requester: "/usr/bin/splinterm-mcp".to_owned(),
            requester_pid: 10,
            requester_uid: 1000,
            agent_name: Some("Claude Code".to_owned()),
            target: ConsentTarget::Lair {
                lair_id: LairId::new(),
                lair_name: "Workspace".to_owned(),
            },
            scopes: AccessLevel::Manage.scopes().to_vec(),
            level: Some(AccessLevel::Manage),
            durations: vec![
                offer(GrantDuration::Once, None),
                offer(
                    GrantDuration::WorkspaceSession,
                    Some("/usr/bin/splinterm-mcp"),
                ),
                remembered,
            ],
            respond_within_seconds: 0,
            origin: ConsentOrigin::Local,
        }
    }

    #[test]
    fn consent_prompts_carry_typed_levels_durations_and_bounds() {
        let prompt = consent_prompt();
        assert_eq!(prompt.validate(), Ok(()));
        let encoded = serde_json::to_value(&prompt).unwrap();
        assert_eq!(encoded["level"], "manage");
        assert_eq!(encoded["durations"][0]["duration"], "once");
        assert!(encoded["durations"][0].get("withheld").is_none());
        assert_eq!(encoded["durations"][2]["duration"], "remembered");
        assert_eq!(
            serde_json::from_value::<ConsentPrompt>(encoded.clone()).unwrap(),
            prompt
        );
        assert!(prompt.offers(GrantDuration::Remembered));

        // Unknown durations and withheld reasons never decode, and neither
        // do more than three durations.
        let mut unknown = encoded.clone();
        unknown["durations"][1]["duration"] = "forever".into();
        assert!(serde_json::from_value::<ConsentPrompt>(unknown).is_err());
        let mut unknown = encoded.clone();
        unknown["durations"][1]["withheld"] = "because".into();
        assert!(serde_json::from_value::<ConsentPrompt>(unknown).is_err());
        let mut oversized = encoded;
        oversized["durations"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"duration": "once"}));
        assert!(serde_json::from_value::<ConsentPrompt>(oversized).is_err());

        let invalid = |change: fn(&mut ConsentPrompt)| {
            let mut prompt = consent_prompt();
            change(&mut prompt);
            prompt.validate()
        };
        assert!(invalid(|prompt| prompt.capability.truncate(1)).is_err());
        assert!(invalid(|prompt| prompt.level = Some(AccessLevel::Look)).is_err());
        assert!(invalid(|prompt| prompt.durations.truncate(0)).is_err());
        assert!(
            invalid(|prompt| {
                prompt.durations.remove(0);
            })
            .is_err()
        );
        assert!(
            invalid(|prompt| prompt.durations[0].withheld = Some(DurationWithheld::Relay)).is_err()
        );
        assert!(invalid(|prompt| prompt.durations.swap(1, 2)).is_err());
        assert!(
            invalid(|prompt| prompt.durations[2].withheld = Some(DurationWithheld::Interpreter))
                .is_err()
        );
        assert!(
            invalid(|prompt| prompt.agent_name = Some("a".repeat(MAX_AGENT_NAME_CHARS + 1)))
                .is_err()
        );
        assert!(
            invalid(|prompt| {
                prompt.durations[1].shared_by = Some("p".repeat(MAX_CONSENT_PATH_CHARS + 1));
            })
            .is_err()
        );
        // A withheld duration shows only its reason.
        let mut withheld = consent_prompt();
        withheld.durations[2] = ConsentDurationOffer {
            duration: GrantDuration::Remembered,
            withheld: Some(DurationWithheld::Interpreter),
            shared_by: None,
            replaces: None,
        };
        assert_eq!(withheld.validate(), Ok(()));
        assert!(!withheld.offers(GrantDuration::Remembered));
        assert_eq!(
            shared_identity_line("/usr/bin/splinterm-mcp"),
            "Any program that runs /usr/bin/splinterm-mcp shares this access."
        );
    }

    #[test]
    fn consent_replies_are_typed_decisions_and_unknown_ones_fail() {
        for (decision, encoded) in [
            (
                ConsentDecision::Deny,
                serde_json::json!({"decision": "deny"}),
            ),
            (
                ConsentDecision::Grant {
                    duration: GrantDuration::WorkspaceSession,
                },
                serde_json::json!({"decision": "grant", "duration": "workspace_session"}),
            ),
        ] {
            assert_eq!(serde_json::to_value(decision).unwrap(), encoded);
            assert_eq!(
                serde_json::from_value::<ConsentDecision>(encoded).unwrap(),
                decision
            );
        }
        for invalid in [
            serde_json::json!({"decision": "maybe"}),
            serde_json::json!({"decision": "grant"}),
            serde_json::json!({"decision": "grant", "duration": "forever"}),
        ] {
            assert!(serde_json::from_value::<ConsentDecision>(invalid).is_err());
        }
        // The protocol 42 boolean reply no longer decodes.
        assert!(
            serde_json::from_value::<ConsentReply>(
                serde_json::json!({"capability": [1], "granted": true})
            )
            .is_err()
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one protocol fixture covers request, response, mapping, and launch bounds"
    )]
    fn atomic_preset_materialization_is_typed_and_bounded() {
        let lair_id = LairId::new();
        let dojo_id = DojoId::new();
        let splint_id = SplintId::new();
        let dojo = PresetDojoLaunch {
            name: "review".into(),
            focus_key: "editor".into(),
            root: PresetLayoutLaunch::Split {
                axis: Axis::Horizontal,
                ratio: SplitRatio::new(650).unwrap(),
                first: Box::new(PresetLayoutLaunch::Pane {
                    key: "editor".into(),
                    title: "editor".into(),
                    launch: LaunchParameters {
                        cwd: PathBuf::from("/tmp"),
                        command: vec!["printf".into(), "$HOME;literal".into()],
                        shell: None,
                        login_shell: false,
                        scrollback_lines: 1000,
                    },
                }),
                second: Box::new(PresetLayoutLaunch::Pane {
                    key: "shell".into(),
                    title: "shell".into(),
                    launch: LaunchParameters {
                        cwd: PathBuf::from("/tmp"),
                        command: Vec::new(),
                        shell: Some("/bin/sh".into()),
                        login_shell: true,
                        scrollback_lines: 1000,
                    },
                }),
            },
        };
        let target = PresetTarget::ExistingLair {
            lair_id,
            rename: None,
        };
        assert!(validate_preset_materialization(&target, std::slice::from_ref(&dojo), &[]).is_ok());
        let directory = PresetDirectoryIdentity {
            path: PathBuf::from("/tmp"),
            device: 1,
            inode: 2,
        };
        assert!(
            validate_preset_materialization(
                &target,
                std::slice::from_ref(&dojo),
                std::slice::from_ref(&directory),
            )
            .is_ok()
        );
        assert!(
            validate_preset_materialization(
                &target,
                std::slice::from_ref(&dojo),
                &[directory.clone(), directory.clone()],
            )
            .is_err()
        );
        let request = Request::MaterializePreset {
            expected_topology_revision: TopologyRevision::new(7),
            target,
            dojos: vec![dojo.clone()],
            directory_identities: Vec::new(),
        };
        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(encoded["type"], "materialize_preset");
        assert_eq!(serde_json::from_value::<Request>(encoded).unwrap(), request);

        let response = Response::PresetMaterialized {
            lair_id,
            dojo_ids: vec![dojo_id],
            panes: vec![PresetPaneIdentity {
                dojo_id,
                key: "editor".into(),
                splint_id,
            }],
            topology_revision: TopologyRevision::new(8),
        };
        assert_eq!(
            serde_json::from_value::<Response>(serde_json::to_value(&response).unwrap()).unwrap(),
            response
        );
        assert!(
            validate_preset_materialized(
                &[dojo_id],
                &[PresetPaneIdentity {
                    dojo_id,
                    key: "editor".into(),
                    splint_id,
                }]
            )
            .is_ok()
        );
        assert!(validate_preset_materialized(&[dojo_id, dojo_id], &[]).is_err());

        let mut invalid = dojo;
        invalid.focus_key = "missing".into();
        assert!(
            validate_preset_materialization(
                &PresetTarget::NewLair { name: "new".into() },
                &[invalid],
                &[]
            )
            .is_err()
        );
        assert!(
            validate_preset_materialization(
                &PresetTarget::NewLair { name: "new".into() },
                &[],
                &[]
            )
            .is_err()
        );
        assert!(
            LaunchParameters {
                cwd: PathBuf::from("/tmp/\0invalid"),
                command: Vec::new(),
                shell: Some("/bin/sh".into()),
                login_shell: true,
                scrollback_lines: 1_000,
            }
            .validate()
            .is_err()
        );
        assert!(
            LaunchParameters {
                cwd: PathBuf::from("/tmp"),
                command: Vec::new(),
                shell: Some("/bin/\0sh".into()),
                login_shell: true,
                scrollback_lines: 1_000,
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn default_terminal_cells_use_compact_protocol_v35_json() {
        let empty = TerminalCell {
            content: String::new(),
            spacer_remaining: None,
            attributes: CellAttributes::default(),
        };
        let encoded = serde_json::to_value(&empty).unwrap();
        assert_eq!(encoded, serde_json::json!({}));
        assert_eq!(
            serde_json::from_value::<TerminalCell>(encoded).unwrap(),
            empty
        );

        let space = TerminalCell {
            content: " ".into(),
            ..empty.clone()
        };
        let encoded = serde_json::to_value(&space).unwrap();
        assert_eq!(encoded, serde_json::json!({"content": " "}));
        assert_eq!(
            serde_json::from_value::<TerminalCell>(encoded).unwrap(),
            space
        );

        let mut bold = space;
        bold.attributes.bold = true;
        assert_eq!(
            serde_json::to_value(&bold).unwrap(),
            serde_json::json!({"content": " ", "attributes": [1, 0, 0, 0, 0, 0, 0, 0]})
        );
        assert_eq!(
            serde_json::from_value::<TerminalCell>(serde_json::json!({
                "content": " ",
                "attributes": [1, 0, 0, 0, 0, 0, 0, 0]
            }))
            .unwrap(),
            bold
        );
        assert!(
            serde_json::from_value::<TerminalCell>(serde_json::json!({
                "attributes": [128, 0, 0, 0, 0, 0, 0, 0]
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<TerminalCell>(serde_json::json!({
                "attributes": [0, 6, 0, 0, 0, 0, 0, 0]
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<TerminalCell>(serde_json::json!({
                "attributes": [0, 0, 4, 0, 0, 0, 0, 0]
            }))
            .is_err()
        );

        let styled = TerminalCell {
            attributes: CellAttributes {
                bold: true,
                dim: true,
                italic: true,
                underline: UnderlineStyle::Curly,
                underline_color_source: ColorSource::Rgb,
                underline_color: 0x00ff_ffff,
                strikethrough: true,
                blink: true,
                conceal: true,
                reverse: true,
                foreground_source: ColorSource::Rgb,
                foreground: 0x00ff_ffff,
                background_source: ColorSource::Rgb,
                background: 0x00ff_ffff,
            },
            ..TerminalCell {
                content: "x".into(),
                spacer_remaining: None,
                attributes: CellAttributes::default(),
            }
        };
        let encoded = serde_json::to_value(&styled).unwrap();
        assert_eq!(
            encoded,
            serde_json::json!({
                "content": "x",
                "attributes": [127, 3, 3, 0x00ff_ffff, 3, 0x00ff_ffff, 3, 0x00ff_ffff]
            })
        );
        assert_eq!(
            serde_json::from_value::<TerminalCell>(encoded).unwrap(),
            styled
        );
    }

    #[test]
    fn first_terminal_read_requests_are_explicit_protocol_v20_shapes() {
        assert_eq!(PROTOCOL_VERSION, 45);
        let splint_id = SplintId::new();
        let attach = Request::Attach {
            splint_id,
            incarnation: None,
            scrollback_rows: 0,
        };
        let scrollback = Request::StartScrollbackPage {
            splint_id,
            incarnation: None,
            max_rows: 16,
        };
        let search = Request::StartSearchScrollback {
            splint_id,
            incarnation: None,
            query: "needle".to_owned(),
            case_sensitive: false,
            max_results: 8,
        };
        let attach_json = serde_json::to_string(&attach).unwrap();
        let scrollback_json = serde_json::to_string(&scrollback).unwrap();
        let search_json = serde_json::to_string(&search).unwrap();
        assert!(attach_json.contains("\"type\":\"attach\""));
        assert!(attach_json.contains("\"incarnation\":null"));
        assert!(scrollback_json.contains("\"type\":\"start_scrollback_page\""));
        assert!(search_json.contains("\"type\":\"start_search_scrollback\""));
        assert_eq!(
            serde_json::from_str::<Request>(&attach_json).unwrap(),
            attach
        );
        assert_eq!(
            serde_json::from_str::<Request>(&scrollback_json).unwrap(),
            scrollback
        );
        assert_eq!(
            serde_json::from_str::<Request>(&search_json).unwrap(),
            search
        );
    }

    #[test]
    fn graphical_focus_messages_are_explicit_protocol_v26_shapes() {
        let splint_id = SplintId::new();
        let read = Request::ReadGraphicalFocus;
        let publish = Request::PublishGraphicalFocus {
            focused_splint_id: Some(splint_id),
        };
        let response = Response::GraphicalFocus {
            focused_splint_id: Some(splint_id),
            cwd: Some(PathBuf::from("/tmp/project")),
        };

        assert_eq!(
            serde_json::to_value(&read).unwrap(),
            serde_json::json!({"type": "read_graphical_focus"})
        );
        assert_eq!(
            serde_json::from_value::<Request>(serde_json::to_value(&publish).unwrap()).unwrap(),
            publish
        );
        assert_eq!(
            serde_json::from_value::<Response>(serde_json::to_value(&response).unwrap()).unwrap(),
            response
        );
    }

    #[test]
    fn mutation_preflight_and_automation_launch_are_explicit_protocol_v21_shapes() {
        let splint_id = SplintId::new();
        let request = Request::PrepareMutation {
            mutation: MutationPreflight::SplitSplint { splint_id },
        };
        let encoded = serde_json::to_string(&request).unwrap();
        assert!(encoded.contains("\"type\":\"prepare_mutation\""));
        assert!(encoded.contains("\"operation\":\"split_splint\""));
        assert_eq!(serde_json::from_str::<Request>(&encoded).unwrap(), request);

        let launch = AutomationLaunch {
            cwd: None,
            argv: Vec::new(),
        };
        launch.validate().unwrap();
        let explicit = AutomationLaunch {
            cwd: Some(PathBuf::from("/tmp")),
            argv: vec!["sh".to_owned(), String::new()],
        };
        explicit.validate().unwrap();
        assert!(
            AutomationLaunch {
                cwd: Some(PathBuf::from("relative")),
                argv: Vec::new(),
            }
            .validate()
            .is_err()
        );
        assert!(
            AutomationLaunch {
                cwd: Some(PathBuf::from("/tmp/has\0nul")),
                argv: Vec::new(),
            }
            .validate()
            .is_err()
        );
        assert!(
            AutomationLaunch {
                cwd: None,
                argv: vec!["sh".to_owned(), "has\0nul".to_owned()],
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn server_limits_are_bounded() {
        let limits = ServerLimits::default();
        assert!(limits.maximum_input_bytes < limits.maximum_frame_bytes);
        assert!(limits.maximum_outstanding_requests > 0);
        assert!(limits.maximum_subscriptions > 0);
        assert_eq!(MAX_GRID_CELLS, 61_440);
        limits.validate_terminal_transport().unwrap();

        let valid_smaller_grid = ServerLimits {
            maximum_columns: 240,
            maximum_rows: 80,
            ..limits
        };
        valid_smaller_grid.validate_terminal_transport().unwrap();

        for invalid in [
            ServerLimits {
                maximum_frame_bytes: 0,
                ..limits
            },
            ServerLimits {
                maximum_frame_bytes: MAX_FRAME_BYTES - 1,
                ..limits
            },
            ServerLimits {
                maximum_frame_bytes: MAX_FRAME_BYTES + 1,
                ..limits
            },
            ServerLimits {
                maximum_input_bytes: 0,
                ..limits
            },
            ServerLimits {
                maximum_input_bytes: MAX_INPUT_BYTES + 1,
                ..limits
            },
            ServerLimits {
                maximum_columns: 1,
                ..limits
            },
            ServerLimits {
                maximum_columns: MAX_COLUMNS + 1,
                ..limits
            },
            ServerLimits {
                maximum_rows: 1,
                ..limits
            },
            ServerLimits {
                maximum_rows: MAX_ROWS + 1,
                ..limits
            },
        ] {
            assert!(invalid.validate_terminal_transport().is_err());
        }
    }

    #[test]
    fn atomic_lair_termination_has_explicit_revision_and_exact_targets() {
        let lair_id = LairId::new();
        let target = MutationTarget {
            lair_id,
            dojo_id: DojoId::new(),
            splint_id: SplintId::new(),
            incarnation: 7,
        };
        let request = Request::TerminateLair {
            expected_topology_revision: TopologyRevision::new(0),
            lair_id,
            targets: vec![target.clone()],
        };
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["type"], "terminate_lair");
        assert_eq!(value["lair_id"], lair_id.to_string());
        assert_eq!(value["targets"][0]["incarnation"], 7);
        assert_eq!(serde_json::from_value::<Request>(value).unwrap(), request);
        assert_eq!(
            MutationPreflight::TerminateLair { lair_id },
            serde_json::from_value(
                serde_json::to_value(MutationPreflight::TerminateLair { lair_id }).unwrap()
            )
            .unwrap()
        );
    }

    #[test]
    fn launch_parameters_and_lifecycle_requests_are_bounded() {
        let launch = LaunchParameters {
            cwd: PathBuf::from("/tmp"),
            command: vec!["printf".into(), "%s".into()],
            shell: None,
            login_shell: false,
            scrollback_lines: 1_000,
        };
        assert!(launch.validate().is_ok());
        let request = Request::SplitSplint {
            expected_topology_revision: TopologyRevision::default(),
            target_splint_id: SplintId::new(),
            axis: Axis::Horizontal,
            side: SplitSide::Second,
            ratio: SplitRatio::new(500).unwrap(),
            launch: launch.clone(),
        };
        assert!(
            serde_json::to_string(&request)
                .unwrap()
                .contains("split_splint")
        );
        let transient = Request::CreateTransientLair {
            expected_topology_revision: TopologyRevision::new(7),
            name: "xdg".into(),
            launch: launch.clone(),
            generated_name: false,
        };
        let encoded = serde_json::to_vec(&transient).unwrap();
        assert_eq!(
            serde_json::from_slice::<Request>(&encoded).unwrap(),
            transient
        );
        let value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(value["type"], "create_transient_lair");
        assert_eq!(value["launch"]["cwd"], "/tmp");
        assert_eq!(
            value["launch"]["command"],
            serde_json::json!(["printf", "%s"])
        );

        let promotion = Request::NewDojo {
            expected_topology_revision: TopologyRevision::new(8),
            lair_id: LairId::new(),
            name: "work".into(),
            launch: launch.clone(),
            promote_transient_lair: true,
            generated_name: false,
        };
        let mut promotion_value = serde_json::to_value(&promotion).unwrap();
        assert_eq!(promotion_value["promote_transient_lair"], true);
        assert_eq!(
            serde_json::from_value::<Request>(promotion_value.clone()).unwrap(),
            promotion
        );
        promotion_value
            .as_object_mut()
            .unwrap()
            .remove("promote_transient_lair");
        assert!(matches!(
            serde_json::from_value::<Request>(promotion_value).unwrap(),
            Request::NewDojo {
                promote_transient_lair: false,
                ..
            }
        ));
        let ordinary_rename = serde_json::to_value(Request::RenameDojo {
            expected_topology_revision: TopologyRevision::new(9),
            dojo_id: DojoId::new(),
            name: "ordinary".into(),
            promote_transient_lair: false,
        })
        .unwrap();
        assert!(ordinary_rename.get("promote_transient_lair").is_none());

        let mut invalid = launch.clone();
        invalid.command = vec!["x".into(); MAX_LAUNCH_ARGUMENTS + 1];
        assert!(invalid.validate().is_err());
        let mut invalid = launch.clone();
        invalid.command[0] = String::new();
        assert!(invalid.validate().is_err());
        let mut invalid = launch.clone();
        invalid.scrollback_lines = MAX_SCROLLBACK_LINES + 1;
        assert!(invalid.validate().is_err());
        let mut invalid = launch.clone();
        invalid.cwd = PathBuf::from("/tmp/has\0nul");
        assert!(invalid.validate().is_err());
        let mut invalid = launch;
        invalid.command.push("has\0nul".to_owned());
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn control_transfer_contract_is_bounded_and_subscriber_specific() {
        let splint_id = SplintId::new();
        let status = ControlStatus {
            splint_id,
            incarnation: 7,
            controlled: true,
            locally_owned: false,
        };
        assert!(status.validate().is_ok());
        assert!(
            ControlStatus {
                locally_owned: true,
                controlled: false,
                ..status
            }
            .validate()
            .is_err()
        );

        assert!(validate_control_modes(&[ControlMode::Input]).is_ok());
        assert!(validate_control_modes(&[ControlMode::Input, ControlMode::Resize]).is_ok());
        assert!(validate_control_modes(&[]).is_err());
        assert!(validate_control_modes(&[ControlMode::Input, ControlMode::Input]).is_err());
        let request = Request::RequestControlTransfer {
            splint_id,
            incarnation: 7,
            modes: vec![ControlMode::Input],
        };
        let encoded = serde_json::to_string(&request).unwrap();
        assert!(encoded.contains("request_control_transfer"));
        let event = SubscriptionEvent::ControlTransferResolved {
            transfer_id: 9,
            outcome: ControlTransferOutcome::Granted,
            controller_id: Some(11),
        };
        let encoded = serde_json::to_string(&event).unwrap();
        assert!(encoded.contains("control_transfer_resolved"));
        assert!(encoded.contains("controller_id"));
    }

    #[test]
    fn targeted_inspection_and_topology_runtime_correlation_are_explicit() {
        let splint_id = SplintId::new();
        let request = Request::InspectSplint { splint_id };
        let encoded = serde_json::to_string(&request).unwrap();
        assert!(encoded.contains("inspect_splint"));
        assert!(encoded.contains("splint_id"));

        let mut topology = Topology::new();
        let lair = topology
            .create_lair("main", PathBuf::from("/tmp"))
            .unwrap()
            .clone();
        let splint_id = match &lair.dojos[0].root {
            splinterm_core::LayoutNode::Leaf(splint) => splint.id,
            splinterm_core::LayoutNode::Branch { .. } => unreachable!(),
        };
        assert!(topology.set_splint_state(splint_id, splinterm_core::SplintState::Exited(0)));
        assert!(topology.set_splint_last_incarnation(splint_id, 1));
        let runtime = SplintRuntimeSummary {
            splint_id,
            live_incarnation: None,
            last_incarnation: Some(1),
            restorable: true,
            lifecycle: SplintLifecycle::Exited,
            exit_status: Some(ProcessExitStatus {
                code: Some(0),
                signal: None,
            }),
            agent_status: None,
        };
        let snapshot = TopologySnapshot {
            revision: topology.revision(),
            topology,
            runtimes: vec![runtime],
        };
        assert!(snapshot.validate().is_ok());

        let mut invalid = snapshot.clone();
        invalid.runtimes.push(invalid.runtimes[0].clone());
        assert!(invalid.validate().is_err());
        let mut invalid = snapshot;
        invalid.runtimes[0].lifecycle = SplintLifecycle::Running;
        assert!(invalid.validate().is_err());
    }

    fn update() -> TerminalUpdate {
        TerminalUpdate {
            base_revision: 4,
            revision: 5,
            rows: vec![TerminalRowPatch {
                index: 1,
                row: TerminalRow {
                    row_id: Some(6),
                    linebreak: false,
                    cells: Vec::new(),
                },
            }],
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
        }
    }

    fn snapshot() -> TerminalSnapshot {
        TerminalSnapshot {
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
            visible_rows: vec![TerminalRow {
                row_id: Some(6),
                linebreak: true,
                cells: Vec::new(),
            }],
            history_generation: 1,
            oldest_available_scrollback_row_id: Some(4),
            newest_available_scrollback_row_id: Some(5),
            scrollback_rows: vec![
                TerminalRow {
                    row_id: Some(4),
                    linebreak: true,
                    cells: Vec::new(),
                },
                TerminalRow {
                    row_id: Some(5),
                    linebreak: true,
                    cells: Vec::new(),
                },
            ],
            available_scrollback_rows: 2,
            omitted_oldest_scrollback_rows: 0,
            images: None,
            exited_code: None,
            exited_signal: None,
        }
    }

    #[test]
    fn terminal_snapshot_validation_enforces_row_identity_scope_and_bounds() {
        assert!(snapshot().validate().is_ok());
        let mut invalid = snapshot();
        invalid.visible_rows[0].row_id = Some(4);
        assert!(invalid.validate().is_err());
        let mut invalid = snapshot();
        invalid.visible_rows[0].row_id = None;
        assert!(invalid.validate().is_err());
        let mut invalid = snapshot();
        invalid.scrollback_rows[1].row_id = Some(4);
        assert!(invalid.validate().is_err());
        let mut invalid = snapshot();
        invalid.scrollback_rows[0].row_id = Some(0);
        assert!(invalid.validate().is_err());
        let mut invalid = snapshot();
        invalid.scrollback_rows.swap(0, 1);
        assert!(invalid.validate().is_err());
        let mut invalid = snapshot();
        invalid.history_generation = 0;
        assert!(invalid.validate().is_err());
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one compact identity matrix checks every image transfer binding field"
    )]
    fn image_wire_contract_binds_identity_and_bounds_metadata() {
        let content = ImageContentMetadata {
            content_id: 7,
            generation: 9,
            width: 2,
            height: 1,
            source_format: ImageSourceFormat::Sixel,
            alpha_mode: ImageAlphaMode::Premultiplied,
            digest: [3; 32],
            byte_length: 8,
            retention: ImageRetention::WhilePlaced,
        };
        let plane = TerminalImagePlane {
            screen: ActiveScreen::Normal,
            contents: vec![content.clone()],
            placements: vec![ImagePlacement {
                placement_id: 11,
                content_id: 7,
                row_id: 5,
                column: 1,
                source: ImagePixelRect {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 1,
                },
                destination_columns: 1,
                destination_rows: 1,
                source_cell_size: Some(ImagePixelSize {
                    width: 8,
                    height: 16,
                }),
                x_offset: 0,
                y_offset: 0,
                z_index: -1,
                application_image_id: None,
                application_placement_id: None,
                creation_order: 13,
                erase_policy: ImageErasePolicy::TextOverwrite,
            }],
        };
        assert!(plane.validate().is_ok());

        let mut invalid = plane.clone();
        invalid.placements[0].content_id = 8;
        assert_eq!(
            invalid.validate().unwrap_err().code,
            ErrorCode::InvalidArgument
        );
        let mut invalid = plane.clone();
        invalid.contents[0].byte_length = MAX_IMAGE_CONTENT_BYTES + 1;
        assert!(invalid.validate().is_err());

        let request = ImageContentRequest {
            splint_id: SplintId::new(),
            incarnation: 2,
            content_id: 7,
            generation: 9,
            digest: [3; 32],
            accepted_transfers: vec![
                ImageTransferMode::SealedMemfd,
                ImageTransferMode::BinaryChunks,
            ],
        };
        assert!(request.validate().is_ok());
        let transfer = ImageContentTransfer {
            splint_id: request.splint_id,
            incarnation: request.incarnation,
            content_id: request.content_id,
            generation: request.generation,
            digest: request.digest,
            byte_length: 8,
            transfer: ImageTransferMode::BinaryChunks,
            token: [4; IMAGE_TRANSFER_TOKEN_BYTES],
            token_ttl_millis: IMAGE_TRANSFER_TOKEN_TTL_MILLIS,
        };
        assert!(transfer.validate().is_ok());
        assert!(transfer.validate_for(&request, &content).is_ok());

        let mut invalid = transfer.clone();
        invalid.splint_id = SplintId::new();
        assert!(invalid.validate_for(&request, &content).is_err());
        let mut invalid = transfer.clone();
        invalid.incarnation += 1;
        assert!(invalid.validate_for(&request, &content).is_err());
        let mut invalid = transfer.clone();
        invalid.content_id += 1;
        assert!(invalid.validate_for(&request, &content).is_err());
        let mut invalid = transfer.clone();
        invalid.generation += 1;
        assert!(invalid.validate_for(&request, &content).is_err());
        let mut invalid = transfer.clone();
        invalid.digest = [5; 32];
        assert!(invalid.validate_for(&request, &content).is_err());
        let mut invalid = transfer.clone();
        invalid.byte_length += 4;
        assert!(invalid.validate_for(&request, &content).is_err());
        let mut invalid = transfer.clone();
        invalid.transfer = ImageTransferMode::SealedMemfd;
        assert!(invalid.validate_for(&request, &content).is_ok());
        let mut binary_only = request.clone();
        binary_only.accepted_transfers = vec![ImageTransferMode::BinaryChunks];
        assert!(invalid.validate_for(&binary_only, &content).is_err());
        invalid = transfer;
        invalid.token = [0; IMAGE_TRANSFER_TOKEN_BYTES];
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn search_page_validation_enforces_identity_ranges_and_bounds() {
        let page = SearchPage {
            splint_id: SplintId::new(),
            incarnation: 2,
            terminal_revision: 3,
            history_generation: 4,
            matches: vec![SearchMatch {
                row_id: 5,
                start_column: 1,
                end_column: 3,
                preview: "hit".into(),
            }],
            next_cursor: Some("0000000000000010".into()),
            timed_out: false,
        };
        assert!(page.validate().is_ok());
        let mut invalid = page.clone();
        invalid.matches[0].end_column = 1;
        assert!(invalid.validate().is_err());
        let mut invalid = page;
        invalid.next_cursor = Some("x".repeat(MAX_SEARCH_CURSOR_BYTES + 1));
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn scrollback_page_validation_enforces_identity_order_and_bounds() {
        let page = ScrollbackPage {
            splint_id: SplintId::new(),
            incarnation: 1,
            terminal_revision: 4,
            history_generation: 2,
            oldest_available_row_id: Some(1),
            newest_available_row_id: Some(9),
            rows: vec![
                TerminalRow {
                    row_id: Some(3),
                    linebreak: false,
                    cells: Vec::new(),
                },
                TerminalRow {
                    row_id: Some(4),
                    linebreak: false,
                    cells: Vec::new(),
                },
            ],
            has_older: true,
        };
        assert!(page.validate().is_ok());

        let mut invalid = page.clone();
        invalid.incarnation = 0;
        assert!(invalid.validate().is_err());
        let mut initial_revision = page.clone();
        initial_revision.terminal_revision = 0;
        assert!(initial_revision.validate().is_ok());
        let mut invalid = page.clone();
        invalid.history_generation = 0;
        assert!(invalid.validate().is_err());
        let mut invalid = page.clone();
        invalid.rows[0].row_id = Some(0);
        assert!(invalid.validate().is_err());
        let mut invalid = page.clone();
        invalid.rows[0].row_id = Some(4);
        assert!(invalid.validate().is_err());
        let mut invalid = page.clone();
        invalid.rows[0].row_id = None;
        assert!(invalid.validate().is_err());
        let mut invalid = page.clone();
        invalid.oldest_available_row_id = Some(10);
        assert!(invalid.validate().is_err());
        let mut invalid = page.clone();
        invalid.newest_available_row_id = Some(2);
        assert!(invalid.validate().is_err());
        let mut invalid = page.clone();
        invalid.rows[1].row_id = Some(10);
        assert!(invalid.validate().is_err());
        let mut invalid = page.clone();
        invalid.rows = vec![invalid.rows[0].clone(); MAX_SCROLLBACK_PAGE_ROWS + 1];
        assert!(invalid.validate().is_err());

        let empty_history = ScrollbackPage {
            oldest_available_row_id: None,
            newest_available_row_id: None,
            rows: Vec::new(),
            has_older: false,
            ..page.clone()
        };
        assert!(empty_history.validate().is_ok());
        let mut invalid = empty_history;
        invalid.has_older = true;
        assert!(invalid.validate().is_err());

        let mut empty_before_oldest = page;
        empty_before_oldest.rows.clear();
        empty_before_oldest.has_older = false;
        assert!(empty_before_oldest.validate().is_ok());
    }

    #[test]
    fn unused_dojo_close_has_explicit_wire_shapes() {
        let dojo_id = DojoId::new();
        let request = Request::CloseUnusedDojo { dojo_id };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::json!({"type": "close_unused_dojo", "dojo_id": dojo_id})
        );
        let used = Response::UnusedDojoClose {
            dojo_id,
            outcome: UnusedDojoCloseOutcome::Used {
                reason: DojoUse::Foreground,
            },
        };
        assert_eq!(
            serde_json::to_value(&used).unwrap(),
            serde_json::json!({
                "type": "unused_dojo_close",
                "dojo_id": dojo_id,
                "outcome": {"outcome": "used", "reason": "foreground"},
            })
        );
        let closed = Response::UnusedDojoClose {
            dojo_id,
            outcome: UnusedDojoCloseOutcome::Closed {
                topology_revision: TopologyRevision::default(),
                lair_removed: false,
            },
        };
        assert_eq!(
            serde_json::to_value(&closed).unwrap()["outcome"],
            serde_json::json!({"outcome": "closed", "topology_revision": 0})
        );
        let lair_removed = Response::UnusedDojoClose {
            dojo_id,
            outcome: UnusedDojoCloseOutcome::Closed {
                topology_revision: TopologyRevision::default(),
                lair_removed: true,
            },
        };
        assert_eq!(
            serde_json::to_value(&lair_removed).unwrap()["outcome"]["lair_removed"],
            serde_json::json!(true)
        );
        for response in [used, closed, lair_removed] {
            assert_eq!(
                serde_json::from_value::<Response>(serde_json::to_value(&response).unwrap())
                    .unwrap(),
                response
            );
        }
        assert_eq!(
            serde_json::from_value::<Request>(serde_json::to_value(&request).unwrap()).unwrap(),
            request
        );
    }

    #[test]
    fn input_marks_focus_reports_only_when_flagged() {
        let splint_id = SplintId::new();
        let input = |focus_report| Request::Input {
            controller_id: 3,
            splint_id,
            incarnation: 4,
            bytes: b"\x1b[I".to_vec(),
            focus_report,
        };
        let ordinary = serde_json::to_value(input(false)).unwrap();
        // Ordinary input keeps the encoding older peers send and accept.
        assert!(ordinary.get("focus_report").is_none());
        assert_eq!(
            serde_json::from_value::<Request>(ordinary).unwrap(),
            input(false)
        );
        let flagged = serde_json::to_value(input(true)).unwrap();
        assert_eq!(flagged["focus_report"], true);
        assert_eq!(
            serde_json::from_value::<Request>(flagged).unwrap(),
            input(true)
        );
    }

    #[test]
    fn terminal_update_validation_bounds_revisions_rows_scrolls_and_palette() {
        assert!(update().validate_against(4, 1, 80, 24).is_ok());

        let mut aggregated = update();
        aggregated.revision = 6;
        assert!(aggregated.validate_against(4, 1, 80, 24).is_ok());

        let mut invalid = update();
        invalid.revision = 4;
        assert!(invalid.validate_against(4, 1, 80, 24).is_err());

        let mut invalid = update();
        invalid.rows[0].index = 24;
        assert!(invalid.validate_against(4, 1, 80, 24).is_err());

        let mut invalid = update();
        invalid.rows[0].row.row_id = None;
        assert!(invalid.validate_against(4, 1, 80, 24).is_err());

        let mut invalid = update();
        invalid.rows.push(invalid.rows[0].clone());
        assert!(invalid.validate_against(4, 1, 80, 24).is_err());

        let mut invalid = update();
        invalid.scrolls.push(TerminalScroll {
            direction: ScrollDirection::Forward,
            start_row: 2,
            end_row: 25,
            rows: 1,
        });
        assert!(invalid.validate_against(4, 1, 80, 24).is_err());

        let mut invalid = update();
        invalid.palette = Some(vec![0; 255]);
        assert!(invalid.validate_against(4, 1, 80, 24).is_err());

        let mut invalid = update();
        invalid.scrollback = Some(TerminalScrollbackUpdate {
            transition: HistoryTransition::Replace,
            history_generation: 1,
            oldest_available_row_id: None,
            newest_available_row_id: None,
            rows: vec![TerminalRow {
                row_id: None,
                linebreak: false,
                cells: Vec::new(),
            }],
            available_rows: 0,
            omitted_oldest_rows: 0,
        });
        assert!(invalid.validate_against(4, 1, 80, 24).is_err());

        let mut valid = update();
        valid.scrollback = Some(TerminalScrollbackUpdate {
            transition: HistoryTransition::Append {
                appended_rows: 1,
                trimmed_rows: 0,
            },
            history_generation: 2,
            oldest_available_row_id: Some(7),
            newest_available_row_id: Some(9),
            rows: vec![
                TerminalRow {
                    row_id: Some(7),
                    linebreak: false,
                    cells: Vec::new(),
                },
                TerminalRow {
                    row_id: Some(9),
                    linebreak: false,
                    cells: Vec::new(),
                },
            ],
            available_rows: 2,
            omitted_oldest_rows: 0,
        });
        assert!(valid.validate_against(4, 2, 80, 24).is_ok());

        let mut invalid_ids = valid.clone();
        invalid_ids.scrollback.as_mut().unwrap().rows[0].row_id = Some(0);
        assert!(invalid_ids.validate_against(4, 2, 80, 24).is_err());
        let mut reversed_ids = valid.clone();
        reversed_ids.scrollback.as_mut().unwrap().rows.swap(0, 1);
        assert!(reversed_ids.validate_against(4, 2, 80, 24).is_err());
        let mut duplicate_ids = valid.clone();
        duplicate_ids.scrollback.as_mut().unwrap().rows[1].row_id = Some(7);
        assert!(duplicate_ids.validate_against(4, 2, 80, 24).is_err());

        let mut stale_reset = valid;
        let scrollback = stale_reset.scrollback.as_mut().unwrap();
        scrollback.transition = HistoryTransition::Reflow;
        scrollback.history_generation = 2;
        assert!(stale_reset.validate_against(4, 2, 80, 24).is_err());
        stale_reset.scrollback.as_mut().unwrap().history_generation = 3;
        assert!(stale_reset.validate_against(4, 2, 80, 24).is_ok());

        let mut stale_clear = update();
        stale_clear.scrollback = Some(TerminalScrollbackUpdate {
            transition: HistoryTransition::Clear,
            history_generation: 2,
            oldest_available_row_id: None,
            newest_available_row_id: None,
            rows: Vec::new(),
            available_rows: 0,
            omitted_oldest_rows: 0,
        });
        assert!(stale_clear.validate_against(4, 2, 80, 24).is_err());
        stale_clear.scrollback.as_mut().unwrap().history_generation = 3;
        assert!(stale_clear.validate_against(4, 2, 80, 24).is_ok());
    }

    #[test]
    fn agent_status_report_wire_shapes_are_closed_and_additive() {
        let splint_id = SplintId::new();
        let request = Request::ReportAgentStatus {
            splint_id,
            incarnation: 3,
            status: Some(AgentStatusReport {
                state: AgentState::NeedsInput,
                detail: Some("waiting for approval".into()),
                ttl_seconds: None,
            }),
        };
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["type"], "report_agent_status");
        assert_eq!(value["status"]["state"], "needs_input");
        assert!(value["status"].get("ttl_seconds").is_none());
        assert_eq!(serde_json::from_value::<Request>(value).unwrap(), request);
        let clear: Request = serde_json::from_value(serde_json::json!({
            "type": "report_agent_status",
            "splint_id": splint_id,
            "incarnation": 3,
            "status": null
        }))
        .unwrap();
        assert!(matches!(
            clear,
            Request::ReportAgentStatus { status: None, .. }
        ));
        assert!(
            serde_json::from_value::<Request>(serde_json::json!({
                "type": "report_agent_status",
                "splint_id": splint_id,
                "incarnation": 3,
                "status": {"state": "thinking"}
            }))
            .is_err(),
            "unknown states are rejected by the closed vocabulary"
        );
        assert!(
            serde_json::from_value::<AgentStatusReport>(serde_json::json!({
                "state": "done",
                "progress": 50
            }))
            .is_err()
        );
        assert_eq!(
            serde_json::to_value(Response::AgentStatusAccepted {
                sequence: 4,
                expires_at_ms: None,
            })
            .unwrap(),
            serde_json::json!({
                "type": "agent_status_accepted",
                "sequence": 4,
                "expires_at_ms": null
            })
        );
        let event = SubscriptionEvent::AgentStatusChanged {
            splint_id,
            incarnation: 3,
            status: None,
            sequence: 5,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "agent_status_changed");
        assert_eq!(
            serde_json::from_value::<SubscriptionEvent>(value).unwrap(),
            event
        );
        assert_eq!(
            serde_json::to_value(AutomationScope::AgentStatusReport).unwrap(),
            "agent_status_report"
        );
        assert_eq!(
            serde_json::to_value(AccessScope::AgentStatusReport).unwrap(),
            "agent_status_report"
        );
        assert_eq!(
            serde_json::to_value(ErrorCode::RateLimited).unwrap(),
            "rate_limited"
        );
        assert_eq!(
            AuditOperation::AgentStatusReport.as_str(),
            serde_json::to_value(AuditOperation::AgentStatusReport).unwrap()
        );
        for state in [
            AgentState::Working,
            AgentState::NeedsInput,
            AgentState::Done,
            AgentState::Error,
            AgentState::Idle,
        ] {
            assert_eq!(serde_json::to_value(state).unwrap(), state.as_str());
        }
    }

    #[test]
    fn runtime_summary_agent_status_is_optional_and_live_only() {
        let splint_id = SplintId::new();
        let mut runtime = SplintRuntimeSummary {
            splint_id,
            live_incarnation: Some(2),
            last_incarnation: Some(2),
            restorable: false,
            lifecycle: SplintLifecycle::Running,
            exit_status: None,
            agent_status: None,
        };
        let value = serde_json::to_value(&runtime).unwrap();
        assert!(value.get("agent_status").is_none());
        assert_eq!(
            serde_json::from_value::<SplintRuntimeSummary>(value).unwrap(),
            runtime
        );
        runtime.agent_status = Some(AgentStatusSummary {
            state: AgentState::Working,
            detail: Some("compiling".into()),
            sequence: 1,
            stale: true,
            reported_age_ms: 121_000,
        });
        assert!(runtime.validate());
        let mut invalid = runtime.clone();
        invalid.agent_status.as_mut().unwrap().sequence = 0;
        assert!(!invalid.validate());
        let mut exited = runtime;
        exited.live_incarnation = None;
        exited.lifecycle = SplintLifecycle::Exited;
        exited.restorable = true;
        assert!(!exited.validate(), "exited incarnations carry no status");
    }

    #[test]
    fn agent_status_detail_and_ttl_are_bounded_and_rejected_not_stripped() {
        let report = |detail: &str, ttl_seconds| AgentStatusReport {
            state: AgentState::Working,
            detail: Some(detail.to_owned()),
            ttl_seconds,
        };
        assert!(report("waiting for approval", None).validate().is_ok());
        assert!(report(&"x".repeat(80), Some(10)).validate().is_ok());
        assert!(report(&"界".repeat(80), Some(900)).validate().is_ok());
        assert!(report(&"x".repeat(81), None).validate().is_err());
        // 61 four-byte scalars stay under 80 scalars but exceed 240 bytes.
        assert!(report(&"🦀".repeat(61), None).validate().is_err());
        assert!(report(&"🦀".repeat(60), None).validate().is_ok());
        assert!(report("", None).validate().is_err());
        for forbidden in [
            "line\nbreak",
            "carriage\rreturn",
            "tab\there",
            "nel\u{85}",
            "line\u{2028}separator",
            "para\u{2029}separator",
            "rlo\u{202e}",
            "isolate\u{2066}",
            "mark\u{200f}",
            "arabic\u{061c}",
            "escape\u{1b}[31m",
        ] {
            let error = report(forbidden, None).validate().unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidArgument, "{forbidden:?}");
        }
        assert!(report("ok", Some(9)).validate().is_err());
        assert!(report("ok", Some(901)).validate().is_err());
        let working = AgentStatusReport {
            state: AgentState::Working,
            detail: None,
            ttl_seconds: None,
        };
        assert_eq!(working.effective_ttl_seconds(), Some(120));
        let done = AgentStatusReport {
            state: AgentState::Done,
            detail: None,
            ttl_seconds: None,
        };
        assert_eq!(done.effective_ttl_seconds(), None);
        let timed_done = AgentStatusReport {
            ttl_seconds: Some(30),
            ..done
        };
        assert_eq!(timed_done.effective_ttl_seconds(), Some(30));
    }

    fn handoff_report() -> HandoffReport {
        HandoffReport {
            daemon_generation: 1,
            build: HandoffBuild {
                version: "0.2.0-dev".into(),
                identity: [9; HANDOFF_BUILD_IDENTITY_BYTES],
                provenance: HandoffBuildProvenance::Unpackaged,
            },
            private_protocol: HandoffVersionRange {
                minimum: PROTOCOL_VERSION,
                maximum: PROTOCOL_VERSION,
            },
            handoff_protocol: HandoffVersionRange {
                minimum: 1,
                maximum: 1,
            },
            terminal_checkpoint: HandoffVersionRange {
                minimum: 2,
                maximum: 2,
            },
            descriptor_manifest: HandoffVersionRange {
                minimum: 1,
                maximum: 1,
            },
            lairs: 3,
            transient_lairs: 1,
            dojos: 4,
            live_splints: 5,
            rollback_available: false,
            blocking_reasons: vec![
                HandoffBlockingReason::TransientLairsOpen {
                    count: 1,
                    lair_ids: vec![LairId::new()],
                },
                HandoffBlockingReason::NotPackaged,
                HandoffBlockingReason::Unsupported,
            ],
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "every handoff request shape and decoding bound is reviewed together"
    )]
    fn daemon_handoff_requests_and_hello_generation_have_explicit_wire_shapes() {
        assert_eq!(
            serde_json::to_value(Request::PrepareDaemonHandoff {
                mode: DaemonHandoffMode::DryRun,
            })
            .unwrap(),
            serde_json::json!({"type": "prepare_daemon_handoff", "mode": "dry_run"})
        );
        let execute: Request = serde_json::from_value(
            serde_json::json!({"type": "prepare_daemon_handoff", "mode": "execute"}),
        )
        .unwrap();
        assert_eq!(
            execute,
            Request::PrepareDaemonHandoff {
                mode: DaemonHandoffMode::Execute,
            }
        );
        assert!(
            serde_json::from_value::<Request>(
                serde_json::json!({"type": "prepare_daemon_handoff", "mode": "force"})
            )
            .is_err()
        );

        let claim = Request::ClaimHandoffControl {
            ticket: HandoffResumeTicket::new([0xa5; HANDOFF_RESUME_TICKET_BYTES]),
            splint: None,
        };
        let value = serde_json::to_value(&claim).unwrap();
        assert_eq!(value["type"], "claim_handoff_control");
        // An absent target is omitted, so a protocol 42 peer decodes it.
        assert!(value.get("splint").is_none(), "{value}");
        let splint_id = SplintId::new();
        let targeted = Request::ClaimHandoffControl {
            ticket: HandoffResumeTicket::new([0xa5; HANDOFF_RESUME_TICKET_BYTES]),
            splint: Some(HandoffClaimTarget {
                splint_id,
                incarnation: 9,
            }),
        };
        let targeted_value = serde_json::to_value(&targeted).unwrap();
        assert_eq!(
            targeted_value["splint"],
            serde_json::json!({"splint_id": splint_id, "incarnation": 9})
        );
        assert_eq!(
            serde_json::from_value::<Request>(targeted_value).unwrap(),
            targeted
        );
        assert_eq!(
            value["ticket"],
            serde_json::json!(vec![0xa5_u8; HANDOFF_RESUME_TICKET_BYTES]),
            "the newtype keeps the plain fixed-size array wire shape"
        );
        // Neither the ticket nor a request holding it prints the bytes.
        for debug in [
            format!(
                "{:?}",
                HandoffResumeTicket::new([0xa5; HANDOFF_RESUME_TICKET_BYTES])
            ),
            format!("{claim:?}"),
            format!("{claim:#?}"),
        ] {
            assert!(debug.contains("32 bytes redacted"), "{debug}");
            assert!(!debug.contains("165"), "{debug}");
            assert!(!debug.to_ascii_lowercase().contains("a5"), "{debug}");
        }
        assert_eq!(serde_json::from_value::<Request>(value).unwrap(), claim);
        for length in [
            0,
            HANDOFF_RESUME_TICKET_BYTES - 1,
            HANDOFF_RESUME_TICKET_BYTES + 1,
            4096,
        ] {
            assert!(
                serde_json::from_value::<Request>(serde_json::json!({
                    "type": "claim_handoff_control",
                    "ticket": vec![1_u8; length],
                }))
                .is_err(),
                "a {length}-byte ticket is rejected while decoding"
            );
        }

        let acknowledge = Request::AcknowledgeHandoffPrepare {
            daemon_generation: 3,
            prepare_id: 17,
        };
        let value = serde_json::to_value(&acknowledge).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "type": "acknowledge_handoff_prepare",
                "daemon_generation": 3,
                "prepare_id": 17
            })
        );
        assert_eq!(
            serde_json::from_value::<Request>(value).unwrap(),
            acknowledge
        );
        // The prepare id is required on the wire.
        assert!(
            serde_json::from_value::<Request>(serde_json::json!({
                "type": "acknowledge_handoff_prepare",
                "daemon_generation": 3
            }))
            .is_err()
        );
        validate_handoff_acknowledgement(3, 17).unwrap();
        for (daemon_generation, prepare_id) in [(0, 17), (3, 0)] {
            assert_eq!(
                validate_handoff_acknowledgement(daemon_generation, prepare_id)
                    .unwrap_err()
                    .code,
                ErrorCode::InvalidArgument
            );
        }

        let hello = ServerFrame::Hello {
            version: PROTOCOL_VERSION,
            limits: ServerLimits::default(),
            development_terminal_access: false,
            daemon_hostname: None,
            daemon_generation: 7,
        };
        let mut value = serde_json::to_value(&hello).unwrap();
        assert_eq!(value["daemon_generation"], 7);
        assert_eq!(
            serde_json::from_value::<ServerFrame>(value.clone()).unwrap(),
            hello
        );
        value.as_object_mut().unwrap().remove("daemon_generation");
        assert!(
            serde_json::from_value::<ServerFrame>(value).is_err(),
            "protocol 42 hello always names the daemon generation"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "every report invariant and decoding bound is reviewed together"
    )]
    fn handoff_report_is_bounded_and_self_consistent() {
        let report = handoff_report();
        report.validate().unwrap();
        let response = Response::HandoffReport {
            report: report.clone(),
        };
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["type"], "handoff_report");
        assert_eq!(value["report"]["build"]["provenance"], "unpackaged");
        assert_eq!(
            value["report"]["blocking_reasons"][0]["reason"],
            "transient_lairs_open"
        );
        assert_eq!(
            value["report"]["blocking_reasons"][1],
            serde_json::json!({"reason": "not_packaged"})
        );
        assert_eq!(
            value["report"]["blocking_reasons"][2],
            serde_json::json!({"reason": "unsupported"})
        );
        assert_eq!(serde_json::from_value::<Response>(value).unwrap(), response);

        let ready = HandoffReport {
            transient_lairs: 0,
            rollback_available: true,
            build: HandoffBuild {
                provenance: HandoffBuildProvenance::ReleaseCandidate,
                ..report.build.clone()
            },
            blocking_reasons: Vec::new(),
            ..report
        };
        ready.validate().unwrap();

        let invalid = |mutate: fn(&mut HandoffReport)| {
            let mut candidate = handoff_report();
            mutate(&mut candidate);
            candidate.validate().unwrap_err().code
        };
        for mutate in [
            |report: &mut HandoffReport| report.daemon_generation = 0,
            |report: &mut HandoffReport| report.build.identity = [0; HANDOFF_BUILD_IDENTITY_BYTES],
            |report: &mut HandoffReport| report.build.version = String::new(),
            |report: &mut HandoffReport| report.build.version = "0.2.0 dev".into(),
            |report: &mut HandoffReport| report.build.version = "0.2.0\u{202e}".into(),
            |report: &mut HandoffReport| {
                report.build.version = "1".repeat(MAX_HANDOFF_BUILD_VERSION_BYTES + 1);
            },
            |report: &mut HandoffReport| {
                report.handoff_protocol = HandoffVersionRange {
                    minimum: 2,
                    maximum: 1,
                };
            },
            |report: &mut HandoffReport| {
                report.private_protocol = HandoffVersionRange {
                    minimum: 0,
                    maximum: 1,
                };
            },
            |report: &mut HandoffReport| report.transient_lairs = 2,
            |report: &mut HandoffReport| report.lairs = 0,
            |report: &mut HandoffReport| {
                report.blocking_reasons.remove(0);
            },
            |report: &mut HandoffReport| report.rollback_available = true,
            // Unpackaged builds always name NotPackaged, never a ready pair or
            // a different rollback reason.
            |report: &mut HandoffReport| {
                report.rollback_available = true;
                report
                    .blocking_reasons
                    .retain(|reason| !matches!(reason, HandoffBlockingReason::NotPackaged));
            },
            |report: &mut HandoffReport| {
                report.blocking_reasons[1] = HandoffBlockingReason::RollbackUnavailable;
            },
            |report: &mut HandoffReport| {
                report
                    .blocking_reasons
                    .push(HandoffBlockingReason::RollbackUnavailable);
            },
            |report: &mut HandoffReport| {
                report
                    .blocking_reasons
                    .push(HandoffBlockingReason::Unsupported);
            },
            |report: &mut HandoffReport| {
                report.blocking_reasons[0] = HandoffBlockingReason::TransientLairsOpen {
                    count: 1,
                    lair_ids: vec![LairId::new(), LairId::new()],
                };
            },
            |report: &mut HandoffReport| {
                let lair_id = LairId::new();
                report.transient_lairs = 2;
                report.blocking_reasons[0] = HandoffBlockingReason::TransientLairsOpen {
                    count: 2,
                    lair_ids: vec![lair_id, lair_id],
                };
            },
        ] {
            assert_eq!(invalid(mutate), ErrorCode::InvalidArgument);
        }

        // Many transient Lairs keep an exact count but name a bounded subset.
        let mut many = handoff_report();
        many.lairs = 40;
        many.transient_lairs = 40;
        many.blocking_reasons[0] = HandoffBlockingReason::TransientLairsOpen {
            count: 40,
            lair_ids: (0..MAX_HANDOFF_REPORTED_LAIRS)
                .map(|_| LairId::new())
                .collect(),
        };
        many.validate().unwrap();

        // Decoding rejects a list one element past its bound. (The whole frame
        // is still parsed first; `MAX_FRAME_BYTES` bounds that.)
        let mut value = serde_json::to_value(&many).unwrap();
        value["blocking_reasons"][0]["lair_ids"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::to_value(LairId::new()).unwrap());
        assert!(serde_json::from_value::<HandoffReport>(value).is_err());
        let mut value = serde_json::to_value(handoff_report()).unwrap();
        let reasons = value["blocking_reasons"].as_array_mut().unwrap();
        reasons.push(serde_json::json!({"reason": "rollback_unavailable"}));
        reasons.push(serde_json::json!({"reason": "unsupported"}));
        assert_eq!(reasons.len(), MAX_HANDOFF_BLOCKING_REASONS + 1);
        assert!(serde_json::from_value::<HandoffReport>(value).is_err());
    }

    #[test]
    fn handoff_errors_and_status_events_have_explicit_wire_shapes() {
        for (code, name) in [
            (ErrorCode::HandoffInProgress, "handoff_in_progress"),
            (ErrorCode::HandoffUnavailable, "handoff_unavailable"),
            (ErrorCode::InvalidHandoffTicket, "invalid_handoff_ticket"),
        ] {
            assert_eq!(serde_json::to_value(code).unwrap(), name);
        }
        let plain = ProtocolError::new(ErrorCode::HandoffInProgress, "daemon handoff in progress");
        assert!(
            serde_json::to_value(&plain)
                .unwrap()
                .get("handoff_blocking_reasons")
                .is_none()
        );
        let refused = ProtocolError::handoff_unavailable(vec![
            HandoffBlockingReason::NotPackaged,
            HandoffBlockingReason::Unsupported,
        ]);
        assert_eq!(refused.code, ErrorCode::HandoffUnavailable);
        let value = serde_json::to_value(ServerFrame::Error {
            request_id: Some(4),
            error: refused.clone(),
        })
        .unwrap();
        assert_eq!(
            value["error"]["handoff_blocking_reasons"],
            serde_json::json!([{"reason": "not_packaged"}, {"reason": "unsupported"}])
        );
        assert!(matches!(
            serde_json::from_value::<ServerFrame>(value).unwrap(),
            ServerFrame::Error { error, .. } if error == refused
        ));
        let oversized = serde_json::json!({
            "code": "handoff_unavailable",
            "message": "daemon handoff is unavailable",
            "handoff_blocking_reasons": vec![serde_json::json!({"reason": "unsupported"}); 5],
        });
        assert!(serde_json::from_value::<ProtocolError>(oversized).is_err());

        for (phase, expected) in [
            (
                HandoffStatusPhase::Preparing,
                serde_json::json!({"state": "preparing"}),
            ),
            (
                HandoffStatusPhase::Fenced,
                serde_json::json!({"state": "fenced"}),
            ),
            (
                HandoffStatusPhase::Cancelled {
                    reason: HandoffCancelReason::TimedOut,
                },
                serde_json::json!({"state": "cancelled", "reason": "timed_out"}),
            ),
        ] {
            let event = SubscriptionEvent::HandoffStatus {
                daemon_generation: 2,
                phase,
            };
            let value = serde_json::to_value(&event).unwrap();
            assert_eq!(
                value,
                serde_json::json!({
                    "type": "handoff_status",
                    "daemon_generation": 2,
                    "phase": expected,
                })
            );
            assert_eq!(
                serde_json::from_value::<SubscriptionEvent>(value).unwrap(),
                event
            );
        }
    }

    #[test]
    fn handoff_claimed_controllers_are_bounded_and_unique() {
        let controller = |controller_id| HandoffClaimedController {
            splint_id: SplintId::new(),
            incarnation: 1,
            controller_id,
            modes: vec![ControlMode::Input, ControlMode::Resize],
        };
        let response = Response::HandoffControlClaimed {
            controllers: vec![controller(1), controller(2)],
        };
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["type"], "handoff_control_claimed");
        assert_eq!(
            value["controllers"][0]["modes"],
            serde_json::json!(["input", "resize"])
        );
        assert_eq!(serde_json::from_value::<Response>(value).unwrap(), response);
        let Response::HandoffControlClaimed { controllers } = response else {
            unreachable!();
        };
        validate_handoff_claimed_controllers(&controllers).unwrap();
        validate_handoff_claimed_controllers(&[]).unwrap();

        let duplicate_splint = HandoffClaimedController {
            controller_id: 3,
            ..controllers[0].clone()
        };
        for invalid in [
            vec![controller(0)],
            vec![HandoffClaimedController {
                incarnation: 0,
                ..controller(1)
            }],
            vec![HandoffClaimedController {
                modes: Vec::new(),
                ..controller(1)
            }],
            vec![controller(1), controller(1)],
            vec![controllers[0].clone(), duplicate_splint],
        ] {
            assert_eq!(
                validate_handoff_claimed_controllers(&invalid)
                    .unwrap_err()
                    .code,
                ErrorCode::InvalidArgument
            );
        }
        let oversized = serde_json::json!({
            "type": "handoff_control_claimed",
            "controllers": (1..=MAX_HANDOFF_CLAIMED_CONTROLLERS as u64 + 1)
                .map(|id| serde_json::to_value(controller(id)).unwrap())
                .collect::<Vec<_>>(),
        });
        assert!(serde_json::from_value::<Response>(oversized).is_err());
    }

    #[test]
    fn hello_window_anchor_is_optional_and_omitted_when_false() {
        let hello = |handoff_window| ClientFrame::Hello {
            minimum_version: PROTOCOL_VERSION,
            maximum_version: PROTOCOL_VERSION,
            role: ClientRole::TrustedUi,
            handoff_window,
        };
        // An undeclared hello keeps its exact protocol 41 shape.
        let plain = serde_json::json!({
            "type": "hello",
            "minimum_version": 45,
            "maximum_version": 45,
            "role": "trusted_ui",
        });
        assert_eq!(serde_json::to_value(hello(false)).unwrap(), plain);
        assert_eq!(
            serde_json::from_value::<ClientFrame>(plain).unwrap(),
            hello(false)
        );
        let declared = serde_json::to_value(hello(true)).unwrap();
        assert_eq!(declared["handoff_window"], true);
        assert_eq!(
            serde_json::from_value::<ClientFrame>(declared).unwrap(),
            hello(true)
        );
    }

    fn unresponsive_window(index: u8) -> HandoffUnresponsiveWindow {
        HandoffUnresponsiveWindow {
            index,
            pid: 4242 + u32::from(index),
            reason: HandoffWindowIssue::NoAcknowledgement,
            dojos: vec![HandoffWindowDojo {
                lair_id: LairId::new(),
                dojo_id: DojoId::new(),
                name: "build".into(),
            }],
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "every decision prompt shape, invariant, and decoding bound is reviewed together"
    )]
    fn handoff_decision_prompt_is_bounded_and_explicit() {
        let first = unresponsive_window(0);
        let undeclared = HandoffUnresponsiveWindow {
            reason: HandoffWindowIssue::Undeclared,
            dojos: Vec::new(),
            ..unresponsive_window(1)
        };
        let prompt = ServerFrame::HandoffDecisionRequired {
            request_id: 9,
            daemon_generation: 3,
            deadline_ms: 20_000,
            windows: vec![first.clone(), undeclared.clone()],
        };
        let value = serde_json::to_value(&prompt).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "type": "handoff_decision_required",
                "request_id": 9,
                "daemon_generation": 3,
                "deadline_ms": 20_000,
                "windows": [
                    {
                        "index": 0,
                        "pid": 4242,
                        "reason": "no_acknowledgement",
                        "dojos": [{
                            "lair_id": first.dojos[0].lair_id,
                            "dojo_id": first.dojos[0].dojo_id,
                            "name": "build",
                        }],
                    },
                    {"index": 1, "pid": 4243, "reason": "undeclared", "dojos": []},
                ],
            })
        );
        assert_eq!(
            serde_json::from_value::<ServerFrame>(value).unwrap(),
            prompt
        );
        for (reason, name) in [
            (HandoffWindowIssue::NoAcknowledgement, "no_acknowledgement"),
            (HandoffWindowIssue::Unpinned, "unpinned"),
            (HandoffWindowIssue::Undeclared, "undeclared"),
        ] {
            assert_eq!(serde_json::to_value(reason).unwrap(), name);
        }
        let windows = vec![first, undeclared];
        validate_handoff_decision_required(9, 3, 20_000, &windows).unwrap();
        validate_handoff_decision_required(9, 3, MAX_HANDOFF_DECISION_DEADLINE_MS, &windows)
            .unwrap();

        // The largest prompt: every Window, each with every Dojo.
        let full = (0..MAX_HANDOFF_WINDOWS)
            .map(|index| {
                let mut window = unresponsive_window(u8::try_from(index).unwrap());
                window.dojos = (0..MAX_HANDOFF_WINDOW_DOJOS)
                    .map(|_| HandoffWindowDojo {
                        lair_id: LairId::new(),
                        dojo_id: DojoId::new(),
                        name: "n".repeat(MAX_HANDOFF_DOJO_NAME_BYTES),
                    })
                    .collect();
                window
            })
            .collect::<Vec<_>>();
        validate_handoff_decision_required(9, 3, 20_000, &full).unwrap();
        let frame = encode_frame(&ServerFrame::HandoffDecisionRequired {
            request_id: 9,
            daemon_generation: 3,
            deadline_ms: 20_000,
            windows: full.clone(),
        })
        .unwrap();
        assert!(frame.len() < MAX_FRAME_BYTES);

        for (request_id, daemon_generation, deadline_ms) in [
            (0, 3, 20_000),
            (9, 0, 20_000),
            (9, 3, 0),
            (9, 3, MAX_HANDOFF_DECISION_DEADLINE_MS + 1),
        ] {
            assert_eq!(
                validate_handoff_decision_required(
                    request_id,
                    daemon_generation,
                    deadline_ms,
                    &windows
                )
                .unwrap_err()
                .code,
                ErrorCode::InvalidArgument
            );
        }
        let invalid = |mutate: fn(&mut Vec<HandoffUnresponsiveWindow>)| {
            let mut candidate = windows.clone();
            mutate(&mut candidate);
            validate_handoff_decision_required(9, 3, 20_000, &candidate)
                .unwrap_err()
                .code
        };
        for mutate in [
            (|windows: &mut Vec<HandoffUnresponsiveWindow>| windows.clear())
                as fn(&mut Vec<HandoffUnresponsiveWindow>),
            |windows| windows[1].index = 0,
            |windows| windows[1].index = u8::try_from(MAX_HANDOFF_WINDOWS).unwrap(),
            |windows| windows[0].pid = 0,
            |windows| windows[0].pid = u32::try_from(i32::MAX).unwrap() + 1,
            |windows| windows[1].pid = windows[0].pid,
            |windows| {
                let dojo = windows[0].dojos[0].clone();
                windows[0].dojos.push(dojo);
            },
            |windows| windows[0].dojos[0].name = String::new(),
            |windows| windows[0].dojos[0].name = " build".into(),
            |windows| windows[0].dojos[0].name = "n".repeat(MAX_HANDOFF_DOJO_NAME_BYTES + 1),
            |windows| {
                let template = windows[0].dojos[0].clone();
                windows[0].dojos = (0..=MAX_HANDOFF_WINDOW_DOJOS)
                    .map(|_| HandoffWindowDojo {
                        dojo_id: DojoId::new(),
                        ..template.clone()
                    })
                    .collect();
            },
        ] {
            assert_eq!(invalid(mutate), ErrorCode::InvalidArgument);
        }
        let mut excess = full.clone();
        excess.push(unresponsive_window(0));
        assert!(validate_handoff_decision_required(9, 3, 20_000, &excess).is_err());

        // Decoding rejects a list one element past either bound.
        let mut value = serde_json::to_value(ServerFrame::HandoffDecisionRequired {
            request_id: 9,
            daemon_generation: 3,
            deadline_ms: 20_000,
            windows: full,
        })
        .unwrap();
        let mut too_many_dojos = value.clone();
        let dojos = too_many_dojos["windows"][0]["dojos"]
            .as_array_mut()
            .unwrap();
        dojos.push(dojos[0].clone());
        assert!(serde_json::from_value::<ServerFrame>(too_many_dojos).is_err());
        let listed = value["windows"].as_array_mut().unwrap();
        listed.push(listed[0].clone());
        assert_eq!(listed.len(), MAX_HANDOFF_WINDOWS + 1);
        assert!(serde_json::from_value::<ServerFrame>(value).is_err());
    }

    #[test]
    fn handoff_decision_answer_is_bounded_and_explicit() {
        let answer = ClientFrame::HandoffDecision {
            request_id: 9,
            proceed_without: vec![1, 0],
        };
        let value = serde_json::to_value(&answer).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "type": "handoff_decision",
                "request_id": 9,
                "proceed_without": [1, 0],
            })
        );
        assert_eq!(
            serde_json::from_value::<ClientFrame>(value).unwrap(),
            answer
        );
        validate_handoff_decision(9, &[1, 0]).unwrap();
        let every = (0..MAX_HANDOFF_WINDOWS)
            .map(|index| u8::try_from(index).unwrap())
            .collect::<Vec<_>>();
        validate_handoff_decision(9, &every).unwrap();

        let mut excess = every.clone();
        excess.push(0);
        for (request_id, proceed_without) in [
            (0, vec![0]),
            (9, Vec::new()),
            (9, vec![2, 2]),
            (9, vec![u8::try_from(MAX_HANDOFF_WINDOWS).unwrap()]),
            (9, vec![u8::MAX]),
            (9, excess.clone()),
        ] {
            assert_eq!(
                validate_handoff_decision(request_id, &proceed_without)
                    .unwrap_err()
                    .code,
                ErrorCode::InvalidArgument
            );
        }
        // Decoding rejects a list one element past its bound.
        assert!(
            serde_json::from_value::<ClientFrame>(serde_json::json!({
                "type": "handoff_decision",
                "request_id": 9,
                "proceed_without": excess,
            }))
            .is_err()
        );
    }

    fn consent_offer() -> ConsentOffer {
        ConsentOffer {
            daemon_generation: 3,
            offer_id: 7,
            request_id: 11,
            nonce: ConsentOfferNonce::new([0xa5; CONSENT_OFFER_NONCE_BYTES]),
            respond_within_seconds: 118,
            prompt: consent_prompt().offered(),
        }
    }

    fn round_trip_request(request: &Request) -> serde_json::Value {
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(
            &serde_json::from_value::<Request>(value.clone()).unwrap(),
            request
        );
        let frame = ClientFrame::Request {
            request_id: 5,
            diagnostic_correlation: None,
            request: request.clone(),
        };
        let encoded = serde_json::to_vec(&frame).unwrap();
        assert_eq!(
            serde_json::from_slice::<ClientFrame>(&encoded).unwrap(),
            frame
        );
        value
    }

    #[test]
    fn consent_responder_requests_round_trip() {
        for (kind, name) in [
            (ConsentResponderKind::Window, "window"),
            (ConsentResponderKind::Terminal, "terminal"),
        ] {
            let value = round_trip_request(&Request::SubscribeConsentOffers { kind });
            assert_eq!(
                value,
                serde_json::json!({"type": "subscribe_consent_offers", "kind": name})
            );
        }
        // An unknown responder kind never decodes.
        assert!(
            serde_json::from_value::<Request>(serde_json::json!({
                "type": "subscribe_consent_offers",
                "kind": "automation",
            }))
            .is_err()
        );

        for decision in [
            ConsentDecision::Deny,
            ConsentDecision::Grant {
                duration: GrantDuration::Remembered,
            },
        ] {
            let answer = Request::AnswerConsentOffer {
                daemon_generation: 3,
                offer_id: 7,
                nonce: ConsentOfferNonce::new([0xa5; CONSENT_OFFER_NONCE_BYTES]),
                decision,
            };
            let value = round_trip_request(&answer);
            assert_eq!(value["type"], "answer_consent_offer");
            assert_eq!(
                value["nonce"],
                serde_json::json!(vec![0xa5_u8; CONSENT_OFFER_NONCE_BYTES]),
                "the nonce keeps the plain fixed-size array wire shape"
            );
            assert_eq!(validate_consent_answer(3, 7), Ok(()));
        }
    }

    #[test]
    fn consent_responder_responses_and_events_round_trip() {
        for (result, expected) in [
            (
                Response::ConsentOffersSubscribed { subscription_id: 9 },
                serde_json::json!({"type": "consent_offers_subscribed", "subscription_id": 9}),
            ),
            (
                Response::ConsentOfferAnswered,
                serde_json::json!({"type": "consent_offer_answered"}),
            ),
        ] {
            assert_eq!(serde_json::to_value(&result).unwrap(), expected);
            assert_eq!(
                serde_json::from_value::<Response>(expected).unwrap(),
                result
            );
            let frame = ServerFrame::Response {
                request_id: 5,
                result,
            };
            let encoded = serde_json::to_vec(&frame).unwrap();
            assert_eq!(
                serde_json::from_slice::<ServerFrame>(&encoded).unwrap(),
                frame
            );
        }

        let offer = consent_offer();
        assert_eq!(offer.validate(), Ok(()));
        let mut events = vec![SubscriptionEvent::ConsentOffered {
            offer: offer.clone(),
        }];
        for (reason, name) in [
            (ConsentOfferEnd::Cancelled, "cancelled"),
            (ConsentOfferEnd::Expired, "expired"),
            (ConsentOfferEnd::RequestEnded, "request_ended"),
            (ConsentOfferEnd::Fenced, "fenced"),
            (ConsentOfferEnd::AnsweredElsewhere, "answered_elsewhere"),
        ] {
            assert_eq!(serde_json::to_value(reason).unwrap(), name);
            events.push(SubscriptionEvent::ConsentOfferWithdrawn {
                offer_id: offer.offer_id,
                reason,
            });
        }
        for event in events {
            let value = serde_json::to_value(&event).unwrap();
            assert_eq!(
                serde_json::from_value::<SubscriptionEvent>(value).unwrap(),
                event
            );
            let frame = ServerFrame::Event {
                subscription_id: 9,
                sequence: 1,
                event,
            };
            let encoded = serde_json::to_vec(&frame).unwrap();
            assert_eq!(
                serde_json::from_slice::<ServerFrame>(&encoded).unwrap(),
                frame
            );
        }
        let value = serde_json::to_value(SubscriptionEvent::ConsentOffered {
            offer: offer.clone(),
        })
        .unwrap();
        assert_eq!(value["type"], "consent_offered");
        assert_eq!(value["offer"]["request_id"], 11);
        assert_eq!(value["offer"]["respond_within_seconds"], 118);
        // The offered prompt carries no capability, origin, or countdown.
        for absent in ["capability", "origin", "respond_within_seconds"] {
            assert!(value["offer"]["prompt"].get(absent).is_none(), "{absent}");
        }
        assert_eq!(
            serde_json::to_value(SubscriptionEvent::ConsentOfferWithdrawn {
                offer_id: 7,
                reason: ConsentOfferEnd::AnsweredElsewhere,
            })
            .unwrap(),
            serde_json::json!({
                "type": "consent_offer_withdrawn",
                "offer_id": 7,
                "reason": "answered_elsewhere",
            })
        );
        for (kind, name) in [
            (ConsentResponderKind::Window, "window"),
            (ConsentResponderKind::Terminal, "terminal"),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), name);
        }
    }

    #[test]
    fn consent_offer_nonce_is_exactly_its_length_and_never_printed() {
        let nonce = ConsentOfferNonce::new([0xa5; CONSENT_OFFER_NONCE_BYTES]);
        let answer = Request::AnswerConsentOffer {
            daemon_generation: 3,
            offer_id: 7,
            nonce,
            decision: ConsentDecision::Deny,
        };
        for debug in [
            format!("{nonce:?}"),
            format!("{answer:?}"),
            format!("{answer:#?}"),
        ] {
            assert!(debug.contains("32 bytes redacted"), "{debug}");
            assert!(!debug.contains("165"), "{debug}");
            assert!(!debug.to_ascii_lowercase().contains("a5"), "{debug}");
        }
        let offered = SubscriptionEvent::ConsentOffered {
            offer: consent_offer(),
        };
        for debug in [format!("{offered:?}"), format!("{offered:#?}")] {
            assert!(debug.contains("32 bytes redacted"), "{debug}");
            assert!(!debug.contains("165, 165"), "{debug}");
            assert!(!debug.contains("165,\n"), "{debug}");
        }

        // Equality compares every byte.
        let mut last = [0xa5; CONSENT_OFFER_NONCE_BYTES];
        last[CONSENT_OFFER_NONCE_BYTES - 1] = 0;
        assert_eq!(
            nonce,
            ConsentOfferNonce::new([0xa5; CONSENT_OFFER_NONCE_BYTES])
        );
        assert_ne!(nonce, ConsentOfferNonce::new(last));
        assert_ne!(
            nonce,
            ConsentOfferNonce::new([0; CONSENT_OFFER_NONCE_BYTES])
        );
        assert_eq!(nonce.as_bytes(), &[0xa5; CONSENT_OFFER_NONCE_BYTES]);

        let event = serde_json::to_value(&offered).unwrap();
        for length in [
            0,
            CONSENT_OFFER_NONCE_BYTES - 1,
            CONSENT_OFFER_NONCE_BYTES + 1,
            4096,
        ] {
            assert!(
                serde_json::from_value::<Request>(serde_json::json!({
                    "type": "answer_consent_offer",
                    "daemon_generation": 3,
                    "offer_id": 7,
                    "nonce": vec![1_u8; length],
                    "decision": {"decision": "deny"},
                }))
                .is_err(),
                "a {length}-byte answer nonce is rejected while decoding"
            );
            let mut wrong = event.clone();
            wrong["offer"]["nonce"] = serde_json::json!(vec![1_u8; length]);
            assert!(
                serde_json::from_value::<SubscriptionEvent>(wrong).is_err(),
                "a {length}-byte offer nonce is rejected while decoding"
            );
        }
    }

    #[test]
    fn consent_prompt_countdown_and_origin_are_additive() {
        let prompt = consent_prompt();
        let encoded = serde_json::to_value(&prompt).unwrap();
        assert_eq!(encoded["respond_within_seconds"], 0);
        assert_eq!(encoded["origin"], serde_json::json!({"kind": "local"}));

        // A protocol 44 prompt, without either field, decodes with no
        // countdown and the local origin.
        let mut old = encoded.clone();
        let fields = old.as_object_mut().unwrap();
        fields.remove("respond_within_seconds").unwrap();
        fields.remove("origin").unwrap();
        let decoded = serde_json::from_value::<ConsentPrompt>(old).unwrap();
        assert_eq!(decoded, prompt);
        assert_eq!(decoded.respond_within_seconds, 0);
        assert_eq!(decoded.origin, ConsentOrigin::Local);

        // A newer prompt's unknown field still decodes.
        let mut newer = encoded;
        newer["a_later_field"] = serde_json::json!({"anything": [1, 2, 3]});
        assert_eq!(
            serde_json::from_value::<ConsentPrompt>(newer).unwrap(),
            prompt
        );

        let mut remote = consent_prompt();
        remote.respond_within_seconds = MAX_CONSENT_RESPOND_WITHIN_SECONDS;
        remote.origin = ConsentOrigin::Remote {
            profile: "neuromancer".to_owned(),
            daemon_hostname: Some("neuromancer.lan".to_owned()),
        };
        assert_eq!(remote.validate(), Ok(()));
        let encoded = serde_json::to_value(&remote).unwrap();
        assert_eq!(
            encoded["origin"],
            serde_json::json!({
                "kind": "remote",
                "profile": "neuromancer",
                "daemon_hostname": "neuromancer.lan",
            })
        );
        assert_eq!(
            serde_json::from_value::<ConsentPrompt>(encoded).unwrap(),
            remote
        );
        assert!(
            serde_json::from_value::<ConsentOrigin>(serde_json::json!({"kind": "elsewhere"}))
                .is_err()
        );
    }

    #[test]
    fn offered_prompts_share_prompt_validation() {
        let prompt = consent_prompt();
        let offered = prompt.offered();
        assert_eq!(offered.validate(), Ok(()));
        for duration in GrantDuration::ALL {
            assert_eq!(offered.offers(duration), prompt.offers(duration));
        }
        // A responder rebuilds the dialog prompt with its own capability,
        // the offer's countdown, and its own origin.
        let origin = ConsentOrigin::Remote {
            profile: "loop".to_owned(),
            daemon_hostname: Some("localhost".to_owned()),
        };
        let rebuilt =
            offered
                .clone()
                .into_prompt(vec![9; CONSENT_CAPABILITY_BYTES], 30, origin.clone());
        assert_eq!(rebuilt.validate(), Ok(()));
        assert_eq!(rebuilt.offered(), offered);
        assert_eq!(rebuilt.capability, vec![9; CONSENT_CAPABILITY_BYTES]);
        assert_eq!(rebuilt.respond_within_seconds, 30);
        assert_eq!(rebuilt.origin, origin);
        assert_eq!(
            serde_json::from_value::<OfferedConsentPrompt>(serde_json::to_value(&offered).unwrap())
                .unwrap(),
            offered
        );

        // The same content rules reject the same prompts, capability aside.
        let invalid = |change: fn(&mut ConsentPrompt)| {
            let mut prompt = consent_prompt();
            change(&mut prompt);
            let offered = prompt.offered();
            assert_eq!(prompt.validate().is_err(), offered.validate().is_err());
            offered.validate()
        };
        assert!(invalid(|prompt| prompt.scopes.clear()).is_err());
        assert!(invalid(|prompt| prompt.level = Some(AccessLevel::Look)).is_err());
        assert!(invalid(|prompt| prompt.durations.swap(1, 2)).is_err());
        assert!(
            invalid(|prompt| prompt.agent_name = Some("a".repeat(MAX_AGENT_NAME_CHARS + 1)))
                .is_err()
        );
        assert!(
            invalid(|prompt| prompt.requester = "r".repeat(MAX_CONSENT_PATH_CHARS + 1)).is_err()
        );
        // The capability is not part of the offered content.
        let mut no_capability = consent_prompt();
        no_capability.capability.clear();
        assert!(no_capability.validate().is_err());
        assert_eq!(no_capability.offered().validate(), Ok(()));

        // Only workspace access is offered to responders.
        let mut splint = consent_prompt().offered();
        splint.target = ConsentTarget::Splint {
            splint_id: SplintId::new(),
            incarnation: 1,
        };
        assert!(splint.validate().is_err());
        let mut offer = consent_offer();
        offer.prompt = splint;
        assert_eq!(
            offer.validate().unwrap_err().code,
            ErrorCode::InvalidArgument
        );
        let mut offer = consent_offer();
        offer.prompt.durations.clear();
        assert_eq!(
            offer.validate().unwrap_err().code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn consent_responder_bounds_reject_out_of_range_values() {
        // Countdowns: at most the pending request lifetime.
        assert_eq!(MAX_CONSENT_RESPOND_WITHIN_SECONDS, 120);
        let mut prompt = consent_prompt();
        prompt.respond_within_seconds = MAX_CONSENT_RESPOND_WITHIN_SECONDS;
        assert_eq!(prompt.validate(), Ok(()));
        prompt.respond_within_seconds = MAX_CONSENT_RESPOND_WITHIN_SECONDS + 1;
        assert!(prompt.validate().is_err());
        for seconds in [0, MAX_CONSENT_RESPOND_WITHIN_SECONDS + 1, u64::MAX] {
            let mut offer = consent_offer();
            offer.respond_within_seconds = seconds;
            assert_eq!(
                offer.validate().unwrap_err().code,
                ErrorCode::InvalidArgument,
                "{seconds}"
            );
        }
        for seconds in [1, MAX_CONSENT_RESPOND_WITHIN_SECONDS] {
            let mut offer = consent_offer();
            offer.respond_within_seconds = seconds;
            assert_eq!(offer.validate(), Ok(()));
        }
        let mut offer = consent_offer();
        offer.daemon_generation = 0;
        assert!(offer.validate().is_err());
        let mut offer = consent_offer();
        offer.offer_id = 0;
        assert!(offer.validate().is_err());

        // An answer names a nonzero generation and offer.
        assert_eq!(validate_consent_answer(3, 7), Ok(()));
        for (daemon_generation, offer_id) in [(0, 7), (3, 0), (0, 0)] {
            assert_eq!(
                validate_consent_answer(daemon_generation, offer_id)
                    .unwrap_err()
                    .code,
                ErrorCode::InvalidArgument,
                "{daemon_generation} {offer_id}"
            );
        }
    }

    #[test]
    fn consent_origins_name_a_usable_profile_and_hostname() {
        let remote = |profile: &str, daemon_hostname: Option<&str>| ConsentOrigin::Remote {
            profile: profile.to_owned(),
            daemon_hostname: daemon_hostname.map(str::to_owned),
        };
        assert_eq!(ConsentOrigin::Local.validate(), Ok(()));
        assert_eq!(
            remote("work-box_2.lan", Some("neuromancer")).validate(),
            Ok(())
        );
        assert_eq!(
            remote(&"p".repeat(MAX_REMOTE_PROFILE_NAME_BYTES), Some("h")).validate(),
            Ok(())
        );
        let longest_host = "h".repeat(MAX_DAEMON_HOSTNAME_BYTES);
        assert_eq!(remote("p", Some(&longest_host)).validate(), Ok(()));
        let long_profile = "p".repeat(MAX_REMOTE_PROFILE_NAME_BYTES + 1);
        let long_host = "h".repeat(MAX_DAEMON_HOSTNAME_BYTES + 1);
        for (profile, daemon_hostname) in [
            ("", Some("neuromancer")),
            ("", None),
            ("-oProxyCommand", None),
            ("two words", Some("neuromancer")),
            ("tab\there", Some("neuromancer")),
            ("bidi\u{202e}", Some("neuromancer")),
            ("caf\u{e9}", Some("neuromancer")),
            (long_profile.as_str(), Some("neuromancer")),
            ("neuromancer", Some("")),
            ("neuromancer", Some("two words")),
            ("neuromancer", Some("line\nbreak")),
            ("neuromancer", Some("bidi\u{2066}host")),
            ("neuromancer", Some(long_host.as_str())),
        ] {
            assert_eq!(
                usable_remote_profile_name(profile),
                profile == "neuromancer",
                "{profile:?}"
            );
            let origin = remote(profile, daemon_hostname);
            assert!(origin.validate().is_err(), "{origin:?}");
            let mut prompt = consent_prompt();
            prompt.origin = origin;
            assert!(prompt.validate().is_err(), "{prompt:?}");
        }
    }

    #[test]
    fn a_remote_origin_may_omit_the_hostname_and_is_never_local() {
        // A daemon that reported no usable hostname still yields a valid
        // remote origin from the Window's own profile.
        let profile_only = ConsentOrigin::Remote {
            profile: "loop".to_owned(),
            daemon_hostname: None,
        };
        assert_eq!(profile_only.validate(), Ok(()));
        let encoded = serde_json::to_value(&profile_only).unwrap();
        assert_eq!(
            encoded,
            serde_json::json!({"kind": "remote", "profile": "loop"})
        );
        assert_eq!(
            serde_json::from_value::<ConsentOrigin>(encoded).unwrap(),
            profile_only
        );
        let mut prompt = consent_prompt();
        prompt.origin = profile_only.clone();
        assert_eq!(prompt.validate(), Ok(()));

        // A remote origin, valid or not, never equals, encodes as, or
        // decodes to the local one, whatever hostname it claims.
        let local = serde_json::to_value(ConsentOrigin::Local).unwrap();
        assert_eq!(local, serde_json::json!({"kind": "local"}));
        for daemon_hostname in [None, Some("localhost"), Some("this-computer"), Some("")] {
            let remote = ConsentOrigin::Remote {
                profile: "loop".to_owned(),
                daemon_hostname: daemon_hostname.map(str::to_owned),
            };
            assert_ne!(remote, ConsentOrigin::Local);
            let encoded = serde_json::to_value(&remote).unwrap();
            assert_eq!(encoded["kind"], "remote");
            assert_ne!(encoded, local);
            assert_ne!(
                serde_json::from_value::<ConsentOrigin>(encoded).unwrap(),
                ConsentOrigin::Local
            );
            assert!(!format!("{remote:?}").starts_with("Local"));
        }
        // A remote origin without its profile never decodes, and so never
        // degrades to the local default.
        assert!(
            serde_json::from_value::<ConsentOrigin>(serde_json::json!({"kind": "remote"})).is_err()
        );
        let mut missing = serde_json::to_value(consent_prompt()).unwrap();
        missing["origin"] = serde_json::json!({"kind": "remote", "daemon_hostname": "h"});
        assert!(serde_json::from_value::<ConsentPrompt>(missing).is_err());
    }
}
