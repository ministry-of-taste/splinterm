use std::{
    fmt::Write as _,
    io::{self, Write},
};

use anyhow::{Context, Result};
use splinterm_core::{SplintState, TopologyRevision};
use splinterm_protocol::{Response, TerminalSnapshot};

#[allow(
    clippy::unnecessary_wraps,
    reason = "response rendering retains a fallible CLI boundary for future output modes"
)]
fn print_restore_results(
    topology_revision: TopologyRevision,
    results: Vec<splinterm_protocol::RestoreLeafResult>,
) {
    println!(
        "Restore completed at topology revision {}.",
        topology_revision.get()
    );
    for result in results {
        match (result.incarnation, result.error) {
            (Some(incarnation), None) => {
                println!(
                    "  {}: started as incarnation {incarnation}",
                    result.splint_id
                );
            }
            // A Splint that has not exited needed no restore (Plan 0067 M5).
            (None, Some(error))
                if error.code == splinterm_protocol::ErrorCode::InvalidArgument
                    && error.message == splinterm_protocol::SPLINT_NOT_EXITED =>
            {
                println!("  {}: already running", result.splint_id);
            }
            (_, Some(error)) => {
                println!("  {}: failed: {}", result.splint_id, error.message);
            }
            _ => println!("  {}: failed without a result", result.splint_id),
        }
    }
}

fn node_has_active_splint(node: &splinterm_core::LayoutNode) -> bool {
    match node {
        splinterm_core::LayoutNode::Leaf(splint) => !matches!(splint.state, SplintState::Exited(_)),
        splinterm_core::LayoutNode::Branch { first, second, .. } => {
            node_has_active_splint(first) || node_has_active_splint(second)
        }
    }
}

fn lair_has_active_splint(dojo: &splinterm_core::Lair) -> bool {
    dojo.dojos
        .iter()
        .any(|dojo| node_has_active_splint(&dojo.root))
}

fn render_active_splint_ids(node: &splinterm_core::LayoutNode, output: &mut String) {
    match node {
        splinterm_core::LayoutNode::Leaf(splint)
            if !matches!(splint.state, SplintState::Exited(_)) =>
        {
            let state = match splint.state {
                SplintState::Starting => "starting",
                SplintState::Running => "running",
                SplintState::Exited(_) => unreachable!("exited Splints are filtered"),
            };
            writeln!(output, "    {state:<8} {}  {}", splint.title, splint.id)
                .expect("writing to String cannot fail");
        }
        splinterm_core::LayoutNode::Leaf(_) => {}
        splinterm_core::LayoutNode::Branch { first, second, .. } => {
            render_active_splint_ids(first, output);
            render_active_splint_ids(second, output);
        }
    }
}

fn render_all_splint_ids(node: &splinterm_core::LayoutNode, output: &mut String) {
    match node {
        splinterm_core::LayoutNode::Leaf(splint) => {
            writeln!(
                output,
                "  {}  {}  {:?}",
                splint.id, splint.title, splint.state
            )
            .expect("writing to String cannot fail");
        }
        splinterm_core::LayoutNode::Branch { first, second, .. } => {
            render_all_splint_ids(first, output);
            render_all_splint_ids(second, output);
        }
    }
}

