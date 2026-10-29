//! One-authentication OpenSSH session for remote graphical daemon channels.

use std::{
    collections::VecDeque,
    ffi::{OsStr, OsString},
    io::IsTerminal as _,
    os::{
        fd::OwnedFd,
        unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _},
    },
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::{Arc, Mutex},
    task::{Context as TaskContext, Poll},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use splinterm_automation_client::Connection;
use splinterm_graphical_relay::{ClientMultiplexer, LogicalChannel};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf},
    process::{Child, Command},
    sync::watch,
};
use tokio_util::sync::CancellationToken;

use crate::{
    remote::RemoteProfile,
    remote_askpass::{self as askpass, AskpassBroker, NativePrompt},
};

const MAX_SSH_DIAGNOSTIC_BYTES: usize = 16 * 1024;
const CHILD_GRACE_PERIOD: Duration = Duration::from_millis(250);
const AUTHENTICATION_GRACE_PERIOD: Duration = Duration::from_secs(120);
/// The most time negotiation may spend paused on native prompts in total, so
/// sequential questions each get their full prompt timeout while a helper that
/// keeps asking still ends.
const MAX_NATIVE_PROMPT_PAUSE: Duration = Duration::from_secs(10 * 60);
/// How long `ssh -G` may take to print the effective config for a remote.
const SSH_CONFIG_QUERY_TIMEOUT: Duration = Duration::from_secs(2);
/// ssh-agent sockets under `$XDG_RUNTIME_DIR`, in the order they are tried
/// when the Window has no `SSH_AUTH_SOCK`: the systemd user agent, GNOME
/// gcr, gnome-keyring, and gpg-agent.
const AGENT_SOCKETS: [&str; 4] = [
    "ssh-agent.socket",
    "gcr/ssh",
    "keyring/ssh",
    "gnupg/S.gpg-agent.ssh",
];

/// Stable high-level classes for remote startup and transport failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteFailureKind {
    UnknownHostKey,
    ChangedHostKey,
    RoutingOrTimeout,
    AuthenticationFailed,
    InteractiveAuthenticationUnavailable,
    RemoteCommandUnavailable,
    RemoteDaemonUnavailable,
    RelayIdentityRejected,
    MultiplexerIncompatible,
    DaemonProtocolIncompatible,
    TransportFailed,
}

impl std::fmt::Display for RemoteFailureKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Self::UnknownHostKey => "unknown host key",
            Self::ChangedHostKey => "changed host key",
            Self::RoutingOrTimeout => "routing or connection timeout",
            Self::AuthenticationFailed => "SSH authentication failed",
            Self::InteractiveAuthenticationUnavailable => {
                "interactive SSH authentication is unavailable"
            }
            Self::RemoteCommandUnavailable => "remote Splinterm command is unavailable",
            Self::RemoteDaemonUnavailable => "remote splinterd is unavailable",
            Self::RelayIdentityRejected => "remote relay or daemon identity validation failed",
            Self::MultiplexerIncompatible => "graphical relay protocol is incompatible",
            Self::DaemonProtocolIncompatible => "private daemon protocol is incompatible",
            Self::TransportFailed => "remote transport failed",
        };
        formatter.write_str(label)
    }
}

/// A categorized, bounded remote-session failure.
#[derive(Debug)]
pub struct RemoteFailure {
    kind: RemoteFailureKind,
    diagnostic: String,
}

impl RemoteFailure {
    /// Builds a categorized failure; the diagnostic is sanitized and bounded.
    #[must_use]
    pub fn new(kind: RemoteFailureKind, diagnostic: &str) -> Self {
        Self {
            kind,
            diagnostic: bounded_failure_message(diagnostic),
        }
    }

    /// Returns the stable failure category.
    #[must_use]
    pub const fn kind(&self) -> RemoteFailureKind {
        self.kind
    }

    /// Returns the bounded, sanitized OpenSSH and relay diagnostic text.
    #[must_use]
    pub fn diagnostic(&self) -> &str {
        &self.diagnostic
    }
}

impl std::fmt::Display for RemoteFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.diagnostic.is_empty() {
            write!(formatter, "{}", self.kind)
        } else {
            write!(formatter, "{}: {}", self.kind, self.diagnostic)
        }
    }
}

impl std::error::Error for RemoteFailure {}

/// A local OpenSSH setup problem found before OpenSSH runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalSshProblem {
    /// `SSH_ASKPASS` is set but does not name an absolute executable file.
    InvalidAskpass,
    /// The OpenSSH client executable could not be started.
    OpenSshUnavailable,
}

/// A local setup failure; no connection was attempted.
#[derive(Debug)]
pub struct LocalSshFailure {
    problem: LocalSshProblem,
    message: String,
}

impl LocalSshFailure {
    /// Builds a local setup failure; the message is sanitized and bounded.
    #[must_use]
    pub fn new(problem: LocalSshProblem, message: &str) -> Self {
        Self {
            problem,
            message: bounded_failure_message(message),
        }
    }

    /// Returns which local setup step failed.
    #[must_use]
    pub const fn problem(&self) -> LocalSshProblem {
        self.problem
    }

    /// Returns the bounded, sanitized message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for LocalSshFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for LocalSshFailure {}

#[derive(Debug, Default)]
struct DiagnosticRing {
    bytes: VecDeque<u8>,
    truncated: bool,
}

impl DiagnosticRing {
    fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            let byte = if byte == b'\n' || byte == b'\t' || !byte.is_ascii_control() {
                byte
            } else {
                b' '
            };
            if self.bytes.len() == MAX_SSH_DIAGNOSTIC_BYTES {
                self.bytes.pop_front();
                self.truncated = true;
            }
            self.bytes.push_back(byte);
        }
    }

    fn render(&self) -> String {
        let bytes = self.bytes.iter().copied().collect::<Vec<_>>();
        let value = String::from_utf8_lossy(&bytes)
            .chars()
            .map(sanitize_diagnostic_character)
            .collect::<String>();
        let value = value.trim();
        if self.truncated && !value.is_empty() {
            format!("…{value}")
        } else {
            value.to_owned()
        }
    }
}

#[derive(Debug)]
struct RemoteLifetime {
    cancellation: CancellationToken,
}

impl Drop for RemoteLifetime {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

/// One authenticated OpenSSH child and its negotiated graphical multiplexer.
#[derive(Clone, Debug)]
pub struct RemoteSession {
    multiplexer: ClientMultiplexer,
    diagnostics: Arc<Mutex<DiagnosticRing>>,
    lifetime: Arc<RemoteLifetime>,
    terminal: bool,
    askpass_available: bool,
    operation_timeout: Duration,
}

impl RemoteSession {
    /// Starts OpenSSH from a validated profile and negotiates the graphical relay.
    ///
    /// # Errors
    ///
    /// Returns a categorized failure when process construction, SSH, relay
    /// negotiation, or lifecycle setup fails.
    pub async fn connect(profile: &RemoteProfile) -> Result<Self> {
        let program = OsStr::new("ssh");
        let negotiation_timeout = Duration::from_secs(u64::from(profile.connect_timeout_seconds()))
            .saturating_add(AUTHENTICATION_GRACE_PERIOD);
        let environment = AuthEnvironment::current()
            .with_agent(profile, program)
            .await;
        Self::connect_with_options(profile, program, negotiation_timeout, &environment, None).await
    }

