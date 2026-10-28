//! Clap command grammar shared by application dispatchers.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use splinterm_core::{Axis, DojoId, LairId, SplintId, SplitSide};
use splinterm_protocol::{AgentState, validate_agent_status_detail};

use super::launcher_check::LauncherStage;

#[derive(Debug, Parser)]
#[command(version, about = "Splinterm terminal client")]
pub(in crate::app) struct Cli {
    /// Select human output or the supported JSON machine contract.
    #[arg(long, global = true, value_enum)]
    pub(in crate::app) output: Option<OutputMode>,
    /// Select the public machine schema major.
    #[arg(long, global = true, value_parser = clap::value_parser!(u16).range(1..))]
    pub(in crate::app) schema_major: Option<u16>,
    /// Bound a machine request deadline in milliseconds.
    #[arg(long, global = true, value_parser = clap::value_parser!(u64).range(1..=300_000))]
    pub(in crate::app) timeout_ms: Option<u64>,
    /// Bind this invocation to one configured remote daemon endpoint.
    #[arg(long, global = true, value_name = "PROFILE")]
    pub(in crate::app) remote: Option<String>,
    /// Start a new session without a controlling terminal (internal launches).
    #[arg(long, hide = true)]
    pub(in crate::app) detach_terminal: bool,
    #[command(subcommand)]
    pub(in crate::app) command: Option<Command>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(in crate::app) enum OutputMode {
    Human,
    Json,
    Ndjson,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(in crate::app) enum SplitAxis {
    Horizontal,
    Vertical,
}

impl From<SplitAxis> for Axis {
    fn from(value: SplitAxis) -> Self {
        match value {
            SplitAxis::Horizontal => Self::Horizontal,
            SplitAxis::Vertical => Self::Vertical,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(in crate::app) enum NewSplintSide {
    First,
    Second,
}

impl From<NewSplintSide> for SplitSide {
    fn from(value: NewSplintSide) -> Self {
        match value {
            NewSplintSide::First => Self::First,
            NewSplintSide::Second => Self::Second,
        }
    }
}

#[derive(Debug, Subcommand)]
pub(in crate::app) enum Command {
    /// Choose a recent running Dojo or create a fresh terminal.
    #[command(alias = "sessions")]
    Dojos,
    /// Reopen the most recently opened logical Dojo that is still running.
    Reopen,
    /// Open a remote in one new Window: pick a profile, then a Dojo (returns at once).
    Remotes,
    /// Render ordered snapshots of one explicitly selected terminal.
    Window {
        /// Select a Lair by stable identity (required with --dojo-id).
        #[arg(long, requires = "dojo_id")]
        lair_id: Option<LairId>,
        /// Select one daemon-owned Dojo (required with --lair-id).
        #[arg(long, requires = "lair_id")]
        dojo_id: Option<DojoId>,
    },
    Ping,
    /// Inspect bounded local client, daemon, service, and crash diagnostics.
    Diagnostics {
        /// Read the authoritative last graphical client exit summary.
        #[arg(long, conflicts_with = "last_crash")]
        last_exit: bool,
        /// Report the newest panic or externally inferred systemd crash.
        #[arg(long, conflicts_with = "last_exit")]
        last_crash: bool,
    },
    /// Read only the keyboard-focused graphical Splint ID and current working directory.
    Focus,
    /// Stop the daemon, back up and clear every Lair, then restart cleanly.
    Reset {
        /// Confirm termination of every daemon-owned shell without prompting.
        #[arg(long)]
        yes: bool,
    },
    /// List active Lairs without flooding the terminal with exited history.
    List {
        /// Include exited-only Lairs and their complete topology.
        #[arg(long)]
        all: bool,
    },
    /// Inspect one Splint's effective authority or revoke a Splint-scoped
    /// once grant. `splinterm access` lists and revokes every grant.
    Authorization {
        #[command(subcommand)]
        command: AuthorizationCommand,
    },
    /// See which agents can reach which workspaces, and revoke their access.
    Access {
        #[command(subcommand)]
        command: AccessCommand,
    },
    /// Add Splinterm's MCP server to an agent host's configuration, or remove
    /// it. Setup grants no access: the agent asks the first time it needs a
    /// workspace.
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// Bridge private protocol bytes over non-terminal stdin/stdout.
    Relay {
        /// Use stdin and stdout as one byte-transparent automation transport.
        #[arg(
            long,
            conflicts_with = "graphical_stdio",
            required_unless_present = "graphical_stdio"
        )]
        stdio: bool,
        /// Carry bounded channels for one native remote graphical client.
        #[arg(long, conflicts_with = "stdio", required_unless_present = "stdio")]
        graphical_stdio: bool,
    },
    /// Inspect strictly parsed remote connection profiles without connecting.
    Remote {
        #[command(subcommand)]
        command: RemoteCommand,
    },
    /// Validate local client configuration without contacting the daemon.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Inspect built-in or effective client keymaps.
    Keymap {
        #[command(subcommand)]
        command: KeymapCommand,
    },
    /// Validate and inspect client-local Dojo presets.
    Preset {
        #[command(subcommand)]
        command: PresetCommand,
    },
    /// Manage explicit, reversible desktop integrations.
    Integration {
        #[command(subcommand)]
        command: IntegrationCommand,
    },
    /// Validate, inspect, or reload the local persistent policy.
    Policy {
        #[command(subcommand)]
        command: PolicyCommand,
    },
    /// Inspect bounded in-memory daemon audit records.
    Audit {
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        after: Option<u64>,
        #[arg(long, default_value_t = 128, value_parser = clap::value_parser!(u16).range(1..=128))]
        max_records: u16,
    },
    /// Stream bounded machine events as NDJSON.
    Subscribe {
        #[command(subcommand)]
        stream: SubscribeCommand,
    },
    /// Inspect all reviewed topology metadata.
    Topology,
    /// Inspect one Splint by stable identity.
    Inspect {
        splint_id: SplintId,
    },
    /// Create a persistent Lair with one Dojo and live Splint.
    New {
        name: String,
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Execute argv directly instead of starting the configured shell.
        #[arg(last = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Private XDG terminal adapter entry point.
    #[command(hide = true)]
    XdgLaunch {
        #[arg(long = "working-directory", alias = "dir")]
        cwd: Option<PathBuf>,
        /// Client-local Wayland identity supplied only by the XDG adapter.
        #[arg(long, value_parser = parse_xdg_app_id)]
        app_id: Option<String>,
        /// Open the Recent Dojos picker instead of creating a terminal.
        #[arg(long, conflicts_with_all = ["new", "command"])]
        picker: bool,
        /// Create a fresh terminal even when `main.startup=dojo-picker`.
        #[arg(long)]
        new: bool,
        /// Executable and arguments passed directly, never through a shell.
        #[arg(last = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Create a fresh graphical Lair, or explicitly attach by Splint ID.
    Launch {
        #[arg(long = "working-directory", alias = "dir")]
        cwd: Option<PathBuf>,
        /// Give the fresh Lair an explicit unique name.
        #[arg(long)]
        name: Option<String>,
        /// Attach an existing Splint by stable identity.
        #[arg(long)]
        splint_id: Option<SplintId>,
        /// Compatibility flag; fresh creation is already the default.
        #[arg(long, conflicts_with = "splint_id")]
        new: bool,
        /// Open the Recent Dojos picker instead of creating a terminal.
        #[arg(long, conflicts_with_all = ["name", "splint_id", "new", "command"])]
        picker: bool,
        /// Executable and arguments passed directly, never through a shell.
        #[arg(last = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Split a target leaf and launch a new sibling.
    Split {
        target_splint_id: SplintId,
        #[arg(long, value_enum)]
        axis: SplitAxis,
        #[arg(long, value_enum)]
        side: NewSplintSide,
        /// Thousandths assigned to the first child (1..=999).
        #[arg(long, default_value_t = 500, value_parser = clap::value_parser!(u16).range(1..=999))]
        ratio: u16,
        /// Fail if the target no longer has this exact live incarnation.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        expected_incarnation: Option<u64>,
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Executable and arguments passed directly, never through a shell.
        #[arg(last = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Close an exited Splint leaf and collapse its parent branch.
    Close {
        splint_id: SplintId,
        /// Confirm destructive topology mutation for machine output.
        #[arg(long)]
        yes: bool,
    },
    /// Change the ratio of the selected Splint's parent branch.
    Ratio {
        target_splint_id: SplintId,
        #[arg(value_parser = clap::value_parser!(u16).range(1..=999))]
        ratio: u16,
    },
    /// Create a persistent Dojo with one live Splint.
    NewDojo {
        lair_id: LairId,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(last = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Close a Dojo whose Splints have all exited.
    CloseDojo {
        dojo_id: DojoId,
        /// Confirm destructive topology mutation for machine output.
        #[arg(long)]
        yes: bool,
    },
    RenameLair {
        lair_id: LairId,
        name: String,
    },
    RenameDojo {
        dojo_id: DojoId,
        name: String,
    },
    /// Set a persisted convenience hint without changing any client's actual focus.
    DojoFocusHint {
        dojo_id: DojoId,
        splint_id: SplintId,
    },
    RenameSplint {
        splint_id: SplintId,
        title: String,
    },
    /// Relaunch an exited Splint under a new process incarnation.
    Relaunch {
        splint_id: SplintId,
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Executable and arguments passed directly, never through a shell.
        #[arg(last = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Restore one exited Splint using its saved launch metadata.
    Restore {
        splint_id: SplintId,
    },
    /// Restore every exited Splint in a saved Dojo.
    RestoreDojo {
        dojo_id: DojoId,
    },
    /// Restore every exited Splint in a saved Lair.
    RestoreLair {
        lair_id: LairId,
    },
    /// Show one live terminal snapshot (development mode only).
    Snapshot {
        splint_id: SplintId,
        /// Fail if the target no longer has this exact live incarnation.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        expected_incarnation: Option<u64>,
    },
    /// Read one bounded page of terminal history.
    Scrollback {
        splint_id: SplintId,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, default_value_t = 16, value_parser = clap::value_parser!(u16).range(1..=16))]
        max_rows: u16,
    },
    /// Search terminal history without echoing the query in machine output.
    Search {
        splint_id: SplintId,
        query: String,
        #[arg(long)]
        case_sensitive: bool,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, default_value_t = 64, value_parser = clap::value_parser!(u16).range(1..=64))]
        max_results: u16,
    },
    /// Send literal UTF-8 text to one live shell (development mode only).
    Send {
        splint_id: SplintId,
        text: String,
        /// Fail if the target no longer has this exact live incarnation.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        expected_incarnation: Option<u64>,
    },
    /// Resize one live terminal (development mode only).
    Resize {
        splint_id: SplintId,
        columns: u16,
        rows: u16,
    },
    /// Kill one live process while retaining its Splint leaf.
    Kill {
        splint_id: SplintId,
        /// Confirm process termination without prompting.
        #[arg(long)]
        yes: bool,
    },
    /// Report, clear, or read the explicitly reported agent status of a Splint.
    AgentStatus {
        #[command(subcommand)]
        command: AgentStatusCommand,
    },
    /// Private daemon-launched trusted consent surface.
    #[command(hide = true)]
    Consent,
    /// Answer agent access requests in this terminal.
    ///
    /// For a computer with no Splinterm Window to show the access prompt,
    /// such as a headless host you reach over SSH. Run it in your own
    /// terminal on the computer splinterd runs on, not inside a Splinterm
    /// pane or a tmux or screen session started from one: splinterd refuses
    /// it there, so agents cannot start it for themselves. Only the installed
    /// splinterm beside the running splinterd may answer.
    ///
    /// Each request shows its agent, program, workspace and access, starts
    /// on Deny, and counts down. Keys typed before it appears are thrown
    /// away, and keys count only a moment after it appears. Up/Down or j/k
    /// choose, Enter answers; Escape, q or Ctrl+C deny. When the time runs
    /// out nothing is sent. Ctrl+C with no request waiting quits. There is
    /// no option that answers without you.
    #[command(verbatim_doc_comment)]
    Approvals,
    /// Inspect or start a live daemon upgrade that keeps terminals running.
    ///
    /// Without --now, show whether the running daemon could hand its terminals
    /// to the installed build, and change nothing. Where live upgrade is not
    /// available, as for Nix installations, restarting the daemon is the
    /// upgrade path.
    ///
    /// Only the installed splinterm beside the running splinterd may ask.
    /// Exit status: 0 report shown or upgraded, 1 error (a restarted daemon
    /// too), 2 usage, 3 cancelled, 4 unavailable, 5 another upgrade in
    /// progress, 6 rolled back, 7 connection closed but the result is
    /// unconfirmed.
    #[command(verbatim_doc_comment)]
    Upgrade {
        /// Start the upgrade now. When open Windows cannot take part, ask
        /// before closing them; their terminals keep running. Asks in the
        /// terminal, or in a dialog when there is none or when run inside a
        /// Splinterm terminal, whose input the upgrade pauses. In the dialog
        /// only G or the green area proceeds; D, Escape and Enter cancel. No
        /// answer cancels.
        #[arg(long)]
        now: bool,
    },
    /// Private trusted dialog child of `upgrade --now`.
    #[command(hide = true, name = "upgrade-dialog")]
    UpgradeDialog,
    /// Private desktop launcher check of the running daemon's state, run by
    /// `splinterm-xdg-terminal-exec` before it opens anything.
    #[command(hide = true, name = "launcher-check")]
    LauncherCheck {
        /// What the launcher just did to splinterd.service.
        #[arg(long, value_enum)]
        after: Option<LauncherStage>,
        /// Never ask: an unattended launch answers any question "Not now".
        #[arg(long)]
        no_ask: bool,
    },
    /// Private trusted dialog child of `launcher-check`.
    #[command(hide = true, name = "restart-dialog")]
    RestartDialog,
}

/// Closed vocabulary of explicitly reported agent display states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(in crate::app) enum AgentStateArg {
    /// The agent is acting and needs nothing.
    Working,
    /// The agent is blocked on the person.
    #[value(name = "needs_input", alias = "needs-input")]
    NeedsInput,
    /// The agent finished its task.
    Done,
    /// The agent stopped on a failure it reported.
    Error,
    /// The agent is present and has no task.
    Idle,
}

impl From<AgentStateArg> for AgentState {
    fn from(value: AgentStateArg) -> Self {
        match value {
            AgentStateArg::Working => Self::Working,
            AgentStateArg::NeedsInput => Self::NeedsInput,
            AgentStateArg::Done => Self::Done,
            AgentStateArg::Error => Self::Error,
            AgentStateArg::Idle => Self::Idle,
        }
    }
}

/// An explicit report target; both halves are required together.
#[derive(Debug, Clone, Copy, clap::Args)]
pub(in crate::app) struct AgentStatusTarget {
    /// Report for this Splint instead of `$SPLINTERM_SPLINT_ID`.
    #[arg(long = "splint", value_name = "ID", requires = "incarnation")]
    pub(in crate::app) splint: Option<SplintId>,
    /// The Splint's live incarnation instead of `$SPLINTERM_SPLINT_INCARNATION`.
    #[arg(
        long,
        value_name = "N",
        requires = "splint",
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    pub(in crate::app) incarnation: Option<u64>,
}

#[derive(Debug, Subcommand)]
pub(in crate::app) enum AgentStatusCommand {
    /// Report a display state for the current Splint (inside a Splint by default).
    Set {
        #[arg(value_enum)]
        state: AgentStateArg,
        /// Bounded display detail: at most 80 characters and 240 bytes, no
        /// control, bidi-formatting, or line-break characters.
        #[arg(long, value_parser = parse_agent_status_detail)]
        detail: Option<String>,
        /// Seconds until an unrefreshed report turns stale (10..=900; active
        /// states default to 120, resting states do not expire).
        #[arg(
            long = "ttl",
            value_name = "SECONDS",
            value_parser = clap::value_parser!(u32).range(10..=900)
        )]
        ttl_seconds: Option<u32>,
        #[command(flatten)]
        target: AgentStatusTarget,
    },
    /// Clear the current Splint's reported status.
    Clear {
        #[command(flatten)]
        target: AgentStatusTarget,
    },
    /// Read one Splint's reported status (default `$SPLINTERM_SPLINT_ID`).
    Get { splint_id: Option<SplintId> },
}

fn parse_agent_status_detail(value: &str) -> Result<String, String> {
    validate_agent_status_detail(value)
        .map(|()| value.to_owned())
        .map_err(|error| error.message)
}

pub(super) fn parse_xdg_app_id(value: &str) -> Result<String, String> {
    const MAX_APP_ID_BYTES: usize = 255;
    if value.is_empty() || value.len() > MAX_APP_ID_BYTES || !value.is_ascii() {
        return Err("app ID must contain 1 to 255 ASCII bytes".to_owned());
    }
    let mut components = value.split('.');
    let valid_component = |component: &str| {
        let mut characters = component.chars();
        characters
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic())
            && characters.all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            })
    };
    let Some(first) = components.next() else {
        return Err("app ID must be a reverse-DNS identity".to_owned());
    };
    if !valid_component(first)
        || components.clone().count() == 0
        || !components.all(valid_component)
    {
        return Err(
            "app ID must be dot-separated ASCII identifiers beginning with letters".to_owned(),
        );
    }
    Ok(value.to_owned())
}

