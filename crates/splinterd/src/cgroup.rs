//! Nested systemd workload units: one transient slice per Dojo and one
//! transient scope per Splint process incarnation, on the user manager.

use std::{
    collections::HashMap,
    future::Future,
    io,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    ProcessPlacement,
    dbus_worker::{DbusWorker, DbusWorkerError},
};
use splinterm_core::{DojoId, SplintId};
use splinterm_pty::LinuxPtyIdentity;
use thiserror::Error;
use zbus::{
    proxy::CacheProperties,
    zvariant::{OwnedObjectPath, OwnedValue, Str, Value},
};

const AGGREGATE_MEMORY_HIGH_PERCENT: u32 = 75;
const METHOD_TIMEOUT: Duration = Duration::from_millis(500);
const VERIFY_TIMEOUT: Duration = Duration::from_secs(2);
const VERIFY_INTERVAL: Duration = Duration::from_millis(25);
static NEXT_PROBE_ID: AtomicU64 = AtomicU64::new(1);

/// Nested `MemoryHigh` percentages for Dojo slices and Splint scopes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkloadMemoryPolicy {
    pub(crate) dojo_memory_high_percent: u32,
    pub(crate) splint_memory_high_percent: u32,
}

impl Default for WorkloadMemoryPolicy {
    fn default() -> Self {
        Self {
            dojo_memory_high_percent: 50,
            splint_memory_high_percent: 25,
        }
    }
}

