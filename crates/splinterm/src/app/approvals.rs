//! `splinterm approvals`: the terminal approver (ADR 0015).
//!
//! The person runs it in their own terminal on the daemon's computer, for
//! example in an SSH login to a headless host. It connects to this
//! computer's daemon as the installed client, subscribes as a `Terminal`
//! consent responder, and shows each workspace access prompt it is offered
//! as text, with the same request, host row, and escaping as the dialog
//! (`consent::consent_request_lines`).
//!
//! It answers only on a key read from its terminal. It refuses to start
//! without a terminal on standard input and output that is its controlling
//! terminal, and has no flag that answers by itself. Each prompt starts on
//! **Deny**; the keys typed before it appeared are thrown away, every key is
//! ignored for [`ARMING_DELAY`] after it appears, and pasted text is ignored,
//! so type-ahead cannot answer it. Moving to an Allow choice arms Enter only
//! after [`SELECTION_ARMING_DELAY`], as choosing a duration rearms the
//! dialog. Escape, `q`, and Ctrl+C deny. Once the countdown runs out nothing
//! is sent.
//!
//! The daemon refuses the approver inside a Splint, inside the requester's
//! process tree or session, and, with workload units, inside a Splint scope
//! or Dojo slice; it also never offers a request to an approver inside that
//! requester's tree or session. These checks do not stop a same-user program
//! that sends keys to the terminal (a multiplexer's or terminal's remote
//! control, or a write to the pseudo-terminal), nor one that starts the
//! approver outside every tree: ADR 0015's residual risks.
//!
//! Rendering, key decoding and the prompt's state are pure, so they are
//! tested without a terminal: [`Approver`] turns offers, withdrawals, keys,
//! and time into [`Effect`]s, and [`Session`] applies them to a [`Screen`].

use std::{
    io::{self, Write as _},
    os::fd::{AsFd, BorrowedFd},
    sync::{
        Arc, Mutex, Once,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    fs::FileType,
    termios::{InputModes, LocalModes, OptionalActions, QueueSelector, SpecialCodeIndex, Termios},
};
use splinterm::{
    DialogLine, DialogLineRole, DialogStyle,
    automation::{
        Connection, ReconnectBackoff, is_connection_loss, protocol_error, request_cancellation,
    },
    frontend::{
        dialog_text::{TITLE_MARK, wrap_line},
        shown, unsafe_display_character,
    },
};
use splinterm_protocol::{
    CONSENT_APPROVER_NEEDS_TERMINAL, CONSENT_RESPONDER_IDENTITY_CHANGED, ConsentDecision,
    ConsentOffer, ConsentOfferEnd, ConsentOfferNonce, ConsentOrigin, ConsentPrompt,
    ConsentResponderKind, ErrorCode, GrantDuration, MAX_DAEMON_HOSTNAME_BYTES, ProtocolError,
    Request, Response, ServerFrame, SubscriptionEvent, TRUSTED_UI_REQUIRES_INSTALLED_CLIENT,
};
use tokio::{
    signal::unix::{Signal, SignalKind, signal},
    sync::mpsc,
};

use super::consent::{
    ChosenLines, access_label, button_label, chosen_lines, consent_request_lines, display_name,
    duration_row, local_hostname, prompt_line, workspace_label,
};

/// How long every key is ignored after a prompt appears. A prompt appears
/// unbidden in a terminal the person may be typing in, with no click to
/// engage it as the dialog has, so this is longer than the dialog's delay.
const ARMING_DELAY: Duration = Duration::from_secs(1);

/// How long Enter is ignored after the selection moves: the dialog's arming
/// delay, which choosing a duration restarts there too.
const SELECTION_ARMING_DELAY: Duration = Duration::from_millis(600);

/// How often the countdown and the terminal's size are checked.
const TICK: Duration = Duration::from_millis(200);

/// The longest connecting, the handshake, and registering the approver take
/// together before it backs off and tries again.
const CONNECT_DEADLINE: Duration = Duration::from_secs(10);

/// The longest the daemon gets to settle an answer.
const ANSWER_DEADLINE: Duration = Duration::from_secs(10);

/// The longest daemon message shown, in characters.
const MAX_REASON_CHARS: usize = 200;

/// The narrowest the prompt is wrapped, whatever the terminal reports.
const MIN_COLUMNS: usize = 20;

/// Where an approver must run, as its refusals say.
const OWN_TERMINAL: &str = "splinterd refuses an approver inside a Splint, inside the requesting agent's process tree or session, and, when workloads run in systemd units, inside a Splint scope or Dojo slice, where a tmux or screen session started from a Splinterm pane stays. Run `splinterm approvals` directly in your own terminal on this computer, such as an SSH login, not inside a Splinterm pane or a tmux or screen session started from one.";

/// The warning printed when the approver runs inside a terminal
/// multiplexer, whose clients can send it keys.
const MULTIPLEXER_WARNING: &str = "Running inside tmux/screen: any program you run can send keys to this approver. Prefer a plain SSH login or terminal.";

/// Printed when a reconnect's handshake is refused because this client's
/// file was replaced, typically by a package upgrade.
const REPLACED: &str = "This splinterm was replaced since it started, for example by a package upgrade. Run `splinterm approvals` again.";

/// Too many responders are connected.
const TOO_MANY: &str = "Too many approvers or remote Windows are connected to splinterd already. Close one, then run `splinterm approvals` again.";

// ---------------------------------------------------------------------------
// The terminal it runs in.

/// What `splinterm approvals` needs to know about its terminal before it
/// connects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TerminalFacts {
    pub(super) stdin_terminal: bool,
    pub(super) stdout_terminal: bool,
    /// The device number of the session's controlling terminal, from
    /// `tty_nr` in `/proc/self/stat` as splinterd reads it; `None` without
    /// one.
    pub(super) controlling: Option<u64>,
    /// The character device standard input is, if it is one.
    pub(super) stdin_device: Option<u64>,
    /// The character device standard output is, if it is one.
    pub(super) stdout_device: Option<u64>,
    /// Whether this process's group is the terminal's foreground group.
    pub(super) foreground: bool,
}

impl TerminalFacts {
    fn read() -> Self {
        let stdin = io::stdin();
        let stdout = io::stdout();
        Self {
            stdin_terminal: rustix::termios::isatty(&stdin),
            stdout_terminal: rustix::termios::isatty(&stdout),
            controlling: std::fs::read("/proc/self/stat")
                .ok()
                .and_then(|stat| controlling_terminal(&stat)),
            stdin_device: character_device(stdin.as_fd()),
            stdout_device: character_device(stdout.as_fd()),
            foreground: rustix::termios::tcgetpgrp(&stdin)
                .is_ok_and(|group| group == rustix::process::getpgrp()),
        }
    }
}

/// The controlling terminal's device number from `/proc/<pid>/stat`
/// contents: the `tty_nr` field after the command name's final `)`, in the
/// kernel's encoding, 0 for none. Opening `/dev/tty` would not do: its
/// `fstat` reports the `/dev/tty` alias itself, never the terminal behind it.
fn controlling_terminal(stat: &[u8]) -> Option<u64> {
    let close = stat.iter().rposition(|&byte| byte == b')')?;
    let rest = std::str::from_utf8(&stat[close + 1..]).ok()?;
    // state, ppid, pgrp, session, tty_nr
    let field = rest.split_ascii_whitespace().nth(4)?;
    let terminal = u32::try_from(field.parse::<i32>().ok()?).ok()?;
    if terminal == 0 {
        return None;
    }
    let major = (terminal >> 8) & 0xfff;
    let minor = (terminal & 0xff) | ((terminal >> 12) & 0xf_ff00);
    Some(rustix::fs::makedev(major, minor))
}

fn character_device(descriptor: BorrowedFd<'_>) -> Option<u64> {
    let stat = rustix::fs::fstat(descriptor).ok()?;
    (FileType::from_raw_mode(stat.st_mode) == FileType::CharacterDevice).then_some(stat.st_rdev)
}

/// Why `splinterm approvals` will not start in this terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TerminalRefusal {
    /// Standard input or output is not a terminal.
    NotATerminal,
    /// The process has no controlling terminal.
    NoControllingTerminal,
    /// Standard input and output are not both the controlling terminal.
    OtherTerminal,
    /// It runs in the background of its terminal.
    Background,
}

impl TerminalRefusal {
    pub(super) const fn message(self) -> &'static str {
        match self {
            Self::NotATerminal => {
                "splinterm approvals answers only from a terminal: run it in a terminal, with standard input and output on that terminal and not redirected"
            }
            Self::NoControllingTerminal => {
                "splinterm approvals needs a controlling terminal: run it in a terminal you type in, such as an SSH login"
            }
            Self::OtherTerminal => {
                "splinterm approvals needs standard input and output on its own controlling terminal, not redirected to another one"
            }
            Self::Background => "splinterm approvals must run in the foreground of its terminal",
        }
    }
}

/// Whether the approver may start in a terminal with these facts: standard
/// input and output are the process's controlling terminal, and it runs in
/// that terminal's foreground.
pub(super) fn check_terminal(facts: &TerminalFacts) -> Result<(), TerminalRefusal> {
    if !facts.stdin_terminal || !facts.stdout_terminal {
        return Err(TerminalRefusal::NotATerminal);
    }
    let Some(controlling) = facts.controlling else {
        return Err(TerminalRefusal::NoControllingTerminal);
    };
    if facts.stdin_device != Some(controlling) || facts.stdout_device != Some(controlling) {
        return Err(TerminalRefusal::OtherTerminal);
    }
    if !facts.foreground {
        return Err(TerminalRefusal::Background);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Keys.

/// The keys the approver tells apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Key {
    Up,
    Down,
    Enter,
    Escape,
    /// `q`.
    Quit,
    /// Ctrl+C.
    Interrupt,
    Other,
}

/// What a terminal in bracketed paste mode sends before and after pasted
/// text.
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// Turns raw terminal input into keys, read by read. Pasted text, between
/// [`PASTE_START`] and [`PASTE_END`], is no key at all, so a paste can never
/// answer; a marker cut in two by a read is still recognised.
#[derive(Debug, Default)]
pub(super) struct KeyDecoder {
    pasting: bool,
    /// The end of the last read, kept when it may be the start of a marker.
    carry: Vec<u8>,
}

impl KeyDecoder {
    pub(super) fn decode(&mut self, bytes: &[u8]) -> Vec<Key> {
        let mut data = std::mem::take(&mut self.carry);
        data.extend_from_slice(bytes);
        let mut keys = Vec::new();
        let mut index = 0;
        while index < data.len() {
            let rest = &data[index..];
            if self.pasting {
                if let Some(at) = rest
                    .windows(PASTE_END.len())
                    .position(|window| window == PASTE_END)
                {
                    index += at + PASTE_END.len();
                    self.pasting = false;
                    continue;
                }
                // The end marker may be cut by this read; the rest is pasted.
                let keep = rest.len().min(PASTE_END.len() - 1);
                self.carry = rest[rest.len() - keep..].to_vec();
                return keys;
            }
            if rest.starts_with(PASTE_START) {
                index += PASTE_START.len();
                self.pasting = true;
                continue;
            }
            // A start marker cut by this read. A lone ESC, or ESC [, is
            // never kept back, so Escape still answers at once.
            if rest.len() >= 3 && rest.len() < PASTE_START.len() && PASTE_START.starts_with(rest) {
                self.carry = rest.to_vec();
                return keys;
            }
            let (key, next) = decode_key(&data, index);
            keys.push(key);
            index = next;
        }
        keys
    }
}

/// The keys in one read of raw terminal input, as a fresh [`KeyDecoder`]
/// reads them.
#[cfg(test)]
pub(super) fn decode_keys(bytes: &[u8]) -> Vec<Key> {
    KeyDecoder::default().decode(bytes)
}

/// The key at `index` of `bytes`, and where the next one starts. An arrow is
/// `ESC [ A` or `ESC O A` (and `B`); any other escape sequence is one
/// [`Key::Other`], and a lone ESC is [`Key::Escape`]. An ESC whose sequence
/// was cut by the read reads as Escape, which only ever denies.
fn decode_key(bytes: &[u8], index: usize) -> (Key, usize) {
    let mut index = index + 1;
    let key = match bytes[index - 1] {
        0x1b => match bytes.get(index) {
            None => Key::Escape,
            Some(b'[') => {
                let start = index + 1;
                let end = bytes[start..]
                    .iter()
                    .position(|byte| (0x40..=0x7e).contains(byte))
                    .map_or(bytes.len(), |offset| start + offset);
                index = (end + 1).min(bytes.len());
                match (bytes.get(end), end == start) {
                    (Some(b'A'), true) => Key::Up,
                    (Some(b'B'), true) => Key::Down,
                    _ => Key::Other,
                }
            }
            Some(b'O') => {
                let final_byte = bytes.get(index + 1).copied();
                index = (index + 2).min(bytes.len());
                match final_byte {
                    Some(b'A') => Key::Up,
                    Some(b'B') => Key::Down,
                    _ => Key::Other,
                }
            }
            // Alt with a key.
            Some(_) => {
                index += 1;
                Key::Other
            }
        },
        b'\r' | b'\n' => Key::Enter,
        b'k' => Key::Up,
        b'j' => Key::Down,
        b'q' | b'Q' => Key::Quit,
        0x03 => Key::Interrupt,
        _ => Key::Other,
    };
    (key, index)
}

// ---------------------------------------------------------------------------
// The prompt.

/// One answer the person can choose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Choice {
    Deny,
    Grant(GrantDuration),
}

