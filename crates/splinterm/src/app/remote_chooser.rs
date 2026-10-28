//! The remote launcher (`splinterm remotes`) and its Recent Dojos entry.

use std::sync::mpsc as std_mpsc;

use anyhow::{Context, Result, bail};
use splinterm::{
    SessionPickerDecision, SessionPickerItem, SessionPickerUi, WindowSurface,
    config::AppConfig,
    diagnostics::{DiagnosticErrorCode, DiagnosticEventCode, DiagnosticLevel, global},
    endpoint::ConnectionFactory,
    remote::RemoteCatalog,
    remote_launch::{chooser_contents, chosen_profile},
    session_picker::{SessionEntry, collect_sessions},
};
use splinterm_core::{Dojo, DojoId, Lair};
use splinterm_protocol::{Request, Response};

use super::{
    remote_status::{RemoteConnected, StatusEnd, connect_with_status},
    session_catalog::{recent_dojo_ids, remember_dojo, select_dojo_from, session_picker_item},
    sessions::{launch_new_terminal_on, run_staged_picker},
    window::{
        WhileParked, parked_close, parked_deadline, run_live_multipane_window_with_app_id,
        while_parked,
    },
};

const REMOTE_DOJOS_TITLE: &str = "Remote Dojos…";
/// The Recent Dojos row that opens the remote chooser. It is offered when
/// profiles exist, and also when `remotes.toml` is invalid so the chooser can
/// say so; an absent or empty catalog adds no row.
fn remote_dojos_row_for(catalog: &Result<RemoteCatalog>) -> Option<SessionPickerItem> {
    let breadcrumb = match catalog {
        Err(_) => "Check remotes.toml".to_owned(),
        Ok(catalog) if catalog.is_empty() => return None,
        Ok(catalog) if catalog.iter().len() == 1 => "1 remote profile".to_owned(),
        Ok(catalog) => format!("{} remote profiles", catalog.iter().len()),
    };
    Some(SessionPickerItem {
        display_title: REMOTE_DOJOS_TITLE.to_owned(),
        breadcrumb,
        working_directory: String::new(),
        pane_count: 0,
        running_pane_count: 0,
        agent: None,
    })
}

pub(in crate::app) fn remote_dojos_row() -> Option<SessionPickerItem> {
    remote_dojos_row_for(&RemoteCatalog::load_default())
}

fn report_invalid_catalog(error: &anyhow::Error) {
    if let Some(diagnostics) = global() {
        diagnostics.emit(
            DiagnosticLevel::Warn,
            DiagnosticEventCode::ConfigWarning,
            Some(DiagnosticErrorCode::Config),
        );
    }
    eprintln!("splinterm remote profiles are invalid: {error:#}");
}

/// Runs the remote launcher: one Window lists the profiles, shows the chosen
/// profile's connection status and any SSH prompt, lists that host's Dojos,
/// and then becomes the remote terminal. Escape steps back one stage; Escape
/// at the profile list closes the Window.
pub(in crate::app) async fn run_remotes(config: AppConfig) -> Result<()> {
    run_launcher(&mut WindowLauncher {
        config,
        last_profile: None,
    })
    .await
}

/// How one launcher phase ended.
enum Phase<S, T> {
    /// The compositor or user closed the Window: the launcher ends.
    Closed,
    /// The phase ended with `T` and left the Window parked as `S`.
    Kept(S, T),
}

/// What the remote Dojo menu chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DojoChoice {
    /// Escape: back to the profiles, ending the SSH connection.
    Back,
    /// A new terminal on the remote.
    New,
    /// The Dojo in this row.
    Open(usize),
}

/// How resolving a Dojo menu row ended.
#[derive(Debug, PartialEq, Eq)]
enum Resolved<D> {
    Found(D),
    /// The Dojo is gone; the next menu shows the fresh list.
    Gone,
    /// The parked Window closed while the remote was asked.
    Closed,
}

/// The phases the remote launcher shows in its one Window. [`WindowLauncher`]
/// runs them as Window phases; the tests drive [`run_launcher`] with a fake.
trait LauncherSurface {
    /// A parked Window. Dropping it unmaps the Window.
    type Surface;
    /// A connected remote. Dropping it ends its SSH connection.
    type Remote;
    /// A remote Dojo resolved for opening.
    type Dojo;

