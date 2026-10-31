use std::{collections::HashMap, sync::Arc, time::Duration};

use rmcp::{
    ErrorData, RoleServer,
    model::{ReadResourceResult, ResourceContents, ResourceUpdatedNotificationParam},
    service::Peer,
};
use serde_json::{Value, json};
use splinterm_automation_client::{
    Connection, ReconnectBackoff, apply_terminal_update, is_connection_loss, project_terminal_rows,
};
use splinterm_core::{DojoId, LairId, SplintId, TopologyRevision};
use splinterm_protocol::{
    ControlStatus, Request, Response, ServerFrame, SubscriptionEvent, TerminalProvenance,
    TerminalSnapshot, TopologySnapshot,
};
use tokio::{
    sync::{Mutex, MutexGuard},
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;

use crate::{dispatch, limits::MAXIMUM_TOOL_RESPONSE_BYTES, tools};

const TOPOLOGY_URI: &str = "splinterm://topology";
pub(crate) const MAXIMUM_RESOURCE_SUBSCRIPTIONS: usize = 16;
const RESOURCE_SCHEMA: &str = "splinterm.mcp.resource.v2";
const REQUEST_DEADLINE: Duration = Duration::from_secs(5);

type ControlModes = Arc<Mutex<HashMap<(SplintId, u64), Vec<String>>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ResourceUri {
    Topology,
    Terminal(SplintId),
    Control(SplintId),
}

impl ResourceUri {
    pub(crate) fn parse(uri: &str) -> Option<Self> {
        if uri == TOPOLOGY_URI {
            return Some(Self::Topology);
        }
        let (id, kind) = uri.strip_prefix("splinterm://splints/")?.split_once('/')?;
        if !tools::canonical_uuid(id) {
            return None;
        }
        let splint_id = id.parse().ok()?;
        match kind {
            "terminal" => Some(Self::Terminal(splint_id)),
            "control" => Some(Self::Control(splint_id)),
            _ => None,
        }
    }

    fn as_string(self) -> String {
        match self {
            Self::Topology => TOPOLOGY_URI.to_owned(),
            Self::Terminal(id) => format!("splinterm://splints/{id}/terminal"),
            Self::Control(id) => format!("splinterm://splints/{id}/control"),
        }
    }

    fn schema_kind(self) -> &'static str {
        match self {
            Self::Topology => "topology",
            Self::Terminal(_) => "terminal",
            Self::Control(_) => "control",
        }
    }
}

#[derive(Debug)]
struct PublishedState {
    value: Value,
    active: bool,
}

#[derive(Debug)]
struct Entry {
    state: Arc<Mutex<PublishedState>>,
    peer: Peer<RoleServer>,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

#[derive(Debug)]
pub(crate) struct ResourceRegistry {
    entries: Mutex<HashMap<ResourceUri, Entry>>,
    setup_gate: Mutex<()>,
    control_modes: ControlModes,
    shutdown: CancellationToken,
}

impl Default for ResourceRegistry {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            setup_gate: Mutex::new(()),
            control_modes: Arc::new(Mutex::new(HashMap::new())),
            shutdown: CancellationToken::new(),
        }
    }
}

impl Drop for ResourceRegistry {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

fn resource_error(message: &'static str) -> ErrorData {
    ErrorData::internal_error(message, None)
}

fn not_found() -> ErrorData {
    ErrorData::resource_not_found("unknown Splinterm resource", None)
}

async fn connect(cancellation: &CancellationToken) -> Result<Connection, ErrorData> {
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(resource_error("resource request was cancelled")),
        result = tokio::time::timeout(REQUEST_DEADLINE, Connection::connect_automation()) => {
            match result {
                Ok(Ok(connection)) => Ok(connection),
                Ok(Err(_)) => Err(resource_error("the local resource request failed")),
                Err(_) => Err(resource_error("the local resource request timed out")),
            }
        }
    }
}

async fn request(
    connection: &mut Connection,
    request: Request,
    cancellation: &CancellationToken,
) -> Result<Response, ErrorData> {
    connection
        .request_with_cancellation(request, REQUEST_DEADLINE, cancellation)
        .await
        .map_err(|_| resource_error("the local resource request failed"))
}

fn topology_document(
    snapshot: &TopologySnapshot,
    resync_revision: Option<TopologyRevision>,
    sequence: u64,
    resync: bool,
) -> Result<Value, ErrorData> {
    let revision = resync_revision.unwrap_or(snapshot.revision);
    let data = if resync {
        json!({"lairs": []})
    } else {
        dispatch::topology_data(snapshot, false)
            .map_err(|_| resource_error("invalid topology resource"))?
    };
    let value = json!({
        "schema": RESOURCE_SCHEMA,
        "uri": TOPOLOGY_URI,
        "sequence": sequence,
        "resync_required": resync,
        "content_trust": "untrusted_terminal_data",
        "resource": {"kind": "topology", "topology_revision": revision.get()},
        "data": data,
    });
    validate_document(ResourceUri::Topology, value)
}

