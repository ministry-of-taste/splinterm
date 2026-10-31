//! Trusted local Windows taking part in a daemon handoff (Plan 0061 M3.13b1;
//! ADR 0011, "Smooth local client recovery").
//!
//! A trusted-UI connection whose `Hello` sets `handoff_window` becomes its
//! Window's anchor: the one connection a handing-off daemon sends that
//! Window's `HandoffPrepare` frame on, and the only one that may acknowledge
//! it. An anchor is accepted only from a connection that passed the
//! trusted-UI check at `Hello`, at most one per Window process, and at most
//! [`MAX_HANDOFF_WINDOWS`] in all. The registry records the Window's process
//! as it was pinned when the anchor was verified: its PID and start time, a
//! close-on-exec duplicate of the connection's pidfd, and the device and
//! inode it executed. It also records every trusted-UI connection with the
//! Splints it is attached to, so a handoff groups a Window's connections by
//! process and names the trusted-UI processes that hold attachments without
//! declaring an anchor.
//!
//! Acknowledgement authority is this participant binding, not the trusted-UI
//! executable check. After a package upgrade every running Window executes
//! the replaced inode, which `<prefix>/splinterm` no longer names, so
//! re-checking that path would refuse every acknowledgement. An anchor may
//! acknowledge only the prepare frame sent to it, echoing its generation and
//! its prepare id, exactly once, while the attempt waits, and only while its
//! process is the one it pinned; see
//! [`HandoffWindows::complete_acknowledgement`]. Every attempt in one daemon
//! names the same generation, but each frame carries a prepare id the process
//! never reuses, so a late answer to an earlier attempt's frame matches
//! nothing.

use std::{
    collections::{HashMap, HashSet},
    os::fd::OwnedFd,
    path::Path,
    sync::{Arc, Mutex},
};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use splinterm_core::{SplintId, Topology};
use splinterm_protocol::{
    HandoffWindowDojo, MAX_HANDOFF_DOJO_NAME_BYTES, MAX_HANDOFF_WINDOW_DOJOS, MAX_HANDOFF_WINDOWS,
};
use tokio::sync::mpsc;
use tracing::warn;

use crate::ControlFrame;

/// The daemon's Window registry, shared by connections and handoff attempts.
pub(crate) type SharedHandoffWindows = Arc<Mutex<HandoffWindows>>;

/// Device and inode of an executable file.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct FileIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

