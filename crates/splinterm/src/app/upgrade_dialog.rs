//! The desktop question for `splinterm upgrade --now` when it cannot ask in a
//! terminal: there is none, or it runs inside a Splinterm terminal whose input
//! the upgrade pauses.
//!
//! It reuses the trusted consent dialog's mechanism and styling: a private
//! socketpair on the child's stdin and stdout, a random capability echoed in
//! the reply, a static snapshot in a `TrustedConsentUi` Window, and "closing
//! the Window means no". Unlike consent, the daemon does not launch it. The
//! upgrade command keeps its own daemon connection and starts the dialog as
//! a child of itself through `/proc/self/exe`, which names the exact image
//! the command runs, the installed client the daemon just verified, even if
//! the file was replaced since. The child never connects to the daemon and
//! holds no anchor; it only shows the question and returns one bit, and the
//! command treats a missing, late, or unauthenticated reply as cancel.
//!
//! Its keys differ from consent because proceeding closes Windows: `D`,
//! `Escape`, `Enter`, and the red left area cancel; only `G` or the green
//! right area proceeds. The dialog never takes keyboard focus when it
//! appears, and its keys stay inert until the person clicks inside it; keys
//! and the proceed click count only 600 ms after that click,
//! and key repeats never count
//! (`frontend::trusted_dialog`). The Window title is "Splinterm — Upgrade".
//!
//! The child mechanism is generic over [`DialogPrompt`], so the desktop
//! launcher's restart question (`launcher_check`) runs through exactly the
//! same parent checks, capability echo, deadline, and reaping. The parent
//! side, which a remote Window's agent access prompts share, is
//! `dialog_child`.

use std::{fmt::Write as _, io, process::Command, sync::mpsc as std_mpsc, time::Duration};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use splinterd::dialog_shortcut::DialogShortcut;
use splinterm::automation::HandoffDecisionPrompt;
use splinterm::{
    DialogIdentity, DialogLine, DialogStyle, DialogText, TrustedConsentUi, WindowOptions,
    config::ResolvedTheme,
};
use splinterm_protocol::{
    HandoffUnresponsiveWindow, MAX_HANDOFF_DOJO_NAME_BYTES, MAX_HANDOFF_WINDOW_DOJOS,
    MAX_HANDOFF_WINDOWS,
};

use super::{
    consent::{
        read_private_frame_bounded, run_trusted_dialog_window, trusted_label,
        write_private_frame_bounded,
    },
    dialog_child::{
        DialogOutcome, DialogReplyFrame, ask_dialog_process as ask_dialog_process_shared,
    },
};

/// The hidden subcommand the dialog child runs.
pub(in crate::app) const UPGRADE_DIALOG_COMMAND: &str = "upgrade-dialog";

// Random bytes the child must echo, as the consent dialog does.
pub(in crate::app) use super::dialog_child::CAPABILITY_BYTES;

/// The largest prompt frame: 64 Windows of 32 Dojos with 128-byte names and
/// their identifiers stay well below it.
const MAX_UPGRADE_DIALOG_FRAME_BYTES: usize = 1024 * 1024;

/// A question a trusted dialog child of this command shows. The prompt frame
/// goes to the child's stdin and a one-bit [`DialogReply`] comes back on its
/// stdout, echoing [`DialogPrompt::capability`].
pub(in crate::app) trait DialogPrompt:
    Serialize + DeserializeOwned + Send + 'static
{
    /// The random bytes the reply must echo.
    fn capability(&self) -> &[u8];
    /// The asking command, which the child checks is still its parent.
    fn parent_pid(&self) -> u32;
    /// Rejects a prompt the child must not show.
    fn validate(&self) -> Result<()>;
    /// The Window that shows the prompt and sends its answer on `decision`,
    /// engaged by `shortcut` as well as by a click when it is on.
    fn window_options(
        &self,
        decision: std_mpsc::Sender<bool>,
        shortcut: Option<DialogShortcut>,
    ) -> WindowOptions;
}

/// The widest dialog line, matching the consent snapshot's column clamp.
pub(in crate::app) const MAX_DIALOG_COLUMNS: usize = 120;

/// The fewest grid rows an upgrade or restart question asks for.
pub(in crate::app) const MIN_DIALOG_ROWS: usize = 18;