#[derive(Debug, Subcommand)]
pub(in crate::app) enum RemoteCommand {
    /// List configured named remotes.
    List,
    /// Show one resolved profile and its credential-free SSH process plan.
    Inspect { profile: String },
    /// Negotiate SSH, relay, and daemon read-only reachability without mapping a Window.
    Check { profile: String },
}

#[derive(Clone, Copy, Debug, Subcommand)]
pub(in crate::app) enum ConfigCommand {
    /// Parse and resolve config.ini plus its selected keymap.
    Check,
}

#[derive(Debug, Subcommand)]
pub(in crate::app) enum KeymapCommand {
    /// List packaged keymap profiles.
    List,
    /// Show the effective keymap, or one named built-in profile.
    Show { profile: Option<String> },
    /// Validate that the effective keymap has no overlapping chords.
    Conflicts,
}

#[derive(Debug, Subcommand)]
pub(in crate::app) enum PresetCommand {
    /// List configured static presets.
    List,
    /// Show one configured preset definition.
    Show { name: String },
    /// Validate the configured catalog or an explicit path.
    Check { path: Option<PathBuf> },
    /// Print guarded shell functions without installing or sourcing them.
    ShellInit {
        integration: ShellIntegration,
        /// Select the generated shell syntax.
        #[arg(long, value_enum)]
        shell: IntegrationShell,
    },
    /// Install one guarded integration to its new dedicated XDG file.
    ShellInstall {
        integration: ShellIntegration,
        /// Select the generated shell syntax.
        #[arg(long, value_enum)]
        shell: IntegrationShell,
    },
    /// Compile and preview one preset without daemon mutation.
    Run {
        name: String,
        /// Override the captured invocation working directory.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Supply one bounded typed preset parameter as NAME=VALUE.
        #[arg(long = "param", value_name = "NAME=VALUE")]
        params: Vec<String>,
        /// Materialize without opening the first created Dojo in a new Window.
        #[arg(long)]
        no_open: bool,
        /// Validate and preview without connecting to the daemon.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(in crate::app) enum ShellIntegration {
    Omarchy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(in crate::app) enum IntegrationShell {
    Bash,
}

#[derive(Clone, Copy, Debug, Subcommand)]
pub(in crate::app) enum IntegrationCommand {
    /// Manage every user-level integration required for Omarchy.
    Omarchy {
        #[command(subcommand)]
        action: IntegrationAction,
    },
    /// Manage only Splinterm's legacy Omarchy screensaver integration.
    OmarchyScreensaver {
        #[command(subcommand)]
        action: IntegrationAction,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Subcommand)]
pub(in crate::app) enum IntegrationAction {
    /// Create the dedicated user-level launcher link when no collision exists.
    Enable,
    /// Remove only the exact launcher link managed by Splinterm.
    Disable,
    /// Report whether the managed launcher link is active.
    Status,
}

#[derive(Debug, Subcommand)]
pub(in crate::app) enum PolicyCommand {
    /// Validate a policy using the daemon's secure loader without publishing it.
    Validate { path: PathBuf },
    /// Print the normalized, validated policy document.
    Inspect { path: PathBuf },
    /// Ask the canonical systemd user service to reload its configured policy.
    Reload,
}

#[derive(Debug, Subcommand)]
pub(in crate::app) enum AuthorizationCommand {
    Status {
        splint_id: SplintId,
    },
    Revoke {
        #[arg(value_parser = clap::value_parser!(u64).range(1..))]
        grant_id: u64,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(in crate::app) enum AccessCommand {
    /// List every agent grant of every duration, pending requests, and the
    /// policy rules that match a connected agent.
    List {
        /// Continue after this entry ID (machine output pages).
        #[arg(long, value_name = "ID")]
        after: Option<String>,
        /// Entries per page, at most 64 (machine output pages).
        #[arg(long, value_parser = clap::value_parser!(u16).range(1..=64))]
        max_entries: Option<u16>,
    },
    /// Revoke one grant by its ID, or every agent grant with --all.
    Revoke {
        /// A grant ID or a remembered entry ID (ra_…) from `access list`.
        #[arg(
            value_name = "ID",
            required_unless_present = "all",
            conflicts_with = "all"
        )]
        id: Option<String>,
        /// Revoke every consent grant of every duration and end every pending
        /// request. Policy rules are never touched.
        #[arg(long)]
        all: bool,
        /// With --all: only report how many grants would be revoked.
        #[arg(long)]
        dry_run: bool,
        /// Confirm without asking.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(in crate::app) enum McpCommand {
    /// Add the installed splinterm-mcp adapter to the host's MCP servers as
    /// `splinterm`. Grants no access and writes no policy.
    Setup {
        #[arg(value_enum)]
        host: McpHostArg,
        /// Print the exact change without writing it.
        #[arg(long)]
        dry_run: bool,
        /// Overwrite a different existing `splinterm` entry.
        #[arg(long)]
        replace: bool,
        /// Edit this file instead of the host's own user configuration.
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
        /// Start this adapter instead of the one installed next to splinterm.
        #[arg(long, value_name = "PATH")]
        adapter: Option<PathBuf>,
    },
    /// Remove the `splinterm` entry from the host's MCP servers. Revokes no
    /// grant.
    Remove {
        #[arg(value_enum)]
        host: McpHostArg,
        /// Print the exact change without writing it.
        #[arg(long)]
        dry_run: bool,
        /// Remove a `splinterm` entry even when it starts something other than
        /// splinterm-mcp.
        #[arg(long)]
        force: bool,
        /// Edit this file instead of the host's own user configuration.
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
    },
}

/// An MCP host `splinterm mcp` can configure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(in crate::app) enum McpHostArg {
    /// Claude Code, user scope (`~/.claude.json`).
    ClaudeCode,
    /// Codex CLI (`~/.codex/config.toml`).
    Codex,
    /// Visual Studio Code, default profile (`~/.config/Code/User/mcp.json`); not
    /// yet verified against a real install.
    Vscode,
}

#[derive(Debug, Subcommand)]
pub(in crate::app) enum SubscribeCommand {
    Terminal {
        splint_id: SplintId,
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        expected_incarnation: Option<u64>,
    },
    Topology,
    Control {
        splint_id: SplintId,
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        expected_incarnation: Option<u64>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_modes_are_human_only_and_mutually_exclusive() {
        let parsed = Cli::try_parse_from(["splinterm", "diagnostics", "--last-exit"]).unwrap();
        assert!(matches!(
            parsed.command,
            Some(Command::Diagnostics {
                last_exit: true,
                last_crash: false,
            })
        ));
        assert!(
            Cli::try_parse_from(["splinterm", "diagnostics", "--last-exit", "--last-crash"])
                .is_err()
        );
    }

    #[test]
    fn upgrade_defaults_to_the_report_and_executes_only_with_now() {
        let parsed = Cli::try_parse_from(["splinterm", "upgrade"]).unwrap();
        assert!(matches!(
            parsed.command,
            Some(Command::Upgrade { now: false })
        ));
        let parsed = Cli::try_parse_from(["splinterm", "upgrade", "--now"]).unwrap();
        assert!(matches!(
            parsed.command,
            Some(Command::Upgrade { now: true })
        ));
        for rejected in [
            ["splinterm", "upgrade", "--yes"],
            ["splinterm", "upgrade", "--execute"],
            ["splinterm", "upgrade", "now"],
        ] {
            assert!(Cli::try_parse_from(rejected).is_err(), "{rejected:?}");
        }
        let parsed = Cli::try_parse_from(["splinterm", "upgrade-dialog"]).unwrap();
        assert!(matches!(parsed.command, Some(Command::UpgradeDialog)));
    }

    #[test]
    fn launcher_check_takes_only_its_stage_and_stays_hidden() {
        use clap::CommandFactory as _;

        for (arguments, after, no_ask) in [
            (&["splinterm", "launcher-check"][..], None, false),
            (
                &["splinterm", "launcher-check", "--after", "start"][..],
                Some(LauncherStage::Start),
                false,
            ),
            (
                &[
                    "splinterm",
                    "launcher-check",
                    "--no-ask",
                    "--after",
                    "restart",
                ][..],
                Some(LauncherStage::Restart),
                true,
            ),
            (&["splinterm", "launcher-check", "--no-ask"][..], None, true),
        ] {
            let parsed = Cli::try_parse_from(arguments).unwrap();
            assert!(
                matches!(
                    parsed.command,
                    Some(Command::LauncherCheck { after: parsed_after, no_ask: parsed_no_ask })
                        if parsed_after == after && parsed_no_ask == no_ask
                ),
                "{arguments:?}"
            );
        }
        for rejected in [
            &["splinterm", "launcher-check", "--after", "stop"][..],
            &["splinterm", "launcher-check", "--yes"][..],
            &["splinterm", "launcher-check", "--", "/usr/bin/true"][..],
        ] {
            assert!(Cli::try_parse_from(rejected).is_err(), "{rejected:?}");
        }
        let parsed = Cli::try_parse_from(["splinterm", "restart-dialog"]).unwrap();
        assert!(matches!(parsed.command, Some(Command::RestartDialog)));
        let help = Cli::command().render_long_help().to_string();
        assert!(!help.contains("launcher-check"), "{help}");
        assert!(!help.contains("restart-dialog"), "{help}");
    }

    #[test]
    fn approvals_takes_no_options_and_its_help_says_where_to_run_it() {
        use clap::CommandFactory as _;

        let parsed = Cli::try_parse_from(["splinterm", "approvals"]).unwrap();
        assert!(matches!(parsed.command, Some(Command::Approvals)));
        for rejected in [
            &["splinterm", "approvals", "--yes"][..],
            &["splinterm", "approvals", "--allow"][..],
            &["splinterm", "approvals", "deny"][..],
        ] {
            assert!(Cli::try_parse_from(rejected).is_err(), "{rejected:?}");
        }
        let mut command = Cli::command();
        assert!(command.render_long_help().to_string().contains("approvals"));
        let approvals = command.find_subcommand_mut("approvals").unwrap();
        let help = approvals.render_long_help().to_string();
        for expected in [
            "not inside a Splinterm",
            "pane or a tmux or screen session started from one",
            "agents cannot start it for themselves",
            "starts\non Deny",
            "no option that answers without you",
        ] {
            assert!(help.contains(expected), "missing {expected:?} in {help}");
        }
    }

    #[test]
    fn upgrade_help_documents_now_trust_and_exit_codes_and_hides_the_dialog() {
        use clap::CommandFactory as _;

        let mut command = Cli::command();
        let help = command.render_long_help().to_string();
        assert!(help.contains("upgrade"), "{help}");
        assert!(!help.contains("upgrade-dialog"), "{help}");
        let upgrade = command.find_subcommand_mut("upgrade").unwrap();
        let help = upgrade.render_long_help().to_string();
        for expected in [
            "--now",
            "as for Nix installations",
            "restarting the daemon",
            "installed splinterm beside the running splinterd",
            "3 cancelled",
            "4 unavailable",
            "1 error (a restarted daemon",
            "5 another upgrade in",
            "6 rolled back",
            "only G or the green area proceeds",
            "7 connection closed but the result is",
        ] {
            assert!(help.contains(expected), "missing {expected:?} in {help}");
        }
        assert!(!help.contains("not enabled"), "{help}");
    }
}
