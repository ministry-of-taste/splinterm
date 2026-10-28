//! A remote Window answers its daemon's agent access prompts (ADR 0015).
//!
//! A Window of a remote (SSH profile) endpoint registers one dedicated relay
//! connection as a consent responder (`SubscribeConsentOffers { kind:
//! window }`). The daemon offers it every workspace access prompt at the same
//! time as its other routes, and the first valid answer wins.
//!
//! For each offer the Window shows the trusted consent dialog itself, as
//! `/proc/self/exe consent`, the same image as the Window and the same child
//! the daemon launches locally. It builds that dialog's prompt from the
//! offer's content, but never takes its origin from the offer: the origin is
//! the Window's own SSH profile and the remote daemon's sanitised `Hello`
//! hostname, so a remote daemon can claim any hostname but never a local
//! prompt. The dialog gets a fresh random capability, never the offer's
//! nonce, which only ever goes back to the daemon in the answer.
//!
//! The dialog races its answer against the offer's withdrawal, its deadline,
//! and the loss of the connection. Only an answer is ever sent: a withdrawal,
//! a lost connection, or the deadline kills and reaps the dialog and sends
//! nothing, so the dialog never denies by itself. A second offer while one
//! shows, or an offer that fails validation, is a protocol fault: the dialog
//! goes and the Window subscribes again on a fresh connection.
//!
//! The responder lives as long as its Window. It subscribes again, with a
//! bounded backoff, after its connection ends, as when the remote daemon is
//! upgraded live (nothing about responders survives a handoff), and it
//! waits while the Window itself reconnects. The viewer's
//! `[integration] remote-agent-prompts = no` stops it, at once on Reload
//! configuration, and `yes` starts it again.

use std::{convert::Infallible, future::Future, process::Command, time::Duration};

use anyhow::{Result, anyhow};
use splinterm::{
    automation::{Connection, protocol_error},
    config::TopologyReloadSettings,
    endpoint::{ConnectionFactory, RemoteConsent, RemoteDisplayIdentity},
    handoff_status::WindowHandoffPhase,
};
use splinterm_protocol::{
    ConsentDecision, ConsentOffer, ConsentOrigin, ConsentPrompt, ConsentReply,
    ConsentResponderKind, ErrorCode, MAX_CONSENT_FRAME_BYTES, MAX_CONSENT_RESPOND_WITHIN_SECONDS,
    Request, Response, ServerFrame, SubscriptionEvent,
};
use tokio::{sync::watch, time::Instant};
use tokio_util::sync::CancellationToken;

use super::dialog_child::{DialogOutcome, ask_dialog_process, dialog_capability};

/// The hidden subcommand of the trusted consent dialog child.
const CONSENT_COMMAND: &str = "consent";

/// The first pause before subscribing again after a connection ended.
const RESUBSCRIBE_FIRST_DELAY: Duration = Duration::from_millis(250);
/// The longest pause between attempts to subscribe again.
const RESUBSCRIBE_MAX_DELAY: Duration = Duration::from_secs(30);
/// A subscription that lasted this long resets the backoff.
const STABLE_SUBSCRIPTION: Duration = Duration::from_secs(60);
/// How often the responder looks whether its Window finished reconnecting.
const WINDOW_RECONNECT_POLL: Duration = Duration::from_millis(250);
/// How long subscribing, or answering, may take.
const REQUEST_DEADLINE: Duration = Duration::from_secs(15);
/// Added to the dialog's own read bound, so the offer's deadline, which the
/// responder enforces itself, always ends an unanswered dialog first.
const DIALOG_BUDGET_SLACK: Duration = Duration::from_secs(5);

/// Where the responder's daemon connections come from.
pub(in crate::app) trait ResponderLink: Send + Sync + 'static {
    /// Opens one new protocol connection to the remote daemon.
    fn connect(&self) -> impl Future<Output = Result<Connection>> + Send;
    /// Whether the Window is reconnecting to its daemon now, as after a live
    /// upgrade; the responder subscribes again once it is done.
    fn window_reconnecting(&self) -> bool;
}

impl ResponderLink for ConnectionFactory {
    async fn connect(&self) -> Result<Connection> {
        Self::connect(self).await
    }

    fn window_reconnecting(&self) -> bool {
        matches!(
            self.handoff_status().view().phase,
            WindowHandoffPhase::Reconnecting | WindowHandoffPhase::Continuing
        )
    }
}

/// How the responder asks the person: the trusted consent dialog.
pub(in crate::app) trait ConsentDialog: Send + Sync + 'static {
    /// Shows `prompt` until it is answered, `budget` passes, or `cancel` is
    /// cancelled, and returns how it ended. A cancelled dialog is gone, and
    /// reaped, when this returns.
    fn ask(
        &self,
        prompt: ConsentPrompt,
        budget: Duration,
        cancel: CancellationToken,
    ) -> impl Future<Output = Result<DialogOutcome<ConsentReply>>> + Send;
}

/// The trusted dialog child, `/proc/self/exe consent`: the image this Window
/// runs, even after a viewer upgrade replaced the installed file.
struct ProcessDialog;

impl ConsentDialog for ProcessDialog {
    async fn ask(
        &self,
        prompt: ConsentPrompt,
        budget: Duration,
        cancel: CancellationToken,
    ) -> Result<DialogOutcome<ConsentReply>> {
        let mut command = Command::new("/proc/self/exe");
        command.arg(CONSENT_COMMAND);
        let capability = prompt.capability.clone();
        ask_dialog_process(
            command,
            prompt,
            capability,
            MAX_CONSENT_FRAME_BYTES,
            budget,
            cancel.cancelled_owned(),
        )
        .await
    }
}

/// The origin every prompt of this Window shows: the Window's own SSH
/// profile and the hostname its daemon reported, never anything an offer
/// says. `None` for a local endpoint, or when no valid remote origin can be
/// built; such a Window answers nothing.
fn responder_origin(consent: &RemoteConsent, hostname: Option<&str>) -> Option<ConsentOrigin> {
    let RemoteConsent::Enabled { profile } = consent else {
        return None;
    };
    let origin = ConsentOrigin::Remote {
        profile: profile.clone(),
        daemon_hostname: hostname.map(str::to_owned),
    };
    match origin.validate() {
        Ok(()) => Some(origin),
        Err(reason) => {
            eprintln!("splinterm: this Window cannot answer agent access prompts: {reason}");
            None
        }
    }
}

/// Starts a remote Window's agent access prompt responder, which runs until
/// `settings`' sender (the Window) is gone or the task is aborted. A local
/// endpoint never starts one, nor does a remote one without a valid origin.
pub(in crate::app) fn spawn_remote_consent_responder(
    factory: &ConnectionFactory,
    identity: Option<&RemoteDisplayIdentity>,
    settings: watch::Receiver<TopologyReloadSettings>,
) -> Option<tokio::task::JoinHandle<()>> {
    if factory.is_local() {
        return None;
    }
    let origin = responder_origin(
        &factory.capabilities().remote_consent,
        identity.and_then(RemoteDisplayIdentity::hostname),
    )?;
    Some(tokio::spawn(run_responder(
        factory.clone(),
        ProcessDialog,
        origin,
        settings,
    )))
}

