use splinterm_terminal::{
    ActiveScreen, CellSnapshotContent, SnapshotRequest, Terminal, TerminalConfig, TerminalDamage,
    TerminalEvent, TerminalRevision,
};

fn terminal(columns: usize, rows: usize) -> Terminal {
    Terminal::new(columns, rows, TerminalConfig::default())
}

fn row_text(row: splinterm_terminal::RowSnapshot<'_>) -> String {
    row.cells()
        .map(|cell| match cell.content() {
            CellSnapshotContent::Empty | CellSnapshotContent::Spacer { .. } => ' ',
            CellSnapshotContent::Scalar(character) => character,
            CellSnapshotContent::Composed(chars) => chars[0],
        })
        .collect()
}

#[test]
fn snapshot_resolves_composed_content_and_semantic_attributes() {
    let mut terminal = terminal(8, 2);
    terminal.advance("\x1b[1;31me\u{301}".as_bytes());

    let snapshot = terminal.snapshot(SnapshotRequest::default());
    assert_eq!(snapshot.revision(), terminal.revision());
    assert_eq!(snapshot.dimensions().columns, 8);
    assert_eq!(snapshot.active_screen(), ActiveScreen::Normal);
    let first = snapshot.visible_rows().next().unwrap();
    assert!(first.id().is_some_and(|id| id > 0));
    let cell = first.cells().next().unwrap();
    assert_eq!(
        cell.content(),
        CellSnapshotContent::Composed(&['e', '\u{301}'])
    );
    assert!(cell.attributes().bold);
    assert_eq!(cell.attributes().foreground.value(), 1);
}

#[test]
fn bounded_scrollback_returns_newest_rows_in_chronological_order() {
    let mut terminal = terminal(4, 2);
    terminal.advance(b"a\r\nb\r\nc\r\nd");

    let snapshot = terminal.snapshot(SnapshotRequest {
        max_scrollback_rows: 2,
    });
    let rows = snapshot.scrollback_rows().map(row_text).collect::<Vec<_>>();
    assert_eq!(rows, vec!["a   ", "b   "]);
    assert_eq!(snapshot.scrollback().available_rows, 2);
    assert_eq!(snapshot.scrollback().returned_rows, 2);
    let ids = snapshot
        .scrollback_rows()
        .map(|row| row.id().expect("history rows carry identities"))
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    assert_eq!(snapshot.scrollback().oldest_available_row_id, Some(ids[0]));
    assert_eq!(snapshot.scrollback().newest_available_row_id, Some(ids[1]));

    let bounded = terminal.snapshot(SnapshotRequest {
        max_scrollback_rows: 1,
    });
    assert_eq!(
        bounded.scrollback_rows().map(row_text).collect::<Vec<_>>(),
        vec!["b   "]
    );
    assert_eq!(bounded.scrollback().omitted_oldest_rows, 1);

    let metadata_only = terminal.snapshot(SnapshotRequest::default());
    assert_eq!(metadata_only.scrollback().available_rows, 2);
    assert_eq!(metadata_only.scrollback().returned_rows, 0);
    assert_eq!(metadata_only.scrollback().oldest_available_row_id, None);
    assert_eq!(metadata_only.scrollback().newest_available_row_id, None);
}

#[test]
fn scrollback_pages_walk_older_rows_without_overlap() {
    let mut terminal = terminal(4, 2);
    terminal.advance(b"a\r\nb\r\nc\r\nd\r\ne\r\nf");
    let newest = terminal.snapshot(SnapshotRequest {
        max_scrollback_rows: 2,
    });
    let before = newest
        .scrollback_rows()
        .next()
        .and_then(splinterm_terminal::RowSnapshot::id)
        .unwrap();
    let page = terminal.scrollback_page(before, 2);
    assert_eq!(page.terminal_revision, terminal.revision());
    assert_eq!(
        page.rows.iter().copied().map(row_text).collect::<Vec<_>>(),
        vec!["a   ", "b   "]
    );
    assert!(!page.has_older);
    assert!(page.rows.iter().all(|row| row.id().unwrap() < before));
}

