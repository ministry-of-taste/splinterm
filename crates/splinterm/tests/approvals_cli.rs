//! `splinterm approvals` answers only from a terminal: without one it
//! refuses before it connects to any daemon.

use std::{
    env, fs,
    path::PathBuf,
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

fn runtime_directory() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = env::temp_dir().join(format!(
        "splinterm-approvals-cli-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&path).unwrap();
    path
}

fn approvals(arguments: &[&str]) -> std::process::Output {
    let runtime = runtime_directory();
    let output = Command::new(env!("CARGO_BIN_EXE_splinterm"))
        .args(arguments)
        // No daemon socket here: a refusal must come before any connection.
        .env("XDG_RUNTIME_DIR", &runtime)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    fs::remove_dir_all(&runtime).unwrap();
    output
}

#[test]
fn approvals_refuses_without_a_terminal_before_connecting() {
    let output = approvals(&["approvals"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("splinterm approvals answers only from a terminal"),
        "{stderr}"
    );
    assert!(!stderr.contains("splinterd"), "it connected: {stderr}");
    assert!(output.stdout.is_empty());
}

#[test]
fn approvals_has_no_machine_or_remote_mode() {
    for arguments in [
        &["--output", "json", "approvals"][..],
        &["--remote", "work", "approvals"][..],
        &["approvals", "--yes"][..],
    ] {
        let output = approvals(arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}: {output:?}");
    }
}
