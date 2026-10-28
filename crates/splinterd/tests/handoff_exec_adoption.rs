//! A complete two-generation daemon handoff through a real exec (Plan 0061
//! slice M3.10b), and its cooperative rollback through a second real exec
//! (M3.11a).
//!
//! The old generation runs in a stage process: it installs an unsalted copy
//! of the `splinterd` it was built with at a private prefix and retains that
//! pair as its rollback authority, then "upgrades" the prefix to a salted
//! copy of the same build (Plan 0061 D9) and a new client, and seals the
//! forward pair. It spawns live PTY Splints and one that exits at once,
//! captures and hands off the live ones, stops its runtime, writes the
//! durable Topology, builds the manifest pair, and crosses the real exec
//! boundary ([`cross_exec_boundary`] with
//! [`ExecInheritancePlan::with_forward_daemon`]): `execveat` of the sealed
//! forward daemon, in the same process. The forward daemon claims, stages,
//! adopts, commits, and serves as generation 2. The parent then checks the
//! adopted daemon over its real sockets and shuts it down.
//!
//! # Rollback
//!
//! The other cases make the forward candidate fail before its commit: the
//! forward client disappears from the prefix after the manifests are built
//! (adoption refuses the prefix), or a test-only fault point fires
//! (`CANDIDATE_FAULT_ENV`, one case per pre-commit step). The candidate's
//! rollback executor then re-executes the sealed rollback daemon snapshot
//! (the unsalted copy) with the unchanged descriptors and the rollback
//! manifest, in the same process again, and the parent checks that the
//! original generation 1 serves every carried Splint with its process and
//! incarnation. A failure on the rollback generation's own adoption has no
//! rollback edge and exits loudly with no further exec. Two cases leave a
//! SIGTERM pending across exec (raised inside the exec boundary, after its
//! own block), which must become an ordered shutdown once the adopting
//! generation installed its handler, never a killed candidate; so must one
//! sent while a failed candidate tears its runtime down. A rollback
//! image whose digest does not match its manifest is never executed, and a
//! stray inheritable descriptor in the failed candidate crosses the rollback
//! exec to be closed by the rollback daemon's claim instead of costing every
//! Splint. After every adoption and rollback, no daemon thread and no newly
//! launched Splint holds SIGINT, SIGTERM, or SIGHUP blocked.
//!
//! # A carried Window
//!
//! The adopt and rollback cases also carry one Window (Plan 0061 M3.13b2 and
//! M3.13c): a stand-in client process the stage spawns with the client end
//! of a credentialed `SOCK_SEQPACKET` continuation channel. The stage
//! records it in both manifests with its pidfd at `ClientProcess(0)`, the
//! daemon end at `Continuation(0)`, and the first carried Splint's
//! controller `HeldBy` its connection, and exec carries both descriptors.
//! The stand-in writes the first packet it reads to a file: the forward
//! daemon's `Adopted` outcome naming generation 2 after adoption, or the
//! rollback daemon's `RolledBack` naming generation 1 after a rollback,
//! which also proves the failed forward candidate sent nothing first.
//! One more rollback case records a duplicate of the listener at
//! `Continuation(0)`: the forward candidate refuses it at staging, and the
//! rollback generation drops that Window, rather than reading the duplicate
//! as an alias of the listener, and serves every carried Splint.
//!
//! # Proofs before live upgrade is enabled (Plan 0061 M3.17)
//!
//! * **A stale client.** The installed client before the upgrade is the same
//!   `python3` image as the forward client, and the stage keeps a hard link
//!   to its pre-upgrade inode. After the adoption, while the relaunched
//!   Window holds its granted ticket unredeemed, a process executing that
//!   stale inode is refused trusted UI at `Hello`; the Window then redeems
//!   the ticket. Same bytes, stale inode: device and inode decide, never
//!   content. (Presenting the ticket as an automation client is refused too,
//!   but that refusal is the authorization table's, for any automation
//!   client; the `Hello` refusal is the stale-inode check.)
//! * **No second handoff (D8).** The adopted daemon and the rollback daemon
//!   refuse another `Execute` and say a restart comes first; their dry run
//!   reports no rollback copy.
//! * **The rollback daemon refuses continuation claims.** It pins no client
//!   snapshot, so a carried Window's claim gets no ticket (`NoSnapshot`).
//!   No ticket ever exists there, so the case only sanity-checks that a
//!   made-up (all-zero) ticket is invalid.
//! * **The hold expires.** A relaunched Window claims its real ticket, then
//!   waits past the 30 s hold before redeeming it. The hold ends with the
//!   Window's placeholder still held, and the real ticket is then invalid.
//! * **The rollback guard timeout.** A thread that outlives the failed
//!   candidate's runtime keeps the rollback executor from one thread; the
//!   candidate exits loudly with no rollback exec.
//! * **The old side's signal carry.** A real `splinterd` process in the
//!   old-side drill (`OLD_SIDE_DRILL_ENV`, integration-test builds only)
//!   runs `Execute` from a trusted UI through the binary's own attempt,
//!   runtime stop, and exec boundary (`stop_runtime_and_cross`), which is
//!   never armed there. A SIGTERM it takes during the runtime stop is
//!   captured, carried across the boundary, and reaches the resumed round,
//!   which shuts down in order.
//! * **A package upgrade keeps running Windows trusted.** A real `splinterd`
//!   serves a trusted-UI connection from its prefix's client; a
//!   byte-identical client is then renamed over that client, as a package
//!   upgrade does, and the open connection's next trusted request still
//!   succeeds, while a new connection from the replaced inode is refused at
//!   `Hello` (AGENTS.md §4.4, the operator decision of 2026-10-20).
//!
//! # Terminal-body privacy (Plan 0037 M5)
//!
//! Each carried Splint fills its screen and scrollback with a sentinel the
//! shell assembles at run time, so every capture seals it into a checkpoint.
//! The stage checks that each checkpoint is an anonymous memfd; each serving
//! generation shows the sentinel in the carried screen and scrollback (the
//! body really crossed), holds no checkpoint descriptor or mapping after its
//! commit, and keeps the sentinel out of its observations and audit records.
//! After every Splint-carrying case, however the candidate ended (adopted,
//! rolled back, crashed, hung, stopped, or killed after its failure while
//! still holding every checkpoint), no name under the test's private
//! directory mentions a checkpoint and no file there holds the sentinel;
//! `/dev/shm` is only listed. A filter argument selects cases as libtest's
//! would ([`CaseSelection`]); one that matches no case fails the run.
//!
//! # Why the boundary is driven at the function level
//!
//! The real request path (a trusted-UI `PrepareDaemonHandoff { Execute }`
//! to a running `splinterd`) cannot reach exec in a test build, whatever the
//! client: a `splinterm` copy at the private prefix would satisfy
//! trusted-UI matching, but
//!
//! * plain `cargo test` builds are unpackaged (zero source commit), so the
//!   report names `NotPackaged` and `Execute` is refused before the gate,
//!   although live handoff itself is enabled (the binary's
//!   `live_handoff_support`, Plan 0061 M3.11b). A package's `check()` builds
//!   them with its provenance, where the D8 refusal names
//!   `RollbackUnavailable`. No seam overrides either refusal;
//! * the running daemon retains rollback authority, and finds the forward
//!   pair's package record, only beside `/usr/bin` or a `/nix/store` output
//!   (`package_provenance_path`), never at a private prefix, and a package
//!   record for an unpackaged build is refused outright;
//! * forward preparation preflights the client too, and no salted
//!   `integration-test` `splinterm` is built by `cargo test -p splinterd`.
//!
//! So this test builds the manifests from the stage's real captures through
//! `encode_manifest_pair_for_integration_test`, and everything from the
//! exec boundary on is the production code. Since M3.11b the boundary's
//! forward exec is compiled into every build. This target still needs the
//! `integration-test` feature for its salt and package-owner seams, and
//! `handoff_exec_production` proves the exec call site in a build without
//! the feature. The salted forward pair's
//! package provenance treatment is pinned by
//! `package_provenance::tests::a_salted_record_verifies_only_with_its_own_salt`.
//!
//! # Isolation
//!
//! Each stage runs with a private socket, state, configuration home, and
//! prefix, and no session bus, so no workload unit manager exists on either
//! side: every Splint runs uncontained and no scope can be stopped. The
//! stage starts without inherited descriptors (`close_fds`) and with only
//! its main thread, as the process guard requires. The parent kills every
//! process a stage left behind if a check fails, and a parent-death signal
//! ends the stage (by then the adopted daemon) if the parent itself is
//! killed.

#![cfg(feature = "integration-test")]