    #[doc(hidden)]
    pub async fn connect_with_program(profile: &RemoteProfile, program: &OsStr) -> Result<Self> {
        let negotiation_timeout = Duration::from_secs(u64::from(profile.connect_timeout_seconds()))
            .saturating_add(AUTHENTICATION_GRACE_PERIOD);
        Self::connect_with_program_and_timeout(profile, program, negotiation_timeout).await
    }

    /// Like [`Self::connect`], but when OpenSSH needs a password, passphrase,
    /// or confirmation and neither a terminal nor the user's `SSH_ASKPASS` is
    /// available, it asks through `prompt` (the status Window).
    ///
    /// # Errors
    ///
    /// Returns a categorized failure as [`Self::connect`] does.
    pub async fn connect_with_prompt(
        profile: &RemoteProfile,
        prompt: &NativePrompt,
    ) -> Result<Self> {
        let program = OsStr::new("ssh");
        let negotiation_timeout = Duration::from_secs(u64::from(profile.connect_timeout_seconds()))
            .saturating_add(AUTHENTICATION_GRACE_PERIOD);
        let environment = AuthEnvironment::current()
            .with_agent(profile, program)
            .await;
        Self::connect_with_options(
            profile,
            program,
            negotiation_timeout,
            &environment,
            Some(prompt),
        )
        .await
    }

    #[doc(hidden)]
    pub async fn connect_with_program_and_timeout(
        profile: &RemoteProfile,
        program: &OsStr,
        negotiation_timeout: Duration,
    ) -> Result<Self> {
        Self::connect_with_options(
            profile,
            program,
            negotiation_timeout,
            &AuthEnvironment::current(),
            None,
        )
        .await
    }

    #[doc(hidden)]
    pub async fn connect_with_options(
        profile: &RemoteProfile,
        program: &OsStr,
        negotiation_timeout: Duration,
        environment: &AuthEnvironment,
        native_prompt: Option<&NativePrompt>,
    ) -> Result<Self> {
        if negotiation_timeout.is_zero() {
            bail!("remote relay negotiation timeout must be nonzero");
        }
        let terminal = environment.terminal;
        let (provider, broker) = prepare_prompt(environment, native_prompt)?;
        let askpass_available = matches!(
            provider,
            PromptProvider::UserAskpass | PromptProvider::NativePrompt
        );
        let mut child = spawn_openssh(profile, program, provider, environment, broker.as_ref())?;
        let stdin = child
            .child
            .stdin
            .take()
            .context("OpenSSH child stdin pipe is unavailable")?;
        let stdout = child
            .child
            .stdout
            .take()
            .context("OpenSSH child stdout pipe is unavailable")?;
        let stderr = child
            .child
            .stderr
            .take()
            .context("OpenSSH child stderr pipe is unavailable")?;
        // Serves prompts only from this OpenSSH child's descendants, and only
        // until negotiation ends. Dropping it unreleased (a failure, or the
        // status Window dropping this attempt) SIGKILLs OpenSSH before closing
        // any open prompt, so OpenSSH never reads a refusal as an empty password.
        let askpass_service =
            broker.and_then(|broker| child.child.id().map(|pid| broker.serve(pid)));
        let diagnostics = Arc::new(Mutex::new(DiagnosticRing::default()));
        let stderr_diagnostics = diagnostics.clone();
        let stderr_task = tokio::spawn(async move {
            drain_stderr(stderr, stderr_diagnostics).await;
        });

        // Time a native prompt waits on the user does not count against the
        // negotiation deadline: each sequential question gets its own full
        // prompt timeout from the status Window.
        let prompt_open = askpass_service
            .as_ref()
            .map(askpass::AskpassService::prompt_open);
        let multiplexer = match timeout_outside_prompts(
            negotiation_timeout,
            MAX_NATIVE_PROMPT_PAUSE,
            prompt_open,
            ClientMultiplexer::negotiate(stdout, stdin),
        )
        .await
        {
            Some(Ok(multiplexer)) => multiplexer,
            Some(Err(error)) => {
                terminate_child(&mut child.child).await;
                // Ends a helper still waiting on the socket, so it releases
                // the inherited stderr pipe. OpenSSH is already dead.
                drop(askpass_service);
                let _ = stderr_task.await;
                let diagnostic = rendered_diagnostics(&diagnostics);
                return Err(anyhow::Error::new(RemoteFailure {
                    kind: classify_failure(
                        &format!("{error:#}\n{diagnostic}"),
                        terminal,
                        askpass_available,
                    ),
                    diagnostic: bounded_failure_message(&format!("{error:#}\n{diagnostic}")),
                }));
            }
            None => {
                terminate_child(&mut child.child).await;
                drop(askpass_service);
                let _ = stderr_task.await;
                return Err(anyhow::Error::new(RemoteFailure {
                    kind: RemoteFailureKind::TransportFailed,
                    diagnostic: "SSH authentication or graphical relay negotiation timed out"
                        .to_owned(),
                }));
            }
        };

        // Authenticated: stop serving prompts without killing OpenSSH.
        if let Some(service) = askpass_service {
            service.release();
        }
        let cancellation = CancellationToken::new();
        let lifetime = Arc::new(RemoteLifetime {
            cancellation: cancellation.clone(),
        });
        tokio::spawn(supervise_child(child, cancellation, stderr_task));
        Ok(Self {
            multiplexer,
            diagnostics,
            lifetime,
            terminal,
            askpass_available,
            operation_timeout: Duration::from_secs(u64::from(profile.connect_timeout_seconds())),
        })
    }

