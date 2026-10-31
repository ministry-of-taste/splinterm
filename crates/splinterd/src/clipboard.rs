//! Plan 0074 M2: routes what programs ask of the clipboard through OSC 52.
//!
//! A Splint's actor reports each set and query ([`ClipboardEvent`]); the
//! broker sends it to the Window connection, installed client or remote
//! Window, that holds the Splint's controller lease, on that connection's
//! control subscription for the Splint. Nobody else hears it: not a Window
//! that only views the Splint, never an automation connection, and never a
//! relay running inside a Splint, so an agent reached only through MCP gains
//! nothing from OSC 52. A query nobody answers in time gets the empty reply
//! the terminal protocol allows; the actor writes every reply in the order
//! the program asked.
//!
//! Sets and queries are audited with their byte count and outcome, never
//! their contents or selection. A Splint writes at most
//! [`AUDIT_RECORDS_PER_WINDOW`] such records a minute, then one record that
//! the rest were suppressed. A set the actor drops because the broker is
//! busy is not audited.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex as StdMutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use splinterd::{ClipboardEvent, ClipboardRequest};
use splinterm_core::SplintId;
use splinterm_protocol::{AuditDecision, AuditOperation, AuditOutcome, ClipboardSelection};
use splinterm_terminal::ClipboardSelection as TerminalSelection;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot},
    time::{self, Instant},
};

use super::DaemonState;

/// How many requests every Splint's actor may have waiting for the broker
/// together. A full queue drops a set and answers a query empty.
pub(super) const BROKER_QUEUE: usize = 8;

/// How long a program waits for the Window's answer to a query.
pub(super) const QUERY_TIMEOUT: Duration = Duration::from_secs(2);

/// The fewest milliseconds between two sets of one selection sent to a
/// Window; a faster program's sets are coalesced, latest wins, so a Window
/// connection is never flooded with events.
pub(super) const SET_INTERVAL: Duration = Duration::from_millis(50);

/// The most set text, as base64, every route holds together before its
/// Window connection's writer sends it. A set beyond it is dropped.
pub(super) const ROUTED_MEMORY_BYTES: usize = 8 * 1024 * 1024;

/// The most set text, as base64, one route holds in its queue, its pacer,
/// and its outbound frame, so one slow Window cannot take the whole budget.
pub(super) const ROUTE_MEMORY_BYTES: usize = 3 * 1024 * 1024;

/// How many clipboard audit records one Splint writes per
/// [`AUDIT_WINDOW`]; past it, one record says the rest were suppressed.
pub(super) const AUDIT_RECORDS_PER_WINDOW: u32 = 16;

/// The window [`AUDIT_RECORDS_PER_WINDOW`] counts over.
pub(super) const AUDIT_WINDOW: Duration = Duration::from_secs(60);

/// Deliveries one route holds before its subscription takes them.
const ROUTE_QUEUE: usize = 4;

/// The routed memory one set holds until its frame is written: its share of
/// the daemon's budget and of its route's.
#[derive(Debug)]
pub(super) struct SetMemory {
    _global: OwnedSemaphorePermit,
    _route: OwnedSemaphorePermit,
}

/// What the broker hands one Window control subscription.
#[derive(Debug)]
pub(super) enum Delivery {
    Set {
        selection: ClipboardSelection,
        data: String,
        memory: SetMemory,
    },
    Query {
        query_id: u64,
        selection: ClipboardSelection,
    },
}

struct Route {
    incarnation: u64,
    token: u64,
    sender: mpsc::Sender<Delivery>,
    memory: Arc<Semaphore>,
}

/// A route's queue and its memory budget.
#[derive(Clone)]
struct RouteSender {
    sender: mpsc::Sender<Delivery>,
    memory: Arc<Semaphore>,
}

struct PendingQuery {
    query_id: u64,
    connection_id: u64,
    answer: oneshot::Sender<Vec<u8>>,
}

