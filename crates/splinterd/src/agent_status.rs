//! Explicitly reported agent display status, held in daemon memory only.
//!
//! A status is display data bound to one live Splint incarnation. It never
//! grants authority, never bumps the topology revision, is never persisted or
//! carried through handoff, and its free-text detail never reaches audit,
//! diagnostics, or traces. One timer task drives TTL expiry, stale retention,
//! and coalesced publication for every Splint through an ordered deadline
//! index; no task is spawned per Splint.

use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    fs,
    sync::{Arc, Mutex as StdMutex, Weak},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use splinterm_core::{SplintId, SplintState};
use splinterm_protocol::{
    AccessScope, AgentState, AgentStatusReport, AgentStatusSummary, ErrorCode, ProtocolError,
    Request, Response, SubscriptionEvent, TopologySnapshot,
};
use tokio::{sync::Notify, task::JoinHandle, time};
use tracing::warn;

use crate::{
    DaemonState, MAX_LIVE_SPLINTS, PeerIdentity, RequestAuditDisposition,
    RequestAuthorizationContext, append_request_audit, audit_resource, internal, not_found,
    request_policy_resources,
};

/// An expired active status stays visible as stale for this long, then drops.
pub(crate) const STALE_RETENTION: Duration = Duration::from_secs(15 * 60);
/// Minimum spacing of published status events for one Splint.
pub(crate) const PUBLISH_INTERVAL: Duration = Duration::from_millis(250);
/// Admission window and limit for reports targeting one Splint.
pub(crate) const ADMISSION_WINDOW: Duration = Duration::from_secs(1);
pub(crate) const ADMISSIONS_PER_WINDOW: usize = 10;
/// Upper bound on the `/proc` parent walk from the reporting peer.
pub(crate) const MAX_ANCESTRY_STEPS: usize = 64;
const IDLE_TIMER: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug)]
struct CurrentStatus {
    state: AgentState,
    detail: Option<String>,
    reported_at: Instant,
    /// Deadline for an expiring status that is not yet stale.
    expires_at: Option<Instant>,
    stale_since: Option<Instant>,
}

#[derive(Debug)]
struct Slot {
    key: u64,
    incarnation: u64,
    current: Option<CurrentStatus>,
    /// Sequence of the latest visible change for this incarnation, or zero.
    sequence: u64,
    admissions: VecDeque<Instant>,
    last_published: Option<Instant>,
    /// When the latest unpublished change was made, if any.
    pending_since: Option<Instant>,
    deadline: Option<Instant>,
    rate_refusal_audited: bool,
}

impl Slot {
    fn summary(&self, now: Instant) -> Option<AgentStatusSummary> {
        let current = self.current.as_ref()?;
        Some(AgentStatusSummary {
            state: current.state,
            detail: current.detail.clone(),
            sequence: self.sequence,
            stale: current.stale_since.is_some(),
            reported_age_ms: u64::try_from(
                now.saturating_duration_since(current.reported_at)
                    .as_millis(),
            )
            .unwrap_or(u64::MAX),
        })
    }