fn terminal_document(
    uri: ResourceUri,
    provenance: &TerminalProvenance,
    snapshot: &TerminalSnapshot,
    sequence: u64,
    resync: bool,
) -> Result<Value, ErrorData> {
    let ResourceUri::Terminal(splint_id) = uri else {
        return Err(not_found());
    };
    snapshot
        .validate()
        .map_err(|_| resource_error("invalid terminal resource"))?;
    if snapshot.splint_id != splint_id
        || provenance.splint_id != splint_id
        || provenance.incarnation != snapshot.incarnation
        || provenance.terminal_revision != snapshot.revision
        || provenance.history_generation != snapshot.history_generation
        || provenance.title != snapshot.title
    {
        return Err(resource_error("invalid terminal resource"));
    }
    let rows = if resync {
        Vec::new()
    } else {
        project_terminal_rows(&snapshot.visible_rows)
            .map_err(|_| resource_error("invalid terminal resource"))?
    };
    let value = json!({
        "schema": RESOURCE_SCHEMA,
        "uri": uri.as_string(),
        "sequence": sequence,
        "resync_required": resync,
        "content_trust": "untrusted_terminal_data",
        "resource": {
            "kind": "terminal",
            "lair_id": provenance.lair_id.to_string(),
            "dojo_id": provenance.dojo_id.to_string(),
            "splint_id": splint_id.to_string(),
            "incarnation": snapshot.incarnation,
            "topology_revision": provenance.topology_revision.get(),
            "terminal_revision": snapshot.revision,
            "history_generation": snapshot.history_generation,
        },
        "data": {
            "content_encoding": "unicode_scalars",
            "title": if resync { "" } else { provenance.title.as_str() },
            "rows": rows,
        },
    });
    validate_document(uri, value)
}

#[allow(
    clippy::too_many_arguments,
    reason = "closed control provenance is explicit"
)]
fn control_document(
    uri: ResourceUri,
    lair_id: LairId,
    dojo_id: DojoId,
    incarnation: u64,
    status: ControlStatus,
    modes: &[String],
    sequence: u64,
    resync: bool,
) -> Result<Value, ErrorData> {
    let ResourceUri::Control(splint_id) = uri else {
        return Err(not_found());
    };
    status
        .validate()
        .map_err(|_| resource_error("invalid control resource"))?;
    if status.splint_id != splint_id || status.incarnation != incarnation {
        return Err(resource_error("invalid control resource"));
    }
    let locally_owned = !resync && status.controlled && !modes.is_empty();
    let public_modes = if locally_owned {
        modes.to_vec()
    } else {
        Vec::new()
    };
    let value = json!({
        "schema": RESOURCE_SCHEMA,
        "uri": uri.as_string(),
        "sequence": sequence,
        "resync_required": resync,
        "content_trust": "trusted_metadata",
        "resource": {
            "kind": "control",
            "lair_id": lair_id.to_string(),
            "dojo_id": dojo_id.to_string(),
            "splint_id": splint_id.to_string(),
            "incarnation": incarnation,
            "control_revision": sequence,
        },
        "data": {
            "controlled": if resync { false } else { status.controlled },
            "locally_owned": locally_owned,
            "modes": public_modes,
        },
    });
    validate_document(uri, value)
}

fn validate_document(uri: ResourceUri, value: Value) -> Result<Value, ErrorData> {
    tools::validate_resource(uri.schema_kind(), &value)
        .map_err(|_| resource_error("invalid projected resource"))?;
    if serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() > MAXIMUM_TOOL_RESPONSE_BYTES) {
        return Err(resource_error(
            "resource response exceeds the adapter limit",
        ));
    }
    Ok(value)
}

fn read_result(uri: ResourceUri, value: &Value) -> Result<ReadResourceResult, ErrorData> {
    let text = serde_json::to_string(value)
        .map_err(|_| resource_error("resource serialization failed"))?;
    let result = ReadResourceResult::new(vec![
        ResourceContents::text(text, uri.as_string()).with_mime_type("application/json"),
    ]);
    if serde_json::to_vec(&result).map_or(true, |bytes| bytes.len() > MAXIMUM_TOOL_RESPONSE_BYTES) {
        return Err(resource_error(
            "resource response exceeds the adapter limit",
        ));
    }
    Ok(result)
}

#[derive(Debug)]
enum Retained {
    Topology {
        snapshot: TopologySnapshot,
        resync_revision: Option<TopologyRevision>,
    },
    Terminal {
        provenance: TerminalProvenance,
        snapshot: Box<TerminalSnapshot>,
    },
    Control {
        lair_id: LairId,
        dojo_id: DojoId,
        incarnation: u64,
        status: ControlStatus,
    },
}

