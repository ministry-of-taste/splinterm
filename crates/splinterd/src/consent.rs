//! Trusted, daemon-owned authorization state and the private consent-client broker.
//!
//! The daemon never creates graphical objects. A one-use random capability and
//! bounded prompt cross only a private inherited socket connected to
//! `splinterm consent`; neither is exposed through argv, environment, or logs.

use std::{
    collections::{BTreeSet, VecDeque},
    fs,
    io::{Read, Write},
    os::{
        fd::{AsFd, AsRawFd, OwnedFd},
        unix::{fs::MetadataExt, net::UnixStream as StdUnixStream},
    },
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use nix::{
    poll::{PollFd, PollFlags, PollTimeout, poll},
    sys::socket::{getsockopt, sockopt::PeerPidfd},
};
use splinterd::{
    child_stderr::{STDERR_TAIL_WAIT, StderrTail, child_failure},
    install_prefix::InstallPrefix,
};
use splinterm_core::{LairId, SplintId};
use splinterm_protocol::{
    AccessGrant, AccessGrantSource, AccessLevel, AccessScope, AuditPeer, CONSENT_CAPABILITY_BYTES,
    ConsentDecision, ConsentDurationOffer, ConsentPrompt, ConsentReply, ConsentTarget,
    GrantDuration, LairAccessGrant, MAX_ACCESS_SCOPES, MAX_CONSENT_FRAME_BYTES,
};
use tokio::{io::unix::AsyncFd, net::UnixStream};

use crate::{
    agent_access::{DurableEvidence, GrantIds, IdentityContext},
    executable_identity::ExecutableIdentity,
};

pub const GRANT_LIFETIME: Duration = Duration::from_secs(5 * 60);
const CONSENT_DEADLINE: Duration = Duration::from_secs(20);
const MAX_AUDIT_RECORDS: usize = 256;

/// The sealed forward client snapshot a daemon adopted from a handoff
/// verified against its package record, held open and pinned for the
/// daemon's lifetime (AGENTS.md §4.4, the operator exception of 2026-10-18;
/// Plan 0061 M3.14).
///
/// A Window relaunched across the adoption executes exactly this file, so
/// its process's executable is this snapshot's device and inode. Holding the
/// descriptor keeps the inode alive, so no other file can take its identity
/// while the daemon runs. Matching is by device and inode only, never by
/// content hash: a different file with the same bytes is not this snapshot.
/// A fresh daemon and a rollback daemon pin nothing.
#[derive(Debug)]
pub struct PinnedClientSnapshot {
    _snapshot: OwnedFd,
    device: u64,
    inode: u64,
}

impl PinnedClientSnapshot {
    /// Pins `snapshot`, which must be a regular file sealed immutable
    /// ([`splinterm_protocol::handoff_prepare::REQUIRED_CLIENT_SNAPSHOT_SEALS`]:
    /// write, grow, shrink, and the seal set itself), so the sealed half of
    /// the §4.4 exception holds at the pin itself.
    ///
    /// # Errors
    /// A descriptor that cannot be examined, is not a regular file, or lacks
    /// a required seal.
    pub fn pin(snapshot: OwnedFd) -> std::io::Result<Self> {
        let status = rustix::fs::fstat(&snapshot)?;
        if rustix::fs::FileType::from_raw_mode(status.st_mode) != rustix::fs::FileType::RegularFile
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the forward client snapshot is not a regular file",
            ));
        }
        // A regular file without sealing support reports EINVAL: not sealed.
        let sealed = rustix::fs::fcntl_get_seals(&snapshot).is_ok_and(|seals| {
            seals.contains(splinterm_protocol::handoff_prepare::REQUIRED_CLIENT_SNAPSHOT_SEALS)
        });
        if !sealed {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the forward client snapshot is not sealed immutable",
            ));
        }
        Ok(Self {
            _snapshot: snapshot,
            device: status.st_dev,
            inode: status.st_ino,
        })
    }

    /// The pinned file's device and inode.
    pub const fn identity(&self) -> (u64, u64) {
        (self.device, self.inode)
    }

    /// This test process's own executable, held open as if it were the
    /// pinned snapshot, so a peer that is this process matches it.
    #[cfg(test)]
    pub fn own_executable_for_test() -> Self {
        let executable = fs::File::open("/proc/self/exe").expect("open the test executable");
        let status = rustix::fs::fstat(&executable).expect("identify the test executable");
        Self {
            _snapshot: OwnedFd::from(executable),
            device: status.st_dev,
            inode: status.st_ino,
        }
    }
}

impl PartialEq for PinnedClientSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.identity() == other.identity()
    }
}

impl Eq for PinnedClientSnapshot {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerIdentity {
    pub uid: u32,
    pub pid: u32,
    /// The requester label shown in consent dialogs: the kernel's
    /// `/proc/<pid>/exe` link text, marked unless it resolves here to the
    /// executing file. Display only; never an identity.
    executable_label: String,
    /// Device and inode of the file object the peer executes, from the opened
    /// executable link itself.
    executable_device: u64,
    executable_inode: u64,
    /// The daemon's installed-prefix authority, attached at verification.
    /// It is not a property of the peer: adjacency is matched against it.
    install_prefix: Arc<InstallPrefix>,
    /// The forward client snapshot an adopted daemon pinned, attached at
    /// verification; `None` in every other daemon.
    pinned_client: Option<Arc<PinnedClientSnapshot>>,
    persistent_executable: Option<ExecutableIdentity>,
    /// The peer's start time in clock ticks since boot, recorded when the
    /// connection is verified, so a reused PID is never mistaken for it.
    start_time: Option<u64>,
    /// What this connection established about durable identity (ADR 0014),
    /// read while its pidfd pinned it; `None` without a pinned executable.
    durable: Option<DurableEvidence>,
    /// Whether this connection's peer was the trusted graphical client at
    /// its `Hello` (AGENTS.md §4.4), and the executable it then ran.
    hello_client: HelloClientMatch,
}

/// A connection's `Hello`-time client match (AGENTS.md §4.4, the operator
/// decision of 2026-10-20): the connection keeps it only while its own
/// process still executes the file it executed at `Hello`, so a package
/// upgrade replacing the installed client leaves running Windows trusted.
///
/// Connection-local state, not peer identity: it never takes part in
/// [`PeerIdentity`] equality, so grants match a process across its
/// connections exactly as before.
#[derive(Clone, Debug, Default)]
struct HelloClientMatch(Option<Arc<HelloClient>>);

impl PartialEq for HelloClientMatch {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for HelloClientMatch {}

#[derive(Debug)]
struct HelloClient {
    /// The peer process and the file it executed when it matched.
    process: TrustedClientProcess,
    /// The connection's pinned peer process; `None` only on a kernel without
    /// `SO_PEERPIDFD`, where the recorded start time stands in for it.
    pidfd: Option<OwnedFd>,
}

/// `statfs` magic of pidfs (Linux 6.9+), where a pidfd's inode number names
/// its process uniquely for the boot.
const PIDFS_MAGIC: u64 = 0x5049_4446;

/// The most client processes [`TrustedClientProcesses`] tracks at once.
const MAX_TRUSTED_CLIENT_PROCESSES: usize = 256;

/// A client process by every identity the daemon can read for it, and the
/// file it executes: its PID, its start time, its pidfs inode where the
/// kernel has pidfs, and its executable's device and inode.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct TrustedClientProcess {
    pid: u32,
    start_time: Option<u64>,
    pidfs_inode: Option<u64>,
    executable: (u64, u64),
}

impl TrustedClientProcess {
    /// Whether this names one process, not merely a PID that a later
    /// process could reuse.
    const fn identifies_the_process(&self) -> bool {
        self.start_time.is_some() || self.pidfs_inode.is_some()
    }
}

/// The pidfs inode of the process `pidfd` names; `None` before pidfs, when
/// every pidfd shares one anonymous inode.
fn pidfs_inode(pidfd: &OwnedFd) -> Option<u64> {
    let filesystem = rustix::fs::fstatfs(pidfd).ok()?;
    if u64::try_from(filesystem.f_type).ok() != Some(PIDFS_MAGIC) {
        return None;
    }
    rustix::fs::fstat(pidfd).ok().map(|status| status.st_ino)
}

/// The live client processes that hold a trusted-UI connection trusted at its
/// own `Hello` (AGENTS.md §4.4, the operator decision of 2026-10-20), counted
/// by connection. A new connection from the same process, still executing the
/// same file, is trusted at `Hello` too, so a Window opened before an upgrade
/// replaced the installed client still opens panes and tabs.
///
/// An entry ends with its process's last trusted connection, so trust never
/// outlives every connection of the process, and a process that exits takes
/// its connections, and so its entry, with it. Bounded: past
/// [`MAX_TRUSTED_CLIENT_PROCESSES`] processes, a further one is not tracked
/// and its later connections need the installed client.
#[derive(Clone, Debug, Default)]
pub struct TrustedClientProcesses(
    Arc<std::sync::Mutex<std::collections::HashMap<TrustedClientProcess, usize>>>,
);

impl TrustedClientProcesses {
    fn holds(&self, process: &TrustedClientProcess) -> bool {
        process.identifies_the_process()
            && self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_key(process)
    }

