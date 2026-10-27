//! Searchable binding-help rows derived from one resolved keymap.

use unicode_casefold::UnicodeCaseFold;

use crate::{
    frontend::{action_menu::action_search_metadata, text_edit::BoundedTextEditor},
    keymap::{ActionId, ResolvedKeymap},
};

pub(crate) const BINDING_HELP_PAGE_ITEMS: usize = 7;
const MAX_QUERY_BYTES: usize = 256;
const MAX_QUERY_SCALARS: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BindingHelpRow {
    id: usize,
    pub(crate) shortcut: String,
    pub(crate) action: String,
    pub(crate) source: String,
    pub(crate) compact: String,
    search_fields: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BindingHelpUi {
    all_rows: Vec<BindingHelpRow>,
    rows: Vec<BindingHelpRow>,
    editor: BoundedTextEditor,
    filtered_query: String,
    selected: usize,
    visible_start: usize,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct MatchScore {
    class: u8,
    penalty: usize,
}

impl BindingHelpUi {
    pub(crate) fn new(keymap: &ResolvedKeymap) -> Self {
        let mut rows = keymap
            .bindings()
            .iter()
            .enumerate()
            .map(|(id, binding)| {
                let action = binding.action();
                let config_name = action.config_name();
                let (label, category, keywords) =
                    action_search_metadata(action).unwrap_or_else(|| {
                        let (label, keywords) = fallback_action_metadata(action);
                        (label, fallback_action_category(action), keywords)
                    });
                let shortcut = binding.display().to_owned();
                let source = binding.source().short_label();
                BindingHelpRow {
                    id,
                    shortcut: shortcut.clone(),
                    action: config_name.to_owned(),
                    source: source.clone(),
                    compact: format!("{shortcut} — {config_name}"),
                    search_fields: search_fields(
                        label,
                        config_name,
                        &shortcut,
                        &source,
                        category,
                        keywords,
                    ),
                }
            })
            .collect::<Vec<_>>();
        let mut push_copy_row =
            |shortcut: &str, action: &str, compact: &str, keywords: &'static [&'static str]| {
                let id = rows.len();
                let source = "copy mode".to_owned();
                rows.push(BindingHelpRow {
                    id,
                    shortcut: shortcut.into(),
                    action: action.into(),
                    source: source.clone(),
                    compact: compact.into(),
                    search_fields: search_fields(
                        action,
                        action,
                        shortcut,
                        &source,
                        "COPY MODE",
                        keywords,
                    ),
                });
            };
        push_copy_row(
            "copy: h/j/k/l · arrows · w/b/e · 0/$ · Home/End · PgUp/PgDn",
            "move copy cursor",
            "copy: h/j/k/l · arrows · w/b/e · 0/$ · Home/End · PgUp/PgDn — move",
            &["copy", "cursor", "move", "navigate", "history"],
        );
        push_copy_row(
            "copy: v",
            "begin selection",
            "copy: v — begin selection",
            &["copy", "select", "selection", "mark", "history"],
        );
        push_copy_row(
            "copy: y / Escape",
            "copy and exit / cancel",
            "copy: y / Escape — copy and exit / cancel",
            &["copy", "yank", "exit", "cancel", "history"],
        );
        let mut ui = Self {
            all_rows: rows.clone(),
            rows,
            editor: BoundedTextEditor::new(
                String::new(),
                MAX_QUERY_BYTES,
                MAX_QUERY_SCALARS,
                false,
            ),
            filtered_query: String::new(),
            selected: 0,
            visible_start: 0,
        };
        ui.refilter();
        ui
    }

    pub(crate) fn query(&self) -> &str {
        self.editor.text()
    }

    pub(crate) fn editor_mut(&mut self) -> &mut BoundedTextEditor {
        &mut self.editor
    }

    pub(crate) fn editor_changed(&mut self) {
        if self.editor.text() != self.filtered_query {
            self.refilter();
        }
    }

    pub(crate) fn rows(&self) -> &[BindingHelpRow] {
        &self.rows
    }

    /// How many bindings the list shows with no query.
    pub(crate) fn total(&self) -> usize {
        self.all_rows.len()
    }

    pub(crate) const fn selected_index(&self) -> usize {
        self.selected
    }

    pub(crate) const fn visible_start(&self) -> usize {
        self.visible_start
    }

    #[cfg(test)]
    pub(crate) fn append_text(&mut self, text: &str) -> bool {
        if !self.editor.insert(text) {
            return false;
        }
        self.refilter();
        true
    }

    #[cfg(test)]
    pub(crate) fn backspace(&mut self) -> bool {
        if !self.editor.backspace() {
            return false;
        }
        self.refilter();
        true
    }

    pub(crate) fn clear_query(&mut self) -> bool {
        if self.editor.text().is_empty() {
            return false;
        }
        let _ = self.editor.select_all();
        let _ = self.editor.cut();
        self.refilter();
        true
    }

    pub(crate) fn move_selection(&mut self, delta: isize) -> bool {
        if self.rows.is_empty() || delta == 0 {
            return false;
        }
        let previous = self.selected;
        let magnitude = delta.unsigned_abs().min(self.rows.len().saturating_sub(1));
        self.selected = if delta.is_negative() {
            self.selected.saturating_sub(magnitude)
        } else {
            self.selected
                .saturating_add(magnitude)
                .min(self.rows.len() - 1)
        };
        self.ensure_visible();
        self.selected != previous
    }

    pub(crate) fn select_first(&mut self) -> bool {
        let changed = self.selected != 0 || self.visible_start != 0;
        self.selected = 0;
        self.visible_start = 0;
        changed
    }

    pub(crate) fn select_last(&mut self) -> bool {
        if self.rows.is_empty() {
            return false;
        }
        let last = self.rows.len() - 1;
        let changed = self.selected != last;
        self.selected = last;
        self.ensure_visible();
        changed
    }

    fn refilter(&mut self) {
        let query = self.editor.text().to_owned();
        self.filtered_query.clone_from(&query);
        if query.is_empty() {
            self.rows.clone_from(&self.all_rows);
        } else {
            let mut matches = self
                .all_rows
                .iter()
                .filter_map(|row| {
                    match_fields(&row.search_fields, &query).map(|score| (score, row))
                })
                .collect::<Vec<_>>();
            matches.sort_by_key(|(score, row)| (*score, row.id));
            self.rows = matches.into_iter().map(|(_, row)| row.clone()).collect();
        }
        self.selected = 0;
        self.visible_start = 0;
    }

    fn ensure_visible(&mut self) {
        if self.selected < self.visible_start {
            self.visible_start = self.selected;
        } else if self.selected >= self.visible_start.saturating_add(BINDING_HELP_PAGE_ITEMS) {
            self.visible_start = self
                .selected
                .saturating_add(1)
                .saturating_sub(BINDING_HELP_PAGE_ITEMS);
        }
    }
}

fn search_fields(
    label: &str,
    config_name: &str,
    shortcut: &str,
    source: &str,
    category: &str,
    keywords: &[&str],
) -> Vec<String> {
    [label, config_name, shortcut, source, category]
        .into_iter()
        .chain(keywords.iter().copied())
        .map(fold_search_text)
        .collect()
}

fn fallback_action_category(action: ActionId) -> &'static str {
    match action
        .config_name()
        .split_once('.')
        .map(|(namespace, _)| namespace)
    {
        Some("dojo") => "TAB",
        Some("lair" | "session") => "DOJO",
        Some("pane") => "PANE",
        Some("history" | "copy-mode") => "HISTORY",
        Some("view") => "VIEW",
        Some("control" | "access") => "CONTROL",
        _ => "APP",
    }
}