use std::{
    env, fs,
    io::Write as _,
    os::{
        fd::{AsFd, OwnedFd},
        unix::{
            fs::{OpenOptionsExt, PermissionsExt},
            net::UnixListener,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitCode, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rustix::{
    fs::{MemfdFlags, memfd_create},
    process::{Pid, Signal, geteuid, kill_process},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use splinterd::{
    HandoffCheckpointBudget, LiveError, LiveSplintConfig, LiveSplintHandle, LiveSplintRuntime,
    encode_manifest_pair_for_integration_test,
    executable_snapshot::{
        ExecutableSnapshotPolicy, ExecutableSourcePair, HandoffExecutableSnapshots,
        RetainedRollbackExecutables, SealedExecutableSnapshot,
    },
    handoff_compatibility::{BuildIdentity, BuildVersion},
    handoff_descriptors::{HandoffDescriptor, HandoffDescriptorSlot, inheritable_descriptors},
    handoff_exec::{
        ExecBoundaryStage, ExecInheritancePlan, ManifestSeal, candidate_daemon_arguments,
        cross_exec_boundary_hooked_for_integration_test, exec_environment,
    },
    handoff_preflight::compiled_handoff_generation,
    handoff_prepare::{CaptureLimits, HandedOffCapture, capture_live_splints},
    integration_test_seams::{
        BUILD_SALT_BYTES, CANDIDATE_FAULT_ENV, CandidateFaultPoint, OLD_SIDE_DRILL_ENV,
        PACKAGED_OWNER_UID_ENV, salt_slot_offset, salted_pair_build_identity,
    },
};
use splinterm_core::{
    Dojo, DojoId, Lair, LairId, LairLifetime, LairRetention, LayoutNode, Splint, SplintId,
    SplintLaunchMetadata, SplintState, Topology, TopologyDocument,
};
use splinterm_protocol::{
    ClientFrame, ClientRole, ControlMode, DaemonHandoffMode, ErrorCode, HandoffBlockingReason,
    HandoffBuildProvenance, LaunchParameters, MAX_FRAME_BYTES, MAX_IMAGE_BYTES_PER_DAEMON,
    PROTOCOL_VERSION, ProtocolError, Request, Response, ServerFrame, TerminalSnapshot,
    encode_frame,
    handoff_outcome::{HandoffOutcome, HandoffOutcomeKind},
    image_content_socket_path,
};
use splinterm_pty::{LinuxPtyBackend, PtyCommand};
use splinterm_terminal::{
    DEFAULT_KITTY_UPLOAD_BYTES_PER_DAEMON, SharedImageBudget, SharedKittyUploadBudget,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    time,
};

const STAGE: &str = "SPLINTERD_EXEC_ADOPTION_STAGE";
const DIRECTORY: &str = "SPLINTERD_EXEC_ADOPTION_DIRECTORY";
/// The stage removes the forward client after building the manifests.
const REMOVE_FORWARD_CLIENT: &str = "SPLINTERD_EXEC_ADOPTION_REMOVE_FORWARD_CLIENT";
/// The stage leaves a SIGTERM pending (blocked) across its exec.
const PENDING_SIGTERM: &str = "SPLINTERD_EXEC_ADOPTION_PENDING_SIGTERM";
/// The stage records a wrong SHA-256 for the rollback daemon snapshot in
/// both manifests, so staging and the rollback executor both refuse it.
const TAMPER_ROLLBACK_IMAGE: &str = "SPLINTERD_EXEC_ADOPTION_TAMPER_ROLLBACK_IMAGE";
/// The stage carries one stand-in Window across exec.
const WINDOW: &str = "SPLINTERD_EXEC_ADOPTION_WINDOW";
/// The stage records a duplicate of its listener, not the Window's
/// continuation end, at `Continuation(0)`.
const ALIAS_WINDOW_CONTINUATION: &str = "SPLINTERD_EXEC_ADOPTION_ALIAS_WINDOW_CONTINUATION";
/// The relaunched Window waits for the stale-client check between its
/// ticket grant and its redemption.
const WINDOW_STALE_CHECK: &str = "SPLINTERD_EXEC_ADOPTION_WINDOW_STALE_CHECK";
/// The relaunched Window waits this many seconds between its ticket grant
/// and its redemption.
const WINDOW_REDEEM_DELAY: &str = "SPLINTERD_EXEC_ADOPTION_WINDOW_REDEEM_DELAY";
/// The stand-in Window also claims on its continuation after a rollback.
const WINDOW_CLAIM_AFTER_ROLLBACK: &str = "SPLINTERD_EXEC_ADOPTION_WINDOW_CLAIM_AFTER_ROLLBACK";
/// A hard link the stage makes to the pre-upgrade installed client before
/// the upgrade replaces it: the stale client inode.
const STALE_CLIENT: &str = "stale-splinterm";
/// Where the relaunched Window writes its granted ticket, hex encoded, when
/// it waits for the stale-client check.
const WINDOW_TICKET: &str = "window-ticket.hex";
/// Created by the parent once the stale-client check is done.
const STALE_CHECKED: &str = "stale-client-checked";
/// What the stand-in Window recorded of its claim after a rollback.
const WINDOW_ROLLBACK_CLAIM: &str = "window-rollback-claim.json";
/// How long the adopted daemon holds a carried Window
/// (`handoff_window_carry::HANDOFF_WINDOW_RESUME_HOLD`).
const RESUME_HOLD_SECONDS: u64 = 30;
/// Where the stand-in Window writes the first continuation packet it reads.
const WINDOW_OUTCOME: &str = "window-outcome.bin";
/// What the relaunched stand-in Window sends to claim its controller: the
/// continuation claim packet, its trusted-UI hello, and its
/// `ClaimHandoffControl` frame (Plan 0061 M3.16).
const WINDOW_CLAIM_FRAMES: &str = "window-claim-frames.json";
/// Where the relaunched stand-in Window writes what the adopted daemon
/// answered its hello and its claim.
const WINDOW_CLAIM: &str = "window-claim.json";
/// The stand-in Window's one old connection, which held a controller.
const WINDOW_CONNECTION: u64 = 41;
/// The stand-in Window: reads one packet from the inherited continuation
/// end named by its first argument (end of file reads as empty, and it
/// gives up after two minutes) and writes it atomically to its second.
///
/// On an `Adopted` outcome it then relaunches as a real Window does: it
/// executes the sealed forward client snapshot it was handed (its third
/// argument, a copy of the `python3` image, so the new image can run
/// [`WINDOW_CLAIM_SCRIPT`]), in the same process, keeping only the
/// continuation inheritable.
///
/// On a `RolledBack` outcome, when its claim frames name a `rollback_claim`,
/// it claims on the continuation anyway and records how many bytes came
/// back (Plan 0061 M3.17: the rollback daemon pins no snapshot and issues no
/// ticket).
const FAKE_WINDOW: &str = r#"
import json, os, select, socket, sys
channel = socket.socket(fileno=int(sys.argv[1]))
ready, _, _ = select.select([channel], [], [], 120)
data = channel.recv(64) if ready else b"timeout"
partial = sys.argv[2] + ".partial"
with open(partial, "wb") as out:
    out.write(data)
os.rename(partial, sys.argv[2])
if len(data) == 16 and data[5] == 1:
    os.set_inheritable(channel.fileno(), True)
    home = sys.prefix if sys.prefix == sys.exec_prefix else sys.prefix + ":" + sys.exec_prefix
    os.execve(
        int(sys.argv[3]),
        ["python3", "-c", sys.argv[5], str(channel.fileno()), sys.argv[4]],
        dict(os.environ, PYTHONHOME=home),
    )
elif len(data) == 16 and data[5] == 2:
    with open(sys.argv[4]) as source:
        frames = json.load(source)
    if "rollback_claim" in frames:
        result = {}
        try:
            channel.send(bytes.fromhex(frames["rollback_claim"]))
            ready, _, _ = select.select([channel], [], [], 30)
            result["grant_bytes"] = len(channel.recv(128)) if ready else -1
        except OSError as error:
            result["error"] = type(error).__name__
        partial = frames["rollback_result"] + ".partial"
        with open(partial, "w") as out:
            json.dump(result, out)
        os.rename(partial, frames["rollback_result"])
"#;
/// The relaunched stand-in Window, executing the pinned snapshot: claims its
/// resume ticket on the continuation, connects to the adopted daemon as
/// trusted UI, presents the ticket for the Splint its old connection
/// controlled, and writes both answers atomically.
///
/// Its frames may also ask it to publish its granted ticket and wait for the
/// stale-client check before redeeming (`ticket_file`, `proceed_file`), or to
/// wait `redeem_delay_seconds` between the grant and the redemption (the
/// expiry case).
const WINDOW_CLAIM_SCRIPT: &str = r#"
import json, os, socket, struct, sys, time
channel = socket.socket(fileno=int(sys.argv[1]))
with open(sys.argv[2]) as source:
    frames = json.load(source)
def finish(value):
    partial = frames["result"] + ".partial"
    with open(partial, "w") as out:
        json.dump(value, out)
    os.rename(partial, frames["result"])
try:
    channel.send(bytes.fromhex(frames["claim"]))
    channel.settimeout(30)
    grant = channel.recv(128)
except OSError:
    grant = b""
if len(grant) != 48:
    finish({"grant_bytes": len(grant)})
    raise SystemExit(0)
if len(grant) == 48 and "ticket_file" in frames:
    partial = frames["ticket_file"] + ".partial"
    with open(partial, "w") as out:
        out.write(grant[16:].hex())
    os.rename(partial, frames["ticket_file"])
    deadline = time.monotonic() + 20
    while not os.path.exists(frames["proceed_file"]):
        if time.monotonic() > deadline:
            raise SystemExit("the stale-client check never finished")
        time.sleep(0.02)
time.sleep(frames.get("redeem_delay_seconds", 0))
daemon = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
daemon.settimeout(30)
daemon.connect(os.environ["SPLINTERM_SOCKET"])
def send(frame):
    body = json.dumps(frame).encode()
    daemon.sendall(struct.pack(">I", len(body)) + body)
def exactly(count):
    data = b""
    while len(data) < count:
        chunk = daemon.recv(count - len(data))
        if not chunk:
            raise SystemExit("splinterd closed the connection")
        data += chunk
    return data
def read():
    (length,) = struct.unpack(">I", exactly(4))
    return json.loads(exactly(length))
send(frames["hello"])
hello = read()
request = frames["request"]
request["request"]["ticket"] = list(grant[16:])
send(request)
while True:
    frame = read()
    if frame.get("request_id") == request["request_id"]:
        break
finish({"grant_bytes": len(grant), "hello": hello, "claim": frame})
"#;
/// One protocol session from a client process whose executable the daemon
/// judges (a copy of `python3` at some path): says the spec's hello, then
/// sends each request in order and records its answer. Prints a JSON list:
/// the hello answer, then one answer per request; `null` where the daemon
/// closed the connection first. A spec naming `pause_before` first prints
/// the answers so far on their own line before that request, and waits for
/// a line on its standard input. A spec naming `second_requests` then opens
/// a second connection from the same process, while the first stays open,
/// and appends its hello answer and one answer per request.
const CLIENT_SESSION: &str = r#"
import json, socket, struct, sys
with open(sys.argv[1]) as source:
    spec = json.load(source)
def connect():
    channel = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    channel.settimeout(30)
    channel.connect(spec["socket"])
    return channel
def send(channel, frame):
    body = json.dumps(frame).encode()
    channel.sendall(struct.pack(">I", len(body)) + body)
def exactly(channel, count):
    data = b""
    while len(data) < count:
        chunk = channel.recv(count - len(data))
        if not chunk:
            return None
        data += chunk
    return data
def read(channel):
    header = exactly(channel, 4)
    if header is None:
        return None
    (length,) = struct.unpack(">I", header)
    return json.loads(exactly(channel, length))
def converse(channel, requests, answers, pause_before=None):
    send(channel, spec["hello"])
    answers.append(read(channel))
    if answers[-1] is None or answers[-1].get("type") != "hello":
        return
    for index, request in enumerate(requests):
        if index == pause_before:
            print(json.dumps(answers), flush=True)
            sys.stdin.readline()
        send(channel, request)
        while True:
            frame = read(channel)
            if frame is None or frame.get("request_id") == request["request_id"]:
                break
        answers.append(frame)
answers = []
daemon = connect()
converse(daemon, spec["requests"], answers, spec.get("pause_before"))
if spec.get("second_requests") is not None:
    converse(connect(), spec["second_requests"], answers)
print(json.dumps(answers))
"#;
/// Every pre-commit step the fault matrix fails the forward candidate at.
const FAULT_POINTS: [CandidateFaultPoint; 6] = [
    CandidateFaultPoint::Staging,
    CandidateFaultPoint::RuntimeEntry,
    CandidateFaultPoint::Startup,
    CandidateFaultPoint::ListenerRegistration,
    CandidateFaultPoint::ScopeAdoption,
    CandidateFaultPoint::ActorStaging,
];
/// Every case, in run order; [`CaseSelection`] picks the ones a run
/// executes.
const CASES: [(&str, fn()); 15] = [
    ("adopt", adopt_case),
    ("rollback", rollback_case),
    ("rollback-dropped-window", rollback_dropped_window_case),
    ("rollback-refuses-claims", rollback_refuses_claims_case),
    ("rollback-of-rollback", rollback_of_rollback_case),
    ("hold-expiry", hold_expiry_case),
    ("pending-sigterm", pending_sigterm_cases),
    ("tampered-rollback-image", tampered_rollback_image_case),
    ("stray-descriptor", stray_descriptor_case),
    ("terminate-during-teardown", terminate_during_teardown_case),
    ("rollback-guard-timeout", rollback_guard_timeout_case),
    ("killed-hung-candidate", killed_hung_candidate_case),
    ("old-side-signal-carry", old_side_signal_carry_case),
    (
        "replaced-client-keeps-its-connection",
        replaced_client_keeps_its_connection_case,
    ),
    ("fault-matrix", fault_matrix_case),
];
const DAEMON: &str = env!("CARGO_BIN_EXE_splinterd");
/// The salt the forward image carries: any nonzero value makes its pair
/// identity differ from the rollback image's.
const SALT: [u8; BUILD_SALT_BYTES] = *b"m3.10b-exec-salt";

/// The client installed before the upgrade and the forward client the
/// upgrade installs: both copies of the `python3` ELF image, each its own
/// inode. The stand-in Window can execute the sealed snapshot of the forward
/// one and still run a script (an interpreter script would execute as its
/// interpreter, not as the snapshot), and a process executing the
/// pre-upgrade inode is the stale client, with the very same bytes.
fn forward_client() -> Vec<u8> {
    let python = env::var_os("PATH")
        .and_then(|path| {
            env::split_paths(&path)
                .map(|directory| directory.join("python3"))
                .find(|candidate| candidate.is_file())
        })
        .expect("python3 is on PATH");
    fs::read(python).unwrap()
}
/// Each carried Splint first fills its screen and scrollback with the
/// terminal-body sentinel (Plan 0037 M5), so every capture seals it into a
/// checkpoint. The shell joins [`SENTINEL_PIECES`] at run time: neither
/// this source, an argv, nor any record holds the joined form, so finding
/// it in a file proves that a terminal body leaked there.
const CAT: &str = "s=$(printf '%s-%s-%s' m5-checkpoint 4b1d93e7 body); i=0; \
                   while [ \"$i\" -lt 40 ]; do printf '%s %02d\\n' \"$s\" \"$i\"; \
                   i=$((i+1)); done; printf ready; exec cat";
const SENTINEL_PIECES: [&str; 3] = ["m5-checkpoint", "4b1d93e7", "body"];
/// The name every sealed terminal checkpoint memfd carries.
const CHECKPOINT_MEMFD: &str = "splinterd-terminal-checkpoint";
const EXCLUDED_EXIT: i32 = 3;
const GUARD_WAIT: Duration = Duration::from_secs(2);
const WAIT: Duration = Duration::from_secs(10);
const STAGE_WAIT: Duration = Duration::from_secs(60);
const RECORD: &str = "stage-record.json";
const STAGED_TOPOLOGY: &str = "topology.staged.json";
const STDOUT: &str = "stage.stdout";
const STDERR: &str = "stage.stderr";
/// Names this test process's PID for the launcher's parent-death check.
const PARENT: &str = "SPLINTERD_EXEC_ADOPTION_PARENT";
/// Runs the stage with `close_fds`, so it inherits no descriptor, and ties
/// both lifetimes to their parents with `PR_SET_PDEATHSIG` = `SIGKILL`.
/// Drop guards cannot run when this test binary is itself killed or timed
/// out, and an adopted daemon never exits on its own: without this it and
/// its Splints would be orphaned.
///
/// The launcher dies with this test process (the thread that spawned it is
/// `main`, which outlives every stage), and the stage with the launcher
/// (whose only thread forks it). The signal is set after fork in the stage
/// (`preexec_fn`) and survives `execve` of the test binary and the stage's
/// own `execveat` into the forward daemon, neither of which is set-user-ID
/// or file-capability bearing; `fork` clears it, so Splint shells do not
/// inherit it (they hang up when the daemon's PTY masters close). Each
/// side re-checks its parent after `prctl`, closing the race with a parent
/// that died first.
const LAUNCHER: &str = r#"
import ctypes, os, signal, subprocess, sys
libc = ctypes.CDLL(None, use_errno=True)
PR_SET_PDEATHSIG = 1
def die_with(parent):
    if libc.prctl(PR_SET_PDEATHSIG, ctypes.c_ulong(signal.SIGKILL), ctypes.c_ulong(0), ctypes.c_ulong(0), ctypes.c_ulong(0)) != 0:
        os._exit(126)
    if os.getppid() != parent:
        os._exit(126)
die_with(int(os.environ["SPLINTERD_EXEC_ADOPTION_PARENT"]))
launcher = os.getpid()
raise SystemExit(subprocess.run(sys.argv[1:], close_fds=True, preexec_fn=lambda: die_with(launcher)).returncode)
"#;

fn main() -> ExitCode {
    if env::args().skip(1).any(|argument| argument == "--list") {
        for (case, _) in CASES {
            println!("handoff_exec_adoption::{case}: test");
        }
        return ExitCode::SUCCESS;
    }
    match env::var(STAGE).ok().as_deref() {
        Some("old-generation") => old_generation_stage(&StageOptions {
            remove_forward_client: env::var_os(REMOVE_FORWARD_CLIENT).is_some(),
            pending_sigterm: env::var_os(PENDING_SIGTERM).is_some(),
            tamper_rollback_image: env::var_os(TAMPER_ROLLBACK_IMAGE).is_some(),
            window: env::var_os(WINDOW).is_some(),
            alias_window_continuation: env::var_os(ALIAS_WINDOW_CONTINUATION).is_some(),
            window_stale_check: env::var_os(WINDOW_STALE_CHECK).is_some(),
            window_redeem_delay: env::var(WINDOW_REDEEM_DELAY)
                .ok()
                .map(|seconds| seconds.parse().expect("a whole number of seconds")),
            window_claim_after_rollback: env::var_os(WINDOW_CLAIM_AFTER_ROLLBACK).is_some(),
            fault: None,
        }),
        Some(stage) => panic!("unknown stage {stage:?}"),
        None => {
            // A harness may leak an inheritable descriptor into this process
            // (CI run 39137743159 did); each stage starts without one anyway,
            // and this deliberately inheritable one proves it on every run.
            let stray = fs::File::open("/dev/null").unwrap();
            let flags = rustix::io::fcntl_getfd(&stray).unwrap();
            rustix::io::fcntl_setfd(&stray, flags - rustix::io::FdFlags::CLOEXEC).unwrap();
            assert_case_selection_parsing();
            let selection = CaseSelection::parse(env::args().skip(1));
            let selected = CASES
                .iter()
                .filter(|(case, _)| selection.matches(case))
                .collect::<Vec<_>>();
            if selected.is_empty() {
                eprintln!(
                    "handoff_exec_adoption: no case matches the filter {:?}",
                    selection.filter.as_deref().unwrap_or_default()
                );
                return ExitCode::FAILURE;
            }
            for (case, run) in &selected {
                run();
                println!("handoff_exec_adoption: {case} passed");
            }
            drop(stray);
            println!(
                "handoff_exec_adoption: {} of {} cases run",
                selected.len(),
                CASES.len()
            );
            println!("handoff_exec_adoption: ok");
        }
    }
    ExitCode::SUCCESS
}

/// The cases this run selects, read from the arguments the way libtest
/// reads them: the first free argument is the filter (with this target's
/// `handoff_exec_adoption::` prefix from `--list` stripped), the value of
/// an option that takes one is never the filter, and `--exact` matches
/// whole names instead of substrings.
#[derive(Debug, Default, PartialEq, Eq)]
struct CaseSelection {
    filter: Option<String>,
    exact: bool,
}

impl CaseSelection {
    /// libtest options whose value is the next argument.
    const VALUE_OPTIONS: [&str; 7] = [
        "--test-threads",
        "--skip",
        "--logfile",
        "--color",
        "--format",
        "--shuffle-seed",
        "-Z",
    ];

    fn parse(arguments: impl IntoIterator<Item = String>) -> Self {
        let mut selection = Self::default();
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            if Self::VALUE_OPTIONS.contains(&argument.as_str()) {
                arguments.next();
            } else if argument == "--exact" {
                selection.exact = true;
            } else if !argument.starts_with('-') && selection.filter.is_none() {
                let name = argument
                    .strip_prefix("handoff_exec_adoption::")
                    .unwrap_or(&argument);
                selection.filter = Some(name.to_owned());
            }
        }
        selection
    }

    fn matches(&self, case: &str) -> bool {
        self.filter.as_deref().is_none_or(|filter| {
            if self.exact {
                case == filter
            } else {
                case.contains(filter)
            }
        })
    }
}

/// [`CaseSelection`]'s parsing, checked on every run of this harness
/// (which has no libtest `#[test]` runner of its own).
fn assert_case_selection_parsing() {
    let parse = |arguments: &[&str]| {
        CaseSelection::parse(arguments.iter().map(|argument| (*argument).to_owned()))
    };
    let filter = |name: &str, exact| CaseSelection {
        filter: Some(name.to_owned()),
        exact,
    };
    assert_eq!(parse(&[]), CaseSelection::default());
    assert_eq!(parse(&["--test-threads", "1"]), CaseSelection::default());
    assert_eq!(parse(&["--test-threads=1"]), CaseSelection::default());
    assert_eq!(
        parse(&[
            "--color",
            "never",
            "-Z",
            "unstable-options",
            "--format",
            "terse"
        ]),
        CaseSelection::default()
    );
    assert_eq!(
        parse(&["--skip", "adopt", "rollback"]),
        filter("rollback", false)
    );
    assert_eq!(
        parse(&["--test-threads", "1", "killed-hung-candidate"]),
        filter("killed-hung-candidate", false)
    );
    let exact = parse(&["--exact", "handoff_exec_adoption::rollback"]);
    assert_eq!(exact, filter("rollback", true));
    assert!(exact.matches("rollback"));
    assert!(!exact.matches("rollback-of-rollback"));
    assert!(parse(&["rollback"]).matches("rollback-of-rollback"));
    assert!(!parse(&["nosuchcase"]).matches("adopt"));
}

/// How one stage's old generation behaves, and what fails its forward
/// candidate.
#[derive(Clone, Debug, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent switches, each one environment variable the stage reads"
)]
struct StageOptions {
    /// Remove the forward client after building the manifests, so forward
    /// adoption refuses the prefix (rollback adoption only reports it).
    remove_forward_client: bool,
    /// Raise SIGTERM from inside the exec boundary, once it has blocked the
    /// handoff signals, so it is pending across exec.
    pending_sigterm: bool,
    /// Record a wrong digest for the rollback daemon snapshot.
    tamper_rollback_image: bool,
    /// Carry one stand-in Window across exec.
    window: bool,
    /// With `window`, record a duplicate of the listener at
    /// `Continuation(0)` instead of the Window's continuation end.
    alias_window_continuation: bool,
    /// With `window`, the relaunched Window publishes its granted ticket and
    /// waits for the parent's stale-client check before redeeming it.
    window_stale_check: bool,
    /// With `window`, the relaunched Window waits this many seconds between
    /// its ticket grant and its redemption.
    window_redeem_delay: Option<u64>,
    /// With `window`, the stand-in Window claims on its continuation after a
    /// rollback too.
    window_claim_after_rollback: bool,
    /// Candidate fault points, as `CANDIDATE_FAULT_ENV` spells them; the
    /// parent sets the variable, and it crosses both execs.
    fault: Option<String>,
}

// ---------------------------------------------------------------------------
// The old generation (stage process)
// ---------------------------------------------------------------------------

/// What the stage tells the parent, written just before the exec boundary.
#[derive(Debug, Deserialize, Serialize)]
struct StageRecord {
    pid: u32,
    source_daemon_generation: u64,
    topology_revision: u64,
    lair_id: LairId,
    carried: Vec<RecordedSplint>,
    excluded: RecordedSplint,
    /// The stand-in Window's process, when the stage carries one.
    window_pid: Option<u32>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
struct RecordedSplint {
    splint_id: SplintId,
    dojo_id: DojoId,
    incarnation: u64,
    child_pid: u32,
}

/// The old generation's Splints after its runtime stopped.
struct Stopped {
    handed_off: Vec<HandedOffCapture>,
    carried: Vec<RecordedSplint>,
    excluded: RecordedSplint,
}

fn assert_single_threaded() {
    assert_eq!(
        fs::read_dir("/proc/self/task").unwrap().count(),
        1,
        "stage must start with only its main thread"
    );
    assert_eq!(
        inheritable_descriptors(16).unwrap(),
        [],
        "stage must start without inherited descriptors"
    );
}

/// Runs the old generation and crosses the exec boundary; returns only by
/// panicking. See [`StageOptions`].
#[allow(
    clippy::too_many_lines,
    reason = "one old generation, from install through the exec call, in production order"
)]
fn old_generation_stage(options: &StageOptions) -> ! {
    assert_single_threaded();
    let directory = PathBuf::from(env::var_os(DIRECTORY).expect("stage directory"));
    let owner = geteuid().as_raw();
    assert_eq!(
        env::var(PACKAGED_OWNER_UID_ENV).ok(),
        Some(owner.to_string()),
        "manifest validation on both sides accepts this uid as the package owner"
    );
    let policy = ExecutableSnapshotPolicy {
        expected_owner_uid: owner,
    };
    let prefix = directory.join("prefix");
    fs::create_dir(&prefix).unwrap();
    let daemon_path = prefix.join("splinterd");
    let client_path = prefix.join("splinterm");
    let pair = ExecutableSourcePair::new(&daemon_path, &client_path).unwrap();

    // The running generation's installed pair, retained before the upgrade.
    let daemon_bytes = fs::read(DAEMON).unwrap();
    install(&daemon_path, &daemon_bytes);
    install(&client_path, &forward_client());
    // The pre-upgrade client inode, kept past the upgrade's rename.
    fs::hard_link(&client_path, directory.join(STALE_CLIENT)).unwrap();
    // Launches in the prefix use its PTY helper, as a packaged daemon's do.
    install(
        &prefix.join("splinterm-pty-child"),
        &fs::read(pty_helper()).unwrap(),
    );
    let rollback = RetainedRollbackExecutables::capture_declared_for_test(&pair, policy).unwrap();

    // The package upgrade: a salted image of the same build, whose pair
    // identity therefore differs, and a new client.
    let mut forward_daemon = daemon_bytes;
    let slot = salt_slot_offset(&forward_daemon)
        .expect("the integration-test daemon carries exactly one patchable salt slot");
    forward_daemon[slot..slot + BUILD_SALT_BYTES].copy_from_slice(&SALT);
    install(&daemon_path, &forward_daemon);
    install(&client_path, &forward_client());
    let snapshots = HandoffExecutableSnapshots::materialize(&pair, &rollback, policy).unwrap();
    drop(rollback);

    let socket = PathBuf::from(env::var_os("SPLINTERM_SOCKET").expect("stage socket"));
    let control = bound_listener(&socket);
    let image = bound_listener(&image_content_socket_path(&socket));

    let stopped = hand_off_and_stop();
    // Plan 0037 M5: each captured terminal body exists only as an anonymous
    // sealed memfd, never as a named file.
    for capture in &stopped.handed_off {
        let descriptor = capture.checkpoint.handoff_descriptor();
        let target = fs::read_link(format!("/proc/self/fd/{}", descriptor.raw_fd())).unwrap();
        assert!(
            target
                .to_string_lossy()
                .starts_with(&format!("/memfd:{CHECKPOINT_MEMFD} ")),
            "a checkpoint is not an anonymous memfd: {}",
            target.display()
        );
    }
    let window = options
        .window
        .then(|| spawn_window(&directory, snapshots.forward().client().descriptor()));
    if window.is_some() {
        write_window_claim_frames(&directory, &stopped, options);
    }
    // The listener's object at a distinct close-on-exec number.
    let listener_duplicate = options
        .alias_window_continuation
        .then(|| control.try_clone().unwrap());
    // The excluded child is still an unreaped zombie of this process: only
    // the forward daemon can reap it and record its real exit.
    assert_eq!(
        proc_stat(stopped.excluded.child_pid).map(|(state, _)| state),
        Some('Z'),
        "the excluded child must cross exec unreaped"
    );

    let lair_id = LairId::new();
    let topology = durable_topology(lair_id, &stopped);
    let revision = topology.revision().get();
    let encoded = TopologyDocument::from_topology(&topology)
        .unwrap()
        .encode()
        .unwrap();
    write_state(&directory, &encoded);
    fs::write(directory.join(STAGED_TOPOLOGY), &encoded).unwrap();

    let storage = [
        manifest_storage("exec-adoption-forward-manifest"),
        manifest_storage("exec-adoption-rollback-manifest"),
    ];
    let mut owners = vec![
        HandoffDescriptor::new(HandoffDescriptorSlot::Listener, control.as_fd()),
        HandoffDescriptor::new(HandoffDescriptorSlot::ImageListener, image.as_fd()),
        HandoffDescriptor::new(
            HandoffDescriptorSlot::CheckpointManifest,
            storage[0].as_fd(),
        ),
        HandoffDescriptor::new(HandoffDescriptorSlot::RollbackManifest, storage[1].as_fd()),
        HandoffDescriptor::new(
            HandoffDescriptorSlot::ForwardClient,
            snapshots.forward().client().descriptor(),
        ),
        HandoffDescriptor::new(
            HandoffDescriptorSlot::RollbackDaemon,
            snapshots.rollback().daemon().descriptor(),
        ),
        HandoffDescriptor::new(
            HandoffDescriptorSlot::RollbackClient,
            snapshots.rollback().client().descriptor(),
        ),
    ];
    for capture in &stopped.handed_off {
        owners.push(HandoffDescriptor::new(
            HandoffDescriptorSlot::PtyMaster(capture.slot()),
            capture.handed_off.session.master(),
        ));
        owners.push(capture.checkpoint.handoff_descriptor());
    }
    if let Some(window) = &window {
        owners.push(HandoffDescriptor::new(
            HandoffDescriptorSlot::ClientProcess(0),
            window.pidfd.as_fd(),
        ));
        owners.push(HandoffDescriptor::new(
            HandoffDescriptorSlot::Continuation(0),
            listener_duplicate
                .as_ref()
                .map_or_else(|| window.continuation.as_fd(), AsFd::as_fd),
        ));
    }
    let (forward_bytes, rollback_bytes) = manifest_pair(
        &prefix,
        &snapshots,
        &owners,
        &stopped,
        lair_id,
        revision,
        options.tamper_rollback_image,
        window.as_ref(),
    );
    let recorded = owners
        .iter()
        .map(|owner| (owner.slot(), owner.raw_fd()))
        .collect::<Vec<_>>();
    let plan = ExecInheritancePlan::new(
        &recorded,
        &owners,
        [
            ManifestSeal::new(storage[0].as_fd(), &forward_bytes),
            ManifestSeal::new(storage[1].as_fd(), &rollback_bytes),
        ],
    )
    .unwrap();
    let forward_manifest = plan.forward_manifest_descriptor().unwrap();
    // Exactly what the binary's `arm_forward_exec` passes: argv rebuilt from
    // state (no workload cgroups required here), the environment unchanged.
    let arguments =
        candidate_daemon_arguments(daemon_path.as_os_str(), forward_manifest, false).unwrap();
    let plan = plan.with_forward_daemon(
        snapshots.forward().daemon().descriptor(),
        arguments,
        exec_environment(),
    );

    if options.remove_forward_client {
        fs::remove_file(&client_path).unwrap();
    }
    let record = StageRecord {
        pid: std::process::id(),
        source_daemon_generation: 1,
        topology_revision: revision,
        lair_id,
        carried: stopped.carried.clone(),
        excluded: stopped.excluded,
        window_pid: window.as_ref().map(|window| window.pid),
    };
    write_atomically(
        &directory.join(RECORD),
        &serde_json::to_vec(&record).unwrap(),
    );

    // As if `systemctl stop` arrived in the exec window: raised from inside
    // the boundary, after it blocked the handoff signals and before exec,
    // with nothing blocked here first. Without the boundary's own block the
    // signal's default action would end this stage at once.
    let mut raised = false;
    let reason = cross_exec_boundary_hooked_for_integration_test(
        &plan,
        Instant::now() + GUARD_WAIT,
        &mut |stage| {
            if options.pending_sigterm && stage == ExecBoundaryStage::Guard && !raised {
                raised = true;
                nix::sys::signal::raise(nix::sys::signal::Signal::SIGTERM).unwrap();
            }
            Ok(())
        },
    );
    panic!("the exec boundary returned instead of executing the forward daemon: {reason}");
}

