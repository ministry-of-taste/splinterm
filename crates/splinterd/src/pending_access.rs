//! Bounded, cancellable pending workspace access requests (ADR 0014,
//! Plan 0053 PR 3).
//!
//! A `RequestLairAccess` that no grant covers becomes a pending request. The
//! daemon opens at most one per requesting process and workspace and at most
//! [`MAX_PENDING_TOTAL`] in all, each for [`PENDING_LIFETIME`]. Only one
//! prompt is visible at a time; the others wait in arrival order. A call
//! waits for the answer at most its wait budget and then replies `pending`;
//! repeating the same request from any connection of the same process joins
//! it.
//!
//! A pending request ends without a grant when it expires, is cancelled, its
//! process exits, its workspace ends, **Revoke all access** runs, or the
//! handoff fence goes up. Its prompt then closes or leaves the queue, and a
//! later answer grants nothing: an answer commits only while its request is
//! still here, and a request ended while its grant is being committed has
//! that grant revoked again by the committer.
//!
//! After a deny, or a prompt that expired unanswered, the same executable
//! identity is refused with `rate_limited` for that workspace for
//! [`DENY_COOLDOWN`], so a retrying agent cannot reopen the prompt. The
//! requesting process collects the deny or the expiry itself, once, by
//! repeating the request.
//!
//! This registry is synchronous and holds no async state; the daemon drives
//! the prompter, the expiry and exit watchers, and the commit. Nothing here
//! survives the process: pending requests never cross a handoff or restart.

use std::{
    collections::VecDeque,
    os::fd::OwnedFd,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use splinterm_core::LairId;
use splinterm_protocol::{
    ACCESS_REQUEST_EXPIRED, AccessScope, ConsentOfferEnd, ErrorCode,
    PENDING_ACCESS_LIFETIME_SECONDS, ProtocolError, Response,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::{
    agent_access::GrantIds,
    consent::{PeerIdentity, PromptCancel},
};

/// At most this many pending workspace requests at once, daemon-wide.
pub const MAX_PENDING_TOTAL: usize = 8;
/// A pending request expires this long after it opens.
pub const PENDING_LIFETIME: Duration = Duration::from_secs(PENDING_ACCESS_LIFETIME_SECONDS);
/// After a deny or an unanswered expiry, new requests from the same
/// executable identity for the same workspace are refused for this long.
pub const DENY_COOLDOWN: Duration = Duration::from_secs(10);
/// Uncollected denials and expiries kept for their requesters, oldest
/// dropped first.
const MAX_UNCOLLECTED_DENIALS: usize = 2 * MAX_PENDING_TOTAL;

/// One requesting process. A later process reusing the PID has another
/// start time, so it is never this one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessKey {
    uid: u32,
    pid: u32,
    start_time: u64,
}

impl ProcessKey {
    /// The peer's process, or `None` when its start time was not recorded,
    /// so a reused PID could not be told apart.
    pub fn of(peer: &PeerIdentity) -> Option<Self> {
        Some(Self {
            uid: peer.uid,
            pid: peer.pid,
            start_time: peer.start_time()?,
        })
    }

    pub const fn pid(&self) -> u32 {
        self.pid
    }

    pub const fn start_time(&self) -> u64 {
        self.start_time
    }
}

/// The executable identity a deny cooldown binds: the verified identity of
/// ADR 0007 when the connection has one, otherwise the executing file.
#[derive(Clone, Debug, Eq, PartialEq)]
enum IdentityKey {
    Verified {
        uid: u32,
        path: PathBuf,
        sha256: String,
    },
    Executing {
        uid: u32,
        device: u64,
        inode: u64,
    },
}

impl IdentityKey {
    fn of(peer: &PeerIdentity) -> Self {
        peer.persistent_executable().map_or_else(
            || {
                let (device, inode) = peer.executable_file();
                Self::Executing {
                    uid: peer.uid,
                    device,
                    inode,
                }
            },
            |executable| Self::Verified {
                uid: peer.uid,
                path: executable.path.clone(),
                sha256: executable.sha256.clone(),
            },
        )
    }
}

/// How a pending request ended, for every call waiting on it.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Granted(Box<Response>),
    Failed(ProtocolError),
}

/// A call admitted to a pending request, new or joined.
#[derive(Debug)]
pub struct Admitted {
    pub request_id: u64,
    pub expires: Instant,
    pub expires_at_unix_seconds: u64,
    pub outcome: watch::Receiver<Option<Outcome>>,
    /// Set for a newly opened request: the caller starts its watcher and
    /// makes sure the prompter runs.
    pub opened: Option<Opened>,
}

/// What a newly opened request's watcher needs.
#[derive(Debug)]
pub struct Opened {
    /// The requester's pidfd: readable once the process exits.
    pub pidfd: Arc<OwnedFd>,
    /// Cancelled when the request ends for any reason.
    pub done: CancellationToken,
}