impl FileIdentity {
    /// The file a path names now, following symlinks as `exec` would.
    pub(crate) fn of_path(path: &Path) -> Option<Self> {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(path).ok().map(|metadata| Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

/// A Window's client process as its anchor connection was verified.
#[derive(Clone, Debug)]
pub(crate) struct WindowProcess {
    pub(crate) pid: u32,
    /// Start time in clock ticks since boot; `None` when it could not be
    /// read, which leaves the process unpinned.
    pub(crate) start_time: Option<u64>,
    /// A duplicate of the connection's pidfd; `None` on a kernel without
    /// `SO_PEERPIDFD`, which leaves the process unpinned.
    pub(crate) pidfd: Option<Arc<OwnedFd>>,
    /// The file the process executed at `Hello`.
    pub(crate) executable: FileIdentity,
}

/// Whether a Window is still the process its anchor pinned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProcessPin {
    /// The pinned process is alive and its start time still matches.
    Live,
    /// The pinned process has exited.
    Exited,
    /// Nothing pins it: no pidfd, no recorded start time, or a start time
    /// that cannot be read now.
    Unpinned,
}

impl WindowProcess {
    /// Checks the pin: the pidfd shows the process alive and its start time
    /// still matches. The start time is read between two pidfd checks, so a
    /// pidfd still alive after the read proves the read described the pinned
    /// process, not a later one reusing its PID. A pidfd that cannot be
    /// polled proves nothing either way and reads as unpinned: a handoff then
    /// names the Window instead of dropping it, and an acknowledgement, which
    /// needs [`ProcessPin::Live`], still fails closed.
    pub(crate) fn pin(&self) -> ProcessPin {
        let (Some(pidfd), Some(start_time)) = (&self.pidfd, self.start_time) else {
            return ProcessPin::Unpinned;
        };
        match pidfd_exited(pidfd) {
            Some(false) => {}
            Some(true) => return ProcessPin::Exited,
            None => return ProcessPin::Unpinned,
        }
        let current = crate::agent_status::process_start_time(self.pid);
        match pidfd_exited(pidfd) {
            Some(false) => {}
            Some(true) => return ProcessPin::Exited,
            None => return ProcessPin::Unpinned,
        }
        if current == Some(start_time) {
            ProcessPin::Live
        } else {
            ProcessPin::Unpinned
        }
    }
}

/// Whether a pidfd reports its process exited: a pidfd becomes readable when
/// its process exits. `None` when it cannot be polled.
pub(crate) fn pidfd_exited(pidfd: &OwnedFd) -> Option<bool> {
    let mut descriptors = [PollFd::new(pidfd, PollFlags::IN)];
    let immediately = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    poll(&mut descriptors, Some(&immediately))
        .ok()
        .map(|ready| ready != 0)
}

/// One trusted-UI connection: its process, and the Splints it holds
/// terminal attachments to.
struct TrustedUiConnection {
    pid: u32,
    start_time: Option<u64>,
    attached: HashMap<SplintId, usize>,
}

impl TrustedUiConnection {
    fn is_process(&self, pid: u32, start_time: Option<u64>) -> bool {
        self.pid == pid && self.start_time == start_time
    }
}

/// One declared Window anchor.
struct WindowAnchor {
    process: WindowProcess,
    /// The anchor connection's control channel, which carries the prepare
    /// frame to its writer.
    control: mpsc::Sender<ControlFrame>,
    /// Present only while an attempt waits for this anchor's
    /// acknowledgement.
    pending: Option<PendingPrepare>,
}

/// A prepare frame sent to an anchor and not yet acknowledged.
struct PendingPrepare {
    daemon_generation: u64,
    prepare_id: u64,
    index: u8,
    events: mpsc::UnboundedSender<WindowEvent>,
}

/// What a handoff attempt learns about one prepared Window while it waits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WindowEvent {
    /// The anchor acknowledged its prepare frame.
    Acknowledged(u8),
    /// The Window's process exited.
    Exited(u8),
    /// The anchor's writer wrote the whole prepare frame.
    Sent(u8),
    /// The anchor's writer could not send the whole prepare frame.
    NotSent(u8),
}

/// The anchor writer's report on one prepare frame, into the attempt's
/// events. Dropped unreported, because the frame was never written or the
/// writer went away, it reports the frame as not sent.
pub(crate) struct PrepareSendReport {
    index: u8,
    events: Option<mpsc::UnboundedSender<WindowEvent>>,
}

impl PrepareSendReport {
    pub(crate) const fn new(index: u8, events: mpsc::UnboundedSender<WindowEvent>) -> Self {
        Self {
            index,
            events: Some(events),
        }
    }

    pub(crate) fn report(mut self, sent: bool) {
        self.send(sent);
    }

    fn send(&mut self, sent: bool) {
        if let Some(events) = self.events.take() {
            let _ = events.send(if sent {
                WindowEvent::Sent(self.index)
            } else {
                WindowEvent::NotSent(self.index)
            });
        }
    }
}

impl Drop for PrepareSendReport {
    fn drop(&mut self) {
        self.send(false);
    }
}

/// Why an anchor declaration is refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AnchorRefusal {
    /// A handoff attempt already took its Windows; a Window declared now
    /// could not take part in it.
    HandoffInProgress,
    /// The process already has an anchor connection.
    AlreadyDeclared,
    /// [`MAX_HANDOFF_WINDOWS`] anchors are declared.
    Full,
    /// The connection's writer has gone, so its `Hello` cannot be queued.
    WriterClosed,
}

/// One declared Window as an attempt took it.
pub(crate) struct AnchorSnapshot {
    pub(crate) connection_id: u64,
    pub(crate) process: WindowProcess,
    pub(crate) control: mpsc::Sender<ControlFrame>,
}

/// A trusted-UI process that holds attachments but declared no anchor.
pub(crate) struct UndeclaredProcess {
    pub(crate) pid: u32,
    pub(crate) start_time: Option<u64>,
}

/// Every trusted-UI connection and declared Window anchor.
#[derive(Default)]
pub(crate) struct HandoffWindows {
    connections: HashMap<u64, TrustedUiConnection>,
    anchors: HashMap<u64, WindowAnchor>,
    /// Set while an attempt keeps the Windows it took; see [`WindowAttempt`].
    attempt: bool,
}

impl HandoffWindows {
    /// Records a connection that passed the trusted-UI check at `Hello`.
    pub(crate) fn register_connection(
        &mut self,
        connection_id: u64,
        pid: u32,
        start_time: Option<u64>,
    ) {
        self.connections.insert(
            connection_id,
            TrustedUiConnection {
                pid,
                start_time,
                attached: HashMap::new(),
            },
        );
    }