/// One Splint's clipboard audit records in the current window.
struct AuditWindow {
    started: Instant,
    records: u32,
}

/// Whether a clipboard audit record is written.
#[derive(Debug, Eq, PartialEq)]
enum AuditAdmission {
    Record,
    /// The first past the bound: one record says the rest are suppressed.
    Suppressed,
    Skip,
}

/// The Window control subscriptions that can carry clipboard requests, and
/// the queries waiting for their answers: at most one per Splint
/// incarnation.
pub(super) struct ClipboardRoutes {
    routes: StdMutex<HashMap<(u64, SplintId), Route>>,
    pending: StdMutex<HashMap<(SplintId, u64), PendingQuery>>,
    audits: StdMutex<HashMap<SplintId, AuditWindow>>,
    next_token: AtomicU64,
    next_query_id: AtomicU64,
    memory: Arc<Semaphore>,
}

impl Default for ClipboardRoutes {
    fn default() -> Self {
        Self {
            routes: StdMutex::default(),
            pending: StdMutex::default(),
            audits: StdMutex::default(),
            next_token: AtomicU64::new(1),
            next_query_id: AtomicU64::new(1),
            memory: Arc::new(Semaphore::new(ROUTED_MEMORY_BYTES)),
        }
    }
}

/// Removes a Window control subscription's route when the subscription
/// ends, and with it the query that route still waits to answer.
pub(super) struct RouteGuard {
    routes: Arc<ClipboardRoutes>,
    key: (u64, SplintId),
    token: u64,
}

impl Drop for RouteGuard {
    fn drop(&mut self) {
        let removed = {
            let mut routes = self.routes.routes.lock().unwrap();
            let current = routes
                .get(&self.key)
                .is_some_and(|route| route.token == self.token);
            if current {
                routes.remove(&self.key);
            }
            current
        };
        if removed {
            // The waiter hears at once that its Window is gone.
            let (connection_id, splint_id) = self.key;
            self.routes
                .pending
                .lock()
                .unwrap()
                .retain(|(pending_splint, _), query| {
                    *pending_splint != splint_id || query.connection_id != connection_id
                });
        }
    }
}

impl ClipboardRoutes {
    /// Registers a Window connection's control subscription for one Splint
    /// incarnation. A newer subscription of the same connection and Splint
    /// replaces it.
    pub(super) fn register(
        self: &Arc<Self>,
        connection_id: u64,
        splint_id: SplintId,
        incarnation: u64,
    ) -> (RouteGuard, mpsc::Receiver<Delivery>) {
        let (sender, receiver) = mpsc::channel(ROUTE_QUEUE);
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        let key = (connection_id, splint_id);
        self.routes.lock().unwrap().insert(
            key,
            Route {
                incarnation,
                token,
                sender,
                memory: Arc::new(Semaphore::new(ROUTE_MEMORY_BYTES)),
            },
        );
        let guard = RouteGuard {
            routes: Arc::clone(self),
            key,
            token,
        };
        (guard, receiver)
    }

    fn route(
        &self,
        connection_id: u64,
        splint_id: SplintId,
        incarnation: u64,
    ) -> Option<RouteSender> {
        self.routes
            .lock()
            .unwrap()
            .get(&(connection_id, splint_id))
            .filter(|route| route.incarnation == incarnation)
            .map(|route| RouteSender {
                sender: route.sender.clone(),
                memory: Arc::clone(&route.memory),
            })
    }

    /// Starts the Splint incarnation's query, or `None` while another is
    /// outstanding.
    fn begin_query(
        &self,
        key: (SplintId, u64),
        connection_id: u64,
    ) -> Option<(u64, oneshot::Receiver<Vec<u8>>)> {
        let mut pending = self.pending.lock().unwrap();
        if pending.contains_key(&key) {
            return None;
        }
        let query_id = self.next_query_id.fetch_add(1, Ordering::Relaxed);
        let (answer, receiver) = oneshot::channel();
        pending.insert(
            key,
            PendingQuery {
                query_id,
                connection_id,
                answer,
            },
        );
        Some((query_id, receiver))
    }

