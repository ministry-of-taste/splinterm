//! Consent responders: remote Windows and the terminal approver (ADR 0015).
//!
//! A connection registers as a responder with `SubscribeConsentOffers`: a
//! remote Window's graphical relay connection as a Window responder, or the
//! installed client's approver command, in a person's own terminal on this
//! host, as a terminal responder. Responder status belongs to that one
//! connection and ends with it or with its `Detach`; no other channel of the
//! same relay or process inherits it.
//!
//! The single prompter shows one workspace (Lair) prompt at a time. Each one
//! is offered at once to every eligible responder, beside the daemon's own
//! dialog when it has a graphical session. Every responder connection gets
//! its own offer, with an offer ID never reused in this daemon and a fresh
//! random nonce. The first valid answer from any route wins: every other
//! offer is withdrawn (`answered_elsewhere`) and the prompter closes its own
//! dialog. An answer commits through the same re-checks as a local one. A
//! prompt that ends unanswered withdraws every offer with the reason it
//! ended; a responder that leaves takes only its own offer with it.
//!
//! Eligibility is checked when a responder subscribes and again before every
//! offer, from `/proc`, never from anything the client claims: the
//! connection is still the installed relay or client it was at `Hello`, a
//! terminal approver reads and writes its session's controlling terminal,
//! and one bounded ancestry walk proves the process is outside every
//! Splint's session and process tree and, for an offer, outside the
//! requester's. Nothing here survives the process, so responders and offers
//! never cross a handoff.

use std::{
    collections::HashMap,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};

use splinterm_protocol::{
    CONSENT_OFFER_NONCE_BYTES, ConsentDecision, ConsentOffer, ConsentOfferEnd, ConsentOfferNonce,
    ConsentResponderKind, ErrorCode, MAX_CONSENT_RESPOND_WITHIN_SECONDS, OfferedConsentPrompt,
    ProtocolError, SubscriptionEvent,
};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{debug, warn};

use crate::{
    agent_status::{self, ProcStat},
    consent::{self, PeerIdentity},
};

/// At most this many responders at once, daemon-wide.
pub const MAX_CONSENT_RESPONDERS: usize = 16;
/// Events queued for one responder. A prompt sends a responder at most an
/// offer and its withdrawal, so a responder this far behind has stopped
/// reading; it is dropped.
const RESPONDER_QUEUE: usize = 8;
/// Taken off the time an offer says is left, for the answer's way back.
const TRANSPORT_MARGIN: Duration = Duration::from_secs(2);

/// Why a workspace request ends `consent_unavailable` when no route could
/// show its prompt.
pub const NO_ROUTE: &str =
    "no Splinterm Window or approver connected to this computer can show the prompt";
/// Why it ends so when every route showing its prompt went away.
pub const ROUTES_CLOSED: &str = "the Window or approver showing the prompt closed";

/// A prompt no route could show, or whose every route closed: its request
/// ends `consent_unavailable` with this message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Unavailable(pub &'static str);

/// Which route answered a workspace prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerRoute {
    /// The daemon's own dialog.
    Local,
    /// A remote Window's dialog.
    Window,
    /// The terminal approver.
    Terminal,
}

impl AnswerRoute {
    const fn of(kind: ConsentResponderKind) -> Self {
        match kind {
            ConsentResponderKind::Window => Self::Window,
            ConsentResponderKind::Terminal => Self::Terminal,
        }
    }

    /// The grant audit's reason for a deny given on this route.
    pub const fn deny_reason(self) -> &'static str {
        match self {
            Self::Local => "denied by the person",
            Self::Window => "answered in a remote Window",
            Self::Terminal => "answered in a terminal approver",
        }
    }
}

/// The audit reason of an accepted `AnswerConsentOffer`. Only the installed
/// relay can hold the Window role and only the installed client the
/// terminal one, so the executable names the route.
pub fn answer_audit_reason(peer: &PeerIdentity) -> &'static str {
    if peer.is_installed_relay() {
        "answered_in_remote_window"
    } else {
        "answered_in_terminal_approver"
    }
}

/// Why a connection may not be a responder, or may not be offered a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ineligible {
    /// It no longer runs the installed relay or client it was at `Hello`.
    Identity,
    /// Its process, or its place in the process tree, could not be proven.
    Unverified,
    /// It runs inside a Splint's session or process tree.
    InsideSplint,
    /// It runs inside the requester's session or process tree.
    InsideRequester,
    /// A terminal approver without its session's controlling terminal on
    /// standard input and output.
    NoTerminal,
}

impl Ineligible {
    pub const fn message(self) -> &'static str {
        match self {
            Self::Identity => "this connection no longer runs the installed relay or client",
            Self::Unverified => "this connection's process could not be verified",
            Self::InsideSplint => "a consent responder must run outside every Splint",
            Self::InsideRequester => "a consent responder must run outside the requester",
            Self::NoTerminal => "the approver needs its own terminal on standard input and output",
        }
    }
}

/// The process trees a responder must be outside.
#[derive(Debug, Clone, Default)]
pub struct TreeContext {
    /// Every Splint's session leader (its child process, whose PID is its
    /// session ID).
    pub splint_leaders: Vec<u32>,
    /// The requesting process, for an offer.
    pub requester: Option<Requester>,
}

/// A requesting process: its PID and start time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Requester {
    pub pid: u32,
    pub start_time: u64,
}

/// Checks one connection from `/proc`: that it still runs the installed
/// relay (`Window`) or client (`Terminal`) it ran at `Hello`, and, with a
/// `context`, that it runs where a responder may. Without one, as for an
/// answer, only the identity is checked.
pub fn check_responder(
    kind: ConsentResponderKind,
    peer: &PeerIdentity,
    context: Option<&TreeContext>,
) -> Result<(), Ineligible> {
    let identity = match kind {
        ConsentResponderKind::Window => {
            peer.is_matching_graphical_relay()
                && consent::executable_file_identity(peer.pid)
                    .is_ok_and(|executing| executing == peer.executable_file())
        }
        ConsentResponderKind::Terminal => peer.connection_matches_splinterm(),
    };
    if !identity {
        return Err(Ineligible::Identity);
    }
    let Some(context) = context else {
        return Ok(());
    };
    let start_time = peer.start_time().ok_or(Ineligible::Unverified)?;
    if kind == ConsentResponderKind::Terminal {
        controlling_terminal(peer.pid, start_time)?;
    }
    outside_trees_with(peer.pid, start_time, context, agent_status::read_proc_stat)
}