/// The width the footer's explanation and answer hint wrap at.
const FOOTER_WRAP_COLUMNS: usize = 70;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct UpgradeDialogPrompt {
    capability: Vec<u8>,
    /// The asking command, which the child checks is still its parent.
    parent_pid: u32,
    respond_within_seconds: u64,
    windows: Vec<HandoffUnresponsiveWindow>,
}

impl DialogPrompt for UpgradeDialogPrompt {
    fn capability(&self) -> &[u8] {
        &self.capability
    }

    fn parent_pid(&self) -> u32 {
        self.parent_pid
    }

    fn window_options(
        &self,
        decision: std_mpsc::Sender<bool>,
        shortcut: Option<DialogShortcut>,
    ) -> WindowOptions {
        dialog_window_options(self, decision, shortcut)
    }

    fn validate(&self) -> Result<()> {
        let valid = self.capability.len() == CAPABILITY_BYTES
            && self.parent_pid > 0
            && !self.windows.is_empty()
            && self.windows.len() <= MAX_HANDOFF_WINDOWS
            && self.windows.iter().all(|window| {
                window.dojos.len() <= MAX_HANDOFF_WINDOW_DOJOS
                    && window
                        .dojos
                        .iter()
                        .all(|dojo| dojo.name.len() <= MAX_HANDOFF_DOJO_NAME_BYTES)
            });
        if !valid {
            bail!("invalid upgrade dialog prompt");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DialogReply {
    capability: Vec<u8>,
    proceed: bool,
}

impl DialogReplyFrame for DialogReply {
    fn capability(&self) -> &[u8] {
        &self.capability
    }
}

/// One Window's dialog line, at most [`MAX_DIALOG_COLUMNS`] bytes. Every
/// Window gets its own line; when its Dojo names do not fit, the line keeps
/// as many as fit and counts the rest, so nothing is silently dropped.
fn window_line(window: &HandoffUnresponsiveWindow) -> String {
    let reason = match window.reason {
        splinterm_protocol::HandoffWindowIssue::NoAcknowledgement => "did not answer in time",
        splinterm_protocol::HandoffWindowIssue::Unpinned => "could not be identified",
        splinterm_protocol::HandoffWindowIssue::Undeclared => "cannot take part",
    };
    let mut line = format!("  • PID {} — {reason} — Dojos: ", window.pid);
    if window.dojos.is_empty() {
        line.push_str("none named");
        return line;
    }
    let names = window
        .dojos
        .iter()
        .map(|dojo| trusted_label(&dojo.name, MAX_HANDOFF_DOJO_NAME_BYTES))
        .collect::<Vec<_>>();
    for (shown, name) in names.iter().enumerate() {
        let remaining = names.len() - shown;
        let separator = if shown == 0 { "" } else { ", " };
        let tail = if remaining > 1 {
            format!(" (+{} more)", remaining - 1)
        } else {
            String::new()
        };
        if line.len() + separator.len() + name.len() + tail.len() > MAX_DIALOG_COLUMNS {
            if shown == 0 {
                let _ = write!(line, "{} Dojo(s)", names.len());
            } else {
                let _ = write!(line, " (+{remaining} more)");
            }
            return line;
        }
        line.push_str(separator);
        line.push_str(name);
    }
    line
}

/// The dialog text: the frame's title, the Windows, what proceeding does,
/// the deadline, how to answer, and the two buttons' labels, the green one
/// naming how many Windows close.
fn dialog_lines(prompt: &UpgradeDialogPrompt) -> Vec<DialogLine> {
    let count = prompt.windows.len();
    let windows = if count == 1 { "Window" } else { "Windows" };
    let mut lines = vec![
        DialogLine::title("close windows to upgrade?"),
        DialogLine::new(format!(
            "splinterd is upgrading. {count} {windows} cannot take part:"
        ))
        .with_style(DialogStyle::Strong),
    ];
    let listed = |window| DialogLine::new(window_line(window)).with_indent(4);
    // Every Window gets a line while they fit; past that, the last line
    // counts the rest, so the number that would close is always visible.
    if count <= MAX_LISTED_WINDOWS {
        lines.extend(prompt.windows.iter().map(listed));
    } else {
        let shown = MAX_LISTED_WINDOWS - 1;
        lines.extend(prompt.windows[..shown].iter().map(listed));
        let rest = &prompt.windows[shown..];
        let dojos: usize = rest.iter().map(|window| window.dojos.len()).sum();
        lines.push(DialogLine::new(format!(
            "  +{} more Windows ({dojos} Dojo{})",
            rest.len(),
            if dojos == 1 { "" } else { "s" }
        )));
    }
    lines.extend([
        DialogLine::blank(),
        DialogLine::new(
            "Proceeding closes these Windows. Their terminals keep running in the upgraded daemon; reopen them from the Recent Dojos picker.",
        )
        .with_wrap_at(FOOTER_WRAP_COLUMNS),
        DialogLine::new("Input is paused in every Window until you answer."),
        DialogLine::new(format!(
            "Closing this dialog, or no answer within {} seconds, cancels.",
            prompt.respond_within_seconds
        )),
        DialogLine::blank(),
        DialogLine::hint(
            "G or the green Proceed button (right) proceeds. Enter, Escape, D, or the red Cancel button (left) cancel.",
        )
        .with_wrap_at(FOOTER_WRAP_COLUMNS),
        DialogLine::deny("Cancel"),
        DialogLine::allow(format!("Proceed, close {count} {windows}")),
    ]);
    lines
}

/// Body rows above and below the Window list at the dialog's natural
/// width: the header row, then the blank row, the two rows of the
/// explanation, the pause and deadline rows, the blank row, and the two
/// rows of the answer hint.
const FIXED_DIALOG_ROWS: usize = 1 + 8;

/// The tallest dialog, in terminal rows. The static snapshot cannot scroll,
/// so the dialog is sized to its content within this bound.
const MAX_DIALOG_ROWS: usize = 40;

/// The most Window lines that fit the body of the tallest dialog, inside
/// its box; a longer list is summarized in its last line.
const MAX_LISTED_WINDOWS: usize =
    MAX_DIALOG_ROWS - splinterm::frontend::dialog_text::CHROME_ROWS - FIXED_DIALOG_ROWS;

/// The snapshot identity of the upgrade and restart questions.
pub(in crate::app) fn question_identity() -> DialogIdentity {
    DialogIdentity {
        title: "Upgrade question".to_owned(),
        splint_id: splinterm_core::SplintId::new(),
        incarnation: 0,
    }
}

fn dialog_text(prompt: &UpgradeDialogPrompt) -> DialogText {
    DialogText {
        lines: dialog_lines(prompt),
        minimum_rows: MIN_DIALOG_ROWS,
    }
}

/// The Window at the natural size of the question's text.
fn dialog_window_options(
    prompt: &UpgradeDialogPrompt,
    decision: std_mpsc::Sender<bool>,
    shortcut: Option<DialogShortcut>,
) -> WindowOptions {
    TrustedConsentUi::upgrade(decision)
        .with_shortcut(shortcut)
        .with_respond_within(Duration::from_secs(prompt.respond_within_seconds))
        .with_text(dialog_text(prompt))
        .with_identity(question_identity())
        .into_window_options()
}

/// The hidden `upgrade-dialog` child: reads one prompt on stdin, shows it,
/// and writes the answer on stdout. Closing the Window answers cancel.
///
/// It dies with the command that asked. The workspace forbids `unsafe`, so
/// the child cannot be given a `pre_exec` hook; instead it sets its own
/// parent-death signal (`PR_SET_PDEATHSIG`, through rustix,
/// [`tie_dialog_to_parent`]) before anything else, before even its theme is
/// read, so a child blocked on a theme file dies with its parent too; it then
/// checks that its parent is still the command named in the prompt, which
/// covers a parent that died before the signal was set. The signal follows
/// the thread that spawned the child, the runtime thread that drives the
/// command for as long as the process lives.
pub(in crate::app) fn run_upgrade_dialog_client(
    shortcut: Option<DialogShortcut>,
    theme: ResolvedTheme,
) -> Result<()> {
    run_dialog_client::<UpgradeDialogPrompt>(shortcut, theme)
}

/// Makes this dialog child die with the command that asked
/// ([`run_upgrade_dialog_client`]). The command's entry calls it first.
pub(in crate::app) fn tie_dialog_to_parent() -> Result<()> {
    rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::KILL))
        .context("tie the dialog to its command")
}

/// The body of every trusted dialog child: see [`run_upgrade_dialog_client`].
pub(in crate::app) fn run_dialog_client<P: DialogPrompt>(
    shortcut: Option<DialogShortcut>,
    theme: ResolvedTheme,
) -> Result<()> {
    let prompt: P =
        read_private_frame_bounded(&mut io::stdin().lock(), MAX_UPGRADE_DIALOG_FRAME_BYTES)?;
    prompt.validate()?;
    if !is_parent(prompt.parent_pid(), rustix::process::getppid()) {
        bail!("the command that asked has exited");
    }
    let (decision, receiver) = std_mpsc::channel();
    let registration = run_trusted_dialog_window(prompt.window_options(decision, shortcut), theme)?;
    let proceed = receiver.try_recv().unwrap_or(false);
    let written = write_private_frame_bounded(
        &mut io::stdout().lock(),
        &DialogReply {
            capability: prompt.capability().to_vec(),
            proceed,
        },
        MAX_UPGRADE_DIALOG_FRAME_BYTES,
    );
    // The answer goes first; the shortcut registration ends after it.
    registration.finish();
    written
}

/// Whether `parent` (from `getppid`) is the command that sent the prompt.
/// An orphaned child is reparented to init or a subreaper, never to it.
fn is_parent(expected: u32, parent: Option<rustix::process::Pid>) -> bool {
    i32::try_from(expected).is_ok_and(|expected| rustix::process::Pid::as_raw(parent) == expected)
}

// A fresh random capability for one dialog prompt.
pub(in crate::app) use super::dialog_child::dialog_capability;

/// Asks the question in the dialog and returns the answer, or
/// [`DialogAnswer::NoAnswer`] when none arrived within the prompt's
/// `respond_within`. Any failure is an error. The caller treats both as
/// cancel.
pub(in crate::app) async fn ask_in_dialog(prompt: HandoffDecisionPrompt) -> Result<DialogAnswer> {
    if std::env::var_os("WAYLAND_DISPLAY").is_none_or(|display| display.is_empty()) {
        bail!(
            "there is no terminal to ask in and no graphical session for the dialog; run `splinterm upgrade --now` from a terminal Splinterm does not own"
        );
    }
    let request = UpgradeDialogPrompt {
        capability: dialog_capability()?,
        parent_pid: std::process::id(),
        respond_within_seconds: prompt.respond_within.as_secs(),
        windows: prompt.windows,
    };
    ask_dialog_child(UPGRADE_DIALOG_COMMAND, request, prompt.respond_within).await
}

/// How a trusted dialog question ended without an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum DialogAnswer {
    /// The person answered: `true` proceeds. Closing the dialog answers
    /// `false`.
    Answered(bool),
    /// Nobody answered within the budget, and the child was killed and
    /// reaped. Every caller treats this as "no" and says so plainly: it is
    /// not a failure, so nothing the child printed (such as the fonts it
    /// resolved) is reported.
    NoAnswer,
}

