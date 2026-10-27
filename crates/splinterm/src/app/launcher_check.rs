//! `splinterm launcher-check`: the desktop launcher's daemon state boundary
//! (Plan 0037 Milestone 4).
//!
//! `splinterm-xdg-terminal-exec` runs this hidden command before it opens
//! anything and only acts on its exit status; it never infers safety from a
//! failed `ping`. The command connects as the trusted local client and reads
//! the running daemon's dry-run handoff report, which carries the daemon's
//! build identity and its exact live-Splint count. It compares that identity
//! with the one compiled into this client, which is the identity of the
//! daemon/client pair it was installed with.
//!
//! | Daemon state | Result |
//! | --- | --- |
//! | none is listening | exit 10: the launcher starts the service |
//! | this build | exit 0: open the Window |
//! | another build that reports no live Splint | exit 11: restart without asking |
//! | another build with live Splints | ask; exit 11 only on an explicit confirmation |
//! | unreadable (another protocol such as 0.1.x, no answer, a bad answer) | ask, with a best-effort count |
//! | unreadable, but the process runs the installed `splinterd` | exit 1: busy, try again |
//! | unreadable, and this client is not the `splinterm` beside it | exit 1: untrusted |
//! | refuses this client as untrusted | exit 1 |
//!
//! The process behind the socket is identified by `SO_PEERCRED` and the
//! device and inode of its `/proc/<pid>/exe`, which needs no cooperation from
//! the daemon. A restart (exit 11) is returned only while that process is
//! `splinterd.service`'s main process, so the restart ends exactly the daemon
//! that was reported idle or confirmed.
//!
//! "Not now", closing the dialog, no answer, or no way to ask exits 3, and the
//! launcher opens nothing. `--no-ask` (unattended launches: an explicit app
//! ID, such as the Omarchy screensaver's, or a command) turns every question
//! into exit 3; an idle restart stays automatic. After the launcher started or
//! restarted the service (`--after start|restart`), the command waits up to
//! five seconds for the socket; after a restart it never asks or restarts
//! again. Live upgrade is never started from here (automatic upgrade on launch
//! is deferred, Plan 0061); when the daemon reports it possible, the question
//! points to `splinterm upgrade`.

use std::{
    fs, io,
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
    sync::mpsc as std_mpsc,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use splinterd::dialog_shortcut::DialogShortcut;
use splinterm::automation::{Connection, protocol_error, socket_path};
use splinterm::frontend::dialog_text::text_rows;
use splinterm::{DialogLine, DialogStyle, DialogText, TrustedConsentUi, WindowOptions};
use splinterm_protocol::{ErrorCode, HandoffReport};
use tokio::net::UnixStream;

use super::{
    consent::trusted_label,
    local_service::user_systemctl_output,
    upgrade::reason_line,
    upgrade_dialog::{
        CAPABILITY_BYTES, DialogAnswer, DialogPrompt, MAX_DIALOG_COLUMNS, MIN_DIALOG_ROWS,
        ask_dialog_child, dialog_capability, question_identity, run_dialog_client,
    },
};

/// The hidden subcommand the restart dialog child runs.
pub(in crate::app) const RESTART_DIALOG_COMMAND: &str = "restart-dialog";

/// How long one probe may take to connect, shake hands, and read the report.
const ANSWER_BOUND: Duration = Duration::from_secs(5);

/// How long the check waits for the socket after the launcher started or
/// restarted the service.
const SOCKET_WAIT: Duration = Duration::from_secs(5);

const SOCKET_POLL: Duration = Duration::from_millis(50);

/// The pause before the one retry of a daemon that did not answer usefully.
const RETRY_PAUSE: Duration = Duration::from_millis(250);

/// How long the restart question stays open before it means "Not now".
const RESPOND_WITHIN: Duration = Duration::from_secs(120);

/// Probes per check. A confirmed restart re-reads the daemon first and asks
/// again if what it would end changed, so this bounds the questions too.
const MAX_PROBES: usize = 4;

/// Width the question text wraps at, below the dialog's column clamp.
const WRAP_COLUMNS: usize = 76;

/// Most lines one blocking reason section may take in the question.
const MAX_REASON_LINES: usize = 10;

/// Most lines a restart dialog shows, and most rows they take at the
/// widest dialog.
const MAX_RESTART_DIALOG_LINES: usize = 48;

/// The longest restart dialog line, in characters: the longest blocking
/// reason with its bullet.
const MAX_DIALOG_LINE_CHARS: usize = 1100;

/// The deepest hanging indent a restart dialog line asks for.
const MAX_DIALOG_INDENT: usize = 16;

/// Splint workload scopes, named `splinterm-splint<id>-i<incarnation>.scope`
/// by every release since 0.1.
const SPLINT_SCOPE_PREFIX: &str = "splinterm-splint";

/// What the launcher just did before running the check again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(in crate::app) enum LauncherStage {
    /// It started `splinterd.service` because no daemon was listening.
    Start,
    /// It restarted `splinterd.service` after an idle report or a confirmation.
    Restart,
}

/// `splinterm launcher-check` exit statuses, which the launcher script acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum LauncherExit {
    /// The running daemon is this build: open the Window.
    Ready,
    /// The check failed and said why. Nothing was started or restarted.
    Error,
    /// Not now: declined, closed, unanswered, unattended, or nothing could
    /// ask. Nothing opens and nothing changed.
    NotNow,
    /// No daemon is listening: start the service.
    StartService,
    /// Restart the service: the daemon proved it runs no live Splint, or the
    /// person confirmed the exact restart, and it is the service's main
    /// process.
    RestartService,
}

impl LauncherExit {
    pub(in crate::app) const fn code(self) -> i32 {
        match self {
            Self::Ready => 0,
            Self::Error => 1,
            Self::NotNow => 3,
            Self::StartService => 10,
            Self::RestartService => 11,
        }
    }
}

/// Why the running daemon could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum Unreadable {
    /// It refused this client's protocol version, or could not parse its
    /// greeting: an older release such as 0.1.x, or another build whose
    /// private protocol differs.
    OtherProtocol,
    /// It refused the connection as over a limit.
    Busy,
    /// No complete answer within [`ANSWER_BOUND`].
    NoAnswer,
    /// It answered with something other than a usable handshake and report,
    /// or hung up.
    InvalidAnswer,
}

impl Unreadable {
    /// Whether one more try may get an answer.
    const fn retried(self) -> bool {
        !matches!(self, Self::OtherProtocol)
    }
}

/// The process behind the socket, identified without its cooperation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) struct DaemonPeer {
    /// From `SO_PEERCRED`.
    pub(in crate::app) pid: u32,
    /// It executes the installed `splinterd` beside this client (same device
    /// and inode), so it is the installed build.
    pub(in crate::app) runs_installed_daemon: bool,
    /// This client is the `splinterm` beside the daemon's executable (same
    /// device and inode), so this client may speak for it.
    pub(in crate::app) client_is_its_sibling: bool,
}

/// What one probe of the daemon socket found.
#[derive(Debug)]
pub(in crate::app) enum Probe {
    /// Nothing listens on the socket (no socket, or connection refused).
    NotRunning,
    /// The socket could not be used for another local reason.
    ConnectFailed(String),
    /// The daemon refused this client as not the installed trusted client.
    Untrusted,
    Unreadable {
        reason: Unreadable,
        /// `None` when the process could not be identified.
        peer: Option<DaemonPeer>,
        /// Running Splint workload scopes, when they could be counted.
        workload_scopes: Option<usize>,
    },
    Report {
        report: Box<HandoffReport>,
        /// From `SO_PEERCRED`; `None` from another PID namespace.
        daemon_pid: Option<u32>,
    },
}

/// A file's identity: device and inode.
pub(in crate::app) type FileId = (u64, u64);