/// Whether the process `pid`, started at `start_time`, is outside every
/// Splint in `context` and outside its requester. Only a complete ancestry
/// walk proves it: a tree that cannot be read, or a reused PID met during
/// the walk, fails closed.
fn outside_trees_with(
    pid: u32,
    start_time: u64,
    context: &TreeContext,
    read: impl Fn(u32) -> Option<ProcStat>,
) -> Result<(), Ineligible> {
    let ancestry = agent_status::walk_ancestry_with(pid, start_time, &read)
        .filter(|ancestry| ancestry.complete)
        .ok_or(Ineligible::Unverified)?;
    let inside =
        |leader: u32| leader == pid || ancestry.session == leader || ancestry.descends_from(leader);
    if context
        .splint_leaders
        .iter()
        .any(|&leader| leader > 1 && inside(leader))
    {
        return Err(Ineligible::InsideSplint);
    }
    if let Some(requester) = context.requester {
        let requesting = read(requester.pid)
            .filter(|process| process.start_time == requester.start_time)
            .ok_or(Ineligible::Unverified)?;
        if inside(requester.pid) || requesting.session == ancestry.session {
            return Err(Ineligible::InsideRequester);
        }
    }
    // The walk described this process, not a later one reusing its PID.
    if read(pid).is_none_or(|process| process.start_time != start_time) {
        return Err(Ineligible::Unverified);
    }
    Ok(())
}

/// What one of a process's standard descriptors names: the link text and,
/// for a character device, its device number.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Descriptor {
    link: PathBuf,
    device: Option<u64>,
}

fn read_descriptor(pid: u32, descriptor: u32) -> Option<Descriptor> {
    let path = format!("/proc/{pid}/fd/{descriptor}");
    let link = std::fs::read_link(&path).ok()?;
    // Through the magic link, the open file itself.
    let metadata = std::fs::metadata(&path).ok()?;
    Some(Descriptor {
        link,
        device: metadata
            .file_type()
            .is_char_device()
            .then(|| metadata.rdev()),
    })
}

/// The `tty_nr` field of `/proc/<pid>/stat`: the session's controlling
/// terminal, 0 for none. Read after the command name's final `)`.
fn stat_terminal(contents: &[u8]) -> Option<u32> {
    let close = contents.iter().rposition(|&byte| byte == b')')?;
    let rest = std::str::from_utf8(&contents[close + 1..]).ok()?;
    // state, ppid, pgrp, session, tty_nr
    let terminal = rest.split_ascii_whitespace().nth(4)?.parse::<i32>().ok()?;
    u32::try_from(terminal).ok()
}

/// Proves that `pid` reads and writes its session's controlling terminal:
/// the session has one, and standard input and output are both open on that
/// very device, a `/dev/pts` or `/dev/tty*` terminal.
fn controlling_terminal(pid: u32, start_time: u64) -> Result<(), Ineligible> {
    let stat = std::fs::read(format!("/proc/{pid}/stat")).map_err(|_| Ineligible::Unverified)?;
    let terminal = stat_terminal(&stat).ok_or(Ineligible::Unverified)?;
    let descriptors = [read_descriptor(pid, 0), read_descriptor(pid, 1)];
    terminal_matches(terminal, &descriptors)?;
    if agent_status::process_start_time(pid) != Some(start_time) {
        return Err(Ineligible::Unverified);
    }
    Ok(())
}

fn terminal_matches(terminal: u32, descriptors: &[Option<Descriptor>]) -> Result<(), Ineligible> {
    if terminal == 0 {
        return Err(Ineligible::NoTerminal);
    }
    // The kernel's encoding of `tty_nr`.
    let controlling = (
        (terminal >> 8) & 0xfff,
        (terminal & 0xff) | ((terminal >> 12) & 0xf_ff00),
    );
    for descriptor in descriptors {
        let Some(Descriptor {
            link,
            device: Some(device),
        }) = descriptor
        else {
            return Err(Ineligible::NoTerminal);
        };
        if !terminal_path(link) || device_numbers(*device) != controlling {
            return Err(Ineligible::NoTerminal);
        }
    }
    Ok(())
}

/// A pseudo-terminal or a `/dev/tty*` device, but not the `/dev/tty` alias.
fn terminal_path(link: &Path) -> bool {
    if link.parent() == Some(Path::new("/dev/pts")) {
        return true;
    }
    link.parent() == Some(Path::new("/dev"))
        && link
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.len() > 3 && name.starts_with("tty"))
}

/// A `stat` device number's major and minor numbers, as glibc splits them.
fn device_numbers(device: u64) -> (u32, u32) {
    let major = ((device >> 8) & 0xfff) | ((device >> 32) & !0xfff);
    let minor = (device & 0xff) | ((device >> 12) & !0xff);
    (
        u32::try_from(major).unwrap_or(u32::MAX),
        u32::try_from(minor & 0xffff_ffff).unwrap_or(u32::MAX),
    )
}

fn random_nonce() -> std::io::Result<ConsentOfferNonce> {
    let mut bytes = [0_u8; CONSENT_OFFER_NONCE_BYTES];
    let mut offset = 0;
    while offset < bytes.len() {
        offset +=
            rustix::rand::getrandom(&mut bytes[offset..], rustix::rand::GetRandomFlags::empty())?;
    }
    Ok(ConsentOfferNonce::new(bytes))
}

/// The whole seconds an offer says are left: the prompt's remaining time
/// less the transport margin, at most the pending-request lifetime. `None`
/// when under a second would be left: no offer is made then, since an offer
/// gives its responder at least a second.
fn respond_within(expires: Instant, now: Instant) -> Option<u64> {
    let seconds = expires
        .saturating_duration_since(now)
        .saturating_sub(TRANSPORT_MARGIN)
        .as_secs();
    (seconds >= 1).then(|| seconds.min(MAX_CONSENT_RESPOND_WITHIN_SECONDS))
}

/// The answer a responder gave to the shown prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteAnswer {
    pub decision: ConsentDecision,
    pub route: AnswerRoute,
}

struct Responder {
    subscription_id: u64,
    kind: ConsentResponderKind,
    peer: PeerIdentity,
    events: mpsc::Sender<SubscriptionEvent>,
}

/// An offer outstanding on one responder connection.
struct Offer {
    connection_id: u64,
    nonce: ConsentOfferNonce,
}

/// The shown prompt's offers.
struct Round {
    id: u64,
    prompt: OfferedConsentPrompt,
    /// Outstanding offers by offer ID.
    offers: HashMap<u64, Offer>,
    /// A route has answered.
    settled: bool,
    answer: Option<oneshot::Sender<RemoteAnswer>>,
    outstanding: watch::Sender<usize>,
}

impl Round {
    fn publish_outstanding(&self) {
        self.outstanding.send_replace(self.offers.len());
    }
}

