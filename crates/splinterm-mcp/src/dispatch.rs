use std::{
    collections::{BTreeSet, HashMap, HashSet, VecDeque, hash_map::RandomState},
    env,
    hash::BuildHasher,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use anyhow::Error;
use serde_json::{Value, json};
use splinterm_automation_client::{
    Connection, is_connection_loss, project_terminal_rows, protocol_error, request_cancellation,
};
use splinterm_core::{Axis, DojoId, LairId, LayoutNode, SplintId, SplitRatio, SplitSide};
use splinterm_protocol::{
    AccessGrant, AccessGrantSource, AccessScope, AgentState, AgentStatusReport, AuditDecision,
    AuditOutcome, AutomationLaunch, ErrorCode, MAX_SCROLLBACK_PAGE_ROWS, MAX_SEARCH_QUERY_BYTES,
    MAX_SEARCH_RESULTS, MutationPreflight, MutationPreparation, Request, Response,
    RestoreLeafResult, ScrollbackPage, SearchPage, SplintLifecycle, SplintRuntimeSummary,
    TerminalProvenance, TerminalSnapshot, TopologySnapshot,
};
use tokio_util::sync::CancellationToken;

const SCHEMA: &str = "splinterm.mcp.v2";
const DEFAULT_DEADLINE: Duration = Duration::from_secs(5);
const MINIMUM_DEADLINE_MS: u64 = 100;
const MAXIMUM_DEADLINE_MS: u64 = 30_000;
const DEFAULT_PAGE_SIZE: usize = 64;
const MAXIMUM_MCP_PAGE: usize = 256;
const MAXIMUM_AUDIT_PAGE: usize = 128;
const MAXIMUM_TERMINAL_CURSORS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
enum TerminalCursorState {
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
        query_binding: [u64; 2],
        case_sensitive: bool,
    },
}

impl TerminalCursorState {
    fn splint_id(&self) -> SplintId {
        match self {
            Self::Scrollback { splint_id, .. } | Self::Search { splint_id, .. } => *splint_id,
        }
    }

    fn is_search(&self) -> bool {
        matches!(self, Self::Search { .. })
    }
}

/// Process-owned, bounded terminal continuation state.
///
/// Public cursor strings contain only a counter and two independently keyed
/// `RandomState` tags. Paging provenance and the keyed search-query binding stay
/// inside this MCP process, and the oldest unconsumed cursor expires when the
/// fixed registry bound is reached.
#[derive(Debug)]
pub(crate) struct CursorRegistry {
    entries: HashMap<String, TerminalCursorState>,
    order: VecDeque<String>,
    next_counter: u64,
    token_hashers: [RandomState; 2],
    query_hashers: [RandomState; 2],
}

impl Default for CursorRegistry {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            next_counter: 1,
            token_hashers: [RandomState::new(), RandomState::new()],
            query_hashers: [RandomState::new(), RandomState::new()],
        }
    }
}

impl CursorRegistry {
    fn query_binding(&self, query: &str) -> [u64; 2] {
        [
            self.query_hashers[0].hash_one(("splinterm-mcp-search-v1", query)),
            self.query_hashers[1].hash_one(("splinterm-mcp-search-v1", query)),
        ]
    }

    fn token(&mut self, search: bool) -> Result<String, DispatchFailure> {
        let counter = self.next_counter;
        self.next_counter = self
            .next_counter
            .checked_add(1)
            .ok_or_else(DispatchFailure::resource_limit)?;
        let kind = u8::from(search);
        let first = self.token_hashers[0].hash_one(("splinterm-mcp-cursor-v1", counter, kind));
        let second = self.token_hashers[1].hash_one(("splinterm-mcp-cursor-v1", counter, kind));
        Ok(format!("cur_{counter:016x}{first:016x}{second:016x}"))
    }

    fn insert(&mut self, state: TerminalCursorState) -> Result<String, DispatchFailure> {
        while self.entries.len() >= MAXIMUM_TERMINAL_CURSORS {
            let expired = self
                .order
                .pop_front()
                .ok_or_else(DispatchFailure::internal)?;
            self.entries.remove(&expired);
        }
        let token = self.token(state.is_search())?;
        if self.entries.insert(token.clone(), state).is_some() {
            return Err(DispatchFailure::internal());
        }
        self.order.push_back(token.clone());
        Ok(token)
    }

    fn issue_scrollback(
        &mut self,
        splint_id: SplintId,
        incarnation: u64,
        terminal_revision: u64,
        history_generation: u64,
        before_row_id: u64,
    ) -> Result<String, DispatchFailure> {
        self.insert(TerminalCursorState::Scrollback {
            splint_id,
            incarnation,
            terminal_revision,
            history_generation,
            before_row_id,
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the stored continuation binds exact terminal and search provenance"
    )]
    fn issue_search(
        &mut self,
        splint_id: SplintId,
        incarnation: u64,
        terminal_revision: u64,
        history_generation: u64,
        daemon_cursor: String,
        query: &str,
        case_sensitive: bool,
    ) -> Result<String, DispatchFailure> {
        let query_binding = self.query_binding(query);
        self.insert(TerminalCursorState::Search {
            splint_id,
            incarnation,
            terminal_revision,
            history_generation,
            daemon_cursor,
            query_binding,
            case_sensitive,
        })
    }

    fn take(
        &mut self,
        token: &str,
        expected_splint: SplintId,
        search: bool,
        query: Option<(&str, bool)>,
    ) -> Result<TerminalCursorState, DispatchFailure> {
        let canonical = token.len() == 52
            && token.starts_with("cur_")
            && token[4..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if !canonical {
            return Err(invalid_terminal_argument("the terminal cursor is invalid"));
        }
        let state = self
            .entries
            .get(token)
            .ok_or_else(|| invalid_terminal_argument("the terminal cursor is invalid"))?;
        let valid_query = match (state, query) {
            (
                TerminalCursorState::Search {
                    query_binding,
                    case_sensitive,
                    ..
                },
                Some((query, expected_case)),
            ) => *query_binding == self.query_binding(query) && *case_sensitive == expected_case,
            (TerminalCursorState::Scrollback { .. }, None) => true,
            _ => false,
        };
        if state.splint_id() != expected_splint || state.is_search() != search || !valid_query {
            return Err(invalid_terminal_argument("the terminal cursor is invalid"));
        }
        let state = self
            .entries
            .remove(token)
            .ok_or_else(DispatchFailure::internal)?;
        self.order.retain(|candidate| candidate != token);
        Ok(state)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DispatchFailure {
    pub(crate) code: &'static str,
    pub(crate) message: &'static str,
    pub(crate) retryable: bool,
}

impl DispatchFailure {
    pub(crate) const fn new(code: &'static str, message: &'static str, retryable: bool) -> Self {
        Self {
            code,
            message,
            retryable,
        }
    }

    pub(crate) const fn internal() -> Self {
        Self::new("internal", "the local automation request failed", false)
    }

    pub(crate) const fn resource_limit() -> Self {
        Self::new(
            "resource_limit",
            "the tool response exceeds the adapter limit",
            false,
        )
    }
}

pub(crate) fn deadline() -> Result<Duration, DispatchFailure> {
    let Some(value) = env::var_os("SPLINTERM_MCP_TIMEOUT_MS") else {
        return Ok(DEFAULT_DEADLINE);
    };
    let value = value
        .to_str()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (MINIMUM_DEADLINE_MS..=MAXIMUM_DEADLINE_MS).contains(value))
        .ok_or_else(DispatchFailure::internal)?;
    Ok(Duration::from_millis(value))
}

struct DaemonSession<'a> {
    connection: Connection,
    cancellation: &'a CancellationToken,
    started: tokio::time::Instant,
    deadline: Duration,
}

impl<'a> DaemonSession<'a> {
    async fn connect(cancellation: &'a CancellationToken) -> Result<Self, DispatchFailure> {
        Self::connect_to(cancellation, None).await
    }

    async fn connect_to(
        cancellation: &'a CancellationToken,
        socket: Option<&Path>,
    ) -> Result<Self, DispatchFailure> {
        let deadline = deadline()?;
        let started = tokio::time::Instant::now();
        let connect = async {
            match socket {
                Some(socket) => Connection::connect_automation_at(socket).await,
                None => Connection::connect_automation().await,
            }
        };
        let connection = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                return Err(DispatchFailure::new("cancelled", "the tool call was cancelled", true));
            }
            result = tokio::time::timeout(deadline, connect) => result,
        };
        let connection = match connection {
            Ok(Ok(connection)) => connection,
            Ok(Err(error)) => return Err(map_client_error(&error)),
            Err(_) => {
                return Err(DispatchFailure::new(
                    "timeout",
                    "the local automation deadline elapsed",
                    true,
                ));
            }
        };
        Ok(Self {
            connection,
            cancellation,
            started,
            deadline,
        })
    }

    async fn request(&mut self, request: Request) -> Result<Response, DispatchFailure> {
        self.request_raw(request)
            .await
            .map_err(|error| map_client_error(&error))
    }

    async fn request_raw(&mut self, request: Request) -> Result<Response, Error> {
        let remaining = self.deadline.saturating_sub(self.started.elapsed());
        self.connection
            .request_with_cancellation(request, remaining, self.cancellation)
            .await
    }

    /// How long the daemon may hold a workspace request waiting for the
    /// person (ADR 0014): what is left of this call's deadline less a margin,
    /// so the `pending` reply always arrives before the deadline.
    fn consent_wait_millis(&self) -> u32 {
        consent_wait_budget(self.deadline.saturating_sub(self.started.elapsed()))
    }

    fn scrollback_limit(&self, requested: usize) -> usize {
        requested
            .min(MAXIMUM_MCP_PAGE)
            .min(MAX_SCROLLBACK_PAGE_ROWS)
            .min(self.connection.limits().maximum_snapshot_scrollback_rows)
    }

    fn search_limit(requested: usize) -> usize {
        requested.min(MAXIMUM_MCP_PAGE).min(MAX_SEARCH_RESULTS)
    }
}

async fn daemon_request(
    request: Request,
    cancellation: &CancellationToken,
) -> Result<Response, DispatchFailure> {
    DaemonSession::connect(cancellation)
        .await?
        .request(request)
        .await
}

/// What a tool call whose daemon connection ended says (Plan 0061 M3.12b).
pub(crate) const CONNECTION_LOST_DURING_CALL: &str = "the splinterd connection ended during the call, as when splinterd is upgraded; it was not retried and may or may not have taken effect, so check the current state before calling again";

pub(crate) fn map_client_error(error: &Error) -> DispatchFailure {
    if let Some(reason) = request_cancellation(error) {
        return match reason {
            splinterm_automation_client::RequestCancellation::DeadlineElapsed => {
                DispatchFailure::new("timeout", "the local automation deadline elapsed", true)
            }
            splinterm_automation_client::RequestCancellation::Cancelled => {
                DispatchFailure::new("cancelled", "the tool call was cancelled", true)
            }
        };
    }
    // Plan 0061 M3.12b: a call is never retried after its connection ended;
    // whether it took effect is unknown.
    if is_connection_loss(error) {
        return DispatchFailure::new("internal", CONNECTION_LOST_DURING_CALL, false);
    }
    let Some(error) = protocol_error(error) else {
        return DispatchFailure::internal();
    };
    map_protocol_error(error)
}