/// The oldest queued request, now shown to the person.
#[derive(Debug)]
pub struct PromptJob {
    pub request_id: u64,
    pub peer: PeerIdentity,
    pub lair_id: LairId,
    pub scopes: Vec<AccessScope>,
    pub agent_name: Option<String>,
    pub expires: Instant,
    pub cancel: Arc<PromptCancel>,
    /// Why the request ended without an answer, set before [`Self::cancel`]
    /// fires, so the prompter withdraws its consent offers with that reason.
    pub ended: Arc<OnceLock<ConsentOfferEnd>>,
}

/// A pinned requester: its pidfd, and the file it executed when pinned
/// (device and inode).
#[derive(Debug)]
pub struct PinnedRequester {
    pub pidfd: OwnedFd,
    pub executing: (u64, u64),
}

/// A request whose grant is being committed.
#[derive(Debug)]
pub struct CommitTicket {
    pub peer: PeerIdentity,
    pub process: ProcessKey,
    pub pidfd: Arc<OwnedFd>,
    /// The file the requester executed when its request opened.
    pub executing: (u64, u64),
}

/// A denied request, for its audit record.
#[derive(Debug)]
pub struct Denied {
    pub peer: PeerIdentity,
    pub lair_id: LairId,
    pub scopes: Vec<AccessScope>,
}

#[derive(Debug)]
enum Phase {
    Queued,
    Prompting,
    /// The person granted it and the daemon is committing; an end that
    /// arrives now is recorded and the committer revokes what it commits.
    Committing {
        ended: Option<ProtocolError>,
    },
    /// The grant is committed and visible: the request can no longer be
    /// ended, and its callers receive the grant when the commit finishes.
    Published,
}

#[derive(Debug)]
struct Entry {
    id: u64,
    process: ProcessKey,
    identity: IdentityKey,
    peer: PeerIdentity,
    lair_id: LairId,
    scopes: Vec<AccessScope>,
    agent_name: Option<String>,
    opened_unix_seconds: u64,
    expires: Instant,
    expires_at_unix_seconds: u64,
    outcome: watch::Sender<Option<Outcome>>,
    cancel: Arc<PromptCancel>,
    ended: Arc<OnceLock<ConsentOfferEnd>>,
    done: CancellationToken,
    pidfd: Arc<OwnedFd>,
    executing: (u64, u64),
    phase: Phase,
}

impl Entry {
    fn deliver(&self, outcome: Outcome) {
        self.outcome.send_replace(Some(outcome));
        self.cancel.cancel();
        self.done.cancel();
    }
}

/// An uncollected deny or expiry: what the requester's next call returns.
#[derive(Debug)]
struct Denial {
    process: ProcessKey,
    lair_id: LairId,
    scopes: Vec<AccessScope>,
    error: ProtocolError,
    until: Instant,
}

/// What a deny or an unanswered expiry remembers about its request.
struct Unanswered {
    identity: IdentityKey,
    process: ProcessKey,
    lair_id: LairId,
    scopes: Vec<AccessScope>,
    /// A call waits for the outcome now and learns it directly.
    waiting: bool,
}

impl Unanswered {
    fn of(entry: &Entry) -> Self {
        Self {
            identity: entry.identity.clone(),
            process: entry.process.clone(),
            lair_id: entry.lair_id,
            scopes: entry.scopes.clone(),
            waiting: entry.outcome.receiver_count() > 0,
        }
    }
}

#[derive(Debug)]
struct Cooldown {
    identity: IdentityKey,
    lair_id: LairId,
    until: Instant,
}

#[derive(Debug, Default)]
struct Inner {
    /// In arrival order: the prompt queue.
    entries: Vec<Entry>,
    denials: VecDeque<Denial>,
    cooldowns: Vec<Cooldown>,
    prompter_running: bool,
    next_id: u64,
    /// Set at shutdown: no request is admitted any more.
    closed: bool,
}

impl Inner {
    fn prune(&mut self, now: Instant) {
        self.denials.retain(|denial| denial.until > now);
        self.cooldowns.retain(|cooldown| cooldown.until > now);
    }

    fn position(&self, request_id: u64) -> Option<usize> {
        self.entries.iter().position(|entry| entry.id == request_id)
    }

    /// Cools the identity down for this workspace and, unless a call learns
    /// `error` directly, keeps it until `collect_until` for the requester to
    /// collect once.
    fn remember(
        &mut self,
        unanswered: Unanswered,
        error: ProtocolError,
        collect_until: Instant,
        now: Instant,
    ) {
        self.cooldowns.push(Cooldown {
            identity: unanswered.identity,
            lair_id: unanswered.lair_id,
            until: now + DENY_COOLDOWN,
        });
        if unanswered.waiting {
            return;
        }
        if self.denials.len() == MAX_UNCOLLECTED_DENIALS {
            self.denials.pop_front();
        }
        self.denials.push_back(Denial {
            process: unanswered.process,
            lair_id: unanswered.lair_id,
            scopes: unanswered.scopes,
            error,
            until: collect_until,
        });
    }