impl Retained {
    fn document(
        &self,
        uri: ResourceUri,
        modes: &[String],
        sequence: u64,
        resync: bool,
    ) -> Result<Value, ErrorData> {
        match self {
            Self::Topology {
                snapshot,
                resync_revision,
            } => topology_document(snapshot, *resync_revision, sequence, resync),
            Self::Terminal {
                provenance,
                snapshot,
            } => terminal_document(uri, provenance, snapshot, sequence, resync),
            Self::Control {
                lair_id,
                dojo_id,
                incarnation,
                status,
            } => control_document(
                uri,
                *lair_id,
                *dojo_id,
                *incarnation,
                *status,
                modes,
                sequence,
                resync,
            ),
        }
    }
}

fn retained_control_modes_from(
    retained: &Retained,
    modes: &HashMap<(SplintId, u64), Vec<String>>,
) -> Vec<String> {
    match retained {
        Retained::Control {
            incarnation,
            status,
            ..
        } => modes
            .get(&(status.splint_id, *incarnation))
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

async fn retained_control_modes(retained: &Retained, modes: &ControlModes) -> Vec<String> {
    if !matches!(retained, Retained::Control { .. }) {
        return Vec::new();
    }
    retained_control_modes_from(retained, &*modes.lock().await)
}

struct OrderedControlModes<'a> {
    _guard: MutexGuard<'a, HashMap<(SplintId, u64), Vec<String>>>,
    values: Vec<String>,
}

async fn ordered_control_modes<'a>(
    retained: &Retained,
    modes: &'a ControlModes,
) -> OrderedControlModes<'a> {
    let guard = modes.lock().await;
    let values = retained_control_modes_from(retained, &guard);
    OrderedControlModes {
        _guard: guard,
        values,
    }
}

struct Started {
    connection: Connection,
    subscription_id: u64,
    retained: Retained,
}

async fn start(uri: ResourceUri, cancellation: &CancellationToken) -> Result<Started, ErrorData> {
    let mut connection = connect(cancellation).await?;
    let (subscription_id, retained) = match uri {
        ResourceUri::Topology => {
            match request(&mut connection, Request::SubscribeTopology, cancellation).await? {
                Response::TopologySubscribed {
                    subscription_id,
                    snapshot,
                } if subscription_id > 0 => (
                    subscription_id,
                    Retained::Topology {
                        snapshot,
                        resync_revision: None,
                    },
                ),
                _ => return Err(resource_error("invalid topology subscription response")),
            }
        }
        ResourceUri::Terminal(splint_id) => match request(
            &mut connection,
            Request::Attach {
                splint_id,
                incarnation: None,
                scrollback_rows: 0,
            },
            cancellation,
        )
        .await?
        {
            Response::Attached {
                subscription_id,
                provenance,
                snapshot,
            } if subscription_id > 0 => (
                subscription_id,
                Retained::Terminal {
                    provenance,
                    snapshot: Box::new(snapshot),
                },
            ),
            _ => return Err(resource_error("invalid terminal subscription response")),
        },
        ResourceUri::Control(splint_id) => {
            let (lair_id, dojo_id, incarnation) = match request(
                &mut connection,
                Request::InspectSplint { splint_id },
                cancellation,
            )
            .await?
            {
                Response::Splint {
                    lair_id,
                    dojo_id,
                    runtime,
                    ..
                } if runtime.splint_id == splint_id && runtime.live_incarnation.is_some() => {
                    (lair_id, dojo_id, runtime.live_incarnation.unwrap())
                }
                _ => return Err(resource_error("invalid control resource identity")),
            };
            match request(
                &mut connection,
                Request::SubscribeControl {
                    splint_id,
                    incarnation,
                },
                cancellation,
            )
            .await?
            {
                Response::ControlSubscribed {
                    subscription_id,
                    status,
                } if subscription_id > 0 => (
                    subscription_id,
                    Retained::Control {
                        lair_id,
                        dojo_id,
                        incarnation,
                        status,
                    },
                ),
                _ => return Err(resource_error("invalid control subscription response")),
            }
        }
    };
    retained.document(uri, &[], 1, false)?;
    Ok(Started {
        connection,
        subscription_id,
        retained,
    })
}

async fn one_shot(
    uri: ResourceUri,
    modes: &[String],
    cancellation: &CancellationToken,
) -> Result<Value, ErrorData> {
    if uri == ResourceUri::Topology {
        let mut connection = connect(cancellation).await?;
        return match request(&mut connection, Request::InspectTopology, cancellation).await? {
            Response::Topology { snapshot } => topology_document(&snapshot, None, 1, false),
            _ => Err(resource_error("invalid topology resource response")),
        };
    }
    let Started {
        mut connection,
        subscription_id,
        retained,
    } = start(uri, cancellation).await?;
    let detached = request(
        &mut connection,
        Request::Detach { subscription_id },
        cancellation,
    )
    .await;
    if !matches!(detached, Ok(Response::Acknowledged)) {
        return Err(resource_error("resource cleanup failed"));
    }
    retained.document(uri, modes, 1, false)
}