/// The tool error for a daemon refusal: a fixed code and message per
/// daemon code, never the daemon's own text.
fn map_protocol_error(error: &splinterm_protocol::ProtocolError) -> DispatchFailure {
    use splinterm_protocol::NO_WORKSPACE_GRANT;

    let (code, message, retryable) = match error.code {
        ErrorCode::AuthenticationFailed => (
            "authentication_failed",
            "daemon authentication failed",
            false,
        ),
        ErrorCode::HandshakeRequired => {
            ("handshake_required", "daemon handshake is required", false)
        }
        ErrorCode::IncompatibleVersion => (
            "incompatible_version",
            "daemon protocol version is incompatible",
            false,
        ),
        ErrorCode::ConsentUnavailable => (
            "consent_unavailable",
            "no Splinterm Window or approver can show the access prompt",
            false,
        ),
        ErrorCode::ConsentDenied => ("consent_denied", "the access request was denied", false),
        // A Lair list or topology read no grant covers: say what to ask for.
        ErrorCode::Unauthorized if error.message == NO_WORKSPACE_GRANT => {
            ("unauthorized", NO_WORKSPACE_GRANT, false)
        }
        ErrorCode::Unauthorized | ErrorCode::DevelopmentFeatureDisabled => (
            "unauthorized",
            "the exact policy scope or resource is not authorized",
            false,
        ),
        ErrorCode::ControllerUnavailable => (
            "controller_unavailable",
            "terminal control is unavailable",
            true,
        ),
        ErrorCode::ControlTransferUnavailable => (
            "control_transfer_unavailable",
            "control transfer is unavailable",
            true,
        ),
        ErrorCode::StaleTopology => ("stale_topology", "the topology revision is stale", true),
        ErrorCode::NotFound | ErrorCode::RequestNotFound | ErrorCode::ImageContentNotFound => {
            ("not_found", "the requested resource was not found", false)
        }
        ErrorCode::StaleIncarnation | ErrorCode::StaleImageContent => (
            "stale_incarnation",
            "the requested Splint incarnation is stale",
            true,
        ),
        ErrorCode::InvalidArgument
        | ErrorCode::InvalidFrame
        | ErrorCode::FrameTooLarge
        | ErrorCode::InvalidRequestId
        | ErrorCode::DuplicateRequestId
        | ErrorCode::UnsupportedOperation
        // Trusted-UI-only handoff outcomes; automation is refused before them.
        | ErrorCode::HandoffUnavailable
        | ErrorCode::InvalidHandoffTicket => {
            ("invalid_argument", "the daemon rejected the request", false)
        }
        ErrorCode::TooManyOutstandingRequests | ErrorCode::ResourceLimit => (
            "resource_limit",
            "the local automation resource limit was reached",
            true,
        ),
        ErrorCode::RateLimited => (
            "resource_limit",
            "the daemon request rate limit was reached; retry later",
            true,
        ),
        ErrorCode::HandoffInProgress => (
            "resource_limit",
            "the daemon is upgrading; retry later",
            true,
        ),
        ErrorCode::Cancelled if error.message == splinterm_protocol::ACCESS_REQUEST_EXPIRED => (
            "cancelled",
            "the access prompt expired without an answer and nothing was granted; asking again within a few seconds fails with resource_limit",
            true,
        ),
        ErrorCode::Cancelled => ("cancelled", "the daemon request was cancelled", true),
        ErrorCode::Internal => ("internal", "the local automation request failed", false),
    };
    DispatchFailure::new(code, message, retryable)
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "closed result values are moved conceptually into the serialized envelope"
)]
fn success(tool: &str, resource: Value, data: Value, truncated: bool, trust: &str) -> Value {
    json!({
        "schema": SCHEMA,
        "tool": tool,
        "ok": true,
        "resource": resource,
        "data": data,
        "truncated": truncated,
        "content_trust": trust,
    })
}

fn parse_splint(arguments: &Value) -> Result<SplintId, DispatchFailure> {
    arguments["splint_id"]
        .as_str()
        .and_then(|value| value.parse().ok())
        .ok_or_else(DispatchFailure::internal)
}

fn parse_incarnation(arguments: &Value) -> Result<u64, DispatchFailure> {
    arguments["incarnation"]
        .as_u64()
        .filter(|value| *value > 0)
        .ok_or_else(DispatchFailure::internal)
}

fn parse_grant_id(arguments: &Value) -> Result<u64, DispatchFailure> {
    arguments["grant_id"]
        .as_str()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .ok_or_else(DispatchFailure::internal)
}

/// The largest margin kept between a workspace request's wait budget and the
/// call's own deadline.
const CONSENT_WAIT_MARGIN: Duration = Duration::from_secs(1);

/// The wait budget for `remaining` time: at most a quarter of it, and at
/// most [`CONSENT_WAIT_MARGIN`], is kept back for the reply, and the daemon's
/// 25-second cap applies.
pub(crate) fn consent_wait_budget(remaining: Duration) -> u32 {
    let margin = (remaining / 4).min(CONSENT_WAIT_MARGIN);
    let budget = remaining.saturating_sub(margin).as_millis();
    u32::try_from(budget)
        .unwrap_or(u32::MAX)
        .min(splinterm_protocol::MAX_CONSENT_WAIT_MILLIS)
}

/// The host-reported MCP client name from `initialize`: shown in the consent
/// prompt as data, never an identity.
static CLIENT_NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Records the host's `clientInfo.name` once, bounded and with control
/// characters removed.
pub(crate) fn record_client_name(name: &str) {
    let name: String = name
        .chars()
        .filter(|character| !character.is_control())
        .take(splinterm_protocol::MAX_AGENT_NAME_CHARS)
        .collect();
    let name = name.trim();
    if !name.is_empty() {
        let _ = CLIENT_NAME.set(name.to_owned());
    }
}

/// The pending request a cancel names: a decimal identifier that fits 64
/// bits. One that only fits the schema's pattern is an invalid argument.
fn parse_cancel_request_id(arguments: &Value) -> Result<u64, DispatchFailure> {
    arguments["cancel_request_id"]
        .as_str()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .ok_or(DispatchFailure::new(
            "invalid_argument",
            "cancel_request_id is not a pending request identifier",
            false,
        ))
}

fn level_from_name(value: &str) -> Option<splinterm_protocol::AccessLevel> {
    match value {
        "look" => Some(splinterm_protocol::AccessLevel::Look),
        "look_and_type" => Some(splinterm_protocol::AccessLevel::LookAndType),
        "manage" => Some(splinterm_protocol::AccessLevel::Manage),
        _ => None,
    }
}

/// `splinterm.request_lair_access`: asks for a level or scopes, or cancels
/// this adapter's pending request. A request the person has not answered
/// within the call's wait budget returns `pending`; calling again with the
/// same Lair and access collects the answer.
async fn request_lair_access(
    tool: &str,
    arguments: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, DispatchFailure> {
    let lair_id = parse_lair(arguments)?;
    let level = arguments.get("level").and_then(Value::as_str);
    let scopes = arguments.get("scopes").and_then(Value::as_array);
    let cancel = arguments.get("cancel_request_id");
    let request = match (level, scopes, cancel) {
        (None, None, Some(_)) => {
            let request_id = parse_cancel_request_id(arguments)?;
            return cancel_lair_access(tool, lair_id, request_id, cancellation).await;
        }
        (Some(level), None, None) => (
            Some(level_from_name(level).ok_or_else(DispatchFailure::internal)?),
            Vec::new(),
        ),
        (None, Some(scopes), None) => (
            None,
            scopes
                .iter()
                .map(|scope| scope.as_str().and_then(scope_from_name))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| {
                    DispatchFailure::new(
                        "invalid_argument",
                        "request_lair_access contains an unsupported scope",
                        false,
                    )
                })?,
        ),
        _ => {
            return Err(DispatchFailure::new(
                "invalid_argument",
                "request_lair_access takes exactly one of level, scopes, or cancel_request_id",
                false,
            ));
        }
    };
    let (level, scopes) = request;
    let mut session = DaemonSession::connect(cancellation).await?;
    let wait_millis = session.consent_wait_millis();
    let response = session
        .request_raw(Request::RequestLairAccess {
            lair_id,
            level,
            scopes,
            wait_millis,
            agent_name: CLIENT_NAME.get().cloned(),
        })
        .await
        .map_err(|error| {
            if protocol_error(&error).is_some_and(|error| error.pending_request_id.is_some()) {
                DispatchFailure::new(
                    "resource_limit",
                    "a different access request for this Lair is already pending; repeat that request or cancel it first",
                    true,
                )
            } else {
                map_client_error(&error)
            }
        })?;
    match response {
        Response::LairAccessGranted {
            topology_revision,
            authorization_revision,
            grant,
        } if grant.lair_id == lair_id => {
            lair_grant_output(tool, topology_revision, authorization_revision, &grant)
        }
        Response::LairAccessPending {
            lair_id: pending_lair,
            topology_revision,
            request_id,
            expires_at_unix_seconds,
        } if pending_lair == lair_id && request_id != 0 => Ok(success(
            tool,
            json!({
                "kind": "lair",
                "lair_id": lair_id.to_string(),
                "topology_revision": topology_revision.get(),
            }),
            json!({
                "committed": false,
                "status": "pending",
                "pending_request_id": request_id.to_string(),
                "expires_at": expires_at_unix_seconds.to_string(),
            }),
            false,
            "trusted_metadata",
        )),
        _ => Err(DispatchFailure::internal()),
    }
}

/// A Lair grant as the tool reports it: the runtime grant ID for consent
/// grants, the expiry only for "just this once".
fn lair_grant_output(
    tool: &str,
    topology_revision: splinterm_core::TopologyRevision,
    authorization_revision: u64,
    grant: &splinterm_protocol::LairAccessGrant,
) -> Result<Value, DispatchFailure> {
    let granted_scopes = grant
        .scopes
        .iter()
        .copied()
        .map(access_scope_name)
        .collect::<Option<Vec<_>>>()
        .ok_or_else(DispatchFailure::internal)?;
    let (source, grant_id, expires_at) = match grant.source {
        AccessGrantSource::Ephemeral if grant.grant_id != 0 => (
            "ephemeral",
            Some(grant.grant_id.to_string()),
            Some(grant.expires_at_unix_seconds.to_string()),
        ),
        // These last until revoked or the workspace ends.
        AccessGrantSource::WorkspaceSession if grant.grant_id != 0 => {
            ("workspace_session", Some(grant.grant_id.to_string()), None)
        }
        AccessGrantSource::Remembered if grant.grant_id != 0 => {
            ("remembered", Some(grant.grant_id.to_string()), None)
        }
        AccessGrantSource::PersistentPolicy if grant.grant_id == 0 => {
            ("persistent_policy", None, None)
        }
        AccessGrantSource::Development if grant.grant_id == 0 => ("development", None, None),
        _ => return Err(DispatchFailure::internal()),
    };
    Ok(success(
        tool,
        json!({
            "kind": "lair",
            "lair_id": grant.lair_id.to_string(),
            "topology_revision": topology_revision.get(),
            "authorization_revision": authorization_revision,
        }),
        json!({
            "committed": true,
            "source": source,
            "grant_id": grant_id,
            "granted_scopes": granted_scopes,
            "expires_at": expires_at,
        }),
        false,
        "trusted_metadata",
    ))
}

async fn cancel_lair_access(
    tool: &str,
    lair_id: LairId,
    request_id: u64,
    cancellation: &CancellationToken,
) -> Result<Value, DispatchFailure> {
    match daemon_request(
        Request::CancelLairAccessRequest {
            lair_id,
            request_id,
        },
        cancellation,
    )
    .await?
    {
        Response::LairAccessRequestCancelled {
            lair_id: cancelled_lair,
            topology_revision,
            request_id: cancelled,
        } if cancelled == request_id && cancelled_lair == lair_id => Ok(success(
            tool,
            json!({
                "kind": "lair",
                "lair_id": lair_id.to_string(),
                "topology_revision": topology_revision.get(),
            }),
            json!({
                "committed": true,
                "status": "cancelled",
                "pending_request_id": request_id.to_string(),
            }),
            false,
            "trusted_metadata",
        )),
        _ => Err(DispatchFailure::internal()),
    }
}

