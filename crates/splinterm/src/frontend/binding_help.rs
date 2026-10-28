//! Searchable binding-help rows derived from one resolved keymap.

use unicode_casefold::UnicodeCaseFold;

use crate::{
    frontend::{
        action_menu::{PaletteGroup, action_group, action_search_metadata},
        text_edit::BoundedTextEditor,
    },
    keymap::{ActionId, BindingSource, ResolvedBinding, ResolvedKeymap},
};

/// Rows the key bindings list shows at once at most.
pub(crate) const BINDING_HELP_PAGE_ITEMS: usize = 10;
const MAX_QUERY_BYTES: usize = 256;
const MAX_QUERY_SCALARS: usize = 128;

/// Copy mode's keys go last, after every command group.
const COPY_MODE_GROUP: (usize, &str) = (PaletteGroup::Control as usize + 1, "copy mode");

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BindingHelpRow {
    id: usize,
    pub(crate) shortcut: String,
    /// The action's config name, as a keybinding file names it.
    pub(crate) action: String,
    pub(crate) source: String,
    pub(crate) compact: String,
    /// What the binding does, in words.
    pub(crate) title: String,
    /// Its keys, one chip each: `ctrl`, `shift`, `↵`.
    pub(crate) chips: Vec<String>,
    /// The group header it shows under.
    pub(crate) group: &'static str,
    /// The selected row's detail line: config name and source.
    pub(crate) detail: String,
    /// Whether the binding comes from the person's own file.
    pub(crate) user: bool,
    group_order: usize,
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

/// One copy-mode key row: its keys as text and as chips, what it does, and
/// its search keywords.
struct CopyRow {
    shortcut: &'static str,
    action: &'static str,
    title: &'static str,
    compact: &'static str,
    chips: &'static [&'static str],
    keywords: &'static [&'static str],
}

const COPY_ROWS: [CopyRow; 3] = [
    CopyRow {
        shortcut: "copy: h/j/k/l · arrows · w/b/e · 0/$ · Home/End · PgUp/PgDn",
        action: "move copy cursor",
        title: "Move the copy cursor",
        compact: "copy: h/j/k/l · arrows · w/b/e · 0/$ · Home/End · PgUp/PgDn — move",
        chips: &["h", "j", "k", "l", "w", "b", "e", "0", "$"],
        keywords: &["copy", "cursor", "move", "navigate", "history"],
    },
    CopyRow {
        shortcut: "copy: v",
        action: "begin selection",
        title: "Start selection",
        compact: "copy: v — begin selection",
        chips: &["v"],
        keywords: &["copy", "select", "selection", "mark", "history"],
    },
    CopyRow {
        shortcut: "copy: y / Escape",
        action: "copy and exit / cancel",
        title: "Copy and exit, or cancel",
        compact: "copy: y / Escape — copy and exit / cancel",
        chips: &["y", "esc"],
        keywords: &["copy", "yank", "exit", "cancel", "history"],
    },
];

/// A binding's keys as chips, from its display (`Ctrl+Shift+Enter`,
/// `Prefix ?`): lowercase modifiers and names, glyphs for the arrows and
/// Enter, one chip per key.
pub(crate) fn shortcut_chips(display: &str) -> Vec<String> {
    let mut chips = Vec::new();
    for word in display.split_whitespace() {
        let mut keys = word.split('+').collect::<Vec<_>>();
        // `Ctrl++` names the plus key itself.
        if keys.len() >= 2 && keys.ends_with(&["", ""]) {
            keys.truncate(keys.len() - 2);
            keys.push("+");
        }
        chips.extend(
            keys.into_iter()
                .filter(|key| !key.is_empty())
                .map(chip_token),
        );
    }
    chips
}

fn chip_token(key: &str) -> String {
    let glyph = match key {
        "Enter" | "Return" => "↵",
        "Escape" => "esc",
        "Left" => "←",
        "Right" => "→",
        "Up" => "↑",
        "Down" => "↓",
        "PageUp" => "pgup",
        "PageDown" => "pgdn",
        "Plus" => "+",
        "Minus" => "-",
        "Equal" => "=",
        "Slash" => "/",
        "Backslash" => "\\",
        "BracketLeft" => "[",
        "BracketRight" => "]",
        "Insert" => "ins",
        other => return other.to_lowercase(),
    };
    glyph.to_owned()
}

/// Where `action`'s group sits in the list, and its header: the group its
/// command shows under in the palette.
fn group(action: ActionId) -> (usize, &'static str) {
    let group = action_group(action);
    (group as usize, group.label())
}

