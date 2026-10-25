//! Human-only, bounded local diagnostics discovery.

use std::{
    env, fs,
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
#[cfg(test)]
use splinterm::diagnostics::ExitClass;
use splinterm::diagnostics::{
    DiagnosticEvent, maintain_retention, newest_abnormal_event, read_last_exit,
};
use splinterm_protocol::DaemonDiagnosticEvent;
use uuid::Uuid;

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_PROBE_BYTES: u64 = 256 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
enum ProbeResult {
    Available(String),
    Unavailable,
    TimedOut,
    Malformed,
}

trait SystemAdapter {
    fn run(&self, program: &str, arguments: &[&str]) -> ProbeResult;
}

struct ProcessAdapter {
    timeout: Duration,
}

impl Default for ProcessAdapter {
    fn default() -> Self {
        Self {
            timeout: PROBE_TIMEOUT,
        }
    }
}

impl SystemAdapter for ProcessAdapter {
    fn run(&self, program: &str, arguments: &[&str]) -> ProbeResult {
        let Ok(mut child) = Command::new(program)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            return ProbeResult::Unavailable;
        };
        let deadline = Instant::now() + self.timeout;
        let success = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.success(),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return ProbeResult::TimedOut;
                }
                Err(_) => return ProbeResult::Unavailable,
            }
        };
        if !success {
            return ProbeResult::Unavailable;
        }
        let Some(stdout) = child.stdout.take() else {
            return ProbeResult::Malformed;
        };
        let mut bytes = Vec::new();
        if stdout
            .take(MAX_PROBE_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() as u64 > MAX_PROBE_BYTES
        {
            return ProbeResult::Malformed;
        }
        String::from_utf8(bytes).map_or(ProbeResult::Malformed, ProbeResult::Available)
    }
}

#[derive(Clone, Debug)]
struct CrashEvidence {
    timestamp_unix_ms: u64,
    signal: Option<u32>,
}

pub(super) fn run(last_exit_only: bool, last_crash_only: bool) -> Result<()> {
    maintain_retention().context("failed to maintain diagnostic retention")?;
    run_with_adapter(last_exit_only, last_crash_only, &ProcessAdapter::default())
}

fn run_with_adapter(
    last_exit_only: bool,
    last_crash_only: bool,
    adapter: &dyn SystemAdapter,
) -> Result<()> {
    let last_exit = read_last_exit().context("failed to read last client exit")?;
    if last_exit_only {
        print_client_event("Last client exit", last_exit.as_ref());
        return Ok(());
    }

    let panic = newest_abnormal_event(true).context("failed to inspect retained panic logs")?;
    let client_executable = env::current_exe()
        .ok()
        .and_then(|path| fs::canonicalize(path).ok());
    let coredump = client_executable
        .as_deref()
        .and_then(|path| query_coredump(adapter, path));
    if last_crash_only {
        match newest_crash(panic.as_ref(), coredump.as_ref()) {
            Some(CrashSource::Client(event)) => {
                println!("Last crash: retained client panic");
                print_client_event("Client event", Some(event));
            }
            Some(CrashSource::Systemd(crash)) => {
                println!("Last crash: crash:signal_inferred (external systemd evidence)");
                print_crash(crash);
            }
            None => println!("Last crash: unavailable"),
        }
        return Ok(());
    }

    print_identity("Client executable", client_executable.as_deref());
    let daemon_executable = client_executable
        .as_deref()
        .and_then(Path::parent)
        .map(|directory| directory.join("splinterd"))
        .filter(|path| path.is_file())
        .or_else(|| resolve_path("splinterd"));
    print_identity("Daemon executable", daemon_executable.as_deref());
    println!("Client build: {}", env!("CARGO_PKG_VERSION"));
    println!(
        "Build commit: {}",
        splinterd::SOURCE_COMMIT.unwrap_or("unavailable")
    );
    println!(
        "splinterd.service: {}",
        probe_label(adapter.run("systemctl", &["--user", "is-active", "splinterd.service"]))
    );
    print_client_event("Last client exit", last_exit.as_ref());
    let abnormal =
        newest_abnormal_event(false).context("failed to inspect retained client logs")?;
    print_client_event("Newest abnormal client event", abnormal.as_ref());

    let correlation = abnormal
        .as_ref()
        .or(last_exit.as_ref())
        .map(|event| event.client_instance_id);
    let daemon_events = correlation.map_or_else(Vec::new, |id| query_daemon_events(adapter, id));
    if daemon_events.is_empty() {
        println!("Matching daemon lifecycle events: unavailable");
    } else {
        println!("Matching daemon lifecycle events: {}", daemon_events.len());
        for event in daemon_events.iter().take(8) {
            println!(
                "  {:?}, window {}, topology revision {}",
                event.event,
                event.window_id,
                event.topology_revision.map_or_else(
                    || "unavailable".to_owned(),
                    |revision| revision.get().to_string()
                )
            );
        }
    }
    match coredump.as_ref() {
        Some(crash) => {
            println!("Systemd coredump: present (external evidence)");
            print_crash(crash);
        }
        None => println!("Systemd coredump: unavailable"),
    }
    Ok(())
}