#[test]
fn stable_history_ids_survive_ring_movement_and_generation_changes_reset_them() {
    let config = TerminalConfig {
        scrollback_lines: 1,
        ..TerminalConfig::default()
    };
    let mut terminal = Terminal::new(2, 2, config);
    terminal.advance(b"x\r\nx\r\nx\r\nx\r\n");
    let first = terminal.snapshot(SnapshotRequest {
        max_scrollback_rows: 2,
    });
    let first_ids = first
        .scrollback_rows()
        .map(|row| row.id().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(first_ids.len(), 2);
    assert_ne!(first_ids[0], first_ids[1], "equal content is not identity");
    let generation = first.scrollback().history_generation;

    terminal.advance(b"x\r\n");
    let rolled = terminal.snapshot(SnapshotRequest {
        max_scrollback_rows: 2,
    });
    let rolled_ids = rolled
        .scrollback_rows()
        .map(|row| row.id().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rolled.scrollback().history_generation, generation);
    assert_eq!(rolled_ids[0], first_ids[1]);
    assert_ne!(rolled_ids[1], first_ids[1]);
    assert_eq!(
        rolled.scrollback().oldest_available_row_id,
        Some(rolled_ids[0])
    );
    assert_eq!(
        rolled.scrollback().newest_available_row_id,
        Some(rolled_ids[1])
    );

    terminal.advance(b"\x1b[3J");
    let cleared = terminal.snapshot(SnapshotRequest {
        max_scrollback_rows: 2,
    });
    assert!(cleared.scrollback().history_generation > generation);
    assert_eq!(cleared.scrollback().available_rows, 0);
    let cleared_generation = cleared.scrollback().history_generation;

    terminal.resize(3, 2);
    let resized = terminal.snapshot(SnapshotRequest {
        max_scrollback_rows: 2,
    });
    assert!(resized.scrollback().history_generation > cleared_generation);
}

#[test]
fn ris_advances_history_namespace_without_reusing_generation_or_row_ids() {
    let mut terminal = terminal(2, 2);
    terminal.advance(b"a\r\nb\r\nc\r\n");
    let before = terminal.snapshot(SnapshotRequest {
        max_scrollback_rows: 16,
    });
    let generation = before.scrollback().history_generation;
    let newest = before
        .scrollback()
        .newest_available_row_id
        .expect("history exists before RIS");

    terminal.advance(b"\x1bc");
    let reset = terminal.snapshot(SnapshotRequest {
        max_scrollback_rows: 16,
    });
    assert!(reset.scrollback().history_generation > generation);
    assert_eq!(reset.scrollback().available_rows, 0);
    let reset_generation = reset.scrollback().history_generation;

    terminal.advance(b"x\r\ny\r\nz");
    let after = terminal.snapshot(SnapshotRequest {
        max_scrollback_rows: 16,
    });
    assert_eq!(after.scrollback().history_generation, reset_generation);
    assert!(
        after
            .scrollback_rows()
            .all(|row| row.id().expect("history identity") > newest)
    );
}

#[test]
fn sparse_wrapped_reflow_assigns_ids_in_chronological_order() {
    let config = TerminalConfig {
        scrollback_lines: 5,
        ..TerminalConfig::default()
    };
    let mut terminal = Terminal::new(4, 3, config);
    terminal.advance(b"a\r\nb\r\nc\r\nd\r\ne");
    let before = terminal.snapshot(SnapshotRequest {
        max_scrollback_rows: 16,
    });
    let previous_newest = before.scrollback().newest_available_row_id.unwrap();
    let previous_generation = before.scrollback().history_generation;

    terminal.resize(3, 3);
    let after = terminal.snapshot(SnapshotRequest {
        max_scrollback_rows: 16,
    });
    let ids = after
        .scrollback_rows()
        .map(|row| row.id().expect("reflowed history identity"))
        .collect::<Vec<_>>();
    assert!(after.scrollback().history_generation > previous_generation);
    assert!(!ids.is_empty());
    assert!(ids.iter().all(|id| *id > previous_newest));
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(
        after.scrollback().oldest_available_row_id,
        ids.first().copied()
    );
    assert_eq!(
        after.scrollback().newest_available_row_id,
        ids.last().copied()
    );
}

#[test]
fn alternate_snapshot_excludes_retained_normal_scrollback() {
    let mut terminal = terminal(4, 2);
    terminal.advance(b"a\r\nb\r\nc\x1b[?1049hALT");
    let snapshot = terminal.snapshot(SnapshotRequest {
        max_scrollback_rows: 20,
    });
    assert_eq!(snapshot.active_screen(), ActiveScreen::Alternate);
    assert_eq!(snapshot.scrollback().available_rows, 0);
    assert_eq!(snapshot.scrollback_rows().count(), 0);
}

#[test]
fn revisions_are_action_based_and_ignore_incomplete_input() {
    let mut terminal = terminal(8, 2);
    assert_eq!(terminal.revision(), TerminalRevision::default());
    terminal.advance(b"");
    terminal.advance(b"\x1b");
    assert_eq!(terminal.revision(), TerminalRevision::default());

    terminal.advance(b"\x18AB");
    assert_eq!(terminal.revision().value(), 2);
    terminal.advance(b"\x1b[2D");
    assert_eq!(terminal.revision().value(), 3);
    terminal.advance(b"\x1b[1m");
    assert_eq!(terminal.revision().value(), 3);
}

#[test]
fn deferred_wrap_changes_revision_even_when_cursor_coordinate_is_unchanged() {
    let mut terminal = terminal(3, 1);
    terminal.advance(b"abc");
    let base = terminal.revision();
    assert!(terminal.grid().cursor().deferred_wrap());

    terminal.advance(b"\x1b[C");
    assert_eq!(terminal.revision().value(), base.value() + 1);
    assert!(!terminal.grid().cursor().deferred_wrap());
    let update = terminal.updates_since(base).unwrap();
    assert!(
        update
            .updates()
            .next()
            .unwrap()
            .damage()
            .any(|damage| matches!(damage, TerminalDamage::Cursor { .. }))
    );
}

#[test]
fn idempotent_erase_and_empty_scrollback_clear_do_not_revise() {
    let mut terminal = terminal(4, 2);
    terminal.advance(b"\x1b[K\x1b[3J");
    assert_eq!(terminal.revision(), TerminalRevision::default());
}

#[test]
fn erase_scrollback_emits_scrollback_damage() {
    let mut terminal = terminal(3, 2);
    terminal.advance(b"a\r\nb\r\nc");
    let base = terminal.revision();
    terminal.advance(b"\x1b[3J");
    let updates = terminal.updates_since(base).unwrap();
    assert!(
        updates
            .updates()
            .next()
            .unwrap()
            .damage()
            .any(|damage| *damage == TerminalDamage::Scrollback)
    );
}

#[test]
fn updates_report_rows_cursor_scroll_and_full_snapshot_damage() {
    let mut terminal = terminal(3, 2);
    terminal.advance(b"A");
    let first = terminal.updates_since(TerminalRevision::default()).unwrap();
    let damage = first
        .updates()
        .next()
        .unwrap()
        .damage()
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        damage
            .iter()
            .any(|item| matches!(item, TerminalDamage::Rows { start: 0, end: 1 }))
    );
    assert!(
        damage
            .iter()
            .any(|item| matches!(item, TerminalDamage::Cursor { .. }))
    );

    let base = terminal.revision();
    terminal.advance(b"bcdefg");
    let updates = terminal.updates_since(base).unwrap();
    assert!(
        updates
            .updates()
            .flat_map(splinterm_terminal::TerminalUpdate::damage)
            .any(|item| { matches!(item, TerminalDamage::Scroll { .. }) })
    );

    let base = terminal.revision();
    terminal.resize(5, 3);
    let resize = terminal.updates_since(base).unwrap();
    assert!(
        resize
            .updates()
            .next()
            .unwrap()
            .damage()
            .any(|item| *item == TerminalDamage::FullSnapshot)
    );
}

