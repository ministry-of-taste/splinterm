#![forbid(unsafe_code)]
#![allow(
    clippy::missing_errors_doc,
    reason = "the extracted internal API preserves concise compatibility docs; errors remain typed"
)]

//! Internal daemon client boundary for non-Wayland automation.
//!
//! This Rust API is reusable by Splinterm clients, but is not part of the
//! supported JSON/NDJSON compatibility contract.

use std::{
    collections::{HashMap, VecDeque},
    env,
    io::{self, ErrorKind, IoSliceMut, Write as _},
    os::fd::{AsFd, OwnedFd},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, recvmsg};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use splinterm_core::{DojoId, LairId, LayoutNode, SplintId, TopologyRevision};
use splinterm_filemap::ReadOnlyFileMap;
use splinterm_protocol::{
    AccessGrant, AccessScope, AgentState, AgentStatusSummary, AuditDecision, AuditOutcome,
    AuditPage, AutomationScope, ClientFrame, ClientRole, ControlStatus, ControlTransferOutcome,
    ErrorCode, ImageContentMetadata, ImageContentRequest, ImageTransferMode, MAX_CWD_BYTES,
    MAX_FRAME_BYTES, MAX_IMAGE_CHUNK_BYTES, MAX_IMAGE_CHUNK_WINDOW, MAX_TERMINAL_TRANSACTION_BYTES,
    PROTOCOL_VERSION, PersistentAuthorizationStatus, Request, Response, RestoreLeafResult,
    ScrollbackPage, SearchPage, ServerFrame, ServerFrameTransactionAssembler, ServerLimits,
    SplintLifecycle, SplintRuntimeSummary, TerminalRow, TerminalSnapshot, TerminalUpdate,
    TopologyChangeKind, TopologySnapshot, encode_frame, image_content_socket_path,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::UnixStream,
};
use tokio_util::sync::CancellationToken;

mod access;
mod daemon_handoff;
mod mcp_setup;
mod reconnect;

pub use access::{
    AccessRevokeIdError, access_entry_id, access_kind_name, access_level_name,
    access_list_envelope, access_revoke_all_envelope, access_revoke_envelope, consent_scope_name,
    parse_access_entry_id, parse_access_revoke_target, public_display_text,
};

pub use mcp_setup::{
    MAX_MCP_SETUP_PATH_BYTES, McpAdapterIdentity, McpConfigFormat, McpHostName, McpSetupAction,
    McpSetupOperation, McpSetupOutcome, mcp_setup_envelope,
};

pub use daemon_handoff::{
    DaemonHandoffOutcome, HANDOFF_DECISION_SAFETY_MARGIN, HandoffDecision, HandoffDecisionPrompt,
};
pub use reconnect::{
    DaemonChange, DaemonIdentity, RECONNECT_INITIAL_DELAY, RECONNECT_MAX_DELAY, ReconnectBackoff,
    is_connection_loss, retry_until,
};

const READ_CHUNK_BYTES: usize = 16 * 1024;
const MAX_BUFFERED_BYTES: usize = MAX_FRAME_BYTES + 4 + READ_CHUNK_BYTES;
const MAX_QUEUED_EVENTS: usize = 64;
const MAX_QUEUED_EVENT_BYTES: usize = MAX_TERMINAL_TRANSACTION_BYTES + 4;
const CLI_SCHEMA_V2: &str = "splinterm.cli.v2";
const CLI_EVENT_SCHEMA_V2: &str = "splinterm.cli.event.v2";

fn decimal_id(value: u64, label: &str) -> Result<String> {
    if value == 0 {
        bail!("{label} must be nonzero");
    }
    Ok(value.to_string())
}

