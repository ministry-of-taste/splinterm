//! Remote profile choosing and the detached self-launches that open remote
//! Windows from the graphical client.
//!
//! A remote Window runs in its own `splinterm --remote PROFILE` process so the
//! one-endpoint-per-Window rule holds. These launches inherit no stdio and, via
//! [`DETACH_TERMINAL_FLAG`], start a new session without a controlling terminal,
//! so OpenSSH never prompts on whatever terminal started the parent client.

use std::{
    ffi::OsString,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
};

use anyhow::{Context, Result};

use crate::{
    SessionPickerItem,
    remote::{RemoteCatalog, RemoteProfile},
};

/// Shown under the remote chooser's creation row when profiles are listed.
pub const PROFILES_BLOCKER: &str = "Add profiles in remotes.toml";
/// Shown when `remotes.toml` is absent or lists no profiles.
pub const EMPTY_BLOCKER: &str = "No remote profiles · add one to remotes.toml";
/// Shown when `remotes.toml` cannot be read.
pub const INVALID_BLOCKER: &str = "remotes.toml is invalid · run `splinterm remote list`";

/// Formats the OpenSSH destination a profile connects to.
#[must_use]
pub fn destination(profile: &RemoteProfile) -> String {
    let user = profile
        .user()
        .map_or_else(String::new, |user| format!("{user}@"));
    let port = profile
        .port()
        .map_or_else(String::new, |port| format!(":{port}"));
    format!("{user}{}{port}", profile.host())
}

/// A profile row. Rows without panes render without a running status.
fn remote_profile_item(profile: &RemoteProfile) -> SessionPickerItem {
    SessionPickerItem {
        display_title: profile.name().to_owned(),
        breadcrumb: destination(profile),
        working_directory: String::new(),
        pane_count: 0,
        running_pane_count: 0,
        agent: None,
    }
}

/// The remote chooser's profile names, rows, and creation-row explanation,
/// for the standalone launcher and the command palette's pop-up alike.
#[must_use]
pub fn chooser_contents(
    catalog: &Result<RemoteCatalog>,
) -> (Vec<String>, Vec<SessionPickerItem>, &'static str) {
    match catalog {
        Err(_) => (Vec::new(), Vec::new(), INVALID_BLOCKER),
        Ok(catalog) if catalog.is_empty() => (Vec::new(), Vec::new(), EMPTY_BLOCKER),
        Ok(catalog) => (
            catalog
                .iter()
                .map(|profile| profile.name().to_owned())
                .collect(),
            catalog.iter().map(remote_profile_item).collect(),
            PROFILES_BLOCKER,
        ),
    }
}

/// The remote profile a chooser row names.
///
/// # Errors
///
/// Returns an error for a row outside the profile list.
pub fn chosen_profile(names: &[String], index: usize) -> Result<String> {
    names
        .get(index)
        .cloned()
        .context("remote chooser returned an invalid selection")
}

/// Hidden client flag that makes the launched process call `setsid` first.
pub const DETACH_TERMINAL_FLAG: &str = "--detach-terminal";

/// Builds `splinterm --detach-terminal --remote PROFILE`, the remote Window launch.
#[must_use]
pub fn remote_window_command(executable: &Path, profile: &str) -> Command {
    detached_command(
        executable,
        [
            OsString::from(DETACH_TERMINAL_FLAG),
            OsString::from("--remote"),
            OsString::from(profile),
        ],
    )
}

/// Builds `splinterm --detach-terminal remotes`, the standalone remote chooser.
#[must_use]
pub fn remote_chooser_command(executable: &Path) -> Command {
    detached_command(
        executable,
        [
            OsString::from(DETACH_TERMINAL_FLAG),
            OsString::from("remotes"),
        ],
    )
}

