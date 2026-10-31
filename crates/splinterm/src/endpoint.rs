//! Explicit local-versus-remote daemon endpoint capabilities.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::Result;
use splinterm_automation_client::Connection;
use splinterm_core::SplintId;

use crate::{
    handoff_control::HandoffControlRestore, handoff_status::HandoffStatusHub,
    remote::RemoteProfile, remote_askpass::NativePrompt, remote_session::RemoteSession,
};

/// Informational connection-time identity, not a security or authority signal.
/// Constructed only from a remote endpoint's negotiated daemon metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteDisplayIdentity {
    hostname: Option<String>,
}

impl RemoteDisplayIdentity {
    fn from_advertised(hostname: Option<&str>) -> Self {
        Self {
            hostname: hostname
                .and_then(splinterm_protocol::usable_daemon_hostname)
                .map(str::to_owned),
        }
    }

    #[must_use]
    pub fn label(&self) -> String {
        self.hostname
            .as_ref()
            .map_or_else(|| "Remote".to_owned(), |host| format!("Remote: {host}"))
    }

    /// The remote daemon's self-reported hostname, as
    /// [`splinterm_protocol::usable_daemon_hostname`] accepts it: display
    /// only, never an identity.
    #[must_use]
    pub fn hostname(&self) -> Option<&str> {
        self.hostname.as_deref()
    }
}

/// Whether terminal image bodies may be retrieved for an endpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageTransport {
    LocalTrusted,
    Unavailable,
}

/// How graphical compositor focus may be published to the daemon.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphicalFocusPublication {
    Enabled,
    Disabled,
}

/// Which request family must be used to create processes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LaunchSemantics {
    LocalTrusted,
    RemoteInteractive,
}

/// Whether this Window may force control of a pane (Ctrl+Shift+U). A local
/// Window confirms in the daemon's own dialog; a remote one in its own
/// dialog, through its consent responder (Plan 0072 M3).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForcedControlTransfer {
    Enabled,
    Disabled,
}

/// Whether a Window of this endpoint answers the daemon's agent access
/// prompts (ADR 0015).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemoteConsent {
    /// A remote Window shows the prompts its daemon offers, labelled with
    /// this SSH profile name as their origin; the viewer's
    /// `[integration] remote-agent-prompts` can still turn it off.
    Enabled { profile: String },
    /// The local daemon shows its own prompts; nothing subscribes.
    Disabled,
}

/// Stable behavior carried with every connection factory clone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EndpointCapabilities {
    pub image_transport: ImageTransport,
    pub graphical_focus_publication: GraphicalFocusPublication,
    pub launch_semantics: LaunchSemantics,
    pub forced_control_transfer: ForcedControlTransfer,
    pub recency_namespace: String,
    pub remote_consent: RemoteConsent,
}

#[derive(Debug)]
enum EndpointKind {
    Local,
    Remote(RemoteSession),
}

/// A cloneable factory bound to exactly one local or authenticated remote endpoint.
#[derive(Clone, Debug)]
pub struct ConnectionFactory {
    endpoint: Arc<EndpointKind>,
    capabilities: Arc<EndpointCapabilities>,
    /// The daemon handoff status every connection of this endpoint session
    /// reports to: one per Window, shared by its panes and its frontend
    /// (Plan 0061 M3.12a).
    handoff_status: HandoffStatusHub,
    /// A relaunched Window's resume ticket for its panes' controller claims
    /// (Plan 0061 M3.16); never armed for any other Window or a remote
    /// endpoint.
    handoff_control: HandoffControlRestore,
    /// The forced takeovers this Window's panes wait on (Plan 0072 M3).
    forces: ForcesInFlight,
}

/// The forced takeovers a Window's panes wait on now (Plan 0072 M3), shared
/// by its factory's clones. A remote Window shows a forced-control
/// confirmation only for a Splint one of its own panes is forcing, so its
/// daemon cannot make it show one nobody asked for.
#[derive(Clone, Debug, Default)]
pub struct ForcesInFlight(Arc<Mutex<Vec<(SplintId, u64)>>>);

impl ForcesInFlight {
    /// Records a force of `splint_id` at `incarnation` until the returned
    /// guard drops.
    #[must_use]
    pub fn begin(&self, splint_id: SplintId, incarnation: u64) -> ForceInFlight {
        self.entries().push((splint_id, incarnation));
        ForceInFlight {
            forces: self.clone(),
            key: (splint_id, incarnation),
        }
    }

