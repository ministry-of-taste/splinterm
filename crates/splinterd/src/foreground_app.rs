//! The name of each Splint's foreground app, held in daemon memory only
//! (Plan 0069).
//!
//! One sampler task reads, every [`SAMPLE_INTERVAL`], which process group
//! holds each live Splint's terminal, through `/proc` and the root's pidfd.
//! A job's name shows once that job has held the terminal for [`HOLD`], so
//! short commands never show, and clears as soon as the shell holds it again.
//!
//! The name is untrusted terminal-derived display data: any process can set
//! its own command line. It is a sanitized basename, never a path or an
//! argument, and only a label. Nothing matches, targets, authorizes, or
//! consents by it; it never reaches audit, diagnostics, or logs; it never
//! bumps the topology revision; and it is neither persisted nor carried
//! through a handoff.

use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::Path,
    sync::{Arc, Mutex as StdMutex, Weak},
    time::{Duration, Instant},
};

use splinterd::RootProcessProbe;
use splinterm_core::SplintId;
use splinterm_protocol::TopologySnapshot;
use tokio::{
    task::JoinHandle,
    time::{self, MissedTickBehavior},
};

use crate::{DaemonState, agent_status::read_proc_stat};

/// How often the sampler reads every live Splint's foreground.
pub(crate) const SAMPLE_INTERVAL: Duration = Duration::from_millis(500);
/// How long a job holds the terminal before its name shows.
pub(crate) const HOLD: Duration = Duration::from_secs(2);
/// The most of `/proc/<pid>/cmdline` read: only the first two arguments
/// matter.
const MAX_CMDLINE_BYTES: u64 = 4096;

/// Shells, whose own name is never shown: one in the foreground shows no
/// name, and one running a script shows the script's.
const SHELLS: &[&str] = &[
    "sh", "ash", "bash", "dash", "ksh", "mksh", "oksh", "yash", "zsh", "csh", "tcsh", "fish", "nu",
    "elvish", "xonsh",
];
/// Interpreters, which show the script they run, else their own name.
const INTERPRETERS: &[&str] = &[
    "perl", "ruby", "node", "nodejs", "lua", "luajit", "php", "tclsh", "Rscript", "pypy", "pypy3",
];

/// What one sample saw holding a Splint's terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Foreground {
    /// The process group led by the process identified by `key` holds the
    /// terminal; `name` is `None` when that process is a shell.
    Holder {
        key: HolderKey,
        name: Option<String>,
    },
    /// Nothing reliable was read; the shown name stays as it was.
    Unknown,
}

/// A group leader's PID and start time, which tell one job from a later one
/// that reuses the PID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HolderKey {
    pub(crate) pid: u32,
    pub(crate) start_time: u64,
}

#[derive(Debug)]
struct Candidate {
    key: HolderKey,
    name: String,
    since: Instant,
}

#[derive(Debug)]
struct Slot {
    incarnation: u64,
    candidate: Option<Candidate>,
    shown: Option<String>,
}

/// At most one shown name per Splint, bound to one incarnation.
#[derive(Debug, Default)]
pub(crate) struct ForegroundAppStore {
    slots: HashMap<SplintId, Slot>,
}

impl ForegroundAppStore {
    /// Applies one sample. A new incarnation starts from nothing.
    pub(crate) fn observe(
        &mut self,
        splint_id: SplintId,
        incarnation: u64,
        sample: Foreground,
        now: Instant,
    ) {
        let slot = self.slots.entry(splint_id).or_insert(Slot {
            incarnation,
            candidate: None,
            shown: None,
        });
        if slot.incarnation != incarnation {
            *slot = Slot {
                incarnation,
                candidate: None,
                shown: None,
            };
        }
        let Foreground::Holder { key, name } = sample else {
            return;
        };
        let Some(name) = name else {
            // The shell holds the terminal again: back to the default at once.
            slot.candidate = None;
            slot.shown = None;
            return;
        };
        // A different job, or the same one under a new name, starts its own
        // hold; what shows now stays until that hold completes.
        if slot
            .candidate
            .as_ref()
            .is_none_or(|candidate| candidate.key != key || candidate.name != name)
        {
            slot.candidate = Some(Candidate {
                key,
                name,
                since: now,
            });
        }
        if let Some(candidate) = &slot.candidate
            && now.saturating_duration_since(candidate.since) >= HOLD
        {
            slot.shown = Some(candidate.name.clone());
        }
    }