impl WorkloadMemoryPolicy {
    fn validate(self) -> Result<Self, WorkloadUnitError> {
        if self.splint_memory_high_percent == 0
            || self.splint_memory_high_percent >= self.dojo_memory_high_percent
            || self.dojo_memory_high_percent >= AGGREGATE_MEMORY_HIGH_PERCENT
        {
            return Err(WorkloadUnitError::InvalidPolicy);
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkloadUnitNames {
    pub(crate) dojo_slice: String,
    pub(crate) splint_scope: String,
}

/// Every Splint scope's unit name starts so.
const SPLINT_SCOPE_PREFIX: &str = "splinterm-splint";

impl WorkloadUnitNames {
    fn new(dojo_id: DojoId, splint_id: SplintId, incarnation: u64) -> Self {
        Self {
            dojo_slice: dojo_slice_name(dojo_id),
            splint_scope: format!(
                "{SPLINT_SCOPE_PREFIX}{}-i{incarnation}.scope",
                compact_id(&splint_id)
            ),
        }
    }
}

/// Whether the cgroup path component `unit` names a Splint scope, as
/// [`WorkloadUnitManager`] names them, of this daemon or an earlier one.
#[must_use]
pub fn is_splint_scope(unit: &str) -> bool {
    unit.strip_prefix(SPLINT_SCOPE_PREFIX)
        .and_then(|rest| rest.strip_suffix(".scope"))
        .is_some_and(|rest| rest.contains("-i"))
}

fn dojo_slice_name(dojo_id: DojoId) -> String {
    format!("app-splinterm-dojo{}.slice", compact_id(&dojo_id))
}

fn compact_id(id: &impl ToString) -> String {
    id.to_string()
        .chars()
        .filter(|character| *character != '-')
        .collect()
}

fn percent_scale(percent: u32) -> u32 {
    let scaled = (u64::from(u32::MAX) * u64::from(percent) + 50) / 100;
    u32::try_from(scaled).expect("bounded percentage scale fits u32")
}

/// A systemd workload unit request or its bookkeeping failed.
#[derive(Debug, Error)]
pub enum WorkloadUnitError {
    #[error("workload memory policy is invalid")]
    InvalidPolicy,
    #[error("systemd user-manager connection failed")]
    Connection(#[source] DbusWorkerError),
    #[error("systemd transient unit request failed")]
    Request(#[source] zbus::Error),
    #[error("systemd transient unit property is invalid")]
    Property(#[source] zbus::zvariant::Error),
    #[error("systemd did not place the PTY child in the expected scope")]
    IdentityMismatch,
    #[error("systemd workload unit did not stop within the deadline")]
    StopTimeout,
    #[error("workload unit state lock is unavailable")]
    State,
    #[error("systemd does not hold the retained Splint process in its expected workload units")]
    RetainedScopeMismatch,
    #[error("this workload unit manager already holds the Splint scope")]
    ScopeAlreadyHeld,
}

trait SystemdUnitAdapter: Send + Sync {
    fn probe(&self) -> Result<(), WorkloadUnitError>;

    /// Object path of the loaded unit `name`, or `None` when systemd has no
    /// such unit loaded.
    fn loaded_unit(&self, name: &str) -> Result<Option<String>, WorkloadUnitError>;

    /// Object path of the unit whose cgroup holds `pid` (`GetUnitByPID`), or
    /// `None` when systemd has no unit for it.
    fn unit_of_pid(&self, pid: u32) -> Result<Option<String>, WorkloadUnitError>;

    /// The slice that the loaded scope unit at `scope_path` runs in.
    fn scope_slice(&self, scope_path: &str) -> Result<String, WorkloadUnitError>;

    /// How many processes run anywhere in the cgroup subtree of the loaded
    /// unit `name` (`GetUnitProcesses`), or `None` when it is not loaded.
    fn unit_process_count(&self, name: &str) -> Result<Option<usize>, WorkloadUnitError>;

    fn start_dojo_slice(
        &self,
        name: &str,
        policy: WorkloadMemoryPolicy,
    ) -> Result<(), WorkloadUnitError>;

    fn start_splint_scope(
        &self,
        names: &WorkloadUnitNames,
        child_pid: u32,
        policy: WorkloadMemoryPolicy,
    ) -> Result<(), WorkloadUnitError>;

    fn stop_unit(&self, name: &str) -> Result<(), WorkloadUnitError>;

    /// Closes the systemd connection and joins any thread it owns.
    fn shutdown(&self) -> Result<(), WorkloadUnitError>;
}

struct SystemdUserManager {
    bus: DbusWorker,
    owner_unit: Option<String>,
}

impl SystemdUserManager {
    fn connect(owner_unit: Option<&str>) -> Result<Self, WorkloadUnitError> {
        let bus =
            DbusWorker::connect_session(METHOD_TIMEOUT).map_err(WorkloadUnitError::Connection)?;
        Ok(Self {
            bus,
            owner_unit: owner_unit.map(str::to_owned),
        })
    }

    /// Runs one systemd manager request on the owned D-Bus worker.
    fn manager<T, F, Fut>(&self, request: F) -> Result<T, WorkloadUnitError>
    where
        T: Send + 'static,
        F: FnOnce(SystemdManagerProxy<'static>) -> Fut + Send + 'static,
        Fut: Future<Output = zbus::Result<T>> + Send,
    {
        self.bus
            .call(move |connection| async move {
                let proxy = SystemdManagerProxy::builder(&connection)
                    .cache_properties(CacheProperties::No)
                    .build()
                    .await?;
                request(proxy).await
            })
            .map_err(WorkloadUnitError::Connection)?
            .map_err(WorkloadUnitError::Request)
    }

    fn start_transient_unit(
        &self,
        name: &str,
        properties: Vec<UnitProperty>,
    ) -> Result<(), WorkloadUnitError> {
        let name = name.to_owned();
        self.manager(move |manager| async move {
            manager
                .start_transient_unit(&name, "fail", properties, Vec::new())
                .await
                .map(|_| ())
        })
    }

    fn stop_unit_request(&self, name: &str) -> Result<OwnedObjectPath, WorkloadUnitError> {
        let name = name.to_owned();
        self.manager(move |manager| async move { manager.stop_unit(&name, "replace").await })
    }

    fn unit_path(&self, name: &str) -> Result<OwnedObjectPath, WorkloadUnitError> {
        let name = name.to_owned();
        self.manager(move |manager| async move { manager.get_unit(&name).await })
    }

    fn pid_unit_path(&self, pid: u32) -> Result<OwnedObjectPath, WorkloadUnitError> {
        self.manager(move |manager| async move { manager.get_unit_by_pid(pid).await })
    }

    fn unit_processes(&self, name: &str) -> Result<Vec<(String, u32, String)>, WorkloadUnitError> {
        let name = name.to_owned();
        self.manager(move |manager| async move { manager.get_unit_processes(&name).await })
    }
}

impl SystemdUnitAdapter for SystemdUserManager {
    fn loaded_unit(&self, name: &str) -> Result<Option<String>, WorkloadUnitError> {
        match self.unit_path(name) {
            Ok(path) => Ok(Some(path.to_string())),
            Err(WorkloadUnitError::Request(error)) if is_no_such_unit(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn unit_of_pid(&self, pid: u32) -> Result<Option<String>, WorkloadUnitError> {
        match self.pid_unit_path(pid) {
            Ok(path) => Ok(Some(path.to_string())),
            Err(WorkloadUnitError::Request(error)) if is_no_unit_for_pid(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn unit_process_count(&self, name: &str) -> Result<Option<usize>, WorkloadUnitError> {
        match self.unit_processes(name) {
            Ok(processes) => Ok(Some(processes.len())),
            Err(WorkloadUnitError::Request(error)) if is_no_such_unit(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn scope_slice(&self, scope_path: &str) -> Result<String, WorkloadUnitError> {
        let path = OwnedObjectPath::try_from(scope_path.to_owned())
            .map_err(WorkloadUnitError::Property)?;
        self.bus
            .call(move |connection| async move {
                SystemdScopeProxy::builder(&connection)
                    .path(path)?
                    .cache_properties(CacheProperties::No)
                    .build()
                    .await?
                    .slice()
                    .await
            })
            .map_err(WorkloadUnitError::Connection)?
            .map_err(WorkloadUnitError::Request)
    }

    fn probe(&self) -> Result<(), WorkloadUnitError> {
        let probe_id = NEXT_PROBE_ID.fetch_add(1, Ordering::Relaxed);
        let name = format!("app-splinterm-probe{}i{probe_id}.slice", std::process::id());
        let properties = capability_probe_properties(self.owner_unit.as_deref())?;
        if let Err(error) = self.start_transient_unit(&name, properties) {
            if !is_unit_exists(&error) {
                let _ = self.stop_unit(&name);
            }
            return Err(error);
        }
        self.stop_unit(&name)
    }

    fn start_dojo_slice(
        &self,
        name: &str,
        policy: WorkloadMemoryPolicy,
    ) -> Result<(), WorkloadUnitError> {
        self.start_transient_unit(
            name,
            dojo_slice_properties(self.owner_unit.as_deref(), policy)?,
        )
    }

    fn start_splint_scope(
        &self,
        names: &WorkloadUnitNames,
        child_pid: u32,
        policy: WorkloadMemoryPolicy,
    ) -> Result<(), WorkloadUnitError> {
        let result = self.start_transient_unit(
            &names.splint_scope,
            splint_scope_properties(names, child_pid, policy)?,
        );
        if let Err(error) = result {
            if !is_unit_exists(&error) {
                let _ = self.stop_unit(&names.splint_scope);
            }
            return Err(error);
        }

        let expected = self.unit_path(&names.splint_scope)?;
        let deadline = Instant::now() + VERIFY_TIMEOUT;
        let mut last_error = None;
        while Instant::now() < deadline {
            match self.pid_unit_path(child_pid) {
                Ok(actual) if actual == expected => return Ok(()),
                Ok(_) => last_error = None,
                Err(error) => last_error = Some(error),
            }
            thread::sleep(VERIFY_INTERVAL);
        }
        let _ = self.stop_unit(&names.splint_scope);
        Err(last_error.unwrap_or(WorkloadUnitError::IdentityMismatch))
    }

    fn stop_unit(&self, name: &str) -> Result<(), WorkloadUnitError> {
        let request_error = match self.stop_unit_request(name) {
            Ok(_) => None,
            Err(WorkloadUnitError::Request(error)) if is_no_such_unit(&error) => return Ok(()),
            Err(WorkloadUnitError::Request(error)) => Some(error),
            Err(error) => return Err(error),
        };
        let deadline = Instant::now() + VERIFY_TIMEOUT;
        while Instant::now() < deadline {
            match self.unit_path(name) {
                Err(WorkloadUnitError::Request(error)) if is_no_such_unit(&error) => return Ok(()),
                Err(error) => return Err(error),
                Ok(_) => thread::sleep(VERIFY_INTERVAL),
            }
        }
        request_error.map_or(Err(WorkloadUnitError::StopTimeout), |error| {
            Err(WorkloadUnitError::Request(error))
        })
    }

    fn shutdown(&self) -> Result<(), WorkloadUnitError> {
        self.bus.shutdown().map_err(WorkloadUnitError::Connection)
    }
}

fn is_unit_exists(error: &WorkloadUnitError) -> bool {
    matches!(
        error,
        WorkloadUnitError::Request(zbus::Error::MethodError(name, _, _))
            if name.as_str() == "org.freedesktop.systemd1.UnitExists"
    )
}

fn is_no_such_unit(error: &zbus::Error) -> bool {
    matches!(
        error,
        zbus::Error::MethodError(name, _, _)
            if name.as_str() == "org.freedesktop.systemd1.NoSuchUnit"
    )
}

fn is_no_unit_for_pid(error: &zbus::Error) -> bool {
    matches!(
        error,
        zbus::Error::MethodError(name, _, _)
            if name.as_str() == "org.freedesktop.systemd1.NoUnitForPID"
    )
}

fn string_property<'a>(name: &'a str, value: &str) -> (&'a str, OwnedValue) {
    (name, OwnedValue::from(Str::from(value.to_owned())))
}

fn u32_property(name: &str, value: u32) -> (&str, OwnedValue) {
    (name, OwnedValue::from(value))
}

fn vec_u32_property(name: &str, value: Vec<u32>) -> Result<(&str, OwnedValue), WorkloadUnitError> {
    Ok((
        name,
        OwnedValue::try_from(Value::from(value)).map_err(WorkloadUnitError::Property)?,
    ))
}

fn vec_string_property(
    name: &str,
    value: Vec<String>,
) -> Result<(&str, OwnedValue), WorkloadUnitError> {
    Ok((
        name,
        OwnedValue::try_from(Value::from(value)).map_err(WorkloadUnitError::Property)?,
    ))
}

type UnitProperty = (&'static str, OwnedValue);

fn capability_probe_properties(
    owner_unit: Option<&str>,
) -> Result<Vec<UnitProperty>, WorkloadUnitError> {
    let mut properties = vec![
        string_property("Description", "Splinterm workload capability probe"),
        u32_property("MemoryHighScale", percent_scale(1)),
        string_property("CollectMode", "inactive-or-failed"),
    ];
    if let Some(owner_unit) = owner_unit {
        properties.push(vec_string_property("PartOf", vec![owner_unit.to_owned()])?);
    }
    Ok(properties)
}

fn dojo_slice_properties(
    owner_unit: Option<&str>,
    policy: WorkloadMemoryPolicy,
) -> Result<Vec<UnitProperty>, WorkloadUnitError> {
    let mut properties = vec![
        string_property("Description", "Splinterm Dojo workload"),
        u32_property(
            "MemoryHighScale",
            percent_scale(policy.dojo_memory_high_percent),
        ),
        string_property("CollectMode", "inactive-or-failed"),
    ];
    if let Some(owner_unit) = owner_unit {
        properties.push(vec_string_property("PartOf", vec![owner_unit.to_owned()])?);
    }
    Ok(properties)
}

fn splint_scope_properties(
    names: &WorkloadUnitNames,
    child_pid: u32,
    policy: WorkloadMemoryPolicy,
) -> Result<Vec<UnitProperty>, WorkloadUnitError> {
    Ok(vec![
        string_property("Description", "Splinterm Splint workload"),
        string_property("Slice", &names.dojo_slice),
        vec_u32_property("PIDs", vec![child_pid])?,
        u32_property(
            "MemoryHighScale",
            percent_scale(policy.splint_memory_high_percent),
        ),
        string_property("CollectMode", "inactive-or-failed"),
    ])
}

#[zbus::proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait SystemdManager {
    fn start_transient_unit(
        &self,
        name: &str,
        mode: &str,
        properties: Vec<(&str, OwnedValue)>,
        auxiliary: Vec<(&str, Vec<(&str, OwnedValue)>)>,
    ) -> zbus::Result<OwnedObjectPath>;

    fn stop_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;

    fn get_unit(&self, name: &str) -> zbus::Result<OwnedObjectPath>;

    #[zbus(name = "GetUnitByPID")]
    fn get_unit_by_pid(&self, pid: u32) -> zbus::Result<OwnedObjectPath>;

    /// Every process in the unit's cgroup subtree: cgroup path, PID, and
    /// command line.
    fn get_unit_processes(&self, name: &str) -> zbus::Result<Vec<(String, u32, String)>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.systemd1.Scope",
    default_service = "org.freedesktop.systemd1"
)]
trait SystemdScope {
    #[zbus(property)]
    fn slice(&self) -> zbus::Result<String>;
}

#[derive(Default)]
struct WorkloadUnitState {
    dojo_references: HashMap<DojoId, usize>,
}

/// How this manager holds one Splint scope's Dojo reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScopeHold {
    dojo_id: DojoId,
    /// The reference is still counted, but the [`ActiveWorkloadScope`] that
    /// owned it was retained for a handoff, so no live value releases it.
    retained: bool,
}

/// Scopes this manager holds a Dojo reference for, keyed by scope unit name.
///
/// Its lock is never held across a systemd request, so retaining a scope
/// cannot block on the session bus.
type ScopeHolds = HashMap<String, ScopeHold>;

/// Places Splint processes in reference-counted Dojo slices and per-Splint
/// scopes on the systemd user manager.
///
/// # Dojo reference counts across a handoff
///
/// Each Splint scope holds one reference on its Dojo slice, and the last
/// release stops the slice. The manager also records which scopes hold those
/// references, by scope unit name:
///
/// - [`ActiveWorkloadScope::retain_for_handoff`] keeps the reference counted,
///   because the retained process still occupies the slice, and marks the
///   scope retained.
/// - [`WorkloadUnitManager::adopt_scope`] transfers a reference this manager
///   already counts for a retained scope (in-process re-adoption: a resume
///   after a failed exec, or a rollback) instead of counting it again, and
///   counts a new reference for a scope it has never held (a fresh manager,
///   such as the forward daemon's). Adopting `N` retained scopes of one Dojo
///   into a fresh manager therefore yields `N` references, and re-adopting
///   them in process leaves the count unchanged.
/// - Adopting a scope that a live [`ActiveWorkloadScope`] of this manager still
///   holds fails with [`WorkloadUnitError::ScopeAlreadyHeld`].
///
/// A retained scope that is never adopted again in this process keeps its
/// reference, so the manager never stops its Dojo slice: the failure leaks a
/// slice rather than killing a Splint. A process that resumes after
/// [`WorkloadUnitManager::shutdown`] cannot make systemd requests on this
/// manager, so it connects a fresh one, adopts every retained scope into it,
/// and drops this one with its retained references.
#[derive(Clone)]
pub struct WorkloadUnitManager {
    adapter: Arc<dyn SystemdUnitAdapter>,
    policy: WorkloadMemoryPolicy,
    state: Arc<Mutex<WorkloadUnitState>>,
    holds: Arc<Mutex<ScopeHolds>>,
}

impl WorkloadUnitManager {
    /// Connects to the session's systemd user manager and probes that it can
    /// create transient memory-limited units.
    ///
    /// # Errors
    /// Reports an invalid policy, an unavailable session bus, or a failed
    /// capability probe.
    pub fn connect(
        policy: WorkloadMemoryPolicy,
        owner_unit: Option<&str>,
    ) -> Result<Self, WorkloadUnitError> {
        Self::new(Arc::new(SystemdUserManager::connect(owner_unit)?), policy)
    }

    fn new(
        adapter: Arc<dyn SystemdUnitAdapter>,
        policy: WorkloadMemoryPolicy,
    ) -> Result<Self, WorkloadUnitError> {
        let policy = policy.validate()?;
        adapter.probe()?;
        Ok(Self {
            adapter,
            policy,
            state: Arc::new(Mutex::new(WorkloadUnitState::default())),
            holds: Arc::new(Mutex::new(ScopeHolds::default())),
        })
    }

    /// Takes a Dojo slice reference for one Splint incarnation, starting the
    /// slice when it has no other reference.
    ///
    /// # Errors
    /// Reports a poisoned state lock or a failed slice start.
    ///
    /// # Panics
    /// Never in practice: the live Splint bound keeps Dojo reference counts
    /// far below `usize::MAX`.
    pub fn prepare(
        &self,
        dojo_id: DojoId,
        splint_id: SplintId,
        incarnation: u64,
    ) -> Result<PreparedWorkloadScope, WorkloadUnitError> {
        let names = WorkloadUnitNames::new(dojo_id, splint_id, incarnation);
        let mut state = self.state.lock().map_err(|_| WorkloadUnitError::State)?;
        let known = state.dojo_references.contains_key(&dojo_id);
        let references = state.dojo_references.entry(dojo_id).or_default();
        if *references == 0 {
            if known {
                self.adapter.stop_unit(&names.dojo_slice)?;
            }
            if let Err(error) = self
                .adapter
                .start_dojo_slice(&names.dojo_slice, self.policy)
            {
                if !is_unit_exists(&error) {
                    let _ = self.adapter.stop_unit(&names.dojo_slice);
                }
                if !known {
                    // Forget a Dojo this call first saw. A known entry at zero
                    // makes the next prepare stop the slice first, and a slice
                    // that already existed (one a previous generation left,
                    // with its Splints still running) must never be stopped.
                    state.dojo_references.remove(&dojo_id);
                }
                return Err(error);
            }
        }
        *references = references
            .checked_add(1)
            .expect("live Splint bound prevents Dojo reference overflow");
        drop(state);

        Ok(PreparedWorkloadScope {
            manager: self.clone(),
            dojo_id,
            splint_id,
            incarnation,
            names,
            committed: false,
        })
    }

    /// Takes ownership of a Splint scope that a previous owner retained with
    /// [`ActiveWorkloadScope::retain_for_handoff`], after checking that
    /// systemd still holds the process in exactly that scope and Dojo slice.
    ///
    /// The checks, all under the reference-count lock so no slice stop can
    /// interleave, are:
    /// - the scope unit is loaded;
    /// - `GetUnitByPID(child_pid)` names that exact unit;
    /// - the scope runs in the Dojo slice named by the retained Dojo, and that
    ///   slice is loaded.
    ///
    /// Then the Dojo reference is transferred or counted as described on
    /// [`WorkloadUnitManager`]. Every failure leaves systemd and the counts
    /// untouched: it stops nothing, so the caller still decides the
    /// process's fate.
    ///
    /// Callers must keep two contracts:
    /// - A failed adoption of a process that is still running leaves it
    ///   uncounted, so this manager stops its Dojo slice, killing that
    ///   process, once the adopted Splints of the same Dojo release. The
    ///   caller must roll back, or kill or exclude that Splint, and never
    ///   continue with it running uncounted.
    /// - Each retained identity is adopted into exactly one manager per
    ///   process. Holds are per manager, so two managers would each count the
    ///   scope and each stop it.
    ///
    /// # Errors
    /// [`WorkloadUnitError::RetainedScopeMismatch`] for an invalid PID, a
    /// process in another unit or in none (an exited process), a missing
    /// scope or slice, or a scope in another slice;
    /// [`WorkloadUnitError::ScopeAlreadyHeld`] when a live scope of this
    /// manager already holds the unit; a failed systemd request; or a
    /// poisoned state lock, or a Dojo whose last slice stop failed.
    ///
    /// # Panics
    /// Never in practice: the live Splint bound keeps Dojo reference counts
    /// far below `usize::MAX`.
    pub fn adopt_scope(
        &self,
        retained: &RetainedWorkloadScope,
    ) -> Result<ActiveWorkloadScope, WorkloadUnitError> {
        let names = &retained.names;
        if retained.child_pid == 0 || i32::try_from(retained.child_pid).is_err() {
            return Err(WorkloadUnitError::RetainedScopeMismatch);
        }
        let mut state = self.state.lock().map_err(|_| WorkloadUnitError::State)?;
        let scope_path = self
            .adapter
            .loaded_unit(&names.splint_scope)?
            .ok_or(WorkloadUnitError::RetainedScopeMismatch)?;
        if self.adapter.unit_of_pid(retained.child_pid)?.as_ref() != Some(&scope_path)
            || self.adapter.scope_slice(&scope_path)? != names.dojo_slice
            || self.adapter.loaded_unit(&names.dojo_slice)?.is_none()
        {
            return Err(WorkloadUnitError::RetainedScopeMismatch);
        }

        let mut holds = lock_holds(&self.holds);
        match holds.get(&names.splint_scope).copied() {
            Some(ScopeHold {
                dojo_id,
                retained: true,
            }) if dojo_id == retained.dojo_id => {
                if state
                    .dojo_references
                    .get(&dojo_id)
                    .is_none_or(|references| *references == 0)
                {
                    return Err(WorkloadUnitError::State);
                }
            }
            Some(ScopeHold { retained: true, .. }) => {
                return Err(WorkloadUnitError::RetainedScopeMismatch);
            }
            Some(ScopeHold {
                retained: false, ..
            }) => return Err(WorkloadUnitError::ScopeAlreadyHeld),
            None => {
                let known = state.dojo_references.contains_key(&retained.dojo_id);
                let references = state.dojo_references.entry(retained.dojo_id).or_default();
                if known && *references == 0 {
                    // The last release's slice stop failed, so the slice may
                    // still be stopping. Counting it again would hide that.
                    return Err(WorkloadUnitError::State);
                }
                *references = references
                    .checked_add(1)
                    .expect("live Splint bound prevents Dojo reference overflow");
            }
        }
        holds.insert(
            names.splint_scope.clone(),
            ScopeHold {
                dojo_id: retained.dojo_id,
                retained: false,
            },
        );
        drop(holds);
        drop(state);

        Ok(ActiveWorkloadScope {
            manager: self.clone(),
            scope: retained.clone(),
            released: false,
        })
    }

    /// Stops a Dojo slice a previous generation left loaded, only when
    /// systemd proves it holds no process at all.
    ///
    /// A handoff that adopts none of a Dojo's carried scopes (every carried
    /// Splint of it exited in the exec window) leaves that Dojo's slice
    /// loaded and uncounted here, and [`Self::prepare`] then fails with
    /// `UnitExists` for every later launch in the Dojo. This reclaims such a
    /// slice when `GetUnitProcesses` reports an empty cgroup subtree, under
    /// the reference-count lock, so no prepare in this manager can interleave.
    /// A slice that still holds any process (a scope this generation did not
    /// adopt, or an exited Splint's surviving background job) is left alone:
    /// stopping it would kill that process.
    ///
    /// # Errors
    /// Reports a poisoned state lock or a failed systemd request; nothing is
    /// stopped then.
    pub fn reclaim_empty_dojo_slice(
        &self,
        dojo_id: DojoId,
    ) -> Result<DojoSliceReclaim, WorkloadUnitError> {
        let state = self.state.lock().map_err(|_| WorkloadUnitError::State)?;
        if state.dojo_references.contains_key(&dojo_id) {
            return Ok(DojoSliceReclaim::Referenced);
        }
        let slice = dojo_slice_name(dojo_id);
        let reclaim = match self.adapter.unit_process_count(&slice)? {
            None => DojoSliceReclaim::Absent,
            Some(0) => {
                self.adapter.stop_unit(&slice)?;
                DojoSliceReclaim::Stopped
            }
            Some(_) => DojoSliceReclaim::Occupied,
        };
        drop(state);
        Ok(reclaim)
    }

    /// Whether systemd has neither the retained scope nor its Dojo slice
    /// loaded: the evidence that the generation which placed this process
    /// ran it uncontained, so no slice exists that could stop under it.
    ///
    /// # Errors
    /// Reports a failed systemd request.
    pub fn retained_units_absent(
        &self,
        retained: &RetainedWorkloadScope,
    ) -> Result<bool, WorkloadUnitError> {
        Ok(self
            .adapter
            .loaded_unit(&retained.names.splint_scope)?
            .is_none()
            && self
                .adapter
                .loaded_unit(&retained.names.dojo_slice)?
                .is_none())
    }

    /// Closes the systemd user-manager connection and joins its D-Bus worker
    /// thread, so the daemon can return to one thread for upgrade handoff.
    ///
    /// Running requests get up to
    /// [`SHUTDOWN_GRACE`](crate::dbus_worker::SHUTDOWN_GRACE) to finish
    /// before they are aborted, and closing is bounded the same way, so a
    /// wedged session bus cannot hold this forever.
    ///
    /// Clones of this manager and any outstanding scopes stay valid values,
    /// but every later systemd request fails with
    /// [`WorkloadUnitError::Connection`]. Dropping a scope after shutdown
    /// therefore leaves its systemd unit running. Calling this again is a
    /// no-op.
    ///
    /// # Errors
    /// Reports a D-Bus worker thread that panicked.
    pub fn shutdown(self) -> Result<(), WorkloadUnitError> {
        self.adapter.shutdown()
    }

    fn release_dojo(&self, dojo_id: DojoId, dojo_slice: &str) -> Result<(), WorkloadUnitError> {
        let mut state = self.state.lock().map_err(|_| WorkloadUnitError::State)?;
        let Some(references) = state.dojo_references.get_mut(&dojo_id) else {
            return Ok(());
        };
        if *references == 0 {
            return Err(WorkloadUnitError::State);
        }
        *references -= 1;
        if *references == 0 {
            self.adapter.stop_unit(dojo_slice)?;
            state.dojo_references.remove(&dojo_id);
        }
        Ok(())
    }

    fn set_hold(&self, scope: &RetainedWorkloadScope, retained: bool) {
        lock_holds(&self.holds).insert(
            scope.names.splint_scope.clone(),
            ScopeHold {
                dojo_id: scope.dojo_id,
                retained,
            },
        );
    }

    /// The unit name of every Dojo slice this manager counts references
    /// for: each live Splint scope runs inside one of them.
    ///
    /// # Errors
    /// Reports a poisoned state lock.
    pub fn dojo_slices(&self) -> Result<Vec<String>, WorkloadUnitError> {
        let state = self.state.lock().map_err(|_| WorkloadUnitError::State)?;
        Ok(state
            .dojo_references
            .keys()
            .map(|dojo_id| dojo_slice_name(*dojo_id))
            .collect())
    }

    fn clear_hold(&self, scope: &RetainedWorkloadScope) {
        lock_holds(&self.holds).remove(&scope.names.splint_scope);
    }

    #[cfg(test)]
    fn dojo_references(&self, dojo_id: DojoId) -> usize {
        self.state
            .lock()
            .unwrap()
            .dojo_references
            .get(&dojo_id)
            .copied()
            .unwrap_or_default()
    }
}

/// Locks the scope holds. Every holder makes one map insertion or removal, so
/// a panicking holder cannot leave the map inconsistent, and poisoning is
/// ignored: retaining a scope must not fail.
fn lock_holds(holds: &Mutex<ScopeHolds>) -> MutexGuard<'_, ScopeHolds> {
    holds.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What [`WorkloadUnitManager::reclaim_empty_dojo_slice`] found and did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DojoSliceReclaim {
    /// This manager counts the Dojo, so its slice is not a leftover.
    Referenced,
    /// No slice for the Dojo is loaded.
    Absent,
    /// The slice still holds a process, so it was left running.
    Occupied,
    /// The slice held no process and was stopped.
    Stopped,
}

/// A Dojo slice reference waiting for its Splint process; dropping it unplaced
/// returns the reference.
pub struct PreparedWorkloadScope {
    manager: WorkloadUnitManager,
    dojo_id: DojoId,
    splint_id: SplintId,
    incarnation: u64,
    names: WorkloadUnitNames,
    committed: bool,
}

impl PreparedWorkloadScope {
    /// Starts the Splint scope around the PTY child and verifies systemd
    /// placed the child in it.
    ///
    /// # Errors
    /// Reports a failed scope start or an unverified placement; the Dojo
    /// reference is returned when the prepared scope drops.
    pub fn place(mut self, identity: LinuxPtyIdentity) -> io::Result<ActiveWorkloadScope> {
        self.manager
            .adapter
            .start_splint_scope(&self.names, identity.child_pid(), self.manager.policy)
            .map_err(io::Error::other)?;
        self.committed = true;
        let scope = RetainedWorkloadScope {
            dojo_id: self.dojo_id,
            splint_id: self.splint_id,
            incarnation: self.incarnation,
            child_pid: identity.child_pid(),
            names: self.names.clone(),
        };
        self.manager.set_hold(&scope, false);
        Ok(ActiveWorkloadScope {
            manager: self.manager.clone(),
            scope,
            released: false,
        })
    }
}

impl Drop for PreparedWorkloadScope {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self
                .manager
                .release_dojo(self.dojo_id, &self.names.dojo_slice);
        }
    }
}

/// A placed Splint scope holding one Dojo slice reference; releasing or
/// dropping it stops the scope and returns the reference, unless it is
/// retained for a handoff.
pub struct ActiveWorkloadScope {
    manager: WorkloadUnitManager,
    scope: RetainedWorkloadScope,
    released: bool,
}

impl ActiveWorkloadScope {
    pub(crate) fn release(mut self) -> Result<(), WorkloadUnitError> {
        self.released = true;
        self.stop()
    }

    fn stop(&self) -> Result<(), WorkloadUnitError> {
        self.manager.clear_hold(&self.scope);
        let scope_result = self
            .manager
            .adapter
            .stop_unit(&self.scope.names.splint_scope);
        let dojo_result = self
            .manager
            .release_dojo(self.scope.dojo_id, &self.scope.names.dojo_slice);
        scope_result.and(dojo_result)
    }

    /// Gives up this scope for a daemon handoff without stopping anything:
    /// the Splint scope keeps running, and its Dojo slice reference stays
    /// counted in this manager, so this manager never stops the slice under
    /// the retained process.
    ///
    /// The manager records the scope as retained, so a later
    /// [`WorkloadUnitManager::adopt_scope`] on it (or a clone) transfers the
    /// reference instead of counting it twice. This makes no systemd request,
    /// never blocks on the session bus, and cannot fail, so the live actor
    /// may call it inline.
    ///
    /// The returned value is plain data with no manager or connection, so
    /// dropping it stops nothing. A new owner rebuilds an equal value from
    /// handoff manifest fields with [`RetainedWorkloadScope::new`].
    #[must_use = "the retained scope identity is what a new owner adopts"]
    pub fn retain_for_handoff(mut self) -> RetainedWorkloadScope {
        self.released = true;
        self.manager.set_hold(&self.scope, true);
        self.scope.clone()
    }

    /// The identity of this scope.
    #[must_use]
    pub fn identity(&self) -> &RetainedWorkloadScope {
        &self.scope
    }
}

impl Drop for ActiveWorkloadScope {
    fn drop(&mut self) {
        if !self.released {
            let _ = self.stop();
        }
    }
}

impl ProcessPlacement for ActiveWorkloadScope {
    fn release(self) {
        let _ = ActiveWorkloadScope::release(self);
    }

    /// The live actor's handoff path: retains the scope as
    /// [`ActiveWorkloadScope::retain_for_handoff`] does and discards the
    /// identity, which the handoff manifest already carries.
    fn retain_for_handoff(self) {
        let _ = ActiveWorkloadScope::retain_for_handoff(self);
    }
}

/// The identity of a Splint workload scope that outlives the value that
/// placed it: a running process in a named scope inside a Dojo slice.
///
/// It owns nothing: dropping it makes no systemd request and changes no
/// reference count. [`WorkloadUnitManager::adopt_scope`] turns it back into
/// an [`ActiveWorkloadScope`] after verifying systemd still agrees.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetainedWorkloadScope {
    dojo_id: DojoId,
    splint_id: SplintId,
    incarnation: u64,
    child_pid: u32,
    names: WorkloadUnitNames,
}

impl RetainedWorkloadScope {
    /// Rebuilds a retained scope identity from the fields a handoff manifest
    /// records for a live Splint. The unit names are derived exactly as
    /// [`WorkloadUnitManager::prepare`] derived them.
    #[must_use]
    pub fn new(dojo_id: DojoId, splint_id: SplintId, incarnation: u64, child_pid: u32) -> Self {
        Self {
            dojo_id,
            splint_id,
            incarnation,
            child_pid,
            names: WorkloadUnitNames::new(dojo_id, splint_id, incarnation),
        }
    }

    /// The Dojo whose slice holds the scope.
    #[must_use]
    pub fn dojo_id(&self) -> DojoId {
        self.dojo_id
    }

    /// The Splint the scope runs.
    #[must_use]
    pub fn splint_id(&self) -> SplintId {
        self.splint_id
    }

    /// The Splint process incarnation the scope is named after.
    #[must_use]
    pub fn incarnation(&self) -> u64 {
        self.incarnation
    }

    /// The PTY child placed in the scope.
    #[must_use]
    pub fn child_pid(&self) -> u32 {
        self.child_pid
    }

    /// The Dojo slice unit name.
    #[must_use]
    pub fn dojo_slice(&self) -> &str {
        &self.names.dojo_slice
    }

    /// The Splint scope unit name.
    #[must_use]
    pub fn splint_scope(&self) -> &str {
        &self.names.splint_scope
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Condvar, mpsc};

    use super::*;

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum AdapterCall {
        StartDojo(String),
        StartSplint(String, String, u32),
        Stop(String),
    }

    #[derive(Default)]
    struct StopGate {
        state: Mutex<(bool, bool)>,
        changed: Condvar,
    }

    impl StopGate {
        fn wait_until_entered(&self) {
            let mut state = self.state.lock().unwrap();
            while !state.0 {
                state = self.changed.wait(state).unwrap();
            }
        }

        fn release(&self) {
            let mut state = self.state.lock().unwrap();
            state.1 = true;
            self.changed.notify_all();
        }
    }

    #[derive(Default)]
    struct FakeAdapter {
        calls: Mutex<Vec<AdapterCall>>,
        fail_probe: Mutex<bool>,
        fail_scope: Mutex<bool>,
        stop_gate: Mutex<Option<Arc<StopGate>>>,
        shutdowns: Mutex<usize>,
        systemd: Mutex<FakeSystemd>,
    }

    /// The user manager's view: loaded units (each scope with its slice) and
    /// the unit holding each process.
    #[derive(Default)]
    struct FakeSystemd {
        units: HashMap<String, Option<String>>,
        pids: HashMap<u32, String>,
    }

    fn fake_unit_path(name: &str) -> String {
        format!("/org/freedesktop/systemd1/unit/{name}")
    }

    /// The error systemd returns when a transient unit name is taken, as
    /// [`is_unit_exists`] classifies it.
    fn unit_exists_error() -> WorkloadUnitError {
        let reply = zbus::Message::method_call("/", "StartTransientUnit")
            .unwrap()
            .build(&())
            .unwrap();
        let error = WorkloadUnitError::Request(zbus::Error::MethodError(
            zbus::names::ErrorName::try_from("org.freedesktop.systemd1.UnitExists")
                .unwrap()
                .into(),
            None,
            reply,
        ));
        assert!(is_unit_exists(&error));
        error
    }

    impl FakeAdapter {
        fn calls(&self) -> Vec<AdapterCall> {
            self.calls.lock().unwrap().clone()
        }

        fn stops(&self) -> Vec<String> {
            self.calls()
                .into_iter()
                .filter_map(|call| match call {
                    AdapterCall::Stop(name) => Some(name),
                    _ => None,
                })
                .collect()
        }

        fn slice_stops(&self, slice: &str) -> usize {
            self.stops().iter().filter(|name| *name == slice).count()
        }

        fn is_loaded(&self, name: &str) -> bool {
            self.systemd.lock().unwrap().units.contains_key(name)
        }
    }

    impl SystemdUnitAdapter for FakeAdapter {
        fn unit_process_count(&self, name: &str) -> Result<Option<usize>, WorkloadUnitError> {
            let systemd = self.systemd.lock().unwrap();
            if !systemd.units.contains_key(name) {
                return Ok(None);
            }
            // A process counts for its own unit and for the slice it runs in.
            Ok(Some(
                systemd
                    .pids
                    .values()
                    .filter(|unit| {
                        *unit == name
                            || systemd.units.get(*unit).and_then(Option::as_deref) == Some(name)
                    })
                    .count(),
            ))
        }

        fn loaded_unit(&self, name: &str) -> Result<Option<String>, WorkloadUnitError> {
            Ok(self
                .systemd
                .lock()
                .unwrap()
                .units
                .contains_key(name)
                .then(|| fake_unit_path(name)))
        }

        fn unit_of_pid(&self, pid: u32) -> Result<Option<String>, WorkloadUnitError> {
            Ok(self
                .systemd
                .lock()
                .unwrap()
                .pids
                .get(&pid)
                .map(|name| fake_unit_path(name)))
        }

        fn scope_slice(&self, scope_path: &str) -> Result<String, WorkloadUnitError> {
            let systemd = self.systemd.lock().unwrap();
            systemd
                .units
                .iter()
                .find(|(name, _)| fake_unit_path(name) == scope_path)
                .and_then(|(_, slice)| slice.clone())
                .ok_or(WorkloadUnitError::RetainedScopeMismatch)
        }

        fn probe(&self) -> Result<(), WorkloadUnitError> {
            if *self.fail_probe.lock().unwrap() {
                Err(WorkloadUnitError::IdentityMismatch)
            } else {
                Ok(())
            }
        }

        fn start_dojo_slice(
            &self,
            name: &str,
            _policy: WorkloadMemoryPolicy,
        ) -> Result<(), WorkloadUnitError> {
            self.calls
                .lock()
                .unwrap()
                .push(AdapterCall::StartDojo(name.to_owned()));
            let mut systemd = self.systemd.lock().unwrap();
            if systemd.units.contains_key(name) {
                return Err(unit_exists_error());
            }
            systemd.units.insert(name.to_owned(), None);
            Ok(())
        }

        fn start_splint_scope(
            &self,
            names: &WorkloadUnitNames,
            child_pid: u32,
            _policy: WorkloadMemoryPolicy,
        ) -> Result<(), WorkloadUnitError> {
            self.calls.lock().unwrap().push(AdapterCall::StartSplint(
                names.dojo_slice.clone(),
                names.splint_scope.clone(),
                child_pid,
            ));
            if *self.fail_scope.lock().unwrap() {
                self.calls
                    .lock()
                    .unwrap()
                    .push(AdapterCall::Stop(names.splint_scope.clone()));
                Err(WorkloadUnitError::IdentityMismatch)
            } else {
                let mut systemd = self.systemd.lock().unwrap();
                if systemd.units.contains_key(&names.splint_scope) {
                    // As the real adapter does, an existing unit is left alone.
                    return Err(unit_exists_error());
                }
                systemd
                    .units
                    .insert(names.splint_scope.clone(), Some(names.dojo_slice.clone()));
                systemd.pids.insert(child_pid, names.splint_scope.clone());
                Ok(())
            }
        }

        fn stop_unit(&self, name: &str) -> Result<(), WorkloadUnitError> {
            self.calls
                .lock()
                .unwrap()
                .push(AdapterCall::Stop(name.to_owned()));
            {
                // Stopping a unit stops its child scopes and their processes.
                let mut systemd = self.systemd.lock().unwrap();
                let mut stopped = vec![name.to_owned()];
                stopped.extend(
                    systemd
                        .units
                        .iter()
                        .filter(|(_, slice)| slice.as_deref() == Some(name))
                        .map(|(scope, _)| scope.clone()),
                );
                systemd.units.retain(|unit, _| !stopped.contains(unit));
                systemd.pids.retain(|_, unit| !stopped.contains(unit));
            }
            let gate = self.stop_gate.lock().unwrap().clone();
            if is_slice_unit(name)
                && let Some(gate) = gate
            {
                let mut state = gate.state.lock().unwrap();
                state.0 = true;
                gate.changed.notify_all();
                while !state.1 {
                    state = gate.changed.wait(state).unwrap();
                }
            }
            Ok(())
        }

        fn shutdown(&self) -> Result<(), WorkloadUnitError> {
            *self.shutdowns.lock().unwrap() += 1;
            Ok(())
        }
    }

    fn identity(pid: u32) -> LinuxPtyIdentity {
        LinuxPtyIdentity::from_raw(pid, pid, pid).unwrap()
    }

    fn is_slice_unit(name: &str) -> bool {
        name.strip_suffix(".slice").is_some()
    }

    fn assert_systemd_unit_absent(manager: &SystemdUserManager, name: &str) {
        assert!(matches!(
            manager.unit_path(name),
            Err(WorkloadUnitError::Request(error)) if is_no_such_unit(&error)
        ));
    }

    fn user_manager_property(unit: Option<&str>, property: &str) -> String {
        let mut command = std::process::Command::new("systemctl");
        command.args(["--user", "show"]);
        if let Some(unit) = unit {
            command.arg(unit);
        }
        let output = command
            .args(["--property", property, "--value"])
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[test]
    fn manager_creation_requires_a_successful_capability_probe() {
        let adapter = Arc::new(FakeAdapter::default());
        *adapter.fail_probe.lock().unwrap() = true;
        assert!(WorkloadUnitManager::new(adapter, WorkloadMemoryPolicy::default()).is_err());
    }

    #[test]
    fn manager_shutdown_closes_the_adapter_once() {
        let adapter = Arc::new(FakeAdapter::default());
        let manager =
            WorkloadUnitManager::new(adapter.clone(), WorkloadMemoryPolicy::default()).unwrap();
        let prepared = manager.prepare(DojoId::new(), SplintId::new(), 1).unwrap();

        manager.shutdown().unwrap();

        assert_eq!(*adapter.shutdowns.lock().unwrap(), 1);
        // Shutdown issues no systemd unit request of its own.
        assert_eq!(adapter.calls().len(), 1);
        drop(prepared);
    }

    #[test]
    fn names_are_stable_unit_safe_and_incarnation_specific() {
        let dojo_id = DojoId::new();
        let splint_id = SplintId::new();
        let first = WorkloadUnitNames::new(dojo_id, splint_id, 7);
        let second = WorkloadUnitNames::new(dojo_id, splint_id, 8);

        assert!(first.dojo_slice.starts_with("app-splinterm-dojo"));
        assert!(is_slice_unit(&first.dojo_slice));
        assert!(first.splint_scope.starts_with("splinterm-splint"));
        assert!(first.splint_scope.ends_with("-i7.scope"));
        assert!(!first.dojo_slice.contains(&dojo_id.to_string()));
        assert_ne!(first.splint_scope, second.splint_scope);
        assert!(
            first
                .dojo_slice
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || ".-".contains(character))
        );
    }

    #[test]
    fn workload_units_inherit_the_user_manager_task_limit() {
        let property_names = |properties: Vec<UnitProperty>| {
            let mut names = properties
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>();
            names.sort_unstable();
            names
        };
        let policy = WorkloadMemoryPolicy::default();
        let names = WorkloadUnitNames::new(DojoId::new(), SplintId::new(), 7);
        assert_eq!(
            property_names(capability_probe_properties(Some("splinterd.service")).unwrap()),
            ["CollectMode", "Description", "MemoryHighScale", "PartOf"]
        );
        assert_eq!(
            property_names(dojo_slice_properties(Some("splinterd.service"), policy).unwrap()),
            ["CollectMode", "Description", "MemoryHighScale", "PartOf"]
        );
        assert_eq!(
            property_names(splint_scope_properties(&names, 123, policy).unwrap()),
            [
                "CollectMode",
                "Description",
                "MemoryHighScale",
                "PIDs",
                "Slice"
            ]
        );
    }

    #[test]
    fn memory_policy_requires_nested_ordering() {
        assert!(WorkloadMemoryPolicy::default().validate().is_ok());
        assert!(
            WorkloadMemoryPolicy {
                splint_memory_high_percent: 50,
                ..WorkloadMemoryPolicy::default()
            }
            .validate()
            .is_err()
        );
        assert_eq!(percent_scale(25), 1_073_741_824);
        assert_eq!(percent_scale(50), 2_147_483_648);
    }

    #[test]
    fn scopes_share_one_dojo_reference_and_last_release_stops_slice() {
        let adapter = Arc::new(FakeAdapter::default());
        let manager =
            WorkloadUnitManager::new(adapter.clone(), WorkloadMemoryPolicy::default()).unwrap();
        let dojo_id = DojoId::new();
        let first_prepared = manager.prepare(dojo_id, SplintId::new(), 1).unwrap();
        let second_prepared = manager.prepare(dojo_id, SplintId::new(), 2).unwrap();
        let first = first_prepared.place(identity(101)).unwrap();
        let second = second_prepared.place(identity(102)).unwrap();

        assert_eq!(
            adapter
                .calls()
                .iter()
                .filter(|call| matches!(call, AdapterCall::StartDojo(_)))
                .count(),
            1
        );
        first.release().unwrap();
        assert_eq!(
            adapter
                .calls()
                .iter()
                .filter(|call| matches!(call, AdapterCall::Stop(name) if is_slice_unit(name)))
                .count(),
            0
        );
        second.release().unwrap();
        assert_eq!(
            adapter
                .calls()
                .iter()
                .filter(|call| matches!(call, AdapterCall::Stop(name) if is_slice_unit(name)))
                .count(),
            1
        );
    }

    #[test]
    fn immediate_reprepare_waits_for_completed_last_slice_stop() {
        let adapter = Arc::new(FakeAdapter::default());
        let manager =
            WorkloadUnitManager::new(adapter.clone(), WorkloadMemoryPolicy::default()).unwrap();
        let dojo_id = DojoId::new();
        let active = manager
            .prepare(dojo_id, SplintId::new(), 1)
            .unwrap()
            .place(identity(104))
            .unwrap();
        let gate = Arc::new(StopGate::default());
        *adapter.stop_gate.lock().unwrap() = Some(Arc::clone(&gate));

        let release = std::thread::spawn(move || active.release().unwrap());
        gate.wait_until_entered();
        let (prepared, received) = mpsc::channel();
        let preparing_manager = manager.clone();
        let prepare = std::thread::spawn(move || {
            prepared
                .send(
                    preparing_manager
                        .prepare(dojo_id, SplintId::new(), 2)
                        .is_ok(),
                )
                .unwrap();
        });
        assert!(received.recv_timeout(Duration::from_millis(50)).is_err());

        gate.release();
        release.join().unwrap();
        assert!(received.recv_timeout(Duration::from_secs(1)).unwrap());
        prepare.join().unwrap();
        assert_eq!(
            adapter
                .calls()
                .iter()
                .filter(|call| matches!(call, AdapterCall::StartDojo(_)))
                .count(),
            2
        );
    }

    #[test]
    fn partial_scope_failure_rolls_back_scope_and_dojo() {
        let adapter = Arc::new(FakeAdapter::default());
        *adapter.fail_scope.lock().unwrap() = true;
        let manager =
            WorkloadUnitManager::new(adapter.clone(), WorkloadMemoryPolicy::default()).unwrap();
        let prepared = manager.prepare(DojoId::new(), SplintId::new(), 1).unwrap();

        assert!(prepared.place(identity(103)).is_err());
        let calls = adapter.calls();
        assert!(matches!(calls[0], AdapterCall::StartDojo(_)));
        let expected_scope = match &calls[1] {
            AdapterCall::StartSplint(_, scope, 103) => scope,
            call => panic!("unexpected scope start call: {call:?}"),
        };
        assert!(matches!(&calls[2], AdapterCall::Stop(name) if name == expected_scope));
        assert!(matches!(&calls[3], AdapterCall::Stop(name) if is_slice_unit(name)));
    }

    fn fake_manager(adapter: &Arc<FakeAdapter>) -> WorkloadUnitManager {
        WorkloadUnitManager::new(adapter.clone(), WorkloadMemoryPolicy::default()).unwrap()
    }

    fn placed(manager: &WorkloadUnitManager, dojo_id: DojoId, pid: u32) -> ActiveWorkloadScope {
        manager
            .prepare(dojo_id, SplintId::new(), u64::from(pid))
            .unwrap()
            .place(identity(pid))
            .unwrap()
    }

    fn dojo_starts(adapter: &FakeAdapter) -> usize {
        adapter
            .calls()
            .iter()
            .filter(|call| matches!(call, AdapterCall::StartDojo(_)))
            .count()
    }

    #[test]
    fn retained_scopes_stop_nothing_and_keep_their_dojo_references() {
        let adapter = Arc::new(FakeAdapter::default());
        let manager = fake_manager(&adapter);
        let dojo_id = DojoId::new();
        let first = placed(&manager, dojo_id, 201);
        let second = placed(&manager, dojo_id, 202);
        let survivor = placed(&manager, dojo_id, 203);
        let survivor_scope = survivor.identity().splint_scope().to_owned();

        let retained = first.retain_for_handoff();
        let second_scope = second.identity().splint_scope().to_owned();
        // The live actor's path discards the identity.
        ProcessPlacement::retain_for_handoff(second);
        assert!(adapter.stops().is_empty());
        assert!(adapter.is_loaded(retained.splint_scope()));
        assert!(adapter.is_loaded(&second_scope));
        assert_eq!(manager.dojo_references(dojo_id), 3);

        // The last live scope leaves the Dojo, but retained processes still
        // occupy its slice.
        survivor.release().unwrap();
        assert_eq!(adapter.stops(), [survivor_scope]);
        assert_eq!(manager.dojo_references(dojo_id), 2);
        assert!(adapter.is_loaded(retained.dojo_slice()));

        drop(retained);
        drop(manager);
        assert_eq!(adapter.stops().len(), 1);
    }

    #[test]
    fn the_manager_names_its_live_dojo_slices_and_splint_scopes() {
        let adapter = Arc::new(FakeAdapter::default());
        let manager = fake_manager(&adapter);
        assert!(manager.dojo_slices().unwrap().is_empty());
        let dojo_id = DojoId::new();
        let placed = placed(&manager, dojo_id, 301);
        assert_eq!(manager.dojo_slices().unwrap(), [dojo_slice_name(dojo_id)]);
        let scope = placed.identity().splint_scope().to_owned();
        assert!(is_splint_scope(&scope), "{scope}");
        for other in [
            "splinterm-splint.slice",
            "splinterm-splintabc.scope",
            "app-splinterm-dojoabc.slice",
            "session-3.scope",
        ] {
            assert!(!is_splint_scope(other), "{other}");
        }
        placed.release().unwrap();
    }

    #[test]
    fn a_fresh_manager_adopts_retained_scopes_with_exact_dojo_counts() {
        let adapter = Arc::new(FakeAdapter::default());
        let old = fake_manager(&adapter);
        let (busy, quiet) = (DojoId::new(), DojoId::new());
        let retained = [
            placed(&old, busy, 211),
            placed(&old, busy, 212),
            placed(&old, busy, 213),
            placed(&old, quiet, 214),
        ]
        .map(ActiveWorkloadScope::retain_for_handoff);
        drop(old);
        let busy_slice = retained[0].dojo_slice().to_owned();
        let quiet_slice = retained[3].dojo_slice().to_owned();

        let fresh = fake_manager(&adapter);
        let adopted = retained
            .iter()
            .map(|scope| fresh.adopt_scope(scope).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(fresh.dojo_references(busy), 3);
        assert_eq!(fresh.dojo_references(quiet), 1);
        assert!(matches!(
            fresh.adopt_scope(&retained[0]),
            Err(WorkloadUnitError::ScopeAlreadyHeld)
        ));
        assert_eq!(fresh.dojo_references(busy), 3);
        assert!(adapter.stops().is_empty());
        assert_eq!(dojo_starts(&adapter), 2);

        let mut adopted = adopted.into_iter();
        adopted.next().unwrap().release().unwrap();
        adopted.next().unwrap().release().unwrap();
        assert_eq!(adapter.slice_stops(&busy_slice), 0);
        adopted.next().unwrap().release().unwrap();
        assert_eq!(adapter.slice_stops(&busy_slice), 1);
        assert_eq!(fresh.dojo_references(busy), 0);
        // Dropping the last adopted scope stops its scope and slice too.
        drop(adopted);
        assert_eq!(adapter.slice_stops(&quiet_slice), 1);
        assert_eq!(adapter.slice_stops(&busy_slice), 1);
        assert_eq!(adapter.stops().len(), 6);
    }

    #[test]
    fn in_process_readoption_transfers_retained_references() {
        let adapter = Arc::new(FakeAdapter::default());
        let manager = fake_manager(&adapter);
        let dojo_id = DojoId::new();
        let live = placed(&manager, dojo_id, 221);
        let retained = [
            placed(&manager, dojo_id, 222),
            placed(&manager, dojo_id, 223),
        ]
        .map(ActiveWorkloadScope::retain_for_handoff);
        assert_eq!(manager.dojo_references(dojo_id), 3);

        // A clone shares the bookkeeping, as the daemon state's clones do.
        let resumed = manager.clone();
        let adopted = retained
            .iter()
            .map(|scope| resumed.adopt_scope(scope).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(manager.dojo_references(dojo_id), 3);
        assert!(matches!(
            resumed.adopt_scope(&retained[0]),
            Err(WorkloadUnitError::ScopeAlreadyHeld)
        ));
        assert!(matches!(
            resumed.adopt_scope(live.identity()),
            Err(WorkloadUnitError::ScopeAlreadyHeld)
        ));
        assert_eq!(manager.dojo_references(dojo_id), 3);

        // A second retain and re-adopt round trip does not drift either.
        let again = adopted
            .into_iter()
            .map(ActiveWorkloadScope::retain_for_handoff)
            .collect::<Vec<_>>();
        assert_eq!(again, retained);
        let adopted = again
            .iter()
            .map(|scope| manager.adopt_scope(scope).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(manager.dojo_references(dojo_id), 3);
        assert!(adapter.stops().is_empty());
        assert_eq!(dojo_starts(&adapter), 1);

        live.release().unwrap();
        drop(adopted);
        assert_eq!(manager.dojo_references(dojo_id), 0);
        assert_eq!(adapter.slice_stops(retained[0].dojo_slice()), 1);
    }

    #[test]
    fn prepare_never_stops_a_slice_a_previous_generation_left() {
        let adapter = Arc::new(FakeAdapter::default());
        let old = fake_manager(&adapter);
        let dojo_id = DojoId::new();
        let retained = placed(&old, dojo_id, 241).retain_for_handoff();
        drop(old);

        // Until the leftover slice is adopted, every prepare into its Dojo
        // fails on the existing unit and stops nothing.
        let fresh = fake_manager(&adapter);
        for _ in 0..3 {
            let Err(error) = fresh.prepare(dojo_id, SplintId::new(), 1) else {
                panic!("prepare into a leftover slice must fail");
            };
            assert!(is_unit_exists(&error));
            assert_eq!(fresh.dojo_references(dojo_id), 0);
        }
        assert!(adapter.stops().is_empty());
        assert!(adapter.is_loaded(retained.dojo_slice()));
        assert!(adapter.is_loaded(retained.splint_scope()));

        let adopted = fresh.adopt_scope(&retained).unwrap();
        assert_eq!(fresh.dojo_references(dojo_id), 1);
        let sibling = placed(&fresh, dojo_id, 242);
        assert_eq!(fresh.dojo_references(dojo_id), 2);
        assert!(adapter.stops().is_empty());

        adopted.release().unwrap();
        sibling.release().unwrap();
        assert_eq!(adapter.slice_stops(retained.dojo_slice()), 1);
    }

    #[test]
    fn only_a_provably_empty_leftover_slice_is_reclaimed() {
        let adapter = Arc::new(FakeAdapter::default());
        let old = fake_manager(&adapter);
        let (emptied, occupied, counted) = (DojoId::new(), DojoId::new(), DojoId::new());
        let exited = placed(&old, emptied, 251).retain_for_handoff();
        let running = placed(&old, occupied, 252).retain_for_handoff();
        let adoptable = placed(&old, counted, 253).retain_for_handoff();
        drop(old);
        // The first Dojo's only carried Splint exited: systemd collected its
        // empty scope, and its slice stays loaded with no process.
        {
            let mut systemd = adapter.systemd.lock().unwrap();
            systemd.units.remove(exited.splint_scope());
            systemd.pids.remove(&251);
        }

        let fresh = fake_manager(&adapter);
        let _adopted = fresh.adopt_scope(&adoptable).unwrap();
        assert_eq!(
            fresh.reclaim_empty_dojo_slice(counted).unwrap(),
            DojoSliceReclaim::Referenced
        );
        assert_eq!(
            fresh.reclaim_empty_dojo_slice(occupied).unwrap(),
            DojoSliceReclaim::Occupied
        );
        assert_eq!(
            fresh.reclaim_empty_dojo_slice(DojoId::new()).unwrap(),
            DojoSliceReclaim::Absent
        );
        assert!(adapter.stops().is_empty());
        assert!(adapter.is_loaded(running.splint_scope()));
        // A process no generation placed has neither unit; one in a loaded
        // scope does not qualify.
        assert!(
            fresh
                .retained_units_absent(&RetainedWorkloadScope::new(
                    DojoId::new(),
                    SplintId::new(),
                    1,
                    259
                ))
                .unwrap()
        );
        assert!(!fresh.retained_units_absent(&running).unwrap());

        assert_eq!(
            fresh.reclaim_empty_dojo_slice(emptied).unwrap(),
            DojoSliceReclaim::Stopped
        );
        assert_eq!(adapter.stops(), [exited.dojo_slice()]);
        // The reclaimed Dojo launches again instead of failing on UnitExists.
        let relaunched = placed(&fresh, emptied, 254);
        assert_eq!(fresh.dojo_references(emptied), 1);
        drop(relaunched);
    }

    #[test]
    fn adoption_fails_closed_without_stopping_anything() {
        let adapter = Arc::new(FakeAdapter::default());
        let old = fake_manager(&adapter);
        let dojo_id = DojoId::new();
        let retained = placed(&old, dojo_id, 231).retain_for_handoff();
        let other = placed(&old, dojo_id, 232).retain_for_handoff();
        let fresh = fake_manager(&adapter);
        let mismatch = |scope: &RetainedWorkloadScope| {
            matches!(
                fresh.adopt_scope(scope),
                Err(WorkloadUnitError::RetainedScopeMismatch)
            )
        };
        let set_pid_unit = |unit: Option<&str>| {
            let pids = &mut adapter.systemd.lock().unwrap().pids;
            match unit {
                Some(unit) => pids.insert(231, unit.to_owned()),
                None => pids.remove(&231),
            };
        };
        let splint_id = retained.splint_id();
        let incarnation = retained.incarnation();

        // The process is in another Splint's scope.
        set_pid_unit(Some(other.splint_scope()));
        assert!(mismatch(&retained));
        // The process exited, so systemd has no unit for it.
        set_pid_unit(None);
        assert!(mismatch(&retained));
        set_pid_unit(Some(retained.splint_scope()));
        // Invalid PIDs never reach GetUnitByPID, which reads 0 as the caller.
        for pid in [0, u32::MAX] {
            assert!(mismatch(&RetainedWorkloadScope::new(
                dojo_id,
                splint_id,
                incarnation,
                pid
            )));
        }
        // Another Dojo names another slice than the scope runs in.
        assert!(mismatch(&RetainedWorkloadScope::new(
            DojoId::new(),
            splint_id,
            incarnation,
            231
        )));
        // Another incarnation names a scope that is not loaded.
        assert!(mismatch(&RetainedWorkloadScope::new(
            dojo_id,
            splint_id,
            incarnation + 1,
            231
        )));
        // The Dojo slice is gone.
        adapter
            .systemd
            .lock()
            .unwrap()
            .units
            .remove(retained.dojo_slice());
        assert!(mismatch(&retained));

        assert!(adapter.stops().is_empty());
        assert_eq!(fresh.dojo_references(dojo_id), 0);
        assert!(adapter.is_loaded(retained.splint_scope()));
        assert!(adapter.is_loaded(other.splint_scope()));
    }

    #[test]
    #[ignore = "requires a running systemd user manager and creates disposable transient units"]
    fn systemd_retained_scope_survives_and_is_adopted_by_a_fresh_manager() {
        let old = WorkloadUnitManager::connect(WorkloadMemoryPolicy::default(), None).unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        let retained = old
            .prepare(DojoId::new(), SplintId::new(), 1)
            .unwrap()
            .place(identity(pid))
            .unwrap()
            .retain_for_handoff();
        old.shutdown().unwrap();

        let fresh = WorkloadUnitManager::connect(WorkloadMemoryPolicy::default(), None).unwrap();
        let elsewhere = RetainedWorkloadScope::new(
            DojoId::new(),
            retained.splint_id(),
            retained.incarnation(),
            pid,
        );
        assert!(matches!(
            fresh.adopt_scope(&elsewhere),
            Err(WorkloadUnitError::RetainedScopeMismatch)
        ));
        let adopted = fresh.adopt_scope(&retained).unwrap();
        assert!(child.try_wait().unwrap().is_none());

        adopted.release().unwrap();
        child.wait().unwrap();
        fresh.shutdown().unwrap();
        let systemd = SystemdUserManager::connect(None).unwrap();
        assert_systemd_unit_absent(&systemd, retained.splint_scope());
        assert_systemd_unit_absent(&systemd, retained.dojo_slice());
        systemd.shutdown().unwrap();
    }

    #[test]
    #[ignore = "requires a running systemd user manager and creates disposable transient units"]
    fn systemd_adapter_moves_and_cleans_up_a_disposable_child() {
        let manager = WorkloadUnitManager::connect(
            WorkloadMemoryPolicy::default(),
            Some("splinterd.service"),
        )
        .unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        let prepared = manager.prepare(DojoId::new(), SplintId::new(), 1).unwrap();
        let names = prepared.names.clone();
        let active = prepared.place(identity(pid)).unwrap();

        assert_eq!(
            user_manager_property(Some(&names.splint_scope), "TasksMax"),
            user_manager_property(None, "DefaultTasksMax")
        );

        active.release().unwrap();
        child.wait().unwrap();
        manager.shutdown().unwrap();
        let systemd = SystemdUserManager::connect(None).unwrap();
        assert_systemd_unit_absent(&systemd, &names.splint_scope);
        assert_systemd_unit_absent(&systemd, &names.dojo_slice);
        systemd.shutdown().unwrap();
    }

    #[test]
    #[ignore = "requires a running systemd user manager and creates disposable transient units"]
    fn pty_target_executes_only_inside_verified_splint_scope() {
        use std::{io::ErrorKind, time::Instant};

        use splinterm_pty::{LinuxPtyBackend, PtyCommand, PtyError, PtySize};

        let test_binary = std::env::current_exe().unwrap();
        let helper = test_binary
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("splinterm-pty-child");
        assert!(helper.is_file(), "missing PTY helper: {}", helper.display());
        let manager = WorkloadUnitManager::connect(WorkloadMemoryPolicy::default(), None).unwrap();
        let prepared = manager.prepare(DojoId::new(), SplintId::new(), 1).unwrap();
        let names = prepared.names.clone();
        let command = PtyCommand::new("/bin/sh", "/tmp").args([
            "-c",
            "printf 'PARENT\\n'; cat /proc/self/cgroup; sleep 0.2 & child=$!; \
             printf 'CHILD\\n'; cat /proc/$child/cgroup; wait; printf 'DONE\\n'",
        ]);
        let (mut session, active) = LinuxPtyBackend::new(helper)
            .spawn_with_placement(&command, PtySize::cells(80, 24), move |identity| {
                prepared.place(identity)
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        let mut buffer = [0_u8; 4096];
        while !String::from_utf8_lossy(&output).contains("DONE") {
            match session.read(&mut buffer) {
                Ok(count) => output.extend_from_slice(&buffer[..count]),
                Err(PtyError::Io { source, .. })
                    if matches!(
                        source.kind(),
                        ErrorKind::WouldBlock | ErrorKind::Interrupted
                    ) => {}
                Err(error) => panic!("failed reading PTY: {error}"),
            }
            assert!(Instant::now() < deadline, "timed out reading cgroup path");
            thread::sleep(Duration::from_millis(10));
        }

        assert!(
            String::from_utf8_lossy(&output)
                .matches(&names.splint_scope)
                .count()
                >= 2
        );
        assert!(session.wait().unwrap().success());
        active.release().unwrap();
        let systemd = SystemdUserManager::connect(None).unwrap();
        assert_systemd_unit_absent(&systemd, &names.splint_scope);
        assert_systemd_unit_absent(&systemd, &names.dojo_slice);
    }
}
