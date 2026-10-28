//! The parent side of a trusted dialog child that this process starts
//! itself: the upgrade and restart questions (`upgrade_dialog`,
//! `launcher_check`) and a remote Window's agent access prompts
//! (`remote_consent`, ADR 0015).
//!
//! The child gets a private socketpair on its stdin and stdout. The parent
//! writes one bounded request frame carrying a fresh random capability and
//! reads one bounded reply frame that must echo it. Every way out kills and
//! reaps the child unless it answered, and only after reaping removes its
//! dialog shortcut entry (`splinterd::dialog_shortcut`), so no dialog stays
//! on screen and no Hyprland binding stays behind. A termination signal
//! removes the entries of the dialogs being asked before the process exits.
//!
//! A question can be cancelled while it is asked: the child is killed and
//! reaped and nothing it might still answer is read.

use std::{
    future::Future,
    io,
    net::Shutdown,
    os::{fd::OwnedFd, unix::net::UnixStream},
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde::{Serialize, de::DeserializeOwned};
use splinterd::{
    child_stderr::{STDERR_TAIL_WAIT, StderrTail, child_failure},
    dialog_shortcut::{DialogShortcutLease, Hyprctl, SystemHyprctl},
};

use super::consent::{read_private_frame_bounded, write_private_frame_bounded};

/// Random bytes the child must echo, as the consent dialog does.
pub(in crate::app) const CAPABILITY_BYTES: usize = 32;

/// A reply frame a dialog child sends, echoing the request's capability.
pub(in crate::app) trait DialogReplyFrame:
    DeserializeOwned + Send + 'static
{
    /// The capability the reply echoes.
    fn capability(&self) -> &[u8];
}

impl DialogReplyFrame for splinterm_protocol::ConsentReply {
    fn capability(&self) -> &[u8] {
        &self.capability
    }
}

/// How a dialog question ended without an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::app) enum DialogOutcome<R> {
    /// The child answered, echoing the capability.
    Reply(R),
    /// Nobody answered within the budget; the child was killed and reaped.
    NoAnswer,
    /// The asker cancelled the question; the child was killed and reaped
    /// and nothing it answered was read.
    Cancelled,
}

/// Kills and reaps the dialog child unless it already answered, so an
/// abandoned question (the daemon ended the attempt, or the deadline passed)
/// never leaves a dialog on screen. Once the child is reaped, by any path,
/// its dialog shortcut registration is removed, so a killed dialog leaves no
/// Hyprland binding behind (`splinterd::dialog_shortcut`).
pub(in crate::app) struct DialogChild<H: Hyprctl = SystemHyprctl> {
    child: Option<Child>,
    /// Dropped only after `child` is reaped.
    shortcut: Option<DialogShortcutLease<H>>,
}

impl DialogChild {
    #[cfg(test)]
    fn new(child: Child) -> Self {
        Self::with_lease(child, DialogShortcutLease::for_child)
    }
}

impl<H: Hyprctl> DialogChild<H> {
    /// Guards `child`, listed for signal cleanup, with the shortcut lease
    /// `lease` makes for its PID.
    fn with_lease(child: Child, lease: impl FnOnce(u32) -> DialogShortcutLease<H>) -> Self {
        asking().push(child.id());
        let shortcut = lease(child.id());
        Self {
            child: Some(child),
            shortcut: Some(shortcut),
        }
    }

    /// Kills the child unless it already exited, reaps it, and returns how
    /// it ended. Its shortcut registration is removed after the reaping, as
    /// on drop.
    fn end(mut self) -> Option<ExitStatus> {
        let mut child = self.child.take()?;
        // Killing an exited, unreaped child does nothing; the wait still
        // returns the status it exited with.
        let _ = child.kill();
        child.wait().ok()
    }
}