    /// Declares `connection_id`, already registered as trusted UI, as its
    /// Window's anchor, after queueing the connection's server `hello` on its
    /// control channel. Both happen under the registry lock, so no attempt
    /// takes the anchor, and no prepare frame is queued for it, before its
    /// `Hello`. Nothing else is queued on a control channel before its
    /// `Hello`, so the queue has room.
    ///
    /// # Errors
    /// Refuses while an attempt holds its Windows, a second anchor from one
    /// process (the same PID and start time), an anchor past
    /// [`MAX_HANDOFF_WINDOWS`], and a connection whose writer has gone.
    pub(crate) fn declare_anchor(
        &mut self,
        connection_id: u64,
        process: WindowProcess,
        control: mpsc::Sender<ControlFrame>,
        hello: ControlFrame,
    ) -> Result<(), AnchorRefusal> {
        if self.attempt {
            return Err(AnchorRefusal::HandoffInProgress);
        }
        if self.anchors.values().any(|anchor| {
            anchor.process.pid == process.pid && anchor.process.start_time == process.start_time
        }) {
            return Err(AnchorRefusal::AlreadyDeclared);
        }
        if self.anchors.len() >= MAX_HANDOFF_WINDOWS {
            return Err(AnchorRefusal::Full);
        }
        control
            .try_send(hello)
            .map_err(|_| AnchorRefusal::WriterClosed)?;
        self.anchors.insert(
            connection_id,
            WindowAnchor {
                process,
                control,
                pending: None,
            },
        );
        Ok(())
    }

    /// Forgets a closed connection, and its anchor if it was one. A pending
    /// acknowledgement goes with it.
    pub(crate) fn remove_connection(&mut self, connection_id: u64) {
        self.connections.remove(&connection_id);
        self.anchors.remove(&connection_id);
    }

    pub(crate) fn is_anchor(&self, connection_id: u64) -> bool {
        self.anchors.contains_key(&connection_id)
    }

    /// Whether `connection_id` is an open trusted-UI connection.
    pub(crate) fn is_trusted_ui_connection(&self, connection_id: u64) -> bool {
        self.connections.contains_key(&connection_id)
    }

    #[cfg(test)]
    pub(crate) fn anchor_count(&self) -> usize {
        self.anchors.len()
    }

    #[cfg(test)]
    pub(crate) fn connection_count(&self) -> usize {
        self.connections.len()
    }

    /// Marks an attempt and takes every anchor, and every trusted-UI process
    /// with an attachment but no anchor.
    fn begin_attempt(&mut self) -> (Vec<AnchorSnapshot>, Vec<UndeclaredProcess>) {
        self.attempt = true;
        let mut anchors = self
            .anchors
            .iter()
            .map(|(&connection_id, anchor)| AnchorSnapshot {
                connection_id,
                process: anchor.process.clone(),
                control: anchor.control.clone(),
            })
            .collect::<Vec<_>>();
        anchors.sort_by_key(|anchor| anchor.connection_id);
        let mut undeclared = Vec::<UndeclaredProcess>::new();
        let mut connections = self.connections.iter().collect::<Vec<_>>();
        connections.sort_by_key(|(connection_id, _)| **connection_id);
        for (_, connection) in connections {
            let declared = self
                .anchors
                .values()
                .any(|anchor| connection.is_process(anchor.process.pid, anchor.process.start_time));
            let listed = undeclared
                .iter()
                .any(|process| connection.is_process(process.pid, process.start_time));
            if !declared && !listed && !connection.attached.is_empty() {
                undeclared.push(UndeclaredProcess {
                    pid: connection.pid,
                    start_time: connection.start_time,
                });
            }
        }
        (anchors, undeclared)
    }

    /// Opens an anchor's acknowledgement for the prepare frame `prepare_id`,
    /// before the frame is queued so a fast answer is never lost. Returns
    /// false when the anchor has gone.
    pub(crate) fn await_acknowledgement(
        &mut self,
        connection_id: u64,
        daemon_generation: u64,
        prepare_id: u64,
        index: u8,
        events: mpsc::UnboundedSender<WindowEvent>,
    ) -> bool {
        let Some(anchor) = self.anchors.get_mut(&connection_id) else {
            return false;
        };
        anchor.pending = Some(PendingPrepare {
            daemon_generation,
            prepare_id,
            index,
            events,
        });
        true
    }

    /// The pinned process of the anchor `connection_id`, whose participant
    /// binding the caller checks before [`Self::complete_acknowledgement`].
    pub(crate) fn anchor_process(&self, connection_id: u64) -> Option<WindowProcess> {
        self.anchors
            .get(&connection_id)
            .map(|anchor| anchor.process.clone())
    }

