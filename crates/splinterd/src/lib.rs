//! Daemon-owned runtime components.

pub mod authorization;
mod candidate_bootstrap;
pub mod cgroup;
pub mod child_stderr;
#[doc(hidden)]
pub mod dbus_worker;
pub mod dialog_shortcut;
pub mod executable_snapshot;
pub mod handoff_adoption;
pub mod handoff_checkpoint;
pub mod handoff_compatibility;
pub mod handoff_continuation;
pub mod handoff_coordinator;
pub mod handoff_descriptors;
pub mod handoff_exec;
#[allow(
    dead_code,
    reason = "the bounded manifest ABI lands before its coordinator adoption slice"
)]
pub(crate) mod handoff_manifest;
pub mod handoff_preflight;
pub mod handoff_prepare;
pub mod handoff_rollback;
pub mod handoff_window_carry;
pub mod image_transport;
pub mod install_prefix;
#[cfg(feature = "integration-test")]
#[doc(hidden)]
pub mod integration_test_seams;
mod live;
pub mod package_provenance;

pub use candidate_bootstrap::{
    CandidateBootstrap, CandidateEntryFailure, CandidatePreRuntimeStage, CandidateRuntimeStage,
    CandidateStageFailure,
};
#[doc(hidden)]
pub use candidate_bootstrap::{
    CandidateManifestAcquisition, bind_candidate_manifest, decode_candidate_manifest,
    handoff_manifest_max_bytes,
};
#[cfg(feature = "integration-test")]
#[doc(hidden)]
pub use handoff_manifest::encode_manifest_pair_for_integration_test;
pub use live::{
    ActorHandoffEvidence, CheckpointReservation, CompactSubscription, HandedOffSplint,
    HandoffCheckpointBudget, LiveAdoptionFailure, LiveAdoptionInputs, LiveCell, LiveError,
    LiveEvent, LiveRow, LiveRuntimeMetrics, LiveScrollbackPage, LiveSearchPage, LiveSnapshot,
    LiveSplintConfig, LiveSplintHandle, LiveSplintRuntime, PreparedPtyHandoff, ProcessExit,
    ProcessIncarnation, ProcessPlacement, RootProcessProbe, StagedLiveSplint, Subscription,
    SubscriptionReceive, TerminalPublicationMemoryLease,
};
pub use splinterm_policy::{executable_identity, inspect_policy_file, policy};

/// Ceiling on Splint processes one daemon owns: registered runtimes, persistent
/// and transient, plus runtimes still retiring and slots reserved for launches.
///
/// The runtime registry refuses every launch at this bound. Handoff manifests
/// carry only registered live Splints, a subset, so they are bounded by it too.
/// A retiring Splint has left the registry and is never captured as live.
pub const MAX_LIVE_SPLINTS: usize = 256;
// A full persisted topology must be able to run entirely live.
const _: () = assert!(MAX_LIVE_SPLINTS >= splinterm_core::MAX_SPLINTS);

/// Client connections the daemon serves at once.
///
/// One graphical Splint currently retains independent observation and control
/// connections. Keep enough bounded capacity for one 32-tab Window plus its
/// topology/theme channels, transient human inspection, and cleanup headroom.
/// Handoff manifests record the Windows' old connections, so they are bounded
/// by it too.
pub const CONNECTION_LIMIT: usize = 128;

/// The candidate activation flag, spelled `CANDIDATE_ACTIVATION_ARGUMENT=<fd>`,
/// where `<fd>` is the canonical decimal number of the inherited forward
/// manifest. It must be the first argument; the ordinary daemon arguments
/// follow it (Plan 0061 D4).
///
/// The binary's first-operation acquisition parses it and the old
/// generation's exec boundary writes it, from this one definition.
pub const CANDIDATE_ACTIVATION_ARGUMENT: &str = "--splinterm-internal-handoff-manifest-fd-v1";

/// The one ordinary daemon argument: every Splint runs in a workload cgroup,
/// and a launch without one is refused.
pub const REQUIRE_WORKLOAD_CGROUPS_ARGUMENT: &str = "--require-workload-cgroups";

/// Source commit for diagnostics: the packaged commit, or a best-effort commit
/// for unpackaged builds. It is never part of the handoff identity.
pub const SOURCE_COMMIT: Option<&str> = option_env!("SPLINTERM_SOURCE_COMMIT");