/// A stand-in Window as the old generation carries it: its process, pinned
/// by a pidfd, and the daemon end of its continuation channel.
struct StandInWindow {
    pid: u32,
    start_time: u64,
    executable: (u64, u64),
    pidfd: OwnedFd,
    continuation: OwnedFd,
}

/// Spawns the stand-in Window with the client end of a fresh credentialed
/// `SOCK_SEQPACKET` pair and a duplicate of the sealed forward client
/// `snapshot`, as a real Window receives both with its prepare frame. Only
/// those two are ever inheritable, and only until the child has them: the
/// stage closes its copies at once, so nothing inheritable reaches the exec
/// boundary's audit.
fn spawn_window(directory: &Path, snapshot: std::os::fd::BorrowedFd<'_>) -> StandInWindow {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::MetadataExt as _;

    let (continuation, client) = rustix::net::socketpair(
        rustix::net::AddressFamily::UNIX,
        rustix::net::SocketType::SEQPACKET,
        rustix::net::SocketFlags::CLOEXEC,
        None,
    )
    .unwrap();
    rustix::net::sockopt::set_socket_passcred(&continuation, true).unwrap();
    rustix::io::fcntl_setfd(&client, rustix::io::FdFlags::empty()).unwrap();
    let snapshot = rustix::io::fcntl_dupfd_cloexec(snapshot, 3).unwrap();
    rustix::io::fcntl_setfd(&snapshot, rustix::io::FdFlags::empty()).unwrap();
    let child = Command::new("python3")
        .arg("-c")
        .arg(FAKE_WINDOW)
        .arg(client.as_raw_fd().to_string())
        .arg(directory.join(WINDOW_OUTCOME))
        .arg(snapshot.as_raw_fd().to_string())
        .arg(directory.join(WINDOW_CLAIM_FRAMES))
        .arg(WINDOW_CLAIM_SCRIPT)
        .stdin(Stdio::null())
        .spawn()
        .expect("spawn the stand-in Window");
    drop(client);
    drop(snapshot);
    let pid = child.id();
    // Never waited for here: the adopting daemon inherits it as a child.
    drop(child);
    let pidfd = rustix::process::pidfd_open(
        Pid::from_raw(i32::try_from(pid).unwrap()).unwrap(),
        rustix::process::PidfdFlags::empty(),
    )
    .unwrap();
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let start_time = stat[stat.rfind(')').unwrap() + 1..]
        .split_whitespace()
        .nth(19)
        .unwrap()
        .parse()
        .unwrap();
    let executable = fs::metadata(format!("/proc/{pid}/exe")).unwrap();
    StandInWindow {
        pid,
        start_time,
        executable: (executable.dev(), executable.ino()),
        pidfd,
        continuation,
    }
}

/// Writes what the relaunched stand-in Window sends after an adoption: the
/// continuation claim naming generation 2, a trusted-UI hello, and a
/// `ClaimHandoffControl` frame for the first carried Splint, whose
/// controller the manifest records as held by the Window's old connection.
/// The Window fills in the ticket it is granted. `options` adds the
/// stale-client wait, the claim delay, or the claim after a rollback
/// (naming generation 1, which the rollback daemon serves).
fn write_window_claim_frames(directory: &Path, stopped: &Stopped, options: &StageOptions) {
    let evidence = stopped.handed_off[0].handed_off.evidence;
    let claim_for = |daemon_generation| {
        splinterm_protocol::handoff_continuation::HandoffTicketClaim { daemon_generation }
            .encode()
            .iter()
            .fold(String::new(), |mut hex, byte| {
                use std::fmt::Write as _;
                let _ = write!(hex, "{byte:02x}");
                hex
            })
    };
    let claim = claim_for(2);
    let mut frames = json!({
        "claim": claim,
        "hello": ClientFrame::Hello {
            minimum_version: PROTOCOL_VERSION,
            maximum_version: PROTOCOL_VERSION,
            role: ClientRole::TrustedUi,
            handoff_window: false,
        },
        "request": ClientFrame::Request {
            request_id: 1,
            diagnostic_correlation: None,
            request: Request::ClaimHandoffControl {
                ticket: splinterm_protocol::HandoffResumeTicket::new(
                    [0; splinterm_protocol::HANDOFF_RESUME_TICKET_BYTES],
                ),
                splint: Some(splinterm_protocol::HandoffClaimTarget {
                    splint_id: evidence.splint_id,
                    incarnation: evidence.incarnation.value(),
                }),
            },
        },
        "result": directory.join(WINDOW_CLAIM),
        "splint_id": evidence.splint_id,
        "incarnation": evidence.incarnation.value(),
    });
    if options.window_stale_check {
        frames["ticket_file"] = json!(directory.join(WINDOW_TICKET));
        frames["proceed_file"] = json!(directory.join(STALE_CHECKED));
    }
    if let Some(seconds) = options.window_redeem_delay {
        frames["redeem_delay_seconds"] = json!(seconds);
    }
    if options.window_claim_after_rollback {
        frames["rollback_claim"] = json!(claim_for(1));
        frames["rollback_result"] = json!(directory.join(WINDOW_ROLLBACK_CLAIM));
    }
    write_atomically(
        &directory.join(WINDOW_CLAIM_FRAMES),
        &serde_json::to_vec(&frames).unwrap(),
    );
}

/// Installs `bytes` at `path` the way a package upgrade does: a new inode
/// renamed over the old one.
fn install(path: &Path, bytes: &[u8]) {
    let staging = path.with_extension("staging");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o755)
        .open(&staging)
        .unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
    drop(file);
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(&staging, path).unwrap();
}

fn bound_listener(path: &Path) -> UnixListener {
    let listener = UnixListener::bind(path).unwrap();
    listener.set_nonblocking(true).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    listener
}

/// One empty, close-on-exec, sealable memfd, as manifest building creates.
fn manifest_storage(name: &str) -> OwnedFd {
    memfd_create(name, MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING).unwrap()
}

fn daemon_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

/// The `splinterm-pty-child` this test was built with (build it first with
/// `cargo build -p splinterm-pty --bin splinterm-pty-child`).
fn pty_helper() -> PathBuf {
    let test_binary = env::current_exe().unwrap();
    let helper = test_binary
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("splinterm-pty-child");
    assert!(helper.is_file(), "missing PTY helper: {}", helper.display());
    helper
}

fn backend() -> LinuxPtyBackend {
    LinuxPtyBackend::new(pty_helper())
}

fn config() -> LiveSplintConfig {
    let mut config = LiveSplintConfig {
        columns: 60,
        rows: 24,
        hangup_grace: Duration::from_millis(30),
        terminate_grace: Duration::from_millis(30),
        poll_interval: Duration::from_millis(5),
        exit_drain_timeout: Duration::from_millis(50),
        ..LiveSplintConfig::default()
    };
    // The daemon's own budget limits: a checkpoint restores only against the
    // budget authority it was taken under.
    config.terminal.shared_image_budget = Some(SharedImageBudget::new(MAX_IMAGE_BYTES_PER_DAEMON));
    config.terminal.shared_kitty_upload_budget = Some(SharedKittyUploadBudget::new(
        DEFAULT_KITTY_UPLOAD_BYTES_PER_DAEMON,
    ));
    config
}

async fn screen_text(handle: &LiveSplintHandle) -> String {
    handle
        .snapshot()
        .await
        .unwrap()
        .visible_rows
        .iter()
        .flat_map(|row| row.cells.iter())
        .filter(|cell| cell.spacer_remaining.is_none())
        .map(|cell| cell.content.as_str())
        .collect()
}