fn render_lairs(lairs: &[splinterm_core::Lair], all: bool) -> String {
    let mut output = String::new();
    if all {
        if lairs.is_empty() {
            return "No Lairs.\n".to_owned();
        }
        for lair in lairs {
            let splints: usize = lair.dojos.iter().map(|dojo| dojo.root.splint_count()).sum();
            writeln!(
                output,
                "{}  {}  {} Dojo(s)  {splints} Splint(s)",
                lair.id,
                lair.name,
                lair.dojos.len()
            )
            .expect("writing to String cannot fail");
            for dojo in &lair.dojos {
                writeln!(
                    output,
                    "  Dojo {}  {}  default-focus {}",
                    dojo.id, dojo.name, dojo.default_focus
                )
                .expect("writing to String cannot fail");
                render_all_splint_ids(&dojo.root, &mut output);
            }
        }
        return output;
    }

    let active: Vec<_> = lairs
        .iter()
        .filter(|lair| lair_has_active_splint(lair))
        .collect();
    let hidden = lairs.len().saturating_sub(active.len());

    if active.is_empty() {
        writeln!(output, "No active Lairs.").expect("writing to String cannot fail");
    } else {
        writeln!(
            output,
            "Active Lair{} ({})",
            if active.len() == 1 { "" } else { "s" },
            active.len()
        )
        .expect("writing to String cannot fail");
        for lair in active {
            writeln!(output, "\n{}", lair.name).expect("writing to String cannot fail");
            writeln!(output, "  Lair    {}", lair.id).expect("writing to String cannot fail");
            for dojo in &lair.dojos {
                if !node_has_active_splint(&dojo.root) {
                    continue;
                }
                writeln!(output, "  Dojo    {}  {}", dojo.name, dojo.id)
                    .expect("writing to String cannot fail");
                render_active_splint_ids(&dojo.root, &mut output);
            }
        }
        writeln!(
            output,
            "\nStop one running Splint with: splinterm kill <SPLINT_ID>"
        )
        .expect("writing to String cannot fail");
    }

    if hidden > 0 {
        writeln!(
            output,
            "{hidden} inactive Lair{} hidden.",
            if hidden == 1 { "" } else { "s" },
        )
        .expect("writing to String cannot fail");
    }
    writeln!(output, "Show complete history with: splinterm list --all")
        .expect("writing to String cannot fail");
    output
}

pub(in crate::app) fn print_lairs(lairs: &[splinterm_core::Lair], all: bool) {
    print!("{}", render_lairs(lairs, all));
}

