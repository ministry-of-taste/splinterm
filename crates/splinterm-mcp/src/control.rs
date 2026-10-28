use std::{
    collections::{HashMap, VecDeque, hash_map::RandomState},
    hash::BuildHasher,
    path::PathBuf,
    sync::{Arc, Weak},
    time::Duration,
};

use serde_json::{Value, json};
use splinterm_automation_client::{Connection, is_connection_loss};
use splinterm_core::{DojoId, LairId, SplintId};
use splinterm_protocol::{
    ControlMode, ControlTransferDecision, ControlTransferOutcome, Request, Response, ServerFrame,
    SubscriptionEvent, validate_control_modes,
};
#[cfg(test)]
use tokio::sync::Notify;
use tokio::{
    sync::{Mutex, mpsc, oneshot},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::{dispatch, resources::ResourceRegistry};

const SCHEMA: &str = "splinterm.mcp.v2";
const MAXIMUM_CONTROL_HANDLES: usize = 8;
const TRANSFER_LIFETIME: Duration = Duration::from_secs(16);
/// How long splinterd waits for a transfer's answer before it ends the
/// transfer and closes the requester's connection.
const DAEMON_TRANSFER_TIMEOUT: Duration =
    Duration::from_secs(splinterm_protocol::CONTROL_TRANSFER_TIMEOUT_SECONDS);

/// How many handles that expired with their daemon connection are remembered,
/// so using one says why it no longer works (Plan 0061 M3.12b).
const MAXIMUM_EXPIRED_HANDLES: usize = 32;

/// A transfer that ended without control, then what to do next (Plan 0067
/// M4). A grant that let the agent ask also covers a takeover, but a policy
/// rule or development access may not, so both next steps are given.
macro_rules! after_unanswered_transfer {
    ($what:literal) => {
        concat!(
            $what,
            "; to take the pane without asking, call acquire_control with takeover: true (needs a Look and type or Manage grant; request_lair_access asks for one). Or call request_control_transfer again and ask the person to press Ctrl+Shift+Y (the default key; Ctrl+Shift+N denies) in that pane within 15 seconds"
        )
    };
}

/// A denied transfer respects the person's answer (operator decision
/// 2026-10-28): no retry or takeover unless they agree.
const TRANSFER_DENIED: &str = "the pane's controller denied the transfer; do not ask again or take the pane unless the person tells you to. If they do: call request_control_transfer and have them press Ctrl+Shift+Y (the default key) in that pane within 15 seconds, or, with a Look and type or Manage grant, call acquire_control with takeover: true";
const TRANSFER_CANCELLED: &str = after_unanswered_transfer!(
    "the transfer was cancelled before an answer, as when control of the pane changed"
);
const TRANSFER_TIMED_OUT: &str =
    after_unanswered_transfer!("nobody answered the transfer within 15 seconds");
/// A transfer request or takeover no grant covers (Plan 0067 M4).
const TAKEOVER_NEEDS_GRANT: &str = "this needs a Look and type or Manage grant for this Splint's Lair; call request_lair_access with level look_and_type";

#[derive(Debug, Clone)]
struct Metadata {
    lair_id: LairId,
    dojo_id: DojoId,
    splint_id: SplintId,
    incarnation: u64,
    modes: Vec<ControlMode>,
}

#[derive(Debug)]
enum ControllerCommand {
    Request {
        request: Request,
        cancellation: CancellationToken,
        terminate: bool,
        reply: oneshot::Sender<Result<Response, dispatch::DispatchFailure>>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

#[derive(Debug)]
enum TransferCommand {
    Take {
        reply: oneshot::Sender<Option<Connection>>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

#[derive(Debug)]
struct ControllerEntry {
    metadata: Metadata,
    private_id: u64,
    sender: mpsc::Sender<ControllerCommand>,
    task: JoinHandle<()>,
}

#[derive(Debug)]
struct TransferEntry {
    metadata: Metadata,
    private_id: u64,
    owner_handle: String,
    sender: mpsc::Sender<TransferCommand>,
    task: JoinHandle<()>,
}

#[derive(Debug)]
struct State {
    controllers: HashMap<String, ControllerEntry>,
    transfers: HashMap<String, TransferEntry>,
    /// Handles whose daemon connection ended, as when splinterd is upgraded:
    /// the daemon released their authority, so they stay invalid. Newest last.
    expired: VecDeque<String>,
    next_token: u64,
    next_revision: u64,
    hashers: [RandomState; 2],
}

impl Default for State {
    fn default() -> Self {
        Self {
            controllers: HashMap::new(),
            transfers: HashMap::new(),
            expired: VecDeque::new(),
            next_token: 1,
            next_revision: 1,
            hashers: [RandomState::new(), RandomState::new()],
        }
    }
}

impl State {
    fn count(&self) -> usize {
        self.controllers.len() + self.transfers.len()
    }

    fn token(&mut self, prefix: &str) -> Result<String, dispatch::DispatchFailure> {
        let counter = self.next_token;
        self.next_token = self
            .next_token
            .checked_add(1)
            .ok_or_else(dispatch::DispatchFailure::resource_limit)?;
        let first = self.hashers[0].hash_one(("splinterm-mcp-control-v1", prefix, counter));
        let second = self.hashers[1].hash_one(("splinterm-mcp-control-v1", prefix, counter));
        Ok(format!("{prefix}_{counter:016x}{first:016x}{second:016x}"))
    }

    /// Remembers that `token` expired with its daemon connection.
    fn expire(&mut self, token: &str) {
        if self.expired.len() == MAXIMUM_EXPIRED_HANDLES {
            self.expired.pop_front();
        }
        self.expired.push_back(token.to_owned());
    }

    /// The failure for a handle this registry does not hold: expired with its
    /// daemon connection, or never valid.
    fn missing_handle(&self, handle: &str) -> dispatch::DispatchFailure {
        if self.expired.iter().any(|expired| expired == handle) {
            expired_handle()
        } else {
            invalid_handle()
        }
    }

    fn revision(&mut self) -> Result<u64, dispatch::DispatchFailure> {
        let revision = self.next_revision;
        self.next_revision = self
            .next_revision
            .checked_add(1)
            .ok_or_else(dispatch::DispatchFailure::resource_limit)?;
        Ok(revision)
    }
}

#[derive(Debug)]
struct Inner {
    state: Mutex<State>,
    gate: Mutex<()>,
    resources: Arc<ResourceRegistry>,
    shutdown: CancellationToken,
    socket: Option<PathBuf>,
    #[cfg(test)]
    post_commit_hook: std::sync::Mutex<Option<(Arc<Notify>, Arc<Notify>)>>,
}

#[derive(Debug, Clone)]
pub(crate) struct ControlRegistry(Arc<Inner>);

#[derive(Debug, Clone, Copy)]
enum RegisteredKind {
    Controller,
    Transfer,
}

#[derive(Debug)]
struct RegistrationGuard {
    inner: Weak<Inner>,
    token: Option<String>,
    kind: RegisteredKind,
}

impl RegistrationGuard {
    fn new(inner: &Arc<Inner>, token: String, kind: RegisteredKind) -> Self {
        Self {
            inner: Arc::downgrade(inner),
            token: Some(token),
            kind,
        }
    }

    fn disarm(&mut self) {
        self.token = None;
    }
}

impl Drop for RegistrationGuard {
    fn drop(&mut self) {
        let Some(token) = self.token.take() else {
            return;
        };
        let inner = self.inner.clone();
        let kind = self.kind;
        tokio::spawn(async move {
            rollback_registration(inner, token, kind).await;
        });
    }
}

#[derive(Debug)]
struct ModeCleanupGuard {
    inner: Weak<Inner>,
    metadata: Option<Metadata>,
}

impl ModeCleanupGuard {
    fn new(inner: &Arc<Inner>, metadata: Metadata) -> Self {
        Self {
            inner: Arc::downgrade(inner),
            metadata: Some(metadata),
        }
    }

    fn disarm(&mut self) {
        self.metadata = None;
    }
}

impl Drop for ModeCleanupGuard {
    fn drop(&mut self) {
        let Some(metadata) = self.metadata.take() else {
            return;
        };
        let inner = self.inner.clone();
        tokio::spawn(async move {
            clear_modes_if_unowned(inner, metadata).await;
        });
    }
}

impl ControlRegistry {
    pub(crate) fn new(resources: Arc<ResourceRegistry>) -> Self {
        Self(Arc::new(Inner {
            state: Mutex::new(State::default()),
            gate: Mutex::new(()),
            resources,
            shutdown: CancellationToken::new(),
            socket: None,
            #[cfg(test)]
            post_commit_hook: std::sync::Mutex::new(None),
        }))
    }

    #[cfg(any(test, feature = "integration-test"))]
    pub(crate) fn new_at(resources: Arc<ResourceRegistry>, socket: &std::path::Path) -> Self {
        Self(Arc::new(Inner {
            state: Mutex::new(State::default()),
            gate: Mutex::new(()),
            resources,
            shutdown: CancellationToken::new(),
            socket: Some(socket.to_owned()),
            #[cfg(test)]
            post_commit_hook: std::sync::Mutex::new(None),
        }))
    }

    pub(crate) async fn dispatch(
        &self,
        tool: &str,
        arguments: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, dispatch::DispatchFailure> {
        match tool {
            "splinterm.acquire_control" => self.acquire(arguments, cancellation).await,
            "splinterm.request_control_transfer" => {
                self.request_transfer(arguments, cancellation).await
            }
            "splinterm.decide_control_transfer" => self.decide(arguments, cancellation).await,
            "splinterm.release_control" => self.release(arguments, cancellation).await,
            "splinterm.input" => self.input(arguments, cancellation).await,
            "splinterm.resize" => self.resize(arguments, cancellation).await,
            _ => Err(dispatch::DispatchFailure::internal()),
        }
    }

    async fn acquire(
        &self,
        arguments: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, dispatch::DispatchFailure> {
        let _gate = self.0.gate.lock().await;
        self.ensure_capacity().await?;
        let splint_id = parse_splint(arguments)?;
        let incarnation = parse_incarnation(arguments)?;
        let modes = parse_modes(arguments)?;
        let takeover = arguments
            .get("takeover")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut connection = self.connect(cancellation).await?;
        let control_request = if takeover {
            Request::ForceControlTransfer {
                splint_id,
                incarnation,
            }
        } else {
            Request::AcquireControl {
                splint_id,
                incarnation,
                modes: modes.clone(),
            }
        };
        let response = request(&mut connection, control_request, cancellation)
            .await
            .map_err(|failure| {
                if takeover {
                    needs_takeover_grant(failure)
                } else {
                    failure
                }
            })?;
        let Response::ControlGranted {
            controller_id,
            lair_id,
            dojo_id,
        } = response
        else {
            return Err(dispatch::DispatchFailure::internal());
        };
        if controller_id == 0 {
            return Err(dispatch::DispatchFailure::internal());
        }
        let metadata = Metadata {
            lair_id,
            dojo_id,
            splint_id,
            incarnation,
            modes,
        };
        let handle = self
            .insert_controller(connection, controller_id, metadata.clone())
            .await?;
        let mut registration =
            RegistrationGuard::new(&self.0, handle.clone(), RegisteredKind::Controller);
        self.observe_post_commit_cancellation(cancellation).await?;
        self.publish_modes(&metadata).await;
        let revision = self.revision().await?;
        let output = success(
            "splinterm.acquire_control",
            resource(&metadata, revision),
            json!({
                "committed": true,
                "controller_handle": handle,
                "modes": mode_names(&metadata.modes),
            }),
        );
        registration.disarm();
        Ok(output)
    }

    async fn request_transfer(
        &self,
        arguments: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, dispatch::DispatchFailure> {
        let _gate = self.0.gate.lock().await;
        self.ensure_capacity().await?;
        let splint_id = parse_splint(arguments)?;
        let incarnation = parse_incarnation(arguments)?;
        let modes = parse_modes(arguments)?;
        let owner_handle = {
            let state = self.0.state.lock().await;
            state.controllers.iter().find_map(|(handle, entry)| {
                (entry.metadata.splint_id == splint_id && entry.metadata.incarnation == incarnation)
                    .then(|| handle.clone())
            })
        };
        let mut requester = self.connect(cancellation).await?;
        let external_subscription = if owner_handle.is_none() {
            match request(
                &mut requester,
                Request::SubscribeControl {
                    splint_id,
                    incarnation,
                },
                cancellation,
            )
            .await
            .map_err(needs_takeover_grant)?
            {
                Response::ControlSubscribed {
                    subscription_id, ..
                } if subscription_id != 0 => Some(subscription_id),
                _ => return Err(dispatch::DispatchFailure::internal()),
            }
        } else {
            None
        };
        // Taken before the request, so it is never later than splinterd's
        // own transfer timer.
        let requested_at = tokio::time::Instant::now();
        let response = request(
            &mut requester,
            Request::RequestControlTransfer {
                splint_id,
                incarnation,
                modes: modes.clone(),
            },
            cancellation,
        )
        .await
        .map_err(needs_takeover_grant)?;
        let Response::ControlTransferPending {
            transfer_id,
            lair_id,
            dojo_id,
        } = response
        else {
            return Err(dispatch::DispatchFailure::internal());
        };
        if transfer_id == 0 {
            return Err(dispatch::DispatchFailure::internal());
        }
        let metadata = Metadata {
            lair_id,
            dojo_id,
            splint_id,
            incarnation,
            modes,
        };
        if let Some(subscription_id) = external_subscription {
            return self
                .complete_external_transfer(
                    requester,
                    subscription_id,
                    transfer_id,
                    metadata,
                    requested_at,
                    cancellation,
                )
                .await;
        }
        let owner_handle = owner_handle.expect("internal transfer has an MCP owner");
        let handle = self
            .insert_transfer(requester, transfer_id, owner_handle, metadata.clone())
            .await?;
        let mut registration =
            RegistrationGuard::new(&self.0, handle.clone(), RegisteredKind::Transfer);
        self.observe_post_commit_cancellation(cancellation).await?;
        let revision = self.revision().await?;
        let output = success(
            "splinterm.request_control_transfer",
            resource(&metadata, revision),
            json!({
                "committed": true,
                "transfer_handle": handle,
                "modes": mode_names(&metadata.modes),
            }),
        );
        registration.disarm();
        Ok(output)
    }

    async fn complete_external_transfer(
        &self,
        mut requester: Connection,
        subscription_id: u64,
        transfer_id: u64,
        metadata: Metadata,
        requested_at: tokio::time::Instant,
        cancellation: &CancellationToken,
    ) -> Result<Value, dispatch::DispatchFailure> {
        let resolution = tokio::time::timeout(TRANSFER_LIFETIME, async {
            loop {
                let frame = tokio::select! {
                    () = cancellation.cancelled() => {
                        return Err(dispatch::DispatchFailure::new(
                            "cancelled",
                            "the tool call was cancelled",
                            true,
                        ));
                    }
                    frame = requester.next_server_frame() => frame
                        .map_err(|error| ended_transfer_failure(&error, requested_at.elapsed()))?,
                };
                if let ServerFrame::Event {
                    subscription_id: event_subscription,
                    event:
                        SubscriptionEvent::ControlTransferResolved {
                            transfer_id: resolved,
                            outcome,
                            controller_id,
                        },
                    ..
                } = frame
                    && event_subscription == subscription_id
                    && resolved == transfer_id
                {
                    return Ok((outcome, controller_id));
                }
            }
        })
        .await
        .map_err(|_| transfer_timed_out())??;
        let controller_id = match resolution {
            (ControlTransferOutcome::Granted, Some(controller_id)) => controller_id,
            (outcome, _) => return Err(unanswered_transfer(outcome)),
        };
        let handle = self
            .insert_controller(requester, controller_id, metadata.clone())
            .await?;
        let mut registration =
            RegistrationGuard::new(&self.0, handle.clone(), RegisteredKind::Controller);
        self.observe_post_commit_cancellation(cancellation).await?;
        self.publish_modes(&metadata).await;
        let revision = self.revision().await?;
        let output = success(
            "splinterm.request_control_transfer",
            resource(&metadata, revision),
            json!({
                "committed": true,
                "controller_handle": handle,
                "outcome": "granted",
                "modes": mode_names(&metadata.modes),
            }),
        );
        registration.disarm();
        Ok(output)
    }

    async fn decide(
        &self,
        arguments: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, dispatch::DispatchFailure> {
        let _gate = self.0.gate.lock().await;
        let transfer_handle = arguments["transfer_handle"]
            .as_str()
            .ok_or_else(dispatch::DispatchFailure::internal)?;
        require_handle(transfer_handle, "xfer")?;
        let decision = match arguments["decision"].as_str() {
            Some("accept") => ControlTransferDecision::Accept,
            Some("deny") => ControlTransferDecision::Deny,
            _ => return Err(dispatch::DispatchFailure::internal()),
        };
        let (metadata, private_id, owner_handle, sender) = {
            let state = self.0.state.lock().await;
            let entry = state
                .transfers
                .get(transfer_handle)
                .ok_or_else(|| state.missing_handle(transfer_handle))?;
            (
                entry.metadata.clone(),
                entry.private_id,
                entry.owner_handle.clone(),
                entry.sender.clone(),
            )
        };
        let requester = take_transfer(sender).await.ok_or_else(stale_handle)?;
        let owner = {
            let state = self.0.state.lock().await;
            match state.controllers.get(&owner_handle) {
                Some(entry) => entry.sender.clone(),
                None if state.expired.contains(&owner_handle) => return Err(expired_handle()),
                None => return Err(stale_handle()),
            }
        };
        let terminate_owner = decision == ControlTransferDecision::Accept;
        let response = actor_request(
            &owner,
            Request::DecideControlTransfer {
                transfer_id: private_id,
                decision,
            },
            cancellation,
            terminate_owner,
        )
        .await?;
        let Response::ControlTransferDecided {
            outcome,
            controller_id,
        } = response
        else {
            return Err(dispatch::DispatchFailure::internal());
        };
        {
            let mut state = self.0.state.lock().await;
            state.transfers.remove(transfer_handle);
            if decision == ControlTransferDecision::Accept {
                state.controllers.remove(&owner_handle);
            }
        }
        let (controller_handle, mut registration) = match (decision, outcome, controller_id) {
            (ControlTransferDecision::Deny, ControlTransferOutcome::Denied, None) => (None, None),
            (ControlTransferDecision::Accept, ControlTransferOutcome::Granted, Some(id))
                if id > 0 =>
            {
                let handle = self
                    .insert_controller(requester, id, metadata.clone())
                    .await?;
                let guard =
                    RegistrationGuard::new(&self.0, handle.clone(), RegisteredKind::Controller);
                self.observe_post_commit_cancellation(cancellation).await?;
                self.publish_modes(&metadata).await;
                (Some(handle), Some(guard))
            }
            _ => {
                return Err(dispatch::DispatchFailure::new(
                    "control_transfer_unavailable",
                    "control transfer is unavailable",
                    true,
                ));
            }
        };
        let revision = self.revision().await?;
        let output = success(
            "splinterm.decide_control_transfer",
            resource(&metadata, revision),
            json!({
                "committed": true,
                "decision": if decision == ControlTransferDecision::Accept { "accepted" } else { "denied" },
                "controller_handle": controller_handle,
            }),
        );
        if let Some(guard) = &mut registration {
            guard.disarm();
        }
        Ok(output)
    }

    async fn release(
        &self,
        arguments: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, dispatch::DispatchFailure> {
        let _gate = self.0.gate.lock().await;
        let handle = arguments["controller_handle"]
            .as_str()
            .ok_or_else(dispatch::DispatchFailure::internal)?;
        require_handle(handle, "ctl")?;
        let (metadata, private_id, sender) = {
            let mut state = self.0.state.lock().await;
            let Some(entry) = state.controllers.remove(handle) else {
                return Err(state.missing_handle(handle));
            };
            (entry.metadata, entry.private_id, entry.sender)
        };
        let mut mode_cleanup = ModeCleanupGuard::new(&self.0, metadata.clone());
        let response = actor_request(
            &sender,
            Request::ReleaseControl {
                controller_id: private_id,
            },
            cancellation,
            true,
        )
        .await;
        // The actor currently needs the private ID in the request. A failed
        // actor lookup still closes the connection and clears public authority.
        self.clear_modes_if_unowned(&metadata).await;
        mode_cleanup.disarm();
        let response = match response {
            Ok(response) => response,
            Err(failure) => return Err(self.expired_or(handle, failure).await),
        };
        match response {
            Response::Acknowledged => {}
            _ => return Err(dispatch::DispatchFailure::internal()),
        }
        let revision = self.revision().await?;
        Ok(success(
            "splinterm.release_control",
            resource(&metadata, revision),
            json!({"committed": true, "released": true}),
        ))
    }

    async fn input(
        &self,
        arguments: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, dispatch::DispatchFailure> {
        let text = arguments["text"]
            .as_str()
            .ok_or_else(dispatch::DispatchFailure::internal)?;
        self.action(
            arguments,
            Some(text.as_bytes().to_vec()),
            None,
            cancellation,
        )
        .await
    }

    async fn resize(
        &self,
        arguments: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, dispatch::DispatchFailure> {
        let columns = u16::try_from(
            arguments["columns"]
                .as_u64()
                .ok_or_else(dispatch::DispatchFailure::internal)?,
        )
        .map_err(|_| dispatch::DispatchFailure::internal())?;
        let rows = u16::try_from(
            arguments["rows"]
                .as_u64()
                .ok_or_else(dispatch::DispatchFailure::internal)?,
        )
        .map_err(|_| dispatch::DispatchFailure::internal())?;
        self.action(arguments, None, Some((columns, rows)), cancellation)
            .await
    }

    #[allow(
        clippy::too_many_lines,
        reason = "atomic and retained controller actions share one exact correlation path"
    )]
    async fn action(
        &self,
        arguments: &Value,
        input: Option<Vec<u8>>,
        resize: Option<(u16, u16)>,
        cancellation: &CancellationToken,
    ) -> Result<Value, dispatch::DispatchFailure> {
        let _gate = self.0.gate.lock().await;
        let splint_id = parse_splint(arguments)?;
        let incarnation = parse_incarnation(arguments)?;
        let needed = if input.is_some() {
            ControlMode::Input
        } else {
            ControlMode::Resize
        };
        let handle = arguments.get("controller_handle").and_then(Value::as_str);
        let mut handled_registration = None;
        let (metadata, response) = if let Some(handle) = handle {
            require_handle(handle, "ctl")?;
            let (metadata, sender, private_id) = {
                let state = self.0.state.lock().await;
                let entry = state
                    .controllers
                    .get(handle)
                    .ok_or_else(|| state.missing_handle(handle))?;
                if entry.metadata.splint_id != splint_id
                    || entry.metadata.incarnation != incarnation
                    || !entry.metadata.modes.contains(&needed)
                {
                    return Err(invalid_handle());
                }
                (
                    entry.metadata.clone(),
                    entry.sender.clone(),
                    entry.private_id,
                )
            };
            handled_registration = Some(RegistrationGuard::new(
                &self.0,
                handle.to_owned(),
                RegisteredKind::Controller,
            ));
            let request = action_request(private_id, &metadata, input.clone(), resize)?;
            let response = match actor_request(&sender, request, cancellation, false).await {
                Ok(response) => response,
                Err(failure) => return Err(self.expired_or(handle, failure).await),
            };
            self.observe_post_commit_cancellation(cancellation).await?;
            (metadata, response)
        } else {
            let modes = vec![needed];
            let mut connection = self.connect(cancellation).await?;
            let granted = request(
                &mut connection,
                Request::AcquireControl {
                    splint_id,
                    incarnation,
                    modes: modes.clone(),
                },
                cancellation,
            )
            .await?;
            let Response::ControlGranted {
                controller_id,
                lair_id,
                dojo_id,
            } = granted
            else {
                return Err(dispatch::DispatchFailure::internal());
            };
            let metadata = Metadata {
                lair_id,
                dojo_id,
                splint_id,
                incarnation,
                modes,
            };
            let response = request(
                &mut connection,
                action_request(controller_id, &metadata, input.clone(), resize)?,
                cancellation,
            )
            .await?;
            let _ = connection
                .request_with_deadline(
                    Request::ReleaseControl { controller_id },
                    Duration::from_secs(1),
                )
                .await;
            (metadata, response)
        };
        let Response::TerminalActionAcknowledged {
            lair_id,
            dojo_id,
            splint_id: acknowledged,
            incarnation: acknowledged_incarnation,
            terminal_revision,
            ..
        } = response
        else {
            return Err(dispatch::DispatchFailure::internal());
        };
        if lair_id != metadata.lair_id
            || dojo_id != metadata.dojo_id
            || acknowledged != metadata.splint_id
            || acknowledged_incarnation != metadata.incarnation
        {
            return Err(dispatch::DispatchFailure::internal());
        }
        let revision = self.revision().await?;
        let (tool, data) = if let Some(bytes) = input {
            (
                "splinterm.input",
                json!({
                    "committed": true,
                    "terminal_revision": terminal_revision,
                    "accepted_bytes": bytes.len(),
                }),
            )
        } else {
            let (columns, rows) = resize.expect("one action kind is required");
            (
                "splinterm.resize",
                json!({
                    "committed": true,
                    "terminal_revision": terminal_revision,
                    "columns": columns,
                    "rows": rows,
                }),
            )
        };
        let output = success(tool, resource(&metadata, revision), data);
        if let Some(guard) = &mut handled_registration {
            guard.disarm();
        }
        Ok(output)
    }

    async fn observe_post_commit_cancellation(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<(), dispatch::DispatchFailure> {
        #[cfg(test)]
        {
            let hook = self.0.post_commit_hook.lock().unwrap().clone();
            if let Some((reached, resume)) = hook {
                reached.notify_one();
                resume.notified().await;
            }
        }
        // Give a cancellation notification that raced the daemon commit one
        // fair scheduling point before public handle/result publication.
        tokio::task::yield_now().await;
        if cancellation.is_cancelled() {
            Err(dispatch::DispatchFailure::new(
                "cancelled",
                "the tool call was cancelled",
                true,
            ))
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    fn install_post_commit_hook(&self) -> (Arc<Notify>, Arc<Notify>) {
        let reached = Arc::new(Notify::new());
        let resume = Arc::new(Notify::new());
        *self.0.post_commit_hook.lock().unwrap() =
            Some((Arc::clone(&reached), Arc::clone(&resume)));
        (reached, resume)
    }

    #[cfg(test)]
    fn clear_post_commit_hook(&self) {
        *self.0.post_commit_hook.lock().unwrap() = None;
    }

    async fn connect(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<Connection, dispatch::DispatchFailure> {
        let deadline = dispatch::deadline()?;
        let connect = async {
            match &self.0.socket {
                Some(socket) => Connection::connect_automation_at(socket).await,
                None => Connection::connect_automation().await,
            }
        };
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(dispatch::DispatchFailure::new("cancelled", "the tool call was cancelled", true)),
            result = tokio::time::timeout(deadline, connect) => match result {
                Ok(Ok(connection)) => Ok(connection),
                Ok(Err(error)) => Err(dispatch::map_client_error(&error)),
                Err(_) => Err(dispatch::DispatchFailure::new("timeout", "the local automation deadline elapsed", true)),
            }
        }
    }

    async fn ensure_capacity(&self) -> Result<(), dispatch::DispatchFailure> {
        if self.0.state.lock().await.count() >= MAXIMUM_CONTROL_HANDLES {
            return Err(dispatch::DispatchFailure::new(
                "resource_limit",
                "the controller handle limit was reached",
                true,
            ));
        }
        Ok(())
    }

    async fn revision(&self) -> Result<u64, dispatch::DispatchFailure> {
        self.0.state.lock().await.revision()
    }

    /// A controller actor that ended between a handle's lookup and its
    /// request leaves only the generic unavailable failure; when it ended
    /// because its daemon connection did, the handle expired instead. The
    /// actor records the expiry before its queue closes.
    async fn expired_or(
        &self,
        handle: &str,
        failure: dispatch::DispatchFailure,
    ) -> dispatch::DispatchFailure {
        if failure.message == stale_handle().message
            && self
                .0
                .state
                .lock()
                .await
                .expired
                .iter()
                .any(|expired| expired == handle)
        {
            expired_handle()
        } else {
            failure
        }
    }

    async fn insert_controller(
        &self,
        connection: Connection,
        private_id: u64,
        metadata: Metadata,
    ) -> Result<String, dispatch::DispatchFailure> {
        let mut state = self.0.state.lock().await;
        if state.count() >= MAXIMUM_CONTROL_HANDLES {
            return Err(dispatch::DispatchFailure::resource_limit());
        }
        let token = state.token("ctl")?;
        let (sender, receiver) = mpsc::channel(4);
        let weak = Arc::downgrade(&self.0);
        let actor_token = token.clone();
        let task = tokio::spawn(controller_actor(
            connection,
            private_id,
            receiver,
            weak,
            actor_token,
        ));
        state.controllers.insert(
            token.clone(),
            ControllerEntry {
                metadata,
                private_id,
                sender,
                task,
            },
        );
        Ok(token)
    }

    async fn insert_transfer(
        &self,
        connection: Connection,
        private_id: u64,
        owner_handle: String,
        metadata: Metadata,
    ) -> Result<String, dispatch::DispatchFailure> {
        let mut state = self.0.state.lock().await;
        if state.count() >= MAXIMUM_CONTROL_HANDLES {
            return Err(dispatch::DispatchFailure::resource_limit());
        }
        let token = state.token("xfer")?;
        let (sender, receiver) = mpsc::channel(2);
        let weak = Arc::downgrade(&self.0);
        let actor_token = token.clone();
        let task = tokio::spawn(transfer_actor(connection, receiver, weak, actor_token));
        state.transfers.insert(
            token.clone(),
            TransferEntry {
                metadata,
                private_id,
                owner_handle,
                sender,
                task,
            },
        );
        Ok(token)
    }

    async fn publish_modes(&self, metadata: &Metadata) {
        self.0
            .resources
            .set_control_modes(
                metadata.splint_id,
                metadata.incarnation,
                mode_names(&metadata.modes),
            )
            .await;
    }

    async fn clear_modes_if_unowned(&self, metadata: &Metadata) {
        let owned = self.0.state.lock().await.controllers.values().any(|entry| {
            entry.metadata.splint_id == metadata.splint_id
                && entry.metadata.incarnation == metadata.incarnation
        });
        if !owned {
            self.0
                .resources
                .set_control_modes(metadata.splint_id, metadata.incarnation, Vec::new())
                .await;
        }
    }

    pub(crate) async fn shutdown(&self) {
        self.0.shutdown.cancel();
        let _gate = self.0.gate.lock().await;
        let (controllers, transfers) = {
            let mut state = self.0.state.lock().await;
            (
                state
                    .controllers
                    .drain()
                    .map(|(_, entry)| entry)
                    .collect::<Vec<_>>(),
                state
                    .transfers
                    .drain()
                    .map(|(_, entry)| entry)
                    .collect::<Vec<_>>(),
            )
        };
        for entry in controllers {
            let metadata = entry.metadata.clone();
            shutdown_controller(entry).await;
            self.0
                .resources
                .set_control_modes(metadata.splint_id, metadata.incarnation, Vec::new())
                .await;
        }
        for entry in transfers {
            shutdown_transfer(entry).await;
        }
    }
}

async fn request(
    connection: &mut Connection,
    request: Request,
    cancellation: &CancellationToken,
) -> Result<Response, dispatch::DispatchFailure> {
    connection
        .request_with_cancellation(request, dispatch::deadline()?, cancellation)
        .await
        .map_err(|error| dispatch::map_client_error(&error))
}

async fn controller_actor(
    mut connection: Connection,
    private_id: u64,
    mut commands: mpsc::Receiver<ControllerCommand>,
    inner: Weak<Inner>,
    token: String,
) {
    // Set when the daemon connection itself ended, as when splinterd is
    // upgraded: the daemon released this controller, and the handle expires.
    let mut lost = false;
    loop {
        tokio::select! {
            biased;
            command = commands.recv() => match command {
                Some(ControllerCommand::Request { request, cancellation, terminate, reply }) => {
                    let result = match connection
                        .request_with_cancellation(request, dispatch::deadline().unwrap_or(Duration::from_secs(5)), &cancellation)
                        .await
                    {
                        Err(error) if is_connection_loss(&error) => {
                            lost = true;
                            Err(expired_during_call())
                        }
                        result => result.map_err(|error| dispatch::map_client_error(&error)),
                    };
                    let failed = result.is_err();
                    let delivered = reply.send(result).is_ok();
                    if failed || terminate || !delivered { break; }
                }
                Some(ControllerCommand::Shutdown { reply }) => {
                    let _ = connection.request_with_deadline(Request::ReleaseControl { controller_id: private_id }, Duration::from_secs(1)).await;
                    let _ = reply.send(());
                    break;
                }
                None => break,
            },
            frame = connection.next_server_frame() => {
                lost = frame.as_ref().is_err_and(is_connection_loss);
                break;
            }
        }
    }
    cleanup_controller(inner, &token, lost).await;
}

async fn transfer_actor(
    mut connection: Connection,
    mut commands: mpsc::Receiver<TransferCommand>,
    inner: Weak<Inner>,
    token: String,
) {
    tokio::select! {
        biased;
        command = commands.recv() => match command {
            Some(TransferCommand::Take { reply }) => { let _ = reply.send(Some(connection)); }
            Some(TransferCommand::Shutdown { reply }) => { let _ = reply.send(()); }
            None => {}
        },
        () = tokio::time::sleep(TRANSFER_LIFETIME) => {},
        frame = connection.next_server_frame() => {
            // A transfer whose connection ended with the daemon expires.
            if frame.as_ref().is_err_and(is_connection_loss) {
                return cleanup_transfer(inner, &token, true).await;
            }
        },
    }
    cleanup_transfer(inner, &token, false).await;
}

/// Drops an ended controller actor's handle; `expired` records that its
/// daemon connection ended, so a later use of the handle says why.
async fn cleanup_controller(inner: Weak<Inner>, token: &str, expired: bool) {
    let Some(inner) = inner.upgrade() else {
        return;
    };
    let metadata = {
        let mut state = inner.state.lock().await;
        if expired {
            state.expire(token);
        }
        state.controllers.remove(token).map(|entry| entry.metadata)
    };
    let Some(metadata) = metadata else {
        return;
    };
    let still_owned = inner.state.lock().await.controllers.values().any(|entry| {
        entry.metadata.splint_id == metadata.splint_id
            && entry.metadata.incarnation == metadata.incarnation
    });
    if !still_owned {
        inner
            .resources
            .set_control_modes(metadata.splint_id, metadata.incarnation, Vec::new())
            .await;
    }
}

async fn cleanup_transfer(inner: Weak<Inner>, token: &str, expired: bool) {
    if let Some(inner) = inner.upgrade() {
        let mut state = inner.state.lock().await;
        if expired {
            state.expire(token);
        }
        state.transfers.remove(token);
    }
}

async fn clear_modes_if_unowned(inner: Weak<Inner>, metadata: Metadata) {
    let Some(inner) = inner.upgrade() else {
        return;
    };
    let still_owned = inner.state.lock().await.controllers.values().any(|entry| {
        entry.metadata.splint_id == metadata.splint_id
            && entry.metadata.incarnation == metadata.incarnation
    });
    if !still_owned {
        inner
            .resources
            .set_control_modes(metadata.splint_id, metadata.incarnation, Vec::new())
            .await;
    }
}

async fn rollback_registration(inner: Weak<Inner>, token: String, kind: RegisteredKind) {
    let Some(inner) = inner.upgrade() else {
        return;
    };
    match kind {
        RegisteredKind::Controller => {
            let entry = inner.state.lock().await.controllers.remove(&token);
            let Some(entry) = entry else {
                return;
            };
            let metadata = entry.metadata.clone();
            shutdown_controller(entry).await;
            let still_owned = inner.state.lock().await.controllers.values().any(|entry| {
                entry.metadata.splint_id == metadata.splint_id
                    && entry.metadata.incarnation == metadata.incarnation
            });
            if !still_owned {
                inner
                    .resources
                    .set_control_modes(metadata.splint_id, metadata.incarnation, Vec::new())
                    .await;
            }
        }
        RegisteredKind::Transfer => {
            let entry = inner.state.lock().await.transfers.remove(&token);
            if let Some(entry) = entry {
                shutdown_transfer(entry).await;
            }
        }
    }
}

async fn actor_request(
    sender: &mpsc::Sender<ControllerCommand>,
    request: Request,
    cancellation: &CancellationToken,
    terminate: bool,
) -> Result<Response, dispatch::DispatchFailure> {
    let (reply, response) = oneshot::channel();
    sender
        .send(ControllerCommand::Request {
            request,
            cancellation: cancellation.clone(),
            terminate,
            reply,
        })
        .await
        .map_err(|_| stale_handle())?;
    response.await.map_err(|_| stale_handle())?
}

async fn take_transfer(sender: mpsc::Sender<TransferCommand>) -> Option<Connection> {
    let (reply, response) = oneshot::channel();
    sender.send(TransferCommand::Take { reply }).await.ok()?;
    response.await.ok().flatten()
}

async fn shutdown_controller(mut entry: ControllerEntry) {
    let (reply, response) = oneshot::channel();
    let _ = entry
        .sender
        .send(ControllerCommand::Shutdown { reply })
        .await;
    let _ = tokio::time::timeout(Duration::from_secs(2), response).await;
    if tokio::time::timeout(Duration::from_secs(1), &mut entry.task)
        .await
        .is_err()
    {
        entry.task.abort();
    }
}

async fn shutdown_transfer(mut entry: TransferEntry) {
    let (reply, response) = oneshot::channel();
    let _ = entry.sender.send(TransferCommand::Shutdown { reply }).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), response).await;
    if tokio::time::timeout(Duration::from_secs(1), &mut entry.task)
        .await
        .is_err()
    {
        entry.task.abort();
    }
}

fn parse_splint(arguments: &Value) -> Result<SplintId, dispatch::DispatchFailure> {
    arguments["splint_id"]
        .as_str()
        .and_then(|value| value.parse().ok())
        .ok_or_else(dispatch::DispatchFailure::internal)
}

fn parse_incarnation(arguments: &Value) -> Result<u64, dispatch::DispatchFailure> {
    arguments["incarnation"]
        .as_u64()
        .filter(|value| *value > 0)
        .ok_or_else(dispatch::DispatchFailure::internal)
}

fn parse_modes(arguments: &Value) -> Result<Vec<ControlMode>, dispatch::DispatchFailure> {
    let modes = arguments["modes"]
        .as_array()
        .ok_or_else(dispatch::DispatchFailure::internal)?
        .iter()
        .map(|mode| match mode.as_str() {
            Some("input") => Ok(ControlMode::Input),
            Some("resize") => Ok(ControlMode::Resize),
            _ => Err(dispatch::DispatchFailure::internal()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate_control_modes(&modes).map_err(|_| dispatch::DispatchFailure::internal())?;
    Ok(modes)
}

fn mode_names(modes: &[ControlMode]) -> Vec<String> {
    [ControlMode::Input, ControlMode::Resize]
        .into_iter()
        .filter(|mode| modes.contains(mode))
        .map(|mode| match mode {
            ControlMode::Input => "input".to_owned(),
            ControlMode::Resize => "resize".to_owned(),
        })
        .collect()
}

fn action_request(
    controller_id: u64,
    metadata: &Metadata,
    input: Option<Vec<u8>>,
    resize: Option<(u16, u16)>,
) -> Result<Request, dispatch::DispatchFailure> {
    match (input, resize) {
        (Some(bytes), None) => Ok(Request::Input {
            controller_id,
            splint_id: metadata.splint_id,
            incarnation: metadata.incarnation,
            bytes,
            focus_report: false,
        }),
        (None, Some((columns, rows))) => Ok(Request::Resize {
            controller_id,
            splint_id: metadata.splint_id,
            incarnation: metadata.incarnation,
            columns,
            rows,
            pixel_width: 0,
            pixel_height: 0,
        }),
        _ => Err(dispatch::DispatchFailure::internal()),
    }
}

fn resource(metadata: &Metadata, revision: u64) -> Value {
    json!({
        "kind": "control",
        "lair_id": metadata.lair_id.to_string(),
        "dojo_id": metadata.dojo_id.to_string(),
        "splint_id": metadata.splint_id.to_string(),
        "incarnation": metadata.incarnation,
        "control_revision": revision,
    })
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "closed output components move conceptually into the serialized envelope"
)]
fn success(tool: &str, resource: Value, data: Value) -> Value {
    json!({
        "schema": SCHEMA,
        "tool": tool,
        "ok": true,
        "resource": resource,
        "data": data,
        "truncated": false,
        "content_trust": "trusted_metadata",
    })
}

fn require_handle(handle: &str, prefix: &str) -> Result<(), dispatch::DispatchFailure> {
    let valid = handle.len() == prefix.len() + 1 + 48
        && handle.starts_with(&format!("{prefix}_"))
        && handle[prefix.len() + 1..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
    if valid { Ok(()) } else { Err(invalid_handle()) }
}

fn invalid_handle() -> dispatch::DispatchFailure {
    dispatch::DispatchFailure::new(
        "invalid_argument",
        "the controller handle is invalid",
        false,
    )
}

/// A handle whose daemon connection ended, as when splinterd is upgraded:
/// the daemon released its authority with the connection (Plan 0061 M3.12b).
/// Stable Splint, Dojo, and Lair IDs stay valid; control is acquired again
/// the normal way, with consent unchanged.
fn expired_handle() -> dispatch::DispatchFailure {
    dispatch::DispatchFailure::new(
        "controller_unavailable",
        "the handle expired because its splinterd connection ended, as when splinterd is upgraded or restarted; acquire control again (consent rules are unchanged)",
        false,
    )
}

/// A handle whose daemon connection ended while one of its calls was in
/// flight: whether that call took effect is unknown, so an agent must look
/// before acquiring control and sending it again (Plan 0061 M3.12b).
fn expired_during_call() -> dispatch::DispatchFailure {
    dispatch::DispatchFailure::new(
        "controller_unavailable",
        "the handle expired because its splinterd connection ended during this call; it was not retried and may or may not have taken effect, so check the terminal before acquiring control again",
        false,
    )
}

fn stale_handle() -> dispatch::DispatchFailure {
    dispatch::DispatchFailure::new(
        "controller_unavailable",
        "the controller handle is unavailable",
        true,
    )
}

/// A transfer request or takeover refused as unauthorized says which grant
/// it needs (Plan 0067 M4); any other failure passes through.
fn needs_takeover_grant(failure: dispatch::DispatchFailure) -> dispatch::DispatchFailure {
    if failure.code == "unauthorized" {
        dispatch::DispatchFailure::new("unauthorized", TAKEOVER_NEEDS_GRANT, false)
    } else {
        failure
    }
}

/// The failure for a requested transfer that ended without control.
fn unanswered_transfer(outcome: ControlTransferOutcome) -> dispatch::DispatchFailure {
    match outcome {
        ControlTransferOutcome::Denied => {
            dispatch::DispatchFailure::new("control_transfer_unavailable", TRANSFER_DENIED, true)
        }
        ControlTransferOutcome::TimedOut => transfer_timed_out(),
        // A grant without a controller ends like a cancel.
        ControlTransferOutcome::Cancelled | ControlTransferOutcome::Granted => {
            dispatch::DispatchFailure::new("control_transfer_unavailable", TRANSFER_CANCELLED, true)
        }
    }
}

fn transfer_timed_out() -> dispatch::DispatchFailure {
    dispatch::DispatchFailure::new("timeout", TRANSFER_TIMED_OUT, true)
}

/// The failure for a requested transfer whose connection failed while it
/// waited `waited`. splinterd ends an unanswered transfer by closing the
/// requester's connection, so a close once its timer has run is that
/// timeout, not a lost connection.
fn ended_transfer_failure(error: &anyhow::Error, waited: Duration) -> dispatch::DispatchFailure {
    if is_connection_loss(error) && waited >= DAEMON_TRANSFER_TIMEOUT {
        transfer_timed_out()
    } else {
        dispatch::map_client_error(error)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        os::unix::net::{UnixListener, UnixStream},
        path::{Path, PathBuf},
        thread,
        time::{SystemTime, UNIX_EPOCH},
    };

    use splinterm_protocol::{
        ClientFrame, ClientRole, ErrorCode, ProtocolError, ServerFrame, ServerLimits, encode_frame,
    };

    use super::*;

    fn socket(label: &str) -> (PathBuf, PathBuf) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("mc-{label:.12}-{}-{nonce:x}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let socket = directory.join("daemon.sock");
        (directory, socket)
    }

    fn read_frame<T: serde::de::DeserializeOwned>(stream: &mut impl Read) -> T {
        let mut length = [0_u8; 4];
        stream.read_exact(&mut length).unwrap();
        let mut body = vec![0_u8; u32::from_be_bytes(length) as usize];
        stream.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[track_caller]
    fn write_frame(stream: &mut impl Write, frame: &ServerFrame) {
        stream.write_all(&encode_frame(frame).unwrap()).unwrap();
        stream.flush().unwrap();
    }

    fn accept(listener: &UnixListener) -> UnixStream {
        accept_at(listener, 1)
    }

    /// Accepts one automation connection for a daemon serving `generation`.
    fn accept_at(listener: &UnixListener, generation: u64) -> UnixStream {
        let (mut stream, _) = listener.accept().unwrap();
        assert!(matches!(
            read_frame::<ClientFrame>(&mut stream),
            ClientFrame::Hello {
                role: ClientRole::Automation,
                ..
            }
        ));
        write_frame(
            &mut stream,
            &ServerFrame::Hello {
                version: splinterm_protocol::PROTOCOL_VERSION,
                limits: ServerLimits::default(),
                development_terminal_access: false,
                daemon_hostname: None,
                daemon_generation: generation,
            },
        );
        stream
    }

    fn ids() -> (LairId, DojoId, SplintId) {
        (
            "018f4d8c-2a18-4b31-8c2f-9e7c5de77101".parse().unwrap(),
            "018f4d8c-2a18-4b31-8c2f-9e7c5de77102".parse().unwrap(),
            "018f4d8c-2a18-4b31-8c2f-9e7c5de77103".parse().unwrap(),
        )
    }

    async fn join(thread: thread::JoinHandle<()>) {
        tokio::task::spawn_blocking(move || thread.join().unwrap())
            .await
            .unwrap();
    }

    /// Waits, bounded, until the registry recorded `handle` as expired.
    async fn wait_until_expired(registry: &ControlRegistry, handle: &str) {
        for _ in 0..200 {
            if registry
                .0
                .state
                .lock()
                .await
                .expired
                .iter()
                .any(|expired| expired == handle)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the handle never expired");
    }

    /// Plan 0061 M3.12b: a committed upgrade ends the controller's daemon
    /// connection, so its handle expires with a clear error, and control is
    /// acquired again the normal way from the daemon serving the next
    /// generation. The Splint's stable ID is unchanged throughout.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(
        clippy::too_many_lines,
        reason = "one ordered fake daemon serves both generations and the shutdown release"
    )]
    async fn handles_expire_across_an_upgrade_and_control_is_acquired_again() {
        let (directory, socket) = socket("upgrade-expiry");
        let listener = UnixListener::bind(&socket).unwrap();
        let (lair_id, dojo_id, splint_id) = ids();
        let daemon = thread::spawn(move || {
            let mut old = accept_at(&listener, 1);
            let ClientFrame::Request {
                request_id,
                request: Request::AcquireControl { .. },
                ..
            } = read_frame(&mut old)
            else {
                panic!("expected the first acquisition")
            };
            write_frame(
                &mut old,
                &ServerFrame::Response {
                    request_id,
                    result: Response::ControlGranted {
                        controller_id: 5,
                        lair_id,
                        dojo_id,
                    },
                },
            );
            // The daemon execs into the next generation: the connection ends.
            drop(old);
            let mut new = accept_at(&listener, 2);
            let ClientFrame::Request {
                request_id,
                request:
                    Request::AcquireControl {
                        splint_id: acquired,
                        ..
                    },
                ..
            } = read_frame(&mut new)
            else {
                panic!("expected the acquisition from the upgraded daemon")
            };
            assert_eq!(acquired, splint_id, "stable IDs stay valid");
            write_frame(
                &mut new,
                &ServerFrame::Response {
                    request_id,
                    result: Response::ControlGranted {
                        controller_id: 6,
                        lair_id,
                        dojo_id,
                    },
                },
            );
            let ClientFrame::Request {
                request_id,
                request: Request::ReleaseControl { controller_id: 6 },
                ..
            } = read_frame(&mut new)
            else {
                panic!("expected the new controller's release at shutdown")
            };
            write_frame(
                &mut new,
                &ServerFrame::Response {
                    request_id,
                    result: Response::Acknowledged,
                },
            );
        });

        let resources = Arc::new(ResourceRegistry::default());
        let registry = ControlRegistry::new_at(Arc::clone(&resources), &socket);
        let acquire = json!({
            "splint_id": splint_id.to_string(),
            "incarnation": 2,
            "modes": ["input"]
        });
        let first = registry
            .dispatch(
                "splinterm.acquire_control",
                &acquire,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        let old_handle = first["data"]["controller_handle"]
            .as_str()
            .unwrap()
            .to_owned();
        let input = json!({
            "splint_id": splint_id.to_string(),
            "incarnation": 2,
            "text": "ls\n",
            "controller_handle": old_handle,
        });
        // The controller's actor sees the connection end on its own.
        wait_until_expired(&registry, &old_handle).await;
        // Used afterwards, the handle has expired; asking again gives the
        // same answer.
        for _ in 0..2 {
            let failure = registry
                .dispatch("splinterm.input", &input, &CancellationToken::new())
                .await
                .unwrap_err();
            assert_eq!(failure.code, "controller_unavailable");
            assert!(!failure.retryable, "the same handle never works again");
            assert!(failure.message.contains("expired"), "{}", failure.message);
            assert!(failure.message.contains("upgraded"), "{}", failure.message);
            assert!(
                failure.message.contains("acquire control again"),
                "{}",
                failure.message
            );
        }
        let second = registry
            .dispatch(
                "splinterm.acquire_control",
                &acquire,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        let new_handle = second["data"]["controller_handle"].as_str().unwrap();
        assert_ne!(new_handle, old_handle);
        assert_eq!(second["resource"]["splint_id"], splint_id.to_string());
        registry.shutdown().await;
        resources.shutdown().await;
        join(daemon).await;
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// Plan 0061 M3.12b review S1: a call whose connection ends while it is
    /// in flight may have taken effect, so it says so rather than inviting a
    /// plain re-acquisition that could type the same input twice. Used
    /// again, the handle reports the plain expiry.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_call_in_flight_when_the_connection_ends_may_have_taken_effect() {
        let (directory, socket) = socket("in-flight-loss");
        let listener = UnixListener::bind(&socket).unwrap();
        let (lair_id, dojo_id, splint_id) = ids();
        let daemon = thread::spawn(move || {
            let mut stream = accept(&listener);
            let ClientFrame::Request {
                request_id,
                request: Request::AcquireControl { .. },
                ..
            } = read_frame(&mut stream)
            else {
                panic!("expected the acquisition")
            };
            write_frame(
                &mut stream,
                &ServerFrame::Response {
                    request_id,
                    result: Response::ControlGranted {
                        controller_id: 5,
                        lair_id,
                        dojo_id,
                    },
                },
            );
            let ClientFrame::Request {
                request: Request::Input {
                    controller_id: 5, ..
                },
                ..
            } = read_frame(&mut stream)
            else {
                panic!("expected the input request")
            };
            // The daemon execs with the input read but unanswered.
            drop(stream);
        });
        let resources = Arc::new(ResourceRegistry::default());
        let registry = ControlRegistry::new_at(Arc::clone(&resources), &socket);
        let acquired = registry
            .dispatch(
                "splinterm.acquire_control",
                &json!({
                    "splint_id": splint_id.to_string(),
                    "incarnation": 2,
                    "modes": ["input"]
                }),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        let handle = acquired["data"]["controller_handle"]
            .as_str()
            .unwrap()
            .to_owned();
        let input = json!({
            "splint_id": splint_id.to_string(),
            "incarnation": 2,
            "text": "make deploy\n",
            "controller_handle": handle,
        });
        let failure = registry
            .dispatch("splinterm.input", &input, &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(failure.code, "controller_unavailable");
        assert!(!failure.retryable);
        assert_eq!(failure.message, expired_during_call().message);
        assert!(
            failure.message.contains("may or may not have taken effect"),
            "{}",
            failure.message
        );
        let failure = registry
            .dispatch("splinterm.input", &input, &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(failure.message, expired_handle().message);
        registry.shutdown().await;
        resources.shutdown().await;
        join(daemon).await;
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn only_a_handle_lost_with_its_connection_is_called_expired() {
        let mut state = State::default();
        let unknown = "ctl_00000000000000010000000000000000ffffffffffffffff";
        assert_eq!(
            state.missing_handle(unknown).message,
            invalid_handle().message
        );
        state.expire(unknown);
        assert_eq!(
            state.missing_handle(unknown).message,
            expired_handle().message
        );
        // The memory is bounded; the oldest expiry is forgotten first.
        for index in 0..MAXIMUM_EXPIRED_HANDLES {
            state.expire(&format!("ctl_{index:048x}"));
        }
        assert_eq!(state.expired.len(), MAXIMUM_EXPIRED_HANDLES);
        assert_eq!(
            state.missing_handle(unknown).message,
            invalid_handle().message
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(
        clippy::too_many_lines,
        reason = "one ordered fake daemon proves both post-registration cancellation boundaries"
    )]
    async fn cancellation_after_acquire_and_transfer_registration_rolls_back_authority() {
        let (directory, socket) = socket("registration-cancel");
        let listener = UnixListener::bind(&socket).unwrap();
        let (lair_id, dojo_id, splint_id) = ids();
        let (transfer_closed_tx, transfer_closed_rx) = oneshot::channel();
        let daemon = thread::spawn(move || {
            let mut cancelled_acquire = accept(&listener);
            let ClientFrame::Request {
                request_id,
                request: Request::AcquireControl { .. },
                ..
            } = read_frame(&mut cancelled_acquire)
            else {
                panic!("expected cancelled acquisition")
            };
            write_frame(
                &mut cancelled_acquire,
                &ServerFrame::Response {
                    request_id,
                    result: Response::ControlGranted {
                        controller_id: 10,
                        lair_id,
                        dojo_id,
                    },
                },
            );
            let ClientFrame::Request {
                request_id,
                request: Request::ReleaseControl { controller_id: 10 },
                ..
            } = read_frame(&mut cancelled_acquire)
            else {
                panic!("expected cancelled acquisition cleanup")
            };
            write_frame(
                &mut cancelled_acquire,
                &ServerFrame::Response {
                    request_id,
                    result: Response::Acknowledged,
                },
            );

            let mut owner = accept(&listener);
            let ClientFrame::Request {
                request_id,
                request: Request::AcquireControl { .. },
                ..
            } = read_frame(&mut owner)
            else {
                panic!("expected owner acquisition")
            };
            write_frame(
                &mut owner,
                &ServerFrame::Response {
                    request_id,
                    result: Response::ControlGranted {
                        controller_id: 20,
                        lair_id,
                        dojo_id,
                    },
                },
            );
            let mut requester = accept(&listener);
            let ClientFrame::Request {
                request_id,
                request: Request::RequestControlTransfer { .. },
                ..
            } = read_frame(&mut requester)
            else {
                panic!("expected cancelled transfer")
            };
            write_frame(
                &mut requester,
                &ServerFrame::Response {
                    request_id,
                    result: Response::ControlTransferPending {
                        transfer_id: 30,
                        lair_id,
                        dojo_id,
                    },
                },
            );
            let mut eof = [0_u8; 1];
            assert_eq!(requester.read(&mut eof).unwrap(), 0);
            transfer_closed_tx.send(()).unwrap();
            let ClientFrame::Request {
                request_id,
                request: Request::ReleaseControl { controller_id: 20 },
                ..
            } = read_frame(&mut owner)
            else {
                panic!("expected owner shutdown")
            };
            write_frame(
                &mut owner,
                &ServerFrame::Response {
                    request_id,
                    result: Response::Acknowledged,
                },
            );
        });

        let resources = Arc::new(ResourceRegistry::default());
        let registry = ControlRegistry::new_at(Arc::clone(&resources), Path::new(&socket));
        let acquire_args = json!({
            "splint_id": splint_id.to_string(), "incarnation": 2, "modes": ["input"]
        });
        let (reached, resume) = registry.install_post_commit_hook();
        let cancellation = CancellationToken::new();
        let task_registry = registry.clone();
        let task_cancel = cancellation.clone();
        let acquire_task = tokio::spawn(async move {
            task_registry
                .dispatch("splinterm.acquire_control", &acquire_args, &task_cancel)
                .await
        });
        reached.notified().await;
        cancellation.cancel();
        resume.notify_one();
        let failure = acquire_task.await.unwrap().unwrap_err();
        assert_eq!(failure.code, "cancelled");
        registry.clear_post_commit_hook();

        let owner = registry
            .dispatch(
                "splinterm.acquire_control",
                &json!({"splint_id":splint_id.to_string(),"incarnation":2,"modes":["input"]}),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(owner["data"]["controller_handle"].as_str().is_some());

        let (reached, resume) = registry.install_post_commit_hook();
        let cancellation = CancellationToken::new();
        let task_registry = registry.clone();
        let task_cancel = cancellation.clone();
        let transfer_task = tokio::spawn(async move {
            task_registry
                .dispatch(
                    "splinterm.request_control_transfer",
                    &json!({"splint_id":splint_id.to_string(),"incarnation":2,"modes":["input"]}),
                    &task_cancel,
                )
                .await
        });
        reached.notified().await;
        cancellation.cancel();
        resume.notify_one();
        let failure = transfer_task.await.unwrap().unwrap_err();
        assert_eq!(failure.code, "cancelled");
        registry.clear_post_commit_hook();

        // Rollback is scheduled asynchronously. Prove that cancellation closes
        // the transfer before global shutdown can drain it behind the owner,
        // whose release acknowledgment the fake daemon sends only after EOF.
        tokio::time::timeout(Duration::from_secs(2), transfer_closed_rx)
            .await
            .expect("cancelled transfer must close before registry shutdown")
            .unwrap();
        registry.shutdown().await;
        resources.shutdown().await;
        join(daemon).await;
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the shared fake-daemon helper proves committed input and resize cleanup"
    )]
    async fn cancelled_handled_action(tool: &'static str) {
        let (directory, socket) = socket(tool);
        let listener = UnixListener::bind(&socket).unwrap();
        let (lair_id, dojo_id, splint_id) = ids();
        let daemon = thread::spawn(move || {
            let mut owner = accept(&listener);
            let ClientFrame::Request {
                request_id,
                request: Request::AcquireControl { .. },
                ..
            } = read_frame(&mut owner)
            else {
                panic!("expected owner acquisition")
            };
            write_frame(
                &mut owner,
                &ServerFrame::Response {
                    request_id,
                    result: Response::ControlGranted {
                        controller_id: 40,
                        lair_id,
                        dojo_id,
                    },
                },
            );
            let ClientFrame::Request {
                request_id,
                request,
                ..
            } = read_frame(&mut owner)
            else {
                panic!("expected handled action")
            };
            assert!(matches!(
                (&request, tool),
                (Request::Input { .. }, "splinterm.input")
                    | (Request::Resize { .. }, "splinterm.resize")
            ));
            write_frame(
                &mut owner,
                &ServerFrame::Response {
                    request_id,
                    result: Response::TerminalActionAcknowledged {
                        lair_id,
                        dojo_id,
                        splint_id,
                        incarnation: 2,
                        terminal_revision: 9,
                        history_generation: 3,
                    },
                },
            );
            let ClientFrame::Request {
                request_id,
                request: Request::ReleaseControl { controller_id: 40 },
                ..
            } = read_frame(&mut owner)
            else {
                panic!("expected cancelled handled-action cleanup")
            };
            write_frame(
                &mut owner,
                &ServerFrame::Response {
                    request_id,
                    result: Response::Acknowledged,
                },
            );
        });

        let resources = Arc::new(ResourceRegistry::default());
        let registry = ControlRegistry::new_at(Arc::clone(&resources), &socket);
        let acquired = registry
            .dispatch(
                "splinterm.acquire_control",
                &json!({"splint_id":splint_id.to_string(),"incarnation":2,"modes":[if tool == "splinterm.input" {"input"} else {"resize"}]}),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        let handle = acquired["data"]["controller_handle"]
            .as_str()
            .unwrap()
            .to_owned();
        let arguments = if tool == "splinterm.input" {
            json!({"splint_id":splint_id.to_string(),"incarnation":2,"text":"committed","controller_handle":handle})
        } else {
            json!({"splint_id":splint_id.to_string(),"incarnation":2,"columns":80,"rows":24,"controller_handle":handle})
        };
        let (reached, resume) = registry.install_post_commit_hook();
        let cancellation = CancellationToken::new();
        let task_registry = registry.clone();
        let task_cancel = cancellation.clone();
        let task =
            tokio::spawn(
                async move { task_registry.dispatch(tool, &arguments, &task_cancel).await },
            );
        reached.notified().await;
        cancellation.cancel();
        resume.notify_one();
        let failure = task.await.unwrap().unwrap_err();
        assert_eq!(failure.code, "cancelled");
        registry.clear_post_commit_hook();
        registry.shutdown().await;
        resources.shutdown().await;
        join(daemon).await;
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn external_graphical_owner_can_grant_a_requested_transfer() {
        let (directory, socket) = socket("external-transfer");
        let listener = UnixListener::bind(&socket).unwrap();
        let (lair_id, dojo_id, splint_id) = ids();
        let daemon = thread::spawn(move || {
            let mut requester = accept(&listener);
            let ClientFrame::Request {
                request_id,
                request: Request::SubscribeControl { .. },
                ..
            } = read_frame(&mut requester)
            else {
                panic!("expected control subscription")
            };
            write_frame(
                &mut requester,
                &ServerFrame::Response {
                    request_id,
                    result: Response::ControlSubscribed {
                        subscription_id: 7,
                        status: splinterm_protocol::ControlStatus {
                            splint_id,
                            incarnation: 2,
                            controlled: true,
                            locally_owned: false,
                        },
                    },
                },
            );
            let ClientFrame::Request {
                request_id,
                request: Request::RequestControlTransfer { .. },
                ..
            } = read_frame(&mut requester)
            else {
                panic!("expected external transfer request")
            };
            write_frame(
                &mut requester,
                &ServerFrame::Response {
                    request_id,
                    result: Response::ControlTransferPending {
                        transfer_id: 9,
                        lair_id,
                        dojo_id,
                    },
                },
            );
            write_frame(
                &mut requester,
                &ServerFrame::Event {
                    subscription_id: 7,
                    sequence: 1,
                    event: SubscriptionEvent::ControlTransferResolved {
                        transfer_id: 9,
                        outcome: ControlTransferOutcome::Granted,
                        controller_id: Some(60),
                    },
                },
            );
            let ClientFrame::Request {
                request_id,
                request: Request::ReleaseControl { controller_id: 60 },
                ..
            } = read_frame(&mut requester)
            else {
                panic!("expected transferred controller cleanup")
            };
            write_frame(
                &mut requester,
                &ServerFrame::Response {
                    request_id,
                    result: Response::Acknowledged,
                },
            );
        });

        let resources = Arc::new(ResourceRegistry::default());
        let registry = ControlRegistry::new_at(Arc::clone(&resources), &socket);
        let transferred = registry
            .dispatch(
                "splinterm.request_control_transfer",
                &json!({
                    "splint_id": splint_id.to_string(),
                    "incarnation": 2,
                    "modes": ["input"]
                }),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(transferred["data"]["outcome"], "granted");
        assert!(transferred["data"]["controller_handle"].as_str().is_some());
        registry.shutdown().await;
        resources.shutdown().await;
        join(daemon).await;
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn approved_takeover_uses_the_explicit_force_transfer_request() {
        let (directory, socket) = socket("approved-takeover");
        let listener = UnixListener::bind(&socket).unwrap();
        let (lair_id, dojo_id, splint_id) = ids();
        let daemon = thread::spawn(move || {
            let mut requester = accept(&listener);
            let ClientFrame::Request {
                request_id,
                request:
                    Request::ForceControlTransfer {
                        splint_id: requested_splint,
                        incarnation: 2,
                    },
                ..
            } = read_frame(&mut requester)
            else {
                panic!("expected approved takeover request")
            };
            assert_eq!(requested_splint, splint_id);
            write_frame(
                &mut requester,
                &ServerFrame::Response {
                    request_id,
                    result: Response::ControlGranted {
                        controller_id: 50,
                        lair_id,
                        dojo_id,
                    },
                },
            );
            let ClientFrame::Request {
                request_id,
                request: Request::ReleaseControl { controller_id: 50 },
                ..
            } = read_frame(&mut requester)
            else {
                panic!("expected takeover cleanup")
            };
            write_frame(
                &mut requester,
                &ServerFrame::Response {
                    request_id,
                    result: Response::Acknowledged,
                },
            );
        });

        let resources = Arc::new(ResourceRegistry::default());
        let registry = ControlRegistry::new_at(Arc::clone(&resources), &socket);
        let acquired = registry
            .dispatch(
                "splinterm.acquire_control",
                &json!({
                    "splint_id": splint_id.to_string(),
                    "incarnation": 2,
                    "modes": ["input"],
                    "takeover": true
                }),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(acquired["data"]["controller_handle"].as_str().is_some());
        registry.shutdown().await;
        resources.shutdown().await;
        join(daemon).await;
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_handled_input_and_resize_cleanup_without_rollback_claims() {
        cancelled_handled_action("splinterm.input").await;
        cancelled_handled_action("splinterm.resize").await;
    }

    /// Plan 0067 M4: a transfer that ends without control names the
    /// takeover, the accept key, and the grant to ask for.
    #[test]
    fn an_unanswered_transfer_says_the_next_step() {
        let failure = |outcome| {
            let mapped = unanswered_transfer(outcome);
            (mapped.code, mapped.message, mapped.retryable)
        };
        assert_eq!(
            failure(ControlTransferOutcome::Denied),
            ("control_transfer_unavailable", TRANSFER_DENIED, true)
        );
        assert_eq!(
            failure(ControlTransferOutcome::Cancelled),
            ("control_transfer_unavailable", TRANSFER_CANCELLED, true)
        );
        assert_eq!(
            failure(ControlTransferOutcome::Granted),
            ("control_transfer_unavailable", TRANSFER_CANCELLED, true)
        );
        assert_eq!(
            failure(ControlTransferOutcome::TimedOut),
            ("timeout", TRANSFER_TIMED_OUT, true)
        );
        for message in [TRANSFER_CANCELLED, TRANSFER_TIMED_OUT] {
            assert!(message.contains("acquire_control with takeover: true"));
            assert!(message.contains("press Ctrl+Shift+Y"));
            assert!(message.contains("request_lair_access"));
        }
        // The person said no: no retry and no takeover unless they agree.
        assert!(TRANSFER_DENIED.starts_with("the pane's controller denied"));
        assert!(TRANSFER_DENIED.contains("do not ask again"));
        assert!(TRANSFER_DENIED.contains("unless the person tells you to"));
    }

    /// splinterd closes the requester's connection when its 15-second timer
    /// ends the transfer; an earlier close is a lost connection.
    #[test]
    fn a_connection_closed_by_the_transfer_timer_is_a_timeout() {
        let closed = || anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        let timed_out = ended_transfer_failure(&closed(), DAEMON_TRANSFER_TIMEOUT);
        assert_eq!(
            (timed_out.code, timed_out.message),
            ("timeout", TRANSFER_TIMED_OUT)
        );
        let lost = ended_transfer_failure(&closed(), Duration::from_secs(3));
        assert_eq!(
            (lost.code, lost.message),
            ("internal", dispatch::CONNECTION_LOST_DURING_CALL)
        );
        let other = ended_transfer_failure(&anyhow::anyhow!("bad frame"), Duration::from_secs(20));
        assert_eq!(other.code, "internal");
        assert_ne!(other.message, TRANSFER_TIMED_OUT);
    }

    #[test]
    fn only_an_unauthorized_takeover_names_the_grant_it_needs() {
        let unauthorized = needs_takeover_grant(dispatch::DispatchFailure::new(
            "unauthorized",
            "the exact policy scope or resource is not authorized",
            false,
        ));
        assert_eq!(unauthorized.code, "unauthorized");
        assert_eq!(unauthorized.message, TAKEOVER_NEEDS_GRANT);
        assert!(!unauthorized.retryable);
        let other = needs_takeover_grant(stale_handle());
        assert_eq!(other.message, stale_handle().message);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_denied_external_transfer_says_the_next_step() {
        let (directory, socket) = socket("denied-xfer");
        let listener = UnixListener::bind(&socket).unwrap();
        let (lair_id, dojo_id, splint_id) = ids();
        let daemon = thread::spawn(move || {
            let mut requester = accept(&listener);
            let ClientFrame::Request {
                request_id,
                request: Request::SubscribeControl { .. },
                ..
            } = read_frame(&mut requester)
            else {
                panic!("expected control subscription")
            };
            write_frame(
                &mut requester,
                &ServerFrame::Response {
                    request_id,
                    result: Response::ControlSubscribed {
                        subscription_id: 7,
                        status: splinterm_protocol::ControlStatus {
                            splint_id,
                            incarnation: 2,
                            controlled: true,
                            locally_owned: false,
                        },
                    },
                },
            );
            let ClientFrame::Request {
                request_id,
                request: Request::RequestControlTransfer { .. },
                ..
            } = read_frame(&mut requester)
            else {
                panic!("expected external transfer request")
            };
            write_frame(
                &mut requester,
                &ServerFrame::Response {
                    request_id,
                    result: Response::ControlTransferPending {
                        transfer_id: 9,
                        lair_id,
                        dojo_id,
                    },
                },
            );
            write_frame(
                &mut requester,
                &ServerFrame::Event {
                    subscription_id: 7,
                    sequence: 1,
                    event: SubscriptionEvent::ControlTransferResolved {
                        transfer_id: 9,
                        outcome: ControlTransferOutcome::Denied,
                        controller_id: None,
                    },
                },
            );
        });

        let resources = Arc::new(ResourceRegistry::default());
        let registry = ControlRegistry::new_at(Arc::clone(&resources), &socket);
        let failure = registry
            .dispatch(
                "splinterm.request_control_transfer",
                &json!({
                    "splint_id": splint_id.to_string(),
                    "incarnation": 2,
                    "modes": ["input"]
                }),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(
            (failure.code, failure.message),
            ("control_transfer_unavailable", TRANSFER_DENIED)
        );
        registry.shutdown().await;
        resources.shutdown().await;
        join(daemon).await;
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// A daemon refusal for each control request, and the adapter's answer.
    async fn refused_control(
        label: &str,
        tool: &'static str,
        takeover: bool,
        error: ProtocolError,
    ) -> dispatch::DispatchFailure {
        refused_control_at(label, tool, takeover, false, error).await
    }

    /// As [`refused_control`]; with `refuse_subscription`, the daemon
    /// refuses a transfer's control subscription, its first request.
    async fn refused_control_at(
        label: &str,
        tool: &'static str,
        takeover: bool,
        refuse_subscription: bool,
        error: ProtocolError,
    ) -> dispatch::DispatchFailure {
        let (directory, socket) = socket(label);
        let listener = UnixListener::bind(&socket).unwrap();
        let (_, _, splint_id) = ids();
        let daemon = thread::spawn(move || {
            let mut requester = accept(&listener);
            let mut frame = read_frame::<ClientFrame>(&mut requester);
            if !refuse_subscription
                && let ClientFrame::Request {
                    request_id,
                    request: Request::SubscribeControl { .. },
                    ..
                } = frame
            {
                write_frame(
                    &mut requester,
                    &ServerFrame::Response {
                        request_id,
                        result: Response::ControlSubscribed {
                            subscription_id: 7,
                            status: splinterm_protocol::ControlStatus {
                                splint_id,
                                incarnation: 2,
                                controlled: true,
                                locally_owned: false,
                            },
                        },
                    },
                );
                frame = read_frame(&mut requester);
            }
            let ClientFrame::Request { request_id, .. } = frame else {
                panic!("expected a control request")
            };
            write_frame(
                &mut requester,
                &ServerFrame::Error {
                    request_id: Some(request_id),
                    error,
                },
            );
        });

        let resources = Arc::new(ResourceRegistry::default());
        let registry = ControlRegistry::new_at(Arc::clone(&resources), &socket);
        let failure = registry
            .dispatch(
                tool,
                &json!({
                    "splint_id": splint_id.to_string(),
                    "incarnation": 2,
                    "modes": ["input"],
                    "takeover": takeover
                }),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        registry.shutdown().await;
        resources.shutdown().await;
        join(daemon).await;
        std::fs::remove_dir_all(directory).unwrap();
        failure
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn control_refusals_through_the_registry_say_the_next_step() {
        let unauthorized = || {
            ProtocolError::new(
                ErrorCode::Unauthorized,
                "no exact persistent policy rule authorizes this request",
            )
        };
        let nobody = refused_control(
            "nobody-takeover",
            "splinterm.acquire_control",
            true,
            ProtocolError::new(
                ErrorCode::ControlTransferUnavailable,
                splinterm_protocol::NO_CONTROLLER_TO_TRANSFER,
            ),
        )
        .await;
        assert_eq!(nobody.message, dispatch::NOBODY_CONTROLS);
        let controlled = refused_control(
            "controlled",
            "splinterm.acquire_control",
            false,
            ProtocolError::new(
                ErrorCode::ControllerUnavailable,
                splinterm_protocol::SPLINT_ALREADY_CONTROLLED,
            ),
        )
        .await;
        assert_eq!(controlled.message, dispatch::ALREADY_CONTROLLED);
        let pending = refused_control(
            "pending-xfer",
            "splinterm.request_control_transfer",
            false,
            ProtocolError::new(
                ErrorCode::ControlTransferUnavailable,
                splinterm_protocol::CONTROL_TRANSFER_PENDING,
            ),
        )
        .await;
        assert_eq!(pending.message, dispatch::TRANSFER_ALREADY_PENDING);
        let takeover = refused_control(
            "unauth-takeover",
            "splinterm.acquire_control",
            true,
            unauthorized(),
        )
        .await;
        assert_eq!(takeover.message, TAKEOVER_NEEDS_GRANT);
        let transfer = refused_control(
            "unauth-xfer",
            "splinterm.request_control_transfer",
            false,
            unauthorized(),
        )
        .await;
        assert_eq!(transfer.message, TAKEOVER_NEEDS_GRANT);
        // With no grant at all, the subscription a transfer starts with is
        // refused first.
        let ungranted = refused_control_at(
            "unauth-sub",
            "splinterm.request_control_transfer",
            false,
            true,
            unauthorized(),
        )
        .await;
        assert_eq!(ungranted.message, TAKEOVER_NEEDS_GRANT);
        // A plain acquire is not a takeover; its refusal stays general.
        let acquire = refused_control(
            "unauth-acquire",
            "splinterm.acquire_control",
            false,
            unauthorized(),
        )
        .await;
        assert_eq!(
            acquire.message,
            "the exact policy scope or resource is not authorized"
        );
    }
}