#[derive(Default)]
struct Inner {
    /// By connection: one subscription per connection.
    responders: HashMap<u64, Responder>,
    next_offer_id: u64,
    next_round_id: u64,
    round: Option<Round>,
}

impl Inner {
    /// Queues `event` for the responder on `connection_id`. One that stopped
    /// reading (a full queue) or is gone is dropped with its offer; `false`
    /// then.
    fn send(&mut self, connection_id: u64, event: SubscriptionEvent) -> bool {
        let Some(responder) = self.responders.get(&connection_id) else {
            return false;
        };
        if responder.events.try_send(event).is_ok() {
            return true;
        }
        debug!(
            connection_id,
            "a consent responder that stopped reading was dropped"
        );
        self.drop_responder(connection_id);
        false
    }

    /// Removes a responder; only its own offer goes with it.
    fn drop_responder(&mut self, connection_id: u64) {
        if self.responders.remove(&connection_id).is_none() {
            return;
        }
        if let Some(round) = &mut self.round {
            round
                .offers
                .retain(|_, offer| offer.connection_id != connection_id);
            round.publish_outstanding();
        }
    }

    /// Withdraws every outstanding offer of the shown prompt.
    fn withdraw_all(&mut self, reason: ConsentOfferEnd) {
        let Some(round) = &mut self.round else {
            return;
        };
        let offers: Vec<_> = round.offers.drain().collect();
        round.publish_outstanding();
        for (offer_id, offer) in offers {
            self.send(
                offer.connection_id,
                SubscriptionEvent::ConsentOfferWithdrawn { offer_id, reason },
            );
        }
    }

    fn round(&mut self, id: u64) -> Option<&mut Round> {
        self.round.as_mut().filter(|round| round.id == id)
    }
}

/// Answers a test sets in place of the `/proc` checks: given a connection
/// and the context (`None` for an answer's identity check).
#[cfg(test)]
pub type TestProbe = Arc<dyn Fn(u64, Option<&TreeContext>) -> Result<(), Ineligible> + Send + Sync>;

/// Every consent responder of this daemon and the offers of the prompt it
/// shows now.
pub struct ConsentResponders {
    daemon_generation: u64,
    inner: StdMutex<Inner>,
    #[cfg(test)]
    probe: StdMutex<Option<TestProbe>>,
}