    fn finish_query(&self, key: (SplintId, u64), query_id: u64) {
        let mut pending = self.pending.lock().unwrap();
        if pending
            .get(&key)
            .is_some_and(|query| query.query_id == query_id)
        {
            pending.remove(&key);
        }
    }

    /// Gives a query its answer. Only the connection the query was sent to
    /// answers it, once; any other answer is ignored and returns `false`.
    pub(super) fn answer(&self, connection_id: u64, query_id: u64, text: Vec<u8>) -> bool {
        let mut pending = self.pending.lock().unwrap();
        let Some(key) = pending
            .iter()
            .find(|(_, query)| query.query_id == query_id && query.connection_id == connection_id)
            .map(|(key, _)| *key)
        else {
            return false;
        };
        let query = pending.remove(&key).expect("the query was just found");
        query.answer.send(text).is_ok()
    }

    /// Reserves a set's memory from its route's budget and the daemon's.
    fn reserve(&self, route: &Arc<Semaphore>, bytes: usize) -> Result<SetMemory, &'static str> {
        let bytes = u32::try_from(bytes).map_err(|_| "clipboard_set_too_large")?;
        let route = Arc::clone(route)
            .try_acquire_many_owned(bytes)
            .map_err(|_| "clipboard_set_window_busy")?;
        let global = Arc::clone(&self.memory)
            .try_acquire_many_owned(bytes)
            .map_err(|_| "clipboard_set_memory_exhausted")?;
        Ok(SetMemory {
            _global: global,
            _route: route,
        })
    }

    /// Counts one of the Splint's clipboard audit records against its
    /// window, and forgets windows that have ended.
    fn admit_audit(&self, splint_id: SplintId, now: Instant) -> AuditAdmission {
        let mut audits = self.audits.lock().unwrap();
        if !audits.contains_key(&splint_id) {
            audits.retain(|_, window| now.duration_since(window.started) < AUDIT_WINDOW);
        }
        let window = audits.entry(splint_id).or_insert(AuditWindow {
            started: now,
            records: 0,
        });
        if now.duration_since(window.started) >= AUDIT_WINDOW {
            *window = AuditWindow {
                started: now,
                records: 0,
            };
        }
        window.records = window.records.saturating_add(1);
        match window.records {
            records if records <= AUDIT_RECORDS_PER_WINDOW => AuditAdmission::Record,
            records if records == AUDIT_RECORDS_PER_WINDOW + 1 => AuditAdmission::Suppressed,
            _ => AuditAdmission::Skip,
        }
    }
}

/// Coalesces one control subscription's sets so each selection is sent at
/// most once per [`SET_INTERVAL`], the latest set winning.
pub(super) struct SetPacer {
    waiting: [Option<Delivery>; 2],
    next_send: [Instant; 2],
}

impl Default for SetPacer {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            waiting: [None, None],
            next_send: [now, now],
        }
    }
}

impl SetPacer {
    const fn slot(selection: ClipboardSelection) -> usize {
        match selection {
            ClipboardSelection::Clipboard => 0,
            ClipboardSelection::Primary => 1,
        }
    }

    /// Takes a delivery and returns it if it should be sent now. A set that
    /// comes too soon waits, replacing an older waiting set.
    pub(super) fn offer(&mut self, delivery: Delivery, now: Instant) -> Option<Delivery> {
        let Delivery::Set { selection, .. } = &delivery else {
            return Some(delivery);
        };
        let slot = Self::slot(*selection);
        if self.waiting[slot].is_none() && now >= self.next_send[slot] {
            self.next_send[slot] = now + SET_INTERVAL;
            return Some(delivery);
        }
        self.waiting[slot] = Some(delivery);
        None
    }