/// A daemon that broke the responder contract. The connection is dropped
/// and the Window subscribes again on a fresh one.
#[derive(Debug)]
struct ProtocolFault(&'static str);

impl std::fmt::Display for ProtocolFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "protocol fault: {}", self.0)
    }
}

impl std::error::Error for ProtocolFault {}

fn fault(reason: &'static str) -> anyhow::Error {
    anyhow::Error::new(ProtocolFault(reason))
}

/// The pause before the attempt after `failures` failed ones.
fn resubscribe_delay(failures: u32) -> Duration {
    RESUBSCRIBE_FIRST_DELAY
        .saturating_mul(1_u32.checked_shl(failures.min(16)).unwrap_or(u32::MAX))
        .min(RESUBSCRIBE_MAX_DELAY)
}

/// Waits until the viewer allows remote prompts; `false` once the Window is
/// gone.
async fn until_enabled(settings: &mut watch::Receiver<TopologyReloadSettings>) -> bool {
    loop {
        if settings.borrow_and_update().remote_agent_prompts {
            return true;
        }
        if settings.changed().await.is_err() {
            return false;
        }
    }
}

/// Waits until the viewer turns remote prompts off (`false`) or the Window
/// is gone (`true`).
async fn until_disabled(settings: &mut watch::Receiver<TopologyReloadSettings>) -> bool {
    loop {
        if settings.changed().await.is_err() {
            return true;
        }
        if !settings.borrow_and_update().remote_agent_prompts {
            return false;
        }
    }
}

/// Waits `delay`, then for as long as the Window reconnects.
async fn pause<L: ResponderLink>(link: &L, delay: Duration) {
    tokio::time::sleep(delay).await;
    while link.window_reconnecting() {
        tokio::time::sleep(WINDOW_RECONNECT_POLL).await;
    }
}

/// The responder's life: one subscription at a time, again after each one
/// ends, while the viewer allows it and the Window lasts.
async fn run_responder<L: ResponderLink, D: ConsentDialog>(
    link: L,
    dialog: D,
    origin: ConsentOrigin,
    mut settings: watch::Receiver<TopologyReloadSettings>,
) {
    let mut failures = 0_u32;
    loop {
        if !until_enabled(&mut settings).await {
            return;
        }
        let started = Instant::now();
        let error = tokio::select! {
            ended = run_subscription(&link, &dialog, &origin) => match ended {
                Ok(never) => match never {},
                Err(error) => error,
            },
            closed = until_disabled(&mut settings) => {
                if closed {
                    return;
                }
                failures = 0;
                continue;
            }
        };
        if started.elapsed() >= STABLE_SUBSCRIPTION {
            failures = 0;
        }
        if failures == 0 || error.downcast_ref::<ProtocolFault>().is_some() {
            eprintln!("splinterm: remote agent prompts: subscribing again: {error:#}");
        }
        let delay = resubscribe_delay(failures);
        failures = failures.saturating_add(1);
        tokio::select! {
            () = pause(&link, delay) => {}
            closed = until_disabled(&mut settings) => {
                if closed {
                    return;
                }
                failures = 0;
            }
        }
    }
}

/// One subscription's events, as the responder contract allows them.
#[derive(Debug)]
enum OfferEvent {
    Offered(Box<ConsentOffer>),
    Withdrawn { offer_id: u64 },
}

/// What one subscription has seen.
#[derive(Debug)]
struct Subscription {
    id: u64,
    daemon_generation: u64,
    next_sequence: u64,
    /// The newest offer: offer IDs only grow.
    last_offer_id: u64,
    /// An offer that ended here unanswered, or with an answer that did not
    /// win, whose withdrawal may still come.
    awaiting_withdrawal: Option<u64>,
}

impl Subscription {
    const fn new(subscription_id: u64, daemon_generation: u64) -> Self {
        Self {
            id: subscription_id,
            daemon_generation,
            next_sequence: 1,
            last_offer_id: 0,
            awaiting_withdrawal: None,
        }
    }

    /// The offer event `frame` carries, in sequence, on this subscription.
    fn event(&mut self, frame: ServerFrame) -> Result<OfferEvent> {
        let ServerFrame::Event {
            subscription_id,
            sequence,
            event,
        } = frame
        else {
            return Err(fault("a frame other than an offer event"));
        };
        if subscription_id != self.id {
            return Err(fault("an event of another subscription"));
        }
        if sequence != self.next_sequence {
            return Err(fault("an event out of sequence"));
        }
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| fault("the event sequence ran out"))?;
        match event {
            SubscriptionEvent::ConsentOffered { offer } => Ok(OfferEvent::Offered(Box::new(offer))),
            SubscriptionEvent::ConsentOfferWithdrawn { offer_id, .. } => {
                Ok(OfferEvent::Withdrawn { offer_id })
            }
            _ => Err(fault("an event that is not about an offer")),
        }
    }

    /// Accepts a new offer while none shows: it is valid, asks for a
    /// workspace, counts down 1 to 120 seconds, comes from this connection's
    /// daemon generation, and is newer than every earlier one, which it ends.
    fn accept(&mut self, offer: &ConsentOffer) -> Result<()> {
        offer
            .validate()
            .map_err(|_| fault("an offer that fails validation"))?;
        if offer.daemon_generation != self.daemon_generation {
            return Err(fault("an offer from another daemon generation"));
        }
        if offer.offer_id <= self.last_offer_id {
            return Err(fault("an offer that is not newer than the last"));
        }
        self.last_offer_id = offer.offer_id;
        // A new offer means every earlier one is gone. Until this one is
        // answered and wins, its withdrawal may come.
        self.awaiting_withdrawal = Some(offer.offer_id);
        Ok(())
    }

    /// A withdrawal while no offer shows: only of the offer that ended here
    /// last, and only once.
    fn withdrawn(&mut self, offer_id: u64) -> Result<()> {
        if self.awaiting_withdrawal != Some(offer_id) {
            return Err(fault("a withdrawal of an offer that is not outstanding"));
        }
        self.awaiting_withdrawal = None;
        Ok(())
    }
}

/// The dialog prompt for `offer`: its content, a fresh random capability
/// that is never the offer's nonce, its countdown, and this Window's own
/// `origin`.
fn offer_prompt(offer: &ConsentOffer, origin: &ConsentOrigin) -> Result<ConsentPrompt> {
    let capability = dialog_capability()?;
    if capability.as_slice() == offer.nonce.as_bytes() {
        return Err(anyhow!("the dialog capability matched the offer nonce"));
    }
    let respond_within = offer
        .respond_within_seconds
        .clamp(1, MAX_CONSENT_RESPOND_WITHIN_SECONDS);
    let prompt = offer
        .prompt
        .clone()
        .into_prompt(capability, respond_within, origin.clone());
    prompt
        .validate()
        .map_err(|reason| anyhow!("the offer cannot be shown: {reason}"))?;
    Ok(prompt)
}