impl<H: Hyprctl + Send + 'static> DialogChild<H> {
    /// Hands a child that has answered to a detached thread, which gives it
    /// a moment to exit and then reaps it, so the answer reaches the daemon
    /// without waiting on the child.
    fn finish(mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let shortcut = self.shortcut.take();
        // Without a thread the child still exits on its own after replying,
        // and init reaps it once this process exits; it removes its own
        // shortcut registration as it exits.
        let _ = std::thread::Builder::new()
            .name("dialog-reaper".to_owned())
            .spawn(move || {
                let started = Instant::now();
                let mut reaped = false;
                while started.elapsed() < Duration::from_secs(2) {
                    if matches!(child.try_wait(), Ok(Some(_))) {
                        reaped = true;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                if !reaped {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                if let Some(shortcut) = shortcut {
                    stop_asking(shortcut.pid());
                    drop(shortcut);
                }
            });
    }
}

impl<H: Hyprctl> Drop for DialogChild<H> {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(shortcut) = self.shortcut.take() {
            stop_asking(shortcut.pid());
            drop(shortcut);
        }
    }
}

/// Dialog children this process is asking now, by PID, so a termination
/// signal can remove their dialog shortcut entries before the process exits.
static ASKING: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

fn asking() -> std::sync::MutexGuard<'static, Vec<u32>> {
    ASKING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Forgets a dialog child once its entry is being removed by its lease.
fn stop_asking(pid: u32) {
    asking().retain(|asked| *asked != pid);
}

/// Removes the dialog shortcut entries of every dialog being asked.
fn unregister_asking() {
    let pids = asking().clone();
    for pid in pids {
        splinterd::dialog_shortcut::unregister_with(&SystemHyprctl, pid);
    }
}

/// Ctrl+C or `SIGTERM` would otherwise end this process before a dialog
/// child's guard runs: the dialog dies with it (`PR_SET_PDEATHSIG`) and its
/// dialog shortcut entry stays behind. From the first dialog on, for the
/// rest of the process, either signal first removes the entries of the
/// dialogs being asked and then exits as the signal would (128 + signal).
///
/// A graphical Window already exits on those signals through its
/// diagnostics task, which would race a second handler; there the removal
/// runs as that task's hook, before it exits.
fn install_dialog_signal_cleanup() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::signal::unix::{SignalKind, signal};

    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::AcqRel) {
        return;
    }
    if splinterm::diagnostics::run_before_signal_exit(unregister_asking) {
        return;
    }
    let mut terminate = signal(SignalKind::terminate()).ok();
    let mut interrupt = signal(SignalKind::interrupt()).ok();
    if terminate.is_none() && interrupt.is_none() {
        return;
    }
    tokio::spawn(async move {
        let signal_number = tokio::select! {
            Some(()) = async { terminate.as_mut()?.recv().await } => libc::SIGTERM,
            Some(()) = async { interrupt.as_mut()?.recv().await } => libc::SIGINT,
            else => return,
        };
        unregister_asking();
        std::process::exit(128 + signal_number);
    });
}

/// A fresh random capability for one dialog prompt.
pub(in crate::app) fn dialog_capability() -> Result<Vec<u8>> {
    let mut capability = vec![0_u8; CAPABILITY_BYTES];
    let mut offset = 0;
    while offset < capability.len() {
        offset += rustix::rand::getrandom(
            &mut capability[offset..],
            rustix::rand::GetRandomFlags::empty(),
        )
        .context("OS randomness unavailable")?;
    }
    Ok(capability)
}

/// Starts `command` as a trusted dialog child, sends it `request`, and
/// returns its reply. The reply must arrive within `budget`, as one frame of
/// at most `max_frame_bytes`, and echo `capability`. A reply that does not
/// arrive in time is [`DialogOutcome::NoAnswer`]; `cancel` finishing first is
/// [`DialogOutcome::Cancelled`]; anything else is an error, which names what
/// the child printed when it failed before answering. The child is killed
/// and reaped unless it answered, and an answered child is reaped in the
/// background. The caller validates `request` first.
pub(in crate::app) async fn ask_dialog_process<Q, R>(
    command: Command,
    request: Q,
    capability: Vec<u8>,
    max_frame_bytes: usize,
    budget: Duration,
    cancel: impl Future<Output = ()>,
) -> Result<DialogOutcome<R>>
where
    Q: Serialize + Send + 'static,
    R: DialogReplyFrame,
{
    ask_with_lease(
        command,
        DialogExchange {
            request,
            capability,
            max_frame_bytes,
            budget,
        },
        cancel,
        DialogShortcutLease::for_child,
    )
    .await
}

/// One question's request and its bounds.
struct DialogExchange<Q> {
    request: Q,
    capability: Vec<u8>,
    max_frame_bytes: usize,
    budget: Duration,
}