    /// Drops every slot whose Splint is not live at the recorded incarnation.
    pub(crate) fn retain_live(&mut self, live: &HashMap<SplintId, u64>) {
        self.slots
            .retain(|splint_id, slot| live.get(splint_id) == Some(&slot.incarnation));
    }

    pub(crate) fn shown(&self, splint_id: SplintId, incarnation: u64) -> Option<&str> {
        self.slots
            .get(&splint_id)
            .filter(|slot| slot.incarnation == incarnation)
            .and_then(|slot| slot.shown.as_deref())
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.slots.len()
    }
}

/// The daemon-owned store the sampler fills and topology snapshots read.
#[derive(Debug, Default)]
pub(crate) struct ForegroundAppService {
    store: StdMutex<ForegroundAppStore>,
}

impl ForegroundAppService {
    fn store(&self) -> std::sync::MutexGuard<'_, ForegroundAppStore> {
        self.store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Prunes names of retired incarnations and fills runtime summaries.
    /// Every topology snapshot passes through here, beside agent status.
    pub(crate) fn attach(&self, snapshot: &mut TopologySnapshot) {
        let live = snapshot
            .runtimes
            .iter()
            .filter_map(|runtime| Some((runtime.splint_id, runtime.live_incarnation?)))
            .collect::<HashMap<_, _>>();
        let mut store = self.store();
        store.retain_live(&live);
        for runtime in &mut snapshot.runtimes {
            runtime.foreground_app = runtime.live_incarnation.and_then(|incarnation| {
                store
                    .shown(runtime.splint_id, incarnation)
                    .map(str::to_owned)
            });
        }
    }

    /// Applies one round of samples, which covers every live runtime, so a
    /// Splint missing from it has retired.
    fn apply(&self, samples: Vec<(SplintId, u64, Foreground)>, now: Instant) {
        let live = samples
            .iter()
            .map(|(splint_id, incarnation, _)| (*splint_id, *incarnation))
            .collect::<HashMap<_, _>>();
        let mut store = self.store();
        store.retain_live(&live);
        for (splint_id, incarnation, sample) in samples {
            store.observe(splint_id, incarnation, sample, now);
        }
    }
}

/// Starts the one daemon-wide sampler. It holds the daemon only while it
/// collects probes, and is aborted when the returned handle is dropped.
pub(crate) fn spawn_sampler(state: &Arc<DaemonState>) -> JoinHandle<()> {
    let service = Arc::clone(&state.foreground_apps);
    let default_shell: Arc<str> = Path::new(&splinterm_pty::default_shell())
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .into();
    let state = Arc::downgrade(state);
    tokio::spawn(run_sampler(service, state, default_shell))
}

async fn run_sampler(
    service: Arc<ForegroundAppService>,
    state: Weak<DaemonState>,
    default_shell: Arc<str>,
) {
    let mut ticks = time::interval(SAMPLE_INTERVAL);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        ticks.tick().await;
        let Some(state) = state.upgrade() else {
            break;
        };
        let probes = state
            .runtimes
            .lock()
            .await
            .handles()
            .into_values()
            .map(|handle| {
                (
                    handle.splint_id,
                    handle.incarnation.value(),
                    handle.root_process_probe(),
                )
            })
            .collect::<Vec<_>>();
        drop(state);
        let default_shell = Arc::clone(&default_shell);
        // One blocking task reads every Splint's `/proc` entries.
        let Ok(samples) = tokio::task::spawn_blocking(move || {
            probes
                .into_iter()
                .map(|(splint_id, incarnation, probe)| {
                    let sample = probe.map_or(Foreground::Unknown, |probe| {
                        sample_foreground(&probe, &default_shell)
                    });
                    (splint_id, incarnation, sample)
                })
                .collect::<Vec<_>>()
        })
        .await
        else {
            continue;
        };
        service.apply(samples, Instant::now());
    }
}