fn print_client_event(label: &str, event: Option<&DiagnosticEvent>) {
    let Some(event) = event else {
        println!("{label}: unavailable");
        return;
    };
    let exit = event
        .exit_class
        .and_then(|exit| serde_json::to_value(exit).ok())
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unavailable".to_owned());
    let failed_at = event
        .failure_location
        .as_deref()
        .map(|location| format!("; failed at {location}"))
        .unwrap_or_default();
    println!(
        "{label}: {exit}{failed_at}; client {}; window {}; topology revision {}",
        event.client_instance_id,
        event
            .window_id
            .map_or_else(|| "unavailable".to_owned(), |id| id.to_string()),
        event.topology_revision.map_or_else(
            || "unavailable".to_owned(),
            |revision| revision.get().to_string()
        )
    );
}

fn print_identity(label: &str, path: Option<&Path>) {
    let Some(path) = path else {
        println!("{label}: unavailable");
        return;
    };
    match fs::metadata(path) {
        Ok(metadata) => println!(
            "{label}: {} (device {}, inode {})",
            path.display(),
            metadata.dev(),
            metadata.ino()
        ),
        Err(_) => println!("{label}: unavailable"),
    }
}

fn resolve_path(program: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|path| {
        env::split_paths(&path)
            .map(|directory| directory.join(program))
            .find(|candidate| candidate.is_file())
            .and_then(|candidate| fs::canonicalize(candidate).ok())
    })
}

fn query_daemon_events(adapter: &dyn SystemAdapter, client_id: Uuid) -> Vec<DaemonDiagnosticEvent> {
    let ProbeResult::Available(output) = adapter.run(
        "journalctl",
        &[
            "--user",
            "-u",
            "splinterd.service",
            "-n",
            "200",
            "--no-pager",
            "-o",
            "json",
        ],
    ) else {
        return Vec::new();
    };
    output
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|record| record.get("MESSAGE")?.as_str().map(str::to_owned))
        .filter_map(|message| serde_json::from_str::<DaemonDiagnosticEvent>(&message).ok())
        .filter(|event| event.client_instance_id == client_id)
        .collect()
}

fn query_coredump(adapter: &dyn SystemAdapter, executable: &Path) -> Option<CrashEvidence> {
    let executable = executable.to_str()?;
    let ProbeResult::Available(output) = adapter.run(
        "coredumpctl",
        &["--no-pager", "--json=short", "-1", executable],
    ) else {
        return None;
    };
    output.lines().find_map(|line| {
        let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
        if value
            .get("COREDUMP_EXE")
            .and_then(serde_json::Value::as_str)
            != Some(executable)
        {
            return None;
        }
        parse_coredump_value(&value)
    })
}

#[cfg(test)]
fn parse_coredump(line: &str) -> Option<CrashEvidence> {
    let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
    parse_coredump_value(&value)
}

fn parse_coredump_value(value: &serde_json::Value) -> Option<CrashEvidence> {
    let timestamp = [
        "_SOURCE_REALTIME_TIMESTAMP",
        "__REALTIME_TIMESTAMP",
        "COREDUMP_TIMESTAMP",
    ]
    .into_iter()
    .find_map(|key| value.get(key).and_then(json_u64))?;
    let signal = value
        .get("COREDUMP_SIGNAL")
        .and_then(json_u64)
        .and_then(|value| u32::try_from(value).ok());
    Some(CrashEvidence {
        timestamp_unix_ms: timestamp / 1_000,
        signal,
    })
}

fn json_u64(value: &serde_json::Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

fn print_crash(crash: &CrashEvidence) {
    println!(
        "  timestamp_unix_ms: {}; signal: {}",
        crash.timestamp_unix_ms,
        crash
            .signal
            .map_or_else(|| "unavailable".to_owned(), |signal| signal.to_string())
    );
}

fn probe_label(result: ProbeResult) -> &'static str {
    match result {
        ProbeResult::Available(output) if output.trim() == "active" => "active",
        ProbeResult::Available(_) => "inactive",
        ProbeResult::Unavailable => "unavailable",
        ProbeResult::TimedOut => "unavailable (timeout)",
        ProbeResult::Malformed => "unavailable (malformed output)",
    }
}