/// [`ask_dialog_process`] with the shortcut lease `lease` makes for the
/// child's PID.
async fn ask_with_lease<Q, R, H>(
    mut command: Command,
    exchange: DialogExchange<Q>,
    cancel: impl Future<Output = ()>,
    lease: impl FnOnce(u32) -> DialogShortcutLease<H>,
) -> Result<DialogOutcome<R>>
where
    Q: Serialize + Send + 'static,
    R: DialogReplyFrame,
    H: Hyprctl + Send + 'static,
{
    install_dialog_signal_cleanup();
    let DialogExchange {
        request,
        capability,
        max_frame_bytes,
        budget,
    } = exchange;
    let (mut parent, child) = UnixStream::pair().context("create private dialog socket")?;
    let child_read = child
        .try_clone()
        .context("duplicate private dialog descriptor")?;
    // Shut down to wake the blocked exchange when the question is cancelled.
    let wake = parent
        .try_clone()
        .context("duplicate private dialog descriptor")?;
    let mut process = command
        .stdin(Stdio::from(OwnedFd::from(child_read)))
        .stdout(Stdio::from(OwnedFd::from(child)))
        .stderr(Stdio::piped())
        .spawn()
        .context("launch the dialog")?;
    // `command` still holds the child's ends of the socket. Closing them
    // here means a child that exits without answering is an end-of-file
    // now, a failure, rather than a read that waits out the budget and
    // looks like no answer.
    drop(command);
    // Why a dialog that fails before answering failed (`child_stderr`).
    let stderr = StderrTail::capture(&mut process);
    let guard = DialogChild::with_lease(process, lease);
    let mut exchanging = tokio::task::spawn_blocking(move || {
        exchange_frames::<Q, R>(&mut parent, &request, max_frame_bytes, budget)
    });
    let exchanged = tokio::select! {
        // A cancelled question reads nothing more, even a reply that
        // arrived at the same moment.
        biased;
        () = cancel => {
            let _ = wake.shutdown(Shutdown::Both);
            // Killed and reaped off a runtime worker; nothing the child
            // printed is awaited or reported.
            let _ = tokio::task::spawn_blocking(move || {
                drop(stderr);
                guard.end()
            })
            .await;
            let _ = exchanging.await;
            return Ok(DialogOutcome::Cancelled);
        }
        joined = &mut exchanging => joined.context("dialog task failed")?,
    };
    let reply = match exchanged {
        Exchange::Reply(reply) => reply,
        Exchange::NoAnswer => {
            // Killed and reaped as for a failure, off a runtime worker, but
            // what the child printed is neither awaited nor reported.
            let _ = tokio::task::spawn_blocking(move || {
                drop(stderr);
                guard.end()
            })
            .await;
            return Ok(DialogOutcome::NoAnswer);
        }
        Exchange::Failed(error) => {
            // Reaping and waiting for the pipe block, so not on a runtime
            // worker. A failed task has dropped the guard, which still kills
            // and reaps the child.
            let (status, printed) =
                tokio::task::spawn_blocking(move || (guard.end(), stderr.finish(STDERR_TAIL_WAIT)))
                    .await
                    .unwrap_or((None, None));
            return Err(child_failure(
                "the dialog",
                &error,
                status,
                printed.as_deref(),
            ));
        }
    };
    if reply.capability() != capability.as_slice() {
        bail!("dialog authentication failed");
    }
    guard.finish();
    Ok(DialogOutcome::Reply(reply))
}

/// What came back on a dialog child's private socket.
#[derive(Debug)]
enum Exchange<R> {
    Reply(R),
    /// The read timed out: nobody answered within the budget.
    NoAnswer,
    Failed(anyhow::Error),
}

/// Sends `request` to the child and reads its reply, each bounded by
/// `budget` and `max_frame_bytes`.
fn exchange_frames<Q: Serialize, R: DeserializeOwned>(
    parent: &mut UnixStream,
    request: &Q,
    max_frame_bytes: usize,
    budget: Duration,
) -> Exchange<R> {
    if let Err(error) = send(parent, request, max_frame_bytes, budget) {
        return Exchange::Failed(error);
    }
    classify_reply(read_private_frame_bounded(parent, max_frame_bytes))
}

fn send<Q: Serialize>(
    parent: &mut UnixStream,
    request: &Q,
    max_frame_bytes: usize,
    budget: Duration,
) -> Result<()> {
    parent.set_write_timeout(Some(budget))?;
    parent.set_read_timeout(Some(budget))?;
    write_private_frame_bounded(parent, request, max_frame_bytes)
}