/// A config name in words, for an action without a title of its own:
/// `pane.split-right` reads "Split right".
fn humanized(config_name: &str) -> String {
    let name = config_name
        .rsplit_once('.')
        .map_or(config_name, |(_, name)| name)
        .replace('-', " ");
    let mut characters = name.chars();
    characters.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(characters).collect()
    })
}

/// One resolved binding's row, at its binding's index.
fn binding_row(id: usize, binding: &ResolvedBinding) -> BindingHelpRow {
    let action = binding.action();
    let config_name = action.config_name();
    let (label, category, keywords) = action_search_metadata(action).unwrap_or_else(|| {
        let (label, keywords) = fallback_action_metadata(action);
        (label, fallback_action_category(action), keywords)
    });
    let shortcut = binding.display().to_owned();
    let source = binding.source().short_label();
    let (group_order, group) = group(action);
    let chips = shortcut_chips(&shortcut);
    BindingHelpRow {
        id,
        search_fields: search_fields(
            [label, config_name, &shortcut, &source, category, group],
            keywords,
            &chips,
        ),
        chips,
        shortcut: shortcut.clone(),
        action: config_name.to_owned(),
        detail: format!("{config_name} · {source}"),
        source: source.clone(),
        compact: format!("{shortcut} — {config_name}"),
        title: if label == config_name {
            humanized(config_name)
        } else {
            label.to_owned()
        },
        group,
        user: matches!(binding.source(), BindingSource::User { .. }),
        group_order,
    }
}