#[allow(
    clippy::too_many_lines,
    reason = "the CLI keeps exhaustive human rendering for every private protocol response"
)]
pub(in crate::app) fn print_response(response: Response) -> Result<()> {
    match response {
        Response::Pong => println!("splinterd is awake"),
        Response::MutationPrepared { .. } => {
            println!("Mutation preflight prepared.");
        }
        Response::Lairs { lairs, .. } => print_lairs(&lairs, true),
        Response::LairCreated { lair, .. } => println!("Created Lair '{}'.", lair.name),
        Response::PresetMaterialized {
            lair_id,
            dojo_ids,
            panes,
            topology_revision,
        } => println!(
            "Materialized {} Dojo(s) and {} pane(s) in Lair {lair_id} at topology revision {}.",
            dojo_ids.len(),
            panes.len(),
            topology_revision.get()
        ),
        Response::Topology { snapshot } => println!(
            "Topology revision {}: {} Lair(s), {} Splint(s)",
            snapshot.revision.get(),
            snapshot.topology.lairs().count(),
            snapshot.runtimes.len()
        ),
        Response::GraphicalFocus {
            focused_splint_id,
            cwd,
        } => println!(
            "Focused Splint: {}; cwd: {}",
            focused_splint_id.map_or_else(|| "none".to_owned(), |id| id.to_string()),
            cwd.map_or_else(
                || "unavailable".to_owned(),
                |path| path.display().to_string()
            )
        ),
        Response::TopologySubscribed {
            subscription_id,
            snapshot,
        } => println!(
            "Topology subscription {subscription_id} started at revision {}.",
            snapshot.revision.get()
        ),
        Response::Splint { runtime, .. } => println!(
            "Splint {:?}: {:?}, incarnation={:?}, exit={:?}",
            runtime.splint_id, runtime.lifecycle, runtime.live_incarnation, runtime.exit_status
        ),
        Response::LaunchDirectoryResolved {
            source_splint_id,
            cwd,
        } => match cwd {
            Some(cwd) => println!("Launch directory for {source_splint_id}: {}", cwd.display()),
            None => println!("Launch directory for {source_splint_id} is unavailable."),
        },
        Response::Attached { snapshot, .. } => print_snapshot(&snapshot),
        Response::ImageContentReady { transfer } => println!(
            "Image content {} generation {} ready via {:?} ({} bytes).",
            transfer.content_id, transfer.generation, transfer.transfer, transfer.byte_length
        ),
        Response::ScrollbackPage { page, .. } => println!(
            "Scrollback page: {} row(s), has_older={}",
            page.rows.len(),
            page.has_older
        ),
        Response::ScrollbackResyncRequired {
            current_revision,
            history_generation,
            ..
        } => println!(
            "Scrollback resync required at revision {current_revision}, generation {history_generation}"
        ),
        Response::SearchResults { page, .. } => println!(
            "Search page: {} match(es), continuation={}, timed_out={}",
            page.matches.len(),
            page.next_cursor.is_some(),
            page.timed_out,
        ),
        Response::SearchResyncRequired {
            current_revision,
            history_generation,
            ..
        } => println!(
            "Search resync required at revision {current_revision}, generation {history_generation}"
        ),
        Response::AccessGranted { grant, .. } => {
            println!("Access grant {} issued.", grant.grant_id);
        }
        Response::AccessRevoked { grant, .. } => {
            println!("Access grant {} revoked.", grant.grant_id);
        }
        Response::LairAccessGranted { grant, .. } => {
            println!("Lair access grant {} issued.", grant.grant_id);
        }
        Response::LairAccessRevoked { grant, .. } => {
            println!("Lair access grant {} revoked.", grant.grant_id);
        }
        Response::LairAccessPending { request_id, .. } => {
            println!("Lair access request {request_id} is waiting for an answer.");
        }
        Response::LairAccessRequestCancelled { request_id, .. } => {
            println!("Lair access request {request_id} cancelled.");
        }
        Response::LairAccessRequestsCancelled { cancelled } => {
            println!("Cancelled {cancelled} pending Lair access requests.");
        }
        Response::AccessInventory {
            entries,
            total_entries,
            ..
        } => println!(
            "Access inventory page: {} of {total_entries} entries.",
            entries.len()
        ),
        Response::AccessEntryRevoked { revocation, .. } => {
            println!("Access grant {} revoked.", revocation.grant_id);
        }
        Response::AllAccessRevoked {
            dry_run, counts, ..
        } => println!(
            "{} {} remembered, {} once, {} while-open grants and {} pending requests.",
            if dry_run { "Would revoke" } else { "Revoked" },
            counts.remembered,
            counts.once,
            counts.workspace_session,
            counts.pending_requests
        ),
        Response::AuthorizationStatus {
            grants,
            lair_grants,
            persistent,
            development_bypass,
            ..
        } => {
            println!(
                "{} active grant(s), {} persistent rule(s); development bypass={development_bypass}",
                grants.len() + lair_grants.len(),
                persistent.len()
            );
        }
        Response::ControlGranted { controller_id, .. } => {
            println!("Controller lease {controller_id} granted.");
        }
        Response::ControlSubscribed {
            subscription_id,
            status,
        } => println!(
            "Control subscription {subscription_id}: controlled={}, locally_owned={}",
            status.controlled, status.locally_owned,
        ),
        Response::ControlTransferPending { transfer_id, .. } => {
            println!("Control transfer {transfer_id} pending.");
        }
        Response::ControlTransferDecided { outcome, .. } => {
            println!("Control transfer {outcome:?}.");
        }
        Response::AuditPage { page } => println!(
            "Audit page: {} record(s), retention_gap={}, newest={:?}.",
            page.records.len(),
            page.retention_gap,
            page.newest_available_audit_id
        ),
        Response::TerminalActionAcknowledged {
            splint_id,
            incarnation,
            terminal_revision,
            ..
        } => println!(
            "Splint {splint_id} incarnation {incarnation} acknowledged at terminal revision {terminal_revision}."
        ),
        Response::Acknowledged => println!("Acknowledged."),
        Response::SplintStarted {
            splint_id,
            incarnation,
            topology_revision,
        } => println!(
            "Splint {splint_id} started as incarnation {incarnation} at topology revision {}.",
            topology_revision.get()
        ),
        Response::DojoStarted {
            dojo_id,
            splint_id,
            incarnation,
            topology_revision,
        } => println!(
            "Dojo {dojo_id:?} started with Splint {splint_id} incarnation {incarnation} at revision {}.",
            topology_revision.get()
        ),
        Response::TopologyCommitted { topology_revision } => {
            println!("Topology revision {} committed.", topology_revision.get());
        }
        Response::RestoreCompleted {
            topology_revision,
            results,
        } => print_restore_results(topology_revision, results),
        Response::SplintKilled {
            splint_id,
            incarnation,
            exit_status,
        } => println!(
            "Splint {splint_id} incarnation {incarnation} exited (code={:?}, signal={:?}).",
            exit_status.code, exit_status.signal
        ),
        Response::AgentStatusAccepted { sequence, .. } => {
            println!("Agent status accepted (sequence {sequence}).");
        }
        Response::UnusedDojoClose { dojo_id, outcome } => match outcome {
            splinterm_protocol::UnusedDojoCloseOutcome::Closed { lair_removed, .. } => {
                if lair_removed {
                    println!("Closed unused Dojo {dojo_id} and its empty unnamed Lair.");
                } else {
                    println!("Closed unused Dojo {dojo_id}.");
                }
            }
            splinterm_protocol::UnusedDojoCloseOutcome::Used { reason } => {
                println!("Dojo {dojo_id} is in use ({reason:?}); left running.");
            }
        },
        Response::HandoffReport { report } => println!(
            "Daemon generation {} ({}): {} handoff blocking reason(s).",
            report.daemon_generation,
            report.build.version,
            report.blocking_reasons.len()
        ),
        Response::HandoffControlClaimed { controllers } => {
            println!(
                "Restored {} controller(s) after handoff.",
                controllers.len()
            );
        }
        Response::ConsentOffersSubscribed { subscription_id } => {
            println!("Subscribed to consent offers ({subscription_id}).");
        }
        Response::ConsentOfferAnswered => println!("Consent offer answered."),
        Response::DojoPresentationClaimed { granted } => println!(
            "Dojo presentation offer {}.",
            if granted { "claimed" } else { "already taken" }
        ),
    }
    io::stdout()
        .flush()
        .context("failed to flush command output")
}