/// The build this client was installed with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::app) struct InstalledBuild {
    pub(in crate::app) identity: [u8; splinterm_protocol::HANDOFF_BUILD_IDENTITY_BYTES],
    pub(in crate::app) version: String,
    /// This client's executable.
    pub(in crate::app) client: Option<FileId>,
    /// The `splinterd` beside this client's executable.
    pub(in crate::app) daemon: Option<FileId>,
}

/// What the running daemon is, as the question describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::app) enum RunningDaemon {
    Build { version: String, generation: u64 },
    Unreadable(Unreadable),
}

/// How many shells a restart ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum ShellCount {
    /// The daemon's own count.
    Exact(usize),
    /// Counted from running Splint workload scopes: best effort.
    About(usize),
    /// Not countable.
    Unknown,
}

/// Whether live upgrade could keep the shells instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::app) enum LiveUpgrade {
    /// The daemon reports nothing blocking it.
    Possible,
    /// One sentence per reason the daemon reported.
    Blocked(Vec<String>),
    /// The daemon could not be asked.
    Unknown,
}

/// The restart question: what runs, what is installed, and what ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::app) struct RestartQuestion {
    pub(in crate::app) running: RunningDaemon,
    pub(in crate::app) installed_version: String,
    pub(in crate::app) shells: ShellCount,
    pub(in crate::app) live_upgrade: LiveUpgrade,
    /// The daemon process the question is about; a restart is bound to it.
    pub(in crate::app) daemon_pid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::app) enum Decision {
    Launch,
    StartService,
    /// The daemon process `daemon_pid` reported zero live Splints.
    RestartIdle {
        daemon_pid: u32,
    },
    Ask(RestartQuestion),
    Fail(Vec<String>),
}

/// Decides what the launcher does from one probe. Pure: every state the
/// launcher can meet is decided here and unit tested.
///
/// Only a report from the daemon itself proves anything: a matching build
/// launches, and only zero live Splints restarts without asking. Anything
/// that cannot be read asks, because it may still run shells, unless its
/// process is the installed build (busy, not incompatible) or this client
/// is not the one beside it (not trusted to ask). Nothing restarts after a
/// restart, and no restart is offered for a process that cannot be named.
pub(in crate::app) fn decide(
    probe: &Probe,
    installed: &InstalledBuild,
    after: Option<LauncherStage>,
) -> Decision {
    match probe {
        Probe::Report { report, .. } if report.build.identity == installed.identity => {
            Decision::Launch
        }
        Probe::NotRunning => match after {
            None => Decision::StartService,
            Some(_) => Decision::Fail(vec![format!(
                "splinterm: splinterd did not start listening within {} seconds; inspect `systemctl --user status splinterd.service`.",
                SOCKET_WAIT.as_secs()
            )]),
        },
        Probe::ConnectFailed(message) => Decision::Fail(vec![format!(
            "splinterm: cannot connect to splinterd: {}. Nothing was started or restarted.",
            trusted_label(message, 512)
        )]),
        Probe::Untrusted => Decision::Fail(untrusted_lines()),
        Probe::Unreadable {
            reason,
            peer: Some(peer),
            ..
        } if peer.runs_installed_daemon => Decision::Fail(busy_lines(*reason)),
        _ if after == Some(LauncherStage::Restart) => {
            Decision::Fail(still_not_installed_lines(probe, installed))
        }
        Probe::Report {
            daemon_pid: None, ..
        }
        | Probe::Unreadable { peer: None, .. } => Decision::Fail(unidentified_lines()),
        Probe::Unreadable {
            peer: Some(peer), ..
        } if !peer.client_is_its_sibling => Decision::Fail(untrusted_lines()),
        Probe::Report {
            report,
            daemon_pid: Some(daemon_pid),
        } if report.live_splints == 0 => Decision::RestartIdle {
            daemon_pid: *daemon_pid,
        },
        Probe::Report {
            report,
            daemon_pid: Some(daemon_pid),
        } => Decision::Ask(RestartQuestion {
            running: RunningDaemon::Build {
                version: report.build.version.clone(),
                generation: report.daemon_generation,
            },
            installed_version: installed.version.clone(),
            shells: ShellCount::Exact(report.live_splints),
            live_upgrade: if report.blocking_reasons.is_empty() {
                LiveUpgrade::Possible
            } else {
                LiveUpgrade::Blocked(
                    report
                        .blocking_reasons
                        .iter()
                        .map(|reason| reason_line(reason, report.build.provenance))
                        .collect(),
                )
            },
            daemon_pid: *daemon_pid,
        }),
        Probe::Unreadable {
            reason,
            peer: Some(peer),
            workload_scopes,
        } => Decision::Ask(RestartQuestion {
            running: RunningDaemon::Unreadable(*reason),
            installed_version: installed.version.clone(),
            // Zero scopes is no proof of idleness: a daemon may run its
            // shells without workload scopes.
            shells: match workload_scopes {
                Some(count) if *count > 0 => ShellCount::About(*count),
                _ => ShellCount::Unknown,
            },
            live_upgrade: LiveUpgrade::Unknown,
            daemon_pid: peer.pid,
        }),
    }
}

fn untrusted_lines() -> Vec<String> {
    vec![
        "splinterm: splinterd answers the launcher only from the installed splinterm beside the running splinterd (on packaged systems /usr/bin/splinterm), matched by device and inode.".to_owned(),
        "Run that installed client, not a development build or a copy earlier on PATH; check with `command -v splinterm`. Nothing was started or restarted.".to_owned(),
    ]
}

fn busy_lines(reason: Unreadable) -> Vec<String> {
    vec![format!(
        "splinterm: the running splinterd is the installed build, but {}; it may be busy. Try again shortly. Nothing was restarted.",
        unreadable_phrase(reason)
    )]
}

fn unidentified_lines() -> Vec<String> {
    vec![
        "splinterm: the process behind the splinterd socket could not be identified, so it is not restarted. Inspect `systemctl --user status splinterd.service`.".to_owned(),
    ]
}

fn not_the_service_lines(daemon_pid: u32, main_pid: Option<u32>) -> Vec<String> {
    let main = main_pid.map_or_else(|| "none".to_owned(), |pid| format!("PID {pid}"));
    vec![format!(
        "splinterm: the daemon that answered (PID {daemon_pid}) is not splinterd.service's main process ({main}), so restarting the service would not replace it. Nothing was restarted."
    )]
}

fn still_not_installed_lines(probe: &Probe, installed: &InstalledBuild) -> Vec<String> {
    let found = match probe {
        Probe::Report { report, .. } => format!(
            "it serves Splinterm {} (generation {})",
            report.build.version, report.daemon_generation
        ),
        Probe::Unreadable { reason, .. } => unreadable_phrase(*reason).to_owned(),
        _ => "it could not be read".to_owned(),
    };
    vec![
        format!(
            "splinterm: splinterd.service was restarted, but it still does not serve this client's build (Splinterm {}): {found}.",
            installed.version
        ),
        "It is not restarted again. Check `command -v splinterm` and `systemctl --user cat splinterd.service`.".to_owned(),
    ]
}

const fn unreadable_phrase(reason: Unreadable) -> &'static str {
    match reason {
        Unreadable::OtherProtocol => "it does not speak this client's protocol",
        Unreadable::Busy => "it refused the connection as over a limit",
        Unreadable::NoAnswer => "it did not answer in time",
        Unreadable::InvalidAnswer => "it did not give a usable answer",
    }
}

/// "4 running shells", "about 4 running shells", or "all running shells".
pub(in crate::app) fn shells_phrase(count: ShellCount) -> String {
    match count {
        ShellCount::Exact(1) => "1 running shell".to_owned(),
        ShellCount::Exact(count) => format!("{count} running shells"),
        ShellCount::About(1) => "about 1 running shell".to_owned(),
        ShellCount::About(count) => format!("about {count} running shells"),
        ShellCount::Unknown => "all running shells".to_owned(),
    }
}

