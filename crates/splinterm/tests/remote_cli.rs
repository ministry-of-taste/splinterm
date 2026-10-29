use std::{
    env, fs,
    os::unix::fs::{PermissionsExt as _, symlink},
    path::PathBuf,
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_splinterm"))
}

fn test_directory(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = env::temp_dir().join(format!(
        "splinterm-remote-cli-{label}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(path.join(".ssh")).unwrap();
    path
}

fn output(command: &mut Command) -> Output {
    command.output().unwrap()
}

fn install_fake_ssh(directory: &std::path::Path) {
    let path = directory.join("ssh");
    fs::write(&path, include_str!("fixtures/fake_ssh.py")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn remote_list_and_inspect_are_strict_local_and_credential_free() {
    let home = test_directory("valid");
    let profiles = home.join("remotes.toml");
    fs::write(home.join(".ssh/id_ed25519"), "do-not-print-this-key").unwrap();
    fs::write(home.join(".ssh/known_hosts"), "wintermute fixture-key").unwrap();
    fs::write(
        &profiles,
        r#"version = 1
[remotes.wintermute]
host = "wintermute"
user = "operator"
port = 2222
identity_files = ["~/.ssh/id_ed25519"]
known_hosts_file = "~/.ssh/known_hosts"
"#,
    )
    .unwrap();

    let listed = output(
        binary()
            .env("HOME", &home)
            .env("SPLINTERM_REMOTES", &profiles)
            .env("PATH", "")
            .args(["remote", "list"]),
    );
    assert!(listed.status.success(), "{:?}", listed.stderr);
    assert_eq!(
        String::from_utf8(listed.stdout).unwrap(),
        "Remote profiles (1)\n  wintermute            wintermute  user=operator  port=2222\n"
    );

    let inspected = output(
        binary()
            .env("HOME", &home)
            .env("SPLINTERM_REMOTES", &profiles)
            .env("PATH", "")
            .args(["remote", "inspect", "wintermute"]),
    );
    assert!(inspected.status.success(), "{:?}", inspected.stderr);
    let stdout = String::from_utf8(inspected.stdout).unwrap();
    assert!(stdout.contains("StrictHostKeyChecking=yes"));
    assert!(stdout.contains("ClearAllForwardings=yes"));
    assert!(stdout.contains("/usr/bin/splinterm relay --graphical-stdio"));
    assert!(!stdout.contains("do-not-print-this-key"));

    fs::remove_dir_all(home).unwrap();
}

#[test]
fn remote_check_uses_configured_nixos_executable_and_only_read_only_probes() {
    let home = test_directory("check");
    let profiles = home.join("remotes.toml");
    fs::write(
        &profiles,
        "version = 1\n[remotes.test]\nhost = \"example.invalid\"\nexecutable = \"/run/current-system/sw/bin/splinterm\"\n",
    )
    .unwrap();
    install_fake_ssh(&home);
    let checked = output(
        binary()
            .env("HOME", &home)
            .env("SPLINTERM_REMOTES", &profiles)
            .env("PATH", &home)
            .env_remove("SSH_ASKPASS")
            // No agent, so no `ssh -G` query reaches the fake.
            .env_remove("SSH_AUTH_SOCK")
            .env("XDG_RUNTIME_DIR", &home)
            .args(["remote", "check", "test"]),
    );
    assert!(checked.status.success(), "{:?}", checked.stderr);
    assert_eq!(
        String::from_utf8(checked.stdout).unwrap(),
        "Remote test is reachable (0 Lairs).\n"
    );
    assert_eq!(fs::read_to_string(home.join("count")).unwrap(), "1");
    let arguments: Vec<String> =
        serde_json::from_slice(&fs::read(home.join("argv.json")).unwrap()).unwrap();
    assert_eq!(
        arguments.last().unwrap(),
        "/run/current-system/sw/bin/splinterm relay --graphical-stdio"
    );
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn global_remote_routes_authenticated_reads_and_surfaces_daemon_denials() {
    for (label, command, mode, succeeds) in [
        ("authenticated", "ping", "read-only", true),
        ("read-only", "list", "read-only", true),
        ("denied", "list", "denied", false),
    ] {
        let home = test_directory(label);
        let profiles = home.join("remotes.toml");
        fs::write(
            &profiles,
            "version = 1\n[remotes.test]\nhost = \"example.invalid\"\n",
        )
        .unwrap();
        install_fake_ssh(&home);
        let result = output(
            binary()
                .env("HOME", &home)
                .env("SPLINTERM_REMOTES", &profiles)
                .env("SPLINTERM_FAKE_SSH_MODE", mode)
                .env("PATH", &home)
                .env_remove("SSH_ASKPASS")
                .env_remove("SSH_AUTH_SOCK")
                .env("XDG_RUNTIME_DIR", &home)
                .args(["--remote", "test", command]),
        );
        assert_eq!(result.status.success(), succeeds, "{:?}", result.stderr);
        assert_eq!(fs::read_to_string(home.join("count")).unwrap(), "1");
        let requests = fs::read_to_string(home.join("requests.jsonl")).unwrap();
        assert!(requests.contains(if command == "ping" {
            "\"type\":\"ping\""
        } else {
            "\"type\":\"list_lairs\""
        }));
        if !succeeds {
            assert!(
                String::from_utf8(result.stderr)
                    .unwrap()
                    .contains("fixture daemon denied topology read")
            );
        }
        fs::remove_dir_all(home).unwrap();
    }
}

#[test]
fn dangling_default_profile_path_is_not_silently_treated_as_absent() {
    let home = test_directory("dangling");
    let config_home = home.join("config");
    let splinterm_config = config_home.join("splinterm");
    fs::create_dir_all(&splinterm_config).unwrap();
    symlink(
        splinterm_config.join("missing.toml"),
        splinterm_config.join("remotes.toml"),
    )
    .unwrap();

    let rejected = output(
        binary()
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", &config_home)
            .env_remove("SPLINTERM_REMOTES")
            .env("PATH", "")
            .args(["remote", "list"]),
    );
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    let stderr = String::from_utf8(rejected.stderr).unwrap();
    assert!(stderr.contains("read remote profiles"));
    fs::remove_dir_all(home).unwrap();
}

#[test]
fn malformed_remote_profiles_fail_before_any_process_launch() {
    let home = test_directory("invalid");
    let profiles = home.join("remotes.toml");
    fs::write(
        &profiles,
        "version = 1\n[remotes.bad]\nhost = \"-oProxyCommand=bad\"\n",
    )
    .unwrap();
    let rejected = output(
        binary()
            .env("HOME", &home)
            .env("SPLINTERM_REMOTES", &profiles)
            .env("PATH", "")
            .args(["remote", "list"]),
    );
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    let stderr = String::from_utf8(rejected.stderr).unwrap();
    assert!(stderr.contains("SSH host or alias"));
    fs::remove_dir_all(home).unwrap();
}