    /// Whether a pane forces `splint_id` at `incarnation` now.
    #[must_use]
    pub fn contains(&self, splint_id: SplintId, incarnation: u64) -> bool {
        self.entries().contains(&(splint_id, incarnation))
    }

    fn entries(&self) -> MutexGuard<'_, Vec<(SplintId, u64)>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One pane's force in flight; dropping it ends it.
#[derive(Debug)]
pub struct ForceInFlight {
    forces: ForcesInFlight,
    key: (SplintId, u64),
}

impl Drop for ForceInFlight {
    fn drop(&mut self) {
        let mut entries = self.forces.entries();
        if let Some(index) = entries.iter().position(|entry| *entry == self.key) {
            entries.swap_remove(index);
        }
    }
}

impl ConnectionFactory {
    /// Creates the unchanged trusted local Unix-socket endpoint.
    #[must_use]
    pub fn local() -> Self {
        Self {
            endpoint: Arc::new(EndpointKind::Local),
            capabilities: Arc::new(EndpointCapabilities {
                image_transport: ImageTransport::LocalTrusted,
                graphical_focus_publication: GraphicalFocusPublication::Enabled,
                launch_semantics: LaunchSemantics::LocalTrusted,
                forced_control_transfer: ForcedControlTransfer::Enabled,
                recency_namespace: "local".to_owned(),
                remote_consent: RemoteConsent::Disabled,
            }),
            handoff_status: HandoffStatusHub::default(),
            handoff_control: HandoffControlRestore::default(),
            forces: ForcesInFlight::default(),
        }
    }

    /// Authenticates one remote profile and binds all factory clones to it.
    ///
    /// # Errors
    ///
    /// Returns an error when OpenSSH or graphical-relay negotiation fails.
    pub async fn remote(profile: &RemoteProfile) -> Result<Self> {
        let session = RemoteSession::connect(profile).await?;
        Ok(Self::from_remote_session(profile.name(), session))
    }

    /// Like [`Self::remote`], but asks through `prompt` (the status Window)
    /// when OpenSSH needs input and no terminal or `SSH_ASKPASS` can answer.
    ///
    /// # Errors
    ///
    /// Returns an error when OpenSSH or graphical-relay negotiation fails.
    pub async fn remote_with_prompt(
        profile: &RemoteProfile,
        prompt: &NativePrompt,
    ) -> Result<Self> {
        let session = RemoteSession::connect_with_prompt(profile, prompt).await?;
        Ok(Self::from_remote_session(profile.name(), session))
    }

    #[doc(hidden)]
    #[must_use]
    pub fn from_remote_session(profile_name: &str, session: RemoteSession) -> Self {
        Self {
            endpoint: Arc::new(EndpointKind::Remote(session)),
            capabilities: Arc::new(EndpointCapabilities {
                image_transport: ImageTransport::Unavailable,
                graphical_focus_publication: GraphicalFocusPublication::Disabled,
                launch_semantics: LaunchSemantics::RemoteInteractive,
                // Plan 0072 M3: confirmed in this Window's own dialog, through
                // its consent responder (ADR 0015).
                forced_control_transfer: ForcedControlTransfer::Enabled,
                recency_namespace: format!("remote-{profile_name}"),
                remote_consent: RemoteConsent::Enabled {
                    profile: profile_name.to_owned(),
                },
            }),
            handoff_status: HandoffStatusHub::default(),
            handoff_control: HandoffControlRestore::default(),
            forces: ForcesInFlight::default(),
        }
    }

    /// Opens one protocol connection with the endpoint's fixed role and transport.
    ///
    /// # Errors
    ///
    /// Returns an error when the local socket or remote logical channel cannot be
    /// opened and negotiated.
    pub async fn connect(&self) -> Result<Connection> {
        let mut connection = match self.endpoint.as_ref() {
            EndpointKind::Local => Connection::connect().await?,
            EndpointKind::Remote(session) => session.connect_interactive().await?,
        };
        if let Some(correlation) = crate::diagnostics::global()
            .and_then(crate::diagnostics::ClientDiagnostics::protocol_correlation)
        {
            connection.set_diagnostic_correlation(correlation);
        }
        Ok(connection)
    }