/// The confirming action's label: the green button's, naming what it ends.
pub(in crate::app) fn confirm_label(count: ShellCount) -> String {
    match count {
        ShellCount::Exact(1) => "Restart now, end 1 shell".to_owned(),
        ShellCount::Exact(count) => format!("Restart now, end {count} shells"),
        ShellCount::About(1) => "Restart now, end about 1 shell".to_owned(),
        ShellCount::About(count) => format!("Restart now, end about {count} shells"),
        ShellCount::Unknown => "Restart now, end all running shells".to_owned(),
    }
}

/// A paragraph of the question, wrapped at [`WRAP_COLUMNS`] at most.
fn prose(text: impl Into<String>) -> DialogLine {
    DialogLine::new(text).with_wrap_at(WRAP_COLUMNS)
}

/// `line` as the terminal note prints it: wrapped at its own width with its
/// hanging indent, and never inside a word, so a long path or unit name
/// stays whole (on a row of its own) for copying.
fn terminal_rows(line: &DialogLine) -> Vec<String> {
    let Some(width) = line.wrap_at else {
        return vec![line.text.clone()];
    };
    let leading = line.text.len() - line.text.trim_start_matches(' ').len();
    let mut rows = Vec::new();
    let mut row = " ".repeat(leading);
    let mut has_word = false;
    for word in line.text.split_whitespace() {
        if has_word && row.chars().count() + 1 + word.chars().count() > width {
            rows.push(std::mem::take(&mut row));
            row = " ".repeat(line.indent);
            has_word = false;
        }
        if has_word {
            row.push(' ');
        }
        row.push_str(word);
        has_word = true;
    }
    if has_word || rows.is_empty() {
        if !has_word {
            row.clear();
        }
        rows.push(row);
    }
    rows
}

/// One blocking reason as its bulleted paragraph.
fn reason_paragraph(text: &str) -> DialogLine {
    DialogLine::new(format!("  • {text}"))
        .with_indent(6)
        .with_wrap_at(WRAP_COLUMNS)
}