    /// When the next waiting set is due.
    pub(super) fn deadline(&self) -> Option<Instant> {
        (0..2)
            .filter(|slot| self.waiting[*slot].is_some())
            .map(|slot| self.next_send[slot])
            .min()
    }

    /// A waiting set that is due now.
    pub(super) fn take_due(&mut self, now: Instant) -> Option<Delivery> {
        let slot =
            (0..2).find(|slot| self.waiting[*slot].is_some() && now >= self.next_send[*slot])?;
        self.next_send[slot] = now + SET_INTERVAL;
        self.waiting[slot].take()
    }
}

/// The subscription event a delivery becomes, and the memory it holds until
/// its frame is written.
pub(super) fn delivery_event(
    delivery: Delivery,
) -> (splinterm_protocol::SubscriptionEvent, Option<SetMemory>) {
    use splinterm_protocol::SubscriptionEvent;

    match delivery {
        Delivery::Set {
            selection,
            data,
            memory,
        } => (
            SubscriptionEvent::ClipboardSet { selection, data },
            Some(memory),
        ),
        Delivery::Query {
            query_id,
            selection,
        } => (
            SubscriptionEvent::ClipboardQuery {
                query_id,
                selection,
            },
            None,
        ),
    }
}

const fn protocol_selection(selection: TerminalSelection) -> ClipboardSelection {
    match selection {
        TerminalSelection::Clipboard => ClipboardSelection::Clipboard,
        TerminalSelection::Primary => ClipboardSelection::Primary,
    }
}

/// Receives every actor's clipboard requests until the daemon state ends.
pub(super) async fn run_broker(
    state: Weak<DaemonState>,
    mut events: mpsc::Receiver<ClipboardEvent>,
) {
    while let Some(event) = events.recv().await {
        let Some(state) = state.upgrade() else {
            break;
        };
        let ClipboardEvent {
            splint_id,
            incarnation,
            request,
        } = event;
        match request {
            ClipboardRequest::Set { selection, bytes } => {
                route_set(&state, splint_id, incarnation, selection, &bytes).await;
            }
            ClipboardRequest::Query { selection, seq } => {
                route_query(&state, (splint_id, incarnation), selection, seq).await;
            }
        }
    }
}

/// The controlling Window connection's route for the Splint incarnation:
/// the lease holder, when it is a Window with a control subscription.
async fn controlling_window(
    state: &DaemonState,
    splint_id: SplintId,
    incarnation: u64,
) -> Option<(u64, RouteSender)> {
    let connection_id = {
        let controller = state.controller.lock().await;
        let lease = controller
            .by_splint
            .get(&splint_id)
            .and_then(|controller_id| controller.by_id.get(controller_id))?;
        (lease.incarnation == incarnation).then_some(lease.connection_id)?
    };
    state
        .clipboard
        .route(connection_id, splint_id, incarnation)
        .map(|route| (connection_id, route))
}

async fn route_set(
    state: &DaemonState,
    splint_id: SplintId,
    incarnation: u64,
    selection: TerminalSelection,
    bytes: &[u8],
) {
    let byte_count = bytes.len();
    let reason = if std::str::from_utf8(bytes).is_err() {
        // Windows publish text; anything else is not offered as text.
        "clipboard_set_not_text"
    } else if let Some((_, route)) = controlling_window(state, splint_id, incarnation).await {
        match splinterm_protocol::encode_clipboard_data(bytes) {
            Ok(data) => match state.clipboard.reserve(&route.memory, data.len()) {
                Ok(memory) => {
                    let delivery = Delivery::Set {
                        selection: protocol_selection(selection),
                        data,
                        memory,
                    };
                    if route.sender.try_send(delivery).is_ok() {
                        "clipboard_set_sent_to_window"
                    } else {
                        "clipboard_set_window_busy"
                    }
                }
                Err(reason) => reason,
            },
            Err(_) => "clipboard_set_too_large",
        }
    } else {
        "clipboard_set_no_controlling_window"
    };
    // Sent is all the daemon knows: the Window may still refuse the set.
    let outcome = if reason == "clipboard_set_sent_to_window" {
        AuditOutcome::Succeeded
    } else {
        AuditOutcome::Failed
    };
    append_clipboard_audit(
        state,
        AuditOperation::ClipboardSet,
        (splint_id, incarnation),
        reason,
        outcome,
        byte_count,
    )
    .await;
}