enum CrashSource<'a> {
    Client(&'a DiagnosticEvent),
    Systemd(&'a CrashEvidence),
}

fn newest_crash<'a>(
    panic: Option<&'a DiagnosticEvent>,
    coredump: Option<&'a CrashEvidence>,
) -> Option<CrashSource<'a>> {
    match (panic, coredump) {
        (Some(event), Some(crash)) if event.timestamp_unix_ms >= crash.timestamp_unix_ms => {
            Some(CrashSource::Client(event))
        }
        (Some(_) | None, Some(crash)) => Some(CrashSource::Systemd(crash)),
        (Some(event), None) => Some(CrashSource::Client(event)),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeAdapter(ProbeResult);

    impl SystemAdapter for FakeAdapter {
        fn run(&self, _program: &str, _arguments: &[&str]) -> ProbeResult {
            self.0.clone()
        }
    }

    #[test]
    fn journal_matching_requires_exact_correlation_and_typed_message() {
        let matched_id = "71d82a68-11e8-47c4-9193-dd83f4b03f1a".parse().unwrap();
        let event = DaemonDiagnosticEvent {
            schema_version: 1,
            timestamp_unix_ms: 42,
            component: splinterm_protocol::DaemonDiagnosticComponent::Splinterd,
            event: splinterm_protocol::DaemonDiagnosticEventCode::DojoClosed,
            level: splinterm_protocol::DaemonDiagnosticLevel::Info,
            pid: 7,
            client_instance_id: matched_id,
            window_id: "727b26c3-2b28-4ea2-b94a-2bbfb8ce74f1".parse().unwrap(),
            topology_revision: None,
            build_version: "test".to_owned(),
            build_commit: None,
        };
        let outer = serde_json::json!({ "MESSAGE": serde_json::to_string(&event).unwrap() });
        let unrelated = serde_json::json!({ "MESSAGE": "arbitrary daemon error sentinel" });
        let adapter = FakeAdapter(ProbeResult::Available(format!("{outer}\n{unrelated}\n")));
        assert_eq!(query_daemon_events(&adapter, matched_id), vec![event]);
        assert!(query_daemon_events(&adapter, Uuid::nil()).is_empty());
    }

    #[test]
    fn process_adapter_enforces_timeout() {
        let adapter = ProcessAdapter {
            timeout: Duration::from_millis(20),
        };
        assert_eq!(adapter.run("sh", &["-c", "sleep 1"]), ProbeResult::TimedOut);
    }

    #[test]
    fn unavailable_system_adapters_are_not_errors() {
        let adapter = FakeAdapter(ProbeResult::Unavailable);
        assert!(query_daemon_events(&adapter, Uuid::nil()).is_empty());
        assert!(query_coredump(&adapter, Path::new("/usr/bin/splinterm")).is_none());
    }

    #[test]
    fn coredump_query_requires_exact_executable_identity() {
        let adapter = FakeAdapter(ProbeResult::Available(format!(
            "{}\n{}\n",
            r#"{"COREDUMP_EXE":"/usr/bin/other","_SOURCE_REALTIME_TIMESTAMP":"42000000","COREDUMP_SIGNAL":"11"}"#,
            r#"{"COREDUMP_EXE":"/usr/bin/splinterm","_SOURCE_REALTIME_TIMESTAMP":"43000000","COREDUMP_SIGNAL":"6"}"#
        )));
        let evidence = query_coredump(&adapter, Path::new("/usr/bin/splinterm")).unwrap();
        assert_eq!(evidence.timestamp_unix_ms, 43_000);
        assert_eq!(evidence.signal, Some(6));
    }

    #[test]
    fn coredump_parser_keeps_only_timestamp_and_signal() {
        let evidence = parse_coredump(
            r#"{"_SOURCE_REALTIME_TIMESTAMP":"42000000","COREDUMP_SIGNAL":"11","COREDUMP_ENVIRON":"SECRET=sentinel","COREDUMP_CMDLINE":"sensitive argv"}"#,
        )
        .unwrap();
        assert_eq!(evidence.timestamp_unix_ms, 42_000);
        assert_eq!(evidence.signal, Some(11));
    }

    #[test]
    fn malformed_coredump_is_unavailable() {
        assert!(parse_coredump("not-json").is_none());
        assert!(parse_coredump(r#"{"COREDUMP_SIGNAL":11}"#).is_none());
    }

    #[test]
    fn newest_crash_prefers_the_newest_evidence() {
        let client = DiagnosticEvent {
            schema_version: 1,
            timestamp_unix_ms: 20,
            component: splinterm::diagnostics::DiagnosticComponent::Splinterm,
            module: splinterm::diagnostics::DiagnosticModule::Client,
            event: splinterm::diagnostics::DiagnosticEventCode::Panic,
            level: splinterm::diagnostics::DiagnosticLevel::Error,
            pid: 1,
            client_instance_id: Uuid::nil(),
            window_id: None,
            dojo_id: None,
            splint_id: None,
            topology_revision: None,
            tab_count: None,
            build_version: "test".to_owned(),
            build_commit: None,
            exit_class: Some(ExitClass::Panic),
            error_code: None,
            failure_location: None,
        };
        let external = CrashEvidence {
            timestamp_unix_ms: 10,
            signal: Some(11),
        };
        assert!(matches!(
            newest_crash(Some(&client), Some(&external)),
            Some(CrashSource::Client(_))
        ));
    }

    #[test]
    fn probe_labels_availability_without_exposing_output() {
        assert_eq!(
            probe_label(ProbeResult::Available("active\n".into())),
            "active"
        );
        assert_eq!(probe_label(ProbeResult::TimedOut), "unavailable (timeout)");
        assert_eq!(
            probe_label(ProbeResult::Malformed),
            "unavailable (malformed output)"
        );
    }
}