/// The blocking reasons, at most [`MAX_REASON_LINES`] rows as the question
/// is first shown. A longer list keeps the rows that fit, cut at a word,
/// and ends by pointing to the full report.
fn reason_lines(reasons: &[String]) -> Vec<DialogLine> {
    let paragraphs: Vec<_> = reasons
        .iter()
        .map(|reason| {
            reason_paragraph(
                &trusted_label(reason, 1024)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        })
        .collect();
    let rows = |line: &DialogLine| terminal_rows(line).len();
    if paragraphs.iter().map(rows).sum::<usize>() <= MAX_REASON_LINES {
        return paragraphs;
    }
    let mut room = MAX_REASON_LINES - 1;
    let mut kept = Vec::new();
    for paragraph in paragraphs {
        if room == 0 {
            break;
        }
        let wrapped = terminal_rows(&paragraph);
        if wrapped.len() <= room {
            room -= wrapped.len();
            kept.push(paragraph);
            continue;
        }
        let words: Vec<_> = wrapped[..room].iter().map(|row| row.trim()).collect();
        kept.push(reason_paragraph(
            words.join(" ").trim_start_matches("• ").trim_start(),
        ));
        room = 0;
    }
    kept.push(reason_paragraph(
        "… run `splinterm upgrade` for the full report.",
    ));
    kept
}

/// The question's explanation as the terminal note prints it: its heading,
/// then the [`question_body`] the dialog shows.
pub(in crate::app) fn question_lines(question: &RestartQuestion) -> Vec<DialogLine> {
    let mut lines = vec![
        DialogLine::new("TRUSTED SPLINTERM UPGRADE QUESTION"),
        DialogLine::blank(),
    ];
    lines.extend(question_body(question));
    lines
}

/// The question's explanation, shared by the dialog and the terminal note:
/// everything above the deadline and the answer hint, as paragraphs.
fn question_body(question: &RestartQuestion) -> Vec<DialogLine> {
    let mut lines = Vec::new();
    let installed = trusted_label(&question.installed_version, 64);
    match &question.running {
        RunningDaemon::Build {
            version,
            generation,
        } => {
            lines.push(
                DialogLine::new("The running Splinterm daemon is not the installed build.")
                    .with_style(DialogStyle::Strong),
            );
            lines.push(
                DialogLine::new(format!(
                    "  Running:   Splinterm {}, generation {generation}",
                    trusted_label(version, 64)
                ))
                .with_indent(13)
                .with_lead(13),
            );
            lines.push(
                DialogLine::new(format!("  Installed: Splinterm {installed}"))
                    .with_indent(13)
                    .with_lead(13),
            );
            lines.push(prose(format!(
                "It still has {}, so it is not restarted without asking.",
                shells_phrase(question.shells)
            )));
        }
        RunningDaemon::Unreadable(reason) => {
            let explanation = match reason {
                Unreadable::OtherProtocol => format!(
                    "The running Splinterm daemon does not speak this client's protocol. It is an older release, such as 0.1.x, or another incompatible build, so this client (Splinterm {installed}) cannot ask it for its shells or ask it to hand them over."
                ),
                Unreadable::Busy => format!(
                    "The running Splinterm daemon refused the connection as over a limit, so it cannot be asked for its shells. Installed: Splinterm {installed}."
                ),
                Unreadable::NoAnswer => format!(
                    "The running Splinterm daemon did not answer within {} seconds, so it cannot be asked for its shells. If a live upgrade is in progress, choose Not now and try again shortly. Installed: Splinterm {installed}.",
                    ANSWER_BOUND.as_secs()
                ),
                Unreadable::InvalidAnswer => format!(
                    "The running Splinterm daemon did not give a usable answer, so it cannot be asked for its shells. If a live upgrade is in progress, choose Not now and try again shortly. Installed: Splinterm {installed}."
                ),
            };
            lines.push(prose(explanation));
            if let ShellCount::About(count) = question.shells {
                lines.push(prose(format!(
                    "About {count} shell{} appear to be running: a best-effort count of its running workload units.",
                    if count == 1 { "" } else { "s" }
                )));
            }
        }
    }
    lines.push(DialogLine::blank());
    let ends = match question.shells {
        // A best-effort count can be short: say what a restart really ends.
        ShellCount::About(_) => "every shell it runs".to_owned(),
        count => shells_phrase(count),
    };
    lines.push(prose(format!(
        "Restarting it starts the installed build and ends {ends} and the programs in them. Open Splinterm Windows must be reopened afterwards."
    )));
    match &question.live_upgrade {
        LiveUpgrade::Possible => {
            lines.push(DialogLine::blank());
            lines.push(prose(
                "Live upgrade can keep them running instead: choose Not now, then run `splinterm upgrade` in a terminal.",
            ));
        }
        LiveUpgrade::Blocked(reasons) => {
            lines.push(DialogLine::blank());
            lines.push(DialogLine::new("Live upgrade is not available now:"));
            lines.extend(reason_lines(reasons));
        }
        LiveUpgrade::Unknown => {}
    }
    lines.push(DialogLine::blank());
    lines.push(DialogLine::new(
        "Not now opens nothing and changes nothing.",
    ));
    lines
}

/// The question's explanation as the terminal note prints it, wrapped as
/// the dialog first shows it.
pub(in crate::app) fn question_text(question: &RestartQuestion) -> Vec<String> {
    question_lines(question)
        .iter()
        .flat_map(terminal_rows)
        .collect()
}

/// The whole dialog: the frame's title, the question, the deadline, how to
/// answer, and the two buttons' labels, the green one naming what a restart
/// ends. The frame's legend says how to engage the keyboard.
pub(in crate::app) fn dialog_lines(question: &RestartQuestion) -> Vec<DialogLine> {
    let mut lines = vec![DialogLine::title("restart splinterd?")];
    lines.extend(question_body(question));
    lines.push(DialogLine::new(format!(
        "Closing this dialog, or no answer within {} seconds, means Not now.",
        RESPOND_WITHIN.as_secs()
    )));
    lines.push(DialogLine::blank());
    lines.push(
        DialogLine::hint(
            "R or the green Restart button (right) restarts. Enter, Escape, D, or the red Not now button (left) answer Not now.",
        )
        .with_wrap_at(WRAP_COLUMNS),
    );
    lines.push(DialogLine::deny("Not now"));
    lines.push(DialogLine::allow(confirm_label(question.shells)));
    lines
}

/// What the launcher prints, before the [`not_now_lines`], when nobody
/// answered the dialog in time.
pub(in crate::app) fn no_answer_line() -> String {
    format!(
        "splinterm: no answer within {} seconds, so nothing was restarted.",
        RESPOND_WITHIN.as_secs()
    )
}

/// What the launcher prints when the answer is "Not now".
pub(in crate::app) fn not_now_lines(question: &RestartQuestion) -> Vec<String> {
    let mut lines =
        vec!["Not now: nothing was opened, and the daemon and its shells keep running.".to_owned()];
    if question.live_upgrade == LiveUpgrade::Possible {
        lines.push(
            "Run `splinterm upgrade` to move them to the installed build without ending them."
                .to_owned(),
        );
    }
    lines
}

/// What an unattended launch prints instead of asking.
pub(in crate::app) fn unattended_lines(question: &RestartQuestion) -> Vec<String> {
    vec![
        format!(
            "splinterm: the running daemon is not the installed build and may end {} on restart, and this unattended launch does not ask. Nothing was opened or restarted.",
            shells_phrase(question.shells)
        ),
        "Open Splinterm from the desktop to choose, or run `splinterm upgrade`.".to_owned(),
    ]
}

/// Counts running Splint workload scopes in `systemctl --user list-units
/// --plain --no-legend` output.
pub(in crate::app) fn workload_scope_count(listing: &str) -> usize {
    listing
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter_map(|unit| unit.strip_suffix(".scope"))
        .filter(|name| name.starts_with(SPLINT_SCOPE_PREFIX))
        .count()
}

/// The `splinterm` beside the executable `/proc/<pid>/exe` names, whose
/// target ends in " (deleted)" once a package replaced it.
pub(in crate::app) fn sibling_client_path(executable: &Path) -> Option<PathBuf> {
    let executable = executable.to_str()?;
    let executable = Path::new(executable.strip_suffix(" (deleted)").unwrap_or(executable));
    // A sealed memfd image (`/memfd:...`, after a live upgrade) has no
    // directory to look beside.
    if !executable.is_absolute() || executable.to_str()?.starts_with("/memfd:") {
        return None;
    }
    Some(executable.parent()?.join("splinterm"))
}

/// Parses `systemctl show -p MainPID --value`; 0 means no main process.
pub(in crate::app) fn parse_main_pid(output: &str) -> Option<u32> {
    output.trim().parse().ok().filter(|&pid| pid > 0)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RestartDialogPrompt {
    capability: Vec<u8>,
    parent_pid: u32,
    /// The question's paragraphs, which the dialog wraps to its width.
    lines: Vec<DialogLine>,
    /// How long the launcher waits for an answer, which the frame counts
    /// down; 0, from a launcher that did not send it, shows no countdown.
    /// Display only: the launcher's own deadline still decides.
    #[serde(default)]
    respond_within_seconds: u64,
}

impl DialogPrompt for RestartDialogPrompt {
    fn capability(&self) -> &[u8] {
        &self.capability
    }

    fn parent_pid(&self) -> u32 {
        self.parent_pid
    }

    fn validate(&self) -> Result<()> {
        let valid = self.capability.len() == CAPABILITY_BYTES
            && self.parent_pid > 0
            && !self.lines.is_empty()
            && self.lines.len() <= MAX_RESTART_DIALOG_LINES
            && self.lines.iter().all(|line| {
                line.text.chars().count() <= MAX_DIALOG_LINE_CHARS
                    && line.indent <= MAX_DIALOG_INDENT
                    && line.wrap_at.is_none_or(|width| width <= MAX_DIALOG_COLUMNS)
            })
            && text_rows(
                &[DialogText {
                    lines: self.lines.clone(),
                    minimum_rows: 0,
                }],
                MAX_DIALOG_COLUMNS,
            ) <= MAX_RESTART_DIALOG_LINES;
        if !valid {
            anyhow::bail!("invalid restart dialog prompt");
        }
        Ok(())
    }

    fn window_options(
        &self,
        decision: std_mpsc::Sender<bool>,
        shortcut: Option<DialogShortcut>,
    ) -> WindowOptions {
        let lines = self
            .lines
            .iter()
            .map(|line| DialogLine {
                text: trusted_label(&line.text, MAX_DIALOG_LINE_CHARS),
                ..line.clone()
            })
            .collect::<Vec<_>>();
        TrustedConsentUi::restart(decision)
            .with_shortcut(shortcut)
            .with_respond_within(Duration::from_secs(self.respond_within_seconds))
            .with_text(DialogText {
                lines,
                minimum_rows: MIN_DIALOG_ROWS,
            })
            .with_identity(question_identity())
            .into_window_options()
    }
}

/// The hidden `restart-dialog` child: shows one restart question and
/// answers on stdout. Closing the Window answers "Not now".
pub(in crate::app) fn run_restart_dialog_client(
    shortcut: Option<DialogShortcut>,
    theme: splinterm::config::ResolvedTheme,
) -> Result<()> {
    run_dialog_client::<RestartDialogPrompt>(shortcut, theme)
}

/// Runs the check and exits with its [`LauncherExit`].
pub(in crate::app) async fn run(after: Option<LauncherStage>, no_ask: bool) -> Result<()> {
    let exit = check(after, no_ask).await;
    std::process::exit(exit.code());
}

async fn check(after: Option<LauncherStage>, no_ask: bool) -> LauncherExit {
    let installed = match installed_build() {
        Ok(installed) => installed,
        Err(error) => {
            eprintln!("splinterm: {error:#}; nothing was started or restarted.");
            return LauncherExit::Error;
        }
    };
    let wait = if after.is_some() {
        SOCKET_WAIT
    } else {
        Duration::ZERO
    };
    let mut confirmed: Option<RestartQuestion> = None;
    let mut unowned = None;
    for _ in 0..MAX_PROBES {
        let daemon_pid = match decide(&probe(wait, &installed).await, &installed, after) {
            Decision::Launch => return LauncherExit::Ready,
            Decision::StartService => return LauncherExit::StartService,
            Decision::Fail(lines) => {
                for line in lines {
                    eprintln!("{line}");
                }
                return LauncherExit::Error;
            }
            Decision::RestartIdle { daemon_pid } => daemon_pid,
            // Re-read after a confirmation: restart only if what ends is
            // still exactly what the person confirmed, in the same process.
            Decision::Ask(question) if confirmed.as_ref() == Some(&question) => question.daemon_pid,
            Decision::Ask(question) if no_ask => {
                for line in unattended_lines(&question) {
                    eprintln!("{line}");
                }
                return LauncherExit::NotNow;
            }
            Decision::Ask(question) => {
                if !ask(&question).await {
                    for line in not_now_lines(&question) {
                        eprintln!("{line}");
                    }
                    return LauncherExit::NotNow;
                }
                confirmed = Some(question);
                continue;
            }
        };
        // The restart must end exactly the process reported idle or
        // confirmed: the service's main process, read now.
        let main_pid =
            user_systemctl_output(&["show", "-p", "MainPID", "--value", "splinterd.service"])
                .as_deref()
                .and_then(parse_main_pid);
        if main_pid == Some(daemon_pid) {
            return LauncherExit::RestartService;
        }
        unowned = Some((daemon_pid, main_pid));
        tokio::time::sleep(RETRY_PAUSE).await;
    }
    if let Some((daemon_pid, main_pid)) = unowned {
        for line in not_the_service_lines(daemon_pid, main_pid) {
            eprintln!("{line}");
        }
        return LauncherExit::Error;
    }
    eprintln!("splinterm: the daemon kept changing while asking, so nothing was restarted.");
    LauncherExit::NotNow
}

fn file_id(path: &Path) -> Option<FileId> {
    fs::metadata(path)
        .ok()
        .map(|metadata| (metadata.dev(), metadata.ino()))
}

fn installed_build() -> Result<InstalledBuild> {
    let generation = splinterd::handoff_preflight::compiled_handoff_generation()
        .map_err(|_| anyhow!("this client's build identity is unavailable"))?;
    let own = Path::new("/proc/self/exe");
    let daemon = fs::read_link(own)
        .ok()
        .and_then(|path| sibling_client_path(&path))
        .and_then(|client| file_id(&client.with_file_name("splinterd")));
    Ok(InstalledBuild {
        identity: generation.build_identity.as_bytes(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        client: file_id(own),
        daemon,
    })
}

/// Identifies the process `pid` behind the socket against this client.
fn daemon_peer(pid: u32, installed: &InstalledBuild) -> DaemonPeer {
    let executable = PathBuf::from(format!("/proc/{pid}/exe"));
    let running = file_id(&executable);
    let sibling = fs::read_link(&executable)
        .ok()
        .and_then(|path| sibling_client_path(&path))
        .and_then(|client| file_id(&client));
    DaemonPeer {
        pid,
        runs_installed_daemon: running.is_some() && running == installed.daemon,
        client_is_its_sibling: sibling.is_some() && sibling == installed.client,
    }
}

fn workload_scopes() -> Option<usize> {
    user_systemctl_output(&[
        "list-units",
        "--type=scope",
        "--state=active",
        "--plain",
        "--no-legend",
        "--no-pager",
        "splinterm-splint*.scope",
    ])
    .map(|listing| workload_scope_count(&listing))
}

/// Probes until something answers other than "not running", for at most
/// `wait`, and tries once more when a daemon did not answer usefully.
async fn probe(wait: Duration, installed: &InstalledBuild) -> Probe {
    let started = Instant::now();
    let mut retried = false;
    loop {
        let probe = probe_once(installed).await;
        match &probe {
            Probe::NotRunning if started.elapsed() < wait => {
                tokio::time::sleep(SOCKET_POLL).await;
            }
            Probe::Unreadable { reason, peer, .. }
                if !retried
                    && reason.retried()
                    && !peer.is_some_and(|peer| peer.runs_installed_daemon) =>
            {
                retried = true;
                tokio::time::sleep(RETRY_PAUSE).await;
            }
            _ => return probe,
        }
    }
}

/// A handshake or report that did not produce a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum Refusal {
    Untrusted,
    Unreadable(Unreadable),
}

async fn probe_once(installed: &InstalledBuild) -> Probe {
    let socket = match socket_path() {
        Ok(socket) => socket,
        Err(error) => return Probe::ConnectFailed(format!("{error:#}")),
    };
    let stream = match tokio::time::timeout(ANSWER_BOUND, UnixStream::connect(&socket)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => return classify_socket_error(&error),
        Err(_) => {
            return Probe::Unreadable {
                reason: Unreadable::NoAnswer,
                peer: None,
                workload_scopes: workload_scopes(),
            };
        }
    };
    // The listener's credentials, recorded at connect time, so they name the
    // daemon even when it closes the connection at once.
    let daemon_pid = stream
        .peer_cred()
        .ok()
        .and_then(|credentials| credentials.pid())
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|&pid| pid > 0);
    let answer = tokio::time::timeout(ANSWER_BOUND, async {
        let mut connection = Connection::connect_trusted_ui_stream(stream)
            .await
            .map_err(|error| classify_handshake_error(&error))?;
        connection
            .daemon_handoff_report()
            .await
            .map_err(|error| classify_report_error(&error))
    })
    .await
    .unwrap_or(Err(Refusal::Unreadable(Unreadable::NoAnswer)));
    match answer {
        Ok(report) => Probe::Report {
            report: Box::new(report),
            daemon_pid,
        },
        Err(Refusal::Untrusted) => Probe::Untrusted,
        Err(Refusal::Unreadable(reason)) => Probe::Unreadable {
            reason,
            peer: daemon_pid.map(|pid| daemon_peer(pid, installed)),
            workload_scopes: workload_scopes(),
        },
    }
}

/// Classifies a failed connection to the socket itself.
pub(in crate::app) fn classify_socket_error(error: &io::Error) -> Probe {
    match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => Probe::NotRunning,
        _ => Probe::ConnectFailed(error.to_string()),
    }
}