fn scope_from_name(value: &str) -> Option<AccessScope> {
    match value {
        "terminal_visible_read" => Some(AccessScope::Observe),
        "scrollback_read" => Some(AccessScope::Scrollback),
        "input" => Some(AccessScope::Input),
        "resize" => Some(AccessScope::Resize),
        "process_terminate" => Some(AccessScope::Terminate),
        "controller_transfer" => Some(AccessScope::ControlTakeover),
        "topology_metadata_read" => Some(AccessScope::TopologyObserve),
        "topology_layout_mutate" => Some(AccessScope::TopologyLayout),
        "topology_name_mutate" => Some(AccessScope::TopologyName),
        "process_spawn" => Some(AccessScope::ProcessSpawn),
        "process_restore" => Some(AccessScope::ProcessRestore),
        "agent_status_report" => Some(AccessScope::AgentStatusReport),
        _ => None,
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

fn automation_scope_name(scope: splinterm_protocol::AutomationScope) -> &'static str {
    match scope {
        splinterm_protocol::AutomationScope::TopologyMetadataRead => "topology_metadata_read",
        splinterm_protocol::AutomationScope::TopologySubscribe => "topology_subscribe",
        splinterm_protocol::AutomationScope::TerminalVisibleRead => "terminal_visible_read",
        splinterm_protocol::AutomationScope::TerminalSubscribe => "terminal_subscribe",
        splinterm_protocol::AutomationScope::ScrollbackRead => "scrollback_read",
        splinterm_protocol::AutomationScope::ScrollbackSearch => "scrollback_search",
        splinterm_protocol::AutomationScope::ControllerAcquire => "controller_acquire",
        splinterm_protocol::AutomationScope::ControllerTransfer => "controller_transfer",
        splinterm_protocol::AutomationScope::Input => "input",
        splinterm_protocol::AutomationScope::Resize => "resize",
        splinterm_protocol::AutomationScope::ProcessSpawn => "process_spawn",
        splinterm_protocol::AutomationScope::ProcessRestore => "process_restore",
        splinterm_protocol::AutomationScope::ProcessTerminate => "process_terminate",
        splinterm_protocol::AutomationScope::TopologyLayoutMutate => "topology_layout_mutate",
        splinterm_protocol::AutomationScope::TopologyNameMutate => "topology_name_mutate",
        splinterm_protocol::AutomationScope::AuthorizationInspect => "authorization_inspect",
        splinterm_protocol::AutomationScope::AuthorizationRevoke => "authorization_revoke",
        splinterm_protocol::AutomationScope::AuditInspect => "audit_inspect",
        splinterm_protocol::AutomationScope::AgentStatusReport => "agent_status_report",
    }
}

fn authorization_resource(
    lair_id: LairId,
    dojo_id: DojoId,
    grant: &AccessGrant,
    revision: u64,
) -> Result<Value, DispatchFailure> {
    if grant.grant_id == 0 || grant.incarnation == 0 {
        return Err(DispatchFailure::internal());
    }
    Ok(json!({
        "kind": "authorization",
        "lair_id": lair_id.to_string(),
        "dojo_id": dojo_id.to_string(),
        "splint_id": grant.splint_id.to_string(),
        "incarnation": grant.incarnation,
        "grant_id": grant.grant_id.to_string(),
        "authorization_revision": revision,
    }))
}

fn state_name(runtime: &SplintRuntimeSummary) -> &'static str {
    match runtime.lifecycle {
        SplintLifecycle::Starting | SplintLifecycle::Running => "running",
        SplintLifecycle::Exited if runtime.restorable => "restorable",
        SplintLifecycle::Exited => "exited",
    }
}

fn runtime_for(
    snapshot: &TopologySnapshot,
    splint_id: SplintId,
) -> Result<&SplintRuntimeSummary, DispatchFailure> {
    snapshot
        .runtimes
        .iter()
        .find(|runtime| runtime.splint_id == splint_id)
        .ok_or_else(DispatchFailure::internal)
}

fn topology_splints(
    node: &LayoutNode,
    snapshot: &TopologySnapshot,
    include_agent_status: bool,
    output: &mut Vec<Value>,
) -> Result<(), DispatchFailure> {
    match node {
        LayoutNode::Leaf(splint) => {
            let runtime = runtime_for(snapshot, splint.id)?;
            let mut value = json!({
                "splint_id": splint.id.to_string(),
                "current_incarnation": runtime.live_incarnation,
                "last_incarnation": runtime.last_incarnation,
                "title": splint.title,
                "state": state_name(runtime),
            });
            if include_agent_status && let Some(status) = agent_status_value(runtime)? {
                value["agent_status"] = status;
            }
            output.push(value);
            Ok(())
        }
        LayoutNode::Branch { first, second, .. } => {
            topology_splints(first, snapshot, include_agent_status, output)?;
            topology_splints(second, snapshot, include_agent_status, output)
        }
    }
}

/// Projects explicitly reported display status of a live incarnation only.
fn agent_status_value(runtime: &SplintRuntimeSummary) -> Result<Option<Value>, DispatchFailure> {
    let Some(status) = &runtime.agent_status else {
        return Ok(None);
    };
    if runtime.live_incarnation.is_none() || !status.validate() {
        return Err(DispatchFailure::internal());
    }
    Ok(Some(json!({
        "state": status.state.as_str(),
        "detail": status.detail,
        "stale": status.stale,
        "reported_age_ms": status.reported_age_ms,
        "sequence": status.sequence,
    })))
}

/// Projects the full logical topology. Agent status rides only the
/// `inspect_topology` tool; the topology resource does not carry it because
/// status changes publish no resource update.
pub(crate) fn topology_data(
    snapshot: &TopologySnapshot,
    include_agent_status: bool,
) -> Result<Value, DispatchFailure> {
    snapshot
        .validate()
        .map_err(|_| DispatchFailure::internal())?;
    let mut lairs = Vec::new();
    for lair in snapshot.topology.lairs() {
        let mut dojos = Vec::new();
        for dojo in &lair.dojos {
            let mut splints = Vec::new();
            topology_splints(&dojo.root, snapshot, include_agent_status, &mut splints)?;
            dojos.push(json!({
                "dojo_id": dojo.id.to_string(),
                "name": dojo.name,
                "default_focus_splint_id": dojo.default_focus.to_string(),
                "splints": splints,
            }));
        }
        lairs.push(json!({
            "lair_id": lair.id.to_string(),
            "name": lair.name,
            "dojos": dojos,
        }));
    }
    Ok(json!({"lairs": lairs}))
}

fn invalid_terminal_argument(message: &'static str) -> DispatchFailure {
    DispatchFailure::new("invalid_argument", message, false)
}

fn terminal_resource(
    provenance: &TerminalProvenance,
    requested_splint: SplintId,
    incarnation: u64,
    terminal_revision: u64,
    history_generation: u64,
) -> Result<Value, DispatchFailure> {
    if provenance.splint_id != requested_splint
        || provenance.incarnation != incarnation
        || provenance.terminal_revision != terminal_revision
        || provenance.history_generation != history_generation
        || incarnation == 0
        || history_generation == 0
        || provenance.title.chars().count() > 1_024
    {
        return Err(DispatchFailure::internal());
    }
    Ok(json!({
        "kind": "terminal",
        "lair_id": provenance.lair_id.to_string(),
        "dojo_id": provenance.dojo_id.to_string(),
        "splint_id": requested_splint.to_string(),
        "incarnation": incarnation,
        "topology_revision": provenance.topology_revision.get(),
        "terminal_revision": terminal_revision,
        "history_generation": history_generation,
    }))
}

fn issue_scrollback_cursor(
    registry: &Mutex<CursorRegistry>,
    splint_id: SplintId,
    incarnation: u64,
    terminal_revision: u64,
    history_generation: u64,
    before_row_id: u64,
) -> Result<String, DispatchFailure> {
    registry
        .lock()
        .map_err(|_| DispatchFailure::internal())?
        .issue_scrollback(
            splint_id,
            incarnation,
            terminal_revision,
            history_generation,
            before_row_id,
        )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the stored continuation binds exact terminal and search provenance"
)]
fn issue_search_cursor(
    registry: &Mutex<CursorRegistry>,
    splint_id: SplintId,
    incarnation: u64,
    terminal_revision: u64,
    history_generation: u64,
    daemon_cursor: String,
    query: &str,
    case_sensitive: bool,
) -> Result<String, DispatchFailure> {
    registry
        .lock()
        .map_err(|_| DispatchFailure::internal())?
        .issue_search(
            splint_id,
            incarnation,
            terminal_revision,
            history_generation,
            daemon_cursor,
            query,
            case_sensitive,
        )
}

fn take_terminal_cursor(
    registry: &Mutex<CursorRegistry>,
    value: &str,
    expected_splint: SplintId,
    search: bool,
    query: Option<(&str, bool)>,
) -> Result<TerminalCursorState, DispatchFailure> {
    registry
        .lock()
        .map_err(|_| DispatchFailure::internal())?
        .take(value, expected_splint, search, query)
}

fn terminal_snapshot_output(
    tool: &str,
    requested_splint: SplintId,
    provenance: &TerminalProvenance,
    snapshot: &TerminalSnapshot,
) -> Result<Value, DispatchFailure> {
    snapshot
        .validate()
        .map_err(|_| DispatchFailure::internal())?;
    if snapshot.splint_id != requested_splint
        || provenance.title != snapshot.title
        || provenance.incarnation != snapshot.incarnation
        || provenance.terminal_revision != snapshot.revision
        || provenance.history_generation != snapshot.history_generation
    {
        return Err(DispatchFailure::internal());
    }
    let resource = terminal_resource(
        provenance,
        requested_splint,
        snapshot.incarnation,
        snapshot.revision,
        snapshot.history_generation,
    )?;
    let rows = serde_json::to_value(
        project_terminal_rows(&snapshot.visible_rows).map_err(|_| DispatchFailure::internal())?,
    )
    .map_err(|_| DispatchFailure::internal())?;
    Ok(success(
        tool,
        resource,
        json!({
            "content_encoding": "unicode_scalars",
            "title": provenance.title,
            "rows": rows,
            "continuation_cursor": null,
            "resync_required": false,
        }),
        false,
        "untrusted_terminal_data",
    ))
}

fn scrollback_output(
    cursor_registry: &Mutex<CursorRegistry>,
    requested_splint: SplintId,
    provenance: &TerminalProvenance,
    page: &ScrollbackPage,
) -> Result<Value, DispatchFailure> {
    page.validate().map_err(|_| DispatchFailure::internal())?;
    if page.splint_id != requested_splint {
        return Err(DispatchFailure::internal());
    }
    let resource = terminal_resource(
        provenance,
        requested_splint,
        page.incarnation,
        page.terminal_revision,
        page.history_generation,
    )?;
    let continuation_cursor = if page.has_older {
        let before_row_id = page
            .rows
            .first()
            .and_then(|row| row.row_id)
            .ok_or_else(DispatchFailure::internal)?;
        Some(issue_scrollback_cursor(
            cursor_registry,
            requested_splint,
            page.incarnation,
            page.terminal_revision,
            page.history_generation,
            before_row_id,
        )?)
    } else {
        None
    };
    let rows = serde_json::to_value(
        project_terminal_rows(&page.rows).map_err(|_| DispatchFailure::internal())?,
    )
    .map_err(|_| DispatchFailure::internal())?;
    Ok(success(
        "splinterm.read_scrollback",
        resource,
        json!({
            "content_encoding": "unicode_scalars",
            "title": provenance.title,
            "rows": rows,
            "continuation_cursor": continuation_cursor,
            "resync_required": false,
        }),
        continuation_cursor.is_some(),
        "untrusted_terminal_data",
    ))
}

fn search_output(
    cursor_registry: &Mutex<CursorRegistry>,
    requested_splint: SplintId,
    query: &str,
    case_sensitive: bool,
    provenance: &TerminalProvenance,
    page: &SearchPage,
) -> Result<Value, DispatchFailure> {
    page.validate().map_err(|_| DispatchFailure::internal())?;
    if page.splint_id != requested_splint {
        return Err(DispatchFailure::internal());
    }
    let resource = terminal_resource(
        provenance,
        requested_splint,
        page.incarnation,
        page.terminal_revision,
        page.history_generation,
    )?;
    let continuation_cursor = page
        .next_cursor
        .as_ref()
        .map(|daemon_cursor| {
            issue_search_cursor(
                cursor_registry,
                requested_splint,
                page.incarnation,
                page.terminal_revision,
                page.history_generation,
                daemon_cursor.clone(),
                query,
                case_sensitive,
            )
        })
        .transpose()?;
    let matches = page
        .matches
        .iter()
        .map(|item| json!({"row": item.row_id, "preview": item.preview}))
        .collect::<Vec<_>>();
    Ok(success(
        "splinterm.search_scrollback",
        resource,
        json!({
            "matches": matches,
            "continuation_cursor": continuation_cursor,
            "resync_required": false,
        }),
        continuation_cursor.is_some(),
        "untrusted_terminal_data",
    ))
}