/// Names what holds the root's terminal. The root leads its session, so a
/// job in it has the root's PID as its session ID; a group leader that does
/// not, or that is not its own group's leader, is a reused PID.
fn sample_foreground(probe: &RootProcessProbe, default_shell: &str) -> Foreground {
    probe
        .observe(|root| {
            let root_stat = read_proc_stat(root)?;
            let leader = u32::try_from(root_stat.terminal_process_group)
                .ok()
                .filter(|leader| *leader > 0)?;
            let start_time = if leader == root {
                root_stat.start_time
            } else {
                let stat = read_proc_stat(leader)?;
                if stat.process_group != leader || stat.session != root {
                    return None;
                }
                stat.start_time
            };
            let cmdline = read_bounded(&format!("/proc/{leader}/cmdline"))?;
            let name = derive_app_name(
                &cmdline,
                || fs::read(format!("/proc/{leader}/comm")).ok(),
                default_shell,
            );
            // The leader must still be the process the names were read from.
            if read_proc_stat(leader)?.start_time != start_time {
                return None;
            }
            let key = HolderKey {
                pid: leader,
                start_time,
            };
            Some(Foreground::Holder { key, name })
        })
        .unwrap_or(Foreground::Unknown)
}

fn read_bounded(path: &str) -> Option<Vec<u8>> {
    let mut contents = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(MAX_CMDLINE_BYTES)
        .read_to_end(&mut contents)
        .ok()?;
    Some(contents)
}

/// The displayed name for a foreground process: the basename of `argv[0]`
/// (a login shell's leading `-` dropped, a Nix `.X-wrapped` shown as `X`),
/// else of its command name. A shell or interpreter running a script shows
/// the script's basename; a shell without one shows nothing. Basenames only:
/// never a path or an argument.
pub(crate) fn derive_app_name(
    cmdline: &[u8],
    comm: impl FnOnce() -> Option<Vec<u8>>,
    default_shell: &str,
) -> Option<String> {
    let mut arguments = cmdline
        .strip_suffix(b"\0")
        .unwrap_or(cmdline)
        .split(|byte| *byte == 0);
    let program = arguments
        .next()
        .map(program_name)
        .filter(|name| !name.is_empty())
        .map(<[u8]>::to_vec)
        .or_else(|| {
            let comm = comm()?;
            let comm = comm.strip_suffix(b"\n").unwrap_or(&comm);
            Some(program_name(comm).to_vec()).filter(|name| !name.is_empty())
        })?;
    let shell = is_shell(&program, default_shell);
    if shell || is_interpreter(&program) {
        let script = arguments
            .next()
            .filter(|argument| !argument.is_empty() && !argument.starts_with(b"-"))
            .map(basename)
            .map(unwrap_nix_wrapper)
            .filter(|script| !script.is_empty());
        if let Some(script) = script {
            return splinterm_protocol::sanitize_foreground_app(script);
        }
        if shell {
            return None;
        }
    }
    splinterm_protocol::sanitize_foreground_app(&program)
}

fn program_name(argument: &[u8]) -> &[u8] {
    let name = basename(argument);
    unwrap_nix_wrapper(name.strip_prefix(b"-").unwrap_or(name))
}

fn basename(argument: &[u8]) -> &[u8] {
    argument
        .rsplit(|byte| *byte == b'/')
        .next()
        .unwrap_or(argument)
}

/// Nix wraps a program `X` as `.X-wrapped`, possibly more than once.
fn unwrap_nix_wrapper(mut name: &[u8]) -> &[u8] {
    while let Some(inner) = name
        .strip_prefix(b".")
        .and_then(|inner| inner.strip_suffix(b"-wrapped"))
        .filter(|inner| !inner.is_empty())
    {
        name = inner;
    }
    name
}

fn is_shell(name: &[u8], default_shell: &str) -> bool {
    SHELLS.iter().any(|shell| shell.as_bytes() == name)
        || (!default_shell.is_empty() && default_shell.as_bytes() == name)
}