    /// Consumes the pending acknowledgement that matches exactly this anchor,
    /// generation, and prepare id, and tells the waiting attempt; the check
    /// and the consumption are one step under the registry lock. Returns
    /// false, and changes nothing, for any other: no prepare pending, the
    /// wait ended, a second acknowledgement, another generation, or another
    /// frame's id, a late answer to an earlier attempt's frame among them.
    pub(crate) fn complete_acknowledgement(
        &mut self,
        connection_id: u64,
        daemon_generation: u64,
        prepare_id: u64,
    ) -> bool {
        let Some(anchor) = self.anchors.get_mut(&connection_id) else {
            return false;
        };
        if anchor.pending.as_ref().is_none_or(|pending| {
            pending.daemon_generation != daemon_generation || pending.prepare_id != prepare_id
        }) {
            return false;
        }
        let Some(pending) = anchor.pending.take() else {
            return false;
        };
        // Sent under the registry lock: once an attempt closes the
        // acknowledgements, every acknowledgement that succeeded is queued.
        let _ = pending
            .events
            .send(WindowEvent::Acknowledged(pending.index));
        true
    }

    /// Ends every pending acknowledgement: the attempt stopped waiting.
    pub(crate) fn close_acknowledgements(&mut self) {
        for anchor in self.anchors.values_mut() {
            anchor.pending = None;
        }
    }

    /// Ends one anchor's pending acknowledgement because its frame could not
    /// be sent completely; a later acknowledgement of it then fails.
    pub(crate) fn abandon_acknowledgement(&mut self, connection_id: u64) {
        if let Some(anchor) = self.anchors.get_mut(&connection_id) {
            anchor.pending = None;
        }
    }

    /// Every trusted-UI connection registered for the process, ascending:
    /// the old connections a carried Window's manifest record names.
    pub(crate) fn process_connections(&self, pid: u32, start_time: Option<u64>) -> Vec<u64> {
        let mut connections = self
            .connections
            .iter()
            .filter(|(_, connection)| connection.is_process(pid, start_time))
            .map(|(&connection_id, _)| connection_id)
            .collect::<Vec<_>>();
        connections.sort_unstable();
        connections
    }

    /// Every Splint the process's trusted-UI connections are attached to.
    pub(crate) fn attached_splints(&self, pid: u32, start_time: Option<u64>) -> HashSet<SplintId> {
        self.connections
            .values()
            .filter(|connection| connection.is_process(pid, start_time))
            .flat_map(|connection| connection.attached.keys().copied())
            .collect()
    }
}

/// One attempt's mark on the registry. While it lives, no anchor is
/// declared; dropping it ends the mark and every pending acknowledgement.
pub(crate) struct WindowAttempt {
    windows: SharedHandoffWindows,
}

impl Drop for WindowAttempt {
    fn drop(&mut self) {
        if let Ok(mut windows) = self.windows.lock() {
            windows.attempt = false;
            windows.close_acknowledgements();
        }
    }
}

/// Marks an attempt and takes its Windows; see [`HandoffWindows::begin_attempt`].
pub(crate) fn begin_attempt(
    windows: &SharedHandoffWindows,
) -> (WindowAttempt, Vec<AnchorSnapshot>, Vec<UndeclaredProcess>) {
    let (anchors, undeclared) = windows.lock().unwrap().begin_attempt();
    (
        WindowAttempt {
            windows: Arc::clone(windows),
        },
        anchors,
        undeclared,
    )
}

/// One terminal attachment of a trusted-UI connection, counted while it is
/// held.
pub(crate) struct WindowAttachment {
    windows: SharedHandoffWindows,
    connection_id: u64,
    splint_id: SplintId,
}

/// Counts an attachment of a registered trusted-UI connection; `None` for
/// any other connection.
pub(crate) fn attach(
    windows: &SharedHandoffWindows,
    connection_id: u64,
    splint_id: SplintId,
) -> Option<WindowAttachment> {
    let mut registry = windows.lock().unwrap();
    let connection = registry.connections.get_mut(&connection_id)?;
    *connection.attached.entry(splint_id).or_default() += 1;
    Some(WindowAttachment {
        windows: Arc::clone(windows),
        connection_id,
        splint_id,
    })
}

impl Drop for WindowAttachment {
    fn drop(&mut self) {
        let Ok(mut registry) = self.windows.lock() else {
            return;
        };
        let Some(connection) = registry.connections.get_mut(&self.connection_id) else {
            return;
        };
        if let Some(count) = connection.attached.get_mut(&self.splint_id) {
            *count -= 1;
            if *count == 0 {
                connection.attached.remove(&self.splint_id);
            }
        }
    }
}