/// Stable public v2 symbolic error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CliErrorCodeV2 {
    AuthenticationFailed,
    HandshakeRequired,
    IncompatibleVersion,
    InvalidRequest,
    UnsupportedSchema,
    ConsentUnavailable,
    ConsentDenied,
    Unauthorized,
    ConfirmationRequired,
    ControllerUnavailable,
    ControlTransferUnavailable,
    StaleTopology,
    NotFound,
    StaleIncarnation,
    InvalidArgument,
    ResourceLimit,
    Cancelled,
    Timeout,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct CliErrorV2 {
    code: CliErrorCodeV2,
    message: String,
    retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_topology_revision: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_terminal_revision: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct GenericEnvelopeDataV2 {
    schema: &'static str,
    request_id: String,
    operation: &'static str,
    ok: bool,
    truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    resource: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<CliErrorV2>,
}

/// Opaque explicit v1 envelope for reviewed operation projections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CliEnvelopeV2(GenericEnvelopeDataV2);

impl CliEnvelopeV2 {
    /// Creates a successful operation envelope from an explicit public projection.
    fn success(
        operation: &'static str,
        resource: Option<Value>,
        data: Value,
        truncated: bool,
    ) -> Result<Self> {
        Ok(Self(GenericEnvelopeDataV2 {
            schema: CLI_SCHEMA_V2,
            request_id: decimal_id(1, "request ID")?,
            operation,
            ok: true,
            truncated,
            resource,
            data: Some(data),
            error: None,
        }))
    }

    /// Creates a failed operation envelope with no terminal or query body.
    pub fn failure(
        operation: &'static str,
        code: CliErrorCodeV2,
        message: impl Into<String>,
        retryable: bool,
    ) -> Result<Self> {
        let message = message.into();
        if message.is_empty() || message.chars().count() > 1024 {
            bail!("public error message length is outside 1..=1024 characters");
        }
        Ok(Self(GenericEnvelopeDataV2 {
            schema: CLI_SCHEMA_V2,
            request_id: decimal_id(1, "request ID")?,
            operation,
            ok: false,
            truncated: false,
            resource: None,
            data: None,
            error: Some(CliErrorV2 {
                code,
                message,
                retryable,
                current_topology_revision: None,
                current_terminal_revision: None,
            }),
        }))
    }

    /// Creates a failed operation envelope preserving reviewed revision evidence.
    pub fn protocol_failure(
        operation: &'static str,
        error: &splinterm_protocol::ProtocolError,
        message: impl Into<String>,
    ) -> Result<Self> {
        let message = message.into();
        if message.is_empty() || message.chars().count() > 1024 {
            bail!("public error message length is outside 1..=1024 characters");
        }
        let (code, retryable) = public_error_code(error.code);
        Ok(Self(GenericEnvelopeDataV2 {
            schema: CLI_SCHEMA_V2,
            request_id: decimal_id(1, "request ID")?,
            operation,
            ok: false,
            truncated: false,
            resource: None,
            data: None,
            error: Some(CliErrorV2 {
                code,
                message,
                retryable,
                current_topology_revision: error
                    .current_topology_revision
                    .map(TopologyRevision::get),
                current_terminal_revision: None,
            }),
        }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct LairSummaryV2 {
    lair_id: String,
    name: String,
    dojo_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct DojoSummaryV2 {
    lair_id: String,
    dojo_id: String,
    name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SplintLifecycleV2 {
    Running,
    Exited,
    Restorable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SplintSummaryV2 {
    lair_id: String,
    dojo_id: String,
    splint_id: String,
    title: String,
    lifecycle: SplintLifecycleV2,
    current_incarnation: Option<u64>,
    last_incarnation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_status: Option<AgentStatusV2>,
}

/// Explicitly reported display status of a live Splint incarnation.
///
/// The detail is reporter-supplied display text: bounded and free of control,
/// bidi-formatting, and line-break characters, but never authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AgentStatusV2 {
    state: &'static str,
    detail: Option<String>,
    stale: bool,
    reported_age_ms: u64,
    sequence: u64,
}

fn agent_status_projection(summary: &AgentStatusSummary) -> Result<AgentStatusV2> {
    if !summary.validate() {
        bail!("agent status exceeds public v2 bounds");
    }
    Ok(AgentStatusV2 {
        state: summary.state.as_str(),
        detail: summary.detail.clone(),
        stale: summary.stale,
        reported_age_ms: summary.reported_age_ms,
        sequence: summary.sequence,
    })
}

fn runtime_agent_status(runtime: &SplintRuntimeSummary) -> Result<Option<AgentStatusV2>> {
    match (&runtime.agent_status, runtime.live_incarnation) {
        (None, _) => Ok(None),
        (Some(status), Some(_)) => agent_status_projection(status).map(Some),
        (Some(_), None) => bail!("agent status was attached to an exited Splint"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AgentStatusResourceV2 {
    splint_id: String,
    incarnation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AgentStatusSetDataV2 {
    accepted: bool,
    state: &'static str,
    sequence: u64,
    expires_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AgentStatusClearDataV2 {
    accepted: bool,
    cleared: bool,
    sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AgentStatusGetDataV2 {
    current_incarnation: Option<u64>,
    agent_status: Option<AgentStatusV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ListLairsDataV2 {
    lairs: Vec<LairSummaryV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct InspectTopologyDataV2 {
    lairs: Vec<LairSummaryV2>,
    dojos: Vec<DojoSummaryV2>,
    splints: Vec<SplintSummaryV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct InspectSplintResourceV2 {
    lair_id: String,
    dojo_id: String,
    splint_id: String,
    topology_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct InspectSplintDataV2 {
    title: String,
    lifecycle: SplintLifecycleV2,
    current_incarnation: Option<u64>,
    last_incarnation: Option<u64>,
    exit_code: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_status: Option<AgentStatusV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TerminalReadResourceV2 {
    lair_id: String,
    dojo_id: String,
    splint_id: String,
    incarnation: u64,
    terminal_revision: u64,
    history_generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalReadProvenanceV2 {
    pub lair_id: LairId,
    pub dojo_id: DojoId,
    pub splint_id: SplintId,
    pub incarnation: u64,
    pub terminal_revision: u64,
    pub history_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TerminalActionDataV2 {
    acknowledged: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    columns: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rows: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TerminalReadRowV2 {
    row_id: Option<u64>,
    linebreak: bool,
    cells: Vec<ProjectedTerminalCell>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TerminalCursorV2 {
    column: usize,
    row: usize,
    visible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TerminalSnapshotDataV2 {
    content_encoding: &'static str,
    columns: usize,
    rows: Vec<TerminalReadRowV2>,
    cursor: TerminalCursorV2,
    continuation_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ScrollbackPageDataV2 {
    kind: &'static str,
    content_encoding: &'static str,
    rows: Vec<TerminalReadRowV2>,
    has_older: bool,
    continuation_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SearchMatchV2 {
    row_id: u64,
    start_column: usize,
    end_column: usize,
    preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SearchResultsDataV2 {
    kind: &'static str,
    matches: Vec<SearchMatchV2>,
    timed_out: bool,
    continuation_cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadResyncReasonV2 {
    StaleRevision,
    HistoryReplaced,
    RetentionGap,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ReadResyncDataV2 {
    kind: &'static str,
    reason: ReadResyncReasonV2,
    continuation_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalContinuationV2 {
    Scrollback {
        splint_id: SplintId,
        incarnation: u64,
        terminal_revision: u64,
        history_generation: u64,
        before_row_id: u64,
    },
    Search {
        splint_id: SplintId,
        incarnation: u64,
        terminal_revision: u64,
        history_generation: u64,
        daemon_cursor: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AuthorizationResourceV2 {
    lair_id: String,
    dojo_id: String,
    splint_id: String,
    incarnation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AuthorizationGrantV2 {
    #[serde(skip_serializing_if = "Option::is_none")]
    grant_id: Option<String>,
    source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    policy_rule_id: Option<String>,
    scopes: Vec<&'static str>,
    expires_unix_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AuthorizationStatusDataV2 {
    grants: Vec<AuthorizationGrantV2>,
    development_bypass: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AuditPeerV2 {
    uid: u32,
    executable_path: String,
    executable_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    device: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inode: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AuditResourceV2 {
    #[serde(skip_serializing_if = "Option::is_none")]
    lair_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dojo_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    splint_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    incarnation: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AuditRecordV2 {
    schema: &'static str,
    retention: &'static str,
    audit_id: String,
    unix_seconds: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    policy_generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    policy_rule_id: Option<String>,
    peer: AuditPeerV2,
    operation: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    resource: Option<AuditResourceV2>,
    requested_scopes: Vec<&'static str>,
    decision: &'static str,
    reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    argument_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    executable_basename: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AuditDataV2 {
    retention: &'static str,
    records: Vec<AuditRecordV2>,
    retention_gap: bool,
    oldest_available_audit_id: Option<String>,
    newest_available_audit_id: Option<String>,
    next_after_audit_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct MutationResourceV2 {
    #[serde(skip_serializing_if = "Option::is_none")]
    lair_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dojo_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    splint_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    topology_revision: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    incarnation: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct CreatedResultV2 {
    created: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct MutationResultV2 {
    committed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    confirmed: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct ProcessStartedV2 {
    started: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct RestoreLeafV2 {
    splint_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    incarnation: Option<u64>,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<CliErrorCodeV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct RestoreManyV2 {
    results: Vec<RestoreLeafV2>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct KillResultV2 {
    terminated: bool,
    confirmed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct RevokeResultV2 {
    revoked_grant_id: String,
    confirmed: bool,
}

fn checked_public_text(value: &str, label: &str) -> Result<String> {
    if value.chars().count() > 255 {
        bail!("{label} exceeds public v2 bounds");
    }
    Ok(value.to_owned())
}

fn project_terminal_cells(
    row: &splinterm_protocol::TerminalRow,
) -> Result<Vec<ProjectedTerminalCell>> {
    let mut cells = Vec::new();
    let mut index = 0;
    while index < row.cells.len() {
        let cell = &row.cells[index];
        if cell.spacer_remaining.is_some() {
            index += 1;
            continue;
        }
        let mut width = 1_usize;
        while index + width < row.cells.len()
            && row.cells[index + width]
                .spacer_remaining
                .is_some_and(|remaining| remaining > 0)
        {
            width += 1;
        }
        if width > 2 || cell.content.chars().count() > 64 {
            bail!("terminal cell exceeds public v2 bounds");
        }
        cells.push(ProjectedTerminalCell {
            text: cell.content.clone(),
            width: u8::try_from(width)?,
        });
        index += width;
    }
    Ok(cells)
}

/// Projects semantic terminal rows into the shared bounded public shape.
///
/// Spacer cells are collapsed into their leading cell, preserving a display width
/// of one or two columns without exposing terminal attributes or private row IDs.
pub fn project_terminal_rows(
    rows: &[splinterm_protocol::TerminalRow],
) -> Result<Vec<ProjectedTerminalRow>> {
    rows.iter()
        .map(|row| {
            Ok(ProjectedTerminalRow {
                linebreak: row.linebreak,
                cells: project_terminal_cells(row)?,
            })
        })
        .collect()
}

fn blank_terminal_row(columns: usize) -> TerminalRow {
    TerminalRow {
        row_id: None,
        linebreak: false,
        cells: Vec::with_capacity(columns),
    }
}

/// Applies one validated aggregate update to a retained non-Wayland snapshot.
///
/// History replacement, clear, and reflow require an explicit resnapshot and are
/// rejected rather than presenting retained terminal contents as current.
pub fn apply_terminal_update(
    snapshot: &mut TerminalSnapshot,
    update: TerminalUpdate,
) -> Result<()> {
    update
        .validate_against(
            snapshot.revision,
            snapshot.history_generation,
            snapshot.columns,
            snapshot.rows,
        )
        .map_err(|error| anyhow::anyhow!(error.message))?;
    if update.scrollback.as_ref().is_some_and(|scrollback| {
        !matches!(
            scrollback.transition,
            splinterm_protocol::HistoryTransition::Append { .. }
        )
    }) {
        bail!("terminal history replacement requires resynchronization");
    }

    // Keep the retained state intact unless every dimension, identity, and
    // projection invariant accepts the complete aggregate update.
    let mut candidate = snapshot.clone();
    if let Some(columns) = update.columns {
        candidate.columns = columns;
        for row in &mut candidate.visible_rows {
            row.cells.truncate(columns);
        }
    }
    if let Some(rows) = update.row_count {
        candidate.rows = rows;
        candidate
            .visible_rows
            .resize_with(rows, || blank_terminal_row(candidate.columns));
        candidate.visible_rows.truncate(rows);
    }
    for patch in update.rows {
        if patch.index >= candidate.rows || patch.row.cells.len() > candidate.columns {
            bail!("terminal row patch exceeds current dimensions");
        }
        candidate.visible_rows[patch.index] = patch.row;
    }
    if let Some(cursor) = update.cursor {
        candidate.cursor_column = cursor.column;
        candidate.cursor_row = cursor.row;
        candidate.cursor_deferred_wrap = cursor.deferred_wrap;
    }
    if let Some(title) = update.title {
        candidate.title = title;
    }
    if let Some(modes) = update.input_modes {
        candidate.input_modes = modes;
    }
    if let Some(screen) = update.active_screen {
        candidate.active_screen = screen;
    }
    if let Some(palette) = update.palette {
        candidate.palette = palette;
    }
    if let Some(colors) = update.default_colors {
        candidate.default_colors = colors;
    }
    if let Some(scrollback) = update.scrollback {
        candidate.history_generation = scrollback.history_generation;
        candidate.oldest_available_scrollback_row_id = scrollback.oldest_available_row_id;
        candidate.newest_available_scrollback_row_id = scrollback.newest_available_row_id;
        candidate.scrollback_rows.clear();
        candidate.available_scrollback_rows = scrollback.available_rows;
        candidate.omitted_oldest_scrollback_rows = scrollback.available_rows;
    }
    candidate.revision = update.revision;
    candidate
        .validate()
        .map_err(|error| anyhow::anyhow!(error.message))?;
    *snapshot = candidate;
    Ok(())
}

fn public_rows(rows: &[splinterm_protocol::TerminalRow]) -> Result<Vec<TerminalReadRowV2>> {
    rows.iter()
        .zip(project_terminal_rows(rows)?)
        .map(|(row, projected)| {
            if projected
                .cells
                .iter()
                .any(|cell| cell.text.chars().count() > 32)
            {
                bail!("terminal cell exceeds public v2 bounds");
            }
            Ok(TerminalReadRowV2 {
                row_id: row.row_id,
                linebreak: projected.linebreak,
                cells: projected.cells,
            })
        })
        .collect()
}

fn base64url_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let bits = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        output.push(char::from(ALPHABET[((bits >> 18) & 63) as usize]));
        output.push(char::from(ALPHABET[((bits >> 12) & 63) as usize]));
        if chunk.len() > 1 {
            output.push(char::from(ALPHABET[((bits >> 6) & 63) as usize]));
        }
        if chunk.len() > 2 {
            output.push(char::from(ALPHABET[(bits & 63) as usize]));
        }
    }
    output
}

fn base64url_decode(input: &str) -> Result<Vec<u8>> {
    if !(16..=256).contains(&input.len()) {
        bail!("continuation cursor length is outside public v2 bounds");
    }
    let value = |byte| match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'-' => Some(62),
        b'_' => Some(63),
        _ => None,
    };
    let mut output = Vec::with_capacity(input.len() * 3 / 4);
    let mut accumulator = 0_u32;
    let mut bits = 0_u8;
    for byte in input.bytes() {
        accumulator = (accumulator << 6)
            | u32::from(value(byte).context("continuation cursor is not base64url")?);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push(u8::try_from(accumulator >> bits)?);
            accumulator &= (1_u32 << bits).wrapping_sub(1);
        }
    }
    if bits >= 6 || accumulator != 0 || base64url_encode(&output) != input {
        bail!("continuation cursor is not canonical base64url");
    }
    Ok(output)
}

fn push_u64(output: &mut Vec<u8>, value: u64, label: &str) -> Result<()> {
    if value == 0 {
        bail!("{label} must be nonzero");
    }
    output.extend_from_slice(&value.to_be_bytes());
    Ok(())
}

fn read_cursor_u64(input: &[u8], offset: &mut usize) -> Result<u64> {
    let end = offset
        .checked_add(8)
        .context("continuation cursor overflow")?;
    let bytes: [u8; 8] = input
        .get(*offset..end)
        .context("continuation cursor is truncated")?
        .try_into()?;
    *offset = end;
    Ok(u64::from_be_bytes(bytes))
}

fn read_u64(input: &[u8], offset: &mut usize, label: &str) -> Result<u64> {
    let value = read_cursor_u64(input, offset)?;
    if value == 0 {
        bail!("continuation cursor {label} must be nonzero");
    }
    Ok(value)
}

/// Encodes private paging provenance into an opaque public v2 cursor.
pub fn encode_terminal_cursor(cursor: &TerminalContinuationV2) -> Result<String> {
    let (kind, splint_id, incarnation, revision, generation) = match cursor {
        TerminalContinuationV2::Scrollback {
            splint_id,
            incarnation,
            terminal_revision,
            history_generation,
            ..
        } => (
            1,
            splint_id,
            incarnation,
            terminal_revision,
            history_generation,
        ),
        TerminalContinuationV2::Search {
            splint_id,
            incarnation,
            terminal_revision,
            history_generation,
            ..
        } => (
            2,
            splint_id,
            incarnation,
            terminal_revision,
            history_generation,
        ),
    };
    let mut bytes = vec![1, kind];
    bytes.extend_from_slice(splint_id.to_string().as_bytes());
    push_u64(&mut bytes, *incarnation, "incarnation")?;
    bytes.extend_from_slice(&revision.to_be_bytes());
    push_u64(&mut bytes, *generation, "history generation")?;
    match cursor {
        TerminalContinuationV2::Scrollback { before_row_id, .. } => {
            push_u64(&mut bytes, *before_row_id, "before row ID")?;
        }
        TerminalContinuationV2::Search { daemon_cursor, .. } => {
            let length = u8::try_from(daemon_cursor.len())?;
            if length == 0 || length > 32 || !daemon_cursor.is_ascii() {
                bail!("daemon search cursor exceeds public v2 bounds");
            }
            bytes.push(length);
            bytes.extend_from_slice(daemon_cursor.as_bytes());
        }
    }
    let encoded = base64url_encode(&bytes);
    if encoded.len() > 256 {
        bail!("continuation cursor exceeds public v2 bounds");
    }
    Ok(encoded)
}

/// Decodes and validates an opaque public v2 terminal cursor.
pub fn decode_terminal_cursor(encoded: &str) -> Result<TerminalContinuationV2> {
    let bytes = base64url_decode(encoded)?;
    let (&version, rest) = bytes
        .split_first()
        .context("continuation cursor is empty")?;
    let (&kind, rest) = rest
        .split_first()
        .context("continuation cursor omits kind")?;
    if version != 1 || rest.len() < 60 {
        bail!("unsupported or truncated continuation cursor");
    }
    let splint_id = std::str::from_utf8(&rest[..36])?.parse()?;
    let mut offset = 36;
    let incarnation = read_u64(rest, &mut offset, "incarnation")?;
    let terminal_revision = read_cursor_u64(rest, &mut offset)?;
    let history_generation = read_u64(rest, &mut offset, "history generation")?;
    match kind {
        1 => {
            let before_row_id = read_u64(rest, &mut offset, "before row ID")?;
            if offset != rest.len() {
                bail!("scrollback cursor has trailing data");
            }
            Ok(TerminalContinuationV2::Scrollback {
                splint_id,
                incarnation,
                terminal_revision,
                history_generation,
                before_row_id,
            })
        }
        2 => {
            let length = usize::from(*rest.get(offset).context("search cursor omits length")?);
            offset += 1;
            let daemon_cursor = std::str::from_utf8(
                rest.get(offset..offset + length)
                    .context("search cursor is truncated")?,
            )?
            .to_owned();
            if length == 0 || offset + length != rest.len() {
                bail!("search cursor has invalid length");
            }
            Ok(TerminalContinuationV2::Search {
                splint_id,
                incarnation,
                terminal_revision,
                history_generation,
                daemon_cursor,
            })
        }
        _ => bail!("continuation cursor kind is unsupported"),
    }
}

fn public_lifecycle(runtime: &SplintRuntimeSummary) -> SplintLifecycleV2 {
    match runtime.lifecycle {
        SplintLifecycle::Starting | SplintLifecycle::Running => SplintLifecycleV2::Running,
        SplintLifecycle::Exited if runtime.restorable => SplintLifecycleV2::Restorable,
        SplintLifecycle::Exited => SplintLifecycleV2::Exited,
    }
}

fn runtime_for(snapshot: &TopologySnapshot, splint_id: SplintId) -> Result<&SplintRuntimeSummary> {
    snapshot
        .runtimes
        .iter()
        .find(|runtime| runtime.splint_id == splint_id)
        .context("validated topology omitted Splint runtime")
}

fn append_splint_summaries(
    node: &LayoutNode,
    snapshot: &TopologySnapshot,
    lair_id: splinterm_core::LairId,
    dojo_id: splinterm_core::DojoId,
    summaries: &mut Vec<SplintSummaryV2>,
) -> Result<()> {
    match node {
        LayoutNode::Leaf(splint) => {
            let runtime = runtime_for(snapshot, splint.id)?;
            summaries.push(SplintSummaryV2 {
                lair_id: lair_id.to_string(),
                dojo_id: dojo_id.to_string(),
                splint_id: splint.id.to_string(),
                title: checked_public_text(&splint.title, "Splint title")?,
                lifecycle: public_lifecycle(runtime),
                current_incarnation: runtime.live_incarnation,
                last_incarnation: runtime.last_incarnation,
                agent_status: runtime_agent_status(runtime)?,
            });
        }
        LayoutNode::Branch { first, second, .. } => {
            append_splint_summaries(first, snapshot, lair_id, dojo_id, summaries)?;
            append_splint_summaries(second, snapshot, lair_id, dojo_id, summaries)?;
        }
    }
    Ok(())
}

fn topology_projection(snapshot: &TopologySnapshot) -> Result<InspectTopologyDataV2> {
    snapshot
        .validate()
        .map_err(|error| anyhow::anyhow!(error.message))?;
    let mut lairs = Vec::new();
    let mut dojos = Vec::new();
    let mut splints = Vec::new();
    for lair in snapshot.topology.lairs() {
        lairs.push(LairSummaryV2 {
            lair_id: lair.id.to_string(),
            name: checked_public_text(&lair.name, "Lair name")?,
            dojo_count: lair.dojos.len(),
        });
        for dojo in &lair.dojos {
            dojos.push(DojoSummaryV2 {
                lair_id: lair.id.to_string(),
                dojo_id: dojo.id.to_string(),
                name: checked_public_text(&dojo.name, "Dojo name")?,
            });
            append_splint_summaries(&dojo.root, snapshot, lair.id, dojo.id, &mut splints)?;
        }
    }
    if lairs.len() > 256 || dojos.len() > 1_024 || splints.len() > 4_096 {
        bail!("topology exceeds public v2 collection bounds");
    }
    Ok(InspectTopologyDataV2 {
        lairs,
        dojos,
        splints,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct GraphicalFocusDataV2 {
    splint_id: Option<String>,
    cwd: Option<String>,
}

/// Converts the narrow graphical-focus projection to the reviewed public envelope.
pub fn graphical_focus_envelope(
    focused_splint_id: Option<SplintId>,
    cwd: Option<&Path>,
) -> Result<CliEnvelopeV2> {
    if focused_splint_id.is_none() && cwd.is_some() {
        bail!("graphical focus CWD requires a focused Splint");
    }
    let cwd = cwd
        .map(|path| {
            if !path.is_absolute() || path.as_os_str().as_encoded_bytes().len() > MAX_CWD_BYTES {
                bail!("graphical focus CWD is outside public bounds");
            }
            path.to_str()
                .map(str::to_owned)
                .context("graphical focus CWD is not UTF-8")
        })
        .transpose()?;
    CliEnvelopeV2::success(
        "focus",
        None,
        serde_json::to_value(GraphicalFocusDataV2 {
            splint_id: focused_splint_id.map(|id| id.to_string()),
            cwd,
        })?,
        false,
    )
}

/// Converts a validated private topology snapshot to the reviewed `list_lairs` envelope.
pub fn list_lairs_envelope(snapshot: &TopologySnapshot) -> Result<CliEnvelopeV2> {
    let projection = topology_projection(snapshot)?;
    CliEnvelopeV2::success(
        "list_lairs",
        Some(serde_json::to_value(TopologyResourceV2 {
            topology_revision: snapshot.revision.get(),
        })?),
        serde_json::to_value(ListLairsDataV2 {
            lairs: projection.lairs,
        })?,
        false,
    )
}

/// Converts a validated private topology snapshot to the reviewed topology envelope.
pub fn inspect_topology_envelope(snapshot: &TopologySnapshot) -> Result<CliEnvelopeV2> {
    CliEnvelopeV2::success(
        "inspect_topology",
        Some(serde_json::to_value(TopologyResourceV2 {
            topology_revision: snapshot.revision.get(),
        })?),
        serde_json::to_value(topology_projection(snapshot)?)?,
        false,
    )
}

/// Converts one Splint in a validated private topology snapshot to the reviewed envelope.
pub fn inspect_splint_envelope(
    snapshot: &TopologySnapshot,
    splint_id: SplintId,
) -> Result<CliEnvelopeV2> {
    snapshot
        .validate()
        .map_err(|error| anyhow::anyhow!(error.message))?;
    for lair in snapshot.topology.lairs() {
        for dojo in &lair.dojos {
            if let Some(splint) = dojo.root.find_splint(splint_id) {
                let runtime = runtime_for(snapshot, splint_id)?;
                let exit_code = runtime.exit_status.and_then(|status| status.code);
                if exit_code.is_some_and(|code| !(0..=255).contains(&code)) {
                    bail!("Splint exit code exceeds public v2 bounds");
                }
                return CliEnvelopeV2::success(
                    "inspect_splint",
                    Some(serde_json::to_value(InspectSplintResourceV2 {
                        lair_id: lair.id.to_string(),
                        dojo_id: dojo.id.to_string(),
                        splint_id: splint_id.to_string(),
                        topology_revision: snapshot.revision.get(),
                    })?),
                    serde_json::to_value(InspectSplintDataV2 {
                        title: checked_public_text(&splint.title, "Splint title")?,
                        lifecycle: public_lifecycle(runtime),
                        current_incarnation: runtime.live_incarnation,
                        last_incarnation: runtime.last_incarnation,
                        exit_code: exit_code.map(u8::try_from).transpose()?,
                        agent_status: runtime_agent_status(runtime)?,
                    })?,
                    false,
                );
            }
        }
    }
    bail!("validated topology omitted requested Splint")
}

/// Converts one Splint's explicitly reported status to the reviewed `agent_status_get` envelope.
pub fn agent_status_get_envelope(
    snapshot: &TopologySnapshot,
    splint_id: SplintId,
) -> Result<CliEnvelopeV2> {
    snapshot
        .validate()
        .map_err(|error| anyhow::anyhow!(error.message))?;
    for lair in snapshot.topology.lairs() {
        for dojo in &lair.dojos {
            if dojo.root.find_splint(splint_id).is_some() {
                let runtime = runtime_for(snapshot, splint_id)?;
                return CliEnvelopeV2::success(
                    "agent_status_get",
                    Some(serde_json::to_value(InspectSplintResourceV2 {
                        lair_id: lair.id.to_string(),
                        dojo_id: dojo.id.to_string(),
                        splint_id: splint_id.to_string(),
                        topology_revision: snapshot.revision.get(),
                    })?),
                    serde_json::to_value(AgentStatusGetDataV2 {
                        current_incarnation: runtime.live_incarnation,
                        agent_status: runtime_agent_status(runtime)?,
                    })?,
                    false,
                );
            }
        }
    }
    bail!("validated topology omitted requested Splint")
}

/// Converts an accepted explicit agent status report to the reviewed
/// `agent_status_set` or `agent_status_clear` envelope.
///
/// The reported detail is deliberately never echoed.
pub fn agent_status_report_envelope(
    splint_id: SplintId,
    incarnation: u64,
    state: Option<AgentState>,
    sequence: u64,
    expires_at_ms: Option<u64>,
) -> Result<CliEnvelopeV2> {
    if incarnation == 0 || (state.is_none() && expires_at_ms.is_some()) {
        bail!("agent status acknowledgement exceeds public v2 bounds");
    }
    let resource = serde_json::to_value(AgentStatusResourceV2 {
        splint_id: splint_id.to_string(),
        incarnation,
    })?;
    match state {
        Some(state) => CliEnvelopeV2::success(
            "agent_status_set",
            Some(resource),
            serde_json::to_value(AgentStatusSetDataV2 {
                accepted: true,
                state: state.as_str(),
                sequence,
                expires_at_ms,
            })?,
            false,
        ),
        None => CliEnvelopeV2::success(
            "agent_status_clear",
            Some(resource),
            serde_json::to_value(AgentStatusClearDataV2 {
                accepted: true,
                cleared: true,
                sequence,
            })?,
            false,
        ),
    }
}

/// Converts a validated private terminal snapshot to the reviewed one-shot projection.
pub fn terminal_snapshot_envelope(
    lair_id: LairId,
    dojo_id: DojoId,
    snapshot: &TerminalSnapshot,
) -> Result<CliEnvelopeV2> {
    snapshot
        .validate()
        .map_err(|error| anyhow::anyhow!(error.message))?;
    let column =
        usize::try_from(snapshot.cursor_column).context("terminal cursor column is negative")?;
    let row = usize::try_from(snapshot.cursor_row).context("terminal cursor row is negative")?;
    if column > snapshot.columns || row >= snapshot.rows {
        bail!("terminal cursor exceeds public v2 bounds");
    }
    CliEnvelopeV2::success(
        "terminal_snapshot",
        Some(serde_json::to_value(TerminalReadResourceV2 {
            lair_id: lair_id.to_string(),
            dojo_id: dojo_id.to_string(),
            splint_id: snapshot.splint_id.to_string(),
            incarnation: snapshot.incarnation,
            terminal_revision: snapshot.revision,
            history_generation: snapshot.history_generation,
        })?),
        serde_json::to_value(TerminalSnapshotDataV2 {
            content_encoding: "unicode_scalars",
            columns: snapshot.columns,
            rows: public_rows(&snapshot.visible_rows)?,
            cursor: TerminalCursorV2 {
                column,
                row,
                visible: snapshot.input_modes.cursor_visible,
            },
            continuation_cursor: None,
        })?,
        false,
    )
}

fn terminal_read_resource(
    lair_id: LairId,
    dojo_id: DojoId,
    splint_id: SplintId,
    incarnation: u64,
    terminal_revision: u64,
    history_generation: u64,
) -> TerminalReadResourceV2 {
    TerminalReadResourceV2 {
        lair_id: lair_id.to_string(),
        dojo_id: dojo_id.to_string(),
        splint_id: splint_id.to_string(),
        incarnation,
        terminal_revision,
        history_generation,
    }
}

/// Converts a validated private scrollback page to the reviewed public projection.
pub fn scrollback_page_envelope(
    lair_id: LairId,
    dojo_id: DojoId,
    page: &ScrollbackPage,
) -> Result<CliEnvelopeV2> {
    page.validate()
        .map_err(|error| anyhow::anyhow!(error.message))?;
    let continuation_cursor = if page.has_older {
        Some(encode_terminal_cursor(
            &TerminalContinuationV2::Scrollback {
                splint_id: page.splint_id,
                incarnation: page.incarnation,
                terminal_revision: page.terminal_revision,
                history_generation: page.history_generation,
                before_row_id: page.rows.first().and_then(|row| row.row_id).context(
                    "validated scrollback page with older rows omitted its first row identity",
                )?,
            },
        )?)
    } else {
        None
    };
    CliEnvelopeV2::success(
        "scrollback_page",
        Some(serde_json::to_value(terminal_read_resource(
            lair_id,
            dojo_id,
            page.splint_id,
            page.incarnation,
            page.terminal_revision,
            page.history_generation,
        ))?),
        serde_json::to_value(ScrollbackPageDataV2 {
            kind: "page",
            content_encoding: "unicode_scalars",
            rows: public_rows(&page.rows)?,
            has_older: page.has_older,
            continuation_cursor,
        })?,
        page.has_older,
    )
}

/// Converts a validated private search page to the reviewed public projection.
pub fn search_page_envelope(
    lair_id: LairId,
    dojo_id: DojoId,
    page: &SearchPage,
) -> Result<CliEnvelopeV2> {
    page.validate()
        .map_err(|error| anyhow::anyhow!(error.message))?;
    let continuation_cursor = page
        .next_cursor
        .as_ref()
        .map(|daemon_cursor| {
            encode_terminal_cursor(&TerminalContinuationV2::Search {
                splint_id: page.splint_id,
                incarnation: page.incarnation,
                terminal_revision: page.terminal_revision,
                history_generation: page.history_generation,
                daemon_cursor: daemon_cursor.clone(),
            })
        })
        .transpose()?;
    let matches = page
        .matches
        .iter()
        .map(|item| SearchMatchV2 {
            row_id: item.row_id,
            start_column: item.start_column,
            end_column: item.end_column,
            preview: item.preview.clone(),
        })
        .collect();
    CliEnvelopeV2::success(
        "search_scrollback",
        Some(serde_json::to_value(terminal_read_resource(
            lair_id,
            dojo_id,
            page.splint_id,
            page.incarnation,
            page.terminal_revision,
            page.history_generation,
        ))?),
        serde_json::to_value(SearchResultsDataV2 {
            kind: "results",
            matches,
            timed_out: page.timed_out,
            continuation_cursor,
        })?,
        page.next_cursor.is_some(),
    )
}

/// Creates a successful scrollback/search resynchronization envelope.
pub fn read_resync_envelope(
    operation: &'static str,
    provenance: TerminalReadProvenanceV2,
    reason: ReadResyncReasonV2,
) -> Result<CliEnvelopeV2> {
    if !matches!(operation, "scrollback_page" | "search_scrollback") {
        bail!("unsupported read resynchronization operation");
    }
    CliEnvelopeV2::success(
        operation,
        Some(serde_json::to_value(terminal_read_resource(
            provenance.lair_id,
            provenance.dojo_id,
            provenance.splint_id,
            provenance.incarnation,
            provenance.terminal_revision,
            provenance.history_generation,
        ))?),
        serde_json::to_value(ReadResyncDataV2 {
            kind: "resync_required",
            reason,
            continuation_cursor: None,
        })?,
        false,
    )
}

fn automation_scope_name(scope: AutomationScope) -> &'static str {
    match scope {
        AutomationScope::TopologyMetadataRead => "topology_metadata_read",
        AutomationScope::TopologySubscribe => "topology_subscribe",
        AutomationScope::TerminalVisibleRead => "terminal_visible_read",
        AutomationScope::TerminalSubscribe => "terminal_subscribe",
        AutomationScope::ScrollbackRead => "scrollback_read",
        AutomationScope::ScrollbackSearch => "scrollback_search",
        AutomationScope::ControllerAcquire => "controller_acquire",
        AutomationScope::ControllerTransfer => "controller_transfer",
        AutomationScope::Input => "input",
        AutomationScope::Resize => "resize",
        AutomationScope::ProcessSpawn => "process_spawn",
        AutomationScope::ProcessRestore => "process_restore",
        AutomationScope::ProcessTerminate => "process_terminate",
        AutomationScope::TopologyLayoutMutate => "topology_layout_mutate",
        AutomationScope::TopologyNameMutate => "topology_name_mutate",
        AutomationScope::AuthorizationInspect => "authorization_inspect",
        AutomationScope::AuthorizationRevoke => "authorization_revoke",
        AutomationScope::AuditInspect => "audit_inspect",
        AutomationScope::AgentStatusReport => "agent_status_report",
    }
}

fn access_scope_name(scope: AccessScope) -> Option<&'static str> {
    match scope {
        AccessScope::Observe => Some("terminal_visible_read"),
        AccessScope::Scrollback => Some("scrollback_read"),
        AccessScope::Input => Some("input"),
        AccessScope::Resize => Some("resize"),
        AccessScope::Terminate => Some("process_terminate"),
        AccessScope::ControlTakeover => Some("controller_transfer"),
        AccessScope::TopologyObserve => Some("topology_metadata_read"),
        AccessScope::TopologyLayout => Some("topology_layout_mutate"),
        AccessScope::TopologyName => Some("topology_name_mutate"),
        AccessScope::ProcessSpawn => Some("process_spawn"),
        AccessScope::ProcessRestore => Some("process_restore"),
        AccessScope::AgentStatusReport => Some("agent_status_report"),
        AccessScope::ClipboardRead | AccessScope::ClipboardWrite => None,
    }
}

fn valid_rule_id(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Converts a correlated private terminal action acknowledgement to public v2.
pub fn terminal_action_envelope(
    operation: &'static str,
    provenance: TerminalReadProvenanceV2,
    dimensions: Option<(u16, u16)>,
) -> Result<CliEnvelopeV2> {
    if provenance.incarnation == 0
        || provenance.history_generation == 0
        || !matches!(operation, "input" | "resize")
        || (operation == "input" && dimensions.is_some())
        || (operation == "resize"
            && !dimensions.is_some_and(|(columns, rows)| columns > 0 && rows > 0))
    {
        bail!("terminal action acknowledgement exceeds public v2 bounds");
    }
    let (columns, rows) =
        dimensions.map_or((None, None), |(columns, rows)| (Some(columns), Some(rows)));
    CliEnvelopeV2::success(
        operation,
        Some(serde_json::to_value(terminal_read_resource(
            provenance.lair_id,
            provenance.dojo_id,
            provenance.splint_id,
            provenance.incarnation,
            provenance.terminal_revision,
            provenance.history_generation,
        ))?),
        serde_json::to_value(TerminalActionDataV2 {
            acknowledged: true,
            columns,
            rows,
        })?,
        false,
    )
}

/// Converts private ephemeral and persistent authority to the reviewed status projection.
pub fn authorization_status_envelope(
    lair_id: LairId,
    dojo_id: DojoId,
    splint_id: SplintId,
    incarnation: u64,
    grants: &[AccessGrant],
    persistent: &[PersistentAuthorizationStatus],
    development_bypass: bool,
) -> Result<CliEnvelopeV2> {
    if incarnation == 0 || grants.len() + persistent.len() > 64 {
        bail!("authorization status exceeds public v2 bounds");
    }
    let mut projected = Vec::with_capacity(grants.len() + persistent.len());
    for grant in grants {
        if grant.splint_id != splint_id
            || grant.incarnation != incarnation
            || grant.expires_at_unix_seconds == 0
        {
            bail!("authorization grant has inconsistent identity or expiry");
        }
        let mut scopes = grant
            .scopes
            .iter()
            .copied()
            .filter_map(access_scope_name)
            .collect::<Vec<_>>();
        scopes.sort_unstable();
        scopes.dedup();
        projected.push(AuthorizationGrantV2 {
            grant_id: Some(decimal_id(grant.grant_id, "grant ID")?),
            source: "grant_once",
            policy_rule_id: None,
            scopes,
            expires_unix_seconds: Some(grant.expires_at_unix_seconds),
        });
    }
    for grant in persistent {
        if !valid_rule_id(&grant.policy_rule_id)
            || grant.expires_at_unix_seconds == Some(0)
            || grant.scopes.len() > 19
        {
            bail!("persistent authorization status exceeds public v2 bounds");
        }
        let mut scopes = grant
            .scopes
            .iter()
            .copied()
            .map(automation_scope_name)
            .collect::<Vec<_>>();
        scopes.sort_unstable();
        if scopes.windows(2).any(|pair| pair[0] == pair[1]) {
            bail!("persistent authorization status contains duplicate scopes");
        }
        projected.push(AuthorizationGrantV2 {
            grant_id: None,
            source: "persistent_policy",
            policy_rule_id: Some(grant.policy_rule_id.clone()),
            scopes,
            expires_unix_seconds: grant.expires_at_unix_seconds,
        });
    }
    CliEnvelopeV2::success(
        "authorization_status",
        Some(serde_json::to_value(AuthorizationResourceV2 {
            lair_id: lair_id.to_string(),
            dojo_id: dojo_id.to_string(),
            splint_id: splint_id.to_string(),
            incarnation,
        })?),
        serde_json::to_value(AuthorizationStatusDataV2 {
            grants: projected,
            development_bypass,
        })?,
        false,
    )
}

fn audit_decision_name(decision: AuditDecision) -> &'static str {
    match decision {
        AuditDecision::Allowed => "allowed",
        AuditDecision::Denied => "denied",
        AuditDecision::Revoked => "revoked",
        AuditDecision::Expired => "expired",
        AuditDecision::Matched => "matched",
        AuditDecision::Rejected => "rejected",
    }
}

fn audit_outcome_name(outcome: AuditOutcome) -> &'static str {
    match outcome {
        AuditOutcome::Succeeded => "succeeded",
        AuditOutcome::Failed => "failed",
        AuditOutcome::Cancelled => "cancelled",
    }
}

fn audit_record(record: &splinterm_protocol::AuditRecord) -> Result<AuditRecordV2> {
    if record.schema != "splinterm.audit.v2"
        || record.retention != "daemon_lifetime"
        || record.unix_seconds == 0
        || record.policy_generation == Some(0)
        || record
            .policy_rule_id
            .as_deref()
            .is_some_and(|value| !valid_rule_id(value))
        || record.peer.executable_path.is_empty()
        || record.peer.executable_path.chars().count() > 4096
        || record.peer.executable_sha256.len() != 64
        || !record
            .peer
            .executable_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || record.requested_scopes.len() > 19
        || record.reason.is_empty()
        || record.reason.len() > 64
        || !record.reason.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase() || byte == b'_' || index > 0 && byte.is_ascii_digit()
        })
        || record.argument_count.is_some_and(|count| count > 64)
        || record.executable_basename.as_deref().is_some_and(|value| {
            value.is_empty() || value.chars().count() > 255 || value.contains('/')
        })
    {
        bail!("audit record exceeds public v2 bounds");
    }
    let mut requested_scopes = record
        .requested_scopes
        .iter()
        .copied()
        .map(automation_scope_name)
        .collect::<Vec<_>>();
    requested_scopes.sort_unstable();
    if requested_scopes.windows(2).any(|pair| pair[0] == pair[1]) {
        bail!("audit record contains duplicate requested scopes");
    }
    let resource = record.resource.as_ref().map(|resource| AuditResourceV2 {
        lair_id: resource.lair_id.map(|id| id.to_string()),
        dojo_id: resource.dojo_id.map(|id| id.to_string()),
        splint_id: resource.splint_id.map(|id| id.to_string()),
        incarnation: resource.incarnation,
    });
    if resource.as_ref().is_some_and(|resource| {
        resource.lair_id.is_none()
            && resource.dojo_id.is_none()
            && resource.splint_id.is_none()
            && resource.incarnation.is_none()
    }) {
        bail!("audit resource must identify at least one field");
    }
    Ok(AuditRecordV2 {
        schema: "splinterm.audit.v2",
        retention: "daemon_lifetime",
        audit_id: decimal_id(record.audit_id, "audit ID")?,
        unix_seconds: record.unix_seconds,
        policy_generation: record.policy_generation,
        policy_rule_id: record.policy_rule_id.clone(),
        peer: AuditPeerV2 {
            uid: record.peer.uid,
            executable_path: record.peer.executable_path.clone(),
            executable_sha256: record.peer.executable_sha256.clone(),
            device: record.peer.device,
            inode: record.peer.inode,
        },
        operation: record.operation.as_str(),
        resource,
        requested_scopes,
        decision: audit_decision_name(record.decision),
        reason: record.reason.clone(),
        outcome: record.outcome.map(audit_outcome_name),
        argument_count: record.argument_count,
        executable_basename: record.executable_basename.clone(),
    })
}

/// Converts a private bounded audit page to the reviewed public projection.
pub fn audit_page_envelope(page: &AuditPage) -> Result<CliEnvelopeV2> {
    if page.records.len() > 128
        || page.oldest_available_audit_id == Some(0)
        || page.newest_available_audit_id == Some(0)
        || page.next_after_audit_id == Some(0)
        || page
            .records
            .windows(2)
            .any(|pair| pair[0].audit_id >= pair[1].audit_id)
    {
        bail!("audit page exceeds public v2 bounds");
    }
    let records = page
        .records
        .iter()
        .map(audit_record)
        .collect::<Result<_>>()?;
    CliEnvelopeV2::success(
        "audit_inspect",
        None,
        serde_json::to_value(AuditDataV2 {
            retention: "daemon_lifetime",
            records,
            retention_gap: page.retention_gap,
            oldest_available_audit_id: page
                .oldest_available_audit_id
                .map(|id| decimal_id(id, "oldest audit ID"))
                .transpose()?,
            newest_available_audit_id: page
                .newest_available_audit_id
                .map(|id| decimal_id(id, "newest audit ID"))
                .transpose()?,
            next_after_audit_id: page
                .next_after_audit_id
                .map(|id| decimal_id(id, "next audit ID"))
                .transpose()?,
        })?,
        page.next_after_audit_id.is_some(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MutationIdentityV2 {
    pub lair_id: Option<LairId>,
    pub dojo_id: Option<DojoId>,
    pub splint_id: Option<SplintId>,
    pub topology_revision: Option<u64>,
    pub incarnation: Option<u64>,
}

fn mutation_resource(identity: MutationIdentityV2) -> Result<MutationResourceV2> {
    if identity.topology_revision == Some(0) || identity.incarnation == Some(0) {
        bail!("mutation provenance must be nonzero");
    }
    Ok(MutationResourceV2 {
        lair_id: identity.lair_id.map(|id| id.to_string()),
        dojo_id: identity.dojo_id.map(|id| id.to_string()),
        splint_id: identity.splint_id.map(|id| id.to_string()),
        topology_revision: identity.topology_revision,
        incarnation: identity.incarnation,
    })
}

/// Creates a reviewed create/split/new-Dojo success envelope.
pub fn created_mutation_envelope(
    operation: &'static str,
    identity: MutationIdentityV2,
) -> Result<CliEnvelopeV2> {
    if !matches!(operation, "create_lair" | "split_splint" | "new_dojo")
        || identity.lair_id.is_none()
        || identity.dojo_id.is_none()
        || identity.splint_id.is_none()
        || identity.topology_revision.is_none()
        || identity.incarnation.is_none()
    {
        bail!("created mutation identity does not match its operation");
    }
    CliEnvelopeV2::success(
        operation,
        Some(serde_json::to_value(mutation_resource(identity)?)?),
        serde_json::to_value(CreatedResultV2 { created: true })?,
        false,
    )
}

/// Creates a reviewed topology-commit success envelope.
pub fn committed_mutation_envelope(
    operation: &'static str,
    identity: MutationIdentityV2,
    confirmed: bool,
) -> Result<CliEnvelopeV2> {
    let destructive = matches!(operation, "close_splint" | "close_dojo");
    let valid = match operation {
        "close_splint" | "set_split_ratio" | "rename_splint" | "set_dojo_default_focus" => {
            identity.lair_id.is_some()
                && identity.dojo_id.is_some()
                && identity.splint_id.is_some()
                && identity.topology_revision.is_some()
                && identity.incarnation.is_none()
        }
        "close_dojo" | "rename_dojo" => {
            identity.lair_id.is_some()
                && identity.dojo_id.is_some()
                && identity.splint_id.is_none()
                && identity.topology_revision.is_some()
                && identity.incarnation.is_none()
        }
        "rename_lair" => {
            identity.lair_id.is_some()
                && identity.dojo_id.is_none()
                && identity.splint_id.is_none()
                && identity.topology_revision.is_some()
                && identity.incarnation.is_none()
        }
        _ => false,
    };
    if !valid || destructive != confirmed {
        bail!("committed mutation identity or confirmation is invalid");
    }
    CliEnvelopeV2::success(
        operation,
        Some(serde_json::to_value(mutation_resource(identity)?)?),
        serde_json::to_value(MutationResultV2 {
            committed: true,
            confirmed: destructive.then_some(true),
        })?,
        false,
    )
}

/// Creates a reviewed relaunch/restore-one success envelope.
pub fn process_started_envelope(
    operation: &'static str,
    identity: MutationIdentityV2,
) -> Result<CliEnvelopeV2> {
    if !matches!(operation, "relaunch_splint" | "restore_splint")
        || identity.lair_id.is_none()
        || identity.dojo_id.is_none()
        || identity.splint_id.is_none()
        || identity.topology_revision.is_none()
        || identity.incarnation.is_none()
    {
        bail!("process start identity does not match its operation");
    }
    CliEnvelopeV2::success(
        operation,
        Some(serde_json::to_value(mutation_resource(identity)?)?),
        serde_json::to_value(ProcessStartedV2 { started: true })?,
        false,
    )
}

fn restore_leaf(result: &RestoreLeafResult) -> Result<RestoreLeafV2> {
    match (&result.error, result.incarnation) {
        (None, Some(incarnation)) if incarnation > 0 => Ok(RestoreLeafV2 {
            splint_id: result.splint_id.to_string(),
            incarnation: Some(incarnation),
            status: "restored",
            error_code: None,
        }),
        (Some(error), None) => Ok(RestoreLeafV2 {
            splint_id: result.splint_id.to_string(),
            incarnation: None,
            status: "failed",
            error_code: Some(public_error_code(error.code).0),
        }),
        _ => bail!("restore result identity and outcome are inconsistent"),
    }
}

/// Creates a reviewed restore-Dojo/restore-Lair aggregate envelope.
pub fn restore_many_envelope(
    operation: &'static str,
    identity: MutationIdentityV2,
    results: &[RestoreLeafResult],
) -> Result<CliEnvelopeV2> {
    let valid = match operation {
        "restore_dojo" => identity.lair_id.is_some() && identity.dojo_id.is_some(),
        "restore_lair" => identity.lair_id.is_some() && identity.dojo_id.is_none(),
        _ => false,
    } && identity.splint_id.is_none()
        && identity.topology_revision.is_some()
        && identity.incarnation.is_none()
        && results.len() <= 4096;
    if !valid {
        bail!("aggregate restore identity does not match its operation");
    }
    let results = results.iter().map(restore_leaf).collect::<Result<_>>()?;
    CliEnvelopeV2::success(
        operation,
        Some(serde_json::to_value(mutation_resource(identity)?)?),
        serde_json::to_value(RestoreManyV2 { results })?,
        false,
    )
}

/// Creates a reviewed confirmed process-termination envelope.
pub fn kill_envelope(
    lair_id: LairId,
    dojo_id: DojoId,
    splint_id: SplintId,
    incarnation: u64,
) -> Result<CliEnvelopeV2> {
    CliEnvelopeV2::success(
        "kill_splint",
        Some(serde_json::to_value(AuthorizationResourceV2 {
            lair_id: lair_id.to_string(),
            dojo_id: dojo_id.to_string(),
            splint_id: splint_id.to_string(),
            incarnation,
        })?),
        serde_json::to_value(KillResultV2 {
            terminated: true,
            confirmed: true,
        })?,
        false,
    )
}

/// Creates a reviewed confirmed ephemeral-grant revocation envelope.
pub fn revoke_envelope(
    lair_id: LairId,
    dojo_id: DojoId,
    grant: &AccessGrant,
) -> Result<CliEnvelopeV2> {
    if grant.incarnation == 0 || grant.expires_at_unix_seconds == 0 {
        bail!("revoked grant provenance is invalid");
    }
    CliEnvelopeV2::success(
        "revoke_access",
        Some(serde_json::to_value(AuthorizationResourceV2 {
            lair_id: lair_id.to_string(),
            dojo_id: dojo_id.to_string(),
            splint_id: grant.splint_id.to_string(),
            incarnation: grant.incarnation,
        })?),
        serde_json::to_value(RevokeResultV2 {
            revoked_grant_id: decimal_id(grant.grant_id, "grant ID")?,
            confirmed: true,
        })?,
        false,
    )
}

#[derive(Debug)]
struct DaemonProtocolFailure(splinterm_protocol::ProtocolError);

impl std::fmt::Display for DaemonProtocolFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.code == ErrorCode::DevelopmentFeatureDisabled {
            return write!(
                formatter,
                "splinterd: {} (restart with SPLINTERM_ENABLE_DEV_ATTACH=1)",
                self.0.message
            );
        }
        write!(
            formatter,
            "splinterd [{}]: {}",
            format!("{:?}", self.0.code).to_lowercase(),
            self.0.message
        )
    }
}

impl std::error::Error for DaemonProtocolFailure {}

/// The reason an in-flight daemon request was cancelled by the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestCancellation {
    /// The request exceeded its client-side deadline.
    DeadlineElapsed,
    /// The caller's cancellation token was cancelled.
    Cancelled,
}

/// Typed cancellation retained through the crate's `anyhow` boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestCancelled {
    reason: RequestCancellation,
}

impl RequestCancelled {
    /// Returns why the request was cancelled.
    #[must_use]
    pub const fn reason(self) -> RequestCancellation {
        self.reason
    }
}

impl std::fmt::Display for RequestCancelled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.reason {
            RequestCancellation::DeadlineElapsed => {
                write!(formatter, "splinterd request timed out")
            }
            RequestCancellation::Cancelled => write!(formatter, "splinterd request cancelled"),
        }
    }
}

impl std::error::Error for RequestCancelled {}

/// Extracts typed client-side request cancellation from the `anyhow` boundary.
#[must_use]
pub fn request_cancellation(error: &anyhow::Error) -> Option<RequestCancellation> {
    error
        .downcast_ref::<RequestCancelled>()
        .map(|cancelled| cancelled.reason())
}

/// The daemon ended the connection cleanly between frames: no partial frame
/// and no incomplete terminal transaction was pending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonClosedConnection;

impl std::fmt::Display for DaemonClosedConnection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("splinterd closed the connection")
    }
}

impl std::error::Error for DaemonClosedConnection {}

/// The daemon ended the connection in the middle of a frame or of a terminal
/// frame transaction, as when its process image is replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DaemonConnectionTruncated(&'static str);

impl std::fmt::Display for DaemonConnectionTruncated {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "splinterd closed {}", self.0)
    }
}

impl std::error::Error for DaemonConnectionTruncated {}

/// Whether an error is the daemon's clean end of file between frames.
#[must_use]
pub fn is_daemon_closed_connection(error: &anyhow::Error) -> bool {
    error.downcast_ref::<DaemonClosedConnection>().is_some()
}

/// Extracts a daemon protocol error retained through the anyhow boundary.
#[must_use]
pub fn protocol_error(error: &anyhow::Error) -> Option<&splinterm_protocol::ProtocolError> {
    error
        .downcast_ref::<DaemonProtocolFailure>()
        .map(|failure| &failure.0)
}

/// Wraps an embedded daemon operation error for stable public error mapping.
#[must_use]
pub fn response_protocol_error(error: splinterm_protocol::ProtocolError) -> anyhow::Error {
    anyhow::Error::new(DaemonProtocolFailure(error))
}

/// Maps every private protocol error to a stable public v2 category.
#[must_use]
pub const fn public_error_code(code: ErrorCode) -> (CliErrorCodeV2, bool) {
    match code {
        ErrorCode::AuthenticationFailed => (CliErrorCodeV2::AuthenticationFailed, false),
        ErrorCode::HandshakeRequired => (CliErrorCodeV2::HandshakeRequired, false),
        ErrorCode::IncompatibleVersion => (CliErrorCodeV2::IncompatibleVersion, false),
        ErrorCode::InvalidFrame
        | ErrorCode::FrameTooLarge
        | ErrorCode::InvalidRequestId
        | ErrorCode::DuplicateRequestId
        | ErrorCode::TooManyOutstandingRequests
        | ErrorCode::UnsupportedOperation
        | ErrorCode::DevelopmentFeatureDisabled
        | ErrorCode::RequestNotFound => (CliErrorCodeV2::InvalidRequest, false),
        ErrorCode::ConsentUnavailable => (CliErrorCodeV2::ConsentUnavailable, true),
        ErrorCode::ConsentDenied => (CliErrorCodeV2::ConsentDenied, false),
        ErrorCode::Unauthorized => (CliErrorCodeV2::Unauthorized, false),
        ErrorCode::ControllerUnavailable => (CliErrorCodeV2::ControllerUnavailable, true),
        ErrorCode::ControlTransferUnavailable => (CliErrorCodeV2::ControlTransferUnavailable, true),
        ErrorCode::StaleTopology => (CliErrorCodeV2::StaleTopology, true),
        ErrorCode::NotFound | ErrorCode::ImageContentNotFound => (CliErrorCodeV2::NotFound, false),
        ErrorCode::StaleIncarnation | ErrorCode::StaleImageContent => {
            (CliErrorCodeV2::StaleIncarnation, true)
        }
        ErrorCode::InvalidArgument => (CliErrorCodeV2::InvalidArgument, false),
        // A handoff fence lifts when the handoff completes or is cancelled.
        ErrorCode::ResourceLimit | ErrorCode::RateLimited | ErrorCode::HandoffInProgress => {
            (CliErrorCodeV2::ResourceLimit, true)
        }
        // Trusted-UI-only handoff outcomes; automation is refused before them.
        ErrorCode::HandoffUnavailable | ErrorCode::InvalidHandoffTicket => {
            (CliErrorCodeV2::InvalidRequest, false)
        }
        ErrorCode::Cancelled => (CliErrorCodeV2::Cancelled, true),
        ErrorCode::Internal => (CliErrorCodeV2::Internal, true),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PingSuccessV2 {
    status: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
enum PingEnvelopeDataV2 {
    Success {
        schema: &'static str,
        request_id: String,
        operation: &'static str,
        ok: bool,
        data: PingSuccessV2,
        truncated: bool,
    },
    Failure {
        schema: &'static str,
        request_id: String,
        operation: &'static str,
        ok: bool,
        error: CliErrorV2,
        truncated: bool,
    },
}

/// An opaque schema-conforming public v2 response for `ping`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PingEnvelopeV2(PingEnvelopeDataV2);

impl PingEnvelopeV2 {
    /// Creates a successful v1 ping envelope.
    pub fn success(request_id: u64) -> Result<Self> {
        Ok(Self(PingEnvelopeDataV2::Success {
            schema: CLI_SCHEMA_V2,
            request_id: decimal_id(request_id, "request ID")?,
            operation: "ping",
            ok: true,
            data: PingSuccessV2 { status: "awake" },
            truncated: false,
        }))
    }

    /// Creates a failed v1 ping envelope with a bounded public message.
    pub fn failure(
        request_id: u64,
        code: CliErrorCodeV2,
        message: impl Into<String>,
        retryable: bool,
    ) -> Result<Self> {
        let message = message.into();
        if message.is_empty() || message.chars().count() > 1024 {
            bail!("public error message length is outside 1..=1024 characters");
        }
        Ok(Self(PingEnvelopeDataV2::Failure {
            schema: CLI_SCHEMA_V2,
            request_id: decimal_id(request_id, "request ID")?,
            operation: "ping",
            ok: false,
            error: CliErrorV2 {
                code,
                message,
                retryable,
                current_topology_revision: None,
                current_terminal_revision: None,
            },
            truncated: false,
        }))
    }
}

/// Writes one compact JSON document plus a newline and flushes it.
pub fn write_json_document(value: &impl Serialize) -> Result<()> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    serde_json::to_writer(&mut output, value).context("failed to encode machine output")?;
    output
        .write_all(b"\n")
        .context("failed to write machine output")?;
    output.flush().context("failed to flush machine output")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TerminalResourceV2 {
    splint_id: String,
    incarnation: u64,
    terminal_revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    history_generation: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TopologyResourceV2 {
    topology_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ControlResourceV2 {
    splint_id: String,
    incarnation: u64,
}

/// One bounded semantic cell in a public terminal projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectedTerminalCell {
    pub text: String,
    pub width: u8,
}

/// One bounded semantic row in a public terminal projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectedTerminalRow {
    pub linebreak: bool,
    pub cells: Vec<ProjectedTerminalCell>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TerminalSnapshotV2 {
    content_encoding: &'static str,
    columns: usize,
    rows: usize,
    title: String,
    visible_rows: Vec<ProjectedTerminalRow>,
}

impl TerminalSnapshotV2 {
    fn try_from_protocol(snapshot: &TerminalSnapshot) -> Result<Self> {
        snapshot
            .validate()
            .map_err(|error| anyhow::anyhow!(error.message))?;
        if snapshot.title.chars().count() > 1_024 {
            bail!("terminal title exceeds public event bounds");
        }
        let visible_rows = project_terminal_rows(&snapshot.visible_rows)?;
        Ok(Self {
            content_encoding: "unicode_scalars",
            columns: snapshot.columns,
            rows: snapshot.rows,
            title: snapshot.title.clone(),
            visible_rows,
        })
    }
}

#[allow(
    clippy::struct_field_names,
    reason = "field names are fixed by the reviewed public v2 JSON schema"
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TopologySnapshotV2 {
    lair_count: usize,
    dojo_count: usize,
    splint_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ControlSnapshotV2 {
    controlled: bool,
    locally_owned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TerminalUpdateV2 {
    content_encoding: &'static str,
    changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TopologyChangeV2 {
    kind: &'static str,
    lair_count: usize,
    dojo_count: usize,
    splint_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TransferRequestedV2 {
    transfer_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct TransferResolvedV2 {
    transfer_id: String,
    outcome: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AccessRevokedV2 {
    grant_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ExitedV2 {
    code: Option<i32>,
    signal: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResyncReasonV2 {
    SubscriberStalled,
    RevisionGap,
    HistoryReplaced,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ResyncV2 {
    reason: ResyncReasonV2,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct EmptyDataV2 {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
enum CliEventDataV2 {
    TerminalSnapshot {
        schema: &'static str,
        subscription_id: String,
        sequence: u64,
        event_type: &'static str,
        resource: TerminalResourceV2,
        data: TerminalSnapshotV2,
        truncated: bool,
    },
    TopologySnapshot {
        schema: &'static str,
        subscription_id: String,
        sequence: u64,
        event_type: &'static str,
        resource: TopologyResourceV2,
        data: TopologySnapshotV2,
        truncated: bool,
    },
    ControlSnapshot {
        schema: &'static str,
        subscription_id: String,
        sequence: u64,
        event_type: &'static str,
        resource: ControlResourceV2,
        data: ControlSnapshotV2,
        truncated: bool,
    },
    TerminalUpdate {
        schema: &'static str,
        subscription_id: String,
        sequence: u64,
        event_type: &'static str,
        resource: TerminalResourceV2,
        data: TerminalUpdateV2,
        truncated: bool,
    },
    TopologyChanged {
        schema: &'static str,
        subscription_id: String,
        sequence: u64,
        event_type: &'static str,
        resource: TopologyResourceV2,
        data: TopologyChangeV2,
        truncated: bool,
    },
    ControlStatusChanged {
        schema: &'static str,
        subscription_id: String,
        sequence: u64,
        event_type: &'static str,
        resource: ControlResourceV2,
        data: ControlSnapshotV2,
        truncated: bool,
    },
    ControlTransferRequested {
        schema: &'static str,
        subscription_id: String,
        sequence: u64,
        event_type: &'static str,
        resource: ControlResourceV2,
        data: TransferRequestedV2,
        truncated: bool,
    },
    ControlTransferResolved {
        schema: &'static str,
        subscription_id: String,
        sequence: u64,
        event_type: &'static str,
        resource: ControlResourceV2,
        data: TransferResolvedV2,
        truncated: bool,
    },
    AccessRevoked {
        schema: &'static str,
        subscription_id: String,
        sequence: u64,
        event_type: &'static str,
        resource: ControlResourceV2,
        data: AccessRevokedV2,
        truncated: bool,
    },
    Exited {
        schema: &'static str,
        subscription_id: String,
        sequence: u64,
        event_type: &'static str,
        resource: ControlResourceV2,
        data: ExitedV2,
        truncated: bool,
    },
    TerminalResync {
        schema: &'static str,
        subscription_id: String,
        sequence: u64,
        event_type: &'static str,
        stream: &'static str,
        resource: TerminalResourceV2,
        data: EmptyDataV2,
        truncated: bool,
        resync: ResyncV2,
    },
    TopologyResync {
        schema: &'static str,
        subscription_id: String,
        sequence: u64,
        event_type: &'static str,
        stream: &'static str,
        resource: TopologyResourceV2,
        data: EmptyDataV2,
        truncated: bool,
        resync: ResyncV2,
    },
    ControlResync {
        schema: &'static str,
        subscription_id: String,
        sequence: u64,
        event_type: &'static str,
        stream: &'static str,
        resource: ControlResourceV2,
        data: EmptyDataV2,
        truncated: bool,
        resync: ResyncV2,
    },
}

/// Opaque public v2 initial-state and resynchronization event record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CliEventV2(CliEventDataV2);

impl CliEventV2 {
    fn common(subscription_id: u64, sequence: u64) -> Result<(String, u64)> {
        if sequence == 0 {
            bail!("event sequence must be nonzero");
        }
        Ok((decimal_id(subscription_id, "subscription ID")?, sequence))
    }

    /// Converts a private terminal snapshot into an explicit public initial event.
    pub fn terminal_snapshot(
        subscription_id: u64,
        sequence: u64,
        snapshot: &TerminalSnapshot,
        truncated: bool,
    ) -> Result<Self> {
        let (subscription_id, sequence) = Self::common(subscription_id, sequence)?;
        Ok(Self(CliEventDataV2::TerminalSnapshot {
            schema: CLI_EVENT_SCHEMA_V2,
            subscription_id,
            sequence,
            event_type: "snapshot",
            resource: TerminalResourceV2 {
                splint_id: snapshot.splint_id.to_string(),
                incarnation: snapshot.incarnation,
                terminal_revision: snapshot.revision,
                history_generation: None,
            },
            data: TerminalSnapshotV2::try_from_protocol(snapshot)?,
            truncated,
        }))
    }

    /// Converts a private topology snapshot into a public count-only initial event.
    pub fn topology_snapshot(
        subscription_id: u64,
        sequence: u64,
        snapshot: &TopologySnapshot,
    ) -> Result<Self> {
        snapshot
            .validate()
            .map_err(|error| anyhow::anyhow!(error.message))?;
        let (subscription_id, sequence) = Self::common(subscription_id, sequence)?;
        let lair_count = snapshot.topology.lairs().len();
        let dojo_count = snapshot.topology.lairs().map(|lair| lair.dojos.len()).sum();
        let splint_count = snapshot
            .topology
            .lairs()
            .flat_map(|dojo| &dojo.dojos)
            .map(|dojo| dojo.root.splint_count())
            .sum();
        Ok(Self(CliEventDataV2::TopologySnapshot {
            schema: CLI_EVENT_SCHEMA_V2,
            subscription_id,
            sequence,
            event_type: "topology_snapshot",
            resource: TopologyResourceV2 {
                topology_revision: snapshot.revision.get(),
            },
            data: TopologySnapshotV2 {
                lair_count,
                dojo_count,
                splint_count,
            },
            truncated: false,
        }))
    }

    /// Converts a private controller status into a public initial event.
    pub fn control_snapshot(
        subscription_id: u64,
        sequence: u64,
        status: ControlStatus,
    ) -> Result<Self> {
        status
            .validate()
            .map_err(|error| anyhow::anyhow!(error.message))?;
        let (subscription_id, sequence) = Self::common(subscription_id, sequence)?;
        Ok(Self(CliEventDataV2::ControlSnapshot {
            schema: CLI_EVENT_SCHEMA_V2,
            subscription_id,
            sequence,
            event_type: "control_snapshot",
            resource: ControlResourceV2 {
                splint_id: status.splint_id.to_string(),
                incarnation: status.incarnation,
            },
            data: ControlSnapshotV2 {
                controlled: status.controlled,
                locally_owned: status.locally_owned,
            },
            truncated: false,
        }))
    }

    /// Emits a bounded terminal revision update without private bytes.
    pub fn terminal_update(
        subscription_id: u64,
        sequence: u64,
        splint_id: SplintId,
        incarnation: u64,
        terminal_revision: u64,
        history_generation: u64,
    ) -> Result<Self> {
        if incarnation == 0 || history_generation == 0 {
            bail!("terminal update incarnation and history generation must be nonzero");
        }
        let (subscription_id, sequence) = Self::common(subscription_id, sequence)?;
        Ok(Self(CliEventDataV2::TerminalUpdate {
            schema: CLI_EVENT_SCHEMA_V2,
            subscription_id,
            sequence,
            event_type: "update",
            resource: TerminalResourceV2 {
                splint_id: splint_id.to_string(),
                incarnation,
                terminal_revision,
                history_generation: Some(history_generation),
            },
            data: TerminalUpdateV2 {
                content_encoding: "unicode_scalars",
                changed: true,
            },
            truncated: false,
        }))
    }

    /// Emits reviewed topology counts for one topology change.
    pub fn topology_changed(
        subscription_id: u64,
        sequence: u64,
        kind: TopologyChangeKind,
        snapshot: &TopologySnapshot,
    ) -> Result<Self> {
        snapshot
            .validate()
            .map_err(|error| anyhow::anyhow!(error.message))?;
        let (subscription_id, sequence) = Self::common(subscription_id, sequence)?;
        let lair_count = snapshot.topology.lairs().len();
        let dojo_count = snapshot.topology.lairs().map(|lair| lair.dojos.len()).sum();
        let splint_count = snapshot
            .topology
            .lairs()
            .flat_map(|dojo| &dojo.dojos)
            .map(|dojo| dojo.root.splint_count())
            .sum();
        let kind = match kind {
            TopologyChangeKind::LairCreated => "lair_created",
            TopologyChangeKind::SplintSplit => "splint_split",
            TopologyChangeKind::SplintClosed => "splint_closed",
            TopologyChangeKind::SplitRatioChanged => "split_ratio_changed",
            TopologyChangeKind::DojoCreated => "dojo_created",
            TopologyChangeKind::PresetMaterialized => "preset_materialized",
            TopologyChangeKind::DojoClosed => "dojo_closed",
            TopologyChangeKind::LairTerminated => "lair_terminated",
            TopologyChangeKind::LairRenamed => "lair_renamed",
            TopologyChangeKind::LairRetentionChanged => "lair_retention_changed",
            TopologyChangeKind::DojoRenamed => "dojo_renamed",
            TopologyChangeKind::DojoDefaultFocusChanged => "dojo_default_focus_changed",
            TopologyChangeKind::SplintRenamed => "splint_renamed",
            TopologyChangeKind::RuntimeChanged => "runtime_changed",
        };
        Ok(Self(CliEventDataV2::TopologyChanged {
            schema: CLI_EVENT_SCHEMA_V2,
            subscription_id,
            sequence,
            event_type: "topology_changed",
            resource: TopologyResourceV2 {
                topology_revision: snapshot.revision.get(),
            },
            data: TopologyChangeV2 {
                kind,
                lair_count,
                dojo_count,
                splint_count,
            },
            truncated: false,
        }))
    }

    /// Emits one reviewed controller-status change.
    pub fn control_status_changed(
        subscription_id: u64,
        sequence: u64,
        status: ControlStatus,
    ) -> Result<Self> {
        status
            .validate()
            .map_err(|error| anyhow::anyhow!(error.message))?;
        let (subscription_id, sequence) = Self::common(subscription_id, sequence)?;
        Ok(Self(CliEventDataV2::ControlStatusChanged {
            schema: CLI_EVENT_SCHEMA_V2,
            subscription_id,
            sequence,
            event_type: "control_status_changed",
            resource: ControlResourceV2 {
                splint_id: status.splint_id.to_string(),
                incarnation: status.incarnation,
            },
            data: ControlSnapshotV2 {
                controlled: status.controlled,
                locally_owned: status.locally_owned,
            },
            truncated: false,
        }))
    }

    /// Emits a controller-transfer request without exposing controller IDs.
    pub fn control_transfer_requested(
        subscription_id: u64,
        sequence: u64,
        splint_id: SplintId,
        incarnation: u64,
        transfer_id: u64,
    ) -> Result<Self> {
        let (subscription_id, sequence) = Self::common(subscription_id, sequence)?;
        Ok(Self(CliEventDataV2::ControlTransferRequested {
            schema: CLI_EVENT_SCHEMA_V2,
            subscription_id,
            sequence,
            event_type: "control_transfer_requested",
            resource: ControlResourceV2 {
                splint_id: splint_id.to_string(),
                incarnation,
            },
            data: TransferRequestedV2 {
                transfer_id: decimal_id(transfer_id, "transfer ID")?,
            },
            truncated: false,
        }))
    }

    /// Emits a controller-transfer outcome without exposing controller IDs.
    pub fn control_transfer_resolved(
        subscription_id: u64,
        sequence: u64,
        splint_id: SplintId,
        incarnation: u64,
        transfer_id: u64,
        outcome: ControlTransferOutcome,
    ) -> Result<Self> {
        let (subscription_id, sequence) = Self::common(subscription_id, sequence)?;
        let outcome = match outcome {
            ControlTransferOutcome::Granted => "granted",
            ControlTransferOutcome::Denied => "denied",
            ControlTransferOutcome::TimedOut => "timed_out",
            ControlTransferOutcome::Cancelled => "cancelled",
        };
        Ok(Self(CliEventDataV2::ControlTransferResolved {
            schema: CLI_EVENT_SCHEMA_V2,
            subscription_id,
            sequence,
            event_type: "control_transfer_resolved",
            resource: ControlResourceV2 {
                splint_id: splint_id.to_string(),
                incarnation,
            },
            data: TransferResolvedV2 {
                transfer_id: decimal_id(transfer_id, "transfer ID")?,
                outcome,
            },
            truncated: false,
        }))
    }

    /// Emits grant revocation without private capability material.
    pub fn access_revoked(
        subscription_id: u64,
        sequence: u64,
        splint_id: SplintId,
        incarnation: u64,
        grant_id: u64,
    ) -> Result<Self> {
        let (subscription_id, sequence) = Self::common(subscription_id, sequence)?;
        Ok(Self(CliEventDataV2::AccessRevoked {
            schema: CLI_EVENT_SCHEMA_V2,
            subscription_id,
            sequence,
            event_type: "access_revoked",
            resource: ControlResourceV2 {
                splint_id: splint_id.to_string(),
                incarnation,
            },
            data: AccessRevokedV2 {
                grant_id: decimal_id(grant_id, "grant ID")?,
            },
            truncated: false,
        }))
    }

    /// Emits a terminal process exit status.
    pub fn exited(
        subscription_id: u64,
        sequence: u64,
        splint_id: SplintId,
        incarnation: u64,
        code: Option<i32>,
        signal: Option<i32>,
    ) -> Result<Self> {
        let (subscription_id, sequence) = Self::common(subscription_id, sequence)?;
        Ok(Self(CliEventDataV2::Exited {
            schema: CLI_EVENT_SCHEMA_V2,
            subscription_id,
            sequence,
            event_type: "exited",
            resource: ControlResourceV2 {
                splint_id: splint_id.to_string(),
                incarnation,
            },
            data: ExitedV2 { code, signal },
            truncated: false,
        }))
    }

    /// Creates a terminal-stream resynchronization record.
    pub fn terminal_resync(
        subscription_id: u64,
        sequence: u64,
        splint_id: SplintId,
        incarnation: u64,
        terminal_revision: u64,
        history_generation: Option<u64>,
        reason: ResyncReasonV2,
    ) -> Result<Self> {
        if incarnation == 0 {
            bail!("terminal resync incarnation must be nonzero");
        }
        if reason == ResyncReasonV2::HistoryReplaced && history_generation.is_none() {
            bail!("history replacement requires a history generation");
        }
        if history_generation == Some(0) {
            bail!("history generation must be nonzero");
        }
        let (subscription_id, sequence) = Self::common(subscription_id, sequence)?;
        Ok(Self(CliEventDataV2::TerminalResync {
            schema: CLI_EVENT_SCHEMA_V2,
            subscription_id,
            sequence,
            event_type: "resync_required",
            stream: "terminal",
            resource: TerminalResourceV2 {
                splint_id: splint_id.to_string(),
                incarnation,
                terminal_revision,
                history_generation,
            },
            data: EmptyDataV2 {},
            truncated: false,
            resync: ResyncV2 { reason },
        }))
    }

    /// Creates a topology-stream resynchronization record.
    pub fn topology_resync(
        subscription_id: u64,
        sequence: u64,
        revision: TopologyRevision,
        reason: ResyncReasonV2,
    ) -> Result<Self> {
        if reason == ResyncReasonV2::HistoryReplaced {
            bail!("topology resync cannot report replaced terminal history");
        }
        let (subscription_id, sequence) = Self::common(subscription_id, sequence)?;
        Ok(Self(CliEventDataV2::TopologyResync {
            schema: CLI_EVENT_SCHEMA_V2,
            subscription_id,
            sequence,
            event_type: "resync_required",
            stream: "topology",
            resource: TopologyResourceV2 {
                topology_revision: revision.get(),
            },
            data: EmptyDataV2 {},
            truncated: false,
            resync: ResyncV2 { reason },
        }))
    }

    /// Creates a control-stream resynchronization record.
    pub fn control_resync(
        subscription_id: u64,
        sequence: u64,
        splint_id: SplintId,
        incarnation: u64,
        reason: ResyncReasonV2,
    ) -> Result<Self> {
        if incarnation == 0 || reason == ResyncReasonV2::HistoryReplaced {
            bail!("invalid control resync provenance or reason");
        }
        let (subscription_id, sequence) = Self::common(subscription_id, sequence)?;
        Ok(Self(CliEventDataV2::ControlResync {
            schema: CLI_EVENT_SCHEMA_V2,
            subscription_id,
            sequence,
            event_type: "resync_required",
            stream: "control",
            resource: ControlResourceV2 {
                splint_id: splint_id.to_string(),
                incarnation,
            },
            data: EmptyDataV2 {},
            truncated: false,
            resync: ResyncV2 { reason },
        }))
    }
}

const IMAGE_CONTENT_HEADER_BYTES: usize = 53;
const IMAGE_MEMFD_HEADER_BYTES: usize = 45;

async fn receive_image_content(
    stream: &mut UnixStream,
    metadata: &ImageContentMetadata,
) -> Result<Vec<u8>> {
    let mut header = [0_u8; IMAGE_CONTENT_HEADER_BYTES];
    stream.read_exact(&mut header).await?;
    if &header[0..5] != b"SPIM\x01"
        || header[13..45] != metadata.digest
        || usize::try_from(u64::from_be_bytes(header[5..13].try_into().unwrap())).ok()
            != Some(metadata.byte_length)
        || usize::try_from(u32::from_be_bytes(header[45..49].try_into().unwrap())).ok()
            != Some(MAX_IMAGE_CHUNK_BYTES)
        || usize::try_from(u32::from_be_bytes(header[49..53].try_into().unwrap())).ok()
            != Some(MAX_IMAGE_CHUNK_WINDOW)
    {
        bail!("image content header does not match negotiated metadata");
    }
    let mut pixels = Vec::with_capacity(metadata.byte_length);
    let mut chunks_in_window = 0_usize;
    while pixels.len() < metadata.byte_length {
        let mut chunk_header = [0_u8; 12];
        stream.read_exact(&mut chunk_header).await?;
        let offset = usize::try_from(u64::from_be_bytes(chunk_header[0..8].try_into().unwrap()))
            .context("image chunk offset exceeds usize")?;
        let length = usize::try_from(u32::from_be_bytes(chunk_header[8..12].try_into().unwrap()))
            .expect("u32 image chunk length fits usize");
        if offset != pixels.len()
            || length == 0
            || length > MAX_IMAGE_CHUNK_BYTES
            || offset
                .checked_add(length)
                .is_none_or(|end| end > metadata.byte_length)
        {
            bail!("image content chunk is out of window or exceeds bounds");
        }
        let start = pixels.len();
        pixels.resize(start + length, 0);
        stream.read_exact(&mut pixels[start..]).await?;
        chunks_in_window += 1;
        if chunks_in_window == MAX_IMAGE_CHUNK_WINDOW || pixels.len() == metadata.byte_length {
            let mut acknowledgement = [0_u8; 9];
            acknowledgement[0] = 1;
            acknowledgement[1..9].copy_from_slice(
                &u64::try_from(pixels.len())
                    .context("image acknowledgement offset exceeds u64")?
                    .to_be_bytes(),
            );
            stream.write_all(&acknowledgement).await?;
            chunks_in_window = 0;
        }
    }
    if Sha256::digest(&pixels).as_slice() != metadata.digest {
        bail!("image content digest does not match metadata");
    }
    Ok(pixels)
}

async fn receive_image_memfd(
    stream: &mut UnixStream,
    metadata: &ImageContentMetadata,
) -> Result<ReadOnlyFileMap> {
    let descriptor = loop {
        stream.readable().await?;
        let mut marker = [0_u8; 1];
        let mut iov = [IoSliceMut::new(&mut marker)];
        let mut ancillary_space =
            [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut ancillary = RecvAncillaryBuffer::new(&mut ancillary_space);
        let message = match recvmsg(
            stream.as_fd(),
            &mut iov,
            &mut ancillary,
            RecvFlags::CMSG_CLOEXEC,
        ) {
            Ok(message) => message,
            Err(error) if error == rustix::io::Errno::WOULDBLOCK => continue,
            Err(error) => return Err(error.into()),
        };
        if message.bytes != 1 || marker != *b"F" || message.flags.contains(ReturnFlags::CTRUNC) {
            bail!("image descriptor message is malformed or truncated");
        }
        let mut descriptor: Option<OwnedFd> = None;
        for item in ancillary.drain() {
            if let RecvAncillaryMessage::ScmRights(mut descriptors) = item {
                if descriptor.is_some() {
                    bail!("image descriptor message contains multiple rights records");
                }
                descriptor = descriptors.next();
                if descriptors.next().is_some() {
                    bail!("image descriptor message contains multiple descriptors");
                }
            }
        }
        break descriptor.context("image descriptor message contains no descriptor")?;
    };
    let mut header = [0_u8; IMAGE_MEMFD_HEADER_BYTES];
    stream.read_exact(&mut header).await?;
    if &header[0..5] != b"SPIF\x01"
        || header[13..45] != metadata.digest
        || usize::try_from(u64::from_be_bytes(header[5..13].try_into().unwrap())).ok()
            != Some(metadata.byte_length)
    {
        bail!("image descriptor header does not match negotiated metadata");
    }
    let mapping = ReadOnlyFileMap::from_sealed_fd(descriptor, metadata.byte_length)
        .context("image descriptor is not exactly sized and immutable")?;
    if Sha256::digest(&*mapping).as_slice() != metadata.digest {
        bail!("mapped image content digest does not match metadata");
    }
    stream.write_all(&[1]).await?;
    Ok(mapping)
}

pub const MAX_IMAGE_SOURCE_CACHE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_RENDERER_IMAGE_RESIDENT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ImageCacheKey {
    content_id: u64,
    generation: u64,
    digest: [u8; 32],
}

impl From<&ImageContentMetadata> for ImageCacheKey {
    fn from(metadata: &ImageContentMetadata) -> Self {
        Self {
            content_id: metadata.content_id,
            generation: metadata.generation,
            digest: metadata.digest,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ImageContentCacheMetrics {
    pub bytes: usize,
    pub entries: usize,
    pub high_water_bytes: usize,
    pub high_water_entries: usize,
}

#[derive(Clone, Debug)]
pub enum ImageContentSource {
    Buffered(Arc<[u8]>),
    Mapped(Arc<ReadOnlyFileMap>),
}

impl ImageContentSource {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Buffered(bytes) => bytes,
            Self::Mapped(mapping) => mapping,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.as_bytes().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.as_bytes().is_empty()
    }

    fn is_externally_leased(&self) -> bool {
        match self {
            Self::Buffered(bytes) => Arc::strong_count(bytes) > 1,
            Self::Mapped(mapping) => Arc::strong_count(mapping) > 1,
        }
    }
}

#[derive(Debug)]
pub struct ImageContentCache {
    entries: HashMap<ImageCacheKey, ImageContentSource>,
    order: VecDeque<ImageCacheKey>,
    maximum_bytes: usize,
    metrics: ImageContentCacheMetrics,
}

impl Default for ImageContentCache {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            maximum_bytes: MAX_IMAGE_SOURCE_CACHE_BYTES,
            metrics: ImageContentCacheMetrics::default(),
        }
    }
}

impl ImageContentCache {
    pub fn with_maximum_bytes(maximum_bytes: usize) -> Result<Self> {
        if maximum_bytes == 0 || maximum_bytes > MAX_RENDERER_IMAGE_RESIDENT_BYTES {
            bail!("image source cache byte limit is invalid");
        }
        Ok(Self {
            maximum_bytes,
            ..Self::default()
        })
    }

    #[must_use]
    pub fn contains(&self, metadata: &ImageContentMetadata) -> bool {
        self.entries.contains_key(&ImageCacheKey::from(metadata))
    }

    pub fn insert(
        &mut self,
        metadata: &ImageContentMetadata,
        pixels: Vec<u8>,
    ) -> Result<ImageContentSource> {
        self.insert_source(metadata, ImageContentSource::Buffered(pixels.into()))
    }

    pub fn insert_source(
        &mut self,
        metadata: &ImageContentMetadata,
        source: ImageContentSource,
    ) -> Result<ImageContentSource> {
        if source.len() != metadata.byte_length
            || source.len() > self.maximum_bytes
            || Sha256::digest(source.as_bytes())[..] != metadata.digest
        {
            bail!("image content does not match cache metadata");
        }
        let key = ImageCacheKey::from(metadata);
        if let Some(existing) = self.entries.get(&key) {
            return Ok(existing.clone());
        }
        while self
            .metrics
            .bytes
            .checked_add(source.len())
            .is_none_or(|bytes| bytes > self.maximum_bytes)
        {
            let position = self
                .order
                .iter()
                .position(|key| {
                    self.entries
                        .get(key)
                        .is_some_and(|source| !source.is_externally_leased())
                })
                .context("image source cache is full of renderer-leased entries")?;
            let oldest = self
                .order
                .remove(position)
                .context("image cache eviction position disappeared")?;
            let removed = self
                .entries
                .remove(&oldest)
                .context("image cache order references a missing entry")?;
            self.metrics.bytes -= removed.len();
            self.metrics.entries = self.entries.len();
        }
        self.metrics.bytes += source.len();
        self.entries.insert(key, source.clone());
        self.order.push_back(key);
        self.metrics.entries = self.entries.len();
        self.metrics.high_water_bytes = self.metrics.high_water_bytes.max(self.metrics.bytes);
        self.metrics.high_water_entries = self.metrics.high_water_entries.max(self.metrics.entries);
        Ok(source)
    }

    #[must_use]
    pub fn get(&self, metadata: &ImageContentMetadata) -> Option<ImageContentSource> {
        self.entries.get(&ImageCacheKey::from(metadata)).cloned()
    }

    #[must_use]
    pub const fn metrics(&self) -> ImageContentCacheMetrics {
        self.metrics
    }
}

#[derive(Clone, Debug, Default)]
pub struct ImageContentLeaseSet {
    entries: HashMap<ImageCacheKey, ImageContentSource>,
    bytes: usize,
}

impl ImageContentLeaseSet {
    #[must_use]
    pub fn get(&self, metadata: &ImageContentMetadata) -> Option<ImageContentSource> {
        self.entries.get(&ImageCacheKey::from(metadata)).cloned()
    }

    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Clone, Debug, Default)]
pub struct SharedImageContentCache(Arc<Mutex<ImageContentCache>>);

impl SharedImageContentCache {
    pub fn with_maximum_bytes(maximum_bytes: usize) -> Result<Self> {
        Ok(Self(Arc::new(Mutex::new(
            ImageContentCache::with_maximum_bytes(maximum_bytes)?,
        ))))
    }

    pub fn contains(&self, metadata: &ImageContentMetadata) -> Result<bool> {
        Ok(self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("image source cache lock is poisoned"))?
            .contains(metadata))
    }

    pub fn insert_source(
        &self,
        metadata: &ImageContentMetadata,
        source: ImageContentSource,
    ) -> Result<ImageContentSource> {
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("image source cache lock is poisoned"))?
            .insert_source(metadata, source)
    }

    pub fn get(&self, metadata: &ImageContentMetadata) -> Result<Option<ImageContentSource>> {
        Ok(self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("image source cache lock is poisoned"))?
            .get(metadata))
    }

    pub fn lease(&self, contents: &[ImageContentMetadata]) -> Result<ImageContentLeaseSet> {
        let cache = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("image source cache lock is poisoned"))?;
        let mut entries = HashMap::with_capacity(contents.len());
        let mut bytes = 0_usize;
        for metadata in contents {
            let source = cache
                .get(metadata)
                .context("snapshot references an unresolved image source")?;
            bytes = bytes
                .checked_add(source.len())
                .filter(|bytes| *bytes <= cache.maximum_bytes)
                .context("leased image sources exceed the renderer byte limit")?;
            entries.insert(ImageCacheKey::from(metadata), source);
        }
        Ok(ImageContentLeaseSet { entries, bytes })
    }

    pub fn metrics(&self) -> Result<ImageContentCacheMetrics> {
        Ok(self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("image source cache lock is poisoned"))?
            .metrics())
    }
}

/// A negotiated connection to the private local daemon protocol.
///
/// This type is an internal implementation boundary. Public automation
/// compatibility is defined only by the checked-in JSON/NDJSON schemas.
type BoxedReader = Box<dyn AsyncRead + Unpin + Send>;
type BoxedWriter = Box<dyn AsyncWrite + Unpin + Send>;

pub struct Connection {
    reader: Option<BoxedReader>,
    writer: Option<BoxedWriter>,
    next_request: u64,
    diagnostic_correlation: Option<splinterm_protocol::DiagnosticCorrelation>,
    read_buffer: Vec<u8>,
    read_offset: usize,
    read_scratch: Box<[u8; READ_CHUNK_BYTES]>,
    server_frame_assembler: ServerFrameTransactionAssembler,
    queued_events: VecDeque<(ServerFrame, usize)>,
    queued_event_bytes: usize,
    limits: ServerLimits,
    daemon_hostname: Option<String>,
    daemon_generation: u64,
    socket_path: Option<PathBuf>,
    daemon_pid: Option<u32>,
    trusted_ui: bool,
    unusable: bool,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Connection")
            .field(
                "connected",
                &(self.reader.is_some() && self.writer.is_some()),
            )
            .field("next_request", &self.next_request)
            .field("limits", &self.limits)
            .field("socket_path", &self.socket_path)
            .field("trusted_ui", &self.trusted_ui)
            .field("unusable", &self.unusable)
            .finish_non_exhaustive()
    }
}

/// Drop guard ensuring an abandoned request cannot leave a correlated response
/// or connection-owned temporary subscription behind.
struct InFlightRequest<'a> {
    connection: &'a mut Connection,
    request_id: u64,
    armed: bool,
}

impl InFlightRequest<'_> {
    async fn perform(&mut self, request: Request) -> Result<Response> {
        self.connection
            .perform_request(self.request_id, request)
            .await
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for InFlightRequest<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.connection.cancel_and_close(self.request_id);
        }
    }
}

impl Connection {
    /// Connects as the graphical client and negotiates the current private protocol.
    pub async fn connect() -> Result<Self> {
        Self::connect_role(ClientRole::TrustedUi).await
    }

    /// Negotiates a trusted-UI connection over an already connected local
    /// daemon socket, so the caller can inspect the listener's credentials
    /// (`SO_PEERCRED`) before, and regardless of, the handshake.
    pub async fn connect_trusted_ui_stream(stream: UnixStream) -> Result<Self> {
        Self::connect_stream_at(stream, ClientRole::TrustedUi, socket_path().ok()).await
    }

    /// Connects without the trusted graphical-client authorization bypass.
    pub async fn connect_automation() -> Result<Self> {
        Self::connect_role(ClientRole::Automation).await
    }

    /// Negotiates a human-interactive connection carried by the graphical SSH relay.
    ///
    /// This role receives normal terminal authority from the remote daemon but
    /// never gains local compositor-focus or image-content privileges.
    pub async fn connect_remote_interactive_transport<R, W>(reader: R, writer: W) -> Result<Self>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::connect_transport(
            Box::new(reader),
            Box::new(writer),
            ClientRole::RemoteInteractive,
            None,
        )
        .await
    }

    /// Connects as automation to one explicit socket for isolated integration tests.
    #[doc(hidden)]
    pub async fn connect_automation_at(socket: &Path) -> Result<Self> {
        let stream = UnixStream::connect(socket)
            .await
            .with_context(|| format!("cannot connect to splinterd at {}", socket.display()))?;
        Self::connect_stream_at(stream, ClientRole::Automation, Some(socket.to_owned())).await
    }

    /// Negotiates an automation-role daemon connection over split async transports.
    ///
    /// The transport does not gain trusted image-content access and is closed on
    /// cancellation or any framing/protocol failure.
    pub async fn connect_automation_transport<R, W>(reader: R, writer: W) -> Result<Self>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::connect_transport(
            Box::new(reader),
            Box::new(writer),
            ClientRole::Automation,
            None,
        )
        .await
    }

    async fn connect_role(role: ClientRole) -> Result<Self> {
        let socket = socket_path()?;
        let stream = UnixStream::connect(&socket)
            .await
            .with_context(|| format!("cannot connect to splinterd at {}", socket.display()))?;
        Self::connect_stream_at(stream, role, Some(socket)).await
    }

    #[cfg(test)]
    async fn connect_stream(stream: UnixStream, role: ClientRole) -> Result<Self> {
        Self::connect_stream_at(stream, role, None).await
    }

    async fn connect_stream_at(
        stream: UnixStream,
        role: ClientRole,
        socket_path: Option<PathBuf>,
    ) -> Result<Self> {
        // The listening process, as `SO_PEERCRED` reports it. A daemon keeps
        // its PID across an in-place handoff or rollback; a restart does not.
        // Zero means the PID is not visible from this PID namespace.
        let daemon_pid = stream
            .peer_cred()
            .ok()
            .and_then(|credentials| credentials.pid())
            .and_then(|pid| u32::try_from(pid).ok())
            .filter(|&pid| pid > 0);
        let (reader, writer) = stream.into_split();
        let mut connection =
            Self::connect_transport(Box::new(reader), Box::new(writer), role, socket_path).await?;
        connection.daemon_pid = daemon_pid;
        Ok(connection)
    }

    async fn connect_transport(
        mut reader: BoxedReader,
        mut writer: BoxedWriter,
        role: ClientRole,
        socket_path: Option<PathBuf>,
    ) -> Result<Self> {
        write_frame(
            writer.as_mut(),
            &ClientFrame::Hello {
                minimum_version: PROTOCOL_VERSION,
                maximum_version: PROTOCOL_VERSION,
                role,
                handoff_window: false,
            },
        )
        .await?;
        let (limits, daemon_hostname, daemon_generation) = match read_frame(reader.as_mut()).await?
        {
            ServerFrame::Hello {
                version,
                limits,
                daemon_hostname,
                daemon_generation,
                ..
            } if version == PROTOCOL_VERSION => {
                // Every serving daemon generation is nonzero.
                if daemon_generation == 0 {
                    bail!("splinterd sent an invalid handshake");
                }
                if let Err(error) = limits.validate_terminal_transport() {
                    bail!(
                        "splinterd advertised invalid terminal transport limits: {}",
                        error.message
                    );
                }
                (limits, daemon_hostname, daemon_generation)
            }
            ServerFrame::Error { error, .. } => {
                return Err(anyhow::Error::new(DaemonProtocolFailure(error)));
            }
            _ => bail!("splinterd sent an invalid handshake"),
        };
        Ok(Self {
            reader: Some(reader),
            writer: Some(writer),
            next_request: 1,
            diagnostic_correlation: None,
            read_buffer: Vec::with_capacity(READ_CHUNK_BYTES),
            read_offset: 0,
            read_scratch: Box::new([0; READ_CHUNK_BYTES]),
            server_frame_assembler: ServerFrameTransactionAssembler::default(),
            queued_events: VecDeque::new(),
            queued_event_bytes: 0,
            limits,
            daemon_hostname,
            daemon_generation,
            socket_path,
            daemon_pid: None,
            trusted_ui: role == ClientRole::TrustedUi,
            unusable: false,
        })
    }

    /// Informational hostname advertised by the daemon at handshake time.
    #[must_use]
    pub fn daemon_hostname(&self) -> Option<&str> {
        self.daemon_hostname.as_deref()
    }

    /// Generation of the daemon that answered the handshake.
    #[must_use]
    pub const fn daemon_generation(&self) -> u64 {
        self.daemon_generation
    }

    /// The daemon process that accepted this local connection, from
    /// `SO_PEERCRED`; `None` over a relayed transport or when its PID is not
    /// visible from this PID namespace.
    #[must_use]
    pub const fn daemon_pid(&self) -> Option<u32> {
        self.daemon_pid
    }

    /// Whether this connection can still send requests. It cannot after a
    /// cancelled or timed-out request, a transport failure, or a
    /// connection-wide daemon error (one with no request ID), so a caller can
    /// tell such an error from a refusal of its own request.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.ensure_usable().is_ok()
    }

    /// Returns the bounds negotiated during the daemon handshake.
    #[must_use]
    pub const fn limits(&self) -> ServerLimits {
        self.limits
    }

    /// Retrieves one exact missing image body using the preferred negotiated source.
    pub async fn image_content_source(
        &mut self,
        splint_id: SplintId,
        incarnation: u64,
        metadata: &ImageContentMetadata,
        cancellation: &CancellationToken,
    ) -> Result<ImageContentSource> {
        if !self.trusted_ui || (!self.limits.image.binary_chunks && !self.limits.image.sealed_memfd)
        {
            bail!("image content transport was not negotiated");
        }
        let mut accepted_transfers = Vec::with_capacity(2);
        if self.limits.image.sealed_memfd {
            accepted_transfers.push(ImageTransferMode::SealedMemfd);
        }
        if self.limits.image.binary_chunks {
            accepted_transfers.push(ImageTransferMode::BinaryChunks);
        }
        let request = ImageContentRequest {
            splint_id,
            incarnation,
            content_id: metadata.content_id,
            generation: metadata.generation,
            digest: metadata.digest,
            accepted_transfers,
        };
        let response = self
            .request_with_cancellation(
                Request::RequestImageContent {
                    request: request.clone(),
                },
                Duration::from_secs(5),
                cancellation,
            )
            .await?;
        let Response::ImageContentReady { transfer } = response else {
            bail!("splinterd returned an invalid image content response");
        };
        transfer
            .validate_for(&request, metadata)
            .map_err(|error| anyhow::anyhow!(error.message))?;
        let control_socket = self
            .socket_path
            .as_deref()
            .context("image content socket path is unavailable")?;
        let mut stream = UnixStream::connect(image_content_socket_path(control_socket))
            .await
            .context("cannot connect to the image content socket")?;
        stream.write_all(&transfer.token).await?;
        let outcome = {
            let receive = async {
                match transfer.transfer {
                    ImageTransferMode::BinaryChunks => receive_image_content(&mut stream, metadata)
                        .await
                        .map(|bytes| ImageContentSource::Buffered(bytes.into())),
                    ImageTransferMode::SealedMemfd => receive_image_memfd(&mut stream, metadata)
                        .await
                        .map(|mapping| ImageContentSource::Mapped(Arc::new(mapping))),
                }
            };
            tokio::pin!(receive);
            let timeout =
                tokio::time::sleep(Duration::from_millis(u64::from(transfer.token_ttl_millis)));
            tokio::pin!(timeout);
            tokio::select! {
                result = &mut receive => Ok(Some(result)),
                () = cancellation.cancelled() => Ok(None),
                () = &mut timeout => Err(anyhow::anyhow!("image content transfer timed out")),
            }
        }?;
        if let Some(result) = outcome {
            if result.is_err() && transfer.transfer == ImageTransferMode::SealedMemfd {
                let _ = stream.write_all(&[2]).await;
            }
            result
        } else {
            if transfer.transfer == ImageTransferMode::BinaryChunks {
                let mut cancel = [0_u8; 9];
                cancel[0] = 2;
                let _ = stream.write_all(&cancel).await;
            } else {
                let _ = stream.write_all(&[2]).await;
            }
            bail!("image content transfer was cancelled")
        }
    }

    /// Retrieves one exact missing image body into a bounded owned buffer.
    pub async fn image_content(
        &mut self,
        splint_id: SplintId,
        incarnation: u64,
        metadata: &ImageContentMetadata,
        cancellation: &CancellationToken,
    ) -> Result<Vec<u8>> {
        Ok(self
            .image_content_source(splint_id, incarnation, metadata, cancellation)
            .await?
            .as_bytes()
            .to_vec())
    }

    /// Sends one request and waits for its correlated response.
    ///
    /// Dropping the returned future while the request is in flight sends at
    /// most one best-effort daemon cancellation and disposes the connection.
    pub async fn request(&mut self, request: Request) -> Result<Response> {
        let request_id = self.reserve_request_id()?;
        let mut in_flight = InFlightRequest {
            connection: self,
            request_id,
            armed: true,
        };
        let result = in_flight.perform(request).await;
        in_flight.disarm();
        result
    }

    /// Sends one request with a client-side deadline.
    ///
    /// On timeout, one best-effort `Cancel` frame is sent and the connection is
    /// closed. The connection is permanently unusable because a late response
    /// could otherwise make request correlation ambiguous.
    pub async fn request_with_deadline(
        &mut self,
        request: Request,
        deadline: Duration,
    ) -> Result<Response> {
        let cancellation = CancellationToken::new();
        self.request_with_cancellation(request, deadline, &cancellation)
            .await
    }

    /// Sends one request with a deadline and caller-owned cancellation token.
    ///
    /// Deadline expiry, token cancellation, or dropping this future after
    /// dispatch sends at most one best-effort daemon `Cancel`, closes the
    /// connection, and discards buffered late frames. A token already cancelled
    /// at entry or a zero deadline returns before reserving an ID or writing.
    /// Cancellation cannot roll back a daemon mutation that has already committed.
    pub async fn request_with_cancellation(
        &mut self,
        request: Request,
        deadline: Duration,
        cancellation: &CancellationToken,
    ) -> Result<Response> {
        if cancellation.is_cancelled() {
            return Err(anyhow::Error::new(RequestCancelled {
                reason: RequestCancellation::Cancelled,
            }));
        }
        if deadline.is_zero() {
            return Err(anyhow::Error::new(RequestCancelled {
                reason: RequestCancellation::DeadlineElapsed,
            }));
        }
        let request_id = self.reserve_request_id()?;
        let mut in_flight = InFlightRequest {
            connection: self,
            request_id,
            armed: true,
        };
        let result = tokio::select! {
            biased;
            result = in_flight.perform(request) => Some(result),
            () = cancellation.cancelled() => None,
            () = tokio::time::sleep(deadline) => {
                return Err(anyhow::Error::new(RequestCancelled {
                    reason: RequestCancellation::DeadlineElapsed,
                }));
            }
        };
        let Some(result) = result else {
            return Err(anyhow::Error::new(RequestCancelled {
                reason: RequestCancellation::Cancelled,
            }));
        };
        in_flight.disarm();
        result
    }

    /// Associates request frames on this connection with one graphical client window.
    pub fn set_diagnostic_correlation(
        &mut self,
        correlation: splinterm_protocol::DiagnosticCorrelation,
    ) {
        self.diagnostic_correlation = Some(correlation);
    }

    fn reserve_request_id(&mut self) -> Result<u64> {
        self.ensure_usable()?;
        let request_id = self.next_request;
        self.next_request = self
            .next_request
            .checked_add(1)
            .context("splinterd request ID space exhausted")?;
        Ok(request_id)
    }

    async fn perform_request(&mut self, request_id: u64, request: Request) -> Result<Response> {
        if let Err(error) = self
            .write_client_frame(&ClientFrame::Request {
                request_id,
                diagnostic_correlation: self.diagnostic_correlation,
                request,
            })
            .await
        {
            self.mark_unusable();
            return Err(error);
        }
        loop {
            let frame = match self.read_server_frame().await {
                Ok(frame) => frame,
                Err(error) => {
                    self.mark_unusable();
                    return Err(error);
                }
            };
            match frame {
                ServerFrame::Response {
                    request_id: response_id,
                    result,
                } if response_id == request_id => return Ok(result),
                ServerFrame::Error {
                    request_id: Some(response_id),
                    error,
                } if response_id == request_id => {
                    return Err(anyhow::Error::new(DaemonProtocolFailure(error)));
                }
                ServerFrame::Error {
                    request_id: None,
                    error,
                } => {
                    self.mark_unusable();
                    return Err(anyhow::Error::new(DaemonProtocolFailure(error)));
                }
                event @ ServerFrame::Event { .. } => {
                    if let Err(error) = self.queue_event(event) {
                        self.mark_unusable();
                        return Err(error);
                    }
                }
                _ => {
                    self.mark_unusable();
                    bail!("splinterd sent a response with the wrong request id")
                }
            }
        }
    }

    /// Returns the next queued or newly received server frame.
    pub async fn next_server_frame(&mut self) -> Result<ServerFrame> {
        self.ensure_usable()?;
        if let Some((event, encoded_bytes)) = self.queued_events.pop_front() {
            self.queued_event_bytes = self.queued_event_bytes.saturating_sub(encoded_bytes);
            return Ok(event);
        }
        let result = match self.read_server_frame().await {
            Ok(ServerFrame::Error { error, .. }) => {
                Err(anyhow::Error::new(DaemonProtocolFailure(error)))
            }
            result => result,
        };
        if result.is_err() {
            self.mark_unusable();
        }
        result
    }

    fn queue_event(&mut self, event: ServerFrame) -> Result<()> {
        let encoded_bytes = serde_json::to_vec(&event)?.len().saturating_add(4);
        if encoded_bytes > MAX_QUEUED_EVENT_BYTES {
            bail!("splinterd sent an event larger than the aggregate transaction limit");
        }
        let total_bytes = self
            .queued_event_bytes
            .checked_add(encoded_bytes)
            .context("splinterd queued-event byte count overflowed")?;
        if self.queued_events.len() >= MAX_QUEUED_EVENTS || total_bytes > MAX_QUEUED_EVENT_BYTES {
            bail!("splinterd sent too many events while a request was pending");
        }
        self.queued_events.push_back((event, encoded_bytes));
        self.queued_event_bytes = total_bytes;
        Ok(())
    }

    async fn read_server_frame(&mut self) -> Result<ServerFrame> {
        self.read_server_frame_with(false).await
    }

    /// Reads one server frame. Only the in-flight `Execute` of
    /// [`Connection::execute_daemon_handoff`] passes `accept_decision_prompt`;
    /// everywhere else a decision prompt is a protocol violation. A clean end
    /// of file between frames fails with [`DaemonClosedConnection`].
    async fn read_server_frame_with(
        &mut self,
        accept_decision_prompt: bool,
    ) -> Result<ServerFrame> {
        self.ensure_usable()?;
        loop {
            let buffered = &self.read_buffer[self.read_offset..];
            if buffered.len() >= 4 {
                let length =
                    u32::from_be_bytes(buffered[..4].try_into().expect("four-byte frame prefix"))
                        as usize;
                if length == 0 || length > MAX_FRAME_BYTES {
                    bail!("splinterd sent an invalid frame length: {length} bytes");
                }
                let frame_length = length + 4;
                if buffered.len() >= frame_length {
                    let frame = serde_json::from_slice(&buffered[4..frame_length])
                        .context("splinterd sent invalid JSON")?;
                    self.read_offset += frame_length;
                    if self.read_offset == self.read_buffer.len() {
                        self.read_buffer.clear();
                        self.read_offset = 0;
                    }
                    match self
                        .server_frame_assembler
                        .push(frame)
                        .map_err(|error| anyhow::anyhow!(error.message))?
                    {
                        // This client takes part in no handoff. It reads with
                        // plain reads, so the kernel already closed any
                        // descriptors sent beside the frame.
                        Some(ServerFrame::HandoffPrepare { .. }) => {
                            bail!("splinterd sent a handoff prepare frame outside a handoff")
                        }
                        // Only an executing handoff request can be asked
                        // about Windows; any other prompt is unsolicited.
                        Some(ServerFrame::HandoffDecisionRequired { .. })
                            if !accept_decision_prompt =>
                        {
                            bail!("splinterd sent a handoff decision prompt outside a handoff")
                        }
                        Some(frame) => return Ok(frame),
                        None => continue,
                    }
                }
            }
            if self.read_offset > 0 {
                let remaining = self.read_buffer.len() - self.read_offset;
                self.read_buffer.copy_within(self.read_offset.., 0);
                self.read_buffer.truncate(remaining);
                self.read_offset = 0;
            }
            let read = self
                .reader
                .as_mut()
                .context("splinterd connection is closed")?
                .read(self.read_scratch.as_mut_slice())
                .await?;
            if read == 0 {
                if self.read_buffer.is_empty() && self.server_frame_assembler.has_pending() {
                    return Err(anyhow::Error::new(DaemonConnectionTruncated(
                        "an incomplete terminal frame transaction",
                    )));
                }
                if self.read_buffer.is_empty() {
                    return Err(anyhow::Error::new(DaemonClosedConnection));
                }
                return Err(anyhow::Error::new(DaemonConnectionTruncated(
                    "a partial frame",
                )));
            }
            self.read_buffer
                .extend_from_slice(&self.read_scratch[..read]);
            if self.read_buffer.len() > MAX_BUFFERED_BYTES {
                bail!("splinterd sent an invalid frame length: buffered data exceeds limit");
            }
        }
    }

    async fn write_client_frame(&mut self, frame: &ClientFrame) -> Result<()> {
        self.ensure_usable()?;
        write_frame(
            self.writer
                .as_mut()
                .context("splinterd connection is closed")?
                .as_mut(),
            frame,
        )
        .await
    }

    fn cancel_and_close(&mut self, request_id: u64) {
        self.unusable = true;
        self.queued_events.clear();
        self.queued_event_bytes = 0;
        self.read_buffer.clear();
        self.read_offset = 0;
        self.server_frame_assembler = ServerFrameTransactionAssembler::default();
        self.reader.take();
        if let Some(mut writer) = self.writer.take()
            && let Ok(frame) = encode_frame(&ClientFrame::Cancel { request_id })
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            runtime.spawn(async move {
                let _ = tokio::time::timeout(Duration::from_millis(100), async {
                    writer.write_all(&frame).await?;
                    writer.shutdown().await
                })
                .await;
            });
        }
    }

    fn ensure_usable(&self) -> Result<()> {
        if self.unusable || self.reader.is_none() || self.writer.is_none() {
            bail!("splinterd connection cannot be reused after cancellation or protocol failure");
        }
        Ok(())
    }

    fn mark_unusable(&mut self) {
        self.unusable = true;
        self.reader.take();
        self.writer.take();
        self.queued_events.clear();
        self.queued_event_bytes = 0;
        self.read_buffer.clear();
        self.read_offset = 0;
        self.server_frame_assembler = ServerFrameTransactionAssembler::default();
    }

    /// Reads the current daemon topology revision.
    pub async fn topology_revision(&mut self) -> Result<TopologyRevision> {
        match self.request(Request::InspectTopology).await? {
            Response::Topology { snapshot } => Ok(snapshot.revision),
            _ => bail!("splinterd did not return topology"),
        }
    }

    /// Resolves the current live incarnation of a selected Splint.
    pub async fn live_incarnation(&mut self, splint_id: SplintId) -> Result<u64> {
        match self.request(Request::InspectSplint { splint_id }).await? {
            Response::Splint { runtime, .. } if runtime.splint_id == splint_id => runtime
                .live_incarnation
                .context("selected Splint does not have a live process"),
            _ => bail!("splinterd did not return the selected Splint identity"),
        }
    }

    /// Acquires a connection-owned controller lease.
    pub async fn acquire_control(
        &mut self,
        splint_id: SplintId,
        incarnation: u64,
        modes: Vec<splinterm_protocol::ControlMode>,
    ) -> Result<u64> {
        match self
            .request(Request::AcquireControl {
                splint_id,
                incarnation,
                modes,
            })
            .await?
        {
            Response::ControlGranted { controller_id, .. } if controller_id != 0 => {
                Ok(controller_id)
            }
            _ => bail!("splinterd did not grant a controller lease"),
        }
    }

    /// Takes the Splint's controller for this Window's connection without
    /// asking, as when a person resizes a pane it only views (Plan 0072).
    /// Another holder's lease ends; this connection's own lease of the
    /// Splint is kept. Only the installed client and remote Windows may.
    pub async fn reclaim_control(
        &mut self,
        splint_id: SplintId,
        incarnation: u64,
        modes: Vec<splinterm_protocol::ControlMode>,
    ) -> Result<u64> {
        match self
            .request(Request::ReclaimControl {
                splint_id,
                incarnation,
                modes,
            })
            .await?
        {
            Response::ControlGranted { controller_id, .. } if controller_id != 0 => {
                Ok(controller_id)
            }
            _ => bail!("splinterd did not grant a controller lease"),
        }
    }

    /// Restores this connection's controller for one Splint from a
    /// relaunched Window's resume ticket (Plan 0061 M3.16). `Ok(None)` means
    /// the daemon accepted the ticket but restored nothing for this Splint;
    /// an invalid, ended, or replayed ticket is an `InvalidHandoffTicket`
    /// protocol error.
    pub async fn claim_handoff_control(
        &mut self,
        ticket: splinterm_protocol::HandoffResumeTicket,
        splint_id: SplintId,
        incarnation: u64,
    ) -> Result<Option<u64>> {
        let Response::HandoffControlClaimed { controllers } = self
            .request(Request::ClaimHandoffControl {
                ticket,
                splint: Some(splinterm_protocol::HandoffClaimTarget {
                    splint_id,
                    incarnation,
                }),
            })
            .await?
        else {
            bail!("splinterd did not answer the handoff control claim");
        };
        if splinterm_protocol::validate_handoff_claimed_controllers(&controllers).is_err() {
            bail!("splinterd answered the handoff control claim with an invalid controller set");
        }
        match controllers.as_slice() {
            [] => Ok(None),
            [controller]
                if controller.splint_id == splint_id && controller.incarnation == incarnation =>
            {
                Ok(Some(controller.controller_id))
            }
            _ => bail!("splinterd restored a controller this claim did not name"),
        }
    }

    /// Releases a controller lease owned by this connection.
    pub async fn release_control(&mut self, controller_id: u64) -> Result<()> {
        if matches!(
            self.request(Request::ReleaseControl { controller_id })
                .await?,
            Response::Acknowledged
        ) {
            Ok(())
        } else {
            bail!("splinterd did not release the controller lease")
        }
    }
}

/// Resolves the configured private daemon socket path.
pub fn socket_path() -> Result<PathBuf> {
    if let Some(path) = env::var_os("SPLINTERM_SOCKET") {
        return Ok(path.into());
    }
    let runtime = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .context("XDG_RUNTIME_DIR is unset; set SPLINTERM_SOCKET explicitly")?;
    Ok(runtime.join("splinterm/splinterd.sock"))
}

async fn write_frame<W>(stream: &mut W, frame: &ClientFrame) -> Result<()>
where
    W: AsyncWrite + Unpin + ?Sized,
{
    stream.write_all(&encode_frame(frame)?).await?;
    Ok(())
}

async fn read_frame<R>(stream: &mut R) -> Result<ServerFrame>
where
    R: AsyncRead + Unpin + ?Sized,
{
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        bail!("splinterd sent an invalid frame length: {length} bytes");
    }
    let mut body = vec![0_u8; length];
    stream.read_exact(&mut body).await.map_err(|error| {
        if error.kind() == ErrorKind::UnexpectedEof {
            anyhow::anyhow!("splinterd sent a truncated frame")
        } else {
            error.into()
        }
    })?;
    serde_json::from_slice(&body).context("splinterd sent invalid JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use splinterm_core::{Dojo, DojoId, Lair, LairId, Splint, SplintState, Topology};
    use splinterm_protocol::{
        ActiveScreen, CellAttributes, ColorSource, MouseTracking, ProtocolError, SearchMatch,
        SubscriptionEvent, TerminalCell, TerminalInputModes, TerminalRow, UnderlineStyle,
        encode_server_frame_transaction,
    };

    #[tokio::test]
    async fn untrusted_role_rejects_image_content_source_before_request() {
        let pixels = [1_u8, 2, 3, 255];
        let metadata = ImageContentMetadata {
            content_id: 1,
            generation: 2,
            width: 1,
            height: 1,
            source_format: splinterm_protocol::ImageSourceFormat::Iterm2,
            alpha_mode: splinterm_protocol::ImageAlphaMode::Opaque,
            digest: Sha256::digest(pixels).into(),
            byte_length: pixels.len(),
            retention: splinterm_protocol::ImageRetention::WhilePlaced,
        };
        let (_server, stream) = UnixStream::pair().unwrap();
        let mut connection = established(stream);
        let error = connection
            .image_content_source(SplintId::new(), 1, &metadata, &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "image content transport was not negotiated"
        );
    }

    #[test]
    fn image_source_cache_deduplicates_exact_identity_and_tracks_bounds() {
        let pixels = vec![1_u8, 2, 3, 255];
        let metadata = ImageContentMetadata {
            content_id: 1,
            generation: 2,
            width: 1,
            height: 1,
            source_format: splinterm_protocol::ImageSourceFormat::Sixel,
            alpha_mode: splinterm_protocol::ImageAlphaMode::Opaque,
            digest: Sha256::digest(&pixels).into(),
            byte_length: pixels.len(),
            retention: splinterm_protocol::ImageRetention::WhilePlaced,
        };
        let mut cache = ImageContentCache::default();
        let first = cache.insert(&metadata, pixels.clone()).unwrap();
        let second = cache.insert(&metadata, pixels).unwrap();
        assert!(matches!(
            (&first, &second),
            (ImageContentSource::Buffered(first), ImageContentSource::Buffered(second))
                if Arc::ptr_eq(first, second)
        ));
        assert!(cache.contains(&metadata));
        assert_eq!(cache.get(&metadata).unwrap().as_bytes(), first.as_bytes());
        assert_eq!(
            cache.metrics(),
            ImageContentCacheMetrics {
                bytes: 4,
                entries: 1,
                high_water_bytes: 4,
                high_water_entries: 1,
            }
        );
    }

    #[test]
    fn image_source_cache_evicts_derived_entries_before_exceeding_its_limit() {
        let metadata = |content_id: u64, pixels: &[u8]| ImageContentMetadata {
            content_id,
            generation: 1,
            width: 1,
            height: 1,
            source_format: splinterm_protocol::ImageSourceFormat::Sixel,
            alpha_mode: splinterm_protocol::ImageAlphaMode::Opaque,
            digest: Sha256::digest(pixels).into(),
            byte_length: pixels.len(),
            retention: splinterm_protocol::ImageRetention::WhilePlaced,
        };
        let first_pixels = vec![1, 2, 3, 255];
        let second_pixels = vec![4, 5, 6, 255];
        let third_pixels = vec![7, 8, 9, 255];
        let first = metadata(1, &first_pixels);
        let second = metadata(2, &second_pixels);
        let third = metadata(3, &third_pixels);
        let mut cache = ImageContentCache::with_maximum_bytes(8).unwrap();
        cache.insert(&first, first_pixels).unwrap();
        cache.insert(&second, second_pixels).unwrap();
        cache.insert(&third, third_pixels).unwrap();
        assert!(!cache.contains(&first));
        assert!(cache.contains(&second));
        assert!(cache.contains(&third));
        assert_eq!(
            cache.metrics(),
            ImageContentCacheMetrics {
                bytes: 8,
                entries: 2,
                high_water_bytes: 8,
                high_water_entries: 2,
            }
        );
    }

    #[test]
    fn renderer_leases_are_atomic_pinned_and_remain_in_resident_metrics() {
        let pixels = vec![1_u8, 2, 3, 255];
        let metadata = ImageContentMetadata {
            content_id: 1,
            generation: 1,
            width: 1,
            height: 1,
            source_format: splinterm_protocol::ImageSourceFormat::Sixel,
            alpha_mode: splinterm_protocol::ImageAlphaMode::Opaque,
            digest: Sha256::digest(&pixels).into(),
            byte_length: pixels.len(),
            retention: splinterm_protocol::ImageRetention::WhilePlaced,
        };
        let cache = SharedImageContentCache::with_maximum_bytes(4).unwrap();
        cache
            .insert_source(&metadata, ImageContentSource::Buffered(Arc::from(pixels)))
            .unwrap();
        let leases = cache.lease(std::slice::from_ref(&metadata)).unwrap();
        assert_eq!(leases.bytes(), 4);
        let replacement_pixels = vec![4_u8, 5, 6, 255];
        let mut replacement = metadata.clone();
        replacement.content_id = 2;
        replacement.digest = Sha256::digest(&replacement_pixels).into();
        assert!(
            cache
                .insert_source(
                    &replacement,
                    ImageContentSource::Buffered(Arc::from(replacement_pixels.clone())),
                )
                .is_err()
        );
        assert_eq!(cache.metrics().unwrap().bytes, 4);
        assert!(cache.contains(&metadata).unwrap());
        drop(leases);
        cache
            .insert_source(
                &replacement,
                ImageContentSource::Buffered(Arc::from(replacement_pixels)),
            )
            .unwrap();
        assert!(!cache.contains(&metadata).unwrap());
        assert!(cache.contains(&replacement).unwrap());
    }

    #[test]
    fn failed_admission_after_partial_eviction_keeps_exact_metrics() {
        let metadata = |content_id: u64, pixels: &[u8]| ImageContentMetadata {
            content_id,
            generation: 1,
            width: 1,
            height: u32::try_from(pixels.len() / 4).unwrap(),
            source_format: splinterm_protocol::ImageSourceFormat::Sixel,
            alpha_mode: splinterm_protocol::ImageAlphaMode::Opaque,
            digest: Sha256::digest(pixels).into(),
            byte_length: pixels.len(),
            retention: splinterm_protocol::ImageRetention::WhilePlaced,
        };
        let first_pixels = vec![1_u8, 2, 3, 255];
        let second_pixels = vec![4_u8, 5, 6, 255];
        let replacement_pixels = vec![7_u8; 8];
        let first = metadata(1, &first_pixels);
        let second = metadata(2, &second_pixels);
        let replacement = metadata(3, &replacement_pixels);
        let cache = SharedImageContentCache::with_maximum_bytes(8).unwrap();
        cache
            .insert_source(
                &first,
                ImageContentSource::Buffered(Arc::from(first_pixels)),
            )
            .unwrap();
        cache
            .insert_source(
                &second,
                ImageContentSource::Buffered(Arc::from(second_pixels)),
            )
            .unwrap();
        let leases = cache.lease(std::slice::from_ref(&first)).unwrap();
        assert!(
            cache
                .insert_source(
                    &replacement,
                    ImageContentSource::Buffered(Arc::from(replacement_pixels)),
                )
                .is_err()
        );
        assert_eq!(
            cache.metrics().unwrap(),
            ImageContentCacheMetrics {
                bytes: 4,
                entries: 1,
                high_water_bytes: 8,
                high_water_entries: 2,
            }
        );
        assert!(cache.contains(&first).unwrap());
        assert!(!cache.contains(&second).unwrap());
        drop(leases);
    }

    #[tokio::test]
    async fn sealed_memfd_receiver_validates_and_maps_immutable_content() {
        use rustix::{
            fs::{MemfdFlags, SealFlags, fcntl_add_seals, memfd_create},
            io::write,
            net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags, sendmsg},
        };

        let pixels = [1_u8, 2, 3, 255];
        let digest: [u8; 32] = Sha256::digest(pixels).into();
        let metadata = ImageContentMetadata {
            content_id: 1,
            generation: 2,
            width: 1,
            height: 1,
            source_format: splinterm_protocol::ImageSourceFormat::Sixel,
            alpha_mode: splinterm_protocol::ImageAlphaMode::Opaque,
            digest,
            byte_length: pixels.len(),
            retention: splinterm_protocol::ImageRetention::WhilePlaced,
        };
        let (mut server, mut client) = UnixStream::pair().unwrap();
        let sender = tokio::spawn(async move {
            let fd = memfd_create(
                "splinterm-client-memfd-test",
                MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
            )
            .unwrap();
            write(&fd, &pixels).unwrap();
            fcntl_add_seals(
                &fd,
                SealFlags::WRITE | SealFlags::GROW | SealFlags::SHRINK | SealFlags::SEAL,
            )
            .unwrap();
            server.writable().await.unwrap();
            let descriptors = [fd.as_fd()];
            let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
            let mut ancillary = SendAncillaryBuffer::new(&mut space);
            assert!(ancillary.push(SendAncillaryMessage::ScmRights(&descriptors)));
            assert_eq!(
                sendmsg(
                    server.as_fd(),
                    &[std::io::IoSlice::new(b"F")],
                    &mut ancillary,
                    SendFlags::empty(),
                )
                .unwrap(),
                1
            );
            let mut header = [0_u8; IMAGE_MEMFD_HEADER_BYTES];
            header[0..5].copy_from_slice(b"SPIF\x01");
            header[5..13].copy_from_slice(&4_u64.to_be_bytes());
            header[13..45].copy_from_slice(&digest);
            server.write_all(&header).await.unwrap();
            let mut acknowledgement = [0_u8; 1];
            server.read_exact(&mut acknowledgement).await.unwrap();
            assert_eq!(acknowledgement, [1]);
        });
        let mapping = receive_image_memfd(&mut client, &metadata).await.unwrap();
        assert_eq!(&*mapping, &pixels);
        sender.await.unwrap();
    }

    #[tokio::test]
    async fn binary_image_content_receiver_bounds_chunks_and_verifies_digest() {
        let pixels = [1_u8, 2, 3, 255];
        let digest: [u8; 32] = Sha256::digest(pixels).into();
        let metadata = ImageContentMetadata {
            content_id: 1,
            generation: 2,
            width: 1,
            height: 1,
            source_format: splinterm_protocol::ImageSourceFormat::Sixel,
            alpha_mode: splinterm_protocol::ImageAlphaMode::Opaque,
            digest,
            byte_length: pixels.len(),
            retention: splinterm_protocol::ImageRetention::WhilePlaced,
        };
        let (mut server, mut client) = UnixStream::pair().unwrap();
        let sender = tokio::spawn(async move {
            let mut header = [0_u8; IMAGE_CONTENT_HEADER_BYTES];
            header[0..5].copy_from_slice(b"SPIM\x01");
            header[5..13].copy_from_slice(&4_u64.to_be_bytes());
            header[13..45].copy_from_slice(&digest);
            header[45..49]
                .copy_from_slice(&u32::try_from(MAX_IMAGE_CHUNK_BYTES).unwrap().to_be_bytes());
            header[49..53]
                .copy_from_slice(&u32::try_from(MAX_IMAGE_CHUNK_WINDOW).unwrap().to_be_bytes());
            server.write_all(&header).await.unwrap();
            server.write_all(&0_u64.to_be_bytes()).await.unwrap();
            server.write_all(&4_u32.to_be_bytes()).await.unwrap();
            server.write_all(&pixels).await.unwrap();
            let mut acknowledgement = [0_u8; 9];
            server.read_exact(&mut acknowledgement).await.unwrap();
            assert_eq!(acknowledgement[0], 1);
            assert_eq!(
                u64::from_be_bytes(acknowledgement[1..9].try_into().unwrap()),
                4
            );
        });
        assert_eq!(
            receive_image_content(&mut client, &metadata).await.unwrap(),
            pixels
        );
        sender.await.unwrap();
    }

    async fn read_client_frame<R>(stream: &mut R) -> ClientFrame
    where
        R: AsyncRead + Unpin,
    {
        let mut length = [0_u8; 4];
        stream.read_exact(&mut length).await.unwrap();
        let mut body = vec![0_u8; u32::from_be_bytes(length) as usize];
        stream.read_exact(&mut body).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn oversized_terminal_event() -> ServerFrame {
        let cell = TerminalCell {
            content: "x".repeat(300),
            spacer_remaining: None,
            attributes: CellAttributes::default(),
        };
        let snapshot = TerminalSnapshot {
            splint_id: SplintId::new(),
            incarnation: 1,
            revision: 1,
            columns: usize::from(splinterm_protocol::MAX_COLUMNS),
            rows: usize::from(splinterm_protocol::MAX_ROWS),
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
            visible_rows: (1..=u64::from(splinterm_protocol::MAX_ROWS))
                .map(|row_id| TerminalRow {
                    row_id: Some(row_id),
                    linebreak: false,
                    cells: vec![cell.clone(); usize::from(splinterm_protocol::MAX_COLUMNS)],
                })
                .collect(),
            history_generation: 1,
            oldest_available_scrollback_row_id: None,
            newest_available_scrollback_row_id: None,
            scrollback_rows: Vec::new(),
            available_scrollback_rows: 0,
            omitted_oldest_scrollback_rows: 0,
            images: None,
            exited_code: None,
            exited_signal: None,
        };
        snapshot.validate().unwrap();
        ServerFrame::Event {
            subscription_id: 1,
            sequence: 1,
            event: SubscriptionEvent::Snapshot { snapshot },
        }
    }

    fn established(stream: UnixStream) -> Connection {
        let (reader, writer) = stream.into_split();
        Connection {
            reader: Some(Box::new(reader)),
            writer: Some(Box::new(writer)),
            next_request: 1,
            diagnostic_correlation: None,
            read_buffer: Vec::with_capacity(READ_CHUNK_BYTES),
            read_offset: 0,
            read_scratch: Box::new([0; READ_CHUNK_BYTES]),
            server_frame_assembler: ServerFrameTransactionAssembler::default(),
            queued_events: VecDeque::new(),
            queued_event_bytes: 0,
            limits: ServerLimits::default(),
            daemon_hostname: None,
            daemon_generation: 1,
            socket_path: None,
            daemon_pid: None,
            trusted_ui: false,
            unusable: false,
        }
    }

    #[tokio::test]
    async fn connection_publishes_one_complete_oversized_terminal_transaction() {
        let logical = oversized_terminal_event();
        let frames = encode_server_frame_transaction(&logical).unwrap();
        assert!(frames.len() >= 3);
        let (client, mut server) = UnixStream::pair().unwrap();
        let writer = tokio::spawn(async move {
            for frame in frames {
                server.write_all(&frame).await.unwrap();
            }
        });
        let mut connection = established(client);
        assert_eq!(connection.next_server_frame().await.unwrap(), logical);
        assert!(!connection.server_frame_assembler.has_pending());
        writer.await.unwrap();
    }

    #[tokio::test]
    async fn connection_rejects_reordered_terminal_transaction_and_becomes_unusable() {
        let start = encode_frame(&ServerFrame::TerminalTransactionStart {
            total_bytes: MAX_FRAME_BYTES + 1,
            chunks: 3,
        })
        .unwrap();
        let reordered = encode_frame(&ServerFrame::TerminalTransactionChunk {
            index: 1,
            data: "AQ==".to_owned(),
        })
        .unwrap();
        let (client, mut server) = UnixStream::pair().unwrap();
        let writer = tokio::spawn(async move {
            server.write_all(&start).await.unwrap();
            server.write_all(&reordered).await.unwrap();
        });
        let mut connection = established(client);
        let error = connection.next_server_frame().await.unwrap_err();
        assert!(error.to_string().contains("out of order"));
        assert!(
            connection
                .next_server_frame()
                .await
                .unwrap_err()
                .to_string()
                .contains("cannot be reused")
        );
        writer.await.unwrap();
    }

    #[tokio::test]
    async fn connection_rejects_eof_during_terminal_transaction() {
        let start = encode_frame(&ServerFrame::TerminalTransactionStart {
            total_bytes: MAX_FRAME_BYTES + 1,
            chunks: 3,
        })
        .unwrap();
        let (client, mut server) = UnixStream::pair().unwrap();
        let writer = tokio::spawn(async move {
            server.write_all(&start).await.unwrap();
            server.shutdown().await.unwrap();
        });
        let mut connection = established(client);
        let error = connection.next_server_frame().await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("incomplete terminal frame transaction")
        );
        assert!(!connection.server_frame_assembler.has_pending());
        writer.await.unwrap();
    }

    fn fixture_document(raw: &str) -> serde_json::Value {
        serde_json::from_str::<serde_json::Value>(raw).unwrap()["document"].clone()
    }

    fn serialized(value: &impl Serialize) -> serde_json::Value {
        serde_json::to_value(value).unwrap()
    }

    fn reviewed_row() -> TerminalRow {
        TerminalRow {
            row_id: Some(2),
            linebreak: true,
            cells: vec![TerminalCell {
                content: "ok".to_owned(),
                spacer_remaining: None,
                attributes: CellAttributes {
                    bold: false,
                    dim: false,
                    italic: false,
                    underline: UnderlineStyle::None,
                    underline_color_source: ColorSource::Default,
                    underline_color: 0,
                    strikethrough: false,
                    blink: false,
                    conceal: false,
                    reverse: false,
                    foreground_source: ColorSource::Default,
                    foreground: 0,
                    background_source: ColorSource::Default,
                    background: 0,
                },
            }],
        }
    }

    #[test]
    fn shared_terminal_projection_preserves_semantic_cells_and_bounds() {
        let attributes = reviewed_row().cells[0].attributes;
        let row = TerminalRow {
            row_id: Some(9),
            linebreak: true,
            cells: vec![
                TerminalCell {
                    content: "e\u{301}".to_owned(),
                    spacer_remaining: None,
                    attributes,
                },
                TerminalCell {
                    content: "界".to_owned(),
                    spacer_remaining: None,
                    attributes,
                },
                TerminalCell {
                    content: String::new(),
                    spacer_remaining: Some(1),
                    attributes,
                },
                TerminalCell {
                    content: "\u{fffd}".to_owned(),
                    spacer_remaining: None,
                    attributes,
                },
            ],
        };
        assert_eq!(
            project_terminal_rows(&[row]).unwrap(),
            [ProjectedTerminalRow {
                linebreak: true,
                cells: vec![
                    ProjectedTerminalCell {
                        text: "e\u{301}".to_owned(),
                        width: 1,
                    },
                    ProjectedTerminalCell {
                        text: "界".to_owned(),
                        width: 2,
                    },
                    ProjectedTerminalCell {
                        text: "\u{fffd}".to_owned(),
                        width: 1,
                    },
                ],
            }]
        );

        let mut oversized = reviewed_row();
        oversized.cells[0].content = "x".repeat(65);
        assert!(project_terminal_rows(&[oversized]).is_err());
    }

    fn reviewed_topology() -> TopologySnapshot {
        let lair_id: LairId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77101".parse().unwrap();
        let dojo_id: DojoId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77102".parse().unwrap();
        let splint_id: SplintId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77103".parse().unwrap();
        let mut splint = Splint::shell(PathBuf::from("/tmp"));
        splint.id = splint_id;
        splint.title = "build".to_owned();
        splint.last_incarnation = Some(2);
        splint.state = SplintState::Running;
        let dojo = Dojo {
            id: dojo_id,
            name: "terminal".to_owned(),
            default_focus: splint_id,
            root: LayoutNode::Leaf(splint),
        };
        let dojo = Lair {
            id: lair_id,
            name: "main".to_owned(),
            lifetime: splinterm_core::LairLifetime::default(),
            retention: splinterm_core::LairRetention::default(),
            provenance: None,
            dojos: vec![dojo],
        };
        let mut topology = Topology::new();
        topology
            .insert_lair_at(TopologyRevision::new(0), dojo)
            .unwrap();
        TopologySnapshot {
            revision: topology.revision(),
            topology,
            runtimes: vec![SplintRuntimeSummary {
                splint_id,
                live_incarnation: Some(2),
                last_incarnation: Some(2),
                restorable: false,
                lifecycle: SplintLifecycle::Running,
                exit_status: None,
                agent_status: None,
                foreground_app: None,
            }],
            presentation_offers: Vec::new(),
        }
    }

    #[test]
    fn public_v2_graphical_focus_is_narrow_and_matches_golden_fixture() {
        let splint_id: SplintId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77103".parse().unwrap();
        let expected = fixture_document(include_str!(
            "../../../tests/automation/fixtures/valid/cli-focus.json"
        ));
        assert_eq!(
            serialized(
                &graphical_focus_envelope(
                    Some(splint_id),
                    Some(Path::new("/home/example/project")),
                )
                .unwrap()
            ),
            expected
        );

        let empty = serialized(&graphical_focus_envelope(None, None).unwrap());
        assert_eq!(empty["data"]["splint_id"], Value::Null);
        assert_eq!(empty["data"]["cwd"], Value::Null);
        assert!(graphical_focus_envelope(None, Some(Path::new("/tmp"))).is_err());
        assert!(graphical_focus_envelope(Some(splint_id), Some(Path::new("relative"))).is_err());
    }

    #[test]
    fn public_v2_read_dtos_match_golden_fixtures() {
        let snapshot = reviewed_topology();
        let mut expected = fixture_document(include_str!(
            "../../../tests/automation/fixtures/valid/cli-list-lairs.json"
        ));
        expected["request_id"] = serde_json::json!("1");
        expected["resource"]["topology_revision"] = serde_json::json!(1);
        assert_eq!(
            serialized(&list_lairs_envelope(&snapshot).unwrap()),
            expected
        );

        let empty = TopologySnapshot {
            revision: TopologyRevision::new(0),
            topology: Topology::new(),
            runtimes: Vec::new(),
            presentation_offers: Vec::new(),
        };
        let mut expected = fixture_document(include_str!(
            "../../../tests/automation/fixtures/valid/cli-inspect-topology.json"
        ));
        expected["request_id"] = serde_json::json!("1");
        expected["resource"]["topology_revision"] = serde_json::json!(0);
        assert_eq!(
            serialized(&inspect_topology_envelope(&empty).unwrap()),
            expected
        );

        let splint_id = snapshot.runtimes[0].splint_id;
        let mut expected = fixture_document(include_str!(
            "../../../tests/automation/fixtures/valid/cli-inspect-splint.json"
        ));
        expected["request_id"] = serde_json::json!("1");
        expected["resource"]["topology_revision"] = serde_json::json!(1);
        assert_eq!(
            serialized(&inspect_splint_envelope(&snapshot, splint_id).unwrap()),
            expected
        );
    }

    fn status_summary(
        state: AgentState,
        detail: Option<&str>,
        is_stale: bool,
        reported_age_ms: u64,
        sequence: u64,
    ) -> AgentStatusSummary {
        AgentStatusSummary {
            state,
            detail: detail.map(str::to_owned),
            sequence,
            stale: is_stale,
            reported_age_ms,
        }
    }

    #[test]
    fn public_v2_agent_status_dtos_match_golden_fixtures() {
        let mut snapshot = reviewed_topology();
        let splint_id = snapshot.runtimes[0].splint_id;
        snapshot.runtimes[0].agent_status = Some(status_summary(
            AgentState::NeedsInput,
            Some("waiting for approval"),
            false,
            1_500,
            4,
        ));
        assert_eq!(
            serialized(&agent_status_get_envelope(&snapshot, splint_id).unwrap()),
            fixture_document(include_str!(
                "../../../tests/automation/fixtures/valid/cli-agent-status-get.json"
            ))
        );
        assert_eq!(
            serialized(&inspect_topology_envelope(&snapshot).unwrap()),
            fixture_document(include_str!(
                "../../../tests/automation/fixtures/valid/cli-inspect-topology-agent-status.json"
            ))
        );
        snapshot.runtimes[0].agent_status =
            Some(status_summary(AgentState::Working, None, true, 121_000, 3));
        assert_eq!(
            serialized(&inspect_splint_envelope(&snapshot, splint_id).unwrap()),
            fixture_document(include_str!(
                "../../../tests/automation/fixtures/valid/cli-inspect-splint-agent-status.json"
            ))
        );

        // No status leaves the reviewed pre-agent-status shapes byte-for-byte.
        snapshot.runtimes[0].agent_status = None;
        let inspected = serialized(&inspect_splint_envelope(&snapshot, splint_id).unwrap());
        assert!(inspected["data"].get("agent_status").is_none());
        let topology = serialized(&inspect_topology_envelope(&snapshot).unwrap());
        assert!(topology["data"]["splints"][0].get("agent_status").is_none());
        let empty = serialized(&agent_status_get_envelope(&snapshot, splint_id).unwrap());
        assert_eq!(empty["data"]["agent_status"], Value::Null);
        assert_eq!(empty["data"]["current_incarnation"], 2);
        assert!(agent_status_get_envelope(&snapshot, SplintId::new()).is_err());

        // A summary that violates the closed bounds is never projected.
        snapshot.runtimes[0].agent_status = Some(status_summary(
            AgentState::Done,
            Some("two\nlines"),
            false,
            0,
            1,
        ));
        assert!(inspect_splint_envelope(&snapshot, splint_id).is_err());
        assert!(agent_status_get_envelope(&snapshot, splint_id).is_err());
    }

    #[test]
    fn public_v2_agent_status_acknowledgements_match_golden_fixtures() {
        let splint_id: SplintId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77103".parse().unwrap();
        assert_eq!(
            serialized(
                &agent_status_report_envelope(
                    splint_id,
                    2,
                    Some(AgentState::NeedsInput),
                    4,
                    Some(1_760_630_400_000),
                )
                .unwrap()
            ),
            fixture_document(include_str!(
                "../../../tests/automation/fixtures/valid/cli-agent-status-set.json"
            ))
        );
        assert_eq!(
            serialized(&agent_status_report_envelope(splint_id, 2, None, 5, None).unwrap()),
            fixture_document(include_str!(
                "../../../tests/automation/fixtures/valid/cli-agent-status-clear.json"
            ))
        );
        let resting = serialized(
            &agent_status_report_envelope(splint_id, 2, Some(AgentState::Done), 6, None).unwrap(),
        );
        assert_eq!(resting["data"]["expires_at_ms"], Value::Null);
        assert!(
            agent_status_report_envelope(splint_id, 0, Some(AgentState::Idle), 1, None).is_err()
        );
        assert!(agent_status_report_envelope(splint_id, 2, None, 1, Some(1)).is_err());
        let failure = CliEnvelopeV2::protocol_failure(
            "agent_status_set",
            &ProtocolError::new(
                ErrorCode::RateLimited,
                "agent status reports exceed 10 per second for this Splint; retry later",
            ),
            "agent status reports exceed 10 per second for this Splint; retry later",
        )
        .unwrap();
        assert_eq!(
            serialized(&failure),
            fixture_document(include_str!(
                "../../../tests/automation/fixtures/valid/cli-agent-status-error.json"
            ))
        );
    }

    #[test]
    fn public_v2_history_dtos_match_golden_fixtures() {
        let lair_id: LairId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77101".parse().unwrap();
        let dojo_id: DojoId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77102".parse().unwrap();
        let splint_id: SplintId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77103".parse().unwrap();
        let page = ScrollbackPage {
            splint_id,
            incarnation: 2,
            terminal_revision: 9,
            history_generation: 3,
            oldest_available_row_id: Some(1),
            newest_available_row_id: Some(2),
            rows: vec![reviewed_row()],
            has_older: true,
        };
        let actual = serialized(&scrollback_page_envelope(lair_id, dojo_id, &page).unwrap());
        let mut expected = fixture_document(include_str!(
            "../../../tests/automation/fixtures/valid/cli-scrollback-page.json"
        ));
        expected["request_id"] = serde_json::json!("1");
        expected["data"]["rows"][0]["row_id"] = serde_json::json!(2);
        expected["data"]["continuation_cursor"] = actual["data"]["continuation_cursor"].clone();
        assert_eq!(actual, expected);

        let search = SearchPage {
            splint_id,
            incarnation: 2,
            terminal_revision: 9,
            history_generation: 3,
            matches: vec![SearchMatch {
                row_id: 1,
                start_column: 0,
                end_column: 2,
                preview: "ok".to_owned(),
            }],
            next_cursor: None,
            timed_out: false,
        };
        let mut expected = fixture_document(include_str!(
            "../../../tests/automation/fixtures/valid/cli-search-results.json"
        ));
        expected["request_id"] = serde_json::json!("1");
        assert_eq!(
            serialized(&search_page_envelope(lair_id, dojo_id, &search).unwrap()),
            expected
        );

        for (operation, reason, fixture) in [
            (
                "scrollback_page",
                ReadResyncReasonV2::HistoryReplaced,
                include_str!("../../../tests/automation/fixtures/valid/cli-scrollback-resync.json"),
            ),
            (
                "search_scrollback",
                ReadResyncReasonV2::StaleRevision,
                include_str!("../../../tests/automation/fixtures/valid/cli-search-resync.json"),
            ),
        ] {
            let mut expected = fixture_document(fixture);
            expected["request_id"] = serde_json::json!("1");
            assert_eq!(
                serialized(
                    &read_resync_envelope(
                        operation,
                        TerminalReadProvenanceV2 {
                            lair_id,
                            dojo_id,
                            splint_id,
                            incarnation: 2,
                            terminal_revision: 9,
                            history_generation: 3,
                        },
                        reason,
                    )
                    .unwrap()
                ),
                expected
            );
        }
    }

    #[test]
    fn public_v2_terminal_cursors_round_trip_and_reject_tampering() {
        let splint_id = SplintId::new();
        for cursor in [
            TerminalContinuationV2::Scrollback {
                splint_id,
                incarnation: 2,
                terminal_revision: 0,
                history_generation: 3,
                before_row_id: 8,
            },
            TerminalContinuationV2::Search {
                splint_id,
                incarnation: 2,
                terminal_revision: 9,
                history_generation: 3,
                daemon_cursor: "next".to_owned(),
            },
        ] {
            let encoded = encode_terminal_cursor(&cursor).unwrap();
            assert!((16..=256).contains(&encoded.len()));
            assert_eq!(decode_terminal_cursor(&encoded).unwrap(), cursor);
            assert!(decode_terminal_cursor(&format!("{encoded}A")).is_err());
        }
    }

    #[test]
    fn public_v2_terminal_action_dtos_match_golden_fixtures() {
        let provenance = TerminalReadProvenanceV2 {
            lair_id: "018f4d8c-2a18-4b31-8c2f-9e7c5de77101".parse().unwrap(),
            dojo_id: "018f4d8c-2a18-4b31-8c2f-9e7c5de77102".parse().unwrap(),
            splint_id: "018f4d8c-2a18-4b31-8c2f-9e7c5de77103".parse().unwrap(),
            incarnation: 2,
            terminal_revision: 9,
            history_generation: 3,
        };
        for (operation, dimensions, fixture) in [
            (
                "input",
                None,
                include_str!("../../../tests/automation/fixtures/valid/cli-input.json"),
            ),
            (
                "resize",
                Some((120, 40)),
                include_str!("../../../tests/automation/fixtures/valid/cli-resize.json"),
            ),
        ] {
            let mut expected = fixture_document(fixture);
            expected["request_id"] = serde_json::json!("1");
            assert_eq!(
                serialized(&terminal_action_envelope(operation, provenance, dimensions).unwrap()),
                expected
            );
        }
    }

    fn mutation_ids() -> (LairId, DojoId, SplintId) {
        (
            "018f4d8c-2a18-4b31-8c2f-9e7c5de77101".parse().unwrap(),
            "018f4d8c-2a18-4b31-8c2f-9e7c5de77102".parse().unwrap(),
            "018f4d8c-2a18-4b31-8c2f-9e7c5de77103".parse().unwrap(),
        )
    }

    fn mutation_identity() -> MutationIdentityV2 {
        let (lair_id, dojo_id, splint_id) = mutation_ids();
        MutationIdentityV2 {
            lair_id: Some(lair_id),
            dojo_id: Some(dojo_id),
            splint_id: Some(splint_id),
            topology_revision: Some(7),
            incarnation: Some(2),
        }
    }

    #[test]
    fn public_v2_mutation_dtos_match_golden_fixtures() {
        let mut expected = fixture_document(include_str!(
            "../../../tests/automation/fixtures/valid/cli-create-lair.json"
        ));
        expected["request_id"] = serde_json::json!("1");
        assert_eq!(
            serialized(&created_mutation_envelope("create_lair", mutation_identity()).unwrap()),
            expected
        );

        let mut identity = mutation_identity();
        identity.incarnation = None;
        let mut expected = fixture_document(include_str!(
            "../../../tests/automation/fixtures/valid/cli-layout-mutation.json"
        ));
        expected["request_id"] = serde_json::json!("1");
        assert_eq!(
            serialized(&committed_mutation_envelope("set_split_ratio", identity, false).unwrap()),
            expected
        );

        identity.splint_id = None;
        let mut expected = fixture_document(include_str!(
            "../../../tests/automation/fixtures/valid/cli-confirmed-close.json"
        ));
        expected["request_id"] = serde_json::json!("1");
        assert_eq!(
            serialized(&committed_mutation_envelope("close_dojo", identity, true).unwrap()),
            expected
        );

        let mut expected = fixture_document(include_str!(
            "../../../tests/automation/fixtures/valid/cli-relaunch-splint.json"
        ));
        expected["request_id"] = serde_json::json!("1");
        assert_eq!(
            serialized(&process_started_envelope("relaunch_splint", mutation_identity()).unwrap()),
            expected
        );
    }

    #[test]
    fn public_v2_restore_kill_and_revoke_dtos_match_golden_fixtures() {
        let (lair_id, dojo_id, splint_id) = mutation_ids();
        let mut expected = fixture_document(include_str!(
            "../../../tests/automation/fixtures/valid/cli-restore-lair.json"
        ));
        expected["request_id"] = serde_json::json!("1");
        assert_eq!(
            serialized(
                &restore_many_envelope(
                    "restore_lair",
                    MutationIdentityV2 {
                        lair_id: Some(lair_id),
                        dojo_id: None,
                        splint_id: None,
                        topology_revision: Some(7),
                        incarnation: None,
                    },
                    &[RestoreLeafResult {
                        splint_id,
                        incarnation: Some(2),
                        error: None,
                    }],
                )
                .unwrap()
            ),
            expected
        );

        let mut expected = fixture_document(include_str!(
            "../../../tests/automation/fixtures/valid/cli-kill.json"
        ));
        expected["request_id"] = serde_json::json!("1");
        assert_eq!(
            serialized(&kill_envelope(lair_id, dojo_id, splint_id, 2).unwrap()),
            expected
        );

        let grant = AccessGrant {
            grant_id: 42,
            splint_id,
            incarnation: 2,
            scopes: vec![AccessScope::Observe],
            requester: "/usr/bin/editor".to_owned(),
            expires_at_unix_seconds: 1,
        };
        let mut expected = fixture_document(include_str!(
            "../../../tests/automation/fixtures/valid/cli-revoke-access.json"
        ));
        expected["request_id"] = serde_json::json!("1");
        assert_eq!(
            serialized(&revoke_envelope(lair_id, dojo_id, &grant).unwrap()),
            expected
        );
    }

    #[test]
    fn public_v2_authorization_and_audit_dtos_match_golden_fixtures() {
        let lair_id: LairId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77101".parse().unwrap();
        let dojo_id: DojoId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77102".parse().unwrap();
        let splint_id: SplintId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77103".parse().unwrap();
        let mut expected = fixture_document(include_str!(
            "../../../tests/automation/fixtures/valid/cli-authorization-status.json"
        ));
        expected["request_id"] = serde_json::json!("1");
        assert_eq!(
            serialized(
                &authorization_status_envelope(
                    lair_id,
                    dojo_id,
                    splint_id,
                    2,
                    &[],
                    &[PersistentAuthorizationStatus {
                        policy_rule_id: "editor".to_owned(),
                        scopes: vec![AutomationScope::TerminalVisibleRead],
                        expires_at_unix_seconds: None,
                    }],
                    false,
                )
                .unwrap()
            ),
            expected
        );

        let page = AuditPage {
            records: vec![splinterm_protocol::AuditRecord {
                schema: "splinterm.audit.v2".to_owned(),
                retention: "daemon_lifetime".to_owned(),
                audit_id: 73,
                unix_seconds: 1_721_760_000,
                policy_generation: None,
                policy_rule_id: None,
                peer: splinterm_protocol::AuditPeer {
                    uid: 1_000,
                    executable_path: "/usr/bin/editor".to_owned(),
                    executable_sha256: "a".repeat(64),
                    device: None,
                    inode: None,
                },
                operation: splinterm_protocol::AuditOperation::Input,
                resource: Some(splinterm_protocol::AuditResource {
                    lair_id: None,
                    dojo_id: None,
                    splint_id: Some(splint_id),
                    incarnation: Some(2),
                }),
                requested_scopes: vec![AutomationScope::Input],
                decision: AuditDecision::Allowed,
                reason: "policy_match".to_owned(),
                outcome: Some(AuditOutcome::Succeeded),
                argument_count: None,
                executable_basename: None,
                byte_count: None,
            }],
            retention_gap: false,
            oldest_available_audit_id: Some(1),
            newest_available_audit_id: Some(73),
            next_after_audit_id: None,
        };
        let mut expected = fixture_document(include_str!(
            "../../../tests/automation/fixtures/valid/cli-audit-inspect.json"
        ));
        expected["request_id"] = serde_json::json!("1");
        assert_eq!(serialized(&audit_page_envelope(&page).unwrap()), expected);
    }

    #[test]
    fn public_v2_protocol_failures_preserve_topology_revision() {
        let error = splinterm_protocol::ProtocolError {
            code: ErrorCode::StaleTopology,
            message: "topology revision is stale".to_owned(),
            current_topology_revision: Some(TopologyRevision::new(9)),
            handoff_blocking_reasons: Vec::new(),
            pending_request_id: None,
        };
        let document = serialized(
            &CliEnvelopeV2::protocol_failure(
                "set_split_ratio",
                &error,
                "splinterd [staletopology]: topology revision is stale",
            )
            .unwrap(),
        );
        assert_eq!(document["error"]["code"], "stale_topology");
        assert_eq!(document["error"]["current_topology_revision"], 9);
        assert_eq!(document["truncated"], false);
    }

    #[test]
    fn public_v2_ping_dtos_match_golden_fixtures() {
        let success = PingEnvelopeV2::success(1).unwrap();
        assert_eq!(
            serialized(&success),
            fixture_document(include_str!(
                "../../../tests/automation/fixtures/valid/cli-ping-success.json"
            ))
        );
        let failure =
            PingEnvelopeV2::failure(1, CliErrorCodeV2::Timeout, "request deadline elapsed", true)
                .unwrap();
        assert_eq!(
            serialized(&failure),
            fixture_document(include_str!(
                "../../../tests/automation/fixtures/valid/cli-ping-timeout.json"
            ))
        );
        assert!(PingEnvelopeV2::success(0).is_err());
        assert!(PingEnvelopeV2::failure(1, CliErrorCodeV2::Internal, "", false).is_err());
    }

    #[test]
    fn public_v2_initial_event_dtos_match_golden_fixtures() {
        let splint_id = "018f4d8c-2a18-4b31-8c2f-9e7c5de77110";
        let terminal = CliEventV2(CliEventDataV2::TerminalSnapshot {
            schema: CLI_EVENT_SCHEMA_V2,
            subscription_id: "1".to_owned(),
            sequence: 1,
            event_type: "snapshot",
            resource: TerminalResourceV2 {
                splint_id: splint_id.to_owned(),
                incarnation: 3,
                terminal_revision: 42,
                history_generation: None,
            },
            data: TerminalSnapshotV2 {
                content_encoding: "unicode_scalars",
                columns: 2,
                rows: 1,
                title: "build".to_owned(),
                visible_rows: vec![ProjectedTerminalRow {
                    linebreak: false,
                    cells: vec![
                        ProjectedTerminalCell {
                            text: "A".to_owned(),
                            width: 1,
                        },
                        ProjectedTerminalCell {
                            text: "界".to_owned(),
                            width: 2,
                        },
                    ],
                }],
            },
            truncated: false,
        });
        assert_eq!(
            serialized(&terminal),
            fixture_document(include_str!(
                "../../../tests/automation/fixtures/valid/subscription-terminal-snapshot.json"
            ))
        );

        let topology = CliEventV2(CliEventDataV2::TopologySnapshot {
            schema: CLI_EVENT_SCHEMA_V2,
            subscription_id: "1".to_owned(),
            sequence: 1,
            event_type: "topology_snapshot",
            resource: TopologyResourceV2 {
                topology_revision: 7,
            },
            data: TopologySnapshotV2 {
                lair_count: 1,
                dojo_count: 2,
                splint_count: 3,
            },
            truncated: false,
        });
        assert_eq!(
            serialized(&topology),
            fixture_document(include_str!(
                "../../../tests/automation/fixtures/valid/subscription-topology-snapshot.json"
            ))
        );

        let control = CliEventV2(CliEventDataV2::ControlSnapshot {
            schema: CLI_EVENT_SCHEMA_V2,
            subscription_id: "1".to_owned(),
            sequence: 1,
            event_type: "control_snapshot",
            resource: ControlResourceV2 {
                splint_id: splint_id.to_owned(),
                incarnation: 3,
            },
            data: ControlSnapshotV2 {
                controlled: true,
                locally_owned: false,
            },
            truncated: false,
        });
        assert_eq!(
            serialized(&control),
            fixture_document(include_str!(
                "../../../tests/automation/fixtures/valid/subscription-control-snapshot.json"
            ))
        );
    }

    #[test]
    fn public_v2_update_event_dtos_match_closed_fixtures() {
        let splint_id: SplintId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77110".parse().unwrap();
        assert_eq!(
            serialized(&CliEventV2::terminal_update(1, 2, splint_id, 3, 43, 2).unwrap()),
            fixture_document(include_str!(
                "../../../tests/automation/fixtures/valid/subscription-terminal-update.json"
            ))
        );
        assert_eq!(
            serialized(
                &CliEventV2::control_transfer_resolved(
                    1,
                    2,
                    splint_id,
                    3,
                    9,
                    ControlTransferOutcome::Granted,
                )
                .unwrap()
            ),
            fixture_document(include_str!(
                "../../../tests/automation/fixtures/valid/subscription-control-transfer-resolved.json"
            ))
        );
        let snapshot = reviewed_topology();
        let actual = serialized(
            &CliEventV2::topology_changed(1, 2, TopologyChangeKind::SplintSplit, &snapshot)
                .unwrap(),
        );
        let mut expected = fixture_document(include_str!(
            "../../../tests/automation/fixtures/valid/subscription-topology-changed.json"
        ));
        expected["resource"]["topology_revision"] = serde_json::json!(1);
        expected["data"]["dojo_count"] = serde_json::json!(1);
        expected["data"]["splint_count"] = serde_json::json!(1);
        assert_eq!(actual, expected);
    }

    #[test]
    fn public_v2_resync_dtos_match_golden_fixtures() {
        let splint_id: SplintId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77110".parse().unwrap();
        let terminal = CliEventV2::terminal_resync(
            1,
            4,
            splint_id,
            3,
            45,
            Some(2),
            ResyncReasonV2::HistoryReplaced,
        )
        .unwrap();
        assert_eq!(
            serialized(&terminal),
            fixture_document(include_str!(
                "../../../tests/automation/fixtures/valid/subscription-terminal-resync.json"
            ))
        );
        let topology = CliEventV2::topology_resync(
            1,
            4,
            TopologyRevision::new(9),
            ResyncReasonV2::RevisionGap,
        )
        .unwrap();
        assert_eq!(
            serialized(&topology),
            fixture_document(include_str!(
                "../../../tests/automation/fixtures/valid/subscription-topology-resync.json"
            ))
        );
        let control =
            CliEventV2::control_resync(1, 4, splint_id, 3, ResyncReasonV2::SubscriberStalled)
                .unwrap();
        assert_eq!(
            serialized(&control),
            fixture_document(include_str!(
                "../../../tests/automation/fixtures/valid/subscription-control-resync.json"
            ))
        );
        assert!(
            CliEventV2::control_resync(1, 4, splint_id, 3, ResyncReasonV2::HistoryReplaced)
                .is_err()
        );
    }

    #[tokio::test]
    async fn requestless_daemon_error_preserves_reload_diagnostic() {
        async fn serve_reload_error(mut server: UnixStream, expect_request: bool) {
            assert!(matches!(
                read_client_frame(&mut server).await,
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
            if expect_request {
                assert!(matches!(
                    read_client_frame(&mut server).await,
                    ClientFrame::Request {
                        request: Request::Ping,
                        ..
                    }
                ));
            }
            server
                .write_all(
                    &encode_frame(&ServerFrame::Error {
                        request_id: None,
                        error: ProtocolError::new(
                            ErrorCode::Unauthorized,
                            "persistent policy reloaded; reconnect required",
                        ),
                    })
                    .unwrap(),
                )
                .await
                .unwrap();
        }

        let (client, server) = UnixStream::pair().unwrap();
        let server_task = tokio::spawn(serve_reload_error(server, true));
        let mut connection = Connection::connect_stream(client, ClientRole::Automation)
            .await
            .unwrap();
        let error = connection.request(Request::Ping).await.unwrap_err();
        assert_eq!(
            protocol_error(&error).map(|error| (error.code, error.message.as_str())),
            Some((
                ErrorCode::Unauthorized,
                "persistent policy reloaded; reconnect required"
            ))
        );
        let unusable = connection.request(Request::Ping).await.unwrap_err();
        assert!(unusable.to_string().contains("cannot be reused"));
        server_task.await.unwrap();

        let (client, server) = UnixStream::pair().unwrap();
        let server_task = tokio::spawn(serve_reload_error(server, false));
        let mut connection = Connection::connect_stream(client, ClientRole::Automation)
            .await
            .unwrap();
        let error = connection.next_server_frame().await.unwrap_err();
        assert_eq!(
            protocol_error(&error).map(|error| (error.code, error.message.as_str())),
            Some((
                ErrorCode::Unauthorized,
                "persistent policy reloaded; reconnect required"
            ))
        );
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn negotiation_exposes_only_bounded_usable_daemon_hostname() {
        for advertised in [
            None,
            Some("actual-daemon"),
            Some("bad\nname"),
            Some("\u{2067}host"),
            Some(&"x".repeat(256)),
        ] {
            let expected = advertised.and_then(splinterm_protocol::usable_daemon_hostname);
            let frame = ServerFrame::Hello {
                version: PROTOCOL_VERSION,
                limits: ServerLimits::default(),
                development_terminal_access: false,
                daemon_hostname: advertised.map(str::to_owned),
                daemon_generation: 1,
            };
            let (client, mut server) = UnixStream::pair().unwrap();
            let server_task = tokio::spawn(async move {
                let _ = read_client_frame(&mut server).await;
                server
                    .write_all(&encode_frame(&frame).unwrap())
                    .await
                    .unwrap();
            });
            let connection = Connection::connect_stream(client, ClientRole::RemoteInteractive)
                .await
                .unwrap();
            assert_eq!(connection.daemon_hostname(), expected);
            server_task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn negotiation_rejects_daemon_generation_zero() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let server_task = tokio::spawn(async move {
            let _ = read_client_frame(&mut server).await;
            let hello = ServerFrame::Hello {
                version: PROTOCOL_VERSION,
                limits: ServerLimits::default(),
                development_terminal_access: false,
                daemon_hostname: None,
                daemon_generation: 0,
            };
            server
                .write_all(&encode_frame(&hello).unwrap())
                .await
                .unwrap();
            server
        });
        let error = Connection::connect_stream(client, ClientRole::Automation)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("invalid handshake"), "{error:#}");
        drop(server_task.await.unwrap());
    }

    #[tokio::test]
    async fn negotiation_stores_daemon_generation_and_rejects_unexpected_handoff_prepare() {
        use std::os::fd::{AsFd as _, OwnedFd};

        use splinterm_protocol::handoff_prepare::{
            HandoffPrepareDescriptors, encode_handoff_prepare, send_handoff_prepare,
        };

        let (client, mut server) = UnixStream::pair().unwrap();
        let server_task = tokio::spawn(async move {
            let _ = read_client_frame(&mut server).await;
            let hello = ServerFrame::Hello {
                version: PROTOCOL_VERSION,
                limits: ServerLimits::default(),
                development_terminal_access: false,
                daemon_hostname: None,
                daemon_generation: 4,
            };
            server
                .write_all(&encode_frame(&hello).unwrap())
                .await
                .unwrap();
            let status = ServerFrame::Event {
                subscription_id: 1,
                sequence: 1,
                event: SubscriptionEvent::HandoffStatus {
                    daemon_generation: 4,
                    phase: splinterm_protocol::HandoffStatusPhase::Fenced,
                },
            };
            server
                .write_all(&encode_frame(&status).unwrap())
                .await
                .unwrap();
            // A frame with descriptors attached, sent to a client that takes
            // part in no handoff.
            let snapshot = rustix::fs::memfd_create(
                "unexpected-snapshot",
                rustix::fs::MemfdFlags::CLOEXEC | rustix::fs::MemfdFlags::ALLOW_SEALING,
            )
            .unwrap();
            rustix::io::write(&snapshot, b"snapshot").unwrap();
            let (continuation, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
            let encoded = encode_handoff_prepare(&splinterm_protocol::HandoffPrepare {
                daemon_generation: 4,
                prepare_id: 1,
                descriptors: splinterm_protocol::HANDOFF_PREPARE_DESCRIPTORS,
                client_snapshot: splinterm_protocol::HandoffClientSnapshot {
                    device: 1,
                    inode: 2,
                    size: 8,
                    sha256: [3; 32],
                },
            })
            .unwrap();
            let descriptors = HandoffPrepareDescriptors::new(snapshot, OwnedFd::from(continuation));
            server.writable().await.unwrap();
            let sent = send_handoff_prepare(server.as_fd(), &encoded, &descriptors).unwrap();
            server.write_all(&encoded[sent..]).await.unwrap();
            server
        });
        let mut connection = Connection::connect_stream(client, ClientRole::TrustedUi)
            .await
            .unwrap();
        assert_eq!(connection.daemon_generation(), 4);
        assert!(matches!(
            connection.next_server_frame().await.unwrap(),
            ServerFrame::Event {
                event: SubscriptionEvent::HandoffStatus {
                    daemon_generation: 4,
                    ..
                },
                ..
            }
        ));
        let error = connection.next_server_frame().await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("handoff prepare frame outside a handoff"),
            "{error:#}"
        );
        assert!(connection.request(Request::Ping).await.is_err());
        drop(server_task.await.unwrap());
    }

    #[tokio::test]
    async fn trusted_ui_declares_no_window_anchor_and_rejects_an_unexpected_decision_prompt() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let server_task = tokio::spawn(async move {
            // No client may declare an anchor before it can receive a
            // handoff prepare (M3.14).
            assert!(matches!(
                read_client_frame(&mut server).await,
                ClientFrame::Hello {
                    role: ClientRole::TrustedUi,
                    handoff_window: false,
                    ..
                }
            ));
            let hello = ServerFrame::Hello {
                version: PROTOCOL_VERSION,
                limits: ServerLimits::default(),
                development_terminal_access: false,
                daemon_hostname: None,
                daemon_generation: 4,
            };
            let prompt = ServerFrame::HandoffDecisionRequired {
                request_id: 1,
                daemon_generation: 4,
                deadline_ms: 20_000,
                windows: vec![splinterm_protocol::HandoffUnresponsiveWindow {
                    index: 0,
                    pid: 77,
                    reason: splinterm_protocol::HandoffWindowIssue::NoAcknowledgement,
                    dojos: Vec::new(),
                }],
            };
            for frame in [hello, prompt] {
                server
                    .write_all(&encode_frame(&frame).unwrap())
                    .await
                    .unwrap();
            }
            server
        });
        let mut connection = Connection::connect_stream(client, ClientRole::TrustedUi)
            .await
            .unwrap();
        let error = connection.request(Request::Ping).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("handoff decision prompt outside a handoff"),
            "{error:#}"
        );
        assert!(connection.request(Request::Ping).await.is_err());
        drop(server_task.await.unwrap());
    }

    #[tokio::test]
    async fn negotiation_stores_limits_and_rejects_error() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let expected_limits = ServerLimits {
            maximum_input_bytes: 17,
            maximum_columns: 240,
            maximum_rows: 80,
            ..ServerLimits::default()
        };
        let server_task = tokio::spawn(async move {
            assert!(matches!(
                read_client_frame(&mut server).await,
                ClientFrame::Hello { .. }
            ));
            let frame = ServerFrame::Hello {
                version: PROTOCOL_VERSION,
                limits: expected_limits,
                development_terminal_access: false,
                daemon_hostname: None,
                daemon_generation: 1,
            };
            server
                .write_all(&encode_frame(&frame).unwrap())
                .await
                .unwrap();
        });
        let connection = Connection::connect_stream(client, ClientRole::Automation)
            .await
            .unwrap();
        assert_eq!(connection.limits(), expected_limits);
        server_task.await.unwrap();

        let (client, mut server) = UnixStream::pair().unwrap();
        let server_task = tokio::spawn(async move {
            let _ = read_client_frame(&mut server).await;
            let frame = ServerFrame::Error {
                request_id: None,
                error: ProtocolError::new(ErrorCode::IncompatibleVersion, "wrong version"),
            };
            server
                .write_all(&encode_frame(&frame).unwrap())
                .await
                .unwrap();
        });
        let error = Connection::connect_stream(client, ClientRole::Automation)
            .await
            .unwrap_err();
        assert_eq!(
            protocol_error(&error).map(|error| error.code),
            Some(ErrorCode::IncompatibleVersion)
        );
        server_task.await.unwrap();

        let (client, mut server) = UnixStream::pair().unwrap();
        let server_task = tokio::spawn(async move {
            let _ = read_client_frame(&mut server).await;
            let frame = ServerFrame::Hello {
                version: PROTOCOL_VERSION - 1,
                limits: ServerLimits::default(),
                development_terminal_access: false,
                daemon_hostname: None,
                daemon_generation: 1,
            };
            server
                .write_all(&encode_frame(&frame).unwrap())
                .await
                .unwrap();
        });
        let error = Connection::connect_stream(client, ClientRole::Automation)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("invalid handshake"));
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn negotiation_rejects_invalid_terminal_transport_limits() {
        for limits in [
            ServerLimits {
                maximum_frame_bytes: 0,
                ..ServerLimits::default()
            },
            ServerLimits {
                maximum_frame_bytes: MAX_FRAME_BYTES - 1,
                ..ServerLimits::default()
            },
            ServerLimits {
                maximum_columns: 1,
                ..ServerLimits::default()
            },
            ServerLimits {
                maximum_rows: splinterm_protocol::MAX_ROWS + 1,
                ..ServerLimits::default()
            },
        ] {
            let (client, mut server) = UnixStream::pair().unwrap();
            let server_task = tokio::spawn(async move {
                let _ = read_client_frame(&mut server).await;
                let frame = ServerFrame::Hello {
                    version: PROTOCOL_VERSION,
                    limits,
                    development_terminal_access: false,
                    daemon_hostname: None,
                    daemon_generation: 1,
                };
                server
                    .write_all(&encode_frame(&frame).unwrap())
                    .await
                    .unwrap();
            });
            let error = Connection::connect_stream(client, ClientRole::Automation)
                .await
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("invalid terminal transport limits")
            );
            server_task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn remote_interactive_transport_negotiates_the_human_role_without_local_ui_authority() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let (reader, writer) = client.into_split();
        let daemon = tokio::spawn(async move {
            assert_eq!(
                read_client_frame(&mut server).await,
                ClientFrame::Hello {
                    minimum_version: PROTOCOL_VERSION,
                    maximum_version: PROTOCOL_VERSION,
                    role: ClientRole::RemoteInteractive,
                    handoff_window: false,
                }
            );
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
        });

        let connection = Connection::connect_remote_interactive_transport(reader, writer)
            .await
            .unwrap();
        assert!(!connection.trusted_ui);
        daemon.await.unwrap();
    }

    #[tokio::test]
    async fn split_transport_negotiates_only_the_automation_role() {
        let (client, mut server) = tokio::io::duplex(64 * 1024);
        let (reader, writer) = tokio::io::split(client);
        let server_task = tokio::spawn(async move {
            assert!(matches!(
                read_client_frame(&mut server).await,
                ClientFrame::Hello {
                    role: ClientRole::Automation,
                    ..
                }
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
        });
        let connection = Connection::connect_automation_transport(reader, writer)
            .await
            .unwrap();
        assert!(!connection.trusted_ui);
        assert!(connection.socket_path.is_none());
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn buffered_reader_rejects_oversized_truncated_and_eof_frames() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let mut connection = established(client);
        server
            .write_all(&u32::try_from(MAX_FRAME_BYTES + 1).unwrap().to_be_bytes())
            .await
            .unwrap();
        assert!(
            connection
                .next_server_frame()
                .await
                .unwrap_err()
                .to_string()
                .contains("invalid frame length")
        );

        let (client, mut server) = UnixStream::pair().unwrap();
        let mut connection = established(client);
        server.write_all(&10_u32.to_be_bytes()).await.unwrap();
        server.write_all(b"{}").await.unwrap();
        drop(server);
        assert!(
            connection
                .next_server_frame()
                .await
                .unwrap_err()
                .to_string()
                .contains("partial frame")
        );

        let (client, server) = UnixStream::pair().unwrap();
        let mut connection = established(client);
        drop(server);
        assert!(
            connection
                .next_server_frame()
                .await
                .unwrap_err()
                .to_string()
                .contains("closed the connection")
        );
    }

    #[tokio::test]
    async fn request_queues_events_and_rejects_mismatched_response_ids() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let mut connection = established(client);
        let server_task = tokio::spawn(async move {
            assert!(matches!(
                read_client_frame(&mut server).await,
                ClientFrame::Request { request_id: 1, .. }
            ));
            let event = ServerFrame::Event {
                subscription_id: 4,
                sequence: 1,
                event: SubscriptionEvent::AccessRevoked { grant_id: 9 },
            };
            server
                .write_all(&encode_frame(&event).unwrap())
                .await
                .unwrap();
            server
                .write_all(
                    &encode_frame(&ServerFrame::Response {
                        request_id: 1,
                        result: Response::Pong,
                    })
                    .unwrap(),
                )
                .await
                .unwrap();
        });
        assert_eq!(
            connection.request(Request::Ping).await.unwrap(),
            Response::Pong
        );
        assert!(matches!(
            connection.next_server_frame().await.unwrap(),
            ServerFrame::Event {
                subscription_id: 4,
                ..
            }
        ));
        server_task.await.unwrap();

        let (client, mut server) = UnixStream::pair().unwrap();
        let mut connection = established(client);
        let server_task = tokio::spawn(async move {
            let _ = read_client_frame(&mut server).await;
            server
                .write_all(
                    &encode_frame(&ServerFrame::Response {
                        request_id: 2,
                        result: Response::Pong,
                    })
                    .unwrap(),
                )
                .await
                .unwrap();
        });
        assert!(
            connection
                .request(Request::Ping)
                .await
                .unwrap_err()
                .to_string()
                .contains("wrong request id")
        );
        assert!(connection.request(Request::Ping).await.is_err());
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn request_rejects_unbounded_event_queue() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let mut connection = established(client);
        let server_task = tokio::spawn(async move {
            let _ = read_client_frame(&mut server).await;
            for sequence in 1..=MAX_QUEUED_EVENTS + 1 {
                let event = ServerFrame::Event {
                    subscription_id: 4,
                    sequence: u64::try_from(sequence).unwrap(),
                    event: SubscriptionEvent::AccessRevoked { grant_id: 9 },
                };
                server
                    .write_all(&encode_frame(&event).unwrap())
                    .await
                    .unwrap();
            }
        });
        let error = connection.request(Request::Ping).await.unwrap_err();
        assert!(error.to_string().contains("too many events"));
        assert!(connection.request(Request::Ping).await.is_err());
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn pre_cancelled_and_zero_deadline_requests_emit_no_frame() {
        for (cancellation, deadline, expected) in [
            {
                let cancellation = CancellationToken::new();
                cancellation.cancel();
                (
                    cancellation,
                    Duration::from_secs(1),
                    RequestCancellation::Cancelled,
                )
            },
            (
                CancellationToken::new(),
                Duration::ZERO,
                RequestCancellation::DeadlineElapsed,
            ),
        ] {
            let (client, mut server) = UnixStream::pair().unwrap();
            let mut connection = established(client);
            let error = connection
                .request_with_cancellation(Request::Ping, deadline, &cancellation)
                .await
                .unwrap_err();
            assert_eq!(request_cancellation(&error), Some(expected));
            let mut byte = [0_u8; 1];
            assert!(
                tokio::time::timeout(Duration::from_millis(20), server.read(&mut byte))
                    .await
                    .is_err(),
                "preflight rejection emitted a frame or closed the connection"
            );
            assert_eq!(connection.next_request, 1);
            assert!(connection.reader.is_some());
            assert!(connection.writer.is_some());
        }
    }

    #[tokio::test]
    async fn cancellation_token_sends_one_cancel_and_discards_queued_frames() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let mut connection = established(client);
        let cancellation = CancellationToken::new();
        let cancel_request = cancellation.clone();
        let server_task = tokio::spawn(async move {
            assert!(matches!(
                read_client_frame(&mut server).await,
                ClientFrame::Request { request_id: 1, .. }
            ));
            let event = ServerFrame::Event {
                subscription_id: 7,
                sequence: 1,
                event: SubscriptionEvent::AccessRevoked { grant_id: 9 },
            };
            server
                .write_all(&encode_frame(&event).unwrap())
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
            cancel_request.cancel();
            assert_eq!(
                read_client_frame(&mut server).await,
                ClientFrame::Cancel { request_id: 1 }
            );
            let mut byte = [0_u8; 1];
            assert_eq!(server.read(&mut byte).await.unwrap(), 0);
        });
        let error = connection
            .request_with_cancellation(Request::Ping, Duration::from_secs(1), &cancellation)
            .await
            .unwrap_err();
        assert_eq!(
            request_cancellation(&error),
            Some(RequestCancellation::Cancelled)
        );
        assert!(connection.queued_events.is_empty());
        assert!(connection.read_buffer.is_empty());
        assert!(connection.reader.is_none());
        assert!(connection.writer.is_none());
        assert!(connection.request(Request::Ping).await.is_err());
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn graphical_correlation_is_carried_on_each_request_frame() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let mut connection = established(client);
        let correlation = splinterm_protocol::DiagnosticCorrelation {
            client_instance_id: "71d82a68-11e8-47c4-9193-dd83f4b03f1a".parse().unwrap(),
            window_id: "727b26c3-2b28-4ea2-b94a-2bbfb8ce74f1".parse().unwrap(),
        };
        connection.set_diagnostic_correlation(correlation);
        let server_task = tokio::spawn(async move {
            assert!(matches!(
                read_client_frame(&mut server).await,
                ClientFrame::Request {
                    request_id: 1,
                    diagnostic_correlation: Some(observed),
                    request: Request::Ping,
                } if observed == correlation
            ));
            server
                .write_all(
                    &encode_frame(&ServerFrame::Response {
                        request_id: 1,
                        result: Response::Pong,
                    })
                    .unwrap(),
                )
                .await
                .unwrap();
        });
        assert_eq!(
            connection.request(Request::Ping).await.unwrap(),
            Response::Pong
        );
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn dropped_request_future_closes_connection_owned_subscription() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let mut connection = established(client);
        let topology = reviewed_topology();
        let (request_seen_tx, request_seen_rx) = tokio::sync::oneshot::channel();
        let server_task = tokio::spawn(async move {
            assert!(matches!(
                read_client_frame(&mut server).await,
                ClientFrame::Request {
                    request_id: 1,
                    request: Request::SubscribeTopology,
                    ..
                }
            ));
            server
                .write_all(
                    &encode_frame(&ServerFrame::Response {
                        request_id: 1,
                        result: Response::TopologySubscribed {
                            subscription_id: 7,
                            snapshot: topology,
                        },
                    })
                    .unwrap(),
                )
                .await
                .unwrap();
            assert!(matches!(
                read_client_frame(&mut server).await,
                ClientFrame::Request { request_id: 2, .. }
            ));
            request_seen_tx.send(()).unwrap();
            assert_eq!(
                read_client_frame(&mut server).await,
                ClientFrame::Cancel { request_id: 2 }
            );
            // A response racing cancellation is never observed because the
            // client has already disposed the complete connection.
            let _ = server
                .write_all(
                    &encode_frame(&ServerFrame::Response {
                        request_id: 2,
                        result: Response::Pong,
                    })
                    .unwrap(),
                )
                .await;
            let mut byte = [0_u8; 1];
            assert_eq!(server.read(&mut byte).await.unwrap(), 0);
        });
        assert!(matches!(
            connection
                .request(Request::SubscribeTopology)
                .await
                .unwrap(),
            Response::TopologySubscribed {
                subscription_id: 7,
                ..
            }
        ));

        let mut request = Box::pin(connection.request(Request::Ping));
        tokio::select! {
            result = &mut request => panic!("request unexpectedly completed: {result:?}"),
            result = request_seen_rx => result.unwrap(),
        }
        drop(request);

        assert!(connection.reader.is_none());
        assert!(connection.writer.is_none());
        assert!(connection.queued_events.is_empty());
        assert!(connection.read_buffer.is_empty());
        assert!(connection.request(Request::Ping).await.is_err());
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn timeout_sends_cancel_closes_and_prevents_reuse() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let mut connection = established(client);
        let server_task = tokio::spawn(async move {
            assert!(matches!(
                read_client_frame(&mut server).await,
                ClientFrame::Request { request_id: 1, .. }
            ));
            assert_eq!(
                read_client_frame(&mut server).await,
                ClientFrame::Cancel { request_id: 1 }
            );
            let mut byte = [0_u8; 1];
            assert_eq!(server.read(&mut byte).await.unwrap(), 0);
        });
        let error = connection
            .request_with_deadline(Request::Ping, Duration::from_millis(10))
            .await
            .unwrap_err();
        assert_eq!(
            request_cancellation(&error),
            Some(RequestCancellation::DeadlineElapsed)
        );
        assert!(error.to_string().contains("timed out"));
        assert!(
            connection
                .request(Request::Ping)
                .await
                .unwrap_err()
                .to_string()
                .contains("cannot be reused")
        );
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn buffered_reader_consumes_coalesced_frames_without_front_draining() {
        let (client, _server) = UnixStream::pair().unwrap();
        let mut connection = established(client);
        let first = ServerFrame::Hello {
            version: PROTOCOL_VERSION,
            limits: ServerLimits::default(),
            development_terminal_access: true,
            daemon_hostname: None,
            daemon_generation: 1,
        };
        let second = ServerFrame::Response {
            request_id: 7,
            result: Response::Pong,
        };
        let first_bytes = encode_frame(&first).unwrap();
        let second_bytes = encode_frame(&second).unwrap();
        connection.read_buffer.extend_from_slice(&first_bytes);
        connection.read_buffer.extend_from_slice(&second_bytes);

        assert_eq!(connection.next_server_frame().await.unwrap(), first);
        assert_eq!(connection.read_offset, first_bytes.len());
        assert_eq!(
            connection.read_buffer.len(),
            first_bytes.len() + second_bytes.len()
        );
        assert_eq!(connection.next_server_frame().await.unwrap(), second);
        assert!(connection.read_buffer.is_empty());
        assert_eq!(connection.read_offset, 0);
    }

    #[tokio::test]
    async fn buffered_reader_compacts_partial_trailing_frame_before_resuming() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let mut connection = established(client);
        let first = ServerFrame::Response {
            request_id: 1,
            result: Response::Pong,
        };
        let second = ServerFrame::Response {
            request_id: 2,
            result: Response::Pong,
        };
        let first_bytes = encode_frame(&first).unwrap();
        let second_bytes = encode_frame(&second).unwrap();
        let partial = 5;
        connection.read_buffer.extend_from_slice(&first_bytes);
        connection
            .read_buffer
            .extend_from_slice(&second_bytes[..partial]);

        assert_eq!(connection.next_server_frame().await.unwrap(), first);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), connection.next_server_frame())
                .await
                .is_err()
        );
        assert_eq!(connection.read_offset, 0);
        assert_eq!(connection.read_buffer, second_bytes[..partial]);

        server.write_all(&second_bytes[partial..]).await.unwrap();
        assert_eq!(connection.next_server_frame().await.unwrap(), second);
        assert!(connection.read_buffer.is_empty());
        assert_eq!(connection.read_offset, 0);
    }

    #[tokio::test]
    async fn buffered_read_resumes_after_outer_cancellation() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let expected = ServerFrame::Hello {
            version: PROTOCOL_VERSION,
            limits: ServerLimits::default(),
            development_terminal_access: true,
            daemon_hostname: None,
            daemon_generation: 1,
        };
        let encoded = encode_frame(&expected).unwrap();
        let (prefix_sent, prefix_received) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
        let writer = tokio::spawn(async move {
            server.write_all(&encoded[..4]).await.unwrap();
            prefix_sent.send(()).unwrap();
            resume_rx.await.unwrap();
            server.write_all(&encoded[4..]).await.unwrap();
        });
        prefix_received.await.unwrap();
        let mut connection = established(client);

        assert!(
            tokio::time::timeout(Duration::from_millis(10), connection.next_server_frame())
                .await
                .is_err()
        );
        assert_eq!(connection.read_buffer.len(), 4);
        resume_tx.send(()).unwrap();
        assert_eq!(connection.next_server_frame().await.unwrap(), expected);
        assert!(connection.read_buffer.is_empty());
        writer.await.unwrap();
    }
}