/// `python`, `python3`, `python3.12` and the rest of the closed list.
fn is_interpreter(name: &[u8]) -> bool {
    INTERPRETERS
        .iter()
        .any(|interpreter| interpreter.as_bytes() == name)
        || name.strip_prefix(b"python").is_some_and(|version| {
            version
                .iter()
                .all(|byte| byte.is_ascii_digit() || *byte == b'.')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(cmdline: &[u8]) -> Option<String> {
        derive_app_name(cmdline, || None, "")
    }

    #[test]
    fn app_names_are_basenames_with_scripts_for_shells_and_interpreters() {
        for (cmdline, expected) in [
            (&b"nvim\0src/main.rs\0"[..], Some("nvim")),
            (b"/usr/bin/htop\0", Some("htop")),
            (b"ssh\0host\0", Some("ssh")),
            (b"claude\0", Some("claude")),
            (b"claude", Some("claude")),
            (b"-zsh\0", None),
            (b"/bin/bash\0", None),
            (b"bash\0-c\0sleep 30\0", None),
            (b"bash\0./build.sh\0", Some("build.sh")),
            (b"/bin/sh\0/home/me/bin/deploy\0--fast\0", Some("deploy")),
            (b"python3\0-m\0http.server\0", Some("python3")),
            (b"python3.12\0/srv/tools/report.py\0", Some("report.py")),
            (b"python\0", Some("python")),
            (b"node\0/opt/app/server.js\0", Some("server.js")),
            (b"perl\0-e\0print 1\0", Some("perl")),
            (b"/nix/store/abc-neovim/bin/.nvim-wrapped\0", Some("nvim")),
            (b"/nix/store/x/bin/..nvim-wrapped-wrapped\0", Some("nvim")),
            (
                b".python3.12-wrapped\0/nix/store/y/bin/.tool-wrapped\0",
                Some("tool"),
            ),
            (b".-wrapped\0", Some(".-wrapped")),
            (b"pythonista\0x.py\0", Some("pythonista")),
            (b"bash\0\0", None),
            (b"vim\xff\0", Some("vim\u{fffd}")),
            (b"rlo\xe2\x80\xaevim\0", Some("rlovim")),
        ] {
            assert_eq!(
                name(cmdline).as_deref(),
                expected,
                "{}",
                String::from_utf8_lossy(cmdline)
            );
        }
    }

    #[test]
    fn the_command_name_fills_in_and_the_default_shell_counts_as_a_shell() {
        assert_eq!(
            derive_app_name(b"", || Some(b"top\n".to_vec()), "").as_deref(),
            Some("top")
        );
        assert_eq!(
            derive_app_name(b"\0", || Some(b".nvim-wrapped\n".to_vec()), "").as_deref(),
            Some("nvim")
        );
        assert_eq!(derive_app_name(b"", || None, ""), None);
        assert_eq!(derive_app_name(b"", || Some(b"\n".to_vec()), ""), None);
        // The command name is read only when argv[0] says nothing.
        assert_eq!(
            derive_app_name(b"htop\0", || unreachable!("comm not needed"), "").as_deref(),
            Some("htop")
        );
        assert_eq!(derive_app_name(b"oil\0", || None, "oil"), None);
        assert_eq!(
            derive_app_name(b"oil\0job.oil\0", || None, "oil").as_deref(),
            Some("job.oil")
        );
        assert_eq!(
            derive_app_name(b"oil\0", || None, "").as_deref(),
            Some("oil")
        );
    }

    #[test]
    fn hostile_names_are_sanitized_and_bounded() {
        let long = format!("/{}\0", "x".repeat(200));
        assert_eq!(name(long.as_bytes()), Some("x".repeat(32)));
        assert_eq!(name(b"\x1b[31m\0"), Some("[31m".into()));
        assert_eq!(name(b"\x1b\x07\0"), None);
        assert_eq!(name(b"vim\xe2\x80\x8b\0"), Some("vim".into()));
    }

    fn key(pid: u32) -> HolderKey {
        HolderKey {
            pid,
            start_time: u64::from(pid) * 10,
        }
    }

    fn job(pid: u32, name: &str) -> Foreground {
        Foreground::Holder {
            key: key(pid),
            name: Some(name.into()),
        }
    }

    fn shell() -> Foreground {
        Foreground::Holder {
            key: key(1),
            name: None,
        }
    }

    #[test]
    fn a_job_shows_after_the_hold_and_the_shell_clears_it_at_once() {
        let splint = SplintId::new();
        let start = Instant::now();
        let at = |millis| start + Duration::from_millis(millis);
        let mut store = ForegroundAppStore::default();

        store.observe(splint, 1, shell(), at(0));
        assert_eq!(store.shown(splint, 1), None);
        store.observe(splint, 1, job(50, "nvim"), at(500));
        store.observe(splint, 1, job(50, "nvim"), at(2_000));
        assert_eq!(store.shown(splint, 1), None, "held only 1.5 s");
        store.observe(splint, 1, job(50, "nvim"), at(2_500));
        assert_eq!(store.shown(splint, 1), Some("nvim"));
        // Unknown samples change nothing.
        store.observe(splint, 1, Foreground::Unknown, at(3_000));
        assert_eq!(store.shown(splint, 1), Some("nvim"));
        // The wrong incarnation sees nothing.
        assert_eq!(store.shown(splint, 2), None);

        store.observe(splint, 1, shell(), at(3_500));
        assert_eq!(store.shown(splint, 1), None, "the shell clears at once");
    }

    #[test]
    fn short_jobs_never_show_and_a_new_job_restarts_the_hold() {
        let splint = SplintId::new();
        let start = Instant::now();
        let at = |millis| start + Duration::from_millis(millis);
        let mut store = ForegroundAppStore::default();

        // `ls` held 1.5 s, then the shell: never shown.
        store.observe(splint, 1, job(60, "ls"), at(0));
        store.observe(splint, 1, job(60, "ls"), at(1_500));
        store.observe(splint, 1, shell(), at(2_000));
        store.observe(splint, 1, job(61, "git"), at(2_500));
        store.observe(splint, 1, shell(), at(3_000));
        assert_eq!(store.shown(splint, 1), None);

        store.observe(splint, 1, job(70, "make"), at(4_000));
        store.observe(splint, 1, job(70, "make"), at(6_000));
        assert_eq!(store.shown(splint, 1), Some("make"));
        // A different job (here a reused PID) keeps `make` until it has held.
        let reused = Foreground::Holder {
            key: HolderKey {
                pid: 70,
                start_time: 99,
            },
            name: Some("cc".into()),
        };
        store.observe(splint, 1, reused.clone(), at(6_500));
        assert_eq!(store.shown(splint, 1), Some("make"));
        store.observe(splint, 1, reused, at(8_500));
        assert_eq!(store.shown(splint, 1), Some("cc"));
        // The same leader under a new name (an exec) holds again too.
        store.observe(
            splint,
            1,
            Foreground::Holder {
                key: HolderKey {
                    pid: 70,
                    start_time: 99,
                },
                name: Some("ld".into()),
            },
            at(9_000),
        );
        assert_eq!(store.shown(splint, 1), Some("cc"));
    }

    #[test]
    fn a_new_incarnation_starts_fresh_and_retired_ones_are_dropped() {
        let splint = SplintId::new();
        let other = SplintId::new();
        let start = Instant::now();
        let at = |millis| start + Duration::from_millis(millis);
        let mut store = ForegroundAppStore::default();
        store.observe(splint, 1, job(80, "htop"), at(0));
        store.observe(splint, 1, job(80, "htop"), at(2_000));
        store.observe(other, 4, job(90, "top"), at(0));
        assert_eq!(store.shown(splint, 1), Some("htop"));

        store.observe(splint, 2, job(80, "htop"), at(2_500));
        assert_eq!(store.shown(splint, 2), None, "a relaunch holds again");
        assert_eq!(store.shown(splint, 1), None);

        store.retain_live(&HashMap::from([(splint, 2)]));
        assert_eq!(store.len(), 1);
        store.retain_live(&HashMap::from([(splint, 3)]));
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn the_service_fills_only_live_runtimes_and_prunes_the_rest() {
        use splinterm_protocol::{SplintLifecycle, SplintRuntimeSummary};

        let service = ForegroundAppService::default();
        let live = SplintId::new();
        let exited = SplintId::new();
        let start = Instant::now();
        service.apply(
            vec![(live, 3, job(5, "nvim")), (exited, 7, job(6, "less"))],
            start,
        );
        service.apply(
            vec![(live, 3, job(5, "nvim")), (exited, 7, job(6, "less"))],
            start + HOLD,
        );
        let topology = splinterm_core::Topology::new();
        let runtime = |splint_id, live_incarnation: Option<u64>| SplintRuntimeSummary {
            splint_id,
            live_incarnation,
            last_incarnation: Some(live_incarnation.unwrap_or(7)),
            restorable: live_incarnation.is_none(),
            lifecycle: if live_incarnation.is_some() {
                SplintLifecycle::Running
            } else {
                SplintLifecycle::Exited
            },
            exit_status: None,
            agent_status: None,
            foreground_app: None,
        };
        let mut snapshot = TopologySnapshot {
            revision: topology.revision(),
            topology,
            runtimes: vec![runtime(live, Some(3)), runtime(exited, None)],
        };
        service.attach(&mut snapshot);
        assert_eq!(snapshot.runtimes[0].foreground_app.as_deref(), Some("nvim"));
        assert_eq!(snapshot.runtimes[1].foreground_app, None);
        assert_eq!(service.store().len(), 1, "the exited Splint's name is gone");
    }
}
