use std::{
    env, fs,
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    process::{Command, Output},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use splinterm_core::{
    Dojo, DojoId, Lair, LairId, LayoutNode, Splint, SplintId, SplintState, Topology,
    TopologyRevision,
};
use splinterm_protocol::{
    ClientFrame, ClientRole, PresetLayoutLaunch, PresetPaneIdentity, Request, Response,
    ServerFrame, ServerLimits, SplintLifecycle, SplintRuntimeSummary, TopologySnapshot,
    encode_frame,
};

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_splinterm"))
}

fn test_directory() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = env::temp_dir().join(format!(
        "splinterm-preset-cli-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&path).unwrap();
    path
}

fn output(command: &mut Command) -> Output {
    command.output().unwrap()
}

fn read_frame(stream: &mut UnixStream) -> ClientFrame {
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length).unwrap();
    let mut body = vec![0_u8; u32::from_be_bytes(length) as usize];
    stream.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn send(stream: &mut UnixStream, request_id: u64, result: Response) {
    stream
        .write_all(&encode_frame(&ServerFrame::Response { request_id, result }).unwrap())
        .unwrap();
}

fn runtime(splint_id: SplintId, incarnation: u64) -> SplintRuntimeSummary {
    SplintRuntimeSummary {
        splint_id,
        live_incarnation: Some(incarnation),
        last_incarnation: Some(incarnation),
        restorable: false,
        lifecycle: SplintLifecycle::Running,
        exit_status: None,
        agent_status: None,
        foreground_app: None,
    }
}

fn write_catalog(directory: &Path) -> PathBuf {
    let path = directory.join("presets.toml");
    fs::write(
        &path,
        r#"version = 1
[commands.editor]
kind = "editor-env"
fallback = ["nvim"]
append = ["."]
[commands.review]
kind = "argv"
argv = ["codex", "literal;$HOME", "*.rs"]
[presets.review]
kind = "dojo"
display-name = "Review workspace"
name = "{cwd.basename}"
root = "main"
focus = "editor"
[presets.review.nodes.main]
type = "split"
orientation = "columns"
ratio = 650
first = "editor"
second = "review"
[presets.review.nodes.editor]
type = "pane"
command = "editor"
cwd = "{cwd}"
[presets.review.nodes.review]
type = "pane"
command = "review"
cwd = "{cwd}"
"#,
    )
    .unwrap();
    path
}

#[test]
fn preset_inspection_and_dry_run_are_local_and_side_effect_free() {
    let directory = test_directory();
    let catalog = write_catalog(&directory);
    let config = directory.join("config.ini");
    fs::write(&config, "[presets]\nfile=presets.toml\n").unwrap();

    let checked = output(binary().args(["preset", "check", catalog.to_str().unwrap()]));
    assert!(checked.status.success(), "{:?}", checked.stderr);
    assert_eq!(
        String::from_utf8(checked.stdout).unwrap(),
        "Preset catalog OK\n  Presets  6\n"
    );

    let listed = output(
        binary()
            .env("SPLINTERM_CONFIG", &config)
            .args(["preset", "list"]),
    );
    assert!(listed.status.success(), "{:?}", listed.stderr);
    let stdout = String::from_utf8(listed.stdout).unwrap();
    assert!(stdout.starts_with("Presets\n"));
    assert!(stdout.contains("review"));
    assert!(stdout.contains("2 panes"));

    let shown = output(
        binary()
            .env("SPLINTERM_CONFIG", &config)
            .args(["preset", "show", "review"]),
    );
    assert!(shown.status.success(), "{:?}", shown.stderr);
    let stdout = String::from_utf8(shown.stdout).unwrap();
    assert!(stdout.contains("Display  Review workspace"));
    assert!(stdout.contains("Focus    editor"));
    assert!(!stdout.contains("$HOME"));

    let previewed = output(
        binary()
            .env("SPLINTERM_CONFIG", &config)
            .env_remove("EDITOR")
            .args([
                "preset",
                "run",
                "review",
                "--cwd",
                directory.to_str().unwrap(),
                "--dry-run",
            ]),
    );
    assert!(previewed.status.success(), "{:?}", previewed.stderr);
    let stdout = String::from_utf8(previewed.stdout).unwrap();
    assert!(stdout.contains("columns 650/350"));
    assert!(stdout.contains("Panes    2"));
    assert!(stdout.contains("no daemon connection or topology mutation"));
    assert!(!stdout.contains("codex"));

    let unavailable = output(
        binary()
            .env("SPLINTERM_CONFIG", &config)
            .env("SPLINTERM_SOCKET", directory.join("missing.sock"))
            .args(["preset", "run", "review", "--no-open"]),
    );
    assert!(!unavailable.status.success());
    assert!(unavailable.stdout.is_empty());
    let stderr = String::from_utf8(unavailable.stderr).unwrap();
    assert!(!stderr.contains("atomic Milestone 6 protocol"));
    assert!(stderr.contains("connect"), "{stderr}");

    fs::remove_dir_all(directory).unwrap();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one fake-daemon scenario proves exact context, wire tree, stable mapping, and final reconciliation"
)]
fn preset_run_sends_one_atomic_tree_and_reconciles_stable_mappings() {
    let directory = test_directory();
    write_catalog(&directory);
    let config = directory.join("config.ini");
    fs::write(&config, "[presets]\nfile=presets.toml\n").unwrap();
    let socket = directory.join("daemon.sock");
    let listener = UnixListener::bind(&socket).unwrap();

    let lair_id = LairId::new();
    let invoking_dojo_id = DojoId::new();
    let invoking_splint_id = SplintId::new();
    let mut invoking = Splint::shell(directory.clone());
    invoking.id = invoking_splint_id;
    invoking.state = SplintState::Running;
    invoking.last_incarnation = Some(1);
    let mut initial = Topology::new();
    initial
        .insert_lair_at(
            TopologyRevision::default(),
            Lair {
                id: lair_id,
                name: "main".into(),
                lifetime: splinterm_core::LairLifetime::Persistent,
                retention: splinterm_core::LairRetention::default(),
                provenance: None,
                dojos: vec![Dojo {
                    id: invoking_dojo_id,
                    name: "terminal".into(),
                    default_focus: invoking_splint_id,
                    root: LayoutNode::Leaf(invoking),
                }],
            },
        )
        .unwrap();
    let initial_snapshot = TopologySnapshot {
        revision: initial.revision(),
        topology: initial.clone(),
        runtimes: vec![runtime(invoking_splint_id, 1)],
        presentation_offers: Vec::new(),
    };

    let created_dojo_id = DojoId::new();
    let editor_id = SplintId::new();
    let review_id = SplintId::new();
    let make_splint = |id: SplintId, title: &str, command: Vec<String>, incarnation| Splint {
        id,
        title: title.into(),
        cwd: directory.clone(),
        command,
        launch: Box::new(splinterm_core::SplintLaunchMetadata::default()),
        last_incarnation: Some(incarnation),
        state: SplintState::Running,
        title_set: true,
    };
    let created = Dojo {
        id: created_dojo_id,
        name: directory
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        default_focus: editor_id,
        root: LayoutNode::Branch {
            axis: splinterm_core::Axis::Horizontal,
            ratio: splinterm_core::SplitRatio::new(650).unwrap(),
            first: Box::new(LayoutNode::Leaf(make_splint(
                editor_id,
                "editor",
                vec!["nvim".into(), ".".into()],
                2,
            ))),
            second: Box::new(LayoutNode::Leaf(make_splint(
                review_id,
                "review",
                vec!["codex".into(), "literal;$HOME".into(), "*.rs".into()],
                3,
            ))),
        },
    };
    let mut committed = initial;
    let committed_revision = committed
        .materialize_dojos_at(committed.revision(), lair_id, None, vec![created])
        .unwrap();
    let committed_snapshot = TopologySnapshot {
        revision: committed_revision,
        topology: committed,
        runtimes: vec![
            runtime(invoking_splint_id, 1),
            runtime(editor_id, 2),
            runtime(review_id, 3),
        ],
        presentation_offers: Vec::new(),
    };

    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        assert!(matches!(
            read_frame(&mut stream),
            ClientFrame::Hello {
                role: ClientRole::TrustedUi,
                ..
            }
        ));
        stream
            .write_all(
                &encode_frame(&ServerFrame::Hello {
                    version: splinterm_protocol::PROTOCOL_VERSION,
                    limits: ServerLimits::default(),
                    development_terminal_access: false,
                    daemon_hostname: None,
                    daemon_generation: 1,
                })
                .unwrap(),
            )
            .unwrap();
        assert!(matches!(
            read_frame(&mut stream),
            ClientFrame::Request {
                request_id: 1,
                request: Request::InspectTopology,
                ..
            }
        ));
        send(
            &mut stream,
            1,
            Response::Topology {
                snapshot: initial_snapshot,
            },
        );
        let ClientFrame::Request {
            request_id: 2,
            request:
                Request::MaterializePreset {
                    expected_topology_revision,
                    target,
                    dojos,
                    directory_identities,
                },
            ..
        } = read_frame(&mut stream)
        else {
            panic!("atomic preset request was not sent");
        };
        assert_eq!(expected_topology_revision, TopologyRevision::new(1));
        assert!(matches!(
            target,
            splinterm_protocol::PresetTarget::ExistingLair {
                lair_id: target,
                rename: None,
            } if target == lair_id
        ));
        assert_eq!(dojos.len(), 1);
        assert!(directory_identities.is_empty());
        let PresetLayoutLaunch::Split {
            ratio,
            first,
            second,
            ..
        } = &dojos[0].root
        else {
            panic!("compiled preset root was not a split");
        };
        assert_eq!(ratio.get(), 650);
        let PresetLayoutLaunch::Pane { key, launch, .. } = first.as_ref() else {
            panic!("editor leaf was absent");
        };
        assert_eq!(key, "editor");
        assert_eq!(launch.command, ["nvim", "."]);
        let PresetLayoutLaunch::Pane { key, launch, .. } = second.as_ref() else {
            panic!("review leaf was absent");
        };
        assert_eq!(key, "review");
        assert_eq!(launch.command, ["codex", "literal;$HOME", "*.rs"]);
        send(
            &mut stream,
            2,
            Response::PresetMaterialized {
                lair_id,
                dojo_ids: vec![created_dojo_id],
                panes: vec![
                    PresetPaneIdentity {
                        dojo_id: created_dojo_id,
                        key: "editor".into(),
                        splint_id: editor_id,
                    },
                    PresetPaneIdentity {
                        dojo_id: created_dojo_id,
                        key: "review".into(),
                        splint_id: review_id,
                    },
                ],
                topology_revision: committed_revision,
            },
        );
        assert!(matches!(
            read_frame(&mut stream),
            ClientFrame::Request {
                request_id: 3,
                request: Request::InspectTopology,
                ..
            }
        ));
        send(
            &mut stream,
            3,
            Response::Topology {
                snapshot: committed_snapshot,
            },
        );
    });

    let run = output(
        binary()
            .env("SPLINTERM_CONFIG", &config)
            .env("SPLINTERM_SOCKET", &socket)
            .env("SPLINTERM_LAIR_ID", lair_id.to_string())
            .env("SPLINTERM_DOJO_ID", invoking_dojo_id.to_string())
            .env("SPLINTERM_SPLINT_ID", invoking_splint_id.to_string())
            .env_remove("EDITOR")
            .args([
                "preset",
                "run",
                "review",
                "--cwd",
                directory.to_str().unwrap(),
                "--no-open",
            ]),
    );
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8(run.stdout).unwrap();
    assert!(stdout.contains("Creating Dojo…"));
    assert!(stdout.contains("Materialized 1 Dojo(s) and 2 pane(s)"));
    assert!(!stdout.contains("Dry run OK"));
    assert!(!stdout.contains("literal;$HOME"));
    server.join().unwrap();
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn bundled_omarchy_presets_are_available_and_unrestricted_aliases_are_opt_in() {
    let directory = test_directory();
    let config = directory.join("config.ini");
    fs::write(&config, "").unwrap();

    let listed = output(
        binary()
            .env("SPLINTERM_CONFIG", &config)
            .args(["preset", "list"]),
    );
    assert!(listed.status.success(), "{:?}", listed.stderr);
    let stdout = String::from_utf8(listed.stdout).unwrap();
    for name in [
        "omarchy.t",
        "omarchy.tdl",
        "omarchy.tds",
        "omarchy.tdlm",
        "omarchy.tsl",
    ] {
        assert!(stdout.contains(name), "{name} missing from {stdout}");
    }

    let tdl = output(
        binary()
            .env("SPLINTERM_CONFIG", &config)
            .env_remove("EDITOR")
            .args([
                "preset",
                "run",
                "omarchy.tdl",
                "--cwd",
                directory.to_str().unwrap(),
                "--param",
                "ai=opencode",
                "--dry-run",
            ]),
    );
    assert!(
        tdl.status.success(),
        "{}",
        String::from_utf8_lossy(&tdl.stderr)
    );
    let stdout = String::from_utf8(tdl.stdout).unwrap();
    assert!(stdout.contains("rows 850/150"));
    assert!(stdout.contains("columns 650/350"));
    assert!(stdout.contains("Focus    editor"));
    assert!(stdout.contains("Panes    3"));

    let restricted = output(
        binary()
            .env("SPLINTERM_CONFIG", &config)
            .env_remove("EDITOR")
            .args([
                "preset",
                "run",
                "omarchy.tdl",
                "--cwd",
                directory.to_str().unwrap(),
                "--param",
                "ai=c",
                "--dry-run",
            ]),
    );
    assert!(!restricted.status.success());
    assert!(
        String::from_utf8(restricted.stderr)
            .unwrap()
            .contains("allow-unrestricted-commands=yes")
    );

    let enabled = directory.join("enabled.ini");
    fs::write(&enabled, "[presets]\nallow-unrestricted-commands=yes\n").unwrap();
    let unrestricted = output(
        binary()
            .env("SPLINTERM_CONFIG", &enabled)
            .env_remove("EDITOR")
            .args([
                "preset",
                "run",
                "omarchy.tdl",
                "--cwd",
                directory.to_str().unwrap(),
                "--param",
                "ai=c",
                "--dry-run",
            ]),
    );
    assert!(
        unrestricted.status.success(),
        "{}",
        String::from_utf8_lossy(&unrestricted.stderr)
    );
    assert!(
        !String::from_utf8(unrestricted.stdout)
            .unwrap()
            .contains("--auto")
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn unsafe_editor_is_rejected_without_echoing_its_value() {
    let directory = test_directory();
    write_catalog(&directory);
    let config = directory.join("config.ini");
    fs::write(&config, "[presets]\nfile=presets.toml\n").unwrap();
    let secret = "nvim $VERY_SECRET_VALUE";
    let rejected = output(
        binary()
            .env("SPLINTERM_CONFIG", &config)
            .env("EDITOR", secret)
            .args([
                "preset",
                "run",
                "review",
                "--cwd",
                directory.to_str().unwrap(),
                "--dry-run",
            ]),
    );
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    let stderr = String::from_utf8(rejected.stderr).unwrap();
    assert!(stderr.contains("ShellMetacharacter"));
    assert!(!stderr.contains("VERY_SECRET_VALUE"));
    fs::remove_dir_all(directory).unwrap();
}