/// The answer to send for one offer: its generation, id and nonce, echoed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Answer {
    pub(super) daemon_generation: u64,
    pub(super) offer_id: u64,
    pub(super) nonce: ConsentOfferNonce,
    pub(super) decision: ConsentDecision,
}

impl Answer {
    const fn request(self) -> Request {
        Request::AnswerConsentOffer {
            daemon_generation: self.daemon_generation,
            offer_id: self.offer_id,
            nonce: self.nonce,
            decision: self.decision,
        }
    }
}

/// The prompt on screen.
#[derive(Debug, Clone)]
struct OpenPrompt {
    daemon_generation: u64,
    offer_id: u64,
    nonce: ConsentOfferNonce,
    /// The offer's prompt as the dialog would show it from this computer.
    prompt: ConsentPrompt,
    /// Deny, then every offered duration in order.
    choices: Vec<Choice>,
    selected: usize,
    /// No key counts before this.
    armed_at: Instant,
    /// Enter on an Allow choice counts only from this.
    grant_armed_at: Instant,
    /// The time to answer ends here; from then on nothing is sent.
    deadline: Instant,
    /// A note from the daemon about an earlier answer.
    notice: Option<&'static str>,
}

impl OpenPrompt {
    fn new(offer: ConsentOffer, now: Instant) -> Self {
        let choices = std::iter::once(Choice::Deny)
            .chain(
                offer
                    .prompt
                    .durations
                    .iter()
                    .filter(|duration| duration.withheld.is_none())
                    .map(|duration| Choice::Grant(duration.duration)),
            )
            .collect();
        let armed_at = now + ARMING_DELAY;
        Self {
            daemon_generation: offer.daemon_generation,
            offer_id: offer.offer_id,
            nonce: offer.nonce,
            // The approver runs on the daemon's computer, so the request
            // comes from this computer. The capability is the dialog
            // child's own and plays no part here.
            prompt: offer.prompt.into_prompt(
                Vec::new(),
                offer.respond_within_seconds,
                ConsentOrigin::Local,
            ),
            choices,
            selected: 0,
            armed_at,
            grant_armed_at: armed_at,
            deadline: now + Duration::from_secs(offer.respond_within_seconds),
            notice: None,
        }
    }

    fn rearm(&mut self, now: Instant) {
        self.armed_at = now + ARMING_DELAY;
        self.grant_armed_at = self.armed_at;
    }

    fn answer(&self, decision: ConsentDecision) -> Answer {
        Answer {
            daemon_generation: self.daemon_generation,
            offer_id: self.offer_id,
            nonce: self.nonce,
            decision,
        }
    }

    fn workspace(&self) -> String {
        workspace_label(&self.prompt.target)
    }
}

/// What the terminal does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Effect {
    /// A new prompt: throw away everything typed so far, then draw it.
    Open,
    /// Draw the prompt again.
    Redraw,
    /// Take the prompt off the screen, if one is shown, and print a line.
    Print(String),
    /// Take the prompt off the screen and send this answer.
    Answer(Answer),
    /// End `splinterm approvals`.
    Quit,
}

/// How an answer ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AnswerResult {
    /// It settled the request.
    Answered,
    /// The offer was gone: another route answered, or the request ended.
    NotFound,
    /// The daemon did not accept the duration; the offer is still open.
    InvalidArgument,
    /// The daemon did not confirm it: the connection ended, timed out, or
    /// failed as a whole, or the daemon was handing off. It may or may not
    /// have counted.
    Lost,
    /// The daemon refused this approver's answers (`Unauthorized`), for
    /// this (escaped) reason; it commits nothing.
    Refused(String),
}

/// The approver's state: at most one prompt, and at most one answer in
/// flight. Every untrusted string it shows is escaped as the dialog escapes
/// it.
#[derive(Debug)]
pub(super) struct Approver {
    /// This computer's hostname, as the host row names it.
    hostname: Option<String>,
    open: Option<OpenPrompt>,
    sent: Option<(OpenPrompt, ConsentDecision)>,
    /// Whether the whole prompt fit the terminal when it was last drawn.
    /// Nothing is allowed while it does not.
    fits: bool,
    /// The countdown as last drawn.
    countdown: Option<String>,
}

impl Approver {
    pub(super) const fn new(hostname: Option<String>) -> Self {
        Self {
            hostname,
            open: None,
            sent: None,
            fits: true,
            countdown: None,
        }
    }

    /// Whether a prompt is on screen.
    #[cfg(test)]
    pub(super) const fn prompting(&self) -> bool {
        self.open.is_some()
    }

    /// One event of the subscription, from the daemon of `generation`.
    pub(super) fn event(
        &mut self,
        event: SubscriptionEvent,
        generation: u64,
        now: Instant,
    ) -> Vec<Effect> {
        match event {
            SubscriptionEvent::ConsentOffered { offer } => self.offered(offer, generation, now),
            SubscriptionEvent::ConsentOfferWithdrawn { offer_id, reason } => {
                self.withdrawn(offer_id, reason)
            }
            _ => Vec::new(),
        }
    }

    /// A new offer. It replaces any earlier one, which is gone.
    pub(super) fn offered(
        &mut self,
        offer: ConsentOffer,
        generation: u64,
        now: Instant,
    ) -> Vec<Effect> {
        if let Err(error) = offer.validate() {
            return vec![Effect::Print(format!(
                "Ignored an access request splinterd sent that is not valid: {}",
                shown(&error.message, MAX_REASON_CHARS)
            ))];
        }
        if offer.daemon_generation != generation {
            return vec![Effect::Print(
                "Ignored an access request from another splinterd generation.".to_owned(),
            )];
        }
        // The same offer again changes nothing: no reprint, no re-arm.
        if self
            .open
            .as_ref()
            .is_some_and(|open| open.offer_id == offer.offer_id)
        {
            return Vec::new();
        }
        let mut effects = Vec::new();
        if self.open.is_some() {
            effects.push(Effect::Print(
                "The earlier request ended: a new one replaced it.".to_owned(),
            ));
        }
        self.open = Some(OpenPrompt::new(offer, now));
        self.countdown = None;
        effects.push(Effect::Open);
        effects
    }

    /// The daemon withdrew an offer: its prompt closes and says why.
    pub(super) fn withdrawn(&mut self, offer_id: u64, reason: ConsentOfferEnd) -> Vec<Effect> {
        if self
            .open
            .as_ref()
            .is_none_or(|open| open.offer_id != offer_id)
        {
            return Vec::new();
        }
        self.open = None;
        vec![Effect::Print(withdrawn_line(reason).to_owned())]
    }

    /// The connection to the daemon ended; its offer went with it.
    pub(super) fn connection_lost(&mut self) -> Vec<Effect> {
        self.sent = None;
        if self.open.take().is_none() {
            return Vec::new();
        }
        vec![Effect::Print(
            "The request's prompt closed without an answer: the connection to splinterd ended."
                .to_owned(),
        )]
    }

    /// One key, read at `at`.
    pub(super) fn key(&mut self, key: Key, at: Instant) -> Vec<Effect> {
        let Some(open) = self.open.as_mut() else {
            return if key == Key::Interrupt {
                vec![Effect::Quit]
            } else {
                Vec::new()
            };
        };
        if at < open.armed_at || at >= open.deadline {
            return Vec::new();
        }
        match key {
            Key::Up | Key::Down => {
                let selected = if key == Key::Up {
                    open.selected.checked_sub(1)
                } else {
                    Some(open.selected + 1).filter(|index| *index < open.choices.len())
                };
                let Some(selected) = selected else {
                    return Vec::new();
                };
                open.selected = selected;
                open.grant_armed_at = open.grant_armed_at.max(at + SELECTION_ARMING_DELAY);
                vec![Effect::Redraw]
            }
            Key::Enter => match open.choices[open.selected] {
                Choice::Deny => self.send(ConsentDecision::Deny),
                Choice::Grant(duration) => {
                    if at < open.grant_armed_at || !self.fits {
                        return Vec::new();
                    }
                    self.send(ConsentDecision::Grant { duration })
                }
            },
            Key::Escape | Key::Quit | Key::Interrupt => self.send(ConsentDecision::Deny),
            Key::Other => Vec::new(),
        }
    }

    fn send(&mut self, decision: ConsentDecision) -> Vec<Effect> {
        let Some(open) = self.open.take() else {
            return Vec::new();
        };
        let answer = open.answer(decision);
        self.sent = Some((open, decision));
        vec![Effect::Answer(answer)]
    }