fn update_retained(
    retained: &mut Retained,
    uri: ResourceUri,
    event: SubscriptionEvent,
) -> Result<(), ()> {
    match (retained, uri, event) {
        (
            Retained::Topology {
                snapshot,
                resync_revision,
            },
            ResourceUri::Topology,
            SubscriptionEvent::TopologyChanged { change },
        ) => {
            change.validate().map_err(|_| ())?;
            if change.revision.get() <= snapshot.revision.get() {
                return Err(());
            }
            *snapshot = change.snapshot;
            *resync_revision = None;
            Ok(())
        }
        (
            Retained::Terminal {
                provenance,
                snapshot,
            },
            ResourceUri::Terminal(id),
            SubscriptionEvent::Snapshot { snapshot: next },
        ) if next.splint_id == id
            && next.incarnation == snapshot.incarnation
            && next.revision > snapshot.revision
            && next.history_generation == snapshot.history_generation =>
        {
            next.validate().map_err(|_| ())?;
            provenance.title.clone_from(&next.title);
            provenance.terminal_revision = next.revision;
            provenance.history_generation = next.history_generation;
            **snapshot = next;
            Ok(())
        }
        (
            Retained::Terminal {
                provenance,
                snapshot,
            },
            ResourceUri::Terminal(id),
            SubscriptionEvent::Update { update },
        ) if snapshot.splint_id == id => {
            apply_terminal_update(snapshot, update).map_err(|_| ())?;
            provenance.title.clone_from(&snapshot.title);
            provenance.terminal_revision = snapshot.revision;
            provenance.history_generation = snapshot.history_generation;
            Ok(())
        }
        (
            Retained::Control {
                incarnation,
                status,
                ..
            },
            ResourceUri::Control(id),
            SubscriptionEvent::ControlStatusChanged { status: next },
        ) if next.splint_id == id && next.incarnation == *incarnation => {
            next.validate().map_err(|_| ())?;
            *status = next;
            Ok(())
        }
        // Transfer state remains private. Advancing the public sequence with the
        // same closed control projection tells clients that status should be read.
        (
            Retained::Control { .. },
            ResourceUri::Control(_),
            SubscriptionEvent::ControlTransferRequested { .. }
            | SubscriptionEvent::ControlTransferResolved { .. },
        ) => Ok(()),
        _ => Err(()),
    }
}

fn apply_explicit_resync(
    retained: &mut Retained,
    event: &SubscriptionEvent,
    uri: ResourceUri,
) -> bool {
    match (retained, uri, event) {
        (
            Retained::Topology {
                snapshot,
                resync_revision,
            },
            ResourceUri::Topology,
            SubscriptionEvent::TopologyResyncRequired { current_revision },
        ) if current_revision.get() >= snapshot.revision.get() => {
            *resync_revision = Some(*current_revision);
            true
        }
        (
            Retained::Terminal { .. },
            ResourceUri::Terminal(_),
            SubscriptionEvent::ResyncRequired { .. }
            | SubscriptionEvent::AccessRevoked { .. }
            | SubscriptionEvent::Exited { .. },
        )
        | (
            Retained::Control { .. },
            ResourceUri::Control(_),
            SubscriptionEvent::AccessRevoked { .. } | SubscriptionEvent::Exited { .. },
        ) => true,
        _ => false,
    }
}

fn next_public_sequence(published: &PublishedState) -> Result<u64, ()> {
    published.value["sequence"]
        .as_u64()
        .and_then(|value| value.checked_add(1))
        .ok_or(())
}

fn apply_control_modes_projection(
    published: &mut PublishedState,
    uri: ResourceUri,
    incarnation: u64,
    modes: &[String],
) -> Result<(), ()> {
    if !published.active || published.value["resource"]["incarnation"].as_u64() != Some(incarnation)
    {
        return Err(());
    }
    let sequence = next_public_sequence(published)?;
    let mut next = published.value.clone();
    next["sequence"] = serde_json::Value::from(sequence);
    next["resource"]["control_revision"] = serde_json::Value::from(sequence);
    let controlled = next["data"]["controlled"].as_bool() == Some(true);
    let locally_owned = controlled && !modes.is_empty();
    let public_modes = if locally_owned { modes } else { &[] };
    next["data"]["locally_owned"] = serde_json::Value::Bool(locally_owned);
    next["data"]["modes"] = serde_json::to_value(public_modes).unwrap_or_default();
    published.value = validate_document(uri, next).map_err(|_| ())?;
    Ok(())
}

async fn commit_retained_projection(
    state: &Arc<Mutex<PublishedState>>,
    uri: ResourceUri,
    retained: &Retained,
    modes: &[String],
) -> Result<(), ()> {
    let mut published = state.lock().await;
    let value = retained
        .document(uri, modes, next_public_sequence(&published)?, false)
        .map_err(|_| ())?;
    published.value = value;
    Ok(())
}

