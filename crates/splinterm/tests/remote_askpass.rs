//! Native askpass fallback end to end: the real `splinterm` binary runs as
//! OpenSSH's askpass helper and answers through a private per-attempt socket.

use std::{
    ffi::OsString,
    fs,
    os::unix::{fs::PermissionsExt as _, process::ExitStatusExt as _},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use splinterm::{
    remote::RemoteCatalog,
    remote_askpass::{
        AskpassBroker, AskpassKind, AskpassReply, NativePrompt, SOCKET_ENV, Secret, TOKEN_ENV,
        Terminator,
    },
    remote_session::{AuthEnvironment, RemoteFailure, RemoteFailureKind, RemoteSession},
};
use splinterm_protocol::{Request, Response};
use tokio::sync::mpsc;

const FAKE_SSH: &str = include_str!("fixtures/fake_ssh.py");
const SECRET: &str = "hunter2-native";
const CLIENT: &str = env!("CARGO_BIN_EXE_splinterm");

fn test_directory(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "splinterm-askpass-{label}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&path).unwrap();
    path
}

/// An OpenSSH stand-in: it records the askpass environment, runs a forced
/// askpass exactly as OpenSSH does (prompt as the only argument), and starts
/// the relay fixture only when the answer matches.
fn fake_ssh(directory: &Path) -> PathBuf {
    fs::write(directory.join("fake_ssh.py"), FAKE_SSH).unwrap();
    let path = directory.join("ssh");
    let source = format!(
        r#"#!/usr/bin/env python3
import json, os, subprocess, sys
base = os.path.dirname(os.path.realpath(sys.argv[0]))
names = ('SSH_ASKPASS', 'SSH_ASKPASS_REQUIRE', '{SOCKET_ENV}', '{TOKEN_ENV}')
with open(os.path.join(base, 'env.json'), 'w', encoding='utf-8') as handle:
    json.dump({{name: os.environ.get(name) for name in names}}, handle)
askpass = os.environ.get('SSH_ASKPASS')
if askpass and os.environ.get('SSH_ASKPASS_REQUIRE') == 'force':
    with open(os.path.join(base, 'ssh-pid'), 'w', encoding='utf-8') as handle:
        handle.write(str(os.getpid()))
    result = subprocess.run([askpass, "case@example.invalid's password: "],
                            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE)
    # Only reached if OpenSSH is still alive when its askpass ends.
    with open(os.path.join(base, 'askpass-returned'), 'w', encoding='utf-8') as handle:
        handle.write(str(result.returncode))
    if result.returncode != 0 or result.stdout != b'{SECRET}\n':
        sys.stderr.write('case@example.invalid: Permission denied (publickey,password).\n')
        sys.exit(255)
else:
    sys.stderr.write('case@example.invalid: Permission denied (publickey).\n')
    sys.exit(255)
os.execv(sys.executable, [sys.executable, os.path.join(base, 'fake_ssh.py')] + sys.argv[1:])
"#
    );
    fs::write(&path, source).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn recorded_environment(directory: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(directory.join("env.json")).unwrap()).unwrap()
}

fn catalog() -> RemoteCatalog {
    RemoteCatalog::parse(
        "version = 1\n[remotes.test]\nhost = \"example.invalid\"\n",
        None,
    )
    .unwrap()
}

fn no_terminal() -> AuthEnvironment {
    AuthEnvironment {
        terminal: false,
        user_askpass: None,
        ..AuthEnvironment::default()
    }
}

async fn connect(
    ssh: &Path,
    environment: &AuthEnvironment,
    prompt: Option<&NativePrompt>,
) -> anyhow::Result<RemoteSession> {
    let catalog = catalog();
    RemoteSession::connect_with_options(
        catalog.get("test").unwrap(),
        ssh.as_os_str(),
        Duration::from_secs(30),
        environment,
        prompt,
    )
    .await
}

fn native_prompt() -> (
    NativePrompt,
    mpsc::Receiver<splinterm::remote_askpass::AskpassRequest>,
) {
    let (requests, prompts) = mpsc::channel(1);
    (NativePrompt::new(PathBuf::from(CLIENT), requests), prompts)
}

#[tokio::test]
async fn the_native_prompt_answers_openssh_through_the_client_binary() {
    let directory = test_directory("answer");
    let ssh = fake_ssh(&directory);
    let (prompt, mut prompts) = native_prompt();
    let answering = tokio::spawn(async move {
        let request = prompts.recv().await.unwrap();
        assert_eq!(request.kind(), AskpassKind::Secret);
        assert_eq!(request.prompt_lines(), ["case@example.invalid's password:"]);
        request.answer(AskpassReply::Submit(
            Secret::from_bytes(SECRET.as_bytes()).unwrap(),
        ));
        prompts
    });
    let session = connect(&ssh, &no_terminal(), Some(&prompt)).await.unwrap();
    let mut prompts = answering.await.unwrap();
    let mut connection = session.connect_interactive().await.unwrap();
    assert!(matches!(
        connection.request(Request::Ping).await.unwrap(),
        Response::Pong
    ));

    let environment = recorded_environment(&directory);
    assert_eq!(environment["SSH_ASKPASS"], CLIENT);
    assert_eq!(environment["SSH_ASKPASS_REQUIRE"], "force");
    let token = environment[TOKEN_ENV].as_str().unwrap();
    assert_eq!(token.len(), 64);
    let socket = PathBuf::from(environment[SOCKET_ENV].as_str().unwrap());
    assert!(
        !socket.exists() && !socket.parent().unwrap().exists(),
        "the private socket and directory end with negotiation"
    );
    assert!(!session.diagnostics().contains(SECRET));
    assert!(prompts.try_recv().is_err(), "exactly one prompt");
    drop(connection);
    drop(session);
    tokio::time::sleep(Duration::from_millis(400)).await;
    fs::remove_dir_all(directory).unwrap();
}

fn assert_openssh_never_read_a_refusal(directory: &Path) {
    assert!(
        !directory.join("askpass-returned").exists(),
        "OpenSSH outlived its askpass and could have sent an empty password"
    );
    let pid = fs::read_to_string(directory.join("ssh-pid")).unwrap();
    let stat = fs::read_to_string(Path::new("/proc").join(pid.trim()).join("stat"));
    assert!(
        stat.map_or(true, |stat| stat.contains(") Z ")),
        "OpenSSH is gone"
    );
}

#[tokio::test]
async fn a_refused_secret_kills_openssh_before_the_helper_exits() {
    let directory = test_directory("refuse");
    let ssh = fake_ssh(&directory);
    let (prompt, mut prompts) = native_prompt();
    let answering = tokio::spawn(async move {
        let request = prompts.recv().await.unwrap();
        // Even an explicit refusal of a secret never reaches OpenSSH.
        request.answer(AskpassReply::Cancel);
    });
    let error = connect(&ssh, &no_terminal(), Some(&prompt))
        .await
        .unwrap_err();
    answering.await.unwrap();
    assert!(error.downcast_ref::<RemoteFailure>().is_some());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_openssh_never_read_a_refusal(&directory);
    fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn dropping_the_attempt_during_a_prompt_kills_openssh_first() {
    let directory = test_directory("drop-attempt");
    let ssh = fake_ssh(&directory);
    let (prompt, mut prompts) = native_prompt();
    let attempt = {
        let ssh = ssh.clone();
        tokio::spawn(async move {
            let _ = connect(&ssh, &no_terminal(), Some(&prompt)).await;
        })
    };
    let request = prompts.recv().await.unwrap();
    // The status Window's Escape, Close, or timeout: the attempt goes first.
    attempt.abort();
    assert!(attempt.await.unwrap_err().is_cancelled());
    drop(request);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_openssh_never_read_a_refusal(&directory);
    fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn the_users_askpass_is_preferred_and_a_terminal_forces_nothing() {
    let directory = test_directory("precedence");
    let ssh = fake_ssh(&directory);
    let user_askpass = directory.join("user-askpass");
    fs::write(&user_askpass, format!("#!/bin/sh\necho '{SECRET}'\n")).unwrap();
    fs::set_permissions(&user_askpass, fs::Permissions::from_mode(0o700)).unwrap();

    let (prompt, mut prompts) = native_prompt();
    let environment = AuthEnvironment {
        terminal: false,
        user_askpass: Some(user_askpass.clone().into_os_string()),
        ..AuthEnvironment::default()
    };
    let session = connect(&ssh, &environment, Some(&prompt)).await.unwrap();
    let recorded = recorded_environment(&directory);
    assert_eq!(recorded["SSH_ASKPASS_REQUIRE"], "force");
    assert!(recorded[SOCKET_ENV].is_null() && recorded[TOKEN_ENV].is_null());
    assert!(
        prompts.try_recv().is_err(),
        "the native prompt was not used"
    );
    drop(session);
    tokio::time::sleep(Duration::from_millis(400)).await;

    let terminal = AuthEnvironment {
        terminal: true,
        user_askpass: Some(user_askpass.into_os_string()),
        ..AuthEnvironment::default()
    };
    let error = connect(&ssh, &terminal, Some(&prompt)).await.unwrap_err();
    let recorded = recorded_environment(&directory);
    assert!(recorded["SSH_ASKPASS_REQUIRE"].is_null());
    assert!(recorded[SOCKET_ENV].is_null() && recorded[TOKEN_ENV].is_null());
    assert_eq!(
        error.downcast_ref::<RemoteFailure>().unwrap().kind(),
        RemoteFailureKind::AuthenticationFailed
    );
    assert!(prompts.try_recv().is_err());

    // Without a terminal, askpass, or native prompt, nothing can ask.
    let error = connect(&ssh, &no_terminal(), None).await.unwrap_err();
    assert!(recorded_environment(&directory)[SOCKET_ENV].is_null());
    assert_eq!(
        error.downcast_ref::<RemoteFailure>().unwrap().kind(),
        RemoteFailureKind::AuthenticationFailed
    );
    fs::remove_dir_all(directory).unwrap();
}

async fn run_helper_binary(
    environment: &[(&'static str, OsString)],
    arguments: &[&str],
) -> std::process::Output {
    tokio::process::Command::new(CLIENT)
        .args(arguments)
        .envs(environment.iter().cloned())
        .env_remove("SSH_ASKPASS_PROMPT")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .unwrap()
}

/// Runs the helper as the child of a shell that stands in for OpenSSH.
async fn run_helper_binary_under_shell(
    environment: &[(&'static str, OsString)],
    arguments: &[&str],
) -> std::process::Output {
    tokio::process::Command::new("sh")
        .args(["-c", "\"$0\" \"$@\"; exit $?", CLIENT])
        .args(arguments)
        .envs(environment.iter().cloned())
        .env_remove("SSH_ASKPASS_PROMPT")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .unwrap()
}

#[tokio::test]
async fn the_client_binary_helper_prints_only_the_answer_and_refuses_bad_tokens() {
    let (prompt, mut prompts) = native_prompt();
    let broker = AskpassBroker::bind(&prompt).unwrap();
    let environment = broker.environment().to_vec();
    // This test process stands in for OpenSSH: it starts the helper directly,
    // so terminations are recorded, never delivered to it.
    let kills = Arc::new(AtomicUsize::new(0));
    let service = broker.serve_with(std::process::id(), Terminator::recording(kills.clone()));

    let answering = tokio::spawn(async move {
        let request = prompts.recv().await.unwrap();
        assert_eq!(request.prompt_lines(), ["Enter PIN for key:"]);
        request.answer(AskpassReply::Submit(
            Secret::from_bytes(SECRET.as_bytes()).unwrap(),
        ));
        prompts
    });
    let output = run_helper_binary(&environment, &["Enter PIN for key: "]).await;
    let mut prompts = answering.await.unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, format!("{SECRET}\n").as_bytes());
    assert!(output.stderr.is_empty(), "the helper logs nothing");

    let mut wrong = environment.clone();
    for (name, value) in &mut wrong {
        if *name == TOKEN_ENV {
            *value = OsString::from("0".repeat(64));
        }
    }
    // A secret the broker never answers fails closed: the helper kills its
    // parent, here a stand-in shell, so OpenSSH could not send it empty.
    let output = run_helper_binary_under_shell(&wrong, &["password:"]).await;
    assert_eq!(output.status.signal(), Some(9));
    assert!(output.stdout.is_empty());
    assert!(prompts.try_recv().is_err(), "a wrong token shows no prompt");

    // Not OpenSSH's calling convention: refused without connecting.
    let output = run_helper_binary(&environment, &["one", "two"]).await;
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(prompts.try_recv().is_err());
    assert_eq!(kills.load(Ordering::SeqCst), 0, "no prompt was refused");
    service.release();
}