fn print_snapshot(snapshot: &TerminalSnapshot) {
    println!(
        "Splint {:?} · incarnation {} · revision {} · {}x{}",
        snapshot.splint_id,
        snapshot.incarnation,
        snapshot.revision,
        snapshot.columns,
        snapshot.rows
    );
    for row in &snapshot.visible_rows {
        let line: String = row
            .cells
            .iter()
            .map(|cell| {
                if cell.content.is_empty() {
                    " "
                } else {
                    &cell.content
                }
            })
            .collect();
        println!("{}", line.trim_end());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn human_list_hides_inactive_lairs_and_all_retains_complete_detail() {
        let active = splinterm_core::Lair::new("active", PathBuf::from("/tmp"));
        let active_lair_id = active.id.to_string();
        let active_dojo_id = active.dojos[0].id.to_string();
        let active_splint_id = active.dojos[0].root.first_splint_id().to_string();
        assert!(lair_has_active_splint(&active));

        let mut inactive = splinterm_core::Lair::new("inactive", PathBuf::from("/tmp"));
        let inactive_id = inactive.id.to_string();
        let splinterm_core::LayoutNode::Leaf(splint) = &mut inactive.dojos[0].root else {
            panic!("new Dojo should contain one Splint")
        };
        splint.state = SplintState::Exited(0);
        assert!(!lair_has_active_splint(&inactive));

        let lairs = vec![active, inactive];
        let concise = render_lairs(&lairs, false);
        assert!(concise.contains("Active Lair (1)"));
        assert!(concise.contains("\nactive\n"));
        assert!(concise.contains(&active_lair_id));
        assert!(concise.contains(&active_dojo_id));
        assert!(concise.contains(&active_splint_id));
        assert!(!concise.contains("\ninactive\n"));
        assert!(concise.contains("1 inactive Lair hidden."));
        assert!(concise.contains("Stop one running Splint with:"));
        assert!(concise.contains("Show complete history with: splinterm list --all"));

        let all = render_lairs(&lairs, true);
        assert!(all.contains(" active "));
        assert!(all.contains(" inactive "));
        assert!(all.contains(&inactive_id));
        assert!(all.contains("default-focus"));
        assert!(all.contains("Exited(0)"));
    }

    #[test]
    fn empty_human_list_still_guides_complete_history() {
        let output = render_lairs(&[], false);
        assert_eq!(
            output,
            "No active Lairs.\nShow complete history with: splinterm list --all\n"
        );
        assert_eq!(render_lairs(&[], true), "No Lairs.\n");
    }
}