    /// Freeze this Window's display identity from its initial protocol connection.
    /// Local daemons advertise the same metadata but never receive remote chrome.
    #[must_use]
    pub fn remote_display_identity(
        &self,
        connection: &Connection,
    ) -> Option<RemoteDisplayIdentity> {
        remote_display_identity(self.is_local(), connection.daemon_hostname())
    }

    /// The Window-wide daemon handoff status shared by this factory's clones.
    #[must_use]
    pub fn handoff_status(&self) -> &HandoffStatusHub {
        &self.handoff_status
    }

    /// The resume ticket this Window's panes present after a daemon upgrade.
    #[must_use]
    pub fn handoff_control(&self) -> &HandoffControlRestore {
        &self.handoff_control
    }

    /// The forced takeovers this Window's panes wait on now.
    #[must_use]
    pub fn forces_in_flight(&self) -> &ForcesInFlight {
        &self.forces
    }

    /// Returns the explicit endpoint behavior contract.
    #[must_use]
    pub fn capabilities(&self) -> &EndpointCapabilities {
        &self.capabilities
    }

    /// Returns true only for the owner-local trusted endpoint.
    #[must_use]
    pub fn is_local(&self) -> bool {
        matches!(self.endpoint.as_ref(), EndpointKind::Local)
    }

    /// A remote endpoint's profile connect timeout, which also bounds the
    /// daemon requests made while a staged Window waits; `None` locally.
    #[must_use]
    pub fn remote_operation_timeout(&self) -> Option<std::time::Duration> {
        match self.endpoint.as_ref() {
            EndpointKind::Local => None,
            EndpointKind::Remote(session) => Some(session.operation_timeout()),
        }
    }
}

fn remote_display_identity(local: bool, hostname: Option<&str>) -> Option<RemoteDisplayIdentity> {
    (!local).then(|| RemoteDisplayIdentity::from_advertised(hostname))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_identity_uses_only_usable_negotiated_metadata_and_local_is_suppressed() {
        for value in [
            None,
            Some("actual-daemon"),
            Some("bad\nname"),
            Some("\u{202e}evil"),
        ] {
            assert!(remote_display_identity(true, value).is_none());
        }
        assert_eq!(
            remote_display_identity(false, Some("actual-daemon"))
                .unwrap()
                .label(),
            "Remote: actual-daemon"
        );
        assert_eq!(
            remote_display_identity(false, Some("actual-daemon"))
                .unwrap()
                .hostname(),
            Some("actual-daemon")
        );
        for value in [
            None,
            Some(""),
            Some("bad\nname"),
            Some("\u{202e}evil"),
            Some(&"x".repeat(256)),
        ] {
            assert_eq!(
                remote_display_identity(false, value).unwrap().label(),
                "Remote"
            );
            assert_eq!(
                remote_display_identity(false, value).unwrap().hostname(),
                None
            );
        }
        assert!(
            crate::frontend::WindowOptions::default()
                .remote_display_identity
                .is_none()
        );
    }

    #[test]
    fn local_capabilities_remain_trusted_and_remote_only_values_are_distinct() {
        let local = ConnectionFactory::local();
        assert!(local.is_local());
        assert_eq!(
            local.capabilities(),
            &EndpointCapabilities {
                image_transport: ImageTransport::LocalTrusted,
                graphical_focus_publication: GraphicalFocusPublication::Enabled,
                launch_semantics: LaunchSemantics::LocalTrusted,
                forced_control_transfer: ForcedControlTransfer::Enabled,
                recency_namespace: "local".to_owned(),
                remote_consent: RemoteConsent::Disabled,
            }
        );
        assert_ne!(
            local.capabilities().image_transport,
            ImageTransport::Unavailable
        );
    }

    #[test]
    fn a_force_is_in_flight_until_its_guard_drops() {
        let forces = ForcesInFlight::default();
        let (splint, other) = (SplintId::new(), SplintId::new());
        let first = forces.begin(splint, 3);
        let second = forces.begin(splint, 3);
        assert!(forces.contains(splint, 3));
        assert!(!forces.contains(splint, 4), "only that incarnation");
        assert!(!forces.contains(other, 3));
        drop(first);
        assert!(forces.contains(splint, 3), "the other pane still forces it");
        drop(second);
        assert!(!forces.contains(splint, 3));
    }
}