    /// Opens and negotiates one independent human-interactive daemon connection.
    ///
    /// # Errors
    ///
    /// Returns an error when channel admission or the private daemon handshake
    /// fails. The connection never receives local compositor or image authority.
    pub async fn connect_interactive(&self) -> Result<Connection> {
        let deadline = tokio::time::Instant::now() + self.operation_timeout;
        let channel = if let Ok(result) =
            tokio::time::timeout_at(deadline, self.multiplexer.open_channel()).await
        {
            result.map_err(|error| {
                let diagnostic = rendered_diagnostics(&self.diagnostics);
                anyhow::Error::new(RemoteFailure {
                    kind: classify_failure(
                        &format!("{error:#}\n{diagnostic}"),
                        self.terminal,
                        self.askpass_available,
                    ),
                    diagnostic: bounded_failure_message(&format!("{error:#}\n{diagnostic}")),
                })
            })?
        } else {
            let stage = self
                .multiplexer
                .terminal_failure()
                .unwrap_or_else(|| "graphical relay channel admission was cancelled".to_owned());
            return Err(anyhow::Error::new(RemoteFailure {
                kind: RemoteFailureKind::TransportFailed,
                diagnostic: format!("remote logical channel admission timed out: {stage}"),
            }));
        };
        let channel_id = channel.channel_id();
        let channel = SessionChannel {
            inner: channel,
            _lifetime: self.lifetime.clone(),
        };
        let (reader, writer) = tokio::io::split(channel);
        match tokio::time::timeout_at(
            deadline,
            Connection::connect_remote_interactive_transport(reader, writer),
        )
        .await
        {
            Ok(result) => result.map_err(|error| {
                let cause = format!("{error:#}");
                let text = format!(
                    "remote private daemon handshake failed on logical channel {channel_id}: {cause}"
                );
                let lower = cause.to_ascii_lowercase();
                let kind = if lower.contains("incompatibleversion")
                    || lower.contains("incompatible version")
                    || lower.contains("invalid handshake")
                {
                    RemoteFailureKind::DaemonProtocolIncompatible
                } else if lower.contains("unauthorized")
                    || lower.contains("installed graphical relay")
                {
                    RemoteFailureKind::RelayIdentityRejected
                } else {
                    RemoteFailureKind::TransportFailed
                };
                anyhow::Error::new(RemoteFailure {
                    kind,
                    diagnostic: bounded_failure_message(&text),
                })
            }),
            Err(_) => Err(anyhow::Error::new(RemoteFailure {
                kind: RemoteFailureKind::TransportFailed,
                diagnostic: format!(
                    "remote private daemon Hello timed out on logical channel {channel_id}"
                ),
            })),
        }
    }

    /// The profile's connect timeout, which bounds each channel admission.
    #[must_use]
    pub const fn operation_timeout(&self) -> Duration {
        self.operation_timeout
    }

    /// Returns bounded sanitized SSH diagnostics retained for this session.
    #[must_use]
    pub fn diagnostics(&self) -> String {
        rendered_diagnostics(&self.diagnostics)
    }

    /// Returns the terminal multiplexer failure, if one has occurred.
    #[must_use]
    pub fn terminal_failure(&self) -> Option<String> {
        self.multiplexer.terminal_failure()
    }
}

#[derive(Debug)]
struct SessionChannel {
    inner: LogicalChannel,
    _lifetime: Arc<RemoteLifetime>,
}

impl AsyncRead for SessionChannel {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(context, buffer)
    }
}

impl AsyncWrite for SessionChannel {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(context, buffer)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}

/// Selects the prompt provider and, for the native prompt, binds this
/// attempt's private broker.
fn prepare_prompt(
    environment: &AuthEnvironment,
    native_prompt: Option<&NativePrompt>,
) -> Result<(PromptProvider, Option<AskpassBroker>)> {
    let provider =
        select_prompt_provider(environment, native_prompt.is_some()).map_err(|error| {
            LocalSshFailure::new(LocalSshProblem::InvalidAskpass, &format!("{error:#}"))
        })?;
    let broker = match (provider, native_prompt) {
        // Without a private socket nothing can prompt; agent keys may still
        // authenticate, and a needed prompt is then reported as unavailable.
        (PromptProvider::NativePrompt, Some(prompt)) => AskpassBroker::bind(prompt).ok(),
        _ => None,
    };
    if provider == PromptProvider::NativePrompt && broker.is_none() {
        return Ok((PromptProvider::Unavailable, None));
    }
    Ok((provider, broker))
}

/// Builds the OpenSSH command with piped protocol stdio and only the askpass
/// environment the selected provider needs.
fn ssh_command(
    profile: &RemoteProfile,
    program: &OsStr,
    provider: PromptProvider,
    environment: &AuthEnvironment,
    broker: Option<&AskpassBroker>,
) -> Command {
    let plan = profile.ssh_plan();
    let mut command = Command::new(program);
    if environment.agent.add_keys {
        // The fixed plan never sets AddKeysToAgent, so this is the only value.
        command.arg("-o").arg("AddKeysToAgent=yes");
    }
    if let Some(socket) = &environment.agent.socket {
        command.env("SSH_AUTH_SOCK", socket);
    }
    command
        .args(plan.arguments())
        .env_remove(askpass::SOCKET_ENV)
        .env_remove(askpass::TOKEN_ENV)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if provider == PromptProvider::UserAskpass
        && let Some(user_askpass) = &environment.user_askpass
    {
        // Exactly the program that was validated.
        command
            .env("SSH_ASKPASS", user_askpass)
            .env("SSH_ASKPASS_REQUIRE", "force");
    }
    if let Some(broker) = broker {
        command.envs(broker.environment());
    }
    command
}

/// Who answers OpenSSH when it needs a password, passphrase, or confirmation.
/// OpenSSH always tries ssh-agent keys first and asks only when they fail.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromptProvider {
    /// Stdin is a terminal; OpenSSH may prompt on it and nothing is forced.
    Terminal,
    /// The user's own `SSH_ASKPASS`, forced with `SSH_ASKPASS_REQUIRE=force`.
    UserAskpass,
    /// Splinterm's prompt in the status Window, used only when neither of the
    /// above is available.
    NativePrompt,
    /// Nothing can ask; only agent or other non-interactive methods can work.
    Unavailable,
}

/// The process facts that select a [`PromptProvider`].
#[doc(hidden)]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AuthEnvironment {
    pub terminal: bool,
    pub user_askpass: Option<OsString>,
    pub agent: AgentSetup,
}

impl AuthEnvironment {
    /// Reads whether stdin is a terminal and the inherited `SSH_ASKPASS`. The
    /// ssh-agent setup starts empty; see [`Self::with_agent`].
    #[must_use]
    pub fn current() -> Self {
        Self {
            terminal: std::io::stdin().is_terminal(),
            user_askpass: std::env::var_os("SSH_ASKPASS"),
            agent: AgentSetup::default(),
        }
    }

    /// Finds the user's ssh-agent and asks OpenSSH whether this remote's
    /// config already decides if a key goes into it (Plan 0070).
    #[must_use]
    pub async fn with_agent(mut self, profile: &RemoteProfile, program: &OsStr) -> Self {
        let found = find_agent_socket(
            std::env::var_os("SSH_AUTH_SOCK").as_deref(),
            std::env::var_os("XDG_RUNTIME_DIR").as_deref(),
            rustix::process::geteuid().as_raw(),
        );
        self.agent = agent_setup(found, ssh_config_query(profile, program)).await;
        self
    }
}