    fn register(&self, process: TrustedClientProcess) -> Option<TrustedClientRegistration> {
        if !process.identifies_the_process() {
            return None;
        }
        let mut processes = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !processes.contains_key(&process) && processes.len() >= MAX_TRUSTED_CLIENT_PROCESSES {
            tracing::warn!("trusted client process registry full; process not tracked");
            return None;
        }
        *processes.entry(process).or_default() += 1;
        Some(TrustedClientRegistration {
            processes: self.clone(),
            process,
        })
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

/// One trusted-UI connection's count in [`TrustedClientProcesses`], held for
/// the connection's lifetime and released when it ends.
#[derive(Debug)]
pub struct TrustedClientRegistration {
    processes: TrustedClientProcesses,
    process: TrustedClientProcess,
}

impl Drop for TrustedClientRegistration {
    fn drop(&mut self) {
        let mut processes = self
            .processes
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(count) = processes.get_mut(&self.process) {
            *count -= 1;
            if *count == 0 {
                processes.remove(&self.process);
            }
        }
    }
}

impl PeerIdentity {
    pub fn from_stream(
        stream: &UnixStream,
        install_prefix: Arc<InstallPrefix>,
        pinned_client: Option<Arc<PinnedClientSnapshot>>,
    ) -> Result<Self> {
        let credentials = stream.peer_cred().context("cannot read peer credentials")?;
        let pid = credentials.pid().context("peer credentials omit pid")?;
        let pid = u32::try_from(pid).context("peer pid is negative")?;
        let (executable_device, executable_inode) = executable_file_identity(pid)?;
        let link_text =
            fs::read_link(format!("/proc/{pid}/exe")).context("cannot label peer executable")?;
        Ok(Self {
            uid: credentials.uid(),
            pid,
            executable_label: executable_label(&link_text, executable_device, executable_inode),
            executable_device,
            executable_inode,
            install_prefix,
            pinned_client,
            persistent_executable: None,
            start_time: None,
            durable: None,
            hello_client: HelloClientMatch::default(),
        })
    }

    pub fn requester_label(&self) -> String {
        self.executable_label.clone()
    }

    #[cfg(test)]
    pub fn for_test() -> Self {
        let executable = std::env::current_exe().expect("test executable");
        let metadata = fs::metadata(&executable).expect("test executable metadata");
        Self {
            uid: rustix::process::geteuid().as_raw(),
            pid: std::process::id(),
            executable_label: executable_label(&executable, metadata.dev(), metadata.ino()),
            executable_device: metadata.dev(),
            executable_inode: metadata.ino(),
            install_prefix: Arc::new(test_install_prefix()),
            pinned_client: None,
            persistent_executable: None,
            start_time: None,
            durable: None,
            hello_client: HelloClientMatch::default(),
        }
    }

    /// This test process as a peer executing `executable` (its file by
    /// device and inode) with `durable` evidence, as a pinned connection
    /// verification records it.
    #[cfg(test)]
    pub fn for_test_executing(executable: ExecutableIdentity, durable: DurableEvidence) -> Self {
        let mut peer = Self::for_test();
        peer.executable_device = executable.device;
        peer.executable_inode = executable.inode;
        peer.persistent_executable = Some(executable);
        peer.durable = Some(durable);
        peer
    }

    /// This test process as a peer in a daemon that pinned `pinned`, with
    /// its start time recorded as connection verification records it.
    #[cfg(test)]
    pub fn for_test_pinned(pinned: Arc<PinnedClientSnapshot>) -> Self {
        let mut peer = Self::for_test();
        peer.pinned_client = Some(pinned);
        peer.start_time = splinterd::handoff_window_carry::process_start_time(std::process::id());
        peer
    }

    pub fn install_persistent_executable(&mut self, executable: ExecutableIdentity) {
        self.persistent_executable = Some(executable);
    }

    pub fn persistent_executable(&self) -> Option<&ExecutableIdentity> {
        self.persistent_executable.as_ref()
    }

    /// Records the durable-identity evidence its pinned verification read,
    /// marking whether the peer executes the installed relay. The relay is
    /// recognized by either executable snapshot this connection took (the
    /// file the stream's peer executed, or the one the pinned identity
    /// hashed), so a file replaced between the two cannot hide it.
    pub fn install_durable_evidence(&mut self, mut durable: DurableEvidence) {
        let pinned_is_relay = self
            .persistent_executable
            .as_ref()
            .is_some_and(|executable| {
                fs::metadata(self.install_prefix.graphical_relay()).is_ok_and(|metadata| {
                    metadata.dev() == executable.device && metadata.ino() == executable.inode
                })
            });
        durable.installed_relay = self.is_installed_relay() || pinned_is_relay;
        self.durable = Some(durable);
    }

    /// The durable-identity evidence of this connection; `None` without a
    /// pidfd-pinned executable.
    pub fn durable_evidence(&self) -> Option<&DurableEvidence> {
        self.durable.as_ref()
    }

    /// Whether the peer executes the installed `splinterm-relay`, by device
    /// and inode, in any mode. Never eligible for the longer durations.
    pub fn is_installed_relay(&self) -> bool {
        self.matches_installed_executable(&self.install_prefix.graphical_relay())
    }

    pub fn install_start_time(&mut self, start_time: u64) {
        self.start_time = Some(start_time);
    }

    pub const fn start_time(&self) -> Option<u64> {
        self.start_time
    }

    /// Device and inode of the file the peer executed when its connection
    /// was verified. A package upgrade leaves a running client on this inode,
    /// so it keeps naming the image the peer actually runs.
    pub const fn executable_file(&self) -> (u64, u64) {
        (self.executable_device, self.executable_inode)
    }

    pub fn audit_peer(&self) -> Option<AuditPeer> {
        let executable = self.persistent_executable.as_ref()?;
        Some(AuditPeer {
            uid: self.uid,
            executable_path: executable
                .path
                .to_string_lossy()
                .chars()
                .take(4096)
                .collect(),
            executable_sha256: executable.sha256.clone(),
            device: Some(executable.device),
            inode: Some(executable.inode),
        })
    }

    pub fn transfer_peer(&self) -> Option<splinterd::image_transport::TransferPeer> {
        self.persistent_executable.as_ref().map(|executable| {
            splinterd::image_transport::TransferPeer {
                uid: self.uid,
                pid: self.pid,
                executable_device: executable.device,
                executable_inode: executable.inode,
                executable_sha256: executable.sha256.clone(),
            }
        })
    }

    /// The `Hello`-time client test: whether the peer is a trusted graphical
    /// client under AGENTS.md §4.4, executing exactly the file now installed
    /// as `<prefix>/splinterm` (same device and inode), or, in a daemon
    /// adopted through a verified handoff, exactly the sealed forward client
    /// snapshot that daemon verified against its package record and pinned
    /// for its lifetime (same device and inode; the operator exception of
    /// 2026-10-18).
    ///
    /// Matching is never by content hash. A fresh or rollback daemon pins no
    /// snapshot, so only the installed rule applies there. A new connection
    /// from a client still running a replaced or unlinked image, a stale
    /// pre-upgrade inode among them, matches neither. Requests on an
    /// established connection use [`Self::connection_matches_splinterm`].
    pub fn is_matching_splinterm(&self) -> bool {
        self.matches_installed_executable(&self.install_prefix.client())
            || self.matches_pinned_client()
    }

    /// Records, at this connection's `Hello`, whether its peer is the
    /// trusted graphical client, with the process and executable it then ran
    /// and a duplicate of the connection's pidfd.
    ///
    /// The peer matches when it executes the installed client or the pinned
    /// snapshot ([`Self::is_matching_splinterm`]), or when `trusted` holds
    /// this very process, executing this very file, from another live
    /// connection that matched at its own `Hello` (the operator decision of
    /// 2026-10-20). A pidfd that cannot be duplicated records no match: fail
    /// closed. The caller still applies [`Self::connection_matches_splinterm`],
    /// which re-reads the executable and proves the process live.
    pub fn record_hello_client_match(
        &mut self,
        monitor: Option<&PeerMonitor>,
        trusted: &TrustedClientProcesses,
    ) {
        self.record_hello_client_match_with(
            || monitor.map(PeerMonitor::duplicate_pidfd).transpose(),
            trusted,
        );
    }

    fn record_hello_client_match_with(
        &mut self,
        pidfd: impl FnOnce() -> Result<Option<OwnedFd>>,
        trusted: &TrustedClientProcesses,
    ) {
        self.hello_client = HelloClientMatch::default();
        let pidfd = match pidfd() {
            Ok(pidfd) => pidfd,
            Err(error) => {
                tracing::warn!(%error, "trusted client match not recorded: peer pidfd unavailable");
                return;
            }
        };
        let process = TrustedClientProcess {
            pid: self.pid,
            start_time: self.start_time,
            pidfs_inode: pidfd.as_ref().and_then(pidfs_inode),
            executable: (self.executable_device, self.executable_inode),
        };
        if self.is_matching_splinterm() || trusted.holds(&process) {
            self.hello_client = HelloClientMatch(Some(Arc::new(HelloClient { process, pidfd })));
        }
    }

    /// Counts this connection in `trusted` for its lifetime, when it matched
    /// at `Hello`, so later connections from its process match too. `None`
    /// when it did not match, its process cannot be told from a later one
    /// reusing its PID, or the registry is full.
    pub fn register_trusted_client(
        &self,
        trusted: &TrustedClientProcesses,
    ) -> Option<TrustedClientRegistration> {
        let hello = self.hello_client.0.as_deref()?;
        trusted.register(hello.process)
    }

    /// Whether this connection is the trusted graphical client under
    /// AGENTS.md §4.4 (the operator decision of 2026-10-20): its peer matched
    /// at `Hello` ([`Self::record_hello_client_match`]), and the same process
    /// still executes that same file, by device and inode, read afresh
    /// through `/proc/<pid>/exe` for every call.
    ///
    /// A package upgrade that replaces the installed client therefore leaves
    /// an existing Window's connections trusted, while a peer that executed
    /// another program over its socket is not, and a new connection from
    /// another process running the old inode never matched at `Hello`. Fails
    /// closed when the peer's current executable cannot be read, or its
    /// process can no longer be shown to be the one that said `Hello`.
    pub fn connection_matches_splinterm(&self) -> bool {
        let Some(hello) = self.hello_client.0.as_deref() else {
            return false;
        };
        let Ok(current) = executable_file_identity(self.pid) else {
            return false;
        };
        if current != hello.process.executable {
            return false;
        }
        // Liveness after the read proves it described this connection's own
        // process, not a later one reusing its PID.
        match &hello.pidfd {
            Some(pidfd) => require_live_pidfd(pidfd).is_ok(),
            None => self.start_time.is_some_and(|start_time| {
                splinterd::handoff_window_carry::process_start_time(self.pid) == Some(start_time)
            }),
        }
    }

    /// Whether the peer executes the pinned forward client snapshot.
    fn matches_pinned_client(&self) -> bool {
        self.pinned_client.as_ref().is_some_and(|pinned| {
            pinned.identity() == (self.executable_device, self.executable_inode)
        })
    }

    pub fn is_matching_graphical_relay(&self) -> bool {
        let executable_matches =
            self.matches_installed_executable(&self.install_prefix.graphical_relay());
        let command_line = fs::read(format!("/proc/{}/cmdline", self.pid));
        let (argument_count, graphical_mode) = command_line.as_ref().map_or((0, false), |value| {
            let arguments = value
                .split(|byte| *byte == 0)
                .filter(|argument| !argument.is_empty())
                .collect::<Vec<_>>();
            (arguments.len(), graphical_relay_arguments(&arguments))
        });
        tracing::debug!(
            peer_pid = self.pid,
            executable_matches,
            command_line_available = command_line.is_ok(),
            argument_count,
            graphical_mode,
            "graphical relay identity evaluated"
        );
        executable_matches && graphical_mode
    }

    // The file installed at `expected` now, by device and inode. The pinned
    // forward client snapshot is matched separately and only for the
    // graphical client ([`Self::matches_pinned_client`]); it never widens
    // the graphical relay's rule.
    fn matches_installed_executable(&self, expected: &Path) -> bool {
        fs::metadata(expected).is_ok_and(|metadata| {
            metadata.dev() == self.executable_device && metadata.ino() == self.executable_inode
        })
    }
}

/// Identifies the file object process `pid` executes: its device and inode.
///
/// The executable link is opened (`O_PATH`) and the descriptor itself is
/// examined, so the kernel resolves the link to the exact executing file even
/// when it is a memfd, unlinked, or replaced on disk. The link's path text is
/// never resolved: it may name nothing, or a different file now installed at
/// that path, or a file in another mount namespace. `O_PATH` needs no read
/// permission on the file, as the `stat` this replaces did not.
///
/// Fails closed when the link cannot be opened or examined, or the executing
/// object is not a regular file.
pub fn executable_file_identity(pid: u32) -> Result<(u64, u64)> {
    let link = format!("/proc/{pid}/exe");
    let executable = rustix::fs::open(
        link.as_str(),
        rustix::fs::OFlags::PATH | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .context("cannot open peer executable")?;
    let status = rustix::fs::fstat(&executable).context("cannot identify peer executable")?;
    if rustix::fs::FileType::from_raw_mode(status.st_mode) != rustix::fs::FileType::RegularFile {
        bail!("peer executable is not a regular file");
    }
    Ok((status.st_dev, status.st_ino))
}

/// The longest requester label, in characters, markers included.
const MAX_REQUESTER_LABEL_CHARS: usize = 1024;

/// Labels a peer executable for a human from its link text, never presenting
/// an unverified path as fact.
///
/// The text is shown plain only when it resolves, in the daemon's mount
/// namespace, to the executing file's device and inode. Otherwise (a memfd
/// image, a deleted or replaced file, or a path that names another file here,
/// such as a binary in another mount namespace) it carries a marker naming
/// the executing file by device and inode. The marker always survives the
/// length bound.
fn executable_label(link_text: &Path, device: u64, inode: u64) -> String {
    let verified = link_text.is_absolute()
        && fs::metadata(link_text)
            .is_ok_and(|metadata| metadata.dev() == device && metadata.ino() == inode);
    let text = link_text.to_string_lossy();
    let marker = if verified {
        String::new()
    } else {
        let kind = if text.starts_with("/memfd:") {
            "memory image, "
        } else if text.ends_with(" (deleted)") {
            "deleted or replaced file, "
        } else {
            ""
        };
        format!(" [{kind}path unverified; executable device {device} inode {inode}]")
    };
    let budget = MAX_REQUESTER_LABEL_CHARS.saturating_sub(marker.chars().count());
    let mut label = text.chars().take(budget).collect::<String>();
    label.push_str(&marker);
    label
}

/// A prefix for unit tests: the test binary's own directory, which holds no
/// `splinterm`, so no test peer is a trusted client.
#[cfg(test)]
pub fn test_install_prefix() -> InstallPrefix {
    let test_binary = std::env::current_exe().expect("test executable");
    let directory = fs::canonicalize(test_binary.parent().expect("test binary directory"))
        .expect("canonical test binary directory");
    InstallPrefix::from_directory(&directory).expect("test install prefix")
}

fn graphical_relay_arguments(arguments: &[&[u8]]) -> bool {
    arguments.len() == 2 && arguments[1] == b"--graphical-stdio"
}

#[derive(Debug)]
pub struct PeerMonitor {
    pidfd: AsyncFd<OwnedFd>,
}

/// What a pidfd-pinned peer verification established.
#[derive(Debug)]
pub enum PeerPinning {
    /// The kernel predates `SO_PEERPIDFD` (Linux 6.5): nothing pins the peer,
    /// so persistent identity, and with it policy authorization, is
    /// unavailable.
    Unsupported,
    /// The peer was live at a point after every identity read, so its PID was
    /// not reused meanwhile. The persistent snapshot is `Err` when the
    /// executable has no bindable path (a memfd, deleted, or replaced image),
    /// which disables only policy authorization.
    Pinned {
        monitor: PeerMonitor,
        executable: Result<ExecutableIdentity>,
        /// Durable-identity evidence for the executable, read under the same
        /// pin; `None` when the executable has no bindable path.
        durable: Option<DurableEvidence>,
    },
}

impl PeerMonitor {
    /// Pins the connected peer with its `SO_PEERPIDFD` and snapshots its
    /// persistent executable identity.
    ///
    /// Every identity read for this connection, including the earlier
    /// [`PeerIdentity::from_stream`], happened before the final liveness
    /// check, so a live pidfd proves those reads described this peer.
    ///
    /// # Errors
    /// Fails closed, and the caller rejects the connection, when the pidfd
    /// names a different process than `SO_PEERCRED`, cannot be inspected, or
    /// shows the peer has exited (its PID may already be reused), and when
    /// `SO_PEERPIDFD` fails for any reason but being unsupported.
    pub async fn initialize(
        stream: &UnixStream,
        pid: u32,
        identity_context: Arc<IdentityContext>,
    ) -> Result<PeerPinning> {
        let Some(pidfd) = supported_peer_pidfd(getsockopt(stream, PeerPidfd))? else {
            return Ok(PeerPinning::Unsupported);
        };
        verify_pidfd_pid(&pidfd, pid)?;
        require_live_pidfd(&pidfd)?;
        // Durable identity is evaluated afresh for every connection, inside
        // the pin, and never cached for another (ADR 0014).
        let (executable, durable) = tokio::task::spawn_blocking(move || {
            let executable = ExecutableIdentity::from_pid(pid);
            let durable = executable
                .as_ref()
                .ok()
                .map(|executable| identity_context.evaluate(executable));
            (executable, durable)
        })
        .await
        .context("peer executable snapshot task failed")?;
        require_live_pidfd(&pidfd)?;
        Ok(PeerPinning::Pinned {
            monitor: Self {
                pidfd: AsyncFd::new(pidfd).context("cannot monitor peer pidfd")?,
            },
            executable,
            durable,
        })
    }

    pub async fn exited(&self) -> Result<()> {
        let _guard = self
            .pidfd
            .readable()
            .await
            .context("peer pidfd monitor failed")?;
        Ok(())
    }

    /// A second close-on-exec descriptor for the pinned peer process, so a
    /// handoff can keep the binding after this connection's monitor ends
    /// (Plan 0061 M3.13b1). Both name the one process the connection was
    /// verified against; a reused PID matches neither.
    ///
    /// # Errors
    /// Fails when the descriptor cannot be duplicated.
    pub fn duplicate_pidfd(&self) -> Result<OwnedFd> {
        rustix::io::fcntl_dupfd_cloexec(self.pidfd.get_ref(), 0)
            .context("cannot duplicate the peer pidfd")
    }
}

/// Tolerates only an unsupported `SO_PEERPIDFD`: an older kernel answers an
/// unknown socket option with `ENOPROTOOPT`. Any other failure fails closed;
/// on Linux 6.5+ `EINVAL` or `ESRCH` means the peer has no live process left.
fn supported_peer_pidfd(result: nix::Result<OwnedFd>) -> Result<Option<OwnedFd>> {
    match result {
        Ok(pidfd) => Ok(Some(pidfd)),
        Err(nix::errno::Errno::ENOPROTOOPT) => Ok(None),
        Err(error) => Err(error).context("cannot pin the socket peer with SO_PEERPIDFD"),
    }
}

fn verify_pidfd_pid(pidfd: &OwnedFd, expected_pid: u32) -> Result<()> {
    let path = format!("/proc/self/fdinfo/{}", pidfd.as_raw_fd());
    let contents = fs::read_to_string(path).context("cannot inspect peer pidfd")?;
    let actual = contents
        .lines()
        .find_map(|line| line.strip_prefix("Pid:\t"))
        .context("peer pidfd omits process identity")?
        .parse::<u32>()
        .context("peer pidfd process identity is invalid")?;
    if actual != expected_pid {
        bail!("SO_PEERCRED and SO_PEERPIDFD identify different processes");
    }
    Ok(())
}

pub fn require_live_pidfd(pidfd: &OwnedFd) -> Result<()> {
    let mut descriptors = [PollFd::new(pidfd.as_fd(), PollFlags::POLLIN)];
    let ready = poll(&mut descriptors, PollTimeout::ZERO).context("cannot poll peer pidfd")?;
    if ready != 0
        || descriptors[0]
            .revents()
            .is_some_and(|events| !events.is_empty())
    {
        bail!("peer exited during persistent identity snapshot");
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct Grant {
    id: u64,
    peer: PeerIdentity,
    splint_id: SplintId,
    incarnation: u64,
    scopes: BTreeSet<AccessScope>,
    expires: Instant,
    expires_at_unix_seconds: u64,
    created_unix_seconds: u64,
}

impl Grant {
    fn wire(&self) -> AccessGrant {
        AccessGrant {
            grant_id: self.id,
            splint_id: self.splint_id,
            incarnation: self.incarnation,
            scopes: self.scopes.iter().copied().collect(),
            requester: self.peer.requester_label(),
            expires_at_unix_seconds: self.expires_at_unix_seconds,
        }
    }
}

#[derive(Clone, Debug)]
struct LairGrant {
    id: u64,
    peer: PeerIdentity,
    lair_id: LairId,
    scopes: BTreeSet<AccessScope>,
    expires: Instant,
    expires_at_unix_seconds: u64,
    created_unix_seconds: u64,
    /// The host-reported agent name: display data only.
    display_name: Option<String>,
}

impl LairGrant {
    fn wire(&self) -> LairAccessGrant {
        LairAccessGrant {
            grant_id: self.id,
            source: AccessGrantSource::Ephemeral,
            lair_id: self.lair_id,
            scopes: self.scopes.iter().copied().collect(),
            requester: self.peer.requester_label(),
            expires_at_unix_seconds: self.expires_at_unix_seconds,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GrantResource {
    Splint {
        splint_id: SplintId,
        incarnation: u64,
    },
    Lair {
        lair_id: LairId,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuditDecision {
    Granted,
    Denied,
    Revoked,
}

#[allow(
    dead_code,
    reason = "bounded audit metadata is retained in memory for future trusted diagnostics"
)]
#[derive(Clone, Debug)]
pub struct AuditRecord {
    pub order: u64,
    pub unix_seconds: u64,
    pub peer_uid: u32,
    pub peer_pid: u32,
    pub requester: String,
    pub lair_id: Option<LairId>,
    pub splint_id: Option<SplintId>,
    pub incarnation: Option<u64>,
    pub scopes: Vec<AccessScope>,
    pub decision: AuditDecision,
    pub reason: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationMutation {
    pub grant: AccessGrant,
    pub authorization_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LairAuthorizationMutation {
    pub grant: LairAccessGrant,
    pub authorization_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevokedAuthorization {
    Splint(AuthorizationMutation),
    Lair(LairAuthorizationMutation),
}

/// One "just this once" grant as the access inventory shows it.
#[derive(Clone, Debug)]
pub struct OnceGrantView {
    pub grant_id: u64,
    /// The requesting process the grant is bound to.
    pub peer: PeerIdentity,
    pub target: GrantResource,
    pub scopes: Vec<AccessScope>,
    pub display_name: Option<String>,
    pub created_unix_seconds: u64,
    pub expires_at_unix_seconds: u64,
}

#[derive(Debug, Default)]
pub struct GrantStore {
    ids: Arc<GrantIds>,
    next_audit_order: u64,
    grants: Vec<Grant>,
    lair_grants: Vec<LairGrant>,
    audit: VecDeque<AuditRecord>,
}

impl GrantStore {
    /// A store allocating runtime grant IDs from the space every authority
    /// source shares, and advancing the revision they share.
    pub fn with_ids(ids: Arc<GrantIds>) -> Self {
        Self {
            ids,
            ..Self::default()
        }
    }

    /// The daemon-wide authorization revision.
    #[must_use]
    pub fn authorization_revision(&self) -> u64 {
        self.ids.revision()
    }

    fn advance_authorization_revision(&mut self) {
        self.ids.advance_revision();
    }

    /// Whether a once grant is still in force: present and unexpired.
    /// Controller and subscription registration check this under their own
    /// lock, after authorization, so a revoke that lands in between wins.
    pub fn is_live(&self, grant_id: u64) -> bool {
        let now = Instant::now();
        self.grants
            .iter()
            .any(|grant| grant.id == grant_id && grant.expires > now)
            || self
                .lair_grants
                .iter()
                .any(|grant| grant.id == grant_id && grant.expires > now)
    }

    /// Every unexpired once grant, for the access inventory.
    pub fn inventory(&mut self) -> Vec<OnceGrantView> {
        self.remove_expired();
        let now = Instant::now();
        let splint = self.grants.iter().map(|grant| OnceGrantView {
            grant_id: grant.id,
            peer: grant.peer.clone(),
            target: GrantResource::Splint {
                splint_id: grant.splint_id,
                incarnation: grant.incarnation,
            },
            scopes: grant.scopes.iter().copied().collect(),
            display_name: None,
            created_unix_seconds: grant.created_unix_seconds,
            expires_at_unix_seconds: grant.expires_at_unix_seconds,
        });
        let lair = self
            .lair_grants
            .iter()
            .filter(|grant| grant.expires > now)
            .map(|grant| OnceGrantView {
                grant_id: grant.id,
                peer: grant.peer.clone(),
                target: GrantResource::Lair {
                    lair_id: grant.lair_id,
                },
                scopes: grant.scopes.iter().copied().collect(),
                display_name: grant.display_name.clone(),
                created_unix_seconds: grant.created_unix_seconds,
                expires_at_unix_seconds: grant.expires_at_unix_seconds,
            });
        splint.chain(lair).collect()
    }

    /// How many once grants are in force, for a dry-run **Revoke all
    /// access**.
    pub fn live_count(&mut self) -> usize {
        self.remove_expired();
        let now = Instant::now();
        self.grants.len()
            + self
                .lair_grants
                .iter()
                .filter(|grant| grant.expires > now)
                .count()
    }

    /// **Revoke all access**: removes every once grant, for a Lair or a
    /// Splint, and returns the IDs of those that were in force. The caller
    /// ends their controllers and subscriptions.
    pub fn revoke_all(&mut self, reason: &'static str) -> Vec<u64> {
        self.remove_expired();
        let now = Instant::now();
        let grants = std::mem::take(&mut self.grants);
        let lair_grants = std::mem::take(&mut self.lair_grants);
        let mut revoked = Vec::new();
        for grant in grants {
            revoked.push(grant.id);
            self.record(
                &grant.peer,
                grant.splint_id,
                grant.incarnation,
                grant.scopes,
                AuditDecision::Revoked,
                reason,
            );
        }
        for grant in lair_grants {
            // An expired grant was already out of force; it leaves silently.
            if grant.expires <= now {
                continue;
            }
            revoked.push(grant.id);
            self.record_lair(
                &grant.peer,
                grant.lair_id,
                grant.scopes,
                AuditDecision::Revoked,
                reason,
            );
        }
        if !revoked.is_empty() {
            self.advance_authorization_revision();
        }
        revoked
    }

    pub fn authorize(
        &mut self,
        peer: &PeerIdentity,
        splint_id: SplintId,
        incarnation: u64,
        required: &[AccessScope],
    ) -> Option<u64> {
        self.remove_expired();
        let required: BTreeSet<_> = required.iter().copied().collect();
        self.grants.iter().find_map(|grant| {
            (grant.peer == *peer
                && grant.splint_id == splint_id
                && grant.incarnation == incarnation
                && required.is_subset(&grant.scopes))
            .then_some(grant.id)
        })
    }

    pub fn authorize_lair(
        &mut self,
        peer: &PeerIdentity,
        lair_id: LairId,
        required: &[AccessScope],
    ) -> Option<u64> {
        self.remove_expired();
        let required: BTreeSet<_> = required.iter().copied().collect();
        self.lair_grants.iter().find_map(|grant| {
            (grant.peer == *peer
                && grant.lair_id == lair_id
                && grant.expires > Instant::now()
                && required.is_subset(&grant.scopes))
            .then_some(grant.id)
        })
    }

    pub fn status(&mut self, splint_id: SplintId, incarnation: u64) -> Vec<AccessGrant> {
        self.remove_expired();
        self.grants
            .iter()
            .filter(|grant| grant.splint_id == splint_id && grant.incarnation == incarnation)
            .map(Grant::wire)
            .collect()
    }

    pub fn lair_status(&mut self, lair_id: LairId) -> Vec<LairAccessGrant> {
        self.remove_expired();
        self.lair_grants
            .iter()
            .filter(|grant| grant.lair_id == lair_id && grant.expires > Instant::now())
            .map(LairGrant::wire)
            .collect()
    }

    pub fn grant(
        &mut self,
        peer: &PeerIdentity,
        splint_id: SplintId,
        incarnation: u64,
        scopes: Vec<AccessScope>,
    ) -> AuthorizationMutation {
        self.remove_expired();
        let scopes: BTreeSet<_> = scopes.into_iter().collect();
        if let Some(index) = self.grants.iter().position(|grant| {
            grant.peer == *peer && grant.splint_id == splint_id && grant.incarnation == incarnation
        }) {
            let wire = {
                let grant = &mut self.grants[index];
                grant.scopes.extend(scopes.iter().copied());
                grant.expires = Instant::now() + GRANT_LIFETIME;
                grant.expires_at_unix_seconds = unix_seconds() + GRANT_LIFETIME.as_secs();
                grant.wire()
            };
            self.advance_authorization_revision();
            self.record(
                peer,
                splint_id,
                incarnation,
                scopes,
                AuditDecision::Granted,
                "grant once",
            );
            return AuthorizationMutation {
                grant: wire,
                authorization_revision: self.ids.revision(),
            };
        }
        let grant = Grant {
            id: self.ids.next(),
            peer: peer.clone(),
            splint_id,
            incarnation,
            scopes: scopes.clone(),
            expires: Instant::now() + GRANT_LIFETIME,
            expires_at_unix_seconds: unix_seconds() + GRANT_LIFETIME.as_secs(),
            created_unix_seconds: unix_seconds(),
        };
        let wire = grant.wire();
        self.grants.push(grant);
        self.advance_authorization_revision();
        self.record(
            peer,
            splint_id,
            incarnation,
            scopes,
            AuditDecision::Granted,
            "grant once",
        );
        AuthorizationMutation {
            grant: wire,
            authorization_revision: self.ids.revision(),
        }
    }

    #[cfg(test)]
    pub fn grant_lair(
        &mut self,
        peer: &PeerIdentity,
        lair_id: LairId,
        scopes: Vec<AccessScope>,
    ) -> LairAuthorizationMutation {
        self.grant_lair_named(peer, lair_id, scopes, None)
    }

    /// [`Self::grant_lair`], keeping the host-reported agent name as
    /// display data for the access inventory.
    pub fn grant_lair_named(
        &mut self,
        peer: &PeerIdentity,
        lair_id: LairId,
        scopes: Vec<AccessScope>,
        display_name: Option<String>,
    ) -> LairAuthorizationMutation {
        self.remove_expired();
        let scopes: BTreeSet<_> = scopes.into_iter().collect();
        if let Some(index) = self
            .lair_grants
            .iter()
            .position(|grant| grant.peer == *peer && grant.lair_id == lair_id)
        {
            let wire = {
                let grant = &mut self.lair_grants[index];
                grant.scopes.extend(scopes.iter().copied());
                grant.expires = Instant::now() + GRANT_LIFETIME;
                grant.expires_at_unix_seconds = unix_seconds() + GRANT_LIFETIME.as_secs();
                if display_name.is_some() {
                    grant.display_name = display_name;
                }
                grant.wire()
            };
            self.advance_authorization_revision();
            self.record_lair(
                peer,
                lair_id,
                scopes,
                AuditDecision::Granted,
                "grant Lair once",
            );
            return LairAuthorizationMutation {
                grant: wire,
                authorization_revision: self.ids.revision(),
            };
        }
        let grant = LairGrant {
            id: self.ids.next(),
            peer: peer.clone(),
            lair_id,
            scopes: scopes.clone(),
            expires: Instant::now() + GRANT_LIFETIME,
            expires_at_unix_seconds: unix_seconds() + GRANT_LIFETIME.as_secs(),
            created_unix_seconds: unix_seconds(),
            display_name,
        };
        let wire = grant.wire();
        self.lair_grants.push(grant);
        self.advance_authorization_revision();
        self.record_lair(
            peer,
            lair_id,
            scopes,
            AuditDecision::Granted,
            "grant Lair once",
        );
        LairAuthorizationMutation {
            grant: wire,
            authorization_revision: self.ids.revision(),
        }
    }

    pub fn deny_lair(
        &mut self,
        peer: &PeerIdentity,
        lair_id: LairId,
        scopes: &[AccessScope],
        reason: &'static str,
    ) {
        self.record_lair(
            peer,
            lair_id,
            scopes.iter().copied().collect(),
            AuditDecision::Denied,
            reason,
        );
    }

    pub fn deny(
        &mut self,
        peer: &PeerIdentity,
        splint_id: SplintId,
        incarnation: u64,
        scopes: &[AccessScope],
        reason: &'static str,
    ) {
        self.record(
            peer,
            splint_id,
            incarnation,
            scopes.iter().copied().collect(),
            AuditDecision::Denied,
            reason,
        );
    }

    pub fn owns_lair_grant(&self, peer: &PeerIdentity, grant_id: u64) -> bool {
        self.lair_grants.iter().any(|grant| {
            grant.id == grant_id && grant.peer == *peer && grant.expires > Instant::now()
        })
    }

    pub fn grant_resource(&self, grant_id: u64) -> Option<GrantResource> {
        self.grants
            .iter()
            .find(|grant| grant.id == grant_id)
            .map(|grant| GrantResource::Splint {
                splint_id: grant.splint_id,
                incarnation: grant.incarnation,
            })
            .or_else(|| {
                self.lair_grants
                    .iter()
                    .find(|grant| grant.id == grant_id)
                    .map(|grant| GrantResource::Lair {
                        lair_id: grant.lair_id,
                    })
            })
    }

    pub fn revoke(&mut self, grant_id: u64) -> Option<AuthorizationMutation> {
        let index = self.grants.iter().position(|grant| grant.id == grant_id)?;
        let grant = self.grants.remove(index);
        let wire = grant.wire();
        self.advance_authorization_revision();
        self.record(
            &grant.peer,
            grant.splint_id,
            grant.incarnation,
            grant.scopes,
            AuditDecision::Revoked,
            "explicit local revocation",
        );
        Some(AuthorizationMutation {
            grant: wire,
            authorization_revision: self.ids.revision(),
        })
    }

    pub fn revoke_any(&mut self, grant_id: u64) -> Option<RevokedAuthorization> {
        if let Some(revoked) = self.revoke(grant_id) {
            return Some(RevokedAuthorization::Splint(revoked));
        }
        let index = self
            .lair_grants
            .iter()
            .position(|grant| grant.id == grant_id)?;
        let grant = self.lair_grants.remove(index);
        let wire = grant.wire();
        self.advance_authorization_revision();
        self.record_lair(
            &grant.peer,
            grant.lair_id,
            grant.scopes,
            AuditDecision::Revoked,
            "explicit local revocation",
        );
        Some(RevokedAuthorization::Lair(LairAuthorizationMutation {
            grant: wire,
            authorization_revision: self.ids.revision(),
        }))
    }

    /// The time left on a once grant (Lair or Splint) that is in force.
    pub fn once_remaining(&self, grant_id: u64) -> Option<Duration> {
        let now = Instant::now();
        self.grants
            .iter()
            .find(|grant| grant.id == grant_id)
            .map(|grant| grant.expires)
            .or_else(|| {
                self.lair_grants
                    .iter()
                    .find(|grant| grant.id == grant_id)
                    .map(|grant| grant.expires)
            })
            .filter(|expires| *expires > now)
            .map(|expires| expires - now)
    }

    /// At a once grant's expiry: `true` when it is no longer in force, after
    /// removing it if it is still held, so the caller broadcasts its end and
    /// releases its controllers. A grant refreshed meanwhile stays, and a
    /// grant already removed (expired silently or revoked) reports `true`
    /// again, which only repeats an end.
    pub fn end_if_expired(&mut self, grant_id: u64) -> bool {
        let now = Instant::now();
        if let Some(index) = self.grants.iter().position(|grant| grant.id == grant_id) {
            if self.grants[index].expires > now {
                return false;
            }
            let grant = self.grants.remove(index);
            self.advance_authorization_revision();
            self.record(
                &grant.peer,
                grant.splint_id,
                grant.incarnation,
                grant.scopes,
                AuditDecision::Revoked,
                "grant expired",
            );
            return true;
        }
        if let Some(grant) = self.lair_grants.iter().find(|grant| grant.id == grant_id)
            && grant.expires > now
        {
            return false;
        }
        let _ = self.revoke_lair_if_expired(grant_id);
        true
    }

    /// Moves a once grant's expiry to now (tests of expiry handling).
    #[cfg(test)]
    pub fn expire_now_for_test(&mut self, grant_id: u64) {
        let now = Instant::now();
        for grant in &mut self.grants {
            if grant.id == grant_id {
                grant.expires = now;
            }
        }
        for grant in &mut self.lair_grants {
            if grant.id == grant_id {
                grant.expires = now;
            }
        }
    }

    pub fn revoke_lair_if_expired(&mut self, grant_id: u64) -> Option<LairAuthorizationMutation> {
        let index = self
            .lair_grants
            .iter()
            .position(|grant| grant.id == grant_id && grant.expires <= Instant::now())?;
        let grant = self.lair_grants.remove(index);
        let wire = grant.wire();
        self.advance_authorization_revision();
        self.record_lair(
            &grant.peer,
            grant.lair_id,
            grant.scopes,
            AuditDecision::Revoked,
            "grant expired",
        );
        Some(LairAuthorizationMutation {
            grant: wire,
            authorization_revision: self.ids.revision(),
        })
    }

    pub fn lair_grant_with_revision(&mut self, grant_id: u64) -> Option<LairAuthorizationMutation> {
        self.remove_expired();
        self.lair_grants
            .iter()
            .find(|grant| grant.id == grant_id)
            .map(|grant| LairAuthorizationMutation {
                grant: grant.wire(),
                authorization_revision: self.ids.revision(),
            })
    }

    pub fn grant_with_revision(&mut self, grant_id: u64) -> Option<AuthorizationMutation> {
        self.remove_expired();
        self.grants
            .iter()
            .find(|grant| grant.id == grant_id)
            .map(|grant| AuthorizationMutation {
                grant: grant.wire(),
                authorization_revision: self.ids.revision(),
            })
    }

    pub fn revoke_lair(&mut self, lair_id: LairId, reason: &'static str) -> Vec<u64> {
        let mut removed = Vec::new();
        self.lair_grants.retain(|grant| {
            if grant.lair_id == lair_id {
                removed.push(grant.clone());
                false
            } else {
                true
            }
        });
        let ids = removed.iter().map(|grant| grant.id).collect();
        if !removed.is_empty() {
            self.advance_authorization_revision();
        }
        for grant in removed {
            self.record_lair(
                &grant.peer,
                lair_id,
                grant.scopes,
                AuditDecision::Revoked,
                reason,
            );
        }
        ids
    }

    pub fn revoke_identity(
        &mut self,
        splint_id: SplintId,
        incarnation: u64,
        reason: &'static str,
    ) -> Vec<u64> {
        let mut removed = Vec::new();
        self.grants.retain(|grant| {
            if grant.splint_id == splint_id && grant.incarnation == incarnation {
                removed.push(grant.clone());
                false
            } else {
                true
            }
        });
        let ids = removed.iter().map(|grant| grant.id).collect();
        if !removed.is_empty() {
            self.advance_authorization_revision();
        }
        for grant in removed {
            self.record(
                &grant.peer,
                splint_id,
                incarnation,
                grant.scopes,
                AuditDecision::Revoked,
                reason,
            );
        }
        ids
    }

    fn remove_expired(&mut self) {
        let previous = self.grants.len();
        self.grants.retain(|grant| grant.expires > Instant::now());
        if self.grants.len() != previous {
            self.advance_authorization_revision();
        }
    }

    fn record(
        &mut self,
        peer: &PeerIdentity,
        splint_id: SplintId,
        incarnation: u64,
        scopes: BTreeSet<AccessScope>,
        decision: AuditDecision,
        reason: &'static str,
    ) {
        self.next_audit_order = self.next_audit_order.saturating_add(1);
        if self.audit.len() == MAX_AUDIT_RECORDS {
            self.audit.pop_front();
        }
        self.audit.push_back(AuditRecord {
            order: self.next_audit_order,
            unix_seconds: unix_seconds(),
            peer_uid: peer.uid,
            peer_pid: peer.pid,
            requester: peer.requester_label(),
            lair_id: None,
            splint_id: Some(splint_id),
            incarnation: Some(incarnation),
            scopes: scopes.into_iter().collect(),
            decision,
            reason,
        });
    }

    fn record_lair(
        &mut self,
        peer: &PeerIdentity,
        lair_id: LairId,
        scopes: BTreeSet<AccessScope>,
        decision: AuditDecision,
        reason: &'static str,
    ) {
        self.next_audit_order = self.next_audit_order.saturating_add(1);
        if self.audit.len() == MAX_AUDIT_RECORDS {
            self.audit.pop_front();
        }
        self.audit.push_back(AuditRecord {
            order: self.next_audit_order,
            unix_seconds: unix_seconds(),
            peer_uid: peer.uid,
            peer_pid: peer.pid,
            requester: peer.requester_label(),
            lair_id: Some(lair_id),
            splint_id: None,
            incarnation: None,
            scopes: scopes.into_iter().collect(),
            decision,
            reason,
        });
    }
}

/// Asks the person to confirm one Splint request or a forced control
/// transfer: a prompt that offers "Just this once" only. `true` only for
/// that grant.
pub async fn prompt(
    install_prefix: &InstallPrefix,
    peer: &PeerIdentity,
    splint_id: SplintId,
    incarnation: u64,
    scopes: Vec<AccessScope>,
) -> Result<bool> {
    let decision = prompt_target(
        install_prefix.client(),
        peer,
        ConsentTarget::Splint {
            splint_id,
            incarnation,
        },
        scopes,
        PromptDetails::once_only(),
        CONSENT_DEADLINE,
        Arc::new(PromptCancel::default()),
    )
    .await?;
    Ok(decision
        == ConsentDecision::Grant {
            duration: GrantDuration::Once,
        })
}

/// What a workspace prompt shows beyond the requester, target, and scopes
/// (ADR 0014).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptDetails {
    /// The host-reported agent name: display data only.
    pub agent_name: Option<String>,
    /// The level the scopes are exactly, or `None` for Custom.
    pub level: Option<AccessLevel>,
    /// Every duration shown, "Just this once" first.
    pub durations: Vec<ConsentDurationOffer>,
}

impl PromptDetails {
    /// "Just this once" and nothing else, for Splint prompts.
    pub fn once_only() -> Self {
        Self {
            agent_name: None,
            level: None,
            durations: vec![ConsentDurationOffer {
                duration: GrantDuration::Once,
                withheld: None,
                shared_by: None,
                replaces: None,
            }],
        }
    }
}

/// The content of one workspace prompt as a consent responder is offered it
/// (ADR 0015): exactly what [`prompt_lair`] shows in the local dialog, built
/// from the same request, without the dialog capability, the origin, or the
/// countdown.
pub fn offered_lair_prompt(
    peer: &PeerIdentity,
    lair_id: LairId,
    lair_name: String,
    scopes: Vec<AccessScope>,
    details: PromptDetails,
) -> splinterm_protocol::OfferedConsentPrompt {
    splinterm_protocol::OfferedConsentPrompt {
        requester: peer.requester_label(),
        requester_pid: peer.pid,
        requester_uid: peer.uid,
        agent_name: details.agent_name,
        target: ConsentTarget::Lair { lair_id, lair_name },
        scopes,
        level: details.level,
        durations: details.durations,
    }
}

/// Closes an open consent prompt from another task: the pending request was
/// cancelled, expired, or ended while the person had not answered.
///
/// The prompt's client is this daemon's own unreaped child, so its PID
/// cannot name another process while it is recorded here.
#[derive(Debug, Default)]
pub struct PromptCancel {
    state: std::sync::Mutex<PromptCancelState>,
    notify: tokio::sync::Notify,
}

#[derive(Debug, Default)]
struct PromptCancelState {
    cancelled: bool,
    child: Option<rustix::process::Pid>,
}

impl PromptCancel {
    fn state(&self) -> std::sync::MutexGuard<'_, PromptCancelState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Closes the prompt: its client is killed, or never starts.
    pub fn cancel(&self) {
        let mut state = self.state();
        state.cancelled = true;
        if let Some(child) = state.child {
            let _ = rustix::process::kill_process(child, rustix::process::Signal::KILL);
        }
        drop(state);
        self.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.state().cancelled
    }

    /// Resolves once [`Self::cancel`] has run.
    pub async fn cancelled(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }

    /// Records the started client; `false` (the caller kills it) when the
    /// prompt was cancelled before it started.
    fn register(&self, child: u32) -> bool {
        let mut state = self.state();
        if state.cancelled {
            return false;
        }
        state.child = i32::try_from(child)
            .ok()
            .and_then(rustix::process::Pid::from_raw);
        true
    }

    /// Forgets the client before it is reaped, so its PID is never signalled
    /// after it could be reused.
    fn unregister(&self) {
        self.state().child = None;
    }
}

/// Shows one workspace prompt and waits for the person's typed answer, at
/// most `deadline`, or until `cancel` closes it.
#[allow(
    clippy::too_many_arguments,
    reason = "one prompt's complete, already-validated contents"
)]
pub async fn prompt_lair(
    install_prefix: &InstallPrefix,
    peer: &PeerIdentity,
    lair_id: LairId,
    lair_name: String,
    scopes: Vec<AccessScope>,
    details: PromptDetails,
    deadline: Duration,
    cancel: Arc<PromptCancel>,
) -> Result<ConsentDecision> {
    prompt_target(
        install_prefix.client(),
        peer,
        ConsentTarget::Lair { lair_id, lair_name },
        scopes,
        details,
        deadline,
        cancel,
    )
    .await
}

#[allow(
    clippy::too_many_arguments,
    reason = "one prompt's complete, already-validated contents"
)]
async fn prompt_target(
    client: std::path::PathBuf,
    peer: &PeerIdentity,
    target: ConsentTarget,
    scopes: Vec<AccessScope>,
    details: PromptDetails,
    deadline: Duration,
    cancel: Arc<PromptCancel>,
) -> Result<ConsentDecision> {
    if scopes.is_empty() || scopes.len() > MAX_ACCESS_SCOPES {
        bail!("invalid consent scope count");
    }
    let prompt = consent_prompt(peer, target, scopes, details, deadline);
    tokio::task::spawn_blocking(move || prompt_blocking(&client, prompt, deadline, &cancel))
        .await
        .context("consent broker task failed")?
}

/// The prompt the trusted consent client shows, before its capability is
/// minted: this daemon's own, so its origin is local, counting down the
/// `deadline` the daemon waits for its answer.
fn consent_prompt(
    peer: &PeerIdentity,
    target: ConsentTarget,
    scopes: Vec<AccessScope>,
    details: PromptDetails,
    deadline: Duration,
) -> ConsentPrompt {
    ConsentPrompt {
        capability: Vec::new(),
        requester: peer.requester_label(),
        requester_pid: peer.pid,
        requester_uid: peer.uid,
        agent_name: details.agent_name,
        target,
        scopes,
        level: details.level,
        durations: details.durations,
        respond_within_seconds: respond_within_seconds(deadline),
        origin: splinterm_protocol::ConsentOrigin::Local,
    }
}

/// The whole seconds the countdown of a prompt the daemon is about to show
/// counts for `deadline`: rounded down, so it never shows more time than
/// the daemon waits, and at most the pending request lifetime a prompt may
/// count down. Never zero, which would show no countdown and leave Allow
/// live: with no whole second left, the dialog starts with its time
/// already up (it counts a second short), so it grants nothing.
fn respond_within_seconds(deadline: Duration) -> u64 {
    deadline
        .as_secs()
        .clamp(1, splinterm_protocol::MAX_CONSENT_RESPOND_WITHIN_SECONDS)
}

/// Launches the trusted consent client, `<prefix>/splinterm`, from the
/// daemon's installed-prefix authority, never from a path derived from the
/// running daemon image, and returns its typed answer. An answer naming a
/// duration the prompt did not offer fails closed.
fn prompt_blocking(
    executable: &Path,
    mut prompt: ConsentPrompt,
    deadline: Duration,
    cancel: &PromptCancel,
) -> Result<ConsentDecision> {
    let mut capability = vec![0_u8; CONSENT_CAPABILITY_BYTES];
    let mut offset = 0;
    while offset < capability.len() {
        offset += rustix::rand::getrandom(
            &mut capability[offset..],
            rustix::rand::GetRandomFlags::empty(),
        )
        .context("OS randomness unavailable")?;
    }
    prompt.capability.clone_from(&capability);
    if let Err(reason) = prompt.validate() {
        bail!("invalid consent prompt: {reason}");
    }
    if cancel.is_cancelled() {
        bail!("consent prompt cancelled");
    }
    let deadline = deadline.max(Duration::from_millis(1));
    let (mut daemon, child) = StdUnixStream::pair().context("create private consent socket")?;
    daemon.set_read_timeout(Some(deadline))?;
    daemon.set_write_timeout(Some(deadline))?;
    let child_read = child
        .try_clone()
        .context("duplicate private consent descriptor")?;
    // The child inherits this thread's signal mask (`std::process::Command`
    // passes it on). No daemon thread that can reach here holds a blocked
    // mask: a handoff candidate releases the signals its exec carried before
    // it builds its runtime (`handoff_exec`, "Signals across exec").
    let mut process = Command::new(executable)
        .arg("consent")
        .stdin(Stdio::from(OwnedFd::from(child_read)))
        .stdout(Stdio::from(OwnedFd::from(child)))
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("launch trusted consent client {}", executable.display()))?;
    // Why a client that fails before answering failed, for the daemon log.
    let stderr = StderrTail::capture(&mut process);
    // Every path below reaps the client before it returns, and only then
    // does this drop: it removes the dialog shortcut entry the client
    // registered, even when the client was killed (`dialog_shortcut`), on a
    // detached thread so the decision never waits for Hyprland.
    let _shortcut = ReapedDialogShortcut(Some(
        splinterd::dialog_shortcut::DialogShortcutLease::for_child(process.id()),
    ));
    if !cancel.register(process.id()) {
        let _ = process.kill();
        let _ = process.wait();
        bail!("consent prompt cancelled");
    }
    let exchanged = write_bounded(&mut daemon, &prompt)
        .and_then(|()| read_bounded::<ConsentReply>(&mut daemon));
    cancel.unregister();
    let reply = match exchanged {
        Ok(reply) => reply,
        Err(error) => {
            let _ = process.kill();
            let status = process.wait().ok();
            let printed = stderr.finish(STDERR_TAIL_WAIT);
            return Err(child_failure(
                "the consent client",
                &error,
                status,
                printed.as_deref(),
            ));
        }
    };
    let status = process.wait().context("wait for consent client")?;
    if !status.success() || reply.capability != capability {
        let error = anyhow::anyhow!("consent client authentication failed");
        if status.success() {
            return Err(error);
        }
        let printed = stderr.finish(STDERR_TAIL_WAIT);
        return Err(child_failure(
            "the consent client",
            &error,
            Some(status),
            printed.as_deref(),
        ));
    }
    if let ConsentDecision::Grant { duration } = reply.decision
        && !prompt.offers(duration)
    {
        bail!("consent client chose a duration the prompt did not offer");
    }
    Ok(reply.decision)
}

/// Releases a reaped consent client's dialog shortcut entry in the
/// background when dropped.
struct ReapedDialogShortcut(Option<splinterd::dialog_shortcut::DialogShortcutLease>);

impl Drop for ReapedDialogShortcut {
    fn drop(&mut self) {
        if let Some(lease) = self.0.take() {
            lease.release_in_background();
        }
    }
}

pub fn read_bounded<T: serde::de::DeserializeOwned>(reader: &mut impl Read) -> Result<T> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_CONSENT_FRAME_BYTES {
        bail!("invalid consent frame length");
    }
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).context("invalid consent frame")
}

pub fn write_bounded<T: serde::Serialize>(writer: &mut impl Write, value: &T) -> Result<()> {
    let body = serde_json::to_vec(value).context("encode consent frame")?;
    if body.is_empty() || body.len() > MAX_CONSENT_FRAME_BYTES {
        bail!("consent frame exceeds bound");
    }
    writer.write_all(&u32::try_from(body.len())?.to_be_bytes())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[tokio::test]
    async fn peer_pidfd_matches_credentials_and_guards_executable_snapshot() {
        let (first, _second) = UnixStream::pair().unwrap();
        let credentials = first.peer_cred().unwrap();
        let pid = u32::try_from(credentials.pid().unwrap()).unwrap();

        let PeerPinning::Pinned {
            monitor: _monitor,
            executable,
            durable,
        } = PeerMonitor::initialize(&first, pid, Arc::new(IdentityContext::for_daemon()))
            .await
            .unwrap()
        else {
            panic!("this kernel supports SO_PEERPIDFD");
        };
        let executable = executable.unwrap();

        assert_eq!(pid, std::process::id());
        assert!(executable.path.is_absolute());
        assert_eq!(executable.sha256.len(), 64);
        // Durable evidence is read under the same pin. The test binary is a
        // build output, never root-owned, so it has no durable identity.
        let durable = durable.expect("evidence for a bindable executable");
        if rustix::process::geteuid().as_raw() != 0 {
            assert_eq!(durable.preferred(), None);
        }
    }

    /// A child process the test started and owns, killed and reaped on drop.
    struct OwnedChild(std::process::Child);

    impl OwnedChild {
        fn pid(&self) -> u32 {
            self.0.id()
        }
    }

    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn spawn_sleep(program: &Path) -> OwnedChild {
        spawn_owned(Command::new(program).arg("30"))
    }

    /// Spawns `command` and returns once the child executes its program.
    fn spawn_owned(command: &mut Command) -> OwnedChild {
        // A sibling test thread's fork can briefly inherit the descriptor
        // that wrote a fresh copy, so its exec may see ETXTBSY.
        for _ in 0..100 {
            match command.spawn() {
                Ok(child) => {
                    let child = OwnedChild(child);
                    wait_for_exec(child.pid());
                    return child;
                }
                Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("spawn child: {error}"),
            }
        }
        panic!("spawn child: executable stayed busy");
    }

    /// Under parallel tests, `spawn` was seen to return while the child's
    /// executable link still named this test binary; waits until it does
    /// not, so identity reads describe the program the child executes.
    fn wait_for_exec(pid: u32) {
        let own = executable_file_identity(std::process::id()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while executable_file_identity(pid).is_ok_and(|current| current == own) {
            assert!(Instant::now() < deadline, "child {pid} never executed");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// A sealed memfd holding a copy of `program`, as a handoff seals the
    /// forward client snapshot.
    fn sealed_copy(program: &Path) -> OwnedFd {
        let memfd = rustix::fs::memfd_create(
            "splinterm",
            rustix::fs::MemfdFlags::CLOEXEC
                | rustix::fs::MemfdFlags::ALLOW_SEALING
                | rustix::fs::MemfdFlags::EXEC,
        )
        .unwrap();
        let mut image = fs::File::from(memfd);
        image.write_all(&fs::read(program).unwrap()).unwrap();
        let memfd = OwnedFd::from(image);
        rustix::fs::fcntl_add_seals(
            &memfd,
            rustix::fs::SealFlags::WRITE
                | rustix::fs::SealFlags::GROW
                | rustix::fs::SealFlags::SHRINK
                | rustix::fs::SealFlags::SEAL,
        )
        .unwrap();
        memfd
    }

    /// AGENTS.md §4.4, the 2026-10-18 exception: an adopted daemon trusts a
    /// client executing exactly the snapshot it pinned, by device and inode;
    /// a daemon that pinned nothing does not, and neither does a different
    /// file with the same bytes or a stale installed inode.
    #[test]
    fn only_the_exact_pinned_snapshot_inode_is_trusted() {
        let snapshot = sealed_copy(system_sleep());
        let child = spawn_sleep(Path::new(&format!(
            "/proc/self/fd/{}",
            snapshot.as_raw_fd()
        )));
        let pinned = Arc::new(PinnedClientSnapshot::pin(snapshot.try_clone().unwrap()).unwrap());
        let stat = rustix::fs::fstat(&snapshot).unwrap();
        assert_eq!(pinned.identity(), (stat.st_dev, stat.st_ino));

        let prefix = test_install_prefix();
        // A fresh or rollback daemon pins nothing: the relaunched client is
        // not trusted there.
        let fresh = peer_for(child.pid(), &prefix);
        assert!(!fresh.is_matching_splinterm());
        // The adopted daemon that pinned this snapshot trusts it.
        let adopted = PeerIdentity {
            pinned_client: Some(Arc::clone(&pinned)),
            ..fresh.clone()
        };
        assert!(adopted.is_matching_splinterm());
        // The pin never widens the graphical relay's rule.
        assert!(!adopted.is_matching_graphical_relay());
        // Same bytes, another file: never matched by content.
        let twin = Arc::new(PinnedClientSnapshot::pin(sealed_copy(system_sleep())).unwrap());
        assert_ne!(twin.identity(), pinned.identity());
        let other = PeerIdentity {
            pinned_client: Some(twin),
            ..fresh.clone()
        };
        assert!(!other.is_matching_splinterm());

        // A client still running a stale installed inode matches neither
        // rule, even in a daemon that pinned a snapshot.
        let scratch = scratch_prefix();
        let client = scratch.client();
        install_copy(system_sleep(), &client);
        let stale_child = spawn_sleep(&client);
        install_copy(system_sleep(), &client);
        let stale = PeerIdentity {
            pinned_client: Some(Arc::clone(&pinned)),
            ..peer_for(stale_child.pid(), &scratch)
        };
        assert!(!stale.is_matching_splinterm());
        drop(stale_child);
        fs::remove_dir_all(scratch.directory()).unwrap();

        // Only a sealed regular file can be pinned.
        let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        assert!(PinnedClientSnapshot::pin(OwnedFd::from(socket)).is_err());
        let unsealed = rustix::fs::memfd_create(
            "splinterm",
            rustix::fs::MemfdFlags::CLOEXEC | rustix::fs::MemfdFlags::ALLOW_SEALING,
        )
        .unwrap();
        rustix::io::write(&unsealed, b"\x7fELF").unwrap();
        assert!(PinnedClientSnapshot::pin(unsealed).is_err());
        // Sealed against writes only: the extent and seal set may change.
        let partial = rustix::fs::memfd_create(
            "splinterm",
            rustix::fs::MemfdFlags::CLOEXEC | rustix::fs::MemfdFlags::ALLOW_SEALING,
        )
        .unwrap();
        rustix::io::write(&partial, b"\x7fELF").unwrap();
        rustix::fs::fcntl_add_seals(&partial, rustix::fs::SealFlags::WRITE).unwrap();
        assert!(PinnedClientSnapshot::pin(partial).is_err());
        // An ordinary file cannot carry seals at all.
        let file = fs::File::open(system_sleep()).unwrap();
        assert!(PinnedClientSnapshot::pin(OwnedFd::from(file)).is_err());
        drop(child);
    }

    fn system_sleep() -> &'static Path {
        ["/usr/bin/sleep", "/bin/sleep"]
            .into_iter()
            .map(Path::new)
            .find(|path| path.is_file())
            .expect("a sleep executable")
    }

    fn scratch_prefix() -> InstallPrefix {
        // Parallel tests can read the same clock value.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "splinterd-peer-identity-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        InstallPrefix::from_directory(&fs::canonicalize(&directory).unwrap()).unwrap()
    }

    fn install_copy(source: &Path, destination: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let staged = destination.with_extension("staged");
        fs::copy(source, &staged).unwrap();
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)).unwrap();
        // Replace by rename, as a package upgrade does: a new inode.
        fs::rename(&staged, destination).unwrap();
    }

    fn peer_for(pid: u32, install_prefix: &InstallPrefix) -> PeerIdentity {
        let (executable_device, executable_inode) = executable_file_identity(pid).unwrap();
        PeerIdentity {
            pid,
            executable_label: executable_label(
                &fs::read_link(format!("/proc/{pid}/exe")).unwrap(),
                executable_device,
                executable_inode,
            ),
            executable_device,
            executable_inode,
            install_prefix: Arc::new(install_prefix.clone()),
            ..PeerIdentity::for_test()
        }
    }

    /// The installed relay is never eligible for the longer durations: it is
    /// recognized by the stream's executable or by the pinned snapshot's.
    #[test]
    fn durable_evidence_marks_the_installed_relay_by_either_snapshot() {
        let prefix = scratch_prefix();
        let relay = prefix.graphical_relay();
        install_copy(system_sleep(), &relay);
        let relay_child = spawn_sleep(&relay);
        let mut running_relay = peer_for(relay_child.pid(), &prefix);
        running_relay.install_durable_evidence(DurableEvidence::default());
        assert!(running_relay.durable_evidence().unwrap().installed_relay);

        // The stream's peer ran another file, but the pinned snapshot hashed
        // the relay: still the relay.
        let other_child = spawn_sleep(system_sleep());
        let mut snapshot_relay = peer_for(other_child.pid(), &prefix);
        let relay_file = fs::metadata(&relay).unwrap();
        snapshot_relay.install_persistent_executable(ExecutableIdentity {
            path: relay.clone(),
            device: relay_file.dev(),
            inode: relay_file.ino(),
            owner_uid: relay_file.uid(),
            mode: relay_file.mode(),
            size: relay_file.len(),
            sha256: "a".repeat(64),
        });
        snapshot_relay.install_durable_evidence(DurableEvidence::default());
        assert!(snapshot_relay.durable_evidence().unwrap().installed_relay);

        // Neither snapshot is the relay.
        let mut not_relay = peer_for(other_child.pid(), &prefix);
        not_relay.install_durable_evidence(DurableEvidence::default());
        assert!(!not_relay.durable_evidence().unwrap().installed_relay);
        drop(relay_child);
        drop(other_child);
        fs::remove_dir_all(prefix.directory()).unwrap();
    }

    #[test]
    fn trusted_client_identity_is_the_executing_inode_not_its_path() {
        let prefix = scratch_prefix();
        let client = prefix.client();
        install_copy(system_sleep(), &client);
        let original = fs::metadata(&client).unwrap();
        let child = spawn_sleep(&client);

        let peer = peer_for(child.pid(), &prefix);
        assert_eq!(
            (peer.executable_device, peer.executable_inode),
            (original.dev(), original.ino())
        );
        assert!(peer.is_matching_splinterm());
        assert!(!peer.is_matching_graphical_relay());
        assert_eq!(peer.requester_label(), client.to_string_lossy());

        // An upgrade replaces the installed client: the running client keeps
        // its old inode, is still identified by it, and no longer matches.
        install_copy(system_sleep(), &client);
        let replaced = fs::metadata(&client).unwrap();
        assert_ne!(replaced.ino(), original.ino());
        let stale = peer_for(child.pid(), &prefix);
        assert_eq!(
            stale.requester_label(),
            format!(
                "{} (deleted) [deleted or replaced file, path unverified; executable device {} inode {}]",
                client.display(),
                original.dev(),
                original.ino()
            )
        );
        assert_eq!(
            (stale.executable_device, stale.executable_inode),
            (original.dev(), original.ino())
        );
        assert!(!stale.is_matching_splinterm());
        assert!(!peer.is_matching_splinterm());

        // With nothing installed at the path, nothing matches.
        fs::remove_file(&client).unwrap();
        assert!(!peer_for(child.pid(), &prefix).is_matching_splinterm());
        drop(child);
        fs::remove_dir_all(prefix.directory()).unwrap();
    }

    fn pidfd_of(pid: u32) -> OwnedFd {
        rustix::process::pidfd_open(
            rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap(),
            rustix::process::PidfdFlags::empty(),
        )
        .unwrap()
    }

    /// `peer`'s connection says `Hello`, pinned by a pidfd for `pid`, in a
    /// daemon where no process holds a trusted connection.
    fn say_hello(peer: &mut PeerIdentity, pid: u32) {
        say_hello_among(peer, pid, &TrustedClientProcesses::default());
    }

    /// [`say_hello`] in a daemon where `trusted` processes hold trusted
    /// connections.
    fn say_hello_among(peer: &mut PeerIdentity, pid: u32, trusted: &TrustedClientProcesses) {
        peer.record_hello_client_match_with(|| Ok(Some(pidfd_of(pid))), trusted);
    }

    /// [`peer_for`] with its start time recorded, as connection verification
    /// records it.
    fn peer_for_process(pid: u32, install_prefix: &InstallPrefix) -> PeerIdentity {
        let mut peer = peer_for(pid, install_prefix);
        peer.install_start_time(splinterd::handoff_window_carry::process_start_time(pid).unwrap());
        peer
    }

    /// AGENTS.md §4.4, the operator decision of 2026-10-20: a connection
    /// trusted at `Hello` stays trusted when an upgrade replaces the
    /// installed client, because its process still runs what it ran then;
    /// a new connection from the replaced inode is refused at `Hello`.
    #[test]
    fn a_connection_trusted_at_hello_survives_replacement_of_the_installed_client() {
        let prefix = scratch_prefix();
        let client = prefix.client();
        install_copy(system_sleep(), &client);
        let child = spawn_sleep(&client);
        let mut connection = peer_for(child.pid(), &prefix);
        // Before its Hello a connection holds no client trust.
        assert!(!connection.connection_matches_splinterm());
        say_hello(&mut connection, child.pid());
        assert!(connection.connection_matches_splinterm());

        // A byte-identical copy renamed over the installed client: a new
        // inode, as a package upgrade installs.
        let original = fs::metadata(&client).unwrap().ino();
        install_copy(system_sleep(), &client);
        assert_ne!(fs::metadata(&client).unwrap().ino(), original);
        assert!(!connection.is_matching_splinterm());
        assert!(connection.connection_matches_splinterm());

        // A new connection from the same, now replaced, inode, in a daemon
        // where no connection of the process is registered as trusted.
        let mut fresh = peer_for(child.pid(), &prefix);
        say_hello(&mut fresh, child.pid());
        assert!(!fresh.connection_matches_splinterm());

        // Connection trust is not identity: grants still match the process.
        let mut untrusted = connection.clone();
        untrusted.hello_client = HelloClientMatch::default();
        assert_eq!(untrusted, connection);

        // The process is gone: fail closed.
        drop(child);
        assert!(!connection.connection_matches_splinterm());
        fs::remove_dir_all(prefix.directory()).unwrap();
    }

    /// A peer that executes another program while holding its socket loses
    /// the trust its connection established at `Hello`, though the
    /// installed client is unchanged.
    #[test]
    fn a_peer_that_executes_another_program_loses_its_connection_trust() {
        let shell = ["/bin/sh", "/usr/bin/sh"]
            .into_iter()
            .map(Path::new)
            .find(|path| path.is_file())
            .expect("a shell");
        let prefix = scratch_prefix();
        let client = prefix.client();
        install_copy(shell, &client);
        let mut child = spawn_owned(
            Command::new(&client)
                .arg("-c")
                .arg(format!("read line; exec {} 30", system_sleep().display()))
                .stdin(Stdio::piped()),
        );
        let pid = child.pid();
        let trusted = TrustedClientProcesses::default();
        let mut connection = peer_for_process(pid, &prefix);
        say_hello_among(&mut connection, pid, &trusted);
        assert!(connection.connection_matches_splinterm());
        let _registration = connection.register_trusted_client(&trusted).unwrap();
        let at_hello = executable_file_identity(pid).unwrap();

        child.0.stdin.take().unwrap().write_all(b"exec\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while executable_file_identity(pid).is_ok_and(|current| current == at_hello) {
            assert!(Instant::now() < deadline, "the peer never executed sleep");
            std::thread::sleep(Duration::from_millis(5));
        }
        // What it ran at Hello is still the installed client: only the live
        // read sees the exec.
        assert!(connection.is_matching_splinterm());
        assert!(!connection.connection_matches_splinterm());
        // Its trusted connection is still open, but a new connection from
        // the process, now executing sleep, does not inherit that trust.
        let mut after_exec = peer_for_process(pid, &prefix);
        say_hello_among(&mut after_exec, pid, &trusted);
        assert!(!after_exec.connection_matches_splinterm());
        drop(child);
        fs::remove_dir_all(prefix.directory()).unwrap();
    }

    /// The operator decision of 2026-10-20: while a process holds a
    /// connection trusted at its own `Hello` and still executes the same
    /// file, its new connections are trusted too, after the installed
    /// client was replaced. Another process on the old inode, a later
    /// process reusing the PID, and the process once every trusted
    /// connection of it closed are not.
    #[test]
    fn a_process_holding_a_trusted_connection_opens_more_after_replacement() {
        let trusted = TrustedClientProcesses::default();
        let prefix = scratch_prefix();
        let client = prefix.client();
        install_copy(system_sleep(), &client);
        let window = spawn_sleep(&client);
        let stale = prefix.directory().join("stale-splinterm");
        fs::hard_link(&client, &stale).unwrap();
        let mut first = peer_for_process(window.pid(), &prefix);
        say_hello_among(&mut first, window.pid(), &trusted);
        assert!(first.connection_matches_splinterm());
        let first_registration = first.register_trusted_client(&trusted).unwrap();
        assert_eq!(trusted.len(), 1);

        install_copy(system_sleep(), &client);
        let mut second = peer_for_process(window.pid(), &prefix);
        assert!(!second.is_matching_splinterm());
        say_hello_among(&mut second, window.pid(), &trusted);
        assert!(second.connection_matches_splinterm());
        let second_registration = second.register_trusted_client(&trusted).unwrap();
        // Counted by connection, under one process entry.
        assert_eq!(trusted.len(), 1);

        // Another process executing the old inode.
        let other = spawn_sleep(&stale);
        let mut intruder = peer_for_process(other.pid(), &prefix);
        say_hello_among(&mut intruder, other.pid(), &trusted);
        assert!(!intruder.connection_matches_splinterm());

        // A later process reusing the Window's PID has another start time
        // and pidfd (a PID cannot be reused on demand, so it is simulated
        // with the other process's pidfd).
        let mut reused = PeerIdentity {
            start_time: first.start_time().map(|start_time| start_time + 1),
            ..peer_for_process(window.pid(), &prefix)
        };
        reused.record_hello_client_match_with(|| Ok(Some(pidfd_of(other.pid()))), &trusted);
        assert!(!reused.connection_matches_splinterm());

        // Once every trusted connection of the process closed, nothing
        // lingers.
        drop(first_registration);
        assert_eq!(trusted.len(), 1);
        drop(second_registration);
        assert_eq!(trusted.len(), 0);
        let mut after_close = peer_for_process(window.pid(), &prefix);
        say_hello_among(&mut after_close, window.pid(), &trusted);
        assert!(!after_close.connection_matches_splinterm());
        drop(other);
        drop(window);
        fs::remove_dir_all(prefix.directory()).unwrap();
    }

    /// The 2026-10-18 snapshot exception reaches connection trust unchanged:
    /// only an adopted daemon that pinned the snapshot trusts a connection
    /// executing it.
    #[test]
    fn connection_trust_keeps_the_pinned_snapshot_rule() {
        let snapshot = sealed_copy(system_sleep());
        let child = spawn_sleep(Path::new(&format!(
            "/proc/self/fd/{}",
            snapshot.as_raw_fd()
        )));
        let pinned = Arc::new(PinnedClientSnapshot::pin(snapshot.try_clone().unwrap()).unwrap());
        let prefix = test_install_prefix();
        let mut fresh = peer_for(child.pid(), &prefix);
        say_hello(&mut fresh, child.pid());
        assert!(!fresh.connection_matches_splinterm());
        let mut adopted = PeerIdentity {
            pinned_client: Some(pinned),
            ..peer_for(child.pid(), &prefix)
        };
        say_hello(&mut adopted, child.pid());
        assert!(adopted.connection_matches_splinterm());
        drop(child);
        assert!(!adopted.connection_matches_splinterm());
    }

    /// Without a duplicated pidfd no trust is recorded; without
    /// `SO_PEERPIDFD` the recorded start time must still name the process.
    #[test]
    fn connection_trust_fails_closed_without_proof_of_the_same_process() {
        let prefix = scratch_prefix();
        let client = prefix.client();
        install_copy(system_sleep(), &client);
        let child = spawn_sleep(&client);
        let pid = child.pid();

        let mut unduplicated = peer_for(pid, &prefix);
        let nothing_trusted = TrustedClientProcesses::default();
        unduplicated
            .record_hello_client_match_with(|| Err(anyhow::anyhow!("no pidfd")), &nothing_trusted);
        assert!(!unduplicated.connection_matches_splinterm());

        let mut unpinned = peer_for(pid, &prefix);
        unpinned.record_hello_client_match_with(|| Ok(None), &nothing_trusted);
        // Neither a start time nor a pidfs inode: never registered, so no
        // later connection could inherit from a PID alone.
        assert!(unpinned.register_trusted_client(&nothing_trusted).is_none());
        assert!(!unpinned.connection_matches_splinterm());
        let start_time = splinterd::handoff_window_carry::process_start_time(pid).unwrap();
        unpinned.install_start_time(start_time + 1);
        assert!(!unpinned.connection_matches_splinterm());
        unpinned.install_start_time(start_time);
        assert!(unpinned.connection_matches_splinterm());

        // A peer that never matched at Hello records nothing.
        let other_child = spawn_sleep(system_sleep());
        let mut other = peer_for(other_child.pid(), &prefix);
        say_hello(&mut other, other_child.pid());
        assert!(!other.connection_matches_splinterm());
        drop(other_child);
        drop(child);
        fs::remove_dir_all(prefix.directory()).unwrap();
    }

    #[test]
    fn a_memfd_executable_is_identified_through_its_descriptor() {
        let memfd = rustix::fs::memfd_create("splinterm", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
        let mut image = fs::File::from(memfd);
        image.write_all(&fs::read(system_sleep()).unwrap()).unwrap();
        let expected = image.metadata().unwrap();
        // exec opens the image before close-on-exec descriptors are closed.
        let child = spawn_sleep(Path::new(&format!("/proc/self/fd/{}", image.as_raw_fd())));

        let label = fs::read_link(format!("/proc/{}/exe", child.pid())).unwrap();
        assert!(label.to_string_lossy().starts_with("/memfd:splinterm"));
        assert_eq!(
            executable_file_identity(child.pid()).unwrap(),
            (expected.dev(), expected.ino())
        );
        let peer = peer_for(child.pid(), &test_install_prefix());
        assert!(!peer.is_matching_splinterm());
        assert!(
            peer.requester_label().ends_with(&format!(
                " (deleted) [memory image, path unverified; executable device {} inode {}]",
                expected.dev(),
                expected.ino()
            )),
            "{}",
            peer.requester_label()
        );
    }

    #[test]
    fn peer_identity_fails_closed_without_an_executing_process() {
        // Above every Linux pid_max: no such process.
        assert!(executable_file_identity(u32::MAX).is_err());
    }

    #[test]
    fn consent_labels_mark_every_path_that_does_not_name_the_executing_file() {
        let running = fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        let identity = fs::metadata(&running).unwrap();
        let (device, inode) = (identity.dev(), identity.ino());
        let marker = format!(" [path unverified; executable device {device} inode {inode}]");

        // Resolves here to the executing file: shown plain.
        assert_eq!(
            executable_label(&running, device, inode),
            running.to_string_lossy()
        );
        // Resolves here to a different file, as a binary at the same path in
        // another mount namespace would.
        let elsewhere = running.parent().unwrap();
        assert_eq!(
            executable_label(elsewhere, device, inode),
            format!("{}{marker}", elsewhere.display())
        );
        // Resolves nowhere, or relative.
        assert_eq!(
            executable_label(Path::new("/nonexistent/splinterm"), device, inode),
            format!("/nonexistent/splinterm{marker}")
        );
        assert_eq!(
            executable_label(Path::new("splinterm"), device, inode),
            format!("splinterm{marker}")
        );
        // The marker survives the length bound.
        let long = PathBuf::from(format!("/{}", "a".repeat(2 * MAX_REQUESTER_LABEL_CHARS)));
        let label = executable_label(&long, device, inode);
        assert_eq!(label.chars().count(), MAX_REQUESTER_LABEL_CHARS);
        assert!(label.ends_with(&marker));
    }

    #[test]
    fn pidfd_pinning_tolerates_only_an_unsupported_socket_option() {
        use nix::errno::Errno;

        assert!(matches!(
            supported_peer_pidfd(Err(Errno::ENOPROTOOPT)),
            Ok(None)
        ));
        for refused in [Errno::EINVAL, Errno::ESRCH, Errno::ENODATA, Errno::EMFILE] {
            assert!(supported_peer_pidfd(Err(refused)).is_err(), "{refused}");
        }
    }

    #[test]
    fn a_pidfd_for_another_or_exited_process_fails_closed() {
        let mut child = Command::new(system_sleep()).arg("30").spawn().unwrap();
        let child_pid = child.id();
        let pidfd = rustix::process::pidfd_open(
            rustix::process::Pid::from_raw(i32::try_from(child_pid).unwrap()).unwrap(),
            rustix::process::PidfdFlags::empty(),
        )
        .unwrap();

        verify_pidfd_pid(&pidfd, child_pid).unwrap();
        require_live_pidfd(&pidfd).unwrap();
        // SO_PEERCRED naming another process than the pidfd.
        assert!(verify_pidfd_pid(&pidfd, std::process::id()).is_err());

        child.kill().unwrap();
        child.wait().unwrap();
        // Exited and reaped: its PID may already belong to another process.
        assert!(require_live_pidfd(&pidfd).is_err());
        assert!(verify_pidfd_pid(&pidfd, child_pid).is_err());
    }

    #[test]
    fn only_exact_graphical_relay_mode_can_carry_the_human_role() {
        assert!(graphical_relay_arguments(&[
            b"/usr/bin/splinterm-relay",
            b"--graphical-stdio",
        ]));
        assert!(!graphical_relay_arguments(&[
            b"/usr/bin/splinterm-relay",
            b"--stdio",
        ]));
        assert!(!graphical_relay_arguments(&[
            b"/usr/bin/splinterm-relay",
            b"--graphical-stdio",
            b"extra",
        ]));
    }

    #[test]
    fn grants_are_bound_to_peer_splint_incarnation_and_scope() {
        let peer = PeerIdentity::for_test();
        let other_peer = PeerIdentity {
            pid: peer.pid.saturating_add(1),
            ..peer.clone()
        };
        let splint = SplintId::new();
        let other_splint = SplintId::new();
        let mut store = GrantStore::default();
        let grant = store.grant(
            &peer,
            splint,
            7,
            vec![AccessScope::Observe, AccessScope::Input],
        );

        assert_eq!(
            store.authorize(&peer, splint, 7, &[AccessScope::Observe]),
            Some(grant.grant.grant_id)
        );
        assert_eq!(
            store.authorize(&peer, splint, 7, &[AccessScope::Resize]),
            None
        );
        assert_eq!(
            store.authorize(&other_peer, splint, 7, &[AccessScope::Observe]),
            None
        );
        assert_eq!(
            store.authorize(&peer, other_splint, 7, &[AccessScope::Observe]),
            None
        );
        assert_eq!(
            store.authorize(&peer, splint, 8, &[AccessScope::Observe]),
            None
        );
    }

    #[test]
    fn lair_grants_are_peer_bound_dynamic_and_unrelated_lairs_fail_closed() {
        let peer = PeerIdentity::for_test();
        let other_peer = PeerIdentity {
            pid: peer.pid.saturating_add(1),
            ..peer.clone()
        };
        let lair_id = LairId::new();
        let other_lair = LairId::new();
        let mut store = GrantStore::default();
        let grant = store.grant_lair(
            &peer,
            lair_id,
            vec![AccessScope::Input, AccessScope::TopologyLayout],
        );

        assert_eq!(
            store.authorize_lair(&peer, lair_id, &[AccessScope::Input]),
            Some(grant.grant.grant_id)
        );
        assert_eq!(
            store.authorize_lair(&peer, lair_id, &[AccessScope::Resize]),
            None
        );
        assert_eq!(
            store.authorize_lair(&other_peer, lair_id, &[AccessScope::Input]),
            None
        );
        assert_eq!(
            store.authorize_lair(&peer, other_lair, &[AccessScope::Input]),
            None
        );

        let revoked = store.revoke_any(grant.grant.grant_id).unwrap();
        assert!(matches!(revoked, RevokedAuthorization::Lair(_)));
        assert_eq!(
            store.authorize_lair(&peer, lair_id, &[AccessScope::Input]),
            None
        );
    }

    #[test]
    fn expired_lair_grant_is_revoked_with_its_exact_identity() {
        let peer = PeerIdentity::for_test();
        let lair_id = LairId::new();
        let mut store = GrantStore::default();
        let grant = store.grant_lair(&peer, lair_id, vec![AccessScope::Input]);
        store.lair_grants[0].expires = Instant::now();

        let revoked = store.revoke_lair_if_expired(grant.grant.grant_id).unwrap();
        assert_eq!(revoked.grant, grant.grant);
        assert_eq!(revoked.authorization_revision, 2);
        assert!(store.lair_grants.is_empty());
    }

    #[test]
    fn grant_and_revoke_return_the_exact_atomic_authorization_revision() {
        let peer = PeerIdentity::for_test();
        let splint = SplintId::new();
        let mut store = GrantStore::default();
        let first = store.grant(&peer, splint, 1, vec![AccessScope::Observe]);
        assert_eq!(first.authorization_revision, 1);
        let second = store.grant(&peer, splint, 1, vec![AccessScope::Resize]);
        assert_eq!(second.authorization_revision, 2);
        assert_eq!(second.grant.grant_id, first.grant.grant_id);
        let revoked = store.revoke(first.grant.grant_id).unwrap();
        assert_eq!(revoked.authorization_revision, 3);
        assert_eq!(revoked.grant, second.grant);
    }

    #[test]
    fn revocation_removes_authority_and_records_bounded_metadata() {
        let peer = PeerIdentity::for_test();
        let splint = SplintId::new();
        let mut store = GrantStore::default();
        let grant = store.grant(&peer, splint, 1, vec![AccessScope::Observe]);
        assert!(store.revoke(grant.grant.grant_id).is_some());
        assert_eq!(
            store.authorize(&peer, splint, 1, &[AccessScope::Observe]),
            None
        );
        assert_eq!(store.audit.len(), 2);
        assert!(
            store
                .audit
                .iter()
                .all(|record| record.requester.len() <= 1024)
        );
        assert!(
            store
                .audit
                .iter()
                .all(|record| record.scopes.len() <= MAX_ACCESS_SCOPES)
        );
    }

    #[test]
    fn private_frames_reject_zero_and_oversized_lengths() {
        let mut zero = &0_u32.to_be_bytes()[..];
        assert!(read_bounded::<ConsentReply>(&mut zero).is_err());
        let oversized = u32::try_from(MAX_CONSENT_FRAME_BYTES + 1)
            .unwrap()
            .to_be_bytes();
        assert!(read_bounded::<ConsentReply>(&mut &oversized[..]).is_err());
    }

    /// Every prompt the daemon shows counts down the time it waits for the
    /// answer, in whole seconds rounded down: a Splint request or a forced
    /// takeover the fixed consent deadline, a workspace request what is
    /// left of its pending lifetime. Its origin is always local.
    #[test]
    fn every_daemon_prompt_counts_down_the_time_the_daemon_waits() {
        let peer = PeerIdentity::for_test();
        let splint = |deadline| {
            consent_prompt(
                &peer,
                ConsentTarget::Splint {
                    splint_id: SplintId::new(),
                    incarnation: 1,
                },
                vec![AccessScope::ControlTakeover],
                PromptDetails::once_only(),
                deadline,
            )
        };
        let lair = |deadline| {
            consent_prompt(
                &peer,
                ConsentTarget::Lair {
                    lair_id: LairId::new(),
                    lair_name: "Workspace".to_owned(),
                },
                AccessLevel::Look.scopes().to_vec(),
                PromptDetails {
                    agent_name: Some("agent".to_owned()),
                    level: Some(AccessLevel::Look),
                    ..PromptDetails::once_only()
                },
                deadline,
            )
        };
        // `prompt`, for Splint requests and forced takeover, waits the
        // consent deadline.
        let shown = splint(CONSENT_DEADLINE);
        assert_eq!(shown.respond_within_seconds, 20);
        assert_eq!(shown.origin, splinterm_protocol::ConsentOrigin::Local);
        // `prompt_lair` waits until the pending request expires. With no
        // whole second left it still counts one, so its dialog starts with
        // its time up and never shows a live Allow without a countdown.
        for (left, seconds) in [
            (Duration::from_millis(119_900), 119),
            (Duration::from_secs(45), 45),
            (Duration::from_millis(400), 1),
            (Duration::from_secs(600), 120),
            (Duration::ZERO, 1),
        ] {
            let shown = lair(left);
            assert_eq!(shown.respond_within_seconds, seconds, "{left:?}");
            assert_eq!(shown.origin, splinterm_protocol::ConsentOrigin::Local);
        }
        // What the daemon builds is a prompt the dialog accepts.
        for mut shown in [splint(CONSENT_DEADLINE), lair(Duration::from_secs(600))] {
            shown.capability = vec![7; CONSENT_CAPABILITY_BYTES];
            assert_eq!(shown.validate(), Ok(()));
        }
    }
}