impl ConsentResponders {
    pub fn new(daemon_generation: u64) -> Self {
        Self {
            daemon_generation,
            inner: StdMutex::default(),
            #[cfg(test)]
            probe: StdMutex::default(),
        }
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Replaces the `/proc` eligibility checks.
    #[cfg(test)]
    pub fn set_probe(&self, probe: TestProbe) {
        *self
            .probe
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(probe);
    }

    #[cfg_attr(
        not(test),
        allow(clippy::unused_self, reason = "only a test's probe reads the registry")
    )]
    fn eligible(
        &self,
        connection_id: u64,
        kind: ConsentResponderKind,
        peer: &PeerIdentity,
        context: Option<&TreeContext>,
    ) -> Result<(), Ineligible> {
        #[cfg(test)]
        {
            let probe = self
                .probe
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(probe) = probe {
                return probe(connection_id, context);
            }
        }
        let _ = connection_id;
        check_responder(kind, peer, context)
    }

    /// How many responders are registered.
    #[cfg(test)]
    pub fn responder_count(&self) -> usize {
        self.inner().responders.len()
    }

    /// Registers `connection_id` as a `kind` responder after checking, from
    /// `/proc`, that it may be one. Its events go to the returned stream;
    /// dropping the stream's guard ends it.
    ///
    /// # Errors
    /// `Unauthorized` for an ineligible process; `ResourceLimit` for a
    /// second subscription on one connection or beyond
    /// [`MAX_CONSENT_RESPONDERS`].
    pub async fn subscribe(
        self: &Arc<Self>,
        connection_id: u64,
        subscription_id: u64,
        kind: ConsentResponderKind,
        peer: &PeerIdentity,
        context: TreeContext,
    ) -> Result<ResponderStream, ProtocolError> {
        let registry = Arc::clone(self);
        let checked = peer.clone();
        let eligible = tokio::task::spawn_blocking(move || {
            registry.eligible(connection_id, kind, &checked, Some(&context))
        })
        .await
        .map_err(|_| {
            ProtocolError::new(ErrorCode::Internal, "the responder check did not finish")
        })?;
        if let Err(why) = eligible {
            return Err(ProtocolError::new(ErrorCode::Unauthorized, why.message()));
        }
        let (events, receiver) = mpsc::channel(RESPONDER_QUEUE);
        let mut inner = self.inner();
        if inner.responders.contains_key(&connection_id) {
            return Err(ProtocolError::new(
                ErrorCode::ResourceLimit,
                "this connection is already a consent responder",
            ));
        }
        if inner.responders.len() >= MAX_CONSENT_RESPONDERS {
            return Err(ProtocolError::new(
                ErrorCode::ResourceLimit,
                "too many consent responders are connected",
            ));
        }
        inner.responders.insert(
            connection_id,
            Responder {
                subscription_id,
                kind,
                peer: peer.clone(),
                events,
            },
        );
        drop(inner);
        Ok(ResponderStream {
            subscription_id,
            events: receiver,
            guard: ResponderGuard {
                registry: Arc::clone(self),
                connection_id,
                subscription_id,
            },
        })
    }

    /// Ends the responder on `connection_id` (its `subscription_id` only,
    /// when given); its outstanding offer goes with it.
    pub fn remove(&self, connection_id: u64, subscription_id: Option<u64>) {
        let mut inner = self.inner();
        if inner
            .responders
            .get(&connection_id)
            .is_some_and(|responder| {
                subscription_id.is_none_or(|id| id == responder.subscription_id)
            })
        {
            inner.drop_responder(connection_id);
        }
    }

    /// The connection and subscription of every responder `/proc` shows may
    /// be offered this prompt now.
    async fn eligible_responders(
        self: &Arc<Self>,
        prompt: &OfferedConsentPrompt,
        context: TreeContext,
    ) -> Vec<(u64, u64)> {
        if context.requester.is_none() || prompt.validate().is_err() {
            return Vec::new();
        }
        let candidates: Vec<_> = self
            .inner()
            .responders
            .iter()
            .map(|(&connection_id, responder)| {
                (
                    connection_id,
                    responder.subscription_id,
                    responder.kind,
                    responder.peer.clone(),
                )
            })
            .collect();
        if candidates.is_empty() {
            return Vec::new();
        }
        let registry = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            candidates
                .into_iter()
                .filter(|(connection_id, _, kind, peer)| {
                    registry
                        .eligible(*connection_id, *kind, peer, Some(&context))
                        .inspect_err(|why| {
                            debug!(
                                connection_id,
                                reason = why.message(),
                                "a consent responder is not offered this prompt"
                            );
                        })
                        .is_ok()
                })
                .map(|(connection_id, subscription_id, ..)| (connection_id, subscription_id))
                .collect()
        })
        .await
        .unwrap_or_default()
    }

    /// Offers the prompt now shown for `request_id` to every responder
    /// eligible for it, each with its own offer ID and nonce. Without a
    /// requester in `context` nobody can be proven outside it, so nobody is
    /// offered it.
    pub async fn open_round(
        self: &Arc<Self>,
        request_id: u64,
        prompt: OfferedConsentPrompt,
        expires: Instant,
        context: TreeContext,
    ) -> OpenRound {
        let eligible = self.eligible_responders(&prompt, context).await;
        let (answer, answered) = oneshot::channel();
        let (outstanding, outstanding_changes) = watch::channel(0);
        // With too little time left for an answer to arrive, nobody is
        // offered the prompt; it expires rather than being unavailable.
        let respond_within_seconds = respond_within(expires, Instant::now());
        let too_late = respond_within_seconds.is_none() && !eligible.is_empty();
        let mut inner = self.inner();
        // One prompt shows at a time; a round left open by a lost prompter
        // ends here.
        inner.withdraw_all(ConsentOfferEnd::RequestEnded);
        inner.round = None;
        inner.next_round_id = inner.next_round_id.wrapping_add(1);
        let id = inner.next_round_id;
        let mut offers = HashMap::new();
        for (connection_id, subscription_id) in eligible {
            let Some(respond_within_seconds) = respond_within_seconds else {
                break;
            };
            if inner
                .responders
                .get(&connection_id)
                .is_none_or(|responder| responder.subscription_id != subscription_id)
            {
                continue;
            }
            let Some(offer_id) = inner.next_offer_id.checked_add(1) else {
                break;
            };
            let nonce = match random_nonce() {
                Ok(nonce) => nonce,
                Err(error) => {
                    warn!(%error, "OS randomness is unavailable; a consent responder is not offered this prompt");
                    continue;
                }
            };
            inner.next_offer_id = offer_id;
            let offer = ConsentOffer {
                daemon_generation: self.daemon_generation,
                offer_id,
                request_id,
                nonce,
                respond_within_seconds,
                prompt: prompt.clone(),
            };
            if inner.send(connection_id, SubscriptionEvent::ConsentOffered { offer }) {
                offers.insert(
                    offer_id,
                    Offer {
                        connection_id,
                        nonce,
                    },
                );
            }
        }
        let offered = offers.len();
        let round = Round {
            id,
            prompt,
            offers,
            settled: false,
            answer: Some(answer),
            outstanding,
        };
        round.publish_outstanding();
        inner.round = Some(round);
        drop(inner);
        OpenRound {
            registry: Arc::clone(self),
            id,
            offered,
            too_late,
            answered: Some(answered),
            outstanding: outstanding_changes,
            end: ConsentOfferEnd::RequestEnded,
        }
    }

    /// Accepts one answer: only on the connection the offer was sent to,
    /// only while it is outstanding, and only for this generation and nonce
    /// (compared in constant time). A grant must name a duration the
    /// offered prompt offers. The first valid answer settles the prompt and
    /// withdraws every other offer.
    ///
    /// # Errors
    /// `NotFound`, granting nothing, for anything but an outstanding offer
    /// on this connection with this generation and nonce; `InvalidArgument`
    /// for a duration the prompt does not offer, which leaves the offer
    /// outstanding; `Unauthorized` once the connection no longer runs the
    /// installed relay or client.
    pub fn answer(
        &self,
        connection_id: u64,
        peer: &PeerIdentity,
        daemon_generation: u64,
        offer_id: u64,
        nonce: &ConsentOfferNonce,
        decision: ConsentDecision,
    ) -> Result<AnswerRoute, ProtocolError> {
        let not_found = || {
            ProtocolError::new(
                ErrorCode::NotFound,
                "no consent offer outstanding on this connection matches the answer",
            )
        };
        let kind = self
            .inner()
            .responders
            .get(&connection_id)
            .map(|responder| responder.kind)
            .ok_or_else(not_found)?;
        if self.eligible(connection_id, kind, peer, None).is_err() {
            return Err(ProtocolError::new(
                ErrorCode::Unauthorized,
                Ineligible::Identity.message(),
            ));
        }
        let mut inner = self.inner();
        if daemon_generation != self.daemon_generation {
            return Err(not_found());
        }
        let Some(round) = inner.round.as_mut().filter(|round| !round.settled) else {
            return Err(not_found());
        };
        if !round
            .offers
            .get(&offer_id)
            .is_some_and(|offer| offer.connection_id == connection_id && offer.nonce == *nonce)
        {
            return Err(not_found());
        }
        if let ConsentDecision::Grant { duration } = decision
            && !round.prompt.offers(duration)
        {
            return Err(ProtocolError::new(
                ErrorCode::InvalidArgument,
                "the answer names a duration the prompt does not offer",
            ));
        }
        let route = AnswerRoute::of(kind);
        round.offers.remove(&offer_id);
        round.settled = true;
        // The prompter learns of the answer before the offers it outlives
        // are withdrawn, so it never mistakes their end for every route
        // closing.
        if let Some(answer) = round.answer.take() {
            let _ = answer.send(RemoteAnswer { decision, route });
        }
        inner.withdraw_all(ConsentOfferEnd::AnsweredElsewhere);
        Ok(route)
    }

    /// The daemon's own dialog answered: it wins unless a responder already
    /// did, and every offer is withdrawn.
    fn claim_local(&self, id: u64) -> bool {
        let mut inner = self.inner();
        let Some(round) = inner.round(id).filter(|round| !round.settled) else {
            return false;
        };
        round.settled = true;
        round.answer = None;
        inner.withdraw_all(ConsentOfferEnd::AnsweredElsewhere);
        true
    }

    fn settled(&self, id: u64) -> bool {
        self.inner().round(id).is_some_and(|round| round.settled)
    }

    fn close_round(&self, id: u64, reason: ConsentOfferEnd) {
        let mut inner = self.inner();
        if inner.round(id).is_some() {
            inner.withdraw_all(reason);
            inner.round = None;
        }
    }
}

/// The offers of one shown prompt. Dropping it withdraws every offer still
/// outstanding, with [`Self::end_with`]'s reason (`request_ended` unless
/// set), so every way a prompt ends unanswered withdraws its offers.
pub struct OpenRound {
    registry: Arc<ConsentResponders>,
    id: u64,
    offered: usize,
    too_late: bool,
    answered: Option<oneshot::Receiver<RemoteAnswer>>,
    outstanding: watch::Receiver<usize>,
    end: ConsentOfferEnd,
}