/// Classifies a failed handshake on a connected socket.
pub(in crate::app) fn classify_handshake_error(error: &anyhow::Error) -> Refusal {
    match protocol_error(error).map(|error| error.code) {
        Some(ErrorCode::Unauthorized) => Refusal::Untrusted,
        Some(ErrorCode::IncompatibleVersion | ErrorCode::HandshakeRequired) => {
            Refusal::Unreadable(Unreadable::OtherProtocol)
        }
        Some(ErrorCode::ResourceLimit | ErrorCode::RateLimited) => {
            Refusal::Unreadable(Unreadable::Busy)
        }
        // Another refusal, a frame that did not decode, a handshake this
        // client rejected, or a daemon that hung up (as one over its
        // connection limit does).
        _ => Refusal::Unreadable(Unreadable::InvalidAnswer),
    }
}

/// Classifies a failed report request on an established connection.
pub(in crate::app) fn classify_report_error(error: &anyhow::Error) -> Refusal {
    match protocol_error(error).map(|error| error.code) {
        Some(ErrorCode::Unauthorized) => Refusal::Untrusted,
        Some(ErrorCode::ResourceLimit | ErrorCode::RateLimited) => {
            Refusal::Unreadable(Unreadable::Busy)
        }
        _ => Refusal::Unreadable(Unreadable::InvalidAnswer),
    }
}