async fn route_query(
    state: &Arc<DaemonState>,
    key: (SplintId, u64),
    selection: TerminalSelection,
    seq: u64,
) {
    let (splint_id, incarnation) = key;
    let refusal = match controlling_window(state, splint_id, incarnation).await {
        None => Err("clipboard_query_no_controlling_window"),
        // While another query of this Splint waits, this one is answered
        // empty now; the actor still writes it after the first.
        Some((connection_id, route)) => match state.clipboard.begin_query(key, connection_id) {
            None => Err("clipboard_query_already_pending"),
            Some((query_id, answer)) => {
                let delivery = Delivery::Query {
                    query_id,
                    selection: protocol_selection(selection),
                };
                if route.sender.try_send(delivery).is_ok() {
                    Ok((connection_id, query_id, answer))
                } else {
                    state.clipboard.finish_query(key, query_id);
                    Err("clipboard_query_window_busy")
                }
            }
        },
    };
    let (connection_id, query_id, answer) = match refusal {
        Ok(started) => started,
        Err(reason) => {
            finish_query(state, key, seq, &[], reason).await;
            return;
        }
    };
    let state = Arc::clone(state);
    tokio::spawn(async move {
        let (mut text, mut reason) = match time::timeout(QUERY_TIMEOUT, answer).await {
            Ok(Ok(text)) if text.is_empty() => (text, "clipboard_query_window_answered_empty"),
            Ok(Ok(text)) => (text, "clipboard_query_window_answered"),
            Ok(Err(_)) => (Vec::new(), "clipboard_query_window_gone"),
            Err(_) => (Vec::new(), "clipboard_query_timed_out"),
        };
        state.clipboard.finish_query(key, query_id);
        // A Window answers only while it still controls the Splint.
        if !text.is_empty()
            && controlling_window(&state, splint_id, incarnation)
                .await
                .is_none_or(|(controlling, _)| controlling != connection_id)
        {
            text.clear();
            reason = "clipboard_query_lease_moved";
        }
        finish_query(&state, key, seq, &text, reason).await;
    });
}

/// Gives the program its reply, empty unless a Window gave text, and audits
/// the query.
async fn finish_query(
    state: &DaemonState,
    (splint_id, incarnation): (SplintId, u64),
    seq: u64,
    text: &[u8],
    reason: &'static str,
) {
    let written = match super::current_handle(state, splint_id, incarnation).await {
        Ok(handle) => handle
            .write_clipboard_reply(seq, text.to_vec())
            .await
            .is_ok(),
        Err(_) => false,
    };
    let (reason, outcome) = match (written, text.is_empty()) {
        (false, _) => ("clipboard_query_reply_not_written", AuditOutcome::Failed),
        (true, false) => (reason, AuditOutcome::Succeeded),
        (true, true) => (reason, AuditOutcome::Failed),
    };
    append_clipboard_audit(
        state,
        AuditOperation::ClipboardQuery,
        (splint_id, incarnation),
        reason,
        outcome,
        text.len(),
    )
    .await;
}