    /// The answer sent last ended with `result`.
    pub(super) fn settle(&mut self, result: AnswerResult, now: Instant) -> Vec<Effect> {
        let Some((mut open, decision)) = self.sent.take() else {
            return Vec::new();
        };
        let line = match result {
            AnswerResult::Answered => outcome_line(&open, decision),
            AnswerResult::NotFound => {
                "Already answered elsewhere, or the request ended: this answer changed nothing."
                    .to_owned()
            }
            AnswerResult::InvalidArgument => {
                open.notice =
                    Some("splinterd did not accept that duration, and nothing was granted. Choose again.");
                // Back to Deny, so a quick Enter cannot repeat the grant.
                open.selected = 0;
                open.rearm(now);
                self.open = Some(open);
                self.countdown = None;
                return vec![Effect::Open];
            }
            AnswerResult::Lost => {
                "The connection to splinterd ended before it confirmed the answer, so it may not have counted."
                    .to_owned()
            }
            AnswerResult::Refused(reason) => {
                format!("splinterd no longer accepts answers from this approver, and nothing was granted: {reason}")
            }
        };
        vec![Effect::Print(line)]
    }

    /// Time passed: once the countdown runs out the prompt closes and sends
    /// nothing; otherwise it is drawn again when the countdown changes.
    pub(super) fn tick(&mut self, now: Instant) -> Vec<Effect> {
        let Some(open) = self.open.as_ref() else {
            return Vec::new();
        };
        if now >= open.deadline {
            self.open = None;
            return vec![Effect::Print("Expired: no answer sent.".to_owned())];
        }
        let countdown = countdown(open.deadline, now);
        if self.countdown.as_ref() == Some(&countdown) {
            return Vec::new();
        }
        vec![Effect::Redraw]
    }

    /// Whether the whole prompt fit the terminal, as last drawn.
    pub(super) const fn set_fits(&mut self, fits: bool) {
        self.fits = fits;
    }

    /// The open prompt's text at `now`, with the note that it does not fit
    /// unless `fits`; `None` without a prompt.
    pub(super) fn lines(&mut self, now: Instant, fits: bool) -> Option<Vec<DialogLine>> {
        let open = self.open.as_ref()?;
        let countdown = countdown(open.deadline, now);
        let lines = prompt_lines(open, self.hostname.as_deref(), &countdown, fits);
        self.countdown = Some(countdown);
        Some(lines)
    }
}

/// The time left at `now`, as the dialog counts it: "denies in m:ss",
/// rounded up, so it shows "0:00" only once the time is up.
fn countdown(deadline: Instant, now: Instant) -> String {
    let left = deadline.saturating_duration_since(now);
    let seconds = left.as_secs() + u64::from(left.subsec_nanos() > 0);
    format!("denies in {}:{:02}", seconds / 60, seconds % 60)
}

/// Why a withdrawn prompt closed.
const fn withdrawn_line(reason: ConsentOfferEnd) -> &'static str {
    match reason {
        ConsentOfferEnd::AnsweredElsewhere => {
            "Withdrawn: answered elsewhere, in another Window or approver."
        }
        ConsentOfferEnd::Cancelled => "Withdrawn: the agent stopped asking.",
        ConsentOfferEnd::Expired => "Withdrawn: the request expired without an answer.",
        ConsentOfferEnd::RequestEnded => {
            "Withdrawn: the request ended, for example because its workspace closed."
        }
        ConsentOfferEnd::Fenced => {
            "Withdrawn: splinterd is upgrading. The agent can ask again once it is done."
        }
    }
}

/// What an answer the daemon settled did.
fn outcome_line(open: &OpenPrompt, decision: ConsentDecision) -> String {
    let workspace = open.workspace();
    let access = access_label(open.prompt.level, &open.prompt.target);
    match decision {
        ConsentDecision::Deny => format!("Denied: no access to '{workspace}'."),
        ConsentDecision::Grant { duration } => match duration {
            GrantDuration::Once => format!("Allowed {access} once for '{workspace}'."),
            GrantDuration::WorkspaceSession => {
                format!("Allowed {access} for '{workspace}' while it is open.")
            }
            GrantDuration::Remembered => format!("Allowed {access} always for '{workspace}'."),
        },
    }
}

/// The label of one choice: "Deny", or the dialog's green button words.
fn choice_label(choice: Choice, access: &str) -> String {
    match choice {
        Choice::Deny => "Deny".to_owned(),
        Choice::Grant(duration) => button_label(access, duration),
    }
}

/// One line of the approver's own text, indented by `indent` columns, with
/// its wrapped rows under it.
fn indented(text: impl AsRef<str>, indent: usize) -> DialogLine {
    prompt_line(format!("{:indent$}{}", "", text.as_ref()), indent)
}

/// The prompt's text: the request exactly as the dialog shows it, then the
/// choices, what the selected one means, how to answer, and the countdown.
fn prompt_lines(
    open: &OpenPrompt,
    hostname: Option<&str>,
    countdown: &str,
    fits: bool,
) -> Vec<DialogLine> {
    let prompt = &open.prompt;
    let workspace = open.workspace();
    let access = access_label(prompt.level, &prompt.target);
    let mut lines = consent_request_lines(prompt, hostname);
    lines.push(DialogLine::blank());
    lines.push(DialogLine::new("your answer").with_style(DialogStyle::Group));
    for (index, choice) in open.choices.iter().enumerate() {
        let selected = index == open.selected;
        let marker = if selected { '>' } else { ' ' };
        lines.push(
            indented(format!("{marker} {}", choice_label(*choice, access)), 2).with_style(
                if selected {
                    DialogStyle::Selected
                } else {
                    DialogStyle::Text
                },
            ),
        );
    }
    for offer in &prompt.durations {
        if let Some(withheld) = offer.withheld {
            lines.push(
                indented(
                    format!(
                        "{} is unavailable: {}",
                        button_label(access, offer.duration),
                        withheld.reason()
                    ),
                    4,
                )
                .with_style(DialogStyle::Secondary),
            );
        }
    }
    lines.push(DialogLine::blank());
    match open.choices[open.selected] {
        Choice::Deny => lines.push(indented("Nothing is granted.", 4)),
        Choice::Grant(duration) => {
            lines.push(indented(duration_row(duration, &workspace), 4));
            let index = prompt
                .durations
                .iter()
                .position(|offer| offer.duration == duration)
                .unwrap_or_default();
            let ChosenLines { shared, replaced } = chosen_lines(prompt, index, &workspace);
            for line in shared.into_iter().chain(replaced) {
                lines.push(indented(&line.text, 4));
            }
        }
    }
    lines.push(DialogLine::blank());
    if let Some(notice) = open.notice {
        lines.push(indented(notice, 0).with_style(DialogStyle::Strong));
    }
    if !fits {
        lines.push(
            indented(
                "The terminal is too small to show the whole request: make it larger to allow. Deny still works.",
                0,
            )
            .with_style(DialogStyle::Strong),
        );
    }
    lines.push(DialogLine::hint(
        "Up/Down or j/k choose, Enter answers. Escape, q or Ctrl+C deny.",
    ));
    lines.push(DialogLine::hint(countdown));
    lines
}

// ---------------------------------------------------------------------------
// Painting.