async fn store_retained_projection(
    state: &Arc<Mutex<PublishedState>>,
    uri: ResourceUri,
    retained: &Retained,
    control_modes: &ControlModes,
) -> Result<(), ()> {
    if matches!(retained, Retained::Control { .. }) {
        // Keep the mode snapshot ordered with the published-state write. Without
        // this guard, set_control_modes could publish new ownership between a
        // stale mode read and this write, only for the subscription to overwrite
        // it permanently with an empty local-mode projection.
        let modes = ordered_control_modes(retained, control_modes).await;
        commit_retained_projection(state, uri, retained, &modes.values).await
    } else {
        commit_retained_projection(state, uri, retained, &[]).await
    }
}

async fn publish_retained(
    state: &Arc<Mutex<PublishedState>>,
    peer: &Peer<RoleServer>,
    uri: ResourceUri,
    retained: &Retained,
    control_modes: &ControlModes,
) -> Result<(), ()> {
    store_retained_projection(state, uri, retained, control_modes).await?;
    peer.notify_resource_updated(ResourceUpdatedNotificationParam::new(uri.as_string()))
        .await
        .map_err(|_| ())
}

/// How a resource subscription's stream ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StreamEnd {
    Cancelled,
    Failed,
    /// The daemon connection itself ended, as when splinterd is upgraded.
    ConnectionLost,
}

/// How long a resource subscription resubscribes after its daemon
/// connection ended before it is left stale (Plan 0061 M3.12b).
const RESUBSCRIBE_WINDOW: Duration = Duration::from_secs(30);

/// Follows one subscription stream, publishing each update, until it ends.
async fn follow_subscription(
    uri: ResourceUri,
    started: &mut Started,
    state: &Arc<Mutex<PublishedState>>,
    cancellation: &CancellationToken,
    control_modes: &ControlModes,
    peer: &Peer<RoleServer>,
) -> StreamEnd {
    let mut private_sequence = 1_u64;
    loop {
        let frame = tokio::select! {
            biased;
            () = cancellation.cancelled() => return StreamEnd::Cancelled,
            frame = started.connection.next_server_frame() => match frame {
                Ok(frame) => frame,
                Err(error) if is_connection_loss(&error) => return StreamEnd::ConnectionLost,
                Err(_) => return StreamEnd::Failed,
            }
        };
        let ServerFrame::Event {
            subscription_id,
            sequence,
            event,
        } = frame
        else {
            return StreamEnd::Failed;
        };
        if subscription_id != started.subscription_id {
            return StreamEnd::Failed;
        }
        if sequence != private_sequence {
            let _ = apply_explicit_resync(&mut started.retained, &event, uri);
            return StreamEnd::Failed;
        }
        private_sequence = private_sequence.saturating_add(1);
        // Agent status rides the topology subscription without a revision;
        // the public topology resource does not project it yet. Handoff status
        // may ride any subscription and no public resource projects it.
        if matches!(event, SubscriptionEvent::HandoffStatus { .. })
            || (uri == ResourceUri::Topology
                && matches!(event, SubscriptionEvent::AgentStatusChanged { .. }))
        {
            continue;
        }
        if apply_explicit_resync(&mut started.retained, &event, uri)
            || update_retained(&mut started.retained, uri, event).is_err()
            || publish_retained(state, peer, uri, &started.retained, control_modes)
                .await
                .is_err()
        {
            return StreamEnd::Failed;
        }
    }
}

/// Publishes the retained document marked stale, keeping the public sequence
/// increasing, and notifies the peer; `active` says whether the subscription
/// is still trying.
async fn publish_stale(
    state: &Arc<Mutex<PublishedState>>,
    peer: &Peer<RoleServer>,
    uri: ResourceUri,
    retained: &Retained,
    active: bool,
) {
    let notify = mark_stale(&mut *state.lock().await, uri, retained, active);
    if notify {
        let _ = peer
            .notify_resource_updated(ResourceUpdatedNotificationParam::new(uri.as_string()))
            .await;
    }
}

/// Marks the published document stale (`resync_required`) under the next
/// public sequence and records whether the subscription is still trying;
/// returns whether the peer should hear of it.
fn mark_stale(
    published: &mut PublishedState,
    uri: ResourceUri,
    retained: &Retained,
    active: bool,
) -> bool {
    let mut notify = false;
    if let Ok(sequence) = next_public_sequence(published)
        && let Ok(value) = retained.document(uri, &[], sequence, true)
    {
        published.value = value;
        notify = true;
    }
    published.active = active;
    notify
}

/// Subscribes to `uri` again on a fresh connection, with backoff, for up to
/// [`RESUBSCRIBE_WINDOW`]; `None` once time is up or the subscription is
/// cancelled.
async fn resubscribe(uri: ResourceUri, cancellation: &CancellationToken) -> Option<Started> {
    retry_start(RESUBSCRIBE_WINDOW, cancellation, || {
        start(uri, cancellation)
    })
    .await
}