fn fallback_action_metadata(action: ActionId) -> (&'static str, &'static [&'static str]) {
    match action {
        ActionId::SelectDojo1 => ("Select Dojo 1", &["select", "dojo", "tab", "one"]),
        ActionId::SelectDojo2 => ("Select Dojo 2", &["select", "dojo", "tab", "two"]),
        ActionId::SelectDojo3 => ("Select Dojo 3", &["select", "dojo", "tab", "three"]),
        ActionId::SelectDojo4 => ("Select Dojo 4", &["select", "dojo", "tab", "four"]),
        ActionId::SelectDojo5 => ("Select Dojo 5", &["select", "dojo", "tab", "five"]),
        ActionId::SelectDojo6 => ("Select Dojo 6", &["select", "dojo", "tab", "six"]),
        ActionId::SelectDojo7 => ("Select Dojo 7", &["select", "dojo", "tab", "seven"]),
        ActionId::SelectDojo8 => ("Select Dojo 8", &["select", "dojo", "tab", "eight"]),
        ActionId::SelectDojo9 => ("Select Dojo 9", &["select", "dojo", "tab", "nine"]),
        ActionId::ResizePaneLeftFive => {
            ("Resize pane left 5%", &["resize", "pane", "left", "five"])
        }
        ActionId::ResizePaneRightFive => {
            ("Resize pane right 5%", &["resize", "pane", "right", "five"])
        }
        ActionId::ResizePaneUpFive => ("Resize pane up 5%", &["resize", "pane", "up", "five"]),
        ActionId::ResizePaneDownFive => {
            ("Resize pane down 5%", &["resize", "pane", "down", "five"])
        }
        ActionId::SendPrefix => ("Send prefix", &["send", "prefix", "terminal", "literal"]),
        ActionId::ClipboardCopy => ("Copy to clipboard", &["copy", "clipboard", "selection"]),
        ActionId::ClipboardPaste => ("Paste from clipboard", &["paste", "clipboard", "input"]),
        _ => (
            action.config_name(),
            &["binding", "keymap", "shortcut", "action"],
        ),
    }
}

fn fold_search_text(text: &str) -> String {
    text.case_fold().collect()
}

fn match_fields(fields: &[String], query: &str) -> Option<MatchScore> {
    let query = fold_search_text(query.trim());
    if query.is_empty() {
        return Some(MatchScore {
            class: 0,
            penalty: 0,
        });
    }
    if fields.iter().any(|field| field == &query) {
        return Some(MatchScore {
            class: 0,
            penalty: 0,
        });
    }
    if fields.iter().any(|field| field.starts_with(&query)) {
        return Some(MatchScore {
            class: 1,
            penalty: 0,
        });
    }
    if fields.iter().any(|field| field.contains(&query)) {
        return Some(MatchScore {
            class: 2,
            penalty: 0,
        });
    }
    if ordered_token_penalty(fields, &query).is_some() {
        return Some(MatchScore {
            class: 3,
            penalty: 0,
        });
    }
    fuzzy_subsequence_penalty(fields, &query).map(|penalty| MatchScore { class: 4, penalty })
}

fn ordered_token_penalty(fields: &[String], query: &str) -> Option<usize> {
    let query_tokens = query.split_whitespace().collect::<Vec<_>>();
    if query_tokens.is_empty() {
        return None;
    }
    let candidate_tokens = fields
        .iter()
        .flat_map(|field| field.split(|character: char| !character.is_alphanumeric()))
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    let mut next = 0_usize;
    let mut penalty = 0_usize;
    for query_token in query_tokens {
        let relative = candidate_tokens[next..]
            .iter()
            .position(|candidate| candidate.starts_with(query_token))?;
        penalty = penalty.saturating_add(relative);
        next = next.saturating_add(relative).saturating_add(1);
    }
    Some(penalty)
}

fn fuzzy_subsequence_penalty(fields: &[String], query: &str) -> Option<usize> {
    let query = query
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<Vec<_>>();
    if query.is_empty() {
        return None;
    }
    fields
        .iter()
        .filter_map(|field| fuzzy_field_penalty(field, &query))
        .min()
}

fn fuzzy_field_penalty(field: &str, query: &[char]) -> Option<usize> {
    let candidate = field.chars().collect::<Vec<_>>();
    let mut next = 0;
    let mut previous = None;
    let mut penalty = 0_usize;
    for query_character in query {
        let relative = candidate[next..]
            .iter()
            .position(|candidate| candidate == query_character)?;
        let position = next.saturating_add(relative);
        if let Some(previous) = previous {
            penalty = penalty.saturating_add(position.saturating_sub(previous + 1) * 4);
        } else {
            penalty = penalty.saturating_add(position * 2);
        }
        let boundary = position == 0 || !candidate[position - 1].is_alphanumeric();
        if !boundary && previous != Some(position.saturating_sub(1)) {
            penalty = penalty.saturating_add(2);
        }
        previous = Some(position);
        next = position.saturating_add(1);
    }
    Some(penalty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{KeymapProfile, built_in_keymap};

    #[test]
    fn help_contains_every_resolved_binding_and_copy_mode_local_keys() {
        let keymap = built_in_keymap(KeymapProfile::OmarchyTmux);
        let help = BindingHelpUi::new(&keymap);
        assert_eq!(help.rows().len(), keymap.bindings().len() + 3);
        for (binding_id, binding) in keymap.bindings().iter().enumerate() {
            let rows = help
                .rows()
                .iter()
                .filter(|row| row.id == binding_id)
                .collect::<Vec<_>>();
            assert_eq!(
                rows.len(),
                1,
                "resolved binding must appear exactly once: {}",
                binding.display()
            );
            assert_eq!(rows[0].shortcut, binding.display());
            assert_eq!(rows[0].action, binding.action().config_name());
        }
        assert!(help.rows().iter().any(|row| {
            row.action == ActionId::BindingHelp.config_name() && row.shortcut == "Prefix ?"
        }));
        assert!(help.rows().iter().any(|row| {
            row.action == ActionId::CopyModeEnter.config_name() && row.shortcut == "Prefix ["
        }));
        assert!(help.rows().iter().any(|row| row.shortcut == "copy: v"));
        let movement = help
            .rows()
            .iter()
            .find(|row| row.action == "move copy cursor")
            .unwrap();
        for alias in ["w/b/e", "0/$"] {
            assert!(movement.shortcut.contains(alias));
            assert!(movement.compact.contains(alias));
        }
    }

    #[test]
    fn help_navigation_is_bounded_and_keeps_selection_visible() {
        let keymap = built_in_keymap(KeymapProfile::OmarchyTmux);
        let mut help = BindingHelpUi::new(&keymap);
        assert!(help.move_selection(isize::MAX));
        assert_eq!(help.selected_index(), help.rows().len() - 1);
        assert!(help.visible_start() <= help.selected_index());
        assert!(help.select_first());
        assert_eq!(help.visible_start(), 0);
        assert!(help.select_last());
    }

    #[test]
    fn search_covers_labels_config_shortcuts_sources_and_keywords() {
        let keymap = built_in_keymap(KeymapProfile::OmarchyTmux);
        let cases = [
            ("show keybindings", ActionId::BindingHelp),
            ("pane.split-right", ActionId::SplitVertical),
            ("Prefix ?", ActionId::BindingHelp),
            ("history", ActionId::SearchScrollback),
            ("maximize", ActionId::TogglePaneZoom),
            ("favorite", ActionId::ToggleCurrentLairPin),
            ("lair restore", ActionId::RestoreCurrentLair),
        ];
        for (query, expected) in cases {
            let mut help = BindingHelpUi::new(&keymap);
            assert!(help.append_text(query));
            assert!(
                help.rows()
                    .iter()
                    .any(|row| row.action == expected.config_name()),
                "query {query:?} did not find {}",
                expected.config_name()
            );
        }

        let source = keymap.bindings()[0].source().short_label();
        let mut help = BindingHelpUi::new(&keymap);
        assert!(help.append_text(&source));
        assert!(help.rows().iter().all(|row| row.source == source));
    }

    #[test]
    fn ranking_obeys_match_classes_and_preserves_source_order_for_ties() {
        let keymap = built_in_keymap(KeymapProfile::OmarchyTmux);
        let mut exact = BindingHelpUi::new(&keymap);
        assert!(exact.append_text("pane.split-right"));
        assert_eq!(
            exact.rows()[0].action,
            ActionId::SplitVertical.config_name()
        );

        let mut tokens = BindingHelpUi::new(&keymap);
        assert!(tokens.append_text("split right"));
        assert_eq!(
            tokens.rows()[0].action,
            ActionId::SplitVertical.config_name()
        );

        let mut fuzzy = BindingHelpUi::new(&keymap);
        assert!(fuzzy.append_text("kybndgs"));
        assert_eq!(fuzzy.rows()[0].action, ActionId::BindingHelp.config_name());

        let mut tied = BindingHelpUi::new(&keymap);
        assert!(tied.append_text("pane"));
        assert!(tied.rows().windows(2).all(|rows| {
            let left = match_fields(&rows[0].search_fields, "pane").expect("visible row matches");
            let right = match_fields(&rows[1].search_fields, "pane").expect("visible row matches");
            (left, rows[0].id) <= (right, rows[1].id)
        }));
    }

    #[test]
    fn matcher_assigns_the_documented_rank_classes() {
        let exact = vec!["restore".to_owned()];
        let prefix = vec!["restore saved lair".to_owned()];
        let substring = vec!["preview and restore".to_owned()];
        let tokens = vec!["split pane".to_owned(), "right".to_owned()];
        let fuzzy = vec!["keybindings".to_owned()];
        assert_eq!(match_fields(&exact, "restore").unwrap().class, 0);
        assert_eq!(match_fields(&prefix, "restore saved").unwrap().class, 1);
        assert_eq!(match_fields(&substring, "restore").unwrap().class, 2);
        assert_eq!(match_fields(&tokens, "split right").unwrap().class, 3);
        assert_eq!(match_fields(&fuzzy, "kybndgs").unwrap().class, 4);
    }

    #[test]
    fn query_edits_are_bounded_and_reset_selection_and_viewport() {
        let keymap = built_in_keymap(KeymapProfile::OmarchyTmux);
        let mut help = BindingHelpUi::new(&keymap);
        assert!(help.move_selection(isize::MAX));
        assert!(help.append_text(&"a".repeat(MAX_QUERY_BYTES * 2)));
        assert!(help.query().len() <= MAX_QUERY_BYTES);
        assert!(help.query().chars().count() <= MAX_QUERY_SCALARS);
        assert_eq!(help.selected_index(), 0);
        assert_eq!(help.visible_start(), 0);
        assert!(help.backspace());
        assert!(help.clear_query());
        assert!(help.query().is_empty());
        assert_eq!(help.rows().len(), keymap.bindings().len() + 3);
        assert!(!help.clear_query());
    }

    #[test]
    fn shared_editor_refilters_only_when_query_text_changes() {
        let keymap = built_in_keymap(KeymapProfile::OmarchyTmux);
        let mut help = BindingHelpUi::new(&keymap);
        assert!(help.editor_mut().insert("pane"));
        help.editor_changed();
        assert!(!help.rows().is_empty());
        assert!(help.move_selection(1));
        let selected = help.selected_index();
        assert!(help.editor_mut().move_left(false));
        help.editor_changed();
        assert_eq!(help.selected_index(), selected);
    }

    #[test]
    fn no_match_is_calm_and_unicode_matching_is_case_insensitive() {
        let keymap = built_in_keymap(KeymapProfile::OmarchyTmux);
        let mut help = BindingHelpUi::new(&keymap);
        assert!(help.append_text("definitely absent xyz"));
        assert!(help.rows().is_empty());
        assert_eq!(help.selected_index(), 0);
        assert_eq!(help.visible_start(), 0);
        assert!(!help.move_selection(1));

        let fields = vec![fold_search_text("Δοκιμή Straße Σίσυφος binding")];
        assert!(match_fields(&fields, "ΔΟΚΙ").is_some());
        assert!(match_fields(&fields, "STRASSE").is_some());
        assert!(match_fields(&fields, "ΣΊΣΥΦΟΣ").is_some());
    }

    #[test]
    fn default_help_lists_and_finds_the_numbered_dojo_chords() {
        let keymap = built_in_keymap(KeymapProfile::Splinterm);
        for digit in 1..=9 {
            let shortcut = format!("Ctrl+Shift+{digit}");
            let action = format!("dojo.select-{digit}");
            let mut help = BindingHelpUi::new(&keymap);
            assert!(
                help.rows()
                    .iter()
                    .any(|row| row.shortcut == shortcut && row.action == action)
            );
            assert!(help.append_text(&shortcut));
            assert!(
                help.rows().iter().any(|row| row.action == action),
                "query {shortcut:?} did not find {action}"
            );
        }
    }
}