/// Subscribes on a new connection and answers its offers until the
/// connection ends or the daemon breaks the contract.
async fn run_subscription<L: ResponderLink, D: ConsentDialog>(
    link: &L,
    dialog: &D,
    origin: &ConsentOrigin,
) -> Result<Infallible> {
    let mut connection = link.connect().await?;
    let subscribed = connection
        .request_with_deadline(
            Request::SubscribeConsentOffers {
                kind: ConsentResponderKind::Window,
            },
            REQUEST_DEADLINE,
        )
        .await?;
    let Response::ConsentOffersSubscribed { subscription_id } = subscribed else {
        return Err(fault("an unexpected answer to subscribing"));
    };
    let mut subscription = Subscription::new(subscription_id, connection.daemon_generation());
    loop {
        let frame = connection.next_server_frame().await?;
        match subscription.event(frame)? {
            OfferEvent::Withdrawn { offer_id } => subscription.withdrawn(offer_id)?,
            OfferEvent::Offered(offer) => {
                let arrived = Instant::now();
                subscription.accept(&offer)?;
                let prompt = match offer_prompt(&offer, origin) {
                    Ok(prompt) => prompt,
                    Err(error) => {
                        // Skipped, never shown as anything else; the daemon
                        // withdraws it in time.
                        eprintln!("splinterm: remote agent prompt skipped: {error:#}");
                        continue;
                    }
                };
                show(
                    &mut connection,
                    &mut subscription,
                    dialog,
                    &offer,
                    prompt,
                    arrived,
                )
                .await?;
            }
        }
    }
}

/// Shows one offer's dialog and races its answer against the offer's
/// withdrawal, its deadline (counted from `arrived`), and the connection.
/// Only an answer is sent. An error ends the subscription; the dialog is
/// gone, and reaped, whenever this returns.
async fn show<D: ConsentDialog>(
    connection: &mut Connection,
    subscription: &mut Subscription,
    dialog: &D,
    offer: &ConsentOffer,
    prompt: ConsentPrompt,
    arrived: Instant,
) -> Result<()> {
    let deadline = arrived + Duration::from_secs(prompt.respond_within_seconds);
    let budget = deadline.saturating_duration_since(Instant::now()) + DIALOG_BUDGET_SLACK;
    let cancel = CancellationToken::new();
    let asked = dialog.ask(prompt, budget, cancel.clone());
    tokio::pin!(asked);
    // Why the dialog was cancelled: `Ok` ends this offer quietly, `Err` the
    // subscription.
    let mut ended: Option<Result<()>> = None;
    let outcome = loop {
        tokio::select! {
            // Once the deadline has passed or the offer is gone, nothing
            // the dialog answered is sent, even at the same moment.
            biased;
            () = tokio::time::sleep_until(deadline), if ended.is_none() => {
                cancel.cancel();
                ended = Some(Ok(()));
            }
            frame = connection.next_server_frame(), if ended.is_none() => {
                let end = match frame.and_then(|frame| subscription.event(frame)) {
                    Ok(OfferEvent::Withdrawn { offer_id }) if offer_id == offer.offer_id => {
                        subscription.awaiting_withdrawal = None;
                        Ok(())
                    }
                    Ok(OfferEvent::Withdrawn { .. }) => {
                        Err(fault("a withdrawal of another offer while one shows"))
                    }
                    Ok(OfferEvent::Offered(_)) => Err(fault("a second offer while one shows")),
                    Err(error) => Err(error),
                };
                cancel.cancel();
                ended = Some(end);
            }
            outcome = &mut asked => break outcome,
        }
    };
    if let Some(end) = ended {
        return end;
    }
    match outcome {
        Ok(DialogOutcome::Reply(reply)) => {
            answer(connection, subscription, offer, reply.decision).await
        }
        Ok(DialogOutcome::NoAnswer | DialogOutcome::Cancelled) => Ok(()),
        Err(error) => {
            // Nothing is sent; the daemon withdraws the offer in time.
            eprintln!("splinterm: remote agent prompt failed: {error:#}");
            Ok(())
        }
    }
}