/// Runs `start` with backoff until it succeeds, `window` passes, or
/// `cancellation` fires.
async fn retry_start<T, F, Fut>(
    window: Duration,
    cancellation: &CancellationToken,
    mut start: F,
) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ErrorData>>,
{
    let deadline = tokio::time::Instant::now() + window;
    let mut backoff = ReconnectBackoff::default();
    loop {
        if let Ok(Ok(started)) = tokio::time::timeout_at(deadline, start()).await {
            return Some(started);
        }
        let resume = tokio::time::Instant::now() + backoff.next_delay();
        if resume >= deadline || cancellation.is_cancelled() {
            return None;
        }
        tokio::select! {
            biased;
            () = cancellation.cancelled() => return None,
            () = tokio::time::sleep_until(resume) => {}
        }
    }
}

async fn subscription_task(
    uri: ResourceUri,
    mut started: Started,
    state: Arc<Mutex<PublishedState>>,
    cancellation: CancellationToken,
    control_modes: ControlModes,
    peer: Peer<RoleServer>,
) {
    loop {
        match follow_subscription(
            uri,
            &mut started,
            &state,
            &cancellation,
            &control_modes,
            &peer,
        )
        .await
        {
            StreamEnd::Cancelled => {
                state.lock().await.active = false;
                break;
            }
            StreamEnd::Failed => {
                publish_stale(&state, &peer, uri, &started.retained, false).await;
                break;
            }
            StreamEnd::ConnectionLost => {
                // Plan 0061 M3.12b: the subscription reconnects transparently
                // from a fresh snapshot; readers see it stale meanwhile. The
                // public sequence keeps increasing; the private one restarts
                // with the new subscription.
                publish_stale(&state, &peer, uri, &started.retained, true).await;
                let Some(next) = resubscribe(uri, &cancellation).await else {
                    state.lock().await.active = false;
                    return;
                };
                started = next;
                if publish_retained(&state, &peer, uri, &started.retained, &control_modes)
                    .await
                    .is_err()
                {
                    publish_stale(&state, &peer, uri, &started.retained, false).await;
                    break;
                }
            }
        }
    }
    let _ = started
        .connection
        .request_with_deadline(
            Request::Detach {
                subscription_id: started.subscription_id,
            },
            Duration::from_secs(1),
        )
        .await;
}