    /// Ends every matching request without a grant: queued and prompting
    /// ones leave at once, committing ones are marked for the committer.
    /// A published grant is already out, so its request is not ended.
    /// `reason` is what a consent offer for a shown request is withdrawn
    /// with (ADR 0015).
    fn end_where(
        &mut self,
        error: &ProtocolError,
        reason: ConsentOfferEnd,
        selected: impl Fn(&Entry) -> bool,
    ) -> usize {
        let mut count = 0;
        let mut index = 0;
        while index < self.entries.len() {
            if matches!(self.entries[index].phase, Phase::Published)
                || !selected(&self.entries[index])
            {
                index += 1;
                continue;
            }
            count += 1;
            if let Phase::Committing { ended } = &mut self.entries[index].phase {
                ended.get_or_insert_with(|| error.clone());
                index += 1;
                continue;
            }
            let entry = self.entries.remove(index);
            let _ = entry.ended.set(reason);
            entry.deliver(Outcome::Failed(error.clone()));
        }
        count
    }
}

/// Every pending workspace request in this daemon process.
#[derive(Debug, Default)]
pub struct PendingAccess {
    inner: Mutex<Inner>,
    /// The daemon-wide authorization revision, advanced whenever a request
    /// opens, is shown, or ends, so the access inventory sees the change.
    ids: Arc<GrantIds>,
}

/// One pending request, for the access inventory.
#[derive(Clone, Debug)]
pub struct PendingView {
    pub request_id: u64,
    pub peer: PeerIdentity,
    pub lair_id: LairId,
    pub scopes: Vec<AccessScope>,
    pub agent_name: Option<String>,
    pub opened_unix_seconds: u64,
    pub expires_at_unix_seconds: u64,
    /// Its prompt is showing (or its answer is being committed).
    pub prompting: bool,
}

pub fn cancelled(message: &'static str) -> ProtocolError {
    ProtocolError::new(ErrorCode::Cancelled, message)
}

fn denied() -> ProtocolError {
    ProtocolError::new(ErrorCode::ConsentDenied, "Lair access was denied")
}

fn unix_seconds_at(expires: Instant, now: Instant) -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_add(expires.saturating_duration_since(now).as_secs())
}