/// Where OpenSSH finds the user's ssh-agent, and whether it adds a key there
/// after the user types its passphrase once.
#[doc(hidden)]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AgentSetup {
    /// An agent socket found under `$XDG_RUNTIME_DIR` because the Window has
    /// no `SSH_AUTH_SOCK`; OpenSSH gets it as `SSH_AUTH_SOCK`.
    pub socket: Option<PathBuf>,
    /// Pass `-o AddKeysToAgent=yes`: an agent is reachable and this remote's
    /// ssh config leaves the setting at its default.
    pub add_keys: bool,
}

/// The ssh-agent the Window can reach.
#[doc(hidden)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentSocket {
    /// The Window has `SSH_AUTH_SOCK`, kept as is; `reachable` when it names
    /// a socket, `gpg_agent` when that socket is gpg-agent's.
    Inherited { reachable: bool, gpg_agent: bool },
    /// No `SSH_AUTH_SOCK`; this socket under `$XDG_RUNTIME_DIR` is the agent.
    Found(PathBuf),
    /// No agent found.
    Missing,
}

/// Finds the ssh-agent: a non-empty `SSH_AUTH_SOCK` is used as is; otherwise
/// the first of [`AGENT_SOCKETS`] under an absolute `$XDG_RUNTIME_DIR` that
/// is a socket owned by `uid`.
#[doc(hidden)]
#[must_use]
pub fn find_agent_socket(
    inherited: Option<&OsStr>,
    runtime_dir: Option<&OsStr>,
    uid: u32,
) -> AgentSocket {
    if let Some(inherited) = inherited.filter(|value| !value.is_empty()) {
        let reachable = Path::new(inherited)
            .metadata()
            .is_ok_and(|metadata| metadata.file_type().is_socket());
        return AgentSocket::Inherited {
            reachable,
            gpg_agent: is_gpg_agent(Path::new(inherited)),
        };
    }
    let Some(runtime_dir) = runtime_dir.map(Path::new).filter(|path| path.is_absolute()) else {
        return AgentSocket::Missing;
    };
    AGENT_SOCKETS
        .iter()
        .map(|name| runtime_dir.join(name))
        .find(|path| {
            path.symlink_metadata()
                .is_ok_and(|metadata| metadata.file_type().is_socket() && metadata.uid() == uid)
        })
        .map_or(AgentSocket::Missing, AgentSocket::Found)
}

/// Whether the socket is gpg-agent's ssh socket.
fn is_gpg_agent(socket: &Path) -> bool {
    socket.file_name() == Some(OsStr::new("S.gpg-agent.ssh"))
}

/// `ssh -G` with this remote's options and host: prints the effective config
/// without connecting.
fn ssh_config_query(profile: &RemoteProfile, program: &OsStr) -> Command {
    let plan = profile.ssh_plan();
    let mut command = Command::new(program);
    command.arg("-G");
    // Everything but the remote command, which ends the plan.
    if let Some((_, options)) = plan.arguments().split_last() {
        command.args(options);
    }
    command
}

/// Decides the agent setup. The config query runs only when an agent that
/// holds keys until logout is reachable; a query that fails, times out, or prints anything but the
/// default leaves `AddKeysToAgent` to the user's config.
async fn agent_setup(found: AgentSocket, mut query: Command) -> AgentSetup {
    // gpg-agent saves an added key on disk, past logout: never add there.
    let (socket, may_add) = match found {
        AgentSocket::Inherited {
            reachable,
            gpg_agent,
        } => (None, reachable && !gpg_agent),
        AgentSocket::Found(path) => {
            let may_add = !is_gpg_agent(&path);
            (Some(path), may_add)
        }
        AgentSocket::Missing => (None, false),
    };
    let add_keys = may_add && {
        query
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        matches!(
            tokio::time::timeout(SSH_CONFIG_QUERY_TIMEOUT, query.output()).await,
            Ok(Ok(output)) if output.status.success() && leaves_keys_out_of_agent(&output.stdout)
        )
    };
    AgentSetup { socket, add_keys }
}

/// Whether `ssh -G` output leaves `AddKeysToAgent` at its default. An
/// explicit `AddKeysToAgent no` prints the same and cannot be told apart.
#[doc(hidden)]
#[must_use]
pub fn leaves_keys_out_of_agent(config: &[u8]) -> bool {
    String::from_utf8_lossy(config)
        .lines()
        .any(|line| line.trim() == "addkeystoagent false")
}

/// Chooses the prompt provider: a terminal, then the user's `SSH_ASKPASS`,
/// then the native prompt when the caller offers one.
///
/// # Errors
///
/// Returns an error when, without a terminal, `SSH_ASKPASS` is set but does
/// not name an absolute executable file. That explicit setting is reported,
/// never silently replaced by the native prompt.
pub fn select_prompt_provider(
    environment: &AuthEnvironment,
    native_prompt_offered: bool,
) -> Result<PromptProvider> {
    if environment.terminal {
        return Ok(PromptProvider::Terminal);
    }
    if validate_askpass_value(false, environment.user_askpass.as_deref())? {
        return Ok(PromptProvider::UserAskpass);
    }
    Ok(if native_prompt_offered {
        PromptProvider::NativePrompt
    } else {
        PromptProvider::Unavailable
    })
}

fn validate_askpass_value(terminal: bool, value: Option<&OsStr>) -> Result<bool> {
    if terminal {
        return Ok(false);
    }
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(false);
    };
    let path = Path::new(value);
    if !path.is_absolute() {
        bail!("SSH_ASKPASS must name an absolute local executable");
    }
    let metadata = path
        .metadata()
        .with_context(|| format!("cannot inspect SSH_ASKPASS executable {}", path.display()))?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        bail!(
            "SSH_ASKPASS is not an executable local file: {}",
            path.display()
        );
    }
    Ok(true)
}

async fn drain_stderr<R>(mut stderr: R, diagnostics: Arc<Mutex<DiagnosticRing>>)
where
    R: AsyncRead + Unpin,
{
    let mut buffer = [0_u8; 1024];
    loop {
        match stderr.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(count) => {
                if let Ok(mut diagnostics) = diagnostics.lock() {
                    diagnostics.push(&buffer[..count]);
                }
            }
        }
    }
}

/// Starts OpenSSH. OpenSSH may turn echo off on our controlling terminal to
/// read a passphrase, and killed mid-read it never turns it back on, so the
/// terminal's modes are saved before it starts and restored once it ends.
fn spawn_openssh(
    profile: &RemoteProfile,
    program: &OsStr,
    provider: PromptProvider,
    environment: &AuthEnvironment,
    broker: Option<&AskpassBroker>,
) -> Result<OpenSsh> {
    let terminal_modes = (provider == PromptProvider::Terminal)
        .then(ControllingTerminal::open)
        .flatten()
        .and_then(TerminalModeGuard::capture);
    let child = ssh_command(profile, program, provider, environment, broker)
        .spawn()
        .map_err(|error| {
            LocalSshFailure::new(
                LocalSshProblem::OpenSshUnavailable,
                &format!(
                    "cannot launch OpenSSH executable {}: {error}",
                    program.to_string_lossy()
                ),
            )
        })?;
    Ok(OpenSsh {
        child,
        _terminal_modes: terminal_modes,
    })
}

