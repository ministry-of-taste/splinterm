use std::{
    io::{self, Read, Write},
    os::fd::{AsFd as _, BorrowedFd},
    sync::mpsc as std_mpsc,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use splinterd::dialog_shortcut::{BackgroundRegistration, DialogShortcut};
use splinterm::{
    DialogIdentity, DialogLine, DialogStyle, DialogText, TrustedConsentUi, WindowOptions,
    config::{AppConfig, OmarchyShellSettings, ResolvedTheme},
    run_window,
};
use splinterm_protocol::{
    AccessLevel, ConsentDecision, ConsentOrigin, ConsentPrompt, ConsentReplacement, ConsentReply,
    ConsentTarget, GrantDuration, MAX_CONSENT_FRAME_BYTES, MAX_CONSENT_PATH_CHARS,
    MAX_DAEMON_HOSTNAME_BYTES, MAX_REMOTE_PROFILE_NAME_BYTES, shared_identity_line,
    usable_daemon_hostname,
};

/// Reads one length-prefixed JSON frame of at most `maximum` bytes from a
/// trusted dialog's private channel.
pub(in crate::app) fn read_private_frame_bounded<T: serde::de::DeserializeOwned>(
    reader: &mut impl Read,
    maximum: usize,
) -> Result<T> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > maximum {
        bail!("invalid private dialog frame length");
    }
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).context("invalid private dialog frame")
}