fn terminal_resync_output(
    tool: &str,
    requested_splint: SplintId,
    provenance: &TerminalProvenance,
    current_revision: u64,
    history_generation: u64,
) -> Result<Value, DispatchFailure> {
    let resource = terminal_resource(
        provenance,
        requested_splint,
        provenance.incarnation,
        current_revision,
        history_generation,
    )?;
    let data = if tool == "splinterm.search_scrollback" {
        json!({
            "matches": [],
            "continuation_cursor": null,
            "resync_required": true,
        })
    } else {
        json!({
            "content_encoding": "unicode_scalars",
            "title": provenance.title,
            "rows": [],
            "continuation_cursor": null,
            "resync_required": true,
        })
    };
    Ok(success(
        tool,
        resource,
        data,
        false,
        "untrusted_terminal_data",
    ))
}

async fn read_terminal(
    arguments: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, DispatchFailure> {
    let splint_id = parse_splint(arguments)?;
    let mut session = DaemonSession::connect(cancellation).await?;
    let response = session
        .request(Request::Attach {
            splint_id,
            incarnation: None,
            scrollback_rows: 0,
        })
        .await?;
    let Response::Attached {
        subscription_id,
        provenance,
        snapshot,
    } = response
    else {
        return Err(DispatchFailure::internal());
    };
    if subscription_id == 0 {
        return Err(DispatchFailure::internal());
    }
    match session.request(Request::Detach { subscription_id }).await? {
        Response::Acknowledged => {
            terminal_snapshot_output("splinterm.read_terminal", splint_id, &provenance, &snapshot)
        }
        _ => Err(DispatchFailure::internal()),
    }
}

async fn read_scrollback(
    arguments: &Value,
    cancellation: &CancellationToken,
    cursor_registry: &Mutex<CursorRegistry>,
) -> Result<Value, DispatchFailure> {
    let splint_id = parse_splint(arguments)?;
    let cursor = arguments
        .get("cursor")
        .and_then(Value::as_str)
        .map(|value| take_terminal_cursor(cursor_registry, value, splint_id, false, None))
        .transpose()?;
    let requested = arguments
        .get("max_rows")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(DEFAULT_PAGE_SIZE);
    let mut session = DaemonSession::connect(cancellation).await?;
    let max_rows = session.scrollback_limit(requested);
    if max_rows == 0 {
        return Err(DispatchFailure::resource_limit());
    }
    let request = match cursor {
        None => Request::StartScrollbackPage {
            splint_id,
            incarnation: None,
            max_rows,
        },
        Some(TerminalCursorState::Scrollback {
            incarnation,
            terminal_revision,
            history_generation,
            before_row_id,
            ..
        }) => Request::ScrollbackPage {
            splint_id,
            incarnation,
            terminal_revision,
            history_generation,
            before_row_id,
            max_rows,
        },
        Some(TerminalCursorState::Search { .. }) => unreachable!(),
    };
    match session.request(request).await? {
        Response::ScrollbackPage { provenance, page } => {
            scrollback_output(cursor_registry, splint_id, &provenance, &page)
        }
        Response::ScrollbackResyncRequired {
            provenance,
            current_revision,
            history_generation,
        } => terminal_resync_output(
            "splinterm.read_scrollback",
            splint_id,
            &provenance,
            current_revision,
            history_generation,
        ),
        _ => Err(DispatchFailure::internal()),
    }
}

async fn search_scrollback(
    arguments: &Value,
    cancellation: &CancellationToken,
    cursor_registry: &Mutex<CursorRegistry>,
) -> Result<Value, DispatchFailure> {
    let splint_id = parse_splint(arguments)?;
    let query = arguments["query"]
        .as_str()
        .ok_or_else(DispatchFailure::internal)?;
    if query.is_empty() || query.len() > MAX_SEARCH_QUERY_BYTES {
        return Err(invalid_terminal_argument(
            "the search query exceeds the daemon limit",
        ));
    }
    let case_sensitive = arguments
        .get("case_sensitive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let cursor = arguments
        .get("cursor")
        .and_then(Value::as_str)
        .map(|value| {
            take_terminal_cursor(
                cursor_registry,
                value,
                splint_id,
                true,
                Some((query, case_sensitive)),
            )
        })
        .transpose()?;
    let requested = arguments
        .get("max_matches")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(DEFAULT_PAGE_SIZE);
    let mut session = DaemonSession::connect(cancellation).await?;
    let max_results = DaemonSession::search_limit(requested);
    let request = match cursor {
        None => Request::StartSearchScrollback {
            splint_id,
            incarnation: None,
            query: query.to_owned(),
            case_sensitive,
            max_results,
        },
        Some(TerminalCursorState::Search {
            incarnation,
            terminal_revision,
            history_generation,
            daemon_cursor,
            ..
        }) => Request::SearchScrollback {
            splint_id,
            incarnation,
            terminal_revision,
            history_generation,
            query: query.to_owned(),
            case_sensitive,
            cursor: Some(daemon_cursor),
            max_results,
        },
        Some(TerminalCursorState::Scrollback { .. }) => unreachable!(),
    };
    match session.request(request).await? {
        Response::SearchResults { provenance, page } => search_output(
            cursor_registry,
            splint_id,
            query,
            case_sensitive,
            &provenance,
            &page,
        ),
        Response::SearchResyncRequired {
            provenance,
            current_revision,
            history_generation,
        } => terminal_resync_output(
            "splinterm.search_scrollback",
            splint_id,
            &provenance,
            current_revision,
            history_generation,
        ),
        _ => Err(DispatchFailure::internal()),
    }
}

fn parse_lair(arguments: &Value) -> Result<LairId, DispatchFailure> {
    arguments["lair_id"]
        .as_str()
        .and_then(|value| value.parse().ok())
        .ok_or_else(DispatchFailure::internal)
}

fn parse_dojo(arguments: &Value) -> Result<DojoId, DispatchFailure> {
    arguments["dojo_id"]
        .as_str()
        .and_then(|value| value.parse().ok())
        .ok_or_else(DispatchFailure::internal)
}

fn automation_launch(arguments: &Value) -> Result<AutomationLaunch, DispatchFailure> {
    let cwd = arguments
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from);
    let argv = arguments["argv"]
        .as_array()
        .ok_or_else(DispatchFailure::internal)?
        .iter()
        .map(|value| value.as_str().map(ToOwned::to_owned))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(DispatchFailure::internal)?;
    let launch = AutomationLaunch { cwd, argv };
    launch.validate().map_err(|_| {
        DispatchFailure::new(
            "invalid_argument",
            "the structured launch parameters exceed bounds",
            false,
        )
    })?;
    Ok(launch)
}

fn split_ratio(arguments: &Value) -> Result<(SplitRatio, f64), DispatchFailure> {
    let ratio = arguments["ratio"]
        .as_f64()
        .filter(|value| value.is_finite() && *value > 0.0 && *value < 1.0)
        .ok_or_else(DispatchFailure::internal)?;
    let thousandths = (ratio * 1000.0).round();
    let value = format!("{thousandths:.0}")
        .parse::<u16>()
        .map_err(|_| DispatchFailure::internal())?;
    let ratio = SplitRatio::new(value).map_err(|_| {
        DispatchFailure::new("invalid_argument", "the split ratio is invalid", false)
    })?;
    Ok((ratio, f64::from(ratio.get()) / 1000.0))
}

async fn mutation_preflight(
    session: &mut DaemonSession<'_>,
    mutation: MutationPreflight,
) -> Result<MutationPreparation, DispatchFailure> {
    match session
        .request(Request::PrepareMutation { mutation })
        .await?
    {
        Response::MutationPrepared { preparation }
            if preparation.topology_revision.get() > 0
                && preparation.targets.len() <= 4096
                && preparation
                    .incarnation
                    .is_none_or(|incarnation| incarnation > 0)
                && preparation
                    .targets
                    .iter()
                    .all(|target| target.incarnation > 0) =>
        {
            let unique = preparation
                .targets
                .iter()
                .map(|target| target.splint_id)
                .collect::<HashSet<_>>();
            if unique.len() != preparation.targets.len() {
                return Err(DispatchFailure::internal());
            }
            Ok(preparation)
        }
        _ => Err(DispatchFailure::internal()),
    }
}

fn next_topology_revision(
    preparation: &MutationPreparation,
    committed: splinterm_core::TopologyRevision,
) -> Result<u64, DispatchFailure> {
    if preparation.topology_revision.get().checked_add(1) != Some(committed.get()) {
        return Err(DispatchFailure::internal());
    }
    Ok(committed.get())
}

fn unchanged_topology_revision(
    preparation: &MutationPreparation,
    committed: splinterm_core::TopologyRevision,
) -> Result<u64, DispatchFailure> {
    if committed != preparation.topology_revision {
        return Err(DispatchFailure::internal());
    }
    Ok(committed.get())
}

fn lair_resource(lair_id: LairId, revision: u64) -> Value {
    json!({
        "kind": "lair",
        "lair_id": lair_id.to_string(),
        "topology_revision": revision,
    })
}

fn dojo_resource(lair_id: LairId, dojo_id: DojoId, revision: u64) -> Value {
    json!({
        "kind": "dojo",
        "lair_id": lair_id.to_string(),
        "dojo_id": dojo_id.to_string(),
        "topology_revision": revision,
    })
}

fn splint_resource(
    lair_id: LairId,
    dojo_id: DojoId,
    splint_id: SplintId,
    incarnation: u64,
    revision: u64,
) -> Result<Value, DispatchFailure> {
    if incarnation == 0 {
        return Err(DispatchFailure::internal());
    }
    Ok(json!({
        "kind": "splint",
        "lair_id": lair_id.to_string(),
        "dojo_id": dojo_id.to_string(),
        "splint_id": splint_id.to_string(),
        "incarnation": incarnation,
        "topology_revision": revision,
    }))
}

fn exact_splint_preparation(
    preparation: &MutationPreparation,
    splint_id: SplintId,
) -> Result<(LairId, DojoId, u64), DispatchFailure> {
    match (
        preparation.lair_id,
        preparation.dojo_id,
        preparation.splint_id,
        preparation.incarnation,
        preparation.targets.is_empty(),
    ) {
        (Some(lair_id), Some(dojo_id), Some(prepared), Some(incarnation), true)
            if prepared == splint_id && incarnation > 0 =>
        {
            Ok((lair_id, dojo_id, incarnation))
        }
        _ => Err(DispatchFailure::internal()),
    }
}

fn exact_dojo_preparation(
    preparation: &MutationPreparation,
    dojo_id: DojoId,
) -> Result<LairId, DispatchFailure> {
    match (
        preparation.lair_id,
        preparation.dojo_id,
        preparation.splint_id,
        preparation.incarnation,
        preparation.targets.is_empty(),
    ) {
        (Some(lair_id), Some(prepared), None, None, true) if prepared == dojo_id => Ok(lair_id),
        _ => Err(DispatchFailure::internal()),
    }
}

fn exact_lair_preparation(
    preparation: &MutationPreparation,
    lair_id: LairId,
) -> Result<(), DispatchFailure> {
    if preparation.lair_id == Some(lair_id)
        && preparation.dojo_id.is_none()
        && preparation.splint_id.is_none()
        && preparation.incarnation.is_none()
        && preparation.targets.is_empty()
    {
        Ok(())
    } else {
        Err(DispatchFailure::internal())
    }
}

fn aggregate_restore_output(
    tool: &str,
    preparation: &MutationPreparation,
    resource: Value,
    topology_revision: splinterm_core::TopologyRevision,
    results: &[RestoreLeafResult],
) -> Result<Value, DispatchFailure> {
    if topology_revision != preparation.topology_revision
        || results.len() != preparation.targets.len()
    {
        return Err(DispatchFailure::internal());
    }
    let expected = preparation
        .targets
        .iter()
        .map(|target| target.splint_id)
        .collect::<HashSet<_>>();
    let actual = results
        .iter()
        .map(|result| result.splint_id)
        .collect::<HashSet<_>>();
    if actual.len() != results.len() || actual != expected {
        return Err(DispatchFailure::internal());
    }
    let mut restored_count = 0_usize;
    let mut failed_count = 0_usize;
    for result in results {
        match (&result.error, result.incarnation) {
            (None, Some(incarnation)) if incarnation > 0 => restored_count += 1,
            (Some(_), None) => failed_count += 1,
            _ => return Err(DispatchFailure::internal()),
        }
    }
    let mut resource = resource;
    resource["topology_revision"] = json!(topology_revision.get());
    Ok(success(
        tool,
        resource,
        json!({
            "committed": true,
            "restored_count": restored_count,
            "failed_count": failed_count,
            "partial": restored_count > 0 && failed_count > 0,
        }),
        false,
        "trusted_metadata",
    ))
}

const SPLINT_ID_ENV: &str = "SPLINTERM_SPLINT_ID";
const SPLINT_INCARNATION_ENV: &str = "SPLINTERM_SPLINT_INCARNATION";
const NOT_INSIDE_SPLINT: DispatchFailure = DispatchFailure::new(
    "invalid_argument",
    "not inside a Splint: the adapter environment has no SPLINTERM_SPLINT_ID or SPLINTERM_SPLINT_INCARNATION; pass splint_id and incarnation",
    false,
);
const MALFORMED_SPLINT_ENVIRONMENT: DispatchFailure = DispatchFailure::new(
    "invalid_argument",
    "the adapter Splint environment is incomplete or malformed; pass splint_id and incarnation",
    false,
);

/// Resolves the report target from explicit arguments, else from the
/// adapter's own environment, which it inherits from the agent. The values
/// are discovery hints only: the daemon decides authority from the peer.
fn agent_status_target(
    arguments: &Value,
    environment: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Result<(SplintId, u64), DispatchFailure> {
    match (arguments.get("splint_id"), arguments.get("incarnation")) {
        (Some(_), Some(_)) => Ok((parse_splint(arguments)?, parse_incarnation(arguments)?)),
        (None, None) => {
            let read = |name| environment(name).filter(|value| !value.is_empty());
            match (read(SPLINT_ID_ENV), read(SPLINT_INCARNATION_ENV)) {
                (None, None) => Err(NOT_INSIDE_SPLINT),
                (Some(splint_id), Some(incarnation)) => splint_id
                    .to_str()
                    .and_then(|value| value.parse::<SplintId>().ok())
                    .zip(
                        incarnation
                            .to_str()
                            .and_then(|value| value.parse::<u64>().ok())
                            .filter(|value| *value > 0),
                    )
                    .ok_or(MALFORMED_SPLINT_ENVIRONMENT),
                _ => Err(MALFORMED_SPLINT_ENVIRONMENT),
            }
        }
        _ => Err(DispatchFailure::new(
            "invalid_argument",
            "splint_id and incarnation must be passed together",
            false,
        )),
    }
}

/// Converts schema-validated arguments to a report; a null state clears.
fn agent_status_report(arguments: &Value) -> Result<Option<AgentStatusReport>, DispatchFailure> {
    if arguments["state"].is_null() {
        if arguments.get("detail").is_some() || arguments.get("ttl_seconds").is_some() {
            return Err(DispatchFailure::new(
                "invalid_argument",
                "clearing agent status takes no detail or ttl_seconds",
                false,
            ));
        }
        return Ok(None);
    }
    let state = serde_json::from_value::<AgentState>(arguments["state"].clone())
        .map_err(|_| DispatchFailure::internal())?;
    let detail = arguments
        .get("detail")
        .map(|detail| {
            detail
                .as_str()
                .map(str::to_owned)
                .ok_or_else(DispatchFailure::internal)
        })
        .transpose()?;
    let ttl_seconds = arguments
        .get("ttl_seconds")
        .map(|ttl| {
            ttl.as_u64()
                .and_then(|ttl| u32::try_from(ttl).ok())
                .ok_or_else(DispatchFailure::internal)
        })
        .transpose()?;
    let report = AgentStatusReport {
        state,
        detail,
        ttl_seconds,
    };
    report.validate().map_err(|_| {
        DispatchFailure::new(
            "invalid_argument",
            "agent status detail or TTL is outside its bounds",
            false,
        )
    })?;
    Ok(Some(report))
}

/// Reports explicit display status without any topology preflight, so a
/// process inside the Splint needs no read grant. The detail is never echoed.
async fn report_agent_status(
    tool: &str,
    arguments: &Value,
    cancellation: &CancellationToken,
    socket: Option<&Path>,
) -> Result<Value, DispatchFailure> {
    let status = agent_status_report(arguments)?;
    let (splint_id, incarnation) = agent_status_target(arguments, |name| env::var_os(name))?;
    let state = status.as_ref().map(|status| status.state);
    let mut session = DaemonSession::connect_to(cancellation, socket).await?;
    match session
        .request(Request::ReportAgentStatus {
            splint_id,
            incarnation,
            status,
        })
        .await?
    {
        Response::AgentStatusAccepted {
            sequence,
            expires_at_ms,
        } if state.is_some() || expires_at_ms.is_none() => Ok(success(
            tool,
            json!({
                "kind": "agent_status",
                "splint_id": splint_id.to_string(),
                "incarnation": incarnation,
            }),
            json!({
                "committed": true,
                "state": state.map(AgentState::as_str),
                "sequence": sequence,
                "expires_at_ms": expires_at_ms,
            }),
            false,
            "trusted_metadata",
        )),
        _ => Err(DispatchFailure::internal()),
    }
}

async fn dispatch_mutation(
    tool: &str,
    arguments: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, DispatchFailure> {
    dispatch_mutation_to(tool, arguments, cancellation, None).await
}

#[cfg(feature = "integration-test")]
pub(crate) async fn dispatch_mutation_at(
    tool: &str,
    arguments: &Value,
    cancellation: &CancellationToken,
    socket: &Path,
) -> Result<Value, DispatchFailure> {
    dispatch_mutation_to(tool, arguments, cancellation, Some(socket)).await
}

#[allow(
    clippy::too_many_lines,
    reason = "Slice 6 keeps exact request/result correlations adjacent for security review"
)]
async fn dispatch_mutation_to(
    tool: &str,
    arguments: &Value,
    cancellation: &CancellationToken,
    socket: Option<&Path>,
) -> Result<Value, DispatchFailure> {
    if tool == "splinterm.report_agent_status" {
        return report_agent_status(tool, arguments, cancellation, socket).await;
    }
    if matches!(
        tool,
        "splinterm.create_lair"
            | "splinterm.split_splint"
            | "splinterm.new_dojo"
            | "splinterm.relaunch_splint"
    ) {
        let _ = automation_launch(arguments)?;
    }
    if matches!(tool, "splinterm.split_splint" | "splinterm.set_split_ratio") {
        let _ = split_ratio(arguments)?;
    }
    let mut session = DaemonSession::connect_to(cancellation, socket).await?;
    match tool {
        "splinterm.create_lair" => {
            let launch = automation_launch(arguments)?;
            let preparation =
                mutation_preflight(&mut session, MutationPreflight::CreateLair).await?;
            if preparation.lair_id.is_some()
                || preparation.dojo_id.is_some()
                || preparation.splint_id.is_some()
                || preparation.incarnation.is_some()
                || !preparation.targets.is_empty()
            {
                return Err(DispatchFailure::internal());
            }
            let name = arguments["name"]
                .as_str()
                .ok_or_else(DispatchFailure::internal)?;
            match session
                .request(Request::CreateLairAutomation {
                    expected_topology_revision: preparation.topology_revision,
                    name: name.to_owned(),
                    launch,
                    generated_name: false,
                })
                .await?
            {
                Response::LairCreated {
                    lair,
                    incarnation,
                    topology_revision,
                } => {
                    let revision = next_topology_revision(&preparation, topology_revision)?;
                    if incarnation == 0 || lair.dojos.len() != 1 {
                        return Err(DispatchFailure::internal());
                    }
                    let dojo = &lair.dojos[0];
                    let LayoutNode::Leaf(splint) = &dojo.root else {
                        return Err(DispatchFailure::internal());
                    };
                    Ok(success(
                        tool,
                        lair_resource(lair.id, revision),
                        json!({
                            "committed": true,
                            "dojo_id": dojo.id.to_string(),
                            "splint_id": splint.id.to_string(),
                            "incarnation": incarnation,
                        }),
                        false,
                        "trusted_metadata",
                    ))
                }
                _ => Err(DispatchFailure::internal()),
            }
        }
        "splinterm.split_splint" => {
            let target = parse_splint(arguments)?;
            let launch = automation_launch(arguments)?;
            let preparation = mutation_preflight(
                &mut session,
                MutationPreflight::SplitSplint { splint_id: target },
            )
            .await?;
            let (lair_id, dojo_id, _) = exact_splint_preparation(&preparation, target)?;
            let axis = match arguments["axis"].as_str() {
                Some("horizontal") => Axis::Horizontal,
                Some("vertical") => Axis::Vertical,
                _ => return Err(DispatchFailure::internal()),
            };
            let side = match arguments["side"].as_str() {
                Some("before") => SplitSide::First,
                Some("after") => SplitSide::Second,
                _ => return Err(DispatchFailure::internal()),
            };
            let (ratio, _) = split_ratio(arguments)?;
            match session
                .request(Request::SplitSplintAutomation {
                    expected_topology_revision: preparation.topology_revision,
                    target_splint_id: target,
                    axis,
                    side,
                    ratio,
                    launch,
                })
                .await?
            {
                Response::SplintStarted {
                    splint_id,
                    incarnation,
                    topology_revision,
                } if splint_id != target => {
                    let revision = next_topology_revision(&preparation, topology_revision)?;
                    Ok(success(
                        tool,
                        splint_resource(lair_id, dojo_id, splint_id, incarnation, revision)?,
                        json!({"committed": true}),
                        false,
                        "trusted_metadata",
                    ))
                }
                _ => Err(DispatchFailure::internal()),
            }
        }
        "splinterm.new_dojo" => {
            let lair_id = parse_lair(arguments)?;
            let launch = automation_launch(arguments)?;
            let preparation =
                mutation_preflight(&mut session, MutationPreflight::NewDojo { lair_id }).await?;
            exact_lair_preparation(&preparation, lair_id)?;
            let name = arguments["name"]
                .as_str()
                .ok_or_else(DispatchFailure::internal)?;
            match session
                .request(Request::NewDojoAutomation {
                    expected_topology_revision: preparation.topology_revision,
                    lair_id,
                    name: name.to_owned(),
                    launch,
                    generated_name: false,
                })
                .await?
            {
                Response::DojoStarted {
                    dojo_id,
                    splint_id,
                    incarnation,
                    topology_revision,
                } => {
                    let revision = next_topology_revision(&preparation, topology_revision)?;
                    Ok(success(
                        tool,
                        dojo_resource(lair_id, dojo_id, revision),
                        json!({
                            "committed": true,
                            "splint_id": splint_id.to_string(),
                            "incarnation": incarnation,
                        }),
                        false,
                        "trusted_metadata",
                    ))
                }
                _ => Err(DispatchFailure::internal()),
            }
        }
        "splinterm.relaunch_splint" => {
            let splint_id = parse_splint(arguments)?;
            let launch = automation_launch(arguments)?;
            let preparation = mutation_preflight(
                &mut session,
                MutationPreflight::RelaunchSplint { splint_id },
            )
            .await?;
            let (lair_id, dojo_id, _) = exact_splint_preparation(&preparation, splint_id)?;
            match session
                .request(Request::RelaunchSplintAutomation {
                    expected_topology_revision: preparation.topology_revision,
                    splint_id,
                    launch,
                })
                .await?
            {
                Response::SplintStarted {
                    splint_id: response_id,
                    incarnation,
                    topology_revision,
                } if response_id == splint_id => {
                    let revision = unchanged_topology_revision(&preparation, topology_revision)?;
                    Ok(success(
                        tool,
                        splint_resource(lair_id, dojo_id, splint_id, incarnation, revision)?,
                        json!({"committed": true}),
                        false,
                        "trusted_metadata",
                    ))
                }
                _ => Err(DispatchFailure::internal()),
            }
        }
        "splinterm.restore_splint" => {
            let splint_id = parse_splint(arguments)?;
            let preparation =
                mutation_preflight(&mut session, MutationPreflight::RestoreSplint { splint_id })
                    .await?;
            let (lair_id, dojo_id, _) = exact_splint_preparation(&preparation, splint_id)?;
            match session
                .request(Request::RestoreSplint {
                    expected_topology_revision: preparation.topology_revision,
                    splint_id,
                })
                .await?
            {
                Response::RestoreCompleted {
                    topology_revision,
                    mut results,
                } if results.len() == 1 && results[0].splint_id == splint_id => {
                    let revision = unchanged_topology_revision(&preparation, topology_revision)?;
                    let result = results.pop().ok_or_else(DispatchFailure::internal)?;
                    if result.error.is_some() {
                        return Err(DispatchFailure::internal());
                    }
                    let incarnation = result
                        .incarnation
                        .filter(|value| *value > 0)
                        .ok_or_else(DispatchFailure::internal)?;
                    Ok(success(
                        tool,
                        splint_resource(lair_id, dojo_id, splint_id, incarnation, revision)?,
                        json!({"committed": true, "restored": true}),
                        false,
                        "trusted_metadata",
                    ))
                }
                _ => Err(DispatchFailure::internal()),
            }
        }
        "splinterm.restore_dojo" => {
            let dojo_id = parse_dojo(arguments)?;
            let preparation =
                mutation_preflight(&mut session, MutationPreflight::RestoreDojo { dojo_id })
                    .await?;
            let lair_id = preparation.lair_id.ok_or_else(DispatchFailure::internal)?;
            if preparation.dojo_id != Some(dojo_id) || preparation.targets.is_empty() {
                return Err(DispatchFailure::internal());
            }
            match session
                .request(Request::RestoreDojo {
                    expected_topology_revision: preparation.topology_revision,
                    dojo_id,
                })
                .await?
            {
                Response::RestoreCompleted {
                    topology_revision,
                    results,
                } => aggregate_restore_output(
                    tool,
                    &preparation,
                    dojo_resource(lair_id, dojo_id, topology_revision.get()),
                    topology_revision,
                    &results,
                ),
                _ => Err(DispatchFailure::internal()),
            }
        }
        "splinterm.restore_lair" => {
            let lair_id = parse_lair(arguments)?;
            let preparation =
                mutation_preflight(&mut session, MutationPreflight::RestoreLair { lair_id })
                    .await?;
            if preparation.lair_id != Some(lair_id) || preparation.targets.is_empty() {
                return Err(DispatchFailure::internal());
            }
            match session
                .request(Request::RestoreLair {
                    expected_topology_revision: preparation.topology_revision,
                    lair_id,
                })
                .await?
            {
                Response::RestoreCompleted {
                    topology_revision,
                    results,
                } => aggregate_restore_output(
                    tool,
                    &preparation,
                    lair_resource(lair_id, topology_revision.get()),
                    topology_revision,
                    &results,
                ),
                _ => Err(DispatchFailure::internal()),
            }
        }
        "splinterm.close_splint" => {
            let splint_id = parse_splint(arguments)?;
            let preparation =
                mutation_preflight(&mut session, MutationPreflight::CloseSplint { splint_id })
                    .await?;
            let (lair_id, dojo_id, incarnation) =
                exact_splint_preparation(&preparation, splint_id)?;
            match session
                .request(Request::CloseSplint {
                    expected_topology_revision: preparation.topology_revision,
                    splint_id,
                })
                .await?
            {
                Response::TopologyCommitted { topology_revision } => {
                    let revision = next_topology_revision(&preparation, topology_revision)?;
                    Ok(success(
                        tool,
                        splint_resource(lair_id, dojo_id, splint_id, incarnation, revision)?,
                        json!({"committed": true, "confirmed": true, "closed": true}),
                        false,
                        "trusted_metadata",
                    ))
                }
                _ => Err(DispatchFailure::internal()),
            }
        }
        "splinterm.close_dojo" => {
            let dojo_id = parse_dojo(arguments)?;
            let preparation =
                mutation_preflight(&mut session, MutationPreflight::CloseDojo { dojo_id }).await?;
            let lair_id = exact_dojo_preparation(&preparation, dojo_id)?;
            match session
                .request(Request::CloseDojo {
                    expected_topology_revision: preparation.topology_revision,
                    dojo_id,
                })
                .await?
            {
                Response::TopologyCommitted { topology_revision } => {
                    let revision = next_topology_revision(&preparation, topology_revision)?;
                    Ok(success(
                        tool,
                        dojo_resource(lair_id, dojo_id, revision),
                        json!({"committed": true, "confirmed": true, "closed": true}),
                        false,
                        "trusted_metadata",
                    ))
                }
                _ => Err(DispatchFailure::internal()),
            }
        }
        "splinterm.kill_splint" => {
            let splint_id = parse_splint(arguments)?;
            let incarnation = parse_incarnation(arguments)?;
            let preparation = mutation_preflight(
                &mut session,
                MutationPreflight::KillSplint {
                    splint_id,
                    incarnation,
                },
            )
            .await?;
            let (lair_id, dojo_id, prepared_incarnation) =
                exact_splint_preparation(&preparation, splint_id)?;
            if prepared_incarnation != incarnation {
                return Err(DispatchFailure::internal());
            }
            match session
                .request(Request::KillSplint {
                    splint_id,
                    incarnation,
                })
                .await?
            {
                Response::SplintKilled {
                    splint_id: response_id,
                    incarnation: response_incarnation,
                    ..
                } if response_id == splint_id && response_incarnation == incarnation => {
                    Ok(success(
                        tool,
                        splint_resource(
                            lair_id,
                            dojo_id,
                            splint_id,
                            incarnation,
                            preparation.topology_revision.get(),
                        )?,
                        json!({"committed": true, "confirmed": true, "terminated": true}),
                        false,
                        "trusted_metadata",
                    ))
                }
                _ => Err(DispatchFailure::internal()),
            }
        }
        "splinterm.set_split_ratio" => {
            let splint_id = parse_splint(arguments)?;
            let preparation =
                mutation_preflight(&mut session, MutationPreflight::SetSplitRatio { splint_id })
                    .await?;
            let (lair_id, dojo_id, incarnation) =
                exact_splint_preparation(&preparation, splint_id)?;
            let (ratio, public_ratio) = split_ratio(arguments)?;
            match session
                .request(Request::SetSplitRatio {
                    expected_topology_revision: preparation.topology_revision,
                    target_splint_id: splint_id,
                    ancestor: 0,
                    ratio,
                })
                .await?
            {
                Response::TopologyCommitted { topology_revision } => {
                    let revision = next_topology_revision(&preparation, topology_revision)?;
                    Ok(success(
                        tool,
                        splint_resource(lair_id, dojo_id, splint_id, incarnation, revision)?,
                        json!({"committed": true, "ratio": public_ratio}),
                        false,
                        "trusted_metadata",
                    ))
                }
                _ => Err(DispatchFailure::internal()),
            }
        }
        "splinterm.rename_lair" => {
            let lair_id = parse_lair(arguments)?;
            let preparation =
                mutation_preflight(&mut session, MutationPreflight::RenameLair { lair_id }).await?;
            exact_lair_preparation(&preparation, lair_id)?;
            let name = arguments["name"]
                .as_str()
                .ok_or_else(DispatchFailure::internal)?;
            match session
                .request(Request::RenameLair {
                    expected_topology_revision: preparation.topology_revision,
                    lair_id,
                    name: name.to_owned(),
                })
                .await?
            {
                Response::TopologyCommitted { topology_revision } => {
                    let revision = next_topology_revision(&preparation, topology_revision)?;
                    Ok(success(
                        tool,
                        lair_resource(lair_id, revision),
                        json!({"committed": true, "renamed": true}),
                        false,
                        "trusted_metadata",
                    ))
                }
                _ => Err(DispatchFailure::internal()),
            }
        }
        "splinterm.rename_dojo" => {
            let dojo_id = parse_dojo(arguments)?;
            let preparation =
                mutation_preflight(&mut session, MutationPreflight::RenameDojo { dojo_id }).await?;
            let lair_id = exact_dojo_preparation(&preparation, dojo_id)?;
            let name = arguments["name"]
                .as_str()
                .ok_or_else(DispatchFailure::internal)?;
            match session
                .request(Request::RenameDojo {
                    expected_topology_revision: preparation.topology_revision,
                    dojo_id,
                    name: name.to_owned(),
                    promote_transient_lair: false,
                })
                .await?
            {
                Response::TopologyCommitted { topology_revision } => {
                    let revision = next_topology_revision(&preparation, topology_revision)?;
                    Ok(success(
                        tool,
                        dojo_resource(lair_id, dojo_id, revision),
                        json!({"committed": true, "renamed": true}),
                        false,
                        "trusted_metadata",
                    ))
                }
                _ => Err(DispatchFailure::internal()),
            }
        }
        "splinterm.rename_splint" => {
            let splint_id = parse_splint(arguments)?;
            let preparation =
                mutation_preflight(&mut session, MutationPreflight::RenameSplint { splint_id })
                    .await?;
            let (lair_id, dojo_id, incarnation) =
                exact_splint_preparation(&preparation, splint_id)?;
            let title = arguments["title"]
                .as_str()
                .ok_or_else(DispatchFailure::internal)?;
            match session
                .request(Request::RenameSplint {
                    expected_topology_revision: preparation.topology_revision,
                    splint_id,
                    title: title.to_owned(),
                })
                .await?
            {
                Response::TopologyCommitted { topology_revision } => {
                    let revision = next_topology_revision(&preparation, topology_revision)?;
                    Ok(success(
                        tool,
                        splint_resource(lair_id, dojo_id, splint_id, incarnation, revision)?,
                        json!({"committed": true, "renamed": true}),
                        false,
                        "trusted_metadata",
                    ))
                }
                _ => Err(DispatchFailure::internal()),
            }
        }
        "splinterm.set_dojo_default_focus" => {
            let dojo_id = parse_dojo(arguments)?;
            let splint_id = parse_splint(arguments)?;
            let preparation = mutation_preflight(
                &mut session,
                MutationPreflight::SetDojoDefaultFocus { dojo_id, splint_id },
            )
            .await?;
            let lair_id = preparation.lair_id.ok_or_else(DispatchFailure::internal)?;
            if preparation.dojo_id != Some(dojo_id)
                || preparation.splint_id != Some(splint_id)
                || preparation.incarnation.is_none()
                || !preparation.targets.is_empty()
            {
                return Err(DispatchFailure::internal());
            }
            match session
                .request(Request::SetDojoDefaultFocus {
                    expected_topology_revision: preparation.topology_revision,
                    dojo_id,
                    splint_id,
                })
                .await?
            {
                Response::TopologyCommitted { topology_revision } => {
                    let revision = next_topology_revision(&preparation, topology_revision)?;
                    Ok(success(
                        tool,
                        dojo_resource(lair_id, dojo_id, revision),
                        json!({"committed": true, "splint_id": splint_id.to_string()}),
                        false,
                        "trusted_metadata",
                    ))
                }
                _ => Err(DispatchFailure::internal()),
            }
        }
        _ => Err(DispatchFailure::internal()),
    }
}

fn audit_cursor(value: u64) -> Option<String> {
    (value > 0).then(|| format!("cur_{value:016x}"))
}

fn decode_audit_cursor(value: &str) -> Option<u64> {
    let suffix = value.strip_prefix("cur_")?;
    (suffix.len() == 16)
        .then(|| u64::from_str_radix(suffix, 16).ok())
        .flatten()
        .filter(|value| *value > 0)
}

fn audit_outcome(decision: AuditDecision, outcome: Option<AuditOutcome>) -> &'static str {
    match (decision, outcome) {
        (AuditDecision::Denied | AuditDecision::Rejected | AuditDecision::Expired, _) => "denied",
        (_, Some(AuditOutcome::Failed | AuditOutcome::Cancelled)) => "failed",
        (AuditDecision::Revoked, Some(AuditOutcome::Succeeded)) => "committed",
        _ => "allowed",
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the exhaustive Slice 4 tool-to-private-request table stays contiguous for security review"
)]
pub(crate) async fn dispatch(
    tool: &str,
    arguments: &Value,
    cancellation: &CancellationToken,
    cursor_registry: &Mutex<CursorRegistry>,
) -> Result<Value, DispatchFailure> {
    match tool {
        "splinterm.ping" => match daemon_request(Request::Ping, cancellation).await? {
            Response::Pong => Ok(success(
                tool,
                json!({"kind": "daemon"}),
                json!({"protocol_version": "2025-11-25"}),
                false,
                "trusted_metadata",
            )),
            _ => Err(DispatchFailure::internal()),
        },
        "splinterm.list_lairs" => match daemon_request(Request::ListLairs, cancellation).await? {
            Response::Lairs {
                lairs,
                topology_revision,
            } => Ok(success(
                tool,
                json!({"kind": "topology", "topology_revision": topology_revision.get()}),
                json!({
                    "lairs": lairs.into_iter().map(|dojo| json!({
                        "lair_id": dojo.id.to_string(),
                        "name": dojo.name,
                    })).collect::<Vec<_>>()
                }),
                false,
                "untrusted_terminal_data",
            )),
            _ => Err(DispatchFailure::internal()),
        },
        "splinterm.inspect_topology" => {
            match daemon_request(Request::InspectTopology, cancellation).await? {
                Response::Topology { snapshot } => Ok(success(
                    tool,
                    json!({"kind": "topology", "topology_revision": snapshot.revision.get()}),
                    topology_data(&snapshot, true)?,
                    false,
                    "untrusted_terminal_data",
                )),
                _ => Err(DispatchFailure::internal()),
            }
        }
        "splinterm.inspect_splint" => {
            let splint_id = parse_splint(arguments)?;
            match daemon_request(Request::InspectSplint { splint_id }, cancellation).await? {
                Response::Splint {
                    lair_id,
                    dojo_id,
                    title,
                    topology_revision,
                    runtime,
                    ..
                } if runtime.splint_id == splint_id => {
                    let mut data = json!({"title": title, "state": state_name(&runtime)});
                    if let Some(status) = agent_status_value(&runtime)? {
                        data["agent_status"] = status;
                    }
                    Ok(success(
                        tool,
                        json!({
                            "kind": "splint",
                            "lair_id": lair_id.to_string(),
                            "dojo_id": dojo_id.to_string(),
                            "splint_id": splint_id.to_string(),
                            "current_incarnation": runtime.live_incarnation,
                            "last_incarnation": runtime.last_incarnation,
                            "topology_revision": topology_revision.get(),
                        }),
                        data,
                        false,
                        "untrusted_terminal_data",
                    ))
                }
                _ => Err(DispatchFailure::internal()),
            }
        }
        "splinterm.read_terminal" => read_terminal(arguments, cancellation).await,
        "splinterm.read_scrollback" => {
            read_scrollback(arguments, cancellation, cursor_registry).await
        }
        "splinterm.search_scrollback" => {
            search_scrollback(arguments, cancellation, cursor_registry).await
        }
        "splinterm.create_lair"
        | "splinterm.split_splint"
        | "splinterm.new_dojo"
        | "splinterm.relaunch_splint"
        | "splinterm.restore_splint"
        | "splinterm.restore_dojo"
        | "splinterm.restore_lair"
        | "splinterm.close_splint"
        | "splinterm.close_dojo"
        | "splinterm.kill_splint"
        | "splinterm.set_split_ratio"
        | "splinterm.rename_lair"
        | "splinterm.rename_dojo"
        | "splinterm.rename_splint"
        | "splinterm.set_dojo_default_focus"
        | "splinterm.report_agent_status" => dispatch_mutation(tool, arguments, cancellation).await,
        "splinterm.request_access" => {
            let splint_id = parse_splint(arguments)?;
            let incarnation = parse_incarnation(arguments)?;
            let scopes = arguments["scopes"]
                .as_array()
                .ok_or_else(DispatchFailure::internal)?
                .iter()
                .map(|scope| scope.as_str().and_then(scope_from_name))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| {
                    DispatchFailure::new(
                        "invalid_argument",
                        "request_access supports only exact terminal access scopes",
                        false,
                    )
                })?;
            match daemon_request(
                Request::RequestAccess {
                    splint_id,
                    incarnation,
                    scopes,
                },
                cancellation,
            )
            .await?
            {
                Response::AccessGranted {
                    lair_id,
                    dojo_id,
                    authorization_revision,
                    grant,
                } if grant.splint_id == splint_id && grant.incarnation == incarnation => {
                    let granted_scopes = grant
                        .scopes
                        .iter()
                        .copied()
                        .map(access_scope_name)
                        .collect::<Option<Vec<_>>>()
                        .ok_or_else(DispatchFailure::internal)?;
                    Ok(success(
                        tool,
                        authorization_resource(lair_id, dojo_id, &grant, authorization_revision)?,
                        json!({
                            "committed": true,
                            "granted_scopes": granted_scopes,
                            "expires_at": grant.expires_at_unix_seconds.to_string(),
                        }),
                        false,
                        "trusted_metadata",
                    ))
                }
                _ => Err(DispatchFailure::internal()),
            }
        }
        "splinterm.request_lair_access" => {
            Box::pin(request_lair_access(tool, arguments, cancellation)).await
        }
        "splinterm.authorization_status" => {
            let splint_id = parse_splint(arguments)?;
            match daemon_request(
                Request::AuthorizationStatus {
                    splint_id,
                    incarnation: None,
                },
                cancellation,
            )
            .await?
            {
                Response::AuthorizationStatus {
                    lair_id,
                    dojo_id,
                    incarnation,
                    topology_revision,
                    policy_generation,
                    grants,
                    lair_grants,
                    persistent,
                    development_bypass,
                } if incarnation > 0 => {
                    let mut scopes = BTreeSet::new();
                    for grant in grants {
                        if grant.splint_id != splint_id || grant.incarnation != incarnation {
                            return Err(DispatchFailure::internal());
                        }
                        scopes.extend(grant.scopes.into_iter().filter_map(access_scope_name));
                    }
                    for grant in lair_grants {
                        if grant.lair_id != lair_id {
                            return Err(DispatchFailure::internal());
                        }
                        scopes.extend(grant.scopes.into_iter().filter_map(access_scope_name));
                    }
                    for grant in persistent {
                        scopes.extend(grant.scopes.into_iter().map(automation_scope_name));
                    }
                    Ok(success(
                        tool,
                        json!({
                            "kind": "splint",
                            "lair_id": lair_id.to_string(),
                            "dojo_id": dojo_id.to_string(),
                            "splint_id": splint_id.to_string(),
                            "incarnation": incarnation,
                            "topology_revision": topology_revision.get(),
                        }),
                        json!({
                            "authorized": development_bypass || !scopes.is_empty(),
                            "scopes": scopes,
                            "policy_generation": policy_generation,
                        }),
                        false,
                        "trusted_metadata",
                    ))
                }
                _ => Err(DispatchFailure::internal()),
            }
        }
        "splinterm.revoke_access" => {
            let grant_id = parse_grant_id(arguments)?;
            match daemon_request(Request::RevokeAccess { grant_id }, cancellation).await? {
                Response::AccessRevoked {
                    lair_id,
                    dojo_id,
                    authorization_revision,
                    grant,
                } if grant.grant_id == grant_id => Ok(success(
                    tool,
                    authorization_resource(lair_id, dojo_id, &grant, authorization_revision)?,
                    json!({"committed": true}),
                    false,
                    "trusted_metadata",
                )),
                Response::LairAccessRevoked {
                    topology_revision,
                    authorization_revision,
                    grant,
                } if grant.grant_id == grant_id => Ok(success(
                    tool,
                    json!({
                        "kind": "lair",
                        "lair_id": grant.lair_id.to_string(),
                        "topology_revision": topology_revision.get(),
                        "authorization_revision": authorization_revision,
                    }),
                    json!({"committed": true}),
                    false,
                    "trusted_metadata",
                )),
                _ => Err(DispatchFailure::internal()),
            }
        }
        "splinterm.inspect_audit" => {
            let after_audit_id = arguments
                .get("cursor")
                .and_then(Value::as_str)
                .map(|cursor| {
                    decode_audit_cursor(cursor).ok_or_else(|| {
                        DispatchFailure::new(
                            "invalid_argument",
                            "the audit cursor is invalid",
                            false,
                        )
                    })
                })
                .transpose()?;
            let max_records = arguments
                .get("max_records")
                .and_then(Value::as_u64)
                .map_or(DEFAULT_PAGE_SIZE, |value| {
                    usize::try_from(value).unwrap_or(usize::MAX)
                });
            if max_records == 0 || max_records > MAXIMUM_AUDIT_PAGE {
                return Err(DispatchFailure::new(
                    "invalid_argument",
                    "the audit page limit exceeds the daemon limit",
                    false,
                ));
            }
            match daemon_request(
                Request::AuditInspect {
                    after_audit_id,
                    max_records,
                },
                cancellation,
            )
            .await?
            {
                Response::AuditPage { page } => {
                    let continuation_cursor = page.next_after_audit_id.and_then(audit_cursor);
                    let records = page
                        .records
                        .into_iter()
                        .map(|record| {
                            json!({
                                "audit_id": record.audit_id,
                                "operation": record.operation.as_str(),
                                "outcome": audit_outcome(record.decision, record.outcome),
                            })
                        })
                        .collect::<Vec<_>>();
                    Ok(success(
                        tool,
                        json!({"kind": "audit"}),
                        json!({
                            "retention": "daemon_lifetime",
                            "retention_gap": page.retention_gap,
                            "records": records,
                            "continuation_cursor": continuation_cursor,
                        }),
                        continuation_cursor.is_some(),
                        "trusted_metadata",
                    ))
                }
                _ => Err(DispatchFailure::internal()),
            }
        }
        _ => Err(DispatchFailure::internal()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_consent_wait_budget_stays_a_margin_below_the_deadline() {
        // The 5-second default keeps one second back.
        assert_eq!(consent_wait_budget(Duration::from_secs(5)), 4_000);
        // The 30-second maximum is capped by the daemon's 25 seconds.
        assert_eq!(consent_wait_budget(Duration::from_secs(30)), 25_000);
        // Short deadlines keep a quarter back.
        assert_eq!(consent_wait_budget(Duration::from_millis(100)), 75);
        assert_eq!(consent_wait_budget(Duration::ZERO), 0);
        for millis in [1_u64, 999, 2_000, 26_000, 30_000] {
            let remaining = Duration::from_millis(millis);
            assert!(u128::from(consent_wait_budget(remaining)) < remaining.as_millis());
        }
    }

    #[test]
    fn process_owned_terminal_cursors_are_bound_consumed_and_tamper_resistant() {
        let splint_id: SplintId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77103".parse().unwrap();
        let other: SplintId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77104".parse().unwrap();
        let mut registry = CursorRegistry::default();
        let token = registry.issue_scrollback(splint_id, 2, 9, 3, 4).unwrap();
        assert_eq!(token.len(), 52);
        assert!(registry.take(&token, other, false, None).is_err());
        assert!(registry.take(&token, splint_id, true, None).is_err());

        let mut tampered = token.clone().into_bytes();
        let last = tampered.last_mut().unwrap();
        *last = if *last == b'a' { b'b' } else { b'a' };
        let tampered = String::from_utf8(tampered).unwrap();
        assert!(registry.take(&tampered, splint_id, false, None).is_err());

        assert_eq!(
            registry.take(&token, splint_id, false, None).unwrap(),
            TerminalCursorState::Scrollback {
                splint_id,
                incarnation: 2,
                terminal_revision: 9,
                history_generation: 3,
                before_row_id: 4,
            }
        );
        assert!(registry.take(&token, splint_id, false, None).is_err());
        assert!(
            registry
                .take(
                    "cur_000000000000000000000000000000000000000000000000",
                    splint_id,
                    false,
                    None
                )
                .is_err()
        );
    }

    #[test]
    fn search_cursors_bind_query_mode_and_registry_eviction_is_bounded() {
        let splint_id: SplintId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77103".parse().unwrap();
        let mut registry = CursorRegistry::default();
        let search = registry
            .issue_search(splint_id, 2, 9, 3, "offset-1".to_owned(), "needle", false)
            .unwrap();
        assert!(
            registry
                .take(&search, splint_id, true, Some(("other", false)))
                .is_err()
        );
        assert!(
            registry
                .take(&search, splint_id, true, Some(("needle", true)))
                .is_err()
        );
        assert_eq!(
            registry
                .take(&search, splint_id, true, Some(("needle", false)))
                .unwrap(),
            TerminalCursorState::Search {
                splint_id,
                incarnation: 2,
                terminal_revision: 9,
                history_generation: 3,
                daemon_cursor: "offset-1".to_owned(),
                query_binding: registry.query_binding("needle"),
                case_sensitive: false,
            }
        );

        let mut issued = Vec::new();
        for before_row_id in 1..=MAXIMUM_TERMINAL_CURSORS + 1 {
            issued.push(
                registry
                    .issue_scrollback(splint_id, 2, 9, 3, u64::try_from(before_row_id).unwrap())
                    .unwrap(),
            );
        }
        assert_eq!(registry.entries.len(), MAXIMUM_TERMINAL_CURSORS);
        assert!(registry.take(&issued[0], splint_id, false, None).is_err());
        assert!(
            registry
                .take(issued.last().unwrap(), splint_id, false, None)
                .is_ok()
        );
    }

    #[test]
    fn aggregate_restore_requires_runtime_only_revision_and_exact_results() {
        let lair_id: LairId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77101".parse().unwrap();
        let dojo_id: DojoId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77102".parse().unwrap();
        let first: SplintId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77103".parse().unwrap();
        let second: SplintId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77104".parse().unwrap();
        let preparation = MutationPreparation {
            topology_revision: splinterm_core::TopologyRevision::new(7),
            lair_id: Some(lair_id),
            dojo_id: Some(dojo_id),
            splint_id: None,
            incarnation: None,
            targets: vec![
                splinterm_protocol::MutationTarget {
                    lair_id,
                    dojo_id,
                    splint_id: first,
                    incarnation: 2,
                },
                splinterm_protocol::MutationTarget {
                    lair_id,
                    dojo_id,
                    splint_id: second,
                    incarnation: 4,
                },
            ],
        };
        let success = |splint_id, incarnation| RestoreLeafResult {
            splint_id,
            incarnation: Some(incarnation),
            error: None,
        };
        let failure = |splint_id| RestoreLeafResult {
            splint_id,
            incarnation: None,
            error: Some(splinterm_protocol::ProtocolError::new(
                ErrorCode::ResourceLimit,
                "private failure",
            )),
        };
        let revision = splinterm_core::TopologyRevision::new(7);
        let resource = dojo_resource(lair_id, dojo_id, revision.get());

        let full = aggregate_restore_output(
            "splinterm.restore_dojo",
            &preparation,
            resource.clone(),
            revision,
            &[success(first, 3), success(second, 5)],
        )
        .unwrap();
        assert_eq!(full["data"]["restored_count"], 2);
        assert_eq!(full["data"]["failed_count"], 0);

        let partial = aggregate_restore_output(
            "splinterm.restore_dojo",
            &preparation,
            resource.clone(),
            revision,
            &[success(first, 3), failure(second)],
        )
        .unwrap();
        assert_eq!(partial["data"]["partial"], true);

        let zero = aggregate_restore_output(
            "splinterm.restore_dojo",
            &preparation,
            resource.clone(),
            revision,
            &[failure(first), failure(second)],
        )
        .unwrap();
        assert_eq!(zero["data"]["restored_count"], 0);
        assert_eq!(zero["data"]["failed_count"], 2);
        assert_eq!(zero["data"]["partial"], false);

        assert!(
            aggregate_restore_output(
                "splinterm.restore_dojo",
                &preparation,
                resource,
                splinterm_core::TopologyRevision::new(8),
                &[success(first, 3), success(second, 5)],
            )
            .is_err()
        );
    }

    #[test]
    fn terminal_page_outputs_reject_cross_splint_responses() {
        let requested: SplintId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77103".parse().unwrap();
        let other: SplintId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77104".parse().unwrap();
        let provenance = TerminalProvenance {
            lair_id: "018f4d8c-2a18-4b31-8c2f-9e7c5de77101".parse().unwrap(),
            dojo_id: "018f4d8c-2a18-4b31-8c2f-9e7c5de77102".parse().unwrap(),
            splint_id: requested,
            incarnation: 2,
            topology_revision: splinterm_core::TopologyRevision::new(1),
            terminal_revision: 9,
            history_generation: 3,
            title: "build".to_owned(),
        };
        let page = ScrollbackPage {
            splint_id: other,
            incarnation: 2,
            terminal_revision: 9,
            history_generation: 3,
            oldest_available_row_id: None,
            newest_available_row_id: None,
            rows: Vec::new(),
            has_older: false,
        };
        let search = SearchPage {
            splint_id: other,
            incarnation: 2,
            terminal_revision: 9,
            history_generation: 3,
            matches: Vec::new(),
            next_cursor: None,
            timed_out: false,
        };
        let registry = Mutex::new(CursorRegistry::default());
        assert!(scrollback_output(&registry, requested, &provenance, &page).is_err());
        assert!(
            search_output(&registry, requested, "needle", false, &provenance, &search).is_err()
        );
        assert!(registry.lock().unwrap().entries.is_empty());
    }
}

#[cfg(test)]
mod protocol_error_tests {
    use splinterm_protocol::{ACCESS_REQUEST_EXPIRED, ProtocolError};

    use super::*;

    #[test]
    fn an_expired_access_prompt_is_told_apart_from_other_cancels() {
        let expired = map_protocol_error(&ProtocolError::new(
            ErrorCode::Cancelled,
            ACCESS_REQUEST_EXPIRED,
        ));
        assert_eq!(expired.code, "cancelled");
        assert!(
            expired
                .message
                .starts_with("the access prompt expired without an answer")
        );
        assert!(expired.retryable);
        let other = map_protocol_error(&ProtocolError::new(
            ErrorCode::Cancelled,
            "the access request was cancelled",
        ));
        assert_eq!(other.message, "the daemon request was cancelled");
    }

    #[test]
    fn a_topology_read_without_a_workspace_grant_says_what_to_ask_for() {
        let ungranted = map_protocol_error(&ProtocolError::new(
            ErrorCode::Unauthorized,
            splinterm_protocol::NO_WORKSPACE_GRANT,
        ));
        assert_eq!(ungranted.code, "unauthorized");
        assert!(ungranted.message.contains("request_lair_access"));
        assert!(!ungranted.retryable);
        let other = map_protocol_error(&ProtocolError::new(
            ErrorCode::Unauthorized,
            "no exact persistent policy rule authorizes this request",
        ));
        assert_eq!(
            other.message,
            "the exact policy scope or resource is not authorized"
        );
    }
}

#[cfg(test)]
mod agent_status_tests {
    use std::{collections::HashMap, ffi::OsString, path::PathBuf};

    use splinterm_core::{Dojo, Lair, Splint, SplintState, Topology, TopologyRevision};
    use splinterm_protocol::AgentStatusSummary;

    use super::*;

    const SPLINT: &str = "018f4d8c-2a18-4b31-8c2f-9e7c5de77103";

    fn environment(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let values = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), OsString::from(value)))
            .collect::<HashMap<_, _>>();
        move |key| values.get(key).cloned()
    }

    #[test]
    fn report_target_prefers_arguments_then_the_adapter_environment() {
        let inside = environment(&[(SPLINT_ID_ENV, SPLINT), (SPLINT_INCARNATION_ENV, "3")]);
        assert_eq!(
            agent_status_target(&json!({"state": "idle"}), &inside).unwrap(),
            (SPLINT.parse().unwrap(), 3)
        );
        let other = "018f4d8c-2a18-4b31-8c2f-9e7c5de77104";
        assert_eq!(
            agent_status_target(
                &json!({"state": "idle", "splint_id": other, "incarnation": 9}),
                &inside
            )
            .unwrap(),
            (other.parse().unwrap(), 9)
        );
        let outside = environment(&[]);
        assert_eq!(
            agent_status_target(&json!({"state": "idle"}), &outside)
                .unwrap_err()
                .message,
            NOT_INSIDE_SPLINT.message
        );
        assert_eq!(
            agent_status_target(&json!({"state": "idle", "splint_id": other}), &inside)
                .unwrap_err()
                .code,
            "invalid_argument"
        );
    }

    #[test]
    fn report_arguments_map_to_the_closed_protocol_report() {
        assert_eq!(agent_status_report(&json!({"state": null})).unwrap(), None);
        let report = agent_status_report(
            &json!({"state": "needs_input", "detail": "waiting", "ttl_seconds": 60}),
        )
        .unwrap()
        .unwrap();
        assert_eq!(report.state, AgentState::NeedsInput);
        assert_eq!(report.detail.as_deref(), Some("waiting"));
        assert_eq!(report.ttl_seconds, Some(60));
        assert!(agent_status_report(&json!({"state": null, "detail": "x"})).is_err());
        assert!(agent_status_report(&json!({"state": "done", "detail": "a\u{2066}b"})).is_err());
    }

    #[test]
    fn topology_resource_projection_omits_agent_status() {
        let splint_id: SplintId = SPLINT.parse().unwrap();
        let mut splint = Splint::shell(PathBuf::from("/srv"));
        splint.id = splint_id;
        splint.last_incarnation = Some(2);
        splint.state = SplintState::Running;
        let lair = Lair {
            id: LairId::new(),
            name: "main".to_owned(),
            lifetime: splinterm_core::LairLifetime::default(),
            retention: splinterm_core::LairRetention::default(),
            provenance: None,
            dojos: vec![Dojo {
                id: DojoId::new(),
                name: "terminal".to_owned(),
                default_focus: splint_id,
                root: LayoutNode::Leaf(splint),
            }],
        };
        let mut topology = Topology::new();
        topology
            .insert_lair_at(TopologyRevision::new(0), lair)
            .unwrap();
        let snapshot = TopologySnapshot {
            revision: topology.revision(),
            topology,
            runtimes: vec![SplintRuntimeSummary {
                splint_id,
                live_incarnation: Some(2),
                last_incarnation: Some(2),
                restorable: false,
                lifecycle: SplintLifecycle::Running,
                exit_status: None,
                agent_status: Some(AgentStatusSummary {
                    state: AgentState::Done,
                    detail: None,
                    sequence: 1,
                    stale: false,
                    reported_age_ms: 10,
                }),
            }],
        };
        let splint = |value: Value| value["lairs"][0]["dojos"][0]["splints"][0].clone();
        assert_eq!(
            splint(topology_data(&snapshot, true).unwrap())["agent_status"]["state"],
            "done"
        );
        assert!(
            splint(topology_data(&snapshot, false).unwrap())
                .get("agent_status")
                .is_none()
        );
    }
}