/// Any character trusted text must never send to the terminal as is. The
/// prompt's text is escaped already; this is the last guard before output.
fn neutralised(row: &str) -> String {
    row.chars()
        .map(|character| {
            if unsafe_display_character(character) {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const REVERSE: &str = "\x1b[7m";
const RESET: &str = "\x1b[0m";

/// The prompt as terminal rows of at most `columns` characters, with this
/// module's own styling as the only escape sequences in them.
pub(super) fn paint(lines: &[DialogLine], columns: usize) -> Vec<String> {
    let columns = columns.max(MIN_COLUMNS);
    let mut rows = Vec::new();
    for line in lines {
        let (line, style) = if line.role == DialogLineRole::Title {
            (DialogLine::new(format!("{TITLE_MARK} {}", line.text)), BOLD)
        } else {
            let style = match (line.role, line.style) {
                (DialogLineRole::Hint, _) | (_, DialogStyle::Secondary | DialogStyle::Group) => DIM,
                (_, DialogStyle::Strong) => BOLD,
                (_, DialogStyle::Selected) => REVERSE,
                _ => "",
            };
            (line.clone(), style)
        };
        for (index, row) in wrap_line(&line, columns).iter().enumerate() {
            let row = neutralised(row);
            let lead = if index == 0 { line.lead } else { 0 };
            let split = row
                .char_indices()
                .nth(lead)
                .map_or(row.len(), |(offset, _)| offset);
            let (label, rest) = row.split_at(split);
            let label = if label.is_empty() {
                String::new()
            } else {
                format!("{DIM}{label}{RESET}")
            };
            let rest = if style.is_empty() || rest.is_empty() {
                rest.to_owned()
            } else {
                format!("{style}{rest}{RESET}")
            };
            rows.push(label + &rest);
        }
    }
    rows
}

/// How many terminal rows `rows`, painted for `columns`, take, as the
/// terminal lays them out: a character that does not fit in what is left of
/// a row starts the next one. A character of ambiguous width counts as two
/// cells, as a CJK terminal draws it, so the count is never too low.
pub(super) fn terminal_rows(rows: &[String], columns: usize) -> usize {
    use unicode_width::UnicodeWidthChar as _;

    let columns = columns.max(1);
    rows.iter()
        .map(|row| {
            let text = [BOLD, DIM, REVERSE, RESET]
                .iter()
                .fold(row.clone(), |text, code| text.replace(code, ""));
            let mut taken = 1;
            let mut column = 0;
            for character in text.chars() {
                let width = character.width_cjk().unwrap_or(0).min(2);
                if column + width > columns {
                    taken += 1;
                    column = 0;
                }
                column += width;
            }
            taken
        })
        .sum()
}

// ---------------------------------------------------------------------------
// The daemon.

/// Why connecting or subscribing failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Attempt {
    /// The daemon refused this approver; trying again cannot help.
    Refused(String),
    /// The daemon is handing off; try again shortly.
    Busy,
    /// The daemon could not be reached.
    Unavailable(String),
}

/// What a refusal of `error` tells the person. `connected_once` says
/// whether this approver was connected before, so a refused handshake means
/// its own file was replaced since.
pub(super) fn classify(error: &anyhow::Error, connected_once: bool) -> Attempt {
    let Some(refusal) = protocol_error(error) else {
        return Attempt::Unavailable(format!(
            "cannot reach splinterd: {}",
            shown(&format!("{error:#}"), MAX_REASON_CHARS)
        ));
    };
    classify_refusal(refusal, connected_once)
}

/// What the daemon's refusal tells the person.
pub(super) fn classify_refusal(refusal: &ProtocolError, connected_once: bool) -> Attempt {
    let reason = shown(&refusal.message, MAX_REASON_CHARS);
    match refusal.code {
        ErrorCode::Unauthorized
            if connected_once && refusal.message == TRUSTED_UI_REQUIRES_INSTALLED_CLIENT =>
        {
            Attempt::Refused(format!("{REPLACED}\n(splinterd: {reason})"))
        }
        ErrorCode::Unauthorized => Attempt::Refused(format!(
            "{}\n(splinterd: {reason})",
            unauthorized_guidance(&refusal.message)
        )),
        ErrorCode::ResourceLimit => Attempt::Refused(format!("{TOO_MANY}\n(splinterd: {reason})")),
        ErrorCode::HandoffInProgress => Attempt::Busy,
        ErrorCode::IncompatibleVersion => Attempt::Refused(format!(
            "splinterd runs another Splinterm version: run the splinterm installed with it.\n(splinterd: {reason})"
        )),
        _ => Attempt::Refused(format!("splinterd refused the approver: {reason}")),
    }
}

/// The guidance for one `Unauthorized` refusal, by the daemon's reason
/// (the protocol's shared reason strings); any other reason is about where
/// the approver runs.
fn unauthorized_guidance(message: &str) -> &'static str {
    match message {
        TRUSTED_UI_REQUIRES_INSTALLED_CLIENT => {
            "Only the installed splinterm beside the running splinterd can answer access requests (for a package, /usr/bin/splinterm). Run that one, not a development build or a copy."
        }
        CONSENT_RESPONDER_IDENTITY_CHANGED => REPLACED,
        CONSENT_APPROVER_NEEDS_TERMINAL => {
            "Run `splinterm approvals` directly in your own terminal, with standard input and output on it and not redirected."
        }
        _ => OWN_TERMINAL,
    }
}

/// Connects to this computer's daemon as the installed client and
/// subscribes as the terminal approver.
async fn connect_approver(connected_once: bool) -> Result<(Connection, u64), Attempt> {
    let mut connection = Connection::connect()
        .await
        .map_err(|error| classify(&error, connected_once))?;
    match connection
        .request_with_deadline(
            Request::SubscribeConsentOffers {
                kind: ConsentResponderKind::Terminal,
            },
            CONNECT_DEADLINE,
        )
        .await
    {
        Ok(Response::ConsentOffersSubscribed { subscription_id }) => {
            Ok((connection, subscription_id))
        }
        Ok(_) => Err(Attempt::Unavailable(
            "splinterd sent an unexpected answer to the approver's subscription".to_owned(),
        )),
        Err(error) if is_connection_loss(&error) || request_cancellation(&error).is_some() => {
            Err(Attempt::Unavailable(
                "the connection to splinterd ended while it checked the approver".to_owned(),
            ))
        }
        Err(error) => Err(classify(&error, connected_once)),
    }
}

/// Sends one answer and waits for the daemon to settle it.
async fn send_answer(connection: &mut Connection, answer: Answer) -> AnswerResult {
    let reply = connection
        .request_with_deadline(answer.request(), ANSWER_DEADLINE)
        .await;
    answer_outcome(reply, connection.is_usable())
}

/// What the daemon's reply to an answer means. `usable` says whether the
/// connection survived it: an error that ended the connection was about the
/// connection, not this answer. Only the three refusals of the answer itself
/// are told apart: `NotFound`, `InvalidArgument`, and `Unauthorized`, after
/// which this approver's answers commit nothing. Anything else, a handoff,
/// an internal or connection-wide error (such as the close of a lagging
/// responder), a timeout, or an unexpected response, leaves the answer
/// unconfirmed, and the approver connects again.
pub(super) fn answer_outcome(reply: Result<Response>, usable: bool) -> AnswerResult {
    match reply {
        Ok(Response::ConsentOfferAnswered) => AnswerResult::Answered,
        Ok(_) => AnswerResult::Lost,
        Err(_) if !usable => AnswerResult::Lost,
        Err(error) => match protocol_error(&error) {
            Some(refusal) if refusal.code == ErrorCode::NotFound => AnswerResult::NotFound,
            Some(refusal) if refusal.code == ErrorCode::InvalidArgument => {
                AnswerResult::InvalidArgument
            }
            Some(refusal) if refusal.code == ErrorCode::Unauthorized => {
                AnswerResult::Refused(shown(&refusal.message, MAX_REASON_CHARS))
            }
            _ => AnswerResult::Lost,
        },
    }
}

/// The line that says why the connection ended.
fn lost_line(error: &anyhow::Error) -> String {
    match protocol_error(error) {
        Some(refusal) if refusal.code == ErrorCode::ResourceLimit => {
            "splinterd closed the approver's connection because it fell behind.".to_owned()
        }
        Some(refusal) => format!(
            "splinterd closed the approver's connection: {}",
            shown(&refusal.message, MAX_REASON_CHARS)
        ),
        None => "The connection to splinterd ended, for example because it upgraded or restarted."
            .to_owned(),
    }
}

/// The line printed while the approver waits for prompts.
fn waiting_line(hostname: Option<&str>) -> String {
    let host = hostname.map_or_else(
        || "this computer".to_owned(),
        |name| display_name(name, MAX_DAEMON_HOSTNAME_BYTES),
    );
    format!("Waiting for agent access requests on {host}… (Ctrl+C to quit)")
}

// ---------------------------------------------------------------------------
// The session.

/// Keys read from the terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Input {
    Keys(Instant, Vec<Key>),
    /// The terminal closed or cannot be read.
    End,
}

/// Where the session draws.
pub(super) trait Screen {
    /// The terminal's columns and rows.
    fn size(&self) -> (usize, usize);
    /// Throws away input the terminal holds that has not been read.
    fn flush_input(&mut self);
    /// Draws the prompt's rows over the whole screen.
    fn show(&mut self, rows: &[String]);
    /// Takes the prompt off the screen.
    fn leave(&mut self);
    /// Prints one line below the earlier ones.
    fn print(&mut self, line: &str);
}

/// What the session loop does after applying effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Flow {
    Answer(Answer),
    Quit,
}

/// How serving one connection ended.
enum End {
    Quit,
    Lost(String),
    Refused(String),
}

/// The approver on one terminal.
pub(super) struct Session<S: Screen> {
    pub(super) approver: Approver,
    pub(super) screen: S,
    pub(super) keys: mpsc::Receiver<Input>,
    shown: bool,
    size: (usize, usize),
}

impl<S: Screen> Session<S> {
    pub(super) const fn new(approver: Approver, screen: S, keys: mpsc::Receiver<Input>) -> Self {
        Self {
            approver,
            screen,
            keys,
            shown: false,
            size: (0, 0),
        }
    }

    /// Applies `effects` in order; what the loop must do next, if anything.
    pub(super) fn apply(&mut self, effects: Vec<Effect>, now: Instant) -> Option<Flow> {
        let mut flow = None;
        for effect in effects {
            match effect {
                Effect::Open => {
                    // Type-ahead: what the terminal still holds, and what was
                    // read but not handled yet. Keys read later and before
                    // the prompt arms are ignored by the approver.
                    self.screen.flush_input();
                    while let Ok(input) = self.keys.try_recv() {
                        if input == Input::End {
                            flow = Some(Flow::Quit);
                        }
                    }
                    self.draw(now);
                }
                Effect::Redraw => self.draw(now),
                Effect::Print(line) => {
                    self.leave();
                    self.screen.print(&line);
                }
                Effect::Answer(answer) => {
                    self.leave();
                    flow = Some(Flow::Answer(answer));
                }
                Effect::Quit => flow = Some(Flow::Quit),
            }
        }
        flow
    }

    fn leave(&mut self) {
        if self.shown {
            self.screen.leave();
            self.shown = false;
        }
    }

    fn draw(&mut self, now: Instant) {
        let (columns, rows) = self.screen.size();
        self.size = (columns, rows);
        let Some(lines) = self.approver.lines(now, true) else {
            return;
        };
        let mut painted = paint(&lines, columns);
        let fits = columns >= MIN_COLUMNS && terminal_rows(&painted, columns) <= rows;
        if !fits && let Some(lines) = self.approver.lines(now, false) {
            painted = paint(&lines, columns);
        }
        self.approver.set_fits(fits);
        self.screen.show(&painted);
        self.shown = true;
    }

    /// Keys read at `at`, up to the first one that answers or quits.
    pub(super) fn keys(&mut self, at: Instant, keys: &[Key]) -> Option<Flow> {
        for key in keys {
            // An Allow answer counts only while the whole prompt fits: look
            // at the size again rather than trust the last tick's.
            if *key == Key::Enter && self.shown && self.screen.size() != self.size {
                self.draw(at);
            }
            let effects = self.approver.key(*key, at);
            if let Some(flow) = self.apply(effects, at) {
                return Some(flow);
            }
        }
        None
    }

    /// The countdown and the terminal's size, at `now`.
    pub(super) fn tick(&mut self, now: Instant) -> Option<Flow> {
        let mut effects = self.approver.tick(now);
        if effects.is_empty() && self.shown && self.screen.size() != self.size {
            effects.push(Effect::Redraw);
        }
        self.apply(effects, now)
    }

    fn print(&mut self, line: &str) {
        self.leave();
        self.screen.print(line);
    }

    /// Serves until Ctrl+C, a signal, or a refusal, reconnecting after a
    /// lost connection with bounded backoff.
    async fn run(&mut self, signals: &mut Signals) -> Result<()> {
        let mut backoff = ReconnectBackoff::default();
        let mut connected_once = false;
        let mut waiting_said = false;
        loop {
            let Some(attempt) = self.connect(connected_once, signals).await else {
                return Ok(());
            };
            match attempt {
                Ok((mut connection, subscription_id)) => {
                    backoff = ReconnectBackoff::default();
                    if connected_once {
                        self.print("Connected to splinterd again.");
                    }
                    connected_once = true;
                    let waiting = waiting_line(self.approver.hostname.as_deref());
                    self.print(&waiting);
                    let end = Box::pin(self.serve(&mut connection, subscription_id, signals)).await;
                    match end {
                        End::Quit => return Ok(()),
                        End::Refused(message) => bail!(message),
                        End::Lost(line) => {
                            let effects = self.approver.connection_lost();
                            self.apply(effects, Instant::now());
                            self.print(&format!("{line} Connecting again…"));
                            waiting_said = true;
                        }
                    }
                }
                Err(Attempt::Refused(message)) => bail!(message),
                Err(Attempt::Unavailable(message)) if !connected_once => bail!(message),
                Err(Attempt::Unavailable(message)) => {
                    if !waiting_said {
                        self.print(&format!("{message}; trying again…"));
                        waiting_said = true;
                    }
                }
                Err(Attempt::Busy) => {
                    if !waiting_said {
                        self.print("splinterd is upgrading; waiting for it…");
                        waiting_said = true;
                    }
                }
            }
            if self.pause(backoff.next_delay(), signals).await {
                return Ok(());
            }
        }
    }

    /// One connection attempt, bounded by [`CONNECT_DEADLINE`]; `None` when
    /// Ctrl+C, a signal, or the end of input quits first. In raw mode Ctrl+C
    /// is only a key, so it must be read here too.
    async fn connect(
        &mut self,
        connected_once: bool,
        signals: &mut Signals,
    ) -> Option<Result<(Connection, u64), Attempt>> {
        let mut attempt = Box::pin(tokio::time::timeout(
            CONNECT_DEADLINE,
            connect_approver(connected_once),
        ));
        loop {
            tokio::select! {
                result = &mut attempt => {
                    return Some(result.unwrap_or_else(|_| {
                        Err(Attempt::Unavailable(
                            "splinterd did not answer the approver in time".to_owned(),
                        ))
                    }));
                }
                () = signals.recv() => return None,
                input = self.keys.recv() => match input {
                    Some(Input::Keys(at, keys)) => {
                        if self.keys(at, &keys) == Some(Flow::Quit) {
                            return None;
                        }
                    }
                    Some(Input::End) | None => return None,
                },
            }
        }
    }