/// The OpenSSH child and the terminal modes to restore once it has ended.
struct OpenSsh {
    /// Dropped first: `kill_on_drop` SIGKILLs a still-running OpenSSH.
    child: Child,
    /// Dropped after the child (fields drop in declaration order), so a
    /// cancelled attempt restores echo only once OpenSSH is killed.
    _terminal_modes: Option<TerminalModeGuard>,
}

/// A terminal whose modes OpenSSH may change while it prompts.
trait TerminalModes {
    type Saved;
    /// Whether this process's group is the terminal's foreground group, so it
    /// may read or change the terminal's modes without being stopped.
    fn in_foreground(&self) -> bool;
    fn save(&self) -> Option<Self::Saved>;
    fn restore(&self, saved: &Self::Saved);
}

/// This process's controlling terminal, opened as `/dev/tty`.
struct ControllingTerminal(OwnedFd);

impl ControllingTerminal {
    /// Opens the controlling terminal; `None` without one.
    fn open() -> Option<Self> {
        use rustix::fs::{Mode, OFlags};
        rustix::fs::open(
            "/dev/tty",
            OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .ok()
        .map(Self)
    }
}

impl TerminalModes for ControllingTerminal {
    type Saved = rustix::termios::Termios;

    fn in_foreground(&self) -> bool {
        rustix::termios::tcgetpgrp(&self.0).is_ok_and(|group| group == rustix::process::getpgrp())
    }

    fn save(&self) -> Option<Self::Saved> {
        rustix::termios::tcgetattr(&self.0).ok()
    }