    /// Lists the profiles, on `surface` or else on a new Window. `Some` names
    /// the chosen profile; `None` is Escape.
    async fn remote_menu(
        &mut self,
        surface: Option<Self::Surface>,
    ) -> Result<Phase<Self::Surface, Option<String>>>;

    /// Shows the connection status and any SSH prompt while connecting to
    /// `profile`. `None` means a status key ended the attempt.
    async fn connect(
        &mut self,
        surface: Self::Surface,
        profile: &str,
    ) -> Result<Phase<Self::Surface, Option<Self::Remote>>>;

    /// Lists the remote's Dojos, with a New terminal row.
    async fn dojo_menu(
        &mut self,
        surface: Self::Surface,
        remote: &mut Self::Remote,
    ) -> Result<Phase<Self::Surface, DojoChoice>>;

    /// Lists the remote's Lairs again, while `surface` is parked, and resolves
    /// menu row `index`. A close of the parked Window ends the request at once.
    async fn resolve(
        &mut self,
        surface: &Self::Surface,
        remote: &mut Self::Remote,
        index: usize,
    ) -> Result<Resolved<Self::Dojo>>;

    /// Opens the remote terminal on the Window, a new one for `None`. The
    /// terminal binds the Window to the remote for good.
    async fn open_terminal(
        &mut self,
        surface: Self::Surface,
        remote: Self::Remote,
        dojo: Option<Self::Dojo>,
    ) -> Result<()>;
}