/// Writes one length-prefixed JSON frame of at most `maximum` bytes to a
/// trusted dialog's private channel.
pub(in crate::app) fn write_private_frame_bounded<T: serde::Serialize>(
    writer: &mut impl Write,
    value: &T,
    maximum: usize,
) -> Result<()> {
    let body = serde_json::to_vec(value).context("encode private dialog frame")?;
    if body.is_empty() || body.len() > maximum {
        bail!("private dialog frame exceeds limit");
    }
    writer.write_all(&u32::try_from(body.len())?.to_be_bytes())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

fn read_private_frame<T: serde::de::DeserializeOwned>(reader: &mut impl Read) -> Result<T> {
    read_private_frame_bounded(reader, MAX_CONSENT_FRAME_BYTES)
}

fn write_private_frame<T: serde::Serialize>(writer: &mut impl Write, value: &T) -> Result<()> {
    write_private_frame_bounded(writer, value, MAX_CONSENT_FRAME_BYTES)
}

/// The one set of characters trusted UI never shows as is, shared with the
/// **Agent access** list.
fn unsafe_label_character(character: char) -> bool {
    splinterm::frontend::unsafe_display_character(character)
}

/// At most `maximum` characters of `value`, with control and bidi-formatting
/// characters replaced, so daemon- or peer-supplied text cannot restyle or
/// reorder trusted UI. The upgrade and launcher dialogs use this; the
/// access prompt escapes instead, as the **Agent access** list does (see
/// `display_name`).
pub(in crate::app) fn trusted_label(value: &str, maximum: usize) -> String {
    value
        .chars()
        .take(maximum)
        .map(|character| {
            if unsafe_label_character(character) {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

/// The longest agent or workspace name the prompt shows, in characters;
/// longer names end in an ellipsis. Fixed text always comes first on a line,
/// so a long name can never push it out of view.
const DISPLAY_NAME_CHARS: usize = 60;

/// The widest line the prompt draws, in characters. Longer text wraps onto
/// indented lines, so nothing the person must read is ever cut off; a
/// narrower Window wraps it narrower still.
const PROMPT_LINE_CHARS: usize = 100;

/// An untrusted name or path (the agent's reported name, the program, a
/// workspace name) as one line of display text, escaped exactly as the
/// **Agent access** list escapes it, so the name approved here reads the
/// same there later: every unsafe character, tabs and newlines among them,
/// every quote and quote look-alike, and the list's separator are shown as
/// `\u{…}`, and a run of spaces as one space. At most `maximum` characters,
/// the last an ellipsis when it was cut; an escape is never cut in two.
pub(in crate::app) fn display_name(value: &str, maximum: usize) -> String {
    let shown = splinterm::frontend::shown_prompt_name(value, maximum);
    // Cut text gets its ellipsis past `maximum`; uncut text always fits.
    if shown.chars().count() <= maximum {
        return shown;
    }
    splinterm::frontend::shown_prompt_name(value, maximum.saturating_sub(1))
}

/// One line of the prompt, wrapped at [`PROMPT_LINE_CHARS`] at most; every
/// wrapped row after the first is indented by `indent` columns. The Window
/// wraps it at spaces, and inside a word (a path) only where it must.
pub(in crate::app) fn prompt_line(text: impl Into<String>, indent: usize) -> DialogLine {
    DialogLine::new(text)
        .with_indent(indent)
        .with_wrap_at(PROMPT_LINE_CHARS)
}

/// The columns a key in the prompt's key/value table takes, its padding
/// included; every value starts there, and wraps back to it.
const KEY_COLUMNS: usize = 10;

/// One row of the prompt's key/value table: `key` in secondary text, then
/// `value`, wrapped under itself.
fn table_line(key: &str, value: impl AsRef<str>) -> DialogLine {
    prompt_line(
        format!("{key:<KEY_COLUMNS$}{}", value.as_ref()),
        KEY_COLUMNS,
    )
    .with_lead(KEY_COLUMNS)
}

/// The target's row of the table. A workspace's name is user data any
/// agent with Manage can change, so its short ID always comes first.
fn consent_target_line(target: &ConsentTarget) -> DialogLine {
    match target {
        ConsentTarget::Splint {
            splint_id,
            incarnation,
        } => table_line(
            "splint",
            format!("{splint_id:?} · incarnation {incarnation}"),
        ),
        ConsentTarget::Lair { lair_id, lair_name } => {
            let short_id: String = lair_id.to_string().chars().take(8).collect();
            table_line(
                "workspace",
                format!(
                    "{short_id} '{}'",
                    display_name(lair_name, DISPLAY_NAME_CHARS)
                ),
            )
        }
    }
}

/// The workspace name as grant labels and duration rows show it.
pub(in crate::app) fn workspace_label(target: &ConsentTarget) -> String {
    let ConsentTarget::Lair { lair_name, .. } = target else {
        return String::new();
    };
    display_name(lair_name, DISPLAY_NAME_CHARS)
}

/// What the grant names: the level, "Custom access" for any other workspace
/// scope set, or "this access" for a Splint request.
pub(in crate::app) fn access_label(
    level: Option<AccessLevel>,
    target: &ConsentTarget,
) -> &'static str {
    match (level, target) {
        (Some(level), _) => level.label(),
        (None, ConsentTarget::Lair { .. }) => "Custom access",
        (None, ConsentTarget::Splint { .. }) => "this access",
    }
}

/// A grant's exact words for one duration (ADR 0014), for example
/// "Always allow Manage for 'Workspace'".
fn grant_label(access: &str, duration: GrantDuration, workspace: &str) -> String {
    match duration {
        GrantDuration::Once => format!("Allow {access} once"),
        GrantDuration::WorkspaceSession => format!("Allow {access} while '{workspace}' is open"),
        GrantDuration::Remembered => format!("Always allow {access} for '{workspace}'"),
    }
}

/// The green button's label for one duration: the grant without the
/// workspace's name, which the request sentence, the table and the chosen
/// duration's row all show, so it always fits half the box.
pub(in crate::app) fn button_label(access: &str, duration: GrantDuration) -> String {
    match duration {
        GrantDuration::Once => format!("Allow {access} once"),
        GrantDuration::WorkspaceSession => format!("Allow {access} while open"),
        GrantDuration::Remembered => format!("Always allow {access}"),
    }
}

pub(in crate::app) fn duration_row(duration: GrantDuration, workspace: &str) -> String {
    match duration {
        GrantDuration::Once => "Just this once: five minutes, for this process only".to_owned(),
        GrantDuration::WorkspaceSession => format!(
            "While '{workspace}' is open: until this workspace is closed or Splinterm restarts"
        ),
        GrantDuration::Remembered => {
            format!("Always for '{workspace}': until you revoke it or the workspace is removed")
        }
    }
}

fn replacement_line(
    replaced: &ConsentReplacement,
    duration: GrantDuration,
    workspace: &str,
) -> String {
    let access = match replaced.level {
        Some(level) => level.label().to_owned(),
        None => format!("Custom access ({} scopes)", replaced.scopes.len()),
    };
    format!(
        "This replaces the earlier answer: {}.",
        grant_label(&access, duration, workspace)
    )
}

/// The computer this dialog runs on, as its kernel names it, when that name
/// is a usable hostname: the same name its daemon reports to Windows.
pub(in crate::app) fn local_hostname() -> Option<String> {
    let name = rustix::system::uname();
    usable_daemon_hostname(name.nodename().to_str().ok()?).map(str::to_owned)
}

/// Where the request comes from, as the prompt names it. Every prompt
/// names it, so a local one never leaves the person to assume.
///
/// The process that launched this dialog chose the origin (ADR 0015): the
/// daemon here for its own prompts, a remote Window from its own endpoint.
/// A remote daemon's hostname is its own report and the profile is the
/// person's configuration, so both are escaped as the names are; and since
/// an agent's name is quoted and escaped, "on <host>" inside it can never
/// pass for this prompt's own words.
///
/// The host row keeps up to [`MAX_DAEMON_HOSTNAME_BYTES`] characters of the
/// escaped hostname: any plain hostname whole, but one of quote-like letters
/// such as `ʼ`, each shown as an escape, is visibly cut with "…". The title
/// still says remote.
enum PromptHost {
    /// This computer, by its hostname when it has a usable one.
    Local { hostname: Option<String> },
    /// The daemon a remote Window reaches through an SSH profile.
    Remote {
        profile: String,
        hostname: Option<String>,
        /// The host as the sentence and the notes name it ([`remote_name`]).
        named: String,
    },
}

impl PromptHost {
    fn of(prompt: &ConsentPrompt, local_hostname: Option<&str>) -> Self {
        match &prompt.origin {
            ConsentOrigin::Local => Self::Local {
                hostname: local_hostname.map(|name| display_name(name, MAX_DAEMON_HOSTNAME_BYTES)),
            },
            ConsentOrigin::Remote {
                profile,
                daemon_hostname,
            } => Self::Remote {
                profile: display_name(profile, MAX_REMOTE_PROFILE_NAME_BYTES),
                hostname: daemon_hostname
                    .as_deref()
                    .map(|name| display_name(name, MAX_DAEMON_HOSTNAME_BYTES)),
                named: remote_name(profile, daemon_hostname.as_deref(), local_hostname),
            },
        }
    }

    /// The remote host's name for the sentence and the notes; `None` for
    /// this computer.
    fn remote_name(&self) -> Option<&str> {
        match self {
            Self::Local { .. } => None,
            Self::Remote { named, .. } => Some(named),
        }
    }

    /// The table's `host` row.
    fn line(&self) -> DialogLine {
        let value = match self {
            Self::Local { hostname: None } => "this computer".to_owned(),
            Self::Local {
                hostname: Some(hostname),
            } => format!("this computer ({hostname})"),
            Self::Remote {
                profile,
                hostname: None,
                ..
            } => format!("remote, SSH profile '{profile}'"),
            Self::Remote {
                profile,
                hostname: Some(hostname),
                ..
            } => format!("{hostname} · remote, SSH profile '{profile}'"),
        };
        table_line("host", value)
    }
}

/// A remote host as the sentence and the notes name it: its hostname, or
/// else its profile, bounded as the other names are. When that name is
/// this computer's own hostname (ignoring ASCII case), the profile follows
/// it, as in "wintermute (SSH profile 'work')", so a remote prompt never
/// reads as if it came from here.
fn remote_name(profile: &str, hostname: Option<&str>, local_hostname: Option<&str>) -> String {
    let name = hostname.unwrap_or(profile);
    let shown = display_name(name, DISPLAY_NAME_CHARS);
    let local = local_hostname.and_then(usable_daemon_hostname);
    match usable_daemon_hostname(name).zip(local) {
        Some((name, local)) if name.eq_ignore_ascii_case(local) => format!(
            "{shown} (SSH profile '{}')",
            display_name(profile, MAX_REMOTE_PROFILE_NAME_BYTES)
        ),
        _ => shown,
    }
}

/// The request as one sentence: who asks, on which remote host when it is
/// one, for what, where. The agent's name is its own report, escaped and
/// quoted; the note below says so.
fn request_sentence(prompt: &ConsentPrompt, workspace: &str, remote: Option<&str>) -> String {
    let name = prompt.agent_name.as_ref().map_or_else(
        || "An agent".to_owned(),
        |name| format!("'{}'", display_name(name, DISPLAY_NAME_CHARS)),
    );
    let who = match remote {
        Some(host) => format!("{name} on {host}"),
        None => name,
    };
    match (&prompt.target, prompt.level) {
        (ConsentTarget::Lair { .. }, Some(level)) => {
            format!("{who} wants {} access to '{workspace}'", level.label())
        }
        (ConsentTarget::Lair { .. }, None) => {
            format!("{who} wants Custom access to '{workspace}'")
        }
        (ConsentTarget::Splint { .. }, _) => format!("{who} wants access to one Splint"),
    }
}

/// The lines one chosen duration adds below the duration list.
pub(in crate::app) struct ChosenLines {
    pub(in crate::app) shared: Option<DialogLine>,
    pub(in crate::app) replaced: Option<DialogLine>,
}

/// The lines a chosen duration adds below the list: who shares it and what
/// it replaces.
pub(in crate::app) fn chosen_lines(
    prompt: &ConsentPrompt,
    selected: usize,
    workspace: &str,
) -> ChosenLines {
    let chosen = prompt.durations.get(selected);
    let shared = chosen
        .and_then(|offer| offer.shared_by.as_deref())
        .map(|path| {
            prompt_line(
                shared_identity_line(&display_name(path, MAX_CONSENT_PATH_CHARS)),
                4,
            )
        });
    let replaced = chosen.and_then(|offer| {
        offer
            .replaces
            .as_ref()
            .map(|replaced| prompt_line(replacement_line(replaced, offer.duration, workspace), 4))
    });
    ChosenLines { shared, replaced }
}

/// The prompt's request, the same in the dialog and in the terminal
/// approver (`splinterm approvals`): the frame's title ("remote agent access
/// request" for a remote one), the request sentence, the note that the
/// agent names itself, for a remote request the note that the answer allows
/// it on that host only, and the table of where and who asks and for what,
/// ending with its `scopes` row. Every name and path in it is escaped
/// ([`display_name`]). `local_hostname` names this computer.
pub(in crate::app) fn consent_request_lines(
    prompt: &ConsentPrompt,
    local_hostname: Option<&str>,
) -> Vec<DialogLine> {
    let workspace = workspace_label(&prompt.target);
    let host = PromptHost::of(prompt, local_hostname);
    let remote = host.remote_name();
    let mut lines = vec![
        DialogLine::title(if remote.is_some() {
            "remote agent access request"
        } else {
            "agent access request"
        }),
        prompt_line(request_sentence(prompt, &workspace, remote), 4)
            .with_style(DialogStyle::Strong),
    ];
    if prompt.agent_name.is_some() {
        lines.push(
            prompt_line(
                "The agent names itself; Splinterm can't verify that name.",
                4,
            )
            .with_style(DialogStyle::Secondary),
        );
    }
    if let Some(remote) = remote {
        lines.push(
            prompt_line(format!("Answering here allows it on {remote} only."), 4)
                .with_style(DialogStyle::Secondary),
        );
    }
    lines.push(DialogLine::blank());
    lines.push(host.line());
    lines.push(table_line(
        "program",
        display_name(&prompt.requester, MAX_CONSENT_PATH_CHARS),
    ));
    let on = remote.map_or_else(String::new, |remote| format!(" (on {remote})"));
    lines.push(table_line(
        "process",
        format!(
            "PID {} · UID {}{on}",
            prompt.requester_pid, prompt.requester_uid
        ),
    ));
    lines.push(consent_target_line(&prompt.target));
    lines.push(table_line(
        "access",
        match prompt.level {
            Some(level) => format!("{} ({})", level.label(), level.summary()),
            None if matches!(prompt.target, ConsentTarget::Lair { .. }) => {
                "Custom, exactly these scopes and nothing more".to_owned()
            }
            None => "exactly these scopes, once".to_owned(),
        },
    ));
    let labels: Vec<_> = prompt.scopes.iter().map(|scope| scope.label()).collect();
    lines.push(table_line("scopes", labels.join(", ")));
    lines
}

/// The prompt's text with the duration at `selected` chosen: the request
/// ([`consent_request_lines`]), how long, and the answer hint; the frame's
/// title and the two buttons' labels, the green one naming the chosen
/// grant. The chosen duration's lines sit in slots, so every selection has
/// the same rows in the same places at any width, and choosing a duration
/// never moves the rows the pointer selects. `local_hostname` names this
/// computer.
fn consent_text(
    prompt: &ConsentPrompt,
    selected: usize,
    local_hostname: Option<&str>,
) -> DialogText {
    if remote_takeover(prompt) {
        return takeover_text(prompt, local_hostname);
    }
    let workspace = workspace_label(&prompt.target);
    let mut lines = consent_request_lines(prompt, local_hostname);
    lines.push(DialogLine::blank());
    lines.push(DialogLine::new("how long").with_style(DialogStyle::Group));
    for (index, offer) in prompt.durations.iter().enumerate() {
        let (marker, style) = match (offer.withheld, index == selected) {
            (Some(_), _) => ("(-)", DialogStyle::Secondary),
            (None, true) => ("(•)", DialogStyle::Selected),
            (None, false) => ("( )", DialogStyle::Text),
        };
        lines.push(
            prompt_line(
                format!("{marker} {}", duration_row(offer.duration, &workspace)),
                4,
            )
            .with_style(style)
            .with_duration(index),
        );
        if let Some(withheld) = offer.withheld {
            lines.push(
                prompt_line(format!("    Unavailable: {}", withheld.reason()), 6)
                    .with_style(DialogStyle::Secondary),
            );
        }
    }
    lines.push(DialogLine::blank());
    // Each slot takes the rows of its tallest selection, blank when it does
    // not apply; an empty line only marks where the slot is.
    let ChosenLines { shared, replaced } = chosen_lines(prompt, selected, &workspace);
    lines.push(shared.unwrap_or_default().in_slot(0));
    lines.push(replaced.unwrap_or_default().in_slot(1));
    lines.push(
        DialogLine::hint(
            "G or the green Allow button (right) grants. Enter, Escape, D, or the red Deny button (left) deny.",
        )
        .with_wrap_at(PROMPT_LINE_CHARS),
    );
    lines.push(DialogLine::deny("Deny"));
    lines.push(DialogLine::allow(button_label(
        access_label(prompt.level, &prompt.target),
        prompt
            .durations
            .get(selected)
            .map_or(GrantDuration::Once, |offer| offer.duration),
    )));
    DialogText {
        lines,
        minimum_rows: 0,
    }
}

/// Whether `prompt` confirms a remote Window's own forced takeover (Plan
/// 0072 M3, Ctrl+Shift+U). No agent's request can take its wording: the
/// remote daemon may offer the Window no other Splint prompt
/// (`OfferedConsentPrompt::validate`), the Window shows this one only while
/// one of its panes forces that Splint, and the origin is the Window's own.
/// The daemon's own dialog for a local Window's force keeps the agent
/// request's wording for now.
fn remote_takeover(prompt: &ConsentPrompt) -> bool {
    matches!(prompt.origin, ConsentOrigin::Remote { .. }) && prompt.is_forced_takeover()
}

/// A remote Window's forced-takeover confirmation: what it does, on which
/// host and Splint, and its two buttons. It offers no duration to choose,
/// and says nothing of an agent or access.
fn takeover_text(prompt: &ConsentPrompt, local_hostname: Option<&str>) -> DialogText {
    let host = PromptHost::of(prompt, local_hostname);
    let mut lines = vec![
        DialogLine::title("Force control"),
        prompt_line("Take control of this pane from whoever holds it now?", 4)
            .with_style(DialogStyle::Strong),
        DialogLine::blank(),
        host.line(),
        consent_target_line(&prompt.target),
        DialogLine::blank(),
    ];
    lines.push(
        DialogLine::hint(
            "G or the green Take control button (right) takes it. Enter, Escape, D, or the red Cancel button (left) cancel.",
        )
        .with_wrap_at(PROMPT_LINE_CHARS),
    );
    lines.push(DialogLine::deny("Cancel"));
    lines.push(DialogLine::allow("Take control"));
    DialogText {
        lines,
        minimum_rows: 0,
    }
}

/// Renders the prompt's text for each selection; the Window lays it out at
/// one size for all of them. This computer's hostname is read once.
fn consent_renderer(prompt: ConsentPrompt) -> splinterm::AccessConsentRenderer {
    let local_hostname = local_hostname();
    Box::new(move |selected| consent_text(&prompt, selected, local_hostname.as_deref()))
}

/// The snapshot identity every rendering of the prompt carries.
fn consent_identity(target: &ConsentTarget) -> DialogIdentity {
    let (splint_id, incarnation) = match target {
        ConsentTarget::Splint {
            splint_id,
            incarnation,
        } => (*splint_id, *incarnation),
        ConsentTarget::Lair { .. } => (splinterm_core::SplintId::new(), 0),
    };
    DialogIdentity {
        title: "Trusted access request".to_owned(),
        splint_id,
        incarnation,
    }
}

/// Shows a trusted dialog Window and returns once it closes.
///
/// When Hyprland maps trusted dialogs without keyboard focus (the Omarchy
/// integration's module, or the same rule in the person's own file), the
/// dialog shortcut is also registered with Hyprland, in the background while
/// the Window maps, so it reaches the dialog from any Window
/// (`splinterd::dialog_shortcut`). The caller writes its answer first and
/// then finishes the returned registration; the process that reaps this one
/// removes it again. Otherwise the shortcut works only inside the dialog
/// once it has focus.
pub(in crate::app) fn run_trusted_dialog_window(
    options: WindowOptions,
    theme: ResolvedTheme,
) -> Result<BackgroundRegistration> {
    let options = WindowOptions { theme, ..options };
    let shortcut = options
        .trusted_consent
        .as_ref()
        .and_then(TrustedConsentUi::shortcut)
        .filter(|_| global_dialog_shortcut());
    let registration = BackgroundRegistration::start(shortcut);
    run_window(options)?;
    Ok(registration)
}

/// Whether trusted dialogs map without keyboard focus here, so they need the
/// Hyprland binding to be reached from the keyboard. Only reads files.
fn global_dialog_shortcut() -> bool {
    super::omarchy_integration::Roots::from_env()
        .is_ok_and(|roots| super::omarchy_integration::dialog_focus_ready(&roots))
}

/// Configures the renderer a trusted dialog child draws with from the
/// person's configuration, as every other Window does. Without it the child
/// draws with the explicit startup fallback font alone, ignoring
/// `main.font` and Fontconfig's `monospace`, and fails where that font is
/// not installed. It returns the theme the dialog draws with: the person's
/// theme as of opening, with its `[colors]` keys ([`dialog_theme`]).
pub(in crate::app) fn configure_dialog_renderer(config: &AppConfig) -> Result<ResolvedTheme> {
    let theme = dialog_theme(super::theme_watch::load_dialog_theme(config));
    super::sessions::configure_picker_renderer(&dialog_config(config), theme)?;
    Ok(theme)
}

/// The configuration a trusted dialog draws with: the person's, but with
/// the default, even padding whatever `[main] padding-*` says. Lopsided
/// padding would move the grid, and the button labels on it, away from
/// the middle of the Window, which no answer depends on but which a person
/// reads as the line between no and yes.
fn dialog_config(config: &AppConfig) -> AppConfig {
    AppConfig {
        padding: splinterm::geometry::TerminalPadding::DEFAULT,
        ..config.clone()
    }
}

/// What a trusted dialog keeps of the person's theme: its colors only. It is
/// always opaque, so nothing behind it shows through, and it keeps
/// Omarchy's default shell settings, so its text size is the configured
/// font's and never follows the shell's text size.
fn dialog_theme(theme: ResolvedTheme) -> ResolvedTheme {
    ResolvedTheme {
        background_alpha: u16::MAX,
        background_blur: false,
        shell: OmarchyShellSettings::default(),
        ..theme
    }
}

/// The consent dialog's Window at the natural size of its text, answering on
/// `decision`. It counts down the time its asker waits, when the prompt
/// says; zero shows no countdown. Once that runs out it grants nothing, but
/// it never answers by itself: its asker ends it unanswered.
fn consent_window_options(
    prompt: ConsentPrompt,
    decision: std_mpsc::Sender<ConsentDecision>,
    shortcut: Option<DialogShortcut>,
) -> WindowOptions {
    let durations = prompt
        .durations
        .iter()
        .map(|offer| (offer.duration, offer.withheld.is_none()))
        .collect();
    let identity = consent_identity(&prompt.target);
    let respond_within = Duration::from_secs(prompt.respond_within_seconds);
    TrustedConsentUi::access_consent(decision, durations, consent_renderer(prompt))
        .with_shortcut(shortcut)
        .with_respond_within(respond_within)
        .with_identity(identity)
        .into_window_options()
}

pub(in crate::app) fn run_consent_client(
    asker: Option<rustix::process::Pid>,
    shortcut: Option<DialogShortcut>,
    theme: ResolvedTheme,
) -> Result<()> {
    let prompt: ConsentPrompt = read_private_frame(&mut io::stdin().lock())?;
    if let Err(reason) = prompt.validate() {
        bail!("invalid trusted consent prompt: {reason}");
    }
    // An asker that went before the death signal was tied, or before this
    // child even started, is gone now: it is no longer the parent, or its
    // end of the private socket is closed. Nobody would read the answer.
    if rustix::process::getppid() != asker || peer_hung_up(io::stdin().as_fd()) {
        bail!("the process that asked has exited");
    }
    let capability = prompt.capability.clone();
    let (decision, receiver) = std_mpsc::channel();
    let registration =
        run_trusted_dialog_window(consent_window_options(prompt, decision, shortcut), theme)?;
    // Closing the Window, or any way out without an answer, denies.
    let decision = receiver.try_recv().unwrap_or(ConsentDecision::Deny);
    let written = write_private_frame(
        &mut io::stdout().lock(),
        &ConsentReply {
            capability,
            decision,
        },
    );
    // The answer goes first; the shortcut registration ends after it.
    registration.finish();
    written
}

/// Whether the other end of the socket `descriptor` is closed, as it is once
/// the process that asked has exited. Neither asker shuts down its writing
/// half before it reaps the dialog, so a half-close means the same.
fn peer_hung_up(descriptor: BorrowedFd<'_>) -> bool {
    let mut descriptors = [PollFd::new(&descriptor, PollFlags::RDHUP)];
    loop {
        match poll(
            &mut descriptors,
            Some(&Timespec {
                tv_sec: 0,
                tv_nsec: 0,
            }),
        ) {
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return true,
            Ok(_) => {
                return descriptors[0].revents().intersects(
                    PollFlags::HUP | PollFlags::RDHUP | PollFlags::ERR | PollFlags::NVAL,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use splinterm_protocol::{
        AccessLevel, AccessScope, ConsentDurationOffer, ConsentOrigin, ConsentPrompt,
        ConsentReplacement, ConsentTarget, DurationWithheld, GrantDuration, MAX_CONSENT_PATH_CHARS,
    };

    use splinterd::dialog_shortcut::DialogShortcut;

    use splinterm::DialogStyle;
    use splinterm::frontend::dialog_text::{
        CHROME_ROWS, DialogLayout, NATURAL_MAX_COLUMNS, natural_columns, natural_layout,
        natural_rows,
    };

    use super::{
        DISPLAY_NAME_CHARS, button_label, consent_renderer, consent_text, consent_window_options,
        display_name, grant_label, read_private_frame_bounded, trusted_label,
        write_private_frame_bounded,
    };

    /// Every selection's text, as the Window lays it out.
    fn variants(prompt: &ConsentPrompt) -> Vec<splinterm::DialogText> {
        let render = consent_renderer(prompt.clone());
        (0..prompt.durations.len()).map(render).collect()
    }

    /// The prompt with the duration at `selected` chosen, at its natural
    /// size.
    fn laid_out(prompt: &ConsentPrompt, selected: usize) -> DialogLayout {
        natural_layout(&variants(prompt), selected)
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "the layout takes the configured setting, which can be off"
    )]
    fn shortcut() -> Option<DialogShortcut> {
        Some(DialogShortcut::default())
    }

    fn offer(duration: GrantDuration, withheld: Option<DurationWithheld>) -> ConsentDurationOffer {
        ConsentDurationOffer {
            duration,
            withheld,
            shared_by: (withheld.is_none() && duration != GrantDuration::Once)
                .then(|| "/usr/bin/splinterm-mcp".to_owned()),
            replaces: None,
        }
    }

    fn manage_prompt() -> ConsentPrompt {
        ConsentPrompt {
            capability: vec![1; splinterm_protocol::CONSENT_CAPABILITY_BYTES],
            requester: "/usr/bin/splinterm-mcp".to_owned(),
            requester_pid: 42,
            requester_uid: 1000,
            agent_name: Some("Claude Code".to_owned()),
            target: ConsentTarget::Lair {
                lair_id: splinterm_core::LairId::new(),
                lair_name: "Workspace".to_owned(),
            },
            scopes: AccessLevel::Manage.scopes().to_vec(),
            level: Some(AccessLevel::Manage),
            durations: vec![
                offer(GrantDuration::Once, None),
                offer(GrantDuration::WorkspaceSession, None),
                offer(GrantDuration::Remembered, None),
            ],
            respond_within_seconds: 0,
            origin: splinterm_protocol::ConsentOrigin::Local,
        }
    }

    /// The dialog sees its asker go: a closed socket end is a hang-up, an
    /// open one is not, even after the request was read.
    #[test]
    fn a_closed_asker_socket_is_a_hang_up() {
        use std::{
            io::{Read as _, Write as _},
            os::{fd::AsFd as _, unix::net::UnixStream},
        };

        let (mut asker, dialog) = UnixStream::pair().unwrap();
        asker.write_all(b"request").unwrap();
        let mut request = [0_u8; 7];
        (&dialog).read_exact(&mut request).unwrap();
        assert!(!super::peer_hung_up(dialog.as_fd()));
        drop(asker);
        assert!(super::peer_hung_up(dialog.as_fd()));
    }

    #[test]
    fn grant_labels_name_the_exact_choice() {
        assert_eq!(
            grant_label("Look", GrantDuration::Once, "Workspace"),
            "Allow Look once"
        );
        assert_eq!(
            grant_label(
                "Look and type",
                GrantDuration::WorkspaceSession,
                "Workspace"
            ),
            "Allow Look and type while 'Workspace' is open"
        );
        assert_eq!(
            grant_label("Manage", GrantDuration::Remembered, "Workspace"),
            "Always allow Manage for 'Workspace'"
        );
        // The green button names the grant without the workspace, which
        // the sentence, the table and the chosen row show.
        assert_eq!(button_label("Look", GrantDuration::Once), "Allow Look once");
        assert_eq!(
            button_label("Look", GrantDuration::WorkspaceSession),
            "Allow Look while open"
        );
        assert_eq!(
            button_label("Manage", GrantDuration::Remembered),
            "Always allow Manage"
        );
    }

    #[test]
    fn the_prompt_names_the_level_agent_program_and_workspace_with_its_short_id() {
        let prompt = manage_prompt();
        let ConsentTarget::Lair { lair_id, .. } = &prompt.target else {
            unreachable!()
        };
        let layout = laid_out(&prompt, 0);
        let text = layout.rows.join("\n");
        let short_id: String = lair_id.to_string().chars().take(8).collect();
        // The request first, then the note on the agent's name, then the
        // table, then how long, in that order.
        let order = [
            "'Claude Code' wants Manage access to 'Workspace'".to_owned(),
            "The agent names itself; Splinterm can't verify that name.".to_owned(),
            "host      this computer".to_owned(),
            "program   /usr/bin/splinterm-mcp".to_owned(),
            "process   PID 42 · UID 1000".to_owned(),
            format!("workspace {short_id} 'Workspace'"),
            "access    Manage (Read terminals, type commands, and create or close sessions)"
                .to_owned(),
            "scopes    ".to_owned(),
            "how long".to_owned(),
            "(•) Just this once".to_owned(),
        ];
        let rows: Vec<usize> = order
            .iter()
            .map(|wanted| {
                layout
                    .rows
                    .iter()
                    .position(|row| row.starts_with(wanted.as_str()))
                    .unwrap_or_else(|| panic!("{wanted:?} in {text}"))
            })
            .collect();
        assert!(rows.is_sorted(), "{rows:?}: {text}");
        assert_eq!(layout.styles[rows[0]].style, DialogStyle::Strong);
        assert_eq!(layout.styles[rows[1]].style, DialogStyle::Secondary);
        // Each key is a label, its value aligned after it.
        assert_eq!(layout.styles[rows[2]].lead, 10);
        assert_eq!(layout.styles[rows[3]].lead, 10);
        assert_eq!(layout.styles[rows[8]].style, DialogStyle::Group);
        assert_eq!(layout.styles[rows[9]].style, DialogStyle::Selected);
        // The scopes wrap under their key, every label whole in its words.
        let words = text.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(words.contains("start processes in this Lair"), "{text}");
        // Just this once is preselected and needs no shared-identity line.
        assert!(!text.contains("shares this access"));
        // The frame's title and the buttons, which are not body rows.
        let variant = &variants(&prompt)[0];
        assert_eq!(variant.title(), "agent access request");
        assert_eq!(variant.deny_label(), "Deny");
        assert_eq!(variant.allow_label(), "Allow Manage once");
        assert!(!text.contains("Allow Manage once"));
        assert!(text.contains("the red Deny button (left) deny"));
        assert!(text.contains("G or the green Allow button (right) grants"));
        // Without a reported name the request names no one, and needs no
        // note.
        let mut anonymous = manage_prompt();
        anonymous.agent_name = None;
        let text = laid_out(&anonymous, 0).rows.join("\n");
        assert!(text.starts_with("An agent wants Manage access to 'Workspace'"));
        assert!(!text.contains("names itself"));
    }

    #[test]
    fn a_longer_choice_shows_who_shares_it_and_what_it_replaces() {
        let mut prompt = manage_prompt();
        prompt.durations[2].replaces = Some(ConsentReplacement {
            level: Some(AccessLevel::Look),
            scopes: AccessLevel::Look.scopes().to_vec(),
        });
        let text = laid_out(&prompt, 2).rows.join("\n");
        assert!(text.contains("(•) Always for 'Workspace'"));
        assert_eq!(variants(&prompt)[2].allow_label(), "Always allow Manage");
        assert!(text.contains("Any program that runs /usr/bin/splinterm-mcp shares this access."));
        assert!(
            text.contains("This replaces the earlier answer: Always allow Look for 'Workspace'.")
        );
        let session = laid_out(&prompt, 1).rows.join("\n");
        assert!(session.contains("(•) While 'Workspace' is open"));
        assert_eq!(
            variants(&prompt)[1].allow_label(),
            "Allow Manage while open"
        );
        assert!(!session.contains("replaces"));
    }

    #[test]
    fn withheld_durations_show_their_reason_and_custom_lists_every_scope() {
        let mut prompt = manage_prompt();
        prompt.scopes = vec![AccessScope::Observe, AccessScope::ControlTakeover];
        prompt.level = None;
        prompt.durations = vec![
            offer(GrantDuration::Once, None),
            offer(
                GrantDuration::WorkspaceSession,
                Some(DurationWithheld::CustomTakeover),
            ),
            offer(
                GrantDuration::Remembered,
                Some(DurationWithheld::CustomTakeover),
            ),
        ];
        assert_eq!(prompt.validate(), Ok(()));
        let layout = laid_out(&prompt, 0);
        let text = layout.rows.join("\n");
        assert!(text.contains("access    Custom, exactly these scopes and nothing more"));
        assert!(text.contains("'Claude Code' wants Custom access to 'Workspace'"));
        assert!(text.contains("observe visible terminal, take over terminal control"));
        assert!(text.contains("(-) While 'Workspace' is open"));
        assert_eq!(
            text.matches(DurationWithheld::CustomTakeover.reason())
                .count(),
            2
        );
        assert_eq!(
            variants(&prompt)[0].allow_label(),
            "Allow Custom access once"
        );
        // A duration that is not offered, and why, are secondary text.
        let withheld = layout
            .rows
            .iter()
            .position(|row| row.starts_with("(-) While"))
            .unwrap();
        assert_eq!(layout.styles[withheld].style, DialogStyle::Secondary);
        assert_eq!(layout.styles[withheld + 1].style, DialogStyle::Secondary);
        // Each duration's first row is where the layout says it is.
        for (index, duration) in ["Just this once", "While", "Always"]
            .into_iter()
            .enumerate()
        {
            let (row, _) = layout
                .duration_rows
                .iter()
                .find(|(_, shown)| *shown == index)
                .unwrap();
            assert!(
                layout.rows[*row].contains(duration),
                "{}",
                layout.rows[*row]
            );
        }
    }

    /// The longest inputs a valid prompt can carry: maximal paths, names,
    /// and scopes, with every longer duration shared and replacing.
    fn longest_prompt() -> ConsentPrompt {
        let path = format!("/{}", "p".repeat(MAX_CONSENT_PATH_CHARS - 1));
        let shared = format!("/{}", "s".repeat(MAX_CONSENT_PATH_CHARS - 1));
        let mut prompt = manage_prompt();
        prompt.agent_name = Some(format!("Agent  \t name {}", "a".repeat(200))[..128].to_owned());
        prompt.requester.clone_from(&path);
        prompt.target = ConsentTarget::Lair {
            lair_id: splinterm_core::LairId::new(),
            lair_name: "n".repeat(MAX_CONSENT_PATH_CHARS),
        };
        prompt.scopes = vec![
            AccessScope::TopologyObserve,
            AccessScope::Observe,
            AccessScope::Scrollback,
            AccessScope::Input,
            AccessScope::Resize,
            AccessScope::ControlTakeover,
            AccessScope::ProcessSpawn,
            AccessScope::TopologyLayout,
            AccessScope::TopologyName,
            AccessScope::Terminate,
            AccessScope::ProcessRestore,
        ];
        prompt.level = None;
        for offer in &mut prompt.durations[1..] {
            offer.shared_by = Some(shared.clone());
            offer.replaces = Some(ConsentReplacement {
                level: None,
                scopes: prompt.scopes.clone(),
            });
        }
        assert_eq!(prompt.validate(), Ok(()));
        prompt
    }

    /// The profile and hostname the longest remote origin carries.
    fn longest_origin() -> (String, String) {
        (
            "p".repeat(splinterm_protocol::MAX_REMOTE_PROFILE_NAME_BYTES),
            "h".repeat(splinterm_protocol::MAX_DAEMON_HOSTNAME_BYTES),
        )
    }

    /// [`longest_prompt`] as a remote Window shows it: the longest profile
    /// and hostname a valid origin can name, and a countdown.
    fn longest_remote_prompt() -> ConsentPrompt {
        let (profile, hostname) = longest_origin();
        let mut prompt = longest_prompt();
        prompt.origin = ConsentOrigin::Remote {
            profile,
            daemon_hostname: Some(hostname),
        };
        prompt.respond_within_seconds = splinterm_protocol::MAX_CONSENT_RESPOND_WITHIN_SECONDS;
        assert_eq!(prompt.validate(), Ok(()));
        prompt
    }

    /// [`manage_prompt`] from the daemon behind SSH profile `profile`.
    fn remote_prompt(profile: &str, hostname: Option<&str>) -> ConsentPrompt {
        let mut prompt = manage_prompt();
        prompt.origin = ConsentOrigin::Remote {
            profile: profile.to_owned(),
            daemon_hostname: hostname.map(str::to_owned),
        };
        prompt
    }

    /// The confirmation a remote Window shows for its own forced takeover
    /// (Plan 0072 M3), from the daemon behind SSH profile `work`.
    fn takeover_prompt() -> ConsentPrompt {
        let prompt = ConsentPrompt {
            capability: vec![1; splinterm_protocol::CONSENT_CAPABILITY_BYTES],
            requester: "/usr/bin/splinterm-relay".to_owned(),
            requester_pid: 42,
            requester_uid: 1000,
            agent_name: None,
            target: ConsentTarget::Splint {
                splint_id: splinterm_core::SplintId::new(),
                incarnation: 3,
            },
            scopes: vec![AccessScope::ControlTakeover],
            level: None,
            durations: vec![offer(GrantDuration::Once, None)],
            respond_within_seconds: 20,
            origin: ConsentOrigin::Remote {
                profile: "work".to_owned(),
                daemon_hostname: Some("neuromancer".to_owned()),
            },
        };
        assert_eq!(prompt.validate(), Ok(()));
        prompt
    }

    /// Plan 0072 M3: a remote Window's forced-takeover confirmation says
    /// what it does, with Take control and Cancel, and no duration. Nothing
    /// in it reads as an agent's access request. The daemon's own dialog for
    /// a local Window keeps its wording, and any other remote prompt keeps
    /// the agent request's.
    #[test]
    fn a_remote_force_confirmation_says_what_it_does_and_offers_no_duration() {
        let prompt = takeover_prompt();
        let shown = variants(&prompt);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].title(), "Force control");
        assert_eq!(shown[0].allow_label(), "Take control");
        assert_eq!(shown[0].deny_label(), "Cancel");
        let text = words(&prompt, 0);
        assert!(
            text.starts_with("Take control of this pane from whoever holds it now?"),
            "{text}"
        );
        assert!(text.contains("host neuromancer · remote, SSH profile 'work'"));
        assert!(text.contains("incarnation 3"));
        for agent_words in ["agent", "access", "Allow", "once", "how long", "minutes"] {
            assert!(!text.contains(agent_words), "{agent_words:?}: {text}");
        }
        assert!(laid_out(&prompt, 0).duration_rows.is_empty());

        let mut local = prompt.clone();
        local.origin = ConsentOrigin::Local;
        assert_eq!(variants(&local)[0].title(), "agent access request");
        let mut agent = prompt;
        agent.agent_name = Some("Force control".to_owned());
        assert_eq!(variants(&agent)[0].title(), "remote agent access request");
    }

    /// A Splint prompt as the daemon sends it: "Just this once" only.
    fn splint_prompt() -> ConsentPrompt {
        let prompt = ConsentPrompt {
            capability: vec![1; splinterm_protocol::CONSENT_CAPABILITY_BYTES],
            requester: "/usr/bin/splinterm-mcp".to_owned(),
            requester_pid: 42,
            requester_uid: 1000,
            agent_name: None,
            target: ConsentTarget::Splint {
                splint_id: splinterm_core::SplintId::new(),
                incarnation: 3,
            },
            scopes: vec![AccessScope::Observe, AccessScope::Input],
            level: None,
            durations: vec![offer(GrantDuration::Once, None)],
            respond_within_seconds: 0,
            origin: splinterm_protocol::ConsentOrigin::Local,
        };
        assert_eq!(prompt.validate(), Ok(()));
        prompt
    }

    /// Every consent Window gets as far as the compositor: each selection's
    /// snapshot validates, and the Window passes every check `run_window`
    /// makes before it connects. Dialog snapshots once had no visible row
    /// identities, which `TerminalSnapshot::validate` refuses, so no trusted
    /// dialog ever mapped.
    #[test]
    fn every_consent_window_passes_the_checks_before_the_compositor() {
        // The renderer the dialog child configures, from the default
        // configuration: Fontconfig's `monospace`.
        super::configure_dialog_renderer(&splinterm::config::AppConfig::default()).unwrap();
        for prompt in [
            manage_prompt(),
            splint_prompt(),
            longest_prompt(),
            longest_remote_prompt(),
            takeover_prompt(),
        ] {
            for shortcut in [shortcut(), None] {
                let (decision, _receiver) = std::sync::mpsc::channel();
                let options = consent_window_options(prompt.clone(), decision, shortcut);
                let snapshot = options.snapshot.as_ref().unwrap();
                assert_eq!(snapshot.validate().map_err(|error| error.message), Ok(()));
                assert_eq!(usize::from(options.initial_columns), snapshot.columns);
                assert_eq!(usize::from(options.initial_rows), snapshot.rows);
                splinterm::check_window_options(options).unwrap();
            }
        }
    }

    /// The top edge of the dialog's first snapshot, as text.
    fn top_edge(prompt: &ConsentPrompt) -> String {
        let (decision, _receiver) = std::sync::mpsc::channel();
        let options = consent_window_options(prompt.clone(), decision, shortcut());
        options.snapshot.as_ref().unwrap().visible_rows[0]
            .cells
            .iter()
            .map(|cell| cell.content.as_str())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    /// The dialog counts down the time its asker waits, which the prompt
    /// carries, as the time it "denies in"; a prompt without one shows no
    /// countdown. Once the time is up it grants nothing and never answers
    /// by itself: its asker ends it unanswered (the trusted dialog's own
    /// test). Even the shortest prompts, local or remote, have room for
    /// the countdown beside the title at their natural size.
    #[test]
    fn the_prompt_counts_down_its_askers_wait_and_none_without_one() {
        let mut prompt = manage_prompt();
        assert!(!top_edge(&prompt).contains("denies in"));
        for (seconds, shown) in [(20, "0:19"), (120, "1:59")] {
            prompt.respond_within_seconds = seconds;
            assert_eq!(prompt.validate(), Ok(()));
            let top = top_edge(&prompt);
            assert!(top.ends_with(&format!(" denies in {shown}")), "{top:?}");
            assert!(top.contains("agent access request"), "{top:?}");
        }
        let mut splint = splint_prompt();
        splint.respond_within_seconds = 20;
        assert!(top_edge(&splint).ends_with(" denies in 0:19"));
        // The shortest prompts: one scope, no name, a one-letter profile.
        for origin in [
            ConsentOrigin::Local,
            ConsentOrigin::Remote {
                profile: "w".to_owned(),
                daemon_hostname: None,
            },
        ] {
            let mut short = manage_prompt();
            short.agent_name = None;
            short.scopes = AccessLevel::Look.scopes().to_vec();
            short.level = Some(AccessLevel::Look);
            short.durations.truncate(1);
            short.origin = origin;
            short.respond_within_seconds = 20;
            assert_eq!(short.validate(), Ok(()));
            let top = top_edge(&short);
            assert!(top.ends_with(" denies in 0:19"), "{top:?}");
        }
    }

    /// The longest inputs a valid prompt can carry still show every fixed
    /// word and every character of the paths: nothing is cut off.
    #[test]
    fn the_longest_prompt_fits_its_columns_and_cuts_nothing() {
        let path = format!("/{}", "p".repeat(MAX_CONSENT_PATH_CHARS - 1));
        let shared = format!("/{}", "s".repeat(MAX_CONSENT_PATH_CHARS - 1));
        let prompt = longest_prompt();
        let columns = natural_columns(&variants(&prompt));
        for selected in 0..prompt.durations.len() {
            let layout = laid_out(&prompt, selected);
            for line in &layout.rows {
                assert!(
                    line.chars().count() <= super::PROMPT_LINE_CHARS
                        && line.chars().count() <= columns,
                    "{} chars: {line}",
                    line.chars().count()
                );
            }
            // Joined back, the lines hold every character of each path.
            let joined: String = layout
                .rows
                .iter()
                .map(|line| line.trim())
                .collect::<Vec<_>>()
                .concat();
            assert!(joined.contains(&path));
            if selected > 0 {
                assert!(joined.contains(&shared));
            }
            let text = layout.rows.join("\n");
            // The tab is escaped as the list shows it, and the run of spaces
            // shows as one.
            assert!(text.contains("'Agent \\u{0009} name"), "{text}");
            assert!(text.contains(&format!("{}…", "a".repeat(10))));
            assert!(text.contains(&format!("'{}…'", "n".repeat(59))));
            // The natural size keeps every row of the text.
            assert!(!layout.clipped);
            assert!(layout.rows.len() <= natural_rows(&variants(&prompt)));
        }
    }

    #[test]
    fn every_selection_renders_at_one_size_with_the_same_rows() {
        let mut prompt = manage_prompt();
        prompt.durations[2].replaces = Some(ConsentReplacement {
            level: Some(AccessLevel::Look),
            scopes: AccessLevel::Look.scopes().to_vec(),
        });
        let variants = variants(&prompt);
        // At the natural width and narrower ones alike, every selection
        // has the same rows, with the durations on the same ones.
        for columns in [natural_columns(&variants), 72, 50, 30] {
            let layouts: Vec<_> = (0..prompt.durations.len())
                .map(|selected| {
                    splinterm::frontend::dialog_text::layout(
                        &variants,
                        selected,
                        columns,
                        usize::MAX,
                    )
                })
                .collect();
            for layout in &layouts {
                assert_eq!(layout.rows.len(), layouts[0].rows.len(), "{columns}");
                assert_eq!(layout.duration_rows, layouts[0].duration_rows, "{columns}");
                assert!(layout.rows.iter().all(|row| row.chars().count() <= columns));
            }
        }
        // The natural size holds the text and the box around it: the
        // title, the buttons and the legend.
        let text_rows = laid_out(&prompt, 2).rows.len();
        assert_eq!(text_rows + CHROME_ROWS, natural_rows(&variants));
        // A Splint prompt's snapshots carry that Splint's identity.
        let splint = splint_prompt();
        let (decision, _receiver) = std::sync::mpsc::channel();
        let options = consent_window_options(splint.clone(), decision, shortcut());
        let snapshot = options.snapshot.as_ref().unwrap();
        assert_eq!(snapshot.title, "Trusted access request");
        let ConsentTarget::Splint {
            splint_id,
            incarnation,
        } = splint.target
        else {
            unreachable!()
        };
        assert_eq!(
            (snapshot.splint_id, snapshot.incarnation),
            (splint_id, incarnation)
        );
    }

    #[test]
    fn workspace_names_are_bounded_in_labels_and_sanitized() {
        let mut prompt = manage_prompt();
        prompt.target = ConsentTarget::Lair {
            lair_id: splinterm_core::LairId::new(),
            lair_name: format!("{}\u{202e}", "w".repeat(60)),
        };
        let text = laid_out(&prompt, 2).rows.join("\n");
        assert!(text.contains(&format!("(•) Always for '{}…'", "w".repeat(59))));
        assert!(text.contains(&format!("wants Manage access to '{}…'", "w".repeat(59))));
        assert!(!text.contains('\u{202e}'));
        // A shorter name shows the override escaped, as the list does.
        prompt.target = ConsentTarget::Lair {
            lair_id: splinterm_core::LairId::new(),
            lair_name: "w\u{202e}v".to_owned(),
        };
        let text = laid_out(&prompt, 2).rows.join("\n");
        assert!(text.contains("(•) Always for 'w\\u{202e}v'"), "{text}");
        // The bound never cuts an escape in two.
        assert_eq!(
            display_name(&format!("{}\u{202e}", "a".repeat(55)), 60),
            format!("{}…", "a".repeat(55))
        );
        assert_eq!(display_name("abc\u{202e}", 11), "abc\\u{202e}");
    }

    /// The prompt shows a hostile name in exactly the escaped form the
    /// **Agent access** list shows it in, so the name a person approves
    /// reads the same there later, and nothing in it can break a line or
    /// reorder or hide the text around it.
    #[test]
    fn a_workspace_name_cannot_close_the_prompts_quote() {
        let name = "Scratch' once (five minutes, this process only";
        assert_eq!(
            display_name(name, DISPLAY_NAME_CHARS),
            "Scratch\\u{0027} once (five minutes, this process only"
        );
        let mut prompt = manage_prompt();
        prompt.target = ConsentTarget::Lair {
            lair_id: splinterm_core::LairId::new(),
            lair_name: name.to_owned(),
        };
        for selected in 0..prompt.durations.len() {
            let text = laid_out(&prompt, selected).rows.join("\n");
            assert!(!text.contains("Scratch'"), "{text}");
            assert!(text.contains("Scratch\\u{0027}"), "{text}");
        }
    }

    #[test]
    fn the_prompt_escapes_untrusted_names_and_paths_as_the_list_does() {
        let hostile = "evil\u{1b}[31mRED\u{202e}gpj.exe\u{7}\tend";
        let escaped = "evil\\u{001b}[31mRED\\u{202e}gpj.exe\\u{0007}\\u{0009}end";
        assert_eq!(display_name(hostile, DISPLAY_NAME_CHARS), escaped);
        assert_eq!(
            splinterm::frontend::shown_name(hostile, DISPLAY_NAME_CHARS),
            escaped,
            "the list's escaper"
        );
        let mut prompt = manage_prompt();
        prompt.agent_name = Some(hostile.to_owned());
        prompt.requester = format!("/opt/{hostile}");
        prompt.target = ConsentTarget::Lair {
            lair_id: splinterm_core::LairId::new(),
            lair_name: hostile.to_owned(),
        };
        prompt.durations[2].shared_by = Some(format!("/opt/{hostile}"));
        for selected in 0..prompt.durations.len() {
            let layout = laid_out(&prompt, selected);
            for row in &layout.rows {
                assert!(
                    !row.chars()
                        .any(splinterm::frontend::unsafe_display_character),
                    "{row:?}"
                );
            }
            let text = layout.rows.join("\n");
            assert!(
                text.contains(&format!("'{escaped}' wants Manage access to")),
                "{text}"
            );
            assert!(
                text.contains(&format!("program   /opt/{escaped}")),
                "{text}"
            );
            assert!(
                layout
                    .rows
                    .iter()
                    .any(|row| row.starts_with("workspace ")
                        && row.ends_with(&format!(" '{escaped}'"))),
                "{text}"
            );
            if selected == 2 {
                assert!(
                    text.contains(&format!("Any program that runs /opt/{escaped}")),
                    "{text}"
                );
            }
        }
    }

    /// The prompt's laid-out rows for `selected`, with each run of spaces
    /// and every wrap shown as one space.
    fn words(prompt: &ConsentPrompt, selected: usize) -> String {
        laid_out(prompt, selected)
            .rows
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// A remote prompt always says so: the title, the sentence naming the
    /// host, the note that answering allows it there only, the host row
    /// first in the table, and the process on that host. Without a
    /// hostname, the profile names the host.
    #[test]
    fn a_remote_prompt_always_names_its_host_and_says_remote() {
        for (prompt, host, row) in [
            (
                remote_prompt("work", Some("neuromancer")),
                "neuromancer",
                "host      neuromancer · remote, SSH profile 'work'",
            ),
            (
                remote_prompt("work", None),
                "work",
                "host      remote, SSH profile 'work'",
            ),
        ] {
            assert_eq!(prompt.validate(), Ok(()));
            for (selected, variant) in variants(&prompt).iter().enumerate() {
                assert_eq!(variant.title(), "remote agent access request");
                let layout = laid_out(&prompt, selected);
                let text = layout.rows.join("\n");
                let order = [
                    format!("'Claude Code' on {host} wants Manage access to 'Workspace'"),
                    "The agent names itself; Splinterm can't verify that name.".to_owned(),
                    format!("Answering here allows it on {host} only."),
                    row.to_owned(),
                    "program   /usr/bin/splinterm-mcp".to_owned(),
                    format!("process   PID 42 · UID 1000 (on {host})"),
                    "workspace ".to_owned(),
                    "how long".to_owned(),
                ];
                let rows: Vec<usize> = order
                    .iter()
                    .map(|wanted| {
                        layout
                            .rows
                            .iter()
                            .position(|shown| shown.starts_with(wanted.as_str()))
                            .unwrap_or_else(|| panic!("{wanted:?} in {text}"))
                    })
                    .collect();
                assert!(rows.is_sorted(), "{rows:?}: {text}");
                assert_eq!(layout.rows[rows[3]].trim_end(), row);
                assert_eq!(layout.styles[rows[0]].style, DialogStyle::Strong);
                assert_eq!(layout.styles[rows[2]].style, DialogStyle::Secondary);
                assert_eq!(layout.styles[rows[3]].lead, 10);
            }
        }
        // Without a reported name, the host still follows the agent.
        let mut anonymous = remote_prompt("work", Some("neuromancer"));
        anonymous.agent_name = None;
        let text = laid_out(&anonymous, 0).rows.join("\n");
        assert!(
            text.starts_with("An agent on neuromancer wants Manage access to 'Workspace'"),
            "{text}"
        );
        assert!(!text.contains("names itself"));
        assert!(text.contains("Answering here allows it on neuromancer only."));
    }

    /// A local prompt names this computer, by its hostname when it has a
    /// usable one, and never says "remote" anywhere; an agent that names
    /// itself after another host stays a quoted, escaped name.
    #[test]
    fn a_local_prompt_names_this_computer_and_never_says_remote() {
        let host_row = |prompt: &ConsentPrompt, hostname| {
            consent_text(prompt, 0, hostname)
                .lines
                .into_iter()
                .find(|line| line.text.starts_with("host "))
                .map(|line| line.text)
        };
        for prompt in [manage_prompt(), splint_prompt(), longest_prompt()] {
            assert_eq!(
                host_row(&prompt, Some("wintermute")).as_deref(),
                Some("host      this computer (wintermute)")
            );
            assert_eq!(
                host_row(&prompt, None).as_deref(),
                Some("host      this computer")
            );
            for hostname in [Some("wintermute"), None] {
                for selected in 0..prompt.durations.len() {
                    let text = consent_text(&prompt, selected, hostname);
                    assert_eq!(text.title(), "agent access request");
                    for line in &text.lines {
                        assert!(
                            !line.text.to_lowercase().contains("remote")
                                && !line.text.contains("Answering here"),
                            "{:?}",
                            line.text
                        );
                    }
                }
            }
        }
        // Drawn, the row names this computer by its kernel's hostname.
        let shown = laid_out(&manage_prompt(), 0).rows;
        let expected = super::local_hostname().map_or_else(
            || "host      this computer".to_owned(),
            |name| format!("host      this computer ({name})"),
        );
        assert!(
            shown.iter().any(|row| row.trim_end() == expected),
            "{expected:?} in {shown:?}"
        );
        // A local agent named after a remote host cannot pass for one.
        let mut spoof = manage_prompt();
        spoof.agent_name = Some("Claude' on neuromancer".to_owned());
        let text = consent_text(&spoof, 0, Some("wintermute"));
        assert_eq!(text.title(), "agent access request");
        assert!(
            text.lines.iter().any(|line| line.text
                == "'Claude\\u{0027} on neuromancer' wants Manage access to 'Workspace'"),
            "{:?}",
            text.lines
        );
    }

    /// The profile, the hostname, and the agent's name are escaped exactly
    /// as the other names are: a control character, ESC, a bidi override,
    /// or a quote look-alike shows as an escape. A hostile profile or
    /// hostname is not a valid origin, but drawn anyway it is escaped; a
    /// valid hostname may still hold a quote look-alike letter.
    #[test]
    fn the_host_and_agent_names_are_escaped_as_the_other_names() {
        let hostile = "evil\u{1b}[31mRED\u{202e}gpj\u{2019}s";
        let escaped = "evil\\u{001b}[31mRED\\u{202e}gpj\\u{2019}s";
        assert_eq!(display_name(hostile, DISPLAY_NAME_CHARS), escaped);
        for (profile, hostname, row) in [
            (
                hostile,
                Some(hostile),
                format!("host {escaped} · remote, SSH profile '{escaped}'"),
            ),
            (
                hostile,
                None,
                format!("host remote, SSH profile '{escaped}'"),
            ),
            (
                "work",
                Some(hostile),
                format!("host {escaped} · remote, SSH profile 'work'"),
            ),
        ] {
            let mut prompt = remote_prompt(profile, hostname);
            prompt.agent_name = Some(hostile.to_owned());
            assert!(prompt.validate().is_err());
            for selected in 0..prompt.durations.len() {
                for shown in &laid_out(&prompt, selected).rows {
                    assert!(
                        !shown
                            .chars()
                            .any(splinterm::frontend::unsafe_display_character),
                        "{shown:?}"
                    );
                }
                let words = words(&prompt, selected);
                for wanted in [
                    format!("'{escaped}' on {escaped} wants Manage access to"),
                    format!("Answering here allows it on {escaped} only."),
                    row.clone(),
                    format!("UID 1000 (on {escaped})"),
                ] {
                    assert!(words.contains(&wanted), "{wanted:?} in {words}");
                }
            }
        }
        // `ʼ` is a letter, so a valid hostname may hold it; it is still
        // shown escaped.
        let prompt = remote_prompt("work", Some("evilʼs"));
        assert_eq!(prompt.validate(), Ok(()));
        let words = words(&prompt, 0);
        assert!(
            words.contains("'Claude Code' on evil\\u{02bc}s wants"),
            "{words}"
        );
        assert!(
            words.contains("host evil\\u{02bc}s · remote, SSH profile 'work'"),
            "{words}"
        );
    }

    /// The longest remote prompt, with the longest profile and hostname, a
    /// long agent name and workspace name, and a countdown, fits the
    /// dialog's natural size and cuts nothing it must show: the host row
    /// keeps a longest plain hostname and the profile whole, the other
    /// mentions are bounded as names are, and the title and countdown both
    /// fit the top edge. A valid hostname of quote-like letters escapes to
    /// more characters than the row keeps, so it is visibly cut with "…";
    /// the title still says remote and nothing else is cut.
    #[test]
    fn the_longest_remote_prompt_fits_its_columns_and_cuts_nothing() {
        let (profile, hostname) = longest_origin();
        let path = format!("/{}", "p".repeat(MAX_CONSENT_PATH_CHARS - 1));
        let prompt = longest_remote_prompt();
        let variants = variants(&prompt);
        let columns = natural_columns(&variants);
        assert!(columns <= NATURAL_MAX_COLUMNS, "{columns}");
        let bounded = format!("{}…", "h".repeat(DISPLAY_NAME_CHARS - 1));
        for (selected, variant) in variants.iter().enumerate() {
            assert!(variant.buttons_fit(columns));
            let layout = laid_out(&prompt, selected);
            for line in &layout.rows {
                assert!(
                    line.chars().count() <= super::PROMPT_LINE_CHARS
                        && line.chars().count() <= columns,
                    "{} chars: {line}",
                    line.chars().count()
                );
            }
            let joined: String = layout
                .rows
                .iter()
                .map(|line| line.trim())
                .collect::<Vec<_>>()
                .concat();
            assert!(joined.contains(&hostname));
            assert!(joined.contains(&format!("'{profile}'")));
            assert!(joined.contains(&path));
            let words = words(&prompt, selected);
            for wanted in [
                format!("on {bounded} wants Custom access to"),
                format!("Answering here allows it on {bounded} only."),
                format!("(on {bounded})"),
                format!("'{}…'", "n".repeat(DISPLAY_NAME_CHARS - 1)),
            ] {
                assert!(words.contains(&wanted), "{wanted:?} in {words}");
            }
            assert!(!layout.clipped);
            assert!(layout.rows.len() + CHROME_ROWS <= natural_rows(&variants));
        }
        let top = top_edge(&prompt);
        assert!(
            top.contains("remote agent access request") && top.ends_with(" denies in 1:59"),
            "{top:?}"
        );
        // 127 `ʼ`, 254 bytes, escape to 1016 characters: the row keeps the
        // first 31 escapes and an ellipsis, and still fits.
        let mut escaping = longest_remote_prompt();
        escaping.origin = ConsentOrigin::Remote {
            profile: profile.clone(),
            daemon_hostname: Some("ʼ".repeat(127)),
        };
        assert_eq!(escaping.validate(), Ok(()));
        let columns = natural_columns(&self::variants(&escaping));
        for selected in 0..escaping.durations.len() {
            let layout = laid_out(&escaping, selected);
            assert!(!layout.clipped);
            assert!(
                layout
                    .rows
                    .iter()
                    .all(|line| line.chars().count() <= columns)
            );
            // The run of escapes is one word that wraps inside itself, so
            // it is checked with the rows joined back: 31 escapes, then the
            // ellipsis.
            let joined: String = layout.rows.iter().map(|line| line.trim()).collect();
            let cut = format!("host{}…", "\\u{02bc}".repeat(31));
            assert!(joined.contains(&cut), "{joined}");
            let words = words(&escaping, selected);
            let rest = format!("… · remote, SSH profile '{profile}'");
            assert!(words.contains(&rest), "{words}");
        }
        assert!(top_edge(&escaping).contains("remote agent access request"));
    }

    /// A remote host named like this computer never reads as this
    /// computer: wherever the prompt names it in words, the profile
    /// follows. The host row already says remote and is unchanged.
    #[test]
    fn a_remote_host_named_like_this_computer_names_its_profile_too() {
        let line = |text: &splinterm::DialogText, start: &str| {
            text.lines
                .iter()
                .find(|line| line.text.starts_with(start))
                .map_or_else(
                    || panic!("{start:?} in {:?}", text.lines),
                    |line| line.text.clone(),
                )
        };
        for (prompt, host, row) in [
            (
                remote_prompt("work", Some("WinterMute")),
                "WinterMute (SSH profile 'work')",
                "host      WinterMute · remote, SSH profile 'work'",
            ),
            (
                remote_prompt("wintermute", None),
                "wintermute (SSH profile 'wintermute')",
                "host      remote, SSH profile 'wintermute'",
            ),
            (
                remote_prompt("work", Some("neuromancer")),
                "neuromancer",
                "host      neuromancer · remote, SSH profile 'work'",
            ),
        ] {
            assert_eq!(prompt.validate(), Ok(()));
            let text = consent_text(&prompt, 0, Some("wintermute"));
            assert_eq!(text.title(), "remote agent access request");
            assert_eq!(
                line(&text, "'Claude Code'"),
                format!("'Claude Code' on {host} wants Manage access to 'Workspace'")
            );
            assert_eq!(
                line(&text, "Answering here"),
                format!("Answering here allows it on {host} only.")
            );
            assert!(line(&text, "process").ends_with(&format!(" (on {host})")));
            assert_eq!(line(&text, "host "), row);
        }
        // Without a usable local hostname nothing can collide.
        let text = consent_text(&remote_prompt("work", Some("wintermute")), 0, None);
        assert!(line(&text, "'Claude Code'").starts_with("'Claude Code' on wintermute wants"));
    }

    #[test]
    fn trusted_dialog_labels_replace_control_characters() {
        assert_eq!(
            trusted_label("safe\nspoof\u{1b}\u{202e}rtl\u{2066}", 64),
            "safe�spoof��rtl�"
        );
        assert_eq!(trusted_label("abcdef", 3), "abc");
    }

    #[test]
    fn private_frames_round_trip_within_their_bound_only() {
        let mut buffer = Vec::new();
        write_private_frame_bounded(&mut buffer, &vec![1_u8, 2, 3], 64).unwrap();
        let decoded: Vec<u8> = read_private_frame_bounded(&mut buffer.as_slice(), 64).unwrap();
        assert_eq!(decoded, [1, 2, 3]);
        assert!(read_private_frame_bounded::<Vec<u8>>(&mut buffer.as_slice(), 4).is_err());
        assert!(write_private_frame_bounded(&mut Vec::new(), &"x".repeat(64), 16).is_err());
    }

    /// A trusted dialog takes the theme's colours only: it is opaque and
    /// keeps the default shell settings, so Omarchy's `[font] base-size`
    /// never changes its text size.
    #[test]
    fn dialogs_take_the_theme_colors_but_not_its_shell_settings() {
        use splinterm::config::{OmarchyShellSettings, ResolvedTheme};
        let mut theme = ResolvedTheme {
            background: 0x10_20_30,
            background_alpha: 1_000,
            background_blur: true,
            ..ResolvedTheme::default()
        };
        theme.shell.text_size_px = Some(22);
        theme.shell.ui_text_size_px = Some(19);
        theme.shell.menu_alpha = 7;
        let dialog = super::dialog_theme(theme);
        assert_eq!(dialog.shell, OmarchyShellSettings::default());
        assert_eq!(dialog.background_alpha, u16::MAX);
        assert!(!dialog.background_blur);
        assert_eq!(dialog.background, 0x10_20_30);
        assert_eq!(dialog.roles, theme.roles);
    }

    /// A trusted dialog keeps the person's font but never their padding:
    /// lopsided `[main] padding-*` cannot move its grid off the middle.
    #[test]
    fn trusted_dialogs_draw_with_even_padding() {
        for padding in [
            splinterm::geometry::TerminalPadding {
                left: 400,
                right: 12,
                top: 0,
                bottom: 900,
            },
            splinterm::geometry::TerminalPadding {
                left: 12,
                right: 400,
                top: 30,
                bottom: 0,
            },
        ] {
            let config = splinterm::config::AppConfig {
                padding,
                font: "Lopsided Mono".to_owned(),
                ..splinterm::config::AppConfig::default()
            };
            let dialog = super::dialog_config(&config);
            assert_eq!(
                dialog.padding,
                splinterm::geometry::TerminalPadding::DEFAULT
            );
            assert_eq!(dialog.font, config.font);
        }
    }
}