#[test]
fn bounded_history_forces_resnapshot_on_gaps_and_future_bases() {
    let config = TerminalConfig {
        update_history_limit: 2,
        ..TerminalConfig::default()
    };
    let mut terminal = Terminal::new(8, 2, config);
    terminal.advance(b"ABCD");
    assert_eq!(terminal.revision().value(), 4);

    let gap = terminal
        .updates_since(TerminalRevision::default())
        .unwrap_err();
    assert_eq!(gap.oldest_available().value(), 2);
    assert_eq!(gap.current().value(), 4);
    assert_eq!(
        terminal
            .update_count_since(TerminalRevision::default())
            .unwrap_err(),
        gap
    );

    assert_eq!(
        terminal
            .update_count_since(TerminalRevision::new(2))
            .unwrap(),
        2
    );
    let retained = terminal.updates_since(TerminalRevision::new(2)).unwrap();
    let expected = retained.updates().cloned().collect::<Vec<_>>();
    assert_eq!(retained.clone().into_updates(), expected);
    assert_eq!(retained.updates().count(), 2);
    assert!(terminal.updates_since(TerminalRevision::new(9)).is_err());
    assert!(
        terminal
            .update_count_since(TerminalRevision::new(9))
            .is_err()
    );
    assert_eq!(
        terminal
            .updates_since(terminal.revision())
            .unwrap()
            .updates()
            .count(),
        0
    );
    assert_eq!(terminal.update_count_since(terminal.revision()).unwrap(), 0);
}