/// Spawns two echoing Splints and one whose shell exits with
/// [`EXCLUDED_EXIT`] that its actor never reaps; captures every live one,
/// ends it for handoff, and stops the runtime.
fn hand_off_and_stop() -> Stopped {
    let runtime = daemon_runtime();
    let stopped = runtime.block_on(async {
        let mut live = Vec::new();
        for _ in 0..2 {
            let command = PtyCommand::new("/bin/sh", PathBuf::from("/tmp")).args(["-c", CAT]);
            let runtime = LiveSplintRuntime::spawn(SplintId::new(), backend(), command, config())
                .await
                .unwrap();
            let handle = runtime.handle();
            time::timeout(WAIT, async {
                while !screen_text(&handle).await.contains("ready") {
                    time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("a Splint never became ready");
            live.push(runtime);
        }
        // Polled once an hour, and its shell outlives the first poll, so its
        // actor never reaps the exited child.
        let mut lazy = config();
        lazy.poll_interval = Duration::from_secs(3600);
        let command = PtyCommand::new("/bin/sh", PathBuf::from("/tmp"))
            .args(["-c", &format!("sleep 0.3; exit {EXCLUDED_EXIT}")]);
        let excluded = LiveSplintRuntime::spawn(SplintId::new(), backend(), command, lazy)
            .await
            .unwrap();
        let excluded_handle = excluded.handle();
        time::timeout(WAIT, async {
            while excluded_handle.root_process_is_live() {
                time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the excluded Splint's shell never exited");

        let mut handles = live
            .iter()
            .map(LiveSplintRuntime::handle)
            .collect::<Vec<_>>();
        handles.push(excluded_handle.clone());
        let captures = capture_live_splints(
            handles,
            CaptureLimits::DEFAULT,
            |handle: LiveSplintHandle, budget: HandoffCheckpointBudget| async move {
                handle.prepare_pty_handoff_within(&budget).await
            },
        )
        .await
        .unwrap();
        assert_eq!(captures.captured().len(), 2);
        assert_eq!(captures.excluded().len(), 1);
        let (handed_off, _) = captures.hand_off();
        let mut carried = Vec::new();
        for runtime in live {
            let handle = runtime.handle();
            carried.push(RecordedSplint {
                splint_id: handle.splint_id,
                dojo_id: DojoId::new(),
                incarnation: handle.incarnation.value(),
                child_pid: handle.child_pid(),
            });
            // Each actor leaves through its handoff path.
            assert!(matches!(runtime.wait().await, Err(LiveError::HandedOff)));
        }
        let excluded_record = RecordedSplint {
            splint_id: excluded_handle.splint_id,
            dojo_id: DojoId::new(),
            incarnation: excluded_handle.incarnation.value(),
            child_pid: excluded_handle.child_pid(),
        };
        // Still running; it ends with the runtime, without reaping.
        drop(excluded);
        Stopped {
            handed_off,
            carried,
            excluded: excluded_record,
        }
    });
    runtime.shutdown_timeout(Duration::from_secs(2));
    stopped
}

/// The Topology the old generation persisted at its fence: one Lair, each
/// Splint alone in its own Dojo at its live incarnation.
fn durable_topology(lair_id: LairId, stopped: &Stopped) -> Topology {
    let dojos = stopped
        .carried
        .iter()
        .chain([&stopped.excluded])
        .enumerate()
        .map(|(index, splint)| Dojo {
            id: splint.dojo_id,
            name: format!("Dojo {}", index + 1),
            default_focus: splint.splint_id,
            root: LayoutNode::Leaf(Splint {
                id: splint.splint_id,
                title: "shell".into(),
                cwd: PathBuf::from("/tmp"),
                command: Vec::new(),
                launch: Box::new(SplintLaunchMetadata::default()),
                last_incarnation: Some(splint.incarnation),
                state: SplintState::Running,
                title_set: false,
            }),
        })
        .collect();
    let mut topology = Topology::new();
    topology
        .insert_lair_at(
            topology.revision(),
            Lair {
                id: lair_id,
                name: "handoff-exec".into(),
                lifetime: LairLifetime::Persistent,
                retention: LairRetention::Disposable,
                provenance: None,
                dojos,
            },
        )
        .unwrap();
    topology
}

/// Writes the durable Topology where the forward daemon's metadata store
/// reads it: `$XDG_STATE_HOME/splinterm/topology.json`, owner-only.
fn write_state(directory: &Path, encoded: &[u8]) {
    let state = directory.join("state/splinterm");
    fs::create_dir_all(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(state.join("topology.json"))
        .unwrap();
    file.write_all(encoded).unwrap();
    file.sync_all().unwrap();
}

fn write_atomically(path: &Path, bytes: &[u8]) {
    let staging = path.with_extension("staging");
    fs::write(&staging, bytes).unwrap();
    fs::rename(&staging, path).unwrap();
}

fn generation(identity: BuildIdentity, version: BuildVersion) -> Value {
    json!({
        "build_identity": identity.as_bytes(),
        "build_version": {
            "major": version.major,
            "minor": version.minor,
            "patch": version.patch,
        },
    })
}

fn executable_image(snapshot: &SealedExecutableSnapshot) -> Value {
    let source = snapshot.source();
    let sealed = snapshot.snapshot();
    json!({
        "source_device": source.device,
        "source_inode": source.inode,
        "source_owner_uid": source.owner_uid,
        "source_mode": source.mode,
        "source_size": source.size,
        "source_sha256": source.sha256,
        "snapshot_device": sealed.device,
        "snapshot_inode": sealed.inode,
        "snapshot_size": sealed.size,
        "snapshot_sha256": sealed.sha256,
        "snapshot_seals": sealed.seals.bits(),
    })
}

/// The forward and rollback manifests for this attempt, as the old
/// generation's manifest builder writes them from its captures: the
/// rollback pair is this build unsalted, the forward pair the salted image.
#[allow(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    reason = "one fixture binds every carried role into one manifest pair"
)]
fn manifest_pair(
    prefix: &Path,
    snapshots: &HandoffExecutableSnapshots,
    owners: &[HandoffDescriptor<'_>],
    stopped: &Stopped,
    lair_id: LairId,
    revision: u64,
    tamper_rollback_image: bool,
    window: Option<&StandInWindow>,
) -> (Vec<u8>, Vec<u8>) {
    let compiled = compiled_handoff_generation().unwrap();
    let source = generation(compiled.build_identity, compiled.build_version);
    let target = generation(
        salted_pair_build_identity(compiled.build_identity, SALT),
        compiled.build_version,
    );
    let capabilities = compiled.capabilities;
    let splints = stopped
        .handed_off
        .iter()
        .map(|capture| {
            let evidence = capture.handed_off.evidence;
            let recorded = stopped
                .carried
                .iter()
                .find(|splint| splint.splint_id == evidence.splint_id)
                .unwrap();
            json!({
                "lair_id": lair_id,
                "dojo_id": recorded.dojo_id,
                "splint_id": evidence.splint_id,
                "incarnation": evidence.incarnation.value(),
                "pty": {
                    "child_pid": evidence.pty_identity.child_pid(),
                    "process_group": evidence.pty_identity.process_group(),
                    "session_id": evidence.pty_identity.session_id(),
                    "master_slot": capture.slot(),
                },
                "size": {
                    "columns": evidence.size.columns,
                    "rows": evidence.size.rows,
                    "pixel_width": evidence.size.pixel_width,
                    "pixel_height": evidence.size.pixel_height,
                },
                "foreground_process_group": evidence.foreground_process_group,
                "terminal_revision": evidence.terminal_revision.value(),
                "history_generation": evidence.history_generation,
                "exit": "running",
                "terminal_checkpoint": capture.checkpoint.terminal_disposition(),
                "image_state": capture.checkpoint.image_disposition(),
                "controller": "released",
                "subscriptions": "dropped",
            })
        })
        .collect::<Vec<_>>();
    let mut splints = splints;
    let windows = window.map_or_else(Vec::new, |window| {
        // The first carried Splint's controller was the Window's.
        splints[0]["controller"] = json!({
            "held_by": { "window": 0, "connection": WINDOW_CONNECTION },
        });
        vec![json!({
            "index": 0,
            "pid": window.pid,
            "start_time": window.start_time,
            "client_device": window.executable.0,
            "client_inode": window.executable.1,
            "old_connections": [WINDOW_CONNECTION],
        })]
    });
    let excluded = stopped.excluded;
    let descriptors = owners
        .iter()
        .map(|owner| {
            json!({
                "slot": owner.slot(),
                "inherited_fd": u32::try_from(owner.raw_fd()).unwrap(),
            })
        })
        .collect::<Vec<_>>();
    let handoff_id = [0x6d_u8; 16];
    let mut forward = json!({
        "schema": 1,
        "kind": "forward",
        "handoff_id": handoff_id,
        "source_generation": source.clone(),
        "target_generation": target.clone(),
        "source_daemon_generation": 1,
        "target_daemon_generation": 2,
        "topology_revision": revision,
        "durable_state_generation": revision,
        "private_protocol": capabilities.private_protocol.maximum(),
        "handoff_protocol": capabilities.handoff_protocol.maximum(),
        "terminal_checkpoint_schema": capabilities.terminal_checkpoint.maximum(),
        "descriptor_manifest_schema": capabilities.descriptor_manifest.maximum(),
        "install_prefix": prefix,
        "executable_pairs": {
            "forward": {
                "daemon": executable_image(snapshots.forward().daemon()),
                "client": executable_image(snapshots.forward().client()),
                "generation": target.clone(),
            },
            "rollback": {
                "daemon": executable_image(snapshots.rollback().daemon()),
                "client": executable_image(snapshots.rollback().client()),
                "generation": source.clone(),
            },
        },
        "counterpart_manifest_slot": HandoffDescriptorSlot::RollbackManifest,
        "splints": splints,
        // No exit recorded: the forward daemon must reap the zombie itself.
        "excluded_splints": [{
            "lair_id": lair_id,
            "dojo_id": excluded.dojo_id,
            "splint_id": excluded.splint_id,
            "incarnation": excluded.incarnation,
            "child_pid": excluded.child_pid,
            "exit": null,
        }],
        "windows": windows,
        "descriptors": descriptors,
    });
    if tamper_rollback_image {
        // Both manifests stay a valid pair (validation requires the source
        // and snapshot digests to agree); only the digest is wrong.
        let image = &mut forward["executable_pairs"]["rollback"]["daemon"];
        let mut digest: Vec<u8> = serde_json::from_value(image["snapshot_sha256"].clone()).unwrap();
        digest[0] ^= 0xff;
        image["snapshot_sha256"] = json!(digest);
        image["source_sha256"] = json!(digest);
    }
    let mut rollback = forward.clone();
    rollback["kind"] = json!("rollback");
    rollback["source_generation"] = target;
    rollback["target_generation"] = source;
    rollback["source_daemon_generation"] = json!(2);
    rollback["target_daemon_generation"] = json!(1);
    rollback["counterpart_manifest_slot"] = json!(HandoffDescriptorSlot::CheckpointManifest);
    encode_manifest_pair_for_integration_test(forward, rollback).unwrap()
}

// ---------------------------------------------------------------------------
// The parent: launch, observe, and clean up
// ---------------------------------------------------------------------------

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

/// A private directory, short enough for the socket paths under it.
struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            % 1_000_000_000;
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path =
            env::temp_dir().join(format!("sd-hx-{}-{unique}-{sequence}", std::process::id()));
        fs::create_dir(&path).unwrap();
        // The manifest's install prefix must be canonical.
        Self(fs::canonicalize(path).unwrap())
    }

    fn socket(&self) -> PathBuf {
        self.0.join("s.sock")
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One launched stage: the `close_fds` launcher and, under it, the stage
/// process that becomes the forward daemon. Dropping it kills every process
/// the stage left: its children first, while they cannot have been reaped,
/// then the stage itself.
struct Stage {
    launcher: Child,
    directory: PathBuf,
}

impl Stage {
    fn launch(directory: &TestDirectory, options: StageOptions) -> Self {
        Self::launch_program(directory, options, &env::current_exe().unwrap(), &[])
    }

    /// Launches `program` instead of this test binary's stage, with the
    /// stage's private environment plus `extra`: the old-side drill runs a
    /// real `splinterd` this way.
    fn launch_program(
        directory: &TestDirectory,
        options: StageOptions,
        program: &Path,
        extra: &[(&str, &str)],
    ) -> Self {
        let stdout = fs::File::create(directory.0.join(STDOUT)).unwrap();
        let stderr = fs::File::create(directory.0.join(STDERR)).unwrap();
        let mut command = Command::new("python3");
        command.env_remove(REMOVE_FORWARD_CLIENT);
        command.env_remove(PENDING_SIGTERM);
        command.env_remove(TAMPER_ROLLBACK_IMAGE);
        command.env_remove(WINDOW);
        command.env_remove(ALIAS_WINDOW_CONTINUATION);
        command.env_remove(WINDOW_STALE_CHECK);
        command.env_remove(WINDOW_REDEEM_DELAY);
        command.env_remove(WINDOW_CLAIM_AFTER_ROLLBACK);
        command.env_remove(CANDIDATE_FAULT_ENV);
        command.env_remove(OLD_SIDE_DRILL_ENV);
        for (name, value) in extra {
            command.env(name, value);
        }
        if options.remove_forward_client {
            command.env(REMOVE_FORWARD_CLIENT, "1");
        }
        if options.pending_sigterm {
            command.env(PENDING_SIGTERM, "1");
        }
        if options.tamper_rollback_image {
            command.env(TAMPER_ROLLBACK_IMAGE, "1");
        }
        if options.window {
            command.env(WINDOW, "1");
        }
        if options.alias_window_continuation {
            command.env(ALIAS_WINDOW_CONTINUATION, "1");
        }
        if options.window_stale_check {
            command.env(WINDOW_STALE_CHECK, "1");
        }
        if let Some(seconds) = options.window_redeem_delay {
            command.env(WINDOW_REDEEM_DELAY, seconds.to_string());
        }
        if options.window_claim_after_rollback {
            command.env(WINDOW_CLAIM_AFTER_ROLLBACK, "1");
        }
        if let Some(fault) = options.fault {
            command.env(CANDIDATE_FAULT_ENV, fault);
        }
        let launcher = command
            .arg("-c")
            .arg(LAUNCHER)
            .arg(program)
            .env(PARENT, std::process::id().to_string())
            .env(STAGE, "old-generation")
            .env(DIRECTORY, &directory.0)
            .env("SPLINTERM_SOCKET", directory.socket())
            .env("XDG_STATE_HOME", directory.0.join("state"))
            .env("XDG_CONFIG_HOME", directory.0.join("config"))
            // The private directory that holds the socket is the stage's
            // runtime directory too, so anything a generation wrote there is
            // in what `assert_no_checkpoint_artifact` scans.
            .env("XDG_RUNTIME_DIR", &directory.0)
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent/splinterd-exec-adoption-bus",
            )
            .env(PACKAGED_OWNER_UID_ENV, geteuid().as_raw().to_string())
            .env("SPLINTERM_ENABLE_DEV_ATTACH", "1")
            .env("SPLINTERM_TEST_SHUTDOWN_GRACE_MS", "200")
            .env("RUST_LOG", "info")
            .env("NO_COLOR", "1")
            .env_remove("SPLINTERM_POLICY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .current_dir(&directory.0)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .expect("launch a stage without inherited descriptors");
        Self {
            launcher,
            directory: directory.0.clone(),
        }
    }

    fn logs(&self) -> String {
        let read = |name| {
            strip_ansi(&String::from_utf8_lossy(
                &fs::read(self.directory.join(name)).unwrap_or_default(),
            ))
        };
        format!("stdout:\n{}\nstderr:\n{}", read(STDOUT), read(STDERR))
    }

    /// # Panics
    /// When the launcher's status cannot be read; `Drop` never calls this.
    fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.launcher.try_wait().unwrap()
    }

    /// Waits for the record the stage writes just before exec.
    fn record(&mut self) -> StageRecord {
        let path = self.directory.join(RECORD);
        let deadline = Instant::now() + STAGE_WAIT;
        loop {
            if let Ok(bytes) = fs::read(&path) {
                return serde_json::from_slice(&bytes).unwrap();
            }
            if let Some(status) = self.exited() {
                panic!("stage exited with {status} before exec\n{}", self.logs());
            }
            assert!(
                Instant::now() < deadline,
                "stage never reached exec\n{}",
                self.logs()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Waits until the stage's logs contain `marker`.
    fn wait_for_log(&mut self, marker: &str) {
        let deadline = Instant::now() + STAGE_WAIT;
        while !self.logs().contains(marker) {
            if let Some(status) = self.exited() {
                panic!(
                    "stage exited with {status} before {marker:?}\n{}",
                    self.logs()
                );
            }
            assert!(
                Instant::now() < deadline,
                "stage never logged {marker:?}\n{}",
                self.logs()
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Waits for the first packet the stand-in Window read and decodes it.
    fn window_outcome(&mut self) -> HandoffOutcome {
        let path = self.directory.join(WINDOW_OUTCOME);
        let deadline = Instant::now() + STAGE_WAIT;
        loop {
            if let Ok(packet) = fs::read(&path) {
                return HandoffOutcome::decode(&packet).unwrap_or_else(|error| {
                    panic!("the Window read {packet:?}: {error}\n{}", self.logs())
                });
            }
            assert!(
                Instant::now() < deadline,
                "the Window never read an outcome\n{}",
                self.logs()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Waits for what the relaunched stand-in Window recorded of its claim.
    fn window_claim(&mut self) -> Value {
        self.window_record(WINDOW_CLAIM, STAGE_WAIT)
    }

    /// Waits up to `wait` for the JSON record the stand-in Window writes at
    /// `name`.
    fn window_record(&mut self, name: &str, wait: Duration) -> Value {
        let path = self.directory.join(name);
        let deadline = Instant::now() + wait;
        loop {
            if let Ok(bytes) = fs::read(&path) {
                return serde_json::from_slice(&bytes).unwrap();
            }
            assert!(
                Instant::now() < deadline,
                "the stand-in Window never recorded {name}\n{}",
                self.logs()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Waits for the launcher, and so the stage, to exit.
    fn wait(&mut self, timeout: Duration) -> std::process::ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.exited() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "stage did not exit\n{}",
                self.logs()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Stage {
    // Runs during unwinding too, so nothing here may panic.
    fn drop(&mut self) {
        if self.launcher.try_wait().ok().flatten().is_none() {
            for stage in children_of(self.launcher.id()) {
                for child in children_of(stage) {
                    signal(child, Signal::KILL);
                }
                signal(stage, Signal::KILL);
            }
            let _ = self.launcher.kill();
            let _ = self.launcher.wait();
        }
    }
}

fn signal(pid: u32, signal: Signal) {
    if let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) {
        let _ = kill_process(pid, signal);
    }
}

/// A process's state letter and parent PID, or `None` once it is gone.
fn proc_stat(pid: u32) -> Option<(char, u32)> {
    let text = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let mut fields = text[text.rfind(')')? + 1..].split_whitespace();
    let state = fields.next()?.chars().next()?;
    let parent = fields.next()?.parse().ok()?;
    Some((state, parent))
}

fn children_of(parent: u32) -> Vec<u32> {
    fs::read_dir("/proc")
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
                .filter(|pid| proc_stat(*pid).is_some_and(|(_, ppid)| ppid == parent))
                .collect()
        })
        .unwrap_or_default()
}

/// A live (not zombie) child of `parent`.
fn is_live_child(pid: u32, parent: u32) -> bool {
    proc_stat(pid).is_some_and(|(state, ppid)| ppid == parent && state != 'Z')
}

/// Waits until `pid` has exited (it may linger as someone else's zombie).
fn wait_until_exited(pid: u32, what: &str) {
    let deadline = Instant::now() + WAIT;
    while proc_stat(pid).is_some_and(|(state, _)| state != 'Z') {
        assert!(Instant::now() < deadline, "{what} {pid} is still running");
        thread::sleep(Duration::from_millis(20));
    }
}

fn strip_ansi(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character == '\u{1b}' {
            for next in characters.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            plain.push(character);
        }
    }
    plain
}

/// The persisted Topology, read back as the daemon's restore path reads it.
fn persisted_topology(directory: &TestDirectory) -> Topology {
    let bytes = fs::read(directory.0.join("state/splinterm/topology.json")).unwrap();
    TopologyDocument::decode(&bytes)
        .unwrap()
        .into_topology()
        .unwrap()
}

/// The terminal-body sentinel every carried Splint printed before capture.
fn sentinel() -> String {
    SENTINEL_PIECES.join("-")
}

/// How many descriptors of `pid` are sealed terminal checkpoints.
fn checkpoint_descriptors(pid: u32) -> usize {
    fs::read_dir(format!("/proc/{pid}/fd"))
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| {
                    fs::read_link(entry.path())
                        .is_ok_and(|target| target.to_string_lossy().contains(CHECKPOINT_MEMFD))
                })
                .count()
        })
        .unwrap_or(0)
}

/// Plan 0037 M5: once a generation committed its adoption, it holds no
/// checkpoint: no descriptor and no mapping.
fn assert_checkpoints_released(pid: u32) {
    assert_eq!(
        checkpoint_descriptors(pid),
        0,
        "the serving daemon still holds a checkpoint descriptor"
    );
    let maps = fs::read_to_string(format!("/proc/{pid}/maps")).unwrap();
    assert!(
        !maps.contains(CHECKPOINT_MEMFD),
        "the serving daemon still maps a checkpoint"
    );
}

/// Plan 0037 M5: after a case, however its candidate ended (adopted, rolled
/// back, crashed, hung, killed, or stopped), nothing under the test's private
/// directory, which is the stage's socket and runtime directory, state and
/// configuration homes, and working directory, is named for a checkpoint,
/// and no file there holds a terminal body: not the durable Topology or its
/// backup, the logs (the journal's stand-in), or any record. `/dev/shm` is
/// only listed, for a checkpoint's name or this directory's unique one.
///
/// The install prefix and the stale client link hold only executables the
/// stage installed before any Splint ran, so their large images are checked
/// by name, not scanned.
fn assert_no_checkpoint_artifact(directory: &TestDirectory) {
    let sentinel = sentinel();
    let prefix = directory.0.join("prefix");
    let stale_client = directory.0.join(STALE_CLIENT);
    let mut pending = vec![directory.0.clone()];
    let mut scanned = 0_usize;
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            assert!(
                !entry
                    .file_name()
                    .to_string_lossy()
                    .to_lowercase()
                    .contains("checkpoint"),
                "a checkpoint-named artifact exists: {}",
                path.display()
            );
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() && !path.starts_with(&prefix) && path != stale_client {
                let bytes = fs::read(&path).unwrap();
                assert!(
                    !bytes
                        .windows(sentinel.len())
                        .any(|window| window == sentinel.as_bytes()),
                    "{} holds a terminal body",
                    path.display()
                );
                scanned += 1;
            }
        }
    }
    assert!(
        directory.0.join("state/splinterm/topology.json").is_file(),
        "the durable Topology was not scanned"
    );
    assert!(scanned > 0, "nothing was scanned");
    let unique = directory
        .0
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    if let Ok(entries) = fs::read_dir("/dev/shm") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            assert!(
                !name.contains(CHECKPOINT_MEMFD) && !name.contains(&unique),
                "/dev/shm/{name} is a named checkpoint artifact"
            );
        }
    }
}

/// A protocol client of the adopted daemon.
struct Client {
    stream: UnixStream,
    request_id: u64,
}

impl Client {
    /// Connects and says hello; returns the client and the served
    /// generation, or `None` when the daemon did not answer in time.
    async fn hello(socket: &Path) -> Option<(Self, u64)> {
        let mut stream = UnixStream::connect(socket).await.ok()?;
        write_frame(
            &mut stream,
            &ClientFrame::Hello {
                minimum_version: PROTOCOL_VERSION,
                maximum_version: PROTOCOL_VERSION,
                role: ClientRole::Automation,
                handoff_window: false,
            },
        )
        .await;
        match time::timeout(Duration::from_secs(2), read_frame(&mut stream))
            .await
            .ok()??
        {
            ServerFrame::Hello {
                version,
                development_terminal_access,
                daemon_generation,
                ..
            } => {
                assert_eq!(version, PROTOCOL_VERSION);
                assert!(development_terminal_access);
                Some((
                    Self {
                        stream,
                        request_id: 1,
                    },
                    daemon_generation,
                ))
            }
            frame => panic!("unexpected hello: {frame:?}"),
        }
    }

    async fn request_result(&mut self, request: Request) -> Result<Response, ProtocolError> {
        let request_id = self.request_id;
        self.request_id += 1;
        write_frame(
            &mut self.stream,
            &ClientFrame::Request {
                request_id,
                diagnostic_correlation: None,
                request,
            },
        )
        .await;
        loop {
            let frame = time::timeout(WAIT, read_frame(&mut self.stream))
                .await
                .expect("timed out waiting for a response")
                .expect("connection closed before a response");
            match frame {
                ServerFrame::Response {
                    request_id: response_id,
                    result,
                } if response_id == request_id => return Ok(result),
                ServerFrame::Error {
                    request_id: Some(response_id),
                    error,
                } if response_id == request_id => return Err(error),
                ServerFrame::Event { .. } => {}
                frame => panic!("unexpected frame: {frame:?}"),
            }
        }
    }

    async fn request(&mut self, request: Request) -> Response {
        self.request_result(request)
            .await
            .unwrap_or_else(|error| panic!("request failed: {:?}: {}", error.code, error.message))
    }

    async fn snapshot(&mut self, splint_id: SplintId, incarnation: u64) -> TerminalSnapshot {
        let Response::Attached {
            subscription_id,
            snapshot,
            ..
        } = self
            .request(Request::Attach {
                splint_id,
                incarnation: Some(incarnation),
                scrollback_rows: 16,
            })
            .await
        else {
            panic!("attach was not granted");
        };
        assert_eq!(
            self.request(Request::Detach { subscription_id }).await,
            Response::Acknowledged
        );
        snapshot
    }

    /// Plan 0037 M5: the carried screen and scrollback still hold the
    /// sentinel, so the checkpoint really carried the terminal body.
    async fn assert_body_carried(&mut self, splint_id: SplintId, incarnation: u64) {
        let Response::Attached {
            subscription_id,
            snapshot,
            ..
        } = self
            .request(Request::Attach {
                splint_id,
                incarnation: Some(incarnation),
                scrollback_rows: 64,
            })
            .await
        else {
            panic!("attach was not granted");
        };
        assert_eq!(
            self.request(Request::Detach { subscription_id }).await,
            Response::Acknowledged
        );
        let sentinel = sentinel();
        for (part, rows) in [
            ("scrollback", &snapshot.scrollback_rows),
            ("screen", &snapshot.visible_rows),
        ] {
            let text = rows
                .iter()
                .flat_map(|row| row.cells.iter())
                .map(|cell| cell.content.as_str())
                .collect::<String>();
            assert!(
                text.contains(&sentinel),
                "{splint_id}'s carried {part} lost the terminal body: {text:?}"
            );
        }
    }

    /// Plan 0037 M5: the serving generation's metadata observations and
    /// audit records, its adoption's record included, hold no terminal body.
    async fn assert_observations_body_free(&mut self, record: &StageRecord) {
        let mut observations = vec![
            self.request(Request::ListLairs).await,
            self.request(Request::InspectTopology).await,
        ];
        for splint in record.carried.iter().chain([&record.excluded]) {
            observations.push(
                self.request(Request::InspectSplint {
                    splint_id: splint.splint_id,
                })
                .await,
            );
        }
        let audit = self
            .request(Request::AuditInspect {
                after_audit_id: None,
                max_records: 128,
            })
            .await;
        let Response::AuditPage { page } = &audit else {
            panic!("no audit page: {audit:?}");
        };
        assert!(
            page.records.iter().any(|record| record.operation
                == splinterm_protocol::AuditOperation::PrepareDaemonHandoff),
            "the adoption was not audited: {page:?}"
        );
        observations.push(audit);
        let sentinel = sentinel();
        for observation in &observations {
            let encoded = serde_json::to_string(observation).unwrap();
            assert!(
                !encoded.contains(&sentinel),
                "an observation holds a terminal body: {encoded}"
            );
        }
    }

    async fn wait_for_text(&mut self, splint_id: SplintId, incarnation: u64, text: &str) {
        self.wait_for_occurrences(splint_id, incarnation, text, 1)
            .await;
    }

    async fn wait_for_occurrences(
        &mut self,
        splint_id: SplintId,
        incarnation: u64,
        text: &str,
        occurrences: usize,
    ) {
        let deadline = Instant::now() + WAIT;
        loop {
            let screen = snapshot_text(&self.snapshot(splint_id, incarnation).await);
            if screen.matches(text).count() >= occurrences {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{splint_id} never showed {text:?} {occurrences} times: {screen:?}"
            );
            time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn echo(&mut self, splint_id: SplintId, incarnation: u64, token: &str) {
        let Response::ControlGranted { controller_id, .. } = self
            .request(Request::AcquireControl {
                splint_id,
                incarnation,
                modes: vec![ControlMode::Input],
            })
            .await
        else {
            panic!("control was not granted");
        };
        assert!(matches!(
            self.request(Request::Input {
                controller_id,
                splint_id,
                incarnation,
                bytes: format!("{token}\n").into_bytes(),
                focus_report: false,
            })
            .await,
            Response::TerminalActionAcknowledged { .. }
        ));
        // Twice: the line discipline echoes the input, and only a live shell
        // (`cat`) writes it back again.
        self.wait_for_occurrences(splint_id, incarnation, token, 2)
            .await;
        assert_eq!(
            self.request(Request::ReleaseControl { controller_id })
                .await,
            Response::Acknowledged
        );
    }
}

async fn write_frame(stream: &mut UnixStream, frame: &ClientFrame) {
    stream
        .write_all(&encode_frame(frame).unwrap())
        .await
        .unwrap();
}

async fn read_frame(stream: &mut UnixStream) -> Option<ServerFrame> {
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length).await.ok()?;
    let length = u32::from_be_bytes(length) as usize;
    assert!((1..=MAX_FRAME_BYTES).contains(&length));
    let mut body = vec![0_u8; length];
    stream.read_exact(&mut body).await.ok()?;
    Some(serde_json::from_slice(&body).unwrap())
}

fn snapshot_text(snapshot: &TerminalSnapshot) -> String {
    snapshot
        .scrollback_rows
        .iter()
        .chain(&snapshot.visible_rows)
        .flat_map(|row| row.cells.iter())
        .map(|cell| cell.content.as_str())
        .collect()
}

/// Connects to the adopted daemon, retrying until it serves or the stage
/// exits. A connection queued while the old generation still held the
/// listener is answered by the forward daemon, which inherited it.
async fn connect_adopted(stage: &mut Stage, socket: &Path) -> (Client, u64) {
    let deadline = Instant::now() + STAGE_WAIT;
    loop {
        if let Some(connected) = Client::hello(socket).await {
            return connected;
        }
        if let Some(status) = stage.exited() {
            panic!(
                "the stage exited with {status} instead of serving\n{}",
                stage.logs()
            );
        }
        assert!(
            Instant::now() < deadline,
            "the forward daemon never served\n{}",
            stage.logs()
        );
        time::sleep(Duration::from_millis(50)).await;
    }
}

/// The full handoff: the stage execs the salted forward daemon, which
/// adopts, commits, and serves as generation 2 in the same process.
fn adopt_case() {
    let directory = TestDirectory::new();
    let mut stage = Stage::launch(
        &directory,
        StageOptions {
            window: true,
            window_stale_check: true,
            ..StageOptions::default()
        },
    );
    let record = stage.record();
    let pid = record.pid;
    // The stage is the launcher's only child.
    assert_eq!(children_of(stage.launcher.id()), [pid]);
    // While the relaunched Window holds its granted ticket unredeemed.
    assert_stale_client_refused(&mut stage, &directory);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let launched = runtime.block_on(async {
        let (mut client, daemon_generation) =
            connect_adopted(&mut stage, &directory.socket()).await;
        // The next served generation.
        assert_eq!(daemon_generation, record.source_daemon_generation + 1);
        assert_same_process(&client, &stage, pid);

        // Every carried Splint: the same process, still this daemon's child,
        // at its original incarnation, its captured screen restored, and
        // echoing through the adopted daemon.
        for splint in &record.carried {
            assert!(
                is_live_child(splint.child_pid, pid),
                "carried Splint {} lost its process",
                splint.splint_id
            );
            let Response::Splint { runtime, .. } = client
                .request(Request::InspectSplint {
                    splint_id: splint.splint_id,
                })
                .await
            else {
                panic!("no Splint response");
            };
            assert_eq!(runtime.live_incarnation, Some(splint.incarnation));
            client
                .wait_for_text(splint.splint_id, splint.incarnation, "ready")
                .await;
            client
                .assert_body_carried(splint.splint_id, splint.incarnation)
                .await;
            client
                .echo(splint.splint_id, splint.incarnation, "echo-after-exec")
                .await;
        }
        assert_checkpoints_released(pid);
        client.assert_observations_body_free(&record).await;

        assert_exits_recorded(&mut client, &directory, &record).await;
        assert_listeners_accept(&directory, daemon_generation).await;
        assert_daemon_threads_unblocked(pid);
        assert_new_splint_unblocked(&mut client, pid, &record).await
    });

    // No workload unit manager on either side: nothing was contained, so no
    // scope could be stopped, and the adoption carried exactly what the
    // manifest named with no stray descriptor.
    let logs = stage.logs();
    for expected in [
        "daemon handoff candidate: adopting",
        "workload_units=false",
        "daemon handoff: adoption committed",
        "daemon_generation=2",
        "carried=2",
        "excluded=1",
        "excluded_exit_unknown=0",
        "closed_unexpected=0",
        "windows_announced=1",
        "windows_exited=0",
        "windows_unsent=0",
        "held_controllers=1",
        "daemon handoff: adopted generation published",
    ] {
        assert!(logs.contains(expected), "missing {expected:?}\n{logs}");
    }
    // The carried Window read exactly the forward daemon's outcome.
    assert!(record.window_pid.is_some());
    assert_eq!(
        stage.window_outcome(),
        HandoffOutcome {
            kind: HandoffOutcomeKind::Adopted,
            daemon_generation: record.source_daemon_generation + 1,
        }
    );
    assert_window_restored_its_controller(&mut stage, &directory);
    // Plan 0061 D8: the adopted daemon serves on, but hands off no further.
    assert_refuses_another_handoff(&directory, record.source_daemon_generation + 1, &[]);
    assert_same_pid_serves(&mut stage, &directory, pid);

    // An ordered shutdown of the adopted daemon ends it and its Splints.
    shut_down(&mut stage, &record, &[launched]);
    assert_no_checkpoint_artifact(&directory);
}

/// The `PYTHONHOME` a copy of `python3` needs when it runs from another
/// path.
fn python_home() -> String {
    let output = Command::new("python3")
        .arg("-c")
        .arg("import sys; print(sys.prefix if sys.prefix == sys.exec_prefix else sys.prefix + ':' + sys.exec_prefix)")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// Runs one [`CLIENT_SESSION`] from `client`, a copy of `python3` whose
/// executable identity the daemon judges: `hello` with `role`, then each
/// request. Returns the hello answer and one answer per request sent; `None`
/// where the daemon closed the connection first.
fn client_session(
    client: &Path,
    socket: &Path,
    role: ClientRole,
    requests: &[Request],
) -> Vec<Option<ServerFrame>> {
    let spec_path = client_session_spec(socket, role, requests, None, None);
    let output = Command::new(client)
        .arg("-c")
        .arg(CLIENT_SESSION)
        .arg(&spec_path)
        .env("PYTHONHOME", python_home())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{} session failed: {output:?}",
        client.display()
    );
    session_answers(&output.stdout)
}

/// [`client_session`], paused on its one connection before request
/// `pause_before`: `between` runs with the answers so far, then the session
/// resumes. Returns every answer.
fn paused_client_session(
    client: &Path,
    socket: &Path,
    role: ClientRole,
    requests: &[Request],
    pause_before: usize,
    second_requests: Option<&[Request]>,
    between: impl FnOnce(&[Option<ServerFrame>]),
) -> Vec<Option<ServerFrame>> {
    use std::io::{BufRead as _, BufReader, Read as _};

    let spec_path =
        client_session_spec(socket, role, requests, Some(pause_before), second_requests);
    let mut session = Command::new(client)
        .arg("-c")
        .arg(CLIENT_SESSION)
        .arg(&spec_path)
        .env("PYTHONHOME", python_home())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(session.stdout.take().unwrap());
    let mut paused = String::new();
    stdout.read_line(&mut paused).unwrap();
    assert!(
        !paused.is_empty(),
        "{} session ended before its pause: {:?}",
        client.display(),
        session.wait()
    );
    between(&session_answers(paused.as_bytes()));
    session
        .stdin
        .take()
        .unwrap()
        .write_all(b"resume\n")
        .unwrap();
    let mut rest = Vec::new();
    stdout.read_to_end(&mut rest).unwrap();
    let status = session.wait().unwrap();
    assert!(
        status.success(),
        "{} session failed: {status}",
        client.display()
    );
    session_answers(&rest)
}

/// Writes a [`CLIENT_SESSION`] spec beside `socket` and returns its path.
fn client_session_spec(
    socket: &Path,
    role: ClientRole,
    requests: &[Request],
    pause_before: Option<usize>,
    second_requests: Option<&[Request]>,
) -> PathBuf {
    let frames = |requests: &[Request]| {
        requests
            .iter()
            .enumerate()
            .map(|(index, request)| ClientFrame::Request {
                request_id: u64::try_from(index).unwrap() + 1,
                diagnostic_correlation: None,
                request: request.clone(),
            })
            .collect::<Vec<_>>()
    };
    let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
    let spec_path = socket.with_file_name(format!("client-session-{sequence}.json"));
    let spec = json!({
        "pause_before": pause_before,
        "socket": socket,
        "hello": ClientFrame::Hello {
            minimum_version: PROTOCOL_VERSION,
            maximum_version: PROTOCOL_VERSION,
            role,
            handoff_window: false,
        },
        "requests": frames(requests),
        "second_requests": second_requests.map(frames),
    });
    fs::write(&spec_path, serde_json::to_vec(&spec).unwrap()).unwrap();
    spec_path
}

/// Decodes the JSON answer list a [`CLIENT_SESSION`] printed.
fn session_answers(printed: &[u8]) -> Vec<Option<ServerFrame>> {
    let answers: Vec<Option<Value>> = serde_json::from_slice(printed).unwrap();
    answers
        .into_iter()
        .map(|answer| {
            answer.map(|frame| {
                serde_json::from_value(frame.clone())
                    .unwrap_or_else(|error| panic!("{error}: {frame}"))
            })
        })
        .collect()
}

fn hex_ticket(hex: &str) -> [u8; splinterm_protocol::HANDOFF_RESUME_TICKET_BYTES] {
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
        .collect::<Vec<_>>();
    bytes.try_into().expect("a whole resume ticket")
}

/// Plan 0061 M3.17, the stale client: once the relaunched Window has been
/// granted its ticket (it waits before redeeming), a process executing the
/// pre-upgrade client inode, with exactly the bytes of the installed client,
/// is refused trusted UI at `Hello`: that is the stale-inode check. As an
/// automation client it cannot present that very ticket either, but that
/// refusal is the authorization table's for every automation client (a
/// policy denial), not an executable check. The Window then redeems the
/// ticket ([`assert_window_restored_its_controller`]): neither refusal
/// reached or ended it.
fn assert_stale_client_refused(stage: &mut Stage, directory: &TestDirectory) {
    use std::os::unix::fs::MetadataExt as _;

    let ticket_path = directory.0.join(WINDOW_TICKET);
    let deadline = Instant::now() + STAGE_WAIT;
    let ticket = loop {
        if let Ok(hex) = fs::read_to_string(&ticket_path) {
            break hex_ticket(hex.trim());
        }
        assert!(
            Instant::now() < deadline,
            "the relaunched Window was never granted a ticket\n{}",
            stage.logs()
        );
        thread::sleep(Duration::from_millis(20));
    };
    let stale_client = directory.0.join(STALE_CLIENT);
    let installed = directory.0.join("prefix/splinterm");
    assert_eq!(
        fs::read(&stale_client).unwrap(),
        fs::read(&installed).unwrap()
    );
    let identity = |path: &Path| {
        let metadata = fs::metadata(path).unwrap();
        (metadata.dev(), metadata.ino())
    };
    assert_ne!(identity(&stale_client), identity(&installed));
    let socket = directory.socket();

    let answers = client_session(&stale_client, &socket, ClientRole::TrustedUi, &[]);
    match answers.as_slice() {
        [Some(ServerFrame::Error { error, .. })] if error.code == ErrorCode::Unauthorized => {}
        answers => panic!(
            "the stale client was not refused trusted UI: {answers:?}\n{}",
            stage.logs()
        ),
    }
    let frames: Value =
        serde_json::from_slice(&fs::read(directory.0.join(WINDOW_CLAIM_FRAMES)).unwrap()).unwrap();
    let claim = Request::ClaimHandoffControl {
        ticket: splinterm_protocol::HandoffResumeTicket::new(ticket),
        splint: Some(splinterm_protocol::HandoffClaimTarget {
            splint_id: serde_json::from_value(frames["splint_id"].clone()).unwrap(),
            incarnation: frames["incarnation"].as_u64().unwrap(),
        }),
    };
    let answers = client_session(&stale_client, &socket, ClientRole::Automation, &[claim]);
    match answers.as_slice() {
        [
            Some(ServerFrame::Hello { .. }),
            Some(ServerFrame::Error { error, .. }),
        ] if error.code == ErrorCode::Unauthorized => {}
        answers => panic!(
            "the stale client could present the ticket: {answers:?}\n{}",
            stage.logs()
        ),
    }
    fs::write(directory.0.join(STALE_CHECKED), b"").unwrap();
}

/// Plan 0061 D8: a daemon started by a live handoff refuses another. Over
/// trusted UI from the installed client, the dry run reports no rollback
/// copy and `Execute` is refused, saying a restart comes first. `first`
/// requests run before those two on the same connection.
fn assert_refuses_another_handoff(
    directory: &TestDirectory,
    daemon_generation: u64,
    first: &[Request],
) -> Vec<Option<ServerFrame>> {
    let mut requests = first.to_vec();
    requests.extend([
        Request::PrepareDaemonHandoff {
            mode: DaemonHandoffMode::DryRun,
        },
        Request::PrepareDaemonHandoff {
            mode: DaemonHandoffMode::Execute,
        },
    ]);
    let answers = client_session(
        &directory.0.join("prefix/splinterm"),
        &directory.socket(),
        ClientRole::TrustedUi,
        &requests,
    );
    assert_eq!(answers.len(), requests.len() + 1, "{answers:?}");
    assert!(
        matches!(
            answers[0],
            Some(ServerFrame::Hello { daemon_generation: generation, .. }) if generation == daemon_generation
        ),
        "the installed client connects as trusted UI: {answers:?}"
    );
    let Some(ServerFrame::Response {
        result: Response::HandoffReport { report },
        ..
    }) = &answers[answers.len() - 2]
    else {
        panic!("no dry-run report: {answers:?}");
    };
    assert_eq!(report.daemon_generation, daemon_generation);
    assert!(!report.rollback_available, "{report:?}");
    // A plain `cargo test` build is unpackaged, so its rollback reason is
    // `NotPackaged`. A package's own `check()` builds the tests with its
    // provenance (`build-local-package.sh` sets `local-committed-head`), so
    // there the spent authority itself is the reason.
    let expected = if report.build.provenance == HandoffBuildProvenance::Unpackaged {
        HandoffBlockingReason::NotPackaged
    } else {
        HandoffBlockingReason::RollbackUnavailable
    };
    assert!(report.blocking_reasons.contains(&expected), "{report:?}");
    let Some(ServerFrame::Error { error, .. }) = &answers[answers.len() - 1] else {
        panic!("Execute was not refused: {answers:?}");
    };
    assert_eq!(error.code, ErrorCode::HandoffUnavailable, "{error:?}");
    assert!(
        error
            .message
            .contains("another live upgrade needs a daemon restart first"),
        "{error:?}"
    );
    answers
}

/// The same daemon process still serves after a refused handoff.
fn assert_same_pid_serves(stage: &mut Stage, directory: &TestDirectory, pid: u32) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (client, _) = connect_adopted(stage, &directory.socket()).await;
        let peer = rustix::net::sockopt::socket_peercred(&client.stream).unwrap();
        assert_eq!(peer.pid.as_raw_nonzero().get(), i32::try_from(pid).unwrap());
    });
}

/// The stand-in Window relaunched into the pinned forward client snapshot
/// in its own process, claimed its resume ticket on the continuation (the
/// daemon checked its kernel credentials, pidfd, and executable), connected
/// as trusted UI because it runs the pinned snapshot, and restored exactly
/// the controller its old connection held at the fence (Plan 0061 M3.16).
fn assert_window_restored_its_controller(stage: &mut Stage, directory: &TestDirectory) {
    let frames: Value =
        serde_json::from_slice(&fs::read(directory.0.join(WINDOW_CLAIM_FRAMES)).unwrap()).unwrap();
    let claim = stage.window_claim();
    let logs = stage.logs();
    assert_eq!(
        claim["grant_bytes"].as_u64(),
        Some(48),
        "no ticket was granted: {claim}\n{logs}"
    );
    let hello: ServerFrame = serde_json::from_value(claim["hello"].clone())
        .unwrap_or_else(|error| panic!("{error}: {claim}\n{logs}"));
    assert!(
        matches!(
            hello,
            ServerFrame::Hello {
                daemon_generation: 2,
                ..
            }
        ),
        "the snapshot connects as trusted UI: {hello:?}\n{logs}"
    );
    let frame: ServerFrame = serde_json::from_value(claim["claim"].clone())
        .unwrap_or_else(|error| panic!("{error}: {claim}\n{logs}"));
    let ServerFrame::Response {
        result: Response::HandoffControlClaimed { controllers },
        ..
    } = frame
    else {
        panic!("the claim was refused: {frame:?}\n{logs}");
    };
    let splint_id: SplintId = serde_json::from_value(frames["splint_id"].clone()).unwrap();
    let incarnation = frames["incarnation"].as_u64().unwrap();
    let [controller] = controllers.as_slice() else {
        panic!("exactly one controller: {controllers:?}\n{logs}");
    };
    assert_eq!(
        (controller.splint_id, controller.incarnation),
        (splint_id, incarnation)
    );
    assert_eq!(controller.modes, [ControlMode::Input, ControlMode::Resize]);
    for expected in [
        "daemon handoff: serving the carried Windows' continuation claims",
        "daemon handoff: issued a carried Window its resume ticket",
        "daemon handoff: restored a relaunched Window's controller",
    ] {
        assert!(logs.contains(expected), "missing {expected:?}\n{logs}");
    }
}

/// Bits of SIGHUP (1), SIGINT (2), and SIGTERM (15) in a `/proc` signal mask:
/// signal `n` is bit `n - 1`.
const HANDOFF_SIGNAL_BITS: u64 = 0x4003;

/// The `SigBlk` mask in a `/proc` status file, or `None` once it is gone.
fn blocked_signals(status: &Path) -> Option<u64> {
    let text = fs::read_to_string(status).ok()?;
    let mask = text.lines().find_map(|line| line.strip_prefix("SigBlk:"))?;
    u64::from_str_radix(mask.trim(), 16).ok()
}

/// No thread of the serving daemon holds SIGINT, SIGTERM, or SIGHUP
/// blocked: the mask the handoff exec carried was released before any
/// runtime thread existed, so none inherited it.
fn assert_daemon_threads_unblocked(pid: u32) {
    for task in fs::read_dir(format!("/proc/{pid}/task")).unwrap() {
        let status = task.unwrap().path().join("status");
        if let Some(mask) = blocked_signals(&status) {
            assert_eq!(
                mask & HANDOFF_SIGNAL_BITS,
                0,
                "{} blocks a handoff signal: SigBlk {mask:016x}",
                status.display()
            );
        }
    }
}

/// A Splint launched now, through the serving daemon, starts with none of
/// SIGINT, SIGTERM, or SIGHUP blocked. Returns its process.
async fn assert_new_splint_unblocked(client: &mut Client, pid: u32, record: &StageRecord) -> u32 {
    let Response::Topology { snapshot } = client.request(Request::InspectTopology).await else {
        panic!("no Topology response");
    };
    let created = client
        .request(Request::CreateLair {
            expected_topology_revision: snapshot.revision,
            name: "after-handoff".to_owned(),
            launch: LaunchParameters {
                cwd: PathBuf::from("/tmp"),
                command: vec![
                    "/bin/sh".to_owned(),
                    "-c".to_owned(),
                    "exec sleep 3600".to_owned(),
                ],
                shell: None,
                login_shell: false,
                scrollback_lines: 100,
            },
            generated_name: false,
        })
        .await;
    assert!(
        matches!(created, Response::LairCreated { .. }),
        "{created:?}"
    );
    // The daemon's one child that is not a carried Splint, once its shell
    // has become `sleep`.
    let deadline = Instant::now() + WAIT;
    let launched = loop {
        let launched = children_of(pid).into_iter().find(|child| {
            record
                .carried
                .iter()
                .all(|splint| splint.child_pid != *child)
                && fs::read_to_string(format!("/proc/{child}/comm"))
                    .is_ok_and(|comm| comm.trim() == "sleep")
        });
        if let Some(launched) = launched {
            break launched;
        }
        assert!(Instant::now() < deadline, "the new Splint never ran");
        time::sleep(Duration::from_millis(20)).await;
    };
    let mask = blocked_signals(Path::new(&format!("/proc/{launched}/status")))
        .expect("the new Splint's status");
    assert_eq!(
        mask & HANDOFF_SIGNAL_BITS,
        0,
        "a Splint launched after the handoff blocks a handoff signal: SigBlk {mask:016x}"
    );
    launched
}

/// The daemon serving `client` is the stage process itself, under the same
/// launcher, now executing the sealed forward snapshot with its activation
/// flag.
fn assert_same_process(client: &Client, stage: &Stage, pid: u32) {
    let peer = rustix::net::sockopt::socket_peercred(&client.stream).unwrap();
    assert_eq!(peer.pid.as_raw_nonzero().get(), i32::try_from(pid).unwrap());
    let executable = fs::read_link(format!("/proc/{pid}/exe")).unwrap();
    assert!(
        executable.to_string_lossy().starts_with("/memfd:"),
        "the adopted daemon runs from its sealed snapshot: {}",
        executable.display()
    );
    let command_line = fs::read(format!("/proc/{pid}/cmdline")).unwrap();
    assert!(
        String::from_utf8_lossy(&command_line).contains(splinterd::CANDIDATE_ACTIVATION_ARGUMENT),
        "{}",
        String::from_utf8_lossy(&command_line)
    );
    assert_eq!(
        proc_stat(pid).map(|(_, ppid)| ppid),
        Some(stage.launcher.id())
    );
}

/// The excluded Splint's zombie was reaped by the forward daemon, and its
/// real exit is recorded live and persisted; every Splint keeps its
/// incarnation in the durable Topology.
async fn assert_exits_recorded(
    client: &mut Client,
    directory: &TestDirectory,
    record: &StageRecord,
) {
    let excluded = record.excluded;
    assert!(
        proc_stat(excluded.child_pid).is_none_or(|(_, ppid)| ppid != record.pid),
        "the excluded Splint's zombie was not reaped"
    );
    let Response::Topology { snapshot } = client.request(Request::InspectTopology).await else {
        panic!("no Topology response");
    };
    let live = |splint_id| snapshot.topology.find_splint(splint_id).unwrap().state;
    assert_eq!(live(excluded.splint_id), SplintState::Exited(EXCLUDED_EXIT));
    for splint in &record.carried {
        assert_eq!(live(splint.splint_id), SplintState::Running);
    }
    let persisted = persisted_topology(directory);
    assert_eq!(persisted.revision().get(), record.topology_revision);
    assert_eq!(
        persisted.find_splint(excluded.splint_id).unwrap().state,
        SplintState::Exited(EXCLUDED_EXIT)
    );
    for splint in record.carried.iter().chain([&excluded]) {
        assert_eq!(
            persisted
                .find_splint(splint.splint_id)
                .unwrap()
                .last_incarnation,
            Some(splint.incarnation)
        );
    }
}

/// Both inherited listeners accept. The image listener closes a connection
/// whose token it cannot claim; one it never accepted would stay open in the
/// backlog instead.
async fn assert_listeners_accept(directory: &TestDirectory, daemon_generation: u64) {
    let mut image = UnixStream::connect(image_content_socket_path(&directory.socket()))
        .await
        .unwrap();
    image.write_all(&[0; 32]).await.unwrap();
    let mut rest = Vec::new();
    let closed = time::timeout(WAIT, image.read_to_end(&mut rest)).await;
    assert!(
        matches!(closed, Ok(Ok(0) | Err(_))),
        "the image listener never accepted: {closed:?}"
    );
    let (_second, generation) = Client::hello(&directory.socket())
        .await
        .expect("the control listener accepts a new connection");
    assert_eq!(generation, daemon_generation);
}

/// The one log line containing `marker`.
fn log_line<'a>(logs: &'a str, marker: &str) -> &'a str {
    let mut lines = logs.lines().filter(|line| line.contains(marker));
    let line = lines
        .next()
        .unwrap_or_else(|| panic!("no {marker:?} line\n{logs}"));
    assert!(
        lines.next().is_none(),
        "more than one {marker:?} line\n{logs}"
    );
    line
}

/// Every `closed_unexpected=` the logs report is zero: neither exec leaked
/// an unmanifested descriptor into the next image.
fn assert_no_stray_descriptors(logs: &str) {
    const FIELD: &str = "closed_unexpected=";
    for (index, _) in logs.match_indices(FIELD) {
        let value = logs[index + FIELD.len()..]
            .split(|character: char| !character.is_ascii_digit())
            .next()
            .unwrap_or_default();
        assert_eq!(value, "0", "a stray descriptor crossed exec\n{logs}");
    }
}

/// The checks every rollback makes: the same process, now executing the
/// sealed rollback daemon snapshot, serves the original generation 1; every
/// carried Splint kept its process (still this daemon's child) and its
/// incarnation, shows its captured screen, and echoes; the excluded
/// Splint's zombie was reaped and its exit recorded; both listeners accept;
/// exactly one rollback exec happened and only the rollback generation
/// committed.
fn assert_rolled_back(
    stage: &mut Stage,
    directory: &TestDirectory,
    record: &StageRecord,
    install_pair_mismatch: bool,
    stray_descriptors: bool,
) -> u32 {
    let pid = record.pid;
    assert_eq!(children_of(stage.launcher.id()), [pid]);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let launched = runtime.block_on(async {
        let (mut client, daemon_generation) = connect_adopted(stage, &directory.socket()).await;
        assert_eq!(
            daemon_generation, record.source_daemon_generation,
            "rollback serves the original generation"
        );
        assert_same_process(&client, stage, pid);
        for splint in &record.carried {
            assert!(
                is_live_child(splint.child_pid, pid),
                "carried Splint {} lost its process in rollback",
                splint.splint_id
            );
            let Response::Splint { runtime, .. } = client
                .request(Request::InspectSplint {
                    splint_id: splint.splint_id,
                })
                .await
            else {
                panic!("no Splint response");
            };
            assert_eq!(runtime.live_incarnation, Some(splint.incarnation));
            client
                .wait_for_text(splint.splint_id, splint.incarnation, "ready")
                .await;
            client
                .assert_body_carried(splint.splint_id, splint.incarnation)
                .await;
            client
                .echo(splint.splint_id, splint.incarnation, "echo-after-rollback")
                .await;
        }
        assert_checkpoints_released(pid);
        client.assert_observations_body_free(record).await;
        assert_exits_recorded(&mut client, directory, record).await;
        assert_listeners_accept(directory, daemon_generation).await;
        assert_daemon_threads_unblocked(pid);
        assert_new_splint_unblocked(&mut client, pid, record).await
    });

    let logs = stage.logs();
    for expected in [
        "daemon handoff candidate failed before its adoption commit",
        "rollback_edge=true",
        "rolling back to the previous generation with the unchanged descriptors",
        "daemon handoff: adopted generation published",
    ] {
        assert!(logs.contains(expected), "missing {expected:?}\n{logs}");
    }
    log_line(
        &logs,
        "daemon handoff: executing the sealed rollback daemon",
    );
    let committed = log_line(&logs, "daemon handoff: adoption committed");
    for expected in [
        "daemon_generation=1",
        "source_daemon_generation=2",
        "rollback=true",
        "carried=2",
        "excluded=1",
        "excluded_exit_unknown=0",
    ] {
        assert!(
            committed.contains(expected),
            "missing {expected:?} in {committed:?}\n{logs}"
        );
    }
    assert_eq!(
        logs.contains("install_pair_mismatch=true"),
        install_pair_mismatch,
        "{logs}"
    );
    if stray_descriptors {
        // Reported at the rollback exec, closed by the rollback daemon's claim.
        assert!(
            logs.contains("unexpected inheritable descriptors cross the rollback exec"),
            "{logs}"
        );
        assert!(
            committed.contains("closed_unexpected=1"),
            "{committed:?}\n{logs}"
        );
    } else {
        assert_no_stray_descriptors(&logs);
    }
    launched
}

/// An ordered shutdown of the serving daemon ends it and its Splints.
fn shut_down(stage: &mut Stage, record: &StageRecord, launched: &[u32]) {
    signal(record.pid, Signal::TERM);
    let status = stage.wait(WAIT * 3);
    assert!(status.success(), "{status}\n{}", stage.logs());
    for splint in &record.carried {
        wait_until_exited(splint.child_pid, "carried Splint");
    }
    for launched in launched {
        wait_until_exited(*launched, "Splint launched after the handoff");
    }
}

/// The forward client is gone from the prefix when the forward daemon
/// adopts, so adoption refuses before its commit and the candidate rolls
/// back (Plan 0061 M3.11a): the rollback daemon adopts the unchanged PTYs
/// and only reports the changed install pair.
fn rollback_case() {
    let directory = TestDirectory::new();
    let mut stage = Stage::launch(
        &directory,
        StageOptions {
            remove_forward_client: true,
            window: true,
            ..StageOptions::default()
        },
    );
    let record = stage.record();
    let launched = assert_rolled_back(&mut stage, &directory, &record, true, false);
    // The failed forward candidate sent nothing; the rollback daemon sent
    // `RolledBack` naming the generation it serves again, after its commit.
    assert_eq!(
        stage.window_outcome(),
        HandoffOutcome {
            kind: HandoffOutcomeKind::RolledBack,
            daemon_generation: record.source_daemon_generation,
        }
    );
    let committed = log_line(&stage.logs(), "daemon handoff: adoption committed").to_owned();
    for expected in ["windows_announced=1", "held_controllers=1"] {
        assert!(
            committed.contains(expected),
            "missing {expected:?} in {committed:?}"
        );
    }
    let logs = stage.logs();
    for expected in [
        // The forward exec happened: the forward daemon claimed and staged.
        "daemon handoff candidate: inherited descriptors claimed",
        "every inherited descriptor and checkpoint verified",
        // It refused at adoption and said why.
        "phase=\"adoption\"",
        "the install prefix does not hold the forward pair the manifest recorded",
        // The rollback daemon adopts despite the changed pair.
        "installed pair changed since handoff preparation; rollback adopts anyway",
        "the installed pair changed since preparation",
    ] {
        assert!(logs.contains(expected), "missing {expected:?}\n{logs}");
    }
    assert!(!logs.contains("rollback failed"), "{logs}");
    shut_down(&mut stage, &record, &[launched]);
    assert_no_checkpoint_artifact(&directory);
}

/// The old generation recorded a duplicate of its listener, at a distinct
/// number, as the Window's continuation end. The forward candidate refuses
/// it at staging and rolls back. The rollback generation refuses the same
/// descriptor and drops the Window: the refused duplicate is no authority,
/// so it is not an alias of the listener, and every carried Splint is
/// served (review of #167).
fn rollback_dropped_window_case() {
    let directory = TestDirectory::new();
    let mut stage = Stage::launch(
        &directory,
        StageOptions {
            window: true,
            alias_window_continuation: true,
            ..StageOptions::default()
        },
    );
    let record = stage.record();
    let launched = assert_rolled_back(&mut stage, &directory, &record, false, false);
    let logs = stage.logs();
    let committed = log_line(&logs, "daemon handoff: adoption committed");
    for expected in [
        "windows_announced=0",
        "windows_exited=1",
        "windows_unsent=0",
        "held_controllers=0",
    ] {
        assert!(
            committed.contains(expected),
            "missing {expected:?} in {committed:?}\n{logs}"
        );
    }
    for expected in [
        // The forward candidate refused the duplicate at staging.
        "phase=\"staging\"",
        "candidate descriptor for Continuation(0) has the wrong kernel object type",
        // The rollback generation dropped the Window instead.
        "daemon handoff rollback: dropping a carried Window it cannot accept",
    ] {
        assert!(logs.contains(expected), "missing {expected:?}\n{logs}");
    }
    assert!(!logs.contains("aliases another role"), "{logs}");
    assert!(!logs.contains("rollback failed"), "{logs}");
    shut_down(&mut stage, &record, &[launched]);
    assert_no_checkpoint_artifact(&directory);
}

/// Plan 0061 M3.17: the rollback daemon refuses continuation claims and,
/// like the adopted daemon, a second handoff (D8). The forward candidate
/// fails at startup with the forward client still installed, so the
/// rollback daemon trusts that installed client. The carried Window reads
/// `RolledBack` and claims anyway: the rollback daemon pins no snapshot, so
/// it sends no ticket (`NoSnapshot`) and closes the channel. That refusal is
/// the proof. No ticket exists in a rollback generation, so the redemption
/// below presents a made-up all-zero ticket and only sanity-checks that it
/// is invalid. `Execute` is refused until a restart.
fn rollback_refuses_claims_case() {
    let directory = TestDirectory::new();
    let mut stage = Stage::launch(
        &directory,
        StageOptions {
            window: true,
            window_claim_after_rollback: true,
            fault: Some(format!("{}@forward", CandidateFaultPoint::Startup.name())),
            ..StageOptions::default()
        },
    );
    let record = stage.record();
    let launched = assert_rolled_back(&mut stage, &directory, &record, false, false);
    assert_eq!(
        stage.window_outcome(),
        HandoffOutcome {
            kind: HandoffOutcomeKind::RolledBack,
            daemon_generation: record.source_daemon_generation,
        }
    );
    let claimed = stage.window_record(WINDOW_ROLLBACK_CLAIM, STAGE_WAIT);
    let logs = stage.logs();
    assert!(
        claimed["grant_bytes"].as_i64() == Some(0) || claimed.get("error").is_some(),
        "the rollback daemon answered a claim: {claimed}\n{logs}"
    );
    for expected in [
        "phase=\"startup\"",
        "injected candidate fault at startup",
        "daemon handoff: refused a carried Window's continuation claim",
        "NoSnapshot",
    ] {
        assert!(logs.contains(expected), "missing {expected:?}\n{logs}");
    }
    assert!(
        !logs.contains("issued a carried Window its resume ticket"),
        "{logs}"
    );
    let frames: Value =
        serde_json::from_slice(&fs::read(directory.0.join(WINDOW_CLAIM_FRAMES)).unwrap()).unwrap();
    // A made-up ticket: the rollback generation issued none (sanity check).
    let presented = Request::ClaimHandoffControl {
        ticket: splinterm_protocol::HandoffResumeTicket::new(
            [0; splinterm_protocol::HANDOFF_RESUME_TICKET_BYTES],
        ),
        splint: Some(splinterm_protocol::HandoffClaimTarget {
            splint_id: serde_json::from_value(frames["splint_id"].clone()).unwrap(),
            incarnation: frames["incarnation"].as_u64().unwrap(),
        }),
    };
    let answers =
        assert_refuses_another_handoff(&directory, record.source_daemon_generation, &[presented]);
    assert!(
        matches!(
            &answers[1],
            Some(ServerFrame::Error { error, .. }) if error.code == ErrorCode::InvalidHandoffTicket
        ),
        "{answers:?}"
    );
    assert_same_pid_serves(&mut stage, &directory, record.pid);
    shut_down(&mut stage, &record, &[launched]);
    assert_no_checkpoint_artifact(&directory);
}

/// Plan 0061 M3.17: the resume hold expires. The relaunched Window claims
/// its ticket at once (a real grant), then waits past the 30 s hold before
/// redeeming it over trusted UI (it still runs the pinned snapshot). The
/// hold ends with the Window and its placeholder still held, and the real
/// ticket is then invalid: nothing is restored.
fn hold_expiry_case() {
    let directory = TestDirectory::new();
    let mut stage = Stage::launch(
        &directory,
        StageOptions {
            window: true,
            window_redeem_delay: Some(RESUME_HOLD_SECONDS + 3),
            ..StageOptions::default()
        },
    );
    let record = stage.record();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (client, daemon_generation) = connect_adopted(&mut stage, &directory.socket()).await;
        assert_eq!(daemon_generation, record.source_daemon_generation + 1);
        assert_same_process(&client, &stage, record.pid);
    });
    assert_eq!(
        stage.window_outcome(),
        HandoffOutcome {
            kind: HandoffOutcomeKind::Adopted,
            daemon_generation: record.source_daemon_generation + 1,
        }
    );
    let claim = stage.window_record(
        WINDOW_CLAIM,
        STAGE_WAIT + Duration::from_secs(RESUME_HOLD_SECONDS),
    );
    let logs = stage.logs();
    assert_eq!(
        claim["grant_bytes"].as_u64(),
        Some(48),
        "the Window was not granted its ticket within the hold: {claim}\n{logs}"
    );
    let hello: ServerFrame = serde_json::from_value(claim["hello"].clone()).unwrap();
    assert!(
        matches!(
            hello,
            ServerFrame::Hello {
                daemon_generation: 2,
                ..
            }
        ),
        "the pinned snapshot is still trusted UI: {hello:?}\n{logs}"
    );
    let frame: ServerFrame = serde_json::from_value(claim["claim"].clone()).unwrap();
    assert!(
        matches!(
            &frame,
            ServerFrame::Error { error, .. } if error.code == ErrorCode::InvalidHandoffTicket
        ),
        "the real ticket redeemed after the hold: {frame:?}\n{logs}"
    );
    let ended = log_line(
        &logs,
        "daemon handoff: the carried Windows' resume hold ended",
    );
    for expected in ["windows=1", "placeholders=1"] {
        assert!(
            ended.contains(expected),
            "missing {expected:?} in {ended:?}\n{logs}"
        );
    }
    for expected in [
        "daemon handoff: issued a carried Window its resume ticket",
        "daemon handoff: refused a handoff control claim",
    ] {
        assert!(logs.contains(expected), "missing {expected:?}\n{logs}");
    }
    assert!(
        !logs.contains("restored a relaunched Window's controller"),
        "{logs}"
    );
    shut_down(&mut stage, &record, &[]);
    assert_no_checkpoint_artifact(&directory);
}

/// The rollback daemon's own adoption fails too. A rollback generation has
/// no rollback edge: it exits loudly and nothing is executed again.
fn rollback_of_rollback_case() {
    let directory = TestDirectory::new();
    let mut stage = Stage::launch(
        &directory,
        StageOptions {
            remove_forward_client: true,
            fault: Some(format!("{}@rollback", CandidateFaultPoint::Startup.name())),
            ..StageOptions::default()
        },
    );
    let record = stage.record();
    let status = stage.wait(STAGE_WAIT);
    let logs = stage.logs();
    assert!(!status.success(), "the refused rollback exited 0\n{logs}");
    for expected in [
        "the install prefix does not hold the forward pair the manifest recorded",
        "rollback_edge=true",
        "injected candidate fault at startup",
        "rollback_edge=false",
        "this daemon is itself a rollback generation, so a failure before its commit has no rollback edge",
        "live-process continuity is not guaranteed",
    ] {
        assert!(logs.contains(expected), "missing {expected:?}\n{logs}");
    }
    // Exactly one rollback exec: the failed rollback generation executed
    // nothing, and nothing committed.
    log_line(
        &logs,
        "daemon handoff: executing the sealed rollback daemon",
    );
    assert!(!logs.contains("adoption committed"), "{logs}");
    // Exiting closed the carried masters, which hung up each carried shell.
    for splint in &record.carried {
        wait_until_exited(splint.child_pid, "carried Splint after a refused rollback");
    }
    // Neither generation committed, so the durable Topology is exactly what
    // the old generation persisted.
    assert_eq!(
        fs::read(directory.0.join("state/splinterm/topology.json")).unwrap(),
        fs::read(directory.0.join(STAGED_TOPOLOGY)).unwrap()
    );
    assert_no_checkpoint_artifact(&directory);
}

/// A SIGTERM pending across the handoff exec (as `systemctl stop` in the
/// exec window) must not kill the candidate: the exec carries it blocked,
/// the candidate unblocks it once its handler exists, and the adopted
/// daemon then shuts down in order. With `rollback`, the forward candidate
/// fails after its handler took the signal, and the rollback executor makes
/// it pending again across the rollback exec, so the rollback daemon shuts
/// down in order once it adopted.
fn pending_sigterm_case(rollback: bool) {
    let directory = TestDirectory::new();
    let mut stage = Stage::launch(
        &directory,
        StageOptions {
            remove_forward_client: rollback,
            pending_sigterm: true,
            ..StageOptions::default()
        },
    );
    let record = stage.record();
    let status = stage.wait(STAGE_WAIT);
    let logs = stage.logs();
    assert!(
        status.success(),
        "a pending SIGTERM must become an ordered shutdown, not kill the candidate: {status}\n{logs}"
    );
    let committed = log_line(&logs, "daemon handoff: adoption committed");
    let generation = if rollback {
        "daemon_generation=1"
    } else {
        "daemon_generation=2"
    };
    assert!(committed.contains(generation), "{committed:?}\n{logs}");
    assert!(
        logs.contains("daemon handoff: adopted generation published"),
        "{logs}"
    );
    if rollback {
        log_line(
            &logs,
            "daemon handoff: executing the sealed rollback daemon",
        );
        assert!(logs.contains("terminate: true"), "{logs}");
    } else {
        assert!(
            !logs.contains("executing the sealed rollback daemon"),
            "{logs}"
        );
    }
    for splint in &record.carried {
        wait_until_exited(splint.child_pid, "carried Splint after an ordered shutdown");
    }
    assert_no_checkpoint_artifact(&directory);
}

fn pending_sigterm_cases() {
    pending_sigterm_case(false);
    println!("handoff_exec_adoption: pending-sigterm (adopt) passed");
    pending_sigterm_case(true);
    println!("handoff_exec_adoption: pending-sigterm (rollback) passed");
}

fn fault_matrix_case() {
    for point in FAULT_POINTS {
        fault_case(point);
        println!(
            "handoff_exec_adoption: fault-matrix {} passed",
            point.name()
        );
    }
}

/// The forward candidate fails at one pre-commit step and rolls back with
/// every Splint alive (Plan 0061 M3.11a fault matrix).
fn fault_case(point: CandidateFaultPoint) {
    let directory = TestDirectory::new();
    let mut stage = Stage::launch(
        &directory,
        StageOptions {
            fault: Some(format!("{}@forward", point.name())),
            ..StageOptions::default()
        },
    );
    let record = stage.record();
    let launched = assert_rolled_back(&mut stage, &directory, &record, false, false);
    let (phase, cause) = match point {
        CandidateFaultPoint::Staging => ("staging", "injected candidate fault at staging"),
        CandidateFaultPoint::RuntimeEntry => ("runtime entry", "candidate runtime entry refused"),
        CandidateFaultPoint::Startup => ("startup", "injected candidate fault at startup"),
        CandidateFaultPoint::ListenerRegistration => (
            "adoption",
            "injected candidate fault at listener registration",
        ),
        CandidateFaultPoint::ScopeAdoption => {
            ("adoption", "injected candidate fault at scope adoption")
        }
        CandidateFaultPoint::ActorStaging => {
            ("adoption", "injected candidate fault at actor staging")
        }
        CandidateFaultPoint::StrayDescriptor
        | CandidateFaultPoint::TerminateDuringTeardown
        | CandidateFaultPoint::GuardTimeout => {
            unreachable!("not a fault point of the matrix")
        }
    };
    let logs = stage.logs();
    for expected in [format!("phase=\"{phase}\""), cause.to_owned()] {
        assert!(logs.contains(&expected), "missing {expected:?}\n{logs}");
    }
    shut_down(&mut stage, &record, &[launched]);
    assert_no_checkpoint_artifact(&directory);
}

/// The manifests record a wrong digest for the rollback daemon snapshot.
/// Forward staging refuses it, and the rollback executor, which verifies the
/// image it would run whatever staging got to, refuses to execute it: the
/// candidate exits loudly with no exec at all (Plan 0061 M3.11a review).
fn tampered_rollback_image_case() {
    let directory = TestDirectory::new();
    let mut stage = Stage::launch(
        &directory,
        StageOptions {
            tamper_rollback_image: true,
            ..StageOptions::default()
        },
    );
    let record = stage.record();
    let status = stage.wait(STAGE_WAIT);
    let logs = stage.logs();
    assert!(!status.success(), "the refused candidate exited 0\n{logs}");
    for expected in [
        "phase=\"staging\"",
        "candidate immutable descriptor does not match manifest authority",
        "the inherited rollback daemon image is not the one the rollback manifest records",
        "live-process continuity is not guaranteed",
    ] {
        assert!(logs.contains(expected), "missing {expected:?}\n{logs}");
    }
    assert!(
        !logs.contains("executing the sealed rollback daemon"),
        "{logs}"
    );
    for splint in &record.carried {
        wait_until_exited(splint.child_pid, "carried Splint after a refused rollback");
    }
    assert_eq!(
        fs::read(directory.0.join("state/splinterm/topology.json")).unwrap(),
        fs::read(directory.0.join(STAGED_TOPOLOGY)).unwrap()
    );
    assert_no_checkpoint_artifact(&directory);
}

/// The failed forward candidate holds an inheritable descriptor no manifest
/// names (as a leaked one would be). The rollback exec reports it instead of
/// refusing, the rollback daemon's own claim closes it, and rollback keeps
/// every Splint (Plan 0061 M3.11a review).
fn stray_descriptor_case() {
    let directory = TestDirectory::new();
    let mut stage = Stage::launch(
        &directory,
        StageOptions {
            remove_forward_client: true,
            fault: Some(format!(
                "{}@forward",
                CandidateFaultPoint::StrayDescriptor.name()
            )),
            ..StageOptions::default()
        },
    );
    let record = stage.record();
    let launched = assert_rolled_back(&mut stage, &directory, &record, true, true);
    let logs = stage.logs();
    assert!(
        logs.contains("test seam: left an inheritable descriptor open"),
        "{logs}"
    );
    shut_down(&mut stage, &record, &[launched]);
    assert_no_checkpoint_artifact(&directory);
}

/// A SIGTERM arrives during rollback teardown: after the forward
/// candidate decided to roll back and blocked the handoff signals on its
/// main thread, but while its runtime's workers still run with them
/// unblocked, so a worker's handler may take it. Captured after teardown or
/// still kernel-pending, it crosses the rollback exec, and the rollback
/// daemon performs an ordered shutdown once it adopted instead of serving
/// until a SIGKILL (Plan 0061 M3.11a review).
fn terminate_during_teardown_case() {
    let directory = TestDirectory::new();
    let mut stage = Stage::launch(
        &directory,
        StageOptions {
            remove_forward_client: true,
            fault: Some(format!(
                "{}@forward",
                CandidateFaultPoint::TerminateDuringTeardown.name()
            )),
            ..StageOptions::default()
        },
    );
    let record = stage.record();
    let status = stage.wait(STAGE_WAIT);
    let logs = stage.logs();
    assert!(
        status.success(),
        "a SIGTERM during rollback teardown must become an ordered shutdown: {status}\n{logs}"
    );
    for expected in [
        "test seam: sent SIGTERM to this process during rollback teardown",
        "daemon handoff: adopted generation published",
    ] {
        assert!(logs.contains(expected), "missing {expected:?}\n{logs}");
    }
    log_line(
        &logs,
        "daemon handoff: executing the sealed rollback daemon",
    );
    let committed = log_line(&logs, "daemon handoff: adoption committed");
    assert!(
        committed.contains("daemon_generation=1") && committed.contains("rollback=true"),
        "{committed:?}\n{logs}"
    );
    for splint in &record.carried {
        wait_until_exited(splint.child_pid, "carried Splint after an ordered shutdown");
    }
    assert_no_checkpoint_artifact(&directory);
}

/// Plan 0061 M3.17, the rollback guard timeout: the failed forward candidate
/// still runs a thread that outlives its runtime, so the rollback executor
/// never reaches one thread. It refuses to execute the rollback daemon (an
/// exec could lose a termination that thread's handler took) and the
/// candidate exits loudly: every carried Splint is hung up, and nothing
/// committed, so the durable Topology is the old generation's.
fn rollback_guard_timeout_case() {
    let directory = TestDirectory::new();
    let mut stage = Stage::launch(
        &directory,
        StageOptions {
            fault: Some(format!(
                "{}@forward",
                CandidateFaultPoint::GuardTimeout.name()
            )),
            ..StageOptions::default()
        },
    );
    let record = stage.record();
    let status = stage.wait(STAGE_WAIT);
    let logs = stage.logs();
    assert!(!status.success(), "the refused rollback exited 0\n{logs}");
    for expected in [
        "test seam: started a thread that outlives the runtime",
        "phase=\"startup\"",
        "injected candidate fault: a thread outlives the runtime",
        "rollback_edge=true",
        "the process did not reach one thread",
        "daemon handoff: no rollback; the candidate exits",
        "live-process continuity is not guaranteed",
    ] {
        assert!(logs.contains(expected), "missing {expected:?}\n{logs}");
    }
    assert!(
        !logs.contains("executing the sealed rollback daemon"),
        "{logs}"
    );
    assert!(!logs.contains("adoption committed"), "{logs}");
    for splint in &record.carried {
        wait_until_exited(splint.child_pid, "carried Splint after a refused rollback");
    }
    assert_eq!(
        fs::read(directory.0.join("state/splinterm/topology.json")).unwrap(),
        fs::read(directory.0.join(STAGED_TOPOLOGY)).unwrap()
    );
    assert_no_checkpoint_artifact(&directory);
}

/// Plan 0037 M5: a forward candidate is killed (`SIGKILL`, as systemd's
/// final stop escalation) after its injected startup failure (the
/// guard-timeout seam, whose thread outlives the runtime), while it still
/// holds every carried checkpoint, which the test asserts before the kill:
/// each carried terminal body is still in an inherited sealed memfd. The
/// kill lands somewhere in the failed candidate's teardown, not necessarily
/// inside its wait for one thread. It leaves no checkpoint-named artifact and
/// no file holding a terminal body anywhere the test owns: the anonymous
/// checkpoint memory goes with the process. Nothing committed, so the
/// durable Topology is the old generation's.
fn killed_hung_candidate_case() {
    let directory = TestDirectory::new();
    let mut stage = Stage::launch(
        &directory,
        StageOptions {
            fault: Some(format!(
                "{}@forward",
                CandidateFaultPoint::GuardTimeout.name()
            )),
            ..StageOptions::default()
        },
    );
    let record = stage.record();
    stage.wait_for_log("test seam: started a thread that outlives the runtime");
    // Failed, and still holding exactly the carried checkpoints as
    // anonymous memfds.
    assert_eq!(
        checkpoint_descriptors(record.pid),
        record.carried.len(),
        "the hung candidate does not hold every carried checkpoint\n{}",
        stage.logs()
    );
    signal(record.pid, Signal::KILL);
    let status = stage.wait(WAIT);
    let logs = stage.logs();
    assert!(!status.success(), "the killed candidate exited 0\n{logs}");
    assert!(!logs.contains("adoption committed"), "{logs}");
    assert!(
        !logs.contains("executing the sealed rollback daemon"),
        "{logs}"
    );
    // The kill closed the carried masters, which hung up each carried shell.
    for splint in &record.carried {
        wait_until_exited(splint.child_pid, "carried Splint after a killed candidate");
    }
    assert_eq!(
        fs::read(directory.0.join("state/splinterm/topology.json")).unwrap(),
        fs::read(directory.0.join(STAGED_TOPOLOGY)).unwrap()
    );
    assert_no_checkpoint_artifact(&directory);
}

/// Plan 0061 M3.17, the old side's full signal carry in a real daemon
/// process (`stop_runtime_and_cross`, untested end to end since M3.11b).
///
/// A real `splinterd`, copied into a private prefix beside a `python3` copy
/// as its installed client, runs the old-side drill: a trusted-UI `Execute`
/// takes the binary's whole attempt (fence, capture, manifests, serve's
/// exit for the boundary) with a test preparation that holds no forward
/// daemon. During the runtime stop the drill sends the process a SIGTERM,
/// after serve stopped listening for one. The old side waits for one
/// thread, captures the termination its handler took, makes it pending
/// again under the block, and crosses the boundary. Live handoff is
/// supported, but with no forward daemon to arm the boundary is not armed
/// (`ExecNotWired`); the resumed round then receives the termination and
/// shuts down in order, exactly once.
fn old_side_signal_carry_case() {
    let directory = TestDirectory::new();
    let prefix = directory.0.join("prefix");
    fs::create_dir(&prefix).unwrap();
    let daemon = prefix.join("splinterd");
    install(&daemon, &fs::read(DAEMON).unwrap());
    install(&prefix.join("splinterm"), &forward_client());
    install(
        &prefix.join("splinterm-pty-child"),
        &fs::read(pty_helper()).unwrap(),
    );
    let mut stage = Stage::launch_program(
        &directory,
        StageOptions::default(),
        &daemon,
        &[(OLD_SIDE_DRILL_ENV, "1")],
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (_, daemon_generation) = connect_adopted(&mut stage, &directory.socket()).await;
        assert_eq!(daemon_generation, 1);
    });
    let answers = client_session(
        &prefix.join("splinterm"),
        &directory.socket(),
        ClientRole::TrustedUi,
        &[Request::PrepareDaemonHandoff {
            mode: DaemonHandoffMode::Execute,
        }],
    );
    // The attempt reached exec readiness, so serve closed the connection
    // instead of answering.
    assert!(
        matches!(answers.as_slice(), [Some(ServerFrame::Hello { .. }), None]),
        "{answers:?}\n{}",
        stage.logs()
    );
    let status = stage.wait(STAGE_WAIT);
    let logs = stage.logs();
    assert!(
        status.success(),
        "the carried SIGTERM must become an ordered shutdown of the resumed round: {status}\n{logs}"
    );
    log_line(
        &logs,
        "test seam: driving an unarmed handoff attempt to the exec boundary",
    );
    log_line(
        &logs,
        "test seam: sent SIGTERM to this process during the runtime stop",
    );
    let carried = log_line(
        &logs,
        "daemon handoff: carrying taken termination signals across the exec boundary",
    );
    assert!(
        carried.contains("terminate=true") && carried.contains("interrupt=false"),
        "{carried:?}\n{logs}"
    );
    // Live handoff is enabled, so the binary's support check allows arming;
    // the exec stays unarmed only because the drill's preparation holds no
    // forward daemon.
    assert!(
        !logs.contains("live handoff is unsupported"),
        "live handoff is supported for this build: {logs}"
    );
    let resumed = log_line(
        &logs,
        "daemon handoff did not exec; resuming the running generation",
    );
    assert!(
        resumed.contains("exec of the forward daemon is not armed for this attempt"),
        "the boundary was crossed and returned unarmed: {resumed:?}\n{logs}"
    );
    // Served twice (fresh, then resumed), and the resumed round ended on the
    // carried termination.
    let ready = logs
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("splinterd ready"))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let resumed_at = logs
        .lines()
        .position(|line| line.contains("resuming the running generation"))
        .unwrap();
    assert!(
        matches!(ready.as_slice(), [fresh, again] if *fresh < resumed_at && resumed_at < *again),
        "{ready:?}\n{logs}"
    );
    assert!(!socket_exists(&directory.socket()), "{logs}");
}