    fn restore(&self, saved: &Self::Saved) {
        let _ = rustix::termios::tcsetattr(&self.0, rustix::termios::OptionalActions::Now, saved);
    }
}

/// Restores a terminal's saved modes when dropped, but only while this
/// process is still in its foreground: a backgrounded launch leaves the
/// terminal to whoever owns it now.
struct TerminalModeGuard<T: TerminalModes = ControllingTerminal> {
    terminal: T,
    saved: T::Saved,
}

impl<T: TerminalModes> TerminalModeGuard<T> {
    /// Saves `terminal`'s modes when this process is in its foreground.
    fn capture(terminal: T) -> Option<Self> {
        if !terminal.in_foreground() {
            return None;
        }
        let saved = terminal.save()?;
        Some(Self { terminal, saved })
    }
}

impl<T: TerminalModes> Drop for TerminalModeGuard<T> {
    fn drop(&mut self) {
        if self.terminal.in_foreground() {
            self.terminal.restore(&self.saved);
        }
    }
}

async fn supervise_child(
    mut ssh: OpenSsh,
    cancellation: CancellationToken,
    mut stderr_task: tokio::task::JoinHandle<()>,
) {
    let child = &mut ssh.child;
    tokio::select! {
        _ = child.wait() => {}
        () = cancellation.cancelled() => {
            if tokio::time::timeout(CHILD_GRACE_PERIOD, child.wait()).await.is_err() {
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
        }
    }
    // OpenSSH has exited: put back any terminal modes it left changed.
    drop(ssh);
    if tokio::time::timeout(CHILD_GRACE_PERIOD, &mut stderr_task)
        .await
        .is_err()
    {
        stderr_task.abort();
        let _ = stderr_task.await;
    }
}

/// Awaits `future` for at most `budget` of time spent outside open native
/// prompts. While `prompt_open` reads true the budget stops running, and that
/// paused time is itself bounded by `max_pause` in total. Returns `None` when
/// either bound is exhausted first.
async fn timeout_outside_prompts<T>(
    budget: Duration,
    max_pause: Duration,
    prompt_open: Option<watch::Receiver<bool>>,
    future: impl std::future::Future<Output = T>,
) -> Option<T> {
    let Some(mut prompt_open) = prompt_open else {
        return tokio::time::timeout(budget, future).await.ok();
    };
    tokio::pin!(future);
    let mut remaining = budget;
    let mut pause_left = max_pause;
    loop {
        let open = *prompt_open.borrow_and_update();
        let started = tokio::time::Instant::now();
        let limit = if open { pause_left } else { remaining };
        tokio::select! {
            output = &mut future => return Some(output),
            () = tokio::time::sleep(limit) => return None,
            changed = prompt_open.changed() => {
                let spent = started.elapsed();
                if open {
                    pause_left = pause_left.saturating_sub(spent);
                } else {
                    remaining = remaining.saturating_sub(spent);
                }
                if changed.is_err() {
                    // No more prompts can open; finish on the plain budget.
                    return tokio::time::timeout(remaining, future).await.ok();
                }
            }
        }
    }
}

async fn terminate_child(child: &mut Child) {
    let _ = child.start_kill();
    let _ = child.wait().await;
}

fn rendered_diagnostics(diagnostics: &Mutex<DiagnosticRing>) -> String {
    diagnostics.lock().map_or_else(
        |_| "SSH diagnostic buffer is unavailable".to_owned(),
        |ring| ring.render(),
    )
}

pub(crate) fn is_bidi_control(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{200e}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

fn sanitize_diagnostic_character(character: char) -> char {
    if is_bidi_control(character) || character.is_control() && !matches!(character, '\n' | '\t') {
        ' '
    } else {
        character
    }
}

fn bounded_failure_message(value: &str) -> String {
    value
        .chars()
        .map(sanitize_diagnostic_character)
        .take(2048)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn classify_failure(text: &str, terminal: bool, askpass_available: bool) -> RemoteFailureKind {
    let lower = text.to_ascii_lowercase();
    if lower.contains("remote host identification has changed") {
        RemoteFailureKind::ChangedHostKey
    } else if lower.contains("host key verification failed")
        || lower.contains("no host key is known")
    {
        RemoteFailureKind::UnknownHostKey
    } else if lower.contains("connection timed out")
        || lower.contains("could not resolve hostname")
        || lower.contains("no route to host")
        || lower.contains("connection refused")
    {
        RemoteFailureKind::RoutingOrTimeout
    } else if !terminal
        && !askpass_available
        && (lower.contains("can't open /dev/tty")
            || lower.contains("cannot open /dev/tty")
            || lower.contains("askpass"))
    {
        RemoteFailureKind::InteractiveAuthenticationUnavailable
    } else if lower.contains("permission denied")
        || lower.contains("no supported authentication methods")
    {
        RemoteFailureKind::AuthenticationFailed
    } else if lower.contains("command not found")
        || lower.contains("no such file or directory") && lower.contains("splinterm")
    {
        RemoteFailureKind::RemoteCommandUnavailable
    } else if lower.contains("cannot connect to the validated daemon socket")
        || lower.contains("xdg_runtime_dir is unset")
    {
        RemoteFailureKind::RemoteDaemonUnavailable
    } else if lower.contains("adjacent splinterd")
        || lower.contains("daemon endpoint")
        || lower.contains("daemon socket")
    {
        RemoteFailureKind::RelayIdentityRejected
    } else if lower.contains("graphical relay version")
        || lower.contains("invalid handshake")
        || lower.contains("magic is invalid")
    {
        RemoteFailureKind::MultiplexerIncompatible
    } else {
        RemoteFailureKind::TransportFailed
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, time::SystemTime};

    use super::*;

    /// A fake terminal: whether this process is in its foreground, its
    /// current mode, and every restore.
    #[derive(Clone, Default)]
    struct FakeTerminal {
        foreground: std::rc::Rc<std::cell::Cell<bool>>,
        echo: std::rc::Rc<std::cell::Cell<bool>>,
        restores: std::rc::Rc<std::cell::Cell<u32>>,
        unreadable: bool,
    }

    impl TerminalModes for FakeTerminal {
        type Saved = bool;

        fn in_foreground(&self) -> bool {
            self.foreground.get()
        }

        fn save(&self) -> Option<bool> {
            (!self.unreadable).then(|| self.echo.get())
        }

        fn restore(&self, saved: &bool) {
            self.echo.set(*saved);
            self.restores.set(self.restores.get() + 1);
        }
    }

    fn foreground_terminal() -> FakeTerminal {
        let terminal = FakeTerminal::default();
        terminal.foreground.set(true);
        terminal.echo.set(true);
        terminal
    }

    #[test]
    fn a_killed_openssh_leaves_echo_restored_on_a_foreground_terminal() {
        let terminal = foreground_terminal();
        let guard = TerminalModeGuard::capture(terminal.clone()).expect("foreground terminal");
        // OpenSSH turns echo off to read a passphrase, then is killed.
        terminal.echo.set(false);
        drop(guard);
        assert!(terminal.echo.get(), "echo is back on");
        assert_eq!(terminal.restores.get(), 1);
    }

    #[test]
    fn a_terminal_this_process_does_not_own_is_left_alone() {
        // Started in the background: nothing is saved or restored.
        let background = FakeTerminal::default();
        assert!(TerminalModeGuard::capture(background.clone()).is_none());
        assert_eq!(background.restores.get(), 0);

        // Moved to the background meanwhile: the new owner keeps its modes.
        let terminal = foreground_terminal();
        let guard = TerminalModeGuard::capture(terminal.clone()).unwrap();
        terminal.echo.set(false);
        terminal.foreground.set(false);
        drop(guard);
        assert!(!terminal.echo.get());
        assert_eq!(terminal.restores.get(), 0);

        // Modes that cannot be read are never written.
        let unreadable = FakeTerminal {
            unreadable: true,
            ..foreground_terminal()
        };
        assert!(TerminalModeGuard::capture(unreadable.clone()).is_none());
        assert_eq!(unreadable.restores.get(), 0);
    }

    #[test]
    fn openssh_is_dropped_before_its_terminal_modes_are_restored() {
        // Field drop order is declaration order: the kill_on_drop child goes
        // first. This pins the field order the restore depends on.
        let source = include_str!("remote_session.rs");
        let start = source.find("struct OpenSsh {").unwrap();
        let body = &source[start..start + source[start..].find("\n}").unwrap()];
        let child = body.find("    child: Child,").unwrap();
        let modes = body.find("    _terminal_modes:").unwrap();
        assert!(child < modes);
    }

    #[test]
    fn failure_categories_are_specific_and_diagnostics_are_bounded() {
        assert_eq!(
            classify_failure("REMOTE HOST IDENTIFICATION HAS CHANGED", true, false),
            RemoteFailureKind::ChangedHostKey
        );
        assert_eq!(
            classify_failure("Host key verification failed", true, false),
            RemoteFailureKind::UnknownHostKey
        );
        assert_eq!(
            classify_failure("Permission denied (publickey,password)", true, false),
            RemoteFailureKind::AuthenticationFailed
        );
        assert_eq!(
            classify_failure("read_passphrase: can't open /dev/tty", false, false),
            RemoteFailureKind::InteractiveAuthenticationUnavailable
        );
        let mut ring = DiagnosticRing::default();
        ring.push(&vec![b'x'; MAX_SSH_DIAGNOSTIC_BYTES + 100]);
        assert!(ring.truncated);
        assert!(ring.render().len() <= MAX_SSH_DIAGNOSTIC_BYTES + '…'.len_utf8());
        let mut bidi = DiagnosticRing::default();
        bidi.push("safe\u{202e}spoof".as_bytes());
        assert_eq!(bidi.render(), "safe spoof");
        assert_eq!(bounded_failure_message("a\u{2066}b"), "a b");
    }

    #[test]
    fn askpass_requires_an_absolute_executable_file() {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("splinterm-askpass-{nonce}"));
        fs::create_dir(&directory).unwrap();
        let executable = directory.join("askpass");
        fs::write(&executable, "fixture").unwrap();
        fs::set_permissions(
            &executable,
            <fs::Permissions as PermissionsExt>::from_mode(0o700),
        )
        .unwrap();
        assert!(validate_askpass_value(false, Some(executable.as_os_str())).unwrap());
        assert!(!validate_askpass_value(true, Some(executable.as_os_str())).unwrap());
        assert!(validate_askpass_value(false, Some(OsStr::new("relative"))).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn prompt_provider_prefers_terminal_then_user_askpass_then_native() {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("splinterm-provider-{nonce}"));
        fs::create_dir(&directory).unwrap();
        let executable = directory.join("askpass");
        fs::write(&executable, "fixture").unwrap();
        fs::set_permissions(
            &executable,
            <fs::Permissions as PermissionsExt>::from_mode(0o700),
        )
        .unwrap();
        let environment = |terminal, askpass: Option<&std::path::Path>| AuthEnvironment {
            terminal,
            user_askpass: askpass.map(|path| path.as_os_str().to_owned()),
            ..AuthEnvironment::default()
        };
        let select = |environment: &AuthEnvironment, native| {
            select_prompt_provider(environment, native).unwrap()
        };

        // A terminal wins: OpenSSH prompts there and nothing is forced, even
        // with a user askpass or a native prompt on offer.
        for native in [false, true] {
            assert_eq!(
                select(&environment(true, Some(&executable)), native),
                PromptProvider::Terminal
            );
            assert_eq!(
                select(&environment(true, None), native),
                PromptProvider::Terminal
            );
        }
        // The user's own SSH_ASKPASS is preferred over the native prompt.
        for native in [false, true] {
            assert_eq!(
                select(&environment(false, Some(&executable)), native),
                PromptProvider::UserAskpass
            );
        }
        // Otherwise the native prompt, only when the caller offers one.
        assert_eq!(
            select(&environment(false, None), true),
            PromptProvider::NativePrompt
        );
        assert_eq!(
            select(&environment(false, None), false),
            PromptProvider::Unavailable
        );
        let empty = AuthEnvironment {
            terminal: false,
            user_askpass: Some(OsString::new()),
            ..AuthEnvironment::default()
        };
        assert_eq!(select(&empty, true), PromptProvider::NativePrompt);
        // An explicit but broken SSH_ASKPASS is reported, never replaced.
        let broken = AuthEnvironment {
            terminal: false,
            user_askpass: Some(OsString::from("relative-askpass")),
            ..AuthEnvironment::default()
        };
        assert!(select_prompt_provider(&broken, true).is_err());
        let missing = environment(false, Some(&directory.join("missing")));
        assert!(select_prompt_provider(&missing, true).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn native_prompt_failures_classify_as_authentication_not_unavailable() {
        // With a native or user prompt available, a refused prompt surfaces as
        // OpenSSH's own authentication failure.
        assert_eq!(
            classify_failure("Permission denied (publickey,password).", false, true),
            RemoteFailureKind::AuthenticationFailed
        );
        assert_eq!(
            classify_failure(
                "splinterm askpass: the Splinterm prompt is unavailable\nPermission denied (password).",
                false,
                true
            ),
            RemoteFailureKind::AuthenticationFailed
        );
    }

    /// Drives the prompt watch through `phases` of (prompt open, seconds), then
    /// completes the negotiation stand-in.
    fn scripted_negotiation(
        phases: &'static [(bool, u64)],
    ) -> (
        watch::Receiver<bool>,
        impl std::future::Future<Output = &'static str>,
    ) {
        let (sender, receiver) = watch::channel(false);
        let negotiation = async move {
            for &(open, seconds) in phases {
                sender.send_replace(open);
                tokio::time::sleep(Duration::from_secs(seconds)).await;
            }
            "negotiated"
        };
        (receiver, negotiation)
    }

    const BUDGET: Duration = Duration::from_secs(10);

    #[tokio::test(start_paused = true)]
    async fn each_sequential_native_prompt_gets_its_full_timeout() {
        // A key passphrase and then a password, each answered after 100 s, with
        // 9 s of OpenSSH work in all: far past a fixed 10 s deadline.
        let (prompt_open, negotiation) =
            scripted_negotiation(&[(false, 4), (true, 100), (false, 4), (true, 100), (false, 1)]);
        let started = tokio::time::Instant::now();
        assert_eq!(
            timeout_outside_prompts(
                BUDGET,
                MAX_NATIVE_PROMPT_PAUSE,
                Some(prompt_open),
                negotiation
            )
            .await,
            Some("negotiated")
        );
        assert_eq!(started.elapsed(), Duration::from_secs(209));
    }

    #[tokio::test(start_paused = true)]
    async fn time_outside_prompts_still_exhausts_the_negotiation_deadline() {
        let (prompt_open, negotiation) =
            scripted_negotiation(&[(false, 4), (true, 100), (false, 60)]);
        let started = tokio::time::Instant::now();
        assert_eq!(
            timeout_outside_prompts(
                BUDGET,
                MAX_NATIVE_PROMPT_PAUSE,
                Some(prompt_open),
                negotiation
            )
            .await,
            None
        );
        // 4 s before the prompt plus the remaining 6 s after it.
        assert_eq!(started.elapsed(), Duration::from_secs(110));
    }

    #[tokio::test(start_paused = true)]
    async fn paused_prompt_time_has_an_overall_cap() {
        let (prompt_open, negotiation) =
            scripted_negotiation(&[(true, 300), (false, 1), (true, 400), (false, 1)]);
        let started = tokio::time::Instant::now();
        let cap = Duration::from_secs(600);
        assert_eq!(
            timeout_outside_prompts(BUDGET, cap, Some(prompt_open), negotiation).await,
            None
        );
        // 300 s + 1 s + the last 300 s of pause allowance.
        assert_eq!(started.elapsed(), Duration::from_secs(601));
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_native_prompt_the_deadline_is_fixed() {
        let started = tokio::time::Instant::now();
        assert_eq!(
            timeout_outside_prompts(
                BUDGET,
                MAX_NATIVE_PROMPT_PAUSE,
                None,
                tokio::time::sleep(Duration::from_secs(11)),
            )
            .await,
            None
        );
        assert_eq!(started.elapsed(), BUDGET);

        // A prompt watch whose broker is gone falls back to the same budget.
        let (sender, receiver) = watch::channel(false);
        drop(sender);
        assert_eq!(
            timeout_outside_prompts(BUDGET, MAX_NATIVE_PROMPT_PAUSE, Some(receiver), async {
                tokio::time::sleep(Duration::from_secs(9)).await;
                "negotiated"
            },)
            .await,
            Some("negotiated")
        );
    }

    fn scratch_directory(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("splinterm-{label}-{nonce}"));
        fs::create_dir(&directory).unwrap();
        directory
    }

    fn agent_profile() -> RemoteProfile {
        crate::remote::RemoteCatalog::parse(
            "version = 1\n[remotes.test]\nhost = \"example.invalid\"\nport = 2222\n",
            None,
        )
        .unwrap()
        .get("test")
        .unwrap()
        .clone()
    }

    #[test]
    fn agent_discovery_keeps_ssh_auth_sock_then_tries_runtime_sockets_in_order() {
        let uid = rustix::process::geteuid().as_raw();
        let directory = scratch_directory("agent");
        let runtime = directory.as_os_str();

        // Nothing there, or no usable runtime directory: no agent.
        assert_eq!(
            find_agent_socket(None, Some(runtime), uid),
            AgentSocket::Missing
        );
        assert_eq!(find_agent_socket(None, None, uid), AgentSocket::Missing);
        assert_eq!(
            find_agent_socket(None, Some(OsStr::new("relative/run")), uid),
            AgentSocket::Missing
        );

        // A plain file where a socket belongs is not an agent.
        fs::create_dir(directory.join("gcr")).unwrap();
        fs::write(directory.join("gcr/ssh"), "not a socket").unwrap();
        assert_eq!(
            find_agent_socket(None, Some(runtime), uid),
            AgentSocket::Missing
        );

        // gnome-keyring's socket is found; the systemd agent, earlier in the
        // order, wins once it exists.
        fs::create_dir(directory.join("keyring")).unwrap();
        let _keyring =
            std::os::unix::net::UnixListener::bind(directory.join("keyring/ssh")).unwrap();
        assert_eq!(
            find_agent_socket(None, Some(runtime), uid),
            AgentSocket::Found(directory.join("keyring/ssh"))
        );
        let _systemd =
            std::os::unix::net::UnixListener::bind(directory.join("ssh-agent.socket")).unwrap();
        assert_eq!(
            find_agent_socket(None, Some(runtime), uid),
            AgentSocket::Found(directory.join("ssh-agent.socket"))
        );
        // A socket owned by someone else is skipped.
        assert_eq!(
            find_agent_socket(None, Some(runtime), uid.wrapping_add(1)),
            AgentSocket::Missing
        );

        // A set SSH_AUTH_SOCK is kept as is, reachable only when it is a
        // socket; an empty one counts as unset.
        let systemd = directory.join("ssh-agent.socket");
        assert_eq!(
            find_agent_socket(Some(systemd.as_os_str()), Some(runtime), uid),
            AgentSocket::Inherited {
                reachable: true,
                gpg_agent: false
            }
        );
        assert_eq!(
            find_agent_socket(Some(directory.join("gone").as_os_str()), Some(runtime), uid),
            AgentSocket::Inherited {
                reachable: false,
                gpg_agent: false
            }
        );
        assert_eq!(
            find_agent_socket(Some(OsStr::new("")), Some(runtime), uid),
            AgentSocket::Found(systemd)
        );
        assert_eq!(
            find_agent_socket(
                Some(OsStr::new("/run/user/0/gnupg/S.gpg-agent.ssh")),
                None,
                uid
            ),
            AgentSocket::Inherited {
                reachable: false,
                gpg_agent: true
            }
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn only_the_default_add_keys_setting_is_overridden() {
        let config = |value: &str| format!("user me\naddkeystoagent {value}\nport 22\n");
        assert!(leaves_keys_out_of_agent(config("false").as_bytes()));
        for kept in ["true", "ask", "confirm", "true 3600", "confirm 8h"] {
            assert!(!leaves_keys_out_of_agent(config(kept).as_bytes()), "{kept}");
        }
        assert!(!leaves_keys_out_of_agent(b""));
        assert!(!leaves_keys_out_of_agent(b"user me\n"));
    }

    #[test]
    fn the_config_query_is_ssh_g_with_the_remote_options_and_no_command() {
        let profile = agent_profile();
        let query = ssh_config_query(&profile, OsStr::new("ssh"));
        let arguments: Vec<_> = query.as_std().get_args().collect();
        let plan = profile.ssh_plan();
        assert_eq!(arguments[0], OsStr::new("-G"));
        let (command, options) = plan.arguments().split_last().unwrap();
        assert_eq!(
            &arguments[1..],
            options.iter().map(OsString::as_os_str).collect::<Vec<_>>()
        );
        assert!(!arguments.contains(&command.as_os_str()));
        assert_eq!(arguments.last(), Some(&OsStr::new("example.invalid")));
    }

    #[tokio::test]
    async fn agent_setup_adds_keys_only_when_an_agent_is_reachable_and_config_is_default() {
        let directory = scratch_directory("agent-setup");
        // Run through /bin/sh so the script itself is never executed.
        let script = directory.join("fake-ssh-g");
        let query = |output: &str| {
            let body = format!("[ \"$1\" = -G ] || exit 9\nprintf '{output}'\n");
            fs::write(&script, body).unwrap();
            let mut command = Command::new("/bin/sh");
            command.arg(&script).arg("-G").arg("example.invalid");
            command
        };
        let found = AgentSocket::Found(directory.join("ssh-agent.socket"));

        let setup = agent_setup(found.clone(), query("addkeystoagent false\\n")).await;
        assert_eq!(setup.socket, Some(directory.join("ssh-agent.socket")));
        assert!(setup.add_keys);
        // The user's config already decides: keep it.
        let kept = agent_setup(found.clone(), query("addkeystoagent confirm\\n")).await;
        assert!(!kept.add_keys);
        // A failing query changes nothing.
        let mut failing = Command::new("/bin/sh");
        failing.arg("-c").arg("echo addkeystoagent false; exit 1");
        assert!(!agent_setup(found, failing).await.add_keys);

        // An inherited reachable agent: the socket is left alone.
        let inherited = agent_setup(
            AgentSocket::Inherited {
                reachable: true,
                gpg_agent: false,
            },
            query("addkeystoagent false\\n"),
        )
        .await;
        assert_eq!(
            inherited,
            AgentSetup {
                socket: None,
                add_keys: true
            }
        );
        // gpg-agent would keep an added key on disk: its socket is used,
        // but no key is added and no query runs.
        let gpg = directory.join("gnupg/S.gpg-agent.ssh");
        let mut never = Command::new("/bin/sh");
        never.arg("-c").arg("echo addkeystoagent false");
        assert_eq!(
            agent_setup(AgentSocket::Found(gpg.clone()), never).await,
            AgentSetup {
                socket: Some(gpg),
                add_keys: false
            }
        );
        // No reachable agent, or an inherited gpg-agent: nothing added.
        for found in [
            AgentSocket::Inherited {
                reachable: false,
                gpg_agent: false,
            },
            AgentSocket::Inherited {
                reachable: true,
                gpg_agent: true,
            },
            AgentSocket::Missing,
        ] {
            let mut never = Command::new("/bin/sh");
            never.arg("-c").arg("echo addkeystoagent false");
            assert_eq!(agent_setup(found, never).await, AgentSetup::default());
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn the_openssh_command_carries_the_agent_setup() {
        let profile = agent_profile();
        let environment = |agent| AuthEnvironment {
            agent,
            ..AuthEnvironment::default()
        };
        let command = |environment: &AuthEnvironment| {
            ssh_command(
                &profile,
                OsStr::new("ssh"),
                PromptProvider::Unavailable,
                environment,
                None,
            )
        };
        let auth_sock = |command: &Command| {
            command
                .as_std()
                .get_envs()
                .find(|(key, _)| *key == OsStr::new("SSH_AUTH_SOCK"))
                .map(|(_, value)| value.map(OsStr::to_owned))
        };
        let plan = profile.ssh_plan();

        // Default: exactly the fixed plan and no SSH_AUTH_SOCK change.
        let fixed: Vec<_> = plan.arguments().iter().map(OsString::as_os_str).collect();
        let unchanged = command(&environment(AgentSetup::default()));
        assert_eq!(unchanged.as_std().get_args().collect::<Vec<_>>(), fixed);
        assert_eq!(auth_sock(&unchanged), None);

        // A found agent and a default config: the socket and AddKeysToAgent.
        let socket = PathBuf::from("/run/user/1000/ssh-agent.socket");
        let full = command(&environment(AgentSetup {
            socket: Some(socket.clone()),
            add_keys: true,
        }));
        let arguments: Vec<_> = full.as_std().get_args().collect();
        assert_eq!(
            &arguments[..2],
            [OsStr::new("-o"), OsStr::new("AddKeysToAgent=yes")]
        );
        assert_eq!(&arguments[2..], fixed);
        assert_eq!(auth_sock(&full), Some(Some(socket.into_os_string())));
    }
}