/// Asks the question and returns whether to restart. Without a graphical
/// session, or when the dialog fails, it explains on stderr and answers no.
async fn ask(question: &RestartQuestion) -> bool {
    if std::env::var_os("WAYLAND_DISPLAY").is_none_or(|display| display.is_empty()) {
        for line in question_text(question) {
            eprintln!("{line}");
        }
        eprintln!(
            "There is no graphical session to ask in, so nothing was restarted. To restart anyway, run from a terminal Splinterm does not own: systemctl --user restart splinterd.service"
        );
        return false;
    }
    let request = match dialog_capability() {
        Ok(capability) => RestartDialogPrompt {
            capability,
            parent_pid: std::process::id(),
            lines: dialog_lines(question),
            respond_within_seconds: RESPOND_WITHIN.as_secs(),
        },
        Err(error) => {
            eprintln!("splinterm: could not ask ({error:#}); nothing was restarted.");
            return false;
        }
    };
    match ask_dialog_child(RESTART_DIALOG_COMMAND, request, RESPOND_WITHIN).await {
        Ok(DialogAnswer::Answered(restart)) => restart,
        Ok(DialogAnswer::NoAnswer) => {
            eprintln!("{}", no_answer_line());
            false
        }
        Err(error) => {
            eprintln!(
                "splinterm: could not ask about restarting ({error:#}); nothing was restarted."
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use splinterm::automation::response_protocol_error;
    use splinterm_protocol::{
        HandoffBlockingReason, HandoffBuild, HandoffBuildProvenance, HandoffVersionRange,
        ProtocolError,
    };

    const INSTALLED: u8 = 7;
    const OLDER: u8 = 6;
    const PID: u32 = 4242;

    fn installed() -> InstalledBuild {
        InstalledBuild {
            identity: [INSTALLED; splinterm_protocol::HANDOFF_BUILD_IDENTITY_BYTES],
            version: "0.2.1".to_owned(),
            client: Some((1, 10)),
            daemon: Some((1, 11)),
        }
    }

    const STAGES: [Option<LauncherStage>; 3] = [
        None,
        Some(LauncherStage::Start),
        Some(LauncherStage::Restart),
    ];

    fn report(identity: u8, live_splints: usize, reasons: Vec<HandoffBlockingReason>) -> Probe {
        let range = HandoffVersionRange {
            minimum: 1,
            maximum: 1,
        };
        Probe::Report {
            report: Box::new(HandoffReport {
                daemon_generation: 3,
                build: HandoffBuild {
                    version: format!("0.2.{identity}"),
                    identity: [identity; splinterm_protocol::HANDOFF_BUILD_IDENTITY_BYTES],
                    provenance: HandoffBuildProvenance::ReleaseCandidate,
                },
                private_protocol: range,
                handoff_protocol: range,
                terminal_checkpoint: range,
                descriptor_manifest: range,
                lairs: 1,
                transient_lairs: 0,
                dojos: 1,
                live_splints,
                rollback_available: true,
                blocking_reasons: reasons,
            }),
            daemon_pid: Some(PID),
        }
    }

    /// An unreadable daemon whose executable this client sits beside.
    fn unreadable(reason: Unreadable, workload_scopes: Option<usize>) -> Probe {
        Probe::Unreadable {
            reason,
            peer: Some(DaemonPeer {
                pid: PID,
                runs_installed_daemon: false,
                client_is_its_sibling: true,
            }),
            workload_scopes,
        }
    }

    fn asked(probe: &Probe) -> RestartQuestion {
        match decide(probe, &installed(), None) {
            Decision::Ask(question) => question,
            other => panic!("expected a question, got {other:?}"),
        }
    }

    fn failed(probe: &Probe, after: Option<LauncherStage>) -> Vec<String> {
        match decide(probe, &installed(), after) {
            Decision::Fail(lines) => lines,
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    fn protocol_failure(code: ErrorCode) -> anyhow::Error {
        response_protocol_error(ProtocolError::new(code, "refused"))
    }

    #[test]
    fn exit_codes_are_distinct_and_keep_clap_usage() {
        let codes = [
            LauncherExit::Ready,
            LauncherExit::Error,
            LauncherExit::NotNow,
            LauncherExit::StartService,
            LauncherExit::RestartService,
        ]
        .map(LauncherExit::code);
        assert_eq!(codes, [0, 1, 3, 10, 11]);
        assert!(!codes.contains(&2));
    }

    #[test]
    fn the_matching_build_launches_whatever_runs_in_it_and_after_any_stage() {
        for live in [0, 1, 40] {
            for after in STAGES {
                assert_eq!(
                    decide(&report(INSTALLED, live, Vec::new()), &installed(), after),
                    Decision::Launch
                );
            }
        }
    }

    #[test]
    fn only_a_reported_zero_restarts_without_asking_and_names_the_process() {
        for after in [None, Some(LauncherStage::Start)] {
            assert_eq!(
                decide(&report(OLDER, 0, Vec::new()), &installed(), after),
                Decision::RestartIdle { daemon_pid: PID }
            );
        }
        // Neither a failed handshake, a missing answer, a busy refusal nor a
        // bad answer is proof of idleness, even with no workload scope.
        for reason in [
            Unreadable::OtherProtocol,
            Unreadable::Busy,
            Unreadable::NoAnswer,
            Unreadable::InvalidAnswer,
        ] {
            for scopes in [None, Some(0)] {
                assert_eq!(
                    asked(&unreadable(reason, scopes)).shells,
                    ShellCount::Unknown
                );
            }
        }
    }

    #[test]
    fn a_report_without_a_process_identity_is_never_restarted() {
        for live in [0, 3] {
            let Probe::Report { report, .. } = report(OLDER, live, Vec::new()) else {
                unreachable!();
            };
            let probe = Probe::Report {
                report,
                daemon_pid: None,
            };
            assert!(failed(&probe, None)[0].contains("could not be identified"));
        }
    }

    #[test]
    fn another_build_with_live_shells_asks_with_the_exact_count_and_process() {
        let question = asked(&report(OLDER, 4, Vec::new()));
        assert_eq!(question.shells, ShellCount::Exact(4));
        assert_eq!(question.daemon_pid, PID);
        assert_eq!(question.live_upgrade, LiveUpgrade::Possible);
        assert_eq!(
            question.running,
            RunningDaemon::Build {
                version: "0.2.6".to_owned(),
                generation: 3
            }
        );
        assert_eq!(question.installed_version, "0.2.1");
        assert_eq!(confirm_label(question.shells), "Restart now, end 4 shells");

        // A running daemon from before live upgrade was enabled reports
        // `Unsupported`, and the question says its build is the reason.
        let blocked = asked(&report(OLDER, 1, vec![HandoffBlockingReason::Unsupported]));
        assert_eq!(
            blocked.live_upgrade,
            LiveUpgrade::Blocked(vec![
                "The running daemon's build does not support live upgrade.".to_owned()
            ])
        );
        let text = natural_rows_of(&dialog_lines(&blocked)).join(" ");
        assert!(
            text.contains("Live upgrade is not available now:"),
            "{text}"
        );
        assert!(!text.contains("then run `splinterm upgrade`"), "{text}");
        assert_eq!(confirm_label(blocked.shells), "Restart now, end 1 shell");
    }

    #[test]
    fn an_unreadable_daemon_is_the_bootstrap_boundary_with_a_best_effort_count() {
        let question = asked(&unreadable(Unreadable::OtherProtocol, Some(3)));
        assert_eq!(question.shells, ShellCount::About(3));
        assert_eq!(question.live_upgrade, LiveUpgrade::Unknown);
        assert_eq!(question.daemon_pid, PID);
        assert_eq!(
            confirm_label(question.shells),
            "Restart now, end about 3 shells"
        );
        let text = line_texts(&dialog_lines(&question)).join(" ");
        assert!(text.contains("such as 0.1.x"), "{text}");
        assert!(text.contains("best-effort count"), "{text}");
        assert!(text.contains("ends every shell it runs"), "{text}");
        assert!(!text.contains("splinterm upgrade"), "{text}");

        let uncounted = asked(&unreadable(Unreadable::OtherProtocol, None));
        assert_eq!(
            confirm_label(uncounted.shells),
            "Restart now, end all running shells"
        );
        let text = line_texts(&dialog_lines(&uncounted)).join(" ");
        assert!(text.contains("ends all running shells"), "{text}");
        assert!(!text.contains("best-effort"), "{text}");
    }

    #[test]
    fn a_busy_installed_daemon_fails_instead_of_asking() {
        for reason in [
            Unreadable::OtherProtocol,
            Unreadable::Busy,
            Unreadable::NoAnswer,
            Unreadable::InvalidAnswer,
        ] {
            let probe = Probe::Unreadable {
                reason,
                peer: Some(DaemonPeer {
                    pid: PID,
                    runs_installed_daemon: true,
                    client_is_its_sibling: true,
                }),
                workload_scopes: Some(5),
            };
            for after in STAGES {
                let lines = failed(&probe, after);
                assert!(lines[0].contains("is the installed build"), "{lines:?}");
                assert!(lines[0].contains("Try again shortly"), "{lines:?}");
            }
        }
    }

    #[test]
    fn only_the_client_beside_an_unreadable_daemon_may_ask() {
        let shadowing = Probe::Unreadable {
            reason: Unreadable::OtherProtocol,
            peer: Some(DaemonPeer {
                pid: PID,
                runs_installed_daemon: false,
                client_is_its_sibling: false,
            }),
            workload_scopes: Some(2),
        };
        assert!(
            failed(&shadowing, None)
                .join(" ")
                .contains("command -v splinterm")
        );
        let unidentified = Probe::Unreadable {
            reason: Unreadable::NoAnswer,
            peer: None,
            workload_scopes: Some(2),
        };
        assert!(failed(&unidentified, None)[0].contains("could not be identified"));
    }

    #[test]
    fn no_daemon_starts_the_service_once_and_never_restarts() {
        assert_eq!(
            decide(&Probe::NotRunning, &installed(), None),
            Decision::StartService
        );
        for after in [LauncherStage::Start, LauncherStage::Restart] {
            assert!(failed(&Probe::NotRunning, Some(after))[0].contains("did not start listening"));
        }
    }

    #[test]
    fn nothing_asks_or_restarts_again_after_a_restart() {
        for probe in [
            report(OLDER, 0, Vec::new()),
            report(OLDER, 5, Vec::new()),
            unreadable(Unreadable::OtherProtocol, Some(2)),
            unreadable(Unreadable::NoAnswer, None),
        ] {
            let lines = failed(&probe, Some(LauncherStage::Restart));
            assert!(lines[0].contains("still does not serve"), "{lines:?}");
            assert!(lines[1].contains("not restarted again"), "{lines:?}");
        }
    }

    #[test]
    fn untrusted_and_local_failures_never_restart() {
        for after in STAGES {
            assert!(
                failed(&Probe::Untrusted, after)
                    .join(" ")
                    .contains("command -v splinterm")
            );
            failed(&Probe::ConnectFailed("permission denied".to_owned()), after);
        }
    }

    #[test]
    fn errors_are_classified_without_guessing_idleness() {
        assert!(matches!(
            classify_socket_error(&io::Error::from(io::ErrorKind::NotFound)),
            Probe::NotRunning
        ));
        assert!(matches!(
            classify_socket_error(&io::Error::from(io::ErrorKind::ConnectionRefused)),
            Probe::NotRunning
        ));
        assert!(matches!(
            classify_socket_error(&io::Error::from(io::ErrorKind::PermissionDenied)),
            Probe::ConnectFailed(_)
        ));
        for (code, refusal) in [
            (
                ErrorCode::IncompatibleVersion,
                Refusal::Unreadable(Unreadable::OtherProtocol),
            ),
            (
                ErrorCode::HandshakeRequired,
                Refusal::Unreadable(Unreadable::OtherProtocol),
            ),
            (ErrorCode::Unauthorized, Refusal::Untrusted),
            (
                ErrorCode::ResourceLimit,
                Refusal::Unreadable(Unreadable::Busy),
            ),
            (
                ErrorCode::RateLimited,
                Refusal::Unreadable(Unreadable::Busy),
            ),
            (
                ErrorCode::Internal,
                Refusal::Unreadable(Unreadable::InvalidAnswer),
            ),
        ] {
            assert_eq!(
                classify_handshake_error(&protocol_failure(code)),
                refusal,
                "{code:?}"
            );
        }
        let hung_up = anyhow::Error::new(io::Error::from(io::ErrorKind::UnexpectedEof));
        assert_eq!(
            classify_handshake_error(&hung_up),
            Refusal::Unreadable(Unreadable::InvalidAnswer)
        );
        assert_eq!(
            classify_handshake_error(&anyhow!("splinterd sent an invalid handshake")),
            Refusal::Unreadable(Unreadable::InvalidAnswer)
        );
        assert_eq!(
            classify_report_error(&protocol_failure(ErrorCode::Unauthorized)),
            Refusal::Untrusted
        );
        assert_eq!(
            classify_report_error(&protocol_failure(ErrorCode::RateLimited)),
            Refusal::Unreadable(Unreadable::Busy)
        );
        assert_eq!(
            classify_report_error(&protocol_failure(ErrorCode::Internal)),
            Refusal::Unreadable(Unreadable::InvalidAnswer)
        );
        assert!(!Unreadable::OtherProtocol.retried());
        assert!(Unreadable::NoAnswer.retried());
        assert!(Unreadable::InvalidAnswer.retried());
        assert!(Unreadable::Busy.retried());
    }

    #[test]
    fn the_dialog_names_the_builds_the_count_the_keys_and_the_default() {
        let question = asked(&report(OLDER, 4, Vec::new()));
        let lines = natural_rows_of(&dialog_lines(&question));
        let text = lines.join(" ");
        for expected in [
            "The running Splinterm daemon is not the installed build.",
            "Running:   Splinterm 0.2.6, generation 3",
            "Installed: Splinterm 0.2.1",
            "It still has 4 running shells",
            "ends 4 running shells",
            "choose Not now, then run `splinterm upgrade`",
            "Not now opens nothing and changes nothing.",
            "no answer within 120 seconds, means Not now",
            "R or the green Restart button (right) restarts.",
            "the red Not now button (left) answer Not now.",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in\n{text}");
        }
        // The frame's title and the buttons, which are not body rows.
        let dialog = DialogText {
            lines: dialog_lines(&question),
            minimum_rows: 0,
        };
        assert_eq!(dialog.title(), "restart splinterd?");
        assert_eq!(dialog.deny_label(), "Not now");
        assert_eq!(dialog.allow_label(), "Restart now, end 4 shells");
        assert!(!text.contains("Restart now, end"), "{text}");
        assert!(!text.contains("TRUSTED SPLINTERM"), "{text}");
        assert!(!text.contains("[G"), "{text}");
        assert!(lines.iter().all(|line| line.len() <= MAX_DIALOG_COLUMNS));
        assert_eq!(
            not_now_lines(&question)[1],
            "Run `splinterm upgrade` to move them to the installed build without ending them."
        );
        let unattended = unattended_lines(&question).join(" ");
        assert!(unattended.contains("does not ask"), "{unattended}");
        assert!(unattended.contains("4 running shells"), "{unattended}");
    }

    /// No answer in time is plain "Not now", not a dialog failure.
    #[test]
    fn no_answer_says_so_plainly() {
        let line = no_answer_line();
        assert_eq!(
            line,
            "splinterm: no answer within 120 seconds, so nothing was restarted."
        );
        assert!(!line.contains("could not ask"), "{line}");
    }

    #[test]
    fn long_blocking_reasons_stay_bounded_and_point_to_the_full_report() {
        let reasons = (0..12)
            .map(|index| format!("reason {index} {}", "word ".repeat(40)))
            .collect::<Vec<_>>();
        let question = RestartQuestion {
            running: RunningDaemon::Build {
                version: "0.2.0".to_owned(),
                generation: 9,
            },
            installed_version: "0.2.1".to_owned(),
            shells: ShellCount::Exact(2),
            live_upgrade: LiveUpgrade::Blocked(reasons),
            daemon_pid: PID,
        };
        let lines = dialog_lines(&question);
        let rows = natural_rows_of(&lines);
        assert!(rows.len() <= MAX_RESTART_DIALOG_LINES, "{}", rows.len());
        assert!(
            rows.iter()
                .all(|line| line.chars().count() <= MAX_DIALOG_COLUMNS)
        );
        assert!(rows.iter().any(|line| line.contains("for the full report")));
        // The reasons take at most their bound of rows, the pointer to the
        // full report among them, and the last one kept is cut at a word.
        let reason_rows = rows
            .iter()
            .skip_while(|row| !row.starts_with("Live upgrade is not available"))
            .skip(1)
            .take_while(|row| row.starts_with("  "))
            .count();
        assert_eq!(reason_rows, MAX_REASON_LINES, "{rows:#?}");
        assert!(
            rows.iter()
                .all(|row| !row.contains("wor ") && !row.ends_with("wor"))
        );
        assert_eq!(not_now_lines(&question).len(), 1);

        let prompt = RestartDialogPrompt {
            capability: vec![1; CAPABILITY_BYTES],
            parent_pid: 42,
            lines,
            respond_within_seconds: RESPOND_WITHIN.as_secs(),
        };
        prompt.validate().unwrap();
        let (decision, _receiver) = std_mpsc::channel();
        let shortcut = Some(DialogShortcut::default());
        let options = prompt.window_options(decision, shortcut);
        let snapshot = options.snapshot.as_ref().unwrap();
        assert_eq!(usize::from(options.initial_rows), snapshot.rows);
        // The frame's legend names the chord the dialog engages by.
        assert_eq!(
            options
                .trusted_consent
                .as_ref()
                .and_then(TrustedConsentUi::shortcut),
            shortcut
        );
        let shown = snapshot
            .visible_rows
            .iter()
            .map(|row| {
                row.cells
                    .iter()
                    .map(|cell| cell.content.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            shown.contains("click inside or ctrl alt g to use the keyboard"),
            "{shown}"
        );
        assert!(!shown.contains("Click inside this dialog"), "{shown}");
        assert_eq!(snapshot.title, "Upgrade question");
    }

    /// The restart question's Window, as the dialog child builds it, passes
    /// every check `run_window` makes before it connects to the compositor,
    /// for the ordinary question and the longest one. Dialog snapshots once
    /// had no visible row identities, which `TerminalSnapshot::validate`
    /// refuses, so the question never mapped.
    #[test]
    fn restart_windows_pass_the_checks_before_the_compositor() {
        // The renderer the dialog child configures, from the default
        // configuration: Fontconfig's `monospace`.
        crate::app::consent::configure_dialog_renderer(&splinterm::config::AppConfig::default())
            .unwrap();
        let reasons = (0..12)
            .map(|index| format!("reason {index} {}", "word ".repeat(40)))
            .collect::<Vec<_>>();
        let longest = RestartQuestion {
            running: RunningDaemon::Build {
                version: "0.2.0".to_owned(),
                generation: 9,
            },
            installed_version: "0.2.1".to_owned(),
            shells: ShellCount::Exact(2),
            live_upgrade: LiveUpgrade::Blocked(reasons),
            daemon_pid: PID,
        };
        for question in [asked(&report(OLDER, 4, Vec::new())), longest] {
            let prompt = RestartDialogPrompt {
                capability: vec![1; CAPABILITY_BYTES],
                parent_pid: 42,
                lines: dialog_lines(&question),
                respond_within_seconds: RESPOND_WITHIN.as_secs(),
            };
            prompt.validate().unwrap();
            for shortcut in [Some(DialogShortcut::default()), None] {
                let (decision, _receiver) = std_mpsc::channel();
                let options = prompt.window_options(decision, shortcut);
                let snapshot = options.snapshot.as_ref().unwrap();
                assert_eq!(snapshot.validate().map_err(|error| error.message), Ok(()));
                splinterm::check_window_options(options).unwrap();
            }
        }
    }

    #[test]
    fn restart_prompts_are_validated() {
        let good = RestartDialogPrompt {
            capability: vec![1; CAPABILITY_BYTES],
            parent_pid: 42,
            lines: vec![DialogLine::new("question")],
            respond_within_seconds: 0,
        };
        good.validate().unwrap();
        for bad in [
            RestartDialogPrompt {
                capability: vec![1; CAPABILITY_BYTES - 1],
                ..good.clone()
            },
            RestartDialogPrompt {
                parent_pid: 0,
                ..good.clone()
            },
            RestartDialogPrompt {
                lines: Vec::new(),
                ..good.clone()
            },
            RestartDialogPrompt {
                lines: vec![DialogLine::new("x".repeat(MAX_DIALOG_LINE_CHARS + 1))],
                ..good.clone()
            },
            RestartDialogPrompt {
                lines: vec![DialogLine::blank(); MAX_RESTART_DIALOG_LINES + 1],
                ..good.clone()
            },
            RestartDialogPrompt {
                lines: vec![DialogLine::new("question").with_indent(MAX_DIALOG_INDENT + 1)],
                ..good.clone()
            },
            RestartDialogPrompt {
                lines: vec![DialogLine::new("question").with_wrap_at(MAX_DIALOG_COLUMNS + 1)],
                ..good.clone()
            },
            // Few lines, but too many rows once wrapped.
            RestartDialogPrompt {
                lines: vec![DialogLine::new("word ".repeat(220)); 6],
                ..good.clone()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn workload_scopes_count_only_running_splint_scopes() {
        let listing = "\
splinterm-splint0a1b-i1.scope loaded active running Splinterm Splint
splinterm-splint2c3d-i4.scope loaded active running Splinterm Splint
app-splinterm-dojo0a1b.slice loaded active active Splinterm Dojo
splinterm-splint-notes.service loaded active running Something else

";
        assert_eq!(workload_scope_count(listing), 2);
        assert_eq!(workload_scope_count(""), 0);
    }

    #[test]
    fn the_sibling_client_survives_a_replaced_executable() {
        assert_eq!(
            sibling_client_path(Path::new("/usr/bin/splinterd (deleted)")),
            Some(PathBuf::from("/usr/bin/splinterm"))
        );
        assert_eq!(
            sibling_client_path(Path::new("/nix/store/abc-splinterm/bin/splinterd")),
            Some(PathBuf::from("/nix/store/abc-splinterm/bin/splinterm"))
        );
        // A sealed memfd image has no directory to look beside.
        assert_eq!(
            sibling_client_path(Path::new("/memfd:splinterd (deleted)")),
            None
        );
        assert_eq!(sibling_client_path(Path::new("splinterd")), None);
    }

    #[test]
    fn the_service_main_pid_is_parsed_strictly() {
        assert_eq!(parse_main_pid("4242\n"), Some(4242));
        assert_eq!(parse_main_pid("0\n"), None);
        assert_eq!(parse_main_pid(""), None);
        assert_eq!(parse_main_pid("[not set]"), None);
    }

    /// The terminal note prints the question as the dialog first shows
    /// it: prose wrapped at its width, other lines whole.
    #[test]
    fn the_terminal_note_wraps_the_prose_at_its_width() {
        let question = asked(&unreadable(Unreadable::OtherProtocol, Some(3)));
        let text = question_text(&question);
        assert!(
            text.iter().all(|line| line.chars().count() <= WRAP_COLUMNS),
            "{text:#?}"
        );
        assert!(text.len() > question_lines(&question).len());
        assert_eq!(
            text.join(" ").split_whitespace().collect::<Vec<_>>(),
            line_texts(&question_lines(&question))
                .join(" ")
                .split_whitespace()
                .collect::<Vec<_>>()
        );
    }

    /// The terminal note never splits a word, however long: a long path
    /// gets a row of its own, whole, for copying.
    #[test]
    fn the_terminal_note_never_splits_a_long_word() {
        let path = format!("/{}", "p".repeat(100));
        let rows = terminal_rows(&prose(format!("Run {path} to see it.")));
        assert_eq!(rows, ["Run".to_owned(), path, "to see it.".to_owned()]);
        let bullet = terminal_rows(&reason_paragraph(&"word ".repeat(30)));
        assert!(bullet[0].starts_with("  • word"));
        assert!(bullet[1..].iter().all(|row| row.starts_with("      word")));
        assert!(bullet.iter().all(|row| row.chars().count() <= WRAP_COLUMNS));
        assert_eq!(terminal_rows(&DialogLine::blank()), [""]);
        assert_eq!(
            terminal_rows(&DialogLine::new("  Running:   Splinterm 0.2.6")),
            ["  Running:   Splinterm 0.2.6"]
        );
    }

    /// The text of each line, as given.
    fn line_texts(lines: &[DialogLine]) -> Vec<String> {
        lines.iter().map(|line| line.text.clone()).collect()
    }

    /// The rows the dialog first shows `lines` in.
    fn natural_rows_of(lines: &[DialogLine]) -> Vec<String> {
        splinterm::frontend::dialog_text::natural_layout(
            &[DialogText {
                lines: lines.to_vec(),
                minimum_rows: 0,
            }],
            0,
        )
        .rows
    }
}