async fn append_clipboard_audit(
    state: &DaemonState,
    operation: AuditOperation,
    (splint_id, incarnation): (SplintId, u64),
    reason: &'static str,
    outcome: AuditOutcome,
    byte_count: usize,
) {
    let admission = state.clipboard.admit_audit(splint_id, Instant::now());
    let (reason, outcome, byte_count) = match admission {
        AuditAdmission::Record => (
            reason,
            Some(outcome),
            Some(u64::try_from(byte_count).unwrap_or(u64::MAX)),
        ),
        AuditAdmission::Suppressed => ("clipboard_audit_suppressed", None, None),
        AuditAdmission::Skip => return,
    };
    let resource = super::daemon_splint_audit_resource(state, splint_id, incarnation).await;
    state.audit.lock().await.record(super::audit::AuditDraft {
        unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        policy_generation: None,
        policy_rule_id: None,
        peer: state.daemon_audit_peer.clone(),
        operation,
        resource,
        requested_scopes: Vec::new(),
        decision: AuditDecision::Allowed,
        reason,
        outcome,
        argument_count: None,
        executable_basename: None,
        byte_count,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route_memory() -> Arc<Semaphore> {
        Arc::new(Semaphore::new(ROUTE_MEMORY_BYTES))
    }

    fn set(routes: &ClipboardRoutes, selection: ClipboardSelection, data: &str) -> Delivery {
        Delivery::Set {
            selection,
            data: data.into(),
            memory: routes.reserve(&route_memory(), data.len()).unwrap(),
        }
    }

    fn set_data(delivery: Option<Delivery>) -> Option<String> {
        match delivery {
            Some(Delivery::Set { data, .. }) => Some(data),
            _ => None,
        }
    }

    #[test]
    fn sets_are_paced_per_selection_and_the_latest_wins() {
        let routes = ClipboardRoutes::default();
        let mut pacer = SetPacer::default();
        let start = Instant::now();
        let clipboard = ClipboardSelection::Clipboard;
        assert_eq!(
            set_data(pacer.offer(set(&routes, clipboard, "a"), start)),
            Some("a".into())
        );
        // A primary set is paced separately and goes at once.
        assert_eq!(
            set_data(pacer.offer(set(&routes, ClipboardSelection::Primary, "p"), start)),
            Some("p".into())
        );
        assert!(pacer.offer(set(&routes, clipboard, "b"), start).is_none());
        assert!(pacer.offer(set(&routes, clipboard, "c"), start).is_none());
        // Queries are never held back.
        assert!(matches!(
            pacer.offer(
                Delivery::Query {
                    query_id: 1,
                    selection: clipboard
                },
                start
            ),
            Some(Delivery::Query { .. })
        ));
        assert_eq!(pacer.deadline(), Some(start + SET_INTERVAL));
        assert!(pacer.take_due(start).is_none());
        assert_eq!(
            set_data(pacer.take_due(start + SET_INTERVAL)),
            Some("c".into())
        );
        assert_eq!(pacer.deadline(), None);
        // The replaced set released its memory.
        drop(pacer);
        assert_eq!(routes.memory.available_permits(), ROUTED_MEMORY_BYTES);
    }

    #[test]
    fn routed_memory_is_bounded_per_route_and_overall() {
        let routes = ClipboardRoutes::default();
        let first = route_memory();
        let held = routes.reserve(&first, ROUTE_MEMORY_BYTES).unwrap();
        assert_eq!(
            routes.reserve(&first, 1).unwrap_err(),
            "clipboard_set_window_busy"
        );
        // Other routes share what is left of the daemon's budget.
        let second = route_memory();
        let more = routes.reserve(&second, ROUTE_MEMORY_BYTES).unwrap();
        let third = route_memory();
        let left = ROUTED_MEMORY_BYTES - 2 * ROUTE_MEMORY_BYTES;
        assert_eq!(
            routes.reserve(&third, left + 1).unwrap_err(),
            "clipboard_set_memory_exhausted"
        );
        // A refused reservation holds nothing.
        assert_eq!(third.available_permits(), ROUTE_MEMORY_BYTES);
        drop((held, more));
        assert_eq!(routes.memory.available_permits(), ROUTED_MEMORY_BYTES);
        assert_eq!(first.available_permits(), ROUTE_MEMORY_BYTES);
        assert!(routes.reserve(&first, 1).is_ok());
    }

    #[tokio::test]
    async fn one_query_per_splint_incarnation_and_only_the_asked_connection_answers() {
        let routes = ClipboardRoutes::default();
        let splint_id = SplintId::new();
        let key = (splint_id, 1);
        let (query_id, answer) = routes.begin_query(key, 7).unwrap();
        assert!(routes.begin_query(key, 7).is_none());
        // Another Splint, or a relaunched one, asks on its own.
        assert!(routes.begin_query((SplintId::new(), 1), 7).is_some());
        assert!(routes.begin_query((splint_id, 2), 7).is_some());
        assert!(!routes.answer(8, query_id, b"stolen".to_vec()));
        assert!(!routes.answer(7, query_id + 100, b"stale".to_vec()));
        assert!(routes.answer(7, query_id, b"text".to_vec()));
        assert_eq!(answer.await.unwrap(), b"text");
        assert!(!routes.answer(7, query_id, b"again".to_vec()));
        assert!(routes.begin_query(key, 7).is_some());
    }

    #[tokio::test]
    async fn a_route_ends_with_its_subscription_but_not_with_a_replaced_one() {
        let routes = Arc::new(ClipboardRoutes::default());
        let splint_id = SplintId::new();
        let (old, _old_receiver) = routes.register(3, splint_id, 1);
        assert!(routes.route(3, splint_id, 1).is_some());
        assert!(routes.route(3, splint_id, 2).is_none());
        assert!(routes.route(4, splint_id, 1).is_none());
        let (new, _new_receiver) = routes.register(3, splint_id, 1);
        let (_, waiting) = routes.begin_query((splint_id, 1), 3).unwrap();
        drop(old);
        assert!(routes.route(3, splint_id, 1).is_some());
        // The replaced route leaves the query to the new one.
        assert_eq!(routes.pending.lock().unwrap().len(), 1);
        drop(new);
        assert!(routes.route(3, splint_id, 1).is_none());
        // Its query ends at once, so the program is answered empty.
        assert!(waiting.await.is_err());
    }

    #[tokio::test]
    async fn an_ended_route_ends_only_its_own_query() {
        let routes = Arc::new(ClipboardRoutes::default());
        let (splint_id, other_splint) = (SplintId::new(), SplintId::new());
        let (route, _receiver) = routes.register(3, splint_id, 1);
        let (_, _other_connection) = routes.begin_query((splint_id, 2), 4).unwrap();
        let (_, _other_route) = routes.begin_query((other_splint, 1), 3).unwrap();
        let (_, own) = routes.begin_query((splint_id, 1), 3).unwrap();
        drop(route);
        assert!(own.await.is_err());
        assert_eq!(routes.pending.lock().unwrap().len(), 2);
    }

    #[test]
    fn clipboard_audits_are_throttled_per_splint() {
        let routes = ClipboardRoutes::default();
        let (splint_id, other) = (SplintId::new(), SplintId::new());
        let start = Instant::now();
        for _ in 0..AUDIT_RECORDS_PER_WINDOW {
            assert_eq!(routes.admit_audit(splint_id, start), AuditAdmission::Record);
        }
        assert_eq!(
            routes.admit_audit(splint_id, start),
            AuditAdmission::Suppressed
        );
        let soon = start + Duration::from_secs(1);
        assert_eq!(routes.admit_audit(splint_id, soon), AuditAdmission::Skip);
        assert_eq!(routes.admit_audit(other, soon), AuditAdmission::Record);
        // A new window starts over, and ended windows are forgotten.
        let later = start + AUDIT_WINDOW;
        assert_eq!(routes.admit_audit(splint_id, later), AuditAdmission::Record);
        let later = soon + AUDIT_WINDOW;
        assert_eq!(
            routes.admit_audit(SplintId::new(), later),
            AuditAdmission::Record
        );
        let audits = routes.audits.lock().unwrap();
        assert!(!audits.contains_key(&other));
        assert!(audits.contains_key(&splint_id));
    }
}