/// AGENTS.md §4.4, the operator decision of 2026-10-20, reproducing the VM
/// acceptance failure on 8728e59b: `pacman -U` replaced `/usr/bin/splinterm`
/// under a running daemon, and every open Window's next trusted request was
/// refused ("trusted consent is required"), so both Windows exited.
///
/// A real `splinterd` serves from a private prefix whose installed client
/// is a `python3` copy. One trusted-UI connection from that client makes a
/// trusted request (a handoff dry run, which both the authorization table
/// and its handler gate on the trusted client), pauses while a
/// byte-identical copy is renamed over the installed client (a new inode;
/// a hard link keeps the old one). Meanwhile another process executing the
/// replaced inode is refused trusted UI at `Hello`. The session then makes
/// the request again on the same connection, and the same process opens a
/// second trusted-UI connection, as a Window opening a pane does: both must
/// succeed. Afterwards a new connection from the newly installed client is
/// served.
fn replaced_client_keeps_its_connection_case() {
    use std::os::unix::fs::MetadataExt as _;

    let directory = TestDirectory::new();
    let prefix = directory.0.join("prefix");
    fs::create_dir(&prefix).unwrap();
    let daemon = prefix.join("splinterd");
    install(&daemon, &fs::read(DAEMON).unwrap());
    let client = prefix.join("splinterm");
    install(&client, &forward_client());
    install(
        &prefix.join("splinterm-pty-child"),
        &fs::read(pty_helper()).unwrap(),
    );
    let mut stage = Stage::launch_program(&directory, StageOptions::default(), &daemon, &[]);
    let socket = directory.socket();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (_, daemon_generation) = connect_adopted(&mut stage, &socket).await;
        assert_eq!(daemon_generation, 1);
    });
    let dry_run = Request::PrepareDaemonHandoff {
        mode: DaemonHandoffMode::DryRun,
    };
    let reported = |answer: &Option<ServerFrame>| {
        matches!(
            answer,
            Some(ServerFrame::Response {
                result: Response::HandoffReport { .. },
                ..
            })
        )
    };
    let stale_client = directory.0.join(STALE_CLIENT);
    let identity = |path: &Path| {
        let metadata = fs::metadata(path).unwrap();
        (metadata.dev(), metadata.ino())
    };
    let answers = paused_client_session(
        &client,
        &socket,
        ClientRole::TrustedUi,
        &[dry_run.clone(), dry_run.clone()],
        1,
        Some(&[dry_run.clone()]),
        |before| {
            assert!(
                matches!(before, [Some(ServerFrame::Hello { .. }), answer] if reported(answer)),
                "the installed client was not served before the upgrade: {before:?}\n{}",
                stage.logs()
            );
            // The upgrade: a byte-identical copy renamed over the client.
            fs::hard_link(&client, &stale_client).unwrap();
            install(&client, &fs::read(&client).unwrap());
            assert_ne!(identity(&client), identity(&stale_client));
            assert_eq!(fs::read(&stale_client).unwrap(), fs::read(&client).unwrap());
            // Another process on the replaced inode, while the session holds
            // its trusted connection open.
            let refused = client_session(&stale_client, &socket, ClientRole::TrustedUi, &[]);
            assert!(
                matches!(
                    refused.as_slice(),
                    [Some(ServerFrame::Error { error, .. })] if error.code == ErrorCode::Unauthorized
                ),
                "another process on the replaced inode was not refused: {refused:?}\n{}",
                stage.logs()
            );
        },
    );
    assert!(
        matches!(
            answers.as_slice(),
            [
                Some(ServerFrame::Hello { .. }),
                before,
                after,
                Some(ServerFrame::Hello { .. }),
                second,
            ] if reported(before) && reported(after) && reported(second)
        ),
        "the Window process lost trust when its client was replaced: {answers:?}\n{}",
        stage.logs()
    );
    let served = client_session(&client, &socket, ClientRole::TrustedUi, &[dry_run]);
    assert!(
        matches!(served.as_slice(), [Some(ServerFrame::Hello { .. }), answer] if reported(answer)),
        "the newly installed client was not served: {served:?}\n{}",
        stage.logs()
    );
}

fn socket_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}