impl ResourceRegistry {
    pub(crate) async fn read(
        &self,
        uri: &str,
        cancellation: &CancellationToken,
    ) -> Result<ReadResourceResult, ErrorData> {
        let uri = ResourceUri::parse(uri).ok_or_else(not_found)?;
        let state = {
            let entries = self.entries.lock().await;
            entries.get(&uri).map(|entry| Arc::clone(&entry.state))
        };
        let value = if let Some(state) = state {
            state.lock().await.value.clone()
        } else {
            let modes = match uri {
                ResourceUri::Control(splint_id) => self
                    .control_modes
                    .lock()
                    .await
                    .iter()
                    .find_map(|((id, _), modes)| (*id == splint_id).then(|| modes.clone()))
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            one_shot(uri, &modes, cancellation).await?
        };
        read_result(uri, &value)
    }

    pub(crate) async fn subscribe(
        &self,
        uri: &str,
        cancellation: &CancellationToken,
        peer: Peer<RoleServer>,
    ) -> Result<(), ErrorData> {
        let uri = ResourceUri::parse(uri).ok_or_else(not_found)?;
        let _setup = self.setup_gate.lock().await;
        let old = {
            let mut entries = self.entries.lock().await;
            if let Some(entry) = entries.get(&uri)
                && entry.state.lock().await.active
            {
                return Ok(());
            }
            entries.remove(&uri)
        };
        if let Some(old) = old {
            stop_entry(old).await;
        }
        {
            let mut entries = self.entries.lock().await;
            if entries.len() >= MAXIMUM_RESOURCE_SUBSCRIPTIONS
                && let Some(closed) = entries
                    .iter()
                    .find_map(|(key, entry)| entry.task.is_finished().then_some(*key))
            {
                entries.remove(&closed);
            }
            if entries.len() >= MAXIMUM_RESOURCE_SUBSCRIPTIONS {
                return Err(resource_error("resource subscription limit reached"));
            }
        }
        let started = start(uri, cancellation).await?;
        let lifetime = self.shutdown.child_token();
        let initial_modes = retained_control_modes(&started.retained, &self.control_modes).await;
        let initial = started.retained.document(uri, &initial_modes, 1, false)?;
        let state = Arc::new(Mutex::new(PublishedState {
            value: initial,
            active: true,
        }));
        let task_state = Arc::clone(&state);
        let task_cancel = lifetime.clone();
        let task_modes = Arc::clone(&self.control_modes);
        let task_peer = peer.clone();
        let task = tokio::spawn(async move {
            subscription_task(uri, started, task_state, task_cancel, task_modes, task_peer).await;
        });
        self.entries.lock().await.insert(
            uri,
            Entry {
                state,
                peer,
                cancel: lifetime,
                task,
            },
        );
        Ok(())
    }

    pub(crate) async fn unsubscribe(&self, uri: &str) -> Result<(), ErrorData> {
        let uri = ResourceUri::parse(uri).ok_or_else(not_found)?;
        let _setup = self.setup_gate.lock().await;
        let entry = self.entries.lock().await.remove(&uri);
        if let Some(entry) = entry {
            stop_entry(entry).await;
        }
        Ok(())
    }

    pub(crate) async fn set_control_modes(
        &self,
        splint_id: SplintId,
        incarnation: u64,
        modes: Vec<String>,
    ) {
        // Serialize mode-map changes with subscription setup so an update cannot
        // miss an entry after its initial mode snapshot but before insertion.
        let setup = self.setup_gate.lock().await;
        let key = (splint_id, incarnation);
        if modes.is_empty() {
            self.control_modes.lock().await.remove(&key);
        } else {
            self.control_modes.lock().await.insert(key, modes.clone());
        }
        let uri = ResourceUri::Control(splint_id);
        let target = {
            let entries = self.entries.lock().await;
            entries
                .get(&uri)
                .map(|entry| (Arc::clone(&entry.state), entry.peer.clone()))
        };
        let Some((state, peer)) = target else {
            return;
        };
        let mut published = state.lock().await;
        if apply_control_modes_projection(&mut published, uri, incarnation, &modes).is_err() {
            return;
        }
        drop(published);
        drop(setup);
        let _ = peer
            .notify_resource_updated(ResourceUpdatedNotificationParam::new(uri.as_string()))
            .await;
    }

    pub(crate) async fn shutdown(&self) {
        self.shutdown.cancel();
        let _setup = self.setup_gate.lock().await;
        let entries = {
            let mut entries = self.entries.lock().await;
            entries.drain().map(|(_, entry)| entry).collect::<Vec<_>>()
        };
        let mut cleanup = JoinSet::new();
        for entry in entries {
            cleanup.spawn(stop_entry(entry));
        }
        while cleanup.join_next().await.is_some() {}
        self.control_modes.lock().await.clear();
    }
}

async fn stop_entry(mut entry: Entry) {
    entry.cancel.cancel();
    if tokio::time::timeout(Duration::from_secs(2), &mut entry.task)
        .await
        .is_err()
    {
        entry.task.abort();
        let _ = entry.task.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn topology_retained() -> Retained {
        let topology = splinterm_core::Topology::new();
        Retained::Topology {
            snapshot: TopologySnapshot {
                revision: topology.revision(),
                topology,
                runtimes: Vec::new(),
                presentation_offers: Vec::new(),
            },
            resync_revision: None,
        }
    }

    /// Plan 0061 M3.12b review S7: a lost subscription reads stale under the
    /// next public sequence while it resubscribes, and stays stale and
    /// inactive once resubscribing gave up.
    #[test]
    fn a_lost_subscription_reads_stale_while_trying_and_inactive_after() {
        let retained = topology_retained();
        let uri = ResourceUri::Topology;
        let mut published = PublishedState {
            value: retained.document(uri, &[], 4, false).unwrap(),
            active: true,
        };
        assert!(mark_stale(&mut published, uri, &retained, true));
        assert_eq!(published.value["sequence"], 5);
        assert_eq!(published.value["resync_required"], true);
        assert!(published.active, "still resubscribing");
        assert!(mark_stale(&mut published, uri, &retained, false));
        assert_eq!(published.value["sequence"], 6);
        assert_eq!(published.value["resync_required"], true);
        assert!(!published.active, "resubscribing gave up");
    }

    #[tokio::test(start_paused = true)]
    async fn resubscribing_backs_off_and_gives_up_after_its_window() {
        let started = tokio::time::Instant::now();
        let mut attempts = 0_u32;
        let outcome: Option<()> =
            retry_start(RESUBSCRIBE_WINDOW, &CancellationToken::new(), || {
                attempts += 1;
                async { Err(resource_error("the local resource request failed")) }
            })
            .await;
        assert!(outcome.is_none());
        assert!(attempts > 2, "{attempts}");
        assert!(started.elapsed() <= RESUBSCRIBE_WINDOW);
        assert!(started.elapsed() >= RESUBSCRIBE_WINDOW - Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn resubscribing_succeeds_once_the_daemon_answers_and_stops_when_cancelled() {
        let mut attempts = 0_u32;
        let outcome = retry_start(RESUBSCRIBE_WINDOW, &CancellationToken::new(), || {
            attempts += 1;
            let ready = attempts > 2;
            async move {
                if ready {
                    Ok(7_u64)
                } else {
                    Err(resource_error("the local resource request failed"))
                }
            }
        })
        .await;
        assert_eq!(outcome, Some(7));
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let started = tokio::time::Instant::now();
        let outcome: Option<()> = retry_start(RESUBSCRIBE_WINDOW, &cancellation, || async {
            Err(resource_error("the local resource request failed"))
        })
        .await;
        assert!(outcome.is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn control_projection_never_claims_local_modes_without_daemon_control() {
        let splint_id: SplintId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77103".parse().unwrap();
        let lair_id: LairId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77101".parse().unwrap();
        let dojo_id: DojoId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77102".parse().unwrap();
        let uri = ResourceUri::Control(splint_id);
        let modes = vec!["input".to_owned()];
        let uncontrolled = control_document(
            uri,
            lair_id,
            dojo_id,
            2,
            ControlStatus {
                splint_id,
                incarnation: 2,
                controlled: false,
                locally_owned: false,
                lost_reason: None,
            },
            &modes,
            1,
            false,
        )
        .unwrap();
        assert_eq!(uncontrolled["data"]["locally_owned"], false);
        assert_eq!(uncontrolled["data"]["modes"], json!([]));

        let controlled = control_document(
            uri,
            lair_id,
            dojo_id,
            2,
            ControlStatus {
                splint_id,
                incarnation: 2,
                controlled: true,
                locally_owned: false,
                lost_reason: None,
            },
            &modes,
            2,
            false,
        )
        .unwrap();
        assert_eq!(controlled["data"]["locally_owned"], true);
        assert_eq!(controlled["data"]["modes"], json!(["input"]));
    }

    #[tokio::test]
    async fn control_projection_orders_a_competing_mode_commit() {
        let splint_id: SplintId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77103".parse().unwrap();
        let lair_id: LairId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77101".parse().unwrap();
        let dojo_id: DojoId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77102".parse().unwrap();
        let uri = ResourceUri::Control(splint_id);
        let retained = Retained::Control {
            lair_id,
            dojo_id,
            incarnation: 2,
            status: ControlStatus {
                splint_id,
                incarnation: 2,
                controlled: true,
                locally_owned: false,
                lost_reason: None,
            },
        };
        let state = Arc::new(Mutex::new(PublishedState {
            value: control_document(
                uri,
                lair_id,
                dojo_id,
                2,
                ControlStatus {
                    splint_id,
                    incarnation: 2,
                    controlled: false,
                    locally_owned: false,
                    lost_reason: None,
                },
                &[],
                5,
                false,
            )
            .unwrap(),
            active: true,
        }));
        let control_modes = Arc::new(Mutex::new(HashMap::new()));
        let ordered = ordered_control_modes(&retained, &control_modes).await;
        assert!(ordered.values.is_empty());

        let (attempting, attempted) = tokio::sync::oneshot::channel();
        let writer = tokio::spawn({
            let state = Arc::clone(&state);
            let control_modes = Arc::clone(&control_modes);
            async move {
                let _ = attempting.send(());
                let modes = vec!["input".to_owned()];
                control_modes
                    .lock()
                    .await
                    .insert((splint_id, 2), modes.clone());
                let mut published = state.lock().await;
                apply_control_modes_projection(&mut published, uri, 2, &modes)
            }
        });
        attempted.await.unwrap();

        assert_eq!(
            commit_retained_projection(&state, uri, &retained, &ordered.values).await,
            Ok(())
        );
        drop(ordered);
        assert_eq!(writer.await.unwrap(), Ok(()));

        let published = state.lock().await;
        assert_eq!(published.value["sequence"], 7);
        assert_eq!(published.value["resource"]["control_revision"], 7);
        assert_eq!(published.value["data"]["locally_owned"], true);
        assert_eq!(published.value["data"]["modes"], json!(["input"]));
    }

    #[tokio::test]
    async fn control_mode_updates_wait_for_subscription_setup() {
        let splint_id: SplintId = "018f4d8c-2a18-4b31-8c2f-9e7c5de77103".parse().unwrap();
        let registry = ResourceRegistry::default();
        let setup = registry.setup_gate.lock().await;
        let update = registry.set_control_modes(splint_id, 2, vec!["input".to_owned()]);
        tokio::pin!(update);
        tokio::select! {
            biased;
            () = &mut update => panic!("mode update bypassed subscription setup ordering"),
            () = tokio::task::yield_now() => {}
        }
        assert!(registry.control_modes.lock().await.is_empty());
        drop(setup);
        update.await;
        assert_eq!(
            registry
                .control_modes
                .lock()
                .await
                .get(&(splint_id, 2))
                .cloned(),
            Some(vec!["input".to_owned()])
        );
    }

    #[test]
    fn uri_parser_is_exact() {
        let id = "018f4d8c-2a18-4b31-8c2f-9e7c5de77103";
        assert_eq!(
            ResourceUri::parse(TOPOLOGY_URI),
            Some(ResourceUri::Topology)
        );
        assert!(matches!(
            ResourceUri::parse(&format!("splinterm://splints/{id}/terminal")),
            Some(ResourceUri::Terminal(_))
        ));
        assert!(matches!(
            ResourceUri::parse(&format!("splinterm://splints/{id}/control")),
            Some(ResourceUri::Control(_))
        ));
        for invalid in [
            "splinterm://topology/",
            "splinterm://splints/018f4d8c-2a18-4b31-8c2f-9e7c5de77103/terminal/more",
            "splinterm://splints/018F4D8C-2A18-4B31-8C2F-9E7C5DE77103/control",
            "https://splinterm/topology",
        ] {
            assert!(ResourceUri::parse(invalid).is_none(), "{invalid}");
        }
    }
}