impl BindingHelpUi {
    pub(crate) fn new(keymap: &ResolvedKeymap) -> Self {
        let actions = keymap
            .bindings()
            .iter()
            .map(ResolvedBinding::action)
            .collect::<Vec<_>>();
        let mut rows = keymap
            .bindings()
            .iter()
            .enumerate()
            .map(|(id, binding)| binding_row(id, binding))
            .collect::<Vec<_>>();
        for copy in &COPY_ROWS {
            let id = rows.len();
            let source = "copy mode".to_owned();
            let (group_order, group) = COPY_MODE_GROUP;
            let chips = copy
                .chips
                .iter()
                .map(|chip| (*chip).to_owned())
                .collect::<Vec<_>>();
            rows.push(BindingHelpRow {
                id,
                search_fields: search_fields(
                    [
                        copy.action,
                        copy.action,
                        copy.shortcut,
                        &source,
                        "COPY MODE",
                        group,
                    ],
                    copy.keywords,
                    &chips,
                ),
                shortcut: copy.shortcut.into(),
                action: copy.action.into(),
                source: source.clone(),
                compact: copy.compact.into(),
                title: copy.title.into(),
                chips,
                group,
                detail: copy.shortcut.into(),
                user: false,
                group_order,
            });
        }
        // Binding rows sit at their binding's index, so a merge after the
        // copy rows keeps every id distinct.
        if let Some((positions, merged)) = merged_dojo_selection(&rows, &actions) {
            let keep = positions[0];
            let mut removed = positions[1..].to_vec();
            removed.sort_unstable();
            for position in removed.iter().rev() {
                rows.remove(*position);
            }
            let keep = keep - removed.iter().filter(|position| **position < keep).count();
            rows[keep] = merged;
        }
        // Group by group, in the keymap's order within each.
        rows.sort_by_key(|row| row.group_order);
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

    /// Each listed row's group header, when the list shows groups: with no
    /// query. Filtering shows one ranked list, so this is empty.
    pub(crate) fn row_groups(&self) -> Vec<&'static str> {
        if self.editor.text().is_empty() {
            self.rows.iter().map(|row| row.group).collect()
        } else {
            Vec::new()
        }
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

    /// Where the layout put the list's window, so scrolling goes on from
    /// there.
    pub(crate) fn set_visible_start(&mut self, visible_start: usize) {
        self.visible_start = visible_start;
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

/// What a row's search matches: its label, config name, shortcut, source,
/// category and group, its keywords, and each of its chips, so `↵`, `←`,
/// `pgdn` or `esc` find the bindings that use them.
fn search_fields(texts: [&str; 6], keywords: &[&str], chips: &[String]) -> Vec<String> {
    texts
        .into_iter()
        .chain(keywords.iter().copied())
        .chain(chips.iter().map(String::as_str))
        .map(fold_search_text)
        .collect()
}

/// The nine Dojo selections, in digit order.
const SELECT_DOJO: [ActionId; 9] = [
    ActionId::SelectDojo1,
    ActionId::SelectDojo2,
    ActionId::SelectDojo3,
    ActionId::SelectDojo4,
    ActionId::SelectDojo5,
    ActionId::SelectDojo6,
    ActionId::SelectDojo7,
    ActionId::SelectDojo8,
    ActionId::SelectDojo9,
];

/// The nine "Select Dojo N" rows as one "Go to Dojo 1–9" row, with the
/// positions in `rows` they hold (`actions[i]` is the action of `rows[i]`),
/// the first being where the merged row goes. Only when each is bound once,
/// all to the same modifiers with their own digit last, from one source;
/// otherwise they stay nine rows.
fn merged_dojo_selection(
    rows: &[BindingHelpRow],
    actions: &[ActionId],
) -> Option<(Vec<usize>, BindingHelpRow)> {
    let mut positions = Vec::with_capacity(SELECT_DOJO.len());
    for (digit, select) in (1_usize..).zip(SELECT_DOJO) {
        let mut found = actions
            .iter()
            .enumerate()
            .filter(|(_, action)| **action == select)
            .map(|(position, _)| position);
        let position = found.next()?;
        if found.next().is_some() {
            return None;
        }
        let (last, modifiers) = rows.get(position)?.chips.split_last()?;
        if *last != digit.to_string() || modifiers.is_empty() {
            return None;
        }
        positions.push(position);
    }
    let first = &rows[positions[0]];
    let modifiers = &first.chips[..first.chips.len() - 1];
    if positions.iter().any(|position| {
        let row = &rows[*position];
        row.chips[..row.chips.len() - 1] != *modifiers || row.source != first.source
    }) {
        return None;
    }
    let mut chips = modifiers.to_vec();
    chips.push("1…9".to_owned());
    let shortcut = first
        .shortcut
        .strip_suffix('1')
        .map_or_else(|| first.shortcut.clone(), |prefix| format!("{prefix}1…9"));
    let action = "dojo.select-1…9";
    let mut search_fields = positions
        .iter()
        .flat_map(|position| rows[*position].search_fields.iter().cloned())
        .chain(["go to dojo 1–9", "1…9"].map(fold_search_text))
        .collect::<Vec<_>>();
    search_fields.sort();
    search_fields.dedup();
    let merged = BindingHelpRow {
        id: first.id,
        compact: format!("{shortcut} — {action}"),
        shortcut,
        action: action.to_owned(),
        source: first.source.clone(),
        title: "Go to Dojo 1–9".to_owned(),
        chips,
        group: first.group,
        detail: format!("{action} · {}", first.source),
        user: first.user,
        group_order: first.group_order,
        search_fields,
    };
    Some((positions, merged))
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
        // The nine Dojo selections may share one row.
        let merged = help
            .rows()
            .iter()
            .any(|row| row.action == "dojo.select-1…9");
        let merged_away = if merged { 8 } else { 0 };
        assert_eq!(help.rows().len(), keymap.bindings().len() + 3 - merged_away);
        for (binding_id, binding) in keymap.bindings().iter().enumerate() {
            if merged && SELECT_DOJO.contains(&binding.action()) {
                continue;
            }
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
        assert_eq!(help.rows().len(), help.total());
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
    fn default_help_lists_the_numbered_dojo_chords_as_one_row_and_finds_each() {
        let keymap = built_in_keymap(KeymapProfile::Splinterm);
        let help = BindingHelpUi::new(&keymap);
        let merged = help
            .rows()
            .iter()
            .filter(|row| row.action.starts_with("dojo.select-"))
            .collect::<Vec<_>>();
        assert_eq!(merged.len(), 1, "the nine chords share one row");
        let merged = merged[0];
        assert_eq!(merged.title, "Go to Dojo 1–9");
        assert_eq!(merged.chips, ["ctrl", "shift", "1…9"]);
        assert_eq!(merged.shortcut, "Ctrl+Shift+1…9");
        assert_eq!(
            merged.detail,
            format!("dojo.select-1…9 · {}", merged.source)
        );
        assert_eq!(merged.group, "dojo");
        for digit in 1..=9 {
            let shortcut = format!("Ctrl+Shift+{digit}");
            let mut help = BindingHelpUi::new(&keymap);
            assert!(help.append_text(&shortcut));
            assert!(
                help.rows()
                    .iter()
                    .any(|row| row.action == "dojo.select-1…9"),
                "query {shortcut:?} did not find the merged row"
            );
        }

        // One chord bound differently: the nine stay nine rows.
        let resolved = crate::keymap::resolve_keymap_text(
            KeymapProfile::Splinterm,
            r#"
version = 1
inherits = "splinterm"

[[binding]]
sequence = ["Alt+3"]
action = "dojo.select-3"
"#,
            std::path::Path::new("keybindings.toml"),
        )
        .unwrap();
        let help = BindingHelpUi::new(&resolved.keymap);
        assert!(help.rows().iter().any(|row| row.shortcut == "Alt+3"));
        for digit in 1..=9 {
            let action = format!("dojo.select-{digit}");
            assert!(
                help.rows().iter().any(|row| row.action == action),
                "{action} keeps its own row"
            );
        }
        assert!(
            !help
                .rows()
                .iter()
                .any(|row| row.action == "dojo.select-1…9")
        );
    }

    #[test]
    fn chips_are_lowercase_keys_with_glyph_arrows_and_enter() {
        let chips = shortcut_chips;
        assert_eq!(chips("Ctrl+Shift+Enter"), ["ctrl", "shift", "↵"]);
        assert_eq!(chips("Ctrl+Shift+P"), ["ctrl", "shift", "p"]);
        assert_eq!(chips("Prefix ?"), ["prefix", "?"]);
        assert_eq!(chips("Prefix Shift+C"), ["prefix", "shift", "c"]);
        assert_eq!(chips("Ctrl+Shift+PageDown"), ["ctrl", "shift", "pgdn"]);
        assert_eq!(chips("Shift+Up"), ["shift", "↑"]);
        assert_eq!(chips("Alt+Left"), ["alt", "←"]);
        assert_eq!(chips("Ctrl+Plus"), ["ctrl", "+"]);
        assert_eq!(chips("Ctrl++"), ["ctrl", "+"]);
        assert_eq!(chips("Escape"), ["esc"]);
        assert_eq!(chips("Ctrl+Tab"), ["ctrl", "tab"]);
        assert!(chips("").is_empty());
        assert_eq!(humanized("pane.split-right"), "Split right");
        assert_eq!(humanized("reload"), "Reload");
    }

    /// Rows say what a binding does in words, show its keys as chips, go
    /// group by group with copy mode last, and carry the config name and
    /// source for the detail line.
    #[test]
    fn rows_have_human_titles_chips_groups_and_a_detail_line() {
        for profile in [KeymapProfile::Splinterm, KeymapProfile::OmarchyTmux] {
            let keymap = built_in_keymap(profile);
            let mut help = BindingHelpUi::new(&keymap);
            let groups = help.row_groups();
            assert_eq!(groups.len(), help.rows().len());
            let orders = help
                .rows()
                .iter()
                .map(|row| row.group_order)
                .collect::<Vec<_>>();
            assert!(orders.windows(2).all(|pair| pair[0] <= pair[1]));
            assert_eq!(groups.last(), Some(&"copy mode"));
            for row in help.rows() {
                assert!(!row.chips.is_empty(), "{}", row.action);
                assert!(!row.title.is_empty());
                assert_ne!(row.title, row.action, "{}", row.action);
                assert!(
                    [
                        "dojo",
                        "pane",
                        "history",
                        "lair",
                        "view",
                        "app",
                        "control",
                        "copy mode"
                    ]
                    .contains(&row.group),
                    "{}",
                    row.group
                );
                if row.group == "dojo" {
                    assert!(!row.title.contains("Lair"), "{}", row.title);
                }
                assert!(!row.user);
                if row.group == "copy mode" {
                    assert_eq!(row.detail, row.shortcut);
                } else {
                    assert_eq!(row.detail, format!("{} · {}", row.action, row.source));
                    if row.action != "dojo.select-1…9" {
                        assert_eq!(row.chips, shortcut_chips(&row.shortcut));
                    }
                }
            }
            assert!(help.append_text("split"));
            assert!(help.row_groups().is_empty());
            assert!(!help.rows().is_empty());
        }
    }

    /// Chip tokens find the bindings that use them.
    #[test]
    fn search_matches_chip_tokens() {
        let keymap = built_in_keymap(KeymapProfile::Splinterm);
        for token in ["↵", "←", "pgdn", "esc"] {
            let mut help = BindingHelpUi::new(&keymap);
            assert!(help.append_text(token));
            assert!(
                help.rows()
                    .iter()
                    .any(|row| row.chips.iter().any(|chip| chip == token)),
                "{token} found nothing"
            );
        }
    }
}