/// Sends the person's `decision` for `offer`, echoing its generation, offer
/// ID, and nonce. An offer that is no longer outstanding (`NotFound`: it was
/// answered elsewhere, or ended) or a duration it does not offer closes it
/// without anything more.
async fn answer(
    connection: &mut Connection,
    subscription: &mut Subscription,
    offer: &ConsentOffer,
    decision: ConsentDecision,
) -> Result<()> {
    if let ConsentDecision::Grant { duration } = decision
        && !offer.prompt.offers(duration)
    {
        eprintln!("splinterm: the agent access dialog chose a duration the prompt does not offer");
        return Ok(());
    }
    let request = Request::AnswerConsentOffer {
        daemon_generation: offer.daemon_generation,
        offer_id: offer.offer_id,
        nonce: offer.nonce,
        decision,
    };
    match connection
        .request_with_deadline(request, REQUEST_DEADLINE)
        .await
    {
        Ok(Response::ConsentOfferAnswered) => {
            // The winner gets no withdrawal.
            subscription.awaiting_withdrawal = None;
            Ok(())
        }
        Ok(_) => Err(fault("an unexpected answer to answering")),
        Err(error) => match protocol_error(&error).map(|error| error.code) {
            Some(ErrorCode::NotFound | ErrorCode::HandoffInProgress) => Ok(()),
            Some(ErrorCode::InvalidArgument) => {
                eprintln!("splinterm: the daemon refused the agent access answer: {error:#}");
                Ok(())
            }
            _ => Err(error),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        pin::Pin,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    use splinterm_core::LairId;
    use splinterm_graphical_relay::{CONSENT_RESPONDER_CHANNELS, FIXED_WINDOW_SERVICE_CHANNELS};
    use splinterm_protocol::{
        AccessLevel, CONSENT_CAPABILITY_BYTES, CONSENT_OFFER_NONCE_BYTES, ClientFrame,
        ConsentDurationOffer, ConsentOfferEnd, ConsentOfferNonce, ConsentTarget, GrantDuration,
        OfferedConsentPrompt, PROTOCOL_VERSION, ProtocolError, ServerLimits, encode_frame,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf};

    use super::*;

    const GENERATION: u64 = 2;

    fn origin() -> ConsentOrigin {
        ConsentOrigin::Remote {
            profile: "work".to_owned(),
            daemon_hostname: Some("neuromancer".to_owned()),
        }
    }

    fn offered_prompt() -> OfferedConsentPrompt {
        OfferedConsentPrompt {
            requester: "/usr/bin/agent".to_owned(),
            requester_pid: 42,
            requester_uid: 1000,
            agent_name: Some("agent".to_owned()),
            target: ConsentTarget::Lair {
                lair_id: LairId::new(),
                lair_name: "api".to_owned(),
            },
            scopes: AccessLevel::Look.scopes().to_vec(),
            level: Some(AccessLevel::Look),
            durations: vec![ConsentDurationOffer {
                duration: GrantDuration::Once,
                withheld: None,
                shared_by: None,
                replaces: None,
            }],
        }
    }

    fn offer(offer_id: u64, respond_within_seconds: u64) -> ConsentOffer {
        ConsentOffer {
            daemon_generation: GENERATION,
            offer_id,
            request_id: 9,
            nonce: ConsentOfferNonce::new(
                [u8::try_from(offer_id % 200).unwrap() + 1; CONSENT_OFFER_NONCE_BYTES],
            ),
            respond_within_seconds,
            prompt: offered_prompt(),
        }
    }

    // ---- the fake daemon, over an in-memory transport ----

    /// The daemon end of one fake responder connection.
    struct FakeDaemon {
        reader: ReadHalf<DuplexStream>,
        writer: WriteHalf<DuplexStream>,
    }

    impl FakeDaemon {
        /// The next client frame, or `None` once the client closed.
        async fn next(&mut self) -> Option<ClientFrame> {
            let mut length = [0_u8; 4];
            self.reader.read_exact(&mut length).await.ok()?;
            let mut body = vec![0_u8; u32::from_be_bytes(length) as usize];
            self.reader.read_exact(&mut body).await.ok()?;
            Some(serde_json::from_slice(&body).unwrap())
        }

        /// The next request, or `None` once the client closed.
        async fn request(&mut self) -> Option<(u64, Request)> {
            match self.next().await? {
                ClientFrame::Request {
                    request_id,
                    request,
                    ..
                } => Some((request_id, request)),
                other => panic!("expected a request, got {other:?}"),
            }
        }

        /// The next request within `wait`; `None` if none came.
        async fn request_within(&mut self, wait: Duration) -> Option<Option<(u64, Request)>> {
            tokio::time::timeout(wait, self.request()).await.ok()
        }

        async fn send(&mut self, frame: &ServerFrame) {
            self.writer
                .write_all(&encode_frame(frame).unwrap())
                .await
                .unwrap();
        }

        async fn respond(&mut self, request_id: u64, result: Response) {
            self.send(&ServerFrame::Response { request_id, result })
                .await;
        }

        async fn refuse(&mut self, request_id: u64, code: ErrorCode) {
            self.send(&ServerFrame::Error {
                request_id: Some(request_id),
                error: ProtocolError::new(code, "fixture refusal"),
            })
            .await;
        }

        /// Answers the hello and the subscription.
        async fn subscribe(&mut self) {
            let Some(ClientFrame::Hello { .. }) = self.next().await else {
                panic!("expected the client hello");
            };
            self.send(&ServerFrame::Hello {
                version: PROTOCOL_VERSION,
                limits: ServerLimits::default(),
                development_terminal_access: false,
                daemon_hostname: Some("claimed-by-the-daemon".to_owned()),
                daemon_generation: GENERATION,
            })
            .await;
            let Some((
                request_id,
                Request::SubscribeConsentOffers {
                    kind: ConsentResponderKind::Window,
                },
            )) = self.request().await
            else {
                panic!("expected a Window consent subscription");
            };
            self.respond(
                request_id,
                Response::ConsentOffersSubscribed { subscription_id: 5 },
            )
            .await;
        }

        async fn event(&mut self, sequence: u64, event: SubscriptionEvent) {
            self.send(&ServerFrame::Event {
                subscription_id: 5,
                sequence,
                event,
            })
            .await;
        }

        async fn offer(&mut self, sequence: u64, offer: ConsentOffer) {
            self.event(sequence, SubscriptionEvent::ConsentOffered { offer })
                .await;
        }

        async fn withdraw(&mut self, sequence: u64, offer_id: u64) {
            self.event(
                sequence,
                SubscriptionEvent::ConsentOfferWithdrawn {
                    offer_id,
                    reason: ConsentOfferEnd::AnsweredElsewhere,
                },
            )
            .await;
        }

        /// Waits until the client closes, failing on any request meanwhile.
        async fn closed_without_requests(&mut self) {
            if let Some(frame) = self.next().await {
                panic!("expected the connection to close, got {frame:?}");
            }
        }
    }

    type Script = Box<dyn FnOnce(FakeDaemon) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

    /// Connections whose daemon ends run the scripts in order; with none
    /// left, connecting waits forever.
    #[derive(Clone, Default)]
    struct FakeLink {
        scripts: Arc<Mutex<VecDeque<Script>>>,
        /// How many scripts were added.
        scripted: Arc<AtomicUsize>,
        connects: Arc<AtomicUsize>,
        /// Set while a connection's daemon end is open.
        open: Arc<Mutex<Vec<Arc<AtomicBool>>>>,
        reconnecting: Arc<AtomicBool>,
        daemons: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    }

    impl FakeLink {
        fn script<F, Fut>(&self, script: F)
        where
            F: FnOnce(FakeDaemon) -> Fut + Send + 'static,
            Fut: Future<Output = ()> + Send + 'static,
        {
            self.scripts
                .lock()
                .unwrap()
                .push_back(Box::new(move |daemon| Box::pin(script(daemon))));
            self.scripted.fetch_add(1, Ordering::SeqCst);
        }

        fn connects(&self) -> usize {
            self.connects.load(Ordering::SeqCst)
        }

        async fn connected(&self, count: usize) {
            tokio::time::timeout(Duration::from_secs(20), async {
                while self.connects() < count {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("the responder did not connect in time");
        }

        /// Every scripted daemon connected and finished, so its assertions
        /// held.
        async fn finished(&self) {
            self.connected(self.scripted.load(Ordering::SeqCst)).await;
            let daemons = std::mem::take(&mut *self.daemons.lock().unwrap());
            for daemon in daemons {
                tokio::time::timeout(Duration::from_secs(20), daemon)
                    .await
                    .expect("a fake daemon did not finish in time")
                    .unwrap();
            }
        }
    }

    impl ResponderLink for FakeLink {
        async fn connect(&self) -> Result<Connection> {
            let script = self.scripts.lock().unwrap().pop_front();
            let Some(script) = script else {
                return std::future::pending().await;
            };
            // One responder connection at a time: every earlier one closed.
            assert!(
                self.open
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|open| !open.load(Ordering::SeqCst)),
                "the responder opened a second connection"
            );
            let open = Arc::new(AtomicBool::new(true));
            self.open.lock().unwrap().push(Arc::clone(&open));
            let (client, server) = tokio::io::duplex(1 << 20);
            let (reader, writer) = tokio::io::split(server);
            let daemon = tokio::spawn(async move {
                script(FakeDaemon { reader, writer }).await;
                open.store(false, Ordering::SeqCst);
            });
            self.daemons.lock().unwrap().push(daemon);
            self.connects.fetch_add(1, Ordering::SeqCst);
            let (reader, writer) = tokio::io::split(client);
            Connection::connect_remote_interactive_transport(reader, writer).await
        }

        fn window_reconnecting(&self) -> bool {
            self.reconnecting.load(Ordering::SeqCst)
        }
    }

    // ---- the fake dialog ----

    /// What one fake dialog does.
    #[derive(Clone, Copy, Debug)]
    enum Person {
        /// Answers after a pause.
        Answers(ConsentDecision, Duration),
        /// Never answers.
        Waits,
    }

    /// How one fake dialog ended.
    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Shown {
        Answered,
        Cancelled,
        /// Dropped while showing, as when the Window closes or the viewer
        /// turns remote prompts off: the real dialog's guard kills and
        /// reaps the child.
        Dropped,
    }

    #[derive(Clone, Default)]
    struct FakeDialog {
        people: Arc<Mutex<VecDeque<Person>>>,
        prompts: Arc<Mutex<Vec<ConsentPrompt>>>,
        ended: Arc<Mutex<Vec<Shown>>>,
    }

    /// Records a fake dialog dropped before it ended.
    struct Showing {
        dialog: FakeDialog,
        ended: bool,
    }

    impl Drop for Showing {
        fn drop(&mut self) {
            if !self.ended {
                self.dialog.end(Shown::Dropped);
            }
        }
    }

    impl FakeDialog {
        fn with(people: impl IntoIterator<Item = Person>) -> Self {
            let dialog = Self::default();
            dialog.people.lock().unwrap().extend(people);
            dialog
        }

        fn prompts(&self) -> Vec<ConsentPrompt> {
            self.prompts.lock().unwrap().clone()
        }

        fn ended(&self) -> Vec<Shown> {
            self.ended.lock().unwrap().clone()
        }

        async fn ended_count(&self, count: usize) {
            tokio::time::timeout(Duration::from_secs(20), async {
                while self.ended().len() < count {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("the dialog did not end in time");
        }

        async fn showing(&self) {
            tokio::time::timeout(Duration::from_secs(20), async {
                while self.prompts().len() <= self.ended().len() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("the dialog did not show in time");
        }

        fn end(&self, shown: Shown) {
            self.ended.lock().unwrap().push(shown);
        }
    }

    impl ConsentDialog for FakeDialog {
        async fn ask(
            &self,
            prompt: ConsentPrompt,
            _budget: Duration,
            cancel: CancellationToken,
        ) -> Result<DialogOutcome<ConsentReply>> {
            self.prompts.lock().unwrap().push(prompt.clone());
            let person = self.people.lock().unwrap().pop_front();
            let mut showing = Showing {
                dialog: self.clone(),
                ended: false,
            };
            let decision = match person.unwrap_or(Person::Waits) {
                Person::Answers(decision, pause) => {
                    tokio::select! {
                        () = tokio::time::sleep(pause) => Some(decision),
                        () = cancel.cancelled() => None,
                    }
                }
                Person::Waits => {
                    cancel.cancelled().await;
                    None
                }
            };
            showing.ended = true;
            let Some(decision) = decision else {
                self.end(Shown::Cancelled);
                return Ok(DialogOutcome::Cancelled);
            };
            self.end(Shown::Answered);
            Ok(DialogOutcome::Reply(ConsentReply {
                capability: prompt.capability,
                decision,
            }))
        }
    }

    fn settings(
        enabled: bool,
    ) -> (
        watch::Sender<TopologyReloadSettings>,
        watch::Receiver<TopologyReloadSettings>,
    ) {
        let config = splinterm::config::AppConfig {
            remote_agent_prompts: enabled,
            ..splinterm::config::AppConfig::default()
        };
        watch::channel(TopologyReloadSettings::from_config(&config))
    }

    /// Starts a responder on the fake link and dialog; it ends when the
    /// returned sender drops or the task is aborted.
    fn start(
        link: &FakeLink,
        dialog: &FakeDialog,
        enabled: bool,
    ) -> (
        watch::Sender<TopologyReloadSettings>,
        tokio::task::JoinHandle<()>,
    ) {
        let (sender, receiver) = settings(enabled);
        let task = tokio::spawn(run_responder(
            link.clone(),
            dialog.clone(),
            origin(),
            receiver,
        ));
        (sender, task)
    }

    const GRANT_ONCE: ConsentDecision = ConsentDecision::Grant {
        duration: GrantDuration::Once,
    };

    // ---- the tests ----

    /// The origin is the Window's own profile and its daemon's hostname;
    /// the offer has no say, and a local or invalid one answers nothing.
    #[test]
    fn the_origin_comes_from_the_endpoint_never_the_offer() {
        let enabled = RemoteConsent::Enabled {
            profile: "work".to_owned(),
        };
        assert_eq!(
            responder_origin(&enabled, Some("neuromancer")),
            Some(origin())
        );
        assert_eq!(
            responder_origin(&enabled, None),
            Some(ConsentOrigin::Remote {
                profile: "work".to_owned(),
                daemon_hostname: None,
            })
        );
        assert_eq!(responder_origin(&RemoteConsent::Disabled, Some("x")), None);
        for profile in ["", "-bad", "two words", "bidi\u{202e}"] {
            let invalid = RemoteConsent::Enabled {
                profile: profile.to_owned(),
            };
            assert_eq!(responder_origin(&invalid, None), None, "{profile:?}");
        }
        assert_eq!(responder_origin(&enabled, Some("bad host\u{202e}")), None);
        // Built from the offer, the prompt still carries only that origin.
        let prompt = offer_prompt(&offer(1, 30), &origin()).unwrap();
        assert_eq!(prompt.origin, origin());
        assert_ne!(prompt.origin, ConsentOrigin::Local);
    }

    /// A local endpoint never starts a responder.
    #[tokio::test]
    async fn a_local_endpoint_never_starts_a_responder() {
        let (_sender, receiver) = settings(true);
        assert!(
            spawn_remote_consent_responder(&ConnectionFactory::local(), None, receiver).is_none()
        );
        assert_eq!(
            ConnectionFactory::local().capabilities().remote_consent,
            RemoteConsent::Disabled
        );
    }

    #[test]
    fn every_prompt_gets_a_fresh_capability_that_is_never_the_nonce() {
        let offer = offer(1, 30);
        let first = offer_prompt(&offer, &origin()).unwrap();
        let second = offer_prompt(&offer, &origin()).unwrap();
        assert_eq!(first.capability.len(), CONSENT_CAPABILITY_BYTES);
        assert_ne!(first.capability.as_slice(), offer.nonce.as_bytes());
        assert_ne!(second.capability.as_slice(), offer.nonce.as_bytes());
        assert_ne!(first.capability, second.capability);
        assert_eq!(first.validate(), Ok(()));
    }

    /// The countdown is the offer's, 1 to 120 seconds; anything else is a
    /// protocol fault, as are an offer of another generation, an older
    /// offer, and one that asks for something other than a workspace.
    #[test]
    fn offers_are_validated_and_their_countdown_bounded() {
        for seconds in [1, 30, MAX_CONSENT_RESPOND_WITHIN_SECONDS] {
            let mut subscription = Subscription::new(5, GENERATION);
            let offer = offer(1, seconds);
            subscription.accept(&offer).unwrap();
            assert_eq!(
                offer_prompt(&offer, &origin())
                    .unwrap()
                    .respond_within_seconds,
                seconds
            );
        }
        let faults = |offer: ConsentOffer| {
            Subscription::new(5, GENERATION)
                .accept(&offer)
                .unwrap_err()
                .downcast_ref::<ProtocolFault>()
                .is_some()
        };
        assert!(faults(offer(1, 0)));
        assert!(faults(offer(1, MAX_CONSENT_RESPOND_WITHIN_SECONDS + 1)));
        let mut other_generation = offer(1, 30);
        other_generation.daemon_generation = GENERATION + 1;
        assert!(faults(other_generation));
        let mut splint = offer(1, 30);
        splint.prompt.target = ConsentTarget::Splint {
            splint_id: splinterm_core::SplintId::new(),
            incarnation: 1,
        };
        assert!(faults(splint));
        let mut zero = offer(1, 30);
        zero.offer_id = 0;
        assert!(faults(zero));

        let mut subscription = Subscription::new(5, GENERATION);
        subscription.accept(&offer(4, 30)).unwrap();
        assert!(subscription.accept(&offer(4, 30)).is_err());
        assert!(subscription.accept(&offer(3, 30)).is_err());
        // A withdrawal only of the offer that ended here, once.
        assert!(subscription.withdrawn(3).is_err());
        subscription.withdrawn(4).unwrap();
        assert!(subscription.withdrawn(4).is_err());
    }

    #[test]
    fn events_must_be_this_subscriptions_and_in_sequence() {
        let event = |subscription_id, sequence| ServerFrame::Event {
            subscription_id,
            sequence,
            event: SubscriptionEvent::ConsentOfferWithdrawn {
                offer_id: 1,
                reason: ConsentOfferEnd::Expired,
            },
        };
        let mut subscription = Subscription::new(5, GENERATION);
        assert!(subscription.event(event(5, 1)).is_ok());
        assert!(subscription.event(event(5, 3)).is_err());
        let mut subscription = Subscription::new(5, GENERATION);
        assert!(subscription.event(event(6, 1)).is_err());
        let mut subscription = Subscription::new(5, GENERATION);
        assert!(
            subscription
                .event(ServerFrame::Response {
                    request_id: 1,
                    result: Response::Acknowledged,
                })
                .is_err()
        );
    }

    #[test]
    fn the_backoff_grows_to_its_bound() {
        assert_eq!(resubscribe_delay(0), RESUBSCRIBE_FIRST_DELAY);
        assert_eq!(resubscribe_delay(1), RESUBSCRIBE_FIRST_DELAY * 2);
        assert!(resubscribe_delay(3) > resubscribe_delay(2));
        for failures in [7, 8, 20, u32::MAX] {
            assert_eq!(resubscribe_delay(failures), RESUBSCRIBE_MAX_DELAY);
        }
    }

    /// The relay counts the responder's one long-lived channel among the
    /// Window's fixed service channels, beside the eight the other services
    /// had, and the responder holds one connection at a time (the fake link
    /// asserts it on every connect).
    #[test]
    fn the_channel_budget_holds_the_responder() {
        assert_eq!(CONSENT_RESPONDER_CHANNELS, 1);
        assert_eq!(
            FIXED_WINDOW_SERVICE_CHANNELS,
            8 + CONSENT_RESPONDER_CHANNELS
        );
    }

    /// The answer echoes the offer's generation, ID, and nonce, with the
    /// person's decision, and the prompt shown is the offer's with this
    /// Window's origin and the offer's countdown.
    #[tokio::test]
    async fn the_answer_echoes_the_offer_its_nonce_and_generation() {
        let link = FakeLink::default();
        let shown = offer(7, 30);
        let expected = shown.clone();
        let expected_prompt = shown.prompt.clone();
        link.script(move |mut daemon| async move {
            daemon.subscribe().await;
            daemon.offer(1, shown).await;
            let Some((
                request_id,
                Request::AnswerConsentOffer {
                    daemon_generation,
                    offer_id,
                    nonce,
                    decision,
                },
            )) = daemon.request().await
            else {
                panic!("expected an answer");
            };
            assert_eq!(daemon_generation, expected.daemon_generation);
            assert_eq!(offer_id, expected.offer_id);
            assert_eq!(nonce, expected.nonce);
            assert_eq!(decision, GRANT_ONCE);
            daemon
                .respond(request_id, Response::ConsentOfferAnswered)
                .await;
            // The winner gets no withdrawal; the next offer shows.
            daemon.offer(2, offer(8, 30)).await;
            let Some((_, Request::AnswerConsentOffer { offer_id: 8, .. })) = daemon.request().await
            else {
                panic!("expected the second answer");
            };
        });
        let dialog = FakeDialog::with([
            Person::Answers(GRANT_ONCE, Duration::from_millis(50)),
            Person::Answers(ConsentDecision::Deny, Duration::ZERO),
        ]);
        let (_settings, task) = start(&link, &dialog, true);
        link.finished().await;
        let prompts = dialog.prompts();
        assert_eq!(prompts.len(), 2);
        assert_eq!(prompts[0].origin, origin());
        assert_eq!(prompts[0].respond_within_seconds, 30);
        assert_eq!(prompts[0].offered(), expected_prompt);
        task.abort();
    }

    /// A withdrawal ends the dialog and nothing is sent; a later offer on
    /// the same subscription still shows.
    #[tokio::test]
    async fn a_withdrawal_ends_the_dialog_and_sends_nothing() {
        let link = FakeLink::default();
        let dialog = FakeDialog::with([Person::Waits, Person::Answers(GRANT_ONCE, Duration::ZERO)]);
        let watched = dialog.clone();
        link.script(move |mut daemon| async move {
            daemon.subscribe().await;
            daemon.offer(1, offer(3, 60)).await;
            watched.showing().await;
            daemon.withdraw(2, 3).await;
            watched.ended_count(1).await;
            assert_eq!(watched.ended(), vec![Shown::Cancelled]);
            // Nothing for the withdrawn offer.
            assert!(
                daemon
                    .request_within(Duration::from_millis(300))
                    .await
                    .is_none()
            );
            daemon.offer(3, offer(4, 60)).await;
            let Some((request_id, Request::AnswerConsentOffer { offer_id: 4, .. })) =
                daemon.request().await
            else {
                panic!("expected the answer to the next offer");
            };
            daemon
                .respond(request_id, Response::ConsentOfferAnswered)
                .await;
        });
        let (_settings, task) = start(&link, &dialog, true);
        link.finished().await;
        assert_eq!(
            link.connects(),
            1,
            "a withdrawal is no reason to resubscribe"
        );
        task.abort();
    }

    /// Once the offer's countdown runs out, the dialog goes and nothing is
    /// sent, even by a person who answers a moment later.
    #[tokio::test]
    async fn nothing_is_sent_once_the_deadline_passes() {
        let link = FakeLink::default();
        let dialog = FakeDialog::with([
            Person::Waits,
            Person::Answers(GRANT_ONCE, Duration::from_millis(1_500)),
        ]);
        let watched = dialog.clone();
        link.script(move |mut daemon| async move {
            daemon.subscribe().await;
            daemon.offer(1, offer(1, 1)).await;
            watched.ended_count(1).await;
            daemon.withdraw(2, 1).await;
            daemon.offer(3, offer(2, 1)).await;
            watched.ended_count(2).await;
            assert_eq!(watched.ended(), vec![Shown::Cancelled, Shown::Cancelled]);
            assert!(
                daemon
                    .request_within(Duration::from_millis(800))
                    .await
                    .is_none()
            );
        });
        let started = Instant::now();
        let (_settings, task) = start(&link, &dialog, true);
        link.finished().await;
        assert!(started.elapsed() >= Duration::from_secs(2));
        task.abort();
    }

    /// A second offer while one shows is a protocol fault: the dialog goes,
    /// nothing is sent, the connection closes, and the Window subscribes
    /// again on a new one.
    #[tokio::test]
    async fn a_second_offer_while_one_shows_resubscribes() {
        let link = FakeLink::default();
        let dialog = FakeDialog::with([Person::Waits]);
        let watched = dialog.clone();
        link.script(move |mut daemon| async move {
            daemon.subscribe().await;
            daemon.offer(1, offer(1, 60)).await;
            watched.showing().await;
            daemon.offer(2, offer(2, 60)).await;
            daemon.closed_without_requests().await;
            assert_eq!(watched.ended(), vec![Shown::Cancelled]);
        });
        link.script(|mut daemon| async move {
            daemon.subscribe().await;
        });
        let (_settings, task) = start(&link, &dialog, true);
        link.connected(2).await;
        link.finished().await;
        assert_eq!(dialog.prompts().len(), 1);
        task.abort();
    }

    /// An invalid offer is a protocol fault: nothing shows and the Window
    /// subscribes again; so is a withdrawal of an offer it never had.
    #[tokio::test]
    async fn an_invalid_offer_or_withdrawal_resubscribes() {
        let link = FakeLink::default();
        let dialog = FakeDialog::default();
        link.script(|mut daemon| async move {
            daemon.subscribe().await;
            let mut splint = offer(1, 30);
            splint.prompt.target = ConsentTarget::Splint {
                splint_id: splinterm_core::SplintId::new(),
                incarnation: 1,
            };
            daemon.offer(1, splint).await;
            daemon.closed_without_requests().await;
        });
        link.script(|mut daemon| async move {
            daemon.subscribe().await;
            daemon.offer(1, offer(1, 0)).await;
            daemon.closed_without_requests().await;
        });
        link.script(|mut daemon| async move {
            daemon.subscribe().await;
            daemon.withdraw(1, 9).await;
            daemon.closed_without_requests().await;
        });
        link.script(|mut daemon| async move {
            daemon.subscribe().await;
        });
        let (_settings, task) = start(&link, &dialog, true);
        link.connected(4).await;
        link.finished().await;
        assert!(dialog.prompts().is_empty());
        task.abort();
    }

    /// An answer the daemon no longer wants (`NotFound`, answered
    /// elsewhere) or refuses (`InvalidArgument`) closes the offer quietly:
    /// its withdrawal is accepted afterwards and the subscription goes on.
    #[tokio::test]
    async fn a_refused_answer_closes_the_offer_and_keeps_the_subscription() {
        let link = FakeLink::default();
        let dialog = FakeDialog::with([
            Person::Answers(GRANT_ONCE, Duration::ZERO),
            Person::Answers(ConsentDecision::Deny, Duration::ZERO),
            Person::Answers(GRANT_ONCE, Duration::ZERO),
        ]);
        link.script(|mut daemon| async move {
            daemon.subscribe().await;
            daemon.offer(1, offer(1, 30)).await;
            let Some((request_id, Request::AnswerConsentOffer { offer_id: 1, .. })) =
                daemon.request().await
            else {
                panic!("expected an answer");
            };
            // Answered elsewhere: its withdrawal was already on its way.
            daemon.withdraw(2, 1).await;
            daemon.refuse(request_id, ErrorCode::NotFound).await;
            daemon.offer(3, offer(2, 30)).await;
            let Some((request_id, Request::AnswerConsentOffer { offer_id: 2, .. })) =
                daemon.request().await
            else {
                panic!("expected the second answer");
            };
            daemon.refuse(request_id, ErrorCode::InvalidArgument).await;
            daemon.withdraw(4, 2).await;
            daemon.offer(5, offer(3, 30)).await;
            let Some((request_id, Request::AnswerConsentOffer { offer_id: 3, .. })) =
                daemon.request().await
            else {
                panic!("expected the third answer");
            };
            daemon
                .respond(request_id, Response::ConsentOfferAnswered)
                .await;
        });
        let (_settings, task) = start(&link, &dialog, true);
        link.finished().await;
        assert_eq!(link.connects(), 1);
        task.abort();
    }

    /// A dialog that picks a duration the prompt does not offer sends
    /// nothing.
    #[tokio::test]
    async fn an_answer_with_an_unoffered_duration_is_never_sent() {
        let link = FakeLink::default();
        let dialog = FakeDialog::with([Person::Answers(
            ConsentDecision::Grant {
                duration: GrantDuration::Remembered,
            },
            Duration::ZERO,
        )]);
        let watched = dialog.clone();
        link.script(move |mut daemon| async move {
            daemon.subscribe().await;
            daemon.offer(1, offer(1, 30)).await;
            watched.ended_count(1).await;
            assert!(
                daemon
                    .request_within(Duration::from_millis(500))
                    .await
                    .is_none()
            );
        });
        let (_settings, task) = start(&link, &dialog, true);
        link.finished().await;
        task.abort();
    }

    /// A lost connection, as a live upgrade of the remote daemon ends every
    /// channel, ends the dialog without an answer, and the Window subscribes
    /// again, after its own reconnection is done.
    #[tokio::test]
    async fn the_responder_subscribes_again_after_its_connection_ends() {
        let link = FakeLink::default();
        let dialog = FakeDialog::with([Person::Waits]);
        let watched = dialog.clone();
        let reconnecting = Arc::clone(&link.reconnecting);
        link.script(move |mut daemon| async move {
            daemon.subscribe().await;
            daemon.offer(1, offer(1, 60)).await;
            watched.showing().await;
            // The Window reconnects too, and the daemon's channel ends.
            reconnecting.store(true, Ordering::SeqCst);
        });
        link.script(|mut daemon| async move {
            daemon.subscribe().await;
        });
        let (_settings, task) = start(&link, &dialog, true);
        dialog.ended_count(1).await;
        assert_eq!(dialog.ended(), vec![Shown::Cancelled]);
        // It waits while the Window reconnects.
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert_eq!(link.connects(), 1);
        link.reconnecting.store(false, Ordering::SeqCst);
        link.connected(2).await;
        link.finished().await;
        task.abort();
    }

    /// `[integration] remote-agent-prompts = no` keeps the responder from
    /// subscribing; a reload to yes starts it, and back to no ends its
    /// subscription and any dialog it shows.
    #[tokio::test]
    async fn the_opt_out_key_stops_and_starts_the_responder() {
        let link = FakeLink::default();
        let dialog = FakeDialog::with([Person::Waits]);
        let watched = dialog.clone();
        link.script(move |mut daemon| async move {
            daemon.subscribe().await;
            daemon.offer(1, offer(1, 60)).await;
            daemon.closed_without_requests().await;
            watched.ended_count(1).await;
            assert_eq!(watched.ended(), vec![Shown::Dropped]);
        });
        let (sender, task) = start(&link, &dialog, false);
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(link.connects(), 0, "turned off, it never subscribes");
        let on = |enabled| {
            let config = splinterm::config::AppConfig {
                remote_agent_prompts: enabled,
                ..splinterm::config::AppConfig::default()
            };
            TopologyReloadSettings::from_config(&config)
        };
        // A reload that keeps it off changes nothing.
        sender.send_replace(on(false));
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(link.connects(), 0);
        sender.send_replace(on(true));
        link.connected(1).await;
        dialog.showing().await;
        sender.send_replace(on(false));
        link.finished().await;
        // The Window closing ends the responder.
        drop(sender);
        tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .expect("the responder outlived its Window")
            .unwrap();
    }

    /// The Window closing ends the responder and its connection.
    #[tokio::test]
    async fn the_responder_ends_with_its_window() {
        let link = FakeLink::default();
        let dialog = FakeDialog::default();
        link.script(|mut daemon| async move {
            daemon.subscribe().await;
            daemon.closed_without_requests().await;
        });
        let (sender, task) = start(&link, &dialog, true);
        link.connected(1).await;
        drop(sender);
        tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .expect("the responder outlived its Window")
            .unwrap();
        link.finished().await;
    }

    /// End to end over the remote transport a Window uses: a fake OpenSSH
    /// child speaking the graphical relay framing to a fixture daemon,
    /// which offers a prompt as the daemon does. The scripted dialog shows
    /// it with this Window's own origin, and its answer reaches the daemon
    /// echoing the offer's generation, ID, and nonce.
    #[tokio::test]
    async fn an_offer_through_the_remote_session_is_answered_end_to_end() {
        use splinterm::{remote::RemoteCatalog, remote_session::RemoteSession};

        // The fixture daemon is generation 1 and names the subscription 5.
        let mut shown = offer(7, 30);
        shown.daemon_generation = 1;
        let directory = fake_ssh_offering(&shown);
        let ssh = directory.join("ssh");

        let catalog = RemoteCatalog::parse(
            "version = 1\n[remotes.loop]\nhost = \"ssh-alias.invalid\"\n",
            None,
        )
        .unwrap();
        let session =
            RemoteSession::connect_with_program(catalog.get("loop").unwrap(), ssh.as_os_str())
                .await
                .unwrap();
        let factory = ConnectionFactory::from_remote_session("loop", session);
        let first = factory.connect().await.unwrap();
        let identity = factory.remote_display_identity(&first);
        drop(first);
        let origin = responder_origin(
            &factory.capabilities().remote_consent,
            identity.as_ref().and_then(RemoteDisplayIdentity::hostname),
        )
        .unwrap();
        assert_eq!(
            origin,
            ConsentOrigin::Remote {
                profile: "loop".to_owned(),
                daemon_hostname: Some("remote-box".to_owned()),
            }
        );
        let dialog = FakeDialog::with([Person::Answers(GRANT_ONCE, Duration::ZERO)]);
        let (settings, receiver) = settings(true);
        let task = tokio::spawn(run_responder(
            factory.clone(),
            dialog.clone(),
            origin.clone(),
            receiver,
        ));

        assert_eq!(
            answer_received(&directory.join("requests.jsonl")).await,
            Request::AnswerConsentOffer {
                daemon_generation: 1,
                offer_id: 7,
                nonce: shown.nonce,
                decision: GRANT_ONCE,
            }
        );
        let prompts = dialog.prompts();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].origin, origin);
        assert_eq!(prompts[0].offered(), shown.prompt);
        assert_ne!(prompts[0].capability.as_slice(), shown.nonce.as_bytes());

        drop(settings);
        tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .expect("the responder outlived its Window")
            .unwrap();
        drop(factory);
        tokio::time::sleep(Duration::from_millis(400)).await;
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// A fresh directory holding the fake OpenSSH program, as `ssh`, whose
    /// daemon calls itself `remote-box` and offers `shown` on subscription 5.
    fn fake_ssh_offering(shown: &ConsentOffer) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        const FAKE_SSH: &str = include_str!("../../tests/fixtures/fake_ssh.py");

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "splinterm-remote-consent-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&directory).unwrap();
        let ssh = directory.join("ssh");
        std::fs::write(
            &ssh,
            FAKE_SSH.replace(
                "'development_terminal_access': False",
                "'development_terminal_access': False, 'daemon_hostname': 'remote-box'",
            ),
        )
        .unwrap();
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(
            directory.join("consent-offer.json"),
            serde_json::to_vec(&ServerFrame::Event {
                subscription_id: 5,
                sequence: 1,
                event: SubscriptionEvent::ConsentOffered {
                    offer: shown.clone(),
                },
            })
            .unwrap(),
        )
        .unwrap();
        directory
    }

    /// The first answer the fixture daemon logged in `requests`.
    async fn answer_received(requests: &std::path::Path) -> Request {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let answered = std::fs::read_to_string(requests)
                    .unwrap_or_default()
                    .lines()
                    .map(|line| serde_json::from_str::<Request>(line).unwrap())
                    .find(|request| matches!(request, Request::AnswerConsentOffer { .. }));
                if let Some(answer) = answered {
                    break answer;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the answer never reached the daemon")
    }
}
