use std::{
    io::{self, Read, Write},
    sync::mpsc as std_mpsc,
};

use anyhow::{Context, Result, bail};
use splinterd::dialog_shortcut::{BackgroundRegistration, DialogShortcut};
use splinterm::{
    DialogIdentity, DialogLine, DialogText, TrustedConsentUi, WindowOptions,
    config::{AppConfig, ResolvedTheme},
    run_window,
};
use splinterm_protocol::{
    AccessLevel, ConsentDecision, ConsentPrompt, ConsentReplacement, ConsentReply, ConsentTarget,
    GrantDuration, MAX_CONSENT_FRAME_BYTES, MAX_CONSENT_PATH_CHARS, shared_identity_line,
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
/// the list's quotes and separator, and the prompt's own `'` quote are shown
/// as `\u{…}`, and spaces stay as they are. At most `maximum` characters,
/// the last an ellipsis when it was cut; an escape is never cut in two.
fn display_name(value: &str, maximum: usize) -> String {
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
fn prompt_line(text: impl Into<String>, indent: usize) -> DialogLine {
    DialogLine::new(text)
        .with_indent(indent)
        .with_wrap_at(PROMPT_LINE_CHARS)
}

fn consent_target_line(target: &ConsentTarget) -> String {
    match target {
        ConsentTarget::Splint {
            splint_id,
            incarnation,
        } => format!("Splint: {splint_id:?} · incarnation {incarnation}"),
        // The name is user data any agent with Manage can change, so the
        // short ID is always shown, before it.
        ConsentTarget::Lair { lair_id, lair_name } => {
            let short_id: String = lair_id.to_string().chars().take(8).collect();
            format!(
                "Workspace {short_id}: '{}'",
                display_name(lair_name, DISPLAY_NAME_CHARS)
            )
        }
    }
}

/// The workspace name as grant labels and duration rows show it.
fn workspace_label(target: &ConsentTarget) -> String {
    let ConsentTarget::Lair { lair_name, .. } = target else {
        return String::new();
    };
    display_name(lair_name, DISPLAY_NAME_CHARS)
}

/// What the grant names: the level, "Custom access" for any other workspace
/// scope set, or "this access" for a Splint request.
fn access_label(level: Option<AccessLevel>, target: &ConsentTarget) -> &'static str {
    match (level, target) {
        (Some(level), _) => level.label(),
        (None, ConsentTarget::Lair { .. }) => "Custom access",
        (None, ConsentTarget::Splint { .. }) => "this access",
    }
}

/// The grant button's exact words for one duration (ADR 0014), for example
/// "Always allow Manage for 'Workspace'".
fn grant_label(access: &str, duration: GrantDuration, workspace: &str) -> String {
    match duration {
        GrantDuration::Once => format!("Allow {access} once"),
        GrantDuration::WorkspaceSession => format!("Allow {access} while '{workspace}' is open"),
        GrantDuration::Remembered => format!("Always allow {access} for '{workspace}'"),
    }
}

fn duration_row(duration: GrantDuration, workspace: &str) -> String {
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

/// The scope labels, wrapped under their heading.
fn scope_line(prompt: &ConsentPrompt) -> DialogLine {
    let labels: Vec<_> = prompt.scopes.iter().map(|scope| scope.label()).collect();
    prompt_line(format!("Scopes: {}", labels.join(", ")), 8)
}

/// The lines one chosen duration adds below the duration list.
struct ChosenLines {
    shared: Option<DialogLine>,
    replaced: Option<DialogLine>,
    grant: DialogLine,
}

/// The lines a chosen duration adds below the list: who shares it, what it
/// replaces, and the grant action that names it exactly.
fn chosen_lines(prompt: &ConsentPrompt, selected: usize, workspace: &str) -> ChosenLines {
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
    let label = grant_label(
        access_label(prompt.level, &prompt.target),
        chosen.map_or(GrantDuration::Once, |offer| offer.duration),
        workspace,
    );
    ChosenLines {
        shared,
        replaced,
        grant: prompt_line(format!("Grant: {label}"), 7),
    }
}

/// The prompt's text with the duration at `selected` chosen. The chosen
/// duration's lines sit in slots, so every selection has the same rows in
/// the same places at any width, and choosing a duration never moves the
/// rows the pointer selects.
fn consent_text(
    prompt: &ConsentPrompt,
    selected: usize,
    shortcut: Option<DialogShortcut>,
) -> DialogText {
    let workspace = workspace_label(&prompt.target);
    let mut lines = vec![
        DialogLine::new("TRUSTED SPLINTERM ACCESS REQUEST"),
        DialogLine::blank(),
    ];
    if let Some(name) = &prompt.agent_name {
        lines.push(prompt_line(
            format!(
                "Agent (reported by the agent, not verified): {}",
                display_name(name, DISPLAY_NAME_CHARS)
            ),
            4,
        ));
    }
    lines.push(prompt_line(
        format!(
            "Program: {}",
            display_name(&prompt.requester, MAX_CONSENT_PATH_CHARS)
        ),
        9,
    ));
    lines.push(prompt_line(
        format!(
            "Process: PID {} · UID {}",
            prompt.requester_pid, prompt.requester_uid
        ),
        4,
    ));
    lines.push(prompt_line(consent_target_line(&prompt.target), 4));
    lines.push(DialogLine::blank());
    lines.push(prompt_line(
        match prompt.level {
            Some(level) => format!("Access: {} ({})", level.label(), level.summary()),
            None if matches!(prompt.target, ConsentTarget::Lair { .. }) => {
                "Access: Custom, exactly these scopes and nothing more".to_owned()
            }
            None => "Access: exactly these scopes, once".to_owned(),
        },
        4,
    ));
    lines.push(scope_line(prompt));
    lines.push(DialogLine::blank());
    lines.push(prompt_line(
        "How long (choose with the arrow keys or a click):",
        4,
    ));
    for (index, offer) in prompt.durations.iter().enumerate() {
        let marker = match (offer.withheld, index == selected) {
            (Some(_), _) => "(-)",
            (None, true) => "(•)",
            (None, false) => "( )",
        };
        lines.push(
            prompt_line(
                format!("  {marker} {}", duration_row(offer.duration, &workspace)),
                6,
            )
            .with_duration(index),
        );
        if let Some(withheld) = offer.withheld {
            lines.push(prompt_line(
                format!("      Unavailable: {}", withheld.reason()),
                8,
            ));
        }
    }
    lines.push(DialogLine::blank());
    // Each slot takes the rows of its tallest selection, blank when it does
    // not apply; an empty line only marks where the slot is.
    let ChosenLines {
        shared,
        replaced,
        grant,
    } = chosen_lines(prompt, selected, &workspace);
    lines.push(shared.unwrap_or_default().in_slot(0));
    lines.push(replaced.unwrap_or_default().in_slot(1));
    lines.push(DialogLine::blank());
    lines.push(grant.in_slot(2));
    lines.push(
        DialogLine::hint(
            "G or the green area (right) grants. Enter, Escape, D, or the red area (left) deny.",
        )
        .with_wrap_at(PROMPT_LINE_CHARS),
    );
    lines.push(DialogLine::hint(keyboard_hint("prompt", shortcut)).with_wrap_at(PROMPT_LINE_CHARS));
    DialogText {
        lines,
        minimum_rows: 0,
    }
}

/// Renders the prompt's text for each selection; the Window lays it out at
/// one size for all of them.
fn consent_renderer(
    prompt: ConsentPrompt,
    shortcut: Option<DialogShortcut>,
) -> splinterm::AccessConsentRenderer {
    Box::new(move |selected| consent_text(&prompt, selected, shortcut))
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

/// The line that tells the person how to engage a trusted dialog's keyboard:
/// a click inside, or the dialog shortcut when it is on.
pub(in crate::app) fn keyboard_hint(noun: &str, shortcut: Option<DialogShortcut>) -> String {
    shortcut.map_or_else(
        || format!("Click inside this {noun} first to answer with the keyboard."),
        |shortcut| {
            format!(
                "Click inside this {noun} or press {} first to answer with the keyboard.",
                shortcut.label()
            )
        },
    )
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
/// theme as of opening, with its `[colors]` keys, always opaque, so nothing
/// behind the dialog shows through it.
pub(in crate::app) fn configure_dialog_renderer(config: &AppConfig) -> Result<ResolvedTheme> {
    let theme = ResolvedTheme {
        background_alpha: u16::MAX,
        background_blur: false,
        ..super::theme_watch::load_dialog_theme(config)
    };
    super::sessions::configure_picker_renderer(config, theme)?;
    Ok(theme)
}

/// The consent dialog's Window at the natural size of its text, answering on
/// `decision`.
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
    TrustedConsentUi::access_consent(decision, durations, consent_renderer(prompt, shortcut))
        .with_shortcut(shortcut)
        .with_identity(identity)
        .into_window_options()
}

pub(in crate::app) fn run_consent_client(
    shortcut: Option<DialogShortcut>,
    theme: ResolvedTheme,
) -> Result<()> {
    let prompt: ConsentPrompt = read_private_frame(&mut io::stdin().lock())?;
    if let Err(reason) = prompt.validate() {
        bail!("invalid trusted consent prompt: {reason}");
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

#[cfg(test)]
mod tests {
    use splinterm_protocol::{
        AccessLevel, AccessScope, ConsentDurationOffer, ConsentPrompt, ConsentReplacement,
        ConsentTarget, DurationWithheld, GrantDuration, MAX_CONSENT_PATH_CHARS,
    };

    use splinterd::dialog_shortcut::DialogShortcut;

    use splinterm::frontend::dialog_text::{
        DialogLayout, natural_columns, natural_layout, natural_rows,
    };

    use super::{
        DISPLAY_NAME_CHARS, consent_renderer, consent_window_options, display_name, grant_label,
        keyboard_hint, read_private_frame_bounded, trusted_label, write_private_frame_bounded,
    };

    /// Every selection's text, as the Window lays it out.
    fn variants(prompt: &ConsentPrompt) -> Vec<splinterm::DialogText> {
        let render = consent_renderer(prompt.clone(), shortcut());
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

    #[test]
    fn the_keyboard_hint_names_the_dialog_shortcut_only_when_it_is_on() {
        assert_eq!(
            keyboard_hint("prompt", shortcut()),
            "Click inside this prompt or press Ctrl+Alt+G first to answer with the keyboard."
        );
        assert_eq!(
            keyboard_hint(
                "dialog",
                DialogShortcut::parse_setting("ctrl+alt+7").unwrap()
            ),
            "Click inside this dialog or press Ctrl+Alt+7 first to answer with the keyboard."
        );
        assert_eq!(
            keyboard_hint("dialog", None),
            "Click inside this dialog first to answer with the keyboard."
        );
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
        }
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
    }

    #[test]
    fn the_prompt_names_the_level_agent_program_and_workspace_with_its_short_id() {
        let prompt = manage_prompt();
        let ConsentTarget::Lair { lair_id, .. } = &prompt.target else {
            unreachable!()
        };
        let text = laid_out(&prompt, 0).rows.join("\n");
        assert!(text.contains("Agent (reported by the agent, not verified): Claude Code"));
        assert!(text.contains("Program: /usr/bin/splinterm-mcp"));
        let short_id: String = lair_id.to_string().chars().take(8).collect();
        assert!(text.contains(&format!("Workspace {short_id}: 'Workspace'")));
        assert!(text.contains(
            "Access: Manage (Read terminals, type commands, and create or close sessions)"
        ));
        assert!(text.contains("start processes in this Lair"));
        // Just this once is preselected and needs no shared-identity line.
        assert!(text.contains("(•) Just this once"));
        assert!(text.contains("Grant: Allow Manage once"));
        assert!(!text.contains("shares this access"));
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
        assert!(text.contains("Grant: Always allow Manage for 'Workspace'"));
        assert!(text.contains("Any program that runs /usr/bin/splinterm-mcp shares this access."));
        assert!(
            text.contains("This replaces the earlier answer: Always allow Look for 'Workspace'.")
        );
        let session = laid_out(&prompt, 1).rows.join("\n");
        assert!(session.contains("Grant: Allow Manage while 'Workspace' is open"));
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
        assert!(text.contains("Access: Custom, exactly these scopes and nothing more"));
        assert!(text.contains("observe visible terminal, take over terminal control"));
        assert!(text.contains("(-) While 'Workspace' is open"));
        assert_eq!(
            text.matches(DurationWithheld::CustomTakeover.reason())
                .count(),
            2
        );
        assert!(text.contains("Grant: Allow Custom access once"));
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
        for prompt in [manage_prompt(), splint_prompt(), longest_prompt()] {
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
            // The tab is escaped as the list shows it; spaces stay.
            assert!(
                text.contains("Agent (reported by the agent, not verified): Agent  \\u{0009} name")
            );
            assert!(text.contains(&format!("{}…", "a".repeat(10))));
            assert!(text.contains(&format!("'{}…'", "n".repeat(59))));
            assert!(text.contains(
                "Click inside this prompt or press Ctrl+Alt+G first to answer with the keyboard."
            ));
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
        // The text stays above the action areas in the bottom 22%.
        let last_text_row = laid_out(&prompt, 2).rows.len();
        assert!(last_text_row * 100 <= natural_rows(&variants) * 72);
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
        assert!(text.contains(&format!("Always allow Manage for '{}…'", "w".repeat(59))));
        assert!(!text.contains('\u{202e}'));
        // A shorter name shows the override escaped, as the list does.
        prompt.target = ConsentTarget::Lair {
            lair_id: splinterm_core::LairId::new(),
            lair_name: "w\u{202e}v".to_owned(),
        };
        let text = laid_out(&prompt, 2).rows.join("\n");
        assert!(
            text.contains("Always allow Manage for 'w\\u{202e}v'"),
            "{text}"
        );
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
                text.contains(&format!(
                    "Agent (reported by the agent, not verified): {escaped}"
                )),
                "{text}"
            );
            assert!(text.contains(&format!("Program: /opt/{escaped}")), "{text}");
            assert!(text.contains(&format!(": '{escaped}'")), "{text}");
            if selected == 2 {
                assert!(
                    text.contains(&format!("Any program that runs /opt/{escaped}")),
                    "{text}"
                );
            }
        }
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
}