impl OpenRound {
    /// How many responders were offered the prompt.
    pub const fn offered(&self) -> usize {
        self.offered
    }

    /// Whether responders could have been offered the prompt but too little
    /// time was left: it then expires instead of being unavailable.
    pub const fn too_late(&self) -> bool {
        self.too_late
    }

    /// Resolves with the winning responder's answer; never when another
    /// route won or the round closed.
    pub async fn answered(&mut self) -> RemoteAnswer {
        if let Some(answered) = &mut self.answered {
            if let Ok(answer) = answered.await {
                self.answered = None;
                return answer;
            }
            self.answered = None;
        }
        std::future::pending().await
    }

    /// Resolves once no offer is outstanding and no responder answered:
    /// every responder showing the prompt went away.
    pub async fn every_route_closed(outstanding: &mut watch::Receiver<usize>) {
        let _ = outstanding.wait_for(|count| *count == 0).await;
    }

    /// The outstanding-offer count, for [`Self::every_route_closed`].
    pub fn outstanding(&self) -> watch::Receiver<usize> {
        self.outstanding.clone()
    }

    /// Whether offers are still outstanding now.
    pub fn any_outstanding(&self) -> bool {
        *self.outstanding.borrow() > 0
    }

    /// Whether a route has answered.
    pub fn settled(&self) -> bool {
        self.registry.settled(self.id)
    }

    /// The daemon's own dialog answered: `true` when it won.
    pub fn claim_local(&self) -> bool {
        self.registry.claim_local(self.id)
    }

    /// The reason the offers are withdrawn with when this round closes.
    pub fn end_with(&mut self, reason: ConsentOfferEnd) {
        self.end = reason;
    }
}

impl Drop for OpenRound {
    fn drop(&mut self) {
        self.registry.close_round(self.id, self.end);
    }
}

/// One responder's subscription: the events for its connection, and the
/// guard whose drop ends it.
pub struct ResponderStream {
    subscription_id: u64,
    events: mpsc::Receiver<SubscriptionEvent>,
    guard: ResponderGuard,
}

impl std::fmt::Debug for ResponderStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResponderStream")
            .field("subscription_id", &self.subscription_id)
            .field("connection_id", &self.guard.connection_id)
            .finish_non_exhaustive()
    }
}

impl ResponderStream {
    pub const fn subscription_id(&self) -> u64 {
        self.subscription_id
    }

    pub fn into_parts(self) -> (mpsc::Receiver<SubscriptionEvent>, ResponderGuard) {
        (self.events, self.guard)
    }
}

/// Ends its responder when dropped: on `Detach`, on disconnect, or when its
/// subscription never started.
pub struct ResponderGuard {
    registry: Arc<ConsentResponders>,
    connection_id: u64,
    subscription_id: u64,
}

impl Drop for ResponderGuard {
    fn drop(&mut self) {
        self.registry
            .remove(self.connection_id, Some(self.subscription_id));
    }
}

#[cfg(test)]
mod tests {
    use splinterm_core::LairId;
    use splinterm_protocol::{AccessScope, ConsentDurationOffer, ConsentTarget, GrantDuration};

    use super::*;

    fn table(entries: &[(u32, u32, u32, u64)]) -> impl Fn(u32) -> Option<ProcStat> {
        let entries = entries.to_vec();
        move |pid| {
            entries
                .iter()
                .find(|entry| entry.0 == pid)
                .map(|&(_, parent, session, start_time)| ProcStat {
                    parent,
                    session,
                    start_time,
                })
        }
    }

    fn splints(leaders: &[u32]) -> TreeContext {
        TreeContext {
            splint_leaders: leaders.to_vec(),
            requester: None,
        }
    }

    /// A Splint (leader 100), a requester (300, inside session 300), and
    /// relays started in, under, or beside each.
    fn processes() -> impl Fn(u32) -> Option<ProcStat> {
        table(&[
            // sshd and a login session.
            (50, 1, 50, 2),
            (60, 50, 60, 3),
            // A relay in its own sshd session: outside everything.
            (61, 60, 60, 4),
            // The Splint and a relay inside it, then one that left its
            // session with setsid but still descends from it.
            (100, 1, 100, 10),
            (110, 100, 100, 20),
            (120, 110, 120, 30),
            // The requester and a relay it started, then one in its session
            // but reparented to init.
            (300, 1, 300, 40),
            (310, 300, 310, 50),
            (320, 1, 300, 60),
        ])
    }

    #[test]
    fn a_relay_inside_a_splint_or_the_requester_is_refused() {
        let read = processes();
        let requester = TreeContext {
            splint_leaders: vec![100],
            requester: Some(Requester {
                pid: 300,
                start_time: 40,
            }),
        };
        assert_eq!(outside_trees_with(61, 4, &requester, &read), Ok(()));
        for (pid, start) in [(110, 20), (120, 30)] {
            assert_eq!(
                outside_trees_with(pid, start, &requester, &read),
                Err(Ineligible::InsideSplint),
                "{pid}"
            );
        }
        assert_eq!(
            outside_trees_with(100, 10, &splints(&[100]), &read),
            Err(Ineligible::InsideSplint),
            "the Splint's own leader"
        );
        for (pid, start) in [(300, 40), (310, 50), (320, 60)] {
            assert_eq!(
                outside_trees_with(pid, start, &requester, &read),
                Err(Ineligible::InsideRequester),
                "{pid}"
            );
        }
        // At subscribe there is no requester yet: only the Splints count.
        assert_eq!(outside_trees_with(310, 50, &splints(&[100]), &read), Ok(()));
        // A requester that is no longer the process that asked fails closed.
        let gone = TreeContext {
            splint_leaders: Vec::new(),
            requester: Some(Requester {
                pid: 300,
                start_time: 41,
            }),
        };
        assert_eq!(
            outside_trees_with(61, 4, &gone, &read),
            Err(Ineligible::Unverified)
        );
    }

    #[test]
    fn an_unproven_process_tree_fails_closed() {
        // A reused PID: the responder's recorded start time is stale.
        let read = processes();
        assert_eq!(
            outside_trees_with(61, 5, &splints(&[]), &read),
            Err(Ineligible::Unverified)
        );
        // An unreadable ancestor proves nothing either way.
        let cut = table(&[(70, 65, 70, 9)]);
        assert_eq!(
            outside_trees_with(70, 9, &splints(&[]), &cut),
            Err(Ineligible::Unverified)
        );
        // An ancestor younger than its child is a reused PID.
        let reused = table(&[(70, 65, 70, 9), (65, 1, 65, 80)]);
        assert_eq!(
            outside_trees_with(70, 9, &splints(&[]), &reused),
            Err(Ineligible::Unverified)
        );
        // A walk deeper than the bound cannot reach init.
        let deep: Vec<_> = (1000..1100)
            .map(|pid| (pid, if pid == 1000 { 1 } else { pid - 1 }, 1000, 1))
            .collect();
        assert_eq!(
            outside_trees_with(1099, 1, &splints(&[]), table(&deep)),
            Err(Ineligible::Unverified)
        );
        assert_eq!(
            outside_trees_with(1010, 1, &splints(&[]), table(&deep)),
            Ok(())
        );
    }