/// One Window that acknowledged its prepare frame and stays in the attempt.
pub(crate) struct ParticipatingWindow {
    /// The Window's index in this attempt's prompt numbering.
    pub(crate) prompt_index: u8,
    pub(crate) anchor_connection_id: u64,
    pub(crate) process: WindowProcess,
    /// The daemon end of the Window's continuation channel. Dropping it ends
    /// the channel, and the Window reads end of file.
    pub(crate) continuation: OwnedFd,
}

/// One Window the attempt carries across exec (Plan 0061 M3.13b2): what its
/// manifest record names, and the two descriptors exec inherits for it.
pub(crate) struct CarriedWindow {
    /// The Window's dense manifest index, from 0 in prompt order over the
    /// carried Windows only: its descriptors are inherited at
    /// `ClientProcess(slot)` and `Continuation(slot)`. A prompt index also
    /// counts Windows dropped by a decision and is never used as a slot.
    pub(crate) slot: u8,
    /// The Window's index in the attempt's prompt numbering, for diagnostics.
    pub(crate) prompt_index: u8,
    pub(crate) anchor_connection_id: u64,
    pub(crate) pid: u32,
    pub(crate) start_time: u64,
    /// The file the process executed at `Hello`.
    pub(crate) executable: FileIdentity,
    /// The anchor's pidfd, shared with the registry's record of it.
    pub(crate) pidfd: Arc<OwnedFd>,
    /// The daemon end of the Window's continuation channel.
    pub(crate) continuation: OwnedFd,
}

/// The Windows one attempt carries, with its mark on the registry.
///
/// Dropping it, on any cancel, when the exec boundary returns, or when the
/// attempt otherwise ends, closes every daemon end, so each Window reads end
/// of file without an outcome and knows the attempt ended. A successful exec
/// instead carries each Window's pidfd and continuation end to the adopting
/// daemon, which sends the outcome.
pub(crate) struct HandoffWindowParticipation {
    pub(crate) windows: Vec<CarriedWindow>,
    _attempt: WindowAttempt,
}

impl HandoffWindowParticipation {
    /// Numbers the participating Windows densely from 0 in prompt order.
    /// A participant always passed its pin check, so it has a pidfd and a
    /// start time; one without either could not be carried and is dropped,
    /// which closes its continuation channel.
    pub(crate) fn new(mut windows: Vec<ParticipatingWindow>, attempt: WindowAttempt) -> Self {
        windows.sort_by_key(|window| window.prompt_index);
        let mut carried = Vec::with_capacity(windows.len());
        for window in windows {
            let ParticipatingWindow {
                prompt_index,
                anchor_connection_id,
                process,
                continuation,
            } = window;
            let (Some(pidfd), Some(start_time)) = (process.pidfd, process.start_time) else {
                warn!(
                    prompt_index,
                    pid = process.pid,
                    "a participating Window is not pinned and cannot be carried"
                );
                continue;
            };
            // At most `MAX_HANDOFF_WINDOWS` anchors take part.
            let Ok(slot) = u8::try_from(carried.len()) else {
                continue;
            };
            carried.push(CarriedWindow {
                slot,
                prompt_index,
                anchor_connection_id,
                pid: process.pid,
                start_time,
                executable: process.executable,
                pidfd,
                continuation,
            });
        }
        Self {
            windows: carried,
            _attempt: attempt,
        }
    }
}

/// The Dojos holding any of `splints`, named for a decision prompt in
/// Topology order, at most [`MAX_HANDOFF_WINDOW_DOJOS`]. A Dojo whose name a
/// prompt cannot carry is left out.
pub(crate) fn window_dojos(
    topology: &Topology,
    splints: &HashSet<SplintId>,
) -> Vec<HandoffWindowDojo> {
    topology
        .lairs()
        .flat_map(|lair| lair.dojos.iter().map(move |dojo| (lair.id, dojo)))
        .filter(|(_, dojo)| {
            splints
                .iter()
                .any(|&splint_id| dojo.root.find_splint(splint_id).is_some())
                && !dojo.name.is_empty()
                && dojo.name.len() <= MAX_HANDOFF_DOJO_NAME_BYTES
                && dojo.name.trim() == dojo.name
        })
        .take(MAX_HANDOFF_WINDOW_DOJOS)
        .map(|(lair_id, dojo)| HandoffWindowDojo {
            lair_id,
            dojo_id: dojo.id,
            name: dojo.name.clone(),
        })
        .collect()
}