    /// Waits `delay` between connection attempts; `true` to quit.
    async fn pause(&mut self, delay: Duration, signals: &mut Signals) -> bool {
        let sleep = tokio::time::sleep(delay);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                () = &mut sleep => return false,
                () = signals.recv() => return true,
                input = self.keys.recv() => match input {
                    Some(Input::Keys(at, keys)) => {
                        if self.keys(at, &keys) == Some(Flow::Quit) {
                            return true;
                        }
                    }
                    Some(Input::End) | None => return true,
                },
            }
        }
    }

    /// Serves one subscribed connection.
    async fn serve(
        &mut self,
        connection: &mut Connection,
        subscription_id: u64,
        signals: &mut Signals,
    ) -> End {
        let mut ticker = tokio::time::interval(TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let flow = tokio::select! {
                () = signals.recv() => return End::Quit,
                input = self.keys.recv() => match input {
                    Some(Input::Keys(at, keys)) => self.keys(at, &keys),
                    Some(Input::End) | None => return End::Quit,
                },
                frame = connection.next_server_frame() => match frame {
                    Ok(ServerFrame::Event { subscription_id: id, event, .. })
                        if id == subscription_id =>
                    {
                        let now = Instant::now();
                        let effects =
                            self.approver.event(event, connection.daemon_generation(), now);
                        self.apply(effects, now)
                    }
                    Ok(_) => None,
                    Err(error) => return End::Lost(lost_line(&error)),
                },
                _ = ticker.tick() => self.tick(Instant::now()),
            };
            match flow {
                None => {}
                Some(Flow::Quit) => return End::Quit,
                Some(Flow::Answer(answer)) => {
                    let result = send_answer(connection, answer).await;
                    let lost = result == AnswerResult::Lost;
                    let refused = match &result {
                        AnswerResult::Refused(reason) => Some(format!(
                            "{}\n(splinterd: {reason})",
                            unauthorized_guidance(reason)
                        )),
                        _ => None,
                    };
                    let now = Instant::now();
                    let effects = self.approver.settle(result, now);
                    if self.apply(effects, now) == Some(Flow::Quit) {
                        return End::Quit;
                    }
                    if lost {
                        return End::Lost("splinterd did not confirm the answer.".to_owned());
                    }
                    if let Some(message) = refused {
                        // The daemon no longer takes this approver's
                        // answers, for example because its file was
                        // replaced; trying again cannot help.
                        return End::Refused(message);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The real terminal.

/// The saved terminal modes and whether the alternate screen is in use, for
/// restoring on exit, on panic, and on a signal.
static SAVED_MODES: Mutex<Option<Termios>> = Mutex::new(None);
static ALTERNATE_SCREEN: AtomicBool = AtomicBool::new(false);
static BRACKETED_PASTE: AtomicBool = AtomicBool::new(false);
static PANIC_HOOK: Once = Once::new();

const ENTER_SCREEN: &str = "\x1b[?1049h\x1b[?25l";
const LEAVE_SCREEN: &str = "\x1b[?25h\x1b[?1049l";
/// Asks the terminal to mark pasted text, so a paste is never read as keys.
const ENABLE_PASTE_MARKS: &str = "\x1b[?2004h";
const DISABLE_PASTE_MARKS: &str = "\x1b[?2004l";

/// Puts the terminal back as it was: the main screen, the cursor, paste
/// marks off, and the saved modes. Safe to call more than once.
fn restore_terminal() {
    let mut text = String::new();
    if ALTERNATE_SCREEN.swap(false, Ordering::SeqCst) {
        text.push_str(LEAVE_SCREEN);
    }
    if BRACKETED_PASTE.swap(false, Ordering::SeqCst) {
        text.push_str(DISABLE_PASTE_MARKS);
    }
    if !text.is_empty() {
        let mut stdout = io::stdout();
        let _ = stdout.write_all(text.as_bytes());
        let _ = stdout.flush();
    }
    let saved = SAVED_MODES.lock().map_or_else(
        |poisoned| poisoned.into_inner().take(),
        |mut saved| saved.take(),
    );
    if let Some(saved) = saved {
        let _ = rustix::termios::tcsetattr(io::stdin(), OptionalActions::Now, &saved);
    }
}

/// The terminal in raw mode for as long as it lives: no echo, no line
/// editing, and Ctrl+C read as a key. Dropping it, a panic, or a signal
/// that ends the session restores the terminal.
struct RawTerminal;

impl RawTerminal {
    fn enter() -> Result<Self> {
        let stdin = io::stdin();
        let saved =
            rustix::termios::tcgetattr(&stdin).context("cannot read the terminal's modes")?;
        let mut raw = saved.clone();
        raw.local_modes
            .remove(LocalModes::ICANON | LocalModes::ECHO | LocalModes::ISIG | LocalModes::IEXTEN);
        raw.input_modes.remove(InputModes::ICRNL | InputModes::IXON);
        // A read returns at once, even with nothing waiting, so a flush
        // between the poll and the read can never leave it blocked.
        raw.special_codes[SpecialCodeIndex::VMIN] = 0;
        raw.special_codes[SpecialCodeIndex::VTIME] = 0;
        *SAVED_MODES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(saved);
        PANIC_HOOK.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore_terminal();
                previous(info);
            }));
        });
        rustix::termios::tcsetattr(&stdin, OptionalActions::Now, &raw)
            .context("cannot set the terminal's modes")?;
        BRACKETED_PASTE.store(true, Ordering::SeqCst);
        TtyScreen::write(ENABLE_PASTE_MARKS);
        Ok(Self)
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// The real terminal: the prompt on the alternate screen, lines on the
/// main one.
struct TtyScreen;

impl TtyScreen {
    fn write(text: &str) {
        let mut stdout = io::stdout().lock();
        let _ = stdout.write_all(text.as_bytes());
        let _ = stdout.flush();
    }
}

impl Screen for TtyScreen {
    /// The terminal's size, or the classic 80 by 24 when it reports none.
    fn size(&self) -> (usize, usize) {
        rustix::termios::tcgetwinsize(io::stdout())
            .ok()
            .filter(|size| size.ws_col > 0 && size.ws_row > 0)
            .map_or((80, 24), |size| {
                (usize::from(size.ws_col), usize::from(size.ws_row))
            })
    }

    fn flush_input(&mut self) {
        let _ = rustix::termios::tcflush(io::stdin(), QueueSelector::IFlush);
    }

    fn show(&mut self, rows: &[String]) {
        let mut text = String::new();
        if !ALTERNATE_SCREEN.swap(true, Ordering::SeqCst) {
            text.push_str(ENTER_SCREEN);
        }
        text.push_str("\x1b[H\x1b[2J");
        text.push_str(&rows.join("\r\n"));
        Self::write(&text);
    }

    fn leave(&mut self) {
        if ALTERNATE_SCREEN.swap(false, Ordering::SeqCst) {
            Self::write(LEAVE_SCREEN);
        }
    }

    fn print(&mut self, line: &str) {
        let line = line
            .lines()
            .map(neutralised)
            .collect::<Vec<_>>()
            .join("\r\n");
        Self::write(&format!("{line}\r\n"));
    }
}

/// Reads the terminal on its own thread, so a read never holds up the
/// daemon's events. It stops within a poll slice once dropped.
struct KeyReader {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl KeyReader {
    fn start() -> Result<(Self, mpsc::Receiver<Input>)> {
        const SLICE: Timespec = Timespec {
            tv_sec: 0,
            tv_nsec: 100_000_000,
        };
        let (sender, receiver) = mpsc::channel(64);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("approvals-keys".to_owned())
            .spawn(move || {
                let stdin = io::stdin();
                let input = stdin.as_fd();
                let mut decoder = KeyDecoder::default();
                let hung_up = PollFlags::HUP | PollFlags::ERR | PollFlags::NVAL;
                while !stopping.load(Ordering::SeqCst) {
                    let mut descriptors = [PollFd::new(&input, PollFlags::IN)];
                    match poll(&mut descriptors, Some(&SLICE)) {
                        Ok(0) | Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => {
                            continue;
                        }
                        Ok(_) => {}
                        Err(_) => {
                            let _ = sender.blocking_send(Input::End);
                            return;
                        }
                    }
                    let events = descriptors[0].revents();
                    let mut chunk = [0_u8; 64];
                    let read = match rustix::io::read(input, &mut chunk) {
                        // With VMIN 0 an empty read is either a hang-up or
                        // input flushed since the poll.
                        Ok(0) if events.intersects(hung_up) => Input::End,
                        Ok(0) | Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => {
                            continue;
                        }
                        Ok(read) => Input::Keys(Instant::now(), decoder.decode(&chunk[..read])),
                        Err(_) => Input::End,
                    };
                    let end = read == Input::End;
                    if sender.blocking_send(read).is_err() || end {
                        return;
                    }
                }
            })
            .context("cannot start reading the terminal")?;
        Ok((
            Self {
                stop,
                thread: Some(thread),
            },
            receiver,
        ))
    }
}

impl Drop for KeyReader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The signals that end the approver: it restores the terminal first.
struct Signals {
    interrupt: Signal,
    terminate: Signal,
    hangup: Signal,
}

impl Signals {
    fn install() -> Result<Self> {
        Ok(Self {
            interrupt: signal(SignalKind::interrupt()).context("cannot handle SIGINT")?,
            terminate: signal(SignalKind::terminate()).context("cannot handle SIGTERM")?,
            hangup: signal(SignalKind::hangup()).context("cannot handle SIGHUP")?,
        })
    }

    async fn recv(&mut self) {
        tokio::select! {
            _ = self.interrupt.recv() => {}
            _ = self.terminate.recv() => {}
            _ = self.hangup.recv() => {}
        }
    }
}

/// The warning to print first when the approver runs inside tmux (`$TMUX`)
/// or screen (`$STY`). It warns and goes on: the person chose the terminal.
fn multiplexer_warning(tmux: bool, screen: bool) -> Option<&'static str> {
    (tmux || screen).then_some(MULTIPLEXER_WARNING)
}

/// `splinterm approvals`.
pub(super) async fn run() -> Result<()> {
    if let Err(refusal) = check_terminal(&TerminalFacts::read()) {
        bail!(refusal.message());
    }
    if let Some(warning) = multiplexer_warning(
        std::env::var_os("TMUX").is_some(),
        std::env::var_os("STY").is_some(),
    ) {
        println!("{warning}");
    }
    let mut signals = Signals::install()?;
    let terminal = RawTerminal::enter()?;
    let (reader, keys) = KeyReader::start()?;
    let mut session = Session::new(Approver::new(local_hostname()), TtyScreen, keys);
    let result = Box::pin(session.run(&mut signals)).await;
    drop(session);
    drop(reader);
    drop(terminal);
    result
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use splinterm::{automation::response_protocol_error, frontend::unsafe_display_character};
    use splinterm_core::LairId;
    use splinterm_protocol::{
        AccessLevel, CONSENT_APPROVER_NEEDS_TERMINAL, CONSENT_OFFER_NONCE_BYTES,
        CONSENT_RESPONDER_IDENTITY_CHANGED, ConsentDecision, ConsentDurationOffer, ConsentOffer,
        ConsentOfferEnd, ConsentOfferNonce, ConsentTarget, DurationWithheld, ErrorCode,
        GrantDuration, OfferedConsentPrompt, ProtocolError, Request, Response,
        TRUSTED_UI_REQUIRES_INSTALLED_CLIENT,
    };
    use tokio::sync::mpsc;

    use super::{
        ARMING_DELAY, AnswerResult, Approver, Attempt, BOLD, Choice, DIM, Effect, Flow, Input, Key,
        KeyDecoder, MIN_COLUMNS, REPLACED, RESET, REVERSE, SELECTION_ARMING_DELAY, Screen, Session,
        TerminalFacts, TerminalRefusal, answer_outcome, check_terminal, classify_refusal,
        controlling_terminal, decode_keys, multiplexer_warning, paint, waiting_line,
    };

    const GENERATION: u64 = 7;

    fn duration(
        duration: GrantDuration,
        withheld: Option<DurationWithheld>,
    ) -> ConsentDurationOffer {
        ConsentDurationOffer {
            duration,
            withheld,
            shared_by: (withheld.is_none() && duration != GrantDuration::Once)
                .then(|| "/usr/bin/splinterm-mcp".to_owned()),
            replaces: None,
        }
    }

    fn offered_prompt() -> OfferedConsentPrompt {
        OfferedConsentPrompt {
            requester: "/usr/bin/splinterm-mcp".to_owned(),
            requester_pid: 42,
            requester_uid: 1000,
            agent_name: Some("Claude Code".to_owned()),
            target: ConsentTarget::Lair {
                lair_id: LairId::new(),
                lair_name: "Workspace".to_owned(),
            },
            scopes: AccessLevel::Manage.scopes().to_vec(),
            level: Some(AccessLevel::Manage),
            durations: vec![
                duration(GrantDuration::Once, None),
                duration(GrantDuration::WorkspaceSession, None),
                duration(GrantDuration::Remembered, None),
            ],
        }
    }

    fn offer(offer_id: u64) -> ConsentOffer {
        ConsentOffer {
            daemon_generation: GENERATION,
            offer_id,
            request_id: 3,
            nonce: ConsentOfferNonce::new([9; CONSENT_OFFER_NONCE_BYTES]),
            respond_within_seconds: 90,
            prompt: offered_prompt(),
        }
    }

    /// An approver showing `offer`, offered at `now`.
    fn showing(offer: ConsentOffer, now: Instant) -> Approver {
        let mut approver = Approver::new(Some("wintermute".to_owned()));
        assert_eq!(approver.offered(offer, GENERATION, now), vec![Effect::Open]);
        approver
    }

    /// The prompt's rows without this module's own styling; any escape
    /// sequence left in them came from somewhere else.
    fn plain(rows: &[String]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                [BOLD, DIM, REVERSE, RESET]
                    .iter()
                    .fold(row.clone(), |row, code| row.replace(code, ""))
            })
            .collect()
    }

    fn shown_rows(approver: &mut Approver, now: Instant) -> Vec<String> {
        plain(&paint(&approver.lines(now, true).expect("a prompt"), 120))
    }

    fn answer(effects: &[Effect]) -> super::Answer {
        match effects {
            [Effect::Answer(answer)] => *answer,
            other => panic!("expected one answer, got {other:?}"),
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Op {
        Flush,
        Show(Vec<String>),
        Leave,
        Print(String),
    }

    struct FakeScreen {
        ops: Vec<Op>,
        size: (usize, usize),
    }

    impl Screen for FakeScreen {
        fn size(&self) -> (usize, usize) {
            self.size
        }
        fn flush_input(&mut self) {
            self.ops.push(Op::Flush);
        }
        fn show(&mut self, rows: &[String]) {
            self.ops.push(Op::Show(rows.to_vec()));
        }
        fn leave(&mut self) {
            self.ops.push(Op::Leave);
        }
        fn print(&mut self, line: &str) {
            self.ops.push(Op::Print(line.to_owned()));
        }
    }

    fn session(size: (usize, usize)) -> (Session<FakeScreen>, mpsc::Sender<Input>) {
        let (sender, keys) = mpsc::channel(8);
        let screen = FakeScreen {
            ops: Vec::new(),
            size,
        };
        (
            Session::new(Approver::new(Some("wintermute".to_owned())), screen, keys),
            sender,
        )
    }

    #[test]
    fn it_refuses_without_its_own_terminal_on_standard_input_and_output() {
        let terminal = TerminalFacts {
            stdin_terminal: true,
            stdout_terminal: true,
            controlling: Some(34_819),
            stdin_device: Some(34_819),
            stdout_device: Some(34_819),
            foreground: true,
        };
        assert_eq!(check_terminal(&terminal), Ok(()));
        for (facts, refusal) in [
            (
                TerminalFacts {
                    stdin_terminal: false,
                    stdin_device: None,
                    ..terminal
                },
                TerminalRefusal::NotATerminal,
            ),
            (
                TerminalFacts {
                    stdout_terminal: false,
                    stdout_device: None,
                    ..terminal
                },
                TerminalRefusal::NotATerminal,
            ),
            (
                TerminalFacts {
                    controlling: None,
                    ..terminal
                },
                TerminalRefusal::NoControllingTerminal,
            ),
            (
                TerminalFacts {
                    stdout_device: Some(34_820),
                    ..terminal
                },
                TerminalRefusal::OtherTerminal,
            ),
            (
                TerminalFacts {
                    foreground: false,
                    ..terminal
                },
                TerminalRefusal::Background,
            ),
        ] {
            assert_eq!(check_terminal(&facts), Err(refusal), "{facts:?}");
            assert!(refusal.message().starts_with("splinterm approvals"));
        }
    }

    #[test]
    fn it_reads_the_controlling_terminal_from_its_stat_line() {
        // /dev/pts/3: major 136, minor 3, tty_nr 34819. The command name
        // may hold spaces and parentheses.
        let stat = b"4242 (a (b) c) S 1 4242 4242 34819 4242 0";
        assert_eq!(
            controlling_terminal(stat),
            Some(rustix::fs::makedev(136, 3))
        );
        // A minor number above 255 uses the upper bits: /dev/pts/300 is
        // tty_nr 1083436, as the kernel encodes it.
        let stat = "1 (sh) S 1 1 1 1083436 1 0";
        assert_eq!(
            controlling_terminal(stat.as_bytes()),
            Some(rustix::fs::makedev(136, 300))
        );
        assert_eq!(controlling_terminal(b"1 (sh) S 1 1 1 0 -1 0"), None);
        assert_eq!(controlling_terminal(b"garbage"), None);
    }

    #[test]
    fn its_own_controlling_terminal_is_its_standard_input_when_it_has_one() {
        // When the test runs with standard input on its session's
        // controlling terminal, both readings must name the same device.
        // Comparing /dev/tty's own number (5, 0) never matched.
        let stdin = std::io::stdin();
        let on_controlling = rustix::termios::tcgetsid(&stdin)
            .is_ok_and(|session| rustix::process::getsid(None).is_ok_and(|own| own == session));
        if on_controlling {
            let facts = TerminalFacts::read();
            assert_eq!(facts.controlling, facts.stdin_device);
        }
    }

    #[test]
    fn it_shows_the_request_as_the_dialog_does_with_this_computer_on_the_host_row() {
        let now = Instant::now();
        let mut approver = showing(offer(1), now);
        let rows = shown_rows(&mut approver, now);
        assert_eq!(rows[0], "⚠ agent access request");
        assert!(
            rows.contains(&"'Claude Code' wants Manage access to 'Workspace'".to_owned()),
            "{rows:#?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.starts_with("The agent names itself"))
        );
        assert!(
            rows.contains(&"host      this computer (wintermute)".to_owned()),
            "{rows:#?}"
        );
        assert!(rows.contains(&"program   /usr/bin/splinterm-mcp".to_owned()));
        assert!(rows.contains(&"process   PID 42 · UID 1000".to_owned()));
        assert!(
            rows.iter()
                .any(|row| row.starts_with("workspace ") && row.ends_with(" 'Workspace'"))
        );
        assert!(rows.contains(&"denies in 1:30".to_owned()), "{rows:#?}");
        assert_eq!(
            waiting_line(Some("wintermute")),
            "Waiting for agent access requests on wintermute… (Ctrl+C to quit)"
        );
    }

    #[test]
    fn untrusted_names_and_paths_never_reach_the_terminal_raw() {
        let now = Instant::now();
        let mut hostile = offer(1);
        hostile.prompt.agent_name =
            Some("\x1b[31mEvil\x1b]0;title\x07 \u{202e}gnp.exe\u{200b}\u{9b}2J".to_owned());
        hostile.prompt.target = ConsentTarget::Lair {
            lair_id: LairId::new(),
            lair_name: "\x1b[2J\u{2066}Work\u{feff}".to_owned(),
        };
        hostile.prompt.requester = "/tmp/\x1b[A\u{200d}x".to_owned();
        let mut approver = showing(hostile, now);
        for selected in 0..4_u32 {
            let rows = shown_rows(&mut approver, now);
            for row in &rows {
                assert!(!row.contains('\x1b'), "raw ESC in {row:?}");
                assert!(
                    !row.chars().any(unsafe_display_character),
                    "unsafe character in {row:?}"
                );
            }
            let text = rows.join("\n");
            assert!(text.contains("\\u{001b}[31mEvil"), "{text}");
            // The name is cut at the dialog's length, never inside an escape.
            assert!(text.contains("\\u{202e}gnp.exe…'"), "{text}");
            assert!(text.contains("\\u{2066}Work\\u{feff}"), "{text}");
            assert!(text.contains("/tmp/\\u{001b}[A\\u{200d}x"), "{text}");
            if selected < 3 {
                approver.key(
                    Key::Down,
                    now + ARMING_DELAY + SELECTION_ARMING_DELAY * selected,
                );
            }
        }
        // The outcome names the workspace escaped too.
        let at = now + ARMING_DELAY * 3;
        let sent = answer(&approver.key(Key::Enter, at));
        assert_eq!(
            sent.decision,
            ConsentDecision::Grant {
                duration: GrantDuration::Remembered
            }
        );
        let settled = approver.settle(AnswerResult::Answered, at);
        let [Effect::Print(line)] = settled.as_slice() else {
            panic!("expected the outcome");
        };
        assert!(!line.contains('\x1b') && !line.chars().any(unsafe_display_character));
        assert!(line.contains("\\u{2066}Work"), "{line}");
        let host = waiting_line(Some("\x1b[2Jhost"));
        assert!(!host.contains('\x1b'), "{host}");
    }

    #[test]
    fn deny_is_selected_first_and_enter_denies() {
        let now = Instant::now();
        let mut approver = showing(offer(1), now);
        let rows = shown_rows(&mut approver, now);
        let first_choice = rows.iter().find(|row| row.starts_with("  > ")).cloned();
        assert_eq!(first_choice.as_deref(), Some("  > Deny"));
        assert!(rows.contains(&"    Nothing is granted.".to_owned()));
        let sent = answer(&approver.key(Key::Enter, now + ARMING_DELAY));
        assert_eq!(sent.decision, ConsentDecision::Deny);
        assert!(!approver.prompting());
    }

    #[test]
    fn keys_count_only_once_the_prompt_is_armed() {
        let now = Instant::now();
        let mut approver = showing(offer(1), now);
        let early = now + Duration::from_millis(999);
        for key in [
            Key::Down,
            Key::Enter,
            Key::Escape,
            Key::Quit,
            Key::Interrupt,
        ] {
            assert_eq!(approver.key(key, early), Vec::new(), "{key:?}");
        }
        // The early Down moved nothing.
        let armed = now + ARMING_DELAY;
        assert_eq!(approver.key(Key::Down, armed), vec![Effect::Redraw]);
        // Enter on an Allow choice waits for the selection to rest.
        let resting = armed + SELECTION_ARMING_DELAY;
        assert_eq!(
            approver.key(Key::Enter, armed + Duration::from_millis(599)),
            Vec::new()
        );
        let sent = answer(&approver.key(Key::Enter, resting));
        assert_eq!(
            sent.decision,
            ConsentDecision::Grant {
                duration: GrantDuration::Once
            }
        );
    }

    #[test]
    fn type_ahead_is_thrown_away_when_a_prompt_appears() {
        let now = Instant::now();
        let (mut session, sender) = session((120, 60));
        sender
            .try_send(Input::Keys(now, vec![Key::Down, Key::Enter]))
            .unwrap();
        let effects = session.approver.offered(offer(1), GENERATION, now);
        assert_eq!(session.apply(effects, now), None);
        assert!(
            session.keys.try_recv().is_err(),
            "type-ahead was not drained"
        );
        assert_eq!(session.screen.ops[0], Op::Flush);
        assert!(matches!(session.screen.ops[1], Op::Show(_)));
        // Keys read after the flush but before the prompt arms do nothing.
        assert_eq!(session.keys(now, &[Key::Enter]), None);
        assert!(session.approver.prompting());
    }

    #[test]
    fn once_the_countdown_runs_out_nothing_is_sent() {
        let now = Instant::now();
        let mut approver = showing(offer(1), now);
        let deadline = now + Duration::from_secs(90);
        approver.key(Key::Down, now + ARMING_DELAY);
        // At the deadline no key answers, not even before the tick.
        for key in [Key::Enter, Key::Escape, Key::Interrupt] {
            assert_eq!(approver.key(key, deadline), Vec::new(), "{key:?}");
        }
        assert_eq!(
            approver.tick(deadline),
            vec![Effect::Print("Expired: no answer sent.".to_owned())]
        );
        assert!(!approver.prompting());
        assert_eq!(approver.key(Key::Enter, deadline), Vec::new());
        // Its later withdrawal is already shown.
        assert_eq!(approver.withdrawn(1, ConsentOfferEnd::Expired), Vec::new());
        // The countdown changes on screen each second before that.
        let mut approver = showing(offer(2), now);
        approver.lines(now, true);
        assert_eq!(approver.tick(now + Duration::from_millis(200)), Vec::new());
        assert_eq!(
            approver.tick(now + Duration::from_secs(1)),
            vec![Effect::Redraw]
        );
    }

    #[test]
    fn a_withdrawn_offer_closes_its_prompt_and_says_why() {
        let now = Instant::now();
        let (mut session, _sender) = session((120, 60));
        let effects = session.approver.offered(offer(1), GENERATION, now);
        session.apply(effects, now);
        assert_eq!(
            session.approver.withdrawn(2, ConsentOfferEnd::Cancelled),
            Vec::new()
        );
        let effects = session
            .approver
            .withdrawn(1, ConsentOfferEnd::AnsweredElsewhere);
        session.apply(effects, now);
        assert!(!session.approver.prompting());
        assert_eq!(
            session.screen.ops[2..],
            [
                Op::Leave,
                Op::Print(
                    "Withdrawn: answered elsewhere, in another Window or approver.".to_owned()
                )
            ]
        );
        // A new offer replaces an open one, which is gone.
        let mut approver = showing(offer(1), now);
        let effects = approver.offered(offer(2), GENERATION, now);
        assert!(matches!(
            effects.as_slice(),
            [Effect::Print(_), Effect::Open]
        ));
        let sent = answer(&approver.key(Key::Escape, now + ARMING_DELAY));
        assert_eq!(sent.offer_id, 2);
    }

    #[test]
    fn escape_q_and_ctrl_c_deny_a_prompt_and_ctrl_c_quits_without_one() {
        let now = Instant::now();
        for key in [Key::Interrupt, Key::Escape, Key::Quit] {
            let mut approver = showing(offer(1), now);
            approver.key(Key::Down, now + ARMING_DELAY);
            let sent = answer(&approver.key(key, now + ARMING_DELAY));
            assert_eq!(sent.decision, ConsentDecision::Deny, "{key:?}");
        }
        let mut idle = Approver::new(None);
        assert_eq!(idle.key(Key::Interrupt, now), vec![Effect::Quit]);
        assert_eq!(idle.key(Key::Quit, now), Vec::new());
        assert_eq!(idle.key(Key::Enter, now), Vec::new());
    }

    #[test]
    fn the_answer_echoes_the_offers_generation_id_and_nonce() {
        let now = Instant::now();
        let mut offered = offer(41);
        offered.nonce = ConsentOfferNonce::new([5; CONSENT_OFFER_NONCE_BYTES]);
        let mut approver = showing(offered, now);
        let sent = answer(&approver.key(Key::Enter, now + ARMING_DELAY));
        assert_eq!(
            sent.request(),
            Request::AnswerConsentOffer {
                daemon_generation: GENERATION,
                offer_id: 41,
                nonce: ConsentOfferNonce::new([5; CONSENT_OFFER_NONCE_BYTES]),
                decision: ConsentDecision::Deny,
            }
        );
        // An offer from another generation is never shown.
        let mut approver = Approver::new(None);
        let effects = approver.offered(offer(1), GENERATION + 1, now);
        assert!(matches!(effects.as_slice(), [Effect::Print(_)]));
        assert!(!approver.prompting());
        // Nor is an invalid one.
        let mut invalid = offer(1);
        invalid.respond_within_seconds = 0;
        assert!(matches!(
            approver.offered(invalid, GENERATION, now).as_slice(),
            [Effect::Print(_)]
        ));
        assert!(!approver.prompting());
    }

    #[test]
    fn only_the_offered_durations_are_choices_and_always_is_one_when_offered() {
        let now = Instant::now();
        let mut withheld = offer(1);
        withheld.prompt.durations[1] = duration(
            GrantDuration::WorkspaceSession,
            Some(DurationWithheld::NoPinnedIdentity),
        );
        withheld.prompt.durations.truncate(2);
        let mut approver = showing(withheld, now);
        let rows = shown_rows(&mut approver, now);
        assert!(rows.contains(&"  > Deny".to_owned()), "{rows:#?}");
        assert!(rows.contains(&"    Allow Manage once".to_owned()));
        assert!(!rows.contains(&"    Allow Manage while open".to_owned()));
        assert!(
            rows.iter()
                .any(|row| row.starts_with("    Allow Manage while open is unavailable: ")),
            "{rows:#?}"
        );
        let open = approver.open.as_ref().unwrap();
        assert_eq!(
            open.choices,
            vec![Choice::Deny, Choice::Grant(GrantDuration::Once)]
        );

        let mut approver = showing(offer(2), now);
        assert_eq!(
            approver.open.as_ref().unwrap().choices,
            vec![
                Choice::Deny,
                Choice::Grant(GrantDuration::Once),
                Choice::Grant(GrantDuration::WorkspaceSession),
                Choice::Grant(GrantDuration::Remembered),
            ]
        );
        let rows = shown_rows(&mut approver, now);
        for label in [
            "    Allow Manage once",
            "    Allow Manage while open",
            "    Always allow Manage",
        ] {
            assert!(rows.contains(&label.to_owned()), "{label} in {rows:#?}");
        }
        let mut at = now + ARMING_DELAY;
        for _ in 0..3 {
            assert_eq!(approver.key(Key::Down, at), vec![Effect::Redraw]);
            at += SELECTION_ARMING_DELAY;
        }
        assert_eq!(
            approver.key(Key::Down, at),
            Vec::new(),
            "past the last choice"
        );
        let rows = paint(&approver.lines(at, true).unwrap(), 120);
        let rows = plain(&rows);
        assert!(
            rows.contains(&"  > Always allow Manage".to_owned()),
            "{rows:#?}"
        );
        assert!(rows.iter().any(|row| {
            row.contains("Any program that runs /usr/bin/splinterm-mcp shares this access.")
        }));
        let sent = answer(&approver.key(Key::Enter, at));
        assert_eq!(
            sent.decision,
            ConsentDecision::Grant {
                duration: GrantDuration::Remembered
            }
        );
        assert_eq!(
            approver.settle(AnswerResult::Answered, at),
            vec![Effect::Print(
                "Allowed Manage always for 'Workspace'.".to_owned()
            )]
        );
    }

    #[test]
    fn a_withheld_duration_is_shown_unavailable_with_its_reason() {
        let now = Instant::now();
        let mut withheld = offer(1);
        withheld.prompt.durations[2] = duration(
            GrantDuration::Remembered,
            Some(DurationWithheld::Interpreter),
        );
        let mut approver = showing(withheld, now);
        let rows = shown_rows(&mut approver, now);
        assert!(
            rows.iter().any(|row| row
                .starts_with("    Always allow Manage is unavailable: This program runs scripts;")),
            "{rows:#?}"
        );
        assert!(!rows.contains(&"    Always allow Manage".to_owned()));
    }

    #[test]
    fn a_prompt_too_tall_for_the_terminal_only_denies() {
        let now = Instant::now();
        let (mut session, _sender) = session((120, 10));
        let effects = session.approver.offered(offer(1), GENERATION, now);
        session.apply(effects, now);
        let Some(Op::Show(rows)) = session.screen.ops.last() else {
            panic!("the prompt was not drawn");
        };
        assert!(
            plain(rows)
                .iter()
                .any(|row| row.starts_with("The terminal is too small"))
        );
        let armed = now + ARMING_DELAY;
        assert_eq!(session.keys(armed, &[Key::Down]), None);
        let resting = armed + SELECTION_ARMING_DELAY;
        assert_eq!(session.keys(resting, &[Key::Enter]), None);
        let Some(Flow::Answer(sent)) = session.keys(resting, &[Key::Escape]) else {
            panic!("Escape did not deny");
        };
        assert_eq!(sent.decision, ConsentDecision::Deny);
    }

    #[test]
    fn wide_names_and_narrow_terminals_count_the_rows_the_terminal_takes() {
        assert_eq!(super::terminal_rows(&["abc".to_owned()], 20), 1);
        assert_eq!(super::terminal_rows(&[String::new()], 20), 1);
        // Twelve wide characters take 24 cells: two rows of 20.
        let wide = format!("{BOLD}{}{RESET}", "漢".repeat(12));
        assert_eq!(super::terminal_rows(&[wide], 20), 2);
        // A terminal narrower than the prompt is ever wrapped only denies.
        let now = Instant::now();
        let (mut session, _sender) = session((MIN_COLUMNS - 1, 500));
        let effects = session.approver.offered(offer(1), GENERATION, now);
        session.apply(effects, now);
        let armed = now + ARMING_DELAY;
        session.keys(armed, &[Key::Down]);
        assert_eq!(
            session.keys(armed + SELECTION_ARMING_DELAY, &[Key::Enter]),
            None
        );
    }

    #[test]
    fn the_daemons_answer_is_reported_and_a_refused_duration_can_be_chosen_again() {
        let now = Instant::now();
        let mut approver = showing(offer(1), now);
        answer(&approver.key(Key::Enter, now + ARMING_DELAY));
        assert_eq!(
            approver.settle(AnswerResult::Answered, now),
            vec![Effect::Print(
                "Denied: no access to 'Workspace'.".to_owned()
            )]
        );
        let mut approver = showing(offer(1), now);
        answer(&approver.key(Key::Enter, now + ARMING_DELAY));
        let settled = approver.settle(AnswerResult::NotFound, now);
        let [Effect::Print(line)] = settled.as_slice() else {
            panic!("expected a line");
        };
        assert!(line.starts_with("Already answered elsewhere"), "{line}");

        let mut approver = showing(offer(1), now);
        let armed = now + ARMING_DELAY;
        approver.key(Key::Down, armed);
        answer(&approver.key(Key::Enter, armed + SELECTION_ARMING_DELAY));
        let later = armed + SELECTION_ARMING_DELAY;
        assert_eq!(
            approver.settle(AnswerResult::InvalidArgument, later),
            vec![Effect::Open]
        );
        assert!(approver.prompting());
        // It arms again before any key counts.
        assert_eq!(approver.key(Key::Enter, later), Vec::new());
        assert!(
            shown_rows(&mut approver, later)
                .iter()
                .any(|row| row.starts_with("splinterd did not accept that duration"))
        );
    }

    #[test]
    fn a_refusal_says_where_to_run_the_approver() {
        let refused =
            |code, message: &str| classify_refusal(&ProtocolError::new(code, message), false);
        for message in [
            "a consent responder must run outside every Splint",
            "a consent responder must run outside the requester",
            "this connection's process could not be verified",
        ] {
            let Attempt::Refused(guidance) = refused(ErrorCode::Unauthorized, message) else {
                panic!("{message} was not a refusal");
            };
            assert!(
                guidance.contains(
                    "not inside a Splinterm pane or a tmux or screen session started from one"
                ),
                "{guidance}"
            );
            assert!(guidance.contains("inside a Splint scope or Dojo slice"));
            assert!(!guidance.contains("cannot start"), "{guidance}");
            assert!(guidance.ends_with(&format!("(splinterd: {message})")));
        }
        let Attempt::Refused(installed) = refused(
            ErrorCode::Unauthorized,
            TRUSTED_UI_REQUIRES_INSTALLED_CLIENT,
        ) else {
            panic!("not a refusal");
        };
        assert!(installed.contains("Only the installed splinterm beside the running splinterd"));
        let Attempt::Refused(terminal) =
            refused(ErrorCode::Unauthorized, CONSENT_APPROVER_NEEDS_TERMINAL)
        else {
            panic!("not a refusal");
        };
        assert!(terminal.contains("not redirected"), "{terminal}");
        let Attempt::Refused(changed) =
            refused(ErrorCode::Unauthorized, CONSENT_RESPONDER_IDENTITY_CHANGED)
        else {
            panic!("not a refusal");
        };
        assert!(changed.starts_with(REPLACED), "{changed}");
        let Attempt::Refused(many) = refused(
            ErrorCode::ResourceLimit,
            "too many consent responders are connected",
        ) else {
            panic!("not a refusal");
        };
        assert!(many.starts_with("Too many approvers or remote Windows are connected"));
        assert_eq!(
            refused(ErrorCode::HandoffInProgress, "handing off"),
            Attempt::Busy
        );
        // The daemon's reason is escaped.
        let Attempt::Refused(escaped) = refused(ErrorCode::Unauthorized, "\x1b[2Jno") else {
            panic!("not a refusal");
        };
        assert!(!escaped.contains('\x1b'), "{escaped}");
    }

    #[test]
    fn a_reconnect_refused_for_a_replaced_client_says_to_start_again() {
        let refusal = ProtocolError::new(
            ErrorCode::Unauthorized,
            TRUSTED_UI_REQUIRES_INSTALLED_CLIENT,
        );
        let Attempt::Refused(again) = classify_refusal(&refusal, true) else {
            panic!("not a refusal");
        };
        assert!(again.starts_with(REPLACED), "{again}");
        assert!(again.contains("Run `splinterm approvals` again."));
        // Before it ever connected, the same refusal means the wrong file.
        let Attempt::Refused(first) = classify_refusal(&refusal, false) else {
            panic!("not a refusal");
        };
        assert!(first.starts_with("Only the installed splinterm"), "{first}");
    }

    #[test]
    fn only_a_refusal_of_the_answer_itself_ends_the_approver() {
        let refusal = |code| -> anyhow::Result<Response> {
            Err(response_protocol_error(ProtocolError::new(code, "no")))
        };
        assert_eq!(
            answer_outcome(Ok(Response::ConsentOfferAnswered), true),
            AnswerResult::Answered
        );
        assert_eq!(answer_outcome(Ok(Response::Pong), true), AnswerResult::Lost);
        assert_eq!(
            answer_outcome(refusal(ErrorCode::NotFound), true),
            AnswerResult::NotFound
        );
        assert_eq!(
            answer_outcome(refusal(ErrorCode::InvalidArgument), true),
            AnswerResult::InvalidArgument
        );
        assert_eq!(
            answer_outcome(refusal(ErrorCode::Unauthorized), true),
            AnswerResult::Refused("no".to_owned())
        );
        for code in [
            ErrorCode::HandoffInProgress,
            ErrorCode::Internal,
            ErrorCode::ResourceLimit,
        ] {
            assert_eq!(
                answer_outcome(refusal(code), true),
                AnswerResult::Lost,
                "{code:?}"
            );
        }
        // An error that ended the connection was about the connection, even
        // when it says Unauthorized or NotFound.
        for code in [ErrorCode::Unauthorized, ErrorCode::NotFound] {
            assert_eq!(
                answer_outcome(refusal(code), false),
                AnswerResult::Lost,
                "{code:?}"
            );
        }
        assert_eq!(
            answer_outcome(Err(anyhow::anyhow!("timed out")), false),
            AnswerResult::Lost
        );
    }

    #[test]
    fn a_refused_duration_goes_back_to_deny() {
        let now = Instant::now();
        let mut approver = showing(offer(1), now);
        let armed = now + ARMING_DELAY;
        approver.key(Key::Down, armed);
        let resting = armed + SELECTION_ARMING_DELAY;
        answer(&approver.key(Key::Enter, resting));
        approver.settle(AnswerResult::InvalidArgument, resting);
        assert_eq!(approver.open.as_ref().map(|open| open.selected), Some(0));
        let sent = answer(&approver.key(Key::Enter, resting + ARMING_DELAY));
        assert_eq!(sent.decision, ConsentDecision::Deny);
    }

    #[test]
    fn the_open_offer_sent_again_changes_nothing() {
        let now = Instant::now();
        let mut approver = showing(offer(1), now);
        let armed = now + ARMING_DELAY;
        approver.key(Key::Down, armed);
        assert_eq!(approver.offered(offer(1), GENERATION, armed), Vec::new());
        // Still armed, and still on the choice it moved to.
        let sent = answer(&approver.key(Key::Enter, armed + SELECTION_ARMING_DELAY));
        assert_eq!(
            sent.decision,
            ConsentDecision::Grant {
                duration: GrantDuration::Once
            }
        );
        // Another offer does replace it.
        let mut approver = showing(offer(1), now);
        assert_eq!(
            approver.offered(offer(2), GENERATION, armed),
            vec![
                Effect::Print("The earlier request ended: a new one replaced it.".to_owned()),
                Effect::Open
            ]
        );
    }

    #[test]
    fn wide_characters_that_wrap_early_make_the_prompt_too_tall() {
        let mut wide = offer(1);
        wide.prompt.target = ConsentTarget::Lair {
            lair_id: LairId::new(),
            lair_name: "漢".repeat(40),
        };
        let now = Instant::now();
        let mut approver = showing(wide.clone(), now);
        let painted = paint(&approver.lines(now, true).expect("a prompt"), 21);
        // Counted by characters the prompt would fit exactly; the terminal
        // takes more rows, since a wrapped row of wide characters wraps
        // again.
        assert!(super::terminal_rows(&painted, 21) > painted.len());
        let (mut session, _sender) = session((21, painted.len()));
        let effects = session.approver.offered(wide, GENERATION, now);
        session.apply(effects, now);
        assert!(!session.approver.fits);
        let armed = now + ARMING_DELAY;
        session.keys(armed, &[Key::Down]);
        assert_eq!(
            session.keys(armed + SELECTION_ARMING_DELAY, &[Key::Enter]),
            None
        );
    }

    #[test]
    fn enter_on_allow_looks_at_the_terminal_size_again() {
        let now = Instant::now();
        let (mut session, _sender) = session((120, 60));
        let effects = session.approver.offered(offer(1), GENERATION, now);
        session.apply(effects, now);
        assert!(session.approver.fits);
        let armed = now + ARMING_DELAY;
        session.keys(armed, &[Key::Down]);
        // The terminal shrinks, and Enter comes before the next tick.
        session.screen.size = (120, 10);
        assert_eq!(
            session.keys(armed + SELECTION_ARMING_DELAY, &[Key::Enter]),
            None
        );
        assert!(!session.approver.fits);
        let Some(Op::Show(rows)) = session.screen.ops.last() else {
            panic!("the prompt was not drawn again");
        };
        assert!(
            plain(rows)
                .iter()
                .any(|row| row.starts_with("The terminal is too small"))
        );
    }

    #[test]
    fn pasted_text_is_never_read_as_keys() {
        assert_eq!(
            decode_keys(b"a\x1b[200~\r\x1b[B\x03q\x1b[201~\r"),
            [Key::Other, Key::Enter]
        );
        // Markers cut across reads.
        let mut decoder = KeyDecoder::default();
        assert_eq!(decoder.decode(b"\x1b[20"), []);
        assert_eq!(decoder.decode(b"0~\r\r\x1b[2"), []);
        assert_eq!(decoder.decode(b"01"), []);
        assert_eq!(decoder.decode(b"~\x1b[B"), [Key::Down]);
        // Escape still answers at once.
        assert_eq!(decoder.decode(b"\x1b"), [Key::Escape]);
    }

    #[test]
    fn tmux_or_screen_gets_a_warning_and_runs() {
        assert_eq!(multiplexer_warning(false, false), None);
        for (tmux, screen) in [(true, false), (false, true), (true, true)] {
            assert_eq!(
                multiplexer_warning(tmux, screen),
                Some(
                    "Running inside tmux/screen: any program you run can send keys to this approver. Prefer a plain SSH login or terminal."
                )
            );
        }
    }

    #[test]
    fn keys_decode_from_raw_terminal_input() {
        assert_eq!(
            decode_keys(b"\x1b[A\x1b[B\x1bOA\x1bOBkj\r\n"),
            [
                Key::Up,
                Key::Down,
                Key::Up,
                Key::Down,
                Key::Up,
                Key::Down,
                Key::Enter,
                Key::Enter
            ]
        );
        assert_eq!(decode_keys(b"\x1b"), [Key::Escape]);
        assert_eq!(decode_keys(b"q\x03"), [Key::Quit, Key::Interrupt]);
        // A modified arrow, a function key, Alt with a key, and other
        // letters answer nothing.
        assert_eq!(
            decode_keys(b"\x1b[1;5A\x1b[15~\x1bxgy"),
            [Key::Other, Key::Other, Key::Other, Key::Other, Key::Other]
        );
        // A sequence cut short reads as one key, never as letters.
        assert_eq!(decode_keys(b"\x1b[1;"), [Key::Other]);
    }
}