    #[test]
    fn the_live_process_tree_places_a_child_inside_its_parent() {
        let own = std::process::id();
        let own_start = agent_status::process_start_time(own).unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .stdin(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let child_pid = child.id();
        let child_start = agent_status::process_start_time(child_pid).unwrap();
        let read = agent_status::read_proc_stat;
        let requester = |pid, start_time| TreeContext {
            splint_leaders: Vec::new(),
            requester: Some(Requester { pid, start_time }),
        };
        // A process this test started, as if it were the requester's.
        assert_eq!(
            outside_trees_with(child_pid, child_start, &requester(own, own_start), read),
            Err(Ineligible::InsideRequester)
        );
        // Treated as a Splint leader, this process holds its child.
        assert_eq!(
            outside_trees_with(child_pid, child_start, &splints(&[own]), read),
            Err(Ineligible::InsideSplint)
        );
        // The child is not its parent's ancestor.
        assert_eq!(
            outside_trees_with(own, own_start, &splints(&[child_pid]), read),
            Ok(())
        );
        // A stopped-and-reaped child is no longer verifiable.
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(
            outside_trees_with(child_pid, child_start, &splints(&[]), read),
            Err(Ineligible::Unverified)
        );
    }

    #[test]
    fn a_terminal_approver_needs_its_controlling_terminal_on_both_descriptors() {
        let pts = |device| {
            Some(Descriptor {
                link: PathBuf::from("/dev/pts/3"),
                device: Some(device),
            })
        };
        // /dev/pts/3 is major 136, minor 3: tty_nr 34819, and the same
        // device number in `stat`.
        let (tty_nr, rdev) = (34_819, (136 << 8) | 3);
        assert_eq!(terminal_matches(tty_nr, &[pts(rdev), pts(rdev)]), Ok(()));
        assert_eq!(
            terminal_matches(0, &[pts(rdev), pts(rdev)]),
            Err(Ineligible::NoTerminal),
            "no controlling terminal"
        );
        assert_eq!(
            terminal_matches(tty_nr, &[pts(rdev), None]),
            Err(Ineligible::NoTerminal),
            "an unreadable descriptor"
        );
        let pipe = Some(Descriptor {
            link: PathBuf::from("pipe:[1234]"),
            device: None,
        });
        assert_eq!(
            terminal_matches(tty_nr, &[pts(rdev), pipe]),
            Err(Ineligible::NoTerminal),
            "output redirected"
        );
        assert_eq!(
            terminal_matches(tty_nr, &[pts(rdev), pts((136 << 8) | 4)]),
            Err(Ineligible::NoTerminal),
            "another terminal than the session's"
        );
        let alias = Some(Descriptor {
            link: PathBuf::from("/dev/tty"),
            device: Some(5 << 8),
        });
        assert_eq!(
            terminal_matches(1280, &[alias.clone(), alias]),
            Err(Ineligible::NoTerminal),
            "the /dev/tty alias proves nothing"
        );
        // A virtual console: /dev/tty2 is major 4, minor 2.
        let console = Some(Descriptor {
            link: PathBuf::from("/dev/tty2"),
            device: Some((4 << 8) | 2),
        });
        assert_eq!(terminal_matches(1026, &[console.clone(), console]), Ok(()));
        // Large minors use the split encodings.
        let minor = 300_u64;
        let rdev = (minor & 0xff) | (136 << 8) | ((minor & !0xff) << 12);
        let tty_nr = u32::try_from(rdev).unwrap();
        assert_eq!(terminal_matches(tty_nr, &[pts(rdev), pts(rdev)]), Ok(()));
        assert_eq!(device_numbers(rdev), (136, 300));
    }

    #[test]
    fn stat_terminal_reads_tty_nr_after_the_command_name() {
        assert_eq!(
            stat_terminal(b"42 (a) b) S 1 42 42 34819 42 0 0"),
            Some(34_819)
        );
        assert_eq!(stat_terminal(b"42 (x) S 1 42 42 0 -1"), Some(0));
        assert_eq!(stat_terminal(b"42 (x) S 1 42 42 -5 -1"), None);
        assert_eq!(stat_terminal(b"garbage"), None);
    }

    #[test]
    fn a_process_without_a_terminal_is_not_an_approver() {
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let start = agent_status::process_start_time(child.id()).unwrap();
        assert_eq!(
            controlling_terminal(child.id(), start),
            Err(Ineligible::NoTerminal)
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn offers_report_the_remaining_time_less_the_margin() {
        let now = Instant::now();
        assert_eq!(
            respond_within(now + Duration::from_secs(120), now),
            Some(118)
        );
        assert_eq!(
            respond_within(now + Duration::from_secs(500), now),
            Some(120)
        );
        assert_eq!(
            respond_within(now + Duration::from_millis(3_900), now),
            Some(1)
        );
        assert_eq!(respond_within(now + Duration::from_secs(3), now), Some(1));
        // Under a second after the margin, no offer is made.
        assert_eq!(
            respond_within(now + Duration::from_millis(2_999), now),
            None
        );
        assert_eq!(respond_within(now, now + Duration::from_secs(5)), None);
    }

    #[tokio::test]
    async fn a_prompt_with_too_little_time_left_is_offered_to_nobody() {
        let registry = Arc::new(ConsentResponders::new(7));
        permissive(&registry);
        let mut stream = responder(&registry, 1).await;
        let round = registry
            .open_round(
                42,
                prompt(),
                Instant::now() + Duration::from_millis(2_500),
                offer_context(),
            )
            .await;
        assert_eq!(round.offered(), 0);
        assert!(round.too_late(), "a responder could have shown it");
        assert!(stream.events.try_recv().is_err());
        drop(round);
        // With nobody to offer it to, it is simply unoffered.
        let lone = Arc::new(ConsentResponders::new(7));
        permissive(&lone);
        let round = lone
            .open_round(
                43,
                prompt(),
                Instant::now() + Duration::from_millis(2_500),
                offer_context(),
            )
            .await;
        assert!(!round.too_late());
    }

    fn prompt() -> OfferedConsentPrompt {
        OfferedConsentPrompt {
            requester: "/usr/bin/agent".into(),
            requester_pid: 300,
            requester_uid: 1000,
            agent_name: None,
            target: ConsentTarget::Lair {
                lair_id: LairId::new(),
                lair_name: "api".into(),
            },
            scopes: vec![AccessScope::Observe],
            level: None,
            durations: vec![ConsentDurationOffer {
                duration: GrantDuration::Once,
                withheld: None,
                shared_by: None,
                replaces: None,
            }],
        }
    }

    fn offer_context() -> TreeContext {
        TreeContext {
            splint_leaders: Vec::new(),
            requester: Some(Requester {
                pid: 300,
                start_time: 40,
            }),
        }
    }

    fn permissive(registry: &ConsentResponders) {
        registry.set_probe(Arc::new(|_, _| Ok(())));
    }

    async fn responder(registry: &Arc<ConsentResponders>, connection_id: u64) -> ResponderStream {
        registry
            .subscribe(
                connection_id,
                1000 + connection_id,
                ConsentResponderKind::Window,
                &PeerIdentity::for_test(),
                TreeContext::default(),
            )
            .await
            .unwrap()
    }

    fn offered(stream: &mut ResponderStream) -> ConsentOffer {
        match stream.events.try_recv() {
            Ok(SubscriptionEvent::ConsentOffered { offer }) => offer,
            other => panic!("expected an offer, got {other:?}"),
        }
    }

    fn withdrawn(stream: &mut ResponderStream) -> (u64, ConsentOfferEnd) {
        match stream.events.try_recv() {
            Ok(SubscriptionEvent::ConsentOfferWithdrawn { offer_id, reason }) => (offer_id, reason),
            other => panic!("expected a withdrawal, got {other:?}"),
        }
    }

    const ONCE: ConsentDecision = ConsentDecision::Grant {
        duration: GrantDuration::Once,
    };

    #[tokio::test]
    async fn every_responder_gets_its_own_offer_and_the_first_answer_wins() {
        let registry = Arc::new(ConsentResponders::new(7));
        permissive(&registry);
        let mut streams = Vec::new();
        for connection_id in 1..=3 {
            streams.push(responder(&registry, connection_id).await);
        }
        let expires = Instant::now() + Duration::from_secs(60);
        let shown = prompt();
        let mut round = registry
            .open_round(42, shown.clone(), expires, offer_context())
            .await;
        assert_eq!(round.offered(), 3);
        let offers: Vec<_> = streams.iter_mut().map(offered).collect();
        for offer in &offers {
            assert_eq!(offer.validate(), Ok(()));
            assert_eq!(offer.daemon_generation, 7);
            assert_eq!(offer.request_id, 42);
            assert!((57..=58).contains(&offer.respond_within_seconds));
            assert_eq!(offer.prompt, shown);
        }
        let ids: std::collections::BTreeSet<_> =
            offers.iter().map(|offer| offer.offer_id).collect();
        assert_eq!(ids.len(), 3, "distinct offer ids");
        assert!(!ids.contains(&0));
        for (index, offer) in offers.iter().enumerate() {
            for other in &offers[index + 1..] {
                assert_ne!(offer.nonce, other.nonce, "distinct nonces");
            }
        }
        let peer = PeerIdentity::for_test();
        // The second responder answers first.
        let winner = &offers[1];
        assert_eq!(
            registry.answer(2, &peer, 7, winner.offer_id, &winner.nonce, ONCE),
            Ok(AnswerRoute::Window)
        );
        assert_eq!(
            round.answered().await,
            RemoteAnswer {
                decision: ONCE,
                route: AnswerRoute::Window,
            }
        );
        // Every other responder sees its own offer withdrawn.
        for index in [0, 2] {
            assert_eq!(
                withdrawn(&mut streams[index]),
                (offers[index].offer_id, ConsentOfferEnd::AnsweredElsewhere)
            );
        }
        assert!(
            streams[1].events.try_recv().is_err(),
            "the winner is answered"
        );
        // Later answers, even the loser's valid one, find nothing.
        for (connection_id, offer) in [(1, &offers[0]), (2, winner)] {
            assert_eq!(
                registry
                    .answer(connection_id, &peer, 7, offer.offer_id, &offer.nonce, ONCE)
                    .unwrap_err()
                    .code,
                ErrorCode::NotFound
            );
        }
        assert!(!round.claim_local(), "the local dialog lost");
        drop(round);
        for stream in &mut streams {
            assert!(
                stream.events.try_recv().is_err(),
                "nothing more after closing"
            );
        }
    }

    #[tokio::test]
    async fn only_the_exact_outstanding_offer_on_its_own_connection_is_answered() {
        let registry = Arc::new(ConsentResponders::new(7));
        permissive(&registry);
        let mut first = responder(&registry, 1).await;
        let mut second = responder(&registry, 2).await;
        let expires = Instant::now() + Duration::from_secs(60);
        let mut round = registry
            .open_round(42, prompt(), expires, offer_context())
            .await;
        let offer = offered(&mut first);
        let other = offered(&mut second);
        let peer = PeerIdentity::for_test();
        let code = |connection_id, generation, offer_id, nonce: &ConsentOfferNonce, decision| {
            registry
                .answer(connection_id, &peer, generation, offer_id, nonce, decision)
                .unwrap_err()
                .code
        };
        let mut wrong = *offer.nonce.as_bytes();
        wrong[31] ^= 1;
        let wrong = ConsentOfferNonce::new(wrong);
        // A wrong nonce, another connection, another generation, another
        // connection's offer, an unknown offer, and a connection that is no
        // responder.
        assert_eq!(
            code(1, 7, offer.offer_id, &wrong, ONCE),
            ErrorCode::NotFound
        );
        assert_eq!(
            code(2, 7, offer.offer_id, &offer.nonce, ONCE),
            ErrorCode::NotFound
        );
        assert_eq!(
            code(1, 8, offer.offer_id, &offer.nonce, ONCE),
            ErrorCode::NotFound
        );
        assert_eq!(
            code(1, 7, other.offer_id, &other.nonce, ONCE),
            ErrorCode::NotFound
        );
        assert_eq!(code(1, 7, 999, &offer.nonce, ONCE), ErrorCode::NotFound);
        assert_eq!(
            code(9, 7, offer.offer_id, &offer.nonce, ONCE),
            ErrorCode::NotFound
        );
        // A duration the prompt does not offer is refused and leaves the
        // offer outstanding.
        let always = ConsentDecision::Grant {
            duration: GrantDuration::Remembered,
        };
        assert_eq!(
            code(1, 7, offer.offer_id, &offer.nonce, always),
            ErrorCode::InvalidArgument
        );
        assert!(round.any_outstanding() && !round.settled());
        // An identity that no longer holds grants nothing.
        registry.set_probe(Arc::new(|_, context| match context {
            None => Err(Ineligible::Identity),
            Some(_) => Ok(()),
        }));
        assert_eq!(
            code(1, 7, offer.offer_id, &offer.nonce, ConsentDecision::Deny),
            ErrorCode::Unauthorized
        );
        permissive(&registry);
        // A deny is an answer too.
        assert_eq!(
            registry.answer(
                1,
                &peer,
                7,
                offer.offer_id,
                &offer.nonce,
                ConsentDecision::Deny
            ),
            Ok(AnswerRoute::Window)
        );
        assert_eq!(round.answered().await.decision, ConsentDecision::Deny);
        assert_eq!(
            withdrawn(&mut second),
            (other.offer_id, ConsentOfferEnd::AnsweredElsewhere)
        );
        assert_eq!(
            code(1, 7, offer.offer_id, &offer.nonce, ConsentDecision::Deny),
            ErrorCode::NotFound,
            "a duplicate"
        );
    }

    #[tokio::test]
    async fn an_unanswered_prompt_withdraws_with_its_reason_and_answers_after_find_nothing() {
        let registry = Arc::new(ConsentResponders::new(7));
        permissive(&registry);
        let mut stream = responder(&registry, 1).await;
        for reason in [
            ConsentOfferEnd::Cancelled,
            ConsentOfferEnd::Expired,
            ConsentOfferEnd::RequestEnded,
            ConsentOfferEnd::Fenced,
        ] {
            let mut round = registry
                .open_round(
                    42,
                    prompt(),
                    Instant::now() + Duration::from_secs(60),
                    offer_context(),
                )
                .await;
            let offer = offered(&mut stream);
            round.end_with(reason);
            drop(round);
            assert_eq!(withdrawn(&mut stream), (offer.offer_id, reason));
            assert_eq!(
                registry
                    .answer(
                        1,
                        &PeerIdentity::for_test(),
                        7,
                        offer.offer_id,
                        &offer.nonce,
                        ONCE
                    )
                    .unwrap_err()
                    .code,
                ErrorCode::NotFound,
                "an answer after the withdrawal"
            );
        }
    }

    #[tokio::test]
    async fn the_local_dialog_answering_first_withdraws_every_offer() {
        let registry = Arc::new(ConsentResponders::new(7));
        permissive(&registry);
        let mut stream = responder(&registry, 1).await;
        let round = registry
            .open_round(
                42,
                prompt(),
                Instant::now() + Duration::from_secs(60),
                offer_context(),
            )
            .await;
        let offer = offered(&mut stream);
        assert!(round.claim_local());
        assert_eq!(
            withdrawn(&mut stream),
            (offer.offer_id, ConsentOfferEnd::AnsweredElsewhere)
        );
        assert_eq!(
            registry
                .answer(
                    1,
                    &PeerIdentity::for_test(),
                    7,
                    offer.offer_id,
                    &offer.nonce,
                    ONCE
                )
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        assert!(!round.claim_local());
    }

    #[tokio::test]
    async fn a_lost_responder_takes_only_its_own_offer() {
        let registry = Arc::new(ConsentResponders::new(7));
        permissive(&registry);
        let first = responder(&registry, 1).await;
        let mut second = responder(&registry, 2).await;
        let round = registry
            .open_round(
                42,
                prompt(),
                Instant::now() + Duration::from_secs(60),
                offer_context(),
            )
            .await;
        let mut outstanding = round.outstanding();
        assert_eq!(*outstanding.borrow(), 2);
        drop(first);
        assert_eq!(registry.responder_count(), 1);
        assert_eq!(*outstanding.borrow(), 1);
        assert!(round.any_outstanding());
        let offer = offered(&mut second);
        // Detaching ends the last one; nothing is left.
        registry.remove(2, Some(9999));
        assert_eq!(
            registry.responder_count(),
            1,
            "another subscription id is not it"
        );
        registry.remove(2, Some(1002));
        assert_eq!(registry.responder_count(), 0);
        tokio::time::timeout(
            Duration::from_secs(1),
            OpenRound::every_route_closed(&mut outstanding),
        )
        .await
        .unwrap();
        assert!(!round.settled());
        assert_eq!(
            registry
                .answer(
                    2,
                    &PeerIdentity::for_test(),
                    7,
                    offer.offer_id,
                    &offer.nonce,
                    ONCE
                )
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
    }

    #[tokio::test]
    async fn responders_are_bounded_one_per_connection_and_checked() {
        let registry = Arc::new(ConsentResponders::new(7));
        permissive(&registry);
        let mut streams = Vec::new();
        for connection_id in 1..=u64::try_from(MAX_CONSENT_RESPONDERS).unwrap() {
            streams.push(responder(&registry, connection_id).await);
        }
        let peer = PeerIdentity::for_test();
        let subscribe = |connection_id| {
            let registry = Arc::clone(&registry);
            let peer = peer.clone();
            async move {
                registry
                    .subscribe(
                        connection_id,
                        5000,
                        ConsentResponderKind::Window,
                        &peer,
                        TreeContext::default(),
                    )
                    .await
                    .unwrap_err()
                    .code
            }
        };
        assert_eq!(subscribe(100).await, ErrorCode::ResourceLimit);
        streams.pop();
        // One subscription per connection.
        assert_eq!(subscribe(1).await, ErrorCode::ResourceLimit);
        // An ineligible process is refused.
        registry.set_probe(Arc::new(|_, _| Err(Ineligible::InsideSplint)));
        assert_eq!(subscribe(100).await, ErrorCode::Unauthorized);
        // Eligibility is checked again at each offer: a responder no longer
        // eligible is not offered the prompt, and with none the round is
        // empty.
        let round = registry
            .open_round(
                42,
                prompt(),
                Instant::now() + Duration::from_secs(60),
                offer_context(),
            )
            .await;
        assert_eq!(round.offered(), 0);
        assert!(streams[0].events.try_recv().is_err());
        drop(round);
        // Without a requester to check against, nobody is offered it.
        permissive(&registry);
        let round = registry
            .open_round(
                42,
                prompt(),
                Instant::now() + Duration::from_secs(60),
                TreeContext::default(),
            )
            .await;
        assert_eq!(round.offered(), 0);
    }

    #[test]
    fn the_real_check_refuses_a_process_that_is_not_the_installed_relay() {
        // This test process is not the installed relay or trusted client.
        let peer = PeerIdentity::for_test();
        for kind in [ConsentResponderKind::Window, ConsentResponderKind::Terminal] {
            assert_eq!(
                check_responder(kind, &peer, Some(&TreeContext::default())),
                Err(Ineligible::Identity)
            );
        }
    }
}