impl PendingAccess {
    /// A registry advancing the authorization revision every grant source
    /// shares.
    pub fn with_ids(ids: Arc<GrantIds>) -> Self {
        Self {
            inner: Mutex::default(),
            ids,
        }
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Advances the revision when `count` requests ended.
    fn ended(&self, count: usize) -> usize {
        if count > 0 {
            self.ids.advance_revision();
        }
        count
    }

    /// Every pending request, in arrival order, for the access inventory. A
    /// request whose grant is already published is no longer pending.
    pub fn inventory(&self) -> Vec<PendingView> {
        self.inner()
            .entries
            .iter()
            .filter(|entry| !matches!(entry.phase, Phase::Published))
            .map(|entry| PendingView {
                request_id: entry.id,
                peer: entry.peer.clone(),
                lair_id: entry.lair_id,
                scopes: entry.scopes.clone(),
                agent_name: entry.agent_name.clone(),
                opened_unix_seconds: entry.opened_unix_seconds,
                expires_at_unix_seconds: entry.expires_at_unix_seconds,
                prompting: !matches!(entry.phase, Phase::Queued),
            })
            .collect()
    }

    /// How many requests **Revoke all access** would end now.
    pub fn live_count(&self) -> usize {
        self.inner()
            .entries
            .iter()
            .filter(|entry| !matches!(entry.phase, Phase::Published))
            .count()
    }

    /// Admits one call: it collects its own uncollected deny or expiry, joins
    /// its process's pending request for the same workspace and scopes, or
    /// opens a new one. `open_pidfd` pins the requester and runs only for a
    /// new request.
    ///
    /// # Errors
    /// The requester's earlier deny (`consent_denied`, once) or unanswered
    /// expiry (`cancelled`, once); a different
    /// request already pending for this process and workspace
    /// (`resource_limit`, naming it); the deny cooldown (`rate_limited`); the
    /// daemon-wide bound (`resource_limit`); a requester that cannot be
    /// pinned (`consent_unavailable`).
    pub fn admit(
        &self,
        peer: &PeerIdentity,
        lair_id: LairId,
        scopes: Vec<AccessScope>,
        agent_name: Option<String>,
        now: Instant,
        open_pidfd: impl FnOnce(&ProcessKey) -> Result<PinnedRequester, ProtocolError>,
    ) -> Result<Admitted, ProtocolError> {
        let process = ProcessKey::of(peer).ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::ConsentUnavailable,
                "the requesting process cannot be pinned for a pending request",
            )
        })?;
        let identity = IdentityKey::of(peer);
        let mut inner = self.inner();
        if inner.closed {
            return Err(ProtocolError::new(
                ErrorCode::ConsentUnavailable,
                "the daemon is shutting down",
            ));
        }
        inner.prune(now);
        let uncollected = inner.denials.iter().position(|denial| {
            denial.process == process && denial.lair_id == lair_id && denial.scopes == scopes
        });
        if let Some(denial) = uncollected.and_then(|index| inner.denials.remove(index)) {
            return Err(denial.error);
        }
        if let Some(entry) = inner
            .entries
            .iter()
            .find(|entry| entry.process == process && entry.lair_id == lair_id)
        {
            if entry.scopes != scopes {
                let mut error = ProtocolError::new(
                    ErrorCode::ResourceLimit,
                    "a different access request for this workspace is already pending",
                );
                error.pending_request_id = Some(entry.id);
                return Err(error);
            }
            return Ok(Admitted {
                request_id: entry.id,
                expires: entry.expires,
                expires_at_unix_seconds: entry.expires_at_unix_seconds,
                outcome: entry.outcome.subscribe(),
                opened: None,
            });
        }
        if inner
            .cooldowns
            .iter()
            .any(|cooldown| cooldown.identity == identity && cooldown.lair_id == lair_id)
        {
            return Err(ProtocolError::new(
                ErrorCode::RateLimited,
                "access to this workspace was just denied or expired unanswered; retry later",
            ));
        }
        if inner.entries.len() >= MAX_PENDING_TOTAL {
            return Err(ProtocolError::new(
                ErrorCode::ResourceLimit,
                "too many access requests are pending",
            ));
        }
        let pinned = open_pidfd(&process)?;
        let pidfd = Arc::new(pinned.pidfd);
        inner.next_id = inner.next_id.saturating_add(1);
        let expires = now + PENDING_LIFETIME;
        let (outcome, receiver) = watch::channel(None);
        let done = CancellationToken::new();
        let entry = Entry {
            id: inner.next_id,
            process,
            identity,
            peer: peer.clone(),
            lair_id,
            scopes,
            agent_name,
            opened_unix_seconds: unix_seconds_at(now, now),
            expires,
            expires_at_unix_seconds: unix_seconds_at(expires, now),
            outcome,
            cancel: Arc::new(PromptCancel::default()),
            ended: Arc::default(),
            done: done.clone(),
            pidfd: Arc::clone(&pidfd),
            executing: pinned.executing,
            phase: Phase::Queued,
        };
        let admitted = Admitted {
            request_id: entry.id,
            expires,
            expires_at_unix_seconds: entry.expires_at_unix_seconds,
            outcome: receiver,
            opened: Some(Opened { pidfd, done }),
        };
        inner.entries.push(entry);
        self.ids.advance_revision();
        Ok(admitted)
    }

    /// Claims the single prompter role; `false` while one runs.
    pub fn claim_prompter(&self) -> bool {
        let mut inner = self.inner();
        !std::mem::replace(&mut inner.prompter_running, true)
    }

    /// Gives the prompter role back after the prompter failed abnormally.
    pub fn release_prompter(&self) {
        self.inner().prompter_running = false;
    }

    /// The oldest queued request, marked as shown. `None` releases the
    /// prompter role: nothing waits.
    pub fn next_prompt(&self) -> Option<PromptJob> {
        let mut inner = self.inner();
        let Some(entry) = inner
            .entries
            .iter_mut()
            .find(|entry| matches!(entry.phase, Phase::Queued))
        else {
            inner.prompter_running = false;
            return None;
        };
        entry.phase = Phase::Prompting;
        self.ids.advance_revision();
        Some(PromptJob {
            request_id: entry.id,
            peer: entry.peer.clone(),
            lair_id: entry.lair_id,
            scopes: entry.scopes.clone(),
            agent_name: entry.agent_name.clone(),
            expires: entry.expires,
            cancel: Arc::clone(&entry.cancel),
            ended: Arc::clone(&entry.ended),
        })
    }

    /// Records a deny: the request ends, its requester can collect the deny
    /// once, and its executable identity cools down for this workspace.
    pub fn deny(&self, request_id: u64, now: Instant) -> Option<Denied> {
        let mut inner = self.inner();
        let index = inner.position(request_id)?;
        if !matches!(inner.entries[index].phase, Phase::Prompting) {
            return None;
        }
        let entry = inner.entries.remove(index);
        self.ids.advance_revision();
        // A call waiting now learns it directly; otherwise the requester
        // collects it by repeating the request.
        inner.remember(Unanswered::of(&entry), denied(), entry.expires, now);
        entry.deliver(Outcome::Failed(denied()));
        Some(Denied {
            peer: entry.peer,
            lair_id: entry.lair_id,
            scopes: entry.scopes,
        })
    }

    /// Ends one request with `error`, whatever its phase: its process exited,
    /// or its prompt failed. A consent offer still open for it is withdrawn as
    /// cancelled.
    pub fn end_request(&self, request_id: u64, error: &ProtocolError) -> bool {
        let count = self
            .inner()
            .end_where(error, ConsentOfferEnd::Cancelled, |entry| {
                entry.id == request_id
            });
        self.ended(count) == 1
    }

    /// Ends one request when it has expired by `now`. An unanswered one counts
    /// like a deny: its identity cools down for the workspace, and a requester
    /// not waiting now collects the expiry once, within [`PENDING_LIFETIME`].
    pub fn expire(&self, request_id: u64, now: Instant) -> bool {
        let error = cancelled(ACCESS_REQUEST_EXPIRED);
        let mut inner = self.inner();
        let unanswered = inner
            .entries
            .iter()
            .find(|entry| {
                entry.id == request_id
                    && entry.expires <= now
                    && matches!(entry.phase, Phase::Queued | Phase::Prompting)
            })
            .map(Unanswered::of);
        let count = inner.end_where(&error, ConsentOfferEnd::Expired, |entry| {
            entry.id == request_id && entry.expires <= now
        });
        if count == 1
            && let Some(unanswered) = unanswered
        {
            inner.remember(unanswered, error, now + PENDING_LIFETIME, now);
        }
        drop(inner);
        self.ended(count) == 1
    }

    /// The requester cancels its own request for this workspace; another
    /// process's request, or one for another workspace, is never found.
    pub fn cancel(&self, process: &ProcessKey, lair_id: LairId, request_id: u64) -> bool {
        let count = self.inner().end_where(
            &cancelled("the access request was cancelled"),
            ConsentOfferEnd::Cancelled,
            |entry| entry.id == request_id && entry.process == *process && entry.lair_id == lair_id,
        );
        self.ended(count) == 1
    }

    /// **Revoke all access**: every pending request ends.
    pub fn cancel_all(&self) -> usize {
        let count = self.inner().end_where(
            &cancelled("access was revoked"),
            ConsentOfferEnd::Cancelled,
            |_| true,
        );
        self.ended(count)
    }

    /// Ordinary shutdown: every pending request ends and none is admitted
    /// again, so no prompt opens while the daemon stops.
    pub fn close(&self) -> usize {
        let mut inner = self.inner();
        inner.closed = true;
        let count = inner.end_where(
            &cancelled("the daemon is shutting down"),
            ConsentOfferEnd::RequestEnded,
            |_| true,
        );
        drop(inner);
        self.ended(count)
    }

    /// Runs `publish` (the step that makes a committed grant visible) only
    /// while the request is still committing and nothing has ended it, and
    /// then marks it published, all under this registry's lock. A request
    /// ended first returns `None` and nothing is published, so an end can
    /// never race a grant into existence. Take any lock `publish` needs
    /// before calling this.
    pub fn publish_if_live<T>(&self, request_id: u64, publish: impl FnOnce() -> T) -> Option<T> {
        let mut inner = self.inner();
        let index = inner.position(request_id)?;
        let entry = &mut inner.entries[index];
        if !matches!(entry.phase, Phase::Committing { ended: None }) {
            return None;
        }
        let published = publish();
        entry.phase = Phase::Published;
        Some(published)
    }

    /// The workspace ended: its pending requests end.
    pub fn end_lair(&self, lair_id: LairId) -> usize {
        let count = self.inner().end_where(
            &ProtocolError::new(ErrorCode::NotFound, "the workspace no longer exists"),
            ConsentOfferEnd::RequestEnded,
            |entry| entry.lair_id == lair_id,
        );
        self.ended(count)
    }

    /// The handoff fence went up: nothing pending survives it.
    pub fn end_all_at_fence(&self) -> usize {
        let count = self.inner().end_where(
            &ProtocolError::new(
                ErrorCode::HandoffInProgress,
                "the daemon is upgrading; ask again after it restarts",
            ),
            ConsentOfferEnd::Fenced,
            |_| true,
        );
        self.ended(count)
    }

    /// Moves a shown request to committing; `None` when it already ended,
    /// so a late answer grants nothing.
    pub fn begin_commit(&self, request_id: u64) -> Option<CommitTicket> {
        let mut inner = self.inner();
        let index = inner.position(request_id)?;
        let entry = &mut inner.entries[index];
        if !matches!(entry.phase, Phase::Prompting) {
            return None;
        }
        entry.phase = Phase::Committing { ended: None };
        Some(CommitTicket {
            peer: entry.peer.clone(),
            process: entry.process.clone(),
            pidfd: Arc::clone(&entry.pidfd),
            executing: entry.executing,
        })
    }

    /// Finishes a commit. `false` when the request was ended while it
    /// committed: its callers receive that end instead, and the committer
    /// revokes the grant it committed.
    pub fn finish_commit(&self, request_id: u64, outcome: Outcome) -> bool {
        let mut inner = self.inner();
        let Some(index) = inner.position(request_id) else {
            return false;
        };
        let entry = inner.entries.remove(index);
        self.ids.advance_revision();
        if let Phase::Committing {
            ended: Some(error), ..
        } = &entry.phase
        {
            entry.deliver(Outcome::Failed(error.clone()));
            return false;
        }
        entry.deliver(outcome);
        true
    }

    /// How many requests are pending.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner().entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer_with_start(start_time: u64) -> PeerIdentity {
        let mut peer = PeerIdentity::for_test();
        peer.install_start_time(start_time);
        peer
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "it stands in for the fallible pin `admit` takes"
    )]
    fn pidfd(_: &ProcessKey) -> Result<PinnedRequester, ProtocolError> {
        Ok(PinnedRequester {
            pidfd: OwnedFd::from(std::fs::File::open("/dev/null").unwrap()),
            executing: (1, 1),
        })
    }

    fn admit(
        pending: &PendingAccess,
        peer: &PeerIdentity,
        lair_id: LairId,
        scopes: &[AccessScope],
        now: Instant,
    ) -> Result<Admitted, ProtocolError> {
        pending.admit(peer, lair_id, scopes.to_vec(), None, now, pidfd)
    }

    const LOOK: &[AccessScope] = &[AccessScope::Observe];
    const TYPE: &[AccessScope] = &[AccessScope::Observe, AccessScope::Input];

    #[test]
    fn one_request_per_process_and_workspace_and_eight_in_all() {
        let pending = PendingAccess::default();
        let now = Instant::now();
        let lair = LairId::new();
        let peer = peer_with_start(1);
        let first = admit(&pending, &peer, lair, LOOK, now).unwrap();
        assert!(first.opened.is_some());
        // The same request joins; it opens nothing.
        let joined = admit(&pending, &peer, lair, LOOK, now).unwrap();
        assert_eq!(joined.request_id, first.request_id);
        assert!(joined.opened.is_none());
        // Different access fails with the pending request's identifier.
        let refused = admit(&pending, &peer, lair, TYPE, now).unwrap_err();
        assert_eq!(refused.code, ErrorCode::ResourceLimit);
        assert_eq!(refused.pending_request_id, Some(first.request_id));
        // Another process gets its own; the ninth request fails.
        for start in 2..=8 {
            admit(&pending, &peer_with_start(start), lair, LOOK, now).unwrap();
        }
        assert_eq!(pending.len(), MAX_PENDING_TOTAL);
        let full = admit(&pending, &peer_with_start(9), lair, LOOK, now).unwrap_err();
        assert_eq!(full.code, ErrorCode::ResourceLimit);
        assert_eq!(full.pending_request_id, None);
        // A requester without a recorded start time cannot be pinned.
        let unpinned = admit(&pending, &PeerIdentity::for_test(), lair, LOOK, now).unwrap_err();
        assert_eq!(unpinned.code, ErrorCode::ConsentUnavailable);
    }

    #[test]
    fn prompts_are_shown_one_at_a_time_in_arrival_order() {
        let pending = PendingAccess::default();
        let now = Instant::now();
        let lair = LairId::new();
        let first = admit(&pending, &peer_with_start(1), lair, LOOK, now).unwrap();
        let second = admit(&pending, &peer_with_start(2), lair, LOOK, now).unwrap();
        assert!(pending.claim_prompter());
        assert!(!pending.claim_prompter(), "only one prompter runs");
        let job = pending.next_prompt().unwrap();
        assert_eq!(job.request_id, first.request_id);
        assert!(pending.begin_commit(job.request_id).is_some());
        assert!(pending.finish_commit(job.request_id, Outcome::Failed(denied())));
        let job = pending.next_prompt().unwrap();
        assert_eq!(job.request_id, second.request_id);
        pending.end_request(job.request_id, &cancelled("test"));
        assert!(pending.next_prompt().is_none());
        assert!(pending.claim_prompter(), "the role was released");
    }

    #[test]
    fn a_deny_cools_the_identity_down_and_is_collected_once() {
        let pending = PendingAccess::default();
        let now = Instant::now();
        let lair = LairId::new();
        let other_lair = LairId::new();
        let peer = peer_with_start(1);
        let admitted = admit(&pending, &peer, lair, LOOK, now).unwrap();
        // Nobody waits on it now.
        drop(admitted.outcome);
        let job = pending.next_prompt().unwrap();
        let denied = pending.deny(job.request_id, now).unwrap();
        assert_eq!(denied.lair_id, lair);
        // The requester collects its deny once, then the cooldown applies to
        // every process of the same executable identity.
        assert_eq!(
            admit(&pending, &peer, lair, LOOK, now).unwrap_err().code,
            ErrorCode::ConsentDenied
        );
        assert_eq!(
            admit(&pending, &peer, lair, LOOK, now).unwrap_err().code,
            ErrorCode::RateLimited
        );
        assert_eq!(
            admit(&pending, &peer_with_start(2), lair, TYPE, now)
                .unwrap_err()
                .code,
            ErrorCode::RateLimited
        );
        // Another workspace is unaffected, and the cooldown ends.
        admit(&pending, &peer, other_lair, LOOK, now).unwrap();
        admit(&pending, &peer, lair, LOOK, now + DENY_COOLDOWN).unwrap();
    }

    #[test]
    fn a_waiting_call_learns_the_deny_directly() {
        let pending = PendingAccess::default();
        let now = Instant::now();
        let lair = LairId::new();
        let peer = peer_with_start(1);
        let admitted = admit(&pending, &peer, lair, LOOK, now).unwrap();
        let job = pending.next_prompt().unwrap();
        pending.deny(job.request_id, now).unwrap();
        assert_eq!(*admitted.outcome.borrow(), Some(Outcome::Failed(denied())));
        assert_eq!(
            admit(&pending, &peer, lair, LOOK, now).unwrap_err().code,
            ErrorCode::RateLimited,
            "nothing is left to collect"
        );
    }

    #[test]
    fn ending_closes_the_prompt_and_a_late_answer_commits_nothing() {
        let pending = PendingAccess::default();
        let now = Instant::now();
        let lair = LairId::new();
        let peer = peer_with_start(1);
        let process = ProcessKey::of(&peer).unwrap();
        let admitted = admit(&pending, &peer, lair, LOOK, now).unwrap();
        let opened = admitted.opened.as_ref().unwrap();
        let job = pending.next_prompt().unwrap();
        // Another process cannot cancel it.
        let other = ProcessKey::of(&peer_with_start(2)).unwrap();
        assert!(!pending.cancel(&other, lair, job.request_id));
        assert!(!pending.cancel(&process, LairId::new(), job.request_id));
        assert!(pending.cancel(&process, lair, job.request_id));
        assert!(job.cancel.is_cancelled(), "the prompt closes");
        assert!(opened.done.is_cancelled(), "the watcher stops");
        assert_eq!(
            admitted
                .outcome
                .borrow()
                .as_ref()
                .map(|outcome| match outcome {
                    Outcome::Failed(error) => error.code,
                    Outcome::Granted(_) => ErrorCode::Internal,
                }),
            Some(ErrorCode::Cancelled)
        );
        assert!(pending.begin_commit(job.request_id).is_none());
        assert!(pending.deny(job.request_id, now).is_none());
    }

    #[test]
    fn an_end_during_commit_wins_and_the_grant_is_revoked() {
        let pending = PendingAccess::default();
        let now = Instant::now();
        let lair = LairId::new();
        let admitted = admit(&pending, &peer_with_start(1), lair, LOOK, now).unwrap();
        let job = pending.next_prompt().unwrap();
        assert!(pending.begin_commit(job.request_id).is_some());
        assert_eq!(pending.cancel_all(), 1);
        let granted = Response::LairAccessRequestsCancelled { cancelled: 0 };
        assert!(!pending.finish_commit(job.request_id, Outcome::Granted(Box::new(granted))));
        assert!(matches!(
            &*admitted.outcome.borrow(),
            Some(Outcome::Failed(error)) if error.code == ErrorCode::Cancelled
        ));
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn an_unanswered_expiry_cools_the_identity_down_and_is_collected_once() {
        let pending = PendingAccess::default();
        let now = Instant::now();
        let lair = LairId::new();
        let other_lair = LairId::new();
        let peer = peer_with_start(1);
        let admitted = admit(&pending, &peer, lair, LOOK, now).unwrap();
        // Nobody waits on it when it expires.
        drop(admitted.outcome);
        let job = pending.next_prompt().unwrap();
        let expired = now + PENDING_LIFETIME;
        assert!(pending.expire(job.request_id, expired));
        // The requester's next call learns that it expired, once; it does
        // not open a new prompt. Then the cooldown applies to the identity.
        let error = admit(&pending, &peer, lair, LOOK, expired).unwrap_err();
        assert_eq!(error.code, ErrorCode::Cancelled);
        assert_eq!(error.message, ACCESS_REQUEST_EXPIRED);
        assert_eq!(
            admit(&pending, &peer, lair, LOOK, expired)
                .unwrap_err()
                .code,
            ErrorCode::RateLimited
        );
        assert_eq!(
            admit(&pending, &peer_with_start(2), lair, TYPE, expired)
                .unwrap_err()
                .code,
            ErrorCode::RateLimited
        );
        // Another workspace is unaffected, and the cooldown ends.
        admit(&pending, &peer, other_lair, LOOK, expired).unwrap();
        admit(&pending, &peer, lair, LOOK, expired + DENY_COOLDOWN).unwrap();
    }

    #[test]
    fn a_waiting_call_learns_the_expiry_directly_and_a_queued_request_cools_down_too() {
        let pending = PendingAccess::default();
        let now = Instant::now();
        let lair = LairId::new();
        let peer = peer_with_start(1);
        // Still queued, never shown, and a call waits on it.
        let admitted = admit(&pending, &peer, lair, LOOK, now).unwrap();
        let expired = now + PENDING_LIFETIME;
        assert!(pending.expire(admitted.request_id, expired));
        assert!(matches!(
            &*admitted.outcome.borrow(),
            Some(Outcome::Failed(error)) if error.code == ErrorCode::Cancelled
        ));
        assert_eq!(
            admit(&pending, &peer, lair, LOOK, expired)
                .unwrap_err()
                .code,
            ErrorCode::RateLimited
        );
        // Nothing is left to collect after the cooldown.
        admit(&pending, &peer, lair, LOOK, expired + DENY_COOLDOWN).unwrap();
    }

    #[test]
    fn an_expiry_during_commit_leaves_no_cooldown() {
        let pending = PendingAccess::default();
        let now = Instant::now();
        let lair = LairId::new();
        let peer = peer_with_start(1);
        let admitted = admit(&pending, &peer, lair, LOOK, now).unwrap();
        drop(admitted.outcome);
        let job = pending.next_prompt().unwrap();
        assert!(pending.begin_commit(job.request_id).is_some());
        // The person answered: the committer decides, and the expiry is not
        // a deny.
        assert!(pending.expire(job.request_id, now + PENDING_LIFETIME));
        let granted = Response::LairAccessRequestsCancelled { cancelled: 0 };
        assert!(!pending.finish_commit(job.request_id, Outcome::Granted(Box::new(granted))));
        admit(&pending, &peer, lair, LOOK, now + PENDING_LIFETIME).unwrap();
    }

    #[test]
    fn expiry_workspace_end_and_the_fence_end_requests() {
        let pending = PendingAccess::default();
        let now = Instant::now();
        let lair = LairId::new();
        let other_lair = LairId::new();
        let first = admit(&pending, &peer_with_start(1), lair, LOOK, now).unwrap();
        assert!(!pending.expire(first.request_id, now));
        assert!(pending.expire(first.request_id, now + PENDING_LIFETIME));
        // After the expiry's cooldown.
        let later = now + PENDING_LIFETIME + DENY_COOLDOWN;
        admit(&pending, &peer_with_start(2), lair, LOOK, later).unwrap();
        admit(&pending, &peer_with_start(3), other_lair, LOOK, later).unwrap();
        assert_eq!(pending.end_lair(lair), 1);
        let last = admit(&pending, &peer_with_start(4), lair, LOOK, later).unwrap();
        assert_eq!(pending.end_all_at_fence(), 2);
        assert!(matches!(
            &*last.outcome.borrow(),
            Some(Outcome::Failed(error)) if error.code == ErrorCode::HandoffInProgress
        ));
        assert_eq!(pending.len(), 0);
    }

    /// F3: a grant becomes visible only while its request is committing and
    /// unended; once visible, the request can no longer be ended.
    #[test]
    fn publishing_happens_only_for_a_live_committing_request() {
        let pending = PendingAccess::default();
        let now = Instant::now();
        let lair = LairId::new();
        let peer = peer_with_start(1);
        let process = ProcessKey::of(&peer).unwrap();
        // Not committing yet: nothing publishes.
        let first = admit(&pending, &peer, lair, LOOK, now).unwrap();
        assert_eq!(pending.publish_if_live(first.request_id, || ()), None);
        let job = pending.next_prompt().unwrap();
        pending.begin_commit(job.request_id).unwrap();
        // Ended during the commit: the publish closure never runs.
        assert!(pending.cancel(&process, lair, job.request_id));
        let mut ran = false;
        assert_eq!(pending.publish_if_live(job.request_id, || ran = true), None);
        assert!(!ran);
        assert!(!pending.finish_commit(job.request_id, Outcome::Failed(cancelled("test"))));
        // Live: it publishes, and then nothing can end it.
        let second = admit(&pending, &peer, lair, LOOK, now).unwrap();
        let job = pending.next_prompt().unwrap();
        pending.begin_commit(job.request_id).unwrap();
        assert_eq!(pending.publish_if_live(job.request_id, || 7), Some(7));
        assert!(!pending.cancel(&process, lair, job.request_id));
        assert_eq!(pending.cancel_all(), 0);
        assert_eq!(pending.end_all_at_fence(), 0);
        let granted = Response::LairAccessRequestsCancelled { cancelled: 0 };
        assert!(pending.finish_commit(job.request_id, Outcome::Granted(Box::new(granted.clone()))));
        assert_eq!(
            *second.outcome.borrow(),
            Some(Outcome::Granted(Box::new(granted)))
        );
    }

    #[test]
    fn a_closed_registry_ends_everything_and_admits_nothing() {
        let pending = PendingAccess::default();
        let now = Instant::now();
        let lair = LairId::new();
        let first = admit(&pending, &peer_with_start(1), lair, LOOK, now).unwrap();
        assert_eq!(pending.close(), 1);
        assert!(matches!(
            &*first.outcome.borrow(),
            Some(Outcome::Failed(error)) if error.code == ErrorCode::Cancelled
        ));
        assert_eq!(
            admit(&pending, &peer_with_start(2), lair, LOOK, now)
                .unwrap_err()
                .code,
            ErrorCode::ConsentUnavailable
        );
    }
}