/// Starts `/proc/self/exe <command>` as a trusted dialog child, sends it
/// `request`, and returns its answer. The answer must arrive within `budget`
/// and echo the request's capability. A reply that does not arrive in time
/// is [`DialogAnswer::NoAnswer`]; anything else is an error, which names
/// what the child printed when it failed before answering. Every caller
/// treats both as "no". The child is killed and reaped unless it answered.
pub(in crate::app) async fn ask_dialog_child<P: DialogPrompt>(
    command: &'static str,
    request: P,
    budget: Duration,
) -> Result<DialogAnswer> {
    let mut child = Command::new("/proc/self/exe");
    child.arg(command);
    ask_dialog_process(child, request, budget).await
}

/// [`ask_dialog_child`] with the child's program and arguments in `command`:
/// the shared trusted dialog child ([`super::dialog_child`]), never
/// cancelled, with its one-bit reply.
async fn ask_dialog_process<P: DialogPrompt>(
    command: Command,
    request: P,
    budget: Duration,
) -> Result<DialogAnswer> {
    request.validate()?;
    let capability = request.capability().to_vec();
    match ask_dialog_process_shared::<_, DialogReply>(
        command,
        request,
        capability,
        MAX_UPGRADE_DIALOG_FRAME_BYTES,
        budget,
        std::future::pending(),
    )
    .await?
    {
        DialogOutcome::Reply(reply) => Ok(DialogAnswer::Answered(reply.proceed)),
        DialogOutcome::NoAnswer | DialogOutcome::Cancelled => Ok(DialogAnswer::NoAnswer),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Instant;

    use splinterm::frontend::dialog_text::{CHROME_ROWS, natural_layout, natural_rows};
    use splinterm_core::{DojoId, LairId};
    use splinterm_protocol::{HandoffWindowDojo, HandoffWindowIssue};

    /// The dialog's rows of text at its natural size.
    fn dialog_rows_of(prompt: &UpgradeDialogPrompt) -> Vec<String> {
        natural_layout(&[dialog_text(prompt)], 0).rows
    }

    /// The footer's rows at the natural size, as they always read.
    const DIALOG_FOOTER: [&str; 5] = [
        "Proceeding closes these Windows. Their terminals keep running in the",
        "upgraded daemon; reopen them from the Recent Dojos picker.",
        "Input is paused in every Window until you answer.",
        "G or the green Proceed button (right) proceeds. Enter, Escape, D, or",
        "the red Cancel button (left) cancel.",
    ];

    fn window(pid: u32, dojos: usize, name_bytes: usize) -> HandoffUnresponsiveWindow {
        HandoffUnresponsiveWindow {
            index: u8::try_from(pid % 64).unwrap(),
            pid,
            reason: HandoffWindowIssue::NoAcknowledgement,
            dojos: (0..dojos)
                .map(|number| HandoffWindowDojo {
                    lair_id: LairId::new(),
                    dojo_id: DojoId::new(),
                    name: format!("{number:0>name_bytes$}"),
                })
                .collect(),
        }
    }

    fn prompt(windows: Vec<HandoffUnresponsiveWindow>) -> UpgradeDialogPrompt {
        UpgradeDialogPrompt {
            capability: vec![9; CAPABILITY_BYTES],
            parent_pid: 1234,
            respond_within_seconds: 18,
            windows,
        }
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "the dialog takes the configured setting, which can be off"
    )]
    fn shortcut() -> Option<DialogShortcut> {
        Some(DialogShortcut::default())
    }

    #[test]
    fn the_dialog_names_its_buttons_and_engages_by_its_shortcut() {
        let dialog = prompt(vec![window(5, 0, 0)]);
        // The frame's title and the buttons; the frame's legend, not the
        // body, says how to engage the keyboard.
        let text = dialog_text(&dialog);
        assert_eq!(text.title(), "close windows to upgrade?");
        assert_eq!(text.deny_label(), "Cancel");
        assert_eq!(text.allow_label(), "Proceed, close 1 Window");
        assert_eq!(
            dialog_text(&prompt(vec![window(5, 0, 0), window(6, 0, 0)])).allow_label(),
            "Proceed, close 2 Windows"
        );
        let lines = dialog_rows_of(&dialog);
        assert_eq!(
            lines.last().map(String::as_str),
            Some("the red Cancel button (left) cancel.")
        );
        assert!(!lines.iter().any(|line| line.contains("Click inside")));
        let (decision, _receiver) = std_mpsc::channel();
        let options = dialog_window_options(&dialog, decision, shortcut());
        assert_eq!(
            options
                .trusted_consent
                .as_ref()
                .and_then(TrustedConsentUi::shortcut),
            shortcut()
        );
    }

    #[test]
    fn short_dojo_lists_are_shown_in_full() {
        let mut short = window(4242, 2, 4);
        short.dojos[1].name = "lo\u{202e}gs".to_owned();
        let line = window_line(&short);
        assert_eq!(
            line,
            "  • PID 4242 — did not answer in time — Dojos: 0000, lo\u{fffd}gs"
        );
    }

    #[test]
    fn long_dojo_lists_keep_a_count_and_never_exceed_the_width() {
        let line = window_line(&window(7, 32, 20));
        assert!(line.len() <= MAX_DIALOG_COLUMNS, "{line}");
        assert!(line.contains("more)"), "{line}");
        let shown = line.matches(", ").count() + 1;
        let counted: usize = line
            .rsplit("(+")
            .next()
            .and_then(|tail| tail.split(' ').next())
            .and_then(|count| count.parse().ok())
            .unwrap();
        assert_eq!(shown + counted, 32, "{line}");

        let one_huge = window_line(&window(8, 3, MAX_HANDOFF_DOJO_NAME_BYTES));
        assert!(one_huge.len() <= MAX_DIALOG_COLUMNS, "{one_huge}");
        assert!(one_huge.ends_with("3 Dojo(s)"), "{one_huge}");
    }

    #[test]
    fn the_largest_prompt_fits_the_bounded_dialog_with_a_count_and_the_actions() {
        let windows = (1..=64).map(|pid| window(pid, 32, 128)).collect::<Vec<_>>();
        let dialog = prompt(windows);
        let lines = dialog_rows_of(&dialog);
        let listed = lines
            .iter()
            .filter(|line| line.starts_with("  • PID "))
            .count();
        assert_eq!(listed, MAX_LISTED_WINDOWS - 1);
        let summary = format!(
            "  +{} more Windows ({} Dojos)",
            64 - listed,
            (64 - listed) * 32
        );
        assert!(lines.contains(&summary), "{lines:#?}");
        assert!(
            lines[0].contains("64 Windows cannot take part"),
            "{}",
            lines[0]
        );
        for footer in DIALOG_FOOTER {
            assert!(
                lines.iter().any(|line| line == footer),
                "missing {footer:?}"
            );
        }
        assert!(
            lines
                .iter()
                .any(|line| line.contains("no answer within 18 seconds"))
        );
        assert!(lines.iter().all(|line| line.len() <= MAX_DIALOG_COLUMNS));
        let (decision, _receiver) = std_mpsc::channel();
        let options = dialog_window_options(&dialog, decision, shortcut());
        let snapshot = options.snapshot.as_ref().unwrap();
        assert!(snapshot.rows <= MAX_DIALOG_ROWS, "{}", snapshot.rows);
        // The grid shows the whole snapshot, the text and the box around it.
        assert_eq!(usize::from(options.initial_rows), snapshot.rows);
        assert_eq!(usize::from(options.initial_columns), snapshot.columns);
        assert_eq!(lines.len() + CHROME_ROWS, snapshot.rows);
        assert_eq!(snapshot.visible_rows.len(), snapshot.rows);
        assert_eq!(snapshot.title, "Upgrade question");
        assert_eq!(snapshot.cursor_row, -1);
    }

    /// The upgrade question's Window, as the dialog child builds it, passes
    /// every check `run_window` makes before it connects to the compositor,
    /// from one Window to the largest prompt. Dialog snapshots once had no
    /// visible row identities, which `TerminalSnapshot::validate` refuses,
    /// so the question never mapped.
    #[test]
    fn upgrade_windows_pass_the_checks_before_the_compositor() {
        // The renderer the dialog child configures, from the default
        // configuration: Fontconfig's `monospace`.
        crate::app::consent::configure_dialog_renderer(&splinterm::config::AppConfig::default())
            .unwrap();
        let listed = u32::try_from(MAX_LISTED_WINDOWS).unwrap();
        for windows in [
            vec![window(5, 0, 0)],
            vec![window(5, 3, 12), window(6, 1, 128)],
            (1..=listed).map(|pid| window(pid, 32, 128)).collect(),
            (1..=64).map(|pid| window(pid, 32, 128)).collect(),
        ] {
            let dialog = prompt(windows);
            dialog.validate().unwrap();
            for shortcut in [shortcut(), None] {
                let (decision, _receiver) = std_mpsc::channel();
                let options = dialog_window_options(&dialog, decision, shortcut);
                let snapshot = options.snapshot.as_ref().unwrap();
                assert_eq!(snapshot.validate().map_err(|error| error.message), Ok(()));
                splinterm::check_window_options(options).unwrap();
            }
        }
    }

    /// A dialog child that fails before answering is reaped, and the error
    /// says what it printed and how it ended instead of only that its answer
    /// never came.
    #[tokio::test]
    async fn a_dialog_that_fails_before_answering_says_why() {
        let mut failing = Command::new("sh");
        failing.args([
            "-c",
            "echo 'splinterm graphical client failed: no compositor' >&2; exit 1",
        ]);
        let started = Instant::now();
        let error = ask_dialog_process(
            failing,
            prompt(vec![window(5, 0, 0)]),
            Duration::from_secs(10),
        )
        .await
        .unwrap_err()
        .to_string();
        // Reported when it exits, not when the budget runs out, which
        // would be no answer.
        assert!(started.elapsed() < Duration::from_secs(5), "{error}");
        assert!(
            error.starts_with(
                "the dialog failed: splinterm graphical client failed: no compositor (exit status: 1; "
            ),
            "{error}"
        );

        let mut silent = Command::new("sh");
        silent.args(["-c", "exit 3"]);
        let error = ask_dialog_process(
            silent,
            prompt(vec![window(5, 0, 0)]),
            Duration::from_secs(10),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(
            error.starts_with("the dialog gave no answer (exit status: 3): "),
            "{error}"
        );
    }

    /// A dialog nobody answers within the budget ends as "no answer", not
    /// as a failure carrying what the child printed, and is killed and
    /// reaped without waiting out its own lifetime.
    #[tokio::test]
    async fn an_unanswered_dialog_is_no_answer_and_is_reaped() {
        let mut lingering = Command::new("sh");
        lingering.args([
            "-c",
            "echo 'Resolved primary regular: JetBrainsMono Nerd Font Regular' >&2; exec sleep 30",
        ]);
        let started = Instant::now();
        let answer = ask_dialog_process(
            lingering,
            prompt(vec![window(5, 0, 0)]),
            Duration::from_millis(300),
        )
        .await
        .unwrap();
        assert_eq!(answer, DialogAnswer::NoAnswer);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn every_window_is_listed_while_the_list_fits() {
        let windows = (1..=u32::try_from(MAX_LISTED_WINDOWS).unwrap())
            .map(|pid| window(pid, 32, 128))
            .collect::<Vec<_>>();
        let dialog = prompt(windows);
        let lines = dialog_rows_of(&dialog);
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.starts_with("  • PID "))
                .count(),
            MAX_LISTED_WINDOWS
        );
        assert!(!lines.iter().any(|line| line.contains("more Windows")));
        assert!(natural_rows(&[dialog_text(&dialog)]) <= MAX_DIALOG_ROWS);
        assert_eq!(lines.len(), FIXED_DIALOG_ROWS + MAX_LISTED_WINDOWS);
        let single = dialog_rows_of(&prompt(vec![window(5, 0, 0)]));
        assert!(
            single[0].contains("1 Window cannot take part"),
            "{}",
            single[0]
        );
    }

    #[test]
    fn the_dialog_says_what_proceeding_does_and_how_to_answer() {
        let text = dialog_rows_of(&prompt(vec![window(5, 0, 0)])).join("\n");
        assert!(text.contains("PID 5 — did not answer in time — Dojos: none named"));
        assert!(text.contains("Proceeding closes these Windows"));
        assert!(text.contains("terminals keep running"));
        assert!(text.contains("Recent Dojos picker"));
        assert!(text.contains("no answer within 18 seconds, cancels"));
        assert!(text.contains("G or the green Proceed button (right) proceeds"));
        assert!(text.contains("the red Cancel button (left) cancel"));
        assert!(!text.contains("G / Enter"));
    }

    #[test]
    fn prompts_are_bounded_and_round_trip_through_the_private_frame() {
        let largest = prompt((1..=64).map(|pid| window(pid, 32, 128)).collect());
        largest.validate().unwrap();
        let mut buffer = Vec::new();
        write_private_frame_bounded(&mut buffer, &largest, MAX_UPGRADE_DIALOG_FRAME_BYTES).unwrap();
        let decoded: UpgradeDialogPrompt =
            read_private_frame_bounded(&mut buffer.as_slice(), MAX_UPGRADE_DIALOG_FRAME_BYTES)
                .unwrap();
        assert_eq!(decoded, largest);

        assert!(prompt(Vec::new()).validate().is_err());
        let mut short_capability = prompt(vec![window(1, 0, 0)]);
        short_capability.capability.pop();
        assert!(short_capability.validate().is_err());
        assert!(prompt(vec![window(1, 33, 1)]).validate().is_err());
        assert!(prompt(vec![window(1, 1, 129)]).validate().is_err());
    }

    #[test]
    fn only_the_asking_command_counts_as_the_parent() {
        let parent = rustix::process::getppid();
        let raw = u32::try_from(rustix::process::Pid::as_raw(parent)).unwrap();
        assert!(is_parent(raw, parent));
        // Reparented to init, or to a subreaper.
        if raw != 1 {
            assert!(!is_parent(raw, rustix::process::Pid::from_raw(1)));
        }
        assert!(!is_parent(raw, None));
        assert!(!is_parent(u32::MAX, parent));
    }
}