/// Do not add `process_group(0)` here: it would make the child a process-group
/// leader, and the `setsid` behind [`DETACH_TERMINAL_FLAG`] would then fail.
fn detached_command(executable: &Path, arguments: impl IntoIterator<Item = OsString>) -> Command {
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// Opens one configured remote in a new Window process.
///
/// The launched client maps a connection-status Window at once, shows
/// categorized failures with Retry / Close, and hands over to the remote's
/// Window when connected; this call returns as soon as the process starts.
///
/// # Errors
///
/// Returns an error when the running client executable cannot be located or
/// the detached process cannot be started.
pub fn open_remote_window(profile: &str) -> Result<()> {
    let executable = std::env::current_exe().context("locate the running Splinterm client")?;
    spawn_detached(remote_window_command(&executable, profile))
        .with_context(|| format!("open remote profile {profile:?} in a new Window"))
}

/// Opens the standalone remote chooser Window.
///
/// # Errors
///
/// Returns an error when the running client executable cannot be located or
/// the detached process cannot be started.
pub fn open_remote_chooser() -> Result<()> {
    let executable = std::env::current_exe().context("locate the running Splinterm client")?;
    spawn_detached(remote_chooser_command(&executable)).context("open the remote chooser")
}

/// Opens one configured remote in a new Window process as this exiting
/// process's final act, leaving the shared last-exit record to that child.
///
/// The chooser exits cleanly right after the launch, while a child that fails
/// immediately (no `ssh`, a missing profile) records its failure as the last
/// client exit; the chooser's clean exit must not overwrite it.
///
/// # Errors
///
/// Returns an error when the detached process cannot be started.
pub fn hand_off_to_remote_window(profile: &str) -> Result<()> {
    open_remote_window(profile)?;
    mark_detached_handoff();
    Ok(())
}

/// Opens the standalone remote chooser as this exiting process's final act; see
/// [`hand_off_to_remote_window`].
///
/// # Errors
///
/// Returns an error when the detached process cannot be started.
pub fn hand_off_to_remote_chooser() -> Result<()> {
    open_remote_chooser()?;
    mark_detached_handoff();
    Ok(())
}

fn mark_detached_handoff() {
    if let Some(diagnostics) = crate::diagnostics::global() {
        diagnostics.mark_detached_handoff();
    }
}

/// Starts `command` and reaps it on a background thread, so a long-lived parent
/// never accumulates zombie children.
///
/// # Errors
///
/// Returns an error when the process cannot be started.
pub fn spawn_detached(mut command: Command) -> Result<()> {
    let child = command
        .spawn()
        .context("start a detached Splinterm process")?;
    reap_in_background(child);
    Ok(())
}

fn reap_in_background(mut child: Child) {
    // If no reaper thread can start, the child is reaped when this client exits.
    let _ = thread::Builder::new()
        .name("splinterm-launch-reaper".to_owned())
        .spawn(move || {
            let _ = child.wait();
        });
}

/// Starts a new session so this process has no controlling terminal.
///
/// Called first by a client started with [`DETACH_TERMINAL_FLAG`]. A failure
/// (for example, already a session or group leader) leaves the process as it
/// was, which is no worse than not detaching.
pub fn detach_from_controlling_terminal() {
    let _ = rustix::process::setsid();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        time::{Duration, Instant},
    };

    fn arguments(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    fn catalog(text: &str) -> Result<RemoteCatalog> {
        RemoteCatalog::parse(text, None)
    }

    const TWO_PROFILES: &str = r#"
version = 1
[remotes.wintermute]
host = "wintermute.example"
user = "case"
port = 2222
[remotes.straylight]
host = "straylight"
"#;

    #[test]
    fn chooser_lists_profiles_in_stable_order_with_destinations() {
        let (names, items, blocker) = chooser_contents(&catalog(TWO_PROFILES));
        assert_eq!(names, ["straylight", "wintermute"]);
        assert_eq!(items[0].display_title, "straylight");
        assert_eq!(items[0].breadcrumb, "straylight");
        assert_eq!(items[1].display_title, "wintermute");
        assert_eq!(items[1].breadcrumb, "case@wintermute.example:2222");
        assert!(items.iter().all(|item| item.pane_count == 0));
        assert_eq!(blocker, PROFILES_BLOCKER);
    }

    #[test]
    fn chooser_explains_missing_empty_and_invalid_profiles() {
        // `load_default` maps an absent default file to an empty catalog.
        let (names, items, blocker) = chooser_contents(&Ok(RemoteCatalog::default()));
        assert!(names.is_empty() && items.is_empty());
        assert_eq!(blocker, EMPTY_BLOCKER);

        let (names, items, blocker) = chooser_contents(&catalog("version = 1\n"));
        assert!(names.is_empty() && items.is_empty());
        assert_eq!(blocker, EMPTY_BLOCKER);

        for invalid in [
            "not toml",
            "version = 2\n",
            "version = 1\n[remotes.bad]\nhost = \"\"\n",
            "version = 1\n[remotes.bad]\nhost = \"x\"\nextra = 1\n",
        ] {
            let (names, items, blocker) = chooser_contents(&catalog(invalid));
            assert!(names.is_empty() && items.is_empty(), "{invalid:?}");
            assert_eq!(blocker, INVALID_BLOCKER, "{invalid:?}");
        }
    }

    #[test]
    fn a_chooser_row_outside_the_profile_list_is_refused() {
        let names = ["straylight".to_owned(), "wintermute".to_owned()];
        assert_eq!(chosen_profile(&names, 1).unwrap(), "wintermute");
        assert!(chosen_profile(&names, 2).is_err());
        assert!(chosen_profile(&[], 0).is_err());
    }

    #[test]
    fn remote_launches_run_the_client_detached_with_exact_argv() {
        let executable = Path::new("/opt/splinterm/bin/splinterm");
        let window = remote_window_command(executable, "wintermute");
        assert_eq!(window.get_program(), executable);
        assert_eq!(
            arguments(&window),
            ["--detach-terminal", "--remote", "wintermute"]
        );
        assert_eq!(window.get_envs().count(), 0, "environment is inherited");
        assert_eq!(window.get_current_dir(), None);

        // A profile name is one argv entry and is never shell-interpreted.
        let odd = remote_window_command(executable, "a b;$(x)");
        assert_eq!(
            arguments(&odd),
            ["--detach-terminal", "--remote", "a b;$(x)"]
        );

        let chooser = remote_chooser_command(executable);
        assert_eq!(chooser.get_program(), executable);
        assert_eq!(arguments(&chooser), ["--detach-terminal", "remotes"]);
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "splinterm-remote-launch-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn detached_launch_inherits_no_stdio() {
        let directory = scratch_dir("stdio");
        let record = directory.join("record");
        let double = directory.join("splinterm");
        // Resolve the inherited descriptors before redirecting the record write.
        let shown = record.display();
        fs::write(
            &double,
            format!(
                "#!/bin/sh\nstdio=$(for fd in 0 1 2; do readlink /proc/$$/fd/$fd; done)\n\
                 {{ printf '%s\\n' \"$@\"; printf '%s\\n' \"$stdio\"; }} > '{shown}.tmp'\n\
                 mv '{shown}.tmp' '{shown}'\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&double, fs::Permissions::from_mode(0o755)).unwrap();

        // Another test thread forking while the double was open for writing can
        // briefly make it busy; retry that transient case only.
        let mut attempts = 0;
        while let Err(error) = spawn_detached(remote_window_command(&double, "wintermute")) {
            let busy = error
                .root_cause()
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::ExecutableFileBusy);
            attempts += 1;
            assert!(busy && attempts < 50, "{error:#}");
            thread::sleep(Duration::from_millis(20));
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while !record.exists() {
            assert!(Instant::now() < deadline, "test double did not run");
            thread::sleep(Duration::from_millis(10));
        }
        let lines = fs::read_to_string(&record).unwrap();
        let lines = lines.lines().collect::<Vec<_>>();
        assert_eq!(
            lines,
            [
                "--detach-terminal",
                "--remote",
                "wintermute",
                "/dev/null",
                "/dev/null",
                "/dev/null"
            ]
        );
        let _ = fs::remove_dir_all(directory);
    }
}