    fn next_deadline(&self) -> Option<Instant> {
        let status = self.current.as_ref().and_then(|current| {
            current
                .stale_since
                .map_or(current.expires_at, |stale_since| {
                    Some(stale_since + STALE_RETENTION)
                })
        });
        let publish = self.pending_since.map(|pending_since| {
            self.last_published
                .map_or(pending_since, |published| published + PUBLISH_INTERVAL)
        });
        match (status, publish) {
            (Some(status), Some(publish)) => Some(status.min(publish)),
            (status, publish) => status.or(publish),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admission {
    Admitted,
    RateLimited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Accepted {
    pub(crate) sequence: u64,
    pub(crate) ttl: Option<Duration>,
    /// A visible change that must be published; identical refreshes are not.
    pub(crate) changed: bool,
    /// The earliest deadline moved earlier, so the sleeping timer must wake to
    /// reschedule. An identical refresh that adds or shortens a TTL sets this
    /// without being a visible change.
    pub(crate) wake_timer: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusEvent {
    pub(crate) splint_id: SplintId,
    pub(crate) incarnation: u64,
    pub(crate) status: Option<AgentStatusSummary>,
    pub(crate) sequence: u64,
}

impl From<StatusEvent> for SubscriptionEvent {
    fn from(event: StatusEvent) -> Self {
        Self::AgentStatusChanged {
            splint_id: event.splint_id,
            incarnation: event.incarnation,
            status: event.status,
            sequence: event.sequence,
        }
    }
}

/// At most one status per Splint, bound to one incarnation.
#[derive(Debug, Default)]
pub(crate) struct AgentStatusStore {
    slots: HashMap<SplintId, Slot>,
    keys: HashMap<u64, SplintId>,
    /// Ordered deadline index: one entry per scheduled slot.
    deadlines: BTreeSet<(Instant, u64)>,
    next_key: u64,
    sequence: u64,
}

impl AgentStatusStore {
    fn next_sequence(&mut self) -> u64 {
        self.sequence = self.sequence.saturating_add(1);
        self.sequence
    }

    fn reschedule(&mut self, splint_id: SplintId) {
        let Some(slot) = self.slots.get_mut(&splint_id) else {
            return;
        };
        if let Some(previous) = slot.deadline.take() {
            self.deadlines.remove(&(previous, slot.key));
        }
        slot.deadline = slot.next_deadline();
        if let Some(deadline) = slot.deadline {
            self.deadlines.insert((deadline, slot.key));
        }
    }

    fn remove(&mut self, splint_id: SplintId) {
        if let Some(slot) = self.slots.remove(&splint_id) {
            self.keys.remove(&slot.key);
            if let Some(deadline) = slot.deadline {
                self.deadlines.remove(&(deadline, slot.key));
            }
        }
    }

    /// Removes the status of `splint_id` only while it still belongs to
    /// `incarnation`.
    pub(crate) fn remove_incarnation(&mut self, splint_id: SplintId, incarnation: u64) {
        if self
            .slots
            .get(&splint_id)
            .is_some_and(|slot| slot.incarnation == incarnation)
        {
            self.remove(splint_id);
        }
    }

    /// Counts one report against the per-Splint admission window. A new
    /// incarnation starts from an empty slot.
    ///
    /// # Errors
    ///
    /// Returns `ResourceLimit` if the bounded slot map is full.
    pub(crate) fn admit(
        &mut self,
        splint_id: SplintId,
        incarnation: u64,
        now: Instant,
    ) -> Result<Admission, ProtocolError> {
        if self
            .slots
            .get(&splint_id)
            .is_some_and(|slot| slot.incarnation != incarnation)
        {
            self.remove(splint_id);
        }
        if !self.slots.contains_key(&splint_id) {
            if self.slots.len() >= MAX_LIVE_SPLINTS {
                return Err(ProtocolError::new(
                    ErrorCode::ResourceLimit,
                    "agent status capacity is exhausted",
                ));
            }
            self.next_key = self.next_key.wrapping_add(1);
            let key = self.next_key;
            self.keys.insert(key, splint_id);
            self.slots.insert(
                splint_id,
                Slot {
                    key,
                    incarnation,
                    current: None,
                    sequence: 0,
                    admissions: VecDeque::with_capacity(ADMISSIONS_PER_WINDOW),
                    last_published: None,
                    pending_since: None,
                    deadline: None,
                    rate_refusal_audited: false,
                },
            );
        }
        let slot = self.slots.get_mut(&splint_id).expect("slot was ensured");
        while slot
            .admissions
            .front()
            .is_some_and(|admitted| now.saturating_duration_since(*admitted) >= ADMISSION_WINDOW)
        {
            slot.admissions.pop_front();
        }
        if slot.admissions.len() >= ADMISSIONS_PER_WINDOW {
            return Ok(Admission::RateLimited);
        }
        slot.admissions.push_back(now);
        Ok(Admission::Admitted)
    }

    /// Returns true exactly once per incarnation, for the audit of a
    /// self-report rate refusal.
    pub(crate) fn claim_rate_refusal_audit(
        &mut self,
        splint_id: SplintId,
        incarnation: u64,
    ) -> bool {
        self.slots
            .get_mut(&splint_id)
            .filter(|slot| slot.incarnation == incarnation)
            .is_some_and(|slot| !std::mem::replace(&mut slot.rate_refusal_audited, true))
    }

    /// Applies an admitted report. `None` clears.
    pub(crate) fn apply(
        &mut self,
        splint_id: SplintId,
        incarnation: u64,
        report: Option<&AgentStatusReport>,
        now: Instant,
    ) -> Accepted {
        let earliest_before = self.next_deadline();
        let identical = self.slots.get(&splint_id).is_some_and(|slot| {
            slot.incarnation == incarnation
                && slot
                    .current
                    .as_ref()
                    .zip(report)
                    .is_some_and(|(current, report)| {
                        current.stale_since.is_none()
                            && current.state == report.state
                            && current.detail == report.detail
                    })
        });
        let had_status = self
            .slots
            .get(&splint_id)
            .is_some_and(|slot| slot.incarnation == incarnation && slot.current.is_some());
        let ttl = report
            .and_then(AgentStatusReport::effective_ttl_seconds)
            .map(|seconds| Duration::from_secs(u64::from(seconds)));
        let changed = !identical && (report.is_some() || had_status);
        let sequence = changed.then(|| self.next_sequence());
        let Some(slot) = self
            .slots
            .get_mut(&splint_id)
            .filter(|slot| slot.incarnation == incarnation)
        else {
            return Accepted {
                sequence: 0,
                ttl,
                changed: false,
                wake_timer: false,
            };
        };
        match report {
            Some(_) if identical => {
                let current = slot.current.as_mut().expect("identical status exists");
                current.reported_at = now;
                current.expires_at = ttl.map(|ttl| now + ttl);
            }
            Some(report) => {
                slot.current = Some(CurrentStatus {
                    state: report.state,
                    detail: report.detail.clone(),
                    reported_at: now,
                    expires_at: ttl.map(|ttl| now + ttl),
                    stale_since: None,
                });
            }
            None => slot.current = None,
        }
        if let Some(sequence) = sequence {
            slot.sequence = sequence;
            slot.pending_since = Some(now);
        }
        let sequence = slot.sequence;
        self.reschedule(splint_id);
        let wake_timer = changed
            || self
                .next_deadline()
                .is_some_and(|earliest| earliest_before.is_none_or(|before| earliest < before));
        Accepted {
            sequence,
            ttl,
            changed,
            wake_timer,
        }
    }

    /// Drops every slot whose Splint is not live at the recorded incarnation.
    pub(crate) fn retain_live(&mut self, live: &HashMap<SplintId, u64>) {
        let retired = self
            .slots
            .iter()
            .filter(|(splint_id, slot)| live.get(splint_id) != Some(&slot.incarnation))
            .map(|(splint_id, _)| *splint_id)
            .collect::<Vec<_>>();
        for splint_id in retired {
            self.remove(splint_id);
        }
    }

    pub(crate) fn summary(
        &self,
        splint_id: SplintId,
        incarnation: u64,
        now: Instant,
    ) -> Option<AgentStatusSummary> {
        self.slots
            .get(&splint_id)
            .filter(|slot| slot.incarnation == incarnation)
            .and_then(|slot| slot.summary(now))
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.deadlines.first().map(|(deadline, _)| *deadline)
    }

    /// Applies expiry and stale retention, then returns the coalesced events
    /// whose publication interval has elapsed.
    pub(crate) fn take_due(&mut self, now: Instant) -> Vec<StatusEvent> {
        let due = self
            .deadlines
            .range(..=(now, u64::MAX))
            .filter_map(|(_, key)| self.keys.get(key).copied())
            .collect::<Vec<_>>();
        let mut events = Vec::new();
        for splint_id in due {
            let transition = self.slots[&splint_id]
                .current
                .as_ref()
                .is_some_and(|current| {
                    let expired = current.stale_since.is_none()
                        && current.expires_at.is_some_and(|expires| expires <= now);
                    let dropped = current
                        .stale_since
                        .is_some_and(|stale_since| stale_since + STALE_RETENTION <= now);
                    expired || dropped
                });
            let sequence = transition.then(|| self.next_sequence());
            let slot = self.slots.get_mut(&splint_id).expect("indexed slot exists");
            if let Some(sequence) = sequence {
                let current = slot.current.as_mut().expect("status exists");
                if current.stale_since.is_some() {
                    slot.current = None;
                } else {
                    current.stale_since = Some(now);
                    current.expires_at = None;
                }
                slot.sequence = sequence;
                slot.pending_since = Some(now);
            }
            if slot.pending_since.is_some()
                && slot
                    .last_published
                    .is_none_or(|published| published + PUBLISH_INTERVAL <= now)
            {
                slot.pending_since = None;
                slot.last_published = Some(now);
                events.push(StatusEvent {
                    splint_id,
                    incarnation: slot.incarnation,
                    status: slot.summary(now),
                    sequence: slot.sequence,
                });
            }
            self.reschedule(splint_id);
        }
        events
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.slots.len()
    }
}

/// Daemon-owned status store plus the wakeup for its single timer task.
#[derive(Debug, Default)]
pub(crate) struct AgentStatusService {
    store: StdMutex<AgentStatusStore>,
    changed: Notify,
    /// Timer wakeups by notification rather than by deadline.
    #[cfg(test)]
    pub(crate) notified_wakes: std::sync::atomic::AtomicUsize,
}

impl AgentStatusService {
    fn store(&self) -> std::sync::MutexGuard<'_, AgentStatusStore> {
        self.store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// How many Splints hold a status slot.
    #[cfg(test)]
    pub(crate) fn slot_count(&self) -> usize {
        self.store().len()
    }

    /// Prunes statuses of retired incarnations and fills runtime summaries.
    ///
    /// Every topology snapshot passes through here, so the transaction that
    /// publishes an exit, relaunch, restore, close, or termination also drops
    /// the retired incarnation's status.
    pub(crate) fn attach(&self, snapshot: &mut TopologySnapshot) {
        let live = snapshot
            .runtimes
            .iter()
            .filter_map(|runtime| Some((runtime.splint_id, runtime.live_incarnation?)))
            .collect::<HashMap<_, _>>();
        let now = Instant::now();
        let mut store = self.store();
        store.retain_live(&live);
        for runtime in &mut snapshot.runtimes {
            runtime.agent_status = runtime
                .live_incarnation
                .and_then(|incarnation| store.summary(runtime.splint_id, incarnation, now));
        }
    }
}

/// Starts the one daemon-wide status timer. It holds only a weak daemon
/// reference and is aborted when the returned handle is dropped.
pub(crate) fn spawn_timer(state: &Arc<DaemonState>) -> JoinHandle<()> {
    let service = Arc::clone(&state.agent_status);
    let state = Arc::downgrade(state);
    tokio::spawn(run_timer(service, state))
}

async fn run_timer(service: Arc<AgentStatusService>, state: Weak<DaemonState>) {
    loop {
        let deadline = service
            .store()
            .next_deadline()
            .unwrap_or_else(|| Instant::now() + IDLE_TIMER);
        tokio::select! {
            () = service.changed.notified() => {
                #[cfg(test)]
                service
                    .notified_wakes
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            () = time::sleep_until(time::Instant::from_std(deadline)) => {}
        }
        let Some(state) = state.upgrade() else {
            break;
        };
        publish_due(&state).await;
    }
}

/// Publishes due status events in topology-transaction order, so a status
/// event never precedes an older topology snapshot on any subscription.
async fn publish_due(state: &DaemonState) {
    // A change coalesced inside its publication interval wakes the timer
    // only to reschedule; skip the transaction and snapshot until due.
    if state
        .agent_status
        .store()
        .next_deadline()
        .is_none_or(|deadline| deadline > Instant::now())
    {
        return;
    }
    let Ok(_transaction) = state.topology_transactions.acquire().await else {
        return;
    };
    // Building the snapshot prunes statuses of retired incarnations first.
    let snapshot = crate::topology_snapshot(state).await;
    let events = state.agent_status.store().take_due(Instant::now());
    if events.is_empty() {
        return;
    }
    let mut hub = state.topology_hub.lock().await;
    for event in events {
        hub.publish_status(event.into(), snapshot.revision);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelfReportPath {
    Session,
    Ancestry,
}

/// The `/proc/<pid>/stat` fields that peer location and foreground app
/// naming (`foreground_app`) read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcStat {
    pub(crate) parent: u32,
    pub(crate) process_group: u32,
    pub(crate) session: u32,
    /// The process group holding the controlling terminal, or -1 without one.
    pub(crate) terminal_process_group: i32,
    /// Clock ticks since boot; tells a process from a later one reusing its PID.
    pub(crate) start_time: u64,
}

/// Parses `/proc/<pid>/stat`. The command name may hold any bytes, including
/// `)` and invalid UTF-8, so the fields are read after its final `)`. The
/// start time comes from the library's one start-time parser
/// ([`splinterd::handoff_window_carry::stat_start_time`]), which a handoff
/// candidate also checks carried Windows with.
fn parse_proc_stat(contents: &[u8]) -> Option<ProcStat> {
    let close = contents.iter().rposition(|&byte| byte == b')')?;
    let rest = std::str::from_utf8(&contents[close + 1..]).ok()?;
    let mut fields = rest.split_ascii_whitespace();
    let _state = fields.next()?;
    let parent = fields.next()?.parse().ok()?;
    let process_group = fields.next()?.parse().ok()?;
    let session = fields.next()?.parse().ok()?;
    let _terminal = fields.next()?;
    let terminal_process_group = fields.next()?.parse().ok()?;
    let start_time = splinterd::handoff_window_carry::stat_start_time(contents)?;
    Some(ProcStat {
        parent,
        process_group,
        session,
        terminal_process_group,
        start_time,
    })
}

pub(crate) fn read_proc_stat(pid: u32) -> Option<ProcStat> {
    parse_proc_stat(&fs::read(format!("/proc/{pid}/stat")).ok()?)
}

/// The start time of `pid`, recorded when a connection is verified and when
/// a Window's pin is checked; the same reader a handoff candidate uses.
pub(crate) fn process_start_time(pid: u32) -> Option<u64> {
    splinterd::handoff_window_carry::process_start_time(pid)
}

/// Locates the peer inside the Splint whose live session leader is
/// `child_pid`. The validated PTY identity requires child, process group, and
/// session to be one PID, so the recorded session ID is `child_pid`. The peer
/// must be in that session, or a bounded parent walk must reach the child.
/// Anything else, including a process detached with `setsid` and reparented
/// to init or `systemd --user`, is outside and needs a grant. Sharing the
/// Splint's cgroup proves nothing: any same-user process can join it.
pub(crate) fn locate_peer(
    peer_pid: u32,
    peer_start_time: u64,
    child_pid: u32,
) -> Option<SelfReportPath> {
    locate_peer_with(peer_pid, peer_start_time, child_pid, read_proc_stat)
}

fn locate_peer_with(
    peer_pid: u32,
    peer_start_time: u64,
    child_pid: u32,
    read: impl Fn(u32) -> Option<ProcStat>,
) -> Option<SelfReportPath> {
    if peer_pid <= 1 || child_pid <= 1 {
        return None;
    }
    let ancestry = walk_ancestry_with(peer_pid, peer_start_time, read)?;
    if ancestry.session == child_pid {
        return Some(SelfReportPath::Session);
    }
    ancestry
        .descends_from(child_pid)
        .then_some(SelfReportPath::Ancestry)
}

/// One process's place in the process tree, from one bounded `/proc` walk
/// ([`walk_ancestry_with`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ancestry {
    /// The process's session ID.
    pub(crate) session: u32,
    /// Every parent the walk reached, nearest first, at most
    /// [`MAX_ANCESTRY_STEPS`]; the process itself is not among them. When the
    /// walk stopped early, the last one is the parent it could not prove.
    pub(crate) ancestors: Vec<u32>,
    /// Whether the walk reached init (a parent of PID 0 or 1) with every
    /// ancestor read and started no later than its child. Only a complete
    /// walk proves that a process is outside a tree; an incomplete one
    /// proves only what it reached.
    pub(crate) complete: bool,
}

impl Ancestry {
    /// Whether the walk reached `pid` as a parent.
    pub(crate) fn descends_from(&self, pid: u32) -> bool {
        self.ancestors.contains(&pid)
    }
}

/// Walks from `pid`, which must still be the process that started at
/// `start_time`, towards init through at most [`MAX_ANCESTRY_STEPS`]
/// parents. A parent that cannot be read, or that started after its child
/// (it exited during the walk and its PID now names another process), ends
/// the walk incomplete. `None` when `pid` is not that process.
///
/// Agent status self-reports ([`locate_peer`]) and consent responder
/// eligibility (`remote_consent`) share this one walk.
pub(crate) fn walk_ancestry_with(
    pid: u32,
    start_time: u64,
    read: impl Fn(u32) -> Option<ProcStat>,
) -> Option<Ancestry> {
    if pid <= 1 {
        return None;
    }
    let process = read(pid)?;
    // The PID must still name the process that opened the connection.
    if process.start_time != start_time {
        return None;
    }
    let mut ancestry = Ancestry {
        session: process.session,
        ancestors: Vec::new(),
        complete: false,
    };
    let (mut parent, mut child_start) = (process.parent, process.start_time);
    for _ in 0..MAX_ANCESTRY_STEPS {
        if parent <= 1 {
            ancestry.complete = true;
            break;
        }
        ancestry.ancestors.push(parent);
        let Some(ancestor) = read(parent) else {
            break;
        };
        // A parent starts no later than its child. A later start means the
        // parent exited during the walk and its PID now names another process.
        if ancestor.start_time > child_start {
            break;
        }
        parent = ancestor.parent;
        child_start = ancestor.start_time;
    }
    Some(ancestry)
}

struct LiveTarget {
    incarnation: u64,
    handle: splinterd::LiveSplintHandle,
}

async fn live_target(
    state: &DaemonState,
    splint_id: SplintId,
) -> Result<LiveTarget, ProtocolError> {
    let exited = state
        .topology
        .read()
        .await
        .find_splint(splint_id)
        .map(|splint| matches!(splint.state, SplintState::Exited(_)))
        .ok_or_else(not_found)?;
    let handle = state
        .runtimes
        .lock()
        .await
        .handle(splint_id)
        .ok_or_else(not_found)?;
    if exited || !handle.root_process_is_live() {
        return Err(stale_incarnation());
    }
    Ok(LiveTarget {
        incarnation: handle.incarnation.value(),
        handle,
    })
}

fn stale_incarnation() -> ProtocolError {
    ProtocolError::new(
        ErrorCode::StaleIncarnation,
        "agent status incarnation is not live",
    )
}

fn rate_limited() -> ProtocolError {
    ProtocolError::new(
        ErrorCode::RateLimited,
        "agent status reports exceed 10 per second for this Splint; retry later",
    )
}

/// The Unix-millisecond wall time `ttl` after `accepted_at`.
///
/// The store schedules expiry from the monotonic instant captured with
/// `accepted_at`, so deriving the advertised expiry from the same moment keeps
/// it equal to the timer's deadline however long acceptance then waits.
fn unix_millis_after(accepted_at: SystemTime, ttl: Duration) -> u64 {
    accepted_at
        .checked_add(ttl)
        .and_then(|deadline| deadline.duration_since(UNIX_EPOCH).ok())
        .map_or(u64::MAX, |deadline| {
            u64::try_from(deadline.as_millis()).unwrap_or(u64::MAX)
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReportAuthority {
    SelfReport,
    Granted,
}

async fn accept_report(
    state: &DaemonState,
    peer: &PeerIdentity,
    request: &Request,
    target: &LiveTarget,
    authority: ReportAuthority,
) -> Result<Response, ProtocolError> {
    let Request::ReportAgentStatus {
        splint_id,
        incarnation,
        status,
    } = request
    else {
        return Err(internal());
    };
    let (splint_id, incarnation) = (*splint_id, *incarnation);
    if let Some(report) = status {
        report.validate()?;
    }
    if target.incarnation != incarnation {
        return Err(stale_incarnation());
    }
    let now = Instant::now();
    let accepted_at = SystemTime::now();
    let admission = state
        .agent_status
        .store()
        .admit(splint_id, incarnation, now)?;
    if admission == Admission::RateLimited {
        let audit_once = authority == ReportAuthority::SelfReport
            && state
                .agent_status
                .store()
                .claim_rate_refusal_audit(splint_id, incarnation);
        if audit_once {
            let resource = request_policy_resources(request, state)
                .await
                .unwrap_or_default()
                .first()
                .copied()
                .and_then(audit_resource);
            append_request_audit(
                state,
                peer,
                request,
                None,
                RequestAuditDisposition {
                    resource,
                    decision: splinterm_protocol::AuditDecision::Rejected,
                    reason: "self_report_rate_limited",
                    outcome: splinterm_protocol::AuditOutcome::Failed,
                },
            )
            .await;
        }
        return Err(rate_limited());
    }
    let accepted = state
        .agent_status
        .store()
        .apply(splint_id, incarnation, status.as_ref(), now);
    // A concurrent exit may have published between the liveness check and the
    // insertion; never leave a status behind for a retired incarnation.
    if live_target(state, splint_id)
        .await
        .is_ok_and(|current| current.incarnation == incarnation)
    {
        if accepted.wake_timer {
            state.agent_status.changed.notify_one();
        }
    } else {
        state
            .agent_status
            .store()
            .remove_incarnation(splint_id, incarnation);
        return Err(stale_incarnation());
    }
    Ok(Response::AgentStatusAccepted {
        sequence: accepted.sequence,
        expires_at_ms: accepted.ttl.map(|ttl| unix_millis_after(accepted_at, ttl)),
    })
}

/// Accepts a report from a process inside the target Splint without a grant
/// and without a per-update audit record. Returns `None` when the peer is not
/// inside the Splint, so ordinary authorization and audit apply.
pub(crate) async fn handle_self_report(
    state: &DaemonState,
    peer: &PeerIdentity,
    request: &Request,
) -> Option<Result<Response, ProtocolError>> {
    let Request::ReportAgentStatus { splint_id, .. } = request else {
        return None;
    };
    // Only pidfd-monitored connections with a recorded start time: peer exit
    // closes the connection, and the start time rejects a reused PID.
    peer.persistent_executable()?;
    let peer_start_time = peer.start_time()?;
    let target = if let Ok(target) = live_target(state, *splint_id).await {
        target
    } else {
        // A Splint's first process can report before the transaction that
        // creates, relaunches, or restores it commits; wait that out once.
        drop(state.topology_transactions.acquire().await.ok()?);
        live_target(state, *splint_id).await.ok()?
    };
    let (peer_pid, child_pid) = (peer.pid, target.handle.child_pid());
    let located =
        tokio::task::spawn_blocking(move || locate_peer(peer_pid, peer_start_time, child_pid))
            .await
            .unwrap_or_else(|error| {
                warn!(%error, "agent status peer lookup task failed");
                None
            })?;
    // The session leader is still the live child, so its PID and session ID
    // could not have been reused during the walk.
    if !target.handle.root_process_is_live() {
        return None;
    }
    tracing::debug!(path = ?located, "agent status self-report");
    Some(accept_report(state, peer, request, &target, ReportAuthority::SelfReport).await)
}

async fn splint_grant_authorizes(
    state: &DaemonState,
    peer: &PeerIdentity,
    splint_id: SplintId,
    incarnation: u64,
) -> bool {
    state
        .grants
        .lock()
        .await
        .authorize(
            peer,
            splint_id,
            incarnation,
            &[AccessScope::AgentStatusReport],
        )
        .is_some()
}

/// The Splint's newest incarnation, whether still running or exited.
async fn newest_incarnation(state: &DaemonState, splint_id: SplintId) -> Option<u64> {
    if let Some(handle) = state.runtimes.lock().await.handle(splint_id) {
        return Some(handle.incarnation.value());
    }
    state
        .topology
        .read()
        .await
        .find_splint(splint_id)?
        .last_incarnation
}

/// Accepts a report that passed ordinary authorization: persistent policy,
/// a Lair grant, a Splint grant with `agent_status_report`, or the
/// development bypass. The trusted UI may only clear.
pub(crate) async fn handle_authorized_report(
    state: &DaemonState,
    peer: &PeerIdentity,
    authorization: &RequestAuthorizationContext,
    request: &Request,
) -> Result<Response, ProtocolError> {
    let Request::ReportAgentStatus {
        splint_id,
        incarnation,
        status,
    } = request
    else {
        return Err(internal());
    };
    let granted = authorization.policy_authorized()
        || authorization.authorized_grant_id().is_some()
        || state.development_terminal_access;
    if !granted {
        if authorization.interactive_bypass {
            if status.is_some() {
                return Err(ProtocolError::new(
                    ErrorCode::Unauthorized,
                    "the trusted UI may only clear agent status",
                ));
            }
        } else if !splint_grant_authorizes(state, peer, *splint_id, *incarnation).await {
            // A Splint grant names one incarnation. A reporter whose grant
            // covers the Splint's newest incarnation is authorized, so its
            // stale report is answered as stale, as on every other path; a
            // peer without such a grant learns nothing about liveness.
            let newest = newest_incarnation(state, *splint_id).await;
            if let Some(newest) = newest.filter(|newest| newest != incarnation)
                && splint_grant_authorizes(state, peer, *splint_id, newest).await
            {
                return Err(stale_incarnation());
            }
            return Err(ProtocolError::new(
                ErrorCode::Unauthorized,
                "agent status requires a process inside the Splint or an agent_status_report grant",
            ));
        }
    }
    // A policy rule matched the incarnation that was current at authorization.
    // A report naming any other incarnation, including one a concurrent
    // relaunch has made current since, was not authorized by that match; it
    // is answered as stale, and accept_report rechecks liveness after applying.
    if authorization.policy_authorized()
        && authorization.authorized_grant_id().is_none()
        && !state.development_terminal_access
        && authorization.policy_splint_incarnation() != Some(*incarnation)
    {
        return Err(stale_incarnation());
    }
    let target = live_target(state, *splint_id).await?;
    accept_report(state, peer, request, &target, ReportAuthority::Granted).await
}

#[cfg(test)]
mod tests {
    use super::*;

    // The advertised expiry is the acceptance instant plus the TTL, however
    // long acceptance waits before answering.
    #[test]
    fn advertised_expiry_counts_from_the_acceptance_instant() {
        let accepted_at = UNIX_EPOCH + Duration::from_millis(1_760_000_000_123);
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(
            unix_millis_after(accepted_at, Duration::from_secs(60)),
            1_760_000_060_123
        );
    }

    fn report(state: AgentState, detail: Option<&str>, ttl: Option<u32>) -> AgentStatusReport {
        AgentStatusReport {
            state,
            detail: detail.map(str::to_owned),
            ttl_seconds: ttl,
        }
    }

    fn admitted(store: &mut AgentStatusStore, splint_id: SplintId, now: Instant) {
        assert_eq!(store.admit(splint_id, 1, now).unwrap(), Admission::Admitted);
    }

    #[test]
    fn active_status_expires_to_stale_then_drops_after_retention() {
        let mut store = AgentStatusStore::default();
        let splint_id = SplintId::new();
        let start = Instant::now();
        admitted(&mut store, splint_id, start);
        let accepted = store.apply(
            splint_id,
            1,
            Some(&report(AgentState::Working, Some("compiling"), None)),
            start,
        );
        assert!(accepted.changed);
        assert_eq!(accepted.ttl, Some(Duration::from_secs(120)));
        let first = store.take_due(start);
        assert_eq!(first.len(), 1, "the first change publishes immediately");
        assert_eq!(first[0].status.as_ref().unwrap().state, AgentState::Working);
        assert_eq!(
            store.next_deadline(),
            Some(start + Duration::from_secs(120))
        );

        assert!(store.take_due(start + Duration::from_secs(119)).is_empty());
        let expiry = start + Duration::from_secs(120);
        let stale = store.take_due(expiry);
        assert_eq!(stale.len(), 1);
        let summary = stale[0].status.as_ref().unwrap();
        assert!(summary.stale);
        assert_eq!(summary.state, AgentState::Working);
        assert_eq!(summary.detail.as_deref(), Some("compiling"));
        assert_eq!(summary.reported_age_ms, 120_000);
        assert!(stale[0].sequence > first[0].sequence);
        assert!(store.summary(splint_id, 1, expiry).unwrap().stale);

        assert_eq!(store.next_deadline(), Some(expiry + STALE_RETENTION));
        assert!(
            store
                .take_due(expiry + (STALE_RETENTION - Duration::from_secs(1)))
                .is_empty()
        );
        let dropped = store.take_due(expiry + STALE_RETENTION);
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].status, None);
        assert!(dropped[0].sequence > stale[0].sequence);
        assert_eq!(store.summary(splint_id, 1, expiry + STALE_RETENTION), None);
        assert_eq!(store.next_deadline(), None);
    }

    #[test]
    fn resting_status_is_untimed_unless_a_ttl_is_reported() {
        let mut store = AgentStatusStore::default();
        let splint_id = SplintId::new();
        let start = Instant::now();
        admitted(&mut store, splint_id, start);
        let done = store.apply(
            splint_id,
            1,
            Some(&report(AgentState::Done, None, None)),
            start,
        );
        assert_eq!(done.ttl, None);
        store.take_due(start);
        assert_eq!(store.next_deadline(), None);
        assert!(
            !store
                .summary(splint_id, 1, start + Duration::from_secs(86_400))
                .unwrap()
                .stale
        );
        admitted(&mut store, splint_id, start);
        let timed = store.apply(
            splint_id,
            1,
            Some(&report(AgentState::Error, None, Some(30))),
            start,
        );
        assert_eq!(timed.ttl, Some(Duration::from_secs(30)));
        let events = store.take_due(start + Duration::from_secs(30));
        assert!(events[0].status.as_ref().unwrap().stale);
    }

    // An identical report is not a visible change, but adding or shortening
    // its TTL moves the earliest deadline earlier, so the timer sleeping on
    // the old deadline (or on its idle wait) must still wake.
    #[test]
    fn identical_report_that_adds_or_shortens_a_ttl_wakes_the_timer() {
        let mut store = AgentStatusStore::default();
        let splint_id = SplintId::new();
        let start = Instant::now();
        admitted(&mut store, splint_id, start);
        let untimed = store.apply(
            splint_id,
            1,
            Some(&report(AgentState::Done, None, None)),
            start,
        );
        assert!(untimed.changed && untimed.wake_timer);
        assert_eq!(store.take_due(start).len(), 1);
        assert_eq!(store.next_deadline(), None, "the timer idles");

        let mut at = start;
        for (ttl, wakes) in [
            (Some(60), true),   // adds a TTL to an untimed status
            (Some(30), true),   // shortens it
            (Some(900), false), // lengthens it: waking early is not needed
            (None, false),      // drops it
        ] {
            at += Duration::from_secs(1);
            admitted(&mut store, splint_id, at);
            let accepted =
                store.apply(splint_id, 1, Some(&report(AgentState::Done, None, ttl)), at);
            assert!(!accepted.changed, "{ttl:?}");
            assert_eq!(accepted.wake_timer, wakes, "{ttl:?}");
            assert_eq!(
                store.next_deadline(),
                ttl.map(|ttl| at + Duration::from_secs(u64::from(ttl)))
            );
        }
    }

    #[test]
    fn identical_report_refreshes_ttl_without_publishing() {
        let mut store = AgentStatusStore::default();
        let splint_id = SplintId::new();
        let start = Instant::now();
        let working = report(AgentState::Working, Some("tests"), Some(10));
        admitted(&mut store, splint_id, start);
        let first = store.apply(splint_id, 1, Some(&working), start);
        assert_eq!(store.take_due(start).len(), 1);

        let later = start + Duration::from_secs(9);
        admitted(&mut store, splint_id, later);
        let refreshed = store.apply(splint_id, 1, Some(&working), later);
        assert!(!refreshed.changed);
        assert_eq!(refreshed.sequence, first.sequence);
        assert_eq!(store.next_deadline(), Some(later + Duration::from_secs(10)));
        assert!(store.take_due(start + Duration::from_secs(10)).is_empty());
        assert_eq!(
            store
                .summary(splint_id, 1, later + Duration::from_secs(1))
                .unwrap()
                .reported_age_ms,
            1_000
        );

        // A refresh of a stale status is a visible change again.
        let stale_at = later + Duration::from_secs(10);
        assert!(store.take_due(stale_at)[0].status.as_ref().unwrap().stale);
        admitted(&mut store, splint_id, stale_at);
        let revived = store.apply(splint_id, 1, Some(&working), stale_at);
        assert!(revived.changed);
        assert!(revived.sequence > first.sequence);
        assert!(!store.summary(splint_id, 1, stale_at).unwrap().stale);
    }

    #[test]
    fn publication_coalesces_to_the_latest_value_every_quarter_second() {
        let mut store = AgentStatusStore::default();
        let splint_id = SplintId::new();
        let start = Instant::now();
        admitted(&mut store, splint_id, start);
        store.apply(
            splint_id,
            1,
            Some(&report(AgentState::Working, None, None)),
            start,
        );
        assert_eq!(store.take_due(start).len(), 1);
        for (offset, state) in [
            (10, AgentState::NeedsInput),
            (20, AgentState::Error),
            (30, AgentState::Done),
        ] {
            let now = start + Duration::from_millis(offset);
            admitted(&mut store, splint_id, now);
            assert!(
                store
                    .apply(splint_id, 1, Some(&report(state, None, None)), now)
                    .changed
            );
            assert!(store.take_due(now).is_empty(), "coalesced within 250 ms");
        }
        assert_eq!(
            store.next_deadline(),
            Some(start + PUBLISH_INTERVAL),
            "one deadline carries the pending publication"
        );
        let events = store.take_due(start + PUBLISH_INTERVAL);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].status.as_ref().unwrap().state, AgentState::Done);
        assert_eq!(store.next_deadline(), None);
    }

    #[test]
    fn clear_publishes_once_and_a_clear_without_status_is_silent() {
        let mut store = AgentStatusStore::default();
        let splint_id = SplintId::new();
        let start = Instant::now();
        admitted(&mut store, splint_id, start);
        let silent = store.apply(splint_id, 1, None, start);
        assert!(!silent.changed);
        assert_eq!(silent.sequence, 0);
        assert!(store.take_due(start).is_empty());
        admitted(&mut store, splint_id, start);
        store.apply(
            splint_id,
            1,
            Some(&report(AgentState::Idle, None, None)),
            start,
        );
        store.take_due(start);
        let later = start + Duration::from_secs(1);
        admitted(&mut store, splint_id, later);
        let cleared = store.apply(splint_id, 1, None, later);
        assert!(cleared.changed);
        let events = store.take_due(later);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].status, None);
        assert_eq!(events[0].sequence, cleared.sequence);
    }

    #[test]
    fn admission_is_limited_to_ten_reports_per_second_per_splint() {
        let mut store = AgentStatusStore::default();
        let splint_id = SplintId::new();
        let other = SplintId::new();
        let start = Instant::now();
        for index in 0..10 {
            admitted(
                &mut store,
                splint_id,
                start + Duration::from_millis(index * 10),
            );
        }
        let limited = start + Duration::from_millis(500);
        assert_eq!(
            store.admit(splint_id, 1, limited).unwrap(),
            Admission::RateLimited
        );
        assert!(store.claim_rate_refusal_audit(splint_id, 1));
        assert!(!store.claim_rate_refusal_audit(splint_id, 1));
        admitted(&mut store, other, limited);
        assert_eq!(
            store.admit(splint_id, 1, start + ADMISSION_WINDOW).unwrap(),
            Admission::Admitted,
            "the window slides past the first admission"
        );
        // A new incarnation starts with a fresh window and audit claim.
        assert_eq!(
            store.admit(splint_id, 2, limited).unwrap(),
            Admission::Admitted
        );
        assert!(store.claim_rate_refusal_audit(splint_id, 2));
    }

    #[test]
    fn retired_incarnations_are_dropped_and_never_summarized() {
        let mut store = AgentStatusStore::default();
        let splint_id = SplintId::new();
        let closed = SplintId::new();
        let start = Instant::now();
        for target in [splint_id, closed] {
            admitted(&mut store, target, start);
            store.apply(
                target,
                1,
                Some(&report(AgentState::Working, None, None)),
                start,
            );
        }
        assert_eq!(store.summary(splint_id, 2, start), None);
        store.retain_live(&HashMap::from([(splint_id, 1)]));
        assert_eq!(store.len(), 1);
        assert!(store.summary(splint_id, 1, start).is_some());
        // Relaunch: the same Splint is now live at incarnation 2.
        store.retain_live(&HashMap::from([(splint_id, 2)]));
        assert_eq!(store.len(), 0);
        assert_eq!(store.next_deadline(), None);
        assert!(store.take_due(start + Duration::from_secs(600)).is_empty());
    }

    #[test]
    fn a_new_report_replaces_the_previous_expiry() {
        let mut store = AgentStatusStore::default();
        let splint_id = SplintId::new();
        let start = Instant::now();
        admitted(&mut store, splint_id, start);
        store.apply(
            splint_id,
            1,
            Some(&report(AgentState::Working, None, Some(10))),
            start,
        );
        store.take_due(start);
        // A change just before expiry publishes, then expiry is spaced.
        let before = start + Duration::from_millis(9_900);
        admitted(&mut store, splint_id, before);
        store.apply(
            splint_id,
            1,
            Some(&report(AgentState::NeedsInput, None, Some(10))),
            before,
        );
        assert_eq!(store.take_due(before).len(), 1);
        assert!(
            store.take_due(start + Duration::from_secs(10)).is_empty(),
            "the replaced report's expiry no longer fires"
        );
        let expiry = before + Duration::from_secs(10);
        let stale = store.take_due(expiry);
        assert_eq!(stale.len(), 1);
        assert_eq!(
            stale[0].status.as_ref().unwrap().state,
            AgentState::NeedsInput
        );
    }

    #[test]
    fn proc_stat_parsing_tolerates_hostile_command_names() {
        let stat = |name: &[u8], parent: u32, session: u32, start: u64| {
            let mut record = b"4242 (".to_vec();
            record.extend_from_slice(name);
            record.extend_from_slice(
                format!(
                    ") R {parent} 4242 {session} 34816 4242 0 0 0 0 0 1 2 0 0 20 0 1 0 {start} 0 0\n"
                )
                .as_bytes(),
            );
            record
        };
        let parsed = |parent, session, start_time| {
            Some(ProcStat {
                parent,
                process_group: 4242,
                session,
                terminal_process_group: 4242,
                start_time,
            })
        };
        assert_eq!(
            parse_proc_stat(&stat(b"a) b (c", 17, 99, 555)),
            parsed(17, 99, 555)
        );
        // A legitimate peer may have a command name that is not UTF-8.
        assert_eq!(
            parse_proc_stat(&stat(b"\xff\xfe)x", 3, 4, 6)),
            parsed(3, 4, 6)
        );
        assert_eq!(parse_proc_stat(b"garbage"), None);
        assert_eq!(parse_proc_stat(b"1 (x) S notanumber 1 1"), None);
        assert_eq!(parse_proc_stat(b"1 (x) S 0 1 1 0"), None, "no start time");
    }

    fn table(entries: &[(u32, u32, u32, u64)]) -> impl Fn(u32) -> Option<ProcStat> {
        let entries = entries.to_vec();
        move |pid| {
            entries
                .iter()
                .find(|entry| entry.0 == pid)
                .map(|&(_, parent, session, start_time)| ProcStat {
                    parent,
                    process_group: 0,
                    session,
                    terminal_process_group: -1,
                    start_time,
                })
        }
    }

    #[test]
    fn peer_location_accepts_only_the_session_or_ancestry() {
        // Splint leader 100 (session 100). 110 runs in its session; 120 left
        // the session but descends from 100 through 115.
        let processes = table(&[
            (100, 1, 100, 10),
            (110, 100, 100, 20),
            (115, 100, 115, 30),
            (120, 115, 115, 40),
            // Started inside, then setsid'd and reparented to init.
            (130, 1, 130, 50),
            // A same-user sibling that may share the Splint's cgroup.
            (140, 2000, 2000, 60),
            (2000, 1, 2000, 5),
        ]);
        let locate = |pid, start| locate_peer_with(pid, start, 100, &processes);
        assert_eq!(locate(110, 20), Some(SelfReportPath::Session));
        assert_eq!(locate(120, 40), Some(SelfReportPath::Ancestry));
        assert_eq!(locate(130, 50), None, "detached agents need a grant");
        assert_eq!(locate(140, 60), None, "a shared cgroup is not inside");
        assert_eq!(locate(110, 21), None, "a reused peer PID is refused");
        assert_eq!(locate_peer_with(110, 20, 1, &processes), None);
    }

    #[test]
    fn ancestry_refuses_an_ancestor_younger_than_its_child() {
        // 215 exited and its PID was reused by a process started after 220,
        // whose parent chain happens to reach the leader.
        let processes = table(&[(200, 1, 200, 10), (215, 200, 215, 90), (220, 215, 220, 40)]);
        assert_eq!(locate_peer_with(220, 40, 200, &processes), None);
        let processes = table(&[(200, 1, 200, 10), (215, 200, 215, 30), (220, 215, 220, 40)]);
        assert_eq!(
            locate_peer_with(220, 40, 200, &processes),
            Some(SelfReportPath::Ancestry)
        );
    }

    #[test]
    fn the_ancestry_walk_is_bounded_and_completes_only_at_init() {
        // A chain 1000 -> 999 -> ... -> 900 -> init: 100 parents, more than
        // the walk may take.
        let mut entries: Vec<(u32, u32, u32, u64)> = (900..=1000)
            .map(|pid| {
                (
                    pid,
                    if pid == 900 { 1 } else { pid - 1 },
                    900,
                    u64::from(pid),
                )
            })
            .collect();
        let processes = table(&entries);
        let deep = walk_ancestry_with(1000, 1000, &processes).unwrap();
        assert!(!deep.complete, "a walk cut off by the bound proves nothing");
        assert_eq!(deep.ancestors.len(), MAX_ANCESTRY_STEPS);
        assert_eq!(deep.ancestors[0], 999);
        // Near the top, the same chain reaches init.
        let shallow = walk_ancestry_with(910, 910, &processes).unwrap();
        assert!(shallow.complete);
        assert_eq!(shallow.ancestors, (900..910).rev().collect::<Vec<_>>());
        assert_eq!(shallow.session, 900);
        // The process itself must be the one that started then.
        assert_eq!(walk_ancestry_with(910, 911, &processes), None);
        assert_eq!(walk_ancestry_with(1, 1, &processes), None);
        // An unreadable parent ends the walk incomplete, naming it.
        let missing = table(&[(20, 10, 20, 5)]);
        let cut = walk_ancestry_with(20, 5, &missing).unwrap();
        assert!(!cut.complete);
        assert_eq!(cut.ancestors, vec![10]);
        drop(processes);
        // A parent younger than its child is a reused PID: incomplete.
        entries.push((2000, 905, 2000, 1));
        let reused = walk_ancestry_with(2000, 1, table(&entries)).unwrap();
        assert!(!reused.complete);
        assert_eq!(reused.ancestors, vec![905]);
    }

    #[test]
    fn peer_location_uses_the_live_process_table() {
        let own = std::process::id();
        let own_start = process_start_time(own).expect("own start time");
        let session = rustix::process::getsid(None)
            .unwrap()
            .as_raw_nonzero()
            .get();
        let session = u32::try_from(session).unwrap();
        if session > 1 {
            assert_eq!(
                locate_peer(own, own_start, session),
                Some(SelfReportPath::Session)
            );
        }
        let mut leader = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .expect("spawn sleep");
        let mut sibling = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .expect("spawn sleep");
        let (leader_pid, sibling_pid) = (leader.id(), sibling.id());
        let leader_start = process_start_time(leader_pid).unwrap();
        let sibling_start = process_start_time(sibling_pid).unwrap();
        // Treat the test process as a Splint leader: its child is inside
        // through the shared session when it leads one, else by ancestry.
        let expected = if session == own {
            SelfReportPath::Session
        } else {
            SelfReportPath::Ancestry
        };
        assert_eq!(locate_peer(leader_pid, leader_start, own), Some(expected));
        // A sibling shares the cgroup and session but not ancestry, and the
        // leader leads no session, so the sibling is outside.
        assert_eq!(locate_peer(sibling_pid, sibling_start, leader_pid), None);
        // A process is never inside a Splint led by its own descendant.
        assert_eq!(locate_peer(own, own_start, leader_pid), None);
        assert_eq!(locate_peer(leader_pid, leader_start + 1, own), None);
        for child in [&mut leader, &mut sibling] {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