/// Tells a reply that did not come in time, the private socket's read
/// timeout expiring (`EAGAIN`, or `ETIMEDOUT`), from every other failed read.
fn classify_reply<R>(read: Result<R>) -> Exchange<R> {
    match read {
        Ok(reply) => Exchange::Reply(reply),
        Err(error) if timed_out(&error) => Exchange::NoAnswer,
        Err(error) => Exchange::Failed(error),
    }
}

fn timed_out(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<io::Error>().is_some_and(|error| {
            matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            )
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fmt::Write as _;

    use splinterm_protocol::{ConsentDecision, ConsentReply};

    /// A `hyprctl` that records each call and whether `pid` was still an
    /// unreaped `sleep` child when it came.
    #[derive(Clone)]
    struct RecordingHyprctl {
        pid: std::sync::Arc<std::sync::Mutex<u32>>,
        calls: std::sync::Arc<std::sync::Mutex<Vec<(String, bool)>>>,
    }

    impl RecordingHyprctl {
        fn new(pid: u32) -> Self {
            Self {
                pid: std::sync::Arc::new(std::sync::Mutex::new(pid)),
                calls: std::sync::Arc::default(),
            }
        }

        fn calls(&self) -> Vec<(String, bool)> {
            self.calls.lock().unwrap().clone()
        }

        fn pid(&self) -> u32 {
            *self.pid.lock().unwrap()
        }

        /// A lease for whichever child the dialog starts, recording its PID.
        fn lease(&self) -> impl FnOnce(u32) -> DialogShortcutLease<Self> + use<> {
            let recorder = self.clone();
            move |pid| {
                *recorder.pid.lock().unwrap() = pid;
                DialogShortcutLease::new(recorder, pid)
            }
        }
    }

    impl Hyprctl for RecordingHyprctl {
        fn run(&self, arguments: &[&str]) -> Option<String> {
            let child_present = std::fs::read_to_string(format!("/proc/{}/stat", self.pid()))
                .is_ok_and(|stat| stat.contains("(sleep)"));
            self.calls
                .lock()
                .unwrap()
                .push((arguments.join(" "), child_present));
            None
        }
    }

    /// A `sleep 30` child guarded as a dialog, with its recorder and PID.
    fn sleeping_child() -> (DialogChild<RecordingHyprctl>, RecordingHyprctl, u32) {
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        let recorder = RecordingHyprctl::new(pid);
        let shortcut = DialogShortcutLease::new(recorder.clone(), child.id());
        (
            DialogChild {
                child: Some(child),
                shortcut: Some(shortcut),
            },
            recorder,
            pid,
        )
    }

    fn unregistered_after_reaping(pid: u32) -> Vec<(String, bool)> {
        vec![(
            format!(
                "eval {}",
                splinterd::dialog_shortcut::unregister_script(Some(pid))
            ),
            false,
        )]
    }

    fn reaped(pid: u32) -> bool {
        !std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .is_ok_and(|stat| stat.contains("(sleep)"))
    }

    fn consent_reply() -> ConsentReply {
        ConsentReply {
            capability: vec![1; CAPABILITY_BYTES],
            decision: ConsentDecision::Deny,
        }
    }

    /// Only the private socket's read timeout is "no answer"; every other
    /// failed read stays a failure that says why.
    #[test]
    fn only_a_timed_out_read_is_no_answer() {
        for kind in [io::ErrorKind::WouldBlock, io::ErrorKind::TimedOut] {
            let read: Result<ConsentReply> = Err(anyhow::Error::from(io::Error::from(kind)));
            assert!(matches!(classify_reply(read), Exchange::NoAnswer), "{kind}");
            let wrapped: Result<ConsentReply> =
                Err(anyhow::Error::from(io::Error::from(kind)).context("read the reply"));
            assert!(
                matches!(classify_reply(wrapped), Exchange::NoAnswer),
                "{kind}"
            );
        }
        for failed in [
            anyhow::Error::from(io::Error::from(io::ErrorKind::UnexpectedEof)),
            anyhow::Error::from(io::Error::from(io::ErrorKind::ConnectionReset)),
            anyhow::anyhow!("invalid private dialog frame length"),
        ] {
            assert!(
                matches!(
                    classify_reply::<ConsentReply>(Err(failed)),
                    Exchange::Failed(_)
                ),
                "a failure"
            );
        }
        assert!(matches!(
            classify_reply(Ok(consent_reply())),
            Exchange::Reply(ConsentReply {
                decision: ConsentDecision::Deny,
                ..
            })
        ));
    }

    #[test]
    fn a_dropped_question_kills_and_reaps_its_dialog() {
        let (guard, recorder, pid) = sleeping_child();
        drop(guard);
        // The shortcut registration went once, after the child was reaped.
        assert_eq!(recorder.calls(), unregistered_after_reaping(pid));
        // Reaped: the pid no longer names our child.
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}/stat")).exists()
                || std::fs::read_to_string(format!("/proc/{pid}/stat"))
                    .is_ok_and(|stat| !stat.contains("(sleep)"))
        );
    }

    /// A termination signal removes the entries of the dialogs being asked,
    /// and only of those.
    #[test]
    fn a_dialog_is_listed_for_signal_cleanup_until_it_is_reaped() {
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        let guard = DialogChild::new(child);
        assert!(asking().contains(&pid));
        drop(guard);
        assert!(!asking().contains(&pid));
    }

    #[test]
    fn finishing_an_answered_dialog_never_blocks_the_caller() {
        // A child that lingers after answering is reaped in the background.
        let (guard, recorder, pid) = sleeping_child();
        let started = Instant::now();
        guard.finish();
        assert!(
            recorder.calls().is_empty(),
            "the registration stays until the child is reaped"
        );
        assert!(started.elapsed() < Duration::from_millis(500));
        // The reaper kills it after its grace period.
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !reaped(pid) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(reaped(pid));
        // The reaper removes the shortcut registration after reaping.
        while Instant::now() < deadline && recorder.calls().is_empty() {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(recorder.calls(), unregistered_after_reaping(pid));
    }

    /// A question cancelled while its dialog shows, as when a remote offer
    /// is withdrawn, kills and reaps the dialog, removes its shortcut entry
    /// only after the reaping, and reads nothing; it does not wait out the
    /// budget.
    #[tokio::test]
    async fn a_cancelled_question_kills_and_reaps_its_dialog_and_removes_its_entry() {
        let recorder = RecordingHyprctl::new(0);
        let mut lingering = Command::new("sh");
        lingering.args(["-c", "exec sleep 30"]);
        let (cancel, cancelled) = tokio::sync::oneshot::channel::<()>();
        let started = Instant::now();
        let asking_recorder = recorder.clone();
        let question = tokio::spawn(async move {
            ask_with_lease::<_, ConsentReply, _>(
                lingering,
                DialogExchange {
                    request: serde_json::json!({"question": 1}),
                    capability: vec![1; CAPABILITY_BYTES],
                    max_frame_bytes: 1024,
                    budget: Duration::from_secs(60),
                },
                async {
                    let _ = cancelled.await;
                },
                asking_recorder.lease(),
            )
            .await
        });
        // The child is up and asking before the withdrawal.
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline
            && (recorder.pid() == 0
                || !std::fs::read_to_string(format!("/proc/{}/stat", recorder.pid()))
                    .is_ok_and(|stat| stat.contains("(sleep)")))
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let pid = recorder.pid();
        assert!(asking().contains(&pid));
        assert!(recorder.calls().is_empty());
        cancel.send(()).unwrap();
        let outcome = question.await.unwrap().unwrap();
        assert_eq!(outcome, DialogOutcome::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(reaped(pid));
        assert!(!asking().contains(&pid));
        assert_eq!(recorder.calls(), unregistered_after_reaping(pid));
    }

    /// A reply that does not echo the capability is never an answer.
    #[tokio::test]
    async fn a_reply_without_the_capability_is_refused() {
        // A child that reads the request and answers with the wrong bytes.
        let reply = serde_json::to_vec(&consent_reply()).unwrap();
        let mut frame = u32::try_from(reply.len()).unwrap().to_be_bytes().to_vec();
        frame.extend(reply);
        let octal = frame.iter().fold(String::new(), |mut octal, byte| {
            write!(octal, "\\{byte:03o}").unwrap();
            octal
        });
        let script = format!("head -c 4 >/dev/null; printf '{octal}'; exec sleep 30");
        let mut answering = Command::new("sh");
        answering.args(["-c", &script]);
        let error = ask_dialog_process::<_, ConsentReply>(
            answering,
            serde_json::json!({"question": 1}),
            vec![2; CAPABILITY_BYTES],
            1024,
            Duration::from_secs(10),
            std::future::pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "dialog authentication failed");
    }
}