#[test]
fn event_overflow_is_explicit_and_snapshot_does_not_drain_effects() {
    let config = TerminalConfig {
        event_limit: 2,
        ..TerminalConfig::default()
    };
    let mut terminal = Terminal::new(4, 1, config);
    terminal.advance(b"\x07\x07\x07");
    {
        let snapshot = terminal.snapshot(SnapshotRequest::default());
        assert_eq!(snapshot.revision().value(), 3);
    }
    assert_eq!(
        terminal.drain_events().collect::<Vec<_>>(),
        vec![TerminalEvent::Bell, TerminalEvent::EventQueueOverflow]
    );
}

#[test]
fn reset_preserves_monotonic_revision_and_chunking_updates_match() {
    let input = b"ab\x1b[2D\x1b]2;title\x07\x1bcZ";
    let mut whole = terminal(8, 2);
    whole.advance(input);

    let mut bytewise = terminal(8, 2);
    for byte in input {
        bytewise.advance(std::slice::from_ref(byte));
    }

    assert_eq!(whole.revision(), bytewise.revision());
    assert_eq!(
        whole.updates_since(TerminalRevision::default()).unwrap(),
        bytewise.updates_since(TerminalRevision::default()).unwrap()
    );
    assert!(whole.revision().value() > 1);
}

fn dump(terminal: &Terminal) -> (Vec<String>, Vec<String>, (i32, i32)) {
    let snapshot = terminal.snapshot(SnapshotRequest {
        max_scrollback_rows: 1000,
    });
    let trim = |row| row_text(row).trim_end().to_owned();
    let screen = snapshot.visible_rows().map(trim).collect::<Vec<_>>();
    let history = snapshot.scrollback_rows().map(trim).collect::<Vec<_>>();
    let cursor = snapshot.cursor().cursor.position();
    (screen, history, (cursor.row, cursor.column))
}

#[test]
fn shrinking_a_short_screen_keeps_its_text_at_the_top_and_out_of_history() {
    let mut terminal = terminal(40, 30);
    for line in 0..50 {
        terminal.advance(format!("old {line}\r\n").as_bytes());
    }
    // `clear`, then three lines and a program waiting for input below them.
    terminal.advance(b"\x1b[H\x1b[2J\x1b[3JL1 top\r\nL2\r\nL3\r\n");
    terminal.resize(40, 10);
    let shrunk = dump(&terminal);
    terminal.resize(40, 30);
    let grown = dump(&terminal);
    for (screen, history, cursor) in [&shrunk, &grown] {
        assert_eq!(&screen[..4], ["L1 top", "L2", "L3", ""]);
        assert!(history.is_empty(), "history {history:?}");
        assert_eq!(*cursor, (3, 0));
    }
}

#[test]
fn a_short_screen_stays_at_the_top_after_an_earlier_resize_pushed_rows_into_history() {
    let mut terminal = terminal(40, 30);
    terminal.advance(b"\x1b[H\x1b[2J\x1b[3J");
    for line in 1..=30 {
        terminal.advance(format!("{line}\r\n").as_bytes());
    }
    // A smaller pane pushes rows into history, then the pane grows back.
    terminal.resize(40, 10);
    terminal.resize(40, 30);
    terminal.advance(b"\x1b[H\x1b[2J\x1b[3JL1 top\r\nL2\r\nL3\r\n");
    terminal.resize(40, 10);
    let shrunk = dump(&terminal);
    terminal.resize(40, 30);
    let grown = dump(&terminal);
    for (screen, history, cursor) in [&shrunk, &grown] {
        assert_eq!(&screen[..4], ["L1 top", "L2", "L3", ""]);
        assert!(history.is_empty(), "history {history:?}");
        assert_eq!(*cursor, (3, 0));
    }
}