/// The launcher's stages: profiles, status, Dojos, then the terminal.
async fn run_launcher<L: LauncherSurface>(launcher: &mut L) -> Result<()> {
    let mut surface = None;
    'profiles: loop {
        let (kept, profile) = match launcher.remote_menu(surface.take()).await? {
            // Escape at the profile list drops the parked Window, unmapping it.
            Phase::Closed | Phase::Kept(_, None) => return Ok(()),
            Phase::Kept(kept, Some(profile)) => (kept, profile),
        };
        let (mut kept, mut remote) = match launcher.connect(kept, &profile).await? {
            Phase::Closed => return Ok(()),
            // Cancelled or failed: OpenSSH is already dead; back to the list.
            Phase::Kept(kept, None) => {
                surface = Some(kept);
                continue 'profiles;
            }
            Phase::Kept(kept, Some(remote)) => (kept, remote),
        };
        loop {
            match launcher.dojo_menu(kept, &mut remote).await? {
                Phase::Closed => return Ok(()),
                Phase::Kept(next, DojoChoice::Back) => {
                    // End the SSH connection before the profiles show again.
                    drop(remote);
                    surface = Some(next);
                    continue 'profiles;
                }
                Phase::Kept(next, DojoChoice::New) => {
                    return launcher.open_terminal(next, remote, None).await;
                }
                Phase::Kept(next, DojoChoice::Open(index)) => {
                    match launcher.resolve(&next, &mut remote, index).await? {
                        Resolved::Found(dojo) => {
                            return launcher.open_terminal(next, remote, Some(dojo)).await;
                        }
                        Resolved::Gone => kept = next,
                        Resolved::Closed => {
                            // The Window is gone: end SSH, then the flow.
                            drop(remote);
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
}

/// A connected remote, its Lairs as last listed, and the Dojo menu's rows.
struct ConnectedRemote {
    factory: ConnectionFactory,
    lairs: Vec<Lair>,
    entries: Vec<SessionEntry>,
}

/// The remote's reopenable Dojos, most recent first.
fn remote_dojo_entries(lairs: &[Lair], recent: &[DojoId]) -> Vec<SessionEntry> {
    collect_sessions(lairs, recent)
        .into_iter()
        .filter(SessionEntry::reopenable)
        .collect()
}

/// The launcher on real Window phases.
struct WindowLauncher {
    config: AppConfig,
    /// The profile chosen last, selected again when the list shows again.
    last_profile: Option<String>,
}

impl LauncherSurface for WindowLauncher {
    type Surface = WindowSurface;
    type Remote = ConnectedRemote;
    type Dojo = Dojo;

    async fn remote_menu(
        &mut self,
        surface: Option<WindowSurface>,
    ) -> Result<Phase<WindowSurface, Option<String>>> {
        // Read again each time, so an edited remotes.toml applies on return.
        let catalog = RemoteCatalog::load_default();
        if let Err(error) = &catalog {
            report_invalid_catalog(error);
        }
        let (names, items, blocker) = chooser_contents(&catalog);
        let last = self
            .last_profile
            .as_ref()
            .and_then(|last| names.iter().position(|name| name == last));
        let config = self.config.clone();
        let (decision, surface) = tokio::task::spawn_blocking(move || {
            let (decision, receiver) = std_mpsc::channel();
            let mut picker = SessionPickerUi::remote_profiles(items, blocker, decision);
            if let Some(index) = last {
                picker.select_item(index);
            }
            run_staged_picker(&config, picker, &receiver, None, surface)
        })
        .await
        .context("remote chooser task failed")??;
        let Some(surface) = surface else {
            return Ok(Phase::Closed);
        };
        let profile = match decision {
            Some(SessionPickerDecision::Open(index)) => Some(chosen_profile(&names, index)?),
            Some(SessionPickerDecision::New) | None => None,
        };
        if profile.is_some() {
            self.last_profile.clone_from(&profile);
        }
        Ok(Phase::Kept(surface, profile))
    }

    async fn connect(
        &mut self,
        surface: WindowSurface,
        profile: &str,
    ) -> Result<Phase<WindowSurface, Option<ConnectedRemote>>> {
        let StatusEnd { connected, surface } = Box::pin(connect_with_status(
            profile.to_owned(),
            &self.config,
            Some(surface),
            true,
        ))
        .await?;
        let Some(surface) = surface else {
            return Ok(Phase::Closed);
        };
        Ok(match connected {
            Ok(RemoteConnected { factory, lairs }) => Phase::Kept(
                surface,
                Some(ConnectedRemote {
                    factory,
                    lairs,
                    entries: Vec::new(),
                }),
            ),
            Err(not_connected) if not_connected.window_closed => Phase::Closed,
            Err(_) => Phase::Kept(surface, None),
        })
    }

    async fn dojo_menu(
        &mut self,
        surface: WindowSurface,
        remote: &mut ConnectedRemote,
    ) -> Result<Phase<WindowSurface, DojoChoice>> {
        remote.entries = remote_dojo_entries(&remote.lairs, &recent_dojo_ids(&remote.factory));
        let items = remote
            .entries
            .iter()
            .map(session_picker_item)
            .collect::<Vec<_>>();
        let config = self.config.clone();
        let (decision, surface) = tokio::task::spawn_blocking(move || {
            let empty = items.is_empty();
            let (decision, receiver) = std_mpsc::channel();
            let mut picker = SessionPickerUi::new(items, decision);
            if empty {
                // A host without Dojos offers only a new terminal.
                picker.select_new_row();
            }
            run_staged_picker(&config, picker, &receiver, None, Some(surface))
        })
        .await
        .context("remote Dojo picker task failed")??;
        let Some(surface) = surface else {
            return Ok(Phase::Closed);
        };
        let choice = match decision {
            None => DojoChoice::Back,
            Some(SessionPickerDecision::New) => DojoChoice::New,
            Some(SessionPickerDecision::Open(index)) => DojoChoice::Open(index),
        };
        Ok(Phase::Kept(surface, choice))
    }

    async fn resolve(
        &mut self,
        surface: &WindowSurface,
        remote: &mut ConnectedRemote,
        index: usize,
    ) -> Result<Resolved<Dojo>> {
        let selected = remote
            .entries
            .get(index)
            .context("remote Dojo picker returned an invalid selection")?;
        let (lair_id, dojo_id) = (selected.lair_id, selected.dojo_id);
        let factory = remote.factory.clone();
        let listed = while_parked(
            parked_close(Some(surface)),
            parked_deadline(&factory),
            async move {
                let mut connection = factory.connect().await?;
                let deadline = parked_deadline(&factory);
                let Response::Lairs { lairs, .. } = connection
                    .request_with_deadline(Request::ListLairs, deadline)
                    .await?
                else {
                    bail!("remote splinterd did not return its session list");
                };
                Ok(lairs)
            },
        )
        .await?;
        let WhileParked::Done(lairs) = listed else {
            return Ok(Resolved::Closed);
        };
        remote.lairs = lairs;
        let reopenable = remote_dojo_entries(&remote.lairs, &[])
            .iter()
            .any(|entry| entry.lair_id == lair_id && entry.dojo_id == dojo_id);
        if !reopenable {
            return Ok(Resolved::Gone);
        }
        let dojo = select_dojo_from(&remote.lairs, (lair_id, dojo_id))?;
        remember_dojo(&remote.factory, dojo.id);
        Ok(Resolved::Found(dojo))
    }

    async fn open_terminal(
        &mut self,
        surface: WindowSurface,
        remote: ConnectedRemote,
        dojo: Option<Dojo>,
    ) -> Result<()> {
        let config = self.config.clone();
        match dojo {
            Some(dojo) => {
                Box::pin(run_live_multipane_window_with_app_id(
                    config,
                    dojo,
                    remote.factory,
                    None,
                    true,
                    Some(surface),
                ))
                .await
            }
            None => Box::pin(launch_new_terminal_on(config, remote.factory, surface)).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::VecDeque, rc::Rc};

    type Log = Rc<RefCell<Vec<String>>>;

    /// A parked fake Window; dropping it records the unmap.
    struct FakeSurface(Log);

    impl Drop for FakeSurface {
        fn drop(&mut self) {
            self.0.borrow_mut().push("unmapped".to_owned());
        }
    }

    /// A fake connected remote; dropping it records that SSH ended.
    struct FakeRemote(Log, String);

    impl Drop for FakeRemote {
        fn drop(&mut self) {
            self.0.borrow_mut().push(format!("ssh to {} ended", self.1));
        }
    }

    /// A scripted resolve: the Dojo exists, is gone, or the remote never
    /// answers and the parked Window is closed meanwhile.
    #[derive(Clone, Copy)]
    enum FakeResolve {
        Found,
        Gone,
        NeverAnswersThenClosed,
    }

    /// A scripted profile list outcome.
    #[derive(Clone, Copy)]
    enum Menu {
        Closed,
        Escape,
        Pick(&'static str),
    }

    /// Scripted phase outcomes: `None` is a compositor close.
    #[derive(Default)]
    struct FakeLauncher {
        log: Log,
        menus: VecDeque<Menu>,
        connects: VecDeque<Option<bool>>,
        dojo_menus: VecDeque<Option<DojoChoice>>,
        /// How each resolve ends.
        resolves: VecDeque<FakeResolve>,
    }

    impl FakeLauncher {
        fn note(&self, event: impl Into<String>) {
            self.log.borrow_mut().push(event.into());
        }

        fn events(&self) -> Vec<String> {
            self.log.borrow().clone()
        }

        fn kept<T>(
            &self,
            surface: Option<FakeSurface>,
            outcome: Option<T>,
        ) -> Phase<FakeSurface, T> {
            if let Some(value) = outcome {
                Phase::Kept(
                    surface.unwrap_or_else(|| FakeSurface(self.log.clone())),
                    value,
                )
            } else {
                // The compositor closed the Window: it is already gone.
                std::mem::forget(surface);
                Phase::Closed
            }
        }
    }

    impl LauncherSurface for FakeLauncher {
        type Surface = FakeSurface;
        type Remote = FakeRemote;
        type Dojo = usize;

        async fn remote_menu(
            &mut self,
            surface: Option<FakeSurface>,
        ) -> Result<Phase<FakeSurface, Option<String>>> {
            self.note(if surface.is_some() {
                "profiles"
            } else {
                "profiles in a new Window"
            });
            let outcome = match self.menus.pop_front().expect("scripted profile list") {
                Menu::Closed => None,
                Menu::Escape => Some(None),
                Menu::Pick(profile) => Some(Some(profile.to_owned())),
            };
            Ok(self.kept(surface, outcome))
        }

        async fn connect(
            &mut self,
            surface: FakeSurface,
            profile: &str,
        ) -> Result<Phase<FakeSurface, Option<FakeRemote>>> {
            self.note(format!("status for {profile}"));
            let outcome = self.connects.pop_front().expect("scripted status");
            let remote = outcome.map(|connected| {
                connected.then(|| FakeRemote(self.log.clone(), profile.to_owned()))
            });
            Ok(self.kept(Some(surface), remote))
        }

        async fn dojo_menu(
            &mut self,
            surface: FakeSurface,
            remote: &mut FakeRemote,
        ) -> Result<Phase<FakeSurface, DojoChoice>> {
            self.note(format!("Dojos on {}", remote.1));
            let outcome = self.dojo_menus.pop_front().expect("scripted Dojo menu");
            Ok(self.kept(Some(surface), outcome))
        }

        async fn resolve(
            &mut self,
            _surface: &FakeSurface,
            _remote: &mut FakeRemote,
            index: usize,
        ) -> Result<Resolved<usize>> {
            let script = self.resolves.pop_front().expect("scripted resolve");
            let outcome = match script {
                FakeResolve::Found => Resolved::Found(index),
                FakeResolve::Gone => Resolved::Gone,
                FakeResolve::NeverAnswersThenClosed => {
                    // A stalled daemon: only the close ends the request.
                    let listed = while_parked(
                        Some(std::future::ready(())),
                        std::time::Duration::from_secs(3600),
                        std::future::pending::<Result<()>>(),
                    )
                    .await?;
                    assert_eq!(listed, WhileParked::Closed);
                    // The compositor already unmapped the Window.
                    Resolved::Closed
                }
            };
            self.note(format!(
                "resolve row {index}: {}",
                match outcome {
                    Resolved::Found(_) => "found",
                    Resolved::Gone => "gone",
                    Resolved::Closed => "closed",
                }
            ));
            Ok(outcome)
        }

        async fn open_terminal(
            &mut self,
            surface: FakeSurface,
            remote: FakeRemote,
            dojo: Option<usize>,
        ) -> Result<()> {
            self.note(match dojo {
                Some(index) => format!("terminal for row {index} on {}", remote.1),
                None => format!("new terminal on {}", remote.1),
            });
            // The terminal phase owns both until the Window closes.
            drop((surface, remote));
            Ok(())
        }
    }

    fn launch(launcher: &mut FakeLauncher) -> Vec<String> {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(run_launcher(launcher))
            .unwrap();
        launcher.events()
    }

    #[test]
    fn escape_at_the_profile_list_closes_the_window() {
        let mut launcher = FakeLauncher {
            menus: VecDeque::from([Menu::Escape]),
            ..FakeLauncher::default()
        };
        assert_eq!(
            launch(&mut launcher),
            ["profiles in a new Window", "unmapped"]
        );
    }

    #[test]
    fn a_compositor_close_at_any_stage_ends_the_launcher_and_ssh() {
        let mut at_profiles = FakeLauncher {
            menus: VecDeque::from([Menu::Closed]),
            ..FakeLauncher::default()
        };
        assert_eq!(launch(&mut at_profiles), ["profiles in a new Window"]);

        let mut at_status = FakeLauncher {
            menus: VecDeque::from([Menu::Pick("wintermute")]),
            connects: VecDeque::from([None]),
            ..FakeLauncher::default()
        };
        assert_eq!(
            launch(&mut at_status),
            ["profiles in a new Window", "status for wintermute"]
        );

        let mut at_dojos = FakeLauncher {
            menus: VecDeque::from([Menu::Pick("wintermute")]),
            connects: VecDeque::from([Some(true)]),
            dojo_menus: VecDeque::from([None]),
            ..FakeLauncher::default()
        };
        assert_eq!(
            launch(&mut at_dojos),
            [
                "profiles in a new Window",
                "status for wintermute",
                "Dojos on wintermute",
                "ssh to wintermute ended"
            ]
        );
    }

    #[test]
    fn a_failed_or_cancelled_connection_returns_to_the_profiles() {
        // Unreachable then Escape, or Escape in the masked field: the status
        // flow has already killed OpenSSH and reports no connection.
        let mut launcher = FakeLauncher {
            menus: VecDeque::from([
                Menu::Pick("wintermute"),
                Menu::Pick("straylight"),
                Menu::Escape,
            ]),
            connects: VecDeque::from([Some(false), Some(false)]),
            ..FakeLauncher::default()
        };
        assert_eq!(
            launch(&mut launcher),
            [
                "profiles in a new Window",
                "status for wintermute",
                "profiles",
                "status for straylight",
                "profiles",
                "unmapped"
            ]
        );
    }

    #[test]
    fn escape_at_the_dojo_menu_ends_ssh_before_the_profiles_show() {
        let mut launcher = FakeLauncher {
            menus: VecDeque::from([Menu::Pick("wintermute"), Menu::Pick("straylight")]),
            connects: VecDeque::from([Some(true), Some(true)]),
            dojo_menus: VecDeque::from([Some(DojoChoice::Back), Some(DojoChoice::New)]),
            ..FakeLauncher::default()
        };
        assert_eq!(
            launch(&mut launcher),
            [
                "profiles in a new Window",
                "status for wintermute",
                "Dojos on wintermute",
                "ssh to wintermute ended",
                "profiles",
                "status for straylight",
                "Dojos on straylight",
                "new terminal on straylight",
                "unmapped",
                "ssh to straylight ended"
            ]
        );
    }

    #[test]
    fn a_dojo_gone_before_it_opens_lists_the_dojos_again() {
        let mut launcher = FakeLauncher {
            menus: VecDeque::from([Menu::Pick("wintermute")]),
            connects: VecDeque::from([Some(true)]),
            dojo_menus: VecDeque::from([Some(DojoChoice::Open(0)), Some(DojoChoice::Open(1))]),
            resolves: VecDeque::from([FakeResolve::Gone, FakeResolve::Found]),
            ..FakeLauncher::default()
        };
        assert_eq!(
            launch(&mut launcher),
            [
                "profiles in a new Window",
                "status for wintermute",
                "Dojos on wintermute",
                "resolve row 0: gone",
                "Dojos on wintermute",
                "resolve row 1: found",
                "terminal for row 1 on wintermute",
                "unmapped",
                "ssh to wintermute ended"
            ]
        );
    }

    #[test]
    fn a_window_closed_while_a_stalled_remote_lists_dojos_ends_ssh_at_once() {
        let mut launcher = FakeLauncher {
            menus: VecDeque::from([Menu::Pick("wintermute")]),
            connects: VecDeque::from([Some(true)]),
            dojo_menus: VecDeque::from([Some(DojoChoice::Open(0))]),
            resolves: VecDeque::from([FakeResolve::NeverAnswersThenClosed]),
            ..FakeLauncher::default()
        };
        let events = launch(&mut launcher);
        assert_eq!(
            events,
            [
                "profiles in a new Window",
                "status for wintermute",
                "Dojos on wintermute",
                "resolve row 0: closed",
                "ssh to wintermute ended",
                "unmapped"
            ]
        );
    }

    #[test]
    fn a_host_without_dojos_offers_only_a_new_terminal() {
        assert!(remote_dojo_entries(&[], &[]).is_empty());
        let mut lair = Lair::new("work", std::path::PathBuf::from("/srv"));
        // A Lair whose only Dojo has no running pane is not reopenable.
        let splinterm_core::LayoutNode::Leaf(splint) = &mut lair.dojos[0].root else {
            panic!("a new Lair has one Splint");
        };
        splint.state = splinterm_core::SplintState::Exited(0);
        assert!(remote_dojo_entries(&[lair.clone()], &[]).is_empty());
        let splinterm_core::LayoutNode::Leaf(splint) = &mut lair.dojos[0].root else {
            panic!("a new Lair has one Splint");
        };
        splint.state = splinterm_core::SplintState::Running;
        assert_eq!(remote_dojo_entries(&[lair], &[]).len(), 1);
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
    fn recent_dojos_offers_a_remote_row_only_when_it_can_help() {
        assert_eq!(remote_dojos_row_for(&Ok(RemoteCatalog::default())), None);
        let row = remote_dojos_row_for(&catalog(TWO_PROFILES)).unwrap();
        assert_eq!(row.display_title, "Remote Dojos…");
        assert_eq!(row.breadcrumb, "2 remote profiles");
        assert_eq!(row.pane_count, 0);
        let one = remote_dojos_row_for(&catalog("version = 1\n[remotes.solo]\nhost = \"solo\"\n"))
            .unwrap();
        assert_eq!(one.breadcrumb, "1 remote profile");
        let invalid = remote_dojos_row_for(&catalog("version = 9\n")).unwrap();
        assert_eq!(invalid.breadcrumb, "Check remotes.toml");
    }
}
